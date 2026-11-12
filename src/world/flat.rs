use super::{Chunk, Generator, Worker, WorldInfo, blocks};
use anyhow::Result;

/// Superflat: the same stack of layers in every column.
pub struct FlatGenerator {
    pub layers: Vec<u16>,
    min_y: i32,
    height: u32,
    biome: &'static str,
}

impl FlatGenerator {
    /// Vanilla's default superflat preset: bedrock, two dirt, grass.
    pub fn classic() -> Self {
        Self {
            layers: vec![blocks::BEDROCK, blocks::DIRT, blocks::DIRT, blocks::GRASS_BLOCK],
            min_y: -64,
            height: 384,
            biome: "minecraft:plains",
        }
    }

    /// A netherrack floor for superflat worlds, which have no nether preset of their own.
    pub fn nether() -> Self {
        let mut layers = vec![blocks::BEDROCK];
        layers.extend([blocks::NETHERRACK; 31]);
        Self { layers, min_y: 0, height: 256, biome: "minecraft:nether_wastes" }
    }
}

impl Generator for FlatGenerator {
    fn worker(&self) -> Box<dyn Worker> {
        Box::new(FlatWorker { layers: self.layers.clone(), min_y: self.min_y, height: self.height })
    }

    fn info(&self) -> WorldInfo {
        WorldInfo {
            min_y: self.min_y,
            height: self.height,
            sea_level: self.min_y + self.layers.len() as i32 - 1,
            is_flat: true,
            biome_names: vec![self.biome.into()],
        }
    }

    fn spawn_point(&self) -> Result<[f64; 3]> {
        Ok([8.5, (self.min_y + self.layers.len() as i32) as f64, 8.5])
    }
}

struct FlatWorker {
    layers: Vec<u16>,
    min_y: i32,
    height: u32,
}

impl Worker for FlatWorker {
    fn generate(&mut self, x: i32, z: i32) -> Result<Chunk> {
        let mut chunk = Chunk::empty(x, z, self.min_y, self.height, 0);
        for (i, &state) in self.layers.iter().enumerate() {
            let section = &mut chunk.sections[i / 16];
            let row = i % 16;
            section.blocks[row * 256..(row + 1) * 256].fill(state);
        }
        Ok(chunk)
    }
}
