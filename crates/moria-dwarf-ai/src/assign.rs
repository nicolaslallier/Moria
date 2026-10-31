use moria_core::ai::AiView;
use moria_core::path::distances_from;
use moria_core::{Dir, DwarfId, TilePos};
use std::collections::HashSet;

/// Which idle dwarf digs which open designation.
pub fn assign(view: &AiView) -> Vec<(DwarfId, TilePos)> {
    let m = &view.mountain;
    let claimed: HashSet<TilePos> = view.claimed.iter().copied().collect();
    let occupied: HashSet<TilePos> = view.occupied.iter().copied().collect();
    // BTreeMap keys are already sorted by TilePos.
    let tasks: Vec<TilePos> = m
        .designations
        .keys()
        .copied()
        .filter(|t| !claimed.contains(t) && !occupied.contains(t))
        .collect();
    let mut dwarves: Vec<_> = view.dwarves.iter().filter(|d| d.idle).collect();
    dwarves.sort_by_key(|d| d.id);
    if tasks.is_empty() || dwarves.is_empty() {
        return vec![];
    }
    // Dwarves don't block each other: empty `blocked`.
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
}
