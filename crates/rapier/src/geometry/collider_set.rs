use crate::data::arena::Arena;
use crate::data::{HasModifiedFlag, ModifiedObjects};
use crate::dynamics::{IslandManager, RigidBodyHandle, RigidBodySet, RigidBodyType};
use crate::geometry::{Collider, ColliderChanges, ColliderHandle, ColliderParent};
use crate::math::Isometry;
use std::ops::{Index, IndexMut};

/// A set of modified colliders
pub type ModifiedColliders = ModifiedObjects<ColliderHandle, Collider>;

impl HasModifiedFlag for Collider {
    #[inline]
    fn has_modified_flag(&self) -> bool {
        self.changes.contains(ColliderChanges::MODIFIED)
    }

    #[inline]
    fn set_modified_flag(&mut self) {
        self.changes |= ColliderChanges::MODIFIED;
    }
}

#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
#[derive(Clone, Default, Debug, Hash)]
/// The collection that stores all colliders (collision shapes) in your physics world.
///
/// Similar to [`RigidBodySet`](crate::dynamics::RigidBodySet), this is the "database" where
/// all your collision shapes live. Each collider can be attached to a rigid body or exist
/// independently.
///
/// # Example
/// ```
/// # use rapier3d::prelude::*;
/// let mut colliders = ColliderSet::new();
/// # let mut bodies = RigidBodySet::new();
/// # let body_handle = bodies.insert(RigidBodyBuilder::dynamic());
///
/// // Add a standalone collider (no parent body)
/// let handle = colliders.insert(ColliderBuilder::ball(0.5));
///
/// // Or attach it to a body
/// let handle = colliders.insert_with_parent(
///     ColliderBuilder::cuboid(1.0, 1.0, 1.0),
///     body_handle,
///     &mut bodies
/// );
/// ```
pub struct ColliderSet {
    pub(crate) colliders: Arena<Collider>,
    pub(crate) modified_colliders: ModifiedColliders,
    pub(crate) removed_colliders: Vec<ColliderHandle>,
}

impl ColliderSet {
    /// Creates a new empty collection of colliders.
    pub fn new() -> Self {
        ColliderSet {
            colliders: Arena::new(),
            modified_colliders: Default::default(),
            removed_colliders: Vec::new(),
        }
    }

    /// Creates a new collection with pre-allocated space for the given number of colliders.
    ///
    /// Use this if you know approximately how many colliders you'll need.
    pub fn with_capacity(capacity: usize) -> Self {
        ColliderSet {
            colliders: Arena::with_capacity(capacity),
            modified_colliders: ModifiedColliders::with_capacity(capacity),
            removed_colliders: Vec::new(),
        }
    }

    /// Fetch the set of colliders modified since the last call to
    /// `take_modified`
    ///
    /// Provides a value that can be passed to the `modified_colliders` argument
    /// of [`BroadPhaseBvh::update`](crate::geometry::BroadPhaseBvh::update).
    ///
    /// Should not be used if this [`ColliderSet`] will be used with a
    /// [`PhysicsPipeline`](crate::pipeline::PhysicsPipeline), which handles
    /// broadphase updates automatically.
    pub fn take_modified(&mut self) -> ModifiedColliders {
        std::mem::take(&mut self.modified_colliders)
    }

    pub(crate) fn set_modified(&mut self, modified: ModifiedColliders) {
        self.modified_colliders = modified;
    }

    /// Fetch the set of colliders removed since the last call to `take_removed`
    ///
    /// Provides a value that can be passed to the `removed_colliders` argument
    /// of [`BroadPhaseBvh::update`](crate::geometry::BroadPhaseBvh::update).
    ///
    /// Should not be used if this [`ColliderSet`] will be used with a
    /// [`PhysicsPipeline`](crate::pipeline::PhysicsPipeline), which handles
    /// broadphase updates automatically.
    pub fn take_removed(&mut self) -> Vec<ColliderHandle> {
        std::mem::take(&mut self.removed_colliders)
    }

    pub(crate) fn set_removed(&mut self, removed: Vec<ColliderHandle>) {
        self.removed_colliders = removed;
    }

    /// Undo-only: overwrite the slot value; the handle must be occupied.
    /// Does NOT touch modified/removed lists (the journal restores those
    /// wholesale).
    pub fn restore_raw(&mut self, handle: ColliderHandle, collider: Collider) {
        *self
            .colliders
            .get_mut(handle.0)
            .expect("restore_raw: slot vacant") = collider;
    }

    /// Allocator state to capture before `insert`/`insert_with_parent` for
    /// [`Self::revert_insert`]. Mirrors [`RigidBodySet::alloc_state`].
    pub fn alloc_state(&self) -> (Option<u32>, u32) {
        self.colliders.alloc_state()
    }

    /// Exact inverse of the MOST RECENT collider insert that returned
    /// `handle`, given the state captured by [`Self::alloc_state`] just
    /// before it. LIFO contract: invalid if any other collider was
    /// inserted/removed/marked modified since. Does NOT touch the parent
    /// body — callers that used `insert_with_parent` must also restore the
    /// parent (its collider list, mass properties and change flags changed)
    /// and its modified-list membership
    /// ([`RigidBodySet::is_marked_modified`] / [`RigidBodySet::pop_modified`]).
    /// Mirrors [`RigidBodySet::revert_insert`].
    pub fn revert_insert(
        &mut self,
        handle: ColliderHandle,
        prev_free_head: Option<u32>,
        prev_items_len: u32,
    ) {
        let popped = self.modified_colliders.pop();
        assert_eq!(
            popped,
            Some(handle),
            "revert_insert: not the most recent insert"
        );
        self.colliders.revert_insert(handle.0, prev_free_head, prev_items_len);
    }

    /// Returns a handle that's guaranteed to be invalid.
    ///
    /// Useful as a sentinel/placeholder value.
    pub fn invalid_handle() -> ColliderHandle {
        ColliderHandle::from_raw_parts(crate::INVALID_U32, crate::INVALID_U32)
    }

    /// Iterates over all colliders in this collection.
    ///
    /// Yields `(handle, &Collider)` pairs for each collider (including disabled ones).
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (ColliderHandle, &Collider)> {
        self.colliders.iter().map(|(h, c)| (ColliderHandle(h), c))
    }

    /// Iterates over only the enabled colliders.
    ///
    /// Disabled colliders are excluded from physics simulation and queries.
    pub fn iter_enabled(&self) -> impl Iterator<Item = (ColliderHandle, &Collider)> {
        self.colliders
            .iter()
            .map(|(h, c)| (ColliderHandle(h), c))
            .filter(|(_, c)| c.is_enabled())
    }

    /// Iterates over all colliders with mutable access.
    #[cfg(not(feature = "dev-remove-slow-accessors"))]
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (ColliderHandle, &mut Collider)> {
        self.modified_colliders.clear();
        let modified_colliders = &mut self.modified_colliders;
        self.colliders.iter_mut().map(move |(h, co)| {
            // NOTE: we push unchecked here since we are just re-populating the
            //       `modified_colliders` set that we just cleared before iteration.
            modified_colliders.push_unchecked(ColliderHandle(h), co);
            (ColliderHandle(h), co)
        })
    }

    /// Iterates over only the enabled colliders with mutable access.
    #[cfg(not(feature = "dev-remove-slow-accessors"))]
    pub fn iter_enabled_mut(&mut self) -> impl Iterator<Item = (ColliderHandle, &mut Collider)> {
        self.iter_mut().filter(|(_, c)| c.is_enabled())
    }

    /// Returns how many colliders are currently in this collection.
    pub fn len(&self) -> usize {
        self.colliders.len()
    }

    /// Returns `true` if there are no colliders in this collection.
    pub fn is_empty(&self) -> bool {
        self.colliders.is_empty()
    }

    /// Checks if the given handle points to a valid collider that still exists.
    pub fn contains(&self, handle: ColliderHandle) -> bool {
        self.colliders.contains(handle.0)
    }

    /// Adds a standalone collider (not attached to any body) and returns its handle.
    ///
    /// Most colliders should be attached to rigid bodies using [`insert_with_parent()`](Self::insert_with_parent) instead.
    /// Standalone colliders are useful for sensors or static collision geometry that doesn't need a body.
    pub fn insert(&mut self, coll: impl Into<Collider>) -> ColliderHandle {
        let mut coll = coll.into();
        // Make sure the internal links are reset, they may not be
        // if this rigid-body was obtained by cloning another one.
        coll.reset_internal_references();
        coll.parent = None;
        let handle = ColliderHandle(self.colliders.insert(coll));
        // NOTE: we push unchecked because this is a brand-new collider
        //       so it was initialized with the changed flag but isn’t in
        //       the set yet.
        self.modified_colliders
            .push_unchecked(handle, &mut self.colliders[handle.0]);
        handle
    }

    /// Adds a collider attached to a rigid body and returns its handle.
    ///
    /// This is the most common way to add colliders. The collider's position is relative
    /// to its parent body, so when the body moves, the collider moves with it.
    ///
    /// # Example
    /// ```
    /// # use rapier3d::prelude::*;
    /// # let mut colliders = ColliderSet::new();
    /// # let mut bodies = RigidBodySet::new();
    /// # let body_handle = bodies.insert(RigidBodyBuilder::dynamic());
    /// // Create a ball collider attached to a dynamic body
    /// let collider_handle = colliders.insert_with_parent(
    ///     ColliderBuilder::ball(0.5),
    ///     body_handle,
    ///     &mut bodies
    /// );
    /// ```
    pub fn insert_with_parent(
        &mut self,
        coll: impl Into<Collider>,
        parent_handle: RigidBodyHandle,
        bodies: &mut RigidBodySet,
    ) -> ColliderHandle {
        let mut coll = coll.into();
        // Make sure the internal links are reset, they may not be
        // if this collider was obtained by cloning another one.
        coll.reset_internal_references();

        if let Some(prev_parent) = &mut coll.parent {
            prev_parent.handle = parent_handle;
        } else {
            coll.parent = Some(ColliderParent {
                handle: parent_handle,
                pos_wrt_parent: coll.pos.0,
            });
        }

        // NOTE: we use `get_mut` instead of `get_mut_internal` so that the
        // modification flag is updated properly.
        let parent = bodies
            .get_mut_internal_with_modification_tracking(parent_handle)
            .expect("Parent rigid body not found.");
        let handle = ColliderHandle(self.colliders.insert(coll));
        let coll = self.colliders.get_mut(handle.0).unwrap();
        // NOTE: we push unchecked because this is a brand-new collider
        //       so it was initialized with the changed flag but isn’t in
        //       the set yet.
        self.modified_colliders.push_unchecked(handle, coll);

        parent.add_collider_internal(
            handle,
            coll.parent.as_mut().unwrap(),
            &mut coll.pos,
            &coll.shape,
            &coll.mprops,
        );
        handle
    }

    /// Changes which rigid body a collider is attached to, or detaches it completely.
    ///
    /// Use this to move a collider from one body to another, or to make it standalone.
    ///
    /// # Parameters
    /// * `new_parent_handle` - `Some(handle)` to attach to a body, `None` to make standalone
    ///
    /// # Example
    /// ```
    /// # use rapier3d::prelude::*;
    /// # let mut colliders = ColliderSet::new();
    /// # let mut bodies = RigidBodySet::new();
    /// # let body_handle = bodies.insert(RigidBodyBuilder::dynamic());
    /// # let other_body = bodies.insert(RigidBodyBuilder::dynamic());
    /// # let collider_handle = colliders.insert_with_parent(ColliderBuilder::ball(0.5).build(), body_handle, &mut bodies);
    /// // Detach collider from its current body
    /// colliders.set_parent(collider_handle, None, &mut bodies);
    ///
    /// // Attach it to a different body
    /// colliders.set_parent(collider_handle, Some(other_body), &mut bodies);
    /// ```
    pub fn set_parent(
        &mut self,
        handle: ColliderHandle,
        new_parent_handle: Option<RigidBodyHandle>,
        bodies: &mut RigidBodySet,
    ) {
        if let Some(collider) = self.get_mut(handle) {
            let curr_parent = collider.parent.map(|p| p.handle);
            if new_parent_handle == curr_parent {
                return; // Nothing to do, this is the same parent.
            }

            // The broad phase suppresses fixed-fixed pairs at creation time
            // (see BroadPhaseBvh::pair_permanently_dead) based on the leaf's
            // AABB, which a re-parent does not move. Re-parenting a collider
            // off of a fixed body (or with no parent, which is Fixed by
            // convention) onto a non-fixed body would need those suppressed
            // pairs re-emitted, and this fork has no re-pairing path. Detaching
            // (`new_parent_handle == None`, effectively Fixed) stays legal —
            // internal removal paths null parents through this method.
            let curr_type = curr_parent
                .and_then(|h| bodies.get(h))
                .map(|rb| rb.body_type)
                .unwrap_or(RigidBodyType::Fixed);
            let new_type = new_parent_handle
                .and_then(|h| bodies.get(h))
                .map(|rb| rb.body_type)
                .unwrap_or(RigidBodyType::Fixed);
            assert!(
                curr_type != RigidBodyType::Fixed || new_type == RigidBodyType::Fixed,
                "set_parent: fixed -> {new_type:?} re-parent with attached collider is \
                 unsupported: fixed-fixed pairs are suppressed at the broad phase and cannot \
                 be re-created; remove and re-insert the collider instead",
            );

            collider.changes |= ColliderChanges::PARENT;

            if let Some(parent_handle) = curr_parent {
                if let Some(rb) = bodies.get_mut(parent_handle) {
                    rb.remove_collider_internal(handle);
                }
            }

            match new_parent_handle {
                Some(new_parent_handle) => {
                    if let Some(parent) = &mut collider.parent {
                        parent.handle = new_parent_handle;
                    } else {
                        collider.parent = Some(ColliderParent {
                            handle: new_parent_handle,
                            pos_wrt_parent: Isometry::identity(),
                        })
                    };

                    if let Some(rb) = bodies.get_mut(new_parent_handle) {
                        rb.add_collider_internal(
                            handle,
                            collider.parent.as_ref().unwrap(),
                            &mut collider.pos,
                            &collider.shape,
                            &collider.mprops,
                        );
                    }
                }
                None => collider.parent = None,
            }
        }
    }

    /// Removes a collider from the world.
    ///
    /// The collider is detached from its parent body (if any) and removed from all
    /// collision detection structures. Returns the removed collider if it existed.
    ///
    /// # Parameters
    /// * `wake_up` - If `true`, wakes up the parent body (useful when collider removal
    ///   changes the body's mass or collision behavior significantly)
    ///
    /// # Example
    /// ```
    /// # use rapier3d::prelude::*;
    /// # let mut colliders = ColliderSet::new();
    /// # let mut bodies = RigidBodySet::new();
    /// # let mut islands = IslandManager::new();
    /// # let body_handle = bodies.insert(RigidBodyBuilder::dynamic().build());
    /// # let handle = colliders.insert_with_parent(ColliderBuilder::ball(0.5).build(), body_handle, &mut bodies);
    /// if let Some(collider) = colliders.remove(
    ///     handle,
    ///     &mut islands,
    ///     &mut bodies,
    ///     true  // Wake up the parent body
    /// ) {
    ///     println!("Removed collider with shape: {:?}", collider.shared_shape());
    /// }
    /// ```
    pub fn remove(
        &mut self,
        handle: ColliderHandle,
        islands: &mut IslandManager,
        bodies: &mut RigidBodySet,
        wake_up: bool,
    ) -> Option<Collider> {
        let collider = self.colliders.remove(handle.0)?;

        /*
         * Delete the collider from its parent body.
         */
        // NOTE: we use `get_mut_internal_with_modification_tracking` instead of `get_mut_internal` so that the
        // modification flag is updated properly.
        if let Some(parent) = &collider.parent {
            if let Some(parent_rb) =
                bodies.get_mut_internal_with_modification_tracking(parent.handle)
            {
                parent_rb.remove_collider_internal(handle);

                if wake_up {
                    islands.wake_up(bodies, parent.handle, true);
                }
            }
        }

        /*
         * Publish removal.
         */
        self.removed_colliders.push(handle);

        Some(collider)
    }

    /// Gets a collider by its index without knowing the generation number.
    ///
    /// ⚠️ **Advanced/unsafe usage** - prefer [`get()`](Self::get) instead! See [`RigidBodySet::get_unknown_gen`] for details.
    pub fn get_unknown_gen(&self, i: u32) -> Option<(&Collider, ColliderHandle)> {
        self.colliders
            .get_unknown_gen(i)
            .map(|(c, h)| (c, ColliderHandle(h)))
    }

    /// Gets a mutable reference to a collider by its index without knowing the generation.
    ///
    /// ⚠️ **Advanced/unsafe usage** - prefer [`get_mut()`](Self::get_mut) instead!
    /// suffer form the ABA problem.
    #[cfg(not(feature = "dev-remove-slow-accessors"))]
    pub fn get_unknown_gen_mut(&mut self, i: u32) -> Option<(&mut Collider, ColliderHandle)> {
        let (collider, handle) = self.colliders.get_unknown_gen_mut(i)?;
        let handle = ColliderHandle(handle);
        self.modified_colliders.push_once(handle, collider);
        Some((collider, handle))
    }

    /// Gets a read-only reference to the collider with the given handle.
    ///
    /// Returns `None` if the handle is invalid or the collider was removed.
    pub fn get(&self, handle: ColliderHandle) -> Option<&Collider> {
        self.colliders.get(handle.0)
    }

    /// Gets a mutable reference to the collider with the given handle.
    ///
    /// Returns `None` if the handle is invalid or the collider was removed.
    /// Use this to modify collider properties like friction, restitution, sensor status, etc.
    #[cfg(not(feature = "dev-remove-slow-accessors"))]
    pub fn get_mut(&mut self, handle: ColliderHandle) -> Option<&mut Collider> {
        let result = self.colliders.get_mut(handle.0)?;
        self.modified_colliders.push_once(handle, result);
        Some(result)
    }

    /// Gets mutable references to two different colliders at once.
    ///
    /// Useful when you need to modify two colliders simultaneously. If both handles
    /// are the same, only the first value will be `Some`.
    #[cfg(not(feature = "dev-remove-slow-accessors"))]
    pub fn get_pair_mut(
        &mut self,
        handle1: ColliderHandle,
        handle2: ColliderHandle,
    ) -> (Option<&mut Collider>, Option<&mut Collider>) {
        if handle1 == handle2 {
            (self.get_mut(handle1), None)
        } else {
            let (mut co1, mut co2) = self.colliders.get2_mut(handle1.0, handle2.0);
            if let Some(co1) = co1.as_deref_mut() {
                self.modified_colliders.push_once(handle1, co1);
            }
            if let Some(co2) = co2.as_deref_mut() {
                self.modified_colliders.push_once(handle2, co2);
            }
            (co1, co2)
        }
    }

    pub(crate) fn index_mut_internal(&mut self, handle: ColliderHandle) -> &mut Collider {
        &mut self.colliders[handle.0]
    }

    pub(crate) fn get_mut_internal(&mut self, handle: ColliderHandle) -> Option<&mut Collider> {
        self.colliders.get_mut(handle.0)
    }

    // Just a very long name instead of `.get_mut` to make sure
    // this is really the method we wanted to use instead of `get_mut_internal`.
    #[allow(dead_code)]
    pub(crate) fn get_mut_internal_with_modification_tracking(
        &mut self,
        handle: ColliderHandle,
    ) -> Option<&mut Collider> {
        let result = self.colliders.get_mut(handle.0)?;
        self.modified_colliders.push_once(handle, result);
        Some(result)
    }
}

impl Index<crate::data::Index> for ColliderSet {
    type Output = Collider;

    fn index(&self, index: crate::data::Index) -> &Collider {
        &self.colliders[index]
    }
}

impl Index<ColliderHandle> for ColliderSet {
    type Output = Collider;

    fn index(&self, index: ColliderHandle) -> &Collider {
        &self.colliders[index.0]
    }
}

#[cfg(not(feature = "dev-remove-slow-accessors"))]
impl IndexMut<ColliderHandle> for ColliderSet {
    fn index_mut(&mut self, handle: ColliderHandle) -> &mut Collider {
        let collider = &mut self.colliders[handle.0];
        self.modified_colliders.push_once(handle, collider);
        collider
    }
}

#[cfg(test)]
mod revert_insert_tests {
    use super::*;
    use crate::dynamics::{IslandManager, RigidBodyBuilder, RigidBodyChanges, RigidBodySet};
    use crate::geometry::ColliderBuilder;
    use std::hash::{Hash, Hasher};

    fn h<T: Hash>(t: &T) -> u64 {
        let mut hasher = std::hash::DefaultHasher::new();
        t.hash(&mut hasher);
        hasher.finish()
    }

    /// Exact inverse of `insert_with_parent`, both parent-tracking cases:
    /// the parent NOT yet on the modified list (the tracking push must be
    /// popped) and already on it (it must not be). Mirrors the game-side
    /// `attach_collider_safe` undo closure; see also the public-API twin in
    /// the game crate's tests/hash_restore.rs.
    #[test]
    fn insert_with_parent_reverts_exactly_both_tracking_cases() {
        let mut bodies = RigidBodySet::new();
        let mut colliders = ColliderSet::new();
        let mut islands = IslandManager::new();

        // A parent body plus some collider history so the arena free list is
        // non-trivial (reuse-case allocation gets exercised too).
        let parent = bodies.insert(RigidBodyBuilder::fixed().build());
        let doomed = colliders.insert(ColliderBuilder::ball(1.0.into()).build());
        colliders.remove(doomed, &mut islands, &mut bodies, false);

        // Case A: parent NOT marked modified at attach time. Reach the clean
        // state the way the pipeline's end-of-step sweep does: clear the
        // flags of everything on the modified lists, then the lists.
        for handle in bodies.take_modified().iter() {
            if let Some(rb) = bodies.bodies.get_mut(handle.0) {
                rb.changes = RigidBodyChanges::empty();
            }
        }
        for handle in colliders.take_modified().iter() {
            // Removed colliders stay on the modified list with a vacant slot.
            if let Some(co) = colliders.colliders.get_mut(handle.0) {
                co.changes = ColliderChanges::empty();
            }
        }
        colliders.take_removed();
        assert!(!bodies.is_marked_modified(parent), "setup: parent must start clean");

        for was_modified_case in [false, true] {
            let was_modified = bodies.is_marked_modified(parent);
            assert_eq!(was_modified, was_modified_case, "case setup");

            let before_bodies = h(&bodies);
            let before_colliders = h(&colliders);
            let pre_parent = bodies.get(parent).unwrap().clone();
            let (free_head, len) = colliders.alloc_state();

            let ch = colliders.insert_with_parent(
                ColliderBuilder::capsule_y(0.5.into(), 0.25.into()).build(),
                parent,
                &mut bodies,
            );
            assert!(bodies.is_marked_modified(parent), "attach must mark the parent");

            // Exact LIFO inverse of insert_with_parent:
            colliders.revert_insert(ch, free_head, len);
            bodies.restore_raw(parent, pre_parent);
            if !was_modified {
                bodies.pop_modified(parent);
            }

            assert_eq!(
                before_bodies,
                h(&bodies),
                "parent body + modified list must restore (was_modified={was_modified})"
            );
            assert_eq!(
                before_colliders,
                h(&colliders),
                "collider slot + alloc + modified list must restore (was_modified={was_modified})"
            );

            // Case B setup: leave the parent marked modified for the second
            // iteration by touching it through the tracked accessor.
            if !was_modified_case {
                let _ = bodies.get_mut_internal_with_modification_tracking(parent);
            }
        }
    }
}
