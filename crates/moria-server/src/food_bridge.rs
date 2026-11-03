//! Link with the food container: the world task queues `Outgoing` messages and drains
//! `Incoming` ones through bounded channels (`try_send`, a full channel drops the message);
//! `run` is the bus side. With a bus, the container owns the food simulation and the server
//! only checks its proposals against the grid.

use crate::food::{apply_bus_events, designate_harvest, snapshot};
use crate::metrics::{M, inc as inc_metric};
use crate::sim::Changes;
use futures::StreamExt;
use moria_bus::{Bus, Queue, redact_url};
use moria_core::food::{FoodAction, FoodEvent};
use moria_core::food_msg::{
    Actions, Events, KEY_ACTION, KEY_EVENTS, KEY_REJECT, KEY_STATE, KEY_TICK, KEY_WORLD,
    MAX_FOOD_MSG_BYTES, MAX_FOOD_WORLD_BYTES, Rejects, Status, Tick, World, decode,
};
use moria_core::inventory::STOPPED_AFTER;
use moria_core::world::World as GameWorld;
use moria_core::{MountainId, Tile, TilePos};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Anti-entropy: a full `World` this often while the container is up.
pub const WORLD_EVERY: Duration = Duration::from_secs(300);
/// A `World` again this often while the heartbeat says none was adopted (tick 0).
pub const WORLD_RETRY: Duration = Duration::from_secs(5);
/// Events proposed for a tick older than this (5 s) are refused whole.
// ponytail: tick-based staleness per tile, no entity ids; ids if two lives of one tile within
// 5 s ever matter.
pub use moria_core::food_msg::MAX_EVENT_AGE;
/// Most incoming messages taken per tick.
pub const MAX_IN_PER_TICK: usize = 64;
/// Incoming channel capacity: a backlog of many ticks before anything is dropped.
pub const IN_CHANNEL: usize = 1024;
/// Outgoing channel capacity: a tick, a few actions and rejects per tick.
pub const OUT_CHANNEL: usize = 256;
/// Actions per message: a `FoodAction` encodes in at most 8 bytes, so this stays far under
/// `MAX_FOOD_MSG_BYTES`.
const ACTIONS_PER_MSG: usize = 1024;

pub enum Outgoing {
    Tick(Tick),
    World(World),
    Actions(Actions),
    Rejects(Rejects),
}

pub enum Incoming {
    Events(Events),
    Status(Status),
}

/// Bounded decoding of an untrusted message by its routing key; anything else is `None`.
pub fn decode_incoming(key: &str, bytes: &[u8]) -> Option<Incoming> {
    let i = match key {
        KEY_EVENTS => decode(bytes, MAX_FOOD_MSG_BYTES).map(Incoming::Events),
        KEY_STATE => decode(bytes, MAX_FOOD_MSG_BYTES).map(Incoming::Status),
        _ => return None,
    };
    if i.is_none() {
        tracing::warn!(
            "message {key} illisible ou trop gros, ignore ({} octets)",
            bytes.len()
        );
    }
    i
}

/// The world task's side of the link.
pub struct Link {
    tx: mpsc::Sender<Outgoing>,
    rx: mpsc::Receiver<Incoming>,
    /// Last heartbeat.
    pub up_at: Option<Instant>,
    /// When the last `World` went out; `None` = send one as soon as the container is up.
    world_at: Option<Instant>,
    /// Mountains in the last `World`: another set needs a new `World`.
    world_ids: Vec<MountainId>,
    /// Tick of the last heartbeat: going back means the container restarted, 0 that it has
    /// adopted no `World` yet.
    status_tick: u64,
    inbox: Vec<Events>,
    /// Server tick of the last harvest or sowing per tile, younger than `MAX_EVENT_AGE`.
    touched: HashMap<(MountainId, TilePos), u64>,
    /// Set by the bus side when it dropped events: the container is out of step.
    dropped: Arc<AtomicBool>,
    /// Events were dropped since the last `World`: another one after `WORLD_RETRY`.
    resync: bool,
}

/// The food freezes, and meals are skipped, while a configured container is silent.
pub fn paused(link: Option<&Link>, now: Instant) -> bool {
    link.is_some_and(|l| !l.up(now))
}

impl Link {
    pub fn new(tx: mpsc::Sender<Outgoing>, rx: mpsc::Receiver<Incoming>) -> Self {
        Self {
            tx,
            rx,
            up_at: None,
            world_at: None,
            world_ids: vec![],
            status_tick: 0,
            inbox: vec![],
            touched: HashMap::new(),
            dropped: Arc::default(),
            resync: false,
        }
    }

    /// For `run`: raised when incoming events are dropped.
    pub fn drop_flag(&self) -> Arc<AtomicBool> {
        self.dropped.clone()
    }

    pub fn up(&self, now: Instant) -> bool {
        self.up_at
            .is_some_and(|t| now.saturating_duration_since(t) < STOPPED_AFTER)
    }

    /// Start of a tick: takes the heartbeats and keeps the events for `step`.
    pub fn poll(&mut self, now: Instant) {
        // Its unanswered proposals stay in its state: resync it, at most every WORLD_RETRY
        // (sustained drops must not build a big `World` every tick).
        self.resync |= self.dropped.swap(false, Relaxed);
        for _ in 0..MAX_IN_PER_TICK {
            match self.rx.try_recv() {
                Ok(Incoming::Status(s)) => {
                    if s.tick < self.status_tick {
                        self.world_at = None; // restarted, maybe empty: catch it up now
                    }
                    self.status_tick = s.tick;
                    self.up_at = Some(now);
                }
                Ok(Incoming::Events(e)) => self.inbox.push(e),
                Err(_) => break,
            }
        }
    }

    /// `try_send`: a full channel drops the message (the next `World` repairs any loss).
    fn send(&self, o: Outgoing) -> bool {
        self.tx.try_send(o).is_ok()
    }

    /// Food part of a tick, after `sim::step` (`w.tick` is the delta's tick): applies the
    /// valid events, sends the rejects back, then the `World` when due, `acts` and the tick.
    pub fn step(
        &mut self,
        w: &mut GameWorld,
        acts: &BTreeMap<MountainId, Vec<FoodAction>>,
        now: Instant,
    ) -> BTreeMap<MountainId, Changes> {
        let tick = w.tick;
        for (&id, list) in acts {
            for a in list {
                if let FoodAction::FieldHarvested(q) | FoodAction::FieldSown(q) = *a {
                    self.touched.insert((id, q), tick);
                }
            }
        }
        // Any event older than an entry here is refused by age anyway.
        self.touched.retain(|_, t| *t + MAX_EVENT_AGE >= tick);

        let mut out: BTreeMap<MountainId, Changes> = BTreeMap::new();
        for ev in std::mem::take(&mut self.inbox) {
            let id = ev.mountain;
            let (Some(m), Some(f)) = (w.mountains.get_mut(&id), w.fortresses.get(&id)) else {
                tracing::warn!("evenements nourriture ignores: montagne inconnue {id:?}");
                continue;
            };
            // The tick is untrusted: one from the future (an older save restarted) or too
            // old is refused whole.
            let rejected = if ev.tick > tick || ev.tick.saturating_add(MAX_EVENT_AGE) < tick {
                ev.list
            } else {
                // A ripe proposed before the tile's last harvest or sowing is stale.
                let touched = &self.touched;
                let stale = |q| touched.get(&(id, q)).is_some_and(|&t| ev.tick <= t);
                let ch = out.entry(id).or_default();
                let n = ch.tiles.len();
                let rejected = apply_bus_events(m, f, &ev.list, stale, &mut ch.tiles);
                // A crop just ripened asks for its harvest now, not at the next periodic
                // check (`keep_harvest_designated` stays the backstop).
                let ripe: Vec<_> = ch.tiles[n..]
                    .iter()
                    .filter(|&&(_, t)| t == Tile::Crop)
                    .map(|&(q, _)| q)
                    .collect();
                for q in ripe {
                    designate_harvest(m, q, ch);
                }
                rejected
            };
            // A move refused because its cow is gone (butchered after the container moved
            // it): the reject would put a ghost back at `from`, and the earlier AnimalRemoved
            // missed it. Remove both ends; rejects and actions travel on separate queues, so
            // this holds in either order of arrival.
            let fix: Vec<FoodAction> = rejected
                .iter()
                .filter_map(|e| match *e {
                    FoodEvent::AnimalMoved { from, to } if m.get(from) != Some(Tile::Cow) => {
                        Some([
                            FoodAction::AnimalRemoved(from),
                            FoodAction::AnimalRemoved(to),
                        ])
                    }
                    _ => None,
                })
                .flatten()
                .collect();
            if !rejected.is_empty() {
                self.send(Outgoing::Rejects(Rejects {
                    tick: ev.tick,
                    mountain: id,
                    list: rejected,
                }));
            }
            for a in chunk_actions(tick, id, &fix) {
                self.send(Outgoing::Actions(a));
            }
        }
        out.retain(|_, ch| *ch != Changes::default());

        // Heartbeat tick 0: the container has adopted no `World` since it started.
        let every = if self.status_tick == 0 || self.resync {
            WORLD_RETRY
        } else {
            WORLD_EVERY
        };
        if !self.up(now) {
            self.world_at = None;
        } else if !w.fortresses.keys().eq(&self.world_ids)
            || self
                .world_at
                .is_none_or(|t| now.saturating_duration_since(t) >= every)
        {
            // One message with every fortress, even those without cells: the container
            // forgets the mountains a `World` leaves out.
            let mountains = w
                .fortresses
                .keys()
                .filter_map(|&id| w.mountains.get(&id).map(|m| (id, snapshot(m))))
                .collect();
            if self.send(Outgoing::World(World { tick, mountains })) {
                self.world_at = Some(now);
                self.resync = false;
                self.world_ids = w.fortresses.keys().copied().collect();
            }
        }
        for (&id, list) in acts {
            for a in chunk_actions(tick, id, list) {
                self.send(Outgoing::Actions(a));
            }
        }
        self.send(Outgoing::Tick(Tick { tick }));
        out
    }
}

/// `acts` in messages that each encode under `MAX_FOOD_MSG_BYTES`.
pub fn chunk_actions(tick: u64, mountain: MountainId, acts: &[FoodAction]) -> Vec<Actions> {
    acts.chunks(ACTIONS_PER_MSG)
        .map(|c| Actions {
            tick,
            mountain,
            list: c.to_vec(),
        })
        .collect()
}

/// Encodes for the bus; a message the container would refuse as too big is dropped here.
// ponytail: `World` is never split (the container forgets mountains a `World` leaves out);
// 2 MiB holds every ground tile of all 16 mountains. Split it with a "last part" flag if
// `moria_food_world_dropped_total` ever moves.
fn encode(o: &Outgoing) -> Option<(&'static str, Vec<u8>)> {
    let (key, max, bytes) = match o {
        Outgoing::Tick(t) => (KEY_TICK, MAX_FOOD_MSG_BYTES, postcard::to_allocvec(t)),
        Outgoing::World(w) => (KEY_WORLD, MAX_FOOD_WORLD_BYTES, postcard::to_allocvec(w)),
        Outgoing::Actions(a) => (KEY_ACTION, MAX_FOOD_MSG_BYTES, postcard::to_allocvec(a)),
        Outgoing::Rejects(r) => (KEY_REJECT, MAX_FOOD_MSG_BYTES, postcard::to_allocvec(r)),
    };
    match bytes {
        Ok(b) if b.len() <= max => Some((key, b)),
        Ok(b) => {
            if key == KEY_WORLD {
                inc_metric(&M.food_world_dropped);
            }
            tracing::error!("message {key} trop gros ({} octets), perdu", b.len());
            None
        }
        Err(e) => {
            tracing::error!("encodage {key} impossible: {e}");
            None
        }
    }
}

/// Resolves once the world task is gone; what it sends meanwhile is dropped (nothing here
/// is durable: the next `World` catches the container up).
async fn closed(out: &mut mpsc::Receiver<Outgoing>) {
    while out.recv().await.is_some() {}
}

/// Hands an incoming message to the world task. Events dropped on a full channel are never
/// answered, so the container's optimistic state drifts: `dropped` asks for a fresh `World`.
/// A dropped heartbeat is harmless.
fn forward(inc: &mpsc::Sender<Incoming>, i: Incoming, dropped: &AtomicBool) {
    if let Err(mpsc::error::TrySendError::Full(Incoming::Events(_))) = inc.try_send(i) {
        dropped.store(true, Relaxed);
        inc_metric(&M.food_events_dropped);
        tracing::warn!("evenements nourriture perdus: le monde est en retard");
    }
}

/// Runs until `out` closes. Own connection, reconnects 2 s after the link breaks.
pub async fn run(
    url: String,
    mut out: mpsc::Receiver<Outgoing>,
    inc: mpsc::Sender<Incoming>,
    dropped: Arc<AtomicBool>,
) {
    loop {
        let bus = tokio::select! {
            b = Bus::connect_forever(&url) => b,
            () = closed(&mut out) => return,
        };
        match (
            bus.subscribe(KEY_EVENTS, Queue::Exclusive).await,
            bus.subscribe(KEY_STATE, Queue::Exclusive).await,
        ) {
            (Ok(mut events), Ok(mut states)) => {
                tracing::info!("lien nourriture connecte a {}", redact_url(&url));
                loop {
                    tokio::select! {
                        o = out.recv() => {
                            let Some(o) = o else {
                                let _ = tokio::time::timeout(Duration::from_secs(2), bus.close()).await;
                                return;
                            };
                            let Some((key, b)) = encode(&o) else { continue };
                            if let Err(e) = bus.publish(key, b, false).await {
                                tracing::warn!("publication {key} echouee: {e}");
                                break;
                            }
                        }
                        m = events.next() => {
                            let Some(m) = m else { break };
                            if let Some(i) = decode_incoming(KEY_EVENTS, &m) {
                                forward(&inc, i, &dropped);
                            }
                        }
                        m = states.next() => {
                            let Some(m) = m else { break };
                            if let Some(i) = decode_incoming(KEY_STATE, &m) {
                                forward(&inc, i, &dropped);
                            }
                        }
                    }
                }
            }
            (Err(e), _) | (_, Err(e)) => tracing::warn!("abonnement nourriture impossible: {e}"),
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), bus.close()).await;
        tracing::warn!("lien nourriture interrompu, reconnexion dans 2 s");
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(2)) => {}
            () = closed(&mut out) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::tests::{M, cow_pen, p, setup};
    use moria_core::Dir;

    type Ends = (mpsc::Receiver<Outgoing>, mpsc::Sender<Incoming>);

    fn link() -> (Link, Ends) {
        let (tx, out) = mpsc::channel(OUT_CHANNEL);
        let (inc, rx) = mpsc::channel(IN_CHANNEL);
        (Link::new(tx, rx), (out, inc))
    }
    /// Heartbeat of a container that has adopted a `World`.
    fn beat(inc: &mpsc::Sender<Incoming>) {
        beat_at(inc, 1);
    }
    fn beat_at(inc: &mpsc::Sender<Incoming>, tick: u64) {
        inc.try_send(Incoming::Status(Status { tick })).unwrap();
    }
    /// One server tick of the link: poll, then step with `acts`.
    fn tick(
        l: &mut Link,
        w: &mut GameWorld,
        acts: &[FoodAction],
        now: Instant,
    ) -> BTreeMap<MountainId, Changes> {
        l.poll(now);
        w.tick += 1;
        l.step(w, &BTreeMap::from([(M, acts.to_vec())]), now)
    }
    fn sent(out: &mut mpsc::Receiver<Outgoing>) -> Vec<Outgoing> {
        std::iter::from_fn(|| out.try_recv().ok()).collect()
    }
    fn worlds(v: &[Outgoing]) -> Vec<&World> {
        v.iter()
            .filter_map(|o| match o {
                Outgoing::World(w) => Some(w),
                _ => None,
            })
            .collect()
    }
    fn rejects(v: &[Outgoing]) -> Vec<&Rejects> {
        v.iter()
            .filter_map(|o| match o {
                Outgoing::Rejects(r) => Some(r),
                _ => None,
            })
            .collect()
    }
    fn secs(t: Instant, s: u64) -> Instant {
        t + Duration::from_secs(s)
    }

    #[test]
    fn garbage_and_oversized_incoming_are_ignored() {
        let ev = postcard::to_allocvec(&Events {
            tick: 3,
            mountain: M,
            list: vec![],
        })
        .unwrap();
        let st = postcard::to_allocvec(&Status { tick: 3 }).unwrap();
        assert!(matches!(
            decode_incoming(KEY_EVENTS, &ev),
            Some(Incoming::Events(_))
        ));
        assert!(matches!(
            decode_incoming(KEY_STATE, &st),
            Some(Incoming::Status(_))
        ));
        assert!(decode_incoming(KEY_EVENTS, &[0xff; 9]).is_none());
        assert!(decode_incoming("food.autre", &st).is_none());
        let mut big = ev.clone();
        big.resize(MAX_FOOD_MSG_BYTES + 1, 0);
        // trailing bytes would decode fine, so only the cap can reject this
        assert!(postcard::from_bytes::<Events>(&big).is_ok());
        assert!(decode_incoming(KEY_EVENTS, &big).is_none());
    }

    #[test]
    fn food_up_expires_after_five_seconds() {
        let (mut l, (_out, inc)) = link();
        let t0 = Instant::now();
        assert!(!l.up(t0), "never heard of");
        beat(&inc);
        l.poll(t0);
        assert!(l.up(t0));
        assert!(l.up(t0 + Duration::from_millis(4_999)));
        assert!(!l.up(secs(t0, 5)));
    }

    #[test]
    fn food_paused_follows_staleness() {
        let t0 = Instant::now();
        assert!(!paused(None, t0), "no bus: food always runs");
        let (mut l, (_out, inc)) = link();
        assert!(paused(Some(&l), t0));
        beat(&inc);
        l.poll(t0);
        assert!(!paused(Some(&l), secs(t0, 4)));
        assert!(paused(Some(&l), secs(t0, 5)));
    }

    #[test]
    fn world_is_resent_when_heartbeat_returns() {
        let (mut l, (mut out, inc)) = link();
        let mut w = setup();
        let t0 = Instant::now();
        tick(&mut l, &mut w, &[], t0);
        assert!(worlds(&sent(&mut out)).is_empty(), "nobody listens yet");
        beat(&inc);
        tick(&mut l, &mut w, &[], secs(t0, 1));
        let v = sent(&mut out);
        let ws = worlds(&v);
        assert_eq!(ws.len(), 1, "first heartbeat");
        let ids: Vec<_> = ws[0].mountains.iter().map(|m| m.0).collect();
        assert_eq!(ids, w.fortresses.keys().copied().collect::<Vec<_>>());
        assert_eq!(ws[0].tick, w.tick);
        beat(&inc);
        tick(&mut l, &mut w, &[], secs(t0, 2));
        assert!(worlds(&sent(&mut out)).is_empty(), "still up: no resend");
        tick(&mut l, &mut w, &[], secs(t0, 10)); // heartbeat lost
        assert!(worlds(&sent(&mut out)).is_empty());
        beat(&inc);
        tick(&mut l, &mut w, &[], secs(t0, 11));
        assert_eq!(worlds(&sent(&mut out)).len(), 1, "back after a gap");
        // a restart faster than STOPPED_AFTER: its heartbeat's tick goes back
        inc.try_send(Incoming::Status(Status { tick: 50 })).unwrap();
        tick(&mut l, &mut w, &[], secs(t0, 11));
        assert!(worlds(&sent(&mut out)).is_empty());
        beat(&inc);
        tick(&mut l, &mut w, &[], secs(t0, 11));
        assert_eq!(worlds(&sent(&mut out)).len(), 1, "restarted container");
        // every tick sends its tick
        tick(&mut l, &mut w, &[], secs(t0, 12));
        assert!(
            sent(&mut out)
                .iter()
                .any(|o| matches!(o, Outgoing::Tick(t) if t.tick == w.tick))
        );
    }

    #[test]
    fn world_is_resent_while_the_container_has_none() {
        // Heartbeat tick 0: no `World` adopted since the container started.
        let (mut l, (mut out, inc)) = link();
        let mut w = setup();
        let t0 = Instant::now();
        let mut at = |l: &mut Link, w: &mut GameWorld, tick, s| {
            beat_at(&inc, tick);
            self::tick(l, w, &[], secs(t0, s));
            worlds(&sent(&mut out)).len()
        };
        assert_eq!(at(&mut l, &mut w, 0, 0), 1, "first heartbeat");
        assert_eq!(at(&mut l, &mut w, 0, 1), 0, "not every tick");
        assert_eq!(
            at(&mut l, &mut w, 0, WORLD_RETRY.as_secs()),
            1,
            "lost World: again"
        );
        assert_eq!(at(&mut l, &mut w, 7, 20), 0, "adopted");
        assert_eq!(at(&mut l, &mut w, 7, 40), 0);
        // a restart faster than STOPPED_AFTER: tick 0 again
        assert_eq!(at(&mut l, &mut w, 0, 41), 1, "restarted");
    }

    #[test]
    fn dropped_events_force_a_fresh_world() {
        use crate::metrics::M as METRICS;
        use std::sync::atomic::Ordering::Relaxed;
        let (mut l, (mut out, inc)) = link();
        let mut w = setup();
        let t0 = Instant::now();
        beat(&inc);
        tick(&mut l, &mut w, &[], t0);
        assert_eq!(worlds(&sent(&mut out)).len(), 1);
        // the bus side finds the world task's channel full: the events are lost
        let (full, _rx) = mpsc::channel(1);
        full.try_send(Incoming::Status(Status { tick: 1 })).unwrap();
        let before = METRICS.food_events_dropped.load(Relaxed);
        let drop_one = |l: &Link| {
            let ev = Events {
                tick: 1,
                mountain: M,
                list: vec![],
            };
            forward(&full, Incoming::Events(ev), &l.drop_flag());
        };
        drop_one(&l);
        assert!(METRICS.food_events_dropped.load(Relaxed) > before);
        // throttled to WORLD_RETRY: no 2 MiB World every tick while drops go on
        let mut worlds_at = |l: &mut Link, w: &mut GameWorld, s| {
            beat(&inc);
            tick(l, w, &[], secs(t0, s));
            worlds(&sent(&mut out)).len()
        };
        assert_eq!(
            worlds_at(&mut l, &mut w, 1),
            0,
            "too soon after the last World"
        );
        assert_eq!(
            worlds_at(&mut l, &mut w, WORLD_RETRY.as_secs()),
            1,
            "container left out of step"
        );
        assert_eq!(worlds_at(&mut l, &mut w, 6), 0, "once");
        drop_one(&l);
        assert_eq!(worlds_at(&mut l, &mut w, 7), 0, "throttled");
        assert_eq!(worlds_at(&mut l, &mut w, 10), 1);
    }

    #[test]
    fn world_is_resent_every_five_minutes() {
        let (mut l, (mut out, inc)) = link();
        let mut w = setup();
        let t0 = Instant::now();
        beat(&inc);
        tick(&mut l, &mut w, &[], t0);
        assert_eq!(worlds(&sent(&mut out)).len(), 1);
        beat(&inc);
        tick(&mut l, &mut w, &[], secs(t0, 299));
        assert!(worlds(&sent(&mut out)).is_empty());
        beat(&inc);
        tick(&mut l, &mut w, &[], secs(t0, 300));
        assert_eq!(worlds(&sent(&mut out)).len(), 1);
        // a newly claimed mountain is sent at once
        moria_core::world::claim_mountain(&mut w, moria_core::PlayerId(9)).unwrap();
        beat(&inc);
        tick(&mut l, &mut w, &[], secs(t0, 301));
        let v = sent(&mut out);
        assert_eq!(worlds(&v).len(), 1);
        assert_eq!(worlds(&v)[0].mountains.len(), 2);
    }

    #[test]
    fn events_are_validated_and_rejects_sent_back() {
        let (mut l, (mut out, inc)) = link();
        let mut w = setup();
        let t0 = Instant::now();
        w.tick = 1_000;
        let c = p(-6, 1, 0);
        cow_pen(&mut w, c, &[Dir::N]);
        let good = FoodEvent::AnimalMoved {
            from: c,
            to: c.step(Dir::N),
        };
        let bad = FoodEvent::AnimalMoved {
            from: c,
            to: c.step(Dir::S), // a tree
        };
        let send = |tick, list| {
            inc.try_send(Incoming::Events(Events {
                tick,
                mountain: M,
                list,
            }))
            .unwrap()
        };
        send(1_000, vec![good, bad]);
        let ch = tick(&mut l, &mut w, &[], t0);
        assert_eq!(w.mountains[&M].get(c.step(Dir::N)), Some(Tile::Cow));
        assert_eq!(
            ch[&M].tiles,
            vec![(c, Tile::Grass), (c.step(Dir::N), Tile::Cow)]
        );
        let v = sent(&mut out);
        let r = rejects(&v);
        assert_eq!(r.len(), 1, "one message per mountain");
        assert_eq!(
            (r[0].tick, r[0].mountain),
            (1_000, M),
            "the proposal's tick"
        );
        assert_eq!(r[0].list, vec![bad]);

        // too old: refused whole, even a valid move
        let back = FoodEvent::AnimalMoved {
            from: c.step(Dir::N),
            to: c,
        };
        send(w.tick - MAX_EVENT_AGE - 1, vec![back]);
        tick(&mut l, &mut w, &[], t0);
        assert_eq!(rejects(&sent(&mut out))[0].list, vec![back]);
        assert_eq!(w.mountains[&M].get(c), Some(Tile::Grass));

        // a ripe proposed before the harvest the server just did is stale
        let q = p(-3, 1, 0);
        w.mountains.get_mut(&M).unwrap().set(q, Tile::Field);
        tick(&mut l, &mut w, &[FoodAction::FieldHarvested(q)], t0);
        let harvested_at = w.tick;
        let v = sent(&mut out);
        assert!(v.iter().any(|o| matches!(o, Outgoing::Actions(a)
            if a.tick == harvested_at && a.list == vec![FoodAction::FieldHarvested(q)])));
        let ripe = FoodEvent::CropRipe { at: q };
        send(harvested_at - 1, vec![ripe]);
        tick(&mut l, &mut w, &[], t0);
        assert_eq!(rejects(&sent(&mut out))[0].list, vec![ripe]);
        assert_eq!(w.mountains[&M].get(q), Some(Tile::Field));
        send(harvested_at + 1, vec![ripe]);
        tick(&mut l, &mut w, &[], t0);
        assert!(rejects(&sent(&mut out)).is_empty());
        assert_eq!(w.mountains[&M].get(q), Some(Tile::Crop));
        // the memory of the harvest does not grow forever
        for _ in 0..=MAX_EVENT_AGE {
            tick(&mut l, &mut w, &[], t0);
        }
        assert!(l.touched.is_empty());
    }

    #[test]
    fn a_move_refused_for_a_missing_origin_removes_the_cow_in_the_container() {
        let (mut l, (mut out, inc)) = link();
        let mut w = setup();
        w.tick = 1_000;
        let c = p(-6, 1, 0);
        cow_pen(&mut w, c, &[Dir::N]);
        let kept = FoodEvent::AnimalMoved {
            from: c,
            to: c.step(Dir::S), // a tree: refused, but the cow is still there
        };
        w.mountains.get_mut(&M).unwrap().set(c, Tile::Grass); // butchered meanwhile
        let gone = FoodEvent::AnimalMoved {
            from: c,
            to: c.step(Dir::N),
        };
        inc.try_send(Incoming::Events(Events {
            tick: 1_000,
            mountain: M,
            list: vec![gone],
        }))
        .unwrap();
        tick(&mut l, &mut w, &[], Instant::now());
        let v = sent(&mut out);
        let r = v
            .iter()
            .position(|o| matches!(o, Outgoing::Rejects(r) if r.list == vec![gone]))
            .unwrap();
        let fix = v.iter().position(|o| {
            matches!(o, Outgoing::Actions(a) if a.mountain == M && a.tick == w.tick
                && a.list == vec![FoodAction::AnimalRemoved(c), FoodAction::AnimalRemoved(c.step(Dir::N))])
        });
        assert!(
            fix.is_some_and(|f| f > r),
            "no correction after the reject: {fix:?}"
        );
        assert_eq!(w.mountains[&M].get(c.step(Dir::N)), Some(Tile::Grass));

        // a move refused while its cow is still there needs no correction
        w.mountains.get_mut(&M).unwrap().set(c, Tile::Cow);
        inc.try_send(Incoming::Events(Events {
            tick: w.tick,
            mountain: M,
            list: vec![kept],
        }))
        .unwrap();
        tick(&mut l, &mut w, &[], Instant::now());
        let v = sent(&mut out);
        assert_eq!(rejects(&v)[0].list, vec![kept]);
        assert!(!v.iter().any(|o| matches!(o, Outgoing::Actions(_))));
    }

    #[test]
    fn a_ripe_event_asks_for_its_harvest_at_once() {
        use moria_core::DesignationKind;
        let (mut l, (_out, inc)) = link();
        let mut w = setup();
        w.tick = 1_000; // the next tick (1_001) is no periodic check
        let q = p(-3, 1, 0);
        w.mountains.get_mut(&M).unwrap().set(q, Tile::Field);
        inc.try_send(Incoming::Events(Events {
            tick: 1_000,
            mountain: M,
            list: vec![FoodEvent::CropRipe { at: q }],
        }))
        .unwrap();
        let ch = tick(&mut l, &mut w, &[], Instant::now());
        assert_eq!(w.mountains[&M].get(q), Some(Tile::Crop));
        assert_eq!(
            w.mountains[&M].designations.get(&q),
            Some(&DesignationKind::Chop)
        );
        assert_eq!(
            ch[&M].designations.get(&q),
            Some(&Some(DesignationKind::Chop))
        );
    }

    #[test]
    fn untrusted_event_ticks_are_refused() {
        let (mut l, (mut out, inc)) = link();
        let mut w = setup();
        w.tick = 1_000;
        let c = p(-6, 1, 0);
        cow_pen(&mut w, c, &[Dir::N]);
        let good = FoodEvent::AnimalMoved {
            from: c,
            to: c.step(Dir::N),
        };
        for bad_tick in [u64::MAX, 5_000] {
            inc.try_send(Incoming::Events(Events {
                tick: bad_tick,
                mountain: M,
                list: vec![good],
            }))
            .unwrap();
            tick(&mut l, &mut w, &[], Instant::now()); // must not panic
            let v = sent(&mut out);
            let r = rejects(&v);
            assert_eq!(r.len(), 1, "tick {bad_tick}");
            assert_eq!((r[0].tick, &r[0].list), (bad_tick, &vec![good]));
            assert_eq!(w.mountains[&M].get(c), Some(Tile::Cow), "future: refused");
        }
    }

    #[test]
    fn world_is_resent_when_the_fortresses_change() {
        let (mut l, (mut out, inc)) = link();
        let mut w = setup();
        let t0 = Instant::now();
        beat(&inc);
        tick(&mut l, &mut w, &[], t0);
        assert_eq!(worlds(&sent(&mut out)).len(), 1);
        // one fortress gone, another claimed: same count, other mountains
        let f = w.fortresses.remove(&M).unwrap();
        w.fortresses.insert(MountainId(5), f);
        beat(&inc);
        tick(&mut l, &mut w, &[], secs(t0, 1));
        let v = sent(&mut out);
        assert_eq!(worlds(&v).len(), 1);
        assert_eq!(worlds(&v)[0].mountains[0].0, MountainId(5));
    }

    #[test]
    fn an_oversized_world_is_dropped_and_counted() {
        use crate::metrics::M as METRICS;
        use moria_core::food::{Cell, CellKind};
        use std::sync::atomic::Ordering::Relaxed;
        let cells = |n: i16| -> Vec<Cell> {
            (0..n)
                .map(|i| Cell {
                    pos: TilePos::new(i16::MAX - i, i16::MIN + i, u8::MAX),
                    kind: CellKind::Cow,
                })
                .collect()
        };
        let small = Outgoing::World(World {
            tick: 1,
            mountains: vec![(M, cells(10))],
        });
        assert!(encode(&small).is_some());
        let big = Outgoing::World(World {
            tick: 1,
            mountains: (0..60).map(|i| (MountainId(i), cells(i16::MAX))).collect(),
        });
        let before = METRICS.food_world_dropped.load(Relaxed);
        assert!(encode(&big).is_none());
        assert!(METRICS.food_world_dropped.load(Relaxed) > before);
    }

    #[test]
    fn actions_are_chunked_under_the_message_limit() {
        let far = |i: i16| TilePos::new(i16::MAX - i, i16::MIN + i, u8::MAX);
        let acts: Vec<_> = (0..4096)
            .map(|i| FoodAction::FieldHarvested(far(i)))
            .collect();
        let msgs = chunk_actions(u64::MAX, MountainId(u16::MAX), &acts);
        assert!(msgs.len() > 1);
        for a in &msgs {
            assert!(postcard::to_allocvec(a).unwrap().len() <= MAX_FOOD_MSG_BYTES);
            assert_eq!((a.tick, a.mountain), (u64::MAX, MountainId(u16::MAX)));
        }
        let all: Vec<_> = msgs.into_iter().flat_map(|a| a.list).collect();
        assert_eq!(all, acts);
        assert!(chunk_actions(1, M, &[]).is_empty(), "no empty message");
    }
}
