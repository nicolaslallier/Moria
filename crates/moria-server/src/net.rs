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
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::time::timeout;

pub const SESSION_QUEUE: usize = 64;
/// One deadline over the whole pre-login handshake (Hello, argon2 wait + compute, invite
/// check, up to the Welcome/Reject). CreateHero comes later and is not covered.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// A client that does not drain its socket for this long is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Handles {
    pub req: mpsc::Sender<Request>,
    pub cmd: mpsc::Sender<(PlayerId, Command)>,
    pub invite: Option<String>,
}

/// Constant-time equality on the compared prefix; the length difference is folded in.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut d = a.len() ^ b.len();
    for (x, y) in a.iter().zip(b) {
        d |= usize::from(x ^ y);
    }
    d == 0
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

/// At most this many argon2 runs at once, whatever the number of handshakes.
static ARGON: Semaphore = Semaphore::const_new(2);

/// Runs argon2 off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let _permit = ARGON.acquire().await.ok()?;
    tokio::task::spawn_blocking(f).await.ok()
}

pub async fn session(stream: TcpStream, h: Handles) -> anyhow::Result<()> {
    let (mut rd, mut wr) = stream.into_split();
    let hs = timeout(HANDSHAKE_TIMEOUT, handshake(&mut rd, &mut wr, &h)).await??;
    let Some((player, mut rx)) = hs else {
        return Ok(());
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

type Handshake = Option<(PlayerId, mpsc::Receiver<ServerMsg>)>;

/// Hello to login. `None` means the session ends quietly (a Reject was sent, or shutdown).
async fn handshake(
    rd: &mut OwnedReadHalf,
    wr: &mut OwnedWriteHalf,
    h: &Handles,
) -> anyhow::Result<Handshake> {
    let ClientMsg::Hello {
        version,
        name,
        password,
        invite,
    } = read_frame_async(rd).await?
    else {
        return Ok(None);
    };
    if version != PROTOCOL_VERSION {
        return reject(
            wr,
            Reject::VersionMismatch {
                server: PROTOCOL_VERSION,
            },
        )
        .await
        .map(|()| None);
    }
    if !valid_name(&name) {
        return reject(wr, Reject::InvalidName).await.map(|()| None);
    }
    let lookup = ask(h, |reply| Request::Lookup {
        name: name.clone(),
        reply,
    })
    .await;
    let new_hash = match lookup {
        None => return Ok(None), // server shutting down
        Some(Lookup::Full) => return reject(wr, Reject::ServerFull).await.map(|()| None),
        Some(Lookup::New) => {
            if let Some(want) = &h.invite
                && !invite.is_some_and(|got| ct_eq(got.as_bytes(), want.as_bytes()))
            {
                return reject(wr, Reject::BadInvite).await.map(|()| None);
            }
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
                return reject(wr, Reject::BadPassword).await.map(|()| None);
            }
            None
        }
    };
    let (tx, rx) = mpsc::channel(SESSION_QUEUE);
    let login = ask(h, |reply| Request::Login {
        name,
        new_hash,
        tx,
        reply,
    })
    .await;
    match login {
        None => Ok(None),
        Some(Err(r)) => reject(wr, r).await.map(|()| None),
        Some(Ok(p)) => Ok(Some((p, rx))),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ct_eq_cases() {
        use super::ct_eq;
        assert!(ct_eq(b"mellon", b"mellon"));
        assert!(!ct_eq(b"mellon", b"mellor"));
        assert!(!ct_eq(b"mellon", b"mello"));
        assert!(!ct_eq(b"mello", b"mellon"));
        assert!(ct_eq(b"", b""));
        assert!(!ct_eq(b"", b"x"));
    }

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
