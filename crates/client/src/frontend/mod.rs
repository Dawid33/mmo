//! The frontend: every screen outside the world, as one Slint layer
//! ([`FrontendUi`], drawn at `FRONTEND_LAYER_ORDER` over the
//! [`FrontendCamera`](crate::session::FrontendCamera)'s plain backdrop).
//!
//! The screen shown follows [`AppState`] (menu 0, generator 1, load 2,
//! starting 3 — also `InWorld` until the world map arrives — login 5, the
//! online world list 6, whose flow is `login::flow`); the layer draws
//! only while [`frontend_ui_shown`]: the frontend owns the screen and the
//! arrival map (the game UI's world map, while not in play) is not up.
//! The generator lives in [`generator`], the world list in [`load_world`] and
//! the menu's map backdrop in [`menu_background`] (native only); the starting screen's lines come from [`starting_lines`].
//! Once in play, the same layer draws the [`loading`] screen (screen 4) over
//! the world camera until the camera is placed and the home region is meshed.

#[cfg(not(target_arch = "wasm32"))]
pub mod generator;
#[cfg(not(target_arch = "wasm32"))]
pub mod load_world;
pub mod loading;
#[cfg(not(target_arch = "wasm32"))]
pub mod menu_background;

use std::cell::RefCell;
use std::rc::Rc;

use bevy::app::AppExit;
use bevy::prelude::*;
use slint_ui::FrontendUi;

use crate::login::flow::{LoginIntent, LoginSession};
use crate::renderer::slint_overlay::{
    FRONTEND_LAYER_ORDER, SlintLayer, SlintLayerPlugin, render_slint_layer,
};
use crate::renderer::world_map::WorldMapState;
use crate::session::SessionResetExt;
use crate::session::{
    AppState, InPlay, LastSessionError, StartingLog, StartingStatus, frontend_ui_shown,
};

/// What a frontend button asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrontendAction {
    SinglePlayer,
    Multiplayer,
    Settings,
    LogIn,
    RefreshWorlds,
    PickRegion(String),
    SelectWorld(String),
    Search(String),
    NewWorld,
    LoadWorld,
    Exit,
    Back,
    /// The login screen's Sign in (the identity provider's device sign-in),
    /// Open browser (its page again) and Cancel.
    SignIn,
    OpenBrowser,
    CancelSignIn,
    /// The online world list's pick, by world id.
    JoinWorld(String),
    Logout,
}

/// What an action does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionEffect {
    Goto(AppState),
    Exit,
    Browser,
    /// Handed to the login flow (`login::flow::LoginIntent`).
    Login,
}

/// Actions queued by Slint callbacks, applied by [`apply_frontend_actions`].
struct FrontendActions(Rc<RefCell<Vec<FrontendAction>>>);

pub const SINGLE_PLAYER_AVAILABLE: bool = cfg!(not(target_arch = "wasm32"));
pub const CAN_EXIT: bool = cfg!(not(target_arch = "wasm32"));

/// Whether any saved world exists, refreshed on entering the main menu.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanLoad(pub bool);

/// `FrontendUi.screen` of the loading screen ([`loading`]).
pub const LOADING_SCREEN: i32 = 4;
/// `FrontendUi.screen` of the login screen and the online world list.
pub const LOGIN_SCREEN: i32 = 5;
pub const WORLDS_SCREEN: i32 = 6;
pub const SETTINGS_SCREEN: i32 = 7;

/// The server browser's durable selection. It outlives world sessions, like
/// the login, so opening another world does not reset the player's filters.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct Browser {
    pub region: String,
    pub selected: Option<String>,
    pub search: String,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            region: login_api::DEFAULT_REGION.to_string(),
            selected: None,
            search: String::new(),
        }
    }
}

pub fn regions(worlds: &[login_api::WorldEntry]) -> Vec<String> {
    use std::collections::BTreeSet;
    let rest: BTreeSet<&str> = worlds
        .iter()
        .map(|world| world.region.as_str())
        .filter(|region| *region != login_api::DEFAULT_REGION)
        .collect();
    let mut result = vec![login_api::DEFAULT_REGION.to_string()];
    result.extend(rest.into_iter().map(str::to_string));
    result
}

pub fn shown_worlds(
    worlds: &[login_api::WorldEntry],
    region: &str,
    search: &str,
) -> Vec<(String, String)> {
    let search = search.to_lowercase();
    worlds
        .iter()
        .filter(|world| {
            world.status == login_api::WorldStatus::Open
                && world.region == region
                && world.name.to_lowercase().contains(&search)
        })
        .map(|world| (world.id.clone(), world.name.clone()))
        .collect()
}

pub fn selected_index(shown: &[(String, String)], selected: Option<&str>) -> i32 {
    selected
        .and_then(|id| shown.iter().position(|world| world.0 == id))
        .map_or(-1, |index| index as i32)
}

/// The `FrontendUi.screen` an [`AppState`] shows (`InWorld` shows the
/// starting screen: the layer is visible there only until the world map
/// arrives, and hidden once in play).
pub fn screen_index(state: AppState) -> i32 {
    match state {
        AppState::MainMenu | AppState::SinglePlayer => 0,
        AppState::MapGenerator => 1,
        AppState::LoadWorld => 2,
        AppState::Starting | AppState::InWorld => 3,
        AppState::Login => LOGIN_SCREEN,
        AppState::Worlds => WORLDS_SCREEN,
        AppState::Settings => SETTINGS_SCREEN,
    }
}

pub fn menu_page(state: AppState) -> i32 {
    i32::from(state == AppState::SinglePlayer)
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
        FrontendAction::SinglePlayer => ActionEffect::Goto(AppState::SinglePlayer),
        FrontendAction::NewWorld => ActionEffect::Goto(AppState::MapGenerator),
        FrontendAction::LoadWorld => ActionEffect::Goto(AppState::LoadWorld),
        FrontendAction::Back => ActionEffect::Goto(AppState::MainMenu),
        FrontendAction::Exit => ActionEffect::Exit,
        FrontendAction::Multiplayer
        | FrontendAction::Settings
        | FrontendAction::LogIn
        | FrontendAction::RefreshWorlds
        | FrontendAction::SignIn
        | FrontendAction::OpenBrowser
        | FrontendAction::CancelSignIn
        | FrontendAction::JoinWorld(_)
        | FrontendAction::Logout => ActionEffect::Login,
        FrontendAction::PickRegion(_)
        | FrontendAction::SelectWorld(_)
        | FrontendAction::Search(_) => ActionEffect::Browser,
    }
}

pub fn allowed_on(action: &FrontendAction, single_player: bool, can_exit: bool) -> bool {
    match action {
        FrontendAction::SinglePlayer | FrontendAction::NewWorld | FrontendAction::LoadWorld => {
            single_player
        }
        FrontendAction::Exit => can_exit,
        _ => true,
    }
}

pub fn back_target(state: AppState, return_to: AppState) -> AppState {
    match state {
        AppState::MapGenerator | AppState::LoadWorld => AppState::SinglePlayer,
        AppState::SinglePlayer | AppState::Worlds | AppState::Settings => AppState::MainMenu,
        AppState::Login => return_to,
        AppState::MainMenu | AppState::Starting | AppState::InWorld => AppState::MainMenu,
    }
}

/// The login call an action makes.
pub fn login_intent(action: &FrontendAction) -> Option<LoginIntent> {
    Some(match action {
        FrontendAction::Multiplayer => LoginIntent::Multiplayer,
        FrontendAction::Settings => LoginIntent::Settings,
        FrontendAction::LogIn => LoginIntent::LogIn,
        FrontendAction::RefreshWorlds => LoginIntent::RefreshWorlds,
        FrontendAction::SignIn => LoginIntent::SignIn,
        FrontendAction::OpenBrowser => LoginIntent::OpenBrowser,
        FrontendAction::CancelSignIn => LoginIntent::CancelSignIn,
        FrontendAction::JoinWorld(id) => LoginIntent::JoinWorld(id.clone()),
        FrontendAction::Logout => LoginIntent::Logout,
        _ => return None,
    })
}

pub struct FrontendPlugin;

impl Plugin for FrontendPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(
            SlintLayerPlugin::new(FrontendUi::new)
                .order(FRONTEND_LAYER_ORDER)
                .with_init(connect_frontend),
        )
        .init_resource::<Browser>()
        .init_resource::<CanLoad>()
        .init_resource::<LastSessionError>()
        .init_resource::<StartingLog>()
        .add_systems(OnEnter(AppState::MainMenu), refresh_can_load)
        .add_session_reset::<loading::LoadingCover>()
        // Multiplayer (`login::flow`): the login outlives world sessions, so
        // none of it is session-reset.
        .add_message::<LoginIntent>()
        .add_systems(Startup, crate::login::flow::setup_login)
        .add_systems(
            OnEnter(AppState::Worlds),
            crate::login::flow::refresh_worlds,
        )
        .add_systems(
            Update,
            (
                apply_frontend_actions,
                crate::login::flow::start_login,
                crate::login::flow::poll_login,
            )
                .chain(),
        )
        // After every Update system that starts play or re-arms the camera, so the
        // loading screen is drawn on the very frame the world camera takes over.
        .add_systems(
            PostUpdate,
            (loading::update_loading_cover, push_frontend_state)
                .chain()
                .before(render_slint_layer::<FrontendUi>),
        );
        #[cfg(not(target_arch = "wasm32"))]
        app.add_plugins((
            generator::GeneratorPlugin,
            load_world::LoadWorldPlugin,
            menu_background::MenuBackgroundPlugin,
        ));
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
    ui.on_single_player(push(FrontendAction::SinglePlayer));
    ui.on_multiplayer(push(FrontendAction::Multiplayer));
    ui.on_settings(push(FrontendAction::Settings));
    ui.on_settings_log_in(push(FrontendAction::LogIn));
    ui.on_settings_log_out(push(FrontendAction::Logout));
    ui.on_world_refresh(push(FrontendAction::RefreshWorlds));
    {
        let queue = actions.clone();
        ui.on_world_region_chosen(move |index| {
            queue
                .borrow_mut()
                .push(FrontendAction::PickRegion(index.to_string()))
        });
    }
    {
        let queue = actions.clone();
        ui.on_world_join(move |id| {
            queue
                .borrow_mut()
                .push(FrontendAction::JoinWorld(id.to_string()))
        });
    }
    {
        let queue = actions.clone();
        ui.on_world_select(move |index| {
            queue
                .borrow_mut()
                .push(FrontendAction::SelectWorld(index.to_string()))
        });
    }
    {
        let queue = actions.clone();
        ui.on_world_search_edited(move |text| {
            queue
                .borrow_mut()
                .push(FrontendAction::Search(text.to_string()))
        });
    }
    ui.on_login_sign_in(push(FrontendAction::SignIn));
    ui.on_login_cancel(push(FrontendAction::CancelSignIn));
    // Native: the flow opens the system browser. Browser build: the flow opens
    // nothing, the page does, from this callback itself: it runs while the
    // click's pointer event is dispatched into Slint (renderer/slint_overlay.rs,
    // the frame right after the DOM event), not a further queued action later,
    // so the browser still counts it as the player's click (transient user
    // activation) and lets the tab open.
    #[cfg(not(target_arch = "wasm32"))]
    ui.on_login_open_browser(push(FrontendAction::OpenBrowser));
    #[cfg(target_arch = "wasm32")]
    {
        use slint_ui::slint::ComponentHandle;
        let weak = ui.as_weak();
        ui.on_login_open_browser(move || {
            let Some(ui) = weak.upgrade() else { return };
            // The URL was checked (http / https) when the answer was parsed.
            let url = ui.get_login_verify_url();
            if url.is_empty() {
                return;
            }
            if let Some(window) = web_sys::window() {
                if window
                    .open_with_url_and_target(url.as_str(), "_blank")
                    .is_err()
                {
                    bevy::log::warn!("[login] the browser refused to open the sign-in tab");
                }
            }
        });
    }
    // The Settings window over the main menu (graphics_settings.rs).
    crate::graphics_settings::connect_settings!(ui, world);
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

#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn apply_frontend_actions(
    actions: Option<NonSend<FrontendActions>>,
    (login, mut login_intents): (Option<Res<LoginSession>>, MessageWriter<LoginIntent>),
    mut browser: ResMut<Browser>,
    state: Res<State<AppState>>,
    in_play: Res<InPlay>,
    world_map: Option<Res<WorldMapState>>,
    mut error: ResMut<LastSessionError>,
    mut log: ResMut<StartingLog>,
    mut next: ResMut<NextState<AppState>>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(actions) = actions else { return };
    let queued = std::mem::take(&mut *actions.0.borrow_mut());
    let map_arrived = world_map.is_some_and(|m| m.map.is_some());
    if !frontend_ui_shown(*state.get(), in_play.0, map_arrived) {
        return;
    }
    for action in queued {
        if !allowed_on(&action, SINGLE_PLAYER_AVAILABLE, CAN_EXIT) {
            continue;
        }
        // Back from the starting screen: its error has been read there, the
        // main menu need not repeat it.
        if action == FrontendAction::Back
            && matches!(state.get(), AppState::Starting | AppState::InWorld)
        {
            error.0 = None;
            log.0.clear();
        }
        // Back off the login screens abandons the login calls in flight (a
        // hung one must leave the screen usable again).
        if action == FrontendAction::Back
            && matches!(
                state.get(),
                AppState::Login | AppState::Worlds | AppState::Settings
            )
        {
            login_intents.write(LoginIntent::Back);
        }
        match &action {
            FrontendAction::PickRegion(index) => {
                if let (Some(login), Ok(index)) = (login.as_deref(), index.parse::<usize>()) {
                    if let Some(region) = regions(&login.worlds).get(index) {
                        browser.region.clone_from(region);
                        browser.selected = None;
                    }
                }
                continue;
            }
            FrontendAction::SelectWorld(index) => {
                if let (Some(login), Ok(index)) = (login.as_deref(), index.parse::<usize>()) {
                    browser.selected =
                        shown_worlds(&login.worlds, &browser.region, &browser.search)
                            .get(index)
                            .map(|world| world.0.clone());
                }
                continue;
            }
            FrontendAction::Search(text) => {
                browser.search.clone_from(text);
                continue;
            }
            _ => {}
        }
        match action_target(&action) {
            // Back from a session's starting screen: where the session ends
            // (the online world list after a ticket join).
            ActionEffect::Goto(AppState::MainMenu)
                if matches!(state.get(), AppState::Starting | AppState::InWorld) =>
            {
                next.set(LoginSession::home(login.as_deref()));
            }
            ActionEffect::Goto(AppState::MainMenu) if action == FrontendAction::Back => {
                let return_to = login
                    .as_deref()
                    .map_or(AppState::Worlds, LoginSession::return_to);
                next.set(back_target(*state.get(), return_to));
            }
            ActionEffect::Goto(target) => next.set(target),
            ActionEffect::Exit => {
                exit.write(AppExit::Success);
            }
            ActionEffect::Login => {
                if let Some(intent) = login_intent(&action) {
                    login_intents.write(intent);
                }
            }
            ActionEffect::Browser => {}
        }
    }
}

#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn push_frontend_state(
    layer: Option<NonSend<SlintLayer<FrontendUi>>>,
    state: Res<State<AppState>>,
    in_play: Res<InPlay>,
    can_load: Res<CanLoad>,
    error: Res<LastSessionError>,
    starting: Option<Res<StartingStatus>>,
    log: Res<StartingLog>,
    world_map: Option<Res<WorldMapState>>,
    #[cfg(not(target_arch = "wasm32"))] host: Option<Res<crate::local_host::LocalHost>>,
    (cover, time): (Res<loading::LoadingCover>, Res<Time<Real>>),
    mut shown_lines: Local<Vec<String>>,
    mut browser: ResMut<Browser>,
    (login, mut cached_worlds, mut cached_regions): (
        Option<Res<LoginSession>>,
        Local<Vec<(String, String)>>,
        Local<Vec<String>>,
    ),
) {
    let Some(layer) = layer else { return };
    let ui = layer.overlay.ui();
    let map_arrived = world_map.is_some_and(|m| m.map.is_some());
    let frontend = frontend_ui_shown(*state.get(), in_play.0, map_arrived);
    let loading = !frontend && cover.visible();
    let shown = frontend || loading;
    if ui.get_shown() != shown {
        ui.set_shown(shown);
    }
    let screen = if loading {
        LOADING_SCREEN
    } else {
        screen_index(*state.get())
    };
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
    if ui.get_menu_page() != menu_page(*state.get()) {
        ui.set_menu_page(menu_page(*state.get()));
    }
    if ui.get_single_player_available() != SINGLE_PLAYER_AVAILABLE {
        ui.set_single_player_available(SINGLE_PLAYER_AVAILABLE);
    }
    if ui.get_can_exit() != CAN_EXIT {
        ui.set_can_exit(CAN_EXIT);
    }
    if ui.get_can_load() != can_load.0 {
        ui.set_can_load(can_load.0);
    }
    if screen != SETTINGS_SCREEN && ui.get_settings_open() {
        ui.set_settings_open(false);
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
    let lines: Vec<String> = starting_lines(status, launched, in_world, failed)
        .into_iter()
        .map(String::from)
        .collect();
    if *shown_lines != lines {
        let model: Vec<slint_ui::slint::SharedString> =
            lines.iter().map(|l| l.as_str().into()).collect();
        ui.set_start_lines(slint_ui::slint::ModelRc::new(
            slint_ui::slint::VecModel::from(model),
        ));
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
    push_login_state(
        ui,
        login,
        &mut browser,
        &mut cached_worlds,
        &mut cached_regions,
    );
}

/// The login screens' properties from the [`LoginSession`] (none: no login
/// service is unexpected now that native has an official URL fallback).
fn push_login_state(
    ui: &FrontendUi,
    login: Option<Res<LoginSession>>,
    browser: &mut Browser,
    cached_worlds: &mut Vec<(String, String)>,
    cached_regions: &mut Vec<String>,
) {
    use slint_ui::slint::{ModelRc, VecModel};
    let Some(login) = login else {
        return;
    };
    if ui.get_login_error() != login.error.as_str() {
        ui.set_login_error(login.error.as_str().into());
    }
    if ui.get_login_waiting() != login.waiting_for_sign_in() {
        ui.set_login_waiting(login.waiting_for_sign_in());
    }
    if ui.get_login_user_code() != login.user_code() {
        ui.set_login_user_code(login.user_code().into());
    }
    if ui.get_login_verify_url() != login.verification_uri_complete() {
        ui.set_login_verify_url(login.verification_uri_complete().into());
    }
    if ui.get_login_busy() != login.busy() {
        ui.set_login_busy(login.busy());
    }
    if ui.get_login_status() != login.status() {
        ui.set_login_status(login.status().into());
    }
    // The worlds screen's "Signed in as": the display name, else the email.
    let account = if login.account_name.is_empty() {
        login.account.as_deref().unwrap_or("")
    } else {
        login.account_name.as_str()
    };
    if ui.get_login_account() != account {
        ui.set_login_account(account.into());
    }
    let all_regions = regions(&login.worlds);
    if !all_regions.contains(&browser.region) {
        browser.region = login_api::DEFAULT_REGION.to_string();
        browser.selected = None;
    }
    let world_region = all_regions
        .iter()
        .position(|region| region == &browser.region)
        .unwrap_or(0) as i32;
    if ui.get_world_region() != world_region {
        ui.set_world_region(world_region);
    }
    if ui.get_world_region_name() != browser.region.as_str() {
        ui.set_world_region_name(browser.region.as_str().into());
    }
    let typed_search = ui.get_world_search().to_string();
    if typed_search != browser.search {
        browser.search = typed_search;
    }
    let worlds = shown_worlds(&login.worlds, &browser.region, &browser.search);
    let selected = selected_index(&worlds, browser.selected.as_deref());
    if selected < 0 {
        browser.selected = None;
    }
    if ui.get_world_selected() != selected {
        ui.set_world_selected(selected);
    }
    if *cached_regions != all_regions {
        let model: Vec<slint_ui::slint::SharedString> = all_regions
            .iter()
            .map(|region| region.as_str().into())
            .collect();
        ui.set_world_regions(ModelRc::new(VecModel::from(model)));
        *cached_regions = all_regions;
    }
    if *cached_worlds != worlds {
        let rows: Vec<slint_ui::OnlineWorldRow> = worlds
            .iter()
            .map(|(id, name)| slint_ui::OnlineWorldRow {
                id: id.into(),
                name: name.into(),
            })
            .collect();
        let names: Vec<slint_ui::slint::SharedString> = worlds
            .iter()
            .map(|(_, name)| name.as_str().into())
            .collect();
        ui.set_worlds(ModelRc::new(VecModel::from(rows)));
        ui.set_world_names(ModelRc::new(VecModel::from(names)));
        *cached_worlds = worlds;
    }
}
