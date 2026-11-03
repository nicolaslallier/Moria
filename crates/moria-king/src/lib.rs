//! `moria-king` : sans état, répond aux questions `king.ask` par `king.reply` via un LLM.
pub mod ollama;
pub mod prompt;

pub use ollama::Ollama;
pub use prompt::{ChatMessage, build_messages, parse_reply};

use futures::StreamExt;
use moria_bus::{Bus, Queue};
use moria_core::king::{Ask, KEY_ASK, KEY_REPLY, Reply};
use std::{
    future::Future,
    time::{Duration, Instant},
};

pub trait Llm {
    /// `timeout` : délai maximal de l'appel, au plus `OLLAMA_TIMEOUT`.
    fn chat(
        &self,
        msgs: &[ChatMessage],
        timeout: Duration,
    ) -> impl Future<Output = anyhow::Result<String>> + Send;
}

/// Une question, une réponse : même montagne et même `seq` ; toute erreur donne le repli.
pub async fn answer(ask: &Ask, llm: &impl Llm, timeout: Duration) -> Reply {
    let raw = match llm.chat(&build_messages(ask), timeout).await {
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

/// Taille maximale d'une question sur le bus, vérifiée avant décodage.
pub const MAX_ASK_BYTES: usize = 64 * 1024;
/// Plafond d'un appel à Ollama, pas objectif.
pub const OLLAMA_TIMEOUT: Duration = Duration::from_secs(60);
/// Le serveur envoie son repli 70 s après la question ; 2 s de marge pour la réponse.
const SERVER_BUDGET: Duration = Duration::from_secs(70 - 2);
/// En dessous, le LLM n'a pas le temps de répondre : inutile de l'occuper.
const MIN_BUDGET: Duration = Duration::from_secs(3);

fn decode_ask(payload: &[u8]) -> Option<Ask> {
    if payload.len() > MAX_ASK_BYTES {
        tracing::warn!("question trop grosse ignorée ({} octets)", payload.len());
        return None;
    }
    postcard::from_bytes(payload)
        .inspect_err(|e| tracing::warn!("question illisible ignorée: {e}"))
        .ok()
}

/// Le LLM n'a que le temps restant avant le repli du serveur (`age` = attente depuis la
/// livraison) ; `None` s'il en reste trop peu : on abandonne sans l'appeler.
async fn handle(ask: &Ask, age: Duration, llm: &impl Llm) -> Option<Reply> {
    let left = SERVER_BUDGET.saturating_sub(age);
    if left < MIN_BUDGET {
        tracing::debug!(mountain = ask.mountain.0, seq = ask.seq, "question périmée");
        return None;
    }
    Some(answer(ask, llm, left.min(OLLAMA_TIMEOUT)).await)
}

/// Répond aux questions jusqu'à `shutdown` ; se reconnecte (2 s) quand le bus tombe.
/// Une question à la fois par instance (prefetch 1, acquittée une fois traitée) : le reste
/// attend dans le broker, où le serveur le fait expirer à 70 s. Pour plus de débit, lancer
/// plusieurs instances sur la file partagée. Une réponse perdue est sans gravité : le
/// serveur envoie son repli.
pub async fn run(
    amqp_url: String,
    llm: impl Llm + Send + Sync + 'static,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
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
        let mut sub = match bus.subscribe_acked(KEY_ASK, queue).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("abonnement impossible ({e}), nouvel essai dans 2 s");
                tokio::select! {
                    _ = &mut shutdown => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(2)) => continue,
                }
            }
        };
        tracing::info!("king connecté à RabbitMQ");
        loop {
            let d = tokio::select! {
                _ = &mut shutdown => {
                    bus.close().await;
                    return Ok(());
                }
                m = sub.next() => match m {
                    Some(d) => d,
                    None => {
                        tracing::warn!("abonnement terminé, reconnexion");
                        break;
                    }
                },
            };
            let received = Instant::now();
            // Illisible ou périmée : pas de réponse, mais acquittée (jamais redistribuée).
            let reply = match decode_ask(&d.body) {
                Some(ask) => tokio::select! {
                    // Arrêt en pleine réflexion : non acquittée, une autre instance la reprend.
                    _ = &mut shutdown => {
                        bus.close().await;
                        return Ok(());
                    }
                    r = handle(&ask, received.elapsed(), &llm) => r,
                },
                None => None,
            };
            if let Some(reply) = reply {
                let res = match postcard::to_allocvec(&reply) {
                    Ok(b) => bus.publish(KEY_REPLY, b, false).await,
                    Err(e) => Err(e.into()),
                };
                if let Err(e) = res {
                    tracing::warn!("publication échouée ({e}), reconnexion dans 2 s");
                    tokio::select! {
                        _ = &mut shutdown => return Ok(()),
                        _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                    }
                    break;
                }
            }
            if let Err(e) = d.ack().await {
                tracing::warn!("ack impossible ({e:#}), reconnexion");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::tests::ask;
    use moria_core::king::{FALLBACK_FR, QuestId};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct Fake(anyhow::Result<String>);

    impl Llm for Fake {
        async fn chat(&self, msgs: &[ChatMessage], _: Duration) -> anyhow::Result<String> {
            assert!(msgs.len() >= 2);
            match &self.0 {
                Ok(s) => Ok(s.clone()),
                Err(e) => Err(anyhow::anyhow!("{e}")),
            }
        }
    }

    /// Compte les appels et note le délai reçu.
    #[derive(Default)]
    struct Counting {
        calls: AtomicUsize,
        timeout: std::sync::Mutex<Option<Duration>>,
    }

    impl Llm for Counting {
        async fn chat(&self, _: &[ChatMessage], timeout: Duration) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.timeout.lock().unwrap() = Some(timeout);
            Ok(r#"{"say":"ok","offer":null}"#.into())
        }
    }

    #[test]
    fn oversized_payload_is_ignored() {
        let ok = postcard::to_allocvec(&ask()).unwrap();
        assert_eq!(decode_ask(&ok), Some(ask()));
        assert_eq!(decode_ask(&vec![0u8; MAX_ASK_BYTES + 1]), None);
        assert_eq!(decode_ask(b"\xff\xff"), None);
    }

    /// Délai passé au LLM pour une question livrée il y a `age` ; `None` = pas d'appel.
    async fn budget(age: u64) -> Option<Duration> {
        let llm = Counting::default();
        let r = handle(&ask(), Duration::from_secs(age), &llm).await;
        assert_eq!(r.is_some(), llm.calls.load(Ordering::SeqCst) == 1);
        *llm.timeout.lock().unwrap()
    }

    #[tokio::test]
    async fn fresh_ask_gets_the_full_ollama_timeout() {
        assert_eq!(budget(0).await, Some(OLLAMA_TIMEOUT));
    }

    #[tokio::test]
    async fn llm_budget_shrinks_with_age() {
        // 70 s du serveur - 20 s d'attente - 2 s de marge.
        assert_eq!(budget(20).await, Some(Duration::from_secs(48)));
        assert_eq!(budget(65).await, Some(Duration::from_secs(3)));
    }

    #[tokio::test]
    async fn ask_delivered_68s_ago_is_skipped_without_calling_the_llm() {
        assert_eq!(budget(68).await, None);
        assert_eq!(budget(66).await, None);
    }

    #[tokio::test]
    async fn answer_replies_with_same_mountain_and_seq() {
        let a = ask();
        let r = answer(
            &a,
            &Fake(Ok(r#"{"say":"Creuse.","offer":"depth"}"#.into())),
            OLLAMA_TIMEOUT,
        )
        .await;
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
        let r = answer(&a, &Fake(Err(anyhow::anyhow!("panne"))), OLLAMA_TIMEOUT).await;
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
        };
        let a = ask();
        let r = tokio::time::timeout(
            Duration::from_secs(5),
            answer(&a, &o, Duration::from_millis(300)),
        )
        .await
        .expect("le délai d'Ollama n'a pas été appliqué");
        assert_eq!((r.mountain, r.seq), (a.mountain, a.seq));
        assert_eq!((r.text.as_str(), r.offer), (FALLBACK_FR, None));
    }
}
