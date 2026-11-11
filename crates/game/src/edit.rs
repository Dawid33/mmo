//! Player tile editing: a left click breaks the tile under the crosshair, a
//! right click fills the one against the face it was hit on.
//!
//! Clicks arrive as ordinary tick-stamped `PlayerInput` mouse-button events,
//! so an edit runs inside the tick on both the predicting client and the
//! server, from the same authoritative eye position and look. Nothing new
//! crosses the wire.
//!
//! A player's edit is an intent to the tile's owner (`intents.rs`), applied at
//! the opening of the next tick: the eye ray reads the stud halo
//! (`stud_halo.rs`), so the tile may be a neighbour's, and the owner applies
//! it with its own refusals (bedrock, an occupied tile). The names
//! (`BlockPos`, `PLACE_BLOCK`, `BlockHit`) are kept from the voxel world: a
//! "block" here is a tile address.

use rapier3d::math::Vector;

use crate::{BlockId, ClientId, MouseButton, Rollback, BLOCK_UNITS};

/// How far a player can reach to break or place, in metres.
pub const REACH: f32 = 5.0;
/// The block a right click places. Dirt until the hotbar drives it.
pub const PLACE_BLOCK: BlockId = BlockId(1);
/// Mouse button that breaks the targeted block.
pub const BREAK_BUTTON: MouseButton = MouseButton::Left;
/// Mouse button that places a block against the targeted face.
pub const PLACE_BUTTON: MouseButton = MouseButton::Right;

/// Region-local block coordinates: block `b` spans world units
/// `[b * BLOCK_UNITS, (b + 1) * BLOCK_UNITS)` on each axis.
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


impl Rollback {
    /// True if any non-fixed body's collider overlaps tile `p` (its 1 m x 1.2 m x 1 m
    /// box). Keeps a placed tile from burying a player (or any other moving entity).
    pub(crate) fn stud_tile_overlaps_body(&self, p: crate::TilePos) -> bool {
        let (w, h) = (crate::tile_width_units(), crate::tile_height_units());
        let lo = [p[0] as f32 * w, crate::level_bottom_units(p[1]), p[2] as f32 * w];
        self.box_overlaps_body(lo, [lo[0] + w, lo[1] + h, lo[2] + w])
    }

    /// True if any non-fixed body's collider AABB overlaps the box `lo .. hi`
    /// (region-local sim units).
    fn box_overlaps_body(&self, lo: [f32; 3], hi: [f32; 3]) -> bool {
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

    /// `client`'s player's eye (sim units) and unit look direction, or
    /// `None` without a body and camera.
    pub fn eye_ray(&self, client: ClientId) -> Option<(Vector, Vector)> {
        let &e = self.data.player_entites.get(&client)?;
        let handle = (*self.data.ecs.rigidbody.try_get(e))?;
        let body = self.data.physics.bodies.get(handle)?;
        let camera = self.data.ecs.camera.try_get(e).as_ref()?;
        let eye = body.translation() + Vector::new(0.0, self.body_scale().eye_offset(), 0.0);
        // Cameras look down -Z, as the renderer's Camera3d does.
        let dir = crate::look_quat(camera.yaw, camera.pitch) * Vector::new(0.0, 0.0, -1.0);
        Some((eye, dir))
    }

    /// One creative (Fortress) break or place for `client`'s player from its
    /// eye along its look: an intent to the block's owner (into `out` when
    /// that is a neighbour), applied at the next tick's opening. Returns
    /// whether one was sent. Edits seen to be invalid (nothing in reach, the
    /// target cell occupied, a body in the way) send nothing; the owner
    /// checks again.
    pub fn player_edit_safe(&mut self, client: ClientId, action: EditAction, out: &mut Vec<crate::IntentMsg>) -> bool {
        use crate::intents::Effects as _;
        // The tile under the crosshair breaks whole, the tile against the hit face is
        // filled (`Rollback::apply_stud_tile_intent_safe`).
        let Some(hit) = self.eye_stud_hit(client) else { return false };
        let Some(by) = self.client_body_id(client) else { return false };
        let (tile, intent) = match action {
            EditAction::Break => (hit.tile, crate::Intent::BreakBlock { by, survival: false }),
            EditAction::Place => {
                if hit.inside
                    || self.stud_tile(hit.outside).fill() == crate::TileFill::Solid
                    || self.stud_tile_overlaps_body(hit.outside)
                {
                    return false;
                }
                (hit.outside, crate::Intent::PlaceBlock { block: PLACE_BLOCK, by, survival: false })
            }
        };
        let p = crate::WorldBlockPos::from_local(*self.data.region, crate::BlockPos::new(tile[0], tile[1], tile[2]));
        crate::intents::RegionEffects { data: self, out }.send(crate::Target::Block(p), intent);
        true
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
    let size = BLOCK_UNITS as f32;
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
