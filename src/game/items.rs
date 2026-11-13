// Dropped items: the entities blocks, deaths and tossing leave behind, falling and sliding
// as vanilla's ItemEntity does until a player walks over them.

use super::inventory::Stack;
use crate::world::{Dimension, blocks};
use uuid::Uuid;

/// Ticks until a dropped item disappears (five minutes).
pub const LIFETIME: u32 = 6000;
/// Pickup delays: blocks' drops, and items a player threw.
pub const BLOCK_DROP_DELAY: u32 = 10;
pub const THROWN_DELAY: u32 = 40;
const HALF_WIDTH: f64 = 0.125;
const HEIGHT: f64 = 0.25;

pub struct ItemEntity {
    pub id: i32,
    pub uuid: Uuid,
    pub dimension: Dimension,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub stack: Stack,
    pub pickup_delay: u32,
    pub age: u32,
    pub on_ground: bool,
    pub sent_pos: [f64; 3],
    /// The count players were last told about.
    pub sent_count: u8,
}

impl ItemEntity {
    pub fn new(id: i32, dimension: Dimension, pos: [f64; 3], vel: [f64; 3], stack: Stack, pickup_delay: u32) -> Self {
        Self {
            id,
            uuid: Uuid::new_v4(),
            dimension,
            pos,
            vel,
            stack,
            pickup_delay,
            age: 0,
            on_ground: false,
            sent_pos: pos,
            sent_count: stack.count,
        }
    }

    /// One tick of movement. Returns false once the item is gone (burnt in lava, or old).
    pub fn tick(&mut self, block: impl Fn(i32, i32, i32) -> Option<u16>) -> bool {
        self.age += 1;
        self.pickup_delay = self.pickup_delay.saturating_sub(1);
        if self.age >= LIFETIME {
            return false;
        }
        let at = |p: [f64; 3]| block(p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32);
        let Some(inside) = at(self.pos) else { return true }; // chunk not loaded: wait
        if blocks::is_lava(inside) {
            return false;
        }
        if blocks::is_solid(inside) && !blocks::is_liquid(inside) {
            // Buried by a placed block: float up out of it.
            self.pos[1] += 0.1;
            self.vel = [0.0, 0.0, 0.0];
            return true;
        }
        if blocks::is_swimmable(inside) {
            // ItemEntity.setUnderwaterMovement: drift up slowly.
            self.vel =
                [self.vel[0] * 0.99, self.vel[1] + if self.vel[1] < 0.06 { 5e-4 } else { 0.0 }, self.vel[2] * 0.99];
        } else {
            self.vel[1] -= 0.04;
        }

        let solid = |x: f64, y: f64, z: f64| at([x, y, z]).is_none_or(|s| blocks::is_solid(s) && !blocks::is_liquid(s));
        let corners = |x: f64, z: f64| {
            [
                (x - HALF_WIDTH, z - HALF_WIDTH),
                (x + HALF_WIDTH, z - HALF_WIDTH),
                (x - HALF_WIDTH, z + HALF_WIDTH),
                (x + HALF_WIDTH, z + HALF_WIDTH),
            ]
        };
        // Vertical first, then each horizontal axis, stopping at solid blocks.
        let [x, y, z] = self.pos;
        let ny = y + self.vel[1];
        self.on_ground = false;
        if self.vel[1] < 0.0 && corners(x, z).iter().any(|&(cx, cz)| solid(cx, ny, cz)) {
            self.pos[1] = ny.floor() + 1.0;
            self.vel[1] = 0.0;
            self.on_ground = true;
        } else if self.vel[1] > 0.0 && corners(x, z).iter().any(|&(cx, cz)| solid(cx, ny + HEIGHT, cz)) {
            self.vel[1] = 0.0;
        } else {
            self.pos[1] = ny;
        }
        let y = self.pos[1];
        for axis in [0, 2] {
            let mut p = self.pos;
            p[axis] += self.vel[axis];
            let blocked =
                corners(p[0], p[2]).iter().any(|&(cx, cz)| solid(cx, y + 0.01, cz) || solid(cx, y + HEIGHT, cz));
            if blocked {
                self.vel[axis] = 0.0;
            } else {
                self.pos = p;
            }
        }
        // Slipperiness 0.6 on the ground, as for most blocks.
        let friction = if self.on_ground { 0.6 * 0.98 } else { 0.98 };
        self.vel = [self.vel[0] * friction, self.vel[1] * 0.98, self.vel[2] * friction];
        if self.on_ground && self.vel[1] < 0.0 {
            self.vel[1] *= -0.5;
        }
        true
    }

    /// Whether a player standing at `feet` touches this item, using vanilla's pickup reach
    /// (the player's box grown by 1 sideways and 0.5 up and down).
    pub fn touches(&self, feet: [f64; 3]) -> bool {
        let reach_x = 0.3 + 1.0 + HALF_WIDTH;
        (self.pos[0] - feet[0]).abs() <= reach_x
            && (self.pos[2] - feet[2]).abs() <= reach_x
            && self.pos[1] + HEIGHT >= feet[1] - 0.5
            && self.pos[1] <= feet[1] + 1.8 + 0.5
    }

    /// Whether `other` lies close enough to join this item's stack.
    pub fn can_merge(&self, other: &ItemEntity) -> bool {
        self.id != other.id
            && self.dimension == other.dimension
            && self.stack.item == other.stack.item
            && self.stack.count as u16 + other.stack.count as u16 <= self.stack.max() as u16
            && (self.pos[0] - other.pos[0]).abs() <= 0.5 + 2.0 * HALF_WIDTH
            && (self.pos[2] - other.pos[2]).abs() <= 0.5 + 2.0 * HALF_WIDTH
            && (self.pos[1] - other.pos[1]).abs() <= HEIGHT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::registry::item_by_name;

    fn ground(_: i32, y: i32, _: i32) -> Option<u16> {
        Some(if y < 64 { blocks::STONE } else { blocks::AIR })
    }

    #[test]
    fn items_fall_and_settle_on_the_ground() {
        let stack = Stack::new(item_by_name("dirt").unwrap(), 1);
        let mut item = ItemEntity::new(1, Dimension::Overworld, [0.5, 70.0, 0.5], [0.05, 0.2, 0.0], stack, 10);
        for _ in 0..100 {
            assert!(item.tick(ground));
        }
        assert!(item.on_ground);
        assert!((item.pos[1] - 64.0).abs() < 1e-9, "{:?}", item.pos);
        assert!(item.vel[0].abs() < 1e-3);
        assert_eq!(item.pickup_delay, 0);
        assert!(item.touches([1.5, 64.0, 0.5]));
        assert!(!item.touches([3.5, 64.0, 0.5]));
    }

    #[test]
    fn walls_stop_items_and_lava_burns_them() {
        let wall = |x: i32, y: i32, z: i32| if x == 2 { Some(blocks::STONE) } else { ground(x, y, z) };
        let stack = Stack::new(item_by_name("dirt").unwrap(), 1);
        let mut item = ItemEntity::new(1, Dimension::Overworld, [1.5, 64.0, 0.5], [0.5, 0.0, 0.0], stack, 0);
        for _ in 0..20 {
            item.tick(wall);
        }
        assert!(item.pos[0] < 2.0 - HALF_WIDTH + 1e-9, "{:?}", item.pos);
        let lava = |_: i32, _: i32, _: i32| Some(blocks::LAVA);
        assert!(!item.tick(lava));
    }
}
