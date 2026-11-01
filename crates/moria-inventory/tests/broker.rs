//! Necessite un broker : voir `AMQP_TEST_URL` (docker run -d --rm --name moria-amqp-test -p 5672:5672 rabbitmq:4-alpine).
//! Lancer avec `-- --ignored --test-threads=1`.

use futures::StreamExt;
use moria_bus::{Bus, Queue};
use moria_core::inventory::{Batch, KEY_APPLY, KEY_STATE, State};
use moria_core::{MountainId, Resources};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

fn url() -> String {
    std::env::var("AMQP_TEST_URL")
        .unwrap_or_else(|_| "amqp://guest:guest@127.0.0.1:5672/%2f".into())
}

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("moria-inv-{}-{name}.bin", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn batch(seq: u64, stone: u32) -> Batch {
    Batch {
        sender: "t".into(),
        epoch: 1,
        seq,
        mountain: MountainId(0),
        add: Resources {
            stone,
            ..Default::default()
        },
        take: Resources::default(),
        seed: false,
    }
}

struct Svc(oneshot::Sender<()>, JoinHandle<anyhow::Result<()>>);

fn start(data: PathBuf) -> Svc {
    let (tx, rx) = oneshot::channel::<()>();
    let h = tokio::spawn(moria_inventory::run(url(), data, async {
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

async fn publish(bus: &Bus, b: &Batch) {
    bus.publish(KEY_APPLY, postcard::to_allocvec(b).unwrap(), true)
        .await
        .unwrap();
}

/// Premier etat recu verifiant `ok`, sous 15 s.
async fn state_where(
    st: &mut (impl futures::Stream<Item = Vec<u8>> + Unpin),
    ok: impl Fn(&State) -> bool,
) -> State {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let s: State = postcard::from_bytes(&st.next().await.expect("flux ferme")).unwrap();
            if ok(&s) {
                return s;
            }
        }
    })
    .await
    .expect("etat attendu jamais recu")
}

/// Connexion, file `inventory` declaree et videe (restes d'un test interrompu), flux d'etats.
async fn setup() -> (Bus, impl futures::Stream<Item = Vec<u8>> + Unpin) {
    let bus = Bus::connect(&url()).await.unwrap();
    bus.declare(KEY_APPLY, Queue::Durable("inventory"))
        .await
        .unwrap();
    bus.purge("inventory").await.unwrap();
    let st = bus.subscribe(KEY_STATE, Queue::Exclusive).await.unwrap();
    (bus, st)
}

fn stone(s: &State) -> u32 {
    s.totals.get(&MountainId(0)).map_or(0, |r| r.stone)
}

#[tokio::test]
#[ignore]
async fn batch_in_state_out() {
    let file = tmp("batch");
    let (bus, mut st) = setup().await;
    let svc = start(file.clone());
    publish(&bus, &batch(1, 5)).await;
    let s = state_where(&mut st, |s| stone(s) == 5).await;
    svc.stop().await;
    assert_eq!(State::load_or_default(&file).unwrap(), s);
    // Service arrete : un message non acquitte serait revenu dans la file.
    assert_eq!(
        bus.queue_len("inventory").await.unwrap(),
        0,
        "lot non acquitte"
    );
}

#[tokio::test]
#[ignore]
async fn crash_before_ack_redelivers_without_double_apply() {
    let file = tmp("dup");
    let (bus, mut st) = setup().await;
    let svc = start(file.clone());
    publish(&bus, &batch(1, 1)).await;
    publish(&bus, &batch(1, 1)).await;
    // Le lot-sentinelle (seq 2, rien a ajouter) suit le doublon dans la file : son ack
    // prouve que le doublon a ete consomme.
    publish(&bus, &batch(2, 0)).await;
    let s = state_where(&mut st, |s| s.acks.get("t") == Some(&(1, 2))).await;
    assert_eq!(stone(&s), 1);
    svc.stop().await;
    let disk = State::load_or_default(&file).unwrap();
    assert_eq!(stone(&disk), 1);
    assert_eq!(disk.acks["t"], (1, 2));
    assert_eq!(bus.queue_len("inventory").await.unwrap(), 0);

    // Seconde instance sur le meme fichier : le lot rejoue est ignore.
    let svc = start(file.clone());
    publish(&bus, &batch(1, 1)).await;
    publish(&bus, &batch(3, 0)).await;
    let s = state_where(&mut st, |s| s.acks.get("t") == Some(&(1, 3))).await;
    assert_eq!(stone(&s), 1);
    svc.stop().await;
    let disk = State::load_or_default(&file).unwrap();
    assert_eq!(stone(&disk), 1);
    assert_eq!(disk.acks["t"], (1, 3));
    assert_eq!(bus.queue_len("inventory").await.unwrap(), 0);
}
