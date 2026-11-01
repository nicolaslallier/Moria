//! Service d'inventaire : seul auteur de l'etat, il applique les lots et diffuse `inventory.state`.

use anyhow::Context;
use futures::StreamExt;
use moria_bus::{Bus, Queue, redact_url};
use moria_core::inventory::{Batch, KEY_APPLY, KEY_STATE, STATE_EVERY, State};
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

/// Decode et applique un lot ; `false` si illisible, perime ou doublon.
pub fn handle(state: &mut State, payload: &[u8]) -> bool {
    match postcard::from_bytes::<Batch>(payload) {
        Ok(b) => state.apply(&b),
        Err(e) => {
            tracing::warn!("lot illisible ignore ({} octets): {e}", payload.len());
            false
        }
    }
}

async fn publish_state(bus: &Bus, state: &State) {
    match postcard::to_allocvec(state) {
        Ok(b) => {
            if let Err(e) = bus.publish(KEY_STATE, b, false).await {
                tracing::warn!("publication de l'etat impossible: {e:#}");
            }
        }
        Err(e) => tracing::error!("etat non serialisable: {e}"),
    }
}

/// Un message n'est acquitte qu'apres la sauvegarde du lot qu'il porte : l'etat publie ne
/// depasse jamais le disque, et un lot non sauvegarde est redistribue (le dedoublonnage
/// rend le rejeu sans danger).
pub async fn run(
    amqp_url: String,
    data: PathBuf,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let mut state = State::load_or_default(&data).with_context(|| {
        format!(
            "inventaire illisible ({}), refus de demarrer",
            data.display()
        )
    })?;
    tracing::info!("inventaire charge, broker {}", redact_url(&amqp_url));
    tokio::pin!(shutdown);
    let mut tick = tokio::time::interval(STATE_EVERY);
    loop {
        let bus = tokio::select! {
            b = Bus::connect_forever(&amqp_url) => b,
            () = &mut shutdown => return save(&state, &data),
        };
        let mut deliveries = match bus
            .subscribe_acked(KEY_APPLY, Queue::Durable("inventory"))
            .await
        {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("abonnement impossible: {e:#}");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        loop {
            tokio::select! {
                () = &mut shutdown => {
                    bus.close().await;
                    return save(&state, &data);
                }
                _ = tick.tick() => {
                    if !bus.is_connected() { break; }
                    publish_state(&bus, &state).await;
                }
                d = deliveries.next() => {
                    let Some(d) = d else { break };
                    let prev = state.clone();
                    let applied = handle(&mut state, &d.body);
                    if applied && let Err(e) = state.save(&data) {
                        // La memoire ne doit pas devancer le disque ; sans ack, le broker
                        // redistribue a la fermeture du canal.
                        tracing::error!("sauvegarde impossible, lot non acquitte: {e}");
                        state = prev;
                        break;
                    }
                    if let Err(e) = d.ack().await {
                        tracing::warn!("ack impossible: {e:#}");
                        break;
                    }
                    if applied {
                        publish_state(&bus, &state).await;
                    }
                }
            }
        }
        bus.close().await;
        tracing::warn!("flux interrompu, reconnexion");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn save(state: &State, data: &std::path::Path) -> anyhow::Result<()> {
    state.save(data).context("sauvegarde finale impossible")
}

#[cfg(test)]
mod tests {
    use super::*;
    use moria_core::{MountainId, Resources};

    fn batch() -> Batch {
        Batch {
            sender: "a".into(),
            epoch: 1,
            seq: 1,
            mountain: MountainId(0),
            add: Resources {
                stone: 4,
                ..Default::default()
            },
            take: Resources::default(),
            seed: false,
        }
    }

    #[test]
    fn garbage_payload_is_ignored() {
        let mut s = State::default();
        assert!(!handle(&mut s, &[0xff; 7]));
        assert_eq!(s, State::default());
    }

    #[test]
    fn handle_applies_a_valid_batch() {
        let mut s = State::default();
        let bytes = postcard::to_allocvec(&batch()).unwrap();
        assert!(handle(&mut s, &bytes));
        assert_eq!(s.totals[&MountainId(0)].stone, 4);
        assert!(!handle(&mut s, &bytes), "doublon");
    }
}
