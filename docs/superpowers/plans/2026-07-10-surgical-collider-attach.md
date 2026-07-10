# Surgical Collider Attach (narrow-phase fix 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove `snapshot_raw()` (whole-`PhysicsState` clone + whole-`PhysicsState` Full-mode hash, including the narrow-phase interaction graphs) from `attach_voxels_collider_safe` and `attach_capsule_collider_safe`, replacing it with an exactly-invertible, footprint-digested tier-2 entry — the same treatment `build_kinematic_body_safe` already gives body inserts.

**Architecture:** Two layers. Fork layer: `ColliderSet` gets `alloc_state()`/`revert_insert()` mirroring `RigidBodySet`'s (the underlying `Arena` already has both primitives), and `RigidBodySet` gets two tiny accessors for the parent body's modified-list bookkeeping. Game layer: a shared `attach_collider_safe` helper does the insert inside a `data.physics.undo_scope_digest()` scope whose registered closure is a true LIFO inverse of everything `insert_with_parent` touches: the collider arena slot + alloc state, the collider modified-list entry, the parent body's value (colliders list, mass props, change flags — all inside the body clone), and the parent's modified-list membership. The Delta digest covers exactly that footprint; Full mode (tests/harnesses) still whole-field-hashes and will catch any missed state.

**Why this is safe to invert surgically** — `insert_with_parent` (`crates/rapier/src/geometry/collider_set.rs:213-253`) performs, in order:
1. `bodies.get_mut_internal_with_modification_tracking(parent)` — pushes parent onto `modified_bodies` **iff** its `MODIFIED` change-flag was clear (`modified_objects.rs:52-55`), and that push itself sets the flag.
2. `colliders.insert(coll)` into the arena (alloc state mutates).
3. `modified_colliders.push_unchecked(handle, coll)` — new collider always appended.
4. `parent.add_collider_internal(...)` — mutates the parent body's collider list + mass props (all inside the `RigidBody` value).

Every touched location is enumerable, and the global LIFO discipline of the rollback log guarantees that at undo time the two modified-lists' tails are exactly what this op left them as.

**Tech Stack:** Rust, vendored `rapier3d` fork (`crates/rapier`), `#[rollback]` macro machinery (`crates/macros`), `crates/game`.

## Global Constraints

- Never disable or weaken hash verification (user directive). This plan replaces a whole-state Full hash with an exact footprint digest — coverage of the mutation stays complete; Full mode in tests still verifies whole fields.
- Rollback correctness bar: `hash(before) == hash(after undo)`, bit-exact (CLAUDE.md). The client convergence harnesses run `HashMode::Full` and are the end-to-end gate.
- Vendored forks only; fork additions follow the existing `revert_insert`/`restore_raw` house style.
- `just test` while iterating, `just test-full` before declaring done; tag tests >100ms with `#[ignore = "slow: >100ms — run via just test-full"]`.
- Commits end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Scope exclusions (deliberate): `remove_entity_safe`'s `snapshot_raw` stays (structural remove cascades; rare), the broad-phase pair filter is NOT implemented (user decision), and the client prediction-runaway re-sim multiplier is separate work.

## File Structure

- Modify: `crates/rapier/src/geometry/collider_set.rs` — `alloc_state`, `revert_insert`.
- Modify: `crates/rapier/src/dynamics/rigid_body_set.rs` — `is_marked_modified`, `pop_modified`.
- Modify: `crates/game/src/state.rs` — `collider_attach_digest`, shared `attach_collider_safe`, rewrite of the two attach fns.
- Test: `crates/game/tests/hash_restore.rs` — fork-inverse guarantees (container level).
- Test: `crates/game/src/state.rs` tests module (follow the existing hash-mode test pattern around `state.rs:1110-1170`) — game-level `hash(before) == hash(after undo)`.

---

### Task 1: Fork — exact insert inverse on `ColliderSet`, modified-list accessors on `RigidBodySet`

**Files:**
- Modify: `crates/rapier/src/geometry/collider_set.rs`
- Modify: `crates/rapier/src/dynamics/rigid_body_set.rs`
- Test: `crates/game/tests/hash_restore.rs`

**Interfaces:**
- Consumes: `Arena::alloc_state(&self) -> (Option<u32>, u32)` and `Arena::revert_insert(i: Index, prev_free_head: Option<u32>, prev_items_len: u32)` (`crates/rapier/src/data/arena.rs:370,378`); `ModifiedObjects::pop()` (`modified_objects.rs:71`); `RigidBodyChanges::MODIFIED` flag semantics via `has_modified_flag` (`rigid_body_set.rs:38` area).
- Produces (exact signatures Task 2 calls):
  - `ColliderSet::alloc_state(&self) -> (Option<u32>, u32)`
  - `ColliderSet::revert_insert(&mut self, handle: ColliderHandle, prev_free_head: Option<u32>, prev_items_len: u32)`
  - `RigidBodySet::is_marked_modified(&self, handle: RigidBodyHandle) -> bool`
  - `RigidBodySet::pop_modified(&mut self, handle: RigidBodyHandle)`
  - (already existing, used by Task 2: `RigidBodySet::restore_raw(&mut self, handle, body)` at `rigid_body_set.rs:124`, `ColliderSet::iter/get/contains`.)

- [ ] **Step 1: Write the failing test**

Append to `crates/game/tests/hash_restore.rs` (helpers `h()` and the imports at the top of the file already exist; extend the `use rapier3d::prelude::...` line with `ColliderBuilder`, `Real`, `Vector` as needed):

```rust
#[test]
fn collider_insert_with_parent_reverts_exactly() {
    use rapier3d::prelude::{ColliderBuilder, RigidBodyBuilder};

    let mut bodies = RigidBodySet::new();
    let mut colliders = ColliderSet::new();

    // A parent body with some history so the arena free list is non-trivial.
    let parent = bodies.insert(RigidBodyBuilder::fixed().build());
    let doomed = colliders.insert(ColliderBuilder::ball(1.0.into()).build());
    let mut islands = IslandManager::new();
    let mut ij = ImpulseJointSet::new();
    let mut mj = MultibodyJointSet::new();
    colliders.remove(doomed, &mut islands, &mut bodies, false);

    // Case A: parent NOT yet marked modified at attach time.
    // Clear tracking state the same way the pipeline's end-of-step sweep does:
    // drain the modified/removed lists so flags and lists are clean.
    bodies.take_modified();   // if these drain helpers don't exist under these
    colliders.take_modified(); // names, use the actual fork API that clears
    colliders.take_removed();  // modified/removed tracking (see set_modified/
                               // take_removed in collider_set.rs:100-106).

    let before_bodies = h(&bodies);
    let before_colliders = h(&colliders);
    let was_modified = bodies.is_marked_modified(parent);
    let pre_parent = bodies.get(parent).unwrap().clone();
    let (free_head, len) = colliders.alloc_state();

    let ch = colliders.insert_with_parent(
        ColliderBuilder::capsule_y(0.5.into(), 0.25.into()).build(),
        parent,
        &mut bodies,
    );

    // Exact LIFO inverse of insert_with_parent, in reverse op order:
    colliders.revert_insert(ch, free_head, len); // pops modified_colliders + arena revert
    bodies.restore_raw(parent, pre_parent);      // colliders list, mprops, change flags
    if !was_modified {
        bodies.pop_modified(parent);             // undoes the tracking push
    }

    assert_eq!(before_bodies, h(&bodies), "parent body + modified list must restore");
    assert_eq!(before_colliders, h(&colliders), "collider slot + alloc + modified list must restore");
}
```

Note for the implementer: if `take_modified`/`take_removed` are `pub(crate)`, either widen them for tests behind the existing pattern or set up Case A by constructing fresh sets (a brand-new `RigidBodySet` after `insert` has the parent in `modified_bodies`, so to get an UNmodified parent simply pop it: the point of `was_modified` is that both cases restore exactly — add a second scenario in the same test exercising `was_modified == true` by attaching twice and reverting the second attach).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test hash_restore collider_insert_with_parent_reverts_exactly`
Expected: FAIL — `alloc_state`/`revert_insert`/`is_marked_modified`/`pop_modified` not found on the sets.

- [ ] **Step 3: Implement the fork methods**

`crates/rapier/src/geometry/collider_set.rs` — mirror `RigidBodySet`'s pair (`rigid_body_set.rs:179-201`) exactly, next to `restore_raw` (line 111):

```rust
    /// Allocator state to capture before `insert`/`insert_with_parent` for
    /// [`Self::revert_insert`].
    pub fn alloc_state(&self) -> (Option<u32>, u32) {
        self.colliders.alloc_state()
    }

    /// Exact inverse of the MOST RECENT collider insert that returned
    /// `handle`, given the state captured by [`Self::alloc_state`] just before
    /// it. LIFO contract: invalid if any other collider was inserted/removed/
    /// marked modified since. Does NOT touch the parent body — callers that
    /// used `insert_with_parent` must also restore the parent (its collider
    /// list and mass properties changed) and its modified-list membership.
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
```

(`ModifiedColliders` is the same `ModifiedObjects` type as the bodies' list; if it lacks `pop`, it already has it — `modified_objects.rs:71` — the bodies' `revert_insert` uses it.)

`crates/rapier/src/dynamics/rigid_body_set.rs` — next to `revert_insert` (line 188):

```rust
    /// Whether `handle`'s body currently carries the MODIFIED change-flag
    /// (i.e. is in the modified list). Capture this BEFORE an operation that
    /// goes through modification tracking, so its undo knows whether to
    /// [`Self::pop_modified`].
    pub fn is_marked_modified(&self, handle: RigidBodyHandle) -> bool {
        self.bodies
            .get(handle.0)
            .is_some_and(|rb| rb.changes.contains(RigidBodyChanges::MODIFIED))
    }

    /// Exact inverse of the modification-tracking push performed on `handle`
    /// by the MOST RECENT tracked mutation (LIFO contract: `handle` must be
    /// the list's tail). Clears the MODIFIED flag via the popped entry's
    /// restore path — callers restore the body VALUE separately via
    /// [`Self::restore_raw`], which rewrites the flags wholesale, so this only
    /// needs to fix the list.
    pub fn pop_modified(&mut self, handle: RigidBodyHandle) {
        let popped = self.modified_bodies.pop();
        assert_eq!(popped, Some(handle), "pop_modified: not the list tail");
    }
```

(Check the actual flag/namespace names when editing — `RigidBodyChanges::MODIFIED` is the flag `has_modified_flag`/`set_modified_flag` manipulate at `rigid_body_set.rs:38`. If the flag enum spells it differently, follow the source. Note `restore_raw` restores the body's `changes` field as part of the value, so ordering in the undo closure must be: `revert_insert` collider → `restore_raw` parent → `pop_modified` parent — the pop must come after the value restore only for clarity; the two don't interact since the pop no longer mutates the flag.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p game --test hash_restore`
Expected: PASS — the new test and all pre-existing container-inverse tests.

- [ ] **Step 5: Commit**

```bash
git add crates/rapier/src/geometry/collider_set.rs crates/rapier/src/dynamics/rigid_body_set.rs crates/game/tests/hash_restore.rs
git commit -m "feat(rapier): exact ColliderSet insert inverse + parent modified-list accessors

revert_insert mirrors RigidBodySet's (arena alloc revert + modified-list pop);
is_marked_modified/pop_modified let callers invert insert_with_parent's
tracking push on the parent body.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Game — footprint-digested `attach_collider_safe`, rewrite both attach fns

**Files:**
- Modify: `crates/game/src/state.rs` (fns at lines ~498-528, digest helpers near `body_slot_digest` at line 248)
- Test: `crates/game/src/state.rs` tests module (existing hash-mode tests live around lines 1110-1170; follow their `set_hash_mode` handling exactly — they manage the global mode carefully because it is process-wide)

**Interfaces:**
- Consumes (from Task 1): `ColliderSet::{alloc_state, revert_insert}`, `RigidBodySet::{is_marked_modified, pop_modified, restore_raw}`. From the macro: `Undo<PhysicsState>::undo_scope_digest()` → `UndoScopeDigest<PhysicsState>` with `wants_delta_digest()`, `raw_fields()` (used the same way in `physics.rs:19-45`), and `register(pre: u32, undo: impl FnOnce(&mut PhysicsState, &Sender<GameDataUpdate>) -> u32)`.
- Produces: `fn collider_attach_digest(alloc: (Option<u32>, u32), slot: Option<&Collider>, parent: Option<&RigidBody>, parent_was_modified: bool) -> u32`; `Rollback::attach_collider_safe(&mut self, e: EntityKey, body: RigidBodyHandle, collider: Collider)`. The public signatures of `attach_voxels_collider_safe` / `attach_capsule_collider_safe` are unchanged.

- [ ] **Step 1: Write the failing game-level test**

In the `state.rs` tests module (alongside the existing hash-mode tests; reuse their setup/mode-guard pattern — the suite there already flips `HashMode` safely):

```rust
#[test]
fn attach_collider_rolls_back_bit_exact() {
    // Full mode: whole-field hash(before) == hash(after undo) is enforced by
    // the rollback machinery itself; this test additionally asserts the
    // overall state hash restores, which catches state OUTSIDE the digested
    // footprint too.
    set_hash_mode(HashMode::Full);
    let mut r = Rollback::new(None);
    r.new_transaction();
    let e = r.ecs.create_entity_safe();
    let (key, body) = {
        // A fixed parent body, inserted through the existing surgical path.
        let mut rb = crate::rapier3d::prelude::RigidBodyBuilder::fixed().build();
        rb.user_data = e.data().as_ffi() as u128;
        let (prev_head, prev_len) = r.data.physics.bodies.alloc_state();
        let mut scope = r.data.physics.bodies.undo_scope_digest();
        let handle = scope.insert(rb);
        scope.register(
            body_slot_digest((prev_head, prev_len), None),
            move |bodies, _| {
                bodies.revert_insert(handle, prev_head, prev_len);
                body_slot_digest(bodies.alloc_state(), bodies.get(handle))
            },
        );
        (e, handle)
    };
    r.forget(); // setup transaction is not under test

    let before = crate::state_hash(&r);
    r.new_transaction();
    r.attach_capsule_collider_safe(key, body, 0.5, 0.25);
    assert_ne!(before, crate::state_hash(&r), "attach must change state");
    r.rollback();
    assert_eq!(before, crate::state_hash(&r), "attach must invert bit-exact");

    // Same again under Delta: the footprint digest path must also verify.
    set_hash_mode(HashMode::Delta);
    let before = crate::state_hash(&r);
    r.new_transaction();
    r.attach_capsule_collider_safe(key, body, 0.5, 0.25);
    r.rollback();
    assert_eq!(before, crate::state_hash(&r), "delta-mode attach must invert bit-exact");
    set_hash_mode(HashMode::Full);
}
```

Adapt construction details to the surrounding tests' idioms (how they build `Rollback`, forget setup, and guard the global hash mode — copy the existing pattern verbatim; if the module has a mode-restoring guard type, use it). The assertions are the contract; the plumbing should match the file.

- [ ] **Step 2: Run test to verify it fails or passes-by-snapshot**

Run: `cargo test -p game attach_collider_rolls_back_bit_exact`
Expected: PASS on the current code (whole-snapshot undo is also exact). That is intentional — this test is the regression gate that must STILL pass after Step 3 removes the snapshot. The Delta-mode leg is the new behavior under test: today it exercises the `undo()` Full fallback; after Step 3 it exercises the footprint digest.

- [ ] **Step 3: Implement the surgical attach**

In `crates/game/src/state.rs`, next to `body_slot_digest` (line 248):

```rust
/// crc32 of the exact footprint of one `insert_with_parent`: the collider
/// set's allocator state, the inserted collider's slot, the parent body's
/// value (its collider list, mass properties and change flags all live
/// inside it), and whether the parent was already on the modified list.
fn collider_attach_digest(
    alloc: (Option<u32>, u32),
    slot: Option<&rapier3d::prelude::Collider>,
    parent: Option<&rapier3d::prelude::RigidBody>,
    parent_was_modified: bool,
) -> u32 {
    use std::hash::Hash;
    let mut h = crc32fast::Hasher::new();
    alloc.hash(&mut h);
    slot.hash(&mut h);
    parent.hash(&mut h);
    parent_was_modified.hash(&mut h);
    h.finalize()
}
```

Replace the bodies of the two attach fns (lines ~498-528) with a shared helper:

```rust
    /// Attach `collider` to `body`, undo-safely, with a footprint-sized log
    /// entry (no whole-PhysicsState snapshot). The registered closure is the
    /// exact LIFO inverse of insert_with_parent: collider arena revert +
    /// modified-list pop, parent body value restore, and parent modified-list
    /// pop iff this op's tracking push added it. Delta digests exactly that
    /// footprint; Full mode still whole-field-hashes (tests/harnesses).
    fn attach_collider_safe(
        &mut self,
        body: RigidBodyHandle,
        collider: rapier3d::prelude::Collider,
    ) {
        let pre_parent = self
            .data
            .physics
            .bodies
            .get(body)
            .expect("attach_collider_safe: parent body missing")
            .clone();
        let parent_was_modified = self.data.physics.bodies.is_marked_modified(body);
        let (prev_head, prev_len) = self.data.physics.colliders.alloc_state();

        let mut scope = self.data.physics.undo_scope_digest();
        let pre = if scope.wants_delta_digest() {
            collider_attach_digest(
                (prev_head, prev_len),
                None,                    // slot vacant before the insert
                Some(&pre_parent),
                parent_was_modified,
            )
        } else {
            0
        };
        let handle = {
            let p = scope.raw_fields();
            p.colliders.insert_with_parent(collider, body, p.bodies)
        };
        scope.register(pre, move |phys, _| {
            let p = phys.raw_undo_parts();
            p.colliders.revert_insert(handle, prev_head, prev_len);
            p.bodies.restore_raw(body, pre_parent);
            if !parent_was_modified {
                p.bodies.pop_modified(body);
            }
            collider_attach_digest(
                p.colliders.alloc_state(),
                p.colliders.get(handle),
                p.bodies.get(body),
                p.bodies.is_marked_modified(body),
            )
        });
    }

    /// Attach a voxel-grid collider to an entity's body. One cell per key;
    /// for voxel size `s`, cell g spans [g*s, (g+1)*s) in body-local space —
    /// scale 1.0 is the render mesh's voxel grid, scale `BLOCK_VOXELS` is
    /// the block grid. Entity-generic. Undo is the exact footprint inverse
    /// (see attach_collider_safe) — no whole-PhysicsState snapshot.
    pub fn attach_voxels_collider_safe(
        &mut self,
        e: EntityKey,
        body: RigidBodyHandle,
        coords: &[Point<i32>],
        voxel_size: Real,
    ) {
        let collider = ColliderBuilder::voxels(Vector::repeat(voxel_size), coords)
            .user_data(e.data().as_ffi() as u128)
            .build();
        self.attach_collider_safe(body, collider);
    }

    /// Attach a capsule collider to an entity's body. Entity-generic: player
    /// today, NPCs later. Undo is the exact footprint inverse (see
    /// attach_collider_safe) — no whole-PhysicsState snapshot.
    pub fn attach_capsule_collider_safe(
        &mut self,
        e: EntityKey,
        body: RigidBodyHandle,
        half_height: f32,
        radius: f32,
    ) {
        let collider = ColliderBuilder::capsule_y(Real::from(half_height), Real::from(radius))
            .user_data(e.data().as_ffi() as u128)
            .build();
        self.attach_collider_safe(body, collider);
    }
```

Notes for the implementer:
- The `undo_scope_digest()` here is on `Undo<PhysicsState>` (`data.physics`), exactly like `PhysicsController::on_tick` (`crates/game/src/physics.rs:19-58`) — `raw_fields()` inside the scope and `raw_undo_parts()` inside the closure are the two mutation licenses.
- The closure's returned digest re-reads the same four footprint components from LIVE post-undo state; the parity rule is the one `component_slot_digest`/`body_slot_digest` already follow (`Option<&T>` hashes like the captured side).
- Under Full mode nothing changes semantically: the scope captures the whole-`PhysicsState` pre-hash and rollback verifies it — which is precisely what proves the inverse misses nothing.
- `pre_parent` is moved into the closure (one `RigidBody` clone, ~hundreds of bytes — replacing a whole-`PhysicsState` clone).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p game`
Expected: PASS — including `attach_collider_rolls_back_bit_exact` (both mode legs), the existing rollback/transaction suites (`log_model.rs`, `simple.rs`, `random_ops.rs`), and `hash_restore.rs`.

- [ ] **Step 5: Run the client convergence harnesses (Full-mode end-to-end gate)**

Run: `cargo test -p client`
Expected: PASS. These drive create-player (capsule attach) and terrain paths under `HashMode::Full` with real rollback/replay; any state the surgical inverse misses fails here loudly.

- [ ] **Step 6: Commit**

```bash
git add crates/game/src/state.rs
git commit -m "perf(game): surgical collider attach — footprint digest instead of whole-PhysicsState snapshot

attach_{voxels,capsule}_collider_safe no longer snapshot_raw (whole-state
clone + Full hash incl. narrow-phase graphs, re-paid per rollback/replay);
the undo is an exact LIFO inverse of insert_with_parent with a slot-sized
Delta digest.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Full suite + live verification

**Files:** none (verification only).

- [ ] **Step 1: Full test suite**

Run: `just test-full`
Expected: PASS, including the slow/ignored tests and the client/server convergence harnesses.

- [ ] **Step 2: Live smoke + profile**

One server + one client (`just run`; `just adapter=6700 run` for the eGPU). Join, move around ~30s (join exercises create_player → capsule attach inside the rollback window, where the old code paid whole-`PhysicsState` clone+hash ~20×/s).

```bash
scripts/perf.sh --no-inline
```

Expected: `Undo<PhysicsState>::hash_data` / `NarrowPhase as Hash` / interaction-graph hashing gone from the top of the profile for the attach paths. Remaining known hashers of the narrow phase after this plan: `remove_entity_safe`'s snapshot (rare; scope-excluded) and `narrow_wholesale` journal digests on collider-removal ticks.

- [ ] **Step 3: Report before/after profile numbers in the branch summary.**

---

## Self-Review Notes

- Footprint completeness argument: `insert_with_parent` touches (a) collider arena + alloc state, (b) `modified_colliders` tail, (c) parent body value, (d) `modified_bodies` tail (conditionally). The undo closure reverts (a) and (b) via `ColliderSet::revert_insert`, (c) via `restore_raw`, (d) via conditional `pop_modified`; the digest hashes all four. The Full-mode harness run (Task 2 Step 5, Task 3 Step 1) is the empirical proof, per the project's `hash(before) == hash(after undo)` bar.
- LIFO validity: both `revert_insert`s assert the popped tail matches, converting any log-ordering bug into a loud panic rather than silent corruption — same contract as the existing body-insert path.
- No behavior change to `remove_entity_safe`, `set_body_pose_safe`, or `create_mesh`'s call sequence — only the two attach fns' undo representation changes.
- Type consistency check: `attach_collider_safe(body: RigidBodyHandle, collider: Collider)` is what both public fns call; fork methods match the signatures listed in Task 1's Produces block; digest fn used identically at log and undo time.
