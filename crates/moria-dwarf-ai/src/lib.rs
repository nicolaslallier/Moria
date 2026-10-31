pub mod assign;

use futures::StreamExt;
use moria_core::ai::{AiView, Assignments, SUBJECT_ASSIGN, SUBJECT_VIEW};
use std::{future::Future, time::Duration};

/// Connect (retrying forever, 2 s backoff) and serve views until `shutdown` resolves.
pub async fn run(nats_url: String, shutdown: impl Future<Output = ()>) -> anyhow::Result<()> {
    tokio::pin!(shutdown);
    let client = loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            r = async_nats::connect(&nats_url) => match r {
                Ok(c) => break c,
                Err(e) => tracing::warn!("NATS indisponible ({e}), nouvel essai dans 2 s"),
            },
        }
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    };
    let mut sub = client
        .queue_subscribe(SUBJECT_VIEW, "dwarf-ai".into())
        .await?;
    tracing::info!("dwarf-ai connecte a {nats_url}");
    loop {
        let msg = tokio::select! {
            _ = &mut shutdown => return Ok(()),
            m = sub.next() => match m { Some(m) => m, None => anyhow::bail!("abonnement NATS termine") },
        };
        let view: AiView = match postcard::from_bytes(&msg.payload) {
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
                if let Err(e) = client.publish(SUBJECT_ASSIGN, b.into()).await {
                    tracing::warn!("publication echouee: {e}");
                }
            }
            Err(e) => tracing::error!("encodage impossible: {e}"),
        }
    }
}
