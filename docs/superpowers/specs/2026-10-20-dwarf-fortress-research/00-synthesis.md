# Dwarf Fortress direction — research synthesis (read first)

2026-10-20. Research and plan only; no engine code changed. Produced from the handoff
`docs/superpowers/plans/2026-10-20-dwarf-fortress-direction-research-handoff.md`. The phased plan
`../2026-10-20-dwarf-fortress-transition-plan.md` was **rejected**: features are implemented
piece by piece, roughly modelling DF. The research and the recommendations below are reference
material for those pieces, not decisions.

## Files

| file | what it answers |
|---|---|
| `01-world-and-embark.md` | world → region → local hierarchy, history/civs/legends, embark preparation |
| `02-map-and-z-levels.md` | tile model, ramps/stairs/digging, geology layers, caverns/aquifers, **mapping DF tiles onto 1 m blocks** |
| `03-designations-and-jobs.md` | designations, labors/work details, job selection, hauling, **DF's pathfinding** |
| `04-dwarves.md` | attributes, skills, personality, needs, stress, moods, eat/drink/sleep numbers, body |
| `05-economy-and-production.md` | items, stockpiles, materials, workshops/reactions, quality, wealth |
| `06-agriculture-and-food.md` | calendar, farming, brewing, cooking, animals |
| `07-rooms-and-buildings.md` | zones, room value, furniture, mechanisms, traps, tile attributes |
| `08-fluids-and-environment.md` | DF water/magma 1–7 + pressure vs. our water design, temperature, weather, miasma |
| `09-population-and-outside-world.md` | migrants, nobles/mandates, caravans, diplomacy |
| `10-threats-and-military.md` | threat ladder, squads, combat model |
| `11-fail-states-pacing-performance.md` | losing, time scale, **FPS death and DF's mitigations** |
| `12-ui-and-interaction.md` | premium UI layout, z-navigation, what the view must support |
| `13-adventure-mode-and-traversal.md` | brief: constraints adventure mode puts on the MVP |
| `engine-mapping.md` | Part 2: how each MVP system lands in this codebase |

**Evidence**: DF wiki pages read as raw wikitext on 2026-10-20 (the wiki is mostly v50/premium
with some v0.47 text left in), the 2021 Stack Overflow interview with Tarn Adams, and DF release
notes via search. Claims from memory of the game are tagged `[mem]` and were not re-verified.
DF is closed source: internal algorithms beyond what Tarn has said are reconstructions.
Terrain worldgen is *not* re-researched — `../2026-09-20-df-worldgen-research/` covers it and the
offline forge implements most of it.

## The ten things that matter most

1. **Control is indirect and sparse.** The player marks the world (designations, zones,
   stockpiles, buildings, work orders); jobs come out of the marks; dwarves choose jobs
   themselves. Player input is a few commands a minute. All per-tick activity is AI.
2. **The cost is agents, not search.** DF: units' turns are ">60 % in larger forts, of which
   less than 10 % is actually pathfinding"; the slowest system is O(n²) line of sight; then
   temperature on items. DF makes pathing cheap with **A\* + connected components** ("removes
   nearly all failed A* calls") and integer traffic costs (1/2/5/25).
3. **DF's numbers are in ticks.** 1,200 ticks/day, 403,200/year; hunger/thirst/drowsiness +1 per
   tick against fixed thresholds; walking gait 900 = 9 ticks per tile; 100 ticks/s cap = a
   67-minute year.
4. **Everything is data.** Creatures, materials, plants, items, entities (civs), reactions are
   raws; modding is editing raws. One material system covers stone, metal, wood, flesh.
5. **The tile is a shape × material × flags cell** in 16×16×1 blocks; floors belong to the tile
   above the wall; ramps move diagonally across levels; undug stone is hidden.
6. **The fortress is a stress machine.** Needs → thoughts → stress → tantrums → deaths → more
   stress is DF's signature failure; room value, food quality and leaders are the levers.
7. **The outside world arrives through one seasonal scheduler**: migrants, caravans, ambushes,
   sieges (population 80), megabeasts (wealth 100k, pop 80) — each sized by fortress wealth,
   population and danger.
8. **Losing is fun, and the fortress persists.** No win state; a dead, retired or abandoned
   fortress remains a site in the world for reclaiming or adventure mode.
9. **Fluids are a 1–7 per-tile CA with "pressure" teleportation**, infinite sources and no
   conservation. Our planned water (finite, bodies + frontier CA, deterministic) reproduces the
   behaviours that matter and is cheaper for standing water; DF's pressure quirks needn't be
   copied.
10. **History is offline and separable.** It makes stories, but fortress mode touches it through a
    handful of numbers (civ population, neighbours, war/peace). Stub them for the MVP; grow the
    history layer in the forge.

## MVP tier table

**MVP** = needed for a playable first fortress. **Soon** = needed soon after. **Later** = beyond.

| group | MVP | soon after | later |
|---|---|---|---|
| World & embark | world-map screen from the forge artifact, pick a region, generate the local map, Play Now party (7 dwarves + loadout) | Prepare Carefully, origin-civ stub | history, civs, sites, legends, megabeasts as figures |
| Map | tile map (wall/floor/open/ramp/stairs), materials, soil + 2–3 stone layers + one ore, hidden tiles, tile attributes, trees as single columns | constructions, smoothing/engraving, cave-ins, designation priorities | aquifers, caverns, magma, multi-tile trees |
| Designations & jobs | dig/channel/stairs/ramp/chop drags over z-ranges, job board with reservations, fixed labors, hauling one item, A\* + components + traffic costs | work details UI, traffic/burrow UI, wheelbarrows/bins/barrels | work orders, minecarts |
| Dwarves | unit from creature raws, walking gait, hunger/thirst/sleep with DF thresholds, a few skills with XP and job speed, minimal wounds + death | attributes, quality roll, thoughts → short-term stress → mood face | facets, needs/focus, long-term stress, breakdowns, relationships, strange moods/artifacts, full body model, medicine |
| Economy | items (type + material + location + stack) with indices, stockpiles (category level), carpenter + mason + still + kitchen as data reactions, materials raws | stockpile filters/links, smelter/forge/fuel, quality + value, wealth | cloth, leather, glass, trade crafts, mechanical properties |
| Agriculture | calendar, farm plot (1 surface + 1 underground crop), seeds back, plant gathering, still | easy meals, butchery/fishing/hunting | fertiliser, irrigation, pastures, breeding, pets |
| Rooms & buildings | bed/table/chair/door as buildings, meeting area, bedroom, dining hall, tile attributes | room detection + value + rank thoughts, trade depot, cage traps | hospital, locations, mechanisms/levers, floodgates, pumps, power |
| Fluids & environment | static forge water as terrain, water-source drinking | miasma | flowing water (our CA + bodies, DF depth rules), freezing, magma, temperature, weather, aquifers |
| Population & world | two hardcoded migrant waves, map-edge arrival, expedition leader | wealth-driven waves, mayor/administrators, caravans + barter | mandates, justice, diplomacy, wars, missions, barony |
| Threats & military | one hostile wildlife type, minimal combat, civilians flee | squads with kill/station orders, ambushes, corpses + burial | schedules, training, sieges, megabeasts, forgotten beasts, siege engines |
| Pacing & fail | DF-tick calendar, pause + speed dial, all-dead fail state, abandon, **save/load**, announcements, population caps | — | retire/reclaim with the fortress persisting in the world |
| UI | z-cutaway + wheel stepping, hidden tiles, rectangle drags, overlays, unit sheet + tile inspector, pause/speed/calendar | level minimap + depth bar, stocks/work-details/stockpile screens, multi-level drags | thoughts/personality panels |

## Recommendation (engine)

Detail and trade-offs in `engine-mapping.md`; the short form:

- **The fortress is one `Region` of a new kind** — same `RegionRunner`/`WorldManager`/snapshot
  seams, different content: a dense **tile map is the simulation truth**; voxel chunks, rapier,
  the player body and slimes leave the fortress sim; the client derives render chunks from tiles.
- **One DF tile = one 1 m block, one z-level = 1 m**; worldgen half blocks become natural ramps.
- **Netcode: deterministic lockstep at prediction depth 0** for the fortress region. Fortress
  input is sparse and indirect, so prediction buys nothing, while rollback costs scale with
  exactly what DF has a lot of (agents × depth, plus undo-logging every mutation every tick).
  Lockstep is a pacing mode of the engine we already have (same deterministic sim, same
  input-only transport, same state hash), co-op on one fortress comes free, and a deterministic
  sim can still be run server-only later if wasm clients can't keep up. Rollback + prediction
  stay for adventure mode's avatar.
- **DF tick semantics** with base speed 20 ticks/s (our `TICK_RATE` 50 ms, unchanged — a 900-gait
  dwarf walks 2.2 m/s), a speed dial to 5× (= DF's pace) and pause.
- **Pathfinding**: A\* on the tile grid + incremental connected components + integer traffic
  costs + Dijkstra fields for "nearest X"; no KCC per dwarf.
- **Agents event-driven**: wake-up queue, need counters as timestamps, bounded sensing via a
  spatial hash, stateless `roll()` randomness.
- **Raws in `assets/raws/*.ron`** grown from the block registry, content-hashed between client and
  server; wasm mods attach behaviour later.
- **Z-cutaway** = shader clip plane + a back-face "cap" coloured from a per-level material
  texture: a level step is a uniform + one small texture upload, never a remesh.
- **Embark = one region at `REGION_CHUNKS = 64`** (128 × 128 tiles, between DF's 2×2 and 3×3) on
  the 16 km forged world; neighbours render as LOD scenery; handoff, near window and cell mode
  are parked (kept compiling and tested) for adventure mode.
- **Save/load the fortress to a file in the MVP**; the in-memory parking lot is not enough for a
  game whose sessions last hours.

## Open questions for Dawid (block phase 1)

1. **Multiplayer shape for the MVP.** Recommended: single player, built on lockstep so co-op on
   one fortress is a later toggle. Alternatives: keep rollback + prediction (more cost, no gain
   for indirect control) or a thin client with server-only sim (drops the determinism discipline
   and needs a replication layer). Which?
2. **Faithfulness.** Proposed simplifications on purpose: no DF pressure quirks (diagonal
   regulators, minus-one-level U-bends); a wound list per body region instead of tissue layers in
   the MVP; no hardcoded-vs-reaction split (all workshop jobs are data); fortress-mode-only time
   scale. Keep DF's exact tick numbers? (recommended: yes). Anything you consider sacred that
   must be in the first playable?
3. **Tile scale.** One DF tile = one 1 m block with ~0.85 m dwarves (recommended), or 1 m × 1 m ×
   2 m levels (human-scale rooms, re-quantised terrain), or 2 m cubes (rejected: 8× volume)?
4. **Z-levels.** MVP depth: surface + ~60 levels of stone below sea level, no caverns/magma
   (recommended); caverns in a later phase. How deep should the final game go (DF: 50–150
   typical)?
5. **World size.** Keep the forge's 16 km world (≈ a 21×21 region-tile DF world, between Pocket and
   Smaller), or aim bigger? Embark = one 128 m region (recommended) or 256 m?
6. **History and legends.** Out of the MVP with stubbed numbers (recommended), or wanted early?
7. **Game speed.** 1× = 20 ticks/s (a 5.6 h year; DF's 67-minute year at 5×) — happy with that
   framing, or should 1× be DF's pace?
8. **Fluids timing.** Flowing water is the most "DF" system after digging (floods are the
   classic disaster). Put it straight after the MVP (recommended) or earlier?

## Not done in this session

- No throwaway spike (e.g. an A\* cost estimate on a real forged embark); estimates in
  `engine-mapping.md` are marked as such. Worth doing at the start of phase 3 if you agree.
- No subagents were used; research ran inline.
- The phased plan was rejected; CLAUDE.md "Direction" and `TODO.md` now say features are built
  piece by piece and point here.
