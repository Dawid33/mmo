# Dwarf Fortress direction — fortress-mode MVP transition plan

> **Status: PROPOSED (2026-10-20), awaiting Dawid's approval.** Once approved it becomes the
> project direction: update the CLAUDE.md "Direction" paragraph, `TODO.md` and the memory note
> `game-direction-pivot` then.

Research behind every decision here: `2026-10-20-dwarf-fortress-research/` (start at
`00-synthesis.md`; engine decisions in `engine-mapping.md`). The deprecated RTS plan lives on
branch `ds/rtsPivot` (`docs/superpowers/specs/2026-10-20-rts-transition-plan.md`).

## Direction (Dawid's words, condensed)

- Recreate Dwarf Fortress **faithfully** in our voxel engine.
- **Generate a huge world up front.** Players play **individual regions** from the commander
  camera (top-down, fixed pitch, pan and zoom).
- Later, **traverse between regions** in adventure mode and fortress mode.
- **The MVP is fortress mode only**: one player runs one fortress in one region of a
  pre-generated world.

## What carries over unchanged

The deterministic sim (`game`, `StateHash`, `game::state_hash`, `just determinism-wasm`), the
input-only server/client transport (QUIC + WebTransport, `encode_packet`), the `WorldManager` /
`RegionRunner` / region threads, the offline forge and forged-world serving (`just forge-proto`,
`VOXEL_WORLD_FILE`), `generate_region`, LOD rings, the pulled-quad renderer and mesher, the
commander camera, Blockbench model loading and animation, the asset pipeline, the perf tooling.

## What changes in kind (decided in `engine-mapping.md`, confirm in phase 0)

1. The fortress is a **`RegionKind::Fortress`** region whose simulation truth is a dense **tile
   map**; voxel chunks become a client-side render product; no rapier, no body, no slimes in it.
2. **Lockstep at prediction depth 0** for fortress regions; rollback + prediction stay for the
   wild/adventure path.
3. **DF tick semantics**: 1,200 ticks/day, 1× = 20 ticks/s (`TICK_RATE` unchanged), speed dial to
   5×, pause.
4. **Raws** in `assets/raws/*.ron`, content-hashed.
5. **Embark = one region at `REGION_CHUNKS = 64`** (128 × 128 tiles) on the forged world;
   neighbours as LOD scenery; handoff/near window/cell mode parked, not deleted.

## Why this phase order (and how it differs from the handoff's sketch)

The handoff proposed: world + embark → z-view + dig/chop designations → dwarves with jobs →
needs/stockpiles/workshops/farming → migrants/threat/fail. Two changes:

- **Foundation before embark.** The tile map as sim truth and lockstep pacing touch the region,
  snapshot, client pacing and render paths; every later phase builds on them, and their risks
  (pacing bugs, the dropped-packet hole, render derivation cost) are best met with nothing else
  moving. The world-map/embark screen is mostly UI on top and comes second.
- **Designations before dwarves, executed by a dev "god hand".** Phase 3 makes designations
  complete instantly, so the cutaway, hidden-tile reveal, tile edits, render updates and
  connectivity maintenance are all verified live before any agent exists. Phase 4 then only adds
  *who* does the work.

Each phase leaves a playable build, ends with `just smoke` green and is committed in steps.

---

## Phase 0 — decisions (no code)

Answer the open questions in `00-synthesis.md` (multiplayer shape, faithfulness, tile scale,
z-depth, world size, history, game speed, fluids timing). Phases below assume the recommended
answers; a different answer to Q1 (netcode) or Q3 (tile scale) changes phases 1 and 3.

Optional, if Dawid agrees: a throwaway A\* + connectivity cost spike on a real forged embark,
outside the workspace crates (under `$CLAUDE_JOB_DIR/tmp` or `target/`), to replace the estimates
in `engine-mapping.md` §4.4.

---

## Phase 1 — the fortress region: tile map truth, derived rendering, lockstep

**Scope**

1. `RegionKind { Wild, Fortress }` on the region seed/runner path (`RegionSeed::into_runner`,
   `region_runner.rs`); the server creates the spawn region as `Fortress` behind a run flag
   (`just fortress=1 run` / `VOXEL_FORTRESS=1`) so the wild path stays the default until phase 2.
2. `crates/game/src/fortress/map.rs`: `FortressMap` (dense `Tile` array in 16×16×1 map blocks:
   material, shape, flags, liquid, designation, traffic), `StateHash` + serde; built once from
   `generate_region` output (blocks → tiles, half blocks → ramps, water → liquid 7, hidden by
   sky flood fill, tile attributes).
3. Fortress regions skip rapier, slimes and the player body (`region.rs` tick gated on kind);
   chunks are not materialised in `GameData` for them.
4. Render updates from the tile map (new `GameDataUpdate` variants per dirty map block, or the
   existing `SetVoxelComponent` path fed from tiles); the client bridge derives render voxels
   from tiles (hidden → solid unknown material) and meshes them as today.
5. **Lockstep pacing**: per-region pacing mode in the client (`crates/client/src/instance.rs`:
   lead ≤ 0, no locally generated `Tick`s, execute up to the server's confirmed head); the server
   stamps fortress-region inputs at `head + INPUT_DELAY_TICKS` (named constant, ~3 ticks).
6. **Fix the dropped-packet gap** (memory `netcode-dropped-packet-gap`: `send_bounded` drops a
   packet after `SEND_DEADLINE`): never drop; a client that falls too far behind is resynced with
   a fresh snapshot instead.
7. Snapshot for fortress regions = the tile map (lz4 via `encode_packet`).
8. Commander camera focus clamped to the fortress box (already clamps to the home region).

**Done when (live run)**: `just world=tiny fortress=1 run` (with or without `world_file=`) shows
one 128 m embark rendered from tiles with LOD scenery around it; the client joins from a
tile-map snapshot; the server and client state hashes agree for 10 minutes idle; killing and
restarting the client rejoins cleanly; no rapier step runs in the fortress region (`[server]`
counters / trace).

**Files/crates**: `game` (`fortress/` new, `region.rs`, `region_runner.rs`, `state.rs`
render-update enum, `protocol.rs` constants/packets), `server` (`region_threads.rs` input stamping,
`lib.rs` `send_bounded` / `SEND_DEADLINE`), `client` (`instance.rs`, `netcode.rs`, `renderer/bridge.rs`,
`renderer/meshing/` input from tiles), `worldgen` (block → tile conversion helper or in `game`),
`justfile` (`fortress=` variable).

**Risks**
- Client pacing (`align_region`, `SyncClock`) is subtle and shared with the wild path — keep the
  wild path's tests green (`just test-threads`).
- Determinism of the tile conversion and hidden flood fill on wasm.
- Render derivation cost at join: 64×64×~60 chunks of derived voxels — measured against the
  current join timeline (`[perf] event=…`), buried-chunk skip must still apply.
- The undo log stays on for `FortressState` in this phase (correctness first); its cost is
  measured in phase 4.

**Tests / verification**
- `just test-one game "--test fortress_map"`: blocks → tiles conversion (walls/floors/ramps/
  hidden), hash stable across a serialize/deserialize round trip.
- `just determinism-wasm` with a new pinned fortress scene (tile map + 1,000 idle ticks).
- `just test-threads` (wild path untouched).
- A slow, opt-in client test (ask first): headless lockstep join + 1,000 ticks, hashes equal.
- `just smoke`.

---

## Phase 2 — world map, embark, save/load

**Scope**
1. A **world map screen** (bevy_ui) showing the forge's relief/river render of the current
   `.world` (or a `just world-map`-style render of the procedural world) with the region grid;
   click a region → embark summary (relief, water, soil/stone from the artifact) → **Embark**.
2. An `Embark` client→server command; the server creates a `Fortress` region at those
   `RegionCoords` and puts the client's session home there (`WorldManager.homes` is already keyed
   by `ClientId`). The run flag from phase 1 goes away: fortress mode is the game.
3. `assets/raws/` begins: `materials.ron` (soil, 2–3 stones, one ore, wood, water) mapped to
   block ids and textures; the forge's geology formations (`forged.rs`, unused today) drive the
   stone layers; content hash checked at connect (add the missing connect handshake).
4. **Save/load**: `FortressState` + world artifact crc + raws hash to a file (bincode + lz4);
   server loads it on start (`just fortress_save=<path> run`) and saves on a command and on exit.
5. Calendar + pause + speed dial (server tick period; SyncClock carries it), shown in a HUD bar.

**Done when**: start → world map → pick a region → embark appears with real stone layers when
you look at a cliff; pause/speed work; save, quit, reload → identical state hash.

**Files/crates**: `client` (new `renderer/worldmap.rs` or `ui/`), `game` (`protocol.rs`
command, `world_manager.rs` embark path, `fortress/raws.rs`, `fortress/save.rs`, `registry.rs`
growth), `server`, `worldgen` (artifact access for the map render, geology lookup),
`assets/raws/`, `assets/blocks/` (stone textures), `justfile`.

**Risks**: the forged world's land/sea at 128 m regions (most regions may be sea or steep —
filter embarkable regions); geology formations were never exercised by the engine; save-file
compatibility (version the format from day one).

**Tests**: `just test-one game "--test fortress_save"` (save → load → hash equal),
`"--test raws"` (registry validation, stable ids, content hash), `just determinism-wasm`
(scene now includes geology), `just smoke`.

---

## Phase 3 — the z-level view and designations (god-hand execution)

**Scope**
1. **Cutaway** in the pulled-quad path: `cut_y` uniform, vertex/fragment clipping inside the
   fortress box, the front-face-culled **cap pass** with `frag_depth` onto the plane, the per-level
   **slice texture** (materials + hidden), depth darkening below the cut, shadows handled inside
   the box (`assets/shaders/voxel_pull.wgsl`, `renderer/voxel_pull/`). Per-chunk fallback path
   (WebGL2) gets the clip at least.
2. **Z navigation**: wheel = level step, Ctrl+wheel = zoom, jump to surface / lowest revealed,
   level number + depth bar (`renderer/commander/`).
3. **Tile picking at the current level** (replaces `GroundPick` for fortress input).
4. **Designations**: dig, channel, up/down/up-down stairs (v50 box inference), ramp, chop, remove
   designation; rectangle drags, multi-level boxes; per-level **overlay texture**; pending
   (unconfirmed) designations drawn dimmed client-side.
5. **God-hand executor** (dev only, `VOXEL_GODHAND=1` or an F3 toggle): designations complete
   instantly as sim events — the tile edit path (`fortress/edit.rs`: dig/channel/stairs/ramp
   rules from `02-…` §3, reveal neighbours, ramps collapsing when their wall goes, tile
   attributes update) and the render updates, exactly as dwarves will trigger them later.
6. **Connected components** (`fortress/connectivity.rs`) maintained incrementally on every edit
   (no consumer yet besides a debug overlay).
7. Trees as single-column objects (logs on chop).

**Done when**: you can dig a stair shaft, rooms and a channel on several levels and scroll
through them; hidden stone stays black until exposed; the cap shows the right material on every
level; the debug overlay shows components merging and splitting correctly; stepping 20 levels
with the wheel has no frame hitch.

**Files/crates**: `client` (`renderer/voxel_pull/*`, shader, `renderer/commander/*`, new
`renderer/fortress_overlay.rs`), `game` (`fortress/edit.rs`, `fortress/connectivity.rs`,
designation events in `protocol.rs`).

**Risks**: cap correctness at chunk borders and with chisel shapes; the shader-validation tests
compose against hand-written import stubs, so run `just pull-smoke` on a real adapter
(CLAUDE.md); clipping in the shadow pass; WebGL2 fallback quality.

**Tests**: `just test-one game "--test fortress_edit"` (each designation's tile rules, ramp
collapse, reveal); `"--test fortress_connectivity"` (seeded random edits vs a full re-flood
oracle, tagged slow if >100 ms); `just pull-smoke`; `just determinism-wasm`; `just smoke`.

---

## Phase 4 — dwarves that do the work

**Scope**
1. `assets/raws/creatures.ron` (dwarf), `items.ron` (boulder, log, pick, axe), `labors.ron`
   (mining, woodcutting, hauling), `embark.ron` (Play Now: 7 dwarves + tools). The embark spawns
   them at the region edge/wagon.
2. Units (`fortress/units.rs`): creature raws, tile position, gait, carried item, current job;
   the **wake-up queue**; movement tile to tile at gait speed; crowding rule (walk over at a
   penalty).
3. **Pathfinding** (`fortress/path.rs`): component pre-check, integer A\* with fixed
   tie-breaks and traffic costs, ramps/stairs/diagonal-gap rules, "path failed" cooldowns;
   Dijkstra fields for "nearest X".
4. **Job board** (`fortress/jobs.rs`): designations become jobs; idle dwarves claim the nearest
   permitted job (labor permission, `everybody does this`); reservations; cancel/cooldown.
   Dig/chop jobs drop boulders/logs (`items.rs` with type/material/position indices).
5. **Hauling** to a **stockpile** zone with a category filter (stone, wood).
6. Skills: XP per job, level → job speed (`04-…` §2 formulas).
7. Renderer: units as Blockbench models with walk/dig/chop clips interpolated between tiles;
   items as instanced meshes; job markers; unit sheet + tile inspector panels.
8. God-hand becomes debug-only.
9. **Measure**: tick cost at 7, 50, 300 units (synthetic) native and wasm, undo log on vs off for
   `FortressState`; decide whether to bypass the undo log (`engine-mapping.md` §3.3).

**Done when**: designate a dig and a chop, and seven dwarves carry it out, haul stone and logs to
stockpiles, never path through walls, never stall on unreachable jobs; 300 synthetic units run at
5× speed under the 10 ms budget native (wasm number recorded).

**Files/crates**: `game` (`fortress/{units,path,jobs,items,stockpile,skills}.rs`, raws),
`client` (renderer units/items, panels), `assets/raws/`, `assets/models/`.

**Risks (agents first)**
- **Agent tick cost**, especially wasm single-threaded under lockstep — the central risk; the
  measurement step is part of the phase, with option (c) (server-only sim) as the fallback if wasm
  can't make it.
- Undo-log cost on high-churn unit state while it is still on.
- Determinism of the A\* heap, job claiming order and item indices (BTreeMap everywhere, fixed
  iteration order).
- Path caches and component maps must stay out of the hash or be recomputed before hashing.

**Tests**: `just test-one game "--test fortress_path"` (A\* optimality vs Dijkstra on seeded maps,
ramps/stairs/diagonals rules, unreachable → O(1)); `"--test fortress_jobs"` (reservation
invariants, no double claims, cancel/cooldown); `just determinism-wasm` (scene: 7 dwarves digging
for 5,000 ticks); a release perf gate at 300 units (tagged slow); `just smoke`.

---

## Phase 5 — the survival loop

**Scope**
1. **Calendar** drives seasons; hunger/thirst/drowsiness as timestamps with DF thresholds
   (`04-…` §7); eat, drink, sleep jobs (sleep on a bed if owned, else floor).
2. **Food and drink**: plant gathering; a **farm plot** (one surface crop, one underground crop,
   per-season choice, seeds back from eating/brewing); **still** (plant → drink); drinking from a
   water-source zone as fallback; easy meals at a kitchen if time allows.
3. **Workshops as data** (`reactions.ron`, `workshops.ron`): carpenter (log → bed, door, table,
   chair, barrel), mason (boulder → door, table, chair, block), still, kitchen.
4. **Buildings**: placement as jobs (haul materials, build); doors affect connectivity (open to
   dwarves); bed/table/chair.
5. **Zones**: meeting area, bedroom (assigned), dining hall; stockpile categories for food,
   drink, furniture.
6. Announcements with auto-pause (season change, starving/dehydrated, job failures).

**Done when**: a fresh embark, left to the player's designations, gets through its first full
year: everyone fed, watered and sleeping in beds; a farm, a still, carpenter and mason shops
working; no dwarf dies of neglect if the player sets things up sensibly, and dwarves do die of
thirst if they don't.

**Files/crates**: `game` (`fortress/{needs,calendar,farming,workshops,buildings,zones}.rs`),
`assets/raws/` (plants, reactions, workshops, buildings), `client` (build menus, zone painting,
workshop panels, HUD).

**Risks**: job-system complexity (needs pre-empting work; reagent search must use indices);
doors and room flood fills changing connectivity; balancing DF's numbers at our tile scale.

**Tests**: `"--test fortress_needs"` (thresholds fire at DF tick counts; a dwarf with no drink
dies at 75,000), `"--test fortress_workshops"` (reaction consumes reagents, produces products,
deterministic), `"--test fortress_farming"`; a long-run determinism scene (one in-game year
compressed, tagged slow); `just smoke`.

---

## Phase 6 — pressure and the fail state

**Scope**
1. **Seasonal event scheduler** (`fortress/events.rs`): at each season start, roll the events'
   dates (stateless `roll()`); the two hardcoded migrant waves (1–10 dwarves) arriving at the map
   edge and walking to the meeting area.
2. **First threat**: one hostile wildlife creature type (raws) spawning at the edge per season,
   population-capped.
3. **Minimal combat**: attack/defend, a wound list per body region, bleeding, unconsciousness,
   death; corpses as items; civilians flee visible hostiles (spatial-hash sensing every 10
   ticks, 26-tile radius).
4. **Fail state**: all citizens dead → "your fortress has crumbled", world-map screen again;
   player **abandon**.
5. Population caps as named tunables; announcements for arrivals, attacks, deaths.

**Done when**: a fortress gains its migrants, loses dwarves to wildlife when undefended, and can
be wiped out — and that end state is reached and reported; deaths leave corpses.

**Files/crates**: `game` (`fortress/{events,combat,wounds,sensing}.rs`), `assets/raws/`
(hostile creature), `client` (announcements, combat markers, death UI).

**Risks**: sensing cost (must stay bounded; DF's worst system); flee/attack AI interacting with
the job board; determinism of combat rolls.

**Tests**: `"--test fortress_combat"` (seeded fights reproduce exactly; wounds → death),
`"--test fortress_events"` (scheduler fires waves on DF dates), `just determinism-wasm` (scene
with a fight), `just smoke`.

---

## After the MVP (ordered proposal, not planned)

1. **Thoughts → stress → mood face**, room value and rank thoughts — the fortress's real economy.
2. **Flowing water** (our deterministic bodies + frontier CA, `2026-09-22-voxel-water-efficiency/`)
   with DF depth rules; floods, wells, mud, irrigation.
3. Caravans + trade depot + wealth-driven migrants; mayor and administrators.
4. Military squads with kill/station orders; ambushes; cage traps; burial.
5. Quality, value and wealth; smelting/forging chain.
6. Personality, needs/focus, strange moods and artifacts.
7. Caverns, magma, forgotten beasts; sieges at population 80.
8. Durable world persistence (the Postgres design), retire/reclaim.
9. World history in the forge (civs, sites, figures, legends).
10. Adventure mode on the parked traversal machinery.

## Tracking

Each phase: implementation plan in `docs/superpowers/plans/` before code, `just smoke` per
commit, live-run "done when" checked by Dawid (functional runs on `adapter=Intel`, memory
`live-test-intel-adapter`; ask before benchmarks), and a TODO.md line updated at the end.
