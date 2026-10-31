use crate::srd::{Abilities, Class, Race};
use crate::*;
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const MAX_OWNED: usize = 10;
/// Little-endian u32 at the start of world.bin; bump it when `World` changes shape.
pub const SAVE_VERSION: u32 = 1;
const GRID: i32 = 4;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Region {
    pub mountain: MountainId,
    pub world_x: i32,
    pub world_y: i32,
    pub owner: Option<PlayerId>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Account {
    pub player: PlayerId,
    pub password_hash: String,
    pub mountain: Option<MountainId>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DigJob {
    pub target: TilePos,
    pub kind: DesignationKind,
    pub progress: u16,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Hero {
    pub name: String,
    pub race: Race,
    pub class: Class,
    pub abilities: Abilities,
    pub pos: TilePos,
    pub dig: Option<DigJob>,
    pub move_cooldown: u8,
    /// Latest Move received during the cooldown, applied when it ends. Not saved.
    #[serde(skip)]
    pub pending_move: Option<Dir>,
    /// Dig or Build received while `pending_move` was set; started right after that move.
    /// Not saved.
    #[serde(skip)]
    pub pending_dig: Option<(Dir, DesignationKind)>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DwarfTask {
    pub target: TilePos,
    pub kind: DesignationKind,
    pub path: Vec<TilePos>,
    pub progress: u16,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Dwarf {
    pub id: DwarfId,
    pub pos: TilePos,
    pub task: Option<DwarfTask>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Fortress {
    pub owner: PlayerId,
    pub hero: Option<Hero>,
    pub dwarves: Vec<Dwarf>,
    pub resources: Resources,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct World {
    pub seed: u64,
    pub regions: Vec<Region>,
    pub mountains: BTreeMap<MountainId, Mountain>,
    pub fortresses: BTreeMap<MountainId, Fortress>,
    pub accounts: BTreeMap<String, Account>,
    pub next_dwarf: u32,
    pub tick: u64,
}

fn gen_mountain(id: MountainId, rng: &mut ChaCha8Rng) -> Mountain {
    let mut m = Mountain::new(id, MOUNTAIN_W, MOUNTAIN_H, MOUNTAIN_D, Tile::Rock);
    for z in 0..MOUNTAIN_D {
        let veins = if z >= 3 { 6 } else { 3 };
        for _ in 0..veins {
            vein(&mut m, rng, z, Tile::Gold);
        }
        if z >= 5 {
            for _ in 0..3 {
                vein(&mut m, rng, z, Tile::Mithril);
            }
        }
    }
    for x in 0..6 {
        for y in 22..26 {
            m.set(TilePos::new(x, y, 0), Tile::Floor);
        }
    }
    m
}

/// Seeded random walk of 6..14 tiles on one level; the entrance is stamped afterwards.
fn vein(m: &mut Mountain, rng: &mut ChaCha8Rng, z: u8, t: Tile) {
    let (mut x, mut y) = (
        rng.random_range(0..MOUNTAIN_W),
        rng.random_range(0..MOUNTAIN_H),
    );
    for _ in 0..rng.random_range(6..14) {
        m.set(TilePos::new(x, y, z), t);
        let d = Dir::HORIZONTAL[rng.random_range(0..4usize)];
        let p = TilePos::new(x, y, z).step(d);
        if m.in_bounds(p) {
            (x, y) = (p.x, p.y);
        }
    }
}

pub fn generate(seed: u64) -> World {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut w = World {
        seed,
        regions: vec![],
        mountains: BTreeMap::new(),
        fortresses: BTreeMap::new(),
        accounts: BTreeMap::new(),
        next_dwarf: 0,
        tick: 0,
    };
    for i in 0..(GRID * GRID) {
        let id = MountainId(i as u16);
        w.regions.push(Region {
            mountain: id,
            world_x: i % GRID,
            world_y: i / GRID,
            owner: None,
        });
        w.mountains.insert(id, gen_mountain(id, &mut rng));
    }
    w
}

pub fn claim_mountain(w: &mut World, player: PlayerId) -> Option<MountainId> {
    if w.regions.iter().filter(|r| r.owner.is_some()).count() >= MAX_OWNED {
        return None;
    }
    let r = w.regions.iter_mut().find(|r| r.owner.is_none())?;
    r.owner = Some(player);
    let id = r.mountain;
    let dwarves = (0..3)
        .map(|i| Dwarf {
            id: DwarfId(w.next_dwarf + i),
            pos: TilePos::new(i as i16, 22, 0),
            task: None,
        })
        .collect();
    w.next_dwarf += 3;
    w.fortresses.insert(
        id,
        Fortress {
            owner: player,
            hero: None,
            dwarves,
            resources: Resources::default(),
        },
    );
    Some(id)
}

pub fn save(w: &World, path: &Path) -> io::Result<()> {
    let mut bytes = SAVE_VERSION.to_le_bytes().to_vec();
    bytes.extend(postcard::to_allocvec(w).map_err(io::Error::other)?);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

pub fn load(path: &Path) -> io::Result<World> {
    let bytes = std::fs::read(path)?;
    let bad = |m: String| io::Error::new(io::ErrorKind::InvalidData, m);
    let Some((v, body)) = bytes.split_first_chunk::<4>() else {
        return Err(bad("fichier de sauvegarde tronqué".into()));
    };
    match u32::from_le_bytes(*v) {
        SAVE_VERSION => postcard::from_bytes(body).map_err(io::Error::other),
        n => Err(bad(format!("format de sauvegarde v{n} non supporté"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::encode;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moria-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn generate_is_deterministic() {
        assert_eq!(encode(&generate(42)), encode(&generate(42)));
        assert_ne!(encode(&generate(42)), encode(&generate(43)));
    }

    #[test]
    fn generated_mountain_has_entrance_and_ores() {
        let w = generate(7);
        assert_eq!(w.mountains.len(), 16);
        for m in w.mountains.values() {
            for x in 0..6 {
                for y in 22..26 {
                    assert_eq!(m.get(TilePos::new(x, y, 0)), Some(Tile::Floor));
                }
            }
            let (mut gold, mut mith) = (0, 0);
            for z in 0..MOUNTAIN_D {
                for y in 0..MOUNTAIN_H {
                    for x in 0..MOUNTAIN_W {
                        match m.get(TilePos::new(x, y, z)) {
                            Some(Tile::Gold) => gold += 1,
                            Some(Tile::Mithril) => {
                                assert!(z >= 5);
                                mith += 1
                            }
                            _ => {}
                        }
                    }
                }
            }
            assert!(gold > 0 && mith > 0);
        }
    }

    #[test]
    fn claim_gives_three_dwarves_on_floor_and_caps_at_ten() {
        let mut w = generate(1);
        for p in 0..10 {
            let id = claim_mountain(&mut w, PlayerId(p)).unwrap();
            let f = &w.fortresses[&id];
            assert_eq!(f.dwarves.len(), 3);
            assert!(f.hero.is_none());
            for (i, d) in f.dwarves.iter().enumerate() {
                assert!(w.mountains[&id].walkable(d.pos));
                assert!(
                    f.dwarves[..i]
                        .iter()
                        .all(|o| o.pos != d.pos && o.id != d.id)
                );
            }
        }
        assert_eq!(claim_mountain(&mut w, PlayerId(10)), None);
        assert_eq!(w.next_dwarf, 30);
    }

    #[test]
    fn save_load_roundtrip() {
        let mut w = generate(3);
        claim_mountain(&mut w, PlayerId(1));
        let p = tmpdir("rt").join("w.bin");
        save(&w, &p).unwrap();
        assert_eq!(load(&p).unwrap(), w);
    }

    #[test]
    fn save_has_version_and_unknown_version_is_refused() {
        let p = tmpdir("version").join("w.bin");
        save(&generate(3), &p).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(bytes[..4], SAVE_VERSION.to_le_bytes());
        let mut other = 2u32.to_le_bytes().to_vec();
        other.extend(&bytes[4..]);
        std::fs::write(&p, other).unwrap();
        let e = load(&p).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert_eq!(e.to_string(), "format de sauvegarde v2 non supporté");
        std::fs::write(&p, [1u8, 0]).unwrap(); // too short for a version
        assert!(load(&p).is_err());
    }

    #[test]
    fn save_is_atomic() {
        let a = generate(5);
        let p = tmpdir("atomic").join("w.bin");
        save(&a, &p).unwrap();
        std::fs::write(p.with_file_name("w.bin.tmp"), b"garbage").unwrap();
        assert_eq!(load(&p).unwrap(), a);
    }
}
