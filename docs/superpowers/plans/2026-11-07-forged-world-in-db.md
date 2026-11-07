# Forged World in the Database (tiled, pulled piecewise) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A world's forge output lives in the world store as small global tables plus tiled rasters. `ForgedWorld` samples through a tile cache that loads only the tiles it touches, and every sample is bit-identical to today's whole-raster sampling. A host's memory then scales with its territory and view, not the world size. Today that is ~1 GiB per process for 100 km / 16 m.

**Architecture:**
- `ForgedWorld`'s raster reads go through a `Rasters` enum: `Whole` (today's artifact, for tools and the forge) or `Tiled` (an LRU of decoded tiles over a `TileLoader`).
- Tiles carry **no halo**: a read past a tile edge loads the neighbouring tile, so results equal whole-raster reads by construction.
- Per-tile storage:
  - the 11 stored rasters;
  - coast distance (computed once, over the whole map, at write time — not recomputable per tile);
  - the river segments touching the tile, with their global ids;
  - the 64 m bucket lists;
  - the lake runs.
- Global, small: meta, geology, region records, volcanoes, lake records, edges, spawn, plus the `WorldMap` built at write time.
- `world-store` (Plan A) writes and loads them. The server installs a tiled world; region threads and LOD workers prefetch and pin a job's tiles first, so a load error is a clean failure, never a panic mid-generation.

**Tech Stack:** Rust; worldgen (zstd row-delta codec reused per tile); world-store / sqlx from Plan A.

**Spec:**
- The settled design: `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` §0.9d (and §0.9c, §0.10).
- Handoff: `docs/superpowers/plans/2026-11-06-persistence-plans-handoff.md` §6.
- **Depends on** `docs/superpowers/plans/2026-11-07-world-store-r0-r1.md` (Tasks 2, 6, 7: the store crate, migrations, `StoreHandle`, server import). Tasks 1–3 here need only `worldgen` and can start before Plan A lands.

## Decisions settled in planning (2026-11-07)

1. **Tile size: `TILE_CELLS` = 256 cells, no halo.**
   - The cross-cell reach of sampling, read from the code:
     - bicubic height + detail slope: ±2 cells;
     - habitat slope probe (`SLOPE_PROBE_M` 4 m + c/2): ±3;
     - biome dither (`BIOME_DITHER_M` 24 m): ±2 at 16 m;
     - the salt-flat bilinear at the dithered point: ±2;
     - lake berm: ±1;
     - other bilinears: ±1.
   - So a halo would be 3 cells at 16 m and 4 at 8 m, and the river carving reach (`half_width + 26 m`) has no cap.
   - Rather than store duplicated halo cells and prove them sufficient, tiles hold exactly their cells and a read routes by global index. That is exact by construction. A ±3-cell read lands in a neighbour tile only within 3 cells of an edge (≈2 % of reads at 256), which costs one cache lookup.
   - At 16 m cells a tile is 4,096 m: one super-region of 16 regions × 256 m. At 8 m or 32 m it is still 256 cells; the world row stores `tile_cells`.
   - `TILE_CELLS × cell_m` must be a multiple of `BUCKET_M` (64 m), so that a river bucket never straddles tiles; this is asserted at write.
2. **Coast distance: stored as a tiled exact `f32` raster**, computed by today's whole-map chamfer (`habitat::coast_distance_by`) at write time.
   - Recomputing per tile is not exact: the transform is global with no range cap.
   - A capped variant might be exact for both consumers, but proving it is more work than the 4 B/cell it saves.
3. **LOD: option (a). LOD samples full-resolution tiles through the LRU.** There is no coarse layer in this plan.
   - At 16 m cells a 15 km view radius covers ~900 km², about 54 tiles of ~1.4 MB decoded (~80 MB), well inside the budget below.
   - Option (b) would change LOD output and needs the coarse ⊇ fine any-solid proof re-done.
   - The world map and menu preview do not need a coarse layer either: the `WorldMap` is stored (Decision 6), and single-player keeps `preview.png`.
4. **LRU budget: `TILE_CACHE_BYTES` = 256 MiB** (env `VOXEL_TILE_CACHE_MB`), counted from decoded tile sizes.
   - Pinned tiles are never evicted, so the cache may exceed the budget while a job pins more than fits; it logs that once.
   - Tile rows are zstd level 19 over row-delta, the codec the artifact already uses.
5. **Single-player stores tiles too: one code path.** The `.sqlite` world loads the same way as a Postgres one.
6. **The `WorldMap` is built once at write time and stored.**
   - Today every server start rebuilds it from the whole rasters; that took 13.5 s for a 100 km world.
   - Hosts read it from `forged_world_map`, and `SocietyGrid` / `RouteMap` take it from there.
7. **The original forge file is kept as an archive row** (`forged_archive`, the artifact bytes, ~40 MB for 100 km), and hosts never read it. It serves three uses:
   - re-tiling after a tile-format bump, without re-forging (Task 6);
   - `worldctl export` for the file-based tools (`world-map`, `cross-section`, `history`), which keep reading `.world` files;
   - the panel's export later.

   The rivers' reaches, needed for exporting and for `build_world_map`, are not reconstructible from per-tile segments, so the archive is what keeps them.
8. **`content_crc` of a tiled world** is crc32 over the meta row's bytes, then every tile row's bytes in `(tz, tx)` order, then the world-map bytes. It is computed by the writer and stored in `world.content_crc`. It keys the worldgen cache as before. Plan A's importer keeps the file's crc only until this task replaces it, so caches built from imported file worlds are invalidated once.
9. **Versioning:** `TILED_FORMAT_VERSION: u32 = 1` is stored in `forged_meta.format_version`. It is independent of `ARTIFACT_VERSION` (still the forge file's version). The stored world map carries `worldgen::tile_map::WORLD_MAP_VERSION` (bumped on ANY change to what `build_world_map` produces, logic included) in its row header (`encode_world_map`: format byte `FORMAT_WORLD_MAP` = 2, the version, a crc32, the zstd body). `load_forged` refuses a tile-format mismatch (`FormatKind::Tiles`) and a world-map row of another layout, another version or that does not decode (`FormatKind::WorldMap`, never "corrupt") with "re-tile: `just worldctl retile --db <db>`" when the archive row exists, else "re-forge". **Final-review ruling:** the server does not stop there — with the archive present it re-tiles at start (`[perf] event=world_retiled`), since re-tiling is lossless (saves are other rows) and a refusal would only make a single-player user run that command; without an archive it refuses. `write_tiled` refuses a world it could not load back (`split` runs the full `ForgedMeta::validate`, read radius included — cells under 8 m) as `StoreError::Unsupported` before anything is written; the server's import keeps the forge file then.

## Sequencing: runs after the water static layer and Plan A

The water static layer (`docs/superpowers/plans/2026-11-06-water-static-layer.md`) is implemented first, then Plan A's store tasks, then this plan. Water does not change `WorldArtifact` or `ARTIFACT_VERSION` (checked 2026-11-07). Still:
- re-verify the `forged.rs` / `lib.rs` line references before Task 1;
- take the Task 1 guard crc **after** water has landed, never before;
- if water added any per-cell raster or global table to the artifact by then, it joins `Rasters` / `ForgedMeta` and the bit-equality tests like the others.

## Global Constraints

- Work on `develop` in the main checkout. **No git worktrees**, including for subagents. `git add` only the files a task names, and run `git status` before each commit.
- `worldgen` stays sqlx-free and wasm-buildable. The tile cache and `TileLoader` trait live in `worldgen` (IO-free; the native-only parts are behind `cfg(not(wasm32))`, like `zstd` today). The SQL lives in `world-store`. `game` is untouched.
- **Bit-identity is the acceptance bar:** for every public sampling method, `Tiled` and `Whole` return equal `to_bits()` for every input, including points on tile edges and corners, past the map's land and sea edges, and under cache eviction.
- No compile-time database (runtime `query()` only), and the schema stays in the shared subset (`BIGINT`/`INTEGER`, `TEXT`, `BYTEA`/`BLOB`). Migrations go in `crates/world-store/migrations/{sqlite,pg}/0002_forged_tiles.sql`, and Plan A's `schemas_match` test must still pass.
- Named consts: `TILE_CELLS` (256), `TILE_CACHE_BYTES` (256 MiB), `TILED_FORMAT_VERSION` (1), `TILE_LOAD_RETRIES` (3).
- A tile load error never produces terrain:
  - region generation and LOD jobs prefetch-and-pin first; an error there is `LoadFailed` (Plan A Task 7) for a region, and a skipped job with a `warn` for LOD;
  - an unpinned read that fails after `TILE_LOAD_RETRIES` panics with the tile coords. That is a bug, since every generating path pins.
- **Verification:**
  - Use `just test-one worldgen "<target> <filter>"`, `just test-one world-store "…"`, `just test-pg "…"` and `just smoke`.
  - The worldgen hash goldens (`scene_hash_golden`) must stay unchanged.
  - Never run whole suites. Tests over 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`.
- **Ask before** any benchmark, memory measurement run, forge of a 100 km world, or live run.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. CLAUDE.md paragraphs go in a separate commit in the task that changes the behaviour.

## Review Focus

1. **Sampling exactly on a tile edge or corner:** a point at `x = k·TILE_CELLS·cell_m` and the cell indices `TILE_CELLS·k − 1 .. +2` read the right tiles, bit-equal. *(Task 2: `tile_edges_and_corners_bit_equal`.)*
2. **A river crossing tiles:** a segment whose reach spans two tiles carves both identically, and the nearest-segment tie order equals the whole world's (candidates in ascending global id). *(Task 2: `rivers_across_tiles_bit_equal`.)*
3. **The world's outer seam:** points past a LAND edge (clamped onto the edge), past a SEA edge (`OUTSIDE_HEIGHT_M`, no water), at corners past both, and `extended_cell_height` far outside. *(Task 2: `past_edge_bit_equal`.)*
4. **LRU eviction under a 15 km LOD view:** with the budget at 4 tiles, sampling an LOD sweep across 30 tiles evicts and reloads but stays bit-equal, and the cache never exceeds the budget beyond pinned tiles. *(Task 3: `eviction_is_transparent`, `cache_respects_budget`.)*
5. **Two hosts reading the same tiles:** one `StoreHandle` and a second reader on its own raw read-only `PgPool` (a test `TileLoader` with the same SELECT) load and sample one Postgres database concurrently with equal bits. Two `StoreHandle`s cannot both open a database: Plan A's world lock (a Postgres session advisory lock; the SQLite lock forbids two processes, by design) admits one. Multi-handle reads need a lock-free reader mode or R2's owner epochs (ruling R-B3). *(Task 4: `two_handles_read_same_tiles`, pg-gated.)*

---

### Task 1: Route every raster read through `Rasters` (whole only)

**Files:**
- Modify:
  - `crates/worldgen/src/forged.rs`: `ForgedWorld` (:550); `with_crc` (:644); `raster_cell` (:728); `lake_level_of` (:740); `bilinear` (:768); `bucket_segments` (:1096); and every `self.artifact.<raster>` read (26 sites)
  - `crates/worldgen/src/habitat.rs` (:105–114)
  - `crates/worldgen/src/lib.rs` (`world.artifact().seed` → `world.seed()`)

**Interfaces:**
- Produces, crate-private unless noted:
  - `pub(crate) enum Rasters { Whole(WholeRasters) /* Tiled added in Task 2 */ }`
  - `pub(crate) struct WholeRasters { artifact: WorldArtifact, coast_dist_m: Vec<f32>, segments: Vec<Segment>, buckets: Vec<Vec<u32>>, buckets_per_side: usize, lake_runs: Vec<(u32, u32, f32)> }` (moved out of `ForgedWorld`)
  - `#[derive(Clone, Copy)] pub enum QRasterId { Height, Uplifted, Bedding, TemperatureC, PrecipMm }`
  - `pub enum ByteRasterId { Drainage, Volcanism, Salinity, Biome, Savagery }`
  - methods on `Rasters`, all taking **clamped** global indices: `q(&self, r: QRasterId, i: usize) -> f32`, `byte(&self, r: ByteRasterId, i: usize) -> u8`, `region_id(&self, i: usize) -> u16`, `coast(&self, i: usize) -> f32`, `lake_level(&self, i: usize) -> Option<f32>`, `bucket(&self, bucket_index: usize) -> SegmentsRef<'_>` (iterates `(u32, &Segment)` in the stored order)
  - `pub fn seed(&self) -> u64` on `ForgedWorld`
  - `pub fn artifact(&self) -> Option<&WorldArtifact>`: `None` when tiled. Callers that need the whole artifact (`tile_map::build_world_map`, the examples) take `.expect("whole world")`, and they only run on `Whole` worlds.
  - `pub struct WorldGlobals`: the non-raster fields, read through `ForgedWorld::globals()` — `seed`, `size_m`, `cell_m`, `n`, `spawn`, `edges`, `column`, `rock_names`, `regions`, `volcanoes`, and per-raster `(min, step)` for the five `QRaster`s.

- [ ] **Step 1: Write a guard test.**
  - In `crates/worldgen/tests/forged_world.rs`, `whole_rasters_refactor_is_bit_exact`: forge 4,096 m / 16 m, seed 4. Hash with crc32 over `to_bits` the outputs of `sample`, `column_climate`, `biome_at`, `region_cell`, `cell_climate`, `snow_line_m`, `geo_offset_blocks`, `surface_rock` and `extended_cell_height` on a 7 m-stride grid covering the map + 200 m outside. Also hash `generate_region_forged` for regions (0,0) and the four corner regions.
  - Pin the crc as a constant recorded **before** the refactor. Slow-tagged.
- [ ] **Step 2: Run** `just test-one worldgen "--test forged_world -- --include-ignored whole_rasters"` on the unchanged code (`forged_world` is its own test target, not part of `it`: it pins the process world). Record the crc in the test and commit the test alone: `test(worldgen): pin forged sampling bits before the raster refactor`.
- [ ] **Step 3: Implement** `Rasters::Whole` and move every read behind it, with no behaviour change.
- [ ] **Step 4: Run** the guard test (`--test forged_world`), `just test-one worldgen "--test it scene_hash_golden"` and `just smoke`. Expected: PASS with unchanged constants.
- [ ] **Step 5: Commit**: `refactor(worldgen): forged sampling reads rasters through Rasters`.

### Task 2: Tiles, the tile splitter, and `Rasters::Tiled`

**Files:**
- Create:
  - `crates/worldgen/src/forged_tiles.rs`: `Tile`, `TileCoords`, `ForgedMeta`, the splitter, the codecs and `TileCache`
  - `crates/worldgen/tests/forged_tiles.rs` (in `tests/it.rs`, slow-tagged)
- Modify: `crates/worldgen/src/forged.rs` (add `Rasters::Tiled(TileCache)`, `ForgedWorld::from_tiles`; `Segment` (:541) becomes `pub` + `Serialize`/`Deserialize`, so tiles can carry it; `ForgedWorld::from_bytes` comes from Plan A Task 7 — add it here if Plan A has not landed)

**Interfaces:**
- Produces:
  - `pub struct TileCoords { pub tx: u32, pub tz: u32 }`
  - `pub const TILE_CELLS: u32 = 256`
  - `pub struct Tile { pub origin: TileCoords, pub w: u32, pub h: u32, pub q: [Vec<u16>; 5], pub bytes: [Vec<u8>; 5], pub region: Vec<u16>, pub coast: Vec<f32>, pub lake_runs: Vec<(u32, u32, f32)>, pub segments: Vec<(u32, Segment)>, pub buckets: Vec<Vec<u32>> }`:
    - edge tiles are smaller (`w`, `h`);
    - `lake_runs` hold global cell indices, clipped to the tile;
    - `buckets` hold the tile's 64 m buckets, each list exactly the whole world's list for that bucket.
  - `pub struct ForgedMeta { pub format_version: u32, pub tile_cells: u32, pub globals: WorldGlobals, pub lakes: Vec<LakeRecord /* level, area, depth, outlet; no cells */> }`
  - `pub fn split(world: &ForgedWorld /* Whole */, tile_cells: u32) -> (ForgedMeta, Vec<Tile>)`
  - `pub fn encode_meta(&ForgedMeta) -> Vec<u8>`, `decode_meta(&[u8]) -> Result<ForgedMeta, ArtifactError>`
  - `pub fn encode_tile(&Tile) -> Vec<u8>` (row-delta + zstd 19), `decode_tile(&[u8]) -> Result<Tile, ArtifactError>`
  - `pub trait TileLoader: Send + Sync { fn load(&self, t: TileCoords) -> Result<Vec<u8>, String>; }`
  - `pub struct TileCache` (thread-safe; loads happen outside the lock; concurrent misses for one tile share a per-tile `OnceLock`; LRU by decoded bytes)
  - `TileCache::new(meta: &ForgedMeta, loader: Arc<dyn TileLoader>, budget_bytes: usize) -> TileCache`
  - `ForgedWorld::from_tiles(meta: ForgedMeta, loader: Arc<dyn TileLoader>, content_crc: u32) -> Result<ForgedWorld, ArtifactError>`
  - `pub struct MemTiles(BTreeMap<TileCoords, Vec<u8>>)`, implementing `TileLoader` (tests, and the importer's verification)
- **Routing rule:** clamp the global indices exactly as `raster_cell` does, then `tile = (ix / tile_cells, iz / tile_cells)`, local = remainder. Bucket `b` lives in the tile containing its origin.

- [ ] **Step 1: Write the failing tests.** Fixture: forge 4,096 m / 16 m, seed 4; `split` with `tile_cells = 32` (64 tiles); `from_tiles` over `MemTiles`. Every assertion compares `to_bits()` with the whole world.
  - `tiled_equals_whole_everywhere`: the Task 1 grid and methods + `generate_region_forged` of all regions (slow).
  - `tile_edges_and_corners_bit_equal`: points at every tile edge/corner ± 1e-9 m and ± 0.5 cell.
  - `rivers_across_tiles_bit_equal`: every segment endpoint lying in a different tile from its start, sampled along the segment.
  - `past_edge_bit_equal`: lines 0–500 m outside each edge and the corners.
  - `meta_and_tile_codecs_roundtrip`.
- [ ] **Step 2: Run** `just test-one worldgen "--test it -- --include-ignored forged_tiles::"`. Expected: FAIL.
- [ ] **Step 3: Implement** the splitter, the codecs, `TileCache` and the `Tiled` arms of every `Rasters` method.
- [ ] **Step 4: Run** the tests above and the Task 1 guard. Expected: PASS.
- [ ] **Step 5: Commit**: `feat(worldgen): tiled forged world, bit-identical to whole-raster sampling`.

### Task 3: Prefetch and pins; eviction

**Files:**
- Modify:
  - `crates/worldgen/src/forged_tiles.rs`
  - `crates/worldgen/src/lib.rs`: the prefetch functions (`generate_columns` / `coarse_tile_forged` keep their signatures — see the Rule)
  - `crates/worldgen/src/forged.rs`: `prefetch_rect`, `from_tiles_with_budget`, `tile_cache`
- Test: `crates/worldgen/tests/forged_tiles.rs`

**Interfaces:**
- Produces:
  - `pub struct TilePins` (the pinned `Arc<Tile>`s plus the cache they are pinned in), whose drop unpins and trims the cache back to its budget
  - `ForgedWorld::prefetch_rect(&self, min_m: [f64; 2], max_m: [f64; 2]) -> Result<TilePins, String>`: loads and pins every tile under the rect grown by `MAX_READ_RADIUS_CELLS` (4) cells and the river bucket reach. It is a no-op `Ok` on `Whole`.
  - `pub fn prefetch_region(rc: RegionCoords) -> Result<TilePins, String>` and `pub fn prefetch_lod(key: &LodKey) -> Result<TilePins, String>` in `worldgen` (on `forged::active()`), plus `prefetch_region_forged` / `prefetch_lod_forged` for an explicit world
  - `pub const TILE_CACHE_BYTES: usize = 256 << 20`, `pub const TILE_LOAD_RETRIES: u32 = 3`
  - `TileCache::resident_bytes(&self) -> usize`, `TileCache::pinned_bytes(&self) -> usize`
  - `ForgedWorld::from_tiles_with_budget(meta, loader, crc, budget_bytes)`; `from_tiles` takes `VOXEL_TILE_CACHE_MB` (`forged_tiles::tile_cache_budget_from_env`, the one place it is read)
- **Rule:** a read first checks the pins (a per-call last-tile memo), then the LRU. `MAX_READ_RADIUS_CELLS` is asserted ≥ the Decision 1 reach for the world's `cell_m` at load (the formula is in its doc comment).
  - As built: pins are a count on the cache's slot (a pinned slot is never an eviction victim), so a pinned tile is always in the cache and the read path stays memo → cache with no pinned view threaded through `generate_columns` / `coarse_tile_forged` / `patch`; callers hold the `TilePins` across the job.

- [ ] **Step 1: Write the failing tests:**
  - `eviction_is_transparent`: budget = 4 tiles; an LOD-style sweep with a 512 m stride over the whole fixture, bit-equal to whole.
  - `cache_respects_budget`: after the sweep `resident_bytes() ≤ budget + pinned`.
  - `prefetch_error_is_reported_not_panicked`: a loader failing tile (1,1) makes `prefetch_rect` over it return `Err` naming `(1, 1)`.
  - `pinned_generation_needs_no_loads`: a counting loader shows that `generate_region_forged` after `prefetch_region` performs zero loads.
- [ ] **Step 2: Run** `just test-one worldgen "--test it -- --include-ignored forged_tiles::"`. Expected: FAIL.
- [ ] **Step 3: Implement** the pins, the prefetch functions and the budgeted LRU.
- [ ] **Step 4: Run** the same command, `just test-one worldgen "--test it -- --include-ignored scene_hash_golden"` (slow-tagged: without the flag it is skipped) and `just smoke`. Expected: PASS.
- [ ] **Step 5: Commit**: `feat(worldgen): tile prefetch + pins; budgeted LRU`.

### Task 4: Store tables, writer, loader

**Files:**
- Create:
  - `crates/world-store/migrations/sqlite/0002_forged_tiles.sql`, `crates/world-store/migrations/pg/0002_forged_tiles.sql`
  - `crates/world-store/src/forged.rs`, `crates/world-store/tests/forged.rs`
- Modify: `crates/world-store/src/handle.rs`, `src/rows.rs`

**Interfaces:**
- Consumes: Plan A `StoreHandle`, `open`; Tasks 2–3.
- **Schema 0002:**
  - `forged_meta(id = 1, format_version, bytes)`
  - `forged_tiles(tx, tz PK, bytes)`
  - `forged_world_map(id = 1, bytes)`, holding `encode_world_map(WorldMap)` (format byte, `WORLD_MAP_VERSION`, crc32, zstd body; Decision 9)
  - `forged_archive(id = 1, bytes)`, the original artifact bytes
  - Plan A's `forged_artifact` is renamed `forged_archive` by this migration (the data is kept).
- Produces:
  - `pub fn write_tiled(handle: &StoreHandle, artifact_bytes: Vec<u8>) -> Result<u32 /* content_crc */, StoreError>`:
    1. `ForgedWorld::from_bytes`, then `split(TILE_CELLS)` (a `Result`: a world that would not load back is `Unsupported`, nothing written);
    2. `tile_map::build_world_map`;
    3. **one transaction** writing meta, tiles, world map, archive and `world.content_crc` (Decision 8).
  - `pub struct StoreTiles(StoreHandle)`, implementing `TileLoader` (`SELECT bytes FROM forged_tiles WHERE tx = $1 AND tz = $2`)
  - `pub fn load_forged(handle: &StoreHandle) -> Result<Option<(ForgedWorld, WorldMap)>, StoreError>`: refuses `TILED_FORMAT_VERSION` mismatch with `StoreError::OtherFormat { found, kind: FormatKind::Tiles { archived } }` (Plan A's variant gained `kind`; the store's own format is `FormatKind::Store`), whose message names `just worldctl retile` when the archive exists, else re-forge.
  - `StoreHandle::forged_archive(&self) -> Result<Option<Vec<u8>>, StoreError>`
  - The tile codec (worldgen) carries a crc32 of each tile's / the meta's zstd body in its header, so a stored bit flip is a load error.
  - A blocking `StoreHandle` call made on the `world-store` thread itself returns a `Backend` error instead of deadlocking (so `StoreTiles::load` there cannot hang).

- [ ] **Step 1: Write the failing tests** (SQLite memory, the same on pg):
  - `write_then_load_bit_equal`: the Task 2 grid subset, through the store.
  - `world_map_is_stored`: equal to `build_world_map(&whole)`.
  - `content_crc_is_stable`: two writes of the same bytes give the same crc.
  - `old_tile_format_refused`.
  - `damaged_tile_row_fails_its_load`: one byte flipped in a stored tile row → the prefetch fails naming the tile.
  - `two_handles_read_same_tiles` (pg only). Plan A's world lock (a session advisory lock) lets only ONE `StoreHandle` open a world database, so the second host is its own Postgres pool + `TileLoader` (same `SELECT`) over the stored meta row; both sample concurrently, bit-equal to the whole world. Two real `StoreHandle`s become possible once R2's owner epochs replace the lock.
  - `schemas_match` (Plan A test, still green).
- [ ] **Step 2: Run** `just test-one world-store "--test it forged::"` and `just test-pg "--test it forged::"`. Expected: FAIL.
- [ ] **Step 3: Implement** the writer, the loader and the migrations.
- [ ] **Step 4: Run** the same commands plus `just test-one world-store "--test it schema::"` (and `just test-pg` of it). Expected: PASS. The forged tests are slow-tagged: run them with `-- --include-ignored`.
- [ ] **Step 5: Commit**: `feat(world-store): forged world as tiles + stored WorldMap`.

### Task 5: Server and tools on the tiled world

**Files:**
- Modify:
  - `crates/server/src/world_file.rs`: Plan A's `ensure_world` uses `write_tiled` instead of `put_artifact`; a forge writes tiles
  - `crates/server/src/lib.rs`:
    - install `load_forged` (:528–543);
    - `set_world_map` from the stored map instead of `build_world_map` (:618–629);
    - `ensure_history` takes the stored map
  - `crates/server/src/region_threads.rs`: `prefetch_region` before seeding a fresh region; an `Err` → `RegionOutput::LoadFailed` (Plan A Task 7). The prefetch is injected (`ThreadRegionSpawner::with_prefetch(RegionPrefetch)`; the server passes `worldgen::prefetch_region`), so tests run it on an explicit world. Every region thread (fresh, stored or parked) pins its 3×3 neighbourhood (`prefetch_region` per neighbour) before it is built and holds the pins for its life: its own generation and every halo seed of a never-run neighbour then read only pinned tiles, and a tile that will not load — its own or a neighbour's — is `LoadFailed`, never an empty seam.
  - `crates/server/src/lod_workers.rs`: `prefetch_lod` before every job (every kind reads `coarse_water`), plus `prefetch_region` for `Pristine` / `Stored` (they may generate the region); an `Err` → the job is re-queued after a capped exponential backoff (`LOD_RETRY_BASE_MS` 250 doubling to `LOD_RETRY_MAX_MS` 30 s, a `lod-retry` delay thread; first failure per job a `warn`, later ones `debug`), so it is eventually built and the manager's `lod_in_flight` entry clears (`game` untouched); a permanently bad tile retries at the cap.
  - `crates/server/src/lib.rs`: the halo generator re-pins the neighbour's (already pinned) tiles and panics naming the tile on a failure (a bug under the rule above, never an empty seed); the manager's own generator (unused on the server) pins too.
  - `crates/server/src/world_file.rs`: `OpenedWorld.map` (the stored map) and `install_world(&mut OpenedWorld) -> Option<WorldMap>` (installs, `[perf] event=world_map source=store`); a store with the archive but no `forged_meta` (Plan A import) is tiled from it at start (ruling R-B4); a leftover forge file is deleted iff its bytes equal the archive row.
  - `crates/world-store/src/ctl.rs` (the logic, `world_store::ctl::run`) + `src/bin/worldctl.rs` (created here): subcommands `forge --db <url> [--preset <name>] [--size <m>] [--cell <m>] [--seed <hex>] [--set knob=value]…` (forge in process, then `write_tiled`; `--seed` is hex like `forge-proto`'s; refused when the store already holds a world or saved regions), `export --db <url> <out.world>` (writes the archive row)
  - `justfile`: `worldctl *args`, `forge-db db *args` (built at `play_blocks`: the world row and world map are tied to the region size)
- Test: `crates/server/tests/store_world.rs` (Plan A's file), `crates/world-store/tests/worldctl.rs`, `crates/server/src/lod_workers.rs` (unit)

**Interfaces:**
- Consumes: Tasks 3–4, Plan A Task 7.
- Produces: the `worldctl forge` CLI. This is the entry point R2's panel `Job` will call; its flags mirror `forge-proto`'s and the generator's `ForgeParams`.

- [ ] **Step 1: Write the failing tests:**
  - `server_world_is_tiled`: after start, `forged::active()` is `Tiled`, and the world map came from the store (a `[perf] event=world_map source=store` line, or an accessor).
  - `tile_load_failure_stops_region`: a store whose tile under region (0,0) is corrupted (default 16 km world: 2×2 tiles). On the `ThreadRegionSpawner` with the store and an explicit-world prefetch, that region answers `LoadFailed`, never generates, and a region on healthy tiles runs.
  - `neighbour_tile_failure_stops_region`: a damaged tile that only a neighbour of the region reads → the region answers `LoadFailed` before it runs; a region whose 3×3 avoids the tile runs.
  - `lod_workers` unit tests: `lod_job_retries_until_its_tiles_load` (a prefetch failing 3 times → built and delivered after the backoff), `lod_retry_backoff_is_capped`.
  - `untiled_store_is_tiled_at_start` (R-B4): an archive-only store is tiled at start; `forged_meta.format_version = 0` is re-tiled at start (final-review ruling, Decision 9), and refused naming re-forge once the archive is gone.
  - `worldctl_forge_writes_store`: a small forge → `load_forged` is `Some`; export gives the archive back; a held store is refused with the lock message.
  - `server_world_is_tiled` is the only test in `store_world` that installs the process-global world; the others (`restart_restores_world` included) sample explicit worlds.
- [ ] **Step 2: Run** `just test-one server "--test store_world -- --include-ignored"` and `just test-one world-store "--test it worldctl::"`. Expected: FAIL.
- [ ] **Step 3: Implement** the above.
- [ ] **Step 4: Run** the same commands, `just test-threads` and `just smoke`. Expected: PASS. Then **ask the user** before a live `just run` and before measuring resident memory on a 100 km world.
- [ ] **Step 5: Commit**: `feat(server): serve the forged world from store tiles; worldctl forge`.
- [ ] **Step 6: Commit the docs separately.** Update the CLAUDE.md `crates/worldgen` forged-worlds paragraph (tiles, `TILE_CACHE_BYTES`, the stored world map, `worldctl`) and the `crates/server` startup notes. Commit: `docs(CLAUDE.md): forged world in the store`.

### Task 6: Re-tile existing stores; retire the single-blob path

**Files:**
- Modify:
  - `crates/world-store/src/bin/worldctl.rs`: `retile --db <url>` (archive → `write_tiled` in one transaction)
  - `crates/world-store/src/handle.rs`: remove `put_artifact` / `artifact` (Plan A); `forged_archive()` stays
  - `crates/server/src/world_file.rs`: nothing to change (the `OtherFormat` message already names `retile`; since the final review the server re-tiles a re-tile-able `OtherFormat` itself at start)
- Test: `crates/world-store/tests/forged.rs`

- [ ] **Step 1: Write the failing tests:**
  - `retile_from_archive_is_bit_equal`: write, overwrite `forged_meta.format_version = 0`, run `retile`, load → bit-equal to the original.
  - `retile_without_archive_says_reforge`.
- [ ] **Step 2: Run** `just test-one world-store "--test it forged::retile"`. Expected: FAIL.
- [ ] **Step 3: Implement** `retile`, and remove the blob API.
- [ ] **Step 4: Run** the tests and `just smoke`. Expected: PASS.
- [ ] **Step 5: Commit**: `feat(world-store): worldctl retile; the single-artifact store path is gone`.

## Self-review notes

- **Handoff §6 coverage:**
  - B1 → Tasks 1–2 (`Rasters`, pinning tests); B2 → Decision 1 + Task 2.
  - Coast distance → Decision 2 (stored).
  - Global tables → `ForgedMeta`. Their sizes are not measured on a 100 km world; that needs a forge, so ask.
  - B3 → Decision 3 (no coarse layer; LOD option (a)); B4 → Decision 6 + Task 4.
  - B5 → Tasks 4–5 (`write_tiled`, `worldctl forge`, `content_crc`, `TILED_FORMAT_VERSION`); B6 → Tasks 3, 5; B7 → Task 6.
- **Left measurable only by a run (ask):**
  - resident memory per host on a 100 km world;
  - tile row sizes per raster;
  - generation-time overhead of the routed reads vs whole. The design keeps it to one memo check per read.
