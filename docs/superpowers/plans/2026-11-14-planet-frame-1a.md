# Planet Frame (Phase 1a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Re-based 2026-11-19** (program `docs/superpowers/plans/2026-11-19-earth-like-world-program.md`, Phase 0) onto
> `c16f7e3d`. Tasks 1–2 are BUILT (`8d6b29a2`, `77f8e8a3`; Task 2 reviewed clean 2026-11-19) and kept below only as
> a record — their test steps describe tests that were since deleted. Tasks 3–19 are new text: the old Tasks 3–17
> re-derived from today's symbols (code check: `.superpowers/sdd/2026-11-14-planet-frame-1a/rebase-code-check.md`),
> plus two tasks the old plan lacked (7: halo, intents and global block positions across the seam; 13: trees,
> structures and towns at the seam). Old → new numbers: 3→3, 4→4, 5→5, 6→6+7, 7→8, 8→9, 9→10, 10→11, 11→12+13,
> 12→14, 13→15, 14→16, 15→17, 16→18, 17→19. **No step commits a test.**

**Goal:** Every forged world is a 2:1 planet in one of four sizes: it wraps east–west, has sea-and-ice poles on the north and south edges and a hot equator across the middle, and it is the only kind of world the forge makes.

**Architecture:** Three moves, in an order that keeps a working world at every commit. (1) The forge, the artifact, the tiles and the world map go from one `size_m` / `n` to `width × height` / `nx × nz`, proven a pure refactor by the existing bit-exact pins. (2) The wrap: every stored, simulated, hashed or sent `RegionCoords` is canonical (x folded into the map), the forge's grid, noise and geometry are periodic in x for the new Planet layout, the runtime sampler folds x, and the client renders through an unwrapped frame. (3) The old layouts, presets and knobs are deleted, the quality gate is re-recorded on a Planet world, and every size is measured.

**Tech Stack:** Rust; crates `worldgen` (forge, `forged.rs`, `forged_tiles.rs`, `tile_map.rs`), `game`, `server`, `world-store`, `client` (Bevy, Slint). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` (Phase 1, part 1a). The older `docs/superpowers/plans/2026-11-05-east-west-world-wrap.md` is superseded by this plan; its design points D1 and D3–D9 are restated below where a task relies on them. Code facts for Tasks 3–19 were gathered 2026-11-19 at `c16f7e3d`; search by the named symbol, never by line.

Phase 1b (the stored monthly-climate family) is a separate plan, written after this one lands.

## Global Constraints

- Do not create git worktrees; work on `develop`. No stash, reset, amend, checkout of files, clean, push. The tree
  holds the owner's unrelated uncommitted changes and other sessions stage in the same index: each commit carries
  only the task's own hunks, committed with an explicit pathspec.
- **Tests.** The repository commits only regression tests of bugs actually seen and the existing gates. Every
  "temporary test" below is written, run, quoted in the implementer's report (name, command, passing output) and
  DELETED before the commit; a commit adds no `#[test]`, and `just test-audit` (first step of `just smoke`) passes.
  An "evidence command" is a tool run whose output is quoted in the report. Client lib tests are run targeted
  (`just test-one client "--lib <filter>"`) without asking — ruling of 2026-11-15, kept.
- **The safety net until Task 17** (the old layouts must not change while the frame is built under them):
  1. the gate `forge_quality_matches_baseline` (`crates/worldgen/tests/forge_lib.rs`, fixture
     `tests/fixtures/forge_quality_16k_32m.ron`) passes UNTOUCHED:
     `just test-one worldgen "--test it -- --include-ignored forge_lib::"`;
  2. the six old layouts forge to the same height raster as before the task: the **layout crcs** — the
     `height_crc` line of `forge_proto --report` (Task 3) for `--set layout=0` … `5` at the size Task 3 records —
     equal the six values in the ledger (`.superpowers/sdd/2026-11-14-planet-frame-1a/progress.md`, "Layout crcs").
  Every task from 3 to 16 that touches `crates/worldgen/src/forge/` or `forged*.rs` runs both and quotes the six
  crcs. A changed crc or a failing gate is BLOCKED, never a re-record.
- There is never a commit with no working world: old layouts are deleted only in Task 17, after a Planet world
  forges and generates regions.
- No forge-time or memory LIMIT is added or enforced; the existing RAM check against physical memory stays.
- **The wrap is for one process** (`VOXEL_ROLE=all`); under any other role the seam is CLOSED (Task 16: the map is
  installed with `wrap_x = false`, so east and west hold like today's outer edges and split-role worlds keep
  running). Do not change `halo::neighbours`, `world_manager/peers.rs`,
  `peer/`, `ownership.rs` (`super_region_of`), `world_manager/land.rs`, `succession.rs`, `drain.rs`,
  `world_cut.rs` or anything else on the host-to-host path beyond what a task names (the seam between hosts is the
  program's Phase 7). What those files REQUIRE is that every `RegionCoords` they see is canonical.
- `game`, `server`, `worldgen` stay Bevy-free; wire format stays `game::encode_packet` / `decode_packet`; hash with
  `StateHash` (derive on any new state type); `BTreeMap`, never `HashMap`, in sim and history; no inherent `f32`
  transcendentals in sim paths (`libm`).
- Never write a region edge as a literal: derive from `REGION_SIZE` / `REGION_BLOCKS`. Tests build at region 64,
  play at 256.
- Runtime sampling in `forged.rs` keeps to exact arithmetic (add, subtract, multiply, divide, floor, sqrt).
- Verification per task: its temporary tests; `just test-one <crate> "<filter>"` for the modules of the files it
  touched where tests exist; `just smoke`; `just determinism-wasm` after Tasks 5, 6, 7, 9, 12, 13 and 17 (sim math,
  hashed or serialized state). Whole suites (`just test`) once, in Task 19. A test slower than 100 ms that stays
  committed (only the gate) keeps its `#[ignore = "slow: >100ms — run via just test-full"]`.
- Any new or changed build/run/test command updates its `justfile` recipe in the same task.
- **Stored formats.** Task 2 bumped all four (artifact 8, tiles 2, world map 2, meta 2). Worlds were forged at
  those versions during the pause, so two more bumps are ruled, each once: `WORLD_MAP_VERSION` 2 → 3 in Task 5
  (the `WorldMap` gains `wrap_x`; a stored map of another version is rebuilt from the archive, not a re-forge) and
  `ARTIFACT_VERSION` 8 → 9 in Task 17 (with `WorldGlobals::validate` refusing any edges but a Planet's, a world of
  a deleted layout is refused with the re-forge wording on every path). The link protocol is untouched (`WorldMap`
  never travels on a link). No other task may change a stored or wire byte layout; one that finds it must reports BLOCKED.
- Rulings made by a task are written in its report under "Rulings" as `Ruling: … — why — cost if wrong`.

## Shared vocabulary (decided here, used by every task)

- Axes: x runs west → east and wraps; z runs north (row 0) → south. `nx` columns, `nz` rows, row-major `i = z * nx + x`.
- `ForgeParams { width_m, height_m, cell_m, seed, knobs, quiet }`. `World { nx, nz, dx, wrap_x, … }` (`wrap_x` from Task 10).
- `WorldArtifact` / `WorldGlobals`: `width_m`, `height_m`, `cell_m`, `nx`, `nz`, `edges`, `knobs: BTreeMap<String, f64>`.
- World sizes (`forge::play`):

  | Name | `width_m` | `height_m` |
  |---|---|---|
  | Tiny | 25,600 | 12,800 |
  | Small | 50,176 | 25,088 |
  | Medium (default) | 100,352 | 50,176 |
  | Large | 200,704 | 100,352 |
  | Dev (not in the menu) | 16,384 | 8,192 |

  All are multiples of 512 m (the largest `REGION_BLOCKS`; so also of the 8 m tree lattice) and of 128 m (the
  coarsest forge cell), so every size is a whole number of regions and of coarsest cells with no snapping. Tree ids
  (`tree_id`, period `ID_CELL_SPAN` = 4,096 m) are NOT periodic in the width — see Task 13.
- `EdgeKind { Sea, Land, Wrap }`; edges are indexed N, E, S, W. A Planet has `[Sea, Wrap, Sea, Wrap]`.
- **Canonical region**: x folded into `[origin.x, origin.x + width)`. **Canonical block position**: a
  `WorldBlockPos` whose x is folded into `[origin.x · REGION_BLOCKS, (origin.x + width) · REGION_BLOCKS)`.
  Likewise a canonical `WorldChunkPos`. **Every key and every position that is stored, hashed or sent is
  canonical; every conversion into one region's local frame goes through ONE nearest-image helper** (the plain
  difference with x folded into `(−W/2, W/2]` regions, ties → positive exactly as `MapExtent::delta` — it accepts
  canonical and in-frame unwrapped inputs alike).
  Unwrapped coordinates exist only transiently: `f64` world positions inside one region's own frame
  (`WorldView`), LOD keys (the client's frame, answered under the same key), and the client's render frame and
  window math.

## Review Focus

1. **A body seen from both sides of the seam at once** — a region in the map's last column ghosting into the
   first: the ghost's `source_region` must be canonical on server and client, or halo de-duplication and
   prediction matching miss (Tasks 6, 7).
2. **Unwrapped coordinates reaching the server or another region** — a click past the seam, an intent on a block
   one past the east edge, a pile's halo target: each must arrive canonical; the manager canonicalises inbound
   client packets anyway (Tasks 6, 7, 14).
3. **Rivers and lakes that straddle x = 0** — a reach carves continuously and a lake is one body (Tasks 10, 12).
4. **A width that is not a whole number of store tiles** — the last tile column is partial and its east
   neighbour is tile column 0 (Task 12).
5. **Features keyed on global block x** — the trees' lattice hashes and their 3×3 neighbour competition: both
   regions at the seam must agree on which candidate wins (Task 13).
6. **An old world on disk** — refused with a "re-forge this world" message in the load list and at server
   start, never a panic or a silent forge over it (Task 2, built; Task 17 for pre-Planet layouts).

---

## Built (record only — do not dispatch)

### Task 1: Rectangular forge grid

**Files:**
- Modify: `crates/worldgen/src/forge/world.rs` (`ForgeParams`, `World`, `World::new`, `new_continental`, `follow_land_edges`, `on_border`, `upsample`, `upsample_onto`, `neighbors`, `center`, `ContinentalShape`, `volcano_sites`)
- Modify: every other file in `crates/worldgen/src/forge/` (`mod.rs`, `plates.rs`, `geology.rs`, `climate.rs`, `erosion.rs`, `routing_fast.rs`, `post.rs`, `hydrology.rs`, `fields.rs`, `regions.rs`, `spawn.rs`, `quality.rs`, `render.rs`, `play.rs`) and `crates/worldgen/src/habitat.rs` (`coast_distance_by`)
- Modify: callers that build `ForgeParams` (`grep -rn "ForgeParams {\|size_m" crates`): `crates/world-store/src/ctl.rs`, `crates/worldgen/examples/forge_proto/{main,search,serve}.rs`, `crates/client/src/frontend/generator.rs`, `crates/server/src/world_file.rs` tests, all crate tests
- Test: `crates/worldgen/src/forge/world.rs` tests, `crates/worldgen/tests/forge_lib.rs`

**Interfaces:**
- Produces:
  - `pub struct ForgeParams { pub width_m: f64, pub height_m: f64, pub cell_m: f64, pub seed: u64, pub knobs: Knobs, pub quiet: bool }`; `impl ForgeParams { pub fn square(size_m: f64, cell_m: f64) -> Self /* default seed, knobs, quiet */; pub fn nx(&self) -> usize; pub fn nz(&self) -> usize }`. `Default` stays 16,000 × 16,000 m at 32 m (changed in Task 15).
  - `pub struct World { pub nx: usize, pub nz: usize, pub dx: f64, … }` (no `n`); `pub fn cells(&self) -> usize`; `pub fn xz(&self, i: usize) -> (usize, usize)`; `pub fn index(&self, x: usize, z: usize) -> usize`.
  - Every forge function that took `n` takes `nx, nz` (or the `World`); every function that took `size_m` takes `width_m, height_m`. `fields::volcanoes` takes the grid size as arguments instead of `sqrt(len)`.
- Rule for one-`half` normalisations (`World::new` island body, `ContinentalShape::at`, `to_land_edge`, `edge_proximity`, `plates`, geology's `half`): `rx = (mx − width/2) / (width/2)`, `rz = (mz − height/2) / (height/2)`. When `width == height` this is today's value bit for bit — keep the SAME floating-point expression order as today for that case (the pins check it).

- [ ] **Step 1: Write the failing tests** in `world.rs`:
  - `rectangular_world_has_its_own_dimensions`: `World::new(&ForgeParams { width_m: 8192.0, height_m: 4096.0, cell_m: 64.0, ..ForgeParams::default() })` has `nx == 128`, `nz == 64`, `h.len() == 128 * 64`; `xz(index(100, 50)) == (100, 50)`; `center(index(127, 0)).0 > 8000.0`.
  - `neighbors_respect_both_bounds`: on that world, `neighbors(index(127, 63))` yields exactly the three in-grid cells.
  - `rectangular_forge_runs` (slow, `forge_lib.rs`): a default-knob forge at 8192 × 4096 m / 64 m finishes; the artifact's rasters all have `nx * nz` entries; some cell is ocean and some is land.
- [ ] **Step 2: Run** `just test-one worldgen "--lib forge::world::"` → compile failure (no `width_m`).
- [ ] **Step 3: Implement** the refactor. `WorldArtifact` still has one `size_m` / `n` after this task: `Forged::artifact` asserts `nx == nz` and writes them (Task 2 lifts that). `rectangular_forge_runs` therefore inspects `forged.world` (`h.len() == nx * nz`) and does not call `artifact()`.
- [ ] **Step 4: Run** `just test-one worldgen "--lib forge::"`, `just test-one worldgen "--test it -- --include-ignored forge_lib::"` → PASS, including `island_layout_unchanged` and `forge_quality_matches_baseline` UNCHANGED. `just smoke` → PASS.
- [ ] **Step 5: Commit** `refactor(forge): width × height grid (square worlds forge bit-identically)`.

### Task 2: Rectangular artifact, tiles and sampler; knobs in the artifact; the one format bump

**Files:**
- Modify: `crates/worldgen/src/forged.rs` (`ARTIFACT_VERSION`, `WorldArtifact`, `validate`, `delta_rows` callers, `WorldGlobals`, `WholeRasters::new`, `ForgedWorld`: `clamped`, `q_cell`, `bilinear_cells`, `nearest_cell`, `prefetch_rect`, `surface_rock`, `place`, `extended_cell_height`, `ground`, `artifact_bounds_engine_m`, `with_regions` ~404, `with_rasters` ~1002)
- Modify: `crates/worldgen/src/forged_tiles.rs` (`TILED_FORMAT_VERSION`, `TileGeometry`, `route_cell`, `split`, `ForgedMeta::validate`)
- Modify: `crates/worldgen/src/forge/mod.rs` (`Forged::artifact`), `forge/world.rs` (`EdgeKind::Wrap` appended, unused until Task 9), `crates/worldgen/src/world_store.rs` (`WorldMeta`, `META_FORMAT`, `check_terrain`), `crates/worldgen/src/tile_map.rs` (`WORLD_MAP_VERSION`, `summarize_tile_with`)
- Modify: every `WorldArtifact { .. }` literal (`forge/mod.rs`, `world_store.rs`, `habitat.rs`, `tests/common/mod.rs`, `tests/underground.rs`) and `WorldMeta { .. }` literal (`world_store.rs` test, `client/src/frontend/generator.rs`, `client/src/frontend/load_world.rs`)
- Test: `crates/worldgen/tests/forged_world.rs`, `crates/worldgen/tests/forged_tiles.rs`, `crates/world-store/tests/forged.rs`

**Interfaces:**
- Consumes: Task 1.
- Produces:
  - `WorldArtifact { version, seed, width_m: f64, height_m: f64, cell_m: f64, nx: u32, nz: u32, …rasters (each nx*nz)…, knobs: BTreeMap<String, f64> }` (no `size_m`, no `n`). `WorldGlobals` gains the same five fields and `knobs`; `validate` requires `nx * cell_m == width_m && nz * cell_m == height_m`.
  - `impl WorldGlobals { pub fn knobs(&self) -> Knobs /* defaults overlaid with the stored map; unknown names ignored */ }`.
  - `TileGeometry { nx, nz, tile_cells, tiles_x, tiles_z, buckets_x, buckets_z, buckets_per_tile }`.
  - `WorldMeta { …, width_m: f64, height_m: f64, … }` (no `size_m`); `pub fn size_text(width_m: f64, height_m: f64) -> String` e.g. `"100 × 50 km"` (moved from `load_world.rs`).
  - `ForgedWorld::artifact_bounds_engine_m()` returns a rectangle; `lapse_c_per_m` and good/evil shares come from `globals.knobs()`.
  - `pub enum EdgeKind { Sea, Land, Wrap }`.

- [ ] **Step 1: Write the failing tests:**
  - `forged_world::rectangular_artifact_round_trips`: forge 8192 × 4096 / 64 m; `WorldArtifact::from_bytes(a.to_bytes())` equals `a`; `a.nx == 128 && a.nz == 64`; `validate()` is `Ok`.
  - `forged_world::sampler_reads_the_east_half`: on that world, `ground` at artifact x = 6000 m, z = 2000 m equals the height raster's bilinear value there (it is not clamped to x = 4096).
  - `forged_world::stored_lapse_is_used`: forge with `lapse_c_per_km = 9.0`; the loaded `ForgedWorld`'s temperature at a 1,000 m summit is 9 °C (± raster step) below its sea-level value at the same latitude row, not 6.5.
  - `forged_tiles::rectangular_split_covers_every_cell`: `split` of the 128 × 64 world at `tile_cells = 48` yields `3 × 2` tiles whose `w * h` sum to `128 * 64`.
  - `forged_world::old_artifact_is_refused_with_reforge_message`: bytes with magic `VXWF` and version 7 → `ArtifactError` whose text contains `re-forge`; `world_store::list` on a directory whose `world.json` has `format_version: 1` marks the entry unusable with the same wording, and does not panic.
- [ ] **Step 2: Run** `just test-one worldgen "--test it forged_world::rectangular"` → compile failure.
- [ ] **Step 3: Implement.** Bump the four version constants. `Forged::artifact` stores `world_store::knobs_map(&p.knobs)`.
- [ ] **Step 4: Run** `just test-one worldgen "--test it -- --include-ignored forged_world:: forged_tiles:: tile_map::"`, `just test-one world-store "--test it -- --include-ignored forged::"`, `just test-one server "--test it store_world::"`, the worldgen lib suite, `just smoke`, `just determinism-wasm` → PASS.
- [ ] **Step 5: Commit** `feat(worldgen)!: width × height world file with its knobs (artifact v8, tiles v2)`. Note in the message that every existing world must be re-forged.


---

## To build

### Task 3: World sizes, command lines, the `--report` mode, the layout crcs

**Files:**
- Modify: `crates/worldgen/src/forge/play.rs` (`WORLD_SIZES`, `DEFAULT_WORLD_SIZE`, `forge_estimate`, `preview_cell_for`, `offered_cells`, `check_play_ram`)
- Modify: `crates/world-store/src/ctl.rs` (`USAGE`, `parse_forge`), `crates/worldgen/examples/forge_proto/{main,search,serve}.rs` (`Params::parse`; `serve.rs` writes `height_m` beside `width_m`)
- Create: `crates/worldgen/examples/forge_proto/report.rs` (the `--report` mode)
- Modify: `crates/client/src/frontend/generator.rs` (`size_m()` → dimensions, `params`, the size row, the estimate line, `check_play_ram` call, the `WorldMeta` it builds), `crates/client/src/frontend/load_world.rs` only if a signature it uses changes
- Modify: `crates/worldgen/src/forged.rs` (`knobs_from_map`), `crates/worldgen/src/world_store.rs` (`size_text`) — the Task 2 minors below
- Modify: `crates/panel/src/forge.rs` (`ForgeRequest::args` sends `--size <number>`, which this task refuses: send `--width` / `--height` or a size name; its form in `web.rs` / templates follows)
- Modify: `justfile` (`forge-bench`, new `forge-report`, the comments on `forge-proto`, `forge-db`, `worldctl`)

**Interfaces — produces:**
- `pub struct WorldSize { pub name: &'static str, pub width_m: f64, pub height_m: f64 }`; `pub const WORLD_SIZES: [WorldSize; 4]` (Tiny, Small, Medium, Large — the table above); `pub const DEFAULT_WORLD_SIZE: usize = 2`; `pub const DEV_WORLD_SIZE: WorldSize` (16,384 × 8,192); `pub fn world_size_named(name: &str) -> Option<WorldSize>` (case-insensitive, includes `"dev"`).
- `forge_estimate(width_m, height_m, cell_m)` (`cells = nx * nz`); `preview_cell_for(width_m, height_m)`; `offered_cells(width_m, height_m)`; `check_play_ram(width_m, height_m, cell_m, physical)` — message `"{W} × {H} km at {cell} m needs …"`.
- CLI, in both `worldctl forge` and `forge_proto`: `--size <tiny|small|medium|large|dev>`, or `--width <m> --height <m>`; a bare number after `--size` is refused with a message naming the two forms.
- `forge-bench width="16384" height="8192" cell="32":` passing `--width`/`--height`; `forge-report *args:` = `forge_proto --report {{args}}` (release, through `cap`).
- **`forge_proto --report [--seeds N]`**: forges (no PNGs, no world file unless `--out` is given) and prints one `key: value` line per number, for each of N seeds (`seed`, `seed + 1`, …; default 1) and then a `summary` block (min / median / max per key). Lines in this task: `size` (`W × H m at C m, nx × nz`), `layout`, `edges`, `height_crc` (crc32fast of `forged.world.h` as little-endian `f64` bytes, 8 hex digits), `land_share`, `land_components_over_1pct`, `land_components_over_3pct`, `lakes`, `river_mouths`, `max_peak_m`, and the biome shares of land the example already prints. Later tasks and phases APPEND lines; a line's key never changes meaning. `report.rs` exposes one `pub fn report(forged: &Forged, p: &ForgeParams) -> Vec<(String, String)>` so later tasks add rows in one place.
- Task 2 minors folded in: `knobs_from_map` skips a stored value that is not finite and clamps the rest to the knob's `Spec` range; `size_text` shows a side under 1 km with one decimal (`"16 × 0.4 km"`); the generator's `WorldMeta` takes `width_m` / `height_m` from the forged artifact (`nx · cell_m`), not from the request. The generator persists its size row by INDEX (`WORLD_SIZE_KEY`): rename the key so a stored index of the old table is not read as a new size.

- [ ] **Step 0: Record the layout crcs BEFORE any change** (temporary test in `crates/worldgen/tests/forge_lib.rs`): for `layout` in 0..=5, `ForgeParams::square(S, 64.0)` with `knobs.layout = layout`, default seed, print `crc32fast::hash` of `forged.world.h` as LE bytes. `S` = the smallest of 4096, 8192, 16384 at which all six forge; record `S` and the six crcs in the report under the heading "Layout crcs" (the controller copies them into the ledger, where every later task reads them).
- [ ] **Step 1: Temporary tests** — `play`: every `WORLD_SIZES` entry and `DEV_WORLD_SIZE` has `width_m == 2.0 * height_m`, both multiples of 512.0 and 128.0; `forge_estimate(100_352.0, 50_176.0, 16.0).secs == forge_estimate(200_704.0, 100_352.0, 32.0).secs`. `ctl`: `parse_forge(["--db", url, "--size", "small"])` gives 50,176 × 25,088; `["--width", "8192", "--height", "4096", "--cell", "64"]` gives those; `["--size", "16000"]` is `Err` containing `--width`. `generator` (client lib): the default state's params are 100,352 × 50,176; selecting row 0 gives 25,600 × 12,800. `forged`: a stored knob map with `lapse_c_per_km = NaN` loads the default; one far out of range is clamped.
- [ ] **Step 2: Implement.** The generator's size row lists `"{name} · {W} × {H} km · {estimate}"`. No size is hidden for being slow.
- [ ] **Step 3: Evidence** — `just forge-report --width S --height S --cell 64 --set layout=L` for L = 0..5: the six `height_crc` values equal Step 0's. `just forge-report --size dev --cell 64` prints every line (the dev size forges with today's default layout). Quote both.
- [ ] **Step 4: Verify** — the safety net (gate + crcs), `just smoke`. Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): four 2:1 world sizes; --size names, --width/--height; forge_proto --report`.

### Task 4: World map of a rectangle — what Task 2 left square

**Files:**
- Modify (only where still square): `crates/worldgen/examples/world_map.rs` (the `--layer` / `write_forged_layer` path draws `n × n`: make it `nx × nz`; `--tiles` is already rectangular), `crates/client/src/renderer/slint_overlay.rs` (`map_tile`, its reverse, and the map window's image/aspect), `crates/client/src/renderer/commander/minimap.rs`, `crates/client/src/renderer/arrival_tabs.rs`, the Slint world-map window sources under `crates/slint-ui/ui/` only if they force a square
- No change expected in `crates/worldgen/src/tile_map.rs` (`build_world_map` is rectangular since Task 2) — verify, do not rewrite.

- [ ] **Step 1: Temporary test** (worldgen `tests/forge_lib.rs`): forge 8192 × 4096 / 64 m (default layout), `build_world_map`: `map.width as f64 * tile_m >= 8192.0`, `map.height as f64 * tile_m >= 4096.0`, `map.width >= 2 * map.height - 1`, and a tile with x index ≥ `width / 2` has a non-default summary.
- [ ] **Step 2: Implement** what the audit finds; list in the report every site checked and whether it was square.
- [ ] **Step 3: Evidence** — `just forge-proto --width 8192 --height 4096 --cell 64 --out target/world-forge/rect` then `just world-map --world target/world-forge/rect.world --tiles --out target/world-forge/rect-map.png`: the PNG is 2:1 (quote its pixel size, `file` output). The same with `--layer height`.
- [ ] **Step 4: Verify** — `just smoke`. Delete the temporary test.
- [ ] **Step 5: Commit** `feat(worldgen): the world-map tools and windows follow width and height` (or report "nothing to change" with the audit list and no commit).

### Task 5: `MapExtent` wrap API

**Files:**
- Modify: `crates/game/src/world_map.rs` (`WorldMap`, `MapExtent`, `off_map`), `crates/game/src/path_plan.rs` (`RouteMap` carries the extent, so it carries `wrap_x`)
- Modify: `crates/game/src/history/grid.rs` (`SocietyGrid::from_world_map`: `world_id` is a crc of the serialized map and is in the history digest that `wasm_determinism` pins — hash the old fields exactly as before and mix in `wrap_x` only when it is true, so the pin does not move; ruled 2026-11-19)
- Modify: every `WorldMap { .. }` literal — `crates/worldgen/src/tile_map.rs`, `crates/server/tests/common/hosts_rig.rs`, `crates/game/tests/wasm_determinism.rs` (a GATE file: add the field `wrap_x: false` only; its pinned hashes must not change — if one does, BLOCKED), `crates/game/tests/history_handover.rs` — and any `MapExtent { .. }` literal (`grep -rn "MapExtent {" crates`)
- Modify: `crates/worldgen/src/tile_map.rs` (`WORLD_MAP_VERSION` 2 → 3; `build_world_map` sets `wrap_x: false` until Task 12), `crates/world-store/tests/fixtures/world_map.bin` is referenced by nothing: do not touch it (its uncommitted change is the owner's). No link-protocol bump: `WorldMap` travels only in `ServerPacket::WorldMap` (client and server are one build)

**Interfaces — produces** (in `world_map.rs`, re-exported at the crate root like `off_map`):
- `pub struct MapExtent { pub origin: RegionCoords, pub width: u32, pub height: u32, pub wrap_x: bool }`
- `impl MapExtent { pub fn canonical(&self, rc: RegionCoords) -> RegionCoords; pub fn contains(&self, rc: RegionCoords) -> bool /* wrapped: z range only */; pub fn neighbour(&self, rc: RegionCoords, dx: i32, dz: i32) -> Option<RegionCoords> /* canonical; None off-map */; pub fn delta(&self, from: RegionCoords, to: RegionCoords) -> (i32, i32) /* x shortest signed when wrapped, ties → positive */; pub fn canonical_block(&self, pos: WorldBlockPos) -> WorldBlockPos /* x folded by width · REGION_BLOCKS when wrapped */ }`
- Free functions over `Option<MapExtent>` (no map = unbounded plane): `canonical(map, rc)`, `region_neighbour(map, rc, dx, dz) -> Option<RegionCoords>`, `region_delta(map, from, to) -> (i32, i32)`, `canonical_block(map, pos)`.
- `WorldMap { …, pub wrap_x: bool }`; `extent()` copies it; `tile(rc)` canonicalises first.
- `wrap_x: false` and "no map" stay supported for good: procedural test worlds and hand-built maps have no wrap.

- [ ] **Step 1: Temporary tests** (a temp module in `crates/game/tests/it.rs`) with `m = MapExtent { origin: (-3, -2), width: 10, height: 4, wrap_x: true }`: `canonical((7, 0)) == (-3, 0)`, `canonical((-4, 1)) == (6, 1)`, `canonical((2, 9)) == (2, 9)`; `contains((100, 0))`, `!contains((0, 2))`, `!contains((0, -3))`; `neighbour((6, 0), 1, 0) == Some((-3, 0))`, `neighbour((-3, 0), -1, 0) == Some((6, 0))`, `neighbour((0, 1), 0, 1) == None`; `delta((6, 0), (-3, 0)) == (1, 0)`, `delta((-3, 0), (6, 0)) == (-1, 0)`, `delta((-3, 0), (2, 0)) == (5, 0)`; `canonical_block` of a block one past the east edge is the first block of column `origin.x`; with `wrap_x: false` the same calls give plain subtraction / `None` past x; `region_neighbour(None, (5, 5), 1, 0) == Some((6, 5))`; a 10 × 4 wrapped `WorldMap` returns the same tile for `(7, 0)` and `(-3, 0)`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify** — `just test-one game "--test wasm_determinism"` only if that target runs natively in seconds (else leave it to `just determinism-wasm`), `just determinism-wasm`, `just smoke`. Delete the temporary tests.
- [ ] **Step 4: Commit** `feat(game): MapExtent east–west wrap helpers (world map v3)`.

### Task 6: Sim crossings, routes and inbound canonicalisation

**Files:**
- Modify: `crates/game/src/protocol.rs` (`rebase_isometry`, `EntityBundle::rebase` — it also shifts `voxel_body.origin` by `(from − to) · REGION_BLOCKS`: that must use the same delta — and `HaloEntityMsg::rebase`)
- Modify: `crates/game/src/state.rs` (`scan_boundaries`: the `closed` closure and leaver / ghost targets via `region_neighbour`; its `off_map` use)
- Modify: `crates/game/src/world_manager/mod.rs`: the departure relay (`RegionOutput::Departures`: `bundle.rebase` / `msg.rebase` by `region_delta(self.map_extent(), rc, target)`, targets canonical); inbound client packets canonicalised before use — `RequestRegionConnection`, `ReleaseRegionConnection`, `PlayRegion`, an internal event's `region_id` before its home check, `Goal.region` inside `Order::MoveTo`, the `WorldBlockPos` of `Order::DevBreakBlock` (`canonical_block`); `play_region` and `landing_in` take canonical; its five `off_map` uses; the four call sites of `halo::neighbours(rc)` canonicalise each result (and drop off-map ones) — `halo::neighbours` itself is NOT changed
- Modify: `crates/game/src/world_manager/placement.rs` (its `off_map` use)
- Modify: `crates/game/src/path_plan.rs` (`next_region`, `route_step`, `line_step`, `on_map`: neighbours via `MapExtent::neighbour`, directions via `delta`), `crates/game/src/adventure.rs` (`plan_walker`), `crates/game/src/people.rs` (`SpawnAt::Edge`, `walk_out_safe`)
- Do NOT modify: anything on the host-to-host path (Global Constraints). Halo targets inside regions, intents, piles, containers and the world view are Task 7.

**Interfaces — produces:** `pub fn rebase_isometry_by(iso: &Pose, delta: (i32, i32)) -> Pose` (translation −= delta · `REGION_SIZE`). `rebase_isometry(iso, from, to)` stays for callers with no map, defined through it; every caller holding a map uses `region_delta(map, from, to)`. `EntityBundle::rebase_by(delta)` / `HaloEntityMsg::rebase_by(delta)` likewise.

- [ ] **Step 1: Temporary tests** (a temp module in `crates/game/tests/it.rs`, on `tests/common/rig.rs` — `Rig::with_spawner` has no "put a body at x" helper and `seam_translate.rs` holds only `uniform_floor`, so script input and orders by hand; copy `rollback_restore.rs`'s private `state_hash` helper into the temp module: a manager with a procedural generator and a hand-built wrapped `WorldMap` of width 4, height 2, origin (0, 0) installed by `set_world_map`). Canonicalising the four `halo::neighbours` results makes `halo::neighbour_offset` return `None` across the seam until Task 7: do not expect seeded halo TERRAIN at the seam here, only the halo entity:
  - a client in region (3, 0) whose body is at `x = REGION_SIZE − 0.5` moving +x: after the crossing its home is (0, 0) and its region-local x is in `[0, 4)`; the mirror, (0, 0) → (3, 0);
  - a body pushing −z in row 0 stays in its region (the north edge still holds);
  - a body near (3, 0)'s east edge produces a halo entity in (0, 0) with `source_region == (3, 0)`;
  - `Order::MoveTo(Goal { region: (0, 0), .. })` from region (3, 0) leaves by the EAST edge;
  - `ClientPacket::PlayRegion((4, 0))` moves the client to (0, 0); `RequestRegionConnection((-1, 0))` subscribes (3, 0);
  - the same east-edge push on the same map with `wrap_x: false` is held at the edge;
  - `hash(before) == hash(after undo)` across a seam crossing (the rollback bar) — reuse `rollback_restore`'s helper.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify** — `just test-one game "--test it -- region_runner:: rollback_restore:: handoff_state:: peer_schedule::"` (the fast ones; add `--include-ignored` for `handoff_state::` and `peer_schedule::` and say how long they took), `just smoke`, `just determinism-wasm`. Delete the temporary tests.
- [ ] **Step 4: Commit** `feat(game): region crossings and routes wrap across the east–west seam`.

### Task 7: Halo, intents, piles, containers and the world view across the seam

The old plan did not know this code. Regions address their neighbours by plain offsets and blocks by global
`WorldBlockPos { x: i64, .. }`; at the seam both are off by the map's width.

**Files (start here; the audit in Step 1 is the real list):**
- `crates/game/src/halo.rs`: senders (`halo_targets`, `halo_updates_for` — it emits `RegionCoords::new(from.x + dx, from.z + dz)` — `halo_seed*`, `neighbour_offset`, `facing_columns`, `facing_windows`: all plain `to − from`); receivers (`halo_cell`, `halo_chunk_at`, `halo_mineable`, `halo_column_known`, `halo_ground_of` build keys with `WorldChunkPos::from_local(self.data.region, local)` — unwrapped past the east edge — while `apply_halo_delta_safe`, `apply_halo_update_safe`, `materialize_behind_halo_safe` convert with `WorldChunkPos::local_in`)
- `crates/game/src/region_runner.rs` (`seed_halo_from_store`, `seed_halo_from_worldgen`: `halo_seed_from_snapshot(n, self.id)` and `facing_windows(n, self.id)` are `None` at the seam today — a silently missing halo)
- `crates/game/src/item_piles.rs` (`pile_halo_targets`, `pile_seed_for` — `offset = to − rc` —, `local_cell`), `crates/game/src/containers.rs` (`container_seed_for`), `crates/game/src/intents.rs` (`Target::region()`, `apply_intent_safe`, `IntentReply::carried_items`' cell), `crates/game/src/world_view.rs`, `crates/game/src/water.rs` (`water_units_at`, `recipe_ground_at`), `crates/game/src/slot_order.rs` (`cell.to_local().0`), `crates/game/src/protocol.rs` (`WorldBlockPos::from_local` / `to_local`, `WorldChunkPos::from_local` / `local_in` / `to_local`, `RegionCoords::from_world`), `crates/game/src/mining.rs`, `edit.rs` where they form a position outside their own region
- From Task 6's report ("For Task 7"): `Walker::path` positions (hashed state, built by `WorldBlockPos::from_local(here, …)`, unwrapped past the seam: make them canonical and read them through the nearest-image conversion in `local_tile`, `Walker::waypoint`, `continue_walk_safe`'s `near`, so a walker walks on after the crossing instead of re-planning); `Goal::local_column` / `local_tile` (plain differences); `PathPlan::plan`'s region-level `nearest_image` (fold it into the one helper); the reach check and `Target::region()` routing of `Order::DevBreakBlock` and intents; `RegionOutput::HaloDeltas` targets; the four manager reseeds and `seed_halo`, which get canonical neighbours since Task 6 and seed nothing across the seam until `halo_seed_from_snapshot` / `neighbour_offset` wrap
- `crates/game/src/world_manager/mod.rs`: where `RegionOutput` halo updates, intents, refusals and replies are routed — the choke point that canonicalises a target region and a position before delivery
- From Task 6's review: `MapExtent::wraps()` (`world_map.rs`) must be false for a map narrower than 3 regions (at width 2 `delta` is +1 both ways, so a hand-back is not the inverse of its departure); `WorldManager::landing_in` debug-asserts a canonical argument — make it fold instead (its callers pass `SPAWN_REGION`, history sites and handed-off targets); the `MoveTo` east-edge walk, shown in Task 6 only on the pure planner, is re-run here end to end through the order once the halo is seeded.
- Do NOT modify the host-to-host path; `world_manager/peers.rs` forwards what it is given.

**The rule to implement** (two helpers, not a per-site patch):
1. **Canonical keys.** Every key of the hashed halo state (`GameData::halo`, `halo_ground`, `halo_piles`, `halo_containers` — also sent in region snapshots and to the client) and every `WorldBlockPos` / `WorldChunkPos` / `RegionCoords` that is stored, hashed or sent is canonical: `canonical_block`, a new `canonical_chunk`, `canonical`. Every `from_local` that builds such a key or a sent position canonicalises.
2. **One nearest-image local conversion.** Every conversion of a block, chunk or region into a region's local frame (`local_in`, `local_cell`, `halo_block`'s difference, `neighbour_offset`'s difference, the seed offsets) goes through one helper: the plain difference with x folded into `(−W·N/2, W·N/2]` (`N` = blocks or chunks per region; ties → positive, built on `MapExtent::delta`'s rule). It accepts canonical and in-frame unwrapped inputs, so `WorldView` callers that form positions from a body's world translation keep working (those `f64` positions are transient and may stay unwrapped). It needs a map at least 3 regions wide (the test map is 4; `MIN_WRAP_WIDTH_M` guarantees 16 at the largest region size).
3. **The map reaches the sim.** `Region` holds `map_extent`, but halo, pile and intent code runs on the rollback data (`self.data.region` only) and the free functions in `halo.rs` take no map. Put the extent beside `region` in the data — set at construction / `set_map_extent`, and treated by the state hash, snapshots and undo exactly as `region` is — or pass it as an argument; say which in the report and why. Server and client regions must hold the same extent before their first tick.
4. The manager canonicalises every target it routes as a second line of defence and counts what it had to fix (a debug assertion in tests; a counter `seam_fixed=` on the periodic `[server]` line — never a log line per message).

With no map, or `wrap_x: false`, both helpers are the identity / the plain difference: bit-identical to today (`physics_goldens::`, `wasm_determinism` and `rollback_restore::` must not move).

- [ ] **Step 1: Audit.** `grep -n "\\.x [+-] \\|x + dx\\|from\\.x\\|RegionCoords::new(" crates/game/src --include=*.rs` and every use of `WorldBlockPos::from_local`, `to_local`, `WorldChunkPos::from_local`, `local_in`, `local_cell(`, `Target::region`, `halo_targets`, `pile_halo_targets`, `neighbour_offset`, `facing_`: list each site in the report as wrap-relevant (changed) / region-local (unchanged, why) / host path (not touched).
- [ ] **Step 2: Temporary tests** (same rig and wrapped 4 × 2 map as Task 6):
  - an item pile dropped in the last block column of region (3, 0) appears in (0, 0)'s halo, and is dropped from it when the pile is removed; the same for a container;
  - a mining / edit intent from a client in (3, 0) on a block one past the east edge (`Order::DevBreakBlock` is the simplest driver) changes the block at local x = 0 of (0, 0), and its reply reaches the sender; the mirror westward;
  - region (3, 0)'s halo of (0, 0) is seeded at start (terrain and ground) and its keys are canonical;
  - `WorldView` in (3, 0) reads the block at local x = `REGION_BLOCKS` (the first column of (0, 0)) and gets (0, 0)'s block;
  - a body standing on the seam collides with terrain of both regions (no fall through the last column);
  - with `wrap_x: false` each of the above behaves as at today's outer edge;
  - the rollback bar across an intent that crossed the seam.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Verify** — as Task 6's Step 3. Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(game): halo, intents and block positions wrap across the east–west seam`.

### Task 8: Server LOD across the seam

**Files:**
- Modify: `crates/game/src/world_manager/mod.rs` (`start_lod_build`: source region = `canonical(map, key.origin_region())`; `start_tile_build`: terrain from `canonical(r)`, positioned by the unwrapped `r` of `key.regions()`; `invalidate_region_lod`: every subscribed key with any `key.regions()` canonicalising to `rc`)
- Modify: `crates/game/src/lod_builder.rs` (`LodBuildJob::Tile { stored: Vec<RegionCoords> }`: the builder reads the store by that coordinate AND overlays at it — carry `(canonical, unwrapped)` pairs: read by the first, position by the second; `Pristine` / `Stored { region }` take the canonical region), `crates/server/src/lod_workers.rs` if it names the field

**Interfaces:** LOD keys stay UNWRAPPED on the wire: a tile key spans several regions and the map width need not divide by the tile span, so the client sends keys in its own frame and the server answers with the same key (`ServerPacket::LodData(LodKey, LodPayload, LodWater)` — all three for the same key). `copy_region_cells_into_tile` is unchanged — called with the unwrapped `r`. The `coarse(&key)` and `water(&key)` closures of `build_lod_job` receive the UNWRAPPED key (`worldgen::coarse_window` samples from `key.origin_region()`): they are correct past the seam once the forged sampler folds x (Task 12) and the tree windows fold (Task 13) — nothing to change for them here.

- [ ] **Step 1: Temporary tests** (in `crates/game/tests/lod_streaming.rs`'s style, deleted after): on a width-4 wrapped map, `RequestLod(LodKey::for_region(2, (4, 0)))` is answered with `LodData` for that same key whose cells and water equal those of `LodKey::for_region(2, (0, 0))`; a block edit in (0, 0) re-sends `LodData` for a subscribed key over (4, 0). Factor 2 takes the sub-block path (`COARSE_MIN_FACTOR` is 16) and the rig's `coarse` / `water` closures are empty: add a factor ≥ 16 key with a test `coarse` closure that records the key it was given (the same unwrapped key) and a stored region read through its canonical coordinate.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify** — `just test-one game "--test it lod_streaming::"`, `just smoke`. Delete the temporary tests.
- [ ] **Step 4: Commit** `feat(game): LOD keys past the seam build from canonical regions`.

### Task 9: History on a cylinder

**Files:**
- Modify: `crates/game/src/history/grid.rs` (`SocietyGrid` gains `wrap_x` from `WorldMap` in `from_world_map`; `index` canonicalises; `label_components` 8-neighbours wrap; `route` neighbours wrap and its heuristic uses the wrapped |dx|)
- Modify: `crates/game/src/history/chain.rs` (the local `chebyshev`; `embark_target` — the history's own settler parties, it STILL EXISTS: its `from_region.x − r ..= + r` scan canonicalises and visits no region twice on a narrow map; `embark_allowed_in`), `history/bootstrap.rs` (local `chebyshev`, civ spacing), `history/handover.rs` (`entry_side`), `history/journey.rs` (`leg_arrivals`, `same_landmass`)
- The grid is not stored (`history/persist.rs` decodes with the grid supplied): no format change. If one is needed, BLOCKED.

**Interfaces — produces:** `impl SocietyGrid { pub fn distance(&self, a: RegionCoords, b: RegionCoords) -> i64 /* Chebyshev, wrapped in x */; pub fn delta(&self, from: RegionCoords, to: RegionCoords) -> (i32, i32) }`. The two local `chebyshev` functions are deleted in favour of `distance`.

- [ ] **Step 1: Temporary tests** (helpers of `crates/game/tests/history_handover.rs`: `land_map`; a wrapped `WorldMap` of width 12, land everywhere unless said): `route(grid, (11, 0), (1, 0), ..)` is 3 regions long counting both ends — (11, 0), (0, 0), (1, 0); land in columns x = 0 and x = 11 only → one component; a site at (11, 0) refuses `embark_allowed_in` at (1, 0) with `TooClose`; `embark_target` from (11, 0) can return a region in column 0 or 1 and never one outside the map; with `wrap_x: false`: no route across, two components, allowed.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify** — `just test-one game "--test it history_handover::"`, `just smoke`, `just determinism-wasm` (its `history_scene` gate runs `bootstrap` → `route`, `label_components` and the distance this task replaces: on a non-wrapping map it must not move). Delete the temporary tests.
- [ ] **Step 4: Commit** `feat(history): routes, landmasses and site spacing wrap east–west`.

### Task 10: The Planet layout — a grid that wraps

**Files:**
- Modify: `crates/worldgen/src/forge/knobs.rs` (`LAYOUT_PLANET = 6` appended to the constants and `CHOICES`; `LAYOUT_LAST`; `pub fn is_planet(&self) -> bool`; `is_continental()` stays `>= LAYOUT_CONTINENT`, so a Planet is continental; `pole_choice()` returns `POLE_BOTH` and the latitude span is the full 90°–0°–90° when `is_planet()`)
- Modify: `crates/worldgen/src/forge/world.rs` (`edges()`: a Planet is `[Sea, Wrap, Sea, Wrap]`; `World { …, pub wrap_x: bool }` — three `World { … }` literals: two in `world.rs`, one in `forge/mod.rs` (`let initial = World {`); `neighbors` wraps x; new `step`; `on_border`, `border_cells`, outlet setup, the "every border cell is an outlet" asserts and `follow_land_edges` cover only the non-wrapped edges; `upsample_onto`'s x axis samples circularly; `ContinentalMass`: a Planet uses the `Continents` formula with sea on N and S only)
- Modify: `forge/routing_fast.rs` (`Steps`, `route_fast`'s border assert: below), `forge/erosion.rs` (the diffusion stencil in `evolve` covers columns 0 and nx−1 with x ± 1 mod nx when wrapping), flood fills and neighbour walks via `World::step`: `forge/mod.rs` (`lake_bodies`), `forge/hydrology.rs` (`find_lakes`, `find_islands`), `forge/fields.rs` (`closed_lakes`, `ground_slope`, `nearest_water`), `forge/regions.rs` (`Flood::components`, `merge`), `habitat.rs` (`coast_distance_by` — gains a `wrap_x` argument; it also has a RUNTIME caller, the `ForgedWorld` load in `forged.rs`, which passes `globals.edges[1] == EdgeKind::Wrap`), `forge/post.rs`, `forge/spawn.rs` (edge clearance on N and S only), `forge/quality.rs`, `forge/render.rs`
- Modify: `forge/climate.rs` (`box_blur_rows` circular along x when wrapping; `orographic`'s per-row x sweep)
- Modify: `crates/worldgen/examples/forge_proto/report.rs` (new lines, below)

**Interfaces — produces:**
- `pub const LAYOUT_PLANET: u32 = 6`.
- `impl World { pub fn step(&self, i: usize, dx: i32, dz: i32) -> Option<usize> /* the one wrap-aware offset helper: x wraps when wrap_x, z never; None off the north or south edge, or off east/west when not wrapping */ }`.
- Routing: `Steps` uses flat index offsets that are only valid because the whole border is outlets. With `wrap_x`, columns 0 and nx−1 are ordinary cells: `Steps` carries `nx` and `wrap_x` and computes a neighbour as `(x ± 1 mod nx, z ± 1)`; the border assert becomes "every cell of rows 0 and nz−1 is an outlet" when wrapping. The shore check wraps its x range the same way. The row-parallel passes stay row-parallel.
- Chamfer distances (`nearest_water`, `coast_distance_by`) with wrap: repeat the forward + backward pass pair until no cell changes (at most two extra pairs).
- Climate sweep with wrap: there is no edge inflow. Each row's sweep runs one spin-up lap around the row starting from moisture 1.0, then the recorded lap continuing from where the first ended.
- With `wrap_x == false` every one of these keeps today's code path and today's floating-point expression order (the layout crcs prove it).
- `--report` gains: `polar_rows_all_sea` (yes/no and the count of land cells in rows 0 and nz−1), `sea_level_temp_c` (`north` / `middle` / `south` rows' mean), `drains_to_sea` (the share of land cells whose receiver chain ends at an ocean cell; a Planet must print 1.000), the summary block never treats `height_crc` or `seed` as numbers (Task 3's summary parses any all-digit crc as a number); the existing `land_components_over_*` lines join land ACROSS the seam on a wrapping world (Task 3's flood fill does not wrap: a continent cut by the seam would count twice), `seam_step_ratio_height` / `_precip` (99th percentile of `|v(nx−1, z) − v(0, z)|` over rows ÷ the 99th percentile of interior column-to-column `|Δv|`), `lakes_across_seam`.

- [ ] **Step 1: Temporary tests** (`#[cfg(test)]` modules added to and removed from the files): `edges(seed, &planet_knobs) == [Sea, Wrap, Sea, Wrap]` for 16 seeds; on a wrapped 8 × 8 `World`, `neighbors(0)` yields exactly {1, 7, 8, 9, 15}, unwrapped {1, 8, 9}; `step(7, 1, 0) == Some(0)`; `step(0, 0, -1) == None`; a hand-built wrapped 16 × 8 world sloping down toward x = 0 from both sides with its only outlets in row 0 — a cell at x = 15 has its receiver at x = 0; a hand-built wrapped world with one depression covering columns {nx−2, nx−1, 0, 1} → `find_lakes` returns one lake of that area; on an all-ocean wrapped grid, precipitation at x = 0 and x = nx−1 of every row differ by less than 1%.
- [ ] **Step 2: Implement.** Noise and geometry are NOT yet periodic (Task 11), so a Planet forged now has a visible terrain step at the seam; drainage and climate already work across it.
- [ ] **Step 3: Evidence** — `just forge-report --size dev --cell 64 --set layout=6 --seeds 8`: `polar_rows_all_sea: yes` for all 8; `sea_level_temp_c` north and south below −15, middle above 20; `drains_to_sea: 1.000`; `seam_step_ratio_precip` quoted (as built: 1.1–6.7, from sea meeting land at the non-periodic seam — judged after Task 11; the height ratio is likewise expected above 1.5 until then). The safety net (gate + six crcs).
- [ ] **Step 4: Verify** — `just smoke`. Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): the Planet layout — polar seas, a grid that wraps east–west`.

### Task 11: Periodic noise and geometry

**Files:**
- Create: `crates/worldgen/src/forge/periodic.rs`
- Modify: `forge/mod.rs` (`pub mod periodic;`), `forge/world.rs` (shape, belt, rough and taper noises; `ContinentalMass` centres; `ContinentalShape`), `forge/plates.rs` (`plates` sites, `Warp`, `convergent_boundaries`, `plate_field`, `interior_ranges`, `interior_strength`; `soften_along_land_edges` has no land edges on a Planet), `forge/fields.rs` (`VolcanismNoise`, `volcanism_grid`, `volcano_sites_on` spacing, `cone`), `forge/geology.rs` (no x dip and x-periodic folds when wrapping; z dip only), `forge/regions.rs` (`savagery`), `forge/climate.rs` (`meander_deg`), `forge/post.rs` (lake-site spacing in `carve_lake_basins`)

**Interfaces — produces**, in `periodic.rs`:
- `pub struct PeriodicX { pub period_m: f64, pub band_m: f64 }`
- `pub fn fold(&self, x: f64) -> f64` (into `[0, period)`), `pub fn dx(&self, a: f64, b: f64) -> f64` (shortest signed b − a)
- `pub fn sample(&self, x: f64, z: f64, f: impl Fn(f64, f64) -> f64) -> f64`
- `pub fn with_images<T: Clone>(&self, items: &[T], x_of: impl Fn(&T) -> f64, shift: impl Fn(&T, f64) -> T) -> Vec<T>` (each item plus its copies at x ± period)
- `pub fn for_world(p: &ForgeParams) -> Option<PeriodicX>` — `Some` for a Planet, `None` otherwise; with `None` every call site keeps today's code path exactly. `pub fn from_globals(g: &WorldGlobals) -> Option<PeriodicX>` (`Some` when `g.edges[1] == Wrap`) for the runtime (Task 12), which has no `ForgeParams`.
- `sample`, for x folded into `[0, period)`: outside the last `band_m` it is `f(x, z)`; inside, with `t` running 0 → 1 across the band and `θ = π/2 · smoothstep(t)`, it is `cos θ · f(x, z) + sin θ · f(x − period, z)`. `cos² + sin² = 1` keeps the noise's variance and the smoothstep keeps its slope continuous. `band_m = min(period / 4, 4 × the noise's longest wavelength)`, set per noise at its construction.
- Geometry: Voronoi sites, boundary and range segments, continent centres, volcano and lake sites get images at x ± period (`with_images`) before any nearest-site or segment-distance query; queries use folded x. `meander_deg`: each wave's x-frequency is rounded to a whole number of cycles per period (at least 1).
- `--report` gains `seam_step_ratio_biome` (share of differing neighbours across the seam ÷ the interior's).
- From Task 10 (built): a Planet's climate already wraps (a world rolled by half its width reproduces its climate within 3%), but `seam_step_ratio_precip` was 1.1–6.7 over 8 seeds because the non-periodic LAND puts sea against land at the seam — this task's periodic land is what must bring it down. The ratio of ONE column pair against the pooled interior is also a noisy statistic (ordinary interior column pairs of the same worlds score up to 2.7). So `--report` also gains, for height, precipitation and biome, `seam_step_rank_*`: the fraction of interior column pairs whose own statistic is at least the seam's (1.0 = the seam is the smoothest pair, 0.0 = the roughest). A seed PASSES a field when its ratio is ≤ 1.5 OR its rank is ≥ 0.05 (the seam is not among the roughest 5% of column pairs); print `seam_ok_*: yes/no` by that rule. Still to wrap here, as Task 10 left them: plates, continent centres, volcano spacing, the meander, the savagery noise.

- [ ] **Step 1: Temporary tests** (in `periodic.rs`, removed after): for an `OctaveNoise` `f`, `|sample(period − ε, z) − sample(ε, z)| < 1e-3` with ε = 1e-4 · period, over 64 z values; the variance of `sample` over the band is within 10% of `f`'s over the interior (10⁴ points); sites at x = 1 and x = 50, period 100 — the nearest to x = 99 is the x = 1 site, at distance 2.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-report --size dev --cell 64 --set layout=6 --seeds 8`: `seam_ok_height`, `seam_ok_precip`, `seam_ok_biome` all yes (quote the ratios and ranks); `land_share` between 0.15 and 0.6; `land_components_over_1pct` ≥ 2 (a miss on a seed is quoted, with the seed — tuning is Task 17's). `just forge-proto --set layout=6 --size dev --cell 64 --out target/world-forge/planet` and `just world-map --world target/world-forge/planet.world --tiles --out target/world-forge/planet-map.png`: record the path. The safety net (gate + six crcs).
- [ ] **Step 4: Verify** — `just smoke`. Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): periodic noise and geometry for the Planet`.

### Task 12: Runtime sampler, rivers, tiles and the world map wrap

**Files:**
- Modify: `crates/worldgen/src/forged.rs`: `to_artifact` folds x into `[0, width_m)` when `globals.edges[1] == Wrap`; `clamped`, `q_cell`, `bilinear_cells`, `bilinear_q`, `bicubic`, `geo_offset_m`, `nearest_cell`, `prefetch_rect`, `lake_level_at`, `surface_rock`, `extended_cell_height` wrap x and clamp z; `place` never reports east or west "past" for a wrapping world (the Task 2 minor: `Wrap` must stop behaving like `Land`); river segments are also bucketed shifted by ± `width_m` where the shifted copy overlaps the map; `patch` gathers segments and lakes for the window's folded x-intervals; the runtime noises (the `detail`, `dither`, `snow_patch` FIELDS of `ForgedWorld`) are sampled through `PeriodicX`
- Modify: `crates/worldgen/src/forged_tiles.rs` (`TileCache::tiles_of_cells` and `TileGeometry::route_cell`: a read past the last tile column continues in column 0; the last column may be partial)
- Modify: `crates/worldgen/src/forge/hydrology.rs` (`find_rivers`: consecutive reach points more than `width_m / 2` apart in x are unwrapped by ± `width_m`, so a reach stays continuous; a lake's row runs that cross the seam are split into two runs at x = 0 when written, so every run stays within a row — the artifact's byte layout does not change)
- Modify: `crates/worldgen/src/tile_map.rs` (`build_world_map`: a wrapping world has `width = round(width_m / tile_m)` exactly and `wrap_x: true`; slope and extended-height reads wrap; `reaches_by_tile` (river reaches → tiles: its `RegionCoords::new(guess.0 + dx, …)` and `in_footprint` fold x, since reach points may now lie outside `[0, width_m)` — else seam reaches vanish from the world map)), `crates/worldgen/src/lib.rs` (`generate_region_forged`, `coarse_tile_forged`, `coarse_window`: they FOLD — any region or key, canonical or not, yields the terrain of its canonical image; no assert: LOD keys arrive unwrapped by design)
- Modify: `crates/worldgen/src/forge/mod.rs` (the forge entry) and `crates/worldgen/src/forged.rs` (`WorldGlobals::validate`): a wrapping world narrower than `forge::play::MIN_WRAP_WIDTH_M` (8,192 m; the const exists since Task 14, else add it) is refused at forge time and at load, on every path, with `"this world is too narrow to walk around (… m; needs 8192 m)"`; likewise a wrapping world whose width is not a whole number of coarsest forge cells (`cell_m · 2^coarse_levels`) is refused at forge time (`upsample_onto`'s circular sampling would put the period mismatch on the seam — Task 10's review) — the client's entry paths see only `world.json`, which has no edges, so the rule lives in worldgen
- Modify: `crates/worldgen/examples/forge_proto/report.rs`

**Interfaces:** `--report` gains `aliased_regions_identical` (for 4 rows: `generate_region_forged(w, (o.x + W + 2, z))` equals `(o.x + 2, z)` column for column — yes/no), `seam_region_cliff` (max surface-height difference between the last block column of region `(o.x + W − 1, z)` and the first of `(o.x, z)` ÷ the largest neighbouring-column difference inside either, over 8 rows), `world_map_wraps` (`map.wrap_x && map.width · tile_m == width_m`), `rivers_across_seam` (count; for each, the largest gap between consecutive carved water columns, in cells).

- [ ] **Step 1: Temporary tests** (one 16,384 × 8,192 / 64 m Planet forged once; in `crates/worldgen/tests/forge_lib.rs`, removed after): `coarse_tile` for the key of `(o.x + W, z)` equals that of `(o.x, z)`; `split` the world with `tile_cells = 96` (256 columns → 3 tile columns, the last 64 wide): a bilinear read at artifact x = `width_m − 1.0` equals the whole-raster world's read there, and so does one at x = 1.0; `place` 500 m north of row 0 is `Place::Sea`; a seed found by search whose river has points on both sides of x = 0: along it no two consecutive carved water columns are more than 2 cells apart.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-report --size dev --cell 64 --set layout=6 --seeds 8`: `aliased_regions_identical: yes`, `seam_region_cliff` ≤ 1.0, `world_map_wraps: yes` on all 8. The safety net (gate + six crcs).
- [ ] **Step 4: Verify** — `just smoke`, `just determinism-wasm`. Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(worldgen): forged sampler, rivers, tiles and world map wrap east–west`.

### Task 13: Trees at the seam (and structure windows)

The old plan did not know this code: the tree lattice hashes on GLOBAL lattice coordinates and a candidate
competes with its 3×3 neighbours, so the last lattice column of the map must compete with the first.

**Files:** `crates/worldgen/src/trees/placement.rs` (`candidate(lx, lz, …)`, `tree_id`, `trees_in(x0, z0, x1, z1, …)`, `too_close`), `crates/worldgen/src/lib.rs` (the callers that hand `trees_in` a window: region generation, and `coarse_patch_window` for unwrapped coarse-LOD keys), `crates/worldgen/src/structures.rs` (`placements_in` / `paints_in` windows only).

**The rule:** fold `lx` into the map's lattice range before every `hash01` / `hash64` / `tree_id`, then shift the candidate's `pos` / `base` back by the image's offset, so window checks stay in the caller's frame. A window that crosses the seam (a coarse-LOD key) therefore yields the folded trees at unwrapped positions. No crown ever crosses a region edge (`candidate` rejects one whose `crown_radius_blocks + LEAF_REACH_PAST_CROWN_BLOCKS` reaches past its region), so no tree is produced by two regions. Tree ids now repeat with the map's width instead of 4,096 m, so two trees across the seam can share an id: confirm in `game` (`edit.rs`, `mining.rs` `player_chop_safe`, the tree-fall path) that an id is only ever compared within one region, and say so in the report; if it is followed into the halo, BLOCKED with the site. Structures and towns reach a forged world only through the dev sources of `server/src/dev_structures.rs` (`VOXEL_DEV_STRUCTURES`): their windows fold the same way; a dev structure placed across the seam is a Known limit, not built here. Non-wrapping worlds keep today's path exactly (`scene_hash_golden` and the layout crcs must not move).

- [ ] **Step 1: Audit** every global x in `trees/placement.rs` and in the window code of `structures.rs` (hash inputs, lattice ids, window bounds): list each in the report as folded / shifted back / region-local.
- [ ] **Step 2: Temporary tests** (on the Task 12 Planet; no `structures::install`, which is process-global): the tree lists of regions `(o.x + W − 1, z)` and `(o.x, z)` are what `trees_in` gives for those regions' folded windows; across the seam no two trunks are closer than `too_close` allows (checked over 8 rows, and it FAILS before the change on at least one seed — quote the seed); `trees_in` over an unwrapped window `[width − 64, width + 64)` equals the union of the two folded windows' trees shifted back; `--report`'s `aliased_regions_identical` still yes.
- [ ] **Step 3: Implement.** `--report` gains `trees_across_seam_ok` (yes/no over 8 rows: the spacing rule holds across the seam).
- [ ] **Step 4: Verify** — `just test-one worldgen "--test it -- scene_hash_golden:: tree_fall::"` unchanged, the safety net (gate + six crcs), `just smoke`, `just determinism-wasm`. Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(worldgen): the tree lattice is continuous across the seam`.

### Task 14: Client frame, window and outgoing canonicalisation

**Files:**
- Create: `crates/client/src/wrap_frame.rs`
- Modify: `crates/client/src/instance.rs`: own a `WrapFrame`; `ServerPacket::WorldMap` → `set_extent`; every home change (`PlayerRegion`, `HomeHop`, the predicted flip in `route_transfers`) → `follow_home`; the predicted relay's two plain `rebase(source, target)` calls in `route_transfers` — since Task 6 the predicted target is canonical, so a plain rebase moves a body `width − 1` regions the wrong way; `desired_window` and `desired_lod` compute around `frame.alias(home)` in unwrapped space, then subscribe to the de-duplicated `canonical` regions while LOD keys stay unwrapped; `route_transfers` rebases with `rebase_by(region_delta(..))` and its halo targets are canonical; `near_body` uses the wrapped delta; `viewer_world_xz` uses `frame.offset(home)`; `window_shift`, `window_for`, `advance_window_center`, `update_window`, `update_lod_window`, `apply_lod_window`; the frame is pushed to the render world on change — `ClientUpdateEvent` lives in `game` (Bevy-free), so it carries plain data, `ClientUpdateEvent::WrapFrame { extent: Option<MapExtent>, anchor: RegionCoords }`, from which the client builds its resource
- Modify: `crates/client/src/connections.rs` (the region → host map and observer bookkeeping key on canonical regions), `crates/client/src/renderer/commander/move_order.rs` (`goal_from_hover`, `goal_from_tile`, the region delta), `designate.rs`, `selection.rs` (reverse lookups through `region_at_world`), `crates/client/src/session.rs` (register the resource for session reset — `add_session_reset`)
- The narrow-world refusal (a wrapping world under `MIN_WRAP_WIDTH_M`) is built in Task 12 (worldgen); this task only adds the const to `crates/worldgen/src/forge/play.rs`

**Interfaces — produces:**
- `#[derive(Resource, Clone, Copy, Default)] pub struct WrapFrame { pub extent: Option<MapExtent>, pub anchor: RegionCoords }`
- `alias(&self, rc) -> RegionCoords` (the copy of canonical `rc` nearest `anchor`), `offset(&self, rc) -> [f32; 3]` (`alias(rc).world_offset()`), `canonical(&self, rc)`, `region_at_world(&self, x: f32, z: f32) -> RegionCoords` (canonical owner of a render-space point), `follow_home(&mut self, home)` (anchor becomes the alias of `home` nearest the old anchor, so the camera never jumps), `set_extent(&mut self, extent: Option<MapExtent>)`
- `pub const MIN_WRAP_WIDTH_M: f64 = 8192.0` in `worldgen::forge::play`: the NEAR window (subscribed regions) must never hold one region twice. The LOD horizon may: `MAX_VIEW_DISTANCE_M` is 16,384 m, more than half of the dev and Tiny worlds, so far rings can show a second image of terrain — that is what a small planet looks like, and Task 15 handles it (rings compare aliases).
- No lap re-centring: after two laps of a 200 km world f32 precision is still centimetres (Known limits). The server refuses LOD keys more than `LOD_WRAP_REACH_WIDTHS` map widths off the map (`crates/game/src/world_manager/mod.rs`, Task 8: 2) — measured in the client's never-re-centred frame, so a player who circles a small world more than twice would silently lose far LOD. Raise that const to 64 in this task (the alias index holds only live subscriptions, so the bound costs nothing per edit; it only keeps key arithmetic far from `i32` limits) and say so in its comment; a new session starts at the canonical frame.

- [ ] **Step 1: Temporary tests** (client lib): extent origin 0, width 10, wrapped, anchor (9, 0): `alias((0, 0)) == (10, 0)`, `alias((8, 0)) == (8, 0)`, `canonical((10, 0)) == (0, 0)`; anchor (9, 0), `follow_home((0, 0))` → anchor (10, 0), then `follow_home((9, 0))` → (9, 0); no extent → identity and `offset == world_offset`; home (9, 0) on a width-10 wrap, radius 1 → the subscribed set is the 9 canonical regions with x ∈ {8, 9, 0}; a hovered tile in alias region (10, 0) gives `Goal.region == (0, 0)`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify** — `just test-one client "--lib -- wrap_frame:: instance::"`, `just smoke`. Delete the temporary tests.
- [ ] **Step 4: Commit** `feat(client): wrap frame — canonical regions, unwrapped view`.

### Task 15: Render positions through the frame

**Files:** replace `rc.world_offset()` used for RENDER placement with `frame.offset(rc)`, and `RegionCoords::from_world` on render-space points with `frame.region_at_world`, in every hit of `grep -rn "world_offset()\|from_world(\|RegionCoords::new(" crates/client/src` — today: `renderer/bridge.rs` (region root transforms; `dedupe_halo_entities` stays keyed by canonical ids), `collider_debug.rs`, `look_target.rs`, `harness.rs`, `slice_plane.rs`, `block_damage.rs`, `item_piles.rs`, `boundaries.rs`, `fortress_view.rs` (if it still exists when the task runs — the owner is deleting the fortress files), `grid_debug.rs`, `bricks.rs`, `voxel_pull/{pipeline,gpu}.rs`, `session.rs`, `instance.rs`, `regions/runner.rs`, `autopilot.rs`, `renderer/commander/{mod (region_rect),first_person,visible,designate,level,cut_entities,selection,move_order,height,adventure,tiles,minimap}.rs`; `renderer/lod_rings.rs` keeps `key.world_offset()` (keys are already unwrapped) and its `replaced_region` test (`covered.contains(rc) || near_ready.contains(rc)`) compares ALIASES — a key's unwrapped regions against `frame.alias(near)` — never canonical ids: a far second image of a near region must stay drawn; the client's consumers of canonical halo keys (`GameDataUpdateKind::SetHaloTerrain` / `SetHaloGround`, the halo pile updates → `renderer/commander/tiles.rs`, the piles renderer) place through the frame; `renderer/slint_overlay.rs` (`map_tile` and its reverse go through the `WorldMap`'s canonicalisation), `arrival_tabs.rs`.

From Task 7 (built): the sim's extent lives in `GameData::map_extent` (not serialized, not hashed, never in the undo log; set by `Region::set_route_map`). `crates/client/src/regions/host.rs` hands the renderer a CLONE of `GameData` taken BEFORE `set_route_map`, so the render-side copy has no extent and any render code calling the seam helpers on it (`world_block`, `local_block`, `map()`) computes plain differences at the seam: give the render copy the extent (clone after the set, or set it on the copy). `item_piles::local_cell` is a plain-difference helper kept public for the client: its client callers go through the frame, then narrow or rename it.

- [ ] **Step 1: Temporary test** (client lib, `bridge.rs`): with a frame anchored at (10, 0) on a width-10 wrap, the root of region (0, 0) is placed at `10 · REGION_SIZE` in x.
- [ ] **Step 2: Implement.** Decide each hit: render placement → the frame; a neighbour for a subscription or a packet → `region_neighbour` / `canonical`; sim-local math inside one region → unchanged. The report lists every hit with its decision.
- [ ] **Step 3: Verify** — the test, `just smoke`. Delete the temporary test.
- [ ] **Step 4: Commit** `feat(client): render regions at their wrap-frame alias`.

### Task 16: Outside role `all` the seam is closed

**Ruling (2026-11-19, Phase 0):** the old plan refused a wrapping world under split roles. After Task 17 every world
is a wrapping Planet, so a refusal would leave `dev-r2`, `dev-r3`, `go-nogo-r3`, the twin, the panel's forge Job
and orion with no world they can run until the seam between hosts exists (the program's Phase 7, which may be
deferred). Instead a process that is not `VOXEL_ROLE=all` runs the same world with the seam CLOSED: its east and
west edges hold like today's outer edges. The terrain is still periodic (the sampler folds), so the far side is
visible across the edge and nothing changes in the store; Phase 7 opens the seam by deleting this task's override.

**Files:**
- Modify: `crates/server/src/roles.rs` (new `seam_for_role`; `run_coordinator` applies it to `opened.map` right after it opens the world, before the history grid and the coordinator state are built from the map; the file has an owner's uncommitted hunk — stage only your own), `crates/server/src/lib.rs` (applies it to `opened.map` right after `open_world_with`, before `check_before_registration`, `install_world` and anything else that reads the map)

**Interfaces — produces:** `pub fn seam_for_role(role: Role, map: &mut game::WorldMap) -> bool` — when `map.wrap_x && role != Role::All` it sets `map.wrap_x = false`, logs ONE line (`"[server] this world wraps east–west, but only a single process (VOXEL_ROLE=all) runs the seam yet: its east and west edges are closed here"`) and returns `true`; otherwise it changes nothing. The stored map is not rewritten. `Role::Fleet` serves no world. Every consumer downstream (regions' `MapExtent`, `RouteMap`, `SocietyGrid`, `ServerPacket::WorldMap` → the client's `WrapFrame`) therefore sees a non-wrapping map, and the host-to-host path never meets a seam.

- [ ] **Step 1: Temporary tests** (`roles.rs` lib): `(All, wrapping map)` → unchanged, `false`; `(Host, wrapping)` and `(Coordinator, wrapping)` → `wrap_x == false`, `true`; any role with a non-wrapping map → unchanged, `false`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify** — `just test-one server "--lib roles::"`, `just smoke`. Delete the temporary tests.
- [ ] **Step 4: Commit** `feat(server): outside role all a wrapping world's seam is closed`.

### Task 17: One generator — delete the old layouts, presets and knobs

**Files:**
- Modify: `crates/worldgen/src/forge/knobs.rs`: delete the knobs `layout`, `pole`, `latitude_span_deg`, `latitude_center_deg`, the whole "Island" group (`island_extent`, `island_count`, `island_spread`, `island_size_variation`, `island_separation`), `sea_edges`, `continent_coast`, `inland_sea_size`, and the island-only "Uplift" knobs `belt_width`, `belt_scale`, `inland_gate` (confirm each by grep: a knob with no reader left after the island body is gone is deleted, one still read is kept); delete `CHOICES`' `layout` and `pole` entries, `LAYOUT_*`, `layout()`, `is_continental()`, `is_planet()`, `sea_edge_count()`, `pole_choice()`. `ocean_margin_start`, `coast_noise_lo`, `coast_noise_hi` move to the "Continent" group, renamed "Land".
- Modify: `forge/world.rs`: delete the island body of `World::new`, `layout_blobs`, `landmass`, `Blob`, `Landmass`, the island constants, `ContinentalMass::{Continent, Inland}`, the seed-picked branch of `edges()`, the land-edge taper and `follow_land_edges`' land-edge handling, `LAND_EDGE_*`; `World::new` is what `new_continental` was. `ForgeParams::default()` becomes `DEV_WORLD_SIZE` at 32 m. Delete `ForgeParams::square`.
- Modify: `forge/climate.rs` (`latitudes`: one formula, 90° at row 0 to −90° at the last row; delete `POLE_*`), `forge/plates.rs` (`soften_along_land_edges` deleted), `forge/fields.rs`, `forge/hydrology.rs` (`Stats.layout`), `forge/quality.rs` (`land_band` has one band; `summarise` loses its `layout` argument), `forge/periodic.rs` (`for_world` always `Some`; the `None` code paths in the forge are deleted — `World.wrap_x` stays, for hand-built grids)
- Modify: `forge/presets.rs`: `PRESETS` = "Earth-like" (no overrides, first), "Great ranges", "Highlands", "Lowlands", "Dry", "Wet" (each with any deleted-knob override removed)
- Modify: `crates/worldgen/src/forged.rs` (`ARTIFACT_VERSION` 8 → 9 — that refuses the world FILE and, through `world.json`'s `artifact_version`, the menu list; a SQLite / Postgres store checks only the tile and map versions, so `WorldGlobals::validate` also refuses any `edges` other than `[Sea, Wrap, Sea, Wrap]` with the re-forge wording, which reaches file, tiles and store with no byte change — say in the report how each of the three paths refuses), `crates/panel/src/forge.rs` (`PRESETS` mirrors the preset names), `crates/client/src/frontend/generator.rs` (`TABS`: the "Land" tab's groups), `crates/world-store/src/ctl.rs` (`USAGE`), `crates/worldgen/examples/forge_proto/{main,search,serve,report}.rs` (the `layout` line goes), `crates/client/src/debug_launch.rs`, `crates/server/src/{lib,roles,world_file}.rs` (they use `ForgeParams::default()` — the dev size must work at region 64 and 256)
- **The gate:** `crates/worldgen/tests/forge_lib.rs` `forge_quality_matches_baseline` names `ForgeParams::square`, `LAYOUT_ONE_ISLAND`, `POLE_NONE` — all deleted here. Rewrite its body to forge `ForgeParams { seed: DEFAULT_SEED, ..ForgeParams::default() }` (the dev Planet at 32 m) against a new fixture path `tests/fixtures/forge_quality_planet_dev_32m.ron`, keep its `/// Gate:` marker, delete the old fixture — and do NOT record the new fixture yet: the program allows ONE re-record at the end of the phase, and Task 18 changes the climate once more. Between this task's commit and Task 18's the gate has no fixture and fails if run (it is `#[ignore]`d as slow, so `just smoke` passes) — ruled acceptable for two consecutive commits.
- Modify: `justfile` (comments on `forge-proto`, `forge-ui`, `run`, `play`, `server`; drop `--set layout=0`), `CLAUDE.md` (the Worldgen and Building paragraphs: one layout, the sizes, the wrap and the canonical-coordinates rule, the role-`all` limit, `forge-report`; the file has the owner's uncommitted hunks — stage only yours), `TODO.md` (the east–west wrap and world-edge items; the re-forge note for the public web world, dev Postgres worlds, the twin and orion)

**Interfaces:** stored knobs of a deleted name are ignored on load (`WorldGlobals::knobs()` ignores unknown names).

- [ ] **Step 1: Temporary tests:** `SPECS` has no knob named `layout`, `pole`, `latitude_span_deg`, `latitude_center_deg`, `sea_edges`, `island_extent`; `PRESETS[0].name == "Earth-like"` with no overrides, `PRESETS.len() == 6`, every override names an existing knob in range; `World::new(&ForgeParams::default())` has `nx == 512`, `nz == 256`, `wrap_x`, and `edges == [Sea, Wrap, Sea, Wrap]`; a version-8 artifact header is refused with text containing `re-forge`.
- [ ] **Step 2: Before deleting**, run `just forge-report --size dev --cell 32 --set layout=6 --seeds 8` and keep the output: after the deletion the same command without `--set layout=6` must print the SAME `height_crc` per seed (the deletion changes no Planet).
- [ ] **Step 3: Implement** the deletions.
- [ ] **Step 4: Evidence** — Step 2's comparison, made on the deletion BEFORE any retune (same `height_crc` per seed); the full 8-seed report quoted. Then, if a seed misses land share 0.15–0.6 or has fewer than two land components over 1%, tune the Planet's land knobs' DEFAULTS in a second commit and say what moved (the crcs then change on purpose: quote the new report).
- [ ] **Step 5: Verify** — `just test-one worldgen "--lib"`, `just smoke`, `just determinism-wasm`, `just clippy`; the gate COMPILES (`just test-one worldgen "--test it --no-run"`). Delete the temporary tests.
- [ ] **Step 6: Commit** `feat(forge)!: the Planet is the only world — old layouts, presets and their knobs removed (artifact v9)` (and, if tuned, `tune(forge): the Planet's land defaults`).

### Task 18: Climate lengths in degrees of latitude

**Files:** `crates/worldgen/src/forge/knobs.rs`, `forge/climate.rs`, `forge/presets.rs` ("Dry" / "Wet" overrides), the gate's new fixture `crates/worldgen/tests/fixtures/forge_quality_planet_dev_32m.ron`.

**Interfaces — produces:** knobs `land_depletion_deg: 16.0` (range 3–100), `sea_recharge_deg: 9.0` (1–36), `meander_wavelength_deg: 72.0` (9–180) replace `land_depletion_m`, `sea_recharge_m`, `meander_wavelength_km`. `pub fn metres_per_degree(height_m: f64) -> f64` = `height_m / 180.0`; each is converted with it at use. The defaults approximately reproduce today's 9 km, 5 km and 40 km on a Large world (557.5 m per degree: 8.9, 5.0 and 40.1 km). `OROGRAPHIC_RUN_M`, `TERRAIN_BLUR_M`, `RAIN_CROSS_BLUR_M`, `RAIN_ALONG_BLUR_M` and `orographic_scale_m` stay in metres: they describe hills, not the planet.

- [ ] **Step 1: Temporary test:** an idealised wrapped world (one meridional mountain wall, sea elsewhere) at 16,384 × 8,192 / 64 m and at 32,768 × 16,384 / 128 m (same grid, twice the metres): the precipitation rasters agree cell for cell within 5% away from the wall's ± 3 columns (fails before the change: the larger world's interior is relatively wetter).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the 8-seed dev report before and after (the climate changed on the dev size: quote biome shares and `sea_level_temp_c`). Then record the gate's fixture — the phase's ONE re-record — with `FORGE_QUALITY_BLESS=1 just test-one worldgen "--test it -- --include-ignored forge_lib::forge_quality_matches_baseline"`, in a commit of its own whose message states the old numbers (the one-island fixture Task 17 deleted: read them from `git show HEAD~N:crates/worldgen/tests/fixtures/forge_quality_16k_32m.ron`) and the new.
- [ ] **Step 4: Verify** — `just test-one worldgen "--test it -- --include-ignored forge_lib::"`, `just smoke`. Delete the temporary test.
- [ ] **Step 5: Commits** `feat(forge): climate relaxation and meander lengths in degrees of latitude` and `test(worldgen): re-record the forge quality gate on the dev Planet (old → new numbers)`.

### Task 19: Measure every size; evidence; close

**Files:** `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` ("Measurements": a "Phase 1a" table), `crates/worldgen/src/forge/play.rs` (`FORGE_SECS_PER_MCELL`, `FORGE_COST_EXPONENT`, `FORGE_RAM_*` refitted, with the date and machine in the comment), `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Evidence: Phase 1a), this file ("As built").

- [ ] **Step 1: Measure** (release, one at a time, nothing else of ours running): `just forge-bench 16384 8192 32`, then each of Tiny, Small, Medium at 16 m and at 32 m, and Large at 32 m. Large at 16 m only if `free -g` shows more than 20 GB available; otherwise record "not run: needs about 14 GB". For each: wall seconds, max RSS, `.world` file size; note `scaling_cur_freq`.
- [ ] **Step 2: Record** the table in the design beside the Phase 0 rows. No pass/fail line: the owner judges.
- [ ] **Step 3: Refit** the estimate constants (a temporary test: the estimate is within ±25% of every measured row).
- [ ] **Step 4: Phase evidence** — `just forge-report --size dev --cell 32 --seeds 8` (every line), `just world-map --tiles` of a dev Planet (PNG path): paste into the program's Evidence section, judged against the Phase 1a row's ranges.
- [ ] **Step 5: Whole suites, once:** `just test` in the foreground; quote the PASS/FAIL line per stage. A failure in this plan's files is fixed here; one outside them is recorded.
- [ ] **Step 6: Commit** `docs: Phase 1a measurements and evidence; forge estimate refitted`.

The live check (walk east across the seam, pan the LOD rings, look at the world map) is the OWNER's: it is listed under the program's "Owner hand checks" and never launched by an agent.

## Known limits (not in this plan)

- One process only. Under split roles the seam is closed (Task 16); the seam between hosts (peer links, ownership, land assignment, placement, observer sessions) is the program's Phase 7.
- A dev structure or town (`VOXEL_DEV_STRUCTURES`) placed across the seam is not handled.
- With a view distance past half the world's width the far LOD rings show a second image of the terrain.
- No lap re-centring of the client frame: fine for many laps of the largest world; after 64 laps in one session the server stops answering far LOD keys (a new session re-centres).
- The world-map window does not scroll around the seam; it shows the canonical rectangle.
- A voxel body that straddles the seam (a felled tree) keeps its existing in-region rule.
- The Planet's land is still today's "continents" formula and annual climate. Plates, monthly climate, water features and biomes are Phases 2–5.
