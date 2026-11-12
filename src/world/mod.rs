pub mod chunk;
pub mod flat;
pub mod level;
pub mod passable;
pub mod store;
pub mod vanilla;

pub use chunk::{Chunk, Section};
pub use level::{Dimension, Level};
pub use store::ChunkStore;

use anyhow::Result;
use std::{sync::Arc, time::Duration};

/// Block state ids are vanilla's (the order of reports/blocks.json), whatever the client edition.
pub mod blocks {
    pub const AIR: u16 = 0;
    pub const STONE: u16 = 1;
    pub const COBBLESTONE: u16 = 14;
    pub const OAK_PLANKS: u16 = 15;
    pub const SAND: u16 = 121;
    pub const GLASS: u16 = 661;
    pub const GRASS_BLOCK: u16 = 9;
    pub const DIRT: u16 = 10;
    pub const BEDROCK: u16 = 88;
    pub const WATER: u16 = 89;
    pub const LAVA: u16 = 105;
    pub const OBSIDIAN: u16 = 4925;
    pub const NETHERRACK: u16 = 8667;
    pub const NETHER_PORTAL_X: u16 = 8687;
    pub const NETHER_PORTAL_Z: u16 = 8688;
    pub const VOID_AIR: u16 = 18649;
    pub const CAVE_AIR: u16 = 18650;

    pub fn is_air(state: u16) -> bool {
        matches!(state, AIR | VOID_AIR | CAVE_AIR)
    }

    /// Water (89..=104) and lava (105..=120), every level.
    pub fn is_liquid(state: u16) -> bool {
        (89..=120).contains(&state)
    }

    pub fn is_lava(state: u16) -> bool {
        (105..=120).contains(&state)
    }

    pub fn is_portal(state: u16) -> bool {
        matches!(state, NETHER_PORTAL_X | NETHER_PORTAL_Z)
    }

    /// Whether a mob stands on or bumps into the block.
    pub fn is_solid(state: u16) -> bool {
        !in_ranges(super::passable::PASSABLE, state)
    }

    /// Whether flowing water or lava may wash the block away and take its place.
    pub fn is_fluid_replaceable(state: u16) -> bool {
        in_ranges(super::passable::FLUID_REPLACEABLE, state)
    }

    /// Whether the block may give off light (any of its states does, lit or not).
    pub fn is_light_source(state: u16) -> bool {
        in_ranges(super::passable::LIGHT_SOURCES, state)
    }

    /// Grass block, snowy or not: where animals spawn.
    pub fn is_grass_block(state: u16) -> bool {
        matches!(state, 8 | GRASS_BLOCK)
    }

    fn in_ranges(ranges: &[(u16, u16)], state: u16) -> bool {
        ranges
            .binary_search_by(|&(lo, hi)| {
                if hi < state {
                    std::cmp::Ordering::Less
                } else if lo > state {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
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
    /// The chunk if it is already generated, without waiting.
    fn cached(&self, _x: i32, _z: i32) -> Option<Arc<Chunk>> {
        None
    }
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

#[cfg(test)]
mod tests {
    use super::blocks::*;

    #[test]
    fn solidity() {
        assert!(!is_solid(AIR) && !is_solid(WATER) && !is_solid(NETHER_PORTAL_X) && !is_solid(30));
        assert!(is_solid(STONE) && is_solid(GRASS_BLOCK) && is_solid(OBSIDIAN) && is_solid(NETHERRACK));
    }
}
