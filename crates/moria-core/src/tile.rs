use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tile {
    Rock,
    Gold,
    Mithril,
    Floor,
    Stair,
}

#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub struct Resources {
    pub stone: u32,
    pub gold: u32,
    pub mithril: u32,
}

impl Resources {
    pub fn add(&mut self, o: Resources) {
        self.stone = self.stone.saturating_add(o.stone);
        self.gold = self.gold.saturating_add(o.gold);
        self.mithril = self.mithril.saturating_add(o.mithril);
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DesignationKind {
    Dig,
    Stairs,
}

impl DesignationKind {
    /// The other mode: what X / the gamepad's north button switch to.
    pub fn toggled(self) -> Self {
        match self {
            Self::Dig => Self::Stairs,
            Self::Stairs => Self::Dig,
        }
    }
}

impl Tile {
    pub fn is_solid(self) -> bool {
        matches!(self, Tile::Rock | Tile::Gold | Tile::Mithril)
    }

    pub fn dig_ticks(self) -> u16 {
        match self {
            Tile::Rock => 10,
            Tile::Gold => 15,
            Tile::Mithril => 25,
            Tile::Floor | Tile::Stair => 0,
        }
    }

    pub fn yield_(self) -> Resources {
        let mut r = Resources::default();
        match self {
            Tile::Rock => r.stone = 1,
            Tile::Gold => r.gold = 1,
            Tile::Mithril => r.mithril = 1,
            Tile::Floor | Tile::Stair => {}
        }
        r
    }
}
