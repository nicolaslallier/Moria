//! Le roi nain : messages du bus entre le serveur et `moria-king`, catalogue de quêtes.
use crate::{MountainId, Resources, TilePos};
use serde::{Deserialize, Serialize};

pub const KEY_ASK: &str = "king.ask";
pub const KEY_REPLY: &str = "king.reply";
pub const MAX_TALK_CHARS: usize = 280;
pub const MAX_REPLY_CHARS: usize = 1000;
pub const HISTORY_LEN: usize = 6;
pub const FALLBACK_FR: &str = "Le roi est perdu dans ses pensées.";
pub const FALLBACK_EN: &str = "The king is lost in thought.";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum QuestId {
    ForgeIngots,
    ForgeDoors,
    Food,
    Depth,
}

impl QuestId {
    pub const ALL: [QuestId; 4] = [
        QuestId::ForgeIngots,
        QuestId::ForgeDoors,
        QuestId::Food,
        QuestId::Depth,
    ];

    pub fn key(self) -> &'static str {
        match self {
            QuestId::ForgeIngots => "forge_ingots",
            QuestId::ForgeDoors => "forge_doors",
            QuestId::Food => "food",
            QuestId::Depth => "depth",
        }
    }

    pub fn from_key(k: &str) -> Option<QuestId> {
        Self::ALL.into_iter().find(|q| q.key() == k)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuestDef {
    pub id: QuestId,
    pub target: u32,
    pub reward: Resources,
}

pub fn quest_def(id: QuestId) -> QuestDef {
    let r = Resources::default();
    let (target, reward) = match id {
        QuestId::ForgeIngots => (
            5,
            Resources {
                stone: 40,
                gold: 20,
                ..r
            },
        ),
        QuestId::ForgeDoors => (
            2,
            Resources {
                iron: 4,
                gold: 30,
                ..r
            },
        ),
        QuestId::Food => (
            20,
            Resources {
                iron: 3,
                stone: 30,
                ..r
            },
        ),
        // Cible en niveaux sous `GROUND_Z`.
        QuestId::Depth => (
            4,
            Resources {
                gold: 40,
                mithril: 5,
                ..r
            },
        ),
    };
    QuestDef { id, target, reward }
}

/// Quête acceptée ; `baseline` = compteur de production à l'acceptation, utilisé par les
/// seules quêtes de forge (0 pour les autres).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quest {
    pub id: QuestId,
    pub baseline: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Exchange {
    pub player: String,
    pub king: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Fr,
    En,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct KingState {
    pub resources: Resources,
    pub dwarves: u32,
    pub depth: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Ask {
    pub mountain: MountainId,
    pub seq: u64,
    pub lang: Lang,
    pub text: String,
    pub history: Vec<Exchange>,
    pub state: KingState,
    pub active: Option<QuestId>,
    pub offered: Vec<QuestId>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Reply {
    pub mountain: MountainId,
    pub seq: u64,
    pub text: String,
    pub offer: Option<QuestId>,
}

/// Le héros doit être sur le même niveau, à une case (diagonale comprise) du roi.
pub fn king_in_reach(king: TilePos, hero: TilePos) -> bool {
    king.z == hero.z && king.x.abs_diff(hero.x) <= 1 && king.y.abs_diff(hero.y) <= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: i16, y: i16, z: u8) -> TilePos {
        TilePos { x, y, z }
    }

    #[test]
    fn quest_keys_round_trip() {
        for id in QuestId::ALL {
            assert_eq!(QuestId::from_key(id.key()), Some(id));
        }
        assert_eq!(QuestId::from_key("nope"), None);
    }

    #[test]
    fn every_quest_has_a_positive_target_and_a_reward() {
        for id in QuestId::ALL {
            let d = quest_def(id);
            assert_eq!(d.id, id);
            assert!(d.target > 0);
            assert_ne!(d.reward, Resources::default());
        }
    }

    #[test]
    fn ask_and_reply_round_trip_through_postcard() {
        let ask = Ask {
            mountain: MountainId(3),
            seq: 7,
            lang: Lang::Fr,
            text: "Salut".into(),
            history: vec![Exchange {
                player: "a".into(),
                king: "b".into(),
            }],
            state: KingState {
                resources: Resources::default(),
                dwarves: 5,
                depth: 2,
            },
            active: Some(QuestId::Food),
            offered: vec![QuestId::Depth],
        };
        let b = postcard::to_allocvec(&ask).unwrap();
        assert_eq!(postcard::from_bytes::<Ask>(&b).unwrap(), ask);
        let r = Reply {
            mountain: MountainId(3),
            seq: 7,
            text: "Oui".into(),
            offer: Some(QuestId::Depth),
        };
        let b = postcard::to_allocvec(&r).unwrap();
        assert_eq!(postcard::from_bytes::<Reply>(&b).unwrap(), r);
    }

    #[test]
    fn king_in_reach_needs_same_z_and_distance_one() {
        let k = pos(5, 5, 0);
        assert!(king_in_reach(k, pos(5, 5, 0)));
        assert!(king_in_reach(k, pos(6, 6, 0)));
        assert!(!king_in_reach(k, pos(7, 5, 0)));
        assert!(!king_in_reach(k, pos(5, 5, 1)));
    }
}
