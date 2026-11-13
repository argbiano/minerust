// What players do with items: the inventory screen, placing blocks, buckets, and dropped
// items being thrown, falling and picked up.

use super::{
    DIG_REACH, Game, dist_sq,
    inventory::{ClickMode, Hand, Stack},
    items::{self, ItemEntity},
    place::{self, Click, Dir},
};
use crate::{
    players::Event,
    server::GameMode,
    world::{Dimension, Level, blocks, registry},
};
use uuid::Uuid;

/// How far buckets reach: vanilla's block interaction range, from the eyes.
const BUCKET_REACH: f64 = 4.5;
const EYE_HEIGHT: f64 = 1.62;

/// The rest of a two-block block (door, tall flower, bed) when one half is broken.
pub(super) fn other_half(level: &Level, pos: [i32; 3], state: u16) -> Option<([i32; 3], u16)> {
    let block = registry::block_of(state)?;
    let other = match (block.get(state, "half"), block.get(state, "part")) {
        (Some("lower"), _) => place::add(pos, [0, 1, 0]),
        (Some("upper"), _) => place::add(pos, [0, -1, 0]),
        (_, Some(part)) => {
            let facing = Dir::ALL.into_iter().find(|d| Some(d.name()) == block.get(state, "facing"))?;
            let toward = if part == "foot" { facing } else { facing.opposite() };
            place::add(pos, toward.offset())
        }
        _ => return None,
    };
    let there = level.cached_block(other[0], other[1], other[2])?;
    if !registry::block_of(there).is_some_and(|b| std::ptr::eq(b, block)) {
        return None;
    }
    let left = if blocks::is_waterlogged(there) { blocks::WATER } else { blocks::AIR };
    Some((other, left))
}

fn item_named(name: &str) -> u16 {
    registry::item_by_name(name).unwrap_or(registry::NONE)
}

impl Game {
    /// Sends the player's inventory as the server has it.
    pub(super) fn sync_inventory(&mut self, uuid: Uuid) {
        let Some(p) = self.players.get_mut(&uuid) else { return };
        let inv = &mut p.inventory;
        inv.state_id = (inv.state_id + 1) & 0x7FFF;
        let event = Event::Inventory { state_id: inv.state_id, slots: Box::new(inv.slots), carried: inv.carried };
        let selected = inv.selected as u8;
        self.send(uuid, event);
        self.send(uuid, Event::HeldSlot(selected));
    }

    fn can_act(&self, uuid: Uuid) -> bool {
        self.gamemode(uuid) != GameMode::Spectator && self.players.get(&uuid).is_some_and(|p| !p.dead && !p.travelling)
    }

    pub(super) fn click(&mut self, uuid: Uuid, slot: i16, button: u8, mode: ClickMode) {
        let creative = self.gamemode(uuid) == GameMode::Creative;
        if self.can_act(uuid)
            && let Some(p) = self.players.get_mut(&uuid)
        {
            let thrown = p.inventory.click(slot, button, mode, creative);
            for stack in thrown {
                self.toss(uuid, stack);
            }
        }
        self.sync_inventory(uuid);
    }

    pub(super) fn creative_slot(&mut self, uuid: Uuid, slot: i16, stack: Option<Stack>) {
        if self.gamemode(uuid) != GameMode::Creative {
            self.sync_inventory(uuid);
            return;
        }
        let Some(p) = self.players.get_mut(&uuid) else { return };
        if let Some(dropped) = p.inventory.set_creative(slot, stack) {
            self.toss(uuid, dropped);
        }
    }

    pub(super) fn close_inventory(&mut self, uuid: Uuid) {
        let Some(p) = self.players.get_mut(&uuid) else { return };
        if let Some(left) = p.inventory.close() {
            self.toss(uuid, left);
        }
        self.sync_inventory(uuid);
    }

    pub(super) fn drop_held(&mut self, uuid: Uuid, all: bool) {
        if self.can_act(uuid)
            && let Some(p) = self.players.get_mut(&uuid)
            && let Some(stack) = p.inventory.drop_held(all)
        {
            self.toss(uuid, stack);
        }
        self.sync_inventory(uuid);
    }

    pub(super) fn swap_hands(&mut self, uuid: Uuid) {
        if self.can_act(uuid)
            && let Some(p) = self.players.get_mut(&uuid)
        {
            p.inventory.swap_hands();
        }
        self.sync_inventory(uuid);
    }

    pub(super) fn pick_block(&mut self, uuid: Uuid, [x, y, z]: [i32; 3]) {
        let creative = self.gamemode(uuid) == GameMode::Creative;
        if !self.can_act(uuid) {
            return;
        }
        let Some(p) = self.players.get_mut(&uuid) else { return };
        let level = self.server.level(p.dimension);
        let Some(item) = level.cached_block(x, y, z).and_then(registry::block_of).map(|b| b.item) else { return };
        if item != registry::NONE && p.inventory.pick(item, creative) {
            self.sync_inventory(uuid);
        }
    }

    /// Right-click on a block with a block item: puts it there, if it fits.
    pub(super) fn use_item_on(
        &mut self,
        uuid: Uuid,
        hand: Hand,
        pos: [i32; 3],
        face: Dir,
        cursor: [f32; 3],
        sequence: i32,
    ) {
        let gamemode = self.gamemode(uuid);
        let placed = matches!(gamemode, GameMode::Survival | GameMode::Creative)
            && self.can_act(uuid)
            && self.try_place(uuid, hand, pos, face, cursor);
        if !placed && let Some(p) = self.players.get(&uuid) {
            // Undo what the client predicted.
            let level = self.server.level(p.dimension);
            for at in [pos, place::add(pos, face.offset())] {
                if let Some(state) = level.cached_block(at[0], at[1], at[2]) {
                    self.send(uuid, Event::BlockChanged { pos: at, state });
                }
            }
        }
        self.send(uuid, Event::BlockAck(sequence));
        self.sync_inventory(uuid);
    }

    fn try_place(&mut self, uuid: Uuid, hand: Hand, pos: [i32; 3], face: Dir, cursor: [f32; 3]) -> bool {
        let survival = self.gamemode(uuid) == GameMode::Survival;
        let Some(p) = self.players.get(&uuid) else { return false };
        let eye = [p.pos[0], p.pos[1] + EYE_HEIGHT, p.pos[2]];
        let centre = [pos[0] as f64 + 0.5, pos[1] as f64 + 0.5, pos[2] as f64 + 0.5];
        if dist_sq(eye, centre) > DIG_REACH * DIG_REACH {
            return false;
        }
        let Some(item) = p.inventory.held(hand).and_then(|s| registry::item(s.item)) else { return false };
        let dimension = p.dimension;
        let level = self.server.level(dimension);
        let click = Click { pos, face, cursor: cursor.map(|c| c.clamp(0.0, 1.0)), yaw: p.yaw, pitch: p.pitch };
        let Some(edits) = place::place(item, &click, |[x, y, z]| level.cached_block(x, y, z)) else { return false };
        // Solid blocks can't go where a player stands.
        let blocked = self.players.values().filter(|q| q.dimension == dimension && !q.dead).any(|q| {
            let (min, max) =
                ([q.pos[0] - 0.3, q.pos[1], q.pos[2] - 0.3], [q.pos[0] + 0.3, q.pos[1] + 1.8, q.pos[2] + 0.3]);
            edits.iter().any(|&(at, state)| place::intersects(state, at, min, max))
        });
        if blocked {
            return false;
        }
        self.apply_edits(dimension, &edits);
        if survival && let Some(p) = self.players.get_mut(&uuid) {
            p.inventory.consume(hand, 1);
        }
        true
    }

    /// Right-click with an item in the air or at a block: buckets fill and empty.
    pub(super) fn use_item(&mut self, uuid: Uuid, hand: Hand, yaw: f32, pitch: f32, sequence: i32) {
        let gamemode = self.gamemode(uuid);
        if matches!(gamemode, GameMode::Survival | GameMode::Creative)
            && self.can_act(uuid)
            && yaw.is_finite()
            && pitch.is_finite()
        {
            self.use_bucket(uuid, hand, yaw, pitch, gamemode == GameMode::Creative);
        }
        self.send(uuid, Event::BlockAck(sequence));
        self.sync_inventory(uuid);
    }

    fn use_bucket(&mut self, uuid: Uuid, hand: Hand, yaw: f32, pitch: f32, creative: bool) {
        let (empty, water, lava) = (item_named("bucket"), item_named("water_bucket"), item_named("lava_bucket"));
        let Some(p) = self.players.get(&uuid) else { return };
        let Some(held) = p.inventory.held(hand) else { return };
        if ![empty, water, lava].contains(&held.item) {
            return;
        }
        let dimension = p.dimension;
        let level = self.server.level(dimension);
        let eye = [p.pos[0], p.pos[1] + EYE_HEIGHT, p.pos[2]];
        let at = |[x, y, z]: [i32; 3]| level.cached_block(x, y, z);
        let dir = place::look(yaw, pitch);
        let solid = |s: u16| !blocks::is_air(s) && !blocks::is_liquid(s);

        let (edit, result) = if held.item == empty {
            // Stops at liquid sources as well as blocks.
            let Some((pos, _, state)) =
                place::raytrace(eye, dir, BUCKET_REACH, at, |s| s == blocks::WATER || s == blocks::LAVA || solid(s))
            else {
                return;
            };
            let block = registry::block_of(state);
            match state {
                blocks::WATER => ((pos, blocks::AIR), water),
                blocks::LAVA => ((pos, blocks::AIR), lava),
                _ if block.and_then(|b| b.get(state, "waterlogged")) == Some("true") => {
                    ((pos, block.unwrap().with(state, "waterlogged", "false")), water)
                }
                _ => return,
            }
        } else {
            let Some((hit, face, state)) = place::raytrace(eye, dir, BUCKET_REACH, at, solid) else { return };
            let fluid = if held.item == water { blocks::WATER } else { blocks::LAVA };
            let block = registry::block_of(state);
            // Water fills a waterloggable block it's poured on; otherwise it goes in front.
            let edit = if fluid == blocks::WATER && block.is_some_and(|b| b.has("waterlogged")) {
                Some((hit, block.unwrap().with(state, "waterlogged", "true")))
            } else {
                let pos = place::add(hit, face.offset());
                let there = at(pos);
                let target = there.and_then(registry::block_of);
                match there {
                    Some(s) if fluid == blocks::WATER && target.is_some_and(|b| b.has("waterlogged")) => {
                        Some((pos, target.unwrap().with(s, "waterlogged", "true")))
                    }
                    Some(s)
                        if blocks::is_air(s)
                            || target.is_some_and(|b| b.replaceable)
                            || blocks::is_fluid_replaceable(s) =>
                    {
                        Some((pos, fluid))
                    }
                    _ => None,
                }
            };
            let Some(edit) = edit else { return };
            // Water boils away in the nether, and the bucket still empties.
            if fluid == blocks::WATER && dimension == Dimension::Nether {
                if !creative && let Some(p) = self.players.get_mut(&uuid) {
                    p.inventory.set_held(hand, Some(Stack::new(empty, 1)));
                }
                return;
            }
            (edit, empty)
        };
        let ([x, y, z], _) = edit;
        if dist_sq(eye, [x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5]) > DIG_REACH * DIG_REACH {
            return;
        }
        self.apply_edits(dimension, &[edit]);
        let Some(p) = self.players.get_mut(&uuid) else { return };
        let inv = &mut p.inventory;
        let filled = Stack::new(result, 1);
        let leftover = if creative {
            // Creative keeps its bucket and gets the filled one if it has none.
            let has = inv.slots.iter().flatten().any(|s| s.item == result);
            if has || result == empty { None } else { inv.insert(filled) }
        } else if held.count == 1 {
            inv.set_held(hand, Some(filled));
            None
        } else {
            inv.consume(hand, 1);
            inv.insert(filled)
        };
        if let Some(left) = leftover {
            self.toss(uuid, left);
        }
    }

    /// What breaking these blocks drops for the player, by what they hold.
    pub(super) fn drops(&self, uuid: Uuid, dimension: Dimension, edits: &[([i32; 3], u16)]) -> Vec<([i32; 3], Stack)> {
        let held = self.players.get(&uuid).and_then(|p| p.inventory.held(Hand::Main)).map(|s| s.name());
        let level = self.server.level(dimension);
        edits
            .iter()
            .filter_map(|&(pos, _)| {
                let state = level.cached_block(pos[0], pos[1], pos[2])?;
                let block = registry::block_of(state)?;
                (block.drop != registry::NONE && block.drop_count > 0 && block.harvestable_with(held))
                    .then(|| (pos, Stack::new(block.drop, block.drop_count)))
            })
            .collect()
    }

    /// A block's drop popping out of where it was.
    pub(super) fn spawn_drop(&mut self, dimension: Dimension, [x, y, z]: [i32; 3], stack: Stack) {
        let r = &mut self.rng;
        let pos = [x as f64 + 0.25 + r.f64() * 0.5, y as f64 + 0.125 + r.f64() * 0.5, z as f64 + 0.25 + r.f64() * 0.5];
        let vel = [r.f64() * 0.2 - 0.1, 0.2, r.f64() * 0.2 - 0.1];
        let id = self.new_entity_id();
        self.items.insert(id, ItemEntity::new(id, dimension, pos, vel, stack, items::BLOCK_DROP_DELAY));
    }

    /// The player throws a stack the way they look.
    fn toss(&mut self, uuid: Uuid, stack: Stack) {
        let Some(p) = self.players.get(&uuid) else { return };
        let (dimension, feet, yaw, pitch) = (p.dimension, p.pos, p.yaw, p.pitch);
        let r = &mut self.rng;
        let [lx, ly, lz] = place::look(yaw, pitch);
        let angle = r.f64() * std::f64::consts::TAU;
        let spread = r.f64() * 0.02;
        let vel = [
            lx * 0.3 + angle.cos() * spread,
            ly * 0.3 + 0.1 + (r.f64() - r.f64()) * 0.1,
            lz * 0.3 + angle.sin() * spread,
        ];
        let pos = [feet[0], feet[1] + EYE_HEIGHT - 0.3, feet[2]];
        let id = self.new_entity_id();
        self.items.insert(id, ItemEntity::new(id, dimension, pos, vel, stack, items::THROWN_DELAY));
    }

    /// A dead player's inventory scatters around them.
    pub(super) fn spill_inventory(&mut self, uuid: Uuid) {
        let Some(p) = self.players.get_mut(&uuid) else { return };
        let stacks = p.inventory.take_all();
        let (dimension, feet) = (p.dimension, p.pos);
        for stack in stacks {
            let r = &mut self.rng;
            let (speed, angle) = (r.f64() * 0.5, r.f64() * std::f64::consts::TAU);
            let vel = [-angle.sin() * speed, 0.2, angle.cos() * speed];
            let pos = [feet[0], feet[1] + EYE_HEIGHT - 0.3, feet[2]];
            let id = self.new_entity_id();
            self.items.insert(id, ItemEntity::new(id, dimension, pos, vel, stack, items::THROWN_DELAY));
        }
        self.sync_inventory(uuid);
    }

    pub(super) fn tick_items(&mut self) {
        let server = self.server.clone();
        self.items.retain(|_, item| {
            let level = server.level(item.dimension);
            item.tick(|x, y, z| level.cached_block(x, y, z))
        });

        // Neighbouring stacks of the same item join up.
        if self.ticks.is_multiple_of(10) && self.items.len() < 4096 {
            let mut ids: Vec<i32> = self.items.keys().copied().collect();
            ids.sort_unstable();
            for &a in &ids {
                for &b in &ids {
                    let (Some(x), Some(y)) = (self.items.get(&a), self.items.get(&b)) else { continue };
                    if a < b && x.can_merge(y) {
                        let other = self.items.remove(&b).unwrap();
                        let x = self.items.get_mut(&a).unwrap();
                        x.stack.count += other.stack.count;
                        x.pickup_delay = x.pickup_delay.max(other.pickup_delay);
                        x.age = x.age.min(other.age);
                    }
                }
            }
        }

        // Pickup by players walking over them.
        let collectors: Vec<Uuid> = self
            .players
            .iter()
            .filter(|(uuid, p)| !p.dead && !p.travelling && self.gamemode(**uuid) != GameMode::Spectator)
            .map(|(uuid, _)| *uuid)
            .collect();
        let mut taken: Vec<(i32, i32, u8)> = Vec::new();
        let mut changed: Vec<Uuid> = Vec::new();
        for uuid in collectors {
            let Some(p) = self.players.get_mut(&uuid) else { continue };
            for item in self.items.values_mut() {
                if item.dimension != p.dimension
                    || item.pickup_delay > 0
                    || item.stack.count == 0
                    || !item.touches(p.pos)
                {
                    continue;
                }
                let left = p.inventory.insert(item.stack).map_or(0, |s| s.count);
                let amount = item.stack.count - left;
                if amount > 0 {
                    item.stack.count = left;
                    taken.push((item.id, p.entity_id, amount));
                    if !changed.contains(&uuid) {
                        changed.push(uuid);
                    }
                }
            }
        }
        for &(item, collector, amount) in &taken {
            for (uuid, q) in &self.players {
                if q.visible.contains(&item) || q.entity_id == collector {
                    self.send(*uuid, Event::TakeItem { item, collector, amount });
                }
            }
        }
        self.items.retain(|_, item| item.stack.count > 0);
        for uuid in changed {
            self.sync_inventory(uuid);
        }

        // Stacks that changed size (merged or partly picked up).
        let mut counts = Vec::new();
        for item in self.items.values_mut() {
            if item.sent_count != item.stack.count {
                item.sent_count = item.stack.count;
                counts.push((item.id, item.stack));
            }
        }
        for (id, stack) in counts {
            for (uuid, q) in &self.players {
                if q.visible.contains(&id) {
                    self.send(*uuid, Event::ItemCount { id, stack });
                }
            }
        }
    }
}
