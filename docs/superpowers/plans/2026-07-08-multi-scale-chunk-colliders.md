# Multi-Scale Chunk Colliders Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build chunk physics colliders at block scale (voxel_size 16) for whole solid blocks and at voxel scale (voxel_size 1) only for chiseled sub-block detail, instead of flattening every chunk to a 32³ voxel grid.

**Architecture:** A chunk body gets up to two `Voxels` colliders on the same fixed rigid body: one with `voxel_size = 16.0` (the block grid, `BLOCK_VOXELS`) keyed by solid unchiseled blocks, and one with `voxel_size = 1.0` (the voxel grid) keyed by solid chisel voxels. A new pure function `collider_cells` in `crates/game/src/block.rs` is the collider-facing counterpart of `derive_voxels` — same solidity rules, but it keeps cells at the coarsest exact scale instead of expanding to 32³. Both collider build sites (`seed_terrain_genesis` genesis bulk path and `create_mesh` runtime path) switch to it. The fully-solid-chunk → cuboid collapse stays.

**Tech Stack:** Rust, vendored rapier/parry forks (`ColliderBuilder::voxels(voxel_size, keys)` — `voxel_size` is per-shape), `cargo test -p game` via `just test`.

## Background (why this is safe)

- 1 block = 16³ voxels (`BLOCK_VOXELS = 16`), 1 chunk = 2×2×2 blocks (`CHUNK_BLOCK_COUNT = 8`) = 32³ voxels. Today a surface chunk's collider enumerates up to 32,768 voxel cells; at block scale a fully-whole-block chunk is at most 8 cells.
- parry's `Voxels` shape takes one uniform `voxel_size` per shape; different scales = multiple colliders on one body. Both carry the entity's `user_data`, so collider→entity resolution is unaffected.
- **Seams:** the two shapes classify their boundary faces independently (the internal-edges problem), so phantom-edge catching at block/chisel boundaries is possible. **Deliberately deferred by decision — do not add seam handling in this plan.**
- **Known accepted behavior change:** a chunk that is fully solid purely via fully-occupied chisel data no longer collapses to a cuboid (the old check was on the derived 32³ grid; the new check is "all 8 blocks whole and solid"). Collision volume is identical either way.

## Global Constraints

- Determinism is sacred: no new float values other than exact binary ones (1.0, 16.0); iterate in `BlockIndex`/fixed nested-loop order; keys are `Point<i32>` (no `usize` on any serialized path — wasm32 compat).
- Rollback bar: `hash(before) == hash(after undo)`, bit-exact. Never disable hash verification.
- `crates/game` stays Bevy-free and windowing-free.
- Tag any new test measured >100ms with `#[ignore = "slow: >100ms — run via just test-full"]`.
- Use `just test` while iterating; `just test-full` before declaring done. No build/run command changes → no justfile edits needed.
- Commit messages end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

### Task 1: `collider_cells` — pure decomposition in the block layer

**Files:**
- Modify: `crates/game/src/block.rs` (add `ColliderCells`, `collider_cells`, tests in the existing `mod tests`)

**Interfaces:**
- Consumes: existing `Block`, `BlockIndex`, `ChiselData`, `BLOCK_VOXELS`, `CHUNK_BLOCK_COUNT`, `derive_voxels` (test oracle).
- Produces (used by Tasks 2–3):
  - `pub struct ColliderCells { pub blocks: Vec<Point<i32>>, pub voxels: Vec<Point<i32>> }`
  - `impl ColliderCells { pub fn is_empty(&self) -> bool; pub fn is_full_cube(&self) -> bool }`
  - `pub fn collider_cells(blocks: &[Block], chisel: &BTreeMap<BlockIndex, ChiselData>) -> ColliderCells`
  - Re-exported at crate root automatically via `lib.rs`'s `pub use block::*;`.

- [ ] **Step 1: Write the failing tests**

Append inside the existing `mod tests` in `crates/game/src/block.rs`:

```rust
    use crate::voxel::Chunk;
    use std::collections::BTreeSet;

    /// Voxel-grid coords of solid cells in a derived 32³ grid — the oracle.
    fn solid_set(voxels: &[Voxel]) -> BTreeSet<(i32, i32, i32)> {
        (0..CHUNK_VOXEL_COUNT)
            .filter(|i| voxels[*i].block != BlockId::AIR)
            .map(|i| {
                let [x, y, z] = ChunkShape::delinearize(i as u32);
                (x as i32, y as i32, z as i32)
            })
            .collect()
    }

    /// Voxel-grid coords covered by the two-scale cells: each block cell
    /// expands to its 16³ voxels, chisel cells are already voxel-grid.
    fn covered_set(cells: &ColliderCells) -> BTreeSet<(i32, i32, i32)> {
        let mut s = BTreeSet::new();
        for b in &cells.blocks {
            for vx in 0..BLOCK_VOXELS as i32 {
                for vy in 0..BLOCK_VOXELS as i32 {
                    for vz in 0..BLOCK_VOXELS as i32 {
                        s.insert((
                            b.x * BLOCK_VOXELS as i32 + vx,
                            b.y * BLOCK_VOXELS as i32 + vy,
                            b.z * BLOCK_VOXELS as i32 + vz,
                        ));
                    }
                }
            }
        }
        for v in &cells.voxels {
            let dup = !s.insert((v.x, v.y, v.z));
            assert!(!dup, "voxel-scale cell {v:?} overlaps a block-scale cell");
        }
        s
    }

    #[test]
    fn collider_cells_uniform_chunk_is_eight_blocks() {
        let c = Chunk::uniform(Block::new(BlockId(1)));
        let cells = collider_cells(&c.blocks, &c.chisel);
        assert_eq!(cells.blocks.len(), CHUNK_BLOCK_COUNT);
        assert!(cells.voxels.is_empty());
        assert!(cells.is_full_cube());
        assert!(!cells.is_empty());
    }

    #[test]
    fn collider_cells_all_air_is_empty() {
        let c = Chunk::default();
        let cells = collider_cells(&c.blocks, &c.chisel);
        assert!(cells.is_empty());
        assert!(!cells.is_full_cube());
    }

    #[test]
    fn collider_cells_mixed_floor_splits_scales() {
        // flat_floor(24): the by=0 block layer (4 blocks) is whole solid,
        // the by=1 layer is chiseled 8 voxel-layers deep (16*8*16 per block).
        let c = Chunk::flat_floor(24);
        let cells = collider_cells(&c.blocks, &c.chisel);
        assert_eq!(cells.blocks.len(), 4);
        assert_eq!(cells.voxels.len(), 4 * 16 * 8 * 16);
        assert!(!cells.is_full_cube());
    }

    #[test]
    fn collider_cells_chisel_only_floor_has_no_block_cells() {
        // flat_floor(8) is shallower than one block: chisel cells only.
        let c = Chunk::flat_floor(8);
        let cells = collider_cells(&c.blocks, &c.chisel);
        assert!(cells.blocks.is_empty());
        assert_eq!(cells.voxels.len(), 4 * 16 * 8 * 16);
    }

    #[test]
    fn collider_cells_skip_air_material_chisel_voxels() {
        // Occupied chisel voxel whose palette resolves to AIR (empty palette
        // -> material_at falls back to AIR) must not produce a collider cell,
        // matching derive_voxels solidity.
        let mut blocks = vec![Block::new(BlockId::AIR); CHUNK_BLOCK_COUNT];
        blocks[0] = Block::new(BlockId(1));
        let mut c = ChiselData::new(vec![]);
        c.set(0, 0, 0, true, 0);
        let mut chisel = BTreeMap::new();
        chisel.insert(BlockIndex(0), c);
        let cells = collider_cells(&blocks, &chisel);
        assert!(cells.is_empty());
    }

    #[test]
    fn collider_cells_cover_exactly_the_derived_solid_voxels() {
        let mut single_chisel = Chunk::default();
        single_chisel.blocks[0] = Block::new(BlockId(1));
        let mut cd = ChiselData::new(vec![BlockId(1)]);
        cd.set(3, 7, 11, true, 0);
        single_chisel.chisel.insert(BlockIndex(0), cd);

        let cases = [
            Chunk::uniform(Block::new(BlockId(1))),
            Chunk::default(),
            Chunk::flat_floor(8),
            Chunk::flat_floor(16),
            Chunk::flat_floor(24),
            Chunk::flat_floor(32),
            single_chisel,
        ];
        for (i, chunk) in cases.iter().enumerate() {
            let cells = collider_cells(&chunk.blocks, &chunk.chisel);
            let derived = derive_voxels(&chunk.blocks, &chunk.chisel);
            assert_eq!(
                covered_set(&cells),
                solid_set(&derived),
                "case {i}: two-scale cells must cover exactly the solid voxels"
            );
        }
    }

    #[test]
    fn collider_cells_is_deterministic() {
        let c = Chunk::flat_floor(24);
        let a = collider_cells(&c.blocks, &c.chisel);
        let b = collider_cells(&c.blocks, &c.chisel);
        assert_eq!(a.blocks, b.blocks);
        assert_eq!(a.voxels, b.voxels);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p game collider_cells`
Expected: compile error — `cannot find function collider_cells` / `cannot find type ColliderCells`.

- [ ] **Step 3: Write the implementation**

In `crates/game/src/block.rs`, add below the imports:

```rust
use parry3d::math::Point;
```

and add above `derive_voxels`:

```rust
/// Chunk collider cells split by grid scale: whole (unchiseled) solid blocks
/// on the block grid, chisel detail on the voxel grid. Together they cover
/// exactly the solid cells of [`derive_voxels`] — this is that function's
/// collider-facing counterpart, kept at the coarsest exact scale instead of
/// expanded to the 32³ grid.
#[derive(Debug, Clone, PartialEq)]
pub struct ColliderCells {
    /// Block-grid keys (0..CHUNK_BLOCKS per axis) of solid, unchiseled blocks.
    pub blocks: Vec<Point<i32>>,
    /// Voxel-grid keys (0..CHUNK_BLOCKS*BLOCK_VOXELS per axis) of solid
    /// chisel voxels (occupied with a non-air material).
    pub voxels: Vec<Point<i32>>,
}

impl ColliderCells {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty() && self.voxels.is_empty()
    }

    /// True iff the chunk is entirely whole solid blocks (no chisel), i.e.
    /// its collision volume is exactly the full 32³ chunk cube. (All eight
    /// blocks present implies no chiseled block, hence no voxel cells.)
    pub fn is_full_cube(&self) -> bool {
        self.blocks.len() == CHUNK_BLOCK_COUNT
    }
}

/// Decompose a chunk's block layer into collider cells. Solidity matches
/// [`derive_voxels`]: an unchiseled block is solid iff its id is non-air; a
/// chisel voxel is solid iff occupied and its palette material is non-air.
/// Deterministic: blocks in `BlockIndex` order, chisel voxels in fixed
/// x-outer/z-inner nested order per block.
pub fn collider_cells(
    blocks: &[Block],
    chisel: &BTreeMap<BlockIndex, ChiselData>,
) -> ColliderCells {
    debug_assert_eq!(blocks.len(), CHUNK_BLOCK_COUNT);
    let mut out = ColliderCells { blocks: Vec::new(), voxels: Vec::new() };
    for (idx, block) in blocks.iter().enumerate() {
        let bi = BlockIndex(idx as u8);
        let (bx, by, bz) = bi.xyz();
        match chisel.get(&bi) {
            None => {
                if block.id != BlockId::AIR {
                    out.blocks.push(Point::new(bx as i32, by as i32, bz as i32));
                }
            }
            Some(c) => {
                for vx in 0..BLOCK_VOXELS {
                    for vy in 0..BLOCK_VOXELS {
                        for vz in 0..BLOCK_VOXELS {
                            if c.is_solid(vx, vy, vz)
                                && c.material_at(vx, vy, vz) != BlockId::AIR
                            {
                                out.voxels.push(Point::new(
                                    (bx * BLOCK_VOXELS + vx) as i32,
                                    (by * BLOCK_VOXELS + vy) as i32,
                                    (bz * BLOCK_VOXELS + vz) as i32,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p game collider_cells`
Expected: all 7 new tests PASS.

Then run: `just test`
Expected: PASS (no other suite touched yet).

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/block.rs
git commit -m "feat(game): collider_cells — two-scale (block/chisel) chunk collider decomposition

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Runtime path — scale-aware `attach_voxels_collider_safe` + `create_mesh`

**Files:**
- Modify: `crates/game/src/state.rs:291-326` (`create_mesh`), `crates/game/src/state.rs:385-400` (`attach_voxels_collider_safe`)

**Interfaces:**
- Consumes: `crate::collider_cells`, `crate::ColliderCells`, `crate::BLOCK_VOXELS` (Task 1, via the crate-root glob re-export).
- Produces: `pub fn attach_voxels_collider_safe(&mut self, e: EntityKey, body: RigidBodyHandle, coords: &[Point<i32>], voxel_size: Real)` — Task 3 does NOT use this (genesis inserts raw), but any future entity-voxel caller does. `create_mesh` signature unchanged.

- [ ] **Step 1: Add the scale parameter to `attach_voxels_collider_safe`**

Replace the function (currently at `crates/game/src/state.rs:385-400`) with:

```rust
    /// Attach a voxel-grid collider to an entity's body. One cell per key;
    /// for voxel size `s`, cell g spans [g*s, (g+1)*s) in body-local space —
    /// scale 1.0 is the render mesh's voxel grid, scale `BLOCK_VOXELS` is
    /// the block grid. Entity-generic. Undo restores the whole PhysicsState
    /// snapshot — same rationale as attach_capsule_collider_safe.
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
        let p = self.data.physics.snapshot_raw();
        p.colliders.insert_with_parent(collider, body, p.bodies);
    }
```

- [ ] **Step 2: Switch `create_mesh` to the two-scale decomposition**

In `create_mesh` (`crates/game/src/state.rs:291-326`), replace the `derive_voxels` + `solid` collection (lines 293-301):

```rust
        let voxels = crate::derive_voxels(&chunk.blocks, &chunk.chisel);
        // Deterministic linearize order; grid coords are body-local.
        let solid: Vec<Point<i32>> = (0..ChunkShape::SIZE)
            .filter(|i| voxels[*i as usize].block != BlockId::AIR)
            .map(|i| {
                let [x, y, z] = ChunkShape::delinearize(i);
                Point::new(x as i32, y as i32, z as i32)
            })
            .collect();
```

with:

```rust
        // Two-scale decomposition: whole solid blocks at block scale, chisel
        // detail at voxel scale. Covers exactly derive_voxels' solid cells.
        let cells = crate::collider_cells(&chunk.blocks, &chunk.chisel);
```

and replace the attach at the end (lines 322-324):

```rust
        if !solid.is_empty() {
            self.attach_voxels_collider_safe(e, handle, &solid);
        }
```

with:

```rust
        if !cells.blocks.is_empty() {
            self.attach_voxels_collider_safe(
                e,
                handle,
                &cells.blocks,
                Real::from(crate::BLOCK_VOXELS as RawReal),
            );
        }
        if !cells.voxels.is_empty() {
            self.attach_voxels_collider_safe(e, handle, &cells.voxels, Real::from(1.0));
        }
```

Note: two attaches mean two whole-PhysicsState snapshots per runtime chunk creation. That is correct (LIFO undo restores in reverse order) and `create_mesh` is a rare runtime op; do not fuse them.

If the compiler now warns that `ChunkShape`, `Voxel`, `ConstShape`, or `BlockId` imports are unused in `state.rs`, remove exactly the unused ones (they may still be used elsewhere in the file — trust the compiler, not grep).

- [ ] **Step 3: Run the rollback and game suites**

Run: `cargo test -p game`
Expected: PASS — in particular `rollback_restore.rs::rollback_of_create_mesh_restores_state`, `log_model.rs`, and `random_ops.rs` all exercise `create_mesh` with `flat_floor(8)` (chisel-only → single voxel-scale collider) and enforce the bit-exact undo-hash invariant over the new two-collider path.

- [ ] **Step 4: Commit**

```bash
git add crates/game/src/state.rs
git commit -m "feat(game): create_mesh builds block-scale + chisel-scale colliders

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Genesis path — `seed_terrain_genesis` + genesis tests

**Files:**
- Modify: `crates/game/src/state.rs:328-383` (`seed_terrain_genesis`)
- Test: `crates/game/tests/collider_genesis.rs`

**Interfaces:**
- Consumes: `crate::collider_cells`, `ColliderCells::{is_empty, is_full_cube}`, `crate::BLOCK_VOXELS` (Task 1).
- Produces: no new API; genesis emits per chunk either 0 colliders (air), 1 cuboid (full cube), or 1–2 `Voxels` colliders (block-scale and/or voxel-scale) on the chunk's fixed body.

- [ ] **Step 1: Write the failing test**

Add to `crates/game/tests/collider_genesis.rs`:

```rust
use game::parry::math::Real;

#[test]
fn mixed_chunk_gets_block_and_voxel_scale_colliders() {
    // flat_floor(24): whole solid by=0 block layer + chiseled by=1 layer.
    let region = Region::from_chunks(
        RegionId::new(0, 0),
        vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor(24))],
    );
    let mut sizes: Vec<Real> = region
        .data()
        .physics
        .colliders
        .iter()
        .filter_map(|(_, c)| c.shape().as_voxels().map(|v| v.voxel_size().x))
        .collect();
    sizes.sort();
    assert_eq!(
        sizes,
        vec![Real::from(1.0), Real::from(16.0)],
        "one chisel-scale and one block-scale voxels collider"
    );
}
```

Adjust the colliders access path to match the existing `full_chunk_becomes_cuboid_partial_stays_voxels` test in the same file verbatim — copy its accessor chain.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p game --test collider_genesis`
Expected: `mixed_chunk_gets_block_and_voxel_scale_colliders` FAILS — current code builds a single voxel-scale collider, so `sizes == [1.0]`.

- [ ] **Step 3: Rewrite the genesis collider build**

In `seed_terrain_genesis` (`crates/game/src/state.rs`), replace the `derive_voxels` + `solid` collection (lines 339-346) with:

```rust
            let cells = crate::collider_cells(&chunk.blocks, &chunk.chisel);
```

and replace everything from `if solid.is_empty()` through the collider insert (lines 363-381) with:

```rust
            if cells.is_empty() {
                continue;
            }
            let user_data = e.data().as_ffi() as u128;
            let mut colliders = Vec::with_capacity(2);
            if cells.is_full_cube() {
                // A fully-solid 32^3 chunk's collision surface is exactly a
                // 32x32x32 cuboid (half-extents 16, centered body-local 16,16,16).
                colliders.push(
                    ColliderBuilder::cuboid(Real::from(16.0), Real::from(16.0), Real::from(16.0))
                        .translation(Vector::new(
                            Real::from(16.0),
                            Real::from(16.0),
                            Real::from(16.0),
                        ))
                        .user_data(user_data)
                        .build(),
                );
            } else {
                // Two-scale decomposition: whole solid blocks at block scale
                // (voxel_size = BLOCK_VOXELS), chisel detail at voxel scale.
                if !cells.blocks.is_empty() {
                    colliders.push(
                        ColliderBuilder::voxels(
                            Vector::repeat(Real::from(crate::BLOCK_VOXELS as RawReal)),
                            &cells.blocks,
                        )
                        .user_data(user_data)
                        .build(),
                    );
                }
                if !cells.voxels.is_empty() {
                    colliders.push(
                        ColliderBuilder::voxels(Vector::repeat(Real::from(1.0)), &cells.voxels)
                            .user_data(user_data)
                            .build(),
                    );
                }
            }
            let phys = self.data.physics.raw_mut();
            for collider in colliders {
                phys.colliders
                    .raw_mut()
                    .insert_with_parent(collider, handle, phys.bodies.raw_mut());
            }
```

Update the function's doc comment (lines 328-335): replace the last sentence ("Fully-solid chunks get a single cuboid … partial (surface) chunks keep the parry `Voxels` shape.") with:

```rust
    /// state and is never rolled back below itself, so raw (unlogged) inserts
    /// are correct and turn the build from O(N^2) (a growing clone per chunk)
    /// into O(N). Fully-solid chunks get a single cuboid; partial chunks get
    /// up to two parry `Voxels` shapes — whole solid blocks at block scale
    /// (voxel_size = BLOCK_VOXELS) and chisel detail at voxel scale (1.0).
    /// A chunk solid purely via fully-occupied chisel no longer collapses to
    /// a cuboid (collision-identical, just uncollapsed).
```

Update the header comment of `crates/game/tests/collider_genesis.rs` (lines 1-3) to:

```rust
//! Genesis collider build (Region::from_chunks -> seed_terrain_genesis):
//! fully-solid chunks collapse to a single cuboid; partial chunks get up to
//! two parry Voxels shapes (block-scale for whole solid blocks, voxel-scale
//! for chisel detail); the build is deterministic (bit-exact hash).
```

- [ ] **Step 4: Run the genesis tests to verify they pass**

Run: `cargo test -p game --test collider_genesis`
Expected: PASS — all 3 tests, including the pre-existing two: `flat_floor(8)` is chisel-only so the partial chunk still yields exactly one `Voxels` collider (`shapes.len() == 2` holds), and `genesis_is_deterministic` must still hash bit-exactly.

Then run: `just test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/state.rs crates/game/tests/collider_genesis.rs
git commit -m "feat(game): genesis builds block-scale + chisel-scale chunk colliders

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Stale comments, full suite, perf sanity

**Files:**
- Modify: `crates/server/tests/threaded_world.rs:15-17` (stale "one per solid voxel" comment)
- Verify: `crates/worldgen/examples/collider_profile.rs` (run, don't edit unless it asserts collider counts)

**Interfaces:**
- Consumes: everything above. Produces: nothing new — this task is verification and doc hygiene.

- [ ] **Step 1: Fix the stale comment**

In `crates/server/tests/threaded_world.rs`, replace lines 15-17:

```rust
/// A fresh region's first build (worldgen chunks -> physics colliders, one
/// per solid voxel — see `Region::from_chunks`/`attach_voxels_collider_safe`)
/// costs low single-digit seconds even for a flat floor; 9 of them in
```

with:

```rust
/// A fresh region's first build (worldgen chunks -> physics colliders,
/// block-scale + chisel-scale voxels shapes — see `Region::from_chunks`/
/// `seed_terrain_genesis`) costs well under a second for a flat floor; 9 in
```

Keep the generous `REGION_STARTUP_BUDGET` as-is (it is a ceiling, not an expectation).

- [ ] **Step 2: Run the full suite**

Run: `just test-full`
Expected: PASS across `game`, `client`, `server` (client convergence harnesses and server threaded-world tests exercise the changed genesis path end to end). If any test asserts collider counts or shapes beyond `collider_genesis.rs`, fix the assertion to the new expectation (0/1 cuboid/1-2 voxels per chunk) — do not weaken hash or determinism assertions.

- [ ] **Step 3: Perf sanity — region collider build**

Run: `cargo run --release -p worldgen --example collider_profile`
Expected: region build time at or (likely well) below the current ~1s baseline (see docs in the example). Record the number in the commit message. If it regressed, stop and investigate before committing — the likely culprit is accidental per-cell work in `collider_cells`.

- [ ] **Step 4: Live verification**

Use the project run flow (`just run`, or the `verify` skill) with server + one client: walk on flat terrain and up a chiseled slope, confirm no fall-through and no rubber-banding (reconciliation hash mismatches would log loudly). Seam catching, if observed, is explicitly out of scope — note it, don't fix it.

- [ ] **Step 5: Commit**

```bash
git add crates/server/tests/threaded_world.rs
git commit -m "docs(server): update stale per-voxel collider comment; verify full suite

collider_profile: <measured time> (was ~1s)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```
