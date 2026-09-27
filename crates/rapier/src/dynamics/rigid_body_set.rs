use crate::data::{Arena, HasModifiedFlag, ModifiedObjects};
use crate::dynamics::{
    ImpulseJointSet, IslandManager, MultibodyJointSet, RigidBody, RigidBodyBuilder,
    RigidBodyChanges, RigidBodyHandle, SoftBodySet,
};
use crate::geometry::ColliderSet;
use core::ops::{Index, IndexMut};

#[cfg(doc)]
use crate::pipeline::PhysicsPipeline;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
/// A pair of rigid body handles.
pub struct BodyPair {
    /// The first rigid body handle.
    pub body1: RigidBodyHandle,
    /// The second rigid body handle.
    pub body2: RigidBodyHandle,
}

impl BodyPair {
    /// Builds a new pair of rigid-body handles.
    pub fn new(body1: RigidBodyHandle, body2: RigidBodyHandle) -> Self {
        BodyPair { body1, body2 }
    }
}

pub(crate) type ModifiedRigidBodies = ModifiedObjects<RigidBodyHandle, RigidBody>;

impl HasModifiedFlag for RigidBody {
    #[inline]
    fn has_modified_flag(&self) -> bool {
        self.changes.contains(RigidBodyChanges::IN_MODIFIED_SET)
    }

    #[inline]
    fn set_modified_flag(&mut self) {
        self.changes |= RigidBodyChanges::IN_MODIFIED_SET;
    }
}

#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
#[derive(Clone, Default, Debug)]
/// The collection that stores all rigid bodies in your physics world.
///
/// This is where you add, remove, and access all your physics objects. Think of it as
/// a "database" of all rigid bodies, where each body gets a unique handle for fast lookup.
///
/// # Why use handles?
///
/// Instead of storing bodies directly, you get back a [`RigidBodyHandle`] when inserting.
/// This handle is lightweight (just an index + generation) and remains valid even if other
/// bodies are removed, protecting you from use-after-free bugs.
///
/// # Example
///
/// ```
/// # use rapier3d::prelude::*;
/// let mut bodies = RigidBodySet::new();
///
/// // Add a dynamic body
/// let handle = bodies.insert(RigidBodyBuilder::dynamic());
///
/// // Access it later
/// if let Some(body) = bodies.get_mut(handle) {
///     body.apply_impulse(Vector::new(0.0, 10.0, 0.0), true);
/// }
/// ```
#[derive(state_hash::StateHash)] // mmo-fork: StateHash
pub struct RigidBodySet {
    // NOTE: the pub(crate) are needed by the broad phase
    // to avoid borrowing issues. It is also needed for
    // parallelism because the `Receiver` breaks the Sync impl.
    // Could we avoid this?
    pub(crate) bodies: Arena<RigidBody>,
    pub(crate) modified_bodies: ModifiedRigidBodies,
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    #[state_hash(skip)] // mmo-fork: StateHash — a constant placeholder body, not state (serde-skipped).
    pub(crate) default_fixed: RigidBody,
    // mmo-fork: first-touch capture buffer, installed only for the duration of
    // a journaled step (`pipeline::step_journal`, upgrade plan Task 4.2).
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    #[state_hash(skip)] // mmo-fork: StateHash — the journal's transient capture buffer, not state.
    pub(crate) capture:
        Option<alloc::boxed::Box<crate::pipeline::step_journal::Capture<RigidBodyHandle, RigidBody>>>,
}

impl RigidBodySet {
    /// Creates a new empty collection of rigid bodies.
    ///
    /// Call this once when setting up your physics world. The collection will
    /// automatically grow as you add more bodies.
    pub fn new() -> Self {
        RigidBodySet {
            bodies: Arena::new(),
            modified_bodies: ModifiedObjects::default(),
            default_fixed: RigidBodyBuilder::fixed().build(),
            capture: None, // mmo-fork: journal capture
        }
    }

    /// Creates a new collection with pre-allocated space for the given number of bodies.
    ///
    /// Use this if you know approximately how many bodies you'll need, to avoid
    /// multiple reallocations as the collection grows.
    ///
    /// # Example
    /// ```
    /// # use rapier3d::prelude::*;
    /// // You know you'll have ~1000 bodies
    /// let mut bodies = RigidBodySet::with_capacity(1000);
    /// ```
    pub fn with_capacity(capacity: usize) -> Self {
        RigidBodySet {
            bodies: Arena::with_capacity(capacity),
            modified_bodies: ModifiedRigidBodies::with_capacity(capacity),
            default_fixed: RigidBodyBuilder::fixed().build(),
            capture: None, // mmo-fork: journal capture
        }
    }

    pub(crate) fn take_modified(&mut self) -> ModifiedRigidBodies {
        core::mem::take(&mut self.modified_bodies)
    }

    // mmo-fork: journal plumbing (upgrade plan Task 4.2).
    pub(crate) fn set_modified(&mut self, modified: ModifiedRigidBodies) {
        self.modified_bodies = modified;
    }

    /// mmo-fork: saves `handle`'s pre-step value on its first `&mut` access
    /// during a journaled step. Every accessor handing out `&mut RigidBody`
    /// calls this first.
    #[inline]
    fn journal_capture(&mut self, handle: RigidBodyHandle) {
        if let Some(capture) = self.capture.as_deref_mut() {
            if let Some(rb) = self.bodies.get(handle.0) {
                capture.save(handle, handle.0.into_raw_parts().0, rb);
            }
        }
    }

    /// mmo-fork: whether a write to `handle` right now would be covered by the
    /// journal: no journaled step is running, or its value is already saved.
    /// The staged solver's raw-pointer writeback asserts this.
    pub(crate) fn journal_covers(&self, handle: RigidBodyHandle) -> bool {
        self.capture
            .as_deref()
            .is_none_or(|c| c.contains(handle.0.into_raw_parts().0))
    }

    // mmo-fork: rollback primitives (upgrade plan Task 4.1; same names and
    // semantics as the old fork's, so the game's undo closures port 1:1).

    /// Undo-only: overwrite the slot value; the handle must be occupied.
    /// Does NOT touch modified/removed lists (the journal restores those
    /// wholesale).
    pub fn restore_raw(&mut self, handle: RigidBodyHandle, body: RigidBody) {
        *self
            .bodies
            .get_mut(handle.0)
            .expect("restore_raw: slot vacant") = body;
    }

    /// Allocator state to capture before `insert` for [`Self::revert_insert`].
    pub fn alloc_state(&self) -> (Option<u32>, u32) {
        self.bodies.alloc_state()
    }

    /// Exact inverse of the MOST RECENT `insert` that returned `handle`,
    /// given the state captured by [`Self::alloc_state`] just before it.
    /// LIFO contract: invalid if any other body was inserted/removed/marked
    /// modified since.
    pub fn revert_insert(
        &mut self,
        handle: RigidBodyHandle,
        prev_free_head: Option<u32>,
        prev_items_len: u32,
    ) {
        let popped = self.modified_bodies.pop();
        assert_eq!(
            popped,
            Some(handle),
            "revert_insert: not the most recent insert"
        );
        self.bodies
            .revert_insert(handle.0, prev_free_head, prev_items_len);
    }

    /// Whether `handle`'s body currently carries the `IN_MODIFIED_SET` change-flag —
    /// i.e. whether a tracked mutation (`get_mut`,
    /// `get_mut_internal_with_modification_tracking`, an insert) would skip
    /// its modified-list push. Capture this BEFORE an operation that goes
    /// through modification tracking so its undo knows whether to
    /// [`Self::pop_modified`].
    pub fn is_marked_modified(&self, handle: RigidBodyHandle) -> bool {
        self.bodies
            .get(handle.0)
            .is_some_and(|rb| rb.has_modified_flag())
    }

    /// Exact inverse of the modified-list push performed on `handle` by the
    /// MOST RECENT tracked mutation. LIFO contract: `handle` must be the
    /// list's tail (asserted). Callers restore the body VALUE separately via
    /// [`Self::restore_raw`] — that rewrite covers the `IN_MODIFIED_SET` flag itself,
    /// so this only fixes the list.
    pub fn pop_modified(&mut self, handle: RigidBodyHandle) {
        let popped = self.modified_bodies.pop();
        assert_eq!(popped, Some(handle), "pop_modified: not the list tail");
    }

    /// Returns how many rigid bodies are currently in this collection.
    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    /// Returns `true` if there are no rigid bodies in this collection.
    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    /// Checks if the given handle points to a valid rigid body that still exists.
    ///
    /// Returns `false` if the body was removed or the handle is invalid.
    pub fn contains(&self, handle: RigidBodyHandle) -> bool {
        self.bodies.contains(handle.0)
    }

    /// Adds a rigid body to the world and returns its handle for future access.
    ///
    /// The handle is how you'll refer to this body later (to move it, apply forces, etc.).
    /// Keep the handle somewhere accessible - you can't get the body back without it!
    ///
    /// # Example
    /// ```
    /// # use rapier3d::prelude::*;
    /// # let mut bodies = RigidBodySet::new();
    /// let handle = bodies.insert(
    ///     RigidBodyBuilder::dynamic()
    ///         .translation(Vector::new(0.0, 5.0, 0.0))
    ///         .build()
    /// );
    /// // Store `handle` to access this body later
    /// ```
    pub fn insert(&mut self, rb: impl Into<RigidBody>) -> RigidBodyHandle {
        // mmo-fork: the journal restores values, not slots.
        debug_assert!(self.capture.is_none(), "body insert during a journaled step");
        let mut rb = rb.into();
        // Make sure the internal links are reset, they may not be
        // if this rigid-body was obtained by cloning another one.
        rb.reset_internal_references();
        rb.changes.set(RigidBodyChanges::all(), true);

        let handle = RigidBodyHandle(self.bodies.insert(rb));
        // Using push_unchecked because this is a brand new rigid-body with the MODIFIED
        // flags set but isn’t in the modified_bodies yet.
        self.modified_bodies
            .push_unchecked(handle, &mut self.bodies[handle.0]);
        handle
    }

    /// Removes a rigid body from the world along with all its attached colliders and joints.
    ///
    /// This is a complete cleanup operation that removes:
    /// - The rigid body itself
    /// - All colliders attached to it (if `remove_attached_colliders` is `true`)
    /// - All joints connected to this body
    ///
    /// Returns the removed body if it existed, or `None` if the handle was invalid.
    ///
    /// # Parameters
    ///
    /// * `remove_attached_colliders` - If `true`, removes all colliders attached to this body.
    ///   If `false`, the colliders are detached and become independent.
    ///
    /// # Example
    /// ```
    /// # use rapier3d::prelude::*;
    /// # let mut bodies = RigidBodySet::new();
    /// # let mut islands = IslandManager::new();
    /// # let mut colliders = ColliderSet::new();
    /// # let mut impulse_joints = ImpulseJointSet::new();
    /// # let mut multibody_joints = MultibodyJointSet::new();
    /// # let handle = bodies.insert(RigidBodyBuilder::dynamic());
    /// # let mut soft_bodies = SoftBodySet::new();
    /// // Remove a body and everything attached to it
    /// if let Some(body) = bodies.remove(
    ///     handle,
    ///     &mut islands,
    ///     &mut colliders,
    ///     &mut impulse_joints,
    ///     &mut multibody_joints,
    ///     &mut soft_bodies,
    ///     true  // Remove colliders too
    /// ) {
    ///     println!("Removed body at {:?}", body.translation());
    /// }
    /// ```
    #[profiling::function]
    pub fn remove(
        &mut self,
        handle: RigidBodyHandle,
        islands: &mut IslandManager,
        colliders: &mut ColliderSet,
        impulse_joints: &mut ImpulseJointSet,
        multibody_joints: &mut MultibodyJointSet,
        soft_bodies: &mut SoftBodySet,
        remove_attached_colliders: bool,
    ) -> Option<RigidBody> {
        // mmo-fork: the journal restores values, not slots.
        debug_assert!(self.capture.is_none(), "body removal during a journaled step");
        // Removing a soft-frame proxy removes its cluster: the particles only it covers, the
        // elements attached to them, and their colliders. Dissolved before the body removal so the
        // proxy (and its parenting of the cluster's colliders) is still intact meanwhile.
        if let Some(rb) = self.bodies.get(handle.0) {
            if let (Some(sb_handle), Some(cluster)) = (rb.soft_body(), rb.soft_cluster()) {
                soft_bodies.on_proxy_removed(sb_handle, cluster, islands, self, colliders);
            }
        }

        let rb = self.bodies.remove(handle.0)?;
        /*
         * Update active sets.
         */
        islands.rigid_body_removed_or_disabled(handle, &rb.ids, self);

        /*
         * Remove colliders attached to this rigid-body.
         */
        if remove_attached_colliders {
            for collider in rb.colliders() {
                colliders.remove(*collider, islands, self, soft_bodies, false);
            }
        } else {
            // If we don’t remove the attached colliders, simply detach them.
            let colliders_to_detach = rb.colliders().to_vec();
            for co_handle in colliders_to_detach {
                colliders.set_parent(co_handle, None, self);
            }
        }

        /*
         * Remove impulse_joints attached to this rigid-body.
         */
        impulse_joints.remove_joints_attached_to_rigid_body(handle);
        multibody_joints.remove_joints_attached_to_rigid_body(handle);

        Some(rb)
    }

    /// Gets a rigid body by its index without knowing the generation number.
    ///
    /// ⚠️ **Advanced/unsafe usage** - prefer [`get()`](Self::get) instead!
    ///
    /// This bypasses the generation check that normally protects against the ABA problem
    /// (where an index gets reused after removal). Only use this if you're certain the
    /// body at this index is the one you expect.
    ///
    /// Returns both the body and its current handle (with the correct generation).
    pub fn get_unknown_gen(&self, i: u32) -> Option<(&RigidBody, RigidBodyHandle)> {
        self.bodies
            .get_unknown_gen(i)
            .map(|(b, h)| (b, RigidBodyHandle(h)))
    }

    /// Gets a mutable reference to a rigid body by its index without knowing the generation.
    ///
    /// ⚠️ **Advanced/unsafe usage** - prefer [`get_mut()`](Self::get_mut) instead!
    ///
    /// This bypasses the generation check. See [`get_unknown_gen()`](Self::get_unknown_gen)
    /// for more details on when this is appropriate (rarely).
    #[cfg(not(feature = "dev-remove-slow-accessors"))]
    pub fn get_unknown_gen_mut(&mut self, i: u32) -> Option<(&mut RigidBody, RigidBodyHandle)> {
        let (_, handle) = self.bodies.get_unknown_gen(i)?;
        self.journal_capture(RigidBodyHandle(handle)); // mmo-fork
        let (rb, handle) = self.bodies.get_unknown_gen_mut(i)?;
        let handle = RigidBodyHandle(handle);
        self.modified_bodies.push_once(handle, rb);
        Some((rb, handle))
    }

    /// Gets a read-only reference to the rigid body with the given handle.
    ///
    /// Returns `None` if the handle is invalid or the body was removed.
    ///
    /// Use this to read body properties like position, velocity, mass, etc.
    pub fn get(&self, handle: RigidBodyHandle) -> Option<&RigidBody> {
        self.bodies.get(handle.0)
    }

    /// Gets a mutable reference to the rigid body with the given handle.
    ///
    /// Returns `None` if the handle is invalid or the body was removed.
    ///
    /// Use this to modify body properties, apply forces/impulses, change velocities, etc.
    ///
    /// # Example
    /// ```
    /// # use rapier3d::prelude::*;
    /// # let mut bodies = RigidBodySet::new();
    /// # let handle = bodies.insert(RigidBodyBuilder::dynamic());
    /// if let Some(body) = bodies.get_mut(handle) {
    ///     body.set_linvel(Vector::new(1.0, 0.0, 0.0), true);
    ///     body.apply_impulse(Vector::new(0.0, 100.0, 0.0), true);
    /// }
    /// ```
    #[cfg(not(feature = "dev-remove-slow-accessors"))]
    pub fn get_mut(&mut self, handle: RigidBodyHandle) -> Option<&mut RigidBody> {
        self.journal_capture(handle); // mmo-fork
        let result = self.bodies.get_mut(handle.0)?;
        self.modified_bodies.push_once(handle, result);
        Some(result)
    }

    /// Gets mutable references to two different rigid bodies at once.
    ///
    /// This is useful when you need to modify two bodies simultaneously (e.g., when manually
    /// handling collisions between them). If both handles are the same, only the first value
    /// will be `Some`.
    ///
    /// # Example
    /// ```
    /// # use rapier3d::prelude::*;
    /// # let mut bodies = RigidBodySet::new();
    /// # let handle1 = bodies.insert(RigidBodyBuilder::dynamic());
    /// # let handle2 = bodies.insert(RigidBodyBuilder::dynamic());
    /// let (body1, body2) = bodies.get_pair_mut(handle1, handle2);
    /// if let (Some(b1), Some(b2)) = (body1, body2) {
    ///     // Can modify both bodies at once
    ///     b1.apply_impulse(Vector::new(10.0, 0.0, 0.0), true);
    ///     b2.apply_impulse(Vector::new(-10.0, 0.0, 0.0), true);
    /// }
    /// ```
    #[cfg(not(feature = "dev-remove-slow-accessors"))]
    pub fn get_pair_mut(
        &mut self,
        handle1: RigidBodyHandle,
        handle2: RigidBodyHandle,
    ) -> (Option<&mut RigidBody>, Option<&mut RigidBody>) {
        if handle1 == handle2 {
            (self.get_mut(handle1), None)
        } else {
            // mmo-fork
            self.journal_capture(handle1);
            self.journal_capture(handle2);
            let (mut rb1, mut rb2) = self.bodies.get2_mut(handle1.0, handle2.0);
            if let Some(rb1) = rb1.as_deref_mut() {
                self.modified_bodies.push_once(handle1, rb1);
            }
            if let Some(rb2) = rb2.as_deref_mut() {
                self.modified_bodies.push_once(handle2, rb2);
            }
            (rb1, rb2)
        }
    }

    pub(crate) fn get_mut_internal(&mut self, handle: RigidBodyHandle) -> Option<&mut RigidBody> {
        self.journal_capture(handle); // mmo-fork
        self.bodies.get_mut(handle.0)
    }

    pub(crate) fn index_mut_internal(&mut self, handle: RigidBodyHandle) -> &mut RigidBody {
        self.journal_capture(handle); // mmo-fork
        &mut self.bodies[handle.0]
    }

    // Just a very long name instead of `.get_mut` to make sure
    // this is really the method we wanted to use instead of `get_mut_internal`.
    pub(crate) fn get_mut_internal_with_modification_tracking(
        &mut self,
        handle: RigidBodyHandle,
    ) -> Option<&mut RigidBody> {
        self.journal_capture(handle); // mmo-fork
        let result = self.bodies.get_mut(handle.0)?;
        self.modified_bodies.push_once(handle, result);
        Some(result)
    }

    /// Iterates over all rigid bodies in this collection.
    ///
    /// Each iteration yields a `(handle, &RigidBody)` pair. Use this to read properties
    /// of all bodies (positions, velocities, etc.) without modifying them.
    ///
    /// # Example
    /// ```
    /// # use rapier3d::prelude::*;
    /// # let mut bodies = RigidBodySet::new();
    /// # bodies.insert(RigidBodyBuilder::dynamic());
    /// for (handle, body) in bodies.iter() {
    ///     println!("Body {:?} is at {:?}", handle, body.translation());
    /// }
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (RigidBodyHandle, &RigidBody)> {
        self.bodies.iter().map(|(h, b)| (RigidBodyHandle(h), b))
    }

    /// Iterates over all rigid bodies with mutable access.
    ///
    /// Each iteration yields a `(handle, &mut RigidBody)` pair. Use this to modify
    /// multiple bodies in one pass.
    ///
    /// # Example
    /// ```
    /// # use rapier3d::prelude::*;
    /// # let mut bodies = RigidBodySet::new();
    /// # bodies.insert(RigidBodyBuilder::dynamic());
    /// // Apply gravity manually to all dynamic bodies
    /// for (handle, body) in bodies.iter_mut() {
    ///     if body.is_dynamic() {
    ///         body.add_force(Vector::new(0.0, -9.81 * body.mass(), 0.0), true);
    ///     }
    /// }
    /// ```
    #[cfg(not(feature = "dev-remove-slow-accessors"))]
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (RigidBodyHandle, &mut RigidBody)> {
        // mmo-fork: every body is handed out.
        if let Some(capture) = self.capture.as_deref_mut() {
            for (h, rb) in self.bodies.iter() {
                capture.save(RigidBodyHandle(h), h.into_raw_parts().0, rb);
            }
        }
        self.modified_bodies.clear();
        let modified_bodies = &mut self.modified_bodies;
        self.bodies.iter_mut().map(move |(h, b)| {
            // NOTE: using `push_unchecked` because we just cleared `modified_bodies`
            //       before iterating.
            modified_bodies.push_unchecked(RigidBodyHandle(h), b);
            (RigidBodyHandle(h), b)
        })
    }

    /// Updates the positions of all colliders attached to bodies that have moved.
    ///
    /// Normally you don't need to call this - it's automatically handled by [`PhysicsPipeline::step`].
    /// Only call this manually if you're:
    /// - Moving bodies yourself outside of `step()`
    /// - Using `QueryPipeline` for raycasts without running physics simulation
    /// - Need collider positions to be immediately up-to-date for some custom logic
    ///
    /// This synchronizes collider world positions based on their parent bodies' positions.
    pub fn propagate_modified_body_positions_to_colliders(&self, colliders: &mut ColliderSet) {
        for body in self.modified_bodies.iter().filter_map(|h| self.get(*h)) {
            if body.changes.contains(RigidBodyChanges::POSITION) {
                for handle in body.colliders() {
                    if let Some(collider) = colliders.get_mut(*handle) {
                        let new_pos = body.position() * collider.position_wrt_parent().unwrap();
                        collider.set_position(new_pos);
                    }
                }
            }
        }
    }
}

impl Index<RigidBodyHandle> for RigidBodySet {
    type Output = RigidBody;

    fn index(&self, index: RigidBodyHandle) -> &RigidBody {
        &self.bodies[index.0]
    }
}

impl Index<crate::data::Index> for RigidBodySet {
    type Output = RigidBody;

    fn index(&self, index: crate::data::Index) -> &RigidBody {
        &self.bodies[index]
    }
}

#[cfg(not(feature = "dev-remove-slow-accessors"))]
impl IndexMut<RigidBodyHandle> for RigidBodySet {
    fn index_mut(&mut self, handle: RigidBodyHandle) -> &mut RigidBody {
        self.journal_capture(handle); // mmo-fork
        let rb = &mut self.bodies[handle.0];
        self.modified_bodies.push_once(handle, rb);
        rb
    }
}
