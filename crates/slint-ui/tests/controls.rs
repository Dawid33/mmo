//! The interactive controls (`ui/controls.slint`), headless: synthetic pointer
//! events into `ControlsHarness`, which hosts a Slider, a Dropdown and Tabs at
//! fixed places.
use std::cell::RefCell;
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::{LogicalPosition, ModelRc, SharedString, VecModel};
use slint_ui::Overlay;
use slint_ui_tools::ControlsHarness;

const SLIDER_X: f32 = 20.0;
const SLIDER_Y: f32 = 20.0;
const SLIDER_W: f32 = 300.0;
const DROPDOWN_X: f32 = 20.0;
const DROPDOWN_Y: f32 = 100.0;
const TABS_X: f32 = 20.0;
const TABS_Y: f32 = 300.0;

fn overlay() -> Overlay<ControlsHarness> {
    let mut overlay = Overlay::with(ControlsHarness::new).expect("harness");
    overlay.resize(800, 600, 1.0);
    let items: Vec<SharedString> = ["zero", "one", "two", "three"].iter().map(|s| (*s).into()).collect();
    overlay.ui().set_items(ModelRc::new(VecModel::from(items)));
    let tabs: Vec<SharedString> = ["Terrain", "Rivers", "Climate"].iter().map(|s| (*s).into()).collect();
    overlay.ui().set_tabs(ModelRc::new(VecModel::from(tabs)));
    overlay.render();
    overlay
}

fn at(overlay: &mut Overlay<ControlsHarness>, x: f32, y: f32, event: impl FnOnce(LogicalPosition) -> WindowEvent) {
    let position = LogicalPosition::new(x, y);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(event(position));
    overlay.render();
}

fn press(overlay: &mut Overlay<ControlsHarness>, x: f32, y: f32) {
    at(overlay, x, y, |position| WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
}

fn release(overlay: &mut Overlay<ControlsHarness>, x: f32, y: f32) {
    at(overlay, x, y, |position| WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
}

fn click(overlay: &mut Overlay<ControlsHarness>, x: f32, y: f32) {
    press(overlay, x, y);
    release(overlay, x, y);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn slider_click_jumps_and_drag_follows() {
    let mut overlay = overlay();
    overlay.ui().set_slider_minimum(10.0);
    overlay.ui().set_slider_maximum(20.0);
    let edits = Rc::new(RefCell::new(Vec::new()));
    let e = edits.clone();
    overlay.ui().on_slider_edited(move |v| e.borrow_mut().push(v));
    overlay.render();

    let y = SLIDER_Y + 5.0;
    press(&mut overlay, SLIDER_X + 0.75 * SLIDER_W, y);
    let value = overlay.ui().get_slider_value();
    let one_pixel = 10.0 / SLIDER_W;
    assert!((value - 17.5).abs() <= one_pixel + 1e-3, "value {value}");

    at(&mut overlay, SLIDER_X, y, |position| WindowEvent::PointerMoved { position });
    assert_eq!(overlay.ui().get_slider_value(), 10.0);
    release(&mut overlay, SLIDER_X, y);
    assert!(edits.borrow().len() >= 2, "edits {:?}", edits.borrow());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn slider_wheel_steps_and_set_from_rust_does_not_fire_edited() {
    let mut overlay = overlay();
    overlay.ui().set_slider_maximum(100.0);
    let edits = Rc::new(RefCell::new(0));
    let e = edits.clone();
    overlay.ui().on_slider_edited(move |_| *e.borrow_mut() += 1);
    overlay.ui().set_slider_value(40.0);
    overlay.render();
    assert_eq!(*edits.borrow(), 0);

    let position = LogicalPosition::new(SLIDER_X + 50.0, SLIDER_Y + 5.0);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: 10.0 });
    overlay.render();
    assert_eq!(*edits.borrow(), 1);
    assert!((overlay.ui().get_slider_value() - 41.0).abs() < 1e-3);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn dropdown_opens_and_selects() {
    let mut overlay = overlay();
    let chosen = Rc::new(RefCell::new(Vec::new()));
    let c = chosen.clone();
    overlay.ui().on_dropdown_chosen(move |i| c.borrow_mut().push(i));
    overlay.ui().set_dropdown_selected(0);
    overlay.render();
    // Closed: nothing under the button.
    let below = (DROPDOWN_Y + 60.0) as u32;
    assert!(!overlay.opaque_at(DROPDOWN_X as u32 + 40, below));

    click(&mut overlay, DROPDOWN_X + 40.0, DROPDOWN_Y + 10.0);
    assert!(overlay.opaque_at(DROPDOWN_X as u32 + 40, below), "popup drawn");
    assert!(chosen.borrow().is_empty());

    // Items are 28 px, the list starts under the 40ish px button: pick the third.
    click(&mut overlay, DROPDOWN_X + 40.0, DROPDOWN_Y + 40.0 + 2.5 * 28.0);
    assert_eq!(*chosen.borrow(), [2]);
    assert_eq!(overlay.ui().get_dropdown_selected(), 2);
    assert!(!overlay.opaque_at(DROPDOWN_X as u32 + 40, below + 60), "popup closed");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tabs_click_selects() {
    let mut overlay = overlay();
    let changed = Rc::new(RefCell::new(Vec::new()));
    let c = changed.clone();
    overlay.ui().on_tab_changed(move |i| c.borrow_mut().push(i));
    overlay.ui().set_tab_selected(0);
    overlay.render();

    // Scan along the tab row for the first click that lands on tab 1.
    let y = TABS_Y + 15.0;
    let mut x = TABS_X + 10.0;
    while changed.borrow().is_empty() && x < TABS_X + 400.0 {
        click(&mut overlay, x, y);
        x += 20.0;
    }
    assert_eq!(*changed.borrow(), [1]);
    assert_eq!(overlay.ui().get_tab_selected(), 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn two_way_bound_value_keeps_following_the_parent_after_a_press() {
    let mut overlay = overlay();
    overlay.ui().set_slider_maximum(100.0);
    overlay.ui().set_slider_value(10.0);
    overlay.render();
    press(&mut overlay, SLIDER_X + 0.5 * SLIDER_W, SLIDER_Y + 5.0);
    release(&mut overlay, SLIDER_X + 0.5 * SLIDER_W, SLIDER_Y + 5.0);
    assert!((overlay.ui().get_slider_value() - 50.0).abs() < 1.0);
    overlay.ui().set_slider_value(80.0);
    overlay.render();
    assert_eq!(overlay.ui().get_slider_value(), 80.0);
    // A click on the thumb's new place still edits from there.
    press(&mut overlay, SLIDER_X + 0.2 * SLIDER_W, SLIDER_Y + 5.0);
    assert!((overlay.ui().get_slider_value() - 20.0).abs() < 1.0);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tabs_collapse_on_second_click() {
    let mut overlay = overlay();
    overlay.ui().set_tabs_collapsible(true);
    overlay.ui().set_tab_selected(-1);
    overlay.render();
    let changed = Rc::new(RefCell::new(Vec::new()));
    let c = changed.clone();
    overlay.ui().on_tab_changed(move |i| c.borrow_mut().push(i));

    // Find tab 0 along the row: the first click that selects anything.
    let y = TABS_Y + 15.0;
    let mut x = TABS_X + 4.0;
    while overlay.ui().get_tab_selected() != 0 && x < TABS_X + 400.0 {
        click(&mut overlay, x, y);
        x += 4.0;
    }
    assert_eq!(overlay.ui().get_tab_selected(), 0, "a click on a tab opens it while collapsed");
    let tab0 = x - 4.0;
    click(&mut overlay, tab0, y);
    assert_eq!(overlay.ui().get_tab_selected(), -1, "a second click on the open tab collapses");
    click(&mut overlay, tab0, y);
    assert_eq!(overlay.ui().get_tab_selected(), 0, "and a third opens it again");
    assert_eq!(*changed.borrow(), [0, -1, 0]);
}
