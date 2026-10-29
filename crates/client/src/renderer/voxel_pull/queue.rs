//! Opaque3d queue + custom draw command for the pulled render path. Structure
//! mirrors bevy-0.18.1/examples/shader_advanced/custom_phase_item.rs.

use bevy::{
    camera::primitives::{Aabb, Frustum},
    core_pipeline::{
        core_3d::{Opaque3d, Opaque3dBatchSetKey, Opaque3dBinKey},
        prepass::{
            Opaque3dPrepass,
            OpaqueNoLightmap3dBatchSetKey, OpaqueNoLightmap3dBinKey,
        },
    },
    ecs::{
        query::ROQueryItem,
        system::{
            lifetimeless::{Read, SRes},
            SystemParamItem,
        },
    },
    pbr::{
        LightEntity, MeshPipelineViewLayoutKey, SetMeshViewBindGroup, SetPrepassViewBindGroup,
        Shadow, ShadowBatchSetKey, ShadowBinKey, ViewKeyCache, ViewLightEntities,
    },
    prelude::*,
    render::{
        render_phase::{
            BinnedRenderPhaseType, DrawFunctions, InputUniformIndex, PhaseItem, RenderCommand,
            RenderCommandResult, SetItemPipeline, TrackedRenderPass, ViewBinnedRenderPhases,
        },
        render_resource::{
            CachedRenderPipelineId, IndexFormat, PipelineCache, SpecializedRenderPipeline,
            SpecializedRenderPipelines,
        },
        sync_world::MainEntity,
        view::ExtractedView,
    },
};

use super::super::fortress_view::{cutaway_active, FortressViewUniform};
use super::arena::GpuQuadArena;
use super::gpu::{ActiveNearMode, GpuNearViewBuffers, FACES_PER_SLOT};
use super::pipeline::{
    VoxelPullDepthPipeline, VoxelPullDepthPipelineKey, VoxelPullPipeline, VoxelPullPipelineKey,
};
use super::prepare::GpuChunkQuads;

/// View-owned CPU-visible draws used by the grouped-direct fallback. A single
/// NonMesh phase item reads this component and emits the contained direct draws.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewChunkDrawList(pub Vec<GpuChunkQuads>);

/// Small pure model used to pin phase-item grouping independently of Bevy's
/// phase internals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupedPhasePlan {
    view_count: usize,
    visible_chunks_per_view: usize,
}

impl GroupedPhasePlan {
    pub const fn new(view_count: usize, visible_chunks_per_view: usize) -> Self {
        Self {
            view_count,
            visible_chunks_per_view,
        }
    }

    pub const fn phase_item_count(self) -> usize {
        self.view_count
    }

    pub const fn direct_draw_count(self) -> usize {
        self.view_count * self.visible_chunks_per_view
    }
}

pub fn grouped_draw_list(
    frustum: &Frustum,
    chunks: impl IntoIterator<Item = (GpuChunkQuads, Vec3, Vec3, bool)>,
) -> ViewChunkDrawList {
    ViewChunkDrawList(
        chunks
            .into_iter()
            .filter_map(|(gpu, center, half_extents, active)| {
                (active && chunk_in_view(frustum, center, half_extents)).then_some(gpu)
            })
            .collect(),
    )
}

/// The grouped draw chain: bind the pipeline and view once, then loop the
/// view's CPU-visible chunks while the arena bind group stays bound.
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
    type Param = (SRes<GpuQuadArena>, SRes<ActiveNearMode>);
    type ViewQuery = (Read<ViewChunkDrawList>, Option<Read<GpuNearViewBuffers>>);
    type ItemQuery = ();

    fn render<'w>(
        _item: &P,
        (draws, gpu_view): ROQueryItem<'w, '_, Self::ViewQuery>,
        _item_entity: Option<ROQueryItem<'w, '_, Self::ItemQuery>>,
        (arena, mode): SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let arena = arena.into_inner();
        let mode = mode.into_inner();
        if *mode == ActiveNearMode::Disabled {
            return RenderCommandResult::Skip;
        }
        let (Some(bind_group), Some((quad_indices, _))) = (&arena.bind_group, &arena.quad_indices)
        else {
            return RenderCommandResult::Skip;
        };
        pass.set_bind_group(1, bind_group, &[]);
        // Indexed: 4 pulled vertices per quad, `QUAD_INDEX_PATTERN` from the
        // shared buffer, `base_vertex` = record_start * 4 per draw.
        pass.set_index_buffer(quad_indices.slice(..), IndexFormat::Uint32);
        if let (ActiveNearMode::GpuDriven { indirect_count }, Some(gpu_view)) = (*mode, gpu_view) {
            if indirect_count {
                pass.multi_draw_indexed_indirect_count(
                    &gpu_view.indirect,
                    0,
                    &gpu_view.count,
                    0,
                    gpu_view.slot_count * FACES_PER_SLOT,
                );
            } else {
                pass.multi_draw_indexed_indirect(
                    &gpu_view.indirect,
                    0,
                    gpu_view.slot_count * FACES_PER_SLOT,
                );
            }
            return RenderCommandResult::Success;
        }
        for gpu in &draws.0 {
            // The vertex shader pulls geometry from group 1.
            pass.draw_indexed(
                0..gpu.quad_count * 6,
                (gpu.record_start * 4) as i32,
                gpu.chunk_slot..gpu.chunk_slot + 1,
            );
        }
        RenderCommandResult::Success
    }
}

/// Whether the z-level cut is on in the extracted fortress view uniform.
fn cutaway_wanted(view: Option<&FortressViewUniform>) -> bool {
    view.is_some_and(cutaway_active)
}

/// Specialize `key` with the cutaway bit set to `cutaway`, but keep drawing
/// with the other variant while the wanted one is still compiling — a
/// not-yet-ready pipeline skips its phase item, which would blank the terrain
/// for the frames after the cut is first toggled.
fn specialize_cutaway<P: SpecializedRenderPipeline>(
    cache: &PipelineCache,
    pipelines: &mut SpecializedRenderPipelines<P>,
    pipeline: &P,
    cutaway: bool,
    key: impl Fn(bool) -> P::Key,
) -> CachedRenderPipelineId {
    let wanted = pipelines.specialize(cache, pipeline, key(cutaway));
    if cache.get_render_pipeline(wanted).is_some() {
        return wanted;
    }
    let other = pipelines.specialize(cache, pipeline, key(!cutaway));
    if cache.get_render_pipeline(other).is_some() {
        other
    } else {
        wanted
    }
}

pub fn queue_voxel_pull(
    mode: Res<ActiveNearMode>,
    pipeline: Res<VoxelPullPipeline>,
    mut pipelines: ResMut<SpecializedRenderPipelines<VoxelPullPipeline>>,
    cache: Res<PipelineCache>,
    view_key_cache: Res<ViewKeyCache>,
    fortress_view: Option<Res<FortressViewUniform>>,
    draw_functions: Res<DrawFunctions<Opaque3d>>,
    mut phases: ResMut<ViewBinnedRenderPhases<Opaque3d>>,
    views: Query<(
        Entity,
        &ExtractedView,
        &Msaa,
    )>,
) {
    if *mode == ActiveNearMode::Disabled {
        return;
    }
    let draw = draw_functions.read().id::<DrawVoxelPull>();
    let cutaway = cutaway_wanted(fortress_view.as_deref());
    for (view_entity, view, msaa) in &views
    {
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else {
            continue;
        };
        // The group(0) layout must be the one `prepare_mesh_view_bind_groups` built
        // for this view. Bevy 0.19 keys it on many view features (distance fog,
        // tonemapping, environment maps, prepasses, …); `ViewKeyCache` holds the
        // same `MeshPipelineKey` Bevy's own mesh pipelines specialize from.
        let Some(view_key) = view_key_cache.get(&view.retained_view_entity) else {
            continue;
        };
        let view_layout_key = MeshPipelineViewLayoutKey::from(*view_key);
        let pipeline_id =
            specialize_cutaway(&cache, &mut pipelines, &pipeline, cutaway, |cutaway| {
                VoxelPullPipelineKey {
                    msaa_samples: msaa.samples(),
                    target_format: view.target_format,
                    view_layout_key,
                    cutaway,
                }
            });
        // One representative NonMesh item per view. Its render command loops
        // `ViewChunkDrawList`, so chunk count no longer inflates phase bins.
        phase.add(
            Opaque3dBatchSetKey {
                pipeline: pipeline_id,
                draw_function: draw,
                material_bind_group_index: None,
                lightmap_slab: None,
                slabs: default(),
            },
            Opaque3dBinKey {
                asset_id: AssetId::<Mesh>::invalid().untyped(),
            },
            (view_entity, MainEntity::from(view_entity)),
            InputUniformIndex::default(),
            BinnedRenderPhaseType::NonMesh,
        );
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
    mode: Res<ActiveNearMode>,
    pipeline: Res<VoxelPullDepthPipeline>,
    mut pipelines: ResMut<SpecializedRenderPipelines<VoxelPullDepthPipeline>>,
    cache: Res<PipelineCache>,
    fortress_view: Option<Res<FortressViewUniform>>,
    draw_functions: Res<DrawFunctions<Opaque3dPrepass>>,
    mut phases: ResMut<ViewBinnedRenderPhases<Opaque3dPrepass>>,
    views: Query<(Entity, &ExtractedView, &Msaa)>,
) {
    if *mode == ActiveNearMode::Disabled {
        return;
    }
    let draw = draw_functions.read().id::<DrawVoxelPullDepth>();
    let cutaway = cutaway_wanted(fortress_view.as_deref());
    for (view_entity, view, msaa) in &views {
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else {
            continue; // this view runs no prepass
        };
        let pipeline_id =
            specialize_cutaway(&cache, &mut pipelines, &pipeline, cutaway, |cutaway| {
                VoxelPullDepthPipelineKey {
                    msaa_samples: msaa.samples(),
                    // The camera depth prepass uses a perspective projection —
                    // never unclipped. Only directional shadow cascades set this.
                    unclipped_depth_ortho: false,
                    cutaway,
                }
            });
        phase.add(
            OpaqueNoLightmap3dBatchSetKey {
                pipeline: pipeline_id,
                draw_function: draw,
                material_bind_group_index: None,
                slabs: default(),
            },
            OpaqueNoLightmap3dBinKey {
                asset_id: AssetId::<Mesh>::invalid().untyped(),
            },
            (view_entity, MainEntity::from(view_entity)),
            InputUniformIndex::default(),
            BinnedRenderPhaseType::NonMesh,
        );
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
    mode: Res<ActiveNearMode>,
    pipeline: Res<VoxelPullDepthPipeline>,
    mut pipelines: ResMut<SpecializedRenderPipelines<VoxelPullDepthPipeline>>,
    cache: Res<PipelineCache>,
    fortress_view: Option<Res<FortressViewUniform>>,
    draw_functions: Res<DrawFunctions<Shadow>>,
    mut shadow_phases: ResMut<ViewBinnedRenderPhases<Shadow>>,
    view_lights: Query<&ViewLightEntities, With<ExtractedView>>,
    // Each light view carries its LightEntity kind, ExtractedView (for the retained
    // id keying the shadow phase), and Frustum (for culling), inserted together in
    // `prepare_lights`.
    light_views: Query<(&LightEntity, &ExtractedView)>,
) {
    if *mode == ActiveNearMode::Disabled {
        return;
    }
    let draw = draw_functions.read().id::<DrawVoxelPullDepth>();
    let cutaway = cutaway_wanted(fortress_view.as_deref());
    for view_lights in &view_lights {
        for light_view_entity in view_lights.lights.iter().copied() {
            let Ok((light_entity, light_view)) = light_views.get(light_view_entity) else {
                continue;
            };
            let Some(phase) = shadow_phases.get_mut(&light_view.retained_view_entity) else {
                continue;
            };
            let is_directional = matches!(light_entity, LightEntity::Directional { .. });
            // Shadow maps are single-sampled (light.rs allocates them at sample_count 1).
            let pipeline_id =
                specialize_cutaway(&cache, &mut pipelines, &pipeline, cutaway, |cutaway| {
                    VoxelPullDepthPipelineKey {
                        msaa_samples: 1,
                        unclipped_depth_ortho: is_directional,
                        cutaway,
                    }
                });
            phase.add(
                ShadowBatchSetKey {
                    pipeline: pipeline_id,
                    draw_function: draw,
                    material_bind_group_index: None,
                    slabs: default(),
                },
                ShadowBinKey {
                    asset_id: AssetId::<Mesh>::invalid().untyped(),
                },
                (light_view_entity, MainEntity::from(light_view_entity)),
                InputUniformIndex::default(),
                BinnedRenderPhaseType::NonMesh,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_plan_has_one_phase_item_per_view_not_per_chunk() {
        let plan = GroupedPhasePlan::new(3, 257);

        assert_eq!(plan.phase_item_count(), 3);
        assert_eq!(plan.direct_draw_count(), 771);
    }

    #[test]
    fn grouped_draw_list_keeps_only_cpu_visible_chunks() {
        let frustum = camera_frustum(1000.0);
        let inside = GpuChunkQuads {
            record_start: 10,
            quad_count: 4,
            chunk_slot: 2,
        };
        let behind = GpuChunkQuads {
            record_start: 30,
            quad_count: 8,
            chunk_slot: 5,
        };

        let list = grouped_draw_list(
            &frustum,
            [
                (inside, Vec3::new(0.0, 0.0, -100.0), Vec3::splat(16.0), true),
                (behind, Vec3::new(0.0, 0.0, 100.0), Vec3::splat(16.0), true),
                (
                    behind,
                    Vec3::new(0.0, 0.0, -100.0),
                    Vec3::splat(16.0),
                    false,
                ),
            ],
        );

        assert_eq!(list.0, vec![inside]);
    }

    /// A camera frustum built the way `update_frusta` builds the real one:
    /// infinite-reverse-Z perspective clip matrix, far half-space patched in
    /// from `PerspectiveProjection::far` (bevy's `Projection::compute_frustum`
    /// via `from_clip_from_world_custom_far`). Camera at origin looking -Z.
    fn camera_frustum(far: f32) -> Frustum {
        let clip_from_view =
            Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_2, 16.0 / 9.0, 0.1);
        let view_from_world = Mat4::IDENTITY; // camera at origin, -Z forward
        let clip_from_world = clip_from_view * view_from_world;
        Frustum(bevy::math::primitives::ViewFrustum::from_clip_from_world_custom_far(
            &clip_from_world,
            &Vec3::ZERO,
            &Vec3::Z, // view_backward for a -Z-forward camera
            far,
        ))
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
