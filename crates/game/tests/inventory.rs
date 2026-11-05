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
    chest[3] = st(1, 70u32.min(64));
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
    region.with_data(|d| d.clients.get_mut(&0).unwrap().inventory.slots[0] = st(1, 8));
    region.forget_last_event();
    let before = game::state_hash(region.data());
    let ev = UiAction::Click { slot: SlotRef::Player(0), button: Right, shift: false };
    region.handle_event(GameEventKind::UiAction(0, ev)).unwrap();
    assert_eq!(region.data().clients[&0].cursor, st(1, 4));
    region.rollback_last_event();
    assert_eq!(before, game::state_hash(region.data()));
}
