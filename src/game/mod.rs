// The server's 20 tick-per-second simulation: world time, mobs (spawning, AI, combat,
// despawning), player health, and nether portal travel. Connections feed it `Input`s
// and it answers through each player's event channel.

pub mod mob;
pub mod portal;

use crate::{
    players::{Color, Event},
    server::{GameMode, Server},
    world::{Dimension, Level, blocks},
};
use mob::{Mob, MobKind, Rng, Temper};
use portal::{Portal, PortalKey};
use std::{
    collections::{HashMap, HashSet},
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
const HOSTILE_CAP: usize = 15;
const PASSIVE_CAP: usize = 8;

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

    fn apply_edits(&self, dimension: Dimension, edits: &[portal::Edit]) {
        let level = self.server.level(dimension);
        for &([x, y, z], state) in edits {
            level.set_block(x, y, z, state);
        }
        for (uuid, p) in &self.players {
            if p.dimension == dimension {
                for &(pos, state) in edits {
                    self.send(*uuid, Event::BlockChanged { pos, state });
                }
            }
        }
    }

    fn tick(&mut self) {
        self.ticks += 1;
        let time = self.server.time.fetch_add(1, Ordering::Relaxed) + 1;
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
            let in_portal = [y, y + 1].iter().any(|&y| level.cached_block(x, y, z).is_some_and(blocks::is_portal));
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
        let targets: Vec<(Uuid, Dimension, [f64; 3], bool)> = self
            .players
            .iter()
            .map(|(uuid, p)| (*uuid, p.dimension, p.pos, !p.dead && !p.travelling && self.takes_damage(*uuid)))
            .collect();

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

            let walk = think(mob, &targets, level, &mut self.rng, &mut actions);
            let cautious = mob.target.is_none() && mob.panic == 0;
            if !mob.step(walk, cautious, &block) {
                mob.wander = None;
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
            let kind = match dimension {
                Dimension::Nether if hostile < HOSTILE_CAP => Some(MobKind::ZombifiedPiglin),
                Dimension::Overworld if !day && hostile < HOSTILE_CAP => Some(self.hostile_kind()),
                Dimension::Overworld if passive < PASSIVE_CAP && self.rng.chance(0.1) => Some(self.passive_kind()),
                _ => None,
            };
            let Some(kind) = kind else { continue };
            let Some(pos) = self.find_spawn(dimension, center, kind) else { continue };
            let group = if kind.temper() == Temper::Passive { self.rng.range(2, 4) } else { 1 };
            for i in 0..group {
                let offset = [pos[0] + i as f64 * 0.7, pos[1], pos[2] + (i % 2) as f64 * 0.7];
                let yaw = self.rng.f64() as f32 * 360.0;
                let mob = Mob::new(self.new_entity_id(), kind, dimension, offset, yaw);
                debug!("spawned {} at {:.1} {:.1} {:.1}", kind.name(), offset[0], offset[1], offset[2]);
                self.mobs.insert(mob.id, mob);
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

    fn passive_kind(&mut self) -> MobKind {
        [MobKind::Pig, MobKind::Cow, MobKind::Sheep, MobKind::Chicken][self.rng.range(0, 4) as usize]
    }

    fn find_spawn(&mut self, dimension: Dimension, center: [f64; 3], kind: MobKind) -> Option<[f64; 3]> {
        let level = self.server.level(dimension);
        let info = level.info();
        for _ in 0..4 {
            let angle = self.rng.f64() * std::f64::consts::TAU;
            let r = 24.0 + self.rng.f64() * 24.0;
            let (x, z) = ((center[0] + angle.cos() * r).floor() as i32, (center[2] + angle.sin() * r).floor() as i32);
            let pos_ok = |y: i32| {
                let ground = level.cached_block(x, y - 1, z)?;
                let ok = blocks::is_solid(ground)
                    && !blocks::is_liquid(ground)
                    && (kind.temper() != Temper::Passive || ground == blocks::GRASS_BLOCK)
                    && [y, y + 1].iter().all(|&y| {
                        level.cached_block(x, y, z).is_some_and(|b| !blocks::is_solid(b) && !blocks::is_liquid(b))
                    });
                Some(ok)
            };
            let y = match dimension {
                Dimension::Overworld => {
                    let y = level.cached_surface(x, z)?;
                    pos_ok(y)?.then_some(y)
                }
                // Caves everywhere: look down from a random height for a floor.
                Dimension::Nether => {
                    let start = self.rng.range(info.min_y + 32, info.min_y + 120);
                    (start - 24..=start).rev().find(|&y| pos_ok(y) == Some(true))
                }
            };
            let Some(y) = y else { continue };
            let pos = [x as f64 + 0.5, y as f64, z as f64 + 0.5];
            let crowded = self.players.values().any(|p| p.dimension == dimension && dist_sq(p.pos, pos) < 24.0 * 24.0);
            if !crowded {
                return Some(pos);
            }
        }
        None
    }

    fn despawn_mobs(&mut self) {
        let mut gone = Vec::new();
        for mob in self.mobs.values() {
            if mob.persistent || !mob.alive() {
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
            let random = mob.kind.temper() != Temper::Passive && nearest > 32.0 * 32.0 && mob.age > 600;
            if far || (random && self.rng.chance(1.0 / 40.0)) {
                gone.push(mob.id);
            }
        }
        self.remove_mobs(&gone);
    }

    /// Spawns, moves and removes mobs on each client to match what it should see.
    fn track(&mut self) {
        let mut moved = Vec::new();
        let mut fire = Vec::new();
        for mob in self.mobs.values_mut() {
            if dist_sq(mob.pos, mob.sent_pos) > 1e-6 || (mob.yaw - mob.sent_yaw).abs() > 0.5 {
                mob.sent_pos = mob.pos;
                mob.sent_yaw = mob.yaw;
                moved.push((mob.id, mob.pos, mob.yaw, mob.on_ground));
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
                    Event::SpawnMob { id, uuid: m.uuid, type_id: m.kind.type_id(), pos: m.pos, yaw: m.yaw },
                ));
                if m.sent_on_fire {
                    sends.push((*uuid, Event::EntityOnFire { id, on_fire: true }));
                }
                if m.death_time.is_some_and(|t| t < 20) {
                    sends.push((*uuid, Event::EntityStatus { id, status: 3 }));
                }
            }
            for &(id, pos, yaw, on_ground) in &moved {
                if p.visible.contains(&id) && in_range.contains(&id) {
                    sends.push((*uuid, Event::MoveEntity { id, pos, yaw, on_ground }));
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

/// Decides what a mob does this tick; returns the horizontal step it wants to take.
fn think(
    mob: &mut Mob,
    players: &[(Uuid, Dimension, [f64; 3], bool)],
    level: &Level,
    rng: &mut Rng,
    actions: &mut Vec<Action>,
) -> [f64; 2] {
    let kind = mob.kind;
    let speed = kind.speed();

    if mob.panic > 0 {
        mob.panic -= 1;
        let (dx, dz) = (mob.pos[0] - mob.panic_from[0], mob.pos[2] - mob.panic_from[2]);
        return toward(mob, dx, dz, speed * 1.6);
    }

    // Pick or drop a target.
    let follow = 16.0;
    let target = mob.target.and_then(|t| players.iter().find(|p| p.0 == t)).copied();
    let target = match kind.temper() {
        Temper::Passive => None,
        Temper::Neutral => {
            mob.anger = mob.anger.saturating_sub(1);
            target.filter(|t| mob.anger > 0 && t.3 && t.1 == mob.dimension && dist_sq(t.2, mob.pos) < 40.0 * 40.0)
        }
        Temper::Hostile => target
            .filter(|t| t.3 && t.1 == mob.dimension && dist_sq(t.2, mob.pos) < follow * follow * 1.5)
            .or_else(|| {
                players
                    .iter()
                    .filter(|p| p.3 && p.1 == mob.dimension && dist_sq(p.2, mob.pos) < follow * follow)
                    .min_by(|a, b| dist_sq(a.2, mob.pos).total_cmp(&dist_sq(b.2, mob.pos)))
                    .copied()
            }),
    };
    mob.target = target.map(|t| t.0);

    let Some((uuid, _, tpos, _)) = target else {
        mob.fuse = 0;
        return wander(mob, rng, speed);
    };
    let (dx, dz) = (tpos[0] - mob.pos[0], tpos[2] - mob.pos[2]);
    let horizontal = (dx * dx + dz * dz).sqrt();
    let dy = (tpos[1] - mob.pos[1]).abs();
    let eye = [mob.pos[0], mob.pos[1] + 1.5, mob.pos[2]];
    let target_eye = [tpos[0], tpos[1] + 1.5, tpos[2]];

    match kind {
        MobKind::Creeper => {
            mob.face(dx, dz);
            if horizontal < 3.0 && dy < 2.0 {
                mob.fuse += 1;
                if mob.fuse >= 30 {
                    actions.push(Action::Explode { pos: mob.pos, source: mob.id });
                }
                return [0.0, 0.0];
            }
            if horizontal > 7.0 {
                mob.fuse = 0;
            }
            toward(mob, dx, dz, speed)
        }
        MobKind::Skeleton => {
            mob.face(dx, dz);
            let sees = line_of_sight(level, eye, target_eye);
            if sees && horizontal < 15.0 && mob.attack_cooldown == 0 {
                mob.attack_cooldown = 40;
                actions.push(Action::Swing { id: mob.id });
                // No arrow entity yet: the shot lands or misses at once.
                if rng.chance(0.7) {
                    let damage = rng.range(2, 5) as f32;
                    actions.push(Action::HurtPlayer {
                        uuid,
                        amount: damage,
                        from: mob.pos,
                        cause: "was shot by Skeleton".into(),
                    });
                }
            }
            if !sees || horizontal > 12.0 {
                toward(mob, dx, dz, speed)
            } else if horizontal < 5.0 {
                let walk = toward(mob, -dx, -dz, speed);
                mob.face(dx, dz);
                walk
            } else {
                [0.0, 0.0]
            }
        }
        _ => {
            if horizontal < 1.6 && dy < 1.5 {
                mob.face(dx, dz);
                if mob.attack_cooldown == 0 {
                    mob.attack_cooldown = 20;
                    actions.push(Action::Swing { id: mob.id });
                    let cause = format!("was slain by {}", kind.display());
                    actions.push(Action::HurtPlayer { uuid, amount: kind.attack_damage(), from: mob.pos, cause });
                }
                return [0.0, 0.0];
            }
            toward(mob, dx, dz, speed)
        }
    }
}

fn toward(mob: &mut Mob, dx: f64, dz: f64, speed: f64) -> [f64; 2] {
    let len = (dx * dx + dz * dz).sqrt();
    if len < 1e-3 {
        return [0.0, 0.0];
    }
    mob.face(dx, dz);
    let step = speed.min(len);
    [dx / len * step, dz / len * step]
}

fn wander(mob: &mut Mob, rng: &mut Rng, speed: f64) -> [f64; 2] {
    if mob.wander.is_none() && rng.chance(1.0 / 120.0) {
        let angle = rng.f64() * std::f64::consts::TAU;
        let r = 3.0 + rng.f64() * 7.0;
        mob.wander = Some([mob.pos[0] + angle.cos() * r, mob.pos[2] + angle.sin() * r]);
        mob.wander_ticks = 0;
    }
    let Some([wx, wz]) = mob.wander else { return [0.0, 0.0] };
    mob.wander_ticks += 1;
    let (dx, dz) = (wx - mob.pos[0], wz - mob.pos[2]);
    if dx * dx + dz * dz < 0.25 || mob.wander_ticks > 200 {
        mob.wander = None;
        return [0.0, 0.0];
    }
    toward(mob, dx, dz, speed * 0.8)
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
