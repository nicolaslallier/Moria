//! Pure inventory sync: `Fortress.resources` stays the live cache the sim mutates; each tick's
//! net change is sent to the inventory as a batch, and the confirmed state replaces the cache.

use moria_core::inventory::{Batch, STOPPED_AFTER, State};
use moria_core::inventory_cache::Cache;
use moria_core::world::World;
use moria_core::{MountainId, Resources};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

pub struct InvSync {
    cache: Cache,
    state: State,
    last_state: Option<Instant>,
    seeded: BTreeSet<MountainId>,
    outbox: Vec<Batch>,
    before: BTreeMap<MountainId, Resources>,
    /// No bus: `Fortress.resources` stays authoritative and nothing is cached or batched.
    disabled: bool,
    dropped_seen: u64,
}

impl InvSync {
    pub fn new(sender: &str, epoch: u64) -> InvSync {
        InvSync {
            cache: Cache::new(sender, epoch),
            state: State::default(),
            last_state: None,
            seeded: BTreeSet::new(),
            outbox: Vec::new(),
            before: BTreeMap::new(),
            disabled: false,
            dropped_seen: 0,
        }
    }

    pub fn disabled() -> InvSync {
        InvSync {
            disabled: true,
            ..InvSync::new("", 0)
        }
    }

    /// Seeds new mountains once, then loads the inventory view into every fortress.
    /// Contract: EVERY change to `Fortress.resources` must happen between `begin_tick` and
    /// `end_tick` (anything outside is overwritten by the view or never batched). Task 9's
    /// order: begin_tick -> requests/commands -> sim::step -> end_tick.
    pub fn begin_tick(&mut self, w: &mut World) {
        if self.disabled {
            return;
        }
        for (&m, f) in &w.fortresses {
            if self.seeded.insert(m) {
                let b = self.cache.push(m, f.resources, Resources::default(), true);
                self.outbox.push(b);
            }
        }
        self.before.clear();
        for (&m, f) in &mut w.fortresses {
            f.resources = self.cache.view(&self.state, m);
            self.before.insert(m, f.resources);
        }
    }

    /// Seed batches first, then one batch per fortress whose resources changed this tick.
    /// See `begin_tick` for the contract on when `Fortress.resources` may change.
    // ponytail: net diff per tick, so add-then-take inside one tick clamps on the net; fine unless a tick both exhausts and refills a resource
    pub fn end_tick(&mut self, w: &World) -> Vec<Batch> {
        if self.disabled {
            return Vec::new();
        }
        let mut out = std::mem::take(&mut self.outbox);
        for (&m, f) in &w.fortresses {
            // A fortress claimed mid-run is not in `before`: the next begin_tick seeds it.
            let Some(b) = self.before.get(&m) else {
                continue;
            };
            let (add, take) = Resources::delta(b, &f.resources);
            if add != Resources::default() || take != Resources::default() {
                out.push(self.cache.push(m, add, take, false));
            }
        }
        let dropped = self.cache.dropped();
        if dropped > self.dropped_seen {
            tracing::error!(
                "inventaire injoignable: {} lots non confirmes oubliés par le cache (total {dropped})",
                dropped - self.dropped_seen
            );
            self.dropped_seen = dropped;
        }
        out
    }

    pub fn on_state(&mut self, st: State) {
        self.cache.reconcile(&st);
        self.state = st;
        self.last_state = Some(Instant::now());
    }

    pub fn inventory_up(&self, now: Instant) -> bool {
        self.last_state
            .is_some_and(|t| now.saturating_duration_since(t) < STOPPED_AFTER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moria_core::PlayerId;
    use moria_core::world::{claim_mountain, generate};
    use std::time::Duration;

    const M: MountainId = MountainId(0);

    fn food(n: u32) -> Resources {
        Resources {
            food: n,
            ..Default::default()
        }
    }
    fn stone(n: u32) -> Resources {
        Resources {
            stone: n,
            ..Default::default()
        }
    }
    fn world(r: Resources) -> World {
        let mut w = generate(1);
        claim_mountain(&mut w, PlayerId(1)).unwrap();
        w.fortresses.get_mut(&M).unwrap().resources = r;
        w
    }
    fn res(w: &World) -> Resources {
        w.fortresses[&M].resources
    }
    /// A running inventory: applies batches and feeds the state back.
    fn feed(inv: &mut InvSync, st: &mut State, bs: &[Batch]) {
        for b in bs {
            st.apply(b);
        }
        inv.on_state(st.clone());
    }

    #[test]
    fn disabled_sync_leaves_resources_alone_and_keeps_nothing() {
        let mut w = world(food(12));
        let mut inv = InvSync::disabled();
        for _ in 0..50 {
            inv.begin_tick(&mut w);
            w.fortresses.get_mut(&M).unwrap().resources.add(stone(1));
            assert!(inv.end_tick(&w).is_empty());
        }
        assert_eq!(res(&w).stone, 50);
        assert_eq!(res(&w).food, 12);
        assert_eq!(inv.cache.pending_len(), 0);
        assert!(!inv.inventory_up(Instant::now()));
    }

    #[test]
    fn digging_yield_becomes_an_add_batch() {
        let mut w = world(food(12));
        let mut inv = InvSync::new("s", 1);
        inv.begin_tick(&mut w);
        assert_eq!(inv.end_tick(&w).len(), 1); // the seed
        inv.begin_tick(&mut w);
        w.fortresses.get_mut(&M).unwrap().resources.stone += 3;
        let bs = inv.end_tick(&w);
        assert_eq!(bs.len(), 1);
        assert_eq!(
            (bs[0].add, bs[0].take, bs[0].seed),
            (stone(3), Resources::default(), false)
        );
    }

    #[test]
    fn build_cost_becomes_a_take_batch() {
        let mut w = world(stone(10));
        let mut inv = InvSync::new("s", 1);
        inv.begin_tick(&mut w);
        inv.end_tick(&w);
        inv.begin_tick(&mut w);
        w.fortresses.get_mut(&M).unwrap().resources.stone -= 4;
        let bs = inv.end_tick(&w);
        assert_eq!(bs.len(), 1);
        assert_eq!((bs[0].add, bs[0].take), (Resources::default(), stone(4)));
    }

    #[test]
    fn quiet_tick_makes_no_batch() {
        let mut w = world(food(5));
        let mut inv = InvSync::new("s", 1);
        inv.begin_tick(&mut w);
        inv.end_tick(&w);
        inv.begin_tick(&mut w);
        assert!(inv.end_tick(&w).is_empty());
    }

    #[test]
    fn state_replaces_stale_world_bin_resources() {
        let mut w = world(food(12));
        let mut inv = InvSync::new("s", 1);
        let mut st = State::default();
        inv.begin_tick(&mut w);
        let bs = inv.end_tick(&w);
        // The inventory already knew this mountain with food 7: our seed is ignored.
        st.totals.insert(M, food(7));
        st.seeded.insert(M);
        feed(&mut inv, &mut st, &bs);
        inv.begin_tick(&mut w);
        assert_eq!(res(&w), food(7));
        assert!(inv.end_tick(&w).is_empty()); // the 12 -> 7 jump is not a take
    }

    #[test]
    fn restart_window_does_not_double_count_stale_world_resources() {
        let mut w = world(food(12));
        let mut inv = InvSync::new("s", 1);
        inv.begin_tick(&mut w);
        let seed = inv.end_tick(&w);
        // A state from before our seed arrived: the mountain is already known, no ack of ours.
        let mut st = State::default();
        st.totals.insert(M, food(7));
        st.seeded.insert(M);
        inv.on_state(st.clone());
        inv.begin_tick(&mut w);
        assert_eq!(res(&w), food(7));
        inv.end_tick(&w);
        feed(&mut inv, &mut st, &seed); // now the seed is applied (ignored) and acked
        inv.begin_tick(&mut w);
        assert_eq!(res(&w), food(7));
    }

    #[test]
    fn unacked_batch_survives_a_new_state() {
        let mut w = world(food(12));
        let mut inv = InvSync::new("s", 1);
        let mut st = State::default();
        inv.begin_tick(&mut w);
        let seed = inv.end_tick(&w);
        feed(&mut inv, &mut st, &seed);
        inv.begin_tick(&mut w);
        w.fortresses.get_mut(&M).unwrap().resources.stone += 2;
        let _lost = inv.end_tick(&w); // never reaches the inventory
        inv.on_state(st.clone()); // state without that batch
        inv.begin_tick(&mut w);
        assert_eq!(
            res(&w),
            Resources {
                food: 12,
                stone: 2,
                ..Default::default()
            }
        );
    }

    #[test]
    fn seed_is_sent_once_and_counted_once() {
        let mut w = world(food(12));
        let mut inv = InvSync::new("s", 1);
        inv.begin_tick(&mut w);
        let first = inv.end_tick(&w);
        inv.begin_tick(&mut w);
        let second = inv.end_tick(&w);
        assert_eq!(first.len() + second.len(), 1);
        assert!(first[0].seed);
        assert_eq!(res(&w), food(12));
        let mut st = State::default();
        feed(&mut inv, &mut st, &first);
        inv.begin_tick(&mut w);
        assert_eq!(res(&w), st.totals[&M]);
        assert_eq!(res(&w), food(12));
    }

    #[test]
    fn mid_run_claim_is_seeded_next_tick() {
        let mut w = world(food(1));
        let mut inv = InvSync::new("s", 1);
        inv.begin_tick(&mut w);
        inv.end_tick(&w);
        let id = claim_mountain(&mut w, PlayerId(2)).unwrap();
        w.fortresses.get_mut(&id).unwrap().resources = food(9);
        assert!(inv.end_tick(&w).is_empty()); // no diff invented
        inv.begin_tick(&mut w);
        let bs = inv.end_tick(&w);
        assert_eq!(bs.len(), 1);
        assert!(bs[0].seed && bs[0].mountain == id && bs[0].add == food(9));
        assert_eq!(w.fortresses[&id].resources, food(9));
    }

    #[test]
    fn inventory_up_expires_after_five_seconds() {
        let mut inv = InvSync::new("s", 1);
        assert!(!inv.inventory_up(Instant::now()));
        inv.on_state(State::default());
        let t0 = Instant::now();
        assert!(inv.inventory_up(t0));
        assert!(inv.inventory_up(t0 + STOPPED_AFTER - Duration::from_millis(1)));
        assert!(!inv.inventory_up(t0 + STOPPED_AFTER + Duration::from_secs(1)));
    }
}
