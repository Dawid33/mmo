# Earth-like world overhaul — umbrella design

Status: APPROVED by Dawid 2026-11-14, with four changes folded in below: single-process wrap confirmed;
the old layouts, presets and options are REMOVED, not kept as a legacy path ("this plan supersedes all
aspects of current world gen"); worlds come in several sizes ("200x100km is quite large, add options to
scale it down"); and forge time is NOT capped yet ("for now we just take measurements, I will judge
whether it takes too long for the result achieved").

**Amended 2026-11-19** (the program `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` drives the
build and wins where the two disagree): Phase 1a Tasks 1–2 are built (`8d6b29a2`, `77f8e8a3`: the forge grid,
the world file, the tiles, the sampler and `world.json` are width × height); the work was paused 2026-11-15 and
resumed 2026-11-19. Changed below: the test rule (Constraints), the multi-host seam row (hosts are by id now),
the day length and the "fortress view" reasoning (Decision 6, Non-goals, Phase 6 — the game is one first-person
character since 2026-11-16), and what a phase's "gate" is (an evidence run, not a committed test).

Research behind it: `docs/research/reports/Earth like world generation overhaul.md` (the report) and the
seven note files in `docs/research/research_notes/Earth like world generation overhaul/`. The report's
evidence ledger says which claims were read from sources and which are untested designs; this document
does not repeat it.

This is an umbrella: it fixes the decisions and the architecture every phase shares, and the order of the
phases. Each phase gets its own spec and plan when it starts. The first one to be planned is **Phase 1,
the planet frame** (below).

## Intent

The default new world should read as a small Earth. A player looking at the world map should see several
continents, mountain ranges of different sizes and ages on them, a hot belt across the middle and ice at
the top and bottom, coasts with bays, deltas and fjords, and biomes that sit where Earth would put them.
Over the in-game year the weather should cycle the way Earth's does. Things should be where they are for
reasons a player can read off the map, because they came out of a simulation and not out of noise.

Dawid's words: hottest at the equator in the centre of the map, glacial top and bottom; "mimic earth
generation as close as possible in the default settings, so continents and mountains are not random but
simulated"; today "mountains usually show up as one big mountain range", wanted is "continents and on those
continents one or more mountain ranges of various sizes", with "different kinds of mountains depending on
their age, new and old"; all the biomes of the 72-biome list; "all the water features like lagoons, bays,
lakes, rivers etc, glaciers"; "weather / climate … which changes over the year in a cyclical nature like
earth".

## Goals

1. Equator across the middle of the map, glacial poles at the north and south edges.
2. Continents and mountains from a plate simulation: several continents, one or more ranges per
   continent, range size and shape depending on what built it and how long ago.
3. The 72 biomes placed from seasonal climate, terrain and water.
4. Water features that exist for a reason: rivers by regime, lakes by origin and water balance, deltas,
   estuaries, fjords, lagoons and barrier islands, reefs, glaciers and sea ice; bays, capes and straits
   detected and named.
5. A yearly cycle at runtime: temperature, rain and snow, freeze and thaw, following the stored climate.
6. All of that in the DEFAULT settings of a new world, forged offline.
7. One world generator. The Earth-like planet is the only kind of world; what it replaces is deleted.
8. Worlds in several sizes, all 2:1, from a few kilometres across to 200 × 100 km.

## Non-goals

- A sphere. The world stays a flat map; a sphere "buys realism the player can never see".
- A real climate model (a GCM) or anything that needs a GPU in the forge.
- A playable deep ocean. Kilometre-deep sea floor is data on the planet grid; the playable sea floor stays
  clamped. Abyssal plain and trench are labels on the map, not diggable depth.
- Deciding the day length. The calendar's day stays whatever the one existing constant says (10 s on
  2026-11-19); whether a first-person game keeps it is the owner's decision, made outside this program
  (see Decision 6).
- The flowing-water engine. Seasonal river levels, tides and floating ice are STORED by the forge but not
  played until that engine exists (its two open decisions stay in `TODO.md`).
- Ground plants, and creatures tied to biomes. A biome stays a ground cover, a tree mix, a map colour and
  a history capacity, as today.
- Migrating existing worlds. Every stored-format change means re-forging; saved regions never regenerate.
- Bit-identical forge output across machines. The forge stays a stored artifact, as today.

## Anti-goals (it has failed if…)

- …a phase lands without its forge time, peak memory and file size measured at every world size and
  recorded here. There is NO time budget yet: Dawid judges cost against result from those numbers.
- …weather or seasons can differ across a region seam or between hosts, add anything to the state hash,
  a save cut or the wire, or break rollback.
- …old ranges come out flat, or every world still has one big range.
- …an option, preset or knob survives that no longer does anything, or that tunes a stage the overhaul
  has replaced.
- …the forge has no quality gate at some point between phases (the old gate goes with the island path; a
  new one on a Planet world replaces it in the same change).
- …a biome is on the map with no rule behind it that can be checked against Earth's shares.

## Constraints (from the project)

- `game`, `server`, `worldgen` stay engine-agnostic. `generate_region` stays a pure function of
  coordinates plus the stored world: every coarse decision the forge makes must be IN the artifact.
- Runtime sampling keeps to exact arithmetic (add, subtract, multiply, divide, floor, sqrt); sim code uses
  `libm` or integers, `BTreeMap`, and randomness only as `roll(tick, subject, salt)`.
- Biome discriminants are stored bytes: append only, never renumber. `WORLD_MAP_VERSION`,
  `ARTIFACT_VERSION` and `TILED_FORMAT_VERSION` are bumped on any change to what they cover.
- A new store table must never be writable by two hosts' cut parts.
- Any new per-world client resource is registered for session reset.
- New build/run/test commands get a `just` recipe in the same change.
- **Tests (owner's rule, 2026-11-19).** Only regression tests (a bug actually seen) and the existing gates
  are committed. The overhaul adds NO new gate: `forge_quality_matches_baseline` stays the one worldgen gate
  and is re-recorded on purpose at the end of Phases 1a, 2, 3 and 4. What this document calls a phase's
  "gate" is an EVIDENCE RUN — `forge_proto --report`, `just forge-bench`, `just world-map`, or a temporary
  test deleted before the commit — whose numbers are recorded in the program's Evidence section. Where a
  sentence below says a test "covers" or "pins" something, read it as such a run.

## Approaches

**World model.** Three were considered.

- *A. Pocket planet, two grids (chosen).* A coarse PLANET grid carries everything planetary — plates,
  crust and belt ages, pressure, winds, currents, monthly climate, coast classes, tides, deep bathymetry —
  with horizontal lengths in degrees of latitude or map fractions, never metres. The existing fine grid
  stays a true-scale landscape for erosion, rivers and landforms. Heights are true scale on both. Cost:
  tectonics and climate are stylised (Earth-scale numbers become shape parameters to re-tune); climate
  bands are kilometres wide, not thousands of kilometres.
- *B. A far larger world on coarser cells.* Closer to real proportions, but the forge's own cost model
  puts even today's 100 km at 16 m cells near 7 minutes and 7 GB, and the fine detail the game plays on
  would be lost.
- *C. Simulate on a sphere and project.* Needs spherical meshing and stretches the projection east–west
  toward the poles, for a map that is flat by design.

Dawid chose A ("go with your recommendations, pocket world etc").

**Map shape.** Square with a wrap was Claude's recommendation (cheapest). Dawid chose **2:1** — twice as
wide as tall, like Earth's 360° × 180° — for more room east–west: more continents side by side, larger
continental interiors.

**Delivery.** Phases with a gate each, in data order (frame → terrain → climate → water and ice → biomes
→ runtime), each landing on `develop`; versus one long-lived branch. Phased is chosen (Claude's call): the least-evidenced parts — plates on a flat map,
ocean currents, 72 sets of thresholds — are tuning problems only maps and validation shares can settle,
so each needs to be looked at before the next is built on it.

## Decisions

Dawid's:

- Pocket planet with the two-grid design.
- 2:1 map.
- "Go with your recommendations" for the rest; those are listed next as Claude's calls. Dawid approved
  them on 2026-11-14 except call 4, which he replaced:
- **The old world generation goes.** "Remove the other presets and world gen options in general, if no
  longer applicable, as this plan supersedes all aspects of current world gen."
- **Several world sizes**, scaled down from 200 × 100 km.
- **No cap on forge time yet.** Measure; Dawid judges.
- Single-process wrap is fine for Phase 1. Claude runs the forge timing.

Claude's calls (each is a proposal until this document is approved):

1. **East–west wrap, north and south are real edges.** Implement the existing plan
   `docs/superpowers/plans/2026-11-05-east-west-world-wrap.md` (cylinder, x only), extended from a square
   to a width × height world.
2. **Polar edges are explicit.** North and south are sea-or-ice edges chosen by the layout, not picked by
   the seed. Latitude runs 90° N at the top row to 90° S at the bottom (`pole = 3`, "Both", full span).
3. **World sizes** (Claude's proposal for Dawid's "various sized worlds"; all 2:1, width wraps):

   | Name | Width × height | Cells at 16 m | Cells at 32 m | Per degree of latitude |
   |---|---|---|---|---|
   | Tiny | 25,600 × 12,800 m (amended 2026-11-19: 12,544 is not a whole number of 512 m regions) | 1.3 M | 0.3 M | 71 m |
   | Small | 50,176 × 25,088 m | 4.9 M | 1.2 M | 139 m |
   | Medium (default) | 100,352 × 50,176 m | 19.7 M | 4.9 M | 279 m |
   | Large | 200,704 × 100,352 m | 78.7 M | 19.7 M | 558 m |

   Plus a development size of 16,384 × 8,192 m for debug worlds and tests (the wrap plan's minimum width
   is 8,192 m). The forge cell (16 m or 32 m) stays a separate choice, as in today's menu. Forge TIME is
   not capped; the menu shows the estimate and lets the player go. Memory is a physical limit, not a
   budget: the existing check against the machine's RAM stays (Large at 16 m is about 14 GB by the
   forge's model). Which size and cell is the default is confirmed from the Phase 0 numbers. Exact sizes
   are snapped as the wrap plan's D3 requires. (As built 2026-11-20: no snapping — every named size is already
   a whole number of 512 m, and an explicit width that is not, or that is under 8,192 m, is REFUSED at forge time
   and at load.)

   What a smaller world means: every size is a whole planet, pole to pole, so climate bands, continents
   and mountain belts shrink with it (they are map fractions), while rivers, valleys and lakes stay true
   scale. A Tiny world has belts a kilometre or two wide and so lower mountains. Every length in the
   planet stages must therefore be a fraction of the map or degrees of latitude; today's physical-metre
   climate constants (9 km inland relaxation, 40 km meander) become fractions in Phase 1.
4. **One generator, old options removed** (Dawid's decision; the removal schedule is Claude's). The
   Planet is the only layout, so `layout` stops being a choice. A preset, knob or code path is deleted in
   the phase that replaces what it tunes:
   - Phase 1: the island, archipelago, arc, continent and inland layouts and their knobs (the "Island"
     group, `sea_edges`, `continent_coast`, `inland_sea_size`, the island-only "Uplift" belt-noise knobs);
     `pole`, `latitude_span_deg` and `latitude_center_deg` (always both poles, full span); the presets
     Continent, Continents, Inland, Temperate band, Frozen north, Tropical, Island, Archipelago, Island
     arc. Presets that only change emphasis (Great ranges, Highlands, Lowlands, Dry, Wet) stay while their
     knobs exist.
   - Phase 2: the static plate knobs (`plate_count` as a Voronoi count, belt widths, interior ranges) and
     `continent_count` / `continent_sea_width`, replaced by the plate simulation's own.
   - Phase 3: the annual-climate knobs (wind band edges, the latitude temperatures, meander, relaxation
     lengths). Phase 4: `lake_basins` and the carved-lake knobs.
   The bit-exact pin `island_layout_unchanged` is deleted with the island path. The `forge_quality` gate
   and its fixture are re-recorded on a development-size Planet world in the same change, and every test
   that forges an island moves to a small Planet world. All existing worlds are re-forged (debug worlds,
   `worlds/default.world`, the public web world, dev Postgres worlds).
5. **Ocean depth is data only** (see Non-goals).
6. **Calendar: 336 days; the day length is NOT decided here.** (Amended 2026-11-19: the 10-second day was
   argued from the fortress view, which is gone; in first person a 10-second day is a strobe. The owner
   decides the day length.) Everything is specified in calendar days and reads the day length from the one
   existing constant, so a later change rescales the weather. The sim derives the date from the world tick
   (today only the coordinator knows it). Season names are local to the hemisphere.
7. **Monthly climate is stored coarse**: twelve monthly normals per CLIMATE cell of
   sea-level temperature, diurnal range, precipitation, wet-day fraction and wind (east and south
   components) — six integer planes, 96 B per cell; the lapse rate is re-applied from the fine height at
   read time; the fine annual precipitation raster stays as the rain-shadow modulator. The climate cell is
   about 256 m but never coarser than 96 rows pole to pole (the stride halves until it has them) and never
   finer than the forge cell: Dev 64 m, Tiny 128 m, Small to Large 256 m. The family is always resident —
   whole in the world file and in the tiled store's meta (`ForgedMeta`), never in tiles. The type is
   `game::MonthlyClimate` (Phase 6's weather lives in `game`, which cannot depend on `worldgen`); the
   world-position API is on `worldgen::forged::ForgedWorld`. (Amended 2026-11-20 by Phase 1b Task 3.)
8. **Seasonal state is derived, not edited**: snow depth, ice thickness and foliage are recipe data on the
   column, computed from (date, stored climate), in the manner of `water_top`. Only player edits (a
   cleared path, a hole in the ice) are stored, as sparse deltas with an expiry. Crops and soil moisture
   are real region state.
9. **Weather is a pure function** of world tick, global position and stored climate, living in `game`,
   computed identically by server and client; never streamed, never hashed.
10. **The 72 are four layers**: surface cell biomes (about fifty, in the `Biome` byte, appended); objects
    and point features (river types, springs, oases, vents, geothermal pools); per-column underground
    properties (caves, underground water, aquifers, sea caves); and a land-use overlay from history stored
    OVER the natural biome. Each cell stores its biome, a second choice and a weight.
11. **Biome backbone**: Köppen per cell as the label and the check against Earth; vegetation from plant
    limits (coldest month, growing degree days, a moisture index, dry-season length); treeline by
    temperature, which retires "above 1,200 m is Mountain".
12. **Validation shares are unweighted** — share of the playable map — and are RANGES around Earth's
    figures, since a generated world's land distribution differs.
13. **Art scope of the first biome pass**: every new biome gets a rule, a map colour, a ground cover and a
    tree list, reusing existing blocks where no art exists. New blocks and art are follow-up work.

## Architecture

```
seed + knobs
   │
   ▼
[planet grid, wraps in x]   plates over time → crust, belt age/kind, deep bathymetry, sea level
   │                         monthly climate → temperature, pressure, wind, currents, rain, ice
   │                         coast classes, tides
   ▼  resampled per level (computed once, never re-derived per resolution)
[fine grid, true scale]     initial height + uplift + age modifiers → erosion (rivers, ice) at a low
   │                         sea → flood → lakes by water balance → deltas, coasts, stamps
   ▼
classifier (passes)         sea → hard land states → shore → climate vegetation → local overrides
   │                         → fire state → underground → land use
   ▼
artifact                    fine rasters (as today, w × h) + a coarse raster family + objects +
   │                         the knobs it was forged with
   ▼
runtime                     generate_region (pure) · date from world tick · weather(tick, pos, climate)
                             · snow/ice/foliage overlays
```

What is kept: the erosion core (Priority-Flood, the depression graph, implicit stream power, the
coarse-to-fine schedule), the `World` contract between "tectonics" and "erosion", the forged-world store
and tiles, niche-based tree placement, regions typed by `RegionType`. Everything else in today's world
generation is replaced and deleted, phase by phase (Decision 4).

What changes shape, once, in Phase 1: the world is `width × height`, not `size × size`; x wraps; the
artifact carries a second, coarser raster family and the knobs.

## Phases

| Phase | Delivers | Gate |
|---|---|---|
| 0. Baseline | Time, memory and file size of today's forge at the size table's cell counts | Numbers recorded under "Measurements" |
| 1. Planet frame | 1a: `width × height` worlds in four sizes, east–west wrap (one process), explicit polar edges, both poles, the Planet as the ONLY layout on TODAY's plates and climate, old layouts / presets / knobs deleted, knobs stored in the artifact. 1b: the coarse monthly-climate family with a placeholder seasonal curve | A 2:1 wrapped world of each size forges and plays in one process; hot middle, ice top and bottom; walking off the east edge arrives in the west; the re-recorded quality gate passes; every size measured |
| 1c (later; the program's Phase 7). Multi-host seam | The wrap between the hosts of a `claim` world: canonical regions in `halo::neighbours`, the peer links, `super_region_of` and land assignment (Elastic 3: hosts are by id, land is assigned by the coordinator) across the seam, placement, observer sessions | Only once the owner has accepted the Elastic 3 build; until then the seam is open under `VOXEL_ROLE=all` and closed under every other role |
| 2. Plates and aged relief | The plate simulation on the planet grid; sea level by land fraction; belt age and kind feeding erosion (rebound, incision threshold, residual uplift); geology that varies in space | Seed score: continent count, belts per continent by age class; slope histograms differ between young and old belts |
| 3. Monthly climate | The twelve-month model; monthly normals stored; derived fields (snow, sea ice, growing degree days, evapotranspiration, aridity) | Köppen shares inside their ranges; three synthetic worlds; real-Earth topography regression |
| 4. Water and ice | Sea as a result (low stand, then flood); water-balance lakes; monthly discharge; sediment deposition; deltas; valley glaciers and ice sheets; coast classes and stamps; water table; feature detection and naming | Lake counts by climate; every river mouth classified; glaciated terrain checks |
| 5. Biomes | The pass classifier; the new `CellClimate`; soil fields; all consumers, map colours, covers, tree lists | Köppen, dryland, wetland and desert-sand shares inside their ranges |
| 6. Runtime | Date from the world tick; the weather function; snow and ice overlays; the first-person client's sun and sky follow the date and latitude | `just determinism-wasm`; a temporary seam and parked-region check; a year's printed summary for five places |

Phase 6's first slice depends only on the SHAPE of the monthly normals, so it may start after Phase 1 on
a simple seasonal curve. Stored-format changes are batched per phase so worlds are re-forged once per
phase, not once per commit.

## Phase 1 in more detail (the first spec and plan)

- **Dimensions.** One `size_m` becomes `width_m` and `height_m` through `ForgeParams`, `World`, every
  raster, the artifact, the tiles, `WorldMap` / `MapExtent`, the world-map window and the menu.
- **Wrap.** The existing wrap plan's design holds (canonical region coordinates everywhere, periodic noise
  and geometry in the forge, the client's wrap frame), with `width` and `height` in place of one size.
  Its size snap applies to the width.
- **Edges.** `EdgeKind::Wrap` (appended) on east and west; north and south fixed by the layout to sea.
  Ice on those edges comes from the climate, as today.
- **Latitude.** `pole = 3` with the full 90°–0°–90° span over `height_m`.
- **Artifact.** Stores the knobs, pole and lapse it was forged with (the runtime stops assuming defaults);
  raster lists stop being fixed arrays of five and five; a coarse raster family exists with its own cell
  size. Version constants are bumped once for all of it.
- **What it looks like.** The Planet on the CURRENT plate belts and annual climate: today's continents on
  a wider, wrapped, two-pole map. That is the gate, not the goal.

Scope calls for Phase 1 (Claude's, from the builder check):

- **One process only.** The wrap seam is built for `VOXEL_ROLE=all`. Under any other role the same world runs
  with the seam CLOSED (amended 2026-11-19: the server installs the map with `wrap_x = false`, so east and west
  hold like the old outer edges — a refusal would leave split-role worlds, the twin and orion with nothing they
  can run once the Planet is the only world; `VOXEL_HOSTS` no longer exists — hosts are by id since Elastic 1). The wrap plan predates R3
  and has no task for peer links, ownership, placement or observer sessions; host code still takes
  neighbours by plain arithmetic (`halo::neighbours`, `foreign_host`, `super_region_of`), and the R3
  go/no-go is still pending, so seam code between hosts is not touched now. The multi-host seam is its own
  later phase, after the R3 verdict.
- **Every forged world wraps.** There is no `wrap` knob (this replaces the wrap plan's D2 knob and its Task
  10 row). The engine types keep a `wrap_x` flag, because procedural test worlds and hand-built maps have
  no wrap; the forge always sets it. Until the old layouts are deleted they must keep forging
  bit-identically, which is the safety net for the refactor.
- **Planet's land in Phase 1** is today's "continents" formula with north and south forced to sea and made
  periodic in x. Phase 2 replaces it.
- **Sizes.** Phase 1 delivers the size table above in the menu, `worldctl forge` and `forge-proto`, with
  the forge-time and memory estimate taking width and height. The wrap plan's "no re-forge,
  `ARTIFACT_VERSION` stays 7" is superseded: versions are bumped once.
- **Measurement, not a limit.** `just forge-bench` takes width, height and cell; Phase 1 ends by timing
  every size and recording time, peak memory and file size under "Measurements" below.
- **Order inside 1a.** Width × height and the Planet layout land first, with the old layouts still
  present; the old layouts, presets and knobs are deleted and the gate re-recorded as the LAST step of 1a,
  once a Planet world forges and plays, so there is never a commit with no working world.
- **Phase 1 owns the SHAPE of the monthly climate**, as its second half (1b) so it can be planned and
  landed separately from the frame (1a): the coarse family carries twelve months × (sea-level temperature,
  diurnal range, precipitation, wet-day fraction), through the artifact, the tiled store (`ForgedMeta` /
  `WorldGlobals`) and the exact-arithmetic sampler, filled by a PLACEHOLDER curve from today's annual
  climate (annual mean, plus an amplitude by latitude and distance from the sea). Phase 3 replaces the
  values, not the format.
- **Knobs in the artifact** must also reach `ForgedMeta`, so store-only and Postgres worlds have them
  (today only `world.json` does; the runtime falls back to `Knobs::default()` in `forged.rs`).

Phase 0: Dawid asked Claude to run the timing (2026-11-14). It does not block Phase 1.

## Measurements

Every phase adds a row per world size here: forge wall time, peak memory, world file size, on which
machine. No limit is set on any of them yet.

### Phase 0 (today's forge, Continent layout, square worlds at the size table's cell counts)

Measured 2026-11-14 with `just forge-bench <size> <cell>` (release, pinned to 16 of 20 logical cores by the
justfile's `cap`, i7-12700H laptop, 31 GB RAM; wall time includes writing the PNGs and the world file). One
run each, on a machine in use, so read them as ±20%. (Note 2026-11-20: the recipe has timed the forge binary
OUTSIDE `cap` since `5cef874a`, 2026-11-01 — only its build is pinned — and that line was unchanged from then until
2026-11-19 (when only its arguments changed), so these runs most likely used all 20 logical cores; not recorded at the time.)

| Square world | Cells | Stands for | Wall time | Peak memory | World file |
|---|---|---|---|---|---|
| 35,456 m at 16 m | 4.9 M | Small at 16 m, Medium at 32 m | 47 s | 0.96 GB | 7.5 MB |
| 70,976 m at 16 m | 19.7 M | Medium at 16 m | 254 s (4 min 14 s) | 3.5 GB | 26.6 MB |
| 141,952 m at 32 m | 19.7 M | Large at 32 m | 238 s (3 min 58 s) | 3.6 GB | 41.6 MB |
| 100,352 m at 16 m | 39.3 M | today's menu default | 638 s (10 min 38 s) | 7.0 GB | 50.2 MB |

Large at 16 m (78.7 M cells) was not run: about 14 GB by the memory model, with 17 GB free at the time.

What they say: memory follows the forge's model (100 MB + 180 B per cell) closely. Time does not: the model
(`9.0 s × Mcells^1.05`) predicts 205 s at 19.7 M cells and 425 s at 39.3 M; measured is 254 s and 638 s,
an exponent near 1.3 between the two largest runs. The estimate shown in the menu is therefore low for big
worlds; Task 19 of the Phase 1a plan refits it.

### Phase 1a (the Planet: 2:1, wraps east–west, every menu size)

Measured 2026-11-20 with `just forge-bench <width> <height> <cell>` (release; the recipe's `cap` pins only its
build, so the forge ran on all 20 logical cores — affinity `0-19`, nice 5, checked during Large at 16 m;
i7-12700H laptop, 31 GB RAM, 21 GB available before and after; wall time includes
writing the PNGs and the world file). One run each, one after another, nothing else heavy running.
`scaling_cur_freq` of cpu0: 660,570 kHz before the series, 618,161 kHz after it (466,547 / 789,947 kHz around
Large at 16 m) — idle-core snapshots between runs, no sign of clamping, not a reading under load.

| Size | Cell | Cells | Wall time | Peak memory | World file |
|---|---|---|---|---|---|
| Dev 16,384 × 8,192 | 32 m | 0.13 M | 1.04 s | 0.08 GB | 0.4 MB |
| Tiny 25,600 × 12,800 | 32 m | 0.32 M | 2.31 s | 0.15 GB | 0.8 MB |
| Tiny | 16 m | 1.28 M | 9.99 s | 0.33 GB | 2.0 MB |
| Small 50,176 × 25,088 | 32 m | 1.23 M | 9.31 s | 0.31 GB | 2.5 MB |
| Small | 16 m | 4.9 M | 39.3 s | 0.93 GB | 5.8 MB |
| Medium 100,352 × 50,176 | 32 m | 4.9 M | 38.5 s | 0.94 GB | 8.0 MB |
| Medium | 16 m | 19.7 M | 160.9 s (2 min 41 s) | 3.38 GB | 18.8 MB |
| Large 200,704 × 100,352 | 32 m | 19.7 M | 164.5 s (2 min 44 s) | 3.40 GB | 27.2 MB |
| Large | 16 m | 78.7 M | 655.3 s (10 min 55 s) | 13.5 GB | 64.5 MB |

(Peak memory is `/usr/bin/time`'s max RSS, KiB × 1,024; MB and GB are 10⁶ and 10⁹ bytes.)

What they say. Against Phase 0 the comparison is INDICATIVE only: a different layout (a square Continent
world then, the 2:1 Planet now), different forge code (Phase 1a rewrote the grid, the noise and the climate
lengths), one run each on a machine in use (±20 %), and the core pinning of Phase 0 was not recorded (most likely
all 20 cores, as now: see the note on its line). At equal cell counts: 4.9 M cells 39 s now against 47 s then
(−17 %, inside the ±20 %); 19.7 M cells 161–164 s against 238–254 s (about −35 %, more than the noise, but which
of the differences above causes it is not known); memory the same within 6 % (0.93 against 0.96 GB,
3.4 against 3.5–3.6 GB); world files of the same order (a 2:1 world at 32 m keeps more tiles per cell than one at
16 m: 8.0 against 5.8 MB at 4.9 M). Within Phase 1a, time grows almost linearly with cells: a least-squares power
law over the nine rows gives 7.75 s × Mcells^1.017 (worst row 5.7 % off), against the old model's 1.05 and Phase
0's measured exponent near 1.3 between its two largest runs. Large at 16 m, not run in Phase 0, takes 11 minutes
and 13.5 GB. The forge's estimate was refitted in Task 19 (`forge::play`) with the fit rounded to `7.7 s ×
Mcells^1.02` (worst 6.8 %) and `70 MB + 172 B per cell` (worst 16 %, the small worlds; under 3 % from 4.9 M cells
up). With the Play forge's 12 GiB cap the menu still offers every size: Tiny and Small at 16 / 32 / 8 m, Medium
at 16 / 32 m, Large at 32 m only (Large at 16 m is estimated at 13.6 GB).

## Left to the builder

- All internal names, module layout, and how width/height thread through each API.
- The climate cell size (about 256 m) and planet grid size (512 × 256 to 1,024 × 512).
- Every tuning constant. All thresholds from the research are starting values; where the report marks one
  recalled or inferred it must be checked against its source before it becomes a named constant (the IUCN
  typology first, for the biome rules).
- How many phases' format changes can share one version bump.

## Open questions for later phases (not blocking Phase 1)

- What happens to crust at the polar edges of the plate simulation (no source covers it; the research
  proposes destroying it there). Needs a prototype in Phase 2.
- Ocean currents by rule or by a stream-function solve (Phase 3).
- The day length (the owner's decision; Phase 6 builds in calendar days and does not make it).
- The water engine's two open decisions in `TODO.md`, which decide whether stored seasonal water can ever
  be played.
