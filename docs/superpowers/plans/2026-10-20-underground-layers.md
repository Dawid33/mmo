# Underground Layers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every region becomes solid ground 60 blocks deep — soil, then the forge's stone layers, then unbreakable bedrock — stored lazily, on a forged world that the server forges on first start, with 256 m regions when playing.

**Architecture:** Each worldgen `Column` carries a small `ColumnGround` recipe (per-block-column surface + geology offset + the strata crossing it). At genesis the region keeps these recipes in `GameData.ground`; a missing chunk below a column's band is *implied ground* computed from the recipe. Breaking a block materialises any implied face-neighbour chunk inside the same undoable edit, so reachable air is always bordered by real chunks. The forge pipeline moves from an example into `worldgen::forge` so the server can forge the default world.

**Tech Stack:** Rust (stable 1.96), `game` (rollback sim, `#[rollback]`, `StateHash`), `worldgen`, `server`, Bevy client (render path unchanged), `just`.

**Spec:** `docs/superpowers/specs/2026-10-20-underground-layers-design.md` (approved 2026-10-20).

## Global Constraints

- `UNDERGROUND_DEPTH = 60` blocks below each block column's own surface; one level = one block.
- Layers only: soil (existing `material_at` rules, `MAX_SOIL_DEPTH = 3`), forge rock, bedrock. No caves, ores, aquifers, underground water.
- Underground evaluation is integer-only (no float at query time); native = wasm bit-exact.
- `game` stays Bevy-free and worldgen-free; no new dependencies in `game` or `client`.
- Bedrock cannot be broken.
- Region size 128 chunks (256 m) **for play recipes only**; `.cargo/config.toml` keeps `VOXEL_REGION_CHUNKS = "32"`; all tests pass at 32.
- At 128, the default near window is one region (near radius 0) plus LOD rings.
- Forged world is the server default; missing default file → forge it; unreadable existing file → fail loudly, never overwrite.
- Procedural stays: tests, wasm offline (`LocalServer`) and explicit selection; procedural ground = plain stone + bedrock.
- Cell mode (`VOXEL_CELL_CHUNKS > 0`) keeps today's band semantics (no implied ground); `cell_activation` / `cell_manager` stay green; `generate_cell` stays bit-identical to the region build.
- Pristine LOD output unchanged.
- Verify with `just test-one <crate> "<target filter>"`, `just smoke`, `just determinism-wasm`; client test binaries only with Dawid's OK; ask before timing runs. Tag any test >100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- Hash goldens re-recorded only in a commit whose message says why.
- Work on `develop` in the main checkout; no worktrees.

## Review Focus

1. **Digging at the region edge** — a break whose face-neighbour lies outside the region's columns must not panic and must not materialise anything outside (`locate` returns `None`). Test in Task 5.
2. **Rollback of a materialising dig** (a client mispredict) — undo must remove the materialised chunks, their bodies and render entities: `hash(before) == hash(after undo)`. Test in Task 5.
3. **Park → restore of a dug region** — `SerializedRegion` round trip keeps `ground` and materialised chunks; implied ground still answers below. Test in Task 4.
4. **Old or foreign world files** — an artifact naming an unknown rock, or with an old `ARTIFACT_VERSION`, fails loudly at load. Test in Task 7.
5. **Corrupt default world file** — the server stops with a message to delete it; the file is untouched. Test in Task 8.

---

### Task 1: Move the forge pipeline into `worldgen::forge`

**Files:**
- Create: `crates/worldgen/src/forge/{mod.rs,world.rs,geology.rs,climate.rs,erosion.rs,post.rs,hydrology.rs,spawn.rs,knobs.rs}` (moved from `crates/worldgen/examples/forge_proto/`)
- Modify: `crates/worldgen/src/lib.rs` (`pub mod forge;`), `crates/worldgen/examples/forge_proto/main.rs` (thin wrapper: CLI parse, render, search, serve stay in the example), `crates/worldgen/examples/forge_proto/{render,search,serve}.rs` (import from `worldgen::forge`), `Cargo.toml` (root: `[profile.dev.package.worldgen] opt-level = 3`)
- Test: `crates/worldgen/tests/forge_lib.rs`

**Interfaces:**
- Produces:
  - `worldgen::forge::ForgeParams { size_m: f64, cell_m: f64, seed: u64, knobs: Knobs, quiet: bool }` with `ForgeParams::default()` = today's `DEFAULT_SIZE_M`, `DEFAULT_CELL_M`, `DEFAULT_SEED`, `Knobs::default()`, `quiet: true`. The example's `Params` keeps `out`/`search`/`serve` and wraps a `ForgeParams`.
  - `worldgen::forge::forge(p: &ForgeParams) -> Forged`; `Forged::artifact(&self, p: &ForgeParams) -> WorldArtifact`.
  - `worldgen::forge::geology::ROCKS: [Rock; 6]` (pub, unchanged order: sandstone, shale, limestone, conglomerate, basalt, granite) and `pub fn rock_index(name: &str) -> Option<usize>`.
- The example used `#[path = "../../src/noise.rs"]`; the library module uses `crate::noise` instead.

- [ ] **Step 1: Write the failing test** `forge_small_world_round_trips` (tag slow if >100 ms): forge `ForgeParams { size_m: 2048.0, cell_m: 64.0, ..ForgeParams::default() }`, take `artifact()`, `to_bytes()` → `WorldArtifact::from_bytes` OK, `rock_names == ROCKS names`, `height.len() == n²`.
- [ ] **Step 2: Run** `just test-one worldgen "--test forge_lib -- --include-ignored"` → FAIL (no `worldgen::forge`).
- [ ] **Step 3: Move the modules** into `src/forge/`, make the pipeline items the example needs `pub`, keep behaviour byte-identical (same seed + knobs → same artifact bytes as before the move).
- [ ] **Step 4: Run** the test → PASS; run `just forge-proto --cell 64 --out target/world-forge/move-check` and confirm it writes the `.world` and PNGs as before.
- [ ] **Step 5: Commit** `refactor(worldgen): forge pipeline as a library module (worldgen::forge)`.

---

### Task 2: Rock and bedrock blocks; `Materials` knows them

**Files:**
- Modify: `assets/blocks/blocks.ron` (append ids 6–12: `bedrock`, `sandstone`, `shale`, `limestone`, `conglomerate`, `basalt`, `granite`, each `All("<name>.png")`)
- Create: `assets/blocks/{bedrock,sandstone,shale,limestone,conglomerate,basalt,granite}.png` (16×16 placeholders: forge `ROCKS` rgb with light per-texel noise; bedrock near-black)
- Modify: `crates/worldgen/src/material.rs`, every `Materials { … }` literal: `crates/server/src/lib.rs:307`, `crates/client/src/local_server.rs:201`, `crates/worldgen/src/main.rs`, `crates/worldgen/tests/{region_load_cost,forged_world}.rs`, `crates/worldgen/examples/{quantize_probe,collider_profile,lod_build_profile,column_bench,snapshot_size}.rs` (and any other the compiler finds)
- Test: `crates/worldgen/src/material.rs` (unit), `crates/worldgen/tests/forged_world.rs`

**Interfaces:**
- Produces: `Materials` gains `pub bedrock: BlockId` and `pub rocks: [BlockId; 6]` (indexed like `forge::geology::ROCKS`); `Materials::from_names(resolve: impl Fn(&str) -> BlockId) -> Materials` resolving `stone, dirt, grass, sand, water, bedrock` and each `ROCKS[i].name`. Server and `LocalServer` call it; test/example literals may keep literals with the new fields.

- [ ] **Step 1: Failing test** `manifest_defines_every_material` (unit, worldgen): parse the repo's `assets/blocks/blocks.ron` with `game::BlockRegistry::from_ron`, `Materials::from_names(|n| reg.id_of(n).unwrap())` → `bedrock == BlockId(6)`, `rocks == [7,8,9,10,11,12]`.
- [ ] **Step 2: Run** `just test-one worldgen "--lib material::"` → FAIL.
- [ ] **Step 3: Implement** the manifest entries, textures, fields and `from_names`; update all literal sites.
- [ ] **Step 4: Run** the test → PASS; `just smoke` → green (type-checks client and server).
- [ ] **Step 5: Commit** `feat(blocks): bedrock + six forge rock blocks; Materials resolves them`.

---

### Task 3: `game::ground` — the per-column ground recipe

**Files:**
- Create: `crates/game/src/ground.rs`
- Modify: `crates/game/src/lib.rs` (`mod ground; pub use ground::*;`), `crates/game/src/volume.rs` (`Column` field + accessors)
- Test: `crates/game/tests/ground.rs`

**Interfaces:**
- Produces:
  - `pub const UNDERGROUND_DEPTH: i32 = 60;`
  - `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, StateHash)] pub struct ColumnGround { pub surface: [i16; CHUNK_BLOCKS * CHUNK_BLOCKS], pub geo: [i16; CHUNK_BLOCKS * CHUNK_BLOCKS], pub strata: Vec<(i16, BlockId)>, pub bedrock: BlockId, pub band_base_sub_y: i32 }` — index `lx * CHUNK_BLOCKS + lz`; `strata` top-down by `top`, the last entry extends downward; `band_base_sub_y` = the column's lowest materialised sub-chunk Y at genesis.
  - `impl ColumnGround { pub fn plain(surface: [i16; 4], stone: BlockId, bedrock: BlockId) -> Self; pub fn block_at(&self, lx: usize, lz: usize, y: i32) -> Block; pub fn is_implied(&self, sub_y: i32) -> bool }`
  - `Column::with_ground(self, ground: ColumnGround) -> Column`, `Column::ground(&self) -> Option<&ColumnGround>` (serde field, default `None`; normalisation unchanged).
- `block_at` rule (integer): `if surface - y >= UNDERGROUND_DEPTH { bedrock } else { stratum at s = y - geo }`, with the forge's own semantics (`forge::geology::Geology::rock_at_s`): the last entry whose `top > s`, else the first entry (the top formation extends upward). `is_implied(sub_y)` is `sub_y < band_base_sub_y`. `block_at` is only ever asked about implied ground (deeper than soil), so it has no soil rules.

- [ ] **Step 1: Failing tests** in `tests/ground.rs`:
  - `bedrock_starts_sixty_below_each_columns_surface`: `plain([100, 90, 100, 100], …)` → `block_at(0,0,41).id == stone`, `block_at(0,0,40).id == bedrock`, `block_at(0,1,30).id == bedrock`, `block_at(0,1,31).id == stone`.
  - `strata_lookup_is_by_stratigraphic_coordinate`: `strata = [(10, A), (0, B), (-20, C)]`, `geo = [5; 4]`, `surface = [200; 4]` → `y=20 (s=15) → A`, `y=5 (s=0) → A`, `y=4 (s=-1) → B`, `y=-15 (s=-20) → B`, `y=-16 (s=-21) → C`, `y=-40 → C`.
  - `column_ground_survives_bincode`: `Column::new(…).with_ground(g)` round-trips through bincode with `ground() == Some(&g)`.
- [ ] **Step 2: Run** `just test-one game "--test ground"` → FAIL.
- [ ] **Step 3: Implement** `ground.rs` and the `Column` field.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(game): ColumnGround recipe for lazy underground`.

---

### Task 4: Genesis keeps the recipes; implied-ground lookup

**Files:**
- Modify: `crates/game/src/state.rs` (`GameData` field), `crates/game/src/region.rs:688` (`from_columns`), `crates/game/src/cells.rs` (next to `chunk_key_at`)
- Modify: `crates/game/tests/wasm_determinism.rs` (re-record the four constants: `GameData` gained a field)
- Test: `crates/game/tests/region_from_columns.rs`

**Interfaces:**
- Consumes: `ColumnGround`, `Column::ground()` (Task 3).
- Produces:
  - `GameData` field `#[undo(map)] ground: BTreeMap<ColumnCoords, ColumnGround>`, written only at genesis via `insert_raw` (same pattern as `chunk_index`), never mutated afterwards.
  - `Rollback::seed_ground_genesis(&mut self, ground: Vec<(ColumnCoords, ColumnGround)>)` (raw, unlogged); `Region::from_columns` calls it with every column's ground (columns without ground — test fixtures — contribute none).
  - `Rollback::implied_block(&self, coords: ChunkCoords, local: (usize, usize, usize)) -> Option<Block>`: `None` when a chunk entity exists, the region is in cell mode (`data.cells.cell_chunks != 0`), the column has no ground, or `!ground.is_implied(coords.y)`; else `Some(ground.block_at(local.0, local.2, coords.y * CHUNK_BLOCKS + local.1))`.

- [ ] **Step 1: Failing tests** in `region_from_columns.rs`:
  - `implied_ground_below_the_band`: a one-column region whose `ColumnGround::plain` surface is 10 and band base sub-Y 2 → `implied_block(ChunkCoords::new(0, 1, 0), (0,0,0))` is stone, `(0, -30, 0)` is bedrock, `(0, 2, 0)` (materialised) is `None`, `(0, 9, 0)` (above) is `None`.
  - `ground_survives_park_and_restore` (Review Focus 3): `SerializedRegion::from_rollback` → `to_rollback` keeps `ground` and `implied_block` answers identically; `state_hash` equal.
  - `cell_mode_has_no_implied_ground`: a `Region::new_cells` region → `implied_block` is `None`.
- [ ] **Step 2: Run** `just test-one game "--test region_from_columns"` → FAIL.
- [ ] **Step 3: Implement** the field, genesis seeding and `implied_block`.
- [ ] **Step 4: Run** → PASS; re-record `wasm_determinism` constants (`WASM_SCENE_HASH_PRINT=1 just test-one game "--test wasm_determinism -- --nocapture"`), then `just determinism-wasm` → PASS on both targets.
- [ ] **Step 5: Commit** `feat(game): regions keep per-column ground recipes; implied ground lookup` (message notes the golden re-record: new `GameData.ground` field).

---

### Task 5: Digging into implied ground

**Files:**
- Modify: `crates/game/src/edit.rs` (`block_solid`, `set_block_uncounted`; factor the chunk-spawn-with-render-updates code at `edit.rs:199-211` into a helper)
- Test: `crates/game/tests/underground_edit.rs`

**Interfaces:**
- Consumes: `implied_block`, `ColumnGround` (Tasks 3–4).
- Produces: `Rollback::materialize_ground_safe(&mut self, coords: ChunkCoords) -> Option<EntityKey>` — builds the chunk's 8 blocks from `implied_block`, inserts it with `create_mesh` (undoable; `RemoveEntity` on undo) and sends the pose + `SetVoxelComponent` updates exactly like the existing new-chunk path. Returns `None` if `implied_block` is `None` for that chunk.
- Behaviour:
  - `block_solid(pos)` with no chunk entity returns `implied_block(..).is_some_and(|b| b.id != AIR)`.
  - When an edit sets a block to AIR: first materialise the block's own chunk if it is implied; refuse (return `false`, change nothing) if the current block's id is that column's `ground.bedrock`; after the write, materialise each of the 6 face-neighbour chunks of `pos` that `locate`s inside the region and is implied — all in the same transaction.

- [ ] **Step 1: Failing tests** in `underground_edit.rs` (build regions with `Region::from_columns` over `ColumnGround::plain` columns, region-local block positions; use `set_block_safe` for digs and the existing `block_edit.rs` player helpers for one raycast case):
  - `breaking_the_band_floor_materialises_the_chunk_below`: break the lowest band block → the chunk below now has an entity, its top blocks are stone, and `block_solid` on it is true.
  - `exposure_invariant_after_random_digs` (seeded, 200 digs down a 4×4 shaft area, tag slow if >100 ms): after each dig, no air block in the region has a face-neighbour whose chunk is missing and implied.
  - `bedrock_cannot_be_broken`: dig to the floor; `set_block_safe(bedrock_pos, AIR)` returns `false` and the hash is unchanged.
  - `undo_of_a_materialising_dig_restores_the_hash` (Review Focus 2): `state_hash` before, dig that materialises 2 chunks, `rollback_last_event`, hash equal and the chunk entities are gone.
  - `digging_at_the_region_edge_does_not_reach_outside` (Review Focus 1): break a band-floor block in column `REGION_CHUNKS-1`; no panic; no chunk outside `0..REGION_CHUNKS` columns.
  - `cell_mode_digs_keep_band_semantics`: in a cell-mode region, breaking the band floor materialises nothing (today's behaviour).
- [ ] **Step 2: Run** `just test-one game "--test underground_edit"` → FAIL.
- [ ] **Step 3: Implement** in `edit.rs`.
- [ ] **Step 4: Run** → PASS; also `just test-one game "--test block_edit"` and `"--test cell_activation"` → PASS.
- [ ] **Step 5: Commit** `feat(game): digging materialises implied ground; bedrock is unbreakable`.

---

### Task 6: Worldgen emits ground recipes; cliffs and depth (procedural)

**Files:**
- Modify: `crates/worldgen/src/lib.rs` (`columns_from_patch`, `SCAN_BOTTOM`)
- Modify: `crates/worldgen/tests/scene_hash_golden.rs` (re-record)
- Test: `crates/worldgen/tests/underground.rs`

**Interfaces:**
- Consumes: `ColumnGround`, `Materials.bedrock` (Tasks 2–3).
- Produces: every `Column` from `generate_region` / `generate_cell` carries `with_ground(..)`: `surface` from the per-block-column surfaces already computed, `band_base_sub_y` = the final `base_sub_y`, and for procedural patches `geo = [0; 4]`, `strata = [(i16::MAX, mats.stone)]`. Solid blocks deeper than soil inside the band are written from `ColumnGround::block_at` so band and implied ground can never disagree (procedural: still stone — band output only changes by the cliff rule).
- **Cliff rule**: the band bottom also reaches down to `min(neighbour surface) + 1` over the 4 face-neighbour block columns of every block column in the footprint, sampled with `patch.sample`; a neighbour outside the patch window uses the footprint's own surface.
- `SCAN_BOTTOM` becomes `SEA_TOP_BLOCK - <deepest sea floor margin> - UNDERGROUND_DEPTH`, i.e. low enough that `surface - UNDERGROUND_DEPTH` is always inside the scan for any surface the generator can produce (derive from constants; no literal).

- [ ] **Step 1: Failing tests** in `underground.rs`:
  - `procedural_columns_carry_plain_ground`: `generate_region(r(0,0), WORLD_SEED, mats)` → every column has `ground()`, `strata.len() == 1`, and `block_at(.., surface - 60) == bedrock`.
  - `band_and_ground_agree`: for every materialised solid block deeper than soil in one generated region, the block equals `ground.block_at` at that position.
  - `cliffs_are_closed_at_genesis`: on a `ForgedWorld` synthetic artifact with a 10 m step (reuse `forged_world.rs` helpers), no generated air/water block has a face-neighbour in a missing implied chunk (same check as Task 5's invariant, applied to the generated columns).
  - `generate_cell_matches_region_slice` still holds (existing test).
- [ ] **Step 2: Run** `just test-one worldgen "--test underground"` → FAIL.
- [ ] **Step 3: Implement**; re-record `scene_hash_golden`.
- [ ] **Step 4: Run** `underground`, `scene_hash_golden`, `forged_world` → PASS; `just smoke` → green.
- [ ] **Step 5: Commit** `feat(worldgen): columns carry ground recipes; cliff faces closed at genesis` (message notes the golden re-record).

---

### Task 7: Forge geology in forged regions

**Files:**
- Modify: `crates/worldgen/src/forged.rs` (geology sampling, load-time validation), `crates/worldgen/src/lib.rs` (forged branch of `columns_from_patch` supplies geology)
- Test: `crates/worldgen/tests/forged_world.rs`

**Interfaces:**
- Consumes: `forge::geology::rock_index`, `Materials.rocks` (Tasks 1–2).
- Produces:
  - `ForgedWorld::geo_offset_blocks(&self, x_m: f64, z_m: f64) -> i16` = `round(uplifted + bedding)` bilinearly sampled from the artifact rasters at the engine point (via the existing engine→artifact mapping), plus the sea-level block (artifact metres are relative to sea level; block `SEA_TOP_BLOCK + 1` = 0 m), clamped to `i16`.
  - `ForgedWorld::strata_blocks(&self, mats: &Materials) -> Vec<(i16, BlockId)>`: artifact `column` tops rounded to blocks, rock index → `mats.rocks[rock_index(name)]`.
  - Forged columns' `ColumnGround`: `geo` per block column from `geo_offset_blocks` at block centres; `strata` = the entries whose range intersects `[min(surface) - UNDERGROUND_DEPTH - max(geo), max(surface) - min(geo)]` (in s), keeping one entry above and below so lookups stay total.
  - `ForgedWorld::load` / `from_bytes` fails with `ArtifactError::Format("unknown rock <name>")` when a `rock_names` entry has no `rock_index`.

- [ ] **Step 1: Failing tests** in `forged_world.rs`, on the synthetic artifact with `uplifted = 0`, `bedding = 0`, `column = [(-5.0, sandstone), (-25.0, shale)]`, `rock_names = ROCKS names`:
  - `shaft_crosses_soil_then_layers_then_bedrock`: with `geo = 64` (uplift and bedding 0, sea level at block 64), `s = y - 64`: pick a land column with surface `S` well above 39 and `S - 60 < 38`; blocks `S..=S-3` soil, `S-4..=39` sandstone (`s ≥ -25`), `38..` shale (`s ≤ -26`) down to `S-59`, bedrock at `S-60` and below — read through the band where materialised and `ground.block_at` below.
  - `unknown_rock_name_fails_loudly` (Review Focus 4): artifact with `rock_names[0] = "marble"` → `from_bytes` is `Err(Format(..))` naming it.
  - `old_artifact_version_fails_loudly` (Review Focus 4): `version = ARTIFACT_VERSION - 1` → `Err`.
- [ ] **Step 2: Run** `just test-one worldgen "--test forged_world"` → FAIL.
- [ ] **Step 3: Implement.** If geology fields require a format change, bump `ARTIFACT_VERSION` and note it in the commit.
- [ ] **Step 4: Run** → PASS; `just smoke` → green.
- [ ] **Step 5: Commit** `feat(worldgen): forged regions use the forge's stone layers`.

---

### Task 8: The forged world is the server default

**Files:**
- Create: `crates/server/src/world_file.rs`
- Modify: `crates/server/src/lib.rs` (`run()` resolves the world before building generators), `crates/worldgen/src/forged.rs` (install hook), `.gitignore` (`/worlds/`), `justfile` (`world_file` comment + `procedural` value), `CLAUDE.md` (world-file paragraph)
- Test: `crates/server/src/world_file.rs` (unit), `crates/worldgen/tests/forged_world.rs`

**Interfaces:**
- Consumes: `worldgen::forge::{forge, ForgeParams}` (Task 1).
- Produces:
  - `worldgen::forged::install(world: Option<ForgedWorld>)` — sets the `active()` `OnceLock` before first use; panics if `active()` was already initialised. `active()` keeps its env fallback for tools.
  - `server::world_file::{WorldSource, DEFAULT_WORLD_PATH, resolve, ensure_forged}`:
    - `enum WorldSource { Procedural, Forged(PathBuf) }`
    - `const DEFAULT_WORLD_PATH: &str = "worlds/default.world"` (relative to the repo root, resolved like the block manifest path).
    - `fn resolve(env_value: Option<&str>) -> WorldSource`: unset or `""` → `Forged(default)`; `"procedural"` → `Procedural`; anything else → `Forged(path)`.
    - `fn ensure_forged(path: &Path, params: &ForgeParams) -> Result<(), String>`: file exists → `Ok` without reading it; missing → create parent dirs, `forge` + `artifact().save(path)`, log `[perf] event=world_forged t_ms=… ms=… path=…`.
  - `run()`: `resolve(env)`; for `Forged(path)`: `ensure_forged`, then `ForgedWorld::load(path)` — on error panic with `"{path}: {e} — delete it to forge a new default world"` — then `forged::install(Some(world))`; for `Procedural`: `install(None)`.

- [ ] **Step 1: Failing tests:**
  - `resolve_defaults_to_the_forged_world`, `resolve_procedural_keyword`, `resolve_explicit_path` (unit, `world_file.rs`).
  - `ensure_forged_writes_once` (slow): tempdir path, `ForgeParams { size_m: 2048.0, cell_m: 64.0, .. }` → file appears and loads; second call leaves its mtime unchanged.
  - `corrupt_existing_world_is_not_overwritten` (Review Focus 5): write garbage to the path → `ensure_forged` is `Ok` and leaves the bytes untouched; `ForgedWorld::load` is `Err`.
- [ ] **Step 2: Run** `just test-one server "--lib world_file"` → FAIL.
- [ ] **Step 3: Implement**; update the justfile comment (`world_file` empty = default forged world, `procedural` = noise world) and CLAUDE.md.
- [ ] **Step 4: Run** → PASS (`-- --include-ignored` for the slow one); `just smoke` → green.
- [ ] **Step 5: Commit** `feat(server): forge and serve a default world file; procedural on request`.

---

### Task 9: 256 m regions when playing

**Files:**
- Modify: `justfile` (region-size variables; play recipes `server`, `client`, `run`, `server-release`, `client-release`, `run-release`, `web`, `web-release`, `diag`, `perf-capture`, `fly-probe`, `shuttle-probe`), `crates/client/src/world_scale.rs:16`, `CLAUDE.md` (region-size + world-size paragraphs)
- Test: `crates/client/src/world_scale.rs` existing unit tests (type-checked by smoke; running them is a client opt-in)

**Interfaces:**
- Justfile shape *(the spec leaves the expression to the builder; this is one that works)*: `region_chunks := env_var_or_default("VOXEL_REGION_CHUNKS", "")`; `test_chunks := if region_chunks == "" { "32" } else { region_chunks }`; `play_chunks := if region_chunks == "" { "128" } else { region_chunks }`; `export VOXEL_REGION_CHUNKS := test_chunks`; each play recipe's cargo/binary invocations get `VOXEL_REGION_CHUNKS={{play_chunks}}` (server and client of one recipe from the same value). `render-bench` stays at `test_chunks` so its baselines remain comparable. *(My call.)*
- `pub const DEFAULT_NEAR_RADIUS: i32 = if game::REGION_CHUNKS >= 128 { 0 } else { 2 };`

- [ ] **Step 1: Verify the recipe wiring:** `just --dry-run run` shows `VOXEL_REGION_CHUNKS=128` on both binaries; `just --dry-run smoke` and `just --dry-run test-one game ""` show 32; `just region_chunks=64 --dry-run run` shows 64.
- [ ] **Step 2: Implement** the justfile and `world_scale.rs` changes; update CLAUDE.md.
- [ ] **Step 3: Run** `just smoke` → green (still at 32).
- [ ] **Step 4: Commit** `build: play recipes default to 256 m regions with a one-region near window`.

---

### Task 10: Cross-section image

**Files:**
- Create: `crates/worldgen/examples/cross_section.rs`
- Modify: `justfile` (recipe `cross-section *args`, release build like `world-map`), `CLAUDE.md` (one line next to `just world-map`)

**Interfaces:**
- Consumes: `generate_region_forged` / `generate_region`, `Column` `ReadVol`, `ColumnGround::block_at`, `forge::geology::ROCKS` colours.
- Behaviour: `just cross-section [--world <file>] [--region X,Z] [--axis x|z] [--at <block>] [--out <png>]` (defaults: the default world file if present else procedural, region (0,0), axis x through the region centre, `target/cross-section/<world>-<region>.png`). One pixel per block, from the highest surface + 8 down to the deepest bedrock + 2: materialised blocks read from the column, deeper ones from `ground.block_at` — the engine's code, not a re-implementation. Colours: rocks from `ROCKS.rgb`, grass/dirt/sand/water/air/bedrock fixed. Prints the stratum sequence (block, top y, bottom y) of the centre block column.

- [ ] **Step 1: Run** `just cross-section --world worlds/default.world` → writes the PNG and prints the sequence; open the PNG and check soil → bands → bedrock following the terrain.
- [ ] **Step 2: Commit** `feat(worldgen): just cross-section — vertical slice of generated ground`.

---

### Task 11: Cross-target determinism of the underground, and the live check

**Files:**
- Modify: `crates/game/tests/wasm_determinism.rs` (new scene + constants)

**Interfaces:**
- Consumes: Tasks 3–5.

- [ ] **Step 1: Add the scene** `underground_scene()`: a 2×2-column region built with `Region::from_columns` from hand-written `ColumnGround`s with three strata, then a fixed sequence of 12 `set_block_safe` digs straight down that materialises chunks and ends on bedrock; returns `(crc32 of SerializedRegion bytes, state_hash)`; constants `UNDERGROUND_DIGEST`, `UNDERGROUND_STATE_HASH`; native + wasm test pair like the existing ones.
- [ ] **Step 2: Record and verify:** `WASM_SCENE_HASH_PRINT=1 just test-one game "--test wasm_determinism -- --nocapture"`, fill the constants, then `just determinism-wasm` → PASS on native and wasm.
- [ ] **Step 3: Commit** `test(game): pin the lazy underground across native and wasm`.
- [ ] **Step 4: Live check for Dawid** (spec "Verification"; functional run on `adapter=Intel`): delete `worlds/default.world` if present; `just adapter=Intel run` forges once and logs `world_forged`; the second `just run` loads without forging; with `VOXEL_CAMERA=first-person` dig a shaft on a hilltop and in a valley: soil, layer changes, bedrock that won't break, solid walls and floor at every depth; compare with `just cross-section` at the same spot. Ask Dawid before any timing measurement (spec point 3).

---

## Self-review notes

- Spec §1 → Tasks 3, 6, 7; §2 → Tasks 3–4; §3 → Tasks 4–5; §4 → Tasks 5–6; §5 → Tasks 2, 5; §6 → Task 2; §7 → Tasks 1, 6, 8; §8 → Task 9; §9 → Task 10; Constraints (determinism, cell mode, LOD, undo) → Tasks 4, 5, 11 and the Global Constraints; Verification → Task 11.
- LOD: no task changes `coarse_tile` or the LOD builder; Task 6's golden re-record covers region output only — if `lod_streaming` pristine tests move, that is a bug in Task 6, not a re-record.
