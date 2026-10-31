//! Adventure mode: a client either commands the fortress from the top-down
//! camera ([`PlayMode::Fortress`]) or plays its one body
//! ([`PlayMode::Adventure`]), walked by `Order::MoveTo` goals rather than the
//! keyboard (`fps_cam_mode` is untouched). The mode is per-client rollback
//! state on [`crate::Client`], so it travels with the client's bundle across
//! seams.

use rapier3d::math::{Pose, Vector};
use state_hash::StateHash;

use crate::movement::{PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS};
use crate::{nearest_standable, ClientId, RegionCoords, Rollback, TilePos, BLOCK_VOXELS};

/// How a client plays: the commander camera or one walking body.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub enum PlayMode {
    #[default]
    Fortress,
    Adventure,
}

/// Where an adventurer is asked to walk: a block column of `region`
/// (region-local to it), optionally at a level `y` (the air tile the feet
/// occupy).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct Goal {
    pub region: RegionCoords,
    pub column: [i32; 2],
    pub y: Option<i32>,
}

/// Gap (voxel units) left between the capsule bottom and the tile floor when
/// a body is placed on a tile; small enough that the next tick's ground snap
/// grounds it.
pub const ADVENTURE_PLACE_CLEARANCE_UNITS: f32 = 0.0;

/// Body-centre position (region-local voxel units) of a player standing on
/// tile `at`: the column's centre, feet on the floor (the top of block
/// `at.y - 1`).
pub fn body_on_tile(at: TilePos) -> Vector {
    let bv = BLOCK_VOXELS as f32;
    Vector::new(
        (at[0] as f32 + 0.5) * bv,
        at[1] as f32 * bv + PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS + ADVENTURE_PLACE_CLEARANCE_UNITS,
        (at[2] as f32 + 0.5) * bv,
    )
}

impl Rollback {
    /// Switches `client`'s play mode, undo-safely. `false` = unknown client,
    /// nothing changed. Entering Adventure while in Adventure is a no-op.
    /// Entering Adventure with `at` moves the body onto the standable tile
    /// nearest `at` in its column (`nearest_standable`), or to the region's
    /// spawn column when there is none; with no `at` the body stays where it
    /// is. Fortress leaves the body where it stands.
    pub fn set_play_mode_safe(&mut self, client: ClientId, mode: PlayMode, at: Option<TilePos>) -> bool {
        let Some(old) = self.data.clients.get(&client).map(|c| c.mode) else { return false };
        if old == mode {
            return true;
        }
        // UndoMap::get_mut logs the prior Client: its undo restores `mode`.
        if let Some(c) = self.data.clients.get_mut(&client) {
            c.mode = mode;
        }
        match mode {
            PlayMode::Adventure => {
                if let Some(at) = at {
                    self.place_body_safe(client, at);
                }
            }
            // Task 3: clear the client's walker here.
            PlayMode::Fortress => {}
        }
        true
    }

    /// Teleports `client`'s body onto the standable tile nearest `at` in
    /// its column, else over the region's spawn column (`CreateClient`'s
    /// spawn rule). Keeps the body's rotation.
    fn place_body_safe(&mut self, client: ClientId, at: TilePos) {
        let Some(key) = self.data.player_entites.get(&client).copied() else { return };
        let Some(handle) = *self.data.ecs.rigidbody.try_get(key) else { return };
        let Some(body) = self.data.physics.bodies.get(handle) else { return };
        let rotation = body.position().rotation;
        let translation = match nearest_standable(self, [at[0], at[2]], Some(at[1])) {
            Some(tile) => body_on_tile(tile),
            None => {
                let xz = self.spawn_xz();
                Vector::new(xz.0, self.player_spawn_y(xz), xz.1)
            }
        };
        self.set_body_pose_safe(key, Pose::from_parts(translation, rotation));
    }
}
