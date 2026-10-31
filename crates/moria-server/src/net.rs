//! One TCP session: handshake, auth (argon2 runs here, never on the tick task), then
//! frames in both directions until either side ends.

use crate::sim::Command;
use crate::{Lookup, Request};
use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use moria_core::PlayerId;
use moria_core::protocol::{
    ClientMsg, PROTOCOL_VERSION, Reject, ServerMsg, encode, read_frame_async,
};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

pub const SESSION_QUEUE: usize = 64;
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// A client that does not drain its socket for this long is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Handles {
    pub req: mpsc::Sender<Request>,
    pub cmd: mpsc::Sender<(PlayerId, Command)>,
}

pub fn valid_name(n: &str) -> bool {
    (3..=16).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

async fn reject(wr: &mut OwnedWriteHalf, r: Reject) -> anyhow::Result<()> {
    wr.write_all(&encode(&ServerMsg::Reject(r))).await?;
    Ok(())
}

async fn ask<T>(h: &Handles, make: impl FnOnce(oneshot::Sender<T>) -> Request) -> Option<T> {
    let (tx, rx) = oneshot::channel();
    h.req.send(make(tx)).await.ok()?;
    rx.await.ok()
}

/// Runs argon2 off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

pub async fn session(stream: TcpStream, h: Handles) -> anyhow::Result<()> {
    let (mut rd, mut wr) = stream.into_split();
    let ClientMsg::Hello {
        version,
        name,
        password,
    } = timeout(HELLO_TIMEOUT, read_frame_async(&mut rd)).await??
    else {
        return Ok(());
    };
    if version != PROTOCOL_VERSION {
        return reject(
            &mut wr,
            Reject::VersionMismatch {
                server: PROTOCOL_VERSION,
            },
        )
        .await;
    }
    if !valid_name(&name) {
        return reject(&mut wr, Reject::InvalidName).await;
    }
    let lookup = ask(&h, |reply| Request::Lookup {
        name: name.clone(),
        reply,
    })
    .await;
    let new_hash = match lookup {
        None => return Ok(()), // server shutting down
        Some(Lookup::Full) => return reject(&mut wr, Reject::ServerFull).await,
        Some(Lookup::New) => {
            let hash = blocking(move || {
                Argon2::default()
                    .hash_password(password.as_bytes())
                    .map(|p| p.to_string())
            })
            .await;
            match hash {
                Some(Ok(hash)) => Some(hash),
                _ => anyhow::bail!("hachage du mot de passe impossible"),
            }
        }
        Some(Lookup::Existing(stored)) => {
            let ok = blocking(move || {
                PasswordHash::new(&stored).is_ok_and(|p| {
                    Argon2::default()
                        .verify_password(password.as_bytes(), &p)
                        .is_ok()
                })
            })
            .await;
            if ok != Some(true) {
                return reject(&mut wr, Reject::BadPassword).await;
            }
            None
        }
    };
    let (tx, mut rx) = mpsc::channel(SESSION_QUEUE);
    let login = ask(&h, |reply| Request::Login {
        name,
        new_hash,
        tx,
        reply,
    })
    .await;
    let player = match login {
        None => return Ok(()),
        Some(Err(r)) => return reject(&mut wr, r).await,
        Some(Ok(p)) => p,
    };
    // The world task queued Welcome (and Snapshot if the hero exists) on `tx`. When the
    // world drops `tx` (kicked, slow, shutdown) the writer ends and so does the session.
    let writer = async {
        while let Some(m) = rx.recv().await {
            timeout(WRITE_TIMEOUT, wr.write_all(&encode(&m))).await??;
        }
        anyhow::Ok(())
    };
    let reader = async {
        loop {
            match read_frame_async::<ClientMsg>(&mut rd).await? {
                ClientMsg::CreateHero { race, class } => {
                    let r = Request::CreateHero {
                        player,
                        race,
                        class,
                    };
                    h.req
                        .send(r)
                        .await
                        .map_err(|_| anyhow::anyhow!("serveur arrete"))?;
                }
                m => {
                    if let Some(c) = Command::from_client(m) {
                        h.cmd
                            .send((player, c))
                            .await
                            .map_err(|_| anyhow::anyhow!("serveur arrete"))?;
                    }
                }
            }
        }
    };
    tokio::select! {
        r = writer => r,
        r = reader => r,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn names() {
        use super::valid_name;
        assert!(valid_name("Gimli_fils-de-G1"));
        assert!(valid_name("abc"));
        assert!(!valid_name("ab"));
        assert!(!valid_name("a b c"));
        assert!(!valid_name("éclat"));
        assert!(!valid_name(&"a".repeat(17)));
    }
}
