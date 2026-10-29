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
//! `mark_remesh_dirty`) a `ChunkKey`. Bricks also carry a copy of their
//! chunk's `SimEntity`, which the commander's height grid and tile mirror
//! still read voxels through.
//!
//! All-air bricks and bricks buried inside their chunk are never spawned
//! ([`spawn_chunk_bricks`]). An edit that needs one ([`apply_block_deltas`])
//! derives it from the mirror, which is why the client keeps the blocks.

use std::collections::BTreeSet;

use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task};
use game::{BlockDelta, BlockId, BlockIndex, Chunk, Voxel, BRICKS_PER_CHUNK_EDGE, BRICK_BLOCKS, BRICK_VOXELS};

use super::bridge::{PendingBlockDeltas, PendingSnapshotVoxels, SimEntity, VoxelData};
use super::meshing::queue::is_buried_by;

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

fn to_brick(local: [u8; 3]) -> [usize; 3] {
    local.map(usize::from)
}

/// Linear index of brick `b` in `Chunk::bricks()` order (x fastest).
fn brick_linear(b: [usize; 3]) -> usize {
    let n = BRICKS_PER_CHUNK_EDGE;
    b[0] + n * (b[1] + n * b[2])
}

/// The render bricks of `chunk`: every brick that is neither all air nor
/// buried inside the chunk (no air, and every face neighbour brick IN THE
/// CHUNK presents no air across the shared face — `is_buried`, per brick; a
/// brick on the chunk's face borders an unknown chunk, so it always stays).
/// A `Uniform` chunk produces its list without per-voxel work. Runs off the
/// main thread (`SnapshotVoxelTask`).
pub fn spawn_chunk_bricks(chunk: &Chunk) -> Vec<([u8; 3], VoxelData)> {
    let n = BRICKS_PER_CHUNK_EDGE;
    if let (Some(block), true) = (chunk.blocks.uniform(), chunk.chisel.is_empty()) {
        if block.id == BlockId::AIR {
            return Vec::new();
        }
        let on_face = |b: [usize; 3]| b.iter().any(|&v| v == 0 || v == n - 1);
        return chunk
            .bricks()
            .filter(|(b, _)| on_face(*b))
            .map(|(b, _)| (to_local(b), VoxelData::uniform(block.id)))
            .collect();
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
    let keep: Vec<bool> = chunk
        .bricks()
        .map(|(b, _)| {
            let voxels = &all[brick_linear(b)];
            !voxels.is_all_air() && !is_buried_by(voxels, |off| at(b, off))
        })
        .collect();
    chunk
        .bricks()
        .zip(all)
        .zip(keep)
        .filter(|(_, keep)| *keep)
        .map(|(((b, _), voxels), _)| (to_local(b), voxels))
        .collect()
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
    if voxels.is_all_air() {
        return;
    }
    let Ok(entity) = world.get_entity(chunk) else { return };
    let Some(root) = entity.get::<ChildOf>().map(ChildOf::parent) else { return };
    let origin = entity.get::<Transform>().map_or(Vec3::ZERO, |t| t.translation);
    let sim = entity.get::<SimEntity>().map(|s| SimEntity { region: s.region, key: s.key });
    let mut brick = world.spawn((
        BrickOf { chunk, local },
        Transform::from_translation(brick_translation(origin, local)),
        Visibility::default(),
        ChildOf(root),
        voxels,
    ));
    if let Some(sim) = sim {
        brick.insert(sim);
    }
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
    for e in std::mem::take(&mut bricks.0) {
        if let Ok(brick) = world.get_entity_mut(e) {
            brick.despawn();
        }
    }
}

/// Despawn a sim entity and, for a terrain chunk, its bricks.
pub fn despawn_sim_entity(world: &mut World, e: Entity) {
    despawn_chunk_bricks(world, e);
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
pub fn land_chunk_bricks(world: &mut World, chunk: Entity, source: Chunk, bricks: Vec<([u8; 3], VoxelData)>) {
    despawn_chunk_bricks(world, chunk);
    let Ok(mut entity) = world.get_entity_mut(chunk) else { return };
    entity.remove::<(PendingSnapshotVoxels, BrickEdits)>().insert(SimChunk(source));
    if !entity.contains::<ChunkBricks>() {
        entity.insert(ChunkBricks::default());
    }
    let pending = entity.take::<PendingBlockDeltas>();
    for (local, voxels) in bricks {
        write_brick(world, chunk, local, voxels);
    }
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
        return;
    };
    let n = BRICKS_PER_CHUNK_EDGE;
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
    exposed.retain(|l| !touched.contains(l) && find_brick(world, chunk, *l).is_none());
    touched.extend(exposed);
    rederive_bricks(world, chunk, touched);
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
    let source = source.0.clone();
    let dirty = edits.dirty.clone();
    edits.task = Some(AsyncComputeTaskPool::get().spawn(async move {
        dirty
            .into_iter()
            .map(|l| (l, VoxelData::from_brick(&source.brick(to_brick(l))).with_boundary_zones()))
            .collect()
    }));
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
        commands.queue(move |world: &mut World| {
            let Ok(mut entity) = world.get_entity_mut(chunk) else { return };
            if entity.get::<BrickEdits>().is_some_and(|e| e.task.is_some()) {
                return; // a newer edit's task covers these bricks
            }
            entity.remove::<BrickEdits>();
            for (local, v) in voxels {
                write_brick(world, chunk, local, v);
            }
        });
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
    let Ok(mut entity) = world.get_entity_mut(chunk) else { return };
    entity.remove::<(PendingSnapshotVoxels, PendingBlockDeltas, BrickEdits, SimChunk)>();
    if !entity.contains::<ChunkBricks>() {
        entity.insert(ChunkBricks::default());
    }
    write_brick(world, chunk, [0; 3], VoxelData::new(voxels));
}

/// `SetVoxelComponent(None)`: the chunk renders nothing.
pub fn clear_chunk_voxels(world: &mut World, chunk: Entity) {
    despawn_chunk_bricks(world, chunk);
    let Ok(mut entity) = world.get_entity_mut(chunk) else { return };
    entity.remove::<(PendingSnapshotVoxels, PendingBlockDeltas, BrickEdits)>();
    if let Some(mut source) = entity.get_mut::<SimChunk>() {
        source.0 = Chunk::default();
    }
    if !entity.contains::<ChunkBricks>() {
        entity.insert(ChunkBricks::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{Block, BlockId, BlockIndex, ChiselData, ChunkBlocks, Voxel, BRICKS_PER_CHUNK_EDGE, BRICK_VOXEL_COUNT};

    fn on_chunk_face(local: [u8; 3]) -> bool {
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
        let bricks = spawn_chunk_bricks(&Chunk::uniform(Block::new(stone)));
        let n = BRICKS_PER_CHUNK_EDGE;
        assert_eq!(bricks.len(), n.pow(3) - n.saturating_sub(2).pow(3));
        for (local, voxels) in &bricks {
            assert!(on_chunk_face(*local), "{local:?} is buried");
            assert_eq!(voxels.runs(), 1, "a uniform brick is one run");
            assert_eq!(voxels.decode(), vec![Voxel::new(stone); BRICK_VOXEL_COUNT]);
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
        for round in 0..150 {
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
    fn spawned_bricks_match_their_views() {
        let mut next = rng(0x2545_F491_4F6C_DD1D);
        for _ in 0..100 {
            let chunk = seeded_chunk(&mut next);
            let bricks = spawn_chunk_bricks(&chunk);
            for (local, voxels) in &bricks {
                assert_eq!(voxels.decode(), chunk.brick(local.map(usize::from)).voxels());
            }
            for (b, view) in chunk.bricks() {
                let local = b.map(|v| v as u8);
                if bricks.iter().all(|(l, _)| *l != local) {
                    let voxels = view.voxels();
                    let air = voxels.iter().all(|v| v.is_air());
                    assert!(air || (!on_chunk_face(local) && voxels.iter().all(|v| !v.is_air())), "{b:?} skipped");
                }
            }
        }
    }
}
