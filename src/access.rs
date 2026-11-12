// ops.json and whitelist.json, in vanilla's format so files can move between servers.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpEntry {
    pub uuid: Uuid,
    pub name: String,
    pub level: u8,
    #[serde(default)]
    pub bypasses_player_limit: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WhitelistEntry {
    pub uuid: Uuid,
    pub name: String,
}

pub struct Access {
    dir: PathBuf,
    default_op_level: u8,
    state: Mutex<State>,
}

struct State {
    ops: Vec<OpEntry>,
    whitelist: Vec<WhitelistEntry>,
    whitelist_on: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Done,
    Unchanged,
}

impl Access {
    pub fn load(dir: &Path, whitelist_on: bool, default_op_level: u8) -> Result<Self> {
        let access = Self {
            dir: dir.to_owned(),
            default_op_level: default_op_level.clamp(1, 4),
            state: Mutex::new(State { ops: Vec::new(), whitelist: Vec::new(), whitelist_on }),
        };
        access.reload()?;
        Ok(access)
    }

    /// Re-reads both files, for edits made while the server runs.
    pub fn reload(&self) -> Result<()> {
        let ops = read_list(&self.dir.join("ops.json"))?;
        let whitelist = read_list(&self.dir.join("whitelist.json"))?;
        let mut state = self.state.lock().unwrap();
        state.ops = ops;
        state.whitelist = whitelist;
        Ok(())
    }

    /// Permission level 0..=4 for a player.
    pub fn op_level(&self, uuid: Uuid, name: &str) -> u8 {
        let state = self.state.lock().unwrap();
        state.ops.iter().find(|o| matches(o.uuid, &o.name, uuid, name)).map_or(0, |o| o.level)
    }

    pub fn bypasses_player_limit(&self, uuid: Uuid, name: &str) -> bool {
        let state = self.state.lock().unwrap();
        state.ops.iter().any(|o| matches(o.uuid, &o.name, uuid, name) && o.bypasses_player_limit)
    }

    pub fn op(&self, uuid: Uuid, name: &str) -> Result<Change> {
        let mut state = self.state.lock().unwrap();
        if state.ops.iter().any(|o| matches(o.uuid, &o.name, uuid, name)) {
            return Ok(Change::Unchanged);
        }
        let entry = OpEntry { uuid, name: name.to_owned(), level: self.default_op_level, bypasses_player_limit: false };
        state.ops.push(entry);
        write_list(&self.dir.join("ops.json"), &state.ops)?;
        Ok(Change::Done)
    }

    pub fn deop(&self, uuid: Uuid, name: &str) -> Result<Change> {
        let mut state = self.state.lock().unwrap();
        let before = state.ops.len();
        state.ops.retain(|o| !matches(o.uuid, &o.name, uuid, name));
        if state.ops.len() == before {
            return Ok(Change::Unchanged);
        }
        write_list(&self.dir.join("ops.json"), &state.ops)?;
        Ok(Change::Done)
    }

    pub fn whitelist_on(&self) -> bool {
        self.state.lock().unwrap().whitelist_on
    }

    pub fn set_whitelist_on(&self, on: bool) {
        self.state.lock().unwrap().whitelist_on = on;
    }

    /// Whether a player may join. Ops always may, as in vanilla.
    pub fn allowed(&self, uuid: Uuid, name: &str) -> bool {
        let state = self.state.lock().unwrap();
        !state.whitelist_on
            || state.whitelist.iter().any(|w| matches(w.uuid, &w.name, uuid, name))
            || state.ops.iter().any(|o| matches(o.uuid, &o.name, uuid, name))
    }

    pub fn whitelist_add(&self, uuid: Uuid, name: &str) -> Result<Change> {
        let mut state = self.state.lock().unwrap();
        if state.whitelist.iter().any(|w| matches(w.uuid, &w.name, uuid, name)) {
            return Ok(Change::Unchanged);
        }
        state.whitelist.push(WhitelistEntry { uuid, name: name.to_owned() });
        write_list(&self.dir.join("whitelist.json"), &state.whitelist)?;
        Ok(Change::Done)
    }

    pub fn whitelist_remove(&self, uuid: Uuid, name: &str) -> Result<Change> {
        let mut state = self.state.lock().unwrap();
        let before = state.whitelist.len();
        state.whitelist.retain(|w| !matches(w.uuid, &w.name, uuid, name));
        if state.whitelist.len() == before {
            return Ok(Change::Unchanged);
        }
        write_list(&self.dir.join("whitelist.json"), &state.whitelist)?;
        Ok(Change::Done)
    }

    pub fn whitelist_names(&self) -> Vec<String> {
        self.state.lock().unwrap().whitelist.iter().map(|w| w.name.clone()).collect()
    }

    pub fn op_names(&self) -> Vec<String> {
        self.state.lock().unwrap().ops.iter().map(|o| o.name.clone()).collect()
    }
}

// Offline mode: a Bedrock player and a Java player with the same name get different
// UUIDs, so names count too (case-insensitively, like vanilla's offline lookups).
fn matches(entry_uuid: Uuid, entry_name: &str, uuid: Uuid, name: &str) -> bool {
    entry_uuid == uuid || entry_name.eq_ignore_ascii_case(name)
}

fn read_list<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Vec::new()),
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("{} is not valid", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_list<T: Serialize>(path: &Path, list: &[T]) -> Result<()> {
    let text = serde_json::to_string_pretty(list)?;
    std::fs::write(path, text + "\n").with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("minerust-access-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ops_round_trip_in_vanilla_format() {
        let d = dir("ops");
        let a = Access::load(&d, false, 4).unwrap();
        let id = Uuid::new_v4();
        assert_eq!(a.op(id, "Steve").unwrap(), Change::Done);
        assert_eq!(a.op(id, "steve").unwrap(), Change::Unchanged);
        assert_eq!(a.op_level(Uuid::nil(), "STEVE"), 4);

        let text = std::fs::read_to_string(d.join("ops.json")).unwrap();
        assert!(text.contains("\"bypassesPlayerLimit\": false") && text.contains("\"level\": 4"));
        let again = Access::load(&d, false, 4).unwrap();
        assert_eq!(again.op_level(id, "x"), 4);
        assert_eq!(again.deop(id, "Steve").unwrap(), Change::Done);
        assert_eq!(again.op_level(id, "Steve"), 0);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn whitelist_rules() {
        let d = dir("wl");
        let a = Access::load(&d, true, 4).unwrap();
        let (alex, op) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(!a.allowed(alex, "Alex"));
        a.whitelist_add(alex, "Alex").unwrap();
        assert!(a.allowed(alex, "Alex"));
        a.op(op, "Admin").unwrap();
        assert!(a.allowed(op, "Admin"), "ops bypass the whitelist");
        a.set_whitelist_on(false);
        assert!(a.allowed(Uuid::new_v4(), "Anyone"));
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn reads_files_vanilla_wrote() {
        let d = dir("vanilla");
        std::fs::write(
            d.join("whitelist.json"),
            r#"[{"uuid": "b50ad385-829d-3141-a216-7e7d7539ba7f", "name": "Notch"}]"#,
        )
        .unwrap();
        let a = Access::load(&d, true, 4).unwrap();
        assert!(a.allowed("b50ad385-829d-3141-a216-7e7d7539ba7f".parse().unwrap(), "whatever"));
        std::fs::remove_dir_all(d).ok();
    }
}
