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

    // One notch up zooms in one level (1 → 2), keeping the map point under
    // the cursor where it was, to the pixel the pan snaps to.
    let config = overlay.ui().global::<WorldMapConfig>();
    let notch = config.get_wheel_notch();
    let (zoom_min, zoom_max) = (config.get_zoom_min(), config.get_zoom_max());
    let cursor = (200.0, 150.0);
    let position = LogicalPosition::new(x0 + cursor.0, y0 + cursor.1);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: notch });
    overlay.render();
    let zoom = overlay.ui().get_map_zoom();
    assert_eq!(zoom, 2.0);
    let under = ((cursor.0 - overlay.ui().get_map_pan_x()) / zoom, (cursor.1 - overlay.ui().get_map_pan_y()) / zoom);
    assert!((under.0 - 150.0).abs() <= 0.5 && (under.1 - 120.0).abs() <= 0.5, "{under:?}");
    assert_eq!(overlay.ui().get_map_pan_x().fract(), 0.0, "pans stay on whole pixels");

    // Less than a notch of scroll (a touchpad) waits until a notch adds up.
    overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: notch / 2.0 });
    assert_eq!(overlay.ui().get_map_zoom(), 2.0);
    overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: notch / 2.0 });
    assert_eq!(overlay.ui().get_map_zoom(), 4.0);

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

/// The home marker frames a tile's visible cell — from its left edge up to
/// its grid line — at every zoom level, with the map's left edge in view or
/// panned off it. (Markers used to cover the grid line on the right and
/// bottom, and zooms other than powers of two drift the image off them.)
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn markers_frame_the_tile_cell_at_every_zoom() {
    use slint_ui::slint::{Image, Rgba8Pixel, SharedPixelBuffer};
    let mut overlay = overlay();
    let config = overlay.ui().global::<WorldMapConfig>();
    let (tile, grid) = (config.get_tile_px() as u32, config.get_grid_px() as u32);
    let (cols, rows) = (32u32, 32u32);
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(cols * tile, rows * tile);
    let w = buf.width();
    for (i, p) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i as u32 % w % tile, i as u32 / w % tile);
        let line = x >= tile - grid || y >= tile - grid;
        *p = if line { Rgba8Pixel { r: 0, g: 0, b: 0, a: 255 } } else { Rgba8Pixel { r: 0, g: 160, b: 0, a: 255 } };
    }
    let ui = overlay.ui();
    ui.set_map_image(Image::from_rgba8(buf));
    ui.set_map_open(true);
    ui.set_map_available(true);
    ui.set_map_cols(cols as i32);
    ui.set_map_rows(rows as i32);
    let (col, row) = (12, 3);
    ui.set_home_col(col);
    ui.set_home_row(row);
    for zoom in [1.0f32, 2.0, 4.0, 8.0] {
        let len = tile as f32 * zoom;
        // The map's left edge in view, then panned off to the left.
        for pan_x in [10.0, (20.0 - (col - 2) as f32 * len).round()] {
            let ui = overlay.ui();
            ui.set_map_zoom(zoom);
            ui.set_map_pan_x(pan_x);
            ui.set_map_pan_y(10.0);
            overlay.render();
            let vy = overlay.ui().get_map_view_y();
            let (width, _) = overlay.size();
            let px = overlay.pixels();
            // A row through the tile's middle, below the marker's top
            // border, and one two tiles down with no marker over the image.
            let y = (vy + 10.0 + (row as f32 + 0.5) * len) as u32;
            let clear_y = (vy + 10.0 + (row as f32 + 2.5) * len) as u32;
            let at = |x: u32| px[(y * width + x) as usize];
            let black = |x: u32| { let c = px[(clear_y * width + x) as usize]; c.green == 0 && c.blue == 0 };
            let cyan = |x: u32| { let c = at(x); c.blue > 200 && c.red < 120 };
            let left = (0..width).find(|&x| cyan(x)).expect("home marker drawn");
            // Past the left border and the inside to the end of the right border.
            let mut right = left;
            while cyan(right) { right += 1; }
            while !cyan(right) { right += 1; }
            while cyan(right) { right += 1; }
            // The image's cell: from inside the previous tile's cell, across
            // its grid line, to the start of this tile's line.
            let mut cell_left = (left as f32 - len / 2.0) as u32;
            while !black(cell_left) { cell_left += 1; }
            while black(cell_left) { cell_left += 1; }
            let mut line = cell_left;
            while !black(line) { line += 1; }
            assert_eq!(left, cell_left, "zoom {zoom}, pan {pan_x}: marker left vs cell left");
            assert_eq!(right, line, "zoom {zoom}, pan {pan_x}: marker right vs grid line");
        }
    }
}

fn open_map_counting_closes(overlay: &mut Overlay<GameUi>) -> Rc<RefCell<u32>> {
    let closed = Rc::new(RefCell::new(0));
    let c = closed.clone();
    overlay.ui().on_close_map(move || *c.borrow_mut() += 1);
    overlay.ui().set_map_open(true);
    overlay.ui().set_map_available(true);
    overlay.ui().set_map_cols(4);
    overlay.ui().set_map_rows(4);
    overlay.render();
    closed
}

/// Regression: today's map closes with its Close button.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn non_modal_map_closes() {
    let mut overlay = overlay();
    let closed = open_map_counting_closes(&mut overlay);
    let (x, y) = (overlay.ui().get_close_button_x(), overlay.ui().get_close_button_y());
    assert!(overlay.opaque_at(x as u32, y as u32), "Close button drawn at ({x}, {y})");
    click(&mut overlay, x, y);
    assert_eq!(*closed.borrow(), 1);
}

/// Before the first Play (the arrival map, `map-modal`) there is no Close:
/// the map is the start screen. (Esc / M are Bevy-side, in the client.)
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn modal_map_cannot_be_closed() {
    let mut overlay = overlay();
    let closed = open_map_counting_closes(&mut overlay);
    // Where the button is while the map is not modal.
    let (x, y) = (overlay.ui().get_close_button_x(), overlay.ui().get_close_button_y());
    overlay.ui().set_map_modal(true);
    overlay.render();
    click(&mut overlay, x, y);
    assert_eq!(*closed.borrow(), 0, "a modal map has no Close button");
    let (px, py) = (overlay.ui().get_play_button_x(), overlay.ui().get_play_button_y());
    assert!(overlay.opaque_at(px as u32, py as u32), "Play is still there");
}

/// The Esc menu over the arrival map offers only Main menu.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn modal_menu_offers_only_main_menu() {
    let mut overlay = overlay();
    overlay.ui().set_menu_open(true);
    overlay.render();
    let drawn = |overlay: &Overlay<GameUi>| overlay.pixels().iter().filter(|p| p.alpha > 0).count();
    let full = drawn(&overlay);
    overlay.ui().set_map_modal(true);
    overlay.render();
    let modal = drawn(&overlay);
    assert!(modal < full, "fewer buttons ({modal} vs {full} pixels)");
    let left = Rc::new(RefCell::new(0));
    let l = left.clone();
    overlay.ui().on_main_menu(move || *l.borrow_mut() += 1);
    let (x, y) = (overlay.ui().get_main_menu_button_x(), overlay.ui().get_main_menu_button_y());
    click(&mut overlay, x, y);
    assert_eq!(*left.borrow(), 1, "Main menu fires");
}

/// The tile (column, row) under view-relative point `(x, y)`, from the map's
/// pan and zoom as the UI holds them.
fn tile_under(overlay: &Overlay<GameUi>, x: f32, y: f32) -> (i32, i32) {
    let ui = overlay.ui();
    let len = ui.global::<WorldMapConfig>().get_tile_px() as f32 * ui.get_map_zoom();
    (((x - ui.get_map_pan_x()) / len).floor() as i32, ((y - ui.get_map_pan_y()) / len).floor() as i32)
}

/// A 100 km world (196 × 196 tiles) does not fit the view at 1:1: fitting
/// shows all of it, centred, at the largest zoom step that fits. From there
/// every wheel notch in, to the closest zoom, and back out to the farthest,
/// keeps the tile under the cursor under it, and a click there selects it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn world_map_zoom_keeps_tile_under_cursor() {
    let mut overlay = overlay();
    let selected = Rc::new(RefCell::new(Vec::new()));
    let s = selected.clone();
    overlay.ui().on_select_tile(move |col, row| s.borrow_mut().push((col, row)));
    let side = 196;
    let ui = overlay.ui();
    ui.set_map_open(true);
    ui.set_map_available(true);
    ui.set_map_cols(side);
    ui.set_map_rows(side);
    overlay.render();
    overlay.ui().invoke_fit_map();
    overlay.render();

    let ui = overlay.ui();
    let config = ui.global::<WorldMapConfig>();
    let (tile, notch) = (config.get_tile_px() as f32, config.get_wheel_notch());
    let (zoom_min, zoom_max) = (config.get_zoom_min(), config.get_zoom_max());
    let (vw, vh) = (ui.get_map_view_width(), ui.get_map_view_height());
    let fit = ui.get_map_zoom();
    let map_len = side as f32 * tile * fit;
    assert!(map_len <= vw && map_len <= vh, "fitted: {map_len} px in a {vw}x{vh} view");
    assert!(fit == zoom_min || 2.0 * map_len > vw.min(vh), "the largest zoom that fits ({fit})");
    assert!(fit < 1.0, "196 tiles do not fit at 1:1");
    assert_eq!(ui.get_map_pan_x(), ((vw - map_len) / 2.0).round(), "centred");
    assert_eq!(ui.get_map_pan_y(), ((vh - map_len) / 2.0).round(), "centred");

    // The cursor a quarter pixel off the middle of an off-centre tile.
    let (col, row) = (61, 133);
    let len = tile * fit;
    let cursor = (
        ui.get_map_pan_x() + (col as f32 + 0.5) * len + 0.25,
        ui.get_map_pan_y() + (row as f32 + 0.5) * len + 0.25,
    );
    assert_eq!(tile_under(&overlay, cursor.0, cursor.1), (col, row));
    let (x0, y0) = (ui.get_map_view_x(), ui.get_map_view_y());
    let position = LogicalPosition::new(x0 + cursor.0, y0 + cursor.1);
    overlay.dispatch(WindowEvent::PointerMoved { position });

    let mut check = |overlay: &mut Overlay<GameUi>, delta: f32, steps: usize| {
        for _ in 0..steps {
            overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: delta });
            overlay.render();
            let zoom = overlay.ui().get_map_zoom();
            assert_eq!(tile_under(overlay, cursor.0, cursor.1), (col, row), "zoom {zoom}");
            selected.borrow_mut().clear();
            click(overlay, position.x, position.y);
            assert_eq!(*selected.borrow(), [(col, row)], "a click at zoom {zoom}");
        }
    };
    let ins = (zoom_max / fit).log2().round() as usize;
    check(&mut overlay, notch, ins);
    assert_eq!(overlay.ui().get_map_zoom(), zoom_max);
    let outs = (zoom_max / zoom_min).log2().round() as usize;
    check(&mut overlay, -notch, outs);
    assert_eq!(overlay.ui().get_map_zoom(), zoom_min);
}

/// Panning stops with the map still on screen: half the view stays covered
/// by a map bigger than it, and a smaller map stays wholly inside it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn panning_is_clamped_to_the_map() {
    let mut overlay = overlay();
    let ui = overlay.ui();
    ui.set_map_open(true);
    ui.set_map_available(true);
    ui.set_map_cols(196);
    ui.set_map_rows(196);
    ui.set_map_zoom(1.0);
    overlay.render();
    let ui = overlay.ui();
    let (x0, y0) = (ui.get_map_view_x(), ui.get_map_view_y());
    let (vw, vh) = (ui.get_map_view_width(), ui.get_map_view_height());
    let map_len = 196.0 * ui.global::<WorldMapConfig>().get_tile_px() as f32;
    let left = PointerEventButton::Left;
    let mut drag = |overlay: &mut Overlay<GameUi>, dx: f32, dy: f32| {
        let start = LogicalPosition::new(x0 + vw / 2.0, y0 + vh / 2.0);
        let end = LogicalPosition::new(start.x + dx, start.y + dy);
        overlay.dispatch(WindowEvent::PointerMoved { position: start });
        overlay.dispatch(WindowEvent::PointerPressed { position: start, button: left });
        overlay.dispatch(WindowEvent::PointerMoved { position: end });
        overlay.dispatch(WindowEvent::PointerReleased { position: end, button: left });
        overlay.render();
    };
    // Far right and down: the map's top-left stops half a view in.
    for _ in 0..4 {
        drag(&mut overlay, 400.0, 300.0);
    }
    let (px, py) = (overlay.ui().get_map_pan_x(), overlay.ui().get_map_pan_y());
    assert!((px - vw / 2.0).abs() <= 1.0 && (py - vh / 2.0).abs() <= 1.0, "({px}, {py}) in {vw}x{vh}");
    // Far left and up: the map's bottom-right stops half a view in.
    for _ in 0..12 {
        drag(&mut overlay, -400.0, -300.0);
    }
    let (px, py) = (overlay.ui().get_map_pan_x(), overlay.ui().get_map_pan_y());
    assert!((px + map_len - vw / 2.0).abs() <= 1.0, "{px}");
    assert!((py + map_len - vh / 2.0).abs() <= 1.0, "{py}");
}

/// The layer buttons, one per name the client gives, pick `map-layer`.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn layer_buttons_pick_the_layer() {
    use slint_ui::slint::{ModelRc, SharedString, VecModel};
    let mut overlay = overlay();
    let names: Vec<SharedString> = ["Biomes", "Relief", "Temperature", "Rain", "Regions"].map(Into::into).to_vec();
    let ui = overlay.ui();
    ui.set_map_layer_names(ModelRc::new(VecModel::from(names)));
    ui.set_map_open(true);
    ui.set_map_available(true);
    ui.set_map_cols(4);
    ui.set_map_rows(4);
    overlay.render();
    assert_eq!(overlay.ui().get_map_layer(), 0, "biomes by default");
    for i in [3, 1, 4, 0] {
        let ui = overlay.ui();
        let (x, y) = (ui.get_layer_button_x(), ui.get_layer_buttons_top() + (i as f32 + 0.5) * ui.get_layer_button_pitch());
        assert!(overlay.opaque_at(x as u32, y as u32), "layer button {i} drawn at ({x}, {y})");
        click(&mut overlay, x, y);
        assert_eq!(overlay.ui().get_map_layer(), i);
    }
}
