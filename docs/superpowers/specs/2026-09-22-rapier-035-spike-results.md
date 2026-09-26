# Rapier 0.36 / parry 0.31 upgrade — Phase 1 spike results

Plan: `docs/superpowers/plans/2026-09-22-rapier-035-upgrade.md` (Phase 1, Tasks 1.1–1.3).
Everything here ran against **pristine crates.io** rapier3d 0.36.0 / parry3d 0.31.1 / glamx 0.3.1
(features `enhanced-determinism, serde-serialize`) from `crates/physics-next`. No game crate
depends on it (`cargo tree -p game|client|server|worldgen` shows no 0.36 rapier).

<!-- DECISION SUMMARY (Task 1.3) goes here -->

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
- **Serialized digests are slow at this scale:** ~2 GB/s nominal crc32, but ~0.3 GB/s through
  bincode's per-field writes. A Full-mode per-entry digest of bodies + colliders + broad phase
  is **~8.6 ms**. With `Voxels` replaced by a stand-in, the collider digest drops from 5.7 ms to
  1.3 ms: still over the 1 ms gate, because 2956 colliders × ~196 bytes of non-shape state
  remain. Buffering the writer would help by a constant; this spike didn't time it.

**D1 gate:** hybrid wholesale ≤ 50 µs: **yes (~12 µs)**. Full clone within 2× of it: **no
(~19×)**. → **Keep the hybrid journal.**

**D2 gate:** crc32(bincode) of the whole `ColliderSet` > 1 ms: **yes (5.7 ms)**. → The
Full-mode digest needs Task 4.4's cached per-`Voxels` content digest (5.7 → ~1.3 ms). Even then,
a Full-mode whole-set digest per log entry costs milliseconds at region scale. That's acceptable
only because Full is a test/harness mode (production runs `Off`, and `Delta` digests only the
entry's footprint).

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
  domain), `voxels_cast_reports_no_edges_inside_a_flat_floor` (internal edges),
  `walk_on_flat_voxel_floor_keeps_its_height` (seam dips; both halves together).
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
  time #485 looks, the body is already at the skin gap and it finds nothing to do. **The two
  don't fight.**
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
