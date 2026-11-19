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
    /// Close the inventory if it is open (a no-op when it is closed): Esc,
    /// which must never open it from a stale view of the state.
    CloseInventory,
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
/// default (Adventure), so nothing is sent until the mode first changes.
/// Unknown client → Adventure.
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

