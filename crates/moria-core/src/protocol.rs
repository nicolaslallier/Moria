use crate::srd::{Abilities, Class, Race};
use crate::{DesignationKind, Dir, DwarfId, Mountain, PlayerId, Resources, Tile, TilePos};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, Read};

/// Frozen across all protocol versions, so any server can answer any client with a
/// readable version error: `ClientMsg::Hello` stays variant 0 with `version` as its first
/// field, `ServerMsg::Reject` stays variant 1 and `Reject::VersionMismatch` stays variant 0
/// (`{ server: u32 }`). Postcard encodes a variant index as a varint u32, so a Hello
/// starts with the bytes of `(0u32, version)` whatever follows.
pub const PROTOCOL_VERSION: u32 = 5;
pub const MAX_FRAME: usize = 1 << 20;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ClientMsg {
    Hello {
        version: u32,
        name: String,
        password: String,
        /// Required to create a new account when the server has an invite code.
        invite: Option<String>,
        /// Create the account if the name is unknown. For a known name it is a plain login.
        create: bool,
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
    /// Hero builds `kind` (wall or furniture) on the floor tile toward `Dir`, for 1 stone.
    Build(Dir, DesignationKind),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Reject {
    VersionMismatch {
        server: u32,
    },
    /// Wrong password, or a login (not creation) with an unknown name: the two are
    /// indistinguishable so names cannot be enumerated.
    BadPassword,
    ServerFull,
    NoFreeMountain,
    InvalidName,
    BadInvite,
    /// Password under 8 characters at account creation.
    WeakPassword,
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
    /// What each busy dwarf is doing; idle dwarves are absent.
    pub dwarf_jobs: Vec<(DwarfId, DesignationKind)>,
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

/// Decodes a frame body (e.g. one read with `read_frame_bytes_async`).
pub fn decode<T: DeserializeOwned>(buf: &[u8]) -> io::Result<T> {
    postcard::from_bytes(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn read_frame<T: DeserializeOwned>(r: &mut impl Read) -> io::Result<T> {
    let mut h = [0u8; 4];
    r.read_exact(&mut h)?;
    let mut buf = vec![0u8; check_len(u32::from_be_bytes(h))?];
    r.read_exact(&mut buf)?;
    decode(&buf)
}

/// The raw body of one frame, undecoded.
#[cfg(feature = "tokio")]
pub async fn read_frame_bytes_async(
    r: &mut (impl tokio::io::AsyncRead + Unpin),
) -> io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut h = [0u8; 4];
    r.read_exact(&mut h).await?;
    let mut buf = vec![0u8; check_len(u32::from_be_bytes(h))?];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(feature = "tokio")]
pub async fn read_frame_async<T: DeserializeOwned>(
    r: &mut (impl tokio::io::AsyncRead + Unpin),
) -> io::Result<T> {
    decode(&read_frame_bytes_async(r).await?)
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
    /// Guards the layout frozen in the `PROTOCOL_VERSION` doc comment.
    #[test]
    fn frozen_positions() {
        let hello = ClientMsg::Hello {
            version: 300,
            name: "x".into(),
            password: String::new(),
            invite: None,
            create: false,
        };
        let b = postcard::to_allocvec(&hello).unwrap();
        let (peek, _) = postcard::take_from_bytes::<(u32, u32)>(&b).unwrap();
        assert_eq!(peek, (0, 300));
        let r = postcard::to_allocvec(&ServerMsg::Reject(Reject::VersionMismatch { server: 1 }))
            .unwrap();
        assert_eq!(r, [1, 0, 1]);
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
