//! Necessite un broker : voir `AMQP_TEST_URL` (docker run -d --rm --name moria-amqp-test -p 5672:5672 rabbitmq:4-alpine).
//! Lancer avec `-- --ignored --test-threads=1`.

use futures::StreamExt;
use moria_bus::{Bus, Queue};
use moria_core::calendar::SEASON_TICKS;
use moria_core::food::{Cell, CellKind, FoodEvent, FoodStore};
use moria_core::food_msg::{
    Events, KEY_ACTION, KEY_EVENTS, KEY_REJECT, KEY_STATE, KEY_TICK, KEY_WORLD, Status, Tick, World,
};
use moria_core::{MountainId, TilePos};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const M: MountainId = MountainId(7);

fn url() -> String {
    std::env::var("AMQP_TEST_URL")
        .unwrap_or_else(|_| "amqp://guest:guest@127.0.0.1:5672/%2f".into())
}

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("moria-food-{}-{name}.bin", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

struct Svc(oneshot::Sender<()>, JoinHandle<anyhow::Result<()>>);

fn start(data: PathBuf) -> Svc {
    let (tx, rx) = oneshot::channel::<()>();
    let h = tokio::spawn(moria_food::run(url(), data, async {
        let _ = rx.await;
    }));
    Svc(tx, h)
}

impl Svc {
    async fn stop(self) {
        let _ = self.0.send(());
        self.1.await.unwrap().unwrap();
    }
}

async fn send<T: serde::Serialize>(bus: &Bus, key: &str, msg: &T) {
    bus.publish(key, postcard::to_allocvec(msg).unwrap(), false)
        .await
        .unwrap();
}

/// Files du service declarees et videes (restes d'un test interrompu).
async fn setup() -> Bus {
    let bus = Bus::connect(&url()).await.unwrap();
    for (key, name, latest_only) in [
        (KEY_TICK, "food-tick", true),
        (KEY_WORLD, "food-world", true),
        (KEY_ACTION, "food-action", false),
        (KEY_REJECT, "food-reject", false),
    ] {
        bus.declare(key, Queue::Shared { name, latest_only })
            .await
            .unwrap();
        bus.purge(name).await.unwrap();
    }
    bus
}

fn cells(kind: CellKind, n: i16) -> Vec<Cell> {
    (0..n)
        .map(|i| Cell {
            pos: TilePos::new(2 + 3 * i, 5, 0),
            kind,
        })
        .collect()
}

/// Publie (instantane eventuel puis tick) toutes les 50 ms, ticks `from + k * every`, jusqu'au
/// premier `Events` verifiant `ok`, sous 15 s.
async fn drive(
    bus: &Bus,
    events: &mut (impl futures::Stream<Item = Vec<u8>> + Unpin),
    world: Option<&World>,
    from: u64,
    every: u64,
    ok: impl Fn(&Events) -> bool,
) -> Events {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut k = 1;
        loop {
            if let Some(w) = world {
                send(bus, KEY_WORLD, w).await;
            }
            send(
                bus,
                KEY_TICK,
                &Tick {
                    tick: from + k * every,
                },
            )
            .await;
            k += 1;
            if let Ok(Some(b)) =
                tokio::time::timeout(Duration::from_millis(50), events.next()).await
            {
                let e: Events = postcard::from_bytes(&b).unwrap();
                if ok(&e) {
                    return e;
                }
            }
        }
    })
    .await
    .expect("evenements attendus jamais recus")
}

#[tokio::test]
#[ignore]
async fn world_in_events_out() {
    let file = tmp("world");
    let bus = setup().await;
    let mut events = bus.subscribe(KEY_EVENTS, Queue::Exclusive).await.unwrap();
    let svc = start(file.clone());
    let w = World {
        tick: 0,
        mountains: vec![(M, cells(CellKind::Cow, 2))],
    };
    let e = drive(&bus, &mut events, Some(&w), 0, 10, |e| e.mountain == M).await;
    assert!(!e.list.is_empty());
    svc.stop().await;
    // Sauvegarde a l'arret ; des naissances ont pu s'ajouter, aucune vache ne se perd.
    assert!(FoodStore::load_or_default(&file).unwrap().mountains[&M].cow_count() >= 2);
}

#[tokio::test]
#[ignore]
async fn heartbeat_is_published_every_second() {
    let file = tmp("beat");
    let bus = setup().await;
    let mut st = bus.subscribe(KEY_STATE, Queue::Exclusive).await.unwrap();
    let svc = start(file);
    let mut at = vec![];
    tokio::time::timeout(Duration::from_secs(15), async {
        while at.len() < 4 {
            let b = st.next().await.expect("flux ferme");
            let _: Status = postcard::from_bytes(&b).unwrap();
            at.push(Instant::now());
        }
    })
    .await
    .expect("battements jamais recus");
    svc.stop().await;
    let span = at[3] - at[0];
    assert!(
        span >= Duration::from_millis(2500) && span <= Duration::from_millis(4500),
        "{span:?}"
    );
}

#[tokio::test]
#[ignore]
async fn restart_keeps_hidden_state() {
    let file = tmp("restart");
    let bus = setup().await;
    let mut events = bus.subscribe(KEY_EVENTS, Queue::Exclusive).await.unwrap();
    // Ete : un champ murit dans le conteneur.
    let svc = start(file.clone());
    let w = World {
        tick: SEASON_TICKS,
        mountains: vec![(M, cells(CellKind::Field, 40))],
    };
    let ripe = |e: &Events| {
        e.list.iter().find_map(|x| match x {
            FoodEvent::CropRipe { at } => Some(*at),
            _ => None,
        })
    };
    let e = drive(&bus, &mut events, Some(&w), SEASON_TICKS, 100, |e| {
        ripe(e).is_some()
    })
    .await;
    let at = ripe(&e).unwrap();
    // Comme le serveur : la grille dit maintenant `Crop` (l'instantane fait foi sur la maturite).
    // Le meme ordre dans la file garantit que ce dernier instantane est adopte apres l'ancien.
    let mut w = w;
    for c in &mut w.mountains[0].1 {
        if c.pos == at {
            c.kind = CellKind::Crop;
        }
    }
    drive(&bus, &mut events, Some(&w), e.tick, 100, |e| {
        e.list.contains(&FoodEvent::CropRipe { at })
    })
    .await;
    svc.stop().await;
    assert!(FoodStore::load_or_default(&file).unwrap().mountains[&M].is_ripe(at));

    // Redemarrage en hiver (rien ne pousse) : sans instantane, battement a 0 et aucun evenement,
    // `food.bin` peut etre perime.
    bus.purge("food-world").await.unwrap();
    while tokio::time::timeout(Duration::from_millis(300), events.next())
        .await
        .is_ok()
    {}
    let before = FoodStore::load_or_default(&file).unwrap().mountains[&M].clone();
    let winter = 3 * SEASON_TICKS;
    let mut st = bus.subscribe(KEY_STATE, Queue::Exclusive).await.unwrap();
    let svc = start(file.clone());
    let b = tokio::time::timeout(Duration::from_secs(5), st.next())
        .await
        .expect("battement jamais recu")
        .unwrap();
    assert_eq!(postcard::from_bytes::<Status>(&b).unwrap().tick, 0);
    for k in 1..=10 {
        send(
            &bus,
            KEY_TICK,
            &Tick {
                tick: winter + k * 100,
            },
        )
        .await;
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(1), events.next())
            .await
            .is_err(),
        "evenement propose avant le premier World"
    );
    // L'instantane arrive : la recolte est redemandee et l'etat cache a survecu.
    let w = World { tick: winter, ..w };
    drive(&bus, &mut events, Some(&w), winter, 100, |e| {
        e.tick > winter && e.list.contains(&FoodEvent::CropRipe { at })
    })
    .await;
    svc.stop().await;
    let mut kept = before;
    kept.adopt(&w.mountains[0].1, winter);
    let mut fresh = moria_core::food::FoodSim::new(moria_food::seed(M));
    fresh.adopt(&w.mountains[0].1, winter);
    assert_ne!(kept, fresh, "aucun etat cache a garder");
    assert_eq!(
        FoodStore::load_or_default(&file).unwrap().mountains[&M],
        kept
    );
}
