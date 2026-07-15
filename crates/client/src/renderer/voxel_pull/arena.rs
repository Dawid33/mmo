use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;

use bevy::{
    prelude::*,
    render::render_resource::{BindGroup, BufferUsages, RawBufferVec},
};

use super::super::meshing::records::QuadRecord;

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
    pub record_allocator: RangeAllocator,
    pub slot_allocator: SlotAllocator,
    pub allocations: HashMap<Entity, ArenaAllocation>,
    pub bind_group: Option<BindGroup>,
}

impl Default for GpuQuadArena {
    fn default() -> Self {
        let mut records = RawBufferVec::new(BufferUsages::STORAGE);
        records.set_label(Some("voxel_pull_record_arena"));
        let mut transforms = RawBufferVec::new(BufferUsages::STORAGE);
        transforms.set_label(Some("voxel_pull_transform_arena"));
        Self {
            records,
            transforms,
            record_allocator: default(),
            slot_allocator: default(),
            allocations: default(),
            bind_group: None,
        }
    }
}

impl GpuQuadArena {
    pub fn release(&mut self, entity: Entity) {
        let Some(allocation) = self.allocations.remove(&entity) else {
            return;
        };
        self.record_allocator.free(allocation.record_range());
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
