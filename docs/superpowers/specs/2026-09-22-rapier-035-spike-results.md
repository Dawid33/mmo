# Rapier 0.36 / parry 0.31 upgrade — Phase 1 spike results

Plan: `docs/superpowers/plans/2026-09-22-rapier-035-upgrade.md` (Phase 1, Tasks 1.1–1.3).
Everything here ran against **pristine crates.io** rapier3d 0.36.0 / parry3d 0.31.1 / glamx 0.3.1
(features `enhanced-determinism, serde-serialize`) from `crates/physics-next`. No game crate
depends on it (`cargo tree -p game|client|server|worldgen` shows no 0.36 rapier).

## Decision summary (Task 1.3) — all confirmed by the user, 2026-09-27

| # | Decision | Outcome |
|---|---|---|
| D1 | Rollback mechanism | **Hybrid journal** (option 2: first-touch body/collider capture + per-tick wholesale islands/narrow/broad clones; not the full re-port of today's fine-grained narrow-phase log, not snapshots) |
| D2 | State hashing | **Own zero-copy `StateHash` trait + derive** — *changed at the gate*, see below |
| D4 | Math forks after the switch | **`simba`, `nalgebra`, `approx`, `ordered-float` → crates.io or dropped; no new forks** |
| D5 | How the port lands | **Side-by-side `*-next` crates on `develop`** |
| D6 | `length_unit` | **`UNITS_PER_METER` (16)** as `movement::PHYSICS_LENGTH_UNIT` |
| D7 | KCC depenetration | **Keep `depenetration_lift`; gate #485 off (`depenetrate = false`)** |

**D2 at the gate.** The spike recommended crc32 over buffered bincode (`StateDigest`). The user
rejected it: when the engine was designed, hashing serialized state was measured far slower
than implementing `Hash`, and that is why the `OrderedFloat` math forks exist. The spike's
digest rows compare two serialization variants with each other, never with `Hash`, so they
don't contradict that.

Chosen instead: our own `StateHash` trait with a derive.
- It is the same zero-copy field walk as std `Hash`, but implementable for glam `f32` types (the
  orphan rule allows our trait on foreign types).
- Floats hash by canonicalised bits, matching `OrderedFloat`.
- `usize` is written as `u64`, so native and wasm32 agree.
- It is derived in the rapier/parry forks, with the cached voxels/BVH digests kept as hand impls.
- Cost: ~140 derive lines in the forks.

Task 4.4 was rewritten accordingly, and its cost check compares `StateHash` against today's
`Hash` on the same region. The D2 paragraph below is the spike's original recommendation, kept
for the record.

**D1 — keep the hybrid journal.**
- *Numbers.* At real region scale (2616 terrain bodies, 2956 colliders), the hybrid's per-tick
  wholesale clone of islands + narrow phase + broad phase costs ~12 µs p50 (15 µs p95). Its
  first-touch capture of four moving bodies costs 0.6 µs. A full 7-structure snapshot costs
  ~225 µs p50 plus ~63 µs to drop, and 3.1 MB per tick. That is ~19× the hybrid and 17× the
  physics step it would protect, so the "full snapshot within 2×" condition fails by an order
  of magnitude.
- *Cost of the hybrid.* Complexity. It needs capture hooks in the set accessors, the
  raw-pointer writes in the staged solver (Review Focus 1), and a fuzzer to prove LIFO exactness.
  Snapshots would inherit upstream's byte-exact snapshot contract for free.
- *Memory.* The hybrid retains about the 240 KB broad-phase clone per logged tick. A full
  snapshot retains ~3.1 MB of serialized state per tick, somewhat less as heap because cloned
  `Voxels` shapes are `Arc`-shared.
- *Where the gap comes from.* One fixed body per chunk, which is how the game builds terrain. It
  wouldn't narrow unless terrain moved to far fewer bodies.
- *Side result.* A fresh `PhysicsPipeline` after revert costs nothing measurable (14.9 vs
  13.4 µs), so Task 4.3's fresh-pipeline rule is free.

**D2 (spike recommendation, rejected at the gate) — `StateDigest` (crc32 over bincode) through a buffered writer.**
- *Why bincode.* It is the only canonical form upstream contracts, and it is target-independent.
  Phase 0 showed std `Hash` is not: it writes `usize` at pointer width. `state_hash` already
  differs between native and wasm32, and the server-authored `cell_hash` would make a wasm32
  cell-mode client fail with `CellHashMismatch`. Moving `cell_hash` to `StateDigest` (Task 5.2)
  fixes that as a side effect.
- *Cost depends on buffering.* Run 1 streamed bincode straight into crc32: one
  `crc32fast::update` per primitive field, 5.7 ms for the whole `ColliderSet`. The reviewer
  flagged that as a likely artefact, so run 2 (your go-ahead) added a reusable buffer and a
  single crc32 pass:
  - `ColliderSet` 5.1 → **0.83 ms** p50 (0.97 p95);
  - bodies 2.3 → 0.43 ms;
  - broad phase 0.51 → 0.06 ms;
  - colliders with `Voxels` stand-ins 1.25 → 0.16 ms.

  **The buffered writer is mandatory:** `DigestWriter` in the plan's Task 4.4 becomes "serialize
  into a reused `Vec`, hash once".
- *Gate.* The whole `ColliderSet` at ≤ 1 ms passes, narrowly. By the plan's rule, Task 4.4's
  `DIGEST_MODE` fork patch can be skipped.
- *Trade-off on that skip.* The margin is thin: p95 0.97 ms, max 1.6 ms. Generated terrain has no
  voxel-scale grids yet, and chisel edits add them (up to 32³ cells per chunk), which grows the
  `Voxels` payload. The stand-in rows show the patch buys 5× on colliders (0.83 → 0.16 ms), for a
  `thread_local!` fork patch that must never leak into wire or parking serialization.
- *Recommendation.* Skip the patch in Task 4.4, and add a Full-mode cost check on an edited
  region instead. Take the patch if that check crosses 1 ms. Full mode stays a test/harness
  mode (production `Off`, `Delta` digests only the footprint), so a Full-mode digest per log
  entry of ~1.3 ms at region scale is acceptable.

**D4 — move the math forks to crates.io (or drop them).**
- *Evidence.* rapier 0.36 already resolves crates.io `nalgebra` 0.35 and `simba` 0.10.2 for its
  internals. Our forks are 0.34.1 and 0.9.1 and cannot serve it without being re-forked at the
  new versions. Upstream's `enhanced-determinism` now forces `simba/libm_force`, `glamx/libm` and
  `glamx/scalar-math` by feature, which is what the forks were for, together with the
  `OrderedFloat` scalar that D3 removes. After the switch, the game's own nalgebra use (e.g. the
  camera's `Perspective3`) moves to glam.
- *Costs:*
  - it reverses CLAUDE.md's "don't switch these back to crates.io" rule;
  - a future determinism defect in upstream nalgebra/simba would need a new fork instead of a
    local patch;
  - Plan 1's `nalgebra_fixes.rs` (`SymmetricEigen`) becomes unreachable if nalgebra is no longer
    a direct dependency. Task 5.4 already allows deleting it with a note, and the determinism-
    sensitive eigen path is kept off fixed bodies by Task 3.1's mass-props skip.
- *Keep vendored:* `rapier3d`/`parry3d` (fork patches), `slotmapd` (rollback inverses) and
  `block-mesh`.
- *Recommendation:* yes.

**D5 — land side by side on `develop`.**
- *Evidence.* This phase already did it: `crates/physics-next` sits in the workspace with
  crates.io rapier 0.36. `cargo tree` shows nothing in `game`/`client`/`server`/`worldgen` picked
  it up, and `just smoke` stayed green. Phase 2 does the same with vendored `rapier-next`/
  `parry-next`.
- *Costs:*
  - two physics stacks in one workspace, so more compile time when you build `physics-next`
    (never on the game path);
  - a large vendored import on `develop`;
  - possible confusion between the two `rapier3d`s. Mitigated by the `*-next` package names and
    by never letting a game crate depend on them.
- *The alternative.* A long-lived branch keeps `develop` clean, but it conflicts with the
  no-worktree rule and with your concurrent edits: a multi-week, very large diff would need
  constant rebasing.
- *Recommendation:* side by side.

**D6 — `length_unit = UNITS_PER_METER` (16).**
- *Measured.* No difference for today's world. Walking trajectories are bit-identical at 1 and
  16, the walking capsule's BVH leaf changes every tick at both, and a standing body's leaf
  changes only once in 1200 ticks at both. The character controller reads none of the scaled
  thresholds, and every game body is fixed or kinematic.
- *Why 16 anyway.* The plan's tie-break ("green walking_smoothness with fewer dirty ticks")
  can't separate the two, so the choice comes down to what the value means. At 1.0 with 16
  units per metre:
  - contact prediction is 1.25 mm;
  - allowed penetration and contact recycling are 16× tighter than rapier's tuning intends;
  - sleep thresholds are 16× stricter, so dynamic bodies stay awake longer, which means more
    journal captures.

  None of this bites until the first dynamic bodies arrive (dropped items, buoyancy).
- *Why now.* Setting 16 at the switch absorbs the change inside the one phase that is already
  judged by tolerance goldens, instead of re-baselining later.
- *Trade-off.* It's a second change riding on the engine switch. If a golden diverges at
  Task 5.4, `length_unit` is one more suspect to rule out, though the probe says it cannot move
  the KCC.
- *Fallback.* Keeping 1.0 (zero change) is the safe alternative, if you'd rather keep the switch
  a pure engine swap.

**D7 — keep `depenetration_lift`, and gate #485 off (the plan's `depenetrate` flag).** Confirmed.
- *Where #485 does nothing.* Upstream's zero-motion depenetration does nothing on voxel terrain:
  parry 0.31.1 has no `contact` for `Voxels`, and it skips the `Unsupported` error.
- *Where it matches the lift.* On a full-chunk cuboid with a body sunk 0.3, it lifts straight up
  to the skin gap, which is exactly what our lift does. `sweep_character` applies the lift before
  `move_shape`, so there #485 then finds nothing to do.
- *Where they disagree (found in review).* The case the lift deliberately declines: a body
  buried deeper than `STEP_HEIGHT`, or one that a lift wouldn't clear. The lift's policy is
  "a buried body stays put, never teleported up through what buried it". #485 then pushes the
  body out along the contact normal, by up to a quarter of the character height per call. The
  probe `kcc_zero_motion_moves_a_body_the_lift_leaves_buried` has it lift a body buried 10.6
  units straight up by 7.2 (the cap). It happens only on cuboid chunks, so behaviour would
  differ between cuboid and voxel ground.
- *Recommendation.* Keep the lift, and take the plan's Task 3.4 patch: a
  `KinematicCharacterController::depenetrate: bool` (default `true`, upstream behaviour), set to
  `false` by the game.
- *Cost.* One small `// mmo-fork:` patch. It's also the only way #485's behaviour stays
  independent of whether a future parry adds `Voxels` contact.
- *The lift keeps working upstream:* `intersect_shape` is implemented for `Voxels` in 0.31.1.

**Also new for the re-port (Tasks 3.2–3.4):** on a bare flat voxel floor, upstream's KCC dips
~1.2 units just before the seams between chunk *bodies* while walking, never at voxel borders
inside a body. That's the walking-smoothness defect our fork fixed, now pinned by the ignored
`walk_on_flat_voxel_floor_keeps_its_height`. The probe doesn't isolate which fix it needs: the
voxels cast (3.2), the cross-collider `cast_shape_on_continuous_surfaces` (3.3; upstream
`move_shape` doesn't use it) or the snap gate (3.4). Expect its ignore to come off only after
all three.


## Task 1.1 — clone and digest cost at real region scale

**Input.** `cargo run --release -p worldgen --example export_region_colliders` exports region
(0, 0) through the legacy, fully materialized genesis path (`Region::from_columns` →
`seed_terrain_genesis`; default `REGION_CHUNKS` = 32, cells off, `WORLD_SEED`):

| | count |
|---|---|
| terrain bodies (one fixed body per chunk, empty chunks included) | 2616 |
| full-chunk cuboid collapses | 1328 |
| `Voxels` grids at block scale (16) | 985 (3932 solid cells) |
| `Voxels` grids at half-block scale (8) | 643 (8324 solid cells) |
| `Voxels` grids at voxel scale (1) | 0 — generated terrain has none; they come only from chisel edits |

**Rig.** `physics-next/examples/clone_cost.rs` rebuilds that set on rapier 0.36 (the game's
default `IntegrationParameters`, `dt` = one 50 ms tick, zero gravity, empty `SoftBodySet`) plus
four kinematic player capsules walking +x at `WALK_SPEED` on the surface. It then times 600
ticks. The "crc32(bincode)" rows stream bincode through a non-allocating writer into crc32,
which is the planned `StateDigest` shape.

**Run** (2026-09-26, user's go-ahead): release, `taskset -c 2`, AC power, core 2 at 3.3–3.5 GHz
before/after (no power clamp). Build 9.1 ms. First step, i.e. the broad phase over all terrain:
7.5 ms. Max active bodies: 4.

| µs per tick | p50 | p95 | max | mean | bincode bytes |
|---|---|---|---|---|---|
| step (warm pipeline) | 13.4 | 27.6 | 53.9 | 15.6 | |
| step on a fresh `PhysicsPipeline` (every 50th tick) | 14.9 | 23.6 | 42.2 | 16.4 | |
| clone bodies | 139.4 | 271.9 | 836.0 | 165.6 | 1 140 013 |
| clone colliders | 71.8 | 124.1 | 487.1 | 81.8 | 1 739 409 |
| clone islands | 0.8 | 1.3 | 4.8 | 0.8 | 300 |
| clone narrow_phase | 2.1 | 3.1 | 29.1 | 2.3 | 1 138 |
| clone broad_phase | 18.8 | 31.4 | 81.3 | 21.0 | 239 830 |
| clone impulse_joints | 0.2 | 0.4 | 1.0 | 0.2 | 97 |
| clone multibody_joints | 0.4 | 0.7 | 1.7 | 0.4 | 101 |
| **hybrid wholesale: islands + narrow + broad** | **11.9** | **14.6** | 34.7 | 12.4 | 241 268 |
| hybrid first-touch capture: 4 bodies + their colliders | 0.6 | 0.9 | 3.0 | 0.6 | |
| **full 7-structure clone** | **225.1** | **313.9** | 684.5 | 241.5 | 3 120 888 |
| … and dropping that snapshot | 62.9 | 84.9 | 185.5 | 66.2 | |
| crc32(bincode) bodies | 2360.8 | 2519.0 | 3050.5 | 2368.6 | 1 140 013 |
| **crc32(bincode) colliders** | **5744.0** | 6234.8 | 7008.2 | 5797.7 | 1 739 409 |
| crc32(bincode) islands | 1.6 | 2.9 | 29.5 | 1.8 | 300 |
| crc32(bincode) narrow_phase | 2.9 | 4.0 | 32.2 | 3.0 | 1 138 |
| crc32(bincode) broad_phase | 513.3 | 569.0 | 758.7 | 516.0 | 239 830 |
| crc32(bincode) impulse_joints | 0.7 | 1.4 | 26.8 | 0.8 | 97 |
| crc32(bincode) multibody_joints | 0.6 | 1.2 | 3.3 | 0.7 | 101 |
| **crc32(bincode) colliders, `Voxels` swapped for a stand-in** | **1264.2** | 1401.0 | 1934.4 | 1277.7 | 578 645 |

**Run 2** (review follow-up, your go-ahead; same pinning, core 2 at 4.4 GHz after). It added
the buffered rows: bincode into a reused `Vec`, then one crc32 pass. Its other rows matched run 1
within ~12% (hybrid 12.4 µs, full clone 224.2 µs, unbuffered colliders 5077 µs).

| µs per tick | p50 | p95 | max | mean | bincode bytes |
|---|---|---|---|---|---|
| buffered crc32(bincode) bodies | 430.7 | 540.9 | 832.8 | 435.9 | 1 140 013 |
| **buffered crc32(bincode) colliders** | **825.9** | 968.7 | 1605.9 | 839.9 | 1 739 409 |
| buffered crc32(bincode) broad_phase | 56.1 | 97.5 | 125.9 | 65.1 | 239 830 |
| buffered crc32(bincode) colliders, `Voxels` stand-ins | 155.8 | 191.2 | 312.3 | 160.3 | 578 645 |

Reading the table:

- **The hybrid's wholesale clone is ~12 µs p50.** The separately timed broad-phase clone (18.8
  µs) is higher than the three-structure row because it runs cache-cold, right after the
  collider clone. Budget ~12–21 µs; both are under the 50 µs gate.
- **A full snapshot is ~19× the hybrid:** 225 µs to clone, 63 µs to drop, 3.1 MB per tick. It is
  also 17× the step itself. Bodies (one fixed body per chunk) and colliders dominate, as the
  plan predicted. The "within 2×" condition for preferring snapshots is nowhere near met.
- **The broad phase is the hybrid's whole cost and memory:** 240 KB per logged tick. With N
  ticks of undo log retained per region, that's N × 240 KB (e.g. 9.6 MB at 40 ticks). Nothing
  smaller exists without a journal inside the BVH.
- **A fresh `PhysicsPipeline` costs nothing measurable:** 14.9 vs 13.4 µs p50. The plan's
  "fresh pipeline after every revert" (Task 4.3) is free.
- **The narrow phase is ~1 KB.** Upstream's default `ActiveCollisionTypes` never creates
  kinematic↔fixed pairs (the plan's Task 3.5 note), and the character controller does its own
  shape casts.
- **Unbuffered digests are slow at this scale.** Streaming bincode field by field into crc32
  (run 1) reaches only ~0.3 GB/s: bodies + colliders + broad phase take ~8.6 ms. Buffered
  (run 2: bincode into a reused `Vec`, one crc32 pass), the same three take **~1.3 ms**. That is
  the per-call overhead of `crc32fast::update`, not bincode.

**D1 gate:** hybrid wholesale ≤ 50 µs: **yes (~12 µs)**. Full clone within 2× of it: **no
(~19×)**. → **Keep the hybrid journal.**

**D2 gate:** buffered crc32(bincode) of the whole `ColliderSet` ≤ 1 ms: **yes, narrowly
(0.83 ms p50, 0.97 ms p95).** Unbuffered, it was 5.7 ms. → Per the plan, Task 4.4's `Voxels`
digest mode may be skipped; the decision summary weighs that. The buffered writer is required
either way.

## Task 1.2 — upstream against our movement fixes

`crates/physics-next/tests/upstream_movement_probe.rs`: the five `shape_cast_grazing.rs` cases
and `gjk_upstream::capsule_touching_cuboid_matches_upstream`, translated to glam with their
assertions unchanged, plus the centred-capsule check and probes for D6 and D7. The cases that
fail upstream are `#[ignore]`d, with the reason in the attribute. Each ignore comes off when its
patch lands in the fork. Run: `just test-one physics-next "--test upstream_movement_probe"`
(6 pass, 4 ignored); append `-- --include-ignored --nocapture` for the failures and the numbers.

| case | upstream 0.31.1 | detail |
|---|---|---|
| `cast_starting_at_the_target_distance_hits_with_the_true_normal` | **FAIL** | No misses, and the normals are right to ≤ 2.6e-3 (the test wants 1e-4). A start 1.5e-3 outside the skin gets the geometric TOI (2.2–2.7e-3); the fork's `GRAZING_START_MARGIN` reports a resting contact (< 1e-3). Within the skin, upstream says `PenetratingOrWithinTargetDist` at TOI 0 (#106, D8). |
| `cast_moving_away_from_within_the_target_distance_is_free` | pass | |
| `voxels_cast_sees_the_voxel_under_a_hovering_shape` | **FAIL** | All three casts miss the floor (search domain). |
| `voxels_cast_reports_no_edges_inside_a_flat_floor` | **FAIL** | Every start gets the geometric TOI (~5.3e-3) instead of a resting contact. Next to a seam (x = 1.01) the normal leans ~4e-3 (0.24°): much smaller than the fork's old degrees-off defect, but over the 1e-4 bar. The slide along the floor is free (pass). |
| `voxels_cast_keeps_real_edges` | pass | |
| `capsule_touching_cuboid_matches_upstream` (gjk) | pass | Upstream output matches Plan 1's hand port to 1e-6. |
| centred capsule, `capsule_y(0.7, 0.3)` at (0, 1, 0) on `Cuboid(8, 8, 8)` at (0, −8, 0), `query::contact`, prediction 0 | pass | `normal1` = +Y within 1e-4, as expected (the nalgebra-era fork gets a stale 45°). |
| **new:** `walk_on_flat_voxel_floor_keeps_its_height` | **FAIL** | The upstream KCC (game settings) walking a flat block-scale voxel floor drops **~1.16 units** just before some chunk seams (x ≈ 28 and 92), then pops up 1.6 ungrounded: 114/120 ticks grounded, max \|Δy\| 1.61. This is the walking-smoothness defect the fork's voxels-cast patches fixed, reproduced on a bare floor. |

**Re-port scope** (`// mmo-fork:` patches; each test's ignore comes off when it lands):

- **Task 3.2 (voxels cast):** `voxels_cast_sees_the_voxel_under_a_hovering_shape` (search
  domain), `voxels_cast_reports_no_edges_inside_a_flat_floor` (internal edges).
- **Tasks 3.2–3.4 together:** `walk_on_flat_voxel_floor_keeps_its_height`. The dips sit at
  chunk-body seams only, and the probe doesn't isolate the cause (voxels cast,
  `cast_shape_on_continuous_surfaces`, snap gate).
- **Task 3.3 (grazing start):** `cast_starting_at_the_target_distance_hits_with_the_true_normal`,
  plus the TOI half of the flat-floor case. Upstream's TOIs here are geometrically right; what
  the fork adds is the policy that a start within `GRAZING_START_MARGIN` of the skin *is* a
  resting contact, and our KCC tuning depends on that. Port the policy, not a fix.

### D7 — upstream's zero-motion depenetration (#485)

`kcc_zero_motion_depenetration_is_cuboid_only`, with a capsule sunk 0.3 units (the
`sunk_body.rs` depth), game controller settings:

| ground | zero desired translation | walking step |
|---|---|---|
| block-scale `Voxels`, voxel middle | **no push** (0, 0, 0), grounded | dy 0 |
| `Voxels`, seam between two voxels | **no push** | dy 0.0046 (a slide off the seam edge; not depenetration) |
| `Voxels`, chunk corner (four voxels, two bodies) | **no push** | dy 0.0005 |
| full-chunk cuboid collapse | **straight up by 0.51600** = sink + skin (0.3 + 0.216) | dy 0 |

- #485 resolves through `QueryDispatcher::contact`, which parry 0.31.1 does not implement for
  `Voxels` (capsule vs `Voxels` → `Err(Unsupported)`, silently skipped). On voxel terrain it does
  nothing.
- On a cuboid it does exactly what `depenetration_lift` does (straight up, to the skin gap).
- It never runs for a nonzero desired translation.
- `game::movement::sweep_character` applies `depenetration_lift` *before* `move_shape`, so by the
  time #485 looks, the body is already at the skin gap and it finds nothing to do. They
  don't fight when the lift succeeds. **They do when the lift declines:** a body buried deeper
  than `STEP_HEIGHT` on a cuboid gets pushed out by #485 (probe
  `kcc_zero_motion_moves_a_body_the_lift_leaves_buried`: 10.6 units deep → lifted 7.2, the
  quarter-height cap). That's why D7 takes the `depenetrate = false` flag.
- `depenetration_lift` survives the switch: its `intersect_shape` goes through
  `intersection_test`, which 0.31.1 *does* implement for `Voxels`.

### D6 — `IntegrationParameters::length_unit`

`length_unit_does_not_change_character_movement`: a 120-tick walk across 16 chunks of flat
block-scale voxels, and a 1200-tick stand, at `length_unit` 1.0 and `UNITS_PER_METER` (16):

| length_unit | walking: ticks the capsule's BVH leaf changed | walking: max \|Δy\| | standing: leaf changes |
|---|---|---|---|
| 1.0 | 120/120 | 1.6092 (the upstream seam dip above) | 1/1200 (the settling tick) |
| 16.0 | 120/120 | 1.6092 | 1/1200 |

- The walking trajectories are bit-identical.
- `length_unit` scales the broad phase's change-detection margin (0.04 × `length_unit`: 0.04 vs
  0.64 units), contact prediction and recycling, and sleep thresholds. The character controller
  reads none of these.
- A walking body moves 3.45 units/tick, beyond either margin, so its leaf changes every tick. A
  standing body's skin creep (~1e-4/tick) stays inside both.
- Separately, **`BroadPhaseBvh`'s serialized bytes change on every step regardless**: its
  `frame_index` counts steps. A dirty flag can't come from a byte diff; the journal clones the
  broad phase every tick anyway (D1).
- The "walking_smoothness green" criterion can't discriminate until Task 3.2 lands: both values
  fail it upstream, identically.

## Carried forward from Phase 0 (for later tasks)

- **std `Hash` is target-dependent (evidence for D2).** It writes `usize` at pointer width
  (length prefixes, `[usize]` slices hashed as raw memory). `game::state_hash` therefore differs
  between native and wasm32 for the same state, which is why `wasm_determinism.rs` compares crc32
  of the region's bincode bytes (`WASM_SCENE_DIGEST`) and pins `state_hash` native-only
  (`NATIVE_STATE_HASH`). The same applies to `cells.rs` `cell_hash`: the server computes it and
  clients hard-fail on a mismatch, so **a wasm32 client in cell mode would get
  `CellHashMismatch`**. Not fixed here; Task 5.2's move of `cell_hash` to `StateDigest` fixes it,
  and Task 4.4's `Voxels::content_digest` must stay target-independent (widen `usize` to `u64`, as
  5e2f219 did for the old fork's cached hash).
- **Likely false alarms at the switch (Task 5.4).** The Phase 0 review noted:
  - `physics_goldens` has no grounded slack at tick 0 (`GROUNDED_SLACK_TICKS` only covers
    transitions), and `jump_in_place` is airborne at tick 0.
  - `slime_hop_sequence` is sensitive to a one-tick landing shift.

  Diverging there first is expected, not a regression. Also, `POS_TOL` = 0.05 units (≈ 3 mm) per
  axis per tick, with no drift allowance. The Phase 0 reviewer suggested ~0.8 units (5 cm); that
  value is the user's call before Phase 5.

## Plan text corrected alongside this doc

- Task 0.2 "Files": the golden landed in `crates/worldgen/tests/scene_hash_golden.rs`, not
  `crates/game/tests/`.
- Task 0.3 Steps 1–3: the cross-target assertion is crc32 of the region's bincode
  (`WASM_SCENE_DIGEST`), not `state_hash`/`WASM_SCENE_HASH`. `state_hash` is pinned native-only.
- Task 0.3 Step 4: the recipe snippet is replaced by a description of the real `determinism-wasm`
  recipe (it installs a Cargo.lock-matched `wasm-bindgen-test-runner` under `target/tools`).

## Import commit (Task 2.1)

**`bb7d5b5`** — `vendor: pristine rapier 0.36.0 + parry 0.31.1 as *-next (unused)`. The fork
diff (Task 6.3) is always `git diff bb7d5b5 -- crates/rapier-next crates/parry-next`.

- Sources: rapier tag `v0.36.0` (b716d375), parry tag `v0.31.1` (3383f51c). Checked
  2026-09-27 against `git ls-remote --tags` and crates.io: no newer 0.36.x / 0.31.x exists
  (parry 0.31.0 is yanked).
- Imported: each upstream root `Cargo.toml`, `src/`, the 3D f32 crate dir
  (`crates/rapier3d/`, `crates/parry3d/`, with upstream's tests), `parry/assets/tests/`, licence
  and docs. Left out: bindings, testbeds, 2D/f64 crates, example dirs, `rapier/assets`, website.
- First fork edit (the commit after it): the two crate manifests become self-contained
  (`rapier3d-next` / `parry3d-next`, `[lib] name` unchanged, rapier's parry dep →
  `parry3d-next`), because our workspace replaces upstream's root for inheritance; upstream's
  kiss3d/ptree/rand_isaac dev-deps and example targets are dropped.
- Against the vendored forks, `upstream_movement_probe` gives exactly the crates.io result:
  7 pass / 4 ignored, and with `--include-ignored` the same 4 fail with byte-identical output
  (same TOIs, same missed floor, 114/120 grounded).
- Upstream's determinism tests pass from the vendored copy (`just physics-next-upstream`):
  `snapshot_roundtrip` (8 pass, 1 upstream-ignored), `snapshot_portability`,
  `simd_backend_parity`, `simd_backend_determinism`. `thread_count_determinism` is
  `parallel`-gated and not run (we never enable `parallel`).
