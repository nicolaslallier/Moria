//! Food simulation model: animals and fields with hidden state, kept in step with the server's grid.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::calendar::{Season, season};
use crate::light::DAY_TICKS;
use crate::{MountainId, TilePos};

/// Ticks between two grazing steps of the herd.
pub const ANIMAL_EVERY: u64 = 10;
/// Ticks between two growth checks; a field ripens with odds 1 in `RIPEN_ODDS` at each.
pub const GROW_EVERY: u64 = 100;
pub const RIPEN_ODDS: u64 = 60;
/// Growth odds in spring and autumn (slower than summer).
pub const SLOW_RIPEN_ODDS: u64 = 90;
/// An animal is adult after this many ticks.
pub const ADULT_TICKS: u64 = DAY_TICKS;
/// Ticks before a mother may breed again.
pub const BIRTH_COOLDOWN: u64 = 2 * DAY_TICKS;
/// A mated, fertile female breeds at an animal step with odds 1 in `BIRTH_ODDS`.
pub const BIRTH_ODDS: u64 = 20;
/// Mates must stand within this many tiles of each other.
pub const MATE_RANGE: i16 = 2;
/// Flock ceiling per species and mountain.
pub const HERD_CAP: usize = 24;

/// ponytail: a hash of a tile and a step number (splitmix64 finalizer) stands in for an RNG.
/// Deterministic and stateless; a seeded RNG if it ever shows patterns.
fn mix(q: TilePos, n: u64) -> u64 {
    let mut h = ((q.x as u64) << 40 ^ (q.y as u64) << 20 ^ n).wrapping_add(0x9e37_79b9_7f4a_7c15);
    h = (h ^ h >> 30).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h = (h ^ h >> 27).wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ h >> 31
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Cow,
    Field,
    Crop,
    // Appended: the postcard index of the variants above must not move.
    Sheep,
    Chicken,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Species {
    Cow,
    Sheep,
    Chicken,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sex {
    Male,
    Female,
}

impl CellKind {
    fn species(self) -> Option<Species> {
        match self {
            CellKind::Cow => Some(Species::Cow),
            CellKind::Sheep => Some(Species::Sheep),
            CellKind::Chicken => Some(Species::Chicken),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub pos: TilePos,
    pub kind: CellKind,
}

/// What the simulation proposes to the server.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoodEvent {
    AnimalMoved { from: TilePos, to: TilePos },
    AnimalBorn { species: Species, at: TilePos },
    CropRipe { at: TilePos },
}

/// What the server tells the simulation happened.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoodAction {
    AnimalRemoved(TilePos),
    FieldSown(TilePos),
    FieldHarvested(TilePos),
    FieldRemoved(TilePos),
    /// A dwarf penned the animal: it is owned from now on, and only owned animals breed.
    /// Appended: the postcard index of the variants above must not move.
    AnimalCaptured(TilePos),
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
struct AnimalState {
    species: Species,
    sex: Sex,
    captured: bool,
    born: u64,
    fertile_at: u64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
struct FieldState {
    ripe: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FoodSim {
    seed: u64,
    adopted_at: u64,
    /// Captures told before the animal was adopted; consumed by the next `adopt`.
    pending_captures: BTreeSet<TilePos>,
    animals: BTreeMap<TilePos, AnimalState>,
    fields: BTreeMap<TilePos, FieldState>,
}

impl FoodSim {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            adopted_at: 0,
            pending_captures: BTreeSet::new(),
            animals: BTreeMap::new(),
            fields: BTreeMap::new(),
        }
    }

    /// Rebuilds the maps from the server's snapshot: known entities keep their hidden state,
    /// unknown ones get a default, those not listed are forgotten. Emits nothing.
    pub fn adopt(&mut self, cells: &[Cell], tick: u64) {
        let (old_animals, old_fields) = (
            std::mem::take(&mut self.animals),
            std::mem::take(&mut self.fields),
        );
        for c in cells {
            match c.kind {
                // Default animal: a wild adult, free to breed once penned.
                CellKind::Cow | CellKind::Sheep | CellKind::Chicken => {
                    let species = c.kind.species().unwrap();
                    let known = old_animals.get(&c.pos).filter(|a| a.species == species);
                    let st = known.copied().unwrap_or(AnimalState {
                        species,
                        sex: if mix(c.pos, self.seed).is_multiple_of(2) {
                            Sex::Male
                        } else {
                            Sex::Female
                        },
                        captured: self.pending_captures.contains(&c.pos),
                        born: tick.saturating_sub(ADULT_TICKS),
                        fertile_at: tick,
                    });
                    self.animals.insert(c.pos, st);
                }
                CellKind::Field | CellKind::Crop => {
                    let ripe = c.kind == CellKind::Crop;
                    let mut st = old_fields
                        .get(&c.pos)
                        .copied()
                        .unwrap_or(FieldState { ripe });
                    st.ripe = ripe; // the server's grid wins (a lost harvest is cleared here)
                    self.fields.insert(c.pos, st);
                }
            }
        }
        self.pending_captures.clear();
        self.adopted_at = tick;
    }

    /// Stopgap until the server tells which animals are penned: every adopted animal counts as
    /// penned, so herds breed as they did before species. Remove with the pen wiring.
    pub fn pen_all(&mut self) {
        self.animals.values_mut().for_each(|a| a.captured = true);
    }

    /// Idempotent; the server's view wins.
    pub fn apply_action(&mut self, a: &FoodAction) {
        match *a {
            FoodAction::AnimalRemoved(p) => {
                self.animals.remove(&p);
            }
            FoodAction::AnimalCaptured(p) => match self.animals.get_mut(&p) {
                Some(a) => a.captured = true,
                None => {
                    self.pending_captures.insert(p);
                }
            },
            FoodAction::FieldSown(p) | FoodAction::FieldHarvested(p) => {
                self.fields.insert(p, FieldState { ripe: false });
            }
            FoodAction::FieldRemoved(p) => {
                self.fields.remove(&p);
            }
        }
    }

    pub fn animal_count(&self) -> usize {
        self.animals.len()
    }

    pub fn field_count(&self) -> usize {
        self.fields.len()
    }

    pub fn is_ripe(&self, p: TilePos) -> bool {
        self.fields.get(&p).is_some_and(|f| f.ripe)
    }

    pub fn is_captured(&self, p: TilePos) -> bool {
        self.animals.get(&p).is_some_and(|a| a.captured)
    }

    pub fn has_animal(&self, p: TilePos) -> bool {
        self.animals.contains_key(&p)
    }

    /// Advances the simulation to `tick` and returns what it proposes. The sim's own state
    /// already assumes the proposals hold; `reject` undoes the refused ones.
    pub fn step(&mut self, tick: u64) -> Vec<FoodEvent> {
        let (mut out, season) = (vec![], season(tick));
        let animal_every = if season == Season::Winter {
            2 * ANIMAL_EVERY
        } else {
            ANIMAL_EVERY
        };
        if tick.is_multiple_of(animal_every) {
            self.step_animals(tick, season, &mut out);
        }
        if tick.is_multiple_of(GROW_EVERY) {
            self.step_fields(tick, season, &mut out);
        }
        out
    }

    /// Blind proposals: the sim has no map, the server refuses what does not fit.
    /// A penned adult female with a penned adult male of her species within `MATE_RANGE`
    /// may give birth; everyone else just wanders.
    fn step_animals(&mut self, tick: u64, season: Season, out: &mut Vec<FoodEvent>) {
        let breeds = matches!(season, Season::Spring | Season::Summer);
        let keys: Vec<TilePos> = self.animals.keys().copied().collect();
        for q in keys {
            let h = mix(q, (tick / ANIMAL_EVERY) ^ self.seed);
            let Some(&st) = self.animals.get(&q) else {
                continue;
            };
            let near = |k: u64| {
                let (dx, dy) = [(1, 0), (-1, 0), (0, 1), (0, -1)][(k % 4) as usize];
                TilePos::new(q.x.saturating_add(dx), q.y.saturating_add(dy), q.z)
            };
            let adult = |a: &AnimalState| a.captured && tick >= a.born.saturating_add(ADULT_TICKS);
            let herd = self
                .animals
                .values()
                .filter(|a| a.species == st.species)
                .count();
            if breeds
                && st.sex == Sex::Female
                && adult(&st)
                && tick >= st.fertile_at
                && (h >> 8).is_multiple_of(BIRTH_ODDS)
                && herd < HERD_CAP
                && self.animals.iter().any(|(m, a)| {
                    a.species == st.species
                        && a.sex == Sex::Male
                        && adult(a)
                        && m.z == q.z
                        && (m.x - q.x).abs().max((m.y - q.y).abs()) <= MATE_RANGE
                })
            {
                let at = near(h >> 16);
                if let std::collections::btree_map::Entry::Vacant(e) = self.animals.entry(at) {
                    e.insert(AnimalState {
                        species: st.species,
                        sex: if (h >> 32) & 1 == 0 {
                            Sex::Male
                        } else {
                            Sex::Female
                        },
                        captured: true,
                        born: tick,
                        fertile_at: tick.saturating_add(ADULT_TICKS),
                    });
                    self.animals.get_mut(&q).unwrap().fertile_at =
                        tick.saturating_add(BIRTH_COOLDOWN);
                    out.push(FoodEvent::AnimalBorn {
                        species: st.species,
                        at,
                    });
                }
                continue;
            }
            let to = near(h);
            if !self.animals.contains_key(&to) {
                let st = self.animals.remove(&q).unwrap();
                self.animals.insert(to, st);
                out.push(FoodEvent::AnimalMoved { from: q, to });
            }
        }
    }

    /// A ripe crop stays ripe and asks for its harvest again at every check; winter freezes growth.
    fn step_fields(&mut self, tick: u64, season: Season, out: &mut Vec<FoodEvent>) {
        let odds = match season {
            Season::Summer => RIPEN_ODDS,
            Season::Winter => 0,
            _ => SLOW_RIPEN_ODDS,
        };
        for (&q, f) in self.fields.iter_mut() {
            if !f.ripe && odds != 0 && mix(q, (tick / GROW_EVERY) ^ self.seed).is_multiple_of(odds)
            {
                f.ripe = true;
            }
            if f.ripe {
                out.push(FoodEvent::CropRipe { at: q });
            }
        }
    }

    /// The server refused `evs` (in proposal order): undo their optimistic effects, last
    /// first, so a cow that moved into a tile another one left finds that tile free again.
    pub fn reject(&mut self, evs: &[FoodEvent]) {
        evs.iter().rev().for_each(|ev| self.reject_one(ev));
    }

    fn reject_one(&mut self, ev: &FoodEvent) {
        match *ev {
            FoodEvent::AnimalMoved { from, to } => {
                if let Some(st) = self.animals.remove(&to) {
                    // origin taken meanwhile: the animal is forgotten, the snapshot will tell
                    self.animals.entry(from).or_insert(st);
                }
            }
            FoodEvent::AnimalBorn { species, at } => {
                // The parent stood next to the calf and got its cooldown on the same tick
                // (it does not move on a birth step): fertile again from that tick.
                // ponytail: a parent that moved away before the reject keeps its cooldown.
                let Some(calf) = self.animals.remove(&at) else {
                    return;
                };
                let t0 = calf.born;
                let cooled = t0.saturating_add(BIRTH_COOLDOWN);
                let parent = [(1, 0), (-1, 0), (0, 1), (0, -1)].map(|(dx, dy)| {
                    TilePos::new(at.x.saturating_add(dx), at.y.saturating_add(dy), at.z)
                });
                if let Some(q) = parent.into_iter().find(|q| {
                    self.animals
                        .get(q)
                        .is_some_and(|a| a.species == species && a.fertile_at == cooled)
                }) {
                    self.animals.get_mut(&q).unwrap().fertile_at = t0;
                }
            }
            FoodEvent::CropRipe { at } => {
                if let Some(f) = self.fields.get_mut(&at) {
                    f.ripe = false;
                }
            }
        }
    }

    /// Tick of the last `adopt`, 0 before the first.
    pub fn adopted_at(&self) -> u64 {
        self.adopted_at
    }
}

/// On-disk format version of `food.bin`.
/// 2: animals carry a species, a sex and a captured flag (1 was cows only; loads as empty).
const FOOD_VERSION: u32 = 2;

/// Hidden food state of every mountain, persisted as `food.bin`.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct FoodStore {
    pub mountains: BTreeMap<MountainId, FoodSim>,
}

impl FoodStore {
    /// Atomic write: temp file, `sync_all`, rename, parent-directory fsync.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut bytes = FOOD_VERSION.to_le_bytes().to_vec();
        bytes.extend(postcard::to_allocvec(self).map_err(io::Error::other)?);
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        // Sans fsync du dossier, un crash de l'hote peut perdre le rename. Best effort.
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

    /// Missing file gives the empty store; anything unreadable is an error, never a silent reset.
    pub fn load_or_default(path: &Path) -> io::Result<FoodStore> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(FoodStore::default()),
            Err(e) => return Err(e),
        };
        let bad = |m: String| io::Error::new(io::ErrorKind::InvalidData, m);
        let Some((v, body)) = bytes.split_first_chunk::<4>() else {
            return Err(bad("nourriture tronquée".into()));
        };
        match u32::from_le_bytes(*v) {
            FOOD_VERSION => postcard::from_bytes(body).map_err(io::Error::other),
            // v1 (cows only) was deployed; its hidden state is rebuilt from the World.
            1 => Ok(FoodStore::default()),
            n => Err(bad(format!("nourriture v{n} non supportée"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i16, y: i16) -> TilePos {
        TilePos { x, y, z: 0 }
    }
    fn cell(pos: TilePos, kind: CellKind) -> Cell {
        Cell { pos, kind }
    }

    #[test]
    fn forged_ticks_and_edge_coordinates_never_panic() {
        // A forged World/Tick: cows at the i16 corners, ticks up to u64::MAX.
        let (lo, hi) = (i16::MIN, i16::MAX);
        let corners = [(lo, lo), (lo, hi), (hi, lo), (hi, hi), (hi, 0), (0, lo)];
        let mut cells: Vec<Cell> = corners
            .iter()
            .map(|&(x, y)| cell(TilePos::new(x, y, u8::MAX), CellKind::Cow))
            .collect();
        cells.push(cell(p(hi, hi), CellKind::Field));
        let mut s = FoodSim::new(u64::MAX);
        s.adopt(&cells, u64::MAX);
        s.adopt(&cells, u64::MAX - 3 * DAY_TICKS);
        for t in u64::MAX - 3 * DAY_TICKS..=u64::MAX {
            for ev in s.step(t) {
                if t % 3 == 0 {
                    s.reject(&[ev]);
                }
            }
        }
        // Spring and summer (births) at huge ticks too.
        use crate::calendar::{SEASON_TICKS, YEAR_TICKS};
        let summer = u64::MAX - u64::MAX % YEAR_TICKS - YEAR_TICKS + SEASON_TICKS;
        s.adopt(&cells, summer);
        for t in (summer..summer + 2 * DAY_TICKS).step_by(ANIMAL_EVERY as usize) {
            s.step(t);
        }
    }

    #[test]
    fn adopt_creates_default_state_for_unknown_cells() {
        let mut s = FoodSim::new(1);
        s.adopt(
            &[
                cell(p(1, 1), CellKind::Cow),
                cell(p(2, 2), CellKind::Field),
                cell(p(3, 3), CellKind::Crop),
            ],
            50,
        );
        assert_eq!(s.animal_count(), 1);
        assert_eq!(s.field_count(), 2);
        assert!(s.has_animal(p(1, 1)));
        assert!(!s.is_ripe(p(2, 2)));
        assert!(s.is_ripe(p(3, 3)));
        // adult animal: born long before the snapshot, may breed at once
        let c = s.animals[&p(1, 1)];
        assert!(c.born < 50 && c.fertile_at <= 50);
    }

    #[test]
    fn adopt_keeps_hidden_state_of_known_entities() {
        let mut s = FoodSim::new(1);
        s.adopt(
            &[cell(p(1, 1), CellKind::Cow), cell(p(2, 2), CellKind::Field)],
            10,
        );
        s.animals.get_mut(&p(1, 1)).unwrap().fertile_at = 999;
        s.adopt(
            &[cell(p(1, 1), CellKind::Cow), cell(p(2, 2), CellKind::Field)],
            20,
        );
        assert_eq!(s.animals[&p(1, 1)].fertile_at, 999);
    }

    #[test]
    fn adopt_takes_ripeness_from_the_server() {
        let mut s = FoodSim::new(1);
        s.adopt(&[cell(p(2, 2), CellKind::Crop)], 10);
        // a lost FieldHarvested: the grid says Field, the server wins
        s.adopt(&[cell(p(2, 2), CellKind::Field)], 20);
        assert!(!s.is_ripe(p(2, 2)));
        s.adopt(&[cell(p(2, 2), CellKind::Crop)], 30);
        assert!(s.is_ripe(p(2, 2)));
    }

    #[test]
    fn adopt_forgets_entities_not_on_the_map() {
        let mut s = FoodSim::new(1);
        s.adopt(
            &[cell(p(1, 1), CellKind::Cow), cell(p(2, 2), CellKind::Field)],
            10,
        );
        s.adopt(&[], 20);
        assert_eq!((s.animal_count(), s.field_count()), (0, 0));
    }

    #[test]
    fn empty_food_state_proposes_nothing() {
        // a fresh sim holds nothing and the API has no way to delete from the map
        let s = FoodSim::new(1);
        assert_eq!((s.animal_count(), s.field_count()), (0, 0));
    }

    #[test]
    fn snapshot_adoption_never_emits_events() {
        // `adopt` returns unit: the type itself forbids events
        let mut s = FoodSim::new(1);
        let () = s.adopt(&[cell(p(1, 1), CellKind::Cow)], 10);
    }

    #[test]
    fn actions_are_idempotent() {
        let mut s = FoodSim::new(1);
        s.adopt(&[cell(p(1, 1), CellKind::Cow)], 10);
        s.apply_action(&FoodAction::AnimalRemoved(p(9, 9)));
        assert_eq!(s.animal_count(), 1);
        s.apply_action(&FoodAction::AnimalRemoved(p(1, 1)));
        let after = s.clone();
        s.apply_action(&FoodAction::AnimalRemoved(p(1, 1)));
        assert_eq!(s, after);
        assert_eq!(s.animal_count(), 0);
    }

    #[test]
    fn field_sown_adds_a_growing_field() {
        let mut s = FoodSim::new(1);
        s.apply_action(&FoodAction::FieldSown(p(4, 4)));
        assert_eq!(s.field_count(), 1);
        assert!(!s.is_ripe(p(4, 4)));
    }

    #[test]
    fn field_harvested_resets_to_growing() {
        let mut s = FoodSim::new(1);
        s.adopt(&[cell(p(3, 3), CellKind::Crop)], 10);
        s.apply_action(&FoodAction::FieldHarvested(p(3, 3)));
        assert_eq!(s.field_count(), 1);
        assert!(!s.is_ripe(p(3, 3)));
    }

    #[test]
    fn field_removed_forgets_it() {
        let mut s = FoodSim::new(1);
        s.apply_action(&FoodAction::FieldSown(p(4, 4)));
        s.apply_action(&FoodAction::FieldRemoved(p(4, 4)));
        s.apply_action(&FoodAction::FieldRemoved(p(4, 4)));
        assert_eq!(s.field_count(), 0);
    }

    #[test]
    fn adopted_at_records_the_snapshot_tick() {
        let mut s = FoodSim::new(1);
        assert_eq!(s.adopted_at(), 0);
        s.adopt(&[], 42);
        assert_eq!(s.adopted_at(), 42);
    }

    const SUMMER: u64 = crate::calendar::SEASON_TICKS;
    const AUTUMN: u64 = 2 * crate::calendar::SEASON_TICKS;
    const WINTER: u64 = 3 * crate::calendar::SEASON_TICKS;
    const YEAR: u64 = crate::calendar::YEAR_TICKS;

    /// Pens every animal and alternates the sexes, so neighbours form male/female pairs.
    fn tame(s: &mut FoodSim) {
        for (i, a) in s.animals.values_mut().enumerate() {
            a.captured = true;
            a.sex = if i % 2 == 0 { Sex::Male } else { Sex::Female };
        }
    }
    fn tamed(cells: &[Cell], tick: u64) -> FoodSim {
        let mut s = FoodSim::new(1);
        s.adopt(cells, tick);
        tame(&mut s);
        s
    }

    fn herd(n: i16) -> Vec<Cell> {
        (0..n).map(|i| cell(p(i * 2, 0), CellKind::Cow)).collect()
    }

    fn count(s: &mut FoodSim, from: u64, to: u64, pick: fn(&FoodEvent) -> bool) -> usize {
        (from..to).flat_map(|t| s.step(t)).filter(pick).count()
    }
    fn is_move(e: &FoodEvent) -> bool {
        matches!(e, FoodEvent::AnimalMoved { .. })
    }
    fn is_birth(e: &FoodEvent) -> bool {
        matches!(e, FoodEvent::AnimalBorn { .. })
    }

    #[test]
    fn animals_step_only_on_animal_ticks() {
        let mut s = FoodSim::new(1);
        s.adopt(&herd(5), 0);
        for t in SUMMER..SUMMER + 200 {
            if t % ANIMAL_EVERY != 0 {
                assert!(s.step(t).is_empty());
            }
        }
    }

    #[test]
    fn an_animal_proposes_a_horizontal_neighbour() {
        let mut s = FoodSim::new(1);
        s.adopt(&[cell(p(5, 5), CellKind::Cow)], 0);
        let mut seen = 0;
        for t in SUMMER..SUMMER + 2000 {
            for ev in s.step(t) {
                if let FoodEvent::AnimalMoved { from, to } = ev {
                    seen += 1;
                    assert_eq!(from.z, to.z);
                    assert_eq!((from.x - to.x).abs() + (from.y - to.y).abs(), 1);
                }
            }
        }
        assert!(seen > 0);
    }

    #[test]
    fn winter_animals_step_half_as_often() {
        let (mut a, mut b) = (FoodSim::new(1), FoodSim::new(1));
        a.adopt(&herd(10), 0);
        b.adopt(&herd(10), 0);
        let summer = count(&mut a, AUTUMN, AUTUMN + 20_000, is_move); // no births in autumn
        let winter = count(&mut b, WINTER, WINTER + 20_000, is_move);
        assert!(
            winter * 10 < summer * 7 && winter * 10 > summer * 3,
            "{winter} vs {summer}"
        );
    }

    #[test]
    fn no_birth_in_winter() {
        let mut s = tamed(&herd(10), WINTER);
        assert_eq!(count(&mut s, WINTER, WINTER + 20_000, is_birth), 0);
        let mut s = tamed(&herd(10), AUTUMN);
        assert_eq!(
            count(&mut s, AUTUMN, AUTUMN + 20_000, is_birth),
            0,
            "nor in autumn"
        );
        let mut s = tamed(&herd(10), SUMMER);
        assert!(
            count(&mut s, SUMMER, SUMMER + 20_000, is_birth) > 0,
            "but in summer"
        );
    }

    /// A penned male at (6,5) and female at (5,5), side by side.
    fn pair(kind: CellKind, tick: u64) -> FoodSim {
        let mut s = FoodSim::new(3);
        s.adopt(&[cell(p(6, 5), kind), cell(p(5, 5), kind)], tick);
        tame(&mut s); // (5,5) sorts first: male, (6,5): female
        s
    }
    fn first_birth(s: &mut FoodSim, from: u64) -> Option<(u64, FoodEvent)> {
        (from..from + 40_000).find_map(|t| s.step(t).into_iter().find(is_birth_ev).map(|e| (t, e)))
    }
    fn is_birth_ev(e: &FoodEvent) -> bool {
        is_birth(e)
    }

    #[test]
    fn pen_all_lets_an_adopted_herd_breed() {
        let mut s = FoodSim::new(1);
        s.adopt(&herd(12), SUMMER);
        assert_eq!(
            count(&mut s, SUMMER, SUMMER + 40_000, is_birth),
            0,
            "wild herd"
        );
        let mut s = FoodSim::new(1);
        s.adopt(&herd(12), SUMMER);
        s.pen_all();
        assert!(
            count(&mut s, SUMMER, SUMMER + 40_000, is_birth) > 0,
            "penned herd"
        );
    }

    #[test]
    fn births_need_an_adult_and_a_cooldown() {
        // a calf mother: no birth during her first day
        let mut s = pair(CellKind::Cow, SUMMER);
        for a in s.animals.values_mut() {
            a.born = SUMMER;
        }
        assert_eq!(count(&mut s, SUMMER, SUMMER + ADULT_TICKS - 1, is_birth), 0);
        // a mother that just gave birth waits for her cooldown
        let mut s = pair(CellKind::Cow, SUMMER);
        for a in s.animals.values_mut() {
            a.fertile_at = SUMMER + 100_000;
        }
        assert_eq!(count(&mut s, SUMMER, SUMMER + 100_000, is_birth), 0);
        // cooldown is set on birth
        let mut s = pair(CellKind::Cow, SUMMER);
        let (t0, _) = first_birth(&mut s, SUMMER).expect("a penned pair breeds in summer");
        let mother = s.animals[&p(6, 5)];
        assert_eq!(mother.fertile_at, t0 + BIRTH_COOLDOWN);
        let calf = s.animals.values().find(|c| c.born == t0).unwrap();
        assert!(calf.fertile_at >= t0 + ADULT_TICKS && calf.captured);
    }

    #[test]
    fn breeding_needs_a_penned_male_and_a_penned_female() {
        let breeds = |s: &mut FoodSim| count(s, SUMMER, SUMMER + 40_000, is_birth) > 0;
        assert!(breeds(&mut pair(CellKind::Cow, SUMMER)));
        // wild pair: nothing
        let mut s = pair(CellKind::Cow, SUMMER);
        s.animals.values_mut().for_each(|a| a.captured = false);
        assert!(!breeds(&mut s), "wild animals do not breed");
        // only the female is penned
        let mut s = pair(CellKind::Cow, SUMMER);
        s.animals.get_mut(&p(5, 5)).unwrap().captured = false;
        assert!(!breeds(&mut s), "the male is wild");
        // two females, two males
        for sex in [Sex::Female, Sex::Male] {
            let mut s = pair(CellKind::Cow, SUMMER);
            s.animals.values_mut().for_each(|a| a.sex = sex);
            assert!(!breeds(&mut s), "{sex:?} and {sex:?}");
        }
        // too far apart
        let mut s = FoodSim::new(3);
        s.adopt(
            &[cell(p(5, 5), CellKind::Cow), cell(p(25, 5), CellKind::Cow)],
            SUMMER,
        );
        tame(&mut s);
        assert!(!breeds(&mut s), "mates far apart");
    }

    #[test]
    fn the_male_is_an_adult_too() {
        let window = |s: &mut FoodSim| count(s, SUMMER, SUMMER + 1_000, is_birth);
        assert!(
            window(&mut pair(CellKind::Cow, SUMMER)) > 0,
            "control: grown pair"
        );
        let mut s = pair(CellKind::Cow, SUMMER);
        s.animals.get_mut(&p(5, 5)).unwrap().born = SUMMER; // a male calf
        assert_eq!(window(&mut s), 0);
    }

    #[test]
    fn every_species_breeds_true_to_its_kind() {
        for (kind, sp) in [
            (CellKind::Cow, Species::Cow),
            (CellKind::Sheep, Species::Sheep),
            (CellKind::Chicken, Species::Chicken),
        ] {
            let mut s = pair(kind, SUMMER);
            let (_, ev) = first_birth(&mut s, SUMMER).expect("breeds");
            let FoodEvent::AnimalBorn { species, at } = ev else {
                unreachable!()
            };
            assert_eq!(species, sp);
            assert_eq!(s.animals[&at].species, sp);
        }
    }

    #[test]
    fn species_do_not_cross() {
        let mut s = FoodSim::new(3);
        s.adopt(
            &[cell(p(5, 5), CellKind::Cow), cell(p(6, 5), CellKind::Sheep)],
            SUMMER,
        );
        tame(&mut s);
        assert_eq!(count(&mut s, SUMMER, SUMMER + 40_000, is_birth), 0);
    }

    #[test]
    fn newborns_are_both_sexes() {
        let mut s = tamed(&herd(10), SUMMER);
        for t in SUMMER..SUMMER + 40_000 {
            s.step(t);
        }
        let (m, f) = s.animals.values().fold((0, 0), |(m, f), a| match a.sex {
            Sex::Male => (m + 1, f),
            Sex::Female => (m, f + 1),
        });
        assert!(m > 5 && f > 5, "{m} males, {f} females");
    }

    #[test]
    fn a_species_has_its_own_cap() {
        let cells: Vec<Cell> = (0..HERD_CAP as i16)
            .map(|i| cell(p(i * 2, 0), CellKind::Sheep))
            .chain([cell(p(0, 10), CellKind::Cow), cell(p(1, 10), CellKind::Cow)])
            .collect();
        let (mut cow_births, mut sheep_births) = (0, 0);
        // short runs over several seeds: free mates wander apart, so one run proves little
        for seed in 0..8 {
            let mut s = FoodSim::new(seed);
            s.adopt(&cells, SUMMER);
            tame(&mut s);
            for t in SUMMER..SUMMER + 600 {
                for ev in s.step(t) {
                    match ev {
                        FoodEvent::AnimalBorn {
                            species: Species::Cow,
                            ..
                        } => cow_births += 1,
                        FoodEvent::AnimalBorn {
                            species: Species::Sheep,
                            ..
                        } => sheep_births += 1,
                        _ => {}
                    }
                }
            }
        }
        assert_eq!(sheep_births, 0, "a full flock stays full");
        assert!(cow_births > 0, "the cows still breed");
    }

    #[test]
    fn capture_pens_an_animal_and_is_idempotent() {
        let mut s = FoodSim::new(1);
        s.adopt(&[cell(p(1, 1), CellKind::Chicken)], 10);
        assert!(!s.is_captured(p(1, 1)));
        s.apply_action(&FoodAction::AnimalCaptured(p(1, 1)));
        s.apply_action(&FoodAction::AnimalCaptured(p(1, 1)));
        s.apply_action(&FoodAction::AnimalCaptured(p(9, 9))); // nobody there
        assert!(s.is_captured(p(1, 1)) && !s.is_captured(p(9, 9)));
        // a snapshot keeps the ownership
        s.adopt(&[cell(p(1, 1), CellKind::Chicken)], 20);
        assert!(s.is_captured(p(1, 1)));
    }

    #[test]
    fn a_capture_that_arrives_before_the_snapshot_is_not_lost() {
        let mut s = FoodSim::new(1);
        s.apply_action(&FoodAction::AnimalCaptured(p(1, 1)));
        s.adopt(&[cell(p(1, 1), CellKind::Cow)], 10);
        assert!(s.is_captured(p(1, 1)));
        // ...but only once: a later animal on that tile is wild
        s.apply_action(&FoodAction::AnimalRemoved(p(1, 1)));
        s.adopt(&[cell(p(1, 1), CellKind::Cow)], 20);
        assert!(!s.is_captured(p(1, 1)));
    }

    #[test]
    fn another_species_on_the_same_tile_starts_afresh() {
        let mut s = FoodSim::new(1);
        s.adopt(&[cell(p(1, 1), CellKind::Cow)], 10);
        s.apply_action(&FoodAction::AnimalCaptured(p(1, 1)));
        s.adopt(&[cell(p(1, 1), CellKind::Sheep)], 20);
        assert_eq!(s.animals[&p(1, 1)].species, Species::Sheep);
        assert!(!s.is_captured(p(1, 1)), "a new sheep is not the old cow");
    }

    #[test]
    fn adopted_animals_get_both_sexes_and_stay_wild() {
        let mut s = FoodSim::new(1);
        s.adopt(&herd(20), 0);
        let males = s.animals.values().filter(|a| a.sex == Sex::Male).count();
        assert!((4..=16).contains(&males), "{males} males of 20");
        assert!(s.animals.values().all(|a| !a.captured));
    }

    #[test]
    fn birth_proposal_is_next_to_a_parent() {
        let mut s = tamed(&herd(4), SUMMER);
        let mut seen = 0;
        for t in SUMMER..SUMMER + 20_000 {
            for ev in s.step(t) {
                if let FoodEvent::AnimalBorn { at, .. } = ev {
                    seen += 1;
                    assert_eq!(at.z, 0);
                    assert!(s.has_animal(at));
                    // someone is next to the newborn (no animal moved far since: moves are 1 tile)
                    let near = s.animals.keys().any(|&c| {
                        c != at && c.z == at.z && (c.x - at.x).abs() + (c.y - at.y).abs() <= 2
                    });
                    assert!(near);
                }
            }
        }
        assert!(seen > 0);
    }

    #[test]
    fn herd_never_exceeds_cap_over_two_years() {
        for refuse_half in [false, true] {
            let mut s = tamed(&herd(10), 0);
            let mut n = 0u32;
            for t in 0..2 * YEAR {
                for ev in s.step(t) {
                    n += 1;
                    if refuse_half && n.is_multiple_of(2) {
                        s.reject(&[ev]);
                    }
                    assert!(s.animal_count() <= HERD_CAP);
                }
                assert!(s.animal_count() <= HERD_CAP);
            }
        }
    }

    fn sown(n: i16) -> FoodSim {
        let mut s = FoodSim::new(1);
        for i in 0..n {
            s.apply_action(&FoodAction::FieldSown(p(i % 10, i / 10)));
        }
        s
    }
    fn ripe_count(s: &FoodSim) -> usize {
        s.fields.values().filter(|f| f.ripe).count()
    }
    fn grow_checks(s: &mut FoodSim, start: u64, n: u64) {
        for k in 0..n {
            s.step(start + k * GROW_EVERY);
        }
    }

    #[test]
    fn no_crop_grows_in_winter() {
        let mut s = sown(100);
        grow_checks(&mut s, WINTER, 1000);
        assert_eq!(ripe_count(&s), 0);
    }

    #[test]
    fn summer_ripening_matches_the_old_odds() {
        let mut s = sown(100);
        grow_checks(&mut s, SUMMER, 1);
        assert!(ripe_count(&s) <= 10, "{}", ripe_count(&s));
        grow_checks(&mut s, SUMMER + GROW_EVERY, 199);
        assert!(ripe_count(&s) >= 60, "{}", ripe_count(&s));
    }

    #[test]
    fn crops_grow_slower_in_spring_and_autumn() {
        let ripe_after = |start| {
            let mut s = sown(100);
            grow_checks(&mut s, start, 60);
            ripe_count(&s)
        };
        let summer = ripe_after(SUMMER);
        let (spring, autumn) = (ripe_after(0), ripe_after(AUTUMN));
        assert!(
            spring < summer && autumn < summer,
            "{spring} {autumn} {summer}"
        );
        assert!(spring > 0 && autumn > 0);
    }

    #[test]
    fn a_ripe_crop_stays_ripe_and_asks_again() {
        let mut s = FoodSim::new(1);
        s.adopt(&[cell(p(1, 1), CellKind::Crop)], 0);
        for t in [SUMMER, SUMMER + GROW_EVERY, WINTER] {
            assert_eq!(s.step(t), vec![FoodEvent::CropRipe { at: p(1, 1) }]);
            assert!(s.is_ripe(p(1, 1)));
        }
    }

    #[test]
    fn step_is_deterministic() {
        let go = || {
            let mut s = FoodSim::new(9);
            s.adopt(&herd(6), 0);
            for i in 0..20 {
                s.apply_action(&FoodAction::FieldSown(p(i, 9)));
            }
            (SUMMER..SUMMER + 5_000)
                .flat_map(|t| s.step(t))
                .collect::<Vec<_>>()
        };
        let a = go();
        assert!(!a.is_empty());
        assert_eq!(a, go());
    }

    #[test]
    fn reject_of_a_move_puts_the_animal_back() {
        let mut s = FoodSim::new(1);
        s.adopt(&[cell(p(6, 5), CellKind::Cow)], 0); // already moved there
        s.reject(&[FoodEvent::AnimalMoved {
            from: p(5, 5),
            to: p(6, 5),
        }]);
        assert!(s.has_animal(p(5, 5)) && !s.has_animal(p(6, 5)));
    }

    #[test]
    fn reject_of_a_move_forgets_the_animal_if_its_origin_is_gone() {
        let mut s = FoodSim::new(1);
        // origin taken by another cow meanwhile: the moved one is forgotten
        s.adopt(
            &[cell(p(5, 5), CellKind::Cow), cell(p(6, 5), CellKind::Cow)],
            0,
        );
        s.reject(&[FoodEvent::AnimalMoved {
            from: p(5, 5),
            to: p(6, 5),
        }]);
        assert_eq!(s.animal_count(), 1);
        assert!(s.has_animal(p(5, 5)));
        // the cow is not at the destination any more: nothing to put back
        s.reject(&[FoodEvent::AnimalMoved {
            from: p(1, 1),
            to: p(2, 1),
        }]);
        assert_eq!(s.animal_count(), 1);
        assert!(!s.has_animal(p(1, 1)));
    }

    #[test]
    fn reject_of_a_birth_removes_it() {
        let mut s = FoodSim::new(1);
        s.adopt(&[cell(p(5, 5), CellKind::Cow)], 0);
        s.reject(&[FoodEvent::AnimalBorn {
            species: Species::Cow,
            at: p(5, 5),
        }]);
        assert_eq!(s.animal_count(), 0);
    }

    #[test]
    fn reject_of_a_birth_gives_the_parent_its_fertility_back() {
        let mut s = pair(CellKind::Cow, SUMMER);
        let (t0, ev) = first_birth(&mut s, SUMMER).expect("a penned pair breeds in summer");
        // the server refuses the blind birth (a tree, the cap...)
        s.reject(&[ev]);
        assert_eq!(s.animal_count(), 2);
        assert!(
            s.animals.values().all(|a| a.fertile_at <= t0),
            "parent left barren"
        );
    }

    #[test]
    fn rejects_of_chained_moves_put_every_cow_back() {
        let (x, y, z) = (p(5, 5), p(6, 5), p(4, 5));
        let mut s = FoodSim::new(1);
        // cow 1 went x -> y, then cow 2 z -> x, in that order of proposal
        s.adopt(&[cell(y, CellKind::Cow), cell(x, CellKind::Cow)], 0);
        s.reject(&[
            FoodEvent::AnimalMoved { from: x, to: y },
            FoodEvent::AnimalMoved { from: z, to: x },
        ]);
        assert!(s.has_animal(x) && s.has_animal(z) && !s.has_animal(y));
        assert_eq!(s.animal_count(), 2);
    }

    #[test]
    fn a_cow_killed_after_it_moved_leaves_no_ghost() {
        let (a, b) = (p(5, 5), p(6, 5));
        let mv = FoodEvent::AnimalMoved { from: a, to: b };
        // the server's correction after a move refused for a missing origin, in both orders
        let fix = [FoodAction::AnimalRemoved(a), FoodAction::AnimalRemoved(b)];
        for reject_first in [true, false] {
            let mut s = FoodSim::new(1);
            s.adopt(&[cell(b, CellKind::Cow)], 0); // the sim already moved it a -> b
            s.apply_action(&FoodAction::AnimalRemoved(a)); // butchered at a: no-op here
            if reject_first {
                s.reject(&[mv]);
            }
            fix.iter().for_each(|f| s.apply_action(f));
            if !reject_first {
                s.reject(&[mv]);
            }
            assert_eq!(
                s.animal_count(),
                0,
                "ghost cow (reject first: {reject_first})"
            );
        }
    }

    #[test]
    fn reject_of_ripe_marks_the_field_growing_again() {
        let mut s = FoodSim::new(1);
        s.adopt(&[cell(p(3, 3), CellKind::Crop)], 0);
        s.reject(&[FoodEvent::CropRipe { at: p(3, 3) }]);
        assert!(!s.is_ripe(p(3, 3)));
    }

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("moria-food-{}-{name}", std::process::id()))
    }

    fn seeded_store() -> FoodStore {
        let mut sim = FoodSim::new(7);
        sim.adopt(
            &[cell(p(1, 1), CellKind::Cow), cell(p(2, 2), CellKind::Crop)],
            5,
        );
        let mut st = FoodStore::default();
        st.mountains.insert(MountainId(1), sim);
        st
    }

    #[test]
    fn save_then_load_roundtrips() {
        let path = tmp("roundtrip");
        let s = seeded_store();
        s.save(&path).unwrap();
        assert_eq!(FoodStore::load_or_default(&path).unwrap(), s);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn load_missing_file_is_default() {
        let path = tmp("missing");
        assert_eq!(
            FoodStore::load_or_default(&path).unwrap(),
            FoodStore::default()
        );
    }

    #[test]
    fn load_rejects_garbage_and_truncated() {
        let path = tmp("garbage");
        std::fs::write(&path, [1, 2, 3]).unwrap();
        assert!(FoodStore::load_or_default(&path).is_err());
        seeded_store().save(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(FoodStore::load_or_default(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn load_rejects_newer_version() {
        let path = tmp("newer");
        seeded_store().save(&path).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[..4].copy_from_slice(&(FOOD_VERSION + 1).to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert!(FoodStore::load_or_default(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_v1_file_loads_as_an_empty_store() {
        // v1 (cows only) is deployed: the hidden state is rebuilt from the server's World.
        let path = tmp("v1");
        std::fs::write(
            &path,
            1u32.to_le_bytes()
                .iter()
                .chain(&[9u8; 7])
                .copied()
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            FoodStore::load_or_default(&path).unwrap(),
            FoodStore::default()
        );
        std::fs::remove_file(&path).unwrap();
    }
}
