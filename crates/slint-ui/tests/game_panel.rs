//! The top-right menu column (`ui/menu-column.slint`) and the stub windows it
//! opens, headless: every button can be clicked inside the window, the
//! context box draws nothing when idle, and a stub window's Close works. The
//! column is pinned to the window's right edge, so a row wider than it would
//! run off screen.
use std::cell::Cell;
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::LogicalPosition;
use slint_ui::{GameUi, Overlay};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
/// The scan covers the top-right of the window, where the column sits.
const SCAN: u32 = 400;
const STEP: usize = 6;

fn click(overlay: &Overlay<GameUi>, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    overlay.dispatch(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
}

fn counter() -> (Rc<Cell<u32>>, impl Fn() + 'static) {
    let n = Rc::new(Cell::new(0));
    let m = n.clone();
    (n, move || m.set(m.get() + 1))
}

fn overlay() -> Overlay<GameUi> {
    let mut overlay = Overlay::new().expect("game UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    overlay
}

/// Clicks across the top-right `SCAN`² corner.
fn scan_clicks(overlay: &Overlay<GameUi>) {
    for y in (0..SCAN).step_by(STEP) {
        for x in (WIDTH - SCAN..WIDTH).step_by(STEP) {
            click(overlay, x as f32, y as f32);
        }
    }
}

/// The column is flush with the window's top-right corner (its buttons reach
/// the last pixel column).
fn assert_flush_in_the_corner(overlay: &Overlay<GameUi>) {
    assert!(overlay.opaque_at(WIDTH - 1, 2), "menu column flush with the right edge");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn menu_buttons_open_windows() {
    let mut overlay = overlay();
    let (settings, on_settings) = counter();
    let (encyclopedia, on_encyclopedia) = counter();
    overlay.ui().on_open_settings(on_settings);
    overlay.ui().on_open_encyclopedia(on_encyclopedia);
    overlay.render();
    assert_flush_in_the_corner(&overlay);
    scan_clicks(&overlay);
    assert!(settings.get() > 0, "Settings reachable");
    assert!(encyclopedia.get() > 0, "Encyclopedia reachable");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn context_box_absent_when_idle() {
    let mut overlay = overlay();
    overlay.ui().set_selected_count(0);
    overlay.ui().set_tool_name("".into());
    overlay.render();
    for y in 80..SCAN {
        for x in WIDTH - SCAN..WIDTH {
            assert!(!overlay.opaque_at(x, y), "idle context box draws at ({x}, {y})");
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn context_box_shows_the_active_tool() {
    let mut overlay = overlay();
    overlay.ui().set_tool_name("Dig".into());
    overlay.render();
    let drawn = (80..SCAN).any(|y| (WIDTH - SCAN..WIDTH).any(|x| overlay.opaque_at(x, y)));
    assert!(drawn, "a tool shows the context box");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn selection_controls_reachable() {
    for following in [false, true] {
        let mut overlay = overlay();
        let (clear, on_clear) = counter();
        let (follow, on_follow) = counter();
        overlay.ui().on_clear_selection(on_clear);
        overlay.ui().on_follow_clicked(on_follow);
        overlay.ui().set_selected_count(1);
        overlay.ui().set_following(following);
        overlay.render();
        assert_flush_in_the_corner(&overlay);
        scan_clicks(&overlay);
        assert!(clear.get() > 0, "Clear selection reachable (following {following})");
        assert!(follow.get() > 0, "Follow reachable (following {following})");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stub_window_closes() {
    let mut overlay = overlay();
    let (closed, on_close) = counter();
    overlay.ui().on_close_window(on_close);
    overlay.ui().set_settings_open(true);
    overlay.render();
    click(&overlay, overlay.ui().get_stub_close_x(), overlay.ui().get_stub_close_y());
    assert_eq!(closed.get(), 1, "Close on the Settings window");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stop_following_reachable_with_nothing_selected() {
    let mut overlay = overlay();
    let (follow, on_follow) = counter();
    overlay.ui().on_follow_clicked(on_follow);
    overlay.ui().set_selected_count(0);
    overlay.ui().set_following(true);
    overlay.render();
    scan_clicks(&overlay);
    assert!(follow.get() > 0, "Stop following reachable after the selection was cleared");
}
