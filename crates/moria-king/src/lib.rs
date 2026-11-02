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
    sync::Arc,
    time::{Duration, Instant},
};
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

/// Taille maximale d'une question sur le bus, vérifiée avant décodage.
pub const MAX_ASK_BYTES: usize = 64 * 1024;
/// Au-delà, le serveur (70 s) a déjà envoyé son repli : inutile d'occuper Ollama.
const STALE: Duration = Duration::from_secs(60);

fn decode_ask(payload: &[u8]) -> Option<Ask> {
    if payload.len() > MAX_ASK_BYTES {
        tracing::warn!("question trop grosse ignorée ({} octets)", payload.len());
        return None;
    }
    postcard::from_bytes(payload)
        .inspect_err(|e| tracing::warn!("question illisible ignorée: {e}"))
        .ok()
}

/// `None` si la question a trop attendu : on l'abandonne sans appeler le LLM.
async fn handle(ask: &Ask, received: Instant, llm: &impl Llm) -> Option<Reply> {
    if received.elapsed() > STALE {
        tracing::debug!(mountain = ask.mountain.0, seq = ask.seq, "question périmée");
        return None;
    }
    Some(answer(ask, llm).await)
}

/// Attend une place libre (contre-pression sur la réception), puis traite la question
/// dans sa propre tâche : au plus `IN_FLIGHT` tâches à la fois.
async fn dispatch<L: Llm + Send + Sync + 'static>(
    ask: Ask,
    received: Instant,
    llm: &Arc<L>,
    slots: &Arc<Semaphore>,
    tx: &mpsc::Sender<Reply>,
) {
    let Ok(permit) = slots.clone().acquire_owned().await else {
        return;
    };
    let (llm, tx) = (llm.clone(), tx.clone());
    tokio::spawn(async move {
        let reply = handle(&ask, received, &*llm).await;
        // La place est rendue avant l'envoi : la boucle peut attendre une place sans
        // bloquer les tâches qui lui rendent leur réponse.
        drop(permit);
        drop(llm);
        if let Some(r) = reply {
            let _ = tx.send(r).await;
        }
    });
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
        tracing::info!("king connecté à RabbitMQ");
        loop {
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                Some(reply) = rx.recv() => {
                    let Ok(b) = postcard::to_allocvec(&reply) else {
                        tracing::error!("encodage de la réponse impossible");
                        continue;
                    };
                    if let Err(e) = bus.publish(KEY_REPLY, b, false).await {
                        tracing::warn!("publication échouée ({e}), reconnexion dans 2 s");
                        tokio::select! {
                            _ = &mut shutdown => return Ok(()),
                            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                        }
                        break;
                    }
                }
                m = sub.next() => {
                    let Some(payload) = m else {
                        tracing::warn!("abonnement terminé, reconnexion");
                        break;
                    };
                    let received = Instant::now();
                    let Some(ask) = decode_ask(&payload) else { continue };
                    tokio::select! {
                        _ = &mut shutdown => return Ok(()),
                        _ = dispatch(ask, received, &llm, &slots, &tx) => {}
                    }
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
    use std::sync::atomic::{AtomicUsize, Ordering};
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

    /// Compte les appels simultanés et garde le maximum observé.
    #[derive(Default)]
    struct Counting {
        now: AtomicUsize,
        max: AtomicUsize,
        calls: AtomicUsize,
    }

    impl Llm for Counting {
        async fn chat(&self, _: &[ChatMessage]) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(30)).await;
            self.now.fetch_sub(1, Ordering::SeqCst);
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

    #[tokio::test]
    async fn stale_ask_is_skipped_without_calling_the_llm() {
        let llm = Counting::default();
        let old = Instant::now() - STALE - Duration::from_secs(1);
        assert_eq!(handle(&ask(), old, &llm).await, None);
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
        assert!(handle(&ask(), Instant::now(), &llm).await.is_some());
        assert_eq!(llm.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_llm_calls_never_exceed_the_limit() {
        let llm = Arc::new(Counting::default());
        let slots = Arc::new(Semaphore::new(IN_FLIGHT));
        let (tx, mut rx) = mpsc::channel(16);
        for seq in 0..7 {
            let mut a = ask();
            a.seq = seq;
            // Bloque tant que IN_FLIGHT questions sont en cours : contre-pression.
            dispatch(a, Instant::now(), &llm, &slots, &tx).await;
            assert!(Arc::strong_count(&llm) <= IN_FLIGHT + 1);
        }
        let mut seqs = Vec::new();
        for _ in 0..7 {
            seqs.push(rx.recv().await.unwrap().seq);
        }
        seqs.sort();
        assert_eq!(seqs, (0..7).collect::<Vec<_>>());
        assert_eq!(llm.max.load(Ordering::SeqCst), IN_FLIGHT);
    }

    #[tokio::test]
    async fn answer_replies_with_same_mountain_and_seq() {
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
