//! The pulled (mesh-free) chunk render path: per-quad records in storage
//! buffers, drawn by custom Opaque3d phase items. Main-world component + toggle
//! live here; the render-world half (pipeline/prepare/queue) is split into
//! submodules. Modeled on bevy-0.18.1/examples/shader_advanced/custom_phase_item.rs.

use std::sync::Arc;

use bevy::{
    camera::primitives::Aabb,
    core_pipeline::{core_3d::Opaque3d, prepass::Opaque3dPrepass},
    pbr::{init_prepass_pipeline, Shadow},
    prelude::*,
    render::{
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_phase::AddRenderCommand,
        render_resource::SpecializedRenderPipelines,
        sync_world::{RenderEntity, SyncToRenderWorld},
        Extract, ExtractSchedule, Render, RenderApp, RenderStartup, RenderSystems,
    },
};

use super::meshing::records::QuadRecord;

pub mod pipeline;
pub mod prepare;
pub mod queue;

use pipeline::{init_voxel_pull_depth_pipeline, VoxelPullDepthPipeline, VoxelPullPipeline};
use prepare::prepare_chunk_quads;
use queue::{
    queue_voxel_pull, queue_voxel_pull_prepass, queue_voxel_pull_shadows, DrawVoxelPull,
    DrawVoxelPullDepth,
};

/// Default for the old-vs-pull render path toggle. Flipped to `true` in Task 8
/// once forward+prepass+shadow parity was confirmed; kept a named constant so
/// it is findable and overridable in tests either way (legacy `Mesh3d` tests
/// pin `RenderPath { pull: false }`).
pub const DEFAULT_PULL_PATH: bool = true;

#[derive(Resource)]
pub struct RenderPath {
    pub pull: bool,
}

impl Default for RenderPath {
    fn default() -> Self {
        Self { pull: DEFAULT_PULL_PATH }
    }
}

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

/// Render-world copy of one chunk's draw inputs. Re-extracted every frame
/// (cheap: `Arc` clone + matrices); buffer upload happens only when `changed`.
#[derive(Component)]
pub struct ExtractedChunkQuads {
    pub records: Arc<Vec<QuadRecord>>,
    pub world_from_local: Mat4,
    /// World-space AABB for per-view frustum tests at queue time (Bevy's
    /// visibility system only frustum-culls `VisibilityClass`'d entities, which
    /// mesh-less chunks are not).
    pub center: Vec3,
    pub half_extents: Vec3,
    pub changed: bool,
}

pub fn extract_chunk_quads(
    mut commands: Commands,
    chunks: Extract<
        Query<(
            RenderEntity,
            Ref<ChunkQuads>,
            &GlobalTransform,
            &Aabb,
            &InheritedVisibility,
        )>,
    >,
) {
    for (entity, quads, transform, aabb, vis) in &chunks {
        let mut ec = commands.entity(entity);
        // Cave-culling hides via Visibility -> InheritedVisibility=false.
        if !vis.get() {
            ec.remove::<ExtractedChunkQuads>();
            continue;
        }
        let world_from_local = transform.to_matrix();
        let center = transform.transform_point(Vec3::from(aabb.center));
        let half = transform.scale() * Vec3::from(aabb.half_extents);
        ec.insert(ExtractedChunkQuads {
            records: quads.0.clone(),
            world_from_local,
            center,
            half_extents: half,
            changed: quads.is_changed(),
        });
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
            .add_systems(ExtractSchedule, extract_chunk_quads)
            .add_systems(
                Render,
                (
                    prepare_chunk_quads.in_set(RenderSystems::PrepareBindGroups),
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
    }
}
