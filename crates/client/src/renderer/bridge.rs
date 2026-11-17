use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, Task};
use crossbeam::channel::Receiver;
use game::{ClientUpdateEvent, GameData, GameDataUpdate, GameDataUpdateKind, RegionId};
use game::{EntityKey, Voxel};
use super::history_view::HistoryMsg;

use super::convert::{iso_to_transform, perspective_to_projection};
use super::commander::tiles::TileMirrorMsg;
use super::hud::HudStatus;
use super::lod_rings;
use super::{ClientUpdates, LocalPlayer};
use crate::session::SessionScoped;

#[derive(Resource, Default)]
pub struct Regions(pub BTreeMap<RegionId, Receiver<GameDataUpdate>>);

/// Each region's terrain root: the parent of its chunks, their bricks and
/// its other static render entities, at the region's fixed world offset. Its
/// moving entities (players, slimes, voxel bodies, …) hang off a separate
/// BODIES root at the same offset ([`RegionRoots::bodies`]): Bevy's static
/// transform optimisation re-walks a whole tree when any node in it changes
/// (a dirty root's `GlobalTransform` is rewritten, which flags every direct
/// child), so one hopping slime under the terrain root cost a ~345k-brick
/// walk every frame — ~15 ms/frame at a 512 m region (2026-10-31 trace).
/// The bodies root is despawned with its region root ([`BodiesOf`]).
#[derive(Resource, Default)]
pub struct RegionRoots(pub BTreeMap<RegionId, Entity>, BodyRoots);

/// The bodies roots behind [`RegionRoots::bodies`].
#[derive(Default)]
pub struct BodyRoots(BTreeMap<RegionId, Entity>);

impl RegionRoots {
    /// The parent of `region`'s non-terrain entities: its bodies root, or
    /// the region root itself for a root mapped without one (tests that
    /// insert into `.0` directly).
    pub fn bodies(&self, region: RegionId) -> Option<Entity> {
        self.1 .0.get(&region).or_else(|| self.0.get(&region)).copied()
    }

    fn insert(&mut self, region: RegionId, root: Entity, bodies: Entity) {
        self.0.insert(region, root);
        self.1 .0.insert(region, bodies);
    }

    fn remove(&mut self, region: &RegionId) -> Option<Entity> {
        self.1 .0.remove(region);
        self.0.remove(region)
    }
}

/// On a region's bodies root: the terrain root it belongs to. Despawning the
/// region root despawns its bodies root and everything under it.
#[derive(Component)]
#[relationship(relationship_target = RegionBodies)]
pub struct BodiesOf(pub Entity);

/// On a region root: its bodies root (see [`BodiesOf`]).
#[derive(Component, Default)]
#[relationship_target(relationship = BodiesOf, linked_spawn)]
pub struct RegionBodies(Vec<Entity>);

/// The region root and bodies root of a newly received region, both at its
/// fixed world offset (they never move).
fn spawn_region_roots(commands: &mut Commands, id: RegionId) -> (Entity, Entity) {
    let offset = id.world_offset();
    let at = Transform::from_translation(Vec3::new(offset[0], offset[1], offset[2]));
    let root = commands
        .spawn((at, Visibility::default(), Name::new(format!("region {:?}", id)), SessionScoped))
        .id();
    // Scoped too, though it goes with its region root: nothing of a
    // session may outlive it, whatever the relationship does.
    let bodies = commands
        .spawn((
            at,
            Visibility::default(),
            Name::new(format!("region {:?} bodies", id)),
            BodiesOf(root),
            SessionScoped,
        ))
        .id();
    (root, bodies)
}

/// Region roots released from the window but not yet despawned, oldest first.
/// A window shift releases a whole row at once — five regions of ~20k chunk
/// entities at the 5x5 window — and despawning all of them in the crossing
/// frame cost up to 372 ms of command application (2026-09-23 `fly-probe`
/// trace). [`despawn_retired_regions`] takes [`REGION_DESPAWNS_PER_FRAME`] a
/// frame instead. A retired root is already out of every map, so nothing
/// reads or updates it; it just stays drawn for those few frames.
#[derive(Resource, Default)]
pub struct RetiredRegionRoots(pub std::collections::VecDeque<(RegionId, Entity)>);

/// Region trees despawned per frame by [`despawn_retired_regions`].
pub const REGION_DESPAWNS_PER_FRAME: usize = 1;

pub fn despawn_retired_regions(mut commands: Commands, mut retired: ResMut<RetiredRegionRoots>) {
    for _ in 0..REGION_DESPAWNS_PER_FRAME {
        let Some((_, root)) = retired.0.pop_front() else { break };
        // despawn() removes descendants via ChildOf relationships.
        commands.entity(root).despawn();
    }
}

#[derive(Resource, Default)]
pub struct SimEntityMap(pub BTreeMap<(RegionId, EntityKey), Entity>);

// identity breadcrumbs for debugging; not yet read
#[allow(dead_code)]
#[derive(Component)]
pub struct SimEntity {
    pub region: RegionId,
    pub key: EntityKey,
}

/// Where the sim says this entity should be. `Transform` chases it (task 5).
/// Written by the bridge on every Do *and* Undo event: target writes are
/// last-write-wins, so a rollback/reapply burst drained in one frame
/// collapses to the final pose. That idempotence is the invariant that
/// makes the one-way bridge safe — do not switch to incremental deltas.
/// Per-frame lerp factor for the camera's *position*. Kept < 1.0 so the
/// eye glides between 50Hz sim poses instead of stepping. Rotation is a
/// separate factor (see `camera`) held at 1.0 so the look stays instant.
pub const CAMERA_POS_SMOOTHING: f32 = 0.3;

#[derive(Component)]
pub struct SimTarget {
    pub pos: Vec3,
    pub rot: Quat,
    pub pos_smoothing: f32,
    pub rot_smoothing: f32,
    pub pos_snap: f32,
    pub rot_snap: f32,
    /// Added to `pos` to get where the `Transform` settles. Zero for bodies;
    /// the eye height for the local player's camera, whose sim pose is the
    /// capsule centre.
    pub offset: Vec3,
}

impl SimTarget {
    // pos_snap values are world-unit distances (1 unit = 1/16 m); smoothing
    // factors and rotation snaps are dimensionless/radians.
    pub fn body(pos: Vec3, rot: Quat) -> Self {
        Self {
            pos,
            rot,
            pos_smoothing: 0.5,
            rot_smoothing: 0.5,
            pos_snap: 1.6,
            rot_snap: 0.1,
            offset: Vec3::ZERO,
        }
    }
    pub fn camera(pos: Vec3, rot: Quat) -> Self {
        // The render camera SNAPS its rotation (rot_smoothing = 1.0) so
        // there's no slerp-arc tilt during turns, but LERPS its position so
        // movement glides instead of stepping. The snapped per-tick sim look
        // is then overwritten by the per-frame local look (`look.rs`).
        Self {
            pos,
            rot,
            pos_smoothing: CAMERA_POS_SMOOTHING,
            rot_smoothing: 1.0,
            pos_snap: 0.008,
            rot_snap: 0.001,
            offset: EYE_OFFSET,
        }
    }

    /// Where the `Transform` converges: the sim pose plus `offset`.
    pub fn goal(&self) -> Vec3 {
        self.pos + self.offset
    }
}

/// The first-person eye sits this far above the player's body origin.
pub const EYE_OFFSET: Vec3 = Vec3::new(0.0, game::movement::PLAYER_EYE_OFFSET, 0.0);

/// A chunk's 32³ voxel grid, run-length encoded along the linear index
/// (`x + 32*y + 32*32*z`, so runs extend along +X).
///
/// The dense form is 64 KiB (32768 voxels × 2 B) and the client used to keep one
/// alive per chunk for the whole session — at ~2800 chunks per region that was
/// ~183 MB per region, three quarters of the client's heap, and the largest
/// single consumer by an order of magnitude.
///
/// Runs are cheap here by construction: a chunk is 8 blocks of 16³, each uniform
/// unless chiselled, so every run is ≥16 voxels and a plain air/stone chunk is
/// one run. Nothing needs the dense array persistently — the mesher expands it
/// transiently to build `PaddedVoxels`, then drops it — so we store the runs and
/// hand out a dense `Vec` on demand.
///
/// The second field caches the chunk's 26 boundary-zone hashes
/// (`meshing::queue::boundary_zones`) when they were computed off the main
/// thread (`with_boundary_zones`, in the snapshot-voxel task); `new` — and so
/// `edit` — start without it. Equality ignores it.
#[derive(Component, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VoxelData(Vec<(u16, game::BlockId)>, Option<[u32; 26]>);

impl PartialEq for VoxelData {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for VoxelData {}

impl VoxelData {
    /// Encode a dense 32³ grid.
    pub fn new(voxels: impl AsRef<[Voxel]>) -> Self {
        let voxels = voxels.as_ref();
        debug_assert_eq!(voxels.len(), game::BRICK_VOXEL_COUNT);
        let mut runs: Vec<(u16, game::BlockId)> = Vec::new();
        for v in voxels {
            match runs.last_mut() {
                // A run can't overflow u16: the whole chunk is 32768 voxels.
                Some((len, id)) if *id == v.block => *len += 1,
                _ => runs.push((1, v.block)),
            }
        }
        Self(runs, None)
    }

    /// One run of `id` over the whole brick.
    pub fn uniform(id: game::BlockId) -> Self {
        Self(vec![(game::BRICK_VOXEL_COUNT as u16, id)], None)
    }

    /// Encode a brick straight from its blocks — identical to
    /// `VoxelData::new(view.voxels())` without building the dense 32³ array.
    /// Each `BRICK_VOXELS` x-row is `BRICK_BLOCKS` 16-voxel segments, one per
    /// block: an unchiselled block adds one 16-run, a chiselled one its row of
    /// materials. Derivation was ~65 µs/chunk of the join's worker time. A
    /// uniform brick is one run with no per-voxel work. Runs stay `u16`: a
    /// brick is 32768 voxels.
    pub fn from_brick(view: &game::BrickView) -> Self {
        use game::{BLOCK_VOXELS, BRICK_BLOCKS, BRICK_VOXELS};
        if let Some(block) = view.uniform() {
            return Self::uniform(block.id);
        }
        fn push(runs: &mut Vec<(u16, game::BlockId)>, len: u16, id: game::BlockId) {
            match runs.last_mut() {
                Some((run, last)) if *last == id => *run += len,
                _ => runs.push((len, id)),
            }
        }
        // Decoded once per chiselled block of the brick.
        let mut materials: std::collections::BTreeMap<[usize; 3], Vec<game::BlockId>> = Default::default();
        for bz in 0..BRICK_BLOCKS {
            for by in 0..BRICK_BLOCKS {
                for bx in 0..BRICK_BLOCKS {
                    if let Some(c) = view.chisel(bx, by, bz) {
                        materials.insert([bx, by, bz], c.materials());
                    }
                }
            }
        }
        let edge = BRICK_VOXELS;
        let mut runs: Vec<(u16, game::BlockId)> = Vec::new();
        for z in 0..edge {
            for y in 0..edge {
                let (by, bz) = (y / BLOCK_VOXELS, z / BLOCK_VOXELS);
                for bx in 0..BRICK_BLOCKS {
                    match materials.get(&[bx, by, bz]) {
                        None => push(&mut runs, BLOCK_VOXELS as u16, view.block_at(bx, by, bz).id),
                        Some(m) => {
                            let (ly, lz) = (y % BLOCK_VOXELS, z % BLOCK_VOXELS);
                            let row = (ly + lz * BLOCK_VOXELS) * BLOCK_VOXELS;
                            for id in &m[row..row + BLOCK_VOXELS] {
                                push(&mut runs, 1, *id);
                            }
                        }
                    }
                }
            }
        }
        Self(runs, None)
    }

    /// Compute and cache the boundary-zone hashes now — for a caller already
    /// off the main thread, so `mark_remesh_dirty` needn't (~2 s of main-thread
    /// hashing per `full` join, 2026-09-22 findings).
    pub fn with_boundary_zones(mut self) -> Self {
        self.1 = Some(super::meshing::queue::compute_boundary_zones(&self));
        self
    }

    /// Every voxel is air.
    pub fn is_all_air(&self) -> bool {
        self.0.iter().all(|&(_, id)| id == game::BlockId::AIR)
    }

    /// Every voxel is air or liquid (`cutout`'s liquids): nothing the chunk
    /// mesher draws.
    pub fn is_all_empty(&self, cutout: &super::meshing::binary::CutoutTable) -> bool {
        self.0.iter().all(|&(_, id)| id == game::BlockId::AIR || cutout.is_liquid(id))
    }

    /// Every voxel hides what is behind it: no air, no liquid and no cutout
    /// block (`cutout`) anywhere in the chunk.
    pub fn is_opaque_throughout(&self, cutout: &super::meshing::binary::CutoutTable) -> bool {
        self.0
            .iter()
            .all(|&(_, id)| id != game::BlockId::AIR && !cutout.is_liquid(id) && !cutout.is_cutout(id))
    }

    /// The cached boundary-zone hashes, if `with_boundary_zones` ran.
    pub fn cached_boundary_zones(&self) -> Option<[u32; 26]> {
        self.1
    }

    /// Expand back to the dense 32³ grid the mesher consumes.
    pub fn decode(&self) -> Vec<Voxel> {
        let mut out = Vec::with_capacity(game::BRICK_VOXEL_COUNT);
        for &(len, id) in &self.0 {
            out.extend(std::iter::repeat_n(Voxel::new(id), len as usize));
        }
        out
    }

    /// Visit monotonically increasing dense-grid indices without expanding the
    /// RLE component. The meshing snapshot builder uses this for interiors and
    /// neighbor boundary zones, where requested indices naturally arrive in
    /// linear x-major order.
    pub(crate) fn for_each_index_sorted(
        &self,
        indices: impl IntoIterator<Item = usize>,
        mut visit: impl FnMut(usize, Voxel),
    ) {
        let mut runs = self.0.iter();
        let Some(&(mut run_len, mut block)) = runs.next() else {
            return;
        };
        let mut run_end = run_len as usize;
        let mut previous = None;
        for index in indices {
            debug_assert!(previous.is_none_or(|p| index >= p), "RLE indices must be sorted");
            debug_assert!(index < game::BRICK_VOXEL_COUNT, "voxel index out of bounds");
            previous = Some(index);
            while index >= run_end {
                let &(next_len, next_block) = runs.next().expect("RLE covers the full chunk");
                run_len = next_len;
                block = next_block;
                run_end += run_len as usize;
            }
            visit(index, Voxel::new(block));
        }
    }

    /// Decode, mutate, re-encode. The edit path is rare (a chisel or a block
    /// break); the read path is what had to stay cheap.
    pub fn edit(&mut self, f: impl FnOnce(&mut Vec<Voxel>)) {
        let mut dense = self.decode();
        f(&mut dense);
        *self = Self::new(dense);
    }

    /// Number of runs — the thing that actually costs memory.
    pub fn runs(&self) -> usize {
        self.0.len()
    }
}

/// Marker: the local player's sim body. In commander mode it is hidden and
/// gets no camera (the commander has no body to look through or show).
#[derive(Component)]
pub struct LocalBody;

/// True when the game renders from the commander camera, not the body's.
fn commander(mode: Option<&super::commander::CameraMode>) -> bool {
    mode.is_some_and(|m| *m == super::commander::CameraMode::Commander)
}

/// Mirror of the sim's `EntityKind` for this entity. Consumed by the avatar
/// system to attach a renderable mesh.
#[derive(Component, Clone, Copy)]
pub struct SimKind(pub game::EntityKind);

/// Marks a bevy entity as an entity-halo mirror (`game::halo`) of the owned
/// entity `id` in region `source`. Used only for render dedupe
/// ([`dedupe_halo_entities`]): one copy per id is drawn.
#[derive(Component, Clone, Copy)]
pub struct HaloMirror {
    pub id: game::EntityId,
    pub source: RegionId,
}

/// Bricks derived per snapshot-voxel task. A region is ~2800 bricks at
/// ~0.3 ms each, so a batch is ~20 ms of worker time: small enough that the
/// first terrain shows up quickly and (on wasm, where tasks run on the main
/// thread) that no single task is a long hitch.
pub const SNAPSHOT_VOXEL_TASK_BRICKS: usize = 64;

/// Sim chunks per snapshot-voxel task: [`SNAPSHOT_VOXEL_TASK_BRICKS`] worth
/// of bricks, at least one chunk.
pub const SNAPSHOT_VOXEL_TASK_CHUNKS: usize = {
    let per_chunk = game::BRICKS_PER_CHUNK_EDGE.pow(3);
    if SNAPSHOT_VOXEL_TASK_BRICKS > per_chunk { SNAPSHOT_VOXEL_TASK_BRICKS / per_chunk } else { 1 }
};

/// Floor on derived snapshot bricks spawned per frame (the live count follows
/// [`SNAPSHOT_VOXELS_PER_SECOND`]; a chunk's bricks land together, so one
/// chunk always fits). Each brick makes `mark_remesh_dirty` fingerprint it
/// (~0.15 ms on a power-clamped machine, far less otherwise), so this is the
/// main-thread cost of a joining region.
pub const SNAPSHOT_VOXELS_PER_FRAME: usize = 64;
/// Target brick insert rate. Kept above the mesher's `MESH_UPLOADS_PER_SECOND` so
/// voxels always arrive faster than they can be meshed: the remesh queue then
/// holds everything pending and its nearest-first ranking decides the order.
pub const SNAPSHOT_VOXELS_PER_SECOND: f32 = 9600.0;
/// Ceiling per frame: twice a 60 Hz frame's worth, so a long frame cannot
/// snowball (each insert costs `mark_remesh_dirty` ~45 us unclamped; 512 per
/// frame measured 31 ms stalls). See `MAX_MESH_UPLOADS_PER_FRAME`.
pub const MAX_SNAPSHOT_VOXELS_PER_FRAME: usize = 160;

/// Snapshot-voxel tasks allowed on the compute pool at once. The pool is FIFO
/// and shared with the mesher: when wire compression made all nine regions of
/// a join arrive within a second, their ~365 batches were spawned together and
/// the first mesh tasks queued behind them (first terrain 0.9 s -> 1.9 s).
/// Twice the default worker count keeps the workers fed without burying
/// everything else; the rest wait in `SnapshotVoxelQueue`, nearest first.
pub const MAX_OUTSTANDING_SNAPSHOT_VOXEL_TASKS: usize = 8;

/// A chunk entity spawned from a region snapshot whose bricks are still
/// being derived off-thread. Removed when the bricks land — or when an
/// incremental `SetVoxelComponent` gets there first, which then wins: the
/// late snapshot result is older and must not overwrite it.
#[derive(Component)]
pub struct PendingSnapshotVoxels;

/// Block edits (`SetBlocks`) that reached a chunk while its bricks were still
/// being derived off-thread. Each delta is a block's full new state, so they
/// are applied in order on top of whatever bricks land.
#[derive(Component, Default)]
pub struct PendingBlockDeltas(pub(crate) Vec<game::BlockDelta>);

/// One batch of snapshot-voxel derivation. Child of the region root, so
/// removing (or replacing) the region despawns — and thereby cancels — it.
#[derive(Component)]
pub struct SnapshotVoxelTask(Task<Vec<DerivedChunk>>);

/// A batch of chunks whose render bricks to derive (a compute job,
/// `renderer::compute`); answers each chunk's bricks in order (`None`: the
/// chunk is gone from the snapshot).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct DeriveBricksJob {
    /// Threads only: the whole snapshot, shared — the chunks at `items` are
    /// read from it and `source` is ignored. Never serialized (a skipped
    /// FIELD: serde's skipped enum *variants* renumber the rest on decode).
    #[serde(skip)]
    pub shared: Option<Arc<GameData>>,
    /// The snapshot chunks to derive (unused for `DeriveSource::Owned`).
    pub items: Vec<game::ChunkCoords>,
    pub source: DeriveSource,
}

/// What a [`DeriveBricksJob`] derives from when it does not share a snapshot.
#[derive(serde::Serialize, serde::Deserialize)]
pub enum DeriveSource {
    /// The part of the snapshot the `items` read (a worker).
    Part(super::bricks::SnapshotPart),
    /// Chunks that came on their own (`AddTerrainChunk`): nothing across.
    Owned(Vec<game::Chunk>),
}

impl DeriveBricksJob {
    /// Snapshot chunks at `items`, the snapshot shared (threads).
    pub fn shared(data: Arc<GameData>, items: Vec<game::ChunkCoords>) -> Self {
        Self { shared: Some(data), items, source: DeriveSource::Part(Default::default()) }
    }

    /// Snapshot chunks at `items` with just the part of the snapshot they read.
    pub fn part(part: super::bricks::SnapshotPart, items: Vec<game::ChunkCoords>) -> Self {
        Self { shared: None, items, source: DeriveSource::Part(part) }
    }

    /// Chunks that came on their own.
    pub fn owned(chunks: Vec<game::Chunk>) -> Self {
        Self { shared: None, items: Vec::new(), source: DeriveSource::Owned(chunks) }
    }
}

impl super::compute::ComputeJob for DeriveBricksJob {
    type Output = Vec<Option<super::bricks::DerivedBricks>>;
    const KIND: super::compute::JobKind = super::compute::JobKind::DeriveBricks;

    fn run(self) -> Self::Output {
        use super::bricks::{derive_chunk_bricks, derive_chunk_bricks_across, snapshot_across, DerivedBricks};
        let zones = |mut derived: DerivedBricks| {
            derived.spawn = derived.spawn.into_iter().map(|(l, v)| (l, v.with_boundary_zones())).collect();
            derived
        };
        let cutout = super::meshing::binary::CutoutTable::installed();
        if let Some(data) = self.shared {
            return self
                .items
                .into_iter()
                .map(|coords| {
                    let key = data.chunk_index.get(&game::chunk_index_key(coords))?;
                    let chunk = data.ecs.chunk.try_get(*key).as_ref()?;
                    Some(zones(derive_chunk_bricks_across(chunk, snapshot_across(&data, coords, &cutout))))
                })
                .collect();
        }
        match self.source {
            DeriveSource::Part(part) => self
                .items
                .into_iter()
                .map(|coords| {
                    let chunk = part.chunks.get(&coords)?.as_ref()?;
                    Some(zones(derive_chunk_bricks_across(chunk, part.across(coords, &cutout))))
                })
                .collect(),
            DeriveSource::Owned(chunks) => {
                chunks.iter().map(|chunk| Some(zones(derive_chunk_bricks(chunk)))).collect()
            }
        }
    }
}

/// A chunk entity, the blocks its bricks were derived from, and the bricks.
type DerivedChunk = (Entity, game::Chunk, super::bricks::DerivedBricks);

/// Derived voxels waiting for their per-frame insert budget.
#[derive(Resource, Default)]
pub struct SnapshotVoxelQueue {
    /// Batches not yet handed to the compute pool (see
    /// [`MAX_OUTSTANDING_SNAPSHOT_VOXEL_TASKS`]).
    batches: VecDeque<PendingVoxelBatch>,
    ready: VecDeque<DerivedChunk>,
    /// Bricks inserted since the queue was last empty (for the `[perf]` line).
    applied: u32,
}

/// One batch of a region snapshot's chunks, waiting for a compute-pool slot.
struct PendingVoxelBatch {
    region: RegionId,
    /// The region root the chunks were spawned under; a replaced or removed
    /// region has a different (or no) root, which retires the batch.
    root: Entity,
    /// World-space centroid of the batch's chunks, for nearest-first dispatch.
    center: Vec3,
    chunks: BatchChunks,
}

/// Where a voxel batch's chunks come from.
enum BatchChunks {
    /// A region snapshot, shared by its batches (no per-chunk clone), with
    /// each chunk's coords: the snapshot also buries face bricks against the
    /// chunks next to them (`bricks::snapshot_across`).
    Snapshot(Arc<GameData>, Vec<(Entity, EntityKey, game::ChunkCoords)>),
    /// Chunks of a cell activated after the snapshot (`AddTerrainChunk`).
    Owned(Vec<(Entity, game::Chunk)>),
}

/// Index of the batch to dispatch next: the one nearest the camera, or simply
/// the oldest while no camera exists yet (the local camera itself arrives in
/// the home region's snapshot).
fn nearest_batch(centers: impl Iterator<Item = Vec3>, camera: Option<Vec3>) -> Option<usize> {
    let mut centers = centers.enumerate();
    let Some(camera) = camera else {
        return centers.next().map(|(index, _)| index);
    };
    centers
        .min_by(|(_, a), (_, b)| a.distance_squared(camera).total_cmp(&b.distance_squared(camera)))
        .map(|(index, _)| index)
}

/// Collect finished snapshot-voxel batches and insert up to
/// [`SNAPSHOT_VOXELS_PER_FRAME`] of them. Spawning a region used to derive and
/// RLE-encode every chunk inside `drain_client_updates`: one 0.5-2 s frame per
/// region, 8.9 s for the production 3x3 join (2026-09-19 capture).
pub fn apply_snapshot_voxels(
    mut commands: Commands,
    time: Res<Time<Real>>,
    mut tasks: Query<(Entity, &mut SnapshotVoxelTask)>,
    mut brick_edits: Query<(Entity, &mut super::bricks::BrickEdits)>,
    pending: Query<(), With<PendingSnapshotVoxels>>,
    cameras: Query<(&GlobalTransform, &Camera), With<Camera3d>>,
    roots: Res<RegionRoots>,
    mut queue: ResMut<SnapshotVoxelQueue>,
) {
    super::bricks::poll_brick_edits(&mut commands, &mut brick_edits);
    let mut outstanding = 0usize;
    for (task_entity, mut task) in &mut tasks {
        if let Some(batch) = block_on(future::poll_once(&mut task.0)) {
            queue.ready.extend(batch);
            commands.entity(task_entity).despawn();
        } else {
            outstanding += 1;
        }
    }

    // Retire batches of regions that were removed or replaced since (their
    // entities are gone; holding the `Arc` would keep ~40 MiB alive each).
    if roots.is_changed() {
        queue.batches.retain(|batch| roots.0.get(&batch.region) == Some(&batch.root));
    }
    // Hand out compute-pool slots, nearest batch to the camera first.
    let camera = cameras
        .iter()
        .find(|(_, camera)| camera.is_active)
        .map(|(transform, _)| transform.translation());
    while outstanding < MAX_OUTSTANDING_SNAPSHOT_VOXEL_TASKS {
        let Some(index) = nearest_batch(queue.batches.iter().map(|batch| batch.center), camera) else {
            break;
        };
        let Some(PendingVoxelBatch { root, chunks, .. }) = queue.batches.remove(index) else {
            break;
        };
        let task = match chunks {
            BatchChunks::Snapshot(data, items) => {
                let coords: Vec<_> = items.iter().map(|&(_, _, coords)| coords).collect();
                // Off the page (a wasm worker) the job carries just what it
                // reads; on a thread it shares the snapshot.
                let job = if super::compute::serializes() {
                    DeriveBricksJob::part(super::bricks::SnapshotPart::gather(&data, coords.iter().copied()), coords)
                } else {
                    DeriveBricksJob::shared(data.clone(), coords)
                };
                super::compute::spawn(job, move |derived: Vec<Option<super::bricks::DerivedBricks>>| {
                    items
                        .into_iter()
                        .zip(derived)
                        .filter_map(|((entity, key, _), derived)| {
                            let chunk = data.ecs.chunk.try_get(key).as_ref()?;
                            Some((entity, chunk.clone(), derived?))
                        })
                        .collect()
                })
            }
            BatchChunks::Owned(items) => {
                let job = DeriveBricksJob::owned(items.iter().map(|(_, chunk)| chunk.clone()).collect());
                super::compute::spawn(job, move |derived: Vec<Option<super::bricks::DerivedBricks>>| {
                    items
                        .into_iter()
                        .zip(derived)
                        .filter_map(|((entity, chunk), derived)| Some((entity, chunk, derived?)))
                        .collect()
                })
            }
        };
        // Child of the region root: removing the region cancels the task.
        commands.spawn((SnapshotVoxelTask(task), ChildOf(root)));
        outstanding += 1;
    }
    let mut budget = super::meshing::queue::per_frame_budget(
        SNAPSHOT_VOXELS_PER_SECOND,
        time.delta(),
        SNAPSHOT_VOXELS_PER_FRAME,
        MAX_SNAPSHOT_VOXELS_PER_FRAME,
    );
    while budget > 0 {
        let Some((entity, chunk, bricks)) = queue.ready.pop_front() else {
            break;
        };
        // No marker: the entity is gone (region removed/replaced) or an
        // incremental update already wrote newer voxels. Costs no budget.
        if !pending.contains(entity) {
            continue;
        }
        // A chunk's bricks land together; edits that arrived while they were
        // derived are newer and go on top (`land_chunk_bricks`).
        let count = bricks.spawn.len();
        commands.queue(move |world: &mut World| {
            if world.get::<PendingSnapshotVoxels>(entity).is_some() {
                super::bricks::land_chunk_bricks(world, entity, chunk, bricks);
            }
        });
        queue.applied += count as u32;
        // TODO(perf, chunk flip): a whole chunk lands in one frame — at 16³
        // chunks up to 512 bricks (296 for a uniform solid chunk: its face
        // shell; ~200 for a typical surface chunk) against
        // MAX_SNAPSHOT_VOXELS_PER_FRAME (160). Kept whole on purpose: the
        // landing swaps the SimChunk mirror and lays PendingBlockDeltas on
        // top in one step, and a half-landed chunk would have edits racing
        // bricks that are not there yet. The overshoot is bounded to one
        // chunk per frame (the budget saturates, nothing carries over), the
        // voxels were derived off-thread, and meshing has its own budget.
        // If `main_stall` / the join timeline show it, split by staging the
        // bricks behind the pending marker instead of here.
        budget = budget.saturating_sub(count.max(1));
    }
    if queue.applied > 0 && queue.ready.is_empty() && queue.batches.is_empty() && outstanding == 0 {
        game::perf_event!("snapshot_voxels_applied", "bricks={}", queue.applied);
        queue.applied = 0;
    }
}

/// The text a lost session shows on the main menu.
pub fn session_end_reason(error: &game::GameError) -> String {
    match error {
        game::GameError::Disconnected(reason) => reason.clone(),
        game::GameError::CrashedOnServerEvent => "the game crashed on a server event".into(),
        game::GameError::QuitRequested => "the session was quit".into(),
    }
}

/// The autopilot's region -> host map (native only); the browser has no autopilot, so its
/// stand-in is a resource nothing ever inserts.
#[cfg(not(target_arch = "wasm32"))]
use super::autopilot::RegionHosts;
#[cfg(target_arch = "wasm32")]
#[derive(Resource)]
pub struct RegionHosts;

pub fn drain_client_updates(
    mut commands: Commands,
    updates: Res<ClientUpdates>,
    mut player: ResMut<LocalPlayer>,
    mut regions: ResMut<Regions>,
    mut roots: ResMut<RegionRoots>,
    mut map: ResMut<SimEntityMap>,
    // Paired to stay within Bevy's 16 system parameters.
    (mut hud_status, mut tick_times, mut drain, mut region_hosts): (
        ResMut<HudStatus>,
        Option<ResMut<super::hud::RegionTickTimes>>,
        Option<ResMut<super::drain_notice::DrainNotice>>,
        Option<ResMut<RegionHosts>>,
    ),
    mut ui_updates: ResMut<crate::renderer::slint_overlay::UiUpdates>,
    mut debug_lines: ResMut<crate::renderer::collider_debug::DebugColliderLines>,
    mut lod_rings: ResMut<lod_rings::LodRings>,
    mut snapshot_voxels: ResMut<SnapshotVoxelQueue>,
    mut retired_roots: ResMut<RetiredRegionRoots>,
    camera_mode: Option<Res<super::commander::CameraMode>>,
    mut tile_msgs: Option<ResMut<Messages<TileMirrorMsg>>>,
    mut history_out: super::history_view::HistoryOut,
    mut session_ended: Option<ResMut<Messages<crate::session::SessionEnded>>>,
) {
    // Unbudgeted: drains everything the game thread queued since last frame.
    let mut stall = game::perf::StallGuard::new("drain_client_updates");
    // One region tree spawned per frame (~20k entities at the 5x5 window):
    // a slow frame that collected several snapshots spent up to 233 ms here
    // (2026-09-23 `fly-probe` trace). Later events wait, in order.
    let mut spawned_region = false;
    while !spawned_region {
        let Ok(event) = updates.0.try_recv() else { break };
        stall.add_work(1);
        match event {
            ClientUpdateEvent::NewRegion(id, data, receiver) => {
                spawned_region = true;
                // Back before its old tree was despawned: finish that first,
                // so two trees of one region never share the chunk grid.
                retired_roots.0.retain(|&(rc, root)| {
                    if rc == id {
                        commands.entity(root).despawn();
                    }
                    rc != id
                });
                if roots.0.contains_key(&id) {
                    // Replace-on-re-receipt: resubscribe/crash-respawn resync.
                    remove_region(&mut commands, &mut regions, &mut roots, &mut map, id);
                }
                let (root, bodies) = spawn_region_roots(&mut commands, id);
                roots.insert(id, root, bodies);
                regions.0.insert(id, receiver);
                // The snapshot's item piles and halo pile mirrors (cells in
                // the region's frame), ahead of the region's own updates.
                let frame = |m: &std::collections::BTreeMap<game::WorldBlockPos, game::ItemPile>| -> Vec<_> {
                    m.iter().map(|(p, pile)| (game::local_cell(id, *p), Some(pile.view()))).collect()
                };
                let (piles, mirrors) = (frame(data.item_piles()), frame(data.halo_piles()));
                // The water pass's recipe (its tiles mesh once the chunks land).
                let water_ground = Arc::new(data.ground_recipes().clone());
                commands.queue(move |world: &mut World| {
                    if let Some(mut msgs) = world.get_resource_mut::<Messages<super::water::WaterMsg>>() {
                        msgs.write(super::water::WaterMsg::Region { region: id, ground: water_ground });
                    }
                    if let Some(mut msgs) = world.get_resource_mut::<Messages<super::item_piles::ItemPileMsg>>() {
                        msgs.write(super::item_piles::ItemPileMsg::RemoveRegion(id));
                        if !piles.is_empty() {
                            msgs.write(super::item_piles::ItemPileMsg::Set { region: id, changes: piles });
                        }
                        if !mirrors.is_empty() {
                            msgs.write(super::item_piles::ItemPileMsg::SetMirrors { region: id, changes: mirrors });
                        }
                    }
                });
                // The fortress tools' tile mirror (commander only).
                if let Some(msgs) = tile_msgs.as_mut() {
                    msgs.write(TileMirrorMsg::Reset {
                        region: id,
                        ground: data.ground_recipes().clone(),
                        designations: data.designations().iter().map(|(p, d)| (*p, *d)).collect(),
                        constructed: data.constructed().keys().copied().collect(),
                    });
                    // Its terrain halo: walks are planned over it too.
                    msgs.write(TileMirrorMsg::halo_of(id, &data));
                }
                let spawn = game::perf::Stopwatch::start();
                let data = Arc::new(data);
                spawn_region_snapshot(
                    &mut commands,
                    root,
                    bodies,
                    id,
                    &data,
                    &mut map,
                    player.0,
                    &mut snapshot_voxels,
                    commander(camera_mode.as_deref()),
                );
                // Main-thread cost of one region: an entity per sim entity.
                // The per-chunk voxel derivation runs off-thread (see
                // `apply_snapshot_voxels`).
                game::perf_event!(
                    "bridge_region_spawned",
                    "region={},{} spawn_ms={} entities={} chunks={} regions={}",
                    id.x,
                    id.z,
                    game::perf::ms(spawn.elapsed()),
                    data.ecs.entities.len(),
                    data.ecs.chunk.iter().filter(|(_, chunk)| chunk.is_some()).count(),
                    roots.0.len()
                );
                info!("bridge: region {:?} loaded", id);
            }
            ClientUpdateEvent::SetPlayer(client_id) => player.0 = Some(client_id),
            ClientUpdateEvent::RemoveRegion(id) => {
                // Unmapped now, despawned over the next frames.
                if let Some(root) = unmap_region(&mut regions, &mut roots, &mut map, id) {
                    retired_roots.0.push_back((id, root));
                    commands.queue(move |world: &mut World| super::bricks::prune_region_skipped(world, id));
                }
                debug_lines.0.remove(&id);
                commands.queue(move |world: &mut World| {
                    if let Some(mut msgs) = world.get_resource_mut::<Messages<super::water::WaterMsg>>() {
                        msgs.write(super::water::WaterMsg::RemoveRegion { region: id });
                    }
                    if let Some(mut msgs) = world.get_resource_mut::<Messages<super::item_piles::ItemPileMsg>>() {
                        msgs.write(super::item_piles::ItemPileMsg::RemoveRegion(id));
                    }
                });
                info!("bridge: region {:?} removed", id);
            }
            ClientUpdateEvent::LodData(key, payload, water) => {
                lod_rings.pending.push(lod_rings::LodEventMsg::Data(key, payload, water));
            }
            ClientUpdateEvent::RemoveLod { key, near_takeover } => {
                lod_rings
                    .pending
                    .push(lod_rings::LodEventMsg::Remove { key, near_takeover });
            }
            ClientUpdateEvent::GameCrash(e) => {
                // The session ended without our Quit (lost server, failed
                // connect, crash): the session flow takes us back to the menu.
                error!("bridge: game session ended: {:?}", e);
                if let Some(ended) = session_ended.as_mut() {
                    ended.write(crate::session::SessionEnded { reason: Some(session_end_reason(&e)) });
                }
            }
            ClientUpdateEvent::HudStatus { home_region, viewer_region, connection } => {
                hud_status.home_region = Some(home_region);
                hud_status.viewer_region = Some(viewer_region);
                hud_status.connection = connection;
            }
            ClientUpdateEvent::UiChannel(recv) => {
                ui_updates.0 = Some(recv);
            }
            ClientUpdateEvent::DebugLines(id, lines) => {
                debug_lines.0.insert(id, lines);
            }
            ClientUpdateEvent::WorldMap(world_map) => {
                if let Some(msgs) = history_out.world_map.as_mut() {
                    msgs.write(super::world_map::WorldMapMsg::Map(world_map));
                }
            }
            ClientUpdateEvent::PlayRefused(rc, why) => {
                if let Some(msgs) = history_out.world_map.as_mut() {
                    msgs.write(super::world_map::WorldMapMsg::Refused(rc, why));
                }
            }
            ClientUpdateEvent::Sites(v) => history_out.write(HistoryMsg::Sites(v)),
            ClientUpdateEvent::Calendar(c) => history_out.write(HistoryMsg::Calendar(c)),
            ClientUpdateEvent::RegionTickTimes(stats) => {
                if let Some(times) = tick_times.as_mut() {
                    times.0 = stats;
                }
            }
            ClientUpdateEvent::Notice(n) => history_out.write(HistoryMsg::Notice(n)),
            ClientUpdateEvent::HostDraining { remaining_secs } => {
                if let Some(drain) = drain.as_mut() {
                    drain.remaining_secs = remaining_secs;
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            ClientUpdateEvent::RegionHosts(hosts) => {
                if let Some(map) = region_hosts.as_mut() {
                    map.0 = hosts.into_iter().collect();
                }
            }
            // Only the native autopilot reads the region -> host map; the browser never sends it.
            #[cfg(target_arch = "wasm32")]
            ClientUpdateEvent::RegionHosts(_) => {
                let _ = &mut region_hosts;
            }
            ClientUpdateEvent::PersonInfo(p) => history_out.write(HistoryMsg::PersonInfo(p)),
            ClientUpdateEvent::Civs(c) => history_out.write(HistoryMsg::Civs(c)),
            ClientUpdateEvent::Legends(p) => history_out.write(HistoryMsg::Legends(p)),
            ClientUpdateEvent::AdventureResult(r) => history_out.write(HistoryMsg::AdventureResult(r)),
        }
    }
}

/// Tear down everything the bridge built for a region: root entity tree,
/// update receiver, entity map entries. Also the first half of
/// replace-on-re-receipt (crash-respawn resync).
fn remove_region(
    commands: &mut Commands,
    regions: &mut Regions,
    roots: &mut RegionRoots,
    map: &mut SimEntityMap,
    id: RegionId,
) {
    if let Some(root) = unmap_region(regions, roots, map, id) {
        // despawn() removes descendants via ChildOf relationships (Bevy 0.16+).
        commands.entity(root).despawn();
        commands.queue(move |world: &mut World| super::bricks::prune_region_skipped(world, id));
    }
}

/// Take a region out of every bridge map and return its root, which the
/// caller despawns.
fn unmap_region(
    regions: &mut Regions,
    roots: &mut RegionRoots,
    map: &mut SimEntityMap,
    id: RegionId,
) -> Option<Entity> {
    regions.0.remove(&id);
    map.0.retain(|(region, _), _| *region != id);
    roots.remove(&id)
}

/// Port of the old `TrueWorld::new` snapshot walk.
fn spawn_region_snapshot(
    commands: &mut Commands,
    root: Entity,
    bodies: Entity,
    region: RegionId,
    data: &Arc<GameData>,
    map: &mut SimEntityMap,
    local_player: Option<game::ClientId>,
    voxel_queue: &mut SnapshotVoxelQueue,
    commander: bool,
) {
    let mut chunk_entities: Vec<(Entity, EntityKey, Vec3)> = Vec::new();
    // Every player in the snapshot has a sim camera, but only the local
    // player's may become this window's active Camera3d.
    let local_camera_key = local_player.and_then(|id| data.player_entites.get(&id).copied());
    // Entity-halo mirrors in the snapshot, by entity.
    let mirrors: BTreeMap<EntityKey, HaloMirror> = data
        .halo_entities
        .iter()
        .map(|(id, h)| (h.entity, HaloMirror { id: *id, source: h.source_region }))
        .collect();
    for (key, _) in data.ecs.entities.iter() {
        // Terrain under the static region root, everything else under the
        // bodies root (see `RegionRoots`).
        let is_chunk = data.ecs.chunk.try_get(key).is_some();
        let mut e = commands.spawn((
            SimEntity { region, key },
            Transform::IDENTITY,
            Visibility::default(),
            ChildOf(if is_chunk { root } else { bodies }),
        ));
        let mut local_position = Vec3::ZERO;
        if let Some(handle) = data.ecs.rigidbody.try_get(key) {
            if let Some(body) = data.physics.bodies.get(*handle) {
                let tf = iso_to_transform(body.position());
                local_position = tf.translation;
                e.insert((tf, SimTarget::body(tf.translation, tf.rotation)));
            }
        }
        if is_chunk {
            e.insert((PendingSnapshotVoxels, super::bricks::ChunkBricks::default()));
            chunk_entities.push((e.id(), key, local_position));
        }
        if let Some(cam) = data.ecs.camera.try_get(key) {
            if Some(key) == local_camera_key && commander {
                e.insert((LocalBody, Visibility::Hidden));
            } else if Some(key) == local_camera_key {
                if let Some(handle) = cam.view_matrix {
                    if let Some(body) = data.physics.bodies.get(handle) {
                        let mut tf = iso_to_transform(body.position());
                        let target = SimTarget::camera(tf.translation, tf.rotation);
                        tf.translation = target.goal();
                        e.insert((
                            Camera3d::default(),
                            Projection::Perspective(perspective_to_projection(&cam.proj_matrix)),
                            tf,
                            target,
                            super::UiCamera,
                        ));
                    }
                }
            }
            // Other players' pose tracking comes from the rigidbody branch
            // above; their cameras stay inert in this window.
        }
        if let Some(kind) = *data.ecs.kind.try_get(key) {
            e.insert(SimKind(kind));
        }
        if let Some(mirror) = mirrors.get(&key) {
            e.insert(*mirror);
        }
        if let Some(body) = data.ecs.voxel_body.try_get(key) {
            e.insert((super::voxel_bodies::PendingVoxelBody(Arc::new(body.model.clone())), super::voxel_bodies::VoxelBodyModel));
        }
        map.0.insert((region, key), e.id());
    }

    // The chunks' voxels are derived + RLE-encoded off the main thread, in
    // batches that share the snapshot through the `Arc` (no per-chunk clone).
    // Batches are only QUEUED here; `apply_snapshot_voxels` hands them to the
    // compute pool a few at a time, nearest to the camera first.
    let offset = region.world_offset();
    let offset = Vec3::new(offset[0], offset[1], offset[2]);
    for batch in chunk_entities.chunks(SNAPSHOT_VOXEL_TASK_CHUNKS) {
        let center = batch.iter().map(|(_, _, position)| *position).sum::<Vec3>() / batch.len() as f32;
        voxel_queue.batches.push_back(PendingVoxelBatch {
            region,
            root,
            // Chunk origin -> chunk centre, region-local -> world.
            center: offset + center + Vec3::splat(game::CHUNK_UNITS as f32 / 2.0),
            chunks: BatchChunks::Snapshot(
                Arc::clone(data),
                batch
                    .iter()
                    .map(|(entity, key, position)| {
                        let t = game::rapier3d::math::Vector::new(position.x, position.y, position.z);
                        (*entity, *key, game::chunk_coords_from_translation(t))
                    })
                    .collect(),
            ),
        });
    }
}

pub fn drain_region_updates(
    mut commands: Commands,
    regions: Res<Regions>,
    roots: Res<RegionRoots>,
    mut map: ResMut<SimEntityMap>,
    mut targets: Query<(
        &mut SimTarget,
        &mut Transform,
        Has<super::bricks::ChunkBricks>,
    )>,
    local_player: Res<LocalPlayer>,
    mut cursor_policy: ResMut<super::cursor::CursorPolicy>,
    mut voxel_queue: ResMut<SnapshotVoxelQueue>,
    camera_mode: Option<Res<super::commander::CameraMode>>,
    mut tile_msgs: Option<ResMut<Messages<TileMirrorMsg>>>,
    mut pile_msgs: Option<ResMut<Messages<super::item_piles::ItemPileMsg>>>,
) {
    let commander = commander(camera_mode.as_deref());
    // Local cameras inserted by Commands this drain, not yet queryable: a
    // position update for one must keep its camera target (eye offset,
    // smoothing) rather than overwrite it with a body target.
    let mut new_cameras: Vec<Entity> = Vec::new();
    for (&region, receiver) in regions.0.iter() {
        let root = *roots.0.get(&region).expect("RegionRoots co-populated with Regions");
        let bodies = roots.bodies(region).expect("RegionRoots co-populated with Regions");
        // Cell terrain loaded this drain: voxels are derived off-thread in the
        // snapshot queue's batches (nearest first, budgeted per frame).
        let mut new_terrain: Vec<(Entity, game::Chunk, Vec3)> = Vec::new();
        while let Ok(update) = receiver.try_recv() {
            // Do and Undo both applied: see SimTarget doc comment.
            match update.update_kind {
                GameDataUpdateKind::AddTerrainChunk(key, iso, chunk) => {
                    let tf = iso_to_transform(&iso);
                    let bundle = (
                        SimEntity { region, key },
                        tf,
                        Visibility::default(),
                        SimTarget::body(tf.translation, tf.rotation),
                        PendingSnapshotVoxels,
                        ChildOf(root),
                    );
                    // A runtime chunk (`spawn_chunk_safe`) already sent its
                    // `CreateEntity`: adopt that entity instead of spawning a
                    // second one, so undo's `RemoveEntity` despawns it.
                    // `ChunkBricks` only if absent: an already-landed entity
                    // keeps its list until the new bricks replace it
                    // (`land_chunk_bricks`), so no brick is orphaned.
                    let e = if let Some(&existing) = map.0.get(&(region, key)) {
                        commands
                            .entity(existing)
                            .insert(bundle)
                            .insert_if_new(super::bricks::ChunkBricks::default());
                        existing
                    } else {
                        let e = commands.spawn((bundle, super::bricks::ChunkBricks::default())).id();
                        map.0.insert((region, key), e);
                        e
                    };
                    new_terrain.push((e, *chunk, tf.translation));
                }
                GameDataUpdateKind::CreateEntity(key) => {
                    let e = commands
                        .spawn((
                            SimEntity { region, key },
                            Transform::IDENTITY,
                            Visibility::default(),
                            SimTarget::body(Vec3::ZERO, Quat::IDENTITY),
                            ChildOf(bodies),
                        ))
                        .id();
                    map.0.insert((region, key), e);
                }
                GameDataUpdateKind::RemoveEntity(key) => {
                    if let Some(e) = map.0.remove(&(region, key)) {
                        // A terrain chunk's bricks are siblings, not children.
                        commands.queue(move |world: &mut World| super::bricks::despawn_sim_entity(world, e));
                    } else {
                        warn!("bridge: RemoveEntity for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::SetEntityPosition(key, iso) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetEntityPosition for unmapped {:?}", key);
                        continue;
                    };
                    let tf = iso_to_transform(&iso);
                    if let Ok((mut target, mut transform, terrain_chunk)) = targets.get_mut(e) {
                        target.pos = tf.translation;
                        target.rot = tf.rotation;
                        // `interpolate_transforms` skips terrain chunks (they
                        // never move, and there are ~64k of them), so one
                        // that does get a new pose snaps to it here, and its
                        // bricks follow.
                        if terrain_chunk {
                            let goal = target.goal();
                            let rot = target.rot;
                            transform.set_if_neq(Transform { translation: goal, rotation: rot, ..*transform });
                            commands.queue(move |world: &mut World| super::bricks::move_chunk_bricks(world, e, goal));
                        }
                    } else {
                        // Entity spawned via Commands earlier this same drain —
                        // not yet queryable. Insert (overwrites on apply), and
                        // start it at its first pose rather than gliding in
                        // from the region origin: a chunk created at runtime
                        // is keyed for meshing by where its Transform is.
                        let target = if new_cameras.contains(&e) {
                            SimTarget::camera(tf.translation, tf.rotation)
                        } else {
                            SimTarget::body(tf.translation, tf.rotation)
                        };
                        let start = Transform { translation: target.goal(), ..tf };
                        commands.entity(e).insert((start, target));
                    }
                }
                GameDataUpdateKind::SetVoxelComponent(key, Some(voxels)) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        // Newer than any snapshot voxels still in flight.
                        commands.queue(move |world: &mut World| super::bricks::write_legacy_voxels(world, e, voxels));
                    } else {
                        warn!("bridge: SetVoxelComponent for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::SetBlocks(key, deltas) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetBlocks for unmapped {:?}", key);
                        continue;
                    };
                    // Bricks in hand: edit them (and their mirror). Still
                    // being derived (or inserted later this drain): buffer
                    // until they land.
                    commands.queue(move |world: &mut World| {
                        super::bricks::apply_block_deltas(world, e, deltas);
                        super::water::chunk_edited(world, region, e);
                    });
                }
                GameDataUpdateKind::SetVoxelComponent(key, None) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.queue(move |world: &mut World| {
                            super::bricks::clear_chunk_voxels(world, e);
                            super::water::chunk_edited(world, region, e);
                        });
                    } else {
                        warn!("bridge: SetVoxelComponent for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::AddCameraComponent(key, client_id, proj, iso) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: AddCameraComponent for unmapped {:?}", key);
                        continue;
                    };
                    let tf = iso_to_transform(&iso);
                    if local_player.0 == Some(client_id) && commander {
                        // The commander's own body: tracked, never shown.
                        commands.entity(e).insert((
                            tf,
                            SimTarget::body(tf.translation, tf.rotation),
                            LocalBody,
                            Visibility::Hidden,
                        ));
                    } else if local_player.0 == Some(client_id) {
                        let target = SimTarget::camera(tf.translation, tf.rotation);
                        let tf = Transform { translation: target.goal(), ..tf };
                        new_cameras.push(e);
                        commands.entity(e).insert((
                            Camera3d::default(),
                            Projection::Perspective(perspective_to_projection(&proj)),
                            tf,
                            target,
                            super::UiCamera,
                        ));
                    } else {
                        // Another player's camera: track its pose so the
                        // entity follows the sim, but never render from it.
                        commands.entity(e).insert((tf, SimTarget::body(tf.translation, tf.rotation)));
                    }
                }
                GameDataUpdateKind::RemoveCameraComponent(key) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).remove::<(Camera3d, Projection)>();
                    } else {
                        warn!("bridge: RemoveCameraComponent for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::UpdateCameraViewProj(key, proj) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).insert(Projection::Perspective(perspective_to_projection(&proj)));
                    } else {
                        warn!("bridge: UpdateCameraViewProj for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::UpdateCameraViewMatrix(key, iso) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: UpdateCameraViewMatrix for unmapped {:?}", key);
                        continue;
                    };
                    let tf = iso_to_transform(&iso);
                    if let Ok((mut target, _, _)) = targets.get_mut(e) {
                        target.pos = tf.translation;
                        target.rot = tf.rotation;
                    } else {
                        commands.entity(e).insert(SimTarget::camera(tf.translation, tf.rotation));
                    }
                }
                GameDataUpdateKind::SetEntityKind(key, kind) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetEntityKind for unmapped {:?}", key);
                        continue;
                    };
                    match kind {
                        Some(k) => { commands.entity(e).insert(SimKind(k)); }
                        None => { commands.entity(e).remove::<SimKind>(); }
                    }
                }
                GameDataUpdateKind::SetHaloSource(key, src) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetHaloSource for unmapped {:?}", key);
                        continue;
                    };
                    match src {
                        Some((source, id)) => { commands.entity(e).insert(HaloMirror { id, source }); }
                        None => {
                            commands.entity(e).remove::<HaloMirror>();
                            // Upgraded in place: an owned body draws.
                            commands.entity(e).insert(Visibility::Inherited);
                        }
                    }
                }
                // Plain per-tile writes, undo included: forwarded to the
                // fortress tools' tile mirror.
                GameDataUpdateKind::SetDesignations(changes) => {
                    if let Some(msgs) = tile_msgs.as_mut() {
                        msgs.write(TileMirrorMsg::Designations { region, changes });
                    }
                }
                GameDataUpdateKind::SetConstructed(changes) => {
                    if let Some(msgs) = tile_msgs.as_mut() {
                        msgs.write(TileMirrorMsg::Constructed { region, changes });
                    }
                }
                // The terrain halo, for walk planning past the region's edge.
                kind @ (GameDataUpdateKind::SetHaloTerrain(_) | GameDataUpdateKind::SetHaloGround(_)) => {
                    if let Some(msg) = tile_msgs.as_mut().and_then(|_| TileMirrorMsg::from_update(region, &kind)) {
                        tile_msgs.as_mut().unwrap().write(msg);
                    }
                }
                // Item piles (`game::item_piles`), drawn by `item_piles`.
                GameDataUpdateKind::SetItemPiles(changes) => {
                    if let Some(msgs) = pile_msgs.as_mut() {
                        msgs.write(super::item_piles::ItemPileMsg::Set { region, changes });
                    }
                }
                GameDataUpdateKind::SetHaloPiles(changes) => {
                    if let Some(msgs) = pile_msgs.as_mut() {
                        msgs.write(super::item_piles::ItemPileMsg::SetMirrors { region, changes });
                    }
                }
                // Render-only pickup / landing events, animated by `item_flights`.
                GameDataUpdateKind::ItemsPickedUp(picks) => {
                    if let Some(msgs) = pile_msgs.as_mut() {
                        msgs.write(super::item_piles::ItemPileMsg::PickedUp { region, picks });
                    }
                }
                GameDataUpdateKind::ItemsLanded(flights) => {
                    if let Some(msgs) = pile_msgs.as_mut() {
                        msgs.write(super::item_piles::ItemPileMsg::Landed { region, flights });
                    }
                }
                GameDataUpdateKind::SetFreeCam(client_id, enabled) => {
                    // Only the local player's toggle may grab this window's cursor.
                    if local_player.0 != Some(client_id) {
                        continue;
                    }
                    // Idempotent: SetFreeCam is emitted Do-only (no Undo
                    // pairing), so every reconcile that replays the toggle tick
                    // re-delivers the SAME value; `apply_cursor_policy` only
                    // writes the window on a real change.
                    cursor_policy.sim_grab = enabled;
                }
                // Sim-owned animation (`game::anim`), drawn by `rigs`.
                GameDataUpdateKind::SetAnimator(key, animator) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetAnimator for unmapped {:?}", key);
                        continue;
                    };
                    match animator {
                        Some(a) => { commands.entity(e).insert(super::rigs::SimAnimator(a)); }
                        None => { commands.entity(e).remove::<super::rigs::SimAnimator>(); }
                    }
                }
                // Detached terrain (`game::voxel_body`): its model is meshed
                // into brick children by `voxel_bodies`, which ride the
                // entity's interpolated pose; `RemoveEntity` despawns them
                // with it. Starts at the pose rather than gliding in.
                GameDataUpdateKind::AddVoxelBody(key, iso, model) => {
                    let tf = iso_to_transform(&iso);
                    let bundle = (
                        tf,
                        SimTarget::body(tf.translation, tf.rotation),
                        super::voxel_bodies::PendingVoxelBody(Arc::new(*model)),
                        super::voxel_bodies::VoxelBodyModel,
                    );
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).insert(bundle);
                    } else {
                        let e = commands
                            .spawn((SimEntity { region, key }, Visibility::default(), ChildOf(bodies), bundle))
                            .id();
                        map.0.insert((region, key), e);
                    }
                }
            }
        }
        if !new_terrain.is_empty() {
            // Region-local centroid -> world, chunk origin -> centre.
            let offset = region.world_offset();
            let offset = Vec3::new(offset[0], offset[1], offset[2]);
            for batch in new_terrain.chunks(SNAPSHOT_VOXEL_TASK_CHUNKS) {
                let center = batch.iter().map(|(_, _, p)| *p).sum::<Vec3>() / batch.len() as f32;
                voxel_queue.batches.push_back(PendingVoxelBatch {
                    region,
                    root,
                    center: offset + center + Vec3::splat(game::CHUNK_UNITS as f32 / 2.0),
                    chunks: BatchChunks::Owned(
                        batch.iter().map(|(e, chunk, _)| (*e, chunk.clone())).collect(),
                    ),
                });
            }
        }
    }
}

/// Render rule: one copy per `EntityId`. A halo mirror hides while its
/// source region is loaded locally (the owned copy draws there); otherwise
/// the mirror in the lowest hosting region draws and the others (a corner
/// entity is mirrored into up to 3 regions) hide. Runs every frame; mirrors
/// are few.
pub fn dedupe_halo_entities(
    roots: Res<RegionRoots>,
    mut mirrors: Query<(&HaloMirror, &SimEntity, &mut Visibility)>,
) {
    // The host region drawing each id whose owner is not loaded here.
    let mut drawn: BTreeMap<game::EntityId, RegionId> = BTreeMap::new();
    for (m, host, _) in mirrors.iter() {
        if !roots.0.contains_key(&m.source) {
            let r = drawn.entry(m.id).or_insert(host.region);
            *r = (*r).min(host.region);
        }
    }
    for (m, host, mut vis) in mirrors.iter_mut() {
        let shown = drawn.get(&m.id) == Some(&host.region);
        let want = if shown { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
    }
}

/// Enforce exactly one active render camera: the local player's camera in the
/// *home* region. During a boundary handoff the player is briefly owned by both
/// the outgoing and incoming region, so both spawn a local `Camera3d` +
/// `UiCamera`. Two active cameras with equal priority make Bevy pick
/// ambiguously per frame (view flickers; mouse-look/HUD bind to the wrong one).
///
/// Rather than remove `Camera3d` (which would need fragile re-add logic when
/// the player crosses back into a still-loaded region), this toggles
/// `Camera::is_active` and moves the single `UiCamera` marker so only
/// the home camera renders. All `Camera3d` entities here are the local player's
/// (the bridge never gives another player a `Camera3d`), so no ownership filter
/// is needed. Falls back to a deterministic keeper if no camera sits in the
/// home region yet (handoff mid-flight) to avoid a black screen or flicker.
///
/// While the frontend has the screen (`session::frontend_has_screen`: menu,
/// starting screen, arrival map) no body camera is active or carries
/// `UiCamera` — the frontend camera does.
pub fn enforce_single_active_camera(
    mut commands: Commands,
    hud_status: Res<HudStatus>,
    state: Option<Res<State<crate::session::AppState>>>,
    in_play: Option<Res<crate::session::InPlay>>,
    mut cams: Query<(Entity, &SimEntity, &mut Camera, Has<super::UiCamera>), With<Camera3d>>,
) {
    if crate::session::frontend_has_screen(state.as_deref(), in_play.as_deref()) {
        for (e, _, mut cam, has_marker) in &mut cams {
            if cam.is_active {
                cam.is_active = false;
            }
            if has_marker {
                commands.entity(e).remove::<super::UiCamera>();
            }
        }
        return;
    }
    let home = hud_status.home_region;
    // Choose the single keeper: the home-region camera if one exists, else a
    // deterministic fallback (lowest Entity) so the pick doesn't flip frame to
    // frame while a handoff is mid-flight (which would itself flicker).
    let mut home_keeper: Option<Entity> = None;
    let mut fallback: Option<Entity> = None;
    for (e, se, _, _) in cams.iter() {
        if fallback.is_none_or(|f| e < f) {
            fallback = Some(e);
        }
        if Some(se.region) == home && home_keeper.is_none_or(|k| e < k) {
            home_keeper = Some(e);
        }
    }
    let Some(keeper) = home_keeper.or(fallback) else { return };

    for (e, _se, mut cam, has_marker) in cams.iter_mut() {
        let active = e == keeper;
        if cam.is_active != active {
            cam.is_active = active;
        }
        match (active, has_marker) {
            (true, false) => {
                commands.entity(e).insert(super::UiCamera);
            }
            (false, true) => {
                commands.entity(e).remove::<super::UiCamera>();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{ClientUpdates, GameEvents, LocalPlayer};
    use game::{ClientUpdateEvent, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, RegionCoords, Rollback};
    use game::EntityKey;
    use slotmapd::KeyData;

    fn app_shell() -> (App, crossbeam::channel::Sender<ClientUpdateEvent>) {
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        let (game_send, _game_recv) = crossbeam::channel::unbounded();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(ClientUpdates(client_recv))
            .insert_resource(GameEvents(game_send))
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<HudStatus>()
            .init_resource::<crate::renderer::slint_overlay::UiUpdates>()
            .init_resource::<crate::renderer::collider_debug::DebugColliderLines>()
            .init_resource::<lod_rings::LodRings>()
            .init_resource::<SnapshotVoxelQueue>()
            .init_resource::<crate::renderer::cursor::CursorPolicy>()
            .init_resource::<RetiredRegionRoots>()
            .add_systems(
                PreUpdate,
                (
                    drain_client_updates,
                    despawn_retired_regions,
                    drain_region_updates,
                    apply_snapshot_voxels,
                )
                    .chain(),
            );
        (app, client_send)
    }

    fn test_app() -> (App, crossbeam::channel::Sender<ClientUpdateEvent>, crossbeam::channel::Sender<GameDataUpdate>, game::RegionId) {
        let (app, client_send) = app_shell();
        let (update_send, update_recv) = crossbeam::channel::unbounded();
        let region_id = RegionCoords::new(0, 0);
        let rb = Rollback::new(None);
        client_send
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv))
            .unwrap();
        (app, client_send, update_send, region_id)
    }

    fn test_proj() -> game::Perspective3 {
        game::Perspective3::new(
            1.5,
            1.2,
            0.1,
            100.0,
        )
    }

    fn key(n: u64) -> EntityKey {
        EntityKey::from(KeyData::from_ffi((1 << 32) | n))
    }

    #[test]
    fn new_region_spawns_root() {
        let (mut app, _c, _u, region_id) = test_app();
        app.update();
        let roots = app.world().resource::<RegionRoots>();
        assert!(roots.0.contains_key(&region_id));
    }

    #[test]
    fn create_set_position_remove() {
        let (mut app, _c, updates, region_id) = test_app();
        app.update();

        let k = key(7);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetEntityPosition(k, game::Pose::identity()))).unwrap();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).expect("entity mapped");
        assert!(app.world().entity(e).contains::<SimTarget>());
        let target = app.world().entity(e).get::<SimTarget>().expect("SimTarget present");
        assert_eq!(target.pos, Vec3::ZERO, "identity isometry should convert to zero translation");
        assert_eq!(target.rot, Quat::IDENTITY, "identity isometry should convert to identity rotation");

        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::RemoveEntity(k))).unwrap();
        app.update();
        assert!(app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).is_none());
        assert!(app.world().get_entity(e).is_err(), "despawned");
    }

    #[test]
    fn unknown_key_is_tolerated() {
        let (mut app, _c, updates, _region_id) = test_app();
        app.update();
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetEntityPosition(key(99), game::Pose::identity()))).unwrap();
        app.update(); // must not panic
    }

    #[test]
    fn camera_add_update_remove() {
        let (mut app, client, updates, region_id) = test_app();
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        app.update();
        let k = key(3);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddCameraComponent(k, 0, test_proj(), game::Pose::identity()),
        )).unwrap();
        app.update();
        // second frame: AddCameraComponent on a same-drain-spawned entity goes through Commands
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        assert!(app.world().entity(e).contains::<Camera3d>());
        assert!(
            app.world().entity(e).contains::<crate::renderer::UiCamera>(),
            "local player's camera must be the default UI camera"
        );
        let Projection::Perspective(p) = app.world().entity(e).get::<Projection>().unwrap() else {
            panic!("expected perspective projection");
        };
        assert!((p.fov - 1.2).abs() < 1e-6);

        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::RemoveCameraComponent(k))).unwrap();
        app.update();
        assert!(!app.world().entity(e).contains::<Camera3d>());
    }

    #[test]
    fn foreign_camera_is_tracked_but_not_activated() {
        let (mut app, client, updates, region_id) = test_app();
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        app.update();
        let k = key(4);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            // client 1's camera arriving at client 0's window
            GameDataUpdateKind::AddCameraComponent(k, 1, test_proj(), game::Pose::identity()),
        )).unwrap();
        app.update();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        assert!(
            !app.world().entity(e).contains::<Camera3d>(),
            "another player's camera must not render this window"
        );
        assert!(
            app.world().entity(e).contains::<SimTarget>(),
            "foreign player's pose must still be tracked"
        );
    }

    #[test]
    fn set_entity_kind_mirrors_to_sim_kind() {
        let (mut app, _c, updates, region_id) = test_app();
        app.update();
        let k = key(5);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(k, Some(game::EntityKind::Player)),
        )).unwrap();
        app.update();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        assert!(app.world().entity(e).contains::<SimKind>());

        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(k, None),
        )).unwrap();
        app.update();
        assert!(!app.world().entity(e).contains::<SimKind>());
    }

    #[test]
    fn snapshot_carries_entity_kind() {
        let (mut app, client) = app_shell();
        let (_update_send, update_recv) = crossbeam::channel::unbounded();
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        rb.create_player_safe(0);
        rb.create_player_safe(1);
        let data = (*rb.data).clone();
        let k0 = *data.player_entites.get(&0).unwrap();
        let k1 = *data.player_entites.get(&1).unwrap();

        let region_id = RegionCoords::new(0, 0);
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        app.update();

        let map = app.world().resource::<SimEntityMap>();
        let e0 = *map.0.get(&(region_id, k0)).unwrap();
        let e1 = *map.0.get(&(region_id, k1)).unwrap();
        assert!(app.world().entity(e0).contains::<SimKind>());
        assert!(app.world().entity(e1).contains::<SimKind>());
    }

    /// A snapshot with one terrain chunk; returns the chunk's sim key and the
    /// voxels its first brick must eventually hold.
    fn chunk_snapshot() -> (game::GameData, EntityKey, VoxelData) {
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        let chunk = game::Chunk::flat_floor(8);
        let expected = VoxelData::new(chunk.brick([0; 3]).voxels());
        let key = rb.create_mesh(game::ChunkCoords::new(0, 0, 0), chunk);
        ((*rb.data).clone(), key, expected)
    }

    /// The bricks of chunk entity `chunk` (empty before they land).
    fn bricks_of(app: &App, chunk: Entity) -> Vec<Entity> {
        app.world().get::<crate::renderer::bricks::ChunkBricks>(chunk).map(|b| b.0.clone()).unwrap_or_default()
    }

    /// The voxels of brick `local` of chunk entity `chunk`, if it exists.
    fn brick_voxels(app: &App, chunk: Entity, local: [u8; 3]) -> Option<VoxelData> {
        bricks_of(app, chunk)
            .into_iter()
            .find(|&b| app.world().get::<crate::renderer::bricks::BrickOf>(b).is_some_and(|o| o.local == local))
            .and_then(|b| app.world().get::<VoxelData>(b).cloned())
    }

    fn wait_for(app: &mut App, mut done: impl FnMut(&App) -> bool) -> bool {
        for _ in 0..500 {
            app.update();
            if done(app) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        false
    }

    /// Voxel derivation left `drain_client_updates` (it was a 0.5-2 s frame per
    /// region): the chunk entity exists at once, flagged pending, and its
    /// voxels land a few frames later from the task pool.
    #[test]
    fn snapshot_chunk_voxels_arrive_off_thread_and_clear_the_marker() {
        let (mut app, client) = app_shell();
        let (_update_send, update_recv) = crossbeam::channel::unbounded();
        let (data, key, expected) = chunk_snapshot();
        let region_id = RegionCoords::new(0, 0);
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, key)).expect("chunk mapped");
        assert!(
            wait_for(&mut app, |app| !bricks_of(app, e).is_empty()),
            "snapshot bricks never arrived"
        );
        assert_eq!(brick_voxels(&app, e, [0; 3]), Some(expected));
        assert!(!app.world().entity(e).contains::<PendingSnapshotVoxels>());
        assert!(!app.world().entity(e).contains::<VoxelData>(), "voxels live on the bricks");
        let mut tasks = app.world_mut().query::<&SnapshotVoxelTask>();
        assert_eq!(tasks.iter(app.world()).count(), 0, "finished task entities are despawned");
    }

    #[test]
    fn nearest_batch_is_fifo_without_a_camera_and_nearest_first_with_one() {
        let centers = [Vec3::new(500.0, 0.0, 0.0), Vec3::new(10.0, 0.0, 10.0), Vec3::new(-300.0, 40.0, 0.0)];
        assert_eq!(nearest_batch(centers.iter().copied(), None), Some(0), "no camera yet: arrival order");
        assert_eq!(nearest_batch(centers.iter().copied(), Some(Vec3::ZERO)), Some(1));
        assert_eq!(nearest_batch(centers.iter().copied(), Some(Vec3::new(-290.0, 40.0, 0.0))), Some(2));
        assert_eq!(nearest_batch(std::iter::empty(), Some(Vec3::ZERO)), None);
    }

    /// A join delivers every region within a second; their voxel batches must
    /// not all hit the FIFO compute pool at once, or the first mesh tasks queue
    /// behind hundreds of them (first terrain moved 0.9 s -> 1.9 s when wire
    /// compression made the regions arrive together).
    #[test]
    fn snapshot_voxel_tasks_are_capped_and_every_chunk_still_gets_voxels() {
        let (mut app, client) = app_shell();
        let (_update_send, update_recv) = crossbeam::channel::unbounded();
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        let chunk_count = SNAPSHOT_VOXEL_TASK_CHUNKS * (MAX_OUTSTANDING_SNAPSHOT_VOXEL_TASKS + 3) + 1;
        let keys: Vec<EntityKey> = (0..chunk_count)
            .map(|i| rb.create_mesh(game::ChunkCoords::new(i % 32, (i / 1024) as i32, (i / 32) % 32), game::Chunk::flat_floor(8)))
            .collect();
        let region_id = RegionCoords::new(0, 0);
        client
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv))
            .unwrap();

        let mut peak = 0;
        let mut done = false;
        for _ in 0..2000 {
            app.update();
            let mut tasks = app.world_mut().query::<&SnapshotVoxelTask>();
            peak = peak.max(tasks.iter(app.world()).count());
            let map = app.world().resource::<SimEntityMap>();
            if keys.iter().all(|k| map.0.get(&(region_id, *k)).is_some_and(|&e| !bricks_of(&app, e).is_empty())) {
                done = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(done, "not every chunk received its snapshot voxels");
        assert!(peak >= 1);
        assert!(
            peak <= MAX_OUTSTANDING_SNAPSHOT_VOXEL_TASKS,
            "{peak} voxel tasks outstanding at once (cap {MAX_OUTSTANDING_SNAPSHOT_VOXEL_TASKS})"
        );
    }

    /// An edit that reaches the bridge before the derived snapshot voxels do is
    /// NEWER than them; the late result must be dropped, not applied on top.
    #[test]
    fn incremental_voxel_update_beats_the_late_snapshot_result() {
        let (mut app, client) = app_shell();
        let (update_send, update_recv) = crossbeam::channel::unbounded();
        let (data, key, snapshot_voxels) = chunk_snapshot();
        let region_id = RegionCoords::new(0, 0);
        let edited = vec![Voxel::new(game::BlockId(9)); game::BRICK_VOXEL_COUNT];
        assert_ne!(VoxelData::new(&edited), snapshot_voxels);
        // Same frame as the snapshot: independent of how fast the task runs.
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        update_send
            .send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetVoxelComponent(key, Some(edited.clone())),
            ))
            .unwrap();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, key)).expect("chunk mapped");
        // Let the task finish and the queue drain, then look at what survived.
        let mut drained = false;
        for _ in 0..500 {
            app.update();
            let mut tasks = app.world_mut().query::<&SnapshotVoxelTask>();
            if tasks.iter(app.world()).count() == 0 {
                drained = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(drained, "the snapshot voxel task never finished");
        app.update(); // one more frame: anything it queued has been applied or dropped
        assert!(!app.world().entity(e).contains::<PendingSnapshotVoxels>());
        if game::BRICKS_PER_CHUNK_EDGE == 1 {
            assert_eq!(brick_voxels(&app, e, [0; 3]), Some(VoxelData::new(&edited)));
        } else {
            // A dense write is one brick's worth: a chunk of many bricks
            // ignores it (`write_legacy_voxels`), and the snapshot lands.
            // Edits racing the snapshot there are `SetBlocks`, covered by
            // `set_blocks_before_the_snapshot_voxels_is_applied_on_top`.
            assert_eq!(brick_voxels(&app, e, [0; 3]), Some(snapshot_voxels));
        }
    }

    /// `SetBlocks` for chunk block 0: air, and a chiselled block 1 when the
    /// chunk has one.
    fn set_blocks(key: EntityKey) -> (GameDataUpdate, Vec<game::BlockDelta>) {
        let air = game::Block::new(game::BlockId::AIR);
        let stone = game::Block::new(game::BlockId(2));
        let deltas = vec![
            game::BlockDelta { index: game::BlockIndex::from_linear(0), block: air, chisel: None },
            game::BlockDelta {
                index: game::BlockIndex::from_linear(1),
                block: stone,
                chisel: Some(game::ChiselData::cuboid(game::BlockId(2), [0, 0, 0], [16, 8, 16])),
            },
        ];
        let update = GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetBlocks(key, deltas.clone()));
        (update, deltas)
    }

    /// The voxels `chunk`'s first brick holds after `deltas`, by the sim's own
    /// derivation.
    fn edited_voxels(chunk: &game::Chunk, deltas: &[game::BlockDelta]) -> VoxelData {
        let mut chunk = chunk.clone();
        for d in deltas {
            chunk.blocks.set(d.index, d.block);
            match &d.chisel {
                Some(c) => {
                    chunk.chisel.insert(d.index, c.clone());
                }
                None => {
                    chunk.chisel.remove(&d.index);
                }
            }
        }
        VoxelData::new(chunk.brick([0; 3]).voxels())
    }

    #[test]
    fn set_blocks_edits_landed_voxels() {
        let (mut app, client) = app_shell();
        let (update_send, update_recv) = crossbeam::channel::unbounded();
        let (data, key, _) = chunk_snapshot();
        let chunk = data.ecs.chunk.get(key).clone();
        let region_id = RegionCoords::new(0, 0);
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        app.update();
        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, key)).expect("chunk mapped");
        assert!(
            wait_for(&mut app, |app| !bricks_of(app, e).is_empty()),
            "snapshot bricks never arrived"
        );
        let (update, deltas) = set_blocks(key);
        update_send.send(update).unwrap();
        app.update();
        assert_eq!(brick_voxels(&app, e, [0; 3]), Some(edited_voxels(&chunk, &deltas)));
    }

    /// Past `BrickEditPolicy::inline_max_bricks` the bricks re-derive on the
    /// compute pool; the result still lands, and a newer edit supersedes an
    /// older one still running.
    #[test]
    fn set_blocks_off_thread_lands_the_latest_edit() {
        let (mut app, client) = app_shell();
        app.insert_resource(crate::renderer::bricks::BrickEditPolicy { inline_max_bricks: 0 });
        let (update_send, update_recv) = crossbeam::channel::unbounded();
        let (data, key, _) = chunk_snapshot();
        let chunk = data.ecs.chunk.get(key).clone();
        let region_id = RegionCoords::new(0, 0);
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        app.update();
        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, key)).expect("chunk mapped");
        assert!(wait_for(&mut app, |app| !bricks_of(app, e).is_empty()), "snapshot bricks never arrived");
        let (first, mut deltas) = set_blocks(key);
        let air = game::Block::new(game::BlockId::AIR);
        let second = game::BlockDelta { index: game::BlockIndex::from_linear(1), block: air, chisel: None };
        deltas.push(second.clone());
        update_send.send(first).unwrap();
        update_send
            .send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetBlocks(key, vec![second])))
            .unwrap();
        let want = edited_voxels(&chunk, &deltas);
        assert!(
            wait_for(&mut app, |app| brick_voxels(app, e, [0; 3]).as_ref() == Some(&want)
                && !app.world().entity(e).contains::<crate::renderer::bricks::BrickEdits>()),
            "the off-thread edit never landed"
        );
    }

    /// An edit that beats the off-thread snapshot voxels is buffered and laid
    /// over them when they land, not lost.
    #[test]
    fn set_blocks_before_the_snapshot_voxels_is_applied_on_top() {
        let (mut app, client) = app_shell();
        let (update_send, update_recv) = crossbeam::channel::unbounded();
        let (data, key, _) = chunk_snapshot();
        let chunk = data.ecs.chunk.get(key).clone();
        let region_id = RegionCoords::new(0, 0);
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        let (update, deltas) = set_blocks(key);
        update_send.send(update).unwrap();
        app.update();
        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, key)).expect("chunk mapped");
        assert!(
            wait_for(&mut app, |app| !bricks_of(app, e).is_empty()),
            "snapshot bricks never arrived"
        );
        assert!(!app.world().entity(e).contains::<PendingBlockDeltas>());
        assert_eq!(brick_voxels(&app, e, [0; 3]), Some(edited_voxels(&chunk, &deltas)));
    }

    /// A seam edit dirties the brick across the seam in the OTHER sim chunk:
    /// its boundary zone toward the edit changed, and the grid finds it.
    #[test]
    fn edit_on_chunk_seam_remeshes_neighbour_brick() {
        use crate::renderer::meshing::queue::{mark_remesh_dirty, ChunkGrid, ChunkKey, FrameUploads, RemeshQueue};
        let (mut app, client) = app_shell();
        app.init_resource::<ChunkGrid>()
            .init_resource::<RemeshQueue>()
            .init_resource::<FrameUploads>()
            .add_systems(Update, mark_remesh_dirty);
        let (update_send, update_recv) = crossbeam::channel::unbounded();
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        let a = rb.create_mesh(game::ChunkCoords::new(0, 0, 0), game::Chunk::flat_floor(8));
        let b = rb.create_mesh(game::ChunkCoords::new(1, 0, 0), game::Chunk::flat_floor(8));
        let region_id = RegionCoords::new(0, 0);
        client.send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv)).unwrap();
        app.update();
        let entity = |app: &App, k| *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        let (ea, eb) = (entity(&app, a), entity(&app, b));
        assert!(
            wait_for(&mut app, |app| [ea, eb].iter().all(|&c| {
                let bricks = bricks_of(app, c);
                !bricks.is_empty() && bricks.iter().all(|&br| app.world().entity(br).contains::<ChunkKey>())
            })),
            "bricks never landed"
        );
        app.update();
        app.world_mut().resource_mut::<RemeshQueue>().dirty.clear();

        // The block of chunk `a` on its +x face, solid in a flat floor.
        let seam = game::BlockIndex::from_xyz(game::CHUNK_BLOCKS - 1, 0, 0);
        let air = game::Block::new(game::BlockId::AIR);
        update_send
            .send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetBlocks(a, vec![game::BlockDelta { index: seam, block: air, chisel: None }]),
            ))
            .unwrap();
        app.update();
        let neighbour = bricks_of(&app, eb)
            .into_iter()
            .find(|&br| app.world().get::<crate::renderer::bricks::BrickOf>(br).is_some_and(|o| o.local[0] == 0))
            .expect("b's -x brick");
        assert!(
            app.world().resource::<RemeshQueue>().dirty.contains_key(&neighbour),
            "the brick across the seam must remesh"
        );
    }

    /// The runtime spawn path (`spawn_chunk_safe`): `CreateEntity` then
    /// `AddTerrainChunk` for the same key make ONE sim entity whose bricks
    /// land; undo's `RemoveEntity` despawns it and its bricks.
    #[test]
    fn runtime_chunk_spawn_and_undo_despawn_its_bricks() {
        let (mut app, _client, updates, region_id) = test_app();
        app.update();
        let k = key(21);
        let pose = game::Pose::translation(game::CHUNK_UNITS as f32, 0.0, 0.0);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates
            .send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::AddTerrainChunk(k, pose, Box::new(game::Chunk::flat_floor(8))),
            ))
            .unwrap();
        app.update();
        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).expect("chunk mapped");
        assert!(wait_for(&mut app, |app| !bricks_of(app, e).is_empty()), "bricks never landed");
        let mut sims = app.world_mut().query_filtered::<&SimEntity, Without<crate::renderer::bricks::BrickOf>>();
        assert_eq!(sims.iter(app.world()).filter(|s| s.key == k).count(), 1, "one sim entity for the key");
        let bricks = bricks_of(&app, e);
        assert_eq!(bricks.len(), game::BRICKS_PER_CHUNK_EDGE.pow(2), "the floor's brick layer");
        for &br in &bricks {
            let of = app.world().get::<crate::renderer::bricks::BrickOf>(br).expect("BrickOf");
            assert_eq!(of.chunk, e);
            assert!(app.world().entity(br).contains::<VoxelData>());
            let x = app.world().get::<Transform>(br).unwrap().translation.x;
            assert_eq!(x, game::CHUNK_UNITS as f32 + (of.local[0] as usize * game::BRICK_VOXELS) as f32);
        }
        assert!(!app.world().entity(e).contains::<VoxelData>(), "the sim entity holds no voxels");

        updates.send(GameDataUpdate::new(GameDataTransactionKind::Undo, GameDataUpdateKind::RemoveEntity(k))).unwrap();
        app.update();
        assert!(app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).is_none());
        assert!(app.world().get_entity(e).is_err(), "sim entity despawned");
        for br in bricks {
            assert!(app.world().get_entity(br).is_err(), "brick despawned");
        }
    }

    /// `AddTerrainChunk` again for a chunk whose bricks already landed: the
    /// old bricks stay listed until the new ones replace them — none orphaned.
    #[test]
    fn re_added_chunk_replaces_its_bricks_without_orphans() {
        let (mut app, _client, updates, region_id) = test_app();
        app.update();
        let k = key(23);
        let add = |chunk: game::Chunk| {
            GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::AddTerrainChunk(k, game::Pose::identity(), Box::new(chunk)),
            )
        };
        updates.send(add(game::Chunk::flat_floor(8))).unwrap();
        app.update();
        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).expect("chunk mapped");
        assert!(wait_for(&mut app, |app| !bricks_of(app, e).is_empty()), "bricks never landed");
        let old = bricks_of(&app, e);
        let stone = game::Chunk::uniform(game::Block::new(game::BlockId(2)));
        updates.send(add(stone.clone())).unwrap();
        let want = VoxelData::new(stone.brick([0; 3]).voxels());
        assert!(wait_for(&mut app, |app| brick_voxels(app, e, [0; 3]).as_ref() == Some(&want)), "re-add never landed");
        for b in old {
            assert!(app.world().get_entity(b).is_err(), "old brick orphaned");
        }
        let mut owned = app.world_mut().query::<&crate::renderer::bricks::BrickOf>();
        let count = owned.iter(app.world()).filter(|o| o.chunk == e).count();
        assert_eq!(count, bricks_of(&app, e).len(), "every brick of the chunk is listed");
    }

    /// A terrain chunk is never interpolated, so a new pose snaps it — and
    /// its bricks, which are siblings, follow.
    #[test]
    fn chunk_pose_moves_its_bricks() {
        let (mut app, _client, updates, region_id) = test_app();
        app.update();
        let k = key(22);
        updates
            .send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::AddTerrainChunk(k, game::Pose::identity(), Box::new(game::Chunk::flat_floor(8))),
            ))
            .unwrap();
        app.update();
        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).expect("chunk mapped");
        assert!(wait_for(&mut app, |app| !bricks_of(app, e).is_empty()), "bricks never landed");
        let to = Vec3::new(0.0, game::CHUNK_UNITS as f32, 0.0);
        updates
            .send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetEntityPosition(k, game::Pose::translation(to.x, to.y, to.z)),
            ))
            .unwrap();
        app.update();
        assert_eq!(app.world().get::<Transform>(e).unwrap().translation, to);
        for br in bricks_of(&app, e) {
            let local = app.world().get::<crate::renderer::bricks::BrickOf>(br).unwrap().local;
            let want = crate::renderer::bricks::brick_translation(to, local);
            assert_eq!(app.world().get::<Transform>(br).unwrap().translation, want);
        }
    }

    #[test]
    fn region_root_sits_at_world_offset() {
        let (mut app, client) = app_shell();
        let (_send, update_recv) = crossbeam::channel::unbounded();
        let region_id = game::RegionCoords::new(1, -2);
        let rb = Rollback::new(None);
        client
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv))
            .unwrap();
        app.update();
        let root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();
        let tf = app.world().entity(root).get::<Transform>().unwrap();
        assert_eq!(tf.translation, Vec3::new(game::REGION_SIZE, 0.0, -2.0 * game::REGION_SIZE));
    }

    #[test]
    fn debug_lines_populate_replace_and_teardown() {
        let (mut app, send) = app_shell();
        let id = RegionCoords::new(0, 0);
        let line = game::DebugLine {
            a: [0.0, 0.0, 0.0],
            b: [1.0, 0.0, 0.0],
            color: [30.0, 1.0, 0.4, 1.0],
        };
        send.send(ClientUpdateEvent::DebugLines(id, vec![line])).unwrap();
        app.update();
        let lines = &app.world().resource::<crate::renderer::collider_debug::DebugColliderLines>().0;
        assert_eq!(lines.get(&id).map(|v| v.len()), Some(1));

        // Replace-on-receipt: an empty buffer overwrites, not appends.
        send.send(ClientUpdateEvent::DebugLines(id, vec![])).unwrap();
        app.update();
        let lines = &app.world().resource::<crate::renderer::collider_debug::DebugColliderLines>().0;
        assert_eq!(lines.get(&id).map(|v| v.len()), Some(0));

        // RemoveRegion drops the entry entirely.
        send.send(ClientUpdateEvent::RemoveRegion(id)).unwrap();
        app.update();
        let lines = &app.world().resource::<crate::renderer::collider_debug::DebugColliderLines>().0;
        assert!(lines.get(&id).is_none());
    }

    #[test]
    fn remove_region_tears_down_root_maps_and_receiver() {
        let (mut app, client, _updates, region_id) = test_app();
        app.update();
        let root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();

        client.send(ClientUpdateEvent::RemoveRegion(region_id)).unwrap();
        app.update();

        assert!(app.world().get_entity(root).is_err(), "root despawned (with children)");
        assert!(!app.world().resource::<RegionRoots>().0.contains_key(&region_id));
        assert!(!app.world().resource::<Regions>().0.contains_key(&region_id));
        assert!(app.world().resource::<SimEntityMap>().0.keys().all(|(r, _)| *r != region_id));
    }

    /// A window shift releases a row of regions at once; their trees go one
    /// per frame, and a region re-received meanwhile loses its old tree first.
    #[test]
    fn released_regions_despawn_one_per_frame_and_rereceipt_flushes() {
        let (mut app, client, _updates, a) = test_app();
        let b = RegionCoords::new(1, 0);
        let c = RegionCoords::new(2, 0);
        for id in [b, c] {
            let (_send, recv) = crossbeam::channel::unbounded();
            let rb = Rollback::new(None);
            client.send(ClientUpdateEvent::NewRegion(id, (*rb.data).clone(), recv)).unwrap();
        }
        // One tree spawns per frame.
        for _ in 0..3 {
            app.update();
        }
        let root = |app: &App, id| *app.world().resource::<RegionRoots>().0.get(&id).unwrap();
        let (ra, rb_, rc) = (root(&app, a), root(&app, b), root(&app, c));

        for id in [a, b, c] {
            client.send(ClientUpdateEvent::RemoveRegion(id)).unwrap();
        }
        app.update();
        assert!(app.world().resource::<RegionRoots>().0.is_empty(), "unmapped at once");
        assert!(app.world().get_entity(ra).is_err(), "first tree despawned this frame");
        assert!(app.world().get_entity(rb_).is_ok(), "the rest wait their frame");

        // c comes straight back before its old tree's turn.
        let (_send, recv) = crossbeam::channel::unbounded();
        let rb = Rollback::new(None);
        client.send(ClientUpdateEvent::NewRegion(c, (*rb.data).clone(), recv)).unwrap();
        app.update();
        assert!(app.world().get_entity(rc).is_err(), "old tree flushed by the re-receipt");
        assert!(app.world().get_entity(rb_).is_err(), "b's turn came");
        assert_ne!(root(&app, c), rc);
    }

    #[test]
    fn new_region_for_loaded_region_replaces_it() {
        let (mut app, client, _updates, region_id) = test_app();
        app.update();
        let first_root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();

        let (_send2, update_recv2) = crossbeam::channel::unbounded();
        let rb = Rollback::new(None);
        client
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv2))
            .unwrap();
        app.update();

        let second_root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();
        assert_ne!(first_root, second_root, "old root replaced");
        assert!(app.world().get_entity(first_root).is_err(), "old root despawned");
    }

    #[test]
    fn snapshot_spawns_camera_only_for_local_player() {
        let (mut app, client) = app_shell();
        let (_update_send, update_recv) = crossbeam::channel::unbounded();

        // Region snapshot that already contains two players (the join flow).
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        rb.create_player_safe(0);
        rb.create_player_safe(1);
        let data = (*rb.data).clone();
        let k0 = *data.player_entites.get(&0).unwrap();
        let k1 = *data.player_entites.get(&1).unwrap();

        let region_id = RegionCoords::new(0, 0);
        // SetPlayer always precedes NewRegion (PlayerRegion precedes Region).
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        app.update();

        let map = app.world().resource::<SimEntityMap>();
        let e0 = *map.0.get(&(region_id, k0)).unwrap();
        let e1 = *map.0.get(&(region_id, k1)).unwrap();
        assert!(app.world().entity(e0).contains::<Camera3d>(), "local player's camera is active");
        assert!(
            !app.world().entity(e1).contains::<Camera3d>(),
            "other player's camera from the snapshot must not be active"
        );
    }

    #[test]
    fn halo_mirror_drawn_once_per_entity_id() {
        let (mut app, client, updates, region_id) = test_app();
        app.add_systems(Update, dedupe_halo_entities);
        // A second host of the same entity's mirror (a corner entity).
        let other_host = RegionCoords::new(0, 1);
        let (updates2, recv2) = crossbeam::channel::unbounded();
        client.send(ClientUpdateEvent::NewRegion(other_host, (*Rollback::new(None).data).clone(), recv2)).unwrap();
        app.update();

        let k = key(11);
        let src = RegionCoords::new(1, 0);
        let id = game::EntityId { birth_region: src, serial: 3 };
        for u in [&updates, &updates2] {
            u.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
            u.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetHaloSource(k, Some((src, id)))))
                .unwrap();
        }
        app.update();
        app.update(); // component insert lands a frame after spawn-in-same-drain

        let map = app.world().resource::<SimEntityMap>();
        let e = *map.0.get(&(region_id, k)).unwrap();
        let e2 = *map.0.get(&(other_host, k)).unwrap();
        let vis = |app: &App, e: Entity| *app.world().entity(e).get::<Visibility>().unwrap();
        // Source region NOT loaded: one mirror draws — the lowest host's.
        assert_ne!(vis(&app, e), Visibility::Hidden, "the mirror renders while its source region is absent");
        assert_eq!(vis(&app, e2), Visibility::Hidden, "a second mirror of the same id hides");

        // Load the source region: the owned copy renders there; both mirrors hide.
        let (_s3, recv3) = crossbeam::channel::unbounded();
        client.send(ClientUpdateEvent::NewRegion(src, (*Rollback::new(None).data).clone(), recv3)).unwrap();
        app.update();
        app.update();
        assert_eq!(vis(&app, e), Visibility::Hidden, "the mirror hides when its source region loads");
        assert_eq!(vis(&app, e2), Visibility::Hidden);

        // Unload the source: one mirror again.
        client.send(ClientUpdateEvent::RemoveRegion(src)).unwrap();
        app.update();
        app.update();
        assert_ne!(vis(&app, e), Visibility::Hidden);
        assert_eq!(vis(&app, e2), Visibility::Hidden);

        // Upgrade clears the mark: never hidden again, even with the old
        // source loaded.
        updates
            .send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetHaloSource(k, None)))
            .unwrap();
        client
            .send(ClientUpdateEvent::NewRegion(src, (*Rollback::new(None).data).clone(), {
                let (_s4, r4) = crossbeam::channel::unbounded();
                r4
            }))
            .unwrap();
        app.update();
        app.update();
        assert_ne!(vis(&app, e), Visibility::Hidden, "upgraded (owned) entity is never deduped");
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn body_camera_yields_to_the_frontend() {
        use crate::session::{AppState, InPlay, SessionPlugin};
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin, SessionPlugin { skip_menu: false, login_first: false }))
            .init_resource::<HudStatus>()
            .add_systems(PreUpdate, enforce_single_active_camera);
        let cam = app
            .world_mut()
            .spawn((
                SimEntity { region: RegionCoords::new(0, 0), key: key(1) },
                Camera3d::default(),
                Camera { is_active: true, ..default() },
                crate::renderer::UiCamera,
            ))
            .id();
        let state = |app: &App| {
            let e = app.world().entity(cam);
            (e.get::<Camera>().unwrap().is_active, e.contains::<crate::renderer::UiCamera>())
        };

        app.update();
        assert_eq!(state(&app), (false, false), "menu: the body camera neither renders nor has the UI");

        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::InWorld);
        app.update();
        app.update();
        assert_eq!(state(&app), (false, false), "in world but not in_play: still the frontend's");

        app.world_mut().resource_mut::<InPlay>().0 = true;
        app.update();
        assert_eq!(state(&app), (true, true), "in_play: the body camera has the screen again");
    }

    #[test]
    fn only_home_region_camera_is_active_across_regions() {
        use game::ConnectionState;

        let (mut app, client) = app_shell();
        app.add_systems(
            PreUpdate,
            enforce_single_active_camera.after(drain_region_updates),
        );

        // Two loaded regions that BOTH have the local player owning a camera —
        // the transient dual-ownership a boundary handoff produces.
        let region_a = RegionCoords::new(0, 0);
        let region_b = RegionCoords::new(1, 0);

        let mut rb_a = Rollback::new(None);
        rb_a.new_transaction();
        rb_a.create_player_safe(0);
        let data_a = (*rb_a.data).clone();
        let ka = *data_a.player_entites.get(&0).unwrap();

        let mut rb_b = Rollback::new(None);
        rb_b.new_transaction();
        rb_b.create_player_safe(0);
        let data_b = (*rb_b.data).clone();
        let kb = *data_b.player_entites.get(&0).unwrap();

        let (_sa, ra) = crossbeam::channel::unbounded();
        let (_sb, rbr) = crossbeam::channel::unbounded();
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_a, data_a, ra)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_b, data_b, rbr)).unwrap();
        client
            .send(ClientUpdateEvent::HudStatus {
                home_region: region_a,
                viewer_region: region_a,
                connection: ConnectionState::Ready,
            })
            .unwrap();
        // One region tree spawns per frame; a frame more for the second.
        app.update();
        app.update();
        app.update();

        let map = app.world().resource::<SimEntityMap>();
        let ea = *map.0.get(&(region_a, ka)).unwrap();
        let eb = *map.0.get(&(region_b, kb)).unwrap();

        assert!(
            app.world().entity(ea).get::<Camera>().unwrap().is_active,
            "home-region camera must be the active one"
        );
        assert!(
            !app.world().entity(eb).get::<Camera>().unwrap().is_active,
            "non-home-region camera must be inactive"
        );
        assert!(
            app.world().entity(ea).contains::<crate::renderer::UiCamera>(),
            "home-region camera must be the default UI camera"
        );
        assert!(
            !app.world().entity(eb).contains::<crate::renderer::UiCamera>(),
            "non-home-region camera must not keep the default UI camera marker"
        );
    }

    #[test]
    fn active_camera_follows_home_region_flip() {
        use game::ConnectionState;

        let (mut app, client) = app_shell();
        app.add_systems(
            PreUpdate,
            enforce_single_active_camera.after(drain_region_updates),
        );

        let region_a = RegionCoords::new(0, 0);
        let region_b = RegionCoords::new(1, 0);
        let mut rb_a = Rollback::new(None);
        rb_a.new_transaction();
        rb_a.create_player_safe(0);
        let data_a = (*rb_a.data).clone();
        let ka = *data_a.player_entites.get(&0).unwrap();
        let mut rb_b = Rollback::new(None);
        rb_b.new_transaction();
        rb_b.create_player_safe(0);
        let data_b = (*rb_b.data).clone();
        let kb = *data_b.player_entites.get(&0).unwrap();

        let (_sa, ra) = crossbeam::channel::unbounded();
        let (_sb, rbr) = crossbeam::channel::unbounded();
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_a, data_a, ra)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_b, data_b, rbr)).unwrap();
        let hud = |home| ClientUpdateEvent::HudStatus {
            home_region: home,
            viewer_region: home,
            connection: ConnectionState::Ready,
        };
        client.send(hud(region_a)).unwrap();
        // One region tree spawns per frame; a frame more for the second.
        app.update();
        app.update();
        app.update();

        let (ea, eb) = {
            let map = app.world().resource::<SimEntityMap>();
            (*map.0.get(&(region_a, ka)).unwrap(), *map.0.get(&(region_b, kb)).unwrap())
        };
        assert!(app.world().entity(ea).get::<Camera>().unwrap().is_active);
        assert!(!app.world().entity(eb).get::<Camera>().unwrap().is_active);

        // Cross into region_b: home flips. The region_b camera — whose
        // `Camera3d` was never removed, only deactivated — must reactivate,
        // and region_a's must go inactive. No AddCameraComponent is replayed.
        client.send(hud(region_b)).unwrap();
        app.update();

        assert!(
            !app.world().entity(ea).get::<Camera>().unwrap().is_active,
            "old home camera deactivates after the flip"
        );
        assert!(
            app.world().entity(eb).get::<Camera>().unwrap().is_active,
            "new home camera reactivates after the flip"
        );
        assert!(app.world().entity(eb).contains::<crate::renderer::UiCamera>());
        assert!(!app.world().entity(ea).contains::<crate::renderer::UiCamera>());
    }

    #[test]
    fn drain_client_updates_writes_hud_status() {
        use game::ConnectionState;

        let (client, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(ClientUpdates(client_recv))
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<RetiredRegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<HudStatus>()
            .init_resource::<crate::renderer::slint_overlay::UiUpdates>()
            .init_resource::<crate::renderer::collider_debug::DebugColliderLines>()
            .init_resource::<lod_rings::LodRings>()
            .init_resource::<SnapshotVoxelQueue>()
            .add_systems(Update, drain_client_updates);

        client
            .send(ClientUpdateEvent::HudStatus {
                home_region: RegionCoords::new(2, -1),
                viewer_region: RegionCoords::new(2, 0),
                connection: ConnectionState::Ready,
            })
            .unwrap();

        app.update();

        let status = app.world().resource::<HudStatus>();
        assert_eq!(status.home_region, Some(RegionCoords::new(2, -1)));
        assert_eq!(status.viewer_region, Some(RegionCoords::new(2, 0)));
        assert_eq!(status.connection, ConnectionState::Ready);
    }
}

#[cfg(test)]
mod voxel_data_tests {
    use super::VoxelData;
    use game::{BlockId, Voxel, BRICK_VOXEL_COUNT};

    fn dense(f: impl Fn(usize) -> u16) -> Vec<Voxel> {
        (0..BRICK_VOXEL_COUNT).map(|i| Voxel::new(BlockId(f(i)))).collect()
    }

    /// The contract the mesher depends on: decode(new(v)) == v, bit for bit.
    #[test]
    fn round_trips_exactly() {
        for case in [
            dense(|_| 0),                                   // all air: 1 run
            dense(|_| 7),                                   // uniform solid
            dense(|i| (i % 2) as u16),                      // worst case: no runs
            dense(|i| ((i / 16) % 3) as u16),               // block-aligned runs
            dense(|i| if i == 12345 { 4 } else { 0 }),      // single chiselled voxel
        ] {
            let encoded = VoxelData::new(&case);
            assert_eq!(encoded.decode(), case, "RLE round trip must be exact");
        }
    }

    /// Uniform chunks — the overwhelmingly common case — must collapse to one
    /// run. This is where the ~183 MB/region saving actually comes from.
    #[test]
    fn uniform_chunk_is_one_run() {
        assert_eq!(VoxelData::new(dense(|_| 0)).runs(), 1);
        assert_eq!(VoxelData::new(dense(|_| 9)).runs(), 1);
    }

    /// A chunk of 8 uniform 16³ blocks must never need more than 32 runs per
    /// row of 32 voxels — runs are >= 16 by construction, so the encoding can
    /// not silently degenerate to the dense size for ordinary terrain.
    #[test]
    fn block_aligned_chunk_stays_compact() {
        // Two materials split at the x=16 block boundary.
        let v = dense(|i| if i % 32 < 16 { 1 } else { 2 });
        let runs = VoxelData::new(&v).runs();
        assert_eq!(runs, 2048, "one run per 16-voxel half-row");
        assert!(runs * 4 < BRICK_VOXEL_COUNT * 2, "still smaller than the dense form");
    }

    /// edit() must decode, apply, and re-encode — the path chisels take.
    #[test]
    fn edit_round_trips() {
        let mut d = VoxelData::new(dense(|_| 0));
        assert_eq!(d.runs(), 1);
        d.edit(|v| v[100] = Voxel::new(BlockId(3)));
        let out = d.decode();
        assert_eq!(out[100], Voxel::new(BlockId(3)));
        assert_eq!(out[99], Voxel::new(BlockId(0)));
        assert_eq!(out.len(), BRICK_VOXEL_COUNT);
        assert_eq!(d.runs(), 3, "air | one voxel | air");
    }

    #[test]
    fn sorted_index_visit_reads_rle_without_dense_decode() {
        let dense = dense(|i| (i % 11) as u16);
        let data = VoxelData::new(&dense);
        let indices = [0usize, 1, 31, 32, 1024, 8191, BRICK_VOXEL_COUNT - 1];
        let mut got = Vec::new();

        data.for_each_index_sorted(indices, |index, voxel| got.push((index, voxel)));

        let expected = indices.into_iter().map(|i| (i, dense[i])).collect::<Vec<_>>();
        assert_eq!(got, expected);
    }
}
