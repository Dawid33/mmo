//! The fortress tool wheel, headless: open at a point, it draws there and
//! reports the wedge under the pointer by angle; the old bottom tool bar is
//! gone.
use slint_ui::slint::platform::WindowEvent;
use slint_ui::slint::{LogicalPosition, ModelRc, VecModel};
use slint_ui::{GameUi, Overlay, RadialItem};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
/// Where the wheel opens.
const AT: (f32, f32) = (400.0, 300.0);

fn overlay() -> Overlay<GameUi> {
    let mut overlay = Overlay::new().expect("game UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    let items: Vec<RadialItem> = ["Dig", "Deconstruct", "Build", "Erase", "Show above"]
        .into_iter()
        .map(|t| RadialItem { text: t.into(), enabled: true, ..Default::default() })
        .collect();
    overlay.ui().set_wheel_items(ModelRc::new(VecModel::from(items)));
    overlay.ui().set_tools_available(true);
    overlay.ui().set_wheel_x(AT.0);
    overlay.ui().set_wheel_y(AT.1);
    overlay.render();
    overlay
}

/// `r` px from the wheel's centre, `deg` clockwise from up.
fn at(deg: f32, r: f32) -> LogicalPosition {
    let a = deg.to_radians();
    LogicalPosition::new(AT.0 + r * a.sin(), AT.1 - r * a.cos())
}

fn hover(overlay: &mut Overlay<GameUi>, position: LogicalPosition) -> i32 {
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.render();
    overlay.ui().get_wheel_hovered()
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn open_wheel_picks_by_angle_around_where_it_opened() {
    let mut overlay = overlay();
    overlay.ui().set_wheel_open(true);
    overlay.render();
    let top = at(0.0, 80.0);
    assert!(overlay.opaque_at(top.x as u32, top.y as u32), "a wedge is drawn above the opening point");
    assert_eq!(hover(&mut overlay, top), 0);
    // Five items: 72° apart, item 2 centred at 144°.
    assert_eq!(hover(&mut overlay, at(144.0, 80.0)), 2);
    // Past the ring still picks; the middle picks nothing.
    assert_eq!(hover(&mut overlay, at(288.0, 300.0)), 4);
    assert_eq!(hover(&mut overlay, at(30.0, 10.0)), -1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn closed_wheel_draws_and_picks_nothing() {
    let mut overlay = overlay();
    let top = at(0.0, 80.0);
    assert!(!overlay.opaque_at(top.x as u32, top.y as u32));
    assert_eq!(hover(&mut overlay, top), -1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn there_is_no_bottom_tool_bar() {
    let overlay = overlay();
    // The old bar sat bottom centre, 12 px off the bottom edge.
    let (x, y) = (WIDTH / 2, HEIGHT - 30);
    assert!(!overlay.opaque_at(x, y), "something is drawn where the tool bar was");
}
