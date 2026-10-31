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
    async fn hello(&mut self, name: &str, password: &str) -> ServerMsg {
        self.hello_invite(name, password, None).await
    }
    async fn hello_invite(
        &mut self,
        name: &str,
        password: &str,
        invite: Option<&str>,
    ) -> ServerMsg {
        self.send(&ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            name: name.into(),
            password: password.into(),
            invite: invite.map(Into::into),
        })
        .await;
        self.recv().await
    }
    /// Logs in a new player and creates the hero; returns the snapshot's hero position.
    async fn new_player(s: &Server, name: &str) -> (Client, TilePos) {
        let mut c = Client::connect(s).await;
        let w = c.hello(name, "secret").await;
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
        c.hello("gimli", "secret").await,
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
        new.hello("gimli", "secret").await,
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
            c.hello(&format!("nain{i}"), "pw").await,
            ServerMsg::Welcome { .. }
        ));
        clients.push(c);
    }
    let mut c = Client::connect(&s).await;
    assert_eq!(
        c.hello("nain10", "pw").await,
        ServerMsg::Reject(Reject::ServerFull)
    );
    // A re-login of a connected name replaces its session and is not counted twice.
    let mut again = Client::connect(&s).await;
    assert!(matches!(
        again.hello("nain0", "pw").await,
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

#[tokio::test]
async fn connections_beyond_cap_are_dropped() {
    use tokio::io::AsyncReadExt;
    let s = Server::start("conncap").await; // per-IP cap set high: proves the global cap
    let mut idle = vec![];
    for _ in 0..32 {
        idle.push(TcpStream::connect(s.addr).await.unwrap());
    }
    let mut extra = TcpStream::connect(s.addr).await.unwrap();
    let mut buf = [0u8; 1];
    let r = timeout(Duration::from_secs(5), extra.read(&mut buf))
        .await
        .expect("33rd connection must be closed, not hang");
    assert!(matches!(r, Ok(0) | Err(_)), "{r:?}");
    drop(idle);
    s.stop().await;
}

#[tokio::test]
async fn new_account_requires_invite_when_configured() {
    let s = Server::start_with("invite", Some("mellon")).await;
    for bad in [None, Some("wrong")] {
        let mut c = Client::connect(&s).await;
        assert_eq!(
            c.hello_invite("gimli", "pw", bad).await,
            ServerMsg::Reject(Reject::BadInvite)
        );
    }
    let mut c = Client::connect(&s).await;
    assert!(matches!(
        c.hello_invite("gimli", "pw", Some("mellon")).await,
        ServerMsg::Welcome { .. }
    ));
    s.stop().await;
}

#[tokio::test]
async fn existing_account_needs_no_invite() {
    let s = Server::start_with("invite-existing", Some("mellon")).await;
    let mut c = Client::connect(&s).await;
    assert!(matches!(
        c.hello_invite("gimli", "pw", Some("mellon")).await,
        ServerMsg::Welcome { .. }
    ));
    drop(c);
    let mut c = Client::connect(&s).await;
    assert!(matches!(
        c.hello("gimli", "pw").await,
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
    let mut buf = [0u8; 1];
    let r = timeout(Duration::from_secs(2), extra.read(&mut buf))
        .await
        .expect("5th connection must be closed promptly");
    assert!(matches!(r, Ok(0) | Err(_)), "{r:?}");
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
        again.hello("gimli", "pw").await,
        ServerMsg::Welcome { .. }
    ));
    s.stop().await;
}

#[tokio::test]
async fn empty_invite_means_open_registration() {
    let s = Server::start_with("invite-empty", Some("")).await;
    let mut c = Client::connect(&s).await;
    assert!(matches!(
        c.hello("gimli", "pw").await,
        ServerMsg::Welcome { .. }
    ));
    s.stop().await;
}
