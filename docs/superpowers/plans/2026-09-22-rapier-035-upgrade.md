# Rapier 0.36 / parry 0.31 (glam) Upgrade Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the nalgebra-era rapier 0.31 / parry 0.25.3 forks with forks of upstream rapier 0.36.0 / parry 0.31.1 (glam via `glamx`, `f32` scalar). Keep bit-exact rollback, client/server determinism (native and wasm32), and today's movement behaviour, and shrink the fork diff to a handful of hooks.

**Architecture:**
- **Land the work without breaking `develop`.**
  - The new forks are vendored next to the old ones under distinct package names (`rapier3d-next`, `parry3d-next`) and get their own test harness crate, `crates/physics-next`. Every fork patch and the new rollback journal land and get tested there, without touching the game.
  - One short "switch" phase then moves the game over, renames the packages back and deletes the old forks.
- **Rollback becomes a hybrid journal:**
  - first-touch value capture of `RigidBody`/`Collider` inside the sets' internal accessors;
  - per-tick wholesale clones of `IslandManager`, `NarrowPhase`, `BroadPhaseBvh` and the joint sets;
  - a fresh `PhysicsPipeline` + `CCDSolver` after every revert.
- **Hashing moves from std `Hash` over `OrderedFloat` to our own zero-copy `StateHash` trait** (new first-party crates `state-hash` + `state-hash-derive`): the same field-by-field walk into the hasher, but implementable for glam `f32` types (floats hash by canonicalised bits), derived in the rapier/parry forks. Hashing serialized bytes was rejected: the user measured it far slower than `Hash` impls when the engine was designed (decided 2026-09-27, D2).

**Tech Stack:** Rust; rapier3d 0.36.0, parry3d 0.31.1, glamx 0.3.1 (glam 0.33), simba 0.10, nalgebra 0.35 (rapier-internal only); bincode 1.3 (wire/parking only); crc32fast (hasher for `StateHash`); wasm-bindgen-test-runner + node for wasm32 goldens.

**Spec:** `docs/superpowers/specs/2026-09-22-vendored-deps-update-investigation.md`, plus the design section of this plan. Roadmap: `docs/superpowers/plans/2026-09-22-physics-upstream-roadmap.md`.

## Global Constraints

- Work on `develop` in the main checkout. No worktrees. No long-lived branches that would strand the user's concurrent uncommitted edits.
- Before every commit, check `git diff -- <file>`, stage by hunk (`git add -p`), and never use `git add -A`, `git add .` or `git stash`.
- `develop` must build and `just smoke` must pass after every commit. Phases 1–5 add crates; they never change what `game`/`client`/`server` compile against.
- Verification uses `just test-one <crate> "<target filter>"` and `just smoke`.
  - Whole suites (`just test`, `just test-full`) only in Phase 6, and only after asking the user.
  - Ask before any timing run or benchmark (the user runs their own on this box).
- Rollback bar: `hash(before) == hash(after undo)`, bit-exact, in every hash mode (`Full`, `Delta`, `Off`).
- Determinism:
  - `enhanced-determinism` stays on.
  - No `parallel` feature; regions are the threading unit.
  - No `simd8`.
  - No new call to an inherent `f32` transcendental (`sin`, `cos`, `acos`, `atan2`, `exp`, `powf`) in any sim path. Use `libm::` or glam's `libm`-backed methods.
- Wire and parking format: the switch breaks both. Server and client ship together; there is no persistent store yet. Bump nothing and don't add compat shims.
- The region edge, chunk edge and `UNITS_PER_METER` (= `BLOCK_VOXELS`, 16) are never written as literals.
- Tag any new test slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`.

## Design decisions (made here; flagged ones need the user's confirmation before their phase)

| # | Decision | Choice | Why |
|---|---|---|---|
| D1 ✅ | Rollback mechanism | **Hybrid journal** (option 2, confirmed 2026-09-27): first-touch value capture of `RigidBody`/`Collider`, plus per-tick wholesale clones of `IslandManager`, `NarrowPhase`, `BroadPhaseBvh` (and the joint sets if non-empty). Not a port of today's fine-grained narrow-phase log, not full snapshots. | Spike at real region scale: wholesale clone ~12 µs/tick, full 7-structure clone ~225 µs (+63 µs drop, 3.1 MB). The narrow phase is ~1 KB upstream (no kinematic↔fixed pairs), so the fine-grained log isn't worth re-porting. Known cost: the broad phase (240 KB) is copied every tick, since its serialized `frame_index` changes every step. |
| D2 ✅ | State hashing | **Own zero-copy `StateHash` trait + derive** (confirmed 2026-09-27), replacing std `Hash` and the plan's earlier bincode `StateDigest`. `f32` hashes canonicalised bits (−0.0 → 0.0, one NaN, matching `OrderedFloat`'s `Hash`); `usize` hashes as `u64` so native and wasm32 agree; impls for glam/glamx and the few nalgebra types in rapier state live in the trait crate (orphan rule allows it); rapier/parry structs `#[derive(StateHash)]` in the forks; cached digests (voxels content hash, pairs, BVH) become hand impls. | The user measured hashing serialized state as far slower than `Hash` impls when the engine was designed; that's why the math forks exist. glam types can't get std `Hash`, so the trait is ours. Cost: the derive on ~140 rapier/parry types is a permanent, mechanical fork burden. |
| D3 | `OrderedFloat` in game state | Remove it. Game-owned floats become plain `f32`; glam types are used directly. | Its only job was std `Hash`/`Ord`; `StateHash` hashes `f32` directly. Any game code that truly needs `Ord` on floats uses `total_cmp` or keeps crates.io `ordered-float` locally. |
| D4 ✅ | Forks that survive the switch | **Keep vendored:** `rapier3d`, `parry3d` (fork patches + `StateHash` derives), `slotmapd` (rollback inverses), `block-mesh` (unchanged). **Move to crates.io or drop:** `simba`, `nalgebra`, `approx`, `ordered-float`. No new forks (glam is not forked). Confirmed 2026-09-27. | Upstream gives determinism through features (`simba/libm_force`, `glamx/libm`, `glamx/scalar-math`); rapier 0.36 uses crates.io nalgebra 0.35 / simba 0.10.2 internally, which our 0.34/0.9 forks can't serve. Reverses the CLAUDE.md "don't switch back to crates.io" rule (rewritten in Task 6.1). |
| D5 ✅ | How the multi-week port lands | Side-by-side vendoring (`crates/rapier-next`, `crates/parry-next`, `crates/physics-next`) on `develop`, then one switch commit series. Confirmed 2026-09-27. | No worktrees or long-lived branches; the game compiles against the old forks until Phase 5, so `develop` builds and `just smoke` passes at every commit. Phase 1 already landed this way. |
| D6 ✅ | `IntegrationParameters::length_unit` | **`UNITS_PER_METER` (16)**, as `movement::PHYSICS_LENGTH_UNIT`. Confirmed 2026-09-27. | Measured neutral today (identical walks and BVH-leaf update counts at 1 and 16; the KCC reads none of the scaled thresholds, and every game body is fixed or kinematic). 16 makes rapier's tolerances (prediction 2 cm instead of 1.25 mm, sleep thresholds, contact recycling) correct for the dynamic bodies to come. |
| D7 ✅ | Character-controller depenetration | Keep `movement::depenetration_lift`; **gate off** upstream's zero-motion depenetration (#485) with a `KinematicCharacterController::depenetrate: bool` fork field (default `true`, the game sets `false`). | Spike: #485 is inert on `Voxels` (no `contact` for them upstream) and matches the lift on cuboids, but where the lift declines (buried deeper than `STEP_HEIGHT`) #485 pushes the body out by up to a quarter of its height, on cuboid chunks only — against the lift's "buried stays put" policy. |
| D8 | Upstream parry #106 (touching = `Converged` at TOI 0) | Accept it, and rework our two separated-start cast filters to `toi == 0 && normal·vel >= 0` | It's in 0.30.2 and carried unchanged into 0.31.1, and reverting it would be a new fork patch. Pinned by `shape_cast_grazing.rs`. |

## Retargeted to rapier 0.36.0 / parry 0.31.1 (2026-09-26)

Written against 0.35.3 / 0.30.2, then retargeted when 0.36.0 shipped (2026-09-25) before any Plan 2 work had started. All seven design facts still hold (checked against source); per-task deltas:

- **Pins.** rapier 0.36.0 pins parry `^0.31.1` (0.31.0 is yanked), glamx `^0.3.1`, simba `^0.10.2`, nalgebra `^0.35` (internal only) and `wide ^1`. MSRV is 1.86 and editions are unchanged. This release is new, so **re-check for a 0.36.x patch at the Task 2.1 import** and take the newest.
- **Soft bodies are always compiled in; we don't use them.**
  - `PhysicsPipeline::step`, `RigidBodySet::remove`, `ColliderSet::remove`, `DebugRenderPipeline::render` and `CCDSolver::solve_continuous` now take `&mut SoftBodySet`.
  - New variants break exhaustive matches: `RigidBodyType::SoftFrame`, which `is_dynamic()` counts as dynamic, and `DebugRenderObject::SoftBody`.
  - `EventHandler` gains a required `handle_soft_body_tear_event`.
  - The journal (D1) clones the empty `SoftBodySet` wholesale and debug-asserts it stays empty.
  - The game keeps one empty `SoftBodySet` in `PhysicsState` (Task 5.2).
- **parry 0.31 API changes:**
  - `ShapeCastHit` gains `subshape1/subshape2`, and `RayIntersection`/`PointProjection` gain `subshape`.
  - `distance`/`intersection_test` return structs.
  - `CompositeShapeRef` queries return `(id, hit)`.
  - The GJK/EPA fixes are byte-identical to 0.30.2, so Plan 1's parity tests apply as-is.
- **Task 3.1:** `rigid_body_components.rs:434-446` now has an `is_soft_frame()` early return. Extend it with `|| body_type.is_fixed()`, keeping the `recompute_max_extent` call if our old patch kept it.
- **Task 3.3:** ported casts must fill `subshape1/2`. `cast_shape_on_continuous_surfaces` uses the `(id, hit)` form and maps ids via `handle_of` (`query_pipeline.rs:174-178`). `cast_shape` is at `:489`.
- **Task 3.4 line refs:** depenetration `character_controller.rs:240` / `:313-317`, snap gate `:457`, `predict_ground` `:491-514`.
- **Task 3.5 line refs:** re-insertion `broad_phase_bvh/update.rs:74-93`, filter `:359-397`.
- **Task 4.1:** `RigidBodySet::remove` takes `soft_bodies`. Re-check the arena free-list layout against 0.36.
- **Task 4.2:**
  - Capture accessors: `rigid_body_set.rs:352-366`, `collider_set.rs:530-545`.
  - The staged solver's raw-pointer writes are at `staged_island_solver/mod.rs:212` (created `solve.rs:267`, `substep.rs:166`; dereferenced `worker.rs:702/964`). The debug asserts go there.
- **Task 4.3:** memos `physics_pipeline/mod.rs:48-53` (used `solve.rs:351-358`); staged assembly cache `staged_island_solver/mod.rs:287`, `joints.rs:52,313`. `CCDSolver` needs no patch; upstream now also calls `invalidate_fixed_targets_cache` itself (`substep.rs:389-397`).
- **Task 5.2:**
  - Game call sites needing `SoftBodySet`: `state.rs:~933` (`bodies.remove`), `debug_render.rs`, `physics.rs` (step), and the `CCDSolver` kept in state (`state.rs:~222`).
  - Add match arms for the new variants.
- **Task 5.4:** upstream's portability golden changed in 0.36; ours are re-recorded anyway.

## Review Focus

1. **A rollback across a tick where the staged solver wrote a body the journal didn't capture.** The capture hooks sit in the set accessors, but the staged solver writes through a raw pointer (0.36: `staged_island_solver/mod.rs:212`, dereferenced `worker.rs:702/964`). Task 4.2 adds a debug assert, and the revert fuzzer must hit wake-up, island-merge and removal ticks.
2. **Revert, then re-step, with a live `PhysicsPipeline`.** Its epoch-keyed joint and assembly memos go stale after a rewind. Task 4.3 pins that a fresh pipeline after revert gives byte-identical re-steps, with joints present even though the game has none today.
3. **The same scene on native and on wasm32.** The voxel mass properties go through std `acos`/`cos` in glamx `SymmetricEigen3`. The fixed-body mass-props skip must survive the re-port, and Task 0.3's cross-target golden must match.
4. **A player walking over chunk and scale seams, standing on a 1-voxel terrace, and sunk into terrain.** These use the re-ported voxels-cast fixes, the KCC snap gate and `depenetration_lift` against the upstream KCC. `walking_smoothness` (with its slow tests), `shape_cast_grazing`, `sunk_body`, `jump_arc`, plus the Phase 0 behaviour goldens.
5. **Parking a region and restoring it, and a snapshot sent client→server→client.** Custom `query_dispatcher`s are serde-skipped, voxel shapes serialize their whole data, and the parry BVH now serializes its optimizer cursor. Restore must be bit-exact (`rollback_restore.rs`, `region_runner`, `handoff_*`), and `state_hash` must be equal before and after the round trip.

---

## Phase 0 — Behaviour rig on the CURRENT forks (lands before anything else)

The port changes numerics, so bit-exact hashes can't carry across it. What carries across is behaviour within tolerance. Record it now, on the engine we trust.

### Task 0.1: Scripted movement goldens

**Files:**
- Create: `crates/game/tests/physics_goldens.rs`
- Create: `crates/game/tests/fixtures/physics_goldens/*.csv` (generated)

**Interfaces:**
- Produces: `fn run_scenario(name: &str) -> Vec<[f32; 4]>`, i.e. per tick `(x, y, z, grounded as f32)` of the tracked body. Scenarios are built only through public game APIs (`Rollback`/`GameData`, `movement`, `MoveKeys`), so the same test source compiles after the switch.
- The CSV format is one row per tick, `tick,x,y,z,grounded`, which is diff-friendly.

- [x] **Step 1: Write the scenario list and harness.** Reuse the setup helpers `walking_smoothness.rs` and `jump_arc.rs` already use; read both first and call the same functions rather than copying them. Scenarios, each 120 ticks:
  - `walk_flat_across_chunk_seam`: +x at walk speed, starting 2 units before a chunk boundary.
  - `walk_scale_seam`: block-scale onto a half-block (chisel) surface.
  - `step_up_one_block`
  - `terrace_uphill_1_voxel`: the known speed-dip case.
  - `jump_in_place`
  - `sprint_jump_over_gap`
  - `sunk_start_depenetrates`: starts 0.3 units inside terrain.
  - `slime_hop_sequence`: one slime, fixed seed.

- [x] **Step 2: Add the record mode.** When `PHYSICS_GOLDENS_RECORD=1`, write the CSVs. Otherwise compare, with tolerances as named constants:
  - `POS_TOL = 0.05 * UNITS_PER_METER / 16.0`, i.e. 0.05 units per axis per tick;
  - `grounded` must match exactly except within `GROUNDED_SLACK_TICKS = 2` of a transition.

  On failure, print the first diverging tick and the full row pair.

- [x] **Step 3: Record, then run in compare mode.** Run `PHYSICS_GOLDENS_RECORD=1 just test-one game "--test physics_goldens"`, then `just test-one game "--test physics_goldens"`. Expected: PASS. Run it twice more to prove it's stable, i.e. deterministic.

- [x] **Step 4: Sanity-check the fixtures by eye.** For `jump_in_place`, peak height ≈ the value `jump_arc.rs` pins. For `walk_flat_across_chunk_seam`, y is constant within 1e-3. If not, the harness is wrong: fix it before committing.

- [x] **Step 5: Commit.** `test(game): behaviour goldens for movement, recorded on the nalgebra-era physics`.

### Task 0.2: State-hash golden for a canonical scene (same-engine regression only)

> **Landed (cc6e060) in `crates/worldgen/tests/scene_hash_golden.rs`, not `crates/game/tests/`:** `game` can't depend on worldgen. `SCENE_HASH = 0xEF4000BC`; skips on non-default world dials.

**Files:**
- Create: `crates/worldgen/tests/scene_hash_golden.rs`

- [x] **Step 1: Write the test.**
  - Build a canonical scene: the fixed worldgen region `RegionCoords { x: 0, z: 0 }` at `set_near_radius(0)` scale if applicable; one player; one slime; 60 ticks of scripted input.
  - Assert `game::state_hash(&rollback)` equals a constant.
  - Include in the doc comment the regeneration command (`SCENE_HASH_PRINT=1 ... -- --nocapture`), and the rule: the constant changes only in a commit that says why.

- [x] **Step 2: Record the constant, then assert.** Tag the test slow if it takes more than 100 ms (region genesis likely does).
- [x] **Step 3: Commit.** `test(game): canonical scene state-hash golden`.

### Task 0.3: Cross-target determinism check (native vs wasm32)

> **Landed (5e2f219, 78fa13a).** The first run did NOT match. The Voxels `cached_hash` hashed slice length prefixes at pointer width, and it is serialized, so the bytes differed per target. Fixed in the parry fork; native values are unchanged. The cross-target assertion compares crc32 of the region's bincode bytes (`WASM_SCENE_DIGEST = 0x105EC680`). `state_hash` is pinned native-only (`0xA93B9D8C`) because std `Hash` is pointer-width dependent (length prefixes, `[usize]` slices). Consequences: Task 4.4's Voxels `content_digest` must be target-independent, and Task 5.2's `cell_hash` → `StateDigest` switch also fixes a wasm cell-mode `CellHashMismatch`.

**Files:**
- Create: `crates/game/tests/wasm_determinism.rs` (native side) and `crates/game/tests/wasm/` or a `#[wasm_bindgen_test]` module in the same file (check how/if the workspace already uses `wasm-bindgen-test`: `grep -rn wasm_bindgen_test crates`)
- Modify: `justfile` (new recipe `determinism-wasm`)

- [x] **Step 1: Write a scene that fits wasm.** Kinematic capsule on a small hand-built voxels floor (8×8 chunks' worth of `Voxels`) plus one slime, 120 ticks. It uses no worldgen, so the wasm build stays small. Compute crc32 of the region's bincode bytes (`state_hash` is pointer-width dependent, see the note above).
- [x] **Step 2: Native side.** `#[test] fn scene_hash_native()` asserts that digest equals `WASM_SCENE_DIGEST`, and `state_hash` equals the native-only `NATIVE_STATE_HASH`.
- [x] **Step 3: wasm side.** `#[wasm_bindgen_test] fn scene_hash_wasm()` asserts the same `WASM_SCENE_DIGEST`.
- [x] **Step 4: Add the justfile recipe** `determinism-wasm` (see the `justfile`: it installs the `wasm-bindgen-test-runner` matching Cargo.lock under `target/tools` on first use, then runs `--test wasm_determinism` natively and with `--target wasm32-unknown-unknown`).

- [x] **Step 5: Run both.** Expected: the same hash. If `game` doesn't build for wasm32 test targets, e.g. because of dev-deps, scope the dev-deps with `[target.'cfg(not(target_arch = "wasm32"))'.dev-dependencies]`. If it still can't be made to run in ≤ 1 h, record that in the commit and fall back to comparing a native hash against one printed by the browser client in a debug build (`?offline`). This test is the only proof of cross-target determinism, so don't drop it silently.
- [x] **Step 6: Commit.** Include CLAUDE.md: add `just determinism-wasm` to the verification list.

### Task 0.4: Port Plan 1's upstream-fix tests into the rig

- [x] **Step 1: Check where Plan 1 stands.** It is `docs/superpowers/plans/2026-09-22-physics-fork-fixes.md`, and it lands first. `gjk_upstream.rs`, `voxels_upstream.rs` and `nalgebra_fixes.rs` exist and are green. Those tests cover fixes that are *in* parry 0.30.2 (unchanged in 0.31.1), so they must stay green after the switch. That's the check that the upstream versions behave like our hand ports.
- [x] **Step 2: Add a module doc line to each file:** "Must stay green across the rapier 0.36 switch (upgrade plan Phase 5)."

---

## Phase 1 — Spikes and decision gate (scratch crate, nothing merged into the game)

> **Landed (04dd668..bf8da0a, 2026-09-26); gate passed 2026-09-27.** Results and numbers: `docs/superpowers/specs/2026-09-22-rapier-035-spike-results.md`. The exporter lives in `crates/worldgen/examples/` (game can't depend on worldgen). D1–D7 confirmed as in the table above. Re-port scope from Task 1.2: the four `#[ignore]`d tests in `crates/physics-next/tests/upstream_movement_probe.rs` (search domain, internal edges, grazing start, the seam-dip walk — the last only comes off after 3.2–3.4 together).

### Task 1.1: Real-scale clone and hash cost

**Files:**
- Create: `crates/physics-next/` (workspace member, `publish = false`), depending on crates.io `rapier3d = { version = "=0.36.0", features = ["enhanced-determinism", "serde-serialize"] }` and `bincode`, `crc32fast`. Phase 2 swaps this for the vendored fork.
- Create: `crates/game/examples/export_region_colliders.rs`. It writes, for one generated region, every chunk collider's `(chunk origin, voxel_size, Vec<[i32;3]>)` or cuboid half-extents to `target/physics-next/region_<x>_<z>.bin` (bincode).
- Create: `crates/physics-next/examples/clone_cost.rs`

- [x] **Step 1: Write the exporter.** It uses `game`'s genesis path (`seed_terrain_genesis`, `state.rs:565`) to get exactly the production collider set; read that function and walk `data.physics.colliders`. Add a `just` recipe `export-region` only if it's worth a one-word command; otherwise document the `cargo run --example` line in the file header.
- [x] **Step 2: Write `clone_cost.rs`.**
  - Load the export.
  - Build a 0.36 world (with an empty `SoftBodySet`) with the same bodies and colliders (fixed body per chunk, as the game does today), plus 4 kinematic capsules on the surface, moved each tick.
  - Measure over 600 ticks: clone time per structure, wholesale `(IslandManager, NarrowPhase, BroadPhaseBvh)` clone, full 7-structure clone, bincode size, crc32 of bincode per structure, first step after `PhysicsPipeline::new()`.
  - Print a table.
- [x] **Step 3: Ask the user before running it** (timing run). Then run it in release, pinned to one core (`taskset -c 2`), and paste the table into `docs/superpowers/specs/2026-09-22-rapier-035-spike-results.md`.
- [x] **Step 4: Gate D1.** Keep the hybrid if the wholesale-small-structures clone costs ≤ 50 µs/tick at real scale. If a full clone is within 2× of that, prefer full snapshots: they're simpler and upstream-guaranteed. Record the decision in the spike doc.
- [x] **Step 5: Gate D2 cost.** If the crc32 of bincode of the whole `ColliderSet` is more than 1 ms, the Full-mode digest needs the cached per-shape digest (Task 4.4). Record which.
- [x] **Step 6: Commit** the crate, exporter and spike doc.

### Task 1.2: Upstream voxels cast and KCC against our movement tests

**Files:**
- Create: `crates/physics-next/tests/upstream_movement_probe.rs`

- [x] **Step 1: Port the pure-parry cases.** Take the five tests in `crates/game/tests/shape_cast_grazing.rs` and translate them to the glam API (`Vec3`, `Pose`; `Voxels::new(Vec3, &[IVec3])`), running against crates.io parry 0.31.1.
- [x] **Step 2: Run them and record which fail.** Expected from the research: the search-domain and internal-edge cases fail upstream; the grazing-start case may pass (partial upstream fallback). This list is the re-port scope for Task 3.2.
- [x] **Step 3: Run a KCC probe.** Capsule on a flat voxels floor, `move_shape` with zero desired translation after placing it 0.3 units sunk. Does upstream's #485 depenetration push it out, and along which normal? Record it for D7.
- [x] **Step 4: Run a `length_unit` probe (D6).** Repeat a 120-tick walk across a chunk seam with `length_unit = 1.0` and `= 16.0`. Record the dirty-BVH tick count and the max |Δy|.
- [x] **Step 5: Write up and commit.** Append the results to the spike doc, then commit.

### Task 1.3: Decision checkpoint with the user

- [x] **Step 1: Summarise.** Write D1, D2, D4, D5 and D6 with the spike numbers at the top of the spike doc.
- [x] **Step 2: Stop and ask the user to confirm D4 and D5** (and D1 if the spike flipped it). Don't start Phase 2 without that.

---

## Phase 2 — Vendor the new forks side by side

> **Landed (bb7d5b5 import, 790a3b4, 2026-09-27).** No newer 0.36.x / 0.31.x existed. The import is a subset (root `Cargo.toml`, `src/`, the 3D crate dir with upstream's tests, parry `assets/tests/`, licence/docs); the crate manifests were then made self-contained, because our root workspace (whose `nalgebra`/`simba`/`parry3d` keys point at the old forks) replaces upstream's for inheritance. The probe gives the crates.io result byte for byte. Upstream's determinism tests run from the vendored copy with `just physics-next-upstream` (`thread_count_determinism` is `parallel`-gated, not run).

### Task 2.1: Import pristine sources

**Files:**
- Create: `crates/rapier-next/` (pristine rapier 0.36.0 git tag (`v0.36.0`, b716d37), `src/` + `crates/rapier3d/Cargo.toml` layout as upstream)
- Create: `crates/parry-next/` (pristine parry 0.31.1, tag `v0.31.1`, 3383f51)
- Root `Cargo.toml`: unchanged. The `*-next` crates are implicit path members (via `physics-next`), and their manifests are self-contained (no workspace inheritance: our `[workspace.dependencies]` keys point at the old forks).

- [x] **Step 1: Import from the tagged git sources**, not the crates.io tarballs, so upstream tests come along: `git clone --depth 1 --branch v0.36.0 https://github.com/dimforge/rapier` into `$CLAUDE_JOB_DIR`-style scratch, then copy. Check the tag names with `git ls-remote --tags`. Import commit message: `vendor: pristine rapier 0.36.0 + parry 0.31.1 as *-next (unused)`. Keep it a pure import, so the fork diff is always `git diff <import-commit> -- crates/rapier-next`. Record the import commit's hash in the spike doc ("Import commit"); Task 6.3 diffs against it.
- [x] **Step 2: Rename the packages** to `rapier3d-next` / `parry3d-next`, with `[lib] name = "rapier3d"` / `"parry3d"`, so source paths inside the forks need no edits. Point rapier-next's parry dependency at `parry3d-next` via `package = "parry3d-next"`.
- [x] **Step 3: Switch `physics-next` to `path` deps on the new forks** with `enhanced-determinism, serde-serialize`. Run `just test-one physics-next "--tests"`. Expected: the Task 1.2 probe gives the same results as against crates.io.
- [x] **Step 4: Confirm the game build is untouched.** `cargo tree -p game | grep -c next` must print 0. Run `just smoke`.
- [x] **Step 5: Run upstream's determinism tests from the vendored copy.** `snapshot_roundtrip`, `snapshot_portability`, `simd_backend_parity`, `thread_count_determinism` (without `parallel`: skip that one if it's feature-gated). If upstream's test dirs don't build inside our workspace, copy those four files into `crates/physics-next/tests/upstream/`. Commit.

---

## Phase 3 — Re-apply the fork's physics patches (each pinned in `physics-next`)

Every task follows the same cycle:
1. Port the pinning test from `crates/game/tests/` into `crates/physics-next/tests/`, translated to glam.
2. See it fail on the pristine fork.
3. Port the patch from the old fork. The old code is at `crates/parry` and `crates/rapier`; `git log -p` on those paths shows each patch with its rationale.
4. Green, then commit.

Keep every patch small and marked `// mmo-fork:` so the fork diff stays greppable.

> **3.1, 3.5, 3.6 landed (dd64deb, 98a21dc, 2026-09-27).** 3.1 also adds `TYPE` to `user_changes`' mass-props recompute trigger (the old fork had it; without the set_body_type guard a fixed→dynamic body would keep mass 0) and re-mints upstream's `snapshot_portability` GOLDEN (only the fixed ground's stored `local_mprops` changed; dynamics bit-identical). Pins: `crates/physics-next/tests/fixed_mass_props.rs`, `pair_filter.rs`.

> **3.2, 3.3, 3.4 landed (b6abc1e, d54e90b, 38cd3da, 2026-09-27; H6).** Every Task 1.2 probe test lost its `#[ignore]`; `walk_on_flat_voxel_floor_keeps_its_height` passes after 3.3 (grounded 120/120, max |Δy| 2.1e-3; pristine 114/120, 1.61). Beyond a straight port:
> - **3.2:** a normal left along one axis is made *exactly* that axis. A floor contact came back as (0, 0.99999994, 0), and upstream's KCC `decompose_hit` (`normal × up` = 0) then read the slide as slipping and dropped the tick's motion. `subshape1` is the hit voxel's flat id.
> - **3.3 / D8:** the separated-start filters key on `toi == 0 && normal·vel >= 0`, and the fork's own resting contacts report #106's status (`Converged` unless within the target distance). None of the five `shape_cast_grazing.rs` assertions changed. `just physics-next-upstream` now also runs upstream's shape-cast / voxels / KCC regression tests.
> - **3.4:** the prediction-loosened voxels manifolds are upstream already (#404), so they are pinned, not patched. `KinematicCharacterController::depenetrate` defaults to `true`; Task 5.2 sets it `false` in `movement::character_controller`. `walking_smoothness.rs` and `sunk_body.rs` run in `physics-next` on a rig (`tests/common/mod.rs`) that drives the fork with `game::movement`'s own `plan`/`settle`; the game's four `slow` cases run untagged there (~0.1 s).
> - H5 review minors: a body turned fixed drops its collider mass (2cafdbe); pair-filter re-pair pins for a groups change and a collider enable toggle (d6d9cae).
> - H6 review: D7's flag is pinned through the rig on cuboid chunks, the only terrain #485 acts on (5612429). Deferred to Task 5.2: set the rig's `length_unit` to `PHYSICS_LENGTH_UNIT` alongside the game.
>
> Pins: `crates/physics-next/tests/{voxels_cast,voxels_upstream,continuous_surfaces,walking_smoothness,sunk_body}.rs`, plus the probe.

### Task 3.1: Fixed-body mass-properties skip (determinism-critical)

- **Old patch:** `git log -p -S "mass" -- crates/rapier/src/dynamics/rigid_body_components.rs crates/rapier/src/dynamics/rigid_body.rs` (landed with 34ec12b).
- **Target:** `crates/rapier-next/src/dynamics/rigid_body_components.rs` ~:421-447 (`RigidBodyMassProps::recompute_mass_properties_from_colliders`). Return early for `RigidBodyType::Fixed`.
- **Test:** port `collider_genesis.rs::mass_props_skip` (read the file for the exact name). Add an assert that a fixed body with a `Voxels` collider never calls into `MassProperties::with_inertia_matrix`, via a debug counter behind `#[cfg(test)]` or by checking `mprops` stays default.
- **Why it's critical:** glamx `SymmetricEigen3` uses std `acos`/`cos`, which isn't identical across native and wasm32.

### Task 3.2: Voxels shape-cast fixes: search domain, internal edges, resting contact without ray casts

- **Old patches:** commits 4173c4e and 7b7eb9e, plus `git log -p -- crates/parry/src/query/shape_cast/shape_cast_voxels_shape.rs` (379 lines, vs upstream's 157).
- **Target:** `crates/parry-next/src/query/shape_cast/shape_cast_voxels_shape.rs`
- **Tests:** the failing subset recorded in Task 1.2, plus the port of `voxels_upstream.rs` (Plan 1).

### Task 3.3: `QueryPipeline::cast_shape_on_continuous_surfaces` and grazing-start handling

- **Old patch:** `crates/rapier/src/pipeline/query_pipeline.rs:512` plus its uses in `character_controller.rs:315/439/736/756`; the parry side is `shape_cast_support_map_support_map.rs` (`GRAZING_START_MARGIN`).
- **Apply D8:** upstream 0.31.1 already returns `Converged` for touching at TOI 0. Re-port only the parts `shape_cast_grazing.rs` still needs, and rewrite the two separated-start filters (the query pipeline, and the voxels cast ~:240) to accept a hit at `toi == 0` only if `normal · vel < 0`.
- **Tests:** all of `shape_cast_grazing.rs`, translated.

### Task 3.4: KCC: snap gate, prediction-loosened ground manifolds, depenetration (D7)

- **Old patch:** `crates/rapier/src/control/character_controller.rs:463-486` (`predict_ground`, `.loosened(prediction)`), and the snap gate (grep `snap` in the old fork's `git log -p`).
- **Target:** `crates/rapier-next/src/control/character_controller.rs` ~:236-300 (#485 depenetration) and ~:443-470 (snap).
- **D7 (decided: gate it):** add `KinematicCharacterController::depenetrate: bool`, defaulting to upstream's `true`; the game sets `false`. Tests: `sunk_body.rs`, translated, and flip `kcc_zero_motion_moves_a_body_the_lift_leaves_buried` (physics-next) to assert that with `depenetrate: false` a buried body is not moved.

### Task 3.5: Pair filter parity and guards

- Upstream has same-body, active-type and group filtering, with forced re-insertion (`broad_phase_bvh/update.rs:73-110, 333-395`). Our fork's fixed-fixed guards (`set_body_type` / `set_parent` panics) are no longer needed; don't port them.
- **Test:** port all of `crates/game/tests/pair_filter.rs`. Then invert `fixed_kinematic_pair_is_not_suppressed`: under upstream, default `ActiveCollisionTypes` filters kinematic↔fixed at creation. Assert the pair is absent, and that it appears after `set_body_type(Dynamic)` (re-insertion). Delete the two "panics" tests with a commit note.
- Grep the game for any read of kinematic↔fixed contact pairs (`contact_pair`, `contact_pairs_with`) before the switch; there were none when this plan was written.

### Task 3.6: `length_unit` (D6)

- Decided: `UNITS_PER_METER` (16). Apply it as a named constant in the game at switch time (`movement::PHYSICS_LENGTH_UNIT`), feeding `IntegrationParameters::length_unit`. There's no fork change.
- Nothing to apply before the switch: `physics-next`'s probes set `length_unit` per test (`length_unit_does_not_change_character_movement` runs both values). At Task 5.2 the value goes where `PhysicsState.integration_parameters` (`crates/game/src/state.rs`, default-constructed today) is built, so every region, parked or restored, carries it in its serialized state.

---

## Phase 4 — Rollback journal and digests on the new engine

> **4.1, 4.2, 4.3 landed (394aac5, 83c072f, 372bce3, review fixes 271c9ee, 2026-09-27; H7).** Pins: `crates/physics-next/tests/{arena_revert,step_journal,journal_fuzz,pipeline_memo,journal_parallel}.rs`. Beyond a straight port:
> - **4.1:** the old fork's tuple API is kept (`alloc_state() -> (Option<u32>, u32)`, `revert_insert(handle, head, len)`), not the `ArenaAllocState` in the table below, so `state.rs` ports 1:1. 0.36's arena layout is the old one; the modified flag is `IN_MODIFIED_SET`.
> - **4.2:** the capture buffer lives in the sets for the duration of `step_journaled` and every `&mut` accessor saves first; the joint sets are cloned when they hold a joint *or pending joint work*. `pre_digest` / `revert_digest` are **not** implemented (they need `StateHash`): they and the game's two `journal_digest_matches_*` scenarios move to Task 4.4 (H8). The fuzzer (8 seeds × 300 ticks, LIFO windows of 1–4, byte-exact revert plus a fresh-pipeline re-step against an unjournaled clone) covers user/contact wakes, sleeps, island merges, body/collider removals and joints; the staged solver's writeback debug-assert never fired. `step_journaled` panics under rapier's `parallel` feature (the capture is single-threaded).
> - **4.3:** re-stepping the same input on the same pipeline never diverged, but re-stepping a **corrected input** did (a stale joint selection). **Interface change:** `StepJournal::revert` bumps a serde-skipped `IslandManager::journal_reverts`, and the pipeline calls `invalidate_caches` itself when that counter moves. `PhysicsParts` gains `ccd_solver`, and `revert` discards its fixed-targets cache: an undone fixed-collider insert left a vacant handle in it, and the re-step panicked. **So the game needs no post-revert call:** the same `PhysicsPipeline` and `CCDSolver` step on. `invalidate_caches` keeps a pending deferred BVH task, since after a zero-`dt` step that task owns the tree. `step_journaled` joins that task after a zero-`dt` step.

> **4.4 landed (b38904a, 1d3362b, 57c11c3, 4872680, 84faa74, review fix c94268d, cost check below; 2026-09-27; H8).** Pins: `crates/state-hash/tests/state_hash.rs`, `crates/physics-next/tests/{voxels_hash_cache,state_hash_parity,state_hash_wasm}.rs`, the digest tests in `step_journal.rs`, and bytes/hash + digest agreement in `journal_fuzz.rs`. Beyond the task text:
> - **Trait crate:** besides primitives/std/glam/glamx, impls for `IndexMap` (in order), `hashbrown` maps (sorted by key; parry without `enhanced-determinism`) and nalgebra matrices (rows, cols, column-major); derive attributes `skip`, `with = "fn"` and `bound = "…"`; `impl_via_bits!` for bitflags. Widths: `Option` a u64 tag, enums a u32 variant index, tuples unprefixed.
> - **Order rulings:** `IndexMap`/`IndexSet` state is hashed in order (the engine iterates it and serde writes it), except `BroadPhaseBvh::pairs` and `PersistentIslands::joint_link_locs`, which upstream serializes sorted — hashed sorted by the serializer's own key fn.
> - **Hand impls:** `dyn Shape` (ShapeType first; `Shape: DynStateHash`), `dyn WorkspaceData`, `Voxels` (`(voxel_size, content_hash)`; the cached FNV content hash is serde-skipped and recomputed on deserialize, so the wire format and upstream's snapshot GOLDEN are unchanged; also refreshed by `propagate_voxel_change`/`combine_voxel_states`), `BvhNodeVec` (raw node image), `VecMap`, `SolverContact` (no padding), `SoftBodySet` (asserts empty). Serde-skipped caches/workspaces are skipped; serde-skipped *pending joint work* (`island_events`, `modified_joints`, `all_joints_modified`, `island_chain_events`) is hashed. Voxels' allocation order/BVH are deliberately not hashed (a known false-equal if the game ever edits voxel shapes in place).
> - **Journal digests:** `pre_digest` / `revert_digest` (+ `live_digest`, the live side without a revert) hash the captured bodies and colliders by sorted handle, plus the joint sets' pre-step hash when they were judged quiescent and not cloned. The wholesale clones are not hashed: the revert moves the saved clone back, so they could never differ (review I1). The broad phase is therefore not hashed per Delta tick at all.
> - **Cross-target:** `SCENE_STATE_HASH = 0x45F9F5BD` on x86_64 and wasm32 (node), per part identical (`just determinism-wasm-next`; merges into `determinism-wasm` at the switch).
> - **Step 4 cost check** (user's go-ahead; release, `taskset -c 2`, core 2 at ~4.1–4.4 GHz, USB-C PD power, region (0, 0): 2616 bodies, 2956 colliders; p50 µs, 600 samples): `StateHash`→crc32 bodies **2307** vs the old forks' std `Hash` **3373** (0.68×), colliders **1261** vs **1742** (0.72×), broad phase **145** vs **140** (1.03×); islands/narrow/joints ≤ 2.5. Same range as today, so no stop. Through a buffering hasher (bytes into a reused `Vec`, one crc32 pass) the same three are 251 / 153 / 20 µs: crc32fast's per-call overhead is ~90% of the field-by-field cost, so Task 5.2 should hash through a buffer. Journal digests: `pre_digest` 5.4, `revert_digest` 6.6 (revert included) per journaled tick with 4 walkers. Rigs: `physics-next/examples/clone_cost.rs` (new rows) and `worldgen/examples/export_region_colliders.rs -- --hash-cost` (old forks, one step first so the broad phase is populated).
> - **For Task 5.2:** slotmapd's `StateHash` impl moves there (slotmapd is a `game` dependency); `physics.rs` calls `pre_digest`/`revert_digest(parts)` in Delta mode.

### Task 4.1: Arena/set rollback primitives the game calls

**Files:** `crates/rapier-next/src/data/arena.rs`, `dynamics/rigid_body_set.rs`, `geometry/collider_set.rs`

**Interfaces:**
- Produces, with the same names and semantics as the old fork so the game's call sites port 1:1:

| Type | Methods |
|---|---|
| `Arena` | `alloc_state() -> ArenaAllocState`, `revert_insert(index, state)`, `revert_remove(index, value, state)` |
| `RigidBodySet` | `restore_raw(handle, RigidBody)`, `alloc_state()`, `revert_insert(handle, state)`, `is_marked_modified(handle) -> bool`, `pop_modified(handle)` |
| `ColliderSet` | `restore_raw(handle, Collider)`, `alloc_state()`, `revert_insert(handle, state)`, `is_marked_modified(handle) -> bool`, `pop_modified(handle)` |

- [ ] **Step 1: Port the tests.** `crates/game/tests/hash_restore.rs`, the arena/`RigidBodySet`/`ColliderSet` exactness tests (10), translated into `physics-next/tests/arena_revert.rs`, asserting byte equality of bincode before and after instead of `Hash`.
- [ ] **Step 2: Watch them fail** (methods missing).
- [ ] **Step 3: Port the implementations.** Old code: `crates/rapier/src/data/arena.rs:370-402`, `rigid_body_set.rs:124-220`, `collider_set.rs:111-154`. Upstream 0.36 arenas: check for changes to the free-list layout before copying.
- [ ] **Step 4: Green, then commit.**

### Task 4.2: Hybrid `StepJournal`

**Files:**
- Create: `crates/rapier-next/src/pipeline/step_journal.rs`
- Modify: `crates/rapier-next/src/pipeline/physics_pipeline/mod.rs` (+ `substep.rs` entry), `dynamics/rigid_body_set.rs:340-346`, `geometry/collider_set.rs:432-438`, `pipeline/user_changes.rs`

**Interfaces:**

```rust
pub struct StepJournal { /* private */ }
impl StepJournal {
    pub fn new() -> Self;
    pub fn is_empty(&self) -> bool;
    /// Undo everything the journaled step did, LIFO-exact.
    pub fn revert(self, parts: PhysicsParts<'_>);
    /// crc32 of the pre-step footprint (Delta hash mode).
    pub fn pre_digest(&self) -> u32;
    /// Revert and return the post-revert footprint digest.
    pub fn revert_digest(self, parts: PhysicsParts<'_>) -> u32;
}
pub struct PhysicsParts<'a> {
    pub islands: &'a mut IslandManager,
    pub broad_phase: &'a mut BroadPhaseBvh,
    pub narrow_phase: &'a mut NarrowPhase,
    pub bodies: &'a mut RigidBodySet,
    pub colliders: &'a mut ColliderSet,
    pub impulse_joints: &'a mut ImpulseJointSet,
    pub multibody_joints: &'a mut MultibodyJointSet,
    /// 0.36: soft bodies are compiled in; the game keeps this empty (asserted).
    pub soft_bodies: &'a mut SoftBodySet,
    /// H7: not restored; its fixed-targets cache is discarded by `revert`.
    pub ccd_solver: &'a mut CCDSolver,
}
impl PhysicsPipeline {
    pub fn step_journaled(&mut self, /* same args as step */, journal: Option<&mut StepJournal>);
}
```

The old signature (`revert(self, islands, broad, narrow, bodies, colliders, ij, mj)`) becomes `PhysicsParts`; the game's call site in `physics.rs:12-53` is adjusted at the switch.

**What it captures:**
- The first write to each `RigidBody`/`Collider` through `index_mut_internal`/`get_mut_internal` and the user-change paths: the value is cloned into a `Vec<(handle, value)>`, deduplicated with a bitset.
- Wholesale clones at step start: `IslandManager`, `NarrowPhase`, `BroadPhaseBvh`, the modified/removed lists; and the joint sets only if they're non-empty.

On `revert`, the captured values are written back in reverse order and the clones are swapped in.

- [ ] **Step 1: Write the revert fuzzer first,** in `physics-next/tests/journal_fuzz.rs`.
  - Seeded scenes: kinematic capsules, a few dynamic balls, one joint pair, collider add/remove, sleep/wake.
  - For each seed and tick: snapshot the bincode of all 7 structures, `step_journaled`, `revert`, and assert the bytes are equal.
  - Then re-step with a fresh pipeline and assert the result is byte-equal to a step taken without the journal.
  - The seed count is a named constant; tag the test slow if it's over 100 ms.
- [ ] **Step 2: Port the scenario tests.** Port `crates/game/tests/step_journal.rs` (18 scenarios) into `physics-next/tests/step_journal.rs`, translated to glam and byte equality.
- [ ] **Step 3: Implement the capture hooks and the journal.** Add `debug_assert!`s in the staged solver's writeback (`staged_island_solver/mod.rs:199`) that every written body handle is already captured. It writes through a raw `*mut RigidBodySet`, so it bypasses the accessor hooks.
- [ ] **Step 4: Green on both files,** plus upstream `snapshot_roundtrip`. Commit.

### Task 4.3: Pipeline memo invalidation after revert

- [ ] **Step 1: Write the test.** A scene with an impulse joint: step to tick N; journal, step and revert; re-step with (a) the same `PhysicsPipeline` and (b) `PhysicsPipeline::new()`. Assert (b) equals a never-reverted run byte for byte. Show whether (a) diverges; either way, record it in the test's doc comment.
- [ ] **Step 2: Implement.** Add `PhysicsPipeline::invalidate_caches(&mut self)` and upstream's public `CCDSolver::invalidate_fixed_targets_cache()` (0.36, `ccd_solver.rs:47-49`; no fork patch), which together clear `joint_constraint_indices`, `joint_selection_primed`, the staged-solver assembly cache and `fixed_targets_cache`. `StepJournal::revert` can't reach the pipeline, so the game calls these after revert. If (a) doesn't diverge, keep the call anyway: it's cheap insurance, and the test pins it.
- [ ] **Step 3: Commit.**

### Task 4.4: `StateHash` trait, derive, and the forks' hash impls (D2)

**Files:**
- Create: `crates/state-hash/` (trait + impls for primitives, std containers, glam/glamx, and the nalgebra types rapier stores), `crates/state-hash-derive/` (proc-macro `#[derive(StateHash)]`), `crates/state-hash/tests/state_hash.rs`.
- Modify: `crates/rapier-next`, `crates/parry-next` (derive `StateHash` on every type the old forks derived `Hash` on; hand impls for the cached digests), `crates/slotmapd` (impl `StateHash` for its maps/keys, next to the existing `Hash` impls).
- At the switch (Task 5.2): `crates/macros/src/lib.rs` (the `Undo<T>` bound ~:740, `hash_data()` ~:864, derives ~:293), `crates/game/src/lib.rs` (`state_hash` :72), `crates/game/src/cells.rs` (`cell_hash`).

**Interfaces:**

```rust
/// Zero-copy canonical state hash: std `Hash`'s shape, but ours, so it can be
/// implemented for foreign `f32` types (glam) and pins widths across targets.
pub trait StateHash {
    fn state_hash<H: core::hash::Hasher>(&self, h: &mut H);
}
// f32: canonicalised bits, matching OrderedFloat's Hash (-0.0 == 0.0, one NaN).
// usize/isize: written as u64/i64, so native and wasm32 hash identically.
// Vec/slices/Option/BTreeMap/arrays/tuples: length as u64, then elements in order.
// glam Vec3/Quat/Mat3/…, glamx Pose3/Rot3: component-wise.
```

`#[derive(StateHash)]` hashes fields in declaration order, like `#[derive(Hash)]`; `#[state_hash(skip)]` for caches/workspaces the old forks excluded from `Hash`.

- [x] **Step 1: Trait crate, tests first.** In `crates/state-hash/tests/state_hash.rs`: `0.0` and `-0.0` hash equal, all NaNs hash equal, any field change changes the hash, a `usize` hashes like the same `u64`, and a derived struct hashes like its fields in order. Watch them fail (crate empty), then implement the trait, impls and derive. Hasher in practice: `crc32fast::Hasher` (implements `core::hash::Hasher`), as today.
- [x] **Step 2: Derive in the forks.** List the types the old forks hash: `grep -rn "derive(.*Hash\|impl.*Hash for" crates/rapier/src crates/parry/src`. Add `#[derive(StateHash)]` (or a hand impl) on the matching types in `rapier-next`/`parry-next`, each line marked `// mmo-fork: StateHash`. Port the old forks' cached digests as hand impls that feed the cached value:
  - `Voxels`: the cached content hash (old code `crates/parry/src/shape/voxels/voxels.rs:516`, target-independent since 5e2f219), maintained on every edit; `StateHash` feeds `(voxel_size, content_hash)`, never the voxel data.
  - Broad phase: the old BVH/pairs digests (`git log -p -S pairs_hash -- crates/rapier crates/parry`).
- [x] **Step 3: Parity tests.** Port `voxels_hash_cache.rs` into `physics-next` (the cached hash tracks edits; equal content ⇒ equal hash). Add a cross-target check: a small scene's `StateHash` over all 7 physics structures equals a constant natively and under `wasm32` (same harness as `just determinism-wasm`).
- [x] **Step 4: Cost check.** Ask the user, then time `StateHash` of the region-scale world from `clone_cost` (bodies, colliders, broad phase), next to the old fork's `Hash` on the same region (the exporter's source). It should be in the same range as today; if it's far slower, stop and report before the switch.
- [ ] **Step 5: Commit.** The macro's `Hash` bound is flipped to `StateHash` in the switch (Task 5.2, Step 2), not here.

---

## Phase 5 — The switch (short; the game moves in one commit series)

### Task 5.1: Point the workspace at the new forks

- [ ] **Step 0: Record the old forks' divergence** for Task 6.3's before/after comparison: clone upstream parry `v0.25.3` and rapier `v0.31.0` into scratch, and save `git diff --no-index --stat` of each against `crates/parry` / `crates/rapier` (plus total `+/-` lines and the count of `mmo-fork` markers) into `docs/superpowers/specs/2026-09-22-rapier-035-spike-results.md` under "Old fork divergence".
- [ ] **Step 1: Delete the old forks.** `git rm -r crates/rapier crates/parry`, then `git mv crates/rapier-next crates/rapier` and `git mv crates/parry-next crates/parry`, and rename the packages back to `rapier3d`/`parry3d`.
- [ ] **Step 2: Update the root `Cargo.toml`.** Rapier gets the features `enhanced-determinism, serde-serialize, debug-render`. Per D4, replace the path deps for `simba`, `nalgebra`, `approx` and `ordered-float` with crates.io versions, or remove them, then `git rm -r` those fork dirs. Leave `slotmapd` and `block-mesh` as they are.
- [ ] **Step 3: Check the build fails as expected.** `cargo check -p game` now fails; that's the migration worklist.

### Task 5.2: Migrate `game`

The type map applies everywhere:

| Old | New |
|---|---|
| `Real = OrderedFloat<f32>`, `Real::from(x)` | `f32`, `x` |
| `Vector<Real>` / `Point<Real>` / `vector!` / `point!` | `Vec3` (`glamx`/`rapier3d::math::Vector`), `Vec3::new` |
| `Isometry<Real>` / `IsometryReal` | `Pose` (`rapier3d::math::Pose`); `.translation.vector` → `.translation` |
| `UnitQuaternion` | `Quat` |
| `na::Matrix4`, `Perspective3` (camera) | `glam::Mat4`, `Mat4::perspective_rh` (check the handedness and depth range `convert.rs` expects) |
| `KinematicCharacterController::up: UnitVector` | `Vec3` |
| `move_shape(.., desired: &Vector)` | `move_shape(.., desired: Vector)` |

- [ ] **Step 1: Migrate the files** in dependency order, with `cargo check -p game` after each: `lib.rs` (the re-exports `pub use parry3d as parry`, `na`) → `protocol.rs` → `state.rs` → `physics.rs` (journal call with `PhysicsParts`, `ccd_solver` included; no post-revert call — H7 made the revert self-invalidate the pipeline and CCD memos) → `movement.rs` (`length_unit` constant from D6, `depenetrate = false` from D7) → `camera.rs` (keep `yaw_quat` libm-forced: `libm::sinf`/`cosf`) → `edit.rs` → `slime.rs` → `cells.rs` → `debug_render.rs` → `input.rs`.

  `cells.rs` came with cell loading after this plan was written:
  - `cell_hash` (~:261-295) feeds `RigidBody`/`Collider` into a `Hasher`. Switch it to `StateHash`: it's a server-authored checksum that clients hard-fail on, so client and server must compute it identically — this also fixes the wasm32 `CellHashMismatch` (std `Hash` wrote `usize` at pointer width).
  - `materialize` (via `seed_terrain_genesis`, ~:310) and `dematerialize` insert and remove terrain bodies and colliders with raw ops. Check each against the new arena primitives (Task 4.1).
  - Also run `just test-one game "--test cell_activation --test cell_manager"` in Task 5.4.
- [ ] **Step 2: Flip hashing to `StateHash`, then remove `OrderedFloat`.**
  - Change the `#[rollback]` macro's generated `Undo<T>` bound from `Hash` to `StateHash` (`crates/macros/src/lib.rs` ~:740). Make the Full-mode `hash_data()` (~:864), the Delta footprint digests, the journal's `pre_digest`/`revert_digest`, and `game::state_hash` (`lib.rs:72`) hash through `StateHash`.
  - Make the macro derive `StateHash` instead of `Hash` on the module's structs (~:293); game types outside the module switch their derives too.
  - Then remove `OrderedFloat` from game state (D3).
  - Run `just test-one game "--test hash_mode --test simple --test log_model --test hash_restore"`.
- [ ] **Step 3: Migrate `crates/game/tests/`,** mechanically with the same type map. Don't change any assertion's meaning. Hash constants in `scene_hash_golden.rs` and `wasm_determinism.rs` are re-recorded in Task 5.4, not here.
- [ ] **Step 4: Run `cargo check -p game --tests`.**

### Task 5.3: Migrate `client`, `server`, `worldgen`

- [ ] **Step 1: client.**
  - `renderer/convert.rs`: `Pose` → Bevy `Transform`. Bevy uses glam 0.32 and rapier glam 0.33, so convert through `to_array()`/`from_array` and never assume the types are the same.
  - `instance.rs`, `local_server.rs`, `renderer/bridge.rs`, `renderer/collider_debug.rs`, and the tests `reconcile_keepup.rs`.
- [ ] **Step 2: worldgen.** `examples/narrow_phase_probe.rs`: migrate it or delete it with a note if it's obsolete.
- [ ] **Step 3: Check everything compiles.** `just smoke` must pass: it type-checks client, server and tests.

### Task 5.4: Behavioural verification (the rig from Phase 0)

- [ ] **Step 1: Port-parity tests, with no re-recording allowed.** `just test-one game "--test physics_goldens"` must pass within the recorded tolerances, and so must `gjk_upstream`, `voxels_upstream`, `nalgebra_fixes` (if nalgebra is still reachable; otherwise delete it with a note), `shape_cast_grazing`, `sunk_body`, `jump_arc`, `pair_filter` (as inverted in 3.5).
- [ ] **Step 1b: Add the centred-capsule check** to `gjk_upstream.rs`: `capsule_centred_touching_cuboid_gets_face_normal`, a `Capsule::new_y(0.7, 0.3)` centred at (0, 1, 0) on a `Cuboid(8, 8, 8)` centred at (0, -8, 0), via `query::contact` with prediction 0. `normal1` must be +Y within 1e-4. It fails on the nalgebra-era fork (stale 45° direction, same as parry 0.25.3) and passes on parry 0.31.1; see the spec's "Landed" section.
- [ ] **Step 2: Walking smoothness.** `just test-one game "--test walking_smoothness -- --include-ignored"`.
- [ ] **Step 3: Rollback exactness.** `just test-one game "--test step_journal --test kinematic --test slime --test random_ops --test rollback_restore --test hash_mode --test state_hash --test block_edit --test hash_restore --test collider_genesis --test cell_activation --test cell_manager"`.
- [ ] **Step 4: Handoff and threading.** `just test-one game "--test handoff_state --test handoff_scan --test handoff_protocol --test handoff_manager"`, then `just test-threads`.
- [ ] **Step 5: If a golden in Step 1 diverges,** diagnose it (superpowers:systematic-debugging) before touching tolerances. The legitimate reasons are the upstream GJK/TOI fixes (already in Plan 1, so they shouldn't show up here) and D6/D7 choices. Write every tolerance change and its reason into the spike doc.
- [ ] **Step 5b: Cross-target mass properties for NON-fixed bodies (H5 review finding).** Task 3.1 keeps fixed bodies away from glamx 0.3.1 `SymmetricEigen3`, but that solver calls inherent std `r.acos()` / `phi.cos()` (`glamx/src/eigen3.rs:101-106`; the `libm`/`nostd-libm` features don't reroute them), and rapier calls it for every kinematic/dynamic body's collider mass (`rigid_body_components.rs` recompute + `attach_collider`). Today's Y-capsule at the body origin has a diagonal inertia and takes the `p1 == 0` early-out, so nothing diverges yet; the first offset/rotated or compound collider on a moving body (a Blockbench mob, a dropped item) gets a principal frame that can differ between native and wasm32, and `local_mprops` is serialized and hashed. Before Step 6: add a kinematic body with an offset, rotated cuboid to the `wasm_determinism.rs` scene; if `just determinism-wasm` then diverges, add a `// mmo-fork:` in rapier-next that routes that eigen solve through `libm::acosf`/`libm::cosf` (no glamx fork, per D4).
- [ ] **Step 6: Re-record the hash goldens** `scene_hash_golden.rs` and `wasm_determinism.rs` once, with a commit message saying it's the physics-engine switch. Then run `just determinism-wasm`: native and wasm hashes must be equal.
- [ ] **Step 7: Commit.**

### Task 5.5: wasm build and live check

- [ ] **Step 1: Build for wasm.** Run `just web` (compile check) and the WebGL2 / per-chunk path build.
- [ ] **Step 2: Ask the user before running the game.** Then do a live run of server + one client (`just adapter=6700 run`, `world=small`): walk over chunk seams, jump, break/place blocks, cross a region seam, watch slimes. Check the log for mispredicts (`PERF_LOG=1`: `[client] mispredicts/s`) against a pre-switch run.
- [ ] **Step 3: Browser check.** The wasm client connects to the native server and walks without desync.

---

## Phase 6 — Cleanup and docs

### Task 6.1: Documentation and memory

- [ ] **CLAUDE.md "Vendored Forks":** rewrite it. Remaining forks: rapier/parry (the `// mmo-fork:` patches, `grep -rn "mmo-fork" crates/rapier crates/parry` lists them), slotmapd, block-mesh. Determinism now comes from upstream features. Remove the `simba` libm_force / `ordered-float` wording and the "don't switch back to crates.io" rule (per D4). Replace "Bevy 0.18" with the actual pin while you're there.
- [ ] **CLAUDE.md `crates/macros` paragraph:** `Hash` → `StateHash` (and mention the `state-hash` crates).
- [ ] **Memory:** update `vendored-deps-update-investigation.md` to IMPLEMENTED, with the commit range; mark `undo-hashing-hotspots`, `narrow-phase-fixed-fixed-flood` and `wasm32-usize-sentinel-hazard` as superseded where upstream now covers them.

### Task 6.2: Broad sweep (ask first)

- [ ] **Step 1: Ask the user.** Then run `just test-full` and fix anything real.
- [ ] **Step 2: Commit.**

### Task 6.3: Final review — fork divergence from upstream

The last task. Measures how far our rapier/parry forks have drifted from pristine upstream, and reviews every hunk: what is necessary, what isn't, and what can be cleaned up. It proposes; it doesn't delete anything without the user's OK.

**Files:** Create `docs/superpowers/specs/<date>-physics-fork-divergence-review.md`.

- [ ] **Step 1: Produce the diff.** Two independent baselines, which must agree:
  - the pristine import commit from Task 2.1 (`IMPORT` = its hash): `git diff --stat IMPORT:crates/rapier-next HEAD:crates/rapier` and the same for parry (tree-ish paths, since Task 5.1 renamed the dirs), then the full diff into the job scratch dir;
  - a fresh clone of upstream at the imported tags (`v0.36.0`, `v0.31.1`) into scratch: `git diff --no-index` against `crates/rapier` / `crates/parry`. Also note whether a newer 0.36.x / 0.31.x release exists and what it changed in files we patch.
  - Record: files touched, `+/-` lines per crate, `mmo-fork:` marker count, and the old forks' numbers from Task 5.1 Step 0 for comparison.
- [ ] **Step 2: Classify every hunk** in a table (file, lines, category, pinning test, verdict):
  - **Required patch** (Phase 3/4: mass-props skip, voxels cast, continuous surfaces, KCC snap/`depenetrate`, arena primitives, `StepJournal` hooks, memo invalidation) — must name the test that fails without it;
  - **`StateHash` derives / hand impls** (mechanical; count them);
  - **Rename / packaging** (Cargo.toml, `[lib] name`);
  - **Unmarked** — any hunk without a `// mmo-fork:` marker is a finding: mark it, justify it, or remove it;
  - **Superseded** — upstream now does the same thing (check the newer release too);
  - **Dead / leftover** — debug code, commented-out blocks, unused helpers, patches whose game-side caller is gone;
  - **Upstreamable** — generic fixes (e.g. the voxels cast defects) worth offering as upstream PRs, which would shrink the fork later.
- [ ] **Step 3: Check necessity where it's in doubt.** For a patch whose need is unclear, find its pinning test (`physics-next` or `crates/game/tests/`) and reason from what it asserts. Only if that's inconclusive, revert that one hunk in the working tree (fork files only, after checking `git status` shows no other change to that file), run the named test with `just test-one`, and restore the file. Never `git stash`; never touch the user's uncommitted files.
- [ ] **Step 4: Also look outside the forks:** `crates/physics-next` (does it still earn its keep, or should its tests move next to the forks?), `crates/state-hash*`, `slotmapd` (diff against upstream slotmap for the rollback inverses only), and any game-side workaround made unnecessary by upstream (e.g. `depenetration_lift` vs #485, the fixed-fixed pair filter vs upstream's).
- [ ] **Step 5: Write the review doc.** Totals and before/after comparison first; then the hunk table; then a ranked cleanup list (what to remove, what to upstream, what must stay and why), each with its risk and the test that proves it.
- [ ] **Step 6: Stop and report to the user** with the cleanup list. Apply cleanups only after they approve, one commit per item, each verified with its pinning test and `just smoke`.
- [ ] **Step 7: Final commit** (the review doc, and any approved cleanups).
