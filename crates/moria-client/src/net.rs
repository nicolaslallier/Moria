//! Blocking TCP on two std threads; the Bevy side only touches the channels.

use moria_core::protocol::{ClientMsg, ServerMsg, encode, read_frame};
use std::io::{ErrorKind, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Duration;

pub enum NetEvent {
    Msg(ServerMsg),
    /// Connection failed or ended; the reason is in French.
    Disconnected(String),
}

/// Dropping it closes the connection (the writer thread shuts the socket down).
pub struct NetHandle {
    pub tx: Sender<ClientMsg>,
    pub rx: Receiver<NetEvent>,
}

/// Connects in the background; messages sent on `tx` before the connection is up are
/// queued. Never blocks the caller.
pub fn connect(addr: String) -> NetHandle {
    let (tx, out_rx) = channel::<ClientMsg>();
    let (ev_tx, rx) = channel();
    thread::spawn(move || {
        let stream = match open(&addr) {
            Ok(s) => s,
            Err(e) => {
                let _ = ev_tx.send(NetEvent::Disconnected(format!(
                    "Connexion impossible à {addr} : {e}"
                )));
                return;
            }
        };
        let Ok(mut wr) = stream.try_clone() else {
            let _ = ev_tx.send(NetEvent::Disconnected("Erreur réseau locale".into()));
            return;
        };
        thread::spawn(move || {
            // Ends when the handle is dropped or the socket fails.
            while let Ok(m) = out_rx.recv() {
                if wr.write_all(&encode(&m)).is_err() {
                    break;
                }
            }
            let _ = wr.shutdown(Shutdown::Both);
        });
        let mut rd = stream;
        loop {
            let ev = match read_frame::<ServerMsg>(&mut rd) {
                Ok(m) => NetEvent::Msg(m),
                Err(e) => NetEvent::Disconnected(match e.kind() {
                    ErrorKind::UnexpectedEof => "Le serveur a fermé la connexion".into(),
                    ErrorKind::InvalidData => "Données invalides reçues du serveur".into(),
                    _ => format!("Connexion perdue : {e}"),
                }),
            };
            let end = matches!(ev, NetEvent::Disconnected(_));
            if ev_tx.send(ev).is_err() || end {
                let _ = rd.shutdown(Shutdown::Both);
                return;
            }
        }
    });
    NetHandle { tx, rx }
}

fn open(addr: &str) -> std::io::Result<TcpStream> {
    let mut last = std::io::Error::new(ErrorKind::NotFound, "adresse introuvable");
    for a in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&a, Duration::from_secs(5)) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    #[test]
    fn bad_frame_becomes_disconnected() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let h = connect(addr);
        let (mut s, _) = l.accept().unwrap();
        s.write_all(&[0, 0, 0, 2, 0xff, 0xff]).unwrap();
        let ev = h.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            matches!(&ev, NetEvent::Disconnected(r) if r == "Données invalides reçues du serveur")
        );
        // Dropping the handle closes the socket.
        drop(h);
        let mut buf = [0u8; 1];
        assert_eq!(s.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn refused_connection_is_reported() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        drop(l);
        let h = connect(addr);
        let ev = h.rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(ev, NetEvent::Disconnected(r) if r.starts_with("Connexion impossible")));
    }
}

/// End-to-end smoke against an in-process server, through the same `Session`/`Game`
/// logic the Bevy systems use. Run with `cargo test -p moria-client -- --ignored`.
#[cfg(test)]
mod smoke {
    use super::*;
    use crate::game::{MOVE_COOLDOWN, Screen, Session, drag_msg};
    use moria_core::protocol::PROTOCOL_VERSION;
    use moria_core::srd::{Class, Race};
    use moria_core::{DesignationKind, Dir};
    use std::time::Instant;

    fn pump(s: &mut Session, h: &NetHandle, until: impl Fn(&Session) -> bool) {
        let t = Instant::now();
        while !until(s) {
            assert!(t.elapsed() < Duration::from_secs(15), "timeout");
            if let Ok(ev) = h.rx.recv_timeout(Duration::from_millis(50)) {
                assert!(!s.handle(ev), "connection ended: {:?}", s.error);
            }
        }
    }

    #[test]
    #[ignore]
    fn login_create_hero_play() {
        let dir = std::env::temp_dir().join(format!("moria-client-smoke-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let data = dir.join("world.bin");
        moria_core::world::save(&moria_core::world::generate(7), &data).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let listener = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let cfg = moria_server::Config {
            data,
            bind: addr,
            nats_url: None,
            invite: Some("secret".into()),
            max_per_ip: 4,
        };
        let _server = rt.spawn(moria_server::run_with_listener(
            cfg,
            listener,
            std::future::pending(),
        ));

        let hello = |invite: Option<&str>| ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            name: "Gimli".into(),
            password: "hache".into(),
            invite: invite.map(Into::into),
        };

        // Wrong invite: French reject on the login screen.
        let h = connect(addr.to_string());
        h.tx.send(hello(Some("faux"))).unwrap();
        let mut s = Session::default();
        let ev = h.rx.recv_timeout(Duration::from_secs(15)).unwrap();
        assert!(s.handle(ev));
        assert_eq!(s.screen, Screen::Login);
        println!("rejet : {}", s.error.as_deref().unwrap());

        let h = connect(addr.to_string());
        h.tx.send(hello(Some("secret"))).unwrap();
        let mut s = Session::default();
        pump(&mut s, &h, |s| s.screen == Screen::CreateHero);
        println!("Welcome needs_hero -> {:?}", s.screen);
        h.tx.send(ClientMsg::CreateHero {
            race: Race::Dwarf,
            class: Class::Fighter,
        })
        .unwrap();
        pump(&mut s, &h, |s| s.screen == Screen::Playing);
        let start = s.game.as_ref().unwrap().hero.pos;
        println!("Snapshot: héros en {start:?}");

        // Walk where possible, else dig, through the prediction rules.
        let t0 = Instant::now();
        let mut sent = vec![];
        for (i, d) in Dir::HORIZONTAL.into_iter().enumerate() {
            let now = i as f64 * (MOVE_COOLDOWN + 0.01);
            let g = s.game.as_mut().unwrap();
            let msg = g.try_move(d, now).or_else(|| g.dig());
            if let Some(m) = msg {
                sent.push(m.clone());
                h.tx.send(m).unwrap();
            }
            std::thread::sleep(Duration::from_millis(250));
            while let Ok(ev) = h.rx.try_recv() {
                assert!(!s.handle(ev));
            }
        }
        println!(
            "envoyés : {sent:?} ; héros (serveur) {:?}",
            s.game.as_ref().unwrap().hero.pos
        );
        let z = s.game.as_ref().unwrap().current_z();
        let p = s.game.as_ref().unwrap().hero.pos;
        let rect = drag_msg(
            (p.x, p.y - 3),
            (p.x + 4, p.y + 3),
            z,
            Some(DesignationKind::Dig),
        );
        h.tx.send(rect).unwrap();
        pump(&mut s, &h, |s| {
            !s.game.as_ref().unwrap().mountain.designations.is_empty()
        });
        let g = s.game.as_ref().unwrap();
        println!(
            "après {:?} : héros {:?}, {} désignations, {} nains, ressources {:?}",
            t0.elapsed(),
            g.hero.pos,
            g.mountain.designations.len(),
            g.dwarves.len(),
            g.resources
        );
        drop(h);
    }
}
