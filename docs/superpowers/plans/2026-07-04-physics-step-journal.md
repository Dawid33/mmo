# Physics Step Journal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the per-tick whole-`PhysicsState` snapshot with a mutation journal (`StepJournal`) captured inside the rapier fork during `step()`, so per-tick undo cost is O(active bodies + touched pairs), bit-exact under the existing rollback log.

**Architecture:** A `StepJournal` in `crates/rapier` records, once per object per tick, the pre-tick value of everything `step()` mutates (save-before-first-write with dedup), plus exact LIFO inverses for graph structural ops. The broad-phase BVH gets a clean-tick skip and wholesale capture on dirty ticks (see spec: `docs/superpowers/specs/2026-07-04-physics-step-journal-design.md`). Game-side, the journal rides one tier-2 log entry via `undo_scope()`/`register` — the macro-generated log keeps owning transactions/hashing/forgetting.

**Tech Stack:** Rust workspace; vendored `rapier`/`parry` forks; `#[rollback]` proc macro in `crates/macros`; crc32 hashing; `cargo test -p game`.

## Global Constraints

- Rollback bar: `hash(before) == hash(after undo)`, bit-exact over the full hashed state (broad/narrow-phase and solver caches included).
- The journal must never influence simulation math — capture is reads + clones only.
- Vendored forks stay forks: never switch to crates.io versions; determinism features (`libm_force`, `enhanced-determinism`, ordered-float) untouched.
- `game`/`server` stay Bevy-free. The macro expands in `game`; rapier/parry must not depend on `game`.
- rapier's `parallel` feature must stay OFF (journal capture assumes serial iteration; it already is off for determinism).
- Full test battery: `cargo test -p game && cargo test -p client`. Fork-level suite for this work: `cargo test -p game --test step_journal`.
- Commit after every task (steps below include exact commits).

**Verified fork facts this plan relies on** (from code investigation; file:line refs throughout): body writes during `step()` are discoverable at choke points (`modified_bodies` at entry ∪ prev `active_set` ∪ `wake_up` calls ∪ one parent-body flag write in collider user-changes); `BroadPhaseBvh::update` refits the whole tree every frame; `IslandManager`'s hashed fields are O(active); `Graph` edge adds append at the tail and prepend to intrusive list heads, removals swap-relocate the last edge; `CCDSolver` is a unit struct; `ContactPair` Hash covers all fields including `workspace`.

---

### Task 1: Macro — runtime hash-verification gate + `raw_undo_parts`

**Files:**
- Modify: `crates/macros/src/lib.rs` (pre_hash sites ~lines 598, 668, 685, 828, 835, 915, 1002; rollback verification ~lines 1117–1190; per-struct items ~lines 343–401)
- Modify: `crates/client/src/main.rs`, `crates/server/src/main.rs` (startup flag)
- Modify: `docs/superpowers/specs/2026-07-04-physics-step-journal-design.md` (hash-gating paragraph: runtime flag set under `cfg(not(debug_assertions))` at app startup, not compile-time-only)
- Test: existing suites (`cargo test -p game`)

**Interfaces:**
- Produces: `game::set_hash_verification(enabled: bool)` (generated at macro invocation site, re-exported via `game`'s glob) — set once before world creation; default `true`.
- Produces: generated `impl PhysicsState { pub fn raw_undo_parts(&mut self) -> PhysicsStateRaw<'_> }` (same for every `#[rollback]` struct) — raw per-field `&mut` view for use inside undo closures; the undo itself is the mutation license.

- [ ] **Step 1: Add the global flag + setter to the macro's generated root items**

In the token block that generates the log/globals (near the `Entry` struct at ~line 598), add:

```rust
pub static VERIFY_HASHES: ::std::sync::atomic::AtomicBool =
    ::std::sync::atomic::AtomicBool::new(true);

/// Disable per-transaction hash self-verification (release perf). Set once,
/// before any world/transaction exists; entries logged while off carry a
/// sentinel pre_hash and are never checked.
pub fn set_hash_verification(enabled: bool) {
    VERIFY_HASHES.store(enabled, ::std::sync::atomic::Ordering::Relaxed);
}
```

- [ ] **Step 2: Gate every `pre_hash` computation and every rollback-time check**

At each generated `let pre_hash = unsafe { self.hash_data() };` (and the hasher-finalize variants at ~915/~1002), emit instead:

```rust
let pre_hash = if VERIFY_HASHES.load(::std::sync::atomic::Ordering::Relaxed) {
    unsafe { self.hash_data() }
} else { 0u32 };
```

At each rollback verification site (the `if new_hash != entry.pre_hash { panic!... }` blocks at ~1130–1190), wrap:

```rust
if VERIFY_HASHES.load(::std::sync::atomic::Ordering::Relaxed) {
    let new_hash = /* existing computation */;
    if new_hash != entry.pre_hash { /* existing panic/println */ }
}
```

(The hash computation moves inside the guard so release rollbacks skip the O(state) walk entirely.)

- [ ] **Step 3: Generate `raw_undo_parts` next to `snapshot_raw` (~line 370)**

```rust
per_struct_items.push(item(
    "raw_undo_parts",
    quote! {
        impl #s_ident {
            /// Raw per-field view for use INSIDE undo closures only —
            /// running as an undo is the mutation license (the log entry
            /// being reverted covers these writes).
            pub fn raw_undo_parts(&mut self) -> #raw_ident<'_> {
                #raw_ident {
                    #(#f_ident: &mut self.#f_ident.data,)*
                }
            }
        }
    },
));
```

- [ ] **Step 4: Set the flag at startup in both binaries**

In `crates/client/src/main.rs` and `crates/server/src/main.rs`, first line of `main()`:

```rust
// Debug builds keep per-transaction hash self-verification (the rollback
// bar); release skips the O(state) walk — state restore is identical.
#[cfg(not(debug_assertions))]
game::set_hash_verification(false);
```

- [ ] **Step 5: Run the full battery — verification must still fire in tests**

Run: `cargo test -p game && cargo test -p client`
Expected: all PASS (tests are debug builds → flag defaults true → behavior unchanged).

- [ ] **Step 6: Commit**

```bash
git add crates/macros/src/lib.rs crates/client/src/main.rs crates/server/src/main.rs docs/superpowers/specs/2026-07-04-physics-step-journal-design.md
git commit -m "feat(macros): runtime-gated hash verification + raw_undo_parts for undo closures"
```

---

### Task 2: `StepJournal` skeleton + `step_journaled` threading

**Files:**
- Create: `crates/rapier/src/pipeline/step_journal.rs`
- Modify: `crates/rapier/src/pipeline/physics_pipeline.rs` (step ~471–730, detect_collisions ~121–177), `crates/rapier/src/pipeline/mod.rs`, `crates/rapier/src/prelude.rs`

**Interfaces:**
- Produces: `StepJournal` (exported in prelude): `Default`, `Send`, with
  `pub fn revert(self, islands: &mut IslandManager, broad_phase: &mut BroadPhaseBvh, narrow_phase: &mut NarrowPhase, bodies: &mut RigidBodySet, colliders: &mut ColliderSet, impulse_joints: &mut ImpulseJointSet, multibody_joints: &mut MultibodyJointSet)` and `pub fn is_empty(&self) -> bool`.
- Produces: `PhysicsPipeline::step_journaled(...)` — `step()`'s params + trailing `journal: &mut StepJournal`.
- Internal: `step_inner(..., journal: Option<&mut StepJournal>)`; `detect_collisions(..., journal: &mut Option<&mut StepJournal>)` (reborrow with `journal.as_deref_mut()`).

- [ ] **Step 1: Create the journal container (fields filled by later tasks)**

```rust
// crates/rapier/src/pipeline/step_journal.rs
//! Per-tick mutation journal: save-before-first-write capture of everything
//! `PhysicsPipeline::step` mutates, with an exact LIFO revert. Pure capture —
//! never influences simulation. See the game repo spec
//! `docs/superpowers/specs/2026-07-04-physics-step-journal-design.md`.
use crate::dynamics::{
    ImpulseJointSet, IslandManager, MultibodyJointSet, RigidBody, RigidBodyHandle, RigidBodySet,
};
use crate::geometry::{BroadPhaseBvh, Collider, ColliderHandle, ColliderSet, NarrowPhase};
use parry3d::utils::hashset::HashSet;

#[derive(Default)]
pub struct StepJournal {
    pub(crate) saved_bodies: Vec<(RigidBodyHandle, RigidBody)>,
    pub(crate) saved_body_set: HashSet<RigidBodyHandle>,
    pub(crate) saved_colliders: Vec<(ColliderHandle, Collider)>,
    pub(crate) saved_collider_set: HashSet<ColliderHandle>,
    pub(crate) islands: Option<crate::dynamics::IslandsSaved>,          // Task 3
    pub(crate) lists: Option<ListsSaved>,                               // Task 3
    pub(crate) joints: Option<Box<(ImpulseJointSet, MultibodyJointSet)>>, // Task 3
    pub(crate) narrow: Vec<crate::geometry::NarrowUndo>,                // Task 5
    pub(crate) narrow_wholesale: Option<Box<NarrowPhase>>,              // Task 5
    pub(crate) broad: Option<Box<crate::geometry::BroadSaved>>,         // Task 4
}

pub(crate) struct ListsSaved {
    pub modified_bodies: crate::dynamics::ModifiedRigidBodies,
    pub modified_colliders: crate::geometry::ModifiedColliders,
    pub removed_colliders: Vec<ColliderHandle>,
    pub impulse_to_wake_up: HashSet<RigidBodyHandle>,
    pub multibody_to_wake_up: HashSet<RigidBodyHandle>,
}

impl StepJournal {
    pub fn is_empty(&self) -> bool {
        self.saved_bodies.is_empty()
            && self.saved_colliders.is_empty()
            && self.narrow.is_empty()
            && self.narrow_wholesale.is_none()
            && self.broad.is_none()
        // islands/lists are captured unconditionally but are O(active)/O(modified);
        // is_empty() reports "nothing moved" for the size assertions, so it
        // additionally requires the islands capture to be empty:
            && self.islands.as_ref().map_or(true, |i| i.active_set.is_empty())
    }

    pub fn save_body(&mut self, handle: RigidBodyHandle, body: &RigidBody) {
        if self.saved_body_set.insert(handle) {
            self.saved_bodies.push((handle, body.clone()));
        }
    }

    pub fn save_collider(&mut self, handle: ColliderHandle, collider: &Collider) {
        if self.saved_collider_set.insert(handle) {
            self.saved_colliders.push((handle, collider.clone()));
        }
    }

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
            bodies.restore_raw(h, old); // Task 3
        }
        for (h, old) in self.saved_colliders.into_iter().rev() {
            colliders.restore_raw(h, old); // Task 3
        }
        // Later tasks: islands, lists, joints, narrow (LIFO ops or wholesale),
        // broad (wholesale).
        let _ = (islands, broad_phase, narrow_phase, impulse_joints, multibody_joints);
        let _ = (self.islands, self.lists, self.joints, self.narrow, self.narrow_wholesale, self.broad);
    }
}
```

(`IslandsSaved`, `NarrowUndo`, `BroadSaved`, `restore_raw` are created in Tasks 3–5; for this task, leave the fields typed as above but comment out the not-yet-existing types and the body of `revert` beyond bodies/colliders so the crate compiles — the struct must exist with `save_body`/`save_collider`/`is_empty` working.)

- [ ] **Step 2: Thread the journal through the pipeline**

In `physics_pipeline.rs`:
- Rename the body of `pub fn step(...)` to `fn step_inner(..., mut journal: Option<&mut StepJournal>)` (same 12 params + journal).
- `pub fn step(...)` → `self.step_inner(gravity, ..., events, None)`.
- Add `pub fn step_journaled(...)` with the same params plus `journal: &mut StepJournal` → `self.step_inner(..., Some(journal))`.
- Give `detect_collisions` a `journal: &mut Option<&mut StepJournal>` parameter; pass `&mut journal.as_deref_mut()` from `step_inner` (both call sites, ~546 and ~682). Inside, forward with `journal.as_deref_mut()` wherever later tasks hook.

Register the module in `pipeline/mod.rs` and export `StepJournal` from `prelude.rs`.

- [ ] **Step 3: Verify no behavior change**

Run: `cargo test -p game && cargo build --workspace --bins`
Expected: all PASS (journal is threaded but captures nothing; `step()` is byte-identical behavior).

- [ ] **Step 4: Commit**

```bash
git add crates/rapier/src/pipeline/ crates/rapier/src/prelude.rs
git commit -m "feat(rapier): StepJournal skeleton threaded through PhysicsPipeline::step"
```

---

### Task 3: Bodies, colliders, islands, lists, joint wholesale — capture + revert

**Files:**
- Modify: `crates/rapier/src/pipeline/step_journal.rs`, `physics_pipeline.rs`, `pipeline/user_changes.rs`
- Modify: `crates/rapier/src/dynamics/island_manager.rs`, `rigid_body_set.rs`, `rigid_body_components.rs` (`RigidBodyColliders::update_positions` ~1111)
- Modify: `crates/rapier/src/geometry/collider_set.rs`
- Test: Create `crates/game/tests/step_journal.rs`

**Interfaces:**
- Consumes: `StepJournal::save_body`/`save_collider` (Task 2).
- Produces: `RigidBodySet::restore_raw(&mut self, h, RigidBody)`, `ColliderSet::restore_raw(&mut self, h, Collider)` — overwrite the arena slot value, no modified-list push (undo-only).
- Produces: `IslandsSaved { active_set, active_islands, additional_solver_iterations, timestamp }` + `IslandManager::journal_save(&self) -> IslandsSaved` / `journal_restore(&mut self, IslandsSaved)` (hashed fields only; `can_sleep`/`stack` are unhashed scratch).
- Produces: `IslandManager::wake_up_journaled(&mut self, bodies, handle, strong, journal: &mut Option<&mut StepJournal>)`; existing `wake_up` delegates with `&mut None`.
- Produces: `RigidBodyColliders::update_positions_journaled(..., journal: &mut Option<&mut StepJournal>)`; existing method delegates.

- [ ] **Step 1: Write the failing test (test file skeleton + free-fall case)**

```rust
// crates/game/tests/step_journal.rs
//! Fork-level StepJournal invariants: step-then-revert must be hash-exact.
//! Style mirrors hash_restore.rs: build components directly, crc32 the Hash.
use rapier3d::prelude::*;

struct Crc32Std(crc32fast::Hasher);
impl std::hash::Hasher for Crc32Std {
    fn write(&mut self, bytes: &[u8]) { self.0.update(bytes) }
    fn finish(&self) -> u64 { self.0.clone().finalize() as u64 }
}
fn h<T: std::hash::Hash>(t: &T) -> u32 {
    let mut hasher = Crc32Std(crc32fast::Hasher::new());
    t.hash(&mut hasher);
    std::hash::Hasher::finish(&hasher) as u32
}

struct World {
    pipeline: PhysicsPipeline,
    islands: IslandManager,
    broad: BroadPhaseBvh,
    narrow: NarrowPhase,
    bodies: RigidBodySet,
    colliders: ColliderSet,
    ij: ImpulseJointSet,
    mj: MultibodyJointSet,
    ccd: CCDSolver,
    params: IntegrationParameters,
    gravity: Vector<Real>,
}
impl World {
    fn new() -> Self {
        Self {
            pipeline: PhysicsPipeline::new(),
            islands: IslandManager::new(),
            broad: BroadPhaseBvh::new(),
            narrow: NarrowPhase::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            ij: ImpulseJointSet::new(),
            mj: MultibodyJointSet::new(),
            ccd: CCDSolver::new(),
            params: IntegrationParameters::default(),
            gravity: vector![Real::from(0.0), Real::from(-9.81), Real::from(0.0)],
        }
    }
    fn step(&mut self) -> StepJournal {
        let mut j = StepJournal::default();
        self.pipeline.step_journaled(
            &self.gravity, &self.params, &mut self.islands, &mut self.broad,
            &mut self.narrow, &mut self.bodies, &mut self.colliders,
            &mut self.ij, &mut self.mj, &mut self.ccd, &(), &(), &mut j,
        );
        j
    }
    fn revert(&mut self, j: StepJournal) {
        j.revert(&mut self.islands, &mut self.broad, &mut self.narrow,
                 &mut self.bodies, &mut self.colliders, &mut self.ij, &mut self.mj);
    }
    // Task 3 asserts only the structures Task 3 covers; Task 5 upgrades
    // callers to hash_full (adds narrow), Task 4 adds broad.
    fn hash_dynamics(&self) -> (u32, u32, u32) {
        (h(&self.bodies), h(&self.colliders), h(&self.islands))
    }
}

#[test]
fn free_fall_revert_is_hash_exact() {
    let mut w = World::new();
    // No colliders at all: broad/narrow untouched; exercises integration,
    // island activation, force/mprops/sleep bookkeeping, modified lists.
    w.bodies.insert(
        RigidBodyBuilder::dynamic()
            .translation(vector![Real::from(0.0), Real::from(10.0), Real::from(0.0)])
            .build(),
    );
    let before = w.hash_dynamics();
    let journals: Vec<StepJournal> = (0..10).map(|_| w.step()).collect();
    for j in journals.into_iter().rev() {
        w.revert(j);
    }
    assert_eq!(before, w.hash_dynamics(), "free-fall 10-step LIFO revert must be exact");
}
```

- [ ] **Step 2: Run it — must fail**

Run: `cargo test -p game --test step_journal`
Expected: FAIL — hashes differ (nothing captured yet).

- [ ] **Step 3: Implement `restore_raw`, `IslandsSaved`, list/joint capture, and the hook sites**

`rigid_body_set.rs` / `collider_set.rs` (next to `revert_insert`):

```rust
    /// Undo-only: overwrite the slot value; the handle must be occupied.
    /// Does NOT touch modified/removed lists (the journal restores those
    /// wholesale).
    pub fn restore_raw(&mut self, handle: RigidBodyHandle, body: RigidBody) {
        *self.bodies.get_mut(handle.0).expect("restore_raw: slot vacant") = body;
    }
```

(ColliderSet analog on `self.colliders`.)

`island_manager.rs`:

```rust
pub struct IslandsSaved {
    pub active_set: Vec<RigidBodyHandle>,
    pub active_islands: Vec<usize>,
    pub additional_solver_iterations: Vec<usize>,
    pub timestamp: u32,
}
impl IslandManager {
    pub fn journal_save(&self) -> IslandsSaved {
        IslandsSaved {
            active_set: self.active_set.clone(),
            active_islands: self.active_islands.clone(),
            additional_solver_iterations: self.active_islands_additional_solver_iterations.clone(),
            timestamp: self.active_set_timestamp,
        }
    }
    pub fn journal_restore(&mut self, s: IslandsSaved) {
        self.active_set = s.active_set;
        self.active_islands = s.active_islands;
        self.active_islands_additional_solver_iterations = s.additional_solver_iterations;
        self.active_set_timestamp = s.timestamp;
    }
    pub fn wake_up_journaled(&mut self, bodies: &mut RigidBodySet, handle: RigidBodyHandle,
                             strong: bool, journal: &mut Option<&mut crate::pipeline::StepJournal>) {
        if let Some(j) = journal.as_deref_mut() {
            if let Some(rb) = bodies.get(handle) { j.save_body(handle, rb); }
        }
        self.wake_up(bodies, handle, strong)
    }
}
```

Capture hooks in `step_inner` (all guarded `if let Some(j) = journal.as_deref_mut()`), in order:

1. **Top of step, before the wake-up drain (~491):** `j.islands = Some(islands.journal_save());` and capture `lists` — clone both `to_wake_up` sets *before* they are drained; capture `joints` wholesale iff `impulse_joints.len() > 0 || multibody_joints.multibodies.len() > 0` (game has none today — wholesale is the documented coarse fallback).
2. **Wake-up drain loop (~501):** replace `islands.wake_up(bodies, handle, true)` with `islands.wake_up_journaled(bodies, handle, true, &mut journal.as_deref_mut())`.
3. **`take_modified`/`take_removed` (~506–515):** after taking, extend `lists` with clones of the three vecs; save each modified collider (`for h in modified_colliders: if let Some(c) = colliders.get(h) { j.save_collider(h, c) }`) and each modified body likewise.
4. **`handle_user_changes_to_colliders` (user_changes.rs ~10–43):** pass journal; before the parent-rb `changes` write (~line 38): `j.save_body(parent_handle, rb)`; the collider itself is already saved via (3).
5. **`handle_user_changes_to_rigid_bodies` (user_changes.rs ~45–176):** pass journal; before attached-collider flag/pos writes (~94–127): `j.save_collider(...)` per collider; `islands.rigid_body_removed` (~170) gets a journal param and saves the swap-relocated body before writing its `ids.active_set_id`.
6. **Before `interpolate_kinematic_velocities` each substep (~640):** save every current active body: `for h in islands.active_bodies() { j.save_body(*h, &bodies[*h]) }` (dedup makes repeats free; this covers the solver writeback, sleep commits, CCD flag writes, `advance_to_final_positions`, and both mass-props loops, all of which iterate subsets of the active set).
7. **`update_active_set_with_contacts`:** no extra hook needed — its writes hit prev-active-set bodies (covered by 6) and bodies entering via `wake_up_journaled` (covered by 2 and Task 5's narrow-phase call sites; until Task 5, non-contact tests don't exercise those).
8. **`RigidBodyColliders::update_positions` (~rigid_body_components.rs:1111):** add `update_positions_journaled(..., journal)`; save each collider before `co.pos`/`co.changes` writes; callers inside step (user_changes ~94, advance_to_final_positions ~388) use the journaled variant.
9. **`clear_modified_colliders` full sweep (~85):** no save (flags of non-modified colliders are already-empty → value no-op). Add
   `debug_assert!(co.changes.is_empty() || journal_saved(co))`-style check gated on the journal being present.
10. **Multibody guard (~539):** `debug_assert!(multibody_joints.multibodies.is_empty() || journal.is_none() || j.joints.is_some());`

Complete `StepJournal::revert` for this task's sections (bodies/colliders as in Task 2, then):

```rust
        if let Some(s) = self.islands { islands.journal_restore(s); }
        if let Some(l) = self.lists {
            bodies.set_modified(l.modified_bodies);
            colliders.set_modified(l.modified_colliders);
            colliders.set_removed(l.removed_colliders);   // add setter next to take_removed
            impulse_joints.to_wake_up = l.impulse_to_wake_up;
            multibody_joints.to_wake_up = l.multibody_to_wake_up;
        }
        if let Some(j) = self.joints {
            *impulse_joints = j.0;
            *multibody_joints = j.1;
        }
```

(`RigidBodySet::set_modified` may need adding next to `take_modified` (~113), mirroring `ColliderSet`'s (~84–90); `to_wake_up` fields need `pub(crate)` visibility from the pipeline module — adjust as required.)

- [ ] **Step 4: Run the test — must pass**

Run: `cargo test -p game --test step_journal`
Expected: PASS.

- [ ] **Step 5: Add the sleep-transition case (same file)**

```rust
#[test]
fn sleep_transition_revert_is_hash_exact() {
    let mut w = World::new();
    w.bodies.insert(
        RigidBodyBuilder::dynamic()
            .translation(vector![Real::from(0.0), Real::from(0.0), Real::from(0.0)])
            .linvel(vector![Real::from(0.0), Real::from(0.0), Real::from(0.0)])
            .gravity_scale(Real::from(0.0)) // no colliders; zero-vel body falls asleep
            .build(),
    );
    let before = w.hash_dynamics();
    // Enough ticks to cross the sleep threshold (activation commits vels=0 + sleep()).
    let journals: Vec<StepJournal> = (0..120).map(|_| w.step()).collect();
    for j in journals.into_iter().rev() { w.revert(j); }
    assert_eq!(before, w.hash_dynamics());
}
```

Run: `cargo test -p game --test step_journal` → PASS (if the sleep commit isn't captured, hook 6 is wrong — fix there, not in the test).

- [ ] **Step 6: Full battery + commit**

Run: `cargo test -p game && cargo test -p client`

```bash
git add crates/rapier/src crates/game/tests/step_journal.rs
git commit -m "feat(rapier): journal capture for bodies, colliders, islands, lists, joints"
```

---

### Task 4: Broad phase — clean-tick skip + dirty wholesale capture, optimizer off

**Files:**
- Modify: `crates/parry/src/partitioning/bvh/bvh_insert.rs` (predicate extraction ~209–231), `crates/parry/src/partitioning/bvh/mod.rs` (export if needed)
- Modify: `crates/rapier/src/geometry/broad_phase_bvh.rs` (update ~101–265, set_aabb ~280, BvhOptimizationStrategy default ~46–56)
- Modify: `crates/rapier/src/pipeline/physics_pipeline.rs` (detect_collisions broad call; last-substep `set_aabb` loop ~702–706)
- Modify: `crates/rapier/src/pipeline/step_journal.rs`
- Test: `crates/game/tests/step_journal.rs`

**Interfaces:**
- Produces: `Bvh::leaf_needs_update(&self, aabb: &Aabb, leaf_index: u32, margin: Real) -> bool` (parry, read-only).
- Produces: `BroadSaved` + `BroadPhaseBvh::journal_save(&self) -> BroadSaved` / `journal_restore(&mut self, BroadSaved)` (tree + pairs + frame_index; workspace is unhashed scratch, never captured).
- Changes: `BroadPhaseBvh::update(..., journal: &mut Option<&mut StepJournal>)`; `set_aabb_journaled(...)`; `BvhOptimizationStrategy` `#[default]` moves to `None` (fork-wide: rollback is this fork's purpose; incremental optimization is per-frame churn the journal would have to pay for. Insert-time rotations keep tree quality; full `Bvh::rebuild` at load is the escalation if broad-phase queries ever profile hot).

- [ ] **Step 1: Failing tests — idle skip + moving-collider dirty capture**

```rust
fn add_floor(w: &mut World, x: i32, z: i32) {
    // One fixed cuboid per "chunk" — stands in for chunk voxel colliders.
    let b = w.bodies.insert(
        RigidBodyBuilder::fixed()
            .translation(vector![Real::from((x * 32) as f32), Real::from(-1.0), Real::from((z * 32) as f32)])
            .build(),
    );
    w.colliders.insert_with_parent(
        ColliderBuilder::cuboid(Real::from(16.0), Real::from(1.0), Real::from(16.0)).build(),
        b, &mut w.bodies,
    );
}
fn hash_broad(w: &World) -> u32 { h(&w.broad) }

#[test]
fn idle_tick_journal_is_empty_and_broad_untouched() {
    let mut w = World::new();
    for x in 0..8 { for z in 0..8 { add_floor(&mut w, x, z); } }
    // Settle the initial inserts (first dirty ticks build the tree).
    for _ in 0..3 { let _ = w.step(); }
    let before_broad = hash_broad(&w);
    let j = w.step(); // nothing moves: clean tick
    assert!(j.broad.is_none(), "clean tick must not capture the BVH");
    assert!(j.is_empty(), "idle tick journal must be empty");
    assert_eq!(before_broad, hash_broad(&w), "clean tick must not mutate the BVH");
}

#[test]
fn moving_body_dirty_tick_revert_is_hash_exact() {
    let mut w = World::new();
    for x in 0..8 { for z in 0..8 { add_floor(&mut w, x, z); } }
    w.bodies.insert(
        RigidBodyBuilder::dynamic()
            .translation(vector![Real::from(2.0), Real::from(8.0), Real::from(2.0)])
            .build(),
    ); // falls fast enough to move leaves past the change-detection skin
    let before = (w.hash_dynamics(), hash_broad(&w));
    let journals: Vec<StepJournal> = (0..20).map(|_| w.step()).collect();
    for j in journals.into_iter().rev() { w.revert(j); }
    assert_eq!(before, (w.hash_dynamics(), hash_broad(&w)));
}
```

- [ ] **Step 2: Run — idle test FAILS** (today refit/optimizer/frame_index mutate every tick), moving test FAILS (broad not captured).

- [ ] **Step 3: Implement**

parry `bvh_insert.rs` — extract the exact predicate `insert_or_update_partially` uses (lines ~215–227):

```rust
    /// Read-only: would `insert_or_update_partially` with these arguments
    /// write anything? (Same conditions, no mutation.)
    pub fn leaf_needs_update(&self, aabb: &Aabb, leaf_index: u32, margin: Real) -> bool {
        match self.leaf_node_indices.get(leaf_index as usize) {
            Some(leaf) => {
                if margin > 0.0.into() { !self.nodes[*leaf].contains_aabb(aabb) } else { true }
            }
            None => true, // new leaf → insert_new_unchecked
        }
    }
```

rapier `broad_phase_bvh.rs`:

```rust
pub struct BroadSaved {
    pub(crate) tree: Bvh,
    pub(crate) pairs: HashMap<(ColliderHandle, ColliderHandle), u32>,
    pub(crate) frame_index: u32,
}
impl BroadPhaseBvh {
    pub fn journal_save(&self) -> BroadSaved {
        BroadSaved { tree: self.tree.clone(), pairs: self.pairs.clone(), frame_index: self.frame_index }
    }
    pub fn journal_restore(&mut self, s: BroadSaved) {
        self.tree = s.tree;
        self.pairs = s.pairs;
        self.frame_index = s.frame_index;
        // workspace: unhashed scratch (serde-skipped) — deliberately untouched.
    }
}
```

Restructure `update` (journal param added; **`frame_index` bump moves inside the dirty branch**):

```rust
        let margin = if Self::CHANGE_DETECTION_ENABLED {
            Self::CHANGE_DETECTION_FACTOR * params.length_unit
        } else { Real::from(0.0) };

        // Pre-scan (read-only): compute AABBs once, decide dirtiness.
        let mut updates: Vec<(u32, Aabb)> = Vec::new();
        for modified in modified_colliders {
            if let Some(collider) = colliders.get(*modified) {
                if !collider.is_enabled() || !collider.changes.needs_broad_phase_update() {
                    continue;
                }
                let aabb = collider.compute_broad_phase_aabb(params, bodies);
                let key = modified.into_raw_parts().0;
                if self.tree.leaf_needs_update(&aabb, key, margin) {
                    updates.push((key, aabb));
                }
            }
        }
        let dirty = !removed_colliders.is_empty() || !updates.is_empty();
        if !dirty {
            return; // clean tick: no tree/pairs/frame_index mutation at all.
        }
        if let Some(j) = journal.as_deref_mut() {
            if j.broad.is_none() {
                j.broad = Some(Box::new(self.journal_save()));
            }
        }
        self.frame_index = self.frame_index.overflowing_add(1).0;
        for handle in removed_colliders {
            self.tree.remove(handle.into_raw_parts().0);
        }
        for (key, aabb) in updates {
            self.tree.insert_or_update_partially(aabb, key, margin);
        }
        // ... existing: optimization_strategy match, refit, traversal, pairs GC
        //     (unchanged from here down; the GC retain-pass is a no-op when no
        //     node changed, which is why skipping it on clean ticks is exact).
```

Semantics notes for the implementer (already verified): the stale-pair GC only deletes a pair when one of its nodes `is_changed()` — on a clean tick no node changed, so skipping GC ≙ running it. `frame_index` only participates in `timestamp != frame_index` staleness checks; not bumping on clean ticks keeps those exact. The `first_pass` variable (~122) becomes `self.tree.is_empty()` checked before inserts as today — with an empty world (no colliders, no updates) the tick is clean and skips.

`set_aabb` (~280) gains a journaled twin used by the last-substep loop (`physics_pipeline.rs` ~702–706):

```rust
    pub fn set_aabb_journaled(&mut self, params: &IntegrationParameters, handle: ColliderHandle,
                              aabb: Aabb, journal: &mut Option<&mut StepJournal>) {
        let margin = /* same skin computation as set_aabb */;
        let key = handle.into_raw_parts().0;
        if !self.tree.leaf_needs_update(&aabb, key, margin) {
            // insert_with_change_detection would still rewrite equal bounds;
            // skip to keep clean ticks clean. (margin > 0 always here.)
            return;
        }
        if let Some(j) = journal.as_deref_mut() {
            if j.broad.is_none() { j.broad = Some(Box::new(self.journal_save())); }
        }
        self.tree.insert_with_change_detection(aabb, key, margin);
    }
```

Move `BvhOptimizationStrategy`'s `#[default]` from `SubtreeOptimizer` to `None` (broad_phase_bvh.rs ~46–56) with a comment: incremental optimization rewrites ~5% of the tree per frame regardless of movement — pure journal noise for a rollback fork; insert rotations keep quality.

Wire `journal.as_deref_mut()` into the `broad_phase.update(...)` call inside `detect_collisions` and into the last-substep loop, and complete `StepJournal::revert`:

```rust
        if let Some(b) = self.broad { broad_phase.journal_restore(*b); }
```

- [ ] **Step 4: Run — both tests PASS.** Also run `cargo test -p game` (full): the changed defaults alter bit trajectories; any golden-hash test that breaks must be re-baselined in THIS commit (behavior change lands alone, bisectable).

- [ ] **Step 5: Commit**

```bash
git add crates/parry/src/partitioning/bvh/ crates/rapier/src/geometry/broad_phase_bvh.rs crates/rapier/src/pipeline/ crates/game/tests/step_journal.rs
git commit -m "feat(rapier,parry): broad-phase clean-tick skip + dirty-tick journal capture; optimizer off"
```

---

### Task 5: Narrow phase — payload saves + exact graph inverses

**Files:**
- Modify: `crates/rapier/src/data/graph.rs`, `crates/rapier/src/geometry/narrow_phase.rs`, `crates/rapier/src/geometry/interaction_graph.rs`, `crates/rapier/src/data/coarena.rs`
- Modify: `crates/rapier/src/pipeline/step_journal.rs`, `physics_pipeline.rs` (detect_collisions forwarding)
- Test: `crates/game/tests/step_journal.rs`

**Interfaces:**
- Produces: `GraphCellUndo<N, E>` + `Graph::add_node_journaled/add_edge_journaled/remove_edge_journaled(..., ops: Option<&mut Vec<GraphCellUndo<N,E>>>)` + `Graph::apply_cell_undo(op)`.
- Produces: `NarrowUndo` enum + `NarrowPhase::journal_revert(&mut self, ops: Vec<NarrowUndo>)`; `Coarena::len()`/slot read helpers as needed.
- Changes: `NarrowPhase::{handle_user_changes, register_pairs, compute_contacts, compute_intersections}` gain `journal: &mut Option<&mut StepJournal>` (threaded from `detect_collisions`).

- [ ] **Step 1: Failing tests — contact lifecycle + full-state hash**

Add to `World`: `fn hash_full(&self) -> (u32, u32, u32, u32, u32) { (h(&self.bodies), h(&self.colliders), h(&self.islands), h(&self.broad), h(&self.narrow)) }`

```rust
#[test]
fn landing_on_floor_revert_is_hash_exact() {
    let mut w = World::new();
    add_floor(&mut w, 0, 0);
    let b = w.bodies.insert(
        RigidBodyBuilder::dynamic()
            .translation(vector![Real::from(2.0), Real::from(3.0), Real::from(2.0)])
            .build(),
    );
    w.colliders.insert_with_parent(ColliderBuilder::ball(Real::from(0.5)).build(), b, &mut w.bodies);
    let mut checkpoints = vec![w.hash_full()];
    let mut journals = Vec::new();
    for _ in 0..90 { // fall, impact (pair add + manifolds + warm-start), settle, sleep
        journals.push(w.step());
        checkpoints.push(w.hash_full());
    }
    for (j, expect) in journals.into_iter().rev().zip(checkpoints.into_iter().rev().skip(1)) {
        w.revert(j);
        assert_eq!(expect, w.hash_full(), "every intermediate tick must restore exactly");
    }
}

#[test]
fn flyby_pair_create_then_destroy_revert_is_hash_exact() {
    let mut w = World::new();
    add_floor(&mut w, 0, 0);
    // Kinematic bullet passing over the floor: pair appears, then DeletePair
    // fires when it leaves — exercises remove_edge's swap-relocation inverse.
    let b = w.bodies.insert(
        RigidBodyBuilder::kinematic_position_based()
            .translation(vector![Real::from(-40.0), Real::from(0.5), Real::from(2.0)])
            .build(),
    );
    w.colliders.insert_with_parent(ColliderBuilder::ball(Real::from(0.5)).build(), b, &mut w.bodies);
    let before = w.hash_full();
    let mut journals = Vec::new();
    for i in 0..60 {
        let x = Real::from(-40.0 + (i as f32) * 2.0);
        w.bodies.get_mut(b).unwrap().set_next_kinematic_translation(vector![x, Real::from(0.5), Real::from(2.0)]);
        journals.push(w.step());
    }
    for j in journals.into_iter().rev() { w.revert(j); }
    // Note: set_next_kinematic_translation is a USER mutation outside step();
    // revert restores to each tick's pre-step state, and the final compare is
    // against `before` taken before any user mutation — the first journal's
    // lists/body capture includes the first tick's pre-step modified state,
    // so assert against a checkpoint taken AFTER the first user write:
    // (simplest correct form: capture `before` after the first
    // set_next_kinematic_translation call, before the first step)
    assert_eq!(before, w.hash_full());
}
```

(Adjust the fly-by test exactly as its comment says: take `before` after the first user write, before the first step — user mutations are the game log's job, not the step journal's.)

- [ ] **Step 2: Run — FAIL** (narrow phase mutations uncaptured → hash mismatch).

- [ ] **Step 3: Implement graph cell journaling (`data/graph.rs`)**

```rust
/// One reverted cell/structural write inside `Graph`. Recorded in execution
/// order; revert applies LIFO. Exact inverses — same discipline as
/// `Arena::revert_insert` (plain re-do does NOT restore link/allocator state).
pub enum GraphCellUndo<N, E> {
    NodePush,                                              // undo: nodes.pop()
    EdgePush,                                              // undo: edges.pop()
    NodeNext { node: u32, dir: usize, old: EdgeIndex },
    EdgeNext { edge: u32, dir: usize, old: EdgeIndex },
    EdgeNode { edge: u32, dir: usize, old: NodeIndex },
    /// edges.swap_remove(at): `removed` was taken out of slot `at`; the then-
    /// last edge (if any) moved into `at`. Undo: move it back to the tail,
    /// put `removed` back into `at`.
    EdgeSwapRemove { at: u32, removed: Edge<E>, had_swap: bool },
    NodeWeight { node: u32, old: N },
}

impl<N, E> Graph<N, E> {
    pub fn apply_cell_undo(&mut self, op: GraphCellUndo<N, E>) {
        match op {
            GraphCellUndo::NodePush => { self.nodes.pop(); }
            GraphCellUndo::EdgePush => { self.edges.pop(); }
            GraphCellUndo::NodeNext { node, dir, old } => self.nodes[node as usize].next[dir] = old,
            GraphCellUndo::EdgeNext { edge, dir, old } => self.edges[edge as usize].next[dir] = old,
            GraphCellUndo::EdgeNode { edge, dir, old } => self.edges[edge as usize].node[dir] = old,
            GraphCellUndo::EdgeSwapRemove { at, removed, had_swap } => {
                if had_swap {
                    let moved = std::mem::replace(&mut self.edges[at as usize], removed);
                    self.edges.push(moved);
                } else {
                    debug_assert_eq!(at as usize, self.edges.len());
                    self.edges.push(removed);
                }
            }
            GraphCellUndo::NodeWeight { node, old } => self.nodes[node as usize].weight = old,
        }
    }
}
```

Journaled mutators: each existing mutation site records the old cell value *before* writing, appending to `ops` in execution order:
- `add_node_journaled`: record `NodePush` after the push.
- `add_edge_journaled` (~219–243): record old `an.next[0]` / `bn.next[1]` as `NodeNext` before overwriting, then `EdgePush`.
- `remove_edge_journaled` (~353–383): instrument `change_edge_links` with an optional ops sink — every `node.next[dir] = ...` / `edge.next[dir] = ...` write records its old value first; the `swap_remove` records `EdgeSwapRemove { at: e, removed, had_swap: e != edges.len() }` (capture `removed` by clone before the swap); the post-swap `change_edge_links(swap...)` rewires record through the same sink. Existing `remove_edge` delegates with `None`.
- `remove_node` is NOT instrumented — the wholesale fallback covers it (below).

LIFO correctness argument (put in a comment): ops are recorded in execution order and applied in reverse, so the post-swap rewires are undone first (restoring references to the old tail index), then the swap itself, then the splice — every index an op mentions is valid at the moment that op replays.

- [ ] **Step 4: Implement `NarrowUndo` + NarrowPhase capture (`narrow_phase.rs`)**

```rust
pub enum NarrowUndo {
    ContactPayload { edge: u32, old: Box<ContactPair> },
    IntersectionPayload { edge: u32, old: IntersectionPair },
    ContactGraph(GraphCellUndo<ColliderHandle, ContactPair>),
    IntersectionGraph(GraphCellUndo<ColliderHandle, IntersectionPair>),
    GraphIndicesSlot { index: u32, old_gen: u32, old: ColliderGraphIndices },
    GraphIndicesTruncate { old_len: u32 },
}
```

Capture rules (each site guarded on the journal, pushing to `journal.narrow`):
- `add_pair` (~620–688): `ensure_pair_exists` records `GraphIndicesTruncate { old_len }` before any resize and `GraphIndicesSlot` before each slot write; node/edge additions go through the `_journaled` graph mutators wrapped as `ContactGraph`/`IntersectionGraph` ops.
- `remove_pair` (~547–617): `remove_edge_journaled` via the same wrappers. The islands `wake_up` calls inside (~599–603) become `wake_up_journaled`.
- `handle_user_changes` (~293–345): if `removed_colliders` is non-empty and the journal is active, set `journal.narrow_wholesale = Some(Box::new(narrow_phase.clone()))` once for the tick and skip fine-grained capture for the rest of the tick (rare structural event; `remove_node` cascades are not instrumented). Sensor-migration pairs (~440–545) use the journaled add/remove paths.
- `compute_contacts` (~817–1127): at each `pair.clear()` early-out, save only if the pair isn't already cleared-empty:

```rust
    if let Some(j) = journal.as_deref_mut() {
        if !(pair.manifolds.is_empty() && pair.workspace.is_none() && !pair.has_any_active_contact) {
            j.narrow.push(NarrowUndo::ContactPayload { edge: edge_id, old: Box::new(pair.clone()) });
        }
    }
```

  and at the top of the full-compute path (the branch that reaches `contact_manifolds`, ~972): save unconditionally (dedup by `(edge_id)` is unnecessary — each edge is visited once per pass; the substep loop can visit twice, which is correct LIFO anyway).
- `compute_intersections` (~717–815): before writing `edge.weight.intersecting`/`start_event_emitted`, save `IntersectionPayload` if the new value differs.

Revert (in `StepJournal::revert`, before the broad restore):

```rust
        if let Some(w) = self.narrow_wholesale {
            *narrow_phase = *w;
        } else {
            narrow_phase.journal_revert(self.narrow); // pops Vec in reverse,
            // dispatching payload restores + graph/coarena cell undos
        }
```

`NarrowPhase::journal_revert` needs private-field access — implement it in `narrow_phase.rs`; `GraphIndicesTruncate` undoes with `self.graph_indices.truncate_raw(old_len)` (add a `pub(crate)` truncate on `Coarena`).

- [ ] **Step 5: Run tests — all PASS.** Then upgrade `free_fall`/`sleep` asserts to `hash_full()` (they now hold for the whole state) and re-run.

Run: `cargo test -p game --test step_journal && cargo test -p game`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/rapier/src crates/game/tests/step_journal.rs
git commit -m "feat(rapier): narrow-phase journal — payload saves + exact graph cell inverses"
```

---

### Task 6: Game flip — journaled `on_tick`, fuzzer, size + perf gates

**Files:**
- Modify: `crates/game/src/physics.rs` (on_tick ~13–43)
- Modify: `crates/game/tests/step_journal.rs` (fuzzer, size property)
- Test: full battery + existing perf regression test

**Interfaces:**
- Consumes: `StepJournal`, `step_journaled` (Tasks 2–5); `undo_scope().raw_fields()` (existing macro API); `raw_undo_parts` (Task 1).

- [ ] **Step 1: Fuzzer + size property tests (fork-level, still standalone)**

```rust
// Deterministic LCG so the test needs no rand dep and reproduces exactly.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 { self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); self.0 >> 33 }
    fn f(&mut self, lo: f32, hi: f32) -> f32 { lo + (self.next() % 10_000) as f32 / 10_000.0 * (hi - lo) }
}

#[test]
fn randomized_scene_revert_is_hash_exact() {
    let mut rng = Lcg(0x5EED_2026_0704);
    let mut w = World::new();
    for x in 0..4 { for z in 0..4 { add_floor(&mut w, x, z); } }
    let mut balls = Vec::new();
    for _ in 0..6 {
        let b = w.bodies.insert(RigidBodyBuilder::dynamic()
            .translation(vector![Real::from(rng.f(1.0, 100.0)), Real::from(rng.f(2.0, 6.0)), Real::from(rng.f(1.0, 100.0))])
            .build());
        w.colliders.insert_with_parent(ColliderBuilder::ball(Real::from(0.4)).build(), b, &mut w.bodies);
        balls.push(b);
    }
    let mut checkpoints = Vec::new();
    let mut journals = Vec::new();
    for tick in 0..200 {
        if tick % 7 == 0 { // random impulse = user mutation BEFORE the step
            let b = balls[(rng.next() as usize) % balls.len()];
            w.bodies.get_mut(b).unwrap().apply_impulse(
                vector![Real::from(rng.f(-2.0, 2.0)), Real::from(rng.f(0.0, 4.0)), Real::from(rng.f(-2.0, 2.0))], true);
        }
        checkpoints.push(w.hash_full()); // post-user-mutation, pre-step
        journals.push(w.step());
    }
    for (j, expect) in journals.into_iter().rev().zip(checkpoints.into_iter().rev()) {
        w.revert(j);
        assert_eq!(expect, w.hash_full(), "fuzzer: pre-step state must restore exactly at every tick");
    }
}

#[test]
fn journal_size_scales_with_activity_not_world_size() {
    let saved_counts = |chunks: i32| -> (usize, usize) {
        let mut w = World::new();
        for x in 0..chunks { for z in 0..chunks { add_floor(&mut w, x, z); } }
        let b = w.bodies.insert(RigidBodyBuilder::dynamic()
            .translation(vector![Real::from(2.0), Real::from(5.0), Real::from(2.0)]).build());
        w.colliders.insert_with_parent(ColliderBuilder::ball(Real::from(0.4)).build(), b, &mut w.bodies);
        for _ in 0..5 { let _ = w.step(); } // settle inserts
        let j = w.step(); // one falling ball, mid-air
        (j.saved_bodies.len(), j.saved_colliders.len())
    };
    let small = saved_counts(2);
    let big = saved_counts(8);
    assert_eq!(small, big, "per-tick saved bodies/colliders must not scale with chunk count");
}
```

(For this test `saved_bodies`/`saved_colliders` need `pub` read access — make the fields `pub` or add `pub fn saved_body_count()`/`saved_collider_count()` accessors on `StepJournal`.)

- [ ] **Step 2: Run — both PASS** (they should already, given Tasks 3–5; a fuzzer failure is a missed hook — fix in the fork, never by weakening the test).

- [ ] **Step 3: Flip `PhysicsController::on_tick` (crates/game/src/physics.rs)**

```rust
    fn on_tick<'a>(&mut self, data: &mut Undo<crate::GameData>) {
        // Journaled step: one tier-2 log entry whose payload is the exact
        // per-tick delta (StepJournal), replacing the whole-PhysicsState
        // snapshot. The scope's registration license covers step()'s writes;
        // the undo closure's raw_undo_parts license covers the revert's.
        let mut journal = StepJournal::default();
        let mut scope = data.physics.undo_scope();
        {
            let p = scope.raw_fields();
            self.pipeline.step_journaled(
                p.gravity,
                p.integration_parameters,
                p.islands,
                p.broad_phase,
                p.narrow_phase,
                p.bodies,
                p.colliders,
                p.implules_joint_set,
                p.multi_body_joint_set,
                p.ccd_solver,
                &(),
                &(),
                &mut journal,
            );
        }
        scope.register(move |phys, _| {
            let p = phys.raw_undo_parts();
            journal.revert(p.islands, p.broad_phase, p.narrow_phase, p.bodies,
                           p.colliders, p.implules_joint_set, p.multi_body_joint_set);
        });

        for handle in data.physics.islands.active_bodies() {
            // ... existing camera-update loop unchanged ...
        }
    }
```

(`step_journaled` takes `&Vector`/`&IntegrationParameters` for the first two params — pass `&*p.gravity`, `&*p.integration_parameters` if the raw view yields `&mut`. `use rapier3d::prelude::StepJournal;` at the top.)

- [ ] **Step 4: Full battery — the rollback hash panic is the completeness oracle**

Run: `cargo test -p game && cargo test -p client`
Expected: ALL PASS — in particular `random_ops` (seeded rollbacks now revert ticks via the journal) and the multi-client suite. Any "Hash verification failed" panic here is a missed capture site in the fork: diagnose by which component's hash diverges (temporarily hash each `PhysicsState` field separately in the failing test), fix the hook, re-run. Do not paper over with a snapshot.

- [ ] **Step 5: Perf + memory evidence**

Run the tick-cost regression test from the hashing branch (`cargo test -p game --test voxels_hash_cache`) — must still pass. Record in the task report: undo-log growth for 100 ticks idle vs. 100 ticks with one moving player, before (snapshot) vs. after (journal) — e.g. via `journal.saved_body_count()` sums or a quick `std::mem::size_of_val` tally in a scratch test; numbers go in the report, not CI.

- [ ] **Step 6: Commit**

```bash
git add crates/game/src/physics.rs crates/game/tests/step_journal.rs crates/rapier/src/pipeline/step_journal.rs
git commit -m "feat: physics step rolls back via StepJournal — per-tick snapshots eliminated"
```

---

### Task 7: Docs, live smoke, wrap-up

**Files:**
- Modify: `docs/superpowers/specs/2026-07-03-undo-api-redesign-design.md` (the phase-4 "per-tick change() snapshot stays" resolution is superseded — point to the step-journal spec)
- Modify: `CLAUDE.md` (one line in the vendored-forks section: forks also carry the StepJournal rollback machinery)

- [ ] **Step 1: Update the two docs** — in the undo-api spec, replace the physics-step resolution paragraph with: "Superseded 2026-07-04: `step()` now logs a `StepJournal` (exact per-tick delta captured inside the fork) instead of a whole-`PhysicsState` snapshot — see `2026-07-04-physics-step-journal-design.md`. Collider attach still snapshots (rare)."

- [ ] **Step 2: Live smoke (server + one client, per testing convention)**

Run server and client (`scripts/run.sh` flow), move around, place/remove a voxel, watch for reconcile stalls or hash panics in either log (debug build → verification ON). Expected: smooth play, no panics, no growing memory on the client.

- [ ] **Step 3: Commit + finish**

```bash
git add docs/ CLAUDE.md
git commit -m "docs: step-journal supersedes per-tick physics snapshots; fork notes"
```

Then use superpowers:finishing-a-development-branch (branch: merge/PR decision with the user).

---

## Self-review notes

- Spec coverage: journal invariant (T2–5), choke-point hooks (T3), BVH skip/capture/optimizer (T4), graph inverses + payload saves + wholesale fallback (T5), tier-2 integration + `raw_undo_parts` + hash gating (T1, T6), all spec tests 1–8 (T3: 1-partial/2/3/4 via free-fall+sleep; T4: 1 full + 6; T5: 2 full/5; T6: 7/8), non-goals untouched (collider attach snapshots remain).
- Known judgment calls an implementer may hit: exact `HashSet`/`HashMap` types are parry's deterministic wrappers (`parry3d::utils::{hashset,hashmap}`); field visibility bumps (`pub(crate)`) where the journal reaches into components are expected and should stay minimal; if `Edge<E>`/`Node<N>` lack `Clone` bounds for the undo enum, add `E: Clone`/`N: Clone` bounds on the journaled methods only.
