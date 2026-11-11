//! The fortress panel's Debug tab (`dev_ui` feature) — dev tooling, never
//! player UI: the last tab of the top-left fortress panel in `GameUi`
//! (crates/slint-ui/ui/debug-tab.slint), on the tab row only in `dev_ui`
//! builds. F3 opens it (and collapses it again), as does a click on the tab.
//! Stats, the debug toggles (the same flags as F5–F8) and stand-ins for
//! systems that don't exist yet.
//!
//! The tab does not pause the game: in the commander view the pointer is
//! free anyway, so the panel only takes it where it drew (`ui_pointer`, like
//! any Slint layer) and panning, zoom, tools and selection keep working. Only
//! when the sim holds the cursor (first person) does it set
//! `CursorPolicy::overlay_open` ([`panel_claims_cursor`]), which frees the
//! cursor and stops `forward_input` sending clicks and mouse motion to the sim.
use std::cell::RefCell;
use std::rc::Rc;

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use slint_ui::slint::{Model, ModelRc, VecModel};
use slint_ui::{DevStat, DevTickRow, GameUi};

use super::boundaries::BoundaryOverlay;
use super::collider_debug::DebugFlagsRes;
use super::cursor::CursorPolicy;
use super::grid_debug::{GridDebug, TerrainDebug};
use super::hud::{HudStatus, RegionTickTimes};
use super::slint_overlay::SlintLayer;
use super::{GameEvents, LocalPlayer, UiCamera};

/// Opens and collapses the Debug tab.
pub const DEV_UI_KEY: KeyCode = KeyCode::F3;
/// How often the stats refresh while the tab is open.
pub const DEV_STATS_INTERVAL_S: f32 = 0.25;
/// The Debug tab's index on the fortress panel's tab row (after Governance,
/// People, Designations, Stocks; `GameUi.fortress-tab`).
pub const DEBUG_TAB: i32 = 4;
/// `GameUi.fortress-tab` with every tab collapsed.
const NO_TAB: i32 = -1;

/// The panel's debug toggles, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevToggle {
    Boundaries,
    ChunkGrid,
    HideLod,
    HideNear,
    Colliders,
}

impl DevToggle {
    pub const ALL: [DevToggle; 5] = [
        DevToggle::Boundaries,
        DevToggle::ChunkGrid,
        DevToggle::HideLod,
        DevToggle::HideNear,
        DevToggle::Colliders,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DevToggle::Boundaries => "Region boundaries (F8)",
            DevToggle::ChunkGrid => "Chunk grid (F5)",
            DevToggle::HideLod => "Hide LOD (F6)",
            DevToggle::HideNear => "Hide near terrain (F7)",
            DevToggle::Colliders => "Collider wireframes",
        }
    }
}

/// What a panel callback asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DevAction {
    Toggle(DevToggle, bool),
    /// Stands in for workers until jobs exist: every designation in the home
    /// region done at once (an undoable sim order).
    CompleteOrders,
    /// Drops a 1 m dynamic body (`Order::DevDropBody`) above the ground the
    /// cursor last pointed at off the panel ([`DevDropTarget`]).
    DropBody,
}

/// The ground point (`GroundPick`) the cursor last pointed at while it was
/// not over a Slint layer: where "Drop body at cursor" aims, since the
/// cursor is on the button when it is clicked.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct DevDropTarget(pub Option<(game::RegionCoords, Vec3)>);

/// Filled by the panel's callbacks during event dispatch (`!Send`).
struct DevActions(Rc<RefCell<Vec<DevAction>>>);

pub struct DevUiPlugin;

impl Plugin for DevUiPlugin {
    fn build(&self, app: &mut App) {
        // The tab lives in the session-scoped game UI layer
        // (`slint_overlay`), whose init calls [`connect_debug_tab`]; the
        // toggles' flags are preferences and outlive the session.
        use crate::session::SessionResetExt as _;
        app.add_session_reset::<DevDropTarget>().add_systems(
            Update,
            (remember_drop_target, toggle_dev_ui, apply_dev_actions, push_dev_panel)
                .chain()
                .run_if(crate::session::world_systems_enabled()),
        );
    }
}

/// Puts the Debug tab on the game UI's fortress panel and queues its
/// callbacks (called from the game UI layer's init, once per session).
pub fn connect_debug_tab(ui: &GameUi, world: &mut World) {
    ui.set_debug_available(true);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let queue = actions.clone();
    ui.on_dev_toggle(move |i, on| {
        if let Some(toggle) = DevToggle::ALL.get(i as usize) {
            queue.borrow_mut().push(DevAction::Toggle(*toggle, on));
        }
    });
    let queue = actions.clone();
    ui.on_dev_complete_orders(move || queue.borrow_mut().push(DevAction::CompleteOrders));
    let queue = actions.clone();
    ui.on_dev_drop_body(move || queue.borrow_mut().push(DevAction::DropBody));
    world.insert_non_send(DevActions(actions));
}

/// The fortress tab F3 leaves open: the Debug tab, or none if it was open.
pub fn f3_tab(current: i32) -> i32 {
    if current == DEBUG_TAB { NO_TAB } else { DEBUG_TAB }
}

/// Whether the open panel must claim the cursor (and with it the game's
/// input): only to free a cursor the sim has grabbed.
pub fn panel_claims_cursor(open: bool, sim_grab: bool) -> bool {
    open && sim_grab
}

fn toggle_dev_ui(
    keys: Res<ButtonInput<KeyCode>>,
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    mut cursor: ResMut<CursorPolicy>,
) {
    let Some(slint) = slint else { return };
    let ui = slint.overlay.ui();
    // Not over the arrival map, which hides the fortress panel.
    if keys.just_pressed(DEV_UI_KEY) && !ui.get_map_modal() {
        ui.set_fortress_tab(f3_tab(ui.get_fortress_tab()));
    }
    let open = ui.get_fortress_tab() == DEBUG_TAB && !ui.get_map_modal();
    let claim = panel_claims_cursor(open, cursor.sim_grab);
    if cursor.overlay_open != claim {
        cursor.overlay_open = claim;
    }
}

/// The flag a toggle shows and flips.
pub fn toggle_checked(toggle: DevToggle, world: &World) -> bool {
    match toggle {
        DevToggle::Boundaries => world.resource::<BoundaryOverlay>().enabled,
        DevToggle::ChunkGrid => world.resource::<GridDebug>().enabled,
        DevToggle::HideLod => world.resource::<TerrainDebug>().hide_lod,
        DevToggle::HideNear => world.resource::<TerrainDebug>().hide_near,
        // Shared with the game thread, which only builds lines while on.
        DevToggle::Colliders => world.resource::<DebugFlagsRes>().0.collider_wireframes(),
    }
}

pub fn apply_dev_action(action: DevAction, world: &mut World) {
    match action {
        DevAction::Toggle(toggle, on) => match toggle {
            DevToggle::Boundaries => world.resource_mut::<BoundaryOverlay>().enabled = on,
            DevToggle::ChunkGrid => world.resource_mut::<GridDebug>().enabled = on,
            DevToggle::HideLod => world.resource_mut::<TerrainDebug>().hide_lod = on,
            DevToggle::HideNear => world.resource_mut::<TerrainDebug>().hide_near = on,
            DevToggle::Colliders => world.resource::<DebugFlagsRes>().0.set_collider_wireframes(on),
        },
        DevAction::CompleteOrders => {
            if let Some(client) = world.resource::<LocalPlayer>().0 {
                let order = game::GameEventKind::Order(client, game::Order::DevCompleteAll);
                let _ = world.resource::<GameEvents>().0.send(order);
            }
        }
        DevAction::DropBody => {
            let target = world.get_resource::<DevDropTarget>().and_then(|t| t.0);
            let home = world.get_resource::<HudStatus>().and_then(|h| h.home_region);
            if let (Some(client), Some(order)) = (world.resource::<LocalPlayer>().0, drop_body_order(target, home)) {
                let _ = world.resource::<GameEvents>().0.send(game::GameEventKind::Order(client, order));
            }
        }
    }
}

/// The dev drop order for ground point `target` (a region and a point in
/// its local frame): the tile under it in the `home` region's block
/// coordinates — orders run in the home region, which refuses a tile off its
/// columns. `None` without a point or a home.
pub fn drop_body_order(
    target: Option<(game::RegionCoords, Vec3)>,
    home: Option<game::RegionCoords>,
) -> Option<game::Order> {
    let ((region, local), home) = (target?, home?);
    Some(game::Order::DevDropBody { at: super::commander::move_order::hover_tile(region, local, home) })
}

/// Keeps [`DevDropTarget`] on the ground under the cursor while the cursor
/// is not over a Slint layer.
fn remember_drop_target(
    cursor: Res<CursorPolicy>,
    pick: Option<Res<super::commander::selection::GroundPick>>,
    mut target: ResMut<DevDropTarget>,
) {
    if cursor.ui_pointer {
        return;
    }
    let Some(pick) = pick else { return };
    if let Some(p) = pick.0 {
        if target.0 != Some(p) {
            target.0 = Some(p);
        }
    }
}

fn apply_dev_actions(world: &mut World) {
    let Some(actions) = world.get_non_send::<DevActions>() else { return };
    let pending: Vec<DevAction> = actions.0.borrow_mut().drain(..).collect();
    for action in pending {
        apply_dev_action(action, world);
    }
}

/// The stats rows, in display order, formatted as the egui panel did.
pub fn dev_stats(
    fps: Option<f64>,
    frame_ms: Option<f64>,
    pos: Option<Vec3>,
    status: &HudStatus,
    entities: usize,
    calendar: Option<game::history::CalendarTick>,
) -> Vec<(String, String)> {
    let region = |r: Option<game::RegionCoords>| r.map_or("--".into(), |r| format!("({}, {})", r.x, r.z));
    vec![
        ("FPS".into(), fps.map_or("--".into(), |f| format!("{f:.0}"))),
        ("Frame".into(), frame_ms.map_or("--".into(), |ms| format!("{ms:.2} ms"))),
        ("Position".into(), pos.map_or("--".into(), |p| format!("{:.1} / {:.1} / {:.1}", p.x, p.y, p.z))),
        ("Home region".into(), region(status.home_region)),
        ("Viewer region".into(), region(status.viewer_region)),
        ("Connection".into(), format!("{:?}", status.connection)),
        ("Entities".into(), entities.to_string()),
        ("Date".into(), calendar.map_or("--".into(), |c| c.date().to_string())),
    ]
}

/// The Debug tab's tick-time table: one row per live region (`RegionId`
/// order) — region, then mean / p50 / p95 / p99 / max in ms; the home region
/// is marked `*`.
pub fn tick_rows(
    times: &[(game::RegionId, game::perf::TickTimeStats)],
    home: Option<game::RegionCoords>,
) -> Vec<[String; 6]> {
    let ms = |us: u32| format!("{:.2}", us as f64 / 1000.0);
    times
        .iter()
        .map(|(id, s)| {
            let mark = if Some(*id) == home { "*" } else { "" };
            [format!("({}, {}){mark}", id.x, id.z), ms(s.mean_us), ms(s.p50_us), ms(s.p95_us), ms(s.p99_us), ms(s.max_us)]
        })
        .collect()
}

/// Client state → the Debug tab's properties: the toggles when a flag
/// changed, the stats every [`DEV_STATS_INTERVAL_S`] while the tab is open.
/// Models are always pushed fresh, which also resets a checkbox the user just
/// clicked.
fn push_dev_panel(
    world: &mut World,
    mut since: Local<Option<f32>>,
    mut shown: Local<Vec<bool>>,
    mut shown_ticks: Local<Vec<[String; 6]>>,
    mut cameras: Local<Option<QueryState<&GlobalTransform, With<UiCamera>>>>,
    mut all: Local<Option<QueryState<()>>>,
) {
    let Some(layer) = world.get_non_send::<SlintLayer<GameUi>>() else { return };
    let open = layer.overlay.ui().get_fortress_tab() == DEBUG_TAB;
    let checked: Vec<bool> = DevToggle::ALL.iter().map(|t| toggle_checked(*t, world)).collect();
    let elapsed = since.map_or(f32::INFINITY, |s| s + world.resource::<Time>().delta_secs());
    let stats = (open && elapsed >= DEV_STATS_INTERVAL_S).then(|| {
        let diagnostics = world.resource::<DiagnosticsStore>();
        let fps = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed());
        let frame_ms = diagnostics.get(&FrameTimeDiagnosticsPlugin::FRAME_TIME).and_then(|d| d.smoothed());
        let cameras = cameras.get_or_insert_with(|| world.query_filtered());
        let pos = cameras.iter(world).next().map(|t| t.translation());
        let entities = all.get_or_insert_with(|| world.query()).iter(world).count();
        let calendar = world.get_resource::<super::history_view::HistoryView>().and_then(|h| h.calendar);
        dev_stats(fps, frame_ms, pos, world.resource(), entities, calendar)
    });
    *since = if stats.is_some() { Some(0.0) } else { Some(elapsed.min(DEV_STATS_INTERVAL_S)) };

    let Some(layer) = world.get_non_send::<SlintLayer<GameUi>>() else { return };
    let ui = layer.overlay.ui();
    // A new session's game UI starts with no toggles or tick rows.
    if ui.get_dev_toggles().row_count() == 0 {
        shown.clear();
    }
    if ui.get_dev_tick_rows().row_count() == 0 {
        shown_ticks.clear();
    }
    let ticks = world
        .get_resource::<RegionTickTimes>()
        .map(|t| tick_rows(&t.0, world.resource::<HudStatus>().home_region))
        .unwrap_or_default();
    if *shown_ticks != ticks {
        let rows: Vec<DevTickRow> = ticks
            .iter()
            .map(|[region, mean, p50, p95, p99, max]| DevTickRow {
                region: region.into(),
                mean: mean.into(),
                p50: p50.into(),
                p95: p95.into(),
                p99: p99.into(),
                max: max.into(),
            })
            .collect();
        ui.set_dev_tick_rows(ModelRc::new(VecModel::from(rows)));
        *shown_ticks = ticks;
    }
    if *shown != checked {
        let toggles: Vec<slint_ui::DevToggle> = DevToggle::ALL
            .iter()
            .zip(&checked)
            .map(|(t, on)| slint_ui::DevToggle { label: t.label().into(), checked: *on })
            .collect();
        ui.set_dev_toggles(ModelRc::new(VecModel::from(toggles)));
        *shown = checked;
    }
    if let Some(rows) = stats {
        let rows: Vec<DevStat> =
            rows.into_iter().map(|(label, value)| DevStat { label: label.into(), value: value.into() }).collect();
        ui.set_dev_stats(ModelRc::new(VecModel::from(rows)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_blocks_the_game_only_to_free_a_grabbed_cursor() {
        assert!(!panel_claims_cursor(true, false), "commander view: the game keeps running its input");
        assert!(panel_claims_cursor(true, true), "first person: the panel frees the cursor");
        assert!(!panel_claims_cursor(false, true), "closed");
    }

    fn row<'a>(rows: &'a [(String, String)], label: &str) -> &'a str {
        &rows.iter().find(|(l, _)| l == label).expect(label).1
    }

    #[test]
    fn stats_rows_match_the_old_panel() {
        let rows = dev_stats(Some(59.6), Some(16.6667), Some(Vec3::new(1.0, 2.25, -3.0)), &HudStatus::default(), 42, Some(game::history::CalendarTick::from_days(0)));
        let labels: Vec<&str> = rows.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            ["FPS", "Frame", "Position", "Home region", "Viewer region", "Connection", "Entities", "Date"]
        );
        assert_eq!(row(&rows, "FPS"), "60");
        assert_eq!(row(&rows, "Frame"), "16.67 ms");
        assert_eq!(row(&rows, "Position"), format!("1.0 / {:.1} / -3.0", 2.25f32));
        assert_eq!(row(&rows, "Home region"), "--");
        assert_eq!(row(&rows, "Entities"), "42");
        assert_eq!(row(&rows, "Date"), "1 Granite 0");
        assert_eq!(row(&dev_stats(None, None, None, &HudStatus::default(), 0, None), "Date"), "--");

        let empty = dev_stats(None, None, None, &HudStatus::default(), 0, None);
        assert_eq!(row(&empty, "FPS"), "--");
        assert_eq!(row(&empty, "Frame"), "--");
        assert_eq!(row(&empty, "Position"), "--");
    }

    #[test]
    fn toggles_keep_the_old_order_and_labels() {
        assert_eq!(
            DevToggle::ALL.map(DevToggle::label),
            [
                "Region boundaries (F8)",
                "Chunk grid (F5)",
                "Hide LOD (F6)",
                "Hide near terrain (F7)",
                "Collider wireframes",
            ]
        );
    }

    fn world() -> (World, crossbeam::channel::Receiver<game::GameEventKind>) {
        let (send, recv) = crossbeam::channel::unbounded();
        let mut world = World::new();
        world.insert_resource(BoundaryOverlay { enabled: false });
        world.insert_resource(GridDebug::default());
        world.insert_resource(TerrainDebug::default());
        world.insert_resource(DebugFlagsRes(game::DebugFlags::default()));
        world.insert_resource(GameEvents(send));
        world.insert_resource(LocalPlayer(None));
        (world, recv)
    }

    #[test]
    fn toggle_actions_write_their_resource() {
        for toggle in DevToggle::ALL {
            let (mut world, _recv) = world();
            apply_dev_action(DevAction::Toggle(toggle, true), &mut world);
            let on: Vec<bool> = DevToggle::ALL.iter().map(|t| toggle_checked(*t, &world)).collect();
            let expected: Vec<bool> = DevToggle::ALL.iter().map(|t| *t == toggle).collect();
            assert_eq!(on, expected, "{toggle:?}");
        }
    }

    #[test]
    fn complete_orders_needs_a_local_player() {
        let (mut world, recv) = world();
        apply_dev_action(DevAction::CompleteOrders, &mut world);
        assert!(recv.try_recv().is_err());
        world.insert_resource(LocalPlayer(Some(7)));
        apply_dev_action(DevAction::CompleteOrders, &mut world);
        assert!(matches!(
            recv.try_recv(),
            Ok(game::GameEventKind::Order(7, game::Order::DevCompleteAll))
        ));
    }

    use super::super::commander::selection::GroundPick;

    #[test]
    fn drop_body_aims_at_the_last_ground_point_in_home_tiles() {
        let home = game::RegionCoords::new(0, 0);
        let b = game::BLOCK_UNITS as f32;
        // Region-local ground point → the home region's tile under it.
        let at = drop_body_order(Some((home, Vec3::new(3.5 * b, b, 9.25 * b))), Some(home));
        assert_eq!(at, Some(game::Order::DevDropBody { at: [3, 1, 9] }));
        // A point in the west neighbour is in home's frame (the sim refuses it).
        let west = game::RegionCoords::new(-1, 0);
        let at = drop_body_order(Some((west, Vec3::new(2.5 * b, b, 1.5 * b))), Some(home));
        assert_eq!(at, Some(game::Order::DevDropBody { at: [2 - game::REGION_BLOCKS as i32, 1, 1] }));
        assert_eq!(drop_body_order(None, Some(home)), None, "no ground seen");
        assert_eq!(drop_body_order(Some((home, Vec3::ZERO)), None), None, "no home yet");
    }

    #[test]
    fn drop_body_sends_the_order_at_the_remembered_point() {
        let (mut world, recv) = world();
        let home = game::RegionCoords::new(0, 0);
        world.insert_resource(HudStatus { home_region: Some(home), ..Default::default() });
        // The top of tile level 0, in the third tile along x and z.
        world.insert_resource(DevDropTarget(Some((home, Vec3::new(40.0, game::tile_height_units(), 40.0)))));
        apply_dev_action(DevAction::DropBody, &mut world);
        assert!(recv.try_recv().is_err(), "no local player: nothing sent");
        world.insert_resource(LocalPlayer(Some(7)));
        apply_dev_action(DevAction::DropBody, &mut world);
        assert!(matches!(
            recv.try_recv(),
            Ok(game::GameEventKind::Order(7, game::Order::DevDropBody { at: [2, 1, 2] }))
        ));
    }

    #[test]
    fn the_drop_target_follows_the_ground_pick_off_the_panel_only() {
        let mut app = App::new();
        app.insert_resource(CursorPolicy::default())
            .insert_resource(GroundPick(Some((game::RegionCoords::new(0, 0), Vec3::ONE)), true))
            .init_resource::<DevDropTarget>()
            .add_systems(Update, remember_drop_target);
        app.update();
        assert_eq!(app.world().resource::<DevDropTarget>().0.map(|p| p.1), Some(Vec3::ONE));
        // Over the panel: the button's click must not aim at the ground behind it.
        app.world_mut().resource_mut::<CursorPolicy>().ui_pointer = true;
        app.world_mut().resource_mut::<GroundPick>().0 = Some((game::RegionCoords::new(0, 0), Vec3::ZERO));
        app.update();
        assert_eq!(app.world().resource::<DevDropTarget>().0.map(|p| p.1), Some(Vec3::ONE));
    }

    #[test]
    fn tick_rows_in_ms_with_home_marked() {
        let stats = game::perf::TickTimeStats { samples: 3, mean_us: 1_234, p50_us: 1_000, p95_us: 2_500, p99_us: 3_000, max_us: 12_345 };
        let (home, other) = (game::RegionCoords::new(0, 0), game::RegionCoords::new(-1, 2));
        let rows = tick_rows(&[(other, stats), (home, stats)], Some(home));
        assert_eq!(rows[0], ["(-1, 2)", "1.23", "1.00", "2.50", "3.00", "12.35"].map(String::from));
        assert_eq!(rows[1][0], "(0, 0)*");
    }

    #[test]
    fn f3_opens_the_debug_tab_and_collapses_it() {
        assert_eq!(f3_tab(NO_TAB), DEBUG_TAB);
        assert_eq!(f3_tab(1), DEBUG_TAB, "from another tab, F3 switches to Debug");
        assert_eq!(f3_tab(DEBUG_TAB), NO_TAB);
    }
}
