# DF-Scale World Terrain Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the island-era world forge into a DF-style world generator: continents ~100 km
across, 1–3 km ranges, latitude climate, DF-style biomes that show in play, a compact world file,
and a forge fast enough to iterate on.

**Architecture:**
- Everything happens in the offline forge (`crates/worldgen/src/forge/`) and the world file it
  writes (`crates/worldgen/src/forged.rs`). The engine samples the file in `generate_region_forged`
  / `coarse_tile_forged`.
- The work runs in order: forge speed first, then the file format, the vertical band, shape,
  climate, biomes, biomes at walking scale, then the UI and tools.
- The procedural (noise) world stays bit-identical throughout.

**Tech Stack:** Rust (dated nightly, `rust-toolchain.toml`), `rayon` 1.11 (already in Cargo.lock),
`zstd` (new, native-only, approved by the user), Slint UI, Bevy client, `just` recipes.

**Spec:** `docs/superpowers/specs/2026-11-01-df-scale-world-terrain-design.md`. Read it fully
before Task 1; section numbers below (§N) refer to it.

## Global Constraints

- **The procedural world stays bit-identical.** The `procedural` world, tests and wasm `?offline`
  must not change. These must pass unchanged after every task:
  - `just test-one worldgen "--test it -- --include-ignored scene_hash_golden::"`
  - `just test-one game "--test wasm_determinism"`
  - `just determinism-wasm`

  If a golden has to move, stop and ask the user.
- **The forge is offline:** single machine, not bit-reproducible across machines. It IS
  reproducible on one machine: the same params give the same bytes, including with threads.
- **Memory:**
  - forge at 100 km / 16 m: ≤ 8 GB peak;
  - server holding a world: ≤ 1 GB;
  - client + server single-player: ≤ 6 GB over today.
- **Mountain region vs a lowland region of the same world:** ≤ 3× in chunk count, snapshot size
  and genesis time. An overrun is recorded, not fixed here.
- **Defaults:**
  - `ForgeParams::default()` = Continent layout, 16 km, 32 m cells (~10 s). It is used by the
    server's missing-world forge, tests and a bare `just forge-proto`.
  - The main-menu generator defaults to 100,352 m / 16 m.
- **World file version:** bump `ARTIFACT_VERSION` whenever `WorldArtifact` changes. An older
  version is refused with an error that says to re-forge the world. No migration.
- **New blocks:** append only to `assets/blocks/blocks.ron`, with stable ids, never renumbered.
- **`zstd` is native-only.** Add it under `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`
  in `crates/worldgen/Cargo.toml`; world-file read/write is `#[cfg(not(target_arch = "wasm32"))]`.
  `cargo check -p client --target wasm32-unknown-unknown` must still pass.
- **rayon** runs in the forge only, native-only, the same way. On wasm the forge runs serially.
- **Build and test only through `just`** (CPU-capped). Never run whole suites. Verify with the
  named `just test-one …` targets plus `just smoke`. Tag any test slower than 100 ms
  `#[ignore = "slow: >100ms — run via just test-full"]` and run it with `-- --include-ignored`.
- **Ask the user before** running timing benchmarks or long forges (>2 min) on this box.
- **Keep the justfile and CLAUDE.md in sync** with any recipe or behaviour change, in the same
  commit.
- **Commits:** stage only the files the task touched; the tree has unrelated uncommitted work.
  End every message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Work location:** `develop` in the main checkout, no worktrees.

## Review Focus

1. **A forged world whose ground crosses block 320 or 512.** Ground there must still be solid,
   standable and pathable (no flat tops, no "no path"). Pinned in Task 7 and Task 8.
2. **Ending up in the sea past a land edge.** A unit or body must never fall into ocean at the map
   edge, and the far view past a land edge must show land, not a cliff. Pinned in Task 11.
3. **Loading an old (v2) `.world` file.** It must be refused with a clear re-forge message; the
   main menu lists it as needing a re-forge instead of crashing. Pinned in Task 6.
4. **Cancelling a 1-hour Play forge from the generator.** It must stop within one erosion
   iteration and leave no half-written world file behind. Pinned in Task 19.
5. **The non-forged world.** The procedural world and its trees must not pick up new species or
   biome ground. Pinned in Task 16 and Task 17.

---

## Phase A — Forge speed (§12). Do this first: every later tuning loop at 100 km depends on it.

### Task 1: Reusable step scratch + a forge benchmark recipe

**Files:**
- Modify: `crates/worldgen/src/forge/erosion.rs` (`route`, `discharge`, `step`)
- Modify: `crates/worldgen/src/forge/mod.rs` (`forge_with` loop)
- Modify: `justfile` (new `forge-bench` recipe); `CLAUDE.md` (mention it under Forged worlds)

**Interfaces:**
- Produces:
  - `pub struct StepScratch` (`Default`): owns every per-iteration buffer `route`, `discharge`
    and `step` used to allocate.
  - `pub fn route_into(w: &World, epsilon: f64, out: &mut Routing)`.
  - `pub fn step(w: &mut World, p: &ForgeParams, geo: &Geology, rain: &[f64], scratch: &mut StepScratch) -> usize`.
  - `route(w, epsilon) -> Routing` stays as a wrapper for the analysis callers.

- [ ] **Step 1: Write the failing test** `step_with_reused_scratch_matches_fresh` in `erosion.rs`
  `#[cfg(test)]`. Build a `World` from `ForgeParams { size_m: 4096.0, cell_m: 64.0, ..Default::default() }`,
  clone it, and run 20 steps on each:
  - the first world reuses one `StepScratch`;
  - the second gets a fresh `StepScratch` every step.

  `assert_eq!(a.h, b.h)` and `assert_eq!(a.uplifted, b.uplifted)`, bitwise.
- [ ] **Step 2: Run it to see it fail to compile.**
  `just test-one worldgen "--lib forge::erosion::"` → FAIL (`StepScratch` not found).
- [ ] **Step 3: Implement `StepScratch` and `route_into`.** Clear and resize the buffers instead of
  allocating, and use `BinaryHeap::clear`. The output must be bit-identical to before.
- [ ] **Step 4: Add the `forge-bench` recipe.**
  `forge-bench size="16000" cell="16":` runs `{{cap}} {{cargo}} run --release -p worldgen --example forge_proto -- --size {{size}} --cell {{cell}} --out target/forge-bench/w`
  and prints `/usr/bin/time` wall time and max RSS.
- [ ] **Step 5: Run the tests and the unchanged-output check.**
  - `just test-one worldgen "--lib forge::"` → PASS.
  - `just test-one worldgen "--test it forge_lib::"` → PASS.
- [ ] **Step 6: Commit** `perf(forge): reuse per-iteration buffers; just forge-bench`.

### Task 2: Forge quality gate (needed before any output-changing speed-up)

**Files:**
- Create: `crates/worldgen/src/forge/quality.rs`
- Create: `crates/worldgen/tests/fixtures/forge_quality_16k_32m.ron`
- Modify: `crates/worldgen/src/forge/mod.rs` (`pub mod quality;`)
- Test: `crates/worldgen/tests/forge_lib.rs`

**Interfaces:**
- Consumes: `hydrology::Analysis` / its `Stats` (`hydrology.rs:286`, `print_report`).
- Produces:
  - `pub struct Quality { land_share, height_p50_m, height_p90_m, height_p99_m, max_peak_m, concavity_theta, hack_exponent, river_mouths, river_km, max_strahler, lakes }` (all `f64`).
  - `pub fn quality(f: &Forged) -> Quality`.
  - `pub fn within(a: &Quality, baseline: &Quality, tol: &QualityTolerance) -> Result<(), String>`.
    The error names every metric out of tolerance.
  - `QualityTolerance::DEFAULT`:
    - relative 10% on heights and river km;
    - ±0.08 on θ and Hack;
    - ±25% on counts;
    - ±1 on Strahler order.

- [ ] **Step 1: Record the baseline.** Write the slow test `forge_quality_matches_baseline` (forge
  `ForgeParams { size_m: 16_000.0, cell_m: 32.0, seed: DEFAULT_SEED, .. }`, then `within(quality(f), fixture, DEFAULT)`).
  Generate the fixture from the CURRENT forge with a one-off `--bless` path, using an env var
  `FORGE_QUALITY_BLESS=1` that writes the RON file. Commit the fixture.
- [ ] **Step 2: Run it.**
  `just test-one worldgen "--test it -- --include-ignored forge_lib::forge_quality"` → PASS.
- [ ] **Step 3: Commit** `test(forge): quality gate against a recorded 16 km baseline`.

### Task 3: Linear-time routing with a depression graph

**Files:**
- Create: `crates/worldgen/src/forge/routing_fast.rs`
- Modify: `crates/worldgen/src/forge/erosion.rs` (`step` uses it); `forge/mod.rs`

**Interfaces:**
- Consumes: `Routing`, `StepScratch` (Task 1).
- Produces: `pub fn route_fast(w: &World, epsilon: f64, scratch: &mut StepScratch, out: &mut Routing)`.
  It fills the same `Routing` fields with the same meaning as `route_into`:
  - `filled` = water surface; depressions are filled to their spill level, plus epsilon, as
    today;
  - `receiver` is strictly downhill on `filled`;
  - `order` is downstream-first.

  The full Priority-Flood `route` stays for the first iteration's initial condition and for
  `hydrology::analyze`.

- [ ] **Step 1: Write the failing tests** in `routing_fast.rs`. Run each on these terrains:
  - a 64² noise heightfield with `ocean` on the border;
  - a single closed bowl;
  - a two-pit basin that spills into a third;
  - a perfectly flat island.

  Tests:
  - `fast_routing_drains_every_land_cell`: following `receiver` from any land cell reaches an
    ocean cell in ≤ n² steps.
  - `fast_routing_order_is_downstream_first`: every cell's position in `order` is greater than
    its receiver's.
  - `fast_routing_filled_matches_priority_flood`: `filled[i]` equals `route(w, FLOOD_EPSILON).filled[i]`
    within `1e-3` m for every cell.
- [ ] **Step 2: Run them to see them fail.** `just test-one worldgen "--lib forge::routing_fast::"`.
- [ ] **Step 3: Implement `route_fast`** (Cordonnier et al. 2019, "Efficient handling of
  depressions"):
  - steepest-descent receivers on `h`;
  - local minima become basins;
  - a basin graph whose edges are the lowest passes between basins;
  - Kruskal/Borůvka MST from the ocean to reroute each basin through its pass;
  - `filled` = pass height over the basin's cells below it, plus the epsilon gradient toward the
    outlet;
  - `order` by a stack traversal from the outlets.
- [ ] **Step 4: Switch `erosion::step` to `route_fast`** after iteration 1. Run:
  - `just test-one worldgen "--lib forge::"`;
  - the Task 2 gate (`--include-ignored forge_lib::forge_quality`) → PASS. Re-bless only if the
    user agrees.
- [ ] **Step 5: Commit** `perf(forge): linear-time routing with a depression graph`.

### Task 4: Parallel per-cell and per-basin work

**Files:**
- Modify: `crates/worldgen/Cargo.toml` (`rayon = "1"`, native-only)
- Modify: `crates/worldgen/src/forge/erosion.rs`, `routing_fast.rs`, `geology.rs`

**Interfaces:**
- Produces: same functions; `step` gains no parameters. Parallelism is internal, behind
  `#[cfg(not(target_arch = "wasm32"))]`.

- [ ] **Step 1: Write the failing test** `parallel_step_matches_serial` in `erosion.rs`: 20 steps
  on a 4096 m / 32 m world, once in a 1-thread `rayon::ThreadPool` and once in an 8-thread pool,
  `assert_eq!` on `h` and `uplifted`, bitwise.
- [ ] **Step 2: Run it to see it fail** (or not compile). `just test-one worldgen "--lib forge::erosion::"`.
- [ ] **Step 3: Parallelise the passes, keeping results order-independent.**
  - In parallel per cell: diffusion rows, geology erodibility, steepest-descent receivers.
  - In parallel per outlet basin: discharge accumulation and implicit incision. Each basin's tree
    is independent; process basins in parallel, cells within one serially.
  - No float reduction may depend on thread order.
- [ ] **Step 4: Check the passes.**
  - `just test-one worldgen "--lib forge::"` → PASS.
  - The Task 2 gate → PASS.
  - `cargo check -p worldgen --target wasm32-unknown-unknown` (via `{{cap}}`) → OK.
- [ ] **Step 5: Commit** `perf(forge): parallel diffusion, receivers and per-basin incision`.

### Task 5: Coarse-to-fine schedule

**Files:**
- Modify: `crates/worldgen/src/forge/mod.rs` (`forge_with`), `forge/world.rs` (upsampling),
  `forge/knobs.rs` (new group)

**Interfaces:**
- Produces:
  - New knobs in group `"Schedule"`:
    - `coarse_levels` (default 2, range [0, 3]): the number of ×2-coarser levels before the
      final cell;
    - `refine_iters` (default 60, range [10, 300]): iterations at each finer level.
  - The coarsest level runs `iters`.
  - `World::upsample(&self, p_fine: &ForgeParams) -> World`:
    - `h` and `uplifted` bilinear;
    - `uplift` and `ocean` recomputed at fine resolution from the same noise as `World::new`.
  - `ForgeProgress.iters` = the total over all levels.
  - Progress and cancel keep working.

- [ ] **Step 1: Write the failing test** `schedule_levels_add_up` (`forge/mod.rs` tests): with
  `coarse_levels = 2`, `iters = 20`, `refine_iters = 5`, `forge_with` reports 30 Eroding
  iterations, and the final `World.n == size_m / cell_m`.
- [ ] **Step 2: Run it to see it fail.** `just test-one worldgen "--lib forge::tests::"`.
- [ ] **Step 3: Implement the schedule and `World::upsample`.**
- [ ] **Step 4: Run the passes.**
  - `just test-one worldgen "--lib forge::"`.
  - The Task 2 gate with the default schedule → PASS.
- [ ] **Step 5: Measure, after asking the user.**
  `just forge-bench 100352 16`. Put the wall time and RSS in the spec's "Measured forge cost"
  table (new row "100 km / 16 m, optimised"). Target ≤ 10 min, ≤ 8 GB. If missed, report the
  phase breakdown (`perf`) to the user before going on.
- [ ] **Step 6: Commit** `perf(forge): coarse-to-fine schedule` (with the spec table update).

## Phase B — World file v3 (§6)

### Task 6: Compact, quantised, zstd world file

**Files:**
- Modify: `crates/worldgen/src/forged.rs` (`WorldArtifact`, `to_bytes`, `from_bytes`, `validate`,
  `ForgedWorld::new` / `load`, the samplers)
- Modify: `crates/worldgen/src/forge/mod.rs` (`Forged::artifact`)
- Modify: `crates/worldgen/Cargo.toml` (`zstd`, native-only)
- Modify: `crates/client/src/frontend/load_world.rs` (re-forge label)
- Test: `crates/worldgen/tests/forged_world.rs`; `forged.rs` `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `pub struct QRaster { pub min: f32, pub step: f32, pub data: Vec<u16> }`, with
    `fn get(&self, i: usize) -> f32` and `fn from_f32(v: &[f32], min: f32, step: f32) -> QRaster`.
    Values clamp into range; NaN is not allowed.
  - `WorldArtifact` v3 fields:
    - `height: QRaster` (min −1000 m, step 0.0625 m);
    - `uplifted: QRaster`, `bedding: QRaster` (step 0.0625 m, min chosen from data);
    - `rain: QRaster` (kept relative for now; step 0.001);
    - `lakes: Vec<Lake>`, where each `Lake` gains `cells: Vec<(u32, u32)>` as run-length
      `(start_index, len)` runs.
    - `lake_level` and `coast_dist_m` are removed.
  - `ForgedWorld` keeps `height` decoded as `Vec<f32>`, and the per-cell lake level built from
    `lakes` in a `HashMap<u32, f32>` or sparse runs. `coast_dist_m` is computed at load by
    `habitat::coast_distance_m`.
  - Encoding: header (magic, version 3) + zstd (level 19) over bincode, with each `QRaster.data`
    row-delta encoded before serialising (`delta_rows(&mut [u16], n)` / `undelta_rows`).
  - `ARTIFACT_VERSION = 3`.
  - `ArtifactError::OldVersion { found: u32 }`, whose `Display` says
    `world file version {found} is older than {ARTIFACT_VERSION}: re-forge this world`.

- [ ] **Step 1: Write the failing tests.**
  - `qraster_round_trip_within_half_step`: random heights in [−900, 3400] round-trip within
    0.0313 m.
  - `artifact_v3_round_trip_preserves_samples`: forge a 4096 m / 64 m world, save, load. Then
    `ForgedWorld::sample` at 200 random points matches the pre-save sample within 0.07 m height,
    and `water_y` presence matches.
  - `v2_file_is_refused_with_reforge_message`: write a header with version 2;
    `load` → `Err(OldVersion { found: 2 })`, and the message contains "re-forge".
- [ ] **Step 2: Run them to see them fail.**
  `just test-one worldgen "--lib forged::"` and `just test-one worldgen "--test forged_world"`.
- [ ] **Step 3: Implement the format.** The main menu's world list shows "needs re-forge" for
  `OldVersion` (`load_world.rs`; `read_artifact_version` already exists).
- [ ] **Step 4: Measure size.** Forge 16 km / 16 m with `just forge-proto`, and record the file
  size and the server-side RSS delta of `ForgedWorld::load` in the spec §6 (replacing the
  estimate). Target ≤ 4 B/cell on disk.
- [ ] **Step 5: Run the passes.**
  - The Step 2 targets.
  - `just test-one server "--lib world_file::"`.
  - `just smoke`.
  - `cargo check -p client --target wasm32-unknown-unknown`.
- [ ] **Step 6: Commit** `feat(worldgen): compact v3 world file (quantised rasters, sparse lakes, zstd)`.

## Phase C — Kilometre-plus vertical band (§3, §7)

### Task 7: Unbounded vertical band for forged profiles; analytic surface scan

**Files:**
- Modify: `crates/worldgen/src/pipeline.rs` (`ColumnProfile`)
- Modify: `crates/worldgen/src/lib.rs` (`SCAN_TOP`/`SCAN_BOTTOM` use, `surface_from_profile`,
  band init at `:399-401`, coarse fill at `:609-612`)
- Test: `crates/worldgen/tests/forged_world.rs`, `crates/worldgen/tests/scene_hash_golden.rs` (must stay green unchanged)

**Interfaces:**
- Produces:
  - `ColumnProfile` gains `unbounded: bool`. `from_surface` sets it to `true`, and every
    procedural constructor to `false`. When `unbounded`, `density_at` uses `neg_y = -y`
    (no ramp clamp).
  - `const FORGED_SCAN_TOP: i32 = 4096; const FORGED_SCAN_BOTTOM: i32 = -1024;`
  - `surface_from_profile` starts at `min(band_top, prof.surface_y().ceil() as i32 + 2)` and scans
    down. The result is identical for procedural profiles, since density is monotone in y inside
    the band.
  - Coarse LOD (`coarse_from_patch`) fills each cell column down to the lowest of its four
    neighbours' surfaces (not `surface − MAX_SOIL_DEPTH` only), so steep faces have no gaps.

- [ ] **Step 1: Write the failing tests** in `forged_world.rs`:
  - `forged_ground_above_block_320_is_solid`: install a synthetic artifact (a 2,500 m cone) and
    generate the region at its summit. The top solid block in the summit column equals
    `round(2500 + SEA_TOP_BLOCK)` ± 1, and there is no flat plateau at 320.
  - `coarse_lod_steep_face_has_no_gaps`: on the same cone, the factor-16 coarse tile on the flank
    is solid between each cell column's surface and its lowest neighbour's.
- [ ] **Step 2: Run them to see them fail.** `just test-one worldgen "--test forged_world"`.
- [ ] **Step 3: Implement** the `unbounded` flag, the analytic scan start and the forged band
  bounds.
- [ ] **Step 4: Run the passes.**
  - Step 2 target.
  - `just test-one worldgen "--test it -- --include-ignored scene_hash_golden::"` → PASS unchanged.
  - `just test-one worldgen "--test it underground::"`.
  - `just determinism-wasm`.
  - `just smoke`.
- [ ] **Step 5: Commit** `feat(worldgen): forged terrain beyond block 320; analytic surface scan; gap-free coarse cliffs`.

### Task 8: Pathfinding at any height

**Files:**
- Modify: `crates/game/src/pathfind.rs:27-29, 155-170`; `crates/game/src/adventure.rs:315`
- Modify: `crates/game/src/orders.rs` (`TileSource`)
- Modify: `crates/client/src/renderer/commander/move_order.rs` (if the client implements `TileSource`)
- Test: `crates/game/tests/adventure.rs`

**Interfaces:**
- Produces:
  - `TileSource` gains `fn column_band(&self, column: [i32; 2]) -> Option<(i32, i32)>`: the lowest
    and highest materialised (or implied-surface) block y of that block column. The default
    implementation returns `None`, in which case callers fall back to the old constants.
  - `nearest_standable` scans `column_band` ± 2 instead of `COLUMN_SCAN_Y_MIN..=MAX`.
  - The `adventure.rs` clamp uses the band.
  - The constants stay as the fallback.

- [ ] **Step 1: Write the failing test** `walker_paths_on_ground_above_block_512` in
  `crates/game/tests/adventure.rs`: a region whose ground is at block 2,000 (one flat chunk
  layer); `Order::MoveTo` across it arrives. Tag it slow if it is.
- [ ] **Step 2: Run it to see it fail.**
  `just test-one game "--test it -- --include-ignored adventure::walker_paths_on_ground_above"`.
- [ ] **Step 3: Implement `column_band`** for the sim's tile source (from the region's chunk
  columns + `ColumnGround`) and for the client `TileMirror`.
- [ ] **Step 4: Run the passes.**
  - Step 2 target.
  - `just test-one game "--test it -- --include-ignored adventure::"`.
  - `just test-threads`.
  - `just smoke`.
- [ ] **Step 5: Commit** `feat(game): pathfinding follows each column's real height band`.

### Task 9: Forge relief to kilometres; geology depth; height-aware tools

**Files:**
- Modify: `crates/worldgen/src/forge/knobs.rs`, `forge/post.rs`, `forge/geology.rs:38-41,113`
- Modify: the colour ramps: `crates/client/src/renderer/commander/minimap.rs:141-148`,
  `crates/worldgen/src/tile_map.rs:217-221,301-315`, `crates/worldgen/src/forge/render.rs:157-170`
- Modify: the height scales: `crates/worldgen/examples/world_map.rs:34-35`,
  `examples/forge_proto/render.rs:9-11`, `examples/seed_scan.rs:18-19`

**Interfaces:**
- Produces:
  - `max_peak_m`: default 2,800, range [80, 3,500].
  - `uplift`: range up to 6e-3.
  - `COLUMN_BOTTOM_S = -5_000.0`, with the granite basement continuing to it.
  - A shared `pub fn height_color(height_m: f64) -> [f64; 3]` in `tile_map.rs`: a ramp to
    3,500 m with a snow cap above the snow line. It is used by the minimap, the world map and the
    forge render. The forge render keeps its own palette only where it differs on purpose.
  - Tool height scales of −1,000…+3,600 m.
  - `MOUNTAINS_MIN_M = 1200.0`, `HILLS_RELIEF_M = 60.0`, `MOUNTAINS_RELIEF_M = 250.0`.

- [ ] **Step 1: Write the failing test** `default_forge_reaches_kilometre_peaks` (slow, in
  `forge_lib.rs`): the default `ForgeParams` (16 km / 32 m Continent once Task 10 lands; until
  then the island) gives `max_peak_m ≥ 1,500` and no `rock_at_s` debug-assert panic.
- [ ] **Step 2: Run it to see it fail.**
  `just test-one worldgen "--test it -- --include-ignored forge_lib::default_forge_reaches"`.
- [ ] **Step 3: Implement** the knob ranges and defaults, the column depth and the shared ramp.
  Retune `uplift` / `erodibility` / `iters` defaults until the test passes with real valleys:
  check `just forge-proto --cell 32` relief PNG and that θ / Hack stay in 0.4–0.7.
- [ ] **Step 4: Re-bless the Task 2 quality fixture** (the relief changed on purpose) with
  `FORGE_QUALITY_BLESS=1`. Note this in the commit.
- [ ] **Step 5: Run the passes.**
  - `just test-one worldgen "--lib forge::"`.
  - `just test-one worldgen "--test it tile_map::"`.
  - `just smoke`.
- [ ] **Step 6: Commit** `feat(forge): kilometre-plus relief, deep geology column, height ramps to 3.5 km`.

## Phase D — Continental shape (§2)

### Task 10: Continent / Continents / Inland layouts, per-edge sea or land, plate belts

**Files:**
- Create: `crates/worldgen/src/forge/plates.rs`
- Modify: `crates/worldgen/src/forge/world.rs` (`layout_blobs`, `landmass`, `World::new`, the ocean
  margin), `forge/knobs.rs`, `forge/erosion.rs` (land-edge outlets), `forge/world.rs`
  `DEFAULT_CELL_M` → 32.0

**Interfaces:**
- Produces:
  - Layout constants `LAYOUT_CONTINENT = 3`, `LAYOUT_CONTINENTS = 4`, `LAYOUT_INLAND = 5`.
    `CHOICES` lists them first-class; the `Knobs` default `layout = 3`.
  - `pub enum EdgeKind { Sea, Land }`.
  - `pub fn edges(seed: u64, k: &Knobs) -> [EdgeKind; 4]` (N, E, S, W), with the knob
    `sea_edges` (0–4, default 2). Which edges are sea is picked by seed, and Inland forces 0.
  - `ocean_margin_start` applies only to sea edges.
  - In `World`:
    - `edges: [EdgeKind; 4]`;
    - land-edge border cells are fixed-height outlets (base level = the cell's height);
    - uplift tapers to 0 over `LAND_EDGE_TAPER_M = 2_000.0` so the edge is not cliffed.
  - `plates::PlateField { plate: Vec<u16>, convergence: Vec<f32> }` and
    `pub fn plate_field(seed: u64, size_m: f64, n: usize, k: &Knobs) -> PlateField`. Uplift belts
    follow convergent boundaries.
  - Knobs `plate_count` (default 7), `belt_length_km` / `interior_ranges`.
  - Noise wavelengths for the new layouts scale with `size_m / 16_000`. The island layouts keep
    today's metres exactly.

- [ ] **Step 1: Write the failing tests** (`world.rs` / `plates.rs` tests):
  - `continent_has_requested_sea_edges`: with `sea_edges = 2`, exactly 2 edges are ≥ 95% ocean
    in their outer 5% band, and the other 2 are ≥ 50% land.
  - `inland_has_no_sea_edges`.
  - `land_edge_drains_out`: every land cell drains to the sea or to a land-edge outlet (routing
    reaches `NO_RECEIVER` only there).
  - `belts_follow_plate_boundaries`: ≥ 70% of cells with uplift > 0.5 × max lie within 3 km of a
    convergent boundary.
  - `island_layout_unchanged`: `layout = 0`, the old knobs → `World::new` `h` equals a value
    recorded before the change (store a crc32 of `h` in the test).
- [ ] **Step 2: Run them to see them fail.** `just test-one worldgen "--lib forge::"`.
- [ ] **Step 3: Implement the layouts, edges, plates and land-edge outlets.** `route` / `route_fast`
  seed the flood from land-edge cells as well as coastal sea.
- [ ] **Step 4: Run the passes.**
  - `just test-one worldgen "--lib forge::"`.
  - The Task 2 gate under `layout = 0`.
  - Look at `just forge-proto --cell 64 --size 100352` PNGs: continental shape, ranges tens of km
    long. Ask the user before the run if it is > 2 min.
- [ ] **Step 5: Commit** `feat(forge): continental layouts, per-edge sea/land, plate-boundary ranges`.

### Task 11: The world past a land edge

**Files:**
- Modify: `crates/worldgen/src/forged.rs` (`OUTSIDE_HEIGHT_M` use, `sample`, `patch`)
- Modify: `crates/game/src/world_manager.rs` / `crates/game/src/adventure.rs` (no crossing off-map)
- Test: `crates/worldgen/tests/forged_world.rs`, `crates/game/tests/adventure.rs`

**Interfaces:**
- Consumes: `WorldArtifact.edges: [EdgeKind; 4]` (add the field; bump the version to 4).
- Produces:
  - Past a land edge, `ForgedWorld::sample` returns the edge cell's height clamped outward, with
    no rivers and no lakes.
  - Past a sea edge it returns ocean (`OUTSIDE_HEIGHT_M`) as today.
  - `WorldMap` gains `fn contains(&self, rc: RegionCoords) -> bool`.
  - A walker goal or an `EntityArrived` into a region outside the map is refused: the walker
    stops at the edge tile.

- [ ] **Step 1: Write the failing tests.**
  - `outside_land_edge_continues_land`: sample 500 m past a land edge equals the edge height.
  - `outside_sea_edge_is_ocean`.
  - `walker_never_leaves_the_map`: in `adventure.rs`, `MoveTo` toward a region past the map edge
    ends on the last tile inside it.
- [ ] **Step 2: Run them to see them fail.**
  `just test-one worldgen "--test forged_world"`; `just test-one game "--test it -- --include-ignored adventure::walker_never"`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the passes.** Step 2 targets; `just test-threads`; `just smoke`.
- [ ] **Step 5: Commit** `feat: land continues past a land edge; nothing crosses the world edge`.

## Phase E — Climate (§4)

### Task 12: Latitude, temperature, banded winds, precipitation in mm/yr

**Files:**
- Modify: `crates/worldgen/src/forge/climate.rs` (rewrite around `Climate`), `forge/mod.rs`,
  `forge/knobs.rs`
- Modify: `crates/worldgen/src/forged.rs` (store; version bump), `crates/worldgen/src/habitat.rs`

**Interfaces:**
- Produces:
  - `pub struct Climate { pub temperature_c: Vec<f32>, pub precip_mm: Vec<f32>, pub latitude_deg: Vec<f32> /* per row */ }`
    and `pub fn compute(w: &World, p: &ForgeParams) -> Climate`.
  - `pub fn discharge_weight(c: &Climate, w: &World) -> Vec<f64>` = `precip_mm / mean(land precip_mm)`.
    It replaces `rainfall()` in the erosion loop (refresh every `RAIN_REFRESH_ITERS` as now).
  - Knobs, group `"Climate"`:
    - `pole` (0 none, 1 north, 2 south, 3 both; default 1);
    - `latitude_span_deg` (default 90);
    - `latitude_center_deg` (used when `pole = 0`; default 45);
    - `equator_temp_c` (27), `pole_temp_c` (−25), `lapse_c_per_km` (6.5);
    - `trade_wind_edge_deg` (30), `polar_edge_deg` (60).
  - Winds: from the east for |lat| < 30 and > 60, from the west between. The row sweep runs in the
    band's direction. Moisture recharges over any ocean or lake cell.
  - `const LATITUDE_PRECIP_MM: [(f32, f32); 7] = [(0.0, 2500.0), (10.0, 1800.0), (25.0, 350.0), (40.0, 900.0), (55.0, 1000.0), (70.0, 400.0), (90.0, 200.0)];`
    interpolated linearly. Then `precip_mm = base(lat) × orographic_relative`, clamped by
    `rain_min` / `rain_max` relative to the base.
  - Artifact: `temperature_c: QRaster` (step 0.1), `precip_mm: QRaster` (step 1); the relative
    `rain` is removed; version bump.
  - `Habitat` (forged): `temperature_c` comes from the field (no more fixed lapse from
    `BASE_TEMPERATURE_C`), and `rain = precip_mm / REFERENCE_PRECIP_MM`
    (`const REFERENCE_PRECIP_MM: f32 = 900.0`), so the existing species niches keep their meaning.
    The procedural habitat is unchanged.

- [ ] **Step 1: Write the failing tests** (`climate.rs`):
  - `temperature_falls_toward_the_pole_and_with_height`: with pole north, the row at the north
    edge is colder than the south edge by ≈ `equator − pole` (±2 °C) at sea level, and a cell
    1,000 m up is 6.5 °C colder than a sea-level cell on its row.
  - `westerlies_cast_a_rain_shadow_east_of_a_ridge`: at lat 45° a N–S ridge has its wet side to
    the west.
  - `trade_winds_cast_the_shadow_west`: the same at lat 15°.
  - `precip_follows_the_latitude_table_on_flat_land`: flat land at lat 25° gets 350 ± 60 mm.
- [ ] **Step 2: Run them to see them fail.** `just test-one worldgen "--lib forge::climate::"`.
- [ ] **Step 3: Implement**, and store the fields in the artifact.
- [ ] **Step 4: Run the passes.**
  - `just test-one worldgen "--lib forge::"`.
  - `just test-one worldgen "--test forged_world"`.
  - `just test-one worldgen "--test it trees_in_terrain::"`.
  - `just test-one worldgen "--test it -- --include-ignored scene_hash_golden::"` unchanged.
- [ ] **Step 5: Commit** `feat(forge): latitude climate — temperature, banded winds, precipitation in mm/yr`.

### Task 13: Drainage, volcanism, salinity

**Files:**
- Create: `crates/worldgen/src/forge/fields.rs`
- Modify: `forge/mod.rs`, `forge/knobs.rs`, `forge/world.rs` (volcano cones in uplift),
  `forged.rs` (store; version bump)

**Interfaces:**
- Produces:
  - `pub fn drainage(w: &World, a: &Analysis, geo: &Geology) -> Vec<u8>` (0–100):
    - high on slopes and on permeable rock (sandstone, limestone, conglomerate);
    - low on flat ground within `DRAINAGE_FLOOD_M = 4.0` m of the nearest river or lake surface,
      and on shale.
  - `pub fn volcanism(seed: u64, w: &World, plates: &PlateField) -> Vec<u8>`: continental-scale
    noise plus hot spots along convergent boundaries.
  - `pub fn volcanoes(seed, &volcanism, k) -> Vec<[f32; 2]>`: cells at 100, spaced
    ≥ `VOLCANO_SPACING_M = 8_000.0`. `World::new` adds a cone of uplift at each.
  - `pub fn salinity(w: &World, a: &Analysis) -> Vec<u8>`: 100 at sea; 100 for endorheic lakes;
    tapering over `SALT_REACH_M` on the coast.
  - Knobs `volcano_count_max` (default 6) and `volcanism_strength` (default 1).
  - Artifact: `drainage`, `volcanism`, `salinity: Vec<u8>`, `volcanoes: Vec<[f32; 2]>`.

- [ ] **Step 1: Write the failing tests** (`fields.rs`):
  - `floodplain_is_poorly_drained`: a cell beside a river on flat ground < 33.
  - `steep_slope_is_well_drained`: slope 0.5 > 66.
  - `closed_lake_is_salt`.
  - `volcanoes_keep_their_spacing`.
- [ ] **Step 2: Run them to see them fail.** `just test-one worldgen "--lib forge::fields::"`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the passes.** `just test-one worldgen "--lib forge::"`; `just test-one worldgen "--test forged_world"`.
- [ ] **Step 5: Commit** `feat(forge): derived drainage, volcanism + volcanoes, salinity`.

## Phase F — Biomes and regions (§5)

### Task 14: Biome classification, regions, savagery, good/evil

**Files:**
- Create: `crates/game/src/biome.rs` (pure data + classification, shared with the client UI;
  engine-free)
- Create: `crates/worldgen/src/forge/regions.rs`
- Modify: `crates/game/src/lib.rs` (`pub mod biome; pub use biome::*;`), `forge/mod.rs`,
  `forged.rs` (store; version bump)

**Interfaces:**
- Produces in `game::biome`:
  - `#[repr(u8)] pub enum Biome { Ocean, FrozenOcean, Lake, Glacier, Tundra, Taiga, ConiferForest, BroadleafForest, TropicalConiferForest, TropicalBroadleafForest, Grassland, Savanna, Shrubland, SandDesert, RockyWasteland, Badlands, Marsh, Swamp, MangroveSwamp, Mountain }`.
  - `pub struct CellClimate { pub height_m: f32, pub temperature_c: f32, pub precip_mm: f32, pub drainage: u8, pub salinity: u8, pub is_lake: bool }`.
  - `pub fn classify(c: &CellClimate) -> Biome`.
  - `pub enum RegionType { Ocean, Lake, Glacier, Tundra, Mountains, Forest, Wetland, Grassland, Hills, Desert }`.
  - `pub fn region_type(b: Biome, drainage: u8) -> RegionType`. Hills = a grassland-family biome
    with drainage ≥ 50.
  - Constants:
    - `MOUNTAIN_MIN_M = 1200.0`;
    - `RAIN_INDEX_MM = 16.0` (rain index `r = precip_mm / 16`, clamped 0–100: desert < 160 mm, broadleaf ≥ 1,200 mm);
    - `FROZEN_C = -5.0`, `GLACIER_C = -12.0`;
    - `TAIGA_MAX_C = 3.0`, `TROPICAL_MIN_C = 20.0`.
  - `classify`, in order:
    - height < 0 or `is_lake` → Ocean / FrozenOcean (T < FROZEN_C) / Lake;
    - height ≥ MOUNTAIN_MIN_M → Mountain;
    - T < GLACIER_C, or (T < FROZEN_C and drainage ≥ 75) → Glacier;
    - T < FROZEN_C → Tundra;
    - otherwise DF's rainfall × drainage table (spec §5; research `01-pipeline-and-fields.md`
      §3.2), using r thresholds 10 / 20 / 33 / 66 / 75 and drainage thresholds 33 / 66.
  - Temperature variants of the table's results:
    - conifer forest becomes Taiga when T < TAIGA_MAX_C, and TropicalConiferForest when
      T ≥ TROPICAL_MIN_C;
    - broadleaf forest becomes TropicalBroadleafForest when T ≥ TROPICAL_MIN_C;
    - swamp becomes MangroveSwamp when T ≥ TROPICAL_MIN_C, salinity ≥ 66 and drainage ≤ 9.
- Produces in `forge::regions`:
  - `pub struct RegionRecord { pub id: u16, pub kind: RegionType, pub cells: u32, pub alignment: i8 /* -1 evil, 0, 1 good */ }`.
  - `pub fn flood(biome: &[u8], drainage: &[u8], n: usize) -> (Vec<u16>, Vec<RegionRecord>)`:
    4-connected; frozen types always split; forest spans taiga..tropical.
  - `pub fn assign_alignment(regions: &mut [RegionRecord], seed: u64, good_share: f64, evil_share: f64)`:
    by size buckets (small ≤ 24 cells ×k, medium, large; DF rule scaled to our cell count).
  - `pub fn savagery(seed: u64, n: usize, cell_m: f64) -> Vec<u8>`: 12 km first-octave noise.
  - Knobs `good_share` 0.1, `evil_share` 0.1.
  - Artifact: `biome: Vec<u8>`, `region: Vec<u16>`, `regions: Vec<RegionRecord>`, `savagery: Vec<u8>`.

- [ ] **Step 1: Write the failing tests.**
  - In `game/src/biome.rs` `#[cfg(test)]`, a table test `classify_table` with one row per biome.
    For example:
    - `CellClimate { height_m: 200., temperature_c: 10., precip_mm: 1500., drainage: 50, salinity: 0, is_lake: false }`
      → BroadleafForest;
    - `precip_mm: 1100., temperature_c: 0.` (conifer row) → Taiga;
    - `precip_mm: 100., drainage: 20` → SandDesert;
    - `temperature_c: -8., drainage: 80` → Glacier;
    - `height_m: 1500.` → Mountain.
  - `region_type_hills_needs_high_drainage`.
  - In `regions.rs`:
    - `flood_splits_frozen_but_joins_forests`;
    - `alignment_hits_target_shares` (±5%).
- [ ] **Step 2: Run them to see them fail.**
  `just test-one game "--lib biome::"`; `just test-one worldgen "--lib forge::regions::"`.
- [ ] **Step 3: Implement**, and store the fields in the artifact. Also expose:
  - `ForgedWorld::cell_climate(x_m, z_m) -> CellClimate`;
  - `ForgedWorld::biome_cell(x_m, z_m) -> Biome`: the nearest cell, no dither; Task 16 adds the
    dither.
- [ ] **Step 4: Print biome shares and region counts per type** in the forge summary
  (`examples/forge_proto/main.rs`). Run:
  - `just test-one worldgen "--lib forge::"`;
  - `just test-one worldgen "--test forged_world"`;
  - `just smoke`.
- [ ] **Step 5: Commit** `feat: DF-style biome classification, named-region flood fill, savagery and alignment`.

## Phase G — Biomes in play (§5b)

### Task 15: New blocks and textures

**Files:**
- Modify: `assets/blocks/blocks.ron` (append ids 22+)
- Create: `assets/blocks/<name>.png` (16×16, one texel per voxel)
- Create (optional, the builder's call): `tools/textures/gen.py` to generate them

**Interfaces:**
- Produces block names:
  - ground and water: `snow`, `ice`, `glacier_ice`, `mud`, `peat`, `gravel`, `red_sand`,
    `dry_grass` (Faces: top / side / dirt bottom), `podzol` (Faces), `frozen_dirt`, `salt`;
  - trees: `spruce_log`, `spruce_branch`, `spruce_leaves`, `acacia_*`, `palm_*`, `jungle_*`
    (log / branch / leaves);
  - `cactus` (solid, `collider: Half`).
  - Leaves are `solid: false, render: Cutout` like the existing ones.

- [ ] **Step 1: Write the failing test** `manifest_has_biome_blocks` (in the existing manifest
  test location, `crates/game`): every name above resolves to a non-air id, and every texture file
  exists.
- [ ] **Step 2: Run it to see it fail.** `just test-one game "--lib block"` (the manifest test
  module).
- [ ] **Step 3: Add the blocks and textures** in the existing style. Check them by eye with
  `just asset-viewer` or a test region.
- [ ] **Step 4: Run the passes.** Step 2 target; `just smoke`.
- [ ] **Step 5: Commit** `feat(assets): biome ground, ice and tree blocks`.

### Task 16: Biome-aware ground cover, dithered borders, ice, glaciers, snow line

**Files:**
- Modify: `crates/worldgen/src/material.rs` (`Materials`, new `GroundCover`)
- Modify: `crates/worldgen/src/forged.rs` (`biome_at` with dither; `ForgedPatch`)
- Modify: `crates/worldgen/src/lib.rs` (`generate_region_forged` path, water and ice placement)
- Test: `crates/worldgen/tests/forged_world.rs`, `crates/worldgen/src/material.rs` tests

**Interfaces:**
- Produces:
  - `Materials` gains `biome: BiomeBlocks` (resolved by name; `BiomeBlocks::NONE` when names are
    missing).
  - `pub struct GroundCover { pub surface: BlockId, pub soil: BlockId, pub soil_depth: i32 }`.
  - `pub fn ground_cover(&self, biome: Biome, above_snow_line: bool, block_y: i32) -> GroundCover`.
    The table follows spec §5b. `material_at` keeps its exact behaviour and is what the
    procedural path still calls.
  - `ForgedWorld::biome_at(x_m: f64, z_m: f64) -> Biome`. Each block column takes the biome of
    the cell nearest to the point displaced by a 2-octave noise of amplitude
    `BIOME_DITHER_M = 24.0` (DF-style neighbour pick).
  - `ForgedWorld::snow_line_m(x_m, z_m) -> f32`: the height where `temperature_c` reaches
    `SNOW_LINE_C = -2.0`.
  - Ice:
    - where `temperature_c < 0`, the top `ice_blocks = clamp(round(-temperature_c / 4), 1, 4)`
      water blocks of lakes and sea become `ice`;
    - glacier cells get `glacier_ice` of thickness `clamp(round(-temperature_c), 4, 40)` blocks
      above the ground.
    - Rivers stay water.

- [ ] **Step 1: Write the failing tests.**
  - In `material.rs`: `ground_cover_table` (desert → sand over sand ≥ 6 deep; swamp → mud over
    peat; tundra → frozen_dirt; alpine above the snow line → snow over rock; temperate → grass over
    dirt = `material_at`'s result).
  - In `forged_world.rs`, on a synthetic artifact with a desert half and a glacier half:
    - `desert_region_surface_is_sand`;
    - `glacier_region_has_ice_over_rock`;
    - `frozen_lake_has_ice_on_top`;
    - `biome_border_is_dithered_not_straight`: along the border line, ≥ 20% of block columns
      within 24 m on each side take the other biome.
  - `procedural_ground_unchanged`: `scene_hash_golden` stays green; just run it.
- [ ] **Step 2: Run them to see them fail.**
  `just test-one worldgen "--lib material::"`; `just test-one worldgen "--test forged_world"`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the passes.**
  - Step 2 targets.
  - `just test-one worldgen "--test it -- --include-ignored scene_hash_golden::"`.
  - `just test-one worldgen "--test it underground::"`.
  - `just determinism-wasm`.
  - `just smoke`.
- [ ] **Step 5: Commit** `feat(worldgen): biome ground cover, dithered borders, ice, glaciers, snow line`.

### Task 17: Vegetation per biome — new species, niches on absolute climate

**Files:**
- Modify: `crates/worldgen/src/trees/species.rs` (`SPECIES` grows), `trees/placement.rs`,
  `crates/worldgen/src/habitat.rs` (`Habitat` gains `biome: Option<Biome>`), `material.rs`
  (`TreeBlocks` sized by `SPECIES.len()`)

**Interfaces:**
- Produces:
  - `SPECIES` = oak, pine, birch (unchanged, indices 0–2), then spruce, acacia, palm, jungle,
    cactus.
  - `Species` gains `biomes: &'static [Biome]`. Empty = any biome; that is the old three, so
    procedural placement is unchanged.
  - The new species are eligible only when `Habitat.biome` is `Some(b)` with `b` in their list;
    the procedural habitat sets `biome: None`.
  - Tree density per biome: `const BIOME_TREE_DENSITY: [(Biome, f32); N]` multiplies placement
    probability. Desert ≈ 0.02 (cactus); grassland 0.05; forests 1.0; jungle 1.4.

- [ ] **Step 1: Write the failing tests.**
  - `procedural_trees_unchanged`: run the existing `trees_in_terrain::` and `tree_chop::` suites
    unchanged.
  - `taiga_grows_spruce_not_oak` and `savanna_grows_scattered_acacia` (`placement.rs` tests with
    a stub `Habitat` carrying the biome).
  - `each_new_species_skeleton_fits_its_crown` (in `trees/` tests, like the existing species
    tests).
- [ ] **Step 2: Run them to see them fail.** `just test-one worldgen "--lib trees::"`.
- [ ] **Step 3: Implement.** Check each species with
  `just tree-view --species <name> --seed 1 --age 60 --grid`.
- [ ] **Step 4: Run the passes.**
  - `just test-one worldgen "--lib trees::"`.
  - `just test-one worldgen "--test it trees_in_terrain::"`.
  - `just test-one worldgen "--test it -- --include-ignored tree_chop::"`.
  - `just test-one worldgen "--test it -- --include-ignored scene_hash_golden::"`.
  - `just smoke`.
- [ ] **Step 5: Commit** `feat(worldgen): biome trees — spruce, acacia, palm, jungle, cactus`.

## Phase H — Surfaces: world map, generator, tools (§8–§11)

### Task 18: World map tiles with biome and climate; pan, zoom and layers

**Files:**
- Modify: `crates/game/src/world_map.rs` (`TileSummary`)
- Modify: `crates/worldgen/src/tile_map.rs` (`summarize_tile`, `rasterize`, `tile_info_lines`)
- Modify: `crates/client/src/renderer/world_map.rs`, `crates/client/src/renderer/slint_overlay.rs`,
  `crates/slint-ui/ui/world-map.slint`
- Test: `crates/worldgen/tests/tile_map.rs`, `crates/slint-ui/tests/` (pointer and zoom hit-test)

**Interfaces:**
- Produces:
  - `TileSummary` gains `biome: Biome` (mode over the tile), `region: u16`, `temperature_c: f32`,
    `precip_mm: f32`, `savagery: u8`, `alignment: i8`, `volcano: bool`.
  - `pub enum MapLayer { Biome, Relief, Temperature, Precipitation, Regions }`.
  - `pub fn rasterize_layer(map: &WorldMap, layer: MapLayer) -> (u32, u32, Vec<[u8; 4]>)`.
  - `tile_info_lines` adds biome, region (type + number), climate lines.
  - The Slint map: wheel zoom about the cursor, drag pan, layer buttons, clamped to the map.
  - Tile selection works at every zoom.

- [ ] **Step 1: Write the failing tests.**
  - `summary_carries_dominant_biome` (tile_map).
  - `rasterize_layer_sizes_and_colours` (one pixel per tile; biome colours distinct).
  - `world_map_packet_fits` (`encode_packet` of a 196×196 `WorldMap` < 8 MB).
  - Slint: `world_map_zoom_keeps_tile_under_cursor`.
- [ ] **Step 2: Run them to see them fail.**
  `just test-one worldgen "--test it tile_map::"`; `just test-one slint-ui "--test it world_map"`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the passes.**
  - Step 2 targets.
  - `just test-one client "--lib world_map::"` (ask the user first: a client test binary is a slow
    build).
  - `just smoke`.
- [ ] **Step 5: Commit** `feat: world map shows biomes and climate, with pan, zoom and layers`.

### Task 19: Generator sizes, preview cell, presets, defaults, safe cancel

**Files:**
- Modify: `crates/client/src/frontend/generator.rs` (`WORLD_SIZES`, `PREVIEW_CELLS`, defaults,
  forge-time labels)
- Modify: `crates/worldgen/src/forge/presets.rs` (+ tests)
- Modify: `crates/worldgen/src/forge/world.rs` (`DEFAULT_SIZE_M` stays 16 km; `DEFAULT_CELL_M = 32.0` from Task 10)
- Modify: `crates/slint-ui/ui/map-generator.slint`

**Interfaces:**
- Produces:
  - `WORLD_SIZES = [(16_000, "16 km"), (32_000, "32 km"), (64_000, "64 km"), (100_352, "100 km (DF Medium)")]`.
    Each label shows the measured forge time from Task 5. The default index is 3.
  - `WORLD_CELLS` default 16 m.
  - `pub fn preview_cell_for(size_m: f64) -> f64`: the smallest of {32, 64, 128, 256} with
    `(size_m / cell)² ≤ 262_144`. It replaces the preview cell choice; the Advanced row keeps an
    override.
  - The Play forge shows "≈ N min" before starting.
  - Cancel stops within one iteration, and the temp file is removed (`save_temp_path` is never
    renamed on cancel).
  - Presets, in order:
    - Continent (= Default, no overrides);
    - Continents;
    - Inland;
    - Temperate band;
    - Frozen north;
    - Tropical;
    - Great ranges;
    - Island, Archipelago, Island arc: layout 0/1/2 plus `pole = 0`, `latitude_center_deg = 45`,
      `sea_edges = 4`, `max_peak_m = 236` (today's look);
    - Highlands, Lowlands, Dry, Wet.

    Override values: the builder's call, within knob ranges.
  - The tests in `presets.rs` are updated (the names list, round trip, ranges).

- [ ] **Step 1: Write the failing tests.**
  - `preview_cell_for_sizes`: 16 km → 32, 32 km → 64, 64 km → 128, 100,352 → 256.
  - `cancelled_play_forge_leaves_no_file`: generator test with a tiny world; cancel at iteration
    2; neither `path` nor `save_temp_path(path)` exists.
  - The presets tests with the new names.
- [ ] **Step 2: Run them to see them fail.** `just test-one worldgen "--lib forge::presets::"`;
  the generator tests in `crates/client` (`--lib frontend::generator::`; ask first, slow build).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the passes.**
  - Step 2 targets.
  - `just test-one server "--lib world_file::"` (`ensure_forged` uses the 16 km / 32 m default).
  - `just smoke`.
- [ ] **Step 5: Commit** `feat(generator): DF-scale sizes, auto preview cell, continental presets, safe cancel`.

### Task 20: Seed search criteria, forge statistics, world-map tool layers

**Files:**
- Modify: `crates/worldgen/examples/forge_proto/search.rs` (`score`)
- Modify: `crates/worldgen/src/forge/hydrology.rs` (`print_report`)
- Modify: `crates/worldgen/examples/world_map.rs` (`--layer`); `crates/worldgen/src/forge/render.rs`
  (`PreviewLayer` gains Biome, Temperature, Precipitation, Drainage, Regions)

**Interfaces:**
- Produces: `score` components, each 0–1, weighted like today:
  - land share in 30–60%;
  - ≥ 3 peaks above the snow line;
  - ≥ 3 Mountain regions bordering non-mountain land;
  - ≥ 3 Forest regions;
  - a river of Strahler ≥ 4 reaching the sea or a land edge;
  - ≥ 10 distinct biomes;
  - a Glacier when `pole ≠ 0`;
  - regions ≤ `REGION_CAP = 2750`.

  The report prints biome shares, region counts per type, temperature and precipitation ranges,
  peaks above the snow line, and rivers by order.

- [ ] **Step 1: Move scoring into the lib.** It becomes
  `pub fn world_score(q: &Quality, biomes: &BiomeSummary) -> (f64, Vec<(&'static str, f64)>)` in
  `forge/quality.rs`, with `pub struct BiomeSummary { shares: Vec<(Biome, f64)>, regions_by_type: Vec<(RegionType, u32)>, peaks_above_snow: u32, mountain_regions_bordering_land: u32 }`.
  `search.rs` calls it.
- [ ] **Step 2: Write the failing test** `score_rewards_biome_variety` in `forge/quality.rs`: two
  synthetic summaries that are identical except for biome count (4 vs 12). The second scores
  higher, and its "biome variety" component is 1.0.
- [ ] **Step 3: Run it to see it fail.** `just test-one worldgen "--lib forge::quality::"`.
- [ ] **Step 4: Implement**, including `just world-map --world <file> --layer biome`. Update the
  justfile help text and CLAUDE.md.
- [ ] **Step 5: Run the passes.** `just test-one worldgen "--lib forge::"`; `just smoke`.
- [ ] **Step 6: Commit** `feat(forge): DF-style seed search criteria, biome stats, map layers`.

## Phase I — Live verification

### Task 21: Forge a DF-scale world and play it (needs the user's GPU session)

**Files:**
- Modify: the spec `docs/superpowers/specs/2026-11-01-df-scale-world-terrain-design.md` (results
  section)
- Modify: `CLAUDE.md` (world size, forge defaults, world-file version, biomes)

- [ ] **Step 1: Ask the user, then forge.**
  `just forge-proto --size 100352 --cell 16 --search 0 --out worlds/df-medium/terrain`, or Play
  from the main menu at 100 km. Record time and RSS.
- [ ] **Step 2: Play** with `just adapter=Intel play`, with the user, on a mountain region with a
  ≥ 2 km summit:
  - stand and path on the summit;
  - dig;
  - look at the LOD ring for steep-face gaps.

  Record chunk count, snapshot size and genesis time vs a lowland region (`[perf]` lines:
  `region_genesis`, `snapshot_sent`). Check:
  - the ≤ 3× budget;
  - that the snapshot is far below `MAX_DECOMPRESSED_PACKET_BYTES` (2 GiB);
  - that `PRISTINE_TERRAIN_CACHE_MAX` memory stays sane;
  - that the cave-cull box (F4 on, `meshing/visibility.rs`) does not explode with ~1 km of
    in-region relief.
- [ ] **Step 3: Screenshot one region per biome** (glacier, tundra, taiga, temperate forest,
  grassland, savanna, desert, badlands, swamp, jungle, alpine). Fix what looks wrong, or list it.
- [ ] **Step 4: Write the results into the spec**: measured times, sizes, budgets, open issues.
  Update CLAUDE.md's Forged worlds / World size paragraphs.
- [ ] **Step 5: Commit** `docs: DF-scale terrain results; CLAUDE.md updated`.
