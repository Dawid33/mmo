//! Sim chunk -> render brick fan-out. A sim chunk is `CHUNK_BLOCKS`³ blocks;
//! the client meshes and draws `BRICK_VOXELS`³-voxel bricks, each its own
//! entity, so the chunk grid, neighbour gather and remesh queue see bricks
//! exactly as they saw chunks, whatever the sim-chunk size.
//!
//! Shape: the sim chunk entity keeps `SimEntity` (and its pose) and gets a
//! [`ChunkBricks`] list plus a [`SimChunk`] mirror of its blocks; it holds no
//! `VoxelData` / `ChunkKey` and is never interpolated. Each brick is a
//! `ChildOf(region root)` sibling with [`BrickOf`], `VoxelData` and a
//! `ChunkKey` (the region-local chunk origin plus `local * BRICK_VOXELS`, in
//! bricks) — and NO `Transform` or `Visibility`: it is meshed, never drawn
//! (`brick_bundle`). The commander's height grid and tile
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
use super::meshing::queue::{presents_opaque_face, ChunkKey};

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

/// On a sim chunk that landed WITHOUT its bricks: it lies beyond the full
/// detail distance (`bridge::detail_reach`), where only its coarse twin
/// (`meshing::coarse`) is ever drawn, so nothing derives or meshes its ~60
/// bricks. It has its [`SimChunk`] mirror, sits in the [`SimChunkIndex`] and
/// takes edits (mirror only). When the camera comes within reach
/// [`materialise_chunk`] derives its bricks from the mirror and it lands
/// again like a snapshot chunk; it does not go back.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct FineDeferred;

/// A chunk and its six face neighbours' blocks (in [`FACES`] order; `None`:
/// no landed chunk there): what deriving a [`FineDeferred`] chunk's bricks
/// reads, taken from the mirrors — its snapshot is long gone.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct MirrorItem {
    pub chunk: Chunk,
    pub neighbours: [Option<Chunk>; 6],
}

impl MirrorItem {
    fn gather(world: &World, chunk: Entity) -> Option<Self> {
        let source = world.get::<SimChunk>(chunk)?.0.clone();
        let key = chunk_world_key(world, chunk)?;
        let index = world.get_resource::<SimChunkIndex>();
        let neighbours = FACES.map(|off| {
            let next = [0, 1, 2].map(|a| key[a] + off[a]);
            let e = *index?.0.get(&next)?;
            Some(world.get::<SimChunk>(e)?.0.clone())
        });
        Some(Self { chunk: source, neighbours })
    }

    /// The chunk's bricks, face bricks buried against a landed neighbour whose
    /// facing blocks are opaque. No chunk below is the implied ground under
    /// the column's band (solid, as the coarse twin and the near mesher take
    /// it) — or one that has not landed yet, which un-buries what it shows
    /// when it does ([`expose_across_mirror`]). No chunk elsewhere never
    /// buries (a snapshot knows the ground there; the mirrors do not).
    pub fn derive(&self) -> DerivedBricks {
        let cutout = CutoutTable::installed();
        derive_chunk_bricks_across(&self.chunk, |b, off| {
            let face = FACES.iter().position(|f| *f == off).expect("a face offset");
            match &self.neighbours[face] {
                Some(next) => chunk_blocks_opaque(next, plane_blocks(b, off, face_layer(off, false)), cutout),
                None => off == [0, -1, 0],
            }
        })
    }
}

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
        // Liquid is empty to the chunk mesher (the water pass draws it).
        if block.id == BlockId::AIR || cutout.is_liquid(block.id) {
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
            if voxels.is_all_empty(&cutout) {
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

/// An unchiselled, non-air, non-liquid, non-cutout block: hides the face
/// beside it. A chiselled block may hold air, so it never counts.
fn block_opaque(block: game::Block, chiselled: bool, cutout: &CutoutTable) -> bool {
    !chiselled && block.id != BlockId::AIR && !cutout.is_liquid(block.id) && !cutout.is_cutout(block.id)
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
        // Keys sort by x first: only the region's own x band is walked, and
        // the ~40k voxel payloads it drops are freed off the main thread
        // (walking the whole map and freeing them here was a third of a
        // seam crossing's long frames, 2026-11-19 perf capture).
        let band = [region.x * per, i32::MIN, i32::MIN]..[(region.x + 1) * per, i32::MIN, i32::MIN];
        let gone: Vec<_> = skipped.0.extract_if(band, |k, _| k[2].div_euclid(per) == region.z).collect();
        if !gone.is_empty() {
            #[cfg(not(target_arch = "wasm32"))]
            bevy::tasks::AsyncComputeTaskPool::get().spawn(async move { drop(gone) }).detach();
        }
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

/// What a brick entity is made of. No `Transform` and no `Visibility` on
/// purpose: a brick is never drawn (its sim chunk draws the merged quads,
/// `meshing/chunk_mesh.rs`) and the mesher finds it by its [`ChunkKey`], set
/// here from the same arithmetic `mark_remesh_dirty` used to derive from the
/// transform. With them, the ~400k bricks of a 3x3 window were walked by
/// Bevy's transform propagation (any moving entity dirties its region's tree)
/// and by `check_visibility` EVERY frame — about half of each compute-pool
/// thread at 60 fps (2026-11-19 perf capture). It stays a child of the region
/// root, so it still goes when the region does.
fn brick_bundle(
    chunk: Entity,
    local: [u8; 3],
    key: [i32; 3],
    root: Entity,
    voxels: VoxelData,
) -> (BrickOf, ChunkKey, ChildOf, VoxelData) {
    (BrickOf { chunk, local }, ChunkKey(key), ChildOf(root), voxels)
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
    if voxels.is_all_empty(&CutoutTable::installed()) {
        return;
    }
    let Ok(entity) = world.get_entity(chunk) else { return };
    let Some(root) = entity.get::<ChildOf>().map(ChildOf::parent) else { return };
    let Some(key) = brick_grid_key(world, chunk, local) else { return };
    let brick = world.spawn(brick_bundle(chunk, local, key, root, voxels)).id();
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

/// Mark the coarse twins of `chunk`'s face neighbours out of date: a twin is
/// padded with its neighbours' boundary blocks (`coarse::boundary_slab`).
/// `deferred_only`: just the [`FineDeferred`] ones — a neighbour with bricks
/// re-meshes its border bricks, which rebuilds its twin anyway.
fn dirty_neighbour_twins(world: &mut World, chunk: Entity, deferred_only: bool) {
    use super::meshing::coarse::{coarse_twins_enabled, CoarseDirty};
    if !coarse_twins_enabled() {
        return;
    }
    let Some(key) = chunk_world_key(world, chunk) else { return };
    for off in FACES {
        let next = [0, 1, 2].map(|a| key[a] + off[a]);
        let Some(&e) = world.get_resource::<SimChunkIndex>().and_then(|i| i.0.get(&next)) else { continue };
        let Ok(mut entity) = world.get_entity_mut(e) else { continue };
        if entity.contains::<SimChunk>()
            && !entity.contains::<CoarseDirty>()
            && (!deferred_only || entity.contains::<FineDeferred>())
        {
            entity.insert(CoarseDirty);
        }
    }
}

/// A chunk beyond the full detail distance landed: it takes its mirror (with
/// any block edits that arrived meanwhile) and asks for its coarse twin, and
/// that is all — see [`FineDeferred`]. The caller checked it is still pending.
pub fn land_chunk_deferred(world: &mut World, chunk: Entity, mut source: Chunk) {
    use super::meshing::coarse::{CoarseDirty, CoarseTwin};
    despawn_chunk_bricks(world, chunk);
    clear_skipped(world, chunk);
    if let Some(key) = chunk_world_key(world, chunk) {
        if let Some(mut index) = world.get_resource_mut::<SimChunkIndex>() {
            index.0.insert(key, chunk);
        }
    }
    let Ok(mut entity) = world.get_entity_mut(chunk) else { return };
    if let Some(PendingBlockDeltas(deltas)) = entity.take::<PendingBlockDeltas>() {
        for delta in &deltas {
            apply_delta_to_chunk(&mut source, delta);
        }
    }
    // Whatever full-detail mesh it had (a chunk landing a second time) goes;
    // its twin, if any, then draws at every distance until rebuilt.
    let twin = entity.get::<CoarseTwin>().map(|t| t.0);
    entity
        .remove::<(
            PendingSnapshotVoxels,
            BrickEdits,
            super::meshing::chunk_mesh::ChunkMeshDirty,
            super::voxel_pull::ChunkQuads,
            super::meshing::TerrainQuadCount,
            super::meshing::InteriorFaces,
        )>()
        .insert((SimChunk(source), ChunkEdits { full: true, ..default() }, FineDeferred, CoarseDirty));
    if !entity.contains::<ChunkBricks>() {
        entity.insert(ChunkBricks::default());
    }
    if let Some(mut twin) = twin.and_then(|t| world.get_entity_mut(t).ok()) {
        twin.insert(super::voxel_pull::DetailBand::Always);
    }
    // Neighbours buried against this chunk's faces see its real blocks now.
    expose_across_mirror(world, chunk);
    dirty_neighbour_twins(world, chunk, false);
}

/// The camera came within reach of a [`FineDeferred`] chunk: read its mirror
/// and its neighbours' NOW (one step with the marker swap, so an edit lands
/// either in the mirror before or in `PendingBlockDeltas` after) and make it
/// a pending chunk again. Returns what `apply_snapshot_voxels` derives its
/// bricks from; `None` if it is no longer deferred.
pub fn materialise_chunk(world: &mut World, chunk: Entity) -> Option<MirrorItem> {
    if world.get::<FineDeferred>(chunk).is_none() || world.get::<PendingSnapshotVoxels>(chunk).is_some() {
        return None;
    }
    let item = MirrorItem::gather(world, chunk)?;
    world.entity_mut(chunk).remove::<FineDeferred>().insert(PendingSnapshotVoxels);
    Some(item)
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
    spawn_landed_bricks(world, chunk, bricks.spawn);
    // Neighbours buried against this chunk's faces see its real blocks now.
    expose_across_mirror(world, chunk);
    dirty_neighbour_twins(world, chunk, true);
    if let Some(PendingBlockDeltas(deltas)) = pending.filter(|p| !p.0.is_empty()) {
        apply_block_deltas(world, chunk, deltas);
    }
}

/// [`write_brick`] for every brick of a chunk that was just emptied
/// (`land_chunk_bricks`): the chunk's root, origin and grid base are looked
/// up once, no brick needs finding (there are none yet — `write_brick`'s
/// linear `find_brick` made a 300-brick landing quadratic), and the bricks
/// spawn in one batch. A local given twice falls back to `write_brick`.
fn spawn_landed_bricks(world: &mut World, chunk: Entity, spawn: Vec<([u8; 3], VoxelData)>) {
    let Ok(entity) = world.get_entity(chunk) else { return };
    let Some(root) = entity.get::<ChildOf>().map(ChildOf::parent) else { return };
    let origin = entity.get::<Transform>().map_or(Vec3::ZERO, |t| t.translation);
    let root_t = world.get::<Transform>(root).map_or(Vec3::ZERO, |t| t.translation);
    // Same arithmetic as `brick_grid_key`, bit for bit.
    let grid_key = |local: [u8; 3]| {
        let k = (root_t + brick_translation(origin, local)) / BRICK_VOXELS as f32;
        [k.x.round() as i32, k.y.round() as i32, k.z.round() as i32]
    };
    let cutout = CutoutTable::installed();
    let n = BRICKS_PER_CHUNK_EDGE;
    let mut seen = vec![false; n * n * n];
    let mut again = Vec::new();
    let mut fresh = Vec::with_capacity(spawn.len());
    {
        let mut skipped = world.get_resource_mut::<SkippedBricks>();
        for (local, voxels) in spawn {
            let slot = &mut seen[local[0] as usize + n * (local[1] as usize + n * local[2] as usize)];
            if std::mem::replace(slot, true) {
                again.push((local, voxels));
                continue;
            }
            if let Some(skipped) = skipped.as_mut() {
                let k = grid_key(local);
                if skipped.0.get(&k).is_some_and(|(owner, _)| *owner == chunk) {
                    skipped.0.remove(&k);
                }
            }
            if voxels.is_all_empty(cutout) {
                continue;
            }
            fresh.push(brick_bundle(chunk, local, grid_key(local), root, voxels));
        }
    }
    let ids: Vec<Entity> = world.spawn_batch(fresh).collect();
    if !ids.is_empty() {
        let mut entity = world.entity_mut(chunk);
        match entity.get_mut::<ChunkBricks>() {
            Some(mut bricks) => bricks.0.extend(ids),
            None => {
                entity.insert(ChunkBricks(ids));
            }
        }
    }
    for (local, voxels) in again {
        write_brick(world, chunk, local, voxels);
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
    if entity.contains::<FineDeferred>() && entity.contains::<SimChunk>() {
        // No bricks to re-derive: the mirror takes the edit, the twin (and
        // the neighbours' twins, padded with these blocks) is rebuilt, and a
        // neighbour's brick this chunk buried may show now.
        let mut source = entity.get_mut::<SimChunk>().expect("checked above");
        for delta in &deltas {
            apply_delta_to_chunk(&mut source.0, delta);
        }
        let indices = deltas.iter().map(|d| d.index);
        match entity.get_mut::<ChunkEdits>() {
            Some(mut edits) => edits.blocks.extend(indices),
            None => {
                entity.insert(ChunkEdits { full: false, blocks: indices.collect() });
            }
        }
        entity.insert(super::meshing::coarse::CoarseDirty);
        expose_edited_faces(world, chunk, &deltas);
        dirty_neighbour_twins(world, chunk, false);
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
    dirty_neighbour_twins(world, chunk, true);
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

