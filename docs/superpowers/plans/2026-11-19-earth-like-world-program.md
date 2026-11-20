# Earth-like world overhaul — the program (run it with `/goal`)

This is the one document that drives the whole overhaul from where it was paused to the end: for each phase
it says write (or re-base) the implementation plan, implement it, review it, measure it, record it, go on.
It is written to be handed to an autonomous run.

**To run it:**

```
/goal every phase in the Status table of docs/superpowers/plans/2026-11-19-earth-like-world-program.md is DONE or DEFERRED, each with its evidence recorded there
```

A session picking this up reads, in this order: this file; the design
`docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md`; the research report
`docs/research/reports/Earth like world generation overhaul.md` (the notes behind it are in
`docs/research/research_notes/Earth like world generation overhaul/`); `CLAUDE.md`. Then it starts at the
first row of the Status table that is not DONE or DEFERRED.

## Work to date

- **Research (2026-11-14).** Seven threads — plate tectonics, mountain shaping and glaciers, climate and
  seasons, water features, biome placement, runtime weather in games, an audit of the forge — and one report.
  Its evidence ledger says which claims were read from sources and which are untested designs.
- **Design (approved by Dawid 2026-11-14).** A "pocket planet": a coarse planet grid for plates, climate and
  coasts, the existing fine grid as a true-scale landscape. Decided by Dawid: 2:1 map; every world wraps
  east–west; the old layouts, presets and options are deleted, not kept; four world sizes; forge time is not
  capped, only measured; single-process wrap first.
- **Baseline timing (2026-11-14, the old square forge).** 4.9 M cells 47 s / 0.96 GB; 19.7 M cells about
  4 min / 3.5 GB; 39.3 M cells 10 min 38 s / 7.0 GB. Time grows faster than the menu's estimate assumes.
- **Phase 1a plan** written: `docs/superpowers/plans/2026-11-14-planet-frame-1a.md`, 17 tasks.
- **Implemented, on `develop`:**
  - Task 1, `8d6b29a2`: the forge grid is width × height (`ForgeParams { width_m, height_m, … }`,
    `World { nx, nz, … }`). Reviewed, clean.
  - Task 2, `77f8e8a3`: the world file, the tiled store, the runtime sampler and `world.json` carry
    width × height; the world file stores the forge's knobs; `EdgeKind::Wrap` is defined; formats bumped once
    (artifact 8, tiles 2, world map 2, meta 2). **Never reviewed** — the run was paused before its review.
- **Paused 2026-11-15** for R4 / R5 and the elastic-hosts work. Ledger of that run:
  `.superpowers/sdd/2026-11-14-planet-frame-1a/progress.md`.

## What changed while it was paused (the old plan is stale in these ways)

The Phase 1a plan and the design were written against the tree and the rules of 2026-11-14. Since then:

1. **Only regression tests and gates are committed** (CLAUDE.md, Testing, 2026-11-19). The 1a plan commits
   dozens of new tests: none of them may be committed now. Temporary tests drive the work and are deleted
   before the commit; `just test-audit` (first step of `just smoke`) refuses anything else.
2. **The island pin is gone.** `island_layout_unchanged` was deleted in the test clean-up (`5eb27852`), and
   the tests Tasks 1–2 added went with it. The only committed worldgen gate is `forge_quality_matches_baseline`
   (`crates/worldgen/tests/forge_lib.rs`, fixture `tests/fixtures/forge_quality_16k_32m.ron`, pinned to the
   one-island layout). The 1a plan's "bit-identical until Task 15" safety net therefore rests on that one gate
   plus temporary checks.
3. **No whole suites per task** (owner, 2026-11-19): a task verifies with the tests it touched, the filtered
   modules of the files it touched, and `just smoke`; each crate's whole suite runs once, at a plan's closing
   task. `just test-threads` no longer exists; `just test` / `just test-full` are the wide runs.
4. **Hosts are by id, land is assigned, the coordinator has replicas** (Elastic 1–3). `VOXEL_HOSTS`,
   `VOXEL_HOST_ORDINAL`, `check_several_hosts` are gone; `world_manager` grew `land.rs`, `succession.rs`; the
   link protocol is 10. The plan's Task 14 ("refused under split roles or several hosts") must be restated as
   "a wrapping world runs only under `VOXEL_ROLE=all`", checked in `roles::check_before_registration`.
   **Ruled otherwise in Phase 0 (2026-11-19):** once the Planet is the only world a refusal would leave `dev-r3`,
   the twin and orion with no world to run until Phase 7 (which may be deferred), so under any other role the
   seam is CLOSED instead (the map is installed with `wrap_x = false`; re-based plan, Task 16).
5. **Embark and the fortress are gone; the game is one first-person character** (2026-11-16/17). The PLAYER's
   embark is gone; history's own settler parties still use `embark_target` (`history/chain.rs` — corrected
   2026-11-19 by Phase 0's code check, so the re-based Task 9 keeps and wraps it); the design's "fortress view"
   reasoning about the day length no longer holds (see Phase 6).
6. **Other sessions commit and stage on `develop` concurrently.** Commit only your own paths (memory notes
   `commit-only-own-paths`, `partial-staging-beside-owner-hunks`).
7. **Worldgen itself moved**: trees are whole blocks, town kits and layouts landed, block transitions and
   oriented blocks exist. Line numbers and some symbols in the 1a plan are stale.

## Standing rules for the whole run

- Work on `develop` in this checkout. No worktrees, no stash, no reset, no amend, no push.
- One plan at a time, executed with `superpowers:subagent-driven-development`: a fresh implementer per task,
  an independent task review, one whole-plan review at the end. Each plan keeps its ledger under
  `.superpowers/sdd/<plan>/progress.md`; after a compaction trust the ledger and `git log`.
- Plans are written with `superpowers:writing-plans`, from a code check made THAT day (symbols, not line
  numbers), and saved to `docs/superpowers/plans/YYYY-MM-DD-<name>.md`. The design is the authority; where
  this file and the design disagree, this file wins (it is newer) and the design is amended in the same
  commit.
- **Tests.** No test is committed except a `/// Regression:` for a bug actually seen, or a change to an
  existing `/// Gate:`. A plan's "tests" are therefore of two kinds: temporary tests (written, run, deleted
  before the commit — the implementer's report quotes their output as evidence) and **evidence commands**
  (below). A plan step must say which it is.
- **Evidence commands, not tests.** What the design calls a phase's gate is produced by a tool run whose
  output is pasted into this file's Evidence section: `just forge-bench`, `just world-map`, and a
  `forge_proto --report` mode (added in Phase 1a) that prints the validation numbers of a forged world
  (land share, continents, belts by age class, Köppen shares, lake and river-mouth counts, biome shares —
  each phase adds its lines). The run is judged against the ranges in the phase's row; a miss is a tuning
  task inside the phase, not a reason to stop.
- **The quality gate.** `forge_quality_matches_baseline` stays the one worldgen gate. It is re-recorded on
  purpose, in a commit of its own that states the old and new numbers, exactly at: the end of Phase 1a (onto
  a development-size Planet world), and the end of every later phase that changes forged terrain on purpose
  (2, 3, 4). Starting the goal is the owner's approval of those re-records and of nothing else: **no NEW
  gate is added** by this program.
- **Worlds are wiped, never migrated.** Any stored-format change says "re-forge" (`just run --reset`,
  `just pg-reset`). Format bumps are batched: at most one bump of each version constant per phase.
- **Verification per task:** the touched tests (temporary or existing), `just test-one <crate> "<filter>"`
  for the modules of the files touched, `just smoke`; `just determinism-wasm` when sim math or anything
  hashed or serialized changed. Whole suites once, at each plan's closing task (`just test`), in the
  foreground.
- **Never launched by agents:** the game window, paid asset generation, anything on orion or the twin,
  benchmarks other than `just forge-bench`. What needs eyes goes to "Owner hand checks" below and does not
  block.
- **Measurements, no limits.** Every phase ends by timing every world size it can (`just forge-bench`) and
  recording wall time, peak memory and file size in the design's Measurements section. No pass line.
- Keep `justfile`, `CLAUDE.md` (Worldgen paragraph and anything a phase makes untrue) and `TODO.md` current in
  the commit that changes the fact.
- Rulings, not stalls: a conflict or a plan defect is decided, written in the ledger as `Ruling: … — why —
  cost if wrong`, and the run goes on. The four things that stop it are in "When to stop".

## The loop (once per phase)

1. **Re-read** the phase's row and notes here, the design's section for it, and the research note(s) named.
2. **Check the code** the phase touches as it is today (a read-only subagent; symbols and current signatures).
3. **Write the plan** (`superpowers:writing-plans`); for Phase 1a re-base the existing plan instead. Run the
   plan past a fresh subagent as a builder ("what would you still have to guess?") and settle what it finds.
4. **Execute** it (`superpowers:subagent-driven-development`), task by task, each reviewed.
5. **Whole-plan review** on the most capable model, one fix wave, a scoped re-review of the wave.
6. **Evidence:** run the phase's evidence commands, paste the numbers under Evidence, tune until they are in
   range or record a ruling on why not. Re-record the quality gate if the phase is one of 1a, 2, 3, 4.
7. **Record:** set the row to DONE with the plan path and the commit range; add the measurements to the
   design; add "As built" to the plan (commits, rulings, deferred minors); update `TODO.md`; list anything
   for the owner's eyes under "Owner hand checks".
8. Go to the next row.

## Status

| # | Phase | Status | Plan | Evidence |
|---|---|---|---|---|
| 0 | Resume: review Task 2; re-base the 1a plan; amend the design | DONE 2026-11-19 | `2026-11-14-planet-frame-1a.md` (re-based, Tasks 3–19) | Evidence § Phase 0 |
| 1a | Planet frame: 2:1 sizes, east–west wrap (role `all`; closed seam elsewhere), polar seas, one layout, old options deleted | DONE 2026-11-20 | `2026-11-14-planet-frame-1a.md` (19 tasks; "As built"); commits `804d85c0` … `4176aacd` (Tasks 1–2: `8d6b29a2`, `77f8e8a3`) | Evidence § Phase 1a |
| 1b | The stored monthly-climate family, filled by a placeholder seasonal curve | PLANNED 2026-11-20 (builder check settled) | `2026-11-20-monthly-climate-container-1b.md` | — |
| 2 | Plates and aged relief | PLANNED 2026-11-20 (builder check settled; re-check Phase 1b's symbols at dispatch) | `2026-11-20-plates-and-aged-relief-2.md` | — |
| 3 | Monthly climate | TODO | — | — |
| 4 | Water and ice | TODO | — | — |
| 5 | Biomes | TODO | — | — |
| 6 | Runtime seasons and weather | TODO | — | — |
| 7 | The wrap across hosts (`claim` worlds) | TODO | — | — |

DEFERRED is allowed only where a phase's notes say so, with the reason written in the row.

## The phases

### Phase 0 — resume

- Review `77f8e8a3` (Task 2) as a task review against its brief
  (`.superpowers/sdd/2026-11-14-planet-frame-1a/task-2-brief.md`), with one fix round if needed. Its tests
  were since deleted by the clean-up; judge the code.
- Re-base the 1a plan in place (same file, a dated "Re-based 2026-11-…" note at the top): every test step
  becomes a temporary test or an evidence command; the safety net is restated (item 2 above: before touching
  the forge further, record by a temporary check that the six old layouts forge to the same height-raster
  crc before and after each of Tasks 3–14 at one small size — the crcs live in the ledger, not in a test);
  Task 14 restated for role `all`; Task 8 without `embark_target`; Task 6's file list re-derived from today's
  `world_manager/`; `just test-threads` removed; whole suites only in the closing task; the `--report` mode
  added to Task 3.
- Amend the design: Constraints (the test rule), Phase 6 (first-person game), the 1c row (hosts by id), the
  status line.
- Done when: the review is clean or its findings fixed, the plan has no step that commits a test, the design
  is amended, `just smoke` passes.

### Phase 1a — the planet frame

The re-based plan. In short: world sizes and command lines; the world map of a rectangle; the wrap in the
simulation, LOD, history, the forge grid, noise and geometry, the runtime sampler and tiles, the client; a
wrapping world refused outside role `all`; then the old layouts, presets and their knobs deleted; climate
lengths in degrees of latitude; every size measured.

- Sizes (all 2:1, both sides multiples of 512 m): Tiny 25,600 × 12,800 (ruled 2026-11-19 in Task 3: the 25,088 × 12,544 first written here is not a multiple of 512 m in height); Small 50,176 × 25,088; Medium
  100,352 × 50,176 (default); Large 200,704 × 100,352; a development size 16,384 × 8,192. The forge cell
  (16 m or 32 m) stays a separate choice; no size is hidden for being slow.
- Evidence: `forge_proto --report` on a development-size Planet for 8 seeds — rows 0 and `nz−1` all sea;
  sea-level temperature below −15 °C at both polar rows and above 20 °C at the middle row; land share
  0.15–0.6; the height step across the seam (99th percentile) at most 1.5 × the interior's; aliased regions
  (`x` and `x + width`) generate identical columns. `just world-map` PNG path recorded. Every size timed.
- The quality gate is re-recorded onto the development-size Planet at the end.

### Phase 1b — the monthly-climate container

- Per CLIMATE cell (about 256 m): twelve months × (sea-level temperature, diurnal range, precipitation,
  wet-day fraction), bytes or `u16`, through the world file, the tiled store and the exact-arithmetic
  sampler; the lapse is applied from the fine height at read time; the fine annual precipitation raster stays
  as the rain-shadow modulator.
- Filled by a placeholder: today's annual mean plus an amplitude by latitude and distance from the sea, a
  one-month lag on land. Phase 3 replaces the values, not the format.
- Sampler API in `worldgen` that Phases 5 and 6 read: monthly normals at a world position, and the derived
  warmest / coldest month and driest-month values. The container type is `game::MonthlyClimate` (data and
  map-metre sampler in `game`, for Phase 6's weather); the world-position API is on `ForgedWorld`.
- Evidence: the report prints, per latitude band, January and July sea-level temperature (opposite signs of
  the seasonal swing in the two hemispheres, near zero at the equator); world-file size per world size.
- Research: `climate_simulation.md` (representation), the audit's storage section.

### Phase 2 — plates and aged relief

- A plate simulation on the planet grid (512 × 256 to 1,024 × 512, wrapping in x), run once and resampled
  per erosion level: rigid plates as rasters with a transform; subduction uplift by distance to the front;
  continental collision as a suture with an uplift pulse; rifting; new ocean crust with depth from age;
  hotspot chains. It starts from one supercontinent plus microcontinents. Per cell it records crust kind, age
  and thickness, orogeny age and kind, fold direction. Sea level is then solved for a target land fraction.
- It replaces the "continents" land formula, the static Voronoi plates and the interior ranges, and feeds
  volcano sites. `plate_count` and its static-plate knobs, `continent_count`, `continent_sea_width` are
  deleted with it.
- Age enters erosion as INPUTS (plain stream power flattens a dead range in 1–10 million years): initial
  height from crust thickness; present uplift from the belt's kind and age; an isostatic rebound term (about
  0.8 of each eroded metre, low-pass filtered); an incision threshold; erodibility and hillslope diffusion
  by orogeny age. Geology varies in space (rock column by crust and belt kind), replacing the one global
  column. `fit_height_band`'s single scale factor is revisited so young and old ranges keep their contrast.
- Polar edges of the plate grid: no source covers them. Start with the research's proposal (crust leaving the
  north or south edge is destroyed, gaps fill with ocean crust, uplift fades in a margin) and judge by maps.
- Finished worlds are scored and failed seeds re-run with a derived seed (the forge already has a seed
  score): at least 2 continents above 3% of the map each; at least one belt per continent; both a young and an
  old belt class present on the map.
- Evidence (8 seeds, development size, plus Medium for 2): continents per world; belts per continent by age
  class; mean slope and local relief of young versus old belts (young clearly higher on both); land share;
  seam continuity as in 1a; timing of the plate stage alone. Re-record the quality gate.
- Research: `plate_tectonics.md`, `mountains_erosion_glaciers.md` (sections on age). Read Tzathas et al. 2024
  by hand first if it can be fetched ("terrain at a given time").

### Phase 3 — monthly climate

- A twelve-month model on the planet grid: closed-form insolation; a seasonal energy-balance temperature with
  shallow land and deep ocean heat capacity, diffusion and wind advection, ice-albedo ramp, global mean
  solved to 15 °C; monthly pressure from a seasonal thermal equator plus land–sea anomalies; wind from the
  pressure gradient; ocean-current temperature anomalies by basin side and latitude; steady moisture
  advection with convergence, subtropical suppression, frontal and orographic rain (the orographic term on
  the fine grid). Derived per cell: snow months, sea-ice months, growing degree days, potential
  evapotranspiration (Thornthwaite), aridity index, a monthly soil-water bucket and runoff.
- It fills Phase 1b's container — including its stored monthly wind (east and south components), which it
  FILLS rather than adds — averaging its planet grid onto the container's grid, and deletes the annual-climate knobs
  (wind band edges, latitude temperatures, meander, relaxation lengths).
- Evidence: Köppen class shares of land by the Peel 2007 rules, unweighted, against ranges around Earth's
  (arid 20–40%, cold 10–35%, tropical 10–30%, temperate 8–22%, polar 6–20%); three synthetic worlds (all
  ocean; one pole-to-pole rectangular continent; one meridional mountain wall whose rain shadow flips between
  the trade-wind and westerly belts); a west-coast check (some Mediterranean climate between 30° and 45° on a
  western coast in most seeds). Re-record the quality gate.
- Research: `climate_simulation.md`. Known failure to test against: a model that loses continental interiors
  to cold desert. Ocean currents are the least-evidenced part: expect iteration.

### Phase 4 — water and ice

- The sea becomes a RESULT: erode against a sea about 120 m lower, then flood to zero (rias, estuaries,
  fjords, shelf islands); the fixed ocean mask and the land clamp go. Deep bathymetry stays data on the
  planet grid; the playable sea floor stays clamped.
- Lakes by water balance (equilibrium area = inflow ÷ (evaporation − precipitation) on the basin's
  hypsometry; below the spill point the lake is closed and salt, or a playa), each tagged with its origin;
  the carved-lake knobs are deleted.
- Rivers keep their network and gain: twelve monthly discharges per reach (snow storage and melt, loss in
  deserts), a regime class, hydraulic-geometry width and depth, braided versus single-thread, knickpoints as
  waterfalls. Sediment deposition in the stream-power solver (fans, floodplains, basin fill).
- Deltas as a second layer: river objects become a graph in which a node can have two downstream edges;
  branch mouths traced back to an apex; each river mouth classed delta, estuary, ria or fjord.
- Ice: ice sheets on the planet grid from the monthly mass balance; valley glaciers by glacial stream-power
  routing down the drainage tree on a coarser level; one cold stage with a lower equilibrium line and the low
  sea, a stepped retreat; moraines, kettles and the like stamped from the simulated ice.
- Coasts classed per segment (tectonic setting from Phase 2, wave exposure from Phase 3's winds, tidal class,
  sediment, winter sea temperature) and stamped: barrier islands and lagoons, reefs and atolls, sandy, rocky
  and muddy shores. A water-table field replaces `drainage`. Bays, capes, straits, peninsulas detected on the
  mask and named; a naming subsystem for map features.
- Stored but not played: seasonal river levels, tides, floating ice (the water engine is not part of this
  program).
- Evidence: lakes per 1,000 km² in arid versus humid cells (humid clearly more; closed salt lakes only where
  the aridity index is below 0.5); every river mouth has a class; share of coast with barrier islands
  (target 5–20%); fjords only poleward of the cold stage's ice limit; named features per world. Re-record the
  quality gate.
- Research: `water_features.md` (much of its catalogue is the researcher's own design — prototype before
  trusting), `mountains_erosion_glaciers.md` (glaciers).

### Phase 5 — biomes

- The classifier in passes: sea cells; hard land states (ice, lava, lakes, rivers); the shore strip; climate
  vegetation from plant limits (coldest month, growing degree days, moisture index, dry-season length,
  treeline by growing-season temperature — "above 1,200 m is Mountain" goes); local overrides (desert
  surface, the wetland ladder, oases); a persistent fire state in the savanna–forest band; underground
  properties; a land-use overlay from history over the natural biome.
- The 72 as four layers: about fifty surface cell biomes appended to the `Biome` byte (append only, never
  renumber); objects and point features; per-column underground properties; the land-use overlay. Each cell
  stores biome, a second choice and a weight. Köppen is computed as the label.
- Every consumer updated (ground cover, tree lists, map colours, history capacity, the seed score). First
  pass reuses existing blocks: a rule, a map colour, a cover and a tree list per biome; new art is not part
  of this program.
- Every threshold from the research marked recalled or inferred is checked against its source before it
  becomes a named constant; the IUCN Global Ecosystem Typology profiles first.
- Evidence: biome shares of land; drylands (aridity index below 0.65) 35–55% of land; sand at most 30% of
  desert; forest absent above the treeline; mangroves only on warm sheltered coasts; every biome of the cell
  layer present in at least one of 8 Medium seeds, or a ruling on why not.
- Research: `biome_placement.md`.

### Phase 6 — runtime seasons and weather

- The sim derives the date from the world tick. Weather is a PURE function of (world tick, global position,
  stored climate) in `game`, the same on server and client: never streamed, never hashed, nothing in a cut,
  no message between regions or hosts. Integer or `libm` arithmetic, hash rolls only.
- Temperature: interpolate the monthly normals, add a diurnal table, a slow anomaly field, the lapse.
  Precipitation: one space–time noise field mapped to a uniform value against the cell's monthly wet-day
  probability, the same value giving the amount; systems drift with the stored seasonal wind; storms as
  hashed objects with closed-form tracks; a per-year anomaly so years differ.
- Snow depth and ice thickness as derived column overlays (in the manner of `water_top`), from a bounded
  replay or a stored normal curve; player edits as sparse deltas with an expiry. The client's sun and sky
  follow the date and latitude; season names are local to the hemisphere.
- **The day length is the owner's decision** and is NOT made here: the game is first-person now, and a
  10-second day is a strobe in first person. The program builds everything in calendar days and reads the
  day length from the one existing constant, so changing it later rescales the weather.
- May start as soon as Phase 1b is DONE (it needs only the container's shape); if started early it is still
  finished after Phase 3.
- Evidence: `just determinism-wasm`; a temporary check that two regions on either side of a seam, and a
  region woken after 1,000 simulated days, compute the same weather, snow and ice as a region that ran
  throughout; a year's printed summary for five places (a pole, a mid-latitude interior, a west coast, a
  desert, the equator).
- Research: `runtime_seasons_weather.md`.

### Phase 7 — the wrap across hosts

- The seam between hosts of a `claim` world: canonical regions in `halo::neighbours`, the peer links,
  `super_region_of` and land assignment across the seam, placement, observer sessions.
- **DEFERRED unless** `TODO.md` shows the Elastic 3 work accepted by the owner when the run reaches this row
  (it is "BUILT, NOT accepted" on 2026-11-19): touching seam code between hosts under an unaccepted elastic
  build would muddy both. If deferred, write the reason in the row and leave a plan stub naming what it
  must cover.

## Owner hand checks (collected by the run; none blocks it)

To be appended as phases finish. Already known:

- After Phase 1a (built 2026-11-20; evidence below):
  1. `just run --reset` (a new dev world). What an old world does: a world file and the menu's world list refuse
     any version-8 world (re-forge wording); a store holding version-8 tiles of an old layout is refused; a
     version-8 PLANET already stored as tiles still loads (ruled acceptable: its bytes are identical) but should be
     re-forged to get the tuned land and the climate in degrees.
  2. Open the world map: ice-pale polar seas top and bottom, the hot band across the middle, the left and right
     edges continuing into each other (as in `target/world-forge/phase1a-map.png-tiles-biomes.png`; a Medium
     world for comparison: `target/world-forge/phase1a-medium-map.png-tiles-biomes.png`).
  3. Walk east across the seam (x = the world's width): no cliff, no step in trees, rivers or biome, no hitch.
  4. Turn and pan the far LOD rings across the seam: no gap or doubled strip (a second image of the terrain past
     half the world's width is a known limit).
  5. Commit or drop the working-tree-only edits made in the OWNER's untracked or uncommitted files (never committed
     by the run): `crates/panel/src/forge.rs` (`PRESETS`), `crates/panel/src/web.rs` and
     `crates/panel/templates/world.html` (the size select) — `task-3-report.md` and `task-17-report.md`;
     `crates/client/src/renderer/look_target.rs` (lines 80, 94, 99, 100, checked in the working tree 2026-11-20) and
     `block_damage.rs` (`block_centre` and its callers) — `task-15-report.md`, "In the owner's hunks"; the one line in `crates/game/src/people.rs` `follow_path_safe`
     (`w.waypoint(…)` in the region's frame) — `task-7-report.md` (all under
     `.superpowers/sdd/2026-11-14-planet-frame-1a/`).
  6. Re-forge every world before running this build (artifact v9, world map v3; as in step 1, only a stored v8
     Planet would still load, without this phase's tuning): dev worlds (`just run --reset`, `worlds/`), the web
     world, dev Postgres worlds (`just pg-reset`), the twin and orion.
- After Phase 1b (built 2026-11-20; evidence below):
  1. Re-forge EVERY world before running a build from `50ebc1ed` on (artifact v10, tiled format v3): dev worlds
     (`just run --reset`, the menu's worlds under `worlds/`), the web world (move its store aside before `just
     deploy`), dev Postgres worlds (`just pg-reset`), the twin and orion (a new empty database, as their runbooks).
     What an old world does (Task 3, run unless marked): a v9 world file → "world file version 9 is older than 10:
     re-forge this world" (also from `just world-map` and `worldctl retile`); the menu's world list → "needs re-forge:
     world file version 9 is older than 10: re-forge this world"; a SQLite store of tiles v2 → "this world's forged
     tiles use format 2 but this build reads format 3; it keeps no archived forge file this build can re-tile from, so
     re-forge the world" (no re-tile, nothing forged over saved regions); a Postgres store → the same message from the
     same code path (argued, not run: the plan's Ruling 17).
  2. Look at the four month maps of the dev world `target/world-forge/phase1b.world` (1,024 × 512, pole to pole):
     `target/world-forge/phase1b-monthly-temperature-04.png` and `…-10.png` (month 4 = the northern midsummer: the
     north warm, the south cold; month 10 the reverse; the left and right edges continuous), and
     `target/world-forge/phase1b-monthly-rain-04.png` and `…-10.png`. The values the tool printed (middle column, at
     the ground): temperature month 4 at 75° N / 45° N / 0° / 45° S / 75° S −9.3 / 13.6 / 25.5 / −9.6 / −20.1 °C,
     month 10 −18.9 / 6.3 / 26.4 / 20.8 / −10.4 °C (the 30 °C swing at 45° S is an inland point, the placeholder's
     continental amplitude); rain month 4 39.7 / 117.6 / 553.2 / 42.7 / 13.3 mm, month 10 13.8 / 41.0 / 627.4 /
     122.4 / 38.0 mm. Values are a placeholder (Phase 3 replaces them).
  3. Memory of a running world: every process that loads the world (role `all`, each host and coordinator pod)
     holds the family resident, outside the tile cache's budget — 3.0 MiB at Dev, 1.8 MiB at Tiny and Small, 7.0 MiB
     at Medium, 28.1 MiB at Large (and transiently about 2–4 × that while loading or writing the tiled world). Check
     it against the pods' memory requests (`deploy/modules/world`) before a Large world runs there.
- After Phases 2–5: `just world-map` on a Medium world — do the continents, ranges, climates and coasts read
  as a small Earth?
- Before or during Phase 6: decide the day length.
- Every dev world is wiped by Phases 1a and later (`just run --reset`, `just pg-reset`); the twin and orion
  need a re-forge before they run a build from this program.

## When to stop (and only then)

- A step would be irreversible or reach outside this checkout (a push, anything on orion or the twin, paid
  generation).
- A phase's plan is so broken by the state of the tree that every way forward is a guess (say exactly what
  was found).
- Another session's uncommitted work sits in a file a task must rewrite wholesale, so the two cannot be
  staged apart.
- `just smoke` fails for a reason outside this program's files and stays failing (note it, work on tasks it
  does not gate, and stop only if nothing is left that it does not gate).

Everything else is a ruling in the ledger.

## Evidence

(Per phase: the commands run, the date, the numbers. Filled by the run.)

### Phase 0 — resume (2026-11-19)

- **Task 2 review** (`77f8e8a3`, range `77f8e8a3^..77f8e8a3`, against its brief; the tests it added were since
  deleted, the code was judged): spec compliant, no Critical or Important finding, no fix round. The one item the
  diff could not show — that the server refuses an old-format store instead of forging over it — was checked in
  the tree: `ensure_world` (`crates/server/src/world_file.rs`) returns the error for any `OtherFormat` that is not
  re-tileable. Five minors, each routed: stored knobs never validated and the menu's `WorldMeta` size and
  `size_text` rounding → re-based Task 3; `EdgeKind::Wrap` acting like `Land` in the sampler → Tasks 10 and 12;
  the old-`world.json` wording → the whole-plan review. Report:
  `.superpowers/sdd/2026-11-14-planet-frame-1a/task-2-review.md` (git-ignored workspace).
- **Re-base.** Code check of every symbol Tasks 3–17 named, at `c16f7e3d`
  (`…/rebase-code-check.md`), then the plan rewritten in place as Tasks 3–19; a fresh builder check
  (`…/builder-check.md`) found 6 blockers and 20 fixes, all settled in the text. No step commits a test: `grep -c
  "Temporary test"` = one per task that has behaviour to drive; the only committed test the plan touches is the
  existing gate (body rewritten in Task 17, ONE re-record in Task 18).
- **Rulings made in Phase 0** (also in the ledger):
  1. Outside `VOXEL_ROLE=all` the seam is CLOSED, not refused (Task 16) — after Task 17 every world wraps, and a
     refusal would leave `dev-r3`, the twin and orion with no world to run until Phase 7. Cost if wrong: a
     ten-line change back to a refusal.
  2. `embark_target` still exists (history's own settler parties): Task 9 keeps and wraps it. This file's item 5
     was a slip, corrected above.
  3. Two tasks added that the old plan lacked: 7 (halo keys, intents and global block positions across the seam —
     canonical keys plus one nearest-image local conversion) and 13 (the tree lattice at the seam).
  4. Formats: `WORLD_MAP_VERSION` 2 → 3 (Task 5; a stored map is rebuilt from the archive) and `ARTIFACT_VERSION`
     8 → 9 plus an edges check in `WorldGlobals::validate` (Task 17), because worlds were forged at Task 2's
     versions during the pause. Cost: one more re-forge of dev worlds, which Task 17 forces anyway.
  5. The six-layouts safety net is the quality gate plus per-layout `height_crc` lines of `forge_proto --report`,
     the crcs kept in the ledger.
  6. The gate has no fixture for the two consecutive commits between Task 17 (old layouts deleted) and Task 18
     (the phase's one re-record).
- **Design amended** (status, the test rule, the multi-host row, the closed seam, the day length, Phase 6's row).
- `just smoke`: `smoke ok in 2s`, exit 0 (2026-11-19, tree at `c16f7e3d` + this phase's documents).

### Phase 1a — the planet frame (2026-11-20)

Plan `docs/superpowers/plans/2026-11-14-planet-frame-1a.md` ("As built": commits per task, every ruling, the
deferred minors). Tree at `a6fb06f9` (Phase 1a's last code commit is `be8ab249`), plus Task 19's estimate refit,
which changes no forged byte.

- **`just forge-report --size dev --cell 32 --seeds 8`** (seeds `…0206`–`…020D`; `--cell 64` gives the same
  field verdicts, per-seed differences noted under the table):

  | line | 0206 | 0207 | 0208 | 0209 | 020A | 020B | 020C | 020D |
  |---|---|---|---|---|---|---|---|---|
  | polar_rows_all_sea | yes | yes | yes | yes | yes | yes | yes | yes |
  | sea_level_temp_c north / middle / south | −23.2 / 26.9 / −23.2 | −23.2 / 26.9 / −23.3 | −23.3 / 26.9 / −23.2 | −23.3 / 26.9 / −23.2 | −23.2 / 26.9 / −23.1 | −23.2 / 26.9 / −23.2 | −23.2 / 26.9 / −23.2 | −23.2 / 26.9 / −23.2 |
  | land_share | 0.310 | 0.352 | 0.258 | 0.321 | 0.294 | 0.304 | 0.317 | 0.372 |
  | land_components_over_1pct | 2 | 2 | 4 | 1 | 3 | 2 | 3 | 2 |
  | drains_to_sea | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
  | seam_step_ratio_height (rank, ok) | 0.740 (0.117, yes) | 0.295 (0.726, yes) | 0.059 (0.904, yes) | 0.931 (0.268, yes) | 0.035 (0.949, yes) | 0.040 (0.918, yes) | 0.213 (0.575, yes) | 1.751 (0.008, no) |
  | seam_step_ratio_precip (rank, ok) | 0.569 (0.327, yes) | 0.368 (0.569, yes) | 0.208 (0.750, yes) | 0.171 (0.843, yes) | 0.134 (0.941, yes) | 0.665 (0.286, yes) | 0.559 (0.427, yes) | 0.708 (0.339, yes) |
  | seam_step_ratio_biome (rank, ok) | 1.801 (0.139, yes) | 1.070 (0.442, yes) | 1.149 (0.378, yes) | 0.510 (0.785, yes) | 0.100 (0.949, yes) | 0.886 (0.552, yes) | 1.529 (0.209, yes) | 1.633 (0.131, yes) |
  | aliased_regions_identical | yes | yes | yes | yes | yes | yes | yes | yes |
  | seam_region_cliff | 0.500 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 | 0.500 | 1.000 |
  | world_map_wraps | yes | yes | yes | yes | yes | yes | yes | yes |
  | rivers_across_seam (max gap, cells) | 0 | 1 (0.031) | 0 | 0 | 0 | 0 | 0 | 2 (0.031) |
  | trees_across_seam_ok | yes | yes | yes | yes | yes | yes | yes | yes |

  Summary lines (32 m): `seam_seeds_not_ok_height: 1`, `seam_pass_height: yes`; `seam_seeds_not_ok_precip: 0`,
  `seam_pass_precip: yes`; `seam_seeds_not_ok_biome: 0`, `seam_pass_biome: yes`; median ranks 0.6505 / 0.498 /
  0.41; `lakes_across_seam: 0` on every seed. At 64 m (`just forge-report --size dev --cell 64 --seeds 8`):
  `seam_seeds_not_ok_height: 1` (020D: 1.756, rank 0.000), `seam_seeds_not_ok_precip: 1` (0207: 3.894, rank 0.008),
  `seam_seeds_not_ok_biome: 0`; median ranks 0.649 / 0.496 / 0.5235; `seam_pass_*` yes ×3; `lakes_across_seam: 0`
  on every seed; land share 0.258698–0.371918; temperatures north −23.1…−22.9, middle 26.8, south −23.1…−22.9;
  `polar_rows_all_sea: yes`, `drains_to_sea` 1 on every seed; `seam_region_cliff` 0.5–1 (020D 0.750);
  `rivers_across_seam_max_gap_cells` at most 0.156 (020D).
- **Judged against the Phase 1a ranges:** rows 0 and `nz−1` all sea — met (8/8 at 32 m and at 64 m). Polar sea-level
  temperature below −15 °C (32 m: −23.3…−23.1) and the middle row above 20 °C (26.9) — met (8/8). Land share
  0.15–0.6 — met (0.258–0.372). Aliased regions identical — met (8/8).
  **The seam.** The program's criterion — the HEIGHT step across the seam at most 1.5 × the interior's — is not met
  on every seed. Raw counts of seeds whose ratio is above 1.5:

  | field | 32 m | 64 m |
  |---|---|---|
  | height | 1 of 8: 020D 1.751 (rank 0.008) | 1 of 8: 020D 1.756 (rank 0.000) |
  | biome | 3 of 8: 0206 1.801 (rank 0.139), 020C 1.529 (0.209), 020D 1.633 (0.131) — each rescued by rank ≥ 0.05 | 1 of 8: 020D 1.532 (rank 0.141) — rescued by rank |
  | precipitation | 0 of 8 (max 0.708) | 1 of 8: 0207 3.894 (rank 0.008) |

  Under the ruled seed-set rule (a seed is `ok` when its ratio ≤ 1.5 or its rank ≥ 0.05; a field passes when at most
  2 of 8 seeds are `no` and the median rank ≥ 0.25): 32 m — height 1 `no`, median rank 0.6505, PASS; precipitation 0
  `no`, 0.498, PASS; biome 0 `no`, 0.41, PASS. 64 m — height 1 `no`, 0.649, PASS; precipitation 1 `no`, 0.496,
  PASS; biome 0 `no`, 0.5235, PASS. The height miss is the same seed (020D) at both cells, where 0.8 % (32 m) and none
  (64 m, rank 0.000) of the interior column pairs are as rough as its seam.
  Also (32 m): every land cell drains to the sea, the world map wraps, trees and rivers continue across the seam
  (river gaps ≤ 0.031 cell), the seam's region cliff is at most the interior's (0.5–1.0). Two land components over
  1 % on 7 of 8 seeds (`…0209` has one).
- **Map:** `just forge-proto --size dev --cell 32 --out target/world-forge/phase1a` (`phase1a.world`, 0.4 MiB, and
  its forge PNGs `target/world-forge/phase1a-{biome,temperature,rain,relief,…}.png`), then `just world-map --world
  target/world-forge/phase1a.world --tiles --out target/world-forge/phase1a-map.png` → `target/world-forge/
  phase1a-map.png-tiles-{biomes,relief,temperature,rain,regions}.png` (2048 × 1032, 256 × 129 tiles). The biome
  map shows pale polar sea bands top and bottom with glacier and tundra on the southern coasts, tropical forest and
  savanna across the middle and one large continent with a central mountain spine, its southern lobe leaving the
  right edge and coming back at the left; the temperature map is a smooth cold–hot–cold banding, warmest over the
  middle rows, whose left and right columns match. A Medium world for the owner: `just forge-proto --size medium
  --cell 32 --out target/world-forge/phase1a-medium` (7.7 MiB) and its map
  `target/world-forge/phase1a-medium-map.png-tiles-{biomes,relief,temperature,rain,regions}.png` (1568 × 785 tiles).
- **Every size timed** (`just forge-bench`, 2026-11-20): the design's Measurements, "Phase 1a" — Dev 32 m 1.04 s,
  Tiny 2.3 / 10.0 s, Small 9.3 / 39.3 s, Medium 38.5 / 160.9 s, Large 164.5 s at 32 m and 655.3 s / 13.5 GB at
  16 m. The menu's estimate refitted: `FORGE_SECS_PER_MCELL` 9.0 → 7.7, `FORGE_COST_EXPONENT` 1.05 → 1.02,
  `FORGE_RAM_BYTES_PER_CELL` 180 → 172, `FORGE_RAM_BASE_BYTES` 100 → 70 MB (worst row 6.8 % time, 16 % memory).
- **The quality gate** (`forge_quality_matches_baseline`, recorded onto the dev Planet 16,384 × 8,192 at 32 m; final
  recording `be8ab249`, which superseded `bd42b95b` inside Task 18):

  | field | old (16 km one-island) | `bd42b95b` | `be8ab249` (final) |
  |---|---|---|---|
  | land_share | 0.329968 | 0.309761 | 0.309761 |
  | height p50 / p90 / p99 m | 60.47 / 1083.76 / 2059.08 | 33.48 / 619.95 / 1495.92 | 33.11 / 628.40 / 1641.65 |
  | max_peak_m | 2326.68 | 1785.72 | 1992.11 |
  | concavity θ / Hack exponent | 0.6680 / 0.4697 | 0.3608 / 0.4358 | 0.2621 / 0.4343 |
  | river mouths / km / max Strahler | 30 / 49.32 / 3 | 15 / 15.63 / 3 | 14 / 15.84 / 2 |
  | lakes | 6 | 2 | 1 |
- **Rulings that changed how a criterion was judged** (all in the plan's "As built"): the seam — "ratio ≤ 1.5 or
  rank ≥ 0.05" per seed, then "at most 2 of 8 seeds `no` and median rank ≥ 0.25" per field (ordinary interior column
  pairs fail the per-seed rule 4.7 % of the time); 7 of 8 seeds with two continents accepted (the land formula is
  replaced in Phase 2); Tiny is 25,600 × 12,800 m (12,544 m is not whole 512 m regions); outside role `all` the seam
  is closed, not refused (Phase 7 opens it); the two rain blurs moved to degrees inside Task 18, so the gate was
  recorded twice within that task (`bd42b95b`, then `be8ab249`).
- **`just test`** (once, 2026-11-20, tree at `a6fb06f9` + this task's uncommitted documents and estimate):
  `PASS test-audit`, `FAIL workspace`, `PASS physics-next-upstream`, `PASS slotmapd`, `PASS quest`, `PASS site-test`.
  The workspace stage's one failure: `game --test wasm_determinism` `scene_hash_native` — `native scene digest
  0xE6CD22F6 != golden 0x38D86635`. Not Phase 1a's: the same value was recorded failing with and without Task 5
  (the first Phase 1a change to `game`) and unchanged through Tasks 12, 13 and 17, and the same test on the
  COMMITTED tree (`git archive HEAD` at `a6fb06f9`, its own target dir) passes, 7 of 7 — the digest moves with the
  owner's uncommitted `crates/game` work (physics goldens and sim files). Every other test passed; the slow
  `scene_hash_golden::` / `tree_fall::` are `#[ignore]`d in `just test` (not run). Workspace clippy is not a stage
  of `just test` (its known failures in `block-mesh`, `bbmodel`, `macros` are in crates Phase 1a did not touch).
- Status: the controller sets the Status row after the whole-plan review.

**The close (2026-11-20).** Whole-plan review by three area reviewers on the most capable model (sim and server;
client; worldgen and tools): no Critical finding; one Important (a `justfile` `forge-bench` example the forge
refused). One fix wave of twelve small items (commits `a058d0cf` … `4176aacd`), re-reviewed: all addressed, no new
breakage, no test added; the 8-seed dev report is identical before and after the wave and the gate passes
untouched. The committed tree alone — an export of `4176aacd` without the owner's uncommitted work — type-checks
(`cargo check -p game -p worldgen -p world-store -p server -p client --lib --bins --examples`: 0 errors). What the
review established for the phases that follow: the runtime path is exact arithmetic (no transcendental reaches
`forged.rs`, generation or trees); the quality gate guards land share, hypsometry and rivers ONLY — the seam, the
wrap, the polar seas, biomes, tiles and the sampler rest on `forge_proto --report`; under a closed seam the region
one past the edge can still be subscribed and is generated as a copy of the far edge (a Known limit, Phase 7's);
`SocietyGrid::world_id` was made independent of `wrap_x` so a world's history id does not depend on the role that
opens it. Rulings and accepted leftovers: the plan's "As built".

### Phase 1b — the monthly-climate container (2026-11-20)

Plan `docs/superpowers/plans/2026-11-20-monthly-climate-container-1b.md` ("As built": commits per task, the rulings
made during execution, the deferred minors). Code commits `99fc8643` … `c30da2a4`; every number below was taken at
`c30da2a4` (the later `8c1d0f2f` changes only a document). Raw outputs: the plan's workspace,
`task-6-report.md`.

- **`just forge-report --size dev --cell 32 --seeds 8`** (seeds `…0206`–`…020D`), its monthly lines over the eight
  seeds (min / median / max; "July" = month 3, "January" = month 9, sea level, °C):

  | band | `sea_temp_jan_c` | `sea_temp_jul_c` | `seasonal_swing_c` (Jul − Jan) |
  |---|---|---|---|
  | 75N | −17.42 / −16.74 / −16.58 | −7.06 / −6.905 / −6.23 | 9.52 / 9.835 / 11.19 |
  | 45N | 3.2 / 4.54 / 7.49 | 15.66 / 18.61 / 19.95 | 8.17 / 14.07 / 16.75 |
  | 15N | 22.31 / 22.78 / 23.52 | 26.46 / 27.2 / 27.67 | 2.95 / 4.425 / 5.36 |
  | 0 | 26.65 / 26.72 / 26.8 | 26.65 / 26.73 / 26.8 | −0.15 / 0.01 / 0.14 |
  | 15S | 26.54 / 27.24 / 27.77 | 22.22 / 22.74 / 23.44 | −5.55 / −4.5 / −3.1 |
  | 45S | 15.85 / 18.195 / 20.07 | 3.08 / 4.955 / 7.3 | −17 / −13.24 / −8.55 |
  | 75S | −7.07 / −7.02 / −6.7 | −16.94 / −16.63 / −16.58 | −10.24 / −9.61 / −9.52 |

  Also: `climate_grid: 256 × 128 cells of 64 m, stride 2`; `climate_clamped_values` 0 on every seed;
  `climate_roundtrip_max_err: temp 0.0050 C, diurnal 0.050 C, precip 0.050 mm, wet 0.00196, wind 0.125 m/s` (half a
  step on every field), `climate_roundtrip_ok`, `climate_tiles_identical` and `monthly_aliased_identical` yes on every
  seed; `monthly_precip_conserved_max_rel` 2.62e-7 … 2.87e-7; `monthly_temp_mean_err_c` ≤ 9.09e-7,
  `monthly_precip_sum_err_mm` ≤ 2.44e-4; `monthly_annual_temp_gap_c` p99 0.237 … 0.296 per seed, max 0.424 … 0.545;
  `world_file_bytes` 968,875 … 1,128,917 (median 1,035,448.5); `climate_bytes_packed` 551,121 … 646,876 (raw and
  resident 3,145,728). Seam of the monthly fields (Ruling 15: the Phase 1a rule on climate-grid columns, month 3):
  temperature ratio 0.675 … 1.148, rank 0.165 … 0.843, `yes` on 8 of 8, `seam_pass_monthly_temp: yes`; precipitation
  ratio 0.213 … 2.802, median rank 0.592, `no` on 1 of 8 — seed 0207, ratio 2.802, rank 0.016 —
  `seam_pass_monthly_precip: yes`. At 64 m (`--cell 64 --seeds 8`): swings 75N 9.52 … 11.14, 45N 8.09 … 16.64, 15N
  2.92 … 5.3, 0 −0.15 … 0.15, 15S −5.53 … −3.05, 45S −16.84 … −8.5, 75S −10.19 … −9.52; the same round-trip errors
  and `yes` lines; gap p99 0.050 on every seed (max ≤ 0.052); world files 707,009 … 820,869 B; temperature seam `yes`
  8 of 8; precipitation `no` on 1 of 8 — again 0207 (4.392, rank 0.000), `seam_pass_monthly_precip: yes`.
- **Judged against the Phase 1b row:** opposite signs of the seasonal swing in the two hemispheres — met at both
  cells on every seed (every northern band's minimum is positive, 2.92 or more; every southern band's maximum
  negative, −3.05 or less). Near zero at the equator — met: −0.15 … 0.15 °C at 32 and 64 m, within the plan's
  1.0 °C. World-file size per world size — recorded (the design's Measurements, "Phase 1b": 0.98 MB Dev to 29.8 MB
  Large at 32 m and 67.2 MB Large at 16 m; the family packs to 0.38–2.64 MB, 9–57 % of a 32 m file). The seam of the
  monthly precipitation has one `no` seed at each cell, 0207; at 64 m that seed's FINE precipitation seam is `no`
  too (3.894, rank 0.008) but at 32 m it is `yes` (0.368, rank 0.569), so the monthly miss at 32 m is not inherited
  from the fine raster (the climate grid is a coarser sampling of the same world); under the ruled seed-set rule
  (at most 2 of 8 `no`, median rank ≥ 0.25) both cells pass. The annual-vs-monthly temperature gap (Ruling 13: a p99
  above 0.3 °C is a finding): Dev 32 m p99 ≤ 0.296 on every seed, Dev 64 m 0.050, Small 32 m 0.148, Medium 32 m
  0.062, Large 32 m 0.051 — and **Tiny 32 m 0.569 (max 1.046), above the bar**; the ledger's ruling accepting the
  polar gap was made before Tiny was measured, so this one is open for the whole-plan review (the plan's "As built").
- **Nothing in this phase moved a forged or stored bit of the existing world.** Both 8-seed reports above are
  identical, line for line, to the ones taken after Task 4 (`diff` of `after4-{32,64}.txt` against today's, the
  recipe's echo lines stripped: no output, 837 lines each), so Tasks 5 and 6 changed no world; and every line of
  the Task 2 baselines `baseline2-{32,64}.txt` is present and unchanged (0 removed or changed lines), among them each
  seed's `height_crc` and `artifact_rasters_crc`. **The quality gate was NOT re-recorded** and passes untouched:
  `just test-one worldgen "--test it -- --include-ignored forge_lib::"` → `test
  forge_lib::forge_quality_matches_baseline ... ok`, `1 passed; 0 failed`.
- **Every size timed** (`just forge-bench`): the design's Measurements, "Phase 1b" — Dev 32 m 1.41 s, Tiny 2.52 /
  10.09 s, Small 9.36 / 39.32 s, Medium 39.45 / 162.00 s, Large 169.78 s at 32 m and 666.74 s / 13.5 GB at 16 m;
  every row from Tiny up within +9 % (time) and +8 % (memory) of Phase 1a's; Dev at 32 m +36 % / +65 % — the world
  file's zstd encode of the family, not the artifact step (measured against the pre-1b binary). The estimate in
  `forge::play` was not refitted (a ruling in the plan's "As built").
- **Maps:** `just world-map --world target/world-forge/phase1b.world --layer monthly-temperature|monthly-rain
  --month 4|10 --out target/world-forge/phase1b` → `target/world-forge/phase1b-monthly-{temperature,rain}-{04,10}.png`
  (1,024 × 512 each); the values they print are in "Owner hand checks", after Phase 1b.
- **`just test`** (once, 2026-11-20, tree at `c30da2a4` + the owner's uncommitted work): `PASS test-audit`, `FAIL
  workspace`, `PASS physics-next-upstream`, `PASS slotmapd`, `PASS quest`, `PASS site-test`. The workspace stage's one
  failure is `game --test wasm_determinism` `scene_hash_native` — `native scene digest 0xE6CD22F6 != golden
  0x38D86635`, the same value as in Phase 1a; no Phase 1b commit touched `crates/game/tests/` (`git log
  758431e7..HEAD -- crates/game/tests/wasm_determinism.rs`: empty; in `game` Phase 1b added only `monthly_climate.rs`
  and its two lines in `lib.rs`), and the same test on the COMMITTED tree (`git archive` of `8c1d0f2f`, its own
  target directory) passes, 7 of 7. Not Phase 1b's. Every other test passed.
- **`just clippy`** fails as before Phase 1b, in crates it did not touch: `block-mesh` (1 error), `bbmodel` (5),
  `macros` (17) (`git log 758431e7..HEAD -- crates/block-mesh crates/bbmodel crates/macros`: empty). Since those stop
  the workspace run before `game` and `worldgen`, the two were linted on their own with lints capped at warnings
  (`cargo clippy -p game -p worldgen --lib --examples -- --cap-lints warn`): no warning on a line from a Phase 1b
  commit (`git blame` of every warned line in the files Phase 1b touched).
- Status: the controller sets the Status row after the whole-plan review.
