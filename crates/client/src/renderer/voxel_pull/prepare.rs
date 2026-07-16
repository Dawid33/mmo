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

use super::arena::GpuQuadArena;
use super::gpu::{
    exact_indirect_draw_count, synchronize_near_metric_mode, ActiveNearMode, ForwardCullSource,
    NearRendererMetrics,
};
use super::pipeline::VoxelPullPipeline;
use super::queue::{grouped_draw_list, ViewChunkDrawList};
use super::{BlockArrayTexture, ExtractedChunkQuads, RemovedChunkQuads};

/// Per-entity draw metadata into the shared arena.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuChunkQuads {
    pub record_start: u32,
    pub quad_count: u32,
    pub chunk_slot: u32,
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
    frustum_visible_chunks: u32,
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

/// Finalizes the view-owned grouped-direct lists after arena allocation, so a
/// chunk uploaded this frame is eligible to draw in the same frame.
pub fn prepare_view_chunk_draws(
    mut commands: Commands,
    views: Query<(
        Entity,
        &bevy::camera::primitives::Frustum,
        Has<bevy::pbr::ShadowView>,
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
    let mut submitted_workload =
        matches!(*mode, ActiveNearMode::GroupedDirect).then_some((0u64, 0u64));
    let mut direct_draw_count = matches!(*mode, ActiveNearMode::GroupedDirect).then_some(0u32);
    for (view_entity, frustum, is_shadow_view) in &views {
        let list = grouped_draw_list(
            frustum,
            chunks.iter().filter_map(|(main_entity, chunk)| {
                let allocation = arena.allocations.get(&main_entity.id())?;
                Some((
                    GpuChunkQuads {
                        record_start: allocation.record_start,
                        quad_count: allocation.quad_count,
                        chunk_slot: allocation.chunk_slot,
                    },
                    chunk.center,
                    chunk.half_extents,
                    chunk.cave_active,
                ))
            }),
        );
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
        frustum_visible_chunks,
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
    chunks: Query<(Entity, &MainEntity, &ExtractedChunkQuads)>,
    existing: Query<&GpuChunkQuads>,
    pipeline: Res<VoxelPullPipeline>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    images: Res<RenderAssets<GpuImage>>,
    block_texture: Res<BlockArrayTexture>,
    mut arena: ResMut<GpuQuadArena>,
    removed: Res<RemovedChunkQuads>,
) {
    for &main_entity in &removed.0 {
        arena.release(main_entity);
    }

    let texture = images.get(&block_texture.0);

    let mut rebuild_bind_group = arena.bind_group.is_none();
    for (entity, main_entity, extracted) in &chunks {
        if !extracted.changed && existing.contains(entity) {
            continue;
        }
        let quad_count = extracted.records.len() as u32;
        if quad_count == 0 {
            arena.release(main_entity.id());
            commands.entity(entity).remove::<GpuChunkQuads>();
            continue;
        }

        let allocation = arena.ensure_allocation(main_entity.id(), quad_count);
        arena.update_metadata(
            main_entity.id(),
            extracted.center,
            extracted.half_extents,
            extracted.cave_active,
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
        let Some(texture) = texture else {
            return;
        };
        let (Some(records), Some(transforms)) =
            (arena.records.binding(), arena.transforms.binding())
        else {
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
            )),
        ));
    }
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
            },
            GpuChunkQuads {
                record_start: 4,
                quad_count: 7,
                chunk_slot: 1,
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

        update_near_metrics_for_prepare(mode, 17, 11, 23, None, None, &mut metrics);

        assert_eq!(metrics.active_chunks, 17);
        assert_eq!(metrics.frustum_visible_chunks, 11);
        assert_eq!(metrics.resident_quads, 23);
        assert_eq!(metrics.hzb_visible_chunks, Some(7));
        assert_eq!(metrics.indirect_draw_count, Some(3));
        assert_eq!(metrics.direct_draw_count, Some(0));
    }

    #[test]
    fn growing_a_low_reused_range_preserves_higher_mirror_entries() {
        let mut values = vec![1, 2, 3, 4];

        grow_to(&mut values, 2, 0);

        assert_eq!(values, vec![1, 2, 3, 4]);
    }
}
