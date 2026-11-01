//! AI bridge: publishes AI views, forwards decoded assignments to the world task.

use crate::metrics::{M, inc};
use futures::StreamExt;
use moria_bus::{Bus, Queue};
use moria_core::MountainId;
use moria_core::ai::{AiView, Assignments, KEY_ASSIGN, KEY_VIEW, MAX_DWARVES_PER_VIEW};
use std::collections::BTreeSet;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;
use tokio::sync::mpsc;

/// Untrusted input from the bus: drop forged/oversized assignments before they cost tick time.
pub fn accept(a: &Assignments, known: &BTreeSet<MountainId>) -> bool {
    if a.list.len() > MAX_DWARVES_PER_VIEW {
        tracing::warn!(
            "assignations ignorees: {} entrees (max {MAX_DWARVES_PER_VIEW})",
            a.list.len()
        );
        return false;
    }
    if !known.contains(&a.mountain) {
        tracing::warn!("assignations ignorees: montagne inconnue {:?}", a.mountain);
        return false;
    }
    true
}

/// Postcard-decodes untrusted bytes and applies `accept`.
pub fn decode_assignments(bytes: &[u8], known: &BTreeSet<MountainId>) -> Option<Assignments> {
    match postcard::from_bytes::<Assignments>(bytes) {
        Ok(a) if accept(&a, known) => Some(a),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("assignations illisibles ignorees: {e}");
            None
        }
    }
}

/// Runs until `views` closes. Connects with retry and reconnects when the subscription ends;
/// views are dropped while the broker is down.
pub async fn run(
    url: String,
    known: BTreeSet<MountainId>,
    mut views: mpsc::Receiver<AiView>,
    assign: mpsc::Sender<Assignments>,
) {
    loop {
        let bus = Bus::connect_forever(&url).await;
        let mut sub = match bus.subscribe(KEY_ASSIGN, Queue::Exclusive).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("abonnement impossible ({e}), nouvel essai dans 2 s");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        tracing::info!("pont IA connecte a {url}");
        M.nats_connected.store(1, Relaxed);
        loop {
            tokio::select! {
                v = views.recv() => {
                    let Some(v) = v else { return };
                    let up = bus.is_connected();
                    M.nats_connected.store(up as i64, Relaxed);
                    if !up {
                        continue;
                    }
                    match postcard::to_allocvec(&v) {
                        Ok(b) => match bus.publish(KEY_VIEW, b, false).await {
                            Ok(()) => inc(&M.views_published),
                            Err(e) => tracing::warn!("publication de vue echouee: {e}"),
                        },
                        Err(e) => tracing::error!("encodage de vue impossible: {e}"),
                    }
                }
                m = sub.next() => {
                    let Some(m) = m else {
                        tracing::warn!("abonnement termine, reconnexion");
                        break;
                    };
                    match decode_assignments(&m, &known) {
                        Some(a) => {
                            inc(&M.assignments_received);
                            // A full queue means the world task is behind: dropping is fine.
                            let _ = assign.try_send(a);
                        }
                        None => inc(&M.assignments_rejected),
                    }
                }
            }
        }
        M.nats_connected.store(0, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moria_core::{DwarfId, TilePos};

    #[test]
    fn filter_drops_oversized_and_unknown() {
        let known = BTreeSet::from([MountainId(0)]);
        let a = |m, n| Assignments {
            mountain: MountainId(m),
            tick: 0,
            list: vec![(DwarfId(0), TilePos::new(0, 0, 0)); n],
        };
        assert!(accept(&a(0, MAX_DWARVES_PER_VIEW), &known));
        assert!(!accept(&a(0, MAX_DWARVES_PER_VIEW + 1), &known));
        assert!(!accept(&a(1, 1), &known));
    }

    #[test]
    fn garbage_assignment_bytes_are_rejected() {
        let known = BTreeSet::from([MountainId(0)]);
        let enc = |n| {
            postcard::to_allocvec(&Assignments {
                mountain: MountainId(0),
                tick: 0,
                list: vec![(DwarfId(0), TilePos::new(0, 0, 0)); n],
            })
            .unwrap()
        };
        assert!(decode_assignments(&[0xff; 5], &known).is_none());
        assert!(decode_assignments(&enc(MAX_DWARVES_PER_VIEW + 1), &known).is_none());
        assert!(decode_assignments(&enc(MAX_DWARVES_PER_VIEW), &known).is_some());
    }
}
