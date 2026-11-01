//! Client-side light model: what the hero has explored and how bright a tile is shown.
//! Bevy-free, like `game.rs`.

// ponytail: consumed by the renderer in Task 5; drop this allow then.
#![allow(dead_code)]

use moria_core::TilePos;

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

/// Brightness drawn for a tile: its light, or the dim memory if explored.
pub fn shown(light: f32, seen: bool) -> f32 {
    light.max(if seen { MEMORY } else { 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

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
