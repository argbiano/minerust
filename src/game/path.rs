// A* over block positions for walking mobs, a small take on vanilla's WalkNodeEvaluator:
// a node is a block a mob's feet can be in, with room for its body above and ground (or
// water to swim in) below. Mobs step up one block, drop up to three, and pay extra for
// water. Unloaded blocks count as walls.

use crate::world::blocks;
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap},
};

pub type Node = [i32; 3];

/// What a mob is allowed to walk through.
#[derive(Clone, Copy)]
pub struct Rules {
    /// Water costs this much more per block; `None` keeps the mob out of water.
    pub water_cost: Option<u32>,
    pub max_drop: i32,
}

pub const WALK: Rules = Rules { water_cost: Some(8), max_drop: 3 };
/// Wandering animals keep their feet dry and avoid big drops, as vanilla's random strolls do.
pub const CAREFUL: Rules = Rules { water_cost: None, max_drop: 1 };

const STEP: u32 = 10;
const DIAGONAL: u32 = 14;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Blocked,
    Open,
    Water,
    Danger,
}

fn kind(state: Option<u16>) -> Kind {
    match state {
        None => Kind::Blocked,
        Some(s) if blocks::is_lava(s) => Kind::Danger,
        Some(s) if blocks::is_liquid(s) => Kind::Water,
        Some(s) if blocks::is_solid(s) => Kind::Blocked,
        Some(_) => Kind::Open,
    }
}

/// Whether a mob's feet can be at `pos`, and if it would be swimming there.
fn standable(block: &impl Fn(i32, i32, i32) -> Option<u16>, [x, y, z]: Node) -> Option<bool> {
    let feet = kind(block(x, y, z));
    let head = kind(block(x, y + 1, z));
    if matches!(feet, Kind::Blocked | Kind::Danger) || matches!(head, Kind::Blocked | Kind::Danger) {
        return None;
    }
    if feet == Kind::Water {
        return Some(true);
    }
    let below = block(x, y - 1, z);
    match kind(below) {
        Kind::Blocked if below.is_some() => Some(false),
        Kind::Water => Some(true),
        _ => None,
    }
}

/// Where a mob ends up stepping from `from` sideways by (dx, dz): level, one up, or a drop.
fn step_target(
    block: &impl Fn(i32, i32, i32) -> Option<u16>,
    rules: Rules,
    [x, y, z]: Node,
    dx: i32,
    dz: i32,
) -> Option<(Node, bool)> {
    let (nx, nz) = (x + dx, z + dz);
    if let Some(swim) = standable(block, [nx, y, nz]) {
        return Some(([nx, y, nz], swim));
    }
    // Step up: needs head room above the mob before it jumps.
    if kind(block(x, y + 2, z)) != Kind::Blocked
        && let Some(swim) = standable(block, [nx, y + 1, nz])
    {
        return Some(([nx, y + 1, nz], swim));
    }
    // Drop down: the column it falls through must be open.
    if !matches!(kind(block(nx, y, nz)), Kind::Open | Kind::Water) || kind(block(nx, y + 1, nz)) == Kind::Blocked {
        return None;
    }
    for d in 1..=rules.max_drop {
        let p = [nx, y - d, nz];
        if let Some(swim) = standable(block, p) {
            return Some((p, swim));
        }
        if !matches!(kind(block(nx, y - d, nz)), Kind::Open | Kind::Water) {
            return None;
        }
    }
    None
}

fn estimate(a: Node, b: Node) -> u32 {
    let (dx, dy, dz) = ((a[0] - b[0]).unsigned_abs(), (a[1] - b[1]).unsigned_abs(), (a[2] - b[2]).unsigned_abs());
    let (lo, hi) = (dx.min(dz), dx.max(dz));
    lo * DIAGONAL + (hi - lo) * STEP + dy * STEP
}

/// The path from `start` to `goal` (both feet positions), excluding `start`. When the goal
/// can't be reached within `max_nodes`, the path leads to the closest place found, as
/// vanilla's does; `None` means the mob can't get any closer.
pub fn find(
    block: &impl Fn(i32, i32, i32) -> Option<u16>,
    rules: Rules,
    start: Node,
    goal: Node,
    max_nodes: usize,
) -> Option<Vec<Node>> {
    let mut open = BinaryHeap::new();
    let mut came_from: HashMap<Node, Node> = HashMap::new();
    let mut cost: HashMap<Node, u32> = HashMap::from([(start, 0)]);
    open.push(Reverse((estimate(start, goal), 0u32, start)));
    let mut best = (estimate(start, goal), start);
    let mut expanded = 0;

    while let Some(Reverse((_, g, node))) = open.pop() {
        if g > cost[&node] {
            continue;
        }
        let h = estimate(node, goal);
        if h < best.0 {
            best = (h, node);
        }
        if node == goal || expanded >= max_nodes {
            break;
        }
        expanded += 1;

        let mut sides = [None; 4];
        for (i, (dx, dz)) in [(1, 0), (-1, 0), (0, 1), (0, -1)].into_iter().enumerate() {
            sides[i] = step_target(block, rules, node, dx, dz);
            if let Some(next) = sides[i] {
                relax(&mut open, &mut came_from, &mut cost, rules, node, g, next, STEP, goal);
            }
        }
        // Diagonals only at the same level, and only past two open sides, so mobs don't cut corners.
        for (dx, dz, a, b) in [(1, 1, 0, 2), (1, -1, 0, 3), (-1, 1, 1, 2), (-1, -1, 1, 3)] {
            let level = |s: Option<(Node, bool)>| s.is_some_and(|(p, _)| p[1] == node[1]);
            if level(sides[a])
                && level(sides[b])
                && let Some(swim) = standable(block, [node[0] + dx, node[1], node[2] + dz])
            {
                let next = ([node[0] + dx, node[1], node[2] + dz], swim);
                relax(&mut open, &mut came_from, &mut cost, rules, node, g, next, DIAGONAL, goal);
            }
        }
    }

    let end = best.1;
    if end == start {
        return None;
    }
    let mut path = vec![end];
    while let Some(&prev) = came_from.get(path.last().unwrap()) {
        if prev == start {
            break;
        }
        path.push(prev);
    }
    path.reverse();
    Some(path)
}

#[allow(clippy::too_many_arguments)]
fn relax(
    open: &mut BinaryHeap<Reverse<(u32, u32, Node)>>,
    came_from: &mut HashMap<Node, Node>,
    cost: &mut HashMap<Node, u32>,
    rules: Rules,
    from: Node,
    g: u32,
    (next, swim): (Node, bool),
    step: u32,
    goal: Node,
) {
    let extra = match (swim, rules.water_cost) {
        (true, None) => return,
        (true, Some(c)) => c * STEP,
        (false, _) => 0,
    };
    let climb = if next[1] > from[1] { STEP / 2 } else { 0 };
    let g2 = g + step + extra + climb;
    if cost.get(&next).is_none_or(|&c| g2 < c) {
        cost.insert(next, g2);
        came_from.insert(next, from);
        open.push(Reverse((g2 + estimate(next, goal), g2, next)));
    }
}

/// The y a mob would stand at in column (x, z), searching `range` blocks up and down from `y`.
pub fn ground_near(block: &impl Fn(i32, i32, i32) -> Option<u16>, x: i32, y: i32, z: i32, range: i32) -> Option<i32> {
    (0..=range).flat_map(|d| [y - d, y + d]).find(|&y| standable(block, [x, y, z]) == Some(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::blocks::{AIR, STONE, WATER};

    /// Stone floor below y=0 in -20..20, with `walls` (x, z) two blocks high.
    fn world(walls: &[(i32, i32)]) -> impl Fn(i32, i32, i32) -> Option<u16> + '_ {
        move |x, y, z| {
            if x.abs() > 20 || z.abs() > 20 {
                return None;
            }
            Some(if y < 0 || (y < 2 && walls.contains(&(x, z))) { STONE } else { AIR })
        }
    }

    #[test]
    fn goes_around_a_wall() {
        // A wall along x=2 from z=-3 to z=3.
        let walls: Vec<(i32, i32)> = (-3..=3).map(|z| (2, z)).collect();
        let block = world(&walls);
        let path = find(&block, WALK, [0, 0, 0], [5, 0, 0], 500).unwrap();
        assert_eq!(*path.last().unwrap(), [5, 0, 0]);
        assert!(path.iter().all(|p| !walls.contains(&(p[0], p[2]))));
        assert!(path.iter().any(|p| p[2].abs() >= 4), "went around the end of the wall");
    }

    #[test]
    fn climbs_one_block_and_drops_down() {
        let block = |x: i32, y: i32, z: i32| {
            if x.abs() > 20 || z.abs() > 20 {
                return None;
            }
            let top = match x {
                2..=4 => 1,  // a one-block step
                5..=7 => -2, // then a two-block drop
                8.. => -1,   // and one step back up
                _ => 0,
            };
            Some(if y < top { STONE } else { AIR })
        };
        let path = find(&block, WALK, [0, 0, 0], [9, -1, 0], 500).unwrap();
        assert!(path.contains(&[2, 1, 0]));
        assert!(path.contains(&[5, -2, 0]));
        assert_eq!(*path.last().unwrap(), [9, -1, 0]);
    }

    #[test]
    fn careful_mobs_stay_dry_and_others_swim() {
        // A water channel along x=2.
        let block = |x: i32, y: i32, z: i32| {
            if x.abs() > 20 || z.abs() > 5 {
                return None;
            }
            Some(match (x, y) {
                (2, -1 | 0) => WATER,
                (_, y) if y < 0 => STONE,
                _ => AIR,
            })
        };
        let path = find(&block, CAREFUL, [0, 0, 0], [4, 0, 0], 500).unwrap();
        assert!(path.iter().all(|p| p[0] < 2), "careful mobs stop at the water: {path:?}");
        let path = find(&block, WALK, [0, 0, 0], [4, 0, 0], 500).unwrap();
        assert_eq!(*path.last().unwrap(), [4, 0, 0]);
    }

    #[test]
    fn unreachable_goals_get_a_partial_path() {
        // Walled in on three sides: the goal is behind the wall.
        let walls: Vec<(i32, i32)> = (-20..=20).map(|z| (3, z)).collect();
        let block = world(&walls);
        let path = find(&block, WALK, [0, 0, 0], [6, 0, 0], 300).unwrap();
        assert_eq!(*path.last().unwrap(), [2, 0, 0], "gets as close as it can");
    }

    #[test]
    fn finds_the_ground() {
        let block = world(&[]);
        assert_eq!(ground_near(&block, 3, 4, 3, 7), Some(0));
        assert_eq!(ground_near(&block, 3, 12, 3, 7), None);
    }
}
