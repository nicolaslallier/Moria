//! End-to-end tests: server + dwarf-ai in-process over a real NATS.
//! Run: NATS_TEST_URL=nats://127.0.0.1:4222 cargo test -p moria-server --test e2e -- --ignored
use moria_core::TilePos;
use moria_core::protocol::{
    ClientMsg, FortressView, PROTOCOL_VERSION, ServerMsg, encode, read_frame_async,
};
use moria_core::srd::{Class, Race};
use moria_core::tile::{DesignationKind, Tile};
use moria_core::world::{generate, save};
use moria_server::{Config, run_with_listener};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

// All tests share one NATS and its subjects: run them one at a time.
static SERIAL: Mutex<()> = Mutex::const_new(());

fn nats_url() -> String {
    std::env::var("NATS_TEST_URL").expect("NATS_TEST_URL")
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
    let done = tokio::spawn(moria_dwarf_ai::run(nats_url(), async {
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
        nats_url: Some(nats_url()),
        invite: None,
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
}

impl Client {
    async fn login(addr: SocketAddr, name: &str) -> Client {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(&encode(&ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            name: name.into(),
            password: "secret".into(),
            invite: None,
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
        let mut c: Vec<TilePos> = (0..48)
            .flat_map(|x| (0..48).map(move |y| TilePos { x, y, z: d.z }))
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
