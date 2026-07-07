# Region Collider Genesis Performance — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans
> to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make bringing a real-terrain region online fit the 20s
`REGION_STARTUP_BUDGET` with comfortable margin, deterministically, by removing
the O(N²) collider-build cost.

**Architecture:** Add an unlogged **genesis** collider-build path
(`Rollback::seed_terrain_genesis`) that inserts terrain bodies/colliders via the
macro's raw accessors (no per-chunk `PhysicsState` clone), and collapse
fully-solid chunks to a single cuboid (collision-identical to a solid `Voxels`
shape) while partial chunks keep the parry `Voxels` shape. `from_chunks` routes
through it.

**Tech Stack:** Rust, vendored rapier3d/parry3d forks, `#[rollback]` macro,
crc32fast hashing.

## Global Constraints

- Determinism / rollback bar is bit-exact: `hash(before) == hash(after undo)`.
  All colliders built from the vendored determinism-locked forks; genesis adds
  no per-tick mutation. Verified: independent builds hash-identical.
- `crates/game` and `crates/server` stay Bevy-free and windowing-free.
- Worldgen output is unchanged — this is a physics/region-layer change only.
- `REGION_STARTUP_BUDGET = Duration::from_secs(20)` (threaded_world.rs:21).

---

### Task 1: Genesis collider path (IMPLEMENTED + VERIFIED in spike)

**Files:**
- Modify: `crates/game/src/state.rs` — add `Rollback::seed_terrain_genesis`.
- Modify: `crates/game/src/region.rs:310` — `from_chunks` calls it.

**Interfaces:**
- Produces: `Rollback::seed_terrain_genesis(&mut self, chunks: Vec<(ChunkCoords, Chunk)>)`.

Status: done in the worktree. `seed_terrain_genesis` mirrors `create_mesh` but
(a) inserts body+collider through `self.data.physics.raw_mut().{bodies,colliders}.raw_mut()`
— zero snapshot clones, zero undo-log entries — and (b) builds a
`ColliderBuilder::cuboid(16,16,16)` translated to (16,16,16) when
`solid.len() == ChunkShape::SIZE`, else `ColliderBuilder::voxels(...)`.
`create_mesh` / `attach_voxels_collider_safe` are untouched (runtime edits).

Measured (release, region (0,0)): `from_columns` build **1.06s** (was ~9–15s),
independent builds hash-identical (`0xa339903c`), `cargo test -p game` green.

- [x] Implement `seed_terrain_genesis`; route `from_chunks` to it.
- [x] `cargo build -p game` clean.
- [x] Manual verify: 1.06s build, deterministic hash, all `game` tests pass.

---

### Task 2: Unit test — genesis determinism + full-chunk cuboid

**Files:**
- Create: `crates/game/tests/collider_genesis.rs`
- Test: same file.

**Interfaces:**
- Consumes: `Region::from_chunks`, `Chunk::flat_floor`, `Chunk` (all `game` public).

- [ ] **Step 1: Write the failing test.** A fully-solid chunk collapses to one
  cuboid collider; a partial chunk stays a voxels shape; and building the same
  chunk set twice yields an identical `crc32` state hash.

```rust
use game::{Chunk, ChunkCoords, Region, RegionId};
use rapier3d::prelude::ShapeType;
use std::hash::{Hash, Hasher};

fn hash_region(r: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    r.data().data.hash(&mut h);
    h.finalize()
}

// A 32^3 chunk with every block solid (Chunk::flat_floor(32) fills all 32 layers).
fn full_chunk() -> Chunk {
    Chunk::flat_floor(32)
}

// A partial chunk: only the bottom 8 layers solid.
fn partial_chunk() -> Chunk {
    Chunk::flat_floor(8)
}

#[test]
fn genesis_collapses_full_chunk_to_cuboid_and_keeps_partial_as_voxels() {
    let id = RegionId::new(0, 0);
    let region = Region::from_chunks(
        id,
        vec![
            (ChunkCoords::new(0, 0, 0), full_chunk()),
            (ChunkCoords::new(1, 0, 0), partial_chunk()),
        ],
    );
    let shapes: Vec<ShapeType> = region
        .data()
        .data
        .physics
        .colliders
        .iter()
        .map(|(_, c)| c.shape().shape_type())
        .collect();
    assert!(shapes.contains(&ShapeType::Cuboid), "full chunk -> cuboid");
    assert!(shapes.contains(&ShapeType::Voxels), "partial chunk -> voxels");
}

#[test]
fn genesis_is_deterministic() {
    let id = RegionId::new(0, 0);
    let build = || {
        Region::from_chunks(
            id,
            vec![
                (ChunkCoords::new(0, 0, 0), full_chunk()),
                (ChunkCoords::new(1, 0, 0), partial_chunk()),
            ],
        )
    };
    assert_eq!(hash_region(&build()), hash_region(&build()));
}
```

- [ ] **Step 2: Run to verify it fails/compiles.** Adjust access paths to match
  the real API — `region.data().data.physics.colliders` field visibility, and
  the exact `Chunk::flat_floor` fill semantics (confirm `flat_floor(32)` fills
  all 32768 cells; if not, construct a full chunk explicitly). `RegionId::new`
  exists (`RegionId = RegionCoords`).
  Run: `cargo test -p game --test collider_genesis -v`
  Expected: compiles; assertions meaningful.

- [ ] **Step 3: Make it pass** (implementation already in place from Task 1).
  Run: `cargo test -p game --test collider_genesis`
  Expected: PASS.

- [ ] **Step 4: Commit.**

```bash
git add crates/game/tests/collider_genesis.rs crates/game/src/state.rs crates/game/src/region.rs
git commit -m "perf(game): unlogged genesis collider build + full-chunk cuboid collapse"
```

---

### Task 3: Re-enable the two ignored threaded_world tests

**Files:**
- Modify: `crates/server/tests/threaded_world.rs:135-140,227-228` — delete the
  two `#[ignore = "..."]` attributes and the stale explanatory comment block.

- [ ] **Step 1:** Remove the `#[ignore]` on
  `roaming_client_cycles_regions_across_threads` (line ~139) and
  `walking_input_crosses_a_boundary_on_real_threads` (line ~227), plus the
  now-obsolete "IGNORED: density-pipeline terrain..." comment above the first.
  Leave the tests' bodies and `REGION_STARTUP_BUDGET` unchanged.

- [ ] **Step 2: Run them.**
  Run: `cargo test -p server --test threaded_world roaming_client_cycles_regions_across_threads walking_input_crosses_a_boundary_on_real_threads -- --nocapture`
  Expected: both PASS, well within the 20s budget (region build is ~1s).

- [ ] **Step 3: Commit.**

```bash
git add crates/server/tests/threaded_world.rs
git commit -m "test(server): re-enable region-cycling tests now that collider build is fast"
```

---

### Task 4: Profiling harness + justfile recipe

**Files:**
- Keep: `crates/worldgen/examples/collider_profile.rs` (already recreated in the
  worktree; measures `from_columns` build time + determinism hash). Remove the
  unused `Hasher` import warning.
- Modify: `justfile` — add a `collider-bench` recipe.

- [ ] **Step 1:** Fix the unused-import warning in the example
  (`use std::hash::{Hash, Hasher};` → drop `Hasher`; only `Hash` is used via the
  trait method on `.data`). Confirm `cargo run --release -p worldgen --example
  collider_profile` still prints build time + `determinism: OK`.

- [ ] **Step 2:** Add to `justfile` after the `worldgen-bench` recipe:

```make
# Time one region's collider genesis build + determinism check.
collider-bench:
    {{cargo}} run --release -p worldgen --example collider_profile
```

- [ ] **Step 3: Commit.**

```bash
git add crates/worldgen/examples/collider_profile.rs justfile
git commit -m "build(worldgen): collider-genesis profiling example + just recipe"
```

---

### Task 5: Full verification

- [ ] `cargo build --workspace --bins` clean.
- [ ] `cargo test -p game` green (rollback/determinism).
- [ ] `cargo test -p client` green (bridge/meshing unaffected).
- [ ] `cargo test -p server --test threaded_world` green including the two
  re-enabled tests.
- [ ] `just clippy` (or `cargo clippy -p game -p server -p worldgen`) clean for
  touched crates.
- [ ] Run the `verify` skill: drive a live server + client on generated terrain,
  confirm a player spawns, stands on terrain, and walks without falling through
  (surface `Voxels` fidelity) and without catching (cuboid interiors are buried).

## Acceptance (from spec)

1. One-region genesis well under budget — measured ~1.06s (19× margin). ✓ (Task 1)
2. Two `threaded_world` tests un-ignored and passing. (Task 3)
3. `cargo test -p game` stays green — no determinism regression. ✓ (Task 1/2)
4. Profiling harness retained + `just` recipe. (Task 4)
