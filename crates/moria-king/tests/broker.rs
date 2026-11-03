//! Necessite un broker : voir `AMQP_TEST_URL` (docker run -d --rm --name moria-amqp-test -p 5672:5672 rabbitmq:4-alpine).
//! Lancer avec `-- --ignored --test-threads=1`.

use futures::StreamExt;
use moria_bus::{Bus, Queue};
use moria_core::king::{Ask, KEY_ASK, KEY_REPLY, KingState, Lang, QuestId, Reply};
use moria_core::{MountainId, Resources};
use moria_king::{ChatMessage, Llm};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinHandle;

fn url() -> String {
    std::env::var("AMQP_TEST_URL")
        .unwrap_or_else(|_| "amqp://guest:guest@127.0.0.1:5672/%2f".into())
}

struct Fake;

impl Llm for Fake {
    async fn chat(&self, _: &[ChatMessage], _: Duration) -> anyhow::Result<String> {
        Ok(r#"{"say":"Bienvenue.","offer":"food"}"#.into())
    }
}

/// LLM bloqué jusqu'à ce que le test ouvre la porte ; compte les appels simultanés.
#[derive(Clone)]
struct Gated {
    gate: Arc<Semaphore>,
    started: Arc<AtomicUsize>,
    now: Arc<AtomicUsize>,
    max: Arc<AtomicUsize>,
}

fn gated() -> Gated {
    Gated {
        gate: Arc::new(Semaphore::new(0)),
        started: Default::default(),
        now: Default::default(),
        max: Default::default(),
    }
}

impl Llm for Gated {
    async fn chat(&self, _: &[ChatMessage], _: Duration) -> anyhow::Result<String> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(n, Ordering::SeqCst);
        self.gate.acquire().await?.forget();
        self.now.fetch_sub(1, Ordering::SeqCst);
        Ok(r#"{"say":"ok","offer":null}"#.into())
    }
}

fn ask(seq: u64) -> Ask {
    Ask {
        mountain: MountainId(2),
        seq,
        lang: Lang::Fr,
        text: "Bonjour".into(),
        history: vec![],
        state: KingState {
            resources: Resources::default(),
            dwarves: 3,
            depth: 0,
        },
        active: None,
        offered: vec![QuestId::Food],
    }
}

struct Svc(oneshot::Sender<()>, JoinHandle<anyhow::Result<()>>);

fn start(llm: impl Llm + Send + Sync + 'static) -> Svc {
    let (tx, rx) = oneshot::channel::<()>();
    let h = tokio::spawn(moria_king::run(url(), llm, async {
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

/// File `king` déclarée et vidée, flux des réponses.
async fn setup() -> (Bus, impl futures::Stream<Item = Vec<u8>> + Unpin) {
    let bus = Bus::connect(&url()).await.unwrap();
    let king = Queue::Shared {
        name: "king",
        latest_only: false,
    };
    bus.declare(KEY_ASK, king).await.unwrap();
    bus.purge("king").await.unwrap();
    let replies = bus.subscribe(KEY_REPLY, Queue::Exclusive).await.unwrap();
    (bus, replies)
}

async fn publish(bus: &Bus, body: Vec<u8>) {
    bus.publish(KEY_ASK, body, false).await.unwrap();
}

async fn next_reply(st: &mut (impl futures::Stream<Item = Vec<u8>> + Unpin)) -> Reply {
    let body = tokio::time::timeout(Duration::from_secs(15), st.next())
        .await
        .expect("aucune reponse")
        .expect("flux ferme");
    postcard::from_bytes(&body).unwrap()
}

/// Attend (5 s max) que `ok` devienne vrai.
async fn until(what: &str, ok: impl Fn() -> bool) {
    for _ in 0..100 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("jamais vrai: {what}");
}

async fn queue_len_becomes(bus: &Bus, n: u32) {
    for _ in 0..100 {
        if bus.queue_len("king").await.unwrap() == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "file king: {} au lieu de {n}",
        bus.queue_len("king").await.unwrap()
    );
}

#[tokio::test]
#[ignore]
async fn ask_in_reply_out() {
    let (bus, mut replies) = setup().await;
    let svc = start(Fake);
    publish(&bus, postcard::to_allocvec(&ask(9)).unwrap()).await;
    assert_eq!(
        next_reply(&mut replies).await,
        Reply {
            mountain: MountainId(2),
            seq: 9,
            text: "Bienvenue.".into(),
            offer: Some(QuestId::Food),
        }
    );
    svc.stop().await;
    queue_len_becomes(&bus, 0).await;
}

#[tokio::test]
#[ignore]
async fn ask_is_acked_only_after_answer() {
    let (bus, mut replies) = setup().await;
    let llm = gated();
    let svc = start(llm.clone());
    publish(&bus, postcard::to_allocvec(&ask(1)).unwrap()).await;
    until("appel au LLM", || llm.started.load(Ordering::SeqCst) == 1).await;
    // Arrêt en pleine réflexion : la question non acquittée revient dans la file.
    svc.stop().await;
    queue_len_becomes(&bus, 1).await;

    let svc = start(Fake);
    assert_eq!(next_reply(&mut replies).await.seq, 1);
    svc.stop().await;
    queue_len_becomes(&bus, 0).await;
}

#[tokio::test]
#[ignore]
async fn undecodable_payload_is_acked_and_not_answered() {
    let (bus, mut replies) = setup().await;
    let svc = start(Fake);
    publish(&bus, vec![0xff, 0xff, 0xff]).await;
    publish(&bus, postcard::to_allocvec(&ask(7)).unwrap()).await;
    // Avec un seul message en vol, un déchet non acquitté bloquerait la question suivante.
    assert_eq!(next_reply(&mut replies).await.seq, 7);
    assert!(
        tokio::time::timeout(Duration::from_millis(500), replies.next())
            .await
            .is_err(),
        "le déchet a reçu une réponse"
    );
    svc.stop().await;
    queue_len_becomes(&bus, 0).await;
}

#[tokio::test]
#[ignore]
async fn concurrent_llm_calls_never_exceed_the_limit() {
    let (bus, mut replies) = setup().await;
    let llm = gated();
    let svc = start(llm.clone());
    for seq in 0..3 {
        publish(&bus, postcard::to_allocvec(&ask(seq)).unwrap()).await;
    }
    until("appel au LLM", || llm.started.load(Ordering::SeqCst) == 1).await;
    // Prefetch 1 : les deux autres questions restent dans le broker.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(bus.queue_len("king").await.unwrap(), 2);
    llm.gate.add_permits(3);
    let mut seqs = Vec::new();
    for _ in 0..3 {
        seqs.push(next_reply(&mut replies).await.seq);
    }
    assert_eq!(seqs, vec![0, 1, 2]);
    assert_eq!(llm.max.load(Ordering::SeqCst), 1);
    svc.stop().await;
    queue_len_becomes(&bus, 0).await;
}
