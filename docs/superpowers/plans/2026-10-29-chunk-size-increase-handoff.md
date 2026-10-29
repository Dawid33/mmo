# Handoff — sim chunk from 2 blocks to 16 blocks per edge

**Date:** 2026-10-29 · **Status:** not started · **Asked by:** Dawid ("a chunk should be at least
16 blocks width / height / length").
**No migration:** no worlds exist outside this checkout, so nothing on disk needs converting.
Delete the worldgen disk cache and re-record goldens on purpose. The forged world file
`worlds/default.world` is height/geology rasters, not chunks, so it stays valid.
**Downstream:** the tree / plant-life design
(`docs/superpowers/specs/2026-10-29-trees-investigation.md`) assumes this change.

Work on `develop` in the main checkout (no worktrees). Check `git status` first and never commit
Dawid's uncommitted work for him. Verify per the CLAUDE.md rules (`just test-one`, `just smoke`,
`just test-threads`, `just determinism-wasm`); don't run whole suites without asking. Ask
before benchmark or perf runs (Dawid runs his own on this box).

---

## 1. Why

`crates/game/src/block.rs:16`: `CHUNK_BLOCKS = 32 / BLOCK_VOXELS` = **2**. The block-layer
commit `52f37d3` made blocks 16 voxels (1 m) but kept the old 32³-voxel chunk edge. So a sim
`Chunk` is 2×2×2 blocks, and **every chunk is an entity with a fixed rapier body and a
collider**. A 256 m play region is ~50k chunk entities (CLAUDE.md region-size notes). That is
probably a large share of region start time (3.9 s at 128), snapshot size (578 MiB raw),
ECS/physics overhead and client entity count.

## 2. Target shape (decided in this handoff; revisit only with a reason)

**Split the two meanings of "chunk".**

| Concept | Size | Owns | Where |
|---|---|---|---|
| **Sim chunk** (`Chunk`) | **16×16×16 blocks** = 256³ voxels = 16 m | block grid, sparse chisel map, ONE entity, ONE fixed body, ONE collider set | game, worldgen, server, wire |
| **Render brick** | **32³ voxels** = 2×2×2 blocks (today's chunk) | mesh records, GPU storage buffer, LOD grid cell, client visibility | client meshing / voxel_pull / LOD |

Why the brick must stay 32³ (confirmed in code):
- **Quad records** (`client/src/renderer/meshing/records.rs:4-5`, LOCKED and mirrored in
  `voxel_pull.wgsl`, `voxel_pull_depth.wgsl`, `voxel_texture.wgsl`) have 6-bit x/y/z/w/h
  fields. 256 needs 9 bits plus 9-bit w/h, which doesn't fit 2×u32.
- **The binary mesher** (`meshing/binary.rs`) uses u64 column masks over a padded 34³.
- **LOD** grids are a fixed 32³ contract (`lod.rs:3-14`).

None of that changes. A sim chunk is a container of 8×8×8 = 512 bricks.

**`CHUNK_BLOCKS` becomes an independent constant (16)**. Add `BRICK_VOXELS = 32` (and
`BRICK_BLOCKS = 2`) for the render/LOD side. Const-assert that
`CHUNK_BLOCKS * BLOCK_VOXELS` is a multiple of `BRICK_VOXELS`. Keep `CHUNK_BLOCKS` a single dial
so 8 or 32 can be tried later. Every *literal* 32 must say which of the two it means.

**The region-size dial moves to blocks.** `REGION_CHUNKS` is in 32-voxel chunks everywhere
(`REGION_SIZE = REGION_CHUNKS * 32`, `protocol.rs:47`). Make **`REGION_BLOCKS` the primary
build dial** (`VOXEL_REGION_BLOCKS`, legal 64 / 128 / 256 … up to the cell-mode ceiling).
Derive `REGION_CHUNKS = REGION_BLOCKS / CHUNK_BLOCKS` and `REGION_SIZE = REGION_BLOCKS * BLOCK_VOXELS`.
Delete the private duplicate in `worldgen/src/lib.rs:34`. Do the same for cells:
`VOXEL_CELL_CHUNKS` becomes `VOXEL_CELL_BLOCKS` (cell edge in blocks, a multiple of
`CHUNK_BLOCKS`). Today's defaults map as 32 → 64 blocks (test / check / clippy) and
128 → 256 blocks (play recipes). Update the `justfile`, `.cargo/config.toml [env]`,
`scripts/run.sh` and CLAUDE.md in the same change. A 64-block (64 m) test region is only
4×4 chunk columns. Check that the tests' seam / corner walks still have room
(`handoff_schedule.rs`, `threaded_world.rs`).

## 3. Required changes (from a full sweep — file:line as of `548f7f1`)

### 3.1 Types that break outright at 16 blocks

- **`BlockIndex(pub u8)`** (`block.rs:33`; `from_xyz` / `xyz` :36-40). 16³ = 4096 overflows
  `u8`, so change it to **`u16`**. It is the chisel `BTreeMap` key, so it is serialized and
  hashed.
- **`ColumnGround { surface: [i16; FOOTPRINT], geo: [i16; FOOTPRINT] }`** (`ground.rs:11-22`).
  FOOTPRINT becomes 256 and serde's array impls stop at 32. Use `Box<[i16]>` / `Vec<i16>`
  with a length check, or `serde-big-array`. StateHash is const-generic and fine.
- **`ChunkShape = ConstShape3u32<32,32,32>` / `CHUNK_VOXEL_COUNT`** (`voxel.rs:9,11`). These are
  really brick types now. Rename them to `BrickShape` / `BRICK_VOXEL_COUNT` and keep them for
  the client and LOD. **The sim must never build a dense voxel array for a whole chunk**: 256³
  `Voxel`s is ~16–32 MiB.
- **`Chunk.blocks: Vec<Block>`** becomes 4096 blocks (~20 KiB dense, with `Block` = u16 + 3 B).
  Most chunks are all-air, all-stone or all-water, so give it a compact representation:
  `enum ChunkBlocks { Uniform(Block), Dense(Box<[Block]>) }` (palette + indices optional
  later). Serialization and `StateHash` must be canonical: a dense chunk that becomes uniform
  normalises to `Uniform`, or equal chunks hash differently.

### 3.2 Chunk coordinates recovered from body translation `/32` — in hashed paths

All of these divide the body translation by a literal 32 to get chunk coords:
- `cells.rs:206-215` `chunk_coords_of`.
- `state.rs:1409` `chunk_terrain_term` (**hashed**).
- `state.rs:1426-1428` `terrain_snapshot`.
- `state.rs:626,670` (`create_mesh`, `seed_terrain_genesis`) and `edit.rs:177-196`
  `spawn_chunk_safe`, which place bodies at `coords * 32`.
- `cells.rs:312-335` `materialize_cell`.

Replace them with a `CHUNK_UNITS` constant (`CHUNK_BLOCKS * BLOCK_VOXELS` = 256), or better,
store `ChunkCoords` as a component on the chunk entity and stop deriving it from float
translation.

### 3.3 Sim hot paths whose cost scales with chunk volume

- **Edits** (`edit.rs:234+`, around :317-330). `set_block_safe` re-derives the WHOLE chunk:
  dense old/new `SetVoxelComponent` + `rebuild_chunk_colliders_safe`. At 16³ blocks that is a
  32 MiB update per click. It needs:
  - **A block-delta render update** instead of `SetVoxelComponent(Vec<Voxel>)`. A new
    `GameDataUpdateKind::SetBlocks(EntityKey, Vec<(BlockIndex, Block, Option<ChiselData>)>)`
    in both apply and undo directions; the client re-derives only the touched bricks (§3.6).
  - **An incremental collider edit.** parry's voxels shape has `set_voxel`
    (`crates/parry/src/shape/voxels/voxels_edition.rs:45-67`). Use it for block-scale cells
    instead of rebuilding. The rollback inverse is the old cell state, and the StepJournal
    already captures collider touches. Check what the voxels content-hash cache
    (`mmo-fork`) does on `set_voxel`. Chiseled blocks keep a voxel-scale collider. Decide
    whether that is one child collider per chiseled block or one voxel-scale shape per
    chunk, and measure.
  - **A smaller undo footprint.** "Undo replaces the whole chunk value" (`block.rs` ChiselData
    doc) is no longer acceptable at 20 KiB+ per entry. Log per-block deltas (tier-1
    `UndoMap`-style) or a sparse diff.
- **Collider genesis** (`block.rs:269-364` `collider_cells`, `state.rs:684-692` full-cube
  collapse to one `cuboid(16,16,16)`). Keep the collapse rule; a uniform-solid chunk becomes
  one 256-unit cuboid. Multi-scale (block 16 + chisel 1) stays.
- **Implied ground** (`ground.rs`, `edit.rs:161-172` `materialize_ground_safe`, iterating
  CHUNK_BLOCKS³). Breaking a block materialises its chunk plus implied face-neighbour chunks.
  At 16 m that is up to 7 × 4096 blocks from the recipe, which is fine at block level, but
  check the undo entry size. `edge_ring_locked` (`edit.rs:110-118`) indexes the footprint
  arrays.
- **`surface_voxel`** (`state.rs:837-866`) has div / rem / loops by 32.
- **Cell mode.** `cells.rs` (`cell_size_units = cell_chunks*32` :38-40, `cell_of_*` :44-66,
  `cell_chunk_keys` :218-230, `cell_hash` :276-308), `cell_store.rs` (`Vec<(ChunkCoords, Chunk)>`
  per cell), and `AddTerrainChunk(e, pose, Box<Chunk>)`. The client now derives bricks off
  thread from the chunk (§3.6).
- **Region assembly / volume.** `region.rs:673-700`, `volume.rs` (`SUB_BLOCKS_Y = CHUNK_BLOCKS`
  :116, `SubChunk`, `Column.base_sub_y` in chunk units). Vertical stacking is 16 m sub-chunks now.
  Check the surface-band rule in `columns_from_patch` (band reaches past the deepest soil and the
  cliff rule): with 16 m vertical granularity the band will usually be 1–2 chunks.
- **Constants derived from the old unit:**
  - `world_manager.rs:35-38` `PRISTINE_TERRAIN_CACHE_MAX`.
  - `protocol.rs:547-558` `MAX_DECOMPRESSED_PACKET_BYTES`.
  - `protocol.rs:92-114` the `REGION_CHUNKS>128 needs cells` assert. Restate it in blocks
    (256 blocks).
  - `cells.rs`, `islands.rs:16-17` `ISLAND_CELL_M` ("a 32-chunk cell").

### 3.4 Worldgen

- `lib.rs:124-160` `generate_region` / `generate_cell` windows and `lib.rs:187-320`
  `columns_from_patch`, whose per-column footprint is `CHUNK_BLOCKS²` (`nx = len*CHUNK_BLOCKS+2`).
  This is mechanical once the constants are honest.
- `lib.rs:341-420` `coarse_tile` and the LOD helpers (:538, :585-701) use 32 for the **LOD
  grid**, so they stay 32 but should use `BRICK_VOXELS`.
- **`cache.rs:121-176`**: the disk cache key covers seed + manifest + region_chunks + island +
  source files, not chunk size. Add `CHUNK_BLOCKS` to the key and bump the format, then delete
  the existing cache directory (no migration).
- `worldgen/tests/scene_hash_golden.rs:30` (`SCENE_HASH`, gated on `REGION_CHUNKS==32`, :47):
  re-gate on the new test region size and re-record.

### 3.5 LOD (server side)

LOD stays on 32³ brick grids. What changes is how chunks feed it:
- `lod.rs:144-200` `build_far_lod_cells` groups chunks 2×2×2 into one factor-2 grid, and
  `lod.rs:240-275` `block_scale_cells` assumes `CHUNK_BLOCKS = 2` (16×16-chunk tiles).
- Both should consume **brick views of a chunk**: add
  `Chunk::brick(bx, by, bz) -> BrickView` (blocks + chisel of one 2×2×2-block brick). That
  is the one adapter LOD, the client and snapshots share.
- `lod.rs:329-353` `LOD_RING_TABLE` matches on `REGION_CHUNKS {32, 64, 128}` and const-asserts
  a power of two ≥ 32. Re-key the arms on `REGION_BLOCKS {64, 128, 256}`. The ring table
  *values* (factors and bounds, in LOD units) should be unchanged.
- `lod_builder.rs:28,44,107` jobs carry `Vec<(ChunkCoords, Chunk)>`. That is fine; fewer,
  bigger items.

### 3.6 Client: one sim chunk → many render bricks

The bridge currently assumes one sim entity is one renderable 32³ thing. The changes:

- **The brick key is the world brick coordinate.** Many existing `translation / 32` keys
  (`meshing/queue.rs:149-151,325-327`, `visibility.rs`, `lod_rings.rs`) stay valid if they
  key bricks, not sim entities. `bridge.rs` keeps `sim chunk entity → set of brick entities`.
- **Spawning**: `AddTerrainChunk` and the join snapshot derive bricks off the main thread
  (budgeted, like cell chunks today). Skip all-air and fully buried bricks: the
  buried-skip already exists for chunks; apply it per brick. Uniform chunks short-circuit.
- **Edits**: the new `SetBlocks` update re-derives the touched bricks plus face neighbours for
  AO and faces (`meshing/neighbors.rs:58-132`, 26-neighbour padded shell).
- **`bridge.rs:117-230`**: the VoxelData RLE is per brick. The direct-from-blocks encoder
  (:163-200) assumes `edge = BLOCK_VOXELS * CHUNK_BLOCKS` and two 16-voxel halves per row.
  Change it to `BRICK_VOXELS`.
- **The commander mirror follows the sim constant silently.** `commander/height.rs:19`
  (`ChunkTops [u8; CHUNK_BLOCKS²]`, heights 1..=32) and `commander/tiles.rs:27,56,224,239`
  (TileMirror, `chunk_key = translation / CHUNK_EDGE`) use
  `CHUNK_EDGE = BLOCK_VOXELS * CHUNK_BLOCKS`. Decide per use: TileMirror could mirror sim
  chunks (now 16 m) directly. Height tops per block column need `u8` heights up to 16 blocks
  or an i16.
- **`voxel_pull/`**: per-brick `GpuNearChunkMeta` and `chunk_transforms` (one mat4 each) are
  unchanged in kind. The bricks of one sim chunk share a pose, so one transform slot per
  sim chunk plus a per-brick offset is a possible later saving, not needed now.
- Also: `grid_debug.rs:77` `CHUNK_SIZE = 32.0` (show both grids?), `meshing/mod.rs:157` AABB,
  `slice_plane.rs:178`, `instance.rs:369`.

### 3.7 Wire

- `SetVoxelComponent(EntityKey, Option<Vec<Voxel>>)` is no longer used for terrain (§3.3).
  Keep it only for non-terrain voxel entities, such as the trees' models later.
- **The join snapshot is `SerializedRegion`** (bincode of the whole `Rollback`): the format
  changes, which is fine with no migration. Expect it to shrink: fewer entities, bodies and
  colliders, and uniform chunks. **Measure** raw and lz4 size at 256 m against the 578 MiB /
  0.9 MiB numbers.

## 4. Tests and goldens that change (re-record on purpose, say so in the commit)

- `game/tests/wasm_determinism.rs:39-50`: six pinned u32 goldens (scene digest, state hash,
  CELL_HASH, underground). Re-record, then `just determinism-wasm` must agree on both targets.
- `game/tests/fixtures/physics_goldens/*.csv` (8). These are **a correctness check, not just
  churn**. Collider geometry is equivalent, so trajectories should be unchanged except where a
  seam moved. `walk_flat_across_chunk_seam.csv` now crosses a block seam inside one chunk.
  Re-record only the ones that differ, and explain each diff.
- `worldgen/tests/scene_hash_golden.rs`. Re-gate and re-record.
- Hardcoded chunk coords / 32 literals: game tests `chunk_blocks`, `collider_genesis`,
  `cell_activation`, `terrain_snapshot`, `spawn_surface`, `sunk_body`,
  `region_from_chunks/columns`, `lod_streaming`, `block_edit`, `random_ops`, `hash_mode`,
  `underground_edit.rs:50` (own `locate()`); `worldgen/tests/underground.rs:18`;
  `physics-next/tests/common/mod.rs:14,23`; `cell_store.rs:284-341`; `cache.rs:223-246`;
  client `bridge.rs:1297,1767-1828`, `lod_rings.rs:685,837,1009,1103`, meshing / visibility
  `x+32*y+1024*z` asserts (these are brick-level and mostly stay).
- `ChunkCoords` appears in 39 `.rs` files (22 of them game tests) and `ColumnCoords` in 30.
  Most are mechanical.

## 5. Suggested order (each step compiles and passes its targeted tests)

1. **Name the two 32s.** Introduce `BRICK_VOXELS` / `BRICK_BLOCKS` / `BrickShape` and replace
   every literal 32 by the constant it means, with `CHUNK_BLOCKS` still 2. Pure refactor, so
   **all goldens must stay identical**. That is the proof the split is right before anything
   changes size. Also replace the `/32` coordinate recovery (§3.2) here.
2. **Region / cell dials in blocks** (`REGION_BLOCKS`, `CELL_BLOCKS`), still at 2-block chunks.
   Goldens identical. Update the justfile, cargo config, run.sh and CLAUDE.md.
3. **Types that can't hold 16** (`BlockIndex u16`, `ColumnGround` vectors, `ChunkBlocks`
   uniform/dense). Goldens change only by serialization, which is expected; re-record wasm
   goldens.
4. **Edit path**: `SetBlocks` block-delta update, incremental `set_voxel` colliders,
   per-block undo. At 2-block chunks this is behaviour-neutral, so goldens stay identical.
5. **Client brick fan-out** (sim entity → bricks, off-thread derive, per-brick edit remesh).
   Still neutral at 2 blocks (1 brick per chunk).
6. **Flip `CHUNK_BLOCKS` to 16.** Fix the fallout, re-record goldens with explanations, and run
   `just smoke`, `just test-threads`, `just determinism-wasm`, the physics goldens and
   `just pull-smoke` on a real adapter. **Ask Dawid** before the measurement run (region start,
   snapshot size, entity count, edit latency, join `world_ready`) at 256 m, `world=tiny` and
   `full`.

Steps 1–5 make the flip a one-line change and keep every intermediate state verifiable
against unchanged goldens.

## 6. Open questions for Dawid

- **16 blocks is the floor he named. Is 16 the target, or try 32?** Larger chunks mean fewer
  entities but bigger edit units and coarser streaming / cell granularity. Recommend 16,
  measured first, with the dial kept.
- **Vertical size the same as horizontal (16 m)?** Cubic is simplest and matches the request.
  Taller chunks would cut vertical stacking further but materialise more implied ground per
  break.
- **The minimum cell / region size** becomes one chunk (16 m). Fine for cells of 16–64 blocks?

---

## 7. Investigation addendum (2026-10-29, verified against HEAD `ffb1f6e`)

A read-only sweep of every claim above. No `crates/game`, `worldgen`, `server` or
`physics-next` file changed since `548f7f1`, so the line refs still hold apart from the
corrections below. **The direction stands** (split sim chunk / 32³ render brick, 6-step order).
What follows are corrections, things the sweep found that §3 missed, and changes to the
step order.

### 7.1 Corrections to §1–§6

- **parry `set_voxel`** is `crates/parry/src/shape/voxels/voxels_edition.rs:118-175`. Lines
  45-67 are doc examples.
- **`Block` is 6 B** (`BlockId(u16)` + `[u8;3]`, padded), so a dense 16³ chunk is ~24 KiB, not 20 KiB.
- **`SetVoxelComponent` is not wire.** `GameDataUpdateKind` (`state.rs:44-50`) has no serde.
  It is the in-process render-sink message. §3.7 is really "client/sink", not protocol.
- **`ISLAND_CELL_M`** is in `crates/worldgen/src/islands.rs:16`, a fixed 64.0 m. Only its comment
  ("a 32-chunk cell") is stale.
- **The worldgen cache has no format number.** Add `CHUNK_BLOCKS` (and the block-region size) to
  the key in `cache.rs:147-176`. The cache dir is `target/worldgen-cache/`; clear it with `just worldgen-cache-clear`.
- **Commander heights are in voxels** (`height.rs:54`, `(y+1) as u8`), i.e. 1..=256 at 16 blocks.
  They need `u16`, not "u8 up to 16 blocks".
- `slice_plane.rs:178` and `instance.rs:369` are region-level (`REGION_BLOCKS` /
  `REGION_SIZE`), not chunk sites. The recent cutaway / working-level / fortress commits
  (2b7055b..ffb1f6e) use only `BLOCK_VOXELS` / `REGION_BLOCKS` / `REGION_SIZE`. Nothing there
  bakes in 2-block chunks.
- **`REGION_BLOCKS` already exists**: `pub const REGION_BLOCKS: i32` in `crates/game/src/orders.rs:32`
  (= `REGION_CHUNKS * CHUNK_BLOCKS`), used by `fortress_view.rs`, `slice_plane.rs` and
  `commander/designate.rs`. The new dial must absorb it, not collide with it.
- §3.2 is half done. `GameData.chunk_index` (`state.rs:194-198`) is coords→entity. What is
  missing is the reverse direction, which every `/32` site recovers from the body translation.

### 7.2 The main hazard: constants that change meaning silently (step 2)

These are written in 32-voxel units. They compile fine at `CHUNK_BLOCKS = 16` and
misbehave at runtime:

| Site | What goes wrong at 16 |
|---|---|
| `protocol.rs:47` `REGION_SIZE = REGION_CHUNKS * 32` | 8× too small |
| `protocol.rs:553` `region_chunks / 32` in `max_decompressed_packet_bytes` | cap becomes **0**, so every snapshot is rejected |
| `world_manager.rs:37` `PRISTINE_TERRAIN_CACHE_MAX` | 4096 entries, ceiling gone |
| `protocol.rs:98-101` "> 128 needs cells" | lets 2048 m whole-built regions through |
| `cells.rs:38-40` `cell_size_units = cell_chunks * 32` | wrong cell size |
| `lod.rs:307-310` `LARGE_HOME_FACTOR` vs `CELL_CHUNKS` | assumes factor-f grid = f chunks |
| client `world_scale.rs:18` `REGION_CHUNKS >= 128 → near radius 0` | play builds flip to a **5×5 window of 256 m regions** |
| client `meshing/visibility.rs:161` `CULL_BFS_MAX_CHEBYSHEV = …*REGION_CHUNKS` (brick units) | BFS covers 1/8 of the window; chunks hidden |
| `lod.rs:329-353` `LOD_RING_TABLE` match | the only one that fails loudly (assert at :353) |

Step 2 should restate each of these in blocks or units AND add a const-assert or unit test that
pins its value at today's sizes. Those pins are what make the step-6 flip safe.

### 7.3 Sim sites §3 missed

- **Dense whole-chunk voxel builds in the sim** (32 MiB each at 256³, `Voxel` = 2 B):
  - `edit.rs:177` `spawn_chunk_safe`: every materialised ground chunk, up to 7 per break.
  - `edit.rs:318` new voxels for the render update.
  - `edit.rs:321-324`: the **old** dense voxels captured in the `emit_on_undo` closure, so they
    live in the undo log.
  - `state.rs:855` `surface_voxel`: per chunk in the column. It is a **tick hot path**, because slime
    spawning calls it (`slime.rs:270`).

  Rule for step 4: no sim code derives voxels for a whole chunk.
- **Undo mechanism.** Chunks are `Ecs.chunk: Component<Chunk>` (`state.rs:219`, a
  `SparseSecondaryMap`, not `#[undo(map)]`). `Component::set_safe` (`state.rs:529-541`) clones the
  whole old `Chunk` per edit. That is the thing to replace with a per-block delta.
- **Silent wraps:** `BlockIndex(idx as u8)` at `block.rs:318` and `:377` aliases indices past 255.
  It does not panic.
- `ground.rs:16` `#[derive(Default)]` on `[i16; 256]` breaks, and `ColumnGround::plain(surface: [i16; FOOTPRINT])`
  (`:32`) changes signature. Callers include client `tiles.rs:297`.
- `state.rs:981` `remove_entity` deletes from `chunk_index` via `chunk_coords_of` (`/32`). The
  ordering comment there depends on the body still existing.
- `volume.rs:46` `SubChunk::uniform` keeps non-air uniform chunks as `Dense(Box<Chunk>)`. So
  `ChunkBlocks::Uniform` is what makes all-stone sub-chunks cheap.
- Stale docs: `block.rs:77` (ChiselData), `block.rs:366-369` (`derive_voxels` "sim (collider)"),
  `volume.rs:1,29` ("32³ SubChunk").

### 7.4 Colliders: answers to §3.3's open question

- parry `Voxels` is **sparse**: a `HashMap` of 8³ chunks of `u8` states (512 B each) plus a BVH.
  Memory follows occupied 8³ chunks, not domain size.
  - **Block scale** over a 16³ grid: ~8 parry chunks, ~4 KiB.
  - **Voxel scale**: chisel octants are 8-aligned, so each one is exactly one parry chunk. That is
    ≤ 4 KiB per chiseled block.
  - **Verdict:** one voxel-scale shape per sim chunk is viable. No need for a collider per chiseled block.
- **`set_voxel` cost:** each state-changing call runs `recompute_content_hash()`
  (`voxels.rs:586-600`, the mmo-fork digest). It sorts all chunk keys and hashes every chunk, so
  it is O(occupied chunks) per voxel. That is fine for one block edit and quadratic for bulk
  chisel edits. Add a batch API or defer the rehash to the end of the edit.
- **Copy-on-write:** `mutate_collider_safe` (`state.rs:463`) clones the `Collider`, so the shape
  Arc is shared. Editing it through `SharedShape::make_mut` then deep-clones the whole
  `Voxels`. That is cheap at block scale but costs MBs on chisel-heavy shapes. It is also the
  natural undo: keep the old Arc.
- **Hash caveat for step 4:** a `set_voxel`-edited shape may not be bit-identical in
  `StateHash` to a freshly built one (bucket layout, a domain that is never shrunk, the BVH).
  Client and server run the same path, so they agree. But "goldens stay identical" in step 4
  only holds if the hash walk is canonical. Check that first, or expect step 4 to
  re-record the hashed goldens.

### 7.5 Per-tick win §1 didn't list

`StepJournal` clones the broad phase and the narrow phase **wholesale every journaled step**
(`crates/rapier/src/pipeline/step_journal.rs:194-196`). Both hold an entry per collider (the
narrow phase's `graph_indices: Coarena`, and the BVH leaves). At 256 m that is up to ~3 × 50k
terrain colliders cloned per tick. At 16 m it is about 2 orders of magnitude fewer. This is
unmeasured; add the per-tick journal clone time to the step-6 measurement list.

Rough entity estimate: today ≈ 50k chunks / 16 384 columns ≈ 3 sub-chunks per 2 m column.
At 16 m there are 256 columns × 1–3 sub-chunks ≈ **300–800 chunk entities per 256 m region**.

### 7.6 Client: the fan-out surface is larger than §3.6 says

The "chunk entity → brick set" map does not exist; `SimEntityMap` is `(region,key) → Entity`
(`bridge.rs:696,761`). Every one of these treats one sim entity as one 32³ renderable today:
- `VoxelData` / `PendingSnapshotVoxels`, the `SnapshotVoxelTask` batches (`SNAPSHOT_VOXEL_TASK_CHUNKS`
  = 64, which becomes 32k bricks a task; budgets must count bricks).
- `ChunkGrid` / `ChunkKey` / `BoundaryZones`, `RemeshQueue.dirty`, `MeshingTask`, `ChunkQuads` /
  `Aabb` / `ChunkConnectivity`.
- `lod_rings.rs:154-185` `reveal_meshed_cells` compares meshed-entity counts with **sim** chunk
  counts (`instance.rs:936`), and `lod_rings.rs:513-555` assumes `ChildOf` = region root.
- `interpolate.rs:29` skips `Without<ChunkKey>`. If `ChunkKey` moves to bricks, sim chunk entities
  get interpolated every frame.
- `queue.rs:320-328` (and `height.rs:193`, `tiles.rs:238`) key from `root + local translation`.
  A brick parented to its chunk entity gets a key without the region offset.
- `bridge.rs:789-799` keyed `SetEntityPosition` snap, and `join_timeline.rs` `meshed_chunks`.
- `HeightGrid.by_entity`, `TileMirror.chunk_of`. Both index brick-layout `VoxelData` with the
  **sim** `CHUNK_EDGE`.

**Recommended shape:** brick entities are `ChildOf(region root)` siblings with region-local
translation, `VoxelData` and `ChunkKey`. The sim chunk entity keeps `SimEntity` plus a
`Bricks(Vec<Entity>)` and has no `ChunkKey`. Retarget the reveal and near-takeover counts to bricks.
Feed height and tiles from the sim chunk's blocks, not from voxels.

Two more hazards:
- **`VoxelData` RLE runs are `u16`** (`bridge.rs:136,154,175`). A uniform 256³ chunk is a single
  16.7M-voxel run, which panics in debug and wraps in release. `VoxelData` must stay per brick.
- **Terrain edits re-encode a dense `SetVoxelComponent` on the main thread** (`bridge.rs:815-821`).
  That is a 32 MiB main-thread stall per click at 16 m. `SetBlocks` is a hard prerequisite of
  the flip.

### 7.7 Tests and fixtures §4 missed

- **game:** `walking_smoothness.rs:63,178,205,289`, `physics_goldens.rs:94` (`world_of(3..=4, 3..=17)`,
  `flat_floor(32)`), `shape_cast_grazing.rs:125-127` (z = 32 seam), `handoff_scan.rs:125-136`,
  `log_model.rs:216`, `orders.rs:133`, `slime.rs:133`, `rollback_restore.rs:115`,
  `region_coords.rs:6`, `cell_activation.rs:305,395,756`, `cell_manager.rs:24`, `orders.rs:216`.
- **Seam / corner walks:** `handoff_schedule.rs:69-84` and `threaded_world.rs:43-58` hardcode floor
  columns `near = 0..=4`, `far = 30..=31`. A 64-block region has 4 chunk columns, so those floors
  vanish. The walk distance is unchanged (still 1024 units). Restate the floors in blocks or units.
  The spawn (128, 128) is no longer a column seam (comment at `threaded_world.rs:39`).
- **physics-next:** `walking_smoothness.rs:28,33` and `sunk_body.rs:25,112` build chunk rows 3..=5
  around (128, 128). At 256-unit chunks the spawn lands in chunk 0 and `Walker::new`'s grounded
  assert fails. `continuous_surfaces.rs:24` hardcodes `CHUNK = 2.0*BLOCK`. The
  `common/mod.rs:15-16` comment is wrong at 16.
- **worldgen:** `forged_world.rs:167,252`, `underground.rs:18-172`, `cache.rs:243`. Examples:
  `export_region_colliders.rs`, `cross_section.rs:81-104`, `cell_profile.rs` (`--cells` in chunks),
  `quantize_probe.rs:44-47`.
- **LOD:** `lod.rs:92-121` `downsample_blocks` and `:174-181` hardcode 2×2×2 blocks + `as u8`. Tests at
  `:1100-1135` use 32-voxel-chunk identities, and the `REGION_CHUNKS > 128` gates at `:1021,1043,1053`.
- **client:** `bridge.rs:1297` (coords up to x=31 in a 4-chunk test region), `tiles.rs:297-298`,
  `lod_rings.rs:685` comment, `mesh_bench.rs:36-38`, `lod_rings.rs:139`.

### 7.8 Changes to the step order (§5)

1. **Step 1** also adds the entity→coords direction: a `ChunkCoords` component, or iterate
   `chunk_index` in `terrain_snapshot` / `surface_voxel`. Then delete all five `/32` recoveries,
   including `remove_entity`'s.
2. **Step 2** restates every §7.2 constant and pins it with an assert or test at today's
   sizes. It also merges `orders.rs::REGION_BLOCKS` into the dial and restates the
   seam/corner-walk and physics-next fixtures in blocks or units. This is the step that
   prevents the zero packet cap and the 5×5 near window.
3. **Step 3:** `ChunkBlocks::{Uniform, Dense}` should also replace `SubChunk::uniform`'s
   `Dense(Box<Chunk>)`. Fix the `as u8` casts with the `u16` change.
4. **Step 4:** add "no whole-chunk `derive_voxels` in the sim" (`spawn_chunk_safe`, both edit
   directions, `surface_voxel`) and replace `Component::set_safe`'s whole-`Chunk` undo. Add a
   batched `set_voxel` (deferred content hash). Hashed goldens may move (§7.4 caveat).
5. **Step 5:** use the brick shape from §7.6. Commander heights go to `u16` here, and
   height/tiles are fed from blocks.
6. **Step 6:** add the per-tick journal clone time (§7.5) to the measurement list (ask first).
