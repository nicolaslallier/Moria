//! The server's side of the food simulation: its grid is the truth for positions, so every
//! proposal of a `FoodSim` is checked here before it touches the map. Without a bus the
//! server also runs the `FoodSim`s itself (`InProcess`).

use crate::sim::{Changes, on_ground};
use moria_core::food::{Cell, CellKind, FoodAction, FoodEvent, FoodSim, GROW_EVERY};
use moria_core::protocol::TickDelta;
use moria_core::world::{Fortress, World};
use moria_core::{DesignationKind, Dir, GROUND_Z, Mountain, MountainId, Tile, TilePos};
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

/// Ticks between two re-adoptions of the grid by an in-process `FoodSim` (5 min at 10 Hz).
pub const RESYNC_EVERY: u64 = 3_000;

/// Whether `ev` still fits the grid. A cow that is designated, or the hero's target, stands
/// still so its job is not lost; nothing steps or is born under the hero or a dwarf.
pub fn validate(m: &Mountain, f: &Fortress, ev: &FoodEvent) -> bool {
    let hero_target = f
        .hero
        .as_ref()
        .and_then(|h| h.dig.as_ref())
        .map(|j| j.target);
    let pinned = |q| m.work_at(q).is_some() || hero_target == Some(q);
    let occupied =
        |q| f.hero.as_ref().is_some_and(|h| h.pos == q) || f.dwarves.iter().any(|d| d.pos == q);
    let free_grass =
        |q: TilePos| q.z == GROUND_Z && m.get(q) == Some(Tile::Grass) && !pinned(q) && !occupied(q);
    match *ev {
        FoodEvent::CowMoved { from, to } => {
            m.get(from) == Some(Tile::Cow)
                && !pinned(from)
                && Dir::HORIZONTAL.iter().any(|&d| from.step(d) == to)
                && free_grass(to)
        }
        FoodEvent::CowBorn { at } => free_grass(at),
        // A ripe crop asks again at every check: harmless, so not refused.
        FoodEvent::CropRipe { at } => matches!(m.get(at), Some(Tile::Field | Tile::Crop)),
    }
}

/// Applies the valid events to the grid in order and records their tiles; returns the
/// rejected ones.
pub fn apply_events(
    m: &mut Mountain,
    f: &Fortress,
    evs: &[FoodEvent],
    tiles: &mut Vec<(TilePos, Tile)>,
) -> Vec<FoodEvent> {
    let mut rejected = vec![];
    for ev in evs {
        if !validate(m, f, ev) {
            rejected.push(*ev);
            continue;
        }
        let changed: &[(TilePos, Tile)] = match *ev {
            FoodEvent::CowMoved { from, to } => &[(from, Tile::Grass), (to, Tile::Cow)],
            FoodEvent::CowBorn { at } => &[(at, Tile::Cow)],
            FoodEvent::CropRipe { at } if m.get(at) == Some(Tile::Field) => &[(at, Tile::Crop)],
            FoodEvent::CropRipe { .. } => &[],
        };
        for &(q, t) in changed {
            m.set(q, t);
            tiles.push((q, t));
        }
    }
    rejected
}

/// Every cow, field and crop of the ground level.
pub fn snapshot(m: &Mountain) -> Vec<Cell> {
    let ground = |x, y| TilePos::new(x, y, GROUND_Z);
    m.rect(ground(0, 0), ground(i16::MAX, i16::MAX))
        .into_iter()
        .filter_map(|pos| {
            let kind = match m.get(pos)? {
                Tile::Cow => CellKind::Cow,
                Tile::Field => CellKind::Field,
                Tile::Crop => CellKind::Crop,
                _ => return None,
            };
            Some(Cell { pos, kind })
        })
        .collect()
}

/// A ripe crop asks for its harvest, and asks again if that was cancelled or the
/// designations were full. Runs with or without a food simulation.
pub fn keep_harvest_designated(m: &mut Mountain, ch: &mut Changes) {
    // Past the designation cap: crops are bounded by the ground, and a famine hides every
    // other designation, so a capped harvest would never come.
    for q in on_ground(m, Tile::Crop) {
        if let Entry::Vacant(e) = m.designations.entry(q) {
            e.insert(DesignationKind::Chop);
            ch.designations.insert(q, Some(DesignationKind::Chop));
        }
    }
}

/// The `FoodSim`s run inside the server when no bus is configured, one per mountain. Not
/// saved: a restart rebuilds them from the grid with default hidden state.
#[derive(Default)]
pub struct InProcess {
    sims: BTreeMap<MountainId, FoodSim>,
}

/// Deterministic per mountain: the world's seed mixed with the mountain id.
fn seed(world_seed: u64, m: MountainId) -> u64 {
    world_seed ^ u64::from(m.0).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// Food part of a tick, after `sim::step` (`w.tick` is already the delta's tick). `sims` is
/// `None` when the food comes from the bus: only the harvest designations are kept then.
pub fn step(
    w: &mut World,
    mut sims: Option<&mut InProcess>,
    actions: &BTreeMap<MountainId, Vec<FoodAction>>,
) -> BTreeMap<MountainId, Changes> {
    let tick = w.tick;
    let mut out = BTreeMap::new();
    for (&id, f) in &w.fortresses {
        let Some(m) = w.mountains.get_mut(&id) else {
            continue;
        };
        let mut ch = Changes::default();
        if let Some(sims) = sims.as_deref_mut() {
            let sim = sims.sims.entry(id).or_insert_with(|| {
                let mut s = FoodSim::new(seed(w.seed, id));
                s.adopt(&snapshot(m), tick);
                s
            });
            for a in actions.get(&id).into_iter().flatten() {
                sim.apply_action(a);
            }
            if tick.saturating_sub(sim.adopted_at()) >= RESYNC_EVERY {
                sim.adopt(&snapshot(m), tick);
            }
            for ev in apply_events(m, f, &sim.step(tick), &mut ch.tiles) {
                sim.reject(&ev);
            }
        }
        if tick.is_multiple_of(GROW_EVERY) {
            keep_harvest_designated(m, &mut ch);
        }
        if ch != Changes::default() {
            out.insert(id, ch);
        }
    }
    out
}

/// Folds food changes, which happen after the step, into its delta: their tiles go last.
pub fn fold(mut ch: Changes, d: &mut TickDelta) {
    d.tiles.append(&mut ch.tiles);
    ch.merge_into(d);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::tests::{M, assign, cow_pen, cows, designate, hero, p, set_str, setup, tile};
    use crate::sim::{Command, apply_command, step_with_food};
    use moria_core::MAX_DESIGNATIONS;
    use moria_core::calendar::SEASON_TICKS;
    use moria_core::srd::hero_dig_ticks;
    use std::collections::HashSet;

    const WINTER: u64 = 3 * SEASON_TICKS;

    /// A grass meadow; the fortress of `setup()` stands far from it.
    fn meadow() -> (Mountain, Fortress) {
        let m = Mountain::new(M, 41, 41, GROUND_Z + 1, Tile::Grass);
        (m, setup().fortresses[&M].clone())
    }
    fn g(x: i16, y: i16) -> TilePos {
        TilePos::new(x, y, GROUND_Z)
    }
    fn moved(from: TilePos, to: TilePos) -> FoodEvent {
        FoodEvent::CowMoved { from, to }
    }

    /// One tick of the server without bus: sim, then food, folded into the delta.
    fn run(w: &mut World, sims: &mut InProcess) -> (Option<TickDelta>, Vec<FoodAction>) {
        let (mut d, mut acts) = step_with_food(w);
        let mut ch = step(w, Some(sims), &acts);
        let mut d = d.remove(&M);
        if let (Some(d), Some(c)) = (d.as_mut(), ch.remove(&M)) {
            fold(c, d);
        }
        (d, acts.remove(&M).unwrap_or_default())
    }

    #[test]
    fn move_of_a_butchered_cow_is_refused() {
        let (mut m, f) = meadow();
        let c = g(20, 20);
        m.set(c, Tile::Cow);
        let ev = moved(c, c.step(Dir::E));
        assert!(validate(&m, &f, &ev));
        m.set(c, Tile::Grass); // butchered before the stale proposal arrives
        assert!(!validate(&m, &f, &ev));
        let mut tiles = vec![];
        assert_eq!(apply_events(&mut m, &f, &[ev], &mut tiles), vec![ev]);
        assert!(tiles.is_empty());
        assert_eq!(m.get(c.step(Dir::E)), Some(Tile::Grass), "no cow reappears");
    }

    #[test]
    fn pinned_cow_move_is_refused() {
        let (mut m, mut f) = meadow();
        let c = g(20, 20);
        let to = c.step(Dir::E);
        m.set(c, Tile::Cow);
        assert!(validate(&m, &f, &moved(c, to)));
        // designated
        m.designate(c, c, DesignationKind::Chop);
        assert!(!validate(&m, &f, &moved(c, to)));
        m.designations.clear();
        // the hero's target
        f.hero.as_mut().unwrap().dig = Some(moria_core::world::DigJob {
            target: c,
            kind: DesignationKind::Chop,
            progress: 0,
        });
        assert!(!validate(&m, &f, &moved(c, to)));
        f.hero.as_mut().unwrap().dig = None;
        // a dwarf stands on the destination
        f.dwarves[0].pos = to;
        assert!(!validate(&m, &f, &moved(c, to)));
        f.dwarves[0].pos = c.step(Dir::W);
        // so does the hero
        f.hero.as_mut().unwrap().pos = to;
        assert!(!validate(&m, &f, &moved(c, to)));
    }

    #[test]
    fn valid_move_updates_the_grid_and_emits_tiles() {
        let (mut m, f) = meadow();
        let c = g(20, 20);
        let to = c.step(Dir::N);
        m.set(c, Tile::Cow);
        let far = moved(c, g(25, 20));
        let mut tiles = vec![];
        let rejected = apply_events(&mut m, &f, &[moved(c, to), far], &mut tiles);
        assert_eq!(rejected, vec![far], "no teleport, and the cow left anyway");
        assert_eq!((m.get(c), m.get(to)), (Some(Tile::Grass), Some(Tile::Cow)));
        assert_eq!(tiles, vec![(c, Tile::Grass), (to, Tile::Cow)]);
        let mut wall = Mountain::new(M, 41, 41, GROUND_Z + 1, Tile::Tree);
        wall.set(c, Tile::Cow);
        assert!(!validate(&wall, &f, &moved(c, to)), "only onto grass");
    }

    #[test]
    fn birth_needs_free_grass() {
        let (mut m, mut f) = meadow();
        let (a, b, t, d) = (g(1, 1), g(3, 3), g(5, 5), g(7, 7));
        m.set(b, Tile::Cow);
        m.set(t, Tile::Tree);
        f.dwarves[0].pos = d;
        let born = |at| FoodEvent::CowBorn { at };
        let mut tiles = vec![];
        let rejected = apply_events(
            &mut m,
            &f,
            &[born(a), born(b), born(t), born(d)],
            &mut tiles,
        );
        assert_eq!(rejected, vec![born(b), born(t), born(d)]);
        assert_eq!(m.get(a), Some(Tile::Cow));
        assert_eq!(tiles, vec![(a, Tile::Cow)]);
    }

    #[test]
    fn ripe_turns_a_field_into_a_crop() {
        let (mut m, f) = meadow();
        let (q, c, grass) = (g(1, 1), g(2, 2), g(3, 3));
        m.set(q, Tile::Field);
        m.set(c, Tile::Crop);
        let ripe = |at| FoodEvent::CropRipe { at };
        let mut tiles = vec![];
        let rejected = apply_events(&mut m, &f, &[ripe(q), ripe(c), ripe(grass)], &mut tiles);
        // An already ripe crop asks again at every check: accepted, nothing to change.
        assert_eq!(rejected, vec![ripe(grass)]);
        assert_eq!(m.get(q), Some(Tile::Crop));
        assert_eq!(tiles, vec![(q, Tile::Crop)]);
    }

    #[test]
    fn ripe_crops_stay_designated_without_food_events() {
        let mut m = Mountain::new(M, 80, 80, GROUND_Z + 1, Tile::Rock);
        m.designate(
            TilePos::new(0, 0, 0),
            TilePos::new(79, 79, 0),
            DesignationKind::Dig,
        );
        assert_eq!(m.designations.len(), MAX_DESIGNATIONS);
        let c = g(1, 1);
        m.set(c, Tile::Crop);
        let mut ch = Changes::default();
        keep_harvest_designated(&mut m, &mut ch);
        assert_eq!(m.designations.get(&c), Some(&DesignationKind::Chop));
        assert_eq!(ch.designations.get(&c), Some(&Some(DesignationKind::Chop)));

        // and through the server's food step, with no FoodSim at all
        let mut w = setup();
        let c = p(-3, 1, 0);
        w.mountains.get_mut(&M).unwrap().set(c, Tile::Crop);
        w.tick = GROW_EVERY;
        let ch = step(&mut w, None, &BTreeMap::new());
        assert_eq!(
            ch[&M].designations.get(&c),
            Some(&Some(DesignationKind::Chop))
        );
    }

    #[test]
    fn an_unasked_crop_asks_again() {
        let mut m = Mountain::new(M, 4, 4, GROUND_Z + 1, Tile::Grass);
        let c = g(1, 1);
        m.set(c, Tile::Crop); // e.g. its harvest was cancelled
        let mut ch = Changes::default();
        keep_harvest_designated(&mut m, &mut ch);
        assert_eq!(m.designations.get(&c), Some(&DesignationKind::Chop));
        assert_eq!(ch.designations.get(&c), Some(&Some(DesignationKind::Chop)));
    }

    #[test]
    fn snapshot_lists_cows_fields_and_crops_of_the_ground_level() {
        let mut m = Mountain::new(M, 6, 6, GROUND_Z + 1, Tile::Grass);
        m.set(g(1, 1), Tile::Cow);
        m.set(g(2, 1), Tile::Field);
        m.set(g(3, 1), Tile::Crop);
        m.set(g(4, 1), Tile::Tree);
        m.set(TilePos::new(1, 2, GROUND_Z - 1), Tile::Cow); // not the ground level
        let mut cells = snapshot(&m);
        cells.sort_by_key(|c| c.pos);
        let cell = |x, kind| Cell { pos: g(x, 1), kind };
        assert_eq!(
            cells,
            vec![
                cell(1, CellKind::Cow),
                cell(2, CellKind::Field),
                cell(3, CellKind::Crop)
            ]
        );
    }

    /// Steps until `done` holds (at most 300 ticks); returns every food action seen.
    fn actions_until(w: &mut World, done: impl Fn(&World) -> bool) -> Vec<FoodAction> {
        let mut seen = vec![];
        for _ in 0..300 {
            let (_, mut acts) = step_with_food(w);
            seen.append(&mut acts.remove(&M).unwrap_or_default());
            if done(w) {
                return seen;
            }
        }
        panic!("never done; actions {seen:?}");
    }

    #[test]
    fn butchering_a_cow_emits_cow_removed() {
        let mut w = setup();
        let c = p(4, 0, 0);
        w.mountains.get_mut(&M).unwrap().set(c, Tile::Cow);
        w.fortresses.get_mut(&M).unwrap().dwarves[0].pos = p(3, 0, 0);
        designate(&mut w, c, DesignationKind::Chop);
        assign(&mut w, 0, c);
        let seen = actions_until(&mut w, |w| tile(w, c) == Tile::Grass);
        assert_eq!(seen, vec![FoodAction::CowRemoved(c)]);
    }

    #[test]
    fn building_a_farm_emits_field_sown() {
        let mut w = setup();
        let q = p(-3, 1, 0);
        for t in [q, q.step(Dir::E)] {
            w.mountains.get_mut(&M).unwrap().set(t, Tile::Grass);
        }
        designate(&mut w, q, DesignationKind::Farm);
        w.fortresses.get_mut(&M).unwrap().dwarves[0].pos = q.step(Dir::E);
        assign(&mut w, 0, q);
        let seen = actions_until(&mut w, |w| tile(w, q) == Tile::Field);
        assert_eq!(seen, vec![FoodAction::FieldSown(q)]);
    }

    #[test]
    fn harvest_emits_field_harvested() {
        let mut w = setup();
        set_str(&mut w, 10);
        let c = p(3, 1, 0);
        w.mountains.get_mut(&M).unwrap().set(c, Tile::Crop);
        apply_command(&mut w, M, Command::Dig(Dir::E));
        assert!(hero(&w).dig.is_some());
        let seen = actions_until(&mut w, |w| tile(w, c) == Tile::Field);
        assert_eq!(seen, vec![FoodAction::FieldHarvested(c)]);
    }

    #[test]
    fn digging_a_field_emits_field_removed() {
        let mut w = setup();
        let q = p(-3, 1, 0);
        w.mountains.get_mut(&M).unwrap().set(q, Tile::Field);
        w.mountains
            .get_mut(&M)
            .unwrap()
            .set(q.step(Dir::E), Tile::Grass);
        designate(&mut w, q, DesignationKind::Dig);
        w.fortresses.get_mut(&M).unwrap().dwarves[0].pos = q.step(Dir::E);
        assign(&mut w, 0, q);
        assert!(w.fortresses[&M].dwarves[0].task.is_some());
        let seen = actions_until(&mut w, |w| tile(w, q) == Tile::Grass);
        assert_eq!(seen, vec![FoodAction::FieldRemoved(q)]);
    }

    #[test]
    fn a_free_cow_roams_the_meadow() {
        let (mut m, f) = meadow();
        let mut c = g(20, 20);
        m.set(c, Tile::Cow);
        let mut sim = FoodSim::new(1);
        sim.adopt(&snapshot(&m), WINTER); // no births in winter: one cow to follow
        let mut seen = HashSet::from([c]);
        for n in 1..=300 {
            let evs = sim.step(WINTER + 20 * n);
            let rejected = apply_events(&mut m, &f, &evs, &mut vec![]);
            for ev in &rejected {
                sim.reject(ev);
            }
            if let Some(FoodEvent::CowMoved { to, .. }) =
                evs.first().filter(|e| !rejected.contains(e))
            {
                c = *to;
            }
            assert_eq!(m.get(c), Some(Tile::Cow));
            seen.insert(c);
        }
        assert!(seen.len() > 20, "the cow only visits {} tiles", seen.len());
    }

    #[test]
    fn fields_ripen_and_call_for_harvest() {
        let (mut m, f) = meadow();
        let fields: Vec<_> = m.rect(g(0, 0), g(9, 9));
        for &q in &fields {
            m.set(q, Tile::Field);
        }
        let ripe = |m: &Mountain| {
            fields
                .iter()
                .filter(|&&q| m.get(q) == Some(Tile::Crop))
                .count()
        };
        let mut sim = FoodSim::new(1);
        sim.adopt(&snapshot(&m), SEASON_TICKS); // summer
        let (mut tiles, mut ch) = (vec![], Changes::default());
        let mut check = |m: &mut Mountain, n: u64| {
            let evs = sim.step(SEASON_TICKS + n * GROW_EVERY);
            assert!(apply_events(m, &f, &evs, &mut tiles).is_empty());
            keep_harvest_designated(m, &mut ch);
        };
        check(&mut m, 0);
        assert!(ripe(&m) <= 10, "a field takes its time: {}", ripe(&m));
        for n in 1..200 {
            check(&mut m, n);
        }
        assert!(
            ripe(&m) >= 60,
            "most fields ripen within a few hours: {}",
            ripe(&m)
        );
        for &q in fields.iter().filter(|&&q| m.get(q) == Some(Tile::Crop)) {
            assert_eq!(
                m.designations.get(&q),
                Some(&DesignationKind::Chop),
                "harvest asked"
            );
            assert_eq!(ch.designations.get(&q), Some(&Some(DesignationKind::Chop)));
            assert!(tiles.contains(&(q, Tile::Crop)));
        }
    }

    #[test]
    fn cows_wander_onto_grass() {
        let mut w = setup();
        w.tick = WINTER; // no births: the herd keeps its size
        let c = p(-6, 1, 0);
        cow_pen(&mut w, c, &[Dir::N]);
        let herd = cows(&w);
        let (mut sims, mut seen) = (InProcess::default(), vec![]);
        for _ in 0..600 {
            seen.extend(run(&mut w, &mut sims).0.unwrap().tiles);
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
        let mut sims = InProcess::default();
        for _ in 0..300 {
            run(&mut w, &mut sims);
        }
        assert_eq!(tile(&w, c), Tile::Cow);
    }

    #[test]
    fn cow_never_steps_onto_the_hero() {
        let mut w = setup();
        let c = p(-6, 1, 0);
        cow_pen(&mut w, c, &[Dir::N]);
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = c.step(Dir::N);
        let mut sims = InProcess::default();
        for _ in 0..300 {
            run(&mut w, &mut sims);
        }
        assert_eq!(tile(&w, c), Tile::Cow);
        assert_ne!(tile(&w, c.step(Dir::N)), Tile::Cow);
    }

    #[test]
    fn hero_butchers_a_cow_that_waits_with_food_running() {
        let mut w = setup();
        set_str(&mut w, 10);
        let c = p(-6, 1, 0);
        cow_pen(&mut w, c, &[Dir::N, Dir::S]);
        w.fortresses.get_mut(&M).unwrap().hero.as_mut().unwrap().pos = c.step(Dir::S);
        apply_command(&mut w, M, Command::Dig(Dir::N));
        let n = hero_dig_ticks(Tile::Cow.dig_ticks(), &hero(&w).abilities);
        let mut sims = InProcess::default();
        for _ in 1..n {
            run(&mut w, &mut sims);
            assert_eq!(tile(&w, c), Tile::Cow, "the cow waits for the hero");
        }
        let (_, acts) = run(&mut w, &mut sims);
        assert_eq!(tile(&w, c), Tile::Grass);
        assert_eq!(acts, vec![FoodAction::CowRemoved(c)]);
        assert_eq!(w.fortresses[&M].resources.food, 4);
    }
}
