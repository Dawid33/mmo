//! Engine-owned UI: a pure projection of authoritative data into typed screen
//! state (the hotbar), sent to the client's Slint HUD as `UiUpdate`s. This is
//! a CLIENT-SIDE projection — it is never hashed, serialized, or sent over the
//! wire. `game` stays Bevy- and Slint-free: these are plain data types.

/// Icon handle the client resolves to a texture. For inventory, derived from
/// `ItemKind` (see `IconId::of`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconId(pub u32);

impl IconId {
    /// Map an item kind to a client icon handle. `Empty` has no icon.
    pub fn of(kind: crate::ItemKind) -> Option<IconId> {
        match kind {
            crate::ItemKind::Empty => None,
            crate::ItemKind::Dirt => Some(IconId(1)),
            crate::ItemKind::Stone => Some(IconId(2)),
        }
    }
}

/// Logical-space UI action (never pixels). Routed to the sim like PlayerInput.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum UiAction {
    /// Swap two inventory slots (drag/drop resolves to this).
    MoveItem { from: usize, to: usize },
    /// Open/close the inventory (drives cursor grab).
    ToggleInventory,
}

/// One filled hotbar slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotView {
    pub icon: IconId,
    pub count: u32,
}

/// The local player's hotbar, slot by slot (`None` = empty).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HotbarView {
    pub slots: Vec<Option<SlotView>>,
}

/// A change to the client's UI state, consumed by the client's Slint HUD.
#[derive(Debug, Clone, PartialEq)]
pub enum UiUpdate {
    /// The hotbar changed; sent only when it differs from the last one.
    Hotbar(HotbarView),
    /// The sim asks for the cursor to be grabbed (or released, while a menu
    /// such as the inventory is open).
    SetCursorGrab(bool),
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
        .map(|slot| slot.and_then(|stack| IconId::of(stack.kind).map(|icon| SlotView { icon, count: stack.count })))
        .collect();
    HotbarView { slots }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_hotbar_projects_seeded_inventory() {
        // A minimal GameData with one client that has a seeded inventory.
        let mut rb = crate::Rollback::new(None);
        rb.new_transaction();
        rb.clients.insert(
            0,
            crate::Client { inventory: crate::Inventory::seeded(), ..crate::Client::default() },
        );
        rb.forget();

        let hotbar = build_hotbar(&rb, 0);
        assert_eq!(hotbar.slots.len(), 9);
        // Slot 0 is seeded (Dirt x42); slot 1 is empty.
        assert_eq!(hotbar.slots[0], Some(SlotView { icon: IconId(1), count: 42 }));
        assert_eq!(hotbar.slots[1], None);
    }

    #[test]
    fn build_hotbar_unknown_client_is_empty() {
        let rb = crate::Rollback::new(None);
        assert_eq!(build_hotbar(&rb, 999), HotbarView::default());
    }
}
