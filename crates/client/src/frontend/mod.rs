//! The frontend: every screen outside the world, as one Slint layer
//! ([`FrontendUi`], drawn at `FRONTEND_LAYER_ORDER` over the
//! [`FrontendCamera`](crate::session::FrontendCamera)'s plain backdrop).
//!
//! The screen shown follows [`AppState`] (menu 0, generator 1, load 2,
//! starting 3 — also `InWorld` until the world map arrives); the layer draws
//! only while [`frontend_ui_shown`]: the frontend owns the screen and the
//! arrival map (the game UI's world map, before the first Play) is not up.
//! The generator lives in [`generator`] and the world list in [`load_world`]
//! (native only); the starting screen's lines come from [`starting_lines`].

#[cfg(not(target_arch = "wasm32"))]
pub mod generator;
#[cfg(not(target_arch = "wasm32"))]
pub mod load_world;

use std::cell::RefCell;
use std::rc::Rc;

use bevy::app::AppExit;
use bevy::prelude::*;
use slint_ui::FrontendUi;

use crate::renderer::slint_overlay::{SlintLayer, SlintLayerPlugin, FRONTEND_LAYER_ORDER};
use crate::renderer::world_map::WorldMapState;
use crate::session::{frontend_ui_shown, AppState, Embarked, LastSessionError, StartingLog, StartingStatus};

/// What a frontend button asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrontendAction {
    NewWorld,
    LoadWorld,
    Exit,
    Back,
}

/// Actions queued by Slint callbacks, applied by [`apply_frontend_actions`].
struct FrontendActions(Rc<RefCell<Vec<FrontendAction>>>);

/// Whether any saved world exists, refreshed on entering the main menu.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanLoad(pub bool);

/// The `FrontendUi.screen` an [`AppState`] shows (`InWorld` shows the
/// starting screen: the layer is visible there only until the world map
/// arrives, and hidden once embarked).
pub fn screen_index(state: AppState) -> i32 {
    match state {
        AppState::MainMenu => 0,
        AppState::MapGenerator => 1,
        AppState::LoadWorld => 2,
        AppState::Starting | AppState::InWorld => 3,
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

/// The state an action leads to (`None`: no state change, `Exit` quits the app).
pub fn action_target(action: FrontendAction) -> Option<AppState> {
    match action {
        FrontendAction::NewWorld => Some(AppState::MapGenerator),
        FrontendAction::LoadWorld => Some(AppState::LoadWorld),
        FrontendAction::Back => Some(AppState::MainMenu),
        FrontendAction::Exit => None,
    }
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
        .add_systems(Update, (apply_frontend_actions, push_frontend_state).chain());
        #[cfg(not(target_arch = "wasm32"))]
        app.add_plugins((generator::GeneratorPlugin, load_world::LoadWorldPlugin));
    }
}

fn connect_frontend(ui: &FrontendUi, world: &mut World) {
    let actions = Rc::new(RefCell::new(Vec::new()));
    let push = |action: FrontendAction| {
        let queue = actions.clone();
        move || queue.borrow_mut().push(action)
    };
    ui.on_new_world(push(FrontendAction::NewWorld));
    ui.on_load_world(push(FrontendAction::LoadWorld));
    ui.on_exit(push(FrontendAction::Exit));
    ui.on_back(push(FrontendAction::Back));
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
        match action_target(action) {
            Some(target) => next.set(target),
            None => {
                exit.write(AppExit::Success);
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
    mut shown_lines: Local<Vec<String>>,
) {
    let Some(layer) = layer else { return };
    let ui = layer.overlay.ui();
    let map_arrived = world_map.is_some_and(|m| m.map.is_some());
    let shown = frontend_ui_shown(*state.get(), embarked.0, map_arrived);
    if ui.get_shown() != shown {
        ui.set_shown(shown);
    }
    let screen = screen_index(*state.get());
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
    fn starting_lines_follow_the_steps() {
        assert_eq!(starting_lines("", false, false, false), [STARTING_SERVER]);
        assert_eq!(starting_lines("", true, false, false), [STARTING_SERVER, CONNECTING]);
        assert_eq!(starting_lines("", true, true, false), [STARTING_SERVER, CONNECTING, RECEIVING_MAP]);
        assert_eq!(starting_lines("Waiting…", false, false, false), ["Waiting…"]);
        assert!(starting_lines("", true, false, true).is_empty(), "a failed start shows its error");
    }

    #[test]
    fn actions_lead_to_states() {
        assert_eq!(action_target(FrontendAction::NewWorld), Some(AppState::MapGenerator));
        assert_eq!(action_target(FrontendAction::LoadWorld), Some(AppState::LoadWorld));
        assert_eq!(action_target(FrontendAction::Back), Some(AppState::MainMenu));
        assert_eq!(action_target(FrontendAction::Exit), None);
    }
}
