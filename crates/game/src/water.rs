//! Authoritative runtime water and the view the rest of the sim uses.
//!
//! Water starts as the ground recipe. Runtime changes are sparse absolute
//! overrides, including zero; untouched cells continue to read the immutable
//! recipe. Runtime flow is a deterministic, two-phase integer update over a
//! bounded hot-edit reserve and a round-robin chunk frontier.
//! Region faces are sealed until the debit/credit journal required for a
//! lossless host cut is implemented.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use rapier3d::math::Vector;

use crate::movement::{Immersion, PLAYER_EYE_HEIGHT, PLAYER_HEIGHT};
use crate::{
    BLOCK_VOXELS, BlockId, BlockPos, CHUNK_BLOCKS, ColumnCoords, ColumnGround, EntityKey,
    GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, REGION_BLOCKS, Rollback,
    WorldBlockPos, WorldChunkPos,
};

/// Water units in one full cell.
pub const WATER_FULL: u16 = 4096;
/// Most water one donor may send in one flow step.
pub const WATER_MAX_FLOW: u16 = 1024;
/// Donor evaluations in one region tick.
pub const WATER_DONORS_PER_TICK: usize = 512;
/// Edit-seeded donors evaluated first. Part of, not in addition to, the
/// total donor budget.
pub const WATER_HOT_DONORS_PER_TICK: usize = 64;
/// The hot queue admits at most one tick's local reserve.
pub const WATER_HOT_QUEUE_MAX: usize = WATER_HOT_DONORS_PER_TICK;
/// Outgoing edges inspected in one region tick (five per donor).
pub const WATER_EDGE_PROBES_PER_TICK: usize = 2_560;
/// Accepted donor/recipient edges committed in one region tick.
pub const WATER_TRANSFERS_PER_TICK: usize = 1_024;
/// Unique authoritative override cells written in one region tick.
pub const WATER_CELL_WRITES_PER_TICK: usize = 1_024;
/// Chunk activation markers inserted in one flow tick.
pub const WATER_ACTIVATIONS_PER_TICK: usize = 256;
/// Self and face wake visits produced by committed writes in one flow tick.
pub const WATER_WAKE_VISITS_PER_TICK: usize = 7_168;
/// Cells carried by one flow delta.
pub const WATER_DELTA_CELLS_PER_TICK: usize = 1_024;
/// Override cells and their immediate faces sampled while bootstrapping a
/// legacy blob that has no saved scheduler.
const WATER_RESTORE_BOOTSTRAP_CELLS: usize = 512;
/// A 16 x 16 x 16 sim chunk's 4,096 possible local cells.
const WATER_WORK_MASK_WORDS: usize = 64;
const _: () = assert!(
    CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS <= WATER_WORK_MASK_WORDS * u64::BITS as usize
);

mod water_mask_serde {
    use super::WATER_WORK_MASK_WORDS;
    use serde::{Deserialize, Serialize};

    pub fn serialize<S>(
        mask: &[u64; WATER_WORK_MASK_WORDS],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        mask.as_slice().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u64; WATER_WORK_MASK_WORDS], D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let words = Vec::<u64>::deserialize(deserializer)?;
        words.try_into().map_err(|words: Vec<u64>| {
            serde::de::Error::invalid_length(words.len(), &"64 water mask words")
        })
    }
}
/// DF's depth levels: a cell shows 1..=`DF_DEPTH_LEVELS` sevenths of full.
pub const DF_DEPTH_LEVELS: u8 = 7;
/// Deepest water a body wades through, standing on the floor below.
pub const WADE_MAX_DEPTH: u8 = 3;
/// Shallowest water a body swims in.
pub const SWIM_MIN_DEPTH: u8 = 4;
/// Depth at which a head under water is drowning.
pub const DROWN_DEPTH: u8 = 7;
/// Extra path cost of stepping onto a tile with wading-depth water (1..=3).
pub const WADE_COST: u32 = 10;
/// `ColumnGround::water_top` of a block column without recipe water.
pub const NO_WATER: i16 = i16::MIN;
/// `ColumnGround::water_body` of a block column in no water body.
pub const NO_BODY: u32 = u32::MAX;

/// One cell's authoritative dynamic volume delta.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct WaterCell {
    pub pos: WorldBlockPos,
    /// `Some(units)` is an absolute override (including zero). `None`
    /// removes the override and restores the immutable recipe.
    pub units: Option<u16>,
}
/// A compact render/prediction delta. Activation is a small tile/ownership
/// marker; the recipe remains authoritative wherever no cell override exists.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct WaterDelta {
    pub activated: BTreeMap<WorldChunkPos, bool>,
    pub cells: Vec<WaterCell>,
}

impl WaterDelta {
    pub fn is_empty(&self) -> bool {
        self.activated.is_empty() && self.cells.is_empty()
    }
}

/// One sim chunk's fixed-size sparse work mask. `next` resumes local order
/// after the last served bit and makes a wake behind the cursor wait for the
/// wrap.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, state_hash::StateHash,
)]
pub struct WaterWorkChunk {
    #[serde(with = "water_mask_serde")]
    pub mask: [u64; WATER_WORK_MASK_WORDS],
    pub count: u16,
    pub next: u16,
    pub oldest_tick: crate::Tick,
}

impl Default for WaterWorkChunk {
    fn default() -> Self {
        Self {
            mask: [0; WATER_WORK_MASK_WORDS],
            count: 0,
            next: 0,
            oldest_tick: 0,
        }
    }
}

impl WaterWorkChunk {
    fn contains(&self, index: u16) -> bool {
        self.mask[index as usize / 64] & (1_u64 << (index % 64)) != 0
    }

    fn insert(&mut self, index: u16, tick: crate::Tick) -> bool {
        if self.contains(index) {
            return false;
        }
        if self.count == 0 {
            self.oldest_tick = tick;
        }
        self.mask[index as usize / 64] |= 1_u64 << (index % 64);
        self.count += 1;
        true
    }

    fn take_next(&mut self) -> Option<u16> {
        if self.count == 0 {
            return None;
        }
        let cells = (CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS) as u16;
        for step in 0..cells {
            let index = (self.next + step) % cells;
            if self.contains(index) {
                self.mask[index as usize / 64] &= !(1_u64 << (index % 64));
                self.count -= 1;
                self.next = (index + 1) % cells;
                return Some(index);
            }
        }
        unreachable!("non-empty water work mask holds a cell")
    }
}

/// Persisted authoritative work state. The hot set is edit-seeded; every
/// flow-created wake goes to a fixed-size per-chunk mask in `fair`.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct WaterScheduler {
    pub hot: BTreeSet<WorldBlockPos>,
    pub fair: BTreeMap<WorldChunkPos, WaterWorkChunk>,
    pub fair_cells: u64,
    pub fair_oldest_counts: BTreeMap<crate::Tick, u64>,
    pub cursor: Option<WorldChunkPos>,
    pub bootstrap_cursor: Option<WorldBlockPos>,
    pub bootstrap_complete: bool,
}

/// Small rollback-owned scheduler fields. Runtime work collections live in
/// per-entry `UndoMap`s so each capped mutation never clones the whole queue.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct WaterSchedulerMeta {
    pub fair_cells: u64,
    pub cursor: Option<WorldChunkPos>,
    pub bootstrap_cursor: Option<WorldBlockPos>,
    pub bootstrap_complete: bool,
}

impl WaterScheduler {
    pub fn frontier_cells(&self) -> usize {
        self.hot.len() + self.fair_cells as usize
    }

    pub fn oldest_wait_ticks(&self, tick: crate::Tick) -> u64 {
        self.fair_oldest_counts
            .first_key_value()
            .map(|(&oldest, _)| tick.saturating_sub(oldest))
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn note_fair_chunk_started(&mut self, tick: crate::Tick) {
        *self.fair_oldest_counts.entry(tick).or_default() += 1;
    }

    #[cfg(test)]
    fn note_fair_chunk_removed(&mut self, tick: crate::Tick) {
        let remove = {
            let count = self
                .fair_oldest_counts
                .get_mut(&tick)
                .expect("nonempty water chunk has an age index");
            *count -= 1;
            *count == 0
        };
        if remove {
            self.fair_oldest_counts.remove(&tick);
        }
    }

    #[cfg(test)]
    fn wake_hot_or_fair(&mut self, pos: WorldBlockPos, tick: crate::Tick, hot: bool) -> bool {
        if hot && self.hot.len() < WATER_HOT_QUEUE_MAX {
            let inserted = self.hot.insert(pos);
            if inserted {
                if let Some(work) = self.fair.get_mut(&chunk_of_world_block(pos)) {
                    if let Some(index) = local_index_in_chunk(pos) {
                        if work.contains(index) {
                            work.mask[index as usize / 64] &= !(1_u64 << (index % 64));
                            work.count -= 1;
                            self.fair_cells -= 1;
                        }
                    }
                    if work.count == 0 {
                        let oldest = work.oldest_tick;
                        self.fair.remove(&chunk_of_world_block(pos));
                        self.note_fair_chunk_removed(oldest);
                    }
                }
            }
            return inserted;
        }
        self.wake_fair(pos, tick)
    }

    #[cfg(test)]
    fn wake_fair(&mut self, pos: WorldBlockPos, tick: crate::Tick) -> bool {
        if self.hot.contains(&pos) {
            return false;
        }
        let chunk = chunk_of_world_block(pos);
        let Some(index) = local_index_in_chunk(pos) else {
            return false;
        };
        let work = self.fair.entry(chunk).or_default();
        let was_empty = work.count == 0;
        let inserted = work.insert(index, tick);
        if inserted {
            self.fair_cells += 1;
            if was_empty {
                self.note_fair_chunk_started(tick);
            }
        }
        inserted
    }

    #[cfg(test)]
    fn pop_fair(&mut self) -> Option<WorldBlockPos> {
        let chunk = self
            .cursor
            .and_then(|cursor| {
                self.fair
                    .range((
                        std::ops::Bound::Excluded(cursor),
                        std::ops::Bound::Unbounded,
                    ))
                    .next()
                    .map(|(&chunk, _)| chunk)
            })
            .or_else(|| self.fair.first_key_value().map(|(&chunk, _)| chunk))?;
        self.cursor = Some(chunk);
        let work = self.fair.get_mut(&chunk)?;
        let index = work.take_next()?;
        self.fair_cells -= 1;
        let removed = (work.count == 0).then_some(work.oldest_tick);
        if let Some(oldest) = removed {
            self.fair.remove(&chunk);
            self.note_fair_chunk_removed(oldest);
        }
        Some(world_block_in_chunk(chunk, index))
    }
}

/// Portable authoritative water state for one region. Active-work indices
/// and cursors are persisted so a save resumes the same bounded trajectory.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct WaterRegion {
    pub activated: BTreeMap<WorldChunkPos, ()>,
    pub cells: BTreeMap<WorldBlockPos, u16>,
    pub scheduler: WaterScheduler,
    pub volume_delta: i64,
}

impl WaterRegion {
    pub fn is_activated(&self, chunk: WorldChunkPos) -> bool {
        self.activated.contains_key(&chunk)
    }

    pub fn override_at(&self, pos: WorldBlockPos) -> Option<u16> {
        self.cells.get(&pos).copied()
    }

    pub fn units_at(&self, pos: WorldBlockPos) -> Option<u16> {
        self.override_at(pos)
    }

    pub fn merge_delta(&mut self, delta: &WaterDelta) {
        for (&chunk, &active) in &delta.activated {
            if active {
                self.activated.insert(chunk, ());
            } else {
                self.activated.remove(&chunk);
            }
        }
        for cell in &delta.cells {
            if let Some(units) = cell.units {
                self.cells.insert(cell.pos, units);
            } else {
                self.cells.remove(&cell.pos);
            }
        }
    }
}

fn chunk_of_world_block(p: WorldBlockPos) -> WorldChunkPos {
    let cb = CHUNK_BLOCKS as i64;
    WorldChunkPos::new(
        p.x.div_euclid(cb),
        p.y.div_euclid(CHUNK_BLOCKS as i32),
        p.z.div_euclid(cb),
    )
}

fn local_index_in_chunk(p: WorldBlockPos) -> Option<u16> {
    let cb = CHUNK_BLOCKS as i64;
    let x = p.x.rem_euclid(cb) as usize;
    let y = p.y.rem_euclid(CHUNK_BLOCKS as i32) as usize;
    let z = p.z.rem_euclid(cb) as usize;
    let cells = CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS;
    let index = (y * CHUNK_BLOCKS + z) * CHUNK_BLOCKS + x;
    (index < cells).then_some(index as u16)
}

fn world_block_in_chunk(chunk: WorldChunkPos, index: u16) -> WorldBlockPos {
    let cb = CHUNK_BLOCKS as i64;
    let index = i64::from(index);
    let x = index % cb;
    let yz = index / cb;
    let z = yz % cb;
    let y = (yz / cb) as i32;
    WorldBlockPos::new(
        chunk.x * cb + x,
        chunk.y * CHUNK_BLOCKS as i32 + y,
        chunk.z * cb + z,
    )
}

fn six_neighbours(p: WorldBlockPos) -> [WorldBlockPos; 6] {
    [
        WorldBlockPos::new(p.x, p.y + 1, p.z),
        WorldBlockPos::new(p.x, p.y - 1, p.z),
        WorldBlockPos::new(p.x - 1, p.y, p.z),
        WorldBlockPos::new(p.x + 1, p.y, p.z),
        WorldBlockPos::new(p.x, p.y, p.z - 1),
        WorldBlockPos::new(p.x, p.y, p.z + 1),
    ]
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WaterStepStats {
    pub work: u64,
    pub hot_visits: u64,
    pub fair_visits: u64,
    pub edge_probes: u64,
    pub active_cells: u64,
    pub active_chunks: u64,
    pub frontier_cells: u64,
    pub frontier_chunks: u64,
    pub override_cells: u64,
    pub oldest_fair_wait_ticks: u64,
    pub local_transfers: u64,
    pub transferred_units: u64,
    pub deferred_work: u64,
    pub unique_writes: u64,
    pub activation_markers: u64,
    pub activation_recipe_scans: u64,
    pub wake_attempts: u64,
    pub wake_deduplications: u64,
    pub delta_cells: u64,
    pub donor_budget_exhausted: u64,
    pub edge_budget_exhausted: u64,
    pub transfer_budget_exhausted: u64,
    pub write_budget_exhausted: u64,
    pub activation_budget_exhausted: u64,
    pub wake_budget_exhausted: u64,
    pub delta_budget_exhausted: u64,
    pub local_mass_delta: i64,
    pub step_ns: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WaterObservability {
    pub active_cells: u64,
    pub active_chunks: u64,
    pub frontier_cells: u64,
    pub frontier_chunks: u64,
    pub override_cells: u64,
    pub oldest_fair_wait_ticks: u64,
    pub pending_transfers: u64,
    pub oldest_pending_ms: u64,
    pub local_transfers: u64,
    pub face_transfers: u64,
    pub source_transfers: u64,
    pub sink_transfers: u64,
    pub deferred_work: u64,
    pub hot_visits: u64,
    pub fair_visits: u64,
    pub edge_probes: u64,
    pub unique_writes: u64,
    pub activation_markers: u64,
    pub activation_recipe_scans: u64,
    pub wake_attempts: u64,
    pub wake_deduplications: u64,
    pub delta_cells: u64,
    pub donor_budget_exhausted: u64,
    pub edge_budget_exhausted: u64,
    pub transfer_budget_exhausted: u64,
    pub write_budget_exhausted: u64,
    pub activation_budget_exhausted: u64,
    pub wake_budget_exhausted: u64,
    pub delta_budget_exhausted: u64,
    pub transferred_units: u64,
    pub step_count: u64,
    pub step_ns_sum: u64,
    pub step_ns_max: u64,
}

static WATER_REGIONS: Mutex<BTreeMap<crate::RegionCoords, WaterStepStats>> =
    Mutex::new(BTreeMap::new());
static WATER_LOCAL_TRANSFERS: AtomicU64 = AtomicU64::new(0);
static WATER_TRANSFERRED_UNITS: AtomicU64 = AtomicU64::new(0);
static WATER_DEFERRED_WORK: AtomicU64 = AtomicU64::new(0);
static WATER_HOT_VISITS: AtomicU64 = AtomicU64::new(0);
static WATER_FAIR_VISITS: AtomicU64 = AtomicU64::new(0);
static WATER_EDGE_PROBES: AtomicU64 = AtomicU64::new(0);
static WATER_UNIQUE_WRITES: AtomicU64 = AtomicU64::new(0);
static WATER_ACTIVATION_MARKERS: AtomicU64 = AtomicU64::new(0);
static WATER_ACTIVATION_RECIPE_SCANS: AtomicU64 = AtomicU64::new(0);
static WATER_WAKE_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static WATER_WAKE_DEDUPLICATIONS: AtomicU64 = AtomicU64::new(0);
static WATER_DELTA_CELLS: AtomicU64 = AtomicU64::new(0);
static WATER_DONOR_EXHAUSTED: AtomicU64 = AtomicU64::new(0);
static WATER_EDGE_EXHAUSTED: AtomicU64 = AtomicU64::new(0);
static WATER_TRANSFER_EXHAUSTED: AtomicU64 = AtomicU64::new(0);
static WATER_WRITE_EXHAUSTED: AtomicU64 = AtomicU64::new(0);
static WATER_ACTIVATION_EXHAUSTED: AtomicU64 = AtomicU64::new(0);
static WATER_WAKE_EXHAUSTED: AtomicU64 = AtomicU64::new(0);
static WATER_DELTA_EXHAUSTED: AtomicU64 = AtomicU64::new(0);
static WATER_STEP_COUNT: AtomicU64 = AtomicU64::new(0);
static WATER_STEP_NS_SUM: AtomicU64 = AtomicU64::new(0);
static WATER_STEP_NS_MAX: AtomicU64 = AtomicU64::new(0);

pub fn observe_water_step(region: crate::RegionCoords, stats: WaterStepStats) {
    WATER_LOCAL_TRANSFERS.fetch_add(stats.local_transfers, Relaxed);
    WATER_TRANSFERRED_UNITS.fetch_add(stats.transferred_units, Relaxed);
    WATER_DEFERRED_WORK.fetch_add(stats.deferred_work, Relaxed);
    WATER_HOT_VISITS.fetch_add(stats.hot_visits, Relaxed);
    WATER_FAIR_VISITS.fetch_add(stats.fair_visits, Relaxed);
    WATER_EDGE_PROBES.fetch_add(stats.edge_probes, Relaxed);
    WATER_UNIQUE_WRITES.fetch_add(stats.unique_writes, Relaxed);
    WATER_ACTIVATION_MARKERS.fetch_add(stats.activation_markers, Relaxed);
    WATER_ACTIVATION_RECIPE_SCANS.fetch_add(stats.activation_recipe_scans, Relaxed);
    WATER_WAKE_ATTEMPTS.fetch_add(stats.wake_attempts, Relaxed);
    WATER_WAKE_DEDUPLICATIONS.fetch_add(stats.wake_deduplications, Relaxed);
    WATER_DELTA_CELLS.fetch_add(stats.delta_cells, Relaxed);
    WATER_DONOR_EXHAUSTED.fetch_add(stats.donor_budget_exhausted, Relaxed);
    WATER_EDGE_EXHAUSTED.fetch_add(stats.edge_budget_exhausted, Relaxed);
    WATER_TRANSFER_EXHAUSTED.fetch_add(stats.transfer_budget_exhausted, Relaxed);
    WATER_WRITE_EXHAUSTED.fetch_add(stats.write_budget_exhausted, Relaxed);
    WATER_ACTIVATION_EXHAUSTED.fetch_add(stats.activation_budget_exhausted, Relaxed);
    WATER_WAKE_EXHAUSTED.fetch_add(stats.wake_budget_exhausted, Relaxed);
    WATER_DELTA_EXHAUSTED.fetch_add(stats.delta_budget_exhausted, Relaxed);
    WATER_STEP_COUNT.fetch_add(1, Relaxed);
    WATER_STEP_NS_SUM.fetch_add(stats.step_ns, Relaxed);
    WATER_STEP_NS_MAX.fetch_max(stats.step_ns, Relaxed);
    WATER_REGIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(region, stats);
}

pub fn forget_water_observation(region: crate::RegionCoords) {
    WATER_REGIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&region);
}

pub fn water_observability() -> WaterObservability {
    let regions = WATER_REGIONS.lock().unwrap_or_else(|e| e.into_inner());
    WaterObservability {
        active_cells: regions.values().map(|s| s.active_cells).sum(),
        active_chunks: regions.values().map(|s| s.active_chunks).sum(),
        frontier_cells: regions.values().map(|s| s.frontier_cells).sum(),
        frontier_chunks: regions.values().map(|s| s.frontier_chunks).sum(),
        override_cells: regions.values().map(|s| s.override_cells).sum(),
        oldest_fair_wait_ticks: regions
            .values()
            .map(|s| s.oldest_fair_wait_ticks)
            .max()
            .unwrap_or(0),
        local_transfers: WATER_LOCAL_TRANSFERS.load(Relaxed),
        transferred_units: WATER_TRANSFERRED_UNITS.load(Relaxed),
        deferred_work: WATER_DEFERRED_WORK.load(Relaxed),
        hot_visits: WATER_HOT_VISITS.load(Relaxed),
        fair_visits: WATER_FAIR_VISITS.load(Relaxed),
        edge_probes: WATER_EDGE_PROBES.load(Relaxed),
        unique_writes: WATER_UNIQUE_WRITES.load(Relaxed),
        activation_markers: WATER_ACTIVATION_MARKERS.load(Relaxed),
        activation_recipe_scans: WATER_ACTIVATION_RECIPE_SCANS.load(Relaxed),
        wake_attempts: WATER_WAKE_ATTEMPTS.load(Relaxed),
        wake_deduplications: WATER_WAKE_DEDUPLICATIONS.load(Relaxed),
        delta_cells: WATER_DELTA_CELLS.load(Relaxed),
        donor_budget_exhausted: WATER_DONOR_EXHAUSTED.load(Relaxed),
        edge_budget_exhausted: WATER_EDGE_EXHAUSTED.load(Relaxed),
        transfer_budget_exhausted: WATER_TRANSFER_EXHAUSTED.load(Relaxed),
        write_budget_exhausted: WATER_WRITE_EXHAUSTED.load(Relaxed),
        activation_budget_exhausted: WATER_ACTIVATION_EXHAUSTED.load(Relaxed),
        wake_budget_exhausted: WATER_WAKE_EXHAUSTED.load(Relaxed),
        delta_budget_exhausted: WATER_DELTA_EXHAUSTED.load(Relaxed),
        step_count: WATER_STEP_COUNT.load(Relaxed),
        step_ns_sum: WATER_STEP_NS_SUM.load(Relaxed),
        step_ns_max: WATER_STEP_NS_MAX.load(Relaxed),
        ..WaterObservability::default()
    }
}

/// A water body of one region: its region-local lexicographically smallest
/// member block column, packed as `x * REGION_BLOCKS + z`. (`Default` only
/// because `#[undo(map)]` keys need it.)
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct WaterBodyId(pub u32);

/// A body of static water: the 4-connected block columns of one region
/// sharing one recipe `water_top`, labelled once at genesis.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct WaterBody {
    /// The surface, absolute: `(water_top + 1) * WATER_FULL`.
    pub level: i64,
    /// `WATER_FULL` per recipe cell that was not ice at genesis.
    pub volume: u64,
}

/// Water units of a cell holding `block`, at block Y `y` of a column whose
/// recipe water is `recipe` (`ColumnGround::recipe_water`): full for air
/// inside the recipe, else none — water is the recipe's alone, chunks hold
/// no water blocks. The rule the sim and the client's tile mirror share.
pub fn static_water_units(block: BlockId, recipe: Option<(i32, i32)>, y: i32) -> u16 {
    let in_recipe = block == BlockId::AIR && recipe.is_some_and(|(lo, hi)| (lo..=hi).contains(&y));
    if in_recipe { WATER_FULL } else { 0 }
}

/// Labels the recipe water of a region's ground into bodies: a
/// deterministic 4-connected flood over the region's block columns in
/// ascending (x, z) order, joining columns with equal `water_top`. Writes
/// each column's `water_body`; a body's `volume` counts the cells of its
/// columns' recipe ranges for which `is_ice(column, (lx, lz), y)` is false.
pub fn label_water_bodies(
    ground: &mut [(ColumnCoords, ColumnGround)],
    is_ice: impl Fn(ColumnCoords, (usize, usize), i32) -> bool,
) -> BTreeMap<WaterBodyId, WaterBody> {
    let cb = CHUNK_BLOCKS;
    // Block column (x, z) -> (ground index, lx, lz), in ascending order.
    let mut at: BTreeMap<(usize, usize), (usize, usize, usize)> = BTreeMap::new();
    for (i, (c, g)) in ground.iter_mut().enumerate() {
        g.water_body.iter_mut().for_each(|b| *b = NO_BODY);
        for lx in 0..cb {
            for lz in 0..cb {
                if g.recipe_water(lx, lz).is_some() {
                    at.insert((c.x * cb + lx, c.z * cb + lz), (i, lx, lz));
                }
            }
        }
    }
    let mut bodies = BTreeMap::new();
    let keys: Vec<(usize, usize)> = at.keys().copied().collect();
    for start in keys {
        let (i, lx, lz) = at[&start];
        if ground[i].1.water_body[lx * cb + lz] != NO_BODY {
            continue;
        }
        let top = ground[i].1.water_top[lx * cb + lz];
        let id = WaterBodyId((start.0 * REGION_BLOCKS as usize + start.1) as u32);
        let mut body = WaterBody {
            level: (i64::from(top) + 1) * i64::from(WATER_FULL),
            volume: 0,
        };
        ground[i].1.water_body[lx * cb + lz] = id.0;
        let mut stack = vec![start];
        while let Some((x, z)) = stack.pop() {
            let (i, lx, lz) = at[&(x, z)];
            let (c, g) = &ground[i];
            let (lo, hi) = g
                .recipe_water(lx, lz)
                .expect("labelled columns hold recipe water");
            let cells = (lo..=hi).filter(|&y| !is_ice(*c, (lx, lz), y)).count() as u64;
            body.volume += cells * u64::from(WATER_FULL);
            for n in [
                (x + 1, z),
                (x, z + 1),
                (x.wrapping_sub(1), z),
                (x, z.wrapping_sub(1)),
            ] {
                let Some(&(j, nx, nz)) = at.get(&n) else {
                    continue;
                };
                let g = &mut ground[j].1;
                if g.water_body[nx * cb + nz] == NO_BODY && g.water_top[nx * cb + nz] == top {
                    g.water_body[nx * cb + nz] = id.0;
                    stack.push(n);
                }
            }
        }
        bodies.insert(id, body);
    }
    bodies
}

/// The DF depth (0..=7) of `units` of water: 0 for none, else
/// `ceil(units * 7 / WATER_FULL)` clamped to 1..=7.
pub fn df_depth(units: u16) -> u8 {
    if units == 0 {
        return 0;
    }
    let levels = u32::from(DF_DEPTH_LEVELS);
    let full = u32::from(WATER_FULL);
    (u32::from(units) * levels).div_ceil(full).clamp(1, levels) as u8
}

/// Height above the feet, in units, of the feet sample: just inside the
/// cell the feet stand in.
const FEET_SAMPLE_UNITS: f32 = 1.0;

/// How deep a body of `height_units` with its feet at `feet` (region-local
/// units) is in water: the deepest of its feet (+1 unit), waist (half the
/// height) and eye (`PLAYER_EYE_HEIGHT / PLAYER_HEIGHT` of the height)
/// whose cell is wet (`water_units_at > 0`). Reads own ∪ halo, so a body at
/// a seam feels its neighbour's water.
pub fn immersion_at(rb: &Rollback, feet: Vector, height_units: f32) -> Immersion {
    let here = *rb.data.region;
    let bv = BLOCK_VOXELS as f32;
    let (x, z) = ((feet.x / bv).floor() as i32, (feet.z / bv).floor() as i32);
    let wet = |y: f32| {
        let p = WorldBlockPos::from_local(here, BlockPos::new(x, (y / bv).floor() as i32, z));
        rb.water_units_at(p) > 0
    };
    if wet(feet.y + height_units * (PLAYER_EYE_HEIGHT / PLAYER_HEIGHT)) {
        Immersion::Head
    } else if wet(feet.y + height_units * 0.5) {
        Immersion::Waist
    } else if wet(feet.y + FEET_SAMPLE_UNITS) {
        Immersion::Feet
    } else {
        Immersion::Dry
    }
}

impl Rollback {
    /// Every mover's (entity with a `Motion`) immersion for this tick, from
    /// its body pose and the height of its movement collider (the body's
    /// first). Written only when it changed. Runs at the opening of the
    /// tick, after the intents, so every mover plans with it.
    pub(crate) fn tick_immersion_safe(&mut self) {
        let movers: Vec<EntityKey> = self
            .data
            .ecs
            .motion
            .iter()
            .filter_map(|(k, m)| m.map(|_| k))
            .collect();
        for e in movers {
            let Some((feet, height)) = self.mover_extent(e) else {
                continue;
            };
            let Some(mut motion) = *self.data.ecs.motion.try_get(e) else {
                continue;
            };
            let immersion = immersion_at(self, feet, height);
            if motion.immersion != immersion {
                motion.immersion = immersion;
                self.data.ecs.motion.set_safe(e, Some(motion));
            }
        }
    }

    /// The feet (bottom of the movement collider, region-local units) and
    /// height of `e`'s body, upright.
    fn mover_extent(&self, e: EntityKey) -> Option<(Vector, f32)> {
        let handle = (*self.data.ecs.rigidbody.try_get(e))?;
        let body = self.data.physics.bodies.get(handle)?;
        let collider = self
            .data
            .physics
            .colliders
            .get(*body.colliders().first()?)?;
        let aabb = collider.shape().compute_local_aabb();
        let lift = collider
            .position_wrt_parent()
            .map_or(0.0, |p| p.translation.y);
        let t = body.translation();
        Some((
            Vector::new(t.x, t.y + lift + aabb.mins.y, t.z),
            aabb.maxs.y - aabb.mins.y,
        ))
    }

    /// Water units in world block `p`, own region ∪ halo; 0 beyond them.
    /// Static water ([`static_water_units`]): [`WATER_FULL`] for an air
    /// cell inside its column's recipe water — the
    /// region's own `ground` for its cells, the neighbour's (`halo_ground`)
    /// for halo cells.
    pub fn water_units_at(&self, p: WorldBlockPos) -> u16 {
        // Canonical (an unwrapped cell of this region's frame folds), so the
        // halo's ground key below is the neighbour's.
        let p = self.data.canonical_block(p);
        let Some(block) = self.halo_block(p) else {
            return 0;
        };
        if let Some(units) = self.data.water_cells.get(&p).copied() {
            return (block.id == BlockId::AIR).then_some(units).unwrap_or(0);
        }
        let cb = CHUNK_BLOCKS as i64;
        let (lx, lz) = (p.x.rem_euclid(cb) as usize, p.z.rem_euclid(cb) as usize);
        let recipe = self
            .recipe_ground_at(p)
            .and_then(|g| g.recipe_water(lx, lz));
        static_water_units(block.id, recipe, p.y)
    }

    /// A detached copy of this region's portable authoritative water state.
    pub fn water_region(&self) -> WaterRegion {
        self.data.water_region()
    }

    /// Marks `p`'s sim chunk as runtime-touched. Activation is O(1): no
    /// recipe cells are scanned or copied.
    fn activate_water_chunk_safe(&mut self, p: WorldBlockPos) -> bool {
        let p = self.data.canonical_block(p);
        let chunk = chunk_of_world_block(p);
        if self.data.water_chunks.contains_key(&chunk) {
            return false;
        }
        let (owner, _) = p.to_local();
        if owner != *self.data.region {
            return false;
        }
        self.data.water_chunks.insert(chunk, ());
        true
    }

    fn recipe_water_units_at(&self, p: WorldBlockPos) -> u16 {
        let p = self.data.canonical_block(p);
        let cb = CHUNK_BLOCKS as i64;
        let (lx, lz) = (p.x.rem_euclid(cb) as usize, p.z.rem_euclid(cb) as usize);
        let recipe = self
            .recipe_ground_at(p)
            .and_then(|g| g.recipe_water(lx, lz));
        static_water_units(BlockId::AIR, recipe, p.y)
    }

    fn note_fair_chunk_started_safe(&mut self, tick: crate::Tick) {
        let count = self
            .data
            .water_fair_oldest_counts
            .get(&tick)
            .copied()
            .unwrap_or(0);
        self.data
            .water_fair_oldest_counts
            .insert(tick, count.saturating_add(1));
    }

    fn note_fair_chunk_removed_safe(&mut self, tick: crate::Tick) {
        let count = self
            .data
            .water_fair_oldest_counts
            .get(&tick)
            .copied()
            .expect("nonempty water chunk has an age index");
        if count == 1 {
            self.data.water_fair_oldest_counts.remove(&tick);
        } else {
            self.data.water_fair_oldest_counts.insert(tick, count - 1);
        }
    }

    fn wake_water_fair_safe(&mut self, meta: &mut WaterSchedulerMeta, pos: WorldBlockPos) -> bool {
        if self.data.water_hot.contains_key(&pos) {
            return false;
        }
        let chunk = chunk_of_world_block(pos);
        let Some(index) = local_index_in_chunk(pos) else {
            return false;
        };
        let mut work = self
            .data
            .water_fair
            .get(&chunk)
            .cloned()
            .unwrap_or_default();
        if work.contains(index) {
            return false;
        }
        let was_empty = work.count == 0;
        assert!(work.insert(index, *self.data.tick));
        self.data.water_fair.insert(chunk, work);
        meta.fair_cells += 1;
        if was_empty {
            self.note_fair_chunk_started_safe(*self.data.tick);
        }
        true
    }

    fn wake_water_hot_or_fair_safe(
        &mut self,
        meta: &mut WaterSchedulerMeta,
        pos: WorldBlockPos,
        hot: bool,
    ) -> bool {
        if hot && self.data.water_hot.len() < WATER_HOT_QUEUE_MAX {
            if self.data.water_hot.contains_key(&pos) {
                return false;
            }
            let chunk = chunk_of_world_block(pos);
            if let Some(index) = local_index_in_chunk(pos) {
                if let Some(existing) = self.data.water_fair.get(&chunk) {
                    if existing.contains(index) {
                        let mut work = existing.clone();
                        work.mask[index as usize / 64] &= !(1_u64 << (index % 64));
                        work.count -= 1;
                        meta.fair_cells -= 1;
                        if work.count == 0 {
                            let oldest = work.oldest_tick;
                            self.data.water_fair.remove(&chunk);
                            self.note_fair_chunk_removed_safe(oldest);
                        } else {
                            self.data.water_fair.insert(chunk, work);
                        }
                    }
                }
            }
            self.data.water_hot.insert(pos, ());
            return true;
        }
        self.wake_water_fair_safe(meta, pos)
    }

    fn wake_water_cells_safe(
        &mut self,
        meta: &mut WaterSchedulerMeta,
        cells: impl IntoIterator<Item = WorldBlockPos>,
        hot: bool,
        attempts: &mut u64,
        deduplications: &mut u64,
    ) {
        let owner = *self.data.region;
        for p in cells {
            *attempts += 1;
            if p.to_local().0 == owner && !self.wake_water_hot_or_fair_safe(meta, p, hot) {
                *deduplications += 1;
            }
        }
    }

    fn pop_hot_water_safe(&mut self, limit: usize) -> Vec<WorldBlockPos> {
        let selected: Vec<_> = self.data.water_hot.keys().take(limit).copied().collect();
        for p in &selected {
            self.data.water_hot.remove(p);
        }
        selected
    }

    fn pop_fair_water_safe(&mut self, meta: &mut WaterSchedulerMeta) -> Option<WorldBlockPos> {
        let chunk = meta
            .cursor
            .and_then(|cursor| {
                self.data
                    .water_fair
                    .range((
                        std::ops::Bound::Excluded(cursor),
                        std::ops::Bound::Unbounded,
                    ))
                    .next()
                    .map(|(&chunk, _)| chunk)
            })
            .or_else(|| {
                self.data
                    .water_fair
                    .first_key_value()
                    .map(|(&chunk, _)| chunk)
            })?;
        meta.cursor = Some(chunk);
        let mut work = self.data.water_fair.get(&chunk)?.clone();
        let index = work.take_next()?;
        meta.fair_cells -= 1;
        if work.count == 0 {
            let oldest = work.oldest_tick;
            self.data.water_fair.remove(&chunk);
            self.note_fair_chunk_removed_safe(oldest);
        } else {
            self.data.water_fair.insert(chunk, work);
        }
        Some(world_block_in_chunk(chunk, index))
    }

    fn water_frontier_cells(&self) -> usize {
        self.data.water_hot.len() + self.data.water_scheduler_meta.fair_cells as usize
    }

    fn own_open_water_cell(&self, p: WorldBlockPos) -> bool {
        let (owner, local) = p.to_local();
        owner == *self.data.region
            && self
                .block_at([local.x, local.y, local.z])
                .is_none_or(|b| b.id == BlockId::AIR)
    }

    /// One complete bounded, canonical flow step. Each donor reads
    /// start-of-step volume; credits cannot be spent until the next step.
    /// Region edges are closed, so every applied amount is an exact local
    /// debit/credit pair.
    pub(crate) fn tick_water_safe(&mut self) -> WaterStepStats {
        let watch = crate::perf::Stopwatch::start();
        let tick = *self.data.tick;
        let mut scheduler = (*self.data.water_scheduler_meta).clone();
        let mut wake_attempts = 0;
        let mut wake_deduplications = 0;

        // A scheduler-less legacy blob gets a bounded continuation through
        // its saved override map. A normal save already carries this state.
        if !scheduler.bootstrap_complete {
            let mut sampled = Vec::new();
            let iter: Box<dyn Iterator<Item = WorldBlockPos> + '_> =
                match scheduler.bootstrap_cursor {
                    Some(after) => Box::new(
                        self.data
                            .water_cells
                            .range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
                            .map(|(&p, _)| p),
                    ),
                    None => Box::new(self.data.water_cells.keys().copied()),
                };
            sampled.extend(iter.take(WATER_RESTORE_BOOTSTRAP_CELLS));
            if sampled.len() < WATER_RESTORE_BOOTSTRAP_CELLS {
                scheduler.bootstrap_complete = true;
                scheduler.bootstrap_cursor = None;
            } else {
                scheduler.bootstrap_cursor = sampled.last().copied();
            }
            let mut boundary = Vec::with_capacity(sampled.len() * 7);
            for p in sampled {
                boundary.push(p);
                boundary.extend(six_neighbours(p));
            }
            self.wake_water_cells_safe(
                &mut scheduler,
                boundary,
                false,
                &mut wake_attempts,
                &mut wake_deduplications,
            );
        }

        let hot = self.pop_hot_water_safe(WATER_HOT_DONORS_PER_TICK);
        let hot_visits = hot.len() as u64;
        let mut frontier = hot;
        while frontier.len() < WATER_DONORS_PER_TICK {
            let Some(p) = self.pop_fair_water_safe(&mut scheduler) else {
                break;
            };
            frontier.push(p);
        }
        let fair_visits = frontier.len() as u64 - hot_visits;
        let donor_budget_exhausted = u64::from(
            frontier.len() == WATER_DONORS_PER_TICK
                && self.data.water_hot.len() + scheduler.fair_cells as usize != 0,
        );
        let mut requeue = BTreeSet::new();
        let start: BTreeMap<_, _> = frontier
            .iter()
            .map(|&p| (p, self.water_units_at(p)))
            .collect();
        let mut recipient_start = BTreeMap::new();
        let mut debits = BTreeMap::<WorldBlockPos, u16>::new();
        let mut credits = BTreeMap::<WorldBlockPos, u16>::new();
        let mut planned_writes = BTreeSet::new();
        let mut activations = BTreeSet::new();
        let mut edge_probes = 0;
        let mut local_transfers = 0;
        let mut edge_budget_exhausted = 0;
        let mut transfer_budget_exhausted = 0;
        let mut write_budget_exhausted = 0;
        let mut activation_budget_exhausted = 0;
        let mut wake_budget_exhausted = 0;
        let wake_for_flow = WATER_WAKE_VISITS_PER_TICK
            .saturating_sub(wake_attempts as usize)
            .saturating_sub(frontier.len());
        let write_limit_for_wakes = (wake_for_flow / 7).min(WATER_CELL_WRITES_PER_TICK);

        for &p in &frontier {
            let mut available = start[&p].saturating_sub(debits.get(&p).copied().unwrap_or(0));
            if available == 0 {
                continue;
            }
            for (edge, n) in [
                WorldBlockPos::new(p.x, p.y - 1, p.z),
                WorldBlockPos::new(p.x - 1, p.y, p.z),
                WorldBlockPos::new(p.x + 1, p.y, p.z),
                WorldBlockPos::new(p.x, p.y, p.z - 1),
                WorldBlockPos::new(p.x, p.y, p.z + 1),
            ]
            .into_iter()
            .enumerate()
            {
                if available == 0 {
                    break;
                }
                if edge_probes >= WATER_EDGE_PROBES_PER_TICK {
                    edge_budget_exhausted += 1;
                    requeue.insert(p);
                    break;
                }
                edge_probes += 1;
                if !self.own_open_water_cell(n) {
                    continue;
                }
                let initial = *recipient_start
                    .entry(n)
                    .or_insert_with(|| self.water_units_at(n));
                let room = WATER_FULL
                    .saturating_sub(initial)
                    .saturating_sub(credits.get(&n).copied().unwrap_or(0));
                let amount = if edge == 0 {
                    available.min(WATER_MAX_FLOW).min(room)
                } else {
                    start[&p]
                        .saturating_sub(debits.get(&p).copied().unwrap_or(0))
                        .saturating_sub(initial)
                        .div_euclid(2)
                        .min(WATER_MAX_FLOW)
                        .min(available)
                        .min(room)
                };
                if amount == 0 {
                    continue;
                }
                if local_transfers >= WATER_TRANSFERS_PER_TICK {
                    transfer_budget_exhausted += 1;
                    requeue.insert(p);
                    continue;
                }
                let new_activations = [chunk_of_world_block(p), chunk_of_world_block(n)]
                    .into_iter()
                    .filter(|chunk| {
                        !self.data.water_chunks.contains_key(chunk) && !activations.contains(chunk)
                    })
                    .collect::<BTreeSet<_>>();
                if activations.len() + new_activations.len() > WATER_ACTIVATIONS_PER_TICK {
                    activation_budget_exhausted += 1;
                    requeue.insert(p);
                    continue;
                }
                let new_writes = [p, n]
                    .into_iter()
                    .filter(|cell| !planned_writes.contains(cell))
                    .count();
                if planned_writes.len() + new_writes > WATER_CELL_WRITES_PER_TICK {
                    write_budget_exhausted += 1;
                    requeue.insert(p);
                    continue;
                }
                if planned_writes.len() + new_writes > write_limit_for_wakes {
                    wake_budget_exhausted += 1;
                    requeue.insert(p);
                    continue;
                }
                activations.extend(new_activations);
                planned_writes.extend([p, n]);
                *debits.entry(p).or_default() += amount;
                *credits.entry(n).or_default() += amount;
                local_transfers += 1;
                available -= amount;
            }
        }

        debug_assert!(local_transfers <= WATER_TRANSFERS_PER_TICK);
        debug_assert!(edge_probes <= WATER_EDGE_PROBES_PER_TICK);
        debug_assert!(planned_writes.len() <= WATER_CELL_WRITES_PER_TICK);
        debug_assert!(activations.len() <= WATER_ACTIVATIONS_PER_TICK);
        let transferred_units = debits.values().map(|&v| u64::from(v)).sum();
        debug_assert_eq!(
            debits.values().copied().map(u32::from).sum::<u32>(),
            credits.values().copied().map(u32::from).sum::<u32>()
        );
        let mut changed = BTreeMap::new();
        for (&p, &amount) in &debits {
            changed.insert(p, start[&p].saturating_sub(amount));
        }
        for (&p, &amount) in &credits {
            let initial = changed
                .remove(&p)
                .or_else(|| recipient_start.get(&p).copied())
                .unwrap_or_else(|| self.water_units_at(p));
            changed.insert(p, (initial + amount).min(WATER_FULL));
        }
        changed.retain(|&p, &mut units| self.water_units_at(p) != units);
        activations.retain(|chunk| changed.keys().any(|p| &chunk_of_world_block(*p) == chunk));
        debug_assert!(changed.len() <= WATER_DELTA_CELLS_PER_TICK);
        for &p in changed.keys() {
            self.wake_water_cells_safe(
                &mut scheduler,
                std::iter::once(p).chain(six_neighbours(p)),
                false,
                &mut wake_attempts,
                &mut wake_deduplications,
            );
        }
        self.wake_water_cells_safe(
            &mut scheduler,
            requeue,
            false,
            &mut wake_attempts,
            &mut wake_deduplications,
        );
        debug_assert!(wake_attempts <= WATER_WAKE_VISITS_PER_TICK as u64);
        self.commit_water_cells_safe(&changed, &activations);
        self.data.water_scheduler_meta.set(scheduler);

        let frontier = self.water_frontier_cells();
        WaterStepStats {
            work: hot_visits + fair_visits,
            hot_visits,
            fair_visits,
            edge_probes: edge_probes as u64,
            active_cells: frontier as u64,
            active_chunks: self.data.water_chunks.len() as u64,
            frontier_cells: frontier as u64,
            frontier_chunks: self.data.water_fair.len() as u64,
            override_cells: self.data.water_cells.len() as u64,
            oldest_fair_wait_ticks: self
                .data
                .water_fair_oldest_counts
                .first_key_value()
                .map(|(&oldest, _)| tick.saturating_sub(oldest))
                .unwrap_or(0),
            local_transfers: local_transfers as u64,
            transferred_units,
            deferred_work: frontier as u64,
            unique_writes: changed.len() as u64,
            activation_markers: activations.len() as u64,
            activation_recipe_scans: 0,
            wake_attempts,
            wake_deduplications,
            delta_cells: changed.len() as u64,
            donor_budget_exhausted,
            edge_budget_exhausted: edge_budget_exhausted as u64,
            transfer_budget_exhausted: transfer_budget_exhausted as u64,
            write_budget_exhausted: write_budget_exhausted as u64,
            activation_budget_exhausted: activation_budget_exhausted as u64,
            wake_budget_exhausted: wake_budget_exhausted as u64,
            delta_budget_exhausted: 0,
            local_mass_delta: *self.data.water_volume_delta,
            step_ns: watch.elapsed().as_nanos() as u64,
        }
    }

    /// Displace the former volume of a newly solid cell. Capacity was checked
    /// before the block write, and the six neighbours are filled in a fixed
    /// order, with the cell above first.
    pub(crate) fn displace_water_safe(&mut self, from: WorldBlockPos, units: u16) {
        if units == 0 {
            return;
        }
        let mut remaining = units;
        let mut changes = BTreeMap::new();
        changes.insert(from, 0);
        for p in six_neighbours(from) {
            if remaining == 0 {
                break;
            }
            if !self.own_open_water_cell(p) {
                continue;
            }
            let current = self.water_units_at(p);
            let amount = remaining.min(WATER_FULL - current);
            if amount != 0 {
                changes.insert(p, current + amount);
                remaining -= amount;
            }
        }
        debug_assert_eq!(
            remaining, 0,
            "water displacement capacity changed during edit"
        );
        self.apply_water_cells_safe(&changes);
        let mut scheduler = (*self.data.water_scheduler_meta).clone();
        let mut attempts = 0;
        let mut deduplications = 0;
        for &p in changes.keys() {
            self.wake_water_cells_safe(
                &mut scheduler,
                std::iter::once(p).chain(six_neighbours(p)),
                true,
                &mut attempts,
                &mut deduplications,
            );
        }
        self.data.water_scheduler_meta.set(scheduler);
    }

    pub(crate) fn can_displace_water(&self, from: WorldBlockPos, units: u16) -> bool {
        if units == 0 {
            return true;
        }
        let mut room = 0u32;
        for p in six_neighbours(from) {
            if self.own_open_water_cell(p) {
                room += u32::from(WATER_FULL - self.water_units_at(p));
                if room >= u32::from(units) {
                    return true;
                }
            }
        }
        false
    }

    pub(crate) fn wake_water_after_block_safe(&mut self, pos: crate::edit::BlockPos) {
        let world = WorldBlockPos::from_local(
            *self.data.region,
            crate::volume::BlockPos::new(pos[0], pos[1], pos[2]),
        );
        let chunk = chunk_of_world_block(world);
        let activated = self.activate_water_chunk_safe(world);
        let mut scheduler = (*self.data.water_scheduler_meta).clone();
        let mut attempts = 0;
        let mut deduplications = 0;
        self.wake_water_cells_safe(
            &mut scheduler,
            std::iter::once(world).chain(six_neighbours(world)),
            true,
            &mut attempts,
            &mut deduplications,
        );
        self.data.water_scheduler_meta.set(scheduler);
        if activated {
            self.data.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::SetWater(WaterDelta {
                    activated: [(chunk, false)].into(),
                    ..WaterDelta::default()
                }),
            ));
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetWater(WaterDelta {
                    activated: [(chunk, true)].into(),
                    ..WaterDelta::default()
                }),
            ));
        }
    }

    pub(crate) fn water_displaced_by(&self, pos: crate::edit::BlockPos) -> u16 {
        self.water_units_at(WorldBlockPos::from_local(
            *self.data.region,
            crate::volume::BlockPos::new(pos[0], pos[1], pos[2]),
        ))
    }

    pub(crate) fn apply_water_cells_safe(&mut self, values: &BTreeMap<WorldBlockPos, u16>) {
        if values.is_empty() {
            return;
        }
        let activations = values
            .keys()
            .map(|&p| chunk_of_world_block(p))
            .filter(|chunk| !self.data.water_chunks.contains_key(chunk))
            .collect::<BTreeSet<_>>();
        self.commit_water_cells_safe(values, &activations);
    }

    fn commit_water_cells_safe(
        &mut self,
        values: &BTreeMap<WorldBlockPos, u16>,
        activations: &BTreeSet<WorldChunkPos>,
    ) {
        if values.is_empty() && activations.is_empty() {
            return;
        }
        let mut prior = WaterDelta::default();
        let mut next = WaterDelta::default();
        let mut writes = Vec::with_capacity(values.len());
        let mut volume_delta = 0_i64;
        for (&p, &new) in values {
            let old_override = self.data.water_cells.get(&p).copied();
            let recipe = self.recipe_water_units_at(p);
            let new_override = (new != recipe).then_some(new);
            if old_override == new_override {
                continue;
            }
            let old_authoritative = old_override.unwrap_or(recipe);
            volume_delta += i64::from(new) - i64::from(old_authoritative);
            prior.cells.push(WaterCell {
                pos: p,
                units: old_override,
            });
            next.cells.push(WaterCell {
                pos: p,
                units: new_override,
            });
            writes.push((p, new_override));
        }
        let mut activated = Vec::new();
        for &chunk in activations {
            if !self.data.water_chunks.contains_key(&chunk) {
                prior.activated.insert(chunk, false);
                next.activated.insert(chunk, true);
                activated.push(chunk);
            }
        }
        if prior.is_empty() && next.is_empty() {
            return;
        }
        self.data.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetWater(prior),
        ));
        for chunk in activated {
            self.data.water_chunks.insert(chunk, ());
        }
        for (p, units) in writes {
            if let Some(units) = units {
                self.data.water_cells.insert(p, units);
            } else {
                self.data.water_cells.remove(&p);
            }
        }
        if volume_delta != 0 {
            self.data
                .water_volume_delta
                .update(|delta| *delta += volume_delta);
        }
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetWater(next),
        ));
    }

    /// The ground recipe of world block `p`'s column: own (`ground`) inside
    /// the region, else the halo's (`halo_ground`).
    fn recipe_ground_at(&self, p: WorldBlockPos) -> Option<&ColumnGround> {
        let n = REGION_BLOCKS as i32;
        let cb = CHUNK_BLOCKS as i32;
        let [x, _, z] = self.data.local_block(p);
        if (0..n).contains(&x) && (0..n).contains(&z) {
            return self
                .data
                .ground
                .get(&ColumnCoords::new((x / cb) as usize, (z / cb) as usize));
        }
        self.data.halo_ground.get(
            &self
                .data
                .world_chunk([x.div_euclid(cb), 0, z.div_euclid(cb)]),
        )
    }

    /// The water body of region-local block `pos`'s column, when the cell
    /// holds water (`water_units_at > 0`). Own cells only: bodies are per
    /// region, so a halo cell has none here.
    pub fn water_body_at(&self, pos: BlockPos) -> Option<WaterBodyId> {
        let n = REGION_BLOCKS as i32;
        if !(0..n).contains(&pos.x) || !(0..n).contains(&pos.z) {
            return None;
        }
        if self.water_units_at(WorldBlockPos::from_local(*self.data.region, pos)) == 0 {
            return None;
        }
        let cb = CHUNK_BLOCKS as i32;
        let g = self.data.ground.get(&ColumnCoords::new(
            (pos.x / cb) as usize,
            (pos.z / cb) as usize,
        ))?;
        let b = g.water_body[(pos.x % cb) as usize * CHUNK_BLOCKS + (pos.z % cb) as usize];
        (b != NO_BODY).then_some(WaterBodyId(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, Chunk, ChunkCoords};

    fn water_rollback() -> (Rollback, crossbeam::channel::Receiver<GameDataUpdate>) {
        let (send, receive) = crossbeam::channel::unbounded();
        let mut rollback = Rollback::new(Some(send));
        let chunk = Chunk::uniform(Block::new(BlockId::AIR));
        rollback.seed_terrain_genesis(vec![(ChunkCoords::new(0, 1, 0), chunk)]);
        (rollback, receive)
    }

    /// Regression: a one-block river breach synchronously activated thousands
    /// of recipe cells and froze the region (debug-world spill, sparse
    /// overrides/constant-time activation fix).
    #[test]
    fn edit_activation_does_not_materialise_recipe_cells() {
        let (mut rollback, _receive) = water_rollback();
        rollback.wake_water_after_block_safe([1, CHUNK_BLOCKS as i32 + 1, 1]);

        let water = rollback.water_region();
        assert_eq!(water.activated.len(), 1);
        assert!(water.cells.is_empty());
        assert!(water.scheduler.hot.len() <= 7);
        assert!(water.scheduler.frontier_cells() <= 7);
    }

    /// Regression: eager activation converted absent recipe cells to dry, so
    /// one drained zero value could not remain authoritative (water cascade
    /// hand check, absolute override delta).
    #[test]
    fn zero_override_round_trips_without_recipe_refill() {
        let pos = WorldBlockPos::new(1, 2, 3);
        let delta = WaterDelta {
            cells: vec![WaterCell {
                pos,
                units: Some(0),
            }],
            ..WaterDelta::default()
        };
        let mut water = WaterRegion::default();
        water.merge_delta(&delta);
        assert_eq!(water.override_at(pos), Some(0));
        let bytes = bincode::serialize(&water).expect("water serializes");
        let restored: WaterRegion = bincode::deserialize(&bytes).expect("water deserializes");
        assert_eq!(restored.override_at(pos), Some(0));

        water.merge_delta(&WaterDelta {
            cells: vec![WaterCell { pos, units: None }],
            ..WaterDelta::default()
        });
        assert_eq!(water.override_at(pos), None);
    }

    /// Regression: the old advertised work bound hid edge, write and wake
    /// work, overflowing the region tick (in-play freeze, bounded scheduler).
    #[test]
    fn capped_step_is_mass_exact_and_rollback_exact() {
        let (mut rollback, _receive) = water_rollback();
        let donor = WorldBlockPos::new(4, CHUNK_BLOCKS as i32 + 4, 4);
        rollback.apply_water_cells_safe(&BTreeMap::from([(donor, WATER_FULL)]));
        let mut scheduler = (*rollback.data.water_scheduler_meta).clone();
        let mut attempts = 0;
        let mut deduplications = 0;
        rollback.wake_water_cells_safe(
            &mut scheduler,
            [donor],
            false,
            &mut attempts,
            &mut deduplications,
        );
        rollback.data.water_scheduler_meta.set(scheduler);
        let volume_delta = *rollback.data.water_volume_delta;
        rollback.new_transaction();
        let before = crate::state_hash(&rollback);

        let stats = rollback.tick_water_safe();
        let total = std::iter::once(donor)
            .chain(six_neighbours(donor))
            .map(|p| rollback.water_units_at(p))
            .sum::<u16>();
        assert_eq!(total, WATER_FULL);
        assert!(stats.work <= WATER_DONORS_PER_TICK as u64);
        assert!(stats.edge_probes <= WATER_EDGE_PROBES_PER_TICK as u64);
        assert!(stats.local_transfers <= WATER_TRANSFERS_PER_TICK as u64);
        assert!(stats.unique_writes <= WATER_CELL_WRITES_PER_TICK as u64);
        assert!(stats.activation_markers <= WATER_ACTIVATIONS_PER_TICK as u64);
        assert!(stats.wake_attempts <= WATER_WAKE_VISITS_PER_TICK as u64);
        assert!(stats.delta_cells <= WATER_DELTA_CELLS_PER_TICK as u64);
        assert_eq!(stats.local_mass_delta, volume_delta);

        rollback.rollback();
        assert_eq!(
            crate::state_hash(&rollback),
            before,
            "water scheduler, overrides, cursors and volume undo exactly"
        );
    }

    /// Regression: a low canonical-key edit could repeatedly outrun distant
    /// work in the old flat frontier (cascade starvation hand check,
    /// per-chunk round-robin cursor).
    #[test]
    fn fair_cursor_reaches_distant_chunk_after_repeated_low_wakes() {
        let low = WorldBlockPos::new(0, 0, 0);
        let high_chunk = WorldChunkPos::new(REGION_BLOCKS as i64 / CHUNK_BLOCKS as i64, 0, 0);
        let high = world_block_in_chunk(high_chunk, 0);
        let mut scheduler = WaterScheduler {
            bootstrap_complete: true,
            ..WaterScheduler::default()
        };
        assert!(scheduler.wake_fair(low, 0));
        assert!(scheduler.wake_fair(high, 0));
        assert_eq!(scheduler.pop_fair(), Some(low));
        assert!(scheduler.wake_fair(low, 1));
        assert_eq!(scheduler.pop_fair(), Some(high));
    }

    /// Regression: every extra burst candidate used to be admitted to one
    /// unbounded BTree frontier (water breach burst, hot reserve and fair
    /// overflow).
    #[test]
    fn hot_overflow_moves_to_fair_without_loss() {
        let mut scheduler = WaterScheduler {
            bootstrap_complete: true,
            ..WaterScheduler::default()
        };
        let mut admitted = 0;
        for x in 0..(WATER_HOT_QUEUE_MAX + 37) {
            admitted += usize::from(scheduler.wake_hot_or_fair(
                WorldBlockPos::new(x as i64, 0, 0),
                0,
                true,
            ));
        }
        assert_eq!(admitted, WATER_HOT_QUEUE_MAX + 37);
        assert_eq!(scheduler.hot.len(), WATER_HOT_QUEUE_MAX);
        assert_eq!(scheduler.frontier_cells(), WATER_HOT_QUEUE_MAX + 37);
    }

    /// Regression: a saved mid-spill could rebuild a full frontier in a
    /// different order (pause/resume water hand check, persisted masks and
    /// cursors).
    #[test]
    fn scheduler_cursor_round_trips() {
        let mut scheduler = WaterScheduler {
            cursor: Some(WorldChunkPos::new(2, 0, 0)),
            bootstrap_complete: true,
            ..WaterScheduler::default()
        };
        scheduler.wake_fair(WorldBlockPos::new(7, 8, 9), 10);
        scheduler.wake_fair(WorldBlockPos::new(40, 8, 9), 10);
        let bytes = bincode::serialize(&scheduler).expect("scheduler serializes");
        let restored: WaterScheduler =
            bincode::deserialize(&bytes).expect("scheduler deserializes");
        let mut expected = scheduler;
        let mut actual = restored;
        assert_eq!(expected.pop_fair(), actual.pop_fair());
        assert_eq!(expected.pop_fair(), actual.pop_fair());
        assert_eq!(expected, actual);
    }

    /// Regression: 16,384 eager frontier candidates could multiply into
    /// unbounded activation and edge work in one tick (the observed region
    /// stall, fixed donor cap and retained suffix).
    #[test]
    fn donor_cap_preserves_unvisited_work() {
        let cells = CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS;
        if cells < WATER_DONORS_PER_TICK + 80 {
            return;
        }
        let (send, _receive) = crossbeam::channel::unbounded();
        let mut rollback = Rollback::new(Some(send));
        rollback.seed_terrain_genesis(vec![(
            ChunkCoords::new(0, 1, 0),
            Chunk::uniform(Block::new(BlockId(1))),
        )]);
        let chunk = WorldChunkPos::new(0, 1, 0);
        let mut scheduler = WaterScheduler {
            bootstrap_complete: true,
            ..WaterScheduler::default()
        };
        for index in 0..WATER_DONORS_PER_TICK + 80 {
            let p = world_block_in_chunk(chunk, index as u16);
            rollback.data.water_cells.insert_raw(p, WATER_FULL);
            assert!(scheduler.wake_fair(p, 0));
        }
        for (chunk, work) in scheduler.fair {
            rollback.data.water_fair.insert_raw(chunk, work);
        }
        for (tick, count) in scheduler.fair_oldest_counts {
            rollback
                .data
                .water_fair_oldest_counts
                .insert_raw(tick, count);
        }
        rollback.data.water_scheduler_meta.set(WaterSchedulerMeta {
            fair_cells: scheduler.fair_cells,
            cursor: scheduler.cursor,
            bootstrap_cursor: scheduler.bootstrap_cursor,
            bootstrap_complete: scheduler.bootstrap_complete,
        });

        let stats = rollback.tick_water_safe();
        assert_eq!(stats.work, WATER_DONORS_PER_TICK as u64);
        assert_eq!(stats.donor_budget_exhausted, 1);
        assert_eq!(stats.frontier_cells, 80);
        assert_eq!(rollback.water_frontier_cells(), 80);
    }
}
