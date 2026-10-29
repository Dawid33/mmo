use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bevy::prelude::*;

#[cfg(test)]
use super::brick::RefreshOutcome;
use super::brick::{
    geometric_slot_capacity, BrickDag, BrickDagStage, BrickRoot, DagLimits, MixedBrick,
    PreparedBrickRoot, RootEntry, RootEntryKind, RootHandle, BRICK_PAGE_BYTES,
    FAR_STATIC_BINDING_OVERHEAD_BYTES, FAR_STORAGE_BUDGET_BYTES, MIXED_BRICK_BYTES, ROOT_BYTES,
};

pub const GPU_FAR_CELL_BYTES: usize = std::mem::size_of::<GpuFarCell>();
pub const GPU_FAR_VISIBLE_SLOT_BYTES: usize = std::mem::size_of::<u32>();
pub const GPU_FAR_CELL_RESIDENT_BYTES: usize = GPU_FAR_CELL_BYTES + GPU_FAR_VISIBLE_SLOT_BYTES;
pub const FAR_MATERIAL_LOOKUP_BYTES: usize = 256 * 256 * std::mem::size_of::<[u16; 4]>();
/// One active `Camera3d` is enforced by the renderer bridge. Its far pass owns
/// one 112-byte cull uniform, one 16-byte indirect command buffer, and a
/// mandatory one-element visible-slot buffer even when no cells are resident.
pub const FAR_SINGLE_VIEW_PASS_FIXED_BYTES: usize = 112 + 16;
pub const FAR_STATIC_RESIDENT_BYTES: usize = FAR_STATIC_BINDING_OVERHEAD_BYTES
    + FAR_MATERIAL_LOOKUP_BYTES
    + FAR_SINGLE_VIEW_PASS_FIXED_BYTES
    + GPU_FAR_VISIBLE_SLOT_BYTES;
pub const FAR_NEIGHBOR_COUNT: usize = 26;
pub const INVALID_FAR_SLOT: u32 = u32::MAX;

/// Shader-facing metadata for one LOD cell. `origin` is the true unskirted
/// world-space minimum; the raster transform supplied to `new` is biased down
/// by one factor and is corrected here.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuFarCell {
    pub origin: [f32; 3],
    pub factor: u32,
    pub root_handle: u32,
    pub active: u32,
    pub _header_padding: [u32; 2],
    pub neighbor_slots: [u32; FAR_NEIGHBOR_COUNT],
    pub _tail_padding: [u32; 2],
}

impl GpuFarCell {
    pub const INACTIVE: Self = Self {
        origin: [0.0; 3],
        factor: 0,
        root_handle: 0,
        active: 0,
        _header_padding: [0; 2],
        neighbor_slots: [INVALID_FAR_SLOT; FAR_NEIGHBOR_COUNT],
        _tail_padding: [0; 2],
    };

    pub fn new(raster_biased_origin: Vec3, factor: u32, root_handle: u32) -> Self {
        let true_origin = raster_biased_origin + Vec3::Y * factor as f32;
        Self {
            origin: true_origin.to_array(),
            factor,
            root_handle,
            active: 1,
            ..Self::INACTIVE
        }
    }

    pub fn set_neighbor(&mut self, offset: NeighborOffset, slot: u32) {
        self.neighbor_slots[offset.index()] = slot;
    }

    pub fn neighbor(&self, offset: NeighborOffset) -> Option<u32> {
        let slot = self.neighbor_slots[offset.index()];
        (slot != INVALID_FAR_SLOT).then_some(slot)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NeighborOffset([i8; 3]);

impl NeighborOffset {
    pub fn new(x: i8, y: i8, z: i8) -> Option<Self> {
        if (-1..=1).contains(&x)
            && (-1..=1).contains(&y)
            && (-1..=1).contains(&z)
            && [x, y, z] != [0, 0, 0]
        {
            Some(Self([x, y, z]))
        } else {
            None
        }
    }

    pub fn index(self) -> usize {
        let linear = usize::from((self.0[0] + 1) as u8)
            + 3 * usize::from((self.0[1] + 1) as u8)
            + 9 * usize::from((self.0[2] + 1) as u8);
        linear - usize::from(linear > 13)
    }

    pub fn components(self) -> [i8; 3] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FarVoxelLimits {
    pub dag: DagLimits,
    pub max_reserved_bytes: usize,
    pub max_cells: usize,
    pub page_bytes: usize,
}

impl FarVoxelLimits {
    pub fn from_storage_binding_limit(binding_limit: usize) -> Self {
        let page_bytes = BRICK_PAGE_BYTES.min(binding_limit);
        Self {
            dag: DagLimits {
                max_reserved_bytes: FAR_STORAGE_BUDGET_BYTES - FAR_STATIC_RESIDENT_BYTES,
                max_mixed_bricks_per_page: page_bytes / MIXED_BRICK_BYTES,
                max_roots: binding_limit / ROOT_BYTES,
            },
            max_reserved_bytes: FAR_STORAGE_BUDGET_BYTES,
            max_cells: binding_limit / GPU_FAR_CELL_BYTES,
            page_bytes,
        }
    }

    pub fn for_tests() -> Self {
        Self::from_storage_binding_limit(BRICK_PAGE_BYTES)
    }

    pub fn is_usable(self) -> bool {
        self.dag.max_mixed_bricks_per_page > 0 && self.dag.max_roots > 0 && self.max_cells > 0
    }
}

impl Default for FarVoxelLimits {
    fn default() -> Self {
        Self::from_storage_binding_limit(BRICK_PAGE_BYTES)
    }
}

#[derive(Clone, Debug)]
pub struct FixedSlotMirror<T> {
    values: Vec<Option<T>>,
    free: BTreeSet<u32>,
}

impl<T> Default for FixedSlotMirror<T> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            free: BTreeSet::new(),
        }
    }
}

impl<T> FixedSlotMirror<T> {
    pub fn allocate(&mut self, value: T) -> u32 {
        if let Some(slot) = self.free.pop_first() {
            self.values[slot as usize] = Some(value);
            slot
        } else {
            let slot = self.values.len() as u32;
            self.values.push(Some(value));
            slot
        }
    }

    pub fn next_slot_and_reserved_len(&self) -> (u32, usize) {
        self.free
            .first()
            .copied()
            .map_or((self.values.len() as u32, self.values.len() + 1), |slot| {
                (slot, self.values.len())
            })
    }

    pub fn set(&mut self, slot: u32, value: T) {
        let index = slot as usize;
        if index >= self.values.len() {
            let old_len = self.values.len();
            self.values.resize_with(index + 1, || None);
            self.free.extend((old_len..index).map(|slot| slot as u32));
        }
        self.values[index] = Some(value);
        self.free.remove(&slot);
    }

    pub fn get(&self, slot: u32) -> Option<&T> {
        self.values.get(slot as usize)?.as_ref()
    }

    pub fn release(&mut self, slot: u32) -> Option<T> {
        let value = self.values.get_mut(slot as usize)?.take()?;
        self.free.insert(slot);
        while self.values.last().is_some_and(Option::is_none) {
            let last = (self.values.len() - 1) as u32;
            self.values.pop();
            self.free.remove(&last);
        }
        Some(value)
    }

    pub fn reserved_slots(&self) -> usize {
        self.values.len()
    }

    pub fn values(&self) -> &[Option<T>] {
        &self.values
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResidentFarCell {
    pub cell_slot: u32,
    pub root: RootHandle,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FarUpload {
    WriteMixed {
        page: u8,
        slot: u32,
        value: Arc<MixedBrick>,
    },
    WriteRoot {
        slot: u32,
        value: BrickRoot,
    },
    WriteCell {
        slot: u32,
        value: GpuFarCell,
    },
    ClearMixed {
        page: u8,
        slot: u32,
    },
    ClearRoot {
        slot: u32,
    },
    ClearCell {
        slot: u32,
    },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FarRefreshOutcome {
    Ready { resident: ResidentFarCell },
    Fallback { kept: Option<ResidentFarCell> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct FarUploadBatch {
    pub id: u64,
    pub uploads: Vec<FarUpload>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FarUploadStatus {
    Ack,
    Nack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FarUploadResult {
    pub id: u64,
    pub status: FarUploadStatus,
}

impl FarUploadResult {
    pub const fn ack(id: u64) -> Self {
        Self {
            id,
            status: FarUploadStatus::Ack,
        }
    }

    pub const fn nack(id: u64) -> Self {
        Self {
            id,
            status: FarUploadStatus::Nack,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FarStageOutcome {
    Pending {
        batch_id: u64,
        resident: ResidentFarCell,
    },
    Unchanged {
        resident: ResidentFarCell,
    },
    Busy,
    Fallback {
        kept: Option<ResidentFarCell>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FarMaintenanceOutcome {
    Pending { batch_id: u64 },
    Unchanged,
    Busy,
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FarRefreshAdmission {
    Any,
    FirstLoadsOnly,
    Blocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FarResolvedUpload {
    Committed {
        entity: Entity,
        resident: ResidentFarCell,
    },
    Rejected {
        entity: Entity,
        kept: Option<ResidentFarCell>,
    },
    Updated {
        entity: Entity,
    },
    Removed {
        entity: Entity,
    },
}

struct PendingFirstLoad {
    entity: Entity,
    stage: BrickDagStage,
    resident: ResidentFarCell,
}

struct PendingCellUpdate {
    entity: Entity,
    cell: GpuFarCell,
}

const MAX_CELL_UPDATES_PER_BATCH: usize = 256;

enum PendingFarMutationKind {
    FirstLoads {
        entries: Vec<PendingFirstLoad>,
    },
    Refresh {
        entity: Entity,
        stage: BrickDagStage,
        resident: ResidentFarCell,
    },
    CellUpdates {
        entries: Vec<PendingCellUpdate>,
    },
    Removal {
        entity: Entity,
    },
}

impl PendingFarMutationKind {
    fn singular_entity(&self) -> Entity {
        match self {
            Self::FirstLoads { .. } | Self::CellUpdates { .. } => {
                panic!("batched mutations contain multiple entities")
            }
            Self::Refresh { entity, .. } | Self::Removal { entity } => *entity,
        }
    }
}

struct PendingFarMutation {
    id: u64,
    kind: PendingFarMutationKind,
    prospective_cell_slots: usize,
    uploads: Vec<FarUpload>,
    mixed_writes: BTreeSet<(u8, u32)>,
    root_writes: BTreeSet<u32>,
    published: bool,
}

fn content_write_keys(uploads: &[FarUpload]) -> (BTreeSet<(u8, u32)>, BTreeSet<u32>) {
    let mut mixed = BTreeSet::new();
    let mut roots = BTreeSet::new();
    for upload in uploads {
        match upload {
            FarUpload::WriteMixed { page, slot, .. } => {
                mixed.insert((*page, *slot));
            }
            FarUpload::WriteRoot { slot, .. } => {
                roots.insert(*slot);
            }
            _ => {}
        }
    }
    (mixed, roots)
}

#[derive(Resource)]
pub struct FarVoxelResidency {
    dag: BrickDag,
    limits: FarVoxelLimits,
    resident: BTreeMap<Entity, ResidentFarCell>,
    mixed: [FixedSlotMirror<MixedBrick>; 2],
    roots: FixedSlotMirror<BrickRoot>,
    cells: FixedSlotMirror<GpuFarCell>,
    #[cfg(test)]
    uploads: Vec<FarUpload>,
    pending: Option<PendingFarMutation>,
    next_batch_id: u64,
}

impl FarVoxelResidency {
    pub fn new(mut limits: FarVoxelLimits) -> Self {
        limits.dag.max_reserved_bytes = limits.dag.max_reserved_bytes.min(
            limits
                .max_reserved_bytes
                .saturating_sub(FAR_STATIC_RESIDENT_BYTES),
        );
        Self {
            dag: BrickDag::with_limits(limits.dag),
            limits,
            resident: BTreeMap::new(),
            mixed: default(),
            roots: default(),
            cells: default(),
            #[cfg(test)]
            uploads: Vec::new(),
            pending: None,
            next_batch_id: 1,
        }
    }

    pub fn stage_refresh(
        &mut self,
        entity: Entity,
        prepared: PreparedBrickRoot,
        raster_biased_origin: Vec3,
        factor: u32,
    ) -> FarStageOutcome {
        let previous = self.resident.get(&entity).copied();
        let admission = self.refresh_admission();
        if matches!(admission, FarRefreshAdmission::Blocked)
            || (matches!(admission, FarRefreshAdmission::FirstLoadsOnly)
                && (previous.is_some() || self.pending_first_load_contains(entity)))
        {
            return FarStageOutcome::Busy;
        }
        let (cell_slot, prospective_cell_slots) = previous.map_or_else(
            || self.next_first_load_slot_and_reserved_len(),
            |resident| (resident.cell_slot, self.cells.reserved_slots()),
        );
        if prospective_cell_slots > self.limits.max_cells {
            return FarStageOutcome::Fallback { kept: previous };
        }
        let prospective_cell_bytes = self.cell_reserved_bytes_for(prospective_cell_slots);
        if self
            .dag
            .stats()
            .reserved_bytes
            .saturating_add(prospective_cell_bytes)
            > self.limits.dag.max_reserved_bytes
        {
            return FarStageOutcome::Fallback { kept: previous };
        }
        let previous_cell_bytes = self.pending.as_ref().map_or_else(
            || self.cell_reserved_bytes(),
            |pending| self.cell_reserved_bytes_for(pending.prospective_cell_slots),
        );
        self.dag.set_external_reserved_bytes(prospective_cell_bytes);
        let stage = match self
            .dag
            .stage_prepared(super::brick::CellId(entity.to_bits()), prepared)
        {
            Ok(stage) => stage,
            Err(_) => {
                self.dag.set_external_reserved_bytes(previous_cell_bytes);
                return FarStageOutcome::Fallback { kept: previous };
            }
        };
        let candidate = stage.candidate();
        let resident = ResidentFarCell {
            cell_slot,
            root: candidate,
        };
        let gpu_cell = GpuFarCell::new(raster_biased_origin, factor, candidate.0);
        let mut uploads = Vec::new();
        self.collect_root_writes(candidate, &mut uploads);
        if self.cells.get(cell_slot) != Some(&gpu_cell) {
            uploads.push(FarUpload::WriteCell {
                slot: cell_slot,
                value: gpu_cell,
            });
        }
        self.collect_predicted_stale_clears(&stage, &mut uploads);
        if uploads.is_empty() {
            self.dag.rollback_stage(stage);
            let committed_cell_bytes = self.cell_reserved_bytes();
            self.dag.set_external_reserved_bytes(committed_cell_bytes);
            return FarStageOutcome::Unchanged {
                resident: previous.expect("an upload-free stage has a committed resident"),
            };
        }
        if previous.is_none() {
            if let Some(pending) = self.pending.as_mut() {
                let PendingFarMutationKind::FirstLoads { entries } = &mut pending.kind else {
                    unreachable!("only an unpublished first-load batch admits another cell")
                };
                entries.push(PendingFirstLoad {
                    entity,
                    stage,
                    resident,
                });
                pending.prospective_cell_slots = prospective_cell_slots;
                for upload in uploads {
                    let duplicate = match &upload {
                        FarUpload::WriteMixed { page, slot, .. } => {
                            !pending.mixed_writes.insert((*page, *slot))
                        }
                        FarUpload::WriteRoot { slot, .. } => !pending.root_writes.insert(*slot),
                        _ => false,
                    };
                    if !duplicate {
                        pending.uploads.push(upload);
                    }
                }
                return FarStageOutcome::Pending {
                    batch_id: pending.id,
                    resident,
                };
            }
        }
        let batch_id = self.allocate_batch_id();
        let kind = if previous.is_none() {
            PendingFarMutationKind::FirstLoads {
                entries: vec![PendingFirstLoad {
                    entity,
                    stage,
                    resident,
                }],
            }
        } else {
            PendingFarMutationKind::Refresh {
                entity,
                stage,
                resident,
            }
        };
        let (mixed_writes, root_writes) = content_write_keys(&uploads);
        self.pending = Some(PendingFarMutation {
            id: batch_id,
            kind,
            prospective_cell_slots,
            uploads,
            mixed_writes,
            root_writes,
            published: false,
        });
        FarStageOutcome::Pending { batch_id, resident }
    }

    pub fn refresh_admission(&self) -> FarRefreshAdmission {
        match self.pending.as_ref() {
            None => FarRefreshAdmission::Any,
            Some(PendingFarMutation {
                kind: PendingFarMutationKind::FirstLoads { .. },
                published: false,
                ..
            }) => FarRefreshAdmission::FirstLoadsOnly,
            Some(_) => FarRefreshAdmission::Blocked,
        }
    }

    /// True while `entity` is already covered by the current first-load
    /// transaction, whether or not that transaction has been published.
    /// Changed data remains queued and is rebuilt after the ACK commits it.
    pub fn has_pending_first_load(&self, entity: Entity) -> bool {
        self.pending_first_load_contains(entity)
    }

    pub fn has_unpublished_first_loads(&self) -> bool {
        matches!(
            self.pending.as_ref(),
            Some(PendingFarMutation {
                kind: PendingFarMutationKind::FirstLoads { .. },
                published: false,
                ..
            })
        )
    }

    /// Benchmark-visible transactional work, including metadata maintenance
    /// which has no per-entity `FarVoxelUploadPending` component.
    pub fn pending_work_count(&self) -> usize {
        self.pending
            .as_ref()
            .map_or(0, |pending| match &pending.kind {
                PendingFarMutationKind::FirstLoads { entries } => entries.len(),
                PendingFarMutationKind::CellUpdates { entries } => entries.len(),
                PendingFarMutationKind::Refresh { .. } | PendingFarMutationKind::Removal { .. } => {
                    1
                }
            })
    }

    pub fn take_ready_upload_batch(
        &mut self,
        first_load_target: usize,
        first_load_work_exhausted: bool,
    ) -> Option<FarUploadBatch> {
        let pending = self.pending.as_mut()?;
        if pending.published {
            return None;
        }
        let ready = match &pending.kind {
            PendingFarMutationKind::FirstLoads { entries } => {
                entries.len() >= first_load_target.max(1) || first_load_work_exhausted
            }
            PendingFarMutationKind::Refresh { .. }
            | PendingFarMutationKind::CellUpdates { .. }
            | PendingFarMutationKind::Removal { .. } => true,
        };
        if !ready {
            return None;
        }
        pending.published = true;
        Some(FarUploadBatch {
            id: pending.id,
            uploads: pending.uploads.clone(),
        })
    }

    #[cfg(test)]
    pub fn take_upload_batch(&mut self) -> Option<FarUploadBatch> {
        self.take_ready_upload_batch(1, true)
    }

    pub fn stage_cell_update(&mut self, entity: Entity, cell: GpuFarCell) -> FarMaintenanceOutcome {
        let Some(resident) = self.resident.get(&entity).copied() else {
            return FarMaintenanceOutcome::Missing;
        };
        let committed = self.cells.get(resident.cell_slot).copied();
        if let Some(pending) = self.pending.as_mut() {
            let PendingFarMutationKind::CellUpdates { entries } = &mut pending.kind else {
                return FarMaintenanceOutcome::Busy;
            };
            if pending.published {
                return FarMaintenanceOutcome::Busy;
            }
            if let Some(entry) = entries.iter_mut().find(|entry| entry.entity == entity) {
                entry.cell = cell;
                let Some(FarUpload::WriteCell { value, .. }) = pending.uploads.iter_mut().find(
                    |upload| matches!(upload, FarUpload::WriteCell { slot, .. } if *slot == resident.cell_slot),
                ) else {
                    unreachable!("every pending cell update owns one cell write")
                };
                *value = cell;
                return FarMaintenanceOutcome::Pending {
                    batch_id: pending.id,
                };
            }
            if committed == Some(cell) {
                return FarMaintenanceOutcome::Unchanged;
            }
            if entries.len() >= MAX_CELL_UPDATES_PER_BATCH {
                return FarMaintenanceOutcome::Busy;
            }
            entries.push(PendingCellUpdate { entity, cell });
            pending.uploads.push(FarUpload::WriteCell {
                slot: resident.cell_slot,
                value: cell,
            });
            return FarMaintenanceOutcome::Pending {
                batch_id: pending.id,
            };
        }
        if committed == Some(cell) {
            return FarMaintenanceOutcome::Unchanged;
        }
        let batch_id = self.allocate_batch_id();
        self.pending = Some(PendingFarMutation {
            id: batch_id,
            kind: PendingFarMutationKind::CellUpdates {
                entries: vec![PendingCellUpdate { entity, cell }],
            },
            prospective_cell_slots: self.cells.reserved_slots(),
            uploads: vec![FarUpload::WriteCell {
                slot: resident.cell_slot,
                value: cell,
            }],
            mixed_writes: BTreeSet::new(),
            root_writes: BTreeSet::new(),
            published: false,
        });
        FarMaintenanceOutcome::Pending { batch_id }
    }

    pub fn stage_remove(&mut self, entity: Entity) -> FarMaintenanceOutcome {
        if self.pending.is_some() {
            return FarMaintenanceOutcome::Busy;
        }
        let Some(resident) = self.resident.get(&entity).copied() else {
            return FarMaintenanceOutcome::Missing;
        };
        let mut uploads = vec![FarUpload::ClearCell {
            slot: resident.cell_slot,
        }];
        self.collect_root_release_clears(resident.root, &mut uploads);
        let batch_id = self.allocate_batch_id();
        self.pending = Some(PendingFarMutation {
            id: batch_id,
            kind: PendingFarMutationKind::Removal { entity },
            prospective_cell_slots: self.cells.reserved_slots(),
            uploads,
            mixed_writes: BTreeSet::new(),
            root_writes: BTreeSet::new(),
            published: false,
        });
        FarMaintenanceOutcome::Pending { batch_id }
    }

    fn allocate_batch_id(&mut self) -> u64 {
        let batch_id = self.next_batch_id;
        self.next_batch_id = self.next_batch_id.wrapping_add(1).max(1);
        batch_id
    }

    pub fn resolve_upload(&mut self, result: FarUploadResult) -> Option<Vec<FarResolvedUpload>> {
        if self.pending.as_ref()?.id != result.id {
            return None;
        }
        let pending = self.pending.take().unwrap();
        match result.status {
            FarUploadStatus::Ack => match pending.kind {
                PendingFarMutationKind::FirstLoads { entries } => {
                    let mut resolved = Vec::with_capacity(entries.len());
                    for PendingFirstLoad {
                        entity,
                        stage,
                        resident,
                    } in entries
                    {
                        self.dag.commit_stage(stage);
                        self.resident.insert(entity, resident);
                        resolved.push(FarResolvedUpload::Committed { entity, resident });
                    }
                    self.apply_uploads_to_mirrors(&pending.uploads);
                    let committed_cell_bytes = self.cell_reserved_bytes();
                    self.dag.set_external_reserved_bytes(committed_cell_bytes);
                    Some(resolved)
                }
                PendingFarMutationKind::Refresh {
                    entity,
                    stage,
                    resident,
                } => {
                    self.dag.commit_stage(stage);
                    self.apply_uploads_to_mirrors(&pending.uploads);
                    self.resident.insert(entity, resident);
                    let committed_cell_bytes = self.cell_reserved_bytes();
                    self.dag.set_external_reserved_bytes(committed_cell_bytes);
                    Some(vec![FarResolvedUpload::Committed { entity, resident }])
                }
                PendingFarMutationKind::CellUpdates { entries } => {
                    self.apply_uploads_to_mirrors(&pending.uploads);
                    Some(
                        entries
                            .into_iter()
                            .map(|entry| FarResolvedUpload::Updated {
                                entity: entry.entity,
                            })
                            .collect(),
                    )
                }
                PendingFarMutationKind::Removal { entity } => {
                    self.dag.remove_cell(super::brick::CellId(entity.to_bits()));
                    self.apply_uploads_to_mirrors(&pending.uploads);
                    self.resident.remove(&entity);
                    let committed_cell_bytes = self.cell_reserved_bytes();
                    self.dag.set_external_reserved_bytes(committed_cell_bytes);
                    Some(vec![FarResolvedUpload::Removed { entity }])
                }
            },
            FarUploadStatus::Nack => {
                let resolved = match pending.kind {
                    PendingFarMutationKind::FirstLoads { entries } => {
                        let resolved = entries
                            .iter()
                            .map(|entry| FarResolvedUpload::Rejected {
                                entity: entry.entity,
                                kept: None,
                            })
                            .collect();
                        for entry in entries.into_iter().rev() {
                            self.dag.rollback_stage(entry.stage);
                        }
                        resolved
                    }
                    PendingFarMutationKind::Refresh { entity, stage, .. } => {
                        let kept = self.resident.get(&entity).copied();
                        self.dag.rollback_stage(stage);
                        vec![FarResolvedUpload::Rejected { entity, kept }]
                    }
                    PendingFarMutationKind::CellUpdates { entries } => entries
                        .into_iter()
                        .map(|entry| FarResolvedUpload::Rejected {
                            entity: entry.entity,
                            kept: self.resident.get(&entry.entity).copied(),
                        })
                        .collect(),
                    kind => {
                        let entity = kind.singular_entity();
                        let kept = self.resident.get(&entity).copied();
                        vec![FarResolvedUpload::Rejected { entity, kept }]
                    }
                };
                let committed_cell_bytes = self.cell_reserved_bytes();
                self.dag.set_external_reserved_bytes(committed_cell_bytes);
                Some(resolved)
            }
        }
    }

    #[cfg(test)]
    fn refresh(
        &mut self,
        entity: Entity,
        prepared: PreparedBrickRoot,
        raster_biased_origin: Vec3,
        factor: u32,
    ) -> FarRefreshOutcome {
        let previous = self.resident.get(&entity).copied();
        let (prospective_slot, prospective_cell_slots) = previous.map_or_else(
            || self.cells.next_slot_and_reserved_len(),
            |resident| (resident.cell_slot, self.cells.reserved_slots()),
        );
        if prospective_cell_slots > self.limits.max_cells {
            return FarRefreshOutcome::Fallback { kept: previous };
        }
        let prospective_cell_bytes = self.cell_reserved_bytes_for(prospective_cell_slots);
        if self
            .dag
            .stats()
            .reserved_bytes
            .saturating_add(prospective_cell_bytes)
            > self.limits.dag.max_reserved_bytes
        {
            return FarRefreshOutcome::Fallback { kept: previous };
        }
        self.dag.set_external_reserved_bytes(prospective_cell_bytes);

        let old_root = previous.and_then(|resident| {
            self.dag
                .root(resident.root)
                .copied()
                .map(|root| (resident.root, root))
        });
        let outcome = self
            .dag
            .refresh_prepared(super::brick::CellId(entity.to_bits()), prepared);
        let Some(root) = outcome.root() else {
            let cell_reserved_bytes = self.cell_reserved_bytes();
            self.dag.set_external_reserved_bytes(cell_reserved_bytes);
            return FarRefreshOutcome::Fallback { kept: previous };
        };
        if matches!(outcome, RefreshOutcome::Fallback { .. }) {
            let cell_reserved_bytes = self.cell_reserved_bytes();
            self.dag.set_external_reserved_bytes(cell_reserved_bytes);
            return FarRefreshOutcome::Fallback { kept: previous };
        }

        self.upload_root_content(root);
        let cell_slot = match previous {
            Some(resident) => resident.cell_slot,
            None => {
                let allocated = self.cells.allocate(GpuFarCell::INACTIVE);
                debug_assert_eq!(allocated, prospective_slot);
                allocated
            }
        };
        let gpu_cell = GpuFarCell::new(raster_biased_origin, factor, root.0);
        if self.cells.get(cell_slot) != Some(&gpu_cell) {
            self.cells.set(cell_slot, gpu_cell);
            self.uploads.push(FarUpload::WriteCell {
                slot: cell_slot,
                value: gpu_cell,
            });
        }
        let resident = ResidentFarCell { cell_slot, root };
        self.resident.insert(entity, resident);

        if let Some((old_handle, old_value)) = old_root {
            self.release_stale_content(old_handle, old_value);
        }
        let cell_reserved_bytes = self.cell_reserved_bytes();
        self.dag.set_external_reserved_bytes(cell_reserved_bytes);
        FarRefreshOutcome::Ready { resident }
    }

    pub fn cell_metadata(&self, entity: Entity) -> Option<&GpuFarCell> {
        let resident = self.resident(entity)?;
        self.cells.get(resident.cell_slot)
    }

    #[cfg(test)]
    fn remove(&mut self, entity: Entity) -> Option<ResidentFarCell> {
        let resident = self.resident.remove(&entity)?;
        let old_root = self.dag.root(resident.root).copied();
        self.dag.remove_cell(super::brick::CellId(entity.to_bits()));
        self.cells.release(resident.cell_slot);
        self.uploads.push(FarUpload::ClearCell {
            slot: resident.cell_slot,
        });
        if let Some(old_root) = old_root {
            self.release_stale_content(resident.root, old_root);
        }
        let cell_reserved_bytes = self.cell_reserved_bytes();
        self.dag.set_external_reserved_bytes(cell_reserved_bytes);
        Some(resident)
    }

    pub fn resident(&self, entity: Entity) -> Option<&ResidentFarCell> {
        self.resident.get(&entity)
    }

    pub fn reconstruct_cell(&self, entity: Entity) -> Option<Vec<game::Voxel>> {
        self.dag.reconstruct(self.resident(entity)?.root)
    }

    #[cfg(test)]
    fn take_uploads(&mut self) -> Vec<FarUpload> {
        std::mem::take(&mut self.uploads)
    }

    pub fn reserved_bytes(&self) -> usize {
        let cell_slots = self
            .pending
            .as_ref()
            .map_or(self.cells.reserved_slots(), |pending| {
                pending.prospective_cell_slots
            });
        FAR_STATIC_RESIDENT_BYTES
            + self.dag.stats().reserved_bytes
            + self.cell_reserved_bytes_for(cell_slots)
    }

    pub fn unique_bricks(&self) -> usize {
        self.dag.stats().unique_mixed_bricks
    }

    pub fn brick_reference_count(&self) -> usize {
        self.dag.cell_mixed_reference_count()
    }

    pub fn brick_dedup_ratio(&self) -> Option<f64> {
        let unique = self.unique_bricks();
        (unique != 0).then(|| self.brick_reference_count() as f64 / unique as f64)
    }

    fn pending_first_load_contains(&self, entity: Entity) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            matches!(
                &pending.kind,
                PendingFarMutationKind::FirstLoads { entries }
                    if entries.iter().any(|entry| entry.entity == entity)
            )
        })
    }

    fn next_first_load_slot_and_reserved_len(&self) -> (u32, usize) {
        let reserved = self
            .pending
            .as_ref()
            .and_then(|pending| match &pending.kind {
                PendingFarMutationKind::FirstLoads { entries } => Some(
                    entries
                        .iter()
                        .map(|entry| entry.resident.cell_slot)
                        .collect::<BTreeSet<_>>(),
                ),
                _ => None,
            })
            .unwrap_or_default();
        if let Some(slot) = self
            .cells
            .free
            .iter()
            .copied()
            .find(|slot| !reserved.contains(slot))
        {
            return (slot, self.cells.reserved_slots());
        }
        let mut slot = self.cells.reserved_slots() as u32;
        while reserved.contains(&slot) {
            slot += 1;
        }
        (slot, slot as usize + 1)
    }

    fn cell_reserved_bytes(&self) -> usize {
        self.cell_reserved_bytes_for(self.cells.reserved_slots())
    }

    fn cell_reserved_bytes_for(&self, slots: usize) -> usize {
        let capacity = geometric_slot_capacity(slots, self.limits.max_cells);
        capacity * GPU_FAR_CELL_BYTES + capacity.saturating_sub(1) * GPU_FAR_VISIBLE_SLOT_BYTES
    }

    fn collect_root_writes(&self, root: RootHandle, uploads: &mut Vec<FarUpload>) {
        let root_value = *self.dag.root(root).expect("staged root is live");
        let mut entries = BTreeMap::new();
        for entry in root_value.entries {
            if let RootEntryKind::Mixed { page, slot } = entry.kind() {
                entries.insert((page, slot), entry);
            }
        }
        for ((page, slot), entry) in entries {
            let brick = *self
                .dag
                .mixed_brick(entry)
                .expect("staged mixed brick is live");
            if self.mixed[page as usize].get(slot) != Some(&brick) {
                uploads.push(FarUpload::WriteMixed {
                    page,
                    slot,
                    value: Arc::new(brick),
                });
            }
        }
        if self.roots.get(root.0) != Some(&root_value) {
            uploads.push(FarUpload::WriteRoot {
                slot: root.0,
                value: root_value,
            });
        }
    }

    fn collect_predicted_stale_clears(&self, stage: &BrickDagStage, uploads: &mut Vec<FarUpload>) {
        let Some(old) = stage.previous() else {
            return;
        };
        if old == stage.candidate() || self.dag.root_ref_count(old) != Some(1) {
            return;
        }
        let old_root = *self.dag.root(old).expect("committed old root is live");
        uploads.push(FarUpload::ClearRoot { slot: old.0 });
        let mut occurrences = BTreeMap::<(u8, u32), (RootEntry, u32)>::new();
        for entry in old_root.entries {
            if let RootEntryKind::Mixed { page, slot } = entry.kind() {
                occurrences
                    .entry((page, slot))
                    .and_modify(|(_, count)| *count += 1)
                    .or_insert((entry, 1));
            }
        }
        for ((page, slot), (entry, count)) in occurrences {
            if self.dag.mixed_ref_count(entry) == Some(count) {
                uploads.push(FarUpload::ClearMixed { page, slot });
            }
        }
    }

    fn collect_root_release_clears(&self, root: RootHandle, uploads: &mut Vec<FarUpload>) {
        if self.dag.root_ref_count(root) != Some(1) {
            return;
        }
        let root_value = *self.dag.root(root).expect("committed root is live");
        uploads.push(FarUpload::ClearRoot { slot: root.0 });
        let mut occurrences = BTreeMap::<(u8, u32), (RootEntry, u32)>::new();
        for entry in root_value.entries {
            if let RootEntryKind::Mixed { page, slot } = entry.kind() {
                occurrences
                    .entry((page, slot))
                    .and_modify(|(_, count)| *count += 1)
                    .or_insert((entry, 1));
            }
        }
        for ((page, slot), (entry, count)) in occurrences {
            if self.dag.mixed_ref_count(entry) == Some(count) {
                uploads.push(FarUpload::ClearMixed { page, slot });
            }
        }
    }

    fn apply_uploads_to_mirrors(&mut self, uploads: &[FarUpload]) {
        for upload in uploads {
            match upload {
                FarUpload::WriteMixed { page, slot, value } => {
                    self.mixed[*page as usize].set(*slot, **value);
                }
                FarUpload::WriteRoot { slot, value } => self.roots.set(*slot, *value),
                FarUpload::WriteCell { slot, value } => self.cells.set(*slot, *value),
                FarUpload::ClearMixed { page, slot } => {
                    self.mixed[*page as usize].release(*slot);
                }
                FarUpload::ClearRoot { slot } => {
                    self.roots.release(*slot);
                }
                FarUpload::ClearCell { slot } => {
                    self.cells.release(*slot);
                }
            }
        }
    }

    #[cfg(test)]
    fn upload_root_content(&mut self, root: RootHandle) {
        let root_value = *self.dag.root(root).expect("new root is live");
        let mut entries = BTreeMap::new();
        for entry in root_value.entries {
            if let RootEntryKind::Mixed { page, slot } = entry.kind() {
                entries.insert((page, slot), entry);
            }
        }
        for ((page, slot), entry) in entries {
            let brick = *self
                .dag
                .mixed_brick(entry)
                .expect("new mixed brick is live");
            if self.mixed[page as usize].get(slot) != Some(&brick) {
                self.mixed[page as usize].set(slot, brick);
                self.uploads.push(FarUpload::WriteMixed {
                    page,
                    slot,
                    value: Arc::new(brick),
                });
            }
        }
        if self.roots.get(root.0) != Some(&root_value) {
            self.roots.set(root.0, root_value);
            self.uploads.push(FarUpload::WriteRoot {
                slot: root.0,
                value: root_value,
            });
        }
    }

    #[cfg(test)]
    fn release_stale_content(&mut self, old_handle: RootHandle, old_root: BrickRoot) {
        if self.dag.root(old_handle).is_none() && self.roots.release(old_handle.0).is_some() {
            self.uploads
                .push(FarUpload::ClearRoot { slot: old_handle.0 });
        }
        let mut entries = BTreeMap::<(u8, u32), RootEntry>::new();
        for entry in old_root.entries {
            if let RootEntryKind::Mixed { page, slot } = entry.kind() {
                entries.insert((page, slot), entry);
            }
        }
        for ((page, slot), entry) in entries {
            if self.dag.mixed_brick(entry).is_none()
                && self.mixed[page as usize].release(slot).is_some()
            {
                self.uploads.push(FarUpload::ClearMixed { page, slot });
            }
        }
    }
}

impl Default for FarVoxelResidency {
    fn default() -> Self {
        Self::new(FarVoxelLimits::default())
    }
}

#[cfg(test)]
mod tests {
    use bevy::prelude::*;
    use game::{BlockId, Voxel, BRICK_VOXEL_COUNT};

    use super::*;
    use crate::renderer::far_voxel::brick::{
        DagLimits, PreparedBrickRoot, BRICK_PAGE_BYTES, FAR_STORAGE_BUDGET_BYTES,
        MIXED_BRICK_BYTES, ROOT_BYTES,
    };

    fn sparse(material: BlockId) -> PreparedBrickRoot {
        let mut voxels = vec![Voxel::new(BlockId::AIR); BRICK_VOXEL_COUNT];
        voxels[0] = Voxel::new(material);
        PreparedBrickRoot::from_voxels(&voxels).unwrap()
    }

    #[test]
    fn fixed_slot_mirror_reuses_lowest_slot_and_trims_trailing_free_slots() {
        let mut mirror = FixedSlotMirror::default();
        let zero = GpuFarCell::INACTIVE;
        let first = mirror.allocate(zero);
        let second = mirror.allocate(zero);
        let third = mirror.allocate(zero);
        assert_eq!((first, second, third), (0, 1, 2));

        mirror.release(second);
        assert_eq!(mirror.allocate(zero), second);
        mirror.release(third);
        mirror.release(second);

        assert_eq!(mirror.reserved_slots(), 1);
    }

    #[test]
    fn gpu_far_cell_layout_contains_true_origin_factor_root_and_neighbors() {
        fn assert_pod<T: bytemuck::Pod + bytemuck::Zeroable>() {}
        assert_pod::<GpuFarCell>();
        assert_eq!(std::mem::size_of::<GpuFarCell>(), GPU_FAR_CELL_BYTES);
        assert_eq!(std::mem::align_of::<GpuFarCell>(), 4);

        let mut cell = GpuFarCell::new(Vec3::new(64.0, 126.0, -32.0), 2, 7);
        cell.set_neighbor(NeighborOffset::new(1, 0, -1).unwrap(), 13);

        assert_eq!(cell.origin, [64.0, 128.0, -32.0]);
        assert_eq!(cell.factor, 2);
        assert_eq!(cell.root_handle, 7);
        assert_eq!(
            cell.neighbor(NeighborOffset::new(1, 0, -1).unwrap()),
            Some(13)
        );
        assert_eq!(cell.neighbor(NeighborOffset::new(-1, 0, 1).unwrap()), None);
    }

    #[test]
    fn storage_limits_cap_each_static_page_and_the_combined_budget() {
        let binding_limit = 64 * 1024 * 1024;
        let limits = FarVoxelLimits::from_storage_binding_limit(binding_limit);

        assert_eq!(limits.max_reserved_bytes, FAR_STORAGE_BUDGET_BYTES);
        assert_eq!(
            limits.dag.max_reserved_bytes + FAR_STATIC_RESIDENT_BYTES,
            limits.max_reserved_bytes
        );
        assert_eq!(
            limits.dag.max_mixed_bricks_per_page,
            binding_limit / MIXED_BRICK_BYTES
        );
        assert_eq!(limits.dag.max_roots, binding_limit / ROOT_BYTES);
        assert_eq!(limits.max_cells, binding_limit / GPU_FAR_CELL_BYTES);
        assert!(limits.page_bytes <= BRICK_PAGE_BYTES);
        assert_eq!(GPU_FAR_CELL_RESIDENT_BYTES, GPU_FAR_CELL_BYTES + 4);
        assert_eq!(FAR_MATERIAL_LOOKUP_BYTES, 512 * 1024);
        assert_eq!(FAR_SINGLE_VIEW_PASS_FIXED_BYTES, 128);
        assert_eq!(
            limits.dag.max_reserved_bytes + FAR_STATIC_RESIDENT_BYTES,
            limits.max_reserved_bytes
        );
    }

    #[test]
    fn zero_cell_budget_includes_one_visible_slot_without_double_counting_cell_growth() {
        let residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());

        assert_eq!(
            FAR_STATIC_RESIDENT_BYTES,
            FAR_STATIC_BINDING_OVERHEAD_BYTES
                + FAR_MATERIAL_LOOKUP_BYTES
                + FAR_SINGLE_VIEW_PASS_FIXED_BYTES
                + GPU_FAR_VISIBLE_SLOT_BYTES
        );
        assert_eq!(residency.cell_reserved_bytes_for(0), 0);
        assert_eq!(residency.cell_reserved_bytes_for(1), GPU_FAR_CELL_BYTES);
        assert_eq!(
            residency.cell_reserved_bytes_for(2),
            2 * GPU_FAR_CELL_BYTES + GPU_FAR_VISIBLE_SLOT_BYTES
        );
    }

    #[test]
    fn refresh_uploads_new_immutable_content_then_swaps_cell_then_releases_old() {
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());
        let entity = Entity::from_bits(17);
        let first = residency.refresh(entity, sparse(BlockId(4)), Vec3::new(0.0, -2.0, 0.0), 2);
        assert!(matches!(first, FarRefreshOutcome::Ready { .. }));
        let first_uploads = residency.take_uploads();
        assert!(matches!(first_uploads[0], FarUpload::WriteMixed { .. }));
        assert!(matches!(first_uploads[1], FarUpload::WriteRoot { .. }));
        assert!(matches!(first_uploads[2], FarUpload::WriteCell { .. }));

        let replaced = residency.refresh(entity, sparse(BlockId(5)), Vec3::new(0.0, -2.0, 0.0), 2);
        assert!(matches!(replaced, FarRefreshOutcome::Ready { .. }));
        let uploads = residency.take_uploads();
        let cell_swap = uploads
            .iter()
            .position(|upload| matches!(upload, FarUpload::WriteCell { .. }))
            .unwrap();
        assert!(uploads[..cell_swap].iter().any(|upload| matches!(
            upload,
            FarUpload::WriteMixed { .. } | FarUpload::WriteRoot { .. }
        )));
        assert!(uploads[cell_swap + 1..].iter().any(|upload| matches!(
            upload,
            FarUpload::ClearMixed { .. } | FarUpload::ClearRoot { .. }
        )));
    }

    #[test]
    fn failed_refresh_keeps_old_resident_cell_and_emits_no_partial_uploads() {
        let limits = FarVoxelLimits {
            dag: DagLimits {
                max_reserved_bytes: ROOT_BYTES + MIXED_BRICK_BYTES + GPU_FAR_CELL_RESIDENT_BYTES,
                max_mixed_bricks_per_page: 1,
                max_roots: 1,
            },
            max_reserved_bytes: ROOT_BYTES
                + MIXED_BRICK_BYTES
                + GPU_FAR_CELL_RESIDENT_BYTES
                + FAR_STATIC_RESIDENT_BYTES,
            max_cells: 1,
            page_bytes: MIXED_BRICK_BYTES,
        };
        let mut residency = FarVoxelResidency::new(limits);
        let entity = Entity::from_bits(19);
        assert!(matches!(
            residency.refresh(entity, sparse(BlockId(1)), Vec3::ZERO, 2),
            FarRefreshOutcome::Ready { .. }
        ));
        residency.take_uploads();
        let before = residency.resident(entity).copied().unwrap();

        assert_eq!(
            residency.refresh(entity, sparse(BlockId(2)), Vec3::ZERO, 2),
            FarRefreshOutcome::Fallback { kept: Some(before) }
        );
        assert_eq!(residency.resident(entity), Some(&before));
        assert!(residency.take_uploads().is_empty());
    }

    #[test]
    fn deduplicated_content_still_falls_back_when_cell_storage_exceeds_budget() {
        let content_and_one_cell = ROOT_BYTES + MIXED_BRICK_BYTES + GPU_FAR_CELL_RESIDENT_BYTES;
        let limits = FarVoxelLimits {
            dag: DagLimits {
                max_reserved_bytes: content_and_one_cell,
                max_mixed_bricks_per_page: 2,
                max_roots: 2,
            },
            max_reserved_bytes: content_and_one_cell + FAR_STATIC_RESIDENT_BYTES,
            max_cells: 2,
            page_bytes: 2 * MIXED_BRICK_BYTES,
        };
        let mut residency = FarVoxelResidency::new(limits);
        let first = Entity::from_bits(20);
        let second = Entity::from_bits(21);
        assert!(matches!(
            residency.refresh(first, sparse(BlockId(4)), Vec3::ZERO, 2),
            FarRefreshOutcome::Ready { .. }
        ));
        residency.take_uploads();

        assert_eq!(
            residency.refresh(second, sparse(BlockId(4)), Vec3::X * 64.0, 2),
            FarRefreshOutcome::Fallback { kept: None }
        );
        assert!(residency.resident(second).is_none());
        assert!(residency.take_uploads().is_empty());
    }

    #[test]
    fn removal_returns_reserved_capacity_to_the_pre_churn_level() {
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());
        let baseline = residency.reserved_bytes();
        let entity = Entity::from_bits(23);
        residency.refresh(entity, sparse(BlockId(7)), Vec3::ZERO, 2);
        assert!(residency.reserved_bytes() > baseline);
        residency.take_uploads();

        residency.remove(entity);

        assert_eq!(residency.reserved_bytes(), baseline);
        assert!(matches!(
            residency.take_uploads().as_slice(),
            [
                FarUpload::ClearCell { .. },
                FarUpload::ClearRoot { .. },
                FarUpload::ClearMixed { .. }
            ]
        ));
    }

    #[test]
    fn dedup_ratio_counts_references_from_every_resident_cell() {
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());
        residency.refresh(Entity::from_bits(31), sparse(BlockId(2)), Vec3::ZERO, 2);
        residency.refresh(Entity::from_bits(32), sparse(BlockId(2)), Vec3::X * 64.0, 2);

        assert_eq!(residency.unique_bricks(), 1);
        assert_eq!(residency.brick_reference_count(), 2);
        assert_eq!(residency.brick_dedup_ratio(), Some(2.0));
    }

    #[test]
    fn first_load_becomes_resident_only_after_upload_ack_and_nack_rolls_back() {
        let entity = Entity::from_bits(40);
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());
        let baseline = residency.reserved_bytes();
        let pending = residency.stage_refresh(entity, sparse(BlockId(6)), Vec3::ZERO, 2);
        let FarStageOutcome::Pending { batch_id, .. } = pending else {
            panic!("first load must wait for a render ACK");
        };
        assert!(residency.resident(entity).is_none());
        assert!(residency.cell_metadata(entity).is_none());
        let batch = residency.take_upload_batch().unwrap();
        assert_eq!(batch.id, batch_id);

        residency.resolve_upload(FarUploadResult::nack(batch.id));
        assert!(residency.resident(entity).is_none());
        assert_eq!(residency.reserved_bytes(), baseline);

        let pending = residency.stage_refresh(entity, sparse(BlockId(6)), Vec3::ZERO, 2);
        let FarStageOutcome::Pending { batch_id, .. } = pending else {
            panic!("retried first load must wait for a render ACK");
        };
        let batch = residency.take_upload_batch().unwrap();
        assert_eq!(batch.id, batch_id);
        residency.resolve_upload(FarUploadResult::ack(batch.id));
        assert!(residency.resident(entity).is_some());
        assert_eq!(residency.cell_metadata(entity).unwrap().root_handle, 0);
    }

    #[test]
    fn unpublished_first_loads_share_one_ack_transaction_and_commit_together() {
        let first = Entity::from_bits(50);
        let second = Entity::from_bits(51);
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());

        let FarStageOutcome::Pending {
            batch_id,
            resident: first_candidate,
        } = residency.stage_refresh(first, sparse(BlockId(6)), Vec3::ZERO, 2)
        else {
            panic!("first cell must stage");
        };
        let FarStageOutcome::Pending {
            batch_id: second_batch_id,
            resident: second_candidate,
        } = residency.stage_refresh(second, sparse(BlockId(6)), Vec3::X * 64.0, 2)
        else {
            panic!("second first-load must join the unpublished batch");
        };

        assert_eq!(second_batch_id, batch_id);
        assert!(residency.has_pending_first_load(first));
        assert!(residency.has_pending_first_load(second));
        assert_eq!(residency.pending_work_count(), 2);
        assert_ne!(first_candidate.cell_slot, second_candidate.cell_slot);
        assert_eq!(first_candidate.root, second_candidate.root);
        assert!(residency.resident(first).is_none());
        assert!(residency.resident(second).is_none());
        let batch = residency.take_ready_upload_batch(2, false).unwrap();
        assert!(residency.has_pending_first_load(first));
        assert_eq!(batch.id, batch_id);
        let cloned = batch.clone();
        let original_brick = batch.uploads.iter().find_map(|upload| match upload {
            FarUpload::WriteMixed { value, .. } => Some(value),
            _ => None,
        });
        let cloned_brick = cloned.uploads.iter().find_map(|upload| match upload {
            FarUpload::WriteMixed { value, .. } => Some(value),
            _ => None,
        });
        assert!(std::ptr::eq(
            original_brick.unwrap().as_ref(),
            cloned_brick.unwrap().as_ref(),
        ));
        assert_eq!(
            batch
                .uploads
                .iter()
                .filter(|upload| matches!(upload, FarUpload::WriteMixed { .. }))
                .count(),
            1,
            "shared immutable brick content is uploaded once per transaction"
        );
        assert_eq!(
            batch
                .uploads
                .iter()
                .filter(|upload| matches!(upload, FarUpload::WriteRoot { .. }))
                .count(),
            1,
            "shared immutable roots are uploaded once per transaction"
        );
        assert_eq!(
            batch
                .uploads
                .iter()
                .filter(|upload| matches!(upload, FarUpload::WriteCell { .. }))
                .count(),
            2
        );
        assert!(residency.take_upload_batch().is_none());

        residency.resolve_upload(FarUploadResult::ack(batch_id));
        assert!(!residency.has_pending_first_load(first));
        assert_eq!(residency.pending_work_count(), 0);
        assert_eq!(residency.resident(first), Some(&first_candidate));
        assert_eq!(residency.resident(second), Some(&second_candidate));
        assert_eq!(residency.dag.root_ref_count(first_candidate.root), Some(2));
        assert_eq!(residency.unique_bricks(), 1);
        assert_eq!(residency.brick_reference_count(), 2);
    }

    #[test]
    fn nacked_first_load_batch_rolls_back_all_refs_and_reuses_slots() {
        let first = Entity::from_bits(52);
        let second = Entity::from_bits(53);
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());
        let baseline = residency.reserved_bytes();

        let FarStageOutcome::Pending { batch_id, .. } =
            residency.stage_refresh(first, sparse(BlockId(7)), Vec3::ZERO, 2)
        else {
            panic!("first cell must stage");
        };
        assert!(matches!(
            residency.stage_refresh(second, sparse(BlockId(7)), Vec3::X * 64.0, 2),
            FarStageOutcome::Pending {
                batch_id: joined,
                ..
            } if joined == batch_id
        ));
        residency.take_upload_batch().unwrap();
        residency.resolve_upload(FarUploadResult::nack(batch_id));

        assert!(residency.resident(first).is_none());
        assert!(residency.resident(second).is_none());
        assert_eq!(residency.reserved_bytes(), baseline);
        assert_eq!(residency.unique_bricks(), 0);
        let FarStageOutcome::Pending { resident, .. } =
            residency.stage_refresh(second, sparse(BlockId(7)), Vec3::ZERO, 2)
        else {
            panic!("retry must stage");
        };
        assert_eq!(resident.cell_slot, 0);
        assert_eq!(resident.root, RootHandle(0));
    }

    #[test]
    fn published_first_load_and_replacement_refreshes_remain_serial() {
        let first = Entity::from_bits(54);
        let second = Entity::from_bits(55);
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());
        assert_eq!(residency.refresh_admission(), FarRefreshAdmission::Any);
        let FarStageOutcome::Pending { batch_id, .. } =
            residency.stage_refresh(first, sparse(BlockId(8)), Vec3::ZERO, 2)
        else {
            panic!("first cell must stage");
        };
        assert_eq!(
            residency.refresh_admission(),
            FarRefreshAdmission::FirstLoadsOnly
        );
        residency.take_upload_batch().unwrap();
        assert_eq!(residency.refresh_admission(), FarRefreshAdmission::Blocked);
        assert!(matches!(
            residency.stage_refresh(second, sparse(BlockId(8)), Vec3::X * 64.0, 2),
            FarStageOutcome::Busy
        ));
        residency.resolve_upload(FarUploadResult::ack(batch_id));

        let FarStageOutcome::Pending { batch_id, .. } =
            residency.stage_refresh(first, sparse(BlockId(9)), Vec3::ZERO, 2)
        else {
            panic!("replacement must stage serially");
        };
        assert_eq!(residency.refresh_admission(), FarRefreshAdmission::Blocked);
        assert!(matches!(
            residency.stage_refresh(second, sparse(BlockId(9)), Vec3::X * 64.0, 2),
            FarStageOutcome::Busy
        ));
        let batch = residency.take_upload_batch().unwrap();
        assert_eq!(batch.id, batch_id);
        assert!(batch.uploads.iter().any(|upload| matches!(
            upload,
            FarUpload::ClearRoot { .. } | FarUpload::ClearMixed { .. }
        )));
    }

    #[test]
    fn first_load_batch_budget_failure_keeps_prior_staged_cells_valid() {
        let mut limits = FarVoxelLimits::for_tests();
        limits.max_cells = 1;
        let first = Entity::from_bits(56);
        let second = Entity::from_bits(57);
        let mut residency = FarVoxelResidency::new(limits);
        let FarStageOutcome::Pending { batch_id, resident } =
            residency.stage_refresh(first, sparse(BlockId(10)), Vec3::ZERO, 2)
        else {
            panic!("first cell must stage");
        };

        assert!(matches!(
            residency.stage_refresh(second, sparse(BlockId(10)), Vec3::X * 64.0, 2),
            FarStageOutcome::Fallback { kept: None }
        ));
        let batch = residency.take_upload_batch().unwrap();
        assert_eq!(batch.id, batch_id);
        residency.resolve_upload(FarUploadResult::ack(batch_id));
        assert_eq!(residency.resident(first), Some(&resident));
        assert!(residency.resident(second).is_none());
    }

    #[test]
    fn refresh_ack_swaps_root_but_nack_preserves_committed_resident() {
        let entity = Entity::from_bits(41);
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());
        let FarStageOutcome::Pending { batch_id, .. } =
            residency.stage_refresh(entity, sparse(BlockId(2)), Vec3::ZERO, 2)
        else {
            panic!("first load must stage");
        };
        residency.take_upload_batch().unwrap();
        residency.resolve_upload(FarUploadResult::ack(batch_id));
        let old = *residency.resident(entity).unwrap();
        let old_cell = *residency.cell_metadata(entity).unwrap();

        let FarStageOutcome::Pending { batch_id, .. } =
            residency.stage_refresh(entity, sparse(BlockId(3)), Vec3::ZERO, 2)
        else {
            panic!("refresh must stage");
        };
        assert_eq!(residency.resident(entity), Some(&old));
        assert_eq!(residency.cell_metadata(entity), Some(&old_cell));
        residency.take_upload_batch().unwrap();
        residency.resolve_upload(FarUploadResult::nack(batch_id));
        assert_eq!(residency.resident(entity), Some(&old));
        assert_eq!(residency.cell_metadata(entity), Some(&old_cell));

        let FarStageOutcome::Pending {
            batch_id,
            resident: candidate,
        } = residency.stage_refresh(entity, sparse(BlockId(3)), Vec3::ZERO, 2)
        else {
            panic!("retried refresh must stage");
        };
        residency.take_upload_batch().unwrap();
        residency.resolve_upload(FarUploadResult::ack(batch_id));
        assert_eq!(residency.resident(entity), Some(&candidate));
        assert_ne!(candidate.root, old.root);
    }

    #[test]
    fn neighbor_metadata_update_uses_acknowledged_batch_transport() {
        let entity = Entity::from_bits(42);
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());
        let FarStageOutcome::Pending { batch_id, .. } =
            residency.stage_refresh(entity, sparse(BlockId(2)), Vec3::ZERO, 2)
        else {
            panic!("first load must stage");
        };
        residency.take_upload_batch().unwrap();
        residency.resolve_upload(FarUploadResult::ack(batch_id));
        let old = *residency.cell_metadata(entity).unwrap();
        let mut updated = old;
        updated.set_neighbor(NeighborOffset::new(1, 0, 0).unwrap(), 77);

        let FarMaintenanceOutcome::Pending { batch_id } =
            residency.stage_cell_update(entity, updated)
        else {
            panic!("neighbor update must stage");
        };
        let batch = residency.take_upload_batch().unwrap();
        assert!(matches!(
            batch.uploads.as_slice(),
            [FarUpload::WriteCell { .. }]
        ));
        assert_eq!(residency.cell_metadata(entity), Some(&old));
        residency.resolve_upload(FarUploadResult::nack(batch_id));
        assert_eq!(residency.cell_metadata(entity), Some(&old));

        let FarMaintenanceOutcome::Pending { batch_id } =
            residency.stage_cell_update(entity, updated)
        else {
            panic!("retried neighbor update must stage");
        };
        residency.take_upload_batch().unwrap();
        residency.resolve_upload(FarUploadResult::ack(batch_id));
        assert_eq!(residency.cell_metadata(entity), Some(&updated));
    }

    #[test]
    fn neighbor_metadata_batch_is_atomic_and_keeps_latest_distinct_cell_updates() {
        let first = Entity::from_bits(58);
        let second = Entity::from_bits(59);
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());
        let FarStageOutcome::Pending { batch_id, .. } =
            residency.stage_refresh(first, sparse(BlockId(11)), Vec3::ZERO, 2)
        else {
            panic!("first cell must stage");
        };
        assert!(matches!(
            residency.stage_refresh(second, sparse(BlockId(12)), Vec3::X * 64.0, 2),
            FarStageOutcome::Pending {
                batch_id: joined,
                ..
            } if joined == batch_id
        ));
        residency.take_ready_upload_batch(2, false).unwrap();
        residency.resolve_upload(FarUploadResult::ack(batch_id));

        let first_old = *residency.cell_metadata(first).unwrap();
        let second_old = *residency.cell_metadata(second).unwrap();
        let mut first_update = first_old;
        first_update.set_neighbor(NeighborOffset::new(1, 0, 0).unwrap(), 70);
        let mut first_latest = first_update;
        first_latest.set_neighbor(NeighborOffset::new(1, 0, 0).unwrap(), 71);
        let mut second_update = second_old;
        second_update.set_neighbor(NeighborOffset::new(-1, 0, 0).unwrap(), 72);

        let FarMaintenanceOutcome::Pending { batch_id } =
            residency.stage_cell_update(first, first_update)
        else {
            panic!("first metadata update must stage");
        };
        assert_eq!(
            residency.stage_cell_update(second, second_update),
            FarMaintenanceOutcome::Pending { batch_id }
        );
        assert_eq!(
            residency.stage_cell_update(first, first_latest),
            FarMaintenanceOutcome::Pending { batch_id }
        );
        let batch = residency.take_upload_batch().unwrap();
        let cell_writes = batch
            .uploads
            .iter()
            .filter_map(|upload| match upload {
                FarUpload::WriteCell { slot, value } => Some((*slot, *value)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(cell_writes.len(), 2);
        assert!(cell_writes.contains(&(residency.resident(first).unwrap().cell_slot, first_latest)));
        assert!(
            cell_writes.contains(&(residency.resident(second).unwrap().cell_slot, second_update))
        );
        assert_eq!(residency.cell_metadata(first), Some(&first_old));
        assert_eq!(residency.cell_metadata(second), Some(&second_old));

        let rejected = residency
            .resolve_upload(FarUploadResult::nack(batch_id))
            .unwrap();
        assert_eq!(rejected.len(), 2);
        assert_eq!(residency.cell_metadata(first), Some(&first_old));
        assert_eq!(residency.cell_metadata(second), Some(&second_old));

        let FarMaintenanceOutcome::Pending { batch_id } =
            residency.stage_cell_update(first, first_latest)
        else {
            panic!("first metadata retry must stage");
        };
        assert_eq!(
            residency.stage_cell_update(second, second_update),
            FarMaintenanceOutcome::Pending { batch_id }
        );
        residency.take_upload_batch().unwrap();
        let updated = residency
            .resolve_upload(FarUploadResult::ack(batch_id))
            .unwrap();
        assert_eq!(updated.len(), 2);
        assert_eq!(residency.cell_metadata(first), Some(&first_latest));
        assert_eq!(residency.cell_metadata(second), Some(&second_update));
    }

    #[test]
    fn neighbor_metadata_batch_is_bounded_and_retries_the_tail() {
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());
        let entities = (0..=MAX_CELL_UPDATES_PER_BATCH)
            .map(|index| Entity::from_bits(1_000 + index as u64))
            .collect::<Vec<_>>();
        let mut first_load_batch = None;
        for (index, entity) in entities.iter().copied().enumerate() {
            let FarStageOutcome::Pending { batch_id, .. } = residency.stage_refresh(
                entity,
                sparse(BlockId(13)),
                Vec3::X * (index * 64) as f32,
                2,
            ) else {
                panic!("first load {index} must stage");
            };
            assert_eq!(*first_load_batch.get_or_insert(batch_id), batch_id);
        }
        let first_load_batch = first_load_batch.unwrap();
        residency
            .take_ready_upload_batch(entities.len(), false)
            .unwrap();
        residency.resolve_upload(FarUploadResult::ack(first_load_batch));

        let mut cell_batch = None;
        for entity in entities.iter().copied().take(MAX_CELL_UPDATES_PER_BATCH) {
            let mut cell = *residency.cell_metadata(entity).unwrap();
            cell.set_neighbor(NeighborOffset::new(1, 0, 0).unwrap(), 99);
            let FarMaintenanceOutcome::Pending { batch_id } =
                residency.stage_cell_update(entity, cell)
            else {
                panic!("bounded metadata update must stage");
            };
            assert_eq!(*cell_batch.get_or_insert(batch_id), batch_id);
        }
        let tail = *entities.last().unwrap();
        let mut tail_cell = *residency.cell_metadata(tail).unwrap();
        tail_cell.set_neighbor(NeighborOffset::new(1, 0, 0).unwrap(), 100);
        assert_eq!(
            residency.stage_cell_update(tail, tail_cell),
            FarMaintenanceOutcome::Busy
        );
        let cell_batch = cell_batch.unwrap();
        assert_eq!(residency.pending_work_count(), MAX_CELL_UPDATES_PER_BATCH);
        residency.take_upload_batch().unwrap();
        residency.resolve_upload(FarUploadResult::ack(cell_batch));

        assert!(matches!(
            residency.stage_cell_update(tail, tail_cell),
            FarMaintenanceOutcome::Pending { .. }
        ));
    }

    #[test]
    fn removal_clear_batch_preserves_committed_cell_on_nack_and_releases_on_ack() {
        let entity = Entity::from_bits(43);
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());
        let baseline = residency.reserved_bytes();
        let FarStageOutcome::Pending { batch_id, .. } =
            residency.stage_refresh(entity, sparse(BlockId(3)), Vec3::ZERO, 2)
        else {
            panic!("first load must stage");
        };
        residency.take_upload_batch().unwrap();
        residency.resolve_upload(FarUploadResult::ack(batch_id));

        let FarMaintenanceOutcome::Pending { batch_id } = residency.stage_remove(entity) else {
            panic!("removal must stage");
        };
        let batch = residency.take_upload_batch().unwrap();
        assert!(matches!(batch.uploads[0], FarUpload::ClearCell { .. }));
        assert!(residency.resident(entity).is_some());
        residency.resolve_upload(FarUploadResult::nack(batch_id));
        assert!(residency.resident(entity).is_some());

        let FarMaintenanceOutcome::Pending { batch_id } = residency.stage_remove(entity) else {
            panic!("retried removal must stage");
        };
        residency.take_upload_batch().unwrap();
        residency.resolve_upload(FarUploadResult::ack(batch_id));
        assert!(residency.resident(entity).is_none());
        assert_eq!(residency.reserved_bytes(), baseline);
    }
}
