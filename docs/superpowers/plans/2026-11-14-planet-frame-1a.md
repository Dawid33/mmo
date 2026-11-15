# Planet Frame (Phase 1a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every forged world is a 2:1 planet in one of four sizes: it wraps east–west, has sea-and-ice poles on the north and south edges and a hot equator across the middle, and it is the only kind of world the forge makes.

**Architecture:** Three moves, in an order that keeps a working world at every commit. (1) The forge, the artifact, the tiles and the world map go from one `size_m` / `n` to `width × height` / `nx × nz`, proven a pure refactor by the existing bit-exact pins. (2) The wrap: every stored, simulated, hashed or sent `RegionCoords` is canonical (x folded into the map), the forge's grid, noise and geometry are periodic in x for the new Planet layout, the runtime sampler folds x, and the client renders through an unwrapped frame. (3) The old layouts, presets and knobs are deleted, the quality gate is re-recorded on a Planet world, and every size is measured.

**Tech Stack:** Rust; crates `worldgen` (forge, `forged.rs`, `forged_tiles.rs`, `tile_map.rs`), `game`, `server`, `world-store`, `client` (Bevy, Slint). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` (Phase 1, part 1a). The older `docs/superpowers/plans/2026-11-05-east-west-world-wrap.md` is superseded by this plan; its design points D1 and D3–D9 are restated below where a task relies on them. Code facts were gathered 2026-11-14 at `a24fc5ef`; line numbers are approximate — search by the named symbol.

Phase 1b (the stored monthly-climate family) is a separate plan, written after this one lands.

## Global Constraints

- Do not create git worktrees; work on `develop`. The tree has the user's unrelated uncommitted changes: each commit `git add`s only the task's files.
- Until Task 15 the six existing layouts must forge **bit-identically** when `width_m == height_m`: `island_layout_unchanged` (crc pins), `forge_quality_matches_baseline` (`tests/fixtures/forge_quality_16k_32m.ron`) and the wasm determinism goldens stay green untouched. They are the safety net for Tasks 1–14.
- There is never a commit with no working world: old layouts are deleted only in Task 15, after a Planet world forges and plays.
- No forge-time or memory LIMIT is added or enforced; the existing RAM check against physical memory stays. Measurements are recorded (Task 17).
- Wrap is for one process (`VOXEL_ROLE=all`, `VOXEL_HOSTS` unset or 1). Do not change `halo::neighbours`, `peers.rs`, `ownership.rs`, `placement.rs` or anything else on the host-to-host path beyond what a task names.
- `game` and `server` stay Bevy-free; wire format stays `game::encode_packet` / `decode_packet`; hash with `StateHash` (derive on any new state type); `BTreeMap`, never `HashMap`, in sim and history.
- Never write a region edge as a literal: derive from `REGION_SIZE` / `REGION_BLOCKS`. Tests build at region 64, play at 256.
- Runtime sampling in `forged.rs` keeps to exact arithmetic (add, subtract, multiply, divide, floor, sqrt).
- Tests: `just test-one <crate> "<cargo test args>"`, `just smoke`. Tag tests slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`. Client unit tests are a slow opt-in: ask the user before running them. `just test-threads` after Task 6; `just determinism-wasm` after Tasks 2, 6, 11 and 15.
- Any new or changed build/run/test command updates its `justfile` recipe in the same task.
- Stored formats are bumped ONCE, in Task 2: `ARTIFACT_VERSION` 7 → 8, `TILED_FORMAT_VERSION` 1 → 2, `WORLD_MAP_VERSION` 1 → 2, `META_FORMAT` 1 → 2. Later tasks must not need another bump (Task 9 appends `EdgeKind::Wrap` — define the variant in Task 2).

## Shared vocabulary (decided here, used by every task)

- Axes: x runs west → east and wraps; z runs north (row 0) → south. `nx` columns, `nz` rows, row-major `i = z * nx + x`.
- `ForgeParams { width_m, height_m, cell_m, seed, knobs, quiet }`. `World { nx, nz, dx, wrap_x, … }`.
- `WorldArtifact` / `WorldGlobals`: `width_m`, `height_m`, `cell_m`, `nx`, `nz`, `knobs: BTreeMap<String, f64>`.
- World sizes (`forge::play`):

  | Name | `width_m` | `height_m` |
  |---|---|---|
  | Tiny | 25,088 | 12,544 |
  | Small | 50,176 | 25,088 |
  | Medium (default) | 100,352 | 50,176 |
  | Large | 200,704 | 100,352 |
  | Dev (not in the menu) | 16,384 | 8,192 |

  All are multiples of 512 m (the largest `REGION_BLOCKS`) and of 128 m (the coarsest forge cell, `cell_m * 2^coarse_levels`, for 32 m cells at the default two coarse levels; 64 m for 16 m cells), so every size is a whole number of regions and of coarsest cells with no snapping.
- `EdgeKind { Sea, Land, Wrap }`; edges are indexed N, E, S, W. A Planet has `[Sea, Wrap, Sea, Wrap]`.
- Canonical region: x folded into `[origin.x, origin.x + width)`. Unwrapped coordinates exist only transiently, in the client's render frame and window math.

## Review Focus

1. **A body seen from both sides of the seam at once** — a region in the map's last column ghosting into the first: the ghost's `source_region` must be canonical on server and client, or halo de-duplication and prediction matching miss. Test in Task 6 (`ghost_across_seam_keeps_canonical_source`).
2. **Unwrapped coordinates reaching the server** — a commander click past the seam yields a region outside the map; the goal, `PlayRegion` and region requests must arrive canonical, and the server canonicalises anyway. Tests in Task 6 (`manager_canonicalises_inbound_regions`) and Task 12 (`goal_from_tile_across_seam`).
3. **Rivers and lakes that straddle x = 0** — a reach must carve continuously and a lake must be one body, not a segment drawn across the whole map or two half-lakes. Tests in Task 9 (`lake_spans_the_seam_as_one_body`) and Task 11 (`river_crosses_the_seam_continuously`).
4. **A width that is not a whole number of store tiles** — every menu size is a whole number of regions, but `nx` is not a multiple of `TILE_CELLS` (256) for all of them, so the last tile column is partial and its east neighbour is tile column 0. Test in Task 11 (`partial_last_tile_column_wraps`).
5. **An old world on disk** — a version-7 artifact, tiled store or `world.json` must be refused with a "re-forge this world" message in the load list and at server start, never a panic or a silent default forge over it. Test in Task 2 (`old_artifact_is_refused_with_reforge_message`).

---

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

### Task 3: World sizes, estimate and command lines

**Files:**
- Modify: `crates/worldgen/src/forge/play.rs` (`WORLD_SIZES`, `DEFAULT_WORLD_SIZE`, `forge_estimate`, `preview_cell_for`, `offered_cells`, `check_play_ram`)
- Modify: `crates/world-store/src/ctl.rs` (`USAGE`, `parse_forge`), `crates/worldgen/examples/forge_proto/{main,search,serve}.rs`
- Modify: `crates/client/src/frontend/generator.rs` (`size_m()`, `params`, the size row, the estimate line, the cache key, `WorldMeta`), `crates/client/src/frontend/load_world.rs`
- Modify: `justfile` (`forge-bench`, the comments on `forge-proto`, `forge-db`, `worldctl`)
- Test: `play.rs` tests, `crates/world-store/tests/worldctl.rs`, generator tests

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces:
  - `pub struct WorldSize { pub name: &'static str, pub width_m: f64, pub height_m: f64 }`; `pub const WORLD_SIZES: [WorldSize; 4]` (Tiny, Small, Medium, Large — the table above); `pub const DEFAULT_WORLD_SIZE: usize = 2`; `pub const DEV_WORLD_SIZE: WorldSize` (16,384 × 8,192); `pub fn world_size_named(name: &str) -> Option<WorldSize>` (case-insensitive, includes `"dev"`).
  - `pub fn forge_estimate(width_m: f64, height_m: f64, cell_m: f64) -> ForgeEstimate` (`cells = nx * nz`); `preview_cell_for(width_m, height_m)`; `offered_cells(width_m, height_m)`; `check_play_ram(width_m, height_m, cell_m, physical)` — message `"{W} × {H} km at {cell} m needs …"`.
  - CLI, in both `worldctl forge` and `forge_proto`: `--size <tiny|small|medium|large|dev>`, or `--width <m> --height <m>`; a bare number after `--size` is refused with a message naming the two forms.
  - `forge-bench width="16384" height="8192" cell="32":` passing `--width`/`--height`.

- [ ] **Step 1: Write the failing tests:**
  - `play::sizes_are_two_to_one_and_region_aligned`: for every `WORLD_SIZES` entry and `DEV_WORLD_SIZE`, `width_m == 2.0 * height_m`, and both are multiples of 512.0 and of 128.0.
  - `play::estimate_counts_cells_once`: `forge_estimate(100_352.0, 50_176.0, 16.0).secs == forge_estimate(200_704.0, 100_352.0, 32.0).secs`.
  - `worldctl::size_names_and_explicit_dimensions`: `parse_forge(["--db", url, "--size", "small"])` gives 50,176 × 25,088; `["--width", "8192", "--height", "4096", "--cell", "64"]` gives those; `["--size", "16000"]` is `Err` containing `--width`.
  - `generator::world_size_row_sets_dimensions` (replaces `world_size_row_sets_size_m`): the default state's params are 100,352 × 50,176; selecting row 0 gives 25,088 × 12,544.
- [ ] **Step 2: Run** `just test-one worldgen "--lib forge::play::"` → compile failure.
- [ ] **Step 3: Implement.** The generator's size row lists `"{name} · {W} × {H} km · {estimate}"`. No size is hidden for being slow; only the existing physical-RAM check can refuse.
- [ ] **Step 4: Run** the three targets above (ask before the client one), `just test-one world-store "--test it worldctl::"`, `just smoke` → PASS.
- [ ] **Step 5: Commit** `feat(forge): four 2:1 world sizes; --size names and --width/--height`.

### Task 4: World map of a rectangle

**Files:**
- Modify: `crates/worldgen/src/tile_map.rs` (`build_world_map`, `summarize_tile_with`)
- Modify: `crates/worldgen/examples/world_map.rs` (image is `width × height` tiles), `crates/client/src/renderer/slint_overlay.rs` world-map helpers and the Slint map window only where they assume a square image
- Test: `crates/worldgen/tests/tile_map.rs`

**Interfaces:**
- Consumes: Task 2 (`artifact_bounds_engine_m` as a rectangle). `WorldMap` already has `width` and `height`.

- [ ] **Step 1: Write the failing test** `tile_map::map_of_a_two_to_one_world`: for an 8192 × 4096 m world, `map.width as f64 * tile_m >= 8192.0`, `map.height as f64 * tile_m >= 4096.0`, `map.width >= 2 * map.height - 1`, and a tile in the east half (x index ≥ `width / 2`) has a non-default summary.
- [ ] **Step 2: Run** `just test-one worldgen "--test it tile_map::map_of"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one worldgen "--test it tile_map::"`, `just smoke` → PASS; `just world-map` on a `just forge-proto --width 8192 --height 4096 --cell 64` world writes a 2:1 PNG.
- [ ] **Step 5: Commit** `feat(worldgen): the world map follows the world's width and height`.

### Task 5: `MapExtent` wrap API

**Files:**
- Modify: `crates/game/src/world_map.rs` (`WorldMap`, `MapExtent`, `off_map`)
- Modify: every `MapExtent { .. }` literal (10: `server/src/claims.rs`, `server/tests/common/hosts_rig.rs`, `game/src/world_map.rs`, `game/tests/people.rs`, `game/tests/adventure.rs` ×4, `game/tests/ownership.rs` ×2) and every `WorldMap { .. }` literal (`grep -rn "WorldMap {" crates`), adding `wrap_x: false`
- Test: `crates/game/tests/world_wrap.rs` (new; add `mod world_wrap;` to `crates/game/tests/it.rs`)

**Interfaces:**
- Produces (in `world_map.rs`, re-exported at the crate root like `off_map`):
  - `pub struct MapExtent { pub origin: RegionCoords, pub width: u32, pub height: u32, pub wrap_x: bool }`
  - `impl MapExtent { pub fn canonical(&self, rc: RegionCoords) -> RegionCoords; pub fn contains(&self, rc: RegionCoords) -> bool /* wrapped: z range only */; pub fn neighbour(&self, rc: RegionCoords, dx: i32, dz: i32) -> Option<RegionCoords> /* canonical; None off-map */; pub fn delta(&self, from: RegionCoords, to: RegionCoords) -> (i32, i32) /* x shortest signed when wrapped, ties → positive */ }`
  - Free functions over `Option<MapExtent>` (no map = unbounded plane): `pub fn canonical(map, rc) -> RegionCoords`, `pub fn region_neighbour(map, rc, dx, dz) -> Option<RegionCoords>`, `pub fn region_delta(map, from, to) -> (i32, i32)`.
  - `WorldMap { …, pub wrap_x: bool }`; `extent()` copies it; `tile(rc)` canonicalises first.
- `wrap_x: false` and "no map" stay supported for good: procedural test worlds and hand-built maps have no wrap. Only forged worlds always wrap (from Task 9).

- [ ] **Step 1: Write the failing tests** with `let m = MapExtent { origin: RegionCoords::new(-3, -2), width: 10, height: 4, wrap_x: true }`:
  - `canonical_folds_x_only`: `m.canonical((7, 0)) == (-3, 0)`, `m.canonical((-4, 1)) == (6, 1)`, `m.canonical((2, 9)) == (2, 9)`.
  - `contains_ignores_x_when_wrapped`: `contains((100, 0))`, `!contains((0, 2))`, `!contains((0, -3))`.
  - `neighbour_wraps_east_and_west`: `neighbour((6, 0), 1, 0) == Some((-3, 0))`, `neighbour((-3, 0), -1, 0) == Some((6, 0))`, `neighbour((0, 1), 0, 1) == None`.
  - `delta_is_shortest`: `delta((6, 0), (-3, 0)) == (1, 0)`, `delta((-3, 0), (6, 0)) == (-1, 0)`, `delta((-3, 0), (2, 0)) == (5, 0)`.
  - `unwrapped_extent_is_plain`: the same calls with `wrap_x: false` give plain subtraction / `None` past x; `region_neighbour(None, (5, 5), 1, 0) == Some((6, 5))`.
  - `world_map_tile_canonicalises`: a 10 × 4 wrapped `WorldMap` returns the same tile for `(7, 0)` and `(-3, 0)`.
- [ ] **Step 2: Run** `just test-one game "--test it world_wrap::"` → compile failure.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one game "--test it world_wrap::"`, `just smoke` → PASS.
- [ ] **Step 5: Commit** `feat(game): MapExtent east–west wrap helpers`.

### Task 6: Sim crossings, routes and inbound canonicalisation

**Files:**
- Modify: `crates/game/src/protocol.rs` (`rebase_isometry` ~293 and its callers ~608, ~663)
- Modify: `crates/game/src/state.rs` (`scan_boundaries` ~1857: `closed` and leaver / ghost targets via `region_neighbour`; the two `off_map` uses)
- Modify: `crates/game/src/world_manager/mod.rs` (the departure relay: rebase by `region_delta(self.map_extent(), rc, target)`; inbound packet handling canonicalises `RequestRegionConnection`, `ReleaseRegionConnection`, `PlayRegion`, `Embark.region`, `GameEvent.region_id` before the `== home` check, and `Goal.region` inside `Order::MoveTo`; `play_region` ~1607 and `landing_in` ~1774 take canonical; the five `off_map` uses), `world_manager/placement.rs` (its `off_map` use)
- Modify: `crates/game/src/path_plan.rs` (`RouteMap`, `next_region` ~183, `route_step` ~197, `line_step` ~263: neighbours via `MapExtent::neighbour`, directions via `delta`), `crates/game/src/adventure.rs` (`plan_walker` ~190), `crates/game/src/people.rs` (`SpawnAt::Edge` ~99, `walk_out_safe` ~159), `crates/game/src/item_piles.rs`, `crates/game/src/containers.rs` (their `rc.x ± 1` uses, where a map is in reach)
- Do NOT modify: `halo::neighbours` (`halo.rs:292`), `world_manager/peers.rs`, `ownership.rs` — the host-to-host path. A single-process manager's halo across the seam is handled in `world_manager/mod.rs` by canonicalising the result of `halo::neighbours(rc)` at its four call sites there.
- Test: `crates/game/tests/world_wrap.rs`

**Interfaces:**
- Consumes: Task 5.
- Produces: `pub fn rebase_isometry_by(iso: &Pose, delta: (i32, i32)) -> Pose` (translation −= delta · `REGION_SIZE`). `rebase_isometry(iso, from, to)` stays, defined as `rebase_isometry_by(iso, (to.x − from.x, to.z − from.z))`, for callers with no map; every caller holding a map uses `rebase_isometry_by(iso, region_delta(map, from, to))`.

- [ ] **Step 1: Write the failing tests** (an `InlineSpawner` `WorldManager` with a procedural generator and a hand-built wrapped `WorldMap` of width 4, height 2, origin (0, 0), installed by `set_world_map`; reuse the helpers of `tests/play_region.rs` / `tests/adventure.rs`):
  - `body_crosses_east_seam_into_origin`: a client in region (3, 0) whose body is at `x = REGION_SIZE − 0.5` moving +x; after the crossing its home is (0, 0) and its region-local x is in `[0, 4)`.
  - `body_crosses_west_seam`: the mirror, (0, 0) → (3, 0).
  - `north_edge_still_holds`: a body pushing −z in row 0 stays in its region (the behaviour `body_is_held_at_the_outer_seam` pins today).
  - `ghost_across_seam_keeps_canonical_source`: a body near (3, 0)'s east edge produces a halo entity in (0, 0) with `source_region == (3, 0)`.
  - `walker_goal_across_seam`: `Order::MoveTo(Goal { region: (0, 0), .. })` from region (3, 0) leaves by the EAST edge.
  - `manager_canonicalises_inbound_regions`: `ClientPacket::PlayRegion((4, 0))` moves the client to (0, 0); `RequestRegionConnection((-1, 0))` subscribes (3, 0).
  - `unwrapped_map_unchanged`: the same east-edge push on the same map with `wrap_x: false` is held at the edge.
- [ ] **Step 2: Run** `just test-one game "--test it world_wrap::"` → the new tests FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one game "--test it world_wrap::"`, `just test-one game "--test it -- --include-ignored adventure:: play_region:: path_plan::"`, `just test-threads`, `just determinism-wasm` → PASS.
- [ ] **Step 5: Commit** `feat(game): region crossings and routes wrap across the east–west seam`.

### Task 7: Server LOD across the seam

**Files:**
- Modify: `crates/game/src/world_manager/mod.rs` (`start_lod_build` ~4595: source region = `canonical(map, key.origin_region())`; `start_tile_build` ~4653: terrain from `canonical(r)`, positioned by the unwrapped `r` of `key.regions()`; `invalidate_region_lod` ~4741: invalidate every subscribed key with any `key.regions()` canonicalising to `rc`; generator closures always receive canonical coordinates)
- Test: `crates/game/tests/world_wrap.rs`

**Interfaces:**
- Consumes: Task 5. LOD keys stay UNWRAPPED on the wire: a tile key spans several regions and the map width need not divide by the tile span, so the client sends keys in its own frame and the server answers with the same key. `copy_region_cells_into_tile(region, key, ..)` (`lod.rs:703`) is unchanged — it is called with the unwrapped `r`.

- [ ] **Step 1: Write the failing tests:**
  - `lod_key_past_the_seam_builds_from_canonical_region`: width-4 wrapped map; `RequestLod(LodKey::for_region(2, (4, 0)))` is answered with `LodData` for that same key whose cells equal the `LodData` of `LodKey::for_region(2, (0, 0))`.
  - `edit_invalidates_aliased_key`: a block edit in (0, 0) re-sends `LodData` for a subscribed key over (4, 0).
- [ ] **Step 2: Run** `just test-one game "--test it world_wrap::lod"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one game "--test it world_wrap:: lod_streaming::"` → PASS.
- [ ] **Step 5: Commit** `feat(game): LOD keys past the seam build from canonical regions`.

### Task 8: History on a cylinder

**Files:**
- Modify: `crates/game/src/history/grid.rs` (`SocietyGrid` gains `wrap_x` from `WorldMap`; `index` canonicalises; `label_components` ~31 8-neighbours wrap; `route` ~166 neighbours wrap and its heuristic uses the wrapped |dx|)
- Modify: `crates/game/src/history/chain.rs` (~76 local `chebyshev`, `embark_target` ~295), `history/bootstrap.rs` (~46 local `chebyshev`), `history/handover.rs` (`entry_side` ~44), `history/journey.rs` (`leg_arrivals` ~104), `crates/client/src/debug_launch.rs` (~274 local `chebyshev`)
- Test: `crates/game/tests/history.rs`

**Interfaces:**
- Produces: `impl SocietyGrid { pub fn distance(&self, a: RegionCoords, b: RegionCoords) -> i64 /* Chebyshev, wrapped in x */; pub fn delta(&self, from: RegionCoords, to: RegionCoords) -> (i32, i32) }`. The three local `chebyshev` functions are deleted in favour of `distance`.

- [ ] **Step 1: Write the failing tests** (a wrapped `WorldMap` of width 12, land everywhere unless said):
  - `route_crosses_the_seam`: `route(grid, (11, 0), (1, 0), ..)` is 3 regions long and passes (0, 0).
  - `landmass_joins_across_seam`: land in columns x = 0 and x = 11 only → one component.
  - `embark_too_close_across_seam`: a site at (11, 0) refuses an embark at (1, 0) with `TooClose` (`EMBARK_MIN_DISTANCE_REGIONS` = 4).
  - `unwrapped_grid_unchanged`: the same three queries with `wrap_x: false` give no route across, two components, and an allowed embark.
- [ ] **Step 2: Run** `just test-one game "--test it history::"` → new tests FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one game "--test it history:: history_play:: history_handover::"` → PASS.
- [ ] **Step 5: Commit** `feat(history): routes, landmasses and site spacing wrap east–west`.

### Task 9: The Planet layout — a grid that wraps

**Files:**
- Modify: `crates/worldgen/src/forge/knobs.rs` (`LAYOUT_PLANET = 6` appended to the constants and `CHOICES`; `LAYOUT_LAST`; `pub fn is_planet(&self) -> bool`; `is_continental()` stays `>= LAYOUT_CONTINENT`, so a Planet is continental; `pole_choice()` returns `POLE_BOTH` and the latitude span is 90 when `is_planet()`)
- Modify: `crates/worldgen/src/forge/world.rs` (`edges()`: a Planet is `[Sea, Wrap, Sea, Wrap]`; `World { …, pub wrap_x: bool }`; `neighbors` wraps x; new `step`; `on_border`, outlet setup, the "every border cell is an outlet" asserts and `follow_land_edges` cover only the non-wrapped edges; `upsample_onto`'s x axis samples circularly; `ContinentalMass`: a Planet uses the `Continents` formula with sea on N and S only)
- Modify: `forge/routing_fast.rs` (`Steps` ~145 and `route_fast`'s border assert ~176: see below), `forge/erosion.rs` (diffusion ~280 covers columns 0 and nx−1 with x ± 1 mod nx when wrapping), flood fills and neighbour walks via `World::step`: `forge/mod.rs` (`lake_bodies`), `forge/hydrology.rs` (`find_lakes`, `find_islands`), `forge/fields.rs` (`closed_lakes`, `ground_slope`, `nearest_water`), `forge/regions.rs` (`components`, `merge`, `kind_at`), `habitat.rs` (`coast_distance_by`), `forge/post.rs`, `forge/spawn.rs` (edge clearance on N and S only), `forge/quality.rs`, `forge/render.rs`
- Modify: `forge/climate.rs` (`box_blur_rows` circular along x when wrapping; `orographic`'s per-row x sweep)
- Test: `crates/worldgen/src/forge/world.rs`, `forge/routing_fast.rs` tests, `crates/worldgen/tests/forge_lib.rs`

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces:
  - `pub const LAYOUT_PLANET: u32 = 6`.
  - `impl World { pub fn step(&self, i: usize, dx: i32, dz: i32) -> Option<usize> /* the one wrap-aware offset helper: x wraps when wrap_x, z never; None off the north or south edge, or off east/west when not wrapping */ }`.
  - `fn planet(width_m, height_m, cell_m) -> ForgeParams` test helper in `tests/common` and in the forge's test modules: `layout = LAYOUT_PLANET`, default seed.
- Routing: `Steps::new` uses flat index offsets that are only valid because the whole border is outlets. With `wrap_x`, columns 0 and nx−1 are ordinary cells. Rewrite `Steps` to carry `nx` and `wrap_x` and compute a neighbour as `(x ± 1 mod nx, z ± 1)`; the border assert becomes "every cell of rows 0 and nz−1 is an outlet" when wrapping, "every border cell" otherwise. The shore check ~214 wraps its x range the same way. The row-parallel passes stay row-parallel (a wrapped neighbour is in the same or an adjacent row).
- Chamfer distances (`nearest_water`, `coast_distance_by`) with wrap: repeat the forward + backward pass pair until no cell changes (at most two extra pairs).
- Climate sweep with wrap: there is no edge inflow. Each row's sweep runs one spin-up lap around the row starting from moisture 1.0, then the recorded lap continuing from where the first ended.

- [ ] **Step 1: Write the failing tests:**
  - `world::planet_edges`: `edges(seed, &planet_knobs) == [Sea, Wrap, Sea, Wrap]` for 16 seeds.
  - `world::neighbors_wrap_x`: on a wrapped 8 × 8 `World`, `neighbors(0)` yields exactly indices {1, 7, 8, 9, 15}; unwrapped, {1, 8, 9}. `step(7, 1, 0) == Some(0)`; `step(0, 0, -1) == None`.
  - `world::planet_has_sea_on_both_polar_edges`: for 8 seeds at 16,384 × 8,192 / 128 m, every cell of rows 0 and nz−1 is ocean.
  - `routing_fast::routes_across_the_seam`: a hand-built wrapped 16 × 8 world that slopes down toward x = 0 from both sides with its only outlets in row 0 — a cell at x = 15 has its receiver at x = 0.
  - `forge_lib::lake_spans_the_seam_as_one_body`: a hand-built wrapped world with one depression covering columns {nx−2, nx−1, 0, 1} → `find_lakes` returns one lake of that area.
  - `forge_lib::planet_drains_without_leaving_x` (slow): a 16,384 × 8,192 / 64 m Planet forge; every land cell's receiver chain ends at an ocean cell.
  - `climate::polar_rows_are_coldest_and_the_middle_hottest`: on a Planet, sea-level temperature of rows 0 and nz−1 is below −15 °C and of row nz/2 above 20 °C.
  - `climate::rain_has_no_seam`: on an all-ocean wrapped test grid, precipitation at x = 0 and x = nx−1 of every row differ by less than 1%.
- [ ] **Step 2: Run** `just test-one worldgen "--lib forge::world::planet"` → compile failure.
- [ ] **Step 3: Implement.** Noise and geometry are NOT yet periodic (Task 10), so a Planet forged now has a visible terrain step at the seam; drainage and climate already work across it.
- [ ] **Step 4: Run** `just test-one worldgen "--lib forge::"`, `just test-one worldgen "--test it -- --include-ignored forge_lib::"` → PASS, old pins unchanged.
- [ ] **Step 5: Commit** `feat(forge): the Planet layout — polar seas, a grid that wraps east–west`.

### Task 10: Periodic noise and geometry

**Files:**
- Create: `crates/worldgen/src/forge/periodic.rs`
- Modify: `forge/mod.rs` (`pub mod periodic;`), `forge/world.rs` (shape, belt, rough and taper noises; `ContinentalMass` centres; `ContinentalShape`), `forge/plates.rs` (`plates` sites, `Warp`, `convergent_boundaries`, `plate_field`, `interior_ranges`, `interior_strength`; `soften_along_land_edges` has no land edges on a Planet), `forge/fields.rs` (`VolcanismNoise`, `volcanism_grid`, `volcano_sites_on` spacing, `cone`), `forge/geology.rs` (no x dip and x-periodic folds when wrapping; z dip only), `forge/regions.rs` (`savagery`), `forge/climate.rs` (`meander_deg`), `forge/post.rs` (lake-site spacing)
- Test: `forge/periodic.rs` unit tests, `crates/worldgen/tests/forge_lib.rs`

**Interfaces:**
- Consumes: Task 9.
- Produces, in `periodic.rs`:
  - `pub struct PeriodicX { pub period_m: f64, pub band_m: f64 }`
  - `pub fn fold(&self, x: f64) -> f64` (into `[0, period)`), `pub fn dx(&self, a: f64, b: f64) -> f64` (shortest signed b − a)
  - `pub fn sample(&self, x: f64, z: f64, f: impl Fn(f64, f64) -> f64) -> f64`
  - `pub fn with_images<T: Clone>(&self, items: &[T], x_of: impl Fn(&T) -> f64, shift: impl Fn(&T, f64) -> T) -> Vec<T>` (each item plus its copies at x ± period)
  - `pub fn for_world(p: &ForgeParams) -> Option<PeriodicX>` — `Some` for a Planet, `None` otherwise; with `None` every call site keeps today's code path exactly.
- `sample`, for x folded into `[0, period)`: outside the last `band_m` it is `f(x, z)`; inside, with `t` running 0 → 1 across the band and `θ = π/2 · smoothstep(t)`, it is `cos θ · f(x, z) + sin θ · f(x − period, z)`. `cos² + sin² = 1` keeps the noise's variance and the smoothstep keeps its slope continuous. `band_m = min(period / 4, 4 × the noise's longest wavelength)`, set per noise at its construction.
- Geometry: Voronoi sites, boundary and range segments, continent centres, volcano and lake sites get images at x ± period (`with_images`) before any nearest-site or segment-distance query; queries use folded x. `meander_deg`: each wave's x-frequency is rounded to a whole number of cycles per period (at least 1).

- [ ] **Step 1: Write the failing tests:**
  - `periodic::sample_is_continuous_at_the_seam`: for an `OctaveNoise` `f`, `|sample(period − ε, z) − sample(ε, z)| < 1e-3` with ε = 1e-4 · period, over 64 z values.
  - `periodic::sample_keeps_variance`: the variance of `sample` over the band is within 10% of `f`'s over the interior (10⁴ points).
  - `periodic::images_find_nearest_across_seam`: sites at x = 1 and x = 50, period 100 — the nearest to x = 99 is the x = 1 site, at distance 2.
  - `forge_lib::planet_has_no_seam_step` (slow): 16,384 × 8,192 / 64 m Planet; the 99th percentile of `|h(nx−1, z) − h(0, z)|` over rows is ≤ 1.5 × the 99th percentile of interior column-to-column `|Δh|`. The same for annual precipitation and for the biome raster's share of differing neighbours.
  - `forge_lib::planet_forges_for_many_seeds` (slow): 8 seeds at 16,384 × 8,192 / 128 m all finish, each with land share between 0.15 and 0.6 and at least two land components larger than 1% of the map.
- [ ] **Step 2: Run** `just test-one worldgen "--lib forge::periodic::"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one worldgen "--lib forge::"`, `just test-one worldgen "--test it -- --include-ignored forge_lib::"` → PASS, old pins unchanged. `just forge-proto --set layout=6 --size dev --out target/world-forge/planet` and `just world-map` on it: the PNG's left and right columns match by eye, poles white, middle green or tan.
- [ ] **Step 5: Commit** `feat(forge): periodic noise and geometry for the Planet`.

### Task 11: Runtime sampler, rivers, tiles and the world map wrap

**Files:**
- Modify: `crates/worldgen/src/forged.rs`: `to_artifact` ~1041 folds x into `[0, width_m)` when `edges[1] == Wrap`; `clamped`, `q_cell`, `bilinear_cells`, `bilinear_q`, `bicubic`, `geo_offset_m`, `nearest_cell`, `prefetch_rect`, `lake_level_at`, `surface_rock` wrap x and clamp z; `place` ~1263 never reports east or west "past" for a wrapping world; river segments are also bucketed shifted by ± `width_m` where the shifted copy overlaps the map; `patch` ~1451 gathers segments and lakes for the window's folded x-intervals; the runtime noises (`detail`, `dither`, `snow_patch`) go through `PeriodicX`
- Modify: `crates/worldgen/src/forged_tiles.rs` (`TileCache::tiles_of_cells` ~956 and `route_cell`: a read past the last tile column continues in column 0; the last column may be partial)
- Modify: `crates/worldgen/src/forge/hydrology.rs` (`find_rivers` ~217: consecutive reach points more than `width_m / 2` apart in x are unwrapped by ± `width_m`, so a reach stays continuous; a lake's row runs may wrap)
- Modify: `crates/worldgen/src/tile_map.rs` (`build_world_map`: a wrapping world has `width = round(width_m / tile_m)` exactly and `wrap_x: true`; slope and extended-height reads wrap)
- Test: `crates/worldgen/tests/forged_wrap.rs` — a module of `tests/it.rs` if it forges in-process like `forged_world.rs`; its own `[[test]]` only if it must install a process-global world

**Interfaces:**
- Consumes: Tasks 2, 5, 9, 10.
- `Lake.cells` runs that cross the seam are split into two runs at x = 0 when written, so every run stays within a row.

- [ ] **Step 1: Write the failing tests** (one 16,384 × 8,192 / 64 m Planet forged once for the module; slow):
  - `aliased_regions_generate_identically`: with `o = map.origin`, `W = map.width`: `generate_region_forged(w, (o.x + W + 2, o.z + 1))` equals `generate_region_forged(w, (o.x + 2, o.z + 1))` column for column.
  - `seam_region_has_no_cliff`: for 8 rows, surface heights of the last block column of region `(o.x + W − 1, z)` and the first of `(o.x, z)` differ by no more than the largest neighbouring-column difference inside either region.
  - `river_crosses_the_seam_continuously`: search seeds in the test until a reach has points on both sides of x = 0; along it no two consecutive carved water columns are more than 2 cells apart.
  - `coarse_tile_past_seam_matches`: `coarse_tile` for the key of `(o.x + W, z)` equals that of `(o.x, z)`.
  - `partial_last_tile_column_wraps`: `split` the world with `tile_cells = 96` (256 columns → 3 tile columns, the last 64 wide); a bilinear read at artifact x = `width_m − 1.0` equals the whole-raster world's read there, and so does one at x = 1.0.
  - `world_map_is_exactly_wrapped`: `map.wrap_x && map.width as f64 * tile_m == 16_384.0`.
  - `north_of_the_map_is_sea`: `place` 500 m north of row 0 is `Place::Sea`.
- [ ] **Step 2: Run** the module → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the module with `--include-ignored`, `just test-one worldgen "--test it tile_map:: forged_tiles:: forged_world::"`, `just test-one world-store "--test it forged::"`, `just determinism-wasm` → PASS.
- [ ] **Step 5: Commit** `feat(worldgen): forged sampler, rivers, tiles and world map wrap east–west`.

### Task 12: Client frame, window and outgoing canonicalisation

**Files:**
- Create: `crates/client/src/wrap_frame.rs`
- Modify: `crates/client/src/instance.rs`: own a `WrapFrame`; `ServerPacket::WorldMap` → `set_extent`; every home change (`PlayerRegion`, the predicted flip in `route_transfers` ~1463) → `follow_home`; `desired_window` ~804 and `desired_lod` ~835 compute around `frame.alias(home)` in unwrapped space, then subscribe to the de-duplicated `canonical` regions while LOD keys stay unwrapped; `route_transfers` rebases with `rebase_isometry_by(.., region_delta(..))`; `viewer_world_xz` ~1611 uses `frame.offset(home)`; the frame is pushed to the render world on change (`ClientUpdateEvent::WrapFrame(WrapFrame)` → resource)
- Modify: `crates/client/src/renderer/commander/move_order.rs` (`goal_from_hover` ~84, `goal_from_tile` ~91), `designate.rs`, `selection.rs` (reverse lookups through `region_at_world`), `crates/client/src/session.rs` (register the resource for session reset)
- Modify: the client's world-entry path: refuse a wrapping world narrower than `MIN_WRAP_WIDTH_M` with the message `"this world is too narrow to walk around (… m; needs 8192 m)"`
- Test: unit tests in `wrap_frame.rs`; `instance.rs` window tests

**Interfaces:**
- Consumes: Tasks 5–6.
- Produces:
  - `#[derive(Resource, Clone, Copy, Default)] pub struct WrapFrame { pub extent: Option<MapExtent>, pub anchor: RegionCoords }`
  - `pub fn alias(&self, rc: RegionCoords) -> RegionCoords` (the copy of canonical `rc` nearest `anchor`), `pub fn offset(&self, rc: RegionCoords) -> [f32; 3]` (`alias(rc).world_offset()`), `pub fn canonical(&self, rc: RegionCoords) -> RegionCoords`, `pub fn region_at_world(&self, x: f32, z: f32) -> RegionCoords` (canonical owner of a render-space point), `pub fn follow_home(&mut self, home: RegionCoords)` (anchor becomes the alias of `home` nearest the old anchor, so the camera never jumps), `pub fn set_extent(&mut self, extent: Option<MapExtent>)`
  - `pub const MIN_WRAP_WIDTH_M: f64 = 8192.0` in `worldgen::forge::play` (the window plus the LOD horizon must never see one region twice).
- No lap re-centring: after two laps of a 200 km world f32 precision is still centimetres. Recorded under Known limits.

- [ ] **Step 1: Write the failing tests:**
  - `wrap_frame::alias_nearest_anchor`: extent origin 0, width 10, wrapped, anchor (9, 0): `alias((0, 0)) == (10, 0)`, `alias((8, 0)) == (8, 0)`, `canonical((10, 0)) == (0, 0)`.
  - `wrap_frame::follow_home_is_continuous`: anchor (9, 0), `follow_home((0, 0))` → anchor (10, 0); then `follow_home((9, 0))` → (9, 0).
  - `wrap_frame::identity_without_wrap`: no extent → `alias` and `canonical` are the identity and `offset == world_offset`.
  - `instance::window_across_seam`: home (9, 0) on a width-10 wrap, radius 1 → the subscribed set is the 9 canonical regions with x ∈ {8, 9, 0}; no x = 10 or −1.
  - `move_order::goal_from_tile_across_seam`: a hovered tile in alias region (10, 0) gives `Goal.region == (0, 0)`.
- [ ] **Step 2: Run** `just test-one client "--lib wrap_frame::"` (ask the user first) → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one client "--lib wrap_frame:: instance:: move_order::"`, `just smoke` → PASS.
- [ ] **Step 5: Commit** `feat(client): wrap frame — canonical regions, unwrapped view`.

### Task 13: Render positions through the frame

**Files:** replace `rc.world_offset()` used for RENDER placement with `frame.offset(rc)`, and `RegionCoords::from_world` on render-space points with `frame.region_at_world`, in:
- `crates/client/src/renderer/bridge.rs` (region root transforms; `dedupe_halo_entities` ~1360 stays keyed by canonical ids)
- `renderer/collider_debug.rs`, `grid_debug.rs`, `slice_plane.rs`, `boundaries.rs`, `fortress_view.rs`, `item_flights.rs`
- `renderer/commander/`: `mod.rs` (`region_rect` ~241), `bounds.rs`, `selection.rs`, `move_order.rs`, `visible.rs`, `height.rs`, `cut_entities.rs`, `adventure.rs`, `level.rs`, `designate.rs`, `first_person.rs`
- `renderer/lod_rings.rs` keeps `key.world_offset()` (keys are already unwrapped); its `covered.contains(rc)` compares canonical ids via `frame.canonical`
- `renderer/slint_overlay.rs` world-map helpers: `map_tile` ~1056 goes through `WorldMap::tile`'s canonicalisation; `play_action` ~304 compares canonical ids
- `crates/client/src/regions/runner.rs`, `autopilot.rs`, `local_server.rs`, `dev_ui.rs`, `chat/run.rs`: each `RegionCoords::new(… ± 1 …)` and `rc.x ± …` is decided as below
- Test: `renderer/bridge.rs` tests

- [ ] **Step 1: Write the failing test** `bridge::region_root_uses_alias_offset`: with a frame anchored at (10, 0) on a width-10 wrap, the root of region (0, 0) is placed at `10 · REGION_SIZE` in x.
- [ ] **Step 2: Run** `just test-one client "--lib bridge::region_root"` (ask first) → FAIL.
- [ ] **Step 3: Implement.** For each hit of `grep -rn "world_offset()\|from_world(\|RegionCoords::new(" crates/client/src`: render placement → the frame; a neighbour for a subscription or a packet → `region_neighbour` / `canonical`; sim-local math inside one region → unchanged.
- [ ] **Step 4: Run** the test, `just smoke` → PASS.
- [ ] **Step 5: Commit** `feat(client): render regions at their wrap-frame alias`.

### Task 14: Wrapped worlds are refused under split roles

**Files:**
- Modify: `crates/server/src/roles.rs` (new check beside `check_several_hosts` ~92; `run_coordinator` ~459 after it opens the world ~467), `crates/server/src/lib.rs` (call it after `open_world` ~710, before `install_world` ~900)
- Test: `crates/server/src/roles.rs` unit tests

**Interfaces:**
- Produces: `pub fn check_wrap_single_process(role: Role, hosts: u32, wrap_x: bool) -> Result<(), String>` — `Err` when `wrap_x && (role != Role::All || hosts > 1)`, with the text `"this world wraps east–west, which only a single process supports yet: run with VOXEL_ROLE=all and without VOXEL_HOSTS (the seam between hosts is not built)"`. The process exits non-zero before any region starts and before any store write.

- [ ] **Step 1: Write the failing tests** `roles::wrapped_world_needs_one_process`: `(All, 1, true)` is `Ok`; `(Host, 1, true)`, `(Coordinator, 1, true)` and `(Host, 3, true)` are `Err` containing `single process`; every combination with `wrap_x == false` is `Ok`.
- [ ] **Step 2: Run** `just test-one server "--lib roles::wrapped"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one server "--lib roles::"`, `just smoke` → PASS.
- [ ] **Step 5: Commit** `feat(server): a wrapping world is refused under split roles or several hosts`.

### Task 15: One generator — delete the old layouts, presets and knobs

**Files:**
- Modify: `crates/worldgen/src/forge/knobs.rs`: delete the `layout`, `pole`, `latitude_span_deg`, `latitude_center_deg` knobs, the whole "Island" group, `sea_edges`, `continent_coast`, `inland_sea_size`, and the island-only "Uplift" knobs `belt_width`, `belt_scale`, `inland_gate` (confirm each by grep: a knob with no reader left after the island body is gone is deleted, one still read is kept); delete `CHOICES`' `layout` and `pole` entries, `LAYOUT_*`, `layout()`, `is_continental()`, `is_planet()`, `sea_edge_count()`, `pole_choice()`. `ocean_margin_start`, `coast_noise_lo`, `coast_noise_hi` move to the "Continent" group, renamed "Land".
- Modify: `forge/world.rs`: delete the island body of `World::new`, `layout_blobs`, `landmass`, `Blob`, `Landmass`, the island constants, `ContinentalMass::{Continent, Inland}`, the seed-picked branch of `edges()`, the land-edge taper and `follow_land_edges`' land-edge handling (a Planet has none), `LAND_EDGE_*`; `World::new` is what `new_continental` was. `ForgeParams::default()` becomes `DEV_WORLD_SIZE` at 32 m. Delete `ForgeParams::square`.
- Modify: `forge/climate.rs` (`latitudes`: one formula, 90° at row 0 to −90° at the last row; delete `POLE_*`), `forge/plates.rs` (`soften_along_land_edges` deleted), `forge/fields.rs`, `forge/quality.rs` (`land_band` has one band; `summarise` loses its `layout` argument), `forge/periodic.rs` (`for_world` always `Some`; the `None` code paths in the forge are deleted — `World.wrap_x` stays, for hand-built test grids)
- Modify: `forge/presets.rs`: `PRESETS` = "Earth-like" (no overrides, first), "Great ranges", "Highlands", "Lowlands", "Dry", "Wet" (each with any deleted-knob override removed)
- Modify: `crates/client/src/frontend/generator.rs` (`TABS`: the "Land" tab's groups; the tests that index the `layout` row or name "Continent"), `crates/world-store/src/ctl.rs` (`USAGE`), `crates/worldgen/examples/forge_proto/{main,search,serve}.rs`, `crates/client/src/debug_launch.rs`, `crates/server/src/{lib,roles,world_file}.rs` (they use `ForgeParams::default()` — verify the dev size works at region 64 and 256)
- Delete tests: `island_layout_unchanged`, `continent_has_requested_sea_edges`, `inland_has_no_sea_edges`, `land_edge_drains_out`, `land_edges_drain_across_without_a_lip_or_ridge`, `island_presets_keep_todays_look`, `continental_presets_are_continental`, `island_layout_has_a_live_land_term`, `unwrapped_*` forge tests from Tasks 9–10. Port to a Planet: `belts_follow_plate_boundaries`, `erosion_below_the_column_is_granite`, `volcano_sites_ignore_the_cell_size`, `default_forge_reaches_kilometre_peaks`, `forged_fields_follow_the_terrain`, `default_preset_has_no_overrides` (pins the six names above), every test that set a `layout`.
- Replace: `crates/worldgen/tests/fixtures/forge_quality_16k_32m.ron` → `forge_quality_planet_dev_32m.ron`, recorded from a `DEV_WORLD_SIZE` / 32 m / `DEFAULT_SEED` Planet; `forge_quality_matches_baseline` and `quality.rs`'s `include_str!` point at it.
- Modify: `justfile` (comments on `forge-proto`, `forge-ui`, `run`, `play`, `server`; drop `--set layout=0`), `CLAUDE.md` (the Worldgen and Building paragraphs: one layout, sizes, the wrap and canonical-coordinates rule, single-process limit), `TODO.md` (the east–west wrap and world-edge items; re-forge note for the public web world and dev Postgres worlds)
- Test: `crates/worldgen/src/forge/presets.rs`, `knobs.rs`, the ported tests

**Interfaces:**
- Consumes: Tasks 1–14 all green.
- Stored knobs of a deleted name are ignored on load (`WorldGlobals::knobs()` already ignores unknown names), so a world forged between Task 2 and now still opens.

- [ ] **Step 1: Write the failing tests:**
  - `knobs::no_layout_or_pole_knob`: `SPECS` has no knob named `layout`, `pole`, `latitude_span_deg`, `latitude_center_deg`, `sea_edges`, `island_extent`.
  - `presets::earth_like_is_first_and_default`: `PRESETS[0].name == "Earth-like"` with no overrides; `PRESETS.len() == 6`; every override names an existing knob in range.
  - `world::default_world_is_a_planet`: `World::new(&ForgeParams::default())` has `nx == 512`, `nz == 256`, `wrap_x`, and `edges == [Sea, Wrap, Sea, Wrap]`.
- [ ] **Step 2: Run** `just test-one worldgen "--lib forge::knobs:: forge::presets::"` → FAIL.
- [ ] **Step 3: Implement** the deletions; record the new quality fixture with the command the old fixture's header names (keep that header current).
- [ ] **Step 4: Run** the whole non-client suite of each touched crate once (say so first; this is the wide change): `just test-one worldgen "--lib"`, `just test-one worldgen "--test it -- --include-ignored"`, `just test-one world-store "--test it"`, `just test-one server "--test it"`, `just test-one game "--test it"`, `just test-server`, `just smoke`, `just determinism-wasm`, `just clippy` on the touched files, `just slint-wasm` only if a `.slint` file changed.
- [ ] **Step 5: Commit** `feat(forge)!: the Planet is the only world — old layouts, presets and their knobs removed`.

### Task 16: Climate lengths in degrees of latitude

**Files:**
- Modify: `crates/worldgen/src/forge/knobs.rs`, `forge/climate.rs`, `forge/presets.rs` ("Dry" / "Wet" overrides)
- Test: `forge/climate.rs` tests

**Interfaces:**
- Consumes: Task 15 (no old layout reads these knobs any more).
- Produces: knobs `land_depletion_deg: 16.0` (range 3–100), `sea_recharge_deg: 9.0` (1–36), `meander_wavelength_deg: 72.0` (9–180) replace `land_depletion_m`, `sea_recharge_m`, `meander_wavelength_km`. `pub fn metres_per_degree(height_m: f64) -> f64` = `height_m / 180.0`; each is converted with it at use. The defaults reproduce today's 9 km, 5 km and 40 km on a Large world (558 m per degree). `OROGRAPHIC_RUN_M`, `TERRAIN_BLUR_M`, `RAIN_CROSS_BLUR_M`, `RAIN_ALONG_BLUR_M` and `orographic_scale_m` stay in metres: they describe hills, not the planet.

- [ ] **Step 1: Write the failing test** `climate::rain_pattern_scales_with_the_world`: an idealised wrapped world (one meridional mountain wall, sea elsewhere) at 16,384 × 8,192 / 64 m and at 32,768 × 16,384 / 128 m (same grid, twice the metres): the precipitation rasters agree cell for cell within 5% away from the wall's ± 3 columns.
- [ ] **Step 2: Run** `just test-one worldgen "--lib forge::climate::rain_pattern"` → FAIL (today the larger world's interior is relatively wetter).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one worldgen "--lib forge::climate::"`; re-record the quality fixture (the climate changed on the dev size) and run `just test-one worldgen "--test it -- --include-ignored forge_lib::"` → PASS.
- [ ] **Step 5: Commit** `feat(forge): climate relaxation and meander lengths in degrees of latitude`.

### Task 17: Measure every size; live check

**Files:**
- Modify: `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` ("Measurements": a "Phase 1a" table), `crates/worldgen/src/forge/play.rs` (`FORGE_SECS_PER_MCELL`, `FORGE_COST_EXPONENT`, `FORGE_RAM_*` refitted to the new numbers, with the date and machine in the comment)

- [ ] **Step 1: Measure** (release; the user has approved timing runs for this work — say which are starting): `just forge-bench 16384 8192 32`, then each of Tiny, Small, Medium at 16 m and at 32 m, and Large at 32 m. Large at 16 m only if `free -g` shows more than 20 GB available; otherwise record "not run: needs about 14 GB". For each: wall seconds, max RSS, `.world` file size.
- [ ] **Step 2: Record** the table in the design document, beside the Phase 0 rows, with the machine (cores, RAM) and `scaling_cur_freq` noted. No pass/fail line: the user judges.
- [ ] **Step 3: Refit** the estimate constants; `play::estimate_counts_cells_once` still passes.
- [ ] **Step 4: Live check** (ask the user before launching; they may prefer to do it): `just run --reset`; open the world map — poles white, a hot band across the middle, the left and right columns matching. Dev Play a tile in the map's last column, pan east across the seam: terrain continuous, no gap or wall in the LOD rings. In adventure mode `MoveTo` a tile across the seam and walk across it. Expected in `worlds/debug-embarked/server.log`: no `departure … into off-map` error, no `DESYNC` line.
- [ ] **Step 5: Commit** `docs: Phase 1a measurements; forge estimate refitted`.

## Known limits (not in this plan)

- One process only. The seam between hosts (peer links, ownership, placement, observer sessions) is design phase "1c", after the R3 go/no-go.
- No lap re-centring of the client frame: fine for many laps of the largest world.
- The world-map window does not scroll around the seam; it shows the canonical rectangle.
- A voxel body that straddles the seam (a felled tree) keeps its existing in-region rule.
- The Planet's land is still today's "continents" formula and annual climate. Plates, monthly climate, water features and biomes are Phases 2–5.
