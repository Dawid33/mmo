//! Inventory model and Minecraft cursor clicks.
use game::{
    click, ClickButton::*, GameEventKind, Inventory, ItemId, ItemStack, Region, RegionCoords, SlotRef, UiAction,
    HOTBAR_SLOTS, INVENTORY_SLOTS, MAX_STACK,
};

fn st(i: u16, n: u32) -> Option<ItemStack> {
    Some(ItemStack { item: ItemId(i), count: n })
}

#[test]
fn add_tops_up_then_fills() {
    let mut inv = Inventory::new();
    inv.slots[0] = st(1, 60);
    assert_eq!(inv.add(st(1, 10).unwrap()), 0);
    assert_eq!(inv.slots[0], st(1, MAX_STACK));
    assert_eq!(inv.slots[1], st(1, 6));
}

#[test]
fn add_full_returns_remainder() {
    let mut inv = Inventory::new();
    for s in inv.slots.iter_mut() {
        *s = st(1, MAX_STACK);
    }
    assert_eq!(inv.add(st(1, 5).unwrap()), 5);
    assert_eq!(inv.add(st(2, 5).unwrap()), 5);
    assert_eq!(inv.slots.len(), INVENTORY_SLOTS);
}

#[test]
fn take_selected_takes_and_empties() {
    let mut inv = Inventory::new();
    inv.slots[2] = st(1, 2);
    assert_eq!(inv.take_selected(2, 1), st(1, 1));
    assert_eq!(inv.take_selected(2, 5), st(1, 1));
    assert_eq!(inv.slots[2], None);
    assert_eq!(inv.take_selected(2, 1), None);
    inv.slots[20] = st(1, 3);
    assert_eq!(inv.take_selected(20, 1), None, "only hotbar slots");
    assert_eq!(inv.slots[20], st(1, 3));
}

fn total(inv: &Inventory, chest: &[Option<ItemStack>], cur: Option<ItemStack>) -> u32 {
    inv.slots.iter().chain(chest).chain(std::iter::once(&cur)).flatten().map(|s| s.count).sum()
}

#[test]
fn clicks_conserve_items() {
    let mut inv = Inventory::new();
    let mut cur = None;
    // Hotbar slot to a nearly full main region: the remainder stays behind.
    for s in inv.slots[HOTBAR_SLOTS..].iter_mut() {
        *s = st(1, MAX_STACK - 1);
    }
    inv.slots[0] = st(1, 40);
    let before = total(&inv, &[], cur);
    click(&mut cur, &mut inv, None, SlotRef::Player(0), Left, true);
    assert_eq!(total(&inv, &[], cur), before);
    assert_eq!(inv.slots[0], st(1, 40 - (INVENTORY_SLOTS - HOTBAR_SLOTS) as u32));
    // Container full: nothing moves, nothing is lost.
    let mut chest = vec![st(1, MAX_STACK); 27];
    inv.slots[1] = st(1, 5);
    let before = total(&inv, &chest, cur);
    click(&mut cur, &mut inv, Some(&mut chest), SlotRef::Player(1), Left, true);
    assert_eq!(total(&inv, &chest, cur), before);
    assert_eq!(inv.slots[1], st(1, 5));
    // Right click a cursor stack onto a MAX_STACK slot of the same item.
    cur = st(1, 4);
    inv.slots[2] = st(1, MAX_STACK);
    let before = total(&inv, &chest, cur);
    click(&mut cur, &mut inv, None, SlotRef::Player(2), Right, false);
    assert_eq!(total(&inv, &chest, cur), before);
    assert_eq!((cur, inv.slots[2]), (st(1, 4), st(1, MAX_STACK)));
}

#[test]
fn left_click_pick_place_swap_merge() {
    let mut inv = Inventory::new();
    let mut cur = None;
    inv.slots[0] = st(1, 10);
    inv.slots[1] = st(2, 3);
    inv.slots[2] = st(1, 60);
    click(&mut cur, &mut inv, None, SlotRef::Player(0), Left, false);
    assert_eq!((cur, inv.slots[0]), (st(1, 10), None));
    click(&mut cur, &mut inv, None, SlotRef::Player(3), Left, false);
    assert_eq!((cur, inv.slots[3]), (None, st(1, 10)));
    click(&mut cur, &mut inv, None, SlotRef::Player(3), Left, false);
    click(&mut cur, &mut inv, None, SlotRef::Player(1), Left, false);
    assert_eq!((cur, inv.slots[1]), (st(2, 3), st(1, 10)));
    // merge with remainder: slot 1 holds 10 of item 1, slot 2 holds 60
    let mut cur = st(1, 10);
    click(&mut cur, &mut inv, None, SlotRef::Player(2), Left, false);
    assert_eq!((cur, inv.slots[2]), (st(1, 6), st(1, 64)));
}

#[test]
fn right_click_halves_and_places_one() {
    let mut inv = Inventory::new();
    let mut cur = None;
    inv.slots[0] = st(1, 7);
    click(&mut cur, &mut inv, None, SlotRef::Player(0), Right, false);
    assert_eq!((cur, inv.slots[0]), (st(1, 4), st(1, 3)));
    click(&mut cur, &mut inv, None, SlotRef::Player(1), Right, false);
    assert_eq!((cur, inv.slots[1]), (st(1, 3), st(1, 1)));
    click(&mut cur, &mut inv, None, SlotRef::Player(1), Right, false);
    assert_eq!((cur, inv.slots[1]), (st(1, 2), st(1, 2)));
}

#[test]
fn shift_moves_hotbar_to_main_and_container_to_player() {
    let mut inv = Inventory::new();
    let mut cur = None;
    inv.slots[0] = st(1, 5);
    click(&mut cur, &mut inv, None, SlotRef::Player(0), Left, true);
    assert_eq!((inv.slots[0], inv.slots[HOTBAR_SLOTS]), (None, st(1, 5)));
    click(&mut cur, &mut inv, None, SlotRef::Player(HOTBAR_SLOTS as u8), Left, true);
    assert_eq!((inv.slots[0], inv.slots[HOTBAR_SLOTS]), (st(1, 5), None));

    let mut chest = vec![None; 27];
    chest[3] = st(1, MAX_STACK);
    click(&mut cur, &mut inv, Some(&mut chest), SlotRef::Container(3), Left, true);
    assert_eq!(chest[3], None);
    assert_eq!(inv.slots[0], st(1, 64));
    assert_eq!(inv.slots[1], st(1, 5));
    assert_eq!(cur, None);
    // player -> container when open
    click(&mut cur, &mut inv, Some(&mut chest), SlotRef::Player(1), Left, true);
    assert_eq!((inv.slots[1], chest[0]), (None, st(1, 5)));
}

#[test]
fn click_out_of_range_is_noop() {
    let mut inv = Inventory::new();
    inv.slots[0] = st(1, 5);
    let before = inv.clone();
    let mut cur = st(2, 1);
    click(&mut cur, &mut inv, None, SlotRef::Player(36), Left, false);
    click(&mut cur, &mut inv, None, SlotRef::Container(0), Left, false);
    click(&mut cur, &mut inv, None, SlotRef::Container(0), Left, true);
    assert_eq!((inv, cur), (before, st(2, 1)));
}

#[test]
fn ui_click_event_undoes() {
    let mut region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.with_data(|d| {
        let c = d.clients.get_mut(&0).unwrap();
        c.inventory.slots[0] = st(1, 8);
        c.inventory_open = true;
    });
    region.forget_last_event();
    let before = game::state_hash(region.data());
    let ev = UiAction::Click { slot: SlotRef::Player(0), button: Right, shift: false };
    region.handle_event(GameEventKind::UiAction(0, ev)).unwrap();
    assert_eq!(region.data().clients[&0].cursor, st(1, 4));
    region.rollback_last_event();
    assert_eq!(before, game::state_hash(region.data()));
}

/// A region with client 0 (Fortress, no body needed) holding `slot0` in slot
/// 0; setup forgotten.
fn one_client(slot0: Option<ItemStack>) -> Region {
    let mut region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.with_data(|d| {
        let c = d.clients.get_mut(&0).unwrap();
        c.inventory = Inventory::new();
        c.inventory.slots[0] = slot0;
    });
    region.forget_last_event();
    region
}

#[test]
fn click_with_inventory_closed_is_noop() {
    let mut region = one_client(st(1, 8));
    assert!(!region.data().clients[&0].inventory_open);
    let before = region.data().clients[&0].clone();
    let ev = UiAction::Click { slot: SlotRef::Player(0), button: Left, shift: false };
    region.handle_event(GameEventKind::UiAction(0, ev)).unwrap();
    let c = &region.data().clients[&0];
    assert_eq!((c.cursor, c.inventory.slots[0]), (None, st(1, 8)));
    assert_eq!(*c, before, "nothing changed");
}

#[test]
fn closing_in_fortress_returns_the_cursor() {
    let mut region = one_client(None);
    region.with_data(|d| {
        let c = d.clients.get_mut(&0).unwrap();
        c.inventory_open = true;
        c.cursor = st(2, 3);
    });
    region.handle_event(GameEventKind::UiAction(0, UiAction::ToggleInventory)).unwrap();
    let c = &region.data().clients[&0];
    assert!(!c.inventory_open);
    assert_eq!(c.cursor, None);
    assert_eq!(c.inventory.slots[0], st(2, 3), "the cursor stack went back into the inventory");
}

#[test]
fn close_inventory_only_closes() {
    let mut region = one_client(None);
    region.handle_event(GameEventKind::UiAction(0, UiAction::CloseInventory)).unwrap();
    assert!(!region.data().clients[&0].inventory_open, "closed stays closed");
    region.with_data(|d| {
        let c = d.clients.get_mut(&0).unwrap();
        c.inventory_open = true;
        c.cursor = st(2, 3);
    });
    region.handle_event(GameEventKind::UiAction(0, UiAction::CloseInventory)).unwrap();
    let c = &region.data().clients[&0];
    assert!(!c.inventory_open);
    assert_eq!((c.cursor, c.inventory.slots[0]), (None, st(2, 3)));
    region.handle_event(GameEventKind::UiAction(0, UiAction::CloseInventory)).unwrap();
    assert!(!region.data().clients[&0].inventory_open, "a second close does not reopen");
}

#[test]
fn fortress_mode_leaves_survival_state() {
    let mut region = one_client(None);
    region.with_data(|d| {
        let c = d.clients.get_mut(&0).unwrap();
        c.mode = game::PlayMode::Adventure;
        c.inventory_open = true;
        c.cursor = st(2, 3);
        c.open_container = Some([1, 2, 3]);
        c.mining = Some(game::Mining { target: [1, 1, 1], started_tick: 0, needed_ticks: 15 });
    });
    let before = game::state_hash(region.data());
    let order = game::Order::SetMode { mode: game::PlayMode::Fortress, at: None };
    region.handle_event(GameEventKind::Order(0, order)).unwrap();
    let c = region.data().clients[&0].clone();
    assert_eq!(c.mode, game::PlayMode::Fortress);
    assert!(!c.inventory_open);
    assert_eq!((c.open_container, c.mining, c.cursor), (None, None, None));
    assert_eq!(c.inventory.slots[0], st(2, 3), "the cursor stack went back into the inventory");
    region.rollback_last_event();
    assert_eq!(game::state_hash(region.data()), before, "undoes bit-exact");
}
