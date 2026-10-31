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
