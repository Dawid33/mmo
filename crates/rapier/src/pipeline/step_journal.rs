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
    // pub(crate) islands: Option<crate::dynamics::IslandsSaved>, // Task 3
    pub(crate) lists: Option<ListsSaved>,
    pub(crate) joints: Option<Box<(ImpulseJointSet, MultibodyJointSet)>>,
    // pub(crate) narrow: Vec<crate::geometry::NarrowUndo>, // Task 5
    pub(crate) narrow_wholesale: Option<Box<NarrowPhase>>,
    // pub(crate) broad: Option<Box<crate::geometry::BroadSaved>>, // Task 4
}

// Task 3 constructs and reads these fields; until then nothing builds a
// `ListsSaved`, so silence the (correct) dead-code warning.
#[allow(dead_code)]
pub(crate) struct ListsSaved {
    pub modified_bodies: crate::dynamics::ModifiedRigidBodies,
    pub modified_colliders: crate::geometry::ModifiedColliders,
    pub removed_colliders: Vec<ColliderHandle>,
    pub impulse_to_wake_up: HashSet<RigidBodyHandle>,
    pub multibody_to_wake_up: HashSet<RigidBodyHandle>,
}

impl StepJournal {
    /// True if nothing has been captured this tick, i.e. `revert` would be a no-op.
    pub fn is_empty(&self) -> bool {
        self.saved_bodies.is_empty()
            && self.saved_colliders.is_empty()
            && self.narrow_wholesale.is_none()
            // && self.broad.is_none() // Task 4
        // islands are captured unconditionally but are O(active)/O(modified);
        // is_empty() reports "nothing moved" for the size assertions, so it
        // will additionally need to require the islands capture to be empty
        // once Task 3 adds the field:
        // self.islands.as_ref().map_or(true, |i| i.active_set.is_empty())
            && true // Task 3
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
        // for (h, old) in self.saved_bodies.into_iter().rev() {
        //     bodies.restore_raw(h, old); // Task 3
        // }
        // for (h, old) in self.saved_colliders.into_iter().rev() {
        //     colliders.restore_raw(h, old); // Task 3
        // }
        // Later tasks: islands, lists, joints, narrow (LIFO ops or wholesale),
        // broad (wholesale).
        let _ = (
            islands,
            broad_phase,
            narrow_phase,
            bodies,
            colliders,
            impulse_joints,
            multibody_joints,
        );
        let _ = (self.lists, self.joints, self.narrow_wholesale); // Task 3/5
    }
}
