//! Cave-culling visibility graph (Tommo/MCPE "Advanced Cave Culling"):
//! per-chunk face-pair connectivity computed by flood fill over the chunk's
//! own 32³ non-solid space, and the per-frame camera BFS that hides chunks
//! no sight line can reach. All state here is render-derived — recomputed on
//! remesh, never rollback-hashed.
//! Reference: https://tomcc.github.io/2014/08/31/visibility-1.html

use std::collections::VecDeque;

use bevy::camera::primitives::{Frustum, Sphere};
use bevy::prelude::*;

use super::binary::{CutoutTable, PaddedVoxels};
use super::queue::{ChunkGrid, ChunkKey};

/// All 15 face pairs connected — the mask of an all-air (or absent) chunk.
pub const ALL_CONNECTED: u16 = 0x7FFF;

/// The chunk's face-pair connectivity, attached next to `Mesh3d` by
/// `apply_meshed_chunks`. Derived render state — recomputed on every remesh.
#[derive(bevy::prelude::Component, Copy, Clone)]
pub struct ChunkConnectivity(pub u16);

/// Index of the unordered face pair (a, b), a < b, faces in `Face::index()`
/// order. Bijection onto 0..15 (test-pinned).
pub fn pair_index(a: u32, b: u32) -> u32 {
    debug_assert!(a < b && b < 6);
    // Formula rewritten to avoid underflow: a * (11 - a) / 2 + (b - a - 1)
    // (equivalent to a * 5 - a * (a - 1) / 2 + (b - a - 1))
    a * (11 - a) / 2 + (b - a - 1)
}

/// Bit for the unordered pair (a, b) in either order.
pub fn pair_bit(a: u32, b: u32) -> u16 {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    1 << pair_index(lo, hi)
}

/// Which of a cell's chunk-local coords sit on which face, in Face::index()
/// order: +X = x == 31, -X = x == 0, +Y = y == 31, -Y = y == 0, +Z/-Z same.
fn touched_faces(x: usize, y: usize, z: usize) -> u8 {
    let mut f = 0u8;
    if x == 31 {
        f |= 1 << 0;
    }
    if x == 0 {
        f |= 1 << 1;
    }
    if y == 31 {
        f |= 1 << 2;
    }
    if y == 0 {
        f |= 1 << 3;
    }
    if z == 31 {
        f |= 1 << 4;
    }
    if z == 0 {
        f |= 1 << 5;
    }
    f
}

/// Flood-fill the chunk's own 32³ open space — air and cutout blocks, which
/// can be seen through — (padded interior 1..=32; the 0/33 neighbor shell is
/// deliberately excluded) and record, for each connected open component,
/// which chunk faces it touches; every pair of touched faces is connected.
/// O(32³) worst case, a few hundred µs — runs inside the async mesh task,
/// never on the main thread.
pub fn face_connectivity(p: &PaddedVoxels, cutout: &CutoutTable) -> u16 {
    const N: usize = 32;
    let idx = |x: usize, y: usize, z: usize| x + N * (y + N * z);
    let mut visited = vec![false; N * N * N];
    let mut mask = 0u16;
    let mut stack: Vec<(usize, usize, usize)> = Vec::new();

    for sz in 0..N {
        for sy in 0..N {
            for sx in 0..N {
                if visited[idx(sx, sy, sz)] || p.is_opaque(sx + 1, sy + 1, sz + 1, cutout) {
                    continue;
                }
                // New air component: flood it, accumulate touched faces.
                let mut faces = 0u8;
                visited[idx(sx, sy, sz)] = true;
                stack.push((sx, sy, sz));
                while let Some((x, y, z)) = stack.pop() {
                    faces |= touched_faces(x, y, z);
                    let mut push = |nx: usize, ny: usize, nz: usize| {
                        if !visited[idx(nx, ny, nz)] && !p.is_opaque(nx + 1, ny + 1, nz + 1, cutout)
                        {
                            visited[idx(nx, ny, nz)] = true;
                            stack.push((nx, ny, nz));
                        }
                    };
                    if x + 1 < N {
                        push(x + 1, y, z);
                    }
                    if x > 0 {
                        push(x - 1, y, z);
                    }
                    if y + 1 < N {
                        push(x, y + 1, z);
                    }
                    if y > 0 {
                        push(x, y - 1, z);
                    }
                    if z + 1 < N {
                        push(x, y, z + 1);
                    }
                    if z > 0 {
                        push(x, y, z - 1);
                    }
                }
                // Union all pairs among the touched faces.
                for a in 0..6u32 {
                    if faces >> a & 1 == 0 {
                        continue;
                    }
                    for b in (a + 1)..6u32 {
                        if faces >> b & 1 == 1 {
                            mask |= pair_bit(a, b);
                        }
                    }
                }
                if mask == ALL_CONNECTED {
                    return mask; // can't grow further
                }
            }
        }
    }
    mask
}

/// Runtime switch for the camera-BFS cave culling (F4). Off ⇒ every chunk is
/// restored to `Inherited` and stays that way.
#[derive(Resource)]
pub struct CullingConfig {
    pub graph_enabled: bool,
}

impl Default for CullingConfig {
    fn default() -> Self {
        Self {
            graph_enabled: true,
        }
    }
}

/// Hard bound on the BFS expansion (Chebyshev radius in chunk cells around
/// the camera cell). Cells outside the expansion domain are never reached and
/// their chunks end Hidden, so this must cover the largest loaded view
/// window: a camera on the edge of its home region is `radius + 1` whole
/// regions from the far side of the near window, hence
/// `(MAX_NEAR_RADIUS + 1) * REGION_BRICKS` (128 bricks at the 256 m play region
/// size — exactly enough for `VOXEL_NEAR_RADIUS=3`, not a margin). The flood
/// box is also clipped to the loaded grid's bounding box, so a smaller window
/// pays only for what it loaded. (`ChunkGrid`, keyed by `ChunkKey`, holds only
/// near-window chunks.) The far ring and the worldgen rings beyond it
/// (Phase 4) render `LodChunk` entities, which carry no `ChunkKey` and so are
/// keyless to this BFS: `cull_chunks` queries `With<ChunkKey>` and never
/// reaches or hides them, at any view distance.
pub const CULL_BFS_MAX_CHEBYSHEV: i32 =
    (crate::world_scale::MAX_NEAR_RADIUS + 1) * game::REGION_BRICKS as i32;

/// Entry-face sentinel for the BFS start cell: no entry face, all exits
/// allowed regardless of the cell's connectivity mask.
const NO_ENTRY: u8 = 6;

/// Run/skip counters for `cull_chunks`, so tests (and a curious profiler) can
/// see the change gate working. Optional: harnesses that never insert it pay
/// nothing.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CullingStats {
    /// Frames where the flood actually ran.
    pub runs: u64,
    /// Frames where every input was unchanged and last frame's result stood.
    pub skipped: u64,
    /// Floods that had to rebuild the cached grid table (the chunk grid
    /// changed); every other flood reuses it.
    pub grid_rebuilds: u64,
    /// Summed wall time of the floods since the last `[client-cull]` report.
    pub flood_us: u64,
}

/// Floods between two `[client-cull]` DEBUG reports (`perf_budget` target,
/// raised by `PERF_LOG=1`): per-report volume, never per frame.
const CULL_REPORT_EVERY: u64 = 128;

/// `cull_chunks`' flat copy of the chunk grid: key, entity and connectivity
/// mask per near chunk, plus the grid's bounding box. At the 5x5 window that
/// is ~64k chunks, and re-deriving it from the `BTreeMap` + a random ECS mask
/// lookup per chunk, plus a `get_mut` per chunk to write back, cost ~9 ms of a
/// 12 ms run (2026-09-23 trace; the flood itself is ~3 ms). Rebuilt only when
/// `ChunkGrid` changes; masks are patched per changed entity.
#[derive(Default)]
pub struct GridCache {
    valid: bool,
    keys: Vec<[i32; 3]>,
    entities: Vec<Entity>,
    /// `ALL_CONNECTED` for chunks without a `ChunkConnectivity` yet — the same
    /// fully-open default the flood uses for grid-absent cells.
    masks: Vec<u16>,
    index: bevy::platform::collections::HashMap<Entity, u32>,
    lo: [i32; 3],
    hi: [i32; 3],
    /// What the last run wrote per entry: 0 unknown (write it), 1 reached
    /// (`Inherited`), 2 not reached (`Hidden`). Writes are diffed against it,
    /// so a turning camera touches only chunks whose visibility flips. Safe
    /// because `cull_chunks` is the only writer of chunk `Visibility`.
    applied: Vec<u8>,
    /// Bumped on every rebuild and mask patch: keys `CullingScratch`'s dense
    /// mask grid, which is refilled only when this moves.
    generation: u64,
}

impl GridCache {
    fn rebuild(&mut self, grid: &ChunkGrid, masks: &Query<&ChunkConnectivity>) {
        self.keys.clear();
        self.entities.clear();
        self.masks.clear();
        self.index.clear();
        let mut lo = [i32::MAX; 3];
        let mut hi = [i32::MIN; 3];
        for (key, &entity) in grid.0.iter() {
            for axis in 0..3 {
                lo[axis] = lo[axis].min(key[axis]);
                hi[axis] = hi[axis].max(key[axis]);
            }
            self.index.insert(entity, self.keys.len() as u32);
            self.keys.push(*key);
            self.entities.push(entity);
            self.masks
                .push(masks.get(entity).map_or(ALL_CONNECTED, |m| m.0));
        }
        self.lo = lo;
        self.hi = hi;
        self.applied.clear();
        self.applied.resize(self.keys.len(), 0);
        self.valid = true;
        self.generation += 1;
    }

    fn set_mask(&mut self, entity: Entity, mask: u16) {
        if let Some(&i) = self.index.get(&entity) {
            if self.masks[i as usize] != mask {
                self.masks[i as usize] = mask;
                self.generation += 1;
            }
        }
    }
}

/// Everything the flood's result depends on besides the grid contents and the
/// connectivity masks (those are tracked by change detection). Two frames with
/// equal inputs, an unchanged grid and unchanged masks reach the same cells.
///
/// The cell is the RAW camera cell and the frustum is the camera's own: the
/// clamped start cell and the "drop the gate when outside the domain" decision
/// are functions of these plus the grid's bounding box, and a grid change
/// already forces a recompute.
#[derive(Clone, Copy, PartialEq, Debug)]
struct CullInputs {
    raw_cell: [i32; 3],
    /// `None` = the camera has no `Frustum` (headless).
    frustum: Option<[[f32; 4]; 6]>,
}

/// One BFS node: cell (dense index + box-relative position), the direction
/// bits used on the path so far, and the face it was entered through.
#[derive(Clone, Copy)]
struct Node {
    index: u32,
    pos: [u16; 3],
    dirs: u8,
    entry: u8,
}

/// Allocation-retaining, DENSE storage for the cave-culling traversal, indexed
/// by cell offset inside the expansion box (`x + dx * (y + dy * z)`). The flood
/// used to key `HashSet`s by `(cell, dirs, entry)`; at the production 3x3
/// window (96 chunks across) that was ~10^6 hash operations per frame and
/// measured 34-115 ms per frame (2026-09-19 trace). `Local` ownership keeps it
/// private to `cull_chunks`.
#[derive(Default)]
pub struct CullingScratch {
    /// Per cell, one bit per BFS state `(dirs, entry)` — see `state_bit`.
    /// A cell is reached iff any bit is set.
    visited: Vec<u128>,
    /// Per cell connectivity mask; `ALL_CONNECTED` for grid-absent cells and
    /// chunks that carry no mask yet (both traverse as fully-open air).
    masks: Vec<u16>,
    /// Per cell frustum verdict, computed lazily: 0 unknown, 1 inside, 2 outside.
    in_frustum: Vec<u8>,
    /// Per cell: the flood (`epoch`) that last touched it. `visited` and
    /// `in_frustum` are only meaningful where `stamp == epoch`; a cell is
    /// reset on its first touch in a flood (`touch`) instead of clearing all
    /// three arrays up front — at the 5x5 window the box is ~0.9 M cells
    /// (~17 MB) but a flood touches a few hundred thousand.
    stamp: Vec<u32>,
    epoch: u32,
    /// `(box origin, box dims, GridCache generation)` the dense `masks` were
    /// filled for; a match skips the refill (the box is the whole grid's
    /// bounding box at production window sizes, so it rarely moves).
    masks_key: Option<([i32; 3], [usize; 3], u64)>,
    queue: VecDeque<Node>,
    /// Box origin and dimensions of the last flood (for `is_reached`).
    lo: [i32; 3],
    dims: [usize; 3],
    /// Inputs of the last flood that was applied; `None` forces a recompute.
    last: Option<CullInputs>,
}

/// Bit of BFS state `(dirs, entry)` inside a cell's `u128`. No-backtrack means
/// each axis contributes at most one direction, so `dirs` has 27 legal values
/// (per axis: unused / + / -), and the entry face is always the opposite of
/// one of the used directions — identified by its axis alone. 27 * 3 = 81
/// states. The start node (`dirs == 0`, `NO_ENTRY`) takes bit 0, which no
/// other state can produce (a real entry implies `dirs != 0`).
fn state_bit(dirs: u8, entry: u8) -> u128 {
    let code = (dirs & 3) as u32 + 3 * ((dirs >> 2) & 3) as u32 + 9 * ((dirs >> 4) & 3) as u32;
    let axis = if entry == NO_ENTRY {
        0
    } else {
        (entry / 2) as u32
    };
    1u128 << (code * 3 + axis)
}

/// Every visited-state bit at a cell that makes an arrival in state
/// `(dirs, entry)` redundant, including that state's own bit.
///
/// State `(D, e)` dominates `(D', e)` when `D ⊆ D'`: fewer used directions
/// ban fewer exits (no-backtrack), and the cell's mask sees the same entry, so
/// every move open to `D'` is open to `D`, landing in a successor state with
/// the same entry and a subset of directions — dominance carries on down the
/// path. In a fully open cell (`ALL_CONNECTED`) the entry face constrains
/// nothing, so a subset-direction state with ANY entry dominates. The frustum
/// and box bounds are per cell, not per state, so they don't break it. Pruned
/// arrivals therefore never reach a cell the dominating state won't; the
/// reached set is exactly the unpruned one (`dense_flood_reaches_exactly_…`
/// checks it against the reference BFS). The start state (bit 0) is never
/// counted as a dominator.
fn dominators(dirs: u8, entry: u8, open: bool) -> u128 {
    static TABLE: std::sync::OnceLock<Vec<[u128; 2]>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        // Legal: at most one direction per axis, and the entry face is the
        // opposite of a used direction.
        let legal = |dirs: u8, entry: u8| {
            (0..3).all(|axis| (dirs >> (2 * axis)) & 3 != 3) && dirs & (1 << (entry ^ 1)) != 0
        };
        let mut table = vec![[0u128; 2]; 64 * 6];
        for dirs in 0..64u8 {
            for entry in 0..6u8 {
                if !legal(dirs, entry) {
                    continue;
                }
                let mut same_entry = 0u128;
                let mut any_entry = 0u128;
                // Every submask of `dirs`, including `dirs` itself.
                let mut sub = dirs;
                loop {
                    for e in 0..6u8 {
                        if legal(sub, e) {
                            any_entry |= state_bit(sub, e);
                            if e == entry {
                                same_entry |= state_bit(sub, e);
                            }
                        }
                    }
                    if sub == 0 {
                        break;
                    }
                    sub = (sub - 1) & dirs;
                }
                table[dirs as usize * 6 + entry as usize] = [same_entry, any_entry];
            }
        }
        table
    });
    table[dirs as usize * 6 + entry as usize][open as usize]
}

impl CullingScratch {
    /// Whether the last flood reached `cell`.
    fn is_reached(&self, cell: [i32; 3]) -> bool {
        let mut index = 0usize;
        let mut stride = 1usize;
        for axis in 0..3 {
            let rel = cell[axis] - self.lo[axis];
            if rel < 0 || rel as usize >= self.dims[axis] {
                return false;
            }
            index += rel as usize * stride;
            stride *= self.dims[axis];
        }
        self.stamp[index] == self.epoch && self.visited[index] != 0
    }

    /// Cells the last flood reached, and their summed BFS states (reporting
    /// and tests; walks the whole box).
    fn reached_counts(&self) -> (usize, u32) {
        let mut cells = 0;
        let mut states = 0;
        for (visited, stamp) in self.visited.iter().zip(&self.stamp) {
            if *stamp == self.epoch && *visited != 0 {
                cells += 1;
                states += visited.count_ones();
            }
        }
        (cells, states)
    }

    /// First touch of `index` in this flood resets its per-flood state.
    #[inline]
    fn touch(&mut self, index: usize) {
        if self.stamp[index] != self.epoch {
            self.stamp[index] = self.epoch;
            self.visited[index] = 0;
            self.in_frustum[index] = 0;
        }
    }

    /// `flood`, but `masks` is only consumed when the dense mask grid is stale
    /// for this box and `masks_generation` (`None` always refills).
    fn flood_cached(
        &mut self,
        lo: [i32; 3],
        hi: [i32; 3],
        camera_cell: [i32; 3],
        frustum: Option<&Frustum>,
        masks_generation: Option<u64>,
        masks: impl Iterator<Item = ([i32; 3], u16)>,
    ) {
        let mut dims = [0usize; 3];
        let mut box_lo = [0i32; 3];
        for axis in 0..3 {
            box_lo[axis] = lo[axis].max(camera_cell[axis] - CULL_BFS_MAX_CHEBYSHEV);
            let box_hi = hi[axis].min(camera_cell[axis] + CULL_BFS_MAX_CHEBYSHEV);
            dims[axis] = (box_hi - box_lo[axis] + 1).max(0) as usize;
        }
        let cells = dims[0] * dims[1] * dims[2];
        if self.stamp.len() != cells || self.dims != dims {
            // New box shape: fresh arrays (rare — the grid's bounding box moved).
            self.visited.clear();
            self.visited.resize(cells, 0);
            self.in_frustum.clear();
            self.in_frustum.resize(cells, 0);
            self.stamp.clear();
            self.stamp.resize(cells, 0);
            self.epoch = 0;
            self.masks_key = None;
        }
        self.lo = box_lo;
        self.dims = dims;
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            // Wrapped: stamps from 2^32 floods ago would alias. Start over.
            self.stamp.fill(0);
            self.epoch = 1;
        }
        self.queue.clear();
        if cells == 0 {
            return;
        }
        let strides = [1usize, dims[0], dims[0] * dims[1]];
        let index_of = |cell: [i32; 3]| -> Option<usize> {
            let mut index = 0usize;
            for axis in 0..3 {
                let rel = cell[axis] - box_lo[axis];
                if rel < 0 || rel as usize >= dims[axis] {
                    return None;
                }
                index += rel as usize * strides[axis];
            }
            Some(index)
        };
        let key = masks_generation.map(|generation| (box_lo, dims, generation));
        if key.is_none() || self.masks_key != key {
            self.masks.clear();
            self.masks.resize(cells, ALL_CONNECTED);
            for (cell, mask) in masks {
                if let Some(index) = index_of(cell) {
                    self.masks[index] = mask;
                }
            }
            self.masks_key = key;
        }

        let Some(start) = index_of(camera_cell) else {
            return;
        };
        // Box dimensions never exceed 2 * CULL_BFS_MAX_CHEBYSHEV + 1 (257, or
        // 1025 with `REGION_BLOCKS = 256` — past u8, inside u16).
        let start_pos = [
            (camera_cell[0] - box_lo[0]) as u16,
            (camera_cell[1] - box_lo[1]) as u16,
            (camera_cell[2] - box_lo[2]) as u16,
        ];
        self.touch(start);
        self.visited[start] |= state_bit(0, NO_ENTRY);
        self.queue.push_back(Node {
            index: start as u32,
            pos: start_pos,
            dirs: 0,
            entry: NO_ENTRY,
        });
        while let Some(Node {
            index,
            pos,
            dirs,
            entry,
        }) = self.queue.pop_front()
        {
            let mask = self.masks[index as usize];
            for d in 0..6u8 {
                // No-backtrack: never step opposite to a direction already used.
                if dirs & (1 << (d ^ 1)) != 0 {
                    continue;
                }
                // Connectivity: the cell's own air must connect entry -> exit
                // face. The start cell (no entry face) exits freely. (An
                // all-open mask has every pair bit set, so absent cells and
                // mask-less chunks need no special case.)
                //
                // entry == d would make pair_bit's a<b debug_assert trip, but
                // it's unreachable: entering via `entry` put `entry^1 == d^1`
                // in dirs, and the no-backtrack check above already skipped d.
                debug_assert!(entry == NO_ENTRY || entry != d);
                if entry != NO_ENTRY && mask & pair_bit(entry as u32, d as u32) == 0 {
                    continue;
                }
                let axis = (d / 2) as usize;
                let forward = d & 1 == 0;
                if forward {
                    if pos[axis] as usize + 1 >= dims[axis] {
                        continue;
                    }
                } else if pos[axis] == 0 {
                    continue;
                }
                let mut next_pos = pos;
                let next_index = if forward {
                    next_pos[axis] += 1;
                    index as usize + strides[axis]
                } else {
                    next_pos[axis] -= 1;
                    index as usize - strides[axis]
                };
                self.touch(next_index);
                // Frustum: reject neighbors fully outside the camera's view. No
                // Frustum (headless / ungated) => everything counts as inside.
                if let Some(frustum) = frustum {
                    if self.in_frustum[next_index] == 0 {
                        let center = Vec3::new(
                            (box_lo[0] + next_pos[0] as i32) as f32 * 32.0 + 16.0,
                            (box_lo[1] + next_pos[1] as i32) as f32 * 32.0 + 16.0,
                            (box_lo[2] + next_pos[2] as i32) as f32 * 32.0 + 16.0,
                        );
                        let sphere = Sphere {
                            center: center.into(),
                            radius: 16.0 * 3f32.sqrt(),
                        };
                        self.in_frustum[next_index] = if frustum.intersects_sphere(&sphere, false) {
                            1
                        } else {
                            2
                        };
                    }
                    if self.in_frustum[next_index] == 2 {
                        continue;
                    }
                }
                let next_dirs = dirs | (1 << d);
                let bit = state_bit(next_dirs, d ^ 1);
                // Skip the arrival if the cell already holds a state that
                // dominates it (see `dominators`) — the same reach, far fewer
                // re-expansions of open air.
                let open = self.masks[next_index] == ALL_CONNECTED;
                #[cfg(test)]
                let redundant = if tests::PRUNING_OFF.load(std::sync::atomic::Ordering::Relaxed) {
                    bit
                } else {
                    dominators(next_dirs, d ^ 1, open)
                };
                #[cfg(not(test))]
                let redundant = dominators(next_dirs, d ^ 1, open);
                if self.visited[next_index] & redundant == 0 {
                    self.visited[next_index] |= bit;
                    self.queue.push_back(Node {
                        index: next_index as u32,
                        pos: next_pos,
                        dirs: next_dirs,
                        entry: d ^ 1,
                    });
                }
            }
        }
    }
}

/// Camera BFS over the chunk grid (Tommo cave culling): flood
/// outward from the camera cell; a step from a cell with a connectivity mask
/// is allowed only if the mask connects the entry face to the exit face, a
/// direction whose opposite was already used on the path is banned
/// (no-backtrack), and neighbors outside the camera frustum are rejected.
/// Chunks whose cell the flood never reaches are `Hidden`.
///
/// Guard rails (under-visibility means holes in the world): grid-absent cells
/// and chunks without a `ChunkConnectivity` yet traverse as fully-open air; a
/// missing `Frustum` (headless) counts as everything-inside; the camera cell
/// is clamped into the grid's inflated bounding box when outside it (flying
/// above the band still sees down into it).
///
/// Change-gated: the flood only re-runs when something it depends on changed
/// — the camera cell, the frustum planes, the grid, any connectivity mask, or
/// the config. A still camera over a settled world costs a few comparisons.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn cull_chunks(
    config: Res<CullingConfig>,
    grid: Res<ChunkGrid>,
    cameras: Query<(&GlobalTransform, Option<&Frustum>, &Camera), With<Camera3d>>,
    masks: Query<&ChunkConnectivity>,
    changed_masks: Query<(Entity, &ChunkConnectivity), Changed<ChunkConnectivity>>,
    mut removed_masks: RemovedComponents<ChunkConnectivity>,
    mut chunks: Query<&mut Visibility, With<ChunkKey>>,
    mut scratch: Local<CullingScratch>,
    // Holds entity ids: per session.
    mut cache: crate::session::SessionLocal<GridCache>,
    mut stats: Option<ResMut<CullingStats>>,
) {
    let cache = cache.get();
    // Always drain the removal events, or they pile up across skipped frames.
    // A grid change rebuilds the whole cache below, so patching is only for a
    // still-valid cache. Removals first: a remove + re-add in one frame shows
    // up in both, and the re-added mask must win.
    let mut masks_changed = false;
    for entity in removed_masks.read() {
        masks_changed = true;
        cache.set_mask(entity, ALL_CONNECTED);
    }
    for (entity, mask) in &changed_masks {
        masks_changed = true;
        cache.set_mask(entity, mask.0);
    }
    if !config.graph_enabled {
        // Restore everything once, when the switch flips; after that there is
        // nothing to do (new chunks spawn `Inherited`).
        if config.is_changed() || cache.valid {
            for mut vis in &mut chunks {
                if *vis != Visibility::Inherited {
                    *vis = Visibility::Inherited;
                }
            }
        }
        scratch.last = None;
        cache.valid = false;
        return;
    }
    // No active camera: leave visibility unchanged rather than flashing the
    // world hidden (startup, camera handoff mid-flight).
    let Some((camera_tf, frustum, _)) = cameras.iter().find(|(_, _, cam)| cam.is_active) else {
        scratch.last = None;
        return;
    };
    if grid.0.is_empty() {
        scratch.last = None;
        return;
    }

    // Expansion domain: the grid keys' bounding box inflated by 1 (so sight
    // lines may route through the open shell just outside the loaded band),
    // intersected with the Chebyshev ball around the camera cell. The camera
    // cell is clamped into the box first, which also keeps the ball centered
    // on the effective start cell.
    let translation = camera_tf.translation();
    let raw_cell = [
        (translation.x / 32.0).floor() as i32,
        (translation.y / 32.0).floor() as i32,
        (translation.z / 32.0).floor() as i32,
    ];
    // Change gate. A NaN plane never compares equal, which errs toward
    // recomputing.
    let inputs = CullInputs {
        raw_cell,
        frustum: frustum.map(frustum_planes),
    };
    let world_unchanged = !grid.is_changed() && !config.is_changed() && !masks_changed;
    if world_unchanged && scratch.last == Some(inputs) {
        if let Some(stats) = stats.as_deref_mut() {
            stats.skipped += 1;
        }
        return;
    }

    if !cache.valid || grid.is_changed() {
        cache.rebuild(&grid, &masks);
        if let Some(stats) = stats.as_deref_mut() {
            stats.grid_rebuilds += 1;
        }
    }
    let (mut lo, mut hi) = (cache.lo, cache.hi);
    for axis in 0..3 {
        lo[axis] -= 1;
        hi[axis] += 1;
    }
    let mut camera_cell = raw_cell;
    for axis in 0..3 {
        camera_cell[axis] = camera_cell[axis].clamp(lo[axis], hi[axis]);
    }
    // A clamped start means the real camera is outside the domain (e.g.
    // flying above the band). Sight lines to on-screen chunks then travel
    // through out-of-domain space, so from the displaced start every BFS
    // path would have to route through cells the frustum gate rejects
    // (below/behind the view when looking near-horizontal or up) and the
    // whole grid ends Hidden. Drop the gate for such frames: the domain is
    // small, and Bevy's per-entity frustum culling still skips off-screen
    // meshes. With the camera inside the domain the gate is safe — the
    // staircase of cells along any in-view sight line is itself a valid
    // BFS path and each of its cells passes the (conservative) sphere test.
    let frustum = if camera_cell == raw_cell {
        frustum
    } else {
        None
    };

    // Node = (cell, dirs_used bitmask, entry face). dirs_used only gains bits
    // along a path and no-backtrack caps it at one direction per axis, so at
    // most 3 bits are ever set; with the entry face in the visited key that
    // bounds re-visits per cell to a small constant. The entry face IS part
    // of the key: two paths can reach a cell with identical dirs_used but
    // different entry faces, and the sets of exits the mask allows for those
    // entries differ — deduping them against each other would under-visit
    // (= holes in the world).
    // wasm-safe clock: F4 / the F3 panel turn culling on in the browser too.
    let flood_start = bevy::platform::time::Instant::now();
    scratch.flood_cached(
        lo,
        hi,
        camera_cell,
        frustum,
        Some(cache.generation),
        cache.keys.iter().copied().zip(cache.masks.iter().copied()),
    );
    let flood_us = flood_start.elapsed().as_micros() as u64;
    scratch.last = Some(inputs);
    if let Some(stats) = stats.as_deref_mut() {
        stats.runs += 1;
        stats.flood_us += flood_us;
        if stats.runs % CULL_REPORT_EVERY == 0 {
            // The counts walk the whole box, so only on report runs.
            let (reached, states) = scratch.reached_counts();
            log::debug!(
                target: "perf_budget",
                "[client-cull] floods={} avg_flood_ms={:.2} box={:?} ({} cells) grid_chunks={} reached={} states={} skipped={} grid_rebuilds={}",
                CULL_REPORT_EVERY,
                stats.flood_us as f64 / CULL_REPORT_EVERY as f64 / 1000.0,
                scratch.dims,
                scratch.visited.len(),
                cache.keys.len(),
                reached,
                states,
                stats.skipped,
                stats.grid_rebuilds,
            );
            stats.flood_us = 0;
        }
    }

    // Apply only what flipped since the last applied run (see
    // `GridCache::applied`), and write only on change (like `dedupe_halo_entities`)
    // to avoid change-detection churn.
    let GridCache {
        keys,
        entities,
        applied,
        ..
    } = &mut *cache;
    for ((key, entity), applied) in keys.iter().zip(entities.iter()).zip(applied.iter_mut()) {
        let reached = scratch.is_reached(*key);
        let state = if reached { 1 } else { 2 };
        if *applied == state {
            continue;
        }
        let Ok(mut vis) = chunks.get_mut(*entity) else {
            continue;
        };
        let want = if reached {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
        *applied = state;
    }
}

/// The frustum as plain numbers, for the change gate (`Frustum` has no `PartialEq`).
fn frustum_planes(frustum: &Frustum) -> [[f32; 4]; 6] {
    std::array::from_fn(|i| frustum.half_spaces[i].normal_d().to_array())
}

/// Registers `cull_chunks` at its one correct slot: `PostUpdate`, after
/// `UpdateFrusta` and before `VisibilityPropagate`. The BFS must read the
/// frustum of the frame being rendered — scheduled in `Update` it read last
/// frame's (`update_frusta` runs in `PostUpdate`), so every camera turn
/// visibly culled faces that were already inside the FOV at the leading
/// screen edge — and its `Visibility` writes must still land before this
/// frame's propagation. Shared by production (`renderer/mod.rs`) and the
/// headless pipeline tests so the schedules cannot drift apart.
pub fn cull_plugin(app: &mut App) {
    use crate::session::{SessionResetExt, world_systems_enabled};
    use bevy::camera::visibility::VisibilitySystems;
    app.add_systems(
        Update,
        super::queue::requeue_for_culling
            .before(super::queue::spawn_mesh_tasks)
            .run_if(world_systems_enabled()),
    );
    app.add_session_reset::<CullingStats>().add_systems(
        PostUpdate,
        cull_chunks
            .after(VisibilitySystems::UpdateFrusta)
            .before(VisibilitySystems::VisibilityPropagate)
            .run_if(world_systems_enabled()),
    );
}

/// F4 flips the cave-culling graph on/off at runtime.
pub fn toggle_culling(keys: Res<ButtonInput<KeyCode>>, mut config: ResMut<CullingConfig>) {
    if keys.just_pressed(KeyCode::F4) {
        config.graph_enabled = !config.graph_enabled;
        info!(
            "cave culling: {}",
            if config.graph_enabled { "on" } else { "off" }
        );
    }
}

/// Native-only: opt every camera into the depth prepass + HZB two-phase
/// occlusion culling (experimental in Bevy 0.16+; REQUIRES DepthPrepass or
/// the component is ignored; incompatible with deferred, which we don't use).
/// Chunk meshes already prepass correctly via the packed-format prepass
/// vertex shader (phase 2). Registered only when `native_occlusion_enabled`.
#[cfg(not(target_arch = "wasm32"))]
pub fn enable_native_occlusion(
    mut commands: Commands,
    cameras: Query<
        Entity,
        (
            With<Camera3d>,
            Without<bevy::core_pipeline::prepass::DepthPrepass>,
        ),
    >,
) {
    for e in &cameras {
        commands.entity(e).insert((
            bevy::core_pipeline::prepass::DepthPrepass,
            bevy::render::occlusion_culling::OcclusionCulling,
        ));
    }
}

/// Depth prepass + HZB occlusion culling is OFF by default;
/// `VOXEL_OCCLUSION=on|1|true` turns it back on. The prepass re-draws every
/// submitted quad, and on open terrain the HZB culled only ~3% of chunks:
/// 2026-09-22 render bench (`full`, 1080p, Intel iGPU) went 27.7 -> 21.1 ms
/// p50 and 52.9 -> 39.1 ms p95 without it, for 1.3x -> 1.8x overdraw.
/// Re-measure in cave-heavy scenes or on a discrete GPU before flipping back.
#[cfg(not(target_arch = "wasm32"))]
pub fn native_occlusion_enabled() -> bool {
    occlusion_from_env(std::env::var("VOXEL_OCCLUSION").ok().as_deref())
}

#[cfg(not(target_arch = "wasm32"))]
fn occlusion_from_env(value: Option<&str>) -> bool {
    matches!(
        value.map(str::to_ascii_lowercase).as_deref(),
        Some("on" | "1" | "true")
    )
}

#[cfg(test)]
mod tests {

    use super::*;

    use game::{BRICK_VOXEL_COUNT, BlockId, Voxel};

    // --- Camera BFS culling (headless pipeline scenes) -------------------
    //
    // These run the real meshing pipeline (`pipeline_app` registers the full
    // production chain, `cull_chunks` last) with a headless camera: explicit
    // `GlobalTransform` + active `Camera` (MinimalPlugins has no transform
    // propagation; a missing/default `Frustum` counts as "everything
    // inside"). Assertions are on END-STATES via `wait_until`, never on
    // transient scheduling intermediates — the mesher can spawn, complete,
    // and apply within a single update.

    use super::super::queue::MeshBudgets;
    use super::super::queue::tests::{
        pipeline_app, spawn_chunk, voxels_with, wait_until, wait_until_settled,
    };

    /// Test-only switch: flood without dominance pruning (exact-state dedupe
    /// only), for the timing comparison below.
    pub(super) static PRUNING_OFF: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    fn vis(app: &App, e: Entity) -> Visibility {
        *app.world()
            .entity(e)
            .get::<Visibility>()
            .expect("chunk has Visibility")
    }

    /// Regression: the cull BFS must use the CURRENT frame's frustum.
    /// `update_frusta` runs in `PostUpdate`, so a cull pass scheduled in
    /// `Update` reads last frame's frustum while the frame renders with this
    /// frame's — turning the camera visibly culls faces that are already
    /// back inside the FOV. With real frustum updates in the loop, turning
    /// toward a chunk must reveal it in the SAME frame.
    #[test]
    fn turning_toward_a_chunk_reveals_it_in_the_same_frame() {
        let mut app = pipeline_app(MeshBudgets::default());
        app.add_plugins((
            bevy::transform::TransformPlugin,
            bevy::camera::CameraProjectionPlugin,
        ));
        // Camera in cell (0,0,0) looking AWAY (-X) from a chunk at cell (2,0,0).
        let cam = app
            .world_mut()
            .spawn((
                Camera3d::default(),
                Camera {
                    is_active: true,
                    ..Default::default()
                },
                Projection::default(),
                Transform::from_translation(Vec3::new(8.0, 8.0, 8.0))
                    .looking_to(Vec3::NEG_X, Vec3::Y),
            ))
            .id();
        let chunk = spawn_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(64.0, 0.0, 0.0),
            voxels_with(&[[5, 5, 5]]),
        );
        // Anchor at cell (-2,0,0) so the grid bbox contains the camera cell:
        // a camera outside the domain disables the frustum gate entirely
        // (see `cull_chunks`), and this test is about the gate's timing.
        spawn_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(-64.0, 0.0, 0.0),
            voxels_with(&[[5, 5, 5]]),
        );
        assert!(
            wait_until(&mut app, |app| vis(app, chunk) == Visibility::Hidden),
            "premise: a chunk fully behind the camera ends Hidden"
        );
        // Turn to face the chunk, then run exactly ONE frame.
        app.world_mut()
            .entity_mut(cam)
            .get_mut::<Transform>()
            .unwrap()
            .look_to(Vec3::X, Vec3::Y);
        app.update();
        assert_eq!(
            vis(&app, chunk),
            Visibility::Inherited,
            "a chunk inside the new frustum must show the same frame it enters the FOV"
        );
    }

    /// Regression: flying above the loaded band must not hide on-screen
    /// ground. The camera cell clamps into the grid's inflated bbox, so the
    /// BFS starts displaced far below the real camera; with the frustum gate
    /// applied to every step, all exits from the clamped start sit below the
    /// bottom frustum plane when looking near-horizontal, every path dies,
    /// and the whole live grid ends Hidden while the LOD rings behind it
    /// still render — ground vanishes exactly when looking up or flying up.
    #[test]
    fn flying_above_the_band_keeps_onscreen_ground_visible() {
        let mut app = pipeline_app(MeshBudgets::default());
        app.add_plugins((
            bevy::transform::TransformPlugin,
            bevy::camera::CameraProjectionPlugin,
        ));
        // Ground band: half-solid chunks along +X at cell y = 0.
        let mut ground = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        for z in 0..32 {
            for y in 0..16 {
                for x in 0..32 {
                    ground[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(1));
                }
            }
        }
        let mut chunks = Vec::new();
        for cx in 0..=20 {
            chunks.push(spawn_chunk(
                &mut app,
                Vec3::ZERO,
                Vec3::new(cx as f32 * 32.0, 0.0, 128.0),
                ground.clone(),
            ));
        }
        // Camera at cell (4, 6, 4) — 6 cells above the band top (cell 0) —
        // pitched 10° below horizontal along +X. With a 45° vertical FOV the
        // ground from ~x=417 outward (cell 13+) is inside the frustum.
        let pitch = 10f32.to_radians();
        app.world_mut().spawn((
            Camera3d::default(),
            Camera {
                is_active: true,
                ..Default::default()
            },
            Projection::default(),
            Transform::from_translation(Vec3::new(144.0, 200.0, 144.0))
                .looking_to(Vec3::new(pitch.cos(), -pitch.sin(), 0.0), Vec3::Y),
        ));
        assert!(wait_until_settled(&mut app), "pipeline must settle");
        app.update();
        let target = chunks[15]; // cell (15, 0, 4): on screen, depression ~27.6° < 32.5°
        assert_eq!(
            vis(&app, target),
            Visibility::Inherited,
            "ground inside the frustum must stay visible when the camera flies above the band"
        );
    }
}
