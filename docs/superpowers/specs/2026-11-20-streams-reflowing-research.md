# Streams Reflowing (Minecraft mod): how its streams work — research

Date: 2026-11-19 (researched), written down 2026-11-20. Online research only.

**Sources and their limits.** Streams Reflowing is closed source (All Rights Reserved); its GitHub repository
`nice-john/SR-Public` is an issue tracker only (empty repository). Everything here comes from the mod page and the
version changelogs (about 70 KB, read through the Modrinth API). The page does not describe the algorithm; anything
past what the changelogs state is marked **(inference)**.

It is NOT delvr's older "Streams" mod (that one needed the Farseek API and filled rivers with custom non-decaying
flowing blocks); the author says only the name is borrowed. Loaders: Fabric, Forge, NeoForge; Minecraft 1.20.1 –
26.3; version 2.17.5 as of 2026-11-15.

## 1. Planning and carving streams at world generation

- **Planned per area.** The world is split into square areas the changelogs call watersheds, stream regions or
  plates: `plateSize`, 512–8192 blocks, default 1024. A world keeps the size it was started with.
- **Heights read first.** Land heights are sampled on a coarse grid (one routing step = 16 blocks at default detail),
  estimated between readings, and read in full where the ground bends ("adaptive grid read"; "terrain fit" setting).
  Heights come through the terrain mod when it can be read (ReTerraForged, FreeTerraForged, …), which is how it works
  on top of any world generator. Cost: a new world sits at 0 % for about a minute; pregenerating with Chunky is
  advised. Quality presets Potato … Max set grid fineness and how much is read.
- **Routing (inference: classic downhill flow routing on that grid; the changelogs talk of basins, spill levels and
  drained area).**
  - A stream starts where enough land drains into it. The drained-area threshold is the SAME at every quality level,
    so a finer grid places the same streams more precisely instead of adding more headwaters.
  - Basins fill as lakes up to their spill level, with a minimum size (`lakeMinArea`). Dry biomes fill lower and leave
    a dryness-scaled share of basins empty.
  - A pit too deep to cross and too small to be a lake becomes a plunge pool; the stream carries on from its rim.
  - Every stream must end in the sea, a river, or a lake at least 4× its width. Short stubs are pruned (default 75
    blocks, measured over the whole branching stream); a stream that bends back onto itself is cut across (an oxbow).
  - Long rivers get their current from a larger area (`riverDrainageSize`, "river drainage area") so they find their
    real outlet.
- **Saved once, never re-planned (2.12.x).** Each area's stream network is saved with the world. A later settings
  change or mod update never re-plans land already carved: "land already generated keeps its streams exactly".
- **Carving** happens per chunk into the real terrain, then the channel is dressed with banks as rings out from the
  water — bed, waterline, bank, optional second bank — whose sizes are a percentage of stream width. Bank styles are
  biome-aware and come from config or datapacks. Banks stand at least a block above the water near drops so it cannot
  spill. Structures are kept out of streams; villages bridge them.
- **Off the main thread**, in parallel; chunk loading waits on it and the log says so. Optionally players who have the
  mod plan streams for the server (helpers are sent the world seed).

## 2. The flowing water

- **Water flows over source blocks.** Vanilla water stops at a source block; here it keeps momentum and direction, so
  drops become cascades instead of flat pools. Toggleable on its own.
- **A current per chunk.** It is worked out the first time anyone looks at a chunk and then kept; currents a player
  has seen keep their direction for good, through setting changes and updates.
  - It pushes players, boats, mobs and items (separate strength sliders). Boats go upstream and climb small falls.
  - Lakes are still except near where streams enter or leave; a stream entering a river turns into its current.
  - Vanilla clients may join a server and see still water: the carved valleys and banks are part of the world.
- **Rivers drain to the sea**: vanilla and modded rivers get a current toward the nearest connected ocean.

## 3. Re-flowing an existing world (2.16.0 – 2.17, October–November 2026)

| Command / tool | What it does |
|---|---|
| `/streams reflow radius <n>` or `all` | Gives water already in the world (ponds, streams, hand-dug channels, WorldPainter maps) a downhill current: ponds drain through every exit, streams flow into each other and on to the sea, water with nowhere lower to go stays still. |
| `/streams reflow clear radius <n>` or `clear all` | Removes that current. |
| `/streams reflow carve radius <n>` | Cuts new streams into land generated without the mod, following the ground actually there; built-up areas and villages are left alone. |
| `/streams reflow carve preview` | Shows the plan, changes nothing. |
| `/streams reflow carve undo` | Puts the land back exactly. |
| `/streams regen` | Marks the surrounding chunks and regenerates them from scratch at the next world open. It replaced `/streams carve` (2.12) because re-carving in place carved over its own output and wrecked the area. |
| Flow Wand (2.16) | Draws currents by hand like a pen tool; sneak-click = still mode (calms a patch or strip). |
| Carve Wand (2.17) | Lays out a path with width and height per point and carves a real stream (may run uphill); Ctrl+Z undoes, including dug ground. |

Reflow and regen change chunks only on command or at load; nothing re-plans land in the background.

## 4. What carries over to our forge

Our forge (`crates/worldgen/src/forge/`) already does the heavier version of their planning: Priority-Flood routing
(`erosion.rs` `route_into`), then the linear-time depression graph (`routing_fast.rs`, Cordonnier et al. 2019),
discharge, stream-power erosion over coarse-to-fine levels (`mod.rs` `forge_with`), post-shaping
(`post.rs`: floodplains, lake basins) and river extraction with Strahler order (`hydrology.rs` `find_rivers`).
Their hard-won rules worth keeping:

- **Plan once, freeze, never re-derive.** Their worst bug class was re-planning land already carved. Our forge
  being a pure function of seed + forged artifact is the safe form of the same rule.
- **Never edit in place on top of your own output** — `regen` regenerates rather than re-carves.
- **Every stream ends somewhere real** — ours via Priority-Flood + outlets.
- **The channel threshold must not depend on grid resolution** — our `Knobs::hillslope_area_m2` exists for the same
  reason.
- Ideas not in our forge: per-chunk current fields derived from routed water (for moving water in play), a
  plunge-pool rule for small pits, oxbow cut-offs, and an operator "reflow" that gives player-dug channels a current.

## Sources

- https://modrinth.com/mod/streams-reflowing
- https://api.modrinth.com/v2/project/streams-reflowing/version (changelogs)
- https://www.curseforge.com/minecraft/mc-mods/streams-reflowing
- https://www.modpackindex.com/mod/157991/streams-reflowing
- https://github.com/nice-john/SR-Public (issue tracker only)
- https://www.modpackindex.com/mod/104269/dynamic-waters-realistic-flowing-rivers (a similar mod)
