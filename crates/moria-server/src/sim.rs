//! Authoritative, pure, synchronous simulation. No I/O, no async: the main loop calls
//! `apply_command` / `apply_assignments` between ticks and `step` once per tick.

use moria_core::ai::Assignments;
use moria_core::path::{find_path, work_spot_goal};
use moria_core::protocol::{ClientMsg, TickDelta};
use moria_core::srd::hero_dig_ticks;
use moria_core::world::{DigJob, DwarfTask, Hero, World};
use moria_core::{DesignationKind, Dir, Mountain, MountainId, Resources, Tile, TilePos};
use std::collections::{BTreeMap, HashSet};

const MOVE_COOLDOWN: u8 = 2;
const STAIRS_EXTRA_TICKS: u16 = 10;
const ALL_DIRS: [Dir; 6] = [Dir::N, Dir::S, Dir::E, Dir::W, Dir::Up, Dir::Down];

/// `ClientMsg` minus the handshake messages.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Move(Dir),
    Dig(Dir),
    Designate {
        a: TilePos,
        b: TilePos,
        kind: DesignationKind,
    },
    Cancel {
        a: TilePos,
        b: TilePos,
    },
}

impl Command {
    pub fn from_client(m: ClientMsg) -> Option<Command> {
        Some(match m {
            ClientMsg::Move(d) => Command::Move(d),
            ClientMsg::Dig(d) => Command::Dig(d),
            ClientMsg::Designate { a, b, kind } => Command::Designate { a, b, kind },
            ClientMsg::CancelDesignation { a, b } => Command::Cancel { a, b },
            ClientMsg::Hello { .. } | ClientMsg::CreateHero { .. } => return None,
        })
    }
}

/// Designation changes caused by commands, folded into that tick's delta. Per tile:
/// `Some(kind)` = designated, `None` = removed; the last write wins.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes(pub BTreeMap<TilePos, Option<DesignationKind>>);

impl Changes {
    /// Folds a `later` command's changes into `self`; the last write on a tile wins.
    pub fn then(&mut self, later: Changes) {
        self.0.extend(later.0);
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
        for (p, k) in self.0.into_iter().filter(|(p, _)| !seen.contains(p)) {
            match k {
                Some(k) => d.designations_added.push((p, k)),
                None => d.designations_removed.push(p),
            }
        }
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
        Command::Dig(d) if h.pending_move.is_some() => h.pending_dig = Some(d),
        Command::Dig(d) => start_dig(h, mt, d),
        Command::Designate { a, b, kind } => {
            let rect = mt.rect(a, b);
            let before: Vec<_> = rect
                .iter()
                .map(|p| mt.designations.get(p).copied())
                .collect();
            mt.designate(a, b, kind);
            for (p, old) in rect.into_iter().zip(before) {
                if old != Some(kind) && mt.designations.get(&p) == Some(&kind) {
                    ch.0.insert(p, Some(kind));
                }
            }
        }
        Command::Cancel { a, b } => {
            ch.0 = mt
                .cancel_designations(a, b)
                .into_iter()
                .map(|p| (p, None))
                .collect()
        }
    }
    ch
}

fn start_dig(h: &mut Hero, mt: &Mountain, d: Dir) {
    if matches!(d, Dir::Up | Dir::Down) {
        return;
    }
    let t = h.pos.step(d);
    if mt.get(t).is_some_and(Tile::is_solid) && h.dig.as_ref().is_none_or(|j| j.target != t) {
        h.dig = Some(DigJob {
            target: t,
            kind: DesignationKind::Dig,
            progress: 0,
        });
    }
}

fn hero_move(h: &mut Hero, mt: &Mountain, d: Dir) {
    h.pending_move = None;
    if mt.can_move(h.pos, d) {
        h.pos = h.pos.step(d);
        h.move_cooldown = MOVE_COOLDOWN;
        h.dig = None;
    }
}

/// Whether the target of a task is still designated as `kind` and diggable as such.
fn target_valid(m: &Mountain, t: TilePos, kind: DesignationKind) -> bool {
    m.designations.get(&t) == Some(&kind) && m.diggable(t, kind)
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
        let Some(kind) = mt.designations.get(&target).copied() else {
            continue;
        };
        if !target_valid(mt, target, kind) {
            continue;
        }
        let taken = f
            .dwarves
            .iter()
            .any(|d| d.task.as_ref().is_some_and(|t| t.target == target));
        let under_occupant = kind == DesignationKind::Stairs
            && (f.hero.as_ref().is_some_and(|h| h.pos == target)
                || f.dwarves.iter().any(|d| d.pos == target));
        if taken || under_occupant {
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
    let res = m.dig(p, kind)?;
    for (q, old_tile, had) in old {
        if let Some(now) = m.get(q).filter(|n| Some(*n) != old_tile) {
            tiles.push((q, now));
        }
        if had && !m.designations.contains_key(&q) {
            ch.0.insert(q, None);
        }
    }
    Some(res)
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

        if let Some(h) = f.hero.as_mut() {
            h.move_cooldown = h.move_cooldown.saturating_sub(1);
            // A Move that arrived during the cooldown (network jitter) is not lost.
            if h.move_cooldown == 0
                && let Some(d) = h.pending_move
            {
                hero_move(h, m, d);
                if let Some(dd) = h.pending_dig.take() {
                    start_dig(h, m, dd);
                }
            }
            if let Some(j) = h.dig.as_mut() {
                match m.get(j.target).filter(|t| t.is_solid()) {
                    None => h.dig = None,
                    Some(t) => {
                        j.progress = j.progress.saturating_add(1);
                        if j.progress >= hero_dig_ticks(t.dig_ticks(), &h.abilities) {
                            let (target, kind) = (j.target, j.kind);
                            h.dig = None;
                            let was = m.designations.get(&target).copied();
                            if let Some(r) = dig_at(m, target, kind, &mut tiles, &mut ch) {
                                f.resources.add(r);
                                // Digging a Stairs-designated tile keeps the stairs intent.
                                if was == Some(DesignationKind::Stairs)
                                    && m.designate(target, target, DesignationKind::Stairs) == 1
                                {
                                    ch.0.insert(target, Some(DesignationKind::Stairs));
                                }
                            }
                        }
                    }
                }
            }
        }

        // An idle dwarf on a designated tile would block it forever (stairs are never dug
        // under an occupant): step it aside onto the first free-of-designation neighbour.
        for d in f.dwarves.iter_mut().filter(|d| d.task.is_none()) {
            if m.designations.contains_key(&d.pos)
                && let Some(to) = Dir::HORIZONTAL
                    .iter()
                    .map(|&dir| d.pos.step(dir))
                    .find(|&q| m.walkable(q) && !m.designations.contains_key(&q))
            {
                d.pos = to;
            }
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
                    let step_ok = ALL_DIRS
                        .iter()
                        .any(|&d| pos.step(d) == next && m.can_move(pos, d));
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
                    DesignationKind::Dig => base,
                    DesignationKind::Stairs => base.saturating_add(STAIRS_EXTRA_TICKS),
                };
                if task.progress >= needed {
                    let blocked = task.kind == DesignationKind::Stairs
                        && (f.hero.as_ref().is_some_and(|h| h.pos == task.target)
                            || f.dwarves.iter().any(|d| d.pos == task.target));
                    // Done, or blocked by an occupant: either way the task ends. When blocked
                    // the designation stays and the AI reassigns it once the tile is free.
                    if !blocked
                        && let Some(r) = dig_at(m, task.target, task.kind, &mut tiles, &mut ch)
                    {
                        f.resources.add(r);
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
                dwarves: f.dwarves.iter().map(|d| (d.id, d.pos)).collect(),
                tiles,
                designations_added: vec![],
                designations_removed: vec![],
                resources: f.resources,
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
    fn p(x: i16, y: i16, z: u8) -> TilePos {
        TilePos::new(x, y, z)
    }

    /// One claimed mountain with a hero at (2,23,0) and an all-Floor surface strip.
    fn setup() -> World {
        let mut w = generate(1);
        claim_mountain(&mut w, PlayerId(1)).unwrap();
        w.fortresses.get_mut(&M).unwrap().hero = Some(Hero {
            name: "Thorin".into(),
            race: Race::Dwarf,
            class: Class::Fighter,
            abilities: roll_abilities(Race::Dwarf, Class::Fighter),
            pos: p(2, 23, 0),
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
    fn hero_moves_with_cooldown() {
        let mut w = setup();
        apply_command(&mut w, M, Command::Move(Dir::E));
        apply_command(&mut w, M, Command::Move(Dir::E));
        assert_eq!(hero(&w).pos, p(3, 23, 0));
        step(&mut w);
        apply_command(&mut w, M, Command::Move(Dir::E));
        assert_eq!(hero(&w).pos, p(3, 23, 0));
        step(&mut w);
        apply_command(&mut w, M, Command::Move(Dir::E));
        assert_eq!(hero(&w).pos, p(4, 23, 0));
    }

    #[test]
    fn move_during_cooldown_is_buffered_not_dropped() {
        let mut w = setup();
        apply_command(&mut w, M, Command::Move(Dir::E));
        // Two moves during the cooldown: only the latest is kept.
        apply_command(&mut w, M, Command::Move(Dir::W));
        apply_command(&mut w, M, Command::Move(Dir::E));
        assert_eq!(hero(&w).pos, p(3, 23, 0));
        let d = step(&mut w);
        assert_eq!(hero(&w).pos, p(3, 23, 0));
        assert_eq!(d[&M].hero_pos, p(3, 23, 0));
        let d = step(&mut w);
        assert_eq!(hero(&w).pos, p(4, 23, 0));
        assert_eq!(d[&M].hero_pos, p(4, 23, 0));
        assert_eq!(hero(&w).pending_move, None);
        step(&mut w);
        step(&mut w);
        assert_eq!(hero(&w).pos, p(4, 23, 0)); // applied once only
        // A Dig applies the buffered move first, then nothing is left pending.
        apply_command(&mut w, M, Command::Move(Dir::W));
        step(&mut w);
        apply_command(&mut w, M, Command::Move(Dir::W));
        apply_command(&mut w, M, Command::Dig(Dir::N));
        assert_eq!(hero(&w).pos, p(3, 23, 0)); // no move before the cooldown ends
        step(&mut w);
        step(&mut w);
        assert_eq!(hero(&w).pos, p(2, 23, 0));
        assert_eq!((hero(&w).pending_move, hero(&w).pending_dig), (None, None));
    }

    #[test]
    fn dig_after_buffered_move_digs_from_new_position() {
        let mut w = setup();
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(3, 23, 0);
        apply_command(&mut w, M, Command::Move(Dir::E));
        apply_command(&mut w, M, Command::Move(Dir::E)); // buffered (cooldown)
        apply_command(&mut w, M, Command::Dig(Dir::E)); // buffered behind the move
        assert_eq!(hero(&w).pos, p(4, 23, 0));
        assert!(hero(&w).dig.is_none());
        step(&mut w);
        assert_eq!(hero(&w).pos, p(4, 23, 0));
        step(&mut w); // cooldown over: move, then dig from the new position
        assert_eq!(hero(&w).pos, p(5, 23, 0));
        assert_eq!(hero(&w).dig.as_ref().map(|j| j.target), Some(p(6, 23, 0)));
        assert_eq!((hero(&w).pending_move, hero(&w).pending_dig), (None, None));
        // A buffered move into rock is still refused; the dig starts from where the hero is.
        let mut w = setup();
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(4, 23, 0);
        apply_command(&mut w, M, Command::Move(Dir::E));
        apply_command(&mut w, M, Command::Move(Dir::E));
        apply_command(&mut w, M, Command::Dig(Dir::E));
        step(&mut w);
        step(&mut w);
        assert_eq!(hero(&w).pos, p(5, 23, 0));
        assert_eq!(hero(&w).dig.as_ref().map(|j| j.target), Some(p(6, 23, 0)));
    }

    #[test]
    fn move_dig_spam_cannot_bypass_cooldown() {
        let run = |with_dig: bool| {
            let mut w = setup();
            let m = w.mountains.get_mut(&M).unwrap();
            for x in 0..40 {
                m.set(p(x, 23, 0), Tile::Floor);
            }
            for _ in 0..20 {
                apply_command(&mut w, M, Command::Move(Dir::E));
                if with_dig {
                    apply_command(&mut w, M, Command::Dig(Dir::E));
                }
                step(&mut w);
            }
            hero(&w).pos.x - 2
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
        assert_eq!(hero(&w).pos, p(2, 23, 0));
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(5, 23, 0);
        apply_command(&mut w, M, Command::Move(Dir::E));
        assert_eq!(hero(&w).pos, p(5, 23, 0));
        assert_eq!(hero(&w).move_cooldown, 0);
    }

    #[test]
    fn strong_hero_digs_rock_in_7_ticks() {
        let mut w = setup();
        set_str(&mut w, 17);
        // (6,23,0) is rock east of the entrance strip.
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(5, 23, 0);
        let t = p(6, 23, 0);
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
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(5, 23, 0);
        apply_command(&mut w, M, Command::Dig(Dir::E));
        step(&mut w);
        assert!(hero(&w).dig.is_some());
        apply_command(&mut w, M, Command::Move(Dir::W));
        assert!(hero(&w).dig.is_none());
    }

    #[test]
    fn dwarf_walks_digs_and_frees_itself() {
        let mut w = setup();
        let t = p(6, 23, 0);
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
        let t = p(6, 23, 0);
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
        let t = p(3, 24, 0);
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
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(2, 23, 0);
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
    fn idle_dwarf_steps_off_designated_tile() {
        let mut w = setup();
        let t = p(1, 22, 0);
        assert_eq!(w.fortresses[&M].dwarves[1].pos, t);
        designate(&mut w, t, DesignationKind::Stairs);
        step(&mut w);
        // N of t is rock: first walkable, undesignated HORIZONTAL neighbour is S.
        assert_eq!(w.fortresses[&M].dwarves[1].pos, p(1, 23, 0));
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
        let t = p(6, 23, 0);
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
        assert_eq!(ch.0, BTreeMap::from([(t, Some(DesignationKind::Dig))]));
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
        assert_eq!(ch.0, BTreeMap::from([(t, None)]));
    }

    #[test]
    fn hero_dig_of_stairs_designation_keeps_stairs_intent() {
        let mut w = setup();
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(5, 23, 0);
        set_str(&mut w, 30);
        let t = p(6, 23, 0);
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
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(5, 23, 0);
        set_str(&mut w, 30);
        let t = p(6, 23, 0);
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
        let t = p(6, 23, 0);
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
        let (a, b) = (p(6, 23, 0), p(6, 24, 0));
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
        let (a, b) = (p(6, 23, 0), p(6, 24, 0));
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
        let t = p(6, 23, 0);
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
        let t = p(6, 23, 0);
        let c = apply_command(
            &mut w,
            M,
            Command::Designate {
                a: p(-9, -9, 0),
                b: p(99, 99, 0),
                kind: DesignationKind::Dig,
            },
        );
        assert!(!c.0.is_empty() && c.0.values().all(Option::is_some));
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
        assert!(c.0.len() == n && c.0.values().all(Option::is_none));
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
                pos: p(2, 23, 0),
                dig: None,
                move_cooldown: 0,
                pending_move: None,
                pending_dig: None,
            });
            let m = w.mountains.get_mut(&id).unwrap();
            for k in 0..50 {
                let q = p(6 + k / 4, 22 + k % 4, 0);
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
            m.designate(p(10, 22, 0), p(47, 47, 0), DesignationKind::Dig);
        }
        let mut total = std::time::Duration::ZERO;
        let mut busy = 0usize;
        for n in 0..1000u64 {
            if n % 10 == 0 {
                let ids: Vec<_> = w.fortresses.keys().copied().collect();
                for id in ids {
                    let view = moria_core::ai::AiView {
                        mountain: w.mountains[&id].clone(),
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
        assert!(mean.as_millis() < 10, "{mean:?}");
    }
}
