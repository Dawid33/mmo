//! The frontend: every screen outside the world, as one Slint layer
//! ([`FrontendUi`], drawn at `FRONTEND_LAYER_ORDER` over the
//! [`FrontendCamera`](crate::session::FrontendCamera)'s plain backdrop).
//!
//! The screen shown follows [`AppState`] (menu 0, generator 1, load 2,
//! starting 3 — also `InWorld` until the world map arrives — login 5, the
//! online world list 6, whose flow is `login::flow`); the layer draws
//! only while [`frontend_ui_shown`]: the frontend owns the screen and the
//! arrival map (the game UI's world map, before the first Play) is not up.
//! The generator lives in [`generator`], the world list in [`load_world`] and
//! the menu's map backdrop in [`menu_background`] (native only); the starting screen's lines come from [`starting_lines`].
//! Once embarked, the same layer draws the [`loading`] screen (screen 4) over
//! the world camera until the camera is placed and the home region is meshed.

#[cfg(not(target_arch = "wasm32"))]
pub mod generator;
pub mod loading;
#[cfg(not(target_arch = "wasm32"))]
pub mod load_world;
#[cfg(not(target_arch = "wasm32"))]
pub mod menu_background;

use std::cell::RefCell;
use std::rc::Rc;

use bevy::app::AppExit;
use bevy::prelude::*;
use slint_ui::slint::ComponentHandle;
use slint_ui::FrontendUi;

use crate::renderer::slint_overlay::{render_slint_layer, SlintLayer, SlintLayerPlugin, FRONTEND_LAYER_ORDER};
use crate::session::SessionResetExt;
use crate::login::flow::{LoginIntent, LoginSession};
use crate::renderer::world_map::WorldMapState;
use crate::session::{frontend_ui_shown, AppState, Embarked, LastSessionError, StartingLog, StartingStatus};

/// What a frontend button asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrontendAction {
    NewWorld,
    LoadWorld,
    Exit,
    Back,
    /// The browser menu's Join server, with the address in [`JoinAddress`].
    JoinServer,
    /// The main menu's Play online.
    PlayOnline,
    /// The login screen's Log in / Register / Forgot password, with the typed
    /// fields in [`LoginForm`].
    LoginSubmit,
    Register,
    ForgotPassword,
    /// The online world list's pick, by world id.
    JoinWorld(String),
    Logout,
}

/// What an action does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionEffect {
    Goto(AppState),
    Exit,
    /// Handed to the login flow (`login::flow::LoginIntent`).
    Login,
}

/// Actions queued by Slint callbacks, applied by [`apply_frontend_actions`].
struct FrontendActions(Rc<RefCell<Vec<FrontendAction>>>);

/// The address the last Join server named (the Slint callback's argument).
struct JoinAddress(Rc<RefCell<String>>);

/// The login form's email and password at the last Log in / Register /
/// Forgot password (read from the UI in the callback). Never logged; the
/// password is taken out when the intent is built ([`take_login_form`]).
struct LoginForm(Rc<RefCell<(String, String)>>);

/// The form's email and password, leaving only the email behind (it is kept
/// for convenience; the password must not outlive the request).
fn take_login_form(form: &NonSend<LoginForm>) -> (String, String) {
    take_form(&form.0)
}

fn take_form(form: &RefCell<(String, String)>) -> (String, String) {
    let mut form = form.borrow_mut();
    (form.0.clone(), std::mem::take(&mut form.1))
}

/// Whether the menu is the browser's: only Join server (the browser cannot
/// make or keep worlds, it only joins a server).
pub const JOIN_ONLY: bool = cfg!(target_arch = "wasm32");

/// Whether any saved world exists, refreshed on entering the main menu.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanLoad(pub bool);

/// `FrontendUi.screen` of the loading screen ([`loading`]).
pub const LOADING_SCREEN: i32 = 4;
/// `FrontendUi.screen` of the login screen and the online world list.
pub const LOGIN_SCREEN: i32 = 5;
pub const WORLDS_SCREEN: i32 = 6;

/// The `FrontendUi.screen` an [`AppState`] shows (`InWorld` shows the
/// starting screen: the layer is visible there only until the world map
/// arrives, and hidden once embarked).
pub fn screen_index(state: AppState) -> i32 {
    match state {
        AppState::MainMenu => 0,
        AppState::MapGenerator => 1,
        AppState::LoadWorld => 2,
        AppState::Starting | AppState::InWorld => 3,
        AppState::Login => LOGIN_SCREEN,
        AppState::Worlds => WORLDS_SCREEN,
    }
}

pub const STARTING_SERVER: &str = "Starting server\u{2026}";
pub const CONNECTING: &str = "Connecting\u{2026}";
pub const RECEIVING_MAP: &str = "Receiving world map\u{2026}";

/// The starting screen's progress lines: `status` (e.g. waiting for the
/// previous server) alone while it is set, else each step reached so far —
/// the server is starting, then (`launched`) we connect, then (`in_world`:
/// joined) the world map is on its way. None after a failed start.
pub fn starting_lines(status: &str, launched: bool, in_world: bool, failed: bool) -> Vec<&str> {
    if failed {
        return Vec::new();
    }
    if !status.is_empty() {
        return vec![status];
    }
    let mut lines = vec![STARTING_SERVER];
    if launched || in_world {
        lines.push(CONNECTING);
    }
    if in_world {
        lines.push(RECEIVING_MAP);
    }
    lines
}

/// What an action does: the state it leads to, quitting the app, or a login
/// call. (Back from a session's starting screen goes where the session ends —
/// [`apply_frontend_actions`] adjusts that.)
pub fn action_target(action: &FrontendAction) -> ActionEffect {
    match action {
        FrontendAction::NewWorld => ActionEffect::Goto(AppState::MapGenerator),
        FrontendAction::LoadWorld => ActionEffect::Goto(AppState::LoadWorld),
        FrontendAction::Back => ActionEffect::Goto(AppState::MainMenu),
        FrontendAction::JoinServer => ActionEffect::Goto(AppState::Starting),
        FrontendAction::Exit => ActionEffect::Exit,
        FrontendAction::PlayOnline
        | FrontendAction::LoginSubmit
        | FrontendAction::Register
        | FrontendAction::ForgotPassword
        | FrontendAction::JoinWorld(_)
        | FrontendAction::Logout => ActionEffect::Login,
    }
}

/// The login call an action makes (`form`: the typed email and password).
pub fn login_intent(action: &FrontendAction, form: &(String, String)) -> Option<LoginIntent> {
    let (email, password) = form.clone();
    Some(match action {
        FrontendAction::PlayOnline => LoginIntent::PlayOnline,
        FrontendAction::LoginSubmit => LoginIntent::Submit { email, password },
        FrontendAction::Register => LoginIntent::Register { email, password },
        FrontendAction::ForgotPassword => LoginIntent::Forgot { email },
        FrontendAction::JoinWorld(id) => LoginIntent::JoinWorld(id.clone()),
        FrontendAction::Logout => LoginIntent::Logout,
        _ => return None,
    })
}

pub struct FrontendPlugin;

impl Plugin for FrontendPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(
            SlintLayerPlugin::new(FrontendUi::new).order(FRONTEND_LAYER_ORDER).with_init(connect_frontend),
        )
        .init_resource::<CanLoad>()
        .init_resource::<LastSessionError>()
        .init_resource::<StartingLog>()
        .add_systems(OnEnter(AppState::MainMenu), refresh_can_load)
        .add_session_reset::<loading::LoadingCover>()
        // Play online (`login::flow`): the login outlives world sessions, so
        // none of it is session-reset.
        .add_message::<LoginIntent>()
        .add_systems(Startup, crate::login::flow::setup_login)
        .add_systems(OnEnter(AppState::Worlds), crate::login::flow::refresh_worlds)
        .add_systems(
            Update,
            (apply_frontend_actions, crate::login::flow::start_login, crate::login::flow::poll_login).chain(),
        )
        // After every Update system that embarks or re-arms the camera, so the
        // loading screen is drawn on the very frame the world camera takes over.
        .add_systems(
            PostUpdate,
            (loading::update_loading_cover, push_frontend_state).chain().before(render_slint_layer::<FrontendUi>),
        );
        #[cfg(not(target_arch = "wasm32"))]
        app.add_plugins((generator::GeneratorPlugin, load_world::LoadWorldPlugin, menu_background::MenuBackgroundPlugin));
    }
}

fn connect_frontend(ui: &FrontendUi, world: &mut World) {
    let actions = Rc::new(RefCell::new(Vec::new()));
    let push = |action: FrontendAction| {
        let queue = actions.clone();
        move || queue.borrow_mut().push(action.clone())
    };
    ui.on_new_world(push(FrontendAction::NewWorld));
    ui.on_load_world(push(FrontendAction::LoadWorld));
    ui.on_exit(push(FrontendAction::Exit));
    ui.on_back(push(FrontendAction::Back));
    let address = Rc::new(RefCell::new(String::new()));
    {
        let (queue, address) = (actions.clone(), address.clone());
        ui.on_join_server(move |typed| {
            *address.borrow_mut() = typed.trim().to_string();
            queue.borrow_mut().push(FrontendAction::JoinServer);
        });
    }
    ui.on_play_online(push(FrontendAction::PlayOnline));
    ui.on_logout(push(FrontendAction::Logout));
    {
        let queue = actions.clone();
        ui.on_world_join(move |id| queue.borrow_mut().push(FrontendAction::JoinWorld(id.to_string())));
    }
    // The form is read when its button is pressed, so the flow gets exactly
    // what was on screen.
    let form = Rc::new(RefCell::new((String::new(), String::new())));
    let push_form = |action: FrontendAction| {
        let (queue, form, weak) = (actions.clone(), form.clone(), ui.as_weak());
        move || {
            if let Some(ui) = weak.upgrade() {
                *form.borrow_mut() = (ui.get_login_email().to_string(), ui.get_login_password().to_string());
            }
            queue.borrow_mut().push(action.clone());
        }
    };
    ui.on_login_submit(push_form(FrontendAction::LoginSubmit));
    ui.on_login_register(push_form(FrontendAction::Register));
    ui.on_login_forgot(push_form(FrontendAction::ForgotPassword));
    world.insert_non_send(LoginForm(form));
    ui.set_join_only(JOIN_ONLY);
    #[cfg(target_arch = "wasm32")]
    ui.set_server_address(initial_server_address().into());
    world.insert_non_send(JoinAddress(address));
    world.insert_non_send(FrontendActions(actions));
    #[cfg(not(target_arch = "wasm32"))]
    generator::connect(ui, world);
    #[cfg(not(target_arch = "wasm32"))]
    load_world::connect(ui, world);
}

#[cfg(not(target_arch = "wasm32"))]
fn refresh_can_load(mut can_load: ResMut<CanLoad>) {
    use worldgen::{forged, world_store};
    let root = forged::from_repo_root(std::path::Path::new(world_store::WORLDS_DIR));
    can_load.set_if_neq(CanLoad(!world_store::list(&root).is_empty()));
}

#[cfg(target_arch = "wasm32")]
fn refresh_can_load() {}

/// The browser menu's prefilled server (`web_endpoint::default_server_address`).
#[cfg(target_arch = "wasm32")]
fn initial_server_address() -> String {
    let location = web_sys::window().map(|w| w.location());
    let protocol = location.as_ref().and_then(|l| l.protocol().ok()).unwrap_or_default();
    let hostname = location.as_ref().and_then(|l| l.hostname().ok()).unwrap_or_default();
    crate::web_endpoint::default_server_address(crate::web_endpoint::BUILT_FOR_SERVER, &protocol, &hostname)
}

#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn apply_frontend_actions(
    actions: Option<NonSend<FrontendActions>>,
    join_address: Option<NonSend<JoinAddress>>,
    (login_form, login, mut login_intents): (
        Option<NonSend<LoginForm>>,
        Option<Res<LoginSession>>,
        MessageWriter<LoginIntent>,
    ),
    mut commands: Commands,
    state: Res<State<AppState>>,
    embarked: Res<Embarked>,
    world_map: Option<Res<WorldMapState>>,
    mut error: ResMut<LastSessionError>,
    mut log: ResMut<StartingLog>,
    mut next: ResMut<NextState<AppState>>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(actions) = actions else { return };
    let queued = std::mem::take(&mut *actions.0.borrow_mut());
    let map_arrived = world_map.is_some_and(|m| m.map.is_some());
    if !frontend_ui_shown(*state.get(), embarked.0, map_arrived) {
        return;
    }
    for action in queued {
        // Back from the starting screen: its error has been read there, the
        // main menu need not repeat it.
        if action == FrontendAction::Back && matches!(state.get(), AppState::Starting | AppState::InWorld) {
            error.0 = None;
            log.0.clear();
        }
        if action == FrontendAction::JoinServer {
            let address = join_address.as_ref().map(|a| a.0.borrow().clone()).unwrap_or_default();
            if address.is_empty() || *state.get() != AppState::MainMenu {
                continue;
            }
            commands.insert_resource(crate::session::PendingServer(address));
        }
        match action_target(&action) {
            // Back from a session's starting screen: where the session ends
            // (the online world list after a ticket join).
            ActionEffect::Goto(AppState::MainMenu)
                if matches!(state.get(), AppState::Starting | AppState::InWorld) =>
            {
                next.set(LoginSession::home(login.as_deref()));
            }
            ActionEffect::Goto(target) => next.set(target),
            ActionEffect::Exit => {
                exit.write(AppExit::Success);
            }
            ActionEffect::Login => {
                let form = login_form.as_ref().map(take_login_form).unwrap_or_default();
                if let Some(intent) = login_intent(&action, &form) {
                    login_intents.write(intent);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn push_frontend_state(
    layer: Option<NonSend<SlintLayer<FrontendUi>>>,
    state: Res<State<AppState>>,
    embarked: Res<Embarked>,
    can_load: Res<CanLoad>,
    error: Res<LastSessionError>,
    starting: Option<Res<StartingStatus>>,
    log: Res<StartingLog>,
    world_map: Option<Res<WorldMapState>>,
    #[cfg(not(target_arch = "wasm32"))] host: Option<Res<crate::local_host::LocalHost>>,
    (cover, time): (Res<loading::LoadingCover>, Res<Time<Real>>),
    mut shown_lines: Local<Vec<String>>,
    (login, mut shown_worlds): (Option<ResMut<LoginSession>>, Local<Vec<(String, String, bool)>>),
) {
    let Some(layer) = layer else { return };
    let ui = layer.overlay.ui();
    let map_arrived = world_map.is_some_and(|m| m.map.is_some());
    let frontend = frontend_ui_shown(*state.get(), embarked.0, map_arrived);
    let loading = !frontend && cover.visible();
    let shown = frontend || loading;
    if ui.get_shown() != shown {
        ui.set_shown(shown);
    }
    let screen = if loading { LOADING_SCREEN } else { screen_index(*state.get()) };
    if loading {
        if ui.get_load_screen_progress() != cover.progress {
            ui.set_load_screen_progress(cover.progress);
        }
        if ui.get_load_screen_step() != cover.step {
            ui.set_load_screen_step(cover.step.into());
        }
        let fade = cover.fade(time.elapsed_secs());
        if ui.get_load_screen_fade() != fade {
            ui.set_load_screen_fade(fade);
        }
    }
    if ui.get_screen() != screen {
        ui.set_screen(screen);
    }
    if ui.get_can_load() != can_load.0 {
        ui.set_can_load(can_load.0);
    }
    let text = error.0.as_deref().unwrap_or("");
    if ui.get_menu_error() != text {
        ui.set_menu_error(text.into());
    }

    // The starting screen: progress, or why the start failed.
    let starting_screen = screen == 3;
    let failed = starting_screen && error.0.is_some();
    #[cfg(not(target_arch = "wasm32"))]
    let launched = host.is_some();
    #[cfg(target_arch = "wasm32")]
    let launched = true;
    let status = starting.as_ref().map_or("", |s| s.0.as_str());
    let in_world = *state.get() == AppState::InWorld;
    let lines: Vec<String> =
        starting_lines(status, launched, in_world, failed).into_iter().map(String::from).collect();
    if *shown_lines != lines {
        let model: Vec<slint_ui::slint::SharedString> = lines.iter().map(|l| l.as_str().into()).collect();
        ui.set_start_lines(slint_ui::slint::ModelRc::new(slint_ui::slint::VecModel::from(model)));
        *shown_lines = lines;
    }
    let start_error = if starting_screen { text } else { "" };
    if ui.get_start_error() != start_error {
        ui.set_start_error(start_error.into());
    }
    let start_log = if failed { log.0.as_str() } else { "" };
    if ui.get_start_log() != start_log {
        ui.set_start_log(start_log.into());
    }
    push_login_state(ui, login, &mut shown_worlds);
}

/// The login screens' properties from the [`LoginSession`] (none: no login
/// service, so no Play online).
fn push_login_state(ui: &FrontendUi, login: Option<ResMut<LoginSession>>, shown_worlds: &mut Vec<(String, String, bool)>) {
    use slint_ui::slint::{ModelRc, VecModel};
    if ui.get_online_available() != login.is_some() {
        ui.set_online_available(login.is_some());
    }
    let Some(mut login) = login else { return };
    if login.clear_password {
        login.clear_password = false;
        ui.set_login_password("".into());
    }
    if ui.get_login_error() != login.error.as_str() {
        ui.set_login_error(login.error.as_str().into());
    }
    if ui.get_login_notice() != login.notice.as_str() {
        ui.set_login_notice(login.notice.as_str().into());
    }
    if ui.get_login_busy() != login.busy() {
        ui.set_login_busy(login.busy());
    }
    let account = login.account.as_deref().unwrap_or("");
    if ui.get_login_account() != account {
        ui.set_login_account(account.into());
    }
    let worlds: Vec<(String, String, bool)> = login
        .worlds
        .iter()
        .map(|w| (w.id.clone(), w.name.clone(), w.status == login_api::WorldStatus::Open))
        .collect();
    if *shown_worlds != worlds {
        let rows: Vec<slint_ui::OnlineWorldRow> = worlds
            .iter()
            .map(|(id, name, open)| slint_ui::OnlineWorldRow { id: id.into(), name: name.into(), open: *open })
            .collect();
        ui.set_worlds(ModelRc::new(VecModel::from(rows)));
        *shown_worlds = worlds;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screens_follow_the_state() {
        assert_eq!(screen_index(AppState::MainMenu), 0);
        assert_eq!(screen_index(AppState::MapGenerator), 1);
        assert_eq!(screen_index(AppState::LoadWorld), 2);
        assert_eq!(screen_index(AppState::Starting), 3);
        assert_eq!(screen_index(AppState::InWorld), 3, "receiving the world map");
    }

    #[test]
    fn the_form_keeps_the_email_but_not_the_password() {
        let form = RefCell::new(("a@b.c".to_string(), "hunter2hunter2".to_string()));
        assert_eq!(take_form(&form), ("a@b.c".to_string(), "hunter2hunter2".to_string()));
        assert_eq!(*form.borrow(), ("a@b.c".to_string(), String::new()));
    }

    #[test]
    fn screen_index_login_and_worlds() {
        assert_eq!(screen_index(AppState::Login), 5);
        assert_eq!(screen_index(AppState::Worlds), 6);
    }

    #[test]
    fn login_actions_go_to_the_login_flow() {
        let form = ("a@b.c".to_string(), "hunter2hunter2".to_string());
        for action in [
            FrontendAction::PlayOnline,
            FrontendAction::LoginSubmit,
            FrontendAction::Register,
            FrontendAction::ForgotPassword,
            FrontendAction::JoinWorld("alpha".into()),
            FrontendAction::Logout,
        ] {
            assert_eq!(action_target(&action), ActionEffect::Login, "{action:?}");
            assert!(login_intent(&action, &form).is_some(), "{action:?}");
        }
        assert!(matches!(
            login_intent(&FrontendAction::LoginSubmit, &form),
            Some(LoginIntent::Submit { email, password }) if email == "a@b.c" && password == "hunter2hunter2"
        ));
        assert!(matches!(
            login_intent(&FrontendAction::JoinWorld("alpha".into()), &form),
            Some(LoginIntent::JoinWorld(id)) if id == "alpha"
        ));
        assert!(login_intent(&FrontendAction::Back, &form).is_none());
    }

    #[test]
    fn starting_lines_follow_the_steps() {
        assert_eq!(starting_lines("", false, false, false), [STARTING_SERVER]);
        assert_eq!(starting_lines("", true, false, false), [STARTING_SERVER, CONNECTING]);
        assert_eq!(starting_lines("", true, true, false), [STARTING_SERVER, CONNECTING, RECEIVING_MAP]);
        assert_eq!(starting_lines("Waiting…", false, false, false), ["Waiting…"]);
        assert!(starting_lines("", true, false, true).is_empty(), "a failed start shows its error");
    }

    #[test]
    fn actions_lead_to_states() {
        assert_eq!(action_target(&FrontendAction::NewWorld), ActionEffect::Goto(AppState::MapGenerator));
        assert_eq!(action_target(&FrontendAction::LoadWorld), ActionEffect::Goto(AppState::LoadWorld));
        assert_eq!(action_target(&FrontendAction::Back), ActionEffect::Goto(AppState::MainMenu));
        assert_eq!(action_target(&FrontendAction::JoinServer), ActionEffect::Goto(AppState::Starting));
        assert_eq!(action_target(&FrontendAction::Exit), ActionEffect::Exit);
    }
}
