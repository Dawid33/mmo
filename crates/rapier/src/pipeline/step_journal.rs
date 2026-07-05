//! Per-tick mutation journal: save-before-first-write capture of everything
//! `PhysicsPipeline::step` mutates, with an exact LIFO revert. Pure capture —
//! never influences simulation. See the game repo spec
//! `docs/superpowers/specs/2026-07-04-physics-step-journal-design.md`.
use crate::dynamics::{
    ImpulseJointSet, IslandManager, MultibodyJointSet, RigidBody, RigidBodyHandle, RigidBodySet,
};
use crate::geometry::{BroadPhaseBvh, Collider, ColliderHandle, ColliderSet, NarrowPhase};
use parry3d::utils::hashset::HashSet;

/// Per-tick mutation journal for [`crate::pipeline::PhysicsPipeline::step_journaled`].
///
/// Captures the pre-mutation state of everything a step touches so the tick
/// can be reverted without a whole-state snapshot. Building this out (actual
/// capture calls, islands/lists/joints/narrow/broad handling) is the job of
/// later tasks; this skeleton only defines the container and the plumbing to
/// thread it through the pipeline. With no capture calls wired up yet, a
/// journal always reverts to a no-op.
#[derive(Default)]
pub struct StepJournal {
    pub(crate) saved_bodies: Vec<(RigidBodyHandle, RigidBody)>,
    pub(crate) saved_body_set: HashSet<RigidBodyHandle>,
    pub(crate) saved_colliders: Vec<(ColliderHandle, Collider)>,
    pub(crate) saved_collider_set: HashSet<ColliderHandle>,
    pub(crate) islands: Option<crate::dynamics::IslandsSaved>,
    pub(crate) lists: Option<ListsSaved>,
    pub(crate) joints: Option<Box<(ImpulseJointSet, MultibodyJointSet)>>,
    /// Fine-grained narrow-phase mutation log (payload saves + exact graph/coarena
    /// cell inverses), recorded in execution order and reverted LIFO. Empty when
    /// [`Self::narrow_wholesale`] is set — collider-removal ticks fall back to a
    /// wholesale snapshot instead (see [`NarrowPhase::handle_user_changes`]).
    pub(crate) narrow: Vec<crate::geometry::NarrowUndo>,
    pub(crate) narrow_wholesale: Option<Box<NarrowPhase>>,
    /// Pre-mutation broad-phase snapshot, captured once on the first dirty tick
    /// (see [`BroadPhaseBvh::journal_save`]). `None` on a clean tick where the
    /// broad phase was never touched (query via [`Self::broad_captured`]).
    pub(crate) broad: Option<Box<crate::geometry::BroadSaved>>,
}

pub(crate) struct ListsSaved {
    pub modified_bodies: crate::dynamics::ModifiedRigidBodies,
    pub modified_colliders: crate::geometry::ModifiedColliders,
    pub removed_colliders: Vec<ColliderHandle>,
    pub impulse_to_wake_up: HashSet<RigidBodyHandle>,
    pub multibody_to_wake_up: HashSet<RigidBodyHandle>,
}

impl StepJournal {
    /// True if this tick mutated the broad phase (a pre-mutation BVH snapshot was
    /// captured). False on a clean tick where the broad phase was never touched.
    pub fn broad_captured(&self) -> bool {
        self.broad.is_some()
    }

    /// True if nothing has been captured this tick, i.e. `revert` would be a no-op.
    pub fn is_empty(&self) -> bool {
        self.saved_bodies.is_empty()
            && self.saved_colliders.is_empty()
            && self.narrow.is_empty()
            && self.narrow_wholesale.is_none()
            && self.broad.is_none()
            // islands are captured unconditionally but are O(active)/O(modified);
            // is_empty() reports "nothing moved" for the size assertions, so an
            // empty capture is one with no active bodies at step start.
            && self
                .islands
                .as_ref()
                .map_or(true, |i| i.active_set.is_empty())
    }

    /// Saves the pre-mutation state of `body` the first time it's touched this tick.
    /// Subsequent saves for the same handle within the same tick are no-ops, so the
    /// oldest (pre-tick) value always wins.
    pub fn save_body(&mut self, handle: RigidBodyHandle, body: &RigidBody) {
        if self.saved_body_set.insert(handle) {
            self.saved_bodies.push((handle, body.clone()));
        }
    }

    /// Saves the pre-mutation state of `collider` the first time it's touched this tick.
    /// Subsequent saves for the same handle within the same tick are no-ops, so the
    /// oldest (pre-tick) value always wins.
    pub fn save_collider(&mut self, handle: ColliderHandle, collider: &Collider) {
        if self.saved_collider_set.insert(handle) {
            self.saved_colliders.push((handle, collider.clone()));
        }
    }

    /// Undoes everything this journal captured, restoring the given structures to
    /// their pre-tick state. Consumes the journal: a reverted tick's journal cannot
    /// be reverted twice.
    pub fn revert(
        self,
        islands: &mut IslandManager,
        broad_phase: &mut BroadPhaseBvh,
        narrow_phase: &mut NarrowPhase,
        bodies: &mut RigidBodySet,
        colliders: &mut ColliderSet,
        impulse_joints: &mut ImpulseJointSet,
        multibody_joints: &mut MultibodyJointSet,
    ) {
        // Sections are disjoint state; within each, restoration is LIFO.
        // Bodies/colliders: value restore (old clone wins over any number of
        // intra-tick writes).
        for (h, old) in self.saved_bodies.into_iter().rev() {
            bodies.restore_raw(h, old);
        }
        for (h, old) in self.saved_colliders.into_iter().rev() {
            colliders.restore_raw(h, old);
        }

        // Islands: whole hashed-field snapshot restore.
        if let Some(s) = self.islands {
            islands.journal_restore(s);
        }

        // Modified/removed lists + the joint-set to-wake-up queues: wholesale
        // restore of the pre-tick values.
        if let Some(l) = self.lists {
            bodies.set_modified(l.modified_bodies);
            colliders.set_modified(l.modified_colliders);
            colliders.set_removed(l.removed_colliders);
            impulse_joints.to_wake_up = l.impulse_to_wake_up;
            multibody_joints.to_wake_up = l.multibody_to_wake_up;
        }

        // Joints: coarse wholesale fallback (only captured when non-empty at
        // step start). Applied after `lists` so the full pre-tick joint sets win.
        if let Some(j) = self.joints {
            let (ij, mj) = *j;
            *impulse_joints = ij;
            *multibody_joints = mj;
        }

        // Narrow phase: the wholesale snapshot (collider-removal ticks) takes
        // precedence over op replay; otherwise LIFO-revert the fine-grained log.
        // Ordered before the broad restore per the design spec.
        if let Some(w) = self.narrow_wholesale {
            *narrow_phase = *w;
        } else {
            narrow_phase.journal_revert(self.narrow);
        }

        // Broad phase: wholesale restore of the pre-tick snapshot (only present
        // on a tick that actually mutated the BVH).
        if let Some(b) = self.broad {
            broad_phase.journal_restore(*b);
        }
    }
}
