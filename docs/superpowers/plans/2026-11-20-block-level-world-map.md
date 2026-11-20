# One Zoomable Block-Level Map Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One map, zoomable from the whole world down to 1 m per pixel, showing real blocks, water,
relief and player edits. It replaces the HUD minimap, the in-game world map (M) and the arrival map's
site choice.

**Architecture:**
- **Tile pyramid:** 256×256 samples per tile; level `k` is 2^k m per sample. Each sample holds
  height, top block, water depth and flags.
- **Building a tile:** `crates/map-tiles` builds a tile from the forged world (worldgen point samples)
  plus the stored top of every chunk a cut has written.
- **Writes:** the store writes a chunk's top beside the chunk itself, in the same statement of the same
  cut part.
- **Single-player:** the native client calls `map-tiles` itself against the world's `terrain.sqlite`
  (opened read-only) and `terrain.world`.
- **Multiplayer:** a thin axum handler on the coordinator's HTTP server serves the same call,
  reached through the `web` proxy.
- **Client:** draws the tiles in a Bevy pass under the Slint layers. Slint keeps the frame, the
  buttons and the markers.

**Tech Stack:** Rust; Bevy 0.19.1 (a render pass in the Core3d graph, WGSL); Slint (software renderer,
existing layers); sqlx (SQLite / PostgreSQL) via `world-store`; worldgen; axum 0.8 + reqwest 0.12
(ring) for the HTTP front; lz4 via `game::encode_packet`; nginx (`deploy/images/web/nginx.conf`);
OpenTofu (`deploy/modules/world`).

**Spec:** `docs/superpowers/specs/2026-11-19-block-level-world-map-design.md` — read sections 3–8b
before any task. Also read `CLAUDE.md` ("Testing", "Client", "Slint UI", "Server / store": "Owner
epochs & fencing").

## Where this plan departs from the spec

1. **A chunk's top, not a region's surface row.**
   - The spec stores one `region_surface` row per region, derived from the chunks the part carries.
     But a part carries only CHANGED chunks (`PortableRegion::chunks`). A column's top depends on its
     whole vertical stack, so it cannot be derived from those chunks alone.
   - Instead, migration `0021_chunk_tops` adds `chunks.top`: that chunk's own top block per block
     column, written by the same `INSERT … ON CONFLICT` that writes `chunks.data`.
   - The spec's invariant holds exactly: the top matches the stored chunk, in one statement, keyed like
     the chunk. It moves with land in every Elastic 3/5 path because it is part of the chunk row.
   - The reader combines a region's chunk tops (highest non-empty chunk per column) and caches the
     result per `(region, terrain_version)`. A region's first save writes every chunk ("all of them
     against empty terms"), so a stored region's tops are complete.
2. **The map route is unauthenticated; the proxy limits rate per client IP.**
   - The surface is not secret: LOD already gives it to every player. Login JWTs are join tickets
     with a 300 s lifetime, and no long-lived player token exists to check.
   - Abuse is bounded by nginx `limit_req` on `/map/`, by the reader's build queue cap (503 +
     `Retry-After`), and by `Cache-Control: immutable` on pristine tiles. **(Dawid, confirm 1.)**
3. **Native multiplayer finds the map URL in login's world list:** `WorldEntry.map_url:
   Option<String>`, set by `login world add --map-url`. Login gets a migration that adds one nullable
   column; no account is touched.
4. **Pristine tree canopies are not stamped in v1.**
   - Pristine land shows its ground cover's top block; real chunks show real canopies.
   - Task 0 measures the cost of stamping crowns. If it is under 25 % of a level-0 build it becomes a
     follow-up, never a hidden extra.

## To confirm with Dawid (the plan proceeds on these defaults)

1. The map route is unauthenticated, rate-limited by the proxy (departure 2).
2. Spec §9 decisions taken as proposed:
   - the whole world is visible from the start (no fog);
   - the minimap is north-up;
   - edits are overlaid down to level 3 (8 m per sample);
   - the region-spawn leftovers are removed (Task 12);
   - the map is served over HTTPS through the `web` proxy;
   - it is mounted in the coordinator's pods first;
   - far terrain may lag by one save interval.
3. Task 0 runs release builds and opens a window on a real adapter: a benchmark run (`CLAUDE.md`,
   Profiling: ask before launching).

## Global Constraints

**Tests**
- **Tests:** only regression tests and gates are committed (`CLAUDE.md`, Testing).
  - Every "temporary test" below is written, run, and DELETED before its task's commit.
  - `just test-audit` must pass.
  - Client tests (`just test-one client …`) are a slow opt-in: ask first.

**Simulation and placement**
- **Nothing in the simulation:** no map work runs on a region thread, nothing is hashed or saved in a
  cut except `chunks.top`, and no `#[rollback]` state changes. `just determinism-wasm` runs once at
  closing.
- **Engine-agnostic:** `game`, `worldgen`, `world-store` and `map-tiles` have no Bevy and no axum.
  `map-tiles` is a native-only dependency of the client (`[target.'cfg(not(target_arch =
  "wasm32"))'.dependencies]`).
- **Region edges are never literals:** derive them from `REGION_BLOCKS`, `CHUNK_BLOCKS`,
  `BLOCK_VOXELS`. A literal 256 is `MAP_TILE_SAMPLES`.

**Store**
- **Migrations:** `0021_chunk_tops.sql` in both `migrations/pg` and `migrations/sqlite`, identical in
  shape. Worlds are re-forged, never migrated. A row without a top (`NULL`) is read as "derive it from
  `data`" (a world saved before 0021 in a dev checkout), never as empty.
- **Forged worlds:** pass the forged world explicitly (`&ForgedWorld`) everywhere in this plan. Never
  `forged::active()` / `forged::install` in client code: one client process plays several worlds.

**Wire and versions**
- **Wire:** tile bodies are `game::encode_packet` / `decode_packet`. Packet enums are append-only;
  this plan adds no game packet.
- **`WORLD_MAP_VERSION`:** bump it if `build_world_map`'s output changes (Task 8's `SiteMarker.at`
  does not touch it — check).

**Client**
- **UI:** Slint only. No `bevy_ui`. Never bind `visible` / `opacity` / shadows on a component's ROOT.
  `CursorPolicy` stays the only cursor-grab writer.
- **Session:** every new client resource or `Local` is registered with `add_session_reset` /
  `SessionLocal`, or tagged `SessionScoped`. The tile cache resets only when the WORLD changes
  (spec 5.3).
- **Shaders:** `assets/shaders/map_tile.wgsl` is compiled in, like the other terrain shaders. After
  Task 6, `just pull-smoke` and a look with `just run` on a real adapter (ask first).

**Working in the shared checkout**
- **Owner's uncommitted edits** sit in `world_map.rs`, `slint_overlay.rs`, `world-map.slint`,
  `hud.slint`, `game.slint`, `world_scale.rs` and `frontend/*` (Play removed, `can-leave`, Settings,
  the hotbar frame).
  - Build on them; never revert them.
  - Stage only your own hunks: `git diff -U3` filtered, `git apply --cached --recount`. Verify
    `git diff --cached --name-only` before every commit; no amend.
- **Verification per task:** the touched tests and `just smoke`. Whole suites only at closing. No
  worktrees. The justfile, `CLAUDE.md` and `TODO.md` change in the same commit as the behaviour they
  describe.
- **Gate for the HTTP tasks:** Task 11 starts only once `docs/superpowers/plans/2026-11-19-coordinator-http-api.md`
  has an "As built" covering its Tasks 3 and 7. Otherwise stop after Task 10 and say so.

## Constants (named, with their homes)

| Const | Value | Where |
|---|---|---|
| `MAP_TILE_SAMPLES` | 256 | `game::map_tile` |
| `MAP_MAX_LEVEL` | derived: first level at which the world's `WorldMap` bounds fit one tile | `game::map_tile::top_level(&WorldMap)` |
| `MAP_EDIT_MAX_LEVEL` | 3 | `map-tiles` (overlays above it are skipped) |
| `MAP_TILE_CACHE_BYTES` | 256 MiB server / 128 MiB client | `map-tiles::TileSourceConfig` |
| `MAP_REGION_TOPS_CACHE` | 512 regions | `map-tiles` |
| `MAP_BUILD_WORKERS` | 2 | `server/src/map_api.rs` |
| `MAP_BUILDS_QUEUED_MAX` | 64 | `server/src/map_api.rs` |
| `MAP_FETCH_IN_FLIGHT` | 6 | client `renderer/map/store.rs` |
| `MAP_REVALIDATE_MS` | 30 000 | client: how often a shown tile with real regions is re-asked |
| `MAP_DISK_CACHE_BYTES` | 512 MiB | client `renderer/map/disk.rs` |
| `MINIMAP_M_PER_PX` | 1.0 default, 0.5 ..= 8.0 | client `renderer/map/views.rs` |

## Review Focus

1. **A dig shows on the minimap at once and stays after the region unloads.** Local painting
   overrides a served tile until a newer `ETag` arrives (spec 5.3). Task 9 step 4 checks it by hand.
2. **The single-player client reads while the child server commits a cut.** WAL readers run beside
   the writer: a tile read during a commit returns the previous or the new tops, never an error or a
   torn row. Task 5's temporary test checks it.
3. **A pre-0021 or partly saved region.** A `NULL` top is derived from `data`; a region with no
   stored chunks is pristine. Task 2's temporary test checks it.
4. **The world's edge and negative coordinates.** Tiles outside the `WorldMap` bounds are never
   requested. Samples past the edge draw as the map's background, not as garbage. Key math floors
   toward −∞. Task 1's temporary test (negative keys) and Task 7's hand check (pan past the edge)
   cover it.
5. **A home hop, a drain's rejoin, a session end and a new world.** The tile cache survives the
   first two, keeps the same world after the third, and is cleared on the fourth (spec 5.3). Task 5
   step 5 checks it by hand with `just run-two` (ask first).

## File structure

| File | Change |
|---|---|
| `crates/game/src/map_tile.rs` | new: `MapTileKey`, `MapSample`, `MapTile`, `ChunkTop`, `chunk_top(&Chunk)`, codec, `top_level`, `tile_regions` |
| `crates/world-store/migrations/{pg,sqlite}/0021_chunk_tops.sql` | new: `chunks.top` |
| `crates/world-store/src/rows.rs` | `cut_rows` writes `top`; `region_tops` read |
| `crates/world-store/src/handle.rs` | `StoreHandle::region_tops` |
| `crates/worldgen/src/surface.rs` | new: `surface_tile_forged`, `surface_tile_procedural` |
| `crates/map-tiles/` | new crate: `TileSource` |
| `crates/client/src/renderer/map/` | new: `mod.rs`, `store.rs` (`MapTiles`, `MapSource`), `palette.rs`, `views.rs` (`MapView`), `pass.rs`, `paint.rs` (local near-window painting), `disk.rs` |
| `assets/shaders/map_tile.wgsl` | new |
| `crates/client/src/renderer/commander/height.rs`, `minimap.rs` | `chunk_tops` uses `game::map_tile::chunk_top`; the base buffer goes |
| `crates/client/src/renderer/world_map.rs`, `slint_overlay.rs` | map image path replaced by `MapView` + markers in screen px |
| `crates/slint-ui/ui/world-map.slint`, `hud.slint` | the map and minimap areas become transparent holes; markers as a model |
| `crates/game/src/protocol.rs` | `SiteMarker.at` |
| `crates/server/src/map_api.rs` | new: the axum handler, mounted on `coordinator_api` and on `VOXEL_MAP_BIND` |
| `deploy/images/web/nginx.conf`, `deploy/modules/world/{web,coordinator}.tf` | `/map/` proxy + `limit_req` |
| `crates/login-api/src/lib.rs`, `crates/login/…` | `WorldEntry.map_url` |
| `crates/worldgen/src/tile_map.rs` | `rasterize` and `MAP_TILE_PX` removed (Task 7); `play_blocker` (Task 12) |

---

### Task 0: Measure and spike (throwaway — nothing committed but numbers)

**Files:** none committed. Scratch in `crates/worldgen/examples/` and `crates/client/examples/`,
deleted after.

- [ ] **Step 1:** Ask Dawid before running (release builds, a window on a real adapter).
- [ ] **Step 2: Build cost.** A scratch example times a tile built from `ForgedWorld::load` of the
  debug world, through Task 3's intended path (point samples over `world.patch`), at levels 0, 3, 6
  and 9 (median of 20).
  - Also time the same with crown stamping as `coarse_from_patch` does it.
  - Record the encoded size of three planes (height i16 delta + lz4, block u16 + lz4, water u8 + lz4)
    for a coast, a mountain and a plain.
- [ ] **Step 3: Store cost.** Time `chunk_top` over a 256 m region's chunks (from `load_terrain`),
  and its encoded size per chunk.
- [ ] **Step 4: Draw spike.** One hard-coded 256² `R16Uint` texture drawn by a pass slotted
  `.after(Core3dSystems::PostProcess).before(slint_composite_pass)`, scissored to the HUD minimap's
  rect, on the Iris Xe and in `just web`.
  - Confirm that the HUD layer's transparent pixels show it, and that the pointer still reaches the
    Slint `TouchArea` there.
- [ ] **Step 5:** Write the numbers into the spec as a §10 "Measured", and adjust the constants table
  of this plan if any is off by more than 2×. Delete the scratch files. Commit the spec and plan only
  (`docs(map): measured tile cost, size and the draw spike`).
- **Stop rule:** if a level-0 tile costs more than 50 ms in release, or the pass cannot sit under the
  Slint layers, stop and report. The design's fallbacks (128-sample tiles; a Slint-image path for the
  minimap only) need Dawid.

### Task 1: The tile types and the chunk top (`game::map_tile`)

**Files:**
- Create: `crates/game/src/map_tile.rs`
- Modify: `crates/game/src/lib.rs` (module + glob re-export), `crates/client/src/renderer/commander/height.rs`

**Interfaces — produces:**
```rust
pub const MAP_TILE_SAMPLES: u32 = 256;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MapTileKey { pub level: u8, pub x: i32, pub z: i32 }
impl MapTileKey {
    pub fn sample_m(self) -> i64;                       // 1 << level
    pub fn origin_block(self) -> (i64, i64);            // x·256·2^level, z·…
    pub fn containing(level: u8, bx: i64, bz: i64) -> MapTileKey; // floors toward −∞
    pub fn parent(self) -> Option<MapTileKey>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MapSample { pub height: i16 /* m, top of the top block */, pub block: BlockId, pub water: u8 /* m, saturating */, pub flags: u8 }
pub mod sample_flags { pub const ICE: u8 = 1; pub const REAL: u8 = 2; pub const OUTSIDE: u8 = 4; }
pub struct MapTile { pub key: MapTileKey, pub samples: Vec<MapSample> /* len 256², row-major z then x */ }
impl MapTile { pub fn encode(&self) -> Vec<u8>; pub fn decode(bytes: &[u8]) -> Result<MapTile, DecodeError>; }
/// One chunk's own top per block column (None = no shown block in this chunk's column).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkTop(pub Vec<Option<(u16 /* voxel y within chunk + 1 */, BlockId)>>); // len CHUNK_BLOCKS²
pub fn chunk_top(chunk: &Chunk) -> ChunkTop;
pub fn tile_regions(key: MapTileKey) -> impl Iterator<Item = RegionCoords>;
pub fn top_level(map: &WorldMap) -> u8;
```

- [ ] **Step 1: Write the temporary test.**
  - `MapTileKey::containing(0, -1, -1) == MapTileKey { level: 0, x: -1, z: -1 }`;
    `containing(3, 2047, 0).x == 0`; `containing(3, 2048, 0).x == 1`.
  - A tile with mixed samples round-trips through `encode` / `decode`.
  - `chunk_top` of a chunk with stone at block (3, 5, 7) and air above returns `Some((6 *
    BLOCK_VOXELS as u16, stone))` at column `3 + CHUNK_BLOCKS * 7`, and `None` in an all-air column.
  - A slope chisel's column counts as a full block (as `chunk_tops` does today).
- [ ] **Step 2:** Run `just test-one game "--lib map_tile"`. Expected: FAIL (module missing).
- [ ] **Step 3:** Implement.
  - `chunk_top` is the block-centre scan of `commander/height.rs::chunk_tops` moved here, keeping
    the block id that `Chunk::column_top` returns; slopes are handled as there.
  - The encoding is three planes: height delta-coded per row, block as u16, water and flags as u8,
    each `encode_packet`'d.
- [ ] **Step 4:** `commander/height.rs::chunk_tops` derives its `surface` from `game::map_tile::chunk_top`.
  Its `ground` (the first non-liquid below) stays local. No behaviour change.
- [ ] **Step 5:** Run the test. Expected: PASS. Delete the temporary test. Run `just smoke`.
- [ ] **Step 6:** Commit (`feat(game): map tile key, tile codec and a chunk's top`).

### Task 2: The store writes and reads chunk tops

**Files:**
- Create: `crates/world-store/migrations/pg/0021_chunk_tops.sql`, `…/sqlite/0021_chunk_tops.sql`
- Modify: `crates/world-store/src/rows.rs` (`cut_rows`, ~1801; a new read), `crates/world-store/src/handle.rs`, `crates/world-store/src/codec.rs` (`FORMAT_CHUNK_TOP`)

**Interfaces — consumes:** `game::map_tile::{chunk_top, ChunkTop}`. **Produces:**
```rust
impl StoreHandle {
    /// The region's terrain_version and the top of every stored chunk (derived from `data` where `top` is NULL);
    /// None when the region has no row.
    pub fn region_tops(&self, rc: RegionCoords) -> Result<Option<(u64, Vec<(ChunkCoords, ChunkTop)>)>, StoreError>;
}
```
(`stored_regions()` / `stored_regions_since(tick)` already give every region's `terrain_version`.)

- [ ] **Step 1:** The migration: `ALTER TABLE chunks ADD COLUMN top BLOB` (pg: `BYTEA`), nullable,
  with a header comment in each file naming the other.
- [ ] **Step 2:** In `cut_rows`, the chunk `INSERT … ON CONFLICT` writes
  `top = encode(FORMAT_CHUNK_TOP, &chunk_top(chunk))` in the same statement (`DO UPDATE SET data =
  excluded.data, top = excluded.top`).
  - No other writer of `chunks` exists (`grep "INSERT INTO chunks"`): confirm, and stop if one
    appears.
  - No lock walk changes: the same row, the same statement.
- [ ] **Step 3:** `region_tops`: one `SELECT terrain_version FROM regions` and one `SELECT cx, cy, cz,
  top, data FROM chunks WHERE rx = $1 AND rz = $2`, decoding `data` only where `top` is NULL. These
  are plain reads, allowed on a `ReadOnly` handle.
- [ ] **Step 4: Temporary test** (SQLite in-memory, then `MMO_TEST_DATABASE_URL` Postgres if set):
  - Commit a one-phase cut with two chunks; `region_tops` returns both tops equal to `chunk_top` of
    each.
  - `UPDATE chunks SET top = NULL` for one: still equal.
  - Run with `just test-one world-store "--lib region_tops"`. Expected: PASS. Delete the test.
- [ ] **Step 5:** `just smoke`. Commit (`feat(world-store): chunks carry their top; region_tops`).

### Task 3: The pristine surface from worldgen

**Files:**
- Create: `crates/worldgen/src/surface.rs`
- Modify: `crates/worldgen/src/lib.rs` (`point_sample` and `ColSample` reachable from `surface.rs`; nothing new made `pub` but the two functions below)

**Interfaces — produces:**
```rust
/// Every sample of `key` from worldgen alone (no stored terrain): height = the top block's top
/// (surface, or glacier ice), block = cover.surface / solid_material at the surface, water = water_top − surface
/// when positive, ICE flag from ice_from. Samples at each sample's centre.
pub fn surface_tile_forged(world: &ForgedWorld, key: MapTileKey, mats: &Materials) -> MapTile;
pub fn surface_tile_procedural(seed: u64, key: MapTileKey, mats: &Materials) -> MapTile;
```

- [ ] **Step 1: Temporary test.** At level 0, over a region of the debug world, every sample's
  `height` equals `ground_y_forged` + 1 at that block. Every sample's block equals the top block of
  `generate_region_forged`'s column there, wherever no tree stands (assert ≥ 99 %, since trees are
  departure 4).
- [ ] **Step 2:** Run it. Expected: FAIL.
- [ ] **Step 3:** Implement with `world.patch(origin, origin, span)` and `point_sample` per sample
  centre (the `coarse_tile_forged_with` pattern, without the steep-face ring). At levels above 4,
  sample `ForgedWorld::sample` directly if Task 0 showed the patch build dominating.
- [ ] **Step 4:** Run it. Expected: PASS. Delete it. `just smoke`.
- [ ] **Step 5:** Commit (`feat(worldgen): a map tile's surface from worldgen`).

### Task 4: `crates/map-tiles` — the shared reader

**Files:**
- Create: `crates/map-tiles/{Cargo.toml,src/lib.rs}`; root `Cargo.toml` (member + workspace dep)

**Interfaces — consumes:** Tasks 1–3. **Produces:**
```rust
pub struct TileSourceConfig { pub cache_bytes: usize, pub region_tops_cache: usize }
pub enum Fetched { Unchanged, Tile { etag: u64, bytes: Arc<Vec<u8>> } }
pub struct TileSource { /* store: StoreHandle (ReadOnly), world: Arc<ForgedWorld> | seed, mats, LRUs */ }
impl TileSource {
    pub fn forged(store: StoreHandle, world: Arc<ForgedWorld>, mats: Materials, cfg: TileSourceConfig) -> TileSource;
    pub fn procedural(store: StoreHandle, seed: u64, mats: Materials, cfg: TileSourceConfig) -> TileSource;
    /// Blocking (a store read and a build); call it on a worker thread. `Send + Sync`.
    pub fn tile(&self, key: MapTileKey, have: Option<u64>) -> Result<Fetched, StoreError>;
    /// Re-read every region's terrain_version (`stored_regions_since(last)`); called by the owner
    /// after a cut notice or on a timer. Drops cached tiles and region tops whose version moved.
    pub fn refresh_versions(&self) -> Result<(), StoreError>;
}
```

- [ ] **Step 1:** Define the `etag` as crc32 (`crc32fast`) over `(key, sorted (rc, terrain_version) of
  the tile's stored regions)` when `key.level <= MAP_EDIT_MAX_LEVEL`, else over `(key, 0)`. A tile with
  no stored region has a constant etag.
- [ ] **Step 2: The overlay.** For each stored region of the tile (level ≤ `MAP_EDIT_MAX_LEVEL`),
  take its combined tops from the region-tops LRU, keyed `(rc, version)`. Each combined top is the
  highest chunk's top per column, built from `region_tops`.
  - Set each sample whose centre falls in that region to that column's top: height in m, the block,
    the `REAL` flag.
  - Keep worldgen's `water` and `ICE` (water is recipe data, never blocks).
- [ ] **Step 3: Temporary test** against an in-memory store with a committed cut that raised a 3×3
  stone pillar 10 blocks over pristine ground:
  - the level-0 tile shows the pillar's height and block there, and pristine elsewhere;
  - `tile(key, Some(etag))` is `Unchanged`;
  - after a second cut changes the pillar and `refresh_versions()`, the etag differs.
  - Run with `just test-one map-tiles ""`. Expected: PASS. Delete it.
- [ ] **Step 4:** `just smoke` (add `map-tiles` to `smoke`'s crate list if it names crates).
- [ ] **Step 5:** Commit (`feat(map-tiles): one reader of tiles over worldgen and stored chunk tops`).

### Task 5: The client's tile store, palette and the local source

**Files:**
- Create: `crates/client/src/renderer/map/{mod.rs,store.rs,palette.rs}`
- Modify: `crates/client/Cargo.toml` (`map-tiles`, `world-store` native-only), `crates/client/src/renderer/mod.rs` (plugin; palette from `build_layers` at ~1119–1135), `crates/client/src/session.rs` (which source a session gets)

**Interfaces — produces:**
```rust
pub enum MapSource { #[cfg(not(target_arch = "wasm32"))] Local(Arc<map_tiles::TileSource>), Http { base: String } /* Task 11 */ , None }
#[derive(Resource)] pub struct MapTiles { /* world id, memory LRU (MAP_TILE_CACHE_BYTES client), per key: decoded tile, GPU handles, etag, fetched_at, painted flag */ }
impl MapTiles {
    pub fn want(&mut self, keys: impl IntoIterator<Item = MapTileKey>);   // this frame's wanted set
    pub fn best(&self, key: MapTileKey) -> Option<(MapTileKey, &GpuTile)>; // key or nearest loaded ancestor
}
#[derive(Resource)] pub struct MapPalette { pub rgb: Vec<[u8; 4]> /* index = BlockId */, pub texture: Handle<Image> }
```

- [ ] **Step 1: Palette.**
  - Each block's colour is the last level of `block_textures::mip_chain` of its TOP layer (from
    `BlockTextureLayers`), averaged over opaque texels only for cutout blocks.
  - A block without a texture takes `BlockProps`' fallback grey `[128, 128, 128, 255]`.
  - Upload as a 1-D-ish `Rgba8UnormSrgb` texture of width = the registry's length.
- [ ] **Step 2: The local source.**
  - When a session starts a menu world or a debug launch (`PendingWorld { entry_dir, terrain }`),
    open `entry_dir/terrain.sqlite` with `OpenMode::ReadOnly` (the store's own thread), and load
    `ForgedWorld::load(terrain)` (the explicit world).
  - Materials come from the client's block registry by name (`Materials::from_names`).
  - Build `MapSource::Local`. A world folder without `terrain.sqlite` (a world not yet started)
    retries the open when the child server reports ready.
- [ ] **Step 3: Fetch loop.**
  - At most `MAP_FETCH_IN_FLIGHT` `TileSource::tile` calls run on `AsyncComputeTaskPool`, nearest the
    wanted set's centre first.
  - On `Tile`, decode and create three textures (`R16Sint`, `R16Uint`, `R8Uint` water +
    `R8Uint` flags packed as `Rg8Uint`).
  - On `Unchanged`, touch `fetched_at`.
  - `refresh_versions()` runs every `MAP_REVALIDATE_MS`, and at once after the client sees its own
    save land (the existing `SaveDone` / cut notice if the client has one — check; else the timer only).
- [ ] **Step 4: Reset rule.** `add_session_reset` clears `MapTiles` only when the next session's
  world id differs. Read the world id from `world.json` / the `WorldMap`'s origin + size hash, and
  note in a comment which was used.
- [ ] **Step 5: Hand check (ask first):** `just run`; log once per 10 s
  `[map] tiles=… bytes=… fetches=…` (per-process volume, `CLAUDE.md` `[perf]` rule).
  - Leave and re-enter the same world from the menu: `tiles` stays. Enter another world: `tiles`
    drops to 0.
  - **Temporary test (Review Focus 2):** a loop of `tile()` while a thread commits cuts to the same
    SQLite file never errors. Delete it.
- [ ] **Step 6:** `just smoke`; `just web-check` (the wasm build must still compile without
  `map-tiles`). Commit (`feat(client): map tile store, block palette, the single-player source`).

### Task 6: The map pass and `MapView`

**Files:**
- Create: `crates/client/src/renderer/map/{views.rs,pass.rs}`, `assets/shaders/map_tile.wgsl`
- Modify: `crates/client/src/renderer/slint_composite.rs` (ordering only), `crates/client/src/renderer/map/mod.rs`

**Interfaces — produces:**
```rust
#[derive(Component, Clone, Copy)]
pub struct MapView {
    pub centre: DVec2,          // world blocks (x, z)
    pub m_per_px: f32,          // continuous
    pub rect: Rect,             // window pixels, top-left origin
    pub layer: MapLayerKind,    // Surface | Biome | Relief | Temperature | Precipitation | Regions
    pub visible: bool,
}
impl MapView {
    pub fn level(&self) -> u8;                         // largest k with 2^k ≤ m_per_px, clamped 0..=top_level
    pub fn world_to_screen(&self, b: DVec2) -> Vec2;
    pub fn screen_to_world(&self, p: Vec2) -> DVec2;
    pub fn wanted(&self, top: u8) -> Vec<MapTileKey>;  // covering rect + one ring
}
```

- [ ] **Step 1: The pass.**
  - Render-world system `map_pass` slotted `.after(Core3dSystems::PostProcess).before(slint_composite_pass)`,
    on the `UiCamera` view only.
  - For each visible `MapView`, set the scissor to `rect` and draw one quad per wanted key, using
    `MapTiles::best(key)` (an ancestor's texture with the sub-rect UV when the key is missing).
- [ ] **Step 2: `map_tile.wgsl`.**
  - Surface layer: palette lookup by block; water tinted by depth over the block colour (0 m = none,
    ≥ 12 m = deep sea colour of `tile_map::sea_color`).
  - Hill shade from the height gradient (sun from the north-west, strength by level).
  - `OUTSIDE` samples use the background colour.
  - Nearest filtering when `m_per_px ≤ sample_m`, a 2×2 box above.
  - Thematic layers: the `WorldMap` raster, 1 px per region tile (`tile_map::rasterize_layer`),
    sampled by world position.
- [ ] **Step 3: Hand check on the Iris Xe and `just web` (ask first):** a debug `MapView` over the
  whole window (F3 pane toggle in `dev_ui`, dev only).
  - Zoom from 512 m/px to 0.25 m/px continuously and pan.
  - No hole: an ancestor shows while a tile loads.
  - 60 fps steady with `VOXEL_VSYNC=off` (`just fps-steady`). Note the frame time in the commit
    message.
- [ ] **Step 4:** `just smoke`; `just web-check`. Commit (`feat(client): the map pass and MapView`).

### Task 7: The full map (M, Esc, arrival) on the pass

**Files:**
- Modify: `crates/slint-ui/ui/world-map.slint`, `crates/slint-ui/ui/game.slint` (bindings), `crates/client/src/renderer/world_map.rs`, `crates/client/src/renderer/slint_overlay.rs` (`sync_world_map` ~1764, `site_marks` ~1112, `map_tile` / `tile_region`, `set_screens`), `crates/worldgen/src/tile_map.rs` (remove `rasterize`, `MAP_TILE_PX`), `crates/worldgen/examples/world_map.rs` (`--tiles` writes the 1 px raster)

**Interfaces — consumes:** `MapView` (Task 6). **Produces** (Slint ↔ Rust):
```slint
in property <length> map-hole-x, map-hole-y, map-hole-w, map-hole-h;   // out: Rust reads the hole's window rect
in property <[MapMarker]> markers;   // struct MapMarker { x: length, y: length, color: color, hollow: bool, label: string, selected: bool }
callback map-drag(length /*dx*/, length /*dy*/);
callback map-wheel(length /*x*/, length /*y*/, float /*notches*/);
callback map-click(length /*x*/, length /*y*/);
```

- [ ] **Step 1: Slint.**
  - Remove the `Image` and the powers-of-two zoom from `world-map.slint`. The map area is a
    transparent `TouchArea` reporting drag, wheel and click.
  - Markers are drawn from `markers` (positions in window pixels, pushed by Rust).
  - Keep the side panel, the layer buttons, `can-leave` / Main menu, and the tile facts.
- [ ] **Step 2: Rust.**
  - One `MapView` entity for the full map, visible while the map screen is.
  - Drag pans; the wheel zooms about the cursor (×1.25 per notch, `m_per_px` clamped to
    `0.25 ..= 2^top_level`).
  - Opening centres on the player at the minimap's `m_per_px`.
  - A click selects the region under the cursor (`UiAction::SelectTile` with that region) and shows
    the block coordinates and height under the cursor beside `tile_info_lines`.
- [ ] **Step 3:** Push `markers` every frame the view or the markers change (home, selected, hover,
  sites — Task 8 moves sites to their real position).
- [ ] **Step 4:** Remove `tile_map::rasterize`, `MAP_TILE_PX`, `MAP_GRID_PX` and their callers.
  `just world-map --tiles` writes `rasterize_layer`'s 1 px raster.
- [ ] **Step 5: Hand check (ask first):** `just play` → arrival map, M in play, every layer, pan past
  the world edge (Review Focus 4), and Main menu still works.
- [ ] **Step 6:** `just smoke`. Commit (`feat(client): the world map on the map pass, continuous zoom`).

### Task 8: Sites at their position; choosing a site on the map

**Files:**
- Modify: `crates/game/src/protocol.rs` (`SiteMarker`), the coordinator's site marker build (`grep -rn "SiteMarker {" crates/game/src`), `crates/client/src/renderer/slint_overlay.rs` (`site_marks`, `UiAction::AdventurePickSite`, `SelectTile` ~1357), `crates/client/src/renderer/arrival_tabs.rs`

**Interfaces — produces:** `SiteMarker { …, at: [i32; 2] }`: a world block, the site region's origin
+ its `TileSummary::landing`.

- [ ] **Step 1:** Add `at` and fill it where markers are built. The landing comes from the world map's
  tile of the site's region. `WORLD_MAP_VERSION` is untouched, since the map does not change; confirm.
- [ ] **Step 2:** Draw site markers at `world_to_screen(at)`. Clicking within 10 px of a marker picks
  that site (`AdventurePickSite`). Picking a site, from the map or the list, eases the view over
  0.4 s to centre on `at` at `m_per_px = 1.5`. `AdventureStart` is unchanged.
- [ ] **Step 3: Hand check (ask first):** `just play` → Adventure tab.
  - The marker sits on the town's buildings at 1–2 m/px.
  - Pick from the list and from the map; Start; End adventure returns to the map.
- [ ] **Step 4:** `just smoke`; `just determinism-wasm` is not needed (`SiteMarker` is not hashed;
  confirm with `grep StateHash`). Commit (`feat(map): sites at their position, chosen on the map`).

### Task 9: The minimap on the pass, painted locally near the player

**Files:**
- Create: `crates/client/src/renderer/map/paint.rs`
- Modify: `crates/client/src/renderer/commander/minimap.rs` (base buffer removed; overlay and pointer kept through `MapView`), `crates/client/src/renderer/commander/height.rs` (keep the top's `BlockId`), `crates/slint-ui/ui/hud.slint` (base `Image` removed; the hole kept), `crates/client/src/renderer/slint_overlay.rs` (`push_hud_state` ~611)

- [ ] **Step 1: Local painting.**
  - `HeightGrid` stores the top `BlockId` beside `surface_y` (from Task 1's `chunk_top`).
  - `paint.rs` writes the level-0 tiles under the loaded near window from it, at most once per 0.25 s
    per dirty tile (`HeightGrid.version`).
  - Mark them `painted`; a painted tile is replaced only by a served tile whose etag changed since
    the painted one was made (spec 5.3).
  - Levels 1–3 are painted by 2× downsampling the painted level-0 tiles.
- [ ] **Step 2: The minimap.**
  - A `MapView` centred on the player's body (the commander camera's focus in the mayor's view),
    north-up, `MINIMAP_M_PER_PX`, rect = `minimap::screen_rect`.
  - The wheel over it zooms within `0.5 ..= 8.0`.
  - The entity dots and the camera footprint are drawn by the overlay as today, positioned through
    `world_to_screen`.
  - `minimap_pointer` maps through `screen_to_world`.
- [ ] **Step 3:** Remove `rebuild_minimap_base`, `BASE_PX`, `BASE_REBUILD_S`, the `minimap-base`
  property and its push.
- [ ] **Step 4: Hand check (ask first, Review Focus 1):** `just run`.
  - Dig a 3×3 hole: the minimap shows it within 0.25 s.
  - Walk 2 regions away and back: still shown. Open M: the same hole at 1 m/px.
  - The mayor's view: click-to-move on the minimap still moves the camera.
- [ ] **Step 5:** `just smoke`. Commit (`feat(client): the minimap on the map pass, painted from loaded chunks`).

### Task 10: Native disk cache

**Files:** Create `crates/client/src/renderer/map/disk.rs`; modify `store.rs`.

- [ ] **Step 1:** Cache under `player_name::data_dir()/map/<world id>/<level>/<x>_<z>.tile`, holding
  `etag: u64` and then the tile bytes. Write with a temp file + rename.
  - Evict the oldest-used entries above `MAP_DISK_CACHE_BYTES`, scanned at start and every 1 000
    writes.
  - Used only for `MapSource::Http`; `Local` reads its store directly.
  - A disk hit is shown at once, and its etag is sent as `have`.
- [ ] **Step 2:** Built and `just smoke` green now; exercised in Task 11's hand check. Commit
  (`feat(client): a disk cache for served map tiles`).

### Task 11: The HTTP front — multiplayer and the dev server

**Gate:** the coordinator HTTP plan's "As built" covers its Tasks 3 and 7. If not, stop and say so.

**Files:**
- Create: `crates/server/src/map_api.rs`
- Modify:
  - `crates/server/src/coordinator_api.rs` (merge the map router; its own route group, NOT under the
    service-token auth layer)
  - `crates/server/src/roles.rs` / `main.rs` (a `TileSource` from the coordinator's store + forged
    world; role `all` mounts on `VOXEL_MAP_BIND` when set)
  - `deploy/images/web/nginx.conf` (`location /map/`)
  - `deploy/modules/world/{web,coordinator}.tf`
  - `crates/login-api/src/lib.rs` (`WorldEntry.map_url: Option<String>`, `#[serde(default)]`)
  - `crates/login/{migrations,src}` (column + `world add --map-url`)
  - client `store.rs` (`MapSource::Http`: reqwest natively, `fetch` in wasm via the existing web
    fetch helper — grep `web_sys::Request`)
  - `justfile` (`world-register` prints `--map-url`), `scripts/twin_check.sh` (one phase)

**Interfaces — produces:**
- `GET /v1/map/tiles/{level}/{x}/{z}`:
  - 200: the body is `MapTile::encode` bytes, with `ETag: "<hex>"` and `Cache-Control`
    (`public, max-age=31536000, immutable` for a constant-etag tile, else `no-cache`).
  - `If-None-Match` → 304.
  - A key outside the world → 404.
  - The build queue at `MAP_BUILDS_QUEUED_MAX` → 503 with `Retry-After: 1`.
- The URL prefix carries the world id (`/map/<world id>/v1/…` at the proxy), so a re-forged world never
  hits an old cache.

- [ ] **Step 1:** `map_api::router(source: Arc<TileSource>) -> axum::Router` runs builds on a
  `MAP_BUILD_WORKERS`-thread blocking pool behind a bounded queue.
  - `refresh_versions` runs on the replica's existing per-cut notice (the terrain LISTEN) and every
    `MAP_REVALIDATE_MS`.
  - Metrics on `/metrics` (role `coordinator`): `mmo_map_tiles_built_total`, `mmo_map_build_ms`
    (summary), `mmo_map_cache_bytes`, `mmo_map_requests_total{code}`.
- [ ] **Step 2:** The coordinator mounts it on its HTTP listener. The map routes do not sit behind
  the service-token layer, which must still guard every other route. Check with a temporary test: no
  token → 200 on `/v1/map/…`, 401 on `/v1/hosts/…`. Delete it.
- [ ] **Step 3: Proxy.** nginx `location /map/` → the coordinator Service's map routes over HTTPS
  (pin: the coordinator's identity, as the module wires the coordinator certificate; check
  `coordinator.tf`).
  - `limit_req_zone $binary_remote_addr zone=map:10m rate=60r/s; limit_req zone=map burst=240 nodelay;`.
  - Forward ONLY `/map/<id>/v1/map/tiles/`.
- [ ] **Step 4: Login.** Add the column `map_url` and `world add --map-url`. `WorldEntry.map_url` is
  sent to clients. The native client's `MapSource::Http { base }` comes from it; the browser uses
  `{origin}/map/<world id>`.
- [ ] **Step 5: Dev server.** Role `all` with `VOXEL_MAP_BIND=127.0.0.1:6470` (the `server` recipe
  sets it) serves the same router, unauthenticated, loopback only. A client launched with
  `VOXEL_SKIP_MENU=1` uses `http://127.0.0.1:6470`.
- [ ] **Step 6: Twin.** One `twin_check.sh` phase after the forge: `curl` a level-`top` tile through
  the web proxy → 200 with an `ETag`; the same with `If-None-Match` → 304. Run `just twin-check`
  (say so first: it takes the machine).
- [ ] **Step 7: Hand check (ask first):** `just coordinators=2 dev-r3 2` with the dev web page.
  - The whole world shows at block level.
  - Kill one coordinator replica: tiles keep loading.
  - Dig, wait one save, zoom out to level 2: the edit shows.
- [ ] **Step 8:** `just smoke`; `just web-check`; `cargo tree -i aws-lc-rs` prints nothing. Commit
  (`feat(map): tiles over HTTP from the coordinator's pods; the dev server's map listener`).

### Task 12: Clean-up, docs, closing

**Files:** `crates/game/src/protocol.rs`, `crates/game/src/world_map.rs`, `crates/game/src/world_manager/mod.rs`, `crates/worldgen/src/tile_map.rs`, `crates/client/src/instance.rs`, `CLAUDE.md`, `TODO.md`, the spec ("As built").

- [ ] **Step 1: Region-spawn leftovers.**
  - The server no longer handles `ClientPacket::PlayRegion`; the variant stays in its append-only
    enum, unsent.
  - Remove `play_region`, `PlayRefusal`, `ServerPacket::PlayRefused`'s sender, `tile_map::play_blocker`
    and the client's `PlayRefused` log arm.
  - `ServerPacket::PlayRefused` stays as an unsent variant.
- [ ] **Step 2:** In `CLAUDE.md`, under Client, add one bullet on the map: the pass, `MapView`, the
  two sources, the reset rule, the shader compiled in. Under Server / store, add one line on
  `chunks.top` ("written in the chunk's own statement; never a separate row").
  - In `TODO.md`: departure 4 (canopies), and the parked selection screen now has its map. Spec:
    "As built".
- [ ] **Step 3:** `grep -rn "rasterize(\|MAP_TILE_PX\|minimap-base\|map-image\|play_blocker\|PlayRefusal" crates`
  returns only doc lines that say "removed".
- [ ] **Step 4: Closing runs (say so first).** Run `just test-full`, `just clippy` and
  `just determinism-wasm`. Then request the whole-branch review (superpowers:requesting-code-review).
  Give the reviewer the seams explicitly:
  - store ↔ cut part;
  - the client source ↔ the session reset;
  - the pass ↔ the Slint layers' pointer ownership;
  - the HTTP route ↔ the service-token layer.
- [ ] **Step 5:** Commit (`docs(map): one zoomable block-level map — as built`).

## Not in this plan

- Fog of exploration, waypoints, other players on the map, map travel, the political layer, caves.
- A browser disk cache beyond the HTTP cache. A login-issued map token.
- Moving the selection screen out of the world (its parked note). The map is ready for it.

## Self-review notes

- **Spec coverage:**
  - §5.1 → Tasks 1, 3, 4; §5.2 → Tasks 2, 4, 11; §5.3 → Tasks 5, 9, 10; §5.4 → Tasks 6, 7;
    §5.5 → Tasks 7, 8, 9; §5.6 → Tasks 7, 9, 12; §6 risks → Task 0.
  - §8b: no ownership read (Task 4 reads versions and chunks only), and the migration ordering
    (`0021`; recheck the next free number at Task 2 — the HTTP plan or Elastic 5 may have taken it).
- **Names used across tasks**, each defined once: `MapTileKey`, `MapTile`, `MapSample`, `ChunkTop`,
  `chunk_top`, `region_tops`, `surface_tile_forged`, `TileSource::{tile, refresh_versions}`,
  `Fetched`, `MapSource`, `MapTiles::{want, best}`, `MapPalette`, `MapView`, `map_api::router`.
- **Unverified while planning** (the implementer checks first):
  - whether the client sees its own cut's commit (Task 5, Step 3);
  - the wasm fetch helper's name (Task 11);
  - how the web image reaches the coordinator's HTTPS with its pin (Task 11, Step 3);
  - where `SiteMarker`s are built (Task 8).
