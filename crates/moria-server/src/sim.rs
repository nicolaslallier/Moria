//! Authoritative, pure, synchronous simulation. No I/O, no async: the main loop calls
//! `apply_command` / `apply_assignments` between ticks and `step` once per tick.

use moria_core::ai::Assignments;
use moria_core::path::{find_path, work_spot_goal};
use moria_core::protocol::{ClientMsg, TickDelta};
use moria_core::srd::hero_dig_ticks;
use moria_core::world::{DigJob, DwarfTask, World};
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

/// Designation changes caused by a command; the caller folds them into that tick's delta.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    pub added: Vec<(TilePos, DesignationKind)>,
    pub removed: Vec<TilePos>,
}

impl Changes {
    /// Appends to `d`. Clients apply `designations_added` before `designations_removed`.
    pub fn merge_into(self, d: &mut TickDelta) {
        d.designations_added.extend(self.added);
        d.designations_removed.extend(self.removed);
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
        Command::Move(d) => {
            if h.move_cooldown == 0 && mt.can_move(h.pos, d) {
                h.pos = h.pos.step(d);
                h.move_cooldown = MOVE_COOLDOWN;
                h.dig = None;
            }
        }
        Command::Dig(d) => {
            if matches!(d, Dir::Up | Dir::Down) {
                return ch;
            }
            let t = h.pos.step(d);
            if mt.get(t).is_some_and(Tile::is_solid) && h.dig.as_ref().is_none_or(|j| j.target != t)
            {
                h.dig = Some(DigJob {
                    target: t,
                    kind: DesignationKind::Dig,
                    progress: 0,
                });
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
                    ch.added.push((p, kind));
                }
            }
        }
        Command::Cancel { a, b } => ch.removed = mt.cancel_designations(a, b),
    }
    ch
}

/// Whether the target of a task can still be dug as `kind`.
fn target_valid(m: &Mountain, t: TilePos, kind: DesignationKind) -> bool {
    m.designations.get(&t) == Some(&kind)
        && match (m.get(t), kind) {
            (Some(x), DesignationKind::Dig) => x.is_solid(),
            (Some(x), DesignationKind::Stairs) => x.is_solid() || x == Tile::Floor,
            (None, _) => false,
        }
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
    for &(id, target) in a.list.iter().take(f.dwarves.len()) {
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

/// Digs `p`; records tile and designation changes. Returns the yield.
fn dig_at(
    m: &mut Mountain,
    p: TilePos,
    kind: DesignationKind,
    tiles: &mut Vec<(TilePos, Tile)>,
    removed: &mut Vec<TilePos>,
) -> Option<Resources> {
    let below = p.step(Dir::Down);
    let (old_p, old_below) = (m.get(p), m.get(below));
    let had = m.designations.contains_key(&p);
    let res = m.dig(p, kind)?;
    for (q, old) in [(p, old_p), (below, old_below)] {
        if let Some(now) = m.get(q).filter(|n| Some(*n) != old) {
            tiles.push((q, now));
        }
    }
    if had {
        removed.push(p);
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
        let (mut tiles, mut removed) = (vec![], vec![]);

        if let Some(h) = f.hero.as_mut() {
            h.move_cooldown = h.move_cooldown.saturating_sub(1);
            if let Some(j) = h.dig.as_mut() {
                match m.get(j.target).filter(|t| t.is_solid()) {
                    None => h.dig = None,
                    Some(t) => {
                        j.progress += 1;
                        if j.progress >= hero_dig_ticks(t.dig_ticks(), &h.abilities) {
                            let (target, kind) = (j.target, j.kind);
                            h.dig = None;
                            if let Some(r) = dig_at(m, target, kind, &mut tiles, &mut removed) {
                                f.resources.add(r);
                            }
                        }
                    }
                }
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
                task.progress += 1;
                let base = m.get(task.target).map_or(0, Tile::dig_ticks);
                let needed = match task.kind {
                    DesignationKind::Dig => base,
                    DesignationKind::Stairs => base + STAIRS_EXTRA_TICKS,
                };
                if task.progress >= needed {
                    let blocked = task.kind == DesignationKind::Stairs
                        && (f.hero.as_ref().is_some_and(|h| h.pos == task.target)
                            || f.dwarves.iter().any(|d| d.pos == task.target));
                    if !blocked {
                        if let Some(r) = dig_at(m, task.target, task.kind, &mut tiles, &mut removed)
                        {
                            f.resources.add(r);
                        }
                        continue; // done: task stays None
                    }
                }
            }
            f.dwarves[i].task = Some(task);
        }

        if let Some(h) = &f.hero {
            out.insert(
                id,
                TickDelta {
                    tick: w.tick,
                    hero_pos: h.pos,
                    dwarves: f.dwarves.iter().map(|d| (d.id, d.pos)).collect(),
                    tiles,
                    designations_added: vec![],
                    designations_removed: removed,
                    resources: f.resources,
                },
            );
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
        // Occupant arrives mid-task: the dig waits instead of completing.
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
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = p(2, 23, 0);
        for _ in 0..3 {
            step(&mut w);
        }
        assert_eq!(tile(&w, t), Tile::Stair);
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
        assert!(!c.added.is_empty());
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
        assert_eq!(c.removed.len(), n);
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
        }
        let mut total = std::time::Duration::ZERO;
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
                    apply_assignments(
                        &mut w,
                        &Assignments {
                            mountain: id,
                            tick: n,
                            list,
                        },
                    );
                }
            }
            let t0 = Instant::now();
            step(&mut w);
            total += t0.elapsed();
        }
        let mean = total / 1000;
        println!(
            "mean step: {mean:?}; designations left: {}",
            w.mountains
                .values()
                .map(|m| m.designations.len())
                .sum::<usize>()
        );
        assert!(mean.as_millis() < 10, "{mean:?}");
    }
}
