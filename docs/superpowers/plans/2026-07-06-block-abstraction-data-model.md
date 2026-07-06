# Block Abstraction Data Model Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a 16³-voxel *block* the authoritative gameplay unit of a chunk, with the 32³ voxel array reframed as a value derived on demand from blocks (+ sparse chisel data).

**Architecture:** A new `game::block` module defines `Block` (compact, Veloren-style), `BlockKind`, `BlockIndex`, `ChiselData` (dense 16³ bitset + material palette, VS-style), and the single shared `derive_voxels(blocks, chisel) -> Vec<Voxel>`. `Chunk` stops storing voxels and stores `blocks` + a sparse `chisel` map instead; the sim derives voxels transiently to build the rapier collider, and the client derives them to feed the existing greedy mesher. No runtime edits, no interaction — blocks are set once at chunk creation, so no new rollback/undo machinery is needed.

**Tech Stack:** Rust (editions 2021/2024 mixed), `serde` + `bincode` (wire), `crc32fast` (state hashing), `block_mesh` (greedy mesher, `ConstShape3u32<32,32,32>`), rapier voxel colliders, Bevy 0.18 (client only).

## Global Constraints

- Rollback correctness bar is bit-exact: `hash(before) == hash(after undo)`. Authoritative chunk state is `blocks` + `chisel` only; the derived voxel array must never be stored, serialized, or hashed.
- Cross-machine determinism: introduce **no** float math in the block layer. All block/chisel data is integer/byte. `derive_voxels` must be deterministic and identical on client and server (single definition in `game`).
- wasm32 wire-compat: no `usize`/`isize` in any serialized block type. Use `u8` (`BlockIndex`), `Vec<u64>`/`Vec<u8>` (`ChiselData`), `Vec<BlockKind>`.
- `game` and `server` stay Bevy-free and windowing-free. The block layer lives in `game`.
- `game`'s crate-root glob re-exports (`pub use voxel::*;` etc. in `lib.rs`) are load-bearing for the `#[rollback]` macro and `borrow::Partial` derives — new modules must be re-exported the same way.
- Voxel scale is fixed: 1 voxel = 1/16 m, 1 block = 16³ voxels = 1 m³, chunk = 32³ voxels = 2×2×2 = 8 blocks. Region math (`coords * 32`, `REGION_SIZE = 256`) is untouched.

---

### Task 1: `game::block` module — types + `derive_voxels`

**Files:**
- Create: `crates/game/src/block.rs`
- Modify: `crates/game/src/lib.rs:24` (add `pub mod block;`) and `:35` area (add `pub use block::*;`)

**Interfaces:**
- Consumes: `crate::voxel::{Voxel, VoxelType, ChunkShape, CHUNK_VOXEL_COUNT}` (existing).
- Produces (relied on by Tasks 2 & 3):
  - `pub struct Block { pub kind: BlockKind, pub data: [u8; 3] }`, `Block::new(BlockKind) -> Block`
  - `pub enum BlockKind { Air, Stone }` (default `Air`), `BlockKind::voxel(self) -> VoxelType`
  - `pub struct BlockIndex(pub u8)`, `BlockIndex::from_xyz(usize,usize,usize) -> BlockIndex`, `BlockIndex::xyz(self) -> (usize,usize,usize)`
  - `pub struct ChiselData` with `ChiselData::new(palette: Vec<BlockKind>) -> ChiselData`, `set(&mut self, vx,vy,vz, solid: bool, material_idx: u8)`, `is_solid(&self, vx,vy,vz) -> bool`, `material_at(&self, vx,vy,vz) -> BlockKind`
  - `pub fn derive_voxels(blocks: &[Block], chisel: &BTreeMap<BlockIndex, ChiselData>) -> Vec<Voxel>`
  - `pub fn voxel_index(x: usize, y: usize, z: usize) -> usize`
  - consts `BLOCK_VOXELS = 16`, `CHUNK_BLOCKS = 2`, `CHUNK_BLOCK_COUNT = 8`, `BLOCK_VOXEL_COUNT = 4096`

- [ ] **Step 1: Write the failing tests**

Append to the new file `crates/game/src/block.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::voxel::VoxelType;
    use std::collections::BTreeMap;

    #[test]
    fn uniform_stone_block_fills_its_subcube() {
        let blocks = vec![Block::new(BlockKind::Stone); CHUNK_BLOCK_COUNT];
        let voxels = derive_voxels(&blocks, &BTreeMap::new());
        assert!(voxels.iter().all(|v| v.kind == VoxelType::Black));
    }

    #[test]
    fn all_air_blocks_derive_to_all_air() {
        let blocks = vec![Block::new(BlockKind::Air); CHUNK_BLOCK_COUNT];
        let voxels = derive_voxels(&blocks, &BTreeMap::new());
        assert!(voxels.iter().all(|v| v.kind == VoxelType::Air));
    }

    #[test]
    fn chiseled_block_uses_sparse_occupancy() {
        // One Stone block at index 0; chisel only voxel (0,0,0) solid.
        let mut blocks = vec![Block::new(BlockKind::Air); CHUNK_BLOCK_COUNT];
        blocks[0] = Block::new(BlockKind::Stone);
        let mut c = ChiselData::new(vec![BlockKind::Stone]);
        c.set(0, 0, 0, true, 0);
        let mut chisel = BTreeMap::new();
        chisel.insert(BlockIndex(0), c);

        let voxels = derive_voxels(&blocks, &chisel);
        assert_eq!(voxels[voxel_index(0, 0, 0)].kind, VoxelType::Black);
        assert_eq!(voxels[voxel_index(1, 0, 0)].kind, VoxelType::Air, "rest of the chiseled block is empty");
    }

    #[test]
    fn derive_is_deterministic() {
        let mut blocks = vec![Block::new(BlockKind::Air); CHUNK_BLOCK_COUNT];
        blocks[3] = Block::new(BlockKind::Stone);
        let a = derive_voxels(&blocks, &BTreeMap::new());
        let b = derive_voxels(&blocks, &BTreeMap::new());
        assert_eq!(a, b);
    }

    #[test]
    fn block_index_roundtrips() {
        for i in 0..CHUNK_BLOCK_COUNT as u8 {
            let (x, y, z) = BlockIndex(i).xyz();
            assert_eq!(BlockIndex::from_xyz(x, y, z), BlockIndex(i));
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p game --lib block::`
Expected: FAIL to compile — `block` module / its items don't exist yet.

- [ ] **Step 3: Write the module**

Create `crates/game/src/block.rs` (above the test module from Step 1):

```rust
//! The block layer: a `Block` (16³ voxels = 1 m³) is the authoritative gameplay
//! unit; voxels are a subdivision derived on demand via [`derive_voxels`].
//! Sub-block detail ("chiseling") is sparse — only chiseled blocks carry a
//! dense 16³ [`ChiselData`]. This module is Bevy-free and deterministic.

use std::collections::BTreeMap;

use block_mesh::ndshape::{ConstShape, ConstShape3u32};

use crate::voxel::{Voxel, VoxelType, CHUNK_VOXEL_COUNT};

pub const BLOCK_VOXELS: usize = 16; // 1 block = 16³ voxels = 1 m³
pub const CHUNK_BLOCKS: usize = 32 / BLOCK_VOXELS; // = 2 blocks per axis
pub const CHUNK_BLOCK_COUNT: usize = CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS; // = 8
pub const BLOCK_VOXEL_COUNT: usize = BLOCK_VOXELS * BLOCK_VOXELS * BLOCK_VOXELS; // = 4096

/// Linear voxel index within a chunk, matching `ChunkShape` (x fastest).
pub fn voxel_index(x: usize, y: usize, z: usize) -> usize {
    ConstShape3u32::<32, 32, 32>::linearize([x as u32, y as u32, z as u32]) as usize
}

/// Index of a block within a chunk, linear over `CHUNK_BLOCKS³` (0..8).
/// `Ord` so it can key a `BTreeMap` deterministically.
#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize, Debug,
)]
pub struct BlockIndex(pub u8);

impl BlockIndex {
    pub fn from_xyz(bx: usize, by: usize, bz: usize) -> Self {
        BlockIndex((bx + by * CHUNK_BLOCKS + bz * CHUNK_BLOCKS * CHUNK_BLOCKS) as u8)
    }
    pub fn xyz(self) -> (usize, usize, usize) {
        let i = self.0 as usize;
        (
            i % CHUNK_BLOCKS,
            (i / CHUNK_BLOCKS) % CHUNK_BLOCKS,
            i / (CHUNK_BLOCKS * CHUNK_BLOCKS),
        )
    }
}

/// The authoritative per-cell gameplay value. Compact; `data` reserved for
/// future block-state (zeroed for now). Hashes trivially.
#[derive(
    Copy, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Debug,
)]
pub struct Block {
    pub kind: BlockKind,
    pub data: [u8; 3],
}

impl Block {
    pub fn new(kind: BlockKind) -> Self {
        Self { kind, data: [0; 3] }
    }
}

/// Gameplay material atom at block granularity. Extensible.
#[derive(
    Copy, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Debug,
)]
pub enum BlockKind {
    #[default]
    Air,
    Stone,
}

impl BlockKind {
    /// The voxel material this block kind derives to.
    pub fn voxel(self) -> VoxelType {
        match self {
            BlockKind::Air => VoxelType::Air,
            BlockKind::Stone => VoxelType::Black,
        }
    }
}

/// Dense 16³ sub-block detail for a chiseled block. Dense (not cuboid-packed)
/// because dense is naturally invertible and trivially hashable — the property
/// a merged-cuboid list lacks. `Vec`-backed to avoid serde big-array friction.
#[derive(Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Debug)]
pub struct ChiselData {
    occupancy: Vec<u64>,       // 64 words = 4096-bit bitset (1 = solid)
    material: Vec<u8>,         // len 4096, palette index per voxel
    palette: Vec<BlockKind>,   // small per-block material list (≤256)
}

impl ChiselData {
    pub fn new(palette: Vec<BlockKind>) -> Self {
        Self {
            occupancy: vec![0u64; BLOCK_VOXEL_COUNT / 64],
            material: vec![0u8; BLOCK_VOXEL_COUNT],
            palette,
        }
    }

    fn local_index(vx: usize, vy: usize, vz: usize) -> usize {
        vx + vy * BLOCK_VOXELS + vz * BLOCK_VOXELS * BLOCK_VOXELS
    }

    pub fn set(&mut self, vx: usize, vy: usize, vz: usize, solid: bool, material_idx: u8) {
        let i = Self::local_index(vx, vy, vz);
        let (w, b) = (i / 64, i % 64);
        if solid {
            self.occupancy[w] |= 1u64 << b;
        } else {
            self.occupancy[w] &= !(1u64 << b);
        }
        self.material[i] = material_idx;
    }

    pub fn is_solid(&self, vx: usize, vy: usize, vz: usize) -> bool {
        let i = Self::local_index(vx, vy, vz);
        (self.occupancy[i / 64] >> (i % 64)) & 1 == 1
    }

    pub fn material_at(&self, vx: usize, vy: usize, vz: usize) -> BlockKind {
        let i = Self::local_index(vx, vy, vz);
        self.palette
            .get(self.material[i] as usize)
            .copied()
            .unwrap_or_default()
    }
}

/// Derive the 32³ voxel array from the authoritative block layer. Output layout
/// is byte-identical to the old `Chunk.voxels`, so the mesher and collider
/// consume it unchanged. Deterministic; the single source of truth for both sim
/// (collider) and client (mesh).
pub fn derive_voxels(blocks: &[Block], chisel: &BTreeMap<BlockIndex, ChiselData>) -> Vec<Voxel> {
    let mut voxels = vec![Voxel::new(VoxelType::Air); CHUNK_VOXEL_COUNT];
    for (idx, block) in blocks.iter().enumerate() {
        let bi = BlockIndex(idx as u8);
        let (bx, by, bz) = bi.xyz();
        let chiseled = chisel.get(&bi);
        for vx in 0..BLOCK_VOXELS {
            for vy in 0..BLOCK_VOXELS {
                for vz in 0..BLOCK_VOXELS {
                    let kind = match chiseled {
                        Some(c) if c.is_solid(vx, vy, vz) => c.material_at(vx, vy, vz),
                        Some(_) => BlockKind::Air,
                        None => block.kind,
                    };
                    let li = voxel_index(bx * BLOCK_VOXELS + vx, by * BLOCK_VOXELS + vy, bz * BLOCK_VOXELS + vz);
                    voxels[li] = Voxel::new(kind.voxel());
                }
            }
        }
    }
    voxels
}
```

Then wire it into `crates/game/src/lib.rs`. Add the module declaration next to the other `pub mod` lines (after `pub mod camera;` at line 17, or with `pub mod voxel;` at line 24):

```rust
pub mod block;
```

And add the glob re-export alongside the others (after `pub use voxel::*;` at line 35):

```rust
pub use block::*;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p game --lib block::`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/block.rs crates/game/src/lib.rs
git commit -m "feat(game): block layer types and derive_voxels"
```

---

### Task 2: Make `Chunk` blocks-authoritative; voxels become derived

**Files:**
- Modify: `crates/game/src/voxel.rs:10-44` (reshape `Chunk`, its `Default`, and `flat_floor`)
- Modify: `crates/game/src/state.rs:20` (import) and `:243-249` (`create_mesh` derives voxels for the collider)
- Modify: `crates/client/src/renderer/bridge.rs:144` (snapshot derives voxels)
- Create: `crates/game/tests/chunk_blocks.rs`

**Interfaces:**
- Consumes: everything Task 1 produces.
- Produces: reshaped `pub struct Chunk { pub blocks: Vec<Block>, pub chisel: BTreeMap<BlockIndex, ChiselData> }` with the same `Chunk::flat_floor(depth: u32) -> Chunk` constructor signature (so all existing call sites keep compiling). `derive_voxels` is the only way to obtain voxels.

- [ ] **Step 1: Write the failing test**

Create `crates/game/tests/chunk_blocks.rs`:

```rust
use game::{derive_voxels, voxel_index, Chunk, VoxelType};
use std::hash::Hash;

fn crc(c: &Chunk) -> u32 {
    let mut h = crc32fast::Hasher::new();
    c.hash(&mut h);
    h.finalize()
}

#[test]
fn flat_floor_is_block_based_with_a_chiseled_slab() {
    let c = Chunk::flat_floor(8);
    assert_eq!(c.blocks.len(), 8, "2×2×2 blocks per chunk");
    // depth 8 leaves the 4 bottom blocks partially filled → chiseled;
    // the 4 top blocks are fully air.
    assert_eq!(c.chisel.len(), 4, "the 4 bottom blocks are partial → chiseled");
}

#[test]
fn chunk_hash_is_stable_and_bincode_roundtrips() {
    let c = Chunk::flat_floor(12);
    assert_eq!(crc(&c), crc(&c.clone()), "clone must hash identically");
    let bytes = bincode::serialize(&c).unwrap();
    let back: Chunk = bincode::deserialize(&bytes).unwrap();
    assert_eq!(crc(&c), crc(&back), "bincode round-trip must hash identically");
}

#[test]
fn derive_reproduces_floor_height() {
    let c = Chunk::flat_floor(8);
    let voxels = derive_voxels(&c.blocks, &c.chisel);
    assert_eq!(voxels[voxel_index(0, 7, 0)].kind, VoxelType::Black, "y<8 solid");
    assert_eq!(voxels[voxel_index(0, 8, 0)].kind, VoxelType::Air, "y>=8 air");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test chunk_blocks`
Expected: FAIL to compile — `Chunk` still has `voxels`/`collider`, not `blocks`/`chisel`.

- [ ] **Step 3a: Reshape `Chunk` in `crates/game/src/voxel.rs`**

Replace the current `Chunk` struct, its `Default`, and `flat_floor` (lines 10-44) with:

```rust
use std::collections::BTreeMap;

use crate::block::{Block, BlockIndex, BlockKind, ChiselData, CHUNK_BLOCKS, CHUNK_BLOCK_COUNT, BLOCK_VOXELS};

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash)]
#[module(crate)]
pub struct Chunk {
    /// Authoritative block grid, length `CHUNK_BLOCK_COUNT` (8).
    pub blocks: Vec<Block>,
    /// Sparse sub-block detail; a block is chiseled iff present here.
    pub chisel: BTreeMap<BlockIndex, ChiselData>,
}

impl Default for Chunk {
    /// All-air. `Component<T>` requires `T: Default`; real content comes from
    /// constructors like [`Chunk::flat_floor`].
    fn default() -> Self {
        Self {
            blocks: vec![Block::default(); CHUNK_BLOCK_COUNT],
            chisel: BTreeMap::new(),
        }
    }
}

impl Chunk {
    /// Solid Stone floor for voxel heights `y < depth`, air above — built at
    /// block granularity. Blocks fully below the floor are whole Stone; blocks
    /// straddling the floor become a chiseled slab; blocks above are air.
    /// Derives (via `derive_voxels`) to the exact same voxel layout the old
    /// voxel-based `flat_floor` produced.
    pub fn flat_floor(depth: u32) -> Self {
        let depth = depth as usize;
        let mut blocks = vec![Block::new(BlockKind::Air); CHUNK_BLOCK_COUNT];
        let mut chisel = BTreeMap::new();
        for by in 0..CHUNK_BLOCKS {
            let (y0, y1) = (by * BLOCK_VOXELS, by * BLOCK_VOXELS + BLOCK_VOXELS);
            for bx in 0..CHUNK_BLOCKS {
                for bz in 0..CHUNK_BLOCKS {
                    let bi = BlockIndex::from_xyz(bx, by, bz);
                    if y1 <= depth {
                        blocks[bi.0 as usize] = Block::new(BlockKind::Stone);
                    } else if y0 >= depth {
                        // stays Air
                    } else {
                        blocks[bi.0 as usize] = Block::new(BlockKind::Stone);
                        let mut c = ChiselData::new(vec![BlockKind::Stone]);
                        for vy in 0..BLOCK_VOXELS {
                            if y0 + vy < depth {
                                for vx in 0..BLOCK_VOXELS {
                                    for vz in 0..BLOCK_VOXELS {
                                        c.set(vx, vy, vz, true, 0);
                                    }
                                }
                            }
                        }
                        chisel.insert(bi, c);
                    }
                }
            }
        }
        Self { blocks, chisel }
    }
}
```

Keep the rest of `voxel.rs` (the `Voxel`, `VoxelType`, `ChunkCoords`, `ChunkShape`, `CHUNK_VOXEL_COUNT`, and the `block_mesh::Voxel`/`MergeVoxel` impls) unchanged. Remove the now-unused `use block_mesh::ndshape::{ConstShape, ConstShape3u32}` import **only if** the compiler flags it as unused after this change; otherwise leave it (it is still used by `ChunkShape`/other code).

> Contingency: if the workspace fails to compile because `borrow::Partial` (the `#[derive(::borrow::Partial)]` on `Chunk`) rejects the `BTreeMap` field, change `chisel` to `Vec<(BlockIndex, ChiselData)>` kept sorted by `BlockIndex`, and update `flat_floor` to `push` + `derive_voxels`/lookups to scan the vec. `Vec` is known to work with `Partial` (the old `voxels: Vec<Voxel>` did). This does not change any public constructor signature.

- [ ] **Step 3b: Derive voxels for the collider in `crates/game/src/state.rs`**

At the top of the file, ensure `derive_voxels` is reachable. The import at line 20 is:

```rust
use crate::voxel::{Chunk, ChunkCoords, ChunkShape, Voxel, VoxelType};
```

Change the body of `create_mesh` (lines 243-249) from reading `chunk.voxels` to deriving:

```rust
        let voxels = crate::derive_voxels(&chunk.blocks, &chunk.chisel);
        // Deterministic linearize order; grid coords are body-local.
        let solid: Vec<Point<i32>> = (0..ChunkShape::SIZE)
            .filter(|i| voxels[*i as usize].kind != VoxelType::Air)
            .map(|i| {
                let [x, y, z] = ChunkShape::delinearize(i);
                Point::new(x as i32, y as i32, z as i32)
            })
            .collect();
```

The subsequent `self.ecs.chunk.set_safe(e, Some(chunk));` still stores the (now block-based) chunk unchanged.

- [ ] **Step 3c: Derive voxels in the client snapshot in `crates/client/src/renderer/bridge.rs`**

Change line 144 from:

```rust
            e.insert(VoxelData(chunk.voxels.clone()));
```

to:

```rust
            e.insert(VoxelData(game::derive_voxels(&chunk.blocks, &chunk.chisel)));
```

(`derive_voxels` is re-exported at the `game` crate root.)

- [ ] **Step 4: Run tests + build to verify green**

Run: `cargo test -p game --test chunk_blocks`
Expected: PASS (3 tests).

Run: `cargo test -p game`
Expected: PASS — the existing rollback suites (`log_model`, `random_ops`, `rollback_restore`, `region_runner`, `region_from_chunks`, `world_manager`) still pass; they build chunks via `Chunk::flat_floor(8)`, whose signature is unchanged, and their `hash(before)==hash(after undo)` assertions are relative so they survive the representation change.

Run: `cargo build --workspace --bins`
Expected: builds (confirms `state.rs` and `bridge.rs` compile against the reshaped `Chunk`).

Run: `cargo test -p client`
Expected: PASS — the bridge snapshot test (`snapshot_carries_entity_kind`, etc.) still works; `VoxelData` is now derived.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/voxel.rs crates/game/src/state.rs crates/client/src/renderer/bridge.rs crates/game/tests/chunk_blocks.rs
git commit -m "feat(game): Chunk stores blocks authoritatively; voxels derived"
```

---

### Task 3: Worldgen chisel coverage + client chiseled-mesh regression + full verification

**Files:**
- Modify: `crates/worldgen/src/lib.rs:31-66` (extend the `tests` module)
- Modify: `crates/client/src/renderer/meshing.rs` (add a test to its `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Chunk::flat_floor` (now block-based, from Task 2), `game::derive_voxels`, `build_chunk_mesh` (client-internal, from `meshing.rs`).
- Produces: nothing new — this task locks in behavior and verifies the whole workspace.

- [ ] **Step 1: Write the worldgen coverage test**

Add to the `tests` module in `crates/worldgen/src/lib.rs` (worldgen builds chunks via `Chunk::flat_floor`, so generated chunks now carry chisel data — this test locks that in):

```rust
    #[test]
    fn generated_chunks_carry_chisel_slabs() {
        // Every floor chunk straddles the floor height (8 or 12 voxels, both
        // sub-block), so its bottom blocks are chiseled.
        let chunks = generate_region(RegionCoords::new(0, 0));
        assert!(
            chunks.iter().all(|(_, c)| !c.chisel.is_empty()),
            "every generated floor chunk should have a chiseled slab"
        );
        assert!(
            chunks.iter().all(|(_, c)| c.blocks.len() == 8),
            "every chunk has 8 blocks"
        );
    }
```

- [ ] **Step 2: Run the worldgen tests**

Run: `cargo test -p worldgen`
Expected: PASS. `generated_chunks_carry_chisel_slabs` passes on the first run because Task 2 already made `flat_floor` emit chisel data — this is a regression lock, not new behavior. If it FAILS, Task 2's `flat_floor` is not emitting chisel for depths 8/12 — fix there before proceeding. `generation_is_pure` (still asserting 64 chunks + equal crc) and the checkerboard tests still pass.

- [ ] **Step 3: Write the client chiseled-mesh regression test**

Add to the `#[cfg(test)] mod tests` in `crates/client/src/renderer/meshing.rs` (this exercises the full block → `derive_voxels` → greedy mesh path the client relies on):

```rust
    #[test]
    fn derives_and_meshes_a_generated_floor_chunk() {
        let chunk = game::Chunk::flat_floor(8);
        let voxels = game::derive_voxels(&chunk.blocks, &chunk.chisel);
        let mesh = build_chunk_mesh(&voxels);
        let mesh = mesh.expect("a floor chunk has solid voxels and must produce a mesh");
        assert!(
            mesh.indices().map(|i| i.len()).unwrap_or(0) > 0,
            "the meshed floor must have triangles"
        );
    }
```

> If `build_chunk_mesh` takes a different argument shape than `&[Voxel]` / returns a differently-named type, match the existing meshing tests in the same module — do not change `build_chunk_mesh`'s signature. If `build_chunk_mesh` is not in scope from the test module, add `use super::build_chunk_mesh;` (it is defined in the same file).

- [ ] **Step 4: Run the client tests**

Run: `cargo test -p client`
Expected: PASS — the new test plus the existing 26.

- [ ] **Step 5: Full-workspace verification**

Run: `cargo test --workspace`
Expected: PASS across `game`, `worldgen`, `client`, `server`.

Run: `cargo build --workspace --bins`
Expected: success.

Run: `cargo check -p client --target wasm32-unknown-unknown`
Expected: success (confirms no `usize` leaked into serialized block types; requires `rustup target add wasm32-unknown-unknown`).

- [ ] **Step 6: Commit**

```bash
git add crates/worldgen/src/lib.rs crates/client/src/renderer/meshing.rs
git commit -m "test: lock in worldgen chisel slabs and client chiseled-mesh path"
```

---

## Self-Review

**Spec coverage** (against `2026-07-06-block-abstraction-data-model-design.md`):
- Three-layer model (blocks authoritative + sparse chisel; voxels transient) → Tasks 1 & 2. Block-entities layer correctly omitted (open question).
- `Block`/`BlockKind`/`BlockIndex`/`ChiselData` types + dense-bitset chisel → Task 1.
- Single shared `derive_voxels` used by sim collider and client mesh → Tasks 1, 2, 3.
- 32³ chunk & region math untouched; 2×2×2 blocks → constants in Task 1, `flat_floor` in Task 2.
- Wire carries blocks+chisel, client derives → Task 2 (Chunk has no voxels field; bridge derives). The `ServerPacket::Region` snapshot serializes the reshaped `Chunk` automatically.
- No new undo machinery (set-once `Component<Chunk>`) → Task 2 leaves `set_safe` untouched.
- Hashing bit-exact / determinism / wasm no-usize → Task 2 hash+bincode test, Task 3 wasm check, Global Constraints.
- Worldgen emits a chiseled slab for sub-block floor heights → Task 2 `flat_floor` + Task 3 assertion.
- Non-goal note: the spec suggested reshaping the render-update enum (`SetVoxelComponent` → block-shaped). Investigation found `SetVoxelComponent` has **no emitter** (dead render channel; chunks reach the client via the snapshot path). Per YAGNI it is left carrying `Vec<Voxel>` and untouched this milestone; it will be reshaped by the interaction milestone that actually emits it. This is a deliberate, documented deviation.

**Placeholder scan:** No TBD/TODO/"handle edge cases". Two contingency notes (BTreeMap-in-Partial fallback; `build_chunk_mesh` signature match) are concrete fallbacks with exact instructions, not placeholders.

**Type consistency:** `derive_voxels(&[Block], &BTreeMap<BlockIndex, ChiselData>) -> Vec<Voxel>`, `voxel_index(usize,usize,usize) -> usize`, `Chunk { blocks, chisel }`, `Chunk::flat_floor(u32)`, `BlockKind::voxel()`, `ChiselData::{new,set,is_solid,material_at}`, `BlockIndex::{from_xyz,xyz,0}` are used identically across Tasks 1–3.
