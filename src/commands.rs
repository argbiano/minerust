// Commands, the same for the console, Java players and Bedrock players.

use crate::{
    access::Change,
    java::offline_uuid,
    players::{Color, Event},
    server::{GameMode, Server},
};
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
