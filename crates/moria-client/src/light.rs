//! Client-side light model: what the hero has explored and how bright a tile is shown.
//! Bevy-free, like `game.rs`.

use moria_core::{MountainId, TilePos};
use std::path::{Path, PathBuf};

/// A tile at or above this brightness becomes explored (moonlight alone does not explore).
pub const SEEN_AT: f32 = 0.25;
/// Shown brightness of an explored, unlit tile.
pub const MEMORY: f32 = 0.3;

/// One bit per tile: has the hero ever seen it lit.
pub struct Explored {
    w: i16,
    h: i16,
    d: u8,
    bits: Vec<u64>,
}

impl Explored {
    fn words(w: i16, h: i16, d: u8) -> usize {
        (w.max(0) as usize * h.max(0) as usize * d as usize).div_ceil(64)
    }

    pub fn new(w: i16, h: i16, d: u8) -> Self {
        Self {
            w,
            h,
            d,
            bits: vec![0; Self::words(w, h, d)],
        }
    }

    /// Bit index, or `None` out of bounds.
    fn index(&self, p: TilePos) -> Option<usize> {
        let ok = (0..self.w).contains(&p.x) && (0..self.h).contains(&p.y) && p.z < self.d;
        ok.then(|| p.x as usize + self.w as usize * (p.y as usize + self.h as usize * p.z as usize))
    }

    pub fn mark(&mut self, p: TilePos) {
        if let Some(i) = self.index(p) {
            self.bits[i / 64] |= 1 << (i % 64);
        }
    }

    pub fn seen(&self, p: TilePos) -> bool {
        self.index(p)
            .is_some_and(|i| self.bits[i / 64] >> (i % 64) & 1 == 1)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.bits.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// `None` unless `b` has exactly the size of a `w`×`h`×`d` map.
    pub fn from_bytes(w: i16, h: i16, d: u8, b: &[u8]) -> Option<Self> {
        if b.len() != Self::words(w, h, d) * 8 {
            return None;
        }
        let bits = b
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_le_bytes(*c))
            .collect();
        Some(Self { w, h, d, bits })
    }
}

/// Memory file for one server, account and mountain; anything but `[A-Za-z0-9_-]` becomes `_`.
pub fn memory_file(dir: &Path, addr: &str, name: &str, mountain: MountainId) -> PathBuf {
    let slug: String = format!("{addr}_{name}_{}", mountain.0)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("moria-souvenir-{slug}.bin"))
}

/// Load/save policy of the explored memory, independent of Bevy. Best effort: I/O errors are ignored.
#[derive(Default)]
pub struct MemorySync {
    /// (primary, fallback) file of the running game.
    files: Option<(PathBuf, PathBuf)>,
    since: f32,
}

impl MemorySync {
    const EVERY: f32 = 30.0;

    fn save(files: &(PathBuf, PathBuf), e: &Explored) {
        let bytes = e.to_bytes();
        if std::fs::write(&files.0, &bytes).is_err() {
            let _ = files.1.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(&files.1, &bytes);
        }
    }

    /// One frame. `left` is a game that just ended (saved to its files); `game` is the running one:
    /// loaded once when it appears (`files` names its files), then saved every 30 s.
    pub fn step(
        &mut self,
        dt: f32,
        left: Option<&Explored>,
        game: Option<&mut Explored>,
        files: impl FnOnce() -> Option<(PathBuf, PathBuf)>,
    ) {
        if let Some(e) = left
            && let Some(f) = self.files.take()
        {
            Self::save(&f, e);
        }
        let Some(game) = game else { return };
        let Some(f) = &self.files else {
            let Some(f) = files() else { return };
            // The most recently written file wins: the primary folder may have become read-only.
            let newest = [&f.0, &f.1]
                .into_iter()
                .filter_map(|p| Some((std::fs::metadata(p).ok()?.modified().ok()?, p)))
                .max_by_key(|(t, _)| *t);
            if let Some(e) = newest.and_then(|(_, p)| {
                Explored::from_bytes(game.w, game.h, game.d, &std::fs::read(p).ok()?)
            }) {
                *game = e;
            }
            self.files = Some(f);
            self.since = 0.0;
            return;
        };
        self.since += dt;
        if self.since >= Self::EVERY {
            self.since = 0.0;
            Self::save(f, game);
        }
    }
}

/// Brightness drawn for a tile: its light, or the dim memory if explored.
pub fn shown(light: f32, seen: bool) -> f32 {
    light.max(if seen { MEMORY } else { 0.0 })
}

/// A dwarf is drawn only where the light is brighter than a memory and enough to explore:
/// `MEMORY` (0.3) is above `SEEN_AT` (0.25), so a remembered dark tile never shows one.
pub fn dwarf_visible(light: f32) -> bool {
    light > MEMORY.max(SEEN_AT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use moria_core::light::MOONLIGHT;

    #[test]
    fn memory_file_is_per_server_account_and_mountain() {
        let d = Path::new("/x");
        let a = memory_file(d, "192.168.2.10:7777", "Gimli", MountainId(3));
        assert_eq!(
            a,
            Path::new("/x/moria-souvenir-192_168_2_10_7777_Gimli_3.bin")
        );
        assert_ne!(
            a,
            memory_file(d, "192.168.2.10:7777", "Gimli", MountainId(4))
        );
        assert!(
            !memory_file(d, "../../etc", "a/b", MountainId(0))
                .to_string_lossy()
                .contains("..")
        );
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moria-sync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn memory_sync_loads_once_keeps_marks_saves_every_30s_and_on_end() {
        let d = tmp("sync");
        let files = (d.join("a.bin"), d.join("b.bin"));
        let mut old = Explored::new(4, 4, 1);
        old.mark(TilePos::new(0, 0, 0));
        std::fs::write(&files.0, old.to_bytes()).unwrap();
        let mut sync = MemorySync::default();
        let mut e = Explored::new(4, 4, 1);
        let f = || Some(files.clone());
        sync.step(1.0, None, Some(&mut e), f);
        assert!(e.seen(TilePos::new(0, 0, 0))); // (a) loaded
        e.mark(TilePos::new(1, 1, 0));
        for _ in 0..5 {
            sync.step(1.0, None, Some(&mut e), f);
        }
        assert!(e.seen(TilePos::new(1, 1, 0))); // (b) not reloaded over
        let on_disk = |p: &PathBuf| Explored::from_bytes(4, 4, 1, &std::fs::read(p).unwrap());
        assert!(on_disk(&files.0).unwrap().seen(TilePos::new(0, 0, 0)));
        assert!(!on_disk(&files.0).unwrap().seen(TilePos::new(1, 1, 0))); // no save yet
        sync.step(30.0, None, Some(&mut e), f);
        assert!(on_disk(&files.0).unwrap().seen(TilePos::new(1, 1, 0))); // (c) saved
        e.mark(TilePos::new(2, 2, 0));
        sync.step(1.0, Some(&e), None, f);
        assert!(on_disk(&files.0).unwrap().seen(TilePos::new(2, 2, 0))); // (d) saved on end
    }

    #[test]
    fn memory_sync_prefers_the_newer_file() {
        let d = tmp("newer");
        let files = (d.join("a.bin"), d.join("b.bin"));
        let mut e = Explored::new(2, 2, 1);
        std::fs::write(&files.0, e.to_bytes()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        e.mark(TilePos::new(1, 0, 0));
        std::fs::write(&files.1, e.to_bytes()).unwrap();
        let mut g = Explored::new(2, 2, 1);
        MemorySync::default().step(0.0, None, Some(&mut g), || Some(files.clone()));
        assert!(g.seen(TilePos::new(1, 0, 0)));
    }

    #[test]
    fn explored_round_trips_and_marks() {
        let mut e = Explored::new(176, 96, 12);
        assert!(!e.seen(TilePos::new(3, 4, 5)));
        e.mark(TilePos::new(3, 4, 5));
        let back = Explored::from_bytes(176, 96, 12, &e.to_bytes()).unwrap();
        assert!(back.seen(TilePos::new(3, 4, 5)));
        assert!(!back.seen(TilePos::new(4, 4, 5)));
        assert!(!back.seen(TilePos::new(-1, 0, 0)));
    }

    #[test]
    fn explored_rejects_wrong_size_bytes() {
        let e = Explored::new(10, 10, 2);
        assert!(Explored::from_bytes(20, 10, 2, &e.to_bytes()).is_none());
        assert!(Explored::from_bytes(10, 10, 2, &[1, 2, 3]).is_none());
    }

    #[test]
    fn dwarf_is_visible_only_where_lit() {
        assert!(!dwarf_visible(shown(0.0, true))); // remembered but dark
        assert!(!dwarf_visible(MOONLIGHT));
        assert!(dwarf_visible(0.5)); // a dwarf's own fade edge still counts
    }

    #[test]
    fn memory_is_dim_and_never_hides_light() {
        assert_eq!(shown(0.0, false), 0.0);
        assert_eq!(shown(0.0, true), MEMORY);
        assert_eq!(shown(1.0, true), 1.0);
    }
}
