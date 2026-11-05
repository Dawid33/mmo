//! The player inventory: 36 slots (hotbar `0..9`, main `9..36`) and the
//! Minecraft cursor-click rules (`click`). Authoritative sim state carried on
//! `Client`, so it follows the player across region handoffs.

use crate::{block_props, ItemId, ItemStack, MAX_STACK};

pub const INVENTORY_SLOTS: usize = 36;
pub const HOTBAR_SLOTS: usize = 9;
/// Chests in a new player's starting kit (slot 0).
pub const STARTING_CHESTS: u32 = 2;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, state_hash::StateHash, PartialEq)]
pub struct Inventory {
    pub slots: Vec<Option<ItemStack>>,
}

impl Default for Inventory {
    fn default() -> Self {
        Self::new()
    }
}

impl Inventory {
    pub fn new() -> Self {
        Self { slots: vec![None; INVENTORY_SLOTS] }
    }

    /// What a new player starts with: `STARTING_CHESTS` chests in slot 0
    /// (empty while the manifest has no `chest` block).
    pub fn starting_kit() -> Self {
        let mut inv = Self::new();
        if let Some(chest) = block_props().id_named("chest") {
            inv.slots[0] = Some(ItemStack { item: ItemId::block(chest), count: STARTING_CHESTS });
        }
        inv
    }

    /// Adds `stack`, topping up existing stacks first (hotbar then main),
    /// then filling empty slots in that order. Returns what did not fit.
    pub fn add(&mut self, stack: ItemStack) -> u32 {
        add_to(&mut self.slots, stack)
    }

    /// Takes up to `n` items from the selected hotbar slot.
    pub fn take_selected(&mut self, selected: u8, n: u32) -> Option<ItemStack> {
        let slot = self.slots.get_mut(selected as usize)?;
        let held = slot.as_mut()?;
        let take = n.min(held.count);
        if take == 0 {
            return None;
        }
        let item = held.item;
        held.count -= take;
        if held.count == 0 {
            *slot = None;
        }
        Some(ItemStack { item, count: take })
    }
}

fn add_to(slots: &mut [Option<ItemStack>], mut stack: ItemStack) -> u32 {
    for s in slots.iter_mut().flatten() {
        if stack.count == 0 {
            break;
        }
        if s.item == stack.item && s.count < MAX_STACK {
            let n = (MAX_STACK - s.count).min(stack.count);
            s.count += n;
            stack.count -= n;
        }
    }
    for s in slots.iter_mut() {
        if stack.count == 0 {
            break;
        }
        if s.is_none() {
            let n = stack.count.min(MAX_STACK);
            *s = Some(ItemStack { item: stack.item, count: n });
            stack.count -= n;
        }
    }
    stack.count
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SlotRef {
    Player(u8),
    Container(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ClickButton {
    Left,
    Right,
}

/// One click on `slot`, Minecraft rules. Out-of-range slots, and container
/// slots with no container, do nothing.
pub fn click(
    cursor: &mut Option<ItemStack>,
    player: &mut Inventory,
    container: Option<&mut Vec<Option<ItemStack>>>,
    slot: SlotRef,
    button: ClickButton,
    shift: bool,
) {
    let (is_container, idx) = match slot {
        SlotRef::Player(i) => (false, i as usize),
        SlotRef::Container(i) => (true, i as usize),
    };
    let mut container = container;
    if is_container && container.is_none() {
        return;
    }
    let len = if is_container { container.as_ref().map_or(0, |c| c.len()) } else { player.slots.len() };
    if idx >= len {
        return;
    }
    if shift {
        shift_move(player, container.as_deref_mut(), is_container, idx);
        return;
    }
    let target: &mut Option<ItemStack> = if is_container {
        &mut container.unwrap()[idx]
    } else {
        &mut player.slots[idx]
    };
    match button {
        ClickButton::Left => match (cursor.take(), target.take()) {
            (None, t) => *cursor = t,
            (Some(c), None) => *target = Some(c),
            (Some(mut c), Some(mut t)) if c.item == t.item => {
                let n = (MAX_STACK - t.count.min(MAX_STACK)).min(c.count);
                t.count += n;
                c.count -= n;
                *target = Some(t);
                *cursor = (c.count > 0).then_some(c);
            }
            (Some(c), Some(t)) => {
                *target = Some(c);
                *cursor = Some(t);
            }
        },
        ClickButton::Right => match (cursor.take(), target.take()) {
            (None, None) => {}
            (None, Some(mut t)) => {
                let take = t.count.div_ceil(2);
                t.count -= take;
                *cursor = Some(ItemStack { item: t.item, count: take });
                *target = (t.count > 0).then_some(t);
            }
            (Some(mut c), None) => {
                *target = Some(ItemStack { item: c.item, count: 1 });
                c.count -= 1;
                *cursor = (c.count > 0).then_some(c);
            }
            (Some(mut c), Some(mut t)) if c.item == t.item => {
                if t.count < MAX_STACK {
                    t.count += 1;
                    c.count -= 1;
                }
                *target = Some(t);
                *cursor = (c.count > 0).then_some(c);
            }
            (Some(c), Some(t)) => {
                *target = Some(c);
                *cursor = Some(t);
            }
        },
    }
}

/// Shift-click: move the whole stack to the other half; what does not fit
/// stays in its slot.
fn shift_move(
    player: &mut Inventory,
    container: Option<&mut Vec<Option<ItemStack>>>,
    is_container: bool,
    idx: usize,
) {
    if is_container {
        let Some(c) = container else { return };
        if let Some(stack) = c[idx].take() {
            let rest = add_to(&mut player.slots, stack);
            c[idx] = (rest > 0).then_some(ItemStack { item: stack.item, count: rest });
        }
        return;
    }
    let Some(stack) = player.slots[idx].take() else { return };
    let rest = if let Some(c) = container {
        add_to(c, stack)
    } else {
        let (hot, main) = player.slots.split_at_mut(HOTBAR_SLOTS);
        add_to(if idx < HOTBAR_SLOTS { main } else { hot }, stack)
    };
    player.slots[idx] = (rest > 0).then_some(ItemStack { item: stack.item, count: rest });
}
