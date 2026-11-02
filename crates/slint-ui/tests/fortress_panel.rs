//! The top-left fortress panel (`ui/fortress-panel.slint`), headless: the
//! header's placeholders, tabs that open and collapse, People rows that pick,
//! long lists that stay on screen, and nothing over the arrival map.
use std::cell::Cell;
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::{LogicalPosition, ModelRc, SharedString, VecModel};
use slint_ui::{GameUi, Overlay};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

fn overlay() -> Overlay<GameUi> {
    let mut overlay = Overlay::new().expect("game UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    overlay.render();
    overlay
}

fn click(overlay: &mut Overlay<GameUi>, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    overlay.dispatch(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    overlay.render();
}

fn strings(items: &[&str]) -> ModelRc<SharedString> {
    let items: Vec<SharedString> = items.iter().map(|s| (*s).into()).collect();
    ModelRc::new(VecModel::from(items))
}

/// The x of tab `index` on the tab row: scans from the row's left end for the
/// click that opens it (the panel starts collapsed), then collapses it again.
fn tab_x(overlay: &mut Overlay<GameUi>, index: i32) -> f32 {
    let y = overlay.ui().get_fortress_tabs_y();
    let mut x = overlay.ui().get_fortress_tabs_x() + 2.0;
    while x < 400.0 {
        overlay.ui().set_fortress_tab(-1);
        overlay.render();
        click(overlay, x, y);
        if overlay.ui().get_fortress_tab() == index {
            overlay.ui().set_fortress_tab(-1);
            overlay.render();
            return x;
        }
        x += 4.0;
    }
    panic!("tab {index} not found on the tab row");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn header_shows_placeholders() {
    let overlay = overlay();
    assert_eq!(overlay.ui().get_fortress_value(), "0");
    assert_eq!(overlay.ui().get_fortress_happiness(), "0");
    assert_eq!(overlay.ui().get_fortress_food(), "0");
    assert!(overlay.opaque_at(24, 24), "the panel draws in the top-left corner");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn values_sit_right_of_the_tabs_in_the_corner() {
    let mut overlay = overlay();
    overlay.ui().set_fortress_tab(0);
    overlay.render();
    let ui = overlay.ui();
    assert!(ui.get_fortress_values_left() >= ui.get_fortress_tabs_end(), "the values start after the last tab");
    assert!(ui.get_fortress_values_top() >= ui.get_fortress_tabs_top(), "the values are on the tab row");
    assert!(ui.get_fortress_values_bottom() <= ui.get_fortress_tabs_top() + ui.get_fortress_tab_row_height());
    assert!(ui.get_fortress_values_bottom() > ui.get_fortress_values_top(), "the values have height");
    assert!(overlay.opaque_at(0, 0), "flush in the top-left corner");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tab_labels_fit_their_tabs() {
    let overlay = overlay();
    let ui = overlay.ui();
    assert!(
        ui.get_fortress_widest_label() <= ui.get_fortress_tab_label_room(),
        "\"Designations\" ({}) is wider than its tab's text room ({})",
        ui.get_fortress_widest_label(),
        ui.get_fortress_tab_label_room()
    );
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tabs_open_and_collapse() {
    let mut overlay = overlay();
    assert_eq!(overlay.ui().get_fortress_tab(), -1, "starts collapsed");
    let y = overlay.ui().get_fortress_tabs_y();
    let x = tab_x(&mut overlay, 0);
    click(&mut overlay, x, y);
    assert_eq!(overlay.ui().get_fortress_tab(), 0);
    click(&mut overlay, x, y);
    assert_eq!(overlay.ui().get_fortress_tab(), -1, "the open tab collapses on a second click");
    // Collapsed, the panel ends just under the tab row: the world below it is
    // the player's again.
    let below = (y + 60.0) as u32;
    for x in (12..300).step_by(6) {
        assert!(!overlay.opaque_at(x, below), "collapsed panel still draws at ({x}, {below})");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn people_row_click_picks() {
    let mut overlay = overlay();
    let picked = Rc::new(Cell::new(-1));
    let p = picked.clone();
    overlay.ui().on_person_picked(move |i| p.set(i));
    overlay.ui().set_people_names(strings(&["Urist", "Bomrek"]));
    overlay.ui().set_fortress_tab(1);
    overlay.render();
    let x = overlay.ui().get_fortress_tabs_x() + 60.0;
    let y = overlay.ui().get_people_first_item_y() + 1.5 * overlay.ui().get_people_row_height();
    click(&mut overlay, x, y);
    assert_eq!(picked.get(), 1, "the second row picks person 1");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn long_list_stays_on_screen() {
    let mut overlay = overlay();
    let lines: Vec<String> = (0..200).map(|i| format!("Dig · priority 4 · {i} tiles")).collect();
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    overlay.ui().set_designation_lines(strings(&lines));
    overlay.ui().set_fortress_tab(2);
    overlay.render();
    for y in 700..HEIGHT {
        for x in (12..400).step_by(6) {
            assert!(!overlay.opaque_at(x, y), "a 200-row list reaches ({x}, {y})");
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn hidden_on_the_arrival_map() {
    let mut overlay = overlay();
    overlay.ui().set_map_modal(true);
    overlay.render();
    assert!(!overlay.opaque_at(24, 24), "no fortress panel during the embark step");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn open_tab_fits_a_small_window() {
    // 1280×720 at VOXEL_UI_SCALE 2: 640×360 logical pixels.
    let mut overlay = Overlay::new().expect("game UI");
    overlay.resize(640, 360, 1.0);
    let lines: Vec<String> = (0..200).map(|i| format!("Dig · priority 4 · {i} tiles")).collect();
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    overlay.ui().set_designation_lines(strings(&lines));
    overlay.ui().set_fortress_tab(2);
    overlay.render();
    for y in 352..360 {
        for x in (12..400).step_by(6) {
            assert!(!overlay.opaque_at(x, y), "the open tab runs off a 360 px window at ({x}, {y})");
        }
    }
}
