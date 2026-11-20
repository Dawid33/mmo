//! The player inventory — the hotbar alone (owner, 2026-11-19: a player
//! carries only that until a backpack adds slots, which nothing does yet) —
//! and the Minecraft cursor-click rules (`click`). Authoritative sim state carried on
//! `Client`, so it follows the player across region handoffs.

use crate::{ItemId, ItemStack, MAX_STACK, STARTING_CHESTS, chest_block};

pub const HOTBAR_SLOTS: usize = 9;
/// Every slot a player has: `0..HOTBAR_SLOTS`, then the backpack's (none yet).
pub const INVENTORY_SLOTS: usize = HOTBAR_SLOTS;

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
        Self {
            slots: vec![None; INVENTORY_SLOTS],
        }
    }

    /// What a new player starts with: `STARTING_CHESTS` chests in slot 0
    /// (empty with a manifest that has no `chest` block).
    pub fn starting_kit() -> Self {
        let mut inv = Self::new();
        if let Some(chest) = chest_block() {
            inv.slots[0] = Some(ItemStack {
                item: ItemId::block(chest),
                count: STARTING_CHESTS,
            });
        }
        inv
    }

    /// Adds `stack`, topping up existing stacks first (hotbar then main),
    /// then filling empty slots in that order. Returns what did not fit.
    pub fn add(&mut self, stack: ItemStack) -> u32 {
        let n = self.slots.len().min(INVENTORY_SLOTS);
        add_to(&mut self.slots[..n], stack)
    }

    /// Brings an inventory saved with another slot count to
    /// [`INVENTORY_SLOTS`]: a short one is padded; the stacks past the end of
    /// a long one move into the slots a player has, and each emptied extra
    /// slot is dropped. What does not fit stays where it was — kept, unseen —
    /// and is tried again at the next call (`parked_client`: every restore
    /// and hand-over). Deterministic.
    pub fn fit(&mut self) {
        if self.slots.len() <= INVENTORY_SLOTS {
            self.slots.resize(INVENTORY_SLOTS, None);
            return;
        }
        let (kept, over) = self.slots.split_at_mut(INVENTORY_SLOTS);
        for slot in over.iter_mut() {
            if let Some(stack) = slot.take() {
                let rest = add_to(kept, stack);
                *slot = (rest > 0).then_some(ItemStack {
                    item: stack.item,
                    count: rest,
                });
            }
        }
        while self.slots.len() > INVENTORY_SLOTS && matches!(self.slots.last(), Some(None)) {
            self.slots.pop();
        }
    }

    /// Takes up to `n` items from the selected hotbar slot.
    pub fn take_selected(&mut self, selected: u8, n: u32) -> Option<ItemStack> {
        if selected as usize >= HOTBAR_SLOTS {
            return None;
        }
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
            *s = Some(ItemStack {
                item: stack.item,
                count: n,
            });
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
    let len = if is_container {
        container.as_ref().map_or(0, |c| c.len())
    } else {
        player.slots.len()
    };
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
                *cursor = Some(ItemStack {
                    item: t.item,
                    count: take,
                });
                *target = (t.count > 0).then_some(t);
            }
            (Some(mut c), None) => {
                *target = Some(ItemStack {
                    item: c.item,
                    count: 1,
                });
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
            let rest = player.add(stack);
            c[idx] = (rest > 0).then_some(ItemStack {
                item: stack.item,
                count: rest,
            });
        }
        return;
    }
    let Some(stack) = player.slots[idx].take() else {
        return;
    };
    let rest = if let Some(c) = container {
        add_to(c, stack)
    } else {
        // Only the slots a player has: an inventory saved with more keeps
        // them out of reach until `Inventory::fit` folds them back.
        let (hot, main) = player.slots.split_at_mut(HOTBAR_SLOTS);
        let reach = (INVENTORY_SLOTS - HOTBAR_SLOTS).min(main.len());
        let main = &mut main[..reach];
        add_to(if idx < HOTBAR_SLOTS { main } else { hot }, stack)
    };
    player.slots[idx] = (rest > 0).then_some(ItemStack {
        item: stack.item,
        count: rest,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a shift-clicked hotbar stack vanished — it went into slots a player no longer has; seen IN PLAY by the owner (2026-11-19) on a character saved with 36 slots after the inventory became the hotbar alone. Fixed by bounding shift-click and `add` to `INVENTORY_SLOTS` and folding a longer saved inventory back (`Inventory::fit`).
    #[test]
    fn a_stack_never_goes_into_slots_a_player_no_longer_has() {
        let stack = ItemStack {
            count: 5,
            ..ItemStack::default()
        };
        // As saved before: 36 slots, one stack already hidden past the hotbar.
        let mut player = Inventory {
            slots: vec![None; 36],
        };
        player.slots[0] = Some(stack);
        player.slots[20] = Some(ItemStack { count: 7, ..stack });
        let mut cursor = None;
        click(
            &mut cursor,
            &mut player,
            None,
            SlotRef::Player(0),
            ClickButton::Left,
            true,
        );
        assert_eq!(
            player.slots[0],
            Some(stack),
            "with nowhere to go the stack stays in its slot"
        );
        player.fit();
        assert_eq!(player.slots.len(), INVENTORY_SLOTS);
        assert_eq!(
            player.slots[0],
            Some(ItemStack { count: 12, ..stack }),
            "the hidden stack is back in reach"
        );
    }
}
