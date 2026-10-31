//! The client as a state machine: main menu → (map generator | load world) →
//! starting → in world → back to the menu, with each world **session** torn
//! down in-process (no relaunch).
//!
//! The pieces every per-world plugin builds on:
//! - [`AppState`], and [`world_systems_enabled`] — the one run condition for
//!   systems that only make sense while a world session exists.
//! - [`SessionScoped`] — a marker on the root of every entity tree a session
//!   spawns; [`end_session`] despawns them (recursively).
//! - [`SessionResetExt`] — the reset registry: per-session resources go back to
//!   their [`FromWorld`] value, per-session message queues are cleared.
//! - [`SessionEnded`] — written when a session ends (`reason: None` = the
//!   player chose to leave).
//! - [`FrontendCamera`] — the camera the menu screens draw on.
//!
//! # Who owns the screen
//! Slint layers composite only onto an active `Camera3d` marked
//! [`UiCamera`](crate::renderer::UiCamera) (`renderer/slint_composite.rs`). While
//! [`frontend_owns`] is true — every [`AppState`] except `InWorld`, and `InWorld`
//! before the player has [`Embarked`] — the [`FrontendCamera`] carries
//! `UiCamera` and **no other system may claim `UiCamera`** or keep a world
//! camera active over it (`bridge::enforce_single_active_camera` is taught to
//! honour this in Task 8). Outside that window the world's cameras own
//! `UiCamera` exactly as before, and the frontend camera does not exist.
//!
//! The frontend camera is spawned when the frontend takes the screen and
//! despawned when it hands it back, rather than kept around inactive: several
//! world systems read "the" camera as the first `Camera3d` they find
//! (`meshing/queue.rs` ranks chunks by `camera.iter().next()`, the render
//! benchmark and autopilot iterate every `Camera3d`), and a parked camera at the
//! origin would skew them.

use bevy::camera::visibility::RenderLayers;
use bevy::ecs::message::Messages;
use bevy::prelude::*;

use crate::renderer::UiCamera;

/// The client's top-level screen.
#[derive(States, Default, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AppState {
    #[default]
    MainMenu,
    MapGenerator,
    LoadWorld,
    /// A world session is starting: server launching / connecting / joining.
    Starting,
    InWorld,
}

/// Root of an entity tree that belongs to the current world session.
/// [`end_session`] despawns it with all its descendants.
#[derive(Component, Clone, Copy, Default, Debug)]
pub struct SessionScoped;

/// A world session ended. `reason: None` = the player chose to leave;
/// otherwise why it was lost (server died, connect failed, …).
#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub struct SessionEnded {
    pub reason: Option<String>,
}

/// The dev bypass: start straight in the world, as before the menu existed.
/// Computed once in `main` ([`skip_menu_from_env`]).
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkipMenu(pub bool);

/// The player has arrived in the world (Task 14 sets it once the arrival map
/// is dismissed). Until then the frontend owns the screen even in `InWorld`.
/// Reset with the session.
#[derive(Resource, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Embarked(pub bool);

/// Why the last session ended without the player's say (server died, connect
/// failed, …), shown on the main menu. Task 9 fills it.
#[derive(Resource, Default, Clone, Debug, PartialEq, Eq)]
pub struct LastSessionError(pub Option<String>);

/// The world the next session plays, set by the map generator's Play (and
/// the Load screen) before `AppState::Starting`; Task 9 starts its server.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct PendingWorld {
    /// `worlds/<slug>/`.
    pub entry_dir: std::path::PathBuf,
    /// Its `terrain.world`.
    pub terrain: std::path::PathBuf,
    pub name: String,
}

/// The menu screens' camera; see the module docs for when it exists.
#[derive(Component, Clone, Copy, Default, Debug)]
pub struct FrontendCamera;

/// Render layer the frontend camera sees — no world entity is on it, so the
/// camera draws only its clear colour (and the Slint composite on top).
pub const FRONTEND_RENDER_LAYER: usize = 31;

/// Above every world camera, should one still be active while the frontend
/// takes over.
pub const FRONTEND_CAMERA_ORDER: isize = 100;

/// The frontend's backdrop behind the Slint screens.
pub const FRONTEND_CLEAR_COLOR: Color = Color::srgb(0.035, 0.035, 0.045);

/// Bumped by [`end_session`]: how many sessions have ended. [`SessionLocal`]
/// compares against it to drop a system's per-session memory.
#[derive(Resource, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct SessionGeneration(pub u64);

/// A system `Local` that belongs to the world session: it is back at
/// `T::default()` the first time the system runs after [`end_session`].
/// Read it through [`SessionLocal::get`] (typically
/// `let state = state.get();` at the top of the system). Without
/// [`SessionGeneration`] (headless tests without the session plugin) it is
/// an ordinary `Local`.
#[derive(bevy::ecs::system::SystemParam)]
pub struct SessionLocal<'w, 's, T: Default + Send + 'static> {
    generation: Option<Res<'w, SessionGeneration>>,
    state: Local<'s, (u64, T)>,
}

impl<T: Default + Send + 'static> SessionLocal<'_, '_, T> {
    /// The current session's value (reset first if a session ended since
    /// the last call).
    pub fn get(&mut self) -> &mut T {
        let now = self.generation.as_ref().map_or(0, |g| g.0);
        if self.state.0 != now {
            *self.state = (now, T::default());
        }
        &mut self.state.1
    }
}

/// Registered per-session resets, run by [`end_session`] in registration order.
#[derive(Resource, Default)]
pub struct SessionResets(pub Vec<fn(&mut World)>);

/// Message types whose queues [`end_session`] clears (after the resets).
#[derive(Resource, Default)]
pub struct SessionMessageClears(pub Vec<fn(&mut World)>);

/// The reset registry on [`App`].
pub trait SessionResetExt {
    /// `R` returns to its [`FromWorld`] value when a session ends (and is
    /// initialised now if absent).
    fn add_session_reset<R: Resource + FromWorld>(&mut self) -> &mut Self;
    /// Run `f` when a session ends (for state that is not one resource).
    fn add_session_reset_with(&mut self, f: fn(&mut World)) -> &mut Self;
    /// Clear `Messages<T>` when a session ends, so nothing written by the old
    /// session is read by the next one.
    fn add_session_message_clear<T: Message>(&mut self) -> &mut Self;
}

fn reset_resource<R: Resource + FromWorld>(world: &mut World) {
    let fresh = R::from_world(world);
    world.insert_resource(fresh);
}

fn clear_messages<T: Message>(world: &mut World) {
    if let Some(mut messages) = world.get_resource_mut::<Messages<T>>() {
        messages.clear();
    }
}

impl SessionResetExt for App {
    fn add_session_reset<R: Resource + FromWorld>(&mut self) -> &mut Self {
        self.init_resource::<R>();
        self.add_session_reset_with(reset_resource::<R>)
    }

    fn add_session_reset_with(&mut self, f: fn(&mut World)) -> &mut Self {
        self.world_mut().get_resource_or_init::<SessionResets>().0.push(f);
        self
    }

    fn add_session_message_clear<T: Message>(&mut self) -> &mut Self {
        self.world_mut().get_resource_or_init::<SessionMessageClears>().0.push(clear_messages::<T>);
        self
    }
}

/// Tear the current world session down: despawn every [`SessionScoped`] tree,
/// run every registered reset in registration order, then clear every
/// registered message queue. Usable as an exclusive system.
pub fn end_session(world: &mut World) {
    let roots: Vec<Entity> =
        world.query_filtered::<Entity, With<SessionScoped>>().iter(world).collect();
    for root in roots {
        // A scoped entity may sit inside another scoped tree, already gone.
        if let Ok(entity) = world.get_entity_mut(root) {
            entity.despawn(); // recursive: takes the children along
        }
    }
    let resets = world.get_resource::<SessionResets>().map(|r| r.0.clone()).unwrap_or_default();
    for reset in resets {
        reset(world);
    }
    let clears =
        world.get_resource::<SessionMessageClears>().map(|c| c.0.clone()).unwrap_or_default();
    for clear in clears {
        clear(world);
    }
    world.get_resource_or_init::<SessionGeneration>().0 += 1;
}

/// The run condition for every per-world system: a session is starting or
/// running. Without the session state (headless tests that build a plugin
/// without [`SessionPlugin`]) the world is all there is, so it holds.
pub fn world_systems_enabled() -> impl SystemCondition<()> {
    IntoSystem::into_system(|state: Option<Res<State<AppState>>>| {
        state.is_none_or(|s| matches!(s.get(), AppState::InWorld | AppState::Starting))
    })
}

/// [`frontend_owns`] over optional resources: without the session state
/// (headless tests, examples) the frontend never owns the screen.
pub fn frontend_has_screen(state: Option<&State<AppState>>, embarked: Option<&Embarked>) -> bool {
    state.is_some_and(|s| frontend_owns(*s.get(), embarked.is_some_and(|e| e.0)))
}

/// Whether the frontend (menu screens, starting screen, arrival map) owns the screen.
pub fn frontend_owns(state: AppState, embarked: bool) -> bool {
    state != AppState::InWorld || !embarked
}

/// [`frontend_owns`] as a run condition.
pub fn frontend_owns_screen()
-> impl FnMut(Option<Res<State<AppState>>>, Option<Res<Embarked>>) -> bool + Clone {
    |state: Option<Res<State<AppState>>>, embarked: Option<Res<Embarked>>| {
        let state = state.map_or(AppState::default(), |s| *s.get());
        frontend_owns(state, embarked.is_some_and(|e| e.0))
    }
}

/// The dev-bypass rule, over an env lookup (`|k| std::env::var(k).ok()`).
pub fn skip_menu_from_env(get: impl Fn(&str) -> Option<String>) -> bool {
    // "Set" the way the features themselves read it: non-empty, and for the
    // two run modes also not "0" (`render_bench::enabled`, `Autopilot::from_env`).
    let non_empty = |key: &str| get(key).is_some_and(|v| !v.is_empty());
    let switched_on = |key: &str| get(key).is_some_and(|v| !v.is_empty() && v != "0");
    let bypass = get("VOXEL_SKIP_MENU").as_deref() == Some("1")
        || non_empty("VOXEL_WORLD_FILE")
        || switched_on("RENDER_BENCH")
        || switched_on("VOXEL_AUTOPILOT");
    if bypass {
        return true;
    }
    // TODO(Task 15): menu becomes the default — return false here and drop
    // VOXEL_MENU. Until the frontend exists the menu is opt-in.
    get("VOXEL_MENU").as_deref() != Some("1")
}

/// [`skip_menu_from_env`] on the process environment; always true on wasm,
/// which keeps today's flow (no menu).
pub fn skip_menu() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        true
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        skip_menu_from_env(|key| std::env::var(key).ok())
    }
}

/// The state the app starts in.
pub fn initial_state(skip_menu: bool) -> AppState {
    // Until the session-start flow exists (Task 9 moves Starting → InWorld),
    // the bypass goes straight to InWorld with the player already embarked,
    // so today's game runs unchanged.
    if skip_menu { AppState::InWorld } else { AppState::MainMenu }
}

/// States, the reset registry, [`SessionEnded`] and the frontend camera.
/// Needs `StatesPlugin` (in `DefaultPlugins` with Bevy's `bevy_state` feature).
pub struct SessionPlugin {
    pub skip_menu: bool,
}

impl Plugin for SessionPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SkipMenu(self.skip_menu))
            .insert_state(initial_state(self.skip_menu))
            .init_resource::<SessionResets>()
            .init_resource::<SessionMessageClears>()
            .init_resource::<SessionGeneration>()
            .insert_resource(Embarked(self.skip_menu))
            .add_session_reset::<Embarked>()
            .init_resource::<LastSessionError>()
            .add_message::<SessionEnded>()
            .add_systems(Update, sync_frontend_camera);
    }
}

/// Spawn the [`FrontendCamera`] when the frontend takes the screen, despawn it
/// when the world takes it back.
fn sync_frontend_camera(
    mut commands: Commands,
    state: Res<State<AppState>>,
    embarked: Res<Embarked>,
    cameras: Query<Entity, With<FrontendCamera>>,
) {
    if frontend_owns(*state.get(), embarked.0) {
        if cameras.is_empty() {
            commands.spawn(frontend_camera_bundle());
        }
    } else {
        for camera in &cameras {
            commands.entity(camera).despawn();
        }
    }
}

fn frontend_camera_bundle() -> impl Bundle {
    (
        FrontendCamera,
        Camera3d::default(),
        Camera {
            is_active: true,
            order: FRONTEND_CAMERA_ORDER,
            clear_color: ClearColorConfig::Custom(FRONTEND_CLEAR_COLOR),
            ..default()
        },
        RenderLayers::layer(FRONTEND_RENDER_LAYER),
        UiCamera,
        // Present-but-empty, so the world's `attach_skybox` / `attach_fog`
        // (which fill in every `Camera3d` lacking them) leave this one alone.
        bevy::core_pipeline::Skybox::default(),
        bevy::pbr::DistanceFog { color: Color::NONE, ..default() },
        Name::new("frontend camera"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::state::app::StatesPlugin;
    use std::collections::HashMap;

    fn app(skip_menu: bool) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, SessionPlugin { skip_menu }));
        app
    }

    #[derive(Resource, Default, PartialEq, Debug)]
    struct Counter(u32);

    #[derive(Resource, Default, PartialEq, Debug)]
    struct Untouched(u32);

    #[derive(Message)]
    struct Ping;

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn end_session_despawns_scoped_trees() {
        let mut app = app(false);
        let world = app.world_mut();
        let child = world.spawn_empty().id();
        let grandchild = world.spawn_empty().id();
        world.entity_mut(child).add_child(grandchild);
        let root = world.spawn(SessionScoped).id();
        world.entity_mut(root).add_child(child);
        let keep = world.spawn(Name::new("unscoped")).id();

        end_session(world);

        for gone in [root, child, grandchild] {
            assert!(world.get_entity(gone).is_err(), "{gone:?} survived end_session");
        }
        assert!(world.get_entity(keep).is_ok());
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn end_session_resets_registered_resources() {
        let mut app = app(false);
        app.add_session_reset::<Counter>().insert_resource(Untouched(7));
        app.add_session_reset_with(|w| w.resource_mut::<Counter>().0 += 100);
        app.world_mut().resource_mut::<Counter>().0 = 5;
        app.world_mut().insert_resource(Embarked(true));

        end_session(app.world_mut());

        // Reset first (5 → 0), then the custom reset (0 → 100): registration order.
        assert_eq!(app.world().resource::<Counter>().0, 100);
        assert_eq!(*app.world().resource::<Untouched>(), Untouched(7));
        assert_eq!(*app.world().resource::<Embarked>(), Embarked(false));
    }

    #[derive(Resource, Default)]
    struct Seen(Vec<u32>);

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn session_local_starts_over_after_end_session() {
        let mut app = app(true);
        app.init_resource::<Seen>().add_systems(Update, |mut n: SessionLocal<u32>, mut seen: ResMut<Seen>| {
            let n = n.get();
            *n += 1;
            seen.0.push(*n);
        });
        app.update();
        app.update();
        end_session(app.world_mut());
        app.update();
        assert_eq!(app.world().resource::<Seen>().0, vec![1, 2, 1], "back at its default after end_session");
        assert_eq!(*app.world().resource::<SessionGeneration>(), SessionGeneration(1));
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn end_session_clears_registered_messages() {
        let mut app = app(false);
        app.add_message::<Ping>().add_session_message_clear::<Ping>();
        app.world_mut().write_message(Ping);
        app.world_mut().write_message(SessionEnded { reason: None });
        assert!(!app.world().resource::<Messages<Ping>>().is_empty());

        end_session(app.world_mut());

        assert!(app.world().resource::<Messages<Ping>>().is_empty());
        // Unregistered queues are untouched: the menu reads why the session ended.
        assert!(!app.world().resource::<Messages<SessionEnded>>().is_empty());
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn world_systems_do_not_run_in_menu() {
        let mut app = app(false);
        app.init_resource::<Counter>()
            .add_systems(Update, (|mut c: ResMut<Counter>| c.0 += 1).run_if(world_systems_enabled()));
        app.update();
        app.update();
        assert_eq!(app.world().resource::<Counter>().0, 0, "ran in MainMenu");

        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Starting);
        app.update();
        assert!(app.world().resource::<Counter>().0 >= 1, "did not run in Starting");

        let before = app.world().resource::<Counter>().0;
        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::InWorld);
        app.update();
        assert!(app.world().resource::<Counter>().0 > before, "did not run in InWorld");
    }

    fn frontend_cameras(app: &mut App) -> Vec<(bool, bool)> {
        let mut q = app
            .world_mut()
            .query_filtered::<(&Camera, Has<UiCamera>), With<FrontendCamera>>();
        q.iter(app.world()).map(|(c, ui)| (c.is_active, ui)).collect()
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn frontend_camera_is_ui_camera_outside_world() {
        let mut app = app(false);
        app.update();
        assert_eq!(frontend_cameras(&mut app), vec![(true, true)], "MainMenu");

        for state in [AppState::MapGenerator, AppState::LoadWorld, AppState::Starting, AppState::InWorld] {
            app.world_mut().resource_mut::<NextState<AppState>>().set(state);
            app.update();
            app.update();
            // InWorld but not yet embarked: still the frontend's screen.
            assert_eq!(frontend_cameras(&mut app), vec![(true, true)], "{state:?}");
        }

        app.world_mut().resource_mut::<Embarked>().0 = true;
        app.update();
        assert!(frontend_cameras(&mut app).is_empty(), "embarked: the world owns the screen");

        // Back to the menu: the session ends, the frontend takes the screen again.
        end_session(app.world_mut());
        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::MainMenu);
        app.update();
        app.update();
        assert_eq!(frontend_cameras(&mut app), vec![(true, true)], "back in MainMenu");
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn skip_menu_starts_embarked_in_world() {
        let mut app = app(true);
        app.update();
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::InWorld);
        assert_eq!(*app.world().resource::<Embarked>(), Embarked(true));
        assert!(frontend_cameras(&mut app).is_empty());
    }

    #[test]
    fn frontend_owns_every_state_but_embarked_in_world() {
        for state in [AppState::MainMenu, AppState::MapGenerator, AppState::LoadWorld, AppState::Starting] {
            assert!(frontend_owns(state, false), "{state:?}");
            assert!(frontend_owns(state, true), "{state:?} embarked");
        }
        assert!(frontend_owns(AppState::InWorld, false));
        assert!(!frontend_owns(AppState::InWorld, true));
    }

    #[test]
    fn skip_menu_env_table() {
        let cases: &[(&[(&str, &str)], bool)] = &[
            // TODO(Task 15): with nothing set the menu becomes the default (false).
            (&[], true),
            (&[("VOXEL_MENU", "1")], false),
            (&[("VOXEL_MENU", "0")], true),
            (&[("VOXEL_MENU", "1"), ("VOXEL_WORLD_FILE", "")], false),
            (&[("VOXEL_MENU", "1"), ("VOXEL_WORLD_FILE", "worlds/a.world")], true),
            (&[("VOXEL_MENU", "1"), ("VOXEL_SKIP_MENU", "1")], true),
            (&[("VOXEL_MENU", "1"), ("VOXEL_SKIP_MENU", "0")], false),
            (&[("VOXEL_MENU", "1"), ("RENDER_BENCH", "1")], true),
            (&[("VOXEL_MENU", "1"), ("RENDER_BENCH", "0")], false),
            (&[("VOXEL_MENU", "1"), ("VOXEL_AUTOPILOT", "shuttle")], true),
            (&[("VOXEL_MENU", "1"), ("VOXEL_AUTOPILOT", "")], false),
        ];
        for (vars, want) in cases {
            let env: HashMap<&str, &str> = vars.iter().copied().collect();
            let got = skip_menu_from_env(|k| env.get(k).map(|v| v.to_string()));
            assert_eq!(got, *want, "{vars:?}");
        }
    }
}
