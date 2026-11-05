//! The inventory / chest screen (`ui/inventory.slint`) and the HUD hotbar,
//! headless: a slot press reports the slot it is (the index mapping the
//! client turns into `SlotRef`s), the chest grid exists only with a chest,
//! and the hotbar highlights the selected slot.
use std::cell::RefCell;
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::{LogicalPosition, ModelRc, VecModel};
use slint_ui::{GameUi, HudUi, ItemSlot, Overlay};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

fn press(overlay: &Overlay<GameUi>, x: f32, y: f32, button: PointerEventButton) {
    let position = LogicalPosition::new(x, y);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerPressed { position, button });
    overlay.dispatch(WindowEvent::PointerReleased { position, button });
}

fn slots(n: usize) -> ModelRc<ItemSlot> {
    let v: Vec<ItemSlot> = (0..n).map(|_| ItemSlot { filled: true, count: "3".into(), icon: Default::default() }).collect();
    ModelRc::new(VecModel::from(v))
}

fn open_screen(container: bool) -> (Overlay<GameUi>, Rc<RefCell<Vec<(bool, i32, bool)>>>) {
    let mut overlay = Overlay::new().expect("game UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    let ui = overlay.ui();
    ui.set_inventory_hotbar(slots(9));
    ui.set_inventory_main(slots(27));
    ui.set_inventory_container(if container { slots(27) } else { slots(0) });
    ui.set_inventory_open(true);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    ui.on_inventory_slot_pressed(move |c, i, r| sink.borrow_mut().push((c, i, r)));
    overlay.render();
    (overlay, seen)
}

/// Centre of the slot at (`col`, `row`) of the grid starting at `top`.
fn centre(overlay: &Overlay<GameUi>, col: usize, row: usize, top: f32) -> (f32, f32) {
    let ui = overlay.ui();
    let (pitch, size) = (ui.get_inventory_slot_pitch(), ui.get_inventory_slot_size());
    (ui.get_inventory_grid_left() + col as f32 * pitch + size / 2.0, top + row as f32 * pitch + size / 2.0)
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn slot_press_reports_slot_ref() {
    let (overlay, seen) = open_screen(true);
    let main_top = overlay.ui().get_inventory_main_top();
    let (x, y) = centre(&overlay, 2, 1, main_top);
    press(&overlay, x, y, PointerEventButton::Left);
    // Main slot (2,1) = player slot 9 + 1*9 + 2.
    assert_eq!(seen.borrow().last(), Some(&(false, 9 + 9 + 2, false)));

    let hotbar_top = overlay.ui().get_inventory_hotbar_top();
    let (x, y) = centre(&overlay, 4, 0, hotbar_top);
    press(&overlay, x, y, PointerEventButton::Right);
    assert_eq!(seen.borrow().last(), Some(&(false, 4, true)), "hotbar column 4, right button");

    let container_top = overlay.ui().get_inventory_container_top();
    let (x, y) = centre(&overlay, 3, 2, container_top);
    press(&overlay, x, y, PointerEventButton::Left);
    assert_eq!(seen.borrow().last(), Some(&(true, 2 * 9 + 3, false)), "chest row 2 column 3");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn container_grid_hidden_without_container() {
    let (with, _) = open_screen(true);
    let (without, seen) = open_screen(false);
    // The chest's grid sits above the player's; without one it is not there.
    let gap = |o: &Overlay<GameUi>| o.ui().get_inventory_main_top() - o.ui().get_inventory_container_top();
    assert!(gap(&with) > 100.0, "chest grid takes room");
    assert_eq!(gap(&without), 0.0, "no chest grid");
    // Where the chest's first slot would be there is just the main grid now.
    let top = without.ui().get_inventory_container_top();
    let (x, y) = centre(&without, 0, 0, top);
    press(&without, x, y, PointerEventButton::Left);
    assert_eq!(seen.borrow().last(), Some(&(false, 9, false)), "no chest slot reports");
    assert!(seen.borrow().iter().all(|(c, _, _)| !c));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn hud_hotbar_highlights_selected() {
    let mut overlay = Overlay::with(HudUi::new).expect("hud");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    let empty = |n| -> ModelRc<ItemSlot> {
        ModelRc::new(VecModel::from((0..n).map(|_| ItemSlot::default()).collect::<Vec<_>>()))
    };
    overlay.ui().set_hotbar(empty(9));
    overlay.ui().set_hotbar_selected(0);
    overlay.render();
    // The strip is centred at the bottom; probe the top border row of slot
    // 0 and slot 4 (pitch 47, size 44, gap 3).
    let strip_left = (WIDTH as f32 - (9.0 * 47.0 + 3.0)) / 2.0;
    let y = HEIGHT - 4 - 3 - 44 + 1; // inside the slot's 3 px selected border
    let px = |slot: u32| (strip_left + 3.0 + slot as f32 * 47.0 + 22.0) as u32;
    let colour_at = |overlay: &Overlay<HudUi>, x: u32, y: u32| {
        let p = overlay.pixels()[(y * overlay.size().0 + x) as usize];
        (p.red, p.green, p.blue, p.alpha)
    };
    let (first_selected, fourth_plain) = (colour_at(&overlay, px(0), y), colour_at(&overlay, px(4), y));
    assert_ne!(first_selected, fourth_plain, "the selected slot has its own border");
    overlay.ui().set_hotbar_selected(4);
    overlay.render();
    assert_eq!(colour_at(&overlay, px(4), y), first_selected, "slot 4 now has the selected border");
    assert_eq!(colour_at(&overlay, px(0), y), fourth_plain, "slot 0 lost it");
}
