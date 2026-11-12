use super::blocks;

pub const SECTION_BLOCKS: usize = 4096;
pub const SECTION_BIOMES: usize = 64;

#[derive(Clone)]
pub struct Section {
    /// Indexed `(y * 16 + z) * 16 + x`.
    pub blocks: Box<[u16; SECTION_BLOCKS]>,
    /// One per 4×4×4 cell, indexed `(y * 4 + z) * 4 + x`.
    pub biomes: [u16; SECTION_BIOMES],
}

impl Section {
    pub fn filled(block: u16, biome: u16) -> Self {
        Self { blocks: Box::new([block; SECTION_BLOCKS]), biomes: [biome; SECTION_BIOMES] }
    }

    pub fn non_air_count(&self) -> usize {
        self.blocks.iter().filter(|b| !blocks::is_air(**b)).count()
    }
}

#[derive(Clone)]
pub struct Chunk {
    pub x: i32,
    pub z: i32,
    pub min_y: i32,
    pub sections: Vec<Section>,
}

impl Chunk {
    pub fn empty(x: i32, z: i32, min_y: i32, height: u32, biome: u16) -> Self {
        Self { x, z, min_y, sections: vec![Section::filled(blocks::AIR, biome); (height / 16) as usize] }
    }

    pub fn max_y(&self) -> i32 {
        self.min_y + self.sections.len() as i32 * 16
    }

    pub fn block(&self, x: usize, y: i32, z: usize) -> u16 {
        let dy = y - self.min_y;
        if dy < 0 || y >= self.max_y() {
            return blocks::AIR;
        }
        self.sections[dy as usize / 16].blocks[((dy as usize % 16) * 16 + z) * 16 + x]
    }

    pub fn set_block(&mut self, x: usize, y: i32, z: usize, state: u16) {
        let dy = y - self.min_y;
        if dy >= 0 && y < self.max_y() {
            self.sections[dy as usize / 16].blocks[((dy as usize % 16) * 16 + z) * 16 + x] = state;
        }
    }

    /// Y just above the highest non-air block in the column, or `min_y` if there is none.
    pub fn surface(&self, x: usize, z: usize) -> i32 {
        (self.min_y..self.max_y()).rev().find(|&y| !blocks::is_air(self.block(x, y, z))).map_or(self.min_y, |y| y + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_indexing() {
        let mut c = Chunk::empty(0, 0, -64, 384, 0);
        c.set_block(3, -60, 5, blocks::STONE);
        assert_eq!(c.block(3, -60, 5), blocks::STONE);
        assert_eq!(c.sections[0].blocks[(4 * 16 + 5) * 16 + 3], blocks::STONE);
        assert_eq!(c.surface(3, 5), -59);
        assert_eq!(c.surface(0, 0), -64);
        assert_eq!(c.block(0, 400, 0), blocks::AIR);
    }
}
