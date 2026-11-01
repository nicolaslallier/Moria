//! AI bridge: publishes AI views, forwards decoded assignments to the world task.

use crate::metrics::{M, inc};
use futures::StreamExt;
use moria_bus::{Bus, Queue};
use moria_core::MountainId;
use moria_core::ai::{AiView, Assignments, KEY_ASSIGN, KEY_VIEW, MAX_DWARVES_PER_VIEW};
use moria_core::inventory::{Batch, KEY_APPLY, KEY_STATE, State};
use std::collections::{BTreeSet, VecDeque};
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

/// A legal assignment is ~1 KB (64 dwarves x ~14 bytes + header).
const MAX_ASSIGNMENT_BYTES: usize = 4096;

/// Postcard-decodes untrusted bytes and applies `accept`.
pub fn decode_assignments(bytes: &[u8], known: &BTreeSet<MountainId>) -> Option<Assignments> {
    if bytes.len() > MAX_ASSIGNMENT_BYTES {
        tracing::warn!("assignations ignorees: {} octets", bytes.len());
        return None;
    }
    match postcard::from_bytes::<Assignments>(bytes) {
        Ok(a) if accept(&a, known) => Some(a),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("assignations illisibles ignorees: {e}");
            None
        }
    }
}

/// Most unsent batches kept while the broker is down; beyond it the oldest are dropped.
pub const MAX_UNSENT: usize = 100_000;

/// An inventory state is ~100 bytes per fortress; far above that is not ours.
const MAX_STATE_BYTES: usize = 64 * 1024;

pub fn push_unsent(q: &mut VecDeque<Batch>, b: Batch) {
    if q.len() >= MAX_UNSENT {
        q.pop_front();
        tracing::error!("lots d'inventaire non envoyes: file pleine, le plus ancien est perdu");
    }
    q.push_back(b);
}

/// Postcard-decodes an untrusted `inventory.state` payload.
pub fn decode_state(bytes: &[u8]) -> Option<State> {
    if bytes.len() > MAX_STATE_BYTES {
        tracing::warn!("etat d'inventaire ignore: {} octets", bytes.len());
        return None;
    }
    match postcard::from_bytes::<State>(bytes) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("etat d'inventaire illisible ignore: {e}");
            None
        }
    }
}

/// Publishes unsent batches in order until one fails (it stays at the front for the next try).
async fn flush(bus: &Bus, unsent: &mut VecDeque<Batch>) {
    while let Some(b) = unsent.front() {
        let bytes = match postcard::to_allocvec(b) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("encodage de lot impossible, perdu: {e}");
                unsent.pop_front();
                continue;
            }
        };
        if let Err(e) = bus.publish(KEY_APPLY, bytes, true).await {
            tracing::warn!(
                "publication de lot echouee ({} en attente): {e}",
                unsent.len()
            );
            // Best effort, once per failed attempt: the queue may have been deleted mid-run.
            // No broker-free seam exists to test this.
            if let Err(e) = bus.declare(KEY_APPLY, Queue::Durable("inventory")).await {
                tracing::warn!("redeclaration de la file inventaire impossible: {e}");
            }
            return;
        }
        unsent.pop_front();
    }
}

/// Runs until `views` closes. Connects with retry and reconnects when the subscription ends;
/// views are dropped while the broker is down. Inventory batches are kept in `unsent` and
/// re-sent in order. Limitation: while the bridge is inside `connect_forever` or a publish
/// confirm (up to 10 s), `batches` is not drained, so the real cap there is the 65_536-slot
/// channel, where the world side drops the NEWEST batches (counter
/// `moria_inventory_dropped_total`). `MAX_UNSENT` (oldest dropped) only applies once the
/// bridge is running again.
pub async fn run(
    url: String,
    known: BTreeSet<MountainId>,
    mut views: mpsc::Receiver<AiView>,
    assign: mpsc::Sender<Assignments>,
    mut batches: mpsc::Receiver<Batch>,
    states: mpsc::Sender<State>,
) {
    let mut unsent: VecDeque<Batch> = VecDeque::new();
    loop {
        let bus = Bus::connect_forever(&url).await;
        // A durable publish to an unbound key errors, so the queue must exist first.
        if let Err(e) = bus.declare(KEY_APPLY, Queue::Durable("inventory")).await {
            tracing::warn!(
                "declaration de la file inventaire impossible ({e}), nouvel essai dans 2 s"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        let (mut sub, mut state_sub) = match (
            bus.subscribe(KEY_ASSIGN, Queue::Exclusive).await,
            bus.subscribe(KEY_STATE, Queue::Exclusive).await,
        ) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!("abonnement impossible ({e}), nouvel essai dans 2 s");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        tracing::info!("pont IA connecte a {}", moria_bus::redact_url(&url));
        M.bus_up.store(1, Relaxed);
        flush(&bus, &mut unsent).await;
        let mut retry = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                v = views.recv() => {
                    let Some(v) = v else { return };
                    let up = bus.is_connected();
                    M.bus_up.store(up as i64, Relaxed);
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
                b = batches.recv() => {
                    let Some(b) = b else { return };
                    push_unsent(&mut unsent, b);
                    if bus.is_connected() {
                        flush(&bus, &mut unsent).await;
                    }
                }
                _ = retry.tick(), if !unsent.is_empty() && bus.is_connected() => {
                    flush(&bus, &mut unsent).await;
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
                m = state_sub.next() => {
                    let Some(m) = m else {
                        tracing::warn!("abonnement inventaire termine, reconnexion");
                        break;
                    };
                    // Dropped when full: the next heartbeat replaces it.
                    if let Some(st) = decode_state(&m) {
                        let _ = states.try_send(st);
                    }
                }
            }
        }
        M.bus_up.store(0, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moria_core::{DwarfId, TilePos};

    fn batch(seq: u64) -> Batch {
        Batch {
            sender: "server".into(),
            epoch: 1,
            seq,
            mountain: MountainId(0),
            add: Default::default(),
            take: Default::default(),
            seed: false,
        }
    }

    #[test]
    fn unsent_list_is_capped_oldest_first() {
        let mut q = VecDeque::new();
        for i in 0..(MAX_UNSENT + 3) as u64 {
            push_unsent(&mut q, batch(i));
        }
        assert_eq!(q.len(), MAX_UNSENT);
        assert_eq!(q.front().unwrap().seq, 3);
        assert_eq!(q.back().unwrap().seq, (MAX_UNSENT + 2) as u64);
    }

    #[test]
    fn garbage_state_bytes_are_ignored() {
        assert!(decode_state(&[0xff; 9]).is_none());
        let ok = postcard::to_allocvec(&State::default()).unwrap();
        assert!(decode_state(&ok).is_some());
        let mut big = ok.clone();
        big.resize(MAX_STATE_BYTES + 1, 0);
        // trailing bytes would decode fine, so only the cap can reject this
        assert!(postcard::from_bytes::<State>(&big).is_ok());
        assert!(decode_state(&big).is_none());
    }

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

    #[test]
    fn oversized_payload_is_rejected_before_decoding() {
        let known = BTreeSet::from([MountainId(0)]);
        let mut b = postcard::to_allocvec(&Assignments {
            mountain: MountainId(0),
            tick: 0,
            list: vec![],
        })
        .unwrap();
        b.resize(MAX_ASSIGNMENT_BYTES + 1, 0);
        // trailing bytes would decode fine, so only the cap can reject this
        assert!(postcard::from_bytes::<Assignments>(&b).is_ok());
        assert!(decode_assignments(&b, &known).is_none());
    }
}
