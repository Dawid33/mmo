# In-game menu, world map, and "Play" on a region tile — design

Date: 2026-10-29. Status: draft for review.

## Intent

The game is heading Dwarf Fortress–style: one big world generated up front, and a
player plays one region of it from the commander camera. Today every player
lands in region (0,0) and can never be anywhere else. This feature gives the
player the world as a map — a grid of one tile per region, drawn by the client
from per-region data the server sends, like Dwarf Fortress's embark map — and lets them pick a tile and
**play** it: their session moves to that region and the commander camera now
lives there.

It is deliberately *not* embarking. Embarking (choosing a party, supplies,
founding a fortress) is future work; so is the launch-time main menu and
adventure mode (walking between tiles). This is the plumbing those will build
on: a menu, a map, and "move my session to that region".

## What the player sees and does

- **Esc** opens an in-game menu. It does **not** pause the game (the sim keeps
  running; it is multiplayer and the server never pauses for one client).
  The menu has a **World Map** button and a **Close** button (Esc again also
  closes it). *My call:* nothing else in the menu yet.
- **M** opens the world map directly, from anywhere in commander mode, without
  going through the menu. M or Esc closes it.
- **The world map** is a window drawn over the game view:
  - A grid of tiles, **one tile per region** (`REGION_SIZE`; at the 256 m
    play size a 16 km world is ~64×64 tiles), Dwarf Fortress–style. The
    **client draws each tile itself** from that region's summary data (below)
    — colour from elevation and water (sea depth, sand, grass→rock→snow, the
    same ramp family `just world-map` uses), shaded by relief, river/lake
    marks — with grid lines between tiles. It is not a continuous relief
    image cut into squares; a tile is one cell of the map.
  - **Zoomable** (mouse wheel, centred on the cursor) and **pannable** (drag).
  - Hovering highlights a tile; clicking **selects** it (one at a time).
  - The player's current region is marked distinctly from the selection.
- **The side panel** (part of the map window) shows whatever is known about
  the selected tile. There isn't much today; show what exists:
  - region coordinates, and "you are here" if it is the current region;
  - elevation: min / mean / max ground height in the tile (m above sea level);
  - biome: the forge has no biomes. Show a provisional terrain class derived
    from height and water — *my call:* Ocean / Coast / Lowland / Hills /
    Mountains with thresholds as named constants — labelled so it is clearly
    a stand-in until real biomes exist;
  - water: rivers in the tile (count of reaches; largest by discharge/width)
    and lakes (present, and level);
  - surface rock: the dominant rock formation at the surface, from the
    forge's geology.
  - Empty / unknown fields are omitted, not shown as "--".
- **Play** button in the side panel moves the player's session to the
  selected tile. *My call:* disabled for the current region and for tiles
  with no dry land (all-ocean); disabled with a reason line, not hidden.
  Pressing it closes the map.

After Play, the player is in the new region: the near window and LOD rings
re-centre there, the commander camera re-centres on the new region (not
clamped to whatever edge it was nearest), the minimap and HUD show the new
region. The old region parks and pauses once nobody is in it (below).

## Rules that come from the user

- UI is **Slint** with the **existing widget library** (`../ui` `widgets`,
  Factorio style), in `crates/slint-ui` — per the standing rule that all
  player-facing UI is Slint. No bevy_ui, no egui for this.
- The menu does **not** pause.
- The button is **Play**, not Embark.
- A region with no players connected **pauses on the server**. This is what
  parking already does (see constraints): an unsubscribed region parks after
  `UNLOAD_GRACE_MS` (5 s), its thread stops, no ticks pass, and it resumes at
  the same tick when someone comes back. Nothing new is needed beyond making
  sure the old region is released.
- **Multiple players may share a tile** — technically allowed, nothing
  prevents it (two Play presses on the same tile put both players there).
- **Fortress players do not move between tiles by walking.** Play on the map
  is the way to change tile. Walking across region borders (the existing
  boundary handoff) is adventure-mode territory, future work. *My call:* this
  feature does not remove or disable the existing handoff code — the
  commander-mode body is inert and hidden, so it never walks anyway; first-
  person debug modes keep working as they do.

## Constraints and how the engine is today

Facts from the code (2026-10-29) the builder needs:

- **World data lives on the server only.** The forged world
  (`worldgen::forged`, `WorldArtifact`: height / lake_level / geology rasters,
  river reaches, lakes, spawn) is loaded by the server (`server::world_file`,
  `forged::install`). The client never has the world file.
- **The server sends data, the client builds the map** (user rule). The
  server sends one **summary per region tile** — the facts the tile is drawn
  from and the side panel shows (elevation min/mean/max, a relief/slope
  figure for shading, water: sea fraction / dry-land flag / lakes / river
  reaches with their largest discharge and width, dominant surface rock).
  It sends no image. The client turns summaries into the drawn map: colour,
  shading, terrain class, grid, labels. *My call:* the colour ramp and
  terrain-class rule live in `worldgen` as engine-free functions of a
  summary (the client already depends on `worldgen`), so the colouring is
  defined once; `examples/world_map.rs` keeps its own per-pixel image and
  may reuse the same ramp.
- **Viewing a tile never generates its region** (user rule). Summaries are
  computed from the world **recipe**, never from `generate_region`, a region
  runner, a snapshot or a parked blob: for a forged world, straight from the
  artifact rasters (height, lake_level, geology) and the river/lake lists
  over the tile's footprint (the default world is 16 m cells → 256 raster
  cells per 256 m tile, ~1 M cells for the whole map — milliseconds). They
  describe pristine terrain; player edits don't show on the map (fine — the
  map is a world map, not a live view).
- **Artifact ↔ engine coordinates:** the artifact's `spawn` maps to block
  (8,8) of region (0,0) (`forged.rs` `ENGINE_SPAWN_M`, `to_artifact`). Region
  tiles are therefore the *engine* region grid laid over the artifact, not
  aligned to the artifact's origin; tiles that hang off the artifact's edge
  are partly ocean (`OUTSIDE_HEIGHT_M`). The map covers every region tile that
  intersects the artifact.
- **Procedural world** (`VOXEL_WORLD_FILE=procedural`, tests, wasm offline):
  there is no finite world. *My call:* the map is forged-only; with no world
  installed the map window shows "No world map (procedural world)" and the
  menu button still opens it. Revisit if procedural play matters.
- **Existing colouring to reuse:** `crates/worldgen/examples/world_map.rs`
  (`base_color`, `ramp`, the hillshade sun vector). Lift the ramp into the
  `worldgen` library for the client's tile colouring.
- **Sending it:** a new `ServerPacket` carrying the grid extent (which region
  coords the summaries cover) and the per-tile summaries, sent once after
  connect; ~4k tiles of a few dozen bytes each, lz4 via `encode_packet` like
  every other packet. Computed once when the world is installed and kept by
  the server. *Left to the builder:* whether the client requests it or the
  server pushes it, and the exact struct shape. It is static for the life of
  the server's world.
- **There is no "move my session to region R" path today.** Spawn is always
  `SPAWN_REGION = (0,0)` (`world_manager.rs`); players change region only by
  the boundary handoff, which only ever reaches a neighbour. What exists and
  is reusable for any distance: `ensure_running(target)` (wakes a parked
  region or generates a new one), the `homes` map (input routes by it),
  `ServerPacket::PlayerRegion` (the client re-homes on it), `refresh_keepalive`
  (starts the old region's park timer). Clients must not be able to inject
  `EntityArrived` (`world_manager.rs` blocks it), so the request must be a
  new `ClientPacket` handled by the manager, not a client-sent sim event.
- **The move must be deterministic on both sides.** Removing the player from
  the old region and creating it in the new one must happen through
  server-authored sim events that the client's predicted copy of each region
  also applies (the way `CreateClient` and `EntityArrived` flow today), not by
  out-of-band surgery on one side. *My call on shape:* the manager sends the
  old region a server-authored "player leaves" event (removes the entity; no
  bundle to carry — it is a fresh start, not a carried body) and the new
  region a `CreateClient`-style event; then updates `homes`, pushes
  `PlayerRegion`, refreshes keep-alive on both. `Quit` is NOT that event — it
  stops the server. *Left to the builder:* reuse `CreateClient` as is vs. a
  variant, provided the result below holds.
- **Landing point** — *my call (settled after review):* the **manager**
  picks the landing column, not the sim, and carries it in the server-authored
  create event (a payload is as deterministic as a sim-side search). It
  searches outward from the tile's centre over the world recipe (forged
  artifact via `worldgen`, the same data the tile info uses) for the nearest
  dry land column, using **one shared definition of "dry"** with the panel's
  "has dry land" flag, so a tile that offers Play always has a landing column.
  No column found → the manager refuses the Play up front (reply to the
  client, nothing changes) — it never sends the leave event first and finds
  out later. In the region, the create event places the body on that column
  with the existing single-column surface lookup + clearance
  (`create_player_safe`'s rule), so no spiral search runs in the sim. In
  cell mode the runner already loads the cells under the arrival point
  before applying the event; it must use the event's column for that, not
  `spawn_xz()`. Today `spawn_xz` returns the legacy constant (block 8,8)
  outside cell mode; first-connect spawn in (0,0) is unchanged.
- **Player state on Play** — *my call:* Play is a fresh start in the new
  region, like a first join there. The leave event removes both the body and
  the client's `GameData.clients` entry (inventory, input state) from the old
  region, undoably; the create event seeds a fresh `Client` entry exactly as
  `CreateClient` does today. Nothing carries over and nothing is left
  behind, so playing A → B → A is fresh on A again. Carrying an inventory
  between regions belongs to embarking (future work).
- **Client side of a far home change already mostly works**
  (`instance.rs`): `PlayerRegion` sets the home, `update_window` requests the
  new home's snapshot, `recentered` jumps the window to the owning region once
  the body is readable, the old window is released, LOD re-diffs. Known gap:
  for the frames between the home change and the new snapshot, input is
  dropped ("home region not loaded") and the window is still centred on the
  old place — acceptable; show a simple "Loading region…" line in the Slint UI
  while the home region has no snapshot.
- **Commander camera:** `track_home_region` only clamps the focus into the
  new region rect (`commander/mod.rs`), which after a far jump leaves the
  camera on an edge; `place_on_body` runs once. On a home change the camera
  must re-place on the new region (centre on the landed body, reset
  terrain-following height).
- **Cell mode** (`cells=`): the target is never preloaded with near radius 0,
  so the body is missing until the snapshot lands (documented phase-1b gap);
  cell regions never park. Play must work in cell mode with those known
  limits; nothing more is required of it here.
- **Input:** Esc and M are new client-side bindings in commander mode. While
  the menu or map is open the Slint overlay owns the pointer over its pixels
  (`CursorPolicy::ui_pointer`); commander pan/zoom/selection must not react
  to wheel/drag inside the map. Keys typed while the map is open must not
  also drive the camera (WASD pan) — *left to the builder* whether the map is
  modal for keyboard camera control. F9 keeps toggling the existing panel.
- **Slint renders via the software renderer** into one window-sized texture
  (`slint_overlay.rs`). A full-window map redraws a large dirty rect on pan/
  zoom; that is acceptable for a menu screen. *Left to the builder:* whether
  the client rasterises the tiles into one `slint::Image` (one pixel block per
  tile, rebuilt only when summaries arrive) or Slint draws ~4k rectangles;
  hover and selection are drawn by Slint over it either way.
- Region edge is never a literal — derive from `REGION_SIZE` / `REGION_CHUNKS`.

## Goals

1. Esc menu (non-pausing) with World Map + Close; M opens the map directly.
2. Server sends per-region tile summaries computed from the world recipe
   (no region generated); the client draws a zoomable, pannable map of one
   tile per region from them, hover + select, current region marked.
3. Side panel with the available tile info (coords, elevation, provisional
   terrain class, rivers/lakes, surface rock).
4. Play moves the session to the selected tile: deterministic leave/create
   events, landing on dry ground, old region released (and so parked/paused),
   client re-homes, camera re-places.
5. Tests: per-tile summary computation (worldgen, pure, from a small
   synthetic artifact — and asserting no region is generated); tile colour /
   terrain class from a summary (worldgen, pure); the manager's play
   request (game, `InlineSpawner`: home changes, old region parks after
   grace, new region has the player on dry ground, client/server state hashes
   agree); the Slint component builds and its callbacks fire
   (slint-ui headless). Run `just test-threads` (touches `WorldManager`) and
   `just determinism-wasm` if sim events change.

## Non-goals

- Embarking (party, supplies, founding), a launch-time main menu, save/load,
  adventure mode / walking between tiles, real biomes, fog of war or
  discovered-only map, other players shown on the map, map markers.
- Pausing the sim.
- Persisting a player's chosen region across server restarts (`homes` is in
  memory; a reconnect keeps it while the server runs, as today).
- The browser (wasm) build: the Slint overlay is commander-mode native-only
  today; whatever that implies for wasm stays as is.

## Anti-goals (would make it a failure even if it "works")

- Client and server disagree about where the player is after Play (desync,
  hard-fail on a state hash) — the move must be sim events, not side-channel.
- The player lands buried, in the sea, or in a lake.
- The old region keeps ticking forever after the last player left it.
- The server renders the map for the client, or sends an image.
- Opening the map (or the server preparing it) generates regions.
- A UI built outside Slint / the `widgets` library.

## Left to the builder

Packet and struct shapes; exact summary fields beyond those listed; zoom
limits and step; how the
dry landing column is searched; terrain-class thresholds (named constants);
whether the map blocks keyboard camera control; layout details within the
Factorio widget style; exact `[perf]` events (add one per Play: request →
home confirmed → new region loaded, per the per-event volume rule).
