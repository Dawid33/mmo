# Handoff: civilizations, territories, religions (world-level society layer)

Written 2026-11-01 at the end of the session that produced the DF-scale terrain design. Purpose:
let the next session start the **design (brainstorm) → plan** for the society layer without
re-researching. Nothing here is built, and nothing here is approved design. The next session runs
`superpowers:brainstorming` with the user first.

## The ask (user's words, 2026-11-01)
"compare it [Azgaar's FMG] to df map generation, goal is to start creating a plan for generating
territories, civilisations, religions, etc. The creation of actual settlements will be procedural
on region load and out of scope of this plan, unless it is required for it."

The user then chose to do the terrain first ("what you described as the df map gen is what I
envision"). That design is written and awaiting approval; this layer sits on top of it.

## Read first
1. `docs/superpowers/specs/2026-11-01-df-scale-world-terrain-design.md`: the world this layer
   lives on. It is a draft awaiting the user's approval. It covers:
   - a ~100 km DF Medium–scale world of 196 × 196 regions of 512 m;
   - continental layouts with per-edge sea or land;
   - 1–3 km ranges;
   - °C temperature, mm/yr rain, derived drainage, volcanism;
   - DF-style biomes per 16 m cell, flood-filled **regions** with ids;
   - savagery and good/evil stored as overlays;
   - a compact world file with region tables;
   - biomes visible in play.

   The society layer reads all of that, and its own output goes into the same world file.
2. `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/14-history-civilizations-religion.md`:
   DF's civs, placement and spread, territory, religion and spheres, entity raws, history sim,
   historical figures, and what fortress mode reads. Sourced; the unknowns are listed at the end.
3. `docs/superpowers/specs/2026-10-31-azgaar-fantasy-map-generator-research.md`: how FMG does it
   (one static pass, cost-capped floods), with formulas.
4. `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/01-world-and-embark.md` §2–4 and
   `09-population-and-outside-world.md`: the fortress-mode coupling (migrants, caravans, sieges,
   neighbours) and MVP tiers. That earlier note: "stub those numbers in the MVP and grow the
   history layer offline in the forge".

## FMG vs DF in one table

| | Azgaar FMG | Dwarf Fortress |
|---|---|---|
| Unit | ~10 k Voronoi cells | world tiles (17²…257²) + per-site detail |
| Placement | ranked cell suitability (`cells.s`: habitability + rivers + coast) with spacing | entity `START_BIOME` / `EXCLUSIVE_START_BIOME`; civs block each other's starts |
| Territory | multi-source Dijkstra flood with per-culture-type cost tables, a cost cap, unclaimed land left over | `BIOME_SUPPORT` weights spread an internal territory; sites + per-site claimed tiles + tribute links; **no border polygons** |
| Time | none; one pass | a history sim over N years (settler armies found sites, wars, figures, artifacts) |
| States / wars | dice rolls over final borders | emergent from ethics clashes, baby-snatching, populations |
| Religion | folk = culture; organised religions flood along roads; heresies | pantheons from 130 spheres (1–2 base + friends, never precluded); v0.47 prophets → temples → holy cities, crossing civ lines along trade routes; demons and goblins |
| Roads | Urquhart graph + shared-trunk Dijkstra | world constructions (road, tunnel, bridge) between sites |
| Names | syllable Markov chains per name base | word lists per language |
| Neighbours | — | reachability (oceans and mountains cut you off), ~30-tile interaction radius |
| Cost | seconds | minutes + GB; history length and site cap dominate |

The FMG doc's recommendation (§"What this means for us"): take FMG's cheap machinery as the
**year-0 placement**, i.e.
- the suitability score;
- one generic cost-capped flood with per-type cost tables, for civs, religions and creature
  ranges;
- Urquhart roads;
- syllable names;
- per-stage sub-seeds.

Then decide how much of DF's **history simulation** to run on top. FMG alone gives no stories;
DF's identity is the stories.

## Constraints already known
- **Offline, in the forge pipeline (or a stage after it).** It is not deterministic-sim code.
  It writes into the world file. Unlimited offline budget, as for terrain.
- **Settlements are generated on region load, out of scope.** This layer decides *that* a site
  exists, where, of what race / civ / type / size, and its relations. It does not decide the
  buildings or the layout. The region-load generator later reads these records.
- **Scale**: 196² regions ≈ 38 k region tiles; 16 m cells ≈ 39 M. Territory needs a graph between
  the two, e.g. 64–128 m cells or a jittered Voronoi graph of ~50–200 k cells over the forged
  rasters. FMG uses 10 k cells for a whole map; DF a 129² grid. Builder's call, but decide it in
  the design.
- **Inputs from the terrain design**:
  - biome per cell and region ids;
  - elevation;
  - mountains (dwarf starts);
  - rivers with order and discharge;
  - lakes and coasts;
  - temperature and rain;
  - savagery and good/evil;
  - reachability: landmasses, water separation, impassable ranges.
- **Entities are data**: a DF-like entity table (race, start / settlement biomes, spread weights,
  site types, ethics, values, religion type and spheres, world constructions). It needs a
  creature roster, and `tools/creatures/` has DF creature ids + models (dwarf, etc.).
- **The game reads**:
  - home civ + its sites (embark);
  - neighbours (trade / war) and their reachability;
  - migrant / caravan / siege sources;
  - deities for worship and temples;
  - names for regions, sites, rivers and civs;
  - the world-map overlays (territory, sites, roads).

  The world map (`WorldMap` / `TileSummary`, `crates/game/src/world_map.rs`) is the first
  consumer.
- **Session lifecycle**: anything per-world on the client must be session-scoped (see
  `CLAUDE.md`, "Session lifecycle").

## Open questions for the brainstorm (theirs to answer)
1. **History or no history?** Choose among:
   - (a) FMG-style static placement only;
   - (b) static placement + a short coarse history (centuries of site founding, conquest,
     population and religion spread at ~1-year steps, no individual figures);
   - (c) DF-style figures and events.

   This decides the size of the project. My recommendation: (b) first, built so (c) can grow on
   it.
2. **Which races and civs at first?** Dwarves only plus "others" as stubs, or the DF five
   (dwarves, humans, elves, goblins, kobolds)? This ties to the creature roster.
3. **Territory model**: filled territories with borders on the world map (FMG), or DF-style sites +
   claimed tiles + tribute, with no borders drawn?
4. **Religion depth**: pantheons from spheres only, or also organised religions spreading (temples,
   holy cities)? Do gods do anything (curses, demons → goblin civs)?
5. **Does the world keep evolving during play** (DF's world activation: armies move, sites change),
   or is it frozen at forge time for now?
6. **Names**: is a naming system (languages / word lists vs Markov syllables) part of this project?
   Region, river and site names are wanted on the world map.
7. **Player-facing surface in v1**:
   - world-map overlays (territories, sites, roads);
   - embark choice of home civ;
   - a legends / history browser;
   - neighbours list.

## Suggested shape (a starting proposal, not decided)
1. **Society graph**: cells over the forged rasters, carrying suitability (habitability by biome
   + rivers + coast − altitude), passability cost per biome and terrain, and landmass /
   reachability.
2. **Entity data** (RON, like `assets/blocks/blocks.ron`): DF-like tokens, trimmed to what is
   used.
3. **Year 0**:
   - civ starts by entity biome rules with spacing;
   - territory by multi-source Dijkstra with per-entity cost tables (`BIOME_SUPPORT`-like) and a
     cap;
   - pantheons from a sphere table;
   - first sites.
4. **History (if chosen)**:
   - a yearly loop: population growth from territory suitability;
   - site founding by settler moves along the graph;
   - war / tribute between neighbours from ethics and pressure;
   - religion spread along roads and trade routes;
   - an event log.
5. **Roads**: Urquhart + shared trunks between sites, tunnels for dwarves.
6. **Output**: world-file sections (entities, sites, territory per cell or per region, roads,
   deities and religions, events, names) plus `WorldMap` overlays.
7. **Tests**: seeded reproducibility of the stage on one machine, plus statistics (civ count,
   sites, reach) in the forge summary, like the terrain stats.

## Practical notes for the next session
- Work on `develop` in the main checkout, no worktrees (`CLAUDE.md`).
- Use `just` recipes; scope tests per crate (`just test-one`, `just smoke`).
- The terrain design is not implemented. If this layer is planned before terrain lands, build it
  against the current 16 km forge output (which has rain + biomes-to-come) behind an interface
  that reads "biome per cell", or plan it to follow the terrain plan.
- Forge timing data for sizing the history sim's budget is in the terrain design's "Measured forge
  cost" table. Terrain alone at 100 km / 16 m is about 1.5 h on one core.
- DF's history cost is dominated by site count and history length. Expose both as knobs from day
  one.
