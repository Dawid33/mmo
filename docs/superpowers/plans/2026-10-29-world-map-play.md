# World Map + Play Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An Esc menu (non-pausing) and an M-key world map — one tile per region, drawn by the client from per-region summaries the server sends — with a side panel of tile facts and a **Play** button that moves the player's session to that region.

**Architecture:** `worldgen` computes a `game::WorldMap` (one `TileSummary` per region tile, incl. a precomputed dry landing column) straight from the forged artifact — never by generating a region. The server hands it to the `WorldManager`, which pushes it to each client on connect and handles `ClientPacket::PlayRegion` by sending two server-authored sim events: `ClientLeft` to the old home and `CreateClientAt` (with the landing column) to the target, then re-homing the client. The client rasterises the summaries into a Slint image and shows menu, map and panel through the existing `crates/slint-ui` overlay.

**Tech Stack:** Rust, `game` / `worldgen` / `server` (engine-free), Bevy 0.19 client, Slint + the `../ui` `widgets` library.

**Spec:** `docs/superpowers/specs/2026-10-29-world-map-play-design.md`

**Sequencing with the chunk-16 plan** (`docs/superpowers/plans/2026-10-29-chunk-16-blocks.md`, landing soon): run this plan **after** that one (at least after its Task 3), so the names here are the new ones. What it changes for us:
- The region dial becomes `REGION_BLOCKS` (`VOXEL_REGION_BLOCKS`, test 64 / play 256 blocks); `REGION_SIZE` stays (world units); `REGION_CHUNKS` becomes derived and means 16-block chunk columns. Everything here is in **blocks** or `REGION_SIZE` — never `REGION_CHUNKS`, never `CHUNK_*`. Tiles are still one per region (~64×64 at 256 m).
- `surface_voxel_y` / `surface_voxel` are rewritten on `Chunk::column_top` + `chunk_index` (its Task 2), so the single-column lookup Task 2 here uses gets cheaper; use whatever name it ends with.
- It edits `crates/worldgen/tests/forged_world.rs` (its Task 4) — Task 4 here moves helpers out of that file; do it on top of the chunk plan's version.
- It rewrites `renderer/bridge.rs` chunk handling and `commander/height.rs` / `tiles.rs` (its Tasks 9–10); this plan keeps its client state OUT of `bridge.rs` (Task 7) and does not touch the height/tile mirrors.
- It re-records `wasm_determinism` goldens; this plan must never re-record them (new event variants don't appear in the pinned scenes).
- If this plan runs first anyway: read `REGION_BLOCKS` as `REGION_SIZE as usize / BLOCK_VOXELS`, and rebase onto the chunk plan's dial rename when it lands.

## Global Constraints

- **Never commit Dawid's uncommitted work** (same rule as the chunk-16 plan): `git status` before every commit, `git add` only the files the task changed; if a file the task must touch is dirty with his hunks (e.g. `CLAUDE.md`, `justfile`, client/slint files), stop and ask.

- Work on `develop` in the main checkout; never create worktrees (CLAUDE.md).
- All player-facing UI is Slint using `@widgets` (`../ui`); no bevy_ui / egui for menu or map.
- The menu does not pause anything.
- The button text is **Play** (not Embark).
- Server sends data, never an image; the client builds the map.
- Building / sending the map never calls `generate_region`, `generate_cell`, a runner, a snapshot or a parked blob — only `ForgedWorld` raster/sample APIs.
- Never write the region edge as a literal: derive from `game::REGION_SIZE` / `UNITS_PER_METER` (tests run at 64 m regions, play at 256 m).
- `game` and `server` stay Bevy-free; wire framing only via `encode_packet`/`decode_packet`.
- Verify with `just test-one <crate> "<target filter>"` and `just smoke`; never whole suites. Client unit/integration tests are a slow opt-in — ask the user before running any.
- Tag any new test >100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- Thresholds/tunables are named `pub const`s.

## Review Focus

1. **Play pressed twice quickly / for the tile you're already moving to** — second request must be refused cleanly (target == current home after the first), never create a second body. Test in Task 3.
2. **Client predicted copy of the old region** — the client's old region must apply `ClientLeft` through reconcile without a mispredict loop or a crash when the local body vanishes (bridge `LocalBody` gone). Covered by the reconcile test in Task 2; live check in Task 10.
3. **Tile partly outside the artifact** (rim) — summaries treat outside cells as ocean (`OUTSIDE_HEIGHT_M`), landing never picks an outside column. Test in Task 4.
4. **Procedural world (no world file)** — no `WorldMap` pushed, Play refused with `NoWorldMap`, map window shows the "No world map (procedural world)" line. Tests in Tasks 3 and 8.
5. **Landing column inside a river channel** — raster cell dry but the carved river makes it wet: landing uses `ForgedWorld::sample` (which carves) to confirm dryness. Test in Task 4.

---

### Task 0: Update to the latest stable Rust

The toolchain is pinned in `rust-toolchain.toml` (currently `1.96.0`, with `wasm32-unknown-unknown`, rustfmt, clippy) and every build/test/wasm/rust-analyzer run uses the pin — so "update" means moving the pin, not just `rustup update`. CLAUDE.md: bump it deliberately, then `just smoke` + `just determinism-wasm`.

**Files:**
- Modify: `rust-toolchain.toml` (`channel`), `CLAUDE.md` ("currently 1.96.0" in Building and Running)

- [ ] **Step 1:** `rustup update stable` and note the version it reports (`rustup run stable rustc --version`) — that is the new pin `X.Y.Z`.
- [ ] **Step 2:** Set `channel = "X.Y.Z"` in `rust-toolchain.toml` (keep `targets`/`components` as they are); `rustup show` in the repo installs it with the wasm target and components.
- [ ] **Step 3:** `just build` → builds. Fix new compiler errors only; new *warnings* get fixed if trivial, otherwise listed in the commit message.
- [ ] **Step 4:** `just clippy` → note new lints; fix ones in first-party crates that are mechanical, leave vendored forks (`crates/rapier`, `crates/parry`, `crates/slotmapd`, `crates/block-mesh`) untouched.
- [ ] **Step 5:** `just smoke` → PASS.
- [ ] **Step 6:** `just determinism-wasm` → PASS (native and wasm32 hashes still equal). If it fails, STOP and report — a toolchain bump must not change sim results; do not re-record anything.
- [ ] **Step 7:** Update the version in CLAUDE.md; commit `build: bump pinned stable toolchain to X.Y.Z`.

---

### Task 1: Wire types for the map and the Play request

**Files:**
- Create: `crates/game/src/world_map.rs`
- Modify: `crates/game/src/lib.rs` (add `pub mod world_map;` + `pub use world_map::*;`), `crates/game/src/protocol.rs` (packet variants)
- Test: `crates/game/tests/packet_codec.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
  pub struct TileSummary {
      pub ground_min_m: f32, pub ground_mean_m: f32, pub ground_max_m: f32, // m rel. sea level
      pub relief_m: f32,          // p90 - p10 of ground over the tile
      pub slope: [f32; 2],        // mean d(ground)/dx, d/dz (m per m), for client hillshade
      pub sea_fraction: f32,      // 0..=1 of raster cells under sea
      pub lake_fraction: f32,     // 0..=1 of raster cells with a lake surface
      pub lake_level_m: Option<f32>,
      pub rivers: u16,            // river reaches with a point inside the tile
      pub river_max_discharge_m2: f32, pub river_max_width_m: f32, // 0 when rivers == 0
      pub surface_rock: Option<u8>,    // index into WorldMap::rock_names (mode over the tile)
      pub landing: Option<[u32; 2]>,   // region-local BLOCK column; None = no dry land
  }
  #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
  pub struct WorldMap {
      pub origin: RegionCoords,  // region of tiles[0] (min x, min z)
      pub width: u32, pub height: u32, // tiles, row-major by z then x
      pub tiles: Vec<TileSummary>,
      pub rock_names: Vec<String>,
  }
  impl WorldMap {
      pub fn tile(&self, rc: RegionCoords) -> Option<&TileSummary>;
      pub fn coords(&self, index: usize) -> RegionCoords;
  }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
  pub enum PlayRefusal { NoWorldMap, OffMap, NoDryLand, AlreadyHere }
  ```
  - `ClientPacket::PlayRegion(RegionCoords)`
  - `ServerPacket::WorldMap(WorldMap)` and `ServerPacket::PlayRefused(RegionCoords, PlayRefusal)`

- [ ] **Step 1: Write failing tests** in `tests/packet_codec.rs`: `world_map_packet_round_trips` (a 2×1 `WorldMap` with one `landing: Some([3, 4])` and one `None`, through `encode_packet`/`decode_packet`, equal after), `play_region_packets_round_trip` (`ClientPacket::PlayRegion(RegionCoords::new(-2, 5))`, `ServerPacket::PlayRefused(.., PlayRefusal::NoDryLand)`), and `world_map_tile_lookup` (`origin (-1,-1)`, width 3, height 2: `tile((1,0))` is `tiles[5]`, `tile((2,0))` is `None`, `coords(4) == (0,0)`).
- [ ] **Step 2:** `just test-one game "--test packet_codec"` → FAIL (types missing).
- [ ] **Step 3:** Implement the types and packet variants. Fix every exhaustive `match` on `ClientPacket`/`ServerPacket` the compiler flags: manager (`world_manager.rs`) — `PlayRegion` → `todo!()`-free placeholder that logs and drops (Task 3 fills it); client `instance.rs` — log and ignore for now (Task 7 fills it).
- [ ] **Step 4:** `just test-one game "--test packet_codec"` → PASS; `just smoke` → PASS.
- [ ] **Step 5:** Commit `feat(game): world-map tile summaries and Play packets`.

---

### Task 2: Server-authored leave / create-at sim events

**Files:**
- Modify: `crates/game/src/protocol.rs` (`GameEventKind`), `crates/game/src/region.rs` (apply + `ev_desc`), `crates/game/src/state.rs` (`create_player_at_safe`), `crates/game/src/region_runner.rs` (`ensure_required`), `crates/game/src/world_manager.rs` (client-injection drop list), `crates/client/src/instance.rs` (ignore locally-originated)
- Test: `crates/game/tests/play_region.rs` (new)

**Interfaces:**
- Produces:
  - `GameEventKind::ClientLeft(ClientId)` — removes the client's player entity AND its `GameData.clients` entry, undoably. `origin_client` → `None`.
  - `GameEventKind::CreateClientAt(ClientId, [u32; 2])` — like `CreateClient` (seeded `Client`), body placed over region-local block column `[bx, bz]`. `origin_client` → `Some(id)`.
  - `impl Rollback { pub fn create_player_at_safe(&mut self, client_id: ClientId, xz: (f32, f32)) }` — `xz` in voxel units; `create_player_safe(c)` becomes `create_player_at_safe(c, self.spawn_xz())`, keeping its existing single-column surface lookup (`surface_voxel_y`, rewritten on `Chunk::column_top` by the chunk-16 plan) + clearance. Block column → voxel: `(bx as f32 + 0.5) * BLOCK_VOXELS as f32` (the voxels-per-block constant, not 16).
- `ClientLeft` reuses `extract_entity_safe(key, region)` and discards the bundle (it already removes the entity, `player_entites` and `clients`); a missing player is a no-op with a `warn!`.
- Runner: `CreateClientAt(_, col)` calls `ensure_required` at that column's voxel xz (not `spawn_xz()`).
- Manager: client-sent `ClientLeft` / `CreateClientAt` (and, while there, `CreateClient`) join the "manager-internal, drop" arm.
- Client `instance.rs` `handle_game_event`: locally-originated `ClientLeft`/`CreateClientAt` are ignored like `CreateClient`.

- [ ] **Step 1: Write failing tests** in `tests/play_region.rs` (region-level, build a region the way `tests/spawn_surface.rs` does):
  - `create_client_at_lands_over_the_column`: `CreateClientAt(0, [5, 7])` → player body x,z within 0.5 block of the column centre and y above `surface_voxel_y` at that column; `clients` contains 0 with `Inventory::seeded()`.
  - `client_left_removes_body_and_client`: after `CreateClient(0)` then `ClientLeft(0)`: `player_entites` and `clients` lack 0, entity count back to the pre-create count.
  - `client_left_undoes_bit_exact`: hash after `CreateClient(0)` == hash after `ClientLeft(0)` + undo of that entry (use the undo helper the rollback tests use, `HashMode::Full`).
  - `client_left_reconciles_on_a_client_replica`: a client-side region (`local_client_id = Some(0)`) receiving the authoritative `ClientLeft(0)` through reconcile ends with no body for 0 and no `GameError`.
- [ ] **Step 2:** `just test-one game "--test play_region"` → FAIL.
- [ ] **Step 3:** Implement as above.
- [ ] **Step 4:** `just test-one game "--test play_region"` → PASS; `just test-one game "--test spawn_surface"` → PASS; `just smoke` → PASS.
- [ ] **Step 5:** `just determinism-wasm` → PASS (new serialized event variants).
- [ ] **Step 6:** Commit `feat(game): ClientLeft / CreateClientAt server-authored events`.

---

### Task 3: WorldManager — push the map, handle Play

**Files:**
- Modify: `crates/game/src/world_manager.rs`
- Test: `crates/game/tests/play_region.rs` (manager section; copy the `Harness` from `tests/world_manager.rs`)

**Interfaces:**
- Consumes: Task 1 types, Task 2 events.
- Produces: `impl WorldManager<S> { pub fn set_world_map(&mut self, map: WorldMap) }` (stored as `Option<WorldMap>`).
- Behaviour:
  - `ClientConnected(id)`: after the existing logic, if a map is set, send `(Some(id), ServerPacket::WorldMap(map.clone()))`.
  - `PlayRegion(target)` from `id`, refusals as `(Some(id), PlayRefused(target, r))` with no state change, checked in order: no map → `NoWorldMap`; `map.tile(target)` None → `OffMap`; `homes[id] == target` → `AlreadyHere`; `tile.landing` None → `NoDryLand`.
  - Success: `send_to_region(home, Event(ClientLeft(id)))`; `ensure_running(target)`; `send_to_region(target, Event(CreateClientAt(id, landing)))`; `homes.insert(id, target)`; send `PlayerRegion(Some(target), id)`; `refresh_keepalive(home, now)` and `(target, now)`; `perf_event!("play_region", "client={id} from={},{} to={},{}", ..)` (one line per Play).

- [ ] **Step 1: Write failing tests** (map = 3×3 tiles at origin (-1,-1), all with `landing: Some([2, 2])` except (1,1) `None`):
  - `connect_pushes_world_map` — packets after `ClientConnected(0)` include `WorldMap` to `Some(0)`; without `set_world_map`, none.
  - `play_moves_home_and_creates_player_in_target` — after Play (1,0): `PlayerRegion(Some((1,0)), 0)` sent; `running_regions()` contains (1,0); `with_region((1,0))` shows client 0's body over block column [2,2]; `with_region(SPAWN_REGION)` has no player 0.
  - `old_region_parks_after_grace` — after Play and `ReleaseRegionConnection(SPAWN_REGION)`, a `maintain` at `UNLOAD_GRACE_MS + 1` leaves SPAWN_REGION not running.
  - `play_refusals` — one assertion per `PlayRefusal` (no map; (5,5) → `OffMap`; current home → `AlreadyHere`; (1,1) → `NoDryLand`), each with homes unchanged.
  - `double_play_is_refused` — two Play (1,0) back to back: second gets `AlreadyHere`; target region has exactly one player entity.
- [ ] **Step 2:** `just test-one game "--test play_region"` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `just test-one game "--test play_region"` → PASS; `just test-one game "--test world_manager"` → PASS; `just test-threads` → PASS.
- [ ] **Step 5:** Commit `feat(game): WorldManager pushes the world map and handles Play`.

---

### Task 4: worldgen — tile summaries from the artifact

**Files:**
- Create: `crates/worldgen/src/tile_map.rs` (`pub mod tile_map;` in `lib.rs`)
- Modify: `crates/worldgen/src/forged.rs` (`pub fn surface_rock(&self, x_m: f64, z_m: f64) -> Option<u8>` — the rock index of `column` at the surface `s`, reusing the `geo_offset_m` lookup; `pub fn artifact_bounds_engine_m(&self) -> ([f64; 2], [f64; 2])` — min/max engine metres covered by the artifact)
- Test: `crates/worldgen/tests/tile_map.rs` (move `synthetic()`, `engine()`, `plain()` and their consts from `tests/forged_world.rs` into `tests/common/mod.rs` and use them from both)

**Interfaces:**
- Produces:
  - `pub fn build_world_map(world: &ForgedWorld) -> game::WorldMap` — grid = every engine region tile intersecting the artifact; per tile, iterate the artifact raster cells whose centres fall in the tile (cells outside the artifact count as ocean at `OUTSIDE_HEIGHT_M`); rivers by any reach point inside the tile footprint (artifact metres); `surface_rock` = mode of `world.surface_rock` at the in-tile cell centres.
  - `pub fn is_dry(sample: &PointSample) -> bool` — the same predicate `examples/world_map.rs` uses for "not water" (surface block at or above `SEA_TOP_BLOCK`, no lake/river surface above ground); move it there and have the example call it.
  - Landing: in-tile cell centres sorted by distance to the tile centre (ties by z then x); first whose `world.sample(x, z)` is `is_dry` → region-local block column `floor((x_m - tile_min_x_m))` etc. (1 block = 1 m — derive from `BLOCK_VOXELS` / `UNITS_PER_METER`), always `< REGION_BLOCKS`. `None` if none.
  - Tile footprint in metres: `REGION_SIZE / UNITS_PER_METER` (= `REGION_BLOCKS` m); tile `rc` spans engine metres `[rc.x * that, (rc.x + 1) * that)`.
- Only `ForgedWorld` raster / `sample` / `surface_rock` calls — no `generate_*`.

- [ ] **Step 1: Write failing tests** (`synthetic()` has sea west of `ix < 4`, a lake basin, a river along `RIVER_Z`):
  - `grid_covers_the_artifact` — `width*height` tiles; every tile intersecting the artifact present, `origin` = region of the artifact's min corner.
  - `sea_tile_has_no_landing` — the tile holding artifact x < 4 cells only: `sea_fraction > 0.95`, `landing == None`.
  - `land_tile_lands_near_its_centre_on_dry_ground` — a plain tile: `landing` Some, `world.sample` at that column `is_dry`, column within 2 blocks of the tile centre.
  - `river_tile_counts_the_river_and_never_lands_in_it` — tile on `RIVER_Z`: `rivers >= 1`, `river_max_width_m == RIVER_WIDTH_M`, landing column's sample `is_dry`.
  - `lake_tile_reports_lake` — `lake_fraction > 0`, `lake_level_m == Some(LAKE_LEVEL)`.
  - `rim_tile_counts_outside_as_sea` — a tile hanging off the artifact edge: outside cells counted in `sea_fraction`; landing (if any) inside the artifact.
  - `surface_rock_is_the_top_formation` — on the synthetic column, `surface_rock` names `SANDSTONE`.
- [ ] **Step 2:** `just test-one worldgen "--test tile_map"` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `just test-one worldgen "--test tile_map"` and `just test-one worldgen "--test forged_world"` → PASS; `just world-map --world worlds/default.world --size 256` still runs.
- [ ] **Step 5:** Commit `feat(worldgen): per-region tile summaries from the forged world`.

---

### Task 5: worldgen — how a tile looks and reads

**Files:**
- Modify: `crates/worldgen/src/tile_map.rs`; `crates/worldgen/examples/world_map.rs` (use the lifted ramp)
- Test: `crates/worldgen/tests/tile_map.rs`

**Interfaces:**
- Produces:
  - `pub enum TerrainClass { Ocean, Coast, Lowland, Hills, Mountains }` + `pub fn terrain_class(t: &TileSummary) -> TerrainClass` and `impl TerrainClass { pub fn label(self) -> &'static str }`. Consts: `OCEAN_SEA_FRACTION = 0.95`, `COAST_SEA_FRACTION = 0.05`, `HILLS_RELIEF_M = 25.0`, `MOUNTAINS_RELIEF_M = 80.0`, `MOUNTAINS_MIN_M = 150.0`. Order: Ocean if `landing.is_none() || sea_fraction >= OCEAN_SEA_FRACTION`; Coast if `sea_fraction > COAST_SEA_FRACTION`; Mountains if `relief_m >= MOUNTAINS_RELIEF_M || ground_max_m >= MOUNTAINS_MIN_M`; Hills if `relief_m >= HILLS_RELIEF_M`; else Lowland.
  - `pub fn land_color(height_m: f64) -> [f64; 3]` / `pub fn sea_color(depth_m: f64) -> [f64; 3]` — lifted from `world_map.rs` `base_color` (example now calls these).
  - `pub fn tile_color(t: &TileSummary) -> [u8; 3]` — sea colour by `-ground_mean_m` if Ocean, else land colour of `ground_mean_m`, blended toward lake blue by `lake_fraction`, shaded by `slope` with the example's sun vector.
  - `pub const MAP_TILE_PX: u32 = 8;` and `pub fn rasterize(map: &game::WorldMap) -> (u32, u32, Vec<[u8; 4]>)` — `MAP_TILE_PX` px per tile, last row/column of each tile darkened by `GRID_DARKEN = 0.75` (grid lines), a 2×2 river mark (`RIVER_RGB`) at the tile centre when `rivers > 0`; opaque.
  - `pub fn tile_info_lines(map: &game::WorldMap, rc: RegionCoords) -> Vec<String>` — in order, omitting absent facts: `"Region (x, z)"`, `"Terrain: <label> (provisional)"`, `"Elevation: <min>–<max> m (mean <mean> m)"` (rounded to whole m), `"Rivers: <n> (largest <w> m wide)"` if rivers, `"Lake: level <l> m"` if lake, `"Rock: <name>"` if rock.
  - `pub fn play_blocker(map: &game::WorldMap, rc: RegionCoords, home: Option<RegionCoords>) -> Option<&'static str>` — `Some("You are here")` if home, `Some("No dry land")` if landing None, else None. (Same rule the manager enforces.)

- [ ] **Step 1: Write failing tests:** `terrain_class_thresholds` (one hand-built summary per class, plus the boundary at each const), `rasterize_size_and_grid` (2×1 map → `(16, 8)`; pixel (7,0) darker than (3,3); river mark present only on the river tile), `info_lines_omit_absent_facts` (a tile with no river/lake/rock yields exactly 3 lines), `play_blocker_rules`.
- [ ] **Step 2:** `just test-one worldgen "--test tile_map"` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `just test-one worldgen "--test tile_map"` → PASS; `just world-map --world worlds/default.world --size 256` output PNG visually unchanged (same colours).
- [ ] **Step 5:** Commit `feat(worldgen): tile colour, terrain class and info lines`.

---

### Task 6: Server — build and install the map

**Files:**
- Modify: `crates/server/src/lib.rs` (after `forged::install`, before/at manager construction)

**Interfaces:**
- Consumes: `worldgen::tile_map::build_world_map`, `WorldManager::set_world_map`.
- Behaviour: when a forged world is installed (`worldgen::forged::active()` is `Some`), `manager.set_world_map(build_world_map(world))` and log `[server] world map: <w>x<h> tiles in <ms> ms`. Procedural: no map.

- [ ] **Step 1:** Implement.
- [ ] **Step 2:** `just smoke` → PASS.
- [ ] **Step 3:** `just server` (play size); confirm the world-map log line and that it reports ~64×64 tiles for `worlds/default.world` in well under a second; stop it.
- [ ] **Step 4:** Commit `feat(server): send the forged world's tile map to clients`.

---

### Task 7: Client plumbing — map in, Play out

**Files:**
- Create: `crates/client/src/renderer/world_map.rs` (`WorldMapState` + the system that fills it) — kept out of `bridge.rs`, which the chunk-16 plan rewrites; `bridge.rs` only forwards the two new `ClientUpdateEvent`s to it (e.g. as Bevy messages) in its existing update-drain match.
- Modify: `crates/game/src/lib.rs` (`ClientUpdateEvent`), `crates/client/src/instance.rs`, `crates/client/src/main.rs` (+ the wasm `sim_driver` wiring), `crates/client/src/renderer/mod.rs` (`SimBridgePlugin`, `mod world_map`), `crates/client/src/renderer/bridge.rs` (forward only)

**Interfaces:**
- Produces:
  - `ClientUpdateEvent::WorldMap(Box<WorldMap>)`, `ClientUpdateEvent::PlayRefused(RegionCoords, PlayRefusal)`.
  - `pub enum ClientRequest { PlayRegion(RegionCoords) }` in `crates/client/src/instance.rs`; `ClientManager` takes a `Receiver<ClientRequest>` and forwards `PlayRegion` as `ClientPacket::PlayRegion` (add the arm to the `select!` loop and the non-blocking drain).
  - Bevy resources: `pub struct ClientRequests(pub Sender<ClientRequest>)` (renderer/mod.rs), `#[derive(Resource, Default)] pub struct WorldMapState { pub map: Option<WorldMap>, pub refused: Option<(RegionCoords, PlayRefusal)> }` (renderer/world_map.rs, filled from the two update events).
- The offline/local-server path (`local_server.rs`) never sets a map — the map shows its "no world map" state there.

- [ ] **Step 1:** Implement.
- [ ] **Step 2:** `just smoke` → PASS (client type-check covers lib, bin, tests).
- [ ] **Step 3:** Commit `feat(client): receive the world map, send Play requests`.

---

### Task 8: Slint — menu, world map, tile panel

**Files:**
- Create: `crates/slint-ui/ui/menu.slint`, `crates/slint-ui/ui/world-map.slint`
- Modify: `crates/slint-ui/ui/game.slint` (import and place both; add the properties/callbacks below to `GameUi`), `crates/slint-ui/src/lib.rs` (tests)

**Interfaces (`GameUi`):**
- `in-out property <bool> menu-open; in-out property <bool> map-open;`
- `in property <image> map-image; in property <int> map-cols; in property <int> map-rows; in property <bool> map-available;` (false → map body shows `"No world map (procedural world)"`)
- `in property <int> home-col: -1; in property <int> home-row: -1; in-out property <int> selected-col: -1; in-out property <int> selected-row: -1;`
- `in property <[string]> tile-lines; in property <bool> play-enabled; in property <string> play-reason; in property <string> map-status;` (e.g. "Loading region…", refusal text)
- `in-out property <float> map-zoom: 1; in-out property <length> map-pan-x; in-out property <length> map-pan-y;`
- `callback open-map(); callback close-menu(); callback close-map(); callback select-tile(int, int); callback play();`
- Menu: `FrameFace` titled "Menu", centred, `Button`s "World Map" (→ `open-map()`) and "Close" (→ `close-menu()`).
- Map: `FrameFace` titled "World Map" filling the window minus a margin; left: clipped area showing `map-image` with `image-rendering: pixelated`, scaled by `map-zoom` (wheel, `MAP_ZOOM_MIN = 0.5` … `MAP_ZOOM_MAX = 8`, zoom about the cursor) and offset by pan (drag); hover rectangle under the pointer, selection rectangle, distinct home-tile rectangle (tile size = `MAP_TILE_PX` logical px × zoom; the px constant duplicated as a Slint global with a comment pointing at `worldgen::tile_map::MAP_TILE_PX`); click → `select-tile(col, row)`. Right: side panel with `tile-lines` as `LabelFace`s, `play-reason` caption when non-empty, `Button` "Play" (`enabled: play-enabled`, → `play()`), `Button` "Close" (→ `close-map()`), `map-status` caption.

- [ ] **Step 1: Write failing tests** in `crates/slint-ui/src/lib.rs` `#[cfg(test)]` (headless `Overlay`): `play_button_fires_play` (open map, set `play-enabled`, render once, dispatch a pointer press/release at the Play button's position — not `invoke_play()`, which would skip the button — and assert the connected `play` callback ran; with `play-enabled` false the same click must not fire it), `clicking_the_map_selects_a_tile` (map-cols/rows 4×4, zoom 1, pan 0, click inside tile (2,1) → `select-tile(2,1)` callback with those args), `closed_ui_draws_nothing` (both closed, panel closed → `opaque_at` false at the window centre).
- [ ] **Step 2:** `just test-one slint-ui "--lib"` → FAIL.
- [ ] **Step 3:** Implement the components.
- [ ] **Step 4:** `just test-one slint-ui "--lib"` → PASS.
- [ ] **Step 5:** Commit `feat(slint-ui): in-game menu and world map`.

---

### Task 9: Client glue — keys, state, Play

**Files:**
- Modify: `crates/client/src/renderer/slint_overlay.rs`, `crates/client/src/renderer/cursor.rs`, `crates/client/src/renderer/commander/mod.rs` (+ any commander system that checks `overlay_open`)

**Interfaces:**
- Consumes: `WorldMapState`, `ClientRequests`, `HudStatus`, `worldgen::tile_map::{rasterize, tile_info_lines, play_blocker}`.
- Produces:
  - Keys (consts next to `SLINT_PANEL_KEY`): `MENU_KEY = KeyCode::Escape`, `MAP_KEY = KeyCode::KeyM`. Esc: map open → close map; else toggle menu. M: toggle map (opening closes the menu).
  - `CursorPolicy.ui_modal: bool` written ONLY by `slint_overlay` (= menu-open || map-open); `impl CursorPolicy { pub fn blocks_game(&self) -> bool { self.overlay_open || self.ui_modal } }`; commander systems that check `overlay_open` switch to `blocks_game()`.
  - `UiAction` gains `OpenMap, CloseMap, CloseMenu, SelectTile(i32, i32), Play`.
  - On `WorldMapState.map` change: `rasterize` → `slint::Image::from_rgba8` once; set `map-cols/rows`, `map-available`.
  - On selection / home / map change: `tile-lines`, `play-enabled = play_blocker(..).is_none()`, `play-reason`; `home-col/row` from `HudStatus.home_region` − `map.origin`.
  - `Play` → `ClientRequests.send(ClientRequest::PlayRegion(rc))`, set a local `PlayPending(Some(rc))`, close the map; `map-status = "Loading region…"` until `HudStatus.home_region == Some(rc)`, then clear. `PlayRefused` → `map-status` = refusal text, pending cleared.
- Esc/M are only bound in commander mode (the overlay plugin only exists there).

- [ ] **Step 1:** Implement.
- [ ] **Step 2:** `just smoke` → PASS.
- [ ] **Step 3:** Commit `feat(client): Esc menu, M world map, Play`.

---

### Task 10: Camera re-places on a home change; docs; live check

**Files:**
- Modify: `crates/client/src/renderer/commander/mod.rs` (`track_home_region`, `place_on_body`), `CLAUDE.md` (client section: menu/map/Play one paragraph; `game` section: `ClientLeft`/`CreateClientAt`, `WorldMap`), `TODO.md` (follow-ups: embark, adventure-mode walking, real biomes)

**Interfaces:**
- `track_home_region`: when `bounds` changes from `Some(old)` to `Some(new)` with `old != new`, set `cam.placed = false` (first home unchanged).
- `place_on_body`: only accepts a body position inside `cam.bounds` (so the old region's body never re-places the camera); sets focus AND `ground_y`.

- [ ] **Step 1:** Implement.
- [ ] **Step 2:** `just smoke` → PASS.
- [ ] **Step 3: Live check (ask the user first — it starts a server + client on their desktop):** `just adapter=Intel run`; Esc → menu → World Map; M opens/closes; zoom/pan; select a land tile → panel lines; Play → "Loading region…", camera lands over the new region, minimap/HUD show it; old region parks (`[server]` log, ~5 s); Play on own tile / ocean disabled with reason.
- [ ] **Step 4:** Commit `feat(client): commander camera follows a Play; docs`.
