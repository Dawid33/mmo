use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;

use bevy::{
    prelude::*,
    render::render_resource::{BindGroup, BufferUsages, RawBufferVec},
};

use super::super::meshing::records::QuadRecord;

/// One stable, shader-readable chunk record indexed by `chunk_slot`.
///
/// The vec4-shaped AABB fields keep the following u32 range block at the same
/// offsets in Rust and WGSL storage layouts (0, 16, and 32 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuNearChunkMeta {
    pub center: [f32; 4],
    pub half_extents: [f32; 4],
    pub record_start: u32,
    pub quad_count: u32,
    pub transform_slot: u32,
    pub cave_active: u32,
}

impl GpuNearChunkMeta {
    pub const INACTIVE: Self = Self {
        center: [0.0; 4],
        half_extents: [0.0; 4],
        record_start: 0,
        quad_count: 0,
        transform_slot: 0,
        cave_active: 0,
    };

    fn from_allocation(
        allocation: ArenaAllocation,
        center: Vec3,
        half_extents: Vec3,
        cave_active: bool,
    ) -> Self {
        Self {
            center: [center.x, center.y, center.z, 0.0],
            half_extents: [half_extents.x, half_extents.y, half_extents.z, 0.0],
            record_start: allocation.record_start,
            quad_count: allocation.quad_count,
            transform_slot: allocation.chunk_slot,
            cave_active: u32::from(cave_active),
        }
    }
}

/// Deterministic first-fit allocator for contiguous quad-record slots.
#[derive(Default)]
pub struct RangeAllocator {
    free: BTreeMap<u32, u32>,
    high_water: u32,
}

impl RangeAllocator {
    pub fn allocate(&mut self, len: u32) -> Range<u32> {
        assert!(len > 0, "arena allocations are non-empty");
        let reusable = self
            .free
            .iter()
            .find(|(_, free_len)| **free_len >= len)
            .map(|(&s, &l)| (s, l));
        if let Some((start, free_len)) = reusable {
            self.free.remove(&start);
            if free_len > len {
                self.free.insert(start + len, free_len - len);
            }
            return start..start + len;
        }
        let start = self.high_water;
        self.high_water = self
            .high_water
            .checked_add(len)
            .expect("quad arena index overflow");
        start..self.high_water
    }

    pub fn free(&mut self, range: Range<u32>) {
        if range.is_empty() {
            return;
        }
        let mut start = range.start;
        let mut end = range.end;
        if let Some((&previous_start, &previous_len)) = self.free.range(..start).next_back() {
            if previous_start + previous_len == start {
                start = previous_start;
                self.free.remove(&previous_start);
            }
        }
        if let Some((&next_start, &next_len)) = self.free.range(start..).next() {
            if end == next_start {
                end = next_start + next_len;
                self.free.remove(&next_start);
            }
        }
        self.free.insert(start, end - start);
    }

    pub fn high_water(&self) -> u32 {
        self.high_water
    }
}

/// Stable allocator for fixed-size chunk-transform slots.
#[derive(Default)]
pub struct SlotAllocator {
    free: BTreeSet<u32>,
    high_water: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArenaAllocation {
    pub record_start: u32,
    pub quad_count: u32,
    pub chunk_slot: u32,
}

impl ArenaAllocation {
    pub fn record_range(self) -> Range<u32> {
        self.record_start..self.record_start + self.quad_count
    }
}

/// Render-world owner of the shared pulled-terrain buffers and their stable
/// per-entity allocations. CPU mirrors make buffer growth deterministic and
/// allow unchanged chunks to survive a reallocation without re-extraction.
#[derive(Resource)]
pub struct GpuQuadArena {
    pub records: RawBufferVec<QuadRecord>,
    pub transforms: RawBufferVec<Mat4>,
    pub metadata: RawBufferVec<GpuNearChunkMeta>,
    pub record_allocator: RangeAllocator,
    pub slot_allocator: SlotAllocator,
    pub allocations: HashMap<Entity, ArenaAllocation>,
    pub metadata_dirty: BTreeSet<u32>,
    pub metadata_generation: u64,
    pub bind_group: Option<BindGroup>,
}

impl Default for GpuQuadArena {
    fn default() -> Self {
        let mut records = RawBufferVec::new(BufferUsages::STORAGE);
        records.set_label(Some("voxel_pull_record_arena"));
        let mut transforms = RawBufferVec::new(BufferUsages::STORAGE);
        transforms.set_label(Some("voxel_pull_transform_arena"));
        let mut metadata = RawBufferVec::new(BufferUsages::STORAGE);
        metadata.set_label(Some("voxel_pull_chunk_metadata"));
        Self {
            records,
            transforms,
            metadata,
            record_allocator: default(),
            slot_allocator: default(),
            allocations: default(),
            metadata_dirty: default(),
            metadata_generation: 0,
            bind_group: None,
        }
    }
}

impl GpuQuadArena {
    /// Returns an allocation while keeping the transform/metadata slot stable
    /// when only the variable-length quad-record range changes.
    pub fn ensure_allocation(&mut self, entity: Entity, quad_count: u32) -> ArenaAllocation {
        assert!(quad_count > 0, "arena allocations are non-empty");
        let allocation = match self.allocations.get(&entity).copied() {
            Some(allocation) if allocation.quad_count == quad_count => allocation,
            Some(old) => {
                self.record_allocator.free(old.record_range());
                let range = self.record_allocator.allocate(quad_count);
                ArenaAllocation {
                    record_start: range.start,
                    quad_count,
                    chunk_slot: old.chunk_slot,
                }
            }
            None => {
                let range = self.record_allocator.allocate(quad_count);
                ArenaAllocation {
                    record_start: range.start,
                    quad_count,
                    chunk_slot: self.slot_allocator.allocate(),
                }
            }
        };
        self.allocations.insert(entity, allocation);

        let end = allocation.chunk_slot as usize + 1;
        if self.metadata.values().len() < end {
            self.metadata
                .values_mut()
                .resize(end, GpuNearChunkMeta::INACTIVE);
        }
        let old = self.metadata.values()[allocation.chunk_slot as usize];
        self.metadata.values_mut()[allocation.chunk_slot as usize] = GpuNearChunkMeta {
            record_start: allocation.record_start,
            quad_count: allocation.quad_count,
            transform_slot: allocation.chunk_slot,
            ..old
        };
        self.metadata_dirty.insert(allocation.chunk_slot);
        allocation
    }

    pub fn update_metadata(
        &mut self,
        entity: Entity,
        center: Vec3,
        half_extents: Vec3,
        cave_active: bool,
    ) {
        let allocation = *self
            .allocations
            .get(&entity)
            .expect("metadata update requires an arena allocation");
        self.metadata.values_mut()[allocation.chunk_slot as usize] =
            GpuNearChunkMeta::from_allocation(allocation, center, half_extents, cave_active);
        self.metadata_dirty.insert(allocation.chunk_slot);
    }

    pub fn release(&mut self, entity: Entity) {
        let Some(allocation) = self.allocations.remove(&entity) else {
            return;
        };
        self.record_allocator.free(allocation.record_range());
        if let Some(metadata) = self
            .metadata
            .values_mut()
            .get_mut(allocation.chunk_slot as usize)
        {
            *metadata = GpuNearChunkMeta::INACTIVE;
            self.metadata_dirty.insert(allocation.chunk_slot);
        }
        self.slot_allocator.free(allocation.chunk_slot);
    }
}

impl SlotAllocator {
    pub fn allocate(&mut self) -> u32 {
        if let Some(slot) = self.free.pop_first() {
            return slot;
        }
        let slot = self.high_water;
        self.high_water = self
            .high_water
            .checked_add(1)
            .expect("chunk arena index overflow");
        slot
    }

    pub fn free(&mut self, slot: u32) {
        self.free.insert(slot);
    }

    pub fn high_water(&self) -> u32 {
        self.high_water
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_chunk_metadata_keeps_slot_stable_across_record_resize() {
        let mut arena = GpuQuadArena::default();
        let entity = Entity::from_raw_u32(7).unwrap();
        let first = arena.ensure_allocation(entity, 4);
        arena.update_metadata(
            entity,
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::new(4.0, 5.0, 6.0),
            true,
        );

        let resized = arena.ensure_allocation(entity, 9);

        assert_eq!(resized.chunk_slot, first.chunk_slot);
        assert_eq!(
            arena.metadata.values()[first.chunk_slot as usize],
            GpuNearChunkMeta {
                center: [1.0, 2.0, 3.0, 0.0],
                half_extents: [4.0, 5.0, 6.0, 0.0],
                record_start: resized.record_start,
                quad_count: 9,
                transform_slot: first.chunk_slot,
                cave_active: 1,
            }
        );
    }

    #[test]
    fn releasing_chunk_clears_sparse_metadata_and_reuses_lowest_slot() {
        let mut arena = GpuQuadArena::default();
        let first_entity = Entity::from_raw_u32(10).unwrap();
        let second_entity = Entity::from_raw_u32(11).unwrap();
        let replacement_entity = Entity::from_raw_u32(12).unwrap();
        let first = arena.ensure_allocation(first_entity, 2);
        let second = arena.ensure_allocation(second_entity, 3);
        arena.update_metadata(first_entity, Vec3::ONE, Vec3::splat(2.0), true);
        arena.update_metadata(second_entity, Vec3::splat(3.0), Vec3::splat(4.0), true);

        arena.release(first_entity);

        assert_eq!(
            arena.metadata.values()[first.chunk_slot as usize],
            GpuNearChunkMeta::INACTIVE
        );
        assert_eq!(
            arena.metadata.values()[second.chunk_slot as usize].quad_count,
            3
        );
        let replacement = arena.ensure_allocation(replacement_entity, 1);
        assert_eq!(replacement.chunk_slot, first.chunk_slot);
    }

    #[test]
    fn near_chunk_metadata_has_wgsl_storage_layout() {
        assert_eq!(std::mem::size_of::<GpuNearChunkMeta>(), 48);
        assert!(
            bytemuck::try_cast_slice::<GpuNearChunkMeta, u8>(&[GpuNearChunkMeta::INACTIVE]).is_ok()
        );
    }

    #[test]
    fn allocator_reuses_and_splits_freed_ranges() {
        let mut allocator = RangeAllocator::default();
        let a = allocator.allocate(8);
        let b = allocator.allocate(4);
        assert_eq!(a, 0..8);
        assert_eq!(b, 8..12);

        allocator.free(a);
        assert_eq!(allocator.allocate(3), 0..3);
        assert_eq!(allocator.allocate(5), 3..8);
        assert_eq!(allocator.high_water(), 12);
    }

    #[test]
    fn allocator_coalesces_adjacent_free_ranges() {
        let mut allocator = RangeAllocator::default();
        let a = allocator.allocate(4);
        let b = allocator.allocate(6);
        let c = allocator.allocate(2);
        allocator.free(b);
        allocator.free(a);
        allocator.free(c);

        assert_eq!(allocator.allocate(12), 0..12);
        assert_eq!(allocator.high_water(), 12);
    }

    #[test]
    fn slot_allocator_reuses_released_slots() {
        let mut slots = SlotAllocator::default();
        let a = slots.allocate();
        let b = slots.allocate();
        slots.free(a);

        assert_eq!(slots.allocate(), a);
        assert_ne!(a, b);
        assert_eq!(slots.high_water(), 2);
    }
}
