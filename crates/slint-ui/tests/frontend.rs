//! The frontend layer (`ui/frontend.slint`), headless: the main menu's buttons
//! fire, Load is disabled without worlds, the error is drawn, `shown: false`
//! draws nothing.
use std::cell::Cell;
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::LogicalPosition;
use slint_ui::{FrontendUi, Overlay};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

fn overlay() -> Overlay<FrontendUi> {
    let mut overlay = Overlay::with(FrontendUi::new).expect("frontend UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    overlay.render();
    overlay
}

fn click(overlay: &mut Overlay<FrontendUi>, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    overlay.dispatch(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    overlay.render();
}

fn counter() -> Rc<Cell<u32>> {
    Rc::new(Cell::new(0))
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn main_menu_buttons_fire() {
    let mut overlay = overlay();
    let (new, load, exit) = (counter(), counter(), counter());
    let (n, l, e) = (new.clone(), load.clone(), exit.clone());
    overlay.ui().on_new_world(move || n.set(n.get() + 1));
    overlay.ui().on_load_world(move || l.set(l.get() + 1));
    overlay.ui().on_exit(move || e.set(e.get() + 1));
    overlay.ui().set_can_load(true);
    overlay.render();

    let ui = overlay.ui();
    let (nx, ny) = (ui.get_new_button_x(), ui.get_new_button_y());
    let (lx, ly) = (ui.get_load_button_x(), ui.get_load_button_y());
    let (ex, ey) = (ui.get_exit_button_x(), ui.get_exit_button_y());
    click(&mut overlay, nx, ny);
    assert_eq!((new.get(), load.get(), exit.get()), (1, 0, 0));
    click(&mut overlay, lx, ly);
    assert_eq!((new.get(), load.get(), exit.get()), (1, 1, 0));
    click(&mut overlay, ex, ey);
    assert_eq!((new.get(), load.get(), exit.get()), (1, 1, 1));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn load_disabled_without_worlds() {
    let mut overlay = overlay();
    let load = counter();
    let l = load.clone();
    overlay.ui().on_load_world(move || l.set(l.get() + 1));
    overlay.ui().set_can_load(false);
    overlay.render();
    let (x, y) = (overlay.ui().get_load_button_x(), overlay.ui().get_load_button_y());
    click(&mut overlay, x, y);
    assert_eq!(load.get(), 0);
    overlay.ui().set_can_load(true);
    overlay.render();
    click(&mut overlay, x, y);
    assert_eq!(load.get(), 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn menu_error_is_shown() {
    let mut overlay = overlay();
    overlay.ui().set_menu_error("The server stopped responding".into());
    overlay.render();
    let drawn = |overlay: &Overlay<FrontendUi>| overlay.pixels().iter().filter(|p| p.alpha > 0).count();
    let after_total = drawn(&overlay);
    overlay.ui().set_menu_error("".into());
    overlay.render();
    let cleared_total = drawn(&overlay);
    assert!(after_total > cleared_total, "error text drawn ({after_total} vs {cleared_total})");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn hidden_frontend_draws_nothing_and_takes_no_input() {
    let mut overlay = overlay();
    assert!(overlay.opaque_at(WIDTH / 2, HEIGHT / 2), "menu drawn");
    let new = counter();
    let n = new.clone();
    overlay.ui().on_new_world(move || n.set(n.get() + 1));
    let (x, y) = (overlay.ui().get_new_button_x(), overlay.ui().get_new_button_y());
    overlay.ui().set_shown(false);
    overlay.render();
    assert!(overlay.pixels().iter().all(|p| p.alpha == 0), "nothing drawn");
    click(&mut overlay, x, y);
    assert_eq!(new.get(), 0);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn placeholder_screens_go_back() {
    let mut overlay = overlay();
    let back = counter();
    let b = back.clone();
    overlay.ui().on_back(move || b.set(b.get() + 1));
    for screen in 1..=3 {
        overlay.ui().set_screen(screen);
        overlay.render();
        let (x, y) = (overlay.ui().get_back_button_x(), overlay.ui().get_back_button_y());
        click(&mut overlay, x, y);
        assert_eq!(back.get(), screen as u32, "screen {screen}");
    }
}

// --- The map generator (screen 1) ---

use slint_ui::slint::{ModelRc, SharedString, VecModel};
use slint_ui::KnobRow;

fn generator() -> Overlay<FrontendUi> {
    let mut overlay = overlay();
    overlay.ui().set_screen(1);
    let strings = |items: &[&str]| -> ModelRc<SharedString> {
        ModelRc::new(VecModel::from(items.iter().map(|s| SharedString::from(*s)).collect::<Vec<_>>()))
    };
    overlay.ui().set_gen_presets(strings(&["Default", "Archipelago", "Island arc", "Custom"]));
    overlay.ui().set_gen_preset(0);
    overlay.ui().set_gen_tabs(strings(&["Land", "Mountains", "Water", "Geology", "Advanced"]));
    overlay.ui().set_gen_layers(strings(&["Relief", "Rivers"]));
    let row = KnobRow {
        key: "uplift".into(),
        label: "Uplift rate".into(),
        help: "Peak rock uplift.".into(),
        min: 0.0,
        max: 1.0,
        value: 0.5,
        default: 0.5,
        step: 0.0,
        is_choice: false,
        choices: strings(&[]),
    };
    overlay.ui().set_gen_rows(ModelRc::new(VecModel::from(vec![row])));
    overlay.render();
    overlay
}

fn drag(overlay: &mut Overlay<FrontendUi>, from: (f32, f32), to: (f32, f32)) {
    let start = LogicalPosition::new(from.0, from.1);
    let end = LogicalPosition::new(to.0, to.1);
    overlay.dispatch(WindowEvent::PointerMoved { position: start });
    overlay.dispatch(WindowEvent::PointerPressed { position: start, button: PointerEventButton::Left });
    overlay.render();
    overlay.dispatch(WindowEvent::PointerMoved { position: end });
    overlay.render();
    overlay.dispatch(WindowEvent::PointerReleased { position: end, button: PointerEventButton::Left });
    overlay.render();
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn knob_row_drag_fires_knob_edited() {
    let mut overlay = generator();
    let edits = Rc::new(std::cell::RefCell::new(Vec::<(String, f32)>::new()));
    let e = edits.clone();
    overlay.ui().on_gen_knob_edited(move |key, v| e.borrow_mut().push((key.to_string(), v)));
    let ui = overlay.ui();
    let (x, w) = (ui.get_gen_slider_x(), ui.get_gen_slider_width());
    let y = ui.get_gen_rows_y() + ui.get_gen_row_height() / 2.0;
    assert!(w > 50.0, "slider width {w}");
    drag(&mut overlay, (x + 0.25 * w, y), (x + 0.75 * w, y));
    let edits = edits.borrow();
    assert!(edits.len() >= 2, "edits {edits:?}");
    assert!(edits.iter().all(|(k, _)| k == "uplift"));
    let last = edits.last().unwrap().1;
    assert!((last - 0.75).abs() < 0.02, "last value {last}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn preset_dropdown_fires_preset_chosen() {
    let mut overlay = generator();
    let chosen = Rc::new(std::cell::RefCell::new(Vec::new()));
    let c = chosen.clone();
    overlay.ui().on_gen_preset_chosen(move |i| c.borrow_mut().push(i));
    let ui = overlay.ui();
    let (x, top, h) = (ui.get_gen_preset_x(), ui.get_gen_preset_top(), ui.get_gen_preset_height());
    click(&mut overlay, x, top + h / 2.0);
    assert!(chosen.borrow().is_empty());
    // Items are 28 px under the button: the second one.
    click(&mut overlay, x, top + h + 4.0 + 1.5 * 28.0);
    assert_eq!(*chosen.borrow(), [1]);
    assert_eq!(overlay.ui().get_gen_preset(), 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn play_and_back_fire() {
    let mut overlay = generator();
    let (play, back) = (counter(), counter());
    let (p, b) = (play.clone(), back.clone());
    overlay.ui().on_gen_play(move || p.set(p.get() + 1));
    overlay.ui().on_back(move || b.set(b.get() + 1));
    let (px, py) = (overlay.ui().get_gen_play_x(), overlay.ui().get_gen_play_y());
    let (bx, by) = (overlay.ui().get_back_button_x(), overlay.ui().get_back_button_y());
    click(&mut overlay, px, py);
    assert_eq!((play.get(), back.get()), (1, 0));
    click(&mut overlay, bx, by);
    assert_eq!((play.get(), back.get()), (1, 1));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn forging_is_modal_and_cancel_fires() {
    let mut overlay = generator();
    let (play, cancel) = (counter(), counter());
    let (p, c) = (play.clone(), cancel.clone());
    overlay.ui().on_gen_play(move || p.set(p.get() + 1));
    overlay.ui().on_gen_cancel_forge(move || c.set(c.get() + 1));
    overlay.ui().set_gen_forging(true);
    overlay.ui().set_gen_forge_progress(0.3);
    overlay.ui().set_gen_forge_label("Eroding 150 / 500".into());
    overlay.render();
    let (px, py) = (overlay.ui().get_gen_play_x(), overlay.ui().get_gen_play_y());
    click(&mut overlay, px, py);
    assert_eq!(play.get(), 0, "Play is under the modal");
    let (cx, cy) = (overlay.ui().get_gen_cancel_x(), overlay.ui().get_gen_cancel_y());
    click(&mut overlay, cx, cy);
    assert_eq!(cancel.get(), 1);
}

// --- The world list (screen 2) ---

use slint_ui::slint::ComponentHandle;
use slint_ui::WorldRow;

fn world_row(name: &str, loadable: bool, legacy: bool) -> WorldRow {
    WorldRow {
        name: name.into(),
        detail: "seed 0x2F1A, 16 km, 2026-10-30".into(),
        thumb: Default::default(),
        loadable,
        reason: if loadable { "".into() } else { "world file version 1, this build reads 2 — re-forge it".into() },
        legacy,
    }
}

/// The list screen with `(name, loadable, legacy)` rows; the first selected.
fn loader(rows: &[(&str, bool, bool)]) -> Overlay<FrontendUi> {
    let mut overlay = overlay();
    overlay.ui().set_screen(2);
    let worlds: Vec<WorldRow> = rows.iter().map(|r| world_row(r.0, r.1, r.2)).collect();
    let labels: Vec<SharedString> = rows.iter().map(|r| SharedString::from(r.0)).collect();
    overlay.ui().set_load_worlds(ModelRc::new(VecModel::from(worlds)));
    overlay.ui().set_load_labels(ModelRc::new(VecModel::from(labels)));
    overlay.ui().set_load_selected(0);
    overlay.render();
    overlay
}

fn click_row(overlay: &mut Overlay<FrontendUi>, i: usize) {
    let ui = overlay.ui();
    let (x, y) = (ui.get_load_row_x(), ui.get_load_row0_y() + i as f32 * ui.get_load_row_pitch());
    click(overlay, x, y);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn unloadable_row_disables_load() {
    let mut overlay = loader(&[("Broken", false, false), ("Fine", true, false)]);
    let (load, delete) = (counter(), counter());
    let (l, d) = (load.clone(), delete.clone());
    overlay.ui().on_load_load(move || l.set(l.get() + 1));
    overlay.ui().on_load_delete(move || d.set(d.get() + 1));
    let (lx, ly) = (overlay.ui().get_load_load_x(), overlay.ui().get_load_load_y());
    click(&mut overlay, lx, ly);
    assert_eq!(load.get(), 0, "Load is disabled on the unloadable row");
    // Delete still works on it (it is the way to get rid of a stale world).
    let (dx, dy) = (overlay.ui().get_load_delete_x(), overlay.ui().get_load_delete_y());
    click(&mut overlay, dx, dy);
    assert_eq!(delete.get(), 1);
    overlay.ui().set_load_confirm_delete(false);
    overlay.ui().set_load_selected(1);
    overlay.render();
    click(&mut overlay, lx, ly);
    assert_eq!(load.get(), 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn legacy_row_cannot_be_deleted() {
    let mut overlay = loader(&[("Legacy", true, true)]);
    let delete = counter();
    let d = delete.clone();
    overlay.ui().on_load_delete(move || d.set(d.get() + 1));
    let (dx, dy) = (overlay.ui().get_load_delete_x(), overlay.ui().get_load_delete_y());
    click(&mut overlay, dx, dy);
    assert_eq!(delete.get(), 0);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn clicking_a_row_selects_it() {
    let mut overlay = loader(&[("A", true, false), ("B", true, false)]);
    let picked = Rc::new(std::cell::RefCell::new(Vec::new()));
    let p = picked.clone();
    overlay.ui().on_load_select(move |i| p.borrow_mut().push(i));
    click_row(&mut overlay, 1);
    assert_eq!(*picked.borrow(), [1]);
    assert_eq!(overlay.ui().get_load_selected(), 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn double_click_loads() {
    let mut overlay = loader(&[("A", true, false), ("B", true, false)]);
    let load = counter();
    let l = load.clone();
    overlay.ui().on_load_load(move || l.set(l.get() + 1));
    click_row(&mut overlay, 1);
    assert_eq!(load.get(), 0, "one click only selects");
    click_row(&mut overlay, 1);
    assert_eq!(load.get(), 1);
    assert_eq!(overlay.ui().get_load_selected(), 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn double_click_on_unloadable_row_does_not_load() {
    let mut overlay = loader(&[("Broken", false, false)]);
    let load = counter();
    let l = load.clone();
    overlay.ui().on_load_load(move || l.set(l.get() + 1));
    click_row(&mut overlay, 0);
    click_row(&mut overlay, 0);
    assert_eq!(load.get(), 0);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn delete_asks_first() {
    let mut overlay = loader(&[("A", true, false)]);
    let (delete, yes, no) = (counter(), counter(), counter());
    let (d, y, n) = (delete.clone(), yes.clone(), no.clone());
    overlay.ui().on_load_delete(move || d.set(d.get() + 1));
    overlay.ui().on_load_confirm_delete_yes(move || y.set(y.get() + 1));
    overlay.ui().on_load_confirm_delete_no(move || n.set(n.get() + 1));
    // The client turns `delete` into the confirm modal; do the same here.
    let ui = overlay.ui();
    let weak = ui.as_weak();
    ui.on_load_delete(move || weak.unwrap().set_load_confirm_delete(true));
    let (dx, dy) = (ui.get_load_delete_x(), ui.get_load_delete_y());
    click(&mut overlay, dx, dy);
    assert!(overlay.ui().get_load_confirm_delete());
    assert_eq!(yes.get(), 0, "nothing deleted until yes");
    let (nx, ny) = (overlay.ui().get_load_no_x(), overlay.ui().get_load_no_y());
    click(&mut overlay, nx, ny);
    assert_eq!((yes.get(), no.get()), (0, 1));
    overlay.ui().set_load_confirm_delete(true);
    overlay.render();
    let (yx, yy) = (overlay.ui().get_load_yes_x(), overlay.ui().get_load_yes_y());
    click(&mut overlay, yx, yy);
    assert_eq!(yes.get(), 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn confirm_modal_blocks_the_list() {
    let mut overlay = loader(&[("A", true, false), ("B", true, false)]);
    overlay.ui().set_load_confirm_delete(true);
    overlay.render();
    click_row(&mut overlay, 1);
    assert_eq!(overlay.ui().get_load_selected(), 0, "the modal swallows the click");
}
