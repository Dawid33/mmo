//! The F3 dev panel (`DevPanelUi`), headless: drawn top-right only while
//! open, and real pointer events on a checkbox / the button fire its callback.
use std::cell::RefCell;
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::{LogicalPosition, ModelRc, VecModel};
use slint_ui::{DevPanelUi, DevStat, DevToggle, Overlay};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

fn panel() -> Overlay<DevPanelUi> {
    let mut overlay = Overlay::with(DevPanelUi::new).expect("dev panel");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    let ui = overlay.ui();
    let stats = vec![DevStat { label: "FPS".into(), value: "60".into() }];
    let toggles = vec![
        DevToggle { label: "Region boundaries (F8)".into(), checked: false },
        DevToggle { label: "Chunk grid (F5)".into(), checked: false },
    ];
    ui.set_stats(ModelRc::new(VecModel::from(stats)));
    ui.set_toggles(ModelRc::new(VecModel::from(toggles)));
    overlay
}

fn click(overlay: &mut Overlay<DevPanelUi>, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    overlay.dispatch(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    overlay.render();
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn closed_panel_draws_nothing() {
    let mut panel = panel();
    panel.render();
    assert!(!panel.opaque_at(WIDTH - 10, 10));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn open_panel_draws_top_right() {
    let mut panel = panel();
    panel.ui().set_open(true);
    panel.render();
    assert!(panel.opaque_at(WIDTH - 20, 20));
    assert!(!panel.opaque_at(10, HEIGHT - 10));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn clicking_a_checkbox_fires_toggle() {
    let mut panel = panel();
    let toggled = Rc::new(RefCell::new(Vec::new()));
    let t = toggled.clone();
    panel.ui().on_toggle(move |i, v| t.borrow_mut().push((i, v)));
    panel.ui().set_open(true);
    panel.render();

    let ui = panel.ui();
    let row = ui.get_toggle_row_height();
    let (x, y) = (ui.get_toggles_x() + 6.0, ui.get_toggles_y() + row + row / 2.0);
    click(&mut panel, x, y);
    assert_eq!(*toggled.borrow(), vec![(1, true)]);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn clicking_the_button_fires_complete_orders() {
    let mut panel = panel();
    let fired = Rc::new(RefCell::new(0));
    let f = fired.clone();
    panel.ui().on_complete_orders(move || *f.borrow_mut() += 1);
    panel.ui().set_open(true);
    panel.render();

    let (x, y) = (panel.ui().get_complete_button_x(), panel.ui().get_complete_button_y());
    click(&mut panel, x, y);
    assert_eq!(*fired.borrow(), 1);
}

/// After a click, the box must still follow the model the client pushes
/// (an F-key flipping the same flag), not keep the clicked state.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn checkbox_follows_the_model_after_a_click() {
    let mut panel = panel();
    let toggled = Rc::new(RefCell::new(Vec::new()));
    let t = toggled.clone();
    panel.ui().on_toggle(move |i, v| t.borrow_mut().push((i, v)));
    panel.ui().set_open(true);
    panel.render();
    let ui = panel.ui();
    let row = ui.get_toggle_row_height();
    let (x, y) = (ui.get_toggles_x() + 6.0, ui.get_toggles_y() + row / 2.0);

    click(&mut panel, x, y); // on
    // The client turns it back off (the F-key) and pushes a fresh model.
    let toggles = vec![
        DevToggle { label: "Region boundaries (F8)".into(), checked: false },
        DevToggle { label: "Chunk grid (F5)".into(), checked: false },
    ];
    panel.ui().set_toggles(ModelRc::new(VecModel::from(toggles)));
    panel.render();
    click(&mut panel, x, y); // on again, not off
    assert_eq!(*toggled.borrow(), vec![(0, true), (0, true)]);
}
