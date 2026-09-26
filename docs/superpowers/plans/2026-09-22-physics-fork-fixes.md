# Physics Fork Fixes (pre-upgrade) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the upstream fixes that matter to this game in the current nalgebra-era forks (slotmapd, nalgebra, parry, rapier) now, each pinned by a test that later becomes part of the rapier 0.35 upgrade's regression rig.

**Architecture:** Hand-port small upstream diffs into the vendored forks, rewritten in the fork's `Real = OrderedFloat<f32>` idiom (`Real::from(1.0)`, `.0` to reach the raw f32). Every regression test lives in `crates/game/tests/`, because the forks' own test directories don't build (`crates/parry/crates/parry3d/_tests`; nalgebra's tests fail to compile, see memory `workspace-test-nalgebra-fails`). Tests reach the forks through `game::parry` / `game::na` / `rapier3d`.

**Tech Stack:** Rust; vendored forks in `crates/{slotmapd,nalgebra,parry,rapier}`; `just test-one`, `just smoke`, `just test-threads`.

**Spec:** `docs/superpowers/specs/2026-09-22-vendored-deps-update-investigation.md`. Roadmap: `docs/superpowers/plans/2026-09-22-physics-upstream-roadmap.md`. Upstream reference diffs: `docs/superpowers/plans/2026-09-22-physics-upstream-patches/`.

## Global Constraints

- Work on `develop` in the main checkout; never create worktrees (CLAUDE.md).
- The checkout has a lot of the user's uncommitted work, possibly including fork files; check `git status --short crates/` first.
  - Before every commit, run `git diff -- <file>` on each file you touch.
  - Stage only this task's hunks, with `git add -p <file>`.
  - Never `git add -A` or `git add .`, and never `git stash`.
- Never run the whole game/client/server suites. Verify with `just test-one game "--test <target>"`, plus `just smoke` (60 s cap) at the end of each task.
- Tag any new test slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- Rollback bar: `hash(before) == hash(after undo)`, bit-exact.
- Determinism: no new float intrinsics. Use `ComplexField`/`RealField` methods (libm-forced through simba) or `Real` arithmetic, never raw `f32::sin` and friends.
- Task 4 may change sim output. When it does, update hard-coded expected values in tests only after reading why each one moved, and say in the commit message which values moved and why. Parked `SerializedRegion` blobs from an older build are not guaranteed to restore afterwards; there's no persistent store yet, so that's acceptable.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`.

## Not in this plan

- **Generalized pair filter (rapier 0.35: same-body, `ActiveCollisionTypes`, collision groups at pair creation).**
  - Why deferred: the existing tests `fixed_kinematic_pair_is_not_suppressed` and `kinematic_to_dynamic_with_colliders_is_allowed` (`crates/game/tests/pair_filter.rs`) deliberately keep kinematic↔fixed pairs and allow a kinematic body to become dynamic. Filtering those pairs would need upstream's forced re-insertion path, ~5 h of journal-sensitive work in a broad phase that the upgrade throws away.
  - Where it goes: the upgrade gets the filter for free (upgrade plan, Phase 3).
- **Skipped upstream items:**
  - C4/#106: conflicts with our cast filters.
  - The 1e-4 small-TOI fallback: covered by `GRAZING_START_MARGIN`.
  - BVH #419/#409/#403: dead code with `BvhOptimizationStrategy::None`; #403 would also change the snapshot layout.
  - Signed-zero impulses: never reached, since no solver contacts exist between kinematic and fixed bodies.

## Review Focus

1. **A player standing still on flat voxel ground, and walking across chunk and scale seams, after Task 4.** The capsule must not sink, jitter or trip. Run `walking_smoothness` including its slow tests.
2. **A capsule touching terrain exactly (distance 0), rolled back mid-contact (Task 4).** More touching pairs now go to EPA; `step_journal` and `kinematic` must stay bit-exact.
3. **A player crossing a region seam after Task 4.** Handoff re-creates the capsule on the far side against different chunk colliders. Run `just test-threads`.
4. **A shape cast starting exactly touching (TOI 0) while sliding away (Task 4).** Our pre-check deliberately reports `PenetratingOrWithinTargetDist`; C4/#106 is deliberately NOT ported. `shape_cast_grazing` must stay green unchanged.
5. **Client and server computing the same result.** Every port must be pure deterministic arithmetic: the GJK perturbation is seeded by an iteration counter, not randomness. `just test-one game "--test random_ops"` and `--test hash_restore` stay green.

---

### Task 1: slotmapd `clone_from` leak and panic guard (upstream slotmap #137)

**Files:**
- Modify: `crates/slotmapd/src/basic.rs:113-127` (slot-level `clone_from`), `:1013-1017` (map-level)
- Modify: `crates/slotmapd/src/hop.rs:119-133`, `:1016-1020` (same pattern; the vacant field is `free` in hop.rs, check the `Vacant(..)` arm)
- Modify: `crates/slotmapd/src/dense.rs:805-809` (map-level only)
- Modify: `crates/slotmapd/src/util.rs` (add `PanicOnDrop`)
- Test: `crates/game/tests/hash_restore.rs` (append; it already exercises slotmapd)

Reference: `docs/superpowers/plans/2026-09-22-physics-upstream-patches/slotmap-137-clone-from.diff`.

**The bug:** in `(OccupiedMut, Vacant)`, the catch-all arm overwrites the union without dropping the old value, so it leaks. `(VacantMut, Occupied)` is fine.

- [ ] **Step 1: Write the failing test.** Append to `crates/game/tests/hash_restore.rs`:

```rust
/// Upstream slotmap #137: cloning a vacant slot over an occupied one leaked
/// the occupied value (the union was overwritten without a drop).
#[test]
fn slotmap_clone_from_drops_overwritten_values() {
    use slotmapd::{DefaultKey, SlotMap};
    use std::rc::Rc;
    let rc = Rc::new(());
    let mut dst: SlotMap<DefaultKey, Rc<()>> = SlotMap::new();
    dst.insert(rc.clone());
    let mut src: SlotMap<DefaultKey, Rc<()>> = SlotMap::new();
    let k = src.insert(Rc::new(()));
    src.remove(k); // slot 1 vacant in src, occupied in dst
    dst.clone_from(&src);
    assert_eq!(Rc::strong_count(&rc), 1, "overwritten value leaked");
}
```

If `slotmapd` isn't a direct dev-dependency of `game`, use the path `game` re-exports it under (check `crates/game/src/lib.rs` for `pub use slotmapd`), or add `slotmapd = { workspace = true }` to `crates/game/Cargo.toml` `[dev-dependencies]` if a workspace entry exists; otherwise use `{ path = "../slotmapd" }`.

- [ ] **Step 2: Run it and check it fails.** `just test-one game "--test hash_restore slotmap_clone_from"`. Expected: FAIL, `left: 2, right: 1`.

- [ ] **Step 3: Implement.** In `util.rs`:

```rust
/// Aborts (by panicking during unwind → abort) if dropped: guards sections
/// that leave a map inconsistent when a user `Clone` impl panics mid-way.
pub(crate) struct PanicOnDrop(pub &'static str);
impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("{}", self.0);
    }
}
```

In `basic.rs`, replace the slot-level match:

```rust
match (self.get_mut(), source.get()) {
    (OccupiedMut(self_val), Occupied(source_val)) => self_val.clone_from(source_val),
    (VacantMut(self_next_free), Vacant(&source_next_free)) => {
        *self_next_free = source_next_free
    }
    (OccupiedMut(_), Vacant(&next_free)) => unsafe {
        // SAFETY: the slot is occupied, so `value` is the live union field.
        ManuallyDrop::drop(&mut self.u.value);
        self.u = SlotUnion { next_free };
    },
    (VacantMut(_), Occupied(value)) => {
        self.u = SlotUnion {
            value: ManuallyDrop::new(value.clone()),
        }
    }
}
```

In the map-level `clone_from`:

```rust
fn clone_from(&mut self, source: &Self) {
    let guard = PanicOnDrop("abort - a panic during SlotMap::clone_from is unrecoverable");
    self.slots.clone_from(&source.slots);
    self.free_head = source.free_head;
    self.num_elems = source.num_elems;
    core::mem::forget(guard);
}
```

Apply the same pattern to `hop.rs` (slot union field names per that file) and the map-level guard to `dense.rs`. Import `crate::util::PanicOnDrop` in each.

- [ ] **Step 4: Run it and check it passes.** `just test-one game "--test hash_restore"`. Expected: all pass, including the existing revert-exactness tests.

- [ ] **Step 5: Commit.**

```bash
git add crates/slotmapd/src/{basic,hop,dense,util}.rs crates/game/tests/hash_restore.rs
git commit -m "fix(slotmapd): drop the overwritten value in Slot::clone_from (upstream slotmap #137)"
```

---

### Task 2: nalgebra SymmetricEigen 2×2 deflation basis (upstream nalgebra #1210)

**Files:**
- Modify: `crates/nalgebra/src/linalg/symmetric_eigen.rs:208-211`
- Create: `crates/game/tests/nalgebra_fixes.rs`

Reference: `.../nalgebra-1210-symmetric-eigen.diff`.

This file is byte-identical to upstream 0.34.1, so the generic code ports verbatim. Nothing in the game reaches it today:
- kinematic bodies have one collider each;
- fixed bodies skip mass properties.

It's insurance for when multi-collider dynamic bodies arrive. It doesn't change any current hash.

- [ ] **Step 1: Write the failing test.** Create `crates/game/tests/nalgebra_fixes.rs`:

```rust
//! Upstream nalgebra fixes carried in the vendored fork.
use game::na::Matrix4;

/// nalgebra #1109 / PR #1210: the 2×2 deflation basis lost all precision
/// when λ0 ≈ d1, so `recompose()` no longer reproduced the input.
#[test]
fn symmetric_eigen_regression_issue_1109() {
    let m = Matrix4::new(
        -19884.07f64, -10.07188, 11.277279, -188560.63,
        -10.07188, 12.518197, 1.3770627, -102.97504,
        11.277279, 1.3770627, 14.587362, 113.26099,
        -188560.63, -102.97504, 113.26099, -1788112.3,
    );
    let eig = m.symmetric_eigen();
    let diff = (m.lower_triangle() - eig.recompose().lower_triangle()).abs().max();
    let scale = m.abs().max();
    assert!(diff <= 1.0e-5 * scale, "recompose error {diff} (scale {scale})");
}
```

This uses f64 on purpose: plain `f64` is still a valid nalgebra scalar. If `abs()`/`max()` aren't available on the fork's matrix, use `.iter().map(|x| x.abs()).fold(0.0, f64::max)`.

- [ ] **Step 2: Run it and check it fails.** `just test-one game "--test nalgebra_fixes"`. Expected: FAIL with a large recompose error.

- [ ] **Step 3: Implement.** Replace the `let basis = Vector2::new(...)` at `symmetric_eigen.rs:208`:

```rust
// Choose the basis least likely to experience cancellation (nalgebra #1210).
let basis = if (eigvals.x.clone() - diag[start + 1].clone()).abs()
    > (eigvals.x.clone() - diag[start].clone()).abs()
{
    Vector2::new(
        eigvals.x.clone() - diag[start + 1].clone(),
        off_diag[start].clone(),
    )
} else {
    Vector2::new(
        off_diag[start].clone(),
        eigvals.x.clone() - diag[start].clone(),
    )
};
```

`T: ComplexField` here, so `.abs()` returns `T::RealField`, which is `PartialOrd`. If the fork's `MinMax` split causes a bound error, use `.norm1()`, which the surrounding code already uses on the same values.

- [ ] **Step 4: Run it and check it passes.** Run `just test-one game "--test nalgebra_fixes"` (PASS), then `just test-one game "--test collider_genesis"` (mass props unchanged, PASS).

- [ ] **Step 5: Commit.** `git add crates/nalgebra/src/linalg/symmetric_eigen.rs crates/game/tests/nalgebra_fixes.rs`, then `git commit -m "fix(nalgebra): stable 2x2 deflation basis in SymmetricEigen (upstream #1210)"`.

---

### Task 3: Pin the voxels fixes for parry #404 and #373 (fixes already committed)

**Files:**
- Read only: `crates/parry/src/query/contact_manifolds/contact_manifolds_voxels_shape.rs` (~:134, `shape2.compute_aabb(pos12).loosened(prediction)`, committed in 6736620)
- Create: `crates/game/tests/voxels_upstream.rs`

Upstream fixed the same bugs in parry PR #437 (issues #404 and #373). Our fork already has both fixes:
- #404 is in `contact_manifolds_voxels_shape.rs:134`, committed in 6736620;
- #373 is in `shape_cast_voxels_shape.rs:187/228/246`, committed in 7b7eb9e / 4173c4e.

This task only pins them.

- [ ] **Step 1: Confirm the fix is already committed.** It went in with 6736620 ("commit all outstanding work in progress"): `git log --oneline -1 -- crates/parry/src/query/contact_manifolds/contact_manifolds_voxels_shape.rs` must show it, and `git diff` on the file must be empty. This task only adds the pinning tests.

- [ ] **Step 2: Write the tests.** Create `crates/game/tests/voxels_upstream.rs`:

```rust
//! parry #404 / #373 (upstream PR #437): voxels contact manifolds see
//! speculative contacts within the prediction distance, and voxels shape
//! casts report witness1 in shape1's frame. Both fixed locally first.
use game::parry::math::{Isometry, Point, Real, Vector};
use game::parry::query::{self, ShapeCastOptions};
use game::parry::shape::{Ball, Capsule, Voxels};

fn r(x: f32) -> Real { Real::from(x) }

fn floor(nx: i32, nz: i32) -> Voxels {
    let pts: Vec<Point<i32>> = (0..nx)
        .flat_map(|x| (0..nz).map(move |z| Point::new(x, 0, z)))
        .collect();
    Voxels::new(Vector::repeat(r(1.0)), &pts)
}

#[test]
fn voxels_manifold_sees_contacts_within_prediction() {
    let vox = floor(8, 8);
    let cap = Capsule::new_y(r(0.3), r(0.2));
    // Capsule bottom at y = 1.55 - 0.5 = 1.05; floor top at y = 1.0 → gap 0.05 < prediction 0.1.
    let pos_cap = Isometry::translation(r(4.0), r(1.55), r(4.0));
    let c = query::contact(&Isometry::identity(), &vox, &pos_cap, &cap, r(0.1))
        .unwrap()
        .expect("contact within prediction must be reported");
    assert!((c.dist.0 - 0.05).abs() < 1.0e-3, "dist {}", c.dist.0);
}

#[test]
fn voxels_shape_cast_witness1_is_in_shape1_frame() {
    let pts: Vec<Point<i32>> = (0..20).map(|x| Point::new(x, 0, 0)).collect();
    let vox = Voxels::new(Vector::repeat(r(1.0)), &pts);
    let ball = Ball::new(r(0.5));
    let pos_ball = Isometry::translation(r(17.5), r(3.5), r(0.5));
    let hit = query::cast_shapes(
        &Isometry::identity(), &Vector::zeros(), &vox,
        &pos_ball, &Vector::new(r(0.0), r(-1.0), r(0.0)), &ball,
        ShapeCastOptions::with_max_time_of_impact(r(10.0)),
    ).unwrap().expect("hit");
    assert!((hit.time_of_impact.0 - 2.0).abs() < 1.0e-4, "toi {}", hit.time_of_impact.0);
    let w = hit.witness1;
    assert!((w.x.0 - 17.5).abs() < 1.0e-3 && (w.y.0 - 1.0).abs() < 1.0e-3, "witness1 {w:?}");
}
```

Adjust the constructor calls to the fork's API before running: check the `Voxels::new` signature in `crates/parry/src/shape/voxels/voxels.rs` and the `contact`/`cast_shapes` signatures in `crates/parry/src/query/`. The *geometry and expected values* are the contract; the call syntax is not.

- [ ] **Step 3: Run the tests.** `just test-one game "--test voxels_upstream"`. Expected: PASS with the uncommitted fix. To prove the #404 test bites, temporarily revert the `.loosened(prediction)` line and confirm `voxels_manifold_sees_contacts_within_prediction` FAILS, then restore it. Do this by hand-editing; do not use `git checkout`/`stash`, which would drop other uncommitted work.

- [ ] **Step 4: Commit** the new test file only. Message: `test(parry): pin voxels prediction-distance contacts (#404) and cast witness frame (#373)`.

---

### Task 4: parry GJK/EPA fixes (upstream PR #437 + #420 subset)

**Files:**
- Modify: `crates/parry/src/query/gjk/gjk.rs`: `closest_points` (:353-450), `minkowski_ray_cast` last-chance exit (:716) and final containment test (:783)
- Modify: `crates/parry/src/query/epa/epa3.rs`: `closest_points` (~:445-560), `FaceId::new` (:24)
- Create: `crates/game/tests/gjk_upstream.rs`

**Reference diffs** in `docs/superpowers/plans/2026-09-22-physics-upstream-patches/`:

| Diff | Fix |
|---|---|
| `parry-171e99b1d5.diff` | #396 stall perturbation |
| `parry-a9ec9f04b2.diff` | #193 stall classification |
| `parry-762e1613f9.diff` | #315 EPA single-point bootstrap |
| `parry-6eb9edc1c5.diff` | #429 TOI refinement |
| `parry-79833ff91b.diff` | #180 scaled containment tolerance |
| `parry-380dc0c757.diff` | #70 test |
| `parry-pr437-full.diff` | the whole PR, for context |

The upstream code uses glam (`Vector::ZERO`, `.length()`, `Pose`); translate it to the fork's nalgebra/OrderedFloat idiom:

| Upstream (glam) | Fork (nalgebra + `OrderedFloat`) |
|---|---|
| `.length()` | `.norm()` |
| `dot(x)` | `.dot(&x)` |
| `Vector::X` | `Vector::x()` |
| `CsoPoint` | `CSOPoint` |
| float literals | `Real::from(..)` |

**Deliberately NOT ported:**
- C4 / #106 (`parry-febcea4f1f.diff`): it breaks our separated-start cast filters. See Review Focus 4.
- The 1e-5→1e-4 small-TOI change: our `GRAZING_START_MARGIN` covers it.

- [ ] **Step 1: Write the failing tests.** Create `crates/game/tests/gjk_upstream.rs`, porting the upstream tests from the diffs:
  - **#193** (`a9ec9f04b2`): capsule and ball sunk 0.01–0.2 into a `Cuboid(20,1,20)` whose top face is at y=0. Cast with `vel1 = (0.1, 1, 0)` and `compute_impact_geometry_on_penetration: true`. Assert `normal2 ≈ +Y` and `normal1 ≈ -Y` within 1e-4.
  - **#396** (`171e99b1d5`): skip the cylinder cases, since the game has no cylinders. Keep `coaxial_cylinder_capsule_intersect` only if `Cylinder` exists in the fork. Otherwise port the capsule/cuboid variant: `Capsule` segment (0,±50,0), r=10, inside `Cuboid(50,100,50)` at identity; `intersection_test` must be true. Also keep the separated guard: the same shapes offset by dy=161 must give false.
  - **#429** (`6eb9edc1c5`): Ball(0.5166) cast straight down onto `Cuboid(h, 0.5, h)` for h ∈ {5, 50}, over 50 start heights in [1, 3] × 4 xz positions. Assert TOI error ≤ 2e-4 against the analytic `start_y - 0.5 - 0.5166`. Tag it slow if it exceeds 100 ms.
  - **#70 / #180** (`380dc0c757`, `79833ff91b`): port `issue_70_capsule_cuboid_false_negatives` verbatim in the fork's idiom.
  - **#315** (`762e1613f9`): two cuboids exactly touching face to face, ball-on-cuboid exactly touching. `contact(.., prediction 0)` must return witnesses on the shared face (not the origin), with normal ±Y.

- [ ] **Step 2: Run them and check they fail.** `just test-one game "--test gjk_upstream"`. Expected: #193, #429 (h=50) and #315 FAIL. Record which ones do; any that already pass stay as guards.

- [ ] **Step 3: Implement C1 (#396 + #193) in `gjk::closest_points`.** Before the loop:

```rust
// Tightest lower bound on the origin→CSO distance over the directions probed:
// > 0 separated, ~0 touching, < 0 bounds the penetration (upstream #193/#396).
let mut best_min_bound = -Real::max_value();
let mut support_scale = Real::from(0.0);
let mut nb_perturbs = 0usize;
const MAX_PERTURBATIONS: usize = 2 * DIM;
```

After `let min_bound = -dir.dot(&cso_point.point.coords);`:

```rust
best_min_bound = best_min_bound.max(min_bound);
support_scale = support_scale.max(cso_point.point.coords.norm());
```

Both stall exits get the same three-way classification. These are the `max_bound >= old_max_bound` branch and the `!simplex.add_point(..)` branch. The stagnation branch shown here uses `result(simplex, true)` and `old_dir`; the add-point branch uses `result(simplex, false)` and `dir`:

```rust
if best_min_bound > Real::from(0.0) {
    if exact_dist {
        let (p1, p2) = result(simplex, true);
        return GJKResult::ClosestPoints(p1, p2, old_dir);
    } else {
        return GJKResult::Proximity(old_dir);
    }
} else if exact_dist && best_min_bound >= -_eps_rel * support_scale {
    let (p1, p2) = result(simplex, true);
    return GJKResult::ClosestPoints(p1, p2, old_dir); // touching
} else if nb_perturbs < MAX_PERTURBATIONS {
    nb_perturbs += 1;
    dir = perturbed_dir(dir, nb_perturbs);
    // continue the loop with the perturbed direction (mirror upstream's control flow)
} else {
    return GJKResult::Intersection; // hand over to EPA
}
```

Match upstream's exact control flow from `a9ec9f04b2.diff`: where the perturbation re-enters the loop, and that `max_bound` is reset so the next iteration doesn't immediately re-stall. Add the helper:

```rust
/// Deterministic perturbation (seeded by the retry count, never random).
fn perturbed_dir(dir: Unit<Vector<Real>>, seed: usize) -> Unit<Vector<Real>> {
    let axis = (seed - 1) % DIM;
    let sign = if ((seed - 1) / DIM) % 2 == 0 { Real::from(1.0) } else { Real::from(-1.0) };
    let mut v = dir.into_inner();
    v[axis] += sign * Real::from(1.0e-2);
    Unit::try_new(v, Real::from(0.0)).unwrap_or(dir)
}
```

- [ ] **Step 4: Implement #315 in `epa3.rs`.**
  - Right after the simplex vertices are pushed, bootstrap a second vertex when `simplex.dimension() == 0`:
    - try the support points along ±x, ±y, ±z (`CSOPoint::from_shapes(pos12, g1, g2, &Vector::x_axis())`, etc.);
    - take the first one farther than `gjk::eps_tol()` from `vertices[0]`;
    - if none qualifies, return `Some((v.orig1, v.orig2, Vector::y_axis()))`.
  - Delete the old `dimension() == 0` early return.
  - Change the `simplex.dimension() == 1` branch to `self.vertices.len() == 2`.

  C1 routes more touching pairs into EPA, which is why C1 and #315 ship together.

- [ ] **Step 5: Implement C3 (#429), C2 (#180) and C6 (#415) in `minkowski_ray_cast` and EPA.**
  - **C3:** at the last-chance exit (`gjk.rs:716`), refine `ltoi` with the witnesses, per `6eb9edc1c5.diff`:

    ```rust
    if last_chance && ltoi > Real::from(0.0) {
        let (w1, w2) = result(simplex, simplex.dimension() == DIM);
        let witness_ltoi = (w1 - w2 - ray.origin.coords).dot(&curr_ray.dir);
        if witness_ltoi.0.is_finite() && witness_ltoi > ltoi {
            ltoi = witness_ltoi;
            if ltoi / ray_length > max_time_of_impact {
                return None;
            }
        }
        return Some((ltoi / ray_length, ldir));
    }
    ```

    Check whether `ray.origin` is a `Point` (then `w1 - w2` is a `Vector` and you subtract `ray.origin.coords`) and that `ray_length` and `max_time_of_impact` are in scope under those names. Upstream's 0.25.3-era signature may differ; adapt the names, not the arithmetic.
  - **C2:** at `gjk.rs:783`:

    ```rust
    let scale = support_point.point.coords.norm().max(curr_ray.origin.coords.norm());
    if min_bound >= _eps_tol * scale.max(Real::from(1.0)) {
    ```

  - **C6:** in `epa3.rs`:
    - give `FaceId::new(id, neg_dist)` a `tol: Real` parameter and use `neg_dist > tol`, where it currently compares against `gjk::eps_tol()`;
    - compute `let dist_tol = gjk::eps_tol() * max_vertex_norm.max(Real::from(1.0));` once from the simplex vertices;
    - pass `dist_tol` at every call site (`:486-501`, `:535`, `:623`).

    Upstream reference: parry PR #420 (0.28); the tolerance code is visible in context in `parry-762e1613f9.diff` around "Tolerance used to reject degenerate faces".

- [ ] **Step 6: Run the new tests and check they pass.** `just test-one game "--test gjk_upstream"`: PASS.

- [ ] **Step 7: Run the movement regression rig, including the slow tests.**

```bash
just test-one game "--test shape_cast_grazing"
just test-one game "--test walking_smoothness -- --include-ignored"
just test-one game "--test jump_arc --test sunk_body --test kinematic --test step_journal"
just test-one game "--test slime --test random_ops --test hash_restore"
just test-threads
```

Expected: all PASS.
- A hard-coded expected value may move. Before updating it, read the failure and confirm the new value is *more* correct: TOI closer to analytic, normal closer to the face normal.
- If `shape_cast_grazing` or `walking_smoothness` regresses in *behaviour* (sinking, tripping, missed floor), stop. Bisect by reverting one of C1/#315/C3/C2/C6 at a time, and report which fix interacts with our local shape-cast patches.

- [ ] **Step 8: Run `just smoke`.** Expected: PASS within 60 s warm.

- [ ] **Step 9: Commit.** Commit C1+#315 together and C3/C2/C6 separately, each with its tests, e.g.:
  - `fix(parry): classify GJK stalls by the best lower bound; bootstrap EPA from a touching point (upstream #193 #396 #315)`
  - `fix(parry): refine last-chance shape-cast TOI with the simplex witnesses (upstream #429)`
  - `fix(parry): scale GJK/EPA tolerances with coordinate magnitude (upstream #180 #415)`

---

### Task 5: Record what landed

**Files:**
- Modify: `docs/superpowers/specs/2026-09-22-vendored-deps-update-investigation.md` (append a "Landed" section: one line per task with commit hash)
- Modify: `CLAUDE.md` "Vendored Forks" paragraph: add one sentence that the forks carry hand-ported upstream fixes (GJK/EPA stall classification, TOI refinement, EPA touching bootstrap), with the list in the spec.

- [ ] **Step 1: Write both edits.**
- [ ] **Step 2: Commit.** `docs: record upstream fixes ported into the physics forks`.
