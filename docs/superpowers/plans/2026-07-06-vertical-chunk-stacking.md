# Vertical Chunk Stacking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restructure a region's terrain from a flat single layer of 32³ chunks into unbounded vertical columns of stacked sub-chunks (Veloren "Chonk" model), so the world gains a real vertical dimension — data-model foundation only, no new gameplay.

**Architecture:** A new `volume.rs` in `crates/game` adds `SubChunk` (air-collapsed or `Dense(Box<Chunk>)`), `Column` (sparse `Vec<SubChunk>` + signed `base_sub_y` + `below`/`above` sentinel blocks), and `ReadVol`/`WriteVol` traits at *block* granularity. Worldgen produces columns; `Region::from_columns` flattens the materialized (`Dense`) sub-chunks into the existing per-chunk `create_mesh` path. The 2D region topology (`RegionCoords`, boundary handoff, parking) is untouched — verticality is region-local.

**Tech Stack:** Rust (edition 2021 in `game`), `bincode` serialization, `crc32fast` state hashing, nalgebra/rapier vendored forks (unchanged here), Bevy client (mesher unchanged).

## Global Constraints

- `game` and `server` stay Bevy-free and windowing-library-free. `volume.rs` must not pull in rendering/windowing deps. Use a local `BlockPos { x, y, z: i32 }`, **not** `bevy_math::IVec3`.
- Determinism bar: state hashing is `crc32fast` over `bincode` bytes; the rollback correctness bar is bit-exact `hash(before) == hash(after undo)`. Every terrain representation MUST be **canonical** — exactly one byte form per logical content.
- Sub-chunk / chunk size is fixed at 32³ voxels = `CHUNK_BLOCKS` (2) blocks per axis; `CHUNK_BLOCK_COUNT` = 8. `REGION_CHUNKS` = 8 (columns per region axis).
- Air is `BlockId::AIR`; the solid material is a registry-resolved `BlockId` passed into worldgen (never hardcoded).
- Keep the `justfile` in sync if any build/test command changes (none expected).
- `ReadVol`/`WriteVol` operate at **whole-block** granularity. Chisel/voxel sub-block detail is a `Dense`-chunk concern and is out of scope for the volume accessor.

## Deviations from the approved spec (with rationale)

These were discovered while grounding the plan in the real call sites; they reduce scope in the user's favor and were surfaced before writing:

1. **`SerializedRegion` is unchanged.** It bincodes the whole `Rollback`; materialized `Dense` sub-chunks are already stored as per-entity `chunk` components, so park→restore round-trips with no new code. Spec §8's "serialize the column form" is unnecessary. Determinism rides on existing chunk-component hashing.
2. **The client mesher is not genericized over `ReadVol`.** `build_chunk_mesh` consumes `&[Voxel]` (derived voxels), not a `Chunk`. Each `Dense` sub-chunk still flows through the existing per-chunk `create_mesh` → `derive_voxels` → `VoxelData` path unchanged. `ReadVol`/`WriteVol` serve the block layer (worldgen construction, `normalize`, future editing, tests).
3. **`ReadVol` is block-granularity**, matching `Chunk.blocks` (the authoritative gameplay unit), not voxel-granularity.

---

## File Structure

- **Create** `crates/game/src/volume.rs` — `BlockPos`, `ReadVol`, `WriteVol`, `SubChunk`, `Column`, `ColumnCoords`, `normalize`. One responsibility: sparse vertical block storage + access.
- **Create** `crates/game/tests/volume.rs` — unit + determinism tests for the above.
- **Modify** `crates/game/src/voxel.rs` — `ChunkCoords.y` `usize` → `i32`; add block-access helpers on `Chunk`.
- **Modify** `crates/game/src/lib.rs` — `pub mod volume;` + glob re-export.
- **Modify** `crates/game/src/region.rs` — add `Region::from_columns`.
- **Modify** `crates/game/src/region_runner.rs` — `RegionSeed` carries columns; `into_region` uses `from_columns`.
- **Modify** `crates/game/src/world_manager.rs` — `RegionGenerator` returns columns; seed construction sites.
- **Modify** `crates/worldgen/src/lib.rs` — `generate_region` returns `Vec<(ColumnCoords, Column)>`.
- **Touch (compile-follow)** `crates/server/src/lib.rs`, `crates/client/src/local_server.rs`, `crates/server/tests/threaded_world.rs`, `crates/game/tests/*`, `crates/client/src/instance.rs` — the generator closures/`from_chunks` empty-region test sites.

---

### Task 1: Signed vertical chunk coordinate + `ColumnCoords`

**Files:**
- Modify: `crates/game/src/voxel.rs` (`ChunkCoords` struct + `new`)
- Create: (new type) `ColumnCoords` in `crates/game/src/volume.rs` — but to keep Task 1 self-contained, define `ColumnCoords` here in `voxel.rs` next to `ChunkCoords` and re-export; §Task 2 moves nothing.
- Test: `crates/game/src/voxel.rs` inline `#[cfg(test)]`

**Interfaces:**
- Produces: `ChunkCoords { x: usize, y: i32, z: usize }`, `ChunkCoords::new(x: usize, y: i32, z: usize)`. `ColumnCoords { x: usize, z: usize }`, `ColumnCoords::new(x: usize, z: usize)`.

- [ ] **Step 1: Write the failing test**

Add to `crates/game/src/voxel.rs`:

```rust
#[cfg(test)]
mod coord_tests {
    use super::*;

    #[test]
    fn chunk_coords_allow_negative_y() {
        let c = ChunkCoords::new(3, -2, 5);
        assert_eq!(c.y, -2);
        assert_eq!(c.x, 3);
        assert_eq!(c.z, 5);
    }

    #[test]
    fn column_coords_construct() {
        let c = ColumnCoords::new(1, 7);
        assert_eq!((c.x, c.z), (1, 7));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p game coord_tests 2>&1 | tail -20`
Expected: FAIL — `ColumnCoords` not found / `-2` not valid for `usize`.

- [ ] **Step 3: Implement**

In `crates/game/src/voxel.rs`, change `ChunkCoords`:

```rust
// Keep the struct's EXISTING derive list verbatim — only the `y` field type
// changes (usize -> i32). Do not add/remove derives.
#[derive(
    Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, PartialOrd, Ord, Eq,
)]
pub struct ChunkCoords {
    pub(crate) x: usize,
    pub(crate) y: i32,
    pub(crate) z: usize,
}

impl ChunkCoords {
    pub fn new(x: usize, y: i32, z: usize) -> Self {
        Self { x, y, z }
    }
}

/// Region-local (x, z) address of a vertical column, 0..REGION_CHUNKS.
#[derive(
    Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub struct ColumnCoords {
    pub x: usize,
    pub z: usize,
}

impl ColumnCoords {
    pub fn new(x: usize, z: usize) -> Self {
        Self { x, z }
    }
}
```

Note: do NOT add `Hash` or any other derive to `ChunkCoords` — the crate compiles with its existing derive set; only the `y` field type changes.

- [ ] **Step 4: Fix `create_mesh` arithmetic**

In `crates/game/src/state.rs` `create_mesh`, the Y translation now multiplies an `i32`:

```rust
Real::from((coords.y * 32) as RawReal),
```

`coords.y * 32` is `i32`; the `as RawReal` cast already handles it. Confirm no other `coords.y` site assumes `usize` — run:

Run: `rg 'coords\.y|\.y \* 32|ChunkCoords' crates/game/src crates/client/src crates/server/src`
Fix any `usize`-typed arithmetic on `.y` (e.g. a `usize` cast) to `i32`.

- [ ] **Step 5: Run the whole game crate to catch coordinate fallout**

Run: `cargo test -p game --no-run 2>&1 | tail -30`
Expected: compiles. Test-helper `ChunkCoords::new(x, 0, z)` calls still work (`0` is a valid `i32`). If any caller passes a `usize` variable for `y`, change it to `i32` at the call site.

- [ ] **Step 6: Run the test**

Run: `cargo test -p game coord_tests -- --nocapture 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/game/src/voxel.rs crates/game/src/state.rs
git commit -m "feat(game): signed ChunkCoords.y + ColumnCoords for vertical stacking

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `BlockPos`, `ReadVol`/`WriteVol`, and `SubChunk`

**Files:**
- Create: `crates/game/src/volume.rs`
- Modify: `crates/game/src/lib.rs` (add `pub mod volume;` + glob re-export)
- Modify: `crates/game/src/voxel.rs` (add `Chunk::block_at` / `Chunk::set_block` / `Chunk::is_all_air`)
- Test: inline `#[cfg(test)]` in `volume.rs`

**Interfaces:**
- Consumes: `Block`, `BlockId`, `BlockIndex`, `Chunk`, `CHUNK_BLOCKS` from Task 1's crate.
- Produces:
  - `struct BlockPos { pub x: i32, pub y: i32, pub z: i32 }` + `BlockPos::new`.
  - `trait ReadVol { fn get(&self, pos: BlockPos) -> Block; }`
  - `trait WriteVol { fn set(&mut self, pos: BlockPos, block: Block) -> Block; }`
  - `enum SubChunk { Air, Dense(Box<Chunk>) }`, `SubChunk::is_air(&self) -> bool`.
  - `Chunk::block_at(bx, by, bz) -> Block`, `Chunk::set_block(bx, by, bz, Block)`, `Chunk::is_all_air() -> bool`.

- [ ] **Step 1: Write the failing test**

Create `crates/game/src/volume.rs` with only the tests first (types come in Step 3):

```rust
#[cfg(test)]
mod subchunk_tests {
    use super::*;
    use crate::{Block, BlockId, Chunk};

    fn stone() -> Block { Block::new(BlockId(2)) }

    #[test]
    fn air_subchunk_reads_air_everywhere() {
        let s = SubChunk::Air;
        assert_eq!(s.get(BlockPos::new(0, 0, 0)).id, BlockId::AIR);
        assert_eq!(s.get(BlockPos::new(1, 1, 1)).id, BlockId::AIR);
        assert!(s.is_air());
    }

    #[test]
    fn dense_subchunk_reads_written_block() {
        let mut s = SubChunk::Dense(Box::new(Chunk::default()));
        let prev = s.set(BlockPos::new(1, 0, 1), stone());
        assert_eq!(prev.id, BlockId::AIR);
        assert_eq!(s.get(BlockPos::new(1, 0, 1)), stone());
        assert!(!s.is_air());
    }

    #[test]
    fn chunk_all_air_detection() {
        assert!(Chunk::default().is_all_air());
        let mut c = Chunk::default();
        c.set_block(0, 0, 0, stone());
        assert!(!c.is_all_air());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p game subchunk_tests 2>&1 | tail -20`
Expected: FAIL — module/types not defined.

- [ ] **Step 3: Implement the block helpers on `Chunk`**

In `crates/game/src/voxel.rs`, inside `impl Chunk`:

```rust
/// Block at block-granularity coords (each axis 0..CHUNK_BLOCKS).
pub fn block_at(&self, bx: usize, by: usize, bz: usize) -> Block {
    self.blocks[BlockIndex::from_xyz(bx, by, bz).0 as usize]
}

/// Set a whole block, clearing any chisel detail for that block cell
/// (whole-block granularity — sub-block chisel is not addressed here).
pub fn set_block(&mut self, bx: usize, by: usize, bz: usize, block: Block) {
    let bi = BlockIndex::from_xyz(bx, by, bz);
    self.blocks[bi.0 as usize] = block;
    self.chisel.remove(&bi);
}

/// True iff every block is air and no chisel detail remains.
pub fn is_all_air(&self) -> bool {
    self.chisel.is_empty() && self.blocks.iter().all(|b| b.id == BlockId::AIR)
}
```

Ensure `BlockIndex` is imported in `voxel.rs` (it already imports from `crate::block`).

- [ ] **Step 4: Implement `volume.rs` types**

Prepend to `crates/game/src/volume.rs` (above the test module):

```rust
//! Sparse vertical block storage. A `Column` is an unbounded stack of 32³
//! `SubChunk`s over one region-local (x, z) cell, with constant sentinels
//! below/above the materialized band. Bevy-free and deterministic.

use crate::block::CHUNK_BLOCKS;
use crate::registry::BlockId;
use crate::voxel::Chunk;
use crate::Block;

/// Whole-block position. `x`/`z` are block coords within a footprint
/// (0..CHUNK_BLOCKS); `y` is a block Y whose frame depends on the impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPos {
    pub fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }
}

/// Read blocks from a volume at whole-block granularity.
pub trait ReadVol {
    fn get(&self, pos: BlockPos) -> Block;
}

/// Write a whole block; returns the previous block.
pub trait WriteVol {
    fn set(&mut self, pos: BlockPos, block: Block) -> Block;
}

/// One 32³ slice of a column. Only the all-air case is collapsed; every
/// other uniform case (including all-stone) stays `Dense` (gets no collider,
/// see plan §Task 5). This keeps a single canonical form per content.
///
/// Derive only `Serialize`/`Deserialize` (+ Debug/Clone). `Chunk` derives
/// `Hash` but NOT `PartialEq`/`Eq`, so `SubChunk` must not derive equality —
/// canonical comparison is done over bincode bytes, never `==`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum SubChunk {
    Air,
    Dense(Box<Chunk>),
}

impl SubChunk {
    pub fn is_air(&self) -> bool {
        matches!(self, SubChunk::Air)
    }
}

impl ReadVol for SubChunk {
    fn get(&self, pos: BlockPos) -> Block {
        match self {
            SubChunk::Air => Block::new(BlockId::AIR),
            SubChunk::Dense(c) => c.block_at(pos.x as usize, pos.y as usize, pos.z as usize),
        }
    }
}

impl WriteVol for SubChunk {
    fn set(&mut self, pos: BlockPos, block: Block) -> Block {
        // Materialize on first write into an Air slice.
        if let SubChunk::Air = self {
            *self = SubChunk::Dense(Box::new(Chunk::default()));
        }
        let SubChunk::Dense(c) = self else { unreachable!() };
        let prev = c.block_at(pos.x as usize, pos.y as usize, pos.z as usize);
        c.set_block(pos.x as usize, pos.y as usize, pos.z as usize, block);
        prev
    }
}

// Referenced so CHUNK_BLOCKS stays a compile-time anchor for callers that
// bound BlockPos; also documents the footprint size.
const _: () = assert!(CHUNK_BLOCKS == 2);
```

Note: `Chunk` derives `Hash` but not `PartialEq`/`Eq`, so `SubChunk` deliberately derives neither — do not add them. Equality in tests is expressed via `matches!` or bincode-byte comparison, never `==` on a `SubChunk`.

- [ ] **Step 5: Wire the module**

In `crates/game/src/lib.rs`, add near the other `pub mod`/re-exports:

```rust
pub mod volume;
pub use volume::*;
```

Run: `cargo build -p game 2>&1 | tail -20`
Expected: compiles. `SubChunk` derives only `Debug, Clone, Serialize, Deserialize` (Step 4) precisely because `Chunk` is not `PartialEq`/`Eq` — if you see an "the trait `PartialEq` is not implemented for `Chunk`" error, you added an equality derive; remove it.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p game subchunk_tests -- --nocapture 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/game/src/volume.rs crates/game/src/voxel.rs crates/game/src/lib.rs
git commit -m "feat(game): volume layer — BlockPos, ReadVol/WriteVol, SubChunk

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `Column` with sentinels + canonical `normalize`

**Files:**
- Modify: `crates/game/src/volume.rs` (add `Column`)
- Test: inline `#[cfg(test)]` in `volume.rs`

**Interfaces:**
- Consumes: `SubChunk`, `BlockPos`, `ReadVol`/`WriteVol`, `Block`, `BlockId`, `Chunk`.
- Produces:
  - `struct Column { base_sub_y: i32, below: Block, above: Block, sub_chunks: Vec<SubChunk> }`
  - `Column::new(base_sub_y, below, above, sub_chunks) -> Self` (auto-normalizes)
  - `Column::normalize(&mut self)` — idempotent canonicalizer
  - `impl ReadVol for Column` (`pos.y` is a column-global block Y)
  - `impl WriteVol for Column` (grows band + re-normalizes)
  - `Column::sub_chunk_at(sub_y: i32) -> Option<&SubChunk>` (None ⇒ sentinel band)

Frame convention: a sub-chunk covers `CHUNK_BLOCKS` (2) blocks of Y. For a column-global block position `pos.y`, `sub_y = pos.y.div_euclid(CHUNK_BLOCKS as i32)` and the in-slice block Y is `pos.y.rem_euclid(CHUNK_BLOCKS as i32)`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/game/src/volume.rs`:

```rust
#[cfg(test)]
mod column_tests {
    use super::*;
    use crate::{Block, BlockId, Chunk};

    fn stone() -> Block { Block::new(BlockId(2)) }
    fn air() -> Block { Block::new(BlockId::AIR) }

    fn dense_stone() -> SubChunk {
        let mut c = Chunk::default();
        for bx in 0..CHUNK_BLOCKS { for by in 0..CHUNK_BLOCKS { for bz in 0..CHUNK_BLOCKS {
            c.set_block(bx, by, bz, stone());
        }}}
        SubChunk::Dense(Box::new(c))
    }

    #[test]
    fn reads_sentinels_outside_band() {
        // One dense stone sub-chunk at sub_y = 0; below = stone, above = air.
        let col = Column::new(0, stone(), air(), vec![dense_stone()]);
        // Below the band (sub_y = -1): stone sentinel.
        assert_eq!(col.get(BlockPos::new(0, -1, 0)), stone());
        // Inside the band: stored stone.
        assert_eq!(col.get(BlockPos::new(0, 0, 0)), stone());
        // Above the band (sub_y = 5): air sentinel.
        assert_eq!(col.get(BlockPos::new(0, 20, 0)), air());
    }

    #[test]
    fn dense_all_air_collapses_on_normalize() {
        let air_dense = SubChunk::Dense(Box::new(Chunk::default()));
        let col = Column::new(0, stone(), air(), vec![air_dense]);
        // After normalize the sole air-dense sub-chunk collapses + trailing-trims.
        assert!(col.sub_chunks.is_empty());
    }

    #[test]
    fn trailing_air_is_trimmed() {
        let col = Column::new(0, stone(), air(), vec![dense_stone(), SubChunk::Air, SubChunk::Air]);
        assert_eq!(col.sub_chunks.len(), 1);
        assert!(matches!(col.sub_chunks[0], SubChunk::Dense(_)));
    }

    #[test]
    fn normalize_is_idempotent() {
        let mut col = Column::new(0, stone(), air(), vec![dense_stone(), SubChunk::Air]);
        let before = bincode::serialize(&col).unwrap();
        col.normalize();
        let after = bincode::serialize(&col).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn serialize_roundtrip_is_bit_exact() {
        let col = Column::new(0, stone(), air(), vec![dense_stone()]);
        let bytes = bincode::serialize(&col).unwrap();
        let back: Column = bincode::deserialize(&bytes).unwrap();
        assert_eq!(bincode::serialize(&back).unwrap(), bytes);
    }

    #[test]
    fn write_above_band_grows_then_normalizes() {
        let mut col = Column::new(0, stone(), air(), vec![dense_stone()]);
        // Write a stone block well above the band.
        let far = BlockPos::new(0, 40, 0);
        assert_eq!(col.get(far), air()); // sentinel before
        col.set(far, stone());
        assert_eq!(col.get(far), stone()); // present after
        // Writing air back at the same spot returns to canonical (collapse/trim).
        col.set(far, air());
        assert_eq!(col.get(far), air());
        // Re-normalized: band is back to the single dense sub-chunk.
        assert_eq!(col.sub_chunks.len(), 1);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p game column_tests 2>&1 | tail -20`
Expected: FAIL — `Column` not defined.

- [ ] **Step 3: Implement `Column`**

Add to `crates/game/src/volume.rs`:

```rust
/// An unbounded vertical column of sub-chunks over one region-local (x, z)
/// cell. Sparse: only `sub_chunks` is materialized; below the band reads as
/// `below`, above as `above`. Always stored normalized (canonical).
///
/// No `PartialEq`/`Eq` (would demand `Chunk: PartialEq`); compare via bincode
/// bytes when a test needs equality.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Column {
    base_sub_y: i32,
    below: Block,
    above: Block,
    sub_chunks: Vec<SubChunk>,
}

impl Column {
    const SUB_BLOCKS_Y: i32 = CHUNK_BLOCKS as i32;

    pub fn new(base_sub_y: i32, below: Block, above: Block, sub_chunks: Vec<SubChunk>) -> Self {
        let mut c = Self { base_sub_y, below, above, sub_chunks };
        c.normalize();
        c
    }

    pub fn base_sub_y(&self) -> i32 {
        self.base_sub_y
    }

    pub fn sub_chunks(&self) -> &[SubChunk] {
        &self.sub_chunks
    }

    /// The stored sub-chunk at absolute sub-Y, or None if outside the band.
    pub fn sub_chunk_at(&self, sub_y: i32) -> Option<&SubChunk> {
        let idx = sub_y.checked_sub(self.base_sub_y)?;
        usize::try_from(idx).ok().and_then(|i| self.sub_chunks.get(i))
    }

    /// Canonicalize: collapse all-air `Dense` to `Air`, then trim trailing
    /// `Air` (equal to the `above` sentinel). Leading trim only when the
    /// `below` sentinel is itself air (never, for terrain). Idempotent.
    pub fn normalize(&mut self) {
        let above_is_air = self.above.id == BlockId::AIR;
        for s in &mut self.sub_chunks {
            if let SubChunk::Dense(c) = s {
                if c.is_all_air() {
                    *s = SubChunk::Air;
                }
            }
        }
        if above_is_air {
            while matches!(self.sub_chunks.last(), Some(SubChunk::Air)) {
                self.sub_chunks.pop();
            }
        }
        if self.below.id == BlockId::AIR {
            while matches!(self.sub_chunks.first(), Some(SubChunk::Air)) {
                self.sub_chunks.remove(0);
                self.base_sub_y += 1;
            }
        }
    }
}

impl ReadVol for Column {
    fn get(&self, pos: BlockPos) -> Block {
        let sub_y = pos.y.div_euclid(Self::SUB_BLOCKS_Y);
        let local_y = pos.y.rem_euclid(Self::SUB_BLOCKS_Y);
        match self.sub_chunk_at(sub_y) {
            Some(s) => s.get(BlockPos::new(pos.x, local_y, pos.z)),
            None if sub_y < self.base_sub_y => self.below,
            None => self.above,
        }
    }
}

impl WriteVol for Column {
    fn set(&mut self, pos: BlockPos, block: Block) -> Block {
        let sub_y = pos.y.div_euclid(Self::SUB_BLOCKS_Y);
        let local_y = pos.y.rem_euclid(Self::SUB_BLOCKS_Y);
        // Grow the band to include sub_y.
        if self.sub_chunks.is_empty() {
            self.base_sub_y = sub_y;
            self.sub_chunks.push(SubChunk::Air);
        }
        while sub_y < self.base_sub_y {
            self.sub_chunks.insert(0, SubChunk::Air);
            self.base_sub_y -= 1;
        }
        let top = self.base_sub_y + self.sub_chunks.len() as i32;
        for _ in top..=sub_y {
            self.sub_chunks.push(SubChunk::Air);
        }
        let idx = (sub_y - self.base_sub_y) as usize;
        let prev = self.sub_chunks[idx].set(BlockPos::new(pos.x, local_y, pos.z), block);
        self.normalize();
        prev
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p game column_tests -- --nocapture 2>&1 | tail -30`
Expected: PASS (all 6).

If `write_above_band_grows_then_normalizes`'s final `sub_chunks.len() == 1` fails, check that setting air back collapses the grown `Dense` (it starts as `Air`, `set(air)` materializes it to a `Dense` all-air, which `normalize` collapses + trailing-trims). That is the intended path.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/volume.rs
git commit -m "feat(game): Column with sentinels + canonical normalize

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Flatten columns into the region + `Region::from_columns`

**Files:**
- Modify: `crates/game/src/volume.rs` (add `Column::materialized_sub_chunks`)
- Modify: `crates/game/src/region.rs` (add `Region::from_columns`)
- Test: `crates/game/tests/region_from_columns.rs` (new)

**Interfaces:**
- Consumes: `Column`, `ColumnCoords`, `ChunkCoords`, `Chunk`, `SubChunk`, `Region::from_chunks`.
- Produces:
  - `Column::materialized_sub_chunks(&self) -> Vec<(i32, Chunk)>` — yields `(sub_y, chunk)` for each `Dense` sub-chunk only (clones the chunk).
  - `Region::from_columns(id: RegionId, columns: Vec<(ColumnCoords, Column)>) -> Region`.

- [ ] **Step 1: Write the failing test**

Create `crates/game/tests/region_from_columns.rs`:

```rust
use game::{Block, BlockId, Chunk, Column, ColumnCoords, RegionCoords, SubChunk};

fn stone() -> Block { Block::new(BlockId(2)) }
fn air() -> Block { Block::new(BlockId::AIR) }

fn dense_floor() -> SubChunk {
    SubChunk::Dense(Box::new(Chunk::flat_floor_with(8, BlockId(2))))
}

#[test]
fn materialized_sub_chunks_skip_air() {
    // base_sub_y = 0, one dense floor, then (trimmed) air above.
    let col = Column::new(0, stone(), air(), vec![dense_floor(), SubChunk::Air]);
    let mats = col.materialized_sub_chunks();
    assert_eq!(mats.len(), 1);
    assert_eq!(mats[0].0, 0); // sub_y
}

#[test]
fn from_columns_builds_a_region() {
    let id = RegionCoords::new(0, 0);
    let columns = vec![
        (ColumnCoords::new(0, 0), Column::new(0, stone(), air(), vec![dense_floor()])),
        (ColumnCoords::new(1, 0), Column::new(0, stone(), air(), vec![dense_floor()])),
    ];
    // Must not panic; produces a region with the two flattened sub-chunks.
    let _region = game::Region::from_columns(id, columns);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p game --test region_from_columns 2>&1 | tail -20`
Expected: FAIL — `materialized_sub_chunks` / `from_columns` not found.

- [ ] **Step 3: Implement the flattener**

Add to `impl Column` in `crates/game/src/volume.rs`:

```rust
/// Materialized (`Dense`) sub-chunks as `(absolute sub_y, chunk)` pairs;
/// `Air` slices are skipped. Used to seed a region's per-chunk physics path.
pub fn materialized_sub_chunks(&self) -> Vec<(i32, Chunk)> {
    self.sub_chunks
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match s {
            SubChunk::Dense(c) => Some((self.base_sub_y + i as i32, (**c).clone())),
            SubChunk::Air => None,
        })
        .collect()
}
```

- [ ] **Step 4: Implement `Region::from_columns`**

In `crates/game/src/region.rs`, next to `from_chunks`:

```rust
/// Seed a region from vertical columns: flatten each column's materialized
/// (Dense) sub-chunks into the existing per-chunk mesh/collider path.
pub fn from_columns(id: RegionId, columns: Vec<(ColumnCoords, Column)>) -> Self {
    let chunks: Vec<(ChunkCoords, Chunk)> = columns
        .into_iter()
        .flat_map(|(col, column)| {
            column
                .materialized_sub_chunks()
                .into_iter()
                .map(move |(sub_y, chunk)| (ChunkCoords::new(col.x, sub_y, col.z), chunk))
        })
        .collect();
    Region::from_chunks(id, chunks)
}
```

Add `Column, ColumnCoords` to the `use` imports at the top of `region.rs` (they re-export from the crate root).

- [ ] **Step 5: Run the tests**

Run: `cargo test -p game --test region_from_columns -- --nocapture 2>&1 | tail -20`
Expected: PASS (both).

- [ ] **Step 6: Commit**

```bash
git add crates/game/src/volume.rs crates/game/src/region.rs crates/game/tests/region_from_columns.rs
git commit -m "feat(game): flatten columns into region via from_columns

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Worldgen produces columns; thread the seam

**Files:**
- Modify: `crates/worldgen/src/lib.rs` (`generate_region` return type + body + tests)
- Modify: `crates/game/src/world_manager.rs` (`RegionGenerator` type; seed sites lines ~29, ~378-379)
- Modify: `crates/game/src/region_runner.rs` (`RegionSeed` variants + `into_region`)
- Compile-follow: `crates/server/src/lib.rs`, `crates/client/src/local_server.rs`, `crates/server/tests/threaded_world.rs`

**Interfaces:**
- Consumes: `Column`, `ColumnCoords`, `SubChunk`, `Chunk::flat_floor_with`, `BlockId`, `REGION_CHUNKS`, `floor_height`.
- Produces: `generate_region(coords: RegionCoords, solid: BlockId) -> Vec<(ColumnCoords, Column)>` (64 columns). `RegionGenerator = Box<dyn FnMut(RegionCoords) -> Vec<(ColumnCoords, Column)> + Send>`. `RegionSeed::Fresh(Vec<(ColumnCoords, Column)>)`, `RegionSeed::Parked(SerializedRegion, Vec<(ColumnCoords, Column)>)`.

- [ ] **Step 1: Write the failing worldgen tests**

Replace the assertions in `crates/worldgen/src/lib.rs`'s `#[cfg(test)] mod tests` that reference `chunks.len() == 64` and the `crc` helper to operate on columns. Add/replace:

```rust
#[test]
fn produces_sixty_four_columns() {
    let cols = generate_region(RegionCoords::new(0, 0), TEST_STONE);
    assert_eq!(cols.len(), REGION_CHUNKS * REGION_CHUNKS); // 64
}

#[test]
fn generation_is_deterministic() {
    let a = generate_region(RegionCoords::new(-3, 7), TEST_STONE);
    let b = generate_region(RegionCoords::new(-3, 7), TEST_STONE);
    assert_eq!(crc(&a), crc(&b));
}

#[test]
fn parity_regions_differ() {
    let even = generate_region(RegionCoords::new(0, 0), TEST_STONE);
    let odd = generate_region(RegionCoords::new(1, 0), TEST_STONE);
    assert_ne!(crc(&even), crc(&odd));
}
```

Update the `crc` helper's signature to take `&[(ColumnCoords, Column)]`:

```rust
fn crc(cols: &[(ColumnCoords, Column)]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    let bytes = bincode::serialize(cols).unwrap();
    h.update(&bytes);
    h.finalize()
}
```

Update the test `use super::*;` to also bring `Column, ColumnCoords, SubChunk` (they re-export from `game`); add `use game::{Column, ColumnCoords};` if needed.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p worldgen 2>&1 | tail -20`
Expected: FAIL — return type mismatch / `ColumnCoords` unresolved.

- [ ] **Step 3: Implement column worldgen**

Rewrite `generate_region` in `crates/worldgen/src/lib.rs`:

```rust
use game::{
    Block, BlockId, Chunk, Column, ColumnCoords, RegionCoords, SubChunk, REGION_CHUNKS,
};

/// The full 8×8 column grid for one region, region-local coordinates.
/// Pure and deterministic; no clocks, no RNG. `solid` is the floor material.
/// Each column: stone sentinel below, air above, one straddling floor
/// sub-chunk at sub_y = 0.
pub fn generate_region(coords: RegionCoords, solid: BlockId) -> Vec<(ColumnCoords, Column)> {
    let depth = floor_height(coords);
    let stone = Block::new(solid);
    let air = Block::new(BlockId::AIR);
    let mut columns = Vec::with_capacity(REGION_CHUNKS * REGION_CHUNKS);
    for x in 0..REGION_CHUNKS {
        for z in 0..REGION_CHUNKS {
            let floor = SubChunk::Dense(Box::new(Chunk::flat_floor_with(depth, solid)));
            let column = Column::new(0, stone, air, vec![floor]);
            columns.push((ColumnCoords::new(x, z), column));
        }
    }
    columns
}
```

Keep `floor_height` unchanged.

- [ ] **Step 4: Thread the generator/seed types**

In `crates/game/src/world_manager.rs` line ~29:

```rust
pub type RegionGenerator =
    Box<dyn FnMut(RegionCoords) -> Vec<(ColumnCoords, Column)> + Send>;
```

Add `Column, ColumnCoords` to the crate-root imports used by `world_manager.rs`. At the seed-construction site (~line 378):

```rust
let columns = (self.generator)(coords); // now Vec<(ColumnCoords, Column)>
match self.parked.remove(&coords) {
    Some(serialized) => RegionSeed::Parked(serialized, columns),
    None => RegionSeed::Fresh(columns),
}
```

Rename the local from `chunks` to `columns` there for clarity.

In `crates/game/src/region_runner.rs`:

```rust
pub enum RegionSeed {
    Fresh(Vec<(ColumnCoords, Column)>),
    Parked(SerializedRegion, Vec<(ColumnCoords, Column)>),
}

impl RegionSeed {
    pub fn into_region(self, id: RegionId) -> Region {
        match self {
            RegionSeed::Fresh(columns) => Region::from_columns(id, columns),
            RegionSeed::Parked(serialized, fallback) => match serialized.to_rollback() {
                Ok(rollback) => Region::from_rollback(id, rollback),
                Err(_) => Region::from_columns(id, fallback),
            },
        }
    }
}
```

(Match the existing `Parked` restore arm; only the fallback constructor changes from `from_chunks` to `from_columns`. If the `Ok` arm currently uses a different constructor name, leave it exactly as-is.)

Add `Column, ColumnCoords` to `region_runner.rs` imports.

- [ ] **Step 5: Fix the compile-follow call sites**

The generator closures already just call `generate_region`, so their bodies are unchanged — only the closure's inferred return type updates. Confirm each still compiles:

- `crates/server/src/lib.rs:238` — `Box::new(move |rc| worldgen::generate_region(rc, stone))`
- `crates/client/src/local_server.rs:38` — same shape
- `crates/server/tests/threaded_world.rs:47` — same shape

Run: `cargo build --workspace --bins 2>&1 | tail -30`
Expected: compiles. Fix any remaining site that named `Vec<(ChunkCoords, Chunk)>` explicitly by switching it to `Vec<(ColumnCoords, Column)>`.

- [ ] **Step 6: Run worldgen + game tests**

Run: `cargo test -p worldgen 2>&1 | tail -20`
Expected: PASS.

Run: `cargo test -p game 2>&1 | tail -20`
Expected: PASS (existing `from_chunks` tests unaffected; new column tests pass).

- [ ] **Step 7: Commit**

```bash
git add crates/worldgen/src/lib.rs crates/game/src/world_manager.rs crates/game/src/region_runner.rs
git commit -m "feat: worldgen produces vertical columns; thread seed seam

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Full-workspace regression + manual verification

**Files:**
- Touch: `justfile` only if a command changed (none expected).

**Interfaces:** none produced; this task proves the foundation is intact.

- [ ] **Step 1: Build everything**

Run: `just build` (or `cargo build --workspace --bins`)
Expected: clean build.

- [ ] **Step 2: Run the game rollback suite**

Run: `cargo test -p game 2>&1 | tail -30`
Expected: PASS — including the rollback/hash-restore invariants (`hash(before) == hash(after undo)`), confirming the column/`ChunkCoords` changes didn't perturb determinism.

- [ ] **Step 3: Run the client headless suite**

Run: `just test-client` (or `cargo test -p client`)
Expected: PASS (26 tests). Meshing/bridge unchanged; the flattened floor sub-chunk meshes as before.

- [ ] **Step 4: Run the server test**

Run: `cargo test -p server 2>&1 | tail -20`
Expected: PASS — `threaded_world` seeds via the new column generator and regions tick.

- [ ] **Step 5: Manual verify (visual)**

Invoke the `verify` skill / `just run` to start server + one client (per the one-client testing convention). Confirm: the world still renders the parity-checkerboard floor with visible region steps, the player stands on the floor, and no chunk is missing or misplaced vertically. A `y=0`-only world should look identical to before this change — the win is structural, not visual.

- [ ] **Step 6: Clippy**

Run: `just clippy 2>&1 | tail -30`
Expected: no new warnings in `game`/`worldgen`.

- [ ] **Step 7: Final commit (if any lint/cleanup)**

```bash
git add -A
git commit -m "chore: vertical chunk stacking foundation — regression pass

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review (author checklist — completed)

- **Spec coverage:** Column/SubChunk/sentinels/unbounded → Tasks 2-3; `ReadVol`/`WriteVol` → Tasks 2-3; air-only collapse + canonical `normalize` + determinism → Task 3; signed `ChunkCoords.y` → Task 1; worldgen columns → Task 5; physics/collider per Dense sub-chunk (no collider for all-stone/sentinel) → inherited unchanged via `from_columns`→`from_chunks`→`create_mesh` (Task 4/6); parking (spec §8) → **deviation noted**: unchanged, `SerializedRegion` already round-trips materialized chunks; mesher genericization (spec §5/§7) → **deviation noted**: mesher stays voxel-based. Non-goals (gravity, vertical crossing, terrain, digging) respected.
- **Placeholder scan:** none — every code step shows complete code and exact commands.
- **Type consistency:** `Column::new`, `normalize`, `materialized_sub_chunks`, `from_columns`, `RegionGenerator`, `RegionSeed` variants, `ColumnCoords`/`ChunkCoords::new(usize, i32, usize)` are consistent across Tasks 1-5.
- **Ambiguity:** block-vs-voxel granularity resolved (block); sub-Y frame (`div_euclid`/`rem_euclid` by `CHUNK_BLOCKS`) stated in Task 3.
