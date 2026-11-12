// Players currently online, whichever edition they joined with, and the way the rest
// of the server talks to their connection.

use crate::{server::GameMode, world::Dimension};
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicI32, AtomicU8, Ordering},
    },
};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edition {
    Java,
    Bedrock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    White,
    Gray,
    Yellow,
    Red,
    Green,
}

impl Color {
    pub fn name(self) -> &'static str {
        match self {
            Color::White => "white",
            Color::Gray => "gray",
            Color::Yellow => "yellow",
            Color::Red => "red",
            Color::Green => "green",
        }
    }

    /// Bedrock and console formatting code.
    pub fn code(self) -> char {
        match self {
            Color::White => 'f',
            Color::Gray => '7',
            Color::Yellow => 'e',
            Color::Red => 'c',
            Color::Green => 'a',
        }
    }
}

/// Things the server asks a connection to do.
#[derive(Debug)]
pub enum Event {
    Message(String, Color),
    GameMode(GameMode),
    /// The player's op level changed; command lists and permissions need refreshing.
    Permission(u8),
    Kick(String),
    /// A mob came into view.
    SpawnMob {
        id: i32,
        uuid: Uuid,
        type_id: i32,
        pos: [f64; 3],
        yaw: f32,
    },
    MoveEntity {
        id: i32,
        pos: [f64; 3],
        yaw: f32,
        on_ground: bool,
    },
    RemoveEntities(Vec<i32>),
    /// Red flash (and camera shake when it is the player's own id).
    EntityHurt {
        id: i32,
        yaw: f32,
    },
    /// Vanilla entity event, e.g. 3 for a living entity's death.
    EntityStatus {
        id: i32,
        status: u8,
    },
    EntityOnFire {
        id: i32,
        on_fire: bool,
    },
    /// Main-arm swing animation.
    Swing {
        id: i32,
    },
    Health(f32),
    BlockChanged {
        pos: [i32; 3],
        state: u16,
    },
    /// Moves the player to another dimension, or back to spawn after death.
    ChangeDimension {
        dimension: Dimension,
        pos: [f64; 3],
        yaw: f32,
        death: bool,
    },
    /// Players to add to the tab list.
    TabAdd(Vec<TabEntry>),
    TabRemove(Vec<Uuid>),
    TabGameMode(Uuid, GameMode),
    TabLatency(Uuid, i32),
    /// Absolute world time in ticks, after it was changed by a command.
    Time(i64),
}

pub struct Player {
    pub name: String,
    pub uuid: Uuid,
    pub edition: Edition,
    gamemode: AtomicU8,
    /// Keep-alive round trip in milliseconds, shown in the tab list.
    latency: AtomicI32,
    tx: mpsc::UnboundedSender<Event>,
    /// Last known dimension and feet position, kept up to date by the connection.
    location: Mutex<(Dimension, [f64; 3])>,
}

/// A line of the player list.
#[derive(Clone, Debug)]
pub struct TabEntry {
    pub uuid: Uuid,
    pub name: String,
    pub gamemode: GameMode,
    pub latency: i32,
}

impl Player {
    pub fn tab_entry(&self) -> TabEntry {
        TabEntry { uuid: self.uuid, name: self.name.clone(), gamemode: self.gamemode(), latency: self.latency() }
    }

    pub fn latency(&self) -> i32 {
        self.latency.load(Ordering::Relaxed)
    }

    pub fn set_latency(&self, ms: i32) {
        self.latency.store(ms, Ordering::Relaxed);
    }
    pub fn location(&self) -> (Dimension, [f64; 3]) {
        *self.location.lock().unwrap()
    }

    pub fn set_location(&self, dimension: Dimension, pos: [f64; 3]) {
        *self.location.lock().unwrap() = (dimension, pos);
    }

    pub fn gamemode(&self) -> GameMode {
        GameMode::from_id(self.gamemode.load(Ordering::Relaxed))
    }

    pub fn send(&self, event: Event) {
        if let Event::GameMode(mode) = &event {
            self.gamemode.store(mode.java_id() as u8, Ordering::Relaxed);
        }
        let _ = self.tx.send(event);
    }
}

#[derive(Default)]
pub struct Players {
    online: Mutex<HashMap<Uuid, std::sync::Arc<Player>>>,
}

/// Removes the player from the list when the connection ends.
pub struct Registration<'a> {
    players: &'a Players,
    uuid: Uuid,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        let mut online = self.players.online.lock().unwrap();
        online.remove(&self.uuid);
        for p in online.values() {
            p.send(Event::TabRemove(vec![self.uuid]));
        }
    }
}

pub enum Joined<'a> {
    Ok(Registration<'a>, mpsc::UnboundedReceiver<Event>),
    AlreadyOnline,
}

impl Players {
    pub fn join(&self, name: &str, uuid: Uuid, edition: Edition, gamemode: GameMode) -> Joined<'_> {
        let mut online = self.online.lock().unwrap();
        if online.contains_key(&uuid) || online.values().any(|p| p.name.eq_ignore_ascii_case(name)) {
            return Joined::AlreadyOnline;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let player = Player {
            name: name.to_owned(),
            uuid,
            edition,
            gamemode: AtomicU8::new(gamemode.java_id() as u8),
            latency: AtomicI32::new(0),
            tx,
            location: Mutex::new((Dimension::Overworld, [0.0; 3])),
        };
        let entry = player.tab_entry();
        for p in online.values() {
            p.send(Event::TabAdd(vec![entry.clone()]));
        }
        online.insert(uuid, std::sync::Arc::new(player));
        // The newcomer gets everyone, itself included.
        let mut everyone: Vec<TabEntry> = online.values().map(|p| p.tab_entry()).collect();
        everyone.sort_by_key(|e| e.name.to_lowercase());
        online[&uuid].send(Event::TabAdd(everyone));
        Joined::Ok(Registration { players: self, uuid }, rx)
    }

    /// Sends an event to everyone online.
    pub fn send_all(&self, event: impl Fn() -> Event) {
        for p in self.online.lock().unwrap().values() {
            p.send(event());
        }
    }

    pub fn get(&self, uuid: Uuid) -> Option<std::sync::Arc<Player>> {
        self.online.lock().unwrap().get(&uuid).cloned()
    }

    pub fn find(&self, name: &str) -> Option<std::sync::Arc<Player>> {
        self.online.lock().unwrap().values().find(|p| p.name.eq_ignore_ascii_case(name)).cloned()
    }

    pub fn all(&self) -> Vec<std::sync::Arc<Player>> {
        let mut list: Vec<_> = self.online.lock().unwrap().values().cloned().collect();
        list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        list
    }

    pub fn count(&self) -> usize {
        self.online.lock().unwrap().len()
    }

    pub fn broadcast(&self, text: &str, color: Color) {
        for p in self.online.lock().unwrap().values() {
            p.send(Event::Message(text.to_owned(), color));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_and_leave() {
        let players = Players::default();
        let id = Uuid::new_v4();
        let Joined::Ok(reg, mut rx) = players.join("Steve", id, Edition::Java, GameMode::Survival) else { panic!() };
        assert!(matches!(
            players.join("steve", Uuid::new_v4(), Edition::Bedrock, GameMode::Survival),
            Joined::AlreadyOnline
        ));
        let p = players.find("STEVE").unwrap();
        assert!(matches!(rx.try_recv(), Ok(Event::TabAdd(list)) if list.len() == 1 && list[0].uuid == id));
        p.send(Event::GameMode(GameMode::Creative));
        assert_eq!(p.gamemode(), GameMode::Creative);
        assert!(matches!(rx.try_recv(), Ok(Event::GameMode(GameMode::Creative))));
        drop(reg);
        assert!(players.find("Steve").is_none());
    }

    #[test]
    fn tab_list_follows_joins_and_leaves() {
        let players = Players::default();
        let steve = Uuid::new_v4();
        let alex = Uuid::new_v4();
        let Joined::Ok(_steve_reg, mut steve_rx) = players.join("Steve", steve, Edition::Java, GameMode::Survival)
        else {
            panic!()
        };
        steve_rx.try_recv().unwrap();
        let Joined::Ok(alex_reg, mut alex_rx) = players.join("Alex", alex, Edition::Java, GameMode::Creative) else {
            panic!()
        };
        let Ok(Event::TabAdd(seen)) = steve_rx.try_recv() else { panic!() };
        assert_eq!((seen.len(), seen[0].uuid, seen[0].gamemode), (1, alex, GameMode::Creative));
        let Ok(Event::TabAdd(all)) = alex_rx.try_recv() else { panic!() };
        let names: Vec<&str> = all.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Alex", "Steve"]);

        drop(alex_reg);
        assert!(matches!(steve_rx.try_recv(), Ok(Event::TabRemove(gone)) if gone == [alex]));
    }
}
