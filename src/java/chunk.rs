use super::codec::Writer;
use crate::world::{Chunk, Section};

// Heightmap.Types ordinals that vanilla sends to clients.
const WORLD_SURFACE: i32 = 1;
const MOTION_BLOCKING: i32 = 4;
const MOTION_BLOCKING_NO_LEAVES: i32 = 5;

/// Body of a level_chunk_with_light packet. `biome_ids` maps the generator's biome ids
/// to this connection's registry ids.
pub fn chunk_packet(chunk: &Chunk, biome_ids: &[i32], biome_registry_size: usize) -> Vec<u8> {
    let mut w = Writer::new();
    w.i32(chunk.x).i32(chunk.z);

    // Surface heights stand in for all three heightmaps until blocks know whether they
    // block motion; the client only uses them for rain and a few render checks.
    let height = (chunk.max_y() - chunk.min_y) as usize;
    let mut surface = [0u64; 256];
    for z in 0..16 {
        for x in 0..16 {
            surface[z * 16 + x] = (chunk.surface(x, z) - chunk.min_y) as u64;
        }
    }
    let heightmap = pack(&surface, bits_for(height + 1));
    w.varint(3);
    for kind in [WORLD_SURFACE, MOTION_BLOCKING, MOTION_BLOCKING_NO_LEAVES] {
        w.varint(kind).varint(heightmap.len() as i32);
        for v in &heightmap {
            w.i64(*v as i64);
        }
    }

    let mut data = Writer::new();
    for section in &chunk.sections {
        write_section(&mut data, section, biome_ids, biome_registry_size);
    }
    w.varint(data.buf.len() as i32).bytes(&data.buf);
    w.varint(0); // block entities

    // Full sky light everywhere, one extra section above and below the world.
    let light_sections = chunk.sections.len() + 2;
    let mask = (1u64 << light_sections) - 1;
    write_bit_set(&mut w, mask); // sky light
    write_bit_set(&mut w, 0); // block light
    write_bit_set(&mut w, 0); // empty sky light
    write_bit_set(&mut w, mask); // empty block light
    w.varint(light_sections as i32);
    for _ in 0..light_sections {
        w.varint(2048).bytes(&[0xFF; 2048]);
    }
    w.varint(0);
    w.buf
}

/// ByteBufCodecs.BIT_SET: `java.util.BitSet.toByteArray()` with a varint length, so
/// little-endian bytes without trailing zeros.
fn write_bit_set(w: &mut Writer, bits: u64) {
    let bytes = bits.to_le_bytes();
    let len = 8 - (bits.leading_zeros() / 8) as usize;
    w.varint(len as i32).bytes(&bytes[..len]);
}

// 26.3 has 35723 block states.
const BLOCK_STATE_BITS: u32 = 16;

fn write_section(w: &mut Writer, section: &Section, biome_ids: &[i32], biome_registry_size: usize) {
    w.i16(section.non_air_count() as i16).i16(0);

    let blocks: Vec<i32> = section.blocks.iter().map(|&b| b as i32).collect();
    write_paletted(w, &blocks, 4, 8, BLOCK_STATE_BITS);

    let biomes: Vec<i32> = section.biomes.iter().map(|&b| biome_ids.get(b as usize).copied().unwrap_or(0)).collect();
    write_paletted(w, &biomes, 1, 3, bits_for(biome_registry_size));
}

/// PalettedContainer wire format: single value, a local palette of `min_bits..=max_bits`,
/// or global ids at `global_bits`.
fn write_paletted(w: &mut Writer, values: &[i32], min_bits: u32, max_bits: u32, global_bits: u32) {
    let mut palette: Vec<i32> = Vec::new();
    let mut indices = Vec::with_capacity(values.len());
    for &v in values {
        let i = match palette.iter().position(|&p| p == v) {
            Some(i) => i,
            None => {
                palette.push(v);
                palette.len() - 1
            }
        };
        indices.push(i as u64);
        if palette.len() > 1 << max_bits {
            break;
        }
    }

    if palette.len() == 1 {
        w.u8(0).varint(palette[0]);
        return;
    }
    let bits = bits_for(palette.len()).max(min_bits);
    if bits <= max_bits {
        w.u8(bits as u8).varint(palette.len() as i32);
        for &p in &palette {
            w.varint(p);
        }
    } else {
        w.u8(global_bits as u8);
        indices = values.iter().map(|&v| v as u64).collect();
        return write_longs(w, &pack(&indices, global_bits));
    }
    write_longs(w, &pack(&indices, bits));
}

fn write_longs(w: &mut Writer, longs: &[u64]) {
    for v in longs {
        w.i64(*v as i64);
    }
}

fn bits_for(values: usize) -> u32 {
    usize::BITS - (values.max(2) - 1).leading_zeros()
}

// Entries never straddle two longs.
fn pack(values: &[u64], bits: u32) -> Vec<u64> {
    let per_long = (64 / bits) as usize;
    let mut out = vec![0u64; values.len().div_ceil(per_long)];
    for (i, v) in values.iter().enumerate() {
        out[i / per_long] |= v << ((i % per_long) as u32 * bits);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Generator, blocks, flat::FlatGenerator};

    #[test]
    fn single_value_section() {
        let mut w = Writer::new();
        write_section(&mut w, &Section::filled(blocks::AIR, 0), &[39], 67);
        assert_eq!(w.buf, [0, 0, 0, 0, 0, 0, 0, 39]);
    }

    #[test]
    fn flat_bottom_section_uses_four_bit_palette() {
        let chunk = FlatGenerator::classic().worker().generate(0, 0).unwrap();
        let mut w = Writer::new();
        write_section(&mut w, &chunk.sections[0], &[39], 67);
        // counts, bits, palette length + 4 single-byte ids, 256 longs, single-value biome
        assert_eq!(w.buf.len(), 4 + 1 + 1 + 4 + 256 * 8 + 2);
        assert_eq!(&w.buf[..2], &1024i16.to_be_bytes());
        assert_eq!(w.buf[4], 4);
    }

    #[test]
    fn many_states_fall_back_to_global_palette() {
        let mut section = Section::filled(blocks::AIR, 0);
        for (i, b) in section.blocks.iter_mut().enumerate() {
            *b = (i % 300) as u16;
        }
        let mut w = Writer::new();
        write_section(&mut w, &section, &[0], 67);
        assert_eq!(w.buf[4], 16);
        // 16 bits per entry: 4096 / 4 longs
        assert_eq!(w.buf.len(), 4 + 1 + 1024 * 8 + 2);
    }

    #[test]
    fn mixed_biomes_get_a_palette() {
        let mut section = Section::filled(blocks::STONE, 0);
        section.biomes[5] = 1;
        let mut w = Writer::new();
        write_section(&mut w, &section, &[10, 20], 67);
        let biome_part = &w.buf[4 + 2..];
        assert_eq!(biome_part[..4], [1, 2, 10, 20]);
    }

    #[test]
    fn bit_set_matches_java() {
        // new BitSet() -> [], bits 0..25 set -> FF FF FF 03
        let mut w = Writer::new();
        write_bit_set(&mut w, 0);
        write_bit_set(&mut w, (1 << 26) - 1);
        assert_eq!(w.buf, [0, 4, 0xFF, 0xFF, 0xFF, 0x03]);
    }

    #[test]
    fn heightmap_packing() {
        assert_eq!(bits_for(385), 9);
        let longs = pack(&[4; 256], 9);
        assert_eq!(longs.len(), 37);
        assert_eq!(longs[0] & 0x1FF, 4);
    }
}
