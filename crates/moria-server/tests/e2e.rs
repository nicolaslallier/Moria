//! End-to-end tests: server + dwarf-ai + inventory in-process over a real RabbitMQ.
//! Run: AMQP_TEST_URL=amqp://guest:guest@127.0.0.1:5672 cargo test -p moria-server --test e2e -- --ignored
use futures::StreamExt;
use moria_bus::{Bus, Queue};
use moria_core::inventory::{Batch, KEY_APPLY, KEY_STATE, State};
use moria_core::protocol::{
    ClientMsg, FortressView, PROTOCOL_VERSION, ServerMsg, encode, read_frame_async,
};
use moria_core::srd::{Class, Race};
use moria_core::tile::{DesignationKind, Tile};
use moria_core::world::{generate, save};
use moria_core::{Dir, Resources, TilePos};
use moria_server::{Config, run_with_listener};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;

// All tests share one broker and its queues: run them one at a time.
static SERIAL: Mutex<()> = Mutex::const_new(());

fn amqp_url() -> String {
    std::env::var("AMQP_TEST_URL").expect("AMQP_TEST_URL")
}

struct Task {
    stop: oneshot::Sender<()>,
    done: JoinHandle<anyhow::Result<()>>,
}

impl Task {
    async fn stop(self) {
        let _ = self.stop.send(());
        timeout(Duration::from_secs(10), self.done)
            .await
            .expect("task stops")
            .unwrap()
            .unwrap();
    }
}

fn start_ai() -> Task {
    let (stop, rx) = oneshot::channel();
    let done = tokio::spawn(moria_dwarf_ai::run(amqp_url(), async {
        let _ = rx.await;
    }));
    Task { stop, done }
}

async fn start_server(data: &Path) -> (Task, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, rx) = oneshot::channel();
    let cfg = Config {
        data: data.to_path_buf(),
        bind: addr,
        amqp_url: Some(amqp_url()),
        invite: None,
        max_per_ip: 1000,
    };
    let done = tokio::spawn(run_with_listener(cfg, listener, async {
        let _ = rx.await;
    }));
    (Task { stop, done }, addr)
}

fn rm(data: &Path) {
    let _ = std::fs::remove_dir_all(data.parent().unwrap());
}

fn world_file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("moria-e2e-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let data = dir.join("world.bin");
    save(&generate(7), &data).unwrap();
    data
}

async fn recv(s: &mut TcpStream) -> ServerMsg {
    timeout(Duration::from_secs(10), read_frame_async(s))
        .await
        .expect("reply in time")
        .expect("frame")
}

/// A logged-in client that mirrors the fortress by applying tick deltas.
struct Client {
    s: TcpStream,
    view: FortressView,
    tick: u64,
    /// Direction of the last dug tile, entered before the next dig.
    step: Option<Dir>,
}

impl Client {
    async fn login(addr: SocketAddr, name: &str) -> Client {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(&encode(&ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            name: name.into(),
            password: "secret12".into(),
            invite: None,
            create: true, // creates on first login, plain login afterwards
        }))
        .await
        .unwrap();
        let needs_hero = match recv(&mut s).await {
            ServerMsg::Welcome { needs_hero, .. } => needs_hero,
            m => panic!("expected welcome, got {m:?}"),
        };
        if needs_hero {
            s.write_all(&encode(&ClientMsg::CreateHero {
                race: Race::Dwarf,
                class: Class::Fighter,
            }))
            .await
            .unwrap();
        }
        match recv(&mut s).await {
            ServerMsg::Snapshot(v) => Client {
                s,
                view: *v,
                tick: 0,
                step: None,
            },
            m => panic!("expected snapshot, got {m:?}"),
        }
    }

    /// Reads one message, applying a tick delta to the mirrored view.
    async fn pump(&mut self) {
        if let ServerMsg::Tick(d) = recv(&mut self.s).await {
            self.tick = d.tick;
            self.view.hero.pos = d.hero_pos;
            self.view.dwarves = d.dwarves;
            self.view.resources = d.resources;
            for (p, t) in d.tiles {
                self.view.mountain.set(p, t);
            }
        }
    }

    async fn ticks(&mut self, n: u64) {
        while self.tick == 0 {
            self.pump().await;
        }
        let end = self.tick + n;
        while self.tick < end {
            self.pump().await;
        }
    }

    /// Waits (bounded) until dwarves and tiles have not changed for 30 ticks.
    async fn quiesce(&mut self) {
        let snap = |c: &Client| (c.view.dwarves.clone(), c.view.mountain.clone());
        let mut last = snap(self);
        let mut calm = 0;
        for _ in 0..600 {
            self.pump().await;
            let now = snap(self);
            calm = if now == last { calm + 1 } else { 0 };
            if calm >= 30 {
                return;
            }
            last = now;
        }
        panic!("dwarves never went idle");
    }

    async fn until(&mut self, max_ticks: u64, mut cond: impl FnMut(&Client) -> bool) {
        for _ in 0..max_ticks {
            if cond(self) {
                return;
            }
            self.pump().await;
        }
        assert!(cond(self), "condition not met within {max_ticks} ticks");
    }

    /// Designates `n` rock tiles next to open floor, nearest to the first dwarf.
    async fn designate_near_dwarves(&mut self, n: usize) -> Vec<TilePos> {
        let m = &self.view.mountain;
        let d = self.view.dwarves[0].1;
        let (w, h, _) = m.size();
        let mut c: Vec<TilePos> = (0..w)
            .flat_map(|x| (0..h).map(move |y| TilePos { x, y, z: d.z }))
            .filter(|&p| m.get(p) == Some(Tile::Rock))
            .filter(|p| {
                [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dy)| {
                    m.get(TilePos {
                        x: p.x + dx,
                        y: p.y + dy,
                        z: p.z,
                    }) == Some(Tile::Floor)
                })
            })
            .collect();
        c.sort_by_key(|p| (p.x - d.x).abs() + (p.y - d.y).abs());
        c.truncate(n);
        assert_eq!(c.len(), n, "not enough diggable tiles");
        for &p in &c {
            self.s
                .write_all(&encode(&ClientMsg::Designate {
                    a: p,
                    b: p,
                    kind: DesignationKind::Dig,
                }))
                .await
                .unwrap();
        }
        c
    }

    fn dug(&self, tiles: &[TilePos]) -> usize {
        tiles
            .iter()
            .filter(|&&p| self.view.mountain.get(p) == Some(Tile::Floor))
            .count()
    }
}

#[tokio::test]
#[ignore]
async fn dwarves_dig_designation_via_ai_service() {
    let _g = SERIAL.lock().await;
    let data = world_file("dig");
    let ai = start_ai();
    let (srv, addr) = start_server(&data).await;
    let mut c = Client::login(addr, "alice").await;
    let t = c.designate_near_dwarves(3).await;
    c.until(300, |c| c.dug(&t) == 3).await;
    srv.stop().await;
    ai.stop().await;
    rm(&data);
}

#[tokio::test]
#[ignore]
async fn ai_restart_dwarves_pause_then_resume() {
    let _g = SERIAL.lock().await;
    let data = world_file("restart-ai");
    let ai = start_ai();
    let (srv, addr) = start_server(&data).await;
    let mut c = Client::login(addr, "alice").await;
    let t = c.designate_near_dwarves(12).await;
    c.until(300, |c| c.dug(&t) >= 1).await;

    ai.stop().await;
    c.quiesce().await; // in-flight assignments have landed and finished
    let settled = c.dug(&t);
    assert!(settled < 12, "AI was killed too late for the test");
    c.ticks(50).await;
    assert_eq!(c.dug(&t), settled, "no new digging while AI is down");

    let ai = start_ai();
    c.until(600, |c| c.dug(&t) == 12).await;
    srv.stop().await;
    ai.stop().await;
    rm(&data);
}

#[tokio::test]
#[ignore]
async fn reconnect_receives_snapshot_with_offline_progress() {
    let _g = SERIAL.lock().await;
    let data = world_file("reconnect");
    let ai = start_ai();
    let (srv, addr) = start_server(&data).await;
    let mut c = Client::login(addr, "alice").await;
    let t = c.designate_near_dwarves(12).await;
    c.ticks(5).await;
    assert!(c.dug(&t) < 12, "all dug before disconnect");
    drop(c);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let c = Client::login(addr, "alice").await;
        if c.dug(&t) == 12 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no offline progress in 60 s"
        );
        drop(c);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    srv.stop().await;
    ai.stop().await;
    rm(&data);
}

#[tokio::test]
#[ignore]
async fn restart_server_restores_fortress() {
    let _g = SERIAL.lock().await;
    let data = world_file("restart-srv");
    let ai = start_ai();
    let (srv, addr) = start_server(&data).await;
    let mut c = Client::login(addr, "alice").await;
    let t = c.designate_near_dwarves(3).await;
    c.until(300, |c| c.dug(&t) == 3).await;
    ai.stop().await;
    c.ticks(30).await; // let everything settle
    let before = (c.view.mountain.clone(), c.view.resources, c.view.hero.pos);
    drop(c);
    srv.stop().await; // final save

    let (srv, addr) = start_server(&data).await;
    let c = Client::login(addr, "alice").await;
    assert_eq!(
        (c.view.mountain.clone(), c.view.resources, c.view.hero.pos),
        before
    );
    assert!(c.view.resources.stone >= 3);
    assert_eq!(c.view.dwarves.len(), 3);
    srv.stop().await;
    rm(&data);
}

// ---- Inventory service: flow, outage, restarts -------------------------------------------

fn start_inventory(file: &Path) -> Task {
    let (stop, rx) = oneshot::channel();
    let done = tokio::spawn(moria_inventory::run(
        amqp_url(),
        file.to_path_buf(),
        async {
            let _ = rx.await;
        },
    ));
    Task { stop, done }
}

fn inventory_file(data: &Path) -> PathBuf {
    data.parent().unwrap().join("inventory.bin")
}

/// Latest `inventory.state` seen on the bus (the file and the queue start empty).
struct Env {
    _bus: Bus,
    states: watch::Receiver<State>,
}

async fn env() -> Env {
    let bus = Bus::connect(&amqp_url()).await.unwrap();
    bus.declare(KEY_APPLY, Queue::Durable("inventory"))
        .await
        .unwrap();
    bus.purge("inventory").await.unwrap(); // leftovers of other tests
    let mut st = bus.subscribe(KEY_STATE, Queue::Exclusive).await.unwrap();
    let (tx, states) = watch::channel(State::default());
    tokio::spawn(async move {
        while let Some(b) = st.next().await {
            if let Ok(s) = postcard::from_bytes::<State>(&b) {
                let _ = tx.send(s);
            }
        }
    });
    Env { _bus: bus, states }
}

/// Resources of the (only) fortress.
fn total(s: &State) -> Resources {
    s.totals.values().next().copied().unwrap_or_default()
}

impl Client {
    /// Pumps until a freshly published inventory state satisfies `ok` (bounded, ticks keep being read).
    async fn until_state(&mut self, env: &Env, ok: impl Fn(&State) -> bool) -> State {
        // Only states published from now on count: an older one may come from a dead service.
        let mut rx = env.states.clone();
        rx.borrow_and_update();
        for _ in 0..600 {
            if rx
                .has_changed()
                .unwrap_or_else(|_| panic!("state forwarder ended; last seen {:?}", *rx.borrow()))
            {
                let s = rx.borrow_and_update().clone();
                if ok(&s) {
                    return s;
                }
            }
            self.pump().await;
        }
        panic!("inventory state never matched; last {:?}", rx.borrow());
    }

    /// Hero digs `n` rock tiles, one after the other; returns the stone they yield.
    async fn dig(&mut self, n: u32) -> u32 {
        for _ in 0..n {
            if let Some(d) = self.step.take() {
                self.s
                    .write_all(&encode(&ClientMsg::Move(d)))
                    .await
                    .unwrap();
                self.ticks(4).await;
            }
            let pos = self.view.hero.pos;
            let dir = Dir::HORIZONTAL
                .into_iter()
                .find(|&d| self.view.mountain.get(pos.step(d)) == Some(Tile::Rock))
                .expect("rock next to the hero");
            let t = pos.step(dir);
            self.s
                .write_all(&encode(&ClientMsg::Dig(dir)))
                .await
                .unwrap();
            self.until(300, |c| c.view.mountain.get(t) != Some(Tile::Rock))
                .await;
            self.step = Some(dir); // entered at the next dig, so callers act right after the yield
        }
        n * Tile::Rock.yield_().stone
    }

    /// Hero builds one wall on a neighbouring floor tile (1 stone).
    async fn build_wall(&mut self) {
        let pos = self.view.hero.pos;
        let dir = Dir::HORIZONTAL
            .into_iter()
            .find(|&d| self.view.mountain.get(pos.step(d)) == Some(Tile::Floor))
            .expect("floor next to the hero");
        let t = pos.step(dir);
        self.s
            .write_all(&encode(&ClientMsg::Build(dir, DesignationKind::Wall)))
            .await
            .unwrap();
        self.until(100, |c| c.view.mountain.get(t) == Some(Tile::Wall))
            .await;
    }
}

#[tokio::test]
#[ignore]
async fn resources_flow_through_the_inventory() {
    let _g = SERIAL.lock().await;
    let data = world_file("inv-flow");
    let env = env().await;
    let inv = start_inventory(&inventory_file(&data));
    let (srv, addr) = start_server(&data).await;
    let mut c = Client::login(addr, "alice").await;
    let start = c.view.resources.stone;
    let expected = start + c.dig(2).await;
    assert!(expected > start, "digging yields stone");

    let s = c.until_state(&env, |s| total(s).stone == expected).await;
    assert_eq!(total(&s).stone, c.view.resources.stone);
    srv.stop().await;
    inv.stop().await;
    let disk = State::load_or_default(&inventory_file(&data)).unwrap();
    assert_eq!(total(&disk).stone, expected);
    rm(&data);
}

#[tokio::test]
#[ignore]
async fn inventory_outage_keeps_the_game_running() {
    let _g = SERIAL.lock().await;
    let data = world_file("inv-outage");
    let file = inventory_file(&data);
    let env = env().await;
    let inv = start_inventory(&file);
    let (srv, addr) = start_server(&data).await;
    let mut c = Client::login(addr, "alice").await;
    let mut expected = c.view.resources.stone;
    expected += c.dig(1).await;
    c.until_state(&env, |s| total(s).stone == expected).await;

    inv.stop().await;
    let t0 = c.tick;
    expected += c.dig(3).await;
    c.build_wall().await;
    expected -= 1;
    c.ticks(20).await;
    assert!(c.tick >= t0 + 20, "ticks keep advancing");
    assert_eq!(
        c.view.resources.stone, expected,
        "client keeps seeing its resources"
    );

    let inv = start_inventory(&file);
    let s = c.until_state(&env, |s| total(s).stone == expected).await;
    c.ticks(10).await;
    assert_eq!(total(&s).stone, c.view.resources.stone);
    assert_eq!(c.view.resources.stone, expected, "no stone counted twice");
    srv.stop().await;
    inv.stop().await;
    assert_eq!(
        total(&State::load_or_default(&file).unwrap()).stone,
        expected
    );
    rm(&data);
}

// Here we wait for the inventory to catch up before stopping; batches still unsent at stop
// are covered by `server_stop_right_after_a_dig_loses_nothing`.
#[tokio::test]
#[ignore]
async fn server_restart_does_not_drop_batches() {
    let _g = SERIAL.lock().await;
    let data = world_file("inv-srv-restart");
    let file = inventory_file(&data);
    let env = env().await;
    let inv = start_inventory(&file);
    let (srv, addr) = start_server(&data).await;
    let mut c = Client::login(addr, "alice").await;
    let mut expected = c.view.resources.stone;
    expected += c.dig(2).await;
    c.until_state(&env, |s| total(s).stone == expected).await;
    drop(c);
    srv.stop().await; // saves world.bin

    // The restarted server seeds again with the saved stock: ignored, the mountain is seeded.
    let (srv, addr) = start_server(&data).await;
    let mut c = Client::login(addr, "alice").await;
    c.until_state(&env, |s| total(s).stone == expected).await;
    c.until(100, |c| c.view.resources.stone == expected).await;
    expected += c.dig(2).await;
    // Reaching `expected` proves the new epoch was accepted, not dropped as stale.
    c.until_state(&env, |s| total(s).stone == expected).await;
    srv.stop().await;
    inv.stop().await;
    assert_eq!(
        total(&State::load_or_default(&file).unwrap()).stone,
        expected
    );
    rm(&data);
}

#[tokio::test]
#[ignore]
async fn inventory_killed_without_final_save_loses_nothing() {
    let _g = SERIAL.lock().await;
    let data = world_file("inv-killed");
    let file = inventory_file(&data);
    let env = env().await;
    let inv = start_inventory(&file);
    let (srv, addr) = start_server(&data).await;
    let mut c = Client::login(addr, "alice").await;
    let mut expected = c.view.resources.stone;
    expected += c.dig(1).await;
    c.until_state(&env, |s| total(s).stone == expected).await;
    // Burst without waiting for the inventory, then kill it right after the last yield, so
    // batches are in flight. Probabilistic: the ack-before-save ordering is also guarded
    // structurally by the `step` unit tests in moria-inventory.
    expected += c.dig(12).await;
    inv.done.abort(); // no clean shutdown, no final save
    let _ = inv.done.await;
    c.ticks(10).await;

    let inv = start_inventory(&file);
    let s = c.until_state(&env, |s| total(s).stone == expected).await;
    c.ticks(10).await;
    assert_eq!(total(&s).stone, c.view.resources.stone);
    srv.stop().await;
    inv.stop().await;
    assert_eq!(
        total(&State::load_or_default(&file).unwrap()).stone,
        expected
    );
    rm(&data);
}

#[tokio::test]
#[ignore]
async fn server_stop_right_after_a_dig_loses_nothing() {
    let _g = SERIAL.lock().await;
    let data = world_file("inv-stop-now");
    let file = inventory_file(&data);
    let env = env().await;
    let inv = start_inventory(&file);
    let (srv, addr) = start_server(&data).await;
    let mut c = Client::login(addr, "alice").await;
    let mut expected = c.view.resources.stone;
    expected += c.dig(12).await;
    // Stop IMMEDIATELY: the last batches may still be in the bridge's channel or unsent.
    drop(c);
    srv.stop().await;
    let mut rx = env.states.clone();
    let got = timeout(Duration::from_secs(30), async {
        loop {
            if total(&rx.borrow_and_update()).stone == expected {
                break;
            }
            rx.changed().await.expect("state forwarder ended");
        }
    })
    .await;
    assert!(
        got.is_ok(),
        "inventory never reached {expected}; last {:?}",
        rx.borrow()
    );
    inv.stop().await;
    assert_eq!(
        total(&State::load_or_default(&file).unwrap()).stone,
        expected
    );
    rm(&data);
}

#[tokio::test]
#[ignore]
async fn durable_confirm_latency_report() {
    let _g = SERIAL.lock().await;
    let data = world_file("inv-latency");
    let env = env().await;
    let inv = start_inventory(&inventory_file(&data));
    let bus = Bus::connect(&amqp_url()).await.unwrap();
    let mut ms = Vec::new();
    for seq in 1..=200u64 {
        let b = Batch {
            sender: "latency".into(),
            epoch: 1,
            seq,
            mountain: moria_core::MountainId(0),
            add: Resources::default(),
            take: Resources::default(),
            seed: false,
        };
        let bytes = postcard::to_allocvec(&b).unwrap();
        let t = std::time::Instant::now();
        bus.publish(KEY_APPLY, bytes, true).await.unwrap();
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg = ms.iter().sum::<f64>() / ms.len() as f64;
    eprintln!(
        "durable confirm latency over 200: min {:.2} avg {avg:.2} p95 {:.2} max {:.2} ms",
        ms[0], ms[189], ms[199]
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while env.states.borrow().acks.get("latency") != Some(&(1, 200)) {
        assert!(tokio::time::Instant::now() < deadline, "not all applied");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    inv.stop().await;
    rm(&data);
}
