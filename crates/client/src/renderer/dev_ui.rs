//! F3 developer panel (`dev_ui` feature) — dev tooling, never player UI: a
//! Slint layer (`slint_ui::DevPanelUi`, crates/slint-ui/ui/dev-panel.slint)
//! above the game UI, in both camera modes. Stats, the debug toggles (the
//! same flags as F4–F8) and stand-ins for systems that don't exist yet.
//!
//! The panel does not pause the game: in the commander view the pointer is
//! free anyway, so the panel only takes it where it drew (`ui_pointer`, like
//! any Slint layer) and panning, zoom, tools and selection keep working. Only
//! when the sim holds the cursor (first person) does it set
//! `CursorPolicy::overlay_open` ([`panel_claims_cursor`]), which frees the
//! cursor and stops `forward_input` sending clicks and mouse motion to the sim.
use std::cell::RefCell;
use std::rc::Rc;

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use slint_ui::slint::{ModelRc, VecModel};
use slint_ui::{DevPanelUi, DevStat};

use super::boundaries::BoundaryOverlay;
use super::collider_debug::DebugFlagsRes;
use super::config::TerrainRenderConfig;
use super::cursor::CursorPolicy;
use super::grid_debug::{GridDebug, TerrainDebug};
use super::hud::HudStatus;
use super::meshing::visibility::CullingConfig;
use super::slint_overlay::{SlintLayer, SlintLayerPlugin, DEV_LAYER_ORDER};
use super::voxel_pull::xray::XraySettings;
use super::{GameEvents, LocalPlayer, UiCamera};

/// Opens and closes the panel.
pub const DEV_UI_KEY: KeyCode = KeyCode::F3;
/// How often the stats refresh while the panel is open.
pub const DEV_STATS_INTERVAL_S: f32 = 0.25;

#[derive(Resource, Default)]
pub struct DevUiOpen(pub bool);

/// The panel's debug toggles, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevToggle {
    Boundaries,
    ChunkGrid,
    CaveCulling,
    HideLod,
    HideNear,
    Colliders,
    /// The cut-off view's dug-space x-ray (`voxel_pull::xray`), for comparison.
    Xray,
}

impl DevToggle {
    pub const ALL: [DevToggle; 7] = [
        DevToggle::Boundaries,
        DevToggle::ChunkGrid,
        DevToggle::CaveCulling,
        DevToggle::HideLod,
        DevToggle::HideNear,
        DevToggle::Colliders,
        DevToggle::Xray,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DevToggle::Boundaries => "Region boundaries (F8)",
            DevToggle::ChunkGrid => "Chunk grid (F5)",
            DevToggle::CaveCulling => "Cave culling (F4)",
            DevToggle::HideLod => "Hide LOD (F6)",
            DevToggle::HideNear => "Hide near terrain (F7)",
            DevToggle::Colliders => "Collider wireframes",
            DevToggle::Xray => "Dug-space x-ray (cut off)",
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
}

/// Filled by the panel's callbacks during event dispatch (`!Send`).
struct DevActions(Rc<RefCell<Vec<DevAction>>>);

pub struct DevUiPlugin;

impl Plugin for DevUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DevUiOpen>()
            // Hidden under the menu screens: it is the world's dev panel.
            // Its toggles are preferences, so it is not session-scoped.
            .add_plugins(
                SlintLayerPlugin::new(DevPanelUi::new)
                    .order(DEV_LAYER_ORDER)
                    .hidden_by_frontend()
                    .with_init(connect_dev_panel),
            )
            .add_systems(
                Update,
                (toggle_dev_ui, apply_dev_actions, push_dev_panel)
                    .chain()
                    .run_if(crate::session::world_systems_enabled()),
            );
    }
}

fn connect_dev_panel(ui: &DevPanelUi, world: &mut World) {
    let actions = Rc::new(RefCell::new(Vec::new()));
    let queue = actions.clone();
    ui.on_toggle(move |i, on| {
        if let Some(toggle) = DevToggle::ALL.get(i as usize) {
            queue.borrow_mut().push(DevAction::Toggle(*toggle, on));
        }
    });
    let queue = actions.clone();
    ui.on_complete_orders(move || queue.borrow_mut().push(DevAction::CompleteOrders));
    world.insert_non_send(DevActions(actions));
}

/// Whether the open panel must claim the cursor (and with it the game's
/// input): only to free a cursor the sim has grabbed.
pub fn panel_claims_cursor(open: bool, sim_grab: bool) -> bool {
    open && sim_grab
}

fn toggle_dev_ui(keys: Res<ButtonInput<KeyCode>>, mut open: ResMut<DevUiOpen>, mut cursor: ResMut<CursorPolicy>) {
    if keys.just_pressed(DEV_UI_KEY) {
        open.0 = !open.0;
    }
    let claim = panel_claims_cursor(open.0, cursor.sim_grab);
    if cursor.overlay_open != claim {
        cursor.overlay_open = claim;
    }
}

/// The flag a toggle shows and flips.
pub fn toggle_checked(toggle: DevToggle, world: &World) -> bool {
    match toggle {
        DevToggle::Boundaries => world.resource::<BoundaryOverlay>().enabled,
        DevToggle::ChunkGrid => world.resource::<GridDebug>().enabled,
        DevToggle::CaveCulling => world.resource::<CullingConfig>().graph_enabled,
        DevToggle::HideLod => world.resource::<TerrainDebug>().hide_lod,
        DevToggle::HideNear => world.resource::<TerrainDebug>().hide_near,
        // Shared with the game thread, which only builds lines while on.
        DevToggle::Colliders => world.resource::<DebugFlagsRes>().0.collider_wireframes(),
        DevToggle::Xray => world.get_resource::<XraySettings>().is_some_and(|x| x.enabled),
    }
}

pub fn apply_dev_action(action: DevAction, world: &mut World) {
    match action {
        DevAction::Toggle(toggle, on) => match toggle {
            DevToggle::Boundaries => world.resource_mut::<BoundaryOverlay>().enabled = on,
            DevToggle::ChunkGrid => world.resource_mut::<GridDebug>().enabled = on,
            DevToggle::CaveCulling => world.resource_mut::<CullingConfig>().graph_enabled = on,
            DevToggle::HideLod => world.resource_mut::<TerrainDebug>().hide_lod = on,
            DevToggle::HideNear => world.resource_mut::<TerrainDebug>().hide_near = on,
            DevToggle::Colliders => world.resource::<DebugFlagsRes>().0.set_collider_wireframes(on),
            DevToggle::Xray => world.insert_resource(XraySettings { enabled: on }),
        },
        DevAction::CompleteOrders => {
            if let Some(client) = world.resource::<LocalPlayer>().0 {
                let order = game::GameEventKind::Order(client, game::Order::DevCompleteAll);
                let _ = world.resource::<GameEvents>().0.send(order);
            }
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
    terrain: &TerrainRenderConfig,
    entities: usize,
) -> Vec<(String, String)> {
    let region = |r: Option<game::RegionCoords>| r.map_or("--".into(), |r| format!("({}, {})", r.x, r.z));
    vec![
        ("FPS".into(), fps.map_or("--".into(), |f| format!("{f:.0}"))),
        ("Frame".into(), frame_ms.map_or("--".into(), |ms| format!("{ms:.2} ms"))),
        ("Position".into(), pos.map_or("--".into(), |p| format!("{:.1} / {:.1} / {:.1}", p.x, p.y, p.z))),
        ("Home region".into(), region(status.home_region)),
        ("Viewer region".into(), region(status.viewer_region)),
        ("Connection".into(), format!("{:?}", status.connection)),
        ("Terrain".into(), format!("near {:?}, far {:?}", terrain.near, terrain.far)),
        ("Entities".into(), entities.to_string()),
    ]
}

/// Client state → panel properties: open/closed, the toggles when a flag
/// changed, the stats every [`DEV_STATS_INTERVAL_S`] while open. Models are
/// always pushed fresh, which also resets a checkbox the user just clicked.
fn push_dev_panel(
    world: &mut World,
    mut since: Local<Option<f32>>,
    mut shown: Local<Vec<bool>>,
    mut cameras: Local<Option<QueryState<&GlobalTransform, With<UiCamera>>>>,
    mut all: Local<Option<QueryState<()>>>,
) {
    let open = world.resource::<DevUiOpen>().0;
    let checked: Vec<bool> = DevToggle::ALL.iter().map(|t| toggle_checked(*t, world)).collect();
    let elapsed = since.map_or(f32::INFINITY, |s| s + world.resource::<Time>().delta_secs());
    let stats = (open && elapsed >= DEV_STATS_INTERVAL_S).then(|| {
        let diagnostics = world.resource::<DiagnosticsStore>();
        let fps = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed());
        let frame_ms = diagnostics.get(&FrameTimeDiagnosticsPlugin::FRAME_TIME).and_then(|d| d.smoothed());
        let cameras = cameras.get_or_insert_with(|| world.query_filtered());
        let pos = cameras.iter(world).next().map(|t| t.translation());
        let entities = all.get_or_insert_with(|| world.query()).iter(world).count();
        dev_stats(fps, frame_ms, pos, world.resource(), world.resource(), entities)
    });
    *since = if stats.is_some() { Some(0.0) } else { Some(elapsed.min(DEV_STATS_INTERVAL_S)) };

    let Some(layer) = world.get_non_send::<SlintLayer<DevPanelUi>>() else { return };
    let ui = layer.overlay.ui();
    if ui.get_open() != open {
        ui.set_open(open);
    }
    if *shown != checked {
        let toggles: Vec<slint_ui::DevToggle> = DevToggle::ALL
            .iter()
            .zip(&checked)
            .map(|(t, on)| slint_ui::DevToggle { label: t.label().into(), checked: *on })
            .collect();
        ui.set_toggles(ModelRc::new(VecModel::from(toggles)));
        *shown = checked;
    }
    if let Some(rows) = stats {
        let rows: Vec<DevStat> =
            rows.into_iter().map(|(label, value)| DevStat { label: label.into(), value: value.into() }).collect();
        ui.set_stats(ModelRc::new(VecModel::from(rows)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::slint_overlay::{GAME_LAYER_ORDER, HUD_LAYER_ORDER};

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
        let cfg = TerrainRenderConfig::default();
        let rows = dev_stats(Some(59.6), Some(16.6667), Some(Vec3::new(1.0, 2.25, -3.0)), &HudStatus::default(), &cfg, 42);
        let labels: Vec<&str> = rows.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            ["FPS", "Frame", "Position", "Home region", "Viewer region", "Connection", "Terrain", "Entities"]
        );
        assert_eq!(row(&rows, "FPS"), "60");
        assert_eq!(row(&rows, "Frame"), "16.67 ms");
        assert_eq!(row(&rows, "Position"), format!("1.0 / {:.1} / -3.0", 2.25f32));
        assert_eq!(row(&rows, "Home region"), "--");
        assert_eq!(row(&rows, "Terrain"), format!("near {:?}, far {:?}", cfg.near, cfg.far));
        assert_eq!(row(&rows, "Entities"), "42");

        let empty = dev_stats(None, None, None, &HudStatus::default(), &cfg, 0);
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
                "Cave culling (F4)",
                "Hide LOD (F6)",
                "Hide near terrain (F7)",
                "Collider wireframes",
                "Dug-space x-ray (cut off)",
            ]
        );
    }

    fn world() -> (World, crossbeam::channel::Receiver<game::GameEventKind>) {
        let (send, recv) = crossbeam::channel::unbounded();
        let mut world = World::new();
        world.insert_resource(BoundaryOverlay { enabled: false });
        world.insert_resource(GridDebug::default());
        world.insert_resource(CullingConfig { graph_enabled: false });
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

    #[test]
    fn dev_layer_is_on_top() {
        assert!(DEV_LAYER_ORDER > GAME_LAYER_ORDER && GAME_LAYER_ORDER > HUD_LAYER_ORDER);
    }
}
