use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tile {
    Rock,
    Gold,
    Mithril,
    Floor,
    Stair,
    /// Built by dwarves from stone; dug back like rock.
    Wall,
    /// Stone furniture: walkable, dug back to floor for its stone.
    Bed,
    Table,
    Chair,
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
    /// Build a wall on a floor tile; costs 1 stone. So does each piece of furniture.
    Wall,
    Bed,
    Table,
    Chair,
}

impl DesignationKind {
    /// The next mode: what X / the gamepad's north button switch to.
    pub fn toggled(self) -> Self {
        match self {
            Self::Dig => Self::Stairs,
            Self::Stairs => Self::Wall,
            Self::Wall | Self::Bed | Self::Table | Self::Chair => Self::Dig,
        }
    }

    /// The tile this kind builds on floor (for 1 stone); `None` for digging kinds.
    pub fn builds(self) -> Option<Tile> {
        match self {
            Self::Dig | Self::Stairs => None,
            Self::Wall => Some(Tile::Wall),
            Self::Bed => Some(Tile::Bed),
            Self::Table => Some(Tile::Table),
            Self::Chair => Some(Tile::Chair),
        }
    }
}

impl Tile {
    pub fn is_solid(self) -> bool {
        matches!(self, Tile::Rock | Tile::Gold | Tile::Mithril | Tile::Wall)
    }

    pub fn is_furniture(self) -> bool {
        matches!(self, Tile::Bed | Tile::Table | Tile::Chair)
    }

    pub fn dig_ticks(self) -> u16 {
        match self {
            Tile::Rock | Tile::Wall | Tile::Bed | Tile::Table | Tile::Chair => 10,
            Tile::Gold => 15,
            Tile::Mithril => 25,
            Tile::Floor | Tile::Stair => 0,
        }
    }

    pub fn yield_(self) -> Resources {
        let mut r = Resources::default();
        match self {
            Tile::Rock | Tile::Wall | Tile::Bed | Tile::Table | Tile::Chair => r.stone = 1,
            Tile::Gold => r.gold = 1,
            Tile::Mithril => r.mithril = 1,
            Tile::Floor | Tile::Stair => {}
        }
        r
    }
}
