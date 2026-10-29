//! The Esc menu and the world map window, headless: real pointer events into
//! `GameUi` press the Play button and pick the tile under the pointer.
use std::cell::RefCell;
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::{ComponentHandle, LogicalPosition};
use slint_ui::{GameUi, Overlay, WorldMapConfig};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

fn overlay() -> Overlay<GameUi> {
    let mut overlay = Overlay::new().expect("game UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    overlay
}

fn click(overlay: &mut Overlay<GameUi>, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    overlay.dispatch(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    overlay.render();
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn play_button_fires_play() {
    let mut overlay = overlay();
    let played = Rc::new(RefCell::new(0));
    let p = played.clone();
    overlay.ui().on_play(move || *p.borrow_mut() += 1);
    overlay.ui().set_map_open(true);
    overlay.ui().set_map_available(true);
    overlay.ui().set_map_cols(4);
    overlay.ui().set_map_rows(4);
    overlay.ui().set_play_enabled(true);
    overlay.render();

    let (x, y) = (overlay.ui().get_play_button_x(), overlay.ui().get_play_button_y());
    assert!(overlay.opaque_at(x as u32, y as u32), "Play button drawn at ({x}, {y})");
    click(&mut overlay, x, y);
    assert_eq!(*played.borrow(), 1);

    overlay.ui().set_play_enabled(false);
    overlay.render();
    click(&mut overlay, x, y);
    assert_eq!(*played.borrow(), 1, "a disabled Play button does not fire");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn clicking_the_map_selects_a_tile() {
    let mut overlay = overlay();
    let selected = Rc::new(RefCell::new(Vec::new()));
    let s = selected.clone();
    overlay.ui().on_select_tile(move |col, row| s.borrow_mut().push((col, row)));
    overlay.ui().set_map_open(true);
    overlay.ui().set_map_available(true);
    overlay.ui().set_map_cols(4);
    overlay.ui().set_map_rows(4);
    overlay.ui().set_map_zoom(1.0);
    overlay.ui().set_map_pan_x(0.0);
    overlay.ui().set_map_pan_y(0.0);
    overlay.render();

    let tile = overlay.ui().global::<WorldMapConfig>().get_tile_px() as f32;
    let (x0, y0) = (overlay.ui().get_map_view_x(), overlay.ui().get_map_view_y());
    // The middle of tile (2, 1).
    click(&mut overlay, x0 + 2.5 * tile, y0 + 1.5 * tile);
    assert_eq!(*selected.borrow(), [(2, 1)]);
    assert_eq!((overlay.ui().get_selected_col(), overlay.ui().get_selected_row()), (2, 1));

    // Off the map's 4×4 tiles: nothing.
    click(&mut overlay, x0 + 6.5 * tile, y0 + 1.5 * tile);
    assert_eq!(*selected.borrow(), [(2, 1)]);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn closed_ui_draws_nothing() {
    let mut overlay = overlay();
    overlay.ui().set_panel_open(false);
    overlay.ui().set_tools_available(false);
    overlay.ui().set_menu_open(false);
    overlay.ui().set_map_open(false);
    overlay.render();
    assert!(!overlay.opaque_at(WIDTH / 2, HEIGHT / 2));

    // Open, each one covers the centre.
    overlay.ui().set_menu_open(true);
    overlay.render();
    assert!(overlay.opaque_at(WIDTH / 2, HEIGHT / 2), "menu drawn");
    overlay.ui().set_menu_open(false);
    overlay.ui().set_map_open(true);
    overlay.render();
    assert!(overlay.opaque_at(WIDTH / 2, HEIGHT / 2), "map drawn");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn dragging_pans_and_the_wheel_zooms_about_the_cursor() {
    let mut overlay = overlay();
    let selected = Rc::new(RefCell::new(Vec::new()));
    let s = selected.clone();
    overlay.ui().on_select_tile(move |col, row| s.borrow_mut().push((col, row)));
    overlay.ui().set_map_open(true);
    overlay.ui().set_map_available(true);
    overlay.ui().set_map_cols(64);
    overlay.ui().set_map_rows(64);
    overlay.render();
    let (x0, y0) = (overlay.ui().get_map_view_x(), overlay.ui().get_map_view_y());

    // A drag pans by the pointer's travel and selects nothing.
    let left = PointerEventButton::Left;
    let start = LogicalPosition::new(x0 + 100.0, y0 + 100.0);
    let end = LogicalPosition::new(x0 + 150.0, y0 + 130.0);
    overlay.dispatch(WindowEvent::PointerMoved { position: start });
    overlay.dispatch(WindowEvent::PointerPressed { position: start, button: left });
    overlay.dispatch(WindowEvent::PointerMoved { position: end });
    overlay.dispatch(WindowEvent::PointerReleased { position: end, button: left });
    overlay.render();
    assert_eq!((overlay.ui().get_map_pan_x(), overlay.ui().get_map_pan_y()), (50.0, 30.0));
    assert!(selected.borrow().is_empty(), "a drag is not a click");

    // One notch up zooms in by `zoom-step`, keeping the map point under the
    // cursor where it was.
    let config = overlay.ui().global::<WorldMapConfig>();
    let (step, notch) = (config.get_zoom_step(), config.get_wheel_notch());
    let (zoom_min, zoom_max) = (config.get_zoom_min(), config.get_zoom_max());
    let cursor = (200.0, 150.0);
    let position = LogicalPosition::new(x0 + cursor.0, y0 + cursor.1);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: notch });
    overlay.render();
    let zoom = overlay.ui().get_map_zoom();
    assert!((zoom - step).abs() < 1e-4, "zoom {zoom}");
    let under = ((cursor.0 - overlay.ui().get_map_pan_x()) / zoom, (cursor.1 - overlay.ui().get_map_pan_y()) / zoom);
    assert!((under.0 - 150.0).abs() < 1e-3 && (under.1 - 120.0).abs() < 1e-3, "{under:?}");

    // Zoom stops at its limits.
    for _ in 0..40 {
        overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: notch });
    }
    assert_eq!(overlay.ui().get_map_zoom(), zoom_max);
    for _ in 0..80 {
        overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: -notch });
    }
    assert_eq!(overlay.ui().get_map_zoom(), zoom_min);

    // Closed, the map takes no clicks.
    overlay.ui().set_map_open(false);
    overlay.render();
    click(&mut overlay, position.x, position.y);
    assert!(selected.borrow().is_empty());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn status_line_draws_only_with_text() {
    let mut overlay = overlay();
    overlay.ui().set_panel_open(false);
    overlay.ui().set_tools_available(false);
    overlay.render();
    let (x, y) = (overlay.ui().get_status_x(), overlay.ui().get_status_y());
    assert!(!overlay.opaque_at(x as u32, y as u32), "no status, no pixels");

    overlay.ui().set_game_status("Loading region\u{2026}".into());
    overlay.render();
    assert!(overlay.opaque_at(x as u32, y as u32), "status line drawn at ({x}, {y})");

    overlay.ui().set_game_status("".into());
    overlay.render();
    assert!(!overlay.opaque_at(x as u32, y as u32), "cleared again");
}
