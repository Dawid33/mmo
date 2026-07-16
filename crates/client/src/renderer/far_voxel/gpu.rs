use std::sync::{Arc, Mutex};

use bevy::{
    prelude::*,
    render::{
        render_resource::{Buffer, BufferDescriptor, BufferUsages},
        renderer::{RenderDevice, RenderQueue},
        Extract, ExtractSchedule, Render, RenderSystems,
    },
};
use bytemuck::{Pod, Zeroable};

use super::{
    brick::{
        geometric_slot_capacity, BrickRoot, MixedBrick, FAR_STATIC_BINDING_OVERHEAD_BYTES,
        FAR_STORAGE_BUDGET_BYTES,
    },
    residency::{
        FarUpload, FarUploadBatch, FarUploadResult, FarVoxelResidency, GpuFarCell,
        FAR_MATERIAL_LOOKUP_BYTES, FAR_SINGLE_VIEW_PASS_FIXED_BYTES,
    },
};
use crate::renderer::{
    far_voxel::FarBrickBuildTask,
    meshing::{lod::LodBuildQueue, queue::MeshBudgets},
};

const FIRST_LOAD_BATCH_WAVES: usize = 1;

pub(crate) const fn first_load_publish_target(max_in_flight: usize) -> usize {
    max_in_flight.saturating_mul(FIRST_LOAD_BATCH_WAVES)
}

#[derive(Clone, Debug)]
pub struct SlotMirror<T> {
    values: Vec<T>,
    occupied: Vec<bool>,
}

impl<T: Zeroable + Copy> Default for SlotMirror<T> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            occupied: Vec::new(),
        }
    }
}

impl<T: Zeroable + Copy> SlotMirror<T> {
    pub fn write(&mut self, slot: u32, value: T) {
        let required = slot as usize + 1;
        self.values
            .resize(required.max(self.values.len()), T::zeroed());
        self.occupied
            .resize(required.max(self.occupied.len()), false);
        self.values[slot as usize] = value;
        self.occupied[slot as usize] = true;
    }

    pub fn clear(&mut self, slot: u32) {
        let Some(value) = self.values.get_mut(slot as usize) else {
            return;
        };
        *value = T::zeroed();
        self.occupied[slot as usize] = false;
        while self.occupied.last() == Some(&false) {
            self.occupied.pop();
            self.values.pop();
        }
    }

    pub fn required_capacity(&self) -> usize {
        self.values.len()
    }

    pub fn values(&self) -> &[T] {
        &self.values
    }

    pub fn active_slots(&self) -> usize {
        self.occupied.iter().filter(|occupied| **occupied).count()
    }
}

#[derive(Clone, Debug, Default)]
struct SlotOccupancy {
    occupied: Vec<bool>,
}

impl SlotOccupancy {
    fn from_mirror<T>(mirror: &SlotMirror<T>) -> Self {
        Self {
            occupied: mirror.occupied.clone(),
        }
    }

    fn write(&mut self, slot: u32) {
        let required = slot as usize + 1;
        self.occupied
            .resize(required.max(self.occupied.len()), false);
        self.occupied[slot as usize] = true;
    }

    fn clear(&mut self, slot: u32) {
        let Some(occupied) = self.occupied.get_mut(slot as usize) else {
            return;
        };
        *occupied = false;
        while self.occupied.last() == Some(&false) {
            self.occupied.pop();
        }
    }

    fn required_capacity(&self) -> usize {
        self.occupied.len()
    }

    #[cfg(test)]
    fn reserved_cpu_bytes(&self) -> usize {
        self.occupied.capacity() * std::mem::size_of::<bool>()
    }
}

pub struct GpuSlotBuffer<T> {
    mirror: SlotMirror<T>,
    buffer: Option<Buffer>,
    reserved_slots: usize,
    max_bytes: usize,
    label: &'static str,
}

impl<T: Pod + Zeroable + Copy> GpuSlotBuffer<T> {
    pub fn logical_used_bytes(&self) -> usize {
        self.mirror
            .active_slots()
            .saturating_mul(std::mem::size_of::<T>())
    }

    fn new(label: &'static str, max_bytes: usize) -> Self {
        Self {
            mirror: default(),
            buffer: None,
            reserved_slots: 0,
            max_bytes,
            label,
        }
    }

    fn write(
        &mut self,
        slot: u32,
        value: T,
        device: &RenderDevice,
        queue: &RenderQueue,
    ) -> Result<(), FarGpuUploadError> {
        self.mirror.write(slot, value);
        if self.reallocate_if_needed(device, queue)? {
            return Ok(());
        }
        let buffer = self.buffer.as_ref().expect("occupied slot has a buffer");
        queue.write_buffer(
            buffer,
            u64::from(slot) * std::mem::size_of::<T>() as u64,
            bytemuck::bytes_of(&value),
        );
        Ok(())
    }

    fn clear(
        &mut self,
        slot: u32,
        device: &RenderDevice,
        queue: &RenderQueue,
    ) -> Result<(), FarGpuUploadError> {
        self.mirror.clear(slot);
        if self.reallocate_if_needed(device, queue)? {
            return Ok(());
        }
        if let Some(buffer) = &self.buffer {
            let zero = T::zeroed();
            queue.write_buffer(
                buffer,
                u64::from(slot) * std::mem::size_of::<T>() as u64,
                bytemuck::bytes_of(&zero),
            );
        }
        Ok(())
    }

    fn reallocate_if_needed(
        &mut self,
        device: &RenderDevice,
        queue: &RenderQueue,
    ) -> Result<bool, FarGpuUploadError> {
        let required = self.mirror.required_capacity();
        let max_slots = self.max_bytes / std::mem::size_of::<T>();
        if required > max_slots {
            return Err(FarGpuUploadError::BindingLimit {
                label: self.label,
                requested: required.saturating_mul(std::mem::size_of::<T>()),
                maximum: self.max_bytes,
            });
        }
        let next_reserved = next_gpu_slot_capacity(self.reserved_slots, required, max_slots);
        if next_reserved == self.reserved_slots {
            return Ok(false);
        }
        let bytes = next_reserved.saturating_mul(std::mem::size_of::<T>());
        if bytes > self.max_bytes {
            return Err(FarGpuUploadError::BindingLimit {
                label: self.label,
                requested: bytes,
                maximum: self.max_bytes,
            });
        }
        self.reserved_slots = next_reserved;
        if next_reserved == 0 {
            self.buffer = None;
            return Ok(true);
        }
        let buffer = device.create_buffer(&BufferDescriptor {
            label: Some(self.label),
            size: bytes as u64,
            usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, bytemuck::cast_slice(self.mirror.values()));
        self.buffer = Some(buffer);
        Ok(true)
    }

    pub fn buffer(&self) -> Option<&Buffer> {
        self.buffer.as_ref()
    }

    pub fn reserved_bytes(&self) -> usize {
        self.reserved_slots * std::mem::size_of::<T>()
    }

    pub fn reserved_slots(&self) -> usize {
        self.reserved_slots
    }
}

fn next_gpu_slot_capacity(_current: usize, required: usize, max_slots: usize) -> usize {
    geometric_slot_capacity(required, max_slots)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FarGpuUploadError {
    InvalidPage {
        page: u8,
    },
    BindingLimit {
        label: &'static str,
        requested: usize,
        maximum: usize,
    },
    CombinedBudget {
        requested: usize,
        maximum: usize,
    },
}

#[derive(Resource)]
pub struct GpuFarBuffers {
    pub mixed_pages: [GpuSlotBuffer<MixedBrick>; 2],
    pub roots: GpuSlotBuffer<BrickRoot>,
    pub cells: GpuSlotBuffer<GpuFarCell>,
    binding_limit: usize,
    dummy_storage: Option<Buffer>,
    pass_reserved_bytes: usize,
    last_error: Option<FarGpuUploadError>,
}

impl GpuFarBuffers {
    pub fn new(binding_limit: usize) -> Self {
        Self {
            mixed_pages: [
                GpuSlotBuffer::new("far_voxel_mixed_page_0", binding_limit),
                GpuSlotBuffer::new("far_voxel_mixed_page_1", binding_limit),
            ],
            roots: GpuSlotBuffer::new("far_voxel_roots", binding_limit),
            cells: GpuSlotBuffer::new("far_voxel_cells", binding_limit),
            binding_limit,
            dummy_storage: None,
            pass_reserved_bytes: 0,
            last_error: None,
        }
    }

    pub fn reserved_bytes(&self) -> usize {
        self.mixed_pages
            .iter()
            .map(GpuSlotBuffer::reserved_bytes)
            .sum::<usize>()
            + self.roots.reserved_bytes()
            + self.cells.reserved_bytes()
            + FAR_STATIC_BINDING_OVERHEAD_BYTES
            + self.pass_reserved_bytes
    }

    pub fn logical_used_bytes(&self) -> usize {
        self.mixed_pages
            .iter()
            .map(GpuSlotBuffer::logical_used_bytes)
            .sum::<usize>()
            + self.roots.logical_used_bytes()
            + self.cells.logical_used_bytes()
            + FAR_STATIC_BINDING_OVERHEAD_BYTES
            + self.pass_reserved_bytes
    }

    pub fn set_pass_reserved_bytes(&mut self, bytes: usize) -> bool {
        self.pass_reserved_bytes = bytes;
        let reserved = self.reserved_bytes();
        if reserved > FAR_STORAGE_BUDGET_BYTES {
            self.last_error = Some(FarGpuUploadError::CombinedBudget {
                requested: reserved,
                maximum: FAR_STORAGE_BUDGET_BYTES,
            });
            false
        } else {
            true
        }
    }

    pub fn binding_limit(&self) -> usize {
        self.binding_limit
    }

    pub fn last_error(&self) -> Option<FarGpuUploadError> {
        self.last_error
    }

    /// The two page bindings are always available, even for an all-uniform
    /// scene where no mixed page has allocated a real slot yet.
    pub fn mixed_page_buffer(&self, page: usize) -> Option<&Buffer> {
        self.mixed_pages[page]
            .buffer()
            .or(self.dummy_storage.as_ref())
    }

    pub fn root_buffer(&self) -> Option<&Buffer> {
        self.roots.buffer().or(self.dummy_storage.as_ref())
    }

    pub fn cell_buffer(&self) -> Option<&Buffer> {
        self.cells.buffer().or(self.dummy_storage.as_ref())
    }

    pub fn cell_capacity(&self) -> u32 {
        self.cells.reserved_slots().min(u32::MAX as usize) as u32
    }

    pub fn active_cell_count(&self) -> u32 {
        self.cells.mirror.active_slots().min(u32::MAX as usize) as u32
    }

    pub fn prevalidate_batch(&self, batch: &FarUploadBatch) -> Result<(), FarGpuUploadError> {
        let mut mixed = [
            SlotOccupancy::from_mirror(&self.mixed_pages[0].mirror),
            SlotOccupancy::from_mirror(&self.mixed_pages[1].mirror),
        ];
        let mut roots = SlotOccupancy::from_mirror(&self.roots.mirror);
        let mut cells = SlotOccupancy::from_mirror(&self.cells.mirror);
        for upload in &batch.uploads {
            match upload {
                FarUpload::WriteMixed { page, slot, value } => {
                    let Some(page_mirror) = mixed.get_mut(*page as usize) else {
                        return Err(FarGpuUploadError::InvalidPage { page: *page });
                    };
                    let _ = value;
                    page_mirror.write(*slot);
                }
                FarUpload::WriteRoot { slot, .. } => roots.write(*slot),
                FarUpload::WriteCell { slot, .. } => cells.write(*slot),
                FarUpload::ClearMixed { page, slot } => {
                    let Some(page_mirror) = mixed.get_mut(*page as usize) else {
                        return Err(FarGpuUploadError::InvalidPage { page: *page });
                    };
                    page_mirror.clear(*slot);
                }
                FarUpload::ClearRoot { slot } => roots.clear(*slot),
                FarUpload::ClearCell { slot } => cells.clear(*slot),
            }
            self.validate_mirrors(&mixed, &roots, &cells)?;
        }
        Ok(())
    }

    fn validate_mirrors(
        &self,
        mixed: &[SlotOccupancy; 2],
        roots: &SlotOccupancy,
        cells: &SlotOccupancy,
    ) -> Result<(), FarGpuUploadError> {
        let mixed_bytes = mixed
            .iter()
            .enumerate()
            .map(|(page, mirror)| {
                validate_occupancy_capacity(
                    mirror.required_capacity(),
                    std::mem::size_of::<MixedBrick>(),
                    self.binding_limit,
                    if page == 0 {
                        "far_voxel_mixed_page_0"
                    } else {
                        "far_voxel_mixed_page_1"
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let root_bytes = validate_occupancy_capacity(
            roots.required_capacity(),
            std::mem::size_of::<BrickRoot>(),
            self.binding_limit,
            "far_voxel_roots",
        )?;
        let cell_bytes = validate_occupancy_capacity(
            cells.required_capacity(),
            std::mem::size_of::<GpuFarCell>(),
            self.binding_limit,
            "far_voxel_cells",
        )?;
        let cell_capacity = geometric_slot_capacity(
            cells.required_capacity(),
            self.binding_limit / std::mem::size_of::<GpuFarCell>(),
        );
        let minimum_pass_bytes = minimum_pass_reserved_bytes(cell_capacity);
        let requested = mixed_bytes
            .into_iter()
            .sum::<usize>()
            .saturating_add(root_bytes)
            .saturating_add(cell_bytes)
            .saturating_add(FAR_STATIC_BINDING_OVERHEAD_BYTES)
            .saturating_add(self.pass_reserved_bytes.max(minimum_pass_bytes));
        if requested > FAR_STORAGE_BUDGET_BYTES {
            return Err(FarGpuUploadError::CombinedBudget {
                requested,
                maximum: FAR_STORAGE_BUDGET_BYTES,
            });
        }
        Ok(())
    }

    fn apply(
        &mut self,
        upload: &FarUpload,
        device: &RenderDevice,
        queue: &RenderQueue,
    ) -> Result<(), FarGpuUploadError> {
        match upload {
            FarUpload::WriteMixed { page, slot, value } => {
                self.mixed_pages[*page as usize].write(*slot, **value, device, queue)?
            }
            FarUpload::WriteRoot { slot, value } => {
                self.roots.write(*slot, *value, device, queue)?
            }
            FarUpload::WriteCell { slot, value } => {
                self.cells.write(*slot, *value, device, queue)?
            }
            FarUpload::ClearMixed { page, slot } => {
                self.mixed_pages[*page as usize].clear(*slot, device, queue)?
            }
            FarUpload::ClearRoot { slot } => self.roots.clear(*slot, device, queue)?,
            FarUpload::ClearCell { slot } => self.cells.clear(*slot, device, queue)?,
        }
        let reserved = self.reserved_bytes();
        if reserved > FAR_STORAGE_BUDGET_BYTES {
            return Err(FarGpuUploadError::CombinedBudget {
                requested: reserved,
                maximum: FAR_STORAGE_BUDGET_BYTES,
            });
        }
        Ok(())
    }
}

fn minimum_pass_reserved_bytes(cell_capacity: usize) -> usize {
    FAR_MATERIAL_LOOKUP_BYTES
        .saturating_add(FAR_SINGLE_VIEW_PASS_FIXED_BYTES)
        .saturating_add(
            cell_capacity
                .max(1)
                .saturating_mul(std::mem::size_of::<u32>()),
        )
}

fn validate_occupancy_capacity(
    required_capacity: usize,
    element_bytes: usize,
    binding_limit: usize,
    label: &'static str,
) -> Result<usize, FarGpuUploadError> {
    let max_slots = binding_limit / element_bytes;
    if required_capacity > max_slots {
        return Err(FarGpuUploadError::BindingLimit {
            label,
            requested: required_capacity.saturating_mul(element_bytes),
            maximum: binding_limit,
        });
    }
    Ok(geometric_slot_capacity(required_capacity, max_slots) * element_bytes)
}

impl FromWorld for GpuFarBuffers {
    fn from_world(world: &mut World) -> Self {
        let binding_limit = world
            .resource::<RenderDevice>()
            .limits()
            .max_storage_buffer_binding_size as usize;
        let mut buffers = Self::new(binding_limit);
        buffers.dummy_storage = Some(world.resource::<RenderDevice>().create_buffer(
            &BufferDescriptor {
                label: Some("far_voxel_dummy_storage"),
                size: FAR_STATIC_BINDING_OVERHEAD_BYTES as u64,
                usage: BufferUsages::STORAGE,
                mapped_at_creation: false,
            },
        ));
        buffers
    }
}

#[derive(Resource, Default)]
pub struct ExtractedFarUploads(pub Vec<FarUploadBatch>);

#[derive(Default)]
struct FarUploadQueues {
    batches: Vec<FarUploadBatch>,
    results: Vec<FarUploadResult>,
}

#[derive(Resource, Clone, Default)]
pub struct FarUploadChannel(Arc<Mutex<FarUploadQueues>>);

impl FarUploadChannel {
    pub fn send_batch(&self, batch: FarUploadBatch) {
        self.lock().batches.push(batch);
    }

    pub fn drain_batches(&self) -> Vec<FarUploadBatch> {
        std::mem::take(&mut self.lock().batches)
    }

    pub fn send_result(&self, result: FarUploadResult) {
        self.lock().results.push(result);
    }

    pub fn drain_results(&self) -> Vec<FarUploadResult> {
        std::mem::take(&mut self.lock().results)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FarUploadQueues> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub fn publish_far_uploads(
    mut residency: ResMut<FarVoxelResidency>,
    channel: Res<FarUploadChannel>,
    budgets: Res<MeshBudgets>,
    build_queue: Res<LodBuildQueue>,
    brick_tasks: Query<Entity, With<FarBrickBuildTask>>,
) {
    let first_load_work_exhausted = build_queue.brick_entities().all(|entity| {
        residency.resident(entity).is_some() || residency.has_pending_first_load(entity)
    }) && brick_tasks.iter().all(|entity| {
        residency.resident(entity).is_some() || residency.has_pending_first_load(entity)
    });
    // Publish one bounded task wave at a time. Immutable content writes are
    // deduplicated within the transaction and shared across extraction, so a
    // 64-cell wave keeps frame cadence responsive without returning to the
    // original one-cell-per-ACK bottleneck.
    if let Some(batch) = residency.take_ready_upload_batch(
        first_load_publish_target(budgets.max_in_flight),
        first_load_work_exhausted,
    ) {
        channel.send_batch(batch);
    }
}

pub fn extract_far_uploads(mut commands: Commands, channel: Extract<Res<FarUploadChannel>>) {
    let channel = (*channel).clone();
    let batches = channel.drain_batches();
    commands.insert_resource(channel);
    commands.insert_resource(ExtractedFarUploads(batches));
}

pub fn apply_far_uploads(
    uploads: Res<ExtractedFarUploads>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut buffers: ResMut<GpuFarBuffers>,
    channel: Res<FarUploadChannel>,
) {
    buffers.last_error = None;
    for batch in &uploads.0 {
        if let Err(error) = buffers.prevalidate_batch(batch) {
            error!("far voxel GPU batch {} rejected: {error:?}", batch.id);
            buffers.last_error = Some(error);
            channel.send_result(FarUploadResult::nack(batch.id));
            continue;
        }
        let mut failed = None;
        for upload in &batch.uploads {
            if let Err(error) = buffers.apply(upload, &device, &queue) {
                failed = Some(error);
                break;
            }
        }
        if let Some(error) = failed {
            error!("far voxel GPU upload failed: {error:?}");
            buffers.last_error = Some(error);
            channel.send_result(FarUploadResult::nack(batch.id));
        } else {
            channel.send_result(FarUploadResult::ack(batch.id));
        }
    }
}

pub fn install_render_resources(render_app: &mut SubApp) {
    render_app
        .init_resource::<GpuFarBuffers>()
        .init_resource::<ExtractedFarUploads>()
        .add_systems(ExtractSchedule, extract_far_uploads)
        .add_systems(
            Render,
            apply_far_uploads.in_set(RenderSystems::PrepareResources),
        );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_load_publish_target_is_one_bounded_task_wave() {
        assert_eq!(first_load_publish_target(64), 64);
        assert_eq!(first_load_publish_target(1), 1);
        assert_eq!(first_load_publish_target(0), 0);
        assert_eq!(first_load_publish_target(usize::MAX), usize::MAX);
    }

    #[test]
    fn slot_mirror_grows_deterministically_and_shrinks_after_trailing_release() {
        let mut mirror = SlotMirror::<u32>::default();
        mirror.write(0, 11);
        assert_eq!(mirror.required_capacity(), 1);
        mirror.write(3, 22);
        assert_eq!(mirror.required_capacity(), 4);

        mirror.clear(3);
        assert_eq!(mirror.required_capacity(), 1);
        mirror.clear(0);
        assert_eq!(mirror.required_capacity(), 0);
    }

    #[test]
    fn slot_mirror_clear_writes_zero_and_preserves_interior_slot_numbers() {
        let mut mirror = SlotMirror::<u32>::default();
        mirror.write(0, 5);
        mirror.write(1, 6);
        mirror.write(2, 7);

        mirror.clear(1);

        assert_eq!(mirror.values(), &[5, 0, 7]);
        assert_eq!(mirror.required_capacity(), 3);
    }

    #[test]
    fn geometric_slot_capacity_has_logarithmic_growth_and_sparse_interior_writes() {
        let mut mirror = SlotMirror::<u32>::default();
        let mut reserved = 0;
        let mut capacity_changes = 0;
        for slot in 0..1_024_u32 {
            mirror.write(slot, slot);
            let next = next_gpu_slot_capacity(reserved, mirror.required_capacity(), 1_024);
            capacity_changes += usize::from(next != reserved);
            reserved = next;
        }

        assert_eq!(reserved, 1_024);
        assert_eq!(capacity_changes, 11);

        mirror.write(511, 99);
        assert_eq!(
            next_gpu_slot_capacity(reserved, mirror.required_capacity(), 1_024),
            reserved
        );
    }

    #[test]
    fn geometric_slot_capacity_shrinks_and_returns_to_zero() {
        assert_eq!(next_gpu_slot_capacity(1_024, 512, 1_024), 512);
        assert_eq!(next_gpu_slot_capacity(512, 257, 1_024), 512);
        assert_eq!(next_gpu_slot_capacity(512, 256, 1_024), 256);
        assert_eq!(next_gpu_slot_capacity(256, 0, 1_024), 0);
    }

    #[test]
    fn failed_batch_prevalidation_is_atomic_and_does_not_drop_the_next_batch() {
        let buffers = GpuFarBuffers::new(std::mem::size_of::<GpuFarCell>());
        let invalid = FarUploadBatch {
            id: 7,
            uploads: vec![
                FarUpload::WriteCell {
                    slot: 0,
                    value: GpuFarCell::INACTIVE,
                },
                FarUpload::WriteCell {
                    slot: 1,
                    value: GpuFarCell::INACTIVE,
                },
            ],
        };
        assert!(matches!(
            buffers.prevalidate_batch(&invalid),
            Err(FarGpuUploadError::BindingLimit { .. })
        ));
        assert_eq!(buffers.cells.mirror.required_capacity(), 0);

        let later = FarUploadBatch {
            id: 8,
            uploads: vec![FarUpload::WriteCell {
                slot: 0,
                value: GpuFarCell::INACTIVE,
            }],
        };
        assert!(buffers.prevalidate_batch(&later).is_ok());
        assert_eq!(buffers.cells.mirror.required_capacity(), 0);
    }

    #[test]
    fn prevalidation_shadow_copies_only_slot_occupancy_not_payloads() {
        let mut mirror = SlotMirror::<MixedBrick>::default();
        mirror.write(7, MixedBrick::zeroed());

        let shadow = SlotOccupancy::from_mirror(&mirror);

        assert_eq!(shadow.required_capacity(), 8);
        assert_eq!(shadow.reserved_cpu_bytes(), 8 * std::mem::size_of::<bool>());
        assert!(shadow.reserved_cpu_bytes() < std::mem::size_of::<MixedBrick>());
    }

    #[test]
    fn authoritative_reserved_bytes_include_dummy_and_exact_pass_allocations_once() {
        let mut buffers = GpuFarBuffers::new(1024 * 1024);
        assert!(buffers.set_pass_reserved_bytes(FAR_MATERIAL_LOOKUP_BYTES + 4_096));

        assert_eq!(
            buffers.reserved_bytes(),
            FAR_STATIC_BINDING_OVERHEAD_BYTES + FAR_MATERIAL_LOOKUP_BYTES + 4_096
        );
    }

    #[test]
    fn logical_bytes_count_active_slots_instead_of_sparse_capacity() {
        let mut buffers = GpuFarBuffers::new(1024 * 1024);
        buffers.cells.mirror.write(7, GpuFarCell::INACTIVE);
        buffers.cells.mirror.write(19, GpuFarCell::INACTIVE);
        buffers.cells.mirror.clear(7);
        assert!(buffers.set_pass_reserved_bytes(4096));

        assert_eq!(
            buffers.logical_used_bytes(),
            FAR_STATIC_BINDING_OVERHEAD_BYTES + 4096 + std::mem::size_of::<GpuFarCell>()
        );
    }

    #[test]
    fn minimum_pass_budget_includes_the_mandatory_visible_slot_at_zero_cells() {
        assert_eq!(
            minimum_pass_reserved_bytes(0),
            FAR_MATERIAL_LOOKUP_BYTES
                + FAR_SINGLE_VIEW_PASS_FIXED_BYTES
                + std::mem::size_of::<u32>()
        );
        assert_eq!(
            minimum_pass_reserved_bytes(1),
            minimum_pass_reserved_bytes(0)
        );
        assert_eq!(
            minimum_pass_reserved_bytes(2),
            minimum_pass_reserved_bytes(0) + std::mem::size_of::<u32>()
        );
    }
}
