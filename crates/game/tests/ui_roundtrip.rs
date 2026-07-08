//! Full logical round-trip with no renderer: a UiAction mutates authoritative
//! inventory, and the re-projected widget tree reflects the change.
use game::{build_player_ui, Content, GameEventKind, Region, RegionCoords, UiAction, WidgetId};

#[test]
fn move_item_action_changes_the_projected_tree() {
    let mut region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.forget_last_event();

    // Before: slot 0 filled (icon), slot 1 empty (no content).
    let before = build_player_ui(region.data(), 0);
    assert!(matches!(before.nodes[&WidgetId(100)].content, Content::Icon(_)));
    assert!(matches!(before.nodes[&WidgetId(101)].content, Content::None));

    region
        .handle_event(GameEventKind::UiAction(0, UiAction::MoveItem { from: 0, to: 1 }))
        .unwrap();

    // After: slot 0 empty, slot 1 filled.
    let after = build_player_ui(region.data(), 0);
    assert!(matches!(after.nodes[&WidgetId(100)].content, Content::None));
    assert!(matches!(after.nodes[&WidgetId(101)].content, Content::Icon(_)));
}
