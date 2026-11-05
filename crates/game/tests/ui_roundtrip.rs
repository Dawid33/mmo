//! Full logical round-trip with no renderer: a UiAction mutates authoritative
//! inventory, and the re-projected hotbar reflects the change.
use game::{build_hotbar, GameEventKind, HotbarView, Region, RegionCoords, UiAction};

#[test]
fn click_moves_hotbar_stack_to_main() {
    let mut region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.with_data(|d| {
        let c = d.clients.get_mut(&0).unwrap();
        c.inventory.slots[0] = Some(game::ItemStack { item: game::ItemId(1), count: 3 });
        // Clicks land only on an open screen.
        c.inventory_open = true;
    });
    region.forget_last_event();
    assert!(build_hotbar(region.data(), 0).slots[0].is_some());

    let shift = UiAction::Click { slot: game::SlotRef::Player(0), button: game::ClickButton::Left, shift: true };
    region.handle_event(GameEventKind::UiAction(0, shift)).unwrap();

    assert!(build_hotbar(region.data(), 0).slots[0].is_none());
    assert_eq!(region.data().clients.get(&0).unwrap().inventory.slots[9].unwrap().count, 3);
}

#[test]
fn unknown_client_has_empty_hotbar() {
    let region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
    assert_eq!(build_hotbar(region.data(), 7), HotbarView::default());
}
