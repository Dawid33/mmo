//! Player block editing: a left click breaks the block under the crosshair, a
//! right click places one against the face it was hit on.
//!
//! Clicks arrive as ordinary tick-stamped `PlayerInput` mouse-button events,
//! so an edit runs inside the tick on both the predicting client and the
//! server, from the same authoritative eye position and look. Nothing new
//! crosses the wire. Everything here is `+ - * /`, `floor` and comparisons on
//! f32 plus the libm-forced `look_quat`, so the raycast is bit-identical
//! across machines.
//!
//! An edit is one undoable change to three things: the chunk's block data
//! (one block, `Undo::<Component<Chunk>>::set_chunk_block_safe` — the chunk
//! is never copied), the renderer (`SetBlocks` block deltas both ways), and
//! the chunk's colliders (the edited block's cells patched into the
//! `Voxels` shapes in place, see `edit_block_colliders_safe` and
//! `Undo::<ColliderSet>::mutate_collider_safe`).
//!
//! Scope: whole blocks only (a break clears chisel detail), and only inside
//! the player's home region. A ray that leaves the region finds nothing.
//!
//! Implied ground (rock below the materialised surface band, stored only as
//! the column's `ColumnGround` recipe) is solid to the ray. Breaking a block
//! materialises its own chunk and every implied face-neighbour chunk in the
//! same undoable edit, so reachable air is always bordered by real chunks.
//! Bedrock cannot be broken.
//!
//! Known limit: the region's outermost block ring cannot be dug below the
//! surface. Its outward face neighbour is in the next region, whose implied
//! ground this region cannot materialise, so the new air would show a face
//! with no mesh and no collider behind it (see-through, fall-through). Such
//! a break is refused (`breaks_into_neighbour_ground`); the surface block
//! itself and everything one block column in stay diggable.


use rapier3d::math::Vector;
use rapier3d::prelude::ColliderBuilder;
use slotmapd::Key as _;

use crate::movement::PLAYER_EYE_OFFSET;
use crate::{
    Block, BlockDelta, BlockId, BlockIndex, Chunk, ChunkBlocks, ChunkCoords, ClientId, EntityKey, GameDataTransactionKind,
    GameDataUpdate, GameDataUpdateKind, Pose, MouseButton, Rollback, BLOCK_VOXELS, CHUNK_BLOCKS,
    CHUNK_BLOCK_COUNT, REGION_CHUNKS,
};

/// How far a player can reach to break or place, in metres (= blocks).
pub const REACH: f32 = 5.0;
/// The block a right click places. Dirt until the hotbar drives it.
pub const PLACE_BLOCK: BlockId = BlockId(1);
/// Mouse button that breaks the targeted block.
pub const BREAK_BUTTON: MouseButton = MouseButton::Left;
/// Mouse button that places a block against the targeted face.
pub const PLACE_BUTTON: MouseButton = MouseButton::Right;

/// Region-local block coordinates: block `b` spans world units
/// `[b * BLOCK_VOXELS, (b + 1) * BLOCK_VOXELS)` on each axis.
pub type BlockPos = [i32; 3];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    Break,
    Place,
}

/// The first solid block a ray enters, and the face it entered through
/// (outward unit normal). The normal is zero when the ray starts inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHit {
    pub block: BlockPos,
    pub normal: [i32; 3],
}

/// The chunk holding `pos` and the block's index inside it, or `None` if the
/// block is outside this region's chunk columns.
pub fn locate(pos: BlockPos) -> Option<(ChunkCoords, (usize, usize, usize))> {
    let cb = CHUNK_BLOCKS as i32;
    let (cx, cz) = (pos[0].div_euclid(cb), pos[2].div_euclid(cb));
    let columns = REGION_CHUNKS as i32;
    if cx < 0 || cz < 0 || cx >= columns || cz >= columns {
        return None;
    }
    let coords = ChunkCoords::new(cx as usize, pos[1].div_euclid(cb), cz as usize);
    let local = (
        pos[0].rem_euclid(cb) as usize,
        pos[1].rem_euclid(cb) as usize,
        pos[2].rem_euclid(cb) as usize,
    );
    Some((coords, local))
}

/// Whether a chunk's block cell is anything but plain air.
fn cell_solid(chunk: &Chunk, (bx, by, bz): (usize, usize, usize)) -> bool {
    chunk.block_at(bx, by, bz).id != BlockId::AIR
        || chunk.chisel.contains_key(&BlockIndex::from_xyz(bx, by, bz))
}

/// Whether `block` is the bedrock of the column `ground` (no ground: never).
/// Pure core of `Rollback::is_bedrock`; the caller handles cell mode.
pub fn is_bedrock_at(ground: Option<&crate::ColumnGround>, block: Block) -> bool {
    ground.is_some_and(|g| block.id == g.bedrock)
}

/// Whether breaking `pos` would open a face into a neighbour region's
/// unmaterialised ground: strictly below its block column's surface with a
/// face neighbour outside this region's columns. Pure core of
/// `Rollback::breaks_into_neighbour_ground`; the caller handles cell mode.
pub fn edge_ring_locked(pos: BlockPos, ground: Option<&crate::ColumnGround>) -> bool {
    let Some(ground) = ground else { return false };
    let cb = CHUNK_BLOCKS as i32;
    let (bx, bz) = (pos[0].rem_euclid(cb) as usize, pos[2].rem_euclid(cb) as usize);
    if pos[1] >= i32::from(ground.surface[bx * CHUNK_BLOCKS + bz]) {
        return false;
    }
    [[1, 0], [-1, 0], [0, 1], [0, -1]]
        .into_iter()
        .any(|[dx, dz]| locate([pos[0] + dx, pos[1], pos[2] + dz]).is_none())
}

impl Rollback {
    fn block_solid(&self, pos: BlockPos) -> bool {
        let Some((coords, local)) = locate(pos) else { return false };
        let Some(e) = self.chunk_key_at(coords) else {
            return self.implied_block(coords, local).is_some_and(|b| b.id != BlockId::AIR);
        };
        self.data.ecs.chunk.try_get(e).as_ref().is_some_and(|c| cell_solid(c, local))
    }

    /// The block at `local` of chunk `coords` as the sim sees it: the chunk
    /// entity's, else the implied ground's. `None` for an empty slot.
    pub(crate) fn current_block(&self, coords: ChunkCoords, (bx, by, bz): (usize, usize, usize)) -> Option<Block> {
        match self.chunk_key_at(coords) {
            Some(e) => self.data.ecs.chunk.try_get(e).as_ref().map(|c| c.block_at(bx, by, bz)),
            None => self.implied_block(coords, (bx, by, bz)),
        }
    }

    /// Whether `block` is the bedrock of the ground under chunk `coords`.
    /// Cell mode has no ground semantics (as `implied_block`).
    fn is_bedrock(&self, coords: ChunkCoords, block: Block) -> bool {
        self.data.cells.cell_chunks == 0
            && is_bedrock_at(
                self.data.ground.get(&crate::ColumnCoords::new(coords.x, coords.z)),
                block,
            )
    }

    /// Whether breaking `pos` (in chunk `coords`) would open a face
    /// into a neighbour region's unmaterialised ground: the block lies
    /// strictly below its own block column's surface and a face neighbour is
    /// outside this region's columns. Only columns with ground (never cell
    /// mode) qualify. See the module docs' known limit.
    fn breaks_into_neighbour_ground(&self, pos: BlockPos, coords: ChunkCoords) -> bool {
        if self.data.cells.cell_chunks != 0 {
            return false;
        }
        edge_ring_locked(pos, self.data.ground.get(&crate::ColumnCoords::new(coords.x, coords.z)))
    }

    /// Turns implied chunk `coords` into a real chunk entity built from the
    /// column's ground recipe, undo-safely (`RemoveEntity` on undo). `None`
    /// when the chunk is not implied (it exists, cell mode, no ground, or
    /// inside the band).
    pub fn materialize_ground_safe(&mut self, coords: ChunkCoords) -> Option<EntityKey> {
        // One canonicalising build: per-block `set_block` into a uniform
        // chunk re-scans the grid on every write (O(N^2) at 16³).
        let mut blocks = Vec::with_capacity(CHUNK_BLOCK_COUNT);
        for i in 0..CHUNK_BLOCK_COUNT {
            blocks.push(self.implied_block(coords, BlockIndex::from_linear(i).xyz())?);
        }
        let chunk = Chunk { blocks: ChunkBlocks::from_vec(blocks), chisel: Default::default() };
        Some(self.spawn_chunk_safe(coords, chunk))
    }

    /// Inserts a runtime chunk entity and hands the renderer the raw chunk
    /// and its place (`AddTerrainChunk`; the client derives voxels off-thread).
    /// `create_mesh`'s entity insert emits `RemoveEntity` on undo.
    fn spawn_chunk_safe(&mut self, coords: ChunkCoords, chunk: Chunk) -> EntityKey {
        let e = self.create_mesh(coords, chunk.clone());
        let pose = Pose::translation(
            (coords.x * crate::CHUNK_UNITS) as f32,
            (coords.y * crate::CHUNK_UNITS as i32) as f32,
            (coords.z * crate::CHUNK_UNITS) as f32,
        );
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddTerrainChunk(e, pose, Box::new(chunk)),
        ));
        e
    }

    /// Walks the block grid from `origin` along `dir` (world units, `dir`
    /// normalised) and returns the first solid block within `reach` metres.
    /// Amanatides–Woo voxel traversal at block scale.
    pub fn raycast_block(&self, origin: Vector, dir: Vector, reach: f32) -> Option<BlockHit> {
        self.raycast_block_in(origin, dir, reach)
    }

    fn raycast_block_in(&self, origin: Vector, dir: Vector, reach: f32) -> Option<BlockHit> {
        raycast_blocks(origin, dir, reach, |p| self.block_solid(p))
    }

    /// True if any non-fixed body's collider overlaps block `pos`. Keeps a
    /// placed block from burying a player (or any other moving entity).
    fn block_overlaps_body(&self, pos: BlockPos) -> bool {
        let size = BLOCK_VOXELS as f32;
        let lo = [pos[0] as f32 * size, pos[1] as f32 * size, pos[2] as f32 * size];
        let hi = [lo[0] + size, lo[1] + size, lo[2] + size];
        for (_, handle) in self.data.ecs.rigidbody.iter() {
            let Some(handle) = handle else { continue };
            let Some(body) = self.data.physics.bodies.get(*handle) else { continue };
            if body.is_fixed() {
                continue;
            }
            for &ch in body.colliders() {
                let Some(collider) = self.data.physics.colliders.get(ch) else { continue };
                let aabb = collider.compute_aabb();
                let (mins, maxs) = (aabb.mins, aabb.maxs);
                let overlaps = (0..3).all(|a| mins[a] < hi[a] && maxs[a] > lo[a]);
                if overlaps {
                    return true;
                }
            }
        }
        false
    }

    /// Sets one whole block, undo-safely: chunk data, render voxels and
    /// colliders. Creates the chunk entity if a placed block lands in a chunk
    /// slot that has none. Returns whether anything changed.
    pub fn set_block_safe(&mut self, pos: BlockPos, block: Block) -> bool {
        self.set_block_in(pos, block)
    }

    fn set_block_in(&mut self, pos: BlockPos, block: Block) -> bool {
        let changed = self.set_block_uncounted(pos, block);
        if changed {
            let cc = self.data.cells.cell_chunks;
            if let Some(cell) = (cc != 0).then(|| crate::cell_of_block(pos, cc)).flatten() {
                let n = self.data.cell_edits.get(&cell).copied().unwrap_or(0);
                self.data.cell_edits.insert(cell, n.wrapping_add(1));
            }
        }
        changed
    }

    fn set_block_uncounted(&mut self, pos: BlockPos, block: Block) -> bool {
        // Cell mode: a dormant cell's terrain lives in the server store; an
        // edit here would create a chunk the store never sees. Gated here, the
        // one funnel of both set_block_safe and player_edit_safe.
        if !self.block_in_active_cell(pos) {
            return false;
        }
        let Some((coords, (bx, by, bz))) = locate(pos) else { return false };
        let breaking = block.id == BlockId::AIR;
        if breaking && self.breaks_into_neighbour_ground(pos, coords) {
            return false;
        }
        // Bedrock takes no write at all, implied bedrock included; refused
        // before anything changes.
        if let Some(current) = self.current_block(coords, (bx, by, bz)) {
            if self.is_bedrock(coords, current) {
                return false;
            }
            // An implied slot that already holds `block`: nothing to do.
            if current == block && self.chunk_key_at(coords).is_none() {
                return false;
            }
        }
        // An implied chunk becomes real before the write lands in it, so its
        // other blocks keep the ground's values.
        let materialised = self.materialize_ground_safe(coords).is_some();
        let changed = self.write_block_safe(coords, (bx, by, bz), block) || materialised;
        if changed && breaking {
            // The new air must never border an implied chunk: materialise
            // every face neighbour in another chunk that still is one.
            for d in [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]] {
                let n = [pos[0] + d[0], pos[1] + d[1], pos[2] + d[2]];
                if let Some((nc, _)) = locate(n).filter(|(nc, _)| *nc != coords) {
                    self.materialize_ground_safe(nc);
                }
            }
        }
        changed
    }

    /// Writes one block into chunk `coords` (creating the chunk for a placed
    /// block in an empty slot). Returns whether anything changed.
    fn write_block_safe(&mut self, coords: ChunkCoords, (bx, by, bz): (usize, usize, usize), block: Block) -> bool {
        let Some(e) = self.chunk_key_at(coords) else {
            if block.id == BlockId::AIR {
                return false;
            }
            // An all-air new chunk: only reachable where the slot is not
            // implied ground (cell mode, a column without ground, or a slot
            // in or above the band) — `set_block_uncounted` materialises an
            // implied chunk before writing into it.
            debug_assert!(
                self.implied_block(coords, (bx, by, bz)).is_none(),
                "write into implied ground at {coords:?} without materialising it"
            );
            let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
            chunk.set_block(bx, by, bz, block);
            self.spawn_chunk_safe(coords, chunk);
            return true;
        };
        let Some(chunk) = self.data.ecs.chunk.try_get(e).as_ref() else { return false };
        let index = BlockIndex::from_xyz(bx, by, bz);
        let old_block = chunk.blocks.get(index);
        if old_block == block && !chunk.chisel.contains_key(&index) {
            return false;
        }
        let old = BlockDelta { index, block: old_block, chisel: chunk.chisel.get(&index).cloned() };
        let old_cells = crate::collider_cells_of_block(chunk, index);
        // LIFO: registered before the write, so the old block is re-sent
        // after the chunk value is restored.
        self.data.ecs.chunk.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetBlocks(e, vec![old]),
        ));
        self.data.ecs.chunk.set_chunk_block_safe(e, index, block, None);
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetBlocks(e, vec![BlockDelta { index, block, chisel: None }]),
        ));
        let new_cells = crate::collider_cells_of_block(self.data.ecs.chunk.get(e), index);
        self.edit_block_colliders_safe(e, &old_cells, &new_cells);
        true
    }

    /// Patches chunk body `e`'s colliders for one block edit: `old` and `new`
    /// are that block's collider cells before and after the write
    /// (`collider_cells_of_block`, chunk-local keys). Each grid's diff goes
    /// into its `Voxels` shape with one batched `set_voxels`, through
    /// `mutate_collider_safe` — its pre-write clone shares the shape's `Arc`,
    /// so `shape_mut` copies the shape on write and the undo puts the old one
    /// back. An edited shape hashes equal to a fresh build of the same keys
    /// (physics-next `voxels_batch_edit`), so the result is the one
    /// `rebuild_chunk_colliders_safe` / `create_mesh` would give.
    ///
    /// Falls back to the full rebuild when patching cannot express the edit:
    /// the body's enabled colliders are not exactly one `Voxels` shape per
    /// non-empty grid in `grids()` order (the genesis full-chunk cuboid, or
    /// anything unexpected), a grid gains its first cell, or a grid loses its
    /// last.
    fn edit_block_colliders_safe(&mut self, e: EntityKey, old: &crate::ColliderCells, new: &crate::ColliderCells) {
        use std::collections::HashSet;
        let Some(body) = *self.data.ecs.rigidbody.try_get(e) else { return };
        let Some(b) = self.data.physics.bodies.get(body) else { return };
        // The enabled colliders, which must be one voxels shape per grid,
        // coarsest first (what every builder attaches).
        let mut grids: Vec<(f32, rapier3d::prelude::ColliderHandle)> = Vec::with_capacity(3);
        let mut patchable = true;
        for &ch in b.colliders() {
            let Some(c) = self.data.physics.colliders.get(ch) else { continue };
            if !c.is_enabled() {
                continue;
            }
            match c.shape().as_voxels() {
                Some(v) if grids.last().is_none_or(|&(prev, _)| v.voxel_size().x < prev) => {
                    grids.push((v.voxel_size().x, ch));
                }
                _ => patchable = false,
            }
        }
        let scales: [(f32, &[rapier3d::math::IVector], &[rapier3d::math::IVector]); 3] = [
            (BLOCK_VOXELS as f32, &old.blocks, &new.blocks),
            (crate::HALF_BLOCK_VOXELS as f32, &old.halves, &new.halves),
            (1.0, &old.voxels, &new.voxels),
        ];
        let mut patches = Vec::with_capacity(3);
        if patchable {
            for (size, before, after) in scales {
                let after_set: HashSet<_> = after.iter().copied().collect();
                let before_set: HashSet<_> = before.iter().copied().collect();
                // Deterministic order: removals in `before` order, then
                // additions in `after` order (the sets only test membership).
                let removed: Vec<_> = before.iter().copied().filter(|k| !after_set.contains(k)).collect();
                let added: Vec<_> = after.iter().copied().filter(|k| !before_set.contains(k)).collect();
                if removed.is_empty() && added.is_empty() {
                    continue;
                }
                let Some(&(_, ch)) = grids.iter().find(|(s, _)| *s == size) else {
                    // A grid gains its first cell: a new collider, in order.
                    patchable = false;
                    break;
                };
                if added.is_empty() {
                    // Would this empty the grid? Every key of a block is
                    // distinct, so it does iff the shape holds no voxel
                    // beyond the removed ones.
                    let shape = self.data.physics.colliders.get(ch).and_then(|c| c.shape().as_voxels());
                    let held = shape.map_or(0, |v| v.voxels().take(removed.len() + 1).count());
                    if held <= removed.len() {
                        patchable = false;
                        break;
                    }
                }
                let edits: Vec<_> = removed.into_iter().map(|k| (k, false)).chain(added.into_iter().map(|k| (k, true))).collect();
                patches.push((ch, edits));
            }
        }
        if !patchable {
            let cells = crate::collider_cells(self.data.ecs.chunk.get(e));
            self.rebuild_chunk_colliders_safe(e, &cells);
            return;
        }
        for (ch, edits) in patches {
            self.data.physics.colliders.mutate_collider_safe(ch, move |c| {
                c.shape_mut().as_voxels_mut().expect("checked: a voxels shape").set_voxels(edits);
            });
        }
    }

    /// Brings a chunk body's colliders in line with `cells`, the same
    /// multi-scale decomposition `create_mesh` builds: whole blocks at block
    /// scale, solid 8³ chisel octants at half-block scale, other chisel
    /// detail at voxel scale. Existing colliders are reused in
    /// order and get their shape swapped in place; spare ones are disabled
    /// rather than removed (removal has no footprint-sized inverse); missing
    /// ones are attached. A genesis cuboid (fully solid chunk) is reused the
    /// same way, with its centring offset cleared.
    fn rebuild_chunk_colliders_safe(&mut self, e: EntityKey, cells: &crate::ColliderCells) {
        let Some(body) = *self.data.ecs.rigidbody.try_get(e) else { return };
        let user_data = e.data().as_ffi() as u128;
        let wanted: Vec<_> = cells
            .grids()
            .map(|(size, keys)| {
                ColliderBuilder::voxels(Vector::splat(size), keys)
                    .user_data(user_data)
                    .build()
            })
            .collect();
        let existing: Vec<_> = match self.data.physics.bodies.get(body) {
            Some(b) => b.colliders().to_vec(),
            None => return,
        };
        let mut wanted = wanted.into_iter();
        for ch in existing {
            match wanted.next() {
                Some(w) => {
                    let shape = w.shared_shape().clone();
                    self.data.physics.colliders.mutate_collider_safe(ch, move |c| {
                        c.set_shape(shape);
                        if c.position_wrt_parent().is_some_and(|p| *p != Pose::IDENTITY) {
                            c.set_position_wrt_parent(Pose::IDENTITY);
                        }
                        if !c.is_enabled() {
                            c.set_enabled(true);
                        }
                    });
                }
                None => {
                    let enabled = self.data.physics.colliders.get(ch).is_some_and(|c| c.is_enabled());
                    if enabled {
                        self.data.physics.colliders.mutate_collider_safe(ch, |c| c.set_enabled(false));
                    }
                }
            }
        }
        for w in wanted {
            self.attach_collider_safe(body, w);
        }
    }

    /// Runs one break or place for `client`'s player from its eye along its
    /// look. Returns whether a block changed. Invalid edits (nothing in reach,
    /// the target cell occupied, a body in the way) change nothing.
    pub fn player_edit_safe(&mut self, client: ClientId, action: EditAction) -> bool {
        let Some(&e) = self.data.player_entites.get(&client) else { return false };
        let Some(handle) = *self.data.ecs.rigidbody.try_get(e) else { return false };
        let Some(body) = self.data.physics.bodies.get(handle) else { return false };
        let Some(camera) = self.data.ecs.camera.try_get(e).as_ref() else { return false };
        let eye = body.translation() + Vector::new(0.0, PLAYER_EYE_OFFSET, 0.0);
        // Cameras look down -Z, as the renderer's Camera3d does.
        let dir = crate::look_quat(camera.yaw, camera.pitch) * Vector::new(0.0, 0.0, -1.0);
        let Some(hit) = self.raycast_block_in(eye, dir, REACH) else { return false };
        match action {
            EditAction::Break => self.set_block_in(hit.block, Block::new(BlockId::AIR)),
            EditAction::Place => {
                if hit.normal == [0; 3] {
                    return false;
                }
                let target = [
                    hit.block[0] + hit.normal[0],
                    hit.block[1] + hit.normal[1],
                    hit.block[2] + hit.normal[2],
                ];
                if self.block_solid(target) || self.block_overlaps_body(target) {
                    return false;
                }
                self.set_block_in(target, Block::new(PLACE_BLOCK))
            }
        }
    }
}

/// Pure Amanatides–Woo walk of the block grid from `origin` along `dir` (sim
/// units, `dir` normalised): the first block `solid` accepts within `reach`
/// blocks. Ties step x, then y, then z, so the walk is deterministic.
pub fn raycast_blocks(
    origin: Vector,
    dir: Vector,
    reach: f32,
    solid: impl Fn(BlockPos) -> bool,
) -> Option<BlockHit> {
    let size = BLOCK_VOXELS as f32;
    let p = [origin.x / size, origin.y / size, origin.z / size];
    let d = [dir.x, dir.y, dir.z];
    let mut cell = [
        p[0].floor() as i32,
        p[1].floor() as i32,
        p[2].floor() as i32,
    ];
    let mut step = [0i32; 3];
    let mut t_max = [f32::INFINITY; 3];
    let mut t_delta = [f32::INFINITY; 3];
    for a in 0..3 {
        if d[a] > 0.0 {
            step[a] = 1;
            t_delta[a] = 1.0 / d[a];
            t_max[a] = (cell[a] as f32 + 1.0 - p[a]) / d[a];
        } else if d[a] < 0.0 {
            step[a] = -1;
            t_delta[a] = -1.0 / d[a];
            t_max[a] = (p[a] - cell[a] as f32) / -d[a];
        }
    }
    let mut normal = [0i32; 3];
    loop {
        if solid(cell) {
            return Some(BlockHit {
                block: cell,
                normal,
            });
        }
        // Fixed tie order (x, then y, then z) keeps the walk deterministic.
        let mut axis = 0;
        if t_max[1] < t_max[axis] {
            axis = 1;
        }
        if t_max[2] < t_max[axis] {
            axis = 2;
        }
        if t_max[axis] > reach {
            return None;
        }
        cell[axis] += step[axis];
        normal = [0; 3];
        normal[axis] = -step[axis];
        t_max[axis] += t_delta[axis];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voxel::CHUNK_CLONES;

    /// A per-block edit must not copy the chunk: at 16³ blocks a clone is a
    /// 4096-block box per click, re-paid on every replay of the tick.
    #[test]
    fn edit_never_clones_the_chunk() {
        // Delta, not Full: Full-mode rollback clones the whole field for its
        // mismatch diagnostics (a harness cost, not the edit's). Delta also
        // checks the per-block entry's slot digest round-trips.
        let _mode = crate::state::delta_digest_tests::delta_mode();
        let (send, _recv) = crossbeam::channel::unbounded();
        let mut r = Rollback::new(Some(send));
        let coords = ChunkCoords::new(1, 0, 1);
        r.seed_terrain_genesis(vec![(coords, Chunk::flat_floor(16))]);
        let cb = CHUNK_BLOCKS as i32;
        let pos = [cb, 0, cb];
        let before = CHUNK_CLONES.with(|c| c.get());
        r.new_transaction();
        assert!(r.set_block_safe(pos, Block::new(BlockId::AIR)), "the floor block breaks");
        r.rollback();
        let after = CHUNK_CLONES.with(|c| c.get());
        assert_eq!(after, before, "a break + undo cloned the chunk {} times", after - before);
    }
}
