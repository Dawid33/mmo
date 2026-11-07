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
//! camera active over it (`bridge::enforce_single_active_camera` stands
//! aside while it does). Outside that window the world's cameras own
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

/// The player has embarked: pressed Play on the arrival map (the world map,
/// modal until then) and arrived. Until then the frontend owns the screen even
/// in `InWorld` — the spawn region loading behind the map is never shown. False
/// after a menu start, true from the first frame with [`SkipMenu`]. Reset with
/// the session.
#[derive(Resource, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Embarked(pub bool);

/// Why the last session ended without the player's say (server died, connect
/// failed, …), shown on the main menu. Set by the session flow when a start
/// fails or a session ends; cleared when the next start begins.
#[derive(Resource, Default, Clone, Debug, PartialEq, Eq)]
pub struct LastSessionError(pub Option<String>);

/// The starting screen's extra status line ("" = nothing to say), e.g. while
/// the previous server is still stopping. Reset with the session.
#[derive(Resource, Default, Clone, Debug, PartialEq, Eq)]
pub struct StartingStatus(pub String);

/// The server log's tail under a failed start's [`LastSessionError`] on the
/// starting screen ("" = none). Cleared when a start begins and on Back.
#[derive(Resource, Default, Clone, Debug, PartialEq, Eq)]
pub struct StartingLog(pub String);

/// The world the next session plays, set by the map generator's Play (and
/// the Load screen) before `AppState::Starting`; the session flow starts its
/// server and removes it.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct PendingWorld {
    /// `worlds/<slug>/`.
    pub entry_dir: std::path::PathBuf,
    /// Its `terrain.world`.
    pub terrain: std::path::PathBuf,
    pub name: String,
}

/// The server the next session joins, set by the browser menu's Join server
/// before `AppState::Starting`; the wasm session flow connects to it and
/// removes it. The address as typed (`web_endpoint::web_server_for`).
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct PendingServer(pub String);

/// Native: the port the next [`PendingWorld`]'s server binds instead of a free
/// one (`client debug embarked --port N`, so other clients can join it).
/// Kept for the process's life.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostPort(pub u16);

/// Native: join this already running server instead of starting a
/// [`PendingWorld`]'s own (`client debug embarked --join ADDR`). Set before
/// `AppState::Starting`; the session flow connects to it and removes it.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinServer(pub std::net::SocketAddr);

/// A start whose world is still being prepared (the `debug embarked` launch
/// forging its world): `Starting` waits for the [`PendingWorld`] to arrive
/// instead of failing for want of one. Whoever inserts it sets the starting
/// screen's [`StartingStatus`] and replaces it with a `PendingWorld` (or ends
/// the start with a [`SessionEnded`] reason).
#[derive(Resource, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct PreparingWorld;

/// The menu screens' camera; see the module docs for when it exists.
/// Extracted to the render world, where the custom terrain passes (near
/// voxel_pull forward / prepass / shadows / x-ray, the far-voxel pass) skip
/// its view ([`view_draws_world`]): they bin into every view and would
/// otherwise draw (or at least render) the world behind the frontend —
/// `RenderLayers` only filter Bevy's own mesh queues.
#[derive(Component, Clone, Copy, Default, Debug, bevy::render::extract_component::ExtractComponent)]
pub struct FrontendCamera;

/// Whether a render view draws the world: every view but the
/// [`FrontendCamera`]'s (`is_frontend_view` = it has the extracted marker).
pub fn view_draws_world(is_frontend_view: bool) -> bool {
    !is_frontend_view
}

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

/// Whether the arrival map is up: in the world, not embarked, and the world
/// map has arrived. Then the game UI (the map only) is drawn and takes input in
/// the frontend's place, while the frontend camera still owns the screen;
/// before the map arrives the frontend's starting screen shows.
pub fn arrival_map_shown(state: AppState, embarked: bool, map_arrived: bool) -> bool {
    state == AppState::InWorld && !embarked && map_arrived
}

/// Whether the frontend's own screens (`FrontendUi`) are drawn: the frontend
/// owns the screen and the arrival map is not up. Exactly one interactive
/// layer has the screen at any time: this, or the game UI.
pub fn frontend_ui_shown(state: AppState, embarked: bool, map_arrived: bool) -> bool {
    frontend_owns(state, embarked) && !arrival_map_shown(state, embarked, map_arrived)
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
    bypass
}

/// [`skip_menu_from_env`] on the process environment; never on wasm, which
/// opens on the main menu (Join server) like the native client.
pub fn skip_menu() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        skip_menu_from_env(|key| std::env::var(key).ok())
    }
}

/// The state the app starts in.
pub fn initial_state(skip_menu: bool) -> AppState {
    // The dev bypass (`VOXEL_SKIP_MENU`, a set `VOXEL_WORLD_FILE`, the probes)
    // goes straight to InWorld with the player already embarked (`Embarked`
    // starts as `skip_menu`), connecting to an already running server.
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
            .add_session_reset::<StartingStatus>()
            .init_resource::<StartingLog>()
            .add_message::<SessionEnded>()
            .add_systems(Update, sync_frontend_camera);
        // Only with a renderer (added after `DefaultPlugins`): the extraction's
        // sync hooks need the render world's resources, which headless apps lack.
        if app.get_sub_app(bevy::render::RenderApp).is_some() {
            app.add_plugins(bevy::render::extract_component::ExtractComponentPlugin::<FrontendCamera>::default());
        }
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

/// One session's channels: the app's ends go into the [`ClientUpdates`](crate::renderer::ClientUpdates) /
/// [`GameEvents`](crate::renderer::GameEvents) / [`ClientRequests`](crate::renderer::ClientRequests) resources, the sim's into its game
/// manager (native: the game thread's connect command; wasm: `sim_driver`).
#[derive(Clone)]
pub struct SessionChannels {
    pub game_send: crossbeam::channel::Sender<game::GameEventKind>,
    pub game_recv: crossbeam::channel::Receiver<game::GameEventKind>,
    pub client_send: crossbeam::channel::Sender<game::ClientUpdateEvent>,
    pub client_recv: crossbeam::channel::Receiver<game::ClientUpdateEvent>,
    pub request_send: crossbeam::channel::Sender<crate::instance::ClientRequest>,
    pub request_recv: crossbeam::channel::Receiver<crate::instance::ClientRequest>,
}

impl SessionChannels {
    pub fn new() -> Self {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        let (request_send, request_recv) = crossbeam::channel::unbounded();
        Self { game_send, game_recv, client_send, client_recv, request_send, request_recv }
    }

    /// Make these the app's session channels.
    pub fn install(&self, world: &mut World) {
        world.insert_resource(crate::renderer::ClientUpdates(self.client_recv.clone()));
        world.insert_resource(crate::renderer::GameEvents(self.game_send.clone()));
        world.insert_resource(crate::renderer::ClientRequests(self.request_send.clone()));
    }
}

impl Default for SessionChannels {
    fn default() -> Self {
        Self::new()
    }
}

/// Channels to nowhere: what the app holds while no session runs, so a
/// late message of an old session can't reach the next one.
pub fn install_dead_channels(world: &mut World) {
    SessionChannels::new().install(world); // the other ends drop here
}

#[cfg(not(target_arch = "wasm32"))]
pub use flow::{
    join_server_stops, leave_session, server_stops_finished, GameThread, SessionFlowPlugin, SERVER_LOG_TAIL_LINES,
    WAITING_FOR_PREVIOUS_SERVER,
};
#[cfg(target_arch = "wasm32")]
pub use web_flow::{leave_session, SessionFlowPlugin, SimConfig};

/// Starting and leaving a world session (native only; wasm keeps today's
/// single in-process session).
///
/// - In `Starting` ([`launch_when_ready`](flow::launch_when_ready)): start the
///   [`PendingWorld`]'s server once no previous server is still stopping ([`LocalHost`](crate::local_host::LocalHost)),
///   swap in a fresh set of [`SessionChannels`], send the game thread its
///   connect. The first `SetPlayer` moves on to `InWorld`.
/// - [`SessionEnded`] while `Starting` / `InWorld` (the Esc menu's Main menu,
///   a lost server, the child exiting) records why in [`LastSessionError`] and
///   goes to `MainMenu`; the transition out runs [`leave_session`].
/// - App exit (window close, the menu's Exit) runs [`leave_session`] at once,
///   so the child server is stopped.
#[cfg(not(target_arch = "wasm32"))]
mod flow {
    use std::sync::Mutex;
    use std::thread::JoinHandle;

    use bevy::app::AppExit;
    use bevy::prelude::*;
    use crossbeam::channel::Sender;
    use game::GameEventKind;

    use super::{
        end_session, install_dead_channels, AppState, HostPort, JoinServer, LastSessionError, PendingWorld, PreparingWorld,
        SessionChannels,
        SessionEnded, StartingLog, StartingStatus,
    };
    use crate::instance::{ClientRequest, Command};
    use crate::local_host::{server_log_path, LocalHost, STOP_GRACE};
    use crate::renderer::collider_debug::DebugFlagsRes;
    use crate::renderer::{ClientRequests, GameEvents, LocalPlayer};
    use crate::world_scale::WorldScale;

    /// The starting screen's line while a previous server is still stopping.
    pub const WAITING_FOR_PREVIOUS_SERVER: &str = "Waiting for the previous server…";

    /// Lines of the server log shown under a lost session's reason.
    pub const SERVER_LOG_TAIL_LINES: usize = 20;

    /// The game thread's command channel (`instance::start_game_thread`).
    #[derive(Resource, Clone)]
    pub struct GameThread(pub Sender<Command>);

    impl SessionChannels {
        /// The game thread's connect command for these channels.
        pub fn connect(
            self,
            addr: std::net::SocketAddr,
            debug_flags: game::DebugFlags,
            world_scale: WorldScale,
        ) -> Command {
            Command::ConnectToServerAndScene(
                self.game_send,
                self.game_recv,
                self.client_send,
                self.request_recv,
                addr,
                debug_flags,
                world_scale,
                crate::netcode::ConnectPolicy::default(),
                None,
            )
        }
    }


    /// Server stops still running in the background (each waits up to
    /// [`STOP_GRACE`]).
    static SERVER_STOPS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

    fn stop_in_background(host: LocalHost) {
        let handle = std::thread::Builder::new()
            .name("local-server-stop".into())
            .spawn(move || match host.stop(STOP_GRACE) {
                Some(status) => info!("[session] local server stopped: {status}"),
                None => warn!("[session] local server could not be reaped"),
            })
            .expect("spawn the server stop thread");
        SERVER_STOPS.lock().unwrap_or_else(|e| e.into_inner()).push(handle);
    }

    /// Register a background stop (tests stand in for a slow server).
    #[cfg(test)]
    pub(super) fn push_server_stop(stop: JoinHandle<()>) {
        SERVER_STOPS.lock().unwrap_or_else(|e| e.into_inner()).push(stop);
    }

    /// Wait for every background server stop (the process must not exit
    /// with a child still running).
    pub fn join_server_stops() {
        let stops = std::mem::take(&mut *SERVER_STOPS.lock().unwrap_or_else(|e| e.into_inner()));
        for stop in stops {
            let _ = stop.join();
        }
    }

    /// Tear the current session down: tell the server and the game thread,
    /// swap the channels to dead ones (BEFORE `end_session`, so nothing of
    /// the old session re-spawns from the old receiver), `end_session`, stop
    /// the local server in the background. A lost session's
    /// [`LastSessionError`] gets the server log's tail. Safe to run twice.
    pub fn leave_session(world: &mut World) {
        if let Some(requests) = world.get_resource::<ClientRequests>() {
            let _ = requests.0.send(ClientRequest::Leave);
        }
        if let Some(events) = world.get_resource::<GameEvents>() {
            let _ = events.0.send(GameEventKind::Quit);
        }
        install_dead_channels(world);
        end_session(world);
        world.remove_resource::<PendingWorld>(); // a start abandoned while waiting
        world.remove_resource::<PreparingWorld>();
        world.remove_resource::<JoinServer>();
        if let Some(host) = world.remove_resource::<LocalHost>() {
            let tail = host.log_tail(SERVER_LOG_TAIL_LINES);
            if let Some(mut error) = world.get_resource_mut::<LastSessionError>() {
                if let Some(reason) = error.0.as_mut() {
                    if !tail.is_empty() {
                        reason.push_str(&format!("\n\nServer log ({}):\n{tail}", host.log_path().display()));
                    }
                }
            }
            stop_in_background(host);
        }
    }

    /// `OnEnter(Starting)`: a session needs a chosen world.
    pub(super) fn begin_starting(world: &mut World) {
        world.resource_mut::<LastSessionError>().0 = None;
        world.get_resource_or_init::<StartingLog>().0.clear();
        if !world.contains_resource::<PendingWorld>()
            && !world.contains_resource::<PreparingWorld>()
            && !world.contains_resource::<JoinServer>()
        {
            fail_start(world, "no world was chosen to play".into());
        }
    }

    /// Whether every background server stop has finished (never blocks).
    pub fn server_stops_finished() -> bool {
        SERVER_STOPS.lock().unwrap_or_else(|e| e.into_inner()).iter().all(|stop| stop.is_finished())
    }

    /// In `Starting`, once no previous server is still stopping (it may still
    /// be saving the world): start the pending world's server, on a free port
    /// ([`LocalHost`]), and connect to it. Until
    /// then the starting screen says what it waits for; the frame never does.
    pub(super) fn launch_when_ready(world: &mut World) {
        if let Some(JoinServer(addr)) = world.remove_resource::<JoinServer>() {
            info!("[session] joining the server on {addr}");
            connect_session(world, addr);
            return;
        }
        if !world.contains_resource::<PendingWorld>() {
            return; // launched (or failed) already
        }
        if !server_stops_finished() {
            set_status(world, WAITING_FOR_PREVIOUS_SERVER);
            return;
        }
        join_server_stops(); // all finished: reaps without waiting
        set_status(world, "");
        let pending = world.remove_resource::<PendingWorld>().expect("checked above");
        let log = server_log_path(&pending.entry_dir, &pending.terrain);
        let port = world.get_resource::<HostPort>().map(|p| p.0);
        let host = match LocalHost::start(&pending.terrain, &log, port) {
            Ok(host) => host,
            Err(e) => {
                fail_start(world, format!("Could not start the server for {:?}: {e}", pending.name));
                return;
            }
        };
        info!(
            "[session] starting {:?}: server pid {} on {}, log {}",
            pending.name,
            host.pid(),
            host.addr,
            log.display()
        );
        let addr = host.addr;
        world.insert_resource(host);
        connect_session(world, addr);
    }

    /// Fresh session channels, and the game thread told to connect them to `addr`.
    fn connect_session(world: &mut World, addr: std::net::SocketAddr) {
        let channels = SessionChannels::new();
        channels.install(world);
        let debug_flags = world.resource::<DebugFlagsRes>().0.clone();
        let scale = *world.resource::<WorldScale>();
        let command = channels.connect(addr, debug_flags, scale);
        if world.resource::<GameThread>().0.send(command).is_err() {
            fail_start(world, "the game thread is gone".into());
        }
    }

    fn set_status(world: &mut World, text: &str) {
        let mut status = world.get_resource_or_init::<StartingStatus>();
        if status.0 != text {
            status.0 = text.into();
        }
    }

    /// A start that failed stays on the starting screen, which shows why
    /// (and the server log's tail, if a server ran) until Back. The session
    /// is torn down at once: a failed start leaves nothing running behind it.
    fn fail_start(world: &mut World, why: String) {
        error!("[session] {why}");
        let tail = world.get_resource::<LocalHost>().map(|h| h.log_tail(SERVER_LOG_TAIL_LINES)).unwrap_or_default();
        leave_session(world);
        world.resource_mut::<LastSessionError>().0 = Some(why);
        world.get_resource_or_init::<StartingLog>().0 = tail;
    }

    /// The first `SetPlayer` of a session: we are in. Not embarked yet — the
    /// arrival map is the embark step (`slint_overlay`).
    fn enter_world_on_player(player: Res<LocalPlayer>, mut next: ResMut<NextState<AppState>>) {
        if player.0.is_some() {
            next.set(AppState::InWorld);
        }
    }

    /// The local server exiting ends the session.
    fn watch_local_host(host: Option<ResMut<LocalHost>>, mut ended: MessageWriter<SessionEnded>) {
        let Some(mut host) = host else { return };
        if let Some(status) = host.try_exited() {
            ended.write(SessionEnded { reason: Some(format!("The server exited ({status}).")) });
        }
    }

    /// [`SessionEnded`] while a session runs: record why, go to the menu (the
    /// transition runs [`leave_session`]) — except a start that was lost
    /// (`Starting` with a reason), which stays on the starting screen to show
    /// why ([`fail_start`]).
    fn end_on_session_ended(
        mut ended: MessageReader<SessionEnded>,
        state: Res<State<AppState>>,
        mut next: ResMut<NextState<AppState>>,
        mut error: ResMut<LastSessionError>,
        mut commands: Commands,
    ) {
        let Some(last) = ended.read().last() else { return };
        if !matches!(state.get(), AppState::Starting | AppState::InWorld) {
            return;
        }
        info!("[session] session ended: {}", last.reason.as_deref().unwrap_or("left by the player"));
        match (state.get(), last.reason.clone()) {
            (AppState::Starting, Some(reason)) => {
                commands.queue(move |world: &mut World| fail_start(world, reason));
            }
            (_, reason) => {
                error.0 = reason;
                next.set(AppState::MainMenu);
            }
        }
    }

    /// The app is exiting: leave the session first (stops the child server).
    fn leave_on_app_exit(mut exits: MessageReader<AppExit>, mut commands: Commands) {
        if exits.read().last().is_some() {
            commands.queue(|world: &mut World| leave_session(world));
        }
    }

    /// The session flow; needs [`SessionPlugin`](super::SessionPlugin),
    /// the session channel resources and (to start sessions) [`GameThread`].
    pub struct SessionFlowPlugin;

    impl Plugin for SessionFlowPlugin {
        fn build(&self, app: &mut App) {
            app.add_systems(OnEnter(AppState::Starting), begin_starting)
                .add_systems(
                    OnTransition { exited: AppState::Starting, entered: AppState::MainMenu },
                    leave_session,
                )
                .add_systems(
                    OnTransition { exited: AppState::InWorld, entered: AppState::MainMenu },
                    leave_session,
                )
                .add_systems(
                    Update,
                    (
                        launch_when_ready.run_if(in_state(AppState::Starting)),
                        enter_world_on_player.run_if(in_state(AppState::Starting)),
                        watch_local_host,
                        end_on_session_ended,
                    )
                        .chain(),
                )
                .add_systems(Last, leave_on_app_exit);
        }
    }
}


/// Joining and leaving a server from the browser (wasm has no local server:
/// it can only join one).
///
/// - Join server ([`PendingServer`]) → `Starting`: a fresh set of
///   [`SessionChannels`], a fresh game manager and a WebTransport connection
///   (`sim_driver::start_session`). The first `SetPlayer` moves on to
///   `InWorld`, not embarked: the arrival map is the embark step, as native.
/// - A connect that gives up, or a lost connection, ends the session with the
///   reason ([`SessionEnded`], from `sim_driver::drive_sim`): a failed start
///   stays on the starting screen to show why, a lost session goes to the menu.
/// - Leaving (the Esc menu's Main menu, Back from a failed start) drops the
///   sim driver, which hangs up the connection.
#[cfg(target_arch = "wasm32")]
mod web_flow {
    use bevy::prelude::*;

    use super::{
        end_session, install_dead_channels, AppState, LastSessionError, PendingServer, SessionChannels, SessionEnded,
        StartingLog, StartingStatus,
    };
    use crate::renderer::collider_debug::DebugFlagsRes;
    use crate::renderer::LocalPlayer;
    use crate::sim_driver::SimDriverRes;
    use crate::world_scale::WorldScale;

    /// What every session's game manager is built with (from `main`).
    #[derive(Resource, Clone, Copy, Debug)]
    pub struct SimConfig {
        pub world_scale: WorldScale,
    }

    /// Tear the current session down: dead channels first (nothing of the old
    /// session reaches the next), drop the sim driver (its game manager's end
    /// of the outgoing channel goes with it, so the connection hangs up),
    /// `end_session`. Safe to run twice.
    pub fn leave_session(world: &mut World) {
        install_dead_channels(world);
        world.remove_resource::<SimDriverRes>();
        end_session(world);
        world.remove_resource::<PendingServer>();
    }

    fn set_status(world: &mut World, text: String) {
        let mut status = world.get_resource_or_init::<StartingStatus>();
        if status.0 != text {
            status.0 = text;
        }
    }

    /// A start that failed stays on the starting screen, which shows why
    /// until Back. Nothing is left running behind it.
    fn fail_start(world: &mut World, why: String) {
        error!("[session] {why}");
        leave_session(world);
        world.resource_mut::<LastSessionError>().0 = Some(why);
    }

    /// `OnEnter(Starting)`: connect to the [`PendingServer`].
    fn begin_join(world: &mut World) {
        world.resource_mut::<LastSessionError>().0 = None;
        world.get_resource_or_init::<StartingLog>().0.clear();
        let Some(PendingServer(address)) = world.remove_resource::<PendingServer>() else {
            fail_start(world, "no server was chosen to join".into());
            return;
        };
        let server = match crate::web_endpoint::web_server_for(&address) {
            Ok(server) => server,
            Err(why) => {
                fail_start(world, format!("Cannot join {address:?}: {why}"));
                return;
            }
        };
        info!("[session] joining {address} ({server:?})");
        set_status(world, format!("Connecting to {address}\u{2026}"));
        let channels = SessionChannels::new();
        channels.install(world);
        let debug_flags = world.resource::<DebugFlagsRes>().0.clone();
        let config = *world.resource::<SimConfig>();
        let driver = crate::sim_driver::start_session(server, channels, debug_flags, config.world_scale, None);
        world.insert_resource(driver);
    }

    /// The first `SetPlayer` of a session: we are in. Not embarked yet — the
    /// arrival map is the embark step (`slint_overlay`).
    fn enter_world_on_player(player: Res<LocalPlayer>, mut next: ResMut<NextState<AppState>>) {
        if player.0.is_some() {
            next.set(AppState::InWorld);
        }
    }

    /// In the world, waiting for the map: say so on the starting screen.
    fn receiving_map(mut status: ResMut<StartingStatus>) {
        status.0 = crate::frontend::RECEIVING_MAP.into();
    }

    /// [`SessionEnded`] while a session runs: record why and go to the menu
    /// (the transition runs [`leave_session`]) — except a start that was
    /// lost, which stays on the starting screen to show why.
    fn end_on_session_ended(
        mut ended: MessageReader<SessionEnded>,
        state: Res<State<AppState>>,
        mut next: ResMut<NextState<AppState>>,
        mut error: ResMut<LastSessionError>,
        mut commands: Commands,
    ) {
        let Some(last) = ended.read().last() else { return };
        if !matches!(state.get(), AppState::Starting | AppState::InWorld) {
            return;
        }
        info!("[session] session ended: {}", last.reason.as_deref().unwrap_or("left by the player"));
        match (state.get(), last.reason.clone()) {
            (AppState::Starting, Some(reason)) => {
                commands.queue(move |world: &mut World| fail_start(world, reason));
            }
            (_, reason) => {
                error.0 = reason;
                next.set(AppState::MainMenu);
            }
        }
    }

    /// The browser session flow; needs [`SessionPlugin`](super::SessionPlugin),
    /// the session channel resources and [`SimConfig`].
    pub struct SessionFlowPlugin;

    impl Plugin for SessionFlowPlugin {
        fn build(&self, app: &mut App) {
            app.add_systems(OnEnter(AppState::Starting), begin_join)
                .add_systems(OnEnter(AppState::InWorld), receiving_map)
                .add_systems(OnTransition { exited: AppState::Starting, entered: AppState::MainMenu }, leave_session)
                .add_systems(OnTransition { exited: AppState::InWorld, entered: AppState::MainMenu }, leave_session)
                .add_systems(
                    Update,
                    (
                        crate::sim_driver::drive_sim,
                        enter_world_on_player.run_if(in_state(AppState::Starting)),
                        end_on_session_ended,
                    )
                        .chain(),
                );
        }
    }
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

    /// Stand-in for the bridge: one `SessionScoped` entity per update read
    /// from whatever `ClientUpdates` currently holds. Not gated, so it would
    /// re-spawn the old session's world if the old receiver survived.
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_per_update(mut commands: Commands, updates: Res<crate::renderer::ClientUpdates>) {
        for _ in updates.0.try_iter() {
            commands.spawn(SessionScoped);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn scoped(app: &mut App) -> usize {
        app.world_mut().query_filtered::<(), With<SessionScoped>>().iter(app.world()).count()
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    #[cfg(not(target_arch = "wasm32"))]
    fn leave_swaps_channels_before_end_session() {
        use crate::instance::ClientRequest;
        let mut app = app(true);
        app.init_resource::<crate::renderer::LocalPlayer>()
            .add_plugins(SessionFlowPlugin)
            .add_systems(Update, spawn_per_update);
        let old = SessionChannels::new();
        old.install(app.world_mut());
        old.client_send.send(game::ClientUpdateEvent::SetPlayer(1)).unwrap();
        app.update();
        assert_eq!(scoped(&mut app), 1, "the session's world");

        app.world_mut().write_message(SessionEnded { reason: Some("server gone".into()) });
        app.update(); // Update: the menu is next
        old.client_send.send(game::ClientUpdateEvent::SetPlayer(1)).unwrap();
        app.update(); // StateTransition: leave_session

        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::MainMenu);
        assert_eq!(scoped(&mut app), 0, "a late update of the old session spawned into the menu");
        assert_eq!(app.world().resource::<LastSessionError>().0.as_deref(), Some("server gone"));
        assert!(matches!(old.request_recv.try_recv(), Ok(ClientRequest::Leave)), "no Leave sent");
        assert!(matches!(old.game_recv.try_iter().last(), Some(game::GameEventKind::Quit)), "no Quit sent");

        old.client_send.send(game::ClientUpdateEvent::SetPlayer(1)).unwrap();
        app.update();
        app.update();
        assert_eq!(scoped(&mut app), 0, "the old receiver is still read");
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    #[cfg(not(target_arch = "wasm32"))]
    fn starting_waits_for_the_previous_server_without_blocking() {
        let mut app = app(false);
        let (command_send, _command_recv) = crossbeam::channel::unbounded();
        app.init_resource::<crate::renderer::LocalPlayer>()
            .insert_resource(GameThread(command_send))
            .add_plugins(SessionFlowPlugin);
        install_dead_channels(app.world_mut());
        let (release, wait) = crossbeam::channel::bounded::<()>(0);
        flow::push_server_stop(std::thread::spawn(move || {
            let _ = wait.recv();
        }));
        app.insert_resource(PendingWorld {
            entry_dir: "/nonexistent/world".into(),
            terrain: "/nonexistent/world/terrain.world".into(),
            name: "w".into(),
        });
        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Starting);
        for _ in 0..3 {
            let frame = std::time::Instant::now();
            app.update();
            assert!(frame.elapsed() < std::time::Duration::from_secs(1), "a frame waited on the old server");
        }
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Starting);
        assert!(app.world().contains_resource::<PendingWorld>(), "launched while the old server was stopping");
        assert!(!app.world().contains_resource::<crate::local_host::LocalHost>());
        assert_eq!(app.world().resource::<StartingStatus>().0, WAITING_FOR_PREVIOUS_SERVER);

        // The old server is gone: the launch goes ahead (and fails here, with
        // no server binary next to the test executable → the starting screen
        // shows why until Back).
        release.send(()).unwrap();
        while !server_stops_finished() {
            std::thread::yield_now();
        }
        app.update();
        app.update();
        assert!(!app.world().contains_resource::<PendingWorld>());
        assert_eq!(app.world().resource::<StartingStatus>().0, "");
        let error = app.world().resource::<LastSessionError>().0.clone().unwrap_or_default();
        assert!(error.starts_with("Could not start the server"), "{error}");
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Starting);
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    #[cfg(not(target_arch = "wasm32"))]
    fn menu_start_enters_the_world_not_embarked() {
        let mut app = app(false);
        app.init_resource::<crate::renderer::LocalPlayer>().add_plugins(SessionFlowPlugin);
        SessionChannels::new().install(app.world_mut());
        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Starting);
        app.update();
        app.world_mut().resource_mut::<crate::renderer::LocalPlayer>().0 = Some(1);
        app.update();
        app.update();
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::InWorld);
        assert_eq!(*app.world().resource::<Embarked>(), Embarked(false), "the arrival map embarks");
        assert_eq!(frontend_cameras(&mut app).len(), 1, "the frontend camera keeps the screen");
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_start_lost_with_a_reason_stays_on_the_starting_screen() {
        let mut app = app(false);
        app.init_resource::<crate::renderer::LocalPlayer>().add_plugins(SessionFlowPlugin);
        SessionChannels::new().install(app.world_mut());
        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Starting);
        app.update();
        app.world_mut().write_message(SessionEnded { reason: Some("The server exited (exit status: 1).".into()) });
        app.update();
        app.update();
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Starting);
        assert_eq!(
            app.world().resource::<LastSessionError>().0.as_deref(),
            Some("The server exited (exit status: 1).")
        );
    }

    #[test]
    fn only_world_views_draw_the_world() {
        assert!(view_draws_world(false));
        assert!(!view_draws_world(true), "the frontend camera's view draws no terrain");
    }

    #[test]
    fn exactly_one_layer_has_the_screen() {
        use AppState::*;
        // (state, embarked, map arrived) → frontend screens shown, arrival map shown
        let cases = [
            (MainMenu, false, false, true, false),
            (Starting, false, true, true, false),
            (InWorld, false, false, true, false), // receiving the world map
            (InWorld, false, true, false, true),
            (InWorld, true, true, false, false),
        ];
        for (state, embarked, map, frontend, arrival) in cases {
            assert_eq!(frontend_ui_shown(state, embarked, map), frontend, "{state:?} {embarked} {map}");
            assert_eq!(arrival_map_shown(state, embarked, map), arrival, "{state:?} {embarked} {map}");
        }
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    #[cfg(not(target_arch = "wasm32"))]
    fn main_menu_leave_clears_the_last_error() {
        let mut app = app(true);
        app.init_resource::<crate::renderer::LocalPlayer>().add_plugins(SessionFlowPlugin);
        SessionChannels::new().install(app.world_mut());
        app.world_mut().resource_mut::<LastSessionError>().0 = Some("old".into());
        app.world_mut().write_message(SessionEnded { reason: None });
        app.update();
        app.update();
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::MainMenu);
        assert_eq!(app.world().resource::<LastSessionError>().0, None);
        assert_eq!(*app.world().resource::<Embarked>(), Embarked(false));
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
            (&[], false),
            (&[("VOXEL_WORLD_FILE", "")], false),
            (&[("VOXEL_WORLD_FILE", "worlds/a.world")], true),
            (&[("VOXEL_SKIP_MENU", "1")], true),
            (&[("VOXEL_SKIP_MENU", "0")], false),
            (&[("RENDER_BENCH", "1")], true),
            (&[("RENDER_BENCH", "0")], false),
            (&[("VOXEL_AUTOPILOT", "shuttle")], true),
            (&[("VOXEL_AUTOPILOT", "")], false),
        ];
        for (vars, want) in cases {
            let env: HashMap<&str, &str> = vars.iter().copied().collect();
            let got = skip_menu_from_env(|k| env.get(k).map(|v| v.to_string()));
            assert_eq!(got, *want, "{vars:?}");
        }
    }
}
