# One zoomable map at block resolution — investigation and design

**Status:** proposal, 2026-11-19. Nothing is built. The approach (section 4) and the open decisions
(section 9) need the owner's word before a task-level plan is written. Section 5.2 was revised the
same day: hosts write a surface row per region and a stateless HTTP reader serves tiles (beside the
coordinator's HTTP API), instead of the home host building and pushing them.

## 1. What is asked

One map, zoomable from the whole world down to one pixel per block (1 m), as the map mods of modded
Minecraft show it. It replaces:

- the minimap (HUD, bottom-left),
- the in-game world map (M, Esc menu),
- the map shown at the start of a world (the arrival map with the Adventure tab), where a site is
  chosen — players spawn in sites now, not in regions.

## 2. What exists today

| Surface | Code | Data | Resolution |
|---|---|---|---|
| Minimap | `renderer/commander/minimap.rs`, `hud.slint` | `HeightGrid` (the near window's block tops, heights only — the block id is thrown away in `chunk_tops`) | 256² px over the HOME REGION only, not centred on the player; 1 m/px at 256 m regions; colour by height ramp |
| World map (in game and at arrival: the same component) | `renderer/world_map.rs`, `world-map.slint`, `slint_overlay.rs` `sync_world_map` | `ServerPacket::WorldMap` (`game::WorldMap`, one `TileSummary` per region), sent once at admit | one tile per region; `tile_map::rasterize` at 8 px per tile; zoom ×0.125…8 in powers of two only |
| Site choice | `arrival.slint`, `arrival_tabs.rs`, `history_view.rs` | `ServerPacket::Sites(Vec<SiteMarker>)`; a marker has a region, no position | a square on the site's region tile; `StartAdventure(SiteId)` |

Facts that shape the design:

- **Every runtime image goes CPU → `slint::Image` → Slint's software renderer → a window-sized Bevy
  texture.** A big image is scaled on the CPU at every redraw; that is why the zoom is powers of two
  only, and why the HUD re-uploads its whole texture on any change. A 100 km world is 100,352² px at
  1 m — this path cannot carry it.
- **Block-scale surface data already exists on the server, edits included, for any place in the
  world.** `LodBuildJob::Tile` (factor ≥ 16 = 1 block) is `worldgen::coarse_tile` — a point sample per
  column, no full generation — overlaid with the regions that have authoritative or stored terrain;
  `invalidate_region_lod` re-builds on `TerrainChanged`, on park and on another host's committed cut
  (`stored_terrain_changed`). `lod_subscribe` has no ownership or distance check: a home host answers
  for another host's regions from the store.
- `worldgen::point_sample` gives, per column: surface height, water top, ice, biome ground cover,
  whether trees stand there. It is private today; `coarse_tile` is built on it.
- **No per-block colour exists**, but `block_textures::mip_chain` already box-filters each texture
  layer down to 1×1 in linear light — the top face's last mip IS the block's map colour.
- No client-side cache of world data exists on any platform (native has `data_dir()`, the web has
  only `localStorage`).
- The parked selection-screen research (`2026-11-19-adventure-selection-screen-research.md`) wants
  the site choice to happen with no world behind it, the map coming from the coordinator.
- Leftovers of the region spawn are still on the wire and the server (`ClientPacket::PlayRegion`,
  `ServerPacket::PlayRefused`, `play_region`, `tile_map::play_blocker`); the client side is already
  removed in the owner's uncommitted tree.

## 3. Goals, non-goals, anti-goals

**Goals**

1. One map component and one data path behind the minimap, the full-screen map and the site choice.
2. Continuous zoom from the whole world to 1 m per pixel; smooth pan; 60 fps on the Iris Xe.
3. Block level shows the real surface: block colours, water, relief shading, player edits and
   buildings (a town looks like a town).
4. Sites are marked at their real position and chosen on the map.
5. Works in the browser build and in a multi-host world, with no new connection kind.

**Non-goals** (this design): underground / cave layers, waypoints, other players' positions, map
travel, the political layer of the town-ranks spec (it becomes an overlay of this map later), moving
the site choice out of the world (the parked spec; section 8 says how this design serves it).

**Anti-goals**

- Any cost in the simulation: no map work on a region thread, nothing hashed, nothing in a cut.
- A second, different picture of the same place (minimap and map must be the same tiles).
- A map whose pan or zoom re-rasterises the screen on the CPU.

## 4. Approaches

### A. Tiles built by the server, drawn on the GPU — recommended

The server builds **surface tiles** on demand (by a stateless reader, section 5.2): for a
tile of 256 × 256 samples at zoom level `k` (one sample per 2^k m), per sample the surface height,
the top block and the water depth — worldgen's point sample where terrain is pristine, the stored or
running region's real top block where it is not. The client asks for the tiles its view needs, keeps
them in memory and on disk, and draws them as textured quads in a Bevy pass; a shader turns
(height, block, water) into colour and relief. Slint keeps the frame, the buttons, the markers and
the panels.

- For: the whole world at block level, also where nobody has been (the site choice needs exactly
  that); edits appear; one source for all three surfaces; pan and zoom cost nothing on the CPU;
  layers (relief, biome, temperature…) are a shader switch.
- Against: a new stored row per changed region and a tile route with its cache; server CPU per new
  tile (to be measured first — section 7, step 0); a new render pass under the Slint layers.

### B. Explored-only, client-built (what the Minecraft mods do)

The client paints tiles from the chunks and LOD data it happens to receive and keeps them on disk;
everything else stays at the region-tile map.

- For: no server work at all; fog of exploration for free.
- Against: the site choice — the map's first use — would show nothing at block level, since the
  player has been nowhere; a browser client has no disk; two players see different maps of the same
  town; LOD payloads are 3-D grids held only while subscribed, an awkward source.

### C. Server tiles drawn through Slint (CPU composition)

As A, but the client composes the visible view into one window-sized buffer and hands it to Slint.

- For: no new render pass.
- Against: about three full-screen copies per frame while panning (compose, Slint's raster, the layer
  texture upload); no smooth zoom; the minimap would repaint the HUD texture every frame the player
  moves. It keeps the limitation that makes today's map coarse.

**Recommendation: A.** B fails the site choice, C fails goal 2. Exploration fog, if wanted, is a
visibility rule on top of A (decision 1 in section 9), not a different data path.

## 5. Design (approach A)

### 5.1 The tile pyramid

- World block coordinates, north up. A tile is `MAP_TILE_SAMPLES` = 256 samples square. Level 0 is
  1 m per sample; level `k` is 2^k m. `MapTileKey { level: u8, x: i32, z: i32 }`, the tile covering
  blocks `[x·256·2^k, (x+1)·256·2^k)`. The grid is NOT the region grid: a tile's regions are derived
  from `REGION_BLOCKS` (at 64 m test regions a level-0 tile spans 16 regions), never a literal.
- The top level is the first at which the world fits one tile: a 100 km world is 392 tiles a side at
  level 0 and one tile at level 9 (512 m per sample). The world's bounds come from the `WorldMap`
  the client already receives.
- **A level is sampled directly, not averaged from the level below**: worldgen is a function of the
  position, so a level-5 tile is 65,536 point samples at a 32 m stride — every level costs about the
  same per tile, and no level depends on another. Regions with real terrain are overlaid from
  their stored surface row (5.2), whose mips serve levels 0–3; above 8 m per sample edits are not
  overlaid (a call, section 9).
- A sample: `height: i16` (m), `block: BlockId` (top visible block, a tree's canopy included where
  real chunks exist), `water: u8` (depth in m, saturating; 0 = dry), `flags: u8` (ice, pristine /
  real). Raw 6 bytes × 65,536 = 384 KB; served as three planes, each delta-coded then lz4 through
  `encode_packet` — expected a few tens of KB per tile, to be measured.
- Pristine tree canopies at level 0–1: worldgen's tree placement is a function of position too
  (`TreeSource`); the builder stamps crowns the way `coarse_from_patch` does. If that proves dear,
  pristine forest falls back to a per-biome canopy tint (builder's choice, measured in step 0).

### 5.2 Server: hosts write a surface per region, a stateless reader serves tiles over HTTP

Revised 2026-11-19 after the owner's question and `docs/superpowers/plans/2026-11-19-coordinator-http-api.md`
(the coordinator becomes an axum HTTP service whose mailbox handlers read the store directly, with no
core). The first draft had the home host build tiles and push them over the game connection; that is
replaced by this.

**Written by hosts — one surface row per region.**
- Table `region_surface (rx, rz, terrain_version, data)`, keyed per region. The owning host writes a
  region's row in ITS OWN cut part, fenced with the part's other rows, and only when the region's
  terrain changed since its last committed cut. It is never written by two hosts' parts (a map TILE
  would be: the tile grid is not the region grid, and coarse tiles span hosts — so hosts never write
  tiles).
- `data`: per block column the top block's height, `BlockId`, water depth and flags (`REGION_BLOCKS`²
  samples), with its 2× mips down to 8 m, delta-coded and lz4'd. Not hashed; never built on a region
  thread.
- **Invariant: a region's surface row is written by the same part that writes that region's changed
  chunks, and from exactly those chunks** — the manager re-derives only the columns of the chunks
  the part carries and re-encodes the row. So the row always matches the stored terrain, whatever
  happens to the process afterwards (a crash, a succession, a move of the land: the next owner loads
  chunks and surface of one cut), no "dirty" flag has to survive a host, and the work is proportional
  to what changed. That matters since Elastic 5: a move's cut is a pause the players on that land
  feel, and this adds to its part only the columns edited since the last cut.
- A region that never ran has no row: its surface is worldgen's, computed by the reader.
- Works in a one-phase cut too (static, all-in-one, SQLite): it is an ordinary row of the part.

**The reader — one crate, two thin fronts** (owner, 2026-11-19: single-player reads its SQLite store
directly, no HTTP; keep the code from being duplicated).
- `crates/map-tiles` (new; depends on `game`, `worldgen`, `world-store`; no Bevy, no axum):
  `TileSource::new(store: StoreHandle /* OpenMode::ReadOnly */, world: Arc<ForgedWorld>, mats)` and
  `TileSource::tile(key, have: Option<Etag>) -> Unchanged | Tile { etag, bytes }`. Inside:
  `worldgen::surface_tile_forged(&world, key, mats)` (the public form of `point_sample` over a patch,
  taking the world EXPLICITLY — `forged::install` is a once-per-process global and a client plays
  several worlds in one run), then every `region_surface` row of the tile's regions over it; the
  `Etag` from those regions' `terrain_version`s; the LRU (`MAP_TILE_CACHE_BYTES`). It needs the
  forged world and read access to the store; **no core, no host, no coordinator state**. The tile
  type, key and codec are `game::map_tile`.
- **Front 1, multiplayer — `server/src/map_api.rs`:** an axum handler around `TileSource::tile`.
- **Front 2, single-player — the native client calls `TileSource::tile` itself**, on its compute
  threads, against the world's `terrain.sqlite` opened `OpenMode::ReadOnly` (the mode made for
  "tools reading a store a server is using": no lock, no migration, WAL lets it read beside the
  child server's writes) and the world's `terrain.world`. No listener, no port, no token.
- The client hides the two behind one seam, `MapSource::{Local(TileSource), Http(base_url)}` with
  the single operation `fetch(key, have_etag)`; everything after it — revalidation timing, the memory
  cache, decode, textures, the pass — is the same code. `Local` skips the disk cache (the store is
  the disk). The browser build has only `Http` (`map-tiles` is a native-only dependency of the
  client).
- The route, shaped like the HTTP plan's mailbox read (a store read in the handler):
  `GET /v1/map/tiles/{level}/{x}/{z}` → `200` body `encode_packet(MapTile)`,
  `ETag` = a hash of the tile's regions' `terrain_version`s (a constant for a wholly pristine tile),
  `If-None-Match` → `304`. Pristine tiles are `Cache-Control: immutable` for the world's life (the
  world id and map version are in the URL prefix); tiles with real regions revalidate.
- Builds run on a capped blocking pool (`MAP_BUILD_WORKERS`), behind an LRU (`MAP_TILE_CACHE_BYTES`)
  whose entries are checked against the regions' `terrain_version`s at each hit (one small read; the
  per-cut NOTIFY is then only an optimisation, and SQLite needs none); requests beyond
  `MAP_BUILDS_QUEUED_MAX` get `503` + `Retry-After` (the client shows the ancestor tile meanwhile).
- **Where it runs:** the router is mounted on the coordinator's HTTP server once that exists (HTTP
  plan, Task 3) — in a route group of its own. Because it needs no core it can move to a Deployment
  of its own (`VOXEL_ROLE=map`, the same image) if its CPU ever competes with transitions; nothing
  else changes. A menu world and a debug launch (`just run`, `just play`) use front 2. The bare dev
  server (`just server` + `just client`, role `all`, no world folder known to the client) mounts
  front 1 on a loopback port when `VOXEL_MAP_BIND` is set, with no auth — the same router, a dev
  dial only.
- **Client routes are not the service routes.** The HTTP plan's listener trusts the service token and
  a pinned self-signed certificate and sits behind a ClusterIP Service: none of that may face
  players. The map group takes the player's login JWT (verified against login's JWKS, as a `jwt` host
  does), and reaches players through the `web` Deployment's proxy exactly as `/login` does — it
  forwards ONLY `/v1/map/` — so the browser gets it same-origin under the real certificate. Native
  clients use the same URL.
- **Discovery:** the map's base URL is `{page origin}/map` in the browser; natively it comes with the
  world's entry in the login service's world list. A local world's session already knows its folder
  (`session.rs`): that selects `MapSource::Local`. No game packet is added for tiles.
- `SiteMarker` gains `at: [i32; 2]` (world block of the site's landing: region origin +
  `TileSummary::landing`), so a marker sits on the town, not on a region square.
- Metrics on the serving process's `/metrics`: `mmo_map_tiles_built_total`, `mmo_map_build_ms`,
  `mmo_map_cache_bytes`, `mmo_map_requests_total{code}`.

**Consequences.** Everything outside the client's near window is as of the last committed cut (the
home host's own regions too). `WorldManager` gets no builder, cache, subscriber set or invalidation
hook for the map. Map load never lands on a host. The site choice can show a town at block level
with zero hosts running.

### 5.3 Client: data

- `renderer/map/store.rs`: a `MapTiles` resource — memory LRU of decoded tiles and their GPU
  textures, the set wanted by the current views, the request pacing. Registered with
  `add_session_reset`, which clears it only for a different world (below).
- **The near window is painted locally and at once**: `HeightGrid` keeps the `BlockId` that
  `Chunk::column_top` already returns, and a system patches the level-0 tiles under the near window
  from it (a dug hole shows on the minimap at once; served tiles lag by up to a save interval).
  Served tiles fill everything else. A locally painted tile outlives its chunks: it stays until a
  served tile with a newer `ETag` replaces it — so nothing un-digs or blanks when regions unload,
  when predicted regions restart from another host's snapshot (a home hop, Elastic 5's move), or
  during a move's pause.
- The tile cache belongs to the WORLD, not the session or a connection: a home hop, a drain's rejoin
  or a move changes nothing in it (it is cleared only when another world is entered).
- Decode (lz4, delta) is a `renderer::compute::JobKind` carrying its bytes — workers on wasm.
- Fetching is HTTP (`reqwest` natively, `fetch` in the browser), a few requests in flight, nearest
  the view's centre first. Disk cache, native: `data_dir()/map/<world id>/<level>/<x>_<z>.tile` with
  the `ETag` beside it, capped by total size with oldest-used eviction, revalidated with
  `If-None-Match` when a shown tile with real regions is older than a save interval. Web: the
  browser's own HTTP cache does the same with no code.
- A `BlockId → RGB` palette cut from `block_textures::mip_chain`'s last level of each top face
  (cutout blocks: the opaque texels' mean), uploaded as a 1-D texture.

### 5.4 Client: drawing

- A `MapView { centre: DVec2 (blocks), metres_per_px: f32, rect: screen rect, rotation }` component
  per shown map: one for the minimap, one for the full map. Each picks the level whose sample size
  is nearest below its `metres_per_px`, wants the tiles its rect covers plus one ring, and draws the
  best loaded ancestor until a tile arrives (never a hole).
- A render pass in the slot just before `slint_composite` (same camera, `UiCamera`): for each
  `MapView`, scissor to its rect, one instanced quad per tile, textures `R16Sint` height, `R16Uint`
  block, `R8Unorm` water. `assets/shaders/map_tile.wgsl` (compiled in, as the other terrain shaders):
  palette lookup, water tint by depth, hill shading from the height gradient, contour hint at low
  zoom, nearest sampling at ≥ 1 px per sample and linear below. Thematic layers (biome, temperature,
  precipitation, regions) are the existing `WorldMap` rasters at 1 px per region tile in a second
  texture, blended by a `layer` uniform — `tile_map::rasterize`'s 8 px-per-tile image goes away.
- Slint draws over it: the map's rect in `world-map.slint` and the minimap's in `hud.slint` become
  transparent holes with the frame, buttons, labels, site and player markers on top. Marker positions
  are pushed in screen pixels by Rust from the `MapView` (Slint never scales a map image again), so
  the zoom is continuous and the powers-of-two rule goes.
- Input stays where it is (the Slint `TouchArea`: drag, wheel about the cursor, click → a world
  block), reporting to Rust, which owns `MapView`.

### 5.5 The three surfaces

- **Minimap**: a `MapView` centred on the player (the commander camera's focus in the mayor's view),
  north up, default 1 m/px over 220 px, wheel-zoomable 0.5–8 m/px (decision 2: rotating with the
  view or not). The commander's click-to-move, entity dots and camera footprint keep working — drawn
  by Slint / the overlay as today, placed through the `MapView` transform.
  `minimap::screen_rect` stays the one answer to "is the pointer over the minimap".
- **Full map (M)**: opens centred on the player at the minimap's zoom; zooms out to the whole world.
  Tile facts (`tile_info_lines`) stay, now for the region under the cursor; plus the block
  coordinates and height under the cursor.
- **Site choice (arrival)**: the same full map, modal, with the Adventure tab beside it. Site markers
  at their positions; clicking a marker or a list row selects the site and eases the view to it at a
  zoom where the town is readable (~1–2 m/px); Start is unchanged (`StartAdventure(SiteId)`). No
  "Play a region" returns. The debug launch and `can-leave` / "Main menu" of the owner's uncommitted
  work are kept as they are.

### 5.6 Removed

`tile_map::rasterize` and `MAP_TILE_PX` (the offline `just world-map --tiles` moves to the 1 px
raster), `rebuild_minimap_base` and its 256² buffer, the `map-image` / `minimap-base` Slint
properties, and — with the owner's word, since it is beyond the map — the region-spawn leftovers
(`PlayRegion`, `PlayRefused`, `PlayRefusal`, `play_region`, `play_blocker`; wire variants stay as
unsent members of their append-only enums).

## 6. Risks, and what settles each

| Risk | Settled by |
|---|---|
| Tile build cost in the serving pods (a 1080p view at level 0 is ~40 tiles) | Step 0 measures `surface_tile` at levels 0, 3, 6, 9 in release, pristine and with overlays. The pool and queue caps bound it; if it competes with the coordinator's transitions the router moves to its own Deployment. |
| The surface row's size and build time inside a cut part (cuts are a critical path) | Step 0 measures both for a heavily edited region; written only for regions whose terrain changed. |
| The native client gains `world-store` (sqlx) as a dependency, and holds a second copy of the forged world's tile LRU beside the child server's | Step 0 measures the client's build time and the memory; `world-store` is already built in the workspace (feature unification), so the cost is link time and binary size. |
| A client-facing route beside service-token routes | Its own route group and auth (login's JWT); exposed only through the `web` proxy's `/v1/map/` rule. |
| A pass under the Slint layers on Bevy 0.19.1 | Step 0 spike: one hard-coded tile in the minimap's hole, `just pull-smoke`-style check on a real adapter, and in the browser. |
| Scraping the world by asking for every tile | The build queue cap and the LRU bound the cost; the surface is not secret (it is what LOD shows). Fog (decision 1) would bound it further. |
| Tile payload size on a slow link | Measured in step 0; the pyramid already shows an ancestor while a tile loads. |
| `SlintLayer` pointer ownership over the hole | The game UI already owns the pointer where it drew; the hole keeps a transparent `TouchArea`. |

## 7. Build order (each step leaves the game working)

0. **Measure and spike** (throwaway): `surface_tile` cost and encoded size; the surface row's size
   and build time for an edited region; one tile drawn under the HUD on the Iris Xe and in the
   browser. Decides tile size and the caps.
1. **The surface row**: the migration, the manager deriving it from a part's chunks, the part writing
   it. Independent of the HTTP plan; before Elastic 5's plan or after Elastic 5, not beside it (8b).
2. **The map pass** (client, independent of the HTTP plan): palette, shader, `MapView`, `MapTiles`
   with memory LRU, fed in this step by the near window's local painting and the `WorldMap` raster
   only; the full map (M) drawn by it at continuous zoom; the old `rasterize` path removed.
3. **Minimap on it**: player-centred `MapView`; commander pointer and footprint through the
   transform; the old base buffer removed. (Needs only local painting: it can ship before tiles.)
4. **The reader, single-player first** (independent of the HTTP plan): `game::map_tile`,
   `worldgen::surface_tile_forged`, `crates/map-tiles`, the client's `MapSource::Local`. The whole
   world is now visible at block level in a local world.
4b. **The HTTP front** — after the HTTP plan's Task 3 (the listener) at the earliest, in practice
   after its Task 7 (the module): `map_api.rs`, the `web` proxy rule and the world module,
   `MapSource::Http`, the dev server's `VOXEL_MAP_BIND`.
5. **Site choice on it**: `SiteMarker.at`, markers at positions, select → ease to the site.
6. **Disk cache** (native) with revalidation.
7. **Clean-up**: region-spawn leftovers (if agreed), CLAUDE.md, justfile recipes for any new
   measuring tool, a twin phase that fetches a tile through the proxy.

Verification per step: the touched tests and `just smoke`; `just determinism-wasm` is NOT touched by
this work (nothing hashed changes) but is run once at the end because `worldgen` and `game` gain
code; the looks are checked by hand with `just run` and `just web`. Tests follow the repository
rule: temporary ones while building, none committed unless a bug was seen.

## 8. How this serves the parked selection screen

When the site choice moves out of the world (no host connection until a site is picked), the map
already works: tiles come over HTTP from a reader that needs no host and no body in the world, with
the player's login JWT. With `fleet.min = 0` a town is visible at block level before any host starts.
The rest of that screen's data (sites, civs, legends, the claim) is that note's own work.

## 8b. Against the hosts work in flight (HTTP coordinator, Elastic 5)

Read 2026-11-19: `plans/2026-11-19-coordinator-http-api.md` and
`plans/2026-11-19-elastic-5-move-occupied-land-planning-handoff.md`.

- **The reader never looks at ownership.** A tile is worldgen plus `region_surface` rows; its `ETag`
  is terrain versions. Land that is assigned, released, inherited or moved (Elastic 5) changes
  nothing for it, and it needs no lock on `super_regions` or `hosts` — plain reads of rows that only
  a committed part writes. (To check at build: that a read of `region_surface` does not wait behind a
  staged part's row locks; a plain `SELECT` does not.)
- **The client's map does not ride a game connection**, so the home hops Elastic 5 makes common, a
  drain's rejoin and a succession cost it nothing. The first draft's per-connection subscriptions
  would have been re-asked at each.
- **Keyed by region, as Elastic 5 re-keys `in_flight`:** the surface row moves with its region's row
  in the giver's part; there is nothing to adopt.
- **Sequencing in the store.** Three pieces of work add a migration and touch the cut part
  (`rows.rs` `cut_rows`, `WorldCut`): the HTTP plan (`0020_mailbox`), Elastic 5 (`in_flight` by
  region, `WorldCut::moves`), and this (`region_surface`). The surface row is small and independent:
  build it either before Elastic 5's plan is written (so that plan reads it as existing) or after
  Elastic 5 closes — not beside it. Its migration takes the next free number at that time, identical
  in shape on both engines.
- **World side, movable to another cluster:** the reader reads only the world's database and is
  reached at a per-world URL; nothing of it lives with login or a future management cluster.

## 9. Decisions wanted from the owner

1. **Is the whole world visible at block level from the start, or only where the character has
   been?** Proposed: everything visible (as the world map is today, and as the site choice needs).
   Fog would be a per-player explored set kept by the server — a larger piece, and a game-design
   decision.
2. **Minimap: north up, or turning with the view?** Proposed: north up, a setting later.
3. **Edits on zoomed-out levels**: proposed overlaid down to 8 m per pixel (the surface row's
   mips), not beyond — a town shows as its marker there.
4. **Remove the region-spawn leftovers on the server and the wire** as part of this work? Proposed:
   yes, in the last step.
5. **The map is served over plain HTTPS, not QUIC / WebTransport.** The parked selection-screen note
   records "QUIC / WebTransport, not through the login service over HTTP"; the coordinator has since
   become an HTTP service, and HTTP gives tiles ETags and the browser's cache for nothing. Proposed:
   HTTP, through the `web` proxy.
6. **Mounted in the coordinator's pods first**, moved to its own Deployment only if measured CPU
   says so? Proposed: yes.
7. **Staleness**: far terrain on the map lags by up to one save interval, the home host's too.
   Proposed: accepted (near the player the client paints its own chunks).

Decided by the owner (2026-11-19): single-player reads the world's SQLite store directly; the tile
builder is shared with the HTTP route (5.2).

## 10. Measured (plan Task 0, 2026-11-20)

Release build, region 64 blocks, the dev Planet (`ForgeParams::default()`, forged in-process), one thread,
this box (capped `nice` + `taskset`); a scratch example over the committed `worldgen::surface` and
`game::map_tile`, deleted after. The owner chose CPU timings only: the draw spike (a pass under the Slint
layers on the Iris Xe and in the browser) was NOT run — it is first seen at Task 6's hand check.

| What | Median (20 runs) | Encoded |
|---|---|---|
| `surface_tile_forged` level 0 (one lattice patch) | 22.2 ms (20.6–24.5) | 25.8 KB |
| level 3 (exact point per sample, `PointPatch`) | 44.2 ms (39.4–46.2) | 47.9 KB |
| level 6 | 12.5 ms | 49.5 KB |
| level 9 (the whole dev world in one tile: few samples inside) | 1.7 ms | 3.5 KB |
| level 0, a coast | — | 17.8 KB |
| level 0, a mountain | — | 26.3 KB |
| `chunk_top` per generated chunk (37 chunks in a 64-block region) | 25.1 µs | 28–193 B, mean 115 B (`FORMAT_CHUNK_TOP`) |

- **Stop rule:** a level-0 tile is well under 50 ms; 256-sample tiles stand.
- **Sizes:** the whole tile frame (three inner planes, lz4 again outside) is smaller than its height plane
  alone; a few tens of KB as expected. No plain tile (relief < 8 m, dry) was found on the middle row.
- **Pristine canopies (departure 4):** `coarse_tile_forged` at 1 block per cell, which stamps crowns among
  much else, costs 32.9 ms per 256 m square — more than a whole level-0 tile (22 ms), so stamping crowns
  is NOT a cheap follow-up (the plan's 25 % bar); a per-biome canopy tint stays the fallback.
- **Store cost:** a chunk's top adds ~25 µs and ~115 B per written chunk to a cut part.
- **Constants:** none is off by 2×; `MAP_BUILD_WORKERS` = 2 builds ~45–90 level-0…3 tiles a second.

## As built (2026-11-20)

Plan `docs/superpowers/plans/2026-11-20-block-level-world-map.md`, executed inline on `develop` (commits
`1fd885c7` … this docs commit; the ledger with every ruling: `.superpowers/sdd/2026-11-20-block-level-world-map/`).
Nothing was run in a window, on the twin or on orion: every hand check is owed (`TODO.md`).

| Part | Where | What differs from §5 |
|---|---|---|
| Tile types | `game::map_tile` | `MapTileKey::regions()` (not a free `tile_regions`: the root glob has one); `MapTileDecodeError`; `top_level` = the first level whose tile edge covers the world's longer side (the grid is aligned at 0: a world across it never fits one tile). |
| Store | `chunks.top`, migration `0021_chunk_tops`; `StoreHandle::region_tops` | Plan departure 1: a chunk's own top beside the chunk, not a `region_surface` row. |
| Worldgen | `worldgen::surface` | Levels 0–2 read one lattice patch (level 0 is generation's top); levels ≥ 3 read each sample exactly (`forged::PointPatch`): a level-9 patch would hold ~10⁹ corners. |
| Reader | `crates/map-tiles` | Region tops cached per (region, version); the overlay re-takes water depth over the new top. |
| Client | `renderer/map/` (`store`, `palette`, `views`, `pass`, `full`, `paint`, `disk`, `http`), `map_tile.wgsl` | The full map's view is a transparent hole BESIDE a side-panel `FrameFace` (its base is opaque); the hole is `#00000001` (pointer ownership needs alpha > 0). Levels 1–3 are painted from the height grid directly, not downsampled. Site markers stand on their region's landing derived on the client (`SiteMarker` unchanged: no wire change). |
| Server | `server/src/map_api.rs` | Unauthenticated (owner). Also on a plain HTTP/1.1 port (`VOXEL_MAP_BIND`, 6470 in the pod): nginx cannot proxy to the API's HTTP/2. Every answer `no-cache`: the etag carries the forge's identity (a re-forge keeps the world id and URL). Keys refused above the top level + 1 and beyond one world period in x; server caches 32 MiB of tiles and 64 region tops (a 512Mi pod). |
| Deploy | `deploy/images/web/nginx.conf`, `deploy/modules/world/{coordinator,web,main}.tf`, `scripts/twin_check.sh` 8b | `limit_req` 60 r/s, burst 240, per client address; only GET tiles forwarded. |
| Login | `worlds.map_url` (migration 0005), `world add --map-url` | Also in the join ticket. |
| Removed | `tile_map::{rasterize, MAP_TILE_PX, MAP_GRID_PX, play_blocker}`, `rebuild_minimap_base`, `minimap-base`, `map-image`, `play_region`, `PlayRefusal` | `PlayRegion` / `PlayRefused` stay as unsent variants (`PlayRefused(RegionCoords)`). |

Pristine tree canopies are not drawn (plan departure 4; §10: stamping costs more than a level-0 tile); proposed
next: stamping on demand at levels 0–1 (TODO.md).
