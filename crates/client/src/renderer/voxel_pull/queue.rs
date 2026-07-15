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
        system::{
            lifetimeless::{Read, SRes},
            SystemParamItem,
        },
    },
    pbr::{
        LightEntity, MeshPipelineViewLayoutKey, SetMeshViewBindGroup, SetPrepassViewBindGroup,
        Shadow, ShadowBatchSetKey, ShadowBinKey, ViewLightEntities,
    },
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

use super::arena::GpuQuadArena;
use super::pipeline::{
    VoxelPullDepthPipeline, VoxelPullDepthPipelineKey, VoxelPullPipeline, VoxelPullPipelineKey,
};
use super::prepare::GpuChunkQuads;
use super::ExtractedChunkQuads;

/// The draw chain for one pulled chunk: bind the pipeline, bind the mesh view
/// bind group at group 0, then bind the shared arena at group 1 and issue the draw.
pub type DrawVoxelPull = (SetItemPipeline, SetMeshViewBindGroup<0>, DrawChunkQuads);

/// The per-view frustum test for one extracted chunk, mirroring bevy's own
/// mesh culling (`bevy_camera visibility/mod.rs:672`): test the near plane but
/// NOT the far plane. The GPU projection is infinite-reverse-Z — nothing is
/// far-clipped at draw time — so `PerspectiveProjection::far` (default 1000)
/// exists only as a legacy frustum field, and culling against it here silently
/// deleted every chunk and LOD ring beyond 1km while the mesh path (which
/// never far-culls) rendered them. `center`/`half_extents` are world-space
/// (extraction folded the chunk transform in), so the OBB test runs against
/// identity.
pub fn chunk_in_view(frustum: &Frustum, chunk_center: Vec3, chunk_half_extents: Vec3) -> bool {
    let aabb = Aabb {
        center: chunk_center.into(),
        half_extents: chunk_half_extents.into(),
    };
    frustum.intersects_obb(&aabb, &bevy::math::Affine3A::IDENTITY, true, false)
}

pub struct DrawChunkQuads;

impl<P: PhaseItem> RenderCommand<P> for DrawChunkQuads {
    type Param = SRes<GpuQuadArena>;
    type ViewQuery = ();
    type ItemQuery = Read<GpuChunkQuads>;

    fn render<'w>(
        _item: &P,
        _view: ROQueryItem<'w, '_, Self::ViewQuery>,
        gpu: Option<ROQueryItem<'w, '_, Self::ItemQuery>>,
        arena: SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let Some(gpu) = gpu else {
            return RenderCommandResult::Skip;
        };
        let arena = arena.into_inner();
        let Some(bind_group) = &arena.bind_group else {
            return RenderCommandResult::Skip;
        };
        pass.set_bind_group(1, bind_group, &[]);
        // Six vertices per quad; the vertex shader pulls geometry from group 1.
        pass.draw(
            gpu.record_start * 6..(gpu.record_start + gpu.quad_count) * 6,
            gpu.chunk_slot..gpu.chunk_slot + 1,
        );
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
    mut debug_frame: Local<u32>,
) {
    *debug_frame = debug_frame.wrapping_add(1);
    let debug_this_frame = *debug_frame % 60 == 0 && std::env::var_os("VOXEL_PULL_DEBUG").is_some();
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
        view_layout_key.set(
            MeshPipelineViewLayoutKey::MOTION_VECTOR_PREPASS,
            motion_prepass,
        );
        view_layout_key.set(
            MeshPipelineViewLayoutKey::DEFERRED_PREPASS,
            deferred_prepass,
        );
        let pipeline_id = pipelines.specialize(
            &cache,
            &pipeline,
            VoxelPullPipelineKey {
                msaa_samples: msaa.samples(),
                hdr: view.hdr,
                view_layout_key,
            },
        );
        let mut total = 0u32;
        let mut kept = 0u32;
        for (entity, main_entity, chunk) in &chunks {
            total += 1;
            if !chunk_in_view(frustum, chunk.center, chunk.half_extents) {
                continue;
            }
            kept += 1;
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
        if debug_this_frame {
            info!(
                "[pull-cull] forward view {:?}: {kept}/{total} chunks in frustum",
                view.retained_view_entity
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
            VoxelPullDepthPipelineKey {
                msaa_samples: msaa.samples(),
                // The camera depth prepass uses a perspective projection — never
                // unclipped. Only directional shadow cascades set this.
                unclipped_depth_ortho: false,
            },
        );
        for (entity, main_entity, chunk) in &chunks {
            if !chunk_in_view(frustum, chunk.center, chunk.half_extents) {
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

/// Queue pulled chunks into every shadow-map view so terrain casts shadows.
/// Mirrors `queue_shadows` (bevy_pbr-0.18.1/src/render/light.rs): iterate each
/// camera's `ViewLightEntities` (the per-cascade / per-face light views created
/// by `prepare_lights`), look up the `Shadow` phase keyed by the light view's
/// `retained_view_entity`, and add items with the depth draw chain.
///
/// Unlike `queue_shadows`, which walks bevy's precomputed per-cascade visible-mesh
/// lists, mesh-less chunks aren't in those lists, so we frustum-cull every chunk
/// against the light view's `Frustum` ourselves (the same world-AABB test the
/// forward/prepass queues use). Directional cascade views render with unclipped
/// depth — flagged via `LightEntity::Directional` into the pipeline key, matching
/// the `UNCLIPPED_DEPTH_ORTHO` bit bevy sets in `check_views_lights_need_specialization`.
pub fn queue_voxel_pull_shadows(
    pipeline: Res<VoxelPullDepthPipeline>,
    mut pipelines: ResMut<SpecializedRenderPipelines<VoxelPullDepthPipeline>>,
    cache: Res<PipelineCache>,
    draw_functions: Res<DrawFunctions<Shadow>>,
    mut shadow_phases: ResMut<ViewBinnedRenderPhases<Shadow>>,
    view_lights: Query<&ViewLightEntities, With<ExtractedView>>,
    // Each light view carries its LightEntity kind, ExtractedView (for the retained
    // id keying the shadow phase), and Frustum (for culling), inserted together in
    // `prepare_lights`.
    light_views: Query<(&LightEntity, &ExtractedView, &Frustum)>,
    chunks: Query<(Entity, &MainEntity, &ExtractedChunkQuads)>,
    mut next_tick: Local<Tick>,
) {
    let draw = draw_functions.read().id::<DrawVoxelPullDepth>();
    for view_lights in &view_lights {
        for light_view_entity in view_lights.lights.iter().copied() {
            let Ok((light_entity, light_view, frustum)) = light_views.get(light_view_entity) else {
                continue;
            };
            let Some(phase) = shadow_phases.get_mut(&light_view.retained_view_entity) else {
                continue;
            };
            let is_directional = matches!(light_entity, LightEntity::Directional { .. });
            // Shadow maps are single-sampled (light.rs allocates them at sample_count 1).
            let pipeline_id = pipelines.specialize(
                &cache,
                &pipeline,
                VoxelPullDepthPipelineKey {
                    msaa_samples: 1,
                    unclipped_depth_ortho: is_directional,
                },
            );
            for (entity, main_entity, chunk) in &chunks {
                if !chunk_in_view(frustum, chunk.center, chunk.half_extents) {
                    continue;
                }
                let this_tick = next_tick.get() + 1;
                next_tick.set(this_tick);
                phase.add(
                    ShadowBatchSetKey {
                        pipeline: pipeline_id,
                        draw_function: draw,
                        material_bind_group_index: None,
                        vertex_slab: default(),
                        index_slab: None,
                    },
                    ShadowBinKey {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera frustum built the way `update_frusta` builds the real one:
    /// infinite-reverse-Z perspective clip matrix, far half-space patched in
    /// from `PerspectiveProjection::far` (bevy's `Projection::compute_frustum`
    /// via `from_clip_from_world_custom_far`). Camera at origin looking -Z.
    fn camera_frustum(far: f32) -> Frustum {
        let clip_from_view =
            Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_2, 16.0 / 9.0, 0.1);
        let view_from_world = Mat4::IDENTITY; // camera at origin, -Z forward
        let clip_from_world = clip_from_view * view_from_world;
        Frustum::from_clip_from_world_custom_far(
            &clip_from_world,
            &Vec3::ZERO,
            &Vec3::Z, // view_backward for a -Z-forward camera
            far,
        )
    }

    /// The bug this pins: the GPU projection is infinite-reverse-Z (no far
    /// clip), so terrain beyond `projection.far` (default 1000) renders on the
    /// mesh path — the pull path's cull must not delete it. The old
    /// `intersects_obb_identity` call tested the far plane and blanked every
    /// chunk and LOD ring past 1km.
    #[test]
    fn chunks_beyond_projection_far_still_draw() {
        let frustum = camera_frustum(1000.0);
        let half = Vec3::splat(16.0);
        // 2km straight ahead: beyond `far`, squarely in view.
        assert!(
            chunk_in_view(&frustum, Vec3::new(0.0, 0.0, -2000.0), half),
            "chunk beyond projection.far must not be culled (GPU never far-clips)"
        );
        // Same chunk under the old all-planes test: culled — the regression.
        let aabb = Aabb {
            center: Vec3::new(0.0, 0.0, -2000.0).into(),
            half_extents: half.into(),
        };
        assert!(
            !frustum.intersects_obb_identity(&aabb),
            "sanity: the old identity test far-culls this chunk (why the bug existed)"
        );
        // LOD-root sized box (factor 256 tile: half ~4096) far out but in view.
        assert!(
            chunk_in_view(
                &frustum,
                Vec3::new(0.0, 0.0, -20_000.0),
                Vec3::splat(4096.0)
            ),
            "distant LOD roots must survive the cull"
        );
    }

    /// The cull must still reject what is genuinely outside the view: behind
    /// the camera (near plane) and far off to the side (side planes).
    #[test]
    fn chunks_outside_the_view_are_still_culled() {
        let frustum = camera_frustum(1000.0);
        let half = Vec3::splat(16.0);
        assert!(
            !chunk_in_view(&frustum, Vec3::new(0.0, 0.0, 500.0), half),
            "chunk behind the camera must be culled"
        );
        assert!(
            !chunk_in_view(&frustum, Vec3::new(10_000.0, 0.0, -100.0), half),
            "chunk far outside the side planes must be culled"
        );
        // Equivalence on the in-range case: identity test and ours agree for a
        // chunk inside `far`, pinning that skipping the far plane changed
        // nothing else about the test.
        let near_center = Vec3::new(0.0, 0.0, -100.0);
        let aabb = Aabb {
            center: near_center.into(),
            half_extents: half.into(),
        };
        assert_eq!(
            chunk_in_view(&frustum, near_center, half),
            frustum.intersects_obb_identity(&aabb),
            "inside projection.far the two tests must agree"
        );
    }
}
