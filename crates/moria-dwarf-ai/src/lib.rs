pub mod assign;

use futures::StreamExt;
use moria_bus::{Bus, Queue};
use moria_core::ai::{AiView, Assignments, KEY_ASSIGN, KEY_VIEW};
use std::{future::Future, time::Duration};

/// Serve views until `shutdown` resolves; reconnects (2 s backoff) whenever the bus drops.
pub async fn run(amqp_url: String, shutdown: impl Future<Output = ()>) -> anyhow::Result<()> {
    tokio::pin!(shutdown);
    loop {
        let bus = tokio::select! {
            _ = &mut shutdown => return Ok(()),
            b = Bus::connect_forever(&amqp_url) => b,
        };
        let queue = Queue::Shared {
            name: "dwarf-ai",
            latest_only: true,
        };
        let mut sub = match bus.subscribe(KEY_VIEW, queue).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("abonnement impossible ({e}), nouvel essai dans 2 s");
                tokio::select! {
                    _ = &mut shutdown => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(2)) => continue,
                }
            }
        };
        tracing::info!("dwarf-ai connecte a RabbitMQ");
        loop {
            let payload = tokio::select! {
                _ = &mut shutdown => return Ok(()),
                m = sub.next() => match m {
                    Some(m) => m,
                    None => {
                        tracing::warn!("abonnement termine, reconnexion");
                        break;
                    }
                },
            };
            let view: AiView = match postcard::from_bytes(&payload) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("vue illisible ignoree: {e}");
                    continue;
                }
            };
            if let Err(e) = assign::validate(&view) {
                tracing::warn!("vue invalide ignoree: {e}");
                continue;
            }
            let (mountain, tick) = (view.mountain.id, view.tick);
            let list = match tokio::task::spawn_blocking(move || assign::assign(&view)).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("assignation echouee: {e}");
                    continue;
                }
            };
            if list.is_empty() {
                continue;
            }
            let out = Assignments {
                mountain,
                tick,
                list,
            };
            match postcard::to_allocvec(&out) {
                Ok(b) => {
                    if let Err(e) = bus.publish(KEY_ASSIGN, b, false).await {
                        tracing::warn!("publication echouee: {e}");
                        if !bus.is_connected() {
                            break;
                        }
                    }
                }
                Err(e) => tracing::error!("encodage impossible: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use moria_core::ai::{KEY_ASSIGN, KEY_VIEW};

    #[test]
    fn keys_are_the_documented_ones() {
        assert_eq!(KEY_VIEW, "ai.view");
        assert_eq!(KEY_ASSIGN, "ai.assign");
    }
}
