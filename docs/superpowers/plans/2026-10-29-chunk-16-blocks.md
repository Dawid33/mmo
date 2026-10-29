# Sim Chunk 16³ Blocks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Grow the sim `Chunk` from 2×2×2 blocks (32³ voxels, 2 m) to 16×16×16 blocks (256³
voxels, 16 m). It stays one entity, one fixed body and one collider set per chunk. The client
keeps meshing, drawing and LODing 32³-voxel render **bricks** unchanged.

**Architecture:** Tasks 1–10 run with `CHUNK_BLOCKS` still 2 and are behaviour-neutral. Each one
must leave every golden identical, except Task 5, which changes serialization on purpose. They
separate the brick from the sim chunk and move the dials to blocks. They also remove every
whole-chunk dense voxel build and every whole-chunk undo or rebuild. Task 11 flips
`CHUNK_BLOCKS` to 16 and re-records the goldens, with an explanation for each.

**Tech Stack:** Rust stable 1.96 workspace; `game` (rollback sim, rapier/parry forks), `worldgen`,
`server`, Bevy 0.19.1 `client`.

**Spec:** `docs/superpowers/plans/2026-10-29-chunk-size-increase-handoff.md`. Read it whole,
**especially §7, the investigation addendum**, which holds the verified file:line inventory this
plan cites as "H§x".

## Global Constraints

- Work on `develop` in the main checkout. **No git worktrees.**
- **Never commit Dawid's uncommitted work.** Run `git status` before every commit and
  `git add` only the files the task changed. At planning time `CLAUDE.md`, `justfile`,
  `Cargo.lock` and several client/slint files carried his uncommitted edits. If a task must
  touch one of those files and it is still dirty with his hunks, stop and ask before
  committing it.
- Target values: `CHUNK_BLOCKS = 16` (cubic chunk). `BRICK_VOXELS = 32`. `BRICK_BLOCKS = 2`.
- Region dial in blocks: the test default is **64** blocks and the play default is **256**. The
  legal values are the powers of two 64..=65536. Above 256, cell mode is required.
- Every literal 32 must name which 32 it is: `BRICK_VOXELS` (render/LOD grid) or `CHUNK_UNITS`
  (= `CHUNK_BLOCKS * BLOCK_VOXELS`, the sim-chunk edge in world units).
- The sim must **never** build a dense voxel array for a whole chunk: no `derive_voxels` over a
  `Chunk` and no `Vec<Voxel>` of `CHUNK_*_COUNT` in `crates/game/src`. Dense voxel arrays are a
  per-brick thing.
- Every `ChunkBlocks` value is canonical: a dense chunk whose blocks are all equal is stored as
  `Uniform`, so equal chunks hash and serialize equally.
- No migration. Delete the worldgen disk cache with `just worldgen-cache-clear`. The
  `worlds/default.world` file stays valid.
- Verify with `just test-one <crate> "<target> <filter>"`, `just smoke`, `just test-threads` and
  `just determinism-wasm`. **Never** run whole suites (`just test` / `test-full`) without asking.
  Client test binaries are a slow opt-in, so ask once before the first one. Headless client
  tests must call `set_lod_table(&[])` and `set_near_radius(0)`. Tag any new test slower than
  100 ms with `#[ignore = "slow: >100ms — run via just test-full"]`.
- **Ask before any benchmark or perf run.** Dawid runs his own on this box.
- Keep the justfile, `.cargo/config.toml [env]`, `scripts/run.sh` and CLAUDE.md in sync in the
  same change as any dial change.
- Determinism: no inherent `f32` transcendental in a sim path. Hash with `StateHash`, never with
  std `Hash` or serialized bytes. After touching parry, run `just determinism-wasm` and the
  physics-next suite.

## Review Focus

1. **Stale old dial in the environment.** `VOXEL_REGION_CHUNKS=128 just run` (old muscle memory) or
   an exported `VOXEL_CELL_CHUNKS` must **fail the build** with a message naming the new
   variable. It must not silently build the default size. *Test: Task 3, step 5.*
2. **A unit constant that changes meaning silently.** At every legal `REGION_BLOCKS`, the packet
   cap must be > 0 and ≥ a whole-region snapshot. The pristine cache must hold ≥ 1 entry and keep
   its ~2.7 GB ceiling. The near radius must be 0 at ≥ 256 blocks. *Test: Task 3,
   `unit_constants_hold_at_every_legal_region_size`.*
3. **A break on a sim-chunk seam at 16 blocks.** The brick across the seam (in the neighbouring
   chunk entity) must remesh. Implied ground across the seam must materialise. Undo must
   restore both chunks bit-exactly. *Tests: Task 6 `break_on_chunk_seam_undoes_exactly`, Task 9
   `edit_on_chunk_seam_remeshes_neighbour_brick`.*
4. **Digging a whole chunk to air, then undoing.** The chunk must normalise to
   `ChunkBlocks::Uniform(air)` and hash equal to a fresh all-air chunk. Undo must restore
   the original `Dense` value bit-exactly. *Tests: Task 5, Task 6.*
5. **A cell size that is not a whole number of chunks** (e.g. `cells=8` blocks after the flip). It
   must fail the build. *Test: Task 3, step 5.*

---

## File Map

| File | Responsibility after this plan |
|---|---|
| `crates/game/src/block.rs` | `BLOCK_VOXELS`, `CHUNK_BLOCKS` (the dial), `CHUNK_UNITS`, `BRICK_*`, `BlockIndex(u16)`, `ColliderCells`, `collider_cells(&Chunk)`, `derive_brick_voxels` |
| `crates/game/src/voxel.rs` | `BrickShape`, `BRICK_VOXEL_COUNT`, `Chunk { blocks: ChunkBlocks, chisel }`, `ChunkBlocks`, `BrickView`, `Chunk::brick`, `Chunk::column_top` |
| `crates/game/src/protocol.rs` | `REGION_BLOCKS` dial + choices, the derived `REGION_CHUNKS`, `REGION_SIZE`, `CELL_BLOCKS`, `CELL_CHUNKS`, packet cap in blocks |
| `crates/game/src/state.rs` | `chunk_coords_from_translation`, `GameDataUpdateKind::SetBlocks`, `Undo<Component<Chunk>>::set_block_safe` |
| `crates/game/src/edit.rs` | the block-delta edit path, incremental collider edit |
| `crates/game/tests/chunk_units.rs` (new) | pins every unit constant |
| `crates/parry/src/shape/voxels/voxels_edition.rs` | `Voxels::set_voxels` batch (an `mmo-fork` hunk) |
| `crates/client/src/renderer/bricks.rs` (new) | `ChunkBricks`, `BrickOf`, sim-chunk → brick fan-out, brick-level derive tasks |
| `crates/client/src/renderer/bridge.rs` | routes `AddTerrainChunk` / `SetBlocks` to `bricks.rs`; `VoxelData` stays per brick |

---

### Task 1: Name the two 32s

**Files:**
- Modify: `crates/game/src/block.rs:13-26`, `crates/game/src/voxel.rs:9-11`, `crates/game/src/lib.rs` (re-exports)
- Modify all users of `ChunkShape` / `CHUNK_VOXEL_COUNT` (game `lod.rs`, `block.rs`; client `bridge.rs`, `meshing/*`, `commander/height.rs`; worldgen `lib.rs`). Find them with `grep -rn "ChunkShape\|CHUNK_VOXEL_COUNT" crates`.
- Modify: every literal 32 classed as (b) "sim-chunk edge" in the H§7 sweep: `state.rs:626,670,686-694,1409,1426-1428`, `edit.rs:180-182`, `cells.rs:40,211-213,320-322`.
- Test: `crates/game/tests/chunk_units.rs` (new)

**Interfaces:**
- Produces in `game` (crate root re-exports):
  - `pub const CHUNK_BLOCKS: usize = 2;` (no longer derived from 32; Task 11 sets it to 16)
  - `pub const CHUNK_UNITS: usize = CHUNK_BLOCKS * BLOCK_VOXELS;` (sim-chunk edge in voxels / world units)
  - `pub const BRICK_VOXELS: usize = 32;`, `pub const BRICK_BLOCKS: usize = BRICK_VOXELS / BLOCK_VOXELS;`
  - `pub type BrickShape = ConstShape3u32<32, 32, 32>;`, `pub const BRICK_VOXEL_COUNT: usize = BRICK_VOXELS.pow(3);`
  - `pub const BRICKS_PER_CHUNK_EDGE: usize = CHUNK_UNITS / BRICK_VOXELS;`
  - `pub fn chunk_coords_from_translation(t: Vector) -> ChunkCoords` in `state.rs`. Divides by
    `CHUNK_UNITS` with `floor`. It is the only place a chunk's coords are recovered from a body.
- `ChunkShape` / `CHUNK_VOXEL_COUNT` are deleted. They are not kept as aliases, so the compiler
  finds every user.

- [ ] **Step 1: Write the failing test** `crates/game/tests/chunk_units.rs`:

```rust
#[test]
fn chunk_edge_is_a_whole_number_of_bricks() {
    assert_eq!(game::CHUNK_UNITS % game::BRICK_VOXELS, 0);
    assert_eq!(game::BRICKS_PER_CHUNK_EDGE * game::BRICK_VOXELS, game::CHUNK_UNITS);
    assert_eq!(game::BRICK_BLOCKS * game::BLOCK_VOXELS, game::BRICK_VOXELS);
}

#[test]
fn chunk_coords_round_trip_through_body_translation() {
    for c in [game::ChunkCoords::new(0, 0, 0), game::ChunkCoords::new(3, -5, 7)] {
        let u = game::CHUNK_UNITS as f32;
        let t = game::Vector::new(c.x as f32 * u, c.y as f32 * u, c.z as f32 * u);
        assert_eq!(game::chunk_coords_from_translation(t), c);
    }
}
```

- [ ] **Step 2: Run** `just test-one game "--test chunk_units"`. Expected: FAIL to compile (`CHUNK_UNITS` undefined).
- [ ] **Step 3: Add the constants. Delete `ChunkShape` / `CHUNK_VOXEL_COUNT`, rename every user, and route `cells.rs::chunk_coords_of`, `chunk_terrain_term` and `terrain_snapshot` through `chunk_coords_from_translation`.** Body placement `coords * 32` becomes `coords * CHUNK_UNITS`. The genesis full-cube collider becomes `cuboid(h, h, h)` translated by `h`, with `h = CHUNK_UNITS as f32 / 2.0`. Also add `const _: () = assert!(CHUNK_UNITS % BRICK_VOXELS == 0);` in `block.rs`.
- [ ] **Step 4: Run** `just test-one game "--test chunk_units"` (PASS). Then `just smoke` (PASS). Then `just determinism-wasm` and `just test-one game "--test physics_goldens"`, both PASS **with no golden changed**. This is a pure rename.
- [ ] **Step 5: Commit** `refactor(game): name the brick 32 and the chunk 32 apart`.

---

### Task 2: No dense whole-chunk voxels in the sim

**Files:**
- Modify: `crates/game/src/state.rs:837-866` (`surface_voxel`), `crates/game/src/edit.rs:176-194` (`spawn_chunk_safe`), `crates/game/src/voxel.rs`
- Test: `crates/game/tests/spawn_surface.rs`, `crates/game/tests/chunk_blocks.rs`

**Interfaces:**
- Produces: `impl Chunk { pub fn column_top(&self, vx: u32, vz: u32) -> Option<(u32, BlockId)> }`.
  `vx`/`vz` are 0..CHUNK_UNITS voxel coords in the chunk. It returns the topmost non-air voxel's
  local Y and material, read from blocks plus chisel without building a voxel array.
- `surface_voxel` uses `chunk_index.range((cx, cz, i32::MIN)..=(cx, cz, i32::MAX))` instead of
  scanning every chunk. It uses `CHUNK_UNITS` for its div/rem.
- `spawn_chunk_safe` sends `GameDataUpdateKind::AddTerrainChunk(e, pose, Box::new(chunk.clone()))`
  instead of `SetEntityPosition` + `SetVoxelComponent`. The client already derives these off
  thread (`bridge.rs:749`). Undo still emits `RemoveEntity` through `create_mesh`'s entity insert.

- [ ] **Step 1: Write the failing test** in `chunk_blocks.rs`, `column_top_matches_derived_voxels`. For `Chunk::flat_floor(depth)` with depth ∈ {0, 1, 8, 16, 17, CHUNK_UNITS}, and for a chunk holding one chiseled half block, check every (vx, vz). `column_top` must equal the topmost non-air entry of `derive_voxels(..)` in that column. Keep `derive_voxels` as the test-only oracle.
- [ ] **Step 2: Run** `just test-one game "--test chunk_blocks column_top"`. Expected: FAIL (no `column_top`).
- [ ] **Step 3: Implement `column_top`, rewrite `surface_voxel` on it, and switch `spawn_chunk_safe` to `AddTerrainChunk`.**
- [ ] **Step 4: Run** `just test-one game "--test chunk_blocks"`, `just test-one game "--test spawn_surface"`, `just test-one game "--test underground_edit"`, `just test-one game "--test slime"` (all PASS). Then `just determinism-wasm` (unchanged). Then ask before the client opt-in `just test-one client "--lib bridge::"` (PASS: runtime chunks still render).
- [ ] **Step 5:** `grep -n "derive_voxels" crates/game/src` shows only `block.rs` (its definition) and `#[cfg(test)]` code. Commit: `refactor(game): sim reads chunk surfaces from blocks, never derives chunk voxels`.

---

### Task 3: Region and cell dials in blocks

**Files:**
- Modify: `crates/game/src/protocol.rs:30-120,540-560`, `crates/game/src/orders.rs:32` (delete its `REGION_BLOCKS`, import the dial), `crates/worldgen/src/lib.rs:34` (delete the private copy), `crates/game/src/world_manager.rs:28-39`, `crates/game/src/cells.rs:31-66`, `crates/game/src/lod.rs:300-353`, `crates/client/src/world_scale.rs:18`, `crates/client/src/renderer/meshing/visibility.rs:155-165`, `crates/server/src/lib.rs:352-362`
- Modify: `justfile:43-77` plus every `VOXEL_REGION_CHUNKS={{play_chunks}}` recipe line (28 of them), `.cargo/config.toml [env]`, `scripts/run.sh:9-10`, CLAUDE.md (region-size and cell paragraphs), the worldgen examples `cell_profile.rs` and `quantize_probe.rs` (doc strings / `--cells`)
- Test: `crates/game/tests/chunk_units.rs`

**Interfaces:**
- Produces in `game`:
  - `pub const REGION_BLOCKS: usize = parse_region_blocks(option_env!("VOXEL_REGION_BLOCKS"));`, default 64
  - `pub const REGION_BLOCKS_CHOICES: [usize; 11] = [64, 128, …, 65536];`
  - `pub const REGION_CHUNKS: usize = REGION_BLOCKS / CHUNK_BLOCKS;` (derived, so it still means sim-chunk columns)
  - `pub const REGION_SIZE: f32 = (REGION_BLOCKS * BLOCK_VOXELS) as f32;`
  - `pub const REGION_BRICKS: usize = REGION_BLOCKS / BRICK_BLOCKS;` (for LOD and client brick math)
  - `pub const CELL_BLOCKS: usize` from `VOXEL_CELL_BLOCKS` (0 = off). It must be a power of two,
    a multiple of `CHUNK_BLOCKS`, and divide `REGION_BLOCKS`. `pub const CELL_CHUNKS: usize = CELL_BLOCKS / CHUNK_BLOCKS;`
    `CellState.cell_chunks` stays in chunks.
  - `pub(crate) const fn max_decompressed_packet_bytes(region_blocks: usize) -> usize` = 256 MiB × (region_blocks / 64)², clamped as today.
  - `PRISTINE_TERRAIN_CACHE_MAX` = `max(1, 64 * 64² / REGION_BLOCKS²)`.
  - `cells.rs`: `cell_size_units = cell_chunks * CHUNK_UNITS`.
  - `lod.rs`: `LARGE_HOME_FACTOR` compares against `CELL_BLOCKS / BRICK_BLOCKS`. The `LOD_RING_TABLE`
    arms are keyed on `REGION_BLOCKS` {64, 128, 256}, with `_` = the placeholder `[(1, 32)]`. The
    assert becomes `REGION_BLOCKS.is_power_of_two() && REGION_BLOCKS >= 64`.
  - Asserts: the ">256 blocks needs cells" assert replaces the >128-chunk one.
- Client: `DEFAULT_NEAR_RADIUS = if REGION_BLOCKS >= 256 { 0 } else { 2 }`, and
  `CULL_BFS_MAX_CHEBYSHEV = (MAX_NEAR_RADIUS + 1) * REGION_BRICKS`.
- justfile: `region_blocks` / `test_blocks` = "64" / `play_blocks` = "256", exported as
  `VOXEL_REGION_BLOCKS`, and `cells` exported as `VOXEL_CELL_BLOCKS`. The CLAUDE.md example
  `region_chunks=1024 cells=16` becomes `region_blocks=2048 cells=32`.

- [ ] **Step 1: Write the failing test** `unit_constants_hold_at_every_legal_region_size` in `chunk_units.rs`. For each `b` in `REGION_BLOCKS_CHOICES`:
  - `max_decompressed_packet_bytes(b) > 0`;
  - for `b ≤ 256`, `max_decompressed_packet_bytes(b) >= 256 << 20` (the 64-block cap);
  - `pristine_cache_max(b) >= 1` (make it a `pub const fn pristine_cache_max(region_blocks)` that the constant calls).

  Also pin the build's values: `assert_eq!(REGION_SIZE, (REGION_BLOCKS * BLOCK_VOXELS) as f32)`, `assert_eq!(REGION_CHUNKS * CHUNK_BLOCKS, REGION_BLOCKS)`, `assert_eq!(REGION_BRICKS * BRICK_VOXELS as usize, REGION_SIZE as usize)`.

  Then pin today's values at the test default: `max_decompressed_packet_bytes(64) == 256 << 20`, `pristine_cache_max(64) == 64`, `pristine_cache_max(128) == 16`. These equal today's `(32)` and `(64)` results.
- [ ] **Step 2: Run** `just test-one game "--test chunk_units"`. Expected: FAIL to compile (`REGION_BLOCKS_CHOICES`).
- [ ] **Step 3: Implement the dial and restate every listed constant.** Also replace the `region_coords.rs:6` identity with `REGION_SIZE == REGION_BLOCKS * BLOCK_VOXELS`. In `lod.rs` tests, `:1100-1135` and `:1021,1043,1053`, restate the chunk identities in bricks (`REGION_BRICKS`).
- [ ] **Step 4: Run** `just test-one game "--test chunk_units"`, `"--test region_coords"`, `"--test lod_streaming"`, `just test-one game "--lib lod::"` (all PASS). Then `just smoke` (PASS) and `just determinism-wasm` (unchanged: 64 blocks = today's 32 chunks).
- [ ] **Step 5: Check the Review Focus 1 and 5 build failures by hand.** In `parse_region_blocks` / `parse_cell_blocks`, add `assert!(option_env!("VOXEL_REGION_CHUNKS").is_none(), "VOXEL_REGION_CHUNKS is gone: use VOXEL_REGION_BLOCKS (blocks, 64..=65536); just region_blocks=N")` and the same for `VOXEL_CELL_CHUNKS`. Delete those two keys from `.cargo/config.toml`. Run `VOXEL_REGION_CHUNKS=32 just check`; expected: the build fails with that message. Run `just cells=3 check`; expected: it fails with the `VOXEL_CELL_BLOCKS` message.
- [ ] **Step 6: Check the play default.** `just --dry-run server` shows `VOXEL_REGION_BLOCKS=256`. `just --dry-run test-one game x` shows 64.
- [ ] **Step 7: Commit** `feat(build): region and cell dials in blocks (VOXEL_REGION_BLOCKS / VOXEL_CELL_BLOCKS)`. Respect the dirty-file rule for `justfile` / CLAUDE.md.

---

### Task 4: Test fixtures independent of chunk size

**Files:**
- Modify: `crates/game/tests/handoff_schedule.rs:69-84`, `crates/server/tests/threaded_world.rs:39-58,399,457,514`, `crates/game/tests/{walking_smoothness.rs:63,178,205,289, physics_goldens.rs:94, shape_cast_grazing.rs:125-127, handoff_scan.rs:125-136, log_model.rs:216, orders.rs:133, slime.rs:133, rollback_restore.rs:115, cell_activation.rs:305,395,756, cell_manager.rs:24, underground_edit.rs:50}`, `crates/game/tests/common/`, `crates/physics-next/tests/{common/mod.rs:14-23, walking_smoothness.rs:28,33, sunk_body.rs:25,112, continuous_surfaces.rs:24}`, `crates/worldgen/tests/{underground.rs, forged_world.rs:167,252}`
- Test: the files themselves

**Interfaces:**
- Produces in `crates/game/tests/common/mod.rs` (and a twin in physics-next `common`):
  - `pub fn floor_chunks(x_units: Range<f32>, z_units: Range<f32>, depth: u32) -> Vec<(ChunkCoords, Chunk)>`:
    every chunk whose footprint overlaps the unit rectangle, each `Chunk::flat_floor(depth)`.
  - `pub fn chunk_of(units: Vector) -> ChunkCoords`, which uses `chunk_coords_from_translation`.
- Every fixture states its floor in **world units** or **blocks**, never in chunk indices. For
  example, the seam walks' `near = 0..=4` / `far = 30..=31` become unit ranges
  `[0, 5·64)` / `[REGION_SIZE − 128, REGION_SIZE)`. Seam positions are `REGION_SIZE` or a block
  edge, not `32.0`. A test whose point is a *chunk* seam uses `CHUNK_UNITS`. A test whose point
  is a *block* seam uses `BLOCK_VOXELS`. Decide per test from its doc comment and say which in
  the commit.

- [ ] **Step 1: Prove the fixtures depend on the chunk size.** Temporarily set `CHUNK_BLOCKS = 4` locally (do not commit). Run `just test-one game "--test handoff_schedule -- --include-ignored"` and `just test-one physics-next "--test walking_smoothness"`. Expected: FAIL (floors missing or grounded assert). Revert.
- [ ] **Step 2: Add the helpers and rewrite each listed fixture in units.**
- [ ] **Step 3: With `CHUNK_BLOCKS = 2`, run** `just test-one game "--test physics_goldens"` and `just test-one game "--test walking_smoothness"` (PASS, goldens unchanged). Then `just test-threads` (PASS). Then `just test-one physics-next ""` (PASS).
- [ ] **Step 4: Repeat step 1 with `CHUNK_BLOCKS = 4`.** The fixture tests now PASS, apart from the hashed goldens (expected to differ, not checked here). Revert.
- [ ] **Step 5: Commit** `test: state terrain fixtures in units, not chunk indices`.

---

### Task 5: Types that can hold 16³

**Files:**
- Modify: `crates/game/src/block.rs:28-46,318,377` (`BlockIndex`), `crates/game/src/voxel.rs:13-90` (`Chunk`), `crates/game/src/ground.rs:11-40`, `crates/game/src/volume.rs:46` (`SubChunk::uniform`), every `chunk.blocks[…]` / `&chunk.blocks, &chunk.chisel` user (game, worldgen, client `bridge.rs:163-200`, `lod.rs:92-121,174-181`)
- Test: `crates/game/tests/chunk_blocks.rs`, `crates/game/tests/ground.rs`, `crates/game/tests/wasm_determinism.rs:39-50` (re-record), `crates/worldgen/tests/scene_hash_golden.rs` (re-gate on `REGION_BLOCKS == 64`, re-record)

**Interfaces:**
- `pub struct BlockIndex(pub u16)`. Every `as u8` becomes `BlockIndex::from_linear(i: usize) -> BlockIndex`, which debug-asserts `i < CHUNK_BLOCK_COUNT`.
- In `voxel.rs`:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
  pub enum ChunkBlocks { Uniform(Block), Dense(Box<[Block]>) }
  impl ChunkBlocks {
      pub fn from_vec(blocks: Vec<Block>) -> Self;  // normalises; asserts len == CHUNK_BLOCK_COUNT
      pub fn get(&self, i: BlockIndex) -> Block;
      pub fn set(&mut self, i: BlockIndex, b: Block); // Uniform→Dense on a differing write; Dense→Uniform when all equal
      pub fn uniform(&self) -> Option<Block>;
      pub fn iter(&self) -> impl Iterator<Item = (BlockIndex, Block)> + '_;
  }
  pub struct Chunk { pub blocks: ChunkBlocks, pub chisel: BTreeMap<BlockIndex, ChiselData> }
  ```
  `Chunk::block_at`, `set_block`, `uniform`, `is_uniform` and `is_all_air` keep their signatures.
  `is_uniform` is O(1) on `Uniform`. `collider_cells(chunk: &Chunk) -> ColliderCells` and
  `derive_voxels(chunk: &Chunk)` take the chunk instead of `(&[Block], &BTreeMap)`.
  `collider_cells` emits one block key per solid block on a `Uniform` chunk without iterating chisel.
- `ColumnGround.surface` / `.geo`: `Box<[i16]>`, with the length checked to equal `FOOTPRINT` on
  deserialize (`#[serde(deserialize_with = …)]`). Drop `#[derive(Default)]` and add a manual
  `Default` of `FOOTPRINT` zeros. `plain(surface: Box<[i16]>, …)`.
- `SubChunk::uniform` stores `Uniform` chunks through `ChunkBlocks::Uniform`; no `Dense(Box<Chunk>)` for a uniform value.

- [ ] **Step 1: Write the failing tests** in `chunk_blocks.rs`:
  - `chunk_blocks_normalise_to_uniform`: `from_vec(vec![stone; N])` is `Uniform(stone)`. Set one block to air, then back to stone; the result `== ChunkBlocks::Uniform(stone)` and `state_hash` equals the fresh value's.
  - `dense_round_trips_bincode_and_hash`: a chunk from `Chunk::flat_floor(17)` survives `encode_packet`/`decode_packet` and keeps its `state_hash`.
  - `block_index_covers_chunk`: for every `i < CHUNK_BLOCK_COUNT`, `BlockIndex::from_xyz(BlockIndex::from_linear(i).xyz()) == from_linear(i)`.
- [ ] **Step 2: Run** `just test-one game "--test chunk_blocks"`. Expected: FAIL to compile.
- [ ] **Step 3: Implement the types and update every user.** In `VoxelData::from_blocks` (client), switch the two-halves-per-row assumption to per-brick input for now. Its full brick form arrives in Task 9; here keep `edge = BRICK_VOXELS` and read through `Chunk::block_at`.
- [ ] **Step 4: Run** `just test-one game "--test chunk_blocks"`, `"--test ground"`, `"--test collider_genesis"`, `"--test block_edit"`, `"--test underground_edit"` (PASS). Then `just test-one game "--test physics_goldens"` (PASS, **unchanged**: geometry is the same).
- [ ] **Step 5: Re-record the hashed goldens.** Run `just determinism-wasm` and record the new values in `wasm_determinism.rs:39-50`; both targets must agree. Run `just test-one worldgen "--test scene_hash_golden"` and re-record it. Run `just worldgen-cache-clear`. Add `CHUNK_BLOCKS` and `REGION_BLOCKS` to the key in `crates/worldgen/src/cache.rs:147-176`.
- [ ] **Step 6: Run** `just smoke` (PASS). Commit `feat(game): BlockIndex u16, uniform/dense ChunkBlocks, boxed ColumnGround`. The commit message says *"re-recorded wasm determinism + scene hash goldens: serialization of BlockIndex/ChunkBlocks/ColumnGround changed, geometry unchanged (physics goldens identical)"*.

---

### Task 6: Block-delta edits and per-block undo

**Files:**
- Modify: `crates/game/src/state.rs:44-60` (`GameDataUpdateKind`), `:515-545` (`Undo<Component<T>>`), `crates/game/src/edit.rs:294-331` (`write_block_safe`)
- Modify: `crates/client/src/renderer/bridge.rs:815-835` (handle `SetBlocks`)
- Test: `crates/game/tests/block_edit.rs`, `crates/game/tests/underground_edit.rs`, `crates/game/tests/random_ops.rs`

**Interfaces:**
- `pub struct BlockDelta { pub index: BlockIndex, pub block: Block, pub chisel: Option<ChiselData> }`, deriving `Clone, Debug, PartialEq`.
- `GameDataUpdateKind::SetBlocks(EntityKey, Vec<BlockDelta>)`. Each entry is a block's new full
  state. The undo direction sends the old states.
- `impl Undo<Component<Chunk>> { pub fn set_block_safe(&mut self, key: EntityKey, index: BlockIndex, block: Block, chisel: Option<ChiselData>) }`.
  The undo closure restores only the old `Block` and the old chisel entry for `index`, through
  `ChunkBlocks::set`, so normalisation is LIFO-exact. The digest is
  `component_slot_digest(key, …)` as in `set_safe`.
- `write_block_safe` builds no `old.clone()`, no `derive_voxels`, and sends no `SetVoxelComponent`.
  It emits `SetBlocks(e, vec![new])` on Do and registers `SetBlocks(e, vec![old])` via
  `emit_on_undo` before the write (LIFO, as today).
- Client (minimal, until Task 9): on `SetBlocks`, `VoxelData::edit` the mapped entity, writing
  each delta's 16³ voxels (block id, or the chisel materials) at its block offset.

- [ ] **Step 1: Write the failing tests:**
  - `edit.rs` lib unit test `edit_never_clones_the_chunk`. Make `Chunk`'s `Clone` a manual impl
    that bumps a `#[cfg(test)]` thread-local `CHUNK_CLONES` counter. A `set_block_safe` break on
    an existing chunk, followed by its undo, must leave the counter unchanged. Today's
    `old.clone()` + `set_safe` clones twice. Run it with `just test-one game "--lib edit::"`.
  - `block_edit.rs::break_on_chunk_seam_undoes_exactly`: break the block at local x =
    `CHUNK_BLOCKS - 1` beside implied ground, so the neighbour chunk materialises. Assert
    `state_hash` before == after undo, under `HashMode::Full`.
  - `block_edit.rs::dig_chunk_to_air_and_undo`: break every solid block of a seeded chunk in one
    transaction. The chunk is `ChunkBlocks::Uniform(air)`. Undo; `state_hash` equals before.
  - `block_edit.rs::edit_emits_set_blocks`: the render sink receives exactly
    `SetBlocks(e, [BlockDelta { index, block: air, chisel: None }])` on Do and the old delta on
    Undo, and no `SetVoxelComponent`.
- [ ] **Step 2: Run** `just test-one game "--test block_edit"` and `just test-one game "--lib edit::"`. Expected: FAIL.
- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run** `just test-one game "--test block_edit"`, `"--test underground_edit"`, `"--test random_ops"`, `"--test hash_mode"`, `"--test orders"` (PASS). Then `just determinism-wasm` (unchanged: state is identical, only render messages changed). Then ask, and run `just test-one client "--lib bridge::"` (PASS, edits render).
- [ ] **Step 5: Commit** `feat(game): block-delta edits (SetBlocks) with per-block undo`.

---

### Task 7: Incremental collider edits

**Files:**
- Modify: `crates/parry/src/shape/voxels/voxels_edition.rs:118-175`, `crates/parry/src/shape/voxels/voxels.rs:586-600` (the `mmo-fork` content hash)
- Modify: `crates/game/src/edit.rs:342-383` (`rebuild_chunk_colliders_safe` stays as the fallback), `write_block_safe`
- Test: `crates/physics-next/tests/voxels_batch_edit.rs` (new), `crates/game/tests/block_edit.rs`

**Interfaces:**
- parry: `impl Voxels { pub fn set_voxels(&mut self, edits: impl IntoIterator<Item = (IVector, bool)>) }`.
  It has the same effect as calling `set_voxel` per item, with **one** `recompute_content_hash()`
  at the end. Mark it with `// mmo-fork: batched set_voxel, one content-hash pass`.
- Requirement: a shape edited with `set_voxels` must `StateHash` equal to a fresh
  `Voxels::new(size, &keys)` with the same final key set. If parry's layout makes them differ
  (unshrunk domain, chunk map or BVH order), change the fork's `StateHash` for `Voxels` to hash
  the canonical content (`content_hash` + voxel size + sorted occupied keys) instead of the
  layout, in the same task.
- game: `fn edit_block_colliders_safe(&mut self, e: EntityKey, old: &ColliderCells, new: &ColliderCells)`.
  It diffs per grid and applies `set_voxels` via `mutate_collider_safe` (the Arc copy-on-write
  is the undo). It falls back to `rebuild_chunk_colliders_safe` when the set of non-empty
  grids changes, or when the body's collider is the genesis cuboid. `collider_cells` is computed
  for the touched block only, via `collider_cells_of_block(chunk: &Chunk, i: BlockIndex) -> ColliderCells`
  with chunk-local keys.

- [ ] **Step 1: Write the failing tests:**
  - physics-next `voxels_batch_edit.rs::batch_equals_per_voxel_and_fresh`: from a random seeded
    key set, apply 200 random toggles three ways: per-voxel `set_voxel`, one `set_voxels`, and a
    fresh build of the final set. All three are equal in `StateHash` and in contact results for a
    capsule shape-cast grid.
  - game `block_edit.rs::incremental_collider_matches_rebuild`: break and place 50 seeded blocks;
    after each, the chunk's colliders `StateHash` equal a `create_mesh` of the same chunk.
- [ ] **Step 2: Run** `just test-one physics-next "--test voxels_batch_edit"`. Expected: FAIL (no `set_voxels`).
- [ ] **Step 3: Implement `set_voxels` (and the canonical hash if needed), then `edit_block_colliders_safe`.**
- [ ] **Step 4: Run** `just test-one physics-next ""`, `just physics-next-upstream`, `just test-one game "--test block_edit"`, `"--test physics_goldens"`, `"--test shape_cast_grazing"`, `"--test walking_smoothness"` (PASS). Then `just determinism-wasm`. If the canonical-hash change moved the pinned values, re-record them and say so in the commit; otherwise they stay unchanged.
- [ ] **Step 5: Commit** `feat(physics): batched Voxels::set_voxels; edits patch colliders instead of rebuilding`.

---

### Task 8: LOD and worldgen read bricks of a chunk

**Files:**
- Modify: `crates/game/src/voxel.rs` (`BrickView`), `crates/game/src/lod.rs:8,92-121,143-209,226-264,537`, `crates/game/src/lod_builder.rs:28-112`, `crates/worldgen/src/lib.rs:124-320,341-420`, `crates/worldgen/src/islands.rs:16-17,115,230`, `crates/worldgen/src/cache.rs:121-138`
- Test: `crates/game/tests/lod_streaming.rs`, `just test-one game "--lib lod::"`, `crates/worldgen/tests/`

**Interfaces:**
- ```rust
  pub struct BrickView<'a> { chunk: &'a Chunk, origin: [usize; 3] /* in blocks */ }
  impl Chunk {
      pub fn brick(&self, b: [usize; 3]) -> BrickView<'_>;           // b in 0..BRICKS_PER_CHUNK_EDGE
      pub fn bricks(&self) -> impl Iterator<Item = ([usize; 3], BrickView<'_>)>;
  }
  impl BrickView<'_> {
      pub fn block_at(&self, bx: usize, by: usize, bz: usize) -> Block;  // 0..BRICK_BLOCKS
      pub fn chisel(&self, bx: usize, by: usize, bz: usize) -> Option<&ChiselData>;
      pub fn uniform(&self) -> Option<Block>;  // O(1) on a Uniform chunk
      pub fn voxels(&self) -> Vec<Voxel>;      // dense BRICK_VOXEL_COUNT — the one allowed dense form
  }
  pub fn brick_coords(chunk: ChunkCoords, b: [usize; 3]) -> [i64; 3]; // world brick coords
  ```
- `build_far_lod_cells` and `block_scale_cells` iterate `chunk.bricks()` and key groups by world
  brick coords / 2 and / 16. `downsample_blocks` reads `BrickView`. `CHUNK_VOXEL_COUNT` users
  become `BRICK_VOXEL_COUNT`. A `Uniform` chunk fills or skips its bricks without visiting blocks.
- worldgen: `columns_from_patch`'s footprint `nx = len * CHUNK_BLOCKS + 2` follows the constant. In
  `coarse_*`, the 32s become `BRICK_VOXELS`. `ISLAND_CELL_M` gets its comment fixed (64 m),
  value unchanged. `islands.rs` uses `REGION_BLOCKS`. `fits_this_build` compares with
  `REGION_CHUNKS²` columns (derived).

- [ ] **Step 1: Write the failing test** `lod_streaming.rs::far_lod_from_bricks_matches_reference`. For a seeded region (`generate_region`), the factor-2/4/16 grids from the new brick path equal the grids from `downsample_reference` (keep today's implementation as `#[cfg(test)] fn downsample_reference`). Compare byte for byte via `PackedLodGrid`.
- [ ] **Step 2: Run** `just test-one game "--test lod_streaming far_lod_from_bricks"`. Expected: FAIL (no `Chunk::bricks`).
- [ ] **Step 3: Implement `BrickView` and port the LOD builders and worldgen.**
- [ ] **Step 4: Run** `just test-one game "--test lod_streaming"`, `just test-one game "--lib lod::"`, `just test-one worldgen ""` (PASS). Then `just determinism-wasm` (unchanged). Run `just world-map` and compare the PNG with the pre-task one; the files must be identical (`cmp`).
- [ ] **Step 5: Commit** `refactor(lod,worldgen): consume chunks as 32³ brick views`.

---

### Task 9: Client fan-out, one sim chunk to many bricks

**Files:**
- Create: `crates/client/src/renderer/bricks.rs`
- Modify: `crates/client/src/renderer/bridge.rs:300-475,696-835,938-952`, `meshing/queue.rs:19-35,213-264,320-352`, `meshing/mod.rs:72,138-185`, `interpolate.rs:29`, `lod_rings.rs:154-185,513-555`, `instance.rs:936`, `join_timeline.rs:104,163-177`, `grid_debug.rs:77`
- Test: `crates/client/src/renderer/bricks.rs` (unit tests), `bridge.rs` test module, `crates/client/tests/` (the existing convergence harness)

**Interfaces:**
- `#[derive(Component)] pub struct ChunkBricks(pub Vec<Entity>);` sits on the sim chunk entity.
  That entity keeps `SimEntity`, has **no** `VoxelData` / `ChunkKey`, and is not interpolated.
- `#[derive(Component)] pub struct BrickOf { pub chunk: Entity, pub local: [u8; 3] }`. Each brick
  entity is a `ChildOf(region root)` sibling with a region-local `Transform` (chunk translation
  plus `local * BRICK_VOXELS`), plus `VoxelData`, `ChunkKey` and the existing mesh components.
  The existing grid, neighbour gather and remesh queue see bricks exactly as they see chunks today.
- `pub fn spawn_chunk_bricks(chunk: &Chunk) -> Vec<([u8; 3], VoxelData)>`, run inside the existing
  `SnapshotVoxelTask` (off main thread). It skips all-air bricks and fully buried bricks (the
  existing buried test, per brick). A `Uniform` chunk produces its brick list without
  per-voxel work.
- `VoxelData::from_brick(view: &BrickView) -> VoxelData` replaces `from_blocks`. Runs stay `u16`
  (≤ 32768 per brick).
- Budgets: `SNAPSHOT_VOXEL_TASK_CHUNKS` becomes `SNAPSHOT_VOXEL_TASK_BRICKS` (= 64), and
  the per-frame insert budgets count bricks.
- `SetBlocks(e, deltas)`: for each delta, `VoxelData::edit` the brick containing it (a missing
  brick is spawned when the block is non-air). Mark that brick and its face-neighbour bricks
  dirty when the block sits on the brick face, including bricks of another sim chunk found
  through `ChunkGrid`. All of this runs off the main thread when > 1 brick is touched.
- `SetEntityPosition` on a keyed chunk moves its bricks. `RemoveEntity` despawns them.
- `reveal_meshed_cells` / `near_regions_meshed` count **bricks**: the expected count comes from the
  bricks spawned for that cell's chunks (recorded in `ChunkBricks`), not `cell_chunk_keys().len()`.
  `join_timeline.meshed_chunks` becomes `meshed_bricks`.

- [ ] **Step 1: Write the failing tests** (unit tests in `bricks.rs`, no Bevy app):
  - `uniform_air_chunk_spawns_no_bricks`, `uniform_stone_chunk_buried_bricks_skipped`. A
    `flat_floor(n)` chunk yields `BRICKS_PER_CHUNK_EDGE²` × (bricks crossing the surface)
    bricks. At `CHUNK_BLOCKS = 2` that is exactly 1.
  - `from_brick_matches_dense_derive`: `VoxelData::from_brick(&chunk.brick(b)).decode() == chunk.brick(b).voxels()` for seeded chunks.
  - In the `bridge.rs` test module, `edit_on_chunk_seam_remeshes_neighbour_brick`: two adjacent sim
    chunks; a `SetBlocks` on the seam block marks the brick in the *other* chunk dirty.
- [ ] **Step 2:** Ask before running the client opt-in (slow). Then `just test-one client "--lib bricks::"`. Expected: FAIL.
- [ ] **Step 3: Implement the fan-out.**
- [ ] **Step 4: Run** `just test-one client "--lib bricks::"`, `"--lib bridge::"`, `"--lib meshing::"`, `"--lib lod_rings::"` (PASS). Then the convergence harness `just test-one client "--test crossing"` (PASS). Then `just smoke` (PASS).
- [ ] **Step 5: Commit** `feat(client): sim chunks fan out to 32³ render bricks`.

---

### Task 10: Commander mirrors read blocks

**Files:**
- Modify: `crates/client/src/renderer/commander/height.rs:19-80,190-200`, `commander/tiles.rs:27-70,216-241,297-298`
- Test: the unit tests in those files

**Interfaces:**
- `HeightGrid` tops are `u16` voxel heights (1..=`CHUNK_UNITS`). They are filled from sim chunk
  blocks via `Chunk::column_top` (Task 2) on `AddTerrainChunk` / `SetBlocks`, keyed by sim chunk
  entity. They no longer read `VoxelData`.
- `TileMirror` mirrors sim chunks: `chunk_key = chunk_coords_from_translation(region-local translation)`.
  It holds per-chunk `Box<[BlockId]>` built from `Chunk::block_at`, and on `SetBlocks` it updates
  only the delta's tiles, with no full-chunk rescan.

- [ ] **Step 1: Write the failing tests** `height_from_blocks_matches_voxels` and `tiles_update_from_set_blocks_touches_one_tile`. The second asserts that the mirror's change counter moved by exactly 1 tile.
- [ ] **Step 2: Run** `just test-one client "--lib commander::"`. Expected: FAIL.
- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run** `just test-one client "--lib commander::"` (PASS). Then `just smoke`.
- [ ] **Step 5: Commit** `refactor(client): commander height/tile mirrors read sim blocks`.

---

### Task 11: Flip `CHUNK_BLOCKS` to 16

**Files:**
- Modify: `crates/game/src/block.rs` (`CHUNK_BLOCKS = 16`)
- Re-record: `crates/game/tests/wasm_determinism.rs:39-50`, `crates/game/tests/fixtures/physics_goldens/*.csv` (only those that differ), `crates/worldgen/tests/scene_hash_golden.rs`
- Modify: CLAUDE.md (chunk/brick paragraph, region numbers), `crates/game/src/block.rs:77,366-369`, `crates/game/src/volume.rs:1,29` (stale docs)

- [ ] **Step 1: Flip the constant.** Run `just check`; fix whatever fallout the compiler shows.
- [ ] **Step 2: Run** `just test-one game "--test chunk_units"`, `"--test chunk_blocks"`, `"--test block_edit"`, `"--test underground_edit"`, `"--test collider_genesis"`, `"--test cell_activation"`, `"--test lod_streaming"`, `just test-one worldgen ""` (PASS without re-recording, since none of these pin hashes).
- [ ] **Step 3: Physics goldens.** Run `just test-one game "--test physics_goldens"`. For every CSV that differs, write one line in the commit: which seam disappeared, or which contact changed, and why that is expected. `walk_flat_across_chunk_seam.csv` now crosses a block seam inside one collider. Re-record only those files. Stop and ask Dawid if a diff has no explanation.
- [ ] **Step 4: Re-record** the wasm determinism (both targets must agree) and scene-hash goldens. Run `just worldgen-cache-clear`.
- [ ] **Step 5: Run** `just smoke`, `just test-threads`, `just determinism-wasm`, `just test-one physics-next ""`, `just physics-next-upstream` (all PASS). Ask, then run the client set from Task 9 step 4 (PASS).
- [ ] **Step 6: Live check on a real adapter** (`adapter=Intel`, functional only, not perf): `just pull-smoke`, then `just world=tiny run`. Confirm terrain renders, a break and a place render and collide, digging into implied ground works, and undo under rollback shows no hash mismatch in the log.
- [ ] **Step 7: Update CLAUDE.md** (chunk = 16 m sim unit, 32³ brick = render/LOD unit, region-dial numbers, cell example) and fix the stale docs. Commit `feat(game): sim chunks are 16³ blocks`, with the golden explanations from steps 3–4.

---

### Task 12: Measurement (ask Dawid first)

- [ ] **Step 1: Ask Dawid** whether and when to run the measurement. Do nothing in this task without a yes.
- [ ] **Step 2:** Measure at 256 m regions, `world=tiny` and `full`, on the adapter he names. Record these at this commit and at the last pre-plan commit (`ffb1f6e`):
  - region start (`[perf] event=region_genesis`);
  - snapshot raw / lz4 size (`snapshot_sent`);
  - chunk entity count;
  - per-tick step time including the journal clone (`[server]` counters);
  - edit latency (break to remesh);
  - `world_ready`.

  Use `just perf-capture`.
- [ ] **Step 3:** Write the numbers into `docs/superpowers/specs/2026-10-29-chunk-16-measurements.md`. Update the memory note `chunk-size-two-meters.md` to "landed". Commit.
