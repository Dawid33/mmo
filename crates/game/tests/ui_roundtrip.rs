//! Full logical round-trip with no renderer: a UiAction mutates authoritative
//! inventory, and the re-projected hotbar reflects the change.
use game::{build_hotbar, GameEventKind, HotbarView, Region, RegionCoords, UiAction};

#[test]
fn move_item_swaps_hotbar_slots() {
    let mut region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.forget_last_event();

    // Before: slot 0 filled, slot 1 empty.
    let before = build_hotbar(region.data(), 0);
    assert!(before.slots[0].is_some());
    assert!(before.slots[1].is_none());

    region
        .handle_event(GameEventKind::UiAction(0, UiAction::MoveItem { from: 0, to: 1 }))
        .unwrap();

    // After: swapped.
    let after = build_hotbar(region.data(), 0);
    assert_eq!(after.slots[0], before.slots[1]);
    assert_eq!(after.slots[1], before.slots[0]);
}

#[test]
fn unknown_client_has_empty_hotbar() {
    let region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
    assert_eq!(build_hotbar(region.data(), 7), HotbarView::default());
}
