use crate::{DesignationKind, Dir, MountainId, Resources, Tile, TilePos};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MOUNTAIN_W: i16 = 48;
pub const MOUNTAIN_H: i16 = 48;
pub const MOUNTAIN_D: u8 = 8;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Mountain {
    pub id: MountainId,
    w: i16,
    h: i16,
    d: u8,
    tiles: Vec<Tile>,
    pub designations: BTreeMap<TilePos, DesignationKind>,
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
        matches!(self.get(p), Some(Tile::Floor | Tile::Stair))
    }

    pub fn can_move(&self, from: TilePos, dir: Dir) -> bool {
        let to = from.step(dir);
        match dir {
            Dir::Up | Dir::Down => {
                to != from
                    && self.get(from) == Some(Tile::Stair)
                    && self.get(to) == Some(Tile::Stair)
            }
            _ => self.walkable(from) && self.walkable(to),
        }
    }

    /// Whether `kind` can be applied at `p` right now.
    fn diggable(&self, p: TilePos, kind: DesignationKind) -> bool {
        match (self.get(p), kind) {
            (Some(t), DesignationKind::Dig) => t.is_solid(),
            (Some(t), DesignationKind::Stairs) => {
                (t.is_solid() || t == Tile::Floor) && p.z + 1 < self.d
            }
            (None, _) => false,
        }
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
            if self.diggable(p, kind) {
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
            DesignationKind::Dig => self.set(p, Tile::Floor),
            DesignationKind::Stairs => {
                self.set(p, Tile::Stair);
                let below = p.step(Dir::Down);
                if let Some(t) = self
                    .get(below)
                    .filter(|t| t.is_solid() || *t == Tile::Floor)
                {
                    res.add(t.yield_());
                    self.set(below, Tile::Stair);
                }
            }
        }
        self.designations.remove(&p);
        Some(res)
    }
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
    fn dig_rock_gives_stone_and_floor() {
        let mut m = rock();
        let r = m.dig(p(1, 1, 0), DesignationKind::Dig).unwrap();
        assert_eq!(
            r,
            Resources {
                stone: 1,
                gold: 0,
                mithril: 0
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
}
