# Voxel water efficiency, part 3: networking, rendering, update rate

**Date:** 2026-09-22 · **Status:** research only, nothing implemented  
**Scope:** (A) how to keep dynamic water in sync across clients, (B) how to draw it cheaply
(near, pulled-quad path, LOD rings, WebGL2), (C) how to run the water sim at 2–5 Hz and still
have it look smooth.  
**Related:** `docs/2026-07-15-water-implementation-plan.md` (the July plan),
`docs/superpowers/specs/2026-09-22-rapier-water-physics-investigation.md`.  
Line and file references are as of the uncommitted 2026-09-22 tree.

## 0. The facts that decide it

- **Scale.** 1 voxel = 1/16 m; 1 block = 16³ voxels = 1 m; 1 chunk = 32³ voxels = 2×2×2 blocks
  = 2 m; 1 region = 32×32 chunk columns = 64 m (`REGION_SIZE` 1024 units). `TICK_RATE` = 50 ms.
- **The client predicts the whole near window.** `DEFAULT_NEAR_RADIUS = 2`
  (`client/src/world_scale.rs`), so every client runs a predicted copy of **25 region sims**.
  On a correction it rolls back and replays the log. The *client prediction runaway* note in
  memory says that replay already dominates client sim cost, and physics is ~99.8% of that.
- **The wire carries events, not state.** A `ServerPacket::GameEvent` in the region's event log
  (`GameEventKind::{Tick, PlayerInput, EntityArrived, GhostUpdate, …}`) is the only thing that
  changes a running region. `ServerPacket::Region` is the full snapshot on subscribe.
  `LodData(LodKey, LodPayload)` streams far terrain. Everything is framed by
  `encode_packet`, and packets over 4 KiB are lz4-compressed.
- **Relayed events already have a prediction-matching path.** `EntityArrived` and
  `GhostUpdate` are relayed events that the client can predict and later match by identity
  (`GameEventKind::matches_prediction`). Scheduled water updates can reuse this path (§A.4).
- **Far terrain refreshes on a cadence.** A running region checks its terrain hash every
  `TERRAIN_HASH_INTERVAL_TICKS` = 25 ticks (1.25 s). On a change it pushes `TerrainChanged`,
  and the manager invalidates and rebuilds **every LOD key** covering that region.
- **Near rendering.** The near path uses pulled quads: 8-byte `QuadRecord`s
  (`meshing/records.rs`) in per-chunk storage buffers or one arena
  (`voxel_pull/arena.rs`). A GPU-driven mode draws them with multi-draw-indirect
  (`voxel_pull/queue.rs`). The layout is locked to `voxel_pull.wgsl`. `face_ends` returns
  `[u32; 8]`, but only 6 face ranges are used, so two slots are free.
- **Depth prepass.** It is off by default: `DepthPrepass` is only added with
  `VOXEL_OCCLUSION=on`, because it re-draws every quad.
- **WebGL2 (wasm).** It has no storage buffers in the vertex stage, so it falls back to
  per-chunk `Mesh` + material. MSAA is forced `Off` (`renderer/config.rs`). In Bevy 0.18 the
  depth-prepass texture *can* be bound on WebGL2 only when MSAA is off
  (`bevy_pbr-0.18.1/src/render/mesh_view_bindings.rs`, the `msaa.samples() == 1` branch).
- **The sea is on every LOD lattice.** `SEA_LEVEL` = 1024 voxels (64 m), a multiple of the
  coarsest cell (factor 32 = 2 m). Ring table at 32-chunk regions:
  `[(2,2),(3,4),(6,8),(13,16),(16,32)]`, horizon 16 384 units = 1 km, and fog is opaque there.

---

## A. Networking and sync of dynamic water

### A.1 What shipping games do

| Game | Who simulates | What goes over the wire | Rate | Notes |
|---|---|---|---|---|
| **Minecraft (Java)** | Server only. The client never ticks fluids. | A single change is `Block Update`. Two or more changes in one 16³ section on one tick become `Update Section Blocks`: an 8-byte packed section position, then a VarInt count, then one VarLong per block holding `stateId << 12 \| x<<8 \| z<<4 \| y`. Water states are small ids, so each entry is **~3 bytes**. | Flow runs every 5 game ticks (4 Hz). It spreads 1 block per step, 7 blocks max on flat ground. | Water pushes entities with a flow vector summed from the four neighbours, ~1.39 m/s. Clients see flow one RTT late, and nobody notices at 4 Hz. [protocol](https://minecraft.wiki/w/Java_Edition_protocol/Packets#Update_Section_Blocks), [water](https://minecraft.wiki/w/Water) |
| **Luanti (Minetest)** | Server. `transformLiquids` works through a queue. | Changed MapBlocks (16³) are re-sent as block data. Liquid updates are marked low priority. | `liquid_update = 1.0` s (1 Hz); `liquid_loop_max = 100000` nodes per step; `liquid_queue_purge_time` drops the backlog. | This is the "coarse sim, bulk resend" end of the spectrum. [conf](https://github.com/luanti-org/luanti/blob/master/minetest.conf.example), [low-priority commit](https://github.com/luanti-org/luanti/commit/00addc3e5d3616f23debbfa7efd4236d2b6fdbbe), [docs](https://docs.luanti.org/for-players/liquid/) |
| **Terraria** | Server (`Main.netMode == 2`). | Per-tile liquid messages via `NetMessage.SendData`. A flood can trip **panic mode** (`numLiquid + numLiquidBuffer > 13500`, or a stuck counter). The server then `QuickWater`-settles the whole map and marks every tile section (200×150 tiles) as unsent for every client, forcing a bulk resend. | `cycles` = 30 on the server. | Players report being kicked for "liquid spam" from floods. Precedent worth copying: when the active set explodes, settle it and resync with a snapshot instead of streaming deltas. [Liquid.cs](https://github.com/TheVamp/Terraria-Source-Code/blob/master/Terraria/Liquid.cs), [NetMessage docs](https://github.com/tModLoader/tModLoader/wiki/NetMessage-Class-Documentation), [kick thread](https://steamcommunity.com/app/105600/discussions/0/619569608522229852/) |
| **Vintage Story** | Server. `BlockBehaviorFiniteSpreadingLiquid` runs as ordinary block updates. | Block updates. | Game tick. | Tick cost only while something changes, such as a wetland forming. [lag notes](https://www.vintagestory.at/forums/topic/18510-known-sources-of-lag/), [wiki](https://wiki.vintagestory.at/Water) |
| **Enshrouded** (Wake of the Water, Nov 2025) | Not disclosed. Dedicated servers exist, so almost certainly server-side. | Not disclosed. | Not disclosed. | Voxel water "spills out indefinitely up to the level of the spring". Sand absorbs water. Water is kept non-destructive to bases. A server role was added to limit world manipulation (griefing by water). [interview](https://www.pcgamesn.com/enshrouded/wake-of-the-water-interview), [news](https://enshrouded.com/en-US/news/enshrouded-wake-of-the-water-update) |
| **Timberborn** (single player) | Local. A 2D **virtual-pipe** heightfield (Mei et al. 2007): depth per tile plus pipe flux to four neighbours, with momentum taken from the last tick's movement. | Not applicable. | "Every couple hundred ms" (~5 Hz). | "Water simulation and moisture are updated in ticks, the visuals need to be rendered continuously". Momentum is shown by texture animation. [deep dive](https://www.gamedeveloper.com/design/deep-dive-timberborn-s-water-mechanics), [Mei 2007](http://www-evasion.imag.fr/Publications/2007/MDH07/FastErosion_PG07.pdf) |
| **Teardown** | Local. No flow at all. | Not applicable. | Not applicable. | Water is **one flat plane per level plus buoyancy**. The surface is a single quad, with the shoreline made by discarding fragments by depth. [devlog](https://blog.voxagon.se/), [80.lv](https://80.lv/articles/teardown-developer-breaks-down-multiplayer-and-voxel-destruction-tech) |
| **From Dust, RTS lockstep** | From Dust was single player, with a GPU shallow-water model. I found no lockstep or rollback game shipping *dynamic* water. Lockstep RTSes keep water static, a terrain type. | Not applicable. | Not applicable. | Nothing published to copy. |

**Pattern.** Every multiplayer game with flowing water runs it **server-authoritative at
1–5 Hz and ships changed cells**. None puts dynamic water in the client's predicted or
rollback set. The one cheap trick they all rely on is that water is slow, so a delay of one
RTT plus one step is invisible.

### A.2 Our three options

#### (1) Fully predicted, deterministic water in the rollback sim (the July plan)

The integer cellular automaton (CA) runs every N ticks inside `GameData` on the server and on
every predicting client. Only the edit events go on the wire.

- **Bandwidth:** about 0. The dam-break edit is one ~30-byte event.
- **Client CPU, the real cost.** Every client steps water for all 25 predicted regions.
  - Consider a drain with ~3,000 active cells: a 32×32 m lake top layer plus the outflow and
    a wet front. That is ≈ 18k edge relaxations per step, roughly 0.1–0.3 ms native and
    2–3× that on wasm.
  - **Every rollback replays it too.** A 20-tick replay contains 5 water steps at 5 Hz, which
    adds ~1 ms per server correction per active region.
  - Undo cost is fine: one before-image per touched chunk per step. A top layer spans ~256
    chunks of ~40 bytes, so ~10 KB per step.
- **Cross-region flow must be deterministic on the clients as well.** Plan Phase 3 (manager
  strip pairing, `ApplyBoundaryWaterFlux`) has to survive client prediction. Region threads
  have no tick barrier, so this is the hardest part of the whole plan.
- **Solver lock-in.** The solver must be bit-exact integer code on native and wasm32, with a
  fixed traversal order, forever. It can't use floats, SIMD reductions, threads, or a better
  model (virtual pipes with momentum) without integer rework. Any rule change desyncs old
  clients.
- **Upside:** the player who breaks the dam sees water move on the same frame, with zero
  latency.

#### (2) Server-authoritative water outside rollback; the client renders the last received state

The server simulates. The client receives compressed per-chunk deltas at low rate, renders
them and interpolates, and player physics samples "the last received water".

- **Bandwidth:** see §A.3.
- **The flaw: player movement mispredicts every tick near changing water.**
  - At tick T the server's swim controller used the server's water at T. The client, which
    predicts ahead by ~RTT/2 + 1.5 ticks, used water it received about one RTT earlier.
  - Whenever the level or front changes near the player during that gap, predicted and
    authoritative positions differ, and the client rolls back.
  - Worst case, a flood front arrives. Front speed is ~1–5 m/s (1 cell per step at 5 Hz is
    5 m/s), and the stale window is ~RTT + one step ≈ 300 ms at 100 ms RTT. The client treats
    the player as dry for ~0.3–1.5 m of front travel. At walk speed 4.3 m/s against a swim
    speed of ~2 m/s, that is ~0.3–0.7 m of position error, corrected by rollback. That is
    visible rubber-banding, the same class of bug already fought in *input-slot
    rubber-banding*.
  - A slowly draining lake (level falling ~0.27 m/s) only causes centimetre-level buoyancy
    differences.
  - Hashes break as well, because the interval state hash can't cover water that lives
    outside the sim.
- **Verdict:** unsound for a sim where water affects movement. It is fine only for purely
  cosmetic water.

#### (3) Hybrid: water is rollback state, but its *evolution* arrives as server-authored events

This is recommended; §A.4 has the details.

- Water cells live in `GameData`: hashed, in snapshots, parked with the region. Settled water
  (worldgen seas, lakes, sleeping bodies) is static state that costs nothing per tick.
- Terrain edits apply their **immediate, local, deterministic** consequences in the sim.
  Placing solid displaces water (the July plan's rule), and removing solid only *wakes*
  neighbours.
- The **flow step is not run by the sim.** A server-side solver computes each step from the
  region's water, and the result enters the region's event log as a
  `GameEventKind::WaterStep` carrying absolute per-cell levels. Server region and client
  region both just *apply* it.
- From the rollback machinery's point of view the solver is one more input source, like a
  player.

### A.3 Bandwidth for a big drain event

Assumed encoding for one `WaterStep`:
- Per touched chunk: region-local chunk index (varint, ~2–3 bytes), an 8-bit mask of which of
  its 8 cells changed, then one byte per changed cell. The byte holds the level in 1/16 m
  steps and a flow-direction code. Render only needs 1/16 m, the voxel height.
- Deltas are sorted and hand-packed into `Vec<u8>`, not bincode'd structs, because
  `bincode::serialize` uses fixed-width ints.
- Then lz4 via `encode_packet` when over 4 KiB.

| Scenario | Changed cells per step | Raw per step | After lz4 (est.) | At 5 Hz | At 2 Hz |
|---|---|---|---|---|---|
| Player drains a pond (8×8 m surface) | ~150 | ~0.4 KB | ~0.3 KB | 1.5 KB/s | 0.6 KB/s |
| Lake 32×32 m falling + outflow + wet front | ~3,000 (≈400 chunks) | ~6 KB | ~2.5–3 KB | **12–15 KB/s (~100 kbit/s)** | 5–6 KB/s |
| Dam break spreading over 100×100 m | ~10,000–15,000 | ~25–35 KB | ~10–15 KB | **50–75 KB/s (~0.5 Mbit/s)** | 20–30 KB/s |
| Minecraft-equivalent of the dam break (3 B/block, section headers) | same | ~35–50 KB | none | ~140–200 KB/s at 4 Hz | — |

These go only to clients subscribed to the region, which is the near window. For comparison,
a region snapshot is ~0.9 MiB after lz4.

**Two multipliers that are easy to kill:**
1. **Body-level records.** A connected, quasi-flat body (a lake draining through one outlet)
   changes every surface cell by the same amount. Send `WaterBodyLevel { body_id, level }`
   instead of 1,024 cells. That cuts the lake row by ~10×. This is the column/heightfield
   insight from Timberborn.
2. **The Terraria escape valve.** If a region's delta stream exceeds a budget (say
   32 KB/step), the server quick-settles the frontier and ships a region water snapshot
   (a `WaterChunk` map, RLE) instead of deltas.

**LOD rings are the hidden cost.** A water change flips the region's terrain hash every
1.25 s during a drain. Each flip rebuilds and resends 4–5 `LodKey` payloads to every client
that has this region in its rings. Keep water **out of** the terrain LOD fingerprint.
Instead ship a separate small per-key water-surface grid (§B.4) on its own slow cadence, for
example only when a body settles, or every ~5 s at most.

### A.4 The recommended sync design in detail

1. **State.** Use the July plan's `WaterChunk` map in `GameData`: `u16` amount per 1 m cell,
   and an active/sleep mask that stays authoritative. The per-tick sim does **no** flow work.
2. **Solver placement.** Each server region thread runs the flow solver every
   `WATER_STEP_TICKS` (4 → 5 Hz; 10 → 2 Hz). Its input is the region's current water plus
   the *pending, not-yet-applied* `WaterStep`s (an overlay). Its output is absolute cell
   levels for cells that changed.
   - Absolute values make application **idempotent and self-healing**. A conflicting edit,
     such as a block placed into a cell that a pending step fills, is resolved the same way on
     both sides: never write water into solid. The solver sees the corrected state on its
     next step.
   - The solver doesn't have to be deterministic, rollback-able or integer-only. It can use
     floats, SIMD, threads, virtual pipes with momentum, or GPU later. Only *application* is
     part of the deterministic contract.
3. **Scheduling lead: zero mispredictions from water.**
   - The server stamps each step "apply at the start of tick `T + WATER_APPLY_LEAD_TICKS`"
     and sends it **immediately**, instead of when it reaches that tick.
   - The client queues it. When its predicted sim reaches that tick, it inserts the step as a
     predicted event, and the later authoritative copy matches by `(region, step_seq)` through
     `matches_prediction`, as `GhostUpdate`/`EntityArrived` already do.
   - The authoritative region inserts it into its own log when it reaches that tick. It
     applies at the start of the tick, before inputs, like `tick_slimes_safe`, so its slot is
     deterministic.
   - A client ahead of the server by `lead_c` needs `one_way + lead_c < LEAD`. With
     `lead_c ≈ one_way + 1.5 ticks`, that is `LEAD > RTT + 75 ms`, so **6 ticks (300 ms)**
     covers 200 ms RTT.
   - The 300 ms is invisible, because water already reacts on the 200 ms step clock and
     Minecraft plays fine at an RTT of lag.
   - A late packet falls back to an ordinary rollback and replay. Replays only re-*apply*
     steps, which is a memcpy-sized cost, never re-solve them.
4. **Edit responsiveness.** Breaking a dam is predicted locally, so the block vanishes on
   the same frame. The water starts moving `LEAD` + one step later, 300–500 ms.
   - If that feels slow, add a **render-only** local preview: the client runs the same solver
     cosmetically on the few cells around its own edit until authoritative steps arrive.
   - That preview is never read by the sim or hashed. Treat it as optional polish, not v1.
5. **Cross-region flow becomes a server-only problem.**
   - Solvers exchange one-cell boundary strips through the manager, and the lower region of
     each pair owns each seam edge.
   - The flux is folded into each side's next `WaterStep`, with a `(pair, step)` idempotency
     token for the mass ledger.
   - Clients never see seams: they apply two regions' absolute levels. Timing jitter between
     region threads can cost a step of seam lag, but it can't desync anyone.
6. **Hash coverage.** The water map is in the interval state hash, so any
   application-divergence bug shows up as the existing desync signal.

**Costs:**
- Server CPU per active region per step: the same solver cost as option (1), paid **once**,
  not 25× per client × replays.
- Client CPU: applying deltas only, microseconds.
- Bandwidth: §A.3, typically ~0–15 KB/s, and 50–75 KB/s during a large flood.
- New code: `WaterStep` event plus application, the client scheduled-event queue with
  prediction matching, the region-thread solver hook, and the manager strip relay.

---

## B. Rendering efficiency

### B.1 How others draw water

- **Minecraft (vanilla/Sodium).**
  - Water is a separate *translucent* render layer.
  - Fluid geometry has per-corner heights, each corner the average of the neighbouring
    levels, so flowing water slopes. The flowing texture is rotated along the flow vector. Top
    faces touching the same fluid are culled.
  - Translucent quads are sorted back to front *within* each 16³ section, and sections are
    drawn in sorted order.
  - Sodium's GFNI sort decides per section whether it needs no sort, a static sort or a
    dynamic sort. Faces with only up/down normals, which covers ordinary water bodies, need
    no dynamic sort because "no face can be seen through another one". It re-sorts only when
    the camera crosses a face plane.
  - Sources: [GFNI write-up](https://hackmd.io/@douira100/sodium-sl-gfni),
    [Sodium fluid renderer](https://deepwiki.com/CaffeineMC/sodium/3.2-block-and-fluid-rendering).
- **Iris/OptiFine shader packs.** These add waves, refraction and absorption in the
  `gbuffers_water` program.
  - The engine gives packs **`depthtex0`** (depth with translucents) and **`depthtex1`**
    (depth *before* translucents). A water fragment gets its water thickness from
    `depthtex1 − own depth`, with no second geometry pass: the depth buffer is copied between
    the opaque and translucent passes.
  - Sources: [Iris depthtex](https://shaders.properties/current/reference/buffers/depthtex/),
    [OptiFine shaders doc](https://github.com/sp614x/optifine/blob/master/OptiFineDoc/doc/shaders.txt).
- **Distant Horizons.** LOD water is its own translucent LOD pass, with a "Transparency"
  option: Complete / Fake / Disabled. Its long-running bugs are exactly the ones to design
  around:
  - LOD water drawing over the player or near terrain when composited with a shader pack;
  - LOD vs vanilla water at the handoff;
  - sources: [Iris #3146](https://github.com/IrisShaders/Iris/issues/3146),
    [DH #503](https://gitlab.com/jeseibel/distant-horizons/-/issues/503).
- **Voxy.** Water, glass and ice are rendered in the LOD translucent pass with blending. Its
  ports advertise "consistent water surface height across LOD levels" and correct underwater
  fog. LOD water animation uses a static frame up close.
  [voxy](https://github.com/MCRcortex/voxy), [forks](https://github.com/MountainOfFrowiningBodies/voxy-forged).
- **Teardown.** The whole water surface is **one quad**, and fragments below the terrain are
  discarded by depth, so the shoreline is free.
- **Timberborn.** It draws a 2D water sheet over terrain, animated by a flow texture.
- **Cheap motion.** Use two-phase flow maps (Vlachos, *Water Flow in Portal 2*, SIGGRAPH
  2010): scroll a normal map along a per-vertex flow vector, cross-fading two phases offset by
  half a period so it never stretches.
  [paper](https://alex.vlachos.com/graphics/Vlachos-SIGGRAPH10-WaterFlow.pdf),
  [tutorial](https://catlikecoding.com/unity/tutorials/flow/texture-distortion/). Put
  low-frequency geometry in the mesh and high-frequency waves in normals
  ([GPU Gems ch. 1](https://developer.nvidia.com/gpugems/gpugems/part-i-natural-effects/chapter-1-effective-water-simulation-physical-models)).

### B.2 Meshing: fit to our geometry

- **Heights land on the voxel lattice.** Water levels quantized to 1/16 m are exactly voxel
  heights. A water top face at any level is therefore a normal `QuadRecord`-style quad with
  integer `y` in 0..32. No fractional coordinates, no new vertex format for positions.
- **Greedy top faces.** Merge cells whose `(level, flow_code)` match. A settled sea merges
  into one quad per chunk, or per tile at the granularity below. Flowing water merges less,
  and that's fine.
- **Side faces** only where the neighbour's level is lower and the neighbour isn't solid.
  They are drawn from the neighbour's level up to ours. Waterfalls are columns of side faces.
- **Culling.** Cull water faces against solid and against equal-or-higher water. Never emit
  the underside unless the camera can be underwater: underwater views show the surface from
  below, so emit top faces two-sided (no backface cull on the water pipeline) rather than a
  second quad.
- **Mesh granularity.** Don't mesh water per 2 m chunk. A sea across the 5×5 near window is
  320×320 m, which is 25,600 chunk columns, and that many sorted phase items or entities would
  cost milliseconds of CPU; the opaque path has already hit that wall (see the 57k-entity
  extract note in `voxel_pull/mod.rs`).
  - Mesh water per **water tile** of 16×16 chunk columns (32×32 m), with the vertical span it
    covers. That is ~4 tiles per region and ~100 in the near window.
  - A settled sea tile greedy-merges to ~1–16 quads.
  - The tile remeshes when any of its cells change.
- **Keep water out of cave-culling connectivity and AO**, as the July plan says. Water must
  not close the portal flood (`visibility.rs:79,89`).

### B.3 The pass: transparent, sorted, or neither?

**Recommendation: "single-layer" water.** Draw after opaque with blending, **depth test on
and depth write on**, and use one pipeline. No per-quad sort and no OIT.

- Each pixel then shows only the nearest water surface. Everything under it is already in the
  colour buffer from the opaque pass.
- The only artefact is water-behind-water: a far lake seen through a waterfall sheet, or
  through a pond surface. The far water is hidden instead of blended. Minecraft players live
  with worse.
- GFNI's result confirms that horizontal surfaces need no sort.
- Order tiles front to back for early-z, so it is the opposite of a painter's sort.
- Bevy's `Transparent3d` sorts phase items by distance, which with ~100 tiles costs
  microseconds. A custom phase item keeps it one multi-draw on the GPU-driven path.
- Skip OIT: it costs memory, needs storage buffers, and so is unavailable on WebGL2.

**Absorption and depth tint without a depth pass.** The prepass is off by default because it
re-draws all terrain.

- **(a) Baked water depth, recommended for v1 and the only option on WebGL2.**
  - The mesher knows, per column, the distance from the surface down to the first solid
    voxel. Store it per quad in the record: the floor depth in 1/4 m steps, 6–8 bits.
  - Shade `alpha/colour = 1 − exp(−σ · depth / max(|view_dir.y|, ε))`, where the vertical
    thickness divided by the view cosine approximates the view-path length.
  - It costs nothing per frame and works on every backend.
  - It is wrong only under overhangs and for side faces; side faces use the horizontal
    distance or a constant.
  - Shallow-shore foam comes from the same number (`depth < 0.3 m`).
- **(b) Copy depth after opaque** (Iris `depthtex1` style), native only as a quality upgrade.
  - Add a render-graph node between `MainOpaquePass` and the water draw that copies the view
    depth into a sampled texture. That is one full-screen copy, ~0.1 ms at 1080p, not a
    geometry re-draw.
  - It gives exact thickness and the soft-shore fade.
  - Treat WebGL2 depth-texture copies as unverified.
- **(c) Enable `DepthPrepass`.** The shader gets `prepass_depth()`, including on WebGL2 since
  MSAA is off, but at the cost of re-drawing every terrain quad. Rejected; that is the cost
  that got it turned off.

**Refraction.** Bevy's `Transmissive3d` screen-space transmission copies the opaque colour
target (`view_transmission_texture`). It is a later polish item, gated on native. Faking it
with the normal map offsetting the absorption tint is enough for v1.

**Motion.** Greedy quads have 4 vertices, so there is no vertex displacement.
- Use two scrolling normal maps, or procedural noise normals, with Fresnel against the
  skybox/atmosphere colour and sun specular.
- Flowing cells scroll along the per-quad flow code with the two-phase flow-map trick.
- Waterfalls are side faces with a fast downward scroll plus foam at the base, where the
  level drops by more than 1 block.
- All render-time and hash-free.

### B.4 Water in the LOD rings

The candidates:

1. **One global sea plane at `SEA_LEVEL`** (Teardown-style): a camera-following quad and
   depth test against terrain.
   - It is nearly free and matches the sea on every lattice.
   - It is **wrong** for forged-world lakes and rivers above sea level. It also floods any dry
     land below 1024 that is visible, such as caves, forged below-sea basins, or a drained
     reservoir.
   - Only acceptable as the beyond-horizon backdrop, and the horizon is fog-opaque, so it is
     not needed.
2. **Water as opaque any-solid LOD volume (today).**
   - Draws the sea as a painted floor.
   - Cells round up, so shallow shelves and coasts shift with the ring factor.
3. **Recommended: a per-key water *surface raster* beside the terrain cells.**
   - `LodPayload` gains, per LOD column, "no water" or "surface height in cells plus floor
     depth". Build it top-down: the topmost water run in each column group, with the
     water-vs-land decision made by what is visible from above, as `merge_group` already does
     for material.
   - Terrain cells below it are the opaque seabed, built as today but with water treated as
     air.
   - The client meshes it with the *same* water record and pipeline at the ring's scale.
     - The sea greedy-merges to a handful of quads per key, because it is all at 1024.
     - Floor depth gives absorption, so shallows read lighter at 1 km, which a painted floor
       can't do.
     - No sorting, because it is the same single-layer rule.
   - It fixes both DH-class bugs by construction:
     - The surface height is exactly `SEA_LEVEL` at every factor (Voxy's "consistent water
       surface height").
     - Ring handoff is plane-to-plane, with no LOD water over near geometry, because each key
       only covers cells outside the near window.
   - Refresh it on the slow cadence from §A.3, not with terrain.
   - Coastline caveat: with any-solid seabed rounding, a coarse cell whose seabed rounds up
     to or above the surface must render land. Resolve per column: `surface > seabed_top`
     means water.

**Far-voxel (DDA) path.** `far_voxel/dda.rs`/`brick.rs` ray-march bricks. Treat water as
empty for the march. Draw the LOD water surface as rasterized quads in the same water pass,
so the far path needs no transparency.

### B.5 A separate record type in the same storage buffer?

**Possible:**
- Two `face_ends` slots (6, 7) are unused.
- Water quads need no AO: 8 AO bits plus `flip` are free in w0.
- A water "layer" needs a few bits of w1's 16-bit layer field.
- So a water record could be an 8-byte `QuadRecord` with the AO bits reinterpreted
  (e.g. `prev_level_delta:6 | spare:2`), `layer:4 | flow:6 | depth:6` in w1, and a water
  range in `face_ends[6]`.

**Recommended instead: its own buffer, per water tile, and its own pipeline.** Reasons:
1. **Lifecycle.** During a drain, water re-uploads at 5 Hz while terrain almost never
   changes. Sharing a chunk's buffer means re-uploading the terrain records with every water
   step and churning the arena.
2. **Different pass and state.** Blend, two-sided, depth-write order after opaque. Separate
   bind groups are cleaner than range tricks inside the opaque cull.
3. **Different granularity.** Water tiles are 32×32 m, terrain chunks are 2 m (§B.2).
4. **Room for a 12-byte record.** Interpolation (§C) wants two heights per quad plus flow
   and depth, which does not fit comfortably in 8 bytes. Suggested layout:
   - w0: `x:6 y:6 z:6 face:3 prev_dy:6(signed) spare:5`
   - w1: `w:6 h:6 flow_dir:4 flow_speed:3 depth:8 kind:5`
   - w2: tile-local chunk offset, or a pad

   Tiles span 16 chunks, so x/z need 9 bits at voxel resolution, or 5 bits at block
   resolution for top faces. A WebGL2 fallback builds the same quads as a `Mesh`.

**Reuse:** the arena allocator, `records.rs`'s corner/index tables (`QUAD_INDEX_PATTERN`,
`CORNER_PERM`), the queue/prepare pattern and the upload budgets. The shader is a new
`water_pull.wgsl` that shares the decode helpers.

**WebGL2:** there are no storage buffers in the vertex stage, so water uses a per-tile `Mesh`
plus an `ExtendedMaterial` with `AlphaMode::Blend`, the same fallback terrain uses.
- Keep tile granularity: ~100 transparent entities are fine for Bevy's sorter.
- No compute, so any GPU-side water sim or GPU culling is native-only.
- Use absorption option (a).

---

## C. Update rate decoupling

### C.1 How low sim rates are hidden

- **Minecraft:** 4 Hz with no interpolation at all. Flowing texture and corner-averaged
  slopes make it read as motion anyway.
- **Luanti:** 1 Hz. It looks steppy and players accept it.
- **Timberborn:** ~5 Hz, with continuous rendering and momentum shown in the texture.

The tools, cheapest first:
1. **Render-rate shader motion.** Flow-map scroll, normal waves and foam run at frame rate
   regardless of sim rate. This is the bulk of perceived smoothness.
2. **Height interpolation.**
   - Each water quad carries `prev_level` and `level`. The vertex shader lerps y by a
     per-tile uniform `α = clamp((now − step_time) / step_period, 0, 1)`.
   - The mesher emits the union of the previous and current wet cells, so a cell appearing
     grows from its floor and a cell vanishing sinks into it.
   - Greedy merging keys on `(prev, level, flow)`.
3. **The scheduling lead gives interpolation for free.** Steps arrive `LEAD` ticks before
   they apply (§A.4), so the renderer always has both endpoints and interpolates *into* the
   next state instead of adding one step of display latency. That is not possible with
   option (2).
4. **Front effects.** A spawn-once foam/splash particle at cells whose level rose from 0
   masks the 1-cell-per-step front advance.

### C.2 Step rate choice and remesh budget

- **Step rate.** Start at **`WATER_STEP_TICKS = 4` (5 Hz)**, a named constant. A front moves
  up to 5 m/s, which is faster than Minecraft's 4 blocks/s. Use 10 ticks (2 Hz) for
  low-priority regions, such as ones with no subscriber within 2 regions, or under budget
  pressure.
- **Remesh per active drain** (lake scenario, ~3,000 changed cells per step):
  - Remesh granularity is the water tile. The lake plus outflow touches ~2–6 tiles per step,
    so **10–30 tile remeshes/s at 5 Hz**.
  - A tile remesh is a greedy pass over ≤1,024 columns: ~20–60 µs native, ~2–3× on wasm.
    That comes to **≤2 ms CPU per second**, less than 0.05 ms per frame, which fits inside
    the existing budgeted remesh queue.
  - Upload is ~100–2,000 quads per tile at 12 bytes, so ≤24 KB per tile and ≤0.7 MB/s during
    a drain.
  - Dam-break worst case: ~15–25 tiles per step is 75–125 remeshes/s, ~5 ms CPU/s. Still
    fine. Cap it with the queue's per-frame budget and let lagging tiles simply interpolate
    over a longer α.
- **Server-side LOD:** water never invalidates terrain LOD keys (§A.3). The LOD water raster
  refreshes on settle, or on a ≥5 s cadence.

---

## Recommendation

| | Choice | Main costs |
|---|---|---|
| **A. Sync** | **Option 3, the hybrid.** Water is rollback state and is hashed. Settled water is static and free. Flow is solved **server-side only**, every 4 ticks, and enters the log as idempotent absolute-level `WaterStep` events. Each event is stamped `apply at T + WATER_APPLY_LEAD_TICKS` (6) and sent early; clients pre-insert it and match it by identity, so there are no water-caused rollbacks. Cross-region flux is a server-only strip exchange. Include body-level records and a quick-settle-and-snapshot valve. | Bandwidth 0–15 KB/s typical and ~50–75 KB/s during a large flood, near-window subscribers only. The first reaction to a player's own edit is +300–500 ms. Needed: the `WaterStep` event, the client scheduled-event queue, the region solver hook and the manager strip relay. |
| **B. Render** | Separate water records (12 bytes) in their own per-tile (32×32 m) buffers and pipeline, drawn after opaque as a single layer: blend, depth write, no sort, no OIT. Absorption from baked floor depth (WebGL2-safe); native depth copy later. Normal-map waves plus two-phase flow maps. LOD gets a per-key water-surface raster meshed by the same pipeline; the sea greedy-merges to a few quads per key. WebGL2 uses per-tile `Mesh` + `ExtendedMaterial`. | GPU cost is one extra blended layer over water pixels. About 100 draw items near, plus LOD keys. Mesher, shader and LOD-builder work, and `LodPayload` gains a water field (a wire format bump). |
| **C. Rate** | 5 Hz steps (2 Hz when far or over budget), shader motion at frame rate, per-quad `prev→level` vertex lerp driven by early-arriving steps, and splash particles at fronts. | ≤2 ms CPU/s of remeshing per active drain, ≤0.7 MB/s uploads. |

### How this departs from the July plan

The July plan chose option (1), where the solver runs in the predicted sim. This report
changes one thing: **the solver moves out of the predicted sim, while the water state stays
in it.** Reasons:
- The near window is now 25 predicted regions per client.
- Rollback replay is already the client's bottleneck.
- A server-only solver frees the model (floats, momentum, threads) from bit-exactness.
- It turns Phase 3 cross-region flux from a client-determinism problem into server plumbing.

Unchanged from the July plan: the representation, the `Air | Solid | Liquid` material classes
(Phase 0), controller-based swimming, the edit/displacement rules, and hashing.
