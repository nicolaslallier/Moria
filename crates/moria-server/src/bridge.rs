//! NATS bridge: publishes AI views, forwards decoded assignments to the world task.

use crate::metrics::{M, inc};
use futures::StreamExt;
use moria_core::MountainId;
use moria_core::ai::{AiView, Assignments, KEY_ASSIGN, KEY_VIEW, MAX_DWARVES_PER_VIEW};
use std::collections::BTreeSet;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;
use tokio::sync::mpsc;

/// Untrusted input from NATS: drop forged/oversized assignments before they cost tick time.
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

/// Runs until `views` closes. Connects with retry; views are dropped while NATS is down.
pub async fn run(
    url: String,
    known: BTreeSet<MountainId>,
    mut views: mpsc::Receiver<AiView>,
    assign: mpsc::Sender<Assignments>,
) {
    let (client, mut sub) = loop {
        let r = match async_nats::connect(&url).await {
            Ok(c) => match c.subscribe(KEY_ASSIGN).await {
                Ok(sub) => Ok((c, sub)),
                Err(e) => Err(format!("abonnement impossible ({e})")),
            },
            Err(e) => Err(format!("NATS indisponible ({e})")),
        };
        match r {
            Ok(x) => break x,
            Err(e) => {
                tracing::warn!("{e}, nouvel essai dans 2 s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
    tracing::info!("pont NATS connecte a {url}");
    M.nats_connected.store(1, Relaxed);
    loop {
        tokio::select! {
            v = views.recv() => {
                let Some(v) = v else { return };
                let up = client.connection_state() == async_nats::connection::State::Connected;
                M.nats_connected.store(up as i64, Relaxed);
                if !up {
                    continue;
                }
                match postcard::to_allocvec(&v) {
                    Ok(b) => {
                        match client.publish(KEY_VIEW, b.into()).await {
                            Ok(()) => inc(&M.views_published),
                            Err(e) => tracing::warn!("publication de vue echouee: {e}"),
                        }
                    }
                    Err(e) => tracing::error!("encodage de vue impossible: {e}"),
                }
            }
            m = sub.next() => {
                let Some(m) = m else {
                    tracing::error!("abonnement NATS termine");
                    return;
                };
                match postcard::from_bytes::<Assignments>(&m.payload) {
                    Ok(a) if accept(&a, &known) => {
                        inc(&M.assignments_received);
                        // A full queue means the world task is behind: dropping is fine.
                        let _ = assign.try_send(a);
                    }
                    Ok(_) => inc(&M.assignments_rejected),
                    Err(e) => {
                        inc(&M.assignments_rejected);
                        tracing::warn!("assignations illisibles ignorees: {e}");
                    }
                }
            }
        }
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
}
