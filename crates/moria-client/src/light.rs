//! Client-side light model: what the hero has explored and how bright a tile is shown.
//! Bevy-free, like `game.rs`.

use crate::game::Session;
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

    /// Adds the tiles `o` has seen; ignored if the sizes differ.
    pub fn merge(&mut self, o: &Explored) {
        if (self.w, self.h, self.d) == (o.w, o.h, o.d) {
            for (a, b) in self.bits.iter_mut().zip(&o.bits) {
                *a |= b;
            }
        }
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

/// Memory file of a game in each candidate folder (one or two), primary first.
pub fn memory_files(
    dirs: &[PathBuf],
    addr: &str,
    name: &str,
    mountain: MountainId,
) -> Vec<PathBuf> {
    dirs.iter()
        .map(|d| memory_file(d, addr, name, mountain))
        .collect()
}

/// Merges the newest file that holds a valid map: the primary folder may have become read-only,
/// and a truncated newest file must not hide an older good one.
fn load(files: &[PathBuf], e: &mut Explored) {
    let mut by_age: Vec<_> = files
        .iter()
        .filter_map(|p| Some((std::fs::metadata(p).ok()?.modified().ok()?, p)))
        .collect();
    by_age.sort_by_key(|&(t, _)| std::cmp::Reverse(t));
    if let Some(old) = by_age
        .into_iter()
        .find_map(|(_, p)| Explored::from_bytes(e.w, e.h, e.d, &std::fs::read(p).ok()?))
    {
        e.merge(&old);
    }
}

/// Writes to the first file that accepts it, through `<file>.tmp` so a crash never truncates it.
/// True once one of `files` holds the map.
fn save(files: &[PathBuf], e: &Explored) -> bool {
    let bytes = e.to_bytes();
    for f in files {
        let mut tmp = f.as_os_str().to_owned();
        tmp.push(".tmp");
        let _ = f.parent().map(std::fs::create_dir_all);
        if std::fs::write(&tmp, &bytes).is_ok() && std::fs::rename(&tmp, f).is_ok() {
            return true;
        }
        let _ = std::fs::remove_file(&tmp);
    }
    false
}

/// Load/save policy of the explored memory, independent of Bevy. Best effort: I/O errors are ignored.
#[derive(Default)]
pub struct MemorySync {
    since: f32,
}

impl MemorySync {
    const EVERY: f32 = 30.0;

    /// One frame. Every game in `s.left` is saved to its own files (`exit` adds the running one);
    /// the running game is loaded once, then saved every 30 s. `files` names a mountain's files.
    pub fn step(
        &mut self,
        dt: f32,
        exit: bool,
        s: &mut Session,
        files: impl Fn(MountainId) -> Vec<PathBuf>,
    ) {
        if exit {
            s.left.extend(s.game.take());
        }
        for mut g in s.left.drain(..) {
            // A game dropped before its first load is merged with its file first.
            let f = g.memory.take().unwrap_or_else(|| {
                let f = files(g.mountain.id);
                load(&f, &mut g.explored);
                f
            });
            save(&f, &g.explored);
        }
        // A save request is answered on this frame whatever the state, so it never lingers.
        let now = std::mem::take(&mut s.save_now);
        let Some(g) = &mut s.game else {
            if now {
                s.saved = Some(false);
            }
            return;
        };
        let Some(f) = &g.memory else {
            let f = files(g.mountain.id);
            load(&f, &mut g.explored);
            if now {
                s.saved = Some(save(&f, &g.explored));
            }
            g.memory = Some(f);
            self.since = 0.0;
            return;
        };
        self.since += dt;
        if now || self.since >= Self::EVERY {
            self.since = 0.0;
            let ok = save(f, &g.explored);
            if now {
                s.saved = Some(ok);
            }
        }
    }
}

/// Brightness drawn for a tile: its light, or the dim memory if explored.
pub fn shown(light: f32, seen: bool) -> f32 {
    light.max(if seen { MEMORY } else { 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::tests::game;
    use crate::net::NetEvent;
    use moria_core::Resources;
    use moria_core::protocol::{FortressView, ServerMsg};

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

    fn p(x: i16) -> TilePos {
        TilePos::new(x, 1, 0)
    }

    /// A snapshot moves the session onto mountain `id`.
    fn on(s: &mut Session, id: u16) {
        let mut g = game();
        g.mountain.id = MountainId(id);
        s.handle(NetEvent::Msg(ServerMsg::Snapshot(Box::new(FortressView {
            mountain: g.mountain,
            hero: g.hero,
            dwarves: vec![],
            resources: Resources::default(),
            tick: 0,
        }))));
    }

    fn mark(s: &mut Session, x: i16) {
        s.game.as_mut().unwrap().explored.mark(p(x));
    }

    /// The map saved for mountain `id`.
    fn disk(d: &Path, id: u16) -> Explored {
        let (w, h, z) = game().mountain.size();
        let f = memory_file(d, "srv", "Gimli", MountainId(id));
        Explored::from_bytes(w, h, z, &std::fs::read(f).unwrap()).unwrap()
    }

    /// One candidate folder only.
    fn files(d: &Path) -> impl Fn(MountainId) -> Vec<PathBuf> + '_ {
        move |m| memory_files(&[d.to_path_buf()], "srv", "Gimli", m)
    }

    #[test]
    fn memory_works_with_one_folder() {
        let d = PathBuf::from("/x");
        assert_eq!(
            memory_files(std::slice::from_ref(&d), "srv", "Gimli", MountainId(1)),
            vec![memory_file(&d, "srv", "Gimli", MountainId(1))]
        );
        let d = tmp("one");
        let (mut s, mut sync) = (Session::default(), MemorySync::default());
        on(&mut s, 1);
        sync.step(0.0, false, &mut s, files(&d));
        mark(&mut s, 2);
        sync.step(0.0, true, &mut s, files(&d));
        assert!(disk(&d, 1).seen(p(2)));
    }

    #[test]
    fn memory_sync_loads_once_keeps_marks_saves_every_30s_and_on_end() {
        let d = tmp("sync");
        let (w, h, z) = game().mountain.size();
        let mut old = Explored::new(w, h, z);
        old.mark(p(0));
        let f1 = memory_file(&d, "srv", "Gimli", MountainId(1));
        std::fs::write(f1, old.to_bytes()).unwrap();
        let (mut s, mut sync) = (Session::default(), MemorySync::default());
        on(&mut s, 1);
        sync.step(1.0, false, &mut s, files(&d));
        assert!(s.game.as_ref().unwrap().explored.seen(p(0))); // (a) loaded
        mark(&mut s, 1);
        for _ in 0..5 {
            sync.step(1.0, false, &mut s, files(&d));
        }
        assert!(s.game.as_ref().unwrap().explored.seen(p(1))); // (b) not reloaded over
        assert!(!disk(&d, 1).seen(p(1))); // no save yet
        sync.step(30.0, false, &mut s, files(&d));
        assert!(disk(&d, 1).seen(p(1))); // (c) saved
        mark(&mut s, 2);
        s.handle(NetEvent::Disconnected(crate::net::Notice::Lost(
            "perdu".into(),
        )));
        sync.step(1.0, false, &mut s, files(&d));
        assert!(disk(&d, 1).seen(p(2))); // (d) saved on end
    }

    #[test]
    fn save_now_writes_the_running_map_at_once() {
        let d = tmp("now");
        let (mut s, mut sync) = (Session::default(), MemorySync::default());
        on(&mut s, 1);
        sync.step(0.0, false, &mut s, files(&d));
        mark(&mut s, 1);
        s.save_now = true;
        sync.step(0.0, false, &mut s, files(&d));
        assert!(disk(&d, 1).seen(p(1)));
        assert!(!s.save_now);
        assert_eq!(s.saved, Some(true));
    }

    #[test]
    fn save_now_before_the_map_loads_still_saves_and_reports_it() {
        let d = tmp("early");
        let (mut s, mut sync) = (Session::default(), MemorySync::default());
        on(&mut s, 1);
        mark(&mut s, 1);
        s.save_now = true;
        sync.step(0.0, false, &mut s, files(&d));
        assert!(disk(&d, 1).seen(p(1)));
        assert!(!s.save_now);
        assert_eq!(s.saved, Some(true));
    }

    #[test]
    fn save_now_reports_a_failed_write() {
        let d = tmp("fail");
        let not_a_dir = d.join("file");
        std::fs::write(&not_a_dir, b"").unwrap();
        let (mut s, mut sync) = (Session::default(), MemorySync::default());
        on(&mut s, 1);
        sync.step(0.0, false, &mut s, files(&not_a_dir));
        s.save_now = true;
        sync.step(0.0, false, &mut s, files(&not_a_dir));
        assert_eq!(s.saved, Some(false));
    }

    #[test]
    fn save_now_without_a_game_is_dropped() {
        let d = tmp("none");
        let (mut s, mut sync) = (Session::default(), MemorySync::default());
        s.save_now = true;
        sync.step(0.0, false, &mut s, files(&d));
        assert!(!s.save_now);
        assert_eq!(s.saved, Some(false));
    }

    #[test]
    fn two_mountain_switches_in_one_frame_save_each_map_to_its_own_file() {
        let d = tmp("switch");
        let (mut s, mut sync) = (Session::default(), MemorySync::default());
        on(&mut s, 1);
        sync.step(0.0, false, &mut s, files(&d));
        mark(&mut s, 1);
        on(&mut s, 2);
        mark(&mut s, 2);
        on(&mut s, 3);
        sync.step(0.0, false, &mut s, files(&d));
        assert!(disk(&d, 1).seen(p(1)) && !disk(&d, 1).seen(p(2)));
        assert!(disk(&d, 2).seen(p(2)) && !disk(&d, 2).seen(p(1)));
    }

    #[test]
    fn exit_saves_the_pending_and_the_running_game() {
        let d = tmp("exit");
        let (mut s, mut sync) = (Session::default(), MemorySync::default());
        on(&mut s, 1);
        sync.step(0.0, false, &mut s, files(&d));
        mark(&mut s, 1);
        on(&mut s, 2); // mountain 1 is now pending
        sync.step(0.0, false, &mut s, files(&d)); // mountain 2 loads
        mark(&mut s, 2);
        on(&mut s, 3); // mountain 2 pending, 3 running
        mark(&mut s, 3);
        sync.step(0.0, true, &mut s, files(&d));
        assert!(disk(&d, 2).seen(p(2)));
        assert!(disk(&d, 3).seen(p(3)));
        assert!(s.left.is_empty() && s.game.is_none());
    }

    #[test]
    fn same_mountain_resnapshot_keeps_memory_without_reload() {
        let d = tmp("same");
        let (mut s, mut sync) = (Session::default(), MemorySync::default());
        on(&mut s, 1);
        sync.step(0.0, false, &mut s, files(&d));
        mark(&mut s, 1);
        on(&mut s, 1);
        assert!(s.left.is_empty());
        assert!(s.game.as_ref().unwrap().memory.is_some());
        sync.step(0.0, true, &mut s, files(&d));
        assert!(disk(&d, 1).seen(p(1)));
    }

    #[test]
    fn memory_sync_prefers_the_newer_file() {
        let d = tmp("newer");
        let files = [d.join("a.bin"), d.join("b.bin")];
        let mut e = Explored::new(2, 2, 1);
        std::fs::write(&files[0], e.to_bytes()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        e.mark(TilePos::new(1, 0, 0));
        std::fs::write(&files[1], e.to_bytes()).unwrap();
        let mut g = Explored::new(2, 2, 1);
        load(&files, &mut g);
        assert!(g.seen(TilePos::new(1, 0, 0)));
    }

    #[test]
    fn load_keeps_marks_made_before_it() {
        let d = tmp("merge");
        let (mut s, mut sync) = (Session::default(), MemorySync::default());
        on(&mut s, 1);
        mark(&mut s, 0);
        sync.step(0.0, true, &mut s, files(&d)); // the file holds tile 0
        on(&mut s, 1);
        mark(&mut s, 1); // lit earlier this frame
        sync.step(0.0, false, &mut s, files(&d)); // first load
        let e = &s.game.as_ref().unwrap().explored;
        assert!(e.seen(p(0)) && e.seen(p(1)));
    }

    #[test]
    fn a_bad_newer_file_falls_back_to_the_older_one() {
        let d = tmp("bad");
        let files = [d.join("a.bin"), d.join("b.bin")];
        let mut e = Explored::new(16, 16, 1);
        e.mark(TilePos::new(1, 0, 0));
        save(&files[..1], &e);
        assert!(!d.join("a.bin.tmp").exists()); // the temp file was renamed
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&files[1], &e.to_bytes()[..3]).unwrap(); // newer but truncated
        let mut g = Explored::new(16, 16, 1);
        load(&files, &mut g);
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
    fn memory_is_dim_and_never_hides_light() {
        assert_eq!(shown(0.0, false), 0.0);
        assert_eq!(shown(0.0, true), MEMORY);
        assert_eq!(shown(1.0, true), 1.0);
    }
}
