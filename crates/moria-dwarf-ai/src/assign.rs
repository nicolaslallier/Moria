use moria_core::ai::{AiView, MAX_DWARVES_PER_VIEW};
use moria_core::path::distances_from;
use moria_core::{Dir, DwarfId, TilePos};
use moria_core::{MOUNTAIN_D, MOUNTAIN_H, MOUNTAIN_W};
use std::collections::HashSet;

/// A deserialized view is untrusted: postcard bypasses `Mountain`'s invariants.
pub fn validate(view: &AiView) -> Result<(), &'static str> {
    let m = &view.mountain;
    let (w, h, d) = m.size();
    if w <= 0 || h <= 0 || d == 0 || w > MOUNTAIN_W || h > MOUNTAIN_H || d > MOUNTAIN_D {
        return Err("dimensions invalides");
    }
    if !m.is_consistent() {
        return Err("tuiles incoherentes");
    }
    if view.dwarves.len() > MAX_DWARVES_PER_VIEW {
        return Err("trop de nains");
    }
    if m.designations.len() > w as usize * h as usize * d as usize {
        return Err("trop de designations");
    }
    let ok = view.dwarves.iter().all(|d| m.in_bounds(d.pos))
        && m.designations.keys().all(|&p| m.in_bounds(p))
        && view
            .claimed
            .iter()
            .chain(&view.occupied)
            .all(|&p| m.in_bounds(p));
    if ok {
        Ok(())
    } else {
        Err("position hors limites")
    }
}

/// Which idle dwarf digs which open designation.
pub fn assign(view: &AiView) -> Vec<(DwarfId, TilePos)> {
    let m = &view.mountain;
    let claimed: HashSet<TilePos> = view.claimed.iter().copied().collect();
    let occupied: HashSet<TilePos> = view.occupied.iter().copied().collect();
    // BTreeMap keys are already sorted by TilePos. The server rejects undiggable targets.
    let tasks: Vec<TilePos> = m
        .designations
        .iter()
        .filter(|&(&t, &k)| m.diggable(t, k) && !claimed.contains(&t) && !occupied.contains(&t))
        .map(|(&t, _)| t)
        .collect();
    let mut dwarves: Vec<_> = view.dwarves.iter().filter(|d| d.idle).collect();
    dwarves.sort_by_key(|d| d.id);
    if tasks.is_empty() || dwarves.is_empty() {
        return vec![];
    }
    // Dwarves don't block each other: empty `blocked`.
    // ponytail: one full BFS per idle dwarf per view (<= 64 dwarves x walkable tiles: BFS over ~12k
    // outdoor tiles on the 176x96x12 map plus whatever is dug); if that gets too slow, run a single multi-source BFS from the tasks' work spots instead.
    let no_block = HashSet::new();
    let dist: Vec<Vec<Option<u32>>> = dwarves
        .iter()
        .map(|d| {
            let dm = distances_from(m, d.pos, &no_block);
            tasks
                .iter()
                .map(|&t| {
                    Dir::HORIZONTAL
                        .iter()
                        .map(|&dir| t.step(dir))
                        .filter(|&s| m.walkable(s))
                        .filter_map(|s| dm.get(&s).copied())
                        .min()
                })
                .collect()
        })
        .collect();
    solve(&dist)
        .iter()
        .zip(&dwarves)
        .filter_map(|(t, d)| t.map(|t| (d.id, tasks[t])))
        .collect()
}

/// `dist[d][t]`: moves for dwarf `d` to reach a work spot of task `t`. Indices must be in
/// DwarfId / TilePos order so ties break by lowest id, then lowest position.
/// Returns the chosen task index per dwarf.
fn solve(dist: &[Vec<Option<u32>>]) -> Vec<Option<usize>> {
    let nd = dist.len();
    let nt = dist.first().map_or(0, Vec::len);
    let mut got: Vec<Option<usize>> = vec![None; nd];
    let mut taken = vec![false; nt];
    // Greedy: globally shortest pair first; (d, t) index order breaks ties.
    while let Some((_, d, t)) = (0..nd)
        .filter(|&d| got[d].is_none())
        .flat_map(|d| (0..nt).filter(|&t| !taken[t]).map(move |t| (d, t)))
        .filter_map(|(d, t)| dist[d][t].map(|c| (c, d, t)))
        .min()
    {
        got[d] = Some(t);
        taken[t] = true;
    }
    // Improvement: pairwise swaps and moves to free tasks, only on strict gain.
    // Each step lowers the total, so this terminates.
    let cost = |d: usize, t: usize| dist[d][t];
    loop {
        let mut improved = false;
        for i in 0..nd {
            let Some(ti) = got[i] else { continue };
            for j in i + 1..nd {
                let Some(tj) = got[j] else { continue };
                if let (Some(a), Some(b), Some(c), Some(e)) =
                    (cost(i, tj), cost(j, ti), cost(i, ti), cost(j, tj))
                    && a + b < c + e
                {
                    got.swap(i, j);
                    improved = true;
                    break;
                }
            }
            let Some(ti) = got[i] else { continue };
            for t in (0..nt).filter(|&t| !taken[t]) {
                if let (Some(n), Some(c)) = (cost(i, t), cost(i, ti))
                    && n < c
                {
                    taken[ti] = false;
                    taken[t] = true;
                    got[i] = Some(t);
                    improved = true;
                    break;
                }
            }
        }
        // Augment: an unassigned dwarf takes a held task if its holder can move to a free one.
        // The assigned count grows each time, so this terminates.
        'aug: for u in (0..nd).filter(|&u| got[u].is_none()) {
            for k in 0..nd {
                let Some(t) = got[k] else { continue };
                if cost(u, t).is_none() {
                    continue;
                }
                if let Some(f) = (0..nt).find(|&f| !taken[f] && cost(k, f).is_some()) {
                    got[u] = Some(t);
                    got[k] = Some(f);
                    taken[f] = true;
                    improved = true;
                    break 'aug;
                }
            }
        }
        if !improved {
            return got;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moria_core::ai::DwarfView;
    use moria_core::{DesignationKind, Mountain, MountainId, Tile};

    fn p(x: i16, y: i16) -> TilePos {
        TilePos::new(x, y, 0)
    }

    /// Corridor along y=1, x in 1..=len; rock elsewhere.
    fn corridor(len: i16) -> Mountain {
        let mut m = Mountain::new(MountainId(0), 40, 8, 1, Tile::Rock);
        for x in 1..=len {
            m.set(p(x, 1), Tile::Floor);
        }
        m
    }

    /// Designate the rock tile above corridor column x (work spot = (x,1)).
    fn task(m: &mut Mountain, x: i16) -> TilePos {
        m.designate(p(x, 0), p(x, 0), DesignationKind::Dig);
        p(x, 0)
    }

    fn dw(id: u32, x: i16, idle: bool) -> DwarfView {
        DwarfView {
            id: DwarfId(id),
            pos: p(x, 1),
            idle,
        }
    }

    fn view(
        m: Mountain,
        dwarves: Vec<DwarfView>,
        claimed: Vec<TilePos>,
        occupied: Vec<TilePos>,
    ) -> AiView {
        AiView {
            mountain: m,
            tick: 0,
            dwarves,
            claimed,
            occupied,
        }
    }

    #[test]
    fn idle_dwarf_takes_reachable_task() {
        let mut m = corridor(10);
        let t = task(&mut m, 5);
        assert_eq!(
            assign(&view(m, vec![dw(1, 1, true)], vec![], vec![])),
            vec![(DwarfId(1), t)]
        );
    }

    #[test]
    fn nearest_task_wins() {
        let mut m = corridor(30);
        let _far = task(&mut m, 25);
        let near = task(&mut m, 3);
        assert_eq!(
            assign(&view(m, vec![dw(1, 1, true)], vec![], vec![])),
            vec![(DwarfId(1), near)]
        );
    }

    #[test]
    fn two_dwarves_never_share_a_task() {
        let mut m = corridor(10);
        let a = task(&mut m, 4);
        let b = task(&mut m, 6);
        let r = assign(&view(
            m,
            vec![dw(1, 5, true), dw(2, 5, true)],
            vec![],
            vec![],
        ));
        assert_eq!(r.len(), 2);
        assert_ne!(r[0].1, r[1].1);
        assert!(r.iter().all(|&(_, t)| t == a || t == b));
    }

    #[test]
    fn global_nearest_pairing() {
        // d1->A=3, d1->B=2, d2->B=3, d2->A=10. Plain greedy gives d1->B, d2->A (12); optimum is 6.
        let dist = vec![vec![Some(3), Some(2)], vec![Some(10), Some(3)]];
        assert_eq!(solve(&dist), vec![Some(0), Some(1)]);
    }

    #[test]
    fn dwarf_that_reaches_nothing_gets_nothing() {
        let dist = vec![vec![Some(5), Some(2)], vec![None, None]];
        assert_eq!(solve(&dist), vec![Some(1), None]);
    }

    #[test]
    fn unassigned_dwarf_gets_work_via_reassignment() {
        let dist = vec![vec![Some(1), Some(5)], vec![Some(9), None]];
        assert_eq!(solve(&dist), vec![Some(1), Some(0)]);
    }

    #[test]
    fn invalid_view_rejected() {
        let mut m = corridor(10);
        task(&mut m, 5);
        let ok = view(m.clone(), vec![dw(1, 1, true)], vec![], vec![]);
        assert!(validate(&ok).is_ok());
        // out-of-bounds dwarf
        let mut bad = ok.clone();
        bad.dwarves[0].pos = TilePos::new(500, 1, 0);
        assert!(validate(&bad).is_err());
        // truncated tiles vec, via a serialized round trip with the tiles cut short
        // truncated tiles vec: build the raw wire form with too few tiles
        #[derive(serde::Serialize)]
        struct Raw(
            MountainId,
            i16,
            i16,
            u8,
            Vec<Tile>,
            std::collections::BTreeMap<TilePos, DesignationKind>,
        );
        let raw = Raw(
            MountainId(0),
            40,
            8,
            1,
            vec![Tile::Rock; 10],
            Default::default(),
        );
        let trunc: Mountain = postcard::from_bytes(&postcard::to_allocvec(&raw).unwrap()).unwrap();
        assert!(validate(&view(trunc, vec![], vec![], vec![])).is_err());
    }

    #[test]
    fn too_many_dwarves_rejected() {
        let m = corridor(10);
        let ds = (0..65).map(|i| dw(i, 1, true)).collect();
        assert!(validate(&view(m, ds, vec![], vec![])).is_err());
    }

    #[test]
    fn ties_break_by_lowest_ids() {
        let mut m = corridor(10);
        let a = task(&mut m, 3);
        let _b = task(&mut m, 7);
        // both dwarves at x=5: equidistant to both tasks.
        let r = assign(&view(
            m,
            vec![dw(2, 5, true), dw(1, 5, true)],
            vec![],
            vec![],
        ));
        assert!(r.contains(&(DwarfId(1), a)));
    }

    #[test]
    fn unreachable_task_not_assigned_then_assigned_when_opened() {
        let mut m = corridor(5);
        m.set(p(8, 1), Tile::Floor); // sealed pocket
        let t = task(&mut m, 8);
        assert!(assign(&view(m.clone(), vec![dw(1, 1, true)], vec![], vec![])).is_empty());
        for x in 6..=7 {
            m.set(p(x, 1), Tile::Floor);
        }
        assert_eq!(
            assign(&view(m, vec![dw(1, 1, true)], vec![], vec![])),
            vec![(DwarfId(1), t)]
        );
    }

    #[test]
    fn busy_and_claimed_are_skipped() {
        let mut m = corridor(10);
        let a = task(&mut m, 3);
        let b = task(&mut m, 8);
        let r = assign(&view(
            m,
            vec![dw(1, 1, false), dw(2, 1, true)],
            vec![a],
            vec![],
        ));
        assert_eq!(r, vec![(DwarfId(2), b)]);
    }

    #[test]
    fn task_under_occupant_not_assigned() {
        let mut m = corridor(10);
        let t = task(&mut m, 4);
        assert!(assign(&view(m, vec![dw(1, 1, true)], vec![], vec![t])).is_empty());
    }

    #[test]
    fn undiggable_designation_not_assigned() {
        let mut m = corridor(10);
        // Stale designation on a tile already turned into Stair (e.g. under a stairway).
        m.set(p(2, 0), Tile::Stair);
        m.designations.insert(p(2, 0), DesignationKind::Dig);
        let valid = task(&mut m, 8);
        assert_eq!(
            assign(&view(m, vec![dw(1, 1, true)], vec![], vec![])),
            vec![(DwarfId(1), valid)]
        );
    }

    /// Outdoor strip: Sky at z=0, Grass at z=1 (the ground), 40x8x2.
    fn outdoors() -> Mountain {
        let mut m = Mountain::new(MountainId(0), 40, 8, 2, Tile::Sky);
        for y in 0..8 {
            for x in 0..40 {
                m.set(TilePos::new(x, y, 1), Tile::Grass);
            }
        }
        m
    }

    fn at(id: u32, x: i16, y: i16, z: u8) -> DwarfView {
        DwarfView {
            id: DwarfId(id),
            pos: TilePos::new(x, y, z),
            idle: true,
        }
    }

    #[test]
    fn tree_across_water_not_assigned() {
        let mut m = outdoors();
        for y in 0..8 {
            m.set(TilePos::new(5, y, 1), Tile::Water);
        }
        let tree = TilePos::new(8, 1, 1);
        m.set(tree, Tile::Tree);
        m.designate(tree, tree, DesignationKind::Chop);
        let dig = TilePos::new(2, 3, 1);
        m.set(dig, Tile::Rock);
        m.designate(dig, dig, DesignationKind::Dig);
        let v = view(m.clone(), vec![at(1, 2, 1, 1)], vec![], vec![]);
        assert_eq!(assign(&v), vec![(DwarfId(1), dig)]);
        // Without the reachable dig, the cut-off tree gets nobody.
        m.designations.remove(&dig);
        assert!(assign(&view(m, vec![at(1, 2, 1, 1)], vec![], vec![])).is_empty());
    }

    #[test]
    fn nearest_tree_wins_via_slope() {
        let mut m = outdoors();
        // Hill x 5..=8 (rock at z=1, grass on top at z=0), climbed by a slope at x=4.
        for y in 0..8 {
            for x in 5..=8 {
                m.set(TilePos::new(x, y, 1), Tile::Rock);
                m.set(TilePos::new(x, y, 0), Tile::Grass);
            }
        }
        m.set(TilePos::new(4, 1, 1), Tile::Slope);
        let hill = TilePos::new(7, 1, 0);
        let flat = TilePos::new(3, 7, 1);
        for t in [hill, flat] {
            m.set(t, Tile::Tree);
            m.designate(t, t, DesignationKind::Chop);
        }
        // Hill tree: 3 moves via the slope; flat tree: 5. The hill is unreachable without slopes.
        let v = view(m, vec![at(1, 3, 1, 1)], vec![], vec![]);
        assert_eq!(assign(&v), vec![(DwarfId(1), hill)]);
    }

    #[test]
    fn full_size_view_validates() {
        let w = moria_core::world::generate(1);
        let m = w.mountains.values().next().unwrap().clone();
        let v = view(m, vec![at(1, 130, 47, 4)], vec![], vec![]);
        assert!(validate(&v).is_ok());
    }
}
