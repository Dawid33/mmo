//! F3 developer panel, drawn with egui (`dev_ui` feature). Dev tooling only:
//! player-facing UI is the Slint game UI (`slint_overlay`), plus `bevy_ui`
//! for the engine-projected widget tree (`ui_bridge`) and the HUD (`hud`).
//! See docs/superpowers/specs/2026-09-22-ui-state-and-options.md.
//!
//! The panel frees the cursor through `CursorPolicy::overlay_open`, which also
//! stops `forward_input` sending clicks and mouse motion to the sim while it
//! is open.
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::ui::IsDefaultUiCamera;
use bevy_egui::{
    egui, EguiContext, EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass,
    EguiSchedule, PrimaryEguiContext,
};

use super::collider_debug::DebugFlagsRes;
use super::config::TerrainRenderConfig;
use super::cursor::CursorPolicy;
use super::grid_debug::{GridDebug, TerrainDebug};
use super::hud::HudStatus;
use super::meshing::visibility::CullingConfig;

/// Opens and closes the panel.
pub const DEV_UI_KEY: KeyCode = KeyCode::F3;

#[derive(Resource, Default)]
pub struct DevUiOpen(pub bool);

pub struct DevUiPlugin;

impl Plugin for DevUiPlugin {
    fn build(&self, app: &mut App) {
        // The primary context follows the local player's camera (see
        // `follow_ui_camera`), not whichever camera happens to spawn first.
        app.insert_resource(EguiGlobalSettings {
            auto_create_primary_context: false,
            ..default()
        })
        .add_plugins(EguiPlugin::default())
        .init_resource::<DevUiOpen>()
        .add_systems(Update, (follow_ui_camera, toggle_dev_ui))
        .add_systems(EguiPrimaryContextPass, draw_dev_ui);
    }
}

/// Keep the egui primary context on the camera marked `IsDefaultUiCamera`.
/// The bridge moves that marker when the active player camera changes (a
/// region flip spawns a new one), so the context moves with it.
fn follow_ui_camera(
    mut commands: Commands,
    ui_camera: Query<Entity, (With<IsDefaultUiCamera>, Without<PrimaryEguiContext>)>,
    current: Query<Entity, With<PrimaryEguiContext>>,
) {
    let Some(camera) = ui_camera.iter().next() else {
        return;
    };
    for old in &current {
        commands
            .entity(old)
            .remove::<(PrimaryEguiContext, EguiContext, EguiSchedule)>();
    }
    commands.entity(camera).insert(PrimaryEguiContext);
}

fn toggle_dev_ui(
    keys: Res<ButtonInput<KeyCode>>,
    mut open: ResMut<DevUiOpen>,
    mut cursor: ResMut<CursorPolicy>,
) {
    if keys.just_pressed(DEV_UI_KEY) {
        open.0 = !open.0;
    }
    if cursor.overlay_open != open.0 {
        cursor.overlay_open = open.0;
    }
}

#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn draw_dev_ui(
    mut contexts: EguiContexts,
    open: Res<DevUiOpen>,
    diagnostics: Res<DiagnosticsStore>,
    status: Res<HudStatus>,
    camera: Query<&GlobalTransform, With<IsDefaultUiCamera>>,
    terrain: Res<TerrainRenderConfig>,
    mut grid: ResMut<GridDebug>,
    mut boundaries: ResMut<super::boundaries::BoundaryOverlay>,
    mut culling: ResMut<CullingConfig>,
    mut terrain_debug: ResMut<TerrainDebug>,
    debug_flags: Res<DebugFlagsRes>,
    entities: Query<()>,
) -> Result {
    if !open.0 {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed());
    let frame_ms = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FRAME_TIME)
        .and_then(|d| d.smoothed());
    let pos = camera.iter().next().map(|t| t.translation());

    egui::Window::new("Dev")
        .anchor(egui::Align2::RIGHT_TOP, [-8.0, 8.0])
        .resizable(false)
        .show(ctx, |ui| {
            egui::Grid::new("dev_stats").num_columns(2).show(ui, |ui| {
                ui.label("FPS");
                ui.label(fps.map_or("--".into(), |f| format!("{f:.0}")));
                ui.end_row();
                ui.label("Frame");
                ui.label(frame_ms.map_or("--".into(), |ms| format!("{ms:.2} ms")));
                ui.end_row();
                ui.label("Position");
                ui.label(pos.map_or("--".into(), |p| {
                    format!("{:.1} / {:.1} / {:.1}", p.x, p.y, p.z)
                }));
                ui.end_row();
                ui.label("Home region");
                ui.label(status.home_region.map_or("--".into(), |r| format!("({}, {})", r.x, r.z)));
                ui.end_row();
                ui.label("Viewer region");
                ui.label(
                    status
                        .viewer_region
                        .map_or("--".into(), |r| format!("({}, {})", r.x, r.z)),
                );
                ui.end_row();
                ui.label("Connection");
                ui.label(format!("{:?}", status.connection));
                ui.end_row();
                ui.label("Terrain");
                ui.label(format!("near {:?}, far {:?}", terrain.near, terrain.far));
                ui.end_row();
                ui.label("Entities");
                ui.label(entities.iter().count().to_string());
                ui.end_row();
            });
            ui.separator();
            ui.checkbox(&mut boundaries.enabled, "Region boundaries (F8)");
            ui.checkbox(&mut grid.enabled, "Chunk grid (F5)");
            ui.checkbox(&mut culling.graph_enabled, "Cave culling (F4)");
            ui.checkbox(&mut terrain_debug.hide_lod, "Hide LOD (F6)");
            ui.checkbox(&mut terrain_debug.hide_near, "Hide near terrain (F7)");
            // Shared with the game thread, which only builds lines while on.
            let mut colliders = debug_flags.0.collider_wireframes();
            if ui.checkbox(&mut colliders, "Collider wireframes").changed() {
                debug_flags.0.set_collider_wireframes(colliders);
            }
        });
    Ok(())
}
