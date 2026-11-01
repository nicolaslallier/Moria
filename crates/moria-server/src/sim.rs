//! Authoritative, pure, synchronous simulation. No I/O, no async: the main loop calls
//! `apply_command` / `apply_assignments` between ticks and `step` once per tick.

use moria_core::ai::Assignments;
use moria_core::path::{find_path, work_spot_goal};
use moria_core::protocol::{ClientMsg, DwarfJob, DwarfState, TickDelta};
use moria_core::srd::hero_dig_ticks;
use moria_core::world::{DigJob, Dwarf, DwarfTask, Fortress, Hero, World};
use moria_core::{
    DesignationKind, Dir, GROUND_Z, MAX_DESIGNATIONS, Mountain, MountainId, Resources, Tile,
    TilePos,
};
use std::collections::{BTreeMap, HashSet};

const MOVE_COOLDOWN: u8 = 2;
const STAIRS_EXTRA_TICKS: u16 = 10;
const WALL_BUILD_TICKS: u16 = 15;
/// Ticks between two grazing steps of the herd.
const COW_EVERY: u64 = 10;
const ALL_DIRS: [Dir; 6] = [Dir::N, Dir::S, Dir::E, Dir::W, Dir::Up, Dir::Down];

/// The dwarf as clients see it: an empty path means it is at its work spot.
pub fn dwarf_view(d: &Dwarf) -> DwarfState {
    let job = d.task.as_ref().map(|t| DwarfJob {
        kind: t.kind,
        working: t.path.is_empty(),
    });
    (d.id, d.pos, job)
}

/// `ClientMsg` minus the handshake messages.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Move(Dir),
    Dig(Dir),
    /// Always a building kind (`DesignationKind::builds` is `Some`).
    Build(Dir, DesignationKind),
    Designate {
        a: TilePos,
        b: TilePos,
        kind: DesignationKind,
    },
    Cancel {
        a: TilePos,
        b: TilePos,
    },
    Use(Dir),
    Order {
        dir: Dir,
        add: u8,
    },
}

impl Command {
    pub fn from_client(m: ClientMsg) -> Option<Command> {
        Some(match m {
            ClientMsg::Move(d) => Command::Move(d),
            ClientMsg::Dig(d) => Command::Dig(d),
            ClientMsg::Build(d, k) if k.builds().is_some() => Command::Build(d, k),
            ClientMsg::Build(..) => return None,
            ClientMsg::Designate { kind, .. } if kind.is_work() => return None,
            ClientMsg::Designate { a, b, kind } => Command::Designate { a, b, kind },
            ClientMsg::CancelDesignation { a, b } => Command::Cancel { a, b },
            ClientMsg::Use(d) => Command::Use(d),
            ClientMsg::Order { dir, add } => Command::Order { dir, add },
            ClientMsg::Hello { .. } | ClientMsg::CreateHero { .. } => return None,
        })
    }
}

/// Changes caused by commands, folded into that tick's delta. Designations per tile:
/// `Some(kind)` = designated, `None` = removed; the last write wins, for orders too.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    pub designations: BTreeMap<TilePos, Option<DesignationKind>>,
    pub tiles: Vec<(TilePos, Tile)>,
    pub orders: BTreeMap<TilePos, u8>,
}

impl Changes {
    /// Folds a `later` command's changes into `self`; the last write on a tile wins.
    pub fn then(&mut self, later: Changes) {
        self.designations.extend(later.designations);
        self.tiles.extend(later.tiles);
        self.orders.extend(later.orders);
    }

    /// Merges into the tick delta. These changes happened before `step`, so whatever `d`
    /// already says about a tile wins; the result never lists a tile in both lists.
    pub fn merge_into(self, d: &mut TickDelta) {
        let seen: HashSet<TilePos> = d
            .designations_added
            .iter()
            .map(|a| a.0)
            .chain(d.designations_removed.iter().copied())
            .collect();
        for (p, k) in self
            .designations
            .into_iter()
            .filter(|(p, _)| !seen.contains(p))
        {
            match k {
                Some(k) => d.designations_added.push((p, k)),
                None => d.designations_removed.push(p),
            }
        }
        // Command tiles go first: the step's later writes win on the client.
        d.tiles.splice(0..0, self.tiles);
        let reported: HashSet<TilePos> = d.orders.iter().map(|o| o.0).collect();
        d.orders.extend(
            self.orders
                .into_iter()
                .filter(|(p, _)| !reported.contains(p)),
        );
    }
}

/// Applies one client command to its own mountain. Unknown mountains, mountains without a
/// hero, and anything out of range are ignored.
pub fn apply_command(w: &mut World, m: MountainId, c: Command) -> Changes {
    let mut ch = Changes::default();
    let (Some(mt), Some(f)) = (w.mountains.get_mut(&m), w.fortresses.get_mut(&m)) else {
        return ch;
    };
    let Some(h) = f.hero.as_mut() else {
        return ch;
    };
    match c {
        Command::Move(d) if h.move_cooldown > 0 => h.pending_move = Some(d),
        Command::Move(d) => hero_move(h, mt, d),
        // Behind a buffered move, the dig waits for it (the cooldown still applies).
        Command::Dig(d) | Command::Build(d, _) if h.pending_move.is_some() => {
            h.pending_dig = Some((d, hero_kind(&c)));
        }
        Command::Dig(d) | Command::Build(d, _) => {
            let kind = hero_kind(&c);
            if hero_spare(f).covers(&kind.cost()) {
                start_dig(f.hero.as_mut().unwrap(), mt, d, kind);
            }
        }
        Command::Designate { a, b, kind } => {
            let rect = mt.rect(a, b);
            let before: Vec<_> = rect
                .iter()
                .map(|p| mt.designations.get(p).copied())
                .collect();
            mt.designate(a, b, kind);
            for (p, old) in rect.into_iter().zip(before) {
                if old != Some(kind) && mt.designations.get(&p) == Some(&kind) {
                    ch.designations.insert(p, Some(kind));
                }
            }
        }
        Command::Use(d) if !matches!(d, Dir::Up | Dir::Down) => {
            let t = h.pos.step(d);
            let free = f.dwarves.iter().all(|w| w.pos != t);
            if free && let Some(new) = mt.get(t).and_then(Tile::toggled_door) {
                mt.set(t, new);
                ch.tiles.push((t, new));
            }
        }
        Command::Order { dir, add } if !matches!(dir, Dir::Up | Dir::Down) => {
            let t = h.pos.step(dir);
            if mt.get(t).and_then(Tile::recipe).is_some() {
                let old = mt.orders.remove(&t).unwrap_or(0);
                let n = if add == 0 {
                    0
                } else {
                    old.saturating_add(add).min(99)
                };
                if n > 0 {
                    mt.orders.insert(t, n);
                }
                ch.orders.insert(t, n);
            }
        }
        Command::Use(_) | Command::Order { .. } => {}
        Command::Cancel { a, b } => {
            ch.designations = mt
                .cancel_designations(a, b)
                .into_iter()
                .map(|p| (p, None))
                .collect()
        }
    }
    ch
}

fn hero_kind(c: &Command) -> DesignationKind {
    match c {
        Command::Build(_, k) => *k,
        _ => DesignationKind::Dig,
    }
}

/// Resources not yet promised to a build in progress (by dwarves or the hero).
pub fn spare(f: &Fortress) -> Resources {
    let mut r = hero_spare(f);
    if let Some(j) = f.hero.as_ref().and_then(|h| h.dig.as_ref()) {
        r.take(&j.kind.cost());
    }
    r
}

/// Resources the hero may build with: its own current build doesn't count, since a new
/// one replaces it.
fn hero_spare(f: &Fortress) -> Resources {
    let mut r = f.resources;
    for t in f.dwarves.iter().filter_map(|d| d.task.as_ref()) {
        r.take(&t.kind.cost());
    }
    r
}

/// The mountain as the dwarf AI sees it: builds the spare resources can't pay are hidden.
pub fn view_mountain(mt: &Mountain, f: &Fortress) -> Mountain {
    let spare = spare(f);
    // ponytail: a full clone (~200 KB of tiles, <= MAX_DESIGNATIONS entries) per view, i.e.
    // per tick; share an Arc<Mountain> with copy-on-write if it ever shows up in a profile.
    let mut m = mt.clone();
    // Orders show as designations of their recipe, within the AI's cap: when full, an
    // order takes the place of the last plain designation.
    for (&p, &n) in &mt.orders {
        if n > 0
            && !mt.designations.contains_key(&p)
            && let Some(k) = mt.get(p).and_then(Tile::recipe)
            && spare.covers(&k.cost())
        {
            if m.designations.len() >= MAX_DESIGNATIONS {
                let Some(last) = m.designations.iter().rev().find(|(_, k)| !k.is_work()) else {
                    break;
                };
                let last = *last.0;
                m.designations.remove(&last);
            }
            m.designations.insert(p, k);
        }
    }
    // ponytail: per-designation check; with spare wood for one table the AI may still pick
    // several (the server keeps one). Send the spare in AiView if that ever matters.
    m.designations.retain(|_, k| spare.covers(&k.cost()));
    m
}

/// Starts digging (`Dig`, a tree is chopped) or building the tile toward `d`.
fn start_dig(h: &mut Hero, mt: &Mountain, d: Dir, kind: DesignationKind) {
    if matches!(d, Dir::Up | Dir::Down) {
        return;
    }
    let t = h.pos.step(d);
    let kind = match kind {
        DesignationKind::Dig if matches!(mt.get(t), Some(Tile::Tree | Tile::Cow)) => {
            DesignationKind::Chop
        }
        k => k,
    };
    if mt.diggable(t, kind)
        && h.dig
            .as_ref()
            .is_none_or(|j| j.target != t || j.kind != kind)
    {
        h.dig = Some(DigJob {
            target: t,
            kind,
            progress: 0,
        });
    }
}

fn hero_move(h: &mut Hero, mt: &Mountain, d: Dir) {
    h.pending_move = None;
    if let Some(to) = mt.hero_move_dest(h.pos, d) {
        h.pos = to;
        h.move_cooldown = MOVE_COOLDOWN;
        h.dig = None;
    }
}

/// Whether the target of a task is still designated (or ordered) as `kind` and diggable as such.
fn target_valid(m: &Mountain, t: TilePos, kind: DesignationKind) -> bool {
    m.work_at(t) == Some(kind) && m.diggable(t, kind)
}

/// The hero and all dwarves.
pub fn occupied(w: &World, m: MountainId) -> Vec<TilePos> {
    w.fortresses.get(&m).map_or(vec![], |f| {
        f.hero
            .iter()
            .map(|h| h.pos)
            .chain(f.dwarves.iter().map(|d| d.pos))
            .collect()
    })
}

/// Targets of the dwarves' current tasks.
pub fn claimed(w: &World, m: MountainId) -> Vec<TilePos> {
    w.fortresses.get(&m).map_or(vec![], |f| {
        f.dwarves
            .iter()
            .filter_map(|d| d.task.as_ref().map(|t| t.target))
            .collect()
    })
}

/// Hands out the AI's assignments; invalid ones are skipped. Untrusted input: at most one
/// pathfinding per dwarf.
pub fn apply_assignments(w: &mut World, a: &Assignments) {
    let (Some(mt), Some(f)) = (
        w.mountains.get(&a.mountain),
        w.fortresses.get_mut(&a.mountain),
    ) else {
        return;
    };
    let no_block = HashSet::new();
    let (mut searches, cap) = (0, f.dwarves.len());
    for &(id, target) in &a.list {
        let Some(kind) = mt.work_at(target) else {
            continue;
        };
        if !target_valid(mt, target, kind) {
            continue;
        }
        let taken = f
            .dwarves
            .iter()
            .any(|d| d.task.as_ref().is_some_and(|t| t.target == target));
        let under_occupant = kind != DesignationKind::Dig
            && !kind.is_work()
            && (f.hero.as_ref().is_some_and(|h| h.pos == target)
                || f.dwarves.iter().any(|d| d.pos == target));
        if taken || under_occupant || !spare(f).covers(&kind.cost()) {
            continue;
        }
        let Some(d) = f
            .dwarves
            .iter_mut()
            .find(|d| d.id == id && d.task.is_none())
        else {
            continue;
        };
        // Only valid entries count against the pathfinding budget.
        if searches >= cap {
            break;
        }
        searches += 1;
        if let Some(path) = find_path(mt, d.pos, work_spot_goal(mt, target), &no_block) {
            d.task = Some(DwarfTask {
                target,
                kind,
                path,
                progress: 0,
            });
        }
    }
}

/// Digs `p`; records tile and designation changes (stairs also clear the tile below).
/// Returns the yield.
fn dig_at(
    m: &mut Mountain,
    p: TilePos,
    kind: DesignationKind,
    tiles: &mut Vec<(TilePos, Tile)>,
    ch: &mut Changes,
) -> Option<Resources> {
    let below = p.step(Dir::Down);
    let old = [p, below].map(|q| (q, m.get(q), m.designations.contains_key(&q)));
    let ordered = m.orders.contains_key(&p);
    let res = m.dig(p, kind)?;
    if ordered && !m.orders.contains_key(&p) {
        ch.orders.insert(p, 0);
    }
    for (q, old_tile, had) in old {
        if let Some(now) = m.get(q).filter(|n| Some(*n) != old_tile) {
            tiles.push((q, now));
        }
        if had && !m.designations.contains_key(&q) {
            ch.designations.insert(q, None);
        }
    }
    Some(res)
}

/// Each cow on the ground level may step onto a free neighbouring grass tile. A cow that is
/// designated, or being butchered by the hero, stands still so its job is not lost.
fn graze(m: &mut Mountain, f: &Fortress, n: u64, tiles: &mut Vec<(TilePos, Tile)>) {
    let hero_target = f
        .hero
        .as_ref()
        .and_then(|h| h.dig.as_ref())
        .map(|j| j.target);
    let pinned = |m: &Mountain, q| m.work_at(q).is_some() || hero_target == Some(q);
    let occupied =
        |q| f.hero.as_ref().is_some_and(|h| h.pos == q) || f.dwarves.iter().any(|d| d.pos == q);
    let ground = |x, y| TilePos::new(x, y, GROUND_Z);
    let herd: Vec<_> = m
        .rect(ground(0, 0), ground(i16::MAX, i16::MAX))
        .into_iter()
        .filter(|&q| m.get(q) == Some(Tile::Cow))
        .collect();
    for c in herd {
        // ponytail: a hash, not an RNG; half the time the cow stays. Seeded RNG if it shows.
        let h = (c.x as u64).wrapping_mul(73_856_093)
            ^ (c.y as u64).wrapping_mul(19_349_663)
            ^ n.wrapping_mul(83_492_791);
        let Some(&d) = Dir::HORIZONTAL.get((h % 8) as usize) else {
            continue;
        };
        let to = c.step(d);
        if pinned(m, c) || m.get(to) != Some(Tile::Grass) || pinned(m, to) || occupied(to) {
            continue;
        }
        m.set(c, Tile::Grass);
        m.set(to, Tile::Cow);
        tiles.extend([(c, Tile::Grass), (to, Tile::Cow)]);
    }
}

/// Advances every fortress by one tick, owner connected or not. Returns a delta for each
/// fortress that has a hero.
pub fn step(w: &mut World) -> BTreeMap<MountainId, TickDelta> {
    let tick = w.tick;
    w.tick += 1;
    let no_block = HashSet::new();
    let mut out = BTreeMap::new();
    for (&id, f) in w.fortresses.iter_mut() {
        let Some(m) = w.mountains.get_mut(&id) else {
            continue;
        };
        // Designation changes of this step; the last write on a tile wins.
        let (mut tiles, mut ch) = (vec![], Changes::default());

        let hero_spare = hero_spare(f);
        if let Some(h) = f.hero.as_mut() {
            h.move_cooldown = h.move_cooldown.saturating_sub(1);
            // A Move that arrived during the cooldown (network jitter) is not lost.
            if h.move_cooldown == 0
                && let Some(d) = h.pending_move
            {
                hero_move(h, m, d);
                if let Some((dd, kind)) = h.pending_dig.take()
                    && hero_spare.covers(&kind.cost())
                {
                    start_dig(h, m, dd, kind);
                }
            }
            if let Some(j) = h.dig.as_mut() {
                let wall = j.kind.builds().is_some();
                // A dwarf standing on the target, or an unpaid cost, pauses a build.
                let blocked = wall
                    && (!f.resources.covers(&j.kind.cost())
                        || f.dwarves.iter().any(|d| d.pos == j.target));
                match m.get(j.target).filter(|_| m.diggable(j.target, j.kind)) {
                    None => h.dig = None,
                    Some(_) if blocked => {}
                    Some(t) => {
                        j.progress = j.progress.saturating_add(1);
                        let base = if wall {
                            WALL_BUILD_TICKS
                        } else {
                            t.dig_ticks()
                        };
                        if j.progress >= hero_dig_ticks(base, &h.abilities) {
                            let (target, kind) = (j.target, j.kind);
                            h.dig = None;
                            let was = m.designations.get(&target).copied();
                            if let Some(r) = dig_at(m, target, kind, &mut tiles, &mut ch) {
                                f.resources.add(r);
                                f.resources.take(&kind.cost());
                                // Digging a Stairs-designated tile keeps the stairs intent.
                                if !wall
                                    && was == Some(DesignationKind::Stairs)
                                    && m.designate(target, target, DesignationKind::Stairs) == 1
                                {
                                    ch.designations
                                        .insert(target, Some(DesignationKind::Stairs));
                                }
                            }
                        }
                    }
                }
            }
        }

        // An idle dwarf on a designated tile, or on the hero's build, would block it forever
        // (nothing is built under an occupant): step it aside onto the first free neighbour.
        let hero_build = f
            .hero
            .as_ref()
            .and_then(|h| h.dig.as_ref())
            .filter(|j| j.kind.builds().is_some())
            .map(|j| j.target);
        let busy = |q: TilePos| m.work_at(q).is_some() || hero_build == Some(q);
        for d in f.dwarves.iter_mut().filter(|d| d.task.is_none()) {
            if busy(d.pos)
                && let Some(to) = Dir::HORIZONTAL
                    .iter()
                    .map(|&dir| d.pos.step(dir))
                    .find(|&q| m.walkable(q) && !busy(q))
            {
                d.pos = to;
            }
        }

        if tick.is_multiple_of(COW_EVERY) {
            graze(m, f, tick / COW_EVERY, &mut tiles);
        }

        for i in 0..f.dwarves.len() {
            let Some(mut task) = f.dwarves[i].task.take() else {
                continue;
            };
            let pos = f.dwarves[i].pos;
            if !target_valid(m, task.target, task.kind) {
                continue;
            }
            if !task.path.is_empty() {
                if tick.is_multiple_of(2) {
                    let next = task.path[0];
                    let step_ok = ALL_DIRS.iter().any(|&d| m.move_dest(pos, d) == Some(next));
                    if step_ok {
                        task.path.remove(0);
                        f.dwarves[i].pos = next;
                    } else {
                        // Blocked: re-path once, drop the task if that fails.
                        match find_path(m, pos, work_spot_goal(m, task.target), &no_block) {
                            Some(p) => task.path = p,
                            None => continue,
                        }
                    }
                }
            } else if !Dir::HORIZONTAL.iter().any(|&d| pos.step(d) == task.target) {
                continue; // not at a work spot
            } else {
                task.progress = task.progress.saturating_add(1);
                let base = m.get(task.target).map_or(0, Tile::dig_ticks);
                let needed = match task.kind {
                    DesignationKind::Dig | DesignationKind::Chop => base,
                    DesignationKind::Stairs => base.saturating_add(STAIRS_EXTRA_TICKS),
                    k if k.is_work() => k.work_ticks(),
                    _ => WALL_BUILD_TICKS, // builds
                };
                if task.progress >= needed {
                    let blocked = task.kind != DesignationKind::Dig
                        && !task.kind.is_work()
                        && (f.hero.as_ref().is_some_and(|h| h.pos == task.target)
                            || f.dwarves.iter().any(|d| d.pos == task.target))
                        || !f.resources.covers(&task.kind.cost());
                    // Done, or blocked by an occupant: either way the task ends. When blocked
                    // the designation stays and the AI reassigns it once the tile is free.
                    if !blocked && task.kind.is_work() {
                        f.resources.take(&task.kind.cost());
                        f.resources.add(task.kind.product());
                        let n = m
                            .orders
                            .get(&task.target)
                            .map_or(0, |n| n.saturating_sub(1));
                        if n == 0 {
                            m.orders.remove(&task.target);
                        } else {
                            m.orders.insert(task.target, n);
                        }
                        ch.orders.insert(task.target, n);
                    } else if !blocked
                        && let Some(r) = dig_at(m, task.target, task.kind, &mut tiles, &mut ch)
                    {
                        f.resources.add(r);
                        f.resources.take(&task.kind.cost());
                    }
                    continue;
                }
            }
            f.dwarves[i].task = Some(task);
        }

        if let Some(h) = &f.hero {
            let mut d = TickDelta {
                tick: w.tick,
                hero_pos: h.pos,
                dwarves: f.dwarves.iter().map(dwarf_view).collect(),
                tiles,
                designations_added: vec![],
                designations_removed: vec![],
                resources: f.resources,
                orders: vec![], // filled from `ch` below
            };
            ch.merge_into(&mut d);
            out.insert(id, d);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use moria_core::srd::{Class, Race, roll_abilities};
    use moria_core::world::{Hero, claim_mountain, generate};
    use moria_core::{DwarfId, PlayerId};
    use std::time::Instant;

    const M: MountainId = MountainId(0);
    /// Offset from the entrance room's north-west corner (z 0 = `GROUND_Z`).
    fn p(x: i16, y: i16, z: u8) -> TilePos {
        let e = moria_core::world::entrance_tiles().next().unwrap();
        TilePos::new(e.x + x, e.y + y, e.z + z)
    }

    /// One claimed mountain with a hero at p(2, 1, 0), in the entrance room.
    fn setup() -> World {
        let mut w = generate(1);
        claim_mountain(&mut w, PlayerId(1)).unwrap();
        w.fortresses.get_mut(&M).unwrap().hero = Some(Hero {
            name: "Thorin".into(),
            race: Race::Dwarf,
            class: Class::Fighter,
            abilities: roll_abilities(Race::Dwarf, Class::Fighter),
            pos: p(2, 1, 0),
            dig: None,
            move_cooldown: 0,
            pending_move: None,
            pending_dig: None,
        });
        w
    }
    fn hero(w: &World) -> &Hero {
        w.fortresses[&M].hero.as_ref().unwrap()
    }
    fn set_str(w: &mut World, s: u8) {
        w.fortresses
            .get_mut(&M)
            .unwrap()
            .hero
            .as_mut()
            .unwrap()
            .abilities
            .str = s;
    }
    fn tile(w: &World, q: TilePos) -> Tile {
        w.mountains[&M].get(q).unwrap()
    }
    fn designate(w: &mut World, q: TilePos, kind: DesignationKind) {
        apply_command(w, M, Command::Designate { a: q, b: q, kind });
    }

    #[test]
    fn hero_locks_and_unlocks_a_door() {
        let mut w = setup();
        let door = p(3, 1, 0);
        w.mountains.get_mut(&M).unwrap().set(door, Tile::WoodDoor);
        let ch = apply_command(&mut w, M, Command::Use(Dir::E));
        assert_eq!(tile(&w, door), Tile::WoodDoorLocked);
        assert_eq!(ch.tiles, vec![(door, Tile::WoodDoorLocked)]);
        apply_command(&mut w, M, Command::Move(Dir::E)); // the hero has the key
        assert_eq!(hero(&w).pos, door);
    }

    #[test]
    fn cannot_lock_an_occupied_door() {
        let mut w = setup();
        let door = p(3, 1, 0);
        w.mountains.get_mut(&M).unwrap().set(door, Tile::StoneDoor);
        w.fortresses.get_mut(&M).unwrap().dwarves[0].pos = door;
        apply_command(&mut w, M, Command::Use(Dir::E));
        assert_eq!(tile(&w, door), Tile::StoneDoor);
    }

    #[test]
    fn locking_a_door_reroutes_or_drops_the_dwarf() {
        let mut w = setup();
        // A corridor east of the room: the door is the only way to the dig target.
        let door = p(7, 1, 0);
        let mt = w.mountains.get_mut(&M).unwrap();
        for x in 6..10 {
            mt.set(p(x, 1, 0), Tile::Floor);
        }
        mt.set(door, Tile::WoodDoor);
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(6, 1, 0);
        let t = p(10, 1, 0);
        designate(&mut w, t, DesignationKind::Dig);
        let id = w.fortresses[&M].dwarves[0].id;
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: M,
                tick: 0,
                list: vec![(id, t)],
            },
        );
        assert!(w.fortresses[&M].dwarves[0].task.is_some());
        apply_command(&mut w, M, Command::Use(Dir::E));
        assert_eq!(tile(&w, door), Tile::WoodDoorLocked);
        for _ in 0..40 {
            step(&mut w);
            assert_ne!(w.fortresses[&M].dwarves[0].pos, door);
        }
        assert!(w.fortresses[&M].dwarves[0].task.is_none());
        assert_eq!(tile(&w, t), Tile::Rock);
    }

    #[test]
    fn hero_moves_with_cooldown() {
        let mut w = setup();
        apply_command(&mut w, M, Command::Move(Dir::E));
        apply_command(&mut w, M, Command::Move(Dir::E));
        assert_eq!(hero(&w).pos, p(3, 1, 0));
        step(&mut w);
        apply_command(&mut w, M, Command::Move(Dir::E));
        assert_eq!(hero(&w).pos, p(3, 1, 0));
        step(&mut w);
        apply_command(&mut w, M, Command::Move(Dir::E));
        assert_eq!(hero(&w).pos, p(4, 1, 0));
    }

    #[test]
    fn move_during_cooldown_is_buffered_not_dropped() {
        let mut w = setup();
        apply_command(&mut w, M, Command::Move(Dir::E));
        // Two moves during the cooldown: only the latest is kept.
        apply_command(&mut w, M, Command::Move(Dir::W));
        apply_command(&mut w, M, Command::Move(Dir::E));
        assert_eq!(hero(&w).pos, p(3, 1, 0));
        let d = step(&mut w);
        assert_eq!(hero(&w).pos, p(3, 1, 0));
        assert_eq!(d[&M].hero_pos, p(3, 1, 0));
        let d = step(&mut w);
        assert_eq!(hero(&w).pos, p(4, 1, 0));
        assert_eq!(d[&M].hero_pos, p(4, 1, 0));
        assert_eq!(hero(&w).pending_move, None);
        step(&mut w);
        step(&mut w);
        assert_eq!(hero(&w).pos, p(4, 1, 0)); // applied once only
        // A Dig applies the buffered move first, then nothing is left pending.
        apply_command(&mut w, M, Command::Move(Dir::W));
        step(&mut w);
        apply_command(&mut w, M, Command::Move(Dir::W));
        apply_command(&mut w, M, Command::Dig(Dir::N));
        assert_eq!(hero(&w).pos, p(3, 1, 0)); // no move before the cooldown ends
        step(&mut w);
        step(&mut w);
        assert_eq!(hero(&w).pos, p(2, 1, 0));
        assert_eq!((hero(&w).pending_move, hero(&w).pending_dig), (None, None));
    }

    #[test]
    fn dig_after_buffered_move_digs_from_new_position() {
        let mut w = setup();
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(3, 1, 0);
        apply_command(&mut w, M, Command::Move(Dir::E));
        apply_command(&mut w, M, Command::Move(Dir::E)); // buffered (cooldown)
        apply_command(&mut w, M, Command::Dig(Dir::E)); // buffered behind the move
        assert_eq!(hero(&w).pos, p(4, 1, 0));
        assert!(hero(&w).dig.is_none());
        step(&mut w);
        assert_eq!(hero(&w).pos, p(4, 1, 0));
        step(&mut w); // cooldown over: move, then dig from the new position
        assert_eq!(hero(&w).pos, p(5, 1, 0));
        assert_eq!(hero(&w).dig.as_ref().map(|j| j.target), Some(p(6, 1, 0)));
        assert_eq!((hero(&w).pending_move, hero(&w).pending_dig), (None, None));
        // A buffered move into rock is still refused; the dig starts from where the hero is.
        let mut w = setup();
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(4, 1, 0);
        apply_command(&mut w, M, Command::Move(Dir::E));
        apply_command(&mut w, M, Command::Move(Dir::E));
        apply_command(&mut w, M, Command::Dig(Dir::E));
        step(&mut w);
        step(&mut w);
        assert_eq!(hero(&w).pos, p(5, 1, 0));
        assert_eq!(hero(&w).dig.as_ref().map(|j| j.target), Some(p(6, 1, 0)));
    }

    #[test]
    fn move_dig_spam_cannot_bypass_cooldown() {
        let run = |with_dig: bool| {
            let mut w = setup();
            let m = w.mountains.get_mut(&M).unwrap();
            for x in 0..40 {
                m.set(p(x, 1, 0), Tile::Floor);
            }
            for _ in 0..20 {
                apply_command(&mut w, M, Command::Move(Dir::E));
                if with_dig {
                    apply_command(&mut w, M, Command::Dig(Dir::E));
                }
                step(&mut w);
            }
            hero(&w).pos.x - p(2, 1, 0).x
        };
        // One immediate move, then one per 2-tick cooldown: 1 + 20 / 2.
        let moved = run(true);
        assert!(moved <= 11, "moved {moved} tiles in 20 ticks");
        assert_eq!(moved, run(false), "Dig must not speed moves up");
    }

    #[test]
    fn hero_cannot_walk_into_rock() {
        let mut w = setup();
        apply_command(&mut w, M, Command::Move(Dir::Up));
        apply_command(&mut w, M, Command::Move(Dir::Down));
        assert_eq!(hero(&w).pos, p(2, 1, 0));
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(5, 1, 0);
        apply_command(&mut w, M, Command::Move(Dir::E));
        assert_eq!(hero(&w).pos, p(5, 1, 0));
        assert_eq!(hero(&w).move_cooldown, 0);
    }

    #[test]
    fn strong_hero_digs_rock_in_7_ticks() {
        let mut w = setup();
        set_str(&mut w, 17);
        // p(6, 1, 0) is rock east of the entrance room.
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(5, 1, 0);
        let t = p(6, 1, 0);
        assert_eq!(tile(&w, t), Tile::Rock);
        apply_command(&mut w, M, Command::Dig(Dir::E));
        for _ in 0..6 {
            step(&mut w);
            assert_eq!(tile(&w, t), Tile::Rock);
        }
        let d = step(&mut w);
        assert_eq!(tile(&w, t), Tile::Floor);
        assert_eq!(w.fortresses[&M].resources.stone, 1);
        assert!(hero(&w).dig.is_none());
        assert_eq!(d[&M].tiles, vec![(t, Tile::Floor)]);
    }

    #[test]
    fn move_cancels_dig() {
        let mut w = setup();
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(5, 1, 0);
        apply_command(&mut w, M, Command::Dig(Dir::E));
        step(&mut w);
        assert!(hero(&w).dig.is_some());
        apply_command(&mut w, M, Command::Move(Dir::W));
        assert!(hero(&w).dig.is_none());
    }

    #[test]
    fn dwarf_walks_digs_and_frees_itself() {
        let mut w = setup();
        let t = p(6, 1, 0);
        designate(&mut w, t, DesignationKind::Dig);
        let id = w.fortresses[&M].dwarves[0].id;
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: M,
                tick: 0,
                list: vec![(id, t)],
            },
        );
        assert!(w.fortresses[&M].dwarves[0].task.is_some());
        assert_eq!(claimed(&w, M), vec![t]);
        let mut removed = false;
        for _ in 0..40 {
            let d = step(&mut w);
            removed |= d[&M].designations_removed == vec![t];
        }
        assert_eq!(tile(&w, t), Tile::Floor);
        assert!(removed);
        assert_eq!(w.fortresses[&M].resources.stone, 1);
        assert!(w.fortresses[&M].dwarves[0].task.is_none());
        assert!(w.mountains[&M].designations.is_empty());
    }

    #[test]
    fn task_dropped_when_target_already_dug() {
        let mut w = setup();
        let t = p(6, 1, 0);
        designate(&mut w, t, DesignationKind::Dig);
        let id = w.fortresses[&M].dwarves[0].id;
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: M,
                tick: 0,
                list: vec![(id, t)],
            },
        );
        w.mountains
            .get_mut(&M)
            .unwrap()
            .dig(t, DesignationKind::Dig);
        step(&mut w);
        assert!(w.fortresses[&M].dwarves[0].task.is_none());
        assert_eq!(w.fortresses[&M].resources.stone, 0);
    }

    #[test]
    fn stairs_never_dug_under_occupant() {
        let mut w = setup();
        let h = hero(&w).pos;
        designate(&mut w, h, DesignationKind::Stairs);
        assert!(w.mountains[&M].designations.contains_key(&h));
        let id = w.fortresses[&M].dwarves[0].id;
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: M,
                tick: 0,
                list: vec![(id, h)],
            },
        );
        assert!(w.fortresses[&M].dwarves[0].task.is_none());
        // Occupant arrives mid-task: the task is dropped and the designation stays.
        let t = p(3, 2, 0);
        designate(&mut w, t, DesignationKind::Stairs);
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: M,
                tick: 0,
                list: vec![(id, t)],
            },
        );
        assert!(w.fortresses[&M].dwarves[0].task.is_some());
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = t;
        for _ in 0..60 {
            step(&mut w);
        }
        assert_eq!(tile(&w, t), Tile::Floor);
        assert!(w.fortresses[&M].dwarves[0].task.is_none());
        assert_eq!(
            w.mountains[&M].designations.get(&t),
            Some(&DesignationKind::Stairs)
        );
        // Once free, it can be reassigned and dug.
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(2, 1, 0);
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: M,
                tick: 0,
                list: vec![(id, t)],
            },
        );
        for _ in 0..40 {
            step(&mut w);
        }
        assert_eq!(tile(&w, t), Tile::Stair);
    }

    #[test]
    fn hero_builds_wall_with_stone() {
        let mut w = setup();
        let t = p(3, 1, 0);
        // No stone: nothing starts.
        apply_command(&mut w, M, Command::Build(Dir::E, DesignationKind::Wall));
        assert!(hero(&w).dig.is_none());
        w.fortresses.get_mut(&M).unwrap().resources.stone = 1;
        apply_command(&mut w, M, Command::Build(Dir::E, DesignationKind::Wall));
        assert_eq!(
            hero(&w).dig.as_ref().map(|j| j.kind),
            Some(DesignationKind::Wall)
        );
        // With its one stone reserved, the hero may still switch to another tile.
        apply_command(&mut w, M, Command::Build(Dir::S, DesignationKind::Wall));
        assert_eq!(hero(&w).dig.as_ref().map(|j| j.target), Some(p(2, 2, 0)));
        apply_command(&mut w, M, Command::Build(Dir::E, DesignationKind::Wall));
        // A dwarf crossing the tile pauses the build, it doesn't cancel it; an idle one
        // standing there is stepped aside.
        w.fortresses.get_mut(&M).unwrap().dwarves[0].pos = t;
        step(&mut w);
        assert_eq!(hero(&w).dig.as_ref().map(|j| j.progress), Some(0));
        assert_ne!(w.fortresses[&M].dwarves[0].pos, t);
        let mut built = None;
        for _ in 0..30 {
            if step(&mut w)[&M].tiles.contains(&(t, Tile::Wall)) {
                built = Some(());
            }
        }
        assert!(built.is_some(), "delta reports the new wall");
        assert_eq!(tile(&w, t), Tile::Wall);
        assert_eq!(w.fortresses[&M].resources.stone, 0);
        // A wall is solid: Build on it does nothing, Dig takes it back.
        apply_command(&mut w, M, Command::Build(Dir::E, DesignationKind::Wall));
        assert!(hero(&w).dig.is_none());
        apply_command(&mut w, M, Command::Dig(Dir::E));
        assert_eq!(
            hero(&w).dig.as_ref().map(|j| j.kind),
            Some(DesignationKind::Dig)
        );
    }

    #[test]
    fn work_kinds_cannot_be_designated() {
        let q = TilePos::new(0, 0, 0);
        for kind in [DesignationKind::Smelt, DesignationKind::Smith] {
            assert_eq!(
                Command::from_client(ClientMsg::Designate { a: q, b: q, kind }),
                None
            );
        }
        assert_eq!(
            Command::from_client(ClientMsg::Use(Dir::E)),
            Some(Command::Use(Dir::E))
        );
        assert_eq!(
            Command::from_client(ClientMsg::Order {
                dir: Dir::N,
                add: 5
            }),
            Some(Command::Order {
                dir: Dir::N,
                add: 5
            })
        );
    }

    #[test]
    fn hero_builds_bed_and_build_must_be_a_build_kind() {
        let k = |kind| Command::from_client(ClientMsg::Build(Dir::E, kind));
        assert_eq!(k(DesignationKind::Stairs), None);
        assert_eq!(k(DesignationKind::Dig), None);
        let mut w = setup();
        let t = p(3, 1, 0);
        w.fortresses.get_mut(&M).unwrap().resources.stone = 1;
        apply_command(&mut w, M, k(DesignationKind::Bed).unwrap());
        for _ in 0..30 {
            step(&mut w);
        }
        assert_eq!(tile(&w, t), Tile::Bed);
        assert_eq!(w.fortresses[&M].resources.stone, 0);
        assert!(w.mountains[&M].walkable(t));
    }

    #[test]
    fn buffered_build_respects_dwarf_reserved_stone() {
        let mut w = setup();
        let f = w.fortresses.get_mut(&M).unwrap();
        f.resources.stone = 1;
        f.dwarves[0].task = Some(DwarfTask {
            target: p(5, 3, 0),
            kind: DesignationKind::Wall,
            path: vec![],
            progress: 0,
        });
        apply_command(&mut w, M, Command::Move(Dir::E));
        apply_command(&mut w, M, Command::Build(Dir::E, DesignationKind::Wall)); // buffered behind the move
        step(&mut w);
        step(&mut w);
        assert!(hero(&w).dig.is_none(), "the dwarf's stone is not taken");
    }

    /// The status page shows what each busy dwarf is doing.
    #[test]
    fn tick_reports_dwarf_jobs() {
        let mut w = setup();
        let t = p(5, 2, 0);
        designate(&mut w, t, DesignationKind::Wall);
        w.fortresses.get_mut(&M).unwrap().resources.stone = 1;
        let id = w.fortresses[&M].dwarves[0].id;
        let list = vec![(id, t)];
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: M,
                tick: 0,
                list,
            },
        );
        let d = step(&mut w).remove(&M).unwrap();
        assert_eq!(d.dwarves[0].2.map(|j| j.kind), Some(DesignationKind::Wall));
        assert!(d.dwarves[1..].iter().all(|x| x.2.is_none()), "idle: no job");
    }

    /// Walking to the work spot is not working yet.
    #[test]
    fn dwarf_view_tells_walking_from_working() {
        let task = |path| DwarfTask {
            target: p(5, 2, 0),
            kind: DesignationKind::Dig,
            path,
            progress: 0,
        };
        let d = |task| Dwarf {
            id: DwarfId(7),
            pos: p(1, 1, 0),
            task,
        };
        let job = |working| {
            Some(DwarfJob {
                kind: DesignationKind::Dig,
                working,
            })
        };
        assert_eq!(dwarf_view(&d(None)), (DwarfId(7), p(1, 1, 0), None));
        assert_eq!(dwarf_view(&d(Some(task(vec![])))).2, job(true));
        assert_eq!(dwarf_view(&d(Some(task(vec![p(2, 1, 0)])))).2, job(false));
    }

    #[test]
    fn dwarf_builds_wall_with_stone() {
        let mut w = setup();
        let t = p(5, 2, 0);
        designate(&mut w, t, DesignationKind::Wall);
        let id = w.fortresses[&M].dwarves[0].id;
        let assign = |w: &mut World| {
            apply_assignments(
                w,
                &Assignments {
                    mountain: M,
                    tick: 0,
                    list: vec![(id, t)],
                },
            )
        };
        // No stone: not assigned, the designation waits.
        assign(&mut w);
        assert!(w.fortresses[&M].dwarves[0].task.is_none());
        w.fortresses.get_mut(&M).unwrap().resources.stone = 1;
        assign(&mut w);
        assert_eq!(spare(&w.fortresses[&M]).stone, 0, "the stone is reserved");
        // One stone, one wall: a second wall is not handed out on the same stone.
        let t2 = p(5, 0, 0);
        designate(&mut w, t2, DesignationKind::Wall);
        let id2 = w.fortresses[&M].dwarves[2].id;
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: M,
                tick: 0,
                list: vec![(id2, t2)],
            },
        );
        assert!(w.fortresses[&M].dwarves[2].task.is_none());
        w.fortresses.get_mut(&M).unwrap().resources.stone = 2;
        for _ in 0..60 {
            step(&mut w);
        }
        assert_eq!(tile(&w, t), Tile::Wall);
        assert_eq!(w.fortresses[&M].resources.stone, 1);
        assert!(!w.mountains[&M].designations.contains_key(&t));
    }

    #[test]
    fn idle_dwarf_steps_off_designated_tile() {
        let mut w = setup();
        let t = p(1, 0, 0);
        assert_eq!(w.fortresses[&M].dwarves[1].pos, t);
        designate(&mut w, t, DesignationKind::Stairs);
        step(&mut w);
        // N of t is rock: first walkable, undesignated HORIZONTAL neighbour is S.
        assert_eq!(w.fortresses[&M].dwarves[1].pos, p(1, 1, 0));
        let id = w.fortresses[&M].dwarves[0].id;
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: M,
                tick: 0,
                list: vec![(id, t)],
            },
        );
        assert!(w.fortresses[&M].dwarves[0].task.is_some());
        for _ in 0..40 {
            step(&mut w);
        }
        assert_eq!(tile(&w, t), Tile::Stair);
    }

    #[test]
    fn designate_after_cancel_same_tick_stays_designated() {
        let mut w = setup();
        let t = p(6, 1, 0);
        let mut ch = Changes::default();
        ch.then(apply_command(
            &mut w,
            M,
            Command::Designate {
                a: t,
                b: t,
                kind: DesignationKind::Dig,
            },
        ));
        ch.then(apply_command(&mut w, M, Command::Cancel { a: t, b: t }));
        ch.then(apply_command(
            &mut w,
            M,
            Command::Designate {
                a: t,
                b: t,
                kind: DesignationKind::Dig,
            },
        ));
        assert_eq!(
            ch.designations,
            BTreeMap::from([(t, Some(DesignationKind::Dig))])
        );
        let mut d = step(&mut w).remove(&M).unwrap();
        ch.merge_into(&mut d);
        assert_eq!(d.designations_added, vec![(t, DesignationKind::Dig)]);
        assert!(d.designations_removed.is_empty());
        // Reverse: designate then cancel ends removed only.
        let mut ch = Changes::default();
        ch.then(apply_command(&mut w, M, Command::Cancel { a: t, b: t }));
        ch.then(apply_command(
            &mut w,
            M,
            Command::Designate {
                a: t,
                b: t,
                kind: DesignationKind::Dig,
            },
        ));
        ch.then(apply_command(&mut w, M, Command::Cancel { a: t, b: t }));
        assert_eq!(ch.designations, BTreeMap::from([(t, None)]));
    }

    #[test]
    fn hero_dig_of_stairs_designation_keeps_stairs_intent() {
        let mut w = setup();
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(5, 1, 0);
        set_str(&mut w, 30);
        let t = p(6, 1, 0);
        designate(&mut w, t, DesignationKind::Stairs);
        apply_command(&mut w, M, Command::Dig(Dir::E));
        let mut last = None;
        for _ in 0..12 {
            let d = step(&mut w).remove(&M).unwrap();
            if d.tiles.contains(&(t, Tile::Floor)) {
                last = Some(d);
            }
        }
        let d = last.expect("tile dug");
        assert_eq!(tile(&w, t), Tile::Floor);
        assert_eq!(
            w.mountains[&M].designations.get(&t),
            Some(&DesignationKind::Stairs)
        );
        assert_eq!(d.designations_added, vec![(t, DesignationKind::Stairs)]);
        assert!(!d.designations_removed.contains(&t));
    }

    #[test]
    fn stairs_dig_reports_removed_designation_below() {
        let mut w = setup();
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(5, 1, 0);
        set_str(&mut w, 30);
        let t = p(6, 1, 0);
        let below = t.step(Dir::Down);
        designate(&mut w, below, DesignationKind::Dig);
        assert!(w.mountains[&M].designations.contains_key(&below));
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().dig = Some(DigJob {
            target: t,
            kind: DesignationKind::Stairs,
            progress: 0,
        });
        let mut removed = false;
        for _ in 0..12 {
            removed |= step(&mut w)[&M].designations_removed.contains(&below);
        }
        assert_eq!(tile(&w, below), Tile::Stair);
        assert!(!w.mountains[&M].designations.contains_key(&below));
        assert!(removed, "delta never reported the removal below");
    }

    #[test]
    fn invalid_head_entries_do_not_starve_valid_ones() {
        let mut w = setup();
        let t = p(6, 1, 0);
        designate(&mut w, t, DesignationKind::Dig);
        let ids: Vec<_> = w.fortresses[&M].dwarves.iter().map(|d| d.id).collect();
        let mut list = vec![(ids[0], p(40, 40, 0)); 5];
        list.push((ids[0], t));
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: M,
                tick: 0,
                list,
            },
        );
        assert!(w.fortresses[&M].dwarves[0].task.is_some());
    }

    #[test]
    fn invalid_assignments_ignored() {
        let mut w = setup();
        let (a, b) = (p(6, 1, 0), p(6, 2, 0));
        designate(&mut w, a, DesignationKind::Dig);
        let ids: Vec<_> = w.fortresses[&M].dwarves.iter().map(|d| d.id).collect();
        let asg = |list| Assignments {
            mountain: M,
            tick: 0,
            list,
        };
        // Undesignated target, unknown dwarf, unknown mountain.
        apply_assignments(&mut w, &asg(vec![(ids[0], b), (DwarfId(999), a)]));
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: MountainId(99),
                tick: 0,
                list: vec![(ids[0], a)],
            },
        );
        assert!(w.fortresses[&M].dwarves.iter().all(|d| d.task.is_none()));
        // Double claim and busy dwarf.
        designate(&mut w, b, DesignationKind::Dig);
        apply_assignments(&mut w, &asg(vec![(ids[0], a), (ids[1], a), (ids[0], b)]));
        let tasks: Vec<_> = w.fortresses[&M]
            .dwarves
            .iter()
            .map(|d| d.task.as_ref().map(|t| t.target))
            .collect();
        assert_eq!(tasks, vec![Some(a), None, None]);
    }

    #[test]
    fn without_assignments_dwarves_finish_then_idle() {
        let mut w = setup();
        let (a, b) = (p(6, 1, 0), p(6, 2, 0));
        designate(&mut w, a, DesignationKind::Dig);
        designate(&mut w, b, DesignationKind::Dig);
        let id = w.fortresses[&M].dwarves[0].id;
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: M,
                tick: 0,
                list: vec![(id, a)],
            },
        );
        for _ in 0..60 {
            step(&mut w);
        }
        assert_eq!(tile(&w, a), Tile::Floor);
        assert_eq!(tile(&w, b), Tile::Rock);
        assert!(w.fortresses[&M].dwarves.iter().all(|d| d.task.is_none()));
    }

    #[test]
    fn all_mountains_simulate_without_owner_connected() {
        let mut w = setup();
        claim_mountain(&mut w, PlayerId(2)).unwrap(); // no hero: owner never connected
        let t = p(6, 1, 0);
        let m1 = MountainId(1);
        w.mountains
            .get_mut(&m1)
            .unwrap()
            .designate(t, t, DesignationKind::Dig);
        let id = w.fortresses[&m1].dwarves[0].id;
        apply_assignments(
            &mut w,
            &Assignments {
                mountain: m1,
                tick: 0,
                list: vec![(id, t)],
            },
        );
        let mut deltas = Default::default();
        for _ in 0..40 {
            deltas = step(&mut w);
        }
        assert_eq!(w.mountains[&m1].get(t), Some(Tile::Floor));
        assert_eq!(w.fortresses[&m1].resources.stone, 1);
        assert_eq!(w.tick, 40);
        assert!(deltas.contains_key(&M) && !deltas.contains_key(&m1));
    }

    #[test]
    fn commands_stay_on_their_mountain() {
        let mut w = setup();
        let t = p(6, 1, 0);
        let c = apply_command(
            &mut w,
            M,
            Command::Designate {
                a: p(-9, -9, 0),
                b: p(99, 99, 0),
                kind: DesignationKind::Dig,
            },
        );
        assert!(!c.designations.is_empty() && c.designations.values().all(Option::is_some));
        let n = w.mountains[&M].designations.len();
        assert!(w.mountains[&M].designations.contains_key(&t));
        let c = apply_command(
            &mut w,
            M,
            Command::Cancel {
                a: p(-9, -9, 0),
                b: p(99, 99, 0),
            },
        );
        assert!(c.designations.len() == n && c.designations.values().all(Option::is_none));
        assert!(w.mountains[&M].designations.is_empty());
        // Unknown mountain, and a mountain nobody owns: ignored.
        let before = w.clone();
        apply_command(
            &mut w,
            MountainId(99),
            Command::Designate {
                a: t,
                b: t,
                kind: DesignationKind::Dig,
            },
        );
        apply_command(
            &mut w,
            MountainId(5),
            Command::Designate {
                a: t,
                b: t,
                kind: DesignationKind::Dig,
            },
        );
        assert_eq!(w, before);
    }

    fn assign(w: &mut World, i: usize, t: TilePos) {
        let id = w.fortresses[&M].dwarves[i].id;
        apply_assignments(
            w,
            &Assignments {
                mountain: M,
                tick: 0,
                list: vec![(id, t)],
            },
        );
    }
    fn res(w: &mut World) -> &mut Resources {
        &mut w.fortresses.get_mut(&M).unwrap().resources
    }
    fn wood(n: u32) -> Resources {
        Resources {
            wood: n,
            ..Default::default()
        }
    }
    fn in_view(w: &World, t: TilePos) -> bool {
        view_mountain(&w.mountains[&M], &w.fortresses[&M])
            .designations
            .contains_key(&t)
    }

    #[test]
    fn dwarf_chops_designated_tree() {
        let mut w = setup();
        let t = p(4, 0, 0);
        w.mountains.get_mut(&M).unwrap().set(t, Tile::Tree);
        w.fortresses.get_mut(&M).unwrap().dwarves[0].pos = p(3, 0, 0);
        designate(&mut w, t, DesignationKind::Chop);
        assert_eq!(
            w.mountains[&M].designations.get(&t),
            Some(&DesignationKind::Chop)
        );
        assign(&mut w, 0, t);
        assert!(w.fortresses[&M].dwarves[0].task.is_some());
        // Already at a work spot: one tick of work per step, 20 for a tree.
        for _ in 0..19 {
            step(&mut w);
            assert_eq!(tile(&w, t), Tile::Tree);
        }
        let d = step(&mut w).remove(&M).unwrap();
        assert_eq!(tile(&w, t), Tile::Grass);
        assert_eq!(w.fortresses[&M].resources, wood(3));
        assert!(!w.mountains[&M].designations.contains_key(&t));
        assert_eq!(d.tiles, vec![(t, Tile::Grass)]);
        assert_eq!(d.designations_removed, vec![t]);
        assert_eq!(d.resources, wood(3));
    }

    #[test]
    fn dwarf_butchers_designated_cow() {
        let mut w = setup();
        let c = p(4, 0, 0); // in the entrance room: no grass around, so it cannot wander off
        w.mountains.get_mut(&M).unwrap().set(c, Tile::Cow);
        w.fortresses.get_mut(&M).unwrap().dwarves[0].pos = p(3, 0, 0);
        designate(&mut w, c, DesignationKind::Chop);
        assign(&mut w, 0, c);
        assert!(w.fortresses[&M].dwarves[0].task.is_some());
        for _ in 0..19 {
            step(&mut w);
            assert_eq!(tile(&w, c), Tile::Cow);
        }
        let d = step(&mut w).remove(&M).unwrap();
        assert_eq!(tile(&w, c), Tile::Grass);
        assert_eq!(d.resources.food, 4);
    }

    /// A cow at `c` (outdoors, on the ground) fenced by trees except toward `open`.
    fn cow_pen(w: &mut World, c: TilePos, open: &[Dir]) {
        let m = w.mountains.get_mut(&M).unwrap();
        m.set(c, Tile::Cow);
        for d in Dir::HORIZONTAL {
            let t = if open.contains(&d) {
                Tile::Grass
            } else {
                Tile::Tree
            };
            m.set(c.step(d), t);
        }
    }
    fn cows(w: &World) -> usize {
        let m = &w.mountains[&M];
        m.rect(
            TilePos::new(0, 0, moria_core::GROUND_Z),
            TilePos::new(i16::MAX, i16::MAX, moria_core::GROUND_Z),
        )
        .into_iter()
        .filter(|&q| m.get(q) == Some(Tile::Cow))
        .count()
    }

    #[test]
    fn cows_wander_onto_grass() {
        let mut w = setup();
        let c = p(-6, 1, 0);
        cow_pen(&mut w, c, &[Dir::N]);
        let herd = cows(&w);
        let mut seen = vec![];
        for _ in 0..300 {
            seen.extend(step(&mut w).remove(&M).unwrap().tiles);
            assert_eq!(cows(&w), herd, "a moving cow is neither lost nor cloned");
        }
        assert!(seen.contains(&(c, Tile::Grass)) && seen.contains(&(c.step(Dir::N), Tile::Cow)));
    }

    #[test]
    fn designated_cow_stands_still() {
        let mut w = setup();
        let c = p(-6, 1, 0);
        cow_pen(&mut w, c, &[Dir::N]);
        designate(&mut w, c, DesignationKind::Chop);
        for _ in 0..300 {
            step(&mut w);
        }
        assert_eq!(tile(&w, c), Tile::Cow);
    }

    #[test]
    fn cow_never_steps_onto_the_hero() {
        let mut w = setup();
        let c = p(-6, 1, 0);
        cow_pen(&mut w, c, &[Dir::N]);
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = c.step(Dir::N);
        for _ in 0..300 {
            step(&mut w);
        }
        assert_eq!(tile(&w, c), Tile::Cow);
    }

    #[test]
    fn hero_butchers_a_cow_that_waits_for_it() {
        let mut w = setup();
        set_str(&mut w, 10);
        let c = p(-6, 1, 0);
        cow_pen(&mut w, c, &[Dir::N, Dir::S]);
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = c.step(Dir::S);
        apply_command(&mut w, M, Command::Dig(Dir::N));
        assert_eq!(
            hero(&w).dig.as_ref().map(|j| j.kind),
            Some(DesignationKind::Chop)
        );
        let n = hero_dig_ticks(Tile::Cow.dig_ticks(), &hero(&w).abilities);
        for _ in 1..n {
            step(&mut w);
            assert_eq!(tile(&w, c), Tile::Cow, "the cow waits for the hero");
        }
        step(&mut w);
        assert_eq!(tile(&w, c), Tile::Grass);
        assert_eq!(w.fortresses[&M].resources.food, 4);
    }

    #[test]
    fn hero_chops_tree() {
        let mut w = setup();
        set_str(&mut w, 10);
        let t = p(3, 1, 0);
        w.mountains.get_mut(&M).unwrap().set(t, Tile::Tree);
        apply_command(&mut w, M, Command::Dig(Dir::E));
        assert_eq!(
            hero(&w).dig.as_ref().map(|j| j.kind),
            Some(DesignationKind::Chop)
        );
        let n = hero_dig_ticks(Tile::Tree.dig_ticks(), &hero(&w).abilities);
        for _ in 1..n {
            step(&mut w);
            assert_eq!(tile(&w, t), Tile::Tree);
        }
        step(&mut w);
        assert_eq!(tile(&w, t), Tile::Grass);
        assert_eq!(w.fortresses[&M].resources, wood(3));
    }

    #[test]
    fn wood_build_waits_without_wood() {
        let mut w = setup();
        let t = p(4, 2, 0);
        designate(&mut w, t, DesignationKind::WoodTable);
        *res(&mut w) = wood(3);
        assign(&mut w, 0, t);
        assert!(w.fortresses[&M].dwarves[0].task.is_none());
        assert!(!in_view(&w, t));
        *res(&mut w) = wood(4);
        assert!(in_view(&w, t));
        assign(&mut w, 0, t);
        assert!(w.fortresses[&M].dwarves[0].task.is_some());
        assert_eq!(spare(&w.fortresses[&M]), Resources::default());
        for _ in 0..80 {
            step(&mut w);
        }
        assert_eq!(tile(&w, t), Tile::WoodTable);
        assert_eq!(w.fortresses[&M].resources, Resources::default());
    }

    #[test]
    fn costs_are_per_resource() {
        let mut w = setup();
        let (wb, sb) = (p(4, 2, 0), p(4, 3, 0));
        designate(&mut w, wb, DesignationKind::WoodBed);
        designate(&mut w, sb, DesignationKind::Bed);
        *res(&mut w) = wood(9);
        assert!(in_view(&w, wb) && !in_view(&w, sb));
        assign(&mut w, 0, wb);
        assign(&mut w, 1, sb);
        assert!(w.fortresses[&M].dwarves[0].task.is_some());
        assert!(
            w.fortresses[&M].dwarves[1].task.is_none(),
            "stone bed waits"
        );
        // The hero can't build stone either.
        apply_command(&mut w, M, Command::Build(Dir::E, DesignationKind::Bed));
        assert!(hero(&w).dig.is_none());
        // Nor spend wood a dwarf task has reserved: 6 wood minus 3 reserved leaves 3 < 4.
        *res(&mut w) = wood(6);
        apply_command(
            &mut w,
            M,
            Command::Build(Dir::E, DesignationKind::WoodTable),
        );
        assert!(hero(&w).dig.is_none(), "reserved wood is not spare");
        *res(&mut w) = wood(9);
        for _ in 0..80 {
            step(&mut w);
        }
        assert_eq!(tile(&w, wb), Tile::WoodBed);
        assert_eq!(tile(&w, sb), Tile::Floor);
        assert_eq!(w.fortresses[&M].resources, wood(6), "no stone spent");
        // Stone only: a wood build never takes stone.
        *res(&mut w) = Resources {
            stone: 5,
            ..Default::default()
        };
        apply_command(
            &mut w,
            M,
            Command::Build(Dir::E, DesignationKind::WoodChair),
        );
        assert!(hero(&w).dig.is_none());
    }

    #[test]
    fn hero_builds_wood_chair() {
        let mut w = setup();
        let t = p(3, 1, 0);
        *res(&mut w) = wood(1);
        apply_command(
            &mut w,
            M,
            Command::Build(Dir::E, DesignationKind::WoodChair),
        );
        assert!(hero(&w).dig.is_none());
        *res(&mut w) = wood(2);
        apply_command(
            &mut w,
            M,
            Command::Build(Dir::E, DesignationKind::WoodChair),
        );
        for _ in 0..60 {
            step(&mut w);
        }
        assert_eq!(tile(&w, t), Tile::WoodChair);
        assert_eq!(w.fortresses[&M].resources, Resources::default());
    }

    #[test]
    fn dismantle_wood_furniture_returns_wood() {
        let mut w = setup();
        let t = p(3, 1, 0);
        w.mountains.get_mut(&M).unwrap().set(t, Tile::WoodBed);
        apply_command(&mut w, M, Command::Dig(Dir::E));
        for _ in 0..60 {
            step(&mut w);
        }
        assert_eq!(tile(&w, t), Tile::Floor);
        assert_eq!(w.fortresses[&M].resources, wood(3));
    }

    /// Security: building costs exactly `cost()` of the right resources and digging the
    /// piece back returns exactly that, by hero and by dwarf.
    #[test]
    fn build_then_dismantle_never_gains() {
        use DesignationKind as K;
        let kinds = [
            K::Wall,
            K::Bed,
            K::Table,
            K::Chair,
            K::WoodBed,
            K::WoodTable,
            K::WoodChair,
            K::Candle,
            K::Torch,
            K::Lamp,
            K::Campfire,
            K::Hearth,
            K::Stove,
        ];
        for kind in kinds {
            let cost = kind.cost();
            let built = kind.builds().unwrap();
            // Hero.
            let mut w = setup();
            let t = p(3, 1, 0);
            *res(&mut w) = cost;
            apply_command(&mut w, M, Command::Build(Dir::E, kind));
            for _ in 0..60 {
                step(&mut w);
            }
            assert_eq!(tile(&w, t), built, "hero {kind:?}");
            assert_eq!(w.fortresses[&M].resources, Resources::default(), "{kind:?}");
            apply_command(&mut w, M, Command::Dig(Dir::E));
            for _ in 0..60 {
                step(&mut w);
            }
            assert_eq!(tile(&w, t), Tile::Floor, "hero {kind:?}");
            assert_eq!(w.fortresses[&M].resources, cost, "hero {kind:?}");
            // Dwarf.
            let mut w = setup();
            let t = p(4, 2, 0);
            *res(&mut w) = cost;
            designate(&mut w, t, kind);
            assign(&mut w, 0, t);
            for _ in 0..80 {
                step(&mut w);
            }
            assert_eq!(tile(&w, t), built, "dwarf {kind:?}");
            assert_eq!(w.fortresses[&M].resources, Resources::default(), "{kind:?}");
            designate(&mut w, t, K::Dig);
            assign(&mut w, 0, t);
            for _ in 0..80 {
                step(&mut w);
            }
            assert_eq!(tile(&w, t), Tile::Floor, "dwarf {kind:?}");
            assert_eq!(w.fortresses[&M].resources, cost, "dwarf {kind:?}");
        }
    }

    #[test]
    #[ignore]
    fn tick_budget_ten_full_fortresses() {
        let mut w = generate(9);
        for i in 0..10 {
            let id = claim_mountain(&mut w, PlayerId(i)).unwrap();
            let f = w.fortresses.get_mut(&id).unwrap();
            f.hero = Some(Hero {
                name: "H".into(),
                race: Race::Dwarf,
                class: Class::Fighter,
                abilities: roll_abilities(Race::Dwarf, Class::Fighter),
                pos: p(2, 1, 0),
                dig: None,
                move_cooldown: 0,
                pending_move: None,
                pending_dig: None,
            });
            let m = w.mountains.get_mut(&id).unwrap();
            for k in 0..50 {
                let q = p(6 + k / 4, k % 4, 0);
                m.designate(
                    q,
                    q,
                    if k % 5 == 0 {
                        DesignationKind::Stairs
                    } else {
                        DesignationKind::Dig
                    },
                );
            }
            // Plenty more rock to keep every dwarf busy for the whole run.
            m.designate(p(10, 0, 0), p(47, 47, 0), DesignationKind::Dig);
            // Outdoors: the forest west of the entrance, stairs on the grass, soil and rock
            // underneath, and wood furniture paid by the chopping.
            m.designate(p(-60, -40, 0), p(-1, 40, 0), DesignationKind::Chop);
            for k in 1..6 {
                let q = p(-6 * k, 1, 0);
                m.designate(q, q, DesignationKind::Stairs);
            }
            m.designate(p(-40, -20, 1), p(5, 20, 1), DesignationKind::Dig);
            m.designate(p(-10, 2, 0), p(-1, 2, 0), DesignationKind::WoodChair);
        }
        let mut total = std::time::Duration::ZERO;
        let mut busy = 0usize;
        for n in 0..1000u64 {
            if n % 10 == 0 {
                let ids: Vec<_> = w.fortresses.keys().copied().collect();
                for id in ids {
                    let view = moria_core::ai::AiView {
                        mountain: view_mountain(&w.mountains[&id], &w.fortresses[&id]),
                        tick: w.tick,
                        dwarves: w.fortresses[&id]
                            .dwarves
                            .iter()
                            .map(|d| moria_core::ai::DwarfView {
                                id: d.id,
                                pos: d.pos,
                                idle: d.task.is_none(),
                            })
                            .collect(),
                        claimed: claimed(&w, id),
                        occupied: occupied(&w, id),
                    };
                    let list = moria_dwarf_ai::assign::assign(&view);
                    let t0 = Instant::now();
                    apply_assignments(
                        &mut w,
                        &Assignments {
                            mountain: id,
                            tick: n,
                            list,
                        },
                    );
                    total += t0.elapsed();
                }
            }
            let t0 = Instant::now();
            step(&mut w);
            total += t0.elapsed();
            busy += w
                .fortresses
                .values()
                .flat_map(|f| &f.dwarves)
                .filter(|d| d.task.is_some())
                .count();
        }
        let mean = total / 1000;
        let left: usize = w.mountains.values().map(|m| m.designations.len()).sum();
        println!(
            "mean step+assign: {mean:?}; busy dwarf-ticks {busy}/30000; designations left: {left}"
        );
        assert!(left > 500 && busy > 10_000, "workload ran dry");
        assert!(
            w.fortresses.values().any(|f| f.resources.wood > 0),
            "no tree was chopped"
        );
        assert!(mean.as_millis() < 10, "{mean:?}");
    }

    /// A forge at p(3, 1, 0), east of the hero, with `n` orders.
    fn ordered_forge(w: &mut World, n: u8) -> TilePos {
        let forge = p(3, 1, 0);
        let mt = w.mountains.get_mut(&M).unwrap();
        mt.set(forge, Tile::Forge);
        mt.orders.insert(forge, n);
        forge
    }
    fn ore(iron_ore: u32, coal: u32) -> Resources {
        Resources {
            iron_ore,
            coal,
            ..Default::default()
        }
    }

    #[test]
    fn order_needs_an_adjacent_workshop_and_clamps() {
        let mut w = setup();
        let forge = p(3, 1, 0);
        w.mountains.get_mut(&M).unwrap().set(forge, Tile::Forge);
        apply_command(
            &mut w,
            M,
            Command::Order {
                dir: Dir::W,
                add: 5,
            },
        ); // not a workshop
        apply_command(
            &mut w,
            M,
            Command::Order {
                dir: Dir::Up,
                add: 5,
            },
        );
        assert!(w.mountains[&M].orders.is_empty());
        for _ in 0..30 {
            apply_command(
                &mut w,
                M,
                Command::Order {
                    dir: Dir::E,
                    add: 5,
                },
            );
        }
        assert_eq!(w.mountains[&M].orders[&forge], 99);
        let ch = apply_command(
            &mut w,
            M,
            Command::Order {
                dir: Dir::E,
                add: 0,
            },
        );
        assert!(w.mountains[&M].orders.is_empty());
        assert_eq!(ch.orders.get(&forge), Some(&0));
        // The command's change reaches that tick's delta.
        let mut d = step(&mut w).remove(&M).unwrap();
        ch.merge_into(&mut d);
        assert_eq!(d.orders, vec![(forge, 0)]);
    }

    #[test]
    fn a_dwarf_smelts_the_ordered_ingots() {
        let mut w = setup();
        let forge = ordered_forge(&mut w, 2);
        *res(&mut w) = ore(4, 2);
        let mut last = None;
        for _ in 0..300 {
            if w.mountains[&M].orders.is_empty() {
                break;
            }
            if w.fortresses[&M].dwarves[0].task.is_none() && in_view(&w, forge) {
                assign(&mut w, 0, forge);
                assert_eq!(
                    w.fortresses[&M].dwarves[0].task.as_ref().map(|t| t.kind),
                    Some(DesignationKind::Smelt)
                );
            }
            last = step(&mut w).remove(&M);
        }
        let r = w.fortresses[&M].resources;
        assert_eq!((r.iron, r.iron_ore, r.coal), (2, 0, 0));
        assert!(w.mountains[&M].orders.is_empty());
        assert!(last.unwrap().orders.contains(&(forge, 0)));
        assert_eq!(tile(&w, forge), Tile::Forge);
    }

    #[test]
    fn smelting_without_stock_produces_nothing() {
        let mut w = setup();
        let forge = ordered_forge(&mut w, 1);
        *res(&mut w) = ore(2, 1);
        assign(&mut w, 0, forge);
        let progress = |w: &World| {
            w.fortresses[&M].dwarves[0]
                .task
                .as_ref()
                .map(|t| t.progress)
        };
        for _ in 0..300 {
            if progress(&w) == Some(DesignationKind::Smelt.work_ticks() - 1) {
                break;
            }
            step(&mut w);
        }
        assert_eq!(progress(&w), Some(29));
        res(&mut w).coal = 0;
        step(&mut w);
        assert!(w.fortresses[&M].dwarves[0].task.is_none());
        let r = w.fortresses[&M].resources;
        assert_eq!((r.iron, r.iron_ore), (0, 2));
        assert_eq!(w.mountains[&M].orders[&forge], 1);
    }

    #[test]
    fn standing_on_the_forge_does_not_waste_a_finished_smelt() {
        let mut w = setup();
        let forge = ordered_forge(&mut w, 1);
        *res(&mut w) = ore(2, 1);
        assign(&mut w, 0, forge);
        for _ in 0..300 {
            let t = w.fortresses[&M].dwarves[0].task.as_ref();
            if t.is_some_and(|t| t.progress == DesignationKind::Smelt.work_ticks() - 1) {
                break;
            }
            step(&mut w);
        }
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = forge;
        step(&mut w);
        assert_eq!(w.fortresses[&M].resources.iron, 1);
        assert!(w.mountains[&M].orders.is_empty());
    }

    #[test]
    fn short_stock_hides_the_order_from_the_ai() {
        let mut w = setup();
        let forge = ordered_forge(&mut w, 3);
        *res(&mut w) = ore(1, 0);
        assert!(!in_view(&w, forge));
        res(&mut w).add(ore(1, 1));
        let view = view_mountain(&w.mountains[&M], &w.fortresses[&M]);
        assert_eq!(view.designations.get(&forge), Some(&DesignationKind::Smelt));
    }

    #[test]
    fn orders_reach_the_ai_even_when_designations_are_full() {
        let mut w = setup();
        let forge = ordered_forge(&mut w, 1);
        *res(&mut w) = ore(2, 1);
        let mt = w.mountains.get_mut(&M).unwrap();
        let (mw, _, d) = mt.size();
        // A forged full list (`designate` caps it), far below the forge.
        for i in 0..MAX_DESIGNATIONS as i16 {
            let q = TilePos::new(i % mw, i / mw, d - 1);
            mt.designations.insert(q, DesignationKind::Dig);
        }
        let view = view_mountain(&w.mountains[&M], &w.fortresses[&M]);
        assert_eq!(view.designations.get(&forge), Some(&DesignationKind::Smelt));
        assert!(view.designations.len() <= MAX_DESIGNATIONS);
    }

    #[test]
    fn digging_a_forge_drops_its_order() {
        let mut w = setup();
        let forge = ordered_forge(&mut w, 3);
        let stone = w.fortresses[&M].resources.stone;
        apply_command(&mut w, M, Command::Dig(Dir::E));
        let mut reported = false;
        for _ in 0..100 {
            if tile(&w, forge) != Tile::Forge {
                break;
            }
            reported |= step(&mut w)[&M].orders.contains(&(forge, 0));
        }
        assert_ne!(tile(&w, forge), Tile::Forge);
        assert!(w.mountains[&M].orders.is_empty());
        assert_eq!(w.fortresses[&M].resources.stone, stone + 6);
        assert!(reported);
    }
}
