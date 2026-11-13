//! Leaving a world and joining again, in one process: after
//! `session::end_session` the client's world state must equal a fresh app's,
//! and the next session must load as the first did.
//!
//! The real client app — `DefaultPlugins` without a window backend or a GPU
//! (`backends: None`, so there is no render world) plus `SessionPlugin` and
//! `SimBridgePlugin` in commander mode — driven in lockstep against an
//! offline `LocalServer`, like `BevyInputHarness`. The smallest world that
//! loads: home region only, no LOD rings (a production-sized world in a
//! headless test runs out of memory).
//!
//! Both tests are slow (a full app, two joins): run them with
//! `just test-one client "--test session_reset -- --include-ignored"`.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::render::settings::{RenderCreation, WgpuSettings};
use bevy::render::RenderPlugin;
use bevy::window::{ExitCondition, WindowPlugin};
use crossbeam::channel::{unbounded, Receiver};
use game::ServerPacket;

use client::renderer::bridge::{RegionRoots, SimEntity, SimEntityMap};
use client::renderer::bricks::BrickOf;
use client::renderer::commander::selection::Selection;
use client::renderer::commander::tiles::TileMirror;
use client::renderer::commander::{CameraMode, CommanderCamera, CommanderView};
use client::renderer::far_voxel::residency::FarVoxelResidency;
use client::renderer::hud::HudStatus;
use client::renderer::join_timeline::WorldReady;
use client::renderer::lod_rings::LodRings;
use client::renderer::meshing::queue::{ChunkGrid, RemeshQueue};
use client::renderer::slint_overlay::SlintLayer;
use client::renderer::history_view::HistoryView;
use client::renderer::world_map::WorldMapState;
use client::renderer::{ClientUpdates, GameEvents, LocalPlayer, SimBridgePlugin, UiCamera};
use client::session::{end_session, AppState, Embarked, FrontendCamera, SessionPlugin, SessionScoped};
use client::world_scale::WorldScale;
use client::{GameInstanceManager, LocalServer};

/// Lockstep steps a join may take to reach `world_ready`.
const JOIN_STEPS: usize = 3_000;
/// Wall-clock cap on one join (the meshing tasks run on real threads).
const JOIN_DEADLINE: Duration = Duration::from_secs(120);

/// One world session's game side: the offline server and the client
/// instance, pumped by hand.
struct Session {
    server: LocalServer,
    client: GameInstanceManager,
    server_to_client: Receiver<ServerPacket>,
}

impl Session {
    /// Fresh channels, swapped into the app BEFORE anything else happens —
    /// the order the session flow uses, so a late message of the old
    /// session lands in a dropped receiver.
    fn start(app: &mut App) -> Self {
        let (game_send, game_recv) = unbounded();
        let (bridge_send, bridge_recv) = unbounded();
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut client =
            GameInstanceManager::new(game_send.clone(), game_recv, bridge_send, addr, game::DebugFlags::default());
        client.set_lod_table(&[]);
        client.set_near_radius(0);
        let (server_to_client_send, server_to_client) = unbounded();
        let server = LocalServer::new_with_generator(
            client.client_packet_recv(),
            server_to_client_send,
            client::local_server::lightweight_test_generator(),
        )
        .expect("local server");
        app.insert_resource(ClientUpdates(bridge_recv)).insert_resource(GameEvents(game_send));
        client.start();
        Self { server, client, server_to_client }
    }

    /// One lockstep frame: the app, a sim tick on both sides, the app again.
    fn step(&mut self, app: &mut App) {
        app.update();
        self.client.send_tick();
        self.client.pump_packets(&self.server_to_client).expect("client pump");
        self.server.pump().expect("server pump");
        self.server.tick();
        self.client.pump_packets(&self.server_to_client).expect("client reconcile pump");
        app.update();
    }

    fn step_until(&mut self, app: &mut App, what: &str, done: impl Fn(&World) -> bool) {
        let start = Instant::now();
        for _ in 0..JOIN_STEPS {
            if done(app.world()) {
                return;
            }
            assert!(start.elapsed() < JOIN_DEADLINE, "{what}: not reached in {JOIN_DEADLINE:?}");
            self.step(app);
        }
        panic!("{what}: not reached in {JOIN_STEPS} steps");
    }

    fn join(&mut self, app: &mut App) {
        self.step_until(app, "world_ready", |w| w.resource::<WorldReady>().0);
    }
}

/// The client app with no window backend and no GPU.
fn app() -> App {
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                    backends: None,
                    ..Default::default()
                })),
                ..Default::default()
            })
            .set(WindowPlugin {
                primary_window: Some(Window::default()),
                exit_condition: ExitCondition::DontExit,
                close_when_requested: false,
                ..Default::default()
            })
            .set(AssetPlugin { file_path: "../../assets".into(), ..Default::default() })
            .disable::<bevy::winit::WinitPlugin>()
            .disable::<bevy::log::LogPlugin>(),
    )
    // With no renderer `RenderPlugin` skips the main→render entity sync, but
    // the extract plugins' removal hooks still report into its queue.
    .add_plugins(bevy::render::sync_world::SyncWorldPlugin)
    .insert_resource(WorldScale::TINY)
    .add_plugins(SessionPlugin { skip_menu: true, login_first: false });
    // Placeholder channels: each session swaps in its own.
    let (game_send, _) = unbounded();
    let (_, client_recv) = unbounded();
    let (requests, _) = unbounded();
    app.add_plugins(SimBridgePlugin {
        client_recv,
        game_send,
        requests,
        debug_flags: game::DebugFlags::default(),
        camera_mode: CameraMode::Commander,
    });
    app.finish();
    app.cleanup();
    app
}

/// Leave: the session flow swaps in dead channels first, then tears down.
fn leave(app: &mut App) {
    let (_, dead) = unbounded();
    let (to_nowhere, _) = unbounded();
    app.insert_resource(ClientUpdates(dead)).insert_resource(GameEvents(to_nowhere));
    end_session(app.world_mut());
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::MainMenu);
}

/// Back into a world: what the session flow's Starting → InWorld plus the
/// arrival map's embark do.
fn enter_world(app: &mut App) {
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::InWorld);
    app.update();
    app.world_mut().resource_mut::<Embarked>().0 = true;
}

fn count<F: bevy::ecs::query::QueryFilter>(app: &mut App) -> usize {
    app.world_mut().query_filtered::<(), F>().iter(app.world()).count()
}

/// The `GameUi` / `HudUi` properties a session sets, as (name, value) pairs.
fn game_ui_props(ui: &slint_ui::GameUi) -> Vec<(&'static str, String)> {
    vec![
        ("tool-name", ui.get_tool_name().to_string()),
        ("fortress-tab", ui.get_fortress_tab().to_string()),
        ("settings-open", ui.get_settings_open().to_string()),
        ("encyclopedia-open", ui.get_encyclopedia_open().to_string()),
        ("level-text", ui.get_level_text().to_string()),
        ("tools-available", ui.get_tools_available().to_string()),
        ("active-tool", ui.get_active_tool().to_string()),
        ("menu-open", ui.get_menu_open().to_string()),
        ("map-open", ui.get_map_open().to_string()),
        ("map-available", ui.get_map_available().to_string()),
    ]
}

fn hud_props(ui: &slint_ui::HudUi) -> Vec<(&'static str, String)> {
    vec![
        ("minimap-visible", ui.get_minimap_visible().to_string()),
        ("band-visible", ui.get_band_visible().to_string()),
    ]
}

fn live_game_ui(app: &App) -> Vec<(&'static str, String)> {
    game_ui_props(app.world().get_non_send::<SlintLayer<slint_ui::GameUi>>().expect("game UI layer").overlay.ui())
}

fn live_hud(app: &App) -> Vec<(&'static str, String)> {
    hud_props(app.world().get_non_send::<SlintLayer<slint_ui::HudUi>>().expect("HUD layer").overlay.ui())
}

/// Every piece of per-world state the client keeps is back at its fresh value.
fn assert_no_world_state(app: &mut App, fresh_ui: &[(&'static str, String)], fresh_hud: &[(&'static str, String)]) {
    assert_eq!(count::<With<SessionScoped>>(app), 0, "SessionScoped trees survived");
    assert_eq!(count::<With<SimEntity>>(app), 0, "sim entities survived");
    assert_eq!(count::<With<BrickOf>>(app), 0, "render bricks survived");
    let w = app.world();
    assert!(w.resource::<RegionRoots>().0.is_empty(), "RegionRoots");
    assert!(w.resource::<SimEntityMap>().0.is_empty(), "SimEntityMap");
    assert!(w.resource::<ChunkGrid>().0.is_empty(), "ChunkGrid");
    assert!(w.resource::<RemeshQueue>().dirty.is_empty(), "RemeshQueue");
    let rings = w.resource::<LodRings>();
    assert!(rings.roots.is_empty() && rings.retired.is_empty() && rings.pending.is_empty(), "LodRings");
    assert_eq!(w.resource::<TileMirror>().region(), None, "TileMirror");
    assert!(w.resource::<WorldMapState>().map.is_none(), "WorldMapState");
    let hv = w.resource::<HistoryView>();
    assert!(
        hv.sites.is_empty()
            && hv.calendar.is_none()
            && hv.notices.is_empty()
            && hv.embark_options.is_none()
            && hv.embark_result.is_none()
            && hv.people.is_empty()
            && hv.fortress.is_none()
            && !hv.retired
            && hv.selection == Default::default()
            && !hv.options_requested
            && !hv.embark_pending
            && hv.embark_refusal.is_none(),
        "HistoryView"
    );
    assert_eq!(w.resource::<FarVoxelResidency>().resident_count(), 0, "far residency");
    assert!(!w.resource::<WorldReady>().0, "WorldReady");
    assert_eq!(w.resource::<LocalPlayer>().0, None, "LocalPlayer");
    assert_eq!(w.resource::<HudStatus>().home_region, None, "HudStatus");
    assert!(w.resource::<Selection>().0.is_empty(), "Selection");
    assert!(!w.resource::<CommanderCamera>().placed, "CommanderCamera");
    assert_eq!(live_game_ui(app), fresh_ui, "GameUi properties");
    assert_eq!(live_hud(app), fresh_hud, "HudUi properties");
    // And nothing the old components drew is left in the textures that are
    // composited under the menu screens.
    let game_target = app.world().get_non_send::<SlintLayer<slint_ui::GameUi>>().unwrap().target().clone();
    let hud_target = app.world().get_non_send::<SlintLayer<slint_ui::HudUi>>().unwrap().target().clone();
    for (name, handle) in [("GameUi", game_target), ("HudUi", hud_target)] {
        let images = app.world().resource::<Assets<Image>>();
        let data = images.get(&handle).and_then(|i| i.data.as_ref()).expect("layer texture data");
        assert!(data.iter().all(|&b| b == 0), "{name} texture still holds the old world's pixels");
    }
}

/// The cameras that render and carry `UiCamera`: (frontend?, commander?).
fn active_ui_cameras(app: &mut App) -> Vec<(bool, bool)> {
    let mut q = app.world_mut().query_filtered::<(&Camera, Has<FrontendCamera>, Has<CommanderView>), With<UiCamera>>();
    q.iter(app.world()).filter(|(c, ..)| c.is_active).map(|(_, f, c)| (f, c)).collect()
}

fn active_cameras(app: &mut App) -> usize {
    let mut q = app.world_mut().query::<&Camera>();
    q.iter(app.world()).filter(|c| c.is_active).count()
}

/// Every sim entity on screen belongs to the current session's maps.
fn assert_entities_belong_to_session(app: &mut App) {
    let mut q = app.world_mut().query::<(Entity, &SimEntity)>();
    let entities: Vec<_> = q.iter(app.world()).map(|(e, s)| (e, s.region, s.key)).collect();
    let map = &app.world().resource::<SimEntityMap>().0;
    let roots = &app.world().resource::<RegionRoots>().0;
    for (e, region, key) in entities {
        assert!(roots.contains_key(&region), "{e:?} is in region {region:?}, which is not loaded");
        assert_eq!(map.get(&(region, key)), Some(&e), "{e:?} is not this session's entity");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn second_session_starts_clean() {
    let mut app = app();
    app.update();
    let fresh_ui = game_ui_props(slint_ui::Overlay::with(slint_ui::GameUi::new).expect("game UI").ui());
    let fresh_hud = hud_props(slint_ui::Overlay::with(slint_ui::HudUi::new).expect("HUD").ui());

    let mut first = Session::start(&mut app);
    first.join(&mut app);
    assert!(count::<With<SessionScoped>>(&mut app) > 0, "the first session spawned its region");
    assert!(!app.world().resource::<ChunkGrid>().0.is_empty(), "the first session meshed terrain");
    assert_ne!(live_game_ui(&app), fresh_ui, "the first session showed itself in the game UI");
    assert_eq!(active_ui_cameras(&mut app), vec![(false, true)], "in the world the commander camera has the UI");
    // A played fortress and a half-made embark choice, which must not reach the next world.
    {
        let mut hv = app.world_mut().resource_mut::<HistoryView>();
        hv.fortress = Some(game::history::SiteId(3));
        hv.retired = true;
        hv.selection.pick_civ(0);
        hv.selection.chosen.insert(game::history::PersonId(5));
        hv.options_requested = true;
        hv.embark_pending = true;
        hv.embark_refusal = Some((game::history::EmbarkRefusal::NoHistory, 1.0));
    }

    leave(&mut app);
    drop(first);
    for _ in 0..5 {
        app.update();
    }
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::MainMenu);
    assert_no_world_state(&mut app, &fresh_ui, &fresh_hud);
    // The menu has the screen: one camera renders, the frontend's, and it
    // alone carries `UiCamera`.
    assert_eq!(active_ui_cameras(&mut app), vec![(true, false)], "the frontend camera has the UI");
    assert_eq!(active_cameras(&mut app), 1, "only the frontend camera renders");

    let mut second = Session::start(&mut app);
    enter_world(&mut app);
    second.join(&mut app);
    assert!(app.world().resource::<WorldReady>().0, "the second session reaches world_ready");
    assert_eq!(app.world().resource::<RegionRoots>().0.len(), 1, "one region, the second session's");
    assert_eq!(count::<With<SessionScoped>>(&mut app), 2, "region root + bodies root");
    assert_entities_belong_to_session(&mut app);
    assert_eq!(active_ui_cameras(&mut app), vec![(false, true)], "the world has the screen again");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn leave_during_join_then_rejoin() {
    let mut app = app();
    app.update();
    let fresh_ui = game_ui_props(slint_ui::Overlay::with(slint_ui::GameUi::new).expect("game UI").ui());
    let fresh_hud = hud_props(slint_ui::Overlay::with(slint_ui::HudUi::new).expect("HUD").ui());

    let mut first = Session::start(&mut app);
    first.step_until(&mut app, "NewRegion", |w| !w.resource::<RegionRoots>().0.is_empty());
    // One more frame: snapshot voxel and meshing tasks are in flight.
    first.step(&mut app);
    assert!(!app.world().resource::<WorldReady>().0, "left before the world was ready");

    leave(&mut app);
    // The old session's game side keeps producing; none of it may land.
    for _ in 0..30 {
        first.step(&mut app);
    }
    drop(first);
    assert_no_world_state(&mut app, &fresh_ui, &fresh_hud);

    let mut second = Session::start(&mut app);
    enter_world(&mut app);
    second.join(&mut app);
    assert_eq!(app.world().resource::<RegionRoots>().0.len(), 1);
    assert_entities_belong_to_session(&mut app);
}
