# Region-Genesis Cost Reduction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut `just test-full` wall time roughly in half by removing redundant work from region genesis (undo digests, discarded mass properties, repeated worldgen), per `docs/superpowers/specs/2026-07-11-genesis-cost-reduction-design.md`.

**Architecture:** Three independent changes: (1) skip voxel mass-property computation for fixed bodies in the vendored rapier fork; (2) make `seed_terrain_genesis`'s ECS writes raw/unlogged like its physics writes already are; (3) move the dev-only `WorldgenCache` from `crates/server` to `crates/worldgen` and use it from the client's `LocalServer` too.

**Tech Stack:** Rust workspace; vendored rapier fork (`crates/rapier`); proc-macro-generated rollback wrappers (`crates/macros`); bincode/crc32fast.

## Global Constraints

- **No git worktrees.** Work directly on `develop` in this checkout (CLAUDE.md).
- Rollback correctness bar: `hash(before) == hash(after undo)`, bit-exact. Tests run with hash mode `Full` (the process-global default); do not change hash modes or test fixtures.
- `game` and `server` stay Bevy-free and windowing-free; determinism-sensitive math stays inside the vendored forks.
- `just test` for routine checks while iterating; `just test-full` as the final gate. Tag any new test slower than 100ms with `#[ignore = "slow: >100ms — run via just test-full"]`.
- Keep the `justfile` in sync when build/test commands change.
- Commit messages end with `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

### Task 1: Skip mass properties for fixed bodies (vendored rapier fork)

Voxel mass properties (`from_voxels`: two full passes over all voxel cells plus a 3×3 shifted-inertia matrix per solid cell, in OrderedFloat/libm math) are computed for every chunk collider and then discarded, because chunk bodies are `RigidBodyType::Fixed` and `update_world_mass_properties` zeroes their effective inverse mass/inertia. ~17% of the hot tests' profile; also the ~318ms/region first-tick join spike.

The fork already forbids fixed→non-fixed transitions while colliders are attached (`set_body_type` asserts, `crates/rapier/src/dynamics/rigid_body.rs:259`), so the only transition needing a recompute guard is a body whose colliders were all removed while it was fixed and which then turns dynamic — handled by adding `RigidBodyChanges::TYPE` to the recompute trigger.

**Files:**
- Test: `crates/game/tests/collider_genesis.rs` (append)
- Modify: `crates/rapier/src/dynamics/rigid_body_components.rs:1130-1135` (`attach_collider`)
- Modify: `crates/rapier/src/pipeline/user_changes.rs:174-186` (`handle_user_changes_to_rigid_bodies`)

**Interfaces:**
- Consumes: `RigidBodyType::is_fixed()` (`rigid_body_components.rs:88`), `RigidBodyMassProps::update_world_mass_properties` (`rigid_body_components.rs:487`).
- Produces: no API changes — behavior only. Fixed bodies keep `local_mprops == MassProperties::default()` unless they were dynamic when it was computed.

- [ ] **Step 1: Write the failing test**

Append to `crates/game/tests/collider_genesis.rs` (it already defines `build()` returning a `Region` with one full and one partial chunk):

```rust
#[test]
fn fixed_terrain_bodies_skip_collider_mass_properties() {
    let region = build();
    let physics = &region.data().physics;
    let mut checked = 0;
    for (_, body) in physics.bodies.iter() {
        assert!(body.is_fixed(), "genesis bodies are fixed terrain");
        // Mass properties of fixed bodies are discarded (effective inverse
        // mass/inertia is zeroed), so the expensive from_voxels computation
        // must be skipped entirely: local mass stays at its default (0).
        assert_eq!(
            body.mass(),
            Real::from(0.0),
            "fixed terrain body computed collider mass properties"
        );
        checked += 1;
    }
    assert_eq!(checked, 2, "one body per chunk");
}
```

Note: the accessor path mirrors the existing `full_chunk_becomes_cuboid_partial_stays_voxels` test (`region.data().physics.colliders`), swapping `colliders` for `bodies`. `Real` is already imported at the top of the file (`use game::parry::math::Real;`). If `body.mass()` doesn't resolve, use `body.mass_properties().local_mprops.mass()`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test collider_genesis fixed_terrain_bodies_skip_collider_mass_properties`
Expected: FAIL — mass is non-zero (the full chunk's cuboid collider currently contributes density-derived mass).

- [ ] **Step 3: Skip the eager computation in `attach_collider`**

In `crates/rapier/src/dynamics/rigid_body_components.rs`, replace lines 1130–1135:

```rust
        let mass_properties = co_mprops
            .mass_properties(&**co_shape)
            .transform_by(&co_parent.pos_wrt_parent);
        self.0.push(co_handle);
        rb_mprops.local_mprops += mass_properties;
        rb_mprops.update_world_mass_properties(rb_type, &rb_pos.position);
```

with:

```rust
        self.0.push(co_handle);
        // Fixed bodies discard collider-derived mass (update_world_mass_properties
        // zeroes their effective inverse mass/inertia), so skip the computation —
        // for terrain Voxels shapes it is two full passes over every voxel cell.
        // A fixed body cannot become non-fixed while colliders are attached
        // (set_body_type asserts), so no recompute path is needed here.
        if !rb_type.is_fixed() {
            let mass_properties = co_mprops
                .mass_properties(&**co_shape)
                .transform_by(&co_parent.pos_wrt_parent);
            rb_mprops.local_mprops += mass_properties;
        }
        rb_mprops.update_world_mass_properties(rb_type, &rb_pos.position);
```

- [ ] **Step 4: Skip the first-tick recompute in `handle_user_changes_to_rigid_bodies`**

In `crates/rapier/src/pipeline/user_changes.rs`, replace lines 174–186:

```rust
            // NOTE: recompute the mass-properties AFTER dealing with the rigid-body changes
            //       that imply a collider change (in particular, after propagation of the
            //       enabled/disabled status).
            if changes
                .intersects(RigidBodyChanges::LOCAL_MASS_PROPERTIES | RigidBodyChanges::COLLIDERS)
            {
                rb.mprops.recompute_mass_properties_from_colliders(
                    colliders,
                    &rb.colliders,
                    rb.body_type,
                    &rb.pos.position,
                );
            }
```

with:

```rust
            // NOTE: recompute the mass-properties AFTER dealing with the rigid-body changes
            //       that imply a collider change (in particular, after propagation of the
            //       enabled/disabled status).
            // Fixed bodies skip the recompute entirely: their collider-derived
            // mass is discarded (update_world_mass_properties zeroes effective
            // inverse mass/inertia), and for terrain chunks the recompute is
            // O(colliders * voxel cells) — the first-tick-after-load spike.
            // TYPE is part of the trigger so a body whose colliders were all
            // removed while fixed recomputes (to additional-mass-only) when it
            // later turns dynamic.
            if changes.intersects(
                RigidBodyChanges::LOCAL_MASS_PROPERTIES
                    | RigidBodyChanges::COLLIDERS
                    | RigidBodyChanges::TYPE,
            ) {
                if rb.body_type.is_fixed() {
                    rb.mprops
                        .update_world_mass_properties(rb.body_type, &rb.pos.position);
                } else {
                    rb.mprops.recompute_mass_properties_from_colliders(
                        colliders,
                        &rb.colliders,
                        rb.body_type,
                        &rb.pos.position,
                    );
                }
            }
```

- [ ] **Step 5: Run the new test and the fast suites**

Run: `cargo test -p game --test collider_genesis`
Expected: PASS (all tests in the file, including the two existing ones).

Run: `just test`
Expected: PASS. Pay attention to `game` physics tests (`kinematic.rs`, `multi_client.rs` quick subset) — they cover dynamic-body mass (player capsules) and would fail if the skip leaked beyond fixed bodies.

- [ ] **Step 6: Run the targeted slow physics tests**

Run: `cargo test -p game -- --include-ignored --test-threads=4 --skip perf_evidence`
Expected: PASS — `multi_client.rs` (`descending_player_stops_on_terrain`, capsule/collider tests) and `hash_restore.rs`/`rollback_restore.rs`/`hash_mode.rs` all green. These assert bit-exact undo hashes across physics mutations and client/server convergence, which is the determinism bar for a fork change.

- [ ] **Step 7: Commit**

```bash
git add crates/rapier/src/dynamics/rigid_body_components.rs crates/rapier/src/pipeline/user_changes.rs crates/game/tests/collider_genesis.rs
git commit -m "perf(rapier): skip collider mass-properties for fixed bodies

from_voxels was two full passes over every voxel cell per chunk collider,
computed at attach and again on the first tick after load, then discarded
because chunk bodies are fixed. ~17% of the convergence-rig test profile
and the ~318ms/region first-tick join spike.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Unlogged ECS writes in `seed_terrain_genesis`

Genesis already inserts physics bodies/colliders raw (`bodies.raw_mut().insert`, raw `insert_with_parent`) because "genesis is the base state and is never rolled back below itself" (`crates/game/src/state.rs:489-498`). The ECS half of the loop still logs: `create_entity_safe` + `chunk.set_safe` + `rigidbody.set_safe` each push an undo entry, and under the tests' `Full` hash mode each entry crc32s the whole growing component map — quadratic per region, ~27% of the hot tests' profile. `seed_terrain_genesis` is only reachable through `Region::from_chunks`/`from_columns` (fresh region construction), never on a live region.

Skipping the log also skips the `#[emit]`-derived renderer updates. That is correct: genesis runs before any subscriber sees the region — clients receive a `SerializedRegion` snapshot, not the genesis delta stream — and the client-side region load path (`instance.rs`) builds its render state from the snapshot. The convergence rigs (`harness_smoke`, `crossing`, `reconcile_keepup`) exercise exactly that path and gate this task.

**Files:**
- Test: `crates/game/tests/log_model.rs` (append)
- Modify: `crates/macros/src/lib.rs` (~line 1044, `impl UndoSlotMap` block) — add `insert_raw`
- Modify: `crates/game/src/state.rs:214-226` (`impl Undo<Ecs>`) — add `create_entity_raw`
- Modify: `crates/game/src/state.rs:499-518` (`seed_terrain_genesis`) — use the raw paths

**Interfaces:**
- Consumes: `Component<T>::insert(&mut self, e: EntityKey, item: Option<T>)` (raw, already public, `state.rs:398`); `Undo<T>::raw_mut()` (`pub(crate)`, macro-generated — usable from `state.rs` because the macro expands in the `game` crate).
- Produces: `UndoSlotMap::insert_raw(&mut self, v: V) -> K` (no log entry, no emit); `Undo<Ecs>::create_entity_raw(&mut self) -> EntityKey` (raw twin of `create_entity_safe`: inserts the entity plus all five `None` component slots).

- [ ] **Step 1: Write the failing test**

Append to `crates/game/tests/log_model.rs` (it already defines `state_hash` and `new_rollback`):

```rust
#[test]
fn genesis_is_unlogged_base_state() {
    let (mut r, recv) = new_rollback();
    r.new_transaction();
    r.seed_terrain_genesis(vec![
        (ChunkCoords::new(0, 0, 0), game::Chunk::flat_floor(8)),
        (ChunkCoords::new(1, 0, 0), game::Chunk::flat_floor(32)),
    ]);
    let h_genesis = state_hash(&r);

    // Genesis is the base state: it must produce no undo entries, so rolling
    // back the enclosing transaction must not disturb it.
    r.rollback();
    assert_eq!(
        h_genesis,
        state_hash(&r),
        "rollback must not undo genesis (genesis writes must be unlogged)"
    );

    // And no renderer emits either: the client builds a freshly loaded region
    // from its snapshot, not from genesis-time deltas.
    assert!(
        recv.try_recv().is_err(),
        "genesis must not emit GameDataUpdates"
    );

    // Post-genesis mutations still roll back exactly to the genesis boundary.
    r.new_transaction();
    r.create_player_safe(7);
    assert_ne!(h_genesis, state_hash(&r));
    r.rollback();
    assert_eq!(h_genesis, state_hash(&r), "hash(before) == hash(after undo)");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test log_model genesis_is_unlogged_base_state`
Expected: FAIL on the first assertion — today genesis logs entries, so `rollback()` tears the ECS half back out (and desyncs it from the raw-inserted physics), changing the hash. (If it instead fails on the emit assertion first, that's the same defect — proceed.)

- [ ] **Step 3: Add `insert_raw` to the generated `UndoSlotMap`**

In `crates/macros/src/lib.rs`, inside the `"impl UndoSlotMap"` `quote!` block, directly after the `insert` method (after line 1061's closing brace):

```rust
                /// Raw insert: no undo entry, no renderer emit. Only correct
                /// for genesis writes — values that are part of the base state
                /// a region is never rolled back below (see
                /// `seed_terrain_genesis`). Anywhere else this desyncs the
                /// undo log from the data.
                pub fn insert_raw(&mut self, v: V) -> K {
                    self.data.insert(v)
                }
```

- [ ] **Step 4: Add `create_entity_raw` and rewrite the genesis loop**

In `crates/game/src/state.rs`, extend the `impl Undo<Ecs>` block (lines 214–226) with a raw twin below `create_entity_safe`:

```rust
    /// Raw (unlogged, no-emit) twin of `create_entity_safe`, for genesis
    /// only: genesis is the base state and is never rolled back below
    /// itself, so these writes must not enter the undo log (under Full
    /// hash mode every logged entry re-hashes the whole component map —
    /// quadratic across a region build).
    pub(crate) fn create_entity_raw(&mut self) -> EntityKey {
        let key = self.entities.insert_raw(());
        self.camera.raw_mut().insert(key, None);
        self.isometry.raw_mut().insert(key, None);
        self.rigidbody.raw_mut().insert(key, None);
        self.chunk.raw_mut().insert(key, None);
        self.kind.raw_mut().insert(key, None);
        key
    }
```

Note: `raw_mut()` on the component wrappers yields `&mut Component<T>`, whose public `insert` (`state.rs:398`) writes the slot without logging. If `create_entity_safe` in the current tree inserts more component slots than these five, mirror the current list exactly — the raw twin must create the same slots.

In `seed_terrain_genesis` (lines 499–518), replace the three logged calls:

```rust
            let e = self.ecs.create_entity_safe();
            let cells = crate::collider_cells(&chunk.blocks, &chunk.chisel);
            self.ecs.chunk.set_safe(e, Some(chunk));
```

with:

```rust
            let e = self.ecs.create_entity_raw();
            let cells = crate::collider_cells(&chunk.blocks, &chunk.chisel);
            self.ecs.chunk.raw_mut().insert(e, Some(chunk));
```

and (line 518):

```rust
            self.ecs.rigidbody.set_safe(e, Some(handle));
```

with:

```rust
            self.ecs.rigidbody.raw_mut().insert(e, Some(handle));
```

Update the function's doc comment (lines 489–498) to say the ECS writes are raw too, e.g. append: "The ECS writes (entity + component slots) are raw for the same reason the physics inserts are — no undo entries, no digests, no renderer emits."

- [ ] **Step 5: Run the test and the rollback suites**

Run: `cargo test -p game --test log_model`
Expected: PASS, including the new test.

Run: `cargo test -p game`
Expected: PASS — `simple.rs`, `state_hash.rs`, `region_from_chunks.rs`, `voxels_hash_cache.rs` quick subsets all green.

- [ ] **Step 6: Run the slow game + client suites (the real gate)**

Run: `cargo test -p game -- --include-ignored --test-threads=4 --skip perf_evidence`
Expected: PASS — `multi_client.rs` and `random_ops.rs` cover post-genesis rollback across region content.

Run: `cargo test -p client -- --include-ignored --test-threads=4 --skip region_tick_cost_fits_keepup_budget --skip soak_realtime_keepup --skip crossing --skip reconcile`
Expected: PASS — `harness_smoke` proves the client renders freshly loaded regions from snapshots alone (no genesis emits), `instance.rs` manager tests prove region receipt/buffering. (The `crossing`/`reconcile` rigs are skipped here only for iteration speed; Task 4 runs everything.)

- [ ] **Step 7: Commit**

```bash
git add crates/macros/src/lib.rs crates/game/src/state.rs crates/game/tests/log_model.rs
git commit -m "perf(game): raw unlogged ECS writes in seed_terrain_genesis

Genesis physics inserts were already raw (base state, never rolled back
below); the ECS half still logged one undo entry per write, and under
Full hash mode each entry crc32s the whole growing component map —
quadratic per region, ~27% of the convergence-rig test profile.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Move `WorldgenCache` into `crates/worldgen`; use it from `LocalServer`

The dev-only disk cache (`crates/server/src/worldgen_cache.rs`, currently uncommitted) makes repeat generation of pristine regions a bincode decode — bit-identical by construction. The test rigs bypass it because they run `LocalServer` (client crate) and `client` doesn't depend on `server`. Move it to `worldgen` (it caches that crate's output and keys on its sources) and wire both callers. ~13% of the hot tests' profile after the first run warms the cache.

**Files:**
- Create: `crates/worldgen/src/cache.rs` (moved from `crates/server/src/worldgen_cache.rs`, adjusted)
- Modify: `crates/worldgen/src/lib.rs` (add `pub mod cache;`)
- Modify: `crates/worldgen/Cargo.toml` (promote `bincode`/`crc32fast` to `[dependencies]`, add `log`)
- Delete: `crates/server/src/worldgen_cache.rs`
- Modify: `crates/server/src/lib.rs:21` (drop `pub mod worldgen_cache;`) and `crates/server/src/lib.rs:261-274` (use the moved cache)
- Modify: `crates/client/src/blocks.rs:13` (make `manifest_src` public)
- Modify: `crates/client/src/local_server.rs:56` (wrap generation in the cache)
- Modify: `justfile` (run worldgen's tests in the `test`/`test-full` recipes)

**Interfaces:**
- Consumes: `worldgen::generate_region(rc, seed, materials) -> Vec<(ColumnCoords, Column)>`, `worldgen::material::Materials` (Copy), `game::{Column, ColumnCoords, RegionCoords}`.
- Produces: `worldgen::cache::WorldgenCache` with the existing API (`from_env(seed: u64, block_manifest: &str)`, `at_dir(PathBuf)`, `load(RegionCoords) -> Option<Vec<(ColumnCoords, Column)>>`, `store(RegionCoords, &[(ColumnCoords, Column)])`) plus new `generate_cached(&self, rc: RegionCoords, seed: u64, materials: Materials) -> Vec<(ColumnCoords, Column)>`; `client::blocks::manifest_src() -> String` (now `pub`).

- [ ] **Step 1: Move the module**

`git mv crates/server/src/worldgen_cache.rs crates/worldgen/src/cache.rs`, then adjust inside `cache.rs`:

1. `use game::{Column, ColumnCoords, RegionCoords};` stays (worldgen depends on `game`).
2. In `from_env`, gate wasm out (the browser offline path has no filesystem):

```rust
        let enabled = !cfg!(target_arch = "wasm32")
            && match std::env::var("WORLDGEN_CACHE").ok().as_deref() {
                Some("0") | Some("off") | Some("false") => false,
                Some(_) => true,
                None => DEFAULT_ENABLED,
            };
```

3. `cache_key`: the source dir is now this crate's own — replace

```rust
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../worldgen/src");
```

with

```rust
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
```

4. The cache dir path stays as-is (`crates/worldgen` and `crates/server` sit at the same depth, so `../../target/worldgen-cache` still resolves to the workspace `target/`).

5. Add the convenience wrapper both callers share, after `store`:

```rust
    /// Load-or-generate-and-store. A hit is bit-identical to regeneration
    /// (cached bytes are the exact bincode of the generated columns).
    pub fn generate_cached(
        &self,
        rc: RegionCoords,
        seed: u64,
        materials: crate::material::Materials,
    ) -> Vec<(ColumnCoords, Column)> {
        if let Some(columns) = self.load(rc) {
            return columns;
        }
        let columns = crate::generate_region(rc, seed, materials);
        self.store(rc, &columns);
        columns
    }
```

6. The `#[cfg(test)] mod tests` block moves with the file unchanged (both tests use `at_dir`, no wasm concern).

In `crates/worldgen/src/lib.rs`, add `pub mod cache;` alongside the existing modules.

In `crates/worldgen/Cargo.toml`, move `bincode = { workspace = true }` and `crc32fast = { workspace = true }` from `[dev-dependencies]` into `[dependencies]` and add `log = { workspace = true }` to `[dependencies]`.

- [ ] **Step 2: Verify the moved tests pass**

Run: `cargo test -p worldgen cache`
Expected: PASS — `store_then_load_roundtrips_bit_exact` and `corrupt_file_is_a_miss_and_gets_removed`.

- [ ] **Step 3: Re-wire the server**

In `crates/server/src/lib.rs`:
- Line 21: delete `pub mod worldgen_cache;`.
- Lines 261–274: replace

```rust
    let gen_cache = worldgen_cache::WorldgenCache::from_env(world_seed, &manifest);
```

with

```rust
    let gen_cache = worldgen::cache::WorldgenCache::from_env(world_seed, &manifest);
```

and the generation closure

```rust
        Box::new(move |rc| {
            if let Some(columns) = gen_cache.load(rc) {
                return columns;
            }
            let columns = worldgen::generate_region(rc, world_seed, materials);
            gen_cache.store(rc, &columns);
            columns
        }),
```

with

```rust
        Box::new(move |rc| gen_cache.generate_cached(rc, world_seed, materials)),
```

Run: `cargo build -p server`
Expected: compiles.

- [ ] **Step 4: Wire `LocalServer`**

In `crates/client/src/blocks.rs:13`, change `fn manifest_src() -> String` to `pub fn manifest_src() -> String` (keep its doc comment; add one if absent: `/// The manifest text used to build the registry — also the worldgen cache key input.`).

In `crates/client/src/local_server.rs`, in `LocalServer::new` after the `materials` binding (line ~46), add:

```rust
        let gen_cache = worldgen::cache::WorldgenCache::from_env(
            worldgen::WORLD_SEED,
            &crate::blocks::manifest_src(),
        );
```

and replace the generation closure (line 56):

```rust
            Box::new(move |rc| worldgen::generate_region(rc, worldgen::WORLD_SEED, materials)),
```

with:

```rust
            Box::new(move |rc| gen_cache.generate_cached(rc, worldgen::WORLD_SEED, materials)),
```

Run: `cargo build -p client`
Expected: compiles. (The closure must be `Send`; `WorldgenCache` is a `PathBuf` wrapper, which is `Send` — no changes needed.)

- [ ] **Step 5: Check the wasm target still builds**

Run: `cargo check -p client --target wasm32-unknown-unknown`
Expected: compiles — the cache is compiled in but `from_env` hard-disables itself on wasm, so no fs call is ever reached.

- [ ] **Step 6: Prove the cache is hit by the offline path**

Run: `just worldgen-cache-clear && cargo test -p client --lib -- --include-ignored --exact local_server::tests::offline_handshake_loads_region --nocapture 2>&1 | grep "worldgen cache"`
Expected: `stored r*.bin` lines on the first run. Re-run the same command **without** the cache clear:
Expected: `hit r*.bin` lines, and a visibly faster run.

- [ ] **Step 7: Update the justfile**

The moved cache tests now live under `-p worldgen`, which no recipe runs — that would be silent coverage loss. In the `justfile`:

- Add after the `test-server` recipe:

```make
# Worldgen suite: pure-function generation + the dev disk cache.
test-worldgen:
    cargo test -p worldgen
```

- Change `test: test-game test-client test-server` to `test: test-game test-client test-server test-worldgen`.
- In `test-full`, add as the last line:

```make
    cargo test -p worldgen -- --include-ignored --test-threads={{test_threads}}
```

- In the comment block above `worldgen-cache-clear` (justfile line ~55), if it references `crates/server/src/worldgen_cache.rs`, update the path to `crates/worldgen/src/cache.rs`.

Run: `just test-worldgen`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/worldgen crates/server/src/lib.rs crates/client/src/blocks.rs crates/client/src/local_server.rs justfile
git rm --cached crates/server/src/worldgen_cache.rs 2>/dev/null; true
git commit -m "perf(worldgen): move dev disk cache into worldgen; use it from LocalServer

The convergence-rig tests regenerate the same pristine 3x3 window dozens
of times through LocalServer, which bypassed the server-only cache.
Cache hits are bit-identical by construction (exact bincode of the
generated columns). Disabled on wasm32 (no filesystem).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Full-suite verification and wall-clock comparison

**Files:** none modified — measurement and gate only.

**Interfaces:**
- Consumes: everything above.
- Produces: the before/after numbers for the final report.

- [ ] **Step 1: Warm the worldgen cache deliberately**

Run: `just worldgen-cache-clear && cargo test -p client --lib -- --include-ignored --exact local_server::tests::offline_window_loads_nine_regions`
Expected: PASS (this generates and stores the 3×3 window the rigs share).

- [ ] **Step 2: Run the complete suite, timed**

Run: `time just test-full`
Expected: everything PASSES. Baseline (2026-07-11, before these changes): 3225s CPU single-threaded; ~13.5 min wall at `--test-threads=4`. Target: roughly half. Record the `real` time.

- [ ] **Step 3: Spot-check the win attribution**

Run: `BIN=$(ls -t target/debug/deps/reconcile_keepup-* | grep -v '\.d$' | head -1); time "$BIN" --include-ignored --exact diag_a_lockstep_single_input`
Expected: PASS, and materially below the 186s baseline for this test (the profile said ~44% of it was mass-props + genesis digests, plus ~13% worldgen now cached — expect roughly 80–100s).

- [ ] **Step 4: Report**

No commit. Report the before/after wall-clock numbers for `just test-full` and the spot-check test, and confirm every suite passed. If the win is materially below expectations, re-profile (`perf record` + `inferno-collapse-perf` on the spot-check test, per the scratchpad scripts) before claiming completion.

---

## Self-review notes

- Spec coverage: Change 1 → Task 2; Change 2 → Task 1; Change 3 → Task 3; spec's verification section → Task 4 plus the per-task test steps. The spec's "store→load round-trip equals fresh generation" requirement is satisfied by the moved `store_then_load_roundtrips_bit_exact` test (Task 3 Step 2).
- The exact line numbers cited (e.g. `state.rs:499`, `user_changes.rs:174`) are from the 2026-07-11 tree; if drift has occurred, locate by the quoted code, which is verbatim.
- Task order is deliberate: Tasks 1–3 are independent, but running the slow suites gets cheaper after each, so the biggest-risk fork change goes first while `test-full` feedback is still slow but the diff is smallest.
