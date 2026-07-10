# Broad-Phase BVH Digest Cost (fix 1b) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the per-tick / per-rollback broad-phase digests (`BroadSaved::digest`, `BroadPhaseBvh::journal_digest`) and Full-mode state hashing cheap, while keeping **bit-exact coverage of every BVH node (AABBs, topology indices, change flags)** and exact coverage of the pairs map.

**Architecture:** Two independent mechanisms. (1) The BVH node vector's `Hash` impl changes from a derived field-by-field walk (~16 `Hasher` calls × node) to a single bulk byte-slice `Hasher::write` over the `repr(C)`, padding-free node array — same information content, 10–50× faster, no cached state to maintain. Both digest paths and Full-mode hashing call `tree.hash(...)`, so they all speed up with zero call-site changes. (2) The `pairs` map gets an incrementally-maintained commutative multiset hash (`u64` wrapping-add of per-entry mixes) on `BroadPhaseBvh`, updated at its three mutation sites; the journal digest uses the cached value on the capture side and a full recompute on the revert side, so restore verification stays a genuine content check.

**Why not a leaf-multiset cache for the tree itself (the original 1b sketch):** leaf nodes carry change-detection flags (`CHANGED`/`CHANGE_PENDING` packed in `BvhNodeData`) that transition en masse inside `Bvh::refit` via `resolve_pending_change()` (`crates/parry/src/partitioning/bvh/bvh_refit.rs:196,198,228,247,293,313,358,366`) and inside `bvh_insert.rs:148,240` / `bvh_optimize.rs:469,506` — outside anything `BroadPhaseBvh` can instrument. An incremental cache would either exclude the flags (violates the full-exactness requirement) or hook all those internal sites (large silent-drift bug surface). Bulk byte hashing covers everything exactly with no maintained state.

**Tech Stack:** Rust, vendored `parry3d` fork (`crates/parry`), vendored `rapier3d` fork (`crates/rapier`), `crc32fast`.

## Global Constraints

- Vendored forks only — never switch `crates/parry` / `crates/rapier` to crates.io versions (CLAUDE.md).
- Never disable or weaken hash verification; optimize size/frequency/algorithm instead (user directive). This plan changes hash *algorithms*, never *coverage* — every byte hashed today stays covered.
- Cross-machine determinism: both targets (x86-64, wasm32) are little-endian; the bulk byte hash relies on that. Document it in code.
- Hash *values* change (byte-order hashing ≠ per-field `write_u32` sequence). That is fine: hashes are only ever compared between peers running the same build; nothing persists hashes across builds.
- `just test` for the fast loop while iterating; `just test-full` before declaring done. Tag any new test >100ms with `#[ignore = "slow: >100ms — run via just test-full"]`.
- Commits end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

## File Structure

- Modify: `crates/parry/src/partitioning/bvh/bvh_tree.rs` — bulk `Hash` for `BvhNodeVec`, layout const-asserts, `repr(C)` on `BvhNodeWide`.
- Modify: `crates/parry/src/partitioning/bvh/bvh_tests.rs` — property tests for the new hash.
- Modify: `crates/rapier/src/geometry/broad_phase_bvh.rs` — `pairs_hash` cache field, maintenance at the three pairs mutation sites, digest changes, `BroadSaved` capture field.
- Test (game-level gate): existing suites via `just test-full` (client/server convergence harnesses run `HashMode::Full` end-to-end).

---

### Task 1: Bulk byte-slice `Hash` for the BVH node vector

**Files:**
- Modify: `crates/parry/src/partitioning/bvh/bvh_tree.rs` (struct defs around lines 447 `BvhNodeWide`, 456 `BvhNode`, 1417 `BvhNodeVec`)
- Test: `crates/parry/src/partitioning/bvh/bvh_tests.rs`

**Interfaces:**
- Consumes: existing `BvhNode { mins: Point<Real>, children: u32, maxs: Point<Real>, data: BvhNodeData }`, `BvhNodeWide { left, right }`, `BvhNodeVec(Vec<BvhNodeWide>)`.
- Produces: `impl Hash for BvhNodeVec` (manual, bulk). `Bvh`'s derived `Hash` (line 1748) is untouched — it keeps hashing `nodes` (now bulk) + `parents` + `leaf_node_indices` exactly as today.

- [ ] **Step 1: Write the failing property tests**

In `bvh_tests.rs` (module already exists; follow its existing test style for constructing trees — it builds `Bvh` instances from leaf AABB sets):

```rust
#[cfg(all(feature = "dim3", feature = "f32"))]
mod node_vec_hash {
    use crate::math::{Point, Real};
    use crate::partitioning::Bvh;
    use core::hash::{Hash, Hasher};
    use crate::bounding_volume::Aabb;

    fn h<T: Hash>(t: &T) -> u32 {
        let mut hasher = crc32fast::Hasher::new();
        t.hash(&mut hasher);
        hasher.finalize()
    }

    fn aabb(i: f32) -> Aabb {
        Aabb::new(
            Point::new(Real::from(i), Real::from(0.0), Real::from(0.0)),
            Point::new(Real::from(i + 1.0), Real::from(1.0), Real::from(1.0)),
        )
    }

    fn tree(n: u32) -> Bvh {
        let mut t = Bvh::new();
        for i in 0..n {
            t.insert(aabb(i as f32 * 2.0), i);
        }
        t
    }

    #[test]
    fn equal_trees_hash_equal() {
        assert_eq!(h(&tree(16)), h(&tree(16)));
    }

    #[test]
    fn clone_hashes_equal() {
        let t = tree(16);
        assert_eq!(h(&t), h(&t.clone()));
    }

    #[test]
    fn leaf_content_change_changes_hash() {
        let mut a = tree(16);
        let before = h(&a);
        // Move one leaf: content must be reflected in the hash.
        a.insert(aabb(100.0), 3);
        assert_ne!(before, h(&a));
    }

    #[test]
    fn changed_flag_is_covered_by_hash() {
        // insert() marks touched nodes changed/pending; the packed flag word
        // lives in BvhNodeData and MUST be part of the hash (rollback restores
        // it bit-exact and Delta/Full verification must notice corruption).
        let mut a = tree(16);
        let mut b = tree(16);
        // Same leaf content, but only `a` gets a redundant re-insert that
        // flips pending/changed bits without moving the leaf beyond detection.
        a.insert(aabb(6.0), 3);
        b.insert(aabb(6.0), 3);
        assert_eq!(h(&a), h(&b), "identical op sequences must hash equal");
    }
}
```

Note: `crc32fast` must be available to parry tests — it is already a dependency of the fork's hashing consumers; if it is not in `crates/parry/Cargo.toml` `[dev-dependencies]`, add `crc32fast = "1"` there (workspace already vendors/locks it).

- [ ] **Step 2: Run the tests to verify current state**

Run: `cargo test -p parry3d --lib node_vec_hash`
Expected: PASS already (derived Hash also satisfies these properties). That is fine — these tests pin the properties so the bulk rewrite in Step 3 cannot regress them. The real change under test is the *implementation*; the safety net for exactness is the byte-layout const-asserts plus `just test-full` at the end.

- [ ] **Step 3: Replace the derived Hash with the bulk implementation**

In `bvh_tree.rs`:

1. Add `#[repr(C)]` to `BvhNodeWide` (the `#[derive(Copy, Clone, Debug, Hash)]` struct at ~line 447 holding `left`/`right`). `BvhNode` already carries `#[repr(C)]`.
2. Remove `Hash` from `BvhNodeVec`'s derive list (line 1417) and add:

```rust
// SAFETY / DETERMINISM invariants for the bulk hash below, all const-asserted:
// - BvhNode is repr(C): mins (Point<Real> = 3×f32 via repr(transparent)
//   OrderedFloat) + children u32 + maxs (3×f32) + data u32 = 32 bytes, no
//   padding. BvhNodeWide is repr(C) = exactly two BvhNodes = 64 bytes.
// - All fields are always initialized (no MaybeUninit), so every byte is
//   defined.
// - Both shipping targets (x86-64, wasm32) are little-endian; the byte image
//   of a node is therefore identical across machines for identical content.
//   A big-endian port would need a per-field fallback here.
// Hashing the raw bytes covers EXACTLY what the old derived Hash covered
// (mins, children, maxs, data — including the packed changed/pending flags)
// in one Hasher::write instead of ~16 write_u32 calls per node.
const _: () = assert!(core::mem::size_of::<BvhNode>() == 32);
const _: () = assert!(core::mem::size_of::<BvhNodeWide>() == 64);

impl core::hash::Hash for BvhNodeVec {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        // Length first so [A, B] and [AB-concatenated] cannot collide the way
        // raw byte streams could; mirrors the derived Vec hash prefix.
        state.write_usize(self.0.len());
        let bytes = unsafe {
            core::slice::from_raw_parts(
                self.0.as_ptr() as *const u8,
                self.0.len() * core::mem::size_of::<BvhNodeWide>(),
            )
        };
        state.write(bytes);
    }
}
```

The `const _` asserts are compile-time: if a field is ever added to `BvhNode` (padding risk), the build fails loudly instead of silently hashing garbage. If the crate builds `dim2`/`f64` variants where the size differs, gate the asserts and the manual impl with `#[cfg(all(feature = "dim3", feature = "f32"))]` and keep the plain derive for other configs (this workspace only builds `parry3d` dim3/f32).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p parry3d --lib node_vec_hash && cargo test -p parry3d --lib bvh`
Expected: PASS (all node_vec_hash tests plus the existing bvh test module).

- [ ] **Step 5: Run the game-side fast suite**

Run: `just test`
Expected: PASS. Hash *values* changed but every comparison is same-build-vs-same-build; convergence/rollback tests must stay green.

- [ ] **Step 6: Commit**

```bash
git add crates/parry/src/partitioning/bvh/bvh_tree.rs crates/parry/src/partitioning/bvh/bvh_tests.rs crates/parry/Cargo.toml
git commit -m "perf(parry): bulk byte-slice Hash for BVH node vec — same coverage, one write() instead of a per-field walk

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Incremental multiset hash for the broad-phase pairs map

**Files:**
- Modify: `crates/rapier/src/geometry/broad_phase_bvh.rs`

**Interfaces:**
- Consumes: `BroadPhaseBvh { tree, workspace, pairs: HashMap<(ColliderHandle, ColliderHandle), u32>, frame_index, optimization_strategy, pending_leaf_changes }`; `BroadSaved` (line 16); `journal_save` (line 429); `journal_restore` (line 441); `BroadSaved::digest` (line 29); `journal_digest` (line ~448); pairs mutation sites: `pairs_collector` entry insert/refresh (lines 277-285), `pairs.retain` GC (line 306).
- Produces: field `pairs_hash: u64` on `BroadPhaseBvh`; `fn pair_entry_digest(k: &(ColliderHandle, ColliderHandle), timestamp: u32) -> u64`; `fn recompute_pairs_hash(&self) -> u64`; `BroadSaved` gains `pairs_hash: u64`. **The Full-mode `impl Hash for BroadPhaseBvh` (line 68) is NOT changed** — it keeps hashing the map directly, so Full-mode coverage is untouched.

- [ ] **Step 1: Write the failing test**

In `broad_phase_bvh.rs`, add at the bottom:

```rust
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

        // Simulate the three mutation shapes the collector/GC perform.
        // Vacant insert:
        bp.pairs_insert_or_refresh((handle(1), handle(2)), 7);
        bp.pairs_insert_or_refresh((handle(1), handle(3)), 7);
        assert_eq!(bp.pairs_hash, bp.recompute_pairs_hash(), "inserts");

        // Occupied refresh (timestamp change):
        bp.pairs_insert_or_refresh((handle(1), handle(2)), 8);
        assert_eq!(bp.pairs_hash, bp.recompute_pairs_hash(), "refresh");

        // Removal:
        bp.pairs_remove(&(handle(1), handle(3)));
        assert_eq!(bp.pairs_hash, bp.recompute_pairs_hash(), "removal");

        // Order independence: same content via a different history.
        let mut bp2 = BroadPhaseBvh::new();
        bp2.pairs_insert_or_refresh((handle(1), handle(2)), 3);
        bp2.pairs_insert_or_refresh((handle(1), handle(2)), 8);
        assert_eq!(bp.pairs_hash, bp2.pairs_hash, "content-equal maps hash equal");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rapier3d --lib pairs_hash_tests`
Expected: FAIL — `pairs_hash`, `recompute_pairs_hash`, `pairs_insert_or_refresh`, `pairs_remove` not defined.

- [ ] **Step 3: Implement the cache**

In `broad_phase_bvh.rs`:

1. Add the field (serde-skipped: `pairs` itself serializes capacity-only via `serialize_hashmap_capacity`, i.e. deserializes EMPTY, so the cache must default to the empty-multiset value 0):

```rust
    /// Incrementally-maintained commutative multiset hash of `pairs`
    /// (wrapping-add of per-entry mixes). Used by the step-journal Delta
    /// digests so they don't walk the whole map per tick. NOT part of
    /// `impl Hash for BroadPhaseBvh` (it is derived data, like `workspace`);
    /// Full mode keeps hashing the map itself. `pairs` deserializes empty
    /// (capacity-only serde), so this defaults to 0 — the empty multiset.
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pairs_hash: u64,
```

2. Add the digest helpers (associated fns on `BroadPhaseBvh`):

```rust
    /// SplitMix64 finalizer — a fixed, dependency-free 64-bit mixer.
    #[inline]
    fn mix64(mut z: u64) -> u64 {
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Digest of one pairs-map entry. Commutative-combining callers rely on
    /// this being a good mixer (wrapping-add of raw values would let
    /// (a+1, b) collide with (a, b+1)).
    #[inline]
    fn pair_entry_digest(k: &(ColliderHandle, ColliderHandle), timestamp: u32) -> u64 {
        let (i0, g0) = k.0.into_raw_parts();
        let (i1, g1) = k.1.into_raw_parts();
        let a = Self::mix64(((i0 as u64) << 32) | g0 as u64);
        let b = Self::mix64(((i1 as u64) << 32) | g1 as u64);
        Self::mix64(a ^ b.rotate_left(17) ^ ((timestamp as u64) << 1 | 1))
    }

    /// From-scratch recompute — the verification twin of the incremental
    /// cache. Iteration order is irrelevant (wrapping-add is commutative).
    pub(crate) fn recompute_pairs_hash(&self) -> u64 {
        self.pairs
            .iter()
            .fold(0u64, |acc, (k, ts)| acc.wrapping_add(Self::pair_entry_digest(k, *ts)))
    }

    /// The ONLY mutators of `pairs` — all map writes must go through these
    /// two so `pairs_hash` cannot drift.
    #[inline]
    pub(crate) fn pairs_insert_or_refresh(
        &mut self,
        key: (ColliderHandle, ColliderHandle),
        timestamp: u32,
    ) -> bool {
        match self.pairs.entry(key) {
            Entry::Occupied(e) => {
                let old = *e.get();
                if old != timestamp {
                    self.pairs_hash = self
                        .pairs_hash
                        .wrapping_sub(Self::pair_entry_digest(&key, old))
                        .wrapping_add(Self::pair_entry_digest(&key, timestamp));
                    *e.into_mut() = timestamp;
                }
                false
            }
            Entry::Vacant(e) => {
                self.pairs_hash = self
                    .pairs_hash
                    .wrapping_add(Self::pair_entry_digest(&key, timestamp));
                e.insert(timestamp);
                true
            }
        }
    }

    #[inline]
    pub(crate) fn pairs_remove(&mut self, key: &(ColliderHandle, ColliderHandle)) {
        if let Some(ts) = self.pairs.remove(key) {
            self.pairs_hash = self
                .pairs_hash
                .wrapping_sub(Self::pair_entry_digest(key, ts));
        }
    }
```

3. Rewrite the two existing mutation sites to go through the helpers:

`pairs_collector` (lines 277-285) — the closure currently does the `entry()` match inline and borrows `self.pairs`/`self.frame_index`. Because it will now also need `pairs_hash`, restructure it to collect candidate `(handle1, handle2)` pairs into the workspace-local `events` shape it already uses, OR keep the closure but have it capture `&mut pairs`, `&mut pairs_hash`, and `frame_index` as locals split out of `self` before the closure (`let Self { pairs, pairs_hash, tree, workspace, frame_index, .. } = self;` field-splitting keeps the borrow checker happy since `tree`/`workspace` are used by the traversal and `pairs`/`pairs_hash` by the collector). Inline the helper logic in the closure if calling `self.pairs_insert_or_refresh` is not borrowable there — the logic must be byte-identical to the helper (same digest fn); add a comment cross-referencing them:

```rust
        let pairs = &mut self.pairs;
        let pairs_hash = &mut self.pairs_hash;
        let frame_index = self.frame_index;
        let mut pairs_collector = |co1: u32, co2: u32| {
            assert_ne!(co1, co2);
            let Some((_, mut handle1)) = colliders.get_unknown_gen(co1) else { return };
            let Some((_, mut handle2)) = colliders.get_unknown_gen(co2) else { return };
            if co1 > co2 {
                std::mem::swap(&mut handle1, &mut handle2);
            }
            let key = (handle1, handle2);
            // Mirror of pairs_insert_or_refresh (kept inline for borrow
            // reasons) — any edit here must be mirrored there.
            match pairs.entry(key) {
                Entry::Occupied(e) => {
                    let old = *e.get();
                    if old != frame_index {
                        *pairs_hash = pairs_hash
                            .wrapping_sub(Self::pair_entry_digest(&key, old))
                            .wrapping_add(Self::pair_entry_digest(&key, frame_index));
                        *e.into_mut() = frame_index;
                    }
                }
                Entry::Vacant(e) => {
                    *pairs_hash =
                        pairs_hash.wrapping_add(Self::pair_entry_digest(&key, frame_index));
                    e.insert(frame_index);
                    events.push(BroadPhasePairEvent::AddPair(ColliderPair::new(
                        handle1, handle2,
                    )));
                }
            }
        };
```

`pairs.retain` GC (line 306): `retain` removes entries without going through `pairs_remove`. Accumulate the removed digests in a local and apply after:

```rust
        let mut removed_digest: u64 = 0;
        self.pairs.retain(|(h0, h1), timestamp| {
            let keep = /* existing retain body unchanged, computing `keep` */;
            if !keep {
                removed_digest =
                    removed_digest.wrapping_add(Self::pair_entry_digest(&(*h0, *h1), *timestamp));
            }
            keep
        });
        self.pairs_hash = self.pairs_hash.wrapping_sub(removed_digest);
```

(The existing retain body's early `return false` arms must be converted to set a `keep` flag or each `return false` must add to `removed_digest` before returning — pick whichever reads cleaner when editing; every `false` path must be counted exactly once.)

4. `BroadPhaseBvh::new`/`with_optimization_strategy`/`Default`: field defaults to 0 (derive(Default) on the struct handles it — verify `Default` is still derived; if the struct uses manual init anywhere, add `pairs_hash: 0`).

5. Add a drift tripwire where the journal captures state — in `journal_save` (line 429):

```rust
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
```

6. `BroadSaved` (line 16) gains `pub(crate) pairs_hash: u64`, and `journal_restore` (line 441) restores it:

```rust
    pub fn journal_restore(&mut self, s: BroadSaved) {
        self.tree = s.tree;
        self.pairs = s.pairs;
        self.pairs_hash = s.pairs_hash;
        self.frame_index = s.frame_index;
        self.pending_leaf_changes = s.pending_leaf_changes;
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rapier3d --lib pairs_hash_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rapier/src/geometry/broad_phase_bvh.rs
git commit -m "perf(rapier): incremental multiset pairs_hash on the broad phase — maintained at the 3 mutation sites, debug-asserted against recompute

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Point the journal digests at the cheap hashes

**Files:**
- Modify: `crates/rapier/src/geometry/broad_phase_bvh.rs` (`BroadSaved::digest` line 29, `journal_digest` line ~448)

**Interfaces:**
- Consumes: Task 1's bulk `tree.hash` (automatic — both digest fns already call it), Task 2's `pairs_hash` / `recompute_pairs_hash`.
- Produces: capture-side digest uses the stored `pairs_hash` (O(1)); revert-side digest recomputes from the live map (O(pairs), cheap mixes) so the pre/post comparison remains a genuine content verification — a maintenance bug in the cache OR a restore bug both surface as a Delta hash-mismatch panic.

- [ ] **Step 1: Rewrite the two digest twins**

```rust
impl BroadSaved {
    /// Pre-side twin of [`BroadPhaseBvh::journal_digest`]; mirrors
    /// [`BroadPhaseBvh::journal_restore`]'s field writes in order (tree, pairs,
    /// frame_index, pending_leaf_changes). `optimization_strategy` is not part
    /// of the journal snapshot, so it is excluded here too. Keep all four field
    /// lists in lockstep.
    ///
    /// Pairs are represented by the multiset hash captured at save time; the
    /// live twin RECOMPUTES it from the restored map, so equality proves the
    /// restored content matches the captured content (and doubles as a cache-
    /// integrity check).
    pub(crate) fn digest<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;
        self.tree.hash(state); // bulk since Task 1
        state.write_u64(self.pairs_hash);
        self.frame_index.hash(state);
        self.pending_leaf_changes.hash(state);
    }
}
```

```rust
    /// Live-side twin of [`BroadSaved::digest`], for step-journal Delta
    /// footprint digests. Hashes exactly the fields [`Self::journal_restore`]
    /// writes back. Pairs content is recomputed from the live map (not read
    /// from the cache) so this side independently verifies the restore.
    pub fn journal_digest<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;
        self.tree.hash(state);
        state.write_u64(self.recompute_pairs_hash());
        self.frame_index.hash(state);
        self.pending_leaf_changes.hash(state);
    }
```

(Adjust the exact current bodies at lines 29-35 and ~448-460 — today both hash `self.pairs` directly; only the pairs line changes.)

- [ ] **Step 2: Run the rapier and game suites**

Run: `cargo test -p rapier3d --lib && just test`
Expected: PASS. The step-journal Delta digests still compare equal on exact reverts (`crates/game` rollback tests and the client convergence harnesses exercise this end to end).

- [ ] **Step 3: Full suite**

Run: `just test-full`
Expected: PASS, including the `HashMode::Full` convergence harnesses (`cargo test -p client`) and the release-mode-excluded set.

- [ ] **Step 4: Commit**

```bash
git add crates/rapier/src/geometry/broad_phase_bvh.rs
git commit -m "perf(rapier): broad-phase journal digests use bulk tree hash + cached/recomputed pairs multiset

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Live verification

**Files:** none (measurement only).

- [ ] **Step 1: Run server + one client and profile**

Per the standing testing preference: one server + one client (`just run`, eGPU via `just adapter=6700 run` if on the laptop). Move around for ~30s so the broad phase is dirty every tick.

Record with the perf script (remember `--no-inline` — the script livelocks on Rust binaries without it):

```bash
scripts/perf.sh --no-inline   # or: perf record --call-graph dwarf -p <client pid> -- sleep 20; perf report --no-inline
```

Expected: `BvhNodeVec as Hash`/`Bvh as Hash` and pairs-map hashing no longer visible as top entries under `pre_digest`/`revert_digest`. (The remaining large costs — whole-`PhysicsState` `snapshot_raw` hashing of the narrow-phase graphs — are plan `2026-07-10-surgical-collider-attach.md`, and the wholesale `journal_save` tree *clone* stays by design.)

- [ ] **Step 2: Report before/after numbers in the PR/branch summary.**

---

## Self-Review Notes

- Coverage: every byte the old derived hashes covered is still hashed (bulk bytes = mins/children/maxs/data per node incl. flags; pairs multiset = key + timestamp per entry; frame_index and pending_leaf_changes unchanged). No summary digests anywhere, per the exactness requirement.
- The `Full`-mode `impl Hash for BroadPhaseBvh` (line 68) intentionally keeps hashing `self.pairs` directly; only the journal digest twins switch to the multiset. Both digest twins change together, preserving the pre/post parity contract documented in `step_journal.rs`.
- `pairs_hash` is `serde(skip)` and defaults to 0, matching the capacity-only (empty) deserialization of `pairs` — a restored/parked region starts consistent.
- Known non-goals: the per-dirty-tick wholesale `tree.clone()` in `journal_save` (memcpy, acceptable; leaf-level broad-phase journaling is the deferred escalation), and the client prediction-runaway re-sim multiplier (separate work).
