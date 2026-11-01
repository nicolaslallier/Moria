//! Service d'inventaire : seul auteur de l'etat, il applique les lots et diffuse `inventory.state`.

use anyhow::Context;
use futures::StreamExt;
use moria_bus::{Bus, Queue, redact_url};
use moria_core::inventory::{Batch, KEY_APPLY, KEY_STATE, STATE_EVERY, State};
use std::future::Future;
use std::path::{Path, PathBuf};
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

/// Traite un message : `Ok(true)` lot applique ET sauvegarde, `Ok(false)` ignore (illisible,
/// perime, doublon), `Err` sauvegarde impossible (`state` est alors revenu a sa valeur d'avant).
pub fn step(state: &mut State, data: &Path, payload: &[u8]) -> Result<bool, std::io::Error> {
    let prev = state.clone();
    if !handle(state, payload) {
        return Ok(false);
    }
    if let Err(e) = state.save(data) {
        // La memoire ne doit pas devancer le disque.
        *state = prev;
        return Err(e);
    }
    Ok(true)
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
    // Sonde d'ecriture : un chemin inscriptible est requis, sinon chaque lot echouerait
    // sans ack et bloquerait la file en silence.
    state.save(&data).with_context(|| {
        format!(
            "ecriture impossible dans {}, refus de demarrer",
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
        let mut stop = false;
        match bus
            .subscribe_acked(KEY_APPLY, Queue::Durable("inventory"))
            .await
        {
            Err(e) => tracing::warn!("abonnement impossible: {e:#}"),
            Ok(mut deliveries) => loop {
                tokio::select! {
                    () = &mut shutdown => {
                        stop = true;
                        break;
                    }
                    _ = tick.tick() => {
                        if !bus.is_connected() { break; }
                        publish_state(&bus, &state).await;
                    }
                    d = deliveries.next() => {
                        let Some(d) = d else { break };
                        match step(&mut state, &data, &d.body) {
                            // Sans ack, le broker redistribue a la fermeture du canal.
                            Err(e) => {
                                tracing::error!("sauvegarde impossible, lot non acquitte: {e}");
                                break;
                            }
                            Ok(applied) => {
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
                }
            },
        }
        if stop {
            // Sauvegarde d'abord : la fermeture du broker ne doit pas pouvoir la retarder.
            let r = save(&state, &data);
            let _ = tokio::time::timeout(Duration::from_secs(2), bus.close()).await;
            return r;
        }
        tokio::select! {
            () = bus.close() => {}
            () = &mut shutdown => return save(&state, &data),
        }
        tracing::warn!("flux interrompu, reconnexion");
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(2)) => {}
            () = &mut shutdown => return save(&state, &data),
        }
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

    fn payload() -> Vec<u8> {
        postcard::to_allocvec(&batch()).unwrap()
    }

    fn good_path(name: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("moria-inv-unit-{}-{name}.bin", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn bad_path() -> PathBuf {
        std::env::temp_dir()
            .join("moria-inv-absent-dir")
            .join("x.bin")
    }

    #[test]
    fn save_failure_reverts_memory() {
        let mut s = State::default();
        let before = s.clone();
        assert!(step(&mut s, &bad_path(), &payload()).is_err());
        assert_eq!(s, before);
    }

    #[test]
    fn redelivery_after_save_failure_applies_exactly_once() {
        let mut s = State::default();
        assert!(step(&mut s, &bad_path(), &payload()).is_err());
        let ok = good_path("redeliver");
        assert!(step(&mut s, &ok, &payload()).unwrap());
        assert!(!step(&mut s, &ok, &payload()).unwrap());
        assert_eq!(s.totals[&MountainId(0)].stone, 4);
        assert_eq!(State::load_or_default(&ok).unwrap(), s);
    }

    #[test]
    fn duplicate_payload_is_not_saved_again() {
        let mut s = State::default();
        let p = good_path("dup");
        assert!(step(&mut s, &p, &payload()).unwrap());
        std::fs::remove_file(&p).unwrap();
        assert!(!step(&mut s, &p, &payload()).unwrap());
        assert!(
            !p.exists(),
            "un doublon ne doit pas declencher de sauvegarde"
        );
    }

    #[test]
    fn garbage_payload_step_is_ok_false() {
        let mut s = State::default();
        let p = good_path("garbage");
        assert!(!step(&mut s, &p, &[0xff; 7]).unwrap());
        assert_eq!(s, State::default());
        assert!(!p.exists());
    }

    #[tokio::test]
    async fn refuses_to_start_when_data_dir_is_missing() {
        // Echoue avant toute connexion : l'URL n'est jamais jointe.
        let r = tokio::time::timeout(
            Duration::from_secs(3),
            run(
                "amqp://x:y@127.0.0.1:1/".into(),
                bad_path(),
                std::future::pending(),
            ),
        )
        .await
        .expect("run aurait du refuser de demarrer");
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn refuses_to_start_on_corrupt_file() {
        let p = good_path("corrupt");
        std::fs::write(&p, [0xffu8; 9]).unwrap();
        let r = tokio::time::timeout(
            Duration::from_secs(3),
            run("amqp://x:y@127.0.0.1:1/".into(), p, std::future::pending()),
        )
        .await
        .expect("run aurait du refuser de demarrer");
        assert!(r.is_err());
    }
}
