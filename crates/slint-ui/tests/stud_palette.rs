//! The stud build palette (`ui/stud-build.slint`) and the Build tool's
//! material list in the context box (`ui/menu-column.slint`), headless: the
//! palette draws only while open, sits in the bottom-right corner inside the
//! window with every baked colour's swatch, and each control reports its pick.
use std::cell::RefCell;
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::{Color, LogicalPosition, ModelRc, SharedString, VecModel};
use slint_ui::{GameUi, Overlay, StudSwatch};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
/// As many parts, naturals and colours as the game feeds it (24 / 24 / 322).
const PARTS: usize = 24;
const NATURALS: usize = 24;
const COLORS: usize = 322;

fn click(overlay: &Overlay<GameUi>, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    overlay.dispatch(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
}

fn strings(prefix: &str, n: usize) -> ModelRc<SharedString> {
    let rows: Vec<SharedString> = (0..n).map(|i| format!("{prefix} {i}").into()).collect();
    ModelRc::new(VecModel::from(rows))
}

fn overlay() -> Overlay<GameUi> {
    let mut overlay = Overlay::new().expect("game UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    let ui = overlay.ui();
    ui.set_stud_parts(strings("Brick", PARTS));
    ui.set_stud_naturals(strings("Rock", NATURALS));
    let swatches: Vec<StudSwatch> = (0..COLORS)
        .map(|i| StudSwatch { color: Color::from_rgb_u8((i % 256) as u8, 90, 160), name: format!("Colour {i}").into() })
        .collect();
    ui.set_stud_colors(ModelRc::new(VecModel::from(swatches)));
    ui.set_stud_part(0);
    ui.set_stud_rotation("Rotation 0°".into());
    ui.set_stud_selection("Brick 0 · 0° · Colour 0".into());
    overlay.render();
    overlay
}

/// What the palette's callbacks reported, in order.
fn record(overlay: &Overlay<GameUi>) -> Rc<RefCell<Vec<String>>> {
    let log = Rc::new(RefCell::new(Vec::new()));
    let ui = overlay.ui();
    let l = log.clone();
    ui.on_stud_pick_part(move |i| l.borrow_mut().push(format!("part {i}")));
    let l = log.clone();
    ui.on_stud_pick_natural(move |i| l.borrow_mut().push(format!("natural {i}")));
    let l = log.clone();
    ui.on_stud_pick_color(move |i| l.borrow_mut().push(format!("color {i}")));
    let l = log.clone();
    ui.on_stud_rotate(move || l.borrow_mut().push("rotate".into()));
    let l = log.clone();
    ui.on_stud_close(move || l.borrow_mut().push("close".into()));
    let l = log.clone();
    ui.on_pick_build_material(move |i| l.borrow_mut().push(format!("build {i}")));
    log
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn the_palette_draws_only_while_the_build_mode_is_on() {
    let mut overlay = overlay();
    assert!(!overlay.opaque_at(WIDTH - 4, HEIGHT - 4), "closed: nothing in the corner");
    let log = record(&overlay);
    // A click where the palette would be reaches nothing of it.
    click(&overlay, overlay.ui().get_stud_rotate_x(), overlay.ui().get_stud_rotate_y());
    assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    overlay.ui().set_stud_build_open(true);
    overlay.render();
    assert!(overlay.opaque_at(WIDTH - 4, HEIGHT - 4), "open: flush with the bottom-right corner");
    assert!(!overlay.opaque_at(40, 300), "and nowhere near the left edge");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn every_palette_control_reports_its_pick() {
    let mut overlay = overlay();
    overlay.ui().set_stud_build_open(true);
    overlay.render();
    let log = record(&overlay);
    let ui = overlay.ui();

    click(&overlay, ui.get_stud_rotate_x(), ui.get_stud_rotate_y());
    let row = ui.get_stud_parts_row_height();
    assert!(row > 8.0, "rows have a height: {row}");
    click(&overlay, ui.get_stud_parts_x(), ui.get_stud_parts_first_y() + 2.5 * row);
    click(&overlay, ui.get_stud_parts_x(), ui.get_stud_naturals_first_y() + 1.5 * row);

    // The swatch grid: every colour inside the window, and a swatch picks its index.
    let (cols, pitch) = (ui.get_stud_swatch_columns(), ui.get_stud_swatch_pitch());
    assert!(cols >= 16, "a compact grid: {cols} columns");
    let at = |i: i32| (ui.get_stud_swatches_x() + (i % cols) as f32 * pitch + 3.0, ui.get_stud_swatches_y() + (i / cols) as f32 * pitch + 3.0);
    let last = at(COLORS as i32 - 1);
    assert!(last.0 < WIDTH as f32 && last.1 < HEIGHT as f32, "the last swatch is on screen: {last:?}");
    assert!(ui.get_stud_parts_first_y() > 0.0, "the palette's top is on screen at {HEIGHT} px");
    for i in [0, 30, COLORS as i32 - 1] {
        let (x, y) = at(i);
        click(&overlay, x, y);
    }
    click(&overlay, ui.get_stud_close_x(), ui.get_stud_close_y());
    assert_eq!(
        *log.borrow(),
        ["rotate", "part 2", "natural 1", "color 0", "color 30", &format!("color {}", COLORS - 1), "close"]
    );
}

/// The picked rows show selected, and the selection line is drawn.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn the_picked_swatch_is_outlined() {
    let mut overlay = overlay();
    overlay.ui().set_stud_build_open(true);
    overlay.ui().set_stud_color(5);
    overlay.render();
    let ui = overlay.ui();
    let (cols, pitch) = (ui.get_stud_swatch_columns(), ui.get_stud_swatch_pitch());
    let corner = |i: i32| ((ui.get_stud_swatches_x() + (i % cols) as f32 * pitch) as u32, (ui.get_stud_swatches_y() + (i / cols) as f32 * pitch) as u32);
    let (px, py) = corner(5);
    let (qx, qy) = corner(6);
    let rgb = |x: u32, y: u32| {
        let p = overlay.pixels()[(y * WIDTH + x) as usize];
        (p.red, p.green, p.blue)
    };
    assert_eq!(rgb(px, py), (255, 255, 255), "the picked swatch has a white border");
    assert_ne!(rgb(qx, qy), (255, 255, 255), "the next has none");
}

/// The Build tool's material list in the context box: there only when the
/// client gives it rows, and a row click reports its index.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn the_build_tools_material_list_reports_a_pick() {
    let mut overlay = overlay();
    let log = record(&overlay);
    overlay.ui().set_tool_name("Build".into());
    overlay.ui().set_level_text("Level 58 · surface".into());
    overlay.render();
    let scan = |overlay: &Overlay<GameUi>| {
        for y in (30..400).step_by(5) {
            click(overlay, WIDTH as f32 - 120.0, y as f32);
        }
    };
    scan(&overlay);
    assert!(log.borrow().is_empty(), "no list without rows: {:?}", log.borrow());
    overlay.ui().set_build_materials(strings("Rock", 12));
    overlay.ui().set_build_material(1);
    overlay.render();
    scan(&overlay);
    let picks = log.borrow();
    assert!(picks.iter().all(|p| p.starts_with("build ")), "{picks:?}");
    for i in 0..4 {
        assert!(picks.contains(&format!("build {i}")), "row {i} is clickable: {picks:?}");
    }
}
