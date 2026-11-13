// Blocks and items as the vanilla server defines them (see tools/ExportRegistry.java), and
// working with block states through their properties.

pub use super::registry_data::{BLOCKS, ITEMS};
use std::{collections::HashMap, sync::OnceLock};

pub const NONE: u16 = u16::MAX;

pub struct Prop {
    pub name: &'static str,
    pub values: &'static [&'static str],
}

pub struct Block {
    pub name: &'static str,
    /// The first state id; a block's states follow, the last property varying fastest.
    pub first: u16,
    pub default: u16,
    pub props: &'static [Prop],
    /// The item that places this block (`NONE` for air, liquids...).
    pub item: u16,
    /// What breaking it drops without silk touch or shears, and how many.
    pub drop: u16,
    pub drop_count: u8,
    /// Drops nothing unless broken with the right tool.
    pub needs_tool: bool,
    /// Bits for the tools that mine it: 1 pickaxe, 2 axe, 4 shovel, 8 hoe.
    pub tools: u8,
    /// Bits for tool materials too weak for it, in `MATERIALS` order.
    pub incorrect: u8,
    /// Placing a block here replaces it (air, liquids, short grass...).
    pub replaceable: bool,
}

pub struct Item {
    pub name: &'static str,
    pub max_stack: u8,
    /// Default state of the block it places, or `NONE`.
    pub block: u16,
    /// For torches, signs and the like: the default state of the wall-mounted variant.
    pub wall: u16,
}

const TOOLS: [&str; 4] = ["pickaxe", "axe", "shovel", "hoe"];
const MATERIALS: [&str; 7] = ["wooden", "stone", "copper", "iron", "golden", "diamond", "netherite"];

impl Block {
    fn strides(&self) -> impl Iterator<Item = (&'static Prop, u16)> + '_ {
        let mut stride = 1u16;
        let mut out: Vec<(&'static Prop, u16)> = Vec::with_capacity(self.props.len());
        for p in self.props.iter().rev() {
            out.push((p, stride));
            stride *= p.values.len() as u16;
        }
        out.into_iter().rev()
    }

    pub fn state_count(&self) -> u16 {
        self.props.iter().map(|p| p.values.len() as u16).product()
    }

    pub fn has(&self, prop: &str) -> bool {
        self.props.iter().any(|p| p.name == prop)
    }

    /// The value of `prop` in `state` (one of this block's states).
    pub fn get(&self, state: u16, prop: &str) -> Option<&'static str> {
        let index = state.checked_sub(self.first)?;
        self.strides()
            .find(|(p, _)| p.name == prop)
            .map(|(p, stride)| p.values[(index / stride) as usize % p.values.len()])
    }

    /// `state` with `prop` set to `value`, or unchanged if the block has no such property or value.
    pub fn with(&self, state: u16, prop: &str, value: &str) -> u16 {
        let Some(index) = state.checked_sub(self.first) else { return state };
        let Some((p, stride)) = self.strides().find(|(p, _)| p.name == prop) else { return state };
        let Some(new) = p.values.iter().position(|v| *v == value) else { return state };
        let old = (index / stride) % p.values.len() as u16;
        self.first + index - old * stride + new as u16 * stride
    }

    /// Whether a held item (by name) gets this block's drop.
    pub fn harvestable_with(&self, held: Option<&str>) -> bool {
        if !self.needs_tool {
            return true;
        }
        let Some((material, tool)) = held.and_then(|h| h.split_once('_')) else { return false };
        let (Some(t), Some(m)) = (TOOLS.iter().position(|t| *t == tool), MATERIALS.iter().position(|m| *m == material))
        else {
            return false;
        };
        self.tools & (1 << t) != 0 && self.incorrect & (1 << m) == 0
    }
}

/// The block a state belongs to.
pub fn block_of(state: u16) -> Option<&'static Block> {
    let i = BLOCKS.partition_point(|b| b.first <= state).checked_sub(1)?;
    let b = &BLOCKS[i];
    (state < b.first + b.state_count()).then_some(b)
}

pub fn item(id: u16) -> Option<&'static Item> {
    ITEMS.get(id as usize)
}

/// Blocks (as indexes into `BLOCKS`) and items by name.
type Names = (HashMap<&'static str, usize>, HashMap<&'static str, u16>);

fn names() -> &'static Names {
    static NAMES: OnceLock<Names> = OnceLock::new();
    NAMES.get_or_init(|| {
        let blocks = BLOCKS.iter().enumerate().map(|(i, b)| (b.name, i)).collect();
        let items = ITEMS.iter().enumerate().map(|(i, it)| (it.name, i as u16)).collect();
        (blocks, items)
    })
}

pub fn block_by_name(name: &str) -> Option<&'static Block> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    names().0.get(name).map(|&i| &BLOCKS[i])
}

pub fn item_by_name(name: &str) -> Option<u16> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    names().1.get(name).copied()
}

/// A block state written as in commands: `name` or `name[prop=value,...]`.
pub fn parse_state(text: &str) -> Option<u16> {
    let (name, props) = match text.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']')?),
        None => (text, ""),
    };
    let block = block_by_name(name)?;
    let mut state = block.default;
    for pair in props.split(',').filter(|p| !p.trim().is_empty()) {
        let (k, v) = pair.split_once('=')?;
        let (k, v) = (k.trim(), v.trim());
        if block.get(state, k).is_none() || !block.props.iter().any(|p| p.name == k && p.values.contains(&v)) {
            return None;
        }
        state = block.with(state, k, v);
    }
    Some(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_and_properties() {
        let stairs = block_by_name("oak_stairs").unwrap();
        let s = stairs.with(stairs.default, "facing", "east");
        assert_eq!(stairs.get(s, "facing"), Some("east"));
        assert_eq!(stairs.get(s, "half"), stairs.get(stairs.default, "half"));
        assert_eq!(block_of(s).unwrap().name, "oak_stairs");
        let s = stairs.with(s, "half", "top");
        assert_eq!((stairs.get(s, "facing"), stairs.get(s, "half")), (Some("east"), Some("top")));
        assert_eq!(stairs.with(s, "nope", "x"), s);
        assert_eq!(block_of(0).unwrap().name, "air");
        assert_eq!(block_of(1).unwrap().name, "stone");
        // Ids from the 26.3 blocks report.
        assert_eq!(parse_state("minecraft:oak_log[axis=x]"), Some(139));
        assert_eq!(parse_state("fire[age=0,east=true,north=true,south=true,up=true,west=false]"), Some(4932));
        assert_eq!(parse_state("oak_log[axis=w]"), None);
    }

    #[test]
    fn drops_need_the_right_tool() {
        let stone = block_by_name("stone").unwrap();
        assert_eq!(item(stone.drop).unwrap().name, "cobblestone");
        assert!(!stone.harvestable_with(None));
        assert!(stone.harvestable_with(Some("wooden_pickaxe")));
        assert!(!stone.harvestable_with(Some("diamond_shovel")));
        let diamond = block_by_name("diamond_ore").unwrap();
        assert!(!diamond.harvestable_with(Some("stone_pickaxe")));
        assert!(diamond.harvestable_with(Some("iron_pickaxe")));
        assert!(block_by_name("dirt").unwrap().harvestable_with(None));
        assert_eq!(block_by_name("short_grass").unwrap().drop, NONE);
        assert!(block_by_name("short_grass").unwrap().replaceable && block_by_name("water").unwrap().replaceable);
    }

    #[test]
    fn items() {
        let torch = item(item_by_name("torch").unwrap()).unwrap();
        assert_eq!(block_of(torch.wall).unwrap().name, "wall_torch");
        assert_eq!(item(item_by_name("ender_pearl").unwrap()).unwrap().max_stack, 16);
        assert_eq!(item(item_by_name("diamond_pickaxe").unwrap()).unwrap().block, NONE);
    }
}
