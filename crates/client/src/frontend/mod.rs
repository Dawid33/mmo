//! The frontend: every screen outside the world, as one Slint layer
//! ([`FrontendUi`], drawn at `FRONTEND_LAYER_ORDER` over the
//! [`FrontendCamera`](crate::session::FrontendCamera)'s plain backdrop).
//!
//! The screen shown follows [`AppState`] (menu 0, generator 1, load 2,
//! starting 3); the layer draws only while [`frontend_owns`] says the frontend
//! owns the screen. Generator, load and starting are placeholders (title +
//! Back) until Tasks 12-14.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::app::AppExit;
use bevy::prelude::*;
use slint_ui::FrontendUi;

use crate::renderer::slint_overlay::{SlintLayer, SlintLayerPlugin, FRONTEND_LAYER_ORDER};
use crate::session::{frontend_owns, AppState, Embarked, LastSessionError};

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

/// The `FrontendUi.screen` an [`AppState`] shows (`InWorld` shows the menu screen: it is only visible while not embarked, and the layer is hidden otherwise).
pub fn screen_index(state: AppState) -> i32 {
    match state {
        AppState::MainMenu | AppState::InWorld => 0,
        AppState::MapGenerator => 1,
        AppState::LoadWorld => 2,
        AppState::Starting => 3,
    }
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
        .add_systems(OnEnter(AppState::MainMenu), refresh_can_load)
        .add_systems(Update, (apply_frontend_actions, push_frontend_state).chain());
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
}

#[cfg(not(target_arch = "wasm32"))]
fn refresh_can_load(mut can_load: ResMut<CanLoad>) {
    use worldgen::{forged, world_store};
    let root = forged::from_repo_root(std::path::Path::new(world_store::WORLDS_DIR));
    can_load.set_if_neq(CanLoad(!world_store::list(&root).is_empty()));
}

#[cfg(target_arch = "wasm32")]
fn refresh_can_load() {}

fn apply_frontend_actions(
    actions: Option<NonSend<FrontendActions>>,
    state: Res<State<AppState>>,
    embarked: Res<Embarked>,
    mut next: ResMut<NextState<AppState>>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(actions) = actions else { return };
    let queued = std::mem::take(&mut *actions.0.borrow_mut());
    if !frontend_owns(*state.get(), embarked.0) {
        return;
    }
    for action in queued {
        match action_target(action) {
            Some(target) => next.set(target),
            None => {
                exit.write(AppExit::Success);
            }
        }
    }
}

fn push_frontend_state(
    layer: Option<NonSend<SlintLayer<FrontendUi>>>,
    state: Res<State<AppState>>,
    embarked: Res<Embarked>,
    can_load: Res<CanLoad>,
    error: Res<LastSessionError>,
) {
    let Some(layer) = layer else { return };
    let ui = layer.overlay.ui();
    let shown = frontend_owns(*state.get(), embarked.0);
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
    }

    #[test]
    fn actions_lead_to_states() {
        assert_eq!(action_target(FrontendAction::NewWorld), Some(AppState::MapGenerator));
        assert_eq!(action_target(FrontendAction::LoadWorld), Some(AppState::LoadWorld));
        assert_eq!(action_target(FrontendAction::Back), Some(AppState::MainMenu));
        assert_eq!(action_target(FrontendAction::Exit), None);
    }
}
