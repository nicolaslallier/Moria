//! One TCP session: handshake, auth (argon2 runs here, never on the tick task), then
//! frames in both directions until either side ends.

use crate::sim::Command;
use crate::{Lookup, Request};
use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use moria_core::PlayerId;
use moria_core::protocol::{
    ClientMsg, PROTOCOL_VERSION, Reject, ServerMsg, decode, encode, read_frame_async,
    read_frame_bytes_async,
};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::time::timeout;

pub const SESSION_QUEUE: usize = 64;
/// Shortest password accepted at account creation, in characters.
const MIN_PASSWORD: usize = 8;
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
static ARGON: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(2)));

/// Verified against for unknown names, so they cost the same argon2 work as a wrong
/// password. Hashed lazily, inside the first `blocking` call that needs it.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    Argon2::default()
        .hash_password(b"moria-dummy-password")
        .expect("argon2 dummy hash")
        .to_string()
});

fn verify(stored: &str, password: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|p| {
        Argon2::default()
            .verify_password(password.as_bytes(), &p)
            .is_ok()
    })
}

/// Runs argon2 off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    blocking_on(ARGON.clone(), f).await
}

/// The permit moves into the blocking closure: a cancelled caller (handshake timeout)
/// cannot free it while the computation still runs.
async fn blocking_on<T: Send + 'static>(
    sem: Arc<Semaphore>,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let permit = sem.acquire_owned().await.ok()?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
    .ok()
}

pub async fn session(stream: TcpStream, h: Handles) -> anyhow::Result<()> {
    let (mut rd, mut wr) = stream.into_split();
    let hs = timeout(HANDSHAKE_TIMEOUT, handshake(&mut rd, &mut wr, &h)).await??;
    let Some((player, display, mut rx)) = hs else {
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
        // One CreateHero per session reaches the shared request queue; repeats are dropped
        // so they cannot crowd out everyone's Lookup/Login.
        let mut hero_requested = false;
        loop {
            match read_frame_async::<ClientMsg>(&mut rd).await? {
                ClientMsg::CreateHero { .. } if hero_requested => {}
                ClientMsg::CreateHero { race, class } => {
                    hero_requested = true;
                    let r = Request::CreateHero {
                        player,
                        name: display.clone(),
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

/// Player, display name as typed, and the queue the world task writes to.
type Handshake = Option<(PlayerId, String, mpsc::Receiver<ServerMsg>)>;

/// Hello to login. `None` means the session ends quietly (a Reject was sent, or shutdown).
async fn handshake(
    rd: &mut OwnedReadHalf,
    wr: &mut OwnedWriteHalf,
    h: &Handles,
) -> anyhow::Result<Handshake> {
    let buf = read_frame_bytes_async(rd).await?;
    // Version first, from the frozen prefix: a future Hello may not decode as today's.
    if let Ok(((0, version), _)) = postcard::take_from_bytes::<(u32, u32)>(&buf)
        && version != PROTOCOL_VERSION
    {
        return reject(
            wr,
            Reject::VersionMismatch {
                server: PROTOCOL_VERSION,
            },
        )
        .await
        .map(|()| None);
    }
    let ClientMsg::Hello {
        name,
        password,
        invite,
        create,
        ..
    } = decode(&buf)?
    else {
        return Ok(None);
    };
    if !valid_name(&name) {
        return reject(wr, Reject::InvalidName).await.map(|()| None);
    }
    // Accounts are keyed by the lowercased name; the hero keeps the typed spelling.
    let key = name.to_lowercase();
    let lookup = ask(h, |reply| Request::Lookup {
        name: key.clone(),
        reply,
    })
    .await;
    let new_hash = match lookup {
        None => return Ok(None), // server shutting down
        Some(Lookup::Full) => return reject(wr, Reject::ServerFull).await.map(|()| None),
        Some(Lookup::New) if !create => {
            // Same work and same reply as a wrong password: no account enumeration.
            blocking(move || verify(&DUMMY_HASH, &password)).await;
            return reject(wr, Reject::BadPassword).await.map(|()| None);
        }
        Some(Lookup::New) => {
            if let Some(want) = &h.invite
                && !invite.is_some_and(|got| ct_eq(got.as_bytes(), want.as_bytes()))
            {
                return reject(wr, Reject::BadInvite).await.map(|()| None);
            }
            if password.chars().count() < MIN_PASSWORD {
                return reject(wr, Reject::WeakPassword).await.map(|()| None);
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
            let ok = blocking(move || verify(&stored, &password)).await;
            if ok != Some(true) {
                return reject(wr, Reject::BadPassword).await.map(|()| None);
            }
            None
        }
    };
    let (tx, rx) = mpsc::channel(SESSION_QUEUE);
    let login = ask(h, |reply| Request::Login {
        name: key,
        new_hash,
        tx,
        reply,
    })
    .await;
    match login {
        None => Ok(None),
        Some(Err(r)) => reject(wr, r).await.map(|()| None),
        Some(Ok(p)) => Ok(Some((p, name, rx))),
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn argon_permit_held_until_blocking_work_finishes() {
        use std::sync::{Arc, mpsc};
        use std::time::Duration;
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let (release, wait) = mpsc::channel::<()>();
        let job = super::blocking_on(sem.clone(), move || {
            let _ = wait.recv();
        });
        // Cancelled like a handshake timeout, while the blocking job runs.
        let r = tokio::time::timeout(Duration::from_millis(200), job).await;
        assert!(r.is_err());
        assert_eq!(sem.available_permits(), 0, "permit freed on cancel");
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while sem.available_permits() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("permit released once the job ends");
    }

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
