//! Necessite un broker : voir `AMQP_TEST_URL` (docker run -d --rm --name moria-amqp-test -p 5672:5672 rabbitmq:4-alpine).
//! Lancer avec `-- --ignored --test-threads=1`.

use futures::StreamExt;
use moria_bus::{Bus, Queue};
use moria_core::king::{Ask, KEY_ASK, KEY_REPLY, KingState, Lang, QuestId, Reply};
use moria_core::{MountainId, Resources};
use moria_king::{ChatMessage, Llm};
use std::time::Duration;

fn url() -> String {
    std::env::var("AMQP_TEST_URL")
        .unwrap_or_else(|_| "amqp://guest:guest@127.0.0.1:5672/%2f".into())
}

struct Fake;

impl Llm for Fake {
    async fn chat(&self, _: &[ChatMessage]) -> anyhow::Result<String> {
        Ok(r#"{"say":"Bienvenue.","offer":"food"}"#.into())
    }
}

#[tokio::test]
#[ignore]
async fn ask_in_reply_out() {
    let bus = Bus::connect(&url()).await.unwrap();
    let king = Queue::Shared {
        name: "king",
        latest_only: false,
    };
    bus.declare(KEY_ASK, king).await.unwrap();
    bus.purge("king").await.unwrap();
    let mut replies = bus.subscribe(KEY_REPLY, Queue::Exclusive).await.unwrap();

    let (stop, rx) = tokio::sync::oneshot::channel::<()>();
    let svc = tokio::spawn(moria_king::run(url(), Fake, async {
        let _ = rx.await;
    }));
    let ask = Ask {
        mountain: MountainId(2),
        seq: 9,
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
    };
    bus.publish(KEY_ASK, postcard::to_allocvec(&ask).unwrap(), false)
        .await
        .unwrap();
    let body = tokio::time::timeout(Duration::from_secs(15), replies.next())
        .await
        .expect("aucune reponse")
        .expect("flux ferme");
    let r: Reply = postcard::from_bytes(&body).unwrap();
    assert_eq!(
        r,
        Reply {
            mountain: MountainId(2),
            seq: 9,
            text: "Bienvenue.".into(),
            offer: Some(QuestId::Food),
        }
    );
    let _ = stop.send(());
    svc.await.unwrap().unwrap();
}
