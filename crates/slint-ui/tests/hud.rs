//! The pass-through HUD (`HudUi`), headless: each element draws where the
//! client expects it and nothing draws by default.
use slint_ui::slint::ComponentHandle;
use slint_ui::{HudConfig, HudUi, Overlay};

const WIDTH: u32 = 800;
const HEIGHT: u32 = 600;

fn hud() -> Overlay<HudUi> {
    let mut overlay = Overlay::with(HudUi::new).expect("hud");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    overlay
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn nothing_drawn_by_default() {
    let mut hud = hud();
    hud.render();
    // (400, 570): bottom centre, where the old hotbar was.
    for (x, y) in [(400, 300), (5, 595), (100, 100), (122, 478), (400, 570)] {
        assert!(!hud.opaque_at(x, y), "({x}, {y}) drawn");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn crosshair_drawn_at_centre_when_visible() {
    let mut hud = hud();
    hud.ui().set_crosshair_visible(true);
    hud.render();
    assert!(hud.opaque_at(400, 300));
    hud.ui().set_crosshair_visible(false);
    hud.render();
    assert!(!hud.opaque_at(400, 300));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn band_drawn_inside_its_rect() {
    let mut hud = hud();
    let ui = hud.ui();
    ui.set_band_x(100.0);
    ui.set_band_y(100.0);
    ui.set_band_width(200.0);
    ui.set_band_height(50.0);
    ui.set_band_visible(true);
    hud.render();
    assert!(hud.opaque_at(100, 100), "border corner");
    assert!(hud.opaque_at(200, 125), "fill");
    assert!(!hud.opaque_at(99, 99));
    assert!(!hud.opaque_at(301, 151));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn minimap_drawn_bottom_left_when_visible() {
    let mut hud = hud();
    let config = hud.ui().global::<HudConfig>();
    let (size, margin) = (config.get_minimap_size(), config.get_minimap_margin());
    let (x, y) = ((margin + size / 2.0) as u32, (HEIGHT as f32 - margin - size / 2.0) as u32);
    hud.ui().set_minimap_visible(true);
    hud.render();
    assert!(hud.opaque_at(x, y));
    hud.ui().set_minimap_visible(false);
    hud.render();
    assert!(!hud.opaque_at(x, y));
}
