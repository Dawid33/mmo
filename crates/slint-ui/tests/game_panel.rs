//! The F9 Fortress panel, headless: every button in it can be clicked inside
//! the window. The panel is pinned to the window's right edge, so a row
//! wider than the panel runs off screen (the Adventure button once did).
use std::cell::Cell;
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::LogicalPosition;
use slint_ui::{GameUi, Overlay};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
/// The scan covers the top-right of the window, where the panel sits.
const SCAN: u32 = 400;
const STEP: usize = 6;
/// The panel sits 12 px in from the window's right edge (`game.slint`) and
/// its frame shadow reaches ~4 px past that; nothing may draw in the last
/// `MARGIN` columns.
const MARGIN: u32 = 4;

fn click(overlay: &Overlay<GameUi>, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    overlay.dispatch(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
}

/// Clicks over the panel's corner of the window; returns how often each of
/// (mode, clear, follow) fired.
fn scan(adventure: bool, following: bool) -> (u32, u32, u32) {
    let mut overlay = Overlay::new().expect("game UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    let (mode, clear, follow) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
    let (m, c, f) = (mode.clone(), clear.clone(), follow.clone());
    overlay.ui().on_toggle_mode(move || m.set(m.get() + 1));
    overlay.ui().on_clear_selection(move || c.set(c.get() + 1));
    overlay.ui().on_follow_clicked(move || f.set(f.get() + 1));
    overlay.ui().set_panel_open(true);
    overlay.ui().set_selected_count(1);
    overlay.ui().set_adventure(adventure);
    overlay.ui().set_following(following);
    overlay.render();
    for y in 0..SCAN {
        for x in WIDTH - MARGIN..WIDTH {
            assert!(!overlay.opaque_at(x, y), "panel draws past its right edge at ({x}, {y})");
        }
    }
    for y in (0..SCAN).step_by(STEP) {
        for x in (WIDTH - SCAN..WIDTH).step_by(STEP) {
            click(&overlay, x as f32, y as f32);
        }
    }
    (mode.get(), clear.get(), follow.get())
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn every_panel_button_is_on_screen() {
    for (adventure, following) in [(false, false), (true, true)] {
        let (mode, clear, follow) = scan(adventure, following);
        assert!(mode > 0, "mode button reachable (adventure {adventure})");
        assert!(clear > 0, "Clear selection reachable (following {following})");
        assert!(follow > 0, "Follow reachable (following {following})");
    }
}

