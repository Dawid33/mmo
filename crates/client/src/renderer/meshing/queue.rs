//! Remesh scheduling: world-keyed chunk grid, boundary-zone change detection,
//! and the dirty queue drained by `spawn_mesh_tasks` (task 7). All state here
//! is render-derived — never rollback-hashed.

use std::collections::{BTreeMap, BTreeSet};

use bevy::camera::primitives::{Frustum, Sphere};
use bevy::prelude::*;

use super::super::block_textures::BlockTextureLayers;
use super::super::bridge::VoxelData;
use super::binary::CutoutTable;
use super::interior::InteriorGround;
use super::neighbors::{gather_padded_rle, zone_index, zone_indices, zone_offsets};
use super::{MeshWire, MeshingTask, mesh_chunk_classified_wire};

/// World-space brick grid: key = world position / `BRICK_VOXELS`. Render-side
/// spatial index for neighbor lookups; rebuilt from entities, never persisted.
/// Its entries are render bricks (`bricks::BrickOf`), not sim chunks.
#[derive(Resource, Default)]
pub struct ChunkGrid(pub BTreeMap<[i32; 3], Entity>);

/// This chunk's grid key, stamped when first keyed.
#[derive(Component, Copy, Clone)]
pub struct ChunkKey(pub [i32; 3]);

/// crc32 fingerprint of each of the 26 boundary zones (indexed by
/// `zone_index`). A neighbor only needs remeshing when the zone it can see
/// actually changed — this is what makes edit-driven remeshes O(affected).
#[derive(Component)]
pub struct BoundaryZones(pub [u32; 26]);

/// Chunks awaiting a mesh task, with their grid key if known. BTreeMap for
/// deterministic drain order (entity order is arbitrary but stable).
#[derive(Resource, Default)]
pub struct RemeshQueue {
    pub dirty: BTreeMap<Entity, Option<[i32; 3]>>,
}

/// Floor on concurrently queued meshing tasks (the live cap scales with the
/// upload budget, see [`MeshBudgetPolicy`]). Each task owns a ~77 KB padded
/// snapshot; the cap bounds join-burst memory and lets priority reordering
/// act on everything not yet started. Tunable.
pub const MAX_IN_FLIGHT_MESH_TASKS: usize = 64;
/// Floor on finished meshes attached per frame, and the fixed value wherever
/// no [`MeshBudgetPolicy`] runs (tests). Tunable.
pub const MESH_UPLOADS_PER_FRAME: usize = 32;

#[derive(Resource)]
pub struct MeshBudgets {
    pub max_in_flight: usize,
    pub uploads_per_frame: usize,
}

impl Default for MeshBudgets {
    fn default() -> Self {
        Self {
            max_in_flight: MAX_IN_FLIGHT_MESH_TASKS,
            uploads_per_frame: MESH_UPLOADS_PER_FRAME,
        }
    }
}

/// Target rate for attaching finished meshes, in chunks per second. The
/// per-frame cap is derived from it and the real frame delta
/// ([`per_frame_budget`]): ~50 per frame at 60 Hz, more when frames are long.
///
/// Why a rate: a fixed per-frame count throttles hardest exactly when frames
/// are slow. Measured 2026-09-19/20 at the production world size, the near
/// window drained at `MESH_UPLOADS_PER_FRAME x fps` — ~260 chunks/s at 8 fps,
/// ~1600/s at 45 fps — with the in-flight task cap pinned full the whole time,
/// i.e. the workers were never the limit. Tunable.
///
/// 2026-09-23: 3000 -> 6000 once `boundary_zones` moved off the main thread
/// (the 5x5 window's ~55k near chunks drained at exactly 3000/s, 18+ s).
pub const MESH_UPLOADS_PER_SECOND: f32 = 6000.0;
/// Ceiling on the derived per-frame upload cap: twice a 60 Hz frame's worth.
/// A budget proportional to the frame delta is a feedback loop — a long frame
/// earns more work, which makes the next frame longer — so the ceiling is what
/// keeps it bounded. The first version allowed 256 (and 512 snapshot inserts)
/// and the first seconds of a join ran at 37-49 ms per frame with 31 ms
/// `mark_remesh_dirty` stalls (2026-09-20 capture).
pub const MAX_MESH_UPLOADS_PER_FRAME: usize = 100;
/// In-flight tasks allowed per upload slot: a finished task holds its slot
/// until it is attached, so the lane needs headroom beyond one frame's worth.
pub const IN_FLIGHT_PER_UPLOAD: usize = 2;

/// A per-second work rate as a per-frame count for a frame that took `dt`,
/// clamped to `min..=max`. `min` keeps high refresh rates from starving the
/// work; `max` bounds what a single long frame may take on.
pub fn per_frame_budget(per_second: f32, dt: std::time::Duration, min: usize, max: usize) -> usize {
    let scaled = (per_second * dt.as_secs_f32()).round();
    // NaN / negative / huge all land inside the clamp.
    (scaled.max(0.0) as usize).clamp(min, max.max(min))
}

/// The rate policy behind [`MeshBudgets`]. Production inserts it and runs
/// [`scale_mesh_budgets`]; harnesses that pin `MeshBudgets` by hand simply
/// don't, and keep their fixed per-frame numbers.
#[derive(Resource, Clone, Copy, Debug)]
pub struct MeshBudgetPolicy {
    pub uploads_per_second: f32,
    pub min_uploads_per_frame: usize,
    pub max_uploads_per_frame: usize,
    pub min_in_flight: usize,
}

impl Default for MeshBudgetPolicy {
    fn default() -> Self {
        Self {
            uploads_per_second: MESH_UPLOADS_PER_SECOND,
            min_uploads_per_frame: MESH_UPLOADS_PER_FRAME,
            max_uploads_per_frame: MAX_MESH_UPLOADS_PER_FRAME,
            min_in_flight: MAX_IN_FLIGHT_MESH_TASKS,
        }
    }
}

/// Re-derive this frame's [`MeshBudgets`] from the policy and the real frame
/// delta. First in the meshing chain, so every lane sees the same numbers.
pub fn scale_mesh_budgets(
    time: Res<Time<Real>>,
    policy: Res<MeshBudgetPolicy>,
    mut budgets: ResMut<MeshBudgets>,
) {
    let uploads = per_frame_budget(
        policy.uploads_per_second,
        time.delta(),
        policy.min_uploads_per_frame,
        policy.max_uploads_per_frame,
    );
    let in_flight = policy.min_in_flight.max(uploads * IN_FLIGHT_PER_UPLOAD);
    if budgets.uploads_per_frame != uploads || budgets.max_in_flight != in_flight {
        budgets.uploads_per_frame = uploads;
        budgets.max_in_flight = in_flight;
    }
}

/// Per-frame count of near mesh uploads (`Mesh3d`/quad insertions). Far raster
/// LOD uses its own equally bounded `lod::FarFrameUploads` counter, so neither
/// lane can starve the other. Reset here by `mark_remesh_dirty`, the first
/// system in the chained Update pipeline.
#[derive(Resource, Default)]
pub struct FrameUploads(pub usize);

/// Sort rank: out-of-frustum chunks after in-frustum, then by squared
/// distance to the camera. No camera (headless/startup) ranks everything 0.
fn chunk_rank(key: [i32; 3], cam: Option<(&GlobalTransform, Option<&Frustum>)>) -> u64 {
    let Some((tf, frustum)) = cam else { return 0 };
    let (edge, half) = (game::BRICK_VOXELS as f32, game::BRICK_VOXELS as f32 / 2.0);
    let center = Vec3::new(
        key[0] as f32 * edge + half,
        key[1] as f32 * edge + half,
        key[2] as f32 * edge + half,
    );
    let dist2 = tf.translation().distance_squared(center) as u64;
    let out = frustum
        .map(|f| {
            !f.intersects_sphere(
                &Sphere {
                    center: center.into(),
                    radius: half * 3f32.sqrt(),
                },
                false,
            )
        })
        .unwrap_or(false);
    ((out as u64) << 62) | dist2.min((1 << 62) - 1)
}

/// Queues at most this long are ranked afresh every frame (edits, steady
/// state): exact nearest-first, no latency. Longer ones (a region join queues
/// ~200k bricks at 512 m) use [`RankCache`]: ranking, frustum-testing and
/// sorting the whole queue every frame cost ~13 ms/frame there (2026-10-31
/// trace), throttling the join itself. Tunable.
pub const RANK_CACHE_MIN_QUEUE: usize = 4096;
/// Frames a cached nearest-first order is used before it is re-ranked against
/// the moved camera (and stale entries are purged). Bricks queued meanwhile
/// wait at most this long. Tunable.
pub const RANK_CACHE_FRAMES: u32 = 8;

/// The nearest slice of a long [`RemeshQueue`], ranked once and consumed over
/// up to [`RANK_CACHE_FRAMES`] frames.
#[derive(Default)]
pub struct RankCache {
    /// `(rank, entity)`, nearest first.
    order: Vec<(u64, Entity)>,
    next: usize,
    frames_left: u32,
}

impl RankCache {
    fn clear(&mut self) {
        self.order.clear();
        self.next = 0;
        self.frames_left = 0;
    }
}

/// Up to `capacity` queued entries, nearest first (`rank`, ties by entity).
/// Does not remove them from the queue. Entries failing `alive` are stale and
/// purged — every frame for a short queue, on each re-rank for a long one
/// (a stale entry picked in between is skipped by the caller's lookup).
/// `lane` is the whole in-flight budget: a long queue's cached slice is sized
/// from it, not from `capacity` — the slots free this instant, a handful
/// while the lane is busy, which made a slice of a few dozen that ran out
/// the next frame and re-ranked ~200k entries every frame (30 % of a thread
/// mid-join, 2026-11-19 perf capture).
fn pick_nearest(
    queue: &mut RemeshQueue,
    cache: &mut RankCache,
    capacity: usize,
    lane: usize,
    alive: impl Fn(Entity) -> bool,
    rank: impl Fn([i32; 3]) -> u64,
) -> Vec<(Entity, Option<[i32; 3]>)> {
    let rank_of = |key: Option<[i32; 3]>| key.map_or(0, &rank);
    if queue.dirty.len() <= RANK_CACHE_MIN_QUEUE {
        cache.clear();
        queue.dirty.retain(|e, _| alive(*e));
        let mut ranked: Vec<(u64, Entity, Option<[i32; 3]>)> = queue
            .dirty
            .iter()
            .map(|(&e, &key)| (rank_of(key), e, key))
            .collect();
        if ranked.len() > capacity {
            ranked.select_nth_unstable_by_key(capacity, |(r, e, _)| (*r, *e));
            ranked.truncate(capacity);
        }
        ranked.sort_unstable_by_key(|(r, e, _)| (*r, *e));
        return ranked.into_iter().map(|(_, e, key)| (e, key)).collect();
    }
    if cache.frames_left == 0 || cache.next >= cache.order.len() {
        queue.dirty.retain(|e, _| alive(*e));
        let keep = lane
            .max(capacity)
            .saturating_mul(RANK_CACHE_FRAMES as usize * 2)
            .max(1);
        let mut ranked: Vec<(u64, Entity)> = queue
            .dirty
            .iter()
            .map(|(&e, &key)| (rank_of(key), e))
            .collect();
        if ranked.len() > keep {
            ranked.select_nth_unstable(keep);
            ranked.truncate(keep);
        }
        ranked.sort_unstable();
        cache.order = ranked;
        cache.next = 0;
        cache.frames_left = RANK_CACHE_FRAMES;
    }
    cache.frames_left -= 1;
    let mut picked = Vec::with_capacity(capacity);
    while picked.len() < capacity && cache.next < cache.order.len() {
        let (_, e) = cache.order[cache.next];
        cache.next += 1;
        // Taken already, or dropped: the cache is a snapshot of the queue.
        if let Some(&key) = queue.dirty.get(&e) {
            picked.push((e, key));
        }
    }
    picked
}

/// Drain the dirty queue into meshing tasks, nearest-first, up to the
/// in-flight budget. Re-inserting `MeshingTask` supersedes (cancels) any
/// prior in-flight task for that entity — same rule as the old pipeline.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn spawn_mesh_tasks(
    mut commands: Commands,
    mut queue: ResMut<RemeshQueue>,
    budgets: Res<MeshBudgets>,
    grid: Res<ChunkGrid>,
    chunks: Query<&VoxelData>,
    in_flight: Query<(), With<MeshingTask>>,
    camera: Query<(&GlobalTransform, Option<&Frustum>), With<Camera3d>>,
    layers: Res<BlockTextureLayers>,
    render_config: Option<Res<super::super::config::TerrainRenderConfig>>,
    skipped: Option<Res<super::super::bricks::SkippedBricks>>,
    loaded: Option<Res<super::super::commander::height::HeightGrid>>,
    ground: Option<Res<super::super::commander::tiles::TileMirror>>,
    culling: Option<Res<super::visibility::CullingConfig>>,
    mut rank_cache: crate::session::SessionLocal<RankCache>,
) {
    // The culling masks are only worth their flood fill while the culler runs
    // (`requeue_for_culling` re-meshes the bricks when it is switched on).
    let connectivity = culling.is_some_and(|c| c.graph_enabled);
    let rank_cache = rank_cache.get();
    if queue.dirty.is_empty() {
        rank_cache.clear();
        return;
    }
    // Every near brick meshes to quad records: the pulled path draws them,
    // the per-chunk path merges them into its sim chunk's mesh
    // (`chunk_mesh`). Dug-space classification is for the pulled x-ray only.
    let pull = true;
    let x_ray = render_config
        .as_deref()
        .is_none_or(|config| config.near.uses_quad_records());
    let capacity = budgets
        .max_in_flight
        .saturating_sub(in_flight.iter().count());
    if capacity == 0 {
        return;
    }
    let cam = camera.iter().next();
    let picked = pick_nearest(
        &mut queue,
        rank_cache,
        capacity,
        budgets.max_in_flight,
        |e| chunks.contains(e),
        |key| chunk_rank(key, cam),
    );

    for (e, key) in picked {
        queue.dirty.remove(&e);
        let Ok(voxels) = chunks.get(e) else { continue };
        // Clone only compact RLE components while ECS access is available.
        // Expansion into the owned padded snapshot happens inside the async
        // task, keeping join/remesh preparation off the main thread.
        let center = voxels.clone();
        let mut owned_neighbors: Vec<([i32; 3], VoxelData)> = Vec::new();
        if let Some(k) = key {
            for off in zone_offsets() {
                let nk = [k[0] + off[0], k[1] + off[1], k[2] + off[2]];
                if let Some(&ne) = grid.0.get(&nk) {
                    if let Ok(nv) = chunks.get(ne) {
                        owned_neighbors.push((off, nv.clone()));
                    }
                } else if let Some((_, nv)) = skipped.as_ref().and_then(|s| s.0.get(&nk)) {
                    // A buried brick its chunk never spawned: solid, not air.
                    owned_neighbors.push((off, nv.clone()));
                } else if let Some(id) = implied_neighbor(nk, loaded.as_deref(), ground.as_deref())
                {
                    // Unmaterialised ground under the column's band: solid
                    // rock, so a dug space shows no wall where its chunk
                    // meets it (and a camera inside the rock sees through).
                    owned_neighbors.push((off, VoxelData::uniform(id)));
                }
            }
        }
        // Dug-space faces (the x-ray view): pulled bricks in the home region,
        // classified against its original surfaces. Meshed before that ground
        // arrived, the brick is re-meshed once it does.
        let mut interior = None;
        if let (true, Some(k), Some(g)) = (x_ray, key, ground.as_deref()) {
            if g.has_home_ground() {
                interior = InteriorGround::around(k, |x, z| g.home_surface(x, z));
            } else {
                commands.entity(e).insert(AwaitingGround(k));
            }
        }
        let job = NearMeshJob {
            center,
            neighbors: owned_neighbors,
            layers: layers.clone(),
            pull,
            interior,
            connectivity,
        };
        let task = super::super::compute::spawn(job, MeshWire::into_output);
        commands.entity(e).insert(MeshingTask(task));
    }
}

/// One near brick to mesh (a compute job, `renderer::compute`): its RLE
/// voxels and its neighbours' — expanded into the padded snapshot in the job.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct NearMeshJob {
    pub center: VoxelData,
    pub neighbors: Vec<([i32; 3], VoxelData)>,
    pub layers: BlockTextureLayers,
    pub pull: bool,
    pub interior: Option<InteriorGround>,
    /// Compute the cave-culling mask (`mesh_chunk_classified_wire`).
    pub connectivity: bool,
}

impl super::super::compute::ComputeJob for NearMeshJob {
    type Output = MeshWire;
    const KIND: super::super::compute::JobKind = super::super::compute::JobKind::NearMesh;

    fn run(self) -> MeshWire {
        if is_buried(&self.center, &self.neighbors, &CutoutTable::installed()) {
            return MeshWire::EMPTY;
        }
        let neighbors = self
            .neighbors
            .iter()
            .map(|(off, data)| (*off, data))
            .collect();
        let padded = gather_padded_rle(&self.center, &neighbors);
        mesh_chunk_classified_wire(
            &padded,
            &self.layers,
            self.pull,
            self.interior.as_ref(),
            self.connectivity,
        )
    }
}

/// Cave culling was switched on: every keyed brick was meshed without its
/// connectivity mask (`spawn_mesh_tasks`), so queue them all again.
pub fn requeue_for_culling(
    config: Res<super::visibility::CullingConfig>,
    grid: Res<ChunkGrid>,
    mut queue: ResMut<RemeshQueue>,
) {
    if config.is_changed() && !config.is_added() && config.graph_enabled {
        for (&key, &e) in &grid.0 {
            queue.dirty.entry(e).or_insert(Some(key));
        }
    }
}

/// On a brick meshed while the home region's ground was not mirrored yet,
/// so its dug-space faces were not classified; holds its grid key.
#[derive(Component, Clone, Copy, Debug)]
pub struct AwaitingGround(pub [i32; 3]);

/// Once the home ground arrives (the mirror's ground epoch moves and it has
/// some), re-queue the bricks meshed without it. Only those carry
/// [`AwaitingGround`] — the bricks of the frames between a join's chunks and
/// its ground — so this never walks the world.
pub fn remesh_awaiting_ground(
    mut commands: Commands,
    mirror: Option<Res<super::super::commander::tiles::TileMirror>>,
    waiting: Query<(Entity, &AwaitingGround)>,
    mut queue: ResMut<RemeshQueue>,
    mut seen: crate::session::SessionLocal<u64>,
) {
    let seen = seen.get();
    let Some(mirror) = mirror else { return };
    if mirror.ground_epoch() == *seen || !mirror.has_home_ground() {
        return;
    }
    *seen = mirror.ground_epoch();
    for (e, waiting) in &waiting {
        queue.dirty.insert(e, Some(waiting.0));
        commands.entity(e).remove::<AwaitingGround>();
    }
}

/// The block a missing neighbour brick `brick` (grid key) stands for when
/// its whole sim chunk was never materialised and lies under its column's
/// band — implied ground ([`TileMirror::implied_chunk_block`]); `None`
/// (air) otherwise, or without the commander's mirrors.
///
/// [`TileMirror::implied_chunk_block`]: super::super::commander::tiles::TileMirror::implied_chunk_block
pub fn implied_neighbor(
    brick: [i32; 3],
    loaded: Option<&super::super::commander::height::HeightGrid>,
    ground: Option<&super::super::commander::tiles::TileMirror>,
) -> Option<game::BlockId> {
    let (loaded, ground) = (loaded?, ground?);
    let chunk = implied_chunk_key(brick);
    if loaded.has_chunk(chunk) {
        return None;
    }
    ground.implied_chunk_block(chunk)
}

/// The world sim-chunk key holding grid brick `brick`.
pub fn implied_chunk_key(brick: [i32; 3]) -> [i32; 3] {
    let per = game::BRICKS_PER_CHUNK_EDGE as i32;
    brick.map(|b| b.div_euclid(per))
}

/// A chunk with no air whose 6 face neighbours all exist and present no air
/// on the shared plane: the mesher would emit no face (faces only border air)
/// and find no air to connect, so `mesh_chunk` is known without running it.
/// Cutout blocks (`cutout`, leaves) count as air: they are seen through.
/// ~30% of a join's chunks (2026-09-22 findings); skipping the padded gather,
/// mesher and connectivity flood saves ~160 µs of worker time each. Edges and
/// corners are irrelevant: they only feed AO, which needs a face. Checked in
/// the task, not the main thread: the plane scans are ~6k voxel visits.
pub fn is_buried(
    center: &VoxelData,
    neighbors: &[([i32; 3], VoxelData)],
    cutout: &CutoutTable,
) -> bool {
    is_buried_by(center, cutout, |off| {
        neighbors.iter().find(|(o, _)| *o == off).map(|(_, v)| v)
    })
}

/// [`is_buried`] over any neighbour lookup (`off` is the face offset; `None`
/// is a missing neighbour, which never buries).
pub fn is_buried_by<'a>(
    center: &VoxelData,
    cutout: &CutoutTable,
    neighbor: impl Fn([i32; 3]) -> Option<&'a VoxelData>,
) -> bool {
    if !center.is_opaque_throughout(cutout) {
        return false;
    }
    let faces = [
        [1, 0, 0],
        [-1, 0, 0],
        [0, 1, 0],
        [0, -1, 0],
        [0, 0, 1],
        [0, 0, -1],
    ];
    faces.into_iter().all(|off| {
        neighbor(off).is_some_and(|neighbor| presents_opaque_face(neighbor, off, cutout))
    })
}

/// Whether `neighbor`, the brick at face offset `off` from some brick,
/// presents no air across the face they share (its plane toward `-off`).
pub fn presents_opaque_face(neighbor: &VoxelData, off: [i32; 3], cutout: &CutoutTable) -> bool {
    if neighbor.is_opaque_throughout(cutout) {
        return true;
    }
    let mut solid = true;
    neighbor.for_each_index_sorted(zone_indices([-off[0], -off[1], -off[2]]), |_, voxel| {
        solid &= cutout.is_opaque(voxel);
    });
    solid
}

/// What `mesh_chunk` returns for a buried chunk, in either render mode.
pub fn buried_mesh_output() -> super::MeshOutput {
    super::MeshOutput {
        mesh: None,
        records: None,
        connectivity: 0,
        quad_count: 0,
        has_interior: false,
    }
}

/// Hash the 26 boundary zones of a 32³ chunk. Zones overlap on purpose: a
/// corner voxel contributes to 3 face + 3 edge + 1 corner zone.
pub fn boundary_zones(voxels: &VoxelData) -> [u32; 26] {
    voxels
        .cached_boundary_zones()
        .unwrap_or_else(|| compute_boundary_zones(voxels))
}

/// The uncached hashing behind `boundary_zones`.
pub fn compute_boundary_zones(voxels: &VoxelData) -> [u32; 26] {
    let mut zones = [0u32; 26];
    for (i, off) in zone_offsets().enumerate() {
        let mut h = crc32fast::Hasher::new();
        voxels.for_each_index_sorted(zone_indices(off), |_, voxel| {
            h.update(&voxel.block.0.to_le_bytes());
        });
        zones[i] = h.finalize();
        debug_assert_eq!(zone_index(off), i);
    }
    zones
}

/// Key changed chunks into the grid, mark them dirty, and dirty any neighbor
/// whose boundary zone changed. Runs in `Update` before `spawn_mesh_tasks`.
pub fn mark_remesh_dirty(
    mut commands: Commands,
    mut grid: ResMut<ChunkGrid>,
    mut queue: ResMut<RemeshQueue>,
    mut uploads: ResMut<FrameUploads>,
    mut changed: Query<
        (
            Entity,
            &VoxelData,
            Option<&Transform>,
            Option<&ChildOf>,
            Option<&ChunkKey>,
            Option<&mut BoundaryZones>,
        ),
        Changed<VoxelData>,
    >,
    roots: Query<&Transform, Without<VoxelData>>,
) {
    uploads.0 = 0;
    // Unbudgeted: every chunk changed this frame is fingerprinted here.
    let mut stall = game::perf::StallGuard::new("mark_remesh_dirty");
    for (e, voxels, tf, parent, key, mut zones) in &mut changed {
        stall.add_work(1);
        // Resolve (or compute) the grid key. Bare test entities (no parented
        // transform) stay unkeyed and skip all neighbor logic.
        let key: Option<[i32; 3]> = key.map(|k| k.0).or_else(|| {
            let (tf, parent) = (tf?, parent?);
            let root = roots.get(parent.parent()).ok()?;
            let world = (root.translation + tf.translation) / game::BRICK_VOXELS as f32;
            let k = [
                world.x.round() as i32,
                world.y.round() as i32,
                world.z.round() as i32,
            ];
            commands.entity(e).insert(ChunkKey(k));
            Some(k)
        });
        // Idempotent: also re-enters an entity that a keyed `ChunkKey` still
        // names but that `prune_chunk_grid` evicted from the grid on an
        // earlier, since-reversed VoxelData removal (cross-frame remove then
        // reinsert) — otherwise it stays treated as air by neighbors forever.
        if let Some(k) = key {
            grid.0.insert(k, e);
        }
        queue.dirty.insert(e, key);

        let new_zones = boundary_zones(voxels);
        if let Some(k) = key {
            let old = zones.as_ref().map(|z| z.0);
            for (i, off) in zone_offsets().enumerate() {
                if old.is_some_and(|o| o[i] == new_zones[i]) {
                    continue;
                }
                let nk = [k[0] + off[0], k[1] + off[1], k[2] + off[2]];
                if let Some(&ne) = grid.0.get(&nk) {
                    queue.dirty.entry(ne).or_insert(Some(nk));
                }
            }
        }
        match zones.as_deref_mut() {
            Some(z) => z.0 = new_zones,
            None => {
                commands.entity(e).insert(BoundaryZones(new_zones));
            }
        }
    }
}

/// Drop grid + queue entries for chunks whose `VoxelData` is gone for good
/// (same liveness rule as `apply_meshed_chunks`: presence wins over the
/// frame's removal log, so same-frame remove+reinsert survives), and
/// re-dirty the 26 grid neighbors of each removed key: a neighbor that
/// culled its shared-plane faces against the now-dead chunk must remesh
/// with an air shell there, or the culled faces become visible holes.
pub fn prune_chunk_grid(
    mut grid: ResMut<ChunkGrid>,
    mut queue: ResMut<RemeshQueue>,
    mut removed: RemovedComponents<VoxelData>,
    still_alive: Query<(), With<VoxelData>>,
) {
    let dead: BTreeSet<Entity> = removed
        .read()
        .filter(|e| !still_alive.contains(*e))
        .collect();
    if dead.is_empty() {
        return;
    }
    let mut removed_keys = Vec::new();
    grid.0.retain(|k, e| {
        if dead.contains(e) {
            removed_keys.push(*k);
            false
        } else {
            true
        }
    });
    for e in &dead {
        queue.dirty.remove(e);
    }
    for k in removed_keys {
        for off in zone_offsets() {
            let nk = [k[0] + off[0], k[1] + off[1], k[2] + off[2]];
            if let Some(&ne) = grid.0.get(&nk) {
                queue.dirty.entry(ne).or_insert(Some(nk));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use game::{BRICK_VOXEL_COUNT, BlockId, Voxel};

    pub(crate) fn voxels_with(cells: &[[usize; 3]]) -> Vec<Voxel> {
        let mut v = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        for [x, y, z] in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(1));
        }
        v
    }

    /// Region-root + chunk child at region-local chunk coords, like the bridge builds.
    pub(crate) fn spawn_chunk(
        app: &mut App,
        root_at: Vec3,
        local: Vec3,
        voxels: Vec<Voxel>,
    ) -> Entity {
        let root = app
            .world_mut()
            .spawn((Transform::from_translation(root_at), Visibility::default()))
            .id();
        app.world_mut()
            .spawn((
                Transform::from_translation(local),
                Visibility::default(),
                VoxelData::new(voxels),
                ChildOf(root),
            ))
            .id()
    }

    use super::super::super::block_textures::BlockTextureLayers;

    use super::super::{MeshingTask, apply_meshed_chunks};

    /// Full pipeline as production registers it in `renderer/mod.rs`,
    /// including the LOD mesh path chained after `apply_meshed_chunks` and
    /// the cave-culling pass via `cull_plugin` (PostUpdate).
    pub(crate) fn pipeline_app(budgets: MeshBudgets) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<ChunkGrid>()
            .init_resource::<RemeshQueue>()
            .init_resource::<FrameUploads>()
            .init_resource::<super::super::lod::LodBuildQueue>()
            .init_resource::<super::super::lod::FarFrameUploads>()
            .init_resource::<BlockTextureLayers>()
            .init_resource::<super::super::visibility::CullingConfig>()
            // These pipeline tests introspect `Mesh3d` / `ATTRIBUTE_VOXEL_PACKED`,
            // so pin the legacy per-chunk backend.
            .insert_resource(super::super::super::config::TerrainRenderConfig {
                near: super::super::super::config::NearRenderBackend::PerChunk,
            })
            .insert_resource(budgets)
            .add_systems(
                Update,
                (
                    mark_remesh_dirty,
                    prune_chunk_grid,
                    spawn_mesh_tasks,
                    apply_meshed_chunks,
                    super::super::lod::spawn_lod_mesh_tasks,
                    super::super::lod::apply_lod_meshes,
                )
                    .chain(),
            )
            .add_plugins(super::super::visibility::cull_plugin);
        app
    }

    /// Deadline-based wait: fixed-iteration polling flaked under full-suite
    /// load (the slow soak tests starve the shared AsyncComputeTaskPool, so a
    /// ~500ms budget failed runs that pass instantly in isolation). Returns
    /// true iff `cond` became true before the deadline.
    pub(crate) fn wait_until(app: &mut App, mut cond: impl FnMut(&mut App) -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            app.update();
            if cond(app) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        false
    }

    /// Waits until the remesh pipeline is fully settled: dirty queue drained
    /// and no `MeshingTask` in flight.
    pub(crate) fn wait_until_settled(app: &mut App) -> bool {
        wait_until(app, |app| {
            let queue_empty = app.world().resource::<RemeshQueue>().dirty.is_empty();
            let none_in_flight = app
                .world_mut()
                .query::<&MeshingTask>()
                .iter(app.world())
                .next()
                .is_none();
            queue_empty && none_in_flight
        })
    }
}
