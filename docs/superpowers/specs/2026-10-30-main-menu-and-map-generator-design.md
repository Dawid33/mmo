# Main menu, New World (map generator) and Load World — design

Status: 2026-10-30 — Dawid answered the open questions (see "Decisions"). Calls marked
**[my call]** were made by Claude without asking; everything else is from the request, from
Dawid's answers, or forced by the code as it is.

## Intent

The client today boots straight into a world: it connects to `127.0.0.1:6466` at startup and
whatever world that server was started with (`worlds/default.world`, forged on first start) is
the game. There is no way, from inside the game, to make a different world or pick one you
made before. This adds the front door a Dwarf Fortress–style game needs:

- a **main menu** with **New world**, **Load world** and **Exit**;
- **New world** opens a **map generator** screen modelled on Factorio's (reference screenshot:
  preset dropdown + seed box with a shuffle button, a short preset description, tabs of
  labelled slider rows, a large live preview on the right with an Auto-refresh checkbox and a
  refresh button, **Back** bottom-left, a wide green **Play** bottom-right), driving the existing
  world forge (`worldgen::forge`) instead of Factorio's noise settings;
- **Load world** lists worlds on disk; picking one starts that world and drops the player on
  **its world map** (the existing M map, `world-map.slint`) to choose the region to play.
  New world ends the same way: forge → world map → Play a region.

So both paths converge on the world map as the "where do I embark" step — the DF embark screen,
which is the right shape for fortress mode.

## Goals

1. Main menu is the first thing the native client shows. No server connection, no sim, no 3D
   world until a world is chosen.
2. New world: every forge knob reachable, grouped into tabs, with a coarse live preview that
   re-forges on change; seed box + randomise; presets; Play forges the full-resolution world
   to disk with a progress bar, then starts it.
3. Load world: a list of saved worlds (name, seed, size, created date, preview thumbnail),
   Load / Delete / Back.
4. After either: a server serving that world is running, the client is connected, and the
   world map is open with no region played yet; Play on a tile embarks there (existing
   `ClientPacket::PlayRegion` flow).
5. Exit quits cleanly (stops any server the client started).
6. In game, the Esc menu gains **Main menu** (leave the world, stop its server, back to 1) —
   a clean in-process exit; the same client can then start another world. Losing the server
   also lands in the main menu, with the reason.

## Non-goals

- **Saving player changes.** A "world" here is the forged terrain recipe (`.world` file).
  Edits, designations and entities still live only in server memory and are gone when the
  server stops. Load world means "play this generated world again", not "resume my fortress".
  Persistence is its own feature (the Postgres/terrain-row direction in
  `2026-09-20-multi-instance-…`); this design only reserves the per-world directory it will
  write into.
- Multiplayer lobby / server browser / joining someone else's world.
- wasm/browser: the browser cannot spawn a server or comfortably run a minute-long forge.
  wasm keeps today's flow (connect, or `?offline`) and skips the main menu **[my call]**.
- Procedural (noise) worlds in the menu. The menu only makes and loads forged worlds;
  `VOXEL_WORLD_FILE=procedural` stays a dev path.
- Map exchange strings (Factorio's bottom-left import/export). The knob set is still moving;
  a shareable string can come later from the same sidecar JSON.
- Planet selector, "Appears on" column, Enemy/Resources tabs — no equivalents yet.

## Anti-goals (would make it a failure even if it works)

- The UI freezing while forging. Preview and full forge run off the UI thread; the screen
  stays responsive and the preview shows a "forging…" state.
- Two copies of the knob table. The generator screen is built from
  `worldgen::forge::knobs::SPECS` / `CHOICES` at runtime (label, help, min, max, default,
  group), exactly like `just forge-ui`; adding a knob must not require touching the UI.
- A main menu that breaks the dev loop. `just run`, the probes, `RENDER_BENCH`,
  `VOXEL_AUTOPILOT` and the headless harnesses must keep booting straight into a world
  (see "Dev bypass").
- Bevy UI. Every screen here is Slint on the `../ui` widgets, per the Slint-only direction.

## Constraints from the code as it is

- **One world per server process.** `worldgen::forged::install` is a `OnceLock`; a server
  serves exactly one world for its life. Changing world = starting a server on a different
  file. This matches the one-pod-per-world direction, so it is kept, not worked around.
- **The client connects at startup** (`Command::ConnectToServerAndScene` sent in `main()`),
  retrying for 2 min. The connect becomes something the menu triggers.
- **The `.world` file does not record the knobs** (`WorldArtifact` has seed, size, cell size,
  rasters, rivers, lakes, spawn — no `Knobs`). The generator settings must be stored beside it.
- **The forge is not bit-reproducible across machines** (by design — the file is the world,
  not the seed). So the file is authoritative; the stored knobs are for display and
  "regenerate with tweaks", never for re-deriving the same world.
- Full forge at 16 m cells takes ~30–60 s (longer on the power-clamped laptop); a 64 m preview
  ~1.5 s, 32 m ~5 s. `forge()` today has no progress callback and no cancellation.
- `worldgen` is already a client dependency, so the client can forge in-process.
- The forge_proto preview renderer (`examples/forge_proto/render.rs`) is example code, not
  library; `worldgen::tile_map::rasterize` (world-map colours) is library code.

## The how — decided

### Process model: the client launches a local server per world (Dawid: yes, for single player)

**Server/world rule (Dawid):** a server app serves exactly one world. In future multiplayer a
world may be split across several servers (each serving part of it), but a server never
runs two different worlds. So the `forged::install` `OnceLock` is the intended invariant,
not a limitation to remove.

On New/Load, the client spawns the `server` binary as a child process with
`VOXEL_WORLD_FILE=<world dir>/terrain.world`, then sends `ConnectToServerAndScene` to it.
Leaving to the main menu stops the child (see "Session lifecycle"); Exit does the same. This is
Factorio's single-player model (an in-process host) expressed as a process, and it is the
shape a pod-per-world deployment already has, so nothing in `server` has to learn to swap
worlds. Alternatives weighed:

- *Server in the client process* — `install`'s `OnceLock` forbids a second world in one
  process, and making it swappable touches worldgen's global state that every generator
  closure assumes is fixed. Rejected.
- *A long-lived server that accepts a "load world" packet* — same OnceLock problem, plus a
  new admin packet with no auth story. Rejected.

The child is found next to the client executable (`current_exe().parent()/server`), with
`VOXEL_SERVER_BIN` as an override **[my call]**. Its stdout/stderr go to
`<world dir>/server.log`. Menu worlds run their server on a free UDP port chosen by the client
(bind 127.0.0.1:0, read the port, release it) and passed as `VOXEL_SERVER_PORT`; the client
connects to that port. A single-player server starts no WebTransport ingress and writes no
cert-hash file. Dev servers (`just run` / `just server`) keep 6466 (QUIC) and 6467
(WebTransport). (Revised 2026-10-31: the fixed 6466 made a menu world fail whenever a dev
server was running.)

### Session lifecycle (in-process exit to the menu — Dawid's decision 2)

Today the client is one session per process (inventory 2026-10-30, file:line refs from
`crates/client/src`): the channels are made once in `main()` and the connect is sent at
startup; `GameInstanceManager::connect_and_run` (`instance.rs`) blocks the game thread until a
`GameEventKind::Quit`, and its command loop (`main.rs` `start_game_thread`) can already take
a second `ConnectToServerAndScene` after it returns; a server disconnect **panics** the game
thread (`instance.rs` `server_msg.unwrap()`, the netcode thread's `connect_and_handle().unwrap()`)
and Bevy never hears of it (`bridge.rs` treats a closed channel as empty; `GameCrash` is never
sent). Per-world state is ~40 Bevy resources, a handful of system `Local`s, top-level entity
trees (region roots, LOD roots, the ui-root), and render-world arenas.

The design:

- **`AppState { MainMenu, MapGenerator, LoadWorld, Starting, InWorld }`** (Bevy `States`). Every
  world system (bridge, meshing, LOD rings, far voxel, commander, fortress view, world map,
  boundaries, join timeline, autopilot, probes) runs under `in_state(InWorld)` (the world map
  also needs `Starting` for its first packet). Menu screens are Slint components switched by
  state.
- **One session = fresh channels.** Entering `Starting` creates new `game`/`client`/`request`
  channels, replaces the `ClientUpdates` / `GameEvents` / `ClientRequests` resources, then sends
  `Command::ConnectToServerAndScene` with the child server's address. Channels are swapped
  BEFORE anything is despawned, so a late message from the old manager lands in a dropped
  receiver, not in the new world.
- **Leaving (`OnExit(InWorld)` or `Starting` aborted):**
  1. send a new `ClientPacket::Leave` (graceful: the server runs its normal
     `ClientDisconnected` path at once instead of waiting for the QUIC idle timeout), then
     `GameEventKind::Quit` so `connect_and_run` returns `Ok` and the manager, tick thread and
     netcode thread wind down;
  2. stop the child server (Leave → server exits when its last client leaves in single-player
     mode **[my call]**: a `--single-player` server flag), else it is killed after a 5 s grace;
  3. despawn everything tagged **`SessionScoped`** (a marker on region roots, retired roots,
     LOD roots + retired, the ui-root, snapshot voxel tasks) with one query — the children go
     with them;
  4. run the **session reset registry**: each plugin registers its own resets in its `build`
     (`app.add_session_reset::<CommanderCamera>()` — re-insert `Default`/a constructor — or a
     closure), so the reset list lives next to the resource it resets and a new resource can't
     silently survive. Startup-only assets and user preferences (block textures, materials,
     skybox, lights, render/culling config, debug toggles, `CursorPolicy`) are not registered.
  5. **Far voxel buffers reset wholesale** (`FarVoxelResidency` re-created + `GpuFarBuffers`
     cleared through a render-world reset message) — their per-entity removal path stages one
     removal per frame and would take minutes for a full LOD horizon. The voxel-pull
     `GpuQuadArena` frees by entity removal already; it keeps its grown capacity **[my call]**.
  6. clear buffered Bevy messages that carry world data (`TileMirrorMsg`, `WorldMapMsg`) and
     in-flight work queues (`SnapshotVoxelQueue`, `LodRings.pending`, remesh / LOD build queues).
- **Per-world `Local`s become resources** (or carry a session id): the join-timeline latch
  (`WorldReady` + `JoinTimelineState`, so a second world gets its own `[perf]` join timeline),
  `OverlayBuilt`, the minimap `built` flag, designate `GhostCache`, LOD reveal/retire holds,
  `visibility.rs` `GridCache`, motion-probe state. Locals that compare by value
  (`MapShown`, `connected`) may stay.
- **Disconnects become events, not panics.** The game thread's `unwrap`s on the server channel
  and the netcode result become `Err` returns; the game thread sends a new
  `ClientUpdateEvent::Disconnected { reason }` and goes back to waiting for commands. Bevy
  turns it into the same leave path, landing in `MainMenu` with the reason shown (server
  exited, connection lost, connect timed out — with the server-log tail when we started it).
- **Process-global state is fine as is**: `forged::ACTIVE` is never touched by the native
  client (the world lives in the server process); perf origin, hash mode and `WorldScale` are
  per process by intent.
- **Verification**: a headless client test (`MinimalPlugins`, offline `LocalServer` like the
  convergence harnesses) that joins, plays a region, leaves, joins again and asserts that no
  `SessionScoped` entity, no registered resource's non-default state, and no far-buffer slot
  survives the first session, and that the second session reaches world-ready.

#### Per-world inventory as built (Task 8, 2026-10-31)

Mechanisms: **reset** = `add_session_reset::<R>()` (back to `FromWorld`), **with** =
`add_session_reset_with(fn)`, **clear** = `add_session_message_clear::<T>()`, **scoped** =
`SessionScoped` root (despawned with its tree), **SessionLocal** = a system `Local` that is back at
its default after `end_session` (`session::SessionLocal`, keyed on `SessionGeneration`), **gate** =
`run_if(world_systems_enabled())` (true without the session state, so headless tests that skip
`SessionPlugin` are unaffected).

| Item | Mechanism |
|---|---|
| region root + bodies root (`bridge::spawn_region_roots`; chunks, bricks, snapshot voxel tasks, sim entities, voxel bodies, rig models, avatars are their descendants) | scoped |
| LOD roots (`lod_rings::spawn_lod_root`; retired roots are the same entities) | scoped |
| `Regions`, `RegionRoots`, `RetiredRegionRoots`, `SimEntityMap`, `SnapshotVoxelQueue`, `LocalPlayer`, `HudStatus`, `UiUpdates`, `DebugColliderLines`, `LocalLook` | reset |
| `ChunkGrid`, `RemeshQueue`, `FrameUploads`, `LodBuildQueue`, `FarFrameUploads`, `PullMemStats`, `SkippedBricks` (now registered in `build`), `CullingStats` | reset |
| `LodRings` (roots, retired, pending), `WorldReady` | reset |
| `WorldMapState`, `world_map::PendingPlay`, `slint_overlay::PlayPending` | reset |
| `BoundaryUniform`, `FortressViewUniform`, `OverlayGrid`, `DragPreview`, `SliceState` | reset |
| commander `CommanderCamera`, `MinimapPointer`, `MinimapPixels`, `Selection`, `GroundPick`, `SelectionBand`, `TileMirror`, `WorkingLevel`, `DesignateState`, `DragReadout`, `ClientPlayMode`, `Follow`, `RightPress`, `PathPreview`, `SentPath` | reset |
| `HeightGrid` (water id from the block registry) | with |
| `CursorPolicy`: the world's fields (`sim_grab`, `ui_modal`, `orbit`, `ui_pointer`); `overlay_open` (dev panel) kept | with (`cursor::reset_session_grab`) |
| far voxel: fresh `FarVoxelResidency` (same limits, batch ids keep counting), `FarBrickRevisions`, `FarDeferredRemovals`; `FarUploadChannel::request_reset` drops queued batches/results and the render world empties `GpuFarBuffers` wholesale | with (`far_voxel::lifecycle::reset_far_voxel`) |
| `GameUi` and `HudUi` Slint components (menu/map open, map image/zoom/pan, selected and home tile, adventure, following, active tool, level text, wheel, band, minimap, …) | with: `SlintLayerPlugin::session_scoped()` rebuilds the component (`rebuild_slint_layer`), so every property is a fresh one; the rebuilt layer is left un-blanked, so while the frontend has the screen its texture is zero-filled the next frame |
| `WorldMapMsg`, `TileMirrorMsg`, `ToggleModeRequest` | clear |
| Locals: join timeline, overlay built, minimap built, designate preview/ghosts/right-travel, follow last, selection press, level was-cut/held, move-order pending, orbit anchor, LOD hold, `GridCache`, mesh rank cache, awaiting-ground epoch, the three motion probes, `forward_input` last window size and mouse coalescer, wheel phase / wheel captions / `MapShown`, Slint pointer state | SessionLocal |
| `Embarked` | reset (Task 7) |

Kept on purpose (preferences, startup assets, process state): `XraySettings`, `CullingConfig`,
`BoundaryOverlay`, `GridDebug`/`TerrainDebug`, `TerrainRenderConfig`, `MeshBudgets`/`MeshBudgetPolicy`,
`MouseCoalesceWindow`, `DebugFlagsRes`, `DevUiOpen` and the dev panel, the minimap's rate-limit `since` timers and its draw `scratch` buffer (plain `Local`s: the next session's first rebuild is forced by the reset `built` key and `MinimapPixels`), block textures/materials,
skybox cubemaps, `AvatarAssets`, `GlobalAmbientLight` and the commander's `surface_ambient` Local
(the startup brightness; the first underground check of the next session restores the surface
lighting), the commander camera entity (re-placed from the reset `CommanderCamera`), the slice plane
entity (hidden until a home region exists), render-world `GpuQuadArena` (frees by entity removal)
and `xray::InteriorChunks` (self-pruning), the autopilot and render-benchmark state (bypass-only
one-shot runs).

Cameras: while the frontend has the screen, `bridge::enforce_single_active_camera` (body cameras)
and `commander::yield_commander_camera` (ungated) leave every world camera inactive and without
`UiCamera`; `slint_composite` composites onto exactly one `UiCamera` view (the highest camera order,
`pick_ui_view`). The dev panel (`DevPanelUi`) is `hidden_by_frontend`.

Ordering contract for Task 9: swap the session channels (`ClientUpdates`, `GameEvents`,
`ClientRequests`) BEFORE `end_session`, or the next frame's drain re-spawns the old world from the
old receiver.

### Dev bypass

If `VOXEL_WORLD_FILE` is set, or `RENDER_BENCH` / `VOXEL_AUTOPILOT` is set, or a new
`VOXEL_SKIP_MENU=1`, the client keeps today's behaviour: connect to 6466 at startup, no menu,
no child server **[my call]**. `just run` sets `VOXEL_SKIP_MENU=1` so the dev loop is
unchanged; a new `just play` recipe builds both binaries and starts **only** the client, so
the menu is exercised end to end.

### Worlds on disk

`worlds/<slug>/` per world (git-ignored, under the repo root like `worlds/default.world`; a
user-data dir is for when there's an installer **[my call]**):

- `terrain.world` — the forged artifact (unchanged format).
- `world.json` — `{ name, slug, created_utc, seed, size_m, cell_m, preset, knobs: {key: value…},
  forge_ms, format_version }` written by the client after a successful forge.
- `preview.png` — the full-resolution relief render used as the Load list thumbnail and
  the generator's "last" image.
- later: save data (non-goal now).

Slug = the name lowercased with non-alphanumerics → `-`, de-duplicated with a numeric suffix.
The existing `worlds/default.world` (no directory) is listed as "default (legacy)" with
unknown settings; loadable, not deletable from the menu **[my call]**.

Writes are atomic: forge into `worlds/.<slug>.tmp/` then rename the directory, so an
interrupted forge leaves no half world in the list.

### Screens (all Slint, `crates/slint-ui/ui/`)

Client app state gains a top-level `AppScreen`: `MainMenu` → `MapGenerator` | `LoadWorld` →
`Starting { world }` (spawning server + connecting, progress text) → `InWorld` (today's game;
the world map opens automatically on first `WorldMap` packet) → back to `MainMenu`. The 3D
camera, sim bridge and commander systems run only in `InWorld`; before that the window shows
the Slint layer over a plain background **[my call: a dimmed render of a random saved world's
`preview.png` is a nice later touch, not in scope]**.

**Main menu** (`main-menu.slint`): game title, a centred `FrameFace` with three buttons —
New world, Load world (disabled when no worlds exist), Exit.

**Map generator** (`map-generator.slint`), laid out like the screenshot:

- Top-left bar: **Preset** dropdown, **Seed** textbox (hex, as the forge prints it) with a
  shuffle button, **Name** textbox (defaults to e.g. "Island 2F1A") **[my call: Factorio has no
  name field here; we need one for the Load list]**.
- Preset description line under it.
- Tabs **[my call — 7 knob groups folded into 5 tabs, so no tab scrolls more than ~12 rows]**:
  - *Land* — the "Island" group (layout choice as a dropdown row, then sliders) + world size.
  - *Mountains* — "Uplift" + "Shaping" peak height / valley floor knobs.
  - *Water* — "Rivers" + the lake knobs of "Shaping" + "Climate".
  - *Geology* — "Geology".
  - *Advanced* — "Erosion" (iterations, years per iteration, K, diffusion…) + preview cell
    size + output cell size (16 m default; 8 m allowed with a "slow" warning).
  The tab → knob mapping is a small table in the client keyed by knob key, with every knob
  not listed falling into Advanced, so a new knob can never be missing from the UI.
- Each row: label (with the `help` text as tooltip/info marker — the screenshot's ⓘ), slider
  over `[min, max]`, numeric value, and a "reset to default" affordance when changed. One
  slider per row, not Factorio's two-column Scale/Coverage pairing — our knobs don't pair up
  **[my call]**.
- Right: preview image of the world at the preview cell size, **Auto-refresh** checkbox
  (default on) and a refresh button. Layers dropdown where Factorio has the planet selector:
  Relief (default) / Rivers / Geology / Rain **[my call]**. Under the image: land share,
  island count, largest island km², highest peak, lake/river counts (the stats `serve.rs`
  already returns). Hover shows nothing in v1.
- Bottom: **Back** (left), **Play** (right, green, wide).
- Play: forges at the output cell size on a worker thread, a modal `ProgressbarFace` with
  the forge's iteration count ("Eroding 312 / 500"), Cancel; on success writes the world
  dir and goes to `Starting`.

Presets **[my call]**: *Default* (knobs defaults), *Archipelago* (layout 1), *Island arc*
(layout 2), *Highlands* (higher uplift, lower K), *Lowlands* (low uplift, more lakes),
*Dry* / *Wet* (climate). Defined as `(name, description, &[(key, value)])` overrides on the
defaults in one table next to `knobs.rs` in `worldgen::forge` so `forge-ui` can offer them
too. Editing a knob switches the dropdown to "Custom".

**Load world** (`load-world.slint`): a `ListBoxFace`/`TableFace` of worlds, newest first —
thumbnail, name, seed, size, created date. Selecting one shows the bigger preview and its
settings summary (preset + changed knobs). Buttons: **Load** (→ `Starting`), **Delete**
(confirm dialog), **Back**. Double-click loads.

**Starting / loading**: a centred status frame — "Starting server…", "Connecting…",
"Receiving world map…" — driven by the existing `[perf]` milestones; errors (server exited,
port in use, connect timed out) show the server log tail and a Back button.

**World map on arrival**: unchanged screen, opened automatically on the first `WorldMap`
after a menu start. Today `WorldManager::handle_server_event(ClientConnected)`
(`crates/game/src/world_manager.rs`) always creates the player in `SPAWN_REGION` before it
sends the map, so the spawn region generates behind the map (~4 s at 256 m) even if the
player embarks elsewhere, and Play then does a normal `ClientLeft` + `CreateClientAt` move.
Because the player already stands in the spawn region, its tile would show "You are here"
(`tile_map::play_blocker`); until the first Play the client treats that tile as playable and
embarking there is client-side only (no packet) **[my call]**.
v1 keeps that (no manager change) and just hides the world behind the map until the first
Play — see open question 2 for the alternative (connect "unplaced", first Play creates the
player).

### Forge plumbing (`worldgen::forge`)

- `forge_with(p, &mut dyn FnMut(ForgeProgress) -> ControlFlow<()>)`: reports
  `{ stage, iter, iters }` and lets the caller cancel between iterations; `forge()` stays as
  a wrapper. Needed for both the progress bar and for discarding a stale preview when a
  slider moves mid-forge.
- The preview renderers the generator needs (relief/rivers/geology/rain → RGBA) move from
  `examples/forge_proto/render.rs` into the library (`worldgen::forge::render`) so the client
  and `forge-ui` share them; the example keeps writing PNGs through it.
- Preview worker: latest-wins. A knob change bumps a generation counter, cancels the
  running preview forge, and starts a new one after a ~250 ms debounce **[my call]**.

## Left to the builder

- Exact Slint layout, spacing, colours (follow `world-map.slint` / the `../ui` widget styles).
- Which `../ui` widgets need behaviour added (several are look-only `*Face` components:
  slider drag, dropdown popup, textbox editing) — build what's missing in `../ui` if it is
  generic, else locally in `crates/slint-ui`.
- How `AppScreen` gates existing plugins (run conditions vs. states).
- Child-process supervision details (startup-ready detection by connect success is fine).
- Preset knob values — reasonable starting numbers, checked visually with the preview.
- Test layout: Slint hit-testing tests like `crates/slint-ui/tests/world_map.rs`; world-dir
  write/list/delete/slug as plain unit tests; forge progress/cancel as a worldgen test at a
  coarse cell size.

## Settled after the builder check

- **Build settings are part of a world [my call].** `world.json` records `region_blocks` and
  `chunk_blocks` (the build-time dials `game::REGION_BLOCKS` / `CHUNK_BLOCKS`). Load world
  shows a mismatching world greyed out with "made with 256 m regions, this build uses 64 m"
  and refuses to load it; the legacy `worlds/default.world` is assumed to match. `just play`
  uses the same 256-block default as the other play recipes.
- **Widgets live in this repo first [my call].** Slider drag, dropdown popup and textbox
  editing are built in `crates/slint-ui` on top of the `../ui` `*Face` components; nothing
  in `../ui` is changed by this work. Moving generic ones upstream is a later, separate step.
- **The world map is modal until the first Play [my call].** After a menu start, the map's
  Close button and M/Esc do nothing until a region has been played; Esc offers "Back to main
  menu" instead. So no "not embarked" state has to gate the commander camera, cutaway or
  designation tools: they simply aren't reachable before the first Play.

## Decisions (Dawid, 2026-10-30)

1. **Process model:** single player = the client spawns a server child per world (as above).
   Multiplayer later = one world per server app, possibly sharded across servers, never two
   worlds in one server.
2. **Back to the main menu must be a clean in-process exit** — no client relaunch. The client
   tears its world session down and returns to the menu in the same process, and can then
   start another world. See "Session lifecycle".
3. **Spawn on arrival:** v1 keeps today's behaviour (spawn region loads behind the map).
4. **Worlds live in `worlds/`** in the repo for now.
