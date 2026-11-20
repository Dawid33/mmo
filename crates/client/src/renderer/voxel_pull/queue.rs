//! Opaque3d queue + custom draw command for the pulled render path. Structure
//! mirrors bevy-0.18.1/examples/shader_advanced/custom_phase_item.rs.
//!
//! Each view queues two items: the chunks' opaque record ranges (`Opaque3d`,
//! `Opaque3dPrepass`, `Shadow`) and their cutout ranges (`AlphaMask3d`,
//! `AlphaMask3dPrepass`, and a second `Shadow` item), the latter with the
//! `cutout` pipeline variants (alpha-tested leaves, `records::CUTOUT_BIT`).

use crate::session::{FrontendCamera, view_draws_world};
use bevy::{
    camera::primitives::{Aabb, Frustum},
    core_pipeline::{
        core_3d::{AlphaMask3d, Opaque3d, Opaque3dBatchSetKey, Opaque3dBinKey},
        prepass::{
            AlphaMask3dPrepass, Opaque3dPrepass, OpaqueNoLightmap3dBatchSetKey,
            OpaqueNoLightmap3dBinKey,
        },
    },
    ecs::{
        query::ROQueryItem,
        system::{
            SystemParamItem,
            lifetimeless::{Read, SRes},
        },
    },
    pbr::{
        LightEntity, MeshPipelineViewLayoutKey, SetMeshViewBindGroup, SetPrepassViewBindGroup,
        Shadow, ShadowBatchSetKey, ShadowBinKey, ViewKeyCache, ViewLightEntities,
    },
    prelude::*,
    render::{
        render_phase::{
            BinnedPhaseItem, BinnedRenderPhase, BinnedRenderPhaseType, DrawFunctions,
            InputUniformIndex, PhaseItem, RenderCommand, RenderCommandResult, SetItemPipeline,
            TrackedRenderPass, ViewBinnedRenderPhases,
        },
        render_resource::{
            CachedRenderPipelineId, IndexFormat, PipelineCache, SpecializedRenderPipeline,
            SpecializedRenderPipelines,
        },
        sync_world::MainEntity,
        view::ExtractedView,
    },
};

use super::super::fortress_view::{FortressViewUniform, cutaway_active};
use super::arena::GpuQuadArena;
use super::gpu::{
    ActiveNearMode, CUTOUT_COMMANDS_PER_SLOT, CUTOUT_COUNT_OFFSET, FACES_PER_SLOT,
    GpuNearViewBuffers,
};
use super::pipeline::{
    VoxelPullDepthPipeline, VoxelPullDepthPipelineKey, VoxelPullPipeline, VoxelPullPipelineKey,
};
use super::prepare::GpuChunkQuads;
use super::xray::{InteriorChunks, SetXrayBindGroup, XraySettings, view_allows_xray, xray_wanted};

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
/// group(2) is the x-ray's interior depth (or its stand-in).
pub type DrawVoxelPull = (
    SetItemPipeline,
    SetMeshViewBindGroup<0>,
    SetXrayBindGroup<2>,
    DrawChunkQuads<false>,
);

/// [`DrawVoxelPull`] for the chunks' cutout ranges (`AlphaMask3d`).
pub type DrawVoxelPullCutout = (
    SetItemPipeline,
    SetMeshViewBindGroup<0>,
    SetXrayBindGroup<2>,
    DrawChunkQuads<true>,
);

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

/// Draws every chunk's opaque record range, or (`CUTOUT`) its cutout range.
pub struct DrawChunkQuads<const CUTOUT: bool>;

impl<P: PhaseItem, const CUTOUT: bool> RenderCommand<P> for DrawChunkQuads<CUTOUT> {
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
            // The cull wrote the opaque commands first, then the cutout ones.
            let (offset, max_commands, count_offset) = if CUTOUT {
                (
                    gpu_view.cutout_command_offset(),
                    gpu_view.slot_count * CUTOUT_COMMANDS_PER_SLOT,
                    CUTOUT_COUNT_OFFSET,
                )
            } else {
                (0, gpu_view.slot_count * FACES_PER_SLOT, 0)
            };
            if indirect_count {
                pass.multi_draw_indexed_indirect_count(
                    &gpu_view.indirect,
                    offset,
                    &gpu_view.count,
                    count_offset,
                    max_commands,
                );
            } else {
                pass.multi_draw_indexed_indirect(&gpu_view.indirect, offset, max_commands);
            }
            return RenderCommandResult::Success;
        }
        for gpu in &draws.0 {
            let (first, quads) = gpu.range(CUTOUT);
            if quads == 0 {
                continue;
            }
            // The vertex shader pulls geometry from group 1.
            pass.draw_indexed(
                0..quads * 6,
                (first * 4) as i32,
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

/// Specialize `key` with the cutaway bit set to `cutaway`, but while a
/// wanted CUTAWAY variant is still compiling keep drawing with the plain one
/// — a not-yet-ready pipeline skips its phase item, which would blank the
/// terrain for the frames after the cut is first toggled. Never the other
/// way: the plain variant is not replaced by a CUTAWAY one.
fn specialize_cutaway<P: SpecializedRenderPipeline>(
    cache: &PipelineCache,
    pipelines: &mut SpecializedRenderPipelines<P>,
    pipeline: &P,
    cutaway: bool,
    key: impl Fn(bool) -> P::Key,
) -> CachedRenderPipelineId {
    let wanted = pipelines.specialize(cache, pipeline, key(cutaway));
    if !cutaway || cache.get_render_pipeline(wanted).is_some() {
        return wanted;
    }
    let plain = pipelines.specialize(cache, pipeline, key(false));
    if cache.get_render_pipeline(plain).is_some() {
        plain
    } else {
        wanted
    }
}

/// Put a view's one representative `NonMesh` item (its render command draws
/// every chunk) in the bin for `batch_set_key`. Bevy's binned phases are
/// RETAINED across frames, and `BinnedRenderPhase::add` never takes an entity
/// out of the bin it was in before — Bevy's own `queue_material_meshes`
/// removes before it re-adds. Without the remove, the item queued with the
/// plain pipeline (cut off, or CUTAWAY still compiling) stayed in that bin
/// after the cut switched to CUTAWAY, so the terrain drew twice: uncut over
/// cut, its tops on the cut z-fighting the slice plane.
fn bin_view_item<BPI: BinnedPhaseItem>(
    phase: &mut BinnedRenderPhase<BPI>,
    batch_set_key: BPI::BatchSetKey,
    bin_key: BPI::BinKey,
    view_entity: Entity,
) {
    bin_view_item_as(
        phase,
        batch_set_key,
        bin_key,
        view_entity,
        MainEntity::from(view_entity),
    );
}

/// The key a view's SECOND item in one phase is binned under (its cutout
/// item in `Shadow`, which holds both): the bin is keyed by `MainEntity`, so
/// the two items need distinct ones or re-binning one would remove the
/// other. The view's own id with its generation pushed half the range on —
/// like the first item's key (a render entity used as a main entity), a
/// stand-in no real main-world entity takes.
fn second_item_key(view_entity: Entity) -> MainEntity {
    MainEntity::from(Entity::from_index_and_generation(
        view_entity.index(),
        view_entity.generation().after_versions(1 << 31),
    ))
}

fn bin_view_item_as<BPI: BinnedPhaseItem>(
    phase: &mut BinnedRenderPhase<BPI>,
    batch_set_key: BPI::BatchSetKey,
    bin_key: BPI::BinKey,
    view_entity: Entity,
    main_entity: MainEntity,
) {
    phase.remove(main_entity);
    phase.add(
        batch_set_key,
        bin_key,
        (view_entity, main_entity),
        InputUniformIndex::default(),
        BinnedRenderPhaseType::NonMesh,
    );
}

pub fn queue_voxel_pull(
    mode: Res<ActiveNearMode>,
    pipeline: Res<VoxelPullPipeline>,
    mut pipelines: ResMut<SpecializedRenderPipelines<VoxelPullPipeline>>,
    cache: Res<PipelineCache>,
    view_key_cache: Res<ViewKeyCache>,
    fortress_view: Option<Res<FortressViewUniform>>,
    xray_settings: Option<Res<XraySettings>>,
    interior_chunks: Res<InteriorChunks>,
    draw_functions: Res<DrawFunctions<Opaque3d>>,
    cutout_draw_functions: Res<DrawFunctions<AlphaMask3d>>,
    mut phases: ResMut<ViewBinnedRenderPhases<Opaque3d>>,
    mut cutout_phases: ResMut<ViewBinnedRenderPhases<AlphaMask3d>>,
    views: Query<(Entity, &ExtractedView, &Msaa, Has<FrontendCamera>)>,
) {
    if *mode == ActiveNearMode::Disabled {
        return;
    }
    let draw = draw_functions.read().id::<DrawVoxelPull>();
    let cutout_draw = cutout_draw_functions.read().id::<DrawVoxelPullCutout>();
    let cutaway = cutaway_wanted(fortress_view.as_deref());
    let xray_any = xray_wanted(
        xray_settings.as_deref(),
        fortress_view.as_deref(),
        &interior_chunks,
    );
    for (view_entity, view, msaa, frontend) in &views {
        if !view_draws_world(frontend) {
            continue;
        }
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
        let xray = xray_any && view_allows_xray(view_layout_key);
        // `special`: the wanted variant (CUTAWAY or XRAY, never both); the
        // plain one stands in while it compiles (`specialize_cutaway`).
        let key = |cutout: bool| {
            move |special: bool| VoxelPullPipelineKey {
                msaa_samples: msaa.samples(),
                target_format: view.target_format,
                view_layout_key,
                cutaway: special && cutaway,
                cutout,
                xray: special && xray,
                interior_pass: false,
            }
        };
        let special = cutaway || xray;
        let pipeline_id =
            specialize_cutaway(&cache, &mut pipelines, &pipeline, special, key(false));
        // One representative NonMesh item per view. Its render command loops
        // `ViewChunkDrawList`, so chunk count no longer inflates phase bins.
        bin_view_item(
            phase,
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
            view_entity,
        );
        // The cutout ranges, alpha-tested, after the opaque ones.
        if let Some(cutout_phase) = cutout_phases.get_mut(&view.retained_view_entity) {
            let pipeline_id =
                specialize_cutaway(&cache, &mut pipelines, &pipeline, special, key(true));
            bin_view_item(
                cutout_phase,
                OpaqueNoLightmap3dBatchSetKey {
                    pipeline: pipeline_id,
                    draw_function: cutout_draw,
                    material_bind_group_index: None,
                    slabs: default(),
                },
                OpaqueNoLightmap3dBinKey {
                    asset_id: AssetId::<Mesh>::invalid().untyped(),
                },
                view_entity,
            );
        }
    }
}

/// Depth-prepass draw chain for one pulled chunk: bind the depth pipeline, the
/// PREPASS view bind group at group 0 (a different layout from the forward
/// pass's mesh view bind group), then reuse `DrawChunkQuads` to bind group 1 and
/// issue the `quad_count * 6` vertex draw. The vertex-only shader writes depth
/// but no color.
pub type DrawVoxelPullDepth = (
    SetItemPipeline,
    SetPrepassViewBindGroup<0>,
    DrawChunkQuads<false>,
);

/// [`DrawVoxelPullDepth`] for the chunks' cutout ranges (`AlphaMask3dPrepass`
/// and the cutout `Shadow` item), with the alpha-testing depth pipeline.
pub type DrawVoxelPullDepthCutout = (
    SetItemPipeline,
    SetPrepassViewBindGroup<0>,
    DrawChunkQuads<true>,
);

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
    cutout_draw_functions: Res<DrawFunctions<AlphaMask3dPrepass>>,
    mut phases: ResMut<ViewBinnedRenderPhases<Opaque3dPrepass>>,
    mut cutout_phases: ResMut<ViewBinnedRenderPhases<AlphaMask3dPrepass>>,
    views: Query<(Entity, &ExtractedView, &Msaa, Has<FrontendCamera>)>,
) {
    if *mode == ActiveNearMode::Disabled {
        return;
    }
    let draw = draw_functions.read().id::<DrawVoxelPullDepth>();
    let cutout_draw = cutout_draw_functions
        .read()
        .id::<DrawVoxelPullDepthCutout>();
    let cutaway = cutaway_wanted(fortress_view.as_deref());
    for (view_entity, view, msaa, frontend) in &views {
        if !view_draws_world(frontend) {
            continue;
        }
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else {
            continue; // this view runs no prepass
        };
        let key = |cutout: bool| {
            move |cutaway| VoxelPullDepthPipelineKey {
                msaa_samples: msaa.samples(),
                // The camera depth prepass uses a perspective projection —
                // never unclipped. Only directional shadow cascades set this.
                unclipped_depth_ortho: false,
                cutaway,
                cutout,
            }
        };
        let pipeline_id =
            specialize_cutaway(&cache, &mut pipelines, &pipeline, cutaway, key(false));
        bin_view_item(
            phase,
            OpaqueNoLightmap3dBatchSetKey {
                pipeline: pipeline_id,
                draw_function: draw,
                material_bind_group_index: None,
                slabs: default(),
            },
            OpaqueNoLightmap3dBinKey {
                asset_id: AssetId::<Mesh>::invalid().untyped(),
            },
            view_entity,
        );
        if let Some(cutout_phase) = cutout_phases.get_mut(&view.retained_view_entity) {
            let pipeline_id =
                specialize_cutaway(&cache, &mut pipelines, &pipeline, cutaway, key(true));
            bin_view_item(
                cutout_phase,
                OpaqueNoLightmap3dBatchSetKey {
                    pipeline: pipeline_id,
                    draw_function: cutout_draw,
                    material_bind_group_index: None,
                    slabs: default(),
                },
                OpaqueNoLightmap3dBinKey {
                    asset_id: AssetId::<Mesh>::invalid().untyped(),
                },
                view_entity,
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
    mode: Res<ActiveNearMode>,
    pipeline: Res<VoxelPullDepthPipeline>,
    mut pipelines: ResMut<SpecializedRenderPipelines<VoxelPullDepthPipeline>>,
    cache: Res<PipelineCache>,
    fortress_view: Option<Res<FortressViewUniform>>,
    draw_functions: Res<DrawFunctions<Shadow>>,
    mut shadow_phases: ResMut<ViewBinnedRenderPhases<Shadow>>,
    // Not the frontend camera's cascades: it draws no terrain.
    view_lights: Query<&ViewLightEntities, (With<ExtractedView>, Without<FrontendCamera>)>,
    // Each light view carries its LightEntity kind, ExtractedView (for the retained
    // id keying the shadow phase), and Frustum (for culling), inserted together in
    // `prepare_lights`.
    light_views: Query<(&LightEntity, &ExtractedView)>,
) {
    if *mode == ActiveNearMode::Disabled {
        return;
    }
    let draw = draw_functions.read().id::<DrawVoxelPullDepth>();
    let cutout_draw = draw_functions.read().id::<DrawVoxelPullDepthCutout>();
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
            let key = |cutout: bool| {
                move |cutaway| VoxelPullDepthPipelineKey {
                    msaa_samples: 1,
                    unclipped_depth_ortho: is_directional,
                    cutaway,
                    cutout,
                }
            };
            let pipeline_id =
                specialize_cutaway(&cache, &mut pipelines, &pipeline, cutaway, key(false));
            bin_view_item(
                phase,
                ShadowBatchSetKey {
                    pipeline: pipeline_id,
                    draw_function: draw,
                    material_bind_group_index: None,
                    slabs: default(),
                },
                ShadowBinKey {
                    asset_id: AssetId::<Mesh>::invalid().untyped(),
                },
                light_view_entity,
            );
            // Leaves cast dappled shadows: the cutout ranges, alpha-tested,
            // as a second item of the same phase.
            let pipeline_id =
                specialize_cutaway(&cache, &mut pipelines, &pipeline, cutaway, key(true));
            bin_view_item_as(
                phase,
                ShadowBatchSetKey {
                    pipeline: pipeline_id,
                    draw_function: cutout_draw,
                    material_bind_group_index: None,
                    slabs: default(),
                },
                ShadowBinKey {
                    asset_id: AssetId::<Mesh>::invalid().untyped(),
                },
                light_view_entity,
                second_item_key(light_view_entity),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: terrain drawn twice under the cutaway, slice-plane normal/dimmed flicker seen on screen (07eba551)
    /// The view's item follows the pipeline it was last queued with: when the
    /// z-level cut turns on (plain -> CUTAWAY, through the compile fallback)
    /// or off, it must leave the old pipeline's bin, else the terrain is
    /// drawn by both — uncut over cut, the slice plane z-fighting the tops.
    #[test]
    fn view_item_leaves_its_previous_pipeline_bin() {
        use bevy::render::{
            batching::gpu_preprocessing::GpuPreprocessingMode, render_phase::DrawFunctionId,
            view::RetainedViewEntity,
        };
        let view = Entity::from_raw_u32(7).unwrap();
        let retained = RetainedViewEntity::new(MainEntity::from(view), None, 0);
        let mut phases = ViewBinnedRenderPhases::<Opaque3d>::default();
        let key = |pipeline: usize| Opaque3dBatchSetKey {
            pipeline: CachedRenderPipelineId::new(pipeline),
            draw_function: DrawFunctionId(0),
            material_bind_group_index: None,
            lightmap_slab: None,
            slabs: default(),
        };
        let bin = || Opaque3dBinKey {
            asset_id: AssetId::<Mesh>::invalid().untyped(),
        };
        let (plain, cutaway) = (1, 2);
        // Frames: plain (cut off / CUTAWAY compiling), CUTAWAY, CUTAWAY,
        // plain again (cut toggled off).
        for (frame, pipeline) in [plain, cutaway, cutaway, plain].into_iter().enumerate() {
            phases.prepare_for_new_frame(retained, GpuPreprocessingMode::None);
            let phase = phases.get_mut(&retained).expect("phase");
            bin_view_item(phase, key(pipeline), bin(), view);
            let bins: Vec<usize> = phase
                .non_mesh_items
                .keys()
                .map(|(k, _)| k.pipeline.id())
                .collect();
            assert_eq!(
                bins,
                [pipeline],
                "frame {frame}: one bin, the current pipeline's"
            );
        }
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
        Frustum(
            bevy::math::primitives::ViewFrustum::from_clip_from_world_custom_far(
                &clip_from_world,
                &Vec3::ZERO,
                &Vec3::Z, // view_backward for a -Z-forward camera
                far,
            ),
        )
    }

    /// Regression: world blanked past 1 km, reported "only the blocks next to the player render" (99b751b7)
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
}
