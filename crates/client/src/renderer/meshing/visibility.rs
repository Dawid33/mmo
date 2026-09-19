//! Cave-culling visibility graph (Tommo/MCPE "Advanced Cave Culling"):
//! per-chunk face-pair connectivity computed by flood fill over the chunk's
//! own 32³ non-solid space, and the per-frame camera BFS that hides chunks
//! no sight line can reach. All state here is render-derived — recomputed on
//! remesh, never rollback-hashed.
//! Reference: https://tomcc.github.io/2014/08/31/visibility-1.html

use std::collections::VecDeque;

use bevy::camera::primitives::{Frustum, Sphere};
use bevy::prelude::*;

use super::binary::PaddedVoxels;
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

/// Flood-fill the chunk's own 32³ non-solid space (padded interior 1..=32;
/// the 0/33 neighbor shell is deliberately excluded) and record, for each
/// connected air component, which chunk faces it touches; every pair of
/// touched faces is connected. O(32³) worst case, a few hundred µs — runs
/// inside the async mesh task, never on the main thread.
pub fn face_connectivity(p: &PaddedVoxels) -> u16 {
    const N: usize = 32;
    let idx = |x: usize, y: usize, z: usize| x + N * (y + N * z);
    let mut visited = vec![false; N * N * N];
    let mut mask = 0u16;
    let mut stack: Vec<(usize, usize, usize)> = Vec::new();

    for sz in 0..N {
        for sy in 0..N {
            for sx in 0..N {
                if visited[idx(sx, sy, sz)] || p.is_solid(sx + 1, sy + 1, sz + 1) {
                    continue;
                }
                // New air component: flood it, accumulate touched faces.
                let mut faces = 0u8;
                visited[idx(sx, sy, sz)] = true;
                stack.push((sx, sy, sz));
                while let Some((x, y, z)) = stack.pop() {
                    faces |= touched_faces(x, y, z);
                    let mut push = |nx: usize, ny: usize, nz: usize| {
                        if !visited[idx(nx, ny, nz)] && !p.is_solid(nx + 1, ny + 1, nz + 1) {
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
/// their chunks end Hidden, so this must overshoot the largest loaded view
/// window — today that's the near 3×3 region window alone, ≈24 chunks across
/// (`ChunkGrid`, keyed by `ChunkKey`, holds only near-window chunks). The far
/// ring and the worldgen rings beyond it (Phase 4) render `LodChunk` entities,
/// which carry no `ChunkKey` and so are keyless to this BFS: `cull_chunks`
/// queries `With<ChunkKey>` and never reaches or hides them, at any view
/// distance. 64 overshoots the near window by more than 2×.
pub const CULL_BFS_MAX_CHEBYSHEV: i32 = 64;

/// Entry-face sentinel for the BFS start cell: no entry face, all exits
/// allowed regardless of the cell's connectivity mask.
const NO_ENTRY: u8 = 6;

/// Unit steps in `Face::index()` order (PosX, NegX, PosY, NegY, PosZ, NegZ) —
/// the locked order in which `opposite(d) = d ^ 1`.
#[cfg(test)]
const DIR_STEPS: [[i32; 3]; 6] = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
];

/// Run/skip counters for `cull_chunks`, so tests (and a curious profiler) can
/// see the change gate working. Optional: harnesses that never insert it pay
/// nothing.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CullingStats {
    /// Frames where the flood actually ran.
    pub runs: u64,
    /// Frames where every input was unchanged and last frame's result stood.
    pub skipped: u64,
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
    pos: [u8; 3],
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
    let axis = if entry == NO_ENTRY { 0 } else { (entry / 2) as u32 };
    1u128 << (code * 3 + axis)
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
        self.visited[index] != 0
    }

    /// The camera BFS (see `cull_chunks` for the rules), over the box
    /// `lo..=hi` intersected with the Chebyshev ball around `camera_cell`
    /// (which must lie inside `lo..=hi`). `masks` yields the connectivity of
    /// every chunk that has one; everything else is fully open.
    fn flood(
        &mut self,
        lo: [i32; 3],
        hi: [i32; 3],
        camera_cell: [i32; 3],
        frustum: Option<&Frustum>,
        masks: impl Iterator<Item = ([i32; 3], u16)>,
    ) {
        let mut dims = [0usize; 3];
        let mut box_lo = [0i32; 3];
        for axis in 0..3 {
            box_lo[axis] = lo[axis].max(camera_cell[axis] - CULL_BFS_MAX_CHEBYSHEV);
            let box_hi = hi[axis].min(camera_cell[axis] + CULL_BFS_MAX_CHEBYSHEV);
            dims[axis] = (box_hi - box_lo[axis] + 1).max(0) as usize;
        }
        self.lo = box_lo;
        self.dims = dims;
        let cells = dims[0] * dims[1] * dims[2];
        self.visited.clear();
        self.visited.resize(cells, 0);
        self.masks.clear();
        self.masks.resize(cells, ALL_CONNECTED);
        self.in_frustum.clear();
        self.in_frustum.resize(cells, 0);
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
        for (cell, mask) in masks {
            if let Some(index) = index_of(cell) {
                self.masks[index] = mask;
            }
        }

        let Some(start) = index_of(camera_cell) else {
            return;
        };
        // Box dimensions never exceed 2 * CULL_BFS_MAX_CHEBYSHEV + 1 = 129.
        let start_pos = [
            (camera_cell[0] - box_lo[0]) as u8,
            (camera_cell[1] - box_lo[1]) as u8,
            (camera_cell[2] - box_lo[2]) as u8,
        ];
        self.visited[start] |= state_bit(0, NO_ENTRY);
        self.queue.push_back(Node { index: start as u32, pos: start_pos, dirs: 0, entry: NO_ENTRY });
        while let Some(Node { index, pos, dirs, entry }) = self.queue.pop_front() {
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
                        self.in_frustum[next_index] =
                            if frustum.intersects_sphere(&sphere, false) { 1 } else { 2 };
                    }
                    if self.in_frustum[next_index] == 2 {
                        continue;
                    }
                }
                let next_dirs = dirs | (1 << d);
                let bit = state_bit(next_dirs, d ^ 1);
                if self.visited[next_index] & bit == 0 {
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
    changed_masks: Query<(), Changed<ChunkConnectivity>>,
    mut removed_masks: RemovedComponents<ChunkConnectivity>,
    mut chunks: Query<&mut Visibility, With<ChunkKey>>,
    mut scratch: Local<CullingScratch>,
    mut stats: Option<ResMut<CullingStats>>,
) {
    // Always drain the removal events, or they pile up across skipped frames.
    let masks_changed = removed_masks.read().count() > 0 || !changed_masks.is_empty();
    if !config.graph_enabled {
        scratch.last = None;
        for mut vis in &mut chunks {
            if *vis != Visibility::Inherited {
                *vis = Visibility::Inherited;
            }
        }
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
    let inputs = CullInputs { raw_cell, frustum: frustum.map(frustum_planes) };
    let world_unchanged = !grid.is_changed() && !config.is_changed() && !masks_changed;
    if world_unchanged && scratch.last == Some(inputs) {
        if let Some(stats) = stats.as_deref_mut() {
            stats.skipped += 1;
        }
        return;
    }

    let mut lo = [i32::MAX; 3];
    let mut hi = [i32::MIN; 3];
    for key in grid.0.keys() {
        for axis in 0..3 {
            lo[axis] = lo[axis].min(key[axis]);
            hi[axis] = hi[axis].max(key[axis]);
        }
    }
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
    scratch.flood(
        lo,
        hi,
        camera_cell,
        frustum,
        grid.0
            .iter()
            .filter_map(|(key, &entity)| masks.get(entity).ok().map(|mask| (*key, mask.0))),
    );
    scratch.last = Some(inputs);
    if let Some(stats) = stats.as_deref_mut() {
        stats.runs += 1;
    }

    // Apply, writing only on change (like `dedupe_ghosts`) to avoid
    // change-detection churn.
    for (key, &entity) in grid.0.iter() {
        let Ok(mut vis) = chunks.get_mut(entity) else {
            continue;
        };
        let want = if scratch.is_reached(*key) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
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
    use bevy::camera::visibility::VisibilitySystems;
    app.init_resource::<CullingStats>().add_systems(
        PostUpdate,
        cull_chunks
            .after(VisibilitySystems::UpdateFrusta)
            .before(VisibilitySystems::VisibilityPropagate),
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
/// vertex shader (phase 2). Own commit: revert this one commit to disable.
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
            bevy::render::experimental::occlusion_culling::OcclusionCulling,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::super::binary::Face;
    use super::*;
    use std::collections::HashSet;
    use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

    fn interior(cells: &[[usize; 3]]) -> PaddedVoxels {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for [x, y, z] in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(1));
        }
        PaddedVoxels::from_interior(&v)
    }

    fn solid_except(air: &[[usize; 3]]) -> PaddedVoxels {
        let mut v = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
        for [x, y, z] in air {
            v[x + 32 * y + 32 * 32 * z] = Voxel::default();
        }
        PaddedVoxels::from_interior(&v)
    }

    #[test]
    fn pair_index_is_a_bijection_onto_0_15() {
        let mut seen = [false; 15];
        for a in 0..6u32 {
            for b in (a + 1)..6u32 {
                let i = pair_index(a, b) as usize;
                assert!(!seen[i], "pair ({a},{b}) collides at {i}");
                seen[i] = true;
            }
        }
        assert!(seen.iter().all(|&s| s), "all 15 indices hit");
    }

    #[test]
    fn empty_chunk_connects_everything() {
        assert_eq!(face_connectivity(&interior(&[])), ALL_CONNECTED);
    }

    #[test]
    fn solid_chunk_connects_nothing() {
        assert_eq!(face_connectivity(&solid_except(&[])), 0);
    }

    #[test]
    fn straight_tunnel_connects_exactly_its_two_faces() {
        // 1-voxel tunnel along X at y=5, z=5 through an otherwise solid chunk.
        let air: Vec<[usize; 3]> = (0..32).map(|x| [x, 5, 5]).collect();
        let mask = face_connectivity(&solid_except(&air));
        assert_eq!(mask, pair_bit(Face::PosX.index(), Face::NegX.index()));
    }

    #[test]
    fn l_tunnel_connects_exactly_its_corner_pair() {
        // Air along -X to center, then down to -Y: connects NegX <-> NegY only.
        let mut air: Vec<[usize; 3]> = (0..=16).map(|x| [x, 16, 16]).collect();
        air.extend((0..=16).map(|y| [16, y, 16]));
        let mask = face_connectivity(&solid_except(&air));
        assert_eq!(mask, pair_bit(Face::NegX.index(), Face::NegY.index()));
    }

    #[test]
    fn open_column_connects_vertical_pair() {
        let air: Vec<[usize; 3]> = (0..32).map(|y| [7, y, 9]).collect();
        let mask = face_connectivity(&solid_except(&air));
        assert_eq!(mask, pair_bit(Face::PosY.index(), Face::NegY.index()));
    }

    #[test]
    fn two_isolated_rooms_union_their_pairs_without_cross_connecting() {
        // Room A: tunnel X (touches ±X). Room B: tunnel Z (touches ±Z),
        // far apart, no shared air. Mask = both pairs, and NOT e.g. PosX<->PosZ.
        let mut air: Vec<[usize; 3]> = (0..32).map(|x| [x, 3, 3]).collect();
        air.extend((0..32).map(|z| [28, 28, z]));
        let mask = face_connectivity(&solid_except(&air));
        let expect = pair_bit(Face::PosX.index(), Face::NegX.index())
            | pair_bit(Face::PosZ.index(), Face::NegZ.index());
        assert_eq!(mask, expect);
    }

    #[test]
    fn interior_pocket_touching_no_face_adds_nothing() {
        // A sealed air pocket in the middle: connects no faces.
        let air: Vec<[usize; 3]> = vec![[15, 15, 15], [16, 15, 15]];
        assert_eq!(face_connectivity(&solid_except(&air)), 0);
    }

    // --- Camera BFS culling (headless pipeline scenes) -------------------
    //
    // These run the real meshing pipeline (`pipeline_app` registers the full
    // production chain, `cull_chunks` last) with a headless camera: explicit
    // `GlobalTransform` + active `Camera` (MinimalPlugins has no transform
    // propagation; a missing/default `Frustum` counts as "everything
    // inside"). Assertions are on END-STATES via `wait_until`, never on
    // transient scheduling intermediates — the mesher can spawn, complete,
    // and apply within a single update.

    use super::super::queue::tests::{
        pipeline_app, spawn_chunk, voxels_with, wait_until, wait_until_settled,
    };
    use super::super::queue::MeshBudgets;
    use crate::renderer::bridge::VoxelData;

    /// The pre-2026-09-19 flood, verbatim in structure: `HashSet`s keyed by
    /// `(cell, dirs, entry)`, a grid lookup per popped node, a frustum test per
    /// step. Kept as the behavioural reference for the dense rewrite.
    fn reference_reached(
        lo: [i32; 3],
        hi: [i32; 3],
        camera_cell: [i32; 3],
        frustum: Option<&Frustum>,
        masks: &std::collections::BTreeMap<[i32; 3], u16>,
    ) -> HashSet<[i32; 3]> {
        let in_domain = |cell: [i32; 3]| {
            (0..3).all(|axis| {
                cell[axis] >= lo[axis]
                    && cell[axis] <= hi[axis]
                    && (cell[axis] - camera_cell[axis]).abs() <= CULL_BFS_MAX_CHEBYSHEV
            })
        };
        let mut reached = HashSet::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert((camera_cell, 0u8, NO_ENTRY));
        queue.push_back((camera_cell, 0u8, NO_ENTRY));
        while let Some((cell, dirs, entry)) = queue.pop_front() {
            reached.insert(cell);
            let mask = masks.get(&cell).copied();
            for d in 0..6u8 {
                if dirs & (1 << (d ^ 1)) != 0 {
                    continue;
                }
                if let Some(mask) = mask {
                    if entry != NO_ENTRY && mask & pair_bit(entry as u32, d as u32) == 0 {
                        continue;
                    }
                }
                let step = DIR_STEPS[d as usize];
                let neighbor = [cell[0] + step[0], cell[1] + step[1], cell[2] + step[2]];
                if !in_domain(neighbor) {
                    continue;
                }
                if let Some(frustum) = frustum {
                    let center = Vec3::new(
                        neighbor[0] as f32 * 32.0 + 16.0,
                        neighbor[1] as f32 * 32.0 + 16.0,
                        neighbor[2] as f32 * 32.0 + 16.0,
                    );
                    let sphere = Sphere { center: center.into(), radius: 16.0 * 3f32.sqrt() };
                    if !frustum.intersects_sphere(&sphere, false) {
                        continue;
                    }
                }
                let node = (neighbor, dirs | (1 << d), d ^ 1);
                if visited.insert(node) {
                    queue.push_back(node);
                }
            }
        }
        reached
    }

    #[test]
    fn state_bits_are_distinct_for_every_legal_bfs_state() {
        let mut seen = HashSet::new();
        assert!(seen.insert(state_bit(0, NO_ENTRY)));
        for dirs in 1u8..64 {
            // Legal `dirs`: never both directions of one axis.
            if (0..3).any(|axis| (dirs >> (2 * axis)) & 3 == 3) {
                continue;
            }
            for d in 0..6u8 {
                // The entry face is the opposite of a direction that was used.
                if dirs & (1 << d) == 0 {
                    continue;
                }
                let bit = state_bit(dirs, d ^ 1);
                assert_eq!(bit.count_ones(), 1);
                assert!(seen.insert(bit), "dirs={dirs:#08b} entry={} collides", d ^ 1);
            }
        }
        // 1 start + 6*1 + 12*2 + 8*3 states.
        assert_eq!(seen.len(), 55);
    }

    /// The dense flood must reach EXACTLY the cells the reference reaches: one
    /// cell fewer is a hole in the world, one more is lost culling.
    #[test]
    fn dense_flood_reaches_exactly_what_the_reference_bfs_reaches() {
        // Small deterministic generator — no `rand` dependency in this crate.
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |modulus: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % modulus
        };
        let mut scratch = CullingScratch::default();
        for round in 0..60 {
            let size = [3 + next(9) as i32, 2 + next(5) as i32, 3 + next(9) as i32];
            let origin = [next(40) as i32 - 20, next(10) as i32 - 5, next(40) as i32 - 20];
            let mut masks = std::collections::BTreeMap::new();
            let (mut lo, mut hi) = ([i32::MAX; 3], [i32::MIN; 3]);
            for x in 0..size[0] {
                for y in 0..size[1] {
                    for z in 0..size[2] {
                        // ~1/6 of the box has no chunk at all.
                        if next(6) == 0 {
                            continue;
                        }
                        let cell = [origin[0] + x, origin[1] + y, origin[2] + z];
                        for axis in 0..3 {
                            lo[axis] = lo[axis].min(cell[axis]);
                            hi[axis] = hi[axis].max(cell[axis]);
                        }
                        // Mix of solid (0), open, random masks, and mask-less chunks.
                        match next(5) {
                            0 => {
                                masks.insert(cell, 0u16);
                            }
                            1 => {
                                masks.insert(cell, ALL_CONNECTED);
                            }
                            2 => {} // chunk present but not meshed yet: no mask
                            _ => {
                                masks.insert(cell, next(0x8000) as u16);
                            }
                        }
                    }
                }
            }
            if lo[0] == i32::MAX {
                continue;
            }
            for axis in 0..3 {
                lo[axis] -= 1;
                hi[axis] += 1;
            }
            let camera_cell = [
                lo[0] + next((hi[0] - lo[0] + 1) as u64) as i32,
                lo[1] + next((hi[1] - lo[1] + 1) as u64) as i32,
                lo[2] + next((hi[2] - lo[2] + 1) as u64) as i32,
            ];
            // Every other round is frustum-gated, looking along a random axis.
            let frustum = (round % 2 == 0).then(|| {
                let eye = Vec3::new(
                    camera_cell[0] as f32 * 32.0 + 16.0,
                    camera_cell[1] as f32 * 32.0 + 16.0,
                    camera_cell[2] as f32 * 32.0 + 16.0,
                );
                let forward = [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z, Vec3::new(1.0, -0.4, 0.3)]
                    [next(5) as usize];
                let view = Transform::from_translation(eye).looking_to(forward, Vec3::Y);
                let projection = Mat4::perspective_infinite_reverse_rh(1.0, 16.0 / 9.0, 0.1);
                Frustum::from_clip_from_world(&(projection * view.to_matrix().inverse()))
            });

            let expected = reference_reached(lo, hi, camera_cell, frustum.as_ref(), &masks);
            scratch.flood(lo, hi, camera_cell, frustum.as_ref(), masks.iter().map(|(k, v)| (*k, *v)));
            for x in lo[0] - 1..=hi[0] + 1 {
                for y in lo[1] - 1..=hi[1] + 1 {
                    for z in lo[2] - 1..=hi[2] + 1 {
                        let cell = [x, y, z];
                        assert_eq!(
                            scratch.is_reached(cell),
                            expected.contains(&cell),
                            "round {round}: cell {cell:?}, camera {camera_cell:?}, gated {}",
                            frustum.is_some()
                        );
                    }
                }
            }
        }
    }

    /// The Chebyshev bound must clip the dense box exactly as `in_domain` did.
    #[test]
    fn dense_flood_respects_the_chebyshev_bound() {
        let mut masks = std::collections::BTreeMap::new();
        let far = CULL_BFS_MAX_CHEBYSHEV + 40;
        masks.insert([0, 0, 0], ALL_CONNECTED);
        masks.insert([far, 0, 0], ALL_CONNECTED);
        let (lo, hi) = ([-1, -1, -1], [far + 1, 1, 1]);
        let mut scratch = CullingScratch::default();
        scratch.flood(lo, hi, [0, 0, 0], None, masks.iter().map(|(k, v)| (*k, *v)));
        let expected = reference_reached(lo, hi, [0, 0, 0], None, &masks);
        assert!(scratch.is_reached([CULL_BFS_MAX_CHEBYSHEV, 0, 0]));
        assert!(!scratch.is_reached([CULL_BFS_MAX_CHEBYSHEV + 1, 0, 0]));
        assert!(!scratch.is_reached([far, 0, 0]));
        assert_eq!(expected.contains(&[far, 0, 0]), scratch.is_reached([far, 0, 0]));
    }

    fn spawn_camera(app: &mut App, pos: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                Camera3d::default(),
                Camera {
                    is_active: true,
                    ..Default::default()
                },
                GlobalTransform::from(Transform::from_translation(pos)),
            ))
            .id()
    }

    fn vis(app: &App, e: Entity) -> Visibility {
        *app.world()
            .entity(e)
            .get::<Visibility>()
            .expect("chunk has Visibility")
    }

    /// Camera in the (empty) cell (0,0,0), a fully solid chunk at cell
    /// (1,0,0), and a chunk with content at cell (2,0,0) behind it.
    fn wall_scene(app: &mut App) -> (Entity, Entity) {
        spawn_camera(app, Vec3::new(8.0, 8.0, 8.0));
        let wall = spawn_chunk(
            app,
            Vec3::ZERO,
            Vec3::new(32.0, 0.0, 0.0),
            vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT],
        );
        let behind = spawn_chunk(
            app,
            Vec3::ZERO,
            Vec3::new(64.0, 0.0, 0.0),
            voxels_with(&[[5, 5, 5]]),
        );
        (wall, behind)
    }

    #[test]
    fn wall_hides_what_is_behind_it() {
        // [camera cell (0,0,0), empty] [solid wall at (1,0,0)] [chunk at (2,0,0)].
        // The wall's mask is 0: the BFS enters it but cannot pass through, and
        // every around-path (over/under/beside) is cut by the no-backtrack
        // rule, so (2,0,0) ends Hidden while the wall itself stays visible.
        let mut app = pipeline_app(MeshBudgets::default());
        let (wall, behind) = wall_scene(&mut app);
        assert!(
            wait_until(&mut app, |app| vis(app, behind) == Visibility::Hidden),
            "the chunk behind a solid wall must end Hidden"
        );
        assert_eq!(
            vis(&app, wall),
            Visibility::Inherited,
            "the wall itself is entered → visible"
        );
    }

    #[test]
    fn open_air_gap_does_not_hide_distant_chunks() {
        // Chunks at (0,0,0) and (5,0,0) with NOTHING between them in the grid:
        // absent cells traverse as fully-open air, so the far chunk stays visible.
        let mut app = pipeline_app(MeshBudgets::default());
        spawn_camera(&mut app, Vec3::new(8.0, 8.0, 8.0));
        let near = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let far = spawn_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(160.0, 0.0, 0.0),
            voxels_with(&[[5, 5, 5]]),
        );
        assert!(wait_until_settled(&mut app), "pipeline must settle");
        app.update();
        assert_eq!(
            vis(&app, near),
            Visibility::Inherited,
            "camera's own cell is always visible"
        );
        assert_eq!(
            vis(&app, far),
            Visibility::Inherited,
            "grid-absent gap must not block sight"
        );
    }

    #[test]
    fn opening_a_wall_reveals_the_room_behind_it() {
        // Spec §7.4 regression: an edit must update connectivity AND visibility.
        let mut app = pipeline_app(MeshBudgets::default());
        let (wall, behind) = wall_scene(&mut app);
        assert!(
            wait_until(&mut app, |app| vis(app, behind) == Visibility::Hidden),
            "the wall must hide the room first"
        );
        // Carve a straight X tunnel through the wall at y=5, z=5.
        {
            let mut voxels = app.world_mut().get_mut::<VoxelData>(wall).unwrap();
            voxels.edit(|v| {
                for x in 0..32 {
                    v[x + 32 * 5 + 32 * 32 * 5] = Voxel::default();
                }
            });
        }
        assert!(
            wait_until(&mut app, |app| vis(app, behind) == Visibility::Inherited),
            "carving a tunnel must remesh the wall and reveal the room behind it"
        );
    }

    /// The change gate: a settled world and a still camera must not re-run the
    /// flood, and every input it depends on must wake it up again.
    #[test]
    fn flood_is_skipped_until_camera_masks_or_grid_change() {
        let mut app = pipeline_app(MeshBudgets::default());
        let (wall, behind) = wall_scene(&mut app);
        assert!(wait_until(&mut app, |app| vis(app, behind) == Visibility::Hidden));
        assert!(wait_until_settled(&mut app));
        // A few more frames so every change-detection window has passed.
        for _ in 0..3 {
            app.update();
        }
        let stats = |app: &App| *app.world().resource::<CullingStats>();

        let before = stats(&app);
        for _ in 0..5 {
            app.update();
        }
        let idle = stats(&app);
        assert_eq!(idle.runs, before.runs, "nothing changed: the flood must not re-run");
        assert_eq!(idle.skipped, before.skipped + 5);
        assert_eq!(vis(&app, behind), Visibility::Hidden, "a skipped frame keeps the result");

        // Moving WITHIN the camera's cell changes nothing the flood depends on
        // (no Frustum component in this harness); crossing into another cell does.
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<Camera3d>>()
            .single(app.world())
            .unwrap();
        app.world_mut()
            .entity_mut(camera)
            .insert(GlobalTransform::from(Transform::from_xyz(9.0, 9.0, 9.0)));
        app.update();
        assert_eq!(stats(&app).runs, idle.runs, "same cell: still skipped");
        app.world_mut()
            .entity_mut(camera)
            .insert(GlobalTransform::from(Transform::from_xyz(8.0, 40.0, 8.0)));
        app.update();
        assert_eq!(stats(&app).runs, idle.runs + 1, "new camera cell: re-run");

        // A connectivity change (edit -> remesh -> new mask) must re-run it too.
        app.world_mut()
            .entity_mut(camera)
            .insert(GlobalTransform::from(Transform::from_xyz(8.0, 8.0, 8.0)));
        assert!(wait_until(&mut app, |app| vis(app, behind) == Visibility::Hidden));
        {
            let mut voxels = app.world_mut().get_mut::<VoxelData>(wall).unwrap();
            voxels.edit(|v| {
                for x in 0..32 {
                    v[x + 32 * 5 + 32 * 32 * 5] = Voxel::default();
                }
            });
        }
        assert!(
            wait_until(&mut app, |app| vis(app, behind) == Visibility::Inherited),
            "a mask change must wake the gated flood"
        );
    }

    #[test]
    fn disabling_the_graph_restores_everything() {
        let mut app = pipeline_app(MeshBudgets::default());
        let (wall, behind) = wall_scene(&mut app);
        assert!(
            wait_until(&mut app, |app| vis(app, behind) == Visibility::Hidden),
            "the wall scene must hide the behind chunk first"
        );
        app.world_mut()
            .resource_mut::<CullingConfig>()
            .graph_enabled = false;
        app.update();
        assert_eq!(vis(&app, wall), Visibility::Inherited);
        assert_eq!(
            vis(&app, behind),
            Visibility::Inherited,
            "culling off ⇒ everything Inherited"
        );
    }

    #[test]
    fn chunks_without_a_mask_yet_stay_visible_and_traversable() {
        // In-flight budget 0: `spawn_mesh_tasks` never runs, so no chunk ever
        // gets a `ChunkConnectivity`. The BFS must treat the (actually solid)
        // wall as open and NOT hide anything behind it — under-visibility is
        // the failure mode.
        let mut app = pipeline_app(MeshBudgets {
            max_in_flight: 0,
            uploads_per_frame: 0,
        });
        spawn_camera(&mut app, Vec3::new(8.0, 8.0, 8.0));
        let wall = spawn_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(32.0, 0.0, 0.0),
            vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT],
        );
        let behind = spawn_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(64.0, 0.0, 0.0),
            voxels_with(&[[5, 5, 5]]),
        );
        for _ in 0..3 {
            app.update();
        }
        assert!(
            app.world()
                .entity(wall)
                .get::<ChunkConnectivity>()
                .is_none(),
            "premise: no mask may have landed with a zero meshing budget"
        );
        assert_eq!(
            vis(&app, wall),
            Visibility::Inherited,
            "mask-less chunk stays visible"
        );
        assert_eq!(
            vis(&app, behind),
            Visibility::Inherited,
            "mask-less chunk must traverse as open — nothing behind it may hide"
        );
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
        let mut ground = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
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

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_cameras_get_depth_prepass_and_occlusion_culling() {
        use bevy::core_pipeline::prepass::DepthPrepass;
        use bevy::render::experimental::occlusion_culling::OcclusionCulling;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_systems(Update, enable_native_occlusion);
        let cam = app.world_mut().spawn(Camera3d::default()).id();
        app.update();
        assert!(app.world().entity(cam).contains::<DepthPrepass>());
        assert!(app.world().entity(cam).contains::<OcclusionCulling>());
    }
}
