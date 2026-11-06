//! Adventure mode: a client either commands the fortress from the top-down
//! camera ([`PlayMode::Fortress`]) or plays its one body
//! ([`PlayMode::Adventure`]), walked by `Order::MoveTo` goals rather than the
//! keyboard (`fps_cam_mode` is untouched). The mode is per-client rollback
//! state on [`crate::Client`], so it travels with the client's bundle across
//! seams.

use rapier3d::math::{Pose, Vector};
use state_hash::StateHash;

use crate::movement::{MoveKeys, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, STEP_HEIGHT, UNITS_PER_METER};
use crate::{
    nearest_standable, standable, walk_ups, ClientId, MapExtent, PathPlan, RegionCoords, Rollback, TilePos, TileSource,
    WorldBlockPos, BLOCK_VOXELS, MAX_PATH_NODES, REGION_BLOCKS, TICK_RATE,
};

/// How a client plays: the commander camera or one walking body.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub enum PlayMode {
    #[default]
    Fortress,
    Adventure,
}

/// Where an adventurer is asked to walk: a WORLD block column, any number
/// of regions away, optionally at a level `y` (the air tile the feet
/// occupy). `path_plan.rs` plans the walk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct Goal {
    /// World block `[x, z]` ([`WorldBlockPos`]'s frame).
    pub column: [i64; 2],
    pub y: Option<i32>,
}

impl Goal {
    /// The goal at world block `p`, at its level.
    pub fn at(p: WorldBlockPos) -> Self {
        Goal { column: [p.x, p.z], y: Some(p.y) }
    }

    /// The goal at block column `column` of region `rc` (region-local; it may
    /// lie past the region's edge).
    pub fn local(rc: RegionCoords, column: [i32; 2], y: Option<i32>) -> Self {
        let w = WorldBlockPos::from_local(rc, crate::BlockPos::new(column[0], 0, column[1]));
        Goal { column: [w.x, w.z], y }
    }

    /// The region the goal's column lies in; `None` past the `i32` region
    /// grid (untrusted input).
    pub fn region(&self) -> Option<RegionCoords> {
        let n = REGION_BLOCKS as i64;
        let x = i32::try_from(self.column[0].div_euclid(n)).ok()?;
        let z = i32::try_from(self.column[1].div_euclid(n)).ok()?;
        Some(RegionCoords::new(x, z))
    }

    /// The goal's column in `here`'s block frame (outside `0..REGION_BLOCKS`
    /// for a goal elsewhere); `None` when it does not fit an `i32`.
    pub fn local_column(&self, here: RegionCoords) -> Option<[i32; 2]> {
        let n = REGION_BLOCKS as i64;
        let x = self.column[0].checked_sub(i64::from(here.x) * n)?;
        let z = self.column[1].checked_sub(i64::from(here.z) * n)?;
        Some([i32::try_from(x).ok()?, i32::try_from(z).ok()?])
    }
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
    /// is. Fortress leaves the body where it stands and closes the survival
    /// state ([`crate::Client::leave_survival`]: inventory, chest, mining;
    /// a cursor stack that no longer fits is dropped at the feet).
    pub fn set_play_mode_safe(&mut self, client: ClientId, mode: PlayMode, at: Option<TilePos>) -> bool {
        // `at` comes off the wire: a tile outside the region is refused
        // before anything changes; its level is clamped to the scan range.
        let at = match at {
            Some(t) => match clamp_tile(self, t) {
                Some(t) => Some(t),
                None => return false,
            },
            None => None,
        };
        let Some(old) = self.data.clients.get(&client).map(|c| c.mode) else { return false };
        if old == mode {
            return true;
        }
        // UndoMap::get_mut logs the prior Client: its undo restores `mode`
        // and the survival state below.
        let mut rest = None;
        if let Some(c) = self.data.clients.get_mut(&client) {
            c.mode = mode;
            if mode == PlayMode::Fortress {
                // The survival screens close and mining stops; the cursor
                // stack goes back into the inventory.
                rest = c.leave_survival();
            }
        }
        if let Some(rest) = rest {
            self.drop_at_feet_safe(client, rest);
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

    /// Walks `client`'s adventurer toward world `goal`, undo-safely
    /// (`here` is this region). `false` with no change when the client is not
    /// in Adventure or there is no path ([`PathPlan::plan`]: to the goal
    /// through this region and its halo, else into the next region of the
    /// coarse route toward it; a goal off `map` — past the world's outer seam
    /// — is walked to the nearest column inside it). Replaces any walk in
    /// progress.
    pub fn move_to_safe(&mut self, client: ClientId, goal: Goal, here: RegionCoords, map: Option<MapExtent>) -> bool {
        if self.data.clients.get(&client).map(|c| c.mode) != Some(PlayMode::Adventure) {
            return false;
        }
        let Some(key) = self.data.player_entites.get(&client).copied() else { return false };
        self.walk_entity_safe(key, goal, here, map)
    }

    /// Walks entity `e` toward `goal` (see [`Self::move_to_safe`] for the
    /// goal rules), undo-safely: the part of `MoveTo` after the entity is
    /// resolved, shared with people. `false` with no change when it has no
    /// body or there is no path.
    pub fn walk_entity_safe(&mut self, e: crate::EntityKey, goal: Goal, here: RegionCoords, map: Option<MapExtent>) -> bool {
        let Some(walker) = self.plan_walker(e, goal, here, map) else { return false };
        self.data.ecs.walker.set_safe(e, Some(walker));
        true
    }

    /// A fresh walk for entity `e` toward `goal`, from the tile its body
    /// stands on; `None` without a body or a path.
    pub(crate) fn plan_walker(&self, e: crate::EntityKey, goal: Goal, here: RegionCoords, map: Option<MapExtent>) -> Option<Walker> {
        let from = current_tile(self, self.body_feet(e)?)?;
        let plan = PathPlan::plan(self, from, &goal, here, map, MAX_PATH_NODES)?;
        let walk_up = walk_ups(self, &plan.local_path(here));
        Some(Walker::new(goal, plan, walk_up))
    }

    /// Advances every adventurer's walk by one tick, before the controllers
    /// steer: passes reached waypoints, removes the walk on arrival, plans
    /// the next stretch at the end of one that only crosses into the next
    /// region of the route, and handles a stuck body (re-plan once, then
    /// give up). Every change goes through `walker.set_safe`.
    pub fn tick_walkers_safe(&mut self, here: RegionCoords, map: Option<MapExtent>) {
        // A body the keyboard drives (first person) is not on a walk.
        let driven: Vec<crate::EntityKey> = self
            .data
            .player_entites
            .iter()
            .filter(|(c, _)| self.data.clients.get(c).is_some_and(|c| c.fps_cam_mode))
            .map(|(_, &e)| e)
            .filter(|&e| self.data.ecs.walker.try_get(e).is_some())
            .collect();
        for key in driven {
            self.data.ecs.walker.set_safe(key, None);
        }
        let walking: Vec<crate::EntityKey> = self
            .data
            .player_entites
            .iter()
            .filter(|(c, _)| self.data.clients.get(c).is_some_and(|c| c.mode == PlayMode::Adventure))
            .map(|(_, &e)| e)
            .chain(self.data.person_entities.values().copied())
            .filter(|&e| self.data.ecs.walker.try_get(e).is_some())
            .collect();
        for key in walking {
            let Some(old) = self.data.ecs.walker.try_get(key).clone() else { continue };
            let Some(feet) = self.body_feet(key) else { continue };
            let mut w = old.clone();
            let last = w.path.len().saturating_sub(1) as u32;
            let mut at_end = false;
            while w.waypoint(w.next, here).is_some_and(|p| reached(p, feet)) {
                if w.next < last {
                    w.next += 1;
                } else {
                    at_end = true;
                    break;
                }
            }
            if w.path.is_empty() || (at_end && w.to_goal) {
                self.data.ecs.walker.set_safe(key, None);
                continue;
            }
            if at_end {
                // The end of a stretch into the next region of the route
                // (normally the arrival there plans the next one first).
                let next = self.plan_walker(key, w.goal, here, map);
                self.data.ecs.walker.set_safe(key, next);
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
                if let Some(fresh) = self.plan_walker(key, w.goal, here, map) {
                    w = Walker { replanned: w.replanned, ..fresh };
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
    pub(crate) fn body_feet(&self, key: crate::EntityKey) -> Option<Vector> {
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
/// Stalled ticks after which [`steer`] holds jump on a climb even when the
/// step walks up a slope, so a failed walk-up recovers by jumping instead of
/// running into the give-up.
pub const WALK_UP_JUMP_AFTER_STALL_TICKS: u32 = STUCK_TICKS / 2;

/// An adventurer walking a planned path toward a `MoveTo` goal. Lives in the
/// rollback ECS (`Ecs::walker`) on the walking body and travels across seams
/// in its bundle: the path is in WORLD blocks, so the region it arrives in
/// walks on along it (`Rollback::continue_walk_safe`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct Walker {
    pub goal: Goal,
    /// The planned tiles in world blocks, inclusive of the start and the end.
    pub path: Vec<WorldBlockPos>,
    /// Per tile of `path`: the step onto it walks up a slope, no jump
    /// ([`crate::walk_ups`]).
    pub walk_up: Vec<bool>,
    /// Index into `path` of the waypoint being walked to.
    pub next: u32,
    /// `path` ends on the goal's tile ([`PathPlan::reaches_goal`]); else it
    /// ends a few blocks into the next region of the route, and the walk is
    /// planned on from there.
    pub to_goal: bool,
    /// Fewest tiles left so far (progress = going below it).
    pub best_dist: u32,
    /// Ticks since the last progress.
    pub stalled: u32,
    /// The walk was re-planned after getting stuck (it gives up next time).
    pub replanned: bool,
}

impl Walker {
    pub(crate) fn new(goal: Goal, plan: PathPlan, walk_up: Vec<bool>) -> Self {
        let next = (plan.path.len() > 1) as u32;
        let mut w = Walker {
            goal,
            path: plan.path,
            walk_up,
            next,
            to_goal: plan.reaches_goal,
            best_dist: 0,
            stalled: 0,
            replanned: false,
        };
        w.best_dist = w.remaining();
        w
    }

    /// Tiles still to reach, the current waypoint included.
    pub fn remaining(&self) -> u32 {
        (self.path.len() as u32).saturating_sub(self.next)
    }

    /// Path tile `i` in `here`'s block frame (outside the region for a tile
    /// in its halo).
    pub fn waypoint(&self, i: u32, here: RegionCoords) -> Option<TilePos> {
        self.path.get(i as usize).map(|&p| local_tile(p, here))
    }
}

/// World tile `p` in `here`'s block frame. Walk tiles lie within a region
/// and its halo, so the offset fits an `i32`.
pub fn local_tile(p: WorldBlockPos, here: RegionCoords) -> TilePos {
    let n = REGION_BLOCKS as i64;
    [(p.x - i64::from(here.x) * n) as i32, p.y, (p.z - i64::from(here.z) * n) as i32]
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
/// else the nearest standable one in its column. The client's path preview
/// starts from the same tile.
pub fn current_tile(src: &impl TileSource, feet: Vector) -> Option<TilePos> {
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

/// `t` with its level clamped to its column's scan range
/// ([`crate::column_scan_range`]); `None` when its column lies outside the
/// region (untrusted input).
fn clamp_tile(src: &impl TileSource, t: TilePos) -> Option<TilePos> {
    let n = REGION_BLOCKS as i32;
    ((0..n).contains(&t[0]) && (0..n).contains(&t[2])).then(|| {
        let (lo, hi) = crate::column_scan_range(src, [t[0], t[2]]);
        [t[0], t[1].clamp(lo, hi), t[2]]
    })
}

/// One tick of walking intent for a body with feet at `feet` (voxel units,
/// local to `here`): forward, yawed toward the next waypoint's column
/// centre — which may lie past the region's edge, in its halo; jump held when the
/// waypoint is more than [`STEP_HEIGHT`] above the feet, the body is
/// grounded and the step is not a walk-up a slope ([`Walker::walk_up`]) — unless the walk
/// has stalled [`WALK_UP_JUMP_AFTER_STALL_TICKS`], then it jumps anyway. Never sprints. The yaw follows `movement::plan`'s convention:
/// forward is local −Z, so a heading `(dx, dz)` is `atan2(−dx, −dz)`.
///
/// Jump is held, never pressed: `plan` toggles flight on a double tap, and
/// a body re-jumping on landing could tap twice inside the window.
pub fn steer(walker: &Walker, feet: Vector, grounded: bool, here: RegionCoords) -> (MoveKeys, f32) {
    let bv = BLOCK_VOXELS as f32;
    let Some(wp) = walker.waypoint(walker.next, here) else { return (MoveKeys::default(), 0.0) };
    let (cx, cz) = ((wp[0] as f32 + 0.5) * bv, (wp[2] as f32 + 0.5) * bv);
    let (dx, dz) = (cx - feet.x, cz - feet.z);
    let yaw = libm::atan2f(-dx, -dz);
    let walk_up = walker.walk_up.get(walker.next as usize).copied().unwrap_or(false);
    let walk_up = walk_up && walker.stalled < WALK_UP_JUMP_AFTER_STALL_TICKS;
    let climb = grounded && !walk_up && wp[1] as f32 * bv - feet.y > STEP_HEIGHT * UNITS_PER_METER;
    let keys = MoveKeys { forward: true, jump_held: climb, ..MoveKeys::default() };
    (keys, yaw)
}
