use crate::{MountainId, Resources};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const EXCHANGE: &str = "moria";
pub const KEY_APPLY: &str = "inventory.apply";
pub const KEY_STATE: &str = "inventory.state";
pub const STATE_EVERY: Duration = Duration::from_secs(1);
pub const STOPPED_AFTER: Duration = Duration::from_secs(5);
pub const INVENTORY_VERSION: u32 = 1;

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

    /// Atomic write: temp file, `sync_all`, rename.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut bytes = INVENTORY_VERSION.to_le_bytes().to_vec();
        bytes.extend(postcard::to_allocvec(self).map_err(io::Error::other)?);
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        // Les lots sont acquittes apres save : sans fsync du dossier, un crash de l'hote peut
        // perdre le rename. Best effort : ouvrir un dossier echoue sur certaines plateformes.
        #[cfg(unix)]
        if let Some(dir) = path.parent() {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    }

    /// Missing file gives the empty state; anything unreadable is an error, never a silent reset.
    pub fn load_or_default(path: &Path) -> io::Result<State> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(State::default()),
            Err(e) => return Err(e),
        };
        let bad = |m: String| io::Error::new(io::ErrorKind::InvalidData, m);
        let Some((v, body)) = bytes.split_first_chunk::<4>() else {
            return Err(bad("inventaire tronqué".into()));
        };
        match u32::from_le_bytes(*v) {
            INVENTORY_VERSION => postcard::from_bytes(body).map_err(io::Error::other),
            n => Err(bad(format!("inventaire v{n} non supporté"))),
        }
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

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("moria-inv-{}-{name}", std::process::id()))
    }

    fn seeded_state() -> State {
        let mut s = State::default();
        s.apply(&batch("a", 1, 1, 5, 2, false));
        s
    }

    #[test]
    fn save_then_load_roundtrips() {
        let p = tmp("roundtrip");
        let s = seeded_state();
        s.save(&p).unwrap();
        assert_eq!(State::load_or_default(&p).unwrap(), s);
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn load_missing_file_is_default() {
        let p = tmp("missing");
        assert_eq!(State::load_or_default(&p).unwrap(), State::default());
    }

    #[test]
    fn load_rejects_garbage_and_truncated() {
        let p = tmp("garbage");
        std::fs::write(&p, [1, 2, 3]).unwrap();
        assert!(State::load_or_default(&p).is_err());
        seeded_state().save(&p).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        std::fs::write(&p, &bytes[..bytes.len() / 2]).unwrap();
        assert!(State::load_or_default(&p).is_err());
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn load_rejects_newer_version() {
        let p = tmp("newer");
        seeded_state().save(&p).unwrap();
        let mut bytes = std::fs::read(&p).unwrap();
        bytes[..4].copy_from_slice(&(INVENTORY_VERSION + 1).to_le_bytes());
        std::fs::write(&p, bytes).unwrap();
        assert!(State::load_or_default(&p).is_err());
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn redelivered_batch_not_applied_twice_after_reload() {
        let p = tmp("redeliver");
        let b = batch("a", 1, 1, 5, 0, false);
        let mut s = State::default();
        assert!(s.apply(&b));
        s.save(&p).unwrap();
        let mut s = State::load_or_default(&p).unwrap();
        assert!(!s.apply(&b));
        assert_eq!(stone(&s), 5);
        std::fs::remove_file(&p).unwrap();
    }
}
