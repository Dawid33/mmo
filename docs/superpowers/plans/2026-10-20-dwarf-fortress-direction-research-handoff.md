# Handoff: Dwarf Fortress direction, research and MVP plan (2026-10-20)

This is a prompt for a fresh session. It asks for **research and a plan, not code.**

## Context

The game is changing direction again. A brief RTS pivot (branch `ds/rtsPivot`, its plan
`docs/superpowers/specs/2026-10-20-rts-transition-plan.md` there) is **deprecated**, and
`develop` was rebuilt from the MMO-era multi-region code (`ds/oldMMOCode`) plus the stable
toolchain, Blockbench model loading and the commander camera. The new
target is a **Dwarf Fortress–style game**. Looking down at the island through the new
commander camera reminded Dawid of DF, and the engine can carry it: deterministic voxel
sim, rollback netcode, a server/client split, offline worldgen.

The long-term vision, in Dawid's words, condensed:

- Recreate Dwarf Fortress **faithfully**, in our voxel engine.
- **Generate a huge world up front.** Players play in **individual regions**, seen from the
  current camera perspective: top-down, fixed pitch, pan and zoom.
- Later, **traverse between regions** in **adventure mode** and **fortress mode**.
- **The MVP is fortress mode only.** One player runs one fortress in one region of a
  pre-generated world.

Read CLAUDE.md first, especially "Building and Running" and "Workspace Layout":

- no worktrees;
- `just smoke` is the default check;
- ask before running client test binaries or benchmarks.

This session produces documents. It changes no engine code.

## What exists to build on

Check the current state of each item. Memory notes may be stale.

- **A multi-region world.** Regions tile the plane (`RegionCoords`), each `REGION_SIZE`
  wide (the build-time `REGION_CHUNKS` dial: 32 / 64 / 128 chunks = 64 / 128 / 256 m, up
  to 65 km in cell mode). The pieces:
  - each region runs its own sim on its own thread;
  - `WorldManager` handles sessions, homes, subscriptions and parking;
  - boundary handoff moves entities between regions, with ghost mirrors;
  - a 5×5 near window streams around the player, with server-built LOD rings beyond it;
  - cell loading (`cells.rs`) materializes parts of large regions lazily.

  CLAUDE.md describes all of it. A block is 1 m = 16³ voxels, and half blocks exist as
  chisel cuboids. Branch `ds/rtsPivot` has a runtime `worldgen::MapConfig`
  (`assets/map.ron`) worth porting if map config is needed.
- **Commander camera.** `crates/client/src/renderer/commander/` (the default view):
  - fixed-pitch pan and zoom that follows the terrain height, with the focus clamped to
    the home region;
  - a sparse per-column `HeightGrid` across loaded regions;
  - a minimap;
  - click and box selection as sim keys;
  - a `GroundPick` resource with the terrain point under the cursor.

  DF needs a **z-level cutaway view** on top of this: hide everything above the current
  level and step levels with keys. Assess how far the camera, the pulled-quad renderer
  (`renderer/voxel_pull/`, `assets/shaders/voxel_pull.wgsl`) and the mesher can go toward
  that.
- **Deterministic rollback sim.** The `game` crate, the `#[rollback]` macro, the undo log,
  `StateHash`, `just determinism-wasm`. Entities are generic (`EntityKind`), and the slime
  AI in `slime.rs` is the template for agents: a stateless hash RNG, so there is no RNG
  state to undo.

  Rollback re-simulation cost grows with agents × rollback depth. DF has hundreds of
  creatures, each with jobs, needs and pathing. This is **the central technical risk**:
  look hard at whether fortress mode needs rollback at all (single player, or co-op on one
  fortress) or could run as a server-authoritative lockstep or plain authoritative sim.
- **Offline world generation research and the prototype.**
  - `docs/superpowers/specs/2026-09-20-df-worldgen-research/` (start with
    `00-synthesis.md`) is a study of **DF's own worldgen pipeline**: fields, rivers, lakes,
    erosion, geology, and adapting it to a chunked generator. Reuse it; don't redo it.
  - `just forge-proto` (`crates/worldgen/examples/forge_proto/`) already simulates a
    16 km ocean-bounded world offline: stream-power erosion, orographic rain, drainage,
    lakes, river objects, geology. It writes a `.world` file that `worldgen/src/forged.rs`
    serves through `generate_region`, selected with `VOXEL_WORLD_FILE` /
    `just world_file=… run`. The `just forge-ui` tuning page exists too.

  This is very close to "generate a huge world up front".
- **Region loading and traversal machinery.** It is all on `develop`: region handoff,
  parking, the near window, LOD rings, cell loading, island regions
  (`docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md`) and the
  `just fly-probe` / `shuttle-probe` traversal probes. Assess what fortress mode (one
  region, many agents) and later traversal need from it, what to simplify, and which known
  issues bite. The memory notes and TODO.md list them: the client-crossing bit-inexact
  regression, flight outrunning streaming, and the dropped-packet gap.
- **Water.** `docs/superpowers/specs/2026-09-22-voxel-water-efficiency/00-synthesis.md`
  decided on deterministic in-sim cellular water with finite volume. DF's fluid model
  (levels 1–7, pressure, flow) should be compared against it.
- **Other prior research worth reading:**
  - `2026-09-20-modding-wasm-research.md`: deterministic wasm mods. DF's raws are
    data-driven, and modding is part of DF's identity.
  - `2026-09-20-multi-instance-k8s-postgres-research.md`: regions as partition units.
  - `2026-07-10-veloren-multithreading-research.md`.
  - `2026-09-22-chat-and-commands-investigation.md`.

## Part 1: research Dwarf Fortress's base game mechanics

Use the current DF (Steam/premium era, v50+) as the reference. Note where classic and
premium differ. Primary sources:

- the DF wiki (dwarffortresswiki.org);
- Bay 12 developer notes and Tarn Adams's talks and interviews;
- the raws themselves, for the data model.

For each system, record:

- what it does for the player;
- the rules and data behind it (the raws structure, the key numbers);
- how it connects to the other systems;
- how complex it is to simulate (per tick, per agent);
- an **MVP tier**: needed for a playable first fortress, needed soon after, or later.

Cover at least:

1. **World and embark.**
   - World generation and history: civilizations, sites, the legends that worldgen
     produces.
   - The world map → region → local map hierarchy: the embark tile grid, local map size,
     z-levels, how the embark area is cut out of the world.
   - Embark preparation: starting dwarves, skills, items, animals.
2. **The map and the z-level model.**
   - Tiles, z-levels, ramps and stairs, digging and channeling, constructions.
   - Cavern layers, the magma sea, aquifers, soil and stone layers, ores and gems.
   - How DF's discrete tile and z-level grid maps onto our 1 m block (16³ voxels) grid,
     and whether chisel or half blocks have a DF meaning.
3. **Designations and jobs.**
   - The designation types: dig, chop, gather, smooth, engrave and so on.
   - The job queue, job selection and priorities, labors and work details (premium),
     hauling.
4. **Dwarves.**
   - Attributes, skills and their gain, personality, needs, thoughts, stress and
     happiness, moods (strange moods, artifacts), relationships.
   - Sleep, eating, drinking.
   - Body and health: the tissue and wound model, at a level of detail suitable for an MVP.
5. **Economy and production.**
   - Stockpiles and their settings.
   - Workshops and reaction chains: wood → furniture, stone → blocks, ore → bars →
     goods, food and drink, cloth and leather.
   - The material system: inorganics, organics, material properties.
   - Item quality and value, wealth.
6. **Agriculture and food.** Farm plots, seasons, crops, brewing, cooking, butchery,
   fishing, gathering, animals and pastures.
7. **Rooms and buildings.** Bedrooms, dining halls, meeting areas, zones, and how room
   value affects thoughts. Doors, furniture, traps and mechanisms (levers, floodgates).
8. **Fluids and environment.** Water and magma levels (1–7), flow and pressure, pumps,
   wells, temperature, weather, seasons, light and darkness, miasma.
9. **Population and the outside world.** Migrant waves, nobles and mandates, caravans and
   trade, diplomacy.
10. **Threats.** Wildlife, ambushes, sieges, megabeasts, forgotten beasts and cavern
    creatures, the military (squads, equipment, training, orders), the combat model, at
    MVP depth.
11. **Fail states and pacing.** How a fortress grows and dies ("losing is fun"), the
    game's time scale (ticks per day and per season), and fortress speed and FPS death.
12. **The UI and interaction model** that the view has to support. Premium's mouse UI is
    the more relevant one: z-level navigation, menus, designation dragging, info panels.
13. **Adventure mode and region traversal.** Brief, for later: how the world map,
    fast travel and site loading work. The MVP must not paint itself into a corner here.

Also collect **performance facts**: why fortresses slow down (pathfinding, temperature,
item counts, fluids) and what DF does about it (path caching, connectivity maps). Those
lessons apply directly to a deterministic sim.

## Part 2: map it onto our engine

Write how each MVP-tier system lands in this codebase:

- **Sim shape.** Is the fortress a `Region` sim? What does a tick mean? DF runs about 100
  ticks per in-game day at up to 100 FPS; compare with `game::TICK_RATE`. What is the agent
  count budget?
- **Netcode and authority.** Fortress mode is single player in the MVP. Should the
  fortress sim still go through rollback and prediction? Is server-authoritative with a
  thin client enough? Or should it run fully locally, the way the offline `LocalServer`
  and wasm `?offline` do today? Give a recommendation with trade-offs (see the memory note
  on design questions needing trade-offs). Include what co-op on one fortress or
  neighbouring fortresses would need later.
- **Pathfinding at scale.** Hundreds of agents on a 3D voxel map with ramps, stairs and
  digging. Consider connectivity components (DF's approach), hierarchical pathfinding, and
  flow fields. It must be deterministic and fit the undo log. Rapier's character
  controller per dwarf is almost certainly wrong.
- **World and region scale.** What a DF embark is in our units:
  - world size;
  - region size (the `REGION_CHUNKS` dial, height in chunks);
  - how the forged `.world` file becomes the world map plus per-region generation;
  - how much of the multi-region machinery the fortress MVP keeps running (a single
    fortress may be one region with its neighbours as scenery or LOD) versus only for
    traversal.
- **Data-driven content.** A raws-like registry for creatures, materials, plants,
  reactions and workshops. It grows from the existing block registry
  (`assets/blocks/blocks.ron`, `game::registry`). Note where wasm mods could hook in.
- **Rendering and the view.**
  - The z-level cutaway on top of the commander camera.
  - Designation overlays and job markers.
  - Item and creature rendering: Blockbench models, `crates/bbmodel`, the asset pipeline
    in `tools/assets/`.
  - Hidden and unrevealed tiles (DF hides unmined stone).
- **What to remove or park.** The RTS-specific parts:
  - the commander's RTS selection semantics;
  - slimes as stand-ins;
  - the first-person body and block break/place, if dwarves do all the digging;
  - forged-world status (it becomes central).

## Part 3: the fortress-mode MVP plan

Propose a phased plan in the style of the RTS transition plan on `ds/rtsPivot`: short phases, each
one leaving a playable build. A plausible shape, which you should challenge:

1. A world generated up front, plus embark: pick a region from a world map, then generate
   the local map.
2. The z-level view, and designations for dig and chop.
3. Dwarves as agents that do jobs: pathing, a job queue, hauling.
4. Needs (eat, drink, sleep), stockpiles, a first workshop chain, farming.
5. Migrants, a first threat, and the fortress fail state.

For each phase give:

- **scope**, and what "done when" looks like in a live run;
- the files and crates touched;
- **risks**, with the determinism and rollback cost of agents first;
- the tests and verification recipes (`just smoke`, `just test-one`,
  `just determinism-wasm`, `just test-threads`).

End with **open questions for Dawid**, the decisions that block phase 1:

- multiplayer shape for the MVP;
- how faithful to be: what to simplify on purpose;
- tile scale: is one DF tile one block;
- how many z-levels and how deep;
- world size;
- whether worldgen history and legends are in scope early.

## Deliverables

- `docs/superpowers/specs/2026-10-2x-dwarf-fortress-research/`:
  - `00-synthesis.md`: read first. A summary, the MVP tier table, the recommendation and
    the open questions.
  - One file per system group from Part 1, with sources linked.
  - `engine-mapping.md` for Part 2.
- `docs/superpowers/specs/2026-10-2x-dwarf-fortress-transition-plan.md`: the Part 3 plan.
  It becomes the project direction once Dawid approves it.
- Once the plan is approved, update the CLAUDE.md "Direction" paragraph, `TODO.md` and the
  memory note `game-direction-pivot`.

## How to work

- This is research-heavy. The `research` / `research-deep` / `research-report` skills fit
  Part 1, and so do parallel subagents per system group if Dawid agrees. Ask before
  spawning subagents; the default is to work inline.
- Prefer primary sources. Mark any claim you could not verify. Note which numbers are from
  classic DF and which from premium.
- Don't re-research what `2026-09-20-df-worldgen-research/` already covers. Cite it, and
  only fill in the gaps: history, civilizations, embark.
- Lay out trade-offs in prose before offering design options (memory note
  "design-questions-need-tradeoffs").
- Write no engine code in this session. Small throwaway spikes (for example a pathfinding
  cost estimate) are fine if Dawid agrees; keep them out of the workspace crates.
