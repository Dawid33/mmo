//! The online screens of the frontend layer (`ui/login.slint` = screen 5,
//! `ui/worlds.slint` = screen 6, and the main menu's Play online), headless:
//! the login screen's Sign in / waiting panel (code, Open browser, Cancel),
//! the error line, joining a world by id, Play online only when a login
//! service is configured (on the browser's join-only menu too).
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::{LogicalPosition, ModelRc, VecModel};
use slint_ui::{FrontendUi, OnlineWorldRow, Overlay};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const LOGIN_SCREEN: i32 = 5;
const WORLDS_SCREEN: i32 = 6;

fn overlay(screen: i32) -> Overlay<FrontendUi> {
    let mut overlay = Overlay::with(FrontendUi::new).expect("frontend UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    overlay.ui().set_screen(screen);
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

fn snapshot(overlay: &Overlay<FrontendUi>) -> Vec<(u8, u8, u8, u8)> {
    overlay.pixels().iter().map(|p| (p.red, p.green, p.blue, p.alpha)).collect()
}

/// Idle: only Sign in (and Back); no code text. Sign in fires on click and
/// is disabled while a request is in flight.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn idle_shows_sign_in_only() {
    let mut overlay = overlay(LOGIN_SCREEN);
    let sign_in = counter();
    let s = sign_in.clone();
    overlay.ui().on_login_sign_in(move || s.set(s.get() + 1));
    let (open, cancel) = (counter(), counter());
    let (o, c) = (open.clone(), cancel.clone());
    overlay.ui().on_login_open_browser(move || o.set(o.get() + 1));
    overlay.ui().on_login_cancel(move || c.set(c.get() + 1));

    let (x, y) = (overlay.ui().get_login_sign_in_x(), overlay.ui().get_login_sign_in_y());
    click(&mut overlay, x, y);
    assert_eq!(sign_in.get(), 1);
    assert_eq!((open.get(), cancel.get()), (0, 0), "no browser or cancel buttons while idle");

    // Idle draws no code: a code set while not waiting changes nothing.
    let idle = snapshot(&overlay);
    overlay.ui().set_login_user_code("ABCD-EFGH".into());
    overlay.render();
    assert!(snapshot(&overlay) == idle, "the code is drawn only while waiting");

    overlay.ui().set_login_busy(true);
    overlay.render();
    click(&mut overlay, x, y);
    assert_eq!(sign_in.get(), 1, "Sign in is disabled while busy");
}

/// Waiting: the code is drawn, Open browser and Cancel fire, Sign in is gone.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn waiting_shows_code_and_buttons() {
    let mut overlay = overlay(LOGIN_SCREEN);
    let idle = snapshot(&overlay);
    let (sign_in, open, cancel) = (counter(), counter(), counter());
    let (s, o, c) = (sign_in.clone(), open.clone(), cancel.clone());
    overlay.ui().on_login_sign_in(move || s.set(s.get() + 1));
    overlay.ui().on_login_open_browser(move || o.set(o.get() + 1));
    overlay.ui().on_login_cancel(move || c.set(c.get() + 1));

    overlay.ui().set_login_waiting(true);
    overlay.ui().set_login_user_code("ABCD-EFGH".into());
    overlay.ui().set_login_verify_url("https://auth.test/device?user_code=ABCD-EFGH".into());
    overlay.render();
    assert!(snapshot(&overlay) != idle, "the waiting panel is drawn");
    assert_eq!(overlay.ui().get_login_user_code(), "ABCD-EFGH");

    let with_code = snapshot(&overlay);
    overlay.ui().set_login_user_code("WWWW-WWWW".into());
    overlay.render();
    assert!(snapshot(&overlay) != with_code, "the code text is drawn");

    let (ox, oy) = (overlay.ui().get_login_open_browser_x(), overlay.ui().get_login_open_browser_y());
    click(&mut overlay, ox, oy);
    let (cx, cy) = (overlay.ui().get_login_cancel_x(), overlay.ui().get_login_cancel_y());
    click(&mut overlay, cx, cy);
    assert_eq!((open.get(), cancel.get(), sign_in.get()), (1, 1, 0));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn error_is_shown() {
    let mut overlay = overlay(LOGIN_SCREEN);
    let empty = snapshot(&overlay);
    overlay.ui().set_login_error("Sign-in expired \u{2014} try again".into());
    overlay.render();
    assert!(snapshot(&overlay) != empty, "the login error is drawn");

    // The worlds screen shows the same line (a failed join).
    // Inside the frame's fixed size: the pixels change, not their count.
    let mut overlay = overlay_worlds(&[("alpha", "Alpha", true)]);
    let before = snapshot(&overlay);
    overlay.ui().set_login_error("That world is closed".into());
    overlay.render();
    assert!(snapshot(&overlay) != before, "the worlds screen's error is drawn");
}

fn overlay_worlds(rows: &[(&str, &str, bool)]) -> Overlay<FrontendUi> {
    let mut overlay = overlay(WORLDS_SCREEN);
    let rows: Vec<OnlineWorldRow> = rows
        .iter()
        .map(|(id, name, open)| OnlineWorldRow { id: (*id).into(), name: (*name).into(), open: *open })
        .collect();
    overlay.ui().set_worlds(ModelRc::new(VecModel::from(rows)));
    overlay.ui().set_login_account("dwarf@example.com".into());
    overlay.render();
    overlay
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn worlds_list_join_fires_id() {
    let mut overlay = overlay_worlds(&[("alpha", "Alpha", true), ("beta", "Beta", false), ("gamma", "Gamma", true)]);
    let joined: Rc<RefCell<Vec<String>>> = Rc::default();
    let j = joined.clone();
    overlay.ui().on_world_join(move |id| j.borrow_mut().push(id.to_string()));
    let ui = overlay.ui();
    let (x, y0, pitch) = (ui.get_worlds_row_x(), ui.get_worlds_row0_y(), ui.get_worlds_row_pitch());
    assert!(pitch > 0.0, "row pitch {pitch}");
    click(&mut overlay, x, y0 + 2.0 * pitch);
    click(&mut overlay, x, y0 + pitch);
    click(&mut overlay, x, y0);
    assert_eq!(*joined.borrow(), ["gamma", "alpha"], "a closed world cannot be joined");

    overlay.ui().set_login_busy(true);
    overlay.render();
    click(&mut overlay, x, y0);
    assert_eq!(joined.borrow().len(), 2, "no join while a request is in flight");
}

/// The "Signed in as" line follows `login-account` (the client feeds it the
/// display name, or the email when the name is empty).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn signed_in_line_follows_account() {
    let mut overlay = overlay_worlds(&[("alpha", "Alpha", true)]);
    let email = snapshot(&overlay);
    overlay.ui().set_login_account("Ada".into());
    overlay.render();
    assert!(snapshot(&overlay) != email, "the line shows the account text");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn logout_fires() {
    let mut overlay = overlay_worlds(&[("alpha", "Alpha", true)]);
    let logout = counter();
    let l = logout.clone();
    overlay.ui().on_logout(move || l.set(l.get() + 1));
    let (x, y) = (overlay.ui().get_logout_x(), overlay.ui().get_logout_y());
    click(&mut overlay, x, y);
    assert_eq!(logout.get(), 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn login_screens_go_back() {
    let mut overlay = overlay(LOGIN_SCREEN);
    let back = counter();
    let b = back.clone();
    overlay.ui().on_back(move || b.set(b.get() + 1));
    for (i, screen) in [LOGIN_SCREEN, WORLDS_SCREEN].into_iter().enumerate() {
        overlay.ui().set_screen(screen);
        overlay.render();
        let (x, y) = (overlay.ui().get_back_button_x(), overlay.ui().get_back_button_y());
        click(&mut overlay, x, y);
        assert_eq!(back.get(), i as u32 + 1, "screen {screen}");
    }
    // Also while a request is in flight: Back abandons it (a hung call must
    // not strand the player; the client ignores its answer's screen change).
    overlay.ui().set_login_busy(true);
    for (i, screen) in [LOGIN_SCREEN, WORLDS_SCREEN].into_iter().enumerate() {
        overlay.ui().set_screen(screen);
        overlay.render();
        let (x, y) = (overlay.ui().get_back_button_x(), overlay.ui().get_back_button_y());
        click(&mut overlay, x, y);
        assert_eq!(back.get(), i as u32 + 3, "Back enabled while busy on screen {screen}");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn play_online_hidden_without_login() {
    let mut overlay = overlay(0);
    let play = counter();
    let p = play.clone();
    overlay.ui().on_play_online(move || p.set(p.get() + 1));
    overlay.ui().set_online_available(true);
    overlay.render();
    let (x, y) = (overlay.ui().get_play_online_button_x(), overlay.ui().get_play_online_button_y());
    click(&mut overlay, x, y);
    assert_eq!(play.get(), 1, "shown with a login service");

    overlay.ui().set_online_available(false);
    overlay.render();
    click(&mut overlay, x, y);
    assert_eq!(play.get(), 1, "hidden without a login service");

    // The browser's join-only menu offers it too (its Back from the login
    // screen lands there), under Join server.
    let joined = counter();
    let j = joined.clone();
    overlay.ui().on_join_server(move |_| j.set(j.get() + 1));
    overlay.ui().set_join_only(true);
    overlay.ui().set_server_address("127.0.0.1".into());
    overlay.render();
    let (jx, jy) = (overlay.ui().get_play_online_button_x(), overlay.ui().get_play_online_button_y());
    click(&mut overlay, jx, jy);
    assert_eq!(play.get(), 1, "hidden on the join-only menu without a login service");
    overlay.ui().set_online_available(true);
    overlay.render();
    let (jx, jy) = (overlay.ui().get_play_online_button_x(), overlay.ui().get_play_online_button_y());
    click(&mut overlay, jx, jy);
    assert_eq!(play.get(), 2, "shown on the join-only menu with a login service");
    let (sx, sy) = (overlay.ui().get_join_button_x(), overlay.ui().get_join_button_y());
    click(&mut overlay, sx, sy);
    assert_eq!(joined.get(), 1, "Join server stays reachable");
}
