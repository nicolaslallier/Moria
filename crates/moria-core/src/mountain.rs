use crate::{DesignationKind, Dir, MountainId, Resources, Tile, TilePos};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub const MOUNTAIN_W: i16 = 176;
pub const MOUNTAIN_H: i16 = 96;
pub const MOUNTAIN_D: u8 = 12;
/// Surface level: z below it is sky, z above it is underground (z grows downward).
pub const GROUND_Z: u8 = 4;
/// Most designations one mountain holds: bounds the AI view (bus message size) and its solver.
pub const MAX_DESIGNATIONS: usize = 4096;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Mountain {
    pub id: MountainId,
    w: i16,
    h: i16,
    d: u8,
    tiles: Vec<Tile>,
    pub designations: BTreeMap<TilePos, DesignationKind>,
    /// Standing work orders per workshop tile: how many more runs are wanted.
    pub orders: BTreeMap<TilePos, u8>,
}

impl Mountain {
    pub fn new(id: MountainId, w: i16, h: i16, d: u8, fill: Tile) -> Self {
        Self {
            id,
            w,
            h,
            d,
            tiles: vec![fill; w as usize * h as usize * d as usize],
            designations: BTreeMap::new(),
            orders: BTreeMap::new(),
        }
    }

    pub fn size(&self) -> (i16, i16, u8) {
        (self.w, self.h, self.d)
    }

    /// False when the tile vec doesn't match the dimensions (possible after untrusted
    /// deserialization, which bypasses `new`).
    pub fn is_consistent(&self) -> bool {
        self.w > 0
            && self.h > 0
            && self.d > 0
            && self.tiles.len() == self.w as usize * self.h as usize * self.d as usize
    }

    pub fn in_bounds(&self, p: TilePos) -> bool {
        (0..self.w).contains(&p.x) && (0..self.h).contains(&p.y) && p.z < self.d
    }

    fn idx(&self, p: TilePos) -> usize {
        (p.z as usize * self.h as usize + p.y as usize) * self.w as usize + p.x as usize
    }

    pub fn get(&self, p: TilePos) -> Option<Tile> {
        self.in_bounds(p).then(|| self.tiles[self.idx(p)])
    }

    /// No-op when out of bounds.
    pub fn set(&mut self, p: TilePos, t: Tile) {
        if self.in_bounds(p) {
            let i = self.idx(p);
            self.tiles[i] = t;
        }
    }

    pub fn walkable(&self, p: TilePos) -> bool {
        self.get(p).is_some_and(|t| {
            !t.is_locked_door()
                && (t.is_floor_like()
                    || matches!(t, Tile::Stair | Tile::Slope | Tile::Field | Tile::Crop)
                    || t.is_furniture())
        })
    }

    /// Where a move from `from` toward `dir` ends up: stairs change level vertically, slopes
    /// change it horizontally (climb onto a hill, descend from it). None when blocked.
    pub fn move_dest(&self, from: TilePos, dir: Dir) -> Option<TilePos> {
        self.move_dest_by(from, dir, |p| self.walkable(p))
    }

    /// Like `move_dest`, but a locked door is passable for the hero (horizontal moves).
    pub fn hero_move_dest(&self, from: TilePos, dir: Dir) -> Option<TilePos> {
        self.move_dest_by(from, dir, |p| {
            self.walkable(p) || self.get(p).is_some_and(Tile::is_locked_door)
        })
    }

    fn move_dest_by(
        &self,
        from: TilePos,
        dir: Dir,
        walkable: impl Fn(TilePos) -> bool,
    ) -> Option<TilePos> {
        let q = from.step(dir);
        if matches!(dir, Dir::Up | Dir::Down) {
            return (q != from
                && self.get(from) == Some(Tile::Stair)
                && self.get(q) == Some(Tile::Stair))
            .then_some(q);
        }
        if !walkable(from) {
            return None;
        }
        if walkable(q) {
            return Some(q);
        }
        // Doors are furniture, hence solid, but no one climbs over one.
        let climbable = |p: TilePos| self.get(p).is_some_and(|t| t.is_solid() && !t.is_door());
        let up = from.step(Dir::Up);
        if self.get(from) == Some(Tile::Slope)
            && climbable(q)
            && up != from
            && self.get(up) == Some(Tile::Sky)
        {
            let q_up = q.step(Dir::Up);
            return walkable(q_up).then_some(q_up);
        }
        // Mirror of the climb: only descend where the slope leads straight back up here.
        if self.get(q) == Some(Tile::Sky) && climbable(from.step(Dir::Down)) {
            let q_down = q.step(Dir::Down);
            return (self.get(q_down) == Some(Tile::Slope)).then_some(q_down);
        }
        None
    }

    /// The bare ground digging leaves at `p`: grass outdoors (sky above, soil below), else floor.
    pub fn ground_at(&self, p: TilePos) -> Tile {
        let outdoors = p.z > 0
            && self.get(p.step(Dir::Up)) == Some(Tile::Sky)
            && self.get(p.step(Dir::Down)) == Some(Tile::Soil);
        if outdoors { Tile::Grass } else { Tile::Floor }
    }

    /// Whether the tile above `p` has a way down off a hill that needs `p` solid under it.
    fn holds_up_a_way_down(&self, p: TilePos) -> bool {
        let up = p.step(Dir::Up);
        up != p
            && Dir::HORIZONTAL
                .iter()
                .any(|&d| self.move_dest(up, d).is_some_and(|q| q.z > up.z))
    }

    /// Whether the slope `p` is the only way from the high ground it leads to back down to
    /// its level: a search over that high ground that never steps onto `p`.
    fn is_last_way_down(&self, p: TilePos) -> bool {
        let mut todo: Vec<TilePos> = Dir::HORIZONTAL
            .iter()
            .filter_map(|&d| self.move_dest(p, d))
            .filter(|q| q.z < p.z)
            .collect();
        let mut seen: HashSet<TilePos> = todo.iter().copied().collect();
        while let Some(q) = todo.pop() {
            if q.z >= p.z {
                return false;
            }
            for d in [Dir::N, Dir::S, Dir::E, Dir::W, Dir::Up, Dir::Down] {
                if let Some(n) = self.move_dest(q, d)
                    && n != p
                    && seen.insert(n)
                {
                    todo.push(n);
                }
            }
        }
        !seen.is_empty()
    }

    pub fn can_move(&self, from: TilePos, dir: Dir) -> bool {
        self.move_dest(from, dir).is_some()
    }

    /// Whether `kind` can be applied at `p` right now.
    pub fn diggable(&self, p: TilePos, kind: DesignationKind) -> bool {
        use DesignationKind as K;
        let Some(t) = self.get(p) else { return false };
        match kind {
            // Never strand someone on a hill: keep its last slope and the ground under its exits.
            K::Dig => {
                ((t.is_solid() && t != Tile::Tree)
                    || matches!(t, Tile::Field | Tile::Crop)
                    || (t == Tile::Slope && !self.is_last_way_down(p)))
                    && !self.holds_up_a_way_down(p)
            }
            K::Chop => matches!(t, Tile::Tree | Tile::Cow | Tile::Crop),
            K::Farm => t == Tile::Grass && p.z == GROUND_Z,
            K::Smelt => t == Tile::Forge,
            K::Smith => t == Tile::Anvil,
            K::Stairs => stairs_ok(t) && p.z + 1 < self.d && !self.holds_up_a_way_down(p),
            _ => t.is_floor_like(), // builds
        }
    }

    /// What a dwarf can do at `p`: its designation, else the workshop's recipe when ordered.
    pub fn work_at(&self, p: TilePos) -> Option<DesignationKind> {
        self.designations.get(&p).copied().or_else(|| {
            if self.orders.get(&p).copied()? > 0 {
                self.get(p)?.recipe()
            } else {
                None
            }
        })
    }

    /// Tiles of the rectangle `a..=b` on level `a.z`, clamped to the mountain (the rectangle
    /// comes from the client). Empty when `a.z` is out of range.
    pub fn rect(&self, a: TilePos, b: TilePos) -> Vec<TilePos> {
        if a.z >= self.d {
            return vec![];
        }
        let (x0, x1) = (a.x.min(b.x).max(0), a.x.max(b.x).min(self.w - 1));
        let (y0, y1) = (a.y.min(b.y).max(0), a.y.max(b.y).min(self.h - 1));
        (y0..=y1)
            .flat_map(|y| (x0..=x1).map(move |x| TilePos::new(x, y, a.z)))
            .collect()
    }

    /// Designates the rectangle `a..=b` on level `a.z`; returns how many tiles were added.
    pub fn designate(&mut self, a: TilePos, b: TilePos, kind: DesignationKind) -> usize {
        let mut n = 0;
        for p in self.rect(a, b) {
            // Cheap cap check first: a full mountain skips the per-tile rules (and their search).
            let full = self.designations.len() >= MAX_DESIGNATIONS;
            // A dragged dig leaves fields alone; grubbing one up takes a one-tile designation.
            let field = kind == DesignationKind::Dig
                && a != b
                && matches!(self.get(p), Some(Tile::Field | Tile::Crop));
            if (!full || self.designations.contains_key(&p)) && !field && self.diggable(p, kind) {
                self.designations.insert(p, kind);
                n += 1;
            }
        }
        n
    }

    /// Removes the designations in the (clamped) rectangle; returns the removed positions.
    pub fn cancel_designations(&mut self, a: TilePos, b: TilePos) -> Vec<TilePos> {
        let mut removed = vec![];
        for p in self.rect(a, b) {
            if self.designations.remove(&p).is_some() {
                removed.push(p);
            }
        }
        removed
    }

    pub fn dig(&mut self, p: TilePos, kind: DesignationKind) -> Option<Resources> {
        if !self.diggable(p, kind) {
            return None;
        }
        let mut res = self.get(p)?.yield_();
        match kind {
            DesignationKind::Smelt | DesignationKind::Smith => return None,
            DesignationKind::Dig if matches!(self.get(p), Some(Tile::Field | Tile::Crop)) => {
                self.set(p, Tile::Grass)
            }
            DesignationKind::Dig => self.set(p, self.ground_at(p)),
            DesignationKind::Chop if self.get(p) == Some(Tile::Crop) => self.set(p, Tile::Field),
            DesignationKind::Chop => self.set(p, Tile::Grass),
            // The stone cost is paid by the caller, which holds the resources.
            DesignationKind::Wall
            | DesignationKind::Bed
            | DesignationKind::Table
            | DesignationKind::Chair
            | DesignationKind::WoodBed
            | DesignationKind::WoodTable
            | DesignationKind::WoodChair
            | DesignationKind::Candle
            | DesignationKind::Torch
            | DesignationKind::Lamp
            | DesignationKind::Campfire
            | DesignationKind::Hearth
            | DesignationKind::Stove
            | DesignationKind::Forge
            | DesignationKind::Anvil
            | DesignationKind::WoodDoor
            | DesignationKind::StoneDoor
            | DesignationKind::IronDoor
            | DesignationKind::Farm => self.set(p, kind.builds()?),
            DesignationKind::Stairs => {
                self.set(p, Tile::Stair);
                let below = p.step(Dir::Down);
                if let Some(t) = self.get(below).filter(|t| stairs_ok(*t)) {
                    res.add(t.yield_());
                    self.set(below, Tile::Stair);
                    // A designation there could never be dug any more.
                    self.designations.remove(&below);
                }
            }
        }
        self.designations.remove(&p);
        self.orders.remove(&p);
        Some(res)
    }
}

/// Stairs can be cut into stone or built on a floor.
fn stairs_ok(t: Tile) -> bool {
    t.is_stone() || t.is_floor_like()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rock() -> Mountain {
        Mountain::new(MountainId(0), 5, 5, 3, Tile::Rock)
    }
    fn p(x: i16, y: i16, z: u8) -> TilePos {
        TilePos::new(x, y, z)
    }

    #[test]
    fn locked_doors_stop_dwarves_not_the_hero() {
        let mut m = Mountain::new(MountainId(0), 5, 1, 1, Tile::Floor);
        let (a, door, b) = (p(1, 0, 0), p(2, 0, 0), p(3, 0, 0));
        m.set(door, Tile::WoodDoor);
        assert_eq!(m.move_dest(a, Dir::E), Some(door));
        m.set(door, Tile::WoodDoorLocked);
        assert!(!m.walkable(door));
        assert_eq!(m.move_dest(a, Dir::E), None);
        assert!(crate::path::find_path(&m, a, |q| q == b, &Default::default()).is_none());
        assert_eq!(m.hero_move_dest(a, Dir::E), Some(door));
        assert_eq!(m.hero_move_dest(door, Dir::E), Some(b));
    }

    #[test]
    fn dig_rock_gives_stone_and_floor() {
        let mut m = rock();
        let r = m.dig(p(1, 1, 0), DesignationKind::Dig).unwrap();
        assert_eq!(
            r,
            Resources {
                stone: 1,
                ..Default::default()
            }
        );
        assert_eq!(m.get(p(1, 1, 0)), Some(Tile::Floor));
        assert_eq!(m.dig(p(1, 1, 0), DesignationKind::Dig), None);
    }

    #[test]
    fn stairs_open_two_levels() {
        let mut m = rock();
        let r = m.dig(p(1, 1, 0), DesignationKind::Stairs).unwrap();
        assert_eq!(r.stone, 2);
        assert_eq!(m.get(p(1, 1, 0)), Some(Tile::Stair));
        assert_eq!(m.get(p(1, 1, 1)), Some(Tile::Stair));
        assert!(m.can_move(p(1, 1, 0), Dir::Down));
        assert!(m.can_move(p(1, 1, 1), Dir::Up));
    }

    #[test]
    fn stairs_designation_allowed_on_floor() {
        let mut m = rock();
        m.set(p(2, 2, 0), Tile::Floor);
        assert_eq!(
            m.designate(p(2, 2, 0), p(2, 2, 0), DesignationKind::Stairs),
            1
        );
        // Dig stays solid-only.
        assert_eq!(m.designate(p(2, 2, 0), p(2, 2, 0), DesignationKind::Dig), 0);
        let r = m.dig(p(2, 2, 0), DesignationKind::Stairs).unwrap();
        assert_eq!(r.stone, 1); // only the tile below
        assert_eq!(m.get(p(2, 2, 0)), Some(Tile::Stair));
        assert_eq!(m.get(p(2, 2, 1)), Some(Tile::Stair));
        assert!(m.designations.is_empty());
        // Floor below also becomes stair, yielding nothing.
        m.set(p(3, 3, 0), Tile::Floor);
        m.set(p(3, 3, 1), Tile::Floor);
        assert_eq!(
            m.dig(p(3, 3, 0), DesignationKind::Stairs),
            Some(Resources::default())
        );
        assert_eq!(m.get(p(3, 3, 1)), Some(Tile::Stair));
        // Not on the bottom level, nor on a Stair.
        m.set(p(0, 0, 2), Tile::Floor);
        assert_eq!(m.dig(p(0, 0, 2), DesignationKind::Stairs), None);
        assert_eq!(m.dig(p(2, 2, 0), DesignationKind::Stairs), None);
    }

    #[test]
    fn stairs_clear_designation_below() {
        let mut m = rock();
        m.designate(p(1, 1, 1), p(1, 1, 1), DesignationKind::Dig);
        m.dig(p(1, 1, 0), DesignationKind::Stairs).unwrap();
        assert_eq!(m.get(p(1, 1, 1)), Some(Tile::Stair));
        assert!(m.designations.is_empty());
    }

    #[test]
    fn wall_only_on_floor_and_digs_back_to_stone() {
        let mut m = rock();
        m.set(p(1, 1, 0), Tile::Floor);
        assert_eq!(
            m.designate(p(0, 1, 0), p(1, 1, 0), DesignationKind::Wall),
            1
        );
        assert_eq!(
            m.dig(p(1, 1, 0), DesignationKind::Wall),
            Some(Resources::default())
        );
        assert_eq!(m.get(p(1, 1, 0)), Some(Tile::Wall));
        assert!(!m.walkable(p(1, 1, 0)));
        assert_eq!(m.dig(p(1, 1, 0), DesignationKind::Dig).unwrap().stone, 1);
    }

    #[test]
    fn furniture_on_floor_is_walkable_and_digs_back_to_stone() {
        let mut m = Mountain::new(MountainId(0), 3, 3, 1, Tile::Floor);
        let r = m.dig(p(1, 1, 0), DesignationKind::Bed).unwrap();
        assert_eq!(r.stone, 0);
        assert_eq!(m.get(p(1, 1, 0)), Some(Tile::Bed));
        assert!(m.walkable(p(1, 1, 0)));
        assert_eq!(
            m.dig(p(1, 1, 0), DesignationKind::Chair),
            None,
            "only on floor"
        );
        assert_eq!(m.dig(p(1, 1, 0), DesignationKind::Dig).unwrap().stone, 1);
        assert_eq!(m.get(p(1, 1, 0)), Some(Tile::Floor));
    }

    #[test]
    fn cannot_move_into_rock_or_out_of_bounds() {
        let mut m = rock();
        m.set(p(0, 0, 0), Tile::Floor);
        assert!(!m.can_move(p(0, 0, 0), Dir::E));
        assert!(!m.can_move(p(0, 0, 0), Dir::N));
        assert!(!m.can_move(p(0, 0, 0), Dir::W));
        m.set(p(1, 0, 0), Tile::Floor);
        assert!(m.can_move(p(0, 0, 0), Dir::E));
        assert!(!m.can_move(p(0, 0, 0), Dir::Down));
        assert!(!m.can_move(p(0, 0, 0), Dir::Up));
    }

    #[test]
    fn orders_make_work_and_die_with_the_workshop() {
        let mut m = Mountain::new(MountainId(0), 4, 4, 1, Tile::Floor);
        let p = TilePos::new(1, 1, 0);
        m.set(p, Tile::Forge);
        assert_eq!(m.work_at(p), None);
        m.orders.insert(p, 5);
        assert_eq!(m.work_at(p), Some(DesignationKind::Smelt));
        assert!(m.diggable(p, DesignationKind::Smelt) && !m.diggable(p, DesignationKind::Smith));
        m.designate(p, p, DesignationKind::Dig);
        assert_eq!(m.work_at(p), Some(DesignationKind::Dig)); // dismantling wins
        assert_eq!(m.dig(p, DesignationKind::Dig).unwrap().stone, 6);
        assert!(m.orders.is_empty());
    }

    #[test]
    fn designate_skips_open_tiles() {
        let mut m = rock();
        m.set(p(1, 0, 0), Tile::Floor);
        assert_eq!(m.designate(p(2, 0, 0), p(0, 0, 0), DesignationKind::Dig), 2);
        assert!(!m.designations.contains_key(&p(1, 0, 0)));
    }

    #[test]
    fn stairs_not_designated_on_bottom_level() {
        let mut m = rock();
        assert_eq!(
            m.designate(p(0, 0, 2), p(2, 2, 2), DesignationKind::Stairs),
            0
        );
    }

    #[test]
    fn designate_huge_rect_is_clamped() {
        let mut m = rock();
        let n = m.designate(
            p(-32768, -32768, 0),
            p(32767, 32767, 0),
            DesignationKind::Dig,
        );
        assert_eq!(n, 25);
        assert_eq!(m.designate(p(0, 0, 9), p(1, 1, 9), DesignationKind::Dig), 0);
        assert_eq!(
            m.designate(p(-9, -9, 0), p(-3, -3, 0), DesignationKind::Dig),
            0
        );
    }

    #[test]
    fn dig_removes_designation() {
        let mut m = rock();
        m.designate(p(1, 1, 0), p(1, 1, 0), DesignationKind::Dig);
        assert_eq!(m.designations.len(), 1);
        m.dig(p(1, 1, 0), DesignationKind::Dig);
        assert!(m.designations.is_empty());
    }

    #[test]
    fn cancel_designations_clamped_and_reported() {
        let mut m = rock();
        m.designate(p(0, 0, 0), p(2, 0, 0), DesignationKind::Dig);
        m.designate(p(0, 1, 1), p(0, 1, 1), DesignationKind::Dig);
        let r = m.cancel_designations(p(-100, -100, 0), p(100, 0, 0));
        assert_eq!(r, vec![p(0, 0, 0), p(1, 0, 0), p(2, 0, 0)]);
        assert_eq!(m.designations.len(), 1);
        assert!(m.cancel_designations(p(0, 0, 9), p(4, 4, 9)).is_empty());
    }

    /// 12x12x4: sky z0-1, grass z2, rock z3. Hill of Soil at (6,5,2) topped by Grass (6,5,1),
    /// Slope at its foot (5,5,2). A slope-less hill at (6,7,2)/(6,7,1).
    fn hill() -> Mountain {
        let mut m = Mountain::new(MountainId(0), 12, 12, 4, Tile::Sky);
        for y in 0..12 {
            for x in 0..12 {
                m.set(p(x, y, 2), Tile::Grass);
                m.set(p(x, y, 3), Tile::Rock);
            }
        }
        m.set(p(5, 5, 2), Tile::Slope);
        for y in [5, 7] {
            m.set(p(6, y, 2), Tile::Soil);
            m.set(p(6, y, 1), Tile::Grass);
        }
        m
    }

    #[test]
    fn climb_slope_onto_hill() {
        assert_eq!(hill().move_dest(p(5, 5, 2), Dir::E), Some(p(6, 5, 1)));
    }

    #[test]
    fn descend_hill_via_slope() {
        assert_eq!(hill().move_dest(p(6, 5, 1), Dir::W), Some(p(5, 5, 2)));
    }

    #[test]
    fn no_climb_over_a_door() {
        let mut m = hill();
        m.set(p(6, 5, 2), Tile::WoodDoorLocked); // grass still on top at (6,5,1)
        assert_eq!(m.move_dest(p(5, 5, 2), Dir::E), None);
        assert_eq!(m.move_dest(p(6, 5, 1), Dir::W), None);
        let top = p(6, 5, 1);
        assert!(
            crate::path::find_path(&m, p(5, 5, 2), |q| q == top, &Default::default()).is_none()
        );
    }

    #[test]
    fn no_climb_without_slope() {
        assert_eq!(hill().move_dest(p(5, 7, 2), Dir::E), None);
    }

    #[test]
    fn no_fall_off_hill_edge() {
        let m = hill();
        for d in [Dir::N, Dir::E, Dir::S] {
            assert_eq!(m.move_dest(p(6, 5, 1), d), None);
        }
    }

    #[test]
    fn water_and_sky_block() {
        let mut m = hill();
        m.set(p(3, 5, 2), Tile::Water);
        assert_eq!(m.move_dest(p(2, 5, 2), Dir::E), None);
        assert_eq!(m.move_dest(p(6, 5, 1), Dir::N), None);
        for k in [
            DesignationKind::Dig,
            DesignationKind::Chop,
            DesignationKind::Wall,
            DesignationKind::Stairs,
            DesignationKind::WoodBed,
        ] {
            assert!(!m.diggable(p(3, 5, 2), k));
            assert!(!m.diggable(p(0, 0, 0), k));
        }
    }

    #[test]
    fn butcher_cow_gives_food_and_grass() {
        let mut m = hill();
        let c = p(1, 1, 2);
        m.set(c, Tile::Cow);
        assert!(!m.walkable(c), "a cow blocks the way like a tree");
        assert!(!m.diggable(c, DesignationKind::Dig));
        assert!(!m.diggable(c, DesignationKind::Stairs));
        let r = m.dig(c, DesignationKind::Chop).unwrap();
        assert_eq!(r.food, 4);
        assert_eq!(m.get(c), Some(Tile::Grass));
    }

    #[test]
    fn fields_are_sown_on_ground_grass_and_harvested_for_food() {
        // The hill's plain is z 2 here, but fields only go on GROUND_Z: build one that deep.
        let mut m = Mountain::new(MountainId(0), 4, 4, GROUND_Z + 1, Tile::Grass);
        let (a, up) = (p(1, 1, GROUND_Z), p(1, 1, GROUND_Z - 1));
        assert!(m.diggable(a, DesignationKind::Farm));
        assert!(
            !m.diggable(up, DesignationKind::Farm),
            "only the ground level"
        );
        m.set(p(2, 1, GROUND_Z), Tile::Floor);
        assert!(
            !m.diggable(p(2, 1, GROUND_Z), DesignationKind::Farm),
            "not on floor"
        );
        assert_eq!(m.dig(a, DesignationKind::Farm), Some(Resources::default()));
        assert_eq!(m.get(a), Some(Tile::Field));
        assert!(m.walkable(a), "a field is walked across");
        assert!(
            !m.diggable(a, DesignationKind::Chop),
            "nothing to harvest yet"
        );
        m.set(a, Tile::Crop);
        assert!(m.walkable(a));
        let r = m.dig(a, DesignationKind::Chop).unwrap();
        assert_eq!(r.food, 2);
        assert_eq!(m.get(a), Some(Tile::Field), "harvest leaves the field sown");
        // Digging grubs a field up for good; a ripe one gives its food first.
        assert_eq!(m.dig(a, DesignationKind::Dig), Some(Resources::default()));
        assert_eq!(m.get(a), Some(Tile::Grass));
        m.set(a, Tile::Crop);
        assert_eq!(m.dig(a, DesignationKind::Dig).map(|r| r.food), Some(2));
        assert_eq!(m.get(a), Some(Tile::Grass));
    }

    #[test]
    fn a_dig_rectangle_spares_fields() {
        let mut m = Mountain::new(MountainId(0), 4, 4, GROUND_Z + 1, Tile::Grass);
        let (field, rock) = (p(1, 1, GROUND_Z), p(2, 1, GROUND_Z));
        m.set(field, Tile::Field);
        m.set(rock, Tile::Rock);
        assert_eq!(m.designate(field, rock, DesignationKind::Dig), 1);
        assert!(
            !m.designations.contains_key(&field),
            "a dragged dig leaves the crops alone"
        );
        assert_eq!(
            m.designate(field, field, DesignationKind::Dig),
            1,
            "one tile on purpose"
        );
    }

    #[test]
    fn chop_tree_gives_wood_and_grass() {
        let mut m = hill();
        m.set(p(1, 1, 2), Tile::Tree);
        assert!(!m.diggable(p(1, 1, 2), DesignationKind::Dig));
        assert!(!m.diggable(p(1, 1, 2), DesignationKind::Stairs));
        assert!(!m.diggable(p(2, 2, 2), DesignationKind::Chop), "grass");
        m.set(p(2, 2, 2), Tile::Floor);
        assert!(!m.diggable(p(2, 2, 2), DesignationKind::Chop), "floor");
        let r = m.dig(p(1, 1, 2), DesignationKind::Chop).unwrap();
        assert_eq!(r.wood, 3);
        assert_eq!(m.get(p(1, 1, 2)), Some(Tile::Grass));
    }

    #[test]
    fn last_slope_down_cannot_be_dug() {
        let mut m = hill();
        assert!(!m.diggable(p(5, 5, 2), DesignationKind::Dig));
        // A second slope (south side) leads to the same hilltop: either may go.
        m.set(p(6, 4, 2), Tile::Slope);
        assert_eq!(m.move_dest(p(6, 4, 2), Dir::S), Some(p(6, 5, 1)));
        assert!(m.diggable(p(5, 5, 2), DesignationKind::Dig));
        assert!(m.dig(p(5, 5, 2), DesignationKind::Dig).is_some());
        assert!(!m.diggable(p(6, 4, 2), DesignationKind::Dig));
        // A slope leading nowhere up is plain ground.
        m.set(p(1, 1, 2), Tile::Slope);
        assert!(m.diggable(p(1, 1, 2), DesignationKind::Dig));
    }

    #[test]
    fn hilltop_way_down_keeps_its_ground() {
        let mut m = hill();
        for k in [DesignationKind::Dig, DesignationKind::Stairs] {
            assert!(!m.diggable(p(6, 5, 2), k), "{k:?}");
            // The slope-less hill's top has no way down to lose.
            assert!(m.diggable(p(6, 7, 2), k), "{k:?}");
        }
        assert_eq!(m.designate(p(6, 5, 2), p(6, 5, 2), DesignationKind::Dig), 0);
        assert_eq!(m.dig(p(6, 5, 2), DesignationKind::Dig), None);
        // Interior: digging under a floor stays allowed.
        let mut r = rock();
        r.set(p(1, 1, 0), Tile::Floor);
        assert!(r.diggable(p(1, 1, 1), DesignationKind::Dig));
    }

    #[test]
    fn dug_ground_is_grass_outdoors_floor_indoors() {
        // z0 sky, z1 grass/rock surface, z2 soil/rock, z3 rock.
        let mut m = Mountain::new(MountainId(0), 4, 1, 4, Tile::Rock);
        for x in 0..4 {
            m.set(p(x, 0, 0), Tile::Sky);
        }
        m.set(p(0, 0, 1), Tile::Grass);
        m.set(p(0, 0, 2), Tile::Soil);
        m.set(p(1, 0, 1), Tile::Slope);
        m.set(p(1, 0, 2), Tile::Soil);
        m.set(p(2, 0, 1), Tile::Floor);
        // Furniture built on grass goes back to grass.
        m.dig(p(0, 0, 1), DesignationKind::WoodTable).unwrap();
        m.dig(p(0, 0, 1), DesignationKind::Dig).unwrap();
        assert_eq!(m.get(p(0, 0, 1)), Some(Tile::Grass));
        assert_eq!(m.ground_at(p(1, 0, 1)), Tile::Grass);
        // Rock surface (rock below): floor. Underground: floor.
        m.dig(p(2, 0, 1), DesignationKind::Bed).unwrap();
        m.dig(p(2, 0, 1), DesignationKind::Dig).unwrap();
        assert_eq!(m.get(p(2, 0, 1)), Some(Tile::Floor));
        m.dig(p(3, 0, 1), DesignationKind::Dig).unwrap();
        assert_eq!(m.get(p(3, 0, 1)), Some(Tile::Floor));
        m.dig(p(0, 0, 2), DesignationKind::Dig).unwrap();
        assert_eq!(m.get(p(0, 0, 2)), Some(Tile::Floor));
        assert_eq!(m.ground_at(p(0, 0, 0)), Tile::Floor);
    }

    #[test]
    fn designations_are_capped() {
        let mut m = Mountain::new(MountainId(0), 100, 100, 2, Tile::Rock);
        let all = m.designate(p(0, 0, 0), p(99, 99, 0), DesignationKind::Dig);
        assert_eq!(all, MAX_DESIGNATIONS);
        assert_eq!(m.designations.len(), MAX_DESIGNATIONS);
        // Re-designating an existing tile still works when full.
        let first = *m.designations.keys().next().unwrap();
        assert_eq!(m.designate(first, first, DesignationKind::Stairs), 1);
        assert_eq!(m.designations[&first], DesignationKind::Stairs);
    }

    #[test]
    fn build_on_grass() {
        let m = hill();
        let g = p(1, 1, 2);
        assert!(m.diggable(g, DesignationKind::WoodBed));
        assert!(m.diggable(g, DesignationKind::Wall));
        assert!(m.diggable(g, DesignationKind::Stairs));
    }

    #[test]
    fn slope_moves_are_symmetric() {
        let m = hill();
        let top = p(6, 5, 1);
        let foot = m.move_dest(top, Dir::W).unwrap();
        assert_eq!(m.move_dest(foot, Dir::E), Some(top));
        let up = m.move_dest(p(5, 5, 2), Dir::E).unwrap();
        assert_eq!(m.move_dest(up, Dir::W), Some(p(5, 5, 2)));
        // Soil under the hilltop dug out: climbing back is impossible, so no descent.
        let mut m = hill();
        m.set(p(6, 5, 2), Tile::Floor);
        assert_eq!(m.move_dest(top, Dir::W), None);
        assert_eq!(m.move_dest(p(5, 5, 2), Dir::E), Some(p(6, 5, 2)));
    }

    #[test]
    fn stairs_do_not_eat_furniture_below() {
        let mut m = rock();
        m.set(p(1, 1, 1), Tile::Bed);
        let r = m.dig(p(1, 1, 0), DesignationKind::Stairs).unwrap();
        assert_eq!(r.stone, 1);
        assert_eq!(m.get(p(1, 1, 0)), Some(Tile::Stair));
        assert_eq!(m.get(p(1, 1, 1)), Some(Tile::Bed));
    }

    #[test]
    fn slope_at_z0_cannot_climb() {
        let mut m = Mountain::new(MountainId(0), 3, 1, 2, Tile::Grass);
        m.set(p(0, 0, 0), Tile::Slope);
        m.set(p(1, 0, 0), Tile::Soil);
        assert_eq!(m.move_dest(p(0, 0, 0), Dir::E), None);
    }

    #[test]
    fn underground_slope_cannot_climb() {
        let mut m = rock();
        m.set(p(1, 1, 1), Tile::Slope);
        m.set(p(2, 1, 0), Tile::Floor);
        assert_eq!(m.move_dest(p(1, 1, 1), Dir::E), None);
    }

    #[test]
    fn furniture_walkable_diggable_but_no_stairs() {
        let mut m = hill();
        m.set(p(1, 1, 2), Tile::WoodBed);
        assert!(m.walkable(p(1, 1, 2)));
        assert!(m.diggable(p(1, 1, 2), DesignationKind::Dig));
        assert!(!m.diggable(p(1, 1, 2), DesignationKind::Stairs));
    }
}
