// Configuration-phase packets recorded from a vanilla 26.3 server, see
// tools/capture_vanilla_config.py.

use super::codec::Reader;
use anyhow::{Context, Result, bail, ensure};

static CAPTURE: &[u8] = include_bytes!("../../data/vanilla_config.bin");
// The same registries with every entry's NBT body, for clients that don't have the
// vanilla pack (modded or third-party clients). Recorded with `--full`.
static CAPTURE_FULL: &[u8] = include_bytes!("../../data/vanilla_config_full.bin");

const CB_REGISTRY_DATA: i32 = 0x07;
const CB_UPDATE_ENABLED_FEATURES: i32 = 0x0D;
const CB_UPDATE_TAGS: i32 = 0x0E;
const CB_SELECT_KNOWN_PACKS: i32 = 0x0F;

pub struct CapturedPacket {
    pub id: i32,
    pub body: &'static [u8],
}

pub struct VanillaData {
    pub enabled_features: CapturedPacket,
    pub select_known_packs: CapturedPacket,
    /// registry_data packets followed by update_tags, in vanilla's order.
    pub registries_and_tags: Vec<CapturedPacket>,
    /// The same, with full entries, for clients that answered select_known_packs differently.
    pub full_registries_and_tags: Vec<CapturedPacket>,
    pub overworld_dimension_type: i32,
    pub nether_dimension_type: i32,
    pub overworld_clock: i32,
    /// worldgen/biome entries in registry order; a biome's network id is its index.
    pub biome_names: Vec<String>,
}

impl VanillaData {
    pub fn load() -> Result<Self> {
        let mut rd = Reader::new(CAPTURE);
        let mut enabled_features = None;
        let mut select_known_packs = None;
        let mut registries_and_tags = Vec::new();
        let mut overworld_dimension_type = None;
        let mut nether_dimension_type = None;
        let mut overworld_clock = None;
        let mut biome_names = Vec::new();

        while !rd.is_empty() {
            let id = rd.varint()?;
            let len = rd.varint()? as usize;
            let body = rd.bytes(len)?;
            let packet = CapturedPacket { id, body };
            match id {
                CB_UPDATE_ENABLED_FEATURES => enabled_features = Some(packet),
                CB_SELECT_KNOWN_PACKS => select_known_packs = Some(packet),
                CB_REGISTRY_DATA => {
                    let (registry, entries) = registry_entries(body)?;
                    let index = |name: &str| entries.iter().position(|e| e == name).map(|i| i as i32);
                    match registry.as_str() {
                        "minecraft:dimension_type" => {
                            overworld_dimension_type = index("minecraft:overworld");
                            nether_dimension_type = index("minecraft:the_nether");
                        }
                        "minecraft:world_clock" => overworld_clock = index("minecraft:overworld"),
                        "minecraft:worldgen/biome" => biome_names = entries.clone(),
                        _ => {}
                    }
                    registries_and_tags.push(packet);
                }
                CB_UPDATE_TAGS => registries_and_tags.push(packet),
                other => bail!("unexpected packet {other} in capture"),
            }
        }

        Ok(Self {
            enabled_features: enabled_features.context("capture lacks update_enabled_features")?,
            select_known_packs: select_known_packs.context("capture lacks select_known_packs")?,
            registries_and_tags,
            full_registries_and_tags: full_registries()?,
            overworld_dimension_type: overworld_dimension_type.context("no minecraft:overworld dimension type")?,
            nether_dimension_type: nether_dimension_type.context("no minecraft:the_nether dimension type")?,
            overworld_clock: overworld_clock.context("no minecraft:overworld world clock")?,
            biome_names,
        })
    }
}

impl VanillaData {
    pub fn biome_id(&self, name: &str) -> Option<i32> {
        self.biome_names.iter().position(|n| n == name).map(|i| i as i32)
    }
}

fn full_registries() -> Result<Vec<CapturedPacket>> {
    let mut rd = Reader::new(CAPTURE_FULL);
    let mut packets = Vec::new();
    while !rd.is_empty() {
        let id = rd.varint()?;
        let len = rd.varint()? as usize;
        let body = rd.bytes(len)?;
        if matches!(id, CB_REGISTRY_DATA | CB_UPDATE_TAGS) {
            packets.push(CapturedPacket { id, body });
        }
    }
    ensure!(packets.last().is_some_and(|p| p.id == CB_UPDATE_TAGS), "full capture lacks update_tags");
    Ok(packets)
}

/// Entry names of a registry_data packet. The capture negotiated known packs,
/// so entries carry no NBT body.
fn registry_entries(body: &[u8]) -> Result<(String, Vec<String>)> {
    let mut rd = Reader::new(body);
    let registry = rd.string(32767)?;
    let count = rd.varint()?;
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        entries.push(rd.string(32767)?);
        if rd.bool()? {
            bail!("{registry} entry carries data; recapture with known packs accepted");
        }
    }
    Ok((registry, entries))
}

#[cfg(test)]
mod tests {
    #[test]
    fn capture_parses() {
        let data = super::VanillaData::load().unwrap();
        assert_eq!(data.registries_and_tags.last().unwrap().id, super::CB_UPDATE_TAGS);
        assert!(data.registries_and_tags.len() > 30);
        assert!(data.biome_id("minecraft:plains").is_some());
        // One registry_data per synced registry plus update_tags, in both variants.
        assert_eq!(data.full_registries_and_tags.len(), data.registries_and_tags.len());
    }
}
