// Mob kinds, their stats, and the block-level movement every mob shares.

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
    pub const ALL: [MobKind; 9] = [
        Self::Zombie,
        Self::Skeleton,
        Self::Creeper,
        Self::Spider,
        Self::ZombifiedPiglin,
        Self::Pig,
        Self::Cow,
        Self::Sheep,
        Self::Chicken,
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
        }
    }

    pub fn max_health(self) -> f32 {
        match self {
            Self::Spider => 16.0,
            Self::Pig | Self::Cow => 10.0,
            Self::Sheep => 8.0,
            Self::Chicken => 4.0,
            _ => 20.0,
        }
    }

    /// Walking speed in blocks per tick.
    pub fn speed(self) -> f64 {
        match self {
            Self::Spider => 0.13,
            Self::Skeleton | Self::Creeper | Self::Zombie | Self::ZombifiedPiglin => 0.1,
            Self::Pig => 0.09,
            _ => 0.07,
        }
    }

    pub fn temper(self) -> Temper {
        match self {
            Self::Zombie | Self::Skeleton | Self::Creeper | Self::Spider => Temper::Hostile,
            Self::ZombifiedPiglin => Temper::Neutral,
            Self::Pig | Self::Cow | Self::Sheep | Self::Chicken => Temper::Passive,
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
}

pub struct Mob {
    pub id: i32,
    pub uuid: Uuid,
    pub kind: MobKind,
    pub dimension: Dimension,
    pub pos: [f64; 3],
    /// Knockback and falling; walking is added on top each tick.
    pub vel: [f64; 3],
    pub yaw: f32,
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
    pub wander: Option<[f64; 2]>,
    pub wander_ticks: u32,
    pub fuse: u32,
    pub fire: u32,
    pub age: u32,
    /// Summoned mobs never despawn on their own.
    pub persistent: bool,
    /// What viewers were last told.
    pub sent_pos: [f64; 3],
    pub sent_yaw: f32,
    pub sent_on_fire: bool,
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
            on_ground: false,
            health: kind.max_health(),
            hurt_time: 0,
            attack_cooldown: 0,
            death_time: None,
            target: None,
            anger: 0,
            panic: 0,
            panic_from: pos,
            wander: None,
            wander_ticks: 0,
            fuse: 0,
            fire: 0,
            age: 0,
            persistent: false,
            sent_pos: pos,
            sent_yaw: yaw,
            sent_on_fire: false,
        }
    }

    pub fn alive(&self) -> bool {
        self.death_time.is_none()
    }

    pub fn face(&mut self, dx: f64, dz: f64) {
        if dx.abs() > 1e-6 || dz.abs() > 1e-6 {
            self.yaw = yaw_towards(dx, dz);
        }
    }

    /// One tick of movement: `walk` is the horizontal step the AI wants. Returns
    /// false when the step was blocked. `block` answers `None` for unloaded blocks,
    /// which count as solid so mobs don't wander into missing chunks.
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
            let moved = self.try_move(dx, dz, cautious, &solid, &liquid)
                || self.try_move(dx, 0.0, cautious, &solid, &liquid)
                || self.try_move(0.0, dz, cautious, &solid, &liquid);
            blocked = !moved;
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
        if liquid(fx, (self.pos[1] + 0.5).floor() as i32, fz) {
            // Swim upwards, as vanilla mobs do.
            self.vel[1] = 0.06;
        } else {
            self.vel[1] = (self.vel[1] - 0.08) * 0.98;
        }
        let ny = self.pos[1] + self.vel[1];
        if self.vel[1] <= 0.0 {
            let below = ny.floor() as i32;
            if under(below) {
                self.pos[1] = (below + 1) as f64;
                self.vel[1] = 0.0;
                self.on_ground = true;
            } else {
                self.pos[1] = ny;
                self.on_ground = false;
            }
        } else if solid(fx, (ny + 1.8).floor() as i32, fz) {
            self.vel[1] = 0.0;
        } else {
            self.pos[1] = ny;
            self.on_ground = false;
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
    ) -> bool {
        if dx == 0.0 && dz == 0.0 {
            return false;
        }
        let (nx, nz) = (self.pos[0] + dx, self.pos[2] + dz);
        // The leading edge of a body about 0.6 blocks wide.
        let bx = (nx + 0.3 * dx.signum()).floor() as i32;
        let bz = (nz + 0.3 * dz.signum()).floor() as i32;
        let fy = (self.pos[1] + 1e-6).floor() as i32;

        let mut y = self.pos[1];
        if solid(bx, fy, bz) {
            if !self.on_ground || solid(bx, fy + 1, bz) || solid(bx, fy + 2, bz) {
                return false;
            }
            y = (fy + 1) as f64; // step (jump) up one block
        } else if solid(bx, fy + 1, bz) {
            return false;
        }
        if cautious {
            let ground = y.floor() as i32;
            let drop = (1..=4).all(|d| !solid(bx, ground - d, bz));
            let wet = liquid(bx, ground, bz) || liquid(bx, ground - 1, bz);
            if drop || wet {
                return false;
            }
        }
        self.pos = [nx, y, nz];
        true
    }
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

        for _ in 0..30 {
            mob.step([0.1, 0.0], true, &terrain);
        }
        assert_eq!(mob.pos[1], 65.0, "stepped up one block");
        for _ in 0..40 {
            mob.step([0.1, 0.0], true, &terrain);
        }
        assert!(mob.pos[0] < 6.0, "a two-block wall stops it, at {:?}", mob.pos);
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
    fn kinds_round_trip() {
        for kind in MobKind::ALL {
            assert_eq!(MobKind::parse(&format!("minecraft:{}", kind.name())), Some(kind));
        }
    }
}
