//! Opaque3d queue + custom draw command for the pulled render path. Structure
//! mirrors bevy-0.18.1/examples/shader_advanced/custom_phase_item.rs.

use bevy::{
    camera::primitives::{Aabb, Frustum},
    core_pipeline::{
        core_3d::{Opaque3d, Opaque3dBatchSetKey, Opaque3dBinKey},
        prepass::{
            DeferredPrepass, DepthPrepass, MotionVectorPrepass, NormalPrepass, Opaque3dPrepass,
            OpaqueNoLightmap3dBatchSetKey, OpaqueNoLightmap3dBinKey,
        },
    },
    ecs::{
        change_detection::Tick,
        query::ROQueryItem,
        system::{lifetimeless::Read, SystemParamItem},
    },
    pbr::{MeshPipelineViewLayoutKey, SetMeshViewBindGroup, SetPrepassViewBindGroup},
    prelude::*,
    render::{
        render_phase::{
            BinnedRenderPhaseType, DrawFunctions, InputUniformIndex, PhaseItem, RenderCommand,
            RenderCommandResult, SetItemPipeline, TrackedRenderPass, ViewBinnedRenderPhases,
        },
        render_resource::{PipelineCache, SpecializedRenderPipelines},
        sync_world::MainEntity,
        view::ExtractedView,
    },
};

use super::pipeline::{
    VoxelPullDepthPipeline, VoxelPullDepthPipelineKey, VoxelPullPipeline, VoxelPullPipelineKey,
};
use super::prepare::GpuChunkQuads;
use super::ExtractedChunkQuads;

/// The draw chain for one pulled chunk: bind the pipeline, bind the mesh view
/// bind group at group 0, then bind our per-chunk group 1 and issue the draw.
pub type DrawVoxelPull = (SetItemPipeline, SetMeshViewBindGroup<0>, DrawChunkQuads);

pub struct DrawChunkQuads;

impl<P: PhaseItem> RenderCommand<P> for DrawChunkQuads {
    type Param = ();
    type ViewQuery = ();
    type ItemQuery = Read<GpuChunkQuads>;

    fn render<'w>(
        _item: &P,
        _view: ROQueryItem<'w, '_, Self::ViewQuery>,
        gpu: Option<ROQueryItem<'w, '_, Self::ItemQuery>>,
        _param: SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let Some(gpu) = gpu else {
            return RenderCommandResult::Skip;
        };
        pass.set_bind_group(1, &gpu.bind_group, &[]);
        // Six vertices per quad; the vertex shader pulls geometry from group 1.
        pass.draw(0..gpu.quad_count * 6, 0..1);
        RenderCommandResult::Success
    }
}

pub fn queue_voxel_pull(
    pipeline: Res<VoxelPullPipeline>,
    mut pipelines: ResMut<SpecializedRenderPipelines<VoxelPullPipeline>>,
    cache: Res<PipelineCache>,
    draw_functions: Res<DrawFunctions<Opaque3d>>,
    mut phases: ResMut<ViewBinnedRenderPhases<Opaque3d>>,
    views: Query<(
        &ExtractedView,
        &Frustum,
        &Msaa,
        Has<DepthPrepass>,
        Has<NormalPrepass>,
        Has<MotionVectorPrepass>,
        Has<DeferredPrepass>,
    )>,
    chunks: Query<(Entity, &MainEntity, &ExtractedChunkQuads)>,
    mut next_tick: Local<Tick>,
) {
    let draw = draw_functions.read().id::<DrawVoxelPull>();
    for (view, frustum, msaa, depth_prepass, normal_prepass, motion_prepass, deferred_prepass) in
        &views
    {
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else {
            continue;
        };
        // Derive the group(0) view-layout key from the view's prepass *components*,
        // exactly as `check_views_need_specialization` builds the mesh pipeline key
        // (bevy_pbr-0.18.1/src/render/mesh.rs) — not from `ViewPrepassTextures`,
        // which isn't populated until Prepare (after this Queue step). Our camera
        // runs `DepthPrepass`, so this adds the `DEPTH_PREPASS` bit and the
        // pipeline's group(0) matches the `mesh_view_bind_group` (built for the
        // `_depth` layout) that `SetMeshViewBindGroup<0>` binds. (OIT / atmosphere
        // bits aren't derived — no camera in this app uses them.)
        let mut view_layout_key = MeshPipelineViewLayoutKey::from(*msaa);
        view_layout_key.set(MeshPipelineViewLayoutKey::DEPTH_PREPASS, depth_prepass);
        view_layout_key.set(MeshPipelineViewLayoutKey::NORMAL_PREPASS, normal_prepass);
        view_layout_key.set(MeshPipelineViewLayoutKey::MOTION_VECTOR_PREPASS, motion_prepass);
        view_layout_key.set(MeshPipelineViewLayoutKey::DEFERRED_PREPASS, deferred_prepass);
        let pipeline_id = pipelines.specialize(
            &cache,
            &pipeline,
            VoxelPullPipelineKey {
                msaa_samples: msaa.samples(),
                hdr: view.hdr,
                view_layout_key,
            },
        );
        for (entity, main_entity, chunk) in &chunks {
            // center/half_extents are already world-space (extraction folded the
            // chunk transform in), so the frustum test runs against identity.
            let aabb = Aabb {
                center: chunk.center.into(),
                half_extents: chunk.half_extents.into(),
            };
            if !frustum.intersects_obb_identity(&aabb) {
                continue;
            }
            // Bump the change tick so the bin is rebuilt (mirrors the example).
            let this_tick = next_tick.get() + 1;
            next_tick.set(this_tick);
            phase.add(
                Opaque3dBatchSetKey {
                    pipeline: pipeline_id,
                    draw_function: draw,
                    material_bind_group_index: None,
                    lightmap_slab: None,
                    vertex_slab: default(),
                    index_slab: None,
                },
                Opaque3dBinKey {
                    asset_id: AssetId::<Mesh>::invalid().untyped(),
                },
                (entity, *main_entity),
                InputUniformIndex::default(),
                BinnedRenderPhaseType::NonMesh,
                *next_tick,
            );
        }
    }
}

/// Depth-prepass draw chain for one pulled chunk: bind the depth pipeline, the
/// PREPASS view bind group at group 0 (a different layout from the forward
/// pass's mesh view bind group), then reuse `DrawChunkQuads` to bind group 1 and
/// issue the `quad_count * 6` vertex draw. The vertex-only shader writes depth
/// but no color.
pub type DrawVoxelPullDepth = (SetItemPipeline, SetPrepassViewBindGroup<0>, DrawChunkQuads);

/// Queue pulled chunks into the depth prepass so the camera's HZB sees terrain.
/// Mirrors `queue_voxel_pull` (same per-view frustum cull, same `NonMesh` bin),
/// but targets `Opaque3dPrepass` with the depth pipeline and prepass draw chain.
pub fn queue_voxel_pull_prepass(
    pipeline: Res<VoxelPullDepthPipeline>,
    mut pipelines: ResMut<SpecializedRenderPipelines<VoxelPullDepthPipeline>>,
    cache: Res<PipelineCache>,
    draw_functions: Res<DrawFunctions<Opaque3dPrepass>>,
    mut phases: ResMut<ViewBinnedRenderPhases<Opaque3dPrepass>>,
    views: Query<(&ExtractedView, &Frustum, &Msaa)>,
    chunks: Query<(Entity, &MainEntity, &ExtractedChunkQuads)>,
    mut next_tick: Local<Tick>,
) {
    let draw = draw_functions.read().id::<DrawVoxelPullDepth>();
    for (view, frustum, msaa) in &views {
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else {
            continue; // this view runs no prepass
        };
        let pipeline_id = pipelines.specialize(
            &cache,
            &pipeline,
            VoxelPullDepthPipelineKey { msaa_samples: msaa.samples() },
        );
        for (entity, main_entity, chunk) in &chunks {
            let aabb = Aabb {
                center: chunk.center.into(),
                half_extents: chunk.half_extents.into(),
            };
            if !frustum.intersects_obb_identity(&aabb) {
                continue;
            }
            let this_tick = next_tick.get() + 1;
            next_tick.set(this_tick);
            phase.add(
                OpaqueNoLightmap3dBatchSetKey {
                    pipeline: pipeline_id,
                    draw_function: draw,
                    material_bind_group_index: None,
                    vertex_slab: default(),
                    index_slab: None,
                },
                OpaqueNoLightmap3dBinKey {
                    asset_id: AssetId::<Mesh>::invalid().untyped(),
                },
                (entity, *main_entity),
                InputUniformIndex::default(),
                BinnedRenderPhaseType::NonMesh,
                *next_tick,
            );
        }
    }
}
