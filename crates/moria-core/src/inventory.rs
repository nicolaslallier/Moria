use crate::{MountainId, Resources};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

pub const EXCHANGE: &str = "moria";
pub const KEY_APPLY: &str = "inventory.apply";
pub const KEY_STATE: &str = "inventory.state";
pub const STATE_EVERY: Duration = Duration::from_secs(1);
pub const STOPPED_AFTER: Duration = Duration::from_secs(5);

/// Net resource change of one mountain over one tick, sent by a simulation.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Batch {
    pub sender: String,
    pub epoch: u64,
    pub seq: u64,
    pub mountain: MountainId,
    pub add: Resources,
    pub take: Resources,
    /// Initial stock: applied once per mountain, ignored afterwards.
    pub seed: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct State {
    pub totals: BTreeMap<MountainId, Resources>,
    pub seeded: BTreeSet<MountainId>,
    /// Last applied `(epoch, seq)` per sender.
    pub acks: BTreeMap<String, (u64, u64)>,
}

impl State {
    /// `true` if the ack advanced (applied or seed-ignored), `false` for a stale or duplicate batch.
    pub fn apply(&mut self, b: &Batch) -> bool {
        let id = (b.epoch, b.seq);
        if self.acks.get(&b.sender).is_some_and(|&a| id <= a) {
            return false;
        }
        // A repeated seed still advances the ack but leaves totals alone.
        if !b.seed || self.seeded.insert(b.mountain) {
            let t = self.totals.entry(b.mountain).or_default();
            t.add(b.add);
            t.take(&b.take);
        }
        self.acks.insert(b.sender.clone(), id);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(stone: u32) -> Resources {
        Resources {
            stone,
            ..Default::default()
        }
    }

    fn batch(sender: &str, epoch: u64, seq: u64, add: u32, take: u32, seed: bool) -> Batch {
        Batch {
            sender: sender.into(),
            epoch,
            seq,
            mountain: MountainId(0),
            add: res(add),
            take: res(take),
            seed,
        }
    }

    fn stone(s: &State) -> u32 {
        s.totals[&MountainId(0)].stone
    }

    #[test]
    fn apply_adds_then_takes() {
        let mut s = State::default();
        assert!(s.apply(&batch("a", 1, 1, 5, 2, false)));
        assert_eq!(stone(&s), 3);
    }

    #[test]
    fn take_below_zero_clamps_to_zero() {
        let mut s = State::default();
        s.apply(&batch("a", 1, 1, 1, 0, false));
        s.apply(&batch("a", 1, 2, 0, 5, false));
        assert_eq!(stone(&s), 0);
    }

    #[test]
    fn duplicate_batch_is_ignored() {
        let mut s = State::default();
        let b = batch("a", 1, 1, 1, 0, false);
        assert!(s.apply(&b));
        assert!(!s.apply(&b));
        assert_eq!(stone(&s), 1);
    }

    #[test]
    fn older_seq_is_ignored() {
        let mut s = State::default();
        s.apply(&batch("a", 1, 5, 1, 0, false));
        assert!(!s.apply(&batch("a", 1, 3, 10, 0, false)));
        assert_eq!(stone(&s), 1);
    }

    #[test]
    fn newer_epoch_resets_numbering() {
        let mut s = State::default();
        s.apply(&batch("a", 1, 9, 1, 0, false));
        assert!(s.apply(&batch("a", 2, 1, 1, 0, false)));
        assert!(!s.apply(&batch("a", 1, 10, 1, 0, false)));
        assert_eq!(stone(&s), 2);
    }

    #[test]
    fn seed_applies_once_per_mountain() {
        let mut s = State::default();
        assert!(s.apply(&batch("a", 1, 1, 7, 0, true)));
        assert!(s.apply(&batch("b", 1, 1, 7, 0, true)));
        assert_eq!(stone(&s), 7);
        assert_eq!(s.acks["b"], (1, 1));
    }

    #[test]
    fn ordinary_batches_before_seed_do_not_block_it() {
        let mut s = State::default();
        s.apply(&batch("a", 1, 1, 2, 0, false));
        s.apply(&batch("a", 1, 2, 7, 0, true));
        assert_eq!(stone(&s), 9);
    }

    #[test]
    fn senders_are_independent() {
        let mut s = State::default();
        s.apply(&batch("a", 1, 5, 1, 0, false));
        assert!(s.apply(&batch("b", 1, 1, 1, 0, false)));
        assert_eq!(stone(&s), 2);
    }
}
