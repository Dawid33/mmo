# Cell loading phase 3 — island worldgen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The procedural world becomes islands in an open ocean (hashed
quadtree placement, island mask in `column()`), with a guaranteed spawn island
around the home region whose size is a build dial, and a worldgen-chosen dry
coastal spawn point that the cell-mode sim uses.

**Architecture:** `worldgen/src/islands.rs` is a pure function from world
position to "the island whose slot contains it" (quadtree of slots hashed from
the seed). `TerrainPipeline::column()` replaces its continent term (|continent
noise| → raw continents) with an island falloff: distance to the island centre
over a coast-warped radius, mapped onto the same continents curve — so the
shelf, drop-off, inland mountain gate and erosion all keep working, and
between islands everything reads as deep sea. Land relief scales with island
radius. The spawn island is forced into the root slot centred on the home
region; its `IslandInfo` (incl. spawn) reaches the sim through an
`IslandDirectory` closure on the spawners (game must not depend on worldgen)
and is stored in `CellState.spawn` (hashed, snapshotted — clients spawn from
the same value).

**Tech Stack:** Rust; worldgen noise/spline/density modules; `option_env!` dials.

**Spec:** `docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md`
§5 (Region = island → Island placement, Worldgen, Spawn, Test dial) and the
Build order phase 3.

## Scope decision (recorded, not in the spec)

Regions stay the fixed build-time `REGION_CHUNKS` squares. The spec's "island
region = power-of-two square sized to the island, keyed by `IslandId`" needs
runtime-variable region sizes (every `REGION_SIZE` use) and pairs naturally
with phase 6 (ocean region + transfer: leaving the island square). This phase
builds everything that is independent of that re-keying — placement, mask,
spawn, dial — and binds the spawn island to the home region (0,0): its centre
is the home region's centre. With `just region_chunks=1024 cells=16 island=16 run`
the player gets a 1 km island inside a 2 km region with sea around it.
`IslandInfo.id`/`origin`/`size_cells` are produced now for that later phase.

Deferred from spec §5 to later: ridge wavelength scaling with island radius
(needs a variable-scale warped-noise node; relief scaling alone keeps small
islands sane), `IslandId` region keying, the ocean region.

## Global Constraints

- Worldgen stays a pure deterministic function of (seed, world position,
  build dials). `column()` must stay bit-identical to the `#[cfg(test)]`
  `DensityFn` graph (`fused_column_matches_density_graphs_bit_exact`,
  `column_profile_matches_graph_bit_exact`).
- `generate_cell` must stay bit-identical to `generate_region` (existing test).
- `game` does not depend on `worldgen`; islands reach it as a closure.
- Forged worlds (`VOXEL_WORLD_FILE`) are untouched (they bypass `column()`).
- Tunables are named constants (spec §9: `ISLAND_MAX_CELLS` = 1024 etc.).
- Build dial `VOXEL_ISLAND_CELLS` (spawn island diameter in 64 m cells,
  0 = default), declared in `.cargo/config.toml` `[env]` like the others,
  part of the worldgen cache key, `just island=<cells>`.
- Verify with `just test-one`, `just smoke`, `just test-threads` (spawners
  touched). End with `just region_chunks=1024 cells=16 build`.

## Review Focus

1. Slot edges: terrain must be continuous across quadtree slot boundaries
   (no cliffs in the sea) — Task 1 test `islands_stay_inside_their_slot`
   (reach of every island < half the slot) + Task 2 test
   `terrain_is_continuous_across_slot_edges`.
2. A spawn island larger than the home region in cell mode would clip —
   Task 3 clamps the spawn into the region and logs; test
   `spawn_is_inside_the_home_region`.
3. Legacy (non-cell) mode keeps its constant spawn and must still land on the
   island — Task 2 test `legacy_spawn_column_is_land`.
4. The worldgen disk cache must not serve pre-island terrain — cache key
   covers the dial; the source crc already covers `islands.rs`.
5. `island=1` (64 m) must still produce land and a dry spawn — Task 3 test
   over sizes 1, 4, 16, 64.

---

### Task 1: quadtree placement (`crates/worldgen/src/islands.rs`)

**Interfaces — Produces:**
```rust
pub const ISLAND_CELL_M: f64 = 64.0;          // the dial / spec cell unit
pub const ISLAND_MAX_CELLS: u32 = 1024;        // root slot edge (65.5 km)
pub const ISLAND_MIN_SLOT_CELLS: u32 = 8;      // 512 m
pub const ISLAND_SPLIT_CHANCE: f64 = 0.75;     // per level above the minimum
pub const ISLAND_EMPTY_CHANCE: f64 = 0.35;     // leaf slot is open sea
pub const ISLAND_RADIUS_FRAC: (f64, f64) = (0.12, 0.22); // radius / slot edge
pub const ISLAND_REACH: f64 = 2.0;             // falloff reaches deep sea at 2 radii (see Task 2)
pub const SPAWN_ISLAND_CELLS_DEFAULT: u32 = 16; // 1 km diameter
pub const SPAWN_ISLAND_CELLS: u32;             // VOXEL_ISLAND_CELLS or default

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IslandId { pub level: u8, pub x: i64, pub z: i64 }
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Island { pub id: IslandId, pub centre: [f64; 2], pub radius: f64, pub slot_min: [f64; 2], pub slot_size: f64 }

pub struct IslandField { seed: u64, root_origin: [f64; 2], spawn_radius: f64 }
impl IslandField {
    pub fn new(seed: u64) -> Self;                       // home centre from game::REGION_SIZE
    pub fn with_spawn_cells(seed: u64, cells: u32) -> Self;
    pub fn island_at(&self, x: f64, z: f64) -> Option<Island>; // world meters
    pub fn spawn_island(&self) -> Island;
}
```
Root slots tile the plane with origin shifted so the home region's centre
(`REGION_SIZE / 2` units = `/ 16` m) is the centre of root slot (0,0), which
never splits and always holds the spawn island at exactly that centre. Other
slots: `hash(seed, level, x, z)` → split while `size > min` and
`u < ISLAND_SPLIT_CHANCE`; a leaf is empty with `ISLAND_EMPTY_CHANCE`, else
radius `lerp(frac) * size`, centre jittered within
`size/2 - ISLAND_REACH * radius * (1 + COAST_WARP)` of the slot centre.
Spawn radius = `cells * ISLAND_CELL_M / 2`, capped so its reach fits the root.

- [ ] Tests (in the module): determinism; `island_at` is local (same island
  for every point of its slot); `islands_stay_inside_their_slot` (reach ≤
  half slot for 2 000 sampled slots); size bounds; the spawn island is at the
  home centre with the requested radius; neighbouring slots never overlap.
- [ ] Implement; `just test-one worldgen "--lib islands"` green; commit.

### Task 2: island mask in `column()`

**Files:** `pipeline.rs`, `density.rs` (new node), `lib.rs` tests if any pin terrain.

- `TerrainPipeline` gains `islands: Arc<IslandField>`; `new(seed)` builds it.
- Continent value: `c_island(x, z)` (world meters, i.e. sample coords minus
  `SAMPLE_OFFSET_*`): no island → `0.0`; else
  `d = dist / (radius * (1 + COAST_WARP * coast))`, where
  `coast = noise[N_CONTINENTS].sample2(x * COAST_FREQ / radius, z * ...)`, and
  `c = (ISLAND_CORE_C - (ISLAND_CORE_C - COAST_C) * d).max(0.0)` with
  `COAST_C = 0.55` (the continents curve's coastline) and `ISLAND_CORE_C = 1.1`
  → deep sea (c = 0) at `d = ISLAND_CORE_C / (ISLAND_CORE_C - COAST_C) = 2.0`
  = `ISLAND_REACH`. `raw = continents_curve(c)` replaces `continents_curve(|cont|)`.
- Relief: `k = (ISLAND_RELIEF_FRAC * radius / FULL_RELIEF_M).clamp(ISLAND_MIN_RELIEF, 1.0)`
  (0.15, 190 m, 0.1; no island → 1). Offset becomes
  `clamp(b.max(0) * k + b.min(0) + mountains * MOUNTAIN_OFFSET * k)` where
  `b = base(raw) * (floor + swing * egate)`.
- Graph mirror: `DensityFn::Island { field: Arc<IslandField>, what: IslandValue::{Continent, Relief} }`
  evaluating `field.continent(x - SAMPLE_OFFSET_X_M, z - SAMPLE_OFFSET_Z_M, noises)`
  / `field.relief(...)`; `raw_continents()` and `offset()` rebuilt with it
  (`Min`/`Max` lose `allow(dead_code)`). The one function `IslandField::continent`
  is shared by both paths.
- Tests: the two oracle tests (now with islands on, sampled across the spawn
  island's coast); `terrain_is_continuous_across_slot_edges`; replace the
  land-share test with island semantics (land share over a 65 km sample
  0.02..0.30; the spawn island centre is land, 3 radii out is sea);
  `legacy_spawn_column_is_land`. Run `just test-one worldgen "--lib"` and
  `"--tests"`; fix any test pinned to continent terrain (seed scans etc.)
  by re-deriving its expectation from islands, not by loosening blindly.
- Commit.

### Task 3: spawn point, `IslandInfo`, `IslandDirectory`, `CellState.spawn`

**Interfaces — Produces:**
```rust
// game (crates/game/src/cells.rs)
pub struct IslandInfo { pub id: (u8, i64, i64), pub origin_m: [f64; 2], pub size_cells: u32, pub spawn: (f32, f32) /* region-local units */ }
pub type IslandDirectory = std::sync::Arc<dyn Fn(RegionCoords) -> Option<IslandInfo> + Send + Sync>;
// CellState gains `pub spawn: Option<(f32, f32)>`
// worldgen
pub fn island_info(seed: u64, rc: RegionCoords) -> Option<game::IslandInfo>; // Some only for the home region
pub fn spawn_point(p: &TerrainPipeline, island: &Island) -> [f64; 2];       // world meters
```
- `spawn_point`: rays at 16 fixed angles, radii from `0.5 r` to `1.2 r` in 2 m
  steps; first column whose surface is 1..=4 m above `SEA_LEVEL` and whose
  4 m neighbours differ by < 1 m → spawn; else the island centre.
- `player_spawn_xz(cell_mode)` → `Rollback::player_spawn_xz()` reading
  `cells.spawn` (fallback: today's constants). `RegionRunner::ensure_required`
  on `CreateClient` uses it. Spawn clamped to the region (log if clamped).
- Spawners (`ThreadRegionSpawner`, `InlineSpawner`) get `.with_islands(dir)`;
  `RegionSeed::into_runner` takes `Option<IslandInfo>` and sets
  `cells.spawn` raw on the fresh cell region. Server `lib.rs` and client
  `local_server.rs` pass `worldgen::island_info`.
- Tests: game — a cell runner with a spawn set spawns the player there and
  activates the 3×3 around it; worldgen — spawn dry/flat/inside region for
  island sizes 1, 4, 16, 64 cells (region permitting).
- `just smoke`, `just test-threads`; commit.

### Task 4: dial, cache key, docs

- `.cargo/config.toml` `[env] VOXEL_ISLAND_CELLS = "0"`; `justfile`
  `island := env_var_or_default("VOXEL_ISLAND_CELLS", "0")` + export + comment;
  cache key gains `SPAWN_ISLAND_CELLS`; CLAUDE.md worldgen + dial paragraphs;
  TODO.md; handoff status; memory.
- `just region_chunks=1024 cells=16 build`; commit.
