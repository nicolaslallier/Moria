//! Food bus messages (postcard). Decoding is bounded: oversized or garbage input yields `None`.
use crate::MountainId;
use crate::food::{Cell, FoodAction, FoodEvent};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const KEY_TICK: &str = "food.tick";
pub const KEY_WORLD: &str = "food.world";
pub const KEY_ACTION: &str = "food.action";
pub const KEY_REJECT: &str = "food.reject";
pub const KEY_EVENTS: &str = "food.events";
pub const KEY_STATE: &str = "food.state";
pub const MAX_FOOD_WORLD_BYTES: usize = 262_144;
pub const MAX_FOOD_MSG_BYTES: usize = 16_384;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Tick {
    pub tick: u64,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct World {
    pub tick: u64,
    pub mountains: Vec<(MountainId, Vec<Cell>)>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Actions {
    pub tick: u64,
    pub mountain: MountainId,
    pub list: Vec<FoodAction>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Rejects {
    pub tick: u64,
    pub mountain: MountainId,
    pub list: Vec<FoodEvent>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Events {
    pub tick: u64,
    pub mountain: MountainId,
    pub list: Vec<FoodEvent>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Status {
    pub tick: u64,
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8], max: usize) -> Option<T> {
    if bytes.len() > max {
        return None;
    }
    postcard::from_bytes(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::food::{CellKind, HERD_CAP, Species};
    use crate::{MOUNTAIN_D, MOUNTAIN_H, MOUNTAIN_W, TilePos};

    fn p(x: i16, y: i16) -> TilePos {
        TilePos { x, y, z: 0 }
    }
    fn enc<T: Serialize>(t: &T) -> Vec<u8> {
        postcard::to_allocvec(t).unwrap()
    }
    const M: MountainId = MountainId(3);

    #[test]
    fn roundtrip_every_message() {
        let m = MAX_FOOD_MSG_BYTES;
        let ev = vec![
            FoodEvent::AnimalBorn {
                species: Species::Sheep,
                at: p(1, 2),
            },
            FoodEvent::CropRipe { at: p(3, 4) },
        ];
        let t = Tick { tick: 7 };
        assert_eq!(decode::<Tick>(&enc(&t), m), Some(t));
        let w = World {
            tick: 7,
            mountains: vec![(
                M,
                vec![Cell {
                    pos: p(1, 2),
                    kind: CellKind::Cow,
                }],
            )],
        };
        assert_eq!(decode::<World>(&enc(&w), MAX_FOOD_WORLD_BYTES), Some(w));
        let a = Actions {
            tick: 7,
            mountain: M,
            list: vec![FoodAction::FieldSown(p(5, 6))],
        };
        assert_eq!(decode::<Actions>(&enc(&a), m), Some(a));
        let r = Rejects {
            tick: 7,
            mountain: M,
            list: ev.clone(),
        };
        assert_eq!(decode::<Rejects>(&enc(&r), m), Some(r));
        let e = Events {
            tick: 7,
            mountain: M,
            list: ev,
        };
        assert_eq!(decode::<Events>(&enc(&e), m), Some(e));
        let s = Status { tick: 7 };
        assert_eq!(decode::<Status>(&enc(&s), m), Some(s));
    }

    #[test]
    fn decode_rejects_oversized_before_decoding() {
        let bytes = enc(&Tick { tick: 7 });
        assert!(decode::<Tick>(&bytes, bytes.len()).is_some());
        assert!(decode::<Tick>(&bytes, bytes.len() - 1).is_none());
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode::<World>(&[0xff; 64], MAX_FOOD_WORLD_BYTES).is_none());
        assert!(decode::<Tick>(&[], MAX_FOOD_MSG_BYTES).is_none());
    }

    #[test]
    fn max_world_fits_in_the_limit() {
        let cells = |n: usize| {
            (0..n)
                .map(|i| {
                    let kind = if i < HERD_CAP {
                        CellKind::Cow
                    } else {
                        CellKind::Field
                    };
                    let (x, y) = (i as i16 % MOUNTAIN_W, i as i16 % MOUNTAIN_H);
                    Cell {
                        pos: TilePos {
                            x,
                            y,
                            z: MOUNTAIN_D - 1,
                        },
                        kind,
                    }
                })
                .collect::<Vec<_>>()
        };
        let w = World {
            tick: u64::MAX,
            mountains: (0..10)
                .map(|i| (MountainId(i), cells(HERD_CAP + 4096)))
                .collect(),
        };
        let b = enc(&w);
        assert!(b.len() <= MAX_FOOD_WORLD_BYTES, "{}", b.len());
        assert!(decode::<World>(&b, MAX_FOOD_WORLD_BYTES).is_some());
    }
}
