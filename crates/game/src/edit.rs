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


use parry3d::math::{Isometry, Real, Vector};
use rapier3d::prelude::ColliderBuilder;
use slotmapd::Key as _;

use crate::movement::PLAYER_EYE_OFFSET;
use crate::{
    Block, BlockId, BlockIndex, Chunk, ChunkCoords, ClientId, EntityKey, GameDataTransactionKind,
    GameDataUpdate, GameDataUpdateKind, IsometryReal, MouseButton, Rollback, BLOCK_VOXELS, CHUNK_BLOCKS,
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
fn locate(pos: BlockPos) -> Option<(ChunkCoords, (usize, usize, usize))> {
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

impl Rollback {
    fn block_solid(&self, pos: BlockPos) -> bool {
        let Some((coords, local)) = locate(pos) else { return false };
        let Some(e) = self.chunk_key_at(coords) else { return false };
        self.data.ecs.chunk.try_get(e).as_ref().is_some_and(|c| cell_solid(c, local))
    }

    /// Walks the block grid from `origin` along `dir` (world units, `dir`
    /// normalised) and returns the first solid block within `reach` metres.
    /// Amanatides–Woo voxel traversal at block scale.
    pub fn raycast_block(&self, origin: Vector<Real>, dir: Vector<Real>, reach: f32) -> Option<BlockHit> {
        self.raycast_block_in(origin, dir, reach)
    }

    fn raycast_block_in(
        &self,
        origin: Vector<Real>,
        dir: Vector<Real>,
        reach: f32,
    ) -> Option<BlockHit> {
        let size = BLOCK_VOXELS as f32;
        let p = [origin.x.0 / size, origin.y.0 / size, origin.z.0 / size];
        let d = [dir.x.0, dir.y.0, dir.z.0];
        let mut cell = [p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32];
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
            if self.block_solid(cell) {
                return Some(BlockHit { block: cell, normal });
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
                let overlaps = (0..3).all(|a| mins[a].0 < hi[a] && maxs[a].0 > lo[a]);
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
        // Cell mode: a dormant cell's terrain lives in the server store; an
        // edit here would create a chunk the store never sees. Gated here, the
        // one funnel of both set_block_safe and player_edit_safe.
        if !self.block_in_active_cell(pos) {
            return false;
        }
        let Some((coords, (bx, by, bz))) = locate(pos) else { return false };
        let Some(e) = self.chunk_key_at(coords) else {
            if block.id == BlockId::AIR {
                return false;
            }
            let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
            chunk.set_block(bx, by, bz, block);
            let voxels = crate::derive_voxels(&chunk.blocks, &chunk.chisel);
            // create_mesh's entity insert emits RemoveEntity on undo.
            let e = self.create_mesh(coords, chunk);
            // create_mesh tells the renderer nothing but CreateEntity: give
            // the new chunk its place and its voxels.
            let pose = IsometryReal::translation(
                Real::from((coords.x * 32) as f32),
                Real::from((coords.y * 32) as f32),
                Real::from((coords.z * 32) as f32),
            );
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetEntityPosition(e, pose),
            ));
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetVoxelComponent(e, Some(voxels)),
            ));
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
                ColliderBuilder::voxels(Vector::repeat(Real::from(size)), keys)
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
                        if c.position_wrt_parent().is_some_and(|p| *p != Isometry::identity()) {
                            c.set_position_wrt_parent(Isometry::identity());
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
        let eye = body.translation() + Vector::new(Real::from(0.0), Real::from(PLAYER_EYE_OFFSET), Real::from(0.0));
        // Cameras look down -Z, as the renderer's Camera3d does.
        let dir = crate::look_quat(camera.yaw, camera.pitch)
            .transform_vector(&Vector::new(Real::from(0.0), Real::from(0.0), Real::from(-1.0)));
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
