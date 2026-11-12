// Commands, the same for the console, Java players and Bedrock players.

use crate::{
    access::Change,
    game::{Input, mob::MobKind},
    java::offline_uuid,
    players::{Color, Event},
    server::{GameMode, Server},
    wgen,
    world::{Dimension, blocks},
};
use std::sync::atomic::Ordering;
use uuid::Uuid;

pub enum Source {
    Console,
    Player { name: String, uuid: Uuid, level: u8 },
}

impl Source {
    fn level(&self) -> u8 {
        match self {
            Source::Console => 4,
            Source::Player { level, .. } => *level,
        }
    }

    fn name(&self) -> &str {
        match self {
            Source::Console => "Server",
            Source::Player { name, .. } => name,
        }
    }
}

pub struct Spec {
    pub name: &'static str,
    pub level: u8,
    pub usage: &'static str,
}

pub const COMMANDS: &[Spec] = &[
    Spec { name: "help", level: 0, usage: "/help" },
    Spec { name: "list", level: 0, usage: "/list" },
    Spec { name: "gamemode", level: 2, usage: "/gamemode <survival|creative|adventure|spectator> [player]" },
    Spec { name: "op", level: 3, usage: "/op <player>" },
    Spec { name: "deop", level: 3, usage: "/deop <player>" },
    Spec { name: "kick", level: 3, usage: "/kick <player> [reason]" },
    Spec { name: "whitelist", level: 3, usage: "/whitelist <on|off|list|add|remove|reload> [player]" },
    Spec { name: "summon", level: 2, usage: "/summon <mob>" },
    Spec { name: "portal", level: 2, usage: "/portal" },
    Spec { name: "time", level: 2, usage: "/time <set <day|noon|night|midnight|ticks>|query>" },
    Spec { name: "setblock", level: 2, usage: "/setblock <x> <y> <z> <block>" },
    Spec { name: "fill", level: 2, usage: "/fill <x1> <y1> <z1> <x2> <y2> <z2> <block>" },
    Spec { name: "wgen", level: 3, usage: "/wgen <radius> [<x> <z>] [cps] [overworld|nether] | stop | status" },
    Spec { name: "stop", level: 4, usage: "/stop" },
];

pub type Output = Vec<(String, Color)>;

pub fn execute(server: &Server, source: &Source, line: &str) -> Output {
    let line = line.trim().trim_start_matches('/');
    let mut args = line.split_whitespace();
    let Some(cmd) = args.next() else { return Vec::new() };
    let cmd = cmd.to_ascii_lowercase();
    let args: Vec<&str> = args.collect();

    let Some(spec) = COMMANDS.iter().find(|s| s.name == cmd) else {
        return err(format!("Unknown command: {cmd}. Type /help for a list."));
    };
    if source.level() < spec.level {
        return err("You don't have permission to use this command.".into());
    }
    let usage = || err(format!("Usage: {}", spec.usage));

    match (cmd.as_str(), args.as_slice()) {
        ("help", _) => {
            COMMANDS.iter().filter(|s| source.level() >= s.level).map(|s| (s.usage.to_owned(), Color::Gray)).collect()
        }
        ("list", _) => {
            let names: Vec<String> = server.players.all().iter().map(|p| p.name.clone()).collect();
            ok(format!(
                "There are {} of a max of {} players online: {}",
                names.len(),
                server.config.max_players,
                names.join(", ")
            ))
        }
        ("gamemode", [mode, rest @ ..]) if rest.len() <= 1 => {
            let Some(mode) = GameMode::parse(mode) else { return usage() };
            let target = match (rest.first(), source) {
                (Some(name), _) => server.players.find(name),
                (None, Source::Player { uuid, .. }) => server.players.all().into_iter().find(|p| p.uuid == *uuid),
                (None, Source::Console) => return err("Name a player when running this from the console.".into()),
            };
            let Some(target) = target else { return err("That player isn't online.".into()) };
            target.send(Event::GameMode(mode));
            let mode_name = mode.display();
            if matches!(source, Source::Player { uuid, .. } if *uuid == target.uuid) {
                ok(format!("Set own game mode to {mode_name} Mode"))
            } else {
                target
                    .send(Event::Message(format!("Your game mode has been updated to {mode_name} Mode"), Color::Gray));
                ok(format!("Set {}'s game mode to {mode_name} Mode", target.name))
            }
        }
        ("op", [name]) => {
            let (uuid, name) = identify(server, name);
            match server.access.op(uuid, &name) {
                Ok(Change::Done) => {
                    refresh_permission(server, &name, uuid);
                    ok(format!("Made {name} a server operator"))
                }
                Ok(Change::Unchanged) => err("Nothing changed. The player already is an operator".into()),
                Err(e) => err(format!("Couldn't save ops.json: {e:#}")),
            }
        }
        ("deop", [name]) => {
            let (uuid, name) = identify(server, name);
            match server.access.deop(uuid, &name) {
                Ok(Change::Done) => {
                    refresh_permission(server, &name, uuid);
                    ok(format!("Made {name} no longer a server operator"))
                }
                Ok(Change::Unchanged) => err("Nothing changed. The player is not an operator".into()),
                Err(e) => err(format!("Couldn't save ops.json: {e:#}")),
            }
        }
        ("kick", [name, reason @ ..]) => {
            let Some(target) = server.players.find(name) else { return err("That player isn't online.".into()) };
            let reason = if reason.is_empty() { "Kicked by an operator".to_owned() } else { reason.join(" ") };
            target.send(Event::Kick(reason.clone()));
            ok(format!("Kicked {}: {reason}", target.name))
        }
        ("whitelist", ["on"]) => {
            server.access.set_whitelist_on(true);
            save_whitelist_setting(server, true);
            enforce_whitelist(server);
            ok("Whitelist is now turned on".into())
        }
        ("whitelist", ["off"]) => {
            server.access.set_whitelist_on(false);
            save_whitelist_setting(server, false);
            ok("Whitelist is now turned off".into())
        }
        ("whitelist", ["list"]) => {
            let names = server.access.whitelist_names();
            if names.is_empty() {
                ok("There are no whitelisted players".into())
            } else {
                ok(format!("There are {} whitelisted player(s): {}", names.len(), names.join(", ")))
            }
        }
        ("whitelist", ["add", name]) => {
            let (uuid, name) = identify(server, name);
            match server.access.whitelist_add(uuid, &name) {
                Ok(Change::Done) => ok(format!("Added {name} to the whitelist")),
                Ok(Change::Unchanged) => err("Player is already whitelisted".into()),
                Err(e) => err(format!("Couldn't save whitelist.json: {e:#}")),
            }
        }
        ("whitelist", ["remove", name]) => {
            let (uuid, name) = identify(server, name);
            match server.access.whitelist_remove(uuid, &name) {
                Ok(Change::Done) => {
                    enforce_whitelist(server);
                    ok(format!("Removed {name} from the whitelist"))
                }
                Ok(Change::Unchanged) => err("Player is not whitelisted".into()),
                Err(e) => err(format!("Couldn't save whitelist.json: {e:#}")),
            }
        }
        ("whitelist", ["reload"]) => match server.access.reload() {
            Ok(()) => {
                enforce_whitelist(server);
                for p in server.players.all() {
                    p.send(Event::Permission(server.access.op_level(p.uuid, &p.name)));
                }
                ok("Reloaded the whitelist".into())
            }
            Err(e) => err(format!("{e:#}")),
        },
        ("summon", [kind]) => {
            let Some(kind) = MobKind::parse(kind) else {
                let names: Vec<&str> = MobKind::ALL.iter().map(|k| k.name()).collect();
                return err(format!("Unknown mob. Choose one of: {}", names.join(", ")));
            };
            let Source::Player { uuid, .. } = source else { return err("Only players can summon mobs.".into()) };
            server.game.send(Input::Summon { uuid: *uuid, kind });
            ok(format!("Summoned new {}", kind.display()))
        }
        ("portal", []) => {
            let Source::Player { uuid, .. } = source else { return err("Only players can build portals.".into()) };
            server.game.send(Input::BuildPortal { uuid: *uuid });
            ok("Built a nether portal in front of you".into())
        }
        ("setblock", [x, y, z, block]) => {
            let Some((dimension, here)) = here(server, source) else { return usage() };
            let (Some(pos), Some(state)) = (block_pos(here, [x, y, z]), block_state(block)) else { return usage() };
            if let Err(e) = check_loaded(server, dimension, pos, pos) {
                return err(e);
            }
            server.game.send(Input::SetBlocks { dimension, blocks: vec![(pos, state)] });
            ok(format!("Changed the block at {} {} {}", pos[0], pos[1], pos[2]))
        }
        ("fill", [x1, y1, z1, x2, y2, z2, block]) => {
            let Some((dimension, here)) = here(server, source) else { return usage() };
            let (Some(a), Some(b), Some(state)) =
                (block_pos(here, [x1, y1, z1]), block_pos(here, [x2, y2, z2]), block_state(block))
            else {
                return usage();
            };
            let lo = [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])];
            let hi = [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])];
            let volume = (0..3).map(|i| (hi[i] - lo[i] + 1) as i64).product::<i64>();
            if volume > MAX_FILL {
                return err(format!("Too many blocks in the specified area (maximum {MAX_FILL}, specified {volume})"));
            }
            if let Err(e) = check_loaded(server, dimension, lo, hi) {
                return err(e);
            }
            let mut blocks = Vec::with_capacity(volume as usize);
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    for x in lo[0]..=hi[0] {
                        blocks.push(([x, y, z], state));
                    }
                }
            }
            server.game.send(Input::SetBlocks { dimension, blocks });
            ok(format!("Successfully filled {volume} block(s)"))
        }
        ("time", ["query"]) => {
            let time = server.time.load(Ordering::Relaxed);
            ok(format!("The time is {}", time.rem_euclid(24000)))
        }
        ("time", ["set", value]) => {
            let day_time = match *value {
                "day" => 1000,
                "noon" => 6000,
                "night" => 13000,
                "midnight" => 18000,
                n => match n.parse::<i64>() {
                    Ok(n) if n >= 0 => n,
                    _ => return usage(),
                },
            };
            // Keep the day count, like vanilla's /time set.
            let now = server.time.load(Ordering::Relaxed);
            server.game.send(Input::SetTime(now - now.rem_euclid(24000) + day_time));
            ok(format!("Set the time to {day_time}"))
        }
        ("wgen", ["stop"]) => {
            if server.wgen.stop() {
                ok("Stopping world generation".into())
            } else {
                err("No world generation is running.".into())
            }
        }
        ("wgen", ["status"]) => match server.wgen.status() {
            Some(p) => ok(format!(
                "Generating {} around chunk {} {} (radius {}, {} chunks/s max): {}",
                p.dimension.name(),
                p.center.0,
                p.center.1,
                p.radius,
                p.cps,
                p.describe()
            )),
            None => ok("No world generation is running.".into()),
        },
        ("wgen", args) if !args.is_empty() => wgen(server, source, args).unwrap_or_else(usage),
        ("stop", []) => {
            server.players.broadcast("Server closed", Color::Yellow);
            for p in server.players.all() {
                p.send(Event::Kick("Server closed".into()));
            }
            tracing::info!("{} stopped the server", source.name());
            server.shutdown.notify_one();
            ok("Stopping the server".into())
        }
        _ => usage(),
    }
}

/// Vanilla's /fill limit.
const MAX_FILL: i64 = 32768;

/// Blocks /setblock and /fill know by name (default states).
const BLOCK_NAMES: &[(&str, u16)] = &[
    ("air", blocks::AIR),
    ("water", blocks::WATER),
    ("lava", blocks::LAVA),
    ("stone", blocks::STONE),
    ("cobblestone", blocks::COBBLESTONE),
    ("dirt", blocks::DIRT),
    ("grass_block", blocks::GRASS_BLOCK),
    ("sand", blocks::SAND),
    ("glass", blocks::GLASS),
    ("oak_planks", blocks::OAK_PLANKS),
    ("obsidian", blocks::OBSIDIAN),
    ("netherrack", blocks::NETHERRACK),
    ("bedrock", blocks::BEDROCK),
];

fn block_state(name: &str) -> Option<u16> {
    let name = name.to_ascii_lowercase();
    let name = name.trim_start_matches("minecraft:");
    BLOCK_NAMES.iter().find(|(n, _)| *n == name).map(|&(_, s)| s)
}

/// Where relative coordinates start from: the player, or spawn for the console.
fn here(server: &Server, source: &Source) -> Option<(Dimension, [f64; 3])> {
    match source {
        Source::Player { uuid, .. } => server.players.get(*uuid).map(|p| p.location()),
        Source::Console => Some((Dimension::Overworld, server.spawn)),
    }
}

/// Block coordinates, each absolute or `~`/`~n` relative to `origin`.
fn block_pos(origin: [f64; 3], args: [&str; 3]) -> Option<[i32; 3]> {
    let mut pos = [0; 3];
    for i in 0..3 {
        let value = match args[i].strip_prefix('~') {
            Some("") => origin[i],
            Some(offset) => origin[i] + offset.parse::<f64>().ok()?,
            None => args[i].parse::<f64>().ok()?,
        };
        if !value.is_finite() || value.abs() > 30_000_000.0 {
            return None;
        }
        pos[i] = value.floor() as i32;
    }
    Some(pos)
}

fn check_loaded(server: &Server, dimension: Dimension, lo: [i32; 3], hi: [i32; 3]) -> Result<(), String> {
    let level = server.level(dimension);
    let info = level.info();
    if lo[1] < info.min_y || hi[1] >= info.min_y + info.height as i32 {
        return Err("Cannot place blocks outside of the world".into());
    }
    for cx in lo[0] >> 4..=hi[0] >> 4 {
        for cz in lo[2] >> 4..=hi[2] >> 4 {
            if level.cached_block(cx << 4, lo[1], cz << 4).is_none() {
                return Err("That position is not loaded".into());
            }
        }
    }
    Ok(())
}

/// `/wgen <radius> [<x> <z>] [cps] [overworld|nether]`; `None` means bad syntax.
fn wgen(server: &Server, source: &Source, args: &[&str]) -> Option<Output> {
    let player = match source {
        Source::Player { uuid, .. } => server.players.get(*uuid),
        Source::Console => None,
    };
    let (here_dimension, here) = player.as_ref().map_or((Dimension::Overworld, server.spawn), |p| p.location());

    let (dimension, args) = match args.split_last() {
        Some((last, rest)) => match *last {
            "overworld" | "minecraft:overworld" => (Dimension::Overworld, rest),
            "nether" | "the_nether" | "minecraft:the_nether" => (Dimension::Nether, rest),
            _ => (here_dimension, args),
        },
        None => return None,
    };
    let numbers: Vec<i64> = args.iter().map(|a| a.parse().ok()).collect::<Option<_>>()?;
    let (radius, block, cps) = match numbers[..] {
        [r] => (r, None, None),
        [r, cps] => (r, None, Some(cps)),
        [r, x, z] => (r, Some((x, z)), None),
        [r, x, z, cps] => (r, Some((x, z)), Some(cps)),
        _ => return None,
    };
    if !(0..=wgen::MAX_RADIUS as i64).contains(&radius) {
        return Some(err(format!("The radius must be between 0 and {} chunks.", wgen::MAX_RADIUS)));
    }
    let cps = cps.unwrap_or(wgen::DEFAULT_CPS as i64);
    if !(1..=wgen::MAX_CPS as i64).contains(&cps) {
        return Some(err(format!("Chunks per second must be between 1 and {}.", wgen::MAX_CPS)));
    }
    let (bx, bz) = match block {
        Some((x, z)) if x.abs() > 29_999_984 || z.abs() > 29_999_984 => {
            return Some(err("Those coordinates are outside the world.".into()));
        }
        Some((x, z)) => (x as i32, z as i32),
        // A player's own position, otherwise the world spawn.
        None => (here[0].floor() as i32, here[2].floor() as i32),
    };

    let request = wgen::Request {
        dimension,
        center: (bx >> 4, bz >> 4),
        radius: radius as i32,
        cps: cps as u32,
        requester: player.map(|p| p.uuid),
    };
    Some(match server.wgen.start(request) {
        Ok(total) => {
            let bytes = total * wgen::chunk_bytes(server.level(dimension).info().height);
            let estimate = wgen::format_duration(total as f64 / cps as f64);
            ok(format!(
                "Generating {total} chunks of {} around {bx} {bz} (radius {radius}) at up to {cps} chunks/s, \
                 at least {estimate}. Nothing is saved to disk yet, so they only live in memory (up to about {}).",
                dimension.name(),
                wgen::format_bytes(bytes)
            ))
        }
        Err(e) => err(e),
    })
}

/// UUID and spelling for a name: the online player's if there is one, otherwise the
/// offline-mode UUID vanilla would give a Java player of that name.
fn identify(server: &Server, name: &str) -> (Uuid, String) {
    match server.players.find(name) {
        Some(p) => (p.uuid, p.name.clone()),
        None => (offline_uuid(name), name.to_owned()),
    }
}

fn refresh_permission(server: &Server, name: &str, uuid: Uuid) {
    if let Some(p) = server.players.find(name).filter(|p| p.uuid == uuid || p.name.eq_ignore_ascii_case(name)) {
        let level = server.access.op_level(p.uuid, &p.name);
        p.send(Event::Permission(level));
        let text = if level > 0 { "You are now a server operator" } else { "You are no longer a server operator" };
        p.send(Event::Message(text.into(), Color::Yellow));
    }
}

fn enforce_whitelist(server: &Server) {
    if !server.config.enforce_whitelist || !server.access.whitelist_on() {
        return;
    }
    for p in server.players.all() {
        if !server.access.allowed(p.uuid, &p.name) {
            p.send(Event::Kick("You are not white-listed on this server!".into()));
        }
    }
}

fn save_whitelist_setting(server: &Server, on: bool) {
    if let Err(e) = crate::server::set_property(&server.config.properties_path, "white-list", &on.to_string()) {
        tracing::warn!("couldn't save white-list in server.properties: {e:#}");
    }
}

fn ok(text: String) -> Output {
    vec![(text, Color::White)]
}

fn err(text: String) -> Output {
    vec![(text, Color::Red)]
}
