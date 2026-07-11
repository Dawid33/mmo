# Server-Streamed LOD Rings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the client-generated worldgen LOD rings (factors 4/16) and the full-chunk `FarSnapshot` far ring (factor 2) with one server-streamed LOD pipeline, extended with two new levels (factor 64 @ Chebyshev bound 16, factor 256 @ bound 32).

**Architecture:** Client pulls with `RequestLod/ReleaseLod(LodKey)`; the `WorldManager` builds LOD cells on cache miss (downsample authoritative columns when a region has state; full generate+downsample for pristine factor-2/4/16 regions; cheap `worldgen::coarse_tile` point-sampling for pristine 64/256 tiles, overlaid with any authoritative regions), caches them RLE-encoded, and re-pushes when a running region's terrain hash changes. Keys are per-region for factors ≤ 16 and per-tile (2×2 / 8×8 regions = exactly one 32³ grid) for 64/256. Spec: `docs/superpowers/specs/2026-07-10-streamed-lod-rings-design.md`.

**Tech Stack:** Rust workspace (`game` 2021, `worldgen` 2024, `server`, `client`/Bevy 0.18), bincode wire, crossbeam channels, crc32fast hashing.

## Global Constraints

- `game` and `server` stay Bevy-free and windowing-free; `worldgen` depends on `game`, never vice versa.
- LOD grids are always 32³ `Vec<Voxel>` in `x + 32*y + 32*32*z` order (the mesher contract).
- Every constant introduced is a named `pub const` (user preference: tunables are named constants).
- Determinism: `coarse_tile` must be a pure function of `(key, seed, mats)` — no HashMap iteration order, no floats compared across machines except through the existing `ProfileGrid` path.
- Wire serialization is bincode; malformed/hostile packet content must log-and-drop, never panic the manager.
- Run `just test` after every task; tag any new test > 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- Commit after every task (small, per-task commits on `develop`).
- Every task must leave the whole workspace compiling and `just test` green — the old far-ring and client-worldgen paths keep working until Task 12 removes them.

## File Structure (end state)

- `crates/game/src/lod.rs` — NEW: `LodCell`, downsamplers (moved from client), `LodKey` + tile math, ring table + `ring_factor_in` (moved from client), `PackedLodGrid` RLE, `downsample_chunks`, `copy_region_cells_into_tile`.
- `crates/game/src/lod_builder.rs` — NEW: `LodBuildJob`, `LodBuilder` trait, `InlineLodBuilder`, the pure `build_lod_job` function both builders call.
- `crates/game/src/protocol.rs` — new packets; Task 12 removes far packets, renames `FarSnapshot` → `TerrainSnapshot`.
- `crates/game/src/world_manager.rs` — LOD subscriptions/cache/pending/invalidation.
- `crates/game/src/region_runner.rs` — Task 12 renames only.
- `crates/worldgen/src/lib.rs` — `coarse_tile` + shared `surface_from_profile`.
- `crates/server/src/lod_workers.rs` — NEW: `ThreadLodBuilder` (2-thread pool).
- `crates/server/src/lib.rs` — builder wiring + select-loop arm.
- `crates/client/src/instance.rs` — `update_lod_window` (replaces `update_far_window` at Task 12), `LodData` routing.
- `crates/client/src/renderer/lod_rings.rs` — NEW (replaces `worldgen_rings.rs`): network-fed roots + retirement.
- `crates/client/src/renderer/meshing/lod.rs` — meshing-only after Task 1 (downsamplers move out).
- `crates/client/src/renderer/bridge.rs` — far path deleted at Task 12; `LodData`/`RemoveLod` routing added.
- Tests: `crates/game/tests/lod_streaming.rs` NEW; existing `world_manager.rs`, `far_snapshot.rs`, client headless suites updated.

---

### Task 1: Move `LodCell` + downsamplers into `game::lod`

**Files:**
- Create: `crates/game/src/lod.rs`
- Modify: `crates/game/src/lib.rs` (add `pub mod lod;` + `pub use lod::*;`)
- Modify: `crates/client/src/renderer/meshing/lod.rs` (remove moved items, re-import)
- Modify: `crates/client/src/renderer/worldgen_rings.rs` (remove moved items, re-import)
- Modify: `crates/client/src/renderer/bridge.rs` (import fix)

**Interfaces:**
- Produces (all in `game::lod`, re-exported at `game::` root):
  - `pub struct LodCell { pub group: [i32; 3], pub grid: Vec<Voxel> }`
  - `pub fn downsample2(sources: &[Option<&[Voxel]>; 8]) -> Vec<Voxel>`
  - `pub fn build_far_lod_cells(chunks: Vec<(ChunkCoords, Chunk)>) -> Vec<LodCell>`
  - `pub fn downsample_cells(cells: Vec<LodCell>) -> Vec<LodCell>`
  - `pub fn block_scale_cells(chunks: Vec<(ChunkCoords, Chunk)>) -> Vec<LodCell>`
  - `pub fn downsample_blocks(chunks: &[Option<&Chunk>; 8]) -> Vec<Voxel>` (test-reference impl, moves for cohesion)

- [x] **Step 1: Create `crates/game/src/lod.rs` by moving code verbatim**

Move these items, unchanged except for import paths (`game::X` → `crate::X`):
- From `crates/client/src/renderer/meshing/lod.rs`: `downsample2` (lines ~22–105), `downsample_blocks` (~107–149), `LodCell` (~225–228), `build_far_lod_cells` (~237–264).
- From `crates/client/src/renderer/worldgen_rings.rs`: `downsample_cells` (~518–539), `block_scale_cells` (~541–578).

Module header:

```rust
//! LOD cell data model + downsampling, shared by the server (builds streamed
//! LOD) and the client (renders it). Pure data code — no Bevy, no I/O.
//! **Layout contract:** LOD grids are always 32³ `Vec<Voxel>` in
//! `x + 32*y + 32*32*z` order.

use std::collections::BTreeMap;

use crate::{derive_voxels, BlockId, Chunk, ChunkCoords, Voxel, CHUNK_VOXEL_COUNT};
```

Note: `build_far_lod_cells` calls `game::derive_voxels` and `game::Chunk` today — inside `game` these become `crate::` paths. `block_scale_cells` uses `Chunk::block_at` and `BlockId::AIR` — already in `game`.

- [x] **Step 2: Move the pure tests**

Into `#[cfg(test)] mod tests` at the bottom of `crates/game/src/lod.rs`, moved verbatim with import fixes:
- From `meshing/lod.rs` tests: `grid_with`, `empty_sources_downsample_to_air`, `any_solid_source_voxel_makes_the_coarse_cell_solid`, `octant_placement_maps_source_grids_to_coarse_sixteenths`, `majority_material_wins_ties_to_lowest`, `downsample_is_deterministic`, `build_far_lod_cells_groups_signed_y_correctly`, `blocks_fast_path_maps_blocks_to_cells`.
- From `worldgen_rings.rs` tests: `chunk_with_block`, `factor4_two_pass_places_cells_at_world_voxel_over_4`, `block_scale_cells_pack_the_region_into_one_grid_per_tile_and_band`.

- [x] **Step 3: Wire into `game` and re-import in the client**

In `crates/game/src/lib.rs`, after `pub mod input;` add `pub mod lod;`, and in the re-export block add `pub use lod::*;`.

In `crates/client/src/renderer/meshing/lod.rs`: delete the moved items and their tests; add `pub use game::lod::{build_far_lod_cells, downsample2, LodCell};` so existing `lod::LodCell` / `lod::build_far_lod_cells` call sites (bridge.rs, worldgen_rings.rs) keep compiling unchanged.

In `crates/client/src/renderer/worldgen_rings.rs`: delete the moved functions + their two tests; add `use game::lod::{block_scale_cells, downsample_cells};` and delete the now-unused local definitions' imports if any become unused.

- [x] **Step 4: Run tests**

Run: `cargo test -p game lod && cargo test -p client` — expect all green (moved tests pass in `game`; client compiles against re-exports).

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor(lod): move LodCell + downsamplers from client renderer into game::lod"
```

---

### Task 2: `LodKey`, tile math, ring table (in `game::lod`)

**Files:**
- Modify: `crates/game/src/lod.rs`
- Modify: `crates/client/src/renderer/worldgen_rings.rs` (re-import `ring_factor_in`)

**Interfaces:**
- Produces:
  - `pub const LOD_FACTORS: [u32; 5] = [2, 4, 16, 64, 256];`
  - `pub const LOD_RING_TABLE: &[(i32, u32)] = &[(2, 2), (4, 4), (8, 16), (16, 64), (32, 256)];`
  - `pub const LOD_STICKY_BAND: i32 = 1;`
  - `pub fn tile_regions(factor: u32) -> i32` — 1 for factors ≤ 32, else `factor/32` (2 at 64, 8 at 256)
  - `pub struct LodKey { pub factor: u32, pub coords: [i32; 2] }` (+ `Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize`)
  - `LodKey::for_region(factor: u32, rc: RegionCoords) -> LodKey`
  - `LodKey::region_span(&self) -> i32`, `LodKey::origin_region(&self) -> RegionCoords`, `LodKey::regions(&self) -> Vec<RegionCoords>`, `LodKey::world_offset(&self) -> [f32; 3]`
  - `pub fn ring_factor_in(table: &[(i32, u32)], band: i32, dist: i32, current: Option<u32>) -> Option<u32>` (moved from client)

- [x] **Step 1: Write failing tests** (append to `lod.rs` tests)

```rust
#[test]
fn lod_key_region_to_tile_mapping_handles_negatives() {
    use crate::RegionCoords;
    // Factors <= 16: key coords ARE the region coords.
    assert_eq!(LodKey::for_region(4, RegionCoords::new(-3, 7)).coords, [-3, 7]);
    assert_eq!(tile_regions(2), 1);
    assert_eq!(tile_regions(16), 1);
    // Factor 64: 2x2-region tiles, div_euclid so negatives round toward -inf.
    assert_eq!(tile_regions(64), 2);
    assert_eq!(LodKey::for_region(64, RegionCoords::new(-1, 3)).coords, [-1, 1]);
    // Factor 256: 8x8-region tiles.
    assert_eq!(tile_regions(256), 8);
    assert_eq!(LodKey::for_region(256, RegionCoords::new(-9, 15)).coords, [-2, 1]);
}

#[test]
fn lod_key_regions_and_world_offset() {
    use crate::{RegionCoords, REGION_SIZE};
    let key = LodKey { factor: 64, coords: [-1, 1] };
    assert_eq!(key.origin_region(), RegionCoords::new(-2, 2));
    let regions = key.regions();
    assert_eq!(regions.len(), 4);
    assert!(regions.contains(&RegionCoords::new(-2, 2)));
    assert!(regions.contains(&RegionCoords::new(-1, 3)));
    assert_eq!(key.world_offset(), [-2.0 * REGION_SIZE, 0.0, 2.0 * REGION_SIZE]);
    // A tile grid spans exactly 32 cells * factor world units = span regions.
    assert_eq!(32 * 64, 2 * REGION_SIZE as i32);
    assert_eq!(32 * 256, 8 * REGION_SIZE as i32);
    // Region-keyed factor: world offset == the region's own offset.
    let rk = LodKey::for_region(4, RegionCoords::new(3, -2));
    assert_eq!(rk.world_offset(), RegionCoords::new(3, -2).world_offset());
}

#[test]
fn ring_table_is_ascending_and_factors_valid() {
    let mut prev = 0;
    for &(bound, factor) in LOD_RING_TABLE {
        assert!(bound > prev, "bounds ascend");
        prev = bound;
        assert!(LOD_FACTORS.contains(&factor));
    }
}
```

Also move the four `ring_factor*` tests from `worldgen_rings.rs` (`ring_factor_fresh_assignment_over_all_distances`, `..._sticky_band_holds_factor_4_...`, `..._holds_factor_16_...`, `..._unknown_current_falls_back_to_fresh`) into `lod.rs` — they pin the OLD 2-entry table, so rewrite their expectations against `LOD_RING_TABLE` (fresh assignment: d 0..=2 → 2, 3..=4 → 4, 5..=8 → 16, 9..=16 → 64, 17..=32 → 256, else None; sticky variants extend each held ring by 1 on both edges exactly as the old tests did for 4/16).

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p game lod` — expect compile FAIL (`LodKey` not defined).

- [x] **Step 3: Implement**

```rust
use crate::{RegionCoords, REGION_SIZE};

/// Every streamable LOD factor. The server validates request factors
/// against this list; `downsample_chunks` supports exactly these.
pub const LOD_FACTORS: [u32; 5] = [2, 4, 16, 64, 256];

/// Ring table: `(chebyshev_bound, lod_factor)` with ascending bounds. A
/// region at Chebyshev distance `d` from the viewer region renders at the
/// factor of the first entry with `d <= bound`; None past the last bound.
pub const LOD_RING_TABLE: &[(i32, u32)] = &[(2, 2), (4, 4), (8, 16), (16, 64), (32, 256)];

/// Hysteresis band for `ring_factor_in` (see that function).
pub const LOD_STICKY_BAND: i32 = 1;

/// Tile edge length in regions for a factor's `LodKey`: factors whose 32³
/// grid fits inside one region key per-region; 64/256 key per-tile so one
/// key is exactly one grid footprint (32 * factor == span * REGION_SIZE).
pub fn tile_regions(factor: u32) -> i32 {
    ((32 * factor) as i32 / REGION_SIZE as i32).max(1)
}

/// Names one streamable LOD unit: a region (factor <= 16) or a tile of
/// `tile_regions(factor)`² regions (64/256). `coords` are region coords
/// divided (euclid) by the tile span, so they're plain region coords for
/// the small factors.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
    serde::Serialize, serde::Deserialize,
)]
pub struct LodKey {
    pub factor: u32,
    pub coords: [i32; 2],
}

impl LodKey {
    pub fn for_region(factor: u32, rc: RegionCoords) -> Self {
        let span = tile_regions(factor);
        Self { factor, coords: [rc.x.div_euclid(span), rc.z.div_euclid(span)] }
    }

    /// Tile edge in regions (1 for factors <= 16).
    pub fn region_span(&self) -> i32 {
        tile_regions(self.factor)
    }

    /// The minimum-corner region this key covers.
    pub fn origin_region(&self) -> RegionCoords {
        let span = self.region_span();
        RegionCoords::new(self.coords[0] * span, self.coords[1] * span)
    }

    /// Every region this key covers (span² of them).
    pub fn regions(&self) -> Vec<RegionCoords> {
        let o = self.origin_region();
        let span = self.region_span();
        let mut out = Vec::with_capacity((span * span) as usize);
        for dx in 0..span {
            for dz in 0..span {
                out.push(RegionCoords::new(o.x + dx, o.z + dz));
            }
        }
        out
    }

    /// World-space origin of this key's render root.
    pub fn world_offset(&self) -> [f32; 3] {
        self.origin_region().world_offset()
    }
}
```

Then MOVE `ring_factor_in` (with its doc comment) verbatim from `crates/client/src/renderer/worldgen_rings.rs:75-90` into `lod.rs`, and in `worldgen_rings.rs` replace the definition with `use game::lod::ring_factor_in;` (the `ring_factor` test-face wrapper there keeps working; it still pins the old client table until Task 11 deletes it).

- [x] **Step 4: Run tests**

Run: `cargo test -p game lod && cargo test -p client` — expect PASS.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(lod): LodKey + tile math + shared ring table in game::lod"
```

---

### Task 3: `PackedLodGrid` RLE wire encoding

**Files:**
- Modify: `crates/game/src/lod.rs`

**Interfaces:**
- Produces:
  - `pub struct PackedLodGrid(pub Vec<(u16, BlockId)>)` (+ `Debug, Clone, PartialEq, Serialize, Deserialize`)
  - `PackedLodGrid::encode(grid: &[Voxel]) -> PackedLodGrid`
  - `PackedLodGrid::decode(&self) -> Option<Vec<Voxel>>` — `None` unless runs sum to exactly `CHUNK_VOXEL_COUNT` (hostile/corrupt data drops, never panics)

- [x] **Step 1: Write failing tests**

```rust
#[test]
fn packed_grid_round_trips() {
    // Mixed content incl. a lone voxel at the very end.
    let mut grid = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
    for i in 0..5000 {
        grid[i] = Voxel::new(BlockId(1));
    }
    grid[9000] = Voxel::new(BlockId(7));
    grid[CHUNK_VOXEL_COUNT - 1] = Voxel::new(BlockId(3));
    let packed = PackedLodGrid::encode(&grid);
    assert_eq!(packed.decode().expect("valid"), grid);
    // All-air compresses to one run.
    let air = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
    assert_eq!(PackedLodGrid::encode(&air).0.len(), 1);
    assert_eq!(PackedLodGrid::encode(&air).decode().unwrap(), air);
    // Worst case: alternating ids still round-trips (u16 runs of 1).
    let mut alt = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
    for (i, v) in alt.iter_mut().enumerate() {
        *v = Voxel::new(BlockId((i % 2) as u16));
    }
    assert_eq!(PackedLodGrid::encode(&alt).decode().unwrap(), alt);
}

#[test]
fn packed_grid_rejects_wrong_total() {
    let short = PackedLodGrid(vec![(100, BlockId(1))]);
    assert!(short.decode().is_none(), "runs must sum to CHUNK_VOXEL_COUNT");
    let long = PackedLodGrid(vec![(u16::MAX, BlockId(0)), (u16::MAX, BlockId(0))]);
    assert!(long.decode().is_none());
}
```

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p game packed_grid` — expect compile FAIL.

- [x] **Step 3: Implement**

```rust
/// Run-length encoding of one 32³ LOD grid in the standard layout order.
/// Terrain grids are dominated by air/uniform-rock runs: ~64 KB raw
/// typically packs to ~1 KB. Runs are capped at u16::MAX (< 32768 total
/// cells, so any grid fits in <= CHUNK_VOXEL_COUNT runs).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PackedLodGrid(pub Vec<(u16, BlockId)>);

impl PackedLodGrid {
    pub fn encode(grid: &[Voxel]) -> Self {
        debug_assert_eq!(grid.len(), CHUNK_VOXEL_COUNT);
        let mut runs: Vec<(u16, BlockId)> = Vec::new();
        for v in grid {
            match runs.last_mut() {
                Some((count, block)) if *block == v.block && *count < u16::MAX => *count += 1,
                _ => runs.push((1, v.block)),
            }
        }
        Self(runs)
    }

    /// None when the run total is not exactly one grid — wire data is
    /// untrusted, so corrupt payloads drop instead of panicking.
    pub fn decode(&self) -> Option<Vec<Voxel>> {
        let total: usize = self.0.iter().map(|&(n, _)| n as usize).sum();
        if total != CHUNK_VOXEL_COUNT {
            return None;
        }
        let mut grid = Vec::with_capacity(CHUNK_VOXEL_COUNT);
        for &(n, block) in &self.0 {
            for _ in 0..n {
                grid.push(Voxel::new(block));
            }
        }
        Some(grid)
    }
}
```

Note: `Voxel::new(BlockId::AIR)` must equal `Voxel::default()` — check `crates/game/src/voxel.rs`; if `Voxel` carries more state than `block`, encode per-field or assert equality in a test. (Today `Voxel` is the block id; the round-trip test catches any drift.)

- [x] **Step 4: Run tests**

Run: `cargo test -p game packed_grid` — expect PASS.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(lod): PackedLodGrid RLE wire encoding with hostile-input-safe decode"
```

---

### Task 4: `downsample_chunks` dispatch + `copy_region_cells_into_tile`

**Files:**
- Modify: `crates/game/src/lod.rs`

**Interfaces:**
- Consumes: Task 1 downsamplers, Task 2 `LodKey`.
- Produces:
  - `pub fn downsample_chunks(chunks: Vec<(ChunkCoords, Chunk)>, factor: u32) -> Vec<LodCell>` — region-local input coords → key-local cells for any factor in `LOD_FACTORS`. For 64/256 the region occupies the low-corner x/z rect of its grids (1024/factor cells).
  - `pub fn cells_per_region(factor: u32) -> usize` — 1024/factor for coarse factors (16 at 64, 4 at 256).
  - `pub fn copy_region_cells_into_tile(tile: &mut BTreeMap<[i32; 3], Vec<Voxel>>, region_cells: &[LodCell], region: RegionCoords, key: &LodKey)` — clears the region's x/z cell rect in EVERY tile band, then writes the region's cells (authoritative overlay; clearing all bands makes edits that removed terrain win over coarse guesses).

- [x] **Step 1: Write failing tests**

```rust
#[test]
fn downsample_chunks_dispatches_every_factor() {
    // One solid block at chunk (0,0,0), block (0,0,0): world voxels 0..16 on
    // each axis. Expected occupancy per factor: f cells = 16/f per axis
    // (min 1), all in the grid at group [0,0,0].
    for &(factor, expect_per_axis) in
        &[(2u32, 8usize), (4, 4), (16, 1), (64, 1), (256, 1)]
    {
        let chunks = vec![(ChunkCoords::new(0, 0, 0), chunk_with_block([0, 0, 0], 5))];
        let cells = downsample_chunks(chunks, factor);
        assert_eq!(cells.len(), 1, "factor {factor}: one populated grid");
        assert_eq!(cells[0].group, [0, 0, 0]);
        let n = cells[0].grid.iter().filter(|v| !v.is_air()).count();
        assert_eq!(n, expect_per_axis.pow(3), "factor {factor}");
        assert_eq!(cells[0].grid[0].block, BlockId(5), "cell (0,0,0) solid");
    }
}

#[test]
fn downsample_chunks_places_region_content_in_the_low_corner_rect() {
    // Chunk (31, 0, 31) is the region's far corner. At factor 64 the region
    // spans cells 0..16, so that chunk's block lands at cell (15, 0, 15).
    let chunks = vec![(ChunkCoords::new(31, 0, 31), chunk_with_block([1, 0, 1], 9))];
    let cells = downsample_chunks(chunks, 64);
    assert_eq!(cells.len(), 1);
    let idx = 15 + 32 * 0 + 32 * 32 * 15;
    assert_eq!(cells[0].grid[idx].block, BlockId(9));
    assert_eq!(cells_per_region(64), 16);
    assert_eq!(cells_per_region(256), 4);
}

#[test]
fn copy_region_cells_overlays_and_clears() {
    use crate::RegionCoords;
    let key = LodKey { factor: 64, coords: [0, 0] }; // covers regions (0..2, 0..2)
    // Coarse tile band 0: fully solid everywhere.
    let mut tile: BTreeMap<[i32; 3], Vec<Voxel>> = BTreeMap::new();
    tile.insert([0, 0, 0], vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT]);
    // Region (1, 0)'s authoritative cells: a single solid cell at its local (0,0,0).
    let mut grid = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
    grid[0] = Voxel::new(BlockId(7));
    let region_cells = vec![LodCell { group: [0, 0, 0], grid }];
    copy_region_cells_into_tile(&mut tile, &region_cells, RegionCoords::new(1, 0), &key);

    let band = &tile[&[0, 0, 0]];
    // Region (1,0) maps to x cells 16..32, z 0..16: rect cleared, one cell rewritten.
    assert_eq!(band[16].block, BlockId(7), "authoritative cell written at x offset 16");
    assert!(band[17].is_air(), "rest of the region rect cleared to authoritative air");
    assert!(band[16 + 32 * 32 * 15].is_air(), "cleared across the whole z extent");
    assert_eq!(band[0].block, BlockId(1), "outside the region rect untouched");
    assert_eq!(band[32 * 32 * 16].block, BlockId(1), "z >= 16 (other region row) untouched");
}
```

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p game downsample_chunks` — expect compile FAIL.

- [x] **Step 3: Implement**

```rust
/// x/z cells one region occupies in a grid at `factor` (16 at 64, 4 at 256;
/// 32 or more for the small factors, where a region spans whole grids).
pub fn cells_per_region(factor: u32) -> usize {
    (REGION_SIZE as u32 / factor) as usize
}

/// Downsample one region's chunks (region-local x/z chunk coords, absolute
/// signed y) to `factor` LOD cells with key-local group coords. Factors
/// follow the composition: 2 = voxel 2x2x2 merge; 4 = one more merge pass;
/// 16 = block scale; 64/256 = block scale + 2/4 more merge passes (each
/// pass halves the group lattice, so groups stay consistent throughout).
pub fn downsample_chunks(chunks: Vec<(ChunkCoords, Chunk)>, factor: u32) -> Vec<LodCell> {
    match factor {
        2 => build_far_lod_cells(chunks),
        4 => downsample_cells(build_far_lod_cells(chunks)),
        16 => block_scale_cells(chunks),
        64 => downsample_cells(downsample_cells(block_scale_cells(chunks))),
        256 => downsample_cells(downsample_cells(downsample_cells(downsample_cells(
            block_scale_cells(chunks),
        )))),
        _ => panic!("unsupported LOD factor {factor} (must be one of {LOD_FACTORS:?})"),
    }
}

/// Overlay one region's authoritative cells into a tile's grids: clear the
/// region's x/z cell rect in EVERY band (so terrain the players removed
/// doesn't survive as a coarse-sampled ghost), then write the region's
/// cells. `region_cells` must come from `downsample_chunks(..., key.factor)`
/// — content in the low-corner rect, absolute band indices in `group[1]`.
pub fn copy_region_cells_into_tile(
    tile: &mut BTreeMap<[i32; 3], Vec<Voxel>>,
    region_cells: &[LodCell],
    region: RegionCoords,
    key: &LodKey,
) {
    let per = cells_per_region(key.factor);
    let origin = key.origin_region();
    let x_off = ((region.x - origin.x) as usize) * per;
    let z_off = ((region.z - origin.z) as usize) * per;
    debug_assert!(x_off + per <= 32 && z_off + per <= 32, "region outside tile");

    for grid in tile.values_mut() {
        for lz in 0..per {
            for ly in 0..32 {
                for lx in 0..per {
                    grid[(x_off + lx) + 32 * ly + 32 * 32 * (z_off + lz)] = Voxel::default();
                }
            }
        }
    }
    for cell in region_cells {
        debug_assert_eq!((cell.group[0], cell.group[2]), (0, 0), "region cells are low-corner");
        let grid = tile
            .entry([0, cell.group[1], 0])
            .or_insert_with(|| vec![Voxel::default(); CHUNK_VOXEL_COUNT]);
        for lz in 0..per {
            for ly in 0..32 {
                for lx in 0..per {
                    grid[(x_off + lx) + 32 * ly + 32 * 32 * (z_off + lz)] =
                        cell.grid[lx + 32 * ly + 32 * 32 * lz];
                }
            }
        }
    }
}
```

- [x] **Step 4: Run tests**

Run: `cargo test -p game lod` — expect PASS (including the moved Task 1 tests).

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(lod): downsample_chunks factor dispatch + tile overlay copy"
```

---

### Task 5: Protocol packets + `ClientUpdateEvent` variants (additive)

**Files:**
- Modify: `crates/game/src/protocol.rs`
- Modify: `crates/game/src/lib.rs` (`ClientUpdateEvent`)
- Modify: `crates/client/src/renderer/bridge.rs` (exhaustive-match stub arms)
- Modify: `crates/game/src/world_manager.rs` (packet match stub arms)

**Interfaces:**
- Produces:
  - `ClientPacket::RequestLod(LodKey)`, `ClientPacket::ReleaseLod(LodKey)`
  - `pub type LodPayload = Vec<([i32; 3], crate::lod::PackedLodGrid)>;` (in protocol.rs)
  - `ServerPacket::LodData(LodKey, LodPayload)`
  - `ClientUpdateEvent::LodData(LodKey, LodPayload)`, `ClientUpdateEvent::RemoveLod(LodKey)`

- [x] **Step 1: Write failing test** (in `crates/game/src/protocol.rs` tests, or a new `#[cfg(test)] mod` if none exists)

```rust
#[test]
fn lod_packets_round_trip_bincode() {
    use crate::lod::{LodKey, PackedLodGrid};
    let key = LodKey { factor: 64, coords: [-1, 2] };
    let req = ClientPacket::RequestLod(key);
    let bytes = bincode::serialize(&req).unwrap();
    assert!(matches!(
        bincode::deserialize::<ClientPacket>(&bytes).unwrap(),
        ClientPacket::RequestLod(k) if k == key
    ));
    let payload: LodPayload = vec![([0, -1, 0], PackedLodGrid(vec![(100, crate::BlockId(2))]))];
    let data = ServerPacket::LodData(key, payload.clone());
    let bytes = bincode::serialize(&data).unwrap();
    match bincode::deserialize::<ServerPacket>(&bytes).unwrap() {
        ServerPacket::LodData(k, p) => {
            assert_eq!(k, key);
            assert_eq!(p, payload);
        }
        other => panic!("wrong variant {other:?}"),
    }
}
```

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p game lod_packets` — expect compile FAIL.

- [x] **Step 3: Implement**

In `protocol.rs`: add `use crate::lod::LodKey;`, then

```rust
/// One LOD response: (key-local cell group, RLE grid) pairs.
pub type LodPayload = Vec<([i32; 3], crate::lod::PackedLodGrid)>;
```

Extend `ClientPacket` (below `ReleaseFarRegion`):

```rust
    /// Subscribe to one LOD key: the server replies with `LodData` (from
    /// cache or after building) and re-pushes on terrain change.
    RequestLod(LodKey),
    /// Unsubscribe; removal is client-driven, there is no "gone" packet.
    ReleaseLod(LodKey),
```

Extend `ServerPacket` (below `FarRegion`):

```rust
    /// Streamed LOD cells for one key (initial send AND refresh —
    /// replace-on-re-receipt).
    LodData(LodKey, LodPayload),
```

Extend `ClientUpdateEvent` in `lib.rs` (below `RemoveFarRegion`):

```rust
    /// Streamed LOD cells for one key (initial send or refresh —
    /// replace-on-re-receipt). Decoded and spawned by the renderer.
    LodData(LodKey, LodPayload),
    /// The ring assignment moved on: retire this key's render root.
    RemoveLod(LodKey),
```

Stub arms so exhaustive matches compile (replaced in later tasks):
- `crates/game/src/world_manager.rs` `handle_server_event` packet match:
  `ClientPacket::RequestLod(_) | ClientPacket::ReleaseLod(_) => {} // Task 7`
- `crates/client/src/renderer/bridge.rs` `drain_client_updates` match:
  `ClientUpdateEvent::LodData(..) | ClientUpdateEvent::RemoveLod(_) => {} // Task 11`

- [x] **Step 4: Run tests**

Run: `cargo test -p game && cargo test -p client` — expect PASS.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(protocol): RequestLod/ReleaseLod/LodData packets + client update variants"
```

---

### Task 6: `worldgen::coarse_tile`

**Files:**
- Modify: `crates/worldgen/src/lib.rs`

**Interfaces:**
- Consumes: `game::lod::{LodKey, LodCell}`, `ProfileGrid::profile_at`, `Materials::material_at`, existing constants `SCAN_TOP`, `SCAN_BOTTOM`, `MAX_SOIL_DEPTH`, `REGION_BLOCKS`, `SEA_LEVEL`.
- Produces:
  - `pub fn coarse_tile(key: &game::lod::LodKey, seed: u64, mats: Materials) -> Vec<game::lod::LodCell>` — pure/deterministic; key-local groups `[0, band, 0]`; only valid for factors 64/256.
  - Internal refactor: `fn surface_from_profile(prof: &ColumnProfile) -> i32` extracted from `surface_block_y` and shared.

- [x] **Step 1: Refactor `surface_block_y` (no behavior change)**

In `crates/worldgen/src/lib.rs`, split the scan loop out of `surface_block_y`:

```rust
/// Highest solid block Y for a precomputed profile, or `SCAN_BOTTOM - 1`
/// if the whole column is air. Shared by full-res and coarse generation.
fn surface_from_profile(prof: &crate::pipeline::ColumnProfile) -> i32 {
    let mut by = SCAN_TOP;
    while by >= SCAN_BOTTOM {
        if prof.density_at(block_center_m(by)) > 0.0 {
            return by;
        }
        by -= 1;
    }
    SCAN_BOTTOM - 1
}

fn surface_block_y(grid: &ProfileGrid, wx: i32, wz: i32) -> i32 {
    surface_from_profile(&grid.profile_at(block_center_m(wx), block_center_m(wz)))
}
```

Run: `cargo test -p worldgen` — expect PASS (pure refactor; the bit-exact pins in worldgen must stay green).

- [x] **Step 2: Write failing tests**

```rust
#[test]
fn coarse_tile_is_deterministic_and_key_local() {
    let key = game::lod::LodKey { factor: 64, coords: [0, 0] };
    let a = coarse_tile(&key, WORLD_SEED, mats());
    let b = coarse_tile(&key, WORLD_SEED, mats());
    assert_eq!(a.len(), b.len());
    for (ca, cb) in a.iter().zip(&b) {
        assert_eq!(ca.group, cb.group);
        assert_eq!(ca.grid, cb.grid);
        assert_eq!((ca.group[0], ca.group[2]), (0, 0), "tile-local groups");
    }
    assert!(!a.is_empty(), "sea-level world always materializes at least one band");
    // Sea level (block 64) must be covered: some cell is water or solid.
    assert!(a.iter().any(|c| c.grid.iter().any(|v| !v.is_air())));
}

/// Coarse point-sampling vs exact generate+downsample: per cell column the
/// highest solid cell must match within one cell for the vast majority of
/// columns. This bounds the accepted §3 inexactness; tighten the threshold
/// only with measurement.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn coarse_tile_tracks_downsampled_full_generation() {
    let key = game::lod::LodKey { factor: 64, coords: [0, 0] };
    let coarse = coarse_tile(&key, WORLD_SEED, mats());

    // Exact: generate each covered region, downsample to 64, overlay into a tile.
    let mut exact: std::collections::BTreeMap<[i32; 3], Vec<game::Voxel>> = Default::default();
    for rc in key.regions() {
        let columns = generate_region(rc, WORLD_SEED, mats());
        let chunks: Vec<(game::ChunkCoords, game::Chunk)> = columns
            .into_iter()
            .flat_map(|(cc, col)| {
                col.materialized_sub_chunks()
                    .into_iter()
                    .map(move |(sub_y, ch)| (game::ChunkCoords::new(cc.x, sub_y, cc.z), ch))
            })
            .collect();
        let cells = game::lod::downsample_chunks(chunks, key.factor);
        game::lod::copy_region_cells_into_tile(&mut exact, &cells, rc, &key);
    }

    let top_of = |grids: &std::collections::BTreeMap<[i32; 3], Vec<game::Voxel>>,
                  x: usize,
                  z: usize| {
        grids
            .iter()
            .flat_map(|(g, grid)| {
                (0..32).rev().find_map(|y| {
                    (!grid[x + 32 * y + 32 * 32 * z].is_air())
                        .then(|| g[1] * 32 + y as i32)
                })
            })
            .max()
    };
    let coarse_map: std::collections::BTreeMap<[i32; 3], Vec<game::Voxel>> =
        coarse.iter().map(|c| (c.group, c.grid.clone())).collect();

    let (mut hits, mut total) = (0, 0);
    for x in 0..32 {
        for z in 0..32 {
            let (a, b) = (top_of(&coarse_map, x, z), top_of(&exact, x, z));
            if let (Some(a), Some(b)) = (a, b) {
                total += 1;
                if (a - b).abs() <= 1 {
                    hits += 1;
                }
            }
        }
    }
    assert!(total > 200, "most columns have terrain (total={total})");
    assert!(
        hits * 10 >= total * 9,
        "coarse surface within ±1 cell for >=90% of columns ({hits}/{total})"
    );
}
```

(`mats()` already exists in this test module.) Note `top_of` for `exact` treats water cells as occupancy too — both sides fill water to sea level, so the comparison is fair.

- [x] **Step 3: Run to verify failure**

Run: `cargo test -p worldgen coarse_tile` — expect compile FAIL.

- [x] **Step 4: Implement**

```rust
/// Cheap coarse LOD generation for one 64/256 tile: one profile sample per
/// cell column (32×32), surface scan, then solid-below-surface + water to
/// sea level. No block materialization, no chiseling. Pure and
/// deterministic: same (key, seed, mats) → identical cells everywhere.
/// Point-sampling is NOT bit-consistent with downsampled full generation
/// (accepted, sub-pixel at these distances — see the design spec §3);
/// `coarse_tile_tracks_downsampled_full_generation` bounds the divergence.
pub fn coarse_tile(
    key: &game::lod::LodKey,
    seed: u64,
    mats: Materials,
) -> Vec<game::lod::LodCell> {
    use game::lod::LodCell;
    use game::{Voxel, CHUNK_VOXEL_COUNT};

    // Blocks per cell edge: 4 at factor 64, 16 at factor 256.
    let cb = (key.factor / game::BLOCK_VOXELS as u32) as i32;
    let span_blocks = key.region_span() * REGION_BLOCKS;
    debug_assert_eq!(span_blocks, 32 * cb, "a tile is exactly one 32³ grid");

    let p = TerrainPipeline::new(seed);
    let origin = key.origin_region();
    let ox = origin.x * REGION_BLOCKS;
    let oz = origin.z * REGION_BLOCKS;
    let grid = p.profile_grid(ox as f64, oz as f64, span_blocks as f64);
    let sea = SEA_LEVEL as i32;

    let mut bands: std::collections::BTreeMap<i32, Vec<Voxel>> = Default::default();
    for cz in 0..32i32 {
        for cx in 0..32i32 {
            let mx = (ox + cx * cb) as f64 + cb as f64 * 0.5;
            let mz = (oz + cz * cb) as f64 + cb as f64 * 0.5;
            let prof = grid.profile_at(mx, mz);
            let surface = surface_from_profile(&prof);

            let solid_lo = (surface - MAX_SOIL_DEPTH).div_euclid(cb);
            let solid_hi = surface.div_euclid(cb);
            let top_cell = surface.max(sea).div_euclid(cb);
            for cy in solid_lo..=top_cell {
                let is_solid = cy <= solid_hi;
                // Water only where the cell's bottom block is at/below sea.
                if !is_solid && cy * cb > sea {
                    continue;
                }
                let mat = if is_solid {
                    let mid = cy * cb + cb / 2;
                    mats.material_at((surface - mid).max(0), mid)
                } else {
                    mats.water
                };
                let band = cy.div_euclid(32);
                let ly = cy.rem_euclid(32) as usize;
                let g = bands
                    .entry(band)
                    .or_insert_with(|| vec![Voxel::default(); CHUNK_VOXEL_COUNT]);
                g[cx as usize + 32 * ly + 32 * 32 * cz as usize] = Voxel::new(mat);
            }
        }
    }
    bands
        .into_iter()
        .map(|(band, grid)| LodCell { group: [0, band, 0], grid })
        .collect()
}
```

Check `game::BLOCK_VOXELS` is exported (it's used in `crates/worldgen/src/lib.rs`'s existing imports — extend that `use game::{...}` list). Verify `Materials::material_at(depth: i32, y: i32) -> BlockId` matches the existing call in `generate_region` (`mats.material_at(depth, by)`) and reuse its exact signature.

- [x] **Step 5: Run tests**

Run: `cargo test -p worldgen && cargo test -p worldgen -- --ignored coarse_tile` — expect PASS. If the 90% tolerance fails, print the hit ratio, inspect a handful of mismatching columns for an off-by-one in band/cell math first; only lower the threshold (with a comment) if the residual is genuine point-sampling noise.

- [x] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(worldgen): coarse_tile point-sampled LOD generation for 64/256 tiles"
```

---

### Task 7: `LodBuilder` + WorldManager subscriptions, cache, and region-key serving

**Files:**
- Create: `crates/game/src/lod_builder.rs`
- Modify: `crates/game/src/lib.rs` (`pub mod lod_builder;` + re-export)
- Modify: `crates/game/src/world_manager.rs`
- Create: `crates/game/tests/lod_streaming.rs`

**Interfaces:**
- Consumes: Tasks 1–6.
- Produces (in `game::lod_builder`):
  - `pub enum LodBuildJob { Region { key: LodKey, chunks: Vec<(ChunkCoords, Chunk)> }, Tile { key: LodKey, overlays: Vec<(RegionCoords, Vec<(ChunkCoords, Chunk)>)> } }`
  - `pub type LodResult = (LodKey, LodPayload);`
  - `pub trait LodBuilder: Send { fn submit(&mut self, job: LodBuildJob); }`
  - `pub struct InlineLodBuilder { pub seed: u64, pub mats: Materials, pub out: Sender<LodResult> }` — computes synchronously on `submit` (wasm/tests)
  - `pub fn build_lod_job(job: LodBuildJob, seed: u64, mats: Materials) -> LodResult` — the pure build both builders call
- Produces (WorldManager):
  - `WorldManager::new` gains two params: `lod_builder: Box<dyn LodBuilder>`, `lod_results: Receiver<LodResult>` — callers updated (server `lib.rs`, client `local_server.rs`, `game/tests/world_manager.rs`, `game/tests/handoff_manager.rs`)
  - `pub fn handle_lod_result(&mut self, key: LodKey, payload: LodPayload)` — cache + fan out to subscribers
  - `pub fn pump_lod_results(&mut self)` — drain the receiver (Inline builder path; the server select loop uses the receiver directly)
  - `pub const LOD_CACHE_MAX_ENTRIES: usize = 8192;`

Note: `worldgen` depends on `game`, so `game` cannot call `coarse_tile` directly. `Materials` lives in `worldgen::material` — that would invert the dependency. **Resolution:** `LodBuildJob`/`LodBuilder`/`WorldManager` stay generation-agnostic: `InlineLodBuilder` and `build_lod_job` take a `coarse: Box<dyn Fn(&LodKey) -> Vec<LodCell> + Send>` closure instead of `(seed, mats)`, exactly like the existing `RegionGenerator` closure pattern. Concretely:

```rust
pub type CoarseGenerator = Box<dyn Fn(&LodKey) -> Vec<LodCell> + Send>;
pub fn build_lod_job(job: LodBuildJob, coarse: &dyn Fn(&LodKey) -> Vec<LodCell>) -> LodResult;
pub struct InlineLodBuilder { pub coarse: CoarseGenerator, pub out: Sender<LodResult> }
```

Callers construct it with `Box::new(move |key| worldgen::coarse_tile(key, seed, materials))`.

- [x] **Step 1: Write failing tests** (`crates/game/tests/lod_streaming.rs`)

Test scaffold mirrors `crates/game/tests/world_manager.rs` — read its helper setup first (how it builds a `WorldManager<InlineSpawner>`, its flat-generator closure, and its `pump`/drain pattern) and reuse the same helpers here. Coarse closure for tests: a fake that returns one recognizable all-stone band, so tests don't need worldgen:

```rust
use crossbeam::channel::unbounded;
use game::lod::{LodCell, LodKey, PackedLodGrid};
use game::*;

/// Flat-floor generator matching world_manager.rs's test generator; coarse
/// closure returns one marker band so sources are distinguishable.
fn manager_with_lod() -> (
    WorldManager<InlineSpawner>,
    crossbeam::channel::Receiver<(Option<ClientId>, ServerPacket)>,
    crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>,
) {
    // ... same channel setup as world_manager.rs tests, plus:
    // let (lod_result_send, lod_result_recv) = unbounded();
    // let coarse = Box::new(|key: &LodKey| vec![LodCell { group: [0, 0, 0],
    //     grid: vec![Voxel::new(BlockId(42)); CHUNK_VOXEL_COUNT] }]);
    // let builder = Box::new(InlineLodBuilder { coarse, out: lod_result_send });
    // WorldManager::new(spawner, generator, out, region_out, builder, lod_result_recv)
    todo!("copy world_manager.rs setup + the two new params")
}

#[test]
fn request_lod_pristine_region_builds_downsamples_and_caches() {
    let (mut m, out, region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    drain_region_outputs(&mut m, &region_out); // helper as in world_manager.rs
    let key = LodKey::for_region(4, RegionCoords::new(5, 5)); // never run
    m.handle_server_event(
        ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1),
        0,
    );
    m.pump_lod_results();
    let data: Vec<_> = out
        .try_iter()
        .filter(|(_, p)| matches!(p, ServerPacket::LodData(..)))
        .collect();
    assert_eq!(data.len(), 1, "one LodData reply");
    let (target, ServerPacket::LodData(k, payload)) = &data[0] else { unreachable!() };
    assert_eq!(*target, Some(1));
    assert_eq!(*k, key);
    assert!(!payload.is_empty(), "flat floor produces cells");
    for (_, grid) in payload {
        assert!(grid.decode().is_some(), "well-formed RLE grids");
    }

    // Second identical request: served from cache, no rebuild (the inline
    // builder would have pushed a second result).
    m.handle_server_event(
        ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1),
        0,
    );
    let cached: Vec<_> = out
        .try_iter()
        .filter(|(_, p)| matches!(p, ServerPacket::LodData(..)))
        .collect();
    assert_eq!(cached.len(), 1, "cache hit answers without building");
}

#[test]
fn request_lod_running_region_downsamples_authoritative_state() {
    // Subscribe a region normally (it runs), edit nothing; request factor 4
    // for it. Running + unseeded terrain cache → the manager asks the region
    // (RequestFarSnapshot), the reply seeds far_cache, and the pending LOD
    // request completes.
    // Assert: exactly one LodData for the key arrives after pumping the
    // spawner + region outputs + lod results.
}

#[test]
fn release_and_disconnect_clear_subscriptions() {
    // RequestLod then ReleaseLod: a later terrain change must push nothing
    // (asserted fully in Task 9's invalidation test; here assert the maps
    // via a fresh RequestLod still answering — i.e. no double-send state).
    // Then RequestLod from client 2 and disconnect client 2: no panic, and
    // client 1's later request still works.
}

#[test]
fn invalid_factor_is_dropped() {
    let (mut m, out, _region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    let bogus = LodKey { factor: 3, coords: [0, 0] };
    m.handle_server_event(
        ServerEvent::ClientPacket(ClientPacket::RequestLod(bogus), 1),
        0,
    );
    m.pump_lod_results();
    assert!(
        out.try_iter().all(|(_, p)| !matches!(p, ServerPacket::LodData(..))),
        "bogus factor never builds or panics"
    );
}
```

Flesh out the two sketched tests with the same helper/drain style as the neighbors in `world_manager.rs` — the assertions named in their comments are the required behavior.

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p game --test lod_streaming` — expect compile FAIL.

- [x] **Step 3: Implement `lod_builder.rs`**

```rust
//! LOD build execution. `WorldManager` never blocks on a build: it submits
//! `LodBuildJob`s to a `LodBuilder` and consumes results from a channel.
//! `InlineLodBuilder` computes synchronously (wasm/tests); the native
//! server wraps a small thread pool (crates/server/src/lod_workers.rs).

use std::collections::BTreeMap;

use crossbeam::channel::Sender;

use crate::lod::{
    copy_region_cells_into_tile, downsample_chunks, LodCell, LodKey, PackedLodGrid,
};
use crate::{Chunk, ChunkCoords, LodPayload, RegionCoords, Voxel};

pub type LodResult = (LodKey, LodPayload);
pub type CoarseGenerator = Box<dyn Fn(&LodKey) -> Vec<LodCell> + Send>;

#[derive(Debug)]
pub enum LodBuildJob {
    /// Region-keyed factors (2/4/16): downsample this region's chunks
    /// (authoritative when the region has state, generated otherwise —
    /// the manager resolves the source, the builder just computes).
    Region {
        key: LodKey,
        chunks: Vec<(ChunkCoords, Chunk)>,
    },
    /// Tile-keyed factors (64/256): coarse-generate the tile, then overlay
    /// each region that has authoritative state.
    Tile {
        key: LodKey,
        overlays: Vec<(RegionCoords, Vec<(ChunkCoords, Chunk)>)>,
    },
}

pub trait LodBuilder: Send {
    fn submit(&mut self, job: LodBuildJob);
}

/// The pure build. Both builder impls call this.
pub fn build_lod_job(job: LodBuildJob, coarse: &dyn Fn(&LodKey) -> Vec<LodCell>) -> LodResult {
    match job {
        LodBuildJob::Region { key, chunks } => {
            let cells = downsample_chunks(chunks, key.factor);
            (key, encode_cells(cells))
        }
        LodBuildJob::Tile { key, overlays } => {
            let mut tile: BTreeMap<[i32; 3], Vec<Voxel>> =
                coarse(&key).into_iter().map(|c| (c.group, c.grid)).collect();
            for (rc, chunks) in overlays {
                let cells = downsample_chunks(chunks, key.factor);
                copy_region_cells_into_tile(&mut tile, &cells, rc, &key);
            }
            let cells = tile
                .into_iter()
                .map(|(group, grid)| LodCell { group, grid })
                .collect();
            (key, encode_cells(cells))
        }
    }
}

fn encode_cells(cells: Vec<LodCell>) -> LodPayload {
    cells
        .into_iter()
        // All-air grids render nothing; don't ship them.
        .filter(|c| c.grid.iter().any(|v| !v.is_air()))
        .map(|c| (c.group, PackedLodGrid::encode(&c.grid)))
        .collect()
}

/// Synchronous builder: computes on submit and pushes straight to `out`.
pub struct InlineLodBuilder {
    pub coarse: CoarseGenerator,
    pub out: Sender<LodResult>,
}

impl LodBuilder for InlineLodBuilder {
    fn submit(&mut self, job: LodBuildJob) {
        let _ = self.out.send(build_lod_job(job, &self.coarse));
    }
}
```

Add `pub mod lod_builder;` + `pub use lod_builder::*;` in `lib.rs`.

- [x] **Step 4: Implement WorldManager state + region-key flow**

New fields on `WorldManager` (+ `use` items):

```rust
    /// LOD subscriptions per key, and the reverse map for disconnects.
    lod_subscribers: BTreeMap<LodKey, BTreeSet<ClientId>>,
    /// key -> RLE payload; LRU-evicted past LOD_CACHE_MAX_ENTRIES,
    /// subscribed keys pinned. `u64` is a monotonically increasing access
    /// stamp (not wall time — determinism).
    lod_cache: BTreeMap<LodKey, (LodPayload, u64)>,
    lod_access_counter: u64,
    /// Builds submitted, result not yet received — dedups double-requests.
    lod_in_flight: BTreeSet<LodKey>,
    /// Region-keyed builds waiting on a running region's terrain reply.
    lod_pending_terrain: BTreeMap<RegionCoords, BTreeSet<LodKey>>,
    lod_builder: Box<dyn LodBuilder>,
    lod_results: Receiver<LodResult>,
```

Session gains `lod_subscribed: BTreeSet<LodKey>` next to `far_subscribed`.

Packet arms replace the Task 5 stubs:

```rust
                ClientPacket::RequestLod(key) => self.lod_subscribe(id, key, now_ms),
                ClientPacket::ReleaseLod(key) => self.lod_unsubscribe(id, key),
```

Core methods (this task implements the region-key half; `start_lod_build` for tiles lands in Task 8 as `todo-free` stub that only handles `region_span() == 1`, extended there):

```rust
    fn lod_subscribe(&mut self, client: ClientId, key: LodKey, _now_ms: u64) {
        if !crate::lod::LOD_FACTORS.contains(&key.factor) {
            log::debug!("dropping RequestLod with invalid factor {}", key.factor);
            return;
        }
        let Some(session) = self.sessions.get_mut(&client) else {
            log::debug!("dropping RequestLod from sessionless client {client}");
            return;
        };
        session.lod_subscribed.insert(key);
        self.lod_subscribers.entry(key).or_default().insert(client);
        self.lod_access_counter += 1;
        let stamp = self.lod_access_counter;
        if let Some((payload, access)) = self.lod_cache.get_mut(&key) {
            *access = stamp;
            let _ = self
                .out
                .send((Some(client), ServerPacket::LodData(key, payload.clone())));
            return;
        }
        if !self.lod_in_flight.contains(&key) {
            self.start_lod_build(key, client);
        }
        // else: result fans out to all subscribers when it lands.
    }

    fn lod_unsubscribe(&mut self, client: ClientId, key: LodKey) {
        if let Some(session) = self.sessions.get_mut(&client) {
            session.lod_subscribed.remove(&key);
        }
        if let Some(set) = self.lod_subscribers.get_mut(&key) {
            set.remove(&client);
            if set.is_empty() {
                self.lod_subscribers.remove(&key);
            }
        }
    }

    /// Kick off a build for `key`. Region keys: authoritative terrain if the
    /// region has state (running-and-seeded or parked-with-cache), request
    /// terrain if running-but-unseeded, generate otherwise. `client` is the
    /// requester used for terrain round-trips.
    fn start_lod_build(&mut self, key: LodKey, client: ClientId) {
        if key.region_span() == 1 {
            let rc = key.origin_region();
            let running = self.regions.get(&rc).map_or(false, |l| !l.stopping);
            if let Some(snap) = self.far_cache.get(&rc) {
                self.lod_in_flight.insert(key);
                self.lod_builder.submit(LodBuildJob::Region {
                    key,
                    chunks: snap.chunks.clone(),
                });
            } else if running {
                // Terrain reply (RegionOutput::FarSnapshot) seeds far_cache
                // and re-enters via lod_terrain_ready.
                self.lod_pending_terrain.entry(rc).or_default().insert(key);
                self.send_to_region(rc, RegionInput::RequestFarSnapshot(client));
            } else {
                let columns = (self.generator)(rc);
                let snap = FarSnapshot::from_columns(columns);
                self.far_cache.insert(rc, snap.clone());
                self.lod_in_flight.insert(key);
                self.lod_builder
                    .submit(LodBuildJob::Region { key, chunks: snap.chunks });
            }
        } else {
            self.start_tile_build(key, client);
        }
    }

    /// Tile keys build in Task 8; this stub keeps Task 7 compiling.
    fn start_tile_build(&mut self, key: LodKey, _client: ClientId) {
        log::debug!("tile LOD build for {key:?} not implemented until Task 8");
    }

    /// A region's terrain just landed in far_cache: submit any LOD builds
    /// that were waiting on it.
    fn lod_terrain_ready(&mut self, rc: RegionCoords, client: ClientId) {
        let Some(keys) = self.lod_pending_terrain.remove(&rc) else { return };
        for key in keys {
            if !self.lod_in_flight.contains(&key) {
                self.start_lod_build(key, client);
            }
        }
    }

    pub fn handle_lod_result(&mut self, key: LodKey, payload: LodPayload) {
        self.lod_in_flight.remove(&key);
        self.lod_access_counter += 1;
        self.lod_cache.insert(key, (payload.clone(), self.lod_access_counter));
        self.evict_lod_cache();
        if let Some(subs) = self.lod_subscribers.get(&key) {
            for client in subs {
                let _ = self
                    .out
                    .send((Some(*client), ServerPacket::LodData(key, payload.clone())));
            }
        }
    }

    /// Drain the builder results channel (Inline path; the native server
    /// selects on the receiver instead and calls handle_lod_result directly).
    pub fn pump_lod_results(&mut self) {
        while let Ok((key, payload)) = self.lod_results.try_recv() {
            self.handle_lod_result(key, payload);
        }
    }

    fn evict_lod_cache(&mut self) {
        while self.lod_cache.len() > LOD_CACHE_MAX_ENTRIES {
            let victim = self
                .lod_cache
                .iter()
                .filter(|(k, _)| !self.lod_subscribers.contains_key(k))
                .min_by_key(|(_, (_, access))| *access)
                .map(|(k, _)| *k);
            match victim {
                Some(k) => {
                    self.lod_cache.remove(&k);
                }
                None => break, // everything pinned by subscribers
            }
        }
    }
```

Hook the existing flows:
- In `handle_region_output`'s `RegionOutput::FarSnapshot(client, snap)` arm: after `self.far_cache.insert(rc, snap.clone());` add `self.lod_terrain_ready(rc, client);` (the FarRegion send stays until Task 12).
- In `ServerEvent::ClientDisconnected`: iterate `session.lod_subscribed` and remove the client from `lod_subscribers` (mirroring the `far_subscribed` cleanup loop directly above it).
- `Session` derive stays `Default` (BTreeSet default is fine).

Update every `WorldManager::new` caller with the two new arguments:
- `crates/server/src/lib.rs:258` — temporary `InlineLodBuilder` (replaced by the thread pool in Task 9): `Box::new(game::InlineLodBuilder { coarse: Box::new(move |key| worldgen::coarse_tile(key, world_seed, materials)), out: lod_result_send })` plus a `pump_lod_results()` call after `manager.maintain(now_ms)` in the loop.
- `crates/client/src/local_server.rs:47` — same Inline construction; add `self.manager.pump_lod_results();` at the top of `drain()`'s per-iteration body (inside the `for _ in 0..2` loop, after `pump()`).
- `crates/game/tests/world_manager.rs` and `crates/game/tests/handoff_manager.rs` — a fake coarse closure (`|_| Vec::new()`) in their shared setup helper.

- [x] **Step 5: Run tests**

Run: `cargo test -p game --test lod_streaming && cargo test -p game --test world_manager && just test` — expect PASS.

- [x] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(server-lod): LodBuilder + WorldManager subscriptions, cache, region-key serving"
```

---

### Task 8: Tile builds (coarse + authoritative overlay + waiting-on-terrain)

**Files:**
- Modify: `crates/game/src/world_manager.rs`
- Modify: `crates/game/tests/lod_streaming.rs`

**Interfaces:**
- Consumes: Task 7's `start_lod_build` dispatch, `LodBuildJob::Tile`.
- Produces: `fn start_tile_build(&mut self, key: LodKey, client: ClientId)`; new field `lod_tile_waiting: BTreeMap<LodKey, BTreeSet<RegionCoords>>`.

- [x] **Step 1: Write failing tests**

```rust
#[test]
fn tile_build_is_coarse_only_when_every_region_is_pristine() {
    let (mut m, out, _region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    let key = LodKey { factor: 64, coords: [50, 50] }; // far away, all pristine
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
    m.pump_lod_results();
    let payload = expect_lod_data(&out, key); // helper: exactly one LodData for key
    // The fake coarse closure emits BlockId(42) everywhere; no overlay ran,
    // so the payload is pure marker.
    let grid = payload[0].1.decode().unwrap();
    assert!(grid.iter().all(|v| v.block == BlockId(42)));
}

#[test]
fn tile_build_overlays_regions_with_authoritative_state() {
    // Park region (0,0) with the flat-floor generator state (subscribe it,
    // release, run maintain past UNLOAD_GRACE_MS — same dance as
    // world_manager.rs's parking tests), then request the factor-64 tile
    // [0,0] (covers regions (0..2, 0..2)).
    // Assert: the tile payload's region-(0,0) x/z cell rect (cells 0..16)
    // is NOT all-BlockId(42) (authoritative overlay replaced the coarse
    // marker there), while the rect for pristine region (1,1)
    // (cells 16..32 x and z) IS all marker/air.
}

#[test]
fn tile_build_waits_for_running_unseeded_regions() {
    // Region (0,0) running (subscribed by client 2) but far_cache unseeded.
    // Client 1 requests tile [0,0] at factor 64.
    // Assert: no LodData yet; after pumping the spawner + routing the
    // region's FarSnapshot reply + pump_lod_results, exactly one LodData
    // arrives, and its region-(0,0) rect shows the authoritative floor.
}
```

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p game --test lod_streaming tile_build` — expect FAIL (`start_tile_build` unimplemented / no LodData).

- [x] **Step 3: Implement**

```rust
    /// Tile keys (64/256): coarse base + one overlay per region with
    /// authoritative state. Regions running-but-unseeded are requested and
    /// the build deferred until every such reply has landed; pristine and
    /// parked-with-cache regions need nothing. Pristine regions are NOT
    /// generated here — coarse sampling covers them (the point of tiles).
    fn start_tile_build(&mut self, key: LodKey, client: ClientId) {
        let mut waiting = BTreeSet::new();
        for rc in key.regions() {
            let running = self.regions.get(&rc).map_or(false, |l| !l.stopping);
            if running && !self.far_cache.contains_key(&rc) {
                waiting.insert(rc);
                self.lod_pending_terrain.entry(rc).or_default().insert(key);
                self.send_to_region(rc, RegionInput::RequestFarSnapshot(client));
            }
        }
        if !waiting.is_empty() {
            self.lod_tile_waiting.insert(key, waiting);
            return;
        }
        let overlays: Vec<(RegionCoords, Vec<(ChunkCoords, Chunk)>)> = key
            .regions()
            .into_iter()
            .filter_map(|rc| {
                self.far_cache.get(&rc).map(|snap| (rc, snap.chunks.clone()))
            })
            .collect();
        self.lod_in_flight.insert(key);
        self.lod_builder.submit(LodBuildJob::Tile { key, overlays });
    }
```

And generalize `lod_terrain_ready` to also settle tile waits:

```rust
    fn lod_terrain_ready(&mut self, rc: RegionCoords, client: ClientId) {
        let Some(keys) = self.lod_pending_terrain.remove(&rc) else { return };
        for key in keys {
            if let Some(waiting) = self.lod_tile_waiting.get_mut(&key) {
                waiting.remove(&rc);
                if !waiting.is_empty() {
                    continue;
                }
                self.lod_tile_waiting.remove(&key);
            }
            if !self.lod_in_flight.contains(&key) {
                self.start_lod_build(key, client);
            }
        }
    }
```

(Note `start_lod_build` re-runs the tile scan; by then every awaited region is seeded, so it submits. A region that parked meanwhile is seeded by `Stopped`'s far_cache insert, so the scan still passes.)

Caveat to encode as a comment: `far_cache` reflects the LAST pushed/parked terrain of a running region — mid-cadence edits appear on the next `FarSnapshotChanged` (which Task 9 turns into invalidation + rebuild), matching the spec's freshness decision.

- [x] **Step 4: Run tests**

Run: `cargo test -p game --test lod_streaming && just test` — expect PASS.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(server-lod): tile builds — coarse base, authoritative overlays, terrain waits"
```

---

### Task 9: Invalidation + refresh push; native-server thread-pool builder

**Files:**
- Modify: `crates/game/src/world_manager.rs`
- Modify: `crates/game/tests/lod_streaming.rs`
- Create: `crates/server/src/lod_workers.rs`
- Modify: `crates/server/src/lib.rs`

**Interfaces:**
- Produces:
  - `fn invalidate_region_lod(&mut self, rc: RegionCoords)` on WorldManager — called from the `FarSnapshotChanged` and `Stopped` arms
  - `server::lod_workers::ThreadLodBuilder` — `LodBuilder` over a 2-thread pool; `pub const LOD_BUILD_THREADS: usize = 2;`

- [x] **Step 1: Write failing tests**

```rust
#[test]
fn terrain_change_invalidates_and_repushes_subscribed_keys() {
    // Client 1 holds factor-4 of region (0,0) AND the factor-64 tile [0,0].
    // Drive the region: subscribe it (client 2), edit terrain via
    // spawner_mut().with_region (place a block through the region's data —
    // copy the mutation idiom from world_manager.rs's terrain tests if one
    // exists; otherwise tick until FAR_SNAPSHOT_MIN_INTERVAL_TICKS with a
    // scripted edit event), then route the resulting FarSnapshotChanged.
    // Assert: BOTH keys get a fresh LodData push to client 1, and the
    // factor-4 payload differs from the pre-edit one.
}

#[test]
fn terrain_change_with_no_subscribers_only_drops_cache() {
    // Build + cache factor-4 of (0,0) for client 1, then ReleaseLod.
    // Route a FarSnapshotChanged for (0,0).
    // Assert: no LodData packet is emitted; a fresh RequestLod afterwards
    // rebuilds (payload reflects the post-change terrain).
}

#[test]
fn lod_cache_evicts_unsubscribed_lru_but_pins_subscribed() {
    // Shrink the cap for the test: make LOD_CACHE_MAX_ENTRIES a field
    // (default LOD_CACHE_MAX_ENTRIES const) — mirror how rings made table
    // and band fields. Fill past the cap with released keys; assert len()
    // stays at cap and the still-subscribed key survives.
}
```

(For the third test, convert the cap into `lod_cache_max: usize` field defaulting to the const, like `WorldgenRings.table` does client-side. Add `pub fn lod_cache_len(&self) -> usize` + `pub fn set_lod_cache_max(&mut self, n: usize)` test hooks.)

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p game --test lod_streaming invalid` — expect FAIL.

- [x] **Step 3: Implement invalidation**

```rust
    /// A region's terrain changed (cadence push) or it parked: every LOD key
    /// depending on it is stale. Drop them from the cache; rebuild the ones
    /// with live subscribers (the fresh far_cache entry is the new source).
    fn invalidate_region_lod(&mut self, rc: RegionCoords) {
        for factor in crate::lod::LOD_FACTORS {
            let key = LodKey::for_region(factor, rc);
            self.lod_cache.remove(&key);
            let has_subs = self.lod_subscribers.get(&key).is_some_and(|s| !s.is_empty());
            if has_subs && !self.lod_in_flight.contains(&key) {
                // Any subscriber works as the terrain-request correlator.
                let client = *self.lod_subscribers[&key].iter().next().unwrap();
                self.start_lod_build(key, client);
            }
        }
    }
```

Call it from `handle_region_output`:
- `RegionOutput::FarSnapshotChanged(snap)` arm: after the `far_cache.insert`, add `self.invalidate_region_lod(rc);` (before the far fan-out).
- `RegionOutput::Stopped(serialized, far)` arm: after `self.far_cache.insert(rc, far);`, add `self.invalidate_region_lod(rc);` (park refreshes the frozen terrain — a final edit in the last cadence window must not be lost).

- [x] **Step 4: Implement `ThreadLodBuilder`** (`crates/server/src/lod_workers.rs`)

```rust
//! Native-server LOD build workers: a fixed pool of threads consuming
//! `LodBuildJob`s so a cache-miss (worst case a full region generation)
//! never stalls the manager thread's event routing.

use crossbeam::channel::{unbounded, Sender};
use game::{build_lod_job, LodBuildJob, LodBuilder, LodResult};

/// Named constant per the tunables convention. Two threads: LOD fill is
/// background work; region sim threads keep priority on cores.
pub const LOD_BUILD_THREADS: usize = 2;

pub struct ThreadLodBuilder {
    jobs: Sender<LodBuildJob>,
}

impl ThreadLodBuilder {
    /// `coarse` must be Clone-able across workers — pass seed + materials
    /// and rebuild the closure per thread.
    pub fn new(
        seed: u64,
        materials: worldgen::material::Materials,
        out: Sender<LodResult>,
    ) -> Self {
        let (jobs, job_recv) = unbounded::<LodBuildJob>();
        for i in 0..LOD_BUILD_THREADS {
            let recv = job_recv.clone();
            let out = out.clone();
            std::thread::Builder::new()
                .name(format!("lod-worker-{i}"))
                .spawn(move || {
                    let coarse =
                        move |key: &game::lod::LodKey| worldgen::coarse_tile(key, seed, materials);
                    while let Ok(job) = recv.recv() {
                        let _ = out.send(build_lod_job(job, &coarse));
                    }
                })
                .expect("spawn lod worker");
        }
        Self { jobs }
    }
}

impl LodBuilder for ThreadLodBuilder {
    fn submit(&mut self, job: LodBuildJob) {
        let _ = self.jobs.send(job);
    }
}
```

Wire in `crates/server/src/lib.rs`: add `mod lod_workers;`; replace the Task 7 temporary `InlineLodBuilder` with `Box::new(lod_workers::ThreadLodBuilder::new(world_seed, materials, lod_result_send))`.

The results receiver stays solely inside the manager (do NOT clone it into a select arm — two receivers on one crossbeam queue would race for results). Instead, drain it on the loop's existing heartbeat: add `manager.pump_lod_results();` next to `manager.maintain(now_ms);` after the `select!`. Worst-case delivery latency is the select's 200 ms `default` timeout — fine for horizon terrain.

- [x] **Step 5: Run tests**

Run: `cargo test -p game --test lod_streaming && cargo build -p server && just test` — expect PASS.

- [x] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(server-lod): terrain-change invalidation + refresh push; thread-pool builder"
```

---

### Task 10: Client instance — ring assignment → RequestLod/ReleaseLod

**Files:**
- Modify: `crates/client/src/instance.rs`

**Interfaces:**
- Consumes: `game::lod::{ring_factor_in, LodKey, LOD_RING_TABLE, LOD_STICKY_BAND}`, packets from Task 5.
- Produces:
  - `pub fn desired_lod(viewer: RegionCoords, excluded: &BTreeSet<RegionCoords>, prev: &BTreeMap<RegionCoords, u32>, table: &[(i32, u32)], band: i32) -> (BTreeMap<RegionCoords, u32>, BTreeSet<LodKey>)` — pure, unit-tested
  - `GameInstanceManager::update_lod_window(&mut self)` — diffs and sends request/release + `ClientUpdateEvent::RemoveLod`
  - `ServerPacket::LodData` routing → `ClientUpdateEvent::LodData`

- [x] **Step 1: Write failing tests** (in `instance.rs`'s test module, or a new `#[cfg(test)] mod lod_window_tests`)

```rust
#[cfg(test)]
mod lod_window_tests {
    use super::*;
    use game::lod::{LodKey, LOD_RING_TABLE, LOD_STICKY_BAND};
    use game::RegionCoords;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn desired_lod_assigns_by_table_and_excludes() {
        let viewer = RegionCoords::new(0, 0);
        let mut excluded = BTreeSet::new();
        excluded.insert(RegionCoords::new(0, 0)); // near window member
        let (assigned, keys) =
            desired_lod(viewer, &excluded, &BTreeMap::new(), LOD_RING_TABLE, 0);
        assert!(!assigned.contains_key(&RegionCoords::new(0, 0)), "excluded");
        assert_eq!(assigned[&RegionCoords::new(0, 1)], 2, "distance 1 -> factor 2");
        assert_eq!(assigned[&RegionCoords::new(0, 4)], 4);
        assert_eq!(assigned[&RegionCoords::new(0, 8)], 16);
        assert_eq!(assigned[&RegionCoords::new(0, 16)], 64);
        assert_eq!(assigned[&RegionCoords::new(0, 32)], 256);
        assert!(!assigned.contains_key(&RegionCoords::new(0, 33)), "past radius");
        // Region-keyed factors produce one key per region; tile factors dedup.
        assert!(keys.contains(&LodKey::for_region(2, RegionCoords::new(0, 1))));
        assert!(keys.contains(&LodKey::for_region(64, RegionCoords::new(0, 16))));
        let f64_keys = keys.iter().filter(|k| k.factor == 64).count();
        let f64_regions = assigned.values().filter(|&&f| f == 64).count();
        assert!(f64_keys * 2 < f64_regions, "2x2 tiles dedup regions into fewer keys");
    }

    #[test]
    fn desired_lod_is_sticky_across_a_boundary_step() {
        let viewer = RegionCoords::new(0, 0);
        let none = BTreeSet::new();
        let (prev, _) = desired_lod(viewer, &none, &BTreeMap::new(), LOD_RING_TABLE, LOD_STICKY_BAND);
        // Step the viewer +1x: region (-4, 0) is now distance 5 = bound 4 + band 1,
        // held at factor 4 by stickiness even though fresh assignment says 16.
        let (next, _) = desired_lod(RegionCoords::new(1, 0), &none, &prev, LOD_RING_TABLE, LOD_STICKY_BAND);
        assert_eq!(next[&RegionCoords::new(-4, 0)], 4, "held by the sticky band");
    }
}
```

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p client lod_window` — expect compile FAIL.

- [x] **Step 3: Implement**

Pure helper (free function in `instance.rs`):

```rust
/// Ring assignment for every region in view: per-region factor via the
/// shared `ring_factor_in` (sticky against `prev`), minus `excluded`
/// (near-window — and, until Task 12, far-ring — members render
/// authoritative state). Returns the region->factor map (the next `prev`)
/// and the deduped key set to hold subscriptions for.
pub fn desired_lod(
    viewer: RegionCoords,
    excluded: &BTreeSet<RegionCoords>,
    prev: &BTreeMap<RegionCoords, u32>,
    table: &[(i32, u32)],
    band: i32,
) -> (BTreeMap<RegionCoords, u32>, BTreeSet<game::lod::LodKey>) {
    let radius = table.last().map_or(0, |&(bound, _)| bound) + band;
    let mut assigned = BTreeMap::new();
    let mut keys = BTreeSet::new();
    let mut candidates: BTreeSet<RegionCoords> = prev.keys().copied().collect();
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            candidates.insert(RegionCoords::new(viewer.x + dx, viewer.z + dz));
        }
    }
    for rc in candidates {
        if excluded.contains(&rc) {
            continue;
        }
        let dist = (rc.x - viewer.x).abs().max((rc.z - viewer.z).abs());
        if let Some(factor) =
            game::lod::ring_factor_in(table, band, dist, prev.get(&rc).copied())
        {
            assigned.insert(rc, factor);
            keys.insert(game::lod::LodKey::for_region(factor, rc));
        }
    }
    (assigned, keys)
}
```

Manager state + method (fields next to `far_subscribed`):

```rust
    lod_assigned: BTreeMap<RegionCoords, u32>,
    lod_subscribed: BTreeSet<game::lod::LodKey>,
```

```rust
    /// Diff the desired LOD key set against current subscriptions. Runs
    /// after update_far_window; until Task 12 removes the far ring, the
    /// table skips factor 2 (`&LOD_RING_TABLE[1..]`) and far-ring regions
    /// are excluded alongside the near window, so streamed LOD and the far
    /// path never cover the same region.
    fn update_lod_window(&mut self) {
        let Some(center) = self.viewer_region() else { return };
        let mut excluded = desired_window(center, self.home_region);
        excluded.extend(self.far_subscribed.iter().copied());
        let (assigned, desired) = desired_lod(
            center,
            &excluded,
            &self.lod_assigned,
            &game::lod::LOD_RING_TABLE[1..],
            game::lod::LOD_STICKY_BAND,
        );
        self.lod_assigned = assigned;
        if desired == self.lod_subscribed {
            return;
        }
        for key in desired.difference(&self.lod_subscribed) {
            self.server_game_send.send(ClientPacket::RequestLod(*key)).unwrap();
        }
        let stale: Vec<game::lod::LodKey> =
            self.lod_subscribed.difference(&desired).copied().collect();
        for key in stale {
            self.server_game_send.send(ClientPacket::ReleaseLod(key)).unwrap();
            let _ = self.client_event_send.send(ClientUpdateEvent::RemoveLod(key));
        }
        self.lod_subscribed = desired;
    }
```

Call `self.update_lod_window();` immediately after the existing `self.update_far_window();` call site (find it — same place `update_window` is driven). If `desired_window` returns a non-BTreeSet type, adapt (it returns the near set used with `.difference` in `update_window` — reuse its actual type).

`handle_server` gains (next to the `FarRegion` arm):

```rust
            game::ServerPacket::LodData(key, payload) => {
                if !self.lod_subscribed.contains(&key) {
                    // Raced a release: tell the server we really don't want it.
                    let _ = self.server_game_send.send(ClientPacket::ReleaseLod(key));
                    return Ok(());
                }
                let _ = self
                    .client_event_send
                    .send(ClientUpdateEvent::LodData(key, payload));
                Ok(())
            }
```

(match the surrounding arms' control flow — if they fall through rather than early-return, do the same.)

- [x] **Step 4: Run tests**

Run: `cargo test -p client && just test` — expect PASS (renderer still runs the old worldgen rings; `LodData` events hit the Task 5 stub arm).

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(client-lod): instance ring assignment drives RequestLod/ReleaseLod subscriptions"
```

---

### Task 11: Renderer — `lod_rings.rs` replaces client-side generation

**Files:**
- Rename: `crates/client/src/renderer/worldgen_rings.rs` → `crates/client/src/renderer/lod_rings.rs` (git mv, then rewrite)
- Modify: `crates/client/src/renderer/mod.rs`
- Modify: `crates/client/src/renderer/bridge.rs` (route the two events)

**Interfaces:**
- Consumes: `ClientUpdateEvent::LodData/RemoveLod` (via bridge), Task 1–3 types, existing `meshing::lod` spawner contract + `pipeline_app` test harness.
- Produces:
  - `#[derive(Resource, Default)] pub struct LodRings { pub roots: BTreeMap<LodKey, LodSlot>, pub retired: Vec<RetiredLod>, pub pending: Vec<LodEventMsg>, pub retirement_max_frames: u32 }`
  - `pub enum LodEventMsg { Data(LodKey, LodPayload), Remove(LodKey) }`
  - `pub struct LodSlot { pub root: Entity, pub retiring: Option<Entity>, pub retiring_frames: u32 }`
  - `pub struct RetiredLod { pub key: LodKey, pub entity: Entity, pub frames: u32 }`
  - Systems: `apply_lod_events`, `retire_lod_roots` (registered where the three worldgen-ring systems were)

- [x] **Step 1: Delete the generation half, keep+rename what survives**

`git mv crates/client/src/renderer/worldgen_rings.rs crates/client/src/renderer/lod_rings.rs`. Delete: `WORLDGEN_VIEW_RADIUS`, `WORLDGEN_RINGS`, `RING_STICKY_BAND` (table/band now live in `game::lod`), `WORLDGEN_TASKS_MAX_IN_FLIGHT`, `ring_factor`/`ring_factor_in` re-export + their moved tests, `WorldgenRings` resource, `WorldgenTask`, `manage_worldgen_rings`, `finish_worldgen_tasks`, `build_worldgen_lod_cells`, and the seed/materials plumbing (`load_worldgen_materials` in `renderer/mod.rs` too). Keep: `RETIREMENT_MAX_FRAMES` and the retirement-shape doc comments as reference for the rewrite.

- [x] **Step 2: Write failing tests** (in `lod_rings.rs`, reusing `pipeline_app`/`wait_until` from `meshing::queue::tests` exactly as the old file did)

```rust
#[test]
fn lod_data_spawns_root_and_children_per_spawner_contract() {
    let mut app = rings_app(); // pipeline_app + LodRings + the two systems
    let key = LodKey { factor: 64, coords: [1, -1] };
    let payload = vec![([0, 2, 0], PackedLodGrid::encode(&one_solid_grid()))];
    push_event(&mut app, LodEventMsg::Data(key, payload));
    assert!(wait_until(&mut app, |app| slot_root(app, key).is_some()));
    let root = slot_root(&app, key).unwrap();
    let off = key.world_offset();
    assert_eq!(
        app.world().entity(root).get::<Transform>().unwrap().translation,
        Vec3::new(off[0], off[1], off[2])
    );
    // Child per the meshing::lod spawner contract.
    let (tf, lc) = single_lod_child(&mut app, root);
    assert_eq!(lc.factor, 64);
    assert_eq!(tf.scale, Vec3::splat(64.0));
    assert_eq!(tf.translation, Vec3::new(0.0, (2 * 32 * 64 - 64) as f32, 0.0));
    // And it meshes via the existing LOD chain.
    assert!(wait_until(&mut app, |app| root_meshed(app, root)));
}

#[test]
fn re_receipt_retires_the_old_root_until_the_replacement_meshes() {
    // Send Data(key, payload), wait meshed; send Data(key, payload2).
    // Assert the first root survives every frame until the second root has
    // a meshed child, then despawns (same invariant style as the old
    // factor_transition test; slow-tag if > 100ms).
}

#[test]
fn remove_lod_retires_until_deadline_or_coverage() {
    // Data + meshed, then Remove(key). With retirement_max_frames shrunk to
    // 3, the root must survive ~2 updates then despawn (no replacement ever
    // comes — deadline path). Assert retired list drains.
}

#[test]
fn undecodable_grids_are_dropped_not_spawned() {
    let mut app = rings_app();
    let key = LodKey { factor: 4, coords: [0, 0] };
    let bad = vec![([0, 0, 0], PackedLodGrid(vec![(3, BlockId(1))]))]; // wrong total
    push_event(&mut app, LodEventMsg::Data(key, bad));
    app.update();
    let root = slot_root(&app, key).expect("root spawns even if all cells drop");
    assert_eq!(child_count(&mut app, root), 0, "bad cell dropped with a warning");
}
```

Write the small helpers (`rings_app`, `push_event` = push onto `LodRings.pending`, `slot_root`, `root_meshed`, `single_lod_child`, `child_count`, `one_solid_grid`) in the test module — model them on the old file's helpers.

- [x] **Step 3: Run to verify failure**

Run: `cargo test -p client lod_rings` — expect compile FAIL.

- [x] **Step 4: Implement**

`apply_lod_events` (system): drain `rings.pending`; for `Data(key, payload)`:
1. Decode each `(group, packed)` via `packed.decode()`; `warn!` and skip cells that return `None`.
2. Spawn a root at `key.world_offset()` (Transform + Visibility + `Name::new(format!("lod {:?} x{}", key.coords, key.factor))`).
3. Spawn one child per cell EXACTLY per the spawner contract (copy the loop from the old `finish_worldgen_tasks`, `crates/client/src/renderer/worldgen_rings.rs:444-486` in git history: translation `group * 32 * factor` with `- factor` on y, `scale: splat(factor)`, `LodChunk { factor }`, `LodVoxelData(grid)`, `ChildOf(root)`, then the same-batch `LodNeighbors` wiring loop).
4. Replace-on-re-receipt: if the key already has a slot, the old root becomes `retiring` (despawning any previous `retiring` first — at most one old root per key, same rule as the old code).

For `Remove(key)`: move the slot's live root (root, else retiring) to `retired` with `frames: 0`; despawn any second entity.

`retire_lod_roots` (system): port the old `retire_roots` logic minus the near/far takeover checks it can no longer see — retiring roots despawn when the slot's new root has a meshed child; retired roots despawn when ANY meshed `LodChunk` child's root belongs to a slot whose key covers one of the retired key's regions (build a `BTreeSet<RegionCoords>` of covered regions from live meshed slots once per run), OR a near-window chunk meshed inside the retired key's region bounds (keep the `keyed_meshes` check from the old code, widened to every region in `key.regions()`), OR `frames >= retirement_max_frames`.

Bridge routing (replace the Task 5 stub arms in `drain_client_updates`):

```rust
            ClientUpdateEvent::LodData(key, payload) => {
                lod_rings.pending.push(lod_rings::LodEventMsg::Data(key, payload));
            }
            ClientUpdateEvent::RemoveLod(key) => {
                lod_rings.pending.push(lod_rings::LodEventMsg::Remove(key));
            }
```

(add `ResMut<lod_rings::LodRings>` to `drain_client_updates`'s params.)

`renderer/mod.rs`: replace the `WorldgenRings::new(load_worldgen_materials())` resource insert with `init_resource::<lod_rings::LodRings>()` (Default impl sets `retirement_max_frames: RETIREMENT_MAX_FRAMES`); replace the three system registrations with `(lod_rings::apply_lod_events, lod_rings::retire_lod_roots).chain()`; delete `load_worldgen_materials` and the `worldgen::material` import.

Delete the old worldgen-ring test suites that tested generation/assignment (assignment now tested in Task 2/`game` and Task 10/`instance.rs`); keep/port the retirement-shape tests as rewritten above.

- [x] **Step 5: Run tests**

Run: `cargo test -p client && just test` — expect PASS. `cargo build -p client` must show no dangling `worldgen_rings` references (grep `worldgen_rings\|WorldgenRings` under `crates/client/` → only `lod_rings.rs` history).

- [x] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(client-lod): network-fed lod_rings renderer replaces client-side worldgen rings"
```

---

### Task 12: Fold in factor 2 — delete the FarSnapshot wire path, rename internals

**Files:**
- Modify: `crates/game/src/protocol.rs`, `crates/game/src/lib.rs`, `crates/game/src/region_runner.rs`, `crates/game/src/world_manager.rs`, `crates/game/src/region.rs` (if `far_snapshot()` lives there)
- Modify: `crates/client/src/instance.rs`, `crates/client/src/renderer/bridge.rs`, `crates/client/src/renderer/mod.rs`
- Modify: `crates/game/tests/far_snapshot.rs` (rename → `terrain_snapshot.rs`), `crates/game/tests/world_manager.rs`, client harness/convergence tests
- Modify: `CLAUDE.md`

This task is a coordinated deletion/rename — run `just test` only at the end of the sweep; individual steps won't compile in isolation.

- [x] **Step 1: Wire deletions**

- `ClientPacket`: delete `RequestFarRegion`/`ReleaseFarRegion`. `ServerPacket`: delete `FarRegion`. `ClientUpdateEvent`: delete `NewFarRegion`/`RemoveFarRegion`. Delete `FAR_VIEW_RADIUS` (`grep -rn FAR_VIEW_RADIUS` → update every reader; the ring table's `(2, 2)` entry replaces it).
- `crates/client/src/instance.rs`: delete `far_subscribed` field, `update_far_window`, `drop_far_region`, `desired_far`, the `FarRegion` packet arm; in `update_lod_window` switch the table from `&LOD_RING_TABLE[1..]` to `game::lod::LOD_RING_TABLE` and drop the far-exclusion `extend` (near window remains the only exclusion).
- `crates/game/src/world_manager.rs`: delete `far_subscribe`/`far_unsubscribe`, `far_subscribers`, `Session.far_subscribed`, the two packet arms, and the `FarSnapshotChanged` fan-out loop (invalidation + LOD re-push replaces it; keep the `far_cache` insert + `invalidate_region_lod` call).
- `crates/client/src/renderer/bridge.rs`: delete `FarRegionRoots`, `spawn_far_region`, `remove_far_region`, `finish_far_lod_builds` registration + the `NewFarRegion`/`RemoveFarRegion` arms and their tests (`far_lod_app_shell` + the far suites — the behavior is covered by Task 11's `lod_rings` tests now). `crates/client/src/renderer/meshing/lod.rs`: delete `FarLodBuild`.
- Fix `retire_lod_roots`'s near-coverage check if it referenced `FarRegionRoots` (it must not, per Task 11).

- [x] **Step 2: Renames (mechanical, workspace-wide)**

`FarSnapshot` → `TerrainSnapshot`; `RegionInput::RequestFarSnapshot` → `RegionInput::RequestTerrain`; `RegionOutput::FarSnapshot` → `RegionOutput::Terrain`; `RegionOutput::FarSnapshotChanged` → `RegionOutput::TerrainChanged`; `far_cache` → `terrain_cache`; `Region::far_snapshot()` → `Region::terrain_snapshot()`; `FAR_SNAPSHOT_MIN_INTERVAL_TICKS` → `TERRAIN_HASH_INTERVAL_TICKS`. Use `grep -rln` per identifier across `crates/{game,server,client}/src` and `crates/game/tests`; update doc comments that describe the old far ring (region_runner.rs module docs, protocol.rs comments). `git mv crates/game/tests/far_snapshot.rs crates/game/tests/terrain_snapshot.rs` and fix its imports.

- [x] **Step 3: Test updates**

- `crates/game/tests/world_manager.rs`: far-subscribe tests become LOD-subscribe tests at factor 2 (`LodKey::for_region(2, rc)`) asserting `LodData` instead of `FarRegion`; the "serve from cache without waking" and "generate on miss" behaviors are the same assertions through the new path.
- Client convergence/harness suites (`crates/client/src/harness.rs`, `bevy_harness.rs`, headless tests referencing far packets): route factor-2 through `LodData`. Run `cargo test -p client 2>&1 | head -50` iteratively.
- Verify the factor-2 grids end-to-end: the existing client/server convergence harness must show LOD children spawning for a distance-1..2 region (add one assertion to whichever harness test previously asserted `NewFarRegion` handling).

- [x] **Step 4: Docs**

`CLAUDE.md`: update the `game` bullet (far-ring/`FarSnapshot` sentences → streamed-LOD/`TerrainSnapshot` + `LodKey` description), the `client` bullet (`worldgen_rings.rs` → `lod_rings.rs`, "renders coarse unsubscribed terrain at zero bandwidth" → "renders server-streamed LOD rings"), and the `worldgen` bullet (mention `coarse_tile`). Justfile: no changes (verify `just --list` recipes still match reality).

- [x] **Step 5: Full verification**

Run: `just test` then `just test-full`. Expected: all green.
Run: `cargo check -p client --target wasm32-unknown-unknown` — the wasm build must compile (netcode_web serializes the same enums; local_server drives the same WorldManager).
Live smoke (needs the user or a `just run` session): server + one client; fly toward the horizon; verify ring transitions and no holes; place a block near a region boundary, walk 3+ regions away, confirm the edit shows in the factor-4 ring after ~the cadence interval.

- [x] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(lod)!: fold factor-2 far ring into streamed LOD; delete FarSnapshot wire path"
```

---

## Self-Review Checklist (run after drafting; findings already folded in)

1. **Spec coverage:** §1 keys/wire → Tasks 2/3/5; §2 cache/sources/invalidation/workers → Tasks 7/8/9; §3 coarse sampler → Task 6; §4 client → Tasks 10/11; §5 fold-in/edge cases/tests → Task 12 + per-task tests; shared-code move → Task 1.
2. **Type consistency:** `LodPayload = Vec<([i32;3], PackedLodGrid)>` used in protocol, builder (`encode_cells`), manager, ClientUpdateEvent, renderer decode. `LodKey` methods: `for_region/region_span/origin_region/regions/world_offset` only — no other method referenced anywhere.
3. **Known deferred items** (spec-sanctioned): tolerance threshold pinned by measurement in Task 6; cross-key `LodNeighbors` out of scope; LOD cache not persisted.
