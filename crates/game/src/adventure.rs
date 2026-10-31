//! Adventure mode: a client either commands the fortress from the top-down
//! camera ([`PlayMode::Fortress`]) or plays its one body
//! ([`PlayMode::Adventure`]), walked by `Order::MoveTo` goals rather than the
//! keyboard (`fps_cam_mode` is untouched). The mode is per-client rollback
//! state on [`crate::Client`], so it travels with the client's bundle across
//! seams.

use std::collections::BTreeSet;

use rapier3d::math::{Pose, Vector};
use state_hash::StateHash;

use crate::movement::{MoveKeys, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, STEP_HEIGHT, UNITS_PER_METER};
use crate::{
    find_path, nearest_standable, standable, ClientId, RegionCoords, Rollback, TilePos, TileSource, BLOCK_VOXELS,
    MAX_PATH_NODES, REGION_BLOCKS, TICK_RATE,
};

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
            PlayMode::Fortress => {
                if let Some(&key) = self.data.player_entites.get(&client) {
                    if self.data.ecs.walker.try_get(key).is_some() {
                        self.data.ecs.walker.set_safe(key, None);
                    }
                }
            }
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
        // A teleport ends any fall or jump: no stale vertical velocity (or
        // flight) carries over onto the new tile.
        if let Some(m) = *self.data.ecs.motion.try_get(key) {
            if m.vel_y != 0.0 || m.flying {
                self.data.ecs.motion.set_safe(key, Some(crate::Motion { vel_y: 0.0, flying: false, ..m }));
            }
        }
    }

    /// Walks `client`'s adventurer toward `goal` (region-local to
    /// `goal.region`; `here` is this region), undo-safely. `false` with no
    /// change when the client is not in Adventure, the goal is more than
    /// [`MAX_GOAL_REGION_STEP`] regions away (Chebyshev), or there is no
    /// path. A same-region goal is walked to `nearest_standable(goal.column,
    /// goal.y)`; a goal elsewhere to [`edge_toward`]'s edge tile, then out
    /// along [`Walker::exit`]. Replaces any walk in progress.
    pub fn move_to_safe(&mut self, client: ClientId, goal: Goal, here: RegionCoords) -> bool {
        if self.data.clients.get(&client).map(|c| c.mode) != Some(PlayMode::Adventure) {
            return false;
        }
        let Some(key) = self.data.player_entites.get(&client).copied() else { return false };
        let Some(feet) = self.body_feet(key) else { return false };
        let Some(from) = current_tile(self, feet) else { return false };
        let Some((path, exit)) = plan_walk(self, from, &goal, here) else { return false };
        self.data.ecs.walker.set_safe(key, Some(Walker::new(goal, path, exit)));
        true
    }

    /// Advances every adventurer's walk by one tick, before the controllers
    /// steer: passes reached waypoints, removes the walk on arrival, and
    /// handles a stuck body (re-plan once, then give up). Every change goes
    /// through `walker.set_safe`.
    pub fn tick_walkers_safe(&mut self, here: RegionCoords) {
        let walking: Vec<crate::EntityKey> = self
            .data
            .player_entites
            .iter()
            .filter(|(c, _)| self.data.clients.get(c).is_some_and(|c| c.mode == PlayMode::Adventure))
            .map(|(_, &e)| e)
            .filter(|&e| self.data.ecs.walker.try_get(e).is_some())
            .collect();
        for key in walking {
            let Some(old) = self.data.ecs.walker.try_get(key).clone() else { continue };
            let Some(feet) = self.body_feet(key) else { continue };
            let mut w = old.clone();
            let last = w.path.len().saturating_sub(1) as u32;
            let mut arrived = false;
            while (w.next as usize) < w.path.len() && reached(w.path[w.next as usize], feet) {
                if w.next < last {
                    w.next += 1;
                } else {
                    arrived = w.exit.is_none();
                    break;
                }
            }
            if arrived || w.path.is_empty() {
                self.data.ecs.walker.set_safe(key, None);
                continue;
            }
            let remaining = w.remaining();
            if remaining < w.best_dist {
                w.best_dist = remaining;
                w.stalled = 0;
            } else {
                w.stalled += 1;
            }
            if w.stalled >= STUCK_TICKS {
                if w.replanned {
                    self.data.ecs.walker.set_safe(key, None);
                    continue;
                }
                // Re-plan once from where the body stands. With no new
                // path the old one is kept: a body still stuck after
                // another STUCK_TICKS gives up.
                if let Some((path, exit)) = current_tile(self, feet).and_then(|from| plan_walk(self, from, &w.goal, here))
                {
                    w.next = Walker::first_next(&path);
                    w.path = path;
                    w.exit = exit;
                }
                w.replanned = true;
                w.stalled = 0;
                w.best_dist = w.remaining();
            }
            if w != old {
                self.data.ecs.walker.set_safe(key, Some(w));
            }
        }
    }

    /// Region-local feet position (voxel units) of entity `key`'s body.
    fn body_feet(&self, key: crate::EntityKey) -> Option<Vector> {
        let handle = (*self.data.ecs.rigidbody.try_get(key))?;
        let t = self.data.physics.bodies.get(handle)?.translation();
        Some(feet_of(t))
    }
}

/// Horizontal distance (metres) within which a waypoint counts as reached.
pub const WAYPOINT_REACH_M: f32 = 0.3;
/// How long (ms) a walk may go without getting closer before it re-plans,
/// then gives up.
pub const STUCK_MS: u64 = 2000;
/// [`STUCK_MS`] in ticks ([`TICK_RATE`] is the tick period in ms).
pub const STUCK_TICKS: u32 = (STUCK_MS / TICK_RATE) as u32;
/// How far (regions, Chebyshev) a `MoveTo` goal may lie from the walker's
/// region.
pub const MAX_GOAL_REGION_STEP: i32 = 1;

/// An adventurer walking a planned path toward a `MoveTo` goal. Lives in the
/// rollback ECS (`Ecs::walker`) on the walking body.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct Walker {
    pub goal: Goal,
    /// The planned tiles, inclusive of the start and the end.
    pub path: Vec<TilePos>,
    /// Index into `path` of the waypoint being walked to.
    pub next: u32,
    /// Outward `(dx, dz)` unit step once the path's last tile is the region
    /// edge toward a goal in another region.
    pub exit: Option<[i8; 2]>,
    /// Fewest tiles left so far (progress = going below it).
    pub best_dist: u32,
    /// Ticks since the last progress.
    pub stalled: u32,
    /// The walk was re-planned after getting stuck (it gives up next time).
    pub replanned: bool,
}

impl Walker {
    fn new(goal: Goal, path: Vec<TilePos>, exit: Option<[i8; 2]>) -> Self {
        let next = Self::first_next(&path);
        let mut w = Walker { goal, path, next, exit, best_dist: 0, stalled: 0, replanned: false };
        w.best_dist = w.remaining();
        w
    }

    /// `path[0]` is where the body stands: walk to the one after it.
    fn first_next(path: &[TilePos]) -> u32 {
        (path.len() > 1) as u32
    }

    /// Tiles still to reach, the current waypoint included.
    pub fn remaining(&self) -> u32 {
        (self.path.len() as u32).saturating_sub(self.next)
    }
}

/// Feet position of a player body whose centre is at `t` (voxel units).
pub fn feet_of(t: Vector) -> Vector {
    t - Vector::Y * (PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS)
}

/// The tile feet at `feet` (voxel units) occupy: the block column under
/// them, at the level nearest the feet.
pub fn feet_tile(feet: Vector) -> TilePos {
    let bv = BLOCK_VOXELS as f32;
    [(feet.x / bv).floor() as i32, (feet.y / bv + 0.5).floor() as i32, (feet.z / bv).floor() as i32]
}

/// The standable tile a body with feet at `feet` walks from: its own tile,
/// else the nearest standable one in its column.
fn current_tile(src: &impl TileSource, feet: Vector) -> Option<TilePos> {
    let t = feet_tile(feet);
    if standable(src, t) {
        Some(t)
    } else {
        nearest_standable(src, [t[0], t[2]], Some(t[1]))
    }
}

/// Whether feet at `feet` have reached waypoint `p`: within
/// [`WAYPOINT_REACH_M`] of its column centre horizontally, on its level.
fn reached(p: TilePos, feet: Vector) -> bool {
    let bv = BLOCK_VOXELS as f32;
    let dx = feet.x - (p[0] as f32 + 0.5) * bv;
    let dz = feet.z - (p[2] as f32 + 0.5) * bv;
    let r = WAYPOINT_REACH_M * UNITS_PER_METER;
    dx * dx + dz * dz <= r * r && (feet.y / bv - p[1] as f32).abs() < 0.5
}

/// `goal.region - here`, when within [`MAX_GOAL_REGION_STEP`].
fn region_step(goal: &Goal, here: RegionCoords) -> Option<[i32; 2]> {
    let d = [goal.region.x - here.x, goal.region.z - here.z];
    (d[0].abs().max(d[1].abs()) <= MAX_GOAL_REGION_STEP).then_some(d)
}

/// The path (and exit step) from `from` toward `goal`, as `move_to_safe`
/// plans it.
fn plan_walk(
    src: &impl TileSource,
    from: TilePos,
    goal: &Goal,
    here: RegionCoords,
) -> Option<(Vec<TilePos>, Option<[i8; 2]>)> {
    let d = region_step(goal, here)?;
    let (to, exit) = if d == [0, 0] {
        (nearest_standable(src, goal.column, goal.y)?, None)
    } else {
        (edge_toward(src, from, goal, here)?, Some([d[0].signum() as i8, d[1].signum() as i8]))
    };
    find_path(src, from, to, MAX_PATH_NODES).map(|p| (p, exit))
}

/// The standable tile on this region's edge facing `goal` (in another
/// region, at most [`MAX_GOAL_REGION_STEP`] away) whose column is nearest
/// the straight line from `from` to the goal; ties go to the lowest `(x,
/// z)`. Its level is the standable one nearest `from`'s. `None` when the
/// goal is in `here` or too far, or no facing column has a standable tile.
pub fn edge_toward(src: &impl TileSource, from: TilePos, goal: &Goal, here: RegionCoords) -> Option<TilePos> {
    let d = region_step(goal, here)?;
    if d == [0, 0] {
        return None;
    }
    let n = REGION_BLOCKS as i32;
    // The goal column in this region's block coordinates.
    let goal_xz = [(goal.column[0] + d[0] * n) as i64, (goal.column[1] + d[1] * n) as i64];
    let (fx, fz) = (from[0] as i64, from[2] as i64);
    let (lx, lz) = (goal_xz[0] - fx, goal_xz[1] - fz);
    let edge = |s: i32| if s > 0 { n - 1 } else { 0 };
    let mut columns = BTreeSet::new();
    if d[0] != 0 {
        columns.extend((0..n).map(|z| [edge(d[0]), z]));
    }
    if d[1] != 0 {
        columns.extend((0..n).map(|x| [x, edge(d[1])]));
    }
    // Twice the triangle area over the line: the distance to it, scaled by
    // the line's (shared) length.
    let off_line = |c: &[i32; 2]| (lx * (c[1] as i64 - fz) - lz * (c[0] as i64 - fx)).abs();
    let mut ranked: Vec<_> = columns.into_iter().map(|c| (off_line(&c), c)).collect();
    ranked.sort();
    ranked.into_iter().find_map(|(_, c)| nearest_standable(src, c, Some(from[1])))
}

/// One tick of walking intent for a body with feet at `feet` (voxel units):
/// forward, yawed toward the next waypoint's column centre (or, on the last
/// tile of a walk that leaves the region, a block past it along `exit`); jump held when the
/// waypoint is more than [`STEP_HEIGHT`] above the feet and the body is
/// grounded. Never sprints. The yaw follows `movement::plan`'s convention:
/// forward is local −Z, so a heading `(dx, dz)` is `atan2(−dx, −dz)`.
///
/// Jump is held, never pressed: `plan` toggles flight on a double tap, and
/// a body re-jumping on landing could tap twice inside the window.
pub fn steer(walker: &Walker, feet: Vector, grounded: bool) -> (MoveKeys, f32) {
    let bv = BLOCK_VOXELS as f32;
    let Some(&wp) = walker.path.get(walker.next as usize) else { return (MoveKeys::default(), 0.0) };
    let centre = |p: TilePos| ((p[0] as f32 + 0.5) * bv, (p[2] as f32 + 0.5) * bv);
    let on_last = walker.next as usize + 1 >= walker.path.len();
    let (cx, cz) = centre(wp);
    // Leaving the region: aim a block past the edge tile, so the heading
    // keeps pointing out once the body is on it.
    let (cx, cz) = match walker.exit {
        Some([ex, ez]) if on_last => (cx + ex as f32 * bv, cz + ez as f32 * bv),
        _ => (cx, cz),
    };
    let (dx, dz) = (cx - feet.x, cz - feet.z);
    let yaw = libm::atan2f(-dx, -dz);
    let climb = grounded && wp[1] as f32 * bv - feet.y > STEP_HEIGHT * UNITS_PER_METER;
    let keys = MoveKeys { forward: true, jump_held: climb, ..MoveKeys::default() };
    (keys, yaw)
}
