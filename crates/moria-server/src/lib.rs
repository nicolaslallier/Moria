//! Central server. One task owns the `World` and ticks at 10 Hz; sessions and the NATS
//! bridge talk to it only through bounded channels. The world task never awaits a session
//! or the network: it drains its inputs with `try_recv` and answers with `try_send`.

pub mod bridge;
pub mod net;
pub mod sim;

use moria_core::ai::{AiView, Assignments, DwarfView};
use moria_core::protocol::{FortressView, HeroView, Reject, ServerMsg};
use moria_core::srd::{Class, Race, roll_abilities};
use moria_core::world::{self, Account, Hero, MAX_OWNED, World};
use moria_core::{MountainId, PlayerId, TilePos};
use sim::{Changes, Command};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::net::SocketAddr;
use std::ops::Bound;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

const TICK: Duration = Duration::from_millis(100);
const SAVE_EVERY: u64 = 300;
/// Most commands applied per tick; the rest wait in the channel (sessions get backpressure).
const MAX_CMDS_PER_TICK: usize = 1024;
const MAX_REQS_PER_TICK: usize = 64;
const MAX_ASSIGN_PER_TICK: usize = 16;

pub struct Config {
    pub data: PathBuf,
    pub bind: SocketAddr,
    /// `None` disables the NATS bridge.
    pub nats_url: Option<String>,
}

pub enum Lookup {
    Full,
    New,
    Existing(String),
}

/// Requests from sessions to the world task.
pub enum Request {
    Lookup {
        name: String,
        reply: oneshot::Sender<Lookup>,
    },
    /// `new_hash` is `Some` for a new account (already hashed by the session).
    Login {
        name: String,
        new_hash: Option<String>,
        tx: mpsc::Sender<ServerMsg>,
        reply: oneshot::Sender<Result<PlayerId, Reject>>,
    },
    CreateHero {
        player: PlayerId,
        race: Race,
        class: Class,
    },
}

/// Binds `cfg.bind` and serves until `shutdown` resolves.
pub async fn run(
    cfg: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(cfg.bind).await?;
    run_with_listener(cfg, listener, shutdown).await
}

/// Like `run` on an already-bound listener (tests bind `127.0.0.1:0`); `cfg.bind` is unused.
/// Returns after the final save.
pub async fn run_with_listener(
    cfg: Config,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let w = match world::load(&cfg.data) {
        Ok(w) => w,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => anyhow::bail!(
            "fichier monde introuvable ({}) : lancez d'abord moria-worldgen",
            cfg.data.display()
        ),
        Err(e) => anyhow::bail!("lecture du monde {} impossible : {e}", cfg.data.display()),
    };
    let (req_tx, req_rx) = mpsc::channel(256);
    let (cmd_tx, cmd_rx) = mpsc::channel(4096);
    let (assign_tx, assign_rx) = mpsc::channel(64);
    let mut view_tx = None;
    let bridge = cfg.nats_url.clone().map(|url| {
        let (tx, rx) = mpsc::channel(16);
        view_tx = Some(tx);
        let known = w.mountains.keys().copied().collect();
        tokio::spawn(bridge::run(url, known, rx, assign_tx.clone()))
    });
    drop(assign_tx);

    let handles = net::Handles {
        req: req_tx,
        cmd: cmd_tx,
    };
    tracing::info!("serveur prêt sur {}", listener.local_addr()?);
    let accept = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let h = handles.clone();
                    tokio::spawn(async move {
                        if let Err(e) = net::session(stream, h).await {
                            tracing::debug!("session {peer} terminee : {e}");
                        }
                    });
                }
                Err(e) => tracing::warn!("accept : {e}"),
            }
        }
    });

    let state = State {
        w,
        data: cfg.data,
        sessions: HashMap::new(),
        req_rx,
        cmd_rx,
        assign_rx,
        view_tx,
        last_view: None,
        saving: None,
    };
    let r = state.run(shutdown).await;
    accept.abort();
    if let Some(b) = bridge {
        b.abort();
    }
    r
}

struct State {
    w: World,
    data: PathBuf,
    sessions: HashMap<PlayerId, mpsc::Sender<ServerMsg>>,
    req_rx: mpsc::Receiver<Request>,
    cmd_rx: mpsc::Receiver<(PlayerId, Command)>,
    assign_rx: mpsc::Receiver<Assignments>,
    view_tx: Option<mpsc::Sender<AiView>>,
    last_view: Option<MountainId>,
    saving: Option<JoinHandle<()>>,
}

impl State {
    async fn run(mut self, shutdown: impl Future<Output = ()>) -> anyhow::Result<()> {
        tokio::pin!(shutdown);
        let mut iv = tokio::time::interval(TICK);
        iv.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = iv.tick() => self.tick(),
            }
        }
        tracing::info!("arret : sauvegarde finale");
        self.sessions.clear();
        if let Some(h) = self.saving.take() {
            let _ = h.await;
        }
        let (w, path) = (self.w.clone(), self.data.clone());
        tokio::task::spawn_blocking(move || world::save(&w, &path)).await??;
        Ok(())
    }

    fn tick(&mut self) {
        self.sessions.retain(|_, tx| !tx.is_closed());
        // Bounded drains: senders on other threads cannot keep the tick looping.
        for _ in 0..MAX_REQS_PER_TICK {
            let Ok(r) = self.req_rx.try_recv() else { break };
            self.request(r);
        }
        for _ in 0..MAX_ASSIGN_PER_TICK {
            let Ok(a) = self.assign_rx.try_recv() else {
                break;
            };
            sim::apply_assignments(&mut self.w, &a);
        }
        let mut changes: BTreeMap<MountainId, Changes> = BTreeMap::new();
        for _ in 0..MAX_CMDS_PER_TICK {
            let Ok((player, c)) = self.cmd_rx.try_recv() else {
                break;
            };
            if let Some(m) = self.mountain_of(player) {
                let ch = sim::apply_command(&mut self.w, m, c);
                changes.entry(m).or_default().then(ch);
            }
        }
        let deltas = sim::step(&mut self.w);
        for (m, mut d) in deltas {
            if let Some(ch) = changes.remove(&m) {
                ch.merge_into(&mut d);
            }
            if let Some(owner) = self.w.fortresses.get(&m).map(|f| f.owner) {
                self.send(owner, ServerMsg::Tick(d));
            }
        }
        self.send_view();
        if self.w.tick.is_multiple_of(SAVE_EVERY)
            && self.saving.as_ref().is_none_or(|h| h.is_finished())
        {
            let (w, path) = (self.w.clone(), self.data.clone());
            self.saving = Some(tokio::task::spawn_blocking(move || {
                if let Err(e) = world::save(&w, &path) {
                    tracing::error!("sauvegarde echouee : {e}");
                }
            }));
        }
    }

    fn mountain_of(&self, player: PlayerId) -> Option<MountainId> {
        self.w
            .fortresses
            .iter()
            .find(|(_, f)| f.owner == player)
            .map(|(&m, _)| m)
    }

    /// `try_send`; a full or closed queue drops that session (the client is disconnected).
    fn send(&mut self, player: PlayerId, m: ServerMsg) {
        if let Some(tx) = self.sessions.get(&player)
            && tx.try_send(m).is_err()
        {
            tracing::info!("client {player:?} trop lent : deconnecte");
            self.sessions.remove(&player);
        }
    }

    fn request(&mut self, r: Request) {
        match r {
            Request::Lookup { name, reply } => {
                let acc = self.w.accounts.get(&name);
                let connected = acc.is_some_and(|a| self.sessions.contains_key(&a.player));
                let l = if self.sessions.len() >= MAX_OWNED && !connected {
                    Lookup::Full
                } else if let Some(a) = acc {
                    Lookup::Existing(a.password_hash.clone())
                } else {
                    Lookup::New
                };
                let _ = reply.send(l);
            }
            Request::Login {
                name,
                new_hash,
                tx,
                reply,
            } => {
                let r = self.login(name, new_hash);
                let _ = reply.send(r.clone().map(|(p, _)| p));
                if let Ok((player, m)) = r {
                    // Replacing the old sender drops it: the older session closes.
                    self.sessions.insert(player, tx);
                    let needs_hero = self.w.fortresses[&m].hero.is_none();
                    self.send(player, ServerMsg::Welcome { player, needs_hero });
                    self.send_snapshot(player, m);
                }
            }
            Request::CreateHero {
                player,
                race,
                class,
            } => {
                let Some(m) = self.mountain_of(player) else {
                    return;
                };
                let name = self.w.accounts.iter().find(|(_, a)| a.player == player);
                let Some(name) = name.map(|(n, _)| n.clone()) else {
                    return;
                };
                let Some(pos) = entrance(&self.w, m) else {
                    return;
                };
                let f = self.w.fortresses.get_mut(&m).expect("owned fortress");
                if f.hero.is_some() {
                    return;
                }
                f.hero = Some(Hero {
                    name,
                    race,
                    class,
                    abilities: roll_abilities(race, class),
                    pos,
                    dig: None,
                    move_cooldown: 0,
                });
                self.send_snapshot(player, m);
            }
        }
    }

    fn login(
        &mut self,
        name: String,
        new_hash: Option<String>,
    ) -> Result<(PlayerId, MountainId), Reject> {
        let existing = self.w.accounts.get(&name).map(|a| a.player);
        let replacing = existing.is_some_and(|p| self.sessions.contains_key(&p));
        if self.sessions.len() >= MAX_OWNED && !replacing {
            return Err(Reject::ServerFull);
        }
        let player = match (existing, new_hash) {
            (Some(p), None) => p,
            // Created concurrently by another session since the lookup.
            (Some(_), Some(_)) | (None, None) => return Err(Reject::BadPassword),
            (None, Some(hash)) => {
                let next = self.w.accounts.values().map(|a| a.player.0 + 1).max();
                let player = PlayerId(next.unwrap_or(0));
                let m = world::claim_mountain(&mut self.w, player).ok_or(Reject::NoFreeMountain)?;
                self.w.accounts.insert(
                    name.clone(),
                    Account {
                        player,
                        password_hash: hash,
                        mountain: Some(m),
                    },
                );
                player
            }
        };
        let m = self.w.accounts[&name].mountain;
        m.filter(|m| self.w.fortresses.contains_key(m))
            .map(|m| (player, m))
            .ok_or(Reject::NoFreeMountain)
    }

    fn send_snapshot(&mut self, player: PlayerId, m: MountainId) {
        let (Some(mt), Some(f)) = (self.w.mountains.get(&m), self.w.fortresses.get(&m)) else {
            return;
        };
        let Some(h) = &f.hero else { return };
        let v = FortressView {
            mountain: mt.clone(),
            hero: HeroView {
                name: h.name.clone(),
                race: h.race,
                class: h.class,
                abilities: h.abilities,
                pos: h.pos,
            },
            dwarves: f.dwarves.iter().map(|d| (d.id, d.pos)).collect(),
            resources: f.resources,
        };
        self.send(player, ServerMsg::Snapshot(Box::new(v)));
    }

    /// One view per tick, for the next fortress in rotation.
    fn send_view(&mut self) {
        let Some(tx) = &self.view_tx else { return };
        let after = self.last_view.map_or(Bound::Unbounded, Bound::Excluded);
        let next = self
            .w
            .fortresses
            .range((after, Bound::Unbounded))
            .next()
            .or_else(|| self.w.fortresses.iter().next());
        let Some((&m, f)) = next else { return };
        self.last_view = Some(m);
        let Some(mt) = self.w.mountains.get(&m) else {
            return;
        };
        let view = AiView {
            mountain: mt.clone(),
            tick: self.w.tick,
            dwarves: f
                .dwarves
                .iter()
                .map(|d| DwarfView {
                    id: d.id,
                    pos: d.pos,
                    idle: d.task.is_none(),
                })
                .collect(),
            claimed: sim::claimed(&self.w, m),
            occupied: sim::occupied(&self.w, m),
        };
        let _ = tx.try_send(view); // a dropped view is fine
    }
}

/// First walkable surface tile, row by row: the entrance strip.
fn entrance(w: &World, m: MountainId) -> Option<TilePos> {
    let mt = w.mountains.get(&m)?;
    let (wd, ht, _) = mt.size();
    (0..ht)
        .flat_map(|y| (0..wd).map(move |x| TilePos::new(x, y, 0)))
        .find(|&p| mt.walkable(p))
}
