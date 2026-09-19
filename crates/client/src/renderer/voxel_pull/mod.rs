//! The pulled (mesh-free) chunk render path: per-quad records in storage
//! buffers, drawn by custom Opaque3d phase items. Main-world components
//! live here; the render-world half (pipeline/prepare/queue) is split into
//! submodules. Modeled on bevy-0.18.1/examples/shader_advanced/custom_phase_item.rs.

use std::sync::Arc;

use bevy::{
    camera::primitives::Aabb,
    core_pipeline::{
        core_3d::{graph::Core3d, Opaque3d},
        prepass::Opaque3dPrepass,
    },
    pbr::{init_prepass_pipeline, Shadow},
    prelude::*,
    render::{
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_graph::{RenderGraph, RenderGraphExt},
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
    DrawVoxelPullDepth,
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
        let center = transform.transform_point(Vec3::from(aabb.center));
        let half = transform.scale() * Vec3::from(aabb.half_extents);
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

pub struct VoxelPullPlugin;

impl Plugin for VoxelPullPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ExtractResourcePlugin::<BlockArrayTexture>::default());
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
                    prepare_chunk_quads.in_set(RenderSystems::PrepareBindGroups),
                    prepare_view_chunk_draws
                        .in_set(RenderSystems::PrepareBindGroups)
                        .after(prepare_chunk_quads),
                    gpu::prepare_active_near_mode.in_set(RenderSystems::PrepareResources),
                    queue_voxel_pull.in_set(RenderSystems::Queue),
                    queue_voxel_pull_prepass.in_set(RenderSystems::Queue),
                    queue_voxel_pull_shadows.in_set(RenderSystems::Queue),
                ),
            );
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<VoxelPullPipeline>();
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
                )
                .add_render_graph_node::<gpu::NearFrustumCullNode>(
                    Core3d,
                    gpu::NearGpuNode::FrustumCull,
                )
                .add_render_graph_node::<gpu::NearLatePhaseClearNode>(
                    Core3d,
                    gpu::NearGpuNode::ClearLatePhases,
                )
                .add_render_graph_node::<gpu::NearHzbCullNode>(Core3d, gpu::NearGpuNode::HzbCull);
            if telemetry_enabled {
                render_app
                    .init_resource::<gpu::NearTelemetryReadback>()
                    .add_systems(
                        Render,
                        gpu::begin_near_telemetry_frame.in_set(RenderSystems::PrepareResources),
                    )
                    .add_render_graph_node::<gpu::NearTelemetryCopyNode>(
                        Core3d,
                        gpu::NearGpuNode::TelemetryCopy,
                    );
            }
            let mut graph = render_app.world_mut().resource_mut::<RenderGraph>();
            let core_3d = graph
                .get_sub_graph_mut(Core3d)
                .expect("Core3d render graph is initialized");
            gpu::add_near_gpu_graph_edges(core_3d, telemetry_enabled);
        }
    }
}
