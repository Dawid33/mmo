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

use super::arena::{ArenaAllocation, GpuQuadArena};
use super::pipeline::VoxelPullPipeline;
use super::{BlockArrayTexture, ExtractedChunkQuads, RemovedChunkQuads};

/// Per-entity draw metadata into the shared arena.
#[derive(Component, Clone, Copy)]
pub struct GpuChunkQuads {
    pub record_start: u32,
    pub quad_count: u32,
    pub chunk_slot: u32,
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
    let Some(texture) = images.get(&block_texture.0) else {
        return;
    };

    for &main_entity in &removed.0 {
        arena.release(main_entity);
    }

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

        let allocation = match arena.allocations.get(&main_entity.id()).copied() {
            Some(allocation) if allocation.quad_count == quad_count => allocation,
            Some(_) => {
                arena.release(main_entity.id());
                let range = arena.record_allocator.allocate(quad_count);
                ArenaAllocation {
                    record_start: range.start,
                    quad_count,
                    chunk_slot: arena.slot_allocator.allocate(),
                }
            }
            None => {
                let range = arena.record_allocator.allocate(quad_count);
                ArenaAllocation {
                    record_start: range.start,
                    quad_count,
                    chunk_slot: arena.slot_allocator.allocate(),
                }
            }
        };

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
        arena.allocations.insert(main_entity.id(), allocation);
        commands.entity(entity).insert(GpuChunkQuads {
            record_start: allocation.record_start,
            quad_count,
            chunk_slot: allocation.chunk_slot,
        });
    }

    if rebuild_bind_group {
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
    fn growing_a_low_reused_range_preserves_higher_mirror_entries() {
        let mut values = vec![1, 2, 3, 4];

        grow_to(&mut values, 2, 0);

        assert_eq!(values, vec![1, 2, 3, 4]);
    }
}
