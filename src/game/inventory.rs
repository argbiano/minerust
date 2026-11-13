// A player's inventory, kept by the game loop. Slots are numbered as in the client's
// inventory screen (container 0): 0 crafting result, 1-4 crafting grid, 5-8 armour,
// 9-35 main, 36-44 hotbar, 45 offhand. The client only predicts; every change is decided
// here and the whole inventory is sent back, so a client can't conjure items.

use crate::world::registry;

pub const SLOTS: usize = 46;
pub const ARMOR: std::ops::Range<usize> = 5..9;
pub const MAIN: std::ops::Range<usize> = 9..36;
pub const HOTBAR: std::ops::Range<usize> = 36..45;
pub const OFFHAND: usize = 45;
/// The slot number clicks outside the inventory window carry.
pub const OUTSIDE: i16 = -999;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stack {
    pub item: u16,
    pub count: u8,
}

impl Stack {
    pub fn new(item: u16, count: u8) -> Self {
        Self { item, count }
    }

    pub fn max(self) -> u8 {
        registry::item(self.item).map_or(64, |i| i.max_stack.max(1))
    }

    pub fn name(self) -> &'static str {
        registry::item(self.item).map_or("air", |i| i.name)
    }

    fn with_count(self, count: u8) -> Option<Self> {
        (count > 0).then_some(Self { count, ..self })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hand {
    Main,
    Off,
}

/// container_click's modes, in ContainerInput order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickMode {
    Pickup,
    QuickMove,
    Swap,
    Clone,
    Throw,
    QuickCraft,
    PickupAll,
}

impl ClickMode {
    pub fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            0 => Self::Pickup,
            1 => Self::QuickMove,
            2 => Self::Swap,
            3 => Self::Clone,
            4 => Self::Throw,
            5 => Self::QuickCraft,
            6 => Self::PickupAll,
            _ => return None,
        })
    }
}

/// An item drag in progress (QUICK_CRAFT): 0 splits evenly, 1 one each, 2 full stacks (creative).
#[derive(Clone, Debug)]
struct Drag {
    kind: u8,
    slots: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct Inventory {
    pub slots: [Option<Stack>; SLOTS],
    /// Hotbar index, 0-8.
    pub selected: usize,
    /// What the cursor holds while the inventory screen is open.
    pub carried: Option<Stack>,
    pub state_id: i32,
    drag: Option<Drag>,
}

impl Default for Inventory {
    fn default() -> Self {
        Self { slots: [None; SLOTS], selected: 0, carried: None, state_id: 0, drag: None }
    }
}

impl Inventory {
    pub fn hand_slot(&self, hand: Hand) -> usize {
        match hand {
            Hand::Main => HOTBAR.start + self.selected,
            Hand::Off => OFFHAND,
        }
    }

    pub fn held(&self, hand: Hand) -> Option<Stack> {
        self.slots[self.hand_slot(hand)]
    }

    /// Takes `n` from the held stack.
    pub fn consume(&mut self, hand: Hand, n: u8) {
        let slot = self.hand_slot(hand);
        self.slots[slot] = self.slots[slot].and_then(|s| s.with_count(s.count.saturating_sub(n)));
    }

    pub fn set_held(&mut self, hand: Hand, stack: Option<Stack>) {
        let slot = self.hand_slot(hand);
        self.slots[slot] = stack;
    }

    /// Order vanilla fills the inventory in: hotbar first, then the main rows.
    fn storage() -> impl Iterator<Item = usize> {
        HOTBAR.chain(MAIN)
    }

    /// Adds as much of `stack` as fits, like picking an item up: onto matching stacks
    /// (held item and offhand first), then into empty slots. Returns what didn't fit.
    pub fn insert(&mut self, stack: Stack) -> Option<Stack> {
        let mut left = stack.count;
        let first = [self.hand_slot(Hand::Main), OFFHAND];
        for slot in first.into_iter().chain(Self::storage()) {
            if left == 0 {
                break;
            }
            if let Some(s) = &mut self.slots[slot]
                && s.item == stack.item
            {
                let moved = left.min(stack.max().saturating_sub(s.count));
                s.count += moved;
                left -= moved;
            }
        }
        for slot in Self::storage() {
            if left == 0 {
                break;
            }
            if self.slots[slot].is_none() {
                let moved = left.min(stack.max());
                self.slots[slot] = Some(Stack { count: moved, ..stack });
                left -= moved;
            }
        }
        stack.with_count(left)
    }

    /// Everything in the inventory, emptying it (death drops).
    pub fn take_all(&mut self) -> Vec<Stack> {
        let mut out: Vec<Stack> = self.slots.iter_mut().filter_map(Option::take).collect();
        out.extend(self.carried.take());
        self.drag = None;
        out
    }

    /// Puts the cursor's stack back when the inventory screen closes. Returns what didn't fit.
    pub fn close(&mut self) -> Option<Stack> {
        self.drag = None;
        self.carried.take().and_then(|s| self.insert(s))
    }

    /// Whether `stack` may go in `slot` by hand.
    fn may_place(slot: usize, stack: Stack) -> bool {
        if ARMOR.contains(&slot) {
            let name = stack.name();
            return match slot - ARMOR.start {
                0 => {
                    name.ends_with("_helmet")
                        || name.ends_with("_head")
                        || name.ends_with("_skull")
                        || name == "carved_pumpkin"
                }
                1 => name.ends_with("_chestplate") || name == "elytra",
                2 => name.ends_with("_leggings"),
                _ => name.ends_with("_boots"),
            };
        }
        slot >= ARMOR.end
    }

    fn slot_max(slot: usize, stack: Stack) -> u8 {
        if ARMOR.contains(&slot) { 1 } else { stack.max() }
    }

    /// A container_click on the inventory screen. Returns stacks thrown out of it.
    pub fn click(&mut self, slot: i16, button: u8, mode: ClickMode, creative: bool) -> Vec<Stack> {
        let mut thrown = Vec::new();
        if mode != ClickMode::QuickCraft {
            self.drag = None;
        }
        let index = usize::try_from(slot).ok().filter(|s| (ARMOR.start..SLOTS).contains(s));
        match mode {
            ClickMode::Pickup if slot == OUTSIDE => {
                if let Some(c) = self.carried {
                    let n = if button == 0 { c.count } else { 1 };
                    thrown.push(Stack { count: n, ..c });
                    self.carried = c.with_count(c.count - n);
                }
            }
            ClickMode::Pickup => {
                if let Some(i) = index
                    && button <= 1
                {
                    self.pickup(i, button == 1);
                }
            }
            ClickMode::QuickMove => {
                if let Some(i) = index {
                    self.quick_move(i);
                }
            }
            ClickMode::Swap => {
                let other = match button {
                    0..=8 => Some(HOTBAR.start + button as usize),
                    40 => Some(OFFHAND),
                    _ => None,
                };
                if let (Some(i), Some(o)) = (index, other) {
                    let (a, b) = (self.slots[i], self.slots[o]);
                    let fits = |slot: usize, s: Option<Stack>| {
                        s.is_none_or(|s| Self::may_place(slot, s) && s.count <= Self::slot_max(slot, s))
                    };
                    if fits(i, b) && fits(o, a) {
                        self.slots[i] = b;
                        self.slots[o] = a;
                    }
                }
            }
            ClickMode::Clone => {
                if creative
                    && self.carried.is_none()
                    && let Some(s) = index.and_then(|i| self.slots[i])
                {
                    self.carried = Some(Stack { count: s.max(), ..s });
                }
            }
            ClickMode::Throw => {
                if self.carried.is_none()
                    && let Some(i) = index
                    && let Some(s) = self.slots[i]
                {
                    let n = if button == 1 { s.count } else { 1 };
                    thrown.push(Stack { count: n, ..s });
                    self.slots[i] = s.with_count(s.count - n);
                }
            }
            ClickMode::QuickCraft => self.drag_step(slot, index, button, creative),
            ClickMode::PickupAll => {
                if let Some(mut c) = self.carried {
                    // Partial stacks first, as vanilla does, then full ones.
                    for full in [false, true] {
                        for i in ARMOR.end..SLOTS {
                            if c.count >= c.max() {
                                break;
                            }
                            let Some(s) = self.slots[i] else { continue };
                            if s.item != c.item || (s.count >= s.max()) != full {
                                continue;
                            }
                            let moved = s.count.min(c.max() - c.count);
                            c.count += moved;
                            self.slots[i] = s.with_count(s.count - moved);
                        }
                    }
                    self.carried = Some(c);
                }
            }
        }
        thrown
    }

    fn pickup(&mut self, i: usize, right: bool) {
        let slot = self.slots[i];
        match (slot, self.carried) {
            (None, None) => {}
            (Some(s), None) => {
                let take = if right { s.count.div_ceil(2) } else { s.count };
                self.carried = Some(Stack { count: take, ..s });
                self.slots[i] = s.with_count(s.count - take);
            }
            (None, Some(c)) => {
                if Self::may_place(i, c) {
                    let put = if right { 1 } else { c.count.min(Self::slot_max(i, c)) };
                    self.slots[i] = Some(Stack { count: put, ..c });
                    self.carried = c.with_count(c.count - put);
                }
            }
            (Some(s), Some(c)) if s.item == c.item => {
                let room = Self::slot_max(i, s).saturating_sub(s.count);
                let put = if right { 1.min(room) } else { c.count.min(room) };
                self.slots[i] = Some(Stack { count: s.count + put, ..s });
                self.carried = c.with_count(c.count - put);
            }
            (Some(s), Some(c)) => {
                if Self::may_place(i, c) && c.count <= Self::slot_max(i, c) {
                    self.slots[i] = Some(c);
                    self.carried = Some(s);
                }
            }
        }
    }

    /// Shift-click: hotbar to main rows and back, armour to its slot, the rest to storage.
    fn quick_move(&mut self, i: usize) {
        let Some(mut s) = self.slots[i] else { return };
        let armor_slot = ARMOR.clone().find(|&a| Self::may_place(a, s) && self.slots[a].is_none());
        let targets: Vec<usize> = if let (Some(a), false) = (armor_slot, ARMOR.contains(&i)) {
            vec![a]
        } else if HOTBAR.contains(&i) {
            MAIN.collect()
        } else if MAIN.contains(&i) {
            HOTBAR.collect()
        } else {
            MAIN.chain(HOTBAR).collect()
        };
        for pass_empty in [false, true] {
            for &t in &targets {
                if s.count == 0 {
                    break;
                }
                let max = Self::slot_max(t, s);
                match self.slots[t] {
                    Some(mut d) if !pass_empty && d.item == s.item => {
                        let moved = s.count.min(max.saturating_sub(d.count));
                        d.count += moved;
                        s.count -= moved;
                        self.slots[t] = Some(d);
                    }
                    None if pass_empty => {
                        let moved = s.count.min(max);
                        self.slots[t] = Some(Stack { count: moved, ..s });
                        s.count -= moved;
                    }
                    _ => {}
                }
            }
        }
        self.slots[i] = s.with_count(s.count);
    }

    fn drag_step(&mut self, slot: i16, index: Option<usize>, button: u8, creative: bool) {
        let (stage, kind) = (button & 3, (button >> 2) & 3);
        match stage {
            0 => {
                let allowed = kind < 2 || (kind == 2 && creative);
                self.drag =
                    (allowed && slot == OUTSIDE && self.carried.is_some()).then(|| Drag { kind, slots: Vec::new() });
            }
            1 => {
                let (Some(drag), Some(c), Some(i)) = (&mut self.drag, self.carried, index) else {
                    self.drag = None;
                    return;
                };
                let fits = self.slots[i].is_none_or(|s| s.item == c.item) && Self::may_place(i, c);
                if drag.kind == kind
                    && fits
                    && !drag.slots.contains(&i)
                    && (kind == 2 || drag.slots.len() < c.count as usize)
                {
                    drag.slots.push(i);
                }
            }
            2 => {
                let Some(drag) = self.drag.take() else { return };
                let Some(mut c) = self.carried else { return };
                if drag.kind != kind || drag.slots.is_empty() {
                    return;
                }
                let each = match kind {
                    0 => (c.count as usize / drag.slots.len()).max(1) as u8,
                    1 => 1,
                    _ => c.max(),
                };
                for i in drag.slots {
                    let have = self.slots[i].map_or(0, |s| s.count);
                    let room = Self::slot_max(i, c).saturating_sub(have);
                    let put = if kind == 2 { room.min(each) } else { each.min(room).min(c.count) };
                    if put > 0 {
                        self.slots[i] = Some(Stack { count: have + put, ..c });
                        if kind != 2 {
                            c.count -= put;
                        }
                    }
                }
                self.carried = c.with_count(c.count);
            }
            _ => self.drag = None,
        }
    }

    /// set_creative_mode_slot: creative players may put anything anywhere. Returns a stack
    /// to drop for slot -1.
    pub fn set_creative(&mut self, slot: i16, stack: Option<Stack>) -> Option<Stack> {
        let stack = stack.filter(|s| registry::item(s.item).is_some() && s.item != 0 && s.count > 0);
        let stack = stack.map(|s| Stack { count: s.count.min(s.max()), ..s });
        match usize::try_from(slot) {
            Ok(i) if (1..SLOTS).contains(&i) => {
                self.slots[i] = stack;
                None
            }
            _ if slot == -1 => stack,
            _ => None,
        }
    }

    /// Where pick-block should put an item that isn't in the hotbar.
    fn suitable_hotbar_slot(&self) -> usize {
        if self.slots[HOTBAR.start + self.selected].is_none() {
            return self.selected;
        }
        (0..9)
            .map(|k| (self.selected + k) % 9)
            .find(|&h| self.slots[HOTBAR.start + h].is_none())
            .unwrap_or(self.selected)
    }

    /// Middle-click on a block: selects the item if the hotbar has it, swaps it in from the
    /// main rows, or (creative) conjures it. Returns whether anything changed.
    pub fn pick(&mut self, item: u16, creative: bool) -> bool {
        if let Some(h) = (0..9).find(|&h| self.slots[HOTBAR.start + h].is_some_and(|s| s.item == item)) {
            self.selected = h;
            return true;
        }
        let target = self.suitable_hotbar_slot();
        let t = HOTBAR.start + target;
        if let Some(m) = MAIN.clone().find(|&m| self.slots[m].is_some_and(|s| s.item == item)) {
            self.slots.swap(m, t);
        } else if creative {
            if let Some(old) = self.slots[t] {
                // Vanilla keeps what was there if there's room for it.
                self.slots[t] = None;
                if let Some(free) = MAIN.clone().find(|&m| self.slots[m].is_none()) {
                    self.slots[free] = Some(old);
                }
            }
            self.slots[t] = Some(Stack::new(item, 1));
        } else {
            return false;
        }
        self.selected = target;
        true
    }

    pub fn swap_hands(&mut self) {
        let main = self.hand_slot(Hand::Main);
        self.slots.swap(main, OFFHAND);
    }

    /// Drops from the held stack (the Q key): one, or all of it.
    pub fn drop_held(&mut self, all: bool) -> Option<Stack> {
        let s = self.held(Hand::Main)?;
        let n = if all { s.count } else { 1 };
        self.consume(Hand::Main, n);
        Some(Stack { count: n, ..s })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> u16 {
        registry::item_by_name(name).unwrap()
    }

    fn total(inv: &Inventory, item: u16) -> u32 {
        inv.slots.iter().chain([&inv.carried]).flatten().filter(|s| s.item == item).map(|s| s.count as u32).sum()
    }

    #[test]
    fn insert_fills_matching_stacks_then_hotbar_then_main() {
        let stone = id("stone");
        let mut inv = Inventory::default();
        assert_eq!(inv.insert(Stack::new(stone, 70)), None);
        assert_eq!(inv.slots[36], Some(Stack::new(stone, 64)));
        assert_eq!(inv.slots[37], Some(Stack::new(stone, 6)));
        // Matching stacks first.
        assert_eq!(inv.insert(Stack::new(stone, 10)), None);
        assert_eq!(inv.slots[37], Some(Stack::new(stone, 16)));
        // Unstackables take a slot each.
        let pick = id("diamond_pickaxe");
        inv.insert(Stack::new(pick, 1));
        inv.insert(Stack::new(pick, 1));
        assert_eq!(inv.slots[38], Some(Stack::new(pick, 1)));
        assert_eq!(inv.slots[39], Some(Stack::new(pick, 1)));
        // A full inventory hands back the rest.
        let mut full = Inventory::default();
        assert_eq!(full.insert(Stack::new(stone, 255)), None);
        for i in HOTBAR.chain(MAIN) {
            full.slots[i] = Some(Stack::new(pick, 1));
        }
        assert_eq!(full.insert(Stack::new(stone, 5)), Some(Stack::new(stone, 5)));
    }

    #[test]
    fn clicks_never_create_items() {
        let (dirt, stone) = (id("dirt"), id("stone"));
        let mut inv = Inventory::default();
        inv.slots[36] = Some(Stack::new(dirt, 33));
        inv.slots[10] = Some(Stack::new(dirt, 40));
        inv.slots[11] = Some(Stack::new(stone, 5));
        // Right-click takes half, rounding up.
        inv.click(36, 1, ClickMode::Pickup, false);
        assert_eq!(inv.carried, Some(Stack::new(dirt, 17)));
        assert_eq!(inv.slots[36], Some(Stack::new(dirt, 16)));
        // Left-click onto a matching stack fills it to 64.
        inv.click(10, 0, ClickMode::Pickup, false);
        assert_eq!(inv.slots[10], Some(Stack::new(dirt, 57)));
        assert_eq!(inv.carried, None);
        // Swapping different items.
        inv.click(11, 0, ClickMode::Pickup, false);
        inv.click(10, 0, ClickMode::Pickup, false);
        assert_eq!(inv.slots[10], Some(Stack::new(stone, 5)));
        assert_eq!(inv.carried, Some(Stack::new(dirt, 57)));
        // A drag over three slots splits evenly and keeps the remainder.
        inv.click(OUTSIDE, 0, ClickMode::QuickCraft, false);
        for s in [20, 21, 22] {
            inv.click(s, 1, ClickMode::QuickCraft, false);
        }
        inv.click(OUTSIDE, 2, ClickMode::QuickCraft, false);
        assert_eq!(inv.slots[20], Some(Stack::new(dirt, 19)));
        assert_eq!(inv.carried, None);
        assert_eq!(total(&inv, dirt), 73);
        // Double-click gathers them back.
        inv.click(20, 0, ClickMode::Pickup, false);
        inv.click(20, 0, ClickMode::PickupAll, false);
        assert_eq!(inv.carried, Some(Stack::new(dirt, 64)));
        assert_eq!(total(&inv, dirt), 73);
        // Clone only works in creative.
        inv.carried = None;
        inv.click(10, 2, ClickMode::Clone, false);
        assert_eq!(inv.carried, None);
        inv.click(10, 2, ClickMode::Clone, true);
        assert_eq!(inv.carried, Some(Stack::new(stone, 64)));
    }

    #[test]
    fn throwing_and_outside_clicks_drop() {
        let dirt = id("dirt");
        let mut inv = Inventory::default();
        inv.slots[9] = Some(Stack::new(dirt, 10));
        assert_eq!(inv.click(9, 0, ClickMode::Throw, false), vec![Stack::new(dirt, 1)]);
        assert_eq!(inv.click(9, 1, ClickMode::Throw, false), vec![Stack::new(dirt, 9)]);
        assert_eq!(inv.slots[9], None);
        inv.carried = Some(Stack::new(dirt, 4));
        assert_eq!(inv.click(OUTSIDE, 1, ClickMode::Pickup, false), vec![Stack::new(dirt, 1)]);
        assert_eq!(inv.click(OUTSIDE, 0, ClickMode::Pickup, false), vec![Stack::new(dirt, 3)]);
        assert_eq!(inv.carried, None);
    }

    #[test]
    fn armor_slots_and_shift_click() {
        let (helmet, dirt) = (id("iron_helmet"), id("dirt"));
        let mut inv = Inventory::default();
        inv.slots[9] = Some(Stack::new(dirt, 5));
        inv.click(9, 0, ClickMode::Pickup, false);
        inv.click(5, 0, ClickMode::Pickup, false);
        assert_eq!(inv.slots[5], None, "dirt is no helmet");
        inv.click(9, 0, ClickMode::Pickup, false);
        inv.slots[40] = Some(Stack::new(helmet, 1));
        inv.click(40, 0, ClickMode::QuickMove, false);
        assert_eq!(inv.slots[5], Some(Stack::new(helmet, 1)));
        inv.click(9, 0, ClickMode::QuickMove, false);
        assert_eq!(inv.slots[36], Some(Stack::new(dirt, 5)));
        // Hotbar swap refuses to put dirt on the head.
        inv.click(5, 0, ClickMode::Swap, false);
        assert_eq!(inv.slots[5], Some(Stack::new(helmet, 1)));
    }

    #[test]
    fn pick_block_and_hands() {
        let (stone, dirt) = (id("stone"), id("dirt"));
        let mut inv = Inventory::default();
        inv.slots[36] = Some(Stack::new(dirt, 3));
        inv.slots[12] = Some(Stack::new(stone, 8));
        assert!(inv.pick(stone, false));
        assert_eq!(inv.selected, 1);
        assert_eq!(inv.held(Hand::Main), Some(Stack::new(stone, 8)));
        assert!(!inv.pick(id("glass"), false));
        assert!(inv.pick(id("glass"), true));
        inv.swap_hands();
        assert_eq!(inv.held(Hand::Off).map(|s| s.item), Some(id("glass")));
        assert_eq!(inv.drop_held(false), None);
        inv.selected = 0;
        assert_eq!(inv.drop_held(true), Some(Stack::new(dirt, 3)));
    }
}
