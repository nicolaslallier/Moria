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
    /// Hillside: walkable; digging it flattens it to bare ground, unless it is a hill's last way down.
    Slope,
    Soil,
    Sky,
    WoodBed,
    WoodTable,
    WoodChair,
    Candle,
    Torch,
    Lamp,
    Campfire,
    Hearth,
    Stove,
    IronOre,
    Coal,
    Forge,
    Anvil,
    WoodDoor,
    StoneDoor,
    IronDoor,
    WoodDoorLocked,
    StoneDoorLocked,
    IronDoorLocked,
    /// Grazes on the grass outdoors; blocks the way. Butchered (`Chop`) for food.
    Cow,
    /// A sown field on the ground level: walkable, ripens into `Crop` by itself.
    Field,
    /// A ripe field: harvested (`Chop`) for food, which leaves it sown again.
    Crop,
}

#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub struct Resources {
    pub stone: u32,
    pub gold: u32,
    pub mithril: u32,
    pub wood: u32,
    pub iron_ore: u32,
    pub coal: u32,
    pub iron: u32,
    pub iron_door: u32,
    pub food: u32,
}

impl Resources {
    pub fn add(&mut self, o: Resources) {
        self.stone = self.stone.saturating_add(o.stone);
        self.gold = self.gold.saturating_add(o.gold);
        self.mithril = self.mithril.saturating_add(o.mithril);
        self.wood = self.wood.saturating_add(o.wood);
        self.iron_ore = self.iron_ore.saturating_add(o.iron_ore);
        self.coal = self.coal.saturating_add(o.coal);
        self.iron = self.iron.saturating_add(o.iron);
        self.iron_door = self.iron_door.saturating_add(o.iron_door);
        self.food = self.food.saturating_add(o.food);
    }

    pub fn covers(&self, c: &Resources) -> bool {
        self.stone >= c.stone
            && self.gold >= c.gold
            && self.mithril >= c.mithril
            && self.wood >= c.wood
            && self.iron_ore >= c.iron_ore
            && self.coal >= c.coal
            && self.iron >= c.iron
            && self.iron_door >= c.iron_door
            && self.food >= c.food
    }

    pub fn take(&mut self, c: &Resources) {
        self.stone = self.stone.saturating_sub(c.stone);
        self.gold = self.gold.saturating_sub(c.gold);
        self.mithril = self.mithril.saturating_sub(c.mithril);
        self.wood = self.wood.saturating_sub(c.wood);
        self.iron_ore = self.iron_ore.saturating_sub(c.iron_ore);
        self.coal = self.coal.saturating_sub(c.coal);
        self.iron = self.iron.saturating_sub(c.iron);
        self.iron_door = self.iron_door.saturating_sub(c.iron_door);
        self.food = self.food.saturating_sub(c.food);
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
    Candle,
    Torch,
    Lamp,
    Campfire,
    Hearth,
    Stove,
    Forge,
    Anvil,
    WoodDoor,
    StoneDoor,
    IronDoor,
    /// Work at a workshop: smelt ore into iron.
    Smelt,
    /// Work at a workshop: forge an iron door.
    Smith,
    /// Sow a field on grass of the ground level; free.
    Farm,
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
            Self::Dig | Self::Stairs | Self::Chop | Self::Smelt | Self::Smith => None,
            Self::Wall => Some(Tile::Wall),
            Self::Bed => Some(Tile::Bed),
            Self::Table => Some(Tile::Table),
            Self::Chair => Some(Tile::Chair),
            Self::WoodBed => Some(Tile::WoodBed),
            Self::WoodTable => Some(Tile::WoodTable),
            Self::WoodChair => Some(Tile::WoodChair),
            Self::Candle => Some(Tile::Candle),
            Self::Torch => Some(Tile::Torch),
            Self::Lamp => Some(Tile::Lamp),
            Self::Campfire => Some(Tile::Campfire),
            Self::Hearth => Some(Tile::Hearth),
            Self::Stove => Some(Tile::Stove),
            Self::Forge => Some(Tile::Forge),
            Self::Anvil => Some(Tile::Anvil),
            Self::WoodDoor => Some(Tile::WoodDoor),
            Self::StoneDoor => Some(Tile::StoneDoor),
            Self::IronDoor => Some(Tile::IronDoor),
            Self::Farm => Some(Tile::Field),
        }
    }

    /// A job done at a workshop rather than a change to the map.
    pub fn is_work(self) -> bool {
        matches!(self, Self::Smelt | Self::Smith)
    }

    pub fn product(self) -> Resources {
        match self {
            Self::Smelt => Resources {
                iron: 1,
                ..Default::default()
            },
            Self::Smith => Resources {
                iron_door: 1,
                ..Default::default()
            },
            _ => Resources::default(),
        }
    }

    pub fn work_ticks(self) -> u16 {
        match self {
            Self::Smelt => 30,
            Self::Smith => 40,
            _ => 0,
        }
    }

    pub fn is_build(self) -> bool {
        self.builds().is_some()
    }

    pub fn cost(self) -> Resources {
        let (stone, wood, gold) = match self {
            Self::Dig | Self::Stairs | Self::Chop | Self::Farm => (0, 0, 0),
            Self::Wall | Self::Bed | Self::Table | Self::Chair => (1, 0, 0),
            Self::WoodTable => (0, 4, 0),
            Self::WoodBed => (0, 3, 0),
            Self::WoodChair => (0, 2, 0),
            Self::Candle | Self::Torch => (0, 1, 0),
            Self::Lamp => (1, 0, 1),
            Self::Campfire => (0, 3, 0),
            Self::Hearth => (4, 0, 0),
            Self::Stove => (3, 2, 0),
            Self::Forge => (6, 0, 0),
            Self::WoodDoor => (0, 3, 0),
            Self::StoneDoor => (3, 0, 0),
            Self::Anvil | Self::IronDoor | Self::Smelt | Self::Smith => (0, 0, 0),
        };
        let (iron_ore, coal, iron, iron_door) = match self {
            Self::Anvil => (0, 0, 3, 0),
            Self::IronDoor => (0, 0, 0, 1),
            Self::Smelt => (2, 1, 0, 0),
            Self::Smith => (0, 0, 2, 0),
            _ => (0, 0, 0, 0),
        };
        Resources {
            stone,
            wood,
            gold,
            iron_ore,
            coal,
            iron,
            iron_door,
            ..Default::default()
        }
    }
}

impl Tile {
    pub fn is_solid(self) -> bool {
        matches!(
            self,
            Tile::Rock
                | Tile::Gold
                | Tile::Mithril
                | Tile::IronOre
                | Tile::Coal
                | Tile::Wall
                | Tile::Soil
                | Tile::Tree
        ) || self.is_furniture()
    }

    /// Plain solid mass: stairs cut into it and it casts a cliff edge. Not trees or furniture.
    pub fn is_stone(self) -> bool {
        self.is_solid() && self != Tile::Tree && !self.is_furniture()
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
                | Tile::Candle
                | Tile::Torch
                | Tile::Lamp
                | Tile::Campfire
                | Tile::Hearth
                | Tile::Stove
                | Tile::Forge
                | Tile::Anvil
        ) || self.is_door()
    }

    pub fn is_door(self) -> bool {
        matches!(
            self,
            Tile::WoodDoor
                | Tile::StoneDoor
                | Tile::IronDoor
                | Tile::WoodDoorLocked
                | Tile::StoneDoorLocked
                | Tile::IronDoorLocked
        )
    }

    pub fn is_locked_door(self) -> bool {
        matches!(
            self,
            Tile::WoodDoorLocked | Tile::StoneDoorLocked | Tile::IronDoorLocked
        )
    }

    /// The same door open <-> locked; `None` for anything else.
    pub fn toggled_door(self) -> Option<Tile> {
        Some(match self {
            Tile::WoodDoor => Tile::WoodDoorLocked,
            Tile::WoodDoorLocked => Tile::WoodDoor,
            Tile::StoneDoor => Tile::StoneDoorLocked,
            Tile::StoneDoorLocked => Tile::StoneDoor,
            Tile::IronDoor => Tile::IronDoorLocked,
            Tile::IronDoorLocked => Tile::IronDoor,
            _ => return None,
        })
    }

    /// The work a workshop offers.
    pub fn recipe(self) -> Option<DesignationKind> {
        match self {
            Tile::Forge => Some(DesignationKind::Smelt),
            Tile::Anvil => Some(DesignationKind::Smith),
            _ => None,
        }
    }

    pub fn dig_ticks(self) -> u16 {
        match self {
            Tile::Rock | Tile::Wall | Tile::Bed | Tile::Table | Tile::Chair => 10,
            Tile::WoodBed | Tile::WoodTable | Tile::WoodChair => 10,
            Tile::Candle
            | Tile::Torch
            | Tile::Lamp
            | Tile::Campfire
            | Tile::Hearth
            | Tile::Stove
            | Tile::Forge
            | Tile::Anvil
            | Tile::WoodDoor
            | Tile::StoneDoor
            | Tile::IronDoor
            | Tile::WoodDoorLocked
            | Tile::StoneDoorLocked
            | Tile::IronDoorLocked => 10,
            Tile::Gold | Tile::IronOre => 15,
            Tile::Coal => 12,
            Tile::Mithril => 25,
            Tile::Soil | Tile::Slope => 8,
            Tile::Tree | Tile::Cow => 20,
            Tile::Crop => 10,
            Tile::Floor | Tile::Stair | Tile::Grass | Tile::Water | Tile::Sky | Tile::Field => 0,
        }
    }

    pub fn yield_(self) -> Resources {
        let mut r = Resources::default();
        match self {
            Tile::Rock => r.stone = 1,
            Tile::Gold => r.gold = 1,
            Tile::Mithril => r.mithril = 1,
            Tile::IronOre => r.iron_ore = 1,
            Tile::Coal => r.coal = 1,
            Tile::Tree => r.wood = 3,
            Tile::Cow => r.food = 4,
            Tile::Crop => r.food = 2,
            Tile::Wall => return DesignationKind::Wall.cost(),
            Tile::Bed => return DesignationKind::Bed.cost(),
            Tile::Table => return DesignationKind::Table.cost(),
            Tile::Chair => return DesignationKind::Chair.cost(),
            Tile::WoodBed => return DesignationKind::WoodBed.cost(),
            Tile::WoodTable => return DesignationKind::WoodTable.cost(),
            Tile::WoodChair => return DesignationKind::WoodChair.cost(),
            Tile::Candle => return DesignationKind::Candle.cost(),
            Tile::Torch => return DesignationKind::Torch.cost(),
            Tile::Lamp => return DesignationKind::Lamp.cost(),
            Tile::Campfire => return DesignationKind::Campfire.cost(),
            Tile::Hearth => return DesignationKind::Hearth.cost(),
            Tile::Stove => return DesignationKind::Stove.cost(),
            Tile::Forge => return DesignationKind::Forge.cost(),
            Tile::Anvil => return DesignationKind::Anvil.cost(),
            Tile::WoodDoor | Tile::WoodDoorLocked => return DesignationKind::WoodDoor.cost(),
            Tile::StoneDoor | Tile::StoneDoorLocked => return DesignationKind::StoneDoor.cost(),
            Tile::IronDoor | Tile::IronDoorLocked => return DesignationKind::IronDoor.cost(),
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
            Rock,
            Gold,
            Mithril,
            Floor,
            Stair,
            Wall,
            Bed,
            Table,
            Chair,
            Grass,
            Tree,
            Water,
            Slope,
            Soil,
            Sky,
            WoodBed,
            WoodTable,
            WoodChair,
            Candle,
            Torch,
            Lamp,
            Campfire,
            Hearth,
            Stove,
            IronOre,
            Coal,
            Forge,
            Anvil,
            WoodDoor,
            StoneDoor,
            IronDoor,
            WoodDoorLocked,
            StoneDoorLocked,
            IronDoorLocked,
            Cow,
            Field,
            Crop,
        ];
        assert_eq!(Tile::ALL, expected);
        for (i, t) in expected.iter().enumerate() {
            assert_eq!(postcard::to_allocvec(t).unwrap(), [i as u8], "{t:?}");
        }
    }

    #[test]
    fn designation_wire_encoding_is_unchanged() {
        use DesignationKind::*;
        // The variant order is the postcard byte on the wire and in saves.
        let expected = [
            Dig, Stairs, Wall, Bed, Table, Chair, Chop, WoodBed, WoodTable, WoodChair, Candle,
            Torch, Lamp, Campfire, Hearth, Stove, Forge, Anvil, WoodDoor, StoneDoor, IronDoor,
            Smelt, Smith, Farm,
        ];
        for (i, k) in expected.iter().enumerate() {
            assert_eq!(postcard::to_allocvec(k).unwrap(), [i as u8], "{k:?}");
        }
    }

    #[test]
    fn iron_chain_costs_and_recipes() {
        use DesignationKind as K;
        let r = |f: fn(&mut Resources)| {
            let mut r = Resources::default();
            f(&mut r);
            r
        };
        assert_eq!(K::Forge.cost(), r(|r| r.stone = 6));
        assert_eq!(K::Anvil.cost(), r(|r| r.iron = 3));
        assert_eq!(K::WoodDoor.cost(), r(|r| r.wood = 3));
        assert_eq!(K::StoneDoor.cost(), r(|r| r.stone = 3));
        assert_eq!(K::IronDoor.cost(), r(|r| r.iron_door = 1));
        assert_eq!(
            K::Smelt.cost(),
            r(|r| {
                r.iron_ore = 2;
                r.coal = 1
            })
        );
        assert_eq!(K::Smelt.product(), r(|r| r.iron = 1));
        assert_eq!(K::Smith.cost(), r(|r| r.iron = 2));
        assert_eq!(K::Smith.product(), r(|r| r.iron_door = 1));
        assert_eq!((K::Smelt.work_ticks(), K::Smith.work_ticks()), (30, 40));
        assert_eq!(Tile::Forge.recipe(), Some(K::Smelt));
        assert_eq!(Tile::Anvil.recipe(), Some(K::Smith));
        assert_eq!(Tile::Rock.recipe(), None);
        assert!(K::Smelt.builds().is_none() && K::Smelt.is_work());
    }

    #[test]
    fn ores_and_doors_dig_back() {
        assert_eq!(Tile::IronOre.yield_().iron_ore, 1);
        assert_eq!(Tile::Coal.yield_().coal, 1);
        assert_eq!(
            (Tile::IronOre.dig_ticks(), Tile::Coal.dig_ticks()),
            (15, 12)
        );
        assert_eq!(Tile::IronDoorLocked.yield_().iron_door, 1);
        assert_eq!(
            Tile::StoneDoorLocked.yield_(),
            DesignationKind::StoneDoor.cost()
        );
        assert_eq!(Tile::WoodDoor.toggled_door(), Some(Tile::WoodDoorLocked));
        assert_eq!(Tile::IronDoorLocked.toggled_door(), Some(Tile::IronDoor));
        assert_eq!(Tile::Forge.toggled_door(), None);
        for t in [
            Tile::Forge,
            Tile::Anvil,
            Tile::WoodDoor,
            Tile::IronDoorLocked,
        ] {
            assert!(t.is_furniture() && !t.is_stone(), "{t:?}");
        }
        assert!(Tile::IronOre.is_stone() && Tile::Coal.is_stone());
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
    fn light_costs() {
        use DesignationKind as K;
        let r = |stone, wood, gold| Resources {
            stone,
            wood,
            gold,
            ..Default::default()
        };
        assert_eq!(K::Candle.cost(), r(0, 1, 0));
        assert_eq!(K::Torch.cost(), r(0, 1, 0));
        assert_eq!(K::Lamp.cost(), r(1, 0, 1));
        assert_eq!(K::Campfire.cost(), r(0, 3, 0));
        assert_eq!(K::Hearth.cost(), r(4, 0, 0));
        assert_eq!(K::Stove.cost(), r(3, 2, 0));
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
    fn stone_is_solid_but_not_tree_or_furniture() {
        let stone: Vec<_> = Tile::ALL.into_iter().filter(|t| t.is_stone()).collect();
        use Tile::*;
        assert_eq!(stone, [Rock, Gold, Mithril, Wall, Soil, IronOre, Coal]);
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
