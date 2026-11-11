//! The generic dynamic body, [`EntityKind::Body`]: a rigid body with no
//! behaviour of its own that falls, tumbles, comes to rest and crosses region
//! seams with its full [`BodyState`] (velocities, sleep state, explicit mass,
//! its weight). Stud bodies (`stud_body.rs`) are bodies of this kind with
//! a compound collider and a `StudBody` component. `Order::DevDropBody`
//! drops a cube from the dev panel ([`Rollback::dev_drop_body_safe`]).
//!
//! **Mass.** Explicit, from [`crate::mass_props::mass_properties`] of its
//! collider at [`BODY_DEFAULT_DENSITY`]; its collider is massless (density
//! 0, `Rollback::build_body_safe`), so rapier never computes a mass of its
//! own (no glamx eigen solve, which is not bit-identical across targets).
//!
//! **Gravity.** The physics world's gravity is zero (players and slimes
//! integrate their own), so a body carries its weight as a constant user
//! force of `mass × BODY_GRAVITY` downward — as stud bodies do — which
//! [`BodyState`] carries across a seam. `gravity_scale` is left at 0, so a
//! world gravity turned on later would not count twice.

use rapier3d::math::{Pose, Vector};

use crate::mass_props::{mass_properties, BODY_DEFAULT_DENSITY};
use crate::movement::UNITS_PER_METER;
use crate::protocol::{BodyKind, BodyState, ColliderSpec};
use crate::{EntityKey, EntityKind, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Rollback, TICK_RATE};

/// How long a body dropped from a few metres takes to land and fall asleep,
/// ms (rapier sleeps a body still for 2 s).
pub const BODY_SETTLE_MS: u64 = 3_000;
/// [`BODY_SETTLE_MS`] in ticks (`TICK_RATE` is the tick period in ms).
pub const BODY_SETTLE_TICKS: usize = (BODY_SETTLE_MS / TICK_RATE) as usize;
/// Downward acceleration of a body, m/s²: the world's gravity (what players,
/// slimes and stud bodies fall with).
pub const BODY_GRAVITY: f32 = crate::movement::GRAVITY;
/// Angular damping of a body (1/s): a little, so a body rolling on the
/// ground settles instead of rocking.
pub const BODY_ANGULAR_DAMPING: f32 = 0.5;
/// Soft-CCD prediction distance of a body (units): one block, so a landing
/// at a block per tick stops at the surface instead of sinking in first
/// (see `stud_body::STUD_BODY_SOFT_CCD`).
pub const BODY_SOFT_CCD: f32 = crate::BLOCK_UNITS as f32;

/// Tile levels above the ordered tile's bottom at which a dev-dropped body's
/// bottom starts (`Order::DevDropBody`).
pub const DEV_DROP_HEIGHT_TILES: i32 = 4;
/// Most spin about each axis (rad/s) a dev-dropped body starts with, rolled
/// per axis from the tick and the tile (`slime::roll`, so a replay spins the
/// same way).
pub const DEV_DROP_SPIN_RAD_PER_S: f32 = 1.0;
/// `roll` salt of a dev-dropped body's spin.
pub const DEV_DROP_SALT: u64 = 0xD20B_B0D1;

/// The state a new body of `collider` starts in at `pose`.
pub fn body_state(pose: Pose, collider: &ColliderSpec, linvel: Vector, angvel: Vector) -> BodyState {
    let mass = mass_properties(collider, BODY_DEFAULT_DENSITY);
    BodyState {
        kind: BodyKind::Dynamic,
        linvel,
        angvel,
        activation: rapier3d::dynamics::RigidBodyActivation::active(),
        linear_damping: 0.0,
        angular_damping: BODY_ANGULAR_DAMPING,
        gravity_scale: 0.0,
        ccd: false,
        soft_ccd: BODY_SOFT_CCD,
        user_force: Vector::new(0.0, -mass.mass() * BODY_GRAVITY * UNITS_PER_METER, 0.0),
        mass: Some(mass),
        ..BodyState::kinematic(pose)
    }
}

impl Rollback {
    /// Spawns a [`EntityKind::Body`] of `collider` at region-local `pose`,
    /// moving at `linvel` / `angvel`, undo-safely (as `spawn_slime_safe`).
    pub fn spawn_body_safe(&mut self, pose: Pose, collider: ColliderSpec, linvel: Vector, angvel: Vector) -> EntityKey {
        let e = self.data.ecs.create_entity_safe();
        self.assign_entity_id_safe(e);
        self.build_body_safe(e, &body_state(pose, &collider, linvel, angvel), &collider);
        self.set_body_kind_safe(e);
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityPosition(e, pose),
        ));
        e
    }

    /// Makes `e` an [`EntityKind::Body`] (renderer told both ways).
    pub(crate) fn set_body_kind_safe(&mut self, e: EntityKey) {
        let kind = EntityKind::Body;
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(e, Some(kind)),
        ));
        self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetEntityKind(e, None),
        ));
        self.data.ecs.kind.set_safe(e, Some(kind));
    }

    /// `Order::DevDropBody`: a 1 m cube over the centre of region-local tile
    /// `at`, its bottom [`DEV_DROP_HEIGHT_TILES`] tile levels above the tile's
    /// (a tile address like every other dev order's, through `tiles`), spinning at
    /// up to [`DEV_DROP_SPIN_RAD_PER_S`] about each axis. Refused (`false`,
    /// nothing changed) off the region's block columns.
    pub fn dev_drop_body_safe(&mut self, at: crate::TilePos) -> bool {
        let n = crate::REGION_BLOCKS as i32;
        if !(0..n).contains(&at[0]) || !(0..n).contains(&at[2]) {
            return false;
        }
        let b = crate::BLOCK_UNITS as f32;
        let (x, z) = crate::tiles::column_centre([at[0], at[2]]);
        let centre = Vector::new(x, crate::tiles::level_bottom_units(at[1] + DEV_DROP_HEIGHT_TILES) + b / 2.0, z);
        let tick = *self.data.tick;
        // Per axis a roll in [-1, 1] (a 2001-step lattice) of the tile.
        let subject = |axis: u64| ((at[0] as u32 as u64) << 32 | at[2] as u32 as u64) ^ (axis << 62);
        let spin = |axis: u64| {
            let r = crate::slime::roll(tick, subject(axis), DEV_DROP_SALT) % 2001;
            (r as f32 - 1000.0) / 1000.0 * DEV_DROP_SPIN_RAD_PER_S
        };
        let cube = ColliderSpec::Cuboid { half_extents: Vector::splat(b / 2.0) };
        self.spawn_body_safe(Pose::from_translation(centre), cube, Vector::ZERO, Vector::new(spin(0), spin(1), spin(2)));
        true
    }

    /// Removes only the body + colliders of `e` (undo-tracked), never the
    /// entity: no RemoveEntity/CreateEntity is emitted, so the bridge keeps
    /// the same render entity. Rides a whole-PhysicsState snapshot like
    /// `remove_entity_safe` (body removal touches colliders, islands and
    /// joints); it is as rare (a mirror upgraded into a body of another
    /// kind).
    pub(crate) fn remove_body_safe(&mut self, e: EntityKey) {
        let Some(handle) = *self.data.ecs.rigidbody.try_get(e) else { return };
        let p = self.data.physics.snapshot_raw();
        p.bodies.remove(handle, p.islands, p.colliders, p.implules_joint_set, p.multi_body_joint_set, p.soft_bodies, true);
        self.data.ecs.rigidbody.set_safe(e, None);
    }

    /// Puts `e`'s existing body in `state` (a halo mirror upgraded in
    /// place, or a replayed arrival), keeping the entity. A body of the same
    /// kind is re-stated in place; one of another kind (a kinematic mirror
    /// becoming a dynamic body) is replaced — [`Self::remove_body_safe`] +
    /// `build_body_safe` with `collider` — so it is built exactly like a
    /// fresh arrival and `set_body_type` never runs on a live body.
    pub(crate) fn restate_body_safe(&mut self, e: EntityKey, state: &BodyState, collider: &ColliderSpec) {
        let Some(handle) = *self.data.ecs.rigidbody.try_get(e) else {
            return self.build_body_safe(e, state, collider);
        };
        let same = self.data.physics.bodies.get(handle).is_some_and(|b| state.same_kind_as(b));
        if same {
            self.data.physics.bodies.mutate_body_safe(handle, |b| state.apply_to(b));
        } else {
            self.remove_body_safe(e);
            self.build_body_safe(e, state, collider);
        }
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityPosition(e, state.pose),
        ));
    }
}
