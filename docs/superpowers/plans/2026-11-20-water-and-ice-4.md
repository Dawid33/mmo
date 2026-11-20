# Water and Ice (Phase 4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Written 2026-11-20** from a code check of that day at `8c1d0f2f` (Phase 1a and Phase 1b BUILT; Phases 2 and 3
> PLANNED, not built). It is dispatched only after Phases 2 and 3 are DONE: every symbol under "Depends on" is named
> as those plans specify it and is RE-READ by the controller at dispatch — a symbol that landed under another name
> is corrected in this text before the first task goes out. Search by the named symbol, never by line.
> **No step commits a test.** **Revised the same day** after the builder check
> (`.superpowers/sdd/2026-11-20-water-and-ice-4/builder-check.md`: a dependency audit against Phases 1b, 2 and 3,
> 11 blockers, about 50 fixes, a verdict per reduced item): every BLOCKER and FIX is settled in the text; the
> controller's verdicts on the reduced items are Rulings 15–25; two dependency notes were written into the Phase 2
> plan (no `new_at` hook there) and four into the Phase 3 plan (constant visibility, the cell-normals helper, the
> roll's mask, the shared `--off`).

**Goal:** Every forged world's water is there for a reason: a coast that is the flooded result of rivers and ice cut against a lower sea; lakes whose level and salt follow from a water balance; rivers with twelve monthly discharges, a regime, a width and depth from their flow, braids, waterfalls and deltas; ice sheets and valley glaciers from the monthly climate and the landforms a cold stage left; coasts classed and built; a water table; bays, capes, straits and peninsulas found and named.

**Architecture:** Five moves. (1) **The sea level is a field of the world, not the constant 0** (`World::sea_level`); the erosion schedule runs as today at stand 0, then a short **cold stage** at the finest level re-stands the world on a lower sea (the shelf exposed), runs the last iterations there — rivers with sediment deposition, valley glaciers by a glacial stream-power term computed on a 4× coarser grid — and **floods** it back to 0: the ocean mask of the final world is the 4-connected flood of `h ≤ 0`, a result. (2) **Hydrology becomes volumes**: runoff per climate cell from Phase 3's bucket, twelve accumulation passes with loss in dry channels, a water balance per depression on its hypsometry. (3) **A post-flood stamp stage** edits the fine terrain from simulated fields only (moraines, kettles, delta plains and lobes, barrier islands and lagoons, reefs, shore profiles), then hydrology is analysed once more on the stamped world. (4) **Planetary water lives on Phase 2's 512 × 256 planet grid** (equilibrium lines, ice sheets, coast classes, tide, exposure, sea-ice months, feature detection), resident like the planet record. (5) **One format bump** stores all of it; the runtime keeps exact arithmetic and reads four new things (a reach's depth and wetness, a lake's kind, the ice thickness, the water table through a table lookup).

**Tech Stack:** Rust; crate `worldgen` (the forge, `forged.rs`, `forged_tiles.rs`, `tile_map.rs`, the `forge_proto` and `world_map` examples); `game` in TWO tasks (Task 17: one `const` lookup table in `biome.rs`; Task 20: `CellClimate` gains `ice`, `world_map::WorldMap` gains `features`); mirrors of knob names in `client` (`frontend/generator.rs`), `panel` (`forge.rs`), `world-store` (`ctl.rs`). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` (Intent; Goal 4; Non-goals: the deep ocean, the flowing-water engine; Decisions 4, 5, 10; Architecture; the Phases table). Program: `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Phase 4; its Standing rules bind every task). Research: `docs/research/research_notes/Earth like world generation overhaul/water_features.md` (all), `mountains_erosion_glaciers.md` §3–8, `current_forge_audit.md` §6, the report's water section and evidence ledger.

Ledger: `.superpowers/sdd/2026-11-20-water-and-ice-4/progress.md` (git-ignored workspace). The commit procedure, implementer and reviewer rules are Phase 1a's: `.superpowers/sdd/2026-11-14-planet-frame-1a/{common,implementer-rules,reviewer-rules}.md`.

## Depends on (the controller RE-READS each at dispatch)

**Phase 1a, 1b — BUILT** (symbols read in the tree 2026-11-20): `World { nx, nz, dx, h, ocean, wrap_x, uplift, uplifted, margin_drowned, edges, outlet }`, `World::{step, neighbors, border_cells, center, upsample_onto}`, `LAND_FLOOR_M`, `forge::{schedule, forge_with, Forged, ForgeStage, RAIN_REFRESH_ITERS}`, `erosion::{Routing, StepScratch, route, route_into, discharge, step, FLOOD_EPSILON, NO_RECEIVER}`, `routing_fast::route_fast` (private `OUTLET_LEVEL_M`, `Grid`), `hydrology::{Lake, River, RiverPoint, Stats, Analysis, analyze, seam_dx, LAKE_MIN_DEPTH_M}` (private `find_lakes`, `find_islands`, `find_rivers`, `chaikin`), `post::{fit_height_band, carve_floodplains, carve_lake_basins}`, `fields::{Fields, compute, drainage, drainage_from, drainage_at, ground_slope, water_levels, water_discharge, closed_lakes, salinity_from, salinity, permeability}`, `quality::{Quality, world_score}`, `forged::{RiverPoint, RiverReach, Lake, Segment, WorldArtifact, WorldGlobals, QRasterId, ByteRasterId, ColumnClimate, OUTSIDE_HEIGHT_M, ARTIFACT_VERSION}` (private `WholeRasters::new`, `refine`), `forged_tiles::{Tile, ForgedMeta, LakeRecord, split, TILED_FORMAT_VERSION}`, `tile_map::{WORLD_MAP_VERSION, build_world_map}` (private `reaches_by_tile`, `summarize_tile_with`), `game::{CellClimate, classify, MonthlyClimate, MonthlyNormals}`, `habitat::coast_distance_by`, `report::report`, the gate `forge_quality_matches_baseline` and its fixture, the recipes `forge-report`, `forge-proto`, `forge-bench`, `world-map`, `web-check`, `test-audit`.

**Phase 2 — PLANNED, not built** (`2026-11-20-plates-and-aged-relief-2.md`):
- `forge::planet::{PLANET_NX = 512, PLANET_NZ = 256, PLANET_DEG_PER_CELL, LANDNESS_UNIT_DEG = 90.0, Planet, planet_for, Sampler, peak_target_m, UPLIFT_START_M_PER_YR, CrustKind, OrogenyKind { None, Arc, Andean, Collision, Plateau, RiftShoulder, Hotspot }, PROVINCE_PLATFORM, PROVINCE_OCEAN}`; `Planet`'s per-cell fields `elevation_m`, `land`, `kind`, `strength`, orogeny age, `crust`, crust age, `continent`, `belt` and its lists `continents`, `belts`; `Sampler::{coast_sd_deg, kind, province, cell}`.
- `World::new(p, planet)` — and NO `new_at` (Phase 2's plan was amended 2026-11-20: its landness hook is gone; this plan's Task 3 ADDS `World::new_at(p, planet, stand_m)` and makes `new` call it at 0.0); `World`'s added fields `k_mult`, `d_mult`, `threshold`, `province` (its Task 12 may delete one with its knob: this plan says "whichever exist"); `World::scale_uplift`; **`evolve`'s shape after its Task 12** — the incision is two branches (`threshold[i] == 0.0`: today's line; else `h_star`, `cut = (hi + lift − h_star).max(0.0)`, `kept = (cut − dt · threshold).max(0.0)`, `h_new = (hi + lift − kept).max(LAND_FLOOR_M)`) and the diffusion carries `d_mult` in both stencils (interior and the wrapping columns); **its rebound's `eroded = max(0, (h_ref + Δuplifted) − h)`** (a deposit adds no load); `ContinentalShape::at` returning `(shape, land)`; the sea-floor line `(160 · shape − 1).max(−90)` and the shoals line, both unchanged by Phase 2.
- `forge_with`'s shape: the planet fetched once; `relief_scale`; `post::fit_uplift_rate` after the coarsest level; `ReboundState::{new, apply}` every `REBOUND_EVERY_ITERS`; `post::fit_height_band` as a soft knee; `Forged::{planet, uplift_fit, peak_target_m}`.
- `forged::PlanetRecord` (resident in `WorldArtifact::planet` and `ForgedMeta::planet`), `PlanetRecord::cell_at`, `ForgedWorld::{planet, planet_cell, province_at}`, the province byte raster — **`ByteRasterId::Province`, the SIXTH byte raster** (`ALL` and `Tile::bytes` are six after Phase 2) — `Geology::{columns, rock_at}`.
- Its knob table ("Land": `land_fraction`, `coast_wander_deg`, `coast_detail_deg`, `ocean_margin_start`; "Uplift": `mountain_aspect`, `belt_uplift`, `lowland_uplift_share`; "Erosion": `rebound_share`, `incision_threshold`, `old_erodibility`, `old_diffusion`), its Ruling 3 peak targets per size, its report lines (the five crcs `height_crc`, `artifact_rasters_crc`, `artifact_climate_crc`, `artifact_header_crc` — seed, size and the stored KNOBS —, `artifact_planet_crc`, `artifact_province_crc`; there is no `knobs_crc`; `planet.*`, the belt lines, `height_p50_m` …, `river_km`, `max_strahler`, `response_time_my`, `margin_drowned_share`, `planet_matches_fine`), `report::print_summary`, `just forge-planet`.
- `ARTIFACT_VERSION` 11, `TILED_FORMAT_VERSION` 4, `WORLD_MAP_VERSION` 3, `META_FORMAT` 2.

**Phase 3 — PLANNED, not built** (`2026-11-20-monthly-climate-3.md`):
- `forge::climate::{planetary_for(w, p) -> Arc<Planetary>, Planetary, Switches, fine::rain(&w, &p, &pl, &lakes, &Switches::ALL_ON) -> FineRain { annual_mm, .. }, discharge_weight(precip_mm: &[f32], w), MonthlyFields, grid::{resample, area_mean, deg_cells}, fine::blur_exact, insolation::day_length_h}`; `forge_with` calling `planetary_for(&fine, p)` on the finest initial world; `Forged::{planetary, monthly, climate, rain, climate_ms}`.
- **`climate::derived::{Derived { pet_mm, aet_mm, runoff_mm, swe_mm, pet_year_mm, aridity, mass_balance_mm, snow_months, sea_ice_months, permanent_snow, .. }, derive(n: &game::MonthlyNormals, lat_deg, sea) -> Derived}`** and its constants (`SNOW_ALL_C`, `SNOW_NONE_C`, `MELT_MM_PER_C_DAY`, `EARTH_DAYS_PER_MONTH`, `SEA_FREEZE_C`), `forged::ground_normals`, its constants `pub(crate)` (Phase 3's plan was amended 2026-11-20). Phase 3 has NO helper that makes a `game::MonthlyNormals` from one cell of the float `MonthlyFields` (its Köppen reads the quantised container): this plan's Task 1 creates `water::monthly_cell_normals`. `Planetary::{wind_east_ms, wind_south_ms}` are 12 × 256 × 128 planes on the MODEL grid (Task 13 reads them onto the planet grid with `grid::resample`).
- Its knobs ("Climate": `global_mean_temp_c`, `rainfall`, `inland_reach_deg`, `ocean_currents`, `lapse_c_per_km`, `climate_strength`, `orographic_scale_m`), its presets ("Dry" and "Wet" still override `lake_basins` and `river_min_discharge_m2`), its report lines (`climate.*`, `koppen.*`, `climate.roll_identical`, `seam_*_monthly_wind`), `just forge-climate`, `climate.roll_identical` as amended 2026-11-20 (the planetary stage is re-run on the rolled INITIAL mask), its `--off` (with `--climate`: the `Switches` names; this plan's names apply without it), its roll-exact rules, its Ruling 13 (tuning capped at four rounds) and Ruling 14 (an evidence line counts only if it fails with its feature off).
- **No version bump in Phase 3**: this phase starts from artifact 11, tiles 4, world map 3.

## What was read, and what is relied on second-hand

| Source | Status for THIS plan |
|---|---|
| Hergarten 2021, "Modeling glacial and fluvial landform evolution at large scales using a stream-power approach" (ESurf 9, 937–952) | **Read through a fetch of the HTML, 2026-11-20: Sections 1–6 to Eq. 40 verbatim; Sections 7–10 as a summary.** Verbatim: `p_i = p · min{(H − H_e)/(H_f − H_e), 1}` (Eq. 23; negative below the equilibrium line); `w ∼ q_i^α`, `α ≈ 0.30` (Eq. 13; the example sets the factor to 1 in grid units — no physical constant is given); `E = K_eff · A^m · S^n`, `K_eff = γ^m K_g + (1 − γ)^m K_f`, `γ = A_i / A` (Eqs. 26–28); `A_i,eff = A^ε · A_i^(1−ε)`, `ε = 0.25` "a safe choice" (Eq. 22); slope on the ICE surface; a catchment-based swath of the glacier's width; `H` in Eq. 23 lagged one step; examples `K_g = 2 K`, `H_e = 8`, `H_f = 10` (nondimensional). Summary only: thickness ≈ a constant share of width (0.25 in physical units), cost 1.7× / 2.7× a fluvial step |
| FastScapeLib (Fortran) manual, for Yuan, Braun, Guerit, Rouby & Cordonnier 2019 | **The paper was NOT read** (HAL: access denied; Wiley: 403). Read instead, 2026-11-20: the manual's equation `∂h/∂t = U − K_f A^m S^n + (G/A) ∫_A (U − ∂h/∂t) dA + K_d ∇²h`; "when g > 0 … Gauss–Seidel iterations … from 1 when g is near 0 to 20 when g is near 1"; "O(n) … implicit in time"; examples with `g = 1`. What is lagged inside a sweep, the convergence test and lake handling are NOT stated there: Task 5's scheme is this plan's own (untested design) |
| Cordonnier, Bovy & Braun 2019 (ESurf 7, 549–562) | Read through a fetch to the start of §2.3.1: basins linked through the pass minimising `max(z_n1, z_n2)`; Kruskal `O(n log n)` or Mareš' planar `O(n)`; each basin has ONE outflow pass (its spill level) and the basin on its other side; **no volume-versus-level bookkeeping**; lakes and endorheic basins are not treated. The forge's `routing_fast.rs` already is this |
| Barnes, Lehman & Mulla 2014 (Priority-Flood); Barnes, Callaghan & Wickert 2020 / 2021 (depression hierarchy; Fill–Spill–Merge) | Abstracts and opening sections read through a fetch; the algorithms' bodies as summaries. Taken: a depression hierarchy with per-node outlet level and volume is the published way to fill depressions PARTLY. Not built here (Ruling 5): one balance per spill-level body |
| Deal & Prasicek 2021 (GRL, "The sliding ice incision model") | Not opened. Not used: Hergarten's law is the one built |
| Leopold & Maddock 1953 (USGS PP 252); Leopold & Wolman 1957 (PP 282-B) | **Not read** (`pubs.usgs.gov` returned 403 on 2026-11-20). The exponents 0.5 / 0.4 and `S_c = 0.06 Q^−0.44` (ft³/s) stay the research note's search-summary reading; the SI form `0.0125 · Q^−0.44` is the researcher's arithmetic. Every coefficient is a knob or a tuned constant |
| Nienhuis et al. 2020 / 2015 (delta dominance ratios); Galloway 1975 | **Not read** (login wall; no open copy tried further). The mouth rule (Ruling 8) is this plan's own, in Galloway's qualitative spirit; no Nienhuis threshold is used |
| Barrier-island extent | Wikipedia "Barrier island", first-hand through a fetch: "chains of barrier islands along approximately 13 to 15% of the world's coastlines", "approximately 1,500 barrier islands … over 15,000 kilometers"; tidal classes 0–2 / 2–4 / > 4 m. It does NOT say what the percentage is a share of. Stutz & Pilkey 2011 (about 10 % of open-ocean shoreline, recalled by the brief): not readable (403). The program's 5–20 % target stands as a tuning band, measured as Ruling 10 defines it |
| The research notes (`water_features.md`, `mountains_erosion_glaciers.md`), the report and its evidence ledger, the forge audit §6 | Read in full / the named sections, 2026-11-20. Everything they mark "Inferences", "bg", "recalled" or "needs prototyping" is treated as untested below |

**Untested designs this plan builds** (each has a prototype or evidence step BEFORE anything depends on it; none is trusted from the notes): the short cold stage standing in for "erosion at a low sea" and its world-scaled stand (Rulings 1, 2; Task 4); the shelf profile (Task 3); time-lagged deposition in the implicit solver (Task 5, prototype first); runoff per climate cell and channel loss (Task 6); the regime classes, the braid rule's SI constant, knickpoints as falls (Tasks 6, 7); the closed-lake area equation on one body's hypsometry (the report's ledger: "inferred from a verified principle"; Task 8); a uniform cooling for the cold stage, equilibrium lines by bisection of Phase 3's mass balance, the plastic ice-sheet profile (recalled: Task 9); Hergarten's law as an explicit cut on a 4× coarser grid with a distance swath (Task 10, prototype first); moraine and kettle stamps (dimensions recalled but for the kettle's sourced bounds: Task 11); morphological feature detection (the ledger: "inferred, needs prototyping"; Task 12, prototype first); wave exposure, the tide rule, the coast class rule (Task 13); the mouth rule (Task 14); delta lobes (Task 15); barrier, reef and atoll stamps (dimensions "bg"; Task 16, prototype first); the height-above-drainage water table (Task 17); the name generator (Task 18).

## Global Constraints

- Do not create git worktrees; work on `develop`. No stash, reset, amend, checkout of files, clean, push. The tree
  holds the owner's unrelated uncommitted changes and other sessions stage in the same index: each commit carries
  only the task's own hunks, staged through the index as `common.md` describes. The committed-tree compile check is
  the CONTROLLER's.
- **Tests.** The repository commits only regression tests of bugs actually seen and the existing gates. Every
  "temporary test" below is written first, seen failing, quoted in the implementer's report (name, command, failing
  then passing output) and DELETED before the commit; a commit adds no `#[test]`. An "evidence command" is a tool
  run whose output is quoted in the report. No new gate. A temporary test that needs a `World` builds it as a
  struct literal listing EVERY field (the literal then fails to compile when a field is added — wanted).
- **The order inside every Verify step is fixed:** (1) the temporary tests pass; (2) DELETE them; (3) `just smoke`
  (if its audit fails on a file that is NOT the task's own, run `just test-audit crates/worldgen` plus the task's
  other crate directories and smoke's remaining steps by hand, and say so); (4) `just test-one worldgen "--no-run
  --tests --examples"` when an example, a test or a public `worldgen` symbol changed (every task but 21, 22);
  (5) `just web-check` when `worldgen` or `game` library code changed (every task but 21–24); (6) `just
  determinism-wasm` when `game` changed (Tasks 17 and 20 only; any other task that finds it must touch `game` is
  BLOCKED first). Whole suites (`just test`) once, in Task 24.
- **Two regimes.**
  - *Tasks 1, 2, 9, 12, 13, 14, 18 change no world* (all but 1 and 2 only add forge-side data, report lines and
    sheets). The safety net, run by each: (1) the gate as it stands at that point (Tasks 1, 2: it PASSES untouched
    — `just test-one worldgen "--test it -- --include-ignored forge_lib::"`; the others: the same failure message
    as the task before); (2) `just forge-report --size dev --cell 32 --seeds 8` and `--cell 64 --seeds 8` reproduce every
    line of the newest baseline pair in the ledger directory, unchanged and in order — lines are only APPENDED
    (`diff base.txt new.txt | grep '^<'` prints nothing); in particular `height_crc` and `artifact_rasters_crc` per
    seed. A changed line is BLOCKED, never a re-record.
  - **The crc lines of the net** (Phase 2's names): `height_crc`, `artifact_rasters_crc`, `artifact_climate_crc`,
    `artifact_header_crc`, `artifact_planet_crc`, `artifact_province_crc`, and `world_file_bytes`. **A task that adds
    or deletes a knob moves `artifact_header_crc` and `world_file_bytes`** (the stored knob map) with no world change:
    in Tasks 3, 9 and 13 those two lines are EXEMPT and quoted old → new; nothing else may move there.
  - *Task 3 changes the sea floor only*: `land_height_crc` (Task 1's line) identical per seed; the exemption list is
    exactly `height_crc`, `artifact_rasters_crc`, `artifact_header_crc`, `world_file_bytes`, the three height-seam
    lines (`seam_step_ratio_height`, `seam_step_rank_height`, `seam_ok_height` take their steps over ALL cells, the
    sea floor included) and Task 3's own lines; the gate passes untouched (it reads land statistics only).
  - *Tasks 4–8, 10, 11, 15–17 and 19–22 change worlds on purpose* (19 and 20 through the stored encodings and the
    classifier's hook only: their terrain is identical, proven by `height_crc`). Evidence is the task's before / after report table against
    its stated targets. On every run these must hold, or the task is BLOCKED: the seam rule (per field at most
    `seeds / 4` seeds `seam_ok_*: no` AND median `seam_step_rank_*` ≥ 0.25 — `seam_pass_*: yes` for height, precip,
    biome, monthly_temp, monthly_precip, monthly_wind), `polar_rows_all_sea: yes`, **`drains_to_sea: 1.000`** (its
    meaning is unchanged: every land cell's RECEIVER chain ends at the sea; a river that runs dry is a flow fact,
    Ruling 6), `aliased_regions_identical: yes`, `world_map_wraps: yes`, `climate.roll_identical: yes`, and from
    Task 4 on **`sea_is_height: yes`** (Ruling 3). From Task 4 the mask depends on the erosion and so on the
    climate: `land_share` is judged by its range 0.15–0.6 (inside Phase 3 it was identical under a climate change,
    rightly: no mask depended on erosion then).
- **The quality gate** `forge_quality_matches_baseline` is re-recorded exactly ONCE, in Task 24, in a commit of its
  own stating old → new numbers. It guards land share, hypsometry and rivers ONLY (mouths, km, Strahler, lakes),
  and this phase moves all of them: from Task 4's commit until Task 24 it FAILS if run (it is `#[ignore]`d as slow:
  `just smoke` and `just test` pass; `just test-full` shows it failing). In that window every world-changing task
  runs it WITHOUT bless and quotes its message, and takes its before / after NUMBERS from the report's own lines
  (`land_share`, `height_p50_m`, `height_p90_m`, `height_p99_m`, `max_peak_m`, `concavity_theta`, `hack_exponent`,
  `river_mouths`, `river_km`, `max_strahler`, `lakes`). A task whose table shows one of them moving by more than
  the task's stated expectation says why or is BLOCKED.
- **Stored formats.** `ARTIFACT_VERSION` 11 → 12 and `TILED_FORMAT_VERSION` 4 → 5 in Task 19; `WORLD_MAP_VERSION`
  3 → 4 in Task 20 (the task that changes what `build_world_map` returns; a stored map of another version is
  rebuilt from the archive, not a re-forge); each once. `META_FORMAT` (2) does not move. Until Task 19 no stored byte LAYOUT changes: new forge data
  lives on `Forged` and reaches the world only through today's fields (a lake's level, a reach's width and points,
  the height raster). **One WIRE layout changes, in Task 20:** `game::WorldMap` (it gains `features`) travels as
  `ServerPacket::WorldMap`; client and server are one build and no handshake carries a version, so nothing else is
  needed — but it is said here. No other task may change a stored or wire byte layout; one that finds it must reports BLOCKED.
  Every world is re-forged, never migrated; an old world is refused on every path (file, menu list, SQLite and
  Postgres store).
- There is never a commit with no working, loadable world whose land drains to the sea and whose seam passes.
- **Knobs.** A knob enters `SPECS` in the task that gives it a forge reader, never earlier; a knob whose reader is
  deleted goes in the same task. `--set` refuses a value outside the knob's range: every `--set` below is inside it.
- **Planetary lengths are degrees of latitude** (`climate::metres_per_degree(height_m)` converts at use); true-scale
  lengths stay metres. Which is which is in "Shared vocabulary" — a new length is classified there first.
- **The forge may use std float math**; it stays independent of the thread count (per-cell maps through
  `par::map_range`, tree work through the tree groups, everything else serial). Anything computed on a wrapping
  grid obeys Phase 3's roll rules (no float `sum()` along a wrapping row, no index-order sweeps whose result
  depends on where the row starts): flood fills, chamfers and component labels break ties by canonical cell index
  only where a tie cannot change a stored value. **`climate.roll_identical` proves the CLIMATE's roll only** — it
  recomputes nothing of this phase. The flood, the chamfers, the swath, the stamps and the feature detection are
  proven roll-exact by each task's temporary test on a hand-built world rolled by half its width, and by nothing
  that stays in the tree: said here so no report claims more.
- **Runtime sampling in `forged.rs`, `lib.rs` and `game` stays exact arithmetic** (add, subtract, multiply, divide,
  floor, sqrt), in a fixed expression order. What this phase adds there is listed in Ruling 13; nothing else.
- `game`, `server`, `worldgen` stay Bevy-free. Never write a region edge as a literal.
- Forge cost is measured, not capped. The realistic estimate at Large / 16 m (78.7 M cells; Phase 1a measured 655 s
  and 13.5 GB) is **+30–40 % wall time and about +1.5 GB at the peak** for the whole phase ("Cost", with the cuts
  available). **Nothing may be super-linear in cells**: every disc, swath and window is a chamfer or a separable
  running window, never a per-cell scan of a radius. A task that measures its stage at more than twice its estimate
  says so in its report (a ruling, not a stop).
- Any new or changed command updates its `justfile` recipe (or its comment) in the same task; `CLAUDE.md`
  (Worldgen paragraph) and `TODO.md` change in the commit that changes the fact.
- Never launched by agents: the game window, paid generation, anything on orion or the twin, the dev Postgres
  world, benchmarks other than `just forge-bench`.
- Rulings made by a task are written in its report under "Rulings" as `Ruling: … — why — cost if wrong`.

## Shared vocabulary (decided here, used by every task)

- **Grids.** *Fine*: the forge grid (`World`). *Ice grid*: the fine grid coarsened by `ICE_COARSEN = 4` (block
  means; 64 m at a 16 m cell, 128 m at 32 m). *Climate grid*: Phase 1b's container grid (64–256 m). *Planet grid*:
  Phase 2's 512 × 256 (0.703° per cell: 32 m on Dev, 50 m Tiny, 98 m Small, 196 m Medium, 392 m Large). All
  row-major, x wraps, z does not.
- **Stand.** `World::sea_level` (m): the level the world's sea stands at and its drainage ends at. 0 in every level
  of the schedule; `−low_stand` in the cold stage; 0 again after the flood. **The low stand of a world** is
  `low_stand_world_m(k, peak_target_m) = max(k.low_stand_m · min(1, peak_target_m / LOW_STAND_FULL_PEAK_M),
  min(LOW_STAND_MIN_M, k.low_stand_m))`, `LOW_STAND_FULL_PEAK_M = 2_000.0`, `LOW_STAND_MIN_M = 10.0`
  (Ruling 2):

  | Size | Peak target (Phase 2, belts 8° … 20° deep) | Low stand | Shelf (`shelf_width_deg` 5°) | Shelf edge depth |
  |---|---|---|---|---|
  | Dev | 218 … 546 m | 13 … 33 m | 228 m (7 cells at 32 m) | 16 … 41 m |
  | Tiny | 341 … 853 m | 20 … 51 m | 356 m | 26 … 64 m |
  | Small | 669 … 1,673 m | 40 … 100 m | 697 m | 50 … 125 m |
  | Medium | 1,338 … 2,800 m | 80 … 120 m | 1.39 km | 100 … 150 m |
  | Large | 2,676 … 2,800 m | 120 m | 2.79 km | 150 m |

- **Initial heights** (`World::new_at`, Task 3). Land: Phase 2's. A cell is **polar sea** when the polar margin
  acts on it and leaves it under the sea — `land ≤ 0 && land < shape` (`ContinentalShape::at` returns `(shape,
  land)`, `land = shape − 2 · smoothstep(margin)`; this holds for today's margin-drowned shoals, `shape ≥ 0`, AND for
  a continent's shelf inside the margin band, `shape < 0`): it is sea at EVERY stand (`World::polar_sea`), so rows 0
  and `nz − 1` — where the margin term is 2 — are sea and outlets at every stand, as `World::new`'s and
  `route_fast`'s border asserts require. A polar-sea cell keeps today's heights (shoals, or the shelf line below).
  Any other sea's-own cell (`shape < 0`), with `d = −shape · LANDNESS_UNIT_DEG` degrees seaward and
  `t = d / k.shelf_width_deg`: `h0 = −1 − shelf_drop · min(t, 1) − SLOPE_DROP_M · smoothstep(1, 2, t)`,
  `shelf_drop = low_stand_for(LOW_STAND_DEFAULT_M, peak_target) / SHELF_SHORE_SHARE` — that is,
  from the `low_stand_m` knob's DEFAULT (`LOW_STAND_DEFAULT_M = 120.0`), never from the value in use: `--off lowstand` then changes the stand
  and nothing else (`SHELF_SHORE_SHARE = 0.8`: the default low-stand shore lies at 80 % of the shelf's width;
  `SLOPE_DROP_M = 150.0`). **A cell is sea at stand `s`** when it is polar sea or `h0 ≤ s`. The STORED height is
  `max(h, OUTSIDE_HEIGHT_M)` (−90 m: the playable clamp; deeper water is the planet record's `elevation_m`).
- **The stored-sea invariant** (Ruling 3): in every stored world a cell is sea ⇔ its stored height is BELOW 0 ⇔ it
  is 4-connected, through cells below 0, to a cell of the **seed sea** (`ColdStage::ocean`: the cold stage's mask —
  the initial mask's sea when no stage ran — which holds the polar seas and every initial inland sea deeper than the
  stand). Every sea cell is at most `−SHORE_STEP_M` and every other cell at least `+SHORE_STEP_M`
  (`SHORE_STEP_M = 0.125`, two height steps of `HEIGHT_STEP_M = 0.0625`): no stored height lies strictly between, so
  `QRaster`'s rounding can never put a shore cell on 0.0, and `classify`'s `height_m < 0.0`, the coast distance's
  `height ≤ 0.0` and the engine's implicit sea all agree. A pocket of water a stamp seals off from the seed sea
  becomes LAND at `+SHORE_STEP_M` (a flat): stamps keep their lagoons open.
- **Fine → planet cell** (`water::to_planet`, Task 1): the value of a planet cell is the mean (or the any / the
  majority, as the caller says) over the fine cells whose CENTRES lie in it; when none does — a planet cell smaller
  than a fine cell: Dev at 64 m — it is the value of the fine cell holding the planet cell's centre. On Dev at 32 m
  a planet cell is exactly one fine cell.
- **Volumes.** `SECONDS_PER_MONTH = 365.25 · 86_400 / 12`. A **flow** is m³/s. `Analysis::q` stays what it is — the
  erosion's rain-weighted drained AREA (m²), which still defines the river network (`river_min_discharge_m2`) and
  the gate's numbers. `flow[m]` of a cell is the accumulated runoff of month `m` after losses.
- **Aridity** is Phase 3's index `MAP / Σ PET` of a climate cell at its mean ground height. **Arid** cell: aridity
  < 0.5. **Humid** cell: aridity ≥ 1.0. Between: neither (left out of both lake densities).
- **Regime** of a reach (`Regime`, stored byte, append only), from its twelve flows `f`, `FLOW_MIN_M3S = 0.01`:
  `Dry = 0` (`max f < FLOW_MIN`), `Ephemeral = 1` (at least 6 months under it), `Intermittent = 2` (1–5 months under
  it), then the perennial ones by what feeds them: `Glacial = 3` (share of the catchment under present ice ≥ 0.05;
  assigned from Task 10), `Nival = 4` (snowfall share of the catchment's precipitation ≥ 0.5), `Pluvial = 5` (else).
  A reach is **wet** (it holds played water) when its mean flow ≥ `FLOW_PLAYED_MIN_M3S = 0.02`.
- **Channel form.** `Q_form` = the largest monthly flow. `width_m = max(k.river_width_coeff · sqrt(Q_form),
  RIVER_MIN_WIDTH_M)`, `depth_m = clamp(k.river_depth_coeff · Q_form^0.4, CHANNEL_DEPTH_MIN_M 0.3,
  CHANNEL_DEPTH_MAX_M 8.0)`. `Pattern { Single = 0, Braided = 1, Distributary = 2 }`: braided when the reach's slope
  > `BRAID_SLOPE_COEFF (0.0125) · Q_form^−0.44` AND its sediment concentration `sediment / Analysis::q` at its lower
  end ≥ `BRAID_LOAD_MIN`; a braided reach is `BRAID_WIDTH_FACTOR = 2.5` wider and `BRAID_DEPTH_FACTOR = 0.5` as deep.
- **Lake** kinds (`LakeKind`, stored byte): `Open = 0` (at its spill level, fresh), `Closed = 1` (below it, salt),
  `Playa = 2` (no standing water; a salt bed). Origins (`LakeOrigin`, stored byte, append only): `Other = 0`,
  `Basin = 1` (closed by its balance, nothing else known), `Fluvial = 2`, `Coastal = 3`, `Tectonic = 4`,
  `Volcanic = 5`, `Glacial = 6`, `Moraine = 7`, `Kettle = 8`.
- **Mouth** = a river-network cell whose receiver is sea. `MouthClass { Delta = 0, Estuary = 1, Ria = 2, Fjord = 3 }`
  (stored byte); `DeltaType { River = 0, Wave = 1, Tide = 2 }`.
- **Ice.** `ELA` = equilibrium line altitude (m; `ELA_NONE = i16::MAX` where the balance is negative at
  `ELA_MAX_M = 9_000`; 0 where it is positive at the sea). `now` = today's climate; `cold` = the same climate
  lowered by `k.ice_age_cooling_c`. `ice_max` = fine cells that carried ice at any time in the cold stage. The **ice
  limit** of a hemisphere (Ruling 9) is CLIMATIC, not the glaciers' reach: the lowest `|latitude|` of a land planet
  cell where the cold climate alone puts ice at the shore — it holds a cold sheet, or its `ela_cold_m` is within
  `ICE_LIMIT_ASL_M = 50` m of the cold sea's level. A valley glacier that flows from high ground to the sea
  equatorward of it is therefore a MISS of the program's fjord criterion, not excluded by definition.
- **Coast.** A *coast cell* = a planet cell with a shore in or beside it: some fine cell assigned to it by "fine →
  planet cell" is a land cell with a sea 4-neighbour or a sea cell with a land 4-neighbour (by the mask the stage
  list gives the coast: after the ice stamps, before the delta and coast stamps). On Dev at 32 m that is every planet
  cell on either side of the shore; on Large it is the 392 m cells the shore crosses. Every size has coast cells. `coast_km` =
  the count of fine land–sea cell edges × `dx` × `COAST_EDGE_FACTOR = π/4` (the staircase correction for edges of
  every direction). `Setting { Collision = 0, Trailing = 1, Marginal = 2 }`. `TideClass`: micro < 2 m, meso 2–4 m,
  macro > 4 m. `CoastClass { None = 0, Ice = 1, Glaciated = 2, Reef = 3, Barrier = 4, Muddy = 5, Rocky = 6,
  Sandy = 7 }` (stored byte, append only).
- **Feature** kinds (`FeatureKind`, stored byte, append only): `Ocean, Sea, Gulf, Bay, Cove, Strait, Peninsula,
  Cape, Isthmus, Island, Continent, Range, Lake, River, Falls, Delta, Lagoon, Glacier`.
- **Switches.** `--off <name>[,<name>…]` of `forge_proto` (Ruling 14). A feature whose knob at 0 IS "off" is switched
  by that knob — the tool rewrites `lowstand` → `low_stand_m=0`, `deposition` → `deposition=0`, `cooling` →
  `ice_age_cooling_c=0`, `glacial` → `glacial_erosion=0`, `tide` → `tide_m=0`. The others are
  `ForgeParams::features: Features { lake_balance, loss, stamps_ice, deltas, barriers, reefs, shores }` (all `true`
  by default and in every caller but the tool; never stored). **One flag, two name sets** (shared with Phase 3,
  whose plan lists both): with `--climate` the names are Phase 3's `Switches` fields (`advection`, `currents`,
  `itcz_shift`, `subsidence`, `frontal`, `convergence`, `recycling`, `land_capacity`, `factors`); without it they
  are the twelve of this bullet; an unknown name is refused listing the names of the mode in use.
- **Knobs after this phase** (the task that adds each gives it a reader):

  | Knob | Group | Default [range] | Task | Reader |
  |---|---|---|---|---|
  | `low_stand_m` (new) | Sea | 120 [0, 200] | 3 | the shelf's depth; from Task 4 the cold stage's stand |
  | `shelf_width_deg` (new) | Sea | 5 [1, 20] | 3 | the shelf's width |
  | `cold_stage_iters` (new) | Schedule | 30 [0, 100] | 4 | iterations of the finest level run in the cold stage |
  | `deposition` (new) | Erosion | 0.5 [0, 2] | 5 | `G` of the stream-power step |
  | `river_width_coeff` (new) | Rivers | 4.5 [1, 12] | 6 | width (m) at 1 m³/s; channel loss |
  | `river_depth_coeff` (new) | Rivers | 0.3 [0.1, 1.0] | 6 | depth (m) at 1 m³/s |
  | `ice_age_cooling_c` (new) | Ice | 6.0 [0, 12] | 9 | the cold stage's climate |
  | `glacial_erosion` (new) | Ice | 2.0 [0, 8] | 10 | `K_g / K` |
  | `tide_m` (new) | Coast | 1.0 [0, 4] | 13 | the open ocean's tidal range |
  | `barrier_islands` (new) | Coast | 1.0 [0, 3] | 16 | scales the sediment a coast needs for a barrier (0 = none) |
  | DELETED `river_width_m` | | | 6 | replaced: its unit (m per √km² of rain-weighted area) has no meaning for a flow |
  | DELETED `lake_basins`, `lake_spacing_m`, `lake_radius_max_m`, `lake_depth_max_m` | | | 8 | Decision 4 |
  | Kept, reader unchanged: `river_min_discharge_m2` (the network), `depression_fill`, `floodplain_m_per_sqrt_km2`, `floodplain_max_m` (Ruling 7), `refine_iters` (the finest level now runs `refine_iters − cold_stage_iters` at stand 0, at least 1) | | | | |

- **Report lines** of this phase are appended at the END of `report::report`'s rows, in task order, under the
  prefixes `sea.`, `sediment.`, `rivers.`, `lakes.`, `ice.`, `coast.`, `tide.`, `mouths.`, `deltas.`, `water_table.`,
  `features.` (the two existing keys `lakes` and `river_mouths` keep their meaning). Sheets are written by
  `forge_proto --out <stem>` through `forge/render.rs`: `<stem>-water.png`, `-ice.png`, `-coast.png`,
  `-features.png`.

## The forge after Phase 4 — ONE stage list (every task places its piece by these numbers)

`forge_with`, in order. A task that adds a stage adds it HERE (Task 4 writes this list into `forge/mod.rs`'s module
doc and each later task extends that doc in the same commit).

| # | Stage | Since | Notes |
|---|---|---|---|
| 1 | The planet (`planet::planet_for`) | Phase 2 | |
| 2 | The schedule's levels at stand 0, coarsest first: `World::new`, `upsample_onto`, geology, rain, erosion steps (with deposition: Task 5), the rate fit after level 0, rebound | Phases 2–3 | `climate::planetary_for` once, on the finest INITIAL mask; the finest level runs `refine_iters − cold_stage_iters` |
| 3 | **Re-stand in place** (`World::restand`): the shelf above the low stand becomes land; the stage-0 `Geology` is dropped and one built for the re-stood world | Task 4 | no second `World` |
| 4 | **The cold stage**: `cold_stage_iters` erosion steps at the low stand; from Task 9 the cold ice climate at its start; from Task 10 the glacier applications and the still-stands | Tasks 4, 9, 10 | rain from `fine::rain` on this world with the present-day planetary planes |
| 5 | **Flood 1** (`post::flood(w, 0.0, &seed)`) | Task 4 | the mask becomes a result |
| 6 | `post::fit_height_band`, `post::carve_floodplains` | today / Phase 2 | (`carve_lake_basins` here until Task 8) |
| 7 | **Flood 2** | Task 4 | the planing can open or close water |
| 8 | **The stored climate**: `fine::rain`, `monthly_fields`, the annual rasters on this world; `water::ClimateCells` | Phase 3 / Task 1 | NOT recomputed after stages 10 and 16: land a stamp makes reads the annual values of the water it was, and the container's land share ignores stamps (metres against a 64–256 m cell) |
| 9 | The present ice climate and present ice (`IceClimate`'s `now` half; `glaciers(.., cold_share = 0)`) | Tasks 9, 10 | |
| 10 | **Ice stamps** (moraines, kettles), then **flood 3** | Task 11 | |
| 11 | **Analysis 1** (`hydrology::analyze`: routing, flows, lakes, rivers) | today / Tasks 6–8 | feeds stages 12–16 only |
| 12 | Mask features (`features::detect_mask` on the planet-grid land) | Task 12 | bays and gulfs for the tide |
| 13 | Coast fields and classes (`coast::coast`) | Task 13 | |
| 14 | Mouths, first pass (`coast::mouths` on analysis 1) | Task 14 | |
| 15 | Delta stamps (`stamps::deltas`) | Task 15 | |
| 16 | Coast stamps (`stamps::coast_stamps`), then **flood 4** | Task 16 | |
| 17 | **Analysis 2** on the stamped world — the one that is stored; `Stats` (the gate's numbers) from it | Task 15 | |
| 18 | Mouths, final (`coast::mouths` on analysis 2; a mouth on a stamped delta keeps `Delta`), `hydrology::attach_deltas` | Tasks 14, 15 | `mouths` and `mouths.classed` count THESE |
| 19 | Object features (`features::detect_objects`), names (`names::name_all`) | Tasks 12, 18 | |
| 20 | Fields: the water table, salinity, volcanism; spawn | Task 17 / today | |

Until a task adds its stage the list simply skips it (Tasks 4–14 have one analysis: stage 11 is then the stored one).

## Cost (estimates at Large / 16 m, 78.7 M cells — measured in Task 24)

| Stage | Estimate | Memory held |
|---|---|---|
| Re-stand in place; one more `Geology::new` at the finest cell | ≈ 12 s | `World::polar_sea` 0.08 GB; the seed mask and `cold.q`: 0.39 GB; NO second `World` |
| The cold stage's erosion steps | 0 (they come out of `refine_iters`) | — |
| The cold stage's `fine::rain` at full resolution: the first call and one refresh (30 / 25) | ≈ 30 s (Phase 3 puts one call at 4–6 × today's `orographic`) | — |
| Deposition | one more tree pass per iteration: ≈ +10 % of the erosion time, ≈ 60 s | `StepScratch::qs` 0.63 GB while a level runs; `World::deposit` 0.31 GB |
| Glaciers: 7 applications + the present one on 4.9 M ice cells (a Priority-Flood, one accumulation, two chamfer passes for the swath, a bilinear subtract each) | ≈ 25 s | `World::ice` 0.31 GB; `ice_max` and four margins 0.39 GB |
| Ice climate: the bisection (16 × land planet cells × 2 climates ≈ 1.3 M `derive` calls) | ≈ 4 s | < 10 MB |
| Four floods, the stamps' chamfers | ≈ 25 s | one distance field at a time: 0.31 GB transient |
| Analysis 1 (two Priority-Floods, one twelve-wide flow sweep, lakes, rivers) | already paid today but for the sweep: ≈ +10 s | the sweep's `[f32; 12]` per LAND cell: ≈ 1.2 GB transient (after the erosion scratch is freed) |
| Analysis 2 (the same again on the stamped world) | ≈ 50 s | as above, not at the same time |
| Planet-grid work (coast, features, names), mouths, deltas | ≈ 5 s | < 10 MB |
| **Total** | **≈ +220 s (+34 %)**; the range +30–40 % | **≈ +1.5 GB at the peak** (the cold stage: `qs`, `deposit`, `ice`, the masks); the flow sweep's 1.2 GB comes after the scratch's 0.63 GB and the routing buffers are freed |

**Cuts available if the measurement is worse** (each a ruling when taken): analysis 1 without river OBJECTS (the
coast needs flows, lakes and sediment only: −15 s); no rain refresh inside the cold stage (−15 s); `ICE_EVERY_ITERS`
10 (−10 s); the flow sweep in two halves of six months (−0.6 GB). Nothing above is super-linear in cells.

## Rulings (made while writing this plan; each `what — why — cost if wrong`, with the alternative rejected)

1. **Scope: everything in the program's list is built; eleven items in a reduced first form (two of them marked so by the builder check: the clamp and the low sea; coast segments) that still prints its
   evidence line; nothing is deferred. Order: tools → the sea → sediment, rivers, lakes → ice → coasts, mouths,
   deltas → water table, names → ONE storage task → the runtime → two capped tuning tasks → close.**

   | Program item | Verdict | Built | What remains (Known limits) |
   |---|---|---|---|
   | The sea is a result; the fixed mask and the clamp go; a sea about 120 m lower | REDUCED | the mask is a result of the flood (Tasks 2–4) | the clamp does not fully go: no land or lake floor below the sea (the runtime has no sea mask: Ruling 3, Ruling 24); the low sea is 13–33 m on Dev, 120 m only on Large (Ruling 2) |
   | Deep bathymetry as data; playable floor clamped | FULL | Phase 2's record; the −90 m store clamp (Task 3) | — |
   | Lakes by water balance; closed / salt / playa | REDUCED | one balance per spill-level body, annual, with a seasonal range (Task 8) | the depression hierarchy (sub-basins of one body share a level); rivers across a closed basin's dry floor |
   | Lake origins | REDUCED | eight origins the forge can tell apart (Tasks 8, 11) | oxbow, landslide, karst, thermokarst, crater bowls, impact: no process makes them |
   | Carved-lake knobs deleted | FULL | Task 8 | — |
   | Twelve monthly discharges, snow, desert loss | FULL | Task 6 | runoff is per climate cell (Ruling 6) |
   | Regime class | FULL | six classes (Tasks 6, 10) | — |
   | Hydraulic-geometry width and depth | FULL | Task 6 | coefficients are tuned, not sourced |
   | Braided versus single-thread | REDUCED | the class, and a wider shallower channel (Tasks 7, 20) | bars, anabranches, meanders, oxbows |
   | Knickpoints as waterfalls | FULL | Task 7 | the falling sheet is not drawn (static water) |
   | Sediment deposition (fans, floodplains, basin fill) | REDUCED | a time-lagged deposition term; lakes trap sediment (Task 5) | lake silting is not mass-coupled; `carve_floodplains` stays (Ruling 7) |
   | Deltas as a second layer; two downstream edges | FULL | Tasks 15, 19 | equal flow split; no avulsion history |
   | Every mouth classed | FULL | Task 14 | the rule is this plan's own |
   | Ice sheets on the planet grid | REDUCED | a plastic profile over the cells above the equilibrium line (Task 9) | no flow, no shelves, no areal scour, no isostatic depression |
   | Valley glaciers by glacial stream power on a coarser level | REDUCED | an explicit cut on the ice grid with a distance swath (Task 10) | the catchment swath, the ice-surface slope with thickness, cirque headwalls |
   | One cold stage, low sea, stepped retreat | FULL | Tasks 4, 10 | one stage stands in for many (the ledger: inferred) |
   | Moraines, kettles "and the like" | REDUCED | terminal and recessional moraines, kettles; outwash through the deposition term (Task 11) | drumlins, eskers, erratics |
   | Coast classes per segment | REDUCED | a segment is a planet cell with a shore in or beside it (Task 13) — on every size | no 20–50 km polyline segments; on Dev a segment is one fine cell |
   | Barrier islands and lagoons | FULL | Task 16 | spits, tombolos, inlet shoals |
   | Reefs and atolls | REDUCED | fringing reefs; atolls on subsided hotspot seamounts (Task 16) | barrier reefs, patch reefs |
   | Sandy, rocky, muddy shores | REDUCED | the class; beach and tidal-flat profiles (Task 16) | cliffs and platforms; the COVER (sand, mud) is Phase 5's shore strip |
   | Water table replaces `drainage` | FULL | Tasks 17, 19 | no aquifer classes (Phase 5's underground layer) |
   | Bays, capes, straits, peninsulas detected and named; a naming subsystem | FULL | Tasks 12, 18–20 | names are syllables, not a language; no history tie-in |
   | Stored, not played: seasonal river levels, tides, floating ice | FULL | twelve flows per reach and a lake's seasonal range; tidal range per coast cell; sea-ice and lake-ice months (Task 19) | — |

   — why this order: every producer runs forge-side first and reaches the world only through today's fields, so
   one format bump (Task 19) stores finished data and no commit holds a half-format — rejected: a container task
   first with placeholders (Phase 1b's pattern: this phase's records are not one fixed-shape family) — cost if
   wrong: until Task 19 the game sees a reach's regime only as "wet or not there" and no ice thickness; nobody plays
   those builds.
2. **The low stand is a SHARE of the world's relief, in metres, capped at 120 m: `low_stand_world_m` (vocabulary)
   — and the shelf is planetary in width, the stand's depth at its shore.** A stand of 120 true metres on Dev (peaks
   218–546 m, coastal land ramping to +45 m over 2 km) drowns every valley for kilometres and leaves a world of
   ridges; Earth's 120 m is about a twentieth of a 2.5 km range. The base level is a planetary event measured
   against relief, as belts are; rivers, valley widths and channel depths stay true scale. So Dev floods valleys
   13–33 m deep (60–200 m wide at ordinary valley-side slopes: 2–6 cells at 32 m), Large the full 120 m. **Phase 2's
   `sea_stand` cannot express it** (its note says so): it is a landness threshold, every ocean cell is shallower
   than 90 m (a −120 m stand would dry the whole sea), and the polar margin's shoals (−20 m) would surface in the
   outlet rows. Task 3 therefore redefines `new_at`'s third argument as a stand in METRES against a new shelf profile,
   and pins margin-drowned cells as sea at every stand — rejected: 120 m on every size (above); a stand in degrees
   of landness (no depth to cut against) — cost if wrong: Dev's rias are too small to see, or Large's too large —
   `low_stand_m` and `LOW_STAND_FULL_PEAK_M` are the dials, Task 21 tunes, and the evidence is per size
   (`sea.drowned_valley_km`, `sea.coast_km_ratio`).
3. **The mask is an input to each erosion level and a RESULT of the last stage; the stored invariant "sea ⇔ height
   ≤ 0" is kept, so the runtime changes nothing for the coast.** The design's "erosion at a low sea → flood" cannot
   mean the whole run: the schedule drives the land to near its steady state (Phase 2's `response_time_my`), and a
   steady state graded to −120 m is a continent regraded, not valleys cut — Earth's low stands lasted 10⁴–10⁵ years.
   So: every level of the schedule keeps its own initial mask at stand 0 (as Phase 3 found it); the **cold stage** —
   `cold_stage_iters` (30) of the finest level's `refine_iters`, 75,000 years at the default step — re-stands the
   world on the low-stand mask and runs there; the **flood** then makes the final mask: sea = the 4-connected flood
   of cells below 0 from the SEED sea (the cold stage's mask), after which every sea cell is at most `−SHORE_STEP_M`
   and every other cell at least `+SHORE_STEP_M` (the vocabulary's invariant — `flood` lowers a shore sea cell and
   raises a low land cell to make it so). The re-stand is IN PLACE (`World::restand`: a sea cell's `h` is still its
   initial floor — outlets are never eroded — so the cells to expose are just the non-polar sea cells above the
   stand; no second `World`, and the stage-0 `Geology` is dropped before the new one is built). The clamp
   `LAND_FLOOR_M` becomes a floor above the STAGE's stand that **never raises a cell above where it was**:
   incision and both diffusion stencils write `x.max((w.sea_level + LAND_FLOOR_M).min(before))` (`before` = the
   cell's height entering that step or sub-step), so a cell the ice cut below the stand stays where the ice left it
   and is otherwise only lowered or silted. The raise to `+SHORE_STEP_M` is forced by the runtime, which has no sea mask:
   `WholeRasters::new` takes the coast distance from `height ≤ 0`, `game::classify` calls `height_m < 0` ocean, and
   the engine floods every column below `SEA_LEVEL` — dry land below the sea cannot be represented, and a lake's
   floor below the sea would read as coast. What every consumer of `World::ocean` reads:

   | Consumer | Today | After |
   |---|---|---|
   | `erosion::evolve` (base level, the shore in diffusion), `route_into`, `routing_fast` | sea cells at level 0 | sea cells at `w.sea_level` (Task 2) |
   | `forge_with`'s per-level mask; `upsample_onto` | `World::new` per level | unchanged: `new_at(.., 0.0)` per level; the cold stage's mask from `new_at(.., −low_stand)`; the final mask from `post::flood` |
   | `climate::planetary_for(&fine, p)` (sea-surface temperature, continentality, the model's land) | the finest initial mask | UNCHANGED — the present-day initial mask is the planetary climate's land. The flooded coast differs from it by drowned valleys and stamps far below a 1.4° model cell; `sea.model_mask_mismatch` (model cells whose land share moved by more than 0.1) is reported and expected 0 |
   | `fine::rain`, `discharge_weight`, `lakes_from_filled`, the annual raster, `monthly_fields` (the container's land) | the level's world | the world passed: the cold world in the cold stage, the FLOODED world for everything stored |
   | `polar_rows_all_sea` | rows 0 and `nz − 1` of `w.ocean` | the same rows of the final mask (margin cells are sea at every stand: it holds by construction) |
   | outlets, `drains_to_sea` | `outlet == ocean` | `outlet == ocean` after every mask change (`post::flood` sets both) |
   | `hydrology`, `fields`, `post`, `spawn::pick`, `render`, `Forged::lake_bodies`, `report` | `w.ocean` | the final mask |
   | `Forged::initial`, Phase 2's `planet_matches_fine` and `margin_drowned_share` | the finest initial world | unchanged (stand 0) |
   | the runtime: `coast_distance_by(height ≤ 0)`, `classify`, `refine`'s "water at or below the sea is the sea", `TileSummary::sea_fraction`, history's habitat | stored height | unchanged, by the invariant; `sea_is_height` proves it per world |
   | the seam | `World::step` walks | the flood and every new walk use `World::step` |
   | `climate.roll_identical` | rolls the forged world and re-runs the climate on it | `planetary_for` on the rolled INITIAL mask (`Forged::initial`'s — what the forge itself fed it), `fine::rain` on the rolled FINAL world (Phase 3's plan amended; Task 4 edits the helper). It proves nothing about this phase's own walks (Global Constraints) |

   — rejected: storing a sea-mask bit and rewiring `classify`, the coast distance and the engine's implicit sea
   (Phase 5 rewrites the classifier; a mask would let basins below the sea exist) — cost if wrong: no dry land or
   lake floor below sea level, ever; `sea.raised_cells` and `sea.lowered_cells` count what the rule moved.
4. **Land share is tuned back, not solved again.** Phase 2 solves the sea level for `land_fraction` on the planet
   grid; the flood then takes land (drowned valleys) and the delta lobes give a little back. `sea.land_lost_share`
   reports it; Task 21 moves `land_fraction`'s DEFAULT so the 8-seed median of `land_share` stays where Phase 3 left
   it (± 0.02) — rejected: a second solve after the flood (it needs the erosion run inside a root-finder) — cost if
   wrong: one default.
5. **Lakes: one annual balance per spill-level body on its hypsometry, decided inside the flow accumulation; a
   closed lake stands below its spill; a body too small or too shallow at its level is a playa; a seasonal range
   is stored.** The forge's routing gives each depression one spill level (Cordonnier 2019, read: no volumes). For
   a depression `D` (every cell under the spill-level flood: `basin_depth > 0`; its body at a level is the cells
   at least `LAKE_MIN_DEPTH_M` under that level): `W` = the annual flow arriving at its outlet cell (m³/yr) — a
   cell of `D` contributes its climate cell's RUNOFF like any land cell, and the balance takes that back by using
   `W − Σ runoff of the cells the lake covers`; `E` = `OPEN_WATER_PET_FACTOR (1.0) ·` the mean annual PET over `D`,
   `P` its mean annual precipitation. `E ≤ P`, or `A* = W / (E − P) ≥ A(spill)` → **open** at the spill, outflow
   `W − (E − P) · A(spill)`. Else **closed** at the level `h*` where the area of `D`'s cells with floor below `h*`
   is `A*` (the hypsometry counts EVERY cell of `D`, the shallow rim included); closed and (`A*` under
   `LAKE_MIN_CELLS` cells or mean depth under `PLAYA_MAX_DEPTH_M = 0.5`) → **playa**. **Lakes in series are decided
   with all twelve months, in one pinned order:** the flow sweep (Ruling 6) carries the twelve monthly flows of
   every land cell and walks the routing upstream-first, so when it reaches a depression's outlet cell every
   upstream lake has already been decided and has already passed on its twelve outflows; the lake is decided there
   from the sum of its twelve inflows, and its twelve outflows replace the outlet cell's flows before the sweep goes
   on (open: `out_m = max(0, in_m − (E_m − P_m) · 10⁻³ · A / SECONDS_PER_MONTH)`, `E_m`, `P_m` the month's mm, `A` the lake's area;
   closed or playa: 0). `level_range_m` = (the largest minus the smallest of the running sum over the months of
   `(in_m − out_m) · SECONDS_PER_MONTH − (E_m − P_m) · 10⁻³ · A`, in m³) ÷ `A`, `A` the area at the balance level.
   lake's area. `Analysis::lake_depth` becomes the depth under the BALANCE level, so every reader of "is this cell
   lake" follows with no edit. Salinity: `Closed` and `Playa` are salt (`fields::closed_lakes` and
   `ENDORHEIC_INFLOW_RATIO` are deleted). **Origins** are told apart only where the forge knows the cause, first
   match: `Kettle` (most cells in a stamped kettle), `Moraine` (its pass is a stamped moraine cell), `Glacial`
   (most cells in `ice_max`), `Volcanic` (within half a cone radius of a volcano site), `Tectonic` (most cells'
   planet kind is `RiftShoulder`), `Coastal` (level under 2 m and within `COASTAL_LAKE_REACH_M = 500` of the sea),
   `Fluvial` (`World::deposit` at its pass ≥ 1 m, or most cells on a planed valley floor), `Basin` (closed or playa),
   else `Other`. **Nothing replaces `carve_lake_basins`:** lakes come from what makes basins — the ice's cuts,
   moraines and kettles, sediment damming side valleys, the flood's leftovers — and where none acts (a warm, wet,
   unglaciated plain) there are none; `depression_fill` (how fast a basin silts) is the dial. The gate's `lakes`
   counts `Open` + `Closed` — rejected: the Barnes hierarchy (a second routing structure for a refinement nobody
   can see at 32 m); keeping nine bowls "for now" (Decision 4 deletes them; a bowl has no reason) — cost if wrong:
   lake-poor warm lowlands (Task 21 judges `lakes.per_1000km2.humid`; a miss after four rounds is a ruling, not a
   bowl); two pools of one closed body share a level; a river entering a closed basin stops at the old spill-level
   shore and the lake lies below it across a dry floor (the routing on a filled basin points at the spill, not at
   the pool).
6. **Monthly discharge: runoff per CLIMATE cell from Phase 3's bucket at the cell's mean ground height, ONE
   upstream-first sweep of the fine routing carrying twelve flows per land cell, loss in the channel where the air is short of water; the network
   is unchanged and a river that runs dry is a dry reach, not a missing one.** `derive` per fine cell would need
   twelve values per fine cell (3.8 GB at Large / 16 m) for relief a 64–256 m climate cell does not have
   (≤ 0.7 °C of lapse). The sweep holds a `[f32; 12]` per LAND cell (indexed by position in the routing order:
   1.2 GB at Large / 16 m, transient) — twelve separate passes through one buffer cannot decide lakes in series
   (Ruling 5). Per month `m`: a land cell's source is `runoff_mm[m] · 10⁻³ · dx² / SECONDS_PER_MONTH`;
   passing from `i` to its receiver the flow loses `max(0, pet_mm[m] − aet_mm[m]) · 10⁻³ · LOSS_WIDTH_FACTOR (10) ·
   width(flow) · dist / SECONDS_PER_MONTH`, floored at 0 (the bucket's unmet demand over a wetted strip ten channel
   widths wide: untested; `--off loss` removes it). Snow storage and melt are the bucket's. A reach stores the
   twelve flows at its lower end. **`drains_to_sea` is untouched** — it is a statement about receivers — and the
   flow facts get their own lines (`rivers.ending_inland`, `rivers.reach_sea_share`). The runtime carves every
   reach's channel and fills only wet ones; the world map draws only wet ones. **Seasonal river levels are stored as
   the twelve flows** (`[f32; 12]` m³/s per reach, 48 B): a stage is `river_depth_coeff · flow^0.4`, which whoever
   plays it computes from the stored knobs — rejected: a channel network re-thresholded on flow (it moves the
   gate's network numbers and Strahler orders for no gain); storing stages — cost if wrong: runoff steps by a
   climate cell under a mountain front (blurred by accumulation); the loss factor is one number.
7. **Deposition is a time-lagged term in the existing implicit step: one pass more per iteration; `carve_floodplains`
   stays.** FastScape's equation (read from its manual): `∂h/∂t = U − K A^m S + (G/A) ∫ (U − ∂h/∂t) dA`, solved there
   by 1–20 Gauss–Seidel sweeps per step. Here the upstream integral `Qs` is taken from the PREVIOUS iteration's
   result (the landscape moves little per step) and the step stays one downstream-first pass:
   `h_star = (h_i + lift + dep + f · base) / (1 + f)`, `dep = dt · G · Qs_in,i / q_i` — in BOTH of Phase 2's incision
   branches: with no threshold `h_new = h_star`; with one, `cut = h_i + lift − h_star` and only a POSITIVE cut is
   reduced by the threshold (`kept = (cut − dt · threshold).max(0)`, `h_new = h_i + lift − kept`), while an
   aggrading cell (`cut ≤ 0`) takes `h_new = h_star` whole — Phase 2's `cut.max(0)` would otherwise drop every
   deposit on an old belt; then one upstream-first pass rebuilds `Qs`
   (`e_i = (h_before + lift − h_new) · dx² / dt`; `Qs_out,i = max(0, e_i + Σ donors)`; a cell under a lake passes
   nothing on). A deposit never lifts a cell above `DEPOSIT_MAX_M_PER_STEP = 2.0`. Task 5 PROTOTYPES it on a
   synthetic range front before the forge uses it, against 8 in-step sweeps. With Phase 2: steady relief stays
   linear in the uplift rate (the rate-space fit holds; the factor becomes `1 + G`-ish, which the fit absorbs);
   Phase 2's rebound counts `max(0, eroded)`, so a deposit adds NO isostatic load — a fan does not sink its
   basin (stated, not wanted: a Known limit); `uplifted` is not raised by a
   deposit, so the rock under a fan is the column's own (no "alluvium" rock: Phase 5's soil). It must show: fans
   where channels leave a belt (`sediment.fans`), fill behind them (`sediment.aggraded_land_share`), a deposited
   share of what was eroded (`sediment.deposited_share`), and `G = 0` must zero all three. **`carve_floodplains`
   survives**: deposition aggrades a channel one cell wide (routing is single-receiver); a floor planed sideways
   is not a deposition result — rejected: Yuan's in-step iteration (up to 20 sweeps: the erosion time doubles or
   worse at Large); a transport-capacity cap (no fans) — cost if wrong: the lag oscillates at a slope break (the
   prototype's test; the fallback is `DEPOSIT_SWEEPS = 2` in-step sweeps, +10 % more).
8. **Deltas are a second layer on the river OBJECTS; the routing stays single-receiver. Each mouth is classed by an
   ordered rule that always returns one of four.** Nothing downstream of the forge assumes a tree: `WorldArtifact::rivers`
   is an unordered list of polylines, `WholeRasters::new` buckets segments reach by reach, `refine` takes the
   channel-nearest segment, `reaches_by_tile` and `TileSummary::rivers` count reaches by their points. The tree lives
   only in `find_rivers` (Strahler, one reach per node pair) and in `Stats::mouths` — both keep reading the routing.
   So the smallest change is: `RiverReach` gains `down: [u32; 2]` (`NO_REACH = u32::MAX`), and a delta appends
   branch reaches (`Pattern::Distributary`) from an apex to their own mouths (Undiscovered Worlds' backward trace:
   read by the researcher). The gate's `river_mouths` stays the TRUNK mouths. **The mouth rule** (this plan's own;
   first match): `Fjord` — its drowned inlet (the cold stage's channel, now sea, followed seaward while the water
   is narrower than `INLET_MAX_WIDTH_M`) is at least `INLET_MIN_CELLS = 4` long and at least half of it lies in
   `ice_max`; `Delta` — the river's sediment over `DELTA_FILL_YEARS = 7_000` is at least the inlet's volume (a
   mouth with no inlet: at least `DELTA_MIN_VOLUME_M3`) and its tide is not macro; `Ria` — the inlet is at least
   `INLET_MIN_CELLS` long; else `Estuary`. A delta's type: `Tide` when meso-tidal, else `Wave` when the coast
   cell's exposure ≥ `DELTA_WAVE_EXPOSURE`, else `River` — rejected: multiple-flow routing (it changes the erosion,
   the gate's numbers and every tree walk); Nienhuis' ratios (not read) — cost if wrong: classes that do not read
   right on the map; every threshold is one constant and Task 22 tunes them.
9. **Ice: a uniform cooling, equilibrium lines by bisection of Phase 3's mass balance on the planet grid, sheets as
   a plastic profile, valley glaciers as an explicit Hergarten cut on the ice grid inside the cold stage, a
   three-step retreat, and present-day ice as a stored thickness.** The cold climate is today's monthly normals
   lowered by `ice_age_cooling_c` (6 °C; the notes' search summaries give 5.4 °C in the tropics and up to 10 °C) —
   the energy balance is not re-run (its ice-albedo ramp would cool the poles more: a refinement). Per planet cell
   and climate: `ELA` = the height where `derive(ground_normals(sea, z, P, lapse), lat, false).mass_balance_mm`
   changes sign (16 bisection steps). **Sheets**: 4-connected groups of at least `SHEET_MIN_CELLS = 12` planet cells
   whose mean ground stands above their `ELA`; thickness `SHEET_COEFF (4.7) · sqrt(distance to the group's edge in
   metres)` (a plastic profile at 100 kPa: recalled, unverified). **Valley glaciers** (every `ICE_EVERY_ITERS = 5`
   cold iterations, on the ice grid): `p_i = p · min((H − H_e)/(H_f − H_e), 1)` with `H_f = H_e + ICE_FULL_ABOVE_M
   (500)`; `A_i` accumulated down the ice grid's own routing, floored at 0; `w = GLACIER_WIDTH_COEFF (4.0) ·
   A_i^0.3` m capped at `GLACIER_WIDTH_MAX_M = 3_000`; thickness `GLACIER_THICKNESS_SHARE (0.05) · w` (the one
   figure the fetched text gives for the adopted `h ∼ w`, from its summary, in the paper's units: 150 m of ice at
   the widest). **The swath: every ice-grid cell within `w / 2` of a glacier cell is cut by ITS OWN slope** — the
   source rejects a uniform cut across the swath as "unable to change the shape of a valley" — `ΔH_j = dt_ice ·
   k.glacial_erosion · K_j · sqrt(A_j^0.25 · A_i,src^0.75) · S_j`: `S_j` the cell's slope to its receiver on the
   ICE surface (ground + the previous application's ice), `A_i,src` the ice flux of the glacier cell that reaches
   it with the most reach to spare (ONE generalised chamfer on the ice grid carrying `w_src / 2 − distance` and
   the source's flux: two passes, repeated while a wrapping row changes — O(cells), never a disc per glacier cell),
   capped at `GLACIAL_CUT_MAX_M = 15` per application, NOT floored at the stand (overdeepenings and fjord basins:
   Ruling 3's clamp never lifts them back), then bilinearly subtracted from the fine heights. Fine fluvial steps
   between applications reconnect the drainage; depressions under ice do not silt. **Retreat**: the first 60 % of
   the cold stage at the cold line, then three still-stands with the line a third of the way back each. The ice
   EDGE (ice cells with a non-ice 4-neighbour) is recorded at the END of each of the four stands: the cold maximum's
   is the TERMINAL margin, the next two are recessional, the fourth is today's. **Present
   ice** = the sheet's thickness now, or a last routing at today's line, as `World::ice` (m). Hergarten's law is for
   valley glaciers and "fails for piedmont and plateau ice": under a sheet the width cap bounds it. **Small
   worlds:** with a true 6.5 °C/km lapse and 200–850 m of relief, Dev and Tiny hold ice by latitude, not by
   altitude — few or no alpine glaciers; evidence per size (`ice.valley_glacier_cells.cold`), stated as a Known
   limit, not tuned away — rejected: shallow-ice flow (the notes: wrong valleys when coupled, hours at fine cells);
   the term inside the implicit solve at the fine cell (2.7× the step) — cost if wrong: V-shaped "glacial"
   valleys (`ice.u_valley_ratio` shows it; the swath radius is the dial).
10. **Coasts: a segment is a planet cell; five inputs; an ordered class rule; stamps on the fine grid only where
    three cells can carry them.** Per coast cell: `Setting` from the plate record (`Collision` when a cell within
    `SETTING_REACH_DEG = 5` inland is an `Arc`, `Andean` or `Collision` orogen younger than 80 My; `Marginal` when
    the water it faces is not the world ocean's component; else `Trailing`); **exposure** 0–1 = over 8 directions,
    `sqrt(fetch / FETCH_CAP_DEG (60))` (open water along the direction on the planet grid) times the year's mean
    of the squared onshore wind (Phase 3's planes), normalised by `EXPOSURE_FULL`; **tide** (m) =
    `k.tide_m · enclosure · funnel` — `enclosure = ENCLOSED_SEA_TIDE (0.2)` in a sea that is not the world ocean,
    else 1; `funnel = 1 + FUNNEL_GAIN (1.5) · min(length / mouth, 3)` inside a detected bay or gulf (Task 12), else
    1; no shelf term (the shelf is one formula everywhere: it carries no information). So without a tide model
    the classes come out micro on open coasts and in enclosed seas, meso to macro at the heads of long bays;
    **sediment** = the mouths' sediment within `SEDIMENT_REACH_DEG = 3`, plus a constant where the cell is in
    `ice_max`; **winter sea temperature** = the coldest month of the nearest sea climate cell. Class, first match:
    `Ice` (present ice at the shore), `Glaciated` (`ice_max` and a steep shore), `Reef` (winter sea ≥ 18 °C, low
    sediment), `Barrier` (exposure ≥ `BARRIER_MIN_EXPOSURE`, tide < 4 m, sediment ≥ `BARRIER_MIN_SEDIMENT /
    k.barrier_islands`, a low shore, not `Collision`), `Muddy` (sheltered or macro-tidal, with sediment), `Rocky`
    (`Collision`, or a steep shore), `Sandy`. A barrier ridge is 3 fine cells wide (48 m at 16 m, 96 m at 32 m)
    behind a lagoon at least 4 cells wide: representable at every forge cell in use, true to scale at 16–32 m.
    **`coast.barrier_share`** = the share of the PRE-STAMP shore's `coast_km` — every land–sea edge of the mask
    that stage 13 saw, islands included — whose sea side has a barrier ridge within `LAGOON_CELLS.1 + BARRIER_CELLS`
    cells; target 0.05–0.20 over the 8-seed median, tuned in Task 22 —
    rejected: polyline segments of 20–50 km (tracing and cutting a coast on a wrapping raster for attributes that
    are smooth planetary fields) — cost if wrong: on Dev (a planet cell is one fine cell at 32 m, half of one at 64 m) a segment is a fine cell and classes may speckle
    (`coast.class_runs_mean_cells` reports the mean run length; a 3 × 3 majority filter is the fallback).
11. **Features are detected on the planet grid by morphological closing and opening at a ladder of radii, with the
    semicircle rule; names come from a new, small, seeded syllable generator; they are stored with the world and
    shown by the map tool and in the map window's tile text.** The mask is the FINAL land (a planet cell is land at
    half or more). Radii `FEATURE_RADII_DEG = [1.4, 3.5, 8.4, 21.0]` (2, 5, 12, 30 planet cells). Water in
    `close(land, r) − land`, by 4-connected component: ONE mouth onto open water and area ≥ `π · mouth² / 8`
    (UNCLOS Art. 10, read by the researcher) → `Cove`, `Bay`, `Bay`, `Gulf` by the smallest radius that finds it;
    two or more mouths → `Strait` (anchored at its narrowest cell). Land in `land − open(land, r)`: one neck →
    `Cape` (the two small radii; anchored at its cell farthest from the neck) or `Peninsula`; two necks → `Isthmus`.
    Also: sea components (the largest is `Ocean`, the rest `Sea`), land components (`Continent` by Phase 2's rule,
    else `Island`), Phase 2's belts (`Range`), and from the fine world the `FEATURE_TOP_N = 24` largest lakes,
    longest rivers (trunk chains by flow), highest falls, largest deltas, lagoons and glaciers. A feature found at
    two radii is kept at the smaller. **No name generator exists in the tree** (`history::records::person_name` is
    `"Dwarf {id}"`): `forge/names.rs` builds 2–3 syllables from fixed tables by `hash01(seed, NAME_SALT, feature
    index · 8 + try)`, unique within the world, and joins a kind's generic term ("Gulf of X", "X Bay", "Cape X",
    "X Strait", "Lake X", "X Falls" …: Dragons Abound's scheme, read by the researcher) — rejected: detecting on
    the fine mask (30-cell discs on 78.7 M cells, for features the planet grid resolves); names in `game`
    (nothing in the simulation reads them) — cost if wrong: the ledger says the detection "needs prototyping" —
    Task 12 prototypes it on drawn masks and a sheet before Task 13 reads a bay from it.
12. **Storage: one resident `WaterRecord`, three extended object types, one byte raster swapped and one added.**

    | What | Where | Encoding | Bytes |
    |---|---|---|---|
    | per reach: `flow_m3s`, `depth_m`, `regime`, `pattern`, `down`, `name` | `RiverReach` (file and tiles' segment source) | `[f32; 12]`, `f32`, `u8`, `u8`, `[u32; 2]`, `u32` (feature index, `NO_NAME`) | +66 per reach (≈ 1.3 MB at 20,000 reaches) |
    | per lake: `kind`, `origin`, `level_range_m`, `ice_months`, `name` | `Lake`, `LakeRecord` | `u8`, `u8`, `f32`, `u8`, `u32` | +11 per lake |
    | wet / dry segment | `Segment::wet` | `bool` | +1 per segment |
    | mouths, waterfalls, features | `WaterRecord` | `Vec<Mouth>` (28 B), `Vec<Waterfall>` (20 B), `Vec<Feature>` (≈ 60 B) | kilobytes |
    | coast: class, setting, exposure, tide, sediment, winter sea | `WaterRecord::coast`, planet grid | six `u8` / `i8` planes | 0.79 MB raw |
    | ice: `ela_now_m`, `ela_cold_m`, `sheet_now_m`, `sheet_cold_m`, `ice_max`, `sea_ice_months` | `WaterRecord::ice`, planet grid | `i16`, `i16`, `u16`, `u16`, `u8`, `u8` | 1.31 MB raw |
    | present ice thickness | tiles: a SEVENTH byte raster `ByteRasterId::Ice`, appended after Phase 2's `Province` | `u8`, `ICE_STEP_M = 2.0` m steps (0–510 m) | +1 per fine cell (78.7 MB raw at Large / 16 m; nearly all zero) |
    | the water table | tiles: `ByteRasterId::Drainage` becomes `ByteRasterId::WaterTable` | `u8` `q`; depth m = `(q / 4)²` (0–4,064 m; a half-step error of at most `√d / 4 + 1/64` m: 6 cm at the surface, 0.3 m at 1.3 m, 2.5 m at 100 m) | 0 (a swap) |

    `WaterRecord` is whole in `WorldArtifact::water` and `ForgedMeta::water` (Phase 1b's and Phase 2's pattern:
    never tiled, about 2.1 MB raw on every size, mostly zeros). Tides are `coast.tide_dm` (decimetres), floating
    ice is `ice.sea_ice_months` and `Lake::ice_months`. Artifact 12 and tiles 5 in Task 19; world map 4 in Task 20
    (`WorldMap::features`) —
    rejected: a bump per slice (five re-forges of every dev world); the ice on the planet grid only (392 m stairs
    of glacier on Large) — cost if wrong: a field wants more range and saturates (the report counts saturated
    values: `water.clamped_values`).
13. **The runtime renders, with exact arithmetic only:** the flooded coast, deltas, barriers, moraines and kettles
    (they are the height raster); lakes at their balance level, none on a playa; a reach's channel `depth_m` deep
    and `width_m` wide (a braided one by its two factors), water only in a wet reach; a waterfall as a two-metre
    segment carrying the whole drop (the existing block-snapped stair of water bodies, one step tall); **glacier
    ice as blocks from the stored thickness** (`ColumnClimate::ice_m`, a bilinear read of a byte raster times a
    constant, capped at `GLACIER_MAX_BLOCKS`, raised 40 → 120), which replaces the temperature rule of
    `glacier_blocks`; the biome `Glacier` where the forge says ice (`CellClimate::ice`, the one hook into `game`
    ahead of Phase 5's passes). **Data until Phases 5–6:** the twelve flows and regimes, tides, sea-ice and lake-ice
    months, coast classes (Phase 5's shore strip lays sand and mud), the water table beyond its drainage lookup,
    equilibrium lines. Sea and lake ICE blocks keep today's annual-temperature rule (Phase 6's overlays) —
    rejected: a falling-water block (the water engine's) — cost if wrong: 120 blocks of ice per column where sheets
    are thick (generation cost: an Owner hand check).
14. **Every evidence line is judged with its feature switched off as well; two capped tuning tasks.** For each
    program criterion (measured as below) the closing run also prints the line with the feature off, and the
    criterion counts only if the off run FAILS it. Tasks 21 and 22 take at most FOUR rounds each (a round = change
    dials, run the task's commands, look at the named sheets, record the table); a miss after the fourth is
    written as a ruling with its number. **Which sizes carry which line:** Dev's land is about 41 km² a seed (330 km² over
    eight, the arid and the humid part each a fraction of it) and its lakes a handful, so a pooled Dev ratio is one
    lake against none. **Lake densities are JUDGED on 2 Medium seeds and 2 Small seeds** (about 3,200 and 800 km² of
    land a pair), pooled per size, and only where each side of the ratio has at least `LAKE_EVIDENCE_MIN = 10`
    bodies — else the line prints `n/a` and the criterion is recorded as not measurable on that pair; the pooled
    Dev lines are printed and never judged. Fjords: the Medium pair. Mouth and coast classes, the barrier share and
    the names: every size has them (a coast cell exists on every size); judged per seed on Medium and as an 8-seed
    median on Dev.

    | Program criterion | Line(s) | Target | Off switch that must fail it |
    |---|---|---|---|
    | Humid cells have clearly more lakes than arid | `lakes.per_1000km2.humid` ÷ `.arid` (cells by aridity ≥ 1.0 / < 0.5) | ratio ≥ 2 on the Medium pair and on the Small pair (each side ≥ 10 bodies, else `n/a`) | `lake_balance` (arid basins then stand full) |
    | Closed salt lakes only where aridity < 0.5 | `lakes.closed_in_humid` (closed or playa bodies whose mean aridity ≥ 0.5); `lakes.closed` | 0 is the program's reading; closed lakes at aridity 0.5–1 are EXPECTED from the balance (`E > P` ⇔ aridity < `OPEN_WATER_PET_FACTOR`), so the count is quoted with each body's numbers and a non-zero count after Task 21 is a ruling (Task 8); and ≥ 1 closed on the Medium pair | `lake_balance` (`lakes.closed` = 0) |
    | Every river mouth has a class | `mouths.classed` = `mouths`; `mouths.class.*` | equal; at least 2 classes present per Medium seed | — (true by construction: stated, not counted as evidence) |
    | Barrier islands on 5–20 % of coast | `coast.barrier_share` | 0.05–0.20 | `barriers` (0) |
    | Fjords only poleward of the cold stage's ice limit | `mouths.fjords_equatorward` (fjord mouths whose `|latitude|` is under their hemisphere's ice limit); `mouths.class.fjord` | 0 (real evidence: the limit is climatic, Ruling 9 — a tidewater valley glacier equatorward of it counts against); and ≥ 1 fjord over the Medium pair | `cooling` (no fjord) |
    | Named features per world | `features.named`, `features.<kind>`, `features.names_unique` | ≥ 20 on Medium, ≥ 5 on Dev; yes | — |
    | (this plan's) the flood made coast | `sea.coast_km_ratio` (final ÷ initial-mask coast), `sea.drowned_valley_km` | ≥ 1.15 on Medium | `lowstand` (≈ 1.0) |
    | (this plan's) deposition built fans | `sediment.fans`, `sediment.deposited_share` | ≥ 1 per Medium seed; 0.1–0.6 | `deposition` (0) |
    | (this plan's) ice shaped valleys | `ice.u_valley_ratio` | ≥ 1.3 where `ice_max` holds valleys | `glacial` (≈ 1.0) |

**The controller's verdicts on the reduced items (team lead, 2026-11-20, after the builder check).** Each is a
ruling of the run, not of this plan's author; the program allows DEFERRED only where a phase's notes say so, and
Phase 4's do not, so each reduced form below is ACCEPTED as the phase's delivery of its item, with its cost.

15. **Lake balance — accepted as an honest reduced first form** (one balance per depression, lakes in series
    decided with all twelve months: Ruling 5) — cost: no depression hierarchy; sub-basins of one closed depression
    share a level; a river does not cross a closed basin's dry floor.
16. **Lake origins — accepted** (nine values, `Other` included; every lake has one) — cost: oxbow, landslide,
    karst, thermokarst, crater and impact lakes do not exist.
17. **Braided — accepted** (the class, and a wider, shallower channel) — cost: no bars, anabranches, meanders or
    oxbows.
18. **Sediment deposition — accepted ONLY AS REDUCED, and said so:** the deposition term delivers FANS; floodplains
    stay rule-based (`carve_floodplains`) and basin fill stays the depression fill (`depression_fill`). The
    program's "sediment deposition in the stream-power solver (fans, floodplains, basin fill)" is therefore
    delivered for fans only — cost: two of the three named landforms still come from rules; listed under Known
    limits.
19. **Ice sheets — accepted** (a plastic profile over the cells above the equilibrium line, from the monthly mass
    balance) — cost: no flow, no shelves, no areal scour, no isostatic depression.
20. **Valley glaciers — accepted ONLY with each swath cell cut by its own slope** (Ruling 9, Task 10: the uniform
    swath is what the cited source rejects), and with the thickness share pinned at 0.05 against the first-hand
    notes — cost: a distance swath, not the source's catchment swath; no cirque headwalls.
21. **Moraines and kettles — accepted with the TERMINAL (cold-maximum) margin recorded as well as the recessional
    ones** (Ruling 9, Tasks 10–11) — cost: no drumlins, eskers or erratics.
22. **Reefs and atolls — accepted** (fringing reef flats; atolls on subsided hotspot cells) — cost: no barrier or
    patch reefs.
23. **Shores — accepted as heightfield work:** beach and tidal-flat profiles are terrain; the sand and mud COVERS
    are Phase 5's shore strip, and a `Rocky` shore is a NO-OP in this phase (the class is stored; no cliff or
    platform is cut) — cost: until Phase 5 a classed shore looks like any other but for its profile.
24. **"The land clamp goes" — it does NOT fully go, and "a sea about 120 m lower" is not 120 m everywhere; both are
    labelled REDUCED.** The mask is a result (delivered). But the raise to `+SHORE_STEP_M` survives because the
    runtime has no sea mask (Ruling 3), and the low stand is 13–33 m on Dev, 120 m only on Large (Ruling 2: a
    true 120 m drowns a world whose mountains are 300 m) — cost: no land or lake floor below sea level; small
    worlds' rias are small.
25. **Coast classes on Dev — the segment definition is FIXED, not an empty line accepted:** a coast cell is a planet
    cell with a shore in or beside it ("Shared vocabulary"), which exists on every size — cost: on Dev a segment is
    one fine cell (classes may speckle; Ruling 10's majority filter is the fallback).

## Small worlds (what this phase gives each size)

| | Dev 16 × 8 km | Tiny | Small | Medium | Large |
|---|---|---|---|---|---|
| Low stand (Ruling 2) | 13–33 m | 20–51 m | 40–100 m | 80–120 m | 120 m |
| Drowned valleys | a few cells wide, hundreds of metres long | | rias of 1–3 km | rias and fjords of several km | tens of km |
| Lakes per world | a handful (pooled evidence) | | tens | hundreds | thousands |
| Alpine glaciers (Ruling 9) | none expected: ice by latitude | few | some in polar-side belts | yes | yes |
| Ice sheets (≥ 12 planet cells) | ≥ 384 m across: polar land only | | | | ≥ 4.7 km across |
| Coast segment = planet cell | 32 m (one fine cell at 32 m) | 50 m | 98 m | 196 m | 392 m |
| Barrier ridge / lagoon (3 / ≥ 4 cells) | 96 / 128 m at 32 m | | | 48 / 64 m at 16 m | |
| Features (radii 1.4°–21°) | 64 m – 0.96 km | | | 0.39 – 5.9 km | 0.78 – 11.7 km |

## Review Focus

1. **A mask that changed and a field that did not follow.** `ocean`, `outlet`, `sea_level` and the heights at the
   shore move together or routing asserts (or worse, does not): `post::flood` is the only writer after the
   schedule, every stamp ends by calling it, and `sea_is_height` is checked on the ARTIFACT (Tasks 4, 11, 15, 16).
2. **The cold stage carried into the present.** Its sea level, its exposed shelf, its ice and its rain are a
   stage: nothing of the cold world may be stored but what the flood and the retreat left (`World::ice` is replaced
   by present ice; `cold_q`, the cold mask and `ice_max` live on `Forged::cold`, never in `World`) (Tasks 4, 9–11).
3. **Deposition that pumps.** A lagged source can raise a cell above its donor and the next step's routing turns
   round; mass can appear. The prototype's conservation line and the cap are the guard; a forge whose
   `sediment.deposited_share` exceeds 1 is wrong (Task 5).
4. **Units.** `Analysis::q` is m² of average rain; a flow is m³/s; `Qs` is m³/yr; runoff is mm per month. Each
   new function's doc names its unit, and the two meanings of "discharge" never share a variable (Tasks 5–8).
5. **The seam and the roll.** Every new flood fill, chamfer, component label, swath and disc walks through
   `World::step` (or the planet grid's wrapping index); a lake, a delta, a barrier or a named bay across x = 0 is
   one object; `climate.roll_identical` and the seam rule run in every world-changing task (all).
6. **Whole world ≠ tiled world.** The new byte raster, the swapped one, `Segment::wet`, the lake records and
   `WaterRecord` each have a tiled path; a missed plane loads and reads zeros. Task 19's identity lines
   (`water_tiles_identical`, `ice_tiles_identical`) are the check.
7. **Exact arithmetic.** The runtime's new reads are a byte times a constant, a table lookup and two stored `f32`s;
   a `powf` or `sqrt` of a flow belongs to the forge. `game::drainage_of_water_table` is a `const` table (Tasks 19,
   20).
8. **A tuned constant presented as a fact.** Nearly every number of Rulings 5–11 is a starting value; a report
   that calls one "sourced" must name the row of "What was read" it comes from (all).

---

## Slice A — tools; the sea level as a field (no world changes)

### Task 1: Baselines, the switches, the "before" lines

**Files:**
- Create: `crates/worldgen/src/forge/water.rs` (this phase's hydrology helpers; `pub mod water;` in `forge/mod.rs`)
- Modify: `crates/worldgen/src/forge/world.rs` (`ForgeParams` gains `pub features: Features`; `Default`) and the two `ForgeParams {` literals that list every field — `world.rs`'s own `Default` and `crates/client/src/frontend/generator.rs` `params` (`features: Features::default()`); `world-store/src/ctl.rs`, `forge/render.rs` and every example use `..` and need nothing
- Modify: `crates/worldgen/src/forge/climate/derived.rs` only if Phase 3 landed `SNOW_ALL_C`, `SNOW_NONE_C`, `MELT_MM_PER_C_DAY`, `EARTH_DAYS_PER_MONTH`, `SEA_FREEZE_C` private (its plan says `pub(crate)`): widen them, nothing else
- Modify: `crates/worldgen/examples/forge_proto/main.rs` (`--off`), `report.rs` (lines below), `justfile` (the comments of `forge-report`, `forge-proto` and `forge-climate`: the flag and BOTH name sets)

**Interfaces — produces:**

```rust
// world.rs
/// Forge features with no knob to switch them (Ruling 14). Never stored; all on outside `forge_proto --off`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Features { pub lake_balance: bool, pub loss: bool, pub stamps_ice: bool, pub deltas: bool,
                      pub barriers: bool, pub reefs: bool, pub shores: bool }
impl Default for Features { /* all true */ }
impl Features { /// Clears the named feature; false when there is no such name.
                pub fn off(&mut self, name: &str) -> bool; }
// water.rs
/// The sea-level normals of climate cell `c` of the forged FLOAT planes (twelve temperatures, precipitations and
/// the other four planes' values), as the container's sampler would return them at that cell's centre.
pub fn monthly_cell_normals(monthly: &climate::MonthlyFields, c: usize) -> game::MonthlyNormals;
/// A fine field reduced to the planet grid ("Shared vocabulary": centres in the cell, else the cell under its centre).
pub fn to_planet_mean(w: &World, fine: &[f32], only: &dyn Fn(usize) -> bool) -> Vec<f32>;   // NaN where `only` holds nowhere
pub fn to_planet_any(w: &World, pred: &dyn Fn(usize) -> bool) -> Vec<bool>;
/// Phase 3's derived climate of every CLIMATE cell of the world.
pub struct ClimateCells { pub cnx: usize, pub cnz: usize, pub cell_m: f64, pub ground_m: Vec<f32>,
                          pub precip_mm: Vec<f32>, pub derived: Vec<climate::derived::Derived>,
                          /// Share of the cell's precipitation that falls as snow (Phase 3's `SNOW_*` ramp).
                          pub snow_share: Vec<f32> }
impl ClimateCells {
    pub fn new(w: &World, monthly: &climate::MonthlyFields, annual_mm: &[f32], lapse_c_per_m: f64) -> ClimateCells;
    pub fn cell_of(&self, w: &World, i: usize) -> usize;      // the climate cell holding fine cell `i`
    pub fn aridity(&self, c: usize) -> f32;                    // derived[c].aridity
}
pub const COAST_EDGE_FACTOR: f64 = std::f64::consts::FRAC_PI_4;
/// Coast length (km): fine land–sea cell edges (4-neighbours through `World::step`) × dx × COAST_EDGE_FACTOR.
pub fn coast_km(w: &World) -> f64;
```

- `ClimateCells::new`, per climate cell: `ground_m` = the mean `h.max(0)` of its fine LAND cells and `precip_mm` the mean of `annual_mm` over them; `lat_deg = climate::latitude_at` of the cell's centre; `derived = derive(&ground_normals(monthly_cell_normals(monthly, c), ground_m, precip_mm, lapse), lat_deg, false)`. A climate cell with NO land cell: `ground_m = 0`, `precip_mm` the mean over all its cells, `derive(&monthly_cell_normals(..), lat_deg, true)`.
- **`--off <name>[,<name>…]`, one flag, by mode** ("Shared vocabulary"): with `--climate` the names go to Phase 3's `Switches::off` exactly as Phase 3 wrote it; with `--report` or `--out` each name is first looked up in the knob aliases (a knob that does not exist yet is an error naming the task that adds it: `lowstand` works from Task 3, `deposition` from 5, `cooling` from 9, `glacial` from 10, `tide` from 13), else in `Features::off`; an unknown name is refused listing the names of the mode in use.
- `--report` gains, in this order: `land_height_crc` (crc32fast of `h` of every non-ocean cell in index order, LE `f64`), `coast_km`, `sea_is_height` (on the ARTIFACT: `yes` when every cell is `ocean ⇔ stored height < 0` AND no stored height lies strictly between `−SHORE_STEP_M` and `+SHORE_STEP_M`; else `no (<n> cells)` — before Task 4 it prints `no`, with the count of shore cells stored at 0–0.0625: that is the "before"), `lakes.per_1000km2.arid`, `lakes.per_1000km2.humid` (lake bodies whose centre cell's climate cell is arid / humid ÷ the land area of such cells, × 1,000 km²; `n/a` when either side has under `LAKE_EVIDENCE_MIN = 10` bodies), `lakes.bodies.arid` / `.humid`, `lakes.humid_arid_ratio`, `lakes.closed`, `lakes.closed_in_humid` (today: from `fields::closed_lakes`), and in the summary the POOLED lines over the seeds (`lakes.pooled_*`: counts summed ÷ areas summed, the same `n/a` rule).

- [ ] **Step 0: Baselines BEFORE any change** — `just forge-report --size dev --cell 32 --seeds 8 > <ledger dir>/base-32.txt`, the same at `--cell 64` (`base-64.txt`), `just forge-report --size medium --cell 32 --seeds 2 > base-medium.txt` and `--size small --cell 32 --seeds 2 > base-small.txt`. The gate passes.
- [ ] **Step 1: Temporary tests:** `Features::default()` is all on and `off("deltas")` clears only that; `coast_km` of a hand-built 16 × 8 world with one 4 × 4 land square is `16 · dx · π/4 / 1000`, and the same when the square straddles the seam; `monthly_cell_normals` of cell `c` equals `monthly.quantise(true).cell_normals(cx, cz)` within the planes' quantisation steps; `to_planet_mean` on a 512 × 256 world is the identity, on a 1,024 × 512 world the 2 × 2 mean, on a 256 × 128 world each fine value four times; `ClimateCells::new` on the default dev world has no NaN and a climate cell with no land has `sea_ice_months` from the sea branch.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the four reports again, saved as `base1-*.txt`: every line of `base-*` present, unchanged, in order (the five crcs per seed quoted); the new lines per seed. `just forge-report --size dev --cell 32 --off deltas`: the same world (`height_crc`). `--off nonsense`: refused, listing the twelve names. `just forge-climate --world synthetic:ocean --off advection` still runs and `--off deltas` there is refused listing the `Switches` names.
- [ ] **Step 4: Verify, in order** (Global Constraints). Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): feature switches for evidence runs; coast length and lake-by-climate report lines`.

### Task 2: `World::sea_level` — the stand as a field

**Files:** `crates/worldgen/src/forge/world.rs` (`World` gains `pub sea_level: f64`; `World::new` sets 0.0; `upsample_onto`: a coarse sea cell reads `self.sea_level`; the `initial` literal in `forge/mod.rs`), `forge/erosion.rs` (`route_into`: `let level = if w.ocean[i] { w.sea_level } else { w.h[i] }`; **`evolve` as Phase 2's Task 12 left it** — `base = if ocean[rc] { sea_level } …`; the floor in BOTH incision branches (the `threshold == 0` line and the `h_new = (hi + lift − kept)` line) and in BOTH diffusion stencils (the interior loop and the two wrapping columns), each becoming `.max((sea_level + LAND_FLOOR_M).min(before))` with `before` = `hi + lift` for the incision and `diffuse[i]` for the diffusion; the diffusion's shore `s(j) = if ocean[j] { sea_level } …`), `forge/routing_fast.rs` (`OUTLET_LEVEL_M` deleted: `Grid` carries `sea_level`, used in `Grid::level` and in pass 1's `filled[x] = if shore && ocean { … }` — its two sites), every other reader of "the sea is 0" found among the lines that name `ocean` in `forge/{hydrology,post,fields,spawn,render}.rs` and `forge/climate/` — list each in the report as changed (to `w.sea_level`) or left (it means "present sea level" and only ever sees a stand-0 world: say why).

**Interfaces — produces:** `World { …, pub sea_level: f64 }`. **The clamp rule (Ruling 3):** a floor never raises a cell above where it was. At stand 0 nothing changes bit for bit: `0.0 + LAND_FLOOR_M` is `LAND_FLOOR_M` exactly, and `LAND_FLOOR_M.min(before)` is `LAND_FLOOR_M` because no land cell is under it today (initial land ≥ 0.5, `upsample_onto` floors at it, the old clamp held it) — the safety net proves it.

- [ ] **Step 1: Temporary tests:** a hand-built 32 × 16 wrapped world, a ramp to a sea strip: with `sea_level = −50.0` and the sea floor at −80, `route(w, FLOOD_EPSILON).filled` at a shore cell is `−50 + ε · dist` and 40 `erosion::step`s leave every land cell ≥ `−50 + LAND_FLOOR_M` and the lowest land cell below 0 (the floor follows the stand); **a land cell set by hand to −70 (below the stand, as the ice will cut) is at most −70 after 10 steps — never lifted** — with and without a non-zero `threshold` and `d_mult`; `route_fast` and `route_into` give the same `filled` at that stand; with `sea_level = 0.0` the result equals a run of the code before the change bit for bit (keep the old functions as `#[cfg(test)]` copies for the comparison; they go with the test).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the safety net: both dev reports identical to `base1-*` line for line (the five crcs per seed quoted); the gate passes untouched.
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `refactor(forge): the sea level is a field of the world (worlds forge bit-identically)`.

## Slice B — the sea becomes a result

### Task 3: The shelf, and a stand in metres — `World::new_at`

**Files:** `crates/worldgen/src/forge/world.rs` (ADD `World::new_at`; `World::new(p, planet)` becomes `new_at(p, planet, 0.0)`; `World` gains `pub polar_sea: Vec<bool>`; the sea-floor line, the sea rule; `low_stand_for`, `low_stand_world_m`; the constants; `upsample_onto` keeps the fine world's `polar_sea`; the `initial` literal lists it), `forge/knobs.rs` (ADD group "Sea": `low_stand_m`, `shelf_width_deg`), `forge/mod.rs` (`Forged::artifact`: `height` is quantised from `h.max(OUTSIDE_HEIGHT_M)`), `forge/presets.rs` only if a preset's help names the shelf, `crates/client/src/frontend/generator.rs` (`TABS`: the "Sea" group on the Land tab), `crates/panel/src/forge.rs` and `crates/world-store/src/ctl.rs` only where they list groups, `report.rs`.

**Interfaces — produces:**

```rust
pub const LOW_STAND_DEFAULT_M: f64 = 120.0;      // = the `low_stand_m` knob's default; the shelf is shaped by THIS
pub const LOW_STAND_FULL_PEAK_M: f64 = 2_000.0;
pub const LOW_STAND_MIN_M: f64 = 10.0;
pub const SHELF_SHORE_SHARE: f64 = 0.8;
pub const SLOPE_DROP_M: f64 = 150.0;
pub const SHORE_STEP_M: f64 = 0.125;             // two HEIGHT_STEP_M: the stored shore's gap (vocabulary)
/// max(stand_m · min(1, peak_target_m / LOW_STAND_FULL_PEAK_M), min(LOW_STAND_MIN_M, stand_m)): Ruling 2.
pub fn low_stand_for(stand_m: f64, peak_target_m: f64) -> f64;
pub fn low_stand_world_m(k: &Knobs, peak_target_m: f64) -> f64;   // low_stand_for(k.low_stand_m, ..)
impl World {
    /// `stand_m`: the sea's level (m; 0.0 = today's sea, negative = a low stand). A cell is sea when it is polar
    /// sea or its initial height is at or under `stand_m`. `sea_level = stand_m`. NEW in this task (Phase 2 has
    /// only `World::new`, which becomes `new_at(p, planet, 0.0)`).
    pub fn new_at(p: &ForgeParams, planet: &Planet, stand_m: f64) -> World;
}
```

- The polar-sea rule, the floor and the sea rule: "Shared vocabulary". At `stand_m = 0.0` the MASK is Phase 2's exactly (land heights are ≥ 0.5, every floor ≤ −1, a polar-sea cell had `land ≤ 0` already). A shelf cell that is land at a negative stand: `h = h0`, `uplift = 0`, `uplifted = 0`, `outlet = false`, and whichever of Phase 2's `k_mult`, `d_mult`, `threshold` exist at their neutral 1, 1, 0; `province` = the sampler's, with `PROVINCE_OCEAN` read as `PROVINCE_PLATFORM` (the shelf is the continent's edge). `margin_drowned` counts as today.
- `--report`: `sea.low_stand_m`, `sea.shelf_width_m`, `sea.shelf_exposed_share` — counted IN PLACE on the final level's initial world (sea cells that are not polar sea and whose `h` is above `−low_stand`, ÷ all cells: a sea cell's `h` is its initial floor; no second `World` is built), `sea.polar_sea_share`, `sea.floor_min_m` (forge side), `sea.stored_floor_min_m` (−90 or shallower).

- [ ] **Step 1: Temporary tests** (small grids only — a 128 × 64 world, never the finest): `new_at(p, planet, 0.0).ocean` equals the pre-change `World::new(p, planet).ocean` cell for cell and every LAND `h`, `uplift` is bit-equal (keep the old `new` as a `#[cfg(test)]` copy; it goes with the test); **for 8 seeds `new_at(.., −low_stand)` has rows 0 and `nz − 1` all sea, every border cell an outlet, and `erosion::route` and one `erosion::step` run on it without an assert** (the pole at a low stand); it has strictly more land, and every newly exposed cell has `h` in `(−low_stand, −1]`, `uplift == 0` and is not `polar_sea`; the floor is monotone seaward along 64 random rays outside the margin band; `low_stand_for(120, 218)`, `(120, 2_000)`, `(120, 2_800)` are 13.08, 120, 120 and `low_stand_world_m` with `low_stand_m = 0` is 0; with `--set low_stand_m=0` the sea floor is bit-identical to the default's (the shelf is shaped by the constant); the artifact's stored height is never under −90.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — both dev reports: `land_height_crc` identical per seed to `base1-*`; the exempt lines moved (Global Constraints' list for Task 3: quote old → new); no other old line moved; the new lines quoted; the gate passes untouched. `just forge-proto --size dev --cell 32 --out target/world-forge/p4/t3`: READ `t3-relief.png` (the shelf as a paler rim).
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): a shelf with a slope beyond it; World::new_at — the sea's stand in metres`.

### Task 4: The cold stage at the low stand, and the flood

**At dispatch the controller tells the owner:** from this commit the quality gate fails until Task 24, and every newly forged world has a flooded coast.

**Files:**
- Modify: `crates/worldgen/src/forge/mod.rs` (`schedule`'s final level runs `refine_iterations − cold_stage_iterations` (at least 1); `forge_with`: stages 3–8 of the stage list; `Forged` gains `pub cold: ColdStage`; **the module doc carries the stage list**; `ForgeStage` gains `ColdStage` with label "Ice age" — its iterations count in `iters`), `forge/world.rs` (`World::restand`), `forge/post.rs` (`flood`), `forge/erosion.rs` (`StepScratch::discharge`), `forge/knobs.rs` (ADD "Schedule" `cold_stage_iters`; `Knobs::cold_stage_iterations()`), `crates/client/src/frontend/generator.rs` (its progress `match` on `ForgeStage` has an arm per stage: add `ColdStage` — the erosion share of the bar covers it, as `Eroding`'s arm does), `forge/play.rs` only if the estimate's comment names the schedule
- Modify: `report.rs`, and Phase 3's `climate.roll_identical` helper there: `planetary_for` is called on the rolled `Forged::initial` mask (what the forge fed it), `fine::rain` on the rolled final world; `CLAUDE.md` (Worldgen paragraph: the sea is flooded after a cold stage; the stored-sea invariant), `TODO.md` (re-forge; the gate is red until the phase closes)

**Interfaces — produces:**

```rust
// world.rs
impl World {
    /// Re-stands this world IN PLACE at `stand_m` (≤ the current stand): every sea cell that is not `polar_sea`
    /// and whose `h` (its initial floor: outlets are never eroded) is above `stand_m` becomes land — `ocean` and
    /// `outlet` false; `h`, `uplift` (0), `uplifted` (0), the Phase 2 fields and `deposit` / `ice` (later tasks)
    /// as they are; province `PROVINCE_OCEAN` → `PROVINCE_PLATFORM`. Old land is untouched. `sea_level = stand_m`.
    /// The result's mask equals `new_at(p, planet, stand_m).ocean`. Returns the cells exposed.
    pub fn restand(&mut self, stand_m: f64) -> usize;
}
// erosion.rs
impl StepScratch { /// The last step's discharge (`Analysis::q`'s unit), or `None` before the first step.
                   pub fn discharge(&self) -> Option<&[f64]>; }
// post.rs
#[derive(Clone, Copy, Default)]
pub struct FloodStats { pub flooded_cells: usize, pub sealed_cells: usize, pub raised_cells: usize, pub lowered_cells: usize, pub sea_cells: usize }
/// Makes the stored-sea invariant at `level` (vocabulary). A cell is floodable when `h <= level`; sea = the seed
/// cells and every floodable cell 4-connected (`World::step`) to one through floodable cells. `ocean` and `outlet` are set to
/// it and `sea_level = level`. Then every sea cell above `level − SHORE_STEP_M` is LOWERED to it, and every other
/// cell under `level + SHORE_STEP_M` is RAISED to it — among them a pocket that was sea and is no longer connected
/// (`sealed_cells`). Idempotent. Seeds are walked in index order; the result does not depend on the order.
pub fn flood(w: &mut World, level: f64, seed: &[bool]) -> FloodStats;
// mod.rs
pub struct ColdStage {
    pub low_stand_m: f64,
    /// The SEED sea: the cold stage's mask (the last level's own mask when the stage did not run).
    pub ocean: Vec<bool>,
    /// The cold stage's last discharge (`Analysis::q`'s unit); empty when the stage did not run.
    pub q: Vec<f32>,
    /// From Task 10: cells that carried ice in the stage, and the ice edge at the end of each of its four stands.
    pub ice_max: Vec<bool>, pub margins: [Vec<bool>; 4],
    /// The sums over every flood of the forge.
    pub flood: post::FloodStats,
}
```

- **`forge_with`, stages 3–8** (`w` = the last level's world, at stand 0; `land0` = a copy of its mask, kept for `sea.land_lost_share` — one `Vec<bool>`): drop the level's `Geology`; `w.restand(−low_stand_world_m(k, target))`; `seed = w.ocean.clone()`; `geo = Geology::new(p, &w)` — THIS geology is `Forged::geology` and feeds `analyze` and `fields` (it knows the shelf's rock); a fresh `StepScratch` and `ReboundState::new(&w)`; `rain` from `fine::rain(&w, …)` (the present-day planetary planes: Ruling 3), refreshed every `RAIN_REFRESH_ITERS` as in a level; `cold_stage_iterations()` × `erosion::step`; `cold.q` from `scratch.discharge()`; `post::flood(&mut w, 0.0, &seed)` (stage 5); `fit_height_band`, `carve_floodplains`, `carve_lake_basins` until Task 8 (stage 6); `post::flood` again (stage 7); the stored climate (stage 8); `analyze`; `fields`. With `cold_stage_iterations() == 0` stages 3–4 are skipped, `seed` is the last level's mask, and **both floods still run** (they make the invariant: shore sea cells are lowered to `−SHORE_STEP_M`, low land raised to `+SHORE_STEP_M`). With `low_stand_m = 0` the stage runs at stand 0 (the ice needs it) and `restand` exposes nothing.
- `--report`: `sea.cold_iters`, `sea.flooded_cells`, `sea.sealed_cells`, `sea.raised_cells`, `sea.lowered_cells` (each the sum over the forge's floods), `sea.land_lost_share` (`land0` cells that are sea now ÷ `land0`), `sea.coast_km_initial` (`coast_km` of `Forged::initial`), `sea.coast_km_ratio` (`coast_km` ÷ it), `sea.drowned_valley_km` (cells that are sea now, were land in the seed mask and had `cold.q ≥ river_min_discharge_m2`, × `dx`), `sea.islands_over_shelf` (land components wholly inside `Forged::initial`'s sea), `sea.model_mask_mismatch` (Ruling 3). `land_share`, `polar_rows_all_sea`, `drains_to_sea` and every other existing line read the FINAL world.

- [ ] **Step 0: Before** — the four reports saved (`t4-before*.txt`).
- [ ] **Step 1: Temporary tests:** `flood` on a hand-built wrapped world — a seed sea strip, a land barrier with a 1-cell gap at height −2, a basin at −5 behind it, a second basin at −5 with no gap, and a third at −5 that is marked `ocean` but is not connected: the first basin is sea, the second is land raised to `+SHORE_STEP_M`, the third is `sealed` and raised too; a shore sea cell at −0.01 is lowered to `−SHORE_STEP_M` and a land cell at 0.05 raised to `+SHORE_STEP_M`; `ocean == outlet`; a second call returns all-zero counts but `sea_cells`; a gap that touches only diagonally does not flood; the world rolled by half its width floods to the rolled result. `restand(−s)` on a 128 × 64 world gives the mask of `new_at(p, planet, −s)` cell for cell, leaves old land's `h`, `uplift`, `uplifted` bit for bit, and rows 0 and `nz − 1` sea. On the default dev world after `forge`: `sea_is_height: yes` on the artifact; the seed mask ⊂ the final sea (true in THIS task: from Task 15 delta lobes may stand on the old shelf, so the check is not kept); the quantised height raster has no value strictly between `−SHORE_STEP_M` and `+SHORE_STEP_M`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the four reports (`t4-after*.txt`). The standing invariants on every seed (`climate.roll_identical: yes` with the amended helper — a `no` here is the D23 defect, BLOCKED). The before / after table, 8 dev seeds and 2 Medium: `land_share`, `sea.land_lost_share`, `coast_km`, `sea.coast_km_ratio`, `sea.drowned_valley_km`, `river_mouths`, `river_km`, `max_strahler`, `lakes`, `height_p50_m`, `concavity_theta`, `sea_is_height` (`no` → `yes`). **Targets:** `sea.coast_km_ratio` ≥ 1.1 on the Medium pair's mean (a first look: Task 21 tunes to 1.15 — 30 steps of 2,500 years against a response time near a million years cut knickpoints only a few cells inland on small rivers, so a miss here is NAMED for Task 21, whose dials are `cold_stage_iters` and the stand, not blocked); `sea.land_lost_share` ≤ 0.15 on every seed; `land_share` in 0.15–0.6. The same Medium run with `--off lowstand`: `sea.coast_km_ratio` within 0.97–1.03 (the line fails with the feature off). The gate run WITHOUT bless: quote its message. `just forge-proto --size medium --cell 32 --out target/world-forge/p4/t4`: READ `t4-relief.png` and say in two sentences whether valleys reach inland as water. Peak memory: `just forge-bench 16384 8192 32` before / after (no second world: the RSS must not grow by more than the masks).
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge)!: the sea is a result — a cold stage at a lower stand, then the flood`.

## Slice C — sediment, flows, lakes

### Task 5: Sediment deposition in the stream-power step (prototype first)

**Files:** `crates/worldgen/src/forge/erosion.rs` (`StepScratch` gains `qs: Vec<f64>`; `evolve`; `pub fn sediment(scratch: &StepScratch) -> &[f64]`), `forge/world.rs` (`World` gains `pub deposit: Vec<f32>` — cumulative deposited thickness, m; `new_at` zeroes it; `upsample_onto` samples it bilinearly like `uplifted`; `restand` leaves it; the `initial` literal), `forge/knobs.rs` (ADD "Erosion" `deposition`), `forge/mod.rs` (`Forged` gains `pub sediment: Vec<f32>` — m³/yr through each cell at the end of the last erosion iteration), `forge/render.rs` (the `-water.png` sheet's first layer: deposit thickness), `report.rs`.

**Interfaces — produces:** `pub const DEPOSIT_SWEEPS: usize = 1; pub const DEPOSIT_MAX_M_PER_STEP: f64 = 2.0;` and in `evolve`, with `g = k.deposition`:
- **the incision, in Phase 2's two branches** (D5): `let dep = (dt * g * qs_prev[i] / q_eff).min(DEPOSIT_MAX_M_PER_STEP);` (`qs_prev` = `scratch.qs`, the sediment that ENTERED the cell in the previous step, m³/yr; `q_eff` = the `q[i].max(k.hillslope_area_m2 * rain[i])` already under the root) and `let h_star = (hi + lift + dep + f * base) / (1.0 + f);`. `threshold[i] == 0.0`: `h_new = h_star.max(floor)`. Else: `let cut = hi + lift − h_star; h_new = if cut > 0.0 { (hi + lift − (cut − dt * threshold).max(0.0)).max(floor) } else { h_star }` — an aggrading cell keeps its deposit whole; Phase 2's `cut.max(0.0)` must not be applied before the sign test. `floor` is Task 2's `(sea_level + LAND_FLOOR_M).min(hi + lift)`. A cell under a lake (`depth > 1e-3`) takes no `dep` (its silting stays `depression_fill`);
- after the incision, one upstream-first pass over each tree group (the discharge pass's shape, same `Disjoint` rules) builds THIS step's flux into a fresh buffer `qs_now` (zeroed first): `e = (hi + lift − h_new) · dx² / dt` (m³/yr; negative where the cell aggraded); `out = if drowned { 0.0 } else { (e + qs_now[i]).max(0.0) }` — `qs_now[i]` being what this step's upstream cells have already passed in; `qs_now[rc] += out` for a land receiver. Then `scratch.qs = qs_now` (the next step's `qs_prev`). The solve reads the PREVIOUS step's inflow and the rebuild sums the CURRENT one: that one-step lag is the whole approximation. `deposit[i]`: first `deposit[i] = (deposit[i] − (hi + lift − h_new).max(0.0)).max(0.0)` (a later cut removes what lay there), then `deposit[i] += (h_new − (hi + lift)).max(0.0)` (the net rise above where uplift alone would put the cell) — the thickness still lying there. Across levels `upsample_onto` samples `deposit` bilinearly beside a bilinear `h`: an approximation of "still lying there" at each refinement, accepted.
- **the two sums** behind `sediment.deposited_share`, kept on the scratch as `f64` and added over every step of the forge by a serial pass in routing order: `eroded_m3 += Σ (hi + lift − h_new).max(0.0) · dx²`, `deposited_m3 += Σ (h_new − (hi + lift)).max(0.0) · dx²` over non-drowned land cells. Each step's deposit is bounded by what the previous step eroded upstream, so `deposited_m3 ≤ eroded_m3` up to one step's lag: a share above 1 at the end of a forge is a bug.
- `g == 0.0` skips both (bit-identical to before: the safety check below).
- `--report`: `sediment.deposited_share` (Σ `deposit` · dx² over land ÷ Σ eroded volume: tracked as two `f64` sums on the scratch across the whole forge), `sediment.aggraded_land_share` (land cells with `deposit ≥ 1 m`), `sediment.deposit_p99_m`, `sediment.to_sea_km3_per_my` (Σ `Forged::sediment` at cells whose receiver is sea), `sediment.fans` (river-network cells where the mean channel slope over the 5 cells upstream is ≥ 3 × that over the 5 downstream AND `deposit ≥ 1 m`, counted once per 20-cell window).

- [ ] **Step 1: The prototype, as temporary tests** — run once with every `threshold` 0 and once with a non-zero `threshold` over the plain (Phase 2's second branch: the fan must survive it) — on a hand-built 256 × 64 wrapped world (64 m cells): a block of uplift 5·10⁻³ m/yr over the northern 24 rows, a flat plain of 30 rows with no uplift, a sea strip; 300 steps. (a) `g = 0`: `deposit` is all zero and `h` equals the pre-change `evolve` bit for bit; (b) `g = 0.5`, one lagged sweep: no cell's `h` changes sign of its step-to-step difference more than 5 times in the last 100 steps at the range front (no oscillation), total deposited volume ≤ total eroded volume at every step, the plain's first 5 rows stand at least 2 m above the `g = 0` run (a fan), and the trunk slope ratio across the front is lower than in (a); (c) the same with 8 in-step Gauss–Seidel sweeps (a throw-away loop in the test): the final `h` of (b) is within 10 % of (c)'s relief over the fan. Quote all three. If (b) oscillates or (c) differs by more: set `DEPOSIT_SWEEPS = 2` (the second sweep re-solves with the first's `Qs`) and say so under Rulings.
- [ ] **Step 2: Implement** in `evolve` (both branches).
- [ ] **Step 3: Evidence** — `--set deposition=0` on the 8 dev seeds: every OLD line equals `t4-after.txt` (this task appends lines; `height_crc` per seed quoted). Then the default: the before / after table (Task 4's lines plus this task's; 8 dev, 2 Medium). **Targets:** `sediment.deposited_share` 0.05–0.7 and ≤ 1 on every seed (above 1 is BLOCKED); `sediment.fans` ≥ 1 on each Medium seed; `max_peak_m` within ± 15 % of before (the rate fit absorbs the rest: quote `uplift_fit` before / after); the standing invariants. READ `t5-water.png` on Medium: deposits at range fronts and in valley floors, none on ridges. Time: `just forge-bench 16384 8192 32` before / after (quote).
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): sediment deposition in the stream-power step (fans, valley fill)`.

### Task 6: Monthly discharge, loss in dry channels, regimes, channel size

**Files:** `crates/worldgen/src/forge/water.rs` (`flows`), `forge/hydrology.rs` (`River` gains `flow_m3s: [f32; 12]`, `regime: Regime`, `depth_m: f64` — every `River {` and `forged::RiverReach {` construction site follows; `width_m` from the flow; `Analysis` gains `climate_cells: ClimateCells`, `flow_mean: Vec<f32>`, `flow_max: Vec<f32>`; `analyze`'s signature gains what it needs — the monthly fields, the annual raster, the lapse, `&Features`; `find_rivers`; `Stats`; `print_report`; `objects_json`), `forge/knobs.rs` (ADD "Rivers" `river_width_coeff`, `river_depth_coeff`; DELETE `river_width_m`), `forge/presets.rs` (any override of `river_width_m`), `forge/mod.rs` (`forge_with`'s `analyze` call; `Forged::artifact`: `RiverReach::width_m` from the new width; a reach that is not wet is still written — the runtime of this commit fills it; Task 20 dries it), `forge/spawn.rs` (`RIVER_DISCHARGE_M2` stays on `Analysis::q`), the knob mirrors (`generator.rs`, `panel/src/forge.rs`, `ctl.rs` `USAGE`), `report.rs`, `forge/render.rs` (the `-water.png` sheet: rivers coloured by regime).

**Interfaces — produces:**

```rust
// water.rs
pub const SECONDS_PER_MONTH: f64 = 365.25 * 86_400.0 / 12.0;
pub const FLOW_MIN_M3S: f32 = 0.01;
pub const FLOW_PLAYED_MIN_M3S: f32 = 0.02;
pub const LOSS_WIDTH_FACTOR: f64 = 10.0;
pub const NIVAL_SNOW_SHARE: f32 = 0.5;
pub const CHANNEL_DEPTH_MIN_M: f64 = 0.3;
pub const CHANNEL_DEPTH_MAX_M: f64 = 8.0;
#[repr(u8)] pub enum Regime { Dry = 0, Ephemeral = 1, Intermittent = 2, Glacial = 3, Nival = 4, Pluvial = 5 }
/// What the flow sweep leaves: per fine cell the mean and the largest of its twelve flows (m³/s), and the twelve
/// flows at each cell of `keep` (reach ends, depression outlets), in `keep`'s order.
pub struct Flows { pub mean: Vec<f32>, pub max: Vec<f32>, pub kept: Vec<[f32; 12]> }
/// ONE upstream-first sweep of `routing.order` carrying `[f32; 12]` per LAND cell (a buffer indexed by position in
/// the order — 48 B per land cell, freed on return). Per cell: its twelve sources (its climate cell's `runoff_mm`
/// — a lake cell too: Ruling 5) plus what its donors passed; then, month by month, less the channel loss on the
/// way to its receiver when `loss` (Ruling 6). At a depression's outlet cell `at_outlet(cell, &mut twelve)` may
/// rewrite the twelve flows before they are passed on (Task 8's lake balance; a no-op until then). Because the
/// order is upstream-first, every lake above a cell has been decided before the cell is reached.
pub fn flow_sweep(w: &World, routing: &Routing, cc: &ClimateCells, k: &Knobs, loss: bool, keep: &[u32],
                  at_outlet: &mut dyn FnMut(usize, &mut [f32; 12])) -> Flows;
pub fn regime(flow: &[f32; 12], snow_share: f32, ice_share: f32) -> Regime;
pub fn channel_width_m(q_form: f64, k: &Knobs) -> f64;
pub fn channel_depth_m(q_form: f64, k: &Knobs) -> f64;
```

- `analyze` runs `flow_sweep` ONCE (not twelve passes: lakes in series need all twelve months at each outlet — Ruling 5), keeping per cell `flow_mean` and `flow_max`, per reach its twelve values at its last cell (`River::flow_m3s`), and — one more accumulation of two sums — per reach the catchment's precipitation-weighted `snow_share`. `regime(.., ice_share = 0.0)` until Task 10. From Task 15 the forge analyses twice (stages 11 and 17): the sweep and the sums are paid twice ("Cost").
- `--report`: `rivers.reaches`, `rivers.wet_reaches`, `rivers.regime.<dry|ephemeral|intermittent|glacial|nival|pluvial>` (shares of reaches), `rivers.ending_inland` (wet reaches whose downstream reach is not wet and is not the sea or a lake), `rivers.reach_sea_share` (trunk mouths that are wet ÷ `river_mouths`), `rivers.flow_max_m3s`, `rivers.width_p50_m`, `rivers.width_max_m`, `rivers.depth_max_m`, `rivers.runoff_ratio` (Σ flow at mouths over a year ÷ Σ precipitation on land).

- [ ] **Step 1: Temporary tests:** a hand-built 3-cell-wide valley of 100 cells to the sea, a `ClimateCells` literal with 100 mm of runoff every month everywhere: the mouth's flow (every month of the sweep's twelve) is `cells · 0.1 · dx² / SECONDS_PER_MONTH` within 10⁻⁹ relative, every month, with `loss` off, and the same with `loss` on when PET equals AET; with a deficit of 150 mm a month over the lower half and runoff only in the upper tenth, the flow falls downstream and reaches 0 before the mouth, never negative; `regime`: twelve zeros → `Dry`, seven months under `FLOW_MIN` → `Ephemeral`, two → `Intermittent`, none with `snow_share` 0.6 → `Nival`, with 0.1 → `Pluvial`; `channel_width_m(100.0, &default)` is 45.0 and `channel_depth_m` 1.893 (± 0.001); a wrapped world rolled by half its width gives the rolled flows.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the before / after table (8 dev, 2 Medium): the gate's lines (the network is unchanged: `river_mouths`, `river_km`, `max_strahler` must be IDENTICAL to Task 5's — a change is BLOCKED), this task's lines. **Targets:** `rivers.runoff_ratio` 0.15–0.6; on the Medium pair at least three regime classes present and `rivers.ending_inland` ≥ 1 on a seed whose `koppen.B` share is above 0.2; with `--off loss`, `rivers.ending_inland` is 0 on that seed (the line fails with the feature off). `rivers.width_max_m` quoted against Task 5's widest reach (the look changes: say by how much). READ `t6-water.png`: blue perennial trunks, dry reaches in the dry belts.
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): twelve monthly flows per reach — snow, dry-channel loss, regimes; width and depth from flow`.

### Task 7: Braided reaches and waterfalls

**Files:** `crates/worldgen/src/forge/hydrology.rs` (`River` gains `pattern: Pattern`; `find_rivers`: the braid test, the knickpoint scan, the unsmoothed fall pair; `Analysis` gains `waterfalls: Vec<Waterfall>`), `forge/water.rs` (the constants), `forge/mod.rs` (`Forged::artifact`: a braided reach's stored `width_m` is × `BRAID_WIDTH_FACTOR`), `report.rs`, `forge/render.rs` (falls as dots, braided reaches as a doubled line).

**Interfaces — produces:**

```rust
pub const BRAID_SLOPE_COEFF: f64 = 0.0125;   // Leopold & Wolman's line in SI: the research note's arithmetic, not read
pub const BRAID_SLOPE_EXPONENT: f64 = -0.44;
pub const BRAID_LOAD_MIN: f64 = 2.0e-4;      // m³/yr of sediment per m² of rain-weighted area: a starting value, Task 21
pub const BRAID_WIDTH_FACTOR: f64 = 2.5;
pub const BRAID_DEPTH_FACTOR: f64 = 0.5;
pub const FALL_MIN_DROP_M: f64 = 4.0;
pub const FALL_STEP_RATIO: f64 = 4.0;       // a fall is a step this many times the reach's local gradient
pub const FALL_WINDOW_CELLS: usize = 3;
pub const FALL_RUN_M: f64 = 2.0;
#[repr(u8)] pub enum Pattern { Single = 0, Braided = 1, Distributary = 2 }
pub struct Waterfall { pub x: f64, pub z: f64, pub drop_m: f64, pub flow_m3s: f32, pub reach: u32 }
```

- A **fall** is a knickpoint RELATIVE to its reach, not a steep reach: two consecutive cells of a WET reach, neither in a lake, whose water surfaces (`routing.filled`) differ by `drop ≥ FALL_MIN_DROP_M` AND `drop ≥ FALL_STEP_RATIO ×` the mean per-cell drop over the `FALL_WINDOW_CELLS` cell pairs upstream and the `FALL_WINDOW_CELLS` downstream of the pair (fewer at a reach's end; a pair with no neighbour pair at all is not a fall). An absolute 4 m between two cells is a 12–25 % slope at 32–16 m cells, which every headwater of a Medium belt has: the ratio is what keeps `rivers.falls` to steps. The reach's point list gets, in place of that pair, two points `FALL_RUN_M` apart about the pair's midpoint, along the flow, carrying the upper and the lower water and bed; Chaikin runs separately on the pieces before and after, never across the pair.
- **What the runtime makes of it** (stated; Known limits): the stored ground is interpolated over 16–32 m cells, so the pair sits on a ramp, not a cliff. `refine` cuts each segment's bed below ITS water and raises a levee ring to `water + FREEBOARD_M` within `LEVEE_WIDTH_M` of ITS channel, so the upper water is held by its own levee up to the pair and the lower channel starts one block-snapped stair below: a fall reads as a 2 m chute with a one-stair step of water as tall as the drop, banked — not a free-falling sheet over a rock lip.
- **The seam** (D28, Phase 1a's residual: "a 9 m riffle 12–20 m east of the artifact seam on one crossing, where the sampler itself is dry"): `--report` gains `rivers_seam_columns` — for every reach that crosses x = 0 (the reaches behind `rivers_across_seam`), the count of them, and `rivers_seam_columns_max_gap_m`: along each such reach, sampled every 4 m of its stored polyline for 96 m either side of the seam through GENERATED block columns (`generate_region_forged` of the two regions, not `ForgedWorld::sample`), the longest run of samples of a wet reach with no water column. This task rebuilds the reach points, so it prints the line before and after.
- `--report`: `rivers.braided_share`, `rivers.falls`, `rivers.fall_max_drop_m`, `rivers.falls_in_ice_max_share` (0 until Task 10).

- [ ] **Step 1: Temporary tests:** a hand-built reach falling 0.5 m a cell with a 12 m step between two cells: one `Waterfall` with `drop_m == 12`; the same reach falling 4 m EVERY cell (a steep chute) has none; a 12 m step on it has none either (12 < 4 × 4); the stored points hold two points 2 m apart with waters 12 m apart and no point between them in height; a 3 m step gives none; a dry reach gives none; a steep reach with `Forged::sediment` above the load line is `Braided`, the same reach with no sediment is `Single`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the table (8 dev, 2 Medium): `rivers.falls` ≥ 1 on each Medium seed (the cold stage's incision front and rock steps make them; none anywhere is a finding: say where the profile's largest step is); `rivers.braided_share` quoted (no target before Task 21). `river_km` may move by the fall pairs only (under 0.1 %). `rivers.falls` per 100 km of wet channel is quoted: above 20 on a Medium seed means the ratio is too low (say so for Task 21). **The seam:** `rivers_seam_columns_max_gap_m` before / after on the 8 dev seeds and on the Phase 1a seed that showed the riffle (`…020D` and the seeds with `rivers_across_seam` ≥ 1): state whether a gap reproduces with the rebuilt reaches, and where (a gap above 8 m on a wet reach is a finding: fixed here if it is the reach data, named for Task 20 if it is the sampler). `just forge-proto --size dev --cell 32 --out target/world-forge/p4/t7` then `just world-map --world target/world-forge/p4/t7.world --tiles --out target/world-forge/p4/t7-map.png`: the world loads and maps (the format is untouched).
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): braided reaches by slope and load; knickpoints stored as waterfalls`.

### Task 8: Lakes by water balance; the carved bowls go

**Files:** `crates/worldgen/src/forge/water.rs` (`lake_balance`), `forge/hydrology.rs` (`Lake` gains `kind`, `origin`, `spill_level`, `level_range_m`, `inflow_m3s`, `aridity`, `cells: Vec<u32>`; `Analysis` gains `basin_depth: Vec<f64>` (the depth under the SPILL-level flood: `> 0` marks a depression's cells — all of them, the shallow rim included — and the hypsometry is taken over them) and keeps `lake_depth` as the depth under the BALANCE level; `find_lakes`; `analyze`: bodies first, then the flows with the lake hook, then rivers; `find_rivers`: a network cell that is basin but not lake is "in lake" for the object — the reach's points there are flagged `in_lake` so no channel is carved across a dry basin floor; `Stats::lake_count` = `Open` + `Closed`), `forge/post.rs` (DELETE `carve_lake_basins` and its four constants), `forge/knobs.rs` (DELETE `lake_basins`, `lake_spacing_m`, `lake_radius_max_m`, `lake_depth_max_m`), `forge/presets.rs` ("Lowlands", "Dry" and "Wet" each override `lake_basins` — 24, 3, 18: all three go, and "Lowlands"' description loses its "many lakes"), `forge/mod.rs` (`forge_with`: the `carve_lake_basins` call and its printout go; `Forged::lake_bodies` no longer floods for itself (`erosion::route(w, 0.0).filled` is the SPILL level): it writes one stored `Lake` per `Analysis` lake from that lake's own `level` and `cells`, as row runs split at the seam as today; playas are not written until Task 19), `forge/fields.rs` (`salinity` takes the closed and playa cells from the analysis; DELETE `closed_lakes`, `ENDORHEIC_INFLOW_RATIO`), `forge/climate/` (`lakes_from_filled` callers: unchanged — during erosion a basin is a lake at its spill), the knob mirrors, `report.rs` (`lakes.closed*` re-pointed to the analysis; lines below), `forge/render.rs` (lakes by kind on `-water.png`), `CLAUDE.md` (Worldgen paragraph: lakes by balance; no carved bowls), `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` (Decision 4's Phase 4 line: as built).

**Interfaces — produces:**

```rust
pub const OPEN_WATER_PET_FACTOR: f64 = 1.0;
pub const PLAYA_MAX_DEPTH_M: f64 = 0.5;
pub const COASTAL_LAKE_REACH_M: f64 = 500.0;
#[repr(u8)] pub enum LakeKind { Open = 0, Closed = 1, Playa = 2 }
#[repr(u8)] pub enum LakeOrigin { Other = 0, Basin = 1, Fluvial = 2, Coastal = 3, Tectonic = 4, Volcanic = 5,
                                  Glacial = 6, Moraine = 7, Kettle = 8 }
pub struct Balance { pub kind: LakeKind, pub level: f64, pub area_m2: f64, pub outflow_m3_per_yr: f64 }
/// Ruling 5. `floors`: the floors of EVERY cell of the depression (`basin_depth > 0`), ascending; `spill`: its spill level; `w_m3_per_yr`: catchment
/// inflow; `e_mm`, `p_mm`: the body's annual open-water evaporation and precipitation.
pub fn lake_balance(floors: &[f64], cell_area_m2: f64, spill: f64, w_m3_per_yr: f64, e_mm: f64, p_mm: f64) -> Balance;
```

- **The order** (Ruling 5): `analyze` finds the depressions and their outlet cells first (`find_lakes` on the spill-level flood), then runs `flow_sweep` with an `at_outlet` that, at each depression's outlet cell, sums the twelve inflows, calls `lake_balance`, records the lake and rewrites the twelve flows to the lake's outflows; the routing's upstream-first order IS the lake order (a lake's outlet comes after everything that drains to it), so lakes in series are decided in one sweep with all twelve months each. Then rivers.
- `level_range_m`, the monthly outflows and the lake's own cells' runoff: Ruling 5's formulas.
- With `features.lake_balance == false` every body is `Open` at its spill (today's lakes).
- Origins: Ruling 5's order with the three ice origins unreachable until Task 11.
- `--report`: `lakes.open`, `lakes.closed`, `lakes.playas`, `lakes.closed_in_humid`, `lakes.origin.<name>`, `lakes.area_share` (lake cells ÷ land cells), `lakes.level_range_max_m`, and Task 1's density lines now from these bodies (a playa is not a lake in them).

- [ ] **Step 1: Temporary tests:** `lake_balance` on a cone-shaped basin (floors from a known area–height law): `E ≤ P` → `Open` at the spill; `W = 0`, `E > P` → `Playa`; a `W` that gives `A* = ` half the spill area → `Closed` with `area_m2` within one cell of it and `level` at that area's floor; `A* ≥ A(spill)` → `Open` with outflow `W − (E − P) · A`; never NaN at `E == P`. On a hand-built world of THREE basins in series under a dry climate literal with a wet season of three months upstream: the upper is `Closed` and passes nothing; with a wetter literal it is `Open`, its twelve outflows are its inflows less each month's net evaporation, and the middle and lower lakes' verdicts equal those of a by-hand balance on those twelve numbers (the sweep decided them in order, with every month). `SPECS` has no knob named `lake_basins`; no preset overrides a deleted knob.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the table (8 dev, 2 Medium, 2 Small; densities judged on the Medium and Small pairs): `lakes`, `lakes.open` / `.closed` / `.playas`, the densities and their ratio, `lakes.closed_in_humid`, `lakes.origin.*`, `biome_share.*` (salt flats and lake cells move), the gate's lines. **First look (Task 21 tunes):** `lakes.closed_in_humid` is QUOTED per seed with each counted body's `E`, `P`, aridity and `W` — it is not a blocker: with `E = OPEN_WATER_PET_FACTOR · PET` and `P = MAP`, a lake can close exactly where aridity < 1, so closed lakes on small catchments at aridity 0.5–1 are what the balance predicts, and the program's "only below 0.5" may MISS (Task 21 judges it; the dial is the factor, the fallback a ruling). What IS blocked: a `Closed` or `Playa` body whose own `E ≤ P`. Densities are judged on the Medium and Small pairs (Ruling 14; `n/a` under 10 bodies a side) — the dev lines are printed only;
`lakes.humid_arid_ratio` quoted; with `--off lake_balance`: `lakes.closed` = 0. If `lakes` is 0 on more than half the dev seeds, say so plainly (Ruling 5: no bowls are added; Tasks 10–11 and 21 are what may change it). READ `t8-water.png` on Medium.
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge)!: lakes by water balance — open, closed and playa; the carved lake basins and their knobs removed`.

## Slice D — ice

### Task 9: The ice climate — equilibrium lines and sheets on the planet grid

**Files:** Create `crates/worldgen/src/forge/ice.rs` (`pub mod ice;`); Modify `forge/knobs.rs` (ADD group "Ice": `ice_age_cooling_c`), `forge/mod.rs` (`forge_with`: `IceClimate::compute(.., cold = true)` at the start of stage 4 — Task 10 reads it there — and `compute(.., cold = false)` at stage 9; `Forged` gains `pub ice_climate: ice::IceClimate`), the knob mirrors, `report.rs`, `forge/render.rs` (`-ice.png`: the two equilibrium lines as bands, the sheets).

**Interfaces — produces:**

```rust
pub const ELA_MAX_M: f64 = 9_000.0;
pub const ELA_NONE: i16 = i16::MAX;
pub const ELA_BISECTIONS: usize = 16;
pub const SHEET_MIN_CELLS: usize = 12;
pub const SHEET_COEFF: f64 = 4.7;            // sqrt(2 · 100 kPa / (917 kg/m³ · 9.81)): a plastic profile, recalled
pub const ICE_LIMIT_ASL_M: f64 = 50.0;
/// Per planet cell (PLANET_NX × PLANET_NZ, row-major, x wraps).
pub struct IceClimate {
    pub ela_now_m: Vec<f32>, pub ela_cold_m: Vec<f32>,        // f32::INFINITY = none; 0.0 = ice at the sea
    pub ground_m: Vec<f32>,                                   // mean fine ground of the cell (sea cells: 0)
    pub sheet_now_m: Vec<f32>, pub sheet_cold_m: Vec<f32>,    // thickness; 0 = none
    pub sea_ice_months: Vec<u8>,                              // sea cells: Derived::sea_ice_months
    pub limit_lat_deg: [f32; 2],                              // north, south; 90 = the cold climate puts no ice at any shore
}
impl IceClimate {
    /// `w`: a fine world (its heights and mask); `monthly`, `annual_mm`: the float normals and the fine annual
    /// raster COMPUTED ON `w` (`fine::rain` → `monthly_fields`). `cold`: fills the cold half (lines lowered by
    /// `k.ice_age_cooling_c`, the cold sheet, the limit) and leaves the present half empty; else the reverse plus
    /// `sea_ice_months`.
    pub fn compute(&mut self, w: &World, monthly: &climate::MonthlyFields, annual_mm: &[f32], cold: bool, p: &ForgeParams);
    pub fn empty() -> IceClimate;
    /// The equilibrium line (m) at fine metres (mx, mz): bilinear on the planet grid, x wrapping by index.
    pub fn ela_at(&self, cold_share: f64, mx: f64, mz: f64, p: &ForgeParams) -> f64;   // 0 = now, 1 = cold
    pub fn sheet_at(&self, cold_share: f64, mx: f64, mz: f64, p: &ForgeParams) -> f64;
}
```

- The mass balance, the bisection, the sheet rule and the plastic profile: Ruling 9. **Pinned:** per planet cell — `ground_m` = `water::to_planet_mean(w, h.max(0), land)`; `P` = `to_planet_mean(w, annual_mm, land)` (a cell with no land: skipped — no line, no sheet); the sea-level normals = the BILINEAR read of the float `MonthlyFields` at the planet cell's centre (`monthly_cell_normals` of the four surrounding climate cells weighted as `MonthlyClimate::normals_at` does: x wraps by index, z clamps — a pure function of the cell's own neighbourhood, so it rolls); the cold normals are those `lowered_by(k.ice_age_cooling_c)`; the line = the `z` in `[0, ELA_MAX_M]` where `derive(&ground_normals(normals, z, P, lapse), lat, false).mass_balance_mm` changes sign (precipitation is not changed with height). A sheet's distance-to-edge is a wrapping chamfer on the planet grid in metres (`height_m / PLANET_NZ` per cell). **Two calls, by the stage list:** `compute(.., cold = true)` at the start of stage 4 on the re-stood world, with the normals `forge_with` gets from `monthly_fields` of that world's first `fine::rain` (one `monthly_fields` more than Phase 3 runs — it is in "Cost"'s rain line); `compute(.., cold = false)` at stage 9 on the flooded world with the stored climate of stage 8. **The limit** (vocabulary): per hemisphere, the lowest `|latitude|` of a land planet cell that is a coast cell of the COLD mask (4-adjacent to a cold-sea planet cell) and either holds a cold sheet or has `ela_cold_m ≤ cold sea level + ICE_LIMIT_ASL_M` — climatic, so Task 14's fjord line can fail.
- `ice_age_cooling_c` enters `SPECS` here: `artifact_header_crc` and `world_file_bytes` move with no world change (exempt, quoted — Global Constraints).
- `--report`: `ice.cooling_c`, `ice.ela_now_m.<75N|60N|45N|eq|45S|60S|75S>` and `ice.ela_cold_m.<…>` (the median over the band's land cells; `none` when no land), `ice.sheet_cells.now`, `ice.sheet_cells.cold`, `ice.sheet_land_share.cold` (÷ land planet cells), `ice.sheet_max_m.cold`, `ice.limit_lat_deg.north` / `.south`, `ice.sea_ice_months.<75N|60N|60S|75S>`, `ice.climate_ms`.

- [ ] **Step 1: Temporary tests:** on a `MonthlyFields` literal at −20 °C all year with 30 mm a month the line is 0 everywhere and a 20-cell land disc holds a sheet whose thickness at the centre is `4.7 · sqrt(edge distance)` within 5 %; at +25 °C all year the line is `ELA_NONE` and there is no sheet; cooling by 6 °C lowers a mid-latitude line by `6 / lapse` within 15 % (the bisection resolves `9000 / 2¹⁶` m); a disc of 11 cells holds no sheet; the planet grid rolled by 256 columns gives the rolled result bit for bit.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the 8 dev seeds and 2 Medium: every line; no old line moves but the two knob-exempt ones (nothing reads `IceClimate` yet: `height_crc`, `artifact_rasters_crc`, `artifact_climate_crc`, `artifact_planet_crc`, `artifact_province_crc` equal to Task 8's). **Targets:** `ice.ela_now_m` rises from the polar bands to the equator on every seed (`eq` ≥ 3,000 m or `none`); `ice.ela_cold_m` is lower than `now` in every band that has both; `ice.sheet_land_share.cold` between 0.03 and 0.5 on at least 6 of 8 dev seeds (a first look — Earth's glacial maximum covered about a quarter of the land: a search summary); with `--off cooling` the cold lines equal the present ones. READ `t9-ice.png`.
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): the ice climate — equilibrium lines and ice sheets on the planet grid, now and in the cold stage`.

### Task 10: Valley glaciers — the glacial cut in the cold stage (prototype first)

**Files:** `crates/worldgen/src/forge/ice.rs` (`Glaciers`), `forge/world.rs` (`World` gains `pub ice: Vec<f32>` — ice thickness, m; empty = none; `new_at`, `upsample_onto`, `restand`, the `initial` literal, the roll helper), `forge/erosion.rs` (`evolve`: a cell with `ice > 0` under a depression does not silt), `forge/mod.rs` (`forge_with`'s cold stage: the schedule below; `ColdStage::ice_max`, `ColdStage::margins: [Vec<bool>; 3]`), `forge/knobs.rs` (ADD "Ice" `glacial_erosion`), `forge/hydrology.rs` (`regime`'s `ice_share`: the catchment's share of cells with present ice — one more accumulation sum), the knob mirrors, `report.rs`, `forge/render.rs`.

**Interfaces — produces:**

```rust
pub const ICE_COARSEN: usize = 4;
pub const ICE_EVERY_ITERS: usize = 5;
pub const ICE_FULL_ABOVE_M: f64 = 500.0;
pub const GLACIER_WIDTH_COEFF: f64 = 4.0;     // m at A_i = 1 m² of rain-weighted ice area; Hergarten gives no physical value
pub const GLACIER_WIDTH_EXPONENT: f64 = 0.3;  // Hergarten 2021 Eq. 13 (read)
pub const GLACIER_WIDTH_MAX_M: f64 = 3_000.0;
pub const GLACIER_THICKNESS_SHARE: f64 = 0.05;   // h ∼ w: the fetched summary's ratio (paper units); Hergarten's Eq. 40 gives h ∼ q_i^0.23 / S instead
pub const ICE_FLUX_MIX: f64 = 0.25;           // ε of Eq. 22 (read)
pub const GLACIAL_CUT_MAX_M: f64 = 15.0;
pub const COLD_FULL_SHARE: f64 = 0.6;
pub const RETREAT_STEPS: usize = 3;
pub const GLACIAL_REGIME_ICE_SHARE: f32 = 0.05;
pub struct Glaciers {
    /// On the fine grid: ice thickness (m) and the cut applied (m, ≥ 0) by this application.
    pub ice_m: Vec<f32>, pub cut_m: Vec<f32>, pub glacier_cells: usize,
}
/// One application (Ruling 9) on `w` at `cold_share` (1 = the cold line, 0 = today's). `lagged_ice`: the fine ice
/// thickness of the previous application (empty = none). `dt_ice` years (0 = route and measure, cut nothing).
pub fn glaciers(w: &World, ic: &IceClimate, geo: &Geology, rain: &[f64], lagged_ice: &[f32], cold_share: f64,
                dt_ice: f64, p: &ForgeParams) -> Glaciers;
```

- **One application, pinned.** (1) The ice grid: `ICE_COARSEN`² block means of the fine `h`, of `rain` (the erosion's rain weight) and of the rock factor `geo.erodibility_at(i, h, uplifted)` × Phase 2's `k_mult` where it exists; a block is sea when most of its cells are; x wraps. (2) `H = ground + block-mean of lagged_ice` (the ice SURFACE, lagged one application). (3) `erosion::route` (the Priority-Flood) on a `World` literal of the ice grid holding `H`: receivers, distances, order. (4) Sources: `p_i = rain · min((H − H_e)/(ICE_FULL_ABOVE_M), 1)` with `H_e = ic.ela_at(cold_share, ..)` at the block's centre — negative below the line; `A` = the accumulated `rain · block area` (m² of average rain, `Analysis::q`'s unit); `A_i` = the accumulated `p_i · block area`, floored at 0 at every cell before it is passed on (a glacier ends where melt has used it up). (5) A glacier cell: `A_i > 0` and not sea; `w_m = min(GLACIER_WIDTH_COEFF · A_i^GLACIER_WIDTH_EXPONENT, GLACIER_WIDTH_MAX_M)`; ice thickness there `GLACIER_THICKNESS_SHARE · w_m`. (6) **The swath, O(cells):** a generalised chamfer over the ice grid seeded at glacier cells with `reach = w_m / 2`, relaxing `reach − step` to the 8 neighbours and carrying the seed's `A_i` and thickness (a cell keeps the candidate with the largest remaining reach; ties: the larger `A_i`, then the lower seed index); forward and backward pass, repeated while a wrapping row still changes (at most `habitat::CHAMFER_WRAP_EXTRA_PASSES` more pairs). Cells with remaining reach > 0 are the swath; their ice is the carried thickness × `min(1, 2 · remaining / w_m + 0.5)` (thinner at the edge). (7) **The cut, each swath cell by its OWN slope:** `ΔH_j = min(dt_ice · k.glacial_erosion · k.erodibility · rock_j · sqrt(A_j^ICE_FLUX_MIX · A_i,carried^(1 − ICE_FLUX_MIX)) · S_j, GLACIAL_CUT_MAX_M)`, `S_j = (H_j − H_receiver(j)) / dist` on the routed ice surface (0 at a pit). (8) To the fine grid: `ice_m` and `cut_m` bilinear between ice-grid cell centres, x wrapping by index, z clamped; a fine sea cell gets 0.
- **The cold stage's schedule** (`n = cold_stage_iterations()`, `n_full = ceil(COLD_FULL_SHARE · n)`): iterations `1 ..= n_full` at `cold_share = 1`; the rest in `RETREAT_STEPS` equal still-stands at `cold_share = 2/3, 1/3, 0` (the last takes the remainder). **Applications, exactly:** before iteration `1 + ICE_EVERY_ITERS · j` for every `j ≥ 0` with that iteration ≤ `n_full`, and before the first iteration of each still-stand; `dt_ice` = `dt_years ×` the iterations since the previous application (`ICE_EVERY_ITERS` for the first). With the defaults (`n` = 30): before iterations 1, 6, 11, 16 (cold), 19, 23, 27 (the three still-stands) — SEVEN applications, plus the present one of stage 9. Each: `g = glaciers(..)`; `w.h[i] −= g.cut_m[i]` for non-sea cells (no floor: Ruling 9; Task 2's clamp keeps it); `w.ice = max(g.ice_m, sheet_at(cold_share))`; `ice_max[i] |= w.ice[i] > 0`. **Margins:** at the END of each stand (after iteration `n_full`, and after each still-stand's last iteration) the ice edge — cells with `w.ice > 0` and a 4-neighbour without — is recorded: `margins[0]` the cold maximum (the TERMINAL margin), `[1]` and `[2]` the two recessional ones, `[3]` the edge when the stage ends. Stage 9: `glaciers(.., cold_share = 0, dt_ice = 0)` on the final world gives the PRESENT `w.ice` (with the present sheet). With `glacial_erosion = 0` the ice is routed and recorded but cuts nothing.
- The sediment the cut removes is added to that cell's `StepScratch::qs` source for the next step (`cut · dx² / dt`), so outwash comes out through Task 5's term.
- `--report`: `ice.valley_glacier_cells.cold` (fine cells with routed ice at `cold_share = 1`, outside sheets), `ice.valley_glacier_cells.now`, `ice.max_share` (`ice_max` ÷ land), `ice.cut_p99_m`, `ice.cut_max_m` (cumulative, over `ice_max`), `ice.overdeepened_cells` (cells of `ice_max` that end under a lake or the sea), `ice.u_valley_ratio` (over river-network cells with `q` in `[1e6, 3e7]`: the share of cells within 5 m of the thalweg's height inside a window 8 cells across the valley, mean inside `ice_max` ÷ mean outside; `nan` when either side has under 200 samples), `rivers.falls_in_ice_max_share`, `ice.glaciers_ms`.

- [ ] **Step 1: The prototype, as temporary tests** on a hand-built 256 × 128 wrapped world at 32 m: a V-shaped trunk valley with two side valleys in a 1,500 m massif, an `IceClimate` literal with a cold line at 700 m. (a) `glaciers` at `cold_share = 1`: ice in the valleys above the line and for some distance below it, none on the plain, `ice_m` ≤ `0.25 · GLACIER_WIDTH_MAX_M`, total `A_i` never negative; (b) 6 applications with 5 `erosion::step`s between them: the trunk's floor is wider than before (the flat-floor share of the cross-section at mid-valley rises by at least 1.5×) — and it is so because the valley SIDES inside the swath were cut by their own steeper slope: with the cut made uniform across the swath (a one-line variant kept in the test) the floor share does NOT rise by 1.5× —, a side valley's mouth hangs at least 5 m above the trunk, at least one cell of the trunk lies below the cell downstream of it (an overdeepening), and no cell fell more than (applications made) × `GLACIAL_CUT_MAX_M`; the swath of 20,000 glacier cells is built by the chamfer in under twice the time of one `erosion::route` on that grid (it is O(cells)); (c) the world rolled by 128 columns gives the rolled result. Quote the cross-sections as printed rows of heights. If (b) gives a slot instead of a floor, widen the swath's reach to `w` (not `w / 2`) and say so under Rulings.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the table (8 dev, 2 Medium, and `just forge-report --size small --cell 32 --seeds 2`): this task's lines, `lakes` and `lakes.origin.*` (basins the ice cut), `rivers.falls`, `rivers.regime.glacial`, the gate's lines, the standing invariants. **Targets:** on the Medium pair `ice.max_share` > 0 and `ice.u_valley_ratio` ≥ 1.2 where it is not `nan` (Task 22 tunes to 1.3), and with `--off glacial` it is within 0.9–1.1; per size, `ice.valley_glacier_cells.cold` is QUOTED (Dev, Small, Medium: Ruling 9 expects none or few on Dev — that is recorded, not a failure). READ `t10-ice.png` and `t10-relief.png` on Medium.
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): valley glaciers in the cold stage — a glacial stream-power cut, a stepped retreat, present-day ice`.

### Task 11: What the ice left — moraines and kettles; glacial lake origins

**Files:** Create `crates/worldgen/src/forge/stamps.rs` (`pub mod stamps;` — every post-flood terrain edit of this phase lives here); Modify `forge/mod.rs` (`forge_with`: stage 10 of the stage list — the stamp stage after the flood and the planing, before the analysis; `Forged` gains `pub stamps: stamps::Stamped`), `forge/water.rs` / `forge/hydrology.rs` (the three ice origins), `report.rs`, `forge/render.rs`.

**Interfaces — produces:**

```rust
pub const MORAINE_HEIGHT_SHARE: f64 = 0.1;      // of the ice thickness just up-glacier; clamp 3 … 30 m (recalled dimensions)
pub const MORAINE_WIDTH_CELLS: usize = 2;
pub const KETTLE_ZONE_M: f64 = 1_500.0;         // outside the edge of `ice_max` (the widest the ice got)
pub const KETTLE_MAX_SLOPE: f64 = 0.03;
pub const KETTLE_DENSITY_PER_KM2: f64 = 2.0;
pub const KETTLE_RADIUS_M: (f64, f64) = (30.0, 150.0);   // "most kettles < 2 km across" (read)
pub const KETTLE_DEPTH_M: (f64, f64) = (2.0, 8.0);        // "< 10 m deep" (read)
/// What the stamps touched: one byte per fine cell (0 = nothing). Later tasks append kinds.
#[repr(u8)] pub enum Stamp { None = 0, Moraine = 1, Kettle = 2, DeltaPlain = 3, DeltaLobe = 4, Barrier = 5,
                             Lagoon = 6, Reef = 7, Atoll = 8, Beach = 9, TidalFlat = 10 }
pub struct Stamped { pub kind: Vec<u8>, pub moraines: usize, pub kettles: usize }
/// Stage 10. Moraine ridges along `cold.margins[0]` (TERMINAL) and `[1]`, `[2]` (recessional) where a margin
/// crosses a valley floor; kettles by hash in the outwash zone outside the terminal margin. `margins[3]` (today's
/// edge) is not stamped. Ends with `post::flood(w, 0.0, &cold.ocean)`. No-op when `!features.stamps_ice` or the stage did not run.
pub fn ice_stamps(w: &mut World, cold: &ColdStage, seed: u64, features: &Features, out: &mut Stamped);
```

- A margin cell is on a valley floor when its `cold.q ≥ river_min_discharge_m2 / 4` or it is within 2 cells of such a cell; the ridge is raised to `floor + clamp(MORAINE_HEIGHT_SHARE · thickness, 3, 30)` over `MORAINE_WIDTH_CELLS`, never under an existing higher cell. A kettle: candidate cells on the fine grid within `KETTLE_ZONE_M` outside the edge of `ice_max` (one wrapping chamfer from it), slope under `KETTLE_MAX_SLOPE`, not sea, not river-network; kept when `hash01(seed, KETTLE_SALT, cell) < KETTLE_DENSITY_PER_KM2 · dx² / 10⁶`; a paraboloid bowl of hashed radius and depth, never lowered under `SHORE_STEP_M`. Seeds come from the cell's canonical index, so a rolled world stamps the rolled kettles.
- A kettle of 30–150 m radius is 1–5 cells across at 32 m: under `LAKE_MIN_CELLS = 4` most make no LAKE on Dev at 32 m (expected; `lakes.origin.kettle` is judged on Medium at 16–32 m).
- `--report`: `ice.moraines` (connected ridges), `ice.moraines.terminal` (those on `margins[0]`), `ice.moraine_cells`, `ice.kettles`, `lakes.origin.glacial` / `.moraine` / `.kettle` (now reachable).

- [ ] **Step 1: Temporary tests:** on Task 10's prototype world with a terminal and one recessional margin recorded across the trunk (two ridges, the terminal one the lower): a ridge of 3–30 m spans the floor, the basin behind it floods to a lake whose origin is `Moraine`, and the trunk's river still reaches the sea (`drains_to_sea` of the world is 1); kettles appear only in the zone and only on gentle ground; `sea_is_height` holds after the stamps; `--off stamps_ice` leaves `h` untouched.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the table (8 dev, 2 Medium, 2 Small): this task's lines, `lakes`, the densities. **Targets:** on any seed with `ice.valley_glacier_cells.cold` > 2,000, `ice.moraines.terminal` ≥ 1; the standing invariants. READ `t11-ice.png`.
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): moraines and kettles stamped from the cold stage's ice margins`.

## Slice E — coasts, mouths, deltas

### Task 12: Feature detection on the planet grid (prototype first; changes no world)

**Files:** Create `crates/worldgen/src/forge/features.rs` (`pub mod features;`); Modify `forge/mod.rs` (`Forged` gains `pub features: features::Detected`: `detect_mask` at stage 12, `detect_objects` at stage 19), `report.rs`, `forge/render.rs` (`-features.png`: the planet-grid mask with each feature's cells tinted by kind and its anchor dotted).

**Interfaces — produces:**

```rust
pub const FEATURE_RADII_CELLS: [usize; 4] = [2, 5, 12, 30];     // 1.4°, 3.5°, 8.4°, 21° on the planet grid
pub const FEATURE_MIN_CELLS: usize = 3;
pub const FEATURE_TOP_N: usize = 24;
#[repr(u8)] pub enum FeatureKind { Ocean = 0, Sea = 1, Gulf = 2, Bay = 3, Cove = 4, Strait = 5, Peninsula = 6,
    Cape = 7, Isthmus = 8, Island = 9, Continent = 10, Range = 11, Lake = 12, River = 13, Falls = 14, Delta = 15,
    Lagoon = 16, Glacier = 17 }
pub struct Found { pub kind: FeatureKind, pub anchor_m: (f64, f64), pub area_m2: f64, pub extent_m: f64,
                   /// Planet cells of a mask feature (empty for an object's); `object`: index into rivers / lakes /
                   /// waterfalls / mouths / belts, by kind; NO_OBJECT otherwise.
                   pub cells: Vec<u32>, pub object: u32, pub mouth_m: f64, pub length_m: f64, pub radius_cells: u16 }
pub struct Detected { pub found: Vec<Found>, pub bay_of_cell: Vec<u32> /* planet cell → index of its bay or gulf, or NO_FEATURE */ }
/// `land`: PLANET_NX × PLANET_NZ, true = land (x wraps). Disc closing / opening by two wrapping chamfer distance
/// transforms each; components 4-connected; Ruling 11's rules. Pure function of the mask. Stage 12.
pub fn detect_mask(land: &[bool], metres_per_cell: f64) -> Detected;
/// Stage 19: appends the object features of the STORED analysis — lakes, rivers (trunk chains by flow), falls,
/// glaciers, Phase 2's belts and continents, the largest FEATURE_TOP_N of each — and, from Tasks 15 and 16, deltas
/// (one per `DeltaPlan`) and lagoons (the sea components of `Stamp::Lagoon` cells).
pub fn detect_objects(d: &mut Detected, w: &World, a: &Analysis, planet: &Planet, stamps: &stamps::Stamped, p: &ForgeParams);
```

- The planet-grid land mask is `water::to_planet_mean(w, land as 0/1, all) ≥ 0.5` ("fine → planet cell": on Dev at 64 m each fine cell gives its value to the four planet cells whose centres it holds).
- A water component of `close(land, r) − land` with NO mouth onto open water (a lake or an inland sea the closing filled whole) is skipped at that radius — inland seas come from the sea components, not from the closing.
- `Found::length_m` (added to the struct beside `mouth_m`): for a `Cove`, `Bay` or `Gulf`, the distance from the midpoint of its mouth to its farthest cell, the shorter way round — Task 13's funnel reads it.

- [ ] **Step 1: The prototype, as temporary tests** on drawn 512 × 256 masks: a continent with a square bay 10 cells wide and 12 deep → exactly one `Bay` whose `mouth_m` is 10 cells and whose anchor is inside the bay; the same bay 10 wide and 2 deep → nothing (the semicircle rule: area 20 < π·100/8); a channel 3 cells wide between two land masses → one `Strait` anchored in it and no `Bay`; a land arm 4 cells wide and 20 long → one `Peninsula` (or `Cape` at the small radii) anchored at its tip; a land bridge 3 wide between two blocks → one `Isthmus`; an inland water body → `Sea`; a bay straddling column 0 is ONE bay; the mask rolled by 256 columns gives the same list with anchors rolled; a bay found at radius 5 and again at 12 appears once, at 5. Quote the list printed for each mask.
- [ ] **Step 2: Implement** `detect_mask` and `detect_objects` (lakes, rivers, falls, glaciers, belts, continents now; deltas and lagoons join in Tasks 15 and 16); nothing reads them but the report and the sheet.
- [ ] **Step 3: Evidence** — the safety net (no world moves: the five crcs equal to Task 11's on 8 dev seeds at 32 m AND at 64 m — at 64 m a planet cell is half a fine cell: the mask rule above is what is exercised; lines appended only). `features.<kind>` counts per seed (8 dev, 2 Medium), `features.total`, `features.detect_ms`. READ `t12-features.png` of two Medium seeds and of one dev seed and judge in the report, feature by feature for the ten largest: is what is tinted a bay / strait / peninsula to the eye? A kind that is wrong more often than right gets its rule's constant changed here (one round) or is DROPPED from the list with a ruling — Task 13 reads bays from this.
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): bays, gulfs, straits, peninsulas, capes and isthmuses detected on the planet grid`.

### Task 13: Coast fields and classes

**Files:** Create `crates/worldgen/src/forge/coast.rs` (`pub mod coast;`); Modify `forge/knobs.rs` (ADD group "Coast": `tide_m`), `forge/mod.rs` (`forge_with`: stage 13 of the stage list — mask features → coast, after the first analysis; `Forged` gains `pub coast: coast::Coast`), the knob mirrors, `report.rs`, `forge/render.rs` (`-coast.png`: class, and exposure / tide as two more panels).

**Interfaces — produces:**

```rust
pub const SETTING_REACH_DEG: f64 = 5.0;
pub const FETCH_CAP_DEG: f64 = 60.0;
pub const EXPOSURE_FULL: f64 = 40.0;            // (m/s)² of mean squared onshore wind at full fetch = exposure 1: Task 22
pub const ENCLOSED_SEA_TIDE: f64 = 0.2;
pub const FUNNEL_GAIN: f64 = 1.5;
pub const SEDIMENT_REACH_DEG: f64 = 3.0;
pub const REEF_MIN_WINTER_SEA_C: f64 = 18.0;    // "few reefs below 18 °C" (read by the researcher)
pub const BARRIER_MAX_TIDE_M: f64 = 4.0;        // read
pub const BARRIER_MIN_EXPOSURE: f64 = 0.35;     // Task 22
pub const BARRIER_MIN_SEDIMENT: f64 = 0.2;      // Task 22
pub const LOW_SHORE_M: f64 = 10.0;              // mean land height within LOW_SHORE_REACH_DEG of the shore
pub const LOW_SHORE_REACH_DEG: f64 = 1.5;
pub const STEEP_SHORE_M: f64 = 60.0;            // mean land height within LOW_SHORE_REACH_DEG at or above this: a steep shore
pub const SEDIMENT_FULL_M3_PER_YR: f64 = 1.0e5; // sediment at which the 0–1 scale is 1: Task 22
pub const WINTER_SEA_SEARCH_CELLS: usize = 8;   // climate cells
#[repr(u8)] pub enum Setting { Collision = 0, Trailing = 1, Marginal = 2 }
#[repr(u8)] pub enum CoastClass { None = 0, Ice = 1, Glaciated = 2, Reef = 3, Barrier = 4, Muddy = 5, Rocky = 6, Sandy = 7 }
/// Per planet cell; meaningful where `class != None` (a coast cell).
pub struct Coast { pub class: Vec<u8>, pub setting: Vec<u8>, pub exposure: Vec<f32>, pub tide_m: Vec<f32>,
                   pub sediment: Vec<f32>, pub winter_sea_c: Vec<f32>, pub shore_height_m: Vec<f32> }
pub fn coast(w: &World, a: &Analysis, forged_so_far: &CoastInputs, p: &ForgeParams) -> Coast;
pub fn tide_class(tide_m: f64) -> u8;   // 0 micro (< 2), 1 meso (2–4), 2 macro (> 4)
```

- `CoastInputs` bundles `&Planet`, `&climate::Planetary`, `&climate::MonthlyFields`, `&features::Detected`, `&ColdStage`, `&[f32]` sediment, `&[f32]` present ice. The inputs and the class rule: Ruling 10. **Pinned:** a coast cell — the vocabulary's rule through `water::to_planet_any` (so `coast.cells` > 0 on every size, Dev at 32 m and 64 m included). The wind: `Planetary::wind_east_ms` / `wind_south_ms` (12 × 256 × 128, model grid) read onto the planet grid with `grid::resample` (each model cell covers 2 × 2 planet cells). Fetch: steps of one planet cell along each of 8 directions until land or `FETCH_CAP_DEG`, x wrapping. `funnel` uses the bay's `Found::length_m` and `mouth_m`: `1 + FUNNEL_GAIN · min(length_m / mouth_m, 3)` for a coast cell whose `bay_of_cell` is set. Winter sea temperature: the coldest month of the nearest SEA climate cell (a climate cell at least half sea) within `WINTER_SEA_SEARCH_CELLS`, by a wrapping ring search of growing radius, ties to the lower index; none within reach → the coast cell's own climate cell. `shore_height_m` = the mean height of land fine cells within `LOW_SHORE_REACH_DEG` of the cell's shore: a LOW shore is under `LOW_SHORE_M`, a STEEP one at or above `STEEP_SHORE_M`. Sediment per coast cell: Σ `Forged::sediment` at the mouths (analysis 1) within `SEDIMENT_REACH_DEG`, normalised `min(1, ln(1 + Qs) / ln(1 + SEDIMENT_FULL_M3_PER_YR))`, plus 0.3 where the cell is in `ice_max`.
- `tide_m` enters `SPECS` here: `artifact_header_crc` and `world_file_bytes` move with no world change (exempt, quoted).
- `--report`: `coast.cells`, `coast.class.<name>` (shares of coast cells, weighted by each cell's fine `coast_km`), `coast.setting.<name>`, `coast.exposure.p50` / `.p90`, `tide.class.<micro|meso|macro>`, `tide.max_m`, `coast.class_runs_mean_cells`, `coast.barrier_eligible_share` (the `Barrier` class's share: Task 16's ceiling).

- [ ] **Step 1: Temporary tests:** on a synthetic planet-grid world — a rectangular continent, Phase 3's wind planes as a literal blowing from the west: the west coast's exposure is at least 3× the east coast's and a coast cell inside a 2-cell-wide inlet reads under 0.2; in a drawn inland sea the tide is `0.2 · tide_m`; at the head of a bay 20 cells long with a 5-cell mouth it is `tide_m · (1 + 1.5 · 3)` (macro at the default), and with `tide_m = 0` it is 0 everywhere; a coast under a young `Collision` orogen is `Collision` and never `Barrier`; a warm, sediment-free coast is `Reef`; the class of a cell is the same when the world is rolled.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the safety net (no world moves but the two knob-exempt lines). The lines on 8 dev seeds at 32 m, 8 at 64 m and 2 Medium: `coast.cells` > 0 on EVERY one (0 anywhere is BLOCKED: the segment definition failed on that size). **First look, no pass line before Task 22:** `coast.barrier_eligible_share` quoted (the stamp can reach at most this; under 0.05 or over 0.4 is NAMED for Task 22); `tide.class.micro` is the largest class; every class but `None` occurs in at least one of the ten worlds, or the missing ones are named. With `--off tide`: `tide.max_m` = 0. READ `t13-coast.png` on Medium.
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): coast cells classed by setting, wave exposure, tide, sediment and winter sea`.

### Task 14: Every river mouth gets a class

**Files:** `crates/worldgen/src/forge/coast.rs` (`mouths`), `forge/mod.rs` (`Forged` gains `pub mouths: Vec<coast::Mouth>`), `report.rs`, `forge/render.rs` (mouth marks by class on `-coast.png`).

**Interfaces — produces:**

```rust
pub const INLET_MAX_WIDTH_M: f64 = 1_500.0;
pub const INLET_MIN_CELLS: usize = 4;
pub const INLET_GLACIATED_SHARE: f64 = 0.5;
pub const DELTA_FILL_YEARS: f64 = 7_000.0;       // the stillstand since the flood: unverified background
pub const DELTA_MIN_VOLUME_M3: f64 = 2.0e5;
pub const DELTA_WAVE_EXPOSURE: f64 = 0.5;
#[repr(u8)] pub enum MouthClass { Delta = 0, Estuary = 1, Ria = 2, Fjord = 3 }
#[repr(u8)] pub enum DeltaType { River = 0, Wave = 1, Tide = 2 }
pub struct Mouth { pub cell: u32, pub x: f64, pub z: f64, pub reach: u32, pub class: MouthClass, pub delta: DeltaType,
                   pub flow_m3s: f32, pub sediment_m3_per_yr: f32, pub inlet_m: f32, pub inlet_volume_m3: f32,
                   pub tide_m: f32, pub lat_deg: f32 }
/// One per trunk mouth of `a` (a river-network cell whose receiver is sea), in routing order. Ruling 8's rule.
pub fn mouths(w: &World, a: &Analysis, cold: &ColdStage, coast: &Coast, sediment: &[f32], ic: &IceClimate, p: &ForgeParams) -> Vec<Mouth>;
```

- The inlet walk: it starts at the mouth's sea 4-neighbour with the largest `cold.q` (ties: the lowest canonical cell index); from a cell it steps to the 8-neighbour (through `World::step`) that is sea now, was land in the seed mask and has the largest `cold.q` not smaller than the current cell's (ties: the lowest index), while the water's width there (twice the distance to the nearest land, from one wrapping chamfer of the mask) is under `INLET_MAX_WIDTH_M`; `inlet_m` is the path length, `inlet_volume_m3` the sum of `−h · dx²` over the path's cells and their sea neighbours within the half-width.
- **Which analysis** (stage list): `mouths` runs at stage 14 on analysis 1 (the deltas and the coast's sediment need it) and AGAIN at stage 18 on analysis 2 — the stored list, whose `reach` indexes the stored rivers and whose length equals the stored `Stats::mouths`. The facts a class rests on (the seed mask, `cold.q`, `ice_max`, the coast cell, the river's sediment) do not move with the stamps, with one exception handled by rule: a stage-18 mouth whose cell is, or is 8-adjacent to, a `Stamp::DeltaLobe` or `Stamp::DeltaPlain` cell is `Delta` with its plan's type (the lobe filled the inlet the rule would look for). Until Task 15 there is one analysis and one call. `mouths` and `mouths.classed` count the STORED list.
- `--report`: `mouths` (the stored list's length = the stored `river_mouths`), `mouths.classed`, `mouths.class.<delta|estuary|ria|fjord>`, `mouths.delta_type.<river|wave|tide>`, `mouths.inlet_max_km`, `mouths.fjords_equatorward` (fjord mouths with `|lat_deg|` under their hemisphere's `ice.limit_lat_deg`), `mouths.fjord_lat_min_deg`.

- [ ] **Step 1: Temporary tests:** four hand-built mouths — a 10-cell drowned valley inside `ice_max` → `Fjord`; the same outside it with no sediment → `Ria`; the same with sediment enough to fill it → `Delta`; no inlet, no sediment → `Estuary`; two sea neighbours with equal `cold.q` → the walk starts from the lower index, and the rolled world gives the rolled mouth; a macro-tidal mouth with sediment → not `Delta`; `mouths().len()` equals `Stats::mouths` on the default dev world and every mouth has a class.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the safety net (no world moves). The lines (8 dev, 2 Medium, 2 Small). **Targets:** `mouths.classed` = `mouths` on every seed; `mouths.fjords_equatorward` = 0 on every seed; on the Medium pair at least 2 classes per seed; with `--off cooling` no fjord on any seed, with `--off lowstand` no ria on any seed (both lines fail with the feature off). READ `t14-coast.png`: for the five largest rivers of one Medium seed, say whether the class matches what the map shows.
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): every river mouth classed — delta, estuary, ria or fjord`.

### Task 15: Deltas — plains, lobes and distributaries

**Files:** `crates/worldgen/src/forge/stamps.rs` (`deltas`), `forge/features.rs` (`detect_objects`: deltas), `forge/hydrology.rs` (`River` gains `down: [u32; 2]` — filled for EVERY reach from the routing — and `NO_REACH`; `attach_deltas`), `forge/mod.rs` (`forge_with`: the stamp → the SECOND `analyze` → `attach_deltas`; `Forged::artifact` writes branch reaches as ordinary reaches), `report.rs`, `forge/render.rs`.

**Interfaces — produces:**

```rust
pub const DELTA_MIN_FLOW_M3S: f32 = 1.0;
pub const DELTA_BRANCHES: (usize, usize) = (2, 5);
pub const DELTA_APEX_WIDTHS: f64 = 12.0;         // the apex lies this many channel widths upstream, 3 cells … DELTA_APEX_MAX_M
pub const DELTA_APEX_MAX_M: f64 = 4_000.0;
pub const DELTA_SPREAD_DEG: f64 = 50.0;          // half-angle of the fan, about the trunk's direction at the apex
pub const DELTA_PLAIN_M: (f64, f64) = (0.5, 2.0);
pub const DELTA_LOBE_MAX_DEPTH_M: f64 = 20.0;    // a lobe does not build into water deeper than this
pub const DELTA_LOBE_MAX_DEG: f64 = 1.5;         // nor farther than this from its mouth
pub const DELTA_SALT: u64 = 0x44454C54;
pub const NO_REACH: u32 = u32::MAX;
pub struct DeltaPlan { pub mouth: u32 /* index into mouths */, pub apex_cell: u32, pub branches: Vec<Vec<u32>> /* cells, apex → sea */ }
/// For each `Delta` mouth with flow ≥ DELTA_MIN_FLOW_M3S: the lobe (sea cells nearest the mouth turned to land at
/// DELTA_PLAIN_M.0, volume = min(sediment · DELTA_FILL_YEARS − inlet volume, cap)), the plain (land inside the fan
/// lowered or raised into DELTA_PLAIN_M, rising inland), the branch paths (each traced from its shore cell BACK to
/// the apex by least cost over plain cells, cost = height above the plain + a hashed jitter; paths that meet merge).
/// Stage 15. No flood of its own: stage 16's `coast_stamps` ends with the one flood for both (`post::flood(w, 0.0, &cold.ocean)`);
/// when `coast_stamps` is switched off entirely the caller floods. No-op when `!features.deltas`.
pub fn deltas(w: &mut World, a: &Analysis, mouths: &[Mouth], seed: u64, features: &Features, out: &mut Stamped) -> Vec<DeltaPlan>;
/// After the second analysis: cuts the trunk reach of each plan at its apex and appends one `Pattern::Distributary`
/// reach per extra branch (points along the path, bed = ground, water from the plain's fall to the sea), each with
/// `1 / branches` of the trunk's twelve flows; sets `down` of the apex reach to its two first branches.
pub fn attach_deltas(a: &mut Analysis, w: &World, plans: &[DeltaPlan], k: &Knobs);   // stage 18, after the final `mouths`
```

- Branch count `N`: `clamp(2 + floor(log2(flow / DELTA_MIN_FLOW_M3S) / 2), 2, 5)`, one fewer for `DeltaType::Wave`, at least 2.
- **The lobe, pinned:** its volume is `V = max(0, sediment · DELTA_FILL_YEARS − inlet_volume)`; sea cells are taken in order of increasing distance from the mouth (one wrapping chamfer through sea cells; ties: the lower index), skipping any deeper than `DELTA_LOBE_MAX_DEPTH_M` or farther than `DELTA_LOBE_MAX_DEG` (in metres); each taken cell becomes land at `DELTA_PLAIN_M.0` and uses `(DELTA_PLAIN_M.0 − h) · dx²` of `V`; it stops when the next cell would overdraw `V`.
- **The branch ends:** the FRONT = the plain's and the lobe's cells that have a sea 4-neighbour after the lobe is built, inside the fan (within `DELTA_SPREAD_DEG` of the trunk's direction at the apex); sorted by their angle about the apex; branch `b` (0-based) ends at the front cell at share `(b + 0.5 + 0.5 · (hash01(seed, DELTA_SALT, mouth · 8 + b) − 0.5)) / N` of that sorted list. A front of fewer than `N` cells lowers `N` to its length (under 2: no delta — the mouth stays as it is and the plan is dropped).
- **The tree:** each branch is traced from its end BACK to the apex; where traced paths meet they share the rest. The union is a tree rooted at the apex; a node where MORE than two paths part is resolved by moving the highest-indexed extra branch's split one cell down its lowest-indexed sibling (repeat), so every node has at most two children. A reach is the path between two nodes: `down` (two slots) is therefore enough for any `N` — the apex reach names its two children, and each of them its own.
- **The trunk below the apex** is not kept as a separate river: analysis 2 routes the trunk's water through the plain to ONE sea cell by D8; `attach_deltas` cuts that reach at the apex and DISCARDS its lower part, replacing it with the tree's reaches (so the routed lower trunk and branch paths never lie side by side). Every leaf reach carries `1 / N` of the trunk's twelve flows, an inner reach the sum of its leaves.
- **Features:** this task adds `Delta` to `features::detect_objects` (one per plan: anchor = the apex, area = plain + lobe).
- The gate's `river_mouths`, `river_km` and `max_strahler` come from `Stats` BEFORE `attach_deltas` (trunks only); the branch totals are their own lines.
- `--report`: `deltas.stamped`, `deltas.branch_mouths`, `deltas.lobe_km2`, `deltas.plain_km2`, `rivers.reaches_with_two_down`, `rivers.distributary_km`.

- [ ] **Step 1: Temporary tests:** a hand-built coastal plain with one 50 m³/s trunk and a `Delta` mouth: at least 2 branches and, with a 400 m³/s trunk, 4 — every node of the tree has at most two children and every reach's `down` holds only reaches of the tree or `NO_REACH`; each branch a 4-connected-or-diagonal cell path from the apex to a sea cell, none uphill by more than 0.5 m; the lobe's cells were sea and are land at 0.5 m; the apex reach has two `down` entries and every other reach at most one; the branches' flows sum to the trunk's within 1 %; `drains_to_sea` is 1 and `sea_is_height` holds after the stamp; a delta drawn across x = 0 is the rolled delta of the rolled world; `--off deltas` changes nothing.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the table (8 dev, 2 Medium): this task's lines, `land_share` (lobes add land: quote), `coast_km`, the gate's lines (trunk numbers move only by the plain's re-routing: quote), the standing invariants. **Targets:** `deltas.stamped` ≥ 1 on each Medium seed with a `Delta` mouth of 1 m³/s or more; `rivers.reaches_with_two_down` = `deltas.stamped`. The world still loads and maps (`just world-map --tiles` on a dev world; the format is untouched). READ `t15-water.png` at two deltas.
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): deltas as a second layer — lobes, plains and distributary reaches from an apex`.

### Task 16: Coast stamps — barrier islands and lagoons, reefs and atolls, beaches and tidal flats (prototype first)

**Files:** `crates/worldgen/src/forge/stamps.rs` (`coast_stamps`), `forge/knobs.rs` (ADD "Coast" `barrier_islands`), `forge/features.rs` (`detect_objects`: lagoons), `forge/mod.rs` (`forge_with`: stage 16 of the stage list — after `deltas`, before the second analysis), the knob mirrors, `report.rs`, `forge/render.rs`.

**Interfaces — produces:**

```rust
pub const COAST_SMOOTH_CELLS: usize = 4;          // the shore is closed by a disc of this radius before it is offset
pub const LAGOON_CELLS: (usize, usize) = (4, 12); // lagoon width: wider on lower shores
pub const BARRIER_CELLS: usize = 3;
pub const BARRIER_HEIGHT_M: (f64, f64) = (1.5, 4.0);
pub const LAGOON_DEPTH_M: f64 = 1.5;
pub const INLET_SPACING_CELLS: [usize; 2] = [80, 30];   // micro-tidal, meso-tidal: "long islands" / "frequent inlets" (read); the numbers are not
pub const INLET_WIDTH_CELLS: usize = 2;
pub const BARRIER_MIN_RUN_CELLS: usize = 24;      // no barrier on a shorter stretch of eligible shore
pub const REEF_FLAT_CELLS: usize = 2;             // "fringing reefs usually < 100 m wide" (read)
pub const REEF_TOP_M: f64 = -1.0;
pub const ATOLL_RADIUS_DEG: f64 = 0.6;
pub const ATOLL_SEAMOUNT_M: (f64, f64) = (-600.0, 0.0);   // the planet record's elevation of a subsided hotspot cell
pub const BEACH_CELLS: usize = 2;
pub const BEACH_TOP_M: f64 = 1.5;
pub const FLAT_CELLS: usize = 3;
/// Barriers and lagoons (`features.barriers`), fringing reefs and atolls (`features.reefs`), beach and tidal-flat
/// profiles (`features.shores`), each on fine shore cells whose planet coast cell has the class. Ends with `post::flood`.
pub fn coast_stamps(w: &mut World, coast: &Coast, planet: &Planet, seed: u64, k: &Knobs, features: &Features, out: &mut Stamped);
```

- **Barrier**: `d` = distance into the sea from the land mask CLOSED by a disc of `COAST_SMOOTH_CELLS` (one wrapping chamfer); on a run of at least `BARRIER_MIN_RUN_CELLS` eligible shore cells with lagoon width `L` (from the shore height, in `LAGOON_CELLS`): sea cells with `d` in `[L, L + BARRIER_CELLS)` become land at a hashed height in `BARRIER_HEIGHT_M` — only where the cell 2 steps further out is open sea with larger `d` (the band does not fold inside a bay) — "out" being the one of the 8 neighbour directions in which `d` rises most per metre (ties: the lowest direction index), taken twice — and the initial water was shallower than the shelf edge; sea cells with `d < L` are raised to `−LAGOON_DEPTH_M`; an inlet `INLET_WIDTH_CELLS` wide every `INLET_SPACING_CELLS[tide class]` cells along the ridge, placed by `hash01(seed, INLET_SALT, run, n)`, keeps every lagoon cell 4-connected, through cells below 0, to the SEED sea: that is the stored-sea invariant, and the final `post::flood(w, 0.0, &cold.ocean)` would turn a sealed lagoon into LAND at `+SHORE_STEP_M` (`sea.sealed_cells`). So after the ridge and the inlets are laid, every lagoon component is checked for that connection and one more inlet is cut at its lowest ridge cell if it lacks it; `coast.lagoons_sealed` reports any the flood still sealed (expected 0). **Reef**: sea cells within `REEF_FLAT_CELLS` of a `Reef` shore, deeper than `−REEF_TOP_M`, raised to `REEF_TOP_M`. **Atoll**: for each planet cell with `kind == Hotspot`, oceanic crust, `elevation_m` in `ATOLL_SEAMOUNT_M`, sea at the fine grid, winter sea ≥ 18 °C, taken in hashed order and kept `2 · ATOLL_RADIUS_DEG` apart: a ring `BARRIER_CELLS` wide at radius `ATOLL_RADIUS_DEG` (at least 6 cells) at `REEF_TOP_M`, with islets at 1–2 m on a hashed third of it, and the inside raised to −20 m. **Beach** (`Sandy`, and the seaward side of a barrier): land within `BEACH_CELLS` of the sea lowered to at most `BEACH_TOP_M · (distance / BEACH_CELLS)`. **Tidal flat** (`Muddy`): land within `FLAT_CELLS` of the sea lowered to at most `SHORE_STEP_M + 0.25` and sea within `FLAT_CELLS` raised to at least −0.5. `Rocky`, `Glaciated`, `Ice`: no edit (the class is the stamp).
- **Features:** this task adds `Lagoon` to `features::detect_objects` (one per 4-connected component of `Stamp::Lagoon` cells of at least `FEATURE_MIN_CELLS` planet cells' area).
- `--report`: `coast.barrier_share` (Ruling 10: over the pre-stamp shore of stage 13's mask, islands included), `coast.lagoons_sealed`, `coast.barrier_islands` (land components made only of barrier cells), `coast.lagoons`, `coast.lagoon_km2`, `coast.reef_cells`, `coast.atolls`, `coast.beach_km`, `coast.flat_km`.

- [ ] **Step 1: The prototype, as temporary tests** on a hand-built 512 × 128 wrapped world at 32 m — a straight low coast 300 cells long with a 20-cell-wide bay in it, a `Coast` literal marking it all `Barrier`, micro-tidal: one ridge 3 cells wide runs 4–12 cells off the straight shore, does not enter the bay nor seal its mouth, is cut by 2-cell inlets, every lagoon cell is still sea after `post::flood`, and the share of the straight shore behind a barrier is above 0.8; the same coast marked `Rocky` gets nothing; meso-tidal has at least twice the inlets; `Reef`: a 2-cell flat at −1 m and no land made; an atoll: a ring with islets, a lagoon inside, 4-connected to the sea through its gaps; rolled world → rolled stamps. Print the coast as rows of characters and quote them.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the table (8 dev, 2 Medium, 2 Small): this task's lines, `coast_km`, `land_share`, the standing invariants. **First look (Task 22 tunes):** `coast.barrier_share` quoted per seed (8 dev at 32 m AND at 64 m, 2 Medium, 2 Small — every size has coast cells) and as the 8-seed dev median; with `--off barriers` it is 0; `coast.atolls` over the ten worlds quoted (0 everywhere is named for Task 22). READ `t16-coast.png` and `t16-relief.png` on Medium and on Dev: are the barriers islands, not a second coastline?
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): barrier islands and lagoons, fringing reefs and atolls, beach and tidal-flat profiles`.

## Slice F — the water table, names, storage, the runtime

### Task 17: The water table replaces `drainage`

**Files:** `crates/game/src/biome.rs` (`drainage_of_water_table`: the phase's first `game` edit — `just determinism-wasm` runs in this task), `crates/worldgen/examples/forge_proto/main.rs` (`PreviewLayer::Drainage` and its `-drainage.png` sheet become the water table's), `crates/worldgen/src/forge/fields.rs` (`water_table`; `Fields` gains `pub water_table_m: Vec<f32>`; `Fields::drainage` is now `drainage_index` of it; DELETE `drainage`, `drainage_from`, `drainage_at`, `flood_height_m`, `nearest_water`, `water_discharge`, `CHAMFER_SETTLED_M` and every `DRAINAGE_*` constant; `ground_slope`, `water_levels`, `permeability` stay — the belt lines, spawn and this field read them), `forge/mod.rs` (the module doc), `report.rs`, `forge/render.rs` and `examples/forge_proto/serve.rs` (their "drainage" panel becomes the water table), `crates/worldgen/src/habitat.rs` (a comment), `CLAUDE.md` if it names drainage (it does not today: check).

**Interfaces — produces:**

```rust
pub const WT_RUNOFF_FULL_MM: f64 = 300.0;     // annual runoff at which the table follows the ground as closely as the rock allows
pub const WT_FOLLOW_MAX: f64 = 0.9;
pub const WT_PERMEABILITY_LOSS: f64 = 0.5;
/// Depth of the water table under the ground (m, ≥ 0) per cell; 0 on water. Height above the nearest drainage
/// (the water surface the cell's receiver chain reaches first: a WET river cell, a lake, the sea) times
/// `1 − WT_FOLLOW_MAX · wet · (1 − WT_PERMEABILITY_LOSS · permeability)`, `wet = min(1, annual runoff / WT_RUNOFF_FULL_MM)`
/// of the cell's climate cell. One downstream-first pass.
pub fn water_table(w: &World, a: &Analysis, geo: &Geology) -> Vec<f32>;
/// Stored byte of a depth: round(4 · sqrt(depth)), saturating at 255.
pub fn water_table_q(depth_m: f32) -> u8;
/// Today's 0–100 drainage scale of a depth, for `game::classify` until Phase 5 — DEFINED on the stored byte, so the
/// forge and the runtime cannot disagree: `game::drainage_of_water_table(water_table_q(depth_m))`.
pub fn drainage_index(depth_m: f32) -> u8;
// game/src/biome.rs
/// The 0–100 drainage scale of a stored water-table byte `q` (depth = (q / 4)² m = q² / 16): piecewise linear through
/// (0 m, 0), (1 m, 33), (4 m, 66), (15 m, 100), 100 beyond; computed in INTEGERS on `d16 = q · q` (sixteenths of a
/// metre), each segment `lo + ((hi − lo) · (d16 − a16) · 2 + (b16 − a16)) / (2 · (b16 − a16))` (round half up),
/// by a `const fn` filling a `const [u8; 256]`. "Swamp-grade" (< 33) is a table within a metre of the ground.
pub const fn drainage_of_water_table(q: u8) -> u8;
```

- Every reader of `drainage` and what it reads after:

  | Reader | After this task | After Task 19 |
  |---|---|---|
  | `WorldArtifact::drainage`, `ByteRasterId::Drainage`, the tiles | the same byte raster, filled with `drainage_index(depth)` | the water-table raster (`water_table_q`) |
  | `WorldArtifact::cell_climate_at`, `ForgedWorld::cell_climate` → `game::CellClimate::drainage` → `classify`, `df_drainage` | unchanged code; the values are the index | `game::drainage_of_water_table(q)` (Task 19's table) |
  | `game::region_type(b, drainage)`, `regions::flood(biome, drainage, …)`, `regions::derive` | unchanged code | fed the same index, derived per cell |
  | `tile_map::drainage_color`, `examples/world_map.rs`'s drainage layer | unchanged | a water-table layer (depth) |
  | `forge/render.rs`, `serve.rs` | the water-table panel | — |

- `--report`: `water_table.depth_p50_m`, `.depth_p90_m`, `water_table.wet_share` (land with depth < 1 m), `water_table.wet_share.arid` / `.humid`, `water_table.oasis_cells` (arid land with depth < 2 m more than 3 cells from any water: Phase 5's oases).

- [ ] **Step 1: Temporary tests:** a hand-built slope to a wet river: depth is 0 at the river, grows with height above it, is smaller under a wet climate literal than a dry one and smaller on shale than on sandstone; a cell draining to a DRY reach reads the next wet water down the chain; `water_table_q(0.0) == 0`, and for `d` in 0–4,000 m the decoded `(q / 4)²` differs from `d` by at most half a step, `√d / 4 + 1/64` m (0.02 m at the surface, 0.3 m at 1.3 m, 2.5 m at 100 m); `drainage_of_water_table` at `q` = 0, 4, 8, 16 (0, 1, 4, 16 m) is 0, 33, 66, 100, is monotone over all 256 values, and `drainage_index(d) == drainage_of_water_table(water_table_q(d))` by definition.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the table (8 dev, 2 Medium): this task's lines; every `biome_share.*` before / after (swamp, marsh, mangrove, glacier and tundra move: quote the five that moved most and the count of biomes present — a biome that vanishes on all ten worlds is named for Task 21); region counts; `height_crc` identical to Task 16's (the terrain does not move). **Targets:** `water_table.wet_share.humid` > `.arid` on every seed that has both; total wet share 0.02–0.3.
- [ ] **Step 4: Verify, in order** — including `just determinism-wasm` (`game` gained the table). Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge)!: a water-table field (height above drainage) replaces the drainage field`.

### Task 18: Names (changes no world)

**Files:** Create `crates/worldgen/src/forge/names.rs` (`pub mod names;`); Modify `forge/features.rs` (`Found` gains `pub name: String`; stage 19: `name_all` after `detect_objects` — names every feature at its end), `report.rs`, `forge/render.rs` (`-features.png` unchanged; `forge_proto --out <stem>` also writes `<stem>-features.txt`: kind, name, x, z, area — one line each), `justfile` (the `forge-proto` comment: the new file).

**Interfaces — produces:**

```rust
pub const NAME_SALT: u64 = 0x4E41_4D45;
/// A specific name of 2–3 syllables from fixed onset / nucleus / coda tables, capitalised; a pure function of its arguments.
pub fn specific(seed: u64, index: u64, attempt: u64) -> String;
/// The full name of a feature: the specific joined with the kind's generic term by the kind's pattern
/// ("Gulf of {}", "{} Bay", "{} Cove", "{} Strait", "{} Peninsula", "Cape {}", "Isthmus of {}", "{} Island",
/// "{}" for a continent, "{} Mountains", "Lake {}", "{} River" / "{} Wadi" for a dry or ephemeral one,
/// "{} Falls", "{} Delta", "{} Lagoon", "{} Glacier", "{} Sea", "{} Ocean").
pub fn full(kind: FeatureKind, specific: &str, dry: bool) -> String;
/// Names every feature in order; a specific already used in this world is re-drawn with the next attempt.
pub fn name_all(seed: u64, found: &mut [Found]);
```

- A river and the delta at its mouth, a lake and its glacier, share nothing by design in this phase (each draws its own): Known limit.
- `--report`: `features.named`, `features.names_unique` (yes/no), `features.name_len_max`, and the first five names of the world on one line (`features.sample`).

- [ ] **Step 1: Temporary tests:** `specific(seed, i, 0)` is the same on every call, and the 1,000 names of 1,000 consecutive `i` hold at least 950 DISTINCT strings; every name is ASCII letters with one capital; `name_all` on 500 features yields 500 distinct specifics; `full(Cape, "Arn", false) == "Cape Arn"`, `full(River, "Arn", true) == "Arn Wadi"`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the safety net (no world moves; lines appended only). `features.named` per seed against Ruling 14's targets (≥ 5 on Dev, ≥ 20 on Medium: a first look), `features.names_unique: yes` on all ten; quote `t18-features.txt` of one Medium seed in full.
- [ ] **Step 4: Verify, in order.** Delete the temporary tests.
- [ ] **Step 5: Commit** `feat(forge): a seeded name generator; every detected feature named`.

### Task 19: Store it — the one format bump

**At dispatch the controller tells the owner:** once this commit lands, `worlds/debug-world`, every `just play` world and every dev store refuse to load until re-forged (`just run --reset`, `just pg-reset`).

**Files:**
- Modify: `crates/worldgen/src/forged.rs`: `ARTIFACT_VERSION` 11 → 12 (its doc: "v12: `water`, reach and lake attributes, the ice and water-table rasters"); `RiverReach`, `Lake`, `Segment`; new `WaterRecord`, `CoastRecord`, `IceRecord`, `Mouth`, `Waterfall`, `Feature`; `WorldArtifact` gains `pub water: WaterRecord` (last field) and `pub ice: Vec<u8>`; `drainage: Vec<u8>` becomes `water_table: Vec<u8>`; `ByteRasterId` (`Drainage` → `WaterTable` in its slot; `Ice` appended AFTER Phase 2's `Province`: `ALL` is SEVEN); `byte_rasters` / `byte_rasters_mut` (seven); `to_bytes` / `from_bytes` (the record's planes row-delta encoded by the PLANET grid's `nx`, as Phase 2's record is); `validate` (the record's grid is `PLANET_NX × PLANET_NZ`; every enum byte is in range; every `down` and `name` index is in range or the sentinel; reach flows finite and ≥ 0); `cell_climate_at`; `WholeRasters::new` (a segment's `depth` and `wet` from the reach; a playa's cells make no lake run); `Rasters` (`water()`); `ForgedWorld::water()`
- Modify: `crates/worldgen/src/forged_tiles.rs`: `TILED_FORMAT_VERSION` 4 → 5; `Tile::bytes` is `[Vec<u8>; 7]` (and `Tile::decoded_bytes`, `encode_tile` / `decode_tile` follow — `Segment` is part of the tile format through `Tile::segments`, so `wet` and the reach's depth travel there); `split` builds `Tile::lake_runs` from every lake but a playa, as `WholeRasters::new` does; `LakeRecord` gains the lake attributes; `ForgedMeta` gains `pub water: WaterRecord` (last); `ForgedMeta::validate`; `split`; `encode_meta` / `decode_meta`; `TileCache` holds the record
- NOT here: `crates/worldgen/src/tile_map.rs` — `build_world_map`'s output does not change in this task, so `WORLD_MAP_VERSION` stays 3 until Task 20 (the one task that changes what it returns)
- No `game` edit here: `game::drainage_of_water_table` exists since Task 17; this task only moves its call from the forge (`drainage_index` filling the old raster) to the readers of the stored water table
- Modify: `crates/worldgen/src/forge/mod.rs` (`Forged::artifact` fills everything), `forge/regions.rs` (`derive` maps the stored water table through `drainage_of_water_table`), `examples/world_map.rs` (it names the drainage raster), `report.rs`, `examples/forge_proto/main.rs` if it names a raster id, `crates/world-store/` only if it names `ByteRasterId` (expected: no change)
- Modify: `CLAUDE.md` (Worldgen paragraph: the water record, where it lives, the versions), `TODO.md` (re-forge every dev world, the web world, dev Postgres worlds, the twin and orion), the design (Decision 10's objects layer: what is stored; the Phases table's row 4 as built)

**Interfaces — produces:**

```rust
pub const ICE_STEP_M: f32 = 2.0;
pub const NO_NAME: u32 = u32::MAX;
pub struct RiverReach { pub order: u32, pub discharge_m2: f64, pub width_m: f32, pub points: Vec<RiverPoint>,
    pub depth_m: f32, pub flow_m3s: [f32; 12], pub regime: u8, pub pattern: u8, pub down: [u32; 2], pub name: u32 }
pub struct Lake { pub level: f32, pub area_m2: f32, pub max_depth: f32, pub outlet: [f32; 2], pub cells: Vec<(u32, u32)>,
    pub kind: u8, pub origin: u8, pub level_range_m: f32, pub ice_months: u8, pub name: u32 }
pub struct Segment { /* a, b, water, half_width, depth as today */ pub(crate) wet: bool }
pub struct Mouth { pub at: [f32; 2], pub reach: u32, pub class: u8, pub delta: u8, pub flow_m3s: f32, pub sediment_m3_per_yr: f32, pub inlet_m: f32, pub name: u32 }
pub struct Waterfall { pub at: [f32; 2], pub drop_m: f32, pub flow_m3s: f32, pub reach: u32, pub name: u32 }
pub struct Feature { pub kind: u8, pub name: String, pub anchor: [f32; 2], pub extent_m: f32, pub area_m2: f32, pub object: u32 }
pub struct CoastRecord { pub class: Vec<u8>, pub setting: Vec<u8>, pub exposure: Vec<u8> /* 255 per 1 */,
    pub tide_dm: Vec<u8>, pub sediment: Vec<u8>, pub winter_sea_c: Vec<i8> }
pub struct IceRecord { pub ela_now_m: Vec<i16>, pub ela_cold_m: Vec<i16>, pub sheet_now_m: Vec<u16>, pub sheet_cold_m: Vec<u16>,
    pub ice_max: Vec<u8>, pub sea_ice_months: Vec<u8> }
/// Resident: whole in the world file and in `ForgedMeta`, on the planet grid (Phase 2's `nx`, `nz`).
pub struct WaterRecord { pub nx: u32, pub nz: u32, pub coast: CoastRecord, pub ice: IceRecord,
    pub mouths: Vec<Mouth>, pub waterfalls: Vec<Waterfall>, pub features: Vec<Feature> }
impl ForgedWorld { pub fn water(&self) -> &WaterRecord; }
```

- A playa is stored as a `Lake` with `kind == Playa` (its `cells` are the dry bed, for Phase 5's cover) and makes no lake run; `lake_level_of` never answers for it.
- The RUNTIME's behaviour in this task changes in exactly two ways the stored data forces (the rest is Task 20): a segment's depth is the reach's `depth_m` — × `BRAID_DEPTH_FACTOR` for a braided reach — (clamped to `CHANNEL_DEPTH_MIN_M … CHANNEL_DEPTH_MAX_M`), and `drainage` reads go through the table. `Segment::wet` is stored and not yet read.
- `--report`: `world_file_bytes` (old → new), `water.record_bytes_raw`, `water.record_bytes_packed`, `water.clamped_values` (values saturated by an encoding), `water_roundtrip_ok` (`from_bytes(to_bytes(a)) == a`), `water_tiles_identical` and `ice_tiles_identical` (a tiled world — `split` → encode → decode → `MemTiles` — returns the same record, the same seven byte rasters at 50,000 sampled cells and the same segments in 2,000 sampled buckets as the whole one), `sea_is_height` on the artifact.

- [ ] **Step 0: Before** — the three reports saved (`t19-before*.txt`).
- [ ] **Step 1: Temporary tests:** the round trip of a forged dev artifact compares `==` (print a one-line summary on a mismatch, never the `Debug`); a version-11 header is refused with text containing `re-forge`; a `ForgedMeta` blob of format 4 is refused by `decode_meta`'s caller the same way (the SQLite path through `world_store::load_forged`; the Postgres path is the same function — Phase 1b's Ruling 17, argued not run); an artifact with a `down` index out of range, an enum byte out of range, a NaN flow or a record on another grid fails `validate` naming the field; a playa makes no lake run in the whole world NOR in a tile (`Tile::lake_runs` of the tile holding it); a tile round-trips `Segment::wet`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the three reports: every line of `t19-before*` unchanged except the exemptions — `world_file_bytes`, `artifact_rasters_crc` (the raster set changed: quote), `artifact_header_crc` if the knob map's bytes moved (they should not: quote); biome shares are IDENTICAL (Task 17 already defined the index on the stored byte: a moved share is BLOCKED); `height_crc` identical. The new lines. `just forge-proto --size dev --cell 32 --out target/world-forge/p4/t19` and `just world-map --world target/world-forge/p4/t19.world --tiles --out target/world-forge/p4/t19-map.png`: it loads and maps. An old world: `just world-map --world target/world-forge/p4/t7.world --tiles` is refused with the re-forge wording (quote).
- [ ] **Step 4: Verify, in order.**
- [ ] **Step 5: Commit** `feat(worldgen)!: the water record, reach and lake attributes, ice and water-table rasters in the world file and the tiles (artifact v12, tiles v5)`.

### Task 20: The runtime and the map read it

**Files:**
- Modify: `crates/worldgen/src/forged.rs`: `refine` (water only from a `wet` segment; the channel is cut either way), `ColumnClimate` (gains `pub ice_m: f32` — the bilinear read of the ice raster × `ICE_STEP_M`; `glacier_blocks` = `round(ice_m)` clamped to `0 ..= GLACIER_MAX_BLOCKS` — NOT multiplied by `glacier_share` any more (the stored thickness already thins to its edge: multiplying again would thin it twice); `GLACIER_MAX_BLOCKS` 40 → 120, `GLACIER_MIN_BLOCKS` deleted; `glacier_share` stays only if a reader of it is left after this change — grep; with none it is deleted with its bilinear read), `column_climate`, `cell_climate` and `WorldArtifact::cell_climate_at` (set `ice`), `forged_tiles::read_radius_cells` if the new bilinear read reaches further than today's (it must stay within `MAX_READ_RADIUS_CELLS`)
- Modify: `crates/game/src/biome.rs`: `CellClimate` gains `pub ice: bool`; `classify`: after the lake and sea rules, `if c.ice { return Biome::Glacier }`; the two temperature rules that returned `Glacier` return `Tundra` (`GLACIER_C`, `GLACIER_MIN_DRAINAGE` deleted with them); every `CellClimate {` literal (`grep -rn "CellClimate {" crates`)
- Modify: `crates/game/src/world_map.rs`: `WorldMap` gains `pub features: Vec<MapFeature>` (`MapFeature { kind: u8, name: String, tile: (u32, u32), extent_tiles: f32 }`); **`WorldMap` is on the WIRE** (`ServerPacket::WorldMap`, `game/src/protocol.rs`): this is the phase's one wire-layout change (Global Constraints) — client and server are one build, no version is negotiated, nothing else is needed, and the commit message says so; every `WorldMap {` literal — `crates/game/tests/{wasm_determinism,history_handover,peer_schedule}.rs`, `crates/server/tests/common/hosts_rig.rs`, `crates/world-store/src/{lib,forged}.rs`, `crates/worldgen/src/tile_map.rs` (the gate file `wasm_determinism.rs` among them: add `features: Vec::new()` only — its pinned hashes must not move; `SocietyGrid::world_id` hashes the fields it names, not the whole map: confirm, and if a pin moves, BLOCKED)
- Modify: `crates/worldgen/src/tile_map.rs`: `WORLD_MAP_VERSION` 3 → 4; `build_world_map` fills `features` from `ForgedWorld::water()`; `reaches_by_tile` counts WET reaches only (`TileSummary::rivers`, `river_max_*`); the tile description lines name the features whose anchor tile is the tile or whose extent covers it (at most three, largest first), a mouth's class, a lake's kind ("salt lake", "dry lake bed"), the reach regime of the tile's largest river
- Modify: `crates/worldgen/src/lib.rs` only where it calls `glacier_blocks` / builds `CellClimate`; `crates/worldgen/examples/world_map.rs`: `ForgedLayer::{Ice, Coast, WaterTable, Regime, Features}` and their `--layer` names; `forge/quality.rs` (`world_score`'s "glacier" term reads the stored biome: unchanged code — confirm it still scores on a polar world); `justfile` (the `world-map` comment: the layers)
- Modify: `CLAUDE.md` (Worldgen paragraph), `TODO.md` (Owner hand checks of this phase, below)

**Interfaces — produces:** as listed in Files; `game::CellClimate { height_m, temperature_c, precip_mm, drainage, salinity, is_lake, ice }`.

- [ ] **Step 1: Temporary tests:** a world with one dry and one wet reach: `sample` over the dry channel returns a bed below the banks and `water_y == None`, over the wet one a water surface; a waterfall's two columns 2 m apart differ in water level by the drop (± 1 block); a column over 37 m of forged ice (stored in `ICE_STEP_M = 2` m steps as 36 or 38 m) has 36 or 38 glacier blocks, the same at full and at zero `glacier_share`, and its neighbour over none has 0; `classify` with `ice` is `Glacier` whatever the temperature, and never `Glacier` without it; a tiled world gives the same `column_climate` as the whole one at 2,000 points, across a tile border and across the seam; `aliased_regions_identical` still yes; the world map of a dev world carries its features and the description of a lake's tile names the lake.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — **the seam residue (D28):** `rivers_across_seam`, its gap line and Task 7's `rivers_seam_columns_max_gap_m` before / after on the 8 dev seeds — now through the runtime that dries dry reaches: on a WET reach across the seam the largest gap must be under 8 m (one bucket's worth is a finding to fix here: it is the sampler's); on a DRY one there is no water by design (say which the Phase 1a riffle was). The three reports: `biome_share.Glacier` and `.Tundra` before / after (the biome now follows the ice), `aliased_regions_identical`, `seam_region_cliff`, `world_map_wraps`, the seam summary. `just world-map --world target/world-forge/p4/t20.world --tiles --out target/world-forge/p4/t20-map.png` and each new `--layer`: paths recorded; READ the ice and coast layers. Region generation time: `just forge-report`'s existing runtime lines generate regions — quote the wall time of the report before / after (a region under thick ice must not take more than twice as long: else say so for the Owner hand check).
- [ ] **Step 4: Verify, in order** — including `just determinism-wasm`, and `just test-one server "--no-run --tests"` and `just test-one world-store "--no-run --tests"` (their `WorldMap {` literals are in test and library code `just smoke` does not compile).
- [ ] **Step 5: Commit** `feat(worldgen)!: the runtime reads the water record — dry reaches, glacier ice from its thickness, named features on the world map (world map v4)`.

## Slice G — tuning, evidence, close

### Task 21: Tune the sea, the sediment, the rivers and the lakes (at most four rounds)

**Files:** defaults in `forge/knobs.rs`, constants in `forge/{world,water,hydrology,post}.rs`, `forge/presets.rs` ("Dry" / "Wet" where a default they sit beside moved).

**Dials:** `low_stand_m`, `LOW_STAND_FULL_PEAK_M`, `shelf_width_deg`, `cold_stage_iters`, `land_fraction` (Ruling 4), `deposition`, `depression_fill`, `LOSS_WIDTH_FACTOR`, `river_width_coeff`, `river_depth_coeff`, `BRAID_LOAD_MIN`, `FALL_MIN_DROP_M`, `OPEN_WATER_PET_FACTOR`, `PLAYA_MAX_DEPTH_M`.

**Targets** (8 dev seeds at 32 m; 2 Medium and 2 Small seeds at 32 m, which carry the lake densities; each also with its off switch, Ruling 14's table): `land_share` 8-seed median within ± 0.02 of Phase 3's closing median and every seed in 0.15–0.6; `sea.coast_km_ratio` ≥ 1.15 (Medium mean) and 0.97–1.03 with `--off lowstand`; `sea.land_lost_share` ≤ 0.15; `sediment.deposited_share` 0.1–0.6, `sediment.fans` ≥ 1 per Medium seed, both 0 with `--off deposition`; `rivers.runoff_ratio` 0.15–0.6; `rivers.width_max_m` ≤ 400 on Medium; `rivers.braided_share` 0.02–0.3; `rivers.falls` ≥ 1 per Medium seed; `lakes.humid_arid_ratio` ≥ 2 on the Medium pair pooled AND on the Small pair pooled (`just forge-report --size small --cell 32 --seeds 2`), each side with at least 10 bodies — `n/a` is recorded as "not measurable on this pair", not as a pass; the dev lines are printed, never judged (Ruling 14); `lakes.closed_in_humid` = 0 is the program's reading — a non-zero count after the last round is a ruling quoting each body (Task 8: the balance closes lakes up to aridity `OPEN_WATER_PET_FACTOR`); `lakes.closed` ≥ 1 over the Medium pair, 0 with `--off lake_balance`; `lakes.per_1000km2.humid` ≥ 5 on Medium (a floor so "humid has more" is not 0.02 against 0.01).

- [ ] **Rounds 1–4** (stop at the first that meets every target): change dials — when `land_fraction`'s default moves, the plate stage re-runs and its score may pick another attempt: quote `planet.attempt` and `planet.ok` per seed before / after; run `just forge-report --size dev --cell 32 --seeds 8`, `--size small --cell 32 --seeds 2`, `--size medium --cell 32 --seeds 2`, and the off runs of the lines that moved; READ `-water.png` and `-relief.png` of both Medium seeds; record the table (round, dials, every target line) in the report.
- [ ] **After the last round:** every remaining miss is a `Ruling:` with its measured number and what would be needed (a mechanism, not a dial) — no fifth round. Run the standing invariants on the final defaults.
- [ ] **Verify** — `just smoke`, `just web-check`; the gate WITHOUT bless (quote).
- [ ] **Commit** `tune(forge): the low stand, sediment, rivers and lakes`.

### Task 22: Tune the ice and the coasts (at most four rounds)

**Dials:** `ice_age_cooling_c`, `glacial_erosion`, `ICE_FULL_ABOVE_M`, `GLACIER_WIDTH_COEFF`, the swath radius, `GLACIAL_CUT_MAX_M`, `KETTLE_DENSITY_PER_KM2`, `tide_m`, `FUNNEL_GAIN`, `EXPOSURE_FULL`, `BARRIER_MIN_EXPOSURE`, `BARRIER_MIN_SEDIMENT`, `barrier_islands`, `LAGOON_CELLS`, `INLET_SPACING_CELLS`, `ATOLL_SEAMOUNT_M`, the mouth rule's four constants.

**Targets:** `ice.sheet_land_share.cold` 0.05–0.4 (8-seed median); `ice.u_valley_ratio` ≥ 1.3 on the Medium pair and 0.9–1.1 with `--off glacial`; `mouths.fjords_equatorward` = 0 and `mouths.class.fjord` ≥ 1 over the Medium pair, 0 with `--off cooling`; at least 3 mouth classes over the Medium pair; **`coast.barrier_share` 0.05–0.20** as the 8-dev-seed median AND on each Medium seed, 0 with `--off barriers`; `tide.class.macro` ≤ 0.15 of coast; `coast.atolls` ≥ 1 on at least 2 of 8 Medium-size seeds (`just forge-report --size medium --cell 64 --seeds 8` for this line only) or a ruling on why not; `features.named` ≥ 20 on Medium and ≥ 5 on Dev.

- [ ] **Rounds 1–4**, as Task 21, reading `-ice.png`, `-coast.png`, `-features.png`.
- [ ] **After the last round:** remaining misses as rulings. The standing invariants.
- [ ] **Verify** — `just smoke`, `just web-check`; the gate WITHOUT bless (quote).
- [ ] **Commit** `tune(forge): the ice age, glacial erosion, tides and coast classes`.

### Task 23: Maps, and the evidence across sizes

**Files:** none expected (a defect found here is fixed here, in its file, with its own before / after).

- [ ] **Step 1: Every size once** — `just forge-report --size <dev|tiny|small|medium> --cell 32 --seeds 2` and `--size large --cell 32 --seeds 1` (if `free -g` shows 6 GB available; else "not run"): one table, size by size, of `sea.low_stand_m`, `sea.coast_km_ratio`, `sea.drowned_valley_km`, `lakes`, the two densities, `rivers.wet_reaches`, `rivers.falls`, `ice.max_share`, `ice.valley_glacier_cells.cold`, `ice.sheet_cells.cold`, `mouths.class.*`, `coast.barrier_share`, `coast.atolls`, `features.named`, and the standing invariants. This is where Ruling 2's and Ruling 9's small-world expectations are confirmed or corrected in "Small worlds" above.
- [ ] **Step 2: Maps for the owner** — `just forge-proto --size medium --cell 32 --out target/world-forge/phase4-medium` (and `--size dev`), then `just world-map --world … --tiles --out target/world-forge/phase4-medium-map.png` and each new layer: paths recorded for the program's Evidence. READ the four forge sheets and the biome map of the Medium world and write five sentences: do the coasts read as drowned valleys, deltas and barriers; do lakes sit where ice or dry basins put them; is the ice where it is cold.
- [ ] **Step 3: Verify** — `just smoke` if anything changed.
- [ ] **Step 4: Commit** (only if a file changed) `fix(forge): …` with what was found.

### Task 24: Evidence, the gate re-record, every size measured, close

**Files:** `crates/worldgen/tests/fixtures/forge_quality_planet_dev_32m.ron` (the phase's ONE re-record), `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` ("Measurements": a "Phase 4" table; Decision 4's Phase 4 line and the Phases table's row as built), `crates/worldgen/src/forge/play.rs` (`FORGE_SECS_PER_MCELL`, `FORGE_COST_EXPONENT`, `FORGE_RAM_*` refitted, with the date and machine), `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Evidence: Phase 4; Owner hand checks), `TODO.md`, `CLAUDE.md` (a last read of the Worldgen paragraph), this file ("As built").

- [ ] **Step 1: The phase's evidence run** — `just forge-report --size dev --cell 32 --seeds 8` and `just forge-report --size medium --cell 32 --seeds 2` (every line), and for each row of Ruling 14's table the same run with its off switch (the judged lines only). Pasted into the program's Evidence under "Phase 4", judged row by row: the on-run meets the target AND the off-run fails it — a row whose off-run also passes is recorded as NOT evidence, with a ruling.
- [ ] **Step 2: The gate** — `FORGE_QUALITY_BLESS=1 just test-one worldgen "--test it -- --include-ignored forge_lib::forge_quality_matches_baseline"`, then the same without bless (passes), in a commit of its own whose message states the old numbers (the fixture as Phase 3 left it: `git show HEAD:crates/worldgen/tests/fixtures/forge_quality_planet_dev_32m.ron`) and the new, field by field.
- [ ] **Step 3: Measure** (release, one at a time, nothing else of ours running) — `just forge-bench 16384 8192 32`, each of Tiny, Small, Medium at 16 m and 32 m, Large at 32 m; Large at 16 m only if `free -g` shows more than 20 GB available, else "not run: needs about 15 GB" (13.5 GB + this phase's 1.5 GB: the re-stand builds no second world). For each: wall seconds, max RSS, `.world` size; note `scaling_cur_freq`. Record beside Phase 3's rows in the design; compare with "Cost" above stage by stage (the forge prints its stage times when not quiet: cold stage, glaciers, stamps, flows, planet-grid work) and say which estimate was off by more than 2×. Refit the estimate constants (a temporary test: within ± 25 % of every measured row; deleted).
- [ ] **Step 4: Whole suites, once** — `just test` in the foreground; quote the PASS/FAIL line per stage. A failure in this plan's files is fixed here; one outside them is recorded with the proof it is not this phase's.
- [ ] **Step 5: Owner hand checks** appended to the program: a new debug world (`just run --reset`) — walk a drowned valley from its head to the sea, stand on a barrier island and look across the lagoon, find a waterfall from the map and look at it, walk up a glacier, cross a dry river bed in a dry belt; the world map's names and the tile text; generation time under thick ice; every dev world, the web world, dev Postgres worlds, the twin and orion re-forged before they run this build.
- [ ] **Step 6: "As built"** in this file (commits per task, every ruling, deferred minors), `TODO.md`, and the commit `docs: Phase 4 measurements and evidence; forge estimate refitted`.

## Known limits (not in this plan)

- **The low stand is one short stage**, not a history: no transgression rate, no drowned beach lines, no rebound of ice-loaded land (raised beaches, shallow inland seas). One cold stage stands in for many (the research's own inference).
- **The land clamp does not fully go, and the low sea is not 120 m everywhere** (Rulings 2, 3, 24): no land and no lake floor below the sea — a basin the flood does not reach is raised to 0.125 m, because the runtime has no sea mask; the stand is 13–33 m on Dev. `sea.raised_cells` says how much.
- **Small worlds hold ice by latitude** (Ruling 9): with true-scale lapse and a few hundred metres of relief, Dev and Tiny have few or no alpine glaciers, so no cirque lakes and few fjords there.
- **Ice sheets do not flow, float or erode**; there is no ice shelf, no iceberg lane, no areal scour (shield lakes), no isostatic depression. Valley-glacier width is a power of ice flux with a tuned constant; the U shape comes from a distance swath; cirques, arêtes and horns are not made.
- **Stamped, not simulated:** moraines and kettles from recorded margins; no drumlins, eskers, erratics. Barrier islands are an offset band (no spits, tombolos, recurved tips, ebb shoals); reefs are fringing flats (no barrier or patch reefs); atolls are rings on subsided hotspot cells. Sea cliffs, platforms, stacks and arches are not made (the last three cannot be a heightfield).
- **Sediment deposition delivers FANS only** (Ruling 18): floodplains stay rule-based (`carve_floodplains`) and basin fill stays the depression fill (`depression_fill`) — two of the program's three named landforms do not come from the solver. Deposition is time-lagged and channel-wide by one cell; a deposit adds no isostatic load (Phase 2's rebound counts erosion only); lakes trap sediment but silt at the old fixed rate; there is no alluvium rock (the rock under a fan is the column's). `carve_floodplains` still planes valley floors by rule. No meanders, oxbows, levees or braid bars: a braided reach is a wider, shallower channel.
- **Lakes: one balance per spill-level body.** Sub-basins of a closed body share a level; a river entering a closed basin stops at the old spill-level shore; lake silting and lake evaporation do not feed back into the climate's rain. Warm wet unglaciated lowlands may have few lakes (no process there makes basins). Origins the forge cannot tell (oxbow, landslide, karst, thermokarst, crater, impact) are absent.
- **Runoff is per climate cell**, loss in channels is one tuned factor, and the river NETWORK is still the erosion's rain-weighted area: a dry reach is stored and carved as a dry bed.
- **Tides are a rule** (an open-ocean range, damped in enclosed seas, amplified in detected bays): no amphidromes, no resonance length, no shelf term. Wave exposure is fetch along 8 directions on the planet grid times the mean squared onshore wind.
- **A `Rocky` shore is a no-op** (the class is stored; no cliff or platform is cut), and the sand and mud covers of the other shores are Phase 5's (Ruling 23).
- **Mouth and coast classes are this plan's own ordered rules**; Galloway's and Nienhuis' quantitative criteria were not read. The 5–20 % barrier band is measured against the whole pre-stamp shore (islands included), one definition among several.
- **Stored, not played:** the twelve flows per reach, a lake's seasonal range, tidal ranges, sea-ice and lake-ice months. Sea and lake ice blocks still follow the annual temperature (Phase 6). A waterfall is a 2 m chute with a one-stair step of water on interpolated ground, banked by its levee — not a cliff with a falling sheet.
- **Names are syllables**: no language, no history, no link between a river and its delta or a bay and its town. The in-game map shows them as tile text, not as labels on the map.
- **Phase 5 owns** the shore strip's cover (sand, mud, reef), wetlands from the water table, oases and springs, the classifier's passes (this phase adds only `CellClimate::ice` and keeps today's drainage scale through a lookup), aquifer classes.
- **The quality gate still guards land share, hypsometry and rivers only**; everything this phase adds rests on `forge_proto --report`.

## As built

(Filled by Task 24: commits per task, rulings in ledger order, deferred minors.)
