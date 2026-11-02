//! Sim chunk -> render brick fan-out. A sim chunk is `CHUNK_BLOCKS`³ blocks;
//! the client meshes and draws `BRICK_VOXELS`³-voxel bricks, each its own
//! entity, so the chunk grid, neighbour gather and remesh queue see bricks
//! exactly as they saw chunks, whatever the sim-chunk size.
//!
//! Shape: the sim chunk entity keeps `SimEntity` (and its pose) and gets a
//! [`ChunkBricks`] list plus a [`SimChunk`] mirror of its blocks; it holds no
//! `VoxelData` / `ChunkKey` and is never interpolated. Each brick is a
//! `ChildOf(region root)` sibling at the region-local chunk origin plus
//! `local * BRICK_VOXELS`, with [`BrickOf`], `VoxelData` and (stamped by
//! `mark_remesh_dirty`) a `ChunkKey`. The commander's height grid and tile
//! mirror key by the sim chunk entity and read [`SimChunk`], never bricks.
//!
//! All-air bricks and buried bricks are never spawned ([`spawn_chunk_bricks`]).
//! Buried means opaque with no air across any face: inside the chunk, and —
//! for a region snapshot, which knows every chunk at once — across the
//! chunk's faces too, against the next chunk or the implied ground under the
//! band ([`snapshot_across`]); that roughly halves a join's brick entities.
//! An edit that needs one ([`apply_block_deltas`]) derives it from the
//! mirror, which is why the client keeps the blocks; a brick buried against
//! another chunk is spawned when that chunk lands, is edited, cleared or
//! removed with air against it ([`expose_across`], [`SimChunkIndex`]).

use std::collections::BTreeSet;

use bevy::prelude::*;
use bevy::tasks::Task;
use game::{BlockDelta, BlockId, BlockIndex, Chunk, Voxel, BRICKS_PER_CHUNK_EDGE, BRICK_BLOCKS, BRICK_VOXELS};

use super::bridge::{PendingBlockDeltas, PendingSnapshotVoxels, VoxelData};
use super::meshing::binary::CutoutTable;
use super::meshing::queue::presents_opaque_face;

/// The render bricks of a sim chunk entity. Present (maybe empty) on every
/// terrain chunk entity from the moment it is spawned: it is also how the
/// bridge and the interpolator tell a terrain chunk from a body.
#[derive(Component, Default, Debug)]
pub struct ChunkBricks(pub Vec<Entity>);

/// A render brick: which sim chunk entity it belongs to, and where in it
/// (each axis `0..BRICKS_PER_CHUNK_EDGE`).
#[derive(Component, Clone, Copy, Debug)]
pub struct BrickOf {
    pub chunk: Entity,
    pub local: [u8; 3],
}

/// The client's copy of a sim chunk's blocks, kept in step with `SetBlocks`.
/// Bricks are re-derived from it on an edit, and skipped (air / buried)
/// bricks materialised from it when an edit exposes them.
#[derive(Component, Debug)]
pub struct SimChunk(pub Chunk);

/// What changed in a chunk's [`SimChunk`] since the commander's tile mirror
/// last looked: `full` for a fresh/replaced mirror, else the edited blocks.
/// The mirror consumes (clears) it.
#[derive(Component, Default, Debug)]
pub struct ChunkEdits {
    pub full: bool,
    pub blocks: BTreeSet<BlockIndex>,
}

/// Bricks edited since their voxels were last written, and the off-thread
/// re-derivation covering ALL of them, if one is running. A newer edit
/// replaces the task (dropping a `Task` cancels it), so a result that lands
/// is always derived from the latest mirror.
#[derive(Component, Default)]
pub struct BrickEdits {
    dirty: BTreeSet<[u8; 3]>,
    task: Option<Task<Vec<([u8; 3], VoxelData)>>>,
}

/// Edits touching at most this many bricks re-derive them on the main thread
/// (one brick is ~a few µs from blocks); more go to the compute pool.
pub const INLINE_BRICK_EDITS: usize = 1;

/// Override of [`INLINE_BRICK_EDITS`] (tests force the off-thread path with 0).
#[derive(Resource, Clone, Copy, Debug)]
pub struct BrickEditPolicy {
    pub inline_max_bricks: usize,
}

impl Default for BrickEditPolicy {
    fn default() -> Self {
        Self { inline_max_bricks: INLINE_BRICK_EDITS }
    }
}

fn to_local(b: [usize; 3]) -> [u8; 3] {
    b.map(|v| v as u8)
}

/// An edited chunk's dirty bricks to re-derive (a compute job,
/// `renderer::compute`).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct BrickEditsJob {
    pub source: Chunk,
    pub dirty: BTreeSet<[u8; 3]>,
}

impl super::compute::ComputeJob for BrickEditsJob {
    type Output = Vec<([u8; 3], VoxelData)>;
    const KIND: super::compute::JobKind = super::compute::JobKind::BrickEdits;

    fn run(self) -> Self::Output {
        self.dirty
            .into_iter()
            .map(|l| (l, VoxelData::from_brick(&self.source.brick(to_brick(l))).with_boundary_zones()))
            .collect()
    }
}

fn to_brick(local: [u8; 3]) -> [usize; 3] {
    local.map(usize::from)
}

/// Linear index of brick `b` in `Chunk::bricks()` order (x fastest).
fn brick_linear(b: [usize; 3]) -> usize {
    let n = BRICKS_PER_CHUNK_EDGE;
    b[0] + n * (b[1] + n * b[2])
}

/// A chunk's bricks, split by what the client does with them.
#[derive(Default, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DerivedBricks {
    /// Spawned as brick entities.
    pub spawn: Vec<([u8; 3], VoxelData)>,
    /// Buried inside the chunk: no entity, but recorded in [`SkippedBricks`]
    /// so their neighbours read them as the solid they are.
    pub buried: Vec<([u8; 3], VoxelData)>,
}

/// The render bricks of `chunk`: every brick that is neither all air nor
/// buried inside the chunk (no air, and every face neighbour brick IN THE
/// CHUNK presents no air across the shared face — `is_buried`, per brick; a
/// brick on the chunk's face borders an unknown chunk, so it always stays).
/// A `Uniform` chunk produces its list without per-voxel work. Runs off the
/// main thread (`SnapshotVoxelTask`).
pub fn spawn_chunk_bricks(chunk: &Chunk) -> Vec<([u8; 3], VoxelData)> {
    derive_chunk_bricks(chunk).spawn
}

/// [`spawn_chunk_bricks`] plus the buried bricks it skips.
pub fn derive_chunk_bricks(chunk: &Chunk) -> DerivedBricks {
    derive_chunk_bricks_across(chunk, |_, _| false)
}

/// The six face offsets, in the order burial checks them.
const FACES: [[i32; 3]; 6] = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]];

/// [`derive_chunk_bricks`] where a brick on the chunk's face may be buried
/// too: `across(b, off)` says whether whatever lies past brick `b`'s face
/// `off` — outside this chunk — presents no air there (see
/// [`snapshot_across`]). A region snapshot answers that for every chunk at
/// once; about half of a join's face bricks are buried by the chunk (or the
/// implied ground) next to them (2026-11-02 census, 256 m regions).
pub fn derive_chunk_bricks_across(chunk: &Chunk, across: impl Fn([usize; 3], [i32; 3]) -> bool) -> DerivedBricks {
    let n = BRICKS_PER_CHUNK_EDGE;
    let mut out = DerivedBricks::default();
    let cutout = CutoutTable::installed();
    let outside = |b: [usize; 3], off: [i32; 3]| (0..3).any(|a| (b[a] as i32 + off[a]) < 0 || (b[a] as i32 + off[a]) >= n as i32);
    if let (Some(block), true) = (chunk.blocks.uniform(), chunk.chisel.is_empty()) {
        if block.id == BlockId::AIR {
            return out;
        }
        // A see-through (cutout) chunk buries nothing; inside an opaque one
        // only the faces past the chunk can show.
        let see_through = cutout.is_cutout(block.id);
        let shows = |b: [usize; 3]| see_through || FACES.iter().any(|&off| outside(b, off) && !across(b, off));
        for (b, _) in chunk.bricks() {
            let list = if shows(b) { &mut out.spawn } else { &mut out.buried };
            list.push((to_local(b), VoxelData::uniform(block.id)));
        }
        return out;
    }
    let all: Vec<VoxelData> = chunk.bricks().map(|(_, view)| VoxelData::from_brick(&view)).collect();
    let at = |b: [usize; 3], off: [i32; 3]| -> Option<&VoxelData> {
        let mut nb = [0usize; 3];
        for a in 0..3 {
            let v = b[a] as i32 + off[a];
            if v < 0 || v >= n as i32 {
                return None;
            }
            nb[a] = v as usize;
        }
        Some(&all[brick_linear(nb)])
    };
    // 0 = air (dropped), 1 = spawn, 2 = buried.
    let kind: Vec<u8> = chunk
        .bricks()
        .map(|(b, _)| {
            let voxels = &all[brick_linear(b)];
            let buried = voxels.is_opaque_throughout(&cutout)
                && FACES.iter().all(|&off| match at(b, off) {
                    Some(neighbor) => presents_opaque_face(neighbor, off, &cutout),
                    None => across(b, off),
                });
            if voxels.is_all_air() {
                0
            } else if buried {
                2
            } else {
                1
            }
        })
        .collect();
    for (((b, _), voxels), kind) in chunk.bricks().zip(all).zip(kind) {
        match kind {
            1 => out.spawn.push((to_local(b), voxels)),
            2 => out.buried.push((to_local(b), voxels)),
            _ => {}
        }
    }
    out
}

/// The chunk-local block coords of brick `b`'s face plane toward `off`
/// (one non-zero axis), on the chunk's block layer `layer` along that axis.
fn plane_blocks(b: [usize; 3], off: [i32; 3], layer: usize) -> impl Iterator<Item = [usize; 3]> {
    let axis = (0..3).find(|&a| off[a] != 0).expect("a face offset");
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    (0..BRICK_BLOCKS * BRICK_BLOCKS).map(move |i| {
        let mut p = [0; 3];
        p[axis] = layer;
        p[u] = b[u] * BRICK_BLOCKS + i % BRICK_BLOCKS;
        p[v] = b[v] * BRICK_BLOCKS + i / BRICK_BLOCKS;
        p
    })
}

/// The chunk block layer brick faces toward `off` lie on: the chunk's own
/// face (`own`), or the neighbour chunk's facing layer.
fn face_layer(off: [i32; 3], own: bool) -> usize {
    let positive = off.iter().sum::<i32>() > 0;
    if positive == own { game::CHUNK_BLOCKS - 1 } else { 0 }
}

/// An unchiselled, non-air, non-cutout block: hides the face beside it. A
/// chiselled block may hold air, so it never counts.
fn block_opaque(block: game::Block, chiselled: bool, cutout: &CutoutTable) -> bool {
    !chiselled && block.id != BlockId::AIR && !cutout.is_cutout(block.id)
}

/// Whether every block of `chunk` at `blocks` hides the face beside it.
fn chunk_blocks_opaque(chunk: &Chunk, mut blocks: impl Iterator<Item = [usize; 3]>, cutout: &CutoutTable) -> bool {
    blocks.all(|[x, y, z]| {
        let i = BlockIndex::from_xyz(x, y, z);
        block_opaque(chunk.blocks.get(i), chunk.chisel.contains_key(&i), cutout)
    })
}

/// The `across` test of [`derive_chunk_bricks_across`] for the chunk at
/// `coords` in a region snapshot: past the chunk's face is either another
/// chunk of the snapshot, the column's implied ground (solid rock below its
/// band), or air. Past the region's edge is unknown: never buries.
pub fn snapshot_across<'a>(
    data: &'a game::GameData,
    coords: game::ChunkCoords,
    cutout: &'a CutoutTable,
) -> impl Fn([usize; 3], [i32; 3]) -> bool + 'a {
    across_with(
        move |next| {
            data.chunk_index
                .get(&game::chunk_index_key(next))
                .map(|&key| data.ecs.chunk.try_get(key).as_ref())
        },
        move |column| data.ground.get(&column),
        coords,
        cutout,
    )
}

/// The chunks and ground a batch of a snapshot's chunks reads to derive its
/// bricks ([`snapshot_across`]): the chunks themselves, their face
/// neighbours inside the region, and the ground of those columns — what a
/// compute worker gets instead of the whole snapshot.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SnapshotPart {
    /// `None`: indexed, but absent from the snapshot (never opaque).
    pub chunks: std::collections::BTreeMap<game::ChunkCoords, Option<Chunk>>,
    pub ground: std::collections::BTreeMap<game::ColumnCoords, game::ColumnGround>,
}

impl SnapshotPart {
    /// What deriving the chunks at `items` reads from `data`.
    pub fn gather(data: &game::GameData, items: impl IntoIterator<Item = game::ChunkCoords>) -> Self {
        let n = game::REGION_CHUNKS as i64;
        let mut part = SnapshotPart::default();
        for coords in items {
            for off in std::iter::once([0, 0, 0]).chain(FACES) {
                let (x, z) = (coords.x as i64 + off[0] as i64, coords.z as i64 + off[2] as i64);
                if !(0..n).contains(&x) || !(0..n).contains(&z) {
                    continue;
                }
                let next = game::ChunkCoords::new(x as usize, coords.y + off[1], z as usize);
                if part.chunks.contains_key(&next) {
                    continue;
                }
                match data.chunk_index.get(&game::chunk_index_key(next)) {
                    Some(&key) => {
                        part.chunks.insert(next, data.ecs.chunk.try_get(key).as_ref().cloned());
                    }
                    None => {
                        let column = game::ColumnCoords::new(next.x, next.z);
                        if let Some(ground) = data.ground.get(&column) {
                            part.ground.entry(column).or_insert_with(|| ground.clone());
                        }
                    }
                }
            }
        }
        part
    }

    /// [`snapshot_across`] over this part.
    pub fn across<'a>(&'a self, coords: game::ChunkCoords, cutout: &'a CutoutTable) -> impl Fn([usize; 3], [i32; 3]) -> bool + 'a {
        across_with(move |next| self.chunks.get(&next).map(Option::as_ref), move |column| self.ground.get(&column), coords, cutout)
    }
}

/// [`snapshot_across`] over any source: `chunk(next)` is `None` where the
/// snapshot has no chunk (implied ground or air: `ground(column)` decides),
/// `Some(None)` for an indexed chunk that is absent.
fn across_with<'a>(
    chunk: impl Fn(game::ChunkCoords) -> Option<Option<&'a Chunk>> + 'a,
    ground: impl Fn(game::ColumnCoords) -> Option<&'a game::ColumnGround> + 'a,
    coords: game::ChunkCoords,
    cutout: &'a CutoutTable,
) -> impl Fn([usize; 3], [i32; 3]) -> bool + 'a {
    move |b, off| {
        let n = game::REGION_CHUNKS as i64;
        let (x, z) = (coords.x as i64 + off[0] as i64, coords.z as i64 + off[2] as i64);
        if !(0..n).contains(&x) || !(0..n).contains(&z) {
            return false;
        }
        let next = game::ChunkCoords::new(x as usize, coords.y + off[1], z as usize);
        let plane = plane_blocks(b, off, face_layer(off, false));
        match chunk(next) {
            Some(found) => found.is_some_and(|chunk| chunk_blocks_opaque(chunk, plane, cutout)),
            None => {
                let ground = ground(game::ColumnCoords::new(next.x, next.z));
                plane.into_iter().all(|[x, y, z]| {
                    game::implied_block(ground, next, (x, y, z)).is_some_and(|block| block_opaque(block, false, cutout))
                })
            }
        }
    }
}

/// Landed terrain chunk entities by world sim-chunk key (region root +
/// chunk origin, over `CHUNK_UNITS`). Lets a chunk find the neighbour its
/// face bricks were buried against. Entries go with the chunk's despawn or
/// its region.
#[derive(Resource, Default, Debug)]
pub struct SimChunkIndex(pub std::collections::HashMap<[i32; 3], Entity>);

/// World sim-chunk key of `chunk` (see [`SimChunkIndex`]).
fn chunk_world_key(world: &World, chunk: Entity) -> Option<[i32; 3]> {
    let per = BRICKS_PER_CHUNK_EDGE as i32;
    brick_grid_key(world, chunk, [0; 3]).map(|k| k.map(|v| v.div_euclid(per)))
}

/// Every (brick, offset) pair of a chunk whose face lies on the chunk's own
/// face, i.e. borders another chunk.
fn boundary_faces() -> impl Iterator<Item = ([usize; 3], [i32; 3])> {
    let n = BRICKS_PER_CHUNK_EDGE;
    (0..n * n * n).flat_map(move |i| {
        let b = [i % n, (i / n) % n, i / (n * n)];
        FACES.into_iter().filter_map(move |off| {
            let a = (0..3).find(|&a| off[a] != 0).unwrap();
            let edge = if off[a] > 0 { n - 1 } else { 0 };
            (b[a] == edge).then_some((b, off))
        })
    })
}

/// Spawn the skipped brick at grid key `key` if a chunk other than `chunk`
/// owns it: something next to it no longer hides its face.
fn expose_skipped(world: &mut World, key: [i32; 3], chunk: Entity) {
    let Some((owner, voxels)) = world
        .get_resource::<SkippedBricks>()
        .and_then(|s| s.0.get(&key))
        .filter(|(owner, _)| *owner != chunk)
        .cloned()
    else {
        return;
    };
    let Some(base) = brick_grid_key(world, owner, [0; 3]) else { return };
    let local = [0, 1, 2].map(|a| (key[a] - base[a]) as u8);
    write_brick(world, owner, local, voxels);
}

/// After `chunk` changed (landed, cleared, removed): spawn every brick of a
/// neighbouring chunk that was buried against one of its faces which no
/// longer hides it — every face when `opaque` is `None` (the chunk is gone),
/// else the faces `opaque(b, off)` says show air.
fn expose_across(world: &mut World, chunk: Entity, opaque: Option<&dyn Fn([usize; 3], [i32; 3]) -> bool>) {
    let Some(base) = brick_grid_key(world, chunk, [0; 3]) else { return };
    if world.get_resource::<SkippedBricks>().is_none_or(|s| s.0.is_empty()) {
        return;
    }
    for (b, off) in boundary_faces() {
        let key = [0, 1, 2].map(|a| base[a] + b[a] as i32 + off[a]);
        let skipped = world.resource::<SkippedBricks>().0.get(&key).is_some_and(|(owner, _)| *owner != chunk);
        if skipped && !opaque.is_some_and(|f| f(b, off)) {
            expose_skipped(world, key, chunk);
        }
    }
}

/// [`expose_across`] from `chunk`'s mirror: the faces whose own blocks show air.
fn expose_across_mirror(world: &mut World, chunk: Entity) {
    let cutout = CutoutTable::installed();
    let Some(mirror) = world.get::<SimChunk>(chunk).map(|m| m.0.clone()) else {
        expose_across(world, chunk, None);
        return;
    };
    let opaque = |b: [usize; 3], off: [i32; 3]| chunk_blocks_opaque(&mirror, plane_blocks(b, off, face_layer(off, true)), &cutout);
    expose_across(world, chunk, Some(&opaque));
}

/// Keep only the buried bricks of a landing chunk whose cross-chunk faces are
/// still hidden by the neighbours that already LANDED: those may have been
/// edited since the snapshot the burial was decided on. A neighbour still
/// deriving checks this chunk when it lands ([`expose_across_mirror`]).
fn recheck_buried(world: &World, chunk: Entity, bricks: &mut DerivedBricks) {
    let n = BRICKS_PER_CHUNK_EDGE;
    let (Some(index), Some(key)) = (world.get_resource::<SimChunkIndex>(), chunk_world_key(world, chunk)) else {
        return;
    };
    let cutout = CutoutTable::installed();
    let mut shown = Vec::new();
    bricks.buried.retain(|(local, voxels)| {
        let b = to_brick(*local);
        let hidden = FACES.iter().all(|&off| {
            let a = (0..3).find(|&a| off[a] != 0).unwrap();
            if b[a] != if off[a] > 0 { n - 1 } else { 0 } {
                return true; // an in-chunk face, checked by the derivation
            }
            let next = [0, 1, 2].map(|i| key[i] + off[i]);
            let Some(&e) = index.0.get(&next) else { return true };
            if world.get::<PendingSnapshotVoxels>(e).is_some() {
                return true;
            }
            world
                .get::<SimChunk>(e)
                .is_some_and(|m| chunk_blocks_opaque(&m.0, plane_blocks(b, off, face_layer(off, false)), &cutout))
        });
        if !hidden {
            shown.push((*local, voxels.clone()));
        }
        hidden
    });
    bricks.spawn.extend(shown);
}

/// Bricks that exist in their sim chunk but were never spawned because they
/// are buried inside it, keyed like `ChunkGrid` (world position /
/// `BRICK_VOXELS`), with their owning chunk entity and voxels. The mesher's
/// neighbour lookup falls back to this when the grid has no entity, so a
/// face brick next to a buried one culls the shared face and the buried test
/// still sees solid there — absent would read as air. Entries go when the
/// brick is spawned, its chunk despawns or is re-derived, or its region goes.
#[derive(Resource, Default, Debug)]
pub struct SkippedBricks(pub std::collections::BTreeMap<[i32; 3], (Entity, VoxelData)>);

/// The `SkippedBricks` keys a chunk entity owns, for its cleanup.
#[derive(Component, Default, Debug)]
pub struct SkippedBrickKeys(pub Vec<[i32; 3]>);

/// Grid key of brick `local` of `chunk` (region root + chunk origin + brick
/// offset, over `BRICK_VOXELS`, rounded as `mark_remesh_dirty` does).
fn brick_grid_key(world: &World, chunk: Entity, local: [u8; 3]) -> Option<[i32; 3]> {
    let entity = world.get_entity(chunk).ok()?;
    let root = entity.get::<ChildOf>()?.parent();
    let root_t = world.get::<Transform>(root).map_or(Vec3::ZERO, |t| t.translation);
    let origin = entity.get::<Transform>().map_or(Vec3::ZERO, |t| t.translation);
    let k = (root_t + brick_translation(origin, local)) / BRICK_VOXELS as f32;
    Some([k.x.round() as i32, k.y.round() as i32, k.z.round() as i32])
}

/// Record `chunk`'s buried bricks in [`SkippedBricks`].
fn record_skipped(world: &mut World, chunk: Entity, buried: Vec<([u8; 3], VoxelData)>) {
    if buried.is_empty() {
        return;
    }
    let keyed: Vec<([i32; 3], VoxelData)> =
        buried.into_iter().filter_map(|(l, v)| Some((brick_grid_key(world, chunk, l)?, v))).collect();
    let keys: Vec<[i32; 3]> = keyed.iter().map(|(k, _)| *k).collect();
    // Registered by the bridge plugin (session-reset); an app without it
    // does not track buried bricks.
    if let Some(mut skipped) = world.get_resource_mut::<SkippedBricks>() {
        for (k, v) in keyed {
            skipped.0.insert(k, (chunk, v));
        }
    }
    world.entity_mut(chunk).insert(SkippedBrickKeys(keys));
}

/// Drop every [`SkippedBricks`] entry `chunk` owns.
fn clear_skipped(world: &mut World, chunk: Entity) {
    let Some(keys) = world.get_entity_mut(chunk).ok().and_then(|mut e| e.take::<SkippedBrickKeys>()) else {
        return;
    };
    if let Some(mut skipped) = world.get_resource_mut::<SkippedBricks>() {
        for k in keys.0 {
            if skipped.0.get(&k).is_some_and(|(owner, _)| *owner == chunk) {
                skipped.0.remove(&k);
            }
        }
    }
}

/// A region left the window: drop its skipped bricks (its entities despawn
/// with the root, without passing through [`clear_skipped`]).
pub fn prune_region_skipped(world: &mut World, region: game::RegionId) {
    let per = game::REGION_SIZE as i32 / BRICK_VOXELS as i32;
    if let Some(mut skipped) = world.get_resource_mut::<SkippedBricks>() {
        skipped.0.retain(|k, _| (k[0].div_euclid(per), k[2].div_euclid(per)) != (region.x, region.z));
    }
    let per = game::REGION_CHUNKS as i32;
    if let Some(mut index) = world.get_resource_mut::<SimChunkIndex>() {
        index.0.retain(|k, _| (k[0].div_euclid(per), k[2].div_euclid(per)) != (region.x, region.z));
    }
}

/// Region-local translation of brick `local` of a chunk at `chunk_origin`.
pub fn brick_translation(chunk_origin: Vec3, local: [u8; 3]) -> Vec3 {
    chunk_origin + Vec3::new(local[0] as f32, local[1] as f32, local[2] as f32) * BRICK_VOXELS as f32
}

/// The brick entity at `local` of `chunk`, if spawned.
fn find_brick(world: &World, chunk: Entity, local: [u8; 3]) -> Option<Entity> {
    let bricks = world.get::<ChunkBricks>(chunk)?;
    bricks.0.iter().copied().find(|&e| world.get::<BrickOf>(e).is_some_and(|o| o.local == local))
}

/// Write brick `local` of `chunk`: replace its voxels, or spawn it (unless
/// all air) as a sibling of the chunk under the region root.
pub fn write_brick(world: &mut World, chunk: Entity, local: [u8; 3], voxels: VoxelData) {
    if let Some(e) = find_brick(world, chunk, local) {
        world.entity_mut(e).insert(voxels);
        return;
    }
    // No longer a stand-in: it is spawned now (or all air, and so absent).
    if let Some(k) = brick_grid_key(world, chunk, local) {
        if let Some(mut skipped) = world.get_resource_mut::<SkippedBricks>() {
            if skipped.0.get(&k).is_some_and(|(owner, _)| *owner == chunk) {
                skipped.0.remove(&k);
            }
        }
    }
    if voxels.is_all_air() {
        return;
    }
    let Ok(entity) = world.get_entity(chunk) else { return };
    let Some(root) = entity.get::<ChildOf>().map(ChildOf::parent) else { return };
    let origin = entity.get::<Transform>().map_or(Vec3::ZERO, |t| t.translation);
    let brick = world.spawn((
        BrickOf { chunk, local },
        Transform::from_translation(brick_translation(origin, local)),
        Visibility::default(),
        ChildOf(root),
        voxels,
    ));
    let brick = brick.id();
    let mut entity = world.entity_mut(chunk);
    match entity.get_mut::<ChunkBricks>() {
        Some(mut bricks) => bricks.0.push(brick),
        None => {
            entity.insert(ChunkBricks(vec![brick]));
        }
    }
}

/// Despawn every brick of `chunk` (the chunk entity itself stays).
pub fn despawn_chunk_bricks(world: &mut World, chunk: Entity) {
    let Some(mut bricks) = world.get_mut::<ChunkBricks>(chunk) else { return };
    let had = !bricks.0.is_empty();
    for e in std::mem::take(&mut bricks.0) {
        if let Ok(brick) = world.get_entity_mut(e) {
            brick.despawn();
        }
    }
    if had {
        // The per-chunk backend's merged mesh drew them (`chunk_mesh`).
        world.entity_mut(chunk).insert(super::meshing::chunk_mesh::ChunkMeshDirty);
    }
}

/// Despawn a sim entity and, for a terrain chunk, its bricks.
pub fn despawn_sim_entity(world: &mut World, e: Entity) {
    if world.get::<ChunkBricks>(e).is_some() {
        // Whatever it buried next door shows now.
        expose_across(world, e, None);
        if let Some(key) = chunk_world_key(world, e) {
            if let Some(mut index) = world.get_resource_mut::<SimChunkIndex>() {
                if index.0.get(&key) == Some(&e) {
                    index.0.remove(&key);
                }
            }
        }
    }
    despawn_chunk_bricks(world, e);
    clear_skipped(world, e);
    if let Ok(entity) = world.get_entity_mut(e) {
        entity.despawn();
    }
}

/// A keyed chunk got a new pose: its bricks follow.
pub fn move_chunk_bricks(world: &mut World, chunk: Entity, origin: Vec3) {
    let Some(bricks) = world.get::<ChunkBricks>(chunk).map(|b| b.0.clone()) else { return };
    for e in bricks {
        let Some(local) = world.get::<BrickOf>(e).map(|o| o.local) else { continue };
        if let Some(mut tf) = world.get_mut::<Transform>(e) {
            tf.translation = brick_translation(origin, local);
        }
    }
}

/// A chunk's derived bricks landed (snapshot or cell activation, off-thread):
/// replace whatever bricks it had, take the mirror, and lay any block edits
/// that arrived meanwhile on top. The caller checked it is still pending.
pub fn land_chunk_bricks(world: &mut World, chunk: Entity, source: Chunk, mut bricks: DerivedBricks) {
    despawn_chunk_bricks(world, chunk);
    clear_skipped(world, chunk);
    recheck_buried(world, chunk, &mut bricks);
    if let Some(key) = chunk_world_key(world, chunk) {
        if let Some(mut index) = world.get_resource_mut::<SimChunkIndex>() {
            index.0.insert(key, chunk);
        }
    }
    let Ok(mut entity) = world.get_entity_mut(chunk) else { return };
    entity.remove::<(PendingSnapshotVoxels, BrickEdits)>().insert((SimChunk(source), ChunkEdits { full: true, ..default() }));
    if !entity.contains::<ChunkBricks>() {
        entity.insert(ChunkBricks::default());
    }
    let pending = entity.take::<PendingBlockDeltas>();
    record_skipped(world, chunk, bricks.buried);
    for (local, voxels) in bricks.spawn {
        write_brick(world, chunk, local, voxels);
    }
    // Neighbours buried against this chunk's faces see its real blocks now.
    expose_across_mirror(world, chunk);
    if let Some(PendingBlockDeltas(deltas)) = pending.filter(|p| !p.0.is_empty()) {
        apply_block_deltas(world, chunk, deltas);
    }
}

/// Brick of a chunk-local block, and the block's coords inside it.
fn brick_of_block(index: BlockIndex) -> ([usize; 3], [usize; 3]) {
    let (bx, by, bz) = index.xyz();
    let b = [bx, by, bz];
    (b.map(|v| v / BRICK_BLOCKS), b.map(|v| v % BRICK_BLOCKS))
}

/// One block delta, applied to a chunk's blocks.
fn apply_delta_to_chunk(chunk: &mut Chunk, delta: &BlockDelta) {
    chunk.blocks.set(delta.index, delta.block);
    match &delta.chisel {
        Some(c) => {
            chunk.chisel.insert(delta.index, c.clone());
        }
        None => {
            chunk.chisel.remove(&delta.index);
        }
    }
}

/// Write one block delta's 16³ voxels (its chisel materials, else its id)
/// into a brick's dense voxel grid at the block's brick-local `at`.
fn apply_delta_to_brick(voxels: &mut [Voxel], at: [usize; 3], delta: &BlockDelta) {
    use game::BLOCK_VOXELS as B;
    let materials = delta.chisel.as_ref().map(game::ChiselData::materials);
    for vz in 0..B {
        for vy in 0..B {
            for vx in 0..B {
                let id = materials.as_ref().map_or(delta.block.id, |m| m[vx + vy * B + vz * B * B]);
                voxels[game::voxel_index(at[0] * B + vx, at[1] * B + vy, at[2] * B + vz)] = Voxel::new(id);
            }
        }
    }
}

/// `SetBlocks` on chunk entity `chunk`. Still deriving its bricks: buffered
/// in `PendingBlockDeltas` until they land. Otherwise the mirror takes the
/// deltas and every brick they touch is re-derived — plus any skipped brick
/// of the same chunk across a brick face a block sits on, which the edit may
/// have exposed. Neighbour bricks (other chunks' included) remesh through
/// `mark_remesh_dirty`'s boundary zones, found in the `ChunkGrid`.
pub fn apply_block_deltas(world: &mut World, chunk: Entity, deltas: Vec<BlockDelta>) {
    let Ok(mut entity) = world.get_entity_mut(chunk) else { return };
    if entity.contains::<PendingSnapshotVoxels>() {
        match entity.get_mut::<PendingBlockDeltas>() {
            Some(mut pending) => pending.0.extend(deltas),
            None => {
                entity.insert(PendingBlockDeltas(deltas));
            }
        }
        return;
    }
    let Some(mut source) = entity.get_mut::<SimChunk>() else {
        edit_bricks_without_mirror(world, chunk, &deltas);
        expose_edited_faces(world, chunk, &deltas);
        return;
    };
    let n = BRICKS_PER_CHUNK_EDGE;
    let indices: Vec<BlockIndex> = deltas.iter().map(|d| d.index).collect();
    let mut touched = BTreeSet::new();
    let mut exposed = BTreeSet::new();
    for delta in &deltas {
        apply_delta_to_chunk(&mut source.0, delta);
        let (b, at) = brick_of_block(delta.index);
        touched.insert(to_local(b));
        for a in 0..3 {
            let mut nb = b;
            if at[a] == 0 && b[a] > 0 {
                nb[a] -= 1;
            } else if at[a] == BRICK_BLOCKS - 1 && b[a] + 1 < n {
                nb[a] += 1;
            } else {
                continue;
            }
            exposed.insert(to_local(nb));
        }
    }
    let mut entity = world.entity_mut(chunk);
    match entity.get_mut::<ChunkEdits>() {
        Some(mut edits) => edits.blocks.extend(indices),
        None => {
            entity.insert(ChunkEdits { full: false, blocks: indices.into_iter().collect() });
        }
    }
    exposed.retain(|l| !touched.contains(l) && find_brick(world, chunk, *l).is_none());
    touched.extend(exposed);
    rederive_bricks(world, chunk, touched);
    expose_edited_faces(world, chunk, &deltas);
}

/// Edited blocks on `chunk`'s own faces that no longer hide what is next to
/// them: spawn the neighbouring chunk's brick buried behind each.
fn expose_edited_faces(world: &mut World, chunk: Entity, deltas: &[BlockDelta]) {
    if world.get_resource::<SkippedBricks>().is_none_or(|s| s.0.is_empty()) {
        return;
    }
    let Some(base) = brick_grid_key(world, chunk, [0; 3]) else { return };
    let cutout = CutoutTable::installed();
    for delta in deltas {
        if block_opaque(delta.block, delta.chisel.is_some(), &cutout) {
            continue;
        }
        let (x, y, z) = delta.index.xyz();
        let p = [x, y, z];
        for off in FACES {
            let a = (0..3).find(|&a| off[a] != 0).unwrap();
            if p[a] != face_layer(off, true) {
                continue;
            }
            let key = [0, 1, 2].map(|i| base[i] + (p[i] / BRICK_BLOCKS) as i32 + off[i]);
            expose_skipped(world, key, chunk);
        }
    }
}

/// Re-derive `locals` (with every brick still dirty from earlier edits) from
/// the mirror: inline for a few, else on the compute pool.
fn rederive_bricks(world: &mut World, chunk: Entity, locals: BTreeSet<[u8; 3]>) {
    let inline_max = world.get_resource::<BrickEditPolicy>().copied().unwrap_or_default().inline_max_bricks;
    let mut entity = world.entity_mut(chunk);
    let mut edits = entity.take::<BrickEdits>().unwrap_or_default();
    edits.dirty.extend(locals);
    let Some(source) = entity.get::<SimChunk>() else { return };
    if edits.dirty.len() <= inline_max {
        // Covers any running task's bricks too; dropping `edits` cancels it.
        let voxels: Vec<([u8; 3], VoxelData)> = edits
            .dirty
            .iter()
            .map(|&l| (l, VoxelData::from_brick(&source.0.brick(to_brick(l)))))
            .collect();
        for (local, v) in voxels {
            write_brick(world, chunk, local, v);
        }
        return;
    }
    let job = BrickEditsJob { source: source.0.clone(), dirty: edits.dirty.clone() };
    edits.task = Some(super::compute::spawn(job, |voxels| voxels));
    entity.insert(edits);
}

/// Poll the off-thread brick re-derivations; a finished one writes its
/// bricks (unless a newer edit replaced the task meanwhile).
pub fn poll_brick_edits(commands: &mut Commands, edits: &mut Query<(Entity, &mut BrickEdits)>) {
    use bevy::tasks::{block_on, futures_lite::future};
    for (chunk, mut edit) in edits.iter_mut() {
        let Some(task) = edit.task.as_mut() else { continue };
        let Some(voxels) = block_on(future::poll_once(task)) else { continue };
        edit.task = None;
        commands.queue(move |world: &mut World| land_brick_edit(world, chunk, voxels));
    }
}

/// Write a finished off-thread re-derivation — only while its `BrickEdits`
/// is still the live one (present, task taken by the poll): a newer edit's
/// task covers these bricks, and a removed one means the bricks were
/// re-landed or cleared since, which makes these voxels stale.
fn land_brick_edit(world: &mut World, chunk: Entity, voxels: Vec<([u8; 3], VoxelData)>) {
    let Ok(mut entity) = world.get_entity_mut(chunk) else { return };
    if !entity.get::<BrickEdits>().is_some_and(|e| e.task.is_none()) {
        return;
    }
    entity.remove::<BrickEdits>();
    for (local, v) in voxels {
        write_brick(world, chunk, local, v);
    }
}

/// `SetBlocks` on a chunk with no mirror (its voxels came from a legacy
/// whole-brick `SetVoxelComponent`): edit the bricks' voxels in place; a
/// missing brick starts as air.
fn edit_bricks_without_mirror(world: &mut World, chunk: Entity, deltas: &[BlockDelta]) {
    for delta in deltas {
        let (b, at) = brick_of_block(delta.index);
        let local = to_local(b);
        let mut voxels = find_brick(world, chunk, local)
            .and_then(|e| world.get::<VoxelData>(e).cloned())
            .unwrap_or_else(|| VoxelData::uniform(BlockId::AIR));
        voxels.edit(|dense| apply_delta_to_brick(dense, at, delta));
        write_brick(world, chunk, local, voxels);
    }
}

/// `SetVoxelComponent(Some)`: a legacy whole-chunk dense write, only
/// meaningful for one-brick chunks. Newer than anything in flight.
pub fn write_legacy_voxels(world: &mut World, chunk: Entity, voxels: Vec<Voxel>) {
    if BRICKS_PER_CHUNK_EDGE != 1 || voxels.len() != game::BRICK_VOXEL_COUNT {
        warn!("bridge: dense SetVoxelComponent on a multi-brick chunk ignored");
        return;
    }
    expose_across(world, chunk, None);
    clear_skipped(world, chunk);
    let Ok(mut entity) = world.get_entity_mut(chunk) else { return };
    entity.remove::<(PendingSnapshotVoxels, PendingBlockDeltas, BrickEdits, SimChunk)>();
    if !entity.contains::<ChunkBricks>() {
        entity.insert(ChunkBricks::default());
    }
    write_brick(world, chunk, [0; 3], VoxelData::new(voxels));
}

/// `SetVoxelComponent(None)`: the chunk renders nothing.
pub fn clear_chunk_voxels(world: &mut World, chunk: Entity) {
    expose_across(world, chunk, None);
    despawn_chunk_bricks(world, chunk);
    clear_skipped(world, chunk);
    let Ok(mut entity) = world.get_entity_mut(chunk) else { return };
    entity.remove::<(PendingSnapshotVoxels, PendingBlockDeltas, BrickEdits)>();
    if let Some(mut source) = entity.get_mut::<SimChunk>() {
        source.0 = Chunk::default();
        entity.insert(ChunkEdits { full: true, ..default() });
    }
    if !entity.contains::<ChunkBricks>() {
        entity.insert(ChunkBricks::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{Block, BlockId, BlockIndex, ChiselData, ChunkBlocks, Voxel, BRICKS_PER_CHUNK_EDGE, BRICK_VOXEL_COUNT};

    fn on_face(local: [u8; 3]) -> bool {
        local.iter().any(|&c| c == 0 || c as usize == BRICKS_PER_CHUNK_EDGE - 1)
    }

    #[test]
    fn uniform_air_chunk_spawns_no_bricks() {
        assert!(spawn_chunk_bricks(&Chunk::uniform(Block::new(BlockId::AIR))).is_empty());
    }

    /// Interior bricks of a solid chunk show no face: skipped. Bricks on the
    /// chunk's faces border an unknown neighbour chunk, so they stay.
    #[test]
    fn uniform_stone_chunk_buried_bricks_skipped() {
        let stone = BlockId(2);
        let derived = derive_chunk_bricks(&Chunk::uniform(Block::new(stone)));
        let n = BRICKS_PER_CHUNK_EDGE;
        assert_eq!(derived.buried.len(), n.saturating_sub(2).pow(3));
        assert!(derived.buried.iter().all(|(l, _)| !on_face(*l)));
        let bricks = derived.spawn;
        assert_eq!(bricks.len(), n.pow(3) - n.saturating_sub(2).pow(3));
        for (local, voxels) in &bricks {
            assert!(on_face(*local), "{local:?} is buried");
            assert_eq!(voxels.runs(), 1, "a uniform brick is one run");
            assert_eq!(voxels.decode(), vec![Voxel::new(stone); BRICK_VOXEL_COUNT]);
        }
    }

    /// Leaves are seen through: a stone brick under (or made of) them shows
    /// faces, so it is never buried.
    #[test]
    fn brick_under_leaves_is_not_buried() {
        let registry = crate::blocks::load_registry(); // installs the shipped props
        let (stone, leaves) = (registry.id_of("stone").unwrap(), registry.id_of("oak_leaves").unwrap());
        let n = BRICKS_PER_CHUNK_EDGE;
        let all_leaves = derive_chunk_bricks(&Chunk::uniform(Block::new(leaves)));
        assert!(all_leaves.buried.is_empty(), "a leaf chunk buries nothing");
        assert_eq!(all_leaves.spawn.len(), n.pow(3));
        if n < 3 {
            return; // no interior brick to bury at this chunk size
        }
        // An interior brick of solid stone with an all-leaf brick above it.
        let mut chunk = Chunk::uniform(Block::new(stone));
        let (c, above) = (n / 2, [n / 2, n / 2 + 1, n / 2]);
        for z in 0..BRICK_BLOCKS {
            for y in 0..BRICK_BLOCKS {
                for x in 0..BRICK_BLOCKS {
                    let at = |a: usize, d: usize| above[a] * BRICK_BLOCKS + d;
                    chunk.set_block(at(0, x), at(1, y), at(2, z), Block::new(leaves));
                }
            }
        }
        let derived = derive_chunk_bricks(&chunk);
        let local = |b: [usize; 3]| b.map(|v| v as u8);
        assert!(derived.spawn.iter().any(|(l, _)| *l == local([c, c, c])), "the brick under the leaves shows");
        assert!(derived.spawn.iter().any(|(l, _)| *l == local(above)), "the leaf brick shows");
        assert!(derived.buried.iter().all(|(l, _)| *l != local([c, c, c])));
        // Its stone neighbour two bricks down, away from the leaves, still buries.
        if c >= 2 {
            assert!(derived.buried.iter().any(|(l, _)| *l == local([c, c - 2, c])));
        }
    }

    /// A floor inside the bottom brick layer: that layer crosses the surface,
    /// everything above it is air.
    #[test]
    fn flat_floor_spawns_the_surface_layer() {
        let chunk = Chunk::flat_floor(8);
        let bricks = spawn_chunk_bricks(&chunk);
        assert_eq!(bricks.len(), BRICKS_PER_CHUNK_EDGE.pow(2));
        for (local, voxels) in &bricks {
            assert_eq!(local[1], 0);
            let b = local.map(usize::from);
            assert_eq!(voxels.decode(), chunk.brick(b).voxels());
        }
    }

    /// xorshift: no `rand` dependency in this crate.
    fn rng(mut seed: u64) -> impl FnMut(u64) -> u64 {
        move |modulus| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % modulus
        }
    }

    /// A seeded chunk of few ids (so runs merge across block and row
    /// boundaries), a third of its blocks chiselled.
    fn seeded_chunk(next: &mut impl FnMut(u64) -> u64) -> Chunk {
        let blocks: Vec<Block> =
            (0..game::CHUNK_BLOCK_COUNT).map(|_| Block::new(BlockId(next(3) as u16))).collect();
        let mut chisel = std::collections::BTreeMap::new();
        for i in 0..game::CHUNK_BLOCK_COUNT {
            if next(3) == 0 {
                let min: [usize; 3] = std::array::from_fn(|_| next(16) as usize);
                let max: [usize; 3] = std::array::from_fn(|a| min[a] + 1 + next((16 - min[a]) as u64) as usize);
                chisel.insert(BlockIndex::from_linear(i), ChiselData::cuboid(BlockId(next(3) as u16), min, max));
            }
        }
        Chunk { blocks: ChunkBlocks::from_vec(blocks), chisel }
    }

    #[test]
    fn from_brick_matches_dense_derive() {
        let mut next = rng(0x9E37_79B9_7F4A_7C15);
        // ~150 random bricks' worth: 150 one-brick chunks, or one 16³ chunk.
        for round in 0..(150 / BRICKS_PER_CHUNK_EDGE.pow(3)).max(1) {
            let chunk = seeded_chunk(&mut next);
            for (b, view) in chunk.bricks() {
                let direct = VoxelData::from_brick(&view);
                assert_eq!(direct.decode(), view.voxels(), "round {round} brick {b:?}");
                assert_eq!(direct, VoxelData::new(view.voxels()), "round {round}: runs must be canonical");
            }
        }
    }

    /// Every spawned brick holds exactly its view's voxels; every skipped one
    /// is all air or buried inside the chunk.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn spawned_bricks_match_their_views() {
        let mut next = rng(0x2545_F491_4F6C_DD1D);
        // ~100 random chunks at one brick each; at least two many-brick ones.
        for _ in 0..(100 / BRICKS_PER_CHUNK_EDGE.pow(3)).max(2) {
            let chunk = seeded_chunk(&mut next);
            let DerivedBricks { spawn, buried } = derive_chunk_bricks(&chunk);
            for (local, voxels) in spawn.iter().chain(&buried) {
                assert_eq!(voxels.decode(), chunk.brick(local.map(usize::from)).voxels());
            }
            for (local, _) in &buried {
                assert!(!on_face(*local), "{local:?}: a chunk-face brick is never buried");
            }
            for (b, view) in chunk.bricks() {
                let local = b.map(|v| v as u8);
                let listed = spawn.iter().chain(&buried).filter(|(l, _)| *l == local).count();
                let air = view.voxels().iter().all(|v| v.is_air());
                assert_eq!(listed, usize::from(!air), "{b:?}: every non-air brick is spawned or buried, once");
            }
        }
    }

    /// Across the chunk's faces, `across` decides: a solid chunk next to
    /// solid on every side buries every brick; one open side shows exactly
    /// the face layer toward it.
    #[test]
    fn face_bricks_bury_against_what_lies_across() {
        let n = BRICKS_PER_CHUNK_EDGE;
        let stone = Block::new(BlockId(2));
        // Uniform, and the same blocks stored densely (the generator's form).
        let dense = Chunk { blocks: ChunkBlocks::from_vec(vec![stone; game::CHUNK_BLOCK_COUNT]), chisel: Default::default() };
        for chunk in [Chunk::uniform(stone), dense] {
            let all = derive_chunk_bricks_across(&chunk, |_, _| true);
            assert!(all.spawn.is_empty(), "solid all round: nothing shows");
            assert_eq!(all.buried.len(), n.pow(3));
            let open_up = derive_chunk_bricks_across(&chunk, |_, off| off != [0, 1, 0]);
            assert_eq!(open_up.spawn.len(), n * n, "only the top layer shows");
            assert!(open_up.spawn.iter().all(|(l, _)| l[1] as usize == n - 1));
            let unknown = derive_chunk_bricks(&chunk);
            assert_eq!(unknown.spawn.len(), n.pow(3) - n.saturating_sub(2).pow(3), "unknown neighbours bury nothing");
        }
    }

    /// Two chunks side by side along +x under one root, both tracked in the
    /// [`SimChunkIndex`]; returns (root, left, right).
    fn two_chunk_world(world: &mut World) -> (Entity, Entity, Entity) {
        world.init_resource::<SkippedBricks>();
        world.init_resource::<SimChunkIndex>();
        // Re-derive edited bricks inline: a bare World has no compute pool.
        world.insert_resource(BrickEditPolicy { inline_max_bricks: usize::MAX });
        let root = world.spawn((Transform::IDENTITY, Visibility::default())).id();
        let mut chunk = |x: f32| {
            world
                .spawn((Transform::from_xyz(x, 0.0, 0.0), Visibility::default(), ChildOf(root), ChunkBricks::default()))
                .id()
        };
        let (left, right) = (chunk(0.0), chunk(game::CHUNK_UNITS as f32));
        (root, left, right)
    }

    fn stone_chunk() -> Chunk {
        Chunk::uniform(Block::new(BlockId(2)))
    }

    /// The left chunk's brick on its +x face, buried against the right chunk.
    fn left_face_brick() -> [u8; 3] {
        [BRICKS_PER_CHUNK_EDGE as u8 - 1, 0, 0]
    }

    fn left_buried() -> DerivedBricks {
        DerivedBricks { spawn: vec![], buried: vec![(left_face_brick(), solid())] }
    }

    /// The right chunk as stone with its -x face block at the left brick's
    /// plane dug out.
    fn right_with_hole() -> Chunk {
        let mut chunk = stone_chunk();
        chunk.set_block(0, 0, 0, Block::new(BlockId::AIR));
        chunk
    }

    /// A buried face brick shows once the chunk next to it lands with air
    /// against it; a solid neighbour keeps it buried.
    #[test]
    fn neighbour_landing_exposes_only_against_air() {
        let mut world = World::new();
        let (_, left, right) = two_chunk_world(&mut world);
        land_chunk_bricks(&mut world, left, stone_chunk(), left_buried());
        land_chunk_bricks(&mut world, right, stone_chunk(), DerivedBricks::default());
        assert!(find_brick(&world, left, left_face_brick()).is_none(), "solid neighbour: still buried");
        assert_eq!(world.resource::<SkippedBricks>().0.len(), 1);
        land_chunk_bricks(&mut world, right, right_with_hole(), DerivedBricks::default());
        assert!(find_brick(&world, left, left_face_brick()).is_some(), "the hole exposes it");
        assert!(world.resource::<SkippedBricks>().0.is_empty());
    }

    /// Digging the neighbour's face block, or the neighbour going away,
    /// exposes the buried brick behind it.
    #[test]
    fn neighbour_edit_or_despawn_exposes() {
        for despawn in [false, true] {
            let mut world = World::new();
            let (_, left, right) = two_chunk_world(&mut world);
            land_chunk_bricks(&mut world, right, stone_chunk(), DerivedBricks::default());
            land_chunk_bricks(&mut world, left, stone_chunk(), left_buried());
            assert!(find_brick(&world, left, left_face_brick()).is_none());
            if despawn {
                despawn_sim_entity(&mut world, right);
                assert!(world.resource::<SimChunkIndex>().0.values().all(|&e| e != right));
            } else {
                let air = Block::new(BlockId::AIR);
                // A block off the shared face changes nothing next door.
                let inner = BlockDelta { index: BlockIndex::from_xyz(1, 0, 0), block: air, chisel: None };
                apply_block_deltas(&mut world, right, vec![inner]);
                assert!(find_brick(&world, left, left_face_brick()).is_none(), "an inner dig keeps it buried");
                let dig = BlockDelta { index: BlockIndex::from_xyz(0, 0, 0), block: air, chisel: None };
                apply_block_deltas(&mut world, right, vec![dig]);
            }
            assert!(find_brick(&world, left, left_face_brick()).is_some(), "despawn={despawn}: exposed");
        }
    }

    /// A chunk landing after its neighbour was edited re-checks the burial
    /// its (older) snapshot decided against the neighbour's current blocks.
    #[test]
    fn landing_rechecks_burial_against_landed_neighbours() {
        let mut world = World::new();
        let (_, left, right) = two_chunk_world(&mut world);
        land_chunk_bricks(&mut world, right, right_with_hole(), DerivedBricks::default());
        land_chunk_bricks(&mut world, left, stone_chunk(), left_buried());
        assert!(find_brick(&world, left, left_face_brick()).is_some(), "the neighbour has a hole there");
        // Still deriving: trusted (its own landing re-checks).
        world.entity_mut(right).insert(PendingSnapshotVoxels);
        land_chunk_bricks(&mut world, left, stone_chunk(), left_buried());
        assert!(find_brick(&world, left, left_face_brick()).is_none());
    }

    /// A world with a region root at the origin and one terrain chunk entity.
    fn chunk_world(world: &mut World) -> Entity {
        let root = world.spawn((Transform::IDENTITY, Visibility::default())).id();
        world.spawn((Transform::IDENTITY, Visibility::default(), ChildOf(root), ChunkBricks::default())).id()
    }

    fn solid() -> VoxelData {
        VoxelData::uniform(BlockId(2))
    }

    /// Buried bricks are stand-ins keyed like the grid; spawning one, the
    /// chunk's despawn, a re-landing and the region's removal all drop them.
    #[test]
    fn skipped_bricks_follow_their_chunk_and_region() {
        let mut world = World::new();
        world.init_resource::<SkippedBricks>(); // registered by the bridge plugin
        let chunk = chunk_world(&mut world);
        let derived = DerivedBricks { spawn: vec![([0, 0, 0], solid())], buried: vec![([1, 0, 0], solid()), ([0, 1, 0], solid())] };
        land_chunk_bricks(&mut world, chunk, Chunk::default(), derived);
        let skipped = |w: &World| w.resource::<SkippedBricks>().0.keys().copied().collect::<Vec<_>>();
        assert_eq!(skipped(&world), vec![[0, 1, 0], [1, 0, 0]]);
        write_brick(&mut world, chunk, [1, 0, 0], solid());
        assert_eq!(skipped(&world), vec![[0, 1, 0]], "a spawned brick is no stand-in");
        despawn_sim_entity(&mut world, chunk);
        assert!(skipped(&world).is_empty());

        let chunk = chunk_world(&mut world);
        land_chunk_bricks(&mut world, chunk, Chunk::default(), DerivedBricks { spawn: vec![], buried: vec![([1, 1, 1], solid())] });
        land_chunk_bricks(&mut world, chunk, Chunk::default(), DerivedBricks::default());
        assert!(skipped(&world).is_empty(), "re-landing replaces the chunk's stand-ins");
        land_chunk_bricks(&mut world, chunk, Chunk::default(), DerivedBricks { spawn: vec![], buried: vec![([1, 1, 1], solid())] });
        prune_region_skipped(&mut world, game::RegionCoords::new(1, 0));
        assert_eq!(skipped(&world).len(), 1, "another region's prune keeps it");
        prune_region_skipped(&mut world, game::RegionCoords::new(0, 0));
        assert!(skipped(&world).is_empty());
    }

    /// A finished off-thread re-derivation is dropped once its `BrickEdits`
    /// is gone (bricks re-landed or cleared since): its voxels are stale.
    #[test]
    fn stale_brick_edit_result_is_dropped() {
        let mut world = World::new();
        let chunk = chunk_world(&mut world);
        write_brick(&mut world, chunk, [0; 3], solid());
        let stale = VoxelData::uniform(BlockId(9));
        land_brick_edit(&mut world, chunk, vec![([0; 3], stale.clone())]);
        let brick = world.get::<ChunkBricks>(chunk).unwrap().0[0];
        assert_eq!(world.get::<VoxelData>(brick), Some(&solid()), "no live BrickEdits: dropped");
        world.entity_mut(chunk).insert(BrickEdits::default());
        land_brick_edit(&mut world, chunk, vec![([0; 3], stale.clone())]);
        assert_eq!(world.get::<VoxelData>(brick), Some(&stale), "the live task's result lands");
        assert!(world.get::<BrickEdits>(chunk).is_none());
    }

    /// Quads the mesher gives `center` with these face neighbours (the
    /// reference the pipeline must match).
    fn reference_quads(center: &VoxelData, neighbors: &[([i32; 3], &VoxelData)]) -> u32 {
        let map: std::collections::BTreeMap<[i32; 3], &VoxelData> = neighbors.iter().copied().collect();
        let padded = super::super::meshing::neighbors::gather_padded_rle(center, &map);
        super::super::meshing::mesh_chunk(&padded, &super::super::block_textures::BlockTextureLayers::default(), true)
            .quad_count
    }

    fn mesh_app() -> App {
        use super::super::meshing::{apply_meshed_chunks, queue};
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<super::super::block_textures::BlockTextureLayers>()
            .init_resource::<queue::ChunkGrid>()
            .init_resource::<queue::RemeshQueue>()
            .init_resource::<queue::MeshBudgets>()
            .init_resource::<queue::FrameUploads>()
            .init_resource::<SkippedBricks>()
            .add_systems(
                Update,
                (queue::mark_remesh_dirty, queue::prune_chunk_grid, queue::spawn_mesh_tasks, apply_meshed_chunks)
                    .chain(),
            );
        app
    }

    /// Run until `e`'s latest mesh carries `want` quads.
    fn settles_to(app: &mut App, e: Entity, want: u32) -> bool {
        use super::super::meshing::{MeshingTask, TerrainQuadCount};
        for _ in 0..2000 {
            app.update();
            let w = app.world();
            let idle = !w.entity(e).contains::<MeshingTask>()
                && !w.resource::<super::super::meshing::queue::RemeshQueue>().dirty.contains_key(&e);
            if idle && w.get::<TerrainQuadCount>(e).map(|q| q.0) == Some(want) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        false
    }

    /// Multi-brick geometry, built by hand (the constant is one brick per
    /// chunk today): a face brick next to a buried, never-spawned brick must
    /// cull the shared face — the stand-in reads solid, not air. Exposing the
    /// buried brick (an edit digs a block on that face) spawns it, and the
    /// two then mesh exactly as two real neighbours: the face brick shows the
    /// hole, the exposed brick no wall toward the face brick.
    #[test]
    fn face_brick_culls_toward_a_buried_brick_until_an_edit_exposes_it() {
        let mut app = mesh_app();
        let chunk = chunk_world(app.world_mut());
        let derived = DerivedBricks { spawn: vec![([0, 0, 0], solid())], buried: vec![([1, 0, 0], solid())] };
        land_chunk_bricks(app.world_mut(), chunk, Chunk::default(), derived);
        let face = app.world().get::<ChunkBricks>(chunk).unwrap().0[0];
        let open = reference_quads(&solid(), &[]);
        let culled = reference_quads(&solid(), &[([1, 0, 0], &solid())]);
        assert!(culled < open, "a solid +x neighbour must remove faces ({culled} vs {open})");
        assert!(settles_to(&mut app, face, culled), "the face brick walled itself off from the buried brick");

        // Dig the buried brick's -x face block: it is spawned with the hole.
        let air = game::Block::new(BlockId::AIR);
        let dig = BlockDelta { index: BlockIndex::from_xyz(0, 0, 0), block: air, chisel: None };
        let mut dug = solid().decode();
        apply_delta_to_brick(&mut dug, [0, 0, 0], &dig);
        let dug = VoxelData::new(dug);
        write_brick(app.world_mut(), chunk, [1, 0, 0], dug.clone());
        assert!(app.world().resource::<SkippedBricks>().0.is_empty());
        let exposed = find_brick(app.world(), chunk, [1, 0, 0]).expect("exposed brick spawned");
        let face_want = reference_quads(&solid(), &[([1, 0, 0], &dug)]);
        let exposed_want = reference_quads(&dug, &[([-1, 0, 0], &solid())]);
        assert!(face_want > culled, "the face brick must show into the hole");
        assert!(settles_to(&mut app, face, face_want), "face brick after exposure");
        assert!(settles_to(&mut app, exposed, exposed_want), "exposed brick walled off its neighbour");
    }
}
