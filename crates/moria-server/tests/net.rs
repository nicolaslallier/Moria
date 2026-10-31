use moria_core::protocol::{
    ClientMsg, MAX_FRAME, PROTOCOL_VERSION, Reject, ServerMsg, encode, read_frame_async,
};
use moria_core::srd::{Class, Race};
use moria_core::world::{generate, save};
use moria_core::{Dir, TilePos};
use moria_server::{Config, run_with_listener};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::timeout;

struct Server {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    done: tokio::task::JoinHandle<anyhow::Result<()>>,
    data: PathBuf,
}

impl Server {
    async fn start(name: &str) -> Server {
        Server::start_with(name, None).await
    }

    async fn start_with(name: &str, invite: Option<&str>) -> Server {
        Server::start_cap(name, invite, 1000).await
    }

    async fn start_cap(name: &str, invite: Option<&str>, max_per_ip: usize) -> Server {
        let dir = std::env::temp_dir().join(format!("moria-net-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let data = dir.join("world.bin");
        save(&generate(7), &data).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, rx) = oneshot::channel();
        let cfg = Config {
            data: data.clone(),
            bind: addr,
            nats_url: None,
            invite: invite.map(Into::into),
            max_per_ip,
        };
        let done = tokio::spawn(run_with_listener(cfg, listener, async {
            let _ = rx.await;
        }));
        Server {
            addr,
            stop: Some(stop),
            done,
            data,
        }
    }

    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        timeout(Duration::from_secs(10), self.done)
            .await
            .expect("server stops")
            .unwrap()
            .unwrap();
    }
}

struct Client(TcpStream);

impl Client {
    async fn connect(s: &Server) -> Client {
        Client(TcpStream::connect(s.addr).await.unwrap())
    }
    async fn send(&mut self, m: &ClientMsg) {
        self.0.write_all(&encode(m)).await.unwrap();
    }
    async fn recv(&mut self) -> ServerMsg {
        timeout(Duration::from_secs(10), read_frame_async(&mut self.0))
            .await
            .expect("reply in time")
            .expect("frame")
    }
    /// "Créer un compte": creates the account, or logs in if the name exists.
    async fn hello(&mut self, name: &str, password: &str) -> ServerMsg {
        self.hello_full(name, password, None, true).await
    }
    /// "Se connecter": never creates an account.
    async fn login(&mut self, name: &str, password: &str) -> ServerMsg {
        self.hello_full(name, password, None, false).await
    }
    async fn hello_invite(
        &mut self,
        name: &str,
        password: &str,
        invite: Option<&str>,
    ) -> ServerMsg {
        self.hello_full(name, password, invite, true).await
    }
    async fn hello_full(
        &mut self,
        name: &str,
        password: &str,
        invite: Option<&str>,
        create: bool,
    ) -> ServerMsg {
        self.send(&ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            name: name.into(),
            password: password.into(),
            invite: invite.map(Into::into),
            create,
        })
        .await;
        self.recv().await
    }
    /// Logs in a new player and creates the hero; returns the snapshot's hero position.
    async fn new_player(s: &Server, name: &str) -> (Client, TilePos) {
        let mut c = Client::connect(s).await;
        let w = c.hello(name, "secret12").await;
        assert!(
            matches!(
                w,
                ServerMsg::Welcome {
                    needs_hero: true,
                    ..
                }
            ),
            "{w:?}"
        );
        c.send(&ClientMsg::CreateHero {
            race: Race::Dwarf,
            class: Class::Fighter,
        })
        .await;
        match c.recv().await {
            ServerMsg::Snapshot(v) => {
                assert_eq!(v.dwarves.len(), 3);
                assert_eq!(v.hero.name, name);
                let pos = v.hero.pos;
                (c, pos)
            }
            m => panic!("expected snapshot, got {m:?}"),
        }
    }
    /// Reads until a Tick arrives.
    async fn next_tick(&mut self) -> moria_core::protocol::TickDelta {
        loop {
            if let ServerMsg::Tick(d) = self.recv().await {
                return d;
            }
        }
    }
}

#[tokio::test]
async fn version_mismatch_rejected() {
    let s = Server::start("version").await;
    let mut c = Client::connect(&s).await;
    c.send(&ClientMsg::Hello {
        version: PROTOCOL_VERSION + 1,
        name: "gimli".into(),
        password: "x".into(),
        invite: None,
        create: false,
    })
    .await;
    assert_eq!(
        c.recv().await,
        ServerMsg::Reject(Reject::VersionMismatch {
            server: PROTOCOL_VERSION
        })
    );
    s.stop().await;
}

#[tokio::test]
async fn invalid_name_rejected() {
    let s = Server::start("badname").await;
    let mut c = Client::connect(&s).await;
    assert_eq!(
        c.hello("a b", "x").await,
        ServerMsg::Reject(Reject::InvalidName)
    );
    s.stop().await;
}

#[tokio::test]
async fn new_player_creates_hero_and_gets_snapshot() {
    let s = Server::start("newplayer").await;
    let (_c, _) = Client::new_player(&s, "gimli").await;
    s.stop().await;
}

#[tokio::test]
async fn returning_player_gets_snapshot_and_save_persists() {
    let s = Server::start("returning").await;
    let (c, pos) = Client::new_player(&s, "gimli").await;
    drop(c);
    let mut c = Client::connect(&s).await;
    assert!(matches!(
        c.login("gimli", "secret12").await,
        ServerMsg::Welcome {
            needs_hero: false,
            ..
        }
    ));
    match c.recv().await {
        ServerMsg::Snapshot(v) => assert_eq!(v.hero.pos, pos),
        m => panic!("expected snapshot, got {m:?}"),
    }
    let data = s.data.clone();
    s.stop().await;
    let w = moria_core::world::load(&data).unwrap();
    let acc = &w.accounts["gimli"];
    assert!(acc.mountain.is_some());
    assert!(w.fortresses[&acc.mountain.unwrap()].hero.is_some());
}

#[tokio::test]
async fn wrong_password_rejected() {
    let s = Server::start("badpw").await;
    let (c, _) = Client::new_player(&s, "gimli").await;
    drop(c);
    let mut c = Client::connect(&s).await;
    assert_eq!(
        c.hello("gimli", "wrong").await,
        ServerMsg::Reject(Reject::BadPassword)
    );
    s.stop().await;
}

#[tokio::test]
async fn second_login_kicks_older_session() {
    let s = Server::start("kick").await;
    let (mut old, _) = Client::new_player(&s, "gimli").await;
    let mut new = Client::connect(&s).await;
    assert!(matches!(
        new.hello("gimli", "secret12").await,
        ServerMsg::Welcome { .. }
    ));
    // The old session is closed: reads end with an error (EOF) after any queued frames.
    let closed = timeout(Duration::from_secs(10), async {
        while read_frame_async::<ServerMsg>(&mut old.0).await.is_ok() {}
    })
    .await;
    assert!(closed.is_ok(), "old session still open");
    new.next_tick().await;
    s.stop().await;
}

#[tokio::test]
async fn eleventh_connection_server_full() {
    let s = Server::start("full").await;
    let mut clients = vec![];
    for i in 0..10 {
        let mut c = Client::connect(&s).await;
        assert!(matches!(
            c.hello(&format!("nain{i}"), "password").await,
            ServerMsg::Welcome { .. }
        ));
        clients.push(c);
    }
    let mut c = Client::connect(&s).await;
    assert_eq!(
        c.hello("nain10", "password").await,
        ServerMsg::Reject(Reject::ServerFull)
    );
    // A re-login of a connected name replaces its session and is not counted twice.
    let mut again = Client::connect(&s).await;
    assert!(matches!(
        again.hello("nain0", "password").await,
        ServerMsg::Welcome { .. }
    ));
    s.stop().await;
}

#[tokio::test]
async fn oversized_frame_disconnects_only_that_client() {
    let s = Server::start("oversized").await;
    let (mut a, _) = Client::new_player(&s, "alice").await;
    let (mut b, _) = Client::new_player(&s, "bob").await;
    a.0.write_all(&((MAX_FRAME as u32 + 1).to_be_bytes()))
        .await
        .unwrap();
    let closed = timeout(Duration::from_secs(10), async {
        while read_frame_async::<ServerMsg>(&mut a.0).await.is_ok() {}
    })
    .await;
    assert!(closed.is_ok(), "bad client not disconnected");
    let t1 = b.next_tick().await.tick;
    let t2 = b.next_tick().await.tick;
    assert!(t2 > t1);
    s.stop().await;
}

#[tokio::test]
async fn move_reflected_in_tick() {
    let s = Server::start("move").await;
    let (mut c, pos) = Client::new_player(&s, "gimli").await;
    c.send(&ClientMsg::Move(Dir::S)).await;
    let want = pos.step(Dir::S);
    let got = timeout(Duration::from_secs(10), async {
        loop {
            if c.next_tick().await.hero_pos == want {
                break;
            }
        }
    })
    .await;
    assert!(got.is_ok(), "hero never reached {want:?}");
    s.stop().await;
}

#[tokio::test]
async fn missing_world_file_is_an_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cfg = Config {
        data: std::env::temp_dir().join("moria-net-does-not-exist/world.bin"),
        bind: listener.local_addr().unwrap(),
        nats_url: None,
        invite: None,
        max_per_ip: 4,
    };
    let e = run_with_listener(cfg, listener, async {})
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("lancez d'abord moria-worldgen"),
        "{e}"
    );
}

/// An over-cap connection gets a best-effort ServerFull, then is closed.
async fn assert_full_then_closed(c: &mut TcpStream) {
    use tokio::io::AsyncReadExt;
    let m = timeout(Duration::from_secs(5), read_frame_async::<ServerMsg>(c))
        .await
        .expect("over-cap connection must not hang");
    assert_eq!(m.unwrap(), ServerMsg::Reject(Reject::ServerFull));
    let mut buf = [0u8; 1];
    let r = timeout(Duration::from_secs(5), c.read(&mut buf))
        .await
        .expect("over-cap connection must be closed");
    assert!(matches!(r, Ok(0) | Err(_)), "{r:?}");
}

#[tokio::test]
async fn connections_beyond_cap_are_dropped() {
    let s = Server::start("conncap").await; // per-IP cap set high: proves the global cap
    let mut idle = vec![];
    for _ in 0..32 {
        idle.push(TcpStream::connect(s.addr).await.unwrap());
    }
    let mut extra = TcpStream::connect(s.addr).await.unwrap();
    assert_full_then_closed(&mut extra).await;
    drop(idle);
    s.stop().await;
}

#[tokio::test]
async fn new_account_requires_invite_when_configured() {
    let s = Server::start_with("invite", Some("mellon")).await;
    for bad in [None, Some("wrong")] {
        let mut c = Client::connect(&s).await;
        assert_eq!(
            c.hello_invite("gimli", "password", bad).await,
            ServerMsg::Reject(Reject::BadInvite)
        );
    }
    let mut c = Client::connect(&s).await;
    assert!(matches!(
        c.hello_invite("gimli", "password", Some("mellon")).await,
        ServerMsg::Welcome { .. }
    ));
    s.stop().await;
}

#[tokio::test]
async fn existing_account_needs_no_invite() {
    let s = Server::start_with("invite-existing", Some("mellon")).await;
    let mut c = Client::connect(&s).await;
    assert!(matches!(
        c.hello_invite("gimli", "password", Some("mellon")).await,
        ServerMsg::Welcome { .. }
    ));
    drop(c);
    let mut c = Client::connect(&s).await;
    assert!(matches!(
        c.login("gimli", "password").await,
        ServerMsg::Welcome { .. }
    ));
    s.stop().await;
}

#[tokio::test]
async fn per_ip_cap_drops_fifth_connection() {
    use tokio::io::AsyncReadExt;
    let s = Server::start_cap("ipcap", None, 4).await;
    let mut idle = vec![];
    for _ in 0..4 {
        idle.push(TcpStream::connect(s.addr).await.unwrap());
    }
    let mut extra = TcpStream::connect(s.addr).await.unwrap();
    assert_full_then_closed(&mut extra).await;
    let mut buf = [0u8; 1];
    // The first four are still open (no data, no EOF).
    for c in &mut idle {
        assert!(
            timeout(Duration::from_millis(200), c.read(&mut buf))
                .await
                .is_err()
        );
    }
    // A slot frees up once one of them closes.
    idle.pop();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut again = Client::connect(&s).await;
    assert!(matches!(
        again.hello("gimli", "password").await,
        ServerMsg::Welcome { .. }
    ));
    s.stop().await;
}

#[tokio::test]
async fn empty_invite_means_open_registration() {
    let s = Server::start_with("invite-empty", Some("")).await;
    let mut c = Client::connect(&s).await;
    assert!(matches!(
        c.hello("gimli", "password").await,
        ServerMsg::Welcome { .. }
    ));
    s.stop().await;
}

#[tokio::test]
async fn unknown_account_without_create_rejected() {
    let s = Server::start("unknown").await;
    for _ in 0..2 {
        let mut c = Client::connect(&s).await;
        assert_eq!(
            c.login("gimli", "secret12").await,
            ServerMsg::Reject(Reject::BadPassword)
        );
    }
    let data = s.data.clone();
    s.stop().await;
    let w = moria_core::world::load(&data).unwrap();
    assert!(w.accounts.is_empty() && w.fortresses.is_empty());
}

/// No account enumeration: an unknown name and a wrong password get the same reply.
#[tokio::test]
async fn unknown_and_wrong_password_look_the_same() {
    let s = Server::start("enum").await;
    let (c, _) = Client::new_player(&s, "gimli").await;
    drop(c);
    let mut c = Client::connect(&s).await;
    let wrong = c.login("gimli", "mauvais-mdp").await;
    let mut c = Client::connect(&s).await;
    let unknown = c.login("legolas", "mauvais-mdp").await;
    assert_eq!(wrong, ServerMsg::Reject(Reject::BadPassword));
    assert_eq!(unknown, wrong);
    s.stop().await;
}

#[tokio::test]
async fn names_are_case_insensitive() {
    let s = Server::start("case").await;
    let (c, _) = Client::new_player(&s, "Gimli").await;
    drop(c);
    let mut c = Client::connect(&s).await;
    assert!(matches!(
        c.login("gIMLI", "secret12").await,
        ServerMsg::Welcome {
            needs_hero: false,
            ..
        }
    ));
    match c.recv().await {
        ServerMsg::Snapshot(v) => assert_eq!(v.hero.name, "Gimli"),
        m => panic!("expected snapshot, got {m:?}"),
    }
    drop(c);
    // "Créer un compte" on a taken name (any case) is a login: wrong password, no account.
    let mut c = Client::connect(&s).await;
    assert_eq!(
        c.hello("GIMLI", "autre-mdp").await,
        ServerMsg::Reject(Reject::BadPassword)
    );
    let data = s.data.clone();
    s.stop().await;
    let w = moria_core::world::load(&data).unwrap();
    assert_eq!(w.accounts.keys().collect::<Vec<_>>(), ["gimli"]);
    assert_eq!(w.fortresses.len(), 1);
}

#[tokio::test]
async fn future_hello_gets_version_mismatch() {
    let s = Server::start("future").await;
    let mut c = Client::connect(&s).await;
    // Hello (tag 0), version 2, then bytes today's Hello cannot decode.
    let body = [0u8, 2, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend(body);
    c.0.write_all(&frame).await.unwrap();
    assert_eq!(
        c.recv().await,
        ServerMsg::Reject(Reject::VersionMismatch { server: 1 })
    );
    s.stop().await;
}

#[tokio::test]
async fn weak_password_rejected_at_creation() {
    let s = Server::start("weakpw").await;
    let mut c = Client::connect(&s).await;
    assert_eq!(
        c.hello("gimli", "court").await,
        ServerMsg::Reject(Reject::WeakPassword)
    );
    let mut c = Client::connect(&s).await;
    assert!(matches!(
        c.hello("gimli", "12345678").await,
        ServerMsg::Welcome { .. }
    ));
    s.stop().await;
}

#[tokio::test]
async fn create_hero_flood_keeps_server_usable() {
    let s = Server::start("hero-flood").await;
    let (mut a, pos) = Client::new_player(&s, "alice").await;
    let flood: Vec<u8> = (0..500)
        .flat_map(|_| {
            encode(&ClientMsg::CreateHero {
                race: Race::Dwarf,
                class: Class::Fighter,
            })
        })
        .collect();
    a.0.write_all(&flood).await.unwrap();
    let mut b = Client::connect(&s).await;
    assert!(matches!(
        timeout(Duration::from_secs(5), b.hello("bob", "secret12"))
            .await
            .expect("login well within the handshake timeout"),
        ServerMsg::Welcome { .. }
    ));
    // The flooding session still works.
    a.send(&ClientMsg::Move(Dir::S)).await;
    let want = pos.step(Dir::S);
    let moved = timeout(Duration::from_secs(10), async {
        while a.next_tick().await.hero_pos != want {}
    })
    .await;
    assert!(moved.is_ok(), "flooding session stopped working");
    s.stop().await;
}
