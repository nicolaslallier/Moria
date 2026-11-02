//! `moria-king` : sans état, répond aux questions `king.ask` par `king.reply` via un LLM.
pub mod ollama;
pub mod prompt;

pub use ollama::Ollama;
pub use prompt::{ChatMessage, build_messages, parse_reply};

use futures::StreamExt;
use moria_bus::{Bus, Queue};
use moria_core::king::{Ask, KEY_ASK, KEY_REPLY, Reply};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::sync::{Semaphore, mpsc};

/// Questions traitées en parallèle (une lente ne bloque pas les autres).
// ponytail: plafond fixe, à rendre réglable si Ollama tient plus de charge.
const IN_FLIGHT: usize = 2;

pub trait Llm {
    fn chat(&self, msgs: &[ChatMessage]) -> impl Future<Output = anyhow::Result<String>> + Send;
}

/// Une question, une réponse : même montagne et même `seq` ; toute erreur donne le repli.
pub async fn answer(ask: &Ask, llm: &impl Llm) -> Reply {
    let raw = match llm.chat(&build_messages(ask)).await {
        Ok(raw) => raw,
        Err(e) => {
            tracing::warn!("LLM en échec ({e:#}), réponse de repli");
            String::new()
        }
    };
    tracing::debug!(mountain = ask.mountain.0, seq = ask.seq, %raw, "sortie du LLM");
    let (text, offer) = parse_reply(&raw, ask);
    Reply {
        mountain: ask.mountain,
        seq: ask.seq,
        text,
        offer,
    }
}

/// Répond aux questions jusqu'à `shutdown` ; se reconnecte (2 s) quand le bus tombe.
/// Une réponse perdue est sans gravité : le serveur envoie son repli au bout de 70 s.
pub async fn run(
    amqp_url: String,
    llm: impl Llm + Send + Sync + 'static,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let llm = Arc::new(llm);
    let slots = Arc::new(Semaphore::new(IN_FLIGHT));
    // Les tâches rendent leur réponse ici ; seule la boucle publie (et gère la reconnexion).
    let (tx, mut rx) = mpsc::channel::<Reply>(16);
    tokio::pin!(shutdown);
    loop {
        let bus = tokio::select! {
            _ = &mut shutdown => return Ok(()),
            b = Bus::connect_forever(&amqp_url) => b,
        };
        let queue = Queue::Shared {
            name: "king",
            latest_only: false,
        };
        let mut sub = match bus.subscribe(KEY_ASK, queue).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("abonnement impossible ({e}), nouvel essai dans 2 s");
                tokio::select! {
                    _ = &mut shutdown => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(2)) => continue,
                }
            }
        };
        tracing::info!("king connecte a RabbitMQ");
        loop {
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                Some(reply) = rx.recv() => {
                    let Ok(b) = postcard::to_allocvec(&reply) else {
                        tracing::error!("encodage de la réponse impossible");
                        continue;
                    };
                    if let Err(e) = bus.publish(KEY_REPLY, b, false).await {
                        tracing::warn!("publication echouee ({e}), reconnexion dans 2 s");
                        tokio::select! {
                            _ = &mut shutdown => return Ok(()),
                            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                        }
                        break;
                    }
                }
                m = sub.next() => {
                    let Some(payload) = m else {
                        tracing::warn!("abonnement termine, reconnexion");
                        break;
                    };
                    let ask: Ask = match postcard::from_bytes(&payload) {
                        Ok(a) => a,
                        Err(e) => {
                            tracing::warn!("question illisible ignoree: {e}");
                            continue;
                        }
                    };
                    let (llm, slots, tx) = (llm.clone(), slots.clone(), tx.clone());
                    tokio::spawn(async move {
                        let Ok(_permit) = slots.acquire_owned().await else { return };
                        let _ = tx.send(answer(&ask, &*llm).await).await;
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::tests::ask;
    use moria_core::king::{FALLBACK_FR, QuestId};
    use std::time::Duration;

    struct Fake(anyhow::Result<String>);

    impl Llm for Fake {
        async fn chat(&self, msgs: &[ChatMessage]) -> anyhow::Result<String> {
            assert!(msgs.len() >= 2);
            match &self.0 {
                Ok(s) => Ok(s.clone()),
                Err(e) => Err(anyhow::anyhow!("{e}")),
            }
        }
    }

    #[tokio::test]
    async fn run_replies_with_same_mountain_and_seq() {
        let a = ask();
        let r = answer(&a, &Fake(Ok(r#"{"say":"Creuse.","offer":"depth"}"#.into()))).await;
        assert_eq!(
            r,
            Reply {
                mountain: a.mountain,
                seq: a.seq,
                text: "Creuse.".into(),
                offer: Some(QuestId::Depth),
            }
        );
    }

    #[tokio::test]
    async fn llm_error_gives_fallback_reply() {
        let a = ask();
        let r = answer(&a, &Fake(Err(anyhow::anyhow!("panne")))).await;
        assert_eq!((r.seq, r.text.as_str(), r.offer), (42, FALLBACK_FR, None));
    }

    #[tokio::test]
    async fn ollama_timeout_gives_fallback_reply() {
        // Serveur muet : accepte la connexion et ne répond jamais.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            let (_s, _) = l.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let o = Ollama {
            url,
            model: "m".into(),
            timeout: Duration::from_millis(300),
        };
        let a = ask();
        let r = tokio::time::timeout(Duration::from_secs(5), answer(&a, &o))
            .await
            .expect("le délai d'Ollama n'a pas été appliqué");
        assert_eq!((r.mountain, r.seq), (a.mountain, a.seq));
        assert_eq!((r.text.as_str(), r.offer), (FALLBACK_FR, None));
    }
}
