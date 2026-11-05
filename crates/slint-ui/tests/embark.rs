//! The embark panel beside the arrival map, headless: its Fortress /
//! Adventure / Legends tabs switch bodies, each tab's controls report back,
//! and the Esc menu offers End adventure while an adventure is held.
use std::cell::RefCell;
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::{LogicalPosition, ModelRc, SharedString, VecModel};
use slint_ui::{GameUi, LegendsFilterRow, Overlay};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

/// The arrival map open with the embark panel beside it.
fn overlay() -> Overlay<GameUi> {
    let mut overlay = Overlay::new().expect("game UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    overlay.ui().set_map_open(true);
    overlay.ui().set_map_available(true);
    overlay.ui().set_map_cols(4);
    overlay.ui().set_map_rows(4);
    overlay.ui().set_map_modal(true);
    overlay.ui().set_embark_visible(true);
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

/// The x of tab `index` on the embark tab row: scans from the row's left end
/// for the click that selects it (from the Fortress tab), then goes back to
/// Fortress.
fn tab_x(overlay: &mut Overlay<GameUi>, index: i32) -> f32 {
    let y = overlay.ui().get_embark_tabs_y();
    let mut x = overlay.ui().get_embark_tabs_x() + 2.0;
    while x < overlay.ui().get_embark_tabs_end() {
        overlay.ui().set_embark_tab(0);
        overlay.render();
        click(overlay, x, y);
        if overlay.ui().get_embark_tab() == index {
            overlay.ui().set_embark_tab(0);
            overlay.render();
            return x;
        }
        x += 4.0;
    }
    panic!("tab {index} not found on the embark tab row");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tabs_switch_bodies() {
    let mut overlay = overlay();
    assert_eq!(overlay.ui().get_embark_tab(), 0, "opens on Fortress");
    let ui = overlay.ui();
    assert!(ui.get_embark_tabs_end() <= ui.get_embark_tabs_right(), "the three tabs fit the panel");

    let changed = Rc::new(RefCell::new(Vec::new()));
    let c = changed.clone();
    overlay.ui().on_embark_tab_changed(move |i| c.borrow_mut().push(i));
    let adventure = tab_x(&mut overlay, 1);
    changed.borrow_mut().clear();
    let y = overlay.ui().get_embark_tabs_y();
    let fortress_pixels = overlay.pixels().iter().filter(|p| p.alpha > 0).count();

    click(&mut overlay, adventure, y);
    assert_eq!(overlay.ui().get_embark_tab(), 1);
    assert_eq!(*changed.borrow(), [1]);

    // Legends widens the panel (the map's reserve follows): more is drawn.
    let legends = tab_x(&mut overlay, 2);
    click(&mut overlay, legends, y);
    assert_eq!(overlay.ui().get_embark_tab(), 2);
    let legends_pixels = overlay.pixels().iter().filter(|p| p.alpha > 0).count();
    assert!(overlay.opaque_at(450, 200), "the widened panel covers x = 450");
    assert!(legends_pixels >= fortress_pixels, "{legends_pixels} vs {fortress_pixels}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn adventure_start_enabled_only_with_site() {
    let mut overlay = overlay();
    let started = Rc::new(RefCell::new(0));
    let s = started.clone();
    overlay.ui().on_adventure_start(move || *s.borrow_mut() += 1);
    overlay.ui().set_embark_tab(1);
    overlay.ui().set_adventure_civs(strings(&["The Realm of Copper", "The Guild of Ash"]));
    overlay.ui().set_adventure_civ_picked(0);
    overlay.ui().set_adventure_sites(strings(&["Boatmurdered — pop 12", "Oilfurnace — pop 3"]));
    overlay.ui().set_adventure_site_picked(-1);
    overlay.ui().set_adventure_enabled(false);
    overlay.render();

    let (x, y) = (overlay.ui().get_adventure_start_x(), overlay.ui().get_adventure_start_y());
    assert!(overlay.opaque_at(x as u32, y as u32), "Start drawn at ({x}, {y})");
    click(&mut overlay, x, y);
    assert_eq!(*started.borrow(), 0, "no site picked: Start does not fire");

    overlay.ui().set_adventure_site_picked(1);
    overlay.ui().set_adventure_region("Region (3, 4)".into());
    overlay.ui().set_adventure_enabled(true);
    overlay.render();
    let (x, y) = (overlay.ui().get_adventure_start_x(), overlay.ui().get_adventure_start_y());
    click(&mut overlay, x, y);
    assert_eq!(*started.borrow(), 1, "a site picked: Start fires");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn legends_filter_click_reports_index() {
    let mut overlay = overlay();
    let picked = Rc::new(RefCell::new(Vec::new()));
    let p = picked.clone();
    overlay.ui().on_legends_pick_filter(move |i| p.borrow_mut().push(i));
    let row = |text: &str, depth: i32, dimmed: bool| LegendsFilterRow { text: text.into(), depth, dimmed };
    let rows = vec![
        row("All events", 0, false),
        row("The Realm of Copper", 0, false),
        row("Boatmurdered", 1, false),
        row("Oldhold", 1, true),
    ];
    overlay.ui().set_legends_filters(ModelRc::new(VecModel::from(rows)));
    overlay.ui().set_legends_header("Events of All events — 2".into());
    overlay.ui().set_legends_lines(strings(&[
        "1 Granite 102: Urist McMiner was born in Boatmurdered.",
        "1 Granite 101: Boatmurdered was founded by the Realm of Copper.",
    ]));
    overlay.ui().set_embark_tab(2);
    overlay.render();

    let ui = overlay.ui();
    let (x, top, pitch) = (ui.get_legends_filter_x(), ui.get_legends_filter_first_y(), ui.get_legends_filter_row_height());
    for i in [2, 3, 0] {
        click(&mut overlay, x, top + (i as f32 + 0.5) * pitch);
    }
    assert_eq!(*picked.borrow(), [2, 3, 0], "an abandoned (dimmed) site still picks");
}

/// Scrolling the event lines to the bottom asks for the next (older) page
/// while there is one.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn legends_scroll_to_bottom_loads_more() {
    let mut overlay = overlay();
    let more = Rc::new(RefCell::new(0));
    let m = more.clone();
    overlay.ui().on_legends_load_more(move || *m.borrow_mut() += 1);
    let lines: Vec<String> = (0..60).map(|i| format!("{i} Granite 102: Urist McMiner was born in Boatmurdered.")).collect();
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    overlay.ui().set_legends_lines(strings(&lines));
    overlay.ui().set_legends_more(true);
    overlay.ui().set_embark_tab(2);
    overlay.render();

    // Over the lines: right of the filter column, in the widened panel.
    let position = LogicalPosition::new(450.0, 400.0);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: -40.0 });
    overlay.render();
    assert_eq!(*more.borrow(), 0, "one notch down is not the bottom");
    for _ in 0..200 {
        overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: -40.0 });
    }
    overlay.render();
    assert!(*more.borrow() >= 1, "the bottom asks for more");

    // Without more, the bottom asks for nothing.
    overlay.ui().set_legends_more(false);
    *more.borrow_mut() = 0;
    for _ in 0..5 {
        overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: 40.0 });
    }
    for _ in 0..5 {
        overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: -40.0 });
    }
    assert_eq!(*more.borrow(), 0);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn menu_shows_end_adventure() {
    let mut overlay = Overlay::<GameUi>::new().expect("game UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    overlay.ui().set_menu_open(true);
    overlay.render();
    let drawn = |overlay: &Overlay<GameUi>| overlay.pixels().iter().filter(|p| p.alpha > 0).count();
    let without = drawn(&overlay);
    let ended = Rc::new(RefCell::new(0));
    let e = ended.clone();
    overlay.ui().on_end_adventure(move || *e.borrow_mut() += 1);

    overlay.ui().set_can_end_adventure(true);
    overlay.render();
    assert!(drawn(&overlay) > without, "End adventure adds a button");
    let (x, y) = (overlay.ui().get_end_adventure_button_x(), overlay.ui().get_end_adventure_button_y());
    click(&mut overlay, x, y);
    assert_eq!(*ended.borrow(), 1, "End adventure fires");

    // Not on the modal arrival map's menu (Main menu only).
    overlay.ui().set_map_modal(true);
    overlay.render();
    let (x, y) = (overlay.ui().get_end_adventure_button_x(), overlay.ui().get_end_adventure_button_y());
    click(&mut overlay, x, y);
    assert_eq!(*ended.borrow(), 1, "the modal menu offers no End adventure");
}
