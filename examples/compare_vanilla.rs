// Compares chunks a vanilla server saved with what minerust generates for the same
// seed, or with another vanilla world of the same seed (vanilla's own run-to-run noise).
//
//   cargo run --release --example compare_vanilla -- <vanilla-data> <regionA> <seed>
//   cargo run --release --example compare_vanilla -- <vanilla-data> <regionA> <regionB>
//
// Only chunks the vanilla server finished (Status "minecraft:full") are compared.

use anyhow::{Context, Result};
use horizon_world::{
    chunk::BlockSection,
    nbt::{Limits, Tag},
    region::RegionFile,
    registry::{BlockRegistry, BlockStateId},
};
use minerust::world::{ChunkSource, vanilla::VanillaWorld};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

type Sections = BTreeMap<i32, Vec<u16>>;

// Blocks the noise stage places; everything else comes from decoration.
const TERRAIN: &[&str] = &[
    "minecraft:stone",
    "minecraft:deepslate",
    "minecraft:water",
    "minecraft:lava",
    "minecraft:air",
    "minecraft:cave_air",
    "minecraft:bedrock",
    "minecraft:grass_block",
    "minecraft:dirt",
    "minecraft:sand",
    "minecraft:gravel",
];

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let data = PathBuf::from(args.next().context("vanilla-data folder")?);
    let regions = PathBuf::from(args.next().context("region folder")?);
    let other = args.next().context("seed or second region folder")?;

    let facts = std::fs::read(data.join("facts.json"))?;
    let registry: &'static BlockRegistry = Box::leak(Box::new(BlockRegistry::from_facts_json(&facts[..])?));
    let reference = load_full_chunks(&regions, registry)?;

    let mut against: HashMap<(i32, i32), Sections> = HashMap::new();
    if Path::new(&other).is_dir() {
        against = load_full_chunks(Path::new(&other), registry)?;
    } else {
        let world = VanillaWorld::open(&data, other.parse()?, std::thread::available_parallelism()?.get())?;
        for (&(x, z), sections) in &reference {
            let chunk = world.get(x, z).await?;
            let mut ours = Sections::new();
            for &sy in sections.keys() {
                let mut states = vec![0u16; 4096];
                for (i, s) in states.iter_mut().enumerate() {
                    *s = chunk.block(i & 15, sy * 16 + (i >> 8) as i32, (i >> 4) & 15);
                }
                ours.insert(sy, states);
            }
            against.insert((x, z), ours);
        }
    }

    let name = |id: u16| registry.state(BlockStateId(id as u32)).map(|s| s.name.to_string()).unwrap_or_default();
    let mut compared = 0usize;
    let mut blocks = 0usize;
    let (mut terrain_total, mut terrain_same) = (0usize, 0usize);
    let mut diffs: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (pos, sections) in &reference {
        let Some(theirs) = against.get(pos) else { continue };
        compared += 1;
        for (sy, a) in sections {
            let Some(b) = theirs.get(sy) else { continue };
            for i in 0..4096 {
                blocks += 1;
                let (na, nb) = (name(a[i]), name(b[i]));
                if TERRAIN.contains(&na.as_str()) {
                    terrain_total += 1;
                    if a[i] == b[i] {
                        terrain_same += 1;
                    }
                }
                if a[i] != b[i] {
                    *diffs.entry((na, nb)).or_default() += 1;
                }
            }
        }
    }

    let total: usize = diffs.values().sum();
    println!("compared {compared} chunks, {blocks} blocks");
    println!("identical blocks: {:.4}%", 100.0 * (blocks - total) as f64 / blocks.max(1) as f64);
    println!("terrain blocks identical: {:.4}%", 100.0 * terrain_same as f64 / terrain_total.max(1) as f64);
    let mut by_count: Vec<_> = diffs.into_iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(&a.1));
    for ((a, b), n) in by_count.into_iter().take(15) {
        println!("{n:>8}  {a} -> {b}");
    }
    Ok(())
}

fn load_full_chunks(dir: &Path, registry: &BlockRegistry) -> Result<HashMap<(i32, i32), Sections>> {
    let mut out = HashMap::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "mca") {
            continue;
        }
        let mut region = RegionFile::open(&path)?;
        let positions: Vec<_> = region.chunks().map(|(pos, _)| pos).collect();
        for pos in positions {
            let Some(record) = region.read(pos)? else { continue };
            let root = record.nbt(Limits::default())?.tag;
            let full = matches!(root.get("Status"), Some(Tag::String(s)) if s.to_string_lossy() == "minecraft:full");
            let Some(Tag::List(list)) = root.get("sections") else { continue };
            if !full {
                continue;
            }
            let mut sections = Sections::new();
            for section in list {
                let (Some(Tag::Byte(sy)), Some(states)) = (section.get("Y"), section.get("block_states")) else {
                    continue;
                };
                let blocks = BlockSection::from_nbt(states, registry)?;
                let ids = (0..4096).map(|i| blocks.get(i).map(|s| s.0 as u16)).collect::<Result<_, _>>()?;
                sections.insert(*sy as i32, ids);
            }
            out.insert((pos.x, pos.z), sections);
        }
    }
    Ok(out)
}
