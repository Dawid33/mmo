# Cell Loading — Phase 2 (large regions in cell mode) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `just region_chunks=1024 cells=16 run` (a 2 km region) and larger — up to `region_chunks=32768` (65.5 km) — start instantly in cell mode: cells are generated on first activation, never the whole region up front.

**Architecture:** Worldgen gains `generate_cell` (a cell's columns only, bit-identical to the same slice of `generate_region`). The server's `CellStore` becomes lazy: it holds a per-cell generator and materializes a cell's content on first payload request. Spawners pass that generator instead of generating the region. The build-time region-size dial is widened to powers of two up to 32768; sizes above 128 require cell mode and get a minimal LOD table (the user will revisit LOD for large regions).

**Spec:** `docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md` (§3 lazy generation, §5 sizes up to 65.5 km). Previous phases: `…-phase1.md`, `…-phase1b.md`.

**User direction (2026-09-25):** regions are too small next to the rest of the world; make them larger; LOD for large regions is deferred ("will fix lod once regions are larger").

## Scope

In: `generate_cell` (procedural + forged); lazy `CellStore`; per-cell generator through both spawners; `REGION_CHUNKS_CHOICES` up to 32768 + cell-mode requirement above 128; audit fixes (wasm32 packet cap overflow, `PRISTINE_TERRAIN_CACHE_MAX` → 0, per-size LOD tests); docs/justfile.

Out: off-thread prefetch of cells (a cell generates on the region thread when first needed — measure first); per-cell disk cache; LOD quality at large sizes; island worldgen (phase 3); f64 physics.

## Global Constraints

- `develop`, no worktrees; stage only own hunks (user's uncommitted work in many files; entangled hunks stay uncommitted and are reported).
- `just test-one` / `just smoke` / `just test-threads`; no client test binaries; no benchmarks/GUI runs without asking.
- `CELL_CHUNKS == 0` and `REGION_CHUNKS ∈ {32, 64, 128}` behave exactly as before.
- Never write the region edge as a literal.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`.

## Review Focus

1. A cell generated alone must equal the same columns of the whole-region build, bit for bit (else server/client/LOD disagree with legacy caches and tests). Test in Task 1.
2. A cell first requested by `ensure_required` on join (not only by tickets) must be generated. Test in Task 2.
3. `dormant_chunks` for LOD must not force-generate the whole region. Test in Task 2.
4. A large-region build without cell mode must fail at build time, not OOM at run time. Const assert in Task 4.
5. Region sizes > 128 must not overflow on wasm32 (`MAX_DECOMPRESSED_PACKET_BYTES`). Task 4.

---

### Task 1: `worldgen::generate_cell`

**Files:** `crates/worldgen/src/lib.rs` (refactor `region_from_patch` into `columns_from_patch(patch, coords, x_range, z_range, mats)`; add `generate_cell`, `generate_cell_forged`). Test in `crates/worldgen/src/lib.rs` tests (slow-tagged).

**Interfaces:** `pub fn generate_cell(coords: RegionCoords, cell: CellCoords, cell_chunks: usize, seed: u64, mats: Materials) -> Vec<(ColumnCoords, Column)>` — region-local `ColumnCoords` of the cell's `cell_chunks²` columns, in the same (cx, cz) order `generate_region` emits them.

- [ ] Failing test: for `cell_chunks = 8`, every cell of region (1, -2): `generate_cell` equals (bincode bytes) the `generate_region` columns whose coords fall in that cell. Tag slow.
- [ ] Implement: window origin = region block origin + `x0 * CHUNK_BLOCKS` (x0 = cell.x * cell_chunks), span = `cell_chunks * CHUNK_BLOCKS` metres; `ProceduralPatch(TerrainPipeline::new(seed).profile_grid(ox, oz, span))`; forged: `world.patch(ox, oz, span)`; `columns_from_patch` over `x0..x0+cell_chunks`, `z0..z0+cell_chunks`.
- [ ] Green; commit.

### Task 2: Lazy `CellStore`

**Files:** `crates/game/src/cell_store.rs`, `crates/game/src/region_runner.rs` (`payload` needs `&mut`), tests in `cell_store.rs`.

**Interfaces:** `pub type CellGenerator = Box<dyn Fn(CellCoords) -> Vec<(ColumnCoords, Column)> + Send>`; `CellStore::lazy(cell_chunks, generate: CellGenerator) -> Self`; `payload(&mut self, cell) -> Option<CellPayload>` (generates on first request if the cell is inside the grid; `None` outside); `dormant_chunks` returns only already-materialized cells; `generated(&self) -> usize`.

- [ ] Failing tests: lazy store generates only the requested cell (counter in the generator); `dormant_chunks` on a fresh lazy store is empty and generates nothing; a cell outside the grid is `None`.
- [ ] Implement (`from_chunks` keeps eager behaviour for tests; entries become `Option`-materialized).
- [ ] Update `RegionRunner::apply_cell_changes` for `&mut`; run cell suites; commit.

### Task 3: Spawners pass a per-cell generator

**Files:** `crates/game/src/region_runner.rs` (`RegionSeed::into_runner` gains a cell generator), `crates/game/src/world_manager.rs` (`InlineSpawner::with_cells` takes a generator factory), `crates/server/src/region_threads.rs` + `lib.rs`, `crates/client/src/local_server.rs`.

**Interfaces:** `pub type CellGeneratorFactory = Arc<dyn Fn(RegionCoords) -> CellGenerator + Send + Sync>`; `into_runner(self, id, out, generate, cell_chunks, cells: Option<&CellGeneratorFactory>)` — cell mode with a factory builds `CellStore::lazy(cc, factory(id))` and never calls `generate`; without a factory it keeps the eager `from_columns` path (tests).

- [ ] Failing test (`cell_manager.rs`): a manager whose region generator panics but whose cell factory works joins a player and activates cells (proves the whole region is never generated).
- [ ] Implement; server builds the factory from `worldgen::generate_cell(rc, cell, CELL_CHUNKS, seed, mats)` (no disk cache for cells yet); local server the same.
- [ ] `just test-threads`, smoke; commit.

### Task 4: Region sizes up to 32768 chunks

**Files:** `crates/game/src/protocol.rs` (`REGION_CHUNKS_CHOICES`, const assert `REGION_CHUNKS <= 128 || CELL_CHUNKS != 0`, `MAX_DECOMPRESSED_PACKET_BYTES` in u64 clamped before the cast), `crates/game/src/lod.rs` (`LOD_RING_TABLE` for > 128: `&[(1, 32)]`; relax the horizon/pixel tests to ≤ 128; `tile_regions` test `.max(1)`), `crates/game/src/world_manager.rs` (`PRISTINE_TERRAIN_CACHE_MAX` `.max(1)`), `crates/client/src/world_scale.rs` (`SMALL_LOD_RING_TABLE` must not index an empty table), justfile doc.

- [ ] Failing tests: `parse_region_chunks` accepts 256…32768; the packet cap never overflows (compute for 32768 in u64); `PRISTINE_TERRAIN_CACHE_MAX >= 1`.
- [ ] Implement; build once with `just region_chunks=1024 cells=16 build` (no run).
- [ ] Commit.

### Task 5: Docs

CLAUDE.md (region-size paragraph: > 128 requires `cells=`; lazy per-cell generation), TODO.md, justfile comment. Smoke; commit.
