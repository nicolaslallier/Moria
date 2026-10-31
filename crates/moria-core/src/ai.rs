use crate::{DwarfId, Mountain, MountainId, TilePos};
use serde::{Deserialize, Serialize};

pub const SUBJECT_VIEW: &str = "moria.ai.view";
pub const SUBJECT_ASSIGN: &str = "moria.ai.assign";
/// Most dwarves in one view, hence most assignments in one reply.
pub const MAX_DWARVES_PER_VIEW: usize = 64;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DwarfView {
    pub id: DwarfId,
    pub pos: TilePos,
    pub idle: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AiView {
    pub mountain: Mountain,
    pub tick: u64,
    pub dwarves: Vec<DwarfView>,
    pub claimed: Vec<TilePos>,
    pub occupied: Vec<TilePos>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Assignments {
    pub mountain: MountainId,
    pub tick: u64,
    pub list: Vec<(DwarfId, TilePos)>,
}
