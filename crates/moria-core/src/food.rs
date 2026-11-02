//! Food simulation model: cows and fields with hidden state, kept in step with the server's grid.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::TilePos;

/// Ticks between two grazing steps of the herd.
pub const COW_EVERY: u64 = 10;
/// Ticks between two growth checks; a field ripens with odds 1 in `RIPEN_ODDS` at each.
pub const GROW_EVERY: u64 = 100;
pub const RIPEN_ODDS: u64 = 60;
/// Herd ceiling per mountain.
pub const COW_CAP: usize = 24;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Cow,
    Field,
    Crop,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub pos: TilePos,
    pub kind: CellKind,
}

/// What the simulation proposes to the server.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoodEvent {
    CowMoved { from: TilePos, to: TilePos },
    CowBorn { at: TilePos },
    CropRipe { at: TilePos },
}

/// What the server tells the simulation happened.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoodAction {
    CowRemoved(TilePos),
    FieldSown(TilePos),
    FieldHarvested(TilePos),
    FieldRemoved(TilePos),
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
struct CowState {
    born: u64,
    fertile_at: u64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
struct FieldState {
    progress: u32,
    ripe: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FoodSim {
    seed: u64,
    adopted_at: u64,
    cows: BTreeMap<TilePos, CowState>,
    fields: BTreeMap<TilePos, FieldState>,
}

impl FoodSim {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            adopted_at: 0,
            cows: BTreeMap::new(),
            fields: BTreeMap::new(),
        }
    }

    /// Rebuilds the maps from the server's snapshot: known entities keep their hidden state,
    /// unknown ones get a default, those not listed are forgotten. Emits nothing.
    pub fn adopt(&mut self, cells: &[Cell], tick: u64) {
        let (old_cows, old_fields) = (
            std::mem::take(&mut self.cows),
            std::mem::take(&mut self.fields),
        );
        for c in cells {
            match c.kind {
                // Default cow: an adult, free to breed at once.
                CellKind::Cow => {
                    let st = old_cows.get(&c.pos).copied().unwrap_or(CowState {
                        born: tick.saturating_sub(1),
                        fertile_at: tick,
                    });
                    self.cows.insert(c.pos, st);
                }
                CellKind::Field | CellKind::Crop => {
                    let ripe = c.kind == CellKind::Crop;
                    let mut st = old_fields
                        .get(&c.pos)
                        .copied()
                        .unwrap_or(FieldState { progress: 0, ripe });
                    st.ripe |= ripe; // the server's grid says ripe: believe it
                    self.fields.insert(c.pos, st);
                }
            }
        }
        self.adopted_at = tick;
    }

    /// Idempotent; the server's view wins.
    pub fn apply_action(&mut self, a: &FoodAction) {
        match *a {
            FoodAction::CowRemoved(p) => {
                self.cows.remove(&p);
            }
            FoodAction::FieldSown(p) | FoodAction::FieldHarvested(p) => {
                self.fields.insert(
                    p,
                    FieldState {
                        progress: 0,
                        ripe: false,
                    },
                );
            }
            FoodAction::FieldRemoved(p) => {
                self.fields.remove(&p);
            }
        }
    }

    pub fn cow_count(&self) -> usize {
        self.cows.len()
    }

    pub fn field_count(&self) -> usize {
        self.fields.len()
    }

    pub fn is_ripe(&self, p: TilePos) -> bool {
        self.fields.get(&p).is_some_and(|f| f.ripe)
    }

    pub fn has_cow(&self, p: TilePos) -> bool {
        self.cows.contains_key(&p)
    }

    /// Tick of the last `adopt`, 0 before the first.
    pub fn adopted_at(&self) -> u64 {
        self.adopted_at
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
        assert_eq!(s.cow_count(), 1);
        assert_eq!(s.field_count(), 2);
        assert!(s.has_cow(p(1, 1)));
        assert!(!s.is_ripe(p(2, 2)));
        assert!(s.is_ripe(p(3, 3)));
        // adult cow: born long before the snapshot, may breed at once
        let c = s.cows[&p(1, 1)];
        assert!(c.born < 50 && c.fertile_at <= 50);
        assert_eq!(s.fields[&p(2, 2)].progress, 0);
    }

    #[test]
    fn adopt_keeps_hidden_state_of_known_entities() {
        let mut s = FoodSim::new(1);
        s.adopt(
            &[cell(p(1, 1), CellKind::Cow), cell(p(2, 2), CellKind::Field)],
            10,
        );
        s.cows.get_mut(&p(1, 1)).unwrap().fertile_at = 999;
        s.fields.get_mut(&p(2, 2)).unwrap().progress = 7;
        s.adopt(
            &[cell(p(1, 1), CellKind::Cow), cell(p(2, 2), CellKind::Field)],
            20,
        );
        assert_eq!(s.cows[&p(1, 1)].fertile_at, 999);
        assert_eq!(s.fields[&p(2, 2)].progress, 7);
    }

    #[test]
    fn adopt_forgets_entities_not_on_the_map() {
        let mut s = FoodSim::new(1);
        s.adopt(
            &[cell(p(1, 1), CellKind::Cow), cell(p(2, 2), CellKind::Field)],
            10,
        );
        s.adopt(&[], 20);
        assert_eq!((s.cow_count(), s.field_count()), (0, 0));
    }

    #[test]
    fn empty_food_state_proposes_nothing() {
        // a fresh sim holds nothing and the API has no way to delete from the map
        let s = FoodSim::new(1);
        assert_eq!((s.cow_count(), s.field_count()), (0, 0));
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
        s.apply_action(&FoodAction::CowRemoved(p(9, 9)));
        assert_eq!(s.cow_count(), 1);
        s.apply_action(&FoodAction::CowRemoved(p(1, 1)));
        let after = s.clone();
        s.apply_action(&FoodAction::CowRemoved(p(1, 1)));
        assert_eq!(s, after);
        assert_eq!(s.cow_count(), 0);
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
        s.fields.get_mut(&p(3, 3)).unwrap().progress = 5;
        s.apply_action(&FoodAction::FieldHarvested(p(3, 3)));
        assert_eq!(s.field_count(), 1);
        assert!(!s.is_ripe(p(3, 3)));
        assert_eq!(s.fields[&p(3, 3)].progress, 0);
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
}
