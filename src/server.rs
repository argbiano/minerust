use crate::{
    access::Access,
    players::Players,
    properties::Properties,
    world::{
        ChunkSource, ChunkStore,
        flat::FlatGenerator,
        vanilla::{VanillaWorld, parse_seed},
    },
};
use anyhow::{Context, Result, bail};
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI32, AtomicUsize},
    },
    time::Instant,
};
use tokio::sync::Notify;
use tracing::info;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
}

impl GameMode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "survival" | "s" | "0" => Self::Survival,
            "creative" | "c" | "1" => Self::Creative,
            "adventure" | "a" | "2" => Self::Adventure,
            "spectator" | "sp" | "3" => Self::Spectator,
            _ => return None,
        })
    }

    pub fn from_id(id: u8) -> Self {
        match id {
            1 => Self::Creative,
            2 => Self::Adventure,
            3 => Self::Spectator,
            _ => Self::Survival,
        }
    }

    pub fn java_id(self) -> i32 {
        self as i32
    }

    pub fn display(self) -> &'static str {
        match self {
            Self::Survival => "Survival",
            Self::Creative => "Creative",
            Self::Adventure => "Adventure",
            Self::Spectator => "Spectator",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelType {
    Normal,
    Flat,
}

impl LevelType {
    fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().trim_start_matches("minecraft:") {
            "normal" | "default" => Self::Normal,
            "flat" => Self::Flat,
            _ => return None,
        })
    }
}

pub struct Config {
    pub motd: String,
    pub max_players: i32,
    pub view_distance: i32,
    pub gamemode: GameMode,
    pub java_bind: String,
    /// TCP address for NetherNet signaling, `None` when Bedrock is turned off.
    pub bedrock_bind: Option<String>,
    /// Where each Bedrock peer binds its UDP socket; port 0 picks a free one.
    pub bedrock_udp_bind: String,
    /// Advertised instead of the local address when the server sits behind NAT.
    pub bedrock_public_ip: Option<IpAddr>,
    pub bedrock_key_file: PathBuf,
    pub level_type: LevelType,
    pub seed: i64,
    /// Output of `minerust prepare`.
    pub vanilla_data: PathBuf,
    pub white_list: bool,
    /// Kick players who stop being allowed when the whitelist changes.
    pub enforce_whitelist: bool,
    pub op_permission_level: u8,
    pub properties_path: PathBuf,
}

impl Config {
    /// Loads server.properties, writing it (or the keys it lacks) with defaults first.
    pub fn load(path: &Path) -> Result<Self> {
        let existed = path.exists();
        let mut p = Properties::load(path)?;

        let ip = p.get_or("server-ip", "");
        let port: u16 = number(&p.get_or("server-port", "25565"), "server-port")?;
        let bedrock_port: u16 = number(&p.get_or("bedrock-port", "19132"), "bedrock-port")?;
        let bedrock_udp_port: u16 = number(&p.get_or("bedrock-udp-port", "0"), "bedrock-udp-port")?;
        let enable_bedrock = boolean(&p.get_or("enable-bedrock", "true"), "enable-bedrock")?;
        let public_ip = p.get_or("bedrock-public-ip", "");
        let gamemode = p.get_or("gamemode", "creative");
        let level_type = p.get_or("level-type", "minecraft:normal");
        let seed = match parse_seed(&p.get_or("level-seed", "")) {
            Some(seed) => seed,
            None => {
                // Nothing is saved yet, so a random seed has to be written down or the
                // world would change on every restart.
                let seed: i64 = rand::random();
                p.set("level-seed", &seed.to_string());
                p.save(path)?;
                info!("no level-seed set, picked {seed}");
                seed
            }
        };
        let vanilla_data = p.get_or("vanilla-data", "vanilla-data");
        let white_list = boolean(&p.get_or("white-list", "false"), "white-list")?;
        let enforce_whitelist = boolean(&p.get_or("enforce-whitelist", "false"), "enforce-whitelist")?;
        let op_permission_level: u8 = number(&p.get_or("op-permission-level", "4"), "op-permission-level")?;

        let host = if ip.is_empty() { "0.0.0.0".to_owned() } else { ip };
        let config = Self {
            motd: p.get_or("motd", "A Minerust Server"),
            max_players: number(&p.get_or("max-players", "20"), "max-players")?,
            view_distance: number::<i32>(&p.get_or("view-distance", "10"), "view-distance")?.clamp(2, 32),
            gamemode: GameMode::parse(&gamemode).with_context(|| format!("unknown gamemode {gamemode:?}"))?,
            java_bind: join(&host, port),
            bedrock_bind: enable_bedrock.then(|| join(&host, bedrock_port)),
            bedrock_udp_bind: join(&host, bedrock_udp_port),
            bedrock_public_ip: if public_ip.is_empty() {
                None
            } else {
                Some(public_ip.parse().context("bedrock-public-ip is not an IP address")?)
            },
            bedrock_key_file: "bedrock-operator.key".into(),
            level_type: LevelType::parse(&level_type).with_context(|| format!("unknown level-type {level_type:?}"))?,
            seed,
            vanilla_data: vanilla_data.into(),
            white_list,
            enforce_whitelist,
            op_permission_level: op_permission_level.clamp(1, 4),
            properties_path: path.to_owned(),
        };

        if p.has_new_keys() {
            p.save(path)?;
            if existed {
                info!("added missing settings to {}", path.display());
            } else {
                info!("created {} with default settings", path.display());
            }
        }
        Ok(config)
    }
}

fn join(host: &str, port: u16) -> String {
    if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }
}

fn number<T: std::str::FromStr>(value: &str, key: &str) -> Result<T> {
    value.trim().parse().ok().with_context(|| format!("{key} must be a number, got {value:?}"))
}

fn boolean(value: &str, key: &str) -> Result<bool> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => bail!("{key} must be true or false, got {other:?}"),
    }
}

/// Changes one key in server.properties, keeping the rest of the file.
pub fn set_property(path: &Path, key: &str, value: &str) -> Result<()> {
    let mut p = Properties::load(path)?;
    p.set(key, value);
    p.save(path)
}

pub struct Server {
    pub config: Config,
    pub chunks: Arc<dyn ChunkSource>,
    /// Where new players appear.
    pub spawn: [f64; 3],
    pub access: Access,
    pub players: Players,
    pub shutdown: Notify,
    pub next_entity_id: AtomicI32,
    pub bedrock_peers: AtomicUsize,
    pub session_id: Uuid,
    pub started: Instant,
}

impl Server {
    pub async fn new(config: Config) -> Result<Self> {
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
        let chunks: Arc<dyn ChunkSource> = match config.level_type {
            LevelType::Flat => {
                info!("superflat world, generating chunks on {threads} threads");
                Arc::new(ChunkStore::new(Arc::new(FlatGenerator::classic()), threads))
            }
            LevelType::Normal => {
                let world = VanillaWorld::open(&config.vanilla_data, config.seed, threads).context(
                    "can't start the vanilla world generator; to use superflat instead set level-type=minecraft\\:flat in server.properties",
                )?;
                Arc::new(world)
            }
        };
        let spawn = chunks.spawn_point().await?;
        info!("spawn point {:.1} {:.1} {:.1}", spawn[0], spawn[1], spawn[2]);
        let dir = config.properties_path.parent().map(Path::to_path_buf).unwrap_or_default();
        let access = Access::load(&dir, config.white_list, config.op_permission_level)?;
        Ok(Self {
            config,
            chunks,
            spawn,
            access,
            players: Players::default(),
            shutdown: Notify::new(),
            next_entity_id: AtomicI32::new(1),
            bedrock_peers: AtomicUsize::new(0),
            session_id: Uuid::new_v4(),
            started: Instant::now(),
        })
    }

    pub fn is_full(&self, uuid: Uuid, name: &str) -> bool {
        self.players.count() >= self.config.max_players.max(0) as usize
            && !self.access.bypasses_player_limit(uuid, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_defaults_then_keeps_edits() {
        let dir = std::env::temp_dir().join(format!("minerust-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("server.properties");

        let config = Config::load(&path).unwrap();
        assert_eq!(config.java_bind, "0.0.0.0:25565");
        assert_eq!(config.bedrock_bind.as_deref(), Some("0.0.0.0:19132"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("server-port=25565") && text.contains("gamemode=creative"));

        let edited = text
            .replace("server-port=25565", "server-port=25570")
            .replace("enable-bedrock=true", "enable-bedrock=false")
            .replace("gamemode=creative", "gamemode=survival");
        std::fs::write(&path, edited).unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.java_bind, "0.0.0.0:25570");
        assert_eq!(config.bedrock_bind, None);
        assert_eq!(config.gamemode, GameMode::Survival);

        std::fs::write(&path, "max-players=abc\n").unwrap();
        assert!(Config::load(&path).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
