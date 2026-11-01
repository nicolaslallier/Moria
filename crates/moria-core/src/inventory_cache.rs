use crate::inventory::{Batch, State};
use crate::{MountainId, Resources};
use std::collections::VecDeque;

/// Unacked batches kept; beyond it the oldest go (already published, the next ack heals the view).
pub const MAX_PENDING: usize = 50_000;

/// Server-side view of the inventory: confirmed state plus batches not yet acked.
pub struct Cache {
    sender: String,
    epoch: u64,
    next_seq: u64,
    pending: VecDeque<Batch>,
    dropped: u64,
}

impl Cache {
    pub fn new(sender: &str, epoch: u64) -> Cache {
        Cache {
            sender: sender.into(),
            epoch,
            next_seq: 1,
            pending: VecDeque::new(),
            dropped: 0,
        }
    }

    pub fn push(
        &mut self,
        mountain: MountainId,
        add: Resources,
        take: Resources,
        seed: bool,
    ) -> Batch {
        let b = Batch {
            sender: self.sender.clone(),
            epoch: self.epoch,
            seq: self.next_seq,
            mountain,
            add,
            take,
            seed,
        };
        self.next_seq += 1;
        if self.pending.len() >= MAX_PENDING {
            self.pending.pop_front();
            self.dropped += 1;
        }
        self.pending.push_back(b.clone());
        b
    }

    /// Drops our batches the inventory has acked. A newer-epoch ack acks all of ours.
    pub fn reconcile(&mut self, st: &State) {
        let Some(&(e, s)) = st.acks.get(&self.sender) else {
            return;
        };
        if e > self.epoch {
            self.pending.clear();
        } else if e == self.epoch {
            self.pending.retain(|b| b.seq > s);
        }
    }

    pub fn view(&self, st: &State, m: MountainId) -> Resources {
        let mut r = st.totals.get(&m).copied().unwrap_or_default();
        for b in self
            .pending
            .iter()
            .filter(|b| b.mountain == m && !(b.seed && st.seeded.contains(&m)))
        {
            r.add(b.add);
            r.take(&b.take);
        }
        r
    }

    /// Pending batches dropped by the cap since start.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: MountainId = MountainId(0);

    fn res(stone: u32) -> Resources {
        Resources {
            stone,
            ..Default::default()
        }
    }

    fn state(total: u32, ack: Option<(u64, u64)>) -> State {
        let mut s = State::default();
        s.totals.insert(M, res(total));
        if let Some(a) = ack {
            s.acks.insert("srv".into(), a);
        }
        s
    }

    #[test]
    fn view_ignores_pending_seed_for_already_seeded_mountain() {
        let mut c = Cache::new("srv", 1);
        c.push(M, res(12), res(0), true);
        let mut st = state(7, None);
        st.seeded.insert(M);
        assert_eq!(c.view(&st, M).stone, 7);
        assert_eq!(c.view(&state(7, None), M).stone, 19);
    }

    #[test]
    fn pending_is_capped_oldest_first() {
        let mut c = Cache::new("srv", 1);
        for _ in 0..MAX_PENDING + 3 {
            c.push(M, res(1), res(0), false);
        }
        assert_eq!(c.pending_len(), MAX_PENDING);
        assert_eq!(c.dropped(), 3);
        assert_eq!(c.pending.front().unwrap().seq, 4);
    }

    #[test]
    fn push_numbers_from_one() {
        let mut c = Cache::new("srv", 7);
        let a = c.push(M, res(1), res(0), false);
        let b = c.push(M, res(1), res(0), true);
        assert_eq!((a.seq, b.seq), (1, 2));
        assert_eq!((a.epoch, a.sender.as_str()), (7, "srv"));
        assert!(b.seed);
        assert_eq!(c.pending_len(), 2);
    }

    #[test]
    fn view_is_totals_plus_pending() {
        let mut c = Cache::new("srv", 1);
        c.push(M, res(5), res(2), false);
        assert_eq!(c.view(&state(10, None), M).stone, 13);
        assert_eq!(c.view(&State::default(), M).stone, 3);
    }

    #[test]
    fn acked_batches_are_not_counted_twice() {
        let mut c = Cache::new("srv", 1);
        c.push(M, res(3), res(0), false);
        let st = state(3, Some((1, 1)));
        c.reconcile(&st);
        assert_eq!(c.pending_len(), 0);
        assert_eq!(c.view(&st, M).stone, 3);
    }

    #[test]
    fn ack_of_other_epoch_keeps_pending() {
        let mut c = Cache::new("srv", 2);
        c.push(M, res(3), res(0), false);
        let st = state(0, Some((1, 99)));
        c.reconcile(&st);
        assert_eq!(c.pending_len(), 1);
        assert_eq!(c.view(&st, M).stone, 3);
    }

    #[test]
    fn newer_epoch_ack_drops_older_pending() {
        let mut c = Cache::new("srv", 1);
        c.push(M, res(3), res(0), false);
        c.reconcile(&state(0, Some((2, 1))));
        assert_eq!(c.pending_len(), 0);
    }

    #[test]
    fn unacked_batch_stays_in_view() {
        let mut c = Cache::new("srv", 1);
        c.push(M, res(3), res(0), false);
        c.push(M, res(4), res(0), false);
        let st = state(3, Some((1, 1)));
        c.reconcile(&st);
        assert_eq!(c.pending_len(), 1);
        assert_eq!(c.view(&st, M).stone, 7);
    }

    #[test]
    fn view_ignores_other_mountains() {
        let other = MountainId(1);
        let mut c = Cache::new("srv", 1);
        c.push(M, res(2), res(0), false);
        c.push(other, res(4), res(0), false);
        let mut st = state(10, None);
        st.totals.insert(other, res(100));
        assert_eq!(c.view(&st, M).stone, 12);
        assert_eq!(c.view(&st, other).stone, 104);
    }

    #[test]
    fn view_applies_add_before_take() {
        let mut c = Cache::new("srv", 1);
        c.push(M, res(5), res(3), false);
        assert_eq!(c.view(&State::default(), M).stone, 2);
    }

    #[test]
    fn view_clamps_at_zero() {
        let mut c = Cache::new("srv", 1);
        c.push(M, res(0), res(9), false);
        assert_eq!(c.view(&state(2, None), M).stone, 0);
    }
}
