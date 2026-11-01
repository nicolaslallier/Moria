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
pub const SAVE_VERSION: u32 = 2;
const GRID: i32 = 4;
/// The entrance room, on `GROUND_Z`; it opens west onto the grass at x = 127.
pub const ENTRANCE_X: std::ops::Range<i16> = 128..134;
pub const ENTRANCE_Y: std::ops::Range<i16> = 46..50;

pub fn entrance_tiles() -> impl Iterator<Item = TilePos> {
    ENTRANCE_Y.flat_map(|y| ENTRANCE_X.map(move |x| TilePos::new(x, y, GROUND_Z)))
}

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

/// One fortress map. Outdoors (x < 128): grass with trees on `GROUND_Z`, 2 levels of soil,
/// then rock; a lake ~100 tiles west of the entrance fed by a river from the mountain's
/// foot; 1-2 soil hills with slopes. The mountain (x >= 128): rock underground and a rock
/// mass above ground that rises eastward. Trails (tree-free) link the entrance to the lake
/// and to each hill's slope, so the forest and the hills are always reachable on foot.
fn gen_mountain(id: MountainId, rng: &mut ChaCha8Rng) -> Mountain {
    use Tile::*;
    let g = GROUND_Z;
    let foot = ENTRANCE_X.start;
    let trail_y = ENTRANCE_Y.start + 1;
    let mut m = Mountain::new(id, MOUNTAIN_W, MOUNTAIN_H, MOUNTAIN_D, Sky);
    for z in 0..MOUNTAIN_D {
        for y in 0..MOUNTAIN_H {
            for x in 0..MOUNTAIN_W {
                // One level higher every 12 tiles east of the foot, up to z 0.
                let top = if x < foot {
                    g
                } else {
                    (g - 1).saturating_sub(((x - foot) / 12) as u8)
                };
                let t = match z {
                    _ if z < top => continue,
                    _ if x >= foot => Rock,
                    _ if z == g => Grass,
                    _ if z <= g + 2 => Soil,
                    _ => Rock,
                };
                m.set(TilePos::new(x, y, z), t);
            }
        }
    }
    for z in g..MOUNTAIN_D {
        for _ in 0..12 {
            vein(&mut m, rng, foot..MOUNTAIN_W, z, Gold);
        }
        if z >= g + 5 {
            for _ in 0..6 {
                vein(&mut m, rng, foot..MOUNTAIN_W, z, Mithril);
            }
        }
        if z > g + 2 {
            for _ in 0..6 {
                vein(&mut m, rng, 0..foot, z, Gold);
            }
        }
    }
    let ground = |x, y| TilePos::new(x, y, g);
    let disc = |cx: i16, cy: i16, r: i16| {
        (cy - r..=cy + r)
            .flat_map(move |y| (cx - r..=cx + r).map(move |x| (x, y)))
            .filter(move |&(x, y)| (x - cx).pow(2) + (y - cy).pow(2) <= r * r)
    };

    // Lake; its east shore stays at x <= 40 on the entrance rows.
    let (lx, ly, lr) = (
        rng.random_range(26..=28),
        rng.random_range(46..=50),
        rng.random_range(10..=12),
    );
    for (x, y) in disc(lx, ly, lr) {
        m.set(ground(x, y), Water);
    }
    // River: from the foot at y ~70 into the lake's south-east shore, wobbling +-3 rows,
    // so it always stays south of the entrance trail.
    let (tx, ty) = (lx + lr * 7 / 10, ly + lr * 7 / 10);
    let (mut wobble, mut prev) = (0i16, None);
    for x in (tx..foot).rev() {
        wobble = (wobble + rng.random_range(-1..=1)).clamp(-3, 3);
        let y = ty + (70 - ty) * (x - tx) / (foot - 1 - tx) + wobble;
        let p = prev.unwrap_or(y);
        for yy in y.min(p)..=y.max(p) + 1 {
            m.set(ground(x, yy), Water);
        }
        prev = Some(y);
    }

    // Tree-free tiles: the entrance approach and the trails.
    let mut clear = vec![false; MOUNTAIN_W as usize * MOUNTAIN_H as usize];
    let mut keep_clear =
        |x: i16, y: i16| clear[y as usize * MOUNTAIN_W as usize + x as usize] = true;
    for y in ENTRANCE_Y {
        for x in foot - 4..foot {
            keep_clear(x, y);
        }
    }
    for x in (0..foot)
        .rev()
        .take_while(|&x| m.get(ground(x, trail_y)) == Some(Grass))
    {
        keep_clear(x, trail_y);
    }

    // Hills: a soil disc topped with grass, tall ones with a second tier; a slope on the
    // ground at the west foot, and one on the first tier for the second.
    let bands = [55..71, 92..111];
    let picks = if rng.random_bool(0.5) {
        vec![0, 1]
    } else {
        vec![rng.random_range(0..2)]
    };
    for i in picks {
        let (cx, cy, r) = (
            rng.random_range(bands[i].clone()),
            rng.random_range(14..=30),
            rng.random_range(5..=8),
        );
        let tall = rng.random_bool(0.5);
        for (x, y) in disc(cx, cy, r) {
            let h = if tall && (x - cx).pow(2) + (y - cy).pow(2) <= (r / 2).pow(2) {
                2
            } else {
                1
            };
            for k in 0..h {
                m.set(TilePos::new(x, y, g - k), Soil);
            }
            m.set(TilePos::new(x, y, g - h), Grass);
        }
        m.set(ground(cx - r - 1, cy), Slope);
        if tall {
            m.set(TilePos::new(cx - r / 2 - 1, cy, g - 1), Slope);
        }
        for y in cy..=trail_y {
            keep_clear(cx - r - 1, y);
        }
    }

    // Trees: sparse near the entrance, denser toward the lake (capped well below the
    // ~40% at which random trees would wall off parts of the forest).
    for y in 0..MOUNTAIN_H {
        for x in 0..foot {
            if clear[y as usize * MOUNTAIN_W as usize + x as usize]
                || m.get(ground(x, y)) != Some(Grass)
            {
                continue;
            }
            let d = f64::from((x - foot).pow(2) + (y - trail_y).pow(2)).sqrt();
            let density = 0.03 + 0.27 * ((d - 20.0) / 80.0).clamp(0.0, 1.0);
            if rng.random_bool(density) {
                m.set(ground(x, y), Tree);
            }
        }
    }
    for p in entrance_tiles() {
        m.set(p, Floor);
    }
    m
}

/// Seeded random walk of 6..14 tiles on level `z` within columns `xs`; only replaces rock.
fn vein(m: &mut Mountain, rng: &mut ChaCha8Rng, xs: std::ops::Range<i16>, z: u8, t: Tile) {
    let (mut x, mut y) = (
        rng.random_range(xs.clone()),
        rng.random_range(0..MOUNTAIN_H),
    );
    for _ in 0..rng.random_range(6..14) {
        let p = TilePos::new(x, y, z);
        if m.get(p) == Some(Tile::Rock) {
            m.set(p, t);
        }
        let n = p.step(Dir::HORIZONTAL[rng.random_range(0..4usize)]);
        if m.in_bounds(n) && xs.contains(&n.x) {
            (x, y) = (n.x, n.y);
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
    let dwarves = entrance_tiles()
        .take(3)
        .zip(w.next_dwarf..)
        .map(|(pos, i)| Dwarf {
            id: DwarfId(i),
            pos,
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

    #[test]
    fn worst_case_snapshot_fits_in_a_frame() {
        use crate::protocol::{FortressView, HeroView, MAX_FRAME};
        use crate::srd::{Abilities, Class, Race};
        use crate::{DesignationKind, Resources, TilePos};
        let mut mountain = generate(1).mountains.remove(&MountainId(0)).unwrap();
        let (w, h, d) = mountain.size();
        for z in 0..d {
            let (a, b) = (TilePos::new(0, 0, z), TilePos::new(w - 1, h - 1, z));
            mountain.designate(a, b, DesignationKind::Dig);
            mountain.designate(a, b, DesignationKind::Chop);
        }
        assert_eq!(mountain.designations.len(), crate::MAX_DESIGNATIONS);
        let view = FortressView {
            mountain,
            hero: HeroView {
                name: "x".repeat(32),
                race: Race::Dwarf,
                class: Class::Barbarian,
                abilities: Abilities {
                    str: 10,
                    dex: 10,
                    con: 10,
                    int: 10,
                    wis: 10,
                    cha: 10,
                },
                pos: TilePos::new(0, 0, 0),
            },
            dwarves: vec![],
            resources: Resources::default(),
            tick: 0,
        };
        let n = encode(&view).len();
        assert!(n < MAX_FRAME, "{n} >= {MAX_FRAME}");
    }

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

    fn tiles(m: &Mountain) -> impl Iterator<Item = (TilePos, Tile)> + '_ {
        let (w, h, d) = m.size();
        (0..d).flat_map(move |z| {
            (0..h).flat_map(move |y| {
                (0..w).map(move |x| {
                    let p = TilePos::new(x, y, z);
                    (p, m.get(p).unwrap())
                })
            })
        })
    }

    fn reach(m: &Mountain) -> std::collections::HashMap<TilePos, u32> {
        let e = entrance_tiles().next().unwrap();
        crate::path::distances_from(m, e, &Default::default())
    }

    #[test]
    fn new_world_dimensions() {
        let w = generate(7);
        assert_eq!(w.mountains.len(), 16);
        for m in w.mountains.values() {
            assert_eq!(m.size(), (176, 96, 12));
        }
    }

    #[test]
    fn entrance_opens_on_grass() {
        assert_eq!(entrance_tiles().count(), 24);
        for m in generate(7).mountains.values() {
            for p in entrance_tiles() {
                assert_eq!(p.z, GROUND_Z);
                assert!(m.walkable(p), "{p:?}");
            }
            assert_eq!(m.get(TilePos::new(127, 47, GROUND_Z)), Some(Tile::Grass));
        }
    }

    #[test]
    fn forest_reachable_from_entrance() {
        for seed in [1, 7, 42] {
            for m in generate(seed).mountains.values() {
                let dist = reach(m);
                let n = tiles(m)
                    .filter(|&(p, t)| {
                        t == Tile::Tree && {
                            let g = crate::path::work_spot_goal(m, p);
                            Dir::HORIZONTAL
                                .iter()
                                .any(|&d| g(p.step(d)) && dist.contains_key(&p.step(d)))
                        }
                    })
                    .count();
                assert!(n >= 50, "seed {seed} {:?}: {n} trees", m.id);
            }
        }
    }

    /// The lake's east shore, on the entrance rows, is ~90 tiles from the entrance.
    #[test]
    fn lake_about_100_tiles_away() {
        for m in generate(7).mountains.values() {
            let shore = tiles(m)
                .filter(|&(p, t)| t == Tile::Water && p.x < 60 && ENTRANCE_Y.contains(&p.y))
                .map(|(p, _)| p.x)
                .max();
            assert!(shore.is_some_and(|x| x <= 40), "{:?}: {shore:?}", m.id);
            assert_eq!(m.get(TilePos::new(28, 48, GROUND_Z)), Some(Tile::Water));
        }
    }

    #[test]
    fn hill_reachable_by_slope() {
        for seed in [1, 7, 42] {
            for m in generate(seed).mountains.values() {
                let dist = reach(m);
                assert!(
                    dist.keys()
                        .any(|&p| p.z < GROUND_Z && m.get(p) == Some(Tile::Grass)),
                    "seed {seed} {:?}",
                    m.id
                );
            }
        }
    }

    #[test]
    fn sky_above_ground_outdoors() {
        for m in generate(7).mountains.values() {
            assert_eq!(m.get(TilePos::new(60, 10, 0)), Some(Tile::Sky));
        }
    }

    #[test]
    fn mithril_only_deep() {
        for m in generate(7).mountains.values() {
            let (mut gold, mut mith) = (0, 0);
            for (p, t) in tiles(m) {
                match t {
                    Tile::Gold => gold += 1,
                    Tile::Mithril => {
                        assert!(p.z >= GROUND_Z + 5, "{p:?}");
                        mith += 1
                    }
                    _ => {}
                }
            }
            assert!(gold > 0 && mith > 0);
        }
    }

    /// NATS refuses messages over its max_payload (1 MiB by default); keep half of it spare.
    #[test]
    fn worst_case_ai_view_fits_in_half_a_nats_message() {
        use crate::ai::{AiView, DwarfView, MAX_DWARVES_PER_VIEW};
        use crate::{DesignationKind, DwarfId, TilePos};
        let mut mountain = generate(1).mountains.remove(&MountainId(0)).unwrap();
        let (w, h, d) = mountain.size();
        for z in 0..d {
            let (a, b) = (TilePos::new(0, 0, z), TilePos::new(w - 1, h - 1, z));
            mountain.designate(a, b, DesignationKind::Dig);
        }
        let far = TilePos::new(w - 1, h - 1, d - 1);
        let view = AiView {
            mountain,
            tick: u64::MAX,
            dwarves: (0..MAX_DWARVES_PER_VIEW as u32)
                .map(|i| DwarfView {
                    id: DwarfId(i),
                    pos: far,
                    idle: true,
                })
                .collect(),
            claimed: vec![far; MAX_DWARVES_PER_VIEW],
            occupied: vec![far; MAX_DWARVES_PER_VIEW + 1],
        };
        let n = postcard::to_allocvec(&view).unwrap().len();
        assert!(n < 512 * 1024, "{n}");
    }

    #[test]
    fn full_map_encodes_under_max_frame() {
        use crate::protocol::{FortressView, HeroView, MAX_FRAME};
        use crate::srd::{Class, Race};
        let mut w = generate(7);
        let mountain = w.mountains.remove(&MountainId(0)).unwrap();
        let v = FortressView {
            mountain,
            hero: HeroView {
                name: "h".into(),
                race: Race::Dwarf,
                class: Class::Barbarian,
                abilities: crate::srd::roll_abilities(Race::Dwarf, Class::Barbarian),
                pos: entrance_tiles().next().unwrap(),
            },
            dwarves: vec![],
            resources: Resources::default(),
            tick: 0,
        };
        let n = encode(&v).len();
        assert!(n < MAX_FRAME / 2, "{n}");
    }

    #[test]
    fn v1_save_is_refused() {
        let p = tmpdir("v1").join("w.bin");
        let mut bytes = 1u32.to_le_bytes().to_vec();
        bytes.extend([0u8; 16]);
        std::fs::write(&p, bytes).unwrap();
        let e = load(&p).unwrap_err();
        assert!(e.to_string().contains("non supporté"), "{e}");
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
                assert_eq!(Some(d.pos), entrance_tiles().nth(i));
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
        let mut other = 3u32.to_le_bytes().to_vec();
        other.extend(&bytes[4..]);
        std::fs::write(&p, other).unwrap();
        let e = load(&p).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert_eq!(e.to_string(), "format de sauvegarde v3 non supporté");
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
