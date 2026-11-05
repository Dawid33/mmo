# East–West World Wrap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A forged world can be a cylinder: walking (or flying, or panning) off the east edge enters the west edge seamlessly, and vice versa. North and south stay real edges.

**Architecture:** One rule carries the whole engine: **every `RegionCoords` that is stored, simulated, hashed or sent is canonical** (x folded into the map's `[origin.x, origin.x + width)`); unwrapped coordinates exist only transiently, inside the client's render frame and window math. `MapExtent` gains the wrap and the three helpers everything else calls (`canonical`, `neighbour`, `delta`). The forge produces terrain periodic in x (wrap-aware grid indexing, variance-preserving seam cross-fade for noise, periodic images for Voronoi/segment geometry), and the runtime sampler folds x modulo the world size, so region `r` and region `r + width` generate identical terrain.

**Tech Stack:** Rust; crates `game`, `server`, `worldgen` (forge + `forged.rs`), `client` (Bevy). No new dependencies.

**Spec:** this plan's Design section below (from the 2026-11-05 feasibility discussion; no separate spec doc). Code facts were gathered 2026-11-05 — line numbers are approximate; search by the named symbol.

## Design (decisions the tasks rely on)

- **D1 Cylinder, x only.** Latitude drives climate, so z never wraps.
- **D2 Flag.** `EdgeKind::Wrap` (new variant, appended — old v7 files still decode, so `ARTIFACT_VERSION` stays 7 and no world needs re-forging). A wrapped world has `edges[1] == edges[3] == Wrap` (east/west); north/south are Sea or Land as today. Knob `wrap` (choice `Off`/`On`, default Off) in `knobs.rs`. `WorldMap` and `MapExtent` gain `wrap_x: bool`.
- **D3 Size.** A wrapped world's `size_m` is snapped to a multiple of `WRAP_SIZE_QUANTUM_M = 512.0` (the largest `REGION_BLOCKS` choice) AND of the coarsest forge cell (`cell_m * 2^coarse_levels`): `forge::world::wrap_size_m(size_m, coarsest_cell_m) -> f64` rounds to the nearest common multiple (lcm), never below it. 100,352 / 32,000 / 64,000 m are already multiples of 512; 16,000 snaps to 15,872. So `width = size_m / region_m` is exact for every region size and no region straddles a partial tile. `MIN_WRAP_WIDTH_M = 8192.0`: the forge refuses wrap below it, so the client's window + 2 km LOD horizon never sees one region twice.
- **D4 Canonical everywhere.** Server canonicalises every inbound `RegionCoords` at the manager's packet edge; region sims (server AND client-predicted) use canonical ids, so rollback state and state hashes match. Region store files, worldgen cache files, history records: canonical.
- **D5 Crossing math.** Never subtract canonical coords or `world_offset`s across the seam: use `MapExtent::delta` (shortest signed). `rebase_isometry` gets a delta-taking sibling.
- **D6 LOD keys stay unwrapped.** Tile keys span several regions and `width` need not divide by the tile span, so the client sends keys in its unwrapped frame; the server builds them as requested (coarse terrain from the x-folding sampler, overlays looked up by `canonical(r)` but positioned by the unwrapped `r`) and replies with the same key. Invalidation matches subscribed keys whose regions canonicalise to the changed region.
- **D7 Client frame.** `WrapFrame { extent: Option<MapExtent>, anchor: RegionCoords }`: `anchor` is the unwrapped alias of the home region and moves continuously (`follow_home` picks the alias of the new home nearest the old anchor), so the camera never jumps. `alias(rc)` = the copy of canonical `rc` nearest `anchor`; `offset(rc) = alias(rc).world_offset()`. No lap re-centring: f32 precision after two laps of a 100 km world is ~1.5 cm (ulp at 3.2 M units = 0.25 units); recorded as a known limit.
- **D8 Periodic noise.** `PeriodicX { period_m, band_m }`, `sample(x, z, f) = cos(θ)·f(x, z) + sin(θ)·f(x − period, z)` for `x` in the last `band_m` before the period, `θ = π/2 · smoothstep(t)`, else `f(x, z)` — `cos² + sin² = 1` keeps the noise's variance, smoothstep keeps the slope continuous. `band_m = min(period / 4, 4 × the noise's longest wavelength)`. Applied to x folded into `[0, period)`.
- **D9 Periodic geometry.** Voronoi sites, belt/range segments, continent centres, volcano and lake sites: add images at `x ± period` before nearest-site / segment-distance queries, then query with folded x. Geology's linear x dip is dropped for wrapped worlds (z dip only). Climate meander waves get an integer number of x-cycles per period.

## Global Constraints

- A world with `wrap` Off must forge **bit-identically** to today (`island_layout_unchanged` crc pin, `forge_quality_16k_32m.ron` fixture, wasm determinism goldens all unchanged).
- `game` and `server` stay Bevy-free; wire format stays `game::encode_packet`/`decode_packet`.
- Hash with `StateHash` (derive on any new state type); `BTreeMap` not `HashMap` in sim/history.
- Forge bench (`just forge-bench`) must not slow down > 5% with wrap Off.
- Tests: `just test-one <crate> "<target> <filter>"`, `just smoke`; `just test-threads` after Task 2; `just determinism-wasm` after Tasks 2 and 5–7. Tag tests > 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- Never write the region edge as a literal; derive from `REGION_SIZE` / `REGION_BLOCKS`.
- The working tree has the user's unrelated uncommitted changes: each commit `git add`s only the task's files.

## Review Focus

1. **Two views of the seam at once** — a region at `origin.x + width − 1` ghosting into `origin.x`: ghost/arrival identity (`source_region`) must be canonical on both client and server or `dedupe_ghosts` and `matches_prediction` miss. Test in Task 2 (`ghost_across_seam_keeps_canonical_source`).
2. **Non-wrapped worlds unchanged** — every new helper must be the identity / plain subtraction with `wrap_x == false` or no map. Tests in Tasks 1, 5 (`unwrapped_world_forges_identically`).
3. **Unwrapped coordinates leaking to the server** — a commander click past the seam produces `RegionCoords::from_world` outside the map; the `MoveTo` goal / `PlayRegion` / region request must arrive canonical, and the server must canonicalise anyway. Tests in Task 2 (`manager_canonicalises_inbound_regions`) and Task 8 (`goal_from_tile_across_seam`).
4. **Rivers and lakes across the seam** — a reach whose cells straddle x = 0 must carve continuously, not draw a segment across the whole map. Test in Task 7 (`river_crosses_the_seam_continuously`).
5. **Tiny wrapped worlds** — a width that lets the window + LOD rings reach the same region from both sides. Refused at forge time. Test in Task 5 (`wrap_refused_below_min_width`).

---

### Task 1: `MapExtent` wrap API

**Files:**
- Modify: `crates/game/src/world_map.rs` (`WorldMap`, `MapExtent`, `off_map`)
- Modify: every `MapExtent { .. }` / `WorldMap { .. }` literal (`grep -rn "MapExtent {\|WorldMap {" crates`), adding `wrap_x: false`
- Test: `crates/game/tests/world_wrap.rs` (new; add `mod world_wrap;` to `crates/game/tests/it.rs`)

**Interfaces:**
- Produces (in `world_map.rs`, re-exported at the crate root like `off_map`):
  - `pub struct MapExtent { pub origin, pub width, pub height, pub wrap_x: bool }`
  - `impl MapExtent { pub fn canonical(&self, rc: RegionCoords) -> RegionCoords; pub fn contains(&self, rc) -> bool /* wrap: z range only */; pub fn neighbour(&self, rc, dx: i32, dz: i32) -> Option<RegionCoords> /* canonical, None off-map */; pub fn delta(&self, from, to) -> (i32, i32) /* x shortest signed when wrap, ties → positive */ }`
  - Free fns over `Option<MapExtent>` (no map = unbounded plane): `pub fn canonical(map, rc) -> RegionCoords`, `pub fn region_neighbour(map, rc, dx, dz) -> Option<RegionCoords>`, `pub fn region_delta(map, from, to) -> (i32, i32)`.
  - `WorldMap { .., pub wrap_x: bool }`; `extent()` copies it; `tile(rc)` canonicalises first.

- [ ] **Step 1: Write the failing tests** in `world_wrap.rs`, with `let m = MapExtent { origin: RegionCoords::new(-3, -2), width: 10, height: 4, wrap_x: true }`:
  - `canonical_folds_x_only`: `m.canonical((7, 0)) == (-3, 0)`, `m.canonical((-4, 1)) == (6, 1)`, `m.canonical((2, 9)) == (2, 9)` (z untouched).
  - `contains_ignores_x_when_wrapped`: `contains((100, 0))`, `!contains((0, 2))`, `!contains((0, -3))`.
  - `neighbour_wraps_east_and_west`: `neighbour((6, 0), 1, 0) == Some((-3, 0))`, `neighbour((-3, 0), -1, 0) == Some((6, 0))`, `neighbour((0, 1), 0, 1) == None`.
  - `delta_is_shortest`: `delta((6, 0), (-3, 0)) == (1, 0)`, `delta((-3, 0), (6, 0)) == (-1, 0)`, `delta((-3, 0), (2, 0)) == (5, 0)` (tie → positive).
  - `unwrapped_extent_is_plain`: same calls with `wrap_x: false` give plain subtraction / `None` past x; `region_neighbour(None, (5, 5), 1, 0) == Some((6, 5))`.
  - `world_map_tile_canonicalises`: a 10×4 wrapped `WorldMap` returns the same tile for `(7, 0)` and `(-3, 0)`.
- [ ] **Step 2: Run** `just test-one game "--test it world_wrap::"` — expect compile failure (no `wrap_x`).
- [ ] **Step 3: Implement** the interfaces above; `wrap_x: false` in every existing literal.
- [ ] **Step 4: Run** `just test-one game "--test it world_wrap::"` → PASS; `just smoke` → PASS.
- [ ] **Step 5: Commit** `feat(game): MapExtent east–west wrap helpers`.

### Task 2: Sim crossings, relays and inbound canonicalisation

**Files:**
- Modify: `crates/game/src/protocol.rs` (add `rebase_isometry_by`)
- Modify: `crates/game/src/state.rs` (`scan_boundaries` ~1562: `closed` and leaver/ghost targets via `region_neighbour`)
- Modify: `crates/game/src/world_manager.rs` (departure relay ~925–984: rebase by `region_delta(self.map_extent(), rc, target)`; inbound packet handling ~752–815 canonicalises `RequestRegionConnection`, `ReleaseRegionConnection`, `PlayRegion`, `Embark.region`, `GameEvent.region_id` before the `== home` check, and `Goal.region` inside `Order::MoveTo`; `play_region`/`landing_in` take canonical)
- Modify: `crates/game/src/adventure.rs` (`region_step` ~353 uses `region_delta`; `plan_walk` exit; `edge_toward`)
- Modify: `crates/game/src/people.rs` (~103 `SpawnAt::Edge`, ~172 `walk_out_safe`: deltas via `region_delta`; targets via `region_neighbour`)
- Test: `crates/game/tests/world_wrap.rs`

**Interfaces:**
- Consumes: Task 1.
- Produces: `pub fn rebase_isometry_by(iso: &Pose, delta: (i32, i32)) -> Pose` (translation −= delta·`REGION_SIZE`). `rebase_isometry(iso, from, to)` stays as `rebase_isometry_by(iso, (to.x − from.x, to.z − from.z))` for unbounded callers; every caller holding a map uses `rebase_isometry_by(iso, region_delta(map, from, to))`.

- [ ] **Step 1: Write the failing tests** (an `InlineSpawner` `WorldManager` with a procedural generator and a hand-built wrapped `WorldMap` of width 4, origin (0, 0), installed by `set_world_map`; reuse the helpers in `tests/play_region.rs` / `tests/adventure.rs`):
  - `body_crosses_east_seam_into_origin`: a client in region (3, 0) whose body is placed at `x = REGION_SIZE − 0.5` moving +x; after the crossing its home is (0, 0) and its region-local x is in `[0, 4)` units — not ~`3·REGION_SIZE`.
  - `body_crosses_west_seam`: mirror from (0, 0) → (3, 0).
  - `north_edge_still_holds`: a body pushing −z in row 0 stays in (x, 0), clamped at the seam (existing `body_is_held_at_the_outer_seam` behaviour).
  - `ghost_across_seam_keeps_canonical_source`: a body within `GHOST_MARGIN` of (3, 0)'s east edge produces a ghost in (0, 0) with `source_region == (3, 0)`.
  - `walker_goal_across_seam`: `Order::MoveTo(Goal { region: (0, 0), .. })` from region (3, 0) walks out the EAST edge (exit `[1, 0]`), not west.
  - `manager_canonicalises_inbound_regions`: `ClientPacket::PlayRegion((4, 0))` moves the client to (0, 0); `RequestRegionConnection((−1, 0))` subscribes (3, 0).
- [ ] **Step 2: Run** `just test-one game "--test it world_wrap::"` → the new tests FAIL.
- [ ] **Step 3: Implement** the file changes above.
- [ ] **Step 4: Run** `just test-one game "--test it world_wrap::"`, `just test-one game "--test it -- --include-ignored adventure::"`, `just test-one game "--test it play_region::"`, `just test-threads`, `just determinism-wasm` → all PASS.
- [ ] **Step 5: Commit** `feat(game): region crossings wrap across the east–west seam`.

### Task 3: Server LOD across the seam

**Files:**
- Modify: `crates/game/src/world_manager.rs` (`start_lod_build` ~1344: source region = `canonical(map, key.origin_region())`; `start_tile_build` ~1396: overlay terrain from `canonical(r)`, positioned by the unwrapped `r` of `key.regions()`; `invalidate_region_lod` ~1468: invalidate every subscribed key with any `key.regions()` canonicalising to `rc`; pristine/`generate` closures always receive canonical coords)
- Test: `crates/game/tests/world_wrap.rs`

**Interfaces:**
- Consumes: Task 1 `canonical`. `copy_region_cells_into_tile(region, key, ..)` unchanged — it is called with the unwrapped `r`.

- [ ] **Step 1: Write the failing tests:**
  - `lod_key_past_the_seam_builds_from_canonical_region`: width-4 wrapped map; a client's `RequestLod(LodKey::for_region(2, (4, 0)))` is answered with `LodData` for that same key whose cells equal the `LodData` of `LodKey::for_region(2, (0, 0))`.
  - `edit_invalidates_aliased_key`: a block edit in (0, 0) re-sends `LodData` for a subscribed key over (4, 0).
- [ ] **Step 2: Run** `just test-one game "--test it world_wrap::lod"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one game "--test it world_wrap::"` and `just test-one game "--test it lod_streaming::"` → PASS.
- [ ] **Step 5: Commit** `feat(game): LOD keys past the seam build from canonical regions`.

### Task 4: History on a cylinder

**Files:**
- Modify: `crates/game/src/history/grid.rs` (`SocietyGrid` gains `wrap_x` from `WorldMap`; `index` canonicalises; `label_components` 8-neighbours wrap; `route` A* neighbours wrap and its heuristic uses the wrapped |dx|; add `pub fn distance(&self, a, b) -> i64` (Chebyshev, wrapped) and `pub fn delta(&self, from, to) -> (i32, i32)`)
- Modify: `crates/game/src/history/chain.rs` and `history/bootstrap.rs` (delete both local `chebyshev`, use `grid.distance`; `embark_target`'s search loop canonicalises candidates)
- Modify: `crates/game/src/history/handover.rs` (`entry_side`), `history/journey.rs` (`leg_arrivals` diagonal test) — via `grid.delta`
- Modify: `history/persist.rs` only if the grid is serialized (it is rebuilt from the map on load — verify; then no change)
- Test: `crates/game/tests/history.rs`

- [ ] **Step 1: Write the failing tests** (a wrapped `WorldMap` of width 12 with land everywhere):
  - `route_crosses_the_seam`: `route(grid, (11, 0), (1, 0), ..)` is 3 regions long and passes (0, 0).
  - `landmass_joins_across_seam`: land columns at x = 0 and x = 11 only → one component.
  - `embark_too_close_across_seam`: a site at (11, 0) refuses an embark at (1, 0) with `TooClose` (`EMBARK_MIN_DISTANCE_REGIONS` = 4).
  - `unwrapped_grid_unchanged`: the same queries with `wrap_x: false` give today's answers (no route, two components, allowed).
- [ ] **Step 2: Run** `just test-one game "--test it history::"` → new tests FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one game "--test it history::"` and `just test-one game "--test it history_play::"` → PASS.
- [ ] **Step 5: Commit** `feat(history): routes, landmasses and site spacing wrap east–west`.

### Task 5: Forge grid wraps (indexing, hydrology, climate sweep)

**Files:**
- Modify: `crates/worldgen/src/forge/knobs.rs` (`wrap: 0.0, [0.0, 1.0]` in a `"World"`/existing group + `CHOICES` entry `("wrap", &["Off", "On"])`; `pub fn wraps(&self) -> bool`)
- Modify: `crates/worldgen/src/forge/world.rs`: `EdgeKind::Wrap` (appended); `edges()` sets E/W to `Wrap` when wrapping (sea-edge count then picks among N/S only); `World { .., pub wrap_x: bool }`; `pub fn wrap_size_m(size_m: f64, coarsest_cell_m: f64) -> f64`; `pub const WRAP_SIZE_QUANTUM_M: f64 = 512.0`; `pub const MIN_WRAP_WIDTH_M: f64 = 8192.0`; `World::neighbors` wraps x; new `pub fn step(&self, i: usize, dx: i32, dz: i32) -> Option<usize>` (the one wrap-aware offset helper every hand-rolled site switches to); `on_border` / outlet setup / the "every border cell is an outlet" asserts / `follow_land_edges` cover only non-wrapped edges; `upsample_onto` axis wraps x
- Modify: `forge/routing_fast.rs` (`Steps::at` wrap-aware when `wrap_x`; edge assert and shore check per non-wrapped edges), `forge/erosion.rs` (diffusion covers columns 0 and n−1 with x±1 mod n), flood fills in `forge/mod.rs` (`lake_bodies`), `forge/hydrology.rs` (`find_lakes`, `find_islands`), `forge/fields.rs` (`closed_lakes`, `ground_slope`, `nearest_water`), `forge/regions.rs` (`components`, `merge`), `habitat.rs` (`coast_distance_by`), `forge/post.rs`, `forge/spawn.rs` (edge clearance N/S only), `forge/quality.rs`, `forge/render.rs` — all via `World::step` or an equivalent `wrap_x` flag
- Modify: `forge/climate.rs`: `box_blur_rows` circular in x when wrapping; `orographic` x sweep runs one spin-up lap then the recorded lap (no edge inflow)
- Modify: the forge entry (`forge/mod.rs` `forge_with` / `ForgeParams` normalisation): when `wraps()`, snap `size_m` with `wrap_size_m` and refuse (`ForgeError`/panic consistent with existing parameter checks) below `MIN_WRAP_WIDTH_M`
- Test: `crates/worldgen/src/forge/world.rs` tests + `crates/worldgen/tests/forge_lib.rs`

Chamfer distances with wrap: repeat the forward+backward pass pair until no cell changes (converges in ≤ 2 extra pairs).

- [ ] **Step 1: Write the failing tests:**
  - `wrap_size_snaps`: `wrap_size_m(16_000.0, 128.0) == 15_872.0`, `wrap_size_m(100_352.0, 256.0) == 100_352.0`, `wrap_size_m(1_000.0, 64.0) == 512.0`.
  - `wrap_refused_below_min_width`: forging with `wrap` On at 4 km fails with the size message.
  - `neighbors_wrap_x`: on a wrapped 8×8 `World`, `neighbors(0)` yields exactly indices {1, 7, 8, 9, 15} (x−1 wraps to x = 7; no row −1); on the same grid unwrapped it yields {1, 8, 9}.
  - `wrapped_world_drains_across_the_seam` (slow): a 16 km / 64 m wrapped Continent forge; every non-ocean cell has a receiver path ending at an ocean or a N/S outlet (no cell drains "off" x).
  - `unwrapped_world_forges_identically`: existing `island_layout_unchanged` and the quality fixture still pass (no new code — re-run).
- [ ] **Step 2: Run** `just test-one worldgen "--lib forge::world::"` → new tests FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one worldgen "--lib forge::"`, `just test-one worldgen "--test it -- --include-ignored forge_lib::"` → PASS; `just forge-bench` with wrap Off within 5% of the pre-change run (record both numbers in the commit message).
- [ ] **Step 5: Commit** `feat(forge): wrap-aware grid, routing and climate sweep`.

### Task 6: Periodic noise and geometry in the forge

**Files:**
- Create: `crates/worldgen/src/forge/periodic.rs` — `pub struct PeriodicX { pub period_m: f64, pub band_m: f64 }`, `pub fn sample(&self, x: f64, z: f64, f: impl Fn(f64, f64) -> f64) -> f64` (D8), `pub fn fold(&self, x: f64) -> f64`, `pub fn dx(&self, a: f64, b: f64) -> f64` (shortest signed), `pub fn with_images<T: Clone>(&self, items: &[T], x_of: impl Fn(&T) -> f64, shift: impl Fn(&T, f64) -> T) -> Vec<T>`; `Option<PeriodicX>` is `None` for unwrapped worlds and every call site keeps its current code path then.
- Modify: `forge/world.rs` (shape/belt/rough/taper noises, `ContinentalMass` centres, island `layout_blobs`), `forge/plates.rs` (sites, warp noise, boundaries, `interior_ranges`), `forge/fields.rs` (`VolcanismNoise`, volcano spacing, `cone`), `forge/geology.rs` (no x dip when wrapping), `forge/regions.rs` (`savagery`), `forge/climate.rs` (`meander_deg`: each wave's x-frequency rounded to an integer number of cycles per period), `forge/post.rs` (lake-site spacing via `dx`)
- Test: `crates/worldgen/src/forge/periodic.rs` unit tests + `forge_lib.rs`

- [ ] **Step 1: Write the failing tests:**
  - `periodic_sample_is_continuous_at_the_seam`: for a `PerlinNoise` `f`, `|sample(period − ε, z) − sample(ε, z)| < 1e−3` with ε = 1e−4·period over 64 z values.
  - `periodic_sample_keeps_variance`: the sample variance of `sample` over the band is within 10% of `f`'s over the interior (10⁴ points).
  - `images_find_nearest_across_seam`: with sites at x = 1 and x = 50 (period 100), the nearest site to x = 99 is the x = 1 site at distance 2.
  - `wrapped_forge_has_no_seam_step` (slow): 16 km / 64 m wrapped Continent; the p99 of |h(n−1, z) − h(0, z)| is ≤ 1.5 × the p99 of interior column-to-column |Δh|.
- [ ] **Step 2: Run** `just test-one worldgen "--lib forge::periodic::"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one worldgen "--lib forge::"`, `just test-one worldgen "--test it -- --include-ignored forge_lib::"` → PASS (quality fixture unchanged).
- [ ] **Step 5: Commit** `feat(forge): periodic noise and geometry for wrapped worlds`.

### Task 7: Runtime sampler, rivers and the world map

**Files:**
- Modify: `crates/worldgen/src/forged.rs`: `to_artifact` folds x into `[0, size_m)` when `edges[1] == Wrap`; `raster_cell`, `nearest_cell`, `bilinear`, `bilinear_cells`, `bicubic`, `geo_offset_m`, `lake_level_at` wrap x; `place()` never reports E/W "past" for Wrap; `with_crc` buckets each river segment also shifted by ±`size_m` where the shifted copy overlaps the map; `patch` gathers segments/lakes for the window's folded x-intervals; runtime noises (`detail`, `dither`, `snow_patch`) go through `PeriodicX`
- Modify: `forge/hydrology.rs` `find_rivers`: consecutive reach points more than `size_m / 2` apart in x are unwrapped (±`size_m`) so a reach stays continuous
- Modify: `crates/worldgen/src/tile_map.rs` `build_world_map`: wrapped → `width = round(size_m / tile_m)` exactly and `wrap_x: true`; slope/extended-height reads wrap
- Modify: `crates/server/src/lib.rs` — nothing beyond what `build_world_map` returns (verify `set_world_map` carries `wrap_x`)
- Test: `crates/worldgen/tests/forged_wrap.rs` (new `[[test]]` if it pins the forged world via env — follow the existing forged tests' pattern; else a module in `tests/it.rs`)

- [ ] **Step 1: Write the failing tests** (one wrapped 16 km / 64 m world forged once per test binary, `#[ignore = "slow…"]`):
  - `aliased_regions_generate_identically`: `generate_region_forged(w, (o.x + W + 2, 1))` == `generate_region_forged(w, (o.x + 2, 1))` (column data equal), at `W = map.width`.
  - `seam_region_has_no_cliff`: surface heights of the last block column of region (o.x + W − 1, z) and the first of (o.x, z) differ by ≤ the max neighbouring-column difference inside either region, for 8 rows z.
  - `river_crosses_the_seam_continuously`: pick (or force via seed search in the test) a reach with points on both sides of x = 0; every carved water column along it is contiguous — no segment longer than 2 cells.
  - `coarse_tile_past_seam_matches`: `coarse_tile(LodKey for (o.x + W, z))` == for `(o.x, z)`.
  - `world_map_is_exactly_wrapped`: `map.wrap_x && map.width as f64 * tile_m == size_m`.
- [ ] **Step 2: Run** the target → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the target → PASS; `just test-one worldgen "--test it tile_map::"` → PASS; `just determinism-wasm` → PASS.
- [ ] **Step 5: Commit** `feat(worldgen): forged sampler, rivers and world map wrap east–west`.

### Task 8: Client frame, window and outgoing canonicalisation

**Files:**
- Create: `crates/client/src/wrap_frame.rs` — `#[derive(Resource, Clone, Copy, Default)] pub struct WrapFrame { pub extent: Option<MapExtent>, pub anchor: RegionCoords }` with `pub fn alias(&self, rc: RegionCoords) -> RegionCoords`, `pub fn offset(&self, rc: RegionCoords) -> [f32; 3]`, `pub fn canonical(&self, rc: RegionCoords) -> RegionCoords`, `pub fn region_at_world(&self, x: f32, z: f32) -> RegionCoords` (canonical owner of a render-space point), `pub fn follow_home(&mut self, home: RegionCoords)`, `pub fn set_extent(&mut self, extent: Option<MapExtent>)`
- Modify: `crates/client/src/instance.rs`: own a `WrapFrame`; `ServerPacket::WorldMap` → `set_extent`; every home change (`PlayerRegion` ~1532, predicted flip in `route_transfers` ~1139) → `follow_home`; `desired_window` and `desired_lod` compute around `frame.alias(...)` in unwrapped space, then `canonical` region subscriptions (dedupe) while LOD keys stay unwrapped (D6); `route_transfers` rebases with `rebase_isometry_by(.., region_delta(..))`; `viewer_world_xz` ~1223 uses `frame.offset(home)`; push the frame to the render world on change (new `ClientUpdateEvent::WrapFrame(WrapFrame)` → resource)
- Modify: `crates/client/src/renderer/commander/move_order.rs` (`goal_from_hover`/`goal_from_tile` canonical region; ~111 offset via `region_delta`), `designate.rs`, `selection.rs` (reverse lookups through `region_at_world`)
- Test: unit tests in `wrap_frame.rs`; `instance.rs` window tests

- [ ] **Step 1: Write the failing tests:**
  - `wrap_frame::alias_nearest_anchor`: extent origin 0 width 10 wrapped, anchor (9, 0): `alias((0, 0)) == (10, 0)`, `alias((8, 0)) == (8, 0)`; `canonical((10, 0)) == (0, 0)`.
  - `wrap_frame::follow_home_is_continuous`: anchor (9, 0), `follow_home((0, 0))` → anchor (10, 0); then `follow_home((9, 0))` → (9, 0).
  - `wrap_frame::identity_without_wrap`: no extent → `alias` / `canonical` identity, `offset == world_offset`.
  - `instance::window_across_seam`: home (9, 0) on a width-10 wrap, radius 1 → subscribed set `{(8,·), (9,·), (0,·)}` canonical, 9 regions, no x = 10 or −1.
  - `move_order::goal_from_tile_across_seam`: a hovered tile in alias region (10, 0) gives `Goal.region == (0, 0)`.
- [ ] **Step 2: Run** `just test-one client "--lib wrap_frame::"` (ask the user first — client unit tests are a slow opt-in) → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one client "--lib wrap_frame::"`, `just test-one client "--lib instance::"`, `just test-one client "--lib move_order::"` → PASS; `just smoke` → PASS.
- [ ] **Step 5: Commit** `feat(client): wrap frame — canonical regions, unwrapped view`.

### Task 9: Render positions through the frame

**Files:** replace `rc.world_offset()` used for RENDER placement with `frame.offset(rc)`, and `RegionCoords::from_world` on render-space points with `frame.region_at_world`, in:
- `renderer/bridge.rs` (~69 root transforms, ~948, ~1245; `dedupe_ghosts` stays keyed by canonical ids)
- `renderer/collider_debug.rs`, `grid_debug.rs`, `slice_plane.rs`, `boundaries.rs`, `fortress_view.rs`
- `renderer/commander/`: `mod.rs` (`region_rect`), `bounds.rs`, `selection.rs`, `move_order.rs`, `visible.rs`, `height.rs`, `cut_entities.rs`, `adventure.rs`, `level.rs`, `designate.rs`
- `renderer/lod_rings.rs` keeps `key.world_offset()` (keys are already unwrapped — D6); its `covered.contains(rc)` check compares canonical ids via `frame.canonical`
- `renderer/slint_overlay.rs` world-map helpers: `map_tile` goes through `WorldMap::tile`'s canonicalisation (Task 1); `play_action` compares canonical ids
- Test: compile + `just smoke`; one headless bridge test if an existing harness spawns region roots (`renderer/bridge.rs` tests) — `bridge::region_root_uses_alias_offset`: with a frame anchored at (10, 0) on width 10, the root of region (0, 0) is placed at `10 · REGION_SIZE`.

- [ ] **Step 1: Write the failing test** `bridge::region_root_uses_alias_offset` (above).
- [ ] **Step 2: Run** `just test-one client "--lib bridge::region_root"` → FAIL.
- [ ] **Step 3: Implement** the replacements (`grep -rn "world_offset()\|from_world(" crates/client/src` and decide each: render placement → frame; sim-local math → unchanged).
- [ ] **Step 4: Run** the test → PASS; `just smoke` → PASS; `just clippy` clean for touched files.
- [ ] **Step 5: Commit** `feat(client): render regions at their wrap-frame alias`.

### Task 10: Generator option, docs, live check

**Files:**
- Modify: `crates/client/src/frontend/generator.rs` — the knob rows come from `SPECS`/`CHOICES` automatically; show the snapped size (and the refusal below `MIN_WRAP_WIDTH_M`) beside the size picker when Wrap is On
- Modify: `CLAUDE.md` (forged-worlds paragraph: the `wrap` knob, `EdgeKind::Wrap`, the canonical-coordinates rule, D7's precision limit); `TODO.md` if it lists the world edge
- Modify: `justfile` only if a recipe is added (none planned: `just forge-proto --set wrap=1` covers it)

- [ ] **Step 1: Run** `just test-one slint-ui "--test it frontend::"` and `just smoke` → PASS after the generator change.
- [ ] **Step 2: Forge a wrapped world:** `just forge-proto --set wrap=1 --size 16000 --out worlds/wrap-test/terrain` → the summary prints size 15,872 m and E/W = Wrap; `just world-map --world worlds/wrap-test/terrain.world` → the PNG's left and right columns match by eye.
- [ ] **Step 3: Live check (ask the user before launching; `adapter=Intel`):** `just world_file=worlds/wrap-test/terrain.world run`, Dev Play a tile in the map's last column, pan east across the seam (terrain continuous, no gap or wall in the LOD rings), then in Adventure `MoveTo` a tile across the seam and F5-walk across it. Expected: no `departure … into off-map` error in the server log, no `DESYNC` lines.
- [ ] **Step 4: Commit** `docs: east–west wrapping worlds` (+ generator change).

## Known limits (not in this plan)

- No lap re-centring of the client frame (D7): fine for many laps of the largest world; revisit if someone circles it dozens of times.
- The world-map window does not scroll around the seam; it shows the canonical rectangle.
- Seam-crossing voxel bodies (felled trees) keep their interim in-region rule, unchanged.
