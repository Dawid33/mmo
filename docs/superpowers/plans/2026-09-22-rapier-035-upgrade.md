# Rapier 0.35 / parry 0.30 (glam) Upgrade Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the nalgebra-era rapier 0.31 / parry 0.25.3 forks with forks of upstream rapier 0.35.3 / parry 0.30.2 (glam via `glamx`, `f32` scalar). Keep bit-exact rollback, client/server determinism (native and wasm32), and today's movement behaviour, and shrink the fork diff to a handful of hooks.

**Architecture:**
- **Land the work without breaking `develop`.**
  - The new forks are vendored next to the old ones under distinct package names (`rapier3d-next`, `parry3d-next`) and get their own test harness crate, `crates/physics-next`. Every fork patch and the new rollback journal land and get tested there, without touching the game.
  - One short "switch" phase then moves the game over, renames the packages back and deletes the old forks.
- **Rollback becomes a hybrid journal:**
  - first-touch value capture of `RigidBody`/`Collider` inside the sets' internal accessors;
  - per-tick wholesale clones of `IslandManager`, `NarrowPhase`, `BroadPhaseBvh` and the joint sets;
  - a fresh `PhysicsPipeline` + `CCDSolver` after every revert.
- **Hashing moves from `derive(Hash)` over `OrderedFloat` to crc32 over canonical bincode bytes.** Upstream contracts byte-identical snapshot round-trips.

**Tech Stack:** Rust; rapier3d 0.35.3, parry3d 0.30.2, glamx 0.3 (glam 0.33), simba 0.10, nalgebra 0.35 (rapier-internal only); bincode 1.3; crc32fast; wasm-bindgen-test-runner + node for wasm32 goldens.

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
| D1 | Rollback mechanism | Hybrid journal (capture on first touch + small wholesale clones), not full snapshots | A full clone is ~170 µs and ~1 MB per region per tick in the spike, dominated by one fixed body per chunk; hybrid is ~10–20 µs. Confirmed or overturned by the Phase 1 spike at real region scale. |
| D2 | State hashing | crc32 over bincode bytes behind a `StateDigest` trait. The `#[rollback]` macro's `Hash` bound becomes `StateDigest`, with a blanket impl for `T: Serialize`. | glam `f32` types have no `Hash`, and re-adding ~140 derives plus custom impls on glam types is a permanent fork burden. Upstream contracts canonical bytes (`tests/snapshot_roundtrip.rs`, `snapshot_portability.rs`). Voxel shapes keep a cached content digest so hashing a collider doesn't re-serialize its voxel data. |
| D3 | `OrderedFloat` in game state | Remove it. Game-owned floats become plain `f32`; glam types are used directly. | Its only job was `Hash`/`Ord` for digests, which D2 replaces. |
| D4 ⚠️ | Forks that survive the switch | **Keep vendored:** `rapier3d`, `parry3d` (fork patches), `slotmapd` (rollback inverses), `block-mesh` (unchanged). **Move to crates.io:** `simba` 0.10, `nalgebra` 0.35, `approx`, `ordered-float` (or drop). | Upstream now gives determinism through features (`libm_force`, `glamx/libm`, `scalar-math`). The math forks existed only for the `OrderedFloat` scalar. **This reverses the CLAUDE.md rule "don't switch these back to crates.io", so it needs the user's OK.** |
| D5 ⚠️ | How the multi-week port lands | Side-by-side vendoring (`crates/rapier-next`, `crates/parry-next`, `crates/physics-next`) on `develop`, then one switch commit series | CLAUDE.md forbids worktrees, and the user edits `develop` concurrently, so a long-lived branch would need constant rebasing of a huge diff. **Needs the user's OK.** |
| D6 | `IntegrationParameters::length_unit` | Decided in the Phase 1 spike: 1.0 (today's behaviour) vs `UNITS_PER_METER` | It scales the broad-phase skin, contact recycling and sleep thresholds, so it sets how often the BVH, and hence the journal, is dirty. Pick the value that keeps `walking_smoothness` green with fewer dirty ticks. |
| D7 | Character-controller depenetration | Keep `movement::depenetration_lift`; disable upstream's zero-motion depenetration (#485) if the two fight | Our lift is pinned by `sunk_body.rs`; having two depenetrations is a jitter source. Decided in Task 3.4. |
| D8 | Upstream parry #106 (touching = `Converged` at TOI 0) | Accept it, and rework our two separated-start cast filters to `toi == 0 && normal·vel >= 0` | It's in 0.30.2, and reverting it would be a new fork patch. Pinned by `shape_cast_grazing.rs`. |

## Review Focus

1. **A rollback across a tick where the staged solver wrote a body the journal didn't capture.** The capture hooks sit in the set accessors, but the staged solver writes through a raw pointer. Task 4.2 adds a debug assert, and the revert fuzzer must hit wake-up, island-merge and removal ticks.
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

- [ ] **Step 1: Write the scenario list and harness.** Reuse the setup helpers `walking_smoothness.rs` and `jump_arc.rs` already use; read both first and call the same functions rather than copying them. Scenarios, each 120 ticks:
  - `walk_flat_across_chunk_seam`: +x at walk speed, starting 2 units before a chunk boundary.
  - `walk_scale_seam`: block-scale onto a half-block (chisel) surface.
  - `step_up_one_block`
  - `terrace_uphill_1_voxel`: the known speed-dip case.
  - `jump_in_place`
  - `sprint_jump_over_gap`
  - `sunk_start_depenetrates`: starts 0.3 units inside terrain.
  - `slime_hop_sequence`: one slime, fixed seed.

- [ ] **Step 2: Add the record mode.** When `PHYSICS_GOLDENS_RECORD=1`, write the CSVs. Otherwise compare, with tolerances as named constants:
  - `POS_TOL = 0.05 * UNITS_PER_METER / 16.0`, i.e. 0.05 units per axis per tick;
  - `grounded` must match exactly except within `GROUNDED_SLACK_TICKS = 2` of a transition.

  On failure, print the first diverging tick and the full row pair.

- [ ] **Step 3: Record, then run in compare mode.** Run `PHYSICS_GOLDENS_RECORD=1 just test-one game "--test physics_goldens"`, then `just test-one game "--test physics_goldens"`. Expected: PASS. Run it twice more to prove it's stable, i.e. deterministic.

- [ ] **Step 4: Sanity-check the fixtures by eye.** For `jump_in_place`, peak height ≈ the value `jump_arc.rs` pins. For `walk_flat_across_chunk_seam`, y is constant within 1e-3. If not, the harness is wrong: fix it before committing.

- [ ] **Step 5: Commit.** `test(game): behaviour goldens for movement, recorded on the nalgebra-era physics`.

### Task 0.2: State-hash golden for a canonical scene (same-engine regression only)

**Files:**
- Create: `crates/game/tests/scene_hash_golden.rs`

- [ ] **Step 1: Write the test.**
  - Build a canonical scene: the fixed worldgen region `RegionCoords { x: 0, z: 0 }` at `set_near_radius(0)` scale if applicable; one player; one slime; 60 ticks of scripted input.
  - Assert `game::state_hash(&rollback)` equals a constant.
  - Include in the doc comment the regeneration command (`SCENE_HASH_PRINT=1 ... -- --nocapture`), and the rule: the constant changes only in a commit that says why.

- [ ] **Step 2: Record the constant, then assert.** Tag the test slow if it takes more than 100 ms (region genesis likely does).
- [ ] **Step 3: Commit.** `test(game): canonical scene state-hash golden`.

### Task 0.3: Cross-target determinism check (native vs wasm32)

**Files:**
- Create: `crates/game/tests/wasm_determinism.rs` (native side) and `crates/game/tests/wasm/` or a `#[wasm_bindgen_test]` module in the same file (check how/if the workspace already uses `wasm-bindgen-test`: `grep -rn wasm_bindgen_test crates`)
- Modify: `justfile` (new recipe `determinism-wasm`)

- [ ] **Step 1: Write a scene that fits wasm.** Kinematic capsule on a small hand-built voxels floor (8×8 chunks' worth of `Voxels`) plus one slime, 120 ticks. It uses no worldgen, so the wasm build stays small. Compute `state_hash`.
- [ ] **Step 2: Native side.** `#[test] fn scene_hash_native()` asserts the constant `WASM_SCENE_HASH`.
- [ ] **Step 3: wasm side.** `#[wasm_bindgen_test] fn scene_hash_wasm()` asserts the same constant.
- [ ] **Step 4: Add the justfile recipe.**

```just
# Cross-target determinism: the same physics scene must hash identically on
# native and wasm32 (node via wasm-bindgen-test-runner).
determinism-wasm:
    {{cap}} {{cargo}} test -p game --test wasm_determinism
    CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner {{cap}} {{cargo}} test -p game --test wasm_determinism --target wasm32-unknown-unknown
```

- [ ] **Step 5: Run both.** Expected: the same hash. If `game` doesn't build for wasm32 test targets, e.g. because of dev-deps, scope the dev-deps with `[target.'cfg(not(target_arch = "wasm32"))'.dev-dependencies]`. If it still can't be made to run in ≤ 1 h, record that in the commit and fall back to comparing a native hash against one printed by the browser client in a debug build (`?offline`). This test is the only proof of cross-target determinism, so don't drop it silently.
- [ ] **Step 6: Commit.** Include CLAUDE.md: add `just determinism-wasm` to the verification list.

### Task 0.4: Port Plan 1's upstream-fix tests into the rig

- [ ] **Step 1: Check where Plan 1 stands.** It is `docs/superpowers/plans/2026-09-22-physics-fork-fixes.md`, and it lands first. `gjk_upstream.rs`, `voxels_upstream.rs` and `nalgebra_fixes.rs` exist and are green. Those tests cover fixes that are *in* parry 0.30.2, so they must stay green after the switch. That's the check that the upstream versions behave like our hand ports.
- [ ] **Step 2: Add a module doc line to each file:** "Must stay green across the rapier 0.35 switch (upgrade plan Phase 5)."

---

## Phase 1 — Spikes and decision gate (scratch crate, nothing merged into the game)

### Task 1.1: Real-scale clone and hash cost

**Files:**
- Create: `crates/physics-next/` (workspace member, `publish = false`), depending on crates.io `rapier3d = { version = "=0.35.3", features = ["enhanced-determinism", "serde-serialize"] }` and `bincode`, `crc32fast`. Phase 2 swaps this for the vendored fork.
- Create: `crates/game/examples/export_region_colliders.rs`. It writes, for one generated region, every chunk collider's `(chunk origin, voxel_size, Vec<[i32;3]>)` or cuboid half-extents to `target/physics-next/region_<x>_<z>.bin` (bincode).
- Create: `crates/physics-next/examples/clone_cost.rs`

- [ ] **Step 1: Write the exporter.** It uses `game`'s genesis path (`seed_terrain_genesis`, `state.rs:565`) to get exactly the production collider set; read that function and walk `data.physics.colliders`. Add a `just` recipe `export-region` only if it's worth a one-word command; otherwise document the `cargo run --example` line in the file header.
- [ ] **Step 2: Write `clone_cost.rs`.**
  - Load the export.
  - Build a 0.35 world with the same bodies and colliders (fixed body per chunk, as the game does today), plus 4 kinematic capsules on the surface, moved each tick.
  - Measure over 600 ticks: clone time per structure, wholesale `(IslandManager, NarrowPhase, BroadPhaseBvh)` clone, full 7-structure clone, bincode size, crc32 of bincode per structure, first step after `PhysicsPipeline::new()`.
  - Print a table.
- [ ] **Step 3: Ask the user before running it** (timing run). Then run it in release, pinned to one core (`taskset -c 2`), and paste the table into `docs/superpowers/specs/2026-09-22-rapier-035-spike-results.md`.
- [ ] **Step 4: Gate D1.** Keep the hybrid if the wholesale-small-structures clone costs ≤ 50 µs/tick at real scale. If a full clone is within 2× of that, prefer full snapshots: they're simpler and upstream-guaranteed. Record the decision in the spike doc.
- [ ] **Step 5: Gate D2 cost.** If the crc32 of bincode of the whole `ColliderSet` is more than 1 ms, the Full-mode digest needs the cached per-shape digest (Task 4.4). Record which.
- [ ] **Step 6: Commit** the crate, exporter and spike doc.

### Task 1.2: Upstream voxels cast and KCC against our movement tests

**Files:**
- Create: `crates/physics-next/tests/upstream_movement_probe.rs`

- [ ] **Step 1: Port the pure-parry cases.** Take the five tests in `crates/game/tests/shape_cast_grazing.rs` and translate them to the glam API (`Vec3`, `Pose`; `Voxels::new(Vec3, &[IVec3])`), running against crates.io parry 0.30.2.
- [ ] **Step 2: Run them and record which fail.** Expected from the research: the search-domain and internal-edge cases fail upstream; the grazing-start case may pass (partial upstream fallback). This list is the re-port scope for Task 3.2.
- [ ] **Step 3: Run a KCC probe.** Capsule on a flat voxels floor, `move_shape` with zero desired translation after placing it 0.3 units sunk. Does upstream's #485 depenetration push it out, and along which normal? Record it for D7.
- [ ] **Step 4: Run a `length_unit` probe (D6).** Repeat a 120-tick walk across a chunk seam with `length_unit = 1.0` and `= 16.0`. Record the dirty-BVH tick count and the max |Δy|.
- [ ] **Step 5: Write up and commit.** Append the results to the spike doc, then commit.

### Task 1.3: Decision checkpoint with the user

- [ ] **Step 1: Summarise.** Write D1, D2, D4, D5 and D6 with the spike numbers at the top of the spike doc.
- [ ] **Step 2: Stop and ask the user to confirm D4 and D5** (and D1 if the spike flipped it). Don't start Phase 2 without that.

---

## Phase 2 — Vendor the new forks side by side

### Task 2.1: Import pristine sources

**Files:**
- Create: `crates/rapier-next/` (pristine rapier 0.35.3 git tag, `src/` + `crates/rapier3d/Cargo.toml` layout as upstream)
- Create: `crates/parry-next/` (pristine parry 0.30.2)
- Modify: root `Cargo.toml` `[workspace] members` and `[workspace.dependencies]`

- [ ] **Step 1: Import from the tagged git sources**, not the crates.io tarballs, so upstream tests come along: `git clone --depth 1 --branch v0.35.3 https://github.com/dimforge/rapier` into `$CLAUDE_JOB_DIR`-style scratch, then copy. Check the tag names with `git ls-remote --tags`. Import commit message: `vendor: pristine rapier 0.35.3 + parry 0.30.2 as *-next (unused)`. Keep it a pure import, so the fork diff is always `git diff <import-commit> -- crates/rapier-next`.
- [ ] **Step 2: Rename the packages** to `rapier3d-next` / `parry3d-next`, with `[lib] name = "rapier3d"` / `"parry3d"`, so source paths inside the forks need no edits. Point rapier-next's parry dependency at `parry3d-next` via `package = "parry3d-next"`.
- [ ] **Step 3: Switch `physics-next` to `path` deps on the new forks** with `enhanced-determinism, serde-serialize`. Run `just test-one physics-next "--tests"`. Expected: the Task 1.2 probe gives the same results as against crates.io.
- [ ] **Step 4: Confirm the game build is untouched.** `cargo tree -p game | grep -c next` must print 0. Run `just smoke`.
- [ ] **Step 5: Run upstream's determinism tests from the vendored copy.** `snapshot_roundtrip`, `snapshot_portability`, `simd_backend_parity`, `thread_count_determinism` (without `parallel`: skip that one if it's feature-gated). If upstream's test dirs don't build inside our workspace, copy those four files into `crates/physics-next/tests/upstream/`. Commit.

---

## Phase 3 — Re-apply the fork's physics patches (each pinned in `physics-next`)

Every task follows the same cycle:
1. Port the pinning test from `crates/game/tests/` into `crates/physics-next/tests/`, translated to glam.
2. See it fail on the pristine fork.
3. Port the patch from the old fork. The old code is at `crates/parry` and `crates/rapier`; `git log -p` on those paths shows each patch with its rationale.
4. Green, then commit.

Keep every patch small and marked `// mmo-fork:` so the fork diff stays greppable.

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
- **Apply D8:** upstream 0.30.2 already returns `Converged` for touching at TOI 0. Re-port only the parts `shape_cast_grazing.rs` still needs, and rewrite the two separated-start filters (the query pipeline, and the voxels cast ~:240) to accept a hit at `toi == 0` only if `normal · vel < 0`.
- **Tests:** all of `shape_cast_grazing.rs`, translated.

### Task 3.4: KCC: snap gate, prediction-loosened ground manifolds, depenetration (D7)

- **Old patch:** `crates/rapier/src/control/character_controller.rs:463-486` (`predict_ground`, `.loosened(prediction)`), and the snap gate (grep `snap` in the old fork's `git log -p`).
- **Target:** `crates/rapier-next/src/control/character_controller.rs` ~:236-300 (#485 depenetration) and ~:443-470 (snap).
- **D7:** if Task 1.2 showed upstream's zero-motion depenetration moving the body differently from `depenetration_lift`, gate it behind a new field `KinematicCharacterController::depenetrate: bool`, defaulting to upstream's `true`. The game sets `false`. The test is `sunk_body.rs`, translated.

### Task 3.5: Pair filter parity and guards

- Upstream has same-body, active-type and group filtering, with forced re-insertion (`broad_phase_bvh/update.rs:73-110, 333-395`). Our fork's fixed-fixed guards (`set_body_type` / `set_parent` panics) are no longer needed; don't port them.
- **Test:** port all of `crates/game/tests/pair_filter.rs`. Then invert `fixed_kinematic_pair_is_not_suppressed`: under upstream, default `ActiveCollisionTypes` filters kinematic↔fixed at creation. Assert the pair is absent, and that it appears after `set_body_type(Dynamic)` (re-insertion). Delete the two "panics" tests with a commit note.
- Grep the game for any read of kinematic↔fixed contact pairs (`contact_pair`, `contact_pairs_with`) before the switch; there were none when this plan was written.

### Task 3.6: `length_unit` (D6)

- Apply the Task 1.2 choice as a named constant in the game at switch time (`movement::PHYSICS_LENGTH_UNIT`). There's no fork change; record it in the spike doc now.

---

## Phase 4 — Rollback journal and digests on the new engine

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
- [ ] **Step 3: Port the implementations.** Old code: `crates/rapier/src/data/arena.rs:370-402`, `rigid_body_set.rs:124-220`, `collider_set.rs:111-154`. Upstream 0.35 arenas: check for changes to the free-list layout before copying.
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
- [ ] **Step 2: Implement.** Add `PhysicsPipeline::invalidate_caches(&mut self)` and `CCDSolver::invalidate_caches(&mut self)`, which clear `joint_constraint_indices`, `joint_selection_primed`, the staged-solver assembly cache and `fixed_targets_cache`. `StepJournal::revert` can't reach the pipeline, so the game calls these after revert. If (a) doesn't diverge, keep the call anyway: it's cheap insurance, and the test pins it.
- [ ] **Step 3: Commit.**

### Task 4.4: `StateDigest` and physics digests

**Files:** Create `crates/game/src/digest.rs`, `crates/game/tests/state_digest.rs`; Modify `crates/parry-next/src/shape/voxels/voxels.rs` (cached content digest, digest mode). At the switch (Task 5.2): `crates/macros/src/lib.rs` (the `Undo<T>` bound ~:740, `hash_data()` ~:864) and `crates/game/src/lib.rs` (`state_hash` :72).

**Interfaces:**

```rust
/// Canonical content digest used by rollback verification and state_hash.
pub trait StateDigest {
    fn digest(&self, h: &mut crc32fast::Hasher);
}
impl<T: serde::Serialize> StateDigest for T {
    fn digest(&self, h: &mut crc32fast::Hasher) {
        bincode::serialize_into(DigestWriter(h), self).expect("digest serialize");
    }
}
```

`DigestWriter` is an `io::Write` that feeds bytes to the hasher without allocating.

- [ ] **Step 1: Write a standalone test first.**
  - Add `StateDigest`, `DigestWriter` and the blanket impl to `crates/game/src/digest.rs`, and export them from `lib.rs`.
  - Test it in `crates/game/tests/state_digest.rs`:
    - a struct holding a plain `f32` and a `Vec<u32>` digests equal after a bincode round trip;
    - it digests differently after any field changes;
    - `-0.0` and `0.0` digest differently, which documents that digests are bit-level.
- [ ] **Step 2: Voxels digest mode in `parry-next`.**
  - Problem: upstream `Voxels` serializes its whole voxel data, so a Full-mode digest of a region's colliders would re-encode megabytes per log entry.
  - Carry the old fork's cached content hash over (old code `crates/parry/src/shape/voxels/voxels.rs:516`), maintained on every edit, as `Voxels::content_digest() -> u32`.
  - Add one fork patch: a `thread_local!` flag `parry3d::shape::DIGEST_MODE` that, while set, makes `Voxels`' `Serialize` emit `(voxel_size, content_digest)` instead of the data.
  - `DigestWriter` users set it with a scope guard (`parry3d::shape::digest_scope(|| ...)`). It must never be set during wire or parking serialization. Assert that in `region_runner`'s park path at the switch.
  - Test: port `voxels_hash_cache.rs` into `physics-next`. Also assert the normal serialization round trip still carries the full data when the flag is off.
  - If Task 1.1 measured the whole-`ColliderSet` bincode digest at ≤ 1 ms, skip this step and note that in the spike doc.
- [ ] **Step 3: Commit.** The macro's `Hash` bound is flipped to `StateDigest` in the switch (Task 5.2, Step 2), not here. The old forks' voxels serialize their full data, so flipping earlier would make every Full-mode test re-encode terrain.

---

## Phase 5 — The switch (short; the game moves in one commit series)

### Task 5.1: Point the workspace at the new forks

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

- [ ] **Step 1: Migrate the files** in dependency order, with `cargo check -p game` after each: `lib.rs` (the re-exports `pub use parry3d as parry`, `na`) → `protocol.rs` → `state.rs` → `physics.rs` (journal call with `PhysicsParts`, then `invalidate_caches`) → `movement.rs` (`length_unit` constant from D6, `depenetrate = false` from D7) → `camera.rs` (keep `yaw_quat` libm-forced: `libm::sinf`/`cosf`) → `edit.rs` → `slime.rs` → `debug_render.rs` → `input.rs`.
- [ ] **Step 2: Flip hashing to `StateDigest`, then remove `OrderedFloat`.**
  - Change the `#[rollback]` macro's generated `Undo<T>` bound from `Hash` to `StateDigest` (`crates/macros/src/lib.rs` ~:740). Make the Full-mode `hash_data()` (~:864) and `game::state_hash` (`lib.rs:72`) digest through `StateDigest`, inside `parry3d::shape::digest_scope`.
  - Stop the macro deriving `Hash` on the module's structs (~:293).
  - Then remove `OrderedFloat` from game state (D3).
  - Run `just test-one game "--test state_digest --test hash_mode --test simple --test log_model"`.
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
- [ ] **Step 2: Walking smoothness.** `just test-one game "--test walking_smoothness -- --include-ignored"`.
- [ ] **Step 3: Rollback exactness.** `just test-one game "--test step_journal --test kinematic --test slime --test random_ops --test rollback_restore --test hash_mode --test state_hash --test block_edit --test hash_restore --test collider_genesis"`.
- [ ] **Step 4: Handoff and threading.** `just test-one game "--test handoff_state --test handoff_scan --test handoff_protocol --test handoff_manager"`, then `just test-threads`.
- [ ] **Step 5: If a golden in Step 1 diverges,** diagnose it (superpowers:systematic-debugging) before touching tolerances. The legitimate reasons are the upstream GJK/TOI fixes (already in Plan 1, so they shouldn't show up here) and D6/D7 choices. Write every tolerance change and its reason into the spike doc.
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
- [ ] **CLAUDE.md `crates/macros` paragraph:** `Hash` → `StateDigest`.
- [ ] **Memory:** update `vendored-deps-update-investigation.md` to IMPLEMENTED, with the commit range; mark `undo-hashing-hotspots`, `narrow-phase-fixed-fixed-flood` and `wasm32-usize-sentinel-hazard` as superseded where upstream now covers them.

### Task 6.2: Broad sweep (ask first)

- [ ] **Step 1: Ask the user.** Then run `just test-full` and fix anything real.
- [ ] **Step 2: Final commit.**
