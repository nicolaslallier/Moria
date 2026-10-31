//! NATS bridge: publishes AI views, forwards decoded assignments to the world task.

use futures::StreamExt;
use moria_core::MountainId;
use moria_core::ai::{AiView, Assignments, SUBJECT_ASSIGN, SUBJECT_VIEW};
use std::collections::BTreeSet;
use std::time::Duration;
use tokio::sync::mpsc;

/// Same cap as `moria_dwarf_ai::assign::MAX_DWARVES` (dwarves per view).
pub const MAX_ASSIGNMENTS: usize = 64;

/// Untrusted input from NATS: drop forged/oversized assignments before they cost tick time.
pub fn accept(a: &Assignments, known: &BTreeSet<MountainId>) -> bool {
    if a.list.len() > MAX_ASSIGNMENTS {
        tracing::warn!(
            "assignations ignorees: {} entrees (max {MAX_ASSIGNMENTS})",
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
    let client = loop {
        match async_nats::connect(&url).await {
            Ok(c) => break c,
            Err(e) => {
                tracing::warn!("NATS indisponible ({e}), nouvel essai dans 2 s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
    let mut sub = match client.subscribe(SUBJECT_ASSIGN).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("abonnement NATS impossible: {e}");
            return;
        }
    };
    tracing::info!("pont NATS connecte a {url}");
    loop {
        tokio::select! {
            v = views.recv() => {
                let Some(v) = v else { return };
                if client.connection_state() != async_nats::connection::State::Connected {
                    continue;
                }
                match postcard::to_allocvec(&v) {
                    Ok(b) => {
                        if let Err(e) = client.publish(SUBJECT_VIEW, b.into()).await {
                            tracing::warn!("publication de vue echouee: {e}");
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
                        // A full queue means the world task is behind: dropping is fine.
                        let _ = assign.try_send(a);
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("assignations illisibles ignorees: {e}"),
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
        assert_eq!(MAX_ASSIGNMENTS, moria_dwarf_ai::assign::MAX_DWARVES);
        let known = BTreeSet::from([MountainId(0)]);
        let a = |m, n| Assignments {
            mountain: MountainId(m),
            tick: 0,
            list: vec![(DwarfId(0), TilePos::new(0, 0, 0)); n],
        };
        assert!(accept(&a(0, MAX_ASSIGNMENTS), &known));
        assert!(!accept(&a(0, MAX_ASSIGNMENTS + 1), &known));
        assert!(!accept(&a(1, 1), &known));
    }
}
