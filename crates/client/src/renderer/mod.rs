use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use crossbeam::channel::{Receiver, Sender};
use game::{ClientId, ClientUpdateEvent, GameEventKind};

mod atmosphere;
#[cfg(not(target_arch = "wasm32"))]
mod autopilot;
mod avatar;
mod block_textures;
pub mod bricks;
pub mod bridge;
pub(crate) mod collider_debug;
pub mod commander;
pub(crate) mod cursor;
#[cfg(feature = "dev_ui")]
pub mod dev_ui;
pub(crate) mod boundaries;
pub(crate) mod fortress_view;
pub(crate) mod slice_plane;
pub mod config;
pub mod convert;
pub(crate) mod diag;
pub mod far_voxel;
pub(crate) mod grid_debug;
pub(crate) mod hud;
pub(crate) mod input;
mod interpolate;
mod look;
mod motion_probe;
pub(crate) mod join_timeline;
pub(crate) mod lod_rings;
pub mod meshing;
pub mod model;
#[cfg(not(target_arch = "wasm32"))]
pub mod render_bench;
mod skybox;
pub mod slint_composite;
pub mod slint_overlay;
pub use slint_composite::UiCamera;
mod voxel_material;
pub mod voxel_pull;
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
        app.add_plugins((
            MaterialPlugin::<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>::default(),
            bevy::diagnostic::FrameTimeDiagnosticsPlugin::default(),
            voxel_pull::VoxelPullPlugin,
            fortress_view::FortressOverlayPlugin,
            far_voxel::FarVoxelPlugin,
            model::BbModelPlugin,
        ))
        .insert_resource(ClientUpdates(self.client_recv.clone()))
        .insert_resource(GameEvents(self.game_send.clone()))
        .insert_resource(ClientRequests(self.requests.clone()))
        .init_resource::<world_map::WorldMapState>()
        .add_message::<world_map::WorldMapMsg>()
        .init_resource::<LocalPlayer>()
        .init_resource::<bridge::Regions>()
        .init_resource::<bridge::RegionRoots>()
        .init_resource::<bridge::SimEntityMap>()
        .init_resource::<bridge::SnapshotVoxelQueue>()
        .init_resource::<bridge::RetiredRegionRoots>()
        .init_resource::<hud::HudStatus>()
        .init_resource::<boundaries::BoundaryOverlay>()
        .init_resource::<boundaries::BoundaryUniform>()
        .init_resource::<fortress_view::FortressViewUniform>()
        .init_resource::<avatar::AvatarAssets>()
        .init_resource::<slint_overlay::UiUpdates>()
        .init_resource::<slint_overlay::Hotbar>()
        .insert_resource(collider_debug::DebugFlagsRes(self.debug_flags.clone()))
        .init_resource::<collider_debug::DebugColliderLines>()
        .init_resource::<grid_debug::GridDebug>()
        .init_resource::<cursor::CursorPolicy>()
        .init_resource::<meshing::queue::ChunkGrid>()
        .init_resource::<meshing::queue::RemeshQueue>()
        .init_resource::<meshing::queue::MeshBudgets>()
        .init_resource::<meshing::queue::MeshBudgetPolicy>()
        .init_resource::<meshing::queue::FrameUploads>()
        .init_resource::<meshing::lod::LodBuildQueue>()
        .init_resource::<meshing::lod::FarFrameUploads>()
        // Cave culling starts OFF: the world has no caves yet, and the flood
        // cost 5-8.5 ms per turning frame at the 5x5 window (2026-09-23
        // bench) for nothing to hide. F4 / the F3 dev panel turn it on.
        .insert_resource(meshing::visibility::CullingConfig { graph_enabled: false })
        .init_resource::<config::TerrainRenderConfig>()
        .init_resource::<input::MouseCoalesceWindow>()
        .init_resource::<look::LocalLook>()
        .init_resource::<lod_rings::LodRings>()
        .init_resource::<grid_debug::TerrainDebug>()
        .init_resource::<join_timeline::WorldReady>()
        .insert_resource(self.camera_mode)
        .add_systems(
            PreUpdate,
            (
                input::forward_input.after(bevy::input::InputSystems),
                look::integrate_local_look.after(bevy::input::InputSystems),
                (
                    bridge::drain_client_updates,
                    bridge::despawn_retired_regions,
                    bridge::drain_region_updates,
                    bridge::apply_snapshot_voxels,
                    bridge::dedupe_ghosts,
                    bridge::enforce_single_active_camera,
                    slint_overlay::drain_ui_updates,
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
                    meshing::queue::spawn_mesh_tasks,
                    meshing::apply_meshed_chunks,
                    // LOD meshes land in the same chain; culling moved to
                    // cull_plugin (PostUpdate) so it sees the current
                    // frame's frustum — it runs after this chain either way.
                    far_voxel::lifecycle::sync_far_pass_readiness,
                    far_voxel::lifecycle::resolve_far_upload_results,
                    meshing::lod::spawn_lod_mesh_tasks,
                    far_voxel::lifecycle::queue_far_brick_tasks,
                    meshing::lod::apply_lod_meshes,
                    far_voxel::lifecycle::remove_far_voxel_cells,
                    far_voxel::lifecycle::refresh_far_neighbors,
                    far_voxel::lifecycle::apply_far_brick_tasks,
                    far_voxel::gpu::publish_far_uploads,
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
                world_map::apply_world_map_msgs,
                // Per-frame mouse look replaces the per-tick sim look that
                // interpolate_transforms just snapped the camera to.
                look::apply_local_look.after(interpolate::interpolate_transforms),
                motion_probe::probe_camera_motion.after(look::apply_local_look),
                motion_probe::probe_still_frames.after(look::apply_local_look),
                join_timeline::track_join_timeline,
                avatar::attach_avatars,
                avatar::squish_slimes.after(interpolate::interpolate_transforms),
                skybox::attach_skybox,
                atmosphere::attach_fog,
                meshing::visibility::toggle_culling,
                collider_debug::draw_collider_wireframes,
                (boundaries::toggle_boundary_overlay, boundaries::update_boundary_uniform).chain(),
                grid_debug::toggle_grid_debug,
                grid_debug::draw_grid_debug.after(grid_debug::toggle_grid_debug),
                (grid_debug::toggle_terrain_debug, grid_debug::apply_terrain_debug).chain(),
            ),
        );
        app.add_systems(
            Update,
            motion_probe::probe_region_crossing.after(interpolate::interpolate_transforms),
        );
        app.add_plugins(slint_overlay::HudLayerPlugin);
        if self.camera_mode == commander::CameraMode::Commander {
            app.add_plugins((
                commander::CommanderPlugin,
                slint_overlay::SlintOverlayPlugin,
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
/// time. The `embedded_block_textures_match_assets_dir` test keeps this list
/// in sync with assets/blocks/*.png.
#[cfg(any(target_arch = "wasm32", test))]
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
                ..Default::default()
            },
            extension: StandardVoxelMaterial {
                voxels_texture: handle,
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

#[cfg(test)]
mod tests {
    use super::{resolve_blocks_dir, EMBEDDED_BLOCK_TEXTURES};

    #[test]
    fn embedded_block_textures_match_assets_dir() {
        let dir = resolve_blocks_dir();
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("assets/blocks must exist for this test")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("png"))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        on_disk.sort();
        let mut embedded: Vec<String> = EMBEDDED_BLOCK_TEXTURES
            .iter()
            .map(|(n, _)| (*n).to_string())
            .collect();
        embedded.sort();
        assert_eq!(
            embedded, on_disk,
            "EMBEDDED_BLOCK_TEXTURES is out of sync with assets/blocks/*.png — update the const in renderer/mod.rs"
        );
    }

    #[test]
    fn embedded_block_textures_cover_manifest_references() {
        use game::TextureSpec;

        let reg = crate::blocks::load_registry();
        for (_, def) in reg.iter() {
            let referenced: Vec<&str> = match &def.textures {
                TextureSpec::All(p) => vec![p.as_str()],
                TextureSpec::Faces { top, side, bottom } => {
                    vec![top.as_str(), side.as_str(), bottom.as_str()]
                }
                TextureSpec::Untextured => vec![],
            };
            for path in referenced {
                assert!(
                    EMBEDDED_BLOCK_TEXTURES.iter().any(|(n, _)| *n == path),
                    "block manifest references {path:?}, but it is not in EMBEDDED_BLOCK_TEXTURES \
                     — add it to that const in renderer/mod.rs so the wasm build has it"
                );
            }
        }
    }

    #[test]
    fn test_resolve_blocks_dir_with_cargo_manifest() {
        let blocks_path = resolve_blocks_dir();
        // When tests run, CARGO_MANIFEST_DIR should be set to crates/client.
        // The resolved path should end with assets/blocks.
        assert!(
            blocks_path.ends_with("assets/blocks"),
            "blocks_path should end with assets/blocks, got {:?}",
            blocks_path
        );

        // Verify the actual directory and asset exist (crates/client/../../assets/blocks -> assets/blocks).
        assert!(
            blocks_path.exists(),
            "resolved blocks directory should exist at {:?}",
            blocks_path
        );

        // Check that the expected dirt.png texture exists.
        let dirt_texture = blocks_path.join("dirt.png");
        assert!(
            dirt_texture.exists(),
            "dirt.png should exist at {:?}",
            dirt_texture
        );
    }
}
