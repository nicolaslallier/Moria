use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MountainId(pub u16);
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlayerId(pub u32);
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DwarfId(pub u32);

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dir {
    N,
    S,
    E,
    W,
    Up,
    Down,
}

impl Dir {
    pub const HORIZONTAL: [Dir; 4] = [Dir::N, Dir::S, Dir::E, Dir::W];
}

/// `z` grows downward: 0 is the surface, `Dir::Down` is `z + 1`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TilePos {
    pub x: i16,
    pub y: i16,
    pub z: u8,
}

impl TilePos {
    pub fn new(x: i16, y: i16, z: u8) -> Self {
        Self { x, y, z }
    }

    /// Neighbour in direction `d`. `z` saturates at 0 and 255; callers bounds-check.
    pub fn step(self, d: Dir) -> TilePos {
        let Self { x, y, z } = self;
        match d {
            Dir::N => Self::new(x, y - 1, z),
            Dir::S => Self::new(x, y + 1, z),
            Dir::E => Self::new(x + 1, y, z),
            Dir::W => Self::new(x - 1, y, z),
            Dir::Up => Self::new(x, y, z.saturating_sub(1)),
            Dir::Down => Self::new(x, y, z.saturating_add(1)),
        }
    }
}
