// The server's 20 tick-per-second simulation: world time, mobs (spawning, AI, combat,
// despawning), player health, and nether portal travel. Connections feed it `Input`s
// and it answers through each player's event channel.

pub mod liquid;
pub mod mob;
pub mod path;
pub mod portal;

use crate::{
    players::{Color, Event},
    server::{GameMode, Server},
    world::{Chunk, Dimension, Level, blocks},
};
use mob::{Look, Mob, MobKind, Rng, Temper};
use portal::{Portal, PortalKey};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use uuid::Uuid;

pub const TICK: Duration = Duration::from_millis(50);
/// Mobs farther than this (horizontally) aren't sent to a player.
const TRACKING_RANGE: f64 = 64.0;
const MAX_HEALTH: f32 = 20.0;
const HOSTILE_CAP: usize = 20;
/// Spots tried per player each second for a monster.
const HOSTILE_TRIES: usize = 4;
const PASSIVE_CAP: usize = 10;
/// Newly loaded chunks brought to life per tick, and how far from players (in chunks).
const SETTLE_PER_TICK: usize = 2;
const SETTLE_RADIUS: i32 = 8;
/// Path searches all mobs together may run per tick.
const PATH_SEARCHES_PER_TICK: u32 = 8;
/// Liquid updates per tick; the rest wait for the next one.
const MAX_LIQUID_UPDATES: usize = 4096;

pub enum Input {
    Join {
        uuid: Uuid,
        name: String,
        entity_id: i32,
    },
    Leave {
        uuid: Uuid,
    },
    Move {
        uuid: Uuid,
        pos: Option<[f64; 3]>,
        yaw: Option<f32>,
        on_ground: bool,
    },
    Attack {
        uuid: Uuid,
        target: i32,
    },
    /// The client pressed "Respawn" on the death screen.
    Respawn {
        uuid: Uuid,
    },
    Summon {
        uuid: Uuid,
        kind: MobKind,
    },
    BuildPortal {
        uuid: Uuid,
    },
    SetTime(i64),
    /// /setblock and /fill.
    SetBlocks {
        dimension: Dimension,
        blocks: Vec<portal::Edit>,
    },
    /// A destination portal finished generating.
    Arrived {
        uuid: Uuid,
        portal: Portal,
        edits: Vec<portal::Edit>,
        new: bool,
        source: Option<PortalKey>,
    },
    TravelFailed {
        uuid: Uuid,
    },
}

pub struct Handle {
    tx: mpsc::UnboundedSender<Input>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Input>>>,
}

impl Handle {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { tx, rx: Mutex::new(Some(rx)) }
    }

    pub fn send(&self, input: Input) {
        let _ = self.tx.send(input);
    }
}

impl Default for Handle {
    fn default() -> Self {
        Self::new()
    }
}

pub fn is_day(time: i64) -> bool {
    let t = time.rem_euclid(24000);
    !(12542..23460).contains(&t)
}

struct PlayerState {
    name: String,
    entity_id: i32,
    dimension: Dimension,
    pos: [f64; 3],
    yaw: f32,
    on_ground: bool,
    /// Highest y since last standing on the ground, for fall damage.
    fall_peak: Option<f64>,
    health: f32,
    dead: bool,
    invulnerable: u32,
    regen: u32,
    portal_ticks: u32,
    /// Set after arriving through a portal: it must be left before it works again.
    portal_wait: bool,
    travelling: bool,
    /// Ticks left burning after leaving lava.
    fire: u32,
    visible: HashSet<i32>,
}

/// Something a mob did that touches other state.
enum Action {
    HurtPlayer { uuid: Uuid, amount: f32, from: [f64; 3], cause: String },
    Explode { pos: [f64; 3], source: i32 },
    Swing { id: i32 },
}

pub async fn run(server: Arc<Server>) {
    let Some(mut rx) = server.game.rx.lock().unwrap().take() else {
        warn!("game loop already running");
        return;
    };
    let mut game = Game::new(server);
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        while let Ok(input) = rx.try_recv() {
            game.input(input);
        }
        game.tick();
    }
}

struct Game {
    server: Arc<Server>,
    players: HashMap<Uuid, PlayerState>,
    mobs: HashMap<i32, Mob>,
    portals: Vec<Portal>,
    /// Where each portal last sent someone, both ways.
    links: HashMap<PortalKey, PortalKey>,
    rng: Rng,
    ticks: u64,
    liquid_queue: BTreeMap<u64, Vec<(Dimension, liquid::Pos)>>,
    liquid_pending: HashSet<(Dimension, liquid::Pos)>,
    /// Chunks whose liquids and animals were already set going.
    settled: HashSet<(Dimension, (i32, i32))>,
}

impl Game {
    fn new(server: Arc<Server>) -> Self {
        Self {
            server,
            players: HashMap::new(),
            mobs: HashMap::new(),
            portals: Vec::new(),
            links: HashMap::new(),
            rng: Rng::new(rand::random()),
            ticks: 0,
            liquid_queue: BTreeMap::new(),
            liquid_pending: HashSet::new(),
            settled: HashSet::new(),
        }
    }

    fn send(&self, uuid: Uuid, event: Event) {
        if let Some(p) = self.server.players.get(uuid) {
            p.send(event);
        }
    }

    fn gamemode(&self, uuid: Uuid) -> GameMode {
        self.server.players.get(uuid).map_or(GameMode::Spectator, |p| p.gamemode())
    }

    fn takes_damage(&self, uuid: Uuid) -> bool {
        matches!(self.gamemode(uuid), GameMode::Survival | GameMode::Adventure)
    }

    fn new_entity_id(&self) -> i32 {
        self.server.next_entity_id.fetch_add(1, Ordering::Relaxed)
    }

    fn input(&mut self, input: Input) {
        match input {
            Input::Join { uuid, name, entity_id } => {
                self.players.insert(
                    uuid,
                    PlayerState {
                        name,
                        entity_id,
                        dimension: Dimension::Overworld,
                        pos: self.server.spawn,
                        yaw: 0.0,
                        on_ground: true,
                        fall_peak: None,
                        health: MAX_HEALTH,
                        dead: false,
                        invulnerable: 0,
                        regen: 0,
                        portal_ticks: 0,
                        portal_wait: false,
                        travelling: false,
                        fire: 0,
                        visible: HashSet::new(),
                    },
                );
                self.send(uuid, Event::Health(MAX_HEALTH));
            }
            Input::Leave { uuid } => {
                self.players.remove(&uuid);
                for mob in self.mobs.values_mut() {
                    if mob.target == Some(uuid) {
                        mob.target = None;
                    }
                }
            }
            Input::Move { uuid, pos, yaw, on_ground } => self.moved(uuid, pos, yaw, on_ground),
            Input::Attack { uuid, target } => self.attack(uuid, target),
            Input::Respawn { uuid } => self.respawn(uuid),
            Input::Summon { uuid, kind } => {
                let Some(p) = self.players.get(&uuid) else { return };
                let (dimension, pos, yaw) = (p.dimension, p.pos, p.yaw);
                let mut mob = Mob::new(self.new_entity_id(), kind, dimension, pos, yaw);
                mob.persistent = true;
                self.mobs.insert(mob.id, mob);
            }
            Input::BuildPortal { uuid } => {
                let Some(p) = self.players.get(&uuid) else { return };
                let portal = portal::in_front(p.dimension, p.pos, p.yaw);
                let edits = portal.blocks();
                self.apply_edits(portal.dimension, &edits);
                self.portals.push(portal);
            }
            Input::SetBlocks { dimension, blocks } => self.apply_edits(dimension, &blocks),
            Input::SetTime(time) => {
                self.server.time.store(time, Ordering::Relaxed);
                for uuid in self.players.keys() {
                    self.send(*uuid, Event::Time(time));
                }
            }
            Input::Arrived { uuid, portal, edits, new, source } => self.arrive(uuid, portal, &edits, new, source),
            Input::TravelFailed { uuid } => {
                if let Some(p) = self.players.get_mut(&uuid) {
                    p.travelling = false;
                    p.portal_wait = true;
                }
            }
        }
    }

    fn moved(&mut self, uuid: Uuid, pos: Option<[f64; 3]>, yaw: Option<f32>, on_ground: bool) {
        let takes_damage = self.takes_damage(uuid);
        let Some(p) = self.players.get_mut(&uuid) else { return };
        if p.travelling {
            return;
        }
        if let Some(yaw) = yaw {
            p.yaw = yaw;
        }
        let Some(pos) = pos else { return };
        p.pos = pos;
        let level = self.server.level(p.dimension);
        let in_liquid = level
            .cached_block(pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32)
            .is_some_and(blocks::is_liquid);
        let mut fall = 0.0;
        if !takes_damage || in_liquid || p.dead {
            p.fall_peak = None;
        } else if !on_ground {
            p.fall_peak = Some(p.fall_peak.map_or(pos[1], |peak| peak.max(pos[1])));
        } else if let Some(peak) = p.fall_peak.take() {
            fall = peak - pos[1];
        }
        p.on_ground = on_ground;
        if fall > 3.0 {
            self.hurt_player(uuid, (fall - 3.0).ceil() as f32, None, "fell from a high place".into());
        }
    }

    fn attack(&mut self, uuid: Uuid, target: i32) {
        let Some(p) = self.players.get(&uuid) else { return };
        if p.dead || self.gamemode(uuid) == GameMode::Spectator {
            return;
        }
        let Some(mob) = self.mobs.get(&target) else { return };
        let eye = [p.pos[0], p.pos[1] + 1.62, p.pos[2]];
        let mid = [mob.pos[0], mob.pos[1] + 0.9, mob.pos[2]];
        if mob.dimension != p.dimension || dist_sq(eye, mid) > 6.0 * 6.0 {
            return;
        }
        // Bare hands, as there is no inventory yet; falling hits are critical.
        let critical = !p.on_ground && p.fall_peak.is_some_and(|peak| peak > p.pos[1]);
        let damage = if critical { 1.5 } else { 1.0 };
        let from = p.pos;
        self.hurt_mob(target, damage, Some(from), Some(uuid));
    }

    fn respawn(&mut self, uuid: Uuid) {
        let spawn = self.server.spawn;
        let Some(p) = self.players.get_mut(&uuid) else { return };
        if !p.dead {
            return;
        }
        p.dead = false;
        p.fire = 0;
        p.health = MAX_HEALTH;
        p.dimension = Dimension::Overworld;
        p.pos = spawn;
        p.fall_peak = None;
        p.portal_ticks = 0;
        p.portal_wait = false;
        p.visible.clear();
        self.send(uuid, Event::ChangeDimension { dimension: Dimension::Overworld, pos: spawn, yaw: 0.0, death: true });
        self.send(uuid, Event::Health(MAX_HEALTH));
    }

    fn arrive(&mut self, uuid: Uuid, portal: Portal, edits: &[portal::Edit], new: bool, source: Option<PortalKey>) {
        self.apply_edits(portal.dimension, edits);
        if new {
            self.portals.push(portal.clone());
        }
        if let Some(source) = source {
            self.links.insert(source, portal.key());
            self.links.insert(portal.key(), source);
        }
        let Some(p) = self.players.get_mut(&uuid) else { return };
        p.travelling = false;
        if p.dead {
            return;
        }
        p.dimension = portal.dimension;
        p.pos = portal.arrival();
        p.fall_peak = None;
        p.portal_ticks = 0;
        p.portal_wait = true;
        // The client forgets every entity when it changes dimension.
        p.visible.clear();
        let (pos, yaw) = (p.pos, p.yaw);
        self.send(uuid, Event::ChangeDimension { dimension: portal.dimension, pos, yaw, death: false });
    }

    /// Changes blocks, tells the players who can see them, and wakes nearby liquids.
    fn apply_edits(&mut self, dimension: Dimension, edits: &[portal::Edit]) {
        let level = self.server.level(dimension);
        for &([x, y, z], state) in edits {
            level.set_block(x, y, z, state);
        }
        self.after_change(dimension, edits);
    }

    fn after_change(&mut self, dimension: Dimension, edits: &[portal::Edit]) {
        for (uuid, p) in &self.players {
            if p.dimension == dimension {
                for &(pos, state) in edits {
                    self.send(*uuid, Event::BlockChanged { pos, state });
                }
            }
        }
        for &(pos, _) in edits {
            self.wake_liquids(dimension, pos);
        }
    }

    /// Schedules the liquid at `pos` and around it, as vanilla's neighbour updates do.
    fn wake_liquids(&mut self, dimension: Dimension, [x, y, z]: liquid::Pos) {
        let server = self.server.clone();
        let level = server.level(dimension);
        let rules = liquid_rules(dimension);
        for p in [[x, y, z], [x, y + 1, z], [x, y - 1, z], [x + 1, y, z], [x - 1, y, z], [x, y, z + 1], [x, y, z - 1]] {
            let Some((fluid, _)) = level.cached_block(p[0], p[1], p[2]).and_then(liquid::fluid) else { continue };
            self.schedule_liquid(dimension, p, fluid.delay(rules));
        }
    }

    fn schedule_liquid(&mut self, dimension: Dimension, pos: liquid::Pos, delay: u64) {
        if self.liquid_pending.insert((dimension, pos)) {
            self.liquid_queue.entry(self.ticks + delay).or_default().push((dimension, pos));
        }
    }

    fn tick_liquids(&mut self) {
        let mut budget = MAX_LIQUID_UPDATES;
        while let Some(mut entry) = self.liquid_queue.first_entry() {
            if *entry.key() > self.ticks || budget == 0 {
                break;
            }
            let due = entry.get_mut();
            let take = due.len().min(budget);
            let batch: Vec<_> = due.drain(..take).collect();
            if due.is_empty() {
                entry.remove();
            }
            budget -= take;
            for (dimension, pos) in batch {
                self.liquid_pending.remove(&(dimension, pos));
                let mut world = LevelWorld { level: self.server.level(dimension), changes: Vec::new() };
                liquid::tick(&mut world, pos, liquid_rules(dimension));
                let changes = world.changes;
                if !changes.is_empty() {
                    self.after_change(dimension, &changes);
                }
            }
        }
    }

    fn tick(&mut self) {
        self.ticks += 1;
        let time = self.server.time.fetch_add(1, Ordering::Relaxed) + 1;
        self.settle_chunks();
        self.tick_liquids();
        self.tick_players();
        self.tick_mobs(time);
        if self.ticks.is_multiple_of(20) {
            self.spawn_mobs(time);
            self.despawn_mobs();
        }
        self.track();
    }

    fn tick_players(&mut self) {
        let uuids: Vec<Uuid> = self.players.keys().copied().collect();
        for uuid in uuids {
            let gamemode = self.gamemode(uuid);
            let p = self.players.get_mut(&uuid).unwrap();
            p.invulnerable = p.invulnerable.saturating_sub(1);
            if p.dead || p.travelling {
                continue;
            }
            p.regen += 1;
            if p.regen >= 80 {
                p.regen = 0;
                if p.health < MAX_HEALTH {
                    p.health = (p.health + 1.0).min(MAX_HEALTH);
                    let health = p.health;
                    self.send(uuid, Event::Health(health));
                }
            }

            let p = self.players.get_mut(&uuid).unwrap();
            let level = self.server.level(p.dimension);
            let (x, y, z) = (p.pos[0].floor() as i32, p.pos[1].floor() as i32, p.pos[2].floor() as i32);
            let body = [y, y + 1].map(|y| level.cached_block(x, y, z));
            let in_lava = body.iter().any(|b| b.is_some_and(blocks::is_lava));
            let in_water = body.iter().any(|b| b.is_some_and(|b| blocks::is_liquid(b) && !blocks::is_lava(b)));
            let burns = !matches!(gamemode, GameMode::Creative | GameMode::Spectator);
            let was_on_fire = p.fire > 0;
            if in_water || !burns {
                p.fire = 0;
            } else if in_lava {
                // Vanilla: 15 seconds of fire after leaving lava.
                p.fire = 300;
            } else {
                p.fire = p.fire.saturating_sub(1);
            }
            let fire = p.fire;
            let entity_id = p.entity_id;
            let in_portal = body.iter().any(|b| b.is_some_and(blocks::is_portal));
            if (fire > 0) != was_on_fire {
                self.send(uuid, Event::EntityOnFire { id: entity_id, on_fire: fire > 0 });
            }
            if in_lava && burns {
                self.hurt_player(uuid, 4.0, None, "tried to swim in lava".into());
            } else if fire > 0 && fire.is_multiple_of(20) {
                self.hurt_player(uuid, 1.0, None, "burned to death".into());
            }

            let Some(p) = self.players.get_mut(&uuid) else { continue };
            if p.dead {
                continue;
            }
            if !in_portal {
                p.portal_ticks = 0;
                p.portal_wait = false;
                continue;
            }
            if p.portal_wait || gamemode == GameMode::Spectator {
                continue;
            }
            p.portal_ticks += 1;
            // Vanilla: creative players go through at once, others after 4 seconds.
            let wait = if gamemode == GameMode::Creative { 1 } else { 80 };
            if p.portal_ticks >= wait {
                p.portal_ticks = 0;
                p.travelling = true;
                let (from, pos) = (p.dimension, p.pos);
                self.travel(uuid, from, pos);
            }
        }
    }

    fn travel(&mut self, uuid: Uuid, from: Dimension, pos: [f64; 3]) {
        let to = from.other();
        let feet = [pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32];
        let source = self
            .portals
            .iter()
            .find(|p| p.contains(from, feet) || p.contains(from, [feet[0], feet[1] + 1, feet[2]]))
            .map(Portal::key);
        // A portal goes back to wherever it last led, even when vanilla's search radius wouldn't find it.
        let linked = source
            .and_then(|s| self.links.get(&s))
            .and_then(|dest| self.portals.iter().find(|p| p.key() == *dest))
            .cloned();
        if let Some(portal) = linked {
            self.arrive(uuid, portal, &[], false, None);
            return;
        }
        let target = portal::scaled_target(from, to, pos);
        let radius = portal::search_radius(to);
        let existing = self
            .portals
            .iter()
            .filter(|p| p.dimension == to && p.horizontal_distance_sq(target[0], target[2]) <= radius * radius)
            .min_by(|a, b| {
                a.horizontal_distance_sq(target[0], target[2])
                    .total_cmp(&b.horizontal_distance_sq(target[0], target[2]))
            })
            .cloned();
        if let Some(portal) = existing {
            self.arrive(uuid, portal, &[], false, source);
            return;
        }
        let server = self.server.clone();
        tokio::spawn(async move {
            let input = match portal::build_destination(server.clone(), to, target).await {
                Ok((portal, edits)) => Input::Arrived { uuid, portal, edits, new: true, source },
                Err(e) => {
                    warn!("building a portal in {}: {e:#}", to.name());
                    Input::TravelFailed { uuid }
                }
            };
            server.game.send(input);
        });
    }

    fn hurt_player(&mut self, uuid: Uuid, amount: f32, from: Option<[f64; 3]>, cause: String) {
        if !self.takes_damage(uuid) {
            return;
        }
        let Some(p) = self.players.get_mut(&uuid) else { return };
        if p.dead || p.invulnerable > 0 || amount <= 0.0 {
            return;
        }
        p.invulnerable = 10;
        p.health = (p.health - amount).max(0.0);
        // Damage direction relative to where the player looks, for the camera tilt.
        let yaw = from
            .map_or(0.0, |f| ((f[2] - p.pos[2]).atan2(f[0] - p.pos[0]).to_degrees() as f32 - p.yaw).rem_euclid(360.0));
        let (id, health, name) = (p.entity_id, p.health, p.name.clone());
        if health <= 0.0 {
            p.dead = true;
            p.fall_peak = None;
        }
        self.send(uuid, Event::EntityHurt { id, yaw });
        self.send(uuid, Event::Health(health));
        if health <= 0.0 {
            self.server.players.broadcast(&format!("{name} {cause}"), Color::White);
        }
    }

    fn hurt_mob(&mut self, id: i32, amount: f32, from: Option<[f64; 3]>, attacker: Option<Uuid>) {
        let Some(mob) = self.mobs.get_mut(&id) else { return };
        if !mob.alive() || mob.hurt_time > 0 {
            return;
        }
        mob.hurt_time = 10;
        mob.health -= amount;
        if let Some(from) = from {
            let (dx, dz) = (mob.pos[0] - from[0], mob.pos[2] - from[2]);
            let len = (dx * dx + dz * dz).sqrt().max(1e-3);
            mob.vel[0] = dx / len * 0.4;
            mob.vel[2] = dz / len * 0.4;
            if mob.on_ground {
                mob.vel[1] = 0.36;
            }
            mob.panic_from = from;
        }
        let kind = mob.kind;
        match kind.temper() {
            Temper::Passive => mob.panic = 60,
            _ if attacker.is_some() => {
                mob.target = attacker;
                mob.anger = 600;
            }
            _ => {}
        }
        let dead = mob.health <= 0.0;
        if dead {
            mob.death_time = Some(0);
        }
        let (pos, dimension) = (mob.pos, mob.dimension);
        self.to_viewers(id, || Event::EntityHurt { id, yaw: 0.0 });
        if dead {
            self.to_viewers(id, || Event::EntityStatus { id, status: 3 });
        }
        // Zombified piglins defend each other.
        if kind == MobKind::ZombifiedPiglin && attacker.is_some() {
            for other in self.mobs.values_mut() {
                if other.kind == kind && other.dimension == dimension && dist_sq(other.pos, pos) < 32.0 * 32.0 {
                    other.target = attacker;
                    other.anger = other.anger.max(400);
                }
            }
        }
    }

    fn to_viewers(&self, id: i32, event: impl Fn() -> Event) {
        for (uuid, p) in &self.players {
            if p.visible.contains(&id) {
                self.send(*uuid, event());
            }
        }
    }

    fn tick_mobs(&mut self, time: i64) {
        let day = is_day(time);
        let mut actions = Vec::new();
        let mut burning = Vec::new();
        let mut removed = Vec::new();
        let players: Vec<Seen> = self
            .players
            .iter()
            .map(|(uuid, p)| {
                let here = !p.dead && !p.travelling;
                Seen {
                    uuid: *uuid,
                    dimension: p.dimension,
                    pos: p.pos,
                    attackable: here && self.takes_damage(*uuid),
                    visible: here && self.gamemode(*uuid) != GameMode::Spectator,
                }
            })
            .collect();
        let mut searches = PATH_SEARCHES_PER_TICK;

        for mob in self.mobs.values_mut() {
            mob.age += 1;
            mob.hurt_time = mob.hurt_time.saturating_sub(1);
            mob.attack_cooldown = mob.attack_cooldown.saturating_sub(1);
            if let Some(t) = &mut mob.death_time {
                *t += 1;
                if *t >= 20 {
                    removed.push(mob.id);
                }
                continue;
            }
            let level = self.server.level(mob.dimension);
            let block = |x: i32, y: i32, z: i32| level.cached_block(x, y, z);
            // Frozen until its chunk is loaded again.
            if block(mob.pos[0].floor() as i32, mob.pos[1].floor() as i32, mob.pos[2].floor() as i32).is_none() {
                continue;
            }
            if mob.pos[1] < level.info().min_y as f64 - 64.0 {
                removed.push(mob.id);
                continue;
            }

            let mut ctx = Ctx { level, players: &players, day, rng: &mut self.rng, searches: &mut searches };
            let (walk, look_at) = think(mob, &mut ctx, &mut actions);
            mob.turn(walk, look_at);
            // Paths already keep mobs off drops and out of water; without one, step carefully.
            let cautious = mob.path.is_empty() && mob.target.is_none() && mob.panic == 0;
            if !mob.step(walk, cautious, &block) {
                mob.clear_path();
            }

            let (x, y, z) = (mob.pos[0].floor() as i32, mob.pos[1].floor() as i32, mob.pos[2].floor() as i32);
            let feet = block(x, y, z).unwrap_or(blocks::AIR);
            if blocks::is_lava(feet) {
                mob.fire = 300;
                burning.push((mob.id, 4.0));
            } else if blocks::is_liquid(feet) {
                mob.fire = 0;
            }
            let exposed = level.cached_surface(x, z).is_some_and(|top| top <= y + 2);
            if mob.kind.burns_in_daylight() && day && mob.dimension == Dimension::Overworld && exposed {
                mob.fire = mob.fire.max(160);
            }
            if mob.fire > 0 {
                mob.fire -= 1;
                // The fire timer is refreshed every tick in sunlight, so it can't pace the damage.
                if mob.age.is_multiple_of(20) {
                    burning.push((mob.id, 1.0));
                }
            }
        }

        for (id, amount) in burning {
            self.hurt_mob(id, amount, None, None);
        }
        for action in actions {
            match action {
                Action::HurtPlayer { uuid, amount, from, cause } => self.hurt_player(uuid, amount, Some(from), cause),
                Action::Swing { id } => self.to_viewers(id, || Event::Swing { id }),
                Action::Explode { pos, source } => {
                    removed.push(source);
                    let dimension = self.mobs.get(&source).map(|m| m.dimension);
                    let hit: Vec<(Uuid, f32)> = self
                        .players
                        .iter()
                        .filter(|(_, p)| Some(p.dimension) == dimension)
                        .filter_map(|(uuid, p)| explosion_damage(pos, p.pos).map(|d| (*uuid, d)))
                        .collect();
                    for (uuid, damage) in hit {
                        self.hurt_player(uuid, damage, Some(pos), "was blown up by Creeper".into());
                    }
                    let mobs: Vec<(i32, f32)> = self
                        .mobs
                        .values()
                        .filter(|m| m.id != source && Some(m.dimension) == dimension)
                        .filter_map(|m| explosion_damage(pos, m.pos).map(|d| (m.id, d)))
                        .collect();
                    for (id, damage) in mobs {
                        self.hurt_mob(id, damage, Some(pos), None);
                    }
                }
            }
        }
        self.remove_mobs(&removed);
    }

    fn remove_mobs(&mut self, ids: &[i32]) {
        if ids.is_empty() {
            return;
        }
        for id in ids {
            self.mobs.remove(id);
        }
        let mut sends = Vec::new();
        for (uuid, p) in &mut self.players {
            let gone: Vec<i32> = ids.iter().copied().filter(|id| p.visible.remove(id)).collect();
            if !gone.is_empty() {
                sends.push((*uuid, gone));
            }
        }
        for (uuid, gone) in sends {
            self.send(uuid, Event::RemoveEntities(gone));
        }
    }

    fn spawn_mobs(&mut self, time: i64) {
        let day = is_day(time);
        let anchors: Vec<(Dimension, [f64; 3])> = self
            .players
            .iter()
            .filter(|(uuid, p)| !p.dead && !p.travelling && self.gamemode(**uuid) != GameMode::Spectator)
            .map(|(_, p)| (p.dimension, p.pos))
            .collect();
        for (dimension, center) in anchors {
            let (mut hostile, mut passive) = (0, 0);
            for m in self.mobs.values().filter(|m| m.dimension == dimension && dist_sq(m.pos, center) < 128.0 * 128.0) {
                match m.kind.temper() {
                    Temper::Passive => passive += 1,
                    _ => hostile += 1,
                }
            }
            if hostile < HOSTILE_CAP {
                // Vanilla tries a spot in every loaded chunk each tick, most of them underground.
                for _ in 0..HOSTILE_TRIES {
                    let kind = match dimension {
                        Dimension::Nether => MobKind::ZombifiedPiglin,
                        Dimension::Overworld => self.hostile_kind(),
                    };
                    if let Some(pos) = self.find_monster_spot(dimension, center, day) {
                        self.spawn(kind, dimension, pos);
                        break;
                    }
                }
            }
            // Animals mostly come with new chunks; this tops them up now and then, like
            // vanilla's pass for persistent creatures every 400 ticks.
            if dimension == Dimension::Overworld && passive < PASSIVE_CAP && self.ticks.is_multiple_of(400) {
                let (x, z) = self.ring(center);
                if self.animal_spot(dimension, x, z).is_some_and(|p| !self.near_player(dimension, p)) {
                    let kind = self.animal_kind();
                    self.spawn_herd(kind, dimension, x, z);
                }
            }
        }
    }

    fn hostile_kind(&mut self) -> MobKind {
        match self.rng.range(0, 100) {
            0..40 => MobKind::Zombie,
            40..70 => MobKind::Skeleton,
            70..85 => MobKind::Spider,
            _ => MobKind::Creeper,
        }
    }

    /// Vanilla's farm animal weights: sheep 12, pig 10, chicken 10, cow 8.
    fn animal_kind(&mut self) -> MobKind {
        match self.rng.range(0, 40) {
            0..12 => MobKind::Sheep,
            12..22 => MobKind::Pig,
            22..32 => MobKind::Chicken,
            _ => MobKind::Cow,
        }
    }

    fn spawn(&mut self, kind: MobKind, dimension: Dimension, pos: [f64; 3]) {
        let yaw = self.rng.f64() as f32 * 360.0 - 180.0;
        let mob = Mob::new(self.new_entity_id(), kind, dimension, pos, yaw);
        debug!("spawned {} at {:.1} {:.1} {:.1}", kind.name(), pos[0], pos[1], pos[2]);
        self.mobs.insert(mob.id, mob);
    }

    /// A group of four around column (x, z), as vanilla spawns farm animals.
    fn spawn_herd(&mut self, kind: MobKind, dimension: Dimension, x: i32, z: i32) {
        let mut spawned = 0;
        for _ in 0..12 {
            if spawned == 4 {
                break;
            }
            let (ax, az) = (x + self.rng.range(-3, 4), z + self.rng.range(-3, 4));
            if let Some(pos) = self.animal_spot(dimension, ax, az) {
                self.spawn(kind, dimension, pos);
                spawned += 1;
            }
        }
    }

    /// A random column 24 to 48 blocks from `center`, where vanilla spawns mobs.
    fn ring(&mut self, center: [f64; 3]) -> (i32, i32) {
        let angle = self.rng.f64() * std::f64::consts::TAU;
        let r = 24.0 + self.rng.f64() * 24.0;
        ((center[0] + angle.cos() * r).floor() as i32, (center[2] + angle.sin() * r).floor() as i32)
    }

    fn near_player(&self, dimension: Dimension, pos: [f64; 3]) -> bool {
        self.players.values().any(|p| p.dimension == dimension && dist_sq(p.pos, pos) < 24.0 * 24.0)
    }

    /// Where a monster may appear: a dry floor with room above, in the dark. With no
    /// lighting engine yet, dark means out of the sky at night or under cover, with no
    /// light-giving block within 6 blocks.
    fn find_monster_spot(&mut self, dimension: Dimension, center: [f64; 3], day: bool) -> Option<[f64; 3]> {
        let (x, z) = self.ring(center);
        let level = self.server.level(dimension);
        let info = level.info();
        let top = level.cached_surface(x, z)?;
        let hi = match dimension {
            Dimension::Overworld => top,
            // Under the bedrock roof.
            Dimension::Nether => (info.min_y + 120).min(top),
        };
        // Like vanilla, a random height up to the surface, so most tries land in caves.
        let start = self.rng.range(info.min_y + 1, hi.max(info.min_y + 1) + 1);
        let open = |y: i32| level.cached_block(x, y, z).is_some_and(|b| !blocks::is_solid(b) && !blocks::is_liquid(b));
        let y = (start - 16..=start).rev().find(|&y| {
            open(y)
                && open(y + 1)
                && level.cached_block(x, y - 1, z).is_some_and(|b| blocks::is_solid(b) && !blocks::is_liquid(b))
        })?;
        let pos = [x as f64 + 0.5, y as f64, z as f64 + 0.5];
        if self.near_player(dimension, pos) {
            return None;
        }
        let under_sky = top <= y + 1;
        if dimension == Dimension::Overworld && under_sky && day {
            return None;
        }
        if near_light(level, [x, y, z], 6) {
            return None;
        }
        Some(pos)
    }

    /// Where an animal may stand in column (x, z): on grass under the open sky.
    fn animal_spot(&self, dimension: Dimension, x: i32, z: i32) -> Option<[f64; 3]> {
        let level = self.server.level(dimension);
        let mut y = level.cached_surface(x, z)?;
        // Down through grass and flowers to the ground they grow on.
        for _ in 0..3 {
            let below = level.cached_block(x, y - 1, z)?;
            if blocks::is_solid(below) || blocks::is_liquid(below) {
                break;
            }
            y -= 1;
        }
        let open = |y: i32| level.cached_block(x, y, z).is_some_and(|b| !blocks::is_solid(b) && !blocks::is_liquid(b));
        (blocks::is_grass_block(level.cached_block(x, y - 1, z)?) && open(y) && open(y + 1)).then_some([
            x as f64 + 0.5,
            y as f64,
            z as f64 + 0.5,
        ])
    }

    fn despawn_mobs(&mut self) {
        let mut gone = Vec::new();
        for mob in self.mobs.values() {
            // Vanilla animals stay where they are, however far away players go.
            if mob.persistent || !mob.alive() || mob.kind.temper() == Temper::Passive {
                continue;
            }
            let nearest = self
                .players
                .values()
                .filter(|p| p.dimension == mob.dimension)
                .map(|p| dist_sq(p.pos, mob.pos))
                .fold(f64::INFINITY, f64::min);
            let far = nearest > 128.0 * 128.0;
            // Vanilla rolls 1/800 per tick past 32 blocks; this runs every 20 ticks.
            let random = nearest > 32.0 * 32.0 && mob.age > 600;
            if far || (random && self.rng.chance(1.0 / 40.0)) {
                gone.push(mob.id);
            }
        }
        self.remove_mobs(&gone);
    }

    /// Brings newly loaded chunks to life, nearest the players first: liquids that vanilla's
    /// world generator would have left flowing start flowing, and animals are placed the
    /// way vanilla populates a chunk when it is generated.
    fn settle_chunks(&mut self) {
        let mut budget = SETTLE_PER_TICK;
        let centers: Vec<(Dimension, (i32, i32))> = self
            .players
            .values()
            .filter(|p| !p.travelling)
            .map(|p| (p.dimension, ((p.pos[0].floor() as i32) >> 4, (p.pos[2].floor() as i32) >> 4)))
            .collect();
        for (dimension, (pcx, pcz)) in centers {
            for r in 0..=SETTLE_RADIUS {
                for (cx, cz) in ring_chunks(pcx, pcz, r) {
                    if budget == 0 {
                        return;
                    }
                    if self.settled.contains(&(dimension, (cx, cz))) {
                        continue;
                    }
                    // Its neighbours too, or liquids at the edge would take unloaded blocks for walls.
                    let level = self.server.level(dimension);
                    let mut chunks = HashMap::new();
                    for dz in -1..=1 {
                        for dx in -1..=1 {
                            if let Some(c) = level.cached_chunk(cx + dx, cz + dz) {
                                chunks.insert((cx + dx, cz + dz), c);
                            }
                        }
                    }
                    if chunks.len() < 9 {
                        continue;
                    }
                    self.settled.insert((dimension, (cx, cz)));
                    budget -= 1;
                    self.settle_liquids(dimension, (cx, cz), &chunks);
                    if dimension == Dimension::Overworld {
                        self.populate(dimension, (cx, cz), &chunks[&(cx, cz)]);
                    }
                }
            }
        }
    }

    fn settle_liquids(&mut self, dimension: Dimension, (cx, cz): (i32, i32), chunks: &HashMap<(i32, i32), Arc<Chunk>>) {
        let rules = liquid_rules(dimension);
        let chunk = &chunks[&(cx, cz)];
        let mut unsettled = Vec::new();
        for (i, section) in chunk.sections.iter().enumerate() {
            if !section.blocks.iter().any(|&b| blocks::is_liquid(b)) {
                continue;
            }
            for (j, &state) in section.blocks.iter().enumerate() {
                let Some((fluid, _)) = liquid::fluid(state) else { continue };
                let pos = [
                    cx * 16 + (j & 15) as i32,
                    chunk.min_y + i as i32 * 16 + (j >> 8) as i32,
                    cz * 16 + ((j >> 4) & 15) as i32,
                ];
                let mut dry = DryRun { chunks, min_y: chunk.min_y, max_y: chunk.max_y(), changed: false };
                liquid::tick(&mut dry, pos, rules);
                if dry.changed {
                    unsettled.push((pos, fluid));
                }
            }
        }
        for (pos, fluid) in unsettled {
            self.schedule_liquid(dimension, pos, fluid.delay(rules));
        }
    }

    /// Vanilla's spawnMobsForChunkGeneration for farm animals: while a 10% roll succeeds,
    /// a herd of four on grass, in biomes that have them.
    fn populate(&mut self, dimension: Dimension, (cx, cz): (i32, i32), chunk: &Chunk) {
        let server = self.server.clone();
        let biome_names = &server.level(dimension).info().biome_names;
        while self.rng.chance(0.1) {
            let (x, z) = (cx * 16 + self.rng.range(0, 16), cz * 16 + self.rng.range(0, 16));
            let Some(pos) = self.animal_spot(dimension, x, z) else { continue };
            let biome = chunk_biome(chunk, x, pos[1] as i32, z).and_then(|id| biome_names.get(id as usize));
            if !biome.is_some_and(|name| has_farm_animals(name)) {
                continue;
            }
            let kind = self.animal_kind();
            self.spawn_herd(kind, dimension, x, z);
        }
    }

    /// Spawns, moves and removes mobs on each client to match what it should see.
    fn track(&mut self) {
        let mut moved = Vec::new();
        let mut fire = Vec::new();
        for mob in self.mobs.values_mut() {
            let turned = (mob.yaw - mob.sent_yaw).abs() > 0.5
                || (mob.head_yaw - mob.sent_head_yaw).abs() > 0.5
                || (mob.pitch - mob.sent_pitch).abs() > 0.5;
            if dist_sq(mob.pos, mob.sent_pos) > 1e-6 || turned {
                mob.sent_pos = mob.pos;
                mob.sent_yaw = mob.yaw;
                mob.sent_head_yaw = mob.head_yaw;
                mob.sent_pitch = mob.pitch;
                moved.push(Event::MoveEntity {
                    id: mob.id,
                    pos: mob.pos,
                    yaw: mob.yaw,
                    head_yaw: mob.head_yaw,
                    pitch: mob.pitch,
                    on_ground: mob.on_ground,
                });
            }
            let on_fire = mob.fire > 0 && mob.alive();
            if on_fire != mob.sent_on_fire {
                mob.sent_on_fire = on_fire;
                fire.push((mob.id, on_fire));
            }
        }

        let mut sends: Vec<(Uuid, Event)> = Vec::new();
        for (uuid, p) in &mut self.players {
            let in_range: HashSet<i32> = self
                .mobs
                .values()
                .filter(|m| {
                    m.dimension == p.dimension
                        && (m.pos[0] - p.pos[0]).powi(2) + (m.pos[2] - p.pos[2]).powi(2)
                            <= TRACKING_RANGE * TRACKING_RANGE
                })
                .map(|m| m.id)
                .collect();
            if p.travelling {
                continue;
            }
            let gone: Vec<i32> = p.visible.difference(&in_range).copied().collect();
            if !gone.is_empty() {
                sends.push((*uuid, Event::RemoveEntities(gone)));
            }
            for &id in in_range.difference(&p.visible) {
                let m = &self.mobs[&id];
                sends.push((
                    *uuid,
                    Event::SpawnMob {
                        id,
                        uuid: m.uuid,
                        type_id: m.kind.type_id(),
                        pos: m.pos,
                        yaw: m.yaw,
                        head_yaw: m.head_yaw,
                        pitch: m.pitch,
                    },
                ));
                if m.sent_on_fire {
                    sends.push((*uuid, Event::EntityOnFire { id, on_fire: true }));
                }
                if m.death_time.is_some_and(|t| t < 20) {
                    sends.push((*uuid, Event::EntityStatus { id, status: 3 }));
                }
            }
            for event in &moved {
                let Event::MoveEntity { id, .. } = event else { continue };
                if p.visible.contains(id) && in_range.contains(id) {
                    sends.push((*uuid, event.clone()));
                }
            }
            for &(id, on_fire) in &fire {
                if p.visible.contains(&id) && in_range.contains(&id) {
                    sends.push((*uuid, Event::EntityOnFire { id, on_fire }));
                }
            }
            p.visible = in_range;
        }
        for (uuid, event) in sends {
            self.send(uuid, event);
        }
    }
}

fn liquid_rules(dimension: Dimension) -> liquid::Rules {
    liquid::Rules { ultrawarm: dimension == Dimension::Nether }
}

/// Tries a liquid update against a few chunks without changing anything.
struct DryRun<'a> {
    chunks: &'a HashMap<(i32, i32), Arc<Chunk>>,
    min_y: i32,
    max_y: i32,
    changed: bool,
}

impl liquid::World for DryRun<'_> {
    fn get(&self, [x, y, z]: liquid::Pos) -> Option<u16> {
        if y < self.min_y || y >= self.max_y {
            return None;
        }
        Some(self.chunks.get(&(x >> 4, z >> 4))?.block((x & 15) as usize, y, (z & 15) as usize))
    }

    fn set(&mut self, _: liquid::Pos, _: u16) {
        self.changed = true;
    }
}

/// Chunks at exactly `r` (Chebyshev distance) from (cx, cz).
fn ring_chunks(cx: i32, cz: i32, r: i32) -> Vec<(i32, i32)> {
    if r == 0 {
        return vec![(cx, cz)];
    }
    let mut out = Vec::with_capacity(8 * r as usize);
    for d in -r..=r {
        out.extend([(cx + d, cz - r), (cx + d, cz + r)]);
    }
    for d in -r + 1..r {
        out.extend([(cx - r, cz + d), (cx + r, cz + d)]);
    }
    out
}

/// The generator's biome id at a block of `chunk`.
fn chunk_biome(chunk: &Chunk, x: i32, y: i32, z: i32) -> Option<u16> {
    let dy = y - chunk.min_y;
    let section = chunk.sections.get(usize::try_from(dy).ok()? / 16)?;
    let (qx, qy, qz) = (((x & 15) >> 2) as usize, ((dy & 15) >> 2) as usize, ((z & 15) >> 2) as usize);
    Some(section.biomes[(qy * 4 + qz) * 4 + qx])
}

/// Biomes whose natural spawns include vanilla's farm animals.
fn has_farm_animals(biome: &str) -> bool {
    const NONE: &[&str] = &[
        "ocean",
        "river",
        "beach",
        "desert",
        "badlands",
        "mushroom",
        "snowy_plains",
        "ice_spikes",
        "frozen",
        "peaks",
        "slopes",
        "grove",
        "stony",
        "dripstone",
        "lush",
        "deep_dark",
        "mangrove",
    ];
    !NONE.iter().any(|n| biome.contains(n))
}

/// Whether a block that gives off light is within `r` blocks.
fn near_light(level: &Level, [x, y, z]: [i32; 3], r: i32) -> bool {
    (-r..=r).any(|dy| {
        (-r..=r).any(|dz| {
            (-r..=r).any(|dx| level.cached_block(x + dx, y + dy, z + dz).is_some_and(blocks::is_light_source))
        })
    })
}

/// Lets liquids read and write a level, remembering what changed.
struct LevelWorld<'a> {
    level: &'a Level,
    changes: Vec<portal::Edit>,
}

impl liquid::World for LevelWorld<'_> {
    fn get(&self, [x, y, z]: liquid::Pos) -> Option<u16> {
        let info = self.level.info();
        if y < info.min_y || y >= info.min_y + info.height as i32 {
            // Outside the build height counts as unloaded, so liquids leave it alone.
            return None;
        }
        self.level.cached_block(x, y, z)
    }

    fn set(&mut self, pos: liquid::Pos, state: u16) {
        self.level.set_block(pos[0], pos[1], pos[2], state);
        self.changes.push((pos, state));
    }
}

/// What mob AI knows about a player.
#[derive(Clone, Copy)]
struct Seen {
    uuid: Uuid,
    dimension: Dimension,
    pos: [f64; 3],
    /// Survival or adventure, alive and here: something to attack.
    attackable: bool,
    /// Not a spectator: something to look at.
    visible: bool,
}

impl Seen {
    fn eye(&self) -> [f64; 3] {
        [self.pos[0], self.pos[1] + 1.62, self.pos[2]]
    }
}

struct Ctx<'a> {
    level: &'a Level,
    players: &'a [Seen],
    day: bool,
    rng: &'a mut Rng,
    /// Path searches left this tick; they are the costly part of mob AI.
    searches: &'a mut u32,
}

impl Ctx<'_> {
    /// Whether the sky is right above `pos`: nothing but air over a mob standing there.
    fn under_sky(&self, [x, y, z]: path::Node) -> bool {
        self.level.cached_surface(x, z).is_some_and(|top| top <= y + 2)
    }
}

/// Decides what a mob does this tick: the horizontal step it wants to take and what it
/// looks at.
fn think(mob: &mut Mob, ctx: &mut Ctx, actions: &mut Vec<Action>) -> ([f64; 2], Option<[f64; 3]>) {
    let kind = mob.kind;
    let speed = kind.speed();
    mob.repath = mob.repath.saturating_sub(1);

    if mob.panic > 0 {
        mob.panic -= 1;
        if mob.path.is_empty() && mob.repath == 0 {
            // Run 5 to 10 blocks off, roughly away from what hurt it (vanilla's PanicGoal).
            let away = (mob.pos[2] - mob.panic_from[2]).atan2(mob.pos[0] - mob.panic_from[0]);
            let angle = away + (ctx.rng.f64() - 0.5) * 1.5;
            let r = 5.0 + ctx.rng.f64() * 5.0;
            let goal = [mob.pos[0] + angle.cos() * r, mob.pos[1], mob.pos[2] + angle.sin() * r];
            go_near(mob, ctx, goal, 4, path::WALK);
            mob.repath = 10;
        }
        return (mob.follow_path(speed * 1.6), None);
    }

    let target = pick_target(mob, ctx);
    mob.target = target.map(|t| t.uuid);
    let Some(t) = target else {
        mob.fuse = 0;
        return idle(mob, ctx);
    };

    let (dx, dz) = (t.pos[0] - mob.pos[0], t.pos[2] - mob.pos[2]);
    let horizontal = (dx * dx + dz * dz).sqrt();
    let dy = (t.pos[1] - mob.pos[1]).abs();
    let look = Some(t.eye());
    match kind {
        MobKind::Creeper => {
            if horizontal < 3.0 && dy < 2.0 {
                mob.clear_path();
                mob.fuse += 1;
                if mob.fuse >= 30 {
                    actions.push(Action::Explode { pos: mob.pos, source: mob.id });
                }
                return ([0.0, 0.0], look);
            }
            if horizontal > 7.0 {
                mob.fuse = 0;
            }
            (chase(mob, ctx, t.pos, speed), look)
        }
        MobKind::Skeleton => {
            let eye = [mob.pos[0], mob.pos[1] + kind.eye_height(), mob.pos[2]];
            let sees = line_of_sight(ctx.level, eye, t.eye());
            if sees && horizontal < 15.0 && mob.attack_cooldown == 0 {
                mob.attack_cooldown = 40;
                actions.push(Action::Swing { id: mob.id });
                // No arrow entity yet: the shot lands or misses at once.
                if ctx.rng.chance(0.7) {
                    let damage = ctx.rng.range(2, 5) as f32;
                    actions.push(Action::HurtPlayer {
                        uuid: t.uuid,
                        amount: damage,
                        from: mob.pos,
                        cause: "was shot by Skeleton".into(),
                    });
                }
            }
            let walk = if !sees || horizontal > 12.0 {
                chase(mob, ctx, t.pos, speed)
            } else if horizontal < 5.0 {
                // Back off, straight away from the target.
                mob.clear_path();
                [-dx / horizontal * speed, -dz / horizontal * speed]
            } else {
                mob.clear_path();
                [0.0, 0.0]
            };
            (walk, look)
        }
        _ => {
            if horizontal < 1.6 && dy < 1.5 {
                mob.clear_path();
                if mob.attack_cooldown == 0 {
                    mob.attack_cooldown = 20;
                    actions.push(Action::Swing { id: mob.id });
                    let cause = format!("was slain by {}", kind.display());
                    actions.push(Action::HurtPlayer {
                        uuid: t.uuid,
                        amount: kind.attack_damage(),
                        from: mob.pos,
                        cause,
                    });
                }
                return ([0.0, 0.0], look);
            }
            (chase(mob, ctx, t.pos, speed), look)
        }
    }
}

fn pick_target(mob: &mut Mob, ctx: &Ctx) -> Option<Seen> {
    const FOLLOW: f64 = 16.0;
    let current = mob.target.and_then(|t| ctx.players.iter().find(|p| p.uuid == t)).copied();
    match mob.kind.temper() {
        Temper::Passive => None,
        Temper::Neutral => {
            mob.anger = mob.anger.saturating_sub(1);
            current.filter(|t| {
                mob.anger > 0 && t.attackable && t.dimension == mob.dimension && dist_sq(t.pos, mob.pos) < 40.0 * 40.0
            })
        }
        Temper::Hostile => current
            .filter(|t| t.attackable && t.dimension == mob.dimension && dist_sq(t.pos, mob.pos) < FOLLOW * FOLLOW * 1.5)
            .or_else(|| {
                // A new target has to be in sight, as for vanilla's NearestAttackableTargetGoal.
                let eye = [mob.pos[0], mob.pos[1] + mob.kind.eye_height(), mob.pos[2]];
                ctx.players
                    .iter()
                    .filter(|p| {
                        p.attackable && p.dimension == mob.dimension && dist_sq(p.pos, mob.pos) < FOLLOW * FOLLOW
                    })
                    .filter(|p| line_of_sight(ctx.level, eye, p.eye()))
                    .min_by(|a, b| dist_sq(a.pos, mob.pos).total_cmp(&dist_sq(b.pos, mob.pos)))
                    .copied()
            }),
    }
}

/// Walks after `target`, finding the way again when it has moved off the path's end.
fn chase(mob: &mut Mob, ctx: &mut Ctx, target: [f64; 3], speed: f64) -> [f64; 2] {
    let goal = [target[0].floor() as i32, (target[1] + 1e-6).floor() as i32, target[2].floor() as i32];
    let moved = mob.path_goal.is_none_or(|g| (0..3).any(|i| (g[i] - goal[i]).abs() > 1));
    if (moved || mob.path.is_empty()) && mob.repath == 0 {
        search(mob, ctx, goal, path::WALK, 400);
        mob.repath = 10;
    }
    let walk = mob.follow_path(speed);
    if walk == [0.0, 0.0] {
        // The last step or two, where the path ends at the target's block.
        let (dx, dz) = (target[0] - mob.pos[0], target[2] - mob.pos[2]);
        let d = (dx * dx + dz * dz).sqrt();
        if d > 0.3 && d < 2.5 {
            return [dx / d * speed.min(d), dz / d * speed.min(d)];
        }
    }
    walk
}

fn search(mob: &mut Mob, ctx: &mut Ctx, goal: path::Node, rules: path::Rules, max_nodes: usize) -> bool {
    if *ctx.searches == 0 {
        return false;
    }
    *ctx.searches -= 1;
    let level = ctx.level;
    let block = |x: i32, y: i32, z: i32| level.cached_block(x, y, z);
    mob.clear_path();
    match path::find(&block, rules, mob.block_pos(), goal, max_nodes) {
        Some(found) => {
            mob.path = found;
            mob.path_goal = Some(goal);
            true
        }
        None => false,
    }
}

/// Finds ground within `range` blocks up or down of `goal` and a path to it.
fn go_near(mob: &mut Mob, ctx: &mut Ctx, goal: [f64; 3], range: i32, rules: path::Rules) -> bool {
    let (x, y, z) = (goal[0].floor() as i32, goal[1].floor() as i32, goal[2].floor() as i32);
    let level = ctx.level;
    let block = |x: i32, y: i32, z: i32| level.cached_block(x, y, z);
    match path::ground_near(&block, x, y, z, range) {
        Some(y) => search(mob, ctx, [x, y, z], rules, 200),
        None => false,
    }
}

/// No target: seek shade, stroll now and then, and look about.
fn idle(mob: &mut Mob, ctx: &mut Ctx) -> ([f64; 2], Option<[f64; 3]>) {
    let speed = mob.kind.speed();
    let here = mob.block_pos();
    // Skeletons burning in the sun head for shade (vanilla's FleeSunGoal; zombies don't).
    let fleeing_sun = mob.kind == MobKind::Skeleton
        && ctx.day
        && mob.fire > 0
        && mob.dimension == Dimension::Overworld
        && ctx.under_sky(here);
    if fleeing_sun && mob.path.is_empty() && mob.repath == 0 {
        mob.repath = 20;
        for _ in 0..10 {
            let [x, y, z] =
                [here[0] + ctx.rng.range(-10, 11), here[1] + ctx.rng.range(-3, 4), here[2] + ctx.rng.range(-10, 11)];
            let level = ctx.level;
            let block = |x: i32, y: i32, z: i32| level.cached_block(x, y, z);
            if let Some(y) = path::ground_near(&block, x, y, z, 3)
                && !ctx.under_sky([x, y, z])
            {
                search(mob, ctx, [x, y, z], path::WALK, 300);
                break;
            }
        }
    }
    // Strolls start about once every six seconds (vanilla's RandomStrollGoal), up to
    // 10 blocks away and 7 up or down, staying out of water. By day skeletons only
    // stroll to shade (vanilla's RestrictSunGoal), so they don't walk back into the sun.
    let shade_only = mob.kind == MobKind::Skeleton && ctx.day && mob.dimension == Dimension::Overworld;
    if mob.path.is_empty() && !fleeing_sun && ctx.rng.chance(1.0 / 120.0) {
        let rules = if mob.kind.temper() == Temper::Passive {
            path::CAREFUL
        } else {
            path::Rules { water_cost: None, max_drop: 3 }
        };
        for _ in 0..if shade_only { 5 } else { 1 } {
            let goal = [
                mob.pos[0] + ctx.rng.range(-10, 11) as f64,
                mob.pos[1] + ctx.rng.range(-3, 4) as f64,
                mob.pos[2] + ctx.rng.range(-10, 11) as f64,
            ];
            let level = ctx.level;
            let block = |x: i32, y: i32, z: i32| level.cached_block(x, y, z);
            let (x, z) = (goal[0].floor() as i32, goal[2].floor() as i32);
            let Some(y) = path::ground_near(&block, x, goal[1].floor() as i32, z, 7) else { continue };
            if shade_only && ctx.under_sky([x, y, z]) {
                continue;
            }
            search(mob, ctx, [x, y, z], rules, 200);
            break;
        }
    }
    let walk = mob.follow_path(if fleeing_sun { speed } else { speed * 0.8 });

    // Looking about: at a nearby player now and then, otherwise somewhere random
    // (vanilla's LookAtPlayerGoal and RandomLookAroundGoal).
    if let Some((_, ticks)) = &mut mob.look {
        *ticks = ticks.saturating_sub(1);
        if *ticks == 0 {
            mob.look = None;
        }
    }
    if mob.look.is_none() && ctx.rng.chance(0.02) {
        let nearby = ctx
            .players
            .iter()
            .filter(|p| p.visible && p.dimension == mob.dimension && dist_sq(p.pos, mob.pos) < 8.0 * 8.0)
            .min_by(|a, b| dist_sq(a.pos, mob.pos).total_cmp(&dist_sq(b.pos, mob.pos)));
        mob.look = match nearby {
            Some(p) => Some((Look::Player(p.uuid), ctx.rng.range(40, 80) as u32)),
            None if walk == [0.0, 0.0] => {
                Some((Look::Direction(mob.yaw + ctx.rng.range(-120, 121) as f32), ctx.rng.range(20, 40) as u32))
            }
            None => None,
        };
    }
    let eye = [mob.pos[0], mob.pos[1] + mob.kind.eye_height(), mob.pos[2]];
    let look_at = match mob.look {
        Some((Look::Player(uuid), _)) => {
            let player = ctx.players.iter().find(|p| p.uuid == uuid && p.visible && p.dimension == mob.dimension);
            match player.filter(|p| dist_sq(p.pos, mob.pos) < 10.0 * 10.0) {
                Some(p) => Some(p.eye()),
                None => {
                    mob.look = None;
                    None
                }
            }
        }
        Some((Look::Direction(yaw), _)) => {
            let rad = (yaw as f64).to_radians();
            Some([eye[0] - rad.sin() * 5.0, eye[1], eye[2] + rad.cos() * 5.0])
        }
        None => None,
    };
    (walk, look_at)
}

fn line_of_sight(level: &Level, from: [f64; 3], to: [f64; 3]) -> bool {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let len = dist_sq(from, to).sqrt();
    let steps = (len * 2.0).ceil() as i32;
    (1..steps).all(|i| {
        let t = i as f64 / steps as f64;
        let p = [from[0] + d[0] * t, from[1] + d[1] * t, from[2] + d[2] * t];
        level
            .cached_block(p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32)
            .is_some_and(|b| !blocks::is_solid(b))
    })
}

/// Vanilla's damage for a power-3 explosion with nothing in the way.
fn explosion_damage(center: [f64; 3], pos: [f64; 3]) -> Option<f32> {
    let radius = 6.0;
    let d = dist_sq(center, pos).sqrt();
    if d >= radius {
        return None;
    }
    let impact = 1.0 - d / radius;
    Some(((impact * impact + impact) / 2.0 * 7.0 * radius + 1.0) as f32)
}

fn dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_and_night() {
        assert!(is_day(6000) && is_day(24000 + 1000) && is_day(23500));
        assert!(!is_day(13000) && !is_day(18000) && !is_day(24000 + 18000));
    }

    #[test]
    fn explosions_fade_with_distance() {
        let at = |d: f64| explosion_damage([0.0; 3], [d, 0.0, 0.0]);
        assert_eq!(at(0.0), Some(43.0));
        assert!(at(3.0).unwrap() < at(1.0).unwrap());
        assert_eq!(at(6.0), None);
    }
}
