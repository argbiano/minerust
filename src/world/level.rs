// A dimension: its generated chunks plus the blocks the server changed since (portals).
// Edits live beside the generator's chunks, so they survive chunk eviction.

use super::{Chunk, ChunkSource, WorldInfo};
use anyhow::Result;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dimension {
    Overworld,
    Nether,
}

impl Dimension {
    pub fn name(self) -> &'static str {
        match self {
            Self::Overworld => "minecraft:overworld",
            Self::Nether => "minecraft:the_nether",
        }
    }

    /// Blocks of the overworld per block of this dimension.
    pub fn scale(self) -> f64 {
        match self {
            Self::Overworld => 1.0,
            Self::Nether => 8.0,
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Overworld => Self::Nether,
            Self::Nether => Self::Overworld,
        }
    }
}

type Edits = HashMap<(i32, i32), HashMap<(usize, i32, usize), u16>>;

pub struct Level {
    pub dimension: Dimension,
    pub source: Arc<dyn ChunkSource>,
    edits: Mutex<Edits>,
}

impl Level {
    pub fn new(dimension: Dimension, source: Arc<dyn ChunkSource>) -> Self {
        Self { dimension, source, edits: Mutex::default() }
    }

    pub fn info(&self) -> &WorldInfo {
        self.source.info()
    }

    /// The chunk as players should see it.
    pub async fn chunk(&self, x: i32, z: i32) -> Result<Arc<Chunk>> {
        let chunk = self.source.get(x, z).await?;
        Ok(self.with_edits(chunk))
    }

    /// The block if its chunk is generated, without waiting.
    pub fn cached_block(&self, x: i32, y: i32, z: i32) -> Option<u16> {
        let key = (x >> 4, z >> 4);
        let local = ((x & 15) as usize, y, (z & 15) as usize);
        if let Some(&state) = self.edits.lock().unwrap().get(&key).and_then(|e| e.get(&local)) {
            return Some(state);
        }
        Some(self.source.cached(key.0, key.1)?.block(local.0, y, local.2))
    }

    /// The topmost column surface (y above the highest non-air block), if generated.
    pub fn cached_surface(&self, x: i32, z: i32) -> Option<i32> {
        let chunk = self.source.cached(x >> 4, z >> 4)?;
        let (lx, lz) = ((x & 15) as usize, (z & 15) as usize);
        let mut top = chunk.surface(lx, lz);
        // Edits may sit above the generated surface.
        if let Some(edits) = self.edits.lock().unwrap().get(&(x >> 4, z >> 4)) {
            for (&(ex, ey, ez), &state) in edits {
                if ex == lx && ez == lz && !super::blocks::is_air(state) {
                    top = top.max(ey + 1);
                }
            }
        }
        Some(top)
    }

    /// Records a block change. Callers tell the players.
    pub fn set_block(&self, x: i32, y: i32, z: i32, state: u16) {
        let info = self.info();
        if y < info.min_y || y >= info.min_y + info.height as i32 {
            return;
        }
        let local = ((x & 15) as usize, y, (z & 15) as usize);
        self.edits.lock().unwrap().entry((x >> 4, z >> 4)).or_default().insert(local, state);
    }

    fn with_edits(&self, chunk: Arc<Chunk>) -> Arc<Chunk> {
        let edits = self.edits.lock().unwrap();
        let Some(changes) = edits.get(&(chunk.x, chunk.z)) else { return chunk };
        let mut edited = (*chunk).clone();
        for (&(x, y, z), &state) in changes {
            edited.set_block(x, y, z, state);
        }
        Arc::new(edited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{ChunkStore, blocks, flat::FlatGenerator};

    #[tokio::test(flavor = "multi_thread")]
    async fn edits_apply_to_served_chunks() {
        let level = Level::new(Dimension::Overworld, Arc::new(ChunkStore::new(Arc::new(FlatGenerator::classic()), 1)));
        assert_eq!(level.cached_block(-3, -61, 5), None);
        level.chunk(-1, 0).await.unwrap();
        assert_eq!(level.cached_block(-3, -61, 5), Some(blocks::GRASS_BLOCK));
        level.set_block(-3, -60, 5, blocks::OBSIDIAN);
        assert_eq!(level.cached_block(-3, -60, 5), Some(blocks::OBSIDIAN));
        assert_eq!(level.cached_surface(-3, 5), Some(-59));
        let chunk = level.chunk(-1, 0).await.unwrap();
        assert_eq!(chunk.block(13, -60, 5), blocks::OBSIDIAN);
    }
}
