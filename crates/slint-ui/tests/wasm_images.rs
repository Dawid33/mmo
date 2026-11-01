//! wasm32 only (`just slint-wasm`, under node): the screens' embedded sprites draw in the
//! browser build. Slint hands embedded images to the browser on wasm32, which the software
//! renderer cannot draw (it panicked "not implemented: The image cannot be rendered");
//! `src/wasm_images.rs` decodes them in Rust instead.
#![cfg(target_arch = "wasm32")]

use slint_ui::{GameUi, Overlay};
use wasm_bindgen_test::wasm_bindgen_test;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
/// The F9 panel sits in the top-right corner of the window.
const SCAN: u32 = 400;

#[wasm_bindgen_test]
fn panel_sprites_draw() {
    let mut overlay: Overlay<GameUi> = Overlay::new().expect("game UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    overlay.ui().set_panel_open(true);
    overlay.render();
    let drawn = (0..SCAN)
        .flat_map(|y| (WIDTH - SCAN..WIDTH).map(move |x| (x, y)))
        .filter(|&(x, y)| overlay.opaque_at(x, y))
        .count();
    assert!(drawn > 1000, "the panel's frame drew only {drawn} opaque pixels");
}
