//! mmo-fork: per-tick rollback journal (upgrade plan Task 4.2, design D1 — the
//! "hybrid" journal).
//!
//! [`PhysicsPipeline::step_journaled`](crate::pipeline::PhysicsPipeline::step_journaled)
//! records everything a step mutates so the tick can be undone exactly,
//! without a whole-state snapshot:
//!
//! - **First-touch value capture** of every `RigidBody` / `Collider` the step
//!   writes. The sets carry a capture buffer for the duration of the step, and
//!   every accessor that hands out `&mut` saves the value on its first use
//!   (`Vec<(handle, value)>` + a dedup bitset). No call site inside the engine
//!   knows about the journal. The staged solver writes bodies through a raw
//!   `*mut RigidBodySet` from its workers; its writeback debug-asserts every
//!   body it writes was already captured.
//! - **Wholesale clones at step start** of the small structures: `IslandManager`,
//!   `NarrowPhase`, `BroadPhaseBvh`, the sets' modified/removed lists, and the
//!   joint sets when they hold anything the step can touch. The broad phase is
//!   the whole cost (~240 KB at region scale, see the spike doc): its
//!   serialized `frame_index` changes every step, so there is no clean tick.
//!
//! Revert writes the captured values back and swaps the clones in. It restores
//! the seven physics structures of [`PhysicsParts`] byte for byte. The
//! `PhysicsPipeline` and the `CCDSolver` hold memos of the undone step instead
//! of state (Task 4.3): the revert discards the CCD solver's fixed-targets
//! cache and bumps a counter in the `IslandManager` that makes the pipeline
//! drop its memos at its next step, so the caller steps on with the same
//! pipeline and solver and needs no post-revert call.
//!
//! **Digests** (`HashMode::Delta` in the game): [`StepJournal::pre_digest`]
//! hashes the captured pre-step footprint and [`StepJournal::revert_digest`]
//! reverts, then hashes the same footprint from the live structures; they are
//! equal iff the revert restored it exactly. The footprint is what the revert
//! restores piecemeal — the saved bodies and colliders, by sorted handle — plus
//! the joint sets when the step was judged unable to touch them (not cloned):
//! their pre-step hash, so that judgement is checked instead of trusted. The
//! wholesale clones are left out: the revert moves the saved clone itself back,
//! so hashing it on both sides could never differ, and the broad phase alone
//! is ~240 KB per side. Both sides go through one walker,
//! [`digest_footprint`], so they can't drift.
//!
//! Contract: nothing but the step itself may run between `step_journaled` and
//! `revert` on the journaled structures, and journals are reverted in LIFO order.
//! User mutations (inserts, removals, `get_mut` writes) between steps are owned
//! by the game's undo log, not by this journal.

use crate::alloc_prelude::*;

use crate::dynamics::{
    CCDSolver, ImpulseJointSet, IslandManager, MultibodyJointSet, RigidBody, RigidBodyHandle, RigidBodySet,
    SoftBodySet,
};
use crate::geometry::{BroadPhaseBvh, Collider, ColliderHandle, ColliderSet, NarrowPhase};

/// First-touch capture buffer: the pre-step value of every object handed out
/// by `&mut` during a journaled step, at most once per arena slot.
#[derive(Clone, Debug)]
pub(crate) struct Capture<H, T> {
    saved: Vec<(H, T)>,
    /// One bit per arena slot index. Keyed by slot, not generation: a step
    /// never inserts or removes bodies or colliders.
    seen: Vec<u64>,
}

impl<H: Copy, T: Clone> Capture<H, T> {
    pub(crate) fn new() -> Self {
        Self {
            saved: Vec::new(),
            seen: Vec::new(),
        }
    }

    /// Saves `value` unless slot `slot` was saved already this step (the
    /// oldest, i.e. pre-step, value wins).
    #[inline]
    pub(crate) fn save(&mut self, handle: H, slot: u32, value: &T) {
        let (word, bit) = ((slot / 64) as usize, slot % 64);
        if word >= self.seen.len() {
            self.seen.resize(word + 1, 0);
        }
        if self.seen[word] & (1 << bit) == 0 {
            self.seen[word] |= 1 << bit;
            self.saved.push((handle, value.clone()));
        }
    }

    #[inline]
    pub(crate) fn contains(&self, slot: u32) -> bool {
        let (word, bit) = ((slot / 64) as usize, slot % 64);
        self.seen.get(word).is_some_and(|w| w & (1 << bit) != 0)
    }
}

/// The seven structures a journaled step mutates and [`StepJournal::revert`]
/// restores.
pub struct PhysicsParts<'a> {
    /// The island manager.
    pub islands: &'a mut IslandManager,
    /// The broad phase.
    pub broad_phase: &'a mut BroadPhaseBvh,
    /// The narrow phase.
    pub narrow_phase: &'a mut NarrowPhase,
    /// The rigid bodies.
    pub bodies: &'a mut RigidBodySet,
    /// The colliders.
    pub colliders: &'a mut ColliderSet,
    /// The impulse joints.
    pub impulse_joints: &'a mut ImpulseJointSet,
    /// The multibody joints.
    pub multibody_joints: &'a mut MultibodyJointSet,
    /// 0.36: soft bodies are compiled in; the game keeps this empty (asserted).
    pub soft_bodies: &'a mut SoftBodySet,
    /// Not restored: its fixed-targets cache (which a revert can make name a
    /// vacant collider) is discarded.
    pub ccd_solver: &'a mut CCDSolver,
}

struct Wholesale {
    islands: IslandManager,
    narrow_phase: NarrowPhase,
    broad_phase: BroadPhaseBvh,
    modified_bodies: crate::dynamics::ModifiedRigidBodies,
    modified_colliders: crate::geometry::ModifiedColliders,
    removed_colliders: Vec<ColliderHandle>,
    joints: Option<(ImpulseJointSet, MultibodyJointSet)>,
    /// The joint sets' `StateHash` when they were judged quiescent and not
    /// cloned: the digests check that the step indeed left them alone.
    quiescent_joints: Option<u32>,
}

/// Per-tick mutation journal for
/// [`PhysicsPipeline::step_journaled`](crate::pipeline::PhysicsPipeline::step_journaled);
/// see the module docs.
#[derive(Default)]
pub struct StepJournal {
    bodies: Vec<(RigidBodyHandle, RigidBody)>,
    colliders: Vec<(ColliderHandle, Collider)>,
    wholesale: Option<Box<Wholesale>>,
}

impl StepJournal {
    /// An empty journal, to pass to one `step_journaled` call.
    pub fn new() -> Self {
        Self::default()
    }

    /// True if the step touched no rigid body and no collider. The wholesale
    /// clones are taken on every journaled step and don't count.
    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty() && self.colliders.is_empty()
    }

    /// Number of rigid bodies whose pre-step value this journal saved: the
    /// per-tick cost that scales with activity rather than world size.
    pub fn saved_body_count(&self) -> usize {
        self.bodies.len()
    }

    /// Number of colliders whose pre-step value this journal saved.
    pub fn saved_collider_count(&self) -> usize {
        self.colliders.len()
    }

    /// True once a journaled step has run: the hybrid journal clones the
    /// broad phase on every step (kept for the old fork's API).
    pub fn broad_captured(&self) -> bool {
        self.wholesale.is_some()
    }

    /// Whether the joint sets were cloned (they held joints or pending joint
    /// work at step start).
    pub fn joints_captured(&self) -> bool {
        self.wholesale.as_ref().is_some_and(|w| w.joints.is_some())
    }

    /// Takes the wholesale clones and installs the capture buffers. Called by
    /// `step_journaled` right before the step.
    pub(crate) fn begin(&mut self, p: PhysicsParts<'_>) {
        assert!(
            self.wholesale.is_none() && self.is_empty(),
            "a StepJournal records exactly one step"
        );
        debug_assert!(p.soft_bodies.is_empty(), "the journal does not capture soft bodies");
        debug_assert!(
            p.bodies.capture.is_none() && p.colliders.capture.is_none(),
            "nested journaled steps"
        );

        let (joints, quiescent_joints) = if joint_sets_touchable(p.impulse_joints, p.multibody_joints) {
            (Some((p.impulse_joints.clone(), p.multibody_joints.clone())), None)
        } else {
            (None, Some(joint_sets_hash(p.impulse_joints, p.multibody_joints)))
        };
        self.wholesale = Some(Box::new(Wholesale {
            islands: p.islands.clone(),
            narrow_phase: p.narrow_phase.clone(),
            broad_phase: p.broad_phase.clone(),
            modified_bodies: p.bodies.modified_bodies.clone(),
            modified_colliders: p.colliders.modified_colliders.clone(),
            removed_colliders: p.colliders.removed_colliders.clone(),
            joints,
            quiescent_joints,
        }));
        p.bodies.capture = Some(Box::new(Capture::new()));
        p.colliders.capture = Some(Box::new(Capture::new()));
    }

    /// Removes the capture buffers from the sets and keeps what they saved.
    pub(crate) fn end(&mut self, bodies: &mut RigidBodySet, colliders: &mut ColliderSet) {
        let b = bodies.capture.take().expect("journal capture missing");
        let c = colliders.capture.take().expect("journal capture missing");
        self.bodies = b.saved;
        self.colliders = c.saved;
    }

    /// crc32 of the captured pre-step footprint (the game's Delta hash mode):
    /// the saved bodies and colliders by sorted handle, plus the joint sets'
    /// pre-step hash when they weren't cloned (see the module docs). Equal to
    /// [`Self::revert_digest`] iff the revert restored that footprint exactly.
    pub fn pre_digest(&self) -> u32 {
        let w = self.wholesale.as_ref().expect("digest of a journal that recorded no step");
        digest_footprint(Footprint {
            bodies: self.bodies.iter().map(|(h, b)| (*h, Some(b))).collect(),
            colliders: self.colliders.iter().map(|(h, c)| (*h, Some(c))).collect(),
            quiescent_joints: w.quiescent_joints,
        })
    }

    /// The footprint this journal captured, read from `parts` as they stand
    /// (no revert): equals [`Self::pre_digest`] iff `parts` hold the pre-step
    /// values of everything the step touched. [`Self::revert_digest`] is this,
    /// right after the revert.
    pub fn live_digest(&self, parts: &PhysicsParts<'_>) -> u32 {
        let w = self.wholesale.as_ref().expect("digest of a journal that recorded no step");
        let bodies: Vec<_> = self.bodies.iter().map(|(h, _)| *h).collect();
        let colliders: Vec<_> = self.colliders.iter().map(|(h, _)| *h).collect();
        live_footprint_digest(bodies, colliders, w.quiescent_joints.is_some(), parts)
    }

    /// Reverts (see [`Self::revert`]) and returns the post-revert footprint
    /// digest, to compare with the [`Self::pre_digest`] taken at step time.
    pub fn revert_digest(self, parts: PhysicsParts<'_>) -> u32 {
        let w = self.wholesale.as_ref().expect("digest of a journal that recorded no step");
        let quiescent_joints = w.quiescent_joints.is_some();
        let bodies: Vec<_> = self.bodies.iter().map(|(h, _)| *h).collect();
        let colliders: Vec<_> = self.colliders.iter().map(|(h, _)| *h).collect();
        self.revert(PhysicsParts {
            islands: &mut *parts.islands,
            broad_phase: &mut *parts.broad_phase,
            narrow_phase: &mut *parts.narrow_phase,
            bodies: &mut *parts.bodies,
            colliders: &mut *parts.colliders,
            impulse_joints: &mut *parts.impulse_joints,
            multibody_joints: &mut *parts.multibody_joints,
            soft_bodies: &mut *parts.soft_bodies,
            ccd_solver: &mut *parts.ccd_solver,
        });
        live_footprint_digest(bodies, colliders, quiescent_joints, &parts)
    }

    /// Undoes the journaled step, restoring `parts` to their pre-step state
    /// byte for byte. Journals of consecutive steps must be reverted LIFO.
    pub fn revert(self, parts: PhysicsParts<'_>) {
        let PhysicsParts {
            islands,
            broad_phase,
            narrow_phase,
            bodies,
            colliders,
            impulse_joints,
            multibody_joints,
            soft_bodies,
            ccd_solver,
        } = parts;
        debug_assert!(soft_bodies.is_empty(), "the journal does not capture soft bodies");
        let w = self
            .wholesale
            .expect("revert of a journal that recorded no step");

        // Each handle is saved once, so the order doesn't matter; LIFO anyway.
        for (handle, body) in self.bodies.into_iter().rev() {
            bodies.restore_raw(handle, body);
        }
        for (handle, collider) in self.colliders.into_iter().rev() {
            colliders.restore_raw(handle, collider);
        }

        let Wholesale {
            islands: saved_islands,
            narrow_phase: saved_narrow,
            broad_phase: saved_broad,
            modified_bodies,
            modified_colliders,
            removed_colliders,
            joints,
            quiescent_joints: _,
        } = *w;
        // The counter survives the swap and moves (see its doc).
        let reverts = islands.journal_reverts.wrapping_add(1);
        *islands = saved_islands;
        islands.journal_reverts = reverts;
        *narrow_phase = saved_narrow;
        *broad_phase = saved_broad;
        bodies.set_modified(modified_bodies);
        colliders.set_modified(modified_colliders);
        colliders.set_removed(removed_colliders);
        if let Some((ij, mj)) = joints {
            *impulse_joints = ij;
            *multibody_joints = mj;
        }
        // The undone step may have rebuilt the cache with colliders the caller's
        // own undo then removes (a terrain edit), and the re-step sees no user
        // change that would invalidate it.
        ccd_solver.invalidate_fixed_targets_cache();
    }
}

/// Whether a step can write anything in the joint sets. False only for sets
/// with no joint and no pending joint work: the step then only drains empty
/// queues, so skipping their clone loses nothing.
fn joint_sets_touchable(ij: &ImpulseJointSet, mj: &MultibodyJointSet) -> bool {
    !ij.journal_quiescent() || !mj.journal_quiescent()
}

/// The live side of the footprint digest: the journal's handles (in any
/// order) and whether its joint sets were judged quiescent, read from `parts`.
fn live_footprint_digest(
    bodies: Vec<RigidBodyHandle>,
    colliders: Vec<ColliderHandle>,
    quiescent_joints: bool,
    parts: &PhysicsParts<'_>,
) -> u32 {
    digest_footprint(Footprint {
        bodies: bodies.into_iter().map(|h| (h, parts.bodies.get(h))).collect(),
        colliders: colliders.into_iter().map(|h| (h, parts.colliders.get(h))).collect(),
        quiescent_joints: quiescent_joints.then(|| joint_sets_hash(parts.impulse_joints, parts.multibody_joints)),
    })
}

/// `StateHash` of both joint sets (cheap: only taken when they are quiescent).
fn joint_sets_hash(ij: &ImpulseJointSet, mj: &MultibodyJointSet) -> u32 {
    use state_hash::StateHash;
    let mut h = crc32fast::Hasher::new();
    ij.state_hash(&mut h);
    mj.state_hash(&mut h);
    h.finalize()
}

/// One side of a footprint digest: saved values (`pre_digest`) or live ones
/// (`live_digest`, `revert_digest`), bodies and colliders in capture order; a
/// live slot a revert failed to refill hashes as `None`.
struct Footprint<'a> {
    bodies: Vec<(RigidBodyHandle, Option<&'a RigidBody>)>,
    colliders: Vec<(ColliderHandle, Option<&'a Collider>)>,
    quiescent_joints: Option<u32>,
}

/// The one walker both digest sides use, so they hash identical logical
/// content in identical order and cannot drift. Bodies and colliders go in
/// sorted by handle (capture order is first-touch order, not canonical).
fn digest_footprint(mut f: Footprint<'_>) -> u32 {
    use state_hash::StateHash;
    f.bodies.sort_unstable_by_key(|(h, _)| *h);
    f.colliders.sort_unstable_by_key(|(h, _)| h.into_raw_parts());
    let mut h = crc32fast::Hasher::new();
    f.bodies.state_hash(&mut h);
    f.colliders.state_hash(&mut h);
    f.quiescent_joints.state_hash(&mut h);
    h.finalize()
}
