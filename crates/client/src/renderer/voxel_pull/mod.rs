//! The pulled (mesh-free) chunk render path: per-quad records in storage
//! buffers, drawn by custom Opaque3d phase items (and AlphaMask3d ones for
//! the cutout ranges — leaves). Main-world components
//! live here; the render-world half (pipeline/prepare/queue) is split into
//! submodules. Modeled on bevy-0.18.1/examples/shader_advanced/custom_phase_item.rs.

use std::sync::Arc;

use bevy::{
    camera::primitives::Aabb,
    core_pipeline::{
        core_3d::{AlphaMask3d, Opaque3d},
        Core3d,
        prepass::{AlphaMask3dPrepass, Opaque3dPrepass},
    },
    pbr::{init_prepass_pipeline, Shadow},
    prelude::*,
    render::{
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_phase::AddRenderCommand,
        render_resource::SpecializedRenderPipelines,
        sync_world::{MainEntity, RenderEntity, SyncToRenderWorld},
        Extract, ExtractSchedule, Render, RenderApp, RenderStartup, RenderSystems,
    },
};

use super::meshing::records::QuadRecord;

pub mod arena;
pub mod gpu;
pub mod pipeline;
pub mod prepare;
pub mod queue;

use pipeline::{init_voxel_pull_depth_pipeline, VoxelPullDepthPipeline, VoxelPullPipeline};
use prepare::{prepare_chunk_quads, prepare_view_chunk_draws};
use queue::{
    queue_voxel_pull, queue_voxel_pull_prepass, queue_voxel_pull_shadows, DrawVoxelPull,
    DrawVoxelPullCutout, DrawVoxelPullDepth, DrawVoxelPullDepthCutout,
};

/// The pull-path replacement for `Mesh3d`: every visible greedy quad of one
/// chunk (or LOD cell), as GPU-ready records. `Arc` so render-world extraction
/// clones the handle, not the data. Requires `Visibility` because without
/// `Mesh3d` nothing else inserts it, and cave-culling writes `&mut Visibility`;
/// requires `SyncToRenderWorld` because — again without `Mesh3d` — nothing else
/// gives the entity a `RenderEntity` for extraction to target.
#[derive(Component, Clone)]
#[require(Visibility, SyncToRenderWorld)]
pub struct ChunkQuads(pub Arc<Vec<QuadRecord>>);

/// The block array texture, shared with the mesh path's material. Extracted
/// into the render world (via `ExtractResource`) so `prepare` can bind it.
#[derive(Resource, Clone, ExtractResource)]
pub struct BlockArrayTexture(pub Handle<Image>);

/// Side-face layer -> top-face layer of the same block
/// (`BlockTextureLayers::side_to_top`), bound as `voxel_pull.wgsl`'s
/// `top_layers` so distant risers can fade to their block's top.
#[derive(Resource, Clone, ExtractResource)]
pub struct BlockTopLayers(pub Arc<Vec<u32>>);

/// Render-world copy of one chunk's draw inputs. The render world is
/// retained, so this is (re-)inserted ONLY when the chunk's quads, transform
/// or inherited visibility changed — re-inserting all ~57k chunk entities
/// every frame measured 17.6 ms/frame at the production world size
/// (2026-09-19 trace), 13 ms of it just applying the insert commands.
/// `prepare_chunk_quads` keys its uploads off `Changed<ExtractedChunkQuads>`,
/// i.e. off that insert; there is deliberately no stored "changed" flag, which
/// would go stale the first frame a chunk is NOT re-extracted.
#[derive(Component)]
pub struct ExtractedChunkQuads {
    pub records: Arc<Vec<QuadRecord>>,
    pub world_from_local: Mat4,
    /// World-space AABB for per-view frustum tests at queue time (Bevy's
    /// visibility system only frustum-culls `VisibilityClass`'d entities, which
    /// mesh-less chunks are not).
    pub center: Vec3,
    pub half_extents: Vec3,
    /// Main-world cave-connectivity result. Hidden chunks keep their stable
    /// metadata slot but are marked inactive for both CPU and GPU culling.
    pub cave_active: bool,
}

#[derive(Resource, Default)]
pub struct RemovedChunkQuads(pub Vec<Entity>);

/// `[pull-arena]` every [`ARENA_LOG_INTERVAL`] (`PERF_LOG=1` only).
const ARENA_LOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

fn log_arena_stats(arena: Res<arena::GpuQuadArena>, mut last: Local<Option<std::time::Instant>>) {
    let now = std::time::Instant::now();
    if last.is_some_and(|t| now - t < ARENA_LOG_INTERVAL) {
        return;
    }
    *last = Some(now);
    arena.log_stats();
}

pub fn extract_removed_chunk_quads(
    mut commands: Commands,
    mut removed: Extract<RemovedComponents<ChunkQuads>>,
    still_alive: Extract<Query<(), With<ChunkQuads>>>,
    render_entities: Query<(Entity, &MainEntity)>,
    mut out: ResMut<RemovedChunkQuads>,
) {
    out.0.clear();
    out.0.extend(
        removed
            .read()
            .filter(|entity| !still_alive.contains(*entity)),
    );
    // The common frame removes nothing: don't walk every render entity for it.
    if out.0.is_empty() {
        return;
    }
    let removed: bevy::platform::collections::HashSet<Entity> = out.0.iter().copied().collect();
    for (render_entity, main_entity) in &render_entities {
        if removed.contains(&main_entity.id()) {
            commands
                .entity(render_entity)
                .remove::<(ExtractedChunkQuads, prepare::GpuChunkQuads)>();
        }
    }
}

/// World-space bounds (centre, half extents) of a local `aabb` under
/// `transform`: the axis-aligned box around its 8 transformed corners (each
/// world half extent sums the local ones weighted by the absolute rotated,
/// scaled axes). Terrain is only translated and scaled, which reduces this
/// exactly to `scale * half`; a voxel body's bricks are rotated too.
pub fn world_aabb(transform: &GlobalTransform, aabb: &Aabb) -> (Vec3, Vec3) {
    let affine = transform.affine();
    let center = affine.transform_point3(Vec3::from(aabb.center));
    let half = Vec3::from(aabb.half_extents);
    let m = affine.matrix3;
    let half = m.x_axis.abs() * half.x + m.y_axis.abs() * half.y + m.z_axis.abs() * half.z;
    (center, Vec3::from(half))
}

#[allow(clippy::type_complexity)] // Bevy system: params are DI, not an API
pub fn extract_chunk_quads(
    mut commands: Commands,
    chunks: Extract<
        Query<
            (RenderEntity, &ChunkQuads, &GlobalTransform, &Aabb, &InheritedVisibility),
            Or<(Changed<ChunkQuads>, Changed<GlobalTransform>, Changed<InheritedVisibility>)>,
        >,
    >,
    all_chunks: Extract<Query<&InheritedVisibility, With<ChunkQuads>>>,
    mut debug_frame: Local<u32>,
) {
    let mut extracted = 0u32;
    for (entity, quads, transform, aabb, vis) in &chunks {
        extracted += 1;
        let mut ec = commands.entity(entity);
        // Cave-culling hides via Visibility -> InheritedVisibility=false.
        let cave_active = vis.get();
        let world_from_local = transform.to_matrix();
        let (center, half) = world_aabb(transform, aabb);
        ec.insert(ExtractedChunkQuads {
            records: quads.0.clone(),
            world_from_local,
            center,
            half_extents: half,
            cave_active,
        });
    }
    // VOXEL_PULL_DEBUG=1: once a second, where do chunks disappear — here
    // (main-world Visibility, i.e. the cave-culling BFS) or at the per-view
    // frustum cull (queue.rs logs the other half)?
    *debug_frame = debug_frame.wrapping_add(1);
    if *debug_frame % 60 == 0 && std::env::var_os("VOXEL_PULL_DEBUG").is_some() {
        let total = all_chunks.iter().count();
        let hidden = all_chunks.iter().filter(|vis| !vis.get()).count();
        info!(
            "[pull-cull] extract: {total} ChunkQuads entities, {hidden} hidden by cave-culling BFS, \
             {extracted} re-extracted this frame"
        );
    }
}

/// Storage buffers the pull path binds in its busiest shader stage, the
/// fragment stage: quad records and chunk transforms (`voxel_pull.wgsl`
/// group(1) bindings 0 and 1, visible to both stages) plus the fragment-only
/// top-layer table (binding 4).
pub const PULL_STORAGE_BUFFERS_PER_STAGE: u32 = 3;

/// Whether the device can run the quad-record backends at all. They read the
/// records from storage buffers in the VERTEX stage, which WebGL2 (the wasm
/// build's `webgl2` backend) and GLES 3.0 do not have — creating
/// `voxel_pull_chunk_layout` there is a fatal wgpu validation error.
pub fn device_supports_quad_records(
    device: &bevy::render::renderer::RenderDevice,
    adapter: &bevy::render::renderer::RenderAdapter,
) -> bool {
    adapter
        .get_downlevel_capabilities()
        .flags
        .contains(bevy::render::render_resource::DownlevelFlags::VERTEX_STORAGE)
        && device.limits().max_storage_buffers_per_shader_stage >= PULL_STORAGE_BUFFERS_PER_STAGE
}

pub struct VoxelPullPlugin;

impl Plugin for VoxelPullPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            ExtractResourcePlugin::<BlockArrayTexture>::default(),
            ExtractResourcePlugin::<BlockTopLayers>::default(),
            ExtractResourcePlugin::<super::boundaries::BoundaryUniform>::default(),
            ExtractResourcePlugin::<super::fortress_view::FortressViewUniform>::default(),
        ));
        // Main-world tally driving the `[pull-mem]` headline log in
        // `apply_meshed_chunks`. Registered here (not in `renderer/mod.rs`) so
        // the pull path owns its own accounting resource.
        app.init_resource::<super::meshing::PullMemStats>();
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<arena::GpuQuadArena>()
            .init_resource::<gpu::ActiveNearMode>()
            .init_resource::<gpu::NearRendererMetrics>()
            .init_resource::<RemovedChunkQuads>()
            .init_resource::<SpecializedRenderPipelines<VoxelPullPipeline>>()
            .init_resource::<SpecializedRenderPipelines<VoxelPullDepthPipeline>>()
            .add_render_command::<Opaque3d, DrawVoxelPull>()
            .add_render_command::<Opaque3dPrepass, DrawVoxelPullDepth>()
            .add_render_command::<Shadow, DrawVoxelPullDepth>()
            .add_render_command::<AlphaMask3d, DrawVoxelPullCutout>()
            .add_render_command::<AlphaMask3dPrepass, DrawVoxelPullDepthCutout>()
            .add_render_command::<Shadow, DrawVoxelPullDepthCutout>()
            // The depth pipeline clones `PrepassPipeline`'s view layout, which
            // only exists after `init_prepass_pipeline` runs in `RenderStartup`.
            .add_systems(
                RenderStartup,
                init_voxel_pull_depth_pipeline.after(init_prepass_pipeline),
            )
            .add_systems(
                ExtractSchedule,
                (
                    extract_chunk_quads,
                    extract_removed_chunk_quads,
                    gpu::extract_requested_near_backend,
                ),
            )
            .add_systems(
                Render,
                (
                    // The pipelines exist only on a device that can run the
                    // pull path (see `finish`); without them nothing is drawn
                    // from records and these have no work.
                    prepare_chunk_quads
                        .in_set(RenderSystems::PrepareBindGroups)
                        .run_if(resource_exists::<VoxelPullPipeline>),
                    prepare_view_chunk_draws
                        .in_set(RenderSystems::PrepareBindGroups)
                        .after(prepare_chunk_quads),
                    gpu::prepare_active_near_mode.in_set(RenderSystems::PrepareResources),
                    log_arena_stats
                        .after(prepare_chunk_quads)
                        .run_if(|| game::perf::budget_lines_requested()),
                    queue_voxel_pull
                        .in_set(RenderSystems::Queue)
                        .run_if(resource_exists::<VoxelPullPipeline>),
                    queue_voxel_pull_prepass
                        .in_set(RenderSystems::Queue)
                        .run_if(resource_exists::<VoxelPullDepthPipeline>),
                    queue_voxel_pull_shadows
                        .in_set(RenderSystems::Queue)
                        .run_if(resource_exists::<VoxelPullDepthPipeline>),
                ),
            );
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app(RenderApp) else {
            return;
        };
        let quad_records_supported = {
            let world = render_app.world();
            device_supports_quad_records(
                world.resource::<bevy::render::renderer::RenderDevice>(),
                world.resource::<bevy::render::renderer::RenderAdapter>(),
            )
        };
        if !quad_records_supported {
            // `finish` runs before the first `Update`, so no chunk has been
            // meshed as records yet: meshing reads the downgraded config.
            if let Some(mut config) = app
                .world_mut()
                .get_resource_mut::<super::config::TerrainRenderConfig>()
            {
                let resolved = config.for_device(false);
                if *config != resolved {
                    warn!(
                        "[render] no vertex-stage storage buffers on this device (WebGL2?): \
                         near backend {} -> {}",
                        config.near.as_str(),
                        resolved.near.as_str()
                    );
                    *config = resolved;
                }
            }
            return;
        }
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        // Bevy 0.19 builds `MeshPipelineViewLayouts` in `RenderStartup`, after
        // `finish`, so the forward pipeline (which clones them) is created there
        // too — ahead of the depth pipeline, which reads it.
        render_app.add_systems(
            RenderStartup,
            init_voxel_pull_pipeline
                .after(bevy::pbr::init_mesh_pipeline_view_layouts)
                .before(init_voxel_pull_depth_pipeline),
        );
        let telemetry_enabled = render_app
            .world()
            .contains_resource::<gpu::NearTelemetryEnabled>();

        let gpu_supported = {
            let world = render_app.world();
            gpu::runtime_capabilities(
                world
                    .resource::<bevy::render::batching::gpu_preprocessing::GpuPreprocessingSupport>(
                    ),
                world.resource::<bevy::render::renderer::RenderDevice>(),
                world.resource::<bevy::render::renderer::RenderAdapterInfo>(),
            )
            .gpu_culling
        };
        if gpu_supported {
            render_app
                .init_resource::<gpu::GpuNearCullPipeline>()
                .add_systems(
                    Render,
                    gpu::prepare_gpu_near_view_buffers
                        .in_set(RenderSystems::PrepareBindGroups)
                        .after(prepare_chunk_quads),
                );
            if telemetry_enabled {
                render_app
                    .init_resource::<gpu::NearTelemetryReadback>()
                    .add_systems(
                        Render,
                        gpu::begin_near_telemetry_frame.in_set(RenderSystems::PrepareResources),
                    );
            }
            let mut schedules = render_app
                .world_mut()
                .resource_mut::<bevy::ecs::schedule::Schedules>();
            gpu::add_near_gpu_systems(schedules.entry(Core3d), telemetry_enabled);
        }
    }
}

fn init_voxel_pull_pipeline(world: &mut World) {
    world.init_resource::<VoxelPullPipeline>();
}
