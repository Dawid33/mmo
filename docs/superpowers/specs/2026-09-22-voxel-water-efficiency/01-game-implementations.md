# Voxel water efficiency — 01: how shipped games do liquids

Date: 2026-09-22. Scope: EFFICIENCY first (CPU per tick, bytes per cell, cost of settled
water, floods/drains, lag pathologies), believability (finite, volume-conserving) second.
Our constraints, for reading each "steal" line: 1 m blocks × 16 sub-voxels per edge, 20 Hz
ticks, 64 m regions on one thread each, deterministic rollback (client re-sims up to ~20 ticks
on correction, so per-tick liquid cost is paid up to ~20× on a replay), native + wasm32.

How sure each claim is:
- **[src]**: read in source code (vanilla-decompiled, open-source, or leaked-and-published).
- **[doc]**: official docs, patch notes, talk slides, or a wiki page that reads the code.
- **[unverified]**: from memory or a secondary source; check it before relying on it.

---

## 1. Minecraft (Java and Bedrock), the baseline to beat

**Data representation.** Java has no separate fluid storage. A `FluidState` is *derived from*
the `BlockState`: `Block.getFluidState(state)` returns a cached `FluidState`, with a `LEVEL` of
1–8 and a `FALLING` flag. A "waterlogged" block is a block-state property, not a second layer.
So fluid costs zero extra bytes per cell: it lives in the chunk section's paletted block-state
container **[src: FlowingFluid javadoc — `LEVEL`, `FALLING`, `getFlow`, `getSpread`,
`getSlopeDistance`, `SpreadContext`, `OCCLUSION_CACHE`](https://aldak.netlify.app/javadoc/1.21.8-21.8.x/net/minecraft/world/level/material/flowingfluid)**.
Bedrock instead stores water in a **second block layer** (primary + optional "extra" block per
position), which is how any block can be waterlogged there
**[doc: [Bedrock Wiki, block co-location](https://wiki.bedrock.dev/blocks/block-colocation)]**.
Water in vanilla is 8 discrete levels, not a conserved volume.

**Update rule and cadence.** The rule is event-driven. A changed fluid block schedules a
*fluid tick* for itself. Water fires `getTickDelay` = 5 game ticks later (4 blocks/s), lava 30
in the Overworld **[doc: [Minecraft Wiki, Water](https://minecraft.wiki/w/Water)]**. On its
tick, a fluid block:
1. recomputes its own state from its neighbours (`getNewLiquid`: highest horizontal neighbour
   level − `dropOff`, plus the rule that 2+ adjacent sources over solid ground make a new
   source);
2. flows down if it can;
3. otherwise calls `getSpread`, which runs `getSlopeDistance`, a depth-limited DFS for "a way
   down", in each horizontal direction.

The search depth is `getSlopeFindDistance`: 4 for water, 2 for Overworld lava. It walks up to
4 blocks sideways and keeps the direction(s) with the shortest path to a drop. "For each
direction this weight is initially set to 1000…" **[doc: Minecraft Wiki, Water]**. The DFS
uses per-call caches (`Short2ObjectMap<Pair<BlockState,FluidState>>`, `Short2BooleanMap`, and
since 1.21.2 a `SpreadContext`) so it doesn't re-read the world
**[src: [Forge 1.19.3 FlowingFluid javadoc](https://nekoyue.github.io/ForgeJavaDocs-NG/javadoc/1.19.3/net/minecraft/world/level/material/FlowingFluid.html)]**.
Worst case is a few hundred block reads per fluid tick on flat open ground. Water ≈ 4 steps of
branching factor 3 per direction [unverified exact count].

**Scheduling limits.** Java executes at most **65,536** scheduled ticks per game tick, block
ticks first, then fluid ticks in FIFO order. Bedrock caps scheduled ticks at **100 per chunk
per game tick**, so big Bedrock floods run slowly instead of lagging
**[doc: [Minecraft Wiki, Tick](https://minecraft.wiki/w/Tick)]**.

**Active set / sleep.** The only active set is the scheduled-tick queue. A still pool has no
pending ticks and costs nothing (water isn't randomly ticked). Settled water is free because
nothing is queued, *not* because anything proved it has settled. Any neighbour change wakes
it: `setBlock` fires `neighborChanged`/`updateShape` on 6 neighbours, and each fluid neighbour
reschedules.

**Side costs per fluid change [src/unverified].** Every level change is a full `setBlock`.
That means:
- a block-state change in the palette;
- neighbour updates (6 reads + possible reschedules);
- heightmap updates (MOTION_BLOCKING counts fluids);
- a light-engine check (water attenuates skylight, so air → water changes light; see the long
  standing [MC-71078 water/lighting frame drops](https://bugs.mojang.com/browse/MC-71078));
- a block-update packet to every client tracking the chunk;
- a client re-mesh of the section.

The spread search is not the dominant cost. The dominant cost is the **per-change side
effects multiplied by the number of changed blocks**.

**Known pathologies.**
- *Draining / receding.* Removing a source makes every downstream flowing block recompute. The
  recession walks outward at 1 block/5 ticks, and every step is a set of `setBlock`s with the
  side costs above. Opening a cave into an ocean floods it the same way.
- *Worldgen-exposed fluids.* Oceans next to freshly generated caves or ravines produce a burst
  of hundreds of fluid ticks at load time. The Flowing Fluids mod ships a post-generation
  "plug all exposed fluids" pass because of exactly this
  **[doc: [Flowing Fluids README](https://github.com/Traben-0/flowing_fluids/blob/master/README.MD)]**.
- *Sponges.* One sponge removes up to 65 water blocks in one tick, so one tick carries 65
  setBlocks, their light updates, and the refill cascade from the surrounding flowing water
  [doc: Minecraft Wiki, Sponge; the lag attribution is from community reports, unverified].
- *Lithium's fixes* show where the time goes: a "fluid flow optimization" mixin, `FluidState`
  stores "whether it is empty" directly, read the fluid from an already-known `BlockState`
  instead of the world, and a packed-integer scheduled-tick collection
  **[doc: [Lithium mixin config](https://github.com/CaffeineMC/lithium/blob/develop/lithium-neoforge-mixin-config.md)]**.
  That is, state lookups and tick bookkeeping, not arithmetic.

**Multiplayer sync.** The server is authoritative and sends ordinary block-change packets. The
client doesn't simulate fluid.

**Steal.** Fluid derived from, or stored beside, the block palette costs zero bytes for
still water. An event-driven queue makes still water free. A per-chunk tick budget (Bedrock's
100) turns "lag" into "slow water".
**Avoid.** The unbounded per-change side effects (light, heightmap, neighbour fan-out, packets)
and infinite-source duplication.

### 1a. Minecraft mods that add finite water

**Flowing Fluids (Traben, 1.20–1.21, the modern reference)**
**[src: [github.com/Traben-0/flowing_fluids](https://github.com/Traben-0/flowing_fluids),
`MixinFlowingFluid.java`, `FFConfig.java`, `config/auto_perf/`]**:
- *Representation.* The vanilla 8 levels are reinterpreted as a conserved integer amount. A
  full level-8 block is saved as a vanilla source, so worlds stay loadable without the mod.
- *Tick rule* (replaces `FlowingFluid.tick`):
  1. move as much as fits into the cell below (`min(8 − below, amount)`);
  2. a cheap **pressure shortcut**: a full cell under fluid pushes the fluid above it through
     the connected body, searching up to 40 cells, instead of simulating pressure;
  3. if more than `dropOff` (1) remains, **pairwise-average with the lowest neighbour** found by
     the vanilla 4-block slope search: `from = avg`, `to = avg + remainder`, which is
     integer-exact and conserving;
  4. if only the drop-off amount remains, it only moves toward a nearby ledge. A 1-level film
     on flat ground *stops*, a "surface tension" rule that is also the settling rule.
- *Leveling is moved off the hot path.* Differences of 1 level inside a pool are evened out
  lazily by **random ticks**: a random-direction walk of up to `randomTickLevelingDistance` =
  32 cells that moves one level to a cell at least 2 lower. It is skipped entirely in any chunk
  with more than 16 pending fluid ticks. Pools settle eventually at near-zero steady cost.
- *Distance gating.* `dontTickAtLocation`, driven by `playerBlockDistanceForFlowing`,
  reschedules far-away fluid ticks 200–400 ticks (10–20 s) later instead of computing them.
- *Auto-performance.* It measures MSPT every `autoPerformanceUpdateRateSeconds` = 10 against a
  target and steps through 4 presets. The presets lengthen `waterTickDelay` (default 2; the
  README suggests 4–8 for performance), shorten flow distance, randomise tick delays
  (`tickDelaySpread`) and shrink the leveling distance.
- *World-gen plug* (`encloseAllFluidOnWorldGen = true`): seals exposed fluids after generation
  so loading a chunk causes no flood.
- *Infinite biomes.* Ocean/river/swamp cells at sea level with skylight refill at a limited
  rate, so the sea stays a reservoir without infinite duplication.
- *Known pathology.* Server MSPT rises with rain refill and evaporation across all loaded
  chunks. Users asked for a mode that only simulates player-placed water, and it is unanswered
  **[doc: [issue #192](https://github.com/Traben-0/flowing_fluids/issues/192)]**.

**PhysEx / Water Physics Overhaul (1.12 era)**
**[doc: [CurseForge page](https://www.curseforge.com/minecraft/mc-mods/water-physics-overhaul)]**:
1 block = 8 "fluid packets". Packets seek lower ground within a configurable distance, bodies
equalize within a configurable distance, and full blocks *displace* packets instead of deleting
them. Notably it "fixes FluidState accesses … to use FluidState in Chunks, not from
BlockState", i.e. it moved fluid to its own per-chunk storage.

**Physics Mod (Haubna)** does wave/ocean effects that are visual and client-side. It is not a
finite-volume solver **[unverified]**, so it isn't relevant to efficiency.

**Steal (Flowing Fluids).** Integer levels with pairwise-average moves conserve volume exactly
and deterministically. A minimum-film "surface tension" rule makes sheets stop spreading.
Pool levelling can be deferred to cheap background passes. Background water can run on a
lower cadence. Flood-prone worldgen gets sealed.

---

## 2. Luanti (Minetest)

**Representation.** Liquids are nodes. The level (0–7) lives in the low bits of the node's
`param2` byte, with a "flowing down" flag bit, so there is **no extra storage**
[unverified bit layout, standard Luanti knowledge].

**Update rule.** `ServerMap::transformLiquids` drains `m_transforming_liquid`, a
`UniqueQueue<v3s16>` (deduplicated positions)
**[src: [servermap.cpp](https://raw.githubusercontent.com/luanti-org/luanti/master/src/servermap.cpp)]**:
- Each node classifies its 6 neighbours as source, flowing, air or other. It takes the max
  neighbour level (−1 sideways, boosted from above) and limits the change by viscosity.
- 2+ sources adjacent to a `liquid_renewable` liquid make a new source.
- Changed nodes collect into `modified_blocks` and get one batched
  `voxalgo::update_lighting_nodes` at the end. Neighbours re-enter the queue.
- Mapgen also pushes liquid nodes that border air into the queue [unverified, from memory of
  `Mapgen::updateLiquid`], and that is the source of post-generation liquid storms.

**Cadence and budget [doc: [settingtypes.txt](https://raw.githubusercontent.com/luanti-org/luanti/master/builtin/settingtypes.txt)]:**
- `liquid_update` = **1.0 s** between steps, so water flows ~1 node/s even at viscosity 0
  **[doc: [issue #5210](https://github.com/minetest/minetest/issues/5210)]**.
- `liquid_loop_max` = **100000** nodes per step.
- `liquid_queue_purge_time` = 0 (off). When set, if the queue stays above what one step can
  process for that many seconds, the engine **dumps the oldest entries** down to
  `liquid_loop_max`. That is lossy: water freezes wherever it was.

**Sleep.** Same as Minecraft: nothing queued, nothing costs.

**Pathologies.** A single source can "flood half the world", and a ceiling source makes a
5-line cross **[doc: [issue #5387](https://github.com/minetest/minetest/issues/5387)]**.
Big queues after cave generation are why the purge setting exists.

**Finite liquid.** Core used to have a `liquid_finite` option; it is no longer in
settingtypes.txt [unverified history]. The `dynamic_liquid` mod does finite water by
**swapping whole source nodes**. An ABM that runs only for sources next to flowing nodes drops
a source into the space below it, otherwise swaps it with a random adjacent flowing or air
node **[doc: [dynamic_liquid](https://github.com/minetest-mods/dynamic_liquid)]**. That is very
cheap per step and conserving (whole blocks), but the surface is noisy. Displacing a node in a
surface-level ocean can make movement "wander infinitely", killing performance
**[doc: [dynamic_liquid #3](https://github.com/minetest-mods/dynamic_liquid/issues/3)]**. The
mod's answer is to treat the sea as a reservoir via "springs".

**Steal.** A dedup queue. A per-step node budget. **Batched lighting and remesh once per step**
instead of per change. The lesson from purge: dropping queue entries is not acceptable for a
conserving sim; slow down instead.

---

## 3. Terraria (2D) and Starbound (2D)

### Terraria
**[src: decompiled 1.2-era `Liquid.cs`, [TheVamp/Terraria-Source-Code](https://github.com/TheVamp/Terraria-Source-Code/blob/master/Terraria/Liquid.cs)]**

**Data.**
- Each tile has a `byte liquid` (0–255) plus liquid-type bits. There's also a per-tile
  `checkingLiquid` flag, so a tile is queued at most once, and a `skipLiquid` flag.
- The active set is `Main.liquid[]`, a fixed array of `maxLiquid` = 5000 records
  `{x, y, kill, delay}`. Overflow spills into a second fixed array, `LiquidBuffer`.
- **Fixed memory**, with no allocation during simulation.

**Update.** `UpdateLiquid` splits the active list into `cycles` slices (10 in single-player;
the tool summary said 30 on servers, unverified) and processes about one slice per frame. Each
tile:
- falls into the tile below (`min(255 − below, self)`);
- then averages with its left/right neighbours, including a 3-tile average when both sides are
  open;
- re-queues the tiles it changed and the one above.

Lava waits 5 updates between moves, honey 10.

**Sleep.** `kill` counts consecutive updates with no change. At `kill > 4`, `DelWater` removes
the tile from the active list. So "settled" means "unchanged 5 times", a cheap
**no-change counter**.

**Not conserving.** `DelWater` zeroes tiles holding under 2/255, and zeroes films under 20/255
beside a lower neighbour. Averaging uses `Math.Round(x/2 + 0.001)`. The deep underworld
evaporates water. Volume is traded for guaranteed settling, on purpose.

**Overload handling.**
- `quickFall` turns on above 2000 active liquids and skips the delays.
- A `stuck` detector trips when the count hovers within ±50 for 10000 updates.
- **Panic mode**: more than 4000 active (liquid + buffer) for 1800 frames, or more than 13500
  at once, calls `StartPanic`, which clears the whole active set. `QuickWater` then settles the
  world analytically one row per frame, bottom-up.

The same `QuickWater` "Settling liquids" pass runs on every world load, and players can force
it with `/settle` on TShock **[doc: [Terraria Wiki, Liquids](https://terraria.wiki.gg/wiki/Liquids);
[WinterNode lag guide](https://winternode.com/blog/terraria/terraria-server-lag-fixes)]**.

**Sync.** The server is authoritative. Liquid changes are sent per tile
(`NetMessage.sendWater`, tile squares for reactions). Hosts call liquids "one of the most
performance impacting parts … both … client-side performance and multiplayer syncing"
**[doc: WinterNode]**.

**Steal.**
- A **bounded active list with an overflow tier**.
- A **no-change counter that retires cells**.
- A **"fast-forward to equilibrium" solver** (QuickWater) used when the live sim can't keep
  up and on load. It is the single most important trick for big floods and drained lakes.

### Starbound
**[src: OpenStarbound (derived from the Starbound 1.4.4 source), `source/base/StarCellularLiquid.hpp`,
`StarWorldServer.cpp`, `StarLiquidTypes.hpp` — [github.com/OpenStarbound/OpenStarbound](https://github.com/OpenStarbound/OpenStarbound)]**

**Data.**
- Each tile stores `{LiquidId (u8), float level, float pressure, bool source}`. Endless source
  cells (oceans) hold pressure.
- The active set is per-liquid ordered hash sets of positions.
- Working cells are built fresh each step in a block-allocated hash map, with cached neighbour
  pointers.

**Update.** Each step runs these phases over the active cells, sorted bottom-up:
`applyPressure` (pressure = the pressure above + the level above), `spreadPressure`,
`limitPressure`, `pressureMove`, `spreadOverfill` (up/lateral/down factors), `levelMove`,
`findInteractions`. Each liquid has its own `tickDelta` (update every N steps). Ties are broken
with a random left/right order.

**Sleep.** A cell goes back into the next active set **only if its level changed by more than
`minimumLivenLevelChange`**, or its pressure by more than `minimumLivenPressureChange`.
Sub-threshold changes still apply but don't keep the cell awake. That is a **hysteresis
threshold**, and it is how still water goes quiet despite float noise.

**Budget.** `setProcessingLimit(liquidEngineBackgroundProcessingLimit)` caps processed cells
per step *outside* `noProcessingLimitRegions`, and the server passes
`clientMonitoringRegions` there. So near players the sim is unlimited, and the background is
budgeted.

**Sync.** The server is authoritative. Changed tiles go to a per-client
`pendingLiquidUpdates` set, flushed as `TileLiquidUpdatePacket(pos, LiquidNetUpdate)`, where
`LiquidNetUpdate` = `{liquid id, uint8 level}`. The float level is **quantised to a byte for
the wire**, and a packet is sent only when the quantised value changes.

**Steal.**
- A liveness threshold with hysteresis.
- Unlimited simulation near players plus a budget elsewhere.
- Per-liquid cadence.
- **Replicate a quantised level and send only when the quantum changes.**
- Cheap vertical pressure as "pressure = the pressure above + the level above".

---

## 4. Other games

### Oxygen Not Included (Klei)
- **Data.** One element per cell (liquids and gases never share a cell) with mass, temperature
  and a disease payload. Each element has a max mass per cell. Liquid **under 10 g is deleted**
  outside pipes. Overfull liquid pushes up into the cell above, which is the "pressure"
  **[doc: [ONI Wiki, Liquid](https://oxygennotincluded.wiki.gg/wiki/Liquid)]**.
- **Engine.** Heat and fluid flow run in a native C++ `SimDLL` on its own thread, separate from
  the Unity/C# game thread. The game side hooks the `Sim200ms` cadence
  **[doc, weak: [Klei forum thread on SimDLL](https://forums.kleientertainment.com/forums/topic/167854-infrequent-features-of-simdll/),
  Steam multi-threaded sim mod]**. The exact sim step (believed 200 ms = 5 Hz) is
  **[unverified]**.
- A Klei bug report says the sim depends on game speed (not deterministic)
  **[doc: [bug r4257](https://forums.kleientertainment.com/klei-bug-tracker/oni/simulation-is-not-deterministic-but-dependent-on-game-speed-r4257/)]**.
  That's fine single-player, and a warning for us.
- **Steal.** A **minimum-mass cutoff** so films disappear instead of spreading forever.
  **Liquid on a lower cadence than the game tick**. Structure-of-arrays cell data in native
  code.

### Noita (Nolla, falling sand)
**[doc: [GDC 2019 "Exploring the Tech and Design of Noita"](https://www.gdcvault.com/play/1025695/Exploring-the-Tech-and-Design),
notes at [braindump](https://braindump.jethro.dev/posts/gdc_vault_exploring_the_tech_and_design_of_noita/)
and [macuyiko](https://blog.macuyiko.com/post/2020/an-exploration-of-cellular-automata-and-graph-based-game-systems-part-4.html)]**
- One byte-ish material per pixel. A **single-buffered** grid swept bottom-up with an
  "already updated" guard. Liquids use the same falling-sand rule (down, down-diagonal, then
  sideways).
- The world is split into **64×64 chunks, each with a dirty rect**. Only the rect is simulated,
  and it shrinks to empty when nothing moves.
- **Multithreading.** No pixel moves more than 32 px per step, so a chunk's writes stay within
  its neighbours. Updates then go in a checkerboard of chunk groups: 4 passes, with no two
  adjacent chunks in the same pass. The 4-pass detail is from talk notes
  **[unverified exact pass count]**.
- **Not conserving and not deterministic across threads**, which is fine for a single-player
  roguelite.
- **Steal.** **Per-chunk dirty AABBs** as the sleep mechanism, plus a **max-displacement bound
  that makes chunk-parallel updates safe**. For us the second part maps onto regions and
  sub-region chunks.

### Timberborn (Mechanistry)
- **Model.** A **2D shallow-water "virtual pipes" heightfield** adapted from Mei et al., "Fast
  Hydraulic Erosion Simulation and Visualization on GPU". Each tile has a depth and 4 pipe
  flows to its neighbours. Momentum comes from comparing flow to previous ticks. The sim ticks
  "every couple hundred ms" and visuals interpolate between ticks
  **[doc: [Game Developer deep dive](https://www.gamedeveloper.com/design/deep-dive-timberborn-s-water-mechanics)]**.
- **Update 6 (Oct 2024)** replaced the 2.5D sheet with "three-dimensional physics":
  "horizontal layers of water … atop one another, with buildings in between", obeying
  communicating vessels. The patch notes say "optimized water … simulations"
  **[doc: [Update 6 patch notes](https://timberborn.wiki.gg/wiki/Update_6)]**. Most likely a
  per-column stack of pipe layers (Enshrouded-style) **[unverified]**.
- Water time runs at 0.5× game time **[doc, secondary: search summary of the Timberborn wiki]**.
  A later update added a **user-facing "fluid simulation frequency" setting**
  **[doc, secondary: search result]**.
- Unity Jobs/Burst threading is **[unverified]**: no primary source found.
- **Pathology.** The water sim is the main CPU cost, and flooding caused *pathfinding* lag
  spikes that were fixed by letting beavers swim instead of re-routing
  **[doc: Game Developer]**. Second-order systems that react to water can cost more than the
  water itself.
- **Steal.** A pipe-model heightfield per column layer: O(cells), branch-light, SIMD-friendly,
  conserving by construction, with a tunable cadence and render interpolation.

### From Dust (Ubisoft Montpellier, 2011)
- The "Galileo" simulator handles water, lava, erosion, sedimentation and vegetation in real
  time. It is "natively implemented in SIMD 128 bits and fully multithreaded (entirely with
  SPU on PS3)"
  **[doc: [GDC Vault, Bel & Chahi](https://www.gdcvault.com/play/1013667/Creating-a-High-Performance-Simulation)]**.
- It is a layered heightfield (rock/sand/water per column), a 2.5D sim like Hydrophobia and
  7 Days to Die, as the Enshrouded slides describe it. Grid size and rate are not in the
  abstract **[unverified]**.
- **Steal.** Heightfield water vectorises well. A whole world of flowing water fit on PS3 SPUs.

### Enshrouded (Keen Games, "Wake of the Water", Nov 2025): the closest analogue to us
**[doc: slides, GPC 2025 "Water Simulation & Rendering in Enshrouded" — [archive](https://graphicsprogrammingconference.com/archive/2025/),
[PDF](https://static.graphicsprogrammingconference.com/public/2025/talks/water-simulation-rendering-in-enshrouded/Mantler-Koenen-water-simulation-rendering-in-enshrouded.pdf)]**
- **Requirements, from the slides.** Dynamic 3D voxel world, soft real time, **fixed memory
  budget**, **authoritative server with replication**, "believable and predictable
  movement". The slides also note that academic methods are "too fine-grained … and not
  client-server ready", and that Minecraft's approach is "voxels expand by limited distance".
- **Simulation data = columns, not voxels.** A column has a bottom and a height, plus **stacked
  fluid sections and displacement sections** (terrain/buildings/blockers): "Stackable!".
  Columns are grouped into **chunks of 16×16 columns**, and the sim API offers **5 LOD levels**.
  So a 3D world is simulated as sparse vertical runs, not per voxel.
- **Resolution.** 0.5 m voxels in a 10k × 10k × 4k world, fill **quantised to 16 states per
  voxel** on the client side. Clients receive state "irregularly (whenever new network packets
  arrive)", and the renderer interpolates SDF bricks to hide the 16-level pops. Clients don't
  predict water; they render replicated state.
- **Rendering cost.** A sparse brick tree: 6³ SDF bricks, node pool 20 MB, 64 MB brick atlas.
  Brick-tree updates take ~0.2 ms GPU and the water surface ~0.2 ms at 1440p on an RTX 4060 Ti.
- **Gameplay limits that are also efficiency limits.** Only water placed, removed or
  redirected *inside bases* is saved **[doc: [patch notes](https://enshrouded.com/en-US/news/enshrouded-wake-of-the-water-update)]**.
  Springs fill "up to the level of the spring", sand soaks water up, drains delete water, and
  water "won't push … objects around"
  **[doc: [PCGamesN interview](https://www.pcgamesn.com/enshrouded/wake-of-the-water-interview)]**.
  Performance "especially around water simulation" is still a stated priority.
- **Steal.** **Column/run-length water** (fluid spans per column, next to solid spans) is the
  representation that makes 3D voxel water tractable. The server simulates and clients
  interpolate quantised states. Persistence is scoped, so wild water can reset to its
  generated state.

### Hytale (Jan 2026)
**[doc: [unofficial server-modding docs, Fluid System](https://hytale-docs.pages.dev/modding/content/fluid/)]**
- A `FluidSection` component per chunk section: fluid id through a **palette**
  (`ISectionPalette`), levels in a **packed nibble array** (16384 entries, two 4-bit levels per
  byte, 8 KiB per 32³ section). Invariant: `level==0 ⇔ id==0`. Access is guarded by a
  `StampedLock`, and changed positions go into `changedPositions`.
- `MaxFluidLevel` = 8. Each fluid's `FluidTicker` has a `FlowRate` in seconds, converted to
  ticks by TPS, plus `CanDemote` and `SupportedBy`.
- **Fluid ticking piggybacks on the section's block-ticking bitset.** A section whose
  ticking-block count is 0 exits immediately. That is a per-section *early-out* plus a
  per-block bitset.
- **Replication tiers.** 1 change → a single packet. 2–1023 → a batched `SetFluids` with
  indices. ≥ 1024 → a cached full-section packet. Lighting is invalidated per non-empty change
  set, not per cell.
- Both infinite and "Water - Finite" fluid types exist
  **[doc: [item page](https://hytale.isaacguru.com/wiki/hytale/item/Fluid_Water_Test)]**.
- **Steal.** Fluid kept *apart from* blocks (palette + nibble), a per-section active bitset
  with an O(1) "nothing to do" check, and tiered delta vs full-section replication.

### Vintage Story
**[src: [vssurvivalmod `BehaviorFiniteSpreadingLiquid.cs`](https://github.com/anegostudios/vssurvivalmod/blob/master/BlockBehavior/BehaviorFiniteSpreadingLiquid.cs)]**
- Liquids live in a **separate fluid block layer** (`BlockLayersAccess.Fluid`) next to the
  solid layer, which enables waterlogging-like cases. There are 7 levels (`MAXLEVEL = 7`).
  Partial walls matter: `GetLiquidBarrierHeightOnSide` compares the barrier height with
  level/7.
- **Event-driven with dedup.** `OnNeighbourBlockChange` calls
  `world.RegisterCallbackUnique(OnDelayedWaterUpdateCheck, pos, spreadDelay)`, one pending
  callback per position, default `spreadDelay` 150 ms.
- A BFS `FindDownwardPaths` looks for drops, like Minecraft's slope search. A separate
  bounded BFS (`maxDist = 7`) handles recession. The vanilla model is not conserving.
- Mods add finite water. `FiniteWater` converts small ponds to finite water and has reports of
  severe multiplayer lag. `Realistic Water` claims "basically no FPS impact … server load
  impact only when something is happening"
  **[doc: [VS ModDB FiniteWater](https://mods.vintagestory.at/finitewater),
  [Realistic Water](https://mods.vintagestory.at/realisticwater)]**.
- **Steal.** A separate fluid layer and deduplicated delayed callbacks. This is basically the
  Minecraft model with a cleaner data layout.

### Dwarf Fortress
**[doc: [DF Wiki, Pressure](https://dwarffortresswiki.org/index.php/DF2014:Pressure)]**
- Each tile holds depth 1–7. Three movement rules:
  - falling (into a tile below with less than 7/7);
  - spreading (averaging with an adjacent tile);
  - **pressure**: water falling onto water, river sources and pumps *teleport* a unit through
    full tiles to the nearest open tile. It tries sideways and down first, up only if those
    fail, orthogonal only.
- Pressure stops when the far end is **one level lower than the source "for reasons of CPU
  time-saving"**. An approximate equilibrium is accepted to cap the work.
- Stable liquid is flagged inactive and skipped until disturbed [unverified: DF's internal
  "flow" flags aren't documented in the fetched page]. Big water works are a known FPS drain
  [community lore, unverified].
- **Steal.** **Pressure as a path search that teleports a unit** instead of propagating a field
  (Flowing Fluids does the same with its 40-cell search). Accept stopping at ±1 level.

### Veloren
Water is ordinary `BlockKind::Water` blocks **[doc: [Veloren BlockKind](https://docs.veloren.net/veloren_common/terrain/block/enum.BlockKind.html)]**.
Rivers and lakes come from worldgen's erosion/hydrology and are **static**: there is no
runtime flow sim **[unverified, but consistent with the docs]**. The cost is zero. The sky-high
realism is in worldgen, which matches our forge direction. **Steal:** worldgen water can
stay static until disturbed.

### Teardown, Valheim, Space Engineers, Medieval Engineers, Dual Universe, Planet Zoo/Coaster
- Teardown and Valheim use static planar water surfaces, per the Enshrouded slides' survey.
- Space Engineers has no vanilla water. The popular Water Mod (Jakaria) is a planet-radius
  water *level* with buoyancy, not a flow sim **[unverified]**. Keen SWH said VRAGE 3 would
  bring "volumetric water on a planetary scale" **[doc, secondary: search summary]**.
- Medieval Engineers ships without water.
- Dual Universe uses static oceans **[unverified]**.
- Planet Zoo/Coaster paint water as height planes with no flow **[unverified]**.
- Lesson: most shipped "open voxel" games **don't simulate world water at all**. That's
  evidence that static-until-disturbed is the norm, not a cop-out.

### Cities: Skylines 1 and 2
- CS1 simulates water on the CPU over a terrain-resolution heightfield grid. Its sources and
  sinks flood, and the community reports odd flows in narrow shallow canals
  **[doc, weak: [Steam discussion](https://steamcommunity.com/app/255710/discussions/0/3423312514212403136/);
  grid details unverified]**.
- CS2 moved water to **GPU compute shaders**. Water, snow, particles and skinning together
  cost ~1.5 ms GPU **[doc: [paavo's CS2 perf analysis](https://blog.paavo.me/cities-skylines-2-performance/)]**,
  and Paradox listed water-sim performance as ongoing work.
- A GPU sim is **not usable for our deterministic, server-authoritative CPU sim** (float
  non-determinism across vendors, readbacks, wasm/WebGL2).
- **Steal.** Only the evidence that heightfield shallow water is cheap enough for city scale.

### Cortex Command / Sandspiel
Both are falling-sand-style particle or cell sims. Sandspiel is a WASM cellular automaton
(Rust) on a small grid [unverified]. Neither adds a technique beyond Noita.

---

## 5. Cross-cutting observations

1. **"Settled costs nothing" means an empty queue or active set, in every shipped game.**
   Nothing scans still water. The variants are:
   - a scheduled-tick queue (MC, Luanti, VS);
   - an active list with a no-change retire counter (Terraria);
   - an active set with a liveness threshold (Starbound);
   - per-chunk dirty rects (Noita);
   - a per-section ticking bitset with a count early-out (Hytale).
2. **The expensive part is the side effects of each change, not the flow arithmetic**:
   lighting, heightmaps, neighbour fan-out, remesh, per-cell packets. The engines that scale
   batch them per step or section: Luanti's single `update_lighting_nodes`, Hytale's per
   change-set light invalidation and tiered replication, Starbound's per-client pending set.
3. **Every engine has an overload valve.** The honest ones slow down: Bedrock's 100 per chunk,
   Luanti's loop max, Starbound's background limit, Flowing Fluids' MSPT-driven presets. The
   lossy ones delete: Luanti's purge, Terraria's panic → QuickWater. For a conserving sim,
   slowing down, or *analytically settling* a body, is the only acceptable valve.
4. **Finite, conserving water that ships at scale uses integer or quantised amounts with
   pairwise transfers**: Flowing Fluids' 8 ints, DF's 7, Terraria's 255 (which cheats with
   deletion), Enshrouded's 16 quantised states on the wire. Floats appear only where
   determinism doesn't matter (Starbound, ONI).
5. **Nobody predicts water on the client.** Terraria, Starbound, Minecraft, Hytale and
   Enshrouded all make it server-authoritative, replicate quantised state and interpolate for
   display. For us this is the biggest lever. If liquid state is outside the rollback-predicted
   sim, or is predicted only as "unchanged", the ~20× replay multiplier disappears.
6. **Big floods and drains are never simulated cell by cell for long.** Terraria settles
   analytically. Flowing Fluids and DF approximate pressure with a bounded path search. Enshrouded
   and Timberborn store water as column spans/heightfields, so a lake is O(columns), not
   O(voxels). Worldgen-exposed water is sealed or left static (Flowing Fluids plug, Veloren,
   Enshrouded's non-base water resets).

---

## 6. Ranked: techniques that beat Minecraft on efficiency

Ranked by expected win for *our* constraints (rollback replay ×20, 64 m regions per thread,
wasm).

1. **Keep liquid out of the rollback-predicted state. The server simulates it and replicates
   quantised levels; the client interpolates.** This removes the replay multiplier entirely,
   the largest single factor for us. *Proven by:* Enshrouded (authoritative server, 16 fill
   states, client interpolation of irregular updates), Starbound (u8 level per changed tile,
   per-client pending set), Terraria, Hytale. Minecraft does this too, but its per-block
   packets are unbatched.
2. **Column/span representation (fluid runs stacked per column) instead of per-voxel levels.**
   A lake or ocean is a few spans per column. Horizontal flow is heightfield-like, O(columns),
   and vertical transfer is span arithmetic. It also handles 16 sub-voxels per block without
   16³ storage. *Proven by:* Enshrouded (stacked fluid and displacement sections, 16×16-column
   chunks, 5 LODs), Timberborn Update 6 (stacked layers), From Dust (layered heightfield,
   SIMD). Minecraft stores and ticks per block.
3. **Active set with a liveness threshold/retire counter + per-section early-out.** Still water
   is never visited, and float or rounding noise can't keep a lake awake. *Proven by:*
   Starbound (`minimumLivenLevelChange`), Terraria (`kill > 4` retire), Hytale (ticking count
   == 0 → skip section), Noita (per-chunk dirty rect). Minecraft has the queue but no
   hysteresis, and any neighbour poke re-arms it.
4. **Bounded per-region budget whose overflow means "slow down", plus a background budget away
   from players.** Lag turns into slower water and conservation survives. *Proven by:* Bedrock
   (100 ticks per chunk per tick), Starbound (`liquidEngineBackgroundProcessingLimit` outside
   client regions), Flowing Fluids (distance gating 10–20 s, MSPT auto-presets), Luanti
   (`liquid_loop_max`). Java's 65,536 global cap only bites after the server already lags.
5. **An analytic "settle" for big bodies.** Fill a basin to its spill level in one pass; don't
   simulate the flood cell by cell. Use it for drained lakes, breached dams and chunk load.
   *Proven by:* Terraria `QuickWater` (on load, in panic, `/settle`), plus DF's and Flowing
   Fluids' bounded pressure path search (teleport a unit to the nearest open cell at the same
   level). Our forge already computes spill-level lakes with Priority-Flood, the same machinery.
6. **Batch the side effects of liquid changes per step and section**: one light pass, one remesh
   mark, one heightmap fix, and one replication message per section (delta indices, or a full
   section above a threshold). *Proven by:* Luanti (batched `update_lighting_nodes` over
   `modified_blocks`), Hytale (1 / ≤1023 indexed / ≥1024 full-section tiers, per-set light
   invalidation). Minecraft pays full `setBlock` side effects per block per change.
7. **Integer amounts with pairwise-average transfers and a minimum-film rule.** Exactly
   conserving, deterministic on native and wasm, cheap. The film rule makes sheets stop
   instead of creeping forever. *Proven by:* Flowing Fluids (8 ints, `avg`/`avg+rem`, stop at
   ≤ dropOff on flat ground), DF (7 units), ONI (10 g deletion, not conserving). Minecraft is
   neither finite nor conserving.
8. **Defer levelling of pools to a cheap background pass.** Only falls and big gradients run
   on the hot path; ±1-level ripples inside a pool even out lazily. *Proven by:* Flowing
   Fluids (random-tick levelling, 32-cell walk, skipped in busy chunks), DF (stops at one
   level below the source).
9. **Lower liquid cadence than the game tick, interpolated in render.** 20 Hz sim with water
   at e.g. 5 Hz is 4× fewer steps, with no visual loss if the renderer lerps. *Proven by:*
   Timberborn (a few hundred ms ticks, 0.5× water time, a user frequency setting), Luanti
   (1 s), ONI (believed 200 ms), Enshrouded (client interpolates irregular updates).
   Minecraft's 5-tick delay is similar, but each tick does full work.
10. **Seal or freeze worldgen water; persist only player-disturbed water.** No post-generation
    flood storms and small saves. *Proven by:* the Flowing Fluids world-gen plug, Enshrouded
    (only in-base water edits are saved), Veloren (static worldgen water), and Luanti as the
    counterexample: the mapgen-queued liquids are why `liquid_queue_purge_time` exists.
11. **Chunk-parallel updates with a max-displacement bound.** Not needed inside one region
    (regions are single-threaded by design), but it tells us how to handle seams: a bound
    on transfer distance per step makes a checkerboard or region-local update safe. *Proven
    by:* Noita (64×64 chunks, ≤ 32 px move, checkerboard passes).

### Unverified claims, check before building on them
- The ONI sim step length (200 ms) and SoA layout.
- Timberborn's threading (Jobs/Burst) and the exact Update 6 layer model.
- From Dust grid size and rate.
- DF's internal "inactive liquid" flags.
- Terraria's server `cycles` value.
- Luanti's `param2` bit layout and the mapgen `updateLiquid` behaviour.
- The Space Engineers water mod model, and the Dual Universe / Planet Zoo static-water
  statements.
- The worst-case read count of Minecraft's slope search.
- The Enshrouded talk's simulation slides are mostly images, so the fine detail of the column
  update rule (pipe model or level spreading) is not in the extracted text.
