//! Opaque3d queue + custom draw command for the pulled render path. Structure
//! mirrors bevy-0.18.1/examples/shader_advanced/custom_phase_item.rs.

use bevy::{
    camera::primitives::{Aabb, Frustum},
    core_pipeline::core_3d::{Opaque3d, Opaque3dBatchSetKey, Opaque3dBinKey},
    ecs::{
        change_detection::Tick,
        query::ROQueryItem,
        system::{lifetimeless::Read, SystemParamItem},
    },
    pbr::SetMeshViewBindGroup,
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

use super::pipeline::{VoxelPullPipeline, VoxelPullPipelineKey};
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
    views: Query<(&ExtractedView, &Frustum, &Msaa)>,
    chunks: Query<(Entity, &MainEntity, &ExtractedChunkQuads)>,
    mut next_tick: Local<Tick>,
) {
    let draw = draw_functions.read().id::<DrawVoxelPull>();
    for (view, frustum, msaa) in &views {
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else {
            continue;
        };
        let pipeline_id = pipelines.specialize(
            &cache,
            &pipeline,
            VoxelPullPipelineKey { msaa_samples: msaa.samples(), hdr: view.hdr },
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
