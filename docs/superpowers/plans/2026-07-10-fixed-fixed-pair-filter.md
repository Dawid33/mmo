# Fixed-Fixed Broad-Phase Pair Filter (narrow-phase fix 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop creating broad-phase pairs (and therefore narrow-phase interaction-graph edges) for collider pairs that can never produce narrow-phase work — both bodies fixed, no `FIXED_FIXED` opt-in — shrinking the per-region contact graph from ~4,884 edges to ~29 (measured, `worldgen/examples/narrow_phase_probe.rs`).

**Architecture:** One new predicate in the vendored rapier fork's BVH broad phase, `BroadPhaseBvh::pair_permanently_dead`, evaluated inside the BVTT `pairs_collector` before a pair is inserted into the `pairs` map / emitted as `AddPair`. The predicate is exactly the narrow phase's `active_collision_types` gate (`compute_contacts` / `compute_intersections`, which already treat these pairs as dead every tick) evaluated once at pair-creation time. Safety hatch: `RigidBody::set_body_type` panics on a fixed → non-fixed transition while colliders are attached, because suppressed pairs have no re-pairing path (this is the blind spot every mainstream engine guards with re-pair machinery — Box2D `TouchProxy`, PhysX `resetFiltering`, Jolt `SetObjectLayer` — which we deliberately do NOT build until something needs it; the game never changes body types today).

**Why this is safe (2026-07-10 research):** Filtering static-static pairs before pair creation is the default behavior of Box2D (`b2Body::ShouldCollide`), Bullet (default filter masks), PhysX (default filter shader always drops static-static), and Jolt (NON_MOVING layer filter). Upstream rapier is the outlier: its broad phases are body-type-agnostic and its `ActiveCollisionTypes` gate (default excludes `FIXED_FIXED` since v0.9) merely `pair.clear()`s the already-created edge. Plain rapier pays ~nothing for a resident dead edge; this rollback fork journals, wholesale-clones, digests, and hashes every edge, so creation-time filtering implements the semantics rapier already promises. Scene queries/raycasts never consult contact pairs and are unaffected.

**Tech Stack:** Rust, vendored `rapier3d` fork (`crates/rapier`), `crates/game` test harness.

## Global Constraints

- Never disable or weaken hash verification (user directive). This plan changes hashed state *content* deterministically (fewer pairs/edges on both peers), never verification coverage.
- Rollback correctness bar: `hash(before) == hash(after undo)`, bit-exact (CLAUDE.md). The client convergence harnesses (`cargo test -p client`, `HashMode::Full`) are the end-to-end gate.
- Determinism: the filter must read only rolled-back simulation state (collider flags, parent handles, body types) — no wall clock, no iteration-order dependence. Both peers run the same fork, so the suppressed set is identical.
- Hash *values* shift (pairs map + interaction graphs shrink). Fine: hashes are only compared between peers running the same build; nothing persists hashes across builds.
- Vendored forks only — never switch `crates/rapier`/`crates/parry` to crates.io versions.
- `just test` while iterating; `just test-full` before declaring done. Tag any new test >100ms with `#[ignore = "slow: >100ms — run via just test-full"]` (the tests below are all sub-100ms: tiny worlds, 1–2 steps).
- Commits end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Scope exclusions (deliberate): the first-tick mass-props spike (69ms/region, separate known issue — skip mass-props for fixed bodies) is NOT this plan; same-parent pair suppression for *dynamic* multi-collider bodies is NOT included (chunk same-parent pairs are both-fixed, so this filter already covers them; dynamic entities have single colliders today); a real fixed→dynamic re-pairing mechanism is NOT built (panic guard instead).

## File Structure

- Modify: `crates/rapier/src/geometry/broad_phase_bvh.rs` — `pair_permanently_dead` predicate + one call in `pairs_collector`.
- Modify: `crates/rapier/src/dynamics/rigid_body.rs` — guard in `set_body_type`.
- Create: `crates/game/tests/pair_filter.rs` — pipeline-level guarantees of the filter, following the `hash_restore.rs` precedent of testing fork behavior from the game crate.

---

### Task 1: Fork — suppress dead fixed-fixed pairs at pair creation

**Files:**
- Modify: `crates/rapier/src/geometry/broad_phase_bvh.rs` (imports at lines 1–10; `pairs_collector` closure at ~line 347; new predicate next to `pairs_insert_or_refresh` at ~line 170)
- Test: `crates/game/tests/pair_filter.rs` (new)

**Interfaces:**
- Consumes: `Collider.parent: Option<ColliderParent>` / `Collider.flags.active_collision_types` (crate-visible fields, same usage as `narrow_phase.rs` `compute_contacts`), `RigidBodySet::get`, `RigidBody.body_type`, `ActiveCollisionTypes::test(RigidBodyType, RigidBodyType)`.
- Produces: `BroadPhaseBvh::pair_permanently_dead(co1: &Collider, co2: &Collider, bodies: &RigidBodySet) -> bool` (private associated fn; Task 2's guard comment references it by name — keep the name).

- [ ] **Step 1: Write the failing tests**

Create `crates/game/tests/pair_filter.rs`:

```rust
//! Fixed-fixed broad-phase pair filter guarantees, driven through the full
//! pipeline (same pattern as hash_restore.rs: fork behavior tested from the
//! game crate, which owns the dev harness).
use rapier3d::prelude::*;

/// One plain (non-journaled) pipeline step over a minimal world.
fn step(
    pipeline: &mut PhysicsPipeline,
    broad: &mut BroadPhaseBvh,
    narrow: &mut NarrowPhase,
    islands: &mut IslandManager,
    bodies: &mut RigidBodySet,
    colliders: &mut ColliderSet,
) {
    let gravity = Vector::y() * Real::from(-9.81);
    let mut impulse_joints = ImpulseJointSet::new();
    let mut multibody_joints = MultibodyJointSet::new();
    let mut ccd = CCDSolver::new();
    pipeline.step(
        &gravity,
        &IntegrationParameters::default(),
        islands,
        broad,
        narrow,
        bodies,
        colliders,
        &mut impulse_joints,
        &mut multibody_joints,
        &mut ccd,
        &(),
        &(),
    );
}

/// Two overlapping fixed cuboids + one dynamic ball overlapping both.
/// Returns (fixed collider A, fixed collider B, ball collider).
fn two_fixed_one_ball(
    bodies: &mut RigidBodySet,
    colliders: &mut ColliderSet,
    fixed_collider: impl Fn() -> ColliderBuilder,
) -> (ColliderHandle, ColliderHandle, ColliderHandle) {
    let a = bodies.insert(RigidBodyBuilder::fixed().build());
    let ca = colliders.insert_with_parent(fixed_collider().build(), a, bodies);

    let b = bodies.insert(
        RigidBodyBuilder::fixed()
            .translation(Vector::new(Real::from(1.5), Real::from(0.0), Real::from(0.0)))
            .build(),
    );
    let cb = colliders.insert_with_parent(fixed_collider().build(), b, bodies);

    let ball = bodies.insert(
        RigidBodyBuilder::dynamic()
            .translation(Vector::new(Real::from(0.75), Real::from(1.2), Real::from(0.0)))
            .build(),
    );
    let cball = colliders.insert_with_parent(
        ColliderBuilder::ball(Real::from(0.5)).build(),
        ball,
        bodies,
    );
    (ca, cb, cball)
}

fn cuboid() -> ColliderBuilder {
    ColliderBuilder::cuboid(Real::from(1.0), Real::from(1.0), Real::from(1.0))
}

#[test]
fn fixed_fixed_pairs_are_not_created() {
    let (mut pipeline, mut broad, mut narrow, mut islands) = (
        PhysicsPipeline::new(),
        BroadPhaseBvh::new(),
        NarrowPhase::new(),
        IslandManager::new(),
    );
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());
    let (ca, cb, cball) = two_fixed_one_ball(&mut bodies, &mut colliders, cuboid);

    step(&mut pipeline, &mut broad, &mut narrow, &mut islands, &mut bodies, &mut colliders);

    // The two fixed cuboids overlap but must have NO interaction-graph edge.
    assert!(narrow.contact_pair(ca, cb).is_none(), "fixed-fixed pair must be suppressed");
    // The dynamic ball must still pair with both fixed cuboids.
    assert!(narrow.contact_pair(ca, cball).is_some(), "dynamic-fixed pair must exist");
    assert!(narrow.contact_pair(cb, cball).is_some(), "dynamic-fixed pair must exist");
    // The whole graph holds exactly the two ball edges.
    assert_eq!(narrow.contact_graph().raw_graph().raw_edges().len(), 2);
}

#[test]
fn fixed_fixed_opt_in_still_pairs() {
    let (mut pipeline, mut broad, mut narrow, mut islands) = (
        PhysicsPipeline::new(),
        BroadPhaseBvh::new(),
        NarrowPhase::new(),
        IslandManager::new(),
    );
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());
    // Opt-in on the colliders: the narrow-phase gate is an OR of the two
    // colliders' flags, so the filter must keep the pair if EITHER opts in.
    // Here both opt in; a one-sided variant is asserted below.
    let opt_in = || {
        cuboid().active_collision_types(
            ActiveCollisionTypes::default() | ActiveCollisionTypes::FIXED_FIXED,
        )
    };
    let (ca, cb, _) = two_fixed_one_ball(&mut bodies, &mut colliders, opt_in);

    step(&mut pipeline, &mut broad, &mut narrow, &mut islands, &mut bodies, &mut colliders);

    let pair = narrow.contact_pair(ca, cb).expect("opted-in fixed-fixed pair must exist");
    assert!(
        pair.has_any_active_contact,
        "overlapping opted-in fixed cuboids must produce actual contacts"
    );
}

#[test]
fn fixed_fixed_opt_in_one_sided_still_pairs() {
    let (mut pipeline, mut broad, mut narrow, mut islands) = (
        PhysicsPipeline::new(),
        BroadPhaseBvh::new(),
        NarrowPhase::new(),
        IslandManager::new(),
    );
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());

    let a = bodies.insert(RigidBodyBuilder::fixed().build());
    let ca = colliders.insert_with_parent(
        cuboid()
            .active_collision_types(
                ActiveCollisionTypes::default() | ActiveCollisionTypes::FIXED_FIXED,
            )
            .build(),
        a,
        &mut bodies,
    );
    let b = bodies.insert(
        RigidBodyBuilder::fixed()
            .translation(Vector::new(Real::from(1.5), Real::from(0.0), Real::from(0.0)))
            .build(),
    );
    // Default flags on this side — the pair must still be kept.
    let cb = colliders.insert_with_parent(cuboid().build(), b, &mut bodies);

    step(&mut pipeline, &mut broad, &mut narrow, &mut islands, &mut bodies, &mut colliders);

    assert!(narrow.contact_pair(ca, cb).is_some(), "one-sided opt-in must keep the pair");
}

#[test]
fn fixed_sensor_with_opt_in_still_intersects_fixed() {
    let (mut pipeline, mut broad, mut narrow, mut islands) = (
        PhysicsPipeline::new(),
        BroadPhaseBvh::new(),
        NarrowPhase::new(),
        IslandManager::new(),
    );
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());

    let solid = bodies.insert(RigidBodyBuilder::fixed().build());
    let c_solid = colliders.insert_with_parent(cuboid().build(), solid, &mut bodies);

    let sensor_body = bodies.insert(
        RigidBodyBuilder::fixed()
            .translation(Vector::new(Real::from(0.5), Real::from(0.0), Real::from(0.0)))
            .build(),
    );
    let c_sensor = colliders.insert_with_parent(
        cuboid()
            .sensor(true)
            .active_collision_types(
                ActiveCollisionTypes::default() | ActiveCollisionTypes::FIXED_FIXED,
            )
            .build(),
        sensor_body,
        &mut bodies,
    );

    step(&mut pipeline, &mut broad, &mut narrow, &mut islands, &mut bodies, &mut colliders);

    assert_eq!(
        narrow.intersection_pair(c_sensor, c_solid),
        Some(true),
        "opted-in fixed sensor vs fixed solid must intersect"
    );
}
```

Note for the implementer: `Real` is `OrderedFloat<f32>` under `enhanced-determinism` — keep the `Real::from(...)` conversions exactly as written. If `two_fixed_one_ball`'s `&mut bodies` vs `bodies` parameter plumbing trips the borrow checker, pass `bodies: &mut RigidBodySet` through consistently (`insert_with_parent` takes `&mut RigidBodySet`).

- [ ] **Step 2: Run tests to verify current failure mode**

Run: `cargo test -p game --test pair_filter`
Expected: `fixed_fixed_pairs_are_not_created` FAILS (the fixed-fixed pair currently exists and the edge count is 4: A-B, A-ball, B-ball, plus nothing else — the assert_eq of 2 fails and the `is_none` fails). The three opt-in/sensor tests PASS already (upstream semantics) — they are regression gates pinning what the filter must NOT break.

- [ ] **Step 3: Implement the filter**

In `crates/rapier/src/geometry/broad_phase_bvh.rs`:

1. Extend the imports (lines 5–8):

```rust
use crate::dynamics::{IntegrationParameters, RigidBodySet, RigidBodyType};
use crate::geometry::{
    Aabb, BroadPhasePairEvent, Collider, ColliderHandle, ColliderPair, ColliderSet,
};
```

2. Add the predicate next to `pairs_insert_or_refresh` (after ~line 193):

```rust
    /// True iff a candidate broad-phase pair can never produce narrow-phase
    /// work under its CURRENT state: both colliders sit on fixed bodies (no
    /// parent counts as fixed, mirroring the narrow phase's
    /// `unwrap_or(RigidBodyType::Fixed)`) and neither collider opts into
    /// fixed-fixed interaction via `ActiveCollisionTypes::FIXED_FIXED`. This
    /// is exactly the active-collision-types gate of `compute_contacts` /
    /// `compute_intersections`, evaluated once at pair-creation time so the
    /// dead pair never becomes a resident interaction-graph edge. In voxel
    /// scenes ~99% of AABB overlaps are adjacent fixed chunk colliders, and
    /// every resident edge is journaled, wholesale-cloned and hashed by the
    /// rollback layer — suppression here shrinks all of those.
    ///
    /// Blind spots, guarded or documented:
    /// - fixed -> non-fixed body type change: the suppressed pairs would be
    ///   missing and nothing re-emits them (the leaf never moves, so the
    ///   change-detection BVTT never re-visits it). `RigidBody::set_body_type`
    ///   panics on that transition while colliders are attached — re-create
    ///   the body instead.
    /// - enabling FIXED_FIXED (or sensor status) at runtime on a collider
    ///   already overlapping another fixed collider will NOT resurrect the
    ///   suppressed pair, for the same no-revisit reason. Construct colliders
    ///   with their final flags; do not toggle these on fixed bodies.
    #[inline]
    fn pair_permanently_dead(co1: &Collider, co2: &Collider, bodies: &RigidBodySet) -> bool {
        let type1 = co1
            .parent
            .and_then(|p| bodies.get(p.handle))
            .map(|rb| rb.body_type)
            .unwrap_or(RigidBodyType::Fixed);
        let type2 = co2
            .parent
            .and_then(|p| bodies.get(p.handle))
            .map(|rb| rb.body_type)
            .unwrap_or(RigidBodyType::Fixed);
        type1 == RigidBodyType::Fixed
            && type2 == RigidBodyType::Fixed
            && !co1.flags.active_collision_types.test(type1, type2)
            && !co2.flags.active_collision_types.test(type1, type2)
    }
```

3. Wire it into `pairs_collector` (~line 347) — bind the collider refs that are currently discarded and bail before the map/event write:

```rust
        let mut pairs_collector = |co1: u32, co2: u32| {
            assert_ne!(co1, co2);

            let Some((co1_ref, mut handle1)) = colliders.get_unknown_gen(co1) else {
                return;
            };
            let Some((co2_ref, mut handle2)) = colliders.get_unknown_gen(co2) else {
                return;
            };

            // Dead-pair suppression (see pair_permanently_dead): skip BEFORE
            // touching the pairs map / pairs_hash / events so a suppressed
            // pair leaves zero trace in hashed state.
            if Self::pair_permanently_dead(co1_ref, co2_ref, bodies) {
                return;
            }

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
```

(`bodies: &RigidBodySet` is already a parameter of `update` and is only read elsewhere in the fn, so the shared borrow into the closure is fine. The GC `pairs.retain` needs no change: suppressed pairs are never in the map, so it can neither retain nor delete them, and stale pre-filter entries cannot exist because pair state never persists across builds.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p game --test pair_filter`
Expected: PASS (all four tests — suppression active, opt-ins and sensor intersection preserved).

- [ ] **Step 5: Run the fork and game fast suites**

Run: `cargo test -p rapier3d --lib && just test`
Expected: PASS. The rollback/convergence tests exercise the journaled step end-to-end; hash values shift but every comparison is same-build. If a game test pins a specific interaction-graph shape (none known), it fails loudly here — fix the expectation, not the filter.

- [ ] **Step 6: Commit**

```bash
git add crates/rapier/src/geometry/broad_phase_bvh.rs crates/game/tests/pair_filter.rs
git commit -m "perf(rapier): suppress dead fixed-fixed pairs at broad-phase pair creation

Both-fixed pairs with no FIXED_FIXED opt-in can never produce narrow-phase
work (the active-collision-types gate clears them every tick), yet each one
became a resident interaction-graph edge that the rollback layer journals,
wholesale-clones and hashes. Evaluate that same gate once in the BVTT pair
collector instead: measured 4884 -> 29 contact-graph edges per region.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Fork — forbid fixed → non-fixed type changes with attached colliders

**Files:**
- Modify: `crates/rapier/src/dynamics/rigid_body.rs` (`set_body_type`, line ~251)
- Test: `crates/game/tests/pair_filter.rs` (append)

**Interfaces:**
- Consumes: `RigidBody.body_type`, `RigidBody.colliders: RigidBodyColliders` (`.0: Vec<ColliderHandle>`, crate-visible — same access as `user_changes.rs`), `pair_permanently_dead` (Task 1, referenced by name in the comment only).
- Produces: no new API — `set_body_type` gains a panic contract: fixed → non-fixed with attached colliders panics with a message containing `"fixed-fixed pairs are suppressed"`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/game/tests/pair_filter.rs`:

```rust
#[test]
#[should_panic(expected = "fixed-fixed pairs are suppressed")]
fn fixed_to_dynamic_with_colliders_panics() {
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());
    let h = bodies.insert(RigidBodyBuilder::fixed().build());
    let _c = colliders.insert_with_parent(cuboid().build(), h, &mut bodies);
    // The broad phase suppressed this body's fixed-fixed pairs at creation;
    // leaving Fixed would need a re-pairing path that does not exist.
    bodies.get_mut(h).unwrap().set_body_type(RigidBodyType::Dynamic, true);
}

#[test]
fn fixed_to_dynamic_without_colliders_is_allowed() {
    let (mut bodies, _colliders) = (RigidBodySet::new(), ColliderSet::new());
    let h = bodies.insert(RigidBodyBuilder::fixed().build());
    // No colliders -> no pairs could have been suppressed -> legal.
    bodies.get_mut(h).unwrap().set_body_type(RigidBodyType::Dynamic, true);
    assert!(bodies.get(h).unwrap().is_dynamic());
}

#[test]
fn kinematic_to_dynamic_with_colliders_is_allowed() {
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());
    let h = bodies.insert(RigidBodyBuilder::kinematic_position_based().build());
    let _c = colliders.insert_with_parent(cuboid().build(), h, &mut bodies);
    // Non-fixed origins never had pairs suppressed; the fork's own
    // physics_pipeline test (line ~1195) also relies on this transition.
    bodies.get_mut(h).unwrap().set_body_type(RigidBodyType::Dynamic, true);
    assert!(bodies.get(h).unwrap().is_dynamic());
}
```

- [ ] **Step 2: Run tests to verify the panic test fails**

Run: `cargo test -p game --test pair_filter fixed_to_dynamic`
Expected: `fixed_to_dynamic_with_colliders_panics` FAILS ("test did not panic as expected"); the two allowed-transition tests PASS.

- [ ] **Step 3: Implement the guard**

In `crates/rapier/src/dynamics/rigid_body.rs`, `set_body_type` (line 251):

```rust
    pub fn set_body_type(&mut self, status: RigidBodyType, wake_up: bool) {
        if status != self.body_type {
            // The broad phase suppresses fixed-fixed pairs at creation time
            // (see BroadPhaseBvh::pair_permanently_dead). A fixed body leaving
            // the Fixed type would need those suppressed pairs re-emitted, and
            // this fork has no re-pairing path (the change-detection BVTT only
            // re-visits moved leaves). Forbid the transition while colliders
            // are attached; remove and re-insert the body/colliders instead.
            assert!(
                self.body_type != RigidBodyType::Fixed || self.colliders.0.is_empty(),
                "set_body_type: fixed -> {status:?} with attached colliders is unsupported: \
                 fixed-fixed pairs are suppressed at the broad phase and cannot be re-created; \
                 remove and re-insert the body instead",
            );
            self.changes.insert(RigidBodyChanges::TYPE);
            self.body_type = status;

            if status == RigidBodyType::Fixed {
                self.vels = RigidBodyVelocity::zero();
            }

            if self.is_dynamic_or_kinematic() && wake_up {
                self.wake_up(true);
            }
        }
    }
```

(Only the `assert!` and its comment are new; the rest of the body is unchanged — shown for placement. `self.colliders.0` is the same crate-visible access `user_changes.rs:117` uses.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p game --test pair_filter && cargo test -p rapier3d --lib`
Expected: PASS. The fork's own `physics_pipeline` test flips kinematic → dynamic (line ~1195) and must stay green — the guard only fires when leaving `Fixed`.

- [ ] **Step 5: Commit**

```bash
git add crates/rapier/src/dynamics/rigid_body.rs crates/game/tests/pair_filter.rs
git commit -m "feat(rapier): panic on fixed -> non-fixed type change with attached colliders

The broad phase suppresses fixed-fixed pairs at creation, and there is no
re-pairing path for a body that leaves Fixed — convert the silent missing-
contacts hole into a loud error. The game never changes body types; when a
future feature needs this, implement leaf re-touch + re-pair instead.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Full suite + measured before/after

**Files:** none (verification only).

- [ ] **Step 1: Full test suite**

Run: `just test-full`
Expected: PASS, including the client/server convergence harnesses (`cargo test -p client`, `HashMode::Full`) — these prove `hash(before) == hash(after undo)` end-to-end with the smaller graph, on both the journaled step and the wholesale narrow-phase snapshot paths.

- [ ] **Step 2: Re-run the probe and record numbers**

Run: `cargo run --release -p worldgen --example narrow_phase_probe`

Baseline (pre-filter, this machine, region (0,0)):
- contact graph after first tick: 198 nodes, 4,884 edges (4,855 fixed-fixed)
- first tick: broad 1.065ms, narrow 2.578ms (user_changes 69.2ms is mass-props, out of scope — expect it unchanged)
- quiescent direct step: narrow 0.019ms of 0.025ms
- removal tick: pre_digest 1.407ms vs 0.572ms step
- remove_entity_safe: 2.92ms

Expected after: ~29 edges (only player-vs-chunk pairs; the 28 same-parent chunk pairs are both-fixed and suppressed too); first-tick broad+narrow well under 1ms combined; quiescent narrow-phase time and the removal-tick `pre_digest` drop by roughly the edge ratio (~170×, so tens of µs); `remove_entity_safe` drops by whatever share of its 2.9ms was narrow-phase clone+hash. Paste the actual numbers into the branch/PR summary next to these baselines.

- [ ] **Step 3: Live smoke**

One server + one client (`just run`; `just adapter=6700 run` on the laptop/eGPU). Join, walk across a region boundary, and dig/place if an edit path is wired. Expected: normal terrain collision (player stands/walks on chunks), no hash-mismatch panics (Delta verification runs every rollback), no visual physics anomalies.

---

## Self-Review Notes

- Spec coverage: filter scoped to strictly Fixed+Fixed (kinematic pairs untouched — they have their own opt-in flags and moving bodies re-pair naturally); `FIXED_FIXED` opt-in honored by mirroring the narrow phase's OR-of-both-colliders gate, which also covers sensors (the intersection path uses the identical gate, verified at `narrow_phase.rs` `compute_intersections`); type-change blind spot converted to a panic; queries unaffected (never used contact pairs); determinism preserved (filter reads only rolled-back state, identically on both peers).
- The filter runs inside `BroadPhaseBvh::update`, whose mutations are already wholesale-journaled (`BroadSaved` snapshot before the first mutation), so no new undo machinery: replaying a tick re-runs the same deterministic filter against the same restored inputs.
- Interaction with the just-landed `pairs_hash` multiset cache: suppression happens before `pairs_insert_or_refresh`, so a suppressed pair touches neither the map nor the cache — the drift `debug_assert` in `journal_save` keeps guarding the invariant.
- Type consistency: `pair_permanently_dead(&Collider, &Collider, &RigidBodySet) -> bool` defined in Task 1, referenced by comment in Task 2; test helpers `step`/`two_fixed_one_ball`/`cuboid` defined once in Task 1's file and reused by Task 2's appended tests; panic message substring in Task 2's `#[should_panic]` matches the `assert!` text.
- Perf claim traceability: every number in Task 3 comes from `worldgen/examples/narrow_phase_probe.rs` output recorded in this session; the probe is committed and re-runnable.
