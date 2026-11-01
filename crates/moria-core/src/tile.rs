use serde::{Deserialize, Serialize};

/// One variant list feeds both the enum and `Tile::ALL`, so `ALL` cannot miss a variant.
/// Variant order is the postcard wire encoding: only ever append.
macro_rules! tiles {
    ($($(#[$m:meta])* $v:ident),* $(,)?) => {
        #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Tile {
            $($(#[$m])* $v),*
        }

        impl Tile {
            /// Every variant, in declaration order.
            pub const ALL: [Tile; [$(Tile::$v),*].len()] = [$(Tile::$v),*];
        }
    };
}

tiles! {
    Rock,
    Gold,
    Mithril,
    Floor,
    Stair,
    /// Built by dwarves from stone; dug back like rock.
    Wall,
    /// Stone furniture: solid, dug back to floor for its stone.
    Bed,
    Table,
    Chair,
    Grass,
    Tree,
    Water,
    /// Hillside: walkable; digging it flattens it to floor.
    Slope,
    Soil,
    Sky,
    WoodBed,
    WoodTable,
    WoodChair,
}

#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub struct Resources {
    pub stone: u32,
    pub gold: u32,
    pub mithril: u32,
    pub wood: u32,
}

impl Resources {
    pub fn add(&mut self, o: Resources) {
        self.stone = self.stone.saturating_add(o.stone);
        self.gold = self.gold.saturating_add(o.gold);
        self.mithril = self.mithril.saturating_add(o.mithril);
        self.wood = self.wood.saturating_add(o.wood);
    }

    pub fn covers(&self, c: &Resources) -> bool {
        self.stone >= c.stone
            && self.gold >= c.gold
            && self.mithril >= c.mithril
            && self.wood >= c.wood
    }

    pub fn take(&mut self, c: &Resources) {
        self.stone = self.stone.saturating_sub(c.stone);
        self.gold = self.gold.saturating_sub(c.gold);
        self.mithril = self.mithril.saturating_sub(c.mithril);
        self.wood = self.wood.saturating_sub(c.wood);
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DesignationKind {
    Dig,
    Stairs,
    /// Build a wall on a floor tile; costs 1 stone. So does each stone furniture piece.
    Wall,
    Bed,
    Table,
    Chair,
    Chop,
    WoodBed,
    WoodTable,
    WoodChair,
}

impl DesignationKind {
    /// The next mode: what X / the gamepad's north button switch to.
    pub fn toggled(self) -> Self {
        match self {
            Self::Dig => Self::Stairs,
            Self::Stairs => Self::Chop,
            Self::Chop => Self::Wall,
            _ => Self::Dig,
        }
    }

    /// The tile this kind builds; `None` for digging kinds.
    pub fn builds(self) -> Option<Tile> {
        match self {
            Self::Dig | Self::Stairs | Self::Chop => None,
            Self::Wall => Some(Tile::Wall),
            Self::Bed => Some(Tile::Bed),
            Self::Table => Some(Tile::Table),
            Self::Chair => Some(Tile::Chair),
            Self::WoodBed => Some(Tile::WoodBed),
            Self::WoodTable => Some(Tile::WoodTable),
            Self::WoodChair => Some(Tile::WoodChair),
        }
    }

    pub fn is_build(self) -> bool {
        self.builds().is_some()
    }

    pub fn cost(self) -> Resources {
        let (stone, wood) = match self {
            Self::Dig | Self::Stairs | Self::Chop => (0, 0),
            Self::Wall | Self::Bed | Self::Table | Self::Chair => (1, 0),
            Self::WoodTable => (0, 4),
            Self::WoodBed => (0, 3),
            Self::WoodChair => (0, 2),
        };
        Resources {
            stone,
            wood,
            ..Default::default()
        }
    }
}

impl Tile {
    pub fn is_solid(self) -> bool {
        matches!(
            self,
            Tile::Rock | Tile::Gold | Tile::Mithril | Tile::Wall | Tile::Soil | Tile::Tree
        ) || self.is_furniture()
    }

    pub fn is_floor_like(self) -> bool {
        matches!(self, Tile::Floor | Tile::Grass)
    }

    pub fn is_furniture(self) -> bool {
        matches!(
            self,
            Tile::Bed
                | Tile::Table
                | Tile::Chair
                | Tile::WoodBed
                | Tile::WoodTable
                | Tile::WoodChair
        )
    }

    pub fn dig_ticks(self) -> u16 {
        match self {
            Tile::Rock | Tile::Wall | Tile::Bed | Tile::Table | Tile::Chair => 10,
            Tile::WoodBed | Tile::WoodTable | Tile::WoodChair => 10,
            Tile::Gold => 15,
            Tile::Mithril => 25,
            Tile::Soil | Tile::Slope => 8,
            Tile::Tree => 20,
            Tile::Floor | Tile::Stair | Tile::Grass | Tile::Water | Tile::Sky => 0,
        }
    }

    pub fn yield_(self) -> Resources {
        let mut r = Resources::default();
        match self {
            Tile::Rock => r.stone = 1,
            Tile::Gold => r.gold = 1,
            Tile::Mithril => r.mithril = 1,
            Tile::Tree => r.wood = 3,
            Tile::Wall => return DesignationKind::Wall.cost(),
            Tile::Bed => return DesignationKind::Bed.cost(),
            Tile::Table => return DesignationKind::Table.cost(),
            Tile::Chair => return DesignationKind::Chair.cost(),
            Tile::WoodBed => return DesignationKind::WoodBed.cost(),
            Tile::WoodTable => return DesignationKind::WoodTable.cost(),
            Tile::WoodChair => return DesignationKind::WoodChair.cost(),
            _ => {}
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_sane_and_unique() {
        assert_eq!(Tile::ALL[0], Tile::Rock);
        let set: std::collections::HashSet<_> = Tile::ALL.iter().collect();
        assert_eq!(set.len(), Tile::ALL.len());
    }

    #[test]
    fn wire_encoding_is_unchanged() {
        use Tile::*;
        // The original variant order since save format v2; the index is the postcard byte.
        let expected = [
            Rock, Gold, Mithril, Floor, Stair, Wall, Bed, Table, Chair, Grass, Tree, Water, Slope,
            Soil, Sky, WoodBed, WoodTable, WoodChair,
        ];
        assert_eq!(Tile::ALL, expected);
        for (i, t) in expected.iter().enumerate() {
            assert_eq!(postcard::to_allocvec(t).unwrap(), [i as u8], "{t:?}");
        }
    }

    #[test]
    fn tree_yields_three_wood() {
        assert_eq!(
            Tile::Tree.yield_(),
            Resources {
                wood: 3,
                ..Default::default()
            }
        );
        assert_eq!(Tile::Tree.dig_ticks(), 20);
    }

    #[test]
    fn wood_furniture_costs() {
        assert_eq!(DesignationKind::WoodTable.cost().wood, 4);
        assert_eq!(DesignationKind::WoodBed.cost().wood, 3);
        assert_eq!(DesignationKind::WoodChair.cost().wood, 2);
        assert_eq!(
            DesignationKind::Bed.cost(),
            Resources {
                stone: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            DesignationKind::Wall.cost(),
            Resources {
                stone: 1,
                ..Default::default()
            }
        );
        assert_eq!(DesignationKind::Chop.cost(), Resources::default());
    }

    #[test]
    fn dismantling_returns_cost() {
        for k in [
            DesignationKind::WoodBed,
            DesignationKind::Bed,
            DesignationKind::Wall,
        ] {
            assert_eq!(k.builds().unwrap().yield_(), k.cost());
        }
        assert!(DesignationKind::WoodChair.is_build() && !DesignationKind::Chop.is_build());
    }

    #[test]
    fn water_and_sky_are_inert() {
        for t in [Tile::Water, Tile::Sky] {
            assert!(!t.is_solid() && !t.is_floor_like());
        }
        assert!(Tile::Grass.is_floor_like() && Tile::Floor.is_floor_like());
        assert!(!Tile::Slope.is_solid() && Tile::Slope.dig_ticks() == 8);
        assert!(Tile::Tree.is_solid() && Tile::Soil.is_solid() && Tile::WoodTable.is_solid());
    }

    #[test]
    fn resources_covers_and_take() {
        let mut r = Resources {
            stone: 1,
            wood: 2,
            ..Default::default()
        };
        assert!(r.covers(&Resources {
            wood: 2,
            ..Default::default()
        }));
        assert!(!r.covers(&Resources {
            wood: 3,
            ..Default::default()
        }));
        r.take(&Resources {
            wood: 5,
            stone: 1,
            ..Default::default()
        });
        assert_eq!((r.wood, r.stone), (0, 0));
    }

    #[test]
    fn toggle_cycle_includes_chop() {
        use DesignationKind::*;
        assert_eq!(Dig.toggled(), Stairs);
        assert_eq!(Stairs.toggled(), Chop);
        assert_eq!(Chop.toggled(), Wall);
        assert_eq!(Wall.toggled(), Dig);
        assert_eq!(WoodBed.toggled(), Dig);
    }
}
