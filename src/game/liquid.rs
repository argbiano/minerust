// Flowing water and lava, following vanilla's FlowingFluid: liquids spread a level weaker
// per block (lava two in the overworld), fall straight down, head for the nearest drop,
// dry up when what fed them is gone, and two water sources make a third. Lava touching
// water turns into obsidian (source) or cobblestone, and lava falling onto water makes stone.
//
// Block states: water is 89 + level and lava 105 + level. Level 0 is a source, 1..=7 is
// flowing with 8 - level eighths of liquid, and 8.. is falling (a full column).

use crate::world::blocks;

pub type Pos = [i32; 3];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fluid {
    Water,
    Lava,
}

/// Where the liquid lives, which changes how lava behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    pub ultrawarm: bool,
}

impl Fluid {
    fn base(self) -> u16 {
        match self {
            Self::Water => blocks::WATER,
            Self::Lava => blocks::LAVA,
        }
    }

    pub fn state(self, level: u8) -> u16 {
        self.base() + level.min(15) as u16
    }

    /// How much weaker the liquid gets per block it spreads.
    fn drop_off(self, rules: Rules) -> u8 {
        match self {
            Self::Lava if !rules.ultrawarm => 2,
            _ => 1,
        }
    }

    /// How far sideways the liquid looks for a drop to flow towards.
    fn slope_distance(self, rules: Rules) -> u32 {
        match self {
            Self::Lava if !rules.ultrawarm => 2,
            _ => 4,
        }
    }

    /// Ticks between a change and the liquid reacting to it.
    pub fn delay(self, rules: Rules) -> u64 {
        match self {
            Self::Water => 5,
            Self::Lava if rules.ultrawarm => 10,
            Self::Lava => 30,
        }
    }
}

/// The liquid in a block state and its level. Seagrass, kelp and waterlogged blocks hold
/// a water source: it spreads and feeds neighbours, but the block itself never changes.
pub fn fluid(state: u16) -> Option<(Fluid, u8)> {
    match state {
        89..=104 => Some((Fluid::Water, (state - 89) as u8)),
        105..=120 => Some((Fluid::Lava, (state - 105) as u8)),
        _ if blocks::is_waterlogged(state) => Some((Fluid::Water, 0)),
        _ => None,
    }
}

/// Eighths of liquid: sources and falling columns are full.
fn amount(level: u8) -> u8 {
    if level == 0 || level >= 8 { 8 } else { 8 - level }
}

fn flowing(amount: u8) -> u8 {
    8 - amount
}

const FALLING: u8 = 8;

pub trait World {
    /// `None` when the block isn't loaded; such blocks are left alone.
    fn get(&self, pos: Pos) -> Option<u16>;
    fn set(&mut self, pos: Pos, state: u16);
}

const SIDES: [Pos; 4] = [[0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];

fn add(a: Pos, b: Pos) -> Pos {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn below(p: Pos) -> Pos {
    [p[0], p[1] - 1, p[2]]
}

fn above(p: Pos) -> Pos {
    [p[0], p[1] + 1, p[2]]
}

fn is_source_of(w: &impl World, pos: Pos, f: Fluid) -> bool {
    w.get(pos).and_then(fluid) == Some((f, 0))
}

fn is_fluid(w: &impl World, pos: Pos, f: Fluid) -> bool {
    w.get(pos).and_then(fluid).is_some_and(|(g, _)| g == f)
}

/// Empty enough for the liquid to move in, or already the same (non-source) liquid.
fn can_pass(w: &impl World, pos: Pos, f: Fluid) -> bool {
    match w.get(pos) {
        None => false,
        Some(state) => match fluid(state) {
            Some((g, level)) => g == f && level != 0,
            None => blocks::is_fluid_replaceable(state),
        },
    }
}

/// A drop the liquid would fall into.
fn is_hole(w: &impl World, pos: Pos, f: Fluid) -> bool {
    let down = below(pos);
    is_fluid(w, down, f) || w.get(down).is_some_and(blocks::is_fluid_replaceable)
}

/// The level the liquid at `pos` should have given its neighbours, `None` for none.
fn new_level(w: &impl World, pos: Pos, f: Fluid, rules: Rules) -> Option<u8> {
    let mut max = 0;
    let mut sources = 0;
    for side in SIDES {
        if let Some((g, level)) = w.get(add(pos, side)).and_then(fluid)
            && g == f
        {
            if level == 0 {
                sources += 1;
            }
            max = max.max(amount(level));
        }
    }
    if f == Fluid::Water && sources >= 2 {
        let floor = w.get(below(pos));
        let solid = floor.is_some_and(|s| blocks::is_solid(s) && fluid(s).is_none());
        if solid || is_source_of(w, below(pos), f) {
            return Some(0);
        }
    }
    if is_fluid(w, above(pos), f) {
        return Some(FALLING);
    }
    let left = max.saturating_sub(f.drop_off(rules));
    (left > 0).then(|| flowing(left))
}

/// Lava next to water (any side but below) hardens.
fn harden(w: &impl World, pos: Pos, level: u8) -> Option<u16> {
    let touching = [above(pos)].into_iter().chain(SIDES.map(|s| add(pos, s))).any(|p| is_fluid(w, p, Fluid::Water));
    touching.then_some(if level == 0 { blocks::OBSIDIAN } else { blocks::COBBLESTONE })
}

/// One scheduled update of the liquid at `pos`. Changed blocks are written through `w`;
/// the caller schedules updates for them and their neighbours.
pub fn tick(w: &mut impl World, pos: Pos, rules: Rules) {
    let Some((f, mut level)) = w.get(pos).and_then(fluid) else { return };
    if f == Fluid::Lava
        && let Some(stone) = harden(w, pos, level)
    {
        w.set(pos, stone);
        return;
    }
    if level != 0 {
        match new_level(w, pos, f, rules) {
            None => {
                w.set(pos, blocks::AIR);
                return;
            }
            Some(new) if new != level => {
                w.set(pos, f.state(new));
                level = new;
            }
            Some(_) => {}
        }
    }
    spread(w, pos, f, level, rules);
}

fn spread(w: &mut impl World, pos: Pos, f: Fluid, level: u8, rules: Rules) {
    let down = below(pos);
    match w.get(down) {
        // Lava pouring onto water; water held by seagrass or a waterlogged block just stops it.
        Some(state) if f == Fluid::Lava && fluid(state).is_some_and(|(g, _)| g == Fluid::Water) => {
            if !blocks::is_waterlogged(state) {
                w.set(down, blocks::STONE);
            }
            return;
        }
        Some(state) if !is_source_of(w, down, f) && can_pass(w, down, f) => {
            if fluid(state).is_none() {
                w.set(down, f.state(FALLING));
            }
            // Vanilla: a source mostly surrounded by sources still spreads sideways.
            if level == 0 && SIDES.iter().filter(|&&s| is_source_of(w, add(pos, s), f)).count() >= 3 {
                spread_sideways(w, pos, f, level, rules);
            }
            return;
        }
        _ => {}
    }
    if level == 0 || !is_hole(w, pos, f) {
        spread_sideways(w, pos, f, level, rules);
    }
}

fn spread_sideways(w: &mut impl World, pos: Pos, f: Fluid, level: u8, rules: Rules) {
    let strength = if level >= FALLING { 7 } else { amount(level).saturating_sub(f.drop_off(rules)) };
    if strength == 0 {
        return;
    }
    let mut best = u32::MAX;
    let mut targets = Vec::new();
    for (i, side) in SIDES.iter().enumerate() {
        let target = add(pos, *side);
        if !can_pass(w, target, f) {
            continue;
        }
        let distance = if is_hole(w, target, f) { 0 } else { slope(w, target, 1, i, f, rules) };
        if distance < best {
            targets.clear();
            best = distance;
        }
        if distance == best {
            targets.push(target);
        }
    }
    for target in targets {
        // Liquid already there updates itself from its neighbours.
        if w.get(target).is_some_and(|s| fluid(s).is_none())
            && let Some(new) = new_level(w, target, f, rules)
        {
            w.set(target, f.state(new));
        }
    }
}

/// Blocks from `pos` to the nearest drop within the liquid's reach (u32::MAX if none),
/// never stepping back the way it came.
fn slope(w: &impl World, pos: Pos, depth: u32, came_from: usize, f: Fluid, rules: Rules) -> u32 {
    let back = came_from ^ 1; // SIDES pairs opposite directions
    let mut best = u32::MAX;
    for (i, side) in SIDES.iter().enumerate() {
        if i == back {
            continue;
        }
        let next = add(pos, *side);
        if !can_pass(w, next, f) {
            continue;
        }
        if is_hole(w, next, f) {
            return depth;
        }
        if depth < f.slope_distance(rules) {
            best = best.min(slope(w, next, depth + 1, i, f, rules));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::blocks::{AIR, COBBLESTONE, GLASS, OBSIDIAN, STONE};

    const SEAGRASS: u16 = 2370;
    use std::collections::{BTreeMap, HashMap, HashSet};

    const OVERWORLD: Rules = Rules { ultrawarm: false };

    /// Stone below y=0, air above, inside x/z -16..16; outside is unloaded.
    #[derive(Default)]
    struct Test {
        blocks: HashMap<Pos, u16>,
        changed: Vec<Pos>,
    }

    impl World for Test {
        fn get(&self, p: Pos) -> Option<u16> {
            if p[0].abs() > 16 || p[2].abs() > 16 {
                return None;
            }
            Some(self.blocks.get(&p).copied().unwrap_or(if p[1] < 0 { STONE } else { AIR }))
        }
        fn set(&mut self, p: Pos, state: u16) {
            self.blocks.insert(p, state);
            self.changed.push(p);
        }
    }

    impl Test {
        fn place(&mut self, p: Pos, state: u16) {
            self.set(p, state);
        }

        /// Runs scheduled updates the way the game loop does until nothing changes.
        fn settle(&mut self) -> u64 {
            let mut queue: BTreeMap<u64, Vec<Pos>> = BTreeMap::new();
            let mut pending = HashSet::new();
            let mut now = 0;
            for _ in 0..100_000 {
                for p in std::mem::take(&mut self.changed) {
                    for q in [p, above(p), below(p)].into_iter().chain(SIDES.map(|s| add(p, s))) {
                        if let Some((f, _)) = self.get(q).and_then(fluid)
                            && pending.insert(q)
                        {
                            queue.entry(now + f.delay(OVERWORLD)).or_default().push(q);
                        }
                    }
                }
                let Some((&at, _)) = queue.iter().next() else { return now };
                now = at;
                for p in queue.remove(&at).unwrap() {
                    pending.remove(&p);
                    tick(self, p, OVERWORLD);
                }
            }
            panic!("liquid never settled");
        }

        fn at(&self, p: Pos) -> Option<(Fluid, u8)> {
            self.get(p).and_then(fluid)
        }
    }

    #[test]
    fn water_spreads_seven_blocks_on_flat_ground() {
        let mut w = Test::default();
        w.place([0, 0, 0], Fluid::Water.state(0));
        w.settle();
        for d in 1..=7 {
            assert_eq!(w.at([d, 0, 0]), Some((Fluid::Water, d as u8)), "{d} blocks out");
        }
        assert_eq!(w.at([8, 0, 0]), None);
        assert_eq!(w.at([3, 0, 4]), Some((Fluid::Water, 7)), "diamond shape");
        assert_eq!(w.at([4, 0, 4]), None);
    }

    #[test]
    fn lava_spreads_three_blocks_in_the_overworld() {
        let mut w = Test::default();
        w.place([0, 0, 0], Fluid::Lava.state(0));
        w.settle();
        assert_eq!(w.at([3, 0, 0]), Some((Fluid::Lava, 6)));
        assert_eq!(w.at([4, 0, 0]), None);
    }

    #[test]
    fn water_falls_then_spreads_and_drains_when_the_source_goes() {
        let mut w = Test::default();
        w.place([0, 5, 0], GLASS);
        w.place([0, 6, 0], Fluid::Water.state(0));
        w.settle();
        assert_eq!(w.at([1, 6, 0]), Some((Fluid::Water, 1)), "spreads on the glass");
        assert_eq!(w.at([1, 5, 0]), Some((Fluid::Water, FALLING)), "falls off its edge");
        assert_eq!(w.at([1, 0, 0]), Some((Fluid::Water, FALLING)));
        assert_eq!(w.at([2, 0, 0]), Some((Fluid::Water, 1)), "spreads again on the ground");

        w.place([0, 6, 0], AIR);
        w.settle();
        assert!(w.blocks.values().all(|&s| fluid(s).is_none()), "all water drained");
    }

    #[test]
    fn flows_towards_the_nearest_drop() {
        let mut w = Test::default();
        w.place([3, -1, 0], AIR); // a hole three blocks east
        w.place([0, 0, 0], Fluid::Water.state(0));
        w.settle();
        assert_eq!(w.at([-1, 0, 0]), None, "doesn't spread away from the hole");
        assert_eq!(w.at([0, 0, 1]), None);
        assert_eq!(w.at([3, 0, 0]), Some((Fluid::Water, 3)));
        assert_eq!(w.at([3, -1, 0]), Some((Fluid::Water, FALLING)));
    }

    #[test]
    fn two_water_sources_make_a_third() {
        let mut w = Test::default();
        for x in -1..=1 {
            for z in -1..=1 {
                w.place([x, 0, z], GLASS);
            }
        }
        // A 3×1 trench with sources at both ends.
        for x in -1..=1 {
            w.place([x, 0, 0], AIR);
        }
        w.place([-1, 0, 0], Fluid::Water.state(0));
        w.place([1, 0, 0], Fluid::Water.state(0));
        w.settle();
        assert_eq!(w.at([0, 0, 0]), Some((Fluid::Water, 0)));
    }

    #[test]
    fn lava_meets_water() {
        let mut w = Test::default();
        w.place([0, 0, 0], Fluid::Lava.state(0));
        w.place([1, 0, 0], Fluid::Water.state(0));
        w.settle();
        assert_eq!(w.get([0, 0, 0]), Some(OBSIDIAN), "lava source next to water");

        let mut w = Test::default();
        w.place([0, 0, 0], Fluid::Lava.state(0));
        w.place([10, 0, 0], Fluid::Water.state(0)); // water stops at x=3, lava reaches x=2
        w.settle();
        assert!(w.blocks.values().any(|&s| s == COBBLESTONE), "flowing lava touching water");
        assert_eq!(w.at([0, 0, 0]), Some((Fluid::Lava, 0)), "the source survives");

        let mut w = Test::default();
        w.place([0, -1, 0], Fluid::Water.state(0));
        w.place([0, 0, 0], GLASS);
        w.place([1, 3, 0], Fluid::Lava.state(0));
        w.place([1, 2, 0], GLASS);
        w.place([0, 3, 0], AIR);
        // Lava runs off the glass and falls onto the water.
        w.place([0, 0, 0], AIR);
        w.settle();
        assert_eq!(w.get([0, -1, 0]), Some(STONE));
    }

    #[test]
    fn unloaded_blocks_stop_the_flow() {
        let mut w = Test::default();
        w.place([15, 0, 0], Fluid::Water.state(0));
        w.settle();
        assert_eq!(w.at([16, 0, 0]), Some((Fluid::Water, 1)));
        assert!(w.blocks.keys().all(|p| p[0] <= 16), "nothing written outside");
    }

    #[test]
    fn states_round_trip() {
        assert_eq!(fluid(blocks::WATER), Some((Fluid::Water, 0)));
        assert_eq!(fluid(120), Some((Fluid::Lava, 15)));
        assert_eq!(fluid(STONE), None);
        assert_eq!(Fluid::Lava.state(3), 108);
        assert_eq!(fluid(SEAGRASS), Some((Fluid::Water, 0)), "seagrass holds water");
    }

    #[test]
    fn waterlogged_blocks_feed_water_but_stay_put() {
        let mut w = Test::default();
        w.place([0, 0, 0], SEAGRASS);
        // Something next to it changed, as when a player breaks the bank of a river.
        w.place([1, 0, 0], AIR);
        w.settle();
        assert_eq!(w.get([0, 0, 0]), Some(SEAGRASS));
        assert_eq!(w.at([1, 0, 0]), Some((Fluid::Water, 1)), "water flows out of the seagrass");

        // Lava falling onto seagrass doesn't turn it into stone.
        let mut w = Test::default();
        w.place([0, 0, 0], SEAGRASS);
        w.place([0, 2, 0], GLASS);
        w.place([0, 1, 0], Fluid::Lava.state(0));
        w.place([0, 2, 0], AIR);
        w.settle();
        assert_eq!(w.get([0, 0, 0]), Some(SEAGRASS));
    }
}
