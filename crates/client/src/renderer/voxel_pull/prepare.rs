//! Render-world upload for the pulled render path. Quad records and chunk
//! transforms live in two shared growable storage buffers; changed chunks
//! update only their assigned ranges in steady state.

use bevy::{
    prelude::*,
    render::{
        render_asset::RenderAssets,
        render_resource::*,
        renderer::{RenderDevice, RenderQueue},
        sync_world::MainEntity,
        texture::GpuImage,
    },
};

use super::super::meshing::records::face_ends;
use super::arena::{quad_index_capacity, quad_index_data, GpuQuadArena};
use super::gpu::{
    exact_indirect_draw_count, synchronize_near_metric_mode, ActiveNearMode, ForwardCullSource,
    NearRendererMetrics,
};
use super::super::boundaries::BoundaryUniform;
use super::super::fortress_view::{FortressViewUniform, OverlayTexture};
use super::pipeline::VoxelPullPipeline;
use super::queue::{grouped_draw_list, ViewChunkDrawList};
use super::{BlockArrayTexture, BlockTopLayers, ExtractedChunkQuads, RemovedChunkQuads};

/// Per-entity draw metadata into the shared arena. The chunk's records are
/// opaque in `0..cutout_start` and cutout (alpha-tested, own pipeline) in
/// `cutout_start..quad_count`, both relative to `record_start`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuChunkQuads {
    pub record_start: u32,
    pub quad_count: u32,
    pub chunk_slot: u32,
    pub cutout_start: u32,
}

impl GpuChunkQuads {
    /// The (first record, quad count) the opaque or the cutout pass draws.
    pub fn range(&self, cutout: bool) -> (u32, u32) {
        let split = self.cutout_start.min(self.quad_count);
        if cutout {
            (self.record_start + split, self.quad_count - split)
        } else {
            (self.record_start, split)
        }
    }
}

pub fn frustum_metric_contribution(visible_chunks: usize, is_shadow_view: bool) -> u32 {
    if is_shadow_view {
        0
    } else {
        visible_chunks.min(u32::MAX as usize) as u32
    }
}

fn draw_list_workload(draws: &ViewChunkDrawList) -> (u64, u64) {
    let quads = draws
        .0
        .iter()
        .map(|draw| u64::from(draw.quad_count))
        .fold(0u64, u64::saturating_add);
    (quads, quads.saturating_mul(6))
}

fn update_near_metrics_for_prepare(
    mode: ActiveNearMode,
    active_chunks: u32,
    frustum_visible_chunks: Option<u32>,
    resident_quads: u64,
    submitted_workload: Option<(u64, u64)>,
    direct_draw_count: Option<u32>,
    metrics: &mut NearRendererMetrics,
) {
    synchronize_near_metric_mode(mode, metrics);
    metrics.active_chunks = active_chunks;
    metrics.frustum_visible_chunks = frustum_visible_chunks;
    metrics.resident_quads = resident_quads;
    // GPU telemetry is mapped asynchronously after the render graph runs.
    // Keep its latest completed values until the readback system replaces
    // them. Direct modes have exact synchronous command counts.
    match mode {
        ActiveNearMode::GpuDriven { .. } => {
            metrics.direct_draw_count = Some(0);
        }
        ActiveNearMode::GroupedDirect => {
            metrics.visibility_source = Some(ForwardCullSource::Frustum);
            metrics.hzb_visible_chunks = None;
            metrics.indirect_draw_count = exact_indirect_draw_count(mode);
            let (quads, vertices) = submitted_workload.unwrap_or_default();
            metrics.submitted_quads = Some(quads);
            metrics.submitted_vertices = Some(vertices);
            metrics.direct_draw_count = direct_draw_count;
        }
        ActiveNearMode::Disabled => {
            metrics.hzb_visible_chunks = None;
            metrics.indirect_draw_count = exact_indirect_draw_count(mode);
            metrics.submitted_quads = None;
            metrics.submitted_vertices = None;
            metrics.direct_draw_count = None;
        }
    }
}

/// Whether GPU-driven mode still computes the CPU frustum-survivor metric
/// (`VOXEL_PULL_DEBUG`). Read once.
fn frustum_metric_wanted() -> bool {
    static WANTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WANTED.get_or_init(|| std::env::var_os("VOXEL_PULL_DEBUG").is_some())
}

/// Finalizes the view-owned grouped-direct lists after arena allocation, so a
/// chunk uploaded this frame is eligible to draw in the same frame.
pub fn prepare_view_chunk_draws(
    mut commands: Commands,
    views: Query<(
        Entity,
        &bevy::camera::primitives::Frustum,
        Has<bevy::pbr::ShadowView>,
        Has<ViewChunkDrawList>,
        Has<crate::session::FrontendCamera>,
    )>,
    chunks: Query<(&MainEntity, &ExtractedChunkQuads)>,
    arena: Res<GpuQuadArena>,
    mode: Res<ActiveNearMode>,
    mut metrics: ResMut<NearRendererMetrics>,
) {
    let active_chunks = arena
        .metadata
        .values()
        .iter()
        .filter(|metadata| metadata.cave_active != 0 && metadata.quad_count != 0)
        .count()
        .min(u32::MAX as usize) as u32;
    let mut frustum_visible_chunks = 0u32;
    let resident_quads = arena
        .allocations
        .values()
        .map(|allocation| u64::from(allocation.quad_count))
        .fold(0u64, u64::saturating_add);
    // GPU-driven draws come from the cull compute's indirect buffers and never
    // read the per-view lists; building them anyway (a frustum test per chunk
    // and LOD cell per view, five views) was 6 ms/frame at `full` (2026-09-22
    // trace) spent only on the `frustum_visible_chunks` metric — kept behind
    // VOXEL_PULL_DEBUG (compare it with the HZB survivors to see what
    // occlusion culling buys). Views still need the (empty) component
    // `DrawChunkQuads` queries.
    if matches!(*mode, ActiveNearMode::GpuDriven { .. }) && !frustum_metric_wanted() {
        for (view_entity, _, _, has_list, _) in &views {
            if !has_list {
                commands.entity(view_entity).insert(ViewChunkDrawList::default());
            }
        }
        update_near_metrics_for_prepare(
            *mode,
            active_chunks,
            None,
            resident_quads,
            None,
            None,
            &mut metrics,
        );
        return;
    }
    let mut submitted_workload =
        matches!(*mode, ActiveNearMode::GroupedDirect).then_some((0u64, 0u64));
    let mut direct_draw_count = matches!(*mode, ActiveNearMode::GroupedDirect).then_some(0u32);
    // Resolved ONCE per frame, not once per view: there are five views (the
    // camera plus four shadow cascades), and the allocation lookup per chunk
    // per view was most of this system's 18 ms/frame at the production world
    // size (2026-09-19 trace). Cave-hidden chunks are dropped here for the
    // same reason; per view only the frustum test remains. Query order is
    // stable within a frame, so every view still sees the same sequence.
    let resolved: Vec<(GpuChunkQuads, Vec3, Vec3, bool)> = chunks
        .iter()
        .filter_map(|(main_entity, chunk)| {
            if !chunk.cave_active {
                return None;
            }
            let allocation = arena.allocations.get(&main_entity.id())?;
            Some((
                GpuChunkQuads {
                    record_start: allocation.record_start,
                    quad_count: allocation.quad_count,
                    chunk_slot: allocation.chunk_slot,
                    cutout_start: arena
                        .metadata
                        .values()
                        .get(allocation.chunk_slot as usize)
                        .map_or(allocation.quad_count, |meta| meta.face_ends[7]),
                },
                chunk.center,
                chunk.half_extents,
                true,
            ))
        })
        .collect();
    for (view_entity, frustum, is_shadow_view, _, frontend) in &views {
        if !crate::session::view_draws_world(frontend) {
            commands.entity(view_entity).insert(ViewChunkDrawList::default());
            continue;
        }
        let list = grouped_draw_list(frustum, resolved.iter().copied());
        frustum_visible_chunks = frustum_visible_chunks
            .saturating_add(frustum_metric_contribution(list.0.len(), is_shadow_view));
        if !is_shadow_view {
            if let Some((submitted_quads, submitted_vertices)) = &mut submitted_workload {
                let (view_quads, view_vertices) = draw_list_workload(&list);
                *submitted_quads = submitted_quads.saturating_add(view_quads);
                *submitted_vertices = submitted_vertices.saturating_add(view_vertices);
            }
            if let Some(count) = &mut direct_draw_count {
                *count = count.saturating_add(list.0.len().min(u32::MAX as usize) as u32);
            }
        }
        commands
            .entity(view_entity)
            .insert(ViewChunkDrawList(list.0));
    }
    update_near_metrics_for_prepare(
        *mode,
        active_chunks,
        Some(frustum_visible_chunks),
        resident_quads,
        submitted_workload,
        direct_draw_count,
        &mut metrics,
    );
}

fn geometric_capacity(required: usize) -> usize {
    required.max(1).next_power_of_two()
}

fn grow_to<T: Clone>(values: &mut Vec<T>, required_len: usize, fill: T) {
    if values.len() < required_len {
        values.resize(required_len, fill);
    }
}

pub fn prepare_chunk_quads(
    mut commands: Commands,
    // Only chunks (re-)extracted since this system last ran: extraction
    // inserts `ExtractedChunkQuads` exactly when something changed.
    chunks: Query<(Entity, &MainEntity, &ExtractedChunkQuads), Changed<ExtractedChunkQuads>>,
    pipeline: Res<VoxelPullPipeline>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    images: Res<RenderAssets<GpuImage>>,
    block_texture: Res<BlockArrayTexture>,
    top_layers: Option<Res<BlockTopLayers>>,
    boundaries: Option<Res<BoundaryUniform>>,
    fortress_view: Option<Res<FortressViewUniform>>,
    overlay: Option<Res<OverlayTexture>>,
    mut arena: ResMut<GpuQuadArena>,
    removed: Res<RemovedChunkQuads>,
) {
    for &main_entity in &removed.0 {
        arena.release(main_entity);
    }

    let texture = images.get(&block_texture.0);

    let mut rebuild_bind_group = arena.bind_group.is_none();
    // Static after startup: uploaded once (again only if a stand-in was
    // uploaded before the resource existed).
    if arena.top_layers.as_ref().is_none_or(|(_, real)| !real && top_layers.is_some()) {
        let lut = top_layers.as_ref().map_or_else(|| vec![0], |l| l.0.to_vec());
        let buffer = device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("voxel_pull_top_layers"),
            contents: bytemuck::cast_slice(&lut),
            usage: BufferUsages::STORAGE,
        });
        arena.top_layers = Some((buffer, top_layers.is_some()));
        rebuild_bind_group = true;
    }
    // The boundary overlay uniform: created once, rewritten when the
    // extracted value changes (or before the main world has sent one, as the
    // all-off default).
    if arena.boundaries.is_none() {
        arena.boundaries = Some(device.create_buffer(&BufferDescriptor {
            label: Some("voxel_pull_boundaries"),
            size: BoundaryUniform::min_size().get(),
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        let value = boundaries.as_deref().copied().unwrap_or_default();
        write_uniform(&queue, arena.boundaries.as_ref(), &value);
        rebuild_bind_group = true;
    } else if let Some(value) = boundaries.as_ref().filter(|b| b.is_changed()) {
        write_uniform(&queue, arena.boundaries.as_ref(), value.as_ref());
    }
    // The fortress view uniform (z-level cut), the same way; the default is
    // the cut off. The queue systems pick the `CUTAWAY` pipelines from the
    // same extracted value.
    if arena.fortress_view.is_none() {
        arena.fortress_view = Some(device.create_buffer(&BufferDescriptor {
            label: Some("voxel_pull_fortress_view"),
            size: FortressViewUniform::min_size().get(),
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        let value = fortress_view.as_deref().copied().unwrap_or_default();
        write_uniform(&queue, arena.fortress_view.as_ref(), &value);
        rebuild_bind_group = true;
    } else if let Some(value) = fortress_view.as_ref().filter(|v| v.is_changed()) {
        write_uniform(&queue, arena.fortress_view.as_ref(), value.as_ref());
    }
    // The shared quad index buffer must cover the largest single chunk.
    let largest = chunks
        .iter()
        .map(|(_, _, extracted)| extracted.records.len() as u32)
        .max()
        .unwrap_or(0);
    if arena.quad_indices.as_ref().is_none_or(|(_, covered)| *covered < largest) {
        let quads = quad_index_capacity(largest);
        let buffer = device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("voxel_pull_quad_indices"),
            contents: bytemuck::cast_slice(&quad_index_data(quads)),
            usage: BufferUsages::INDEX,
        });
        arena.quad_indices = Some((buffer, quads));
    }
    for (entity, main_entity, extracted) in &chunks {
        let quad_count = extracted.records.len() as u32;
        if quad_count == 0 {
            arena.release(main_entity.id());
            commands.entity(entity).remove::<GpuChunkQuads>();
            continue;
        }

        let allocation = arena.ensure_allocation(main_entity.id(), quad_count);
        let ends = chunk_face_ends(&extracted.records, &extracted.world_from_local);
        arena.update_metadata(
            main_entity.id(),
            extracted.center,
            extracted.half_extents,
            extracted.cave_active,
            ends,
        );

        let record_end = allocation.record_start as usize + quad_count as usize;
        grow_to(
            arena.records.values_mut(),
            record_end,
            super::super::meshing::records::QuadRecord([0, 0]),
        );
        arena.records.values_mut()[allocation.record_start as usize..record_end]
            .copy_from_slice(&extracted.records);
        let transform_end = allocation.chunk_slot as usize + 1;
        grow_to(arena.transforms.values_mut(), transform_end, Mat4::IDENTITY);
        arena.transforms.values_mut()[allocation.chunk_slot as usize] = extracted.world_from_local;

        let record_capacity = geometric_capacity(arena.record_allocator.high_water() as usize);
        let transform_capacity = geometric_capacity(arena.slot_allocator.high_water() as usize);
        let records_grew = record_capacity > arena.records.capacity();
        let transforms_grew = transform_capacity > arena.transforms.capacity();
        arena.records.reserve(record_capacity, &device);
        arena.transforms.reserve(transform_capacity, &device);
        if records_grew {
            arena.records.write_buffer(&device, &queue);
        } else {
            arena
                .records
                .write_buffer_range(&queue, allocation.record_start as usize..record_end)
                .expect("record arena was reserved");
        }
        if transforms_grew {
            arena.transforms.write_buffer(&device, &queue);
        } else {
            arena
                .transforms
                .write_buffer_range(&queue, allocation.chunk_slot as usize..transform_end)
                .expect("transform arena was reserved");
        }
        rebuild_bind_group |= records_grew || transforms_grew;
        commands.entity(entity).insert(GpuChunkQuads {
            record_start: allocation.record_start,
            quad_count,
            chunk_slot: allocation.chunk_slot,
            cutout_start: ends[7],
        });
    }

    // Metadata writes are collected across allocation/update/release so an
    // inactive slot reaches the GPU even in a frame with no chunk uploads.
    let metadata_capacity = geometric_capacity(arena.slot_allocator.high_water() as usize);
    let metadata_grew = metadata_capacity > arena.metadata.capacity();
    arena.metadata.reserve(metadata_capacity, &device);
    if metadata_grew {
        arena.metadata.write_buffer(&device, &queue);
        arena.metadata_generation = arena.metadata_generation.wrapping_add(1);
    } else {
        let dirty: Vec<u32> = arena.metadata_dirty.iter().copied().collect();
        for slot in dirty {
            arena
                .metadata
                .write_buffer_range(&queue, slot as usize..slot as usize + 1)
                .expect("metadata arena was reserved");
        }
    }
    arena.metadata_dirty.clear();

    if rebuild_bind_group {
        // The designation overlay texture: written in place by
        // `fortress_view::upload_overlay` (the dirty sub-box), so the bind
        // group only needs its view.
        let overlay = overlay.as_ref().and_then(|o| images.get(&o.0));
        let (Some(texture), Some(overlay)) = (texture, overlay) else {
            return;
        };
        let (
            Some(records),
            Some(transforms),
            Some((top_layers, _)),
            Some(boundaries),
            Some(fortress_view),
        ) = (
            arena.records.binding(),
            arena.transforms.binding(),
            arena.top_layers.as_ref(),
            arena.boundaries.as_ref(),
            arena.fortress_view.as_ref(),
        ) else {
            return;
        };
        arena.bind_group = Some(device.create_bind_group(
            "voxel_pull_arena",
            &pipeline.chunk_layout,
            &BindGroupEntries::sequential((
                records,
                transforms,
                &texture.texture_view,
                &texture.sampler,
                top_layers.as_entire_binding(),
                boundaries.as_entire_binding(),
                fortress_view.as_entire_binding(),
                &overlay.texture_view,
            )),
        ));
    }
}

/// `records::face_ends` for a chunk drawn through `world_from_local`, with
/// the "unsorted, never face-culled" flag (entry 6) set unless the transform
/// keeps every local face direction on the same world direction (a positive
/// diagonal upper 3×3: translation and per-axis scale — terrain and LOD).
/// The cull shader skips a face direction's range by comparing the eye with
/// the chunk's world AABB along that WORLD axis (`face_may_be_visible`), so
/// for a rotated chunk — a falling voxel body's brick, whose local −Y faces
/// may point up — it would drop faces that face the camera. Entry 7 (where
/// the cutout range starts) is kept, so the cutout pass still draws.
pub(crate) fn chunk_face_ends(records: &[super::super::meshing::records::QuadRecord], world_from_local: &Mat4) -> [u32; 8] {
    let mut ends = face_ends(records);
    if !keeps_face_axes(world_from_local) {
        ends[6] = 1;
    }
    ends
}

/// Whether the upper 3×3 of `m` is diagonal (off-diagonal entries below a
/// relative [`AXIS_ALIGNED_EPSILON`]) with positive entries.
fn keeps_face_axes(m: &Mat4) -> bool {
    let cols = [m.x_axis, m.y_axis, m.z_axis];
    let scale = (0..3).map(|i| cols[i][i].abs()).fold(0.0f32, f32::max);
    (0..3).all(|c| {
        (0..3).all(|r| if r == c { cols[c][r] > 0.0 } else { cols[c][r].abs() <= AXIS_ALIGNED_EPSILON * scale })
    })
}

/// Relative size of an off-diagonal transform entry still treated as zero
/// by [`keeps_face_axes`] (float noise on an axis-aligned transform).
const AXIS_ALIGNED_EPSILON: f32 = 1e-5;

/// Encode one small uniform (`BoundaryUniform`, `FortressViewUniform`) into
/// its buffer.
fn write_uniform<T: ShaderType + encase::internal::WriteInto>(
    queue: &RenderQueue,
    buffer: Option<&Buffer>,
    value: &T,
) {
    let Some(buffer) = buffer else {
        return;
    };
    let mut bytes = encase::UniformBuffer::new(Vec::<u8>::new());
    bytes.write(value).expect("overlay uniform encodes");
    queue.write_buffer(buffer, 0, bytes.as_ref());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_list_workload_sums_quads_and_vertices() {
        let draws = ViewChunkDrawList(vec![
            GpuChunkQuads {
                record_start: 0,
                quad_count: 4,
                chunk_slot: 0,
                cutout_start: 4,
            },
            GpuChunkQuads {
                record_start: 4,
                quad_count: 7,
                chunk_slot: 1,
                cutout_start: 5,
            },
        ]);
        assert_eq!(draw_list_workload(&draws), (11, 66));
    }

    #[test]
    fn gpu_prepare_preserves_the_latest_asynchronous_telemetry() {
        let mode = ActiveNearMode::GpuDriven {
            indirect_count: true,
        };
        let mut metrics = NearRendererMetrics {
            active_mode: mode,
            hzb_visible_chunks: Some(7),
            indirect_draw_count: Some(3),
            ..default()
        };

        update_near_metrics_for_prepare(mode, 17, Some(11), 23, None, None, &mut metrics);

        assert_eq!(metrics.active_chunks, 17);
        assert_eq!(metrics.frustum_visible_chunks, Some(11));
        assert_eq!(metrics.resident_quads, 23);
        assert_eq!(metrics.hzb_visible_chunks, Some(7));
        assert_eq!(metrics.indirect_draw_count, Some(3));
        assert_eq!(metrics.direct_draw_count, Some(0));
    }

    /// Face-sorted records: one opaque quad per face direction, then one
    /// cutout quad.
    fn sorted_records() -> Vec<super::super::super::meshing::records::QuadRecord> {
        use super::super::super::meshing::binary::{Face, Quad};
        use super::super::super::meshing::records::quads_to_records;
        let quad = |face, cutout| Quad { min: [1, 2, 3], w: 1, h: 1, face, block: game::BlockId(1), ao: [3; 4], cutout, interior: false };
        let mut quads: Vec<Quad> = [Face::PosX, Face::NegX, Face::PosY, Face::NegY, Face::PosZ, Face::NegZ]
            .into_iter()
            .map(|f| quad(f, false))
            .collect();
        quads.push(quad(Face::PosY, true));
        quads_to_records(&quads, &Default::default())
    }

    #[test]
    fn terrain_chunks_stay_face_culled() {
        let records = sorted_records();
        let want = face_ends(&records);
        assert_eq!(want[6], 0, "sorted records are face-culled");
        // Terrain: translation and uniform scale (LOD) only.
        for m in [Mat4::IDENTITY, Mat4::from_translation(Vec3::new(1024.0, -16.0, 7.5)), Mat4::from_scale(Vec3::splat(4.0))] {
            assert_eq!(chunk_face_ends(&records, &m), want, "{m:?}");
        }
    }

    #[test]
    fn rotated_chunks_are_never_face_culled() {
        // A falling voxel body's brick: its local −Y faces may point up, so
        // the cull shader's world-axis face test would drop them.
        let records = sorted_records();
        let cutout = face_ends(&records)[7];
        for m in [
            Mat4::from_rotation_z(2.5),
            Mat4::from_rotation_x(0.01) * Mat4::from_translation(Vec3::splat(5.0)),
            // A quarter turn permutes the face axes: still not world-aligned.
            Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2),
            // A mirror flips them.
            Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)),
        ] {
            let ends = chunk_face_ends(&records, &m);
            assert_ne!(ends[6], 0, "unsorted flag for {m:?}");
            assert_eq!(ends[7], cutout, "the cutout range still starts after the opaque one");
        }
    }

    #[test]
    fn growing_a_low_reused_range_preserves_higher_mirror_entries() {
        let mut values = vec![1, 2, 3, 4];

        grow_to(&mut values, 2, 0);

        assert_eq!(values, vec![1, 2, 3, 4]);
    }
}
