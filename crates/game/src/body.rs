//! The generic dynamic body, [`EntityKind::Body`]: a rigid body with no
//! behaviour of its own that falls, tumbles, comes to rest and crosses region
//! seams with its full [`BodyState`] (velocities, sleep state, explicit mass,
//! its weight). Voxel bodies (`voxel_body.rs`) are meant to build on it.
//!
//! **Mass.** Explicit, from [`crate::mass_props::mass_properties`] of its
//! collider at [`BODY_DEFAULT_DENSITY`]; its collider is massless (density
//! 0, `Rollback::build_body_safe`), so rapier never computes a mass of its
//! own (no glamx eigen solve, which is not bit-identical across targets).
//!
//! **Gravity.** The physics world's gravity is zero (players and slimes
//! integrate their own), so a body carries its weight as a constant user
//! force of `mass × BODY_GRAVITY` downward — as voxel bodies do — which
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
/// slimes and voxel bodies fall with).
pub const BODY_GRAVITY: f32 = crate::movement::GRAVITY;
/// Angular damping of a body (1/s): a little, so a body rolling on the
/// ground settles instead of rocking.
pub const BODY_ANGULAR_DAMPING: f32 = 0.5;
/// Soft-CCD prediction distance of a body (units): one block, so a landing
/// at a block per tick stops at the surface instead of sinking in first
/// (see `voxel_body::VOXEL_BODY_SOFT_CCD`).
pub const BODY_SOFT_CCD: f32 = crate::BLOCK_VOXELS as f32;

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
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityPosition(e, pose),
        ));
        e
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
