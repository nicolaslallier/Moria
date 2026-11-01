use crate::inventory::{Batch, State};
use crate::{MountainId, Resources};

/// Server-side view of the inventory: confirmed state plus batches not yet acked.
pub struct Cache {
    sender: String,
    epoch: u64,
    next_seq: u64,
    pending: Vec<Batch>,
}

impl Cache {
    pub fn new(sender: &str, epoch: u64) -> Cache {
        Cache {
            sender: sender.into(),
            epoch,
            next_seq: 1,
            pending: Vec::new(),
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
        self.pending.push(b.clone());
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
        for b in self.pending.iter().filter(|b| b.mountain == m) {
            r.add(b.add);
            r.take(&b.take);
        }
        r
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
    fn view_clamps_at_zero() {
        let mut c = Cache::new("srv", 1);
        c.push(M, res(0), res(9), false);
        assert_eq!(c.view(&state(2, None), M).stone, 0);
    }
}
