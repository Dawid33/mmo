use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use crossbeam::channel::{Receiver, Sender};
use game::{ClientId, ClientUpdateEvent, GameEventKind};

use crate::session::{world_systems_enabled, SessionResetExt};

pub mod atmosphere;
#[cfg(not(target_arch = "wasm32"))]
mod autopilot;
mod avatar;
mod block_textures;
pub mod bricks;
pub mod bridge;
pub mod chat;
pub(crate) mod collider_debug;
pub mod commander;
pub(crate) mod cursor;
#[cfg(feature = "dev_ui")]
pub mod dev_ui;
pub(crate) mod boundaries;
pub mod fortress_panel;
pub mod fortress_view;
pub(crate) mod slice_plane;
pub mod compute; // off-main-thread jobs: Bevy's pool natively, Web Workers on wasm
pub mod config;
pub mod convert;
pub(crate) mod diag;
pub mod drain_notice;
pub(crate) mod grid_debug;
pub mod hud;
pub(crate) mod input;
mod interpolate;
mod look;
mod motion_probe;
pub mod crossing_timeline;
pub mod join_timeline;
pub mod item_icons;
pub mod item_flights;
pub mod item_piles;
pub mod lod_rings;
pub mod map; // the world map's tiles: one pyramid behind the minimap, the full map and the site choice
pub mod meshing;
pub mod model;
#[cfg(not(target_arch = "wasm32"))]
pub mod render_bench;
pub mod rigs;
mod skybox;
pub mod slint_composite;
pub mod slint_overlay;
pub mod text_entry;
pub use slint_composite::UiCamera;
pub mod voxel_bodies;
mod voxel_material;
pub mod voxel_pull;
pub mod water;
pub mod arrival_tabs;
pub mod history_view;
pub mod world_map;
use block_textures::build_layers;
use voxel_material::StandardVoxelMaterial;

/// `RenderPlugin` with Bevy's default (env-driven) `WgpuSettings`, minus the
/// adapter features that break device creation. Bevy requests every feature
/// the adapter advertises; Mesa's NVK driver (this box's T600) advertises
/// `EXPERIMENTAL_COOPERATIVE_MATRIX` under wgpu 29 but then fails
/// `vkCreateDevice` with it, so an unpinned launch would panic instead of
/// running. Nothing here uses cooperative matrices.
pub fn render_plugin() -> bevy::render::RenderPlugin {
    use bevy::render::settings::{RenderCreation, WgpuFeatures, WgpuSettings};
    bevy::render::RenderPlugin {
        render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
            disabled_features: Some(WgpuFeatures::EXPERIMENTAL_COOPERATIVE_MATRIX),
            ..Default::default()
        })),
        ..Default::default()
    }
}

#[derive(Resource)]
pub struct ClientUpdates(pub Receiver<ClientUpdateEvent>);

#[derive(Resource)]
pub struct GameEvents(pub Sender<GameEventKind>);

/// UI → game thread: requests for the server (`ClientRequest::PlayRegion`).
#[derive(Resource)]
pub struct ClientRequests(pub Sender<crate::instance::ClientRequest>);

#[derive(Resource, Default)]
pub struct LocalPlayer(pub Option<ClientId>);

pub struct SimBridgePlugin {
    pub client_recv: Receiver<ClientUpdateEvent>,
    pub game_send: Sender<GameEventKind>,
    /// Requests for the server, read by the game thread's manager.
    pub requests: Sender<crate::instance::ClientRequest>,
    pub debug_flags: game::DebugFlags,
    /// Commander (the game) or the legacy first-person body view.
    pub camera_mode: commander::CameraMode,
}

impl Plugin for SimBridgePlugin {
    fn build(&self, app: &mut App) {
        voxel_material::embed_voxel_texture_shader(app);
        app.add_plugins(water::WaterPlugin);
        app.add_plugins((
            MaterialPlugin::<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>::default(),
            bevy::diagnostic::FrameTimeDiagnosticsPlugin::default(),
            voxel_pull::VoxelPullPlugin,
            fortress_view::FortressOverlayPlugin,
            model::BbModelPlugin,
            item_piles::ItemPilesPlugin,
            map::MapPlugin,
        ))
        .insert_resource(ClientUpdates(self.client_recv.clone()))
        .insert_resource(GameEvents(self.game_send.clone()))
        .insert_resource(ClientRequests(self.requests.clone()))
        // Per-session state goes back to its fresh value in `end_session`
        // (`add_session_reset`); preferences, startup assets and render
        // config are plain `init_resource` and survive it.
        .add_session_reset::<world_map::WorldMapState>()
        .add_session_reset::<world_map::PendingPlay>()
        .add_message::<world_map::WorldMapMsg>()
        .add_session_message_clear::<world_map::WorldMapMsg>()
        .add_session_reset::<history_view::HistoryView>()
        .add_session_reset::<drain_notice::DrainNotice>()
        // The unwrapped frame the bridge keeps (`ClientUpdateEvent::WrapFrame`):
        // a new session starts at the canonical frame.
        .add_session_reset::<crate::wrap_frame::WrapFrame>()
        .add_message::<history_view::HistoryMsg>()
        .add_session_message_clear::<history_view::HistoryMsg>()
        .add_session_reset::<LocalPlayer>()
        .add_session_reset::<bridge::Regions>()
        .add_session_reset::<bridge::RegionRoots>()
        .add_session_reset::<bridge::SimEntityMap>()
        .add_session_reset::<bridge::SnapshotVoxelQueue>()
        .add_session_reset::<bridge::RetiredRegionRoots>()
        .add_session_reset::<hud::HudStatus>()
        .add_session_reset::<hud::RegionTickTimes>()
        .init_resource::<boundaries::BoundaryOverlay>()
        .add_session_reset::<boundaries::BoundaryUniform>()
        .add_session_reset::<fortress_view::FortressViewUniform>()
        .init_resource::<avatar::AvatarAssets>()
        .add_session_reset::<slint_overlay::UiUpdates>()
        // `drain_ui_updates` (below) runs in both camera modes and fills these;
        // the overlay that shows them exists only in commander mode.
        .add_session_reset::<slint_overlay::HudItems>()
        .insert_resource(collider_debug::DebugFlagsRes(self.debug_flags.clone()))
        .add_session_reset::<collider_debug::DebugColliderLines>()
        .init_resource::<grid_debug::GridDebug>()
        .init_resource::<cursor::CursorPolicy>()
        .add_session_reset_with(cursor::reset_session_grab)
        .add_session_reset::<text_entry::TextEntry>()
        .add_session_reset::<meshing::queue::ChunkGrid>()
        .add_session_reset::<meshing::queue::RemeshQueue>()
        .init_resource::<meshing::queue::MeshBudgets>()
        .init_resource::<meshing::queue::MeshBudgetPolicy>()
        .add_session_reset::<meshing::queue::FrameUploads>()
        .add_session_reset::<meshing::lod::LodBuildQueue>()
        .add_session_reset::<meshing::lod::FarFrameUploads>()
        .add_session_reset::<bricks::SkippedBricks>()
        .add_session_reset::<bricks::SimChunkIndex>()
        // Cave culling starts OFF: the world has no caves yet, and the flood
        // cost 5-8.5 ms per turning frame at the 5x5 window (2026-09-23
        // bench) for nothing to hide. F4 / the F3 dev panel turn it on.
        .insert_resource(meshing::visibility::CullingConfig { graph_enabled: false })
        .init_resource::<config::TerrainRenderConfig>()
        .init_resource::<input::MouseCoalesceWindow>()
        .add_session_reset::<look::LocalLook>()
        .add_session_reset::<lod_rings::LodRings>()
        .init_resource::<grid_debug::TerrainDebug>()
        .add_session_reset::<join_timeline::WorldReady>()
        .insert_resource(self.camera_mode)
        .add_systems(
            PreUpdate,
            (
                // Before anything reads the keys: while a text field has the
                // keyboard there are none, and the frame it takes it the sim
                // is sent the key-ups of what was held.
                text_entry::mute_keys_while_typing.after(bevy::input::InputSystems),
                input::forward_input
                    .after(bevy::input::InputSystems)
                    .run_if(world_systems_enabled()),
                look::integrate_local_look
                    .after(bevy::input::InputSystems)
                    .run_if(world_systems_enabled()),
                (
                    (
                        bridge::drain_client_updates,
                        bridge::despawn_retired_regions,
                        bridge::drain_region_updates,
                        bridge::apply_snapshot_voxels,
                        bridge::dedupe_halo_entities,
                    )
                        .chain()
                        .run_if(world_systems_enabled()),
                    // Ungated: they also hand the screen and the pointer to
                    // the frontend (menu) when no world is running.
                    bridge::enforce_single_active_camera,
                    slint_overlay::drain_ui_updates.run_if(world_systems_enabled()),
                    cursor::retake_pointer,
                    cursor::apply_cursor_policy,
                )
                    .chain(),
            )
                .chain(),
        )
        .add_plugins(meshing::visibility::cull_plugin)
        .add_systems(Startup, (setup_scene, skybox::setup_skybox))
        .add_systems(
            Update,
            (
                (
                    // Rate-based budgets: re-derived from the real frame delta
                    // before any lane reads them.
                    meshing::queue::scale_mesh_budgets,
                    meshing::queue::mark_remesh_dirty,
                    meshing::queue::prune_chunk_grid,
                    meshing::queue::remesh_awaiting_ground,
                    meshing::queue::spawn_mesh_tasks,
                    meshing::apply_meshed_chunks,
                    meshing::chunk_mesh::rebuild_chunk_meshes,
                    // LOD meshes land in the same chain; culling moved to
                    // cull_plugin (PostUpdate) so it sees the current
                    // frame's frustum — it runs after this chain either way.
                    meshing::lod::spawn_lod_mesh_tasks,
                    meshing::lod::apply_lod_meshes,
                    meshing::lod_tiles::rebuild_lod_tiles,
                )
                    .chain(),
                // LOD rings run in Update but outside the meshing chain:
                // they only spawn/despawn LOD roots and children, which
                // the LOD mesh systems pick up on later frames.
                (
                    lod_rings::apply_lod_events,
                    lod_rings::retire_lod_roots,
                )
                    .chain(),
                interpolate::interpolate_transforms,
                (voxel_bodies::spawn_voxel_body_meshes, voxel_bodies::apply_voxel_body_meshes).chain(),
                // Coarse twins of sim chunks (distance LOD in the simulated
                // neighbourhood): dirtied by `rebuild_chunk_meshes`.
                (meshing::coarse::finish_band_handovers, meshing::coarse::apply_coarse_tasks, meshing::coarse::spawn_coarse_tasks)
                    .chain()
                    .after(meshing::chunk_mesh::rebuild_chunk_meshes),
                world_map::apply_world_map_msgs,
                history_view::apply_history_msgs,
                // Per-frame mouse look replaces the per-tick sim look that
                // interpolate_transforms just snapped the camera to.
                look::apply_local_look.after(interpolate::interpolate_transforms),
                // It measures the eye: before the third-person camera leaves it.
                motion_probe::probe_camera_motion
                    .after(look::apply_local_look)
                    .before(commander::first_person::place_third_person_camera),
                motion_probe::probe_still_frames.after(look::apply_local_look),
                join_timeline::track_join_timeline,
                avatar::attach_avatars,
                avatar::squish_slimes.after(interpolate::interpolate_transforms),
                meshing::visibility::toggle_culling,
                collider_debug::draw_collider_wireframes,
                (boundaries::toggle_boundary_overlay, boundaries::update_boundary_uniform).chain(),
                grid_debug::toggle_grid_debug,
                grid_debug::draw_grid_debug.after(grid_debug::toggle_grid_debug),
                (grid_debug::toggle_terrain_debug, grid_debug::apply_terrain_debug).chain(),
            )
                .run_if(world_systems_enabled()),
        );
        // Every camera, the frontend's included (which brings its own).
        app.add_systems(Update, (skybox::attach_skybox, atmosphere::attach_fog));
        // Far planes follow the fog's end (`ViewDistance`), before the frusta.
        app.add_systems(PostUpdate, atmosphere::sync_far_plane.before(bevy::camera::CameraUpdateSystems));
        app.add_systems(
            Update,
            motion_probe::probe_region_crossing
                .after(interpolate::interpolate_transforms)
                .run_if(world_systems_enabled()),
        );
        // One timeline per region crossing (`[perf] event=crossing_*`).
        app.add_systems(Update, crossing_timeline::track_crossing_timeline.run_if(world_systems_enabled()));
        // Sim-animated entities (`game::anim`): model child, then the pose
        // from the sim state over the interpolated body.
        app.add_systems(
            Update,
            (
                rigs::attach_rig_models.after(avatar::attach_avatars),
                rigs::detach_rig_models,
                rigs::pose_rig_models
                    .after(interpolate::interpolate_transforms)
                    .after(model::spawn_bbmodels),
            )
                .chain()
                .run_if(world_systems_enabled()),
        );
        app.add_plugins(slint_overlay::HudLayerPlugin);
        if self.camera_mode == commander::CameraMode::Commander {
            app.add_plugins((
                commander::CommanderPlugin,
                slint_overlay::SlintOverlayPlugin,
                chat::ChatPlugin,
                slice_plane::SlicePlanePlugin,
            ));
        }
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(Update, config::configure_native_antialiasing);
        #[cfg(not(target_arch = "wasm32"))]
        if meshing::visibility::native_occlusion_enabled() {
            app.add_systems(Update, meshing::visibility::enable_native_occlusion);
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(pilot) = autopilot::Autopilot::from_env() {
            autopilot::plugin(app, pilot);
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(benchmark) = render_bench::RenderBenchmarkPlugin::from_env() {
            app.add_plugins(benchmark);
        }

        // Opt-in (DIAG=1): per-render-pass GPU/CPU ms + pipeline stats, dumped to
        // stdout. What matters is that it lands after DefaultPlugins, since
        // RenderDiagnosticsPlugin reaches into the RenderApp sub-app.
        if diag::enabled() {
            app.add_plugins(diag::DiagPlugin);
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(secs) = diag::exit_after_from_env() {
            diag::exit_after_plugin(app, secs);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some((secs, path)) = diag::screenshot_from_env() {
            diag::screenshot_plugin(app, secs, path);
        }
    }
}

/// Resolve the blocks asset directory; must mirror AssetPlugin.file_path resolution in main.rs.
///
/// Base-path priority mirrors bevy_asset's `get_base_path` exactly (see
/// `bevy_asset::io::file::get_base_path`): `BEVY_ASSET_ROOT` env var, then
/// `CARGO_MANIFEST_DIR` (set by cargo at runtime), then the running executable's
/// parent directory. Each base is then joined with the same relative path
/// `AssetPlugin { file_path: "../../assets", .. }` uses in main.rs, plus our own
/// `blocks` subdir, so all three bases resolve consistently.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn resolve_blocks_dir() -> std::path::PathBuf {
    use std::path::PathBuf;

    let base = if let Ok(root) = std::env::var("BEVY_ASSET_ROOT") {
        PathBuf::from(root)
    } else if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        PathBuf::from(manifest_dir)
    } else if let Ok(exe_path) = std::env::current_exe() {
        match exe_path.parent() {
            Some(exe_dir) => exe_dir.to_path_buf(),
            // No parent (e.g. exe at filesystem root): fall back to cwd-relative.
            None => return PathBuf::from("assets/blocks"),
        }
    } else {
        // Final fallback: cwd-relative (original behavior).
        return PathBuf::from("assets/blocks");
    };

    base.join("../../assets/blocks")
}

/// Block textures for targets with no filesystem (wasm): embedded at compile
/// time; keep this list in sync with assets/blocks/*.png.
#[cfg(target_arch = "wasm32")]
const EMBEDDED_BLOCK_TEXTURES: &[(&str, &[u8])] = &[
    (
        "dirt.png",
        include_bytes!("../../../../assets/blocks/dirt.png"),
    ),
    (
        "grass_side.png",
        include_bytes!("../../../../assets/blocks/grass_side.png"),
    ),
    (
        "grass_top.png",
        include_bytes!("../../../../assets/blocks/grass_top.png"),
    ),
    (
        "marker.png",
        include_bytes!("../../../../assets/blocks/marker.png"),
    ),
    (
        "basalt.png",
        include_bytes!("../../../../assets/blocks/basalt.png"),
    ),
    (
        "bedrock.png",
        include_bytes!("../../../../assets/blocks/bedrock.png"),
    ),
    (
        "conglomerate.png",
        include_bytes!("../../../../assets/blocks/conglomerate.png"),
    ),
    (
        "granite.png",
        include_bytes!("../../../../assets/blocks/granite.png"),
    ),
    (
        "limestone.png",
        include_bytes!("../../../../assets/blocks/limestone.png"),
    ),
    (
        "sandstone.png",
        include_bytes!("../../../../assets/blocks/sandstone.png"),
    ),
    (
        "shale.png",
        include_bytes!("../../../../assets/blocks/shale.png"),
    ),
    (
        "oak_bark.png",
        include_bytes!("../../../../assets/blocks/oak_bark.png"),
    ),
    (
        "oak_rings.png",
        include_bytes!("../../../../assets/blocks/oak_rings.png"),
    ),
    (
        "oak_branch.png",
        include_bytes!("../../../../assets/blocks/oak_branch.png"),
    ),
    (
        "oak_leaves.png",
        include_bytes!("../../../../assets/blocks/oak_leaves.png"),
    ),
    (
        "pine_bark.png",
        include_bytes!("../../../../assets/blocks/pine_bark.png"),
    ),
    (
        "pine_rings.png",
        include_bytes!("../../../../assets/blocks/pine_rings.png"),
    ),
    (
        "pine_branch.png",
        include_bytes!("../../../../assets/blocks/pine_branch.png"),
    ),
    (
        "pine_leaves.png",
        include_bytes!("../../../../assets/blocks/pine_leaves.png"),
    ),
    (
        "birch_bark.png",
        include_bytes!("../../../../assets/blocks/birch_bark.png"),
    ),
    (
        "birch_rings.png",
        include_bytes!("../../../../assets/blocks/birch_rings.png"),
    ),
    (
        "birch_branch.png",
        include_bytes!("../../../../assets/blocks/birch_branch.png"),
    ),
    (
        "birch_leaves.png",
        include_bytes!("../../../../assets/blocks/birch_leaves.png"),
    ),
    (
        "sand.png",
        include_bytes!("../../../../assets/blocks/sand.png"),
    ),
    (
        "stone.png",
        include_bytes!("../../../../assets/blocks/stone.png"),
    ),
    (
        "water.png",
        include_bytes!("../../../../assets/blocks/water.png"),
    ),
    (
        "acacia_bark.png",
        include_bytes!("../../../../assets/blocks/acacia_bark.png"),
    ),
    (
        "acacia_branch.png",
        include_bytes!("../../../../assets/blocks/acacia_branch.png"),
    ),
    (
        "acacia_leaves.png",
        include_bytes!("../../../../assets/blocks/acacia_leaves.png"),
    ),
    (
        "acacia_rings.png",
        include_bytes!("../../../../assets/blocks/acacia_rings.png"),
    ),
    (
        "cactus.png",
        include_bytes!("../../../../assets/blocks/cactus.png"),
    ),
    (
        "chest_side.png",
        include_bytes!("../../../../assets/blocks/chest_side.png"),
    ),
    (
        "chest_top.png",
        include_bytes!("../../../../assets/blocks/chest_top.png"),
    ),
    (
        "dry_grass_side.png",
        include_bytes!("../../../../assets/blocks/dry_grass_side.png"),
    ),
    (
        "dry_grass_top.png",
        include_bytes!("../../../../assets/blocks/dry_grass_top.png"),
    ),
    (
        "frozen_dirt.png",
        include_bytes!("../../../../assets/blocks/frozen_dirt.png"),
    ),
    (
        "glacier_ice.png",
        include_bytes!("../../../../assets/blocks/glacier_ice.png"),
    ),
    (
        "gravel.png",
        include_bytes!("../../../../assets/blocks/gravel.png"),
    ),
    (
        "ice.png",
        include_bytes!("../../../../assets/blocks/ice.png"),
    ),
    (
        "jungle_bark.png",
        include_bytes!("../../../../assets/blocks/jungle_bark.png"),
    ),
    (
        "jungle_branch.png",
        include_bytes!("../../../../assets/blocks/jungle_branch.png"),
    ),
    (
        "jungle_leaves.png",
        include_bytes!("../../../../assets/blocks/jungle_leaves.png"),
    ),
    (
        "jungle_rings.png",
        include_bytes!("../../../../assets/blocks/jungle_rings.png"),
    ),
    (
        "mud.png",
        include_bytes!("../../../../assets/blocks/mud.png"),
    ),
    (
        "palm_bark.png",
        include_bytes!("../../../../assets/blocks/palm_bark.png"),
    ),
    (
        "palm_branch.png",
        include_bytes!("../../../../assets/blocks/palm_branch.png"),
    ),
    (
        "palm_leaves.png",
        include_bytes!("../../../../assets/blocks/palm_leaves.png"),
    ),
    (
        "palm_rings.png",
        include_bytes!("../../../../assets/blocks/palm_rings.png"),
    ),
    (
        "peat.png",
        include_bytes!("../../../../assets/blocks/peat.png"),
    ),
    (
        "podzol_side.png",
        include_bytes!("../../../../assets/blocks/podzol_side.png"),
    ),
    (
        "podzol_top.png",
        include_bytes!("../../../../assets/blocks/podzol_top.png"),
    ),
    (
        "red_sand.png",
        include_bytes!("../../../../assets/blocks/red_sand.png"),
    ),
    (
        "salt.png",
        include_bytes!("../../../../assets/blocks/salt.png"),
    ),
    (
        "snow.png",
        include_bytes!("../../../../assets/blocks/snow.png"),
    ),
    (
        "spruce_bark.png",
        include_bytes!("../../../../assets/blocks/spruce_bark.png"),
    ),
    (
        "spruce_branch.png",
        include_bytes!("../../../../assets/blocks/spruce_branch.png"),
    ),
    (
        "spruce_leaves.png",
        include_bytes!("../../../../assets/blocks/spruce_leaves.png"),
    ),
    (
        "spruce_rings.png",
        include_bytes!("../../../../assets/blocks/spruce_rings.png"),
    ),
    (
        "acacia_planks.png",
        include_bytes!("../../../../assets/blocks/acacia_planks.png"),
    ),
    (
        "andesite.png",
        include_bytes!("../../../../assets/blocks/andesite.png"),
    ),
    (
        "birch_planks.png",
        include_bytes!("../../../../assets/blocks/birch_planks.png"),
    ),
    (
        "black_concrete.png",
        include_bytes!("../../../../assets/blocks/black_concrete.png"),
    ),
    (
        "black_stained_glass.png",
        include_bytes!("../../../../assets/blocks/black_stained_glass.png"),
    ),
    (
        "black_terracotta.png",
        include_bytes!("../../../../assets/blocks/black_terracotta.png"),
    ),
    (
        "black_wool.png",
        include_bytes!("../../../../assets/blocks/black_wool.png"),
    ),
    (
        "blue_concrete.png",
        include_bytes!("../../../../assets/blocks/blue_concrete.png"),
    ),
    (
        "blue_stained_glass.png",
        include_bytes!("../../../../assets/blocks/blue_stained_glass.png"),
    ),
    (
        "blue_terracotta.png",
        include_bytes!("../../../../assets/blocks/blue_terracotta.png"),
    ),
    (
        "blue_wool.png",
        include_bytes!("../../../../assets/blocks/blue_wool.png"),
    ),
    (
        "bone_block.png",
        include_bytes!("../../../../assets/blocks/bone_block.png"),
    ),
    (
        "bone_block_top.png",
        include_bytes!("../../../../assets/blocks/bone_block_top.png"),
    ),
    (
        "bookshelf.png",
        include_bytes!("../../../../assets/blocks/bookshelf.png"),
    ),
    (
        "bricks.png",
        include_bytes!("../../../../assets/blocks/bricks.png"),
    ),
    (
        "brown_concrete.png",
        include_bytes!("../../../../assets/blocks/brown_concrete.png"),
    ),
    (
        "brown_stained_glass.png",
        include_bytes!("../../../../assets/blocks/brown_stained_glass.png"),
    ),
    (
        "brown_terracotta.png",
        include_bytes!("../../../../assets/blocks/brown_terracotta.png"),
    ),
    (
        "brown_wool.png",
        include_bytes!("../../../../assets/blocks/brown_wool.png"),
    ),
    (
        "calcite.png",
        include_bytes!("../../../../assets/blocks/calcite.png"),
    ),
    (
        "chiseled_sandstone.png",
        include_bytes!("../../../../assets/blocks/chiseled_sandstone.png"),
    ),
    (
        "clay.png",
        include_bytes!("../../../../assets/blocks/clay.png"),
    ),
    (
        "cobbled_deepslate.png",
        include_bytes!("../../../../assets/blocks/cobbled_deepslate.png"),
    ),
    (
        "cobbled_road.png",
        include_bytes!("../../../../assets/blocks/cobbled_road.png"),
    ),
    (
        "cobblestone.png",
        include_bytes!("../../../../assets/blocks/cobblestone.png"),
    ),
    (
        "copper_block.png",
        include_bytes!("../../../../assets/blocks/copper_block.png"),
    ),
    (
        "cracked_stone_bricks.png",
        include_bytes!("../../../../assets/blocks/cracked_stone_bricks.png"),
    ),
    (
        "cut_sandstone.png",
        include_bytes!("../../../../assets/blocks/cut_sandstone.png"),
    ),
    (
        "cyan_concrete.png",
        include_bytes!("../../../../assets/blocks/cyan_concrete.png"),
    ),
    (
        "cyan_stained_glass.png",
        include_bytes!("../../../../assets/blocks/cyan_stained_glass.png"),
    ),
    (
        "cyan_terracotta.png",
        include_bytes!("../../../../assets/blocks/cyan_terracotta.png"),
    ),
    (
        "cyan_wool.png",
        include_bytes!("../../../../assets/blocks/cyan_wool.png"),
    ),
    (
        "dark_oak_planks.png",
        include_bytes!("../../../../assets/blocks/dark_oak_planks.png"),
    ),
    (
        "dark_prismarine.png",
        include_bytes!("../../../../assets/blocks/dark_prismarine.png"),
    ),
    (
        "deepslate.png",
        include_bytes!("../../../../assets/blocks/deepslate.png"),
    ),
    (
        "deepslate_bricks.png",
        include_bytes!("../../../../assets/blocks/deepslate_bricks.png"),
    ),
    (
        "diorite.png",
        include_bytes!("../../../../assets/blocks/diorite.png"),
    ),
    (
        "glass.png",
        include_bytes!("../../../../assets/blocks/glass.png"),
    ),
    (
        "gold_block.png",
        include_bytes!("../../../../assets/blocks/gold_block.png"),
    ),
    (
        "gray_concrete.png",
        include_bytes!("../../../../assets/blocks/gray_concrete.png"),
    ),
    (
        "gray_stained_glass.png",
        include_bytes!("../../../../assets/blocks/gray_stained_glass.png"),
    ),
    (
        "gray_terracotta.png",
        include_bytes!("../../../../assets/blocks/gray_terracotta.png"),
    ),
    (
        "gray_wool.png",
        include_bytes!("../../../../assets/blocks/gray_wool.png"),
    ),
    (
        "green_concrete.png",
        include_bytes!("../../../../assets/blocks/green_concrete.png"),
    ),
    (
        "green_stained_glass.png",
        include_bytes!("../../../../assets/blocks/green_stained_glass.png"),
    ),
    (
        "green_terracotta.png",
        include_bytes!("../../../../assets/blocks/green_terracotta.png"),
    ),
    (
        "green_wool.png",
        include_bytes!("../../../../assets/blocks/green_wool.png"),
    ),
    (
        "hay_bale.png",
        include_bytes!("../../../../assets/blocks/hay_bale.png"),
    ),
    (
        "hay_bale_top.png",
        include_bytes!("../../../../assets/blocks/hay_bale_top.png"),
    ),
    (
        "iron_block.png",
        include_bytes!("../../../../assets/blocks/iron_block.png"),
    ),
    (
        "jungle_planks.png",
        include_bytes!("../../../../assets/blocks/jungle_planks.png"),
    ),
    (
        "light_blue_concrete.png",
        include_bytes!("../../../../assets/blocks/light_blue_concrete.png"),
    ),
    (
        "light_blue_stained_glass.png",
        include_bytes!("../../../../assets/blocks/light_blue_stained_glass.png"),
    ),
    (
        "light_blue_terracotta.png",
        include_bytes!("../../../../assets/blocks/light_blue_terracotta.png"),
    ),
    (
        "light_blue_wool.png",
        include_bytes!("../../../../assets/blocks/light_blue_wool.png"),
    ),
    (
        "light_gray_concrete.png",
        include_bytes!("../../../../assets/blocks/light_gray_concrete.png"),
    ),
    (
        "light_gray_stained_glass.png",
        include_bytes!("../../../../assets/blocks/light_gray_stained_glass.png"),
    ),
    (
        "light_gray_terracotta.png",
        include_bytes!("../../../../assets/blocks/light_gray_terracotta.png"),
    ),
    (
        "light_gray_wool.png",
        include_bytes!("../../../../assets/blocks/light_gray_wool.png"),
    ),
    (
        "lime_concrete.png",
        include_bytes!("../../../../assets/blocks/lime_concrete.png"),
    ),
    (
        "lime_stained_glass.png",
        include_bytes!("../../../../assets/blocks/lime_stained_glass.png"),
    ),
    (
        "lime_terracotta.png",
        include_bytes!("../../../../assets/blocks/lime_terracotta.png"),
    ),
    (
        "lime_wool.png",
        include_bytes!("../../../../assets/blocks/lime_wool.png"),
    ),
    (
        "magenta_concrete.png",
        include_bytes!("../../../../assets/blocks/magenta_concrete.png"),
    ),
    (
        "magenta_stained_glass.png",
        include_bytes!("../../../../assets/blocks/magenta_stained_glass.png"),
    ),
    (
        "magenta_terracotta.png",
        include_bytes!("../../../../assets/blocks/magenta_terracotta.png"),
    ),
    (
        "magenta_wool.png",
        include_bytes!("../../../../assets/blocks/magenta_wool.png"),
    ),
    (
        "mangrove_planks.png",
        include_bytes!("../../../../assets/blocks/mangrove_planks.png"),
    ),
    (
        "mossy_stone_bricks.png",
        include_bytes!("../../../../assets/blocks/mossy_stone_bricks.png"),
    ),
    (
        "mud_bricks.png",
        include_bytes!("../../../../assets/blocks/mud_bricks.png"),
    ),
    (
        "oak_planks.png",
        include_bytes!("../../../../assets/blocks/oak_planks.png"),
    ),
    (
        "orange_concrete.png",
        include_bytes!("../../../../assets/blocks/orange_concrete.png"),
    ),
    (
        "orange_stained_glass.png",
        include_bytes!("../../../../assets/blocks/orange_stained_glass.png"),
    ),
    (
        "orange_terracotta.png",
        include_bytes!("../../../../assets/blocks/orange_terracotta.png"),
    ),
    (
        "orange_wool.png",
        include_bytes!("../../../../assets/blocks/orange_wool.png"),
    ),
    (
        "packed_mud.png",
        include_bytes!("../../../../assets/blocks/packed_mud.png"),
    ),
    (
        "pink_concrete.png",
        include_bytes!("../../../../assets/blocks/pink_concrete.png"),
    ),
    (
        "pink_stained_glass.png",
        include_bytes!("../../../../assets/blocks/pink_stained_glass.png"),
    ),
    (
        "pink_terracotta.png",
        include_bytes!("../../../../assets/blocks/pink_terracotta.png"),
    ),
    (
        "pink_wool.png",
        include_bytes!("../../../../assets/blocks/pink_wool.png"),
    ),
    (
        "polished_andesite.png",
        include_bytes!("../../../../assets/blocks/polished_andesite.png"),
    ),
    (
        "polished_deepslate.png",
        include_bytes!("../../../../assets/blocks/polished_deepslate.png"),
    ),
    (
        "polished_diorite.png",
        include_bytes!("../../../../assets/blocks/polished_diorite.png"),
    ),
    (
        "polished_granite.png",
        include_bytes!("../../../../assets/blocks/polished_granite.png"),
    ),
    (
        "prismarine.png",
        include_bytes!("../../../../assets/blocks/prismarine.png"),
    ),
    (
        "purple_concrete.png",
        include_bytes!("../../../../assets/blocks/purple_concrete.png"),
    ),
    (
        "purple_stained_glass.png",
        include_bytes!("../../../../assets/blocks/purple_stained_glass.png"),
    ),
    (
        "purple_terracotta.png",
        include_bytes!("../../../../assets/blocks/purple_terracotta.png"),
    ),
    (
        "purple_wool.png",
        include_bytes!("../../../../assets/blocks/purple_wool.png"),
    ),
    (
        "quartz.png",
        include_bytes!("../../../../assets/blocks/quartz.png"),
    ),
    (
        "quartz_bricks.png",
        include_bytes!("../../../../assets/blocks/quartz_bricks.png"),
    ),
    (
        "quartz_pillar.png",
        include_bytes!("../../../../assets/blocks/quartz_pillar.png"),
    ),
    (
        "quartz_pillar_top.png",
        include_bytes!("../../../../assets/blocks/quartz_pillar_top.png"),
    ),
    (
        "red_concrete.png",
        include_bytes!("../../../../assets/blocks/red_concrete.png"),
    ),
    (
        "red_sandstone.png",
        include_bytes!("../../../../assets/blocks/red_sandstone.png"),
    ),
    (
        "red_stained_glass.png",
        include_bytes!("../../../../assets/blocks/red_stained_glass.png"),
    ),
    (
        "red_terracotta.png",
        include_bytes!("../../../../assets/blocks/red_terracotta.png"),
    ),
    (
        "red_wool.png",
        include_bytes!("../../../../assets/blocks/red_wool.png"),
    ),
    (
        "smooth_quartz.png",
        include_bytes!("../../../../assets/blocks/smooth_quartz.png"),
    ),
    (
        "smooth_red_sandstone.png",
        include_bytes!("../../../../assets/blocks/smooth_red_sandstone.png"),
    ),
    (
        "smooth_sandstone.png",
        include_bytes!("../../../../assets/blocks/smooth_sandstone.png"),
    ),
    (
        "smooth_stone.png",
        include_bytes!("../../../../assets/blocks/smooth_stone.png"),
    ),
    (
        "spruce_planks.png",
        include_bytes!("../../../../assets/blocks/spruce_planks.png"),
    ),
    (
        "stone_bricks.png",
        include_bytes!("../../../../assets/blocks/stone_bricks.png"),
    ),
    (
        "stripped_acacia.png",
        include_bytes!("../../../../assets/blocks/stripped_acacia.png"),
    ),
    (
        "stripped_birch.png",
        include_bytes!("../../../../assets/blocks/stripped_birch.png"),
    ),
    (
        "stripped_dark_oak.png",
        include_bytes!("../../../../assets/blocks/stripped_dark_oak.png"),
    ),
    (
        "stripped_jungle.png",
        include_bytes!("../../../../assets/blocks/stripped_jungle.png"),
    ),
    (
        "stripped_mangrove.png",
        include_bytes!("../../../../assets/blocks/stripped_mangrove.png"),
    ),
    (
        "stripped_oak.png",
        include_bytes!("../../../../assets/blocks/stripped_oak.png"),
    ),
    (
        "stripped_spruce.png",
        include_bytes!("../../../../assets/blocks/stripped_spruce.png"),
    ),
    (
        "stripped_warped.png",
        include_bytes!("../../../../assets/blocks/stripped_warped.png"),
    ),
    (
        "terracotta.png",
        include_bytes!("../../../../assets/blocks/terracotta.png"),
    ),
    (
        "thatch.png",
        include_bytes!("../../../../assets/blocks/thatch.png"),
    ),
    (
        "tuff.png",
        include_bytes!("../../../../assets/blocks/tuff.png"),
    ),
    (
        "warped_planks.png",
        include_bytes!("../../../../assets/blocks/warped_planks.png"),
    ),
    (
        "white_concrete.png",
        include_bytes!("../../../../assets/blocks/white_concrete.png"),
    ),
    (
        "white_stained_glass.png",
        include_bytes!("../../../../assets/blocks/white_stained_glass.png"),
    ),
    (
        "white_terracotta.png",
        include_bytes!("../../../../assets/blocks/white_terracotta.png"),
    ),
    (
        "white_wool.png",
        include_bytes!("../../../../assets/blocks/white_wool.png"),
    ),
    (
        "yellow_concrete.png",
        include_bytes!("../../../../assets/blocks/yellow_concrete.png"),
    ),
    (
        "yellow_stained_glass.png",
        include_bytes!("../../../../assets/blocks/yellow_stained_glass.png"),
    ),
    (
        "yellow_terracotta.png",
        include_bytes!("../../../../assets/blocks/yellow_terracotta.png"),
    ),
    (
        "yellow_wool.png",
        include_bytes!("../../../../assets/blocks/yellow_wool.png"),
    ),
];

/// Anisotropic filtering samples for the block array texture (wgpu's maximum).
pub const BLOCK_TEXTURE_ANISOTROPY: u16 = 16;

fn setup_scene(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut voxel_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>>,
) {
    let registry = crate::blocks::load_registry();

    // Loader: native reads PNGs from assets/blocks/; wasm resolves from the
    // embedded texture map. Returns None on any failure (build_layers then
    // falls the block back to the layer-0 missing texture).
    let (rgba_layers, block_layers) = build_layers(&registry, load_block_texture);
    // The map's block colours: each block's top layer, before the crack layers join the array.
    let map_rgb = map::palette::block_colours(&registry, &rgba_layers, &block_layers);
    commands.insert_resource(map::palette::make_palette(&mut images, map_rgb));

    let (w, h) = (rgba_layers[0].width(), rgba_layers[0].height());
    // Every layer carries its full mip chain (see `block_textures::mip_chain`),
    // laid out layer-major — each layer's levels back to back — which is the
    // order `Image::data_order`'s default uploads.
    let mut mip_level_count = 1;
    let mut data: Vec<u8> = Vec::new();
    for layer in &rgba_layers {
        let chain = block_textures::mip_chain(layer);
        mip_level_count = chain.len() as u32;
        for level in &chain {
            data.extend_from_slice(level.as_raw());
        }
    }
    // `new_uninit`, not `new`: `new` asserts the data is exactly one mip level.
    let mut array_image = Image::new_uninit(
        Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: rgba_layers.len() as u32,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    array_image.texture_descriptor.mip_level_count = mip_level_count;
    array_image.data = Some(data);
    // Repeat addressing: the shader's block-cell UVs (position / BLOCK_VOXELS)
    // exceed 1.0 past the first block. All filters linear because anisotropic
    // filtering requires it (wgpu rejects anisotropy with a nearest filter),
    // and grazing-angle ground needs anisotropy or trilinear smears it into
    // shimmering noise. The crisp near look is kept in the shaders instead:
    // `crisp_uv` snaps magnified samples to texel centres, antialiasing only
    // the one-pixel seam between texels. Minification blends texels and mip
    // levels so distant terrain converges on each block's average colour.
    let mut sampler = ImageSamplerDescriptor::linear();
    sampler.anisotropy_clamp = BLOCK_TEXTURE_ANISOTROPY;
    sampler.address_mode_u = ImageAddressMode::Repeat;
    sampler.address_mode_v = ImageAddressMode::Repeat;
    sampler.address_mode_w = ImageAddressMode::Repeat;
    array_image.sampler = ImageSampler::Descriptor(sampler);
    // wgpu's default texture-view dimension for a `D2` texture collapses to plain `D2`
    // when `depth_or_array_layers == 1` (per the WebGPU default-view algorithm), which
    // mismatches the `dimension = "2d_array"` binding `StandardVoxelMaterial` declares as
    // soon as there's only a single block texture. Force the view to stay an array so
    // the bind group is valid regardless of layer count.
    array_image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..Default::default()
    });
    let handle = images.add(array_image);

    // The pulled render path binds the same array texture directly (no material).
    commands.insert_resource(voxel_pull::BlockArrayTexture(handle.clone()));
    commands.insert_resource(voxel_pull::BlockTopLayers(std::sync::Arc::new(
        block_layers.side_to_top(),
    )));
    commands.insert_resource(meshing::ChunkMaterial(voxel_materials.add(
        ExtendedMaterial {
            base: StandardMaterial {
                perceptual_roughness: 0.9,
                // Cutout blocks (leaves) drop their alpha-0 texels in
                // voxel_texture.wgsl's `alpha_discard`. One material serves
                // every chunk, which is fine: every other block texture is
                // fully opaque (alpha 255), so only leaves lose texels.
                alpha_mode: AlphaMode::Mask(0.5),
                ..Default::default()
            },
            extension: StandardVoxelMaterial {
                voxels_texture: handle,
                fortress: Default::default(),
                top_layers: images.add(voxel_material::top_layers_image(&block_layers.side_to_top())),
            },
        },
    )));
    commands.insert_resource(block_layers);

    atmosphere::spawn_lights(&mut commands);
}

/// Load one block texture as RGBA8. Native reads `assets/blocks/<path>`; wasm
/// resolves it from the compiled-in texture table. `None` on any failure.
fn load_block_texture(path: &str) -> Option<image::RgbaImage> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let full = resolve_blocks_dir().join(path);
        match image::ImageReader::open(&full)
            .ok()
            .and_then(|r| r.decode().ok())
        {
            Some(img) => Some(img.to_rgba8()),
            None => {
                warn!("could not read block texture {full:?}");
                None
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = EMBEDDED_BLOCK_TEXTURES
            .iter()
            .find(|(n, _)| *n == path)
            .map(|(_, b)| *b)?;
        match image::load_from_memory(bytes) {
            Ok(img) => Some(img.to_rgba8()),
            Err(e) => {
                warn!("could not decode embedded block texture {path}: {e:?}");
                None
            }
        }
    }
}

