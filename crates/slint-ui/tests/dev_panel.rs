//! The fortress panel's Debug tab (`GameUi`, `ui/debug-tab.slint`),
//! headless: on the tab row only when `debug-available`, the open tab draws
//! under the panel's row, and real pointer events on a checkbox / the button
//! fire its callbacks.
use std::cell::RefCell;
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::{LogicalPosition, ModelRc, VecModel};
use slint_ui::{DevStat, DevTickRow, DevToggle, GameUi, Overlay};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
/// `GameUi.fortress-tab` of the Debug tab (client: `dev_ui::DEBUG_TAB`).
const DEBUG_TAB: i32 = 4;

fn toggles(checked: bool) -> ModelRc<DevToggle> {
    ModelRc::new(VecModel::from(vec![
        DevToggle { label: "Region boundaries (F8)".into(), checked },
        DevToggle { label: "Chunk grid (F5)".into(), checked: false },
    ]))
}

fn panel() -> Overlay<GameUi> {
    let mut overlay = Overlay::new().expect("game UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    let ui = overlay.ui();
    ui.set_debug_available(true);
    let stats = vec![DevStat { label: "FPS".into(), value: "60".into() }];
    ui.set_dev_stats(ModelRc::new(VecModel::from(stats)));
    ui.set_dev_toggles(toggles(false));
    overlay.render();
    overlay
}

fn open(panel: &mut Overlay<GameUi>) {
    panel.ui().set_fortress_tab(DEBUG_TAB);
    panel.render();
}

fn click(overlay: &mut Overlay<GameUi>, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    overlay.dispatch(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    overlay.render();
}

/// The x of the first tab-row point whose click opens tab `index`, or None.
fn find_tab(overlay: &mut Overlay<GameUi>, index: i32) -> Option<f32> {
    let y = overlay.ui().get_fortress_tabs_y();
    let start = overlay.ui().get_fortress_tabs_x() + 2.0;
    let end = overlay.ui().get_fortress_tabs_end();
    let mut x = start;
    while x < end {
        overlay.ui().set_fortress_tab(-1);
        overlay.render();
        click(overlay, x, y);
        if overlay.ui().get_fortress_tab() == index {
            return Some(x);
        }
        x += 8.0;
    }
    None
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn debug_tab_only_when_available() {
    let mut panel = panel();
    assert!(find_tab(&mut panel, DEBUG_TAB).is_some(), "Debug is on the tab row");
    let mut plain = Overlay::<GameUi>::new().expect("game UI");
    plain.resize(WIDTH, HEIGHT, 1.0);
    plain.render();
    assert!(find_tab(&mut plain, DEBUG_TAB).is_none(), "no Debug tab without dev_ui");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tab_row_clear_of_the_menu_column() {
    let mut panel = panel();
    open(&mut panel);
    // The game UI's menu column (260 px, 12 px from the edge) keeps the
    // window's right edge; the tab row widened by Debug (and its figures)
    // must end before it.
    let right = panel.ui().get_fortress_panel_right();
    let column_left = (WIDTH - 12 - 260) as f32;
    assert!(right <= column_left, "fortress panel ends at {right}, the menu column starts at {column_left}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn clicking_a_checkbox_fires_toggle() {
    let mut panel = panel();
    let toggled = Rc::new(RefCell::new(Vec::new()));
    let t = toggled.clone();
    panel.ui().on_dev_toggle(move |i, v| t.borrow_mut().push((i, v)));
    open(&mut panel);

    let ui = panel.ui();
    let row = ui.get_dev_toggle_row_height();
    let (x, y) = (ui.get_dev_toggles_x() + 6.0, ui.get_dev_toggles_y() + row + row / 2.0);
    click(&mut panel, x, y);
    assert_eq!(*toggled.borrow(), vec![(1, true)]);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn clicking_the_button_fires_complete_orders() {
    let mut panel = panel();
    let fired = Rc::new(RefCell::new(0));
    let f = fired.clone();
    panel.ui().on_dev_complete_orders(move || *f.borrow_mut() += 1);
    open(&mut panel);

    let (x, y) = (panel.ui().get_dev_complete_x(), panel.ui().get_dev_complete_y());
    click(&mut panel, x, y);
    assert_eq!(*fired.borrow(), 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn clicking_drop_body_fires_its_callback_only() {
    let mut panel = panel();
    let dropped = Rc::new(RefCell::new(0));
    let completed = Rc::new(RefCell::new(0));
    let (d, c) = (dropped.clone(), completed.clone());
    panel.ui().on_dev_drop_body(move || *d.borrow_mut() += 1);
    panel.ui().on_dev_complete_orders(move || *c.borrow_mut() += 1);
    open(&mut panel);

    let (x, y) = (panel.ui().get_dev_drop_x(), panel.ui().get_dev_drop_y());
    click(&mut panel, x, y);
    assert_eq!((*dropped.borrow(), *completed.borrow()), (1, 0));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn collapsed_tab_takes_no_clicks() {
    let mut panel = panel();
    let fired = Rc::new(RefCell::new(0));
    let f = fired.clone();
    panel.ui().on_dev_complete_orders(move || *f.borrow_mut() += 1);
    open(&mut panel);
    let (x, y) = (panel.ui().get_dev_complete_x(), panel.ui().get_dev_complete_y());
    panel.ui().set_fortress_tab(-1);
    panel.render();
    click(&mut panel, x, y);
    assert_eq!(*fired.borrow(), 0);
    assert!(!panel.opaque_at(x as u32, y as u32), "collapsed: nothing drawn where the button was");
}

/// After a click, the box must still follow the model the client pushes
/// (an F-key flipping the same flag), not keep the clicked state.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn checkbox_follows_the_model_after_a_click() {
    let mut panel = panel();
    let toggled = Rc::new(RefCell::new(Vec::new()));
    let t = toggled.clone();
    panel.ui().on_dev_toggle(move |i, v| t.borrow_mut().push((i, v)));
    open(&mut panel);
    let ui = panel.ui();
    let row = ui.get_dev_toggle_row_height();
    let (x, y) = (ui.get_dev_toggles_x() + 6.0, ui.get_dev_toggles_y() + row / 2.0);

    click(&mut panel, x, y); // on
    // The client turns it back off (the F-key) and pushes a fresh model.
    panel.ui().set_dev_toggles(toggles(false));
    panel.render();
    click(&mut panel, x, y); // on again, not off
    assert_eq!(*toggled.borrow(), vec![(0, true), (0, true)]);
}

fn tick_row(region: &str) -> DevTickRow {
    DevTickRow {
        region: region.into(),
        mean: "1.23".into(),
        p50: "1.00".into(),
        p95: "2.50".into(),
        p99: "3.00".into(),
        max: "12.35".into(),
    }
}

/// A 3x3 fortress neighbourhood's tick table fits the page and the panel
/// still ends before the menu column.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tick_table_fits_nine_regions() {
    let mut panel = panel();
    let rows: Vec<DevTickRow> =
        (-1..=1).flat_map(|z| (-1..=1).map(move |x| tick_row(&format!("({x}, {z})")))).collect();
    panel.ui().set_dev_tick_rows(ModelRc::new(VecModel::from(rows)));
    open(&mut panel);
    let ui = panel.ui();
    assert!(ui.get_dev_ticks_x() > ui.get_dev_toggles_x(), "the table is right of the toggles");
    let right = ui.get_fortress_panel_right();
    assert!(right <= (WIDTH - 12 - 260) as f32, "fortress panel ends at {right}");
    if let Ok(path) = std::env::var("DEBUG_TAB_DUMP") {
        let rgba: Vec<u8> = panel.pixels().iter().flat_map(|p| [p.red, p.green, p.blue, p.alpha]).collect();
        std::fs::write(path, rgba).unwrap();
    }
}
