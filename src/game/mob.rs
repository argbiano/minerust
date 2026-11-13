// Mob kinds, their stats, and the block-level movement every mob shares.

use super::path;
use crate::world::{Dimension, blocks};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MobKind {
    Zombie,
    Skeleton,
    Creeper,
    Spider,
    ZombifiedPiglin,
    Pig,
    Cow,
    Sheep,
    Chicken,
    Cod,
    Salmon,
    TropicalFish,
    Squid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Temper {
    /// Attacks players on sight.
    Hostile,
    /// Attacks only players who hit it (or its kin).
    Neutral,
    /// Never attacks; runs when hurt.
    Passive,
}

impl MobKind {
    pub const ALL: [MobKind; 13] = [
        Self::Zombie,
        Self::Skeleton,
        Self::Creeper,
        Self::Spider,
        Self::ZombifiedPiglin,
        Self::Pig,
        Self::Cow,
        Self::Sheep,
        Self::Chicken,
        Self::Cod,
        Self::Salmon,
        Self::TropicalFish,
        Self::Squid,
    ];

    /// Entity type name without the `minecraft:` namespace.
    pub fn name(self) -> &'static str {
        match self {
            Self::Zombie => "zombie",
            Self::Skeleton => "skeleton",
            Self::Creeper => "creeper",
            Self::Spider => "spider",
            Self::ZombifiedPiglin => "zombified_piglin",
            Self::Pig => "pig",
            Self::Cow => "cow",
            Self::Sheep => "sheep",
            Self::Chicken => "chicken",
            Self::Cod => "cod",
            Self::Salmon => "salmon",
            Self::TropicalFish => "tropical_fish",
            Self::Squid => "squid",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        let s = s.trim_start_matches("minecraft:");
        Self::ALL.into_iter().find(|k| k.name() == s)
    }

    pub fn display(self) -> &'static str {
        match self {
            Self::Zombie => "Zombie",
            Self::Skeleton => "Skeleton",
            Self::Creeper => "Creeper",
            Self::Spider => "Spider",
            Self::ZombifiedPiglin => "Zombified Piglin",
            Self::Pig => "Pig",
            Self::Cow => "Cow",
            Self::Sheep => "Sheep",
            Self::Chicken => "Chicken",
            Self::Cod => "Cod",
            Self::Salmon => "Salmon",
            Self::TropicalFish => "Tropical Fish",
            Self::Squid => "Squid",
        }
    }

    /// Id in the 26.3 `minecraft:entity_type` registry.
    pub fn type_id(self) -> i32 {
        match self {
            Self::Zombie => 154,
            Self::Skeleton => 118,
            Self::Creeper => 32,
            Self::Spider => 127,
            Self::ZombifiedPiglin => 158,
            Self::Pig => 101,
            Self::Cow => 30,
            Self::Sheep => 114,
            Self::Chicken => 26,
            Self::Cod => 27,
            Self::Salmon => 113,
            Self::TropicalFish => 140,
            Self::Squid => 130,
        }
    }

    pub fn max_health(self) -> f32 {
        match self {
            Self::Spider => 16.0,
            Self::Pig | Self::Cow | Self::Squid => 10.0,
            Self::Sheep => 8.0,
            Self::Chicken => 4.0,
            Self::Cod | Self::Salmon | Self::TropicalFish => 3.0,
            _ => 20.0,
        }
    }

    /// Walking (or swimming) speed in blocks per tick.
    pub fn speed(self) -> f64 {
        match self {
            Self::Spider => 0.13,
            Self::Skeleton | Self::Creeper | Self::Zombie | Self::ZombifiedPiglin => 0.1,
            Self::Pig => 0.09,
            Self::Cod | Self::Salmon | Self::TropicalFish => 0.08,
            Self::Squid => 0.05,
            _ => 0.07,
        }
    }

    pub fn temper(self) -> Temper {
        match self {
            Self::Zombie | Self::Skeleton | Self::Creeper | Self::Spider => Temper::Hostile,
            Self::ZombifiedPiglin => Temper::Neutral,
            _ => Temper::Passive,
        }
    }

    /// Lives in water: swims instead of walking and suffocates on land.
    pub fn is_aquatic(self) -> bool {
        matches!(self, Self::Cod | Self::Salmon | Self::TropicalFish | Self::Squid)
    }

    /// Players farther than this can't keep it from despawning (vanilla's mob category
    /// despawn distance: 64 for fish, 128 for everything else).
    pub fn despawn_distance(self) -> f64 {
        match self {
            Self::Cod | Self::Salmon | Self::TropicalFish => 64.0,
            _ => 128.0,
        }
    }

    pub fn attack_damage(self) -> f32 {
        match self {
            Self::Zombie | Self::Skeleton => 3.0,
            Self::Spider => 2.0,
            Self::ZombifiedPiglin => 5.0,
            _ => 0.0,
        }
    }

    pub fn burns_in_daylight(self) -> bool {
        matches!(self, Self::Zombie | Self::Skeleton)
    }

    /// Hitbox height, from vanilla's entity dimensions.
    pub fn height(self) -> f64 {
        match self {
            Self::Zombie | Self::ZombifiedPiglin => 1.95,
            Self::Skeleton => 1.99,
            Self::Creeper => 1.7,
            Self::Spider | Self::Pig => 0.9,
            Self::Cow => 1.4,
            Self::Sheep => 1.3,
            Self::Chicken => 0.7,
            Self::Cod => 0.3,
            Self::Salmon | Self::TropicalFish => 0.4,
            Self::Squid => 0.8,
        }
    }

    pub fn eye_height(self) -> f64 {
        self.height() * 0.85
    }
}

/// Breath a water mob has out of water (vanilla fish: 300 ticks).
pub const MAX_AIR: u32 = 300;

pub struct Mob {
    pub id: i32,
    pub uuid: Uuid,
    pub kind: MobKind,
    pub dimension: Dimension,
    pub pos: [f64; 3],
    /// Knockback, jumping and falling; walking is added on top each tick.
    pub vel: [f64; 3],
    /// Body rotation.
    pub yaw: f32,
    pub head_yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub health: f32,
    /// Ticks of damage immunity left after being hurt.
    pub hurt_time: u32,
    pub attack_cooldown: u32,
    /// Ticks since death; the body is removed after the death animation.
    pub death_time: Option<u32>,
    pub target: Option<Uuid>,
    /// Ticks a neutral mob stays angry at `target`.
    pub anger: u32,
    /// Ticks a passive mob keeps fleeing from `panic_from`.
    pub panic: u32,
    pub panic_from: [f64; 3],
    /// Blocks still to walk through, nearest first.
    pub path: Vec<path::Node>,
    /// Where `path` leads, so it isn't searched again while that holds.
    pub path_goal: Option<path::Node>,
    /// Ticks before the path may be searched again.
    pub repath: u32,
    /// Ticks spent on the path without getting nearer to its next block.
    pub stuck: u32,
    pub stuck_best: f64,
    pub look: Option<(Look, u32)>,
    pub fuse: u32,
    pub fire: u32,
    /// Ticks of breath left out of water, for water mobs.
    pub air: u32,
    /// Where a water mob is swimming to.
    pub swim_goal: Option<[f64; 3]>,
    pub age: u32,
    /// Summoned mobs never despawn on their own.
    pub persistent: bool,
    /// What viewers were last told.
    pub sent_pos: [f64; 3],
    pub sent_yaw: f32,
    pub sent_head_yaw: f32,
    pub sent_pitch: f32,
    pub sent_on_fire: bool,
}

/// What a mob's head turns to when it isn't busy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Look {
    Player(Uuid),
    Direction(f32),
}

impl Mob {
    pub fn new(id: i32, kind: MobKind, dimension: Dimension, pos: [f64; 3], yaw: f32) -> Self {
        Self {
            id,
            uuid: Uuid::new_v4(),
            kind,
            dimension,
            pos,
            vel: [0.0; 3],
            yaw,
            head_yaw: yaw,
            pitch: 0.0,
            on_ground: false,
            health: kind.max_health(),
            hurt_time: 0,
            attack_cooldown: 0,
            death_time: None,
            target: None,
            anger: 0,
            panic: 0,
            panic_from: pos,
            path: Vec::new(),
            path_goal: None,
            repath: 0,
            stuck: 0,
            stuck_best: f64::INFINITY,
            look: None,
            fuse: 0,
            fire: 0,
            air: MAX_AIR,
            swim_goal: None,
            age: 0,
            persistent: false,
            sent_pos: pos,
            sent_yaw: yaw,
            sent_head_yaw: yaw,
            sent_pitch: 0.0,
            sent_on_fire: false,
        }
    }

    pub fn alive(&self) -> bool {
        self.death_time.is_none()
    }

    /// The block the mob's feet are in.
    pub fn block_pos(&self) -> path::Node {
        [self.pos[0].floor() as i32, (self.pos[1] + 1e-6).floor() as i32, self.pos[2].floor() as i32]
    }

    pub fn in_water(&self, block: &impl Fn(i32, i32, i32) -> Option<u16>) -> bool {
        let y = (self.pos[1] + self.kind.height() / 2.0).floor() as i32;
        block(self.pos[0].floor() as i32, y, self.pos[2].floor() as i32).is_some_and(blocks::is_swimmable)
    }

    /// One tick of swimming by `dir` (blocks per tick) plus knockback. The mob only moves
    /// where its body stays in water; returns false when the way is blocked. Out of water it
    /// falls and flops about, as stranded fish do.
    pub fn swim(&mut self, dir: [f64; 3], block: &impl Fn(i32, i32, i32) -> Option<u16>, flop: bool) -> bool {
        if !self.in_water(block) {
            if self.on_ground && flop {
                self.vel[1] = 0.4;
            }
            self.step([0.0, 0.0], false, block);
            return false;
        }
        let d = [dir[0] + self.vel[0], dir[1], dir[2] + self.vel[2]];
        self.vel = [self.vel[0] * 0.6, 0.0, self.vel[2] * 0.6];
        let next = [self.pos[0] + d[0], self.pos[1] + d[1], self.pos[2] + d[2]];
        let mid = next[1] + self.kind.height() / 2.0;
        let lead = |v: f64, d: f64| (v + 0.3 * d.signum()).floor() as i32;
        let water = [
            [lead(next[0], d[0]), mid.floor() as i32, lead(next[2], d[2])],
            [next[0].floor() as i32, mid.floor() as i32, next[2].floor() as i32],
        ]
        .iter()
        .all(|&[x, y, z]| block(x, y, z).is_some_and(blocks::is_swimmable));
        self.on_ground = false;
        if water {
            self.pos = next;
            true
        } else {
            false
        }
    }

    pub fn clear_path(&mut self) {
        self.path.clear();
        self.path_goal = None;
        self.stuck = 0;
        self.stuck_best = f64::INFINITY;
    }

    /// The horizontal step towards the next block of the path, dropping blocks already
    /// reached and the path itself when the mob stops making progress on it.
    pub fn follow_path(&mut self, speed: f64) -> [f64; 2] {
        while let Some(&[x, y, z]) = self.path.first() {
            let (dx, dz) = (x as f64 + 0.5 - self.pos[0], z as f64 + 0.5 - self.pos[2]);
            let close = dx * dx + dz * dz < 0.35 * 0.35;
            // A step up counts once the mob is up there; a drop as soon as it is above it.
            if close && (self.pos[1] - y as f64).abs() < 1.2 {
                self.path.remove(0);
                self.stuck = 0;
                self.stuck_best = f64::INFINITY;
                continue;
            }
            let distance = (dx * dx + dz * dz).sqrt();
            if distance < self.stuck_best - 0.05 {
                self.stuck_best = distance;
                self.stuck = 0;
            } else {
                self.stuck += 1;
                if self.stuck > 40 {
                    self.clear_path();
                    return [0.0, 0.0];
                }
            }
            // Slow down for the last block so it doesn't overshoot and turn around.
            let step = speed.min(distance);
            return [dx / distance * step, dz / distance * step];
        }
        self.path_goal = None;
        [0.0, 0.0]
    }

    /// Turns the body towards where it walks (or its head when standing) and the head
    /// towards `look`, a little each tick as vanilla's body and look controls do.
    pub fn turn(&mut self, walk: [f64; 2], look_at: Option<[f64; 3]>) {
        let moving = walk[0].abs() > 1e-3 || walk[1].abs() > 1e-3;
        if moving {
            self.yaw = approach(self.yaw, yaw_towards(walk[0], walk[1]), 30.0);
        }
        let (head, pitch) = match look_at {
            Some(at) => {
                let eye = [self.pos[0], self.pos[1] + self.kind.eye_height(), self.pos[2]];
                let (dx, dy, dz) = (at[0] - eye[0], at[1] - eye[1], at[2] - eye[2]);
                let flat = (dx * dx + dz * dz).sqrt();
                (yaw_towards(dx, dz), (-dy.atan2(flat).to_degrees()) as f32)
            }
            None => (self.yaw, 0.0),
        };
        self.head_yaw = approach(self.head_yaw, head, 40.0);
        // Heads turn at most 75° from the body; past that the body follows.
        let off = wrap(self.head_yaw - self.yaw);
        if off.abs() > 75.0 {
            self.yaw = wrap(self.head_yaw - 75.0 * off.signum());
        }
        self.pitch = approach(self.pitch, pitch.clamp(-40.0, 40.0), 20.0);
    }

    /// One tick of movement: `walk` is the horizontal step the AI wants. Returns false
    /// when the mob is standing against something it can neither walk nor jump over.
    /// `block` answers `None` for unloaded blocks, which count as solid so mobs don't
    /// wander into missing chunks.
    pub fn step(&mut self, walk: [f64; 2], cautious: bool, block: &impl Fn(i32, i32, i32) -> Option<u16>) -> bool {
        let solid = |x: i32, y: i32, z: i32| block(x, y, z).is_none_or(blocks::is_solid);
        let liquid = |x: i32, y: i32, z: i32| block(x, y, z).is_some_and(blocks::is_liquid);

        let dx = walk[0] + self.vel[0];
        let dz = walk[1] + self.vel[2];
        self.vel[0] *= 0.6;
        self.vel[2] *= 0.6;
        let mut blocked = false;
        if dx.abs() > 1e-4 || dz.abs() > 1e-4 {
            // Slide along walls: try the full step, then each axis on its own.
            let mut result = self.try_move(dx, dz, cautious, &solid, &liquid);
            if result == Move::Blocked {
                result = self.try_move(dx, 0.0, cautious, &solid, &liquid);
            }
            if result == Move::Blocked {
                result = self.try_move(0.0, dz, cautious, &solid, &liquid);
            }
            // Mid-jump the mob is still on its way up, not stuck.
            blocked = result == Move::Blocked && self.on_ground;
        }

        let fx = self.pos[0].floor() as i32;
        let fz = self.pos[2].floor() as i32;
        // Any block under the 0.6-wide footprint holds the mob up.
        let (px, pz) = (self.pos[0], self.pos[2]);
        let under = |y: i32| {
            [(-0.3, -0.3), (-0.3, 0.3), (0.3, -0.3), (0.3, 0.3)].iter().any(|(ox, oz)| {
                let (bx, bz) = ((px + ox).floor() as i32, (pz + oz).floor() as i32);
                solid(bx, y, bz) && !liquid(bx, y, bz)
            })
        };
        let in_liquid = liquid(fx, (self.pos[1] + 0.5).floor() as i32, fz);
        if in_liquid {
            // Swim upwards, as vanilla mobs do.
            self.vel[1] = 0.06;
        }
        let ny = self.pos[1] + self.vel[1];
        if self.vel[1] <= 0.0 {
            // The first floor between here and where the fall ends, so fast falls can't skip one.
            let from = (self.pos[1] - 1e-6).floor() as i32;
            let to = (ny - 1e-6).floor() as i32;
            match (to..=from).rev().find(|&y| under(y)) {
                Some(floor) => {
                    self.pos[1] = (floor + 1) as f64;
                    self.vel[1] = 0.0;
                    self.on_ground = true;
                }
                None => {
                    self.pos[1] = ny;
                    self.on_ground = false;
                }
            }
        } else if solid(fx, (ny + self.kind.height()).floor() as i32, fz) {
            self.vel[1] = 0.0;
        } else {
            self.pos[1] = ny;
            self.on_ground = false;
        }
        // Gravity after moving, as vanilla applies it, so a 0.42 jump clears a block.
        if !in_liquid && !self.on_ground {
            self.vel[1] = (self.vel[1] - 0.08) * 0.98;
        }
        !blocked
    }

    fn try_move(
        &mut self,
        dx: f64,
        dz: f64,
        cautious: bool,
        solid: &impl Fn(i32, i32, i32) -> bool,
        liquid: &impl Fn(i32, i32, i32) -> bool,
    ) -> Move {
        if dx == 0.0 && dz == 0.0 {
            return Move::Blocked;
        }
        let (nx, nz) = (self.pos[0] + dx, self.pos[2] + dz);
        // The leading edge of a body about 0.6 blocks wide.
        let bx = (nx + 0.3 * dx.signum()).floor() as i32;
        let bz = (nz + 0.3 * dz.signum()).floor() as i32;
        let fy = (self.pos[1] + 1e-6).floor() as i32;
        let (cx, cz) = (self.pos[0].floor() as i32, self.pos[2].floor() as i32);

        if solid(bx, fy, bz) {
            // A one-block step: jump it, with the vanilla jump speed, and walk on once high enough.
            if self.on_ground && !solid(bx, fy + 1, bz) && !solid(bx, fy + 2, bz) && !solid(cx, fy + 2, cz) {
                self.vel[1] = 0.42;
                self.on_ground = false;
                return Move::Jumping;
            }
            return Move::Blocked;
        }
        if solid(bx, fy + 1, bz) {
            return Move::Blocked;
        }
        if cautious {
            let drop = (1..=4).all(|d| !solid(bx, fy - d, bz));
            let wet = liquid(bx, fy, bz) || liquid(bx, fy - 1, bz);
            if drop || wet {
                return Move::Blocked;
            }
        }
        self.pos = [nx, self.pos[1], nz];
        Move::Moved
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Move {
    Moved,
    Jumping,
    Blocked,
}

/// `from` turned towards `to` by at most `max` degrees, the short way round.
pub fn approach(from: f32, to: f32, max: f32) -> f32 {
    wrap(from + wrap(to - from).clamp(-max, max))
}

/// Degrees in -180..180.
pub fn wrap(degrees: f32) -> f32 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

/// Minecraft yaw (0 = +z, 90 = -x) of a horizontal direction.
pub fn yaw_towards(dx: f64, dz: f64) -> f32 {
    (dz.atan2(dx).to_degrees() - 90.0) as f32
}

/// Small, fast, seedable; vanilla's spawn rolls don't need anything stronger.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// In `[0, 1)`.
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// In `lo..hi`.
    pub fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next_u64() % (hi - lo).max(1) as u64) as i32
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::blocks::{AIR, STONE, WATER};

    // Stone below y=64, a one-block step at x>=3, a two-block wall at x>=6.
    fn terrain(x: i32, y: i32, _z: i32) -> Option<u16> {
        let top = if x >= 6 {
            67
        } else if x >= 3 {
            65
        } else {
            64
        };
        Some(if y < top { STONE } else { AIR })
    }

    #[test]
    fn walks_falls_steps_and_stops_at_walls() {
        let mut mob = Mob::new(1, MobKind::Zombie, Dimension::Overworld, [0.5, 66.0, 0.5], 0.0);
        for _ in 0..20 {
            mob.step([0.0, 0.0], true, &terrain);
        }
        assert_eq!(mob.pos[1], 64.0, "fell onto the ground");
        assert!(mob.on_ground);

        let mut peak: f64 = 0.0;
        for _ in 0..40 {
            mob.step([0.1, 0.0], true, &terrain);
            peak = peak.max(mob.pos[1]);
        }
        assert_eq!(mob.pos[1], 65.0, "stepped up one block");
        assert!(peak > 65.0 && peak < 65.5, "by jumping, not teleporting: peak {peak}");
        for _ in 0..40 {
            mob.step([0.1, 0.0], true, &terrain);
        }
        assert!(mob.pos[0] < 6.0, "a two-block wall stops it, at {:?}", mob.pos);
        assert!(!mob.step([0.1, 0.0], true, &terrain), "and reports being blocked");
    }

    #[test]
    fn turns_gradually_and_heads_lead_the_body() {
        let mut mob = Mob::new(1, MobKind::Cow, Dimension::Overworld, [0.5, 64.0, 0.5], 0.0);
        mob.turn([-0.1, 0.0], None); // walk west: yaw 90
        assert_eq!(mob.yaw, 30.0);
        for _ in 0..5 {
            mob.turn([-0.1, 0.0], None);
        }
        assert_eq!(mob.yaw, 90.0);
        // Look back east while standing still: the head turns, the body follows past 75°.
        for _ in 0..10 {
            mob.turn([0.0, 0.0], Some([10.0, 65.3, 0.5]));
        }
        assert!((wrap(mob.head_yaw) - -90.0).abs() < 0.01, "head {}", mob.head_yaw);
        assert!(wrap(mob.head_yaw - mob.yaw).abs() <= 75.01);
        assert_eq!(approach(170.0, -170.0, 30.0), -170.0, "turns the short way round");
    }

    #[test]
    fn cautious_mobs_avoid_water_and_cliffs() {
        let pond = |x: i32, y: i32, _z: i32| {
            Some(if y < 63 || (y == 63 && x < 3) {
                STONE
            } else if y == 63 {
                WATER
            } else {
                AIR
            })
        };
        let mut mob = Mob::new(1, MobKind::Pig, Dimension::Overworld, [0.5, 64.0, 0.5], 0.0);
        mob.on_ground = true;
        for _ in 0..60 {
            mob.step([0.1, 0.0], true, &pond);
        }
        assert!(mob.pos[0] < 3.0, "walked into water at {:?}", mob.pos);

        let cliff = |x: i32, y: i32, _z: i32| Some(if y < if x < 3 { 64 } else { 50 } { STONE } else { AIR });
        let mut mob = Mob::new(2, MobKind::Pig, Dimension::Overworld, [0.5, 64.0, 0.5], 0.0);
        mob.on_ground = true;
        for _ in 0..60 {
            mob.step([0.1, 0.0], true, &cliff);
        }
        assert!(mob.pos[0] < 3.0 && mob.pos[1] == 64.0, "walked off the cliff to {:?}", mob.pos);
    }

    #[test]
    fn yaw_matches_vanilla_convention() {
        assert_eq!(yaw_towards(0.0, 1.0).rem_euclid(360.0).round(), 0.0); // south
        assert_eq!(yaw_towards(-1.0, 0.0).rem_euclid(360.0).round(), 90.0); // west
        assert_eq!(yaw_towards(0.0, -1.0).rem_euclid(360.0).round(), 180.0); // north
    }

    #[test]
    fn fish_stay_in_water_and_flop_on_land() {
        // Water below y=64 for x < 5, stone floor below y=60.
        let pond = |x: i32, y: i32, _z: i32| {
            Some(if y < 60 {
                STONE
            } else if y < 64 && x < 5 {
                WATER
            } else if y < 61 {
                STONE
            } else {
                AIR
            })
        };
        let mut fish = Mob::new(1, MobKind::Cod, Dimension::Overworld, [0.5, 62.0, 0.5], 0.0);
        for _ in 0..100 {
            fish.swim([0.08, 0.0, 0.0], &pond, true);
        }
        assert!(fish.pos[0] < 5.0, "swam out of the water to {:?}", fish.pos);
        assert_eq!(fish.pos[1], 62.0, "no gravity while swimming");
        assert!(!fish.swim([0.0, 0.1, 0.0], &|_, _, _| Some(WATER), true) || fish.pos[1] > 62.0);

        let mut stranded = Mob::new(2, MobKind::Salmon, Dimension::Overworld, [7.5, 63.0, 0.5], 0.0);
        let mut peak: f64 = 0.0;
        for _ in 0..40 {
            stranded.swim([0.0; 3], &pond, true);
            peak = peak.max(stranded.pos[1]);
        }
        for _ in 0..20 {
            stranded.swim([0.0; 3], &pond, false);
        }
        assert_eq!(stranded.pos[1], 61.0, "fell to the ground");
        assert!(peak > 61.2, "and flops");
    }

    #[test]
    fn kinds_round_trip() {
        for kind in MobKind::ALL {
            assert_eq!(MobKind::parse(&format!("minecraft:{}", kind.name())), Some(kind));
        }
    }
}
