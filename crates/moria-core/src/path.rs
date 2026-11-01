use crate::{Dir, Mountain, TilePos};
use std::collections::{HashMap, HashSet, VecDeque};

/// BFS shortest path. Excludes `from`, includes the goal tile.
/// Neighbours are expanded in a fixed order, so results are deterministic.
pub fn find_path(
    m: &Mountain,
    from: TilePos,
    goal: impl Fn(TilePos) -> bool,
    blocked: &HashSet<TilePos>,
) -> Option<Vec<TilePos>> {
    if goal(from) {
        return Some(vec![]);
    }
    let mut prev: HashMap<TilePos, TilePos> = HashMap::new();
    let mut q = VecDeque::from([from]);
    while let Some(p) = q.pop_front() {
        for d in [Dir::N, Dir::S, Dir::E, Dir::W, Dir::Up, Dir::Down] {
            let Some(n) = m.move_dest(p, d) else { continue };
            if n == from || prev.contains_key(&n) || blocked.contains(&n) {
                continue;
            }
            prev.insert(n, p);
            if goal(n) {
                let mut path = vec![n];
                let mut c = n;
                while let Some(&b) = prev.get(&c) {
                    if b == from {
                        break;
                    }
                    path.push(b);
                    c = b;
                }
                path.reverse();
                return Some(path);
            }
            q.push_back(n);
        }
    }
    None
}

/// BFS distance (in moves) from `from` to every reachable tile, `from` included at 0.
/// Same neighbour rules as `find_path`.
pub fn distances_from(
    m: &Mountain,
    from: TilePos,
    blocked: &HashSet<TilePos>,
) -> HashMap<TilePos, u32> {
    let mut dist = HashMap::from([(from, 0)]);
    let mut q = VecDeque::from([from]);
    while let Some(p) = q.pop_front() {
        let d0 = dist[&p];
        for d in [Dir::N, Dir::S, Dir::E, Dir::W, Dir::Up, Dir::Down] {
            let Some(n) = m.move_dest(p, d) else { continue };
            if dist.contains_key(&n) || blocked.contains(&n) {
                continue;
            }
            dist.insert(n, d0 + 1);
            q.push_back(n);
        }
    }
    dist
}

/// True for a walkable tile horizontally adjacent to `target`, same z.
pub fn work_spot_goal(m: &Mountain, target: TilePos) -> impl Fn(TilePos) -> bool + '_ {
    move |p| {
        p.z == target.z && Dir::HORIZONTAL.iter().any(|&d| target.step(d) == p) && m.walkable(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MountainId, Tile};

    fn carve(m: &mut Mountain, ps: &[(i16, i16, u8)]) {
        for &(x, y, z) in ps {
            m.set(TilePos::new(x, y, z), Tile::Floor);
        }
    }

    #[test]
    fn path_through_tunnel_and_stairs() {
        let mut m = Mountain::new(MountainId(0), 8, 8, 2, Tile::Rock);
        carve(
            &mut m,
            &[
                (1, 1, 0),
                (2, 1, 0),
                (3, 1, 0),
                (3, 2, 0),
                (1, 1, 1),
                (2, 1, 1),
            ],
        );
        m.set(TilePos::new(3, 2, 0), Tile::Stair);
        m.set(TilePos::new(3, 2, 1), Tile::Stair);
        carve(&mut m, &[(3, 3, 1)]);
        let p = find_path(
            &m,
            TilePos::new(1, 1, 0),
            |t| t == TilePos::new(3, 3, 1),
            &HashSet::new(),
        );
        let p = p.unwrap();
        assert_eq!(p.last(), Some(&TilePos::new(3, 3, 1)));
        assert_eq!(p.len(), 5);
    }

    #[test]
    fn no_path_into_sealed_pocket() {
        let mut m = Mountain::new(MountainId(0), 8, 8, 1, Tile::Rock);
        carve(&mut m, &[(1, 1, 0), (5, 5, 0)]);
        assert!(
            find_path(
                &m,
                TilePos::new(1, 1, 0),
                |t| t == TilePos::new(5, 5, 0),
                &HashSet::new()
            )
            .is_none()
        );
    }

    #[test]
    fn path_is_shortest() {
        let mut m = Mountain::new(MountainId(0), 8, 8, 1, Tile::Rock);
        carve(&mut m, &[(1, 1, 0), (2, 1, 0), (3, 1, 0), (4, 1, 0)]);
        carve(
            &mut m,
            &[
                (1, 2, 0),
                (1, 3, 0),
                (2, 3, 0),
                (3, 3, 0),
                (4, 3, 0),
                (4, 2, 0),
            ],
        );
        let p = find_path(
            &m,
            TilePos::new(1, 1, 0),
            |t| t == TilePos::new(4, 1, 0),
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(p.len(), 3);
    }

    #[test]
    fn start_is_goal_and_blocked() {
        let mut m = Mountain::new(MountainId(0), 8, 8, 1, Tile::Rock);
        carve(&mut m, &[(1, 1, 0), (2, 1, 0)]);
        let a = TilePos::new(1, 1, 0);
        let b = TilePos::new(2, 1, 0);
        assert_eq!(find_path(&m, a, |t| t == a, &HashSet::new()), Some(vec![]));
        assert!(find_path(&m, a, |t| t == b, &HashSet::from([b])).is_none());
    }

    #[test]
    fn distances_match_find_path() {
        let mut m = Mountain::new(MountainId(0), 8, 8, 1, Tile::Rock);
        carve(&mut m, &[(1, 1, 0), (2, 1, 0), (3, 1, 0), (5, 5, 0)]);
        let d = distances_from(&m, TilePos::new(1, 1, 0), &HashSet::new());
        assert_eq!(d[&TilePos::new(1, 1, 0)], 0);
        assert_eq!(d[&TilePos::new(3, 1, 0)], 2);
        assert!(!d.contains_key(&TilePos::new(5, 5, 0)));
    }

    #[test]
    fn work_spot_is_adjacent_walkable_same_z() {
        let mut m = Mountain::new(MountainId(0), 8, 8, 2, Tile::Rock);
        carve(&mut m, &[(2, 1, 0), (2, 2, 1)]);
        let g = work_spot_goal(&m, TilePos::new(2, 2, 0));
        assert!(g(TilePos::new(2, 1, 0)));
        assert!(!g(TilePos::new(2, 2, 1)));
        assert!(!g(TilePos::new(3, 2, 0)));
    }

    #[test]
    fn path_uses_slopes() {
        let mut m = Mountain::new(MountainId(0), 12, 12, 4, Tile::Sky);
        for y in 0..12 {
            for x in 0..12 {
                m.set(TilePos::new(x, y, 2), Tile::Grass);
            }
        }
        m.set(TilePos::new(5, 5, 2), Tile::Slope);
        m.set(TilePos::new(6, 5, 2), Tile::Soil);
        m.set(TilePos::new(6, 5, 1), Tile::Grass);
        let goal = TilePos::new(6, 5, 1);
        let p = find_path(&m, TilePos::new(2, 5, 2), |t| t == goal, &HashSet::new()).unwrap();
        assert_eq!(p.last(), Some(&goal));
        assert!(p.contains(&TilePos::new(5, 5, 2)));
        assert_eq!(
            distances_from(&m, TilePos::new(2, 5, 2), &HashSet::new())[&goal],
            4
        );
    }
}
