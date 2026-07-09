//! Per-tick mutation journal: save-before-first-write capture of everything
//! `PhysicsPipeline::step` mutates, with an exact LIFO revert. Pure capture —
//! never influences simulation. See the game repo spec
//! `docs/superpowers/specs/2026-07-04-physics-step-journal-design.md`.
use crate::dynamics::{
    ImpulseJointSet, IslandManager, MultibodyJointSet, RigidBody, RigidBodyHandle, RigidBodySet,
};
use crate::geometry::{BroadPhaseBvh, Collider, ColliderHandle, ColliderSet, NarrowPhase};
use parry3d::utils::hashset::HashSet;
use std::hash::Hash;

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
    /// Per-edge dedup guard for the solver-boundary payload saves in
    /// [`NarrowPhase::select_active_contacts`]. A re-woken body's dormant contact
    /// pairs carry no collider flags, so `compute_contacts` never saves them, yet
    /// the solver writes warm-start impulses into their manifolds; select's save
    /// closes that gap. This set records which contact-graph edges select has
    /// already snapshotted so the same edge is captured at most once across the
    /// tick's substeps. Double-saves against `compute_contacts` (a pair that is
    /// both flag-dirty and selected) are harmless: LIFO revert applies the
    /// latest-pushed op first, so the earliest (true pre-tick) payload always wins.
    pub(crate) saved_narrow_edges: HashSet<u32>,
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

/// The handle lists + per-section presence flags a [`StepJournal::revert_inner`]
/// restored on one tick. Lets [`StepJournal::revert_digest`] re-read the exact
/// same footprint from the live structures without duplicating the revert logic.
/// Handle lists are stored sorted ascending (canonical digest order).
struct Footprint {
    body_handles: Vec<RigidBodyHandle>,
    collider_handles: Vec<ColliderHandle>,
    had_islands: bool,
    had_lists: bool,
    had_joints: bool,
    had_narrow_wholesale: bool,
    had_broad: bool,
}

impl StepJournal {
    /// True if this tick mutated the broad phase (a pre-mutation BVH snapshot was
    /// captured). False on a clean tick where the broad phase was never touched.
    pub fn broad_captured(&self) -> bool {
        self.broad.is_some()
    }

    /// Number of rigid bodies whose pre-tick state this journal saved. A proxy for
    /// per-tick undo-log cost that scales with activity, not world size.
    pub fn saved_body_count(&self) -> usize {
        self.saved_bodies.len()
    }

    /// Number of colliders whose pre-tick state this journal saved. A proxy for
    /// per-tick undo-log cost that scales with activity, not world size.
    pub fn saved_collider_count(&self) -> usize {
        self.saved_colliders.len()
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
    /// be reverted twice. Thin wrapper over [`Self::revert_inner`] (whose returned
    /// footprint it discards); [`Self::revert_digest`] uses the same inner fn so
    /// the revert logic lives in exactly one place.
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
        let _ = self.revert_inner(
            islands,
            broad_phase,
            narrow_phase,
            bodies,
            colliders,
            impulse_joints,
            multibody_joints,
        );
    }

    /// The shared body of [`Self::revert`], returning the reverted tick's
    /// [`Footprint`] (the sorted body/collider handles it restored plus the
    /// per-section presence flags). Restoration is unchanged from the original
    /// `revert`; the only addition is snapshotting the footprint up front so
    /// [`Self::revert_digest`] can re-read it from the live structures afterwards
    /// without re-implementing any revert logic.
    fn revert_inner(
        self,
        islands: &mut IslandManager,
        broad_phase: &mut BroadPhaseBvh,
        narrow_phase: &mut NarrowPhase,
        bodies: &mut RigidBodySet,
        colliders: &mut ColliderSet,
        impulse_joints: &mut ImpulseJointSet,
        multibody_joints: &mut MultibodyJointSet,
    ) -> Footprint {
        // Snapshot the footprint (handle lists + section presence) BEFORE the
        // moves below consume `self`. Handles are sorted ascending so both digest
        // sides iterate in the same canonical order (saved order is first-touch,
        // not canonical).
        let mut body_handles: Vec<RigidBodyHandle> =
            self.saved_bodies.iter().map(|(h, _)| *h).collect();
        body_handles.sort_unstable();
        let mut collider_handles: Vec<ColliderHandle> =
            self.saved_colliders.iter().map(|(h, _)| *h).collect();
        collider_handles.sort_unstable();
        let footprint = Footprint {
            body_handles,
            collider_handles,
            had_islands: self.islands.is_some(),
            had_lists: self.lists.is_some(),
            had_joints: self.joints.is_some(),
            had_narrow_wholesale: self.narrow_wholesale.is_some(),
            had_broad: self.broad.is_some(),
        };

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

        footprint
    }

    /// Digest of the **pre-tick** values of everything this journal captured,
    /// section by section in a fixed order: (1) bodies, (2) colliders,
    /// (3) islands, (4) lists, (5) joints, (6) narrow, (7) broad. Its counterpart
    /// [`Self::revert_digest`] recomputes the identical digest from the LIVE
    /// structures after an exact revert, so the two are equal iff the revert
    /// restored the captured footprint bit-for-bit.
    ///
    /// Parity rules (both sides MUST hash identical logical content in identical
    /// order — the per-section `digest_*` helpers are called from both sides to
    /// keep them from drifting):
    /// - Bodies/colliders: handles sorted ascending; hash `(handle, Some(&saved))`
    ///   here vs `(handle, set.get(handle))` on the live side.
    /// - Wholesale-restored sections (islands, lists, joints, narrow-wholesale,
    ///   broad): the saved snapshot IS the post-revert live state, so `hash(saved)`
    ///   here must equal `hash(live-after-restore)` there — paired digest fns hash
    ///   the same fields in the same order.
    /// - HashSets (`*_to_wake_up`) are hashed as sorted `Vec`s on both sides.
    /// - The fine-grained `narrow` op log is EXCLUDED from both sides; only a
    ///   presence `bool` for the WHOLESALE snapshot is hashed. Its payloads are
    ///   covered by Full mode and the fork's own step_journal revert-exactness
    ///   tests, not by Delta digests. A presence `bool` is hashed for every
    ///   optional section so presence/absence itself is checked.
    pub fn pre_digest(&self) -> u32 {
        let mut h = crc32fast::Hasher::new();

        // 1. Bodies: saved payloads keyed by handle, sorted ascending.
        let mut bodies_sorted: Vec<(RigidBodyHandle, &RigidBody)> =
            self.saved_bodies.iter().map(|(hnd, b)| (*hnd, b)).collect();
        bodies_sorted.sort_unstable_by_key(|(hnd, _)| *hnd);
        Self::digest_bodies(&mut h, bodies_sorted.iter().map(|&(hnd, b)| (hnd, Some(b))));

        // 2. Colliders: same shape.
        let mut colliders_sorted: Vec<(ColliderHandle, &Collider)> =
            self.saved_colliders.iter().map(|(hnd, c)| (*hnd, c)).collect();
        colliders_sorted.sort_unstable_by_key(|(hnd, _)| *hnd);
        Self::digest_colliders(&mut h, colliders_sorted.iter().map(|&(hnd, c)| (hnd, Some(c))));

        // 3. Islands: presence + saved payload (cf. revert_digest step 3).
        self.islands.is_some().hash(&mut h);
        if let Some(s) = &self.islands {
            s.digest(&mut h);
        }

        // 4. Lists: presence + saved five values (cf. revert_digest step 4).
        self.lists.is_some().hash(&mut h);
        if let Some(l) = &self.lists {
            Self::digest_lists(
                &mut h,
                &l.modified_bodies,
                &l.modified_colliders,
                &l.removed_colliders,
                &l.impulse_to_wake_up,
                &l.multibody_to_wake_up,
            );
        }

        // 5. Joints: presence + saved sets (cf. revert_digest step 5).
        self.joints.is_some().hash(&mut h);
        if let Some(j) = &self.joints {
            Self::digest_joints(&mut h, &j.0, &j.1);
        }

        // 6. Narrow phase: presence of the WHOLESALE snapshot + its payload. The
        //    fine-grained op log is intentionally not hashed on either side.
        self.narrow_wholesale.is_some().hash(&mut h);
        if let Some(w) = &self.narrow_wholesale {
            Self::digest_narrow(&mut h, w);
        }

        // 7. Broad phase: presence + saved payload (cf. revert_digest step 7).
        self.broad.is_some().hash(&mut h);
        if let Some(b) = &self.broad {
            b.digest(&mut h);
        }

        h.finalize()
    }

    /// Runs exactly [`Self::revert`] (via [`Self::revert_inner`]), then recomputes
    /// [`Self::pre_digest`]'s digest from the now-restored LIVE structures over the
    /// same footprint and section order. Equal to `pre_digest()` iff the revert
    /// restored the footprint exactly. Same parameters as [`Self::revert`].
    pub fn revert_digest(
        self,
        islands: &mut IslandManager,
        broad_phase: &mut BroadPhaseBvh,
        narrow_phase: &mut NarrowPhase,
        bodies: &mut RigidBodySet,
        colliders: &mut ColliderSet,
        impulse_joints: &mut ImpulseJointSet,
        multibody_joints: &mut MultibodyJointSet,
    ) -> u32 {
        // Consumes the journal and restores the live structures; `fp` carries the
        // exact footprint so the digest below re-reads the same logical content.
        let fp = self.revert_inner(
            islands,
            broad_phase,
            narrow_phase,
            bodies,
            colliders,
            impulse_joints,
            multibody_joints,
        );

        let mut h = crc32fast::Hasher::new();

        // 1. Bodies: live values for the same sorted handles (cf. pre_digest 1).
        Self::digest_bodies(
            &mut h,
            fp.body_handles.iter().map(|&hnd| (hnd, bodies.get(hnd))),
        );

        // 2. Colliders (cf. pre_digest 2).
        Self::digest_colliders(
            &mut h,
            fp.collider_handles.iter().map(|&hnd| (hnd, colliders.get(hnd))),
        );

        // 3. Islands: live fields after journal_restore (cf. pre_digest 3).
        fp.had_islands.hash(&mut h);
        if fp.had_islands {
            islands.journal_digest(&mut h);
        }

        // 4. Lists: live modified/removed lists + wake sets, read via the same
        //    fields `revert_inner` restores (cf. pre_digest 4).
        fp.had_lists.hash(&mut h);
        if fp.had_lists {
            Self::digest_lists(
                &mut h,
                &bodies.modified_bodies,
                &colliders.modified_colliders,
                &colliders.removed_colliders,
                &impulse_joints.to_wake_up,
                &multibody_joints.to_wake_up,
            );
        }

        // 5. Joints: live sets (cf. pre_digest 5).
        fp.had_joints.hash(&mut h);
        if fp.had_joints {
            Self::digest_joints(&mut h, impulse_joints, multibody_joints);
        }

        // 6. Narrow phase: live narrow phase, only when a wholesale snapshot was
        //    captured (cf. pre_digest 6).
        fp.had_narrow_wholesale.hash(&mut h);
        if fp.had_narrow_wholesale {
            Self::digest_narrow(&mut h, narrow_phase);
        }

        // 7. Broad phase: live fields after journal_restore (cf. pre_digest 7).
        fp.had_broad.hash(&mut h);
        if fp.had_broad {
            broad_phase.journal_digest(&mut h);
        }

        h.finalize()
    }

    // --- Per-section digest helpers, shared by pre_digest and revert_digest so
    //     the two sides cannot drift. `Option<&T>` hashes identically whether the
    //     value came from a saved payload (`Some(&saved)`) or a live lookup
    //     (`set.get(handle)`), which is what makes the pre/live parity hold.

    fn digest_bodies<'a>(
        h: &mut crc32fast::Hasher,
        items: impl Iterator<Item = (RigidBodyHandle, Option<&'a RigidBody>)>,
    ) {
        for (handle, body) in items {
            handle.hash(h);
            body.hash(h);
        }
    }

    fn digest_colliders<'a>(
        h: &mut crc32fast::Hasher,
        items: impl Iterator<Item = (ColliderHandle, Option<&'a Collider>)>,
    ) {
        for (handle, collider) in items {
            handle.hash(h);
            collider.hash(h);
        }
    }

    fn digest_lists(
        h: &mut crc32fast::Hasher,
        modified_bodies: &crate::dynamics::ModifiedRigidBodies,
        modified_colliders: &crate::geometry::ModifiedColliders,
        removed_colliders: &[ColliderHandle],
        impulse_to_wake_up: &HashSet<RigidBodyHandle>,
        multibody_to_wake_up: &HashSet<RigidBodyHandle>,
    ) {
        // Modified/removed are order-deterministic Vecs (restored wholesale), so
        // hash them directly. The to-wake-up HashSets have no canonical iteration
        // order, so hash a sorted Vec of their contents on both sides.
        modified_bodies.hash(h);
        modified_colliders.hash(h);
        removed_colliders.hash(h);
        Self::hash_sorted_bodies(h, impulse_to_wake_up);
        Self::hash_sorted_bodies(h, multibody_to_wake_up);
    }

    fn digest_joints(h: &mut crc32fast::Hasher, ij: &ImpulseJointSet, mj: &MultibodyJointSet) {
        ij.hash(h);
        mj.hash(h);
    }

    fn digest_narrow(h: &mut crc32fast::Hasher, narrow: &NarrowPhase) {
        narrow.hash(h);
    }

    fn hash_sorted_bodies(h: &mut crc32fast::Hasher, set: &HashSet<RigidBodyHandle>) {
        let mut v: Vec<RigidBodyHandle> = set.iter().copied().collect();
        v.sort_unstable();
        v.hash(h);
    }
}
