//! The Esc menu and the world map window, headless: real pointer events into
//! `GameUi` press the Play button and pick the tile under the pointer.

use slint_ui::slint::ComponentHandle;
use slint_ui::{GameUi, Overlay, WorldMapConfig};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

fn overlay() -> Overlay<GameUi> {
    let mut overlay = Overlay::new().expect("game UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    overlay
}

/// Regression: World-map selection/markers drifted off their tile when zooming; seen IN PLAY by the owner (session 0f9dcc3c, 2026-10-30 10:21: "if I zoom in and out a bit the selection is not in line with the tile"), fixed in 5cc6a492
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

