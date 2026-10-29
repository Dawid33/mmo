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
//! (`Component::set_safe`), the renderer (`SetVoxelComponent` both ways), and
//! the chunk's colliders (shape swapped in place, see
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
    Block, BlockId, BlockIndex, Chunk, ChunkCoords, ClientId, EntityKey, GameDataTransactionKind,
    GameDataUpdate, GameDataUpdateKind, Pose, MouseButton, Rollback, BLOCK_VOXELS, CHUNK_BLOCKS,
    REGION_CHUNKS,
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
        let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
        for bx in 0..CHUNK_BLOCKS {
            for by in 0..CHUNK_BLOCKS {
                for bz in 0..CHUNK_BLOCKS {
                    chunk.set_block(bx, by, bz, self.implied_block(coords, (bx, by, bz))?);
                }
            }
        }
        Some(self.spawn_chunk_safe(coords, chunk))
    }

    /// Inserts a runtime chunk entity and tells the renderer its place and
    /// its voxels (`create_mesh` alone sends nothing but `CreateEntity`).
    /// `create_mesh`'s entity insert emits `RemoveEntity` on undo.
    fn spawn_chunk_safe(&mut self, coords: ChunkCoords, chunk: Chunk) -> EntityKey {
        let voxels = crate::derive_voxels(&chunk.blocks, &chunk.chisel);
        let e = self.create_mesh(coords, chunk);
        let pose = Pose::translation(
            (coords.x * 32) as f32,
            (coords.y * 32) as f32,
            (coords.z * 32) as f32,
        );
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityPosition(e, pose),
        ));
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetVoxelComponent(e, Some(voxels)),
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
        let Some(old) = self.data.ecs.chunk.try_get(e).clone() else { return false };
        let bi = BlockIndex::from_xyz(bx, by, bz);
        if old.block_at(bx, by, bz) == block && !old.chisel.contains_key(&bi) {
            return false;
        }
        let mut new = old.clone();
        new.set_block(bx, by, bz, block);
        let cells = crate::collider_cells(&new.blocks, &new.chisel);
        let new_voxels = crate::derive_voxels(&new.blocks, &new.chisel);
        // LIFO: registered before set_safe, so the old voxels are re-sent
        // after the chunk value is restored.
        self.data.ecs.chunk.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetVoxelComponent(e, Some(crate::derive_voxels(&old.blocks, &old.chisel))),
        ));
        self.data.ecs.chunk.set_safe(e, Some(new));
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetVoxelComponent(e, Some(new_voxels)),
        ));
        self.rebuild_chunk_colliders_safe(e, &cells);
        true
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
