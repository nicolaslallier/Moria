use crate::srd::{Abilities, Class, Race};
use crate::{DesignationKind, Dir, DwarfId, Mountain, PlayerId, Resources, Tile, TilePos};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, Read};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME: usize = 1 << 20;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ClientMsg {
    Hello {
        version: u32,
        name: String,
        password: String,
        /// Required to create a new account when the server has an invite code.
        invite: Option<String>,
    },
    CreateHero {
        race: Race,
        class: Class,
    },
    Move(Dir),
    Dig(Dir),
    Designate {
        a: TilePos,
        b: TilePos,
        kind: DesignationKind,
    },
    CancelDesignation {
        a: TilePos,
        b: TilePos,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Reject {
    VersionMismatch { server: u32 },
    BadPassword,
    ServerFull,
    NoFreeMountain,
    InvalidName,
    BadInvite,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HeroView {
    pub name: String,
    pub race: Race,
    pub class: Class,
    pub abilities: Abilities,
    pub pos: TilePos,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FortressView {
    pub mountain: Mountain,
    pub hero: HeroView,
    pub dwarves: Vec<(DwarfId, TilePos)>,
    pub resources: Resources,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TickDelta {
    pub tick: u64,
    pub hero_pos: TilePos,
    pub dwarves: Vec<(DwarfId, TilePos)>,
    pub tiles: Vec<(TilePos, Tile)>,
    pub designations_added: Vec<(TilePos, DesignationKind)>,
    pub designations_removed: Vec<TilePos>,
    pub resources: Resources,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ServerMsg {
    Welcome { player: PlayerId, needs_hero: bool },
    Reject(Reject),
    Snapshot(Box<FortressView>),
    Tick(TickDelta),
}

/// u32 big-endian length prefix, then the postcard body.
pub fn encode(msg: &impl Serialize) -> Vec<u8> {
    let body = postcard::to_allocvec(msg).expect("postcard serialization");
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend(body);
    out
}

fn check_len(len: u32) -> io::Result<usize> {
    if len as usize > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    Ok(len as usize)
}

fn decode<T: DeserializeOwned>(buf: &[u8]) -> io::Result<T> {
    postcard::from_bytes(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn read_frame<T: DeserializeOwned>(r: &mut impl Read) -> io::Result<T> {
    let mut h = [0u8; 4];
    r.read_exact(&mut h)?;
    let mut buf = vec![0u8; check_len(u32::from_be_bytes(h))?];
    r.read_exact(&mut buf)?;
    decode(&buf)
}

#[cfg(feature = "tokio")]
pub async fn read_frame_async<T: DeserializeOwned>(
    r: &mut (impl tokio::io::AsyncRead + Unpin),
) -> io::Result<T> {
    use tokio::io::AsyncReadExt;
    let mut h = [0u8; 4];
    r.read_exact(&mut h).await?;
    let mut buf = vec![0u8; check_len(u32::from_be_bytes(h))?];
    r.read_exact(&mut buf).await?;
    decode(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frame_roundtrip() {
        let b = encode(&ClientMsg::Move(Dir::N));
        let m: ClientMsg = read_frame(&mut Cursor::new(b)).unwrap();
        assert_eq!(m, ClientMsg::Move(Dir::N));
    }
    #[test]
    fn oversized_frame_rejected() {
        let b = (MAX_FRAME as u32 + 1).to_be_bytes().to_vec();
        let e = read_frame::<ClientMsg>(&mut Cursor::new(b)).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    }
    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn async_roundtrip_and_oversized() {
        let b = encode(&ClientMsg::Dig(Dir::Down));
        let m: ClientMsg = read_frame_async(&mut &b[..]).await.unwrap();
        assert_eq!(m, ClientMsg::Dig(Dir::Down));
        let big = (MAX_FRAME as u32 + 1).to_be_bytes();
        let e = read_frame_async::<ClientMsg>(&mut &big[..])
            .await
            .unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    }
}
