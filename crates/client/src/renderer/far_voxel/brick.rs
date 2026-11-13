use std::collections::{BTreeMap, BTreeSet, HashMap};

use game::{BlockId, Voxel, BRICK_VOXEL_COUNT};

pub const ROOT_ENTRY_COUNT: usize = 64;
pub const BRICK_EDGE: usize = 8;
pub const BRICKS_PER_AXIS: usize = 4;
pub const MIXED_BRICK_BYTES: usize = 16 * 4 + 256 * 4;
pub const ROOT_BYTES: usize = ROOT_ENTRY_COUNT * 4;
pub const FAR_STORAGE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
/// One shared empty-binding allocation, sized for the largest runtime-array
/// element that may use it (`MixedBrick`).
pub const FAR_STATIC_BINDING_OVERHEAD_BYTES: usize = MIXED_BRICK_BYTES;
pub const BRICK_PAGE_BYTES: usize = 128 * 1024 * 1024;

/// Returns the geometric GPU arena capacity needed to address `required`
/// slots without exceeding the binding's slot limit.
pub fn geometric_slot_capacity(required: usize, max_slots: usize) -> usize {
    if required == 0 || max_slots == 0 {
        return 0;
    }
    required
        .checked_next_power_of_two()
        .unwrap_or(max_slots)
        .min(max_slots)
}

const ROOT_TAG_SHIFT: u32 = 30;
const ROOT_TAG_MASK: u32 = 0b11 << ROOT_TAG_SHIFT;
const ROOT_TAG_UNIFORM: u32 = 0b01 << ROOT_TAG_SHIFT;
const ROOT_TAG_PAGE_0: u32 = 0b10 << ROOT_TAG_SHIFT;
const ROOT_TAG_PAGE_1: u32 = 0b11 << ROOT_TAG_SHIFT;
const ROOT_PAYLOAD_MASK: u32 = (1 << ROOT_TAG_SHIFT) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CellId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootHandle(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(transparent)]
pub struct RootEntry(u32);

impl RootEntry {
    pub const EMPTY: Self = Self(0);

    pub const fn as_raw(self) -> u32 {
        self.0
    }

    pub fn uniform(material: BlockId) -> Self {
        debug_assert_ne!(material, BlockId::AIR);
        Self(ROOT_TAG_UNIFORM | u32::from(material.0))
    }

    pub fn mixed(page: u8, slot: u32) -> Self {
        assert!(
            slot <= ROOT_PAYLOAD_MASK,
            "mixed brick slot exceeds tagged handle payload"
        );
        match page {
            0 => Self(ROOT_TAG_PAGE_0 | slot),
            1 => Self(ROOT_TAG_PAGE_1 | slot),
            _ => panic!("mixed brick page must be 0 or 1"),
        }
    }

    pub fn kind(self) -> RootEntryKind {
        match self.0 & ROOT_TAG_MASK {
            0 => RootEntryKind::Empty,
            ROOT_TAG_UNIFORM => RootEntryKind::Uniform(BlockId(self.0 as u16)),
            ROOT_TAG_PAGE_0 => RootEntryKind::Mixed {
                page: 0,
                slot: self.0 & ROOT_PAYLOAD_MASK,
            },
            ROOT_TAG_PAGE_1 => RootEntryKind::Mixed {
                page: 1,
                slot: self.0 & ROOT_PAYLOAD_MASK,
            },
            _ => unreachable!(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootEntryKind {
    Empty,
    Uniform(BlockId),
    Mixed { page: u8, slot: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct MixedBrick {
    pub occupancy: [u32; 16],
    pub materials: [u32; 256],
}

impl Default for MixedBrick {
    fn default() -> Self {
        Self {
            occupancy: [0; 16],
            materials: [0; 256],
        }
    }
}

impl MixedBrick {
    fn set(&mut self, local: usize, material: BlockId) {
        if material != BlockId::AIR {
            self.occupancy[local / 32] |= 1 << (local % 32);
        }
        let shift = (local % 2) * 16;
        self.materials[local / 2] |= u32::from(material.0) << shift;
    }

    pub fn material(&self, local: usize) -> BlockId {
        if self.occupancy[local / 32] & (1 << (local % 32)) == 0 {
            return BlockId::AIR;
        }
        BlockId((self.materials[local / 2] >> ((local % 2) * 16)) as u16)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct BrickRoot {
    pub entries: [RootEntry; ROOT_ENTRY_COUNT],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparedBrick {
    Empty,
    Uniform(BlockId),
    Mixed(Box<MixedBrick>),
}

/// CPU result of the asynchronous per-cell encoding stage. Handles remain
/// unresolved until the main-world residency owner interns this immutable
/// content into its deterministic shared arenas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedBrickRoot {
    pub entries: [PreparedBrick; ROOT_ENTRY_COUNT],
}

impl PreparedBrickRoot {
    pub fn from_voxels(voxels: &[Voxel]) -> Result<Self, FallbackReason> {
        if voxels.len() != BRICK_VOXEL_COUNT {
            return Err(FallbackReason::InvalidVoxelCount {
                expected: BRICK_VOXEL_COUNT,
                actual: voxels.len(),
            });
        }
        Ok(Self {
            entries: std::array::from_fn(|index| {
                let bx = index % BRICKS_PER_AXIS;
                let by = index / BRICKS_PER_AXIS % BRICKS_PER_AXIS;
                let bz = index / (BRICKS_PER_AXIS * BRICKS_PER_AXIS);
                let first =
                    voxels[voxel_index(bx * BRICK_EDGE, by * BRICK_EDGE, bz * BRICK_EDGE)].block;
                let uniform = (0..BRICK_EDGE).all(|lz| {
                    (0..BRICK_EDGE).all(|ly| {
                        (0..BRICK_EDGE).all(|lx| {
                            voxels[voxel_index(
                                bx * BRICK_EDGE + lx,
                                by * BRICK_EDGE + ly,
                                bz * BRICK_EDGE + lz,
                            )]
                            .block
                                == first
                        })
                    })
                });
                if uniform {
                    if first == BlockId::AIR {
                        PreparedBrick::Empty
                    } else {
                        PreparedBrick::Uniform(first)
                    }
                } else {
                    PreparedBrick::Mixed(Box::new(encode_mixed_brick(voxels, bx, by, bz)))
                }
            }),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    Inserted(RootHandle),
    Unchanged(RootHandle),
    Replaced {
        old: RootHandle,
        new: RootHandle,
    },
    Fallback {
        kept: Option<RootHandle>,
        reason: FallbackReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    BudgetExceeded,
    InvalidVoxelCount { expected: usize, actual: usize },
}

#[derive(Debug)]
pub struct BrickDagStage {
    cell: CellId,
    previous: Option<RootHandle>,
    candidate: RootHandle,
}

impl BrickDagStage {
    pub fn previous(&self) -> Option<RootHandle> {
        self.previous
    }

    pub fn candidate(&self) -> RootHandle {
        self.candidate
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommittedBrickDagStage {
    pub old: Option<RootHandle>,
    pub new: RootHandle,
}

impl RefreshOutcome {
    pub fn root(self) -> Option<RootHandle> {
        match self {
            Self::Inserted(root) | Self::Unchanged(root) => Some(root),
            Self::Replaced { new, .. } => Some(new),
            Self::Fallback { kept, .. } => kept,
        }
    }
}

pub struct BrickDag {
    cells: BTreeMap<CellId, RootHandle>,
    roots: Vec<Option<RefSlot<BrickRoot>>>,
    mixed_pages: [Vec<Option<RefSlot<MixedBrick>>>; 2],
    root_intern: HashMap<u64, Vec<RootHandle>>,
    mixed_intern: HashMap<u64, Vec<RootEntry>>,
    free_roots: BTreeSet<usize>,
    free_mixed: [BTreeSet<usize>; 2],
    limits: DagLimits,
    external_reserved_bytes: usize,
    root_hash: fn(&BrickRoot) -> u64,
    mixed_hash: fn(&MixedBrick) -> u64,
}

struct RefSlot<T> {
    value: T,
    refs: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DagLimits {
    pub max_reserved_bytes: usize,
    pub max_mixed_bricks_per_page: usize,
    pub max_roots: usize,
}

impl Default for DagLimits {
    fn default() -> Self {
        Self {
            max_reserved_bytes: FAR_STORAGE_BUDGET_BYTES,
            max_mixed_bricks_per_page: BRICK_PAGE_BYTES / MIXED_BRICK_BYTES,
            max_roots: BRICK_PAGE_BYTES / ROOT_BYTES,
        }
    }
}

impl Default for BrickDag {
    fn default() -> Self {
        Self::with_limits(DagLimits::default())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BrickDagStats {
    pub cells: usize,
    pub unique_roots: usize,
    pub unique_mixed_bricks: usize,
    pub reserved_bytes: usize,
}

impl BrickDag {
    pub fn with_limits(limits: DagLimits) -> Self {
        Self {
            cells: BTreeMap::new(),
            roots: Vec::new(),
            mixed_pages: [Vec::new(), Vec::new()],
            root_intern: HashMap::new(),
            mixed_intern: HashMap::new(),
            free_roots: BTreeSet::new(),
            free_mixed: [BTreeSet::new(), BTreeSet::new()],
            limits,
            external_reserved_bytes: 0,
            root_hash: hash_root,
            mixed_hash: hash_mixed,
        }
    }

    #[cfg(test)]
    pub fn refresh_cell(&mut self, cell: CellId, voxels: &[Voxel]) -> RefreshOutcome {
        let previous = self.cells.get(&cell).copied();
        let prepared = match PreparedBrickRoot::from_voxels(voxels) {
            Ok(prepared) => prepared,
            Err(reason) => {
                return RefreshOutcome::Fallback {
                    kept: previous,
                    reason,
                };
            }
        };
        self.refresh_prepared(cell, prepared)
    }

    #[cfg(test)]
    pub fn refresh_prepared(
        &mut self,
        cell: CellId,
        prepared: PreparedBrickRoot,
    ) -> RefreshOutcome {
        let previous = self.cells.get(&cell).copied();
        let stage = match self.stage_prepared(cell, prepared) {
            Ok(stage) => stage,
            Err(reason) => {
                return RefreshOutcome::Fallback {
                    kept: previous,
                    reason,
                };
            }
        };
        let candidate = stage.candidate;
        self.commit_stage(stage);
        match previous {
            None => RefreshOutcome::Inserted(candidate),
            Some(old) if old == candidate => RefreshOutcome::Unchanged(candidate),
            Some(old) => RefreshOutcome::Replaced {
                old,
                new: candidate,
            },
        }
    }

    pub fn stage_prepared(
        &mut self,
        cell: CellId,
        prepared: PreparedBrickRoot,
    ) -> Result<BrickDagStage, FallbackReason> {
        let previous = self.cells.get(&cell).copied();
        let page_lengths = [self.mixed_pages[0].len(), self.mixed_pages[1].len()];
        let mut acquired_mixed = Vec::new();
        let mut entries = [RootEntry::EMPTY; ROOT_ENTRY_COUNT];
        for (index, prepared_brick) in prepared.entries.into_iter().enumerate() {
            entries[index] = match prepared_brick {
                PreparedBrick::Empty => RootEntry::EMPTY,
                PreparedBrick::Uniform(material) => RootEntry::uniform(material),
                PreparedBrick::Mixed(brick) => match self.intern_mixed(*brick) {
                    Ok(entry) => {
                        acquired_mixed.push(entry);
                        entry
                    }
                    Err(()) => {
                        self.rollback_mixed_acquires(acquired_mixed, page_lengths);
                        return Err(FallbackReason::BudgetExceeded);
                    }
                },
            };
        }
        let root_value = BrickRoot { entries };
        let Ok((root, inserted)) = self.intern_root(root_value) else {
            self.rollback_mixed_acquires(acquired_mixed, page_lengths);
            return Err(FallbackReason::BudgetExceeded);
        };
        if !inserted {
            for entry in acquired_mixed {
                self.undo_mixed_acquire(entry);
            }
        }
        Ok(BrickDagStage {
            cell,
            previous,
            candidate: root,
        })
    }

    pub fn commit_stage(&mut self, stage: BrickDagStage) -> CommittedBrickDagStage {
        assert_eq!(
            self.cells.get(&stage.cell).copied(),
            stage.previous,
            "committed cell changed while its root was staged"
        );
        match stage.previous {
            None => {
                self.cells.insert(stage.cell, stage.candidate);
            }
            Some(old) if old == stage.candidate => {
                self.release_root(stage.candidate);
            }
            Some(old) => {
                self.cells.insert(stage.cell, stage.candidate);
                self.release_root(old);
            }
        }
        CommittedBrickDagStage {
            old: stage.previous,
            new: stage.candidate,
        }
    }

    pub fn rollback_stage(&mut self, stage: BrickDagStage) {
        assert_eq!(
            self.cells.get(&stage.cell).copied(),
            stage.previous,
            "committed cell changed while its root was staged"
        );
        self.release_root(stage.candidate);
    }

    pub fn set_external_reserved_bytes(&mut self, bytes: usize) {
        self.external_reserved_bytes = bytes;
    }

    pub fn root(&self, handle: RootHandle) -> Option<&BrickRoot> {
        self.roots
            .get(handle.0 as usize)?
            .as_ref()
            .map(|slot| &slot.value)
    }

    pub fn reconstruct(&self, handle: RootHandle) -> Option<Vec<Voxel>> {
        let root = self.root(handle)?;
        let mut voxels = vec![Voxel::new(BlockId::AIR); BRICK_VOXEL_COUNT];
        for bz in 0..BRICKS_PER_AXIS {
            for by in 0..BRICKS_PER_AXIS {
                for bx in 0..BRICKS_PER_AXIS {
                    let entry = root.entries[root_index(bx, by, bz)];
                    for lz in 0..BRICK_EDGE {
                        for ly in 0..BRICK_EDGE {
                            for lx in 0..BRICK_EDGE {
                                let material = match entry.kind() {
                                    RootEntryKind::Empty => BlockId::AIR,
                                    RootEntryKind::Uniform(material) => material,
                                    RootEntryKind::Mixed { .. } => {
                                        let local =
                                            lx + BRICK_EDGE * ly + BRICK_EDGE * BRICK_EDGE * lz;
                                        self.mixed_brick(entry)?.material(local)
                                    }
                                };
                                voxels[voxel_index(
                                    bx * BRICK_EDGE + lx,
                                    by * BRICK_EDGE + ly,
                                    bz * BRICK_EDGE + lz,
                                )] = Voxel::new(material);
                            }
                        }
                    }
                }
            }
        }
        Some(voxels)
    }

    pub fn stats(&self) -> BrickDagStats {
        BrickDagStats {
            cells: self.cells.len(),
            unique_roots: self.roots.iter().filter(|slot| slot.is_some()).count(),
            unique_mixed_bricks: self
                .mixed_pages
                .iter()
                .flat_map(|page| page.iter())
                .filter(|slot| slot.is_some())
                .count(),
            reserved_bytes: self.reserved_bytes(),
        }
    }

    pub fn mixed_brick(&self, entry: RootEntry) -> Option<&MixedBrick> {
        let RootEntryKind::Mixed { page, slot } = entry.kind() else {
            return None;
        };
        self.mixed_pages[page as usize]
            .get(slot as usize)?
            .as_ref()
            .map(|slot| &slot.value)
    }

    pub fn root_ref_count(&self, handle: RootHandle) -> Option<u32> {
        Some(self.roots.get(handle.0 as usize)?.as_ref()?.refs)
    }

    pub fn mixed_ref_count(&self, entry: RootEntry) -> Option<u32> {
        let RootEntryKind::Mixed { page, slot } = entry.kind() else {
            return None;
        };
        Some(
            self.mixed_pages[page as usize]
                .get(slot as usize)?
                .as_ref()?
                .refs,
        )
    }

    pub fn cell_root(&self, cell: CellId) -> Option<RootHandle> {
        self.cells.get(&cell).copied()
    }

    pub fn cell_mixed_reference_count(&self) -> usize {
        self.cells
            .values()
            .filter_map(|handle| self.root(*handle))
            .map(|root| {
                root.entries
                    .iter()
                    .filter(|entry| matches!(entry.kind(), RootEntryKind::Mixed { .. }))
                    .count()
            })
            .sum()
    }

    pub fn remove_cell(&mut self, cell: CellId) -> Option<RootHandle> {
        let root = self.cells.remove(&cell)?;
        self.release_root(root);
        Some(root)
    }

    fn intern_mixed(&mut self, brick: MixedBrick) -> Result<RootEntry, ()> {
        let hash = (self.mixed_hash)(&brick);
        if let Some(entry) = self.mixed_intern.get(&hash).and_then(|bucket| {
            bucket
                .iter()
                .copied()
                .find(|entry| self.mixed_brick(*entry) == Some(&brick))
        }) {
            let RootEntryKind::Mixed { page, slot } = entry.kind() else {
                unreachable!()
            };
            self.mixed_pages[page as usize][slot as usize]
                .as_mut()
                .unwrap()
                .refs += 1;
            return Ok(entry);
        }

        let (page, slot) = self.allocate_mixed_slot().ok_or(())?;
        let entry = RootEntry::mixed(page as u8, slot);
        let value = Some(RefSlot {
            value: brick,
            refs: 1,
        });
        if slot as usize == self.mixed_pages[page].len() {
            self.mixed_pages[page].push(value);
        } else {
            self.mixed_pages[page][slot as usize] = value;
        }
        self.mixed_intern.entry(hash).or_default().push(entry);
        Ok(entry)
    }

    fn intern_root(&mut self, root: BrickRoot) -> Result<(RootHandle, bool), ()> {
        let hash = (self.root_hash)(&root);
        if let Some(handle) = self.root_intern.get(&hash).and_then(|bucket| {
            bucket
                .iter()
                .copied()
                .find(|handle| self.root(*handle) == Some(&root))
        }) {
            self.roots[handle.0 as usize].as_mut().unwrap().refs += 1;
            return Ok((handle, false));
        }
        let index = if let Some(index) = self.free_roots.pop_first() {
            index
        } else {
            let prospective_reserved = self.reserved_bytes_for(
                self.roots.len() + 1,
                [self.mixed_pages[0].len(), self.mixed_pages[1].len()],
            );
            if self.roots.len() >= self.limits.max_roots
                || prospective_reserved.saturating_add(self.external_reserved_bytes)
                    > self.limits.max_reserved_bytes
            {
                return Err(());
            }
            self.roots.len()
        };
        let handle = RootHandle(index as u32);
        let value = Some(RefSlot {
            value: root,
            refs: 1,
        });
        if index == self.roots.len() {
            self.roots.push(value);
        } else {
            self.roots[index] = value;
        }
        self.root_intern.entry(hash).or_default().push(handle);
        Ok((handle, true))
    }

    fn undo_mixed_acquire(&mut self, entry: RootEntry) {
        let RootEntryKind::Mixed { page, slot } = entry.kind() else {
            return;
        };
        let refs = &mut self.mixed_pages[page as usize][slot as usize]
            .as_mut()
            .unwrap()
            .refs;
        assert!(
            *refs > 1,
            "new root is the only owner of a newly interned brick"
        );
        *refs -= 1;
    }

    fn allocate_mixed_slot(&mut self) -> Option<(usize, u32)> {
        for page in 0..2 {
            if let Some(slot) = self.free_mixed[page].pop_first() {
                return Some((page, slot as u32));
            }
            let mut prospective_lengths = [self.mixed_pages[0].len(), self.mixed_pages[1].len()];
            prospective_lengths[page] += 1;
            let prospective_reserved =
                self.reserved_bytes_for(self.roots.len(), prospective_lengths);
            if self.mixed_pages[page].len() < self.limits.max_mixed_bricks_per_page
                && prospective_reserved.saturating_add(self.external_reserved_bytes)
                    <= self.limits.max_reserved_bytes
            {
                return Some((page, self.mixed_pages[page].len() as u32));
            }
        }
        None
    }

    fn reserved_bytes(&self) -> usize {
        self.reserved_bytes_for(
            self.roots.len(),
            [self.mixed_pages[0].len(), self.mixed_pages[1].len()],
        )
    }

    fn reserved_bytes_for(&self, roots: usize, mixed_pages: [usize; 2]) -> usize {
        geometric_slot_capacity(roots, self.limits.max_roots) * ROOT_BYTES
            + mixed_pages
                .into_iter()
                .map(|slots| {
                    geometric_slot_capacity(slots, self.limits.max_mixed_bricks_per_page)
                        * MIXED_BRICK_BYTES
                })
                .sum::<usize>()
    }

    fn rollback_mixed_acquires(&mut self, acquired: Vec<RootEntry>, page_lengths: [usize; 2]) {
        for entry in acquired.into_iter().rev() {
            self.release_mixed(entry);
        }
        for (page, length) in page_lengths.into_iter().enumerate() {
            debug_assert!(self.mixed_pages[page][length..].iter().all(Option::is_none));
            self.mixed_pages[page].truncate(length);
            self.free_mixed[page].retain(|slot| *slot < length);
        }
    }

    fn release_root(&mut self, handle: RootHandle) {
        let index = handle.0 as usize;
        let slot = self.roots[index].as_mut().expect("live root handle");
        if slot.refs > 1 {
            slot.refs -= 1;
            return;
        }
        let root = self.roots[index].take().unwrap().value;
        let hash = (self.root_hash)(&root);
        let remove_bucket = if let Some(bucket) = self.root_intern.get_mut(&hash) {
            bucket.retain(|candidate| *candidate != handle);
            bucket.is_empty()
        } else {
            false
        };
        if remove_bucket {
            self.root_intern.remove(&hash);
        }
        self.free_roots.insert(index);
        for entry in root.entries {
            if matches!(entry.kind(), RootEntryKind::Mixed { .. }) {
                self.release_mixed(entry);
            }
        }
        self.trim_free_roots();
    }

    fn release_mixed(&mut self, entry: RootEntry) {
        let RootEntryKind::Mixed { page, slot } = entry.kind() else {
            return;
        };
        let page = page as usize;
        let index = slot as usize;
        let value = self.mixed_pages[page][index]
            .as_mut()
            .expect("live mixed handle");
        if value.refs > 1 {
            value.refs -= 1;
            return;
        }
        let brick = self.mixed_pages[page][index].take().unwrap().value;
        let hash = (self.mixed_hash)(&brick);
        let remove_bucket = if let Some(bucket) = self.mixed_intern.get_mut(&hash) {
            bucket.retain(|candidate| *candidate != entry);
            bucket.is_empty()
        } else {
            false
        };
        if remove_bucket {
            self.mixed_intern.remove(&hash);
        }
        self.free_mixed[page].insert(index);
        self.trim_free_mixed_page(page);
    }

    fn trim_free_roots(&mut self) {
        while self.roots.last().is_some_and(Option::is_none) {
            let index = self.roots.len() - 1;
            self.roots.pop();
            self.free_roots.remove(&index);
        }
    }

    fn trim_free_mixed_page(&mut self, page: usize) {
        while self.mixed_pages[page].last().is_some_and(Option::is_none) {
            let index = self.mixed_pages[page].len() - 1;
            self.mixed_pages[page].pop();
            self.free_mixed[page].remove(&index);
        }
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn hash_words(words: impl IntoIterator<Item = u32>) -> u64 {
    let mut hash = FNV_OFFSET;
    for word in words {
        for byte in word.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

fn hash_mixed(brick: &MixedBrick) -> u64 {
    hash_words(brick.occupancy.into_iter().chain(brick.materials))
}

fn hash_root(root: &BrickRoot) -> u64 {
    hash_words(root.entries.iter().map(|entry| entry.as_raw()))
}

fn encode_mixed_brick(voxels: &[Voxel], bx: usize, by: usize, bz: usize) -> MixedBrick {
    let mut brick = MixedBrick::default();
    for lz in 0..BRICK_EDGE {
        for ly in 0..BRICK_EDGE {
            for lx in 0..BRICK_EDGE {
                let local = lx + BRICK_EDGE * ly + BRICK_EDGE * BRICK_EDGE * lz;
                let material = voxels[voxel_index(
                    bx * BRICK_EDGE + lx,
                    by * BRICK_EDGE + ly,
                    bz * BRICK_EDGE + lz,
                )]
                .block;
                brick.set(local, material);
            }
        }
    }
    brick
}

fn root_index(bx: usize, by: usize, bz: usize) -> usize {
    bx + BRICKS_PER_AXIS * by + BRICKS_PER_AXIS * BRICKS_PER_AXIS * bz
}

fn voxel_index(x: usize, y: usize, z: usize) -> usize {
    x + 32 * y + 32 * 32 * z
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sparse_grid(material: BlockId) -> Vec<Voxel> {
        let mut grid = vec![Voxel::new(BlockId::AIR); BRICK_VOXEL_COUNT];
        grid[voxel_index(0, 0, 0)] = Voxel::new(material);
        grid
    }

    fn fill_brick(grid: &mut [Voxel], bx: usize, by: usize, bz: usize, material: BlockId) {
        for lz in 0..BRICK_EDGE {
            for ly in 0..BRICK_EDGE {
                for lx in 0..BRICK_EDGE {
                    grid[voxel_index(
                        bx * BRICK_EDGE + lx,
                        by * BRICK_EDGE + ly,
                        bz * BRICK_EDGE + lz,
                    )] = Voxel::new(material);
                }
            }
        }
    }

    #[test]
    fn prepared_grid_can_be_interned_without_reencoding_voxels() {
        let grid = sparse_grid(BlockId(31));
        let prepared = PreparedBrickRoot::from_voxels(&grid).expect("valid 32 cubed grid");
        let mut dag = BrickDag::default();

        let root = dag
            .refresh_prepared(CellId(6), prepared)
            .root()
            .expect("prepared cell is resident");

        assert_eq!(dag.reconstruct(root), Some(grid));
    }

    #[test]
    fn gpu_layouts_are_plain_words_without_implicit_padding() {
        fn assert_pod<T: bytemuck::Pod + bytemuck::Zeroable>() {}

        assert_pod::<RootEntry>();
        assert_pod::<MixedBrick>();
        assert_pod::<BrickRoot>();
        assert_eq!(std::mem::size_of::<RootEntry>(), 4);
        assert_eq!(std::mem::align_of::<RootEntry>(), 4);
        assert_eq!(std::mem::size_of::<MixedBrick>(), MIXED_BRICK_BYTES);
        assert_eq!(std::mem::align_of::<MixedBrick>(), 4);
        assert_eq!(std::mem::size_of::<BrickRoot>(), ROOT_BYTES);
        assert_eq!(std::mem::align_of::<BrickRoot>(), 4);
        assert_eq!(
            FAR_STATIC_BINDING_OVERHEAD_BYTES,
            std::mem::size_of::<MixedBrick>()
                .max(std::mem::size_of::<BrickRoot>())
                .max(std::mem::size_of::<super::super::residency::GpuFarCell>())
        );
    }

    #[test]
    fn empty_grid_reconstructs_exactly() {
        let grid = vec![Voxel::new(BlockId::AIR); BRICK_VOXEL_COUNT];
        let mut dag = BrickDag::default();

        let outcome = dag.refresh_cell(CellId(7), &grid);
        let root = outcome.root().expect("empty grid is resident");

        assert_eq!(dag.reconstruct(root), Some(grid));
        assert!(dag
            .root(root)
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.kind() == RootEntryKind::Empty));
    }

    #[test]
    fn uniform_grid_reconstructs_exactly_without_mixed_bricks() {
        let material = BlockId(42);
        let grid = vec![Voxel::new(material); BRICK_VOXEL_COUNT];
        let mut dag = BrickDag::default();

        let root = dag
            .refresh_cell(CellId(8), &grid)
            .root()
            .expect("uniform grid is resident");

        assert_eq!(dag.reconstruct(root), Some(grid));
        assert!(dag
            .root(root)
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.kind() == RootEntryKind::Uniform(material)));
        assert_eq!(dag.stats().unique_mixed_bricks, 0);
    }

    #[test]
    fn sparse_grid_uses_exact_mixed_layout_and_reconstructs() {
        let mut grid = vec![Voxel::new(BlockId::AIR); BRICK_VOXEL_COUNT];
        grid[voxel_index(3, 4, 5)] = Voxel::new(BlockId(17));
        let mut dag = BrickDag::default();

        let root = dag
            .refresh_cell(CellId(9), &grid)
            .root()
            .expect("sparse grid is resident");
        let entry = dag.root(root).unwrap().entries[0];
        let RootEntryKind::Mixed { page, slot } = entry.kind() else {
            panic!("sparse brick must be mixed")
        };
        assert_eq!((page, slot), (0, 0));

        let brick = dag.mixed_brick(entry).unwrap();
        let local = 3 + BRICK_EDGE * 4 + BRICK_EDGE * BRICK_EDGE * 5;
        assert_eq!(brick.occupancy[local / 32], 1 << (local % 32));
        assert_eq!(brick.materials[local / 2], u32::from(17_u16) << 16);
        assert_eq!(std::mem::size_of::<MixedBrick>(), 16 * 4 + 256 * 4);
        assert_eq!(dag.reconstruct(root), Some(grid));
    }

    #[test]
    fn mixed_material_grid_preserves_air_and_full_u16_ids() {
        let mut grid = vec![Voxel::new(BlockId::AIR); BRICK_VOXEL_COUNT];
        for z in 0..32 {
            for y in 0..32 {
                for x in 0..32 {
                    let material = match (x + y + z) % 3 {
                        0 => BlockId::AIR,
                        1 => BlockId(0x1234),
                        _ => BlockId(0xabcd),
                    };
                    grid[voxel_index(x, y, z)] = Voxel::new(material);
                }
            }
        }
        let mut dag = BrickDag::default();

        let root = dag
            .refresh_cell(CellId(10), &grid)
            .root()
            .expect("mixed grid is resident");
        let entry = dag.root(root).unwrap().entries[0];
        let brick = dag.mixed_brick(entry).unwrap();

        assert_eq!(entry.as_raw() >> ROOT_TAG_SHIFT, 0b10);
        assert_eq!(brick.occupancy[0] & 0b111, 0b110);
        assert_eq!(brick.materials[0], 0x1234_0000);
        assert_eq!(brick.materials[1] & 0xffff, 0xabcd);
        assert_eq!(dag.reconstruct(root), Some(grid));
    }

    #[test]
    fn mixed_handles_distinguish_page_zero_from_page_one() {
        let mut grid = vec![Voxel::new(BlockId::AIR); BRICK_VOXEL_COUNT];
        grid[voxel_index(0, 0, 0)] = Voxel::new(BlockId(1));
        grid[voxel_index(8, 0, 0)] = Voxel::new(BlockId(2));
        let limits = DagLimits {
            max_reserved_bytes: FAR_STORAGE_BUDGET_BYTES,
            max_mixed_bricks_per_page: 1,
            max_roots: DagLimits::default().max_roots,
        };
        let mut dag = BrickDag::with_limits(limits);

        let root = dag
            .refresh_cell(CellId(11), &grid)
            .root()
            .expect("two pages are available");
        assert_eq!(
            dag.root(root).unwrap().entries[0].kind(),
            RootEntryKind::Mixed { page: 0, slot: 0 }
        );
        assert_eq!(
            dag.root(root).unwrap().entries[1].kind(),
            RootEntryKind::Mixed { page: 1, slot: 0 }
        );
        assert_eq!(dag.reconstruct(root), Some(grid));
    }

    #[test]
    fn identical_cells_deduplicate_root_and_brick_refs() {
        let grid = sparse_grid(BlockId(5));
        let mut dag = BrickDag::default();

        let first = dag.refresh_cell(CellId(20), &grid).root().unwrap();
        let second = dag.refresh_cell(CellId(21), &grid).root().unwrap();
        let entry = dag.root(first).unwrap().entries[0];

        assert_eq!(first, second);
        assert_eq!(dag.stats().unique_roots, 1);
        assert_eq!(dag.stats().unique_mixed_bricks, 1);
        assert_eq!(dag.root_ref_count(first), Some(2));
        assert_eq!(dag.mixed_ref_count(entry), Some(1));
    }

    #[test]
    fn distinct_roots_deduplicate_their_shared_brick() {
        let mut first_grid = sparse_grid(BlockId(5));
        fill_brick(&mut first_grid, 1, 0, 0, BlockId(6));
        let mut second_grid = sparse_grid(BlockId(5));
        fill_brick(&mut second_grid, 1, 0, 0, BlockId(7));
        let mut dag = BrickDag::default();

        let first = dag.refresh_cell(CellId(22), &first_grid).root().unwrap();
        let second = dag.refresh_cell(CellId(23), &second_grid).root().unwrap();
        let first_entry = dag.root(first).unwrap().entries[0];
        let second_entry = dag.root(second).unwrap().entries[0];

        assert_ne!(first, second);
        assert_eq!(first_entry, second_entry);
        assert_eq!(dag.stats().unique_roots, 2);
        assert_eq!(dag.stats().unique_mixed_bricks, 1);
        assert_eq!(dag.mixed_ref_count(first_entry), Some(2));
    }

    #[test]
    fn forced_hash_collisions_still_compare_exact_content() {
        fn collide_mixed(_: &MixedBrick) -> u64 {
            0
        }
        fn collide_root(_: &BrickRoot) -> u64 {
            0
        }

        let first_grid = sparse_grid(BlockId(8));
        let second_grid = sparse_grid(BlockId(9));
        let mut dag = BrickDag {
            mixed_hash: collide_mixed,
            root_hash: collide_root,
            ..BrickDag::default()
        };

        let first = dag.refresh_cell(CellId(24), &first_grid).root().unwrap();
        let second = dag.refresh_cell(CellId(25), &second_grid).root().unwrap();

        assert_ne!(first, second);
        assert_eq!(dag.stats().unique_roots, 2);
        assert_eq!(dag.stats().unique_mixed_bricks, 2);
        assert_eq!(dag.reconstruct(first), Some(first_grid));
        assert_eq!(dag.reconstruct(second), Some(second_grid));
    }

    #[test]
    fn refresh_is_refcount_neutral_when_unchanged_and_replaces_when_changed() {
        let first_grid = sparse_grid(BlockId(10));
        let second_grid = sparse_grid(BlockId(11));
        let mut dag = BrickDag::default();

        let first = dag.refresh_cell(CellId(30), &first_grid).root().unwrap();
        assert_eq!(
            dag.refresh_cell(CellId(30), &first_grid),
            RefreshOutcome::Unchanged(first)
        );
        assert_eq!(dag.root_ref_count(first), Some(1));

        let RefreshOutcome::Replaced { old, new } = dag.refresh_cell(CellId(30), &second_grid)
        else {
            panic!("changed content replaces the cell root")
        };
        assert_eq!(old, first);
        assert_eq!(dag.cell_root(CellId(30)), Some(new));
        assert_eq!(dag.root(first), None);
        assert_eq!(dag.reconstruct(new), Some(second_grid));
        assert_eq!(dag.stats().unique_roots, 1);
        assert_eq!(dag.stats().unique_mixed_bricks, 1);
    }

    #[test]
    fn staged_root_keeps_committed_mapping_until_commit_and_rolls_back_by_ref_delta() {
        let mut dag = BrickDag::default();
        let cell = CellId(42);
        let old_grid = sparse_grid(BlockId(3));
        let new_grid = sparse_grid(BlockId(9));
        let old = dag.refresh_cell(cell, &old_grid).root().unwrap();
        let committed_cells = dag.stats().cells;

        let staged = dag
            .stage_prepared(cell, PreparedBrickRoot::from_voxels(&new_grid).unwrap())
            .unwrap();
        assert_eq!(dag.cell_root(cell), Some(old));
        assert_eq!(dag.stats().cells, committed_cells);
        dag.rollback_stage(staged);
        assert_eq!(dag.cell_root(cell), Some(old));
        assert_eq!(dag.reconstruct(old).unwrap(), old_grid);

        let staged = dag
            .stage_prepared(cell, PreparedBrickRoot::from_voxels(&new_grid).unwrap())
            .unwrap();
        let committed = dag.commit_stage(staged);
        assert_eq!(dag.cell_root(cell), Some(committed.new));
        assert_eq!(dag.reconstruct(committed.new).unwrap(), new_grid);
        assert!(dag.root(old).is_none());
    }

    #[test]
    fn removal_decrements_shared_refs_and_frees_the_last_owner() {
        let grid = sparse_grid(BlockId(12));
        let mut dag = BrickDag::default();
        let root = dag.refresh_cell(CellId(31), &grid).root().unwrap();
        dag.refresh_cell(CellId(32), &grid);
        let entry = dag.root(root).unwrap().entries[0];

        assert_eq!(dag.remove_cell(CellId(31)), Some(root));
        assert_eq!(dag.root_ref_count(root), Some(1));
        assert_eq!(dag.mixed_ref_count(entry), Some(1));
        assert_eq!(dag.remove_cell(CellId(32)), Some(root));
        assert_eq!(dag.root(root), None);
        assert_eq!(dag.mixed_brick(entry), None);
        assert_eq!(dag.stats().cells, 0);
        assert_eq!(dag.stats().unique_roots, 0);
        assert_eq!(dag.stats().unique_mixed_bricks, 0);
    }

    #[test]
    fn freed_root_and_brick_slots_reuse_the_lowest_index() {
        let mut dag = BrickDag::default();
        let first = dag
            .refresh_cell(CellId(40), &sparse_grid(BlockId(13)))
            .root()
            .unwrap();
        let middle = dag
            .refresh_cell(CellId(41), &sparse_grid(BlockId(14)))
            .root()
            .unwrap();
        let last = dag
            .refresh_cell(CellId(42), &sparse_grid(BlockId(15)))
            .root()
            .unwrap();
        let first_entry = dag.root(first).unwrap().entries[0];
        assert_eq!(first, RootHandle(0));
        assert_eq!(middle, RootHandle(1));
        assert_eq!(last, RootHandle(2));

        dag.remove_cell(CellId(42));
        dag.remove_cell(CellId(40));
        let reused = dag
            .refresh_cell(CellId(43), &sparse_grid(BlockId(16)))
            .root()
            .unwrap();

        assert_eq!(reused, RootHandle(0));
        assert_eq!(dag.root(reused).unwrap().entries[0], first_entry);
    }

    #[test]
    fn combined_budget_failure_is_transactional_and_keeps_the_old_cell() {
        assert_eq!(DagLimits::default().max_reserved_bytes, 256 * 1024 * 1024);
        let limits = DagLimits {
            max_reserved_bytes: ROOT_BYTES + MIXED_BRICK_BYTES,
            max_mixed_bricks_per_page: 2,
            max_roots: 1,
        };
        let first_grid = sparse_grid(BlockId(20));
        let replacement_grid = sparse_grid(BlockId(21));
        let mut dag = BrickDag::with_limits(limits);
        let first = dag.refresh_cell(CellId(50), &first_grid).root().unwrap();
        let before = dag.stats();
        assert_eq!(before.reserved_bytes, ROOT_BYTES + MIXED_BRICK_BYTES);

        assert_eq!(
            dag.refresh_cell(CellId(50), &replacement_grid),
            RefreshOutcome::Fallback {
                kept: Some(first),
                reason: FallbackReason::BudgetExceeded,
            }
        );

        assert_eq!(dag.cell_root(CellId(50)), Some(first));
        assert_eq!(dag.reconstruct(first), Some(first_grid));
        assert_eq!(dag.root_ref_count(first), Some(1));
        assert_eq!(dag.stats(), before);

        dag.remove_cell(CellId(50));
        let replacement = dag
            .refresh_cell(CellId(51), &replacement_grid)
            .root()
            .unwrap();
        assert_eq!(
            replacement, first,
            "freed capacity is reused without growing the budget"
        );
        assert_eq!(dag.stats().reserved_bytes, before.reserved_bytes);
    }

    #[test]
    fn failure_after_multiple_new_bricks_rolls_back_every_partial_acquire() {
        let limits = DagLimits {
            max_reserved_bytes: 2 * MIXED_BRICK_BYTES,
            max_mixed_bricks_per_page: 1,
            max_roots: 1,
        };
        let mut grid = vec![Voxel::new(BlockId::AIR); BRICK_VOXEL_COUNT];
        grid[voxel_index(0, 0, 0)] = Voxel::new(BlockId(1));
        grid[voxel_index(8, 0, 0)] = Voxel::new(BlockId(2));
        grid[voxel_index(16, 0, 0)] = Voxel::new(BlockId(3));
        let mut dag = BrickDag::with_limits(limits);

        assert_eq!(
            dag.refresh_cell(CellId(60), &grid),
            RefreshOutcome::Fallback {
                kept: None,
                reason: FallbackReason::BudgetExceeded,
            }
        );
        assert_eq!(dag.stats(), BrickDagStats::default());

        let root = dag
            .refresh_cell(CellId(61), &sparse_grid(BlockId(9)))
            .root()
            .expect("rolled-back page zero capacity is reusable");
        assert_eq!(
            dag.root(root).unwrap().entries[0].kind(),
            RootEntryKind::Mixed { page: 0, slot: 0 }
        );
    }

    #[test]
    fn repeated_identical_bricks_in_one_root_hold_one_reference_per_entry() {
        let mut grid = vec![Voxel::new(BlockId::AIR); BRICK_VOXEL_COUNT];
        grid[voxel_index(0, 0, 0)] = Voxel::new(BlockId(4));
        grid[voxel_index(8, 0, 0)] = Voxel::new(BlockId(4));
        let mut dag = BrickDag::default();

        let root = dag.refresh_cell(CellId(62), &grid).root().unwrap();
        let first = dag.root(root).unwrap().entries[0];
        let second = dag.root(root).unwrap().entries[1];

        assert_eq!(first, second);
        assert_eq!(dag.stats().unique_mixed_bricks, 1);
        assert_eq!(dag.mixed_ref_count(first), Some(2));
        dag.remove_cell(CellId(62));
        assert_eq!(dag.mixed_brick(first), None);
    }
}
