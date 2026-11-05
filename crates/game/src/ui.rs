//! Engine-owned UI: a pure projection of authoritative data into typed screen
//! state (the hotbar), sent to the client's Slint HUD as `UiUpdate`s. This is
//! a CLIENT-SIDE projection — it is never hashed, serialized, or sent over the
//! wire. `game` stays Bevy- and Slint-free: these are plain data types.

/// Icon handle the client resolves to a texture. For inventory, derived from
/// `ItemId` (see `IconId::of`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IconId(pub u32);

impl IconId {
    /// The client icon handle of an item: its id.
    pub fn of(item: crate::ItemId) -> IconId {
        IconId(item.0 as u32)
    }
}

/// Logical-space UI action (never pixels). Routed to the sim like PlayerInput.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum UiAction {
    /// A click on an inventory slot (`inventory::click`).
    Click { slot: crate::SlotRef, button: crate::ClickButton, shift: bool },
    /// Open/close the inventory (drives cursor grab).
    ToggleInventory,
}

/// One filled hotbar slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SlotView {
    pub icon: IconId,
    pub count: u32,
}

/// The local player's hotbar, slot by slot (`None` = empty).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct HotbarView {
    pub slots: Vec<Option<SlotView>>,
    /// The selected slot.
    pub selected: u8,
}

/// A change to the client's UI state, consumed by the client's Slint HUD.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum UiUpdate {
    /// The hotbar changed; sent only when it differs from the last one.
    Hotbar(HotbarView),
    /// The sim asks for the cursor to be grabbed (or released, while a menu
    /// such as the inventory is open).
    SetCursorGrab(bool),
    /// The local client's play mode; sent only when it changes.
    PlayMode(crate::PlayMode),
    /// The inventory / chest screen's contents; sent only when they change
    /// (`InventoryView::default()` = closed).
    Inventory(InventoryView),
    /// The block being mined, 0..=1000 (`None` = not mining); sent only when
    /// it changes.
    Mining(Option<u32>),
}

/// What the inventory screen shows while it is open (client-side projection).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct InventoryView {
    pub open: bool,
    /// The 36 player slots: hotbar `0..9`, then the main grid.
    pub slots: Vec<Option<SlotView>>,
    /// The stack held on the mouse cursor.
    pub cursor: Option<SlotView>,
    /// The open chest's slots, if one is open.
    pub container: Option<Vec<Option<SlotView>>>,
}

fn slot_view(stack: &Option<crate::ItemStack>) -> Option<SlotView> {
    stack.map(|s| SlotView { icon: IconId::of(s.item), count: s.count })
}

/// The inventory screen for a client: closed = the default (empty) view.
pub fn build_inventory_view(
    open: bool,
    inventory: &crate::Inventory,
    cursor: &Option<crate::ItemStack>,
    container: Option<&crate::ContainerView>,
) -> InventoryView {
    if !open {
        return InventoryView::default();
    }
    InventoryView {
        open,
        slots: inventory.slots.iter().map(slot_view).collect(),
        cursor: slot_view(cursor),
        container: container.map(|c| c.slots.iter().map(slot_view).collect()),
    }
}

/// The local client's play mode as `UiUpdate::PlayMode`, only when it
/// differs from `*prev` (which it then updates). `*prev` starts at the
/// default (Fortress), so nothing is sent until the mode first changes.
/// Unknown client → Fortress.
pub fn play_mode_update(
    prev: &mut crate::PlayMode,
    data: &crate::GameData,
    client: crate::ClientId,
) -> Option<UiUpdate> {
    let mode = data.clients.get(&client).map(|c| c.mode).unwrap_or_default();
    (mode != *prev).then(|| {
        *prev = mode;
        UiUpdate::PlayMode(mode)
    })
}

/// Project the local player's authoritative data into its hotbar. Pure and
/// deterministic; run client-side only. Unknown client → empty hotbar. A slot
/// shows only items that have an icon.
pub fn build_hotbar(data: &crate::GameData, client: crate::ClientId) -> HotbarView {
    let Some(c) = data.clients.get(&client) else {
        return HotbarView::default();
    };
    let slots = c
        .inventory
        .slots
        .iter()
        .take(crate::HOTBAR_SLOTS)
        .map(|slot| slot.map(|stack| SlotView { icon: IconId::of(stack.item), count: stack.count }))
        .collect();
    HotbarView { slots, selected: c.selected }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_view_is_empty_when_closed_and_full_when_open() {
        let mut inv = crate::Inventory::new();
        inv.slots[12] = Some(crate::ItemStack { item: crate::ItemId(3), count: 7 });
        let cursor = Some(crate::ItemStack { item: crate::ItemId(4), count: 2 });
        assert_eq!(build_inventory_view(false, &inv, &cursor, None), InventoryView::default());
        let open = build_inventory_view(true, &inv, &cursor, None);
        assert!(open.open && open.container.is_none());
        assert_eq!(open.slots.len(), crate::INVENTORY_SLOTS);
        assert_eq!(open.slots[12], Some(SlotView { icon: IconId(3), count: 7 }));
        assert_eq!(open.cursor, Some(SlotView { icon: IconId(4), count: 2 }));
    }

    #[test]
    fn build_hotbar_projects_first_nine_slots_and_selection() {
        let mut rb = crate::Rollback::new(None);
        rb.new_transaction();
        let mut inventory = crate::Inventory::new();
        inventory.slots[0] = Some(crate::ItemStack { item: crate::ItemId(1), count: 42 });
        inventory.slots[20] = Some(crate::ItemStack { item: crate::ItemId(2), count: 1 });
        rb.clients.insert(0, crate::Client { inventory, selected: 4, ..crate::Client::default() });
        rb.forget();

        let hotbar = build_hotbar(&rb, 0);
        assert_eq!(hotbar.slots.len(), 9);
        assert_eq!(hotbar.slots[0], Some(SlotView { icon: IconId(1), count: 42 }));
        assert_eq!(hotbar.slots[1], None);
        assert_eq!(hotbar.selected, 4);
    }

    #[test]
    fn build_hotbar_unknown_client_is_empty() {
        let rb = crate::Rollback::new(None);
        assert_eq!(build_hotbar(&rb, 999), HotbarView::default());
    }
}
