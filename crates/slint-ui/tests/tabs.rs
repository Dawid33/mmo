//! The `Tabs` control inside the inventory screen, headless: real pointer
//! events into `GameUi`.

use slint_ui::slint::platform::WindowEvent;
use slint_ui::slint::{LogicalPosition, ModelRc, VecModel};
use slint_ui::{GameUi, ItemSlot, Overlay};

/// Regression: the inventory's Journal tab stayed highlighted after the pointer moved off it onto the page below, until it left the modal; seen IN PLAY by the owner (2026-11-19), fixed by counting the pointed tab only while the tab row has the pointer (`Tabs`, controls.slint)
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_tab_loses_its_highlight_when_the_pointer_moves_onto_the_page() {
    let mut overlay: Overlay<GameUi> = Overlay::new().expect("game UI");
    overlay.resize(1280, 720, 1.0);
    let ui = overlay.ui();
    let empty = |n: usize| ModelRc::new(VecModel::from(vec![ItemSlot::default(); n]));
    ui.set_inventory_hotbar(empty(9));
    ui.set_inventory_open(true);
    overlay.render();
    let ui = overlay.ui();
    // The page's top (the character panel with no chest open).
    let (left, top) = (
        ui.get_inventory_grid_left(),
        ui.get_inventory_container_top(),
    );
    // On the second tab (Journal), and a point of its face left of its label.
    let tab = LogicalPosition::new(left + 120.0, top - 30.0);
    let face = |overlay: &Overlay<GameUi>| {
        let (width, _) = overlay.size();
        let p = overlay.pixels()[(tab.y as u32 * width + tab.x as u32 - 30) as usize];
        (p.red, p.green, p.blue)
    };
    let idle = face(&overlay);
    overlay.dispatch(WindowEvent::PointerMoved { position: tab });
    overlay.render();
    assert_ne!(face(&overlay), idle, "the tab under the pointer lights up");
    // Down onto the slots: still inside the modal.
    overlay.dispatch(WindowEvent::PointerMoved {
        position: LogicalPosition::new(left + 120.0, top + 60.0),
    });
    overlay.render();
    assert_eq!(face(&overlay), idle);
}
