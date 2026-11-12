pub mod chunk;
pub mod flat;
pub mod store;
pub mod vanilla;

pub use chunk::{Chunk, Section};
pub use store::ChunkStore;

use anyhow::Result;
use std::{sync::Arc, time::Duration};

/// Block state ids are vanilla's (the order of reports/blocks.json), whatever the client edition.
pub mod blocks {
    pub const AIR: u16 = 0;
    pub const STONE: u16 = 1;
    pub const GRASS_BLOCK: u16 = 9;
    pub const DIRT: u16 = 10;
    pub const BEDROCK: u16 = 88;
    pub const WATER: u16 = 89;
    pub const VOID_AIR: u16 = 18649;
    pub const CAVE_AIR: u16 = 18650;

    pub fn is_air(state: u16) -> bool {
        matches!(state, AIR | VOID_AIR | CAVE_AIR)
    }
}

pub struct WorldInfo {
    pub min_y: i32,
    pub height: u32,
    pub sea_level: i32,
    /// Superflat worlds get a lower horizon on the client.
    pub is_flat: bool,
    /// Names behind the biome ids in generated chunks.
    pub biome_names: Vec<String>,
}

/// Where finished chunks come from.
#[async_trait::async_trait]
pub trait ChunkSource: Send + Sync + 'static {
    async fn get(&self, x: i32, z: i32) -> Result<Arc<Chunk>>;
    fn info(&self) -> &WorldInfo;
    /// Feet position of a player who has never joined before.
    async fn spawn_point(&self) -> Result<[f64; 3]>;
    /// Lets go of chunks nobody asked for within `idle`, where that is safe.
    fn evict_idle(&self, _idle: Duration) -> usize {
        0
    }
}

/// A generator whose chunks don't depend on their neighbours. Generation runs on
/// several threads at once, each with its own `Worker`, so per-thread caches need no locking.
pub trait Generator: Send + Sync + 'static {
    fn worker(&self) -> Box<dyn Worker>;
    fn info(&self) -> WorldInfo;
    fn spawn_point(&self) -> Result<[f64; 3]>;
}

pub trait Worker: Send {
    fn generate(&mut self, x: i32, z: i32) -> Result<Chunk>;
}
