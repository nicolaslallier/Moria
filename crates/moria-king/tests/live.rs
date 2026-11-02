//! Vrai Ollama : `OLLAMA_URL=http://192.168.2.35:11434 cargo test -p moria-king --test live -- --ignored --nocapture`.
//! Un seul appel : ne pas marteler le serveur.

use moria_core::king::{Ask, Exchange, FALLBACK_FR, KingState, Lang, MAX_REPLY_CHARS, QuestId};
use moria_core::{MountainId, Resources};
use moria_king::{Llm, Ollama, build_messages, parse_reply};
use std::time::{Duration, Instant};

#[tokio::test]
#[ignore]
async fn real_ollama_answers_in_json() {
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.into());
    let o = Ollama {
        url: env("OLLAMA_URL", "http://192.168.2.35:11434"),
        model: env("OLLAMA_MODEL", "gemma4:26b-a4b-it-qat"),
        timeout: Duration::from_secs(60),
    };
    let ask = Ask {
        mountain: MountainId(0),
        seq: 1,
        lang: Lang::Fr,
        text: "Ô roi, comment obtenir des lingots de fer ? As-tu une tâche pour moi ?".into(),
        history: vec![Exchange {
            player: "Bonjour, majesté.".into(),
            king: "Sois le bienvenu dans ma salle, héros.".into(),
        }],
        state: KingState {
            resources: Resources {
                stone: 30,
                iron_ore: 4,
                coal: 2,
                food: 6,
                ..Default::default()
            },
            dwarves: 5,
            depth: 1,
        },
        active: None,
        offered: vec![QuestId::ForgeIngots, QuestId::Food, QuestId::Depth],
    };
    let t = Instant::now();
    let raw = o.chat(&build_messages(&ask)).await.expect("appel Ollama");
    println!("latence {:?}\nbrut: {raw}", t.elapsed());
    let (say, offer) = parse_reply(&raw, &ask);
    println!("say: {say}\noffer: {offer:?}");
    assert!(
        serde_json::from_str::<serde_json::Value>(raw.trim()).is_ok(),
        "pas du JSON pur"
    );
    assert_ne!(say, FALLBACK_FR);
    assert!(say.chars().count() <= MAX_REPLY_CHARS);
}
