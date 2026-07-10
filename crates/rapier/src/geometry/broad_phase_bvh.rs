use ordered_float::OrderedFloat;

use parry3d::partitioning::{Bvh, BvhWorkspace};
use parry3d::utils::hashmap::{Entry, HashMap};
use crate::dynamics::{IntegrationParameters, RigidBodySet};
use crate::geometry::{
    Aabb, BroadPhasePairEvent, ColliderHandle, ColliderPair, ColliderSet,
};
use crate::math::Real;
use crate::pipeline::StepJournal;

/// Pre-mutation snapshot of the hashed broad-phase state, captured by the
/// [`StepJournal`] the first time a step is about to mutate the BVH. The
/// workspace is unhashed serde-skipped scratch and is deliberately never
/// captured or restored.
pub struct BroadSaved {
    pub(crate) tree: Bvh,
    pub(crate) pairs: HashMap<(ColliderHandle, ColliderHandle), u32>,
    /// The multiset hash of `pairs` at capture time (see
    /// [`BroadPhaseBvh::pairs_hash`]). Restored with the map so the cache
    /// stays in lockstep across rollbacks; the pre-side digest hashes this
    /// O(1) value while the live side recomputes from the restored map,
    /// making the comparison a genuine content check.
    pub(crate) pairs_hash: u64,
    pub(crate) frame_index: u32,
    pub(crate) pending_leaf_changes: bool,
}

impl BroadSaved {
    /// Pre-side twin of [`BroadPhaseBvh::journal_digest`]; mirrors
    /// [`BroadPhaseBvh::journal_restore`]'s field writes in order (tree, pairs,
    /// frame_index, pending_leaf_changes). `optimization_strategy` is not part of
    /// the journal snapshot, so it is excluded here too. Keep all four field lists
    /// in lockstep.
    ///
    /// Pairs content is represented by the multiset hash captured at save
    /// time (O(1) here); the live twin RECOMPUTES it from the restored map,
    /// so digest equality proves the restored pairs content matches the
    /// captured content — and doubles as a cache-integrity check.
    pub(crate) fn digest<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;
        self.tree.hash(state);
        state.write_u64(self.pairs_hash);
        self.frame_index.hash(state);
        self.pending_leaf_changes.hash(state);
    }
}

/// The broad-phase collision detector that quickly filters out distant object pairs.
///
/// The broad-phase is the "first pass" of collision detection. It uses a hierarchical
/// bounding volume tree (BVH) to quickly identify which collider pairs are close enough
/// to potentially collide, avoiding expensive narrow-phase checks for distant objects.
///
/// Think of it as a "spatial index" that answers: "Which objects are near each other?"
///
/// You typically don't interact with this directly - it's managed by [`PhysicsPipeline`](crate::pipeline::PhysicsPipeline).
/// However, you can use it to create a [`QueryPipeline`](crate::pipeline::QueryPipeline) for spatial queries.
#[derive(Default, Clone)]
#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
pub struct BroadPhaseBvh {
    pub(crate) tree: Bvh,
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    workspace: BvhWorkspace,
    pairs: HashMap<(ColliderHandle, ColliderHandle), u32>,
    frame_index: u32,
    optimization_strategy: BvhOptimizationStrategy,
    /// True when [`Self::set_aabb`]/[`Self::set_aabb_journaled`] wrote leaves
    /// since the last [`Self::update`]. Moving bodies refresh their leaves
    /// through the last-substep `set_aabb` path WITHOUT appearing in the next
    /// tick's `modified_colliders`, so `update` must treat such a tick as dirty
    /// (run the BVTT traversal + pair GC) even with empty modified/removed
    /// lists — otherwise new overlaps from those moves would never form pairs.
    /// Hashed and journal-captured like the rest of the broad-phase state so
    /// rollback and replay stay bit-exact.
    pending_leaf_changes: bool,
    /// Incrementally-maintained commutative multiset hash of `pairs`
    /// (wrapping-add of [`Self::pair_entry_digest`] per entry). Lets the
    /// step-journal Delta digests avoid walking the whole map every tick.
    /// Derived data, like `workspace`: NOT part of `impl Hash` (Full mode
    /// keeps hashing the map itself) and serde-skipped — `pairs` serializes
    /// capacity-only (deserializes EMPTY), so the empty-multiset default of 0
    /// matches. Journal-captured/restored via [`BroadSaved::pairs_hash`].
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pairs_hash: u64,
}

impl std::hash::Hash for BroadPhaseBvh {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.tree.hash(state);
        self.pairs.hash(state);
        self.frame_index.hash(state);
        self.optimization_strategy.hash(state);
        self.pending_leaf_changes.hash(state);
    }
}

// TODO: would be interesting to try out:
// "Fast Insertion-Based Optimization of Bounding Volume Hierarchies"
// by Bittner et al.
/// Selection of strategies to maintain through time the broad-phase BVH in shape that remains
/// efficient for collision-detection and scene queries.
#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
#[derive(Default, PartialEq, Eq, Copy, Clone, Hash)]
pub enum BvhOptimizationStrategy {
    /// Different sub-trees of the BVH will be optimized at each frame.
    SubtreeOptimizer,
    /// Disables incremental BVH optimization.
    ///
    /// Default for this rollback fork: incremental optimization rewrites ~5% of
    /// the tree every frame regardless of movement, which is pure per-tick churn
    /// the [`StepJournal`] would have to capture and revert on every dirty tick.
    /// Insert-time rotations keep the tree in good enough shape; a full
    /// [`Bvh::rebuild`] at load time is the escalation if broad-phase queries
    /// ever profile hot.
    #[default]
    None,
}

const ENABLE_TREE_VALIDITY_CHECK: bool = false;

impl BroadPhaseBvh {
    const CHANGE_DETECTION_ENABLED: bool = true;
    const CHANGE_DETECTION_FACTOR: Real = OrderedFloat(1.0e-2);

    /// Initializes a new empty broad-phase.
    pub fn new() -> Self {
        Self::default()
    }

    /// SplitMix64 finalizer — a fixed, dependency-free 64-bit mixer used by
    /// the pairs multiset hash.
    #[inline]
    fn mix64(mut z: u64) -> u64 {
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Digest of one `pairs` entry. Commutative-combining callers
    /// (wrapping-add/sub into [`Self::pairs_hash`]) rely on this being a good
    /// mixer — adding raw values would let (a+1, b) collide with (a, b+1).
    #[inline]
    fn pair_entry_digest(k: &(ColliderHandle, ColliderHandle), timestamp: u32) -> u64 {
        let (i0, g0) = k.0.into_raw_parts();
        let (i1, g1) = k.1.into_raw_parts();
        let a = Self::mix64(((i0 as u64) << 32) | g0 as u64);
        let b = Self::mix64(((i1 as u64) << 32) | g1 as u64);
        Self::mix64(a ^ b.rotate_left(17) ^ (((timestamp as u64) << 1) | 1))
    }

    /// From-scratch recompute of [`Self::pairs_hash`] — the verification twin
    /// of the incremental cache. Iteration order is irrelevant (wrapping-add
    /// is commutative), so this is deterministic for any map order.
    pub(crate) fn recompute_pairs_hash(&self) -> u64 {
        self.pairs
            .iter()
            .fold(0u64, |acc, (k, ts)| acc.wrapping_add(Self::pair_entry_digest(k, *ts)))
    }

    /// Insert-or-refresh one `pairs` entry, keeping `pairs_hash` in lockstep.
    /// Returns true iff the entry was newly inserted. Free-standing over the
    /// two fields (not `&mut self`) so the BVTT pair collector — a closure
    /// that disjointly captures `pairs`/`pairs_hash` while the tree is
    /// borrowed by the traversal — can use the same single implementation.
    /// The retain-based GC in [`Self::update`] is the only other `pairs`
    /// mutation site; it subtracts [`Self::pair_entry_digest`] per removal.
    #[inline]
    fn pairs_insert_or_refresh(
        pairs: &mut HashMap<(ColliderHandle, ColliderHandle), u32>,
        pairs_hash: &mut u64,
        key: (ColliderHandle, ColliderHandle),
        timestamp: u32,
    ) -> bool {
        match pairs.entry(key) {
            Entry::Occupied(e) => {
                let old = *e.get();
                if old != timestamp {
                    *pairs_hash = pairs_hash
                        .wrapping_sub(Self::pair_entry_digest(&key, old))
                        .wrapping_add(Self::pair_entry_digest(&key, timestamp));
                    *e.into_mut() = timestamp;
                }
                false
            }
            Entry::Vacant(e) => {
                *pairs_hash = pairs_hash.wrapping_add(Self::pair_entry_digest(&key, timestamp));
                e.insert(timestamp);
                true
            }
        }
    }

    /// Initializes a new empty broad-phase with the specified strategy for incremental
    /// BVH optimization.
    pub fn with_optimization_strategy(optimization_strategy: BvhOptimizationStrategy) -> Self {
        Self {
            optimization_strategy,
            ..Default::default()
        }
    }

    /// Updates the broad-phase.
    ///
    /// The results are output through the `events` struct. The broad-phase algorithm is only
    /// required to generate new events (i.e. no need to re-send an `AddPair` event if it was already
    /// sent previously and no `RemovePair` happened since then). Sending redundant events is allowed
    /// but can result in a slight computational overhead.
    ///
    /// The `colliders` set is mutable only to provide access to
    /// [`collider.set_internal_broad_phase_proxy_index`]. Other properties of the collider should
    /// **not** be modified during the broad-phase update.
    ///
    /// # Parameters
    /// - `params`: the integration parameters governing the simulation.
    /// - `colliders`: the set of colliders. Change detection with `collider.needs_broad_phase_update()`
    ///   can be relied on at this stage.
    /// - `modified_colliders`: colliders that are know to be modified since the last update.
    /// - `removed_colliders`: colliders that got removed since the last update. Any associated data
    ///   in the broad-phase should be removed by this call to `update`.
    /// - `events`: the broad-phase’s output. They indicate what collision pairs need to be created
    ///   and what pairs need to be removed. It is OK to create pairs for colliders that don’t
    ///   actually collide (though this can increase computational overhead in the narrow-phase)
    ///   but it is important not to indicate removal of a collision pair if the underlying colliders
    ///   are still touching or closer than `prediction_distance`.
    pub fn update(
        &mut self,
        params: &IntegrationParameters,
        colliders: &ColliderSet,
        bodies: &RigidBodySet,
        modified_colliders: &[ColliderHandle],
        removed_colliders: &[ColliderHandle],
        events: &mut Vec<BroadPhasePairEvent>,
        journal: &mut Option<&mut StepJournal>,
    ) {
        let change_detection_skin = if Self::CHANGE_DETECTION_ENABLED {
            Self::CHANGE_DETECTION_FACTOR * params.length_unit
        } else {
            Real::from(0.0)
        };

        // The pre-scan (read-only) computes each modified collider's AABB once and
        // asks whether `insert_or_update_partially` would actually write anything.
        // It MUST observe post-removal leaf state: when collider X (raw index i) is
        // removed and a replacement Y reuses slot i in the same tick, comparing Y's
        // AABB against X's still-present, margin-inflated leaf would wrongly skip
        // Y's insertion. So:
        //   - removals present → the tick is unconditionally dirty: capture, bump,
        //     apply removals FIRST, then pre-scan the (post-removal) tree;
        //   - no removals → pre-scan first; if nothing would be written AND no
        //     leaves were changed via `set_aabb` since the last update
        //     (`pending_leaf_changes`), this is a clean tick that bails before
        //     any tree/pairs/frame_index change.
        let updates;
        if !removed_colliders.is_empty() {
            // Capture the pre-mutation state BEFORE the first tree/pairs mutation
            // (removals included), once per tick.
            if let Some(j) = journal.as_deref_mut() {
                if j.broad.is_none() {
                    j.broad = Some(Box::new(self.journal_save()));
                }
            }
            self.frame_index = self.frame_index.overflowing_add(1).0;

            // Removals must be handled first, in case another collider in
            // `modified_colliders` shares the same index.
            for handle in removed_colliders {
                self.tree.remove(handle.into_raw_parts().0);
            }

            updates = self.scan_updates(params, colliders, bodies, modified_colliders, change_detection_skin);
        } else {
            updates = self.scan_updates(params, colliders, bodies, modified_colliders, change_detection_skin);
            if updates.is_empty() && !self.pending_leaf_changes {
                // Clean tick: no node changed (neither here nor via `set_aabb`
                // since the last update), so the stale-pair GC below would be
                // a no-op (its removal condition requires a changed node) and the
                // BVTT traversal would re-find the same pairs. Skipping the whole
                // update — including the frame_index bump — is therefore exact and
                // leaves the hashed broad-phase state bit-identical.
                return;
            }
            if let Some(j) = journal.as_deref_mut() {
                if j.broad.is_none() {
                    j.broad = Some(Box::new(self.journal_save()));
                }
            }
            self.frame_index = self.frame_index.overflowing_add(1).0;
        }
        // The traversal + pair GC below will account for every leaf changed via
        // `set_aabb` since the last update.
        self.pending_leaf_changes = false;

        let first_pass = self.tree.is_empty();

        for (key, aabb) in updates {
            self.tree
                .insert_or_update_partially(aabb, key, change_detection_skin);
        }

        if ENABLE_TREE_VALIDITY_CHECK {
            if first_pass {
                self.tree.assert_well_formed();
            }

            self.tree.assert_well_formed_topology_only();
        }

        // let t0 = std::time::Instant::now();
        match self.optimization_strategy {
            BvhOptimizationStrategy::SubtreeOptimizer => {
                self.tree.optimize_incremental(&mut self.workspace);
            }
            BvhOptimizationStrategy::None => {}
        };
        // println!(
        //     "Incremental optimization: {}",
        //     t0.elapsed().as_secs_f32() * 1000.0
        // );

        // NOTE: we run refit after optimization so we can skip updating internal nodes during
        //       optimization, and so we can reorder the tree in memory (in depth-first order)
        //       to make it more cache friendly after the rebuild shuffling everything around.
        // let t0 = std::time::Instant::now();
        self.tree.refit(&mut self.workspace);

        if ENABLE_TREE_VALIDITY_CHECK {
            self.tree.assert_well_formed();
        }

        // println!("Refit: {}", t0.elapsed().as_secs_f32() * 1000.0);
        // println!(
        //     "leaf count: {}/{} (changed: {})",
        //     self.tree.leaf_count(),
        //     self.tree.reachable_leaf_count(0),
        //     self.tree.changed_leaf_count(0),
        // );
        // self.tree.assert_is_depth_first();
        // self.tree.assert_well_formed();
        // println!(
        //     "Is well formed. Tree height: {}",
        //     self.tree.subtree_height(0),
        // );
        // // println!("Tree quality: {}", self.tree.quality_metric());

        let mut pairs_collector = |co1: u32, co2: u32| {
            assert_ne!(co1, co2);

            let Some((_, mut handle1)) = colliders.get_unknown_gen(co1) else {
                return;
            };
            let Some((_, mut handle2)) = colliders.get_unknown_gen(co2) else {
                return;
            };

            if co1 > co2 {
                std::mem::swap(&mut handle1, &mut handle2);
            }

            if Self::pairs_insert_or_refresh(
                &mut self.pairs,
                &mut self.pairs_hash,
                (handle1, handle2),
                self.frame_index,
            ) {
                events.push(BroadPhasePairEvent::AddPair(ColliderPair::new(
                    handle1, handle2,
                )));
            }
        };

        // let t0 = std::time::Instant::now();
        self.tree
            .traverse_bvtt_single_tree::<{ Self::CHANGE_DETECTION_ENABLED }>(
                &mut self.workspace,
                &mut pairs_collector,
            );
        // println!("Detection: {}", t0.elapsed().as_secs_f32() * 1000.0);
        // println!(">>>>>> Num events: {}", events.iter().len());

        // Find outdated entries.
        // TODO PERF:
        // Currently, the narrow-phase isn’t capable of removing its own outdated
        // collision pairs. So we need to run a pass here to find aabbs that are
        // no longer overlapping. This, and the pair deduplication happening in
        // the `pairs_collector` is expensive and should be done more efficiently
        // by the narrow-phase itself (or islands) once we rework it.
        //
        // let t0 = std::time::Instant::now();
        // HashMap::retain removes entries without going through
        // pairs_insert_or_refresh, so accumulate the removed entries' digests
        // and subtract them from the multiset cache after the pass. Every
        // `false` path below must be counted exactly once.
        let mut removed_digest: u64 = 0;
        self.pairs.retain(|(h0, h1), timestamp| {
            let keep = if *timestamp != self.frame_index {
                if !colliders.contains(*h0) || !colliders.contains(*h1) {
                    // At least one of the colliders no longer exist, don’t retain the pair.
                    false
                } else if let (Some(node0), Some(node1)) = (
                    self.tree.leaf_node(h0.into_raw_parts().0),
                    self.tree.leaf_node(h1.into_raw_parts().0),
                ) {
                    if (!Self::CHANGE_DETECTION_ENABLED
                        || node0.is_changed()
                        || node1.is_changed())
                        && !node0.intersects(node1)
                    {
                        events.push(BroadPhasePairEvent::DeletePair(ColliderPair::new(*h0, *h1)));
                        false
                    } else {
                        true
                    }
                } else {
                    // A leaf no longer exists in the tree.
                    false
                }
            } else {
                // If the timestamps match, we already saw this pair during traversal.
                // There can be rare occurrences where the timestamp will be equal
                // even though we didn’t see the pair during traversal. This happens
                // if the frame index overflowed. But this is fine, we’ll catch it
                // in another frame.
                true
            };
            if !keep {
                removed_digest = removed_digest
                    .wrapping_add(Self::pair_entry_digest(&(*h0, *h1), *timestamp));
            }
            keep
        });
        self.pairs_hash = self.pairs_hash.wrapping_sub(removed_digest);

        // println!(
        //     "Post-filtering: {} (added pairs: {}, removed pairs: {})",
        //     t0.elapsed().as_secs_f32() * 1000.0,
        //     added_pairs,
        //     removed_pairs
        // );
    }

    /// Read-only pre-scan for [`Self::update`]: compute each modified collider's
    /// AABB once and keep only those for which `insert_or_update_partially`
    /// would actually write ([`Bvh::leaf_needs_update`]). Never mutates the tree,
    /// and allocates nothing when no collider needs a write.
    fn scan_updates(
        &self,
        params: &IntegrationParameters,
        colliders: &ColliderSet,
        bodies: &RigidBodySet,
        modified_colliders: &[ColliderHandle],
        change_detection_skin: Real,
    ) -> Vec<(u32, Aabb)> {
        let mut updates: Vec<(u32, Aabb)> = Vec::new();
        for modified in modified_colliders {
            if let Some(collider) = colliders.get(*modified) {
                if !collider.is_enabled() || !collider.changes.needs_broad_phase_update() {
                    continue;
                }
                let aabb = collider.compute_broad_phase_aabb(params, bodies);
                let key = modified.into_raw_parts().0;
                if self
                    .tree
                    .leaf_needs_update(&aabb, key, change_detection_skin)
                {
                    updates.push((key, aabb));
                }
            }
        }
        updates
    }

    /// Sets the AABB associated to the given collider.
    ///
    /// The AABB change will be immediately applied and propagated through the underlying BVH.
    /// Change detection will automatically take it into account during the next broad-phase update.
    pub fn set_aabb(&mut self, params: &IntegrationParameters, handle: ColliderHandle, aabb: Aabb) {
        let change_detection_skin = if Self::CHANGE_DETECTION_ENABLED {
            Self::CHANGE_DETECTION_FACTOR * params.length_unit
        } else {
            Real::from(0.0)
        };
        self.tree.insert_with_change_detection(
            aabb,
            handle.into_raw_parts().0,
            change_detection_skin,
        );
        self.pending_leaf_changes = true;
    }

    /// Journaled twin of [`Self::set_aabb`] used by the last-substep broad-phase
    /// refresh. Captures the pre-mutation broad-phase state into `journal` before
    /// the first write, and skips the write entirely when the AABB change would be
    /// a no-op (keeping clean ticks clean; `insert_with_change_detection` would
    /// otherwise rewrite even equal bounds).
    pub fn set_aabb_journaled(
        &mut self,
        params: &IntegrationParameters,
        handle: ColliderHandle,
        aabb: Aabb,
        journal: &mut Option<&mut StepJournal>,
    ) {
        let change_detection_skin = if Self::CHANGE_DETECTION_ENABLED {
            Self::CHANGE_DETECTION_FACTOR * params.length_unit
        } else {
            Real::from(0.0)
        };
        let key = handle.into_raw_parts().0;
        if !self.tree.leaf_needs_update(&aabb, key, change_detection_skin) {
            return;
        }
        if let Some(j) = journal.as_deref_mut() {
            if j.broad.is_none() {
                j.broad = Some(Box::new(self.journal_save()));
            }
        }
        self.tree
            .insert_with_change_detection(aabb, key, change_detection_skin);
        self.pending_leaf_changes = true;
    }

    /// Snapshots the hashed broad-phase state (tree + pairs + frame index +
    /// pending-leaf flag) for the [`StepJournal`]. The workspace is unhashed
    /// scratch and is not captured.
    pub fn journal_save(&self) -> BroadSaved {
        debug_assert_eq!(
            self.pairs_hash,
            self.recompute_pairs_hash(),
            "pairs_hash cache drifted from map content"
        );
        BroadSaved {
            tree: self.tree.clone(),
            pairs: self.pairs.clone(),
            pairs_hash: self.pairs_hash,
            frame_index: self.frame_index,
            pending_leaf_changes: self.pending_leaf_changes,
        }
    }

    /// Restores a [`BroadSaved`] snapshot, reverting the broad-phase to its
    /// pre-tick state. The workspace (unhashed serde-skipped scratch) is
    /// deliberately left untouched.
    pub fn journal_restore(&mut self, s: BroadSaved) {
        self.tree = s.tree;
        self.pairs = s.pairs;
        self.pairs_hash = s.pairs_hash;
        self.frame_index = s.frame_index;
        self.pending_leaf_changes = s.pending_leaf_changes;
    }

    /// Live-side twin of [`BroadSaved::digest`], for step-journal Delta footprint
    /// digests. Hashes exactly the fields [`Self::journal_restore`] writes back,
    /// in the same order, so after a restore this equals [`BroadSaved::digest`] of
    /// the snapshot it came from. Unlike the full [`std::hash::Hash`] impl,
    /// `optimization_strategy` is deliberately EXCLUDED: it is not journal-saved
    /// or restored (a step never mutates it), so the pre-side [`BroadSaved`] has no
    /// copy of it and including it would break the pre/post parity. Keep the field
    /// lists of `journal_save`, `journal_restore`, [`BroadSaved::digest`] and this
    /// fn in lockstep.
    pub(crate) fn journal_digest<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;
        self.tree.hash(state);
        // Recomputed from the live map — NOT read from the cache — so this
        // side independently verifies both the restore and the incremental
        // cache maintenance (cf. BroadSaved::digest, which hashes the O(1)
        // captured value).
        state.write_u64(self.recompute_pairs_hash());
        self.frame_index.hash(state);
        self.pending_leaf_changes.hash(state);
    }
}

#[cfg(test)]
mod pairs_hash_tests {
    use super::*;

    fn handle(i: u32) -> ColliderHandle {
        ColliderHandle::from_raw_parts(i, 0)
    }

    #[test]
    fn cached_pairs_hash_matches_recompute_after_mutations() {
        let mut bp = BroadPhaseBvh::new();
        assert_eq!(bp.pairs_hash, bp.recompute_pairs_hash(), "empty");

        // The three mutation shapes the collector/GC perform, via the shared
        // helper. Vacant inserts:
        assert!(BroadPhaseBvh::pairs_insert_or_refresh(
            &mut bp.pairs, &mut bp.pairs_hash, (handle(1), handle(2)), 7,
        ));
        assert!(BroadPhaseBvh::pairs_insert_or_refresh(
            &mut bp.pairs, &mut bp.pairs_hash, (handle(1), handle(3)), 7,
        ));
        assert_eq!(bp.pairs_hash, bp.recompute_pairs_hash(), "inserts");

        // Occupied refresh (timestamp change):
        assert!(!BroadPhaseBvh::pairs_insert_or_refresh(
            &mut bp.pairs, &mut bp.pairs_hash, (handle(1), handle(2)), 8,
        ));
        assert_eq!(bp.pairs_hash, bp.recompute_pairs_hash(), "refresh");

        // Removal (retain-style: subtract the entry digest):
        let removed = BroadPhaseBvh::pair_entry_digest(&(handle(1), handle(3)), 7);
        bp.pairs.remove(&(handle(1), handle(3)));
        bp.pairs_hash = bp.pairs_hash.wrapping_sub(removed);
        assert_eq!(bp.pairs_hash, bp.recompute_pairs_hash(), "removal");

        // Order independence: same content reached via a different history.
        let mut bp2 = BroadPhaseBvh::new();
        BroadPhaseBvh::pairs_insert_or_refresh(
            &mut bp2.pairs, &mut bp2.pairs_hash, (handle(1), handle(2)), 3,
        );
        BroadPhaseBvh::pairs_insert_or_refresh(
            &mut bp2.pairs, &mut bp2.pairs_hash, (handle(1), handle(2)), 8,
        );
        assert_eq!(bp.pairs_hash, bp2.pairs_hash, "content-equal maps hash equal");
    }
}
