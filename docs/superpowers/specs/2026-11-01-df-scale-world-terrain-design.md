# DF-scale world terrain — design

Status: draft for review, 2026-11-01. Next step after approval: an implementation plan
(`superpowers:writing-plans`). Follow-on design (separate, later): civilizations, territories and
religions on top of this world, see "Next project".

## Intent

The world forge (`crates/worldgen/src/forge/`, `just forge-proto`, the main-menu map generator)
was built for the old island-MMO plan: one 16 km island, ocean on every side, peaks capped at
~236 m, rain only from the west, and no temperature or biomes. The game is now a Dwarf
Fortress–style game, and the world should read like a DF world:
- **a real continent at DF Medium scale (~100 km)**;
- **land running off some edges and sea on others**;
- **kilometre-plus mountain ranges**;
- **climate bands from pole to equator**;
- **biomes derived from physical causes, grouped into named regions**.

The user confirmed the DF pipeline as summarised in this session as "what I envision". They chose
DF Medium scale ("forging time doesn't matter") and real kilometre-plus ranges.

Why now: the civilizations / territories / religions layer (the next design) places and spreads
civs by biome, elevation and reachability. Those fields must exist first.

Research behind this:
- `docs/superpowers/specs/2026-09-20-df-worldgen-research/` (DF's fields, biome table,
  hierarchy);
- `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/14-history-civilizations-religion.md`;
- `docs/superpowers/specs/2026-10-31-azgaar-fantasy-map-generator-research.md`.

## Goals

1. **Scale**: a default world of DF Medium size, ~100 km across. Smaller sizes stay available.
2. **Shape**: continental layouts with per-edge sea or land. The island layouts remain as presets.
3. **Relief**: ranges of 1–3 km above sea level, playable (standable, pathable, diggable) at the
   top.
4. **Climate**:
   - temperature from latitude and altitude;
   - prevailing winds by latitude band;
   - rain shadows from any wind direction;
   - drainage derived from terrain;
   - volcanism.
5. **Biomes**: DF-style classification from the fields. Contiguous biome groups become regions
   with ids. Savagery and good/evil are stored as gameplay overlays.
6. **Biomes visible in play** (user, 2026-11-01): a tundra, glacier, desert, swamp or jungle
   region generates and looks like one at walking scale (§5b).
7. **World file**: a compact new format, so a 100 km world fits comfortably in server memory.
8. **Presets** in the forge and the main-menu generator for the new shapes and climates.
9. **Seed search** with DF-like world-quality criteria.

## Non-goals

- Civilizations, sites, territories, religions, history, names (the next design).
- Creatures and wildlife per biome (they belong with the creature roster and the civ layer).
- Ground plants and farming (DF's plant system).
- Settlements: these are generated procedurally on region load and are out of scope.
- **Long view distance.** The 1 km LOD horizon, fog and far clip stay as they are, so distant
  ranges are not visible from afar. Pushing the horizon to ~10 km+ is a separate rendering
  project (coarser rings, the coarse-LOD cliff holes, fog, shadows). Confirmed by the user:
  "view distance later".
- Caverns, magma sea and the underworld layer (DF's underground features).
- Changing region size (512 m), chunk size or the sim.
- Real-time weather and seasons.

## Anti-goals (failure even if it "works")

- Mountains that are just scaled-up 236 m hills: smooth domes with no valleys, or one material
  all the way down.
- Biomes painted by noise instead of derived from climate and terrain.
- A procedural (non-forged) world, test fixture or hash golden that changes silently. Every such
  change must be deliberate and re-pinned (see Constraints).
- A world that only loads on a 32 GB machine.

## Measured forge cost (this machine, one core, release, current forge, 500 iterations)

| World | Cell | Cells | Wall time | Peak RSS | World file (today's format) |
|---|---|---|---|---|---|
| 16 km | 64 m | 62 k | 1.3 s | 14 MB | 0.7 MB |
| 16 km | 32 m | 250 k | 6.0 s | 46 MB | 2.8 MB |
| 16 km (today's default) | 16 m | 1.0 M | 29 s | 171 MB | 11 MB |
| 32 km | 32 m | 1.0 M | 34 s | 173 MB | 11 MB |
| 64 km | 32 m | 4.0 M | 3 min 32 s | 679 MB | 44 MB |
| 100 km | 64 m | 2.4 M | 2 min 13 s | 425 MB | 28 MB |
| 100 km | 32 m | 9.8 M | 9 min 19 s | 1.4 GB | 107 MB |
| 100 km | 16 m | 39 M | 1 h 9 min (4,128 s) | 5.5 GB | 422 MB |
| 16 km, optimised (Tasks 1-5) | 32 m | 250 k | 0.74 s | 49 MB | 2.7 MB |
| 16 km, optimised (Tasks 1-5) | 16 m | 1.0 M | 2.3 s | 181 MB | 10.7 MB |
| 100 km / 16 m, optimised — not yet measured (needs user go-ahead) | 16 m | 39 M | — | — | — |

Notes:
- Cost grows a bit faster than linearly with cell count, and the forge is single-threaded.
- The "optimised" rows (2026-11-01, `just forge-bench <size> <cell>`, wall time of the
  release binary incl. PNG/world-file writing) are multi-threaded (rayon, all 20 logical
  cores) and use the coarse-to-fine schedule: 500 iterations at 4× the cell, 40 at 2×, 80
  at the cell (§12 item 1). Before the schedule (Tasks 1-4 only) the same runs took
  1.44 s / 49 MB (32 m) and 7.15 s / 187 MB (16 m).
- These runs used today's island layout stretched to the size, so the new layouts, climate and
  biome passes will add to them.
- Larger basins may need more iterations to reach steady state: the 100 km runs still had
  hundreds of cells under depressions at iteration 500.

## Constraints

- The forge is offline, single-machine and not bit-reproducible across machines (by design,
  `forge/mod.rs`). The world file is the canonical artifact.
- `game` and `server` stay engine-agnostic. The sim's determinism is untouched: none of this runs
  in the sim. Note that `generate_region` output for forged worlds *does* change.
- **The procedural path** (`VOXEL_WORLD_FILE=procedural`, tests, wasm `?offline`) must stay
  bit-identical, or its goldens must be re-pinned on purpose:
  - `scene_hash_golden`, `wasm_determinism` and the worldgen tests;
  - in particular the density ramp constants (`pipeline.rs:99-103`) shape procedural terrain
    everywhere.

  Preferred: extend the vertical band for forged profiles without moving the procedural ramp.
- Server memory: the whole world file is held in memory by the server (`ForgedWorld`).
- Old `.world` files: the version bumps. Worlds in `worlds/` are git-ignored dev data, so no
  migration is needed. The loader refuses an old version with a clear "re-forge this world"
  message, and the main menu shows it as such. *My call.*

## The design

### 1. Scale and units
- **Default world size**: a whole number of regions near 100 km, e.g. 196 × 512 m = 100,352 m
  (exact value: builder's call).
- **Default forge cell**: 16 m, as today ("forging time doesn't matter").
- **Generator sizes**: 16 km, 32 km, 64 km, ~100 km (default). Each label shows its approximate
  forge time from the table above.
- **World-map grid** at the default: 196 × 196 region tiles (≈ 38 k). Today's map is 32 × 32.

### 2. Shape: layouts and edges
- New layout choices next to One island / Archipelago / Island arc:
  - **Continent** (new default): one large landmass with sea on some edges, land to the others.
  - **Continents**: 2–4 large landmasses with seas between.
  - **Inland**: land on every edge, an inland sea and closed basins.
- **Per-edge setting**: sea or land, with a "random, at least N sea edges" mode like DF's
  `PARTIAL_OCEAN_EDGE_MIN`.
  - A land edge is an open drainage boundary: rivers may leave the map there.
  - The base level at a land edge is the edge cell's own height, i.e. a fixed outlet.
  - Erosion must not cliff the edge.
  - Builder's call how: e.g. edge cells as fixed-height outlets, plus a gentle uplift taper.
- **Mountain belts at continental scale**: today's belt noise has a 4 km wavelength and is gated
  "inland", which is island logic. For continents, belts should follow plate-like structure:
  - along the boundaries between landmasses or plates;
  - plus a few interior ranges;
  - with lengths of tens of km.
  - Noise wavelengths must scale with world size, or be set in km knobs, not hard-coded metres.
  - Mechanism: builder's call (e.g. Voronoi plates with convergent boundaries).
- **Existing knobs**: the island-only knobs keep working for the island layouts. The new layouts
  get their own group in `knobs.rs`.

### 3. Relief: kilometre-plus ranges (forge side)
- `max_peak_m`:
  - default ~2,500–3,000 m, range up to 3,500 m;
  - `fit_height_band` keeps its job, fitting relief into the engine band, with the band raised.
- Uplift / erodibility / iteration ranges retuned so 1–3 km peaks with real valley systems are
  reachable. Today's UI maximum gives ~1.9 km of total uplift at the default iterations.
- **Geology column depth**: `COLUMN_BOTTOM_S = -1800` and its debug assert (`geology.rs:38-41`,
  `:113`). Extend it to cover the deepest exhumation (≥ 4 km), with formations to match; granite
  basement may simply continue down.
- **Tree line and alpine bands**: `habitat.rs` uses a fixed lapse from 14 °C. Switch it to the new
  temperature field (§4) so the tree line follows climate.

### 4. Climate
- **Latitude**:
  - A world spans a latitude range, by default pole on one edge and equator on the opposite edge
    (DF-like: glacier → tundra → taiga → temperate → steppe/desert → jungle).
  - Knobs: pole placement (none / north / south / both, like DF's `POLE`) and the latitude span.
- **Temperature** (°C, stored):
  - sea-level temperature from latitude (knobs for equator and pole temperatures);
  - minus 6.5 °C/km lapse;
  - optional small ocean moderation near coasts.
- **Winds by latitude band**:
  - trade winds from the east (0–30°);
  - westerlies (30–60°);
  - polar easterlies (60–90°).
  - Band edges and directions are knobs.
  - `climate.rs` generalises from a west-only row sweep to a sweep along each band's wind
    direction. Moisture recharges over any sea, including inland seas.
  - Rain becomes an **absolute** annual precipitation (mm/yr) with a latitude modulation (wet
    equator, dry ~30° belts, wet ~50–60°; DF/FMG-style band table), not "relative to the land
    mean". Discharge weighting keeps using it.
- **Drainage** (0–100, derived, not random):
  - from local slope / relief, rock permeability (limestone and sandstone drain, shale doesn't),
    and height above the nearest river or lake;
  - flat, low and wet ground is poorly drained (marsh, swamp).
  - Formula: builder's call. Acceptance: swamps form on floodplains and deltas, never on slopes.
- **Volcanism** (0–100):
  - a field at continental scale, with hot spots along convergent belts;
  - volcano sites (conical peaks) where it maxes out.
  - Magma and igneous geology coupling: later.
- **Salinity**: sea = salt; endorheic (closed) lakes are salt, others fresh. Derived.

### 5. Biomes and regions
- **Classification** per cell, a pure function of the fields, following DF's table
  (`2026-09-20-df-worldgen-research/01-pipeline-and-fields.md` §3):
  - elevation gates: ocean, land, mountain;
  - land by rainfall × drainage: desert / rocky wasteland / badlands, grassland, savanna,
    shrubland, marsh, swamp, conifer forest, broadleaf forest;
  - temperature variants: frozen → tundra / glacier, cold forest → taiga, tropical variants;
  - salinity variants and mangrove.
  - The "mountain" cut: a height and/or relief threshold (knob, builder's call).
  - Thresholds map our absolute fields onto DF's 0–100 scales.
  - Stored per cell as `u8`.
- **Regions**: a flood fill of region type (DF's grouping: wetland, forest, grassland/hills,
  desert, tundra, glacier, mountains, lake, ocean) over the cells, each with an id and a type.
  Frozen types always split; a forest region may span taiga and jungle.
  - **Names** come from the next design's naming system. Until then the UI shows type + number.
- **Savagery** (0–100): a fractal field per cell.
- **Good/evil**: per region, assigned by region size buckets until target shares are met (DF's
  rule).
- Savagery and good/evil have no consumers yet. They are stored for creatures and civilizations.

### 5b. Biomes in play (`generate_region` for forged worlds)
The procedural world (tests, wasm `?offline`) stays as it is.
- **Per-column biome at block scale.**
  - The 16 m biome raster is not bilinear-thresholded.
  - Each block column takes a neighbouring cell's biome across a noise-dithered boundary (DF's
    "pick a neighbouring tile" refinement), so borders are irregular, not stepped or blurred.
- **Ground cover per biome.** `material.rs` `Materials::material_at` becomes biome-aware: surface
  block, soil block and soil depth per biome, with slope and altitude thinning kept.
  - temperate grassland / forest: grass over dirt (today);
  - steppe / savanna: dry grass;
  - sand desert: sand, deep;
  - rocky wasteland / badlands: gravel, red sand, bare rock;
  - marsh / swamp: mud, peat, with standing water in hollows if the forge's water allows;
  - taiga: podzol or grass;
  - tundra: frozen dirt / gravel with snow patches;
  - glacier: glacier ice over rock;
  - alpine above the snow line (from temperature): snow over rock;
  - beaches: sand; salt flats around closed salt lakes.
- **Frozen water.** Where the mean temperature is below freezing, lake and sea surfaces get an ice
  layer of whole blocks (thickness from temperature, builder's call). Glaciers are solid ice
  bodies of a few to tens of metres over the ground in glacier cells. Rivers stay liquid
  (no seasons yet).
- **Vegetation.**
  - Tree niches (`trees/species.rs`, `habitat.rs`) move from "rain relative to the land mean" and
    the fixed 14 °C lapse onto the forge's absolute fields: °C from §4, mm/yr, drainage.
  - Tree density follows biome and rain.
  - New species so each forest biome has its own trees: at least spruce (taiga), acacia
    (savanna), palm (tropical coast), a large tropical broadleaf (jungle), cactus (hot desert).
    Mangrove and dead trees are optional.
  - Each new species is a `ShapeParams` entry plus its log / branch / leaves blocks, viewable with
    `just tree-view`.
  - Ground plants (grass tufts, shrubs, crops) are NOT in scope: DF treats plants as a gameplay
    system of their own.
- **New blocks** in `assets/blocks/blocks.ron` (append only, stable ids): snow, ice, glacier ice,
  mud, peat, gravel, red sand, dry grass, podzol, frozen dirt, salt, plus the new species' blocks.
  - Every block needs 16×16 textures. How they are made is the builder's call: hand-drawn
    pixel art, or a small script that generates them in the existing style, checked by eye.
- **LOD and minimap**: the far rings' visible-from-above material and the minimap / world-map
  colours pick up the new surface blocks unchanged.
- **Verification**: per biome, one region screenshot at walking scale (glacier, tundra, taiga,
  temperate forest, grassland, savanna, desert, badlands, swamp, jungle, alpine), plus the worldgen
  tests for the material table and the tree niches.

### 6. World file v-next (compact)
Today: six full-resolution `f32` rasters (24 B/cell) + lz4 ≈ 11 B/cell on disk, and the same
rasters in server memory. At 100 km / 16 m that is ~430 MB on disk and ~940 MB in memory.

New format:
- **Drop** `coast_dist_m`; recompute it on load (`habitat::coast_distance_m`).
- **Quantise**:
  - height `u16` at 6.25 cm over −1 km…+3 km (or a fixed offset/scale in the header);
  - `uplifted` / `bedding` `u16`;
  - rain `u16` mm/yr;
  - temperature `i8` °C (or `i16` at 0.1 °C);
  - drainage / volcanism / savagery `u8`;
  - biome `u8`; region id `u16`.
- **Lakes**: a sparse list of lake polygons or cell runs + levels, instead of a NaN raster.
- **Compression**: row-delta the smooth rasters, then **zstd** instead of lz4. zstd is a new
  dependency, `worldgen` only; approved by the user.
- **New tables**: regions (id, type, area, good/evil), volcano sites.
- **Built (v3, Task 6)**: height / uplifted / bedding / rain are `QRaster`s (`u16`, `min + k * step`;
  min = floor of the data minimum, step 0.0625 m, widened to `span / 65535` when the span exceeds
  4095.9 m; rain step 0.001), lakes are bodies with run-length `(start, len)` cell runs (sparse,
  no NaN raster), coast distance is recomputed on load, and the body is zstd level 19 over
  bincode with every raster row-delta encoded. zstd is native-only; file read/write is
  `#[cfg(not(target_arch = "wasm32"))]`.
- **Measured** (16 km / 16 m = 1000 x 1000 cells, `just forge-proto --cell 16`, seed default):
  file 1,131,754 B = **1.13 B/cell** on disk (target was <= 4), so a 100 km / 16 m world
  (39 M cells) is ~44 MB. `ForgedWorld::load` of it takes 31 ms; process RSS delta around the
  load (a temporary example reading `/proc/self/status` VmRSS before/after, release build;
  includes the transient decompressed body) is 18.7 MB = **~19 B/cell**, i.e. ~0.75 GB at
  100 km / 16 m. Steady state is lower (8 B of `u16` rasters + 4 B decoded height + 4 B coast
  distance = 16 B/cell, ~0.62 GB at 100 km); `uplifted`/`bedding`/`rain` could be dropped to
  one copy later if memory presses.
- Header/version as today; an older version is refused (`ArtifactError::OldVersion`) with a re-forge message, which the main menu's world list shows as "needs re-forge: ...".

### 7. Engine changes for kilometre-plus terrain (from the 2026-11-01 code audit)
Hard limits:
1. **Density ramp and surface scan**:
   - `RAMP_TO_Y = 320` (`worldgen/src/pipeline.rs:99-103`) and `SCAN_TOP = 320`
     (`worldgen/src/lib.rs:44`): ground above block 320 is cut flat.
   - Raise the band for forged worlds (≥ 3,500 m above sea plus headroom) without moving the
     procedural ramp (Constraints).
   - Replace the linear top-down surface scan (`surface_from_profile`, `lib.rs:86-95`) with a
     start at the analytic `surface_y()` (`pipeline.rs:580`) or a bisection, so cost does not
     grow with the band.
2. **Forge height fit**: `max_peak_m` and its 256 m UI cap (`forge/knobs.rs:95`,
   `forge/post.rs:38`) — §3.
3. **Pathfinding**: `COLUMN_SCAN_Y_MIN/MAX = -256/512` (`game/src/pathfind.rs:27-29`), clamped
   at `adventure.rs:315`. Nothing can stand above block 512. Derive the range from the column's
   actual surface band (or widen it to the world band), and keep `nearest_standable` cheap.
4. **Geology column depth**: §3.
5. **Tools and colours**:
   - colour ramps that end at 250 m (minimap `minimap.rs:141-148`, `tile_map.rs:301-315`,
     `forge/render.rs:157-170`);
   - the `HEIGHT_MIN/MAX_M = -64/320` scales in `examples/world_map.rs`,
     `forge_proto/render.rs` and `seed_scan.rs`;
   - `tile_map.rs` terrain classes (`MOUNTAINS_MIN_M = 150`, etc.).
   - Rescale to the new band. Biome colouring replaces height-only colouring on the world map.

Costs to watch (measure, don't pre-optimise):
- **Chunks per column**: the stored band grows with local slope (≈ 17·slope/16 + 1 chunks), not
  total relief. Measure region chunk counts, snapshot size and genesis time on a mountain region
  vs a lowland one. Check against `MAX_DECOMPRESSED_PACKET_BYTES` (2 GiB cap,
  `protocol.rs:552-561`) and `PRISTINE_TERRAIN_CACHE_MAX` (`world_manager.rs:33-43`).
- **Client cave-cull BFS box**: a dense box over the loaded y range (`meshing/visibility.rs`).
  It is off by default; check it stays sane with ~1 km of in-region relief.
- **Coarse-LOD steep-face holes** (`worldgen/src/lib.rs:609-612`; inferred from code, not
  observed): fills only ~4 blocks under each coarse cell's surface, so steep faces may show gaps
  in LOD rings. Verify visually on a mountain region; fix (fill down to the lowest neighbour)
  if seen.
- Edit-ray reach and other absolute-height terms (`orders.rs:554`): check they work at y ≈ 3000.

Checked and fine (no change): chunk y and LOD group y are `i32`; ground recipes are `i16`; quad
records are brick-local; GPU origins are `f32` (fine at 3 km); worldgen cache keys; spawn height.

### 8. World map and generator UI
- **`TileSummary`** gains:
  - dominant biome;
  - region id;
  - mean temperature;
  - annual rain;
  - savagery / alignment;
  - volcanism flag.

  `WorldMap` for 196² tiles ≈ 38 k tiles (a few MB before lz4) is still sent once on connect.
  Check the packet size.
- **World map screen**:
  - pan and zoom (196² tiles do not fit a panel at 1 px per tile; today's map is 32²);
  - layer toggles: biomes (default), relief, temperature, rain, regions;
  - the selected tile's summary lines show biome, region, climate.
- **Main-menu generator** (`client/src/frontend/generator.rs`):
  - size choices 16 / 32 / 64 / ~100 km, labelled with forge time;
  - the new layout and climate knob groups;
  - the presets below.
  - The preview needs a coarser cell at large sizes. 100 km at 64 m is 2 min 13 s, far too slow
    for a live preview. Use e.g. 256 m (≈ 150 k cells, ~1 s, extrapolated); builder's call.
  - The Play forge at 100 km / 16 m takes about an hour and several GB of RAM in-process, with
    the existing progress bar. Acceptable per the user, but show the estimate before starting.

### 9. Presets (`forge/presets.rs`)

| Preset | Overrides |
|---|---|
| **Continent** (new Default) | Continent layout, 2 random sea edges, pole-to-equator, 2.5–3 km ranges |
| Continents | 2–4 landmasses, seas between, ranges at their boundaries |
| Inland | land edges all round, inland sea, closed basins, drier |
| Temperate band | no pole, narrow latitude span: mostly forest / grassland |
| Frozen north | pole edge, cold: tundra, taiga, glaciers |
| Tropical | equatorial span: jungle, savanna, wetlands |
| Great ranges | taller, broader belts, harder rock |
| Island / Archipelago / Island arc | today's layouts and tuning, kept as they are |
| Highlands / Lowlands / Dry / Wet | kept, applied on top of the new default where sensible |

The preset tests (`presets.rs` tests: names list, round trip, ranges) are updated to match.

### 10. Seed search (`examples/forge_proto/search.rs`)
DF-like criteria, scored like today's `world score`:
- land share;
- a peak count above the snow line;
- at least N mountain regions with adjacent non-mountain land (future dwarf starts);
- forest regions;
- rivers that reach the sea (or leave a land edge) with Strahler order ≥ 4;
- at least K distinct biome types;
- a glacier present when a pole is on the map;
- a cap on the number of regions (DF's `SUBREGION_MAX`: no confetti).

### 11. Statistics and verification
- **Forge summary lines**: biome shares, region counts per type, temperature range, mm/yr range,
  peak count above the snow line, rivers by order.
- **`just world-map --world <file>`** and the forge PNGs gain biome, temperature, rain, drainage
  and region layers.
- **A mountain region played live**:
  - stand and path on a 2 km summit;
  - dig;
  - look at the LOD ring for steep-face holes;
  - record chunk count, snapshot size and genesis time vs lowland.
- **Tests**:
  - biome classification as a pure function (table cases);
  - world file round trip and size;
  - the procedural path bit-identical, or goldens re-pinned with a note;
  - `just smoke`;
  - `just test-one worldgen …` for the touched suites.

### 12. Forge performance (user: "that's too long, look for ways to optimize it")
**Profile** (`perf`, 16 km / 16 m, 29 s, one core):

| Where | Share |
|---|---|
| `erosion::route`, the Priority-Flood with a `BinaryHeap` over all cells, every iteration (heap pop alone 26%) | 66% |
| `erosion::step`: incision + diffusion | 17% |
| `Geology::surface_rock` per cell per iteration | 7% |
| `discharge` | 3.5% |
| memset/memmove of ~6 full-size arrays allocated per iteration | 4% |

At 100 km / 16 m, iterations slow from ~6 s to ~11 s as depression cells (heap size) grow.

**Changes**, in order of expected win (estimates, measure each):
1. **Coarse-to-fine schedule** (~5× less work).
   - Run most iterations at coarse cells, upsample the state (h, uplift, uplifted, ocean), then a
     few equilibration iterations at each finer level. E.g. 400 @ 64 m → 75 @ 32 m → 50 @ 16 m.
   - The output changes, which is acceptable: the artifact is canonical.
   - Acceptance: the forge statistics hold at the final scale vs a full fine run on the 16 km
     world:
     - concavity θ;
     - Hack exponent;
     - hypsometry;
     - river and lake counts;
     - peak heights.
2. **Linear-time routing** (removes most of the 66%).
   - Steepest-descent receivers on `h` plus downstream-first order by tree traversal from outlets.
   - Depressions handled by a basin graph (Cordonnier et al. 2019) instead of a full Priority-Flood
     each iteration.
   - Keep the full Priority-Flood for the initial condition and the final analysis.
3. **Multi-threading** (~3–6× on the per-cell work, rayon).
   - Diffusion, geology erodibility and receivers are per cell.
   - Discharge and implicit incision are per drainage basin.
   - Output must stay reproducible on one machine (no order-dependent float reductions).
4. **Small**: reuse scratch arrays across iterations; refresh geology erodibility every few
   iterations.

Not: `f32` heights. At 3 km heights the `f32` step (~0.25 mm) is coarser than the flood's per-cell
fall (`FLOOD_EPSILON` × 16 m = 0.16 mm), so it would break drainage.

**Targets**:
- 100 km / 16 m in ≤ 10 min on this machine (measured today: 69 min, 5.5 GB).
- A 100 km preview in the main-menu generator at 256 m cells in ~1 s.

Do this first in the plan: every later tuning loop at 100 km depends on it.

## Decisions from the builder check (my calls unless marked)

- **Defaults.** `ForgeParams::default()` stays a quick dev world: Continent layout, 16 km, 32 m
  cells, ~10 s. Three paths keep using it:
  - the server's missing-world auto-forge (`server::world_file::ensure_forged`);
  - tests;
  - `just forge-proto` without flags.

  The DF-scale default (~100 km / 16 m) is what the main-menu generator selects and what the
  Continent preset's size choice defaults to. A fresh checkout never waits an hour.
- **Beyond a land edge.** The playable world ends at the map edge, as today:
  - Play outside is refused (`PlayRefusal::OffMap`), and walkers and bodies cannot cross the outer
    seam.
  - For the LOD rings past a land edge, `forged.rs` extends the edge rasters outward (clamped edge
    height, rivers stop) instead of returning ocean at `OUTSIDE_HEIGHT_M`. The horizon then shows
    land continuing, not a cliff into sea.
- **Mountain-region budget.**
  - Targets: chunk count ≤ 3× a lowland region of the same world; snapshot and genesis time within
    3× as well; nowhere near the 2 GiB packet cap.
  - The plan measures this early (a mountain region from the first forged 2 km range).
  - If it overruns, the overrun is recorded and fixed in a follow-up engine plan, not here. This
    plan stays worldgen + the five hard limits of §7.
- **Memory targets.**
  - The forge at 100 km / 16 m: ≤ 8 GB peak.
  - The server holding the world: ≤ 1 GB.
  - Client + server together during a single-player game: ≤ 6 GB on top of today's footprint.
  - If the 16 m forge exceeds 8 GB once the new fields are in, the default forge cell becomes
    24–32 m. Measure, then decide.

## Left to the builder
- The plate / belt mechanism.
- The land-edge boundary handling.
- The drainage formula.
- The precipitation band table.
- The biome threshold mapping onto our absolute units.
- The quantisation scales.
- The preview cell size.
- The exact world size.
- Iteration counts for steady state at 100 km.
- How to extend the vertical band without touching the procedural ramp.

## Next project
Civilizations, territories and religions (DF-style: entities with biome preferences, start sites,
territory spread per biome, reachability-based neighbours, pantheons from spheres, a history
simulation). It builds on §4–§5 and the world file of §6. Settlement generation on region load
stays out of scope there too.
