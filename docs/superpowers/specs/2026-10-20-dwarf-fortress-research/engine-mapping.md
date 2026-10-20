# Engine mapping — how fortress mode lands in this codebase

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. This is Part 2 of
the handoff: for each MVP-tier system, where it lives, what changes, and what it costs. Facts
about our code were read on `develop` at `0d910c1` (2026-10-20); file paths are given so each
claim can be checked. Design estimates are marked *(estimate)*.

## 0. Summary of recommendations

1. **The fortress is one `Region` with a new sim kind**, not a new server. It keeps the
   `RegionRunner` / `WorldManager` / snapshot / parking seams.
2. **A dense tile map is the simulation truth** for the fortress (`FortressMap`: material +
   shape + flags + liquid per 1 m tile). The voxel `Chunk` entities, their rapier colliders and
   the player body leave the fortress sim; the client *derives* render chunks from tiles.
3. **One DF tile = one 1 m block; one z-level = 1 m** (open question Q3 for Dawid).
4. **Netcode: deterministic lockstep at prediction depth 0** — clients execute only
   server-confirmed ticks, so fortress state needs no rollback and no re-simulation. The engine
   already *is* a deterministic, input-only sim; lockstep is a pacing mode of it, not a rewrite.
   Keep rollback + prediction for adventure mode later.
5. **Fortress tick = DF tick** (1,200 per day). Base speed 20 ticks/s (`TICK_RATE` = 50 ms,
   unchanged), with a speed dial up to 5× and pause. At 20 Hz a 900-gait dwarf walks 2.2 m/s.
6. **Pathfinding = A\* on the tile grid + incrementally maintained connected components +
   traffic costs + per-target Dijkstra fields for "nearest X"**, all integer, fixed tie-breaks.
   No rapier and no KCC for dwarves.
7. **Agents are event-driven**: a deterministic wake-up queue, need counters as timestamps,
   bounded-radius sensing through a spatial hash. Budget: 300 units + 10k items within ~5 ms per
   tick native *(estimate, to be measured in phase 3)*.
8. **Raws as RON registries** (`assets/raws/*.ron`) growing from `blocks.ron`, hashed so client
   and server agree; wasm mods hook in later as behaviour for raws-defined things.
9. **Z-level cutaway = a clip plane in the pulled-quad shader plus a "cap" from back faces**,
   coloured from a per-level 2D material texture. Stepping a level is a uniform write and a
   256×256 texture upload, no remesh.
10. **Park, don't delete**, the multi-region traversal machinery, first-person body, block
    editing and slimes: they are adventure-mode assets. The forged world becomes central.

## 1. Sim shape

### 1.1 Where the fortress lives

Today a region tick (`crates/game/src/region.rs`) applies events, ticks slimes
(`region.rs:488`, `Rollback::tick_slimes_safe`), moves player bodies through rapier's KCC and
steps physics once (`movement::TICK_DT`). State is `GameData` (`crates/game/src/state.rs:155`):
an ECS of slotmap entities whose components include one `Chunk` (2×2×2 blocks, 32³ voxels) per
terrain entity, each with a rapier body/collider; everything mutates through the `#[rollback]`
undo log.

Fortress mode needs a different *content* of the region, not a different container. Proposal:
a `RegionKind { Wild, Fortress }` chosen when the region is created (the `RegionSeed` →
`into_runner` path that cell mode already uses, `region_runner.rs`). A fortress region:

- holds a `FortressState` (tile map, units, items, jobs, stockpiles, zones, buildings, calendar)
  next to — not inside — the voxel ECS;
- skips rapier entirely (no terrain colliders, no bodies: nothing needs them);
- skips slimes and the player body;
- emits render updates (§8) from tile and unit changes;
- is hashed into `game::state_hash` like everything else (`StateHash` derive on every type).

### 1.2 What a tick means

DF: 1 tick = 72 game-seconds; 1,200 ticks/day; 403,200/year; default cap 100 ticks/s
(`11-…` §2). All of DF's numbers (hunger +1/tick, eat at 45,000, sleep −19/tick, caravans stay
30–40k ticks, gait 900 = 9 ticks/tile) are in ticks. **Keep DF's tick semantics** so the raws'
numbers carry over unchanged.

Our `TICK_RATE` is a *period*, 50 ms (`crates/game/src/lib.rs:71`). At 20 ticks/s:

| | DF @100 FPS | ours @20 Hz (1×) | ours @100 Hz (5×) |
|---|---|---|---|
| day | 12 s | 60 s | 12 s |
| year | 67 min | 5.6 h | 67 min |
| walking dwarf (9 ticks/tile, 1 m tiles) | 11 tiles/s | **2.2 m/s** | 11 m/s |

20 Hz is a natural 1× in a 3D view (a brisk walk, smooth with interpolation), and DF's own pace
is the 5× setting. The **speed dial** is the server changing its tick period (pause = no
ticks); `SyncClock` already carries a per-region tick rate to clients (`netcode.rs:171`).
The tick budget is set by the fastest speed: **10 ms per tick at 5×** for server and every
client (lockstep runs the sim everywhere).

### 1.3 Agent count budget

DF's practical ceiling is ~200 dwarves plus animals, merchants and invaders (`11-…` §3).
Target **300 units, 10,000 items, a 128×128×~120 tile map** *(estimate)*: under 5 ms per tick
native, 10 ms wasm (single-threaded in the browser). That is affordable only if most units cost
~nothing most ticks (§5): with walking at 9 ticks/tile and jobs lasting hundreds of ticks, a
unit acts on maybe one tick in five to ten.

## 2. The tile map as simulation truth

### 2.1 Why not the voxel chunks

- A chunk is 2×2×2 blocks with a `Vec<Block>` + `BTreeMap` chisel + a rapier collider, as an
  ECS entity (`voxel.rs`, `state.rs`). A 128 m × 120-level embark is 64×64×60 ≈ **246k chunk
  entities** *(estimate)*; the 128-chunk region measured 578 MiB raw snapshot and 3.9 s start
  with a much thinner vertical band (`CLAUDE.md` region-size notes).
- DF needs per-tile data the chunk lacks: shape (ramp/stairs/floor), hidden, designation bits,
  liquid level, traffic, inside/light/subterranean, occupancy.
- Grid agents don't use colliders.

### 2.2 `FortressMap`

A dense array over the embark box, x fastest, in 16×16×1 *map blocks* like DF (good for
dirty-tracking, meshing hand-off and per-block lists such as veins and spatter):

```
struct Tile {            // 8 bytes (estimate)
    material: MaterialId,   // u16, from materials.ron (stone, soil, ore, wood, construction…)
    shape: TileShape,       // u8: Wall, Floor, Open, Ramp, StairUp/Down/UpDown, Fortification, TreeTrunk…
    flags: TileFlags,       // u16: hidden, smooth, engraved, outside, light, subterranean, aquifer…
    liquid: u8,             // level 0..=7 + kind bit (water/magma), DF semantics
    designation: u8,        // dig kind + priority, or none
    traffic: u8,            // 1/2/5/25 class
}
```

128×128×120 ≈ 2M tiles ≈ 16 MB *(estimate)*; lz4 of mostly-uniform stone is tiny. It is the
**snapshot payload** for joining clients (instead of the voxel chunks) and the **save** payload
(§9).

### 2.3 From world artifact to tile map

Generation stays where it is: `worldgen::generate_region` (procedural or forged via
`VOXEL_WORLD_FILE`, `crates/worldgen/src/forged.rs`) produces block columns. The fortress
**converts blocks to tiles** once at embark:

- solid block → `Wall` of that block's material; air over solid → `Floor`; air over air → `Open`;
- a worldgen **half block** (one-cuboid `ChiselData`) at a surface step → natural `Ramp` (DF
  surfaces are ramps; ours already sit where the surface changes level);
- water blocks → liquid 7 (sea/rivers static in the MVP);
- material layers from the forge's geology (`forged.rs:109`, formations "not used by the engine
  yet") → stone layers + an ore vein table; needs new stone/ore blocks in the registry;
- `hidden` = every tile not exposed to the surface air (flood fill from open sky);
- tile attributes (outside/light/subterranean) from the same flood fill.

Using `generate_region` keeps the fortress's border identical to the LOD scenery the
neighbouring area renders from the same artifact (§6.4).

### 2.4 Edits

A dig/channel/build completion is one sim mutation: tile write(s) + revealed neighbours +
connectivity update (§4) + support check + one render update per touched map block. The old
`edit.rs` / `set_block_safe` (player raycast editing that keeps chunk data, render voxels and
colliders in step) is not on this path.

## 3. Netcode and authority

### 3.1 What fortress mode actually asks of the netcode

- Player input is **sparse and indirect**: designations, building placements, zone paints, menu
  toggles — a few per minute, never twitch input (`03-…` §1).
- Nothing the player sees needs zero-latency confirmation: a designation can draw instantly as a
  client-local "pending" overlay and become real when its tick arrives.
- The world state is **large and wide** (every unit, item, tile) and changes **every tick**
  (hundreds of agents), while inputs are tiny.
- MVP is single player; later co-op on one fortress, or neighbouring fortresses.

### 3.2 The options and their trade-offs

**(a) Keep rollback + client prediction (status quo).** The client runs ~1.5 ticks ahead
(`CLIENT_LEAD_MS`, `crates/client/src/instance.rs:350`), predicts its own inputs, and re-simulates
when authoritative events arrive. What it buys — instant feedback for the local player's own
actions — is what fortress mode doesn't need. What it costs grows with exactly what fortress
mode has a lot of: every mutation of every agent, item and tile goes through the undo log every
tick (typed deltas in `UndoMap`/`UndoSlotMap`, or snapshot `change()`), and a late input re-runs
*agents × depth* ticks. Derived caches (paths, component maps, job indices) either live in
rollback state (logged, hashed) or must be rebuilt after every rollback. And every new DF system
pays the "write a true inverse" tax forever. In single player with correct tick stamping, actual
rollbacks are rare — but the logging cost is paid every tick regardless.

**(b) Deterministic lockstep, prediction depth 0.** The server orders inputs and stamps each at
`head + delay`; it broadcasts the stream (it already does — inputs travel, not state); clients
**execute only confirmed ticks** and never predict. Nobody ever rolls back, so the undo log can
be bypassed for fortress state, and caches can be ordinary derived data rebuilt identically on
every replica. Bandwidth stays tiny; co-op on one fortress is free (more input sources into the
same ordered stream); the existing periodic state hash catches desyncs; joining is the existing
snapshot path with a tile-map payload. Costs: the **determinism tax stays** (integer/libm math,
ordered containers, no parallel nondeterminism — the discipline this codebase already has, and
`just determinism-wasm` already polices); input latency = the delay (~100–200 ms, invisible for
designations); **every client simulates the whole fortress**, so the weakest client (wasm,
single-threaded) bounds the speed dial; a slow client falls behind rather than slowing others,
as the RTS decision accepted.

**(c) Server-authoritative simulation, thin client (state replication).** Only the server
simulates; clients receive replicated state. Determinism and undo are both unnecessary; the
server could even multithread the fortress. Offline still works (the embedded `LocalServer`
shape, `crates/client/src/local_server.rs`). Costs: a replication layer the engine doesn't
have — interest management, delta compression, per-tick unit movement streams (300 units moving
every ~9 ticks at 5× ≈ 3,000 moves/s *(estimate)*), plus an on-demand **query protocol** for the
info-heavy DF UI (unit sheets, stocks lists of thousands of items); the render seam exists
(`GameDataUpdate`s are what the bridge consumes), the rest does not. It also walks away from the
choice already made twice for this engine — deterministic in-sim water and deterministic wasm
mods (memories `voxel-water-efficiency-research`, `modding-wasm-research`).

### 3.3 Recommendation

**(b), lockstep at depth 0, for the fortress region**, with the undo log bypassed for
`FortressState` once measurements say it matters (start with it on: correctness first, then a
measured switch-off — the `HashMode` and unlogged-genesis precedents show the pattern).

Why: it is the smallest change from today (same sim, same transport, same hashing, a pacing
mode), it removes the central technical risk named in the handoff (agents × rollback depth → 0),
and it keeps both doors open — a deterministic sim can always later be run server-only as (c)
if wasm clients can't keep up, while the reverse is not true. Keep (a) for the avatar in
adventure mode.

What has to be built (phase 1 of the plan): a per-region **pacing mode** in the client
(`instance.rs`: lead ≤ 0, no locally generated `Tick`s for a lockstep region; execute up to the
server's confirmed head); server-side **input stamping at `head + delay`** for lockstep regions
(today inputs carry client-predicted ticks); the client-local pending-designation overlay;
verification that nothing in the fortress path depends on prediction (the slimes/body path is
gone from it).

### 3.4 Later: co-op and neighbours

- **Co-op on one fortress**: free under (b) — several clients' inputs into one stream. Needs
  per-player permissions only if wanted.
- **Neighbouring fortresses**: separate regions, each its own lockstep stream. A client
  observing a neighbour subscribes to its stream (and simulates it) or sees it as LOD scenery.
  Interaction across the seam (caravans between fortresses, a siege crossing a border) is a
  handoff of units between regions — the existing `EntityArrived` pattern, at fortress
  granularity, at most a few times a season.
- **The k8s/Postgres direction** (`2026-09-20-multi-instance-k8s-postgres-research.md`) still
  fits: the region is the partition unit; a fortress is one region.

## 4. Pathfinding at scale

### 4.1 The graph

Nodes are tiles a unit can stand in (floor, ramp, stairs, shallow liquid ≤3/7). Edges: the 8
horizontal neighbours (diagonals allowed, including through diagonal gaps — DF rule), vertical
moves through stair pairs, diagonal-vertical moves through usable ramps (ramp + open above +
adjacent wall + walkable on top of it, `02-…` §2). Cost = the destination tile's traffic class
(high 1, normal 2, low 5, restricted 25) — DF's integer costs [wiki Traffic]. No float weights.

### 4.2 Algorithms, in the order to build them

1. **Connected components** (DF's approach, `03-…` §5). A `u32` component id per walkable
   tile, maintained incrementally: a dig/build that *joins* areas relabels the smaller side
   (union); one that *splits* (rare: walls, raised bridges, floods) re-floods the affected
   component. A path request first compares ids → unreachable requests cost O(1), which removes
   DF's worst pathing failure mode. Per movement class later (walker, flyer, swimmer) if needed;
   DF itself only keeps the walker index.
2. **A\*** with an admissible octile/Chebyshev-style heuristic in integer units, a binary heap
   ordered by `(f, h, tile_index)` so ties break identically everywhere; neighbour expansion in a
   fixed order. Path from target to unit or unit to target, consistently.
3. **Dijkstra fields for "nearest of a set"** (nearest food, nearest bed, nearest stockpile slot
   for item type X, nearest dig job): one multi-source flood from all members gives every unit
   its nearest member and the path. Rebuilt lazily when the set or the map changes, on a cadence.
   This replaces DF's obstacle-blind "nearest by Manhattan distance" material choice with true
   path distance at similar cost.
4. **Hierarchical A\*** over 16×16×1 map blocks (HPA\*) only if profiling says long paths
   matter. DF never needed it.
5. **Flow fields** per shared destination (meeting area, depot, a siege's target) — useful for
   crowds; same structure as (3).

Rejected: rapier's KCC per dwarf (shape casts against voxel colliders per step; built for one
walking avatar and already the measured hot spot, memory `client-prediction-runaway`); navmesh
(the map changes constantly; DF rejected even jump-point search for this reason, [SO-2021]).

### 4.3 Determinism and undo

Everything above is a pure function of the tile map and the request, with integer costs and
fixed tie-breaks, so every replica computes the same path. Under lockstep (§3) paths, component
ids and fields are **derived caches**, not logged state: they are rebuilt identically on every
replica, and they must be **excluded from the state hash or recomputed before hashing** so a
cache rebuilt at a different moment can't desync the hash. Under rollback they would have to be
logged or rebuilt after every revert — one more reason for §3.3.

### 4.4 Cost *(estimates; measure with a throwaway spike if Dawid agrees)*

A* on a 2M-tile map with a good heuristic and component pre-check: typical fortress paths of
50–200 tiles expand 10³–10⁴ nodes → 0.05–0.5 ms. With a path per job leg (hundreds of ticks),
300 units cost ~1–3 path searches per tick. Component relabelling on a dig: O(size of smaller
side), usually tiny; a split floods one component (up to the whole fortress, ~10⁵–10⁶ tiles,
1–10 ms) and is rare.

## 5. Agents

### 5.1 The unit loop

- **Wake-up queue**: `BTreeMap<(Tick, UnitId), Wake>` (deterministic order). A walking unit
  wakes when its next step is due (gait ticks per tile); a working unit when its job progresses
  or completes; an idle unit on a job-search cadence (DF's "1/120 chance per tick if idle" maps
  to `roll(tick, unit, salt)` from `slime.rs:121`).
- **Need counters as timestamps**: store `last_ate_tick`, `last_drank_tick`, `drowsy_since` and
  compute DF's counter = `now - last` on demand. Thresholds become *scheduled wake-ups* ("at
  `last_ate + 45,000` re-evaluate"). Zero per-tick cost, nothing to log.
- **Decision**: a priority list in DF's order — emergencies (flee visible hostile, drowning) →
  critical needs past their thresholds → current job → new job from the job board → needs below
  thresholds → idle (meeting area, socialise). A utility system can replace it later; keep it
  data-driven (labors, thresholds, priorities in raws).
- **Sensing**: spatial hash keyed by 16×16×1 map block; "who is hostile within 26 tiles"
  on a cadence (every 10 ticks), never all-pairs every tick (DF's v50 hot spot, `11-…` §3).
- **Randomness**: stateless `roll(tick, subject, salt)` everywhere (no RNG state, nothing to
  undo — the slime template).

### 5.2 Jobs and items

- **Job board**: jobs keyed by stable ids in a `BTreeMap`, indexed by labor and by map block;
  reservation of target + materials on claim; cancel → back on the board with a cooldown.
- **Items**: ECS entities with type + material + quality + location; indices by (type,
  material), by map block, and by container. No per-tick item processing (no temperature/wear
  polling in the MVP; `08-…` §5 for the event-driven later design).
- **Stockpiles**: tile sets with filters; "free slot" sets per stockpile feed the Dijkstra field
  for hauling (§4.2 (3)).

### 5.3 Where it goes in the code

A new `crates/game/src/fortress/` module tree (map, units, jobs, items, path, raws glue),
engine-free like the rest of `game`. It must derive `StateHash` + serde; it needs the
`#[rollback]` macro only as long as the undo log stays on for it (§3.3). Bevy stays out.

## 6. World and region scale

### 6.1 Units

1 tile = 1 block = 1 m = 16 voxels = 16 world units; a chunk is 2 tiles on a side; a region is
`REGION_CHUNKS × 2` tiles (`protocol.rs:46`).

| DF | tiles | ours |
|---|---|---|
| embark square | 48 | 48 m (not a power of two) |
| default embark 4×4 | 192 | between `REGION_CHUNKS` 64 (128 m) and 128 (256 m) |
| 3×3 embark | 144 | ≈ 64 chunks (128 m) + margin |
| region tile (16×16 squares) | 768 | 768 m |
| Pocket world 17² region tiles | 13,056 | ≈ 13 km |
| Smaller 33² | 25,344 | ≈ 25 km |
| our forge world | — | 16 km ≈ a 21×21 region-tile DF world (between Pocket and Smaller) |

### 6.2 Recommended embark

**One fortress = one region at `REGION_CHUNKS = 64` (128 × 128 tiles)**, between DF's 2×2 and
3×3 — the size DF players pick for FPS, and a quarter of the 256 m option's area and every
per-region cost (measured ∝ area). 256 m (`region_chunks=128`, ≈5×5 DF squares) stays a build
dial for later. The embark rectangle is chosen on the world map and snapped to the region grid
(regions tile the world in `RegionCoords`); arbitrary-offset embarks would need region origin
offsets — not worth it.

**Vertical**: from the highest surface in the embark + ~15 levels of sky, down to a floor
~60 levels below sea level in the MVP (no caverns, no magma); caverns later add depth. The
column model (`volume.rs`) is already unbounded vertically; the tile map's z-range is per embark.

### 6.3 From `.world` to world map to region

- The forge already simulates a 16 km ocean-bounded world and renders it (`render.rs`,
  `just world-map --world <file>`). The **world map screen** shows that render (relief, rivers,
  lakes, later biomes/sites) with a region-grid overlay; the player clicks a region → embark.
- The forge's 16 m cells are already finer than DF's world tiles; DF's "local" refinement is our
  `generate_region` sampling the artifact (`forged.rs`).
- Later the artifact grows a sites/civilizations layer (history, `01-…` §2) — keep the format
  versioned and extensible.

### 6.4 How much multi-region machinery runs in fortress mode

- **Simulated**: one region, the fortress.
- **Rendered**: neighbours as LOD scenery from the same artifact (the LOD rings,
  `lod_rings.rs`; no near window around the fortress: `world=tiny`-like near radius 0) so the
  fortress sits in a real landscape. The cutaway (§8.1) applies only inside the fortress box.
- **Off in the fortress path**: region handoff, ghosts, home flips, cell loading (a 64-chunk
  region needs none), the 5×5 near window. They stay compiled and tested
  (`just test-threads`) for adventure mode.
- Known issues that then **don't bite** the MVP: the client-crossing bit-inexact regression
  (no crossings), flight outrunning streaming (no flight), cell-mode neighbour limits. The one
  that **does**: the dropped-packet gap (memory `netcode-dropped-packet-gap`: `send_bounded`
  drops a packet after 2 s → the event stream has a hole → panic). Lockstep makes a hole fatal
  by construction; fix it in phase 1 (never drop, resync via snapshot instead).

## 7. Data-driven content ("raws")

Today: `assets/blocks/blocks.ron` + `game::registry` (`crates/game/src/registry.rs`): explicit
stable ids, a RON manifest, Bevy-free, loaded by both sides.

Grow it into `assets/raws/`:

| file | content (MVP subset first) |
|---|---|
| `materials.ron` | inorganic + organic materials: id, name, block/texture, colour, value, density, layer type, ore yields, flags (fire-safe, …); later the mechanical properties |
| `creatures.ron` | dwarf + one hostile + one animal: body template ref, attribute medians, gait, size, needs thresholds, labors allowed |
| `plants.ron` | crops (season, surface/underground, yields, seeds), trees (wood material) |
| `items.ron` | item types: boulder, log, bar, block, seed, plant, drink, meal, bed, door, table, chair, barrel, bin, pick, axe … |
| `buildings.ron` / `workshops.ron` | footprints, passability, materials to build |
| `reactions.ron` | every workshop job: reagents, products, skill, time, fuel (DF's hardcoded jobs become data) |
| `labors.ron` | labors, the skill each uses, default work details |
| `embark.ron` | the Play Now party and loadout |

Rules: explicit stable ids (append-only, like blocks); validated at load; a **content hash**
(crc32 of the canonical bytes) sent by the server and checked by the client — there is no
connect handshake today (`protocol.rs` notes), so add one; materials map to `BlockId`s for
rendering. Modding: raws are DF's primary modding surface and become ours for free (load extra
RON from a mods directory, hashed into the content hash). The wasm mod design (memory
`modding-wasm-research`: deterministic in-sim mods, stateless guests, host-owned undo-logged
state) later attaches *behaviour* to raws-defined things — custom reaction effects, new job
kinds, creature behaviours — through host functions over `FortressState`.

## 8. Rendering and the view

> **UI rule (Dawid, 2026-10-20, after this research):** every player-facing screen and panel —
> world map, embark, unit sheets, tile inspector, menus, announcements, build/designation
> toolbars — is built in **Slint** (`crates/slint-ui`, the `~/Projects/ui` widget library,
> rendered by `renderer/slint_overlay.rs`) wherever Slint can do it. Bevy-side UI is only for the
> F3 developer panel (`renderer/dev_ui.rs`). In-world overlays (designation tints, job markers,
> cutaway) stay renderer work, as below.

### 8.1 Z-level cutaway

Facts: the pulled-quad path draws per-chunk storage buffers of 8-byte quad records
(`renderer/meshing/records.rs`, `assets/shaders/voxel_pull.wgsl`), back-face culled
(`voxel_pull/pipeline.rs:132`); the mesher costs ~161 µs per chunk (memory
`chunk-loading-speed-investigation`) and skips buried chunks (`meshing/queue.rs:226`).

Remeshing the chunk layer at the cut on every wheel step would be 64×64 = 4,096 chunks
≈ 0.66 s single-threaded at 128 m *(estimate)* — too slow for a scroll wheel. Instead:

1. **Clip**: a `cut_y` uniform; the vertex stage collapses quads entirely above the plane, the
   fragment stage discards fragments above it (quads that straddle). Applies inside the fortress
   box only; also in the shadow pass (or shadows off inside the box).
2. **Cap**: a second draw of the chunk layer(s) under the cut with **front-face culling**. Where
   the view ray enters solid rock at the cut plane, the first surface it hits is a *back* face;
   the cap shader moves that fragment onto the plane (writes `frag_depth` — the pipeline already
   does depth emulation, `pipeline.rs:221`) and colours it from a **slice texture**: a 2D
   R16 texture of the tile materials at the current level (+ hidden flag → black). Where the ray
   passes through air below the cut it hits a front face first, which wins the depth test — so
   the cap only appears over solid tiles, by construction.
3. **Depth shading**: lower levels darkened by distance below `cut_y` (DF draws the level below
   darker).
4. **Stepping a level** = write `cut_y` + upload one 128×128 slice texture. No remesh.

Risks: the cap needs closed meshes (true for greedy meshes with neighbour culling, worth a
test); chisel shapes straddling the cut show their interior; LOD scenery outside the box is not
clipped (intended).

### 8.2 Hidden tiles

The client has the whole map (lockstep), so hiding is a render rule: when deriving render voxels
from tiles, a hidden tile becomes solid "unknown" material, and its faces never show. Revealing
a tile marks its map block dirty. In the slice texture hidden is black. (Hiding is not
anti-cheat under lockstep; it doesn't need to be for single player/co-op.)

### 8.3 Overlays and markers

- **Designations, stockpiles, zones, traffic**: a per-level overlay texture (same 128×128 grid
  as the slice texture, one byte per tile → colour/pattern in the shader) drawn on the floor
  tops at the current level; designations below the cut drawn faded. Cheap and exact.
- **Placement previews**: client-local ghost meshes/bbmodels, green/red validity from a
  client-side check of the tile map (the sim re-validates).
- **Job markers, status icons** (sleeping, eating, hungry): billboards over units, from unit
  state in the render updates.
- **Pending designations** (input sent, tick not yet confirmed): the same overlay, dimmed.

### 8.4 Creatures and items

- **Units** render as Blockbench models (`crates/bbmodel`, `renderer/model.rs`,
  `BbModel` + `BbAnimationPlayer`) — walk, work, sleep clips; positions interpolate between
  tiles over the gait period (the sim moves tile to tile; the renderer must not).
- **Items**: thousands → instanced draws per item type (one mesh per type, a per-instance
  material tint); a tile shows its top item or a pile marker.
- Art from the asset pipeline (`tools/assets/`: Leonardo picture → TRELLIS.2 GLB) or
  hand-made `.bbmodel`s; low-poly budget per model since hundreds draw at once.

### 8.5 Picking

`GroundPick` (`commander/selection.rs:50`) picks the terrain surface under the cursor.
Fortress picking is **tile under the cursor at the current level**: intersect the view ray with
the `cut_y` plane (and with the floor of that level), then walk down through open tiles
— exact, and it uses the tile map, not the height grid. Units/buildings are picked first.

### 8.6 Commander camera

Keep: fixed pitch, pan, zoom, minimap (`renderer/commander/`). Change: focus clamped to the
fortress box; wheel = z-level step (zoom on Ctrl+wheel, DF's binding); the minimap shows the
current level; the height grid only drives camera height *above the cut*.

## 9. Persistence

A fortress lasts hours and DF fortresses persist in the world (`13-…` §3). The parking lot is
in-memory (`world_manager.rs`); the durable-persistence design
(`2026-09-20-durable-world-persistence-design.md`) is a draft built around players and
Postgres. MVP: **save/load the fortress region to a file** — `FortressState` serialized with
bincode + lz4 (the `encode_packet` framing), plus the world artifact's crc and the raws content
hash. Postgres rows later, per that design.

## 10. What to remove or park

| thing | now | fortress MVP |
|---|---|---|
| Commander selection (`selection.rs`, RTS click/box as sim keys) | default | **repurpose**: click = inspect unit/tile; drag = designation rectangle on the current level |
| Slimes (`slime.rs`) | spawn in every region | **off in fortress regions** (spawner gated by `RegionKind`); keep the module as the AI template and test asset |
| First-person body + KCC + `movement.rs` | inert, hidden (`LocalBody`) | **no body in fortress regions**; keep for `VOXEL_CAMERA=first-person` / adventure mode |
| Block break/place (`edit.rs`) | sim raycast from the eye | **not in the fortress path** (dwarves dig); keep for adventure mode |
| Rapier in the fortress | per-chunk colliders | **none** |
| Multi-region handoff, near window, cell mode | live | **parked** (compiled + tested, not used by fortress) |
| LOD rings | live | **kept** as scenery around the fortress |
| Forged world (`forged.rs`, `just forge-proto`) | opt-in | **central**: the world file is the world |
| `MapConfig` from `ds/rtsPivot` | not on develop | not needed: the forge config is the world config |

## 11. Determinism and tests

- Every fortress type derives `StateHash`; no `HashMap`/`HashSet` iteration in the sim; no
  `f32` transcendentals (integer tiles make this easy); `roll()` for randomness.
- New wasm determinism scene: a seeded embark with N dwarves running M thousand ticks;
  `game::state_hash` equal on native and wasm (`just determinism-wasm`).
- Seeded randomized tests for incremental connectivity (compare against a full re-flood after
  every random dig/build), A\* optimality against Dijkstra, job reservation invariants.
- A lockstep convergence harness: server + headless client, random designations, hashes equal
  every K ticks (client test binaries are a slow opt-in — ask first, CLAUDE.md).
- Performance gate: a release-mode tick-cost test at 300 units (tagged slow).

## 12. Risks, in order

1. **Agent cost in wasm under lockstep** — every client sims the fortress; the browser is
   single-threaded. Mitigation: event-driven units (§5), measure early (phase 3), fall back to
   option (c) for weak clients if needed.
2. **Lockstep pacing changes in the client** (`instance.rs`) — touches subtle code
   (`align_region`, SyncClock); the threaded handoff tests guard the wild path; the fortress
   path needs its own convergence test.
3. **Dropped-packet gap** becomes fatal under lockstep — fix in phase 1.
4. **Cutaway cap correctness** with chisel geometry and at chunk borders.
5. **Content volume**: DF is decades of systems; the raws + tiers in `00-synthesis.md` keep the
   MVP bounded, but each "soon after" item is real work.
6. **Tile scale** decision (Q3) changes art and every number; decide before phase 1.
