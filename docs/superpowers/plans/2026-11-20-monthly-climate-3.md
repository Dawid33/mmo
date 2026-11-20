# Monthly Climate (Phase 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Written 2026-11-20** from a code check of that day (tree at `758431e7` plus Phase 1b's in-flight working-tree
> changes in `crates/game` and `crates/worldgen`; Phase 1b IN PROGRESS, Phase 2 PLANNED, neither built). It is
> dispatched only after Phase 2 is DONE. Search by the named symbol, never by line. **No step commits a test.**
> It has NOT had its builder check yet (the program's loop, step 3).

**Goal:** Every forged world's climate comes out of one twelve-month model: temperature whose seasons, lag and continentality follow from heat capacities; pressure belts and winds that migrate with the sun and bend round the continents; rain that follows the wind, the thermal equator, the subtropical highs and the mountains — stored as Phase 1b's monthly normals, with the fine annual rasters derived from them.

**Architecture:** Two stages. (1) A **planetary stage** (`forge/climate/`: `insolation`, `ebm`, `circulation`, `ocean`, `moisture`) on a FIXED 256 × 128 model grid, run ONCE per forge from the world's land mask and four knobs: closed-form insolation → a seasonal energy-balance temperature (land and ocean heat capacity, diffusion, wind advection, ice ramp, outgoing radiation solved for a 15 °C mean) → monthly pressure and wind → ocean-current anomalies by basin side → a steady moisture transport giving the monthly precipitation of FLAT ground. It reads no height. (2) A **fine stage** (`forge/climate/fine.rs`) on the forge grid, re-run as erosion changes the relief: four axis sweeps (wind from W, E, N, S) give each fine cell a windward / lee factor per direction, blended by the model's wind — today's row sweep generalised. The annual fine rasters are DERIVED from the monthly family; the container's format does not change and no version constant is bumped. Köppen, the synthetic worlds and every derived field live forge-side (`forge/koppen.rs`, `forge/climate/derived.rs`, `forge/climate/synthetic.rs`), never in `game` or `forged.rs`.

**Tech Stack:** Rust; crate `worldgen` only (the forge, the `forge_proto` and `world_map` examples). `game` is NOT touched. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` (Intent; Goals 1, 5, 6; Non-goals; Approach A; Decisions 4, 6–9, 12; the Phases table; "Open questions": ocean currents). Program: `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Phase 3; its Standing rules bind every task). Research: `docs/research/research_notes/Earth like world generation overhaul/climate_simulation.md` (all), the report's climate section and evidence ledger.

Ledger: `.superpowers/sdd/2026-11-20-monthly-climate-3/progress.md` (git-ignored workspace). The commit procedure, implementer and reviewer rules are Phase 1a's: `.superpowers/sdd/2026-11-14-planet-frame-1a/{common,implementer-rules,reviewer-rules}.md`.

## Depends on (planned, not built on 2026-11-20 — the controller RE-READS each at dispatch)

**Phase 1b** (`2026-11-20-monthly-climate-container-1b.md`; its Tasks 1–2 were in the working tree, uncommitted, when this was written):
- `game::MonthlyClimate { nx, nz, cell_m, wrap_x, temp_sea_centi_c, diurnal_deci_c, precip_deci_mm, wet_day_255, wind_east_q, wind_south_q }`, `game::MonthlyNormals` and its methods (`warmest_month`, `coldest_month`, `driest_month`, `wettest_month`, `annual_mean_temp_c`, `annual_precip_mm`, `lowered_by`), `game::{CLIMATE_MONTHS, SOLSTICE_NORTH_SUMMER_MONTHS, NORTH_MIDSUMMER_MONTH, NORTH_MIDWINTER_MONTH, CLIMATE_*_PER_*}`.
- In `forge/climate.rs`: `CLIMATE_CELL_M`, `CLIMATE_MIN_ROWS`, `climate_stride`, `check_climate_grid`, `latitude_at`, `MonthlyFields { nx, nz, cell_m, temp_sea_c, diurnal_c, precip_mm, wet_day, wind_east_ms, wind_south_ms }`, `MonthlyFields::{quantise, clamped_values}`, `monthly_placeholder` and its seventeen `SEASON_*` / `DIURNAL_*` / `WET_DAY_*` / `PRECIP_SEASON_*` / `WIND_PLACEHOLDER_MS` / `CONTINENTALITY_HALF_DEG` constants (this plan DELETES the placeholder and those constants).
- `WorldArtifact::climate`, `ForgedMeta::climate`, `validate_climate`, `forged::ground_normals(sea, ground_m, annual_precip_mm, lapse_c_per_m)`, `ForgedWorld::{monthly_climate, monthly_normals, monthly_at_ground, monthly_at_ground_map, annual_climate, lapse_per_m}`.
- `--report` lines: `artifact_rasters_crc`, `climate_grid`, `climate_clamped_values`, `sea_temp_jan_c.<band>`, `sea_temp_jul_c.<band>`, `seasonal_swing_c.<band>`, `monthly_temp_mean_err_c`, `monthly_precip_sum_err_mm`, `seam_*_monthly_temp`, `seam_*_monthly_precip`, `world_file_bytes`, `climate_bytes_*`, `climate_roundtrip_*`, `climate_tiles_identical`, `monthly_aliased_identical`, `monthly_precip_conserved_max_rel`, `monthly_annual_temp_gap_c`. `world_map`'s `ForgedLayer::{MonthlyTemperature, MonthlyRain}` and `--month`.
- `ARTIFACT_VERSION` 10, `TILED_FORMAT_VERSION` 3.

**Phase 2** (`2026-11-20-plates-and-aged-relief-2.md`):
- `forge::planet::{PLANET_NX = 512, PLANET_NZ = 256, Planet, planet_for}`, `World::new(p: &ForgeParams, planet: &Planet)`, whatever fields Tasks 12–13 add to `World` (the synthetic worlds of Task 1 build a `World` literal), `forge_with`'s shape (the planet fetched once; `post::fit_uplift_rate` after the coarsest level; `World::scale_uplift`; the three `World::new` sites), `Forged::{uplift_fit, peak_target_m}` and the field holding the planet.
- `ARTIFACT_VERSION` 11, `TILED_FORMAT_VERSION` 4 (`PlanetRecord`, `province`); `WORLD_MAP_VERSION` 3, `META_FORMAT` 2.
- The knob table as Phase 2 leaves it ("Land": `land_fraction`, `coast_wander_deg`, `coast_detail_deg`, `ocean_margin_start`; `uplift` deleted, so `forge_proto`'s `--uplift` flag is gone), the presets as it leaves them ("Dry" and "Wet" still override `rain_max`, `rain_min`, `land_depletion_deg`, `lake_basins`, `river_min_discharge_m2`), its report lines (`planet.*`, the belt lines, the hypsometry and river lines its Task 9 adds — this plan's before / after tables use them), `just forge-planet`.
- Its Ruling 3: peak targets per size (about 200–850 m on Dev and Tiny, 2,800 m on Large).

## What was read, and what is relied on second-hand

- **Read first-hand:** Peel, Finlayson & McMahon 2007 (the PDF, text extracted from the fetched file): Table 1 with its footnote, the C / D boundary (coldest month > 0 °C, after Russell 1931, not −3 °C), "the B climates must be identified first", the Cs-and-Cw (Ds-and-Dw) tie rule, and the global shares (B 30.2 %, D 24.6 %, A 19.0 %, C 13.4 %, E 12.8 %; BWh 14.2 %, Aw 11.5 %; Csc never, Cwc 0.002 %). Every project document and code file named in this plan's brief.
- **Second-hand, through the research note (which read them):** the insolation formulas (Wikipedia); climlab's energy-balance defaults (`A` 210 W/m², `B` 2 W/m²/°C, `D` 0.555 W/m²/°C, albedo `0.3 + 0.078·P2(sin φ)`, ice albedo 0.62 below −10 °C; its notebook's "about 10 m of water matches the zonal-mean cycle"); Thornthwaite's formula (Wikipedia); Smith & Barstad's transfer function (the authors' extended abstract); Worldbuilding Pasta's pressure, wind, current and rain rules; the observed seasonal lags. A fetch of North & Coakley 1979, a textbook energy-balance treatment, Thornthwaite and Smith & Barstad was attempted for this plan and its results did not come back: **nothing here rests on a new first-hand reading of those four.**
- **Recalled, never read (each is marked where it is used):** North, Mengel & Short 1983's land / ocean / sea-ice heat capacities; Willmott's high-temperature Thornthwaite branch (NOT used: Ruling 9); Earth's mean land precipitation (700–800 mm/yr); the −1.8 °C freezing point of sea water; degree-day melt factors (3–5 mm per °C per day).
- **The researcher's untested designs this plan builds on** (the report's ledger: "the twelve-month diagnostic climate as a whole — inferred, unmeasured"; "ocean-current rules and magnitudes — inferred from a hobbyist rule set"): the pressure construction, the wind turn, the current rule, the moisture transport and its four rain terms, the snow / bucket bookkeeping. Each is built BESIDE the old model first (Tasks 3–8), judged on synthetic worlds (Task 9), and only then switched in (Task 10).

## Global Constraints

- Do not create git worktrees; work on `develop`. No stash, reset, amend, checkout of files, clean, push. The tree holds the owner's unrelated uncommitted changes and other sessions stage in the same index: each commit carries only the task's own hunks, staged through the index as `common.md` describes. The committed-tree compile check is the CONTROLLER's.
- **Tests.** The repository commits only regression tests of bugs actually seen and the existing gates. Every "temporary test" below is written first, seen failing, quoted in the implementer's report (name, command, failing then passing output) and DELETED before the commit; a commit adds no `#[test]`. An "evidence command" is a tool run whose output is quoted in the report. No new gate.
- **The order inside every Verify step is fixed:** (1) the temporary tests pass; (2) DELETE them; (3) `just smoke` (if its audit fails on a file that is NOT the task's own, run `just test-audit crates/worldgen` and smoke's remaining steps by hand, and say so); (4) `just test-one worldgen "--no-run --tests --examples"` when an example, a test file or a public `worldgen` symbol changed (smoke compiles neither examples nor integration tests); (5) `just web-check` when `worldgen` library code changed. `just determinism-wasm` is NOT part of this phase: `game` is not touched — a task that finds it must touch `game` reports BLOCKED first.
- **Two regimes.**
  - *Tasks 1–9 change no world.* The model is built beside the old one and reached only through `forge_proto --climate`. Safety net, run by each: (1) the gate passes UNTOUCHED (`just test-one worldgen "--test it -- --include-ignored forge_lib::"`); (2) `just forge-report --size dev --cell 32 --seeds 8` and `just forge-report --size dev --cell 64 --seeds 8` reproduce every line of `baseline-32.txt` / `baseline-64.txt` (saved in the ledger directory by Task 1 Step 0 BEFORE any change), unchanged and in order — lines are only APPENDED (`diff baseline-N.txt new-N.txt | grep '^<'` prints nothing); in particular `height_crc` and `artifact_rasters_crc` per seed. A changed line or a failing gate is BLOCKED, never a re-record. **Exemption list:** none in Tasks 1 and 3–9; Task 2 APPENDS the `koppen.*` lines (then saves `baseline2-*.txt`, which Tasks 3–9 are judged against).
  - *Tasks 10–14 change worlds on purpose* (Task 11 changes none and proves it by Task 10's `height_crc`s). Evidence is the task's before / after report table. On every run these must still hold: `polar_rows_all_sea: yes`, `drains_to_sea: 1.000`, `aliased_regions_identical: yes`, `world_map_wraps: yes`, the seam rule (`seam_pass_*: yes` for height, precip, biome, monthly_temp, monthly_precip and, from Task 10, monthly_wind — per field at most `seeds / 4` seeds `no` AND median rank ≥ 0.25), and Phase 1b's `climate_roundtrip_ok`, `climate_tiles_identical`, `monthly_aliased_identical: yes`, `monthly_precip_conserved_max_rel ≤ 1e-5`, `climate_clamped_values: 0`. One that fails is BLOCKED.
- **The quality gate** `forge_quality_matches_baseline` (`crates/worldgen/tests/forge_lib.rs`, fixture `tests/fixtures/forge_quality_planet_dev_32m.ron`) is re-recorded exactly ONCE, in Task 15, in a commit of its own stating old → new numbers (climate moves rivers). From Task 10's commit until then it FAILS if run (it is `#[ignore]`d as slow: `just smoke` and `just test` pass, `just test-full` shows it failing): Tasks 10–14 run it WITHOUT bless and quote its message, and take their before / after NUMBERS from the report's own lines (land share, the hypsometry and river lines of Phase 2). **The gate guards land share, hypsometry and rivers only**: every climate property of this phase rests on a `--report` or `--climate` line named in its task.
- **Stored formats: this phase bumps NO version constant** (Ruling 10). `ARTIFACT_VERSION` 11, `TILED_FORMAT_VERSION` 4, `WORLD_MAP_VERSION` 3, `META_FORMAT` 2 stay. A task that finds it must change a stored or wire byte layout reports BLOCKED. A world forged before Task 10 still LOADS (its climate is the placeholder's, self-consistent); it is re-forged to get the model.
- There is never a commit with no working, loadable world.
- **The model grid is fixed** (`MODEL_NX × MODEL_NZ` = 256 × 128) and the planetary stage reads ONLY the land fraction per model cell and `ClimateKey`: never a height, `cell_m`, `width_m`, `height_m` or the seed. Every planetary length is in degrees of latitude; true-scale lengths (`orographic_scale_m`, `OROGRAPHIC_RUN_M`, `TERRAIN_BLUR_M`) stay in metres.
- **Roll-exactness** (the half-width shift: "the climate of a world rolled by half its width equals the rolled climate, exactly"). It holds by construction only if EVERY planetary step obeys five rules; a step that cannot is BLOCKED, not approximated:
  1. resampling between grids uses INTEGER phases (`grid::area_mean`, `grid::resample`): no position is computed as a float and then floored;
  2. any sum or mean over x (a zonal mean, the global mean) goes through `grid::exact_sum` (fixed-point integers): a float sum's rounding depends on where the row starts;
  3. smoothing along x is a direct window sum in a fixed order RELATIVE to the cell — never a running sum started at column 0;
  4. iterative solves are Jacobi or red–black (both colours' updates are independent of column order; a half-width roll is an even number of cells on both grids) with FIXED iteration counts or a stop decided by a max-norm; never a cyclic tridiagonal solve, Gauss–Seidel in index order or a dot-product method;
  5. nothing is seeded, nothing depends on the thread count (per-row or per-month `par::map_range` only).
  The fine stage keeps today's wrapping sweep; its laps run until the entering moisture repeats bit for bit (Ruling 6).
- **The forge may use std float math** (its output is a stored artifact). **Runtime sampling stays exact arithmetic**: this phase adds NOTHING to `forged.rs`'s runtime path or to `game`.
- `game`, `server`, `worldgen` stay Bevy-free. Never write a region edge as a literal.
- **Knobs.** A knob enters `SPECS` in the task that gives it a reader in the forge (Task 10), never earlier; until then the model reads `ClimateKey::DEFAULT` and the tool's `--key name=value`. No knob may be left with no reader at the end of a task, except the ten old climate knobs across Tasks 10 → 11. Structural constants are named `const`s in their module, each with a doc line giving its source or the words "untested design".
- **Tool facts (Phase 1a's fix wave):** `--set <knob>=<value>` REFUSES a value outside the knob's range at parse time; the report summary's seam verdict allows `seeds / 4` seeds `no` for `seeds ≥ 4` with a median rank ≥ 0.25.
- Forge cost is measured, not capped. The design budget is in Ruling 1; `climate.planetary_ms` and `climate.fine_ms` report it.
- Any new or changed command updates its `justfile` recipe (or its comment) in the same task; `CLAUDE.md` (Worldgen paragraph) and `TODO.md` change in the commit that changes the fact.
- Never launched by agents: the game window, paid generation, anything on orion or the twin, the dev Postgres world, benchmarks other than `just forge-bench`.
- Rulings made by a task are written in its report under "Rulings" as `Ruling: … — why — cost if wrong`.

## Shared vocabulary (decided here, used by every task)

- **Three grids.** *Fine grid*: the forge grid of a level (`World { nx, nz, dx }`). *Climate grid*: Phase 1b's container grid (`climate_stride`; `cnx × cnz`). *Model grid*: `MODEL_NX = 256`, `MODEL_NZ = 128`, `MODEL_DEG = 180.0 / 128.0 = 1.40625` degrees per cell both ways (a 2 × 2 block of Phase 2's planet cells); the *EBM grid* is `EBM_NX = 128`, `EBM_NZ = 64` (2.8125°, a 2 × 2 block of model cells). All are row-major, x west → east and wrapping, z north → south and not wrapping; cell `(cx, cz)`'s centre is at latitude `90 − (cz + 0.5) · 180 / nz` (north positive) — `latitude_at`'s line.
- **The map is flat.** One degree of longitude is one degree of latitude everywhere (360° across the width, 180° down the height); meridians do not converge. The Laplacian is `∂²/∂λ² + ∂²/∂φ²` in radians; areas are cell counts. The SUN is not flat: insolation and day length are the sphere's, by latitude. Two means exist and are never mixed: the **planet mean** `⟨·⟩` weights each row by `cos φ` (only the 15 °C solve uses it — Ruling 2); every validation SHARE is unweighted (Decision 12).
- **Months.** Phase 1b's: `m` in `0 .. 12`, month `m` is calendar month `m + 1`; the year starts at the northern spring equinox; the northern summer solstice is at 3.0 months; "July" = month 3, "January" = month 9. The orbital angle from the March equinox at model time `t` months is `θ = 2π · t / 12`. **A model month is a twelfth of a YEAR**: precipitation "per month" and every per-year threshold from the literature (Köppen's, Thornthwaite's) apply unscaled. Where a literature formula counts days it uses `EARTH_DAYS_PER_MONTH = 365.25 / 12` (Thornthwaite's `N`, degree days, melt); only the wet-day fraction counts the calendar's 28 game days (`DAYS_PER_MONTH`). The model year lasts `YEAR_S = 365.25 · 86_400` seconds for the heat capacities' lag.
- **Half-years for Köppen.** Peel's AMJJAS is months `0 ..= 5` (equinox to equinox, the northern summer half), ONDJFM is months `6 ..= 11`. A place's "summer" is whichever half has the higher mean temperature (ties: `0 ..= 5`).
- **Planes** on the model grid are month-major like the container's: `i = m · MODEL_NX · MODEL_NZ + cz · MODEL_NX + cx`.
- **Units and signs.** Temperature °C at SEA LEVEL (the container's datum); precipitation mm per month; pressure hPa as an ANOMALY from the map mean (positive = high); wind m/s, `east` toward +x, `south` toward +z (Phase 1b's planes: a trade wind in the northern tropics has `east < 0`, `south > 0`). "Wind FROM the west" has `east > 0`. `η` (eta) is the belt coordinate of Ruling 3 (0 at the thermal equator, 1 at the pole of that side). Land fraction `ℓ` in 0 ..= 1 per model cell.
- **`ClimateKey`** — everything the planetary stage reads besides the land: `{ global_mean_temp_c, rainfall, inland_reach_deg, ocean_currents }` (`ClimateKey::DEFAULT = { 15.0, 1.0, 18.0, 1.0 }`).
- **Knobs after this phase** ("Climate" group; Task 10 adds, Task 11 deletes):

  | Knob | Default [range] | Reader |
  |---|---|---|
  | `global_mean_temp_c` (new) | 15.0 [5, 25] | the outgoing-radiation solve (`ebm`) |
  | `rainfall` (new) | 1.0 [0.4, 2.5] | the precipitation unit (`moisture`) |
  | `inland_reach_deg` (new) | 18.0 [6, 60] | the rain-out length over land (`moisture`) |
  | `ocean_currents` (new) | 1.0 [0, 1.5] | scales every current anomaly (`ocean`); 0 = none |
  | `lapse_c_per_km` (kept) | 6.5 [3, 10] | the annual raster and the runtime's read |
  | `climate_strength` (kept, help reworded: "0 = no rain shadows or windward rain") | 1.0 [0, 1] | `fine::rain_factors` |
  | `orographic_scale_m` (kept) | 650 [100, 3000] | `fine::rain_factors` |
  | DELETED: `equator_temp_c`, `pole_temp_c`, `meander_amplitude_deg`, `meander_wavelength_deg`, `trade_wind_edge_deg`, `polar_edge_deg`, `land_depletion_deg`, `sea_recharge_deg`, `rain_min`, `rain_max` | | Decision 4 (the latitude temperatures, meander, wind band edges, relaxation lengths) and the two bounds "relative to the latitude table", which goes with them |

  Presets: "Dry" = `rainfall` 0.6, `inland_reach_deg` 10, plus its `lake_basins` / `river_min_discharge_m2` overrides as they stand; "Wet" = `rainfall` 1.5, `inland_reach_deg` 36, plus its two.
- **Report lines** of this phase are appended at the END of `report::report`'s rows, in task order; planetary lines start `climate.`, Köppen lines `koppen.`. The tool mode `--climate` prints its own lines (prefix `model.`; synthetic worlds `synth.<name>.`) and is not part of the baseline diff.

## Rulings (made while writing this plan; each `what — why — cost if wrong`, with the alternative rejected)

1. **The planetary stage runs on a fixed 256 × 128 model grid (the energy balance on 128 × 64), ONCE per forge, and reads no height; relief acts only through the fine stage and the lapse.** The program says "on the planet grid"; Phase 2 fixed that grid at 512 × 256 and left the choice open ("Phase 3 … may adopt `PLANET_NX × PLANET_NZ`"). Half of it is taken because (i) the container it fills is 1.4°–1.9° per cell on Dev, Tiny and Small anyway (Phase 1b Ruling 1), so a 0.7° model would be averaged away on three of five sizes; (ii) the energy balance is an elliptic problem — with climlab's `D` an explicit step on 0.7° cells is limited to about 9 minutes of model time — and its cost grows with the square of the cell count per axis; (iii) a FIXED grid gives the same planetary climate at every world size and preview cell, as Phase 2's planet does. It runs once because it is a function of the land mask alone and `World::ocean` is fixed by `World::new` for the whole forge (until Phase 4): erosion changes heights, and heights reach the climate only through the fine stage, which IS re-run at every `RAIN_REFRESH_ITERS` as today. **Time stepping:** 48 implicit steps a year, years repeated until the monthly means repeat within 0.02 °C (Ruling 2). **Budget:** energy balance 8,192 cells × 48 steps × 16 red–black sweeps × about 28 years (two passes) = 1.8 × 10⁸ point updates; moisture 12 months × 128 iterations × 32,768 cells = 5 × 10⁷ bilinear updates; the rest is under 10⁷. Designed to **1.5 s on the dev box in release, the same at every size**: about +100 % on a Dev / 32 m forge (near 1.4 s after Phase 2), +15 % on Small / 32 m, +4 % on Medium / 32 m, under 1 % on Medium / 16 m; one-entry memo so a preview that changes an erosion knob does not pay it again. The fine stage is designed to at most 3 × today's `orographic` per call (four sweeps and ten blur passes against two and four). Measured (`climate.planetary_ms`, `climate.fine_ms`), not capped — rejected: (a) the full model at every erosion level (four to seventeen runs: seconds on Dev for a result that cannot differ, since it reads no height); (b) the model on the climate grid (a different grid per size: the cross-size evidence of Phase 1a Task 18 would measure the grid, not the climate); (c) 512 × 256 with a multigrid solver (four times the linear work, a solver that is a task of its own, for detail three sizes cannot store) — cost if wrong: coasts and narrow seas under 1.4° (392 m on Dev … 784 m on Large) are blurred in temperature and flat rain; then `MODEL_NX/NZ` double (constants; the red–black rule still holds) and the budget quadruples. The planetary stage is blind to plateaus (no snow albedo over high ground, no blocking of the flow): Known limits.
2. **Temperature is a one-layer seasonal energy-balance model on a flat metric; the mean it is solved for is the cos-weighted planet mean.** On the EBM grid: `C(x,T) · ∂T/∂t = S(φ,t) · (1 − α(φ,T)) − (A + B·T) + D · ∇²T − K · (u, v) · ∇T`.
   - `S`: daily-mean top-of-atmosphere insolation `S0/π · (h0 · sin φ · sin δ + cos φ · cos δ · sin h0)`, `cos h0 = −tan φ · tan δ` clamped to −1 ..= 1 (polar night and day), `sin δ = sin ε · sin θ`; `SOLAR_CONSTANT_W_M2 = 1361.0`, `OBLIQUITY_DEG = 23.44`, a CIRCULAR orbit (the two hemispheres are mirror images half a year apart: what the all-ocean world checks) — read by the researcher; eccentricity rejected (1.7 % of flux, an asymmetry nobody would see, one more dial).
   - `α = lerp(ALBEDO_A0 + ALBEDO_A2 · P2(sin φ), ALBEDO_ICE, ice(T))`, `ALBEDO_A0 = 0.30`, `ALBEDO_A2 = 0.078`, `ALBEDO_ICE = 0.62` (climlab, second-hand), `ice(T) = 1 − smoothstep(ICE_FULL_C = −10.0, ICE_NONE_C = 0.0, T)` — a RAMP where climlab steps at −10 °C (the note's proposal: a step makes the solve jump); `T` of the previous step.
   - `C = ℓ · C_LAND + (1 − ℓ) · lerp(C_OCEAN, C_SEA_ICE, ice(T))`, `C = 4.2e6 J/m³/K × depth`: `LAND_HEAT_DEPTH_M = 2.0`, `OCEAN_MIXED_LAYER_M = 50.0`, `SEA_ICE_HEAT_DEPTH_M = 6.0`. Sources: the researcher's slab derivation (1–2 m and 40–70 m reproduce the observed one-month and two-to-three-month lags), ExoPlaSim's 50 m default; the sea-ice value is RECALLED from North, Mengel & Short 1983 (untested here). **Continentality and lag come out of these three numbers**; Phase 1b's amplitude, lag and `CONTINENTALITY_HALF_DEG` are deleted.
   - `OLR_B_W_M2_K = 2.0`; `A` starts at 210 and is SOLVED: after each model year `A += EBM_A_GAIN (0.5) · B · (⟨T̄⟩ − key.global_mean_temp_c)`, `⟨T̄⟩` the cos-weighted annual mean through `exact_sum`. One scalar, as the program asks; it also keeps a world with much high-latitude land out of a snowball. The target is a sea-level mean (Earth's 15 °C is a surface mean, about 1 °C lower: ignored).
   - `EBM_DIFFUSION_W_M2_K = 0.555` per rad² (climlab's value on a sphere; on the flat metric it is a starting value — the equator-to-pole contrast is its tuning target, Task 3).
   - Advection: first-order upwind, implicit, `K · |wind| / Δ` with `EBM_ADVECTION_W_M2_K_PER_MS = 0.15` per radian (untested design: at 8 m/s it equals diffusion over about 25°, which makes west coasts maritime and east coasts continental without carrying sea air across a continent).
   - Solve: backward Euler, `EBM_STEPS_PER_MONTH = 4`; each step `EBM_SOR_SWEEPS = 16` red–black sweeps at `EBM_SOR_OMEGA = 1.7`, warm-started; sources at the step's mid-time. Years repeat until `max |T_month(year) − T_month(year − 1)| < EBM_CONVERGED_C = 0.02` AND `|⟨T̄⟩ − target| < 0.02`, at most `EBM_YEARS_MAX = 40` (the count is reported; hitting the cap is a finding). Monthly output = the mean of the month's four steps, resampled to the model grid.
   - **Two passes** (`CLIMATE_PASSES = 2`): pass 0 with no wind → pressure and wind → pass 1 with advection, warm-started → the final pressure and wind from pass 1's temperature. No loop to convergence (a fixed count is exact under the roll and bounded in cost).
   - The lapse is not in the model: the container stays at sea level and `lapse_c_per_km` stays a knob with the readers it has. **Diurnal range** (per climate cell and month; untested design — the research consulted no source on it): `ℓ_c · (DIURNAL_LAND_C 8.0 + DIURNAL_DRY_C 8.0 · dry_m) + (1 − ℓ_c) · DIURNAL_SEA_C 3.0`, `dry_m = clamp(1 − P_m / PET_m, 0, 1)` (PET by Thornthwaite from the sea-level month; `dry_m = 0` when `PET_m = 0`), capped at 25.5. Aridity and land share only: continentality enters through `ℓ_c`, elevation not at all (the family is a sea-level datum).
   — rejected: a two-layer air / surface model (twice the state for an exchange coefficient nobody sourced); the spherical operator in φ with a flat x (zonal diffusion divides by cos² φ at the poles); explicit stepping (55,000 steps a year); ADI (a cyclic tridiagonal solve breaks the roll rule) — cost if wrong: the flat metric gives the polar rows as much weight as the equator, so `D` and the ice ramp must carry the profile; if no `D` gives both a 25–29 °C equator and −30…−15 °C polar rows on the all-ocean world, Task 3 rules a latitude-dependent `D` (one more constant).
3. **Pressure is built per month from a thermal equator and a thermal anomaly; wind is the down-gradient direction turned by a rule; nothing is seeded.** All of it is the researcher's synthesis of Pasta's hand rules (untested design):
   - `Ts` = the month's sea-level temperature smoothed by a direct box of radius `CIRC_SMOOTH_DEG = 8` (x wraps, z clamps).
   - **Thermal equator** `φ_I(x, m)`: per column the warmth-weighted latitude `Σ φ · w / Σ w` over `|φ| ≤ ITCZ_SEARCH_DEG = 35`, `w = max(Ts − (column max − ITCZ_WARM_BAND_C 2.0), 0)`; clamped to `± lerp(ITCZ_MAX_OCEAN_DEG 8, ITCZ_MAX_LAND_DEG 22, land share of the column within |φ| ≤ 25°)`; then a circular window mean of radius `ITCZ_ZONAL_SMOOTH_DEG = 20` along x. A centroid, not an argmax: the tropical profile is flat-topped.
   - **Belt coordinate** `η = (φ − φ_I) / (90 − φ_I)` north of `φ_I`, `(φ_I − φ) / (90 + φ_I)` south of it. **Zonal pressure** `p_z = −P_BELT_HPA (8.0) · cos(3π · η)`: low at the thermal equator, high at η = 1/3, low at 2/3, high at the pole. With `φ_I` 8° into the summer hemisphere the highs sit at 35° there and 25° in the winter hemisphere — Pasta's seasonal rule falls out of one formula.
   - **Thermal anomaly** `p_t = clamp(−P_THERMAL_HPA_PER_C (0.8) · (Ts − zonal mean of Ts), ± P_THERMAL_MAX_HPA 12)`, smoothed over `THERMAL_SMOOTH_DEG = 10`: winter highs over cold land, summer lows over hot land (the monsoon low). `p = p_z + p_t`, minus its map mean.
   - **Wind** = `g = −∇p` (hPa per degree, central differences, x wraps) rotated by `θ(φ) = sign(φ) · (90° − FRICTION_TURN_DEG 25) · min(1, |φ| / CORIOLIS_RAMP_DEG 12)` — clockwise on the map (north up) in the north: `(east, south) = (g_e · cos θ − g_s · sin θ, g_e · sin θ + g_s · cos θ)`; the sign is the GEOGRAPHIC hemisphere's and the turn fades to straight down-gradient flow at the equator. Speed `= WIND_MS_PER_HPA_DEG (10.0) · |g|`, capped at `WIND_MAX_MS = 20`. Check by hand: high at 30° N, low at the equator → `g` points south → wind toward the south-west: the north-east trades.
   - This replaces `westerly_weight`, `wind`, `Wind`, `BAND_BLEND_DEG`, the two band-edge knobs and the meander (bands now wander because continents heat and cool, not by a noise). **Stored** as Phase 1b's two `i8` planes (4 per m/s, 12 months): the month's mean vector, bilinear from the model grid to the climate grid.
   — rejected: a shallow-water step (the note: fragile on a flat compressed map); latitude-fitted wind curves (no land–sea effect: no monsoon) — cost if wrong: belts are rigid functions of one coordinate, so storm tracks do not tilt and there is no jet; calm cells give a direction of no meaning (the moisture step floors the speed, `WIND_CALM_MS`).
4. **Ocean currents are a RULE on basin sides, applied as a wind-carried temperature anomaly AFTER the energy balance; the stream-function solve is not built.** The design's open question ("by rule or by a stream-function solve") is closed for the rule: a stream function needs a β per degree on a planet that has none, a sparse solve that breaks the roll rule unless it too is red–black, and has no more evidence than the rule (both are the researcher's proposals; the rule at least mechanises a published hand method).
   - **Coasts that count**: the land fraction smoothed by a direct box of radius `CURRENT_LAND_SMOOTH_DEG = 4`; a model cell is "coast land" when that is ≥ 0.5 (islands a few degrees across steer no current). Per sea cell, along its row: `west_gap_deg` / `east_gap_deg` = degrees to the nearest coast land to the west / east (integer cell counts; a row with no coast land has neither: a circumpolar belt gets no anomaly).
   - **Target anomaly** (°C, constant through the year): `key.ocean_currents · [ sub(|φ|) · (CURRENT_WARM_SUB_C 3 · f(west_gap) − CURRENT_COLD_SUB_C 5 · f(east_gap)) + high(|φ|) · (CURRENT_WARM_HIGH_C 8 · f(east_gap) − CURRENT_COLD_HIGH_C 6 · f(west_gap)) ]`, `f(d) = exp(−d / CURRENT_REACH_DEG 12)`, `sub = smoothstep(8, 15) · (1 − smoothstep(35, 45))`, `high = smoothstep(40, 50) · (1 − smoothstep(70, 80))`. A sea cell with land close to its WEST is on the western side of its basin: warm in the subtropics (a poleward boundary current), cold at high latitude; with land close to its EAST it is on the eastern side: cold in the subtropics (an equatorward current), warm at high latitude. Plus **upwelling** on subtropical eastern sides: `− UPWELLING_C (3) · sub · f(east_gap · 3) · clamp(equatorward wind / 8 m/s, 0, 1)`. Magnitudes are scaled down from Pasta's AIR-temperature rules of thumb (+15 / −10 °C at 50–70°, "minor" near 20°): untested.
   - **Carried onshore**: per month the anomaly field `a` is the target over sea and over land obeys `d a / d s = −a / CURRENT_INLAND_REACH_DEG (10)` along the wind (the moisture step's Jacobi upstream iteration, `CURRENT_CARRY_ITERS = 48`). `t_sea[m] += a`. Onshore wind brings it inland (a mild west coast at 50°); offshore wind leaves the land alone.
   - It is added after the energy balance (not as a heat flux inside it) so that `ocean_currents = 0` removes it exactly and the solve for `A` does not fight it; the moisture stage reads the temperature WITH the anomaly (a cold coast evaporates less and is capped: Ruling 5).
   - **Its own tuning task (12) with before / after evidence, and a fallback:** the phase's evidence is run a second time with `--set ocean_currents=0`; the Köppen main-class ranges must hold there too (the west-coast check need not).
   — cost if wrong: gyres do not close, there is no circumpolar current, a strait or an inland sea is treated as an ocean basin; a world whose model is wrong here ships with `ocean_currents` lowered and the miss recorded.
5. **Moisture is a steady transport along the wind's DIRECTION with lengths in degrees; precipitation is what it rains out, times four rules; the planetary stage rains on flat ground.** Per month on the model grid, `W` relative to saturation at 15 °C (untested design; the structure is the note's recommendation):
   - `W_sat = exp(CC_PER_C (0.067) · (T − 15))`; sea-ice share `ice = 1 − smoothstep(−4, SEA_FREEZE_C −1.8, T)`.
   - One Jacobi iteration: `W'` = `W` read bilinearly one model cell upwind (`−d`, `d = wind / max(|wind|, WIND_CALM_MS 1.0)`), then `W ← (W' + Δ · s · W_sat / SEA_RECHARGE_DEG) / (1 + Δ · [s / SEA_RECHARGE_DEG + M · (1 − ℓ · rec) / key.inland_reach_deg])`, `Δ = MODEL_DEG`, `s = (1 − ℓ)(1 − ice)`, `SEA_RECHARGE_DEG = 9.0` (today's value), then mixed with its four neighbours by `MOIST_MIX = 0.1`. `MOIST_ITERS = 128` from `W = 0.5 · W_sat` (information crosses 180°). Speed does not enter: a length in degrees must not depend on a rule's metres per second.
   - **Land recycling** `rec = RECYCLE_MAX (0.6) · smoothstep(0, 20 °C, T)`: warm land gives rain back, frozen land none. This and the summer thermal low are the two things the research names as missing in the model that lost Asia's interior to cold desert.
   - **Rain multiplier** `M = max(M_MIN 0.05, (1 + ITCZ_RAIN 3.0 · exp(−((φ − φ_I) / ITCZ_HALF_WIDTH_DEG 7)²) + FRONTAL_RAIN 1.2 · front · (1 + FRONTAL_WINTER_BOOST 0.5 · winter) + CONVERGENCE_RAIN 1.5 · clamp(−p_t / P_BELT_HPA, 0, 1)) · (1 − SUBSIDENCE_DRY 0.85 · clamp(p / P_BELT_HPA, 0, 1)) · (1 − COLD_CURRENT_DRY 0.5 · clamp(−a / 5, 0, 1)))` — convergence at the thermal equator; the thermal low's convergence (the monsoon); frontal rain between the subtropical high and the subpolar low (`front = max(0, sin(3π · (η − 1/3)))` for η in 1/3 ..= 2/3, `winter` = how far the month is below the cell's annual mean, 0 ..= 1); suppression under ANY high (`p` is the total anomaly, so a summer land low cancels the belt's high: east coasts stay wet; the polar high and winter continental highs dry); a cap over cold currents.
   - **Flat precipitation** `P0_m = RAIN_UNIT_MM (60.0) · key.rainfall · W · M · (INLAND_REACH_REF_DEG 18 / key.inland_reach_deg)` mm per month. `RAIN_UNIT_MM` is tuned (Task 6) so the all-ocean equatorial belt reaches 200–300 mm in its wettest month and land averages 550–950 mm a year on forged worlds (Earth's 700–800 is recalled).
   - **Why a Mediterranean west coast should appear** (the phase's check, argued before it is measured): at 38° in summer `η` is 0.37 — under the high, suppressed, and the eastern-side cold current caps what is left; in winter `η` is 0.47 — the high has moved equatorward and `front` is near 1.
   - **Wet-day fraction** (per climate cell and month; the runtime note's inference, untested): `clamp(P_m / (28 · amount), 0, WET_DAY_MAX 0.9)`, `amount = lerp(WET_DAY_MM_COOL 5, WET_DAY_MM_HOT 14, clamp(T_m / 25, 0, 1))` — Phase 1b's rule kept (its constants lose the word "placeholder"). **Snow fraction** is a derived field (Ruling 9), not part of the transport.
   — rejected: Smith & Barstad's linear model (real metres and seconds and one wind per FFT window; the note itself prefers the upslope term on a compressed planet); a pure wind march with no subsidence term (both seasonal hobby generators needed "encourage Mediterranean / rainforest / savanna" patches) — cost if wrong: regimes that need a jet or a storm track (double rainy seasons, east-coast cyclones) are absent; the multiplier's five constants are the tuning surface of Tasks 9 and 13.
6. **The fine stage is today's sweep, four ways: wind from W, E, N and S, blended by the model's wind; the monthly values on the climate grid come from block means of the four factor planes.** `fine::rain_factors` runs today's `orographic` sweep along rows (from W, from E; wrapping) and along columns (from N, from S; no wrap, starting at the polar sea) with ONE change of meaning: the air's moisture relaxes toward 1.0 over land AND sea (over `LEE_RECOVERY_DEG = 16.0` on land — today's `land_depletion_deg` default — and `SEA_RECOVERY_DEG = 9.0` over sea and lakes): the coast-to-interior drying is now the planetary stage's, so flat ground gets exactly factor 1. The rise still wrings (`rise / orographic_scale_m`) and still rains on the windward side (`OROGRAPHIC_RUN_M`); each factor is clamped to `ORO_FACTOR_MIN 0.25 ..= ORO_FACTOR_MAX 4.0`, blended with 1 by `climate_strength`, and blurred over land with ONE radius `RAIN_BLUR_DEG = 1.5` both ways (`box_blur_rows` as it is; "cross" and "along" mean nothing with four directions). A wrapping sweep laps until the moisture entering the lap repeats BIT FOR BIT (at most `SWEEP_LAPS_MAX = 6`; it contracts by e⁻²² per lap on the smallest world), then records one lap: the result does not depend on the start column.
   - **Direction weights** per model cell and month from the wind: `w_W = max(east, 0) / (|east| + |south|)`, `w_E = max(−east, 0) / …`, `w_N = max(south, 0) / …`, `w_S = max(−south, 0) / …` (calm: a quarter each). A diagonal wind is a mix of two axis sweeps — a wall is felt by the share of the wind that crosses it.
   - **Annual fine precipitation** `A(x) = P0_year(x) · Σ_dir D_dir(x) · r_dir(x)`, `D_dir = Σ_m P0_m · w_dir,m / Σ_m P0_m` on the model grid (the precipitation-weighted annual share of each direction), `P0_year` and `D_dir` resampled to the fine grid. Because the blend is linear this IS the sum of the twelve fine months `P0_m · Σ_dir w_dir,m · r_dir` — no twelve fine planes are ever built. Erosion's refresh computes exactly this.
   - **The container's precipitation** for climate cell `c`, month `m`: `P0_m(c) · Σ_dir w_dir,m(c) · r̄_dir(c)`, `r̄_dir` the block mean of the factor plane over the cell's fine cells.
   - **Phase 1b's Ruling 8 stands unchanged** ("fine precipitation = fine annual raster × coarse monthly share"): the fine annual raster is now the sum of the fine-resolved months, as that ruling's own cost line foresaw, and the share is the climate cell's. What is lost: inside one climate cell the windward and lee slopes share one seasonal split.
   — rejected: tracing the wind upstream from every fine cell (cells × path × months: 10¹⁰ at Medium / 16 m, per refresh); a fast-sweeping solve (its result depends on the sweep's start: the roll would hold only to a tolerance) — cost if wrong: a range oblique to the wind casts a blurred shadow; the transient cost is four `f32` planes per fine cell (16 B: +9 % of the forge's 172 B per cell — measured in Task 14).
7. **Lengths: everything planetary is in degrees; the SAME planetary climate at every size is true by construction, and what differs by size is said, not hidden.** The model grid, every constant of Rulings 2–5 and the two recovery lengths are degrees or cells of a fixed grid. Three true-scale lengths stay in metres (`orographic_scale_m`, `OROGRAPHIC_RUN_M`, `TERRAIN_BLUR_M`): a hill is a hill. So on Dev and Tiny, whose mountains are 200–850 m high (Phase 2 Ruling 3), a rise wrings out a third to all of the air instead of all of it several times over, and peaks are 1–5 °C colder than the plain instead of 18 °C: rain shadows are weak and there is no alpine `ET` / `EF` — the polar class on small worlds comes from latitude alone. That is the pocket planet's price, not a defect to tune out: `orographic_scale_m` is NOT scaled with the world (a 300 m hill casting a desert would be the opposite lie). **Evidence** (Task 14): the L1 distance between the five Köppen main-class shares of the same seed at two sizes — target ≤ 0.25 for Dev ↔ Small, Dev ↔ Medium, Small ↔ Medium (Phase 1a Task 18's biome-share L1 was 0.189 / 0.232 / 0.104) — printed beside the biome-share L1 — cost if wrong: the cross-size distance is dominated by the fine stage and the target is missed; the ruling then records the measured distance per class.
8. **The switch is one task (10) preceded by nine that change no world and followed by one that only deletes; the gate is re-recorded once, at the end.** Order: the scaffold and the synthetic worlds (1); Köppen in the tool, on TODAY's climate, as the "before" (2); the model stage by stage, each judged on synthetic worlds through `forge_proto --climate` (3–8); a capped tuning task on the synthetic worlds (9); the switch — `forge_with`, erosion's rain, the container, the annual rasters, the new knobs, the report lines (10); the deletion of the annual model, the placeholder and the ten old knobs (11); two capped tuning tasks on forged worlds (12 currents, 13 Köppen and the checks); the maps, the roll and the cross-size evidence (14); the close (15). Every commit forges a loadable world: before Task 10 the old model still feeds everything; from Task 10 the new one does. **The window:** the ten old knobs have no reader for exactly one commit (10 → 11); the gate fails from Task 10 to Task 15 (Global Constraints). **Existing consumers keep working because the fine annual rasters keep their meaning** — `temperature_c` = annual mean at the ground, `precip_mm` = annual total: erosion's `discharge_weight`, `regions::derive` → `game::classify` (today's classifier, until Phase 5), `tile_map::climate`, `ForgedWorld::{snow_line_m, column_climate, cell_climate}`, `habitat`, `quality`'s snow peaks, `render`'s two layers. Task 10's evidence includes the biome-share table so "worlds stay plausible between phases" is a number — cost if wrong: today's classifier, built for the latitude table (a 180 mm desert belt, `FROZEN_C` −5), reads the model's fields differently and a biome vanishes; Task 13 may then move `RAIN_UNIT_MM` or `global_mean_temp_c`, never `game::biome`'s constants (Phase 5 replaces them).
9. **Derived fields are forge-side FUNCTIONS of the stored normals; none is stored.** `climate::derived::derive(n: &game::MonthlyNormals, lat_deg: f64, sea: bool) -> Derived` (the normals at the ground from `ground_normals`, or at sea level for a sea cell):
   - `pet_mm[12]` — Thornthwaite 1948 (the note's reading): `16 · (L/12) · (N/30) · (10·T/I)^a`, `T` the month's mean floored at 0 and CAPPED at `THORNTHWAITE_MAX_C = 26.5`, `I = Σ (T_i/5)^1.514`, `a = 6.75e-7·I³ − 7.71e-5·I² + 1.792e-2·I + 0.49239`, `N = EARTH_DAYS_PER_MONTH`, `L = 24 · h0/π` hours from the insolation's own `h0` at mid-month (`insolation::day_length_h(lat_deg, month)`); 0 when `I = 0`. Willmott's branch above 26.5 °C is recalled, not read: not used.
   - `gdd0`, `gdd5` = `Σ EARTH_DAYS_PER_MONTH · max(T_m − base, 0)` (Phase 5's plant limits are in Earth degree days).
   - Snow: snowfall share `1 − smoothstep(SNOW_ALL_C −1.0, SNOW_NONE_C 3.0, T)` of the month's precipitation; melt `MELT_MM_PER_C_DAY (4.0) · max(T, 0) · EARTH_DAYS_PER_MONTH`; the snow water recurrence run for `BUCKET_LAPS = 3` years, capped at `SWE_CAP_MM = 5000`. `swe_mm[12]`, `snow_months` (mean of a month's start and end ≥ `SNOW_COVER_MM = 10`), `permanent_snow` (never 0 in the last lap), `mass_balance_mm` (annual snowfall − annual potential melt: Phase 4's ice reads it). Constants: the note's "unverified background".
   - `sea_ice_months` (sea cells): months below `SEA_FREEZE_C = −1.8` (recalled).
   - The bucket: `S ← clamp(S + rain + melt − AET, 0, SOIL_BUCKET_MM 150)`, `AET = PET · min(1, S / (0.75 · 150))`, the excess is `runoff_mm[12]`; three laps (Thornthwaite–Mather as the note proposes it: unverified). `aet_mm[12]`, `alpha = Σ AET / Σ PET`.
   - `aridity = MAP / Σ PET` (capped at 100 when PET is 0), `dry_months` (months under `DRY_MONTH_MM = 60`: the biome note's recalled band), and through the normals' own methods the warmest and coldest month.
   **Why nothing is stored:** Phases 4 and 5 run in the forge (std float math, the float `MonthlyFields` and `Planetary` still in memory on `Forged`); Phase 6 reads temperature, precipitation, wet-day fraction and wind, which ARE stored, and may replay snow from them (add, multiply, compare: exact arithmetic) or store a curve in its own bump. Thornthwaite needs a power and an arc cosine: it cannot be a runtime read, and nothing at runtime asks for it — cost if wrong: Phase 5 calls `derive` per fine cell (2.4 × 10⁸ powers at Medium / 16 m: seconds); Phase 6 wants a stored snow or evapotranspiration normal and bumps the format then.
10. **No version constant is bumped.** The container's layout, encodings and grid rule are Phase 1b's; this phase changes VALUES. `knobs_from_map` ignores a stored knob this build does not know and defaults one the file lacks, so a world forged before Task 10 loads and reads its own (placeholder) climate; nothing reads a stored field with a new meaning. The one allowed bump per constant is left unused — rejected: bumping to force a re-forge (the program batches format bumps; a version that changes no byte makes `worldctl retile` refuse archives it could read) — cost if wrong: a stale world is played after the switch and its climate is the placeholder's; `TODO.md` and the owner hand checks say "re-forge".
11. **Köppen lives in the LIBRARY's forge side (`worldgen::forge::koppen`), by Peel's Table 1 exactly; the runtime never calls it.** The brief asks for it "in the tool, not in the runtime"; two tools need it (`forge_proto --report`, `world_map --layer koppen`) and two example binaries share no module, and Phase 5 computes Köppen "as the label" in the forge. Rules, in this order: `B` first (`MAP < 10 · Pth`; `Pth = 2·MAT` if ≥ 70 % of MAP falls in the winter half, `2·MAT + 28` if ≥ 70 % in the summer half, else `2·MAT + 14`; `W` when `MAP < 5 · Pth`, else `S`; `h` when `MAT ≥ 18`, else `k`); else `E` when `Thot < 10` (`T` when `Thot > 0`, else `F`); else `A` when `Tcold ≥ 18` (`f`: `Pdry ≥ 60`; else `m`: `Pdry ≥ 100 − MAP/25`; else `w`); else `C` when `Tcold > 0`, else `D`. Second letter of C and D: `s` when `Psdry < 40` and `Psdry < Pwwet / 3`; `w` when `Pwdry < Pswet / 10`; BOTH → `w` if the summer half's total exceeds the winter half's, else `s` (Peel's added rule); neither → `f`. Third: `a` when `Thot ≥ 22`; else `b` when at least 4 months exceed 10 °C; else `d` (D only) when `Tcold < −38`; else `c`. Peel writes `Thot > 10` for C and D and `< 10` for E; exactly 10.0 is given to C / D here. Shares are of fine LAND cells (`!World::ocean`; lake cells count), UNWEIGHTED, from `ground_normals` of the stored container — the numbers the runtime and Phase 5 read — cost if wrong: none (a move of one file).
12. **The design's "real-Earth topography regression" is not built.** The design's Phases table lists it as part of Phase 3's gate; the program's Phase 3 section (newer, and it wins) does not. It needs Earth's topography and a reference Köppen raster fetched and resampled onto a flat rectangle, and it would test the model on a land distribution with polar land this world can never have. The three synthetic worlds and the Köppen ranges carry the phase; the design's row is amended in Task 15 — cost if wrong: the model is tuned with no real reference; a later task could add `--climate --world earth:<file>` behind the same tool mode.
13. **Tuning is planned work with caps.** Three tuning tasks (9: the synthetic worlds; 12: currents; 13: the Köppen ranges, the west-coast check and the interiors), each at most FOUR rounds (a round = change dials, run the task's evidence commands, look at the named sheets, record the table). After the fourth round every remaining miss is written as a ruling with its measured number; no fifth round, no new mechanism inside a tuning task (a mechanism is BLOCKED and goes to the controller).

## Review Focus

1. **A world that changes before Task 10.** `forge_with`, `Forged::artifact` and `report::report` must not call the model in Tasks 1–9. The proof is the two baseline diffs, `height_crc` and `artifact_rasters_crc` — not a reading of the diff.
2. **Month and hemisphere phase.** `θ` at mid-step, the solstice at 3.0, the half-year split `0 ..= 5` / `6 ..= 11`, the sign of `θ(φ)` in the wind turn, `east` / `south` signs in the stored planes. Every one of these produces plausible maps when wrong; the all-ocean numbers (mirror symmetry half a year apart, trades with `east < 0` in both hemispheres, the thermal equator north in months 2–7) are the check.
3. **The two means.** The 15 °C solve is cos-weighted; every share is unweighted. A cos-weighted Köppen share, or an unweighted global mean (it would put the equator near 35 °C), passes every type check.
4. **Datum.** The model, `Planetary::t_sea_c` and the container are at SEA LEVEL; `Climate::temperature_c` and Köppen's inputs are at the GROUND. Köppen on sea-level normals turns every mountain tropical.
5. **Roll-exactness leaks.** A float `sum()` over a row, a running-sum blur, a position `(cx as f64 + 0.5) / n * m as f64` floored, an SOR loop in index order: each breaks the roll by one unit in the last place and then by a quantisation step somewhere. `climate.roll_identical` (Task 10) is the check; it must be `yes`, not "close".
6. **Conservation between the three precipitation numbers.** The fine annual raster, the sum of the container's twelve months over a climate cell, and the model's `P0` must agree where the ground is flat (`climate.flat_precip_gap`), and the erosion-time annual must be the same function as the final one.
7. **Costs that hide.** The planetary stage is fixed-cost (dominant on Dev: the 8-seed report and the gate test each pay it per forge); the fine stage is called about seventeen times per forge and is three times today's; `Forged` now holds `MonthlyFields` (288 B per climate cell) and `Planetary` (about 5 MB).

---

### Task 1: The scaffold — module split, the model grid, insolation, the synthetic worlds, the tool mode

**Files:**
- Move: `crates/worldgen/src/forge/climate.rs` → `crates/worldgen/src/forge/climate/mod.rs` (`git mv`; content unchanged)
- Create: `crates/worldgen/src/forge/climate/{grid.rs, insolation.rs, synthetic.rs}`; `crates/worldgen/examples/forge_proto/climate_tool.rs`
- Modify: `crates/worldgen/examples/forge_proto/main.rs` (`--climate`, `--world synthetic:<name>`, `--key <name>=<value>`), `justfile` (new recipe `forge-climate *args` = `forge_proto --climate {{args}}`, with a comment listing its flags and lines), `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` and the design (Phase 3: the model grid is 256 × 128, half the planet grid — Ruling 1; amended in this commit, as the program requires)

**Interfaces — produces:**

```rust
// grid.rs
pub const MODEL_NX: usize = 256;
pub const MODEL_NZ: usize = 128;
pub const MODEL_DEG: f64 = 180.0 / 128.0;
pub const EBM_NX: usize = 128;
pub const EBM_NZ: usize = 64;
pub const MONTHS: usize = game::CLIMATE_MONTHS;
/// Latitude (deg, north positive) of row `cz` of a grid with `nz` rows.
pub fn row_lat_deg(cz: usize, nz: usize) -> f64;
/// Index of (cx, cz): x by rem_euclid(nx), z clamped.
pub fn at(nx: usize, nz: usize, cx: i64, cz: i64) -> usize;
/// Area mean of `src` (snx × snz) on a (dnx × dnz) grid spanning the same map. Overlaps are INTEGERS
/// (along x a source cell is dnx units wide, a destination cell snx units), so a roll of `src` by snx / 2
/// columns rolls the result by dnx / 2 exactly. Works both ways (finer → coarser and coarser → finer).
pub fn area_mean(src: &[f64], snx: usize, snz: usize, dnx: usize, dnz: usize) -> Vec<f64>;
/// Bilinear read of `src` at every cell centre of a (dnx × dnz) grid; the phase of a centre is the integer
/// pair ((2·cx + 1)·snx − dnx, 2·dnx): floor and remainder in integers; x wraps, z clamps.
pub fn resample(src: &[f64], snx: usize, snz: usize, dnx: usize, dnz: usize) -> Vec<f64>;
/// Σ of round(v · 2^32) in i128, back to f64: the same for any order of the terms.
pub fn exact_sum(values: impl Iterator<Item = f64>) -> f64;
/// Mean of the (2r + 1)-wide window along x (wrapping) or z (clamped), summed in the order −r ..= r.
pub fn window_mean(src: &[f64], nx: usize, nz: usize, radius_cells: usize, along_x: bool) -> Vec<f64>;
/// Land fraction per model cell from a fine world's ocean mask (`area_mean` of 0 / 1).
pub fn model_land(w: &World) -> Vec<f32>;

// insolation.rs
pub const SOLAR_CONSTANT_W_M2: f64 = 1361.0;
pub const OBLIQUITY_DEG: f64 = 23.44;
pub const YEAR_S: f64 = 365.25 * 86_400.0;
pub const EARTH_DAYS_PER_MONTH: f64 = 365.25 / 12.0;
/// Declination (rad) at model time `t_months` (0 = the northern spring equinox).
pub fn declination(t_months: f64) -> f64;
/// Daily-mean top-of-atmosphere flux (W/m²) at latitude `lat_deg`, time `t_months`. Circular orbit.
pub fn daily_mean_w_m2(lat_deg: f64, t_months: f64) -> f64;
/// Hours of daylight at mid-month `month`.
pub fn day_length_h(lat_deg: f64, month: usize) -> f64;

// synthetic.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Synthetic { Ocean, Continent, Wall }
impl Synthetic { pub fn parse(name: &str) -> Option<Synthetic>; pub fn name(self) -> &'static str; }
/// A hand-built world on a 256 × 128 fine grid of 392 m cells (Medium's 100,352 × 50,176 m: a fine cell IS
/// a model cell, and a degree is Medium's 278.8 m), `wrap_x`, polar rows sea, and the params to run it with.
pub fn world(kind: Synthetic) -> (World, ForgeParams);
```

- `Synthetic::Ocean`: every cell sea (`h` −100). `Continent`: land from 78° N to 78° S, 90° of longitude wide (64 columns, centred on column 128), flat at 200 m. `Wall`: land from 60° N to 60° S, 120° wide, flat at 200 m, with one meridional ridge through its middle — crest 1,500 m, 6° wide (a triangular profile across 4 cells), from 55° N to 55° S. The `World` literal names EVERY field of the struct as it stands at dispatch (no `..`), `uplift` / `uplifted` zero, `outlet` true on sea cells, `edges` `[Sea, Wrap, Sea, Wrap]`.
- **The tool mode** `forge_proto --climate [--world synthetic:<ocean|continent|wall>] [--size … --cell … --seed … --seeds N] [--key name=value]… [--out <stem>]`: builds the synthetic world or forges one (the OLD pipeline, untouched), then runs whatever stages of the new model exist and prints `model.*` lines; with `--out` it writes PNG sheets. In this task it prints `model.grid: 256 × 128 (1.40625°)`, `model.land_share`, `model.insolation_w_m2.<lat>` for `90N, 60N, 30N, 0, 30S, 60S, 90S` at months 0, 3, 6, 9 and the annual mean, `model.day_length_h.<lat>` at months 3 and 9; the sheet `<stem>-land.png` (256 × 128, land fraction). Later tasks append lines and sheets; `climate_tool.rs`'s header comment lists them all.

- [ ] **Step 0: The baseline, BEFORE any change** — `just forge-report --size dev --cell 32 --seeds 8 > <ledger dir>/baseline-32.txt`, the same at `--cell 64` → `baseline-64.txt`; the gate (`just test-one worldgen "--test it -- --include-ignored forge_lib::"`): quote its pass.
- [ ] **Step 1: Temporary tests** (`#[cfg(test)] mod tmp` in `grid.rs` / `insolation.rs`; `just test-one worldgen "--lib forge::climate"`):
  - `area_mean` of a 512 × 256 field onto 256 × 128 equals the 2 × 2 block mean; of an 800 × 400 field (Tiny at 32 m) it conserves the field's total within 1e-9 relative; of a 128 × 64 field onto 256 × 128 every destination cell equals its source cell; rolled input (by `snx / 2`) gives the rolled output BIT FOR BIT for 512, 800 and 1,568 columns;
  - `resample` at 256 → 256 is the identity; 256 → 392 columns (Medium's climate grid) of a field linear in z reproduces it within 1e-12 away from the polar rows; rolled input gives rolled output bit for bit (392 and 200 columns);
  - `exact_sum` of a row equals `exact_sum` of the row rotated by 37; `window_mean` of a rolled row is the rolled result bit for bit;
  - `daily_mean_w_m2`: at the equator at an equinox `S0/π` = 433.2 ± 0.5; at 90° N at the northern solstice `S0 · sin ε` = 541.3 ± 0.5; at 80° S at the northern solstice 0; `daily_mean_w_m2(φ, t) == daily_mean_w_m2(−φ, t + 6)` within 1e-9 for 20 pairs; the annual mean at the equator 410–425 and at the pole 170–180; `day_length_h(0, m) = 12 ± 0.01` for every month, `day_length_h(60, 3) > 18`, `day_length_h(60, 9) < 6`;
  - `synthetic::world`: `nx == 256`, `nz == 128`, rows 0 and 127 all sea for the three kinds; `Continent`'s land share is `64 · round(156 / 1.40625) / (256 · 128)` ± one row; the wall's crest is 1,500 m at column 128 and the plain 200 m ten columns away; `model_land(&w)` equals the mask cell for cell.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-climate --world synthetic:continent --out target/world-forge/p3/t1` : quote every line; READ `t1-land.png`. `just forge-climate --size dev --cell 32`: `model.land_share` equals the report's `land_share` within 0.002. The safety net (gate + both diffs).
- [ ] **Step 4: Verify, in order** — temporary tests pass; DELETE them; `just smoke`; `just test-one worldgen "--no-run --tests --examples"`; `just web-check`.
- [ ] **Step 5: Commit** `feat(forge): the climate model's grid, insolation and synthetic worlds (no world changes)`.

### Task 2: Köppen by Peel's rules, and the "before" numbers

**Files:**
- Create: `crates/worldgen/src/forge/koppen.rs`; Modify: `crates/worldgen/src/forge/mod.rs` (`pub mod koppen;`), `crates/worldgen/examples/forge_proto/report.rs` (appended lines), `justfile` (the `forge-report` comment)

**Interfaces — produces:**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Koppen { Af, Am, Aw, BWh, BWk, BSh, BSk, Csa, Csb, Csc, Cwa, Cwb, Cwc, Cfa, Cfb, Cfc,
                  Dsa, Dsb, Dsc, Dsd, Dwa, Dwb, Dwc, Dwd, Dfa, Dfb, Dfc, Dfd, ET, EF }
impl Koppen {
    pub const ALL: [Koppen; 30];
    pub fn name(self) -> &'static str;
    pub fn main(self) -> char;            // 'A' | 'B' | 'C' | 'D' | 'E'
    pub fn rgb(self) -> [u8; 3];          // the usual map colours (Task 14 draws them)
}
/// Peel, Finlayson & McMahon 2007, Table 1 (Ruling 11). `n` is at the GROUND.
pub fn classify(n: &game::MonthlyNormals) -> Koppen;
/// Shares of `artifact`'s fine land cells per type, `Koppen::ALL` order (sum 1; all 0 with no land).
pub fn land_shares(a: &crate::forged::WorldArtifact, ocean: &[bool]) -> [f64; 30];
```

- `land_shares` reads, per fine land cell `i`, `forged::ground_normals(a.climate.normals_at(cx_m, cz_m), f64::from(a.height.get(i)).max(0.0), f64::from(a.precip_mm.get(i)), lapse_c_per_m)` with the cell centre in map metres and the lapse from `forged::knobs_from_map(&a.knobs)` — Phase 1b's "for Phase 5" expression; rows in parallel (`par::map_range`), counts added as integers.
- `--report` appends: `koppen.A`, `koppen.B`, `koppen.C`, `koppen.D`, `koppen.E` (`{:.4}`), then one line per type `koppen.<name>` in `ALL` order (`{:.4}`; thirty lines, so the summary gives each its min / median / max).

- [ ] **Step 1: Temporary tests** (`mod tmp` in `koppen.rs`), each a hand-built twelve-month pair:
  - Peel's Herberton (southern: warm half = months 6–11): MAT 19.9, coldest month 15.5, MAP 1,146 with over 70 % in the warm half, driest warm-half month 25.3, wettest cool-half month 86.0, driest cool-half month 16.5, wettest warm-half month 238.4 → not B (threshold 678), `Cw?` by the tie rule — assert the first two letters `Cw`;
  - Peel's Batopilas: driest summer month 5.3, wettest winter month 49.3, driest winter month 18.4, wettest summer month 154.6, summer total 441 > winter 191, hot summer → `Csa` (only `s` is satisfied: the tie rule does not apply);
  - one case per first letter and per boundary: `Tcold` 18.0 → A; `Thot` 10.0 → not E; `Thot` 9.99 → `ET`; `Thot` 0.0 → `EF`; `Tcold` 0.0 → D, 0.01 → C; MAP exactly `10 · Pth` → not B; `Pdry` 60 → `Af`; the three `Pth` variants (a place with 70 % of its rain in winter at MAT 10 is B below 200 mm, in summer below 480 mm); `Tcold` −38.5 with 3 months over 10 °C → `Dfd`-family; 4 months over 10 °C and `Thot` 21.9 → `b`;
  - the half-year test: the same twelve values shifted by six months give the same class;
  - `land_shares` of an all-ocean artifact is all zeros (no NaN).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-report --size dev --cell 32 --seeds 8` and `--cell 64 --seeds 8`: quote the five main-class lines per seed and the summary — this is **today's climate under Köppen**, the phase's "before" (expected far from the ranges: the placeholder's rain has one seasonal shape; say what it shows). The safety net: gate + both diffs against `baseline-*` (no `<` line); then save the two runs as `baseline2-32.txt` / `baseline2-64.txt`.
- [ ] **Step 4: Verify, in order** — temporary tests pass; DELETE them; `just smoke`; `just test-one worldgen "--no-run --tests --examples"`; `just web-check`.
- [ ] **Step 5: Commit** `feat(forge): Köppen–Geiger classes by Peel 2007 in the forge report`.

### Task 3: The energy balance — temperature from heat capacities

**Files:** Create `crates/worldgen/src/forge/climate/ebm.rs`; Modify `climate/mod.rs` (`pub mod`), `examples/forge_proto/climate_tool.rs` (lines and sheets).

**Interfaces — produces** (every constant of Ruling 2 as a `pub const` with its source line):

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClimateKey { pub global_mean_temp_c: f64, pub rainfall: f64, pub inland_reach_deg: f64, pub ocean_currents: f64 }
impl ClimateKey { pub const DEFAULT: ClimateKey; pub fn set(&mut self, name: &str, v: f64) -> bool; }

pub struct Ebm {
    /// 12 × EBM_NX × EBM_NZ, month-major: monthly mean sea-level temperature (°C).
    pub t_c: Vec<f64>,
    pub state: Vec<f64>,        // the last step's field (the next pass's warm start)
    pub olr_a: f64,             // the solved A (W/m²)
    pub years: u32,             // model years run in this pass
    pub planet_mean_c: f64,     // cos-weighted annual mean
}
/// One pass. `land`: EBM_NX × EBM_NZ fractions. `wind`: None, or (east, south) m/s, 12 planes each on the
/// EBM grid. `start`: None (15 °C everywhere, A = 210) or a previous pass's (state, olr_a).
pub fn solve(land: &[f64], wind: Option<(&[f64], &[f64])>, start: Option<(&[f64], f64)>, key: &ClimateKey) -> Ebm;
/// cos-weighted mean over rows of the per-row `exact_sum` mean.
pub fn planet_mean(field: &[f64], nx: usize, nz: usize) -> f64;
```

- Implementation notes that are not free choices: the red–black update of cell `i` solves its own row of `(C/Δt + B + 4D/Δ² + adv) · T_i − (D/Δ²) · ΣT_nb − adv terms = C/Δt · T_i^old + S·(1 − α) − A` with `Δ = EBM cell in radians`, poles Neumann (the missing neighbour is the cell itself), x by `grid::at`; `C` and `α` from the PREVIOUS step's field; rows of one colour may run in parallel (`par::map_range` over rows, collected, then written) — never in place across threads.
- Tool lines (`--climate`): `model.ebm.years`, `model.ebm.olr_a`, `model.ebm.planet_mean_c`, `model.ebm.ms`; per band `75N, 45N, 15N, 0, 15S, 45S, 75S`: `model.t_annual_c.<band>`, `model.t_range_c.<band>` (warmest − coldest month of the band mean), `model.t_warmest_month.<band>`; `model.t_polar_row_c` (annual mean of rows 0 and 63, both), `model.t_zonal_asym_c` (largest `|T − zonal mean|` over all cells and months). Sheets: `<stem>-temp-<MM>.png` for months 03 and 09 (`tile_map::temperature_color`), `<stem>-temp-range.png`.

- [ ] **Step 1: Temporary tests** (`mod tmp` in `ebm.rs`; the all-ocean and continent masks built directly on the EBM grid):
  - a single cell with no neighbours' influence (D = 0 via a test-only argument or a 1 × 1 grid) forced by `F·cos ωt`: amplitude within 5 % of `F / √(B² + (Cω)²)` and lag within 0.15 month of `atan(Cω/B)` for the land and the ocean capacity (the note's slab formula: about 1.3 months and 2.9 months);
  - all ocean: `planet_mean_c` = 15.0 ± 0.02; `t_zonal_asym_c` == 0.0 exactly; `T(φ, m)` equals `T(−φ, m + 6)` within 0.05 °C; `years < EBM_YEARS_MAX`;
  - the continent mask: the annual range at the land's centre at 50° N is at least 2.5 × the ocean's at the same latitude on the far side, and its warmest month is earlier;
  - a land mask rolled by 64 columns gives `t_c` rolled by 64, BIT FOR BIT (`==` on the vectors; print one line on a mismatch).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence and first tuning** — `just forge-climate --world synthetic:ocean --out target/world-forge/p3/t3-ocean` and `…:continent …`: quote every `model.ebm.*` / `model.t_*` line; READ the two temperature sheets of each. **Targets** (all-ocean): `t_annual_c.0` 25–29; `t_polar_row_c` −30…−15; `t_range_c.45N` 4–10 and its warmest month 4 or 5 (a lag of 1.5–3 months); `t_range_c.0` < 2; `years ≤ 25`; `model.ebm.ms ≤ 400` for the pass. (Continent): `t_range_c` of the interior at 50° ≥ 25 °C with the warmest month 3 or 4; the polar row colder than the all-ocean one. Dials, in this order, at most 3 rounds here (the capped tuning is Task 9): `EBM_DIFFUSION_W_M2_K` (the equator-to-pole contrast), `SEA_ICE_HEAT_DEPTH_M` and the ice ramp (the polar winter), `LAND_HEAT_DEPTH_M` (the interior's range and lag), `OCEAN_MIXED_LAYER_M`. A target still missed is written in the report with its number (Task 9 inherits it). The safety net (gate + both diffs against `baseline2-*`).
- [ ] **Step 4: Verify, in order** — temporary tests pass; DELETE them; `just smoke`; `just test-one worldgen "--no-run --tests --examples"`; `just web-check`.
- [ ] **Step 5: Commit** `feat(forge): a seasonal energy-balance temperature on the model grid (beside the old climate)`.

### Task 4: Pressure and wind; advection in the second pass

**Files:** Create `climate/circulation.rs`; Modify `climate/ebm.rs` (the advection term, if Task 3 left it a stub), `climate/mod.rs`, `climate_tool.rs`.

**Interfaces — produces** (constants of Ruling 3 as `pub const`s, each "untested design" unless sourced):

```rust
pub struct Circulation {
    pub itcz_lat_deg: Vec<f64>,   // 12 × MODEL_NX
    pub eta: Vec<f64>,            // 12 × model cells: the belt coordinate
    pub p_zonal_hpa: Vec<f64>,    // 12 × model cells
    pub p_thermal_hpa: Vec<f64>,
    pub pressure_hpa: Vec<f64>,   // their sum minus its map mean
    pub wind_east_ms: Vec<f64>,
    pub wind_south_ms: Vec<f64>,
}
/// `t_sea_c`: 12 × model cells; `land`: model cells.
pub fn diagnose(t_sea_c: &[f64], land: &[f64]) -> Circulation;
```

- Tool: the driver now runs pass 0 → `diagnose` → pass 1 (wind resampled to the EBM grid by `area_mean`) → `diagnose`. Lines: `model.itcz_lat_deg.m03` / `.m09` (min, mean, max over x), `model.wind_east_ms.<band>.m03` / `.m09` for the seven bands, `model.wind_speed_max_ms`, `model.high_lat_deg.north.m03` / `.m09` and `.south.…` (the latitude of the zonal-mean pressure maximum between 15° and 50°), `model.circ.ms`; (continent) `model.winter_west_minus_east_c.50N` (coldest-month sea-level temperature 4° inland of the west coast minus 4° inland of the east coast at 50° N) for pass 0 and pass 1, `model.land_low_hpa.m03` (the lowest `p_thermal` over the continent north of 15° N in month 3) and `model.land_high_hpa.m09`. Sheets per month 03 and 09: `<stem>-pressure-<MM>.png` (blue low, red high), `<stem>-wind-<MM>.png` (speed as brightness, an arrow every 8 cells, the thermal equator drawn as a line).

- [ ] **Step 1: Temporary tests:** on a hand-made temperature field warmest at 10° N in every column, `itcz_lat_deg` is 10 ± 1.5 over ocean-clamped columns clamped to 8; a pressure field high at 30° N and low at the equator gives wind with `east < 0` and `south > 0` at 15° N (the north-east trades), and its mirror gives `east < 0`, `south < 0` at 15° S; high at 30° N and low at 60° N gives `east > 0` at 45° N; at the equator the wind is parallel to `−∇p` within 1°; speed never exceeds `WIND_MAX_MS`; a 6 °C warm land anomaly gives `p_thermal` of −4.8 hPa before smoothing; a rolled temperature field gives rolled planes bit for bit.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the two synthetic commands of Task 3 with new stems: quote the new lines, READ the pressure and wind sheets of both months. **Targets** (all-ocean): `wind_east_ms.15N` and `.15S` negative and `.45N`, `.45S` positive in both months; `itcz_lat_deg.m03` mean +2…+8, `.m09` mean −8…−2; `high_lat_deg.north.m03` 31–39 and `.m09` 21–29 (the summer / winter positions); `wind_speed_max_ms ≤ 15`. (Continent): `land_low_hpa.m03 ≤ −3` and `land_high_hpa.m09 ≥ +3`; `winter_west_minus_east_c.50N` larger in pass 1 than in pass 0 by at least 2 °C (advection makes the west coast maritime); `itcz_lat_deg.m03` max over the continent ≥ 12. The safety net.
- [ ] **Step 4: Verify, in order** — as Task 3.
- [ ] **Step 5: Commit** `feat(forge): monthly pressure and wind from the thermal equator and land–sea contrast`.

### Task 5: Ocean-current anomalies by basin side

**Files:** Create `climate/ocean.rs`; Modify `climate/mod.rs`, `climate_tool.rs`.

**Interfaces — produces** (constants of Ruling 4, each "untested design (scaled from Pasta's air-temperature rules)"):

```rust
pub struct Basins { pub coast_land: Vec<bool>, pub west_gap_deg: Vec<f32>, pub east_gap_deg: Vec<f32> }  // model cells; gap = f32::INFINITY when the row has no coast land, 0 on coast land
pub fn basins(land: &[f64]) -> Basins;
/// The target anomaly (°C) per model cell and month (12 planes; the months differ only by upwelling); 0 on land.
pub fn target_anomaly(b: &Basins, circ: &Circulation, key: &ClimateKey) -> Vec<f64>;
/// The anomaly as the air carries it: the target over sea, decaying inland along the wind (12 planes).
pub fn carried(target: &[f64], land: &[f64], circ: &Circulation) -> Vec<f64>;
```

- Tool: lines `model.current_c.west_side.<lat>` and `.east_side.<lat>` for `20, 30, 55` north and south (the mean target over sea cells within 6° of the coast on that side of the basin), `model.current_carried_c.west_coast.55N` / `.east_coast.55N` (4° inland), `model.current_mean_c` (the planet mean of the carried anomaly: reported, not solved away), `model.ocean.ms`. Sheet `<stem>-current.png`. With `--key ocean_currents=0` every one of these is 0.

- [ ] **Step 1: Temporary tests:** on the continent mask `west_gap_deg` just east of the continent is one cell and grows eastward round the world to the continent's west coast; the all-ocean mask gives all-infinite gaps and a zero target; a 2-cell island does not make `coast_land`; the target is positive at 25° on the western side of the basin and negative on the eastern side, and the reverse at 55°; `carried` equals the target over sea, decays to 1/e within `CURRENT_INLAND_REACH_DEG ± 2°` downwind of an onshore coast and is under 5 % of the coast's value 10° inland of an OFFSHORE coast; `ocean_currents = 0` gives zeros; rolled inputs give rolled outputs bit for bit.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-climate --world synthetic:continent --out target/world-forge/p3/t5` and the same with `--key ocean_currents=0`: quote the lines of both; READ `t5-current.png` and the month-09 temperature sheet of each (the west coast at 55° N visibly milder with currents on). **Targets:** `current_c.east_side.30N ≤ −3`, `current_c.west_side.30N ≥ +1.5`, `current_c.east_side.55N ≥ +4`, `current_c.west_side.55N ≤ −3`; `current_carried_c.west_coast.55N ≥ +2`; `|current_mean_c| ≤ 0.5`. The safety net.
- [ ] **Step 4: Verify, in order** — as Task 3.
- [ ] **Step 5: Commit** `feat(forge): ocean-current temperature anomalies by basin side, carried onshore by the wind`.

### Task 6: Moisture and the flat-ground precipitation; the planetary stage whole

**Files:** Create `climate/moisture.rs`, `climate/planetary.rs`; Modify `climate/mod.rs`, `climate_tool.rs`.

**Interfaces — produces** (constants of Ruling 5):

```rust
// moisture.rs
/// 12 planes of flat-ground precipitation (mm per month) on the model grid, and the moisture field.
pub fn flat_precip(t_sea_c: &[f64], land: &[f64], circ: &Circulation, carried_anomaly: &[f64], key: &ClimateKey)
    -> (Vec<f64>, Vec<f64>);

// planetary.rs
pub struct Planetary {
    pub key: ClimateKey,
    pub land: Vec<f32>,                 // model cells
    pub t_sea_c: Vec<f32>,              // 12 × cells: sea-level temperature WITH the carried anomaly
    pub pressure_hpa: Vec<f32>,
    pub wind_east_ms: Vec<f32>,
    pub wind_south_ms: Vec<f32>,
    pub itcz_lat_deg: Vec<f32>,         // 12 × MODEL_NX
    pub current_c: Vec<f32>,            // 12 × cells: the carried anomaly (Phase 4's cold-coast flag reads it)
    pub precip_flat_mm: Vec<f32>,       // 12 × cells
    pub precip_flat_year_mm: Vec<f32>,  // cells
    pub dir_share: [Vec<f32>; 4],       // cells: the precipitation-weighted annual share of wind FROM W, E, N, S
    pub olr_a: f64, pub ebm_years: u32, pub planet_mean_c: f64,
    pub ms: f64,
}
pub const DIR_FROM: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];   // the direction the air MOVES for W, E, N, S
/// The whole planetary stage (Ruling 1's order): ebm pass 0 → circulation → ebm pass 1 → circulation → currents → moisture.
pub fn planetary(land: &[f32], key: &ClimateKey) -> Planetary;
/// Direction weights of one month at one model cell from its wind (Ruling 6); they sum to 1.
pub fn dir_weights(east_ms: f64, south_ms: f64) -> [f64; 4];
```

- The twelve months of `flat_precip` and of `ocean::carried` are independent: `par::map_range(12, …)`.
- Tool: the driver is now `planetary(…)`. Lines: `model.precip_mm.<band>.m03` / `.m09` and `model.precip_year_mm.<band>` (band means over ALL cells), `model.precip_year_mm.land_mean`, `model.precip_max_month_mm`, `model.moist.ms`, `model.planetary_ms`; a **model-grid Köppen** (the model cell's sea-level normals with `P0` — flat ground, so no lapse — through `koppen::classify`, land cells with `ℓ ≥ 0.5`): `model.koppen.A` … `.E` and, for the continent, the pattern lines of Task 9. Sheets: `<stem>-rain-<MM>.png` (months 03 and 09), `<stem>-rain-year.png`, `<stem>-koppen.png`.

- [ ] **Step 1: Temporary tests:** with a uniform westerly wind and `M = 1` (hand-made `Circulation`), flat precipitation over a continent falls to `1/e` of its west-coast value within `inland_reach_deg / (1 − ℓ·rec)` ± 15 % and is equal at every latitude of equal temperature; doubling `rainfall` doubles every value; raising `inland_reach_deg` from 18 to 36 raises the interior's share of the coast's value; a cell under `p = +P_BELT_HPA` gets at most 0.2 × a cell at `p = 0` with the same moisture; over sea ice the source is 0; `dir_weights(5, 0) == [1, 0, 0, 0]`, `(0, −3) == [0, 0, 0, 1]`, `(0, 0) == [0.25; 4]`, and they sum to 1 for 100 random winds; `dir_share` sums to 1 per cell; `planetary` of a rolled land mask is the rolled result bit for bit in EVERY plane.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the three synthetic worlds (`ocean`, `continent`, `wall` — the wall's planetary stage sees only its land) with `--out target/world-forge/p3/t6-<name>`: quote every new line; READ the rain sheets and `-koppen.png` of the continent. **Targets** (all-ocean): `precip_mm` of the wettest band-month 200–300; `precip_year_mm.0` the largest of the bands (these precipitation lines use nine bands: the seven plus `30N`, `30S`); the subtropical minimum — the lower of the 15° and the 30° band of a hemisphere — ≤ 40 % of the equator's in both hemispheres; `precip_year_mm.45N` and `.45S` ≥ 1.5 × that minimum; `precip_year_mm.75N` ≤ 400; `model.planetary_ms ≤ 1500`. (Continent): `precip_year_mm.land_mean` 400–1,100; the model-grid Köppen has all five main classes present. First values only — the pattern is tuned in Task 9. The safety net.
- [ ] **Step 4: Verify, in order** — as Task 3.
- [ ] **Step 5: Commit** `feat(forge): steady moisture transport and flat-ground precipitation; the planetary stage whole`.

### Task 7: The fine stage — four sweeps, the annual raster, the monthly fields

**Files:** Create `climate/fine.rs`; Modify `climate/mod.rs` (re-exports; `box_blur_rows` and `radius_cells` become `pub(super)`), `climate_tool.rs`.

**Interfaces — consumes:** today's `orographic` (its sweep, `TERRAIN_BLUR_M`, `OROGRAPHIC_RUN_M`, `box_blur_rows`, `lakes_from_filled`), `climate_stride`, `MonthlyFields`, `grid::resample`. **Produces:**

```rust
pub const LEE_RECOVERY_DEG: f64 = 16.0;
pub const SEA_RECOVERY_DEG: f64 = 9.0;
pub const ORO_FACTOR_MIN: f64 = 0.25;
pub const ORO_FACTOR_MAX: f64 = 4.0;
pub const RAIN_BLUR_DEG: f64 = 1.5;
pub const SWEEP_LAPS_MAX: usize = 6;
/// The windward / lee factor of every fine cell for wind FROM W, E, N, S (1.0 on flat ground and at sea).
pub struct DirFactors { pub from: [Vec<f32>; 4] }
pub fn rain_factors(w: &World, p: &ForgeParams, lakes: &[bool]) -> DirFactors;
/// Annual precipitation (mm) per fine cell: Ruling 6's A(x).
pub fn annual_precip(w: &World, pl: &Planetary, f: &DirFactors) -> Vec<f32>;
/// The monthly family on `w`'s climate grid from the planetary stage and the block means of `f` (Rulings 2, 5, 6).
pub fn monthly_fields(w: &World, pl: &Planetary, f: &DirFactors) -> MonthlyFields;
/// The annual fine climate: temperature = the mean of `m`'s twelve sea-level months read bilinearly
/// (`grid::resample` from the climate grid) minus lapse × max(h, 0) (the sea at 0); precipitation as given.
pub fn annual_climate(w: &World, p: &ForgeParams, m: &MonthlyFields, precip_mm: Vec<f32>) -> Climate;
```

- `rain_factors` with `climate_strength ≤ 0` returns four planes of 1.0 without sweeping (today's shortcut). The lengths convert with `metres_per_degree(w.nz as f64 * w.dx)`. Rows (W, E) and columns (N, S) sweep in parallel, each line serial.
- `monthly_fields`: temperature and the two wind components = `grid::resample` of the model planes onto the climate grid; precipitation by Ruling 6; then wet-day fraction (Ruling 5) and diurnal range (Ruling 2) per climate cell — `derived`'s Thornthwaite is Task 8's, so this task puts the two Thornthwaite helpers it needs in `insolation.rs` / a private fn and Task 8 moves them.
- Tool: for a forged world the driver takes `forged.world` and its lakes (`erosion::route(w, 0.0).filled` → `lakes_from_filled`), builds `DirFactors`, the annual raster and the monthly fields, quantises (`quantise(w.wrap_x)`), and classifies every fine land cell with `ground_normals` exactly as `koppen::land_shares` does — but from the NEW fields (a private variant taking the container and the precipitation slice). Lines: `model.fine.koppen.A` … `.E`, `model.fine.precip_land_mean_mm`, `model.fine.factor_min` / `_max` / `_land_mean` per direction, `model.fine_ms`, `model.flat_precip_gap` (on cells whose four factors are within 1 % of 1: the largest relative gap between the fine annual value and the resampled `precip_flat_year_mm` — expected under 1e-4), `model.fine.clamped_values`; (wall) the shadow lines of Task 9. Sheets: `<stem>-fine-rain-year.png`, `<stem>-factor-W.png`, `-E`, `-N`, `-S`.

- [ ] **Step 1: Temporary tests** (`mod tmp` in `fine.rs`, on `synthetic::world(Wall)` and a flat variant): on flat land every factor is exactly 1.0; on the wall the from-W factor is above 1.3 on the west flank's crest-ward half and below 0.8 for at least 4° east of the crest, recovering to within 10 % of 1 by 3 × `LEE_RECOVERY_DEG`; the from-E factor is its mirror; the from-N and from-S factors along the ridge line stay within 5 % of 1 away from its ends; every factor is within `ORO_FACTOR_MIN ..= ORO_FACTOR_MAX`; `climate_strength = 0` gives all ones; a world rolled by `nx / 2` gives factors rolled bit for bit (the lap rule); with a hand-made `Planetary` whose wind is westerly everywhere `annual_precip` equals `precip_flat_year × from-W factor` cell for cell, and the sum of `monthly_fields`' twelve precipitation planes over a climate cell equals the block mean of `annual_precip` within 0.5 % on flat cells; `annual_climate`'s temperature at a 1,000 m cell is the sea-level annual mean minus `lapse` within 1e-3; `monthly_fields(..).quantise(true).validate()` is `Ok` and `clamped_values() == 0`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-climate --world synthetic:wall --out target/world-forge/p3/t7-wall`: quote the lines, READ the four factor sheets and the annual rain sheet. `just forge-climate --size dev --cell 32 --seeds 8` and `--size medium --cell 32 --seeds 2` (the OLD forge's terrain with the NEW climate beside it): quote `model.fine.koppen.*`, `model.fine.precip_land_mean_mm`, `model.fine_ms`, `model.planetary_ms`, and, timed by the tool with `std::time::Instant` around one call of each, `model.old_compute_ms` (today's `climate::compute` on the same world) beside `model.fine_ms` — **the fine stage's budget is 3 × the old call** (Ruling 1); above it, say where the time goes. `model.flat_precip_gap ≤ 1e-4`. The safety net.
- [ ] **Step 4: Verify, in order** — as Task 3.
- [ ] **Step 5: Commit** `feat(forge): windward and lee factors in four directions; the annual and monthly climate from the model (beside the old one)`.

### Task 8: Derived fields

**Files:** Create `climate/derived.rs`; Modify `climate/mod.rs`, `climate/fine.rs` (use `derived`'s Thornthwaite for the diurnal range), `climate_tool.rs`.

**Interfaces — produces** (constants of Ruling 9, each with "read by the researcher" / "recalled" / "unverified background"):

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Derived {
    pub pet_mm: [f32; 12], pub aet_mm: [f32; 12], pub runoff_mm: [f32; 12], pub swe_mm: [f32; 12],
    pub gdd0: f32, pub gdd5: f32, pub pet_year_mm: f32, pub aridity: f32, pub alpha: f32,
    pub mass_balance_mm: f32,
    pub snow_months: u8, pub sea_ice_months: u8, pub dry_months: u8, pub permanent_snow: bool,
}
pub fn thornthwaite_mm(temp_c: &[f32; 12], lat_deg: f64) -> [f32; 12];
pub fn derive(n: &game::MonthlyNormals, lat_deg: f64, sea: bool) -> Derived;
```

- Doc comment of `derive`, for Phases 4 and 5: `n` is at the ground for a land cell (`forged::ground_normals`) and at sea level for a sea cell; the function is forge-side (it uses `powf` and `acos`): never call it from `forged.rs`'s runtime path or from `game`.
- Tool lines (forged worlds; per fine land cell from the new fields): `model.dryland_share` (aridity < 0.65), `model.aridity.p10` / `.p50` / `.p90`, `model.snow_months.mean`, `model.permanent_snow_share`, `model.runoff_ratio` (Σ runoff ÷ Σ precipitation over land), `model.sea_ice_months.<band>` for `75N, 60N, 60S, 75S` (the mean over the band's sea cells), `model.gdd5.<band>`; (synthetic continent) the same. Sheets: `<stem>-aridity.png`, `<stem>-snow-months.png`, `<stem>-runoff-year.png`.

- [ ] **Step 1: Temporary tests:** `thornthwaite_mm` of twelve months at 20 °C at the equator is twelve equal values of 16 · 1.0146 · (200/I)^a with `I = 12 · 4^1.514` (compute `I` and `a` in the test) within 0.5 %; a month at or below 0 °C has PET 0; a month at 35 °C equals the same month at 26.5 °C; an all-frozen year has `pet_year_mm == 0`, `aridity == 100` (the cap) and no NaN anywhere; `gdd5` of twelve months at 15 °C is `10 · 365.25`; a place at −20 °C all year with 20 mm a month has `permanent_snow`, `snow_months == 12`, `mass_balance_mm == 240`; a place at 25 °C has `snow_months == 0`; a month at 1 °C snows half its precipitation; the bucket: with P = 200 mm and PET = 50 mm every month `runoff` settles at 150 a month and `alpha == 1`; with P = 10 and PET = 150, `runoff == 0` and `alpha` ≈ P/PET; a sea cell at −5 °C for four months has `sea_ice_months == 4`; `dry_months` counts months under 60 mm.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-climate --world synthetic:continent --out target/world-forge/p3/t8` and `just forge-climate --size dev --cell 32 --seeds 2`: quote the new lines, READ the aridity and snow sheets. No pass range (Phase 5 judges drylands at 35–55 %): the numbers are recorded as the first look, and a `dryland_share` outside 20–70 % or a `runoff_ratio` outside 0.15–0.6 is NAMED in the report for Task 13. The safety net.
- [ ] **Step 4: Verify, in order** — as Task 3.
- [ ] **Step 5: Commit** `feat(forge): derived climate fields — evapotranspiration, aridity, snow, sea ice, growing degree days, runoff`.

### Task 9: Tune the model on the three synthetic worlds (at most four rounds)

No new mechanism. **Files:** the constants of `climate/{ebm, circulation, ocean, moisture, fine}.rs`; `climate_tool.rs` (the pattern lines below, added FIRST as a no-behaviour change); `justfile` (the `forge-climate` comment names the three evidence commands).

**The evidence commands** (run every round; quoted whole in the last): `just forge-climate --world synthetic:ocean --out target/world-forge/p3/t9-ocean`, `…:continent …`, `…:wall …`.

**What each world must show** (`synth.<name>.*` lines; ✓ / ✗ per line in the round's table):

| World | Line | Target |
|---|---|---|
| ocean | `zonal_asym_c`, `mirror_gap_c` | 0.0 exactly; ≤ 0.05 (hemispheres mirror half a year apart) |
| ocean | `planet_mean_c`; `t_annual_c.0`; `t_polar_row_c` | 15.0 ± 0.1; 25–29; −30…−15 |
| ocean | `t_range_c.45N`, its warmest month; `t_range_c.0` | 4–10 °C, month 4 or 5; < 2 |
| ocean | `wind_east_ms.15` / `.45` (both hemispheres, months 03 and 09) | negative / positive in all eight |
| ocean | `itcz_lat_deg.m03`, `.m09` | +2…+8; −8…−2 — and NO reversal: `wind_east_ms.15N` keeps its sign all year (no monsoon without land) |
| ocean | `precip` belts (Task 6's four targets) | as Task 6 |
| continent | `interior_range_c.50N` ÷ the ocean's at 50° N; its lag | ≥ 2.5; warmest month 3 or 4 |
| continent | `winter_west_minus_east_c.50N` | ≥ 5 °C |
| continent | `koppen.band.<lo>_<hi>.<side>` — the commonest class in a box (`side`: `west` = within 12° of the west coast, `east` likewise, `mid` between), both hemispheres | `0_8.mid`: A; `18_30.west`: B (BWh or BSh); `30_45.west`: **Cs (Csa or Csb)**; `25_40.east`: Cfa or Cwa; `45_60.west`: Cfb or Cfc; `45_65.mid`: D; `70_78.mid`: E |
| continent | `cs_share.30_45.west` and `.east` | west ≥ 0.25 in BOTH hemispheres; east ≤ half the west's |
| continent | `interior.D_share`, `interior.B_share` (land at 45–65°, more than 12° from the sea) | D ≥ 0.5; B ≤ 0.3 — **the known failure: interiors lost to cold desert** |
| continent | `koppen.A` … `.E` | printed; all five present |
| wall | `shadow_ratio.<lat>` = annual precipitation 2–10° WEST of the crest ÷ 2–10° EAST of it, at `15N, 15S, 45N, 45S` | ≤ 0.67 at 15° (trade winds from the east: the west is the lee); ≥ 1.5 at 45° (westerlies: the east is the lee) |
| wall | `flip_lat_deg.m03`, `.m09` (the northern latitude where the monthly ratio crosses 1) | both between 22° and 40°; they differ by ≥ 4° (the belts migrate) |

**Dials and what each moves** (change the fewest that explain a miss):

| Miss | First dial | Then |
|---|---|---|
| equator too hot / poles too warm | `EBM_DIFFUSION_W_M2_K` | the ice ramp's two temperatures |
| interior range too small, lag too long | `LAND_HEAT_DEPTH_M` | `EBM_ADVECTION_W_M2_K_PER_MS` |
| west no milder than east | `EBM_ADVECTION_W_M2_K_PER_MS` | `CURRENT_WARM_HIGH_C`, `CURRENT_INLAND_REACH_DEG` |
| no B on the west at 18–30° | `SUBSIDENCE_DRY`, `CURRENT_COLD_SUB_C` | `COLD_CURRENT_DRY` |
| no Cs on the west at 30–45° | `FRONTAL_RAIN`, `FRONTAL_WINTER_BOOST` | `ITCZ_MAX_LAND_DEG` / `ITCZ_MAX_OCEAN_DEG` (how far the highs migrate), `SUBSIDENCE_DRY` |
| interior B instead of D | `RECYCLE_MAX`, `CONVERGENCE_RAIN` | `key.inland_reach_deg`'s default, `P_THERMAL_HPA_PER_C` |
| no A at the equator / A too wide | `ITCZ_RAIN`, `ITCZ_HALF_WIDTH_DEG` | `RAIN_UNIT_MM` |
| east coast dry at 25–40° | `CONVERGENCE_RAIN` | `CURRENT_WARM_SUB_C` |
| wall shadow too weak / wrong side | check the SIGN first (`DIR_FROM`, the wind turn) — a wrong side is a defect, BLOCKED | `LEE_RECOVERY_DEG` |

- [ ] **Step 0:** add the pattern lines; run the three commands; save as round 0 (`<ledger dir>/t9-round0-*.txt`).
- [ ] **Steps 1–4: up to four rounds.** Each: the table of every line above (value, ✓ / ✗), the dials changed and why, and the sheets READ this round — `-temp-03`, `-temp-09`, `-pressure-03`, `-pressure-09`, `-wind-03`, `-wind-09`, `-rain-03`, `-rain-09`, `-rain-year`, `-koppen`, `-current` of the continent; the four factor sheets of the wall — one line each on what they show. Stop early when every line is ✓.
- [ ] **Step 5:** every line still ✗ after round four is a ruling in the report (`Ruling: <line> is <value>, target <target> — <why no dial reaches it> — <what it costs the forged worlds>`). The safety net (gate + both diffs): constants of a model nothing calls cannot move a world — if a baseline line moves, something calls it: BLOCKED.
- [ ] **Step 6: Verify** — `just smoke`; `just test-one worldgen "--no-run --tests --examples"`; `just web-check`.
- [ ] **Step 7: Commit** `tune(forge): the climate model on the three synthetic worlds`.

### Task 10: The switch — the forge's climate is the model

**At dispatch the controller tells the owner:** from this commit every newly forged world has the model's climate; a world forged earlier still loads (Ruling 10) and should be re-forged (`just run --reset`, `just pg-reset`). The quality gate fails from here until Task 15.

**Files:**
- Modify: `crates/worldgen/src/forge/mod.rs` (`forge_with`, `Forged`, `Forged::artifact`, the module doc's stage list), `forge/climate/planetary.rs` (`planetary_for`), `forge/climate/mod.rs` (`discharge_weight`'s signature), `forge/knobs.rs` (ADD the four new knobs; reword `climate_strength`'s help), `forge/presets.rs` ("Dry", "Wet"), `forge/render.rs` only if a signature it calls changed
- Modify: `crates/worldgen/examples/forge_proto/report.rs` (re-pointed and appended lines), `climate_tool.rs` (for a forged world it now reads `forged.planetary` / `forged.monthly` instead of recomputing; `--key` still overrides for synthetic worlds), `main.rs` (`--no-climate` keeps setting `climate_strength = 0`), `justfile` (comments of `forge-report`, `forge-climate`)
- Modify: `CLAUDE.md` (Worldgen paragraph: the climate is a twelve-month model on a 256 × 128 grid run once per forge, the fine factors per erosion refresh, the annual rasters derived from the monthly family, `just forge-climate`; stage only your hunks), `TODO.md` (re-forge every dev world, the web world, dev Postgres worlds, the twin and orion to get the model climate; the gate is red until the phase closes)

**Interfaces — produces:**

```rust
// planetary.rs
impl ClimateKey { pub fn from_knobs(k: &Knobs) -> ClimateKey; }
/// The planetary stage of `w`'s land with `p`'s knobs; a one-entry process-wide memo on (key, crc32 of the
/// model land's bytes) — the menu preview re-forges on every knob change.
pub fn planetary_for(w: &World, p: &ForgeParams) -> std::sync::Arc<Planetary>;
// mod.rs (climate)
pub fn discharge_weight(precip_mm: &[f32], w: &World) -> Vec<f64>;     // was (&Climate, &World)
// forge/mod.rs
pub struct Forged { …, pub planetary: std::sync::Arc<climate::Planetary>, pub monthly: climate::MonthlyFields,
                    pub climate_ms: [f64; 3] /* planetary, erosion-time refreshes summed, the final pass */ }
```

- `forge_with`: `let pl = climate::planetary_for(&fine, p);` right after the finest `World::new` (the mask every level shares). Every place that today calls `climate::compute(&lw, &lp)` / `compute_with_lakes(…)` for `rain` becomes `let f = fine::rain_factors(&lw, &lp, &lakes); rain = discharge_weight(&fine::annual_precip(&lw, &pl, &f), &lw);` (lakes from `erosion::route(&lw, 0.0).filled` where `compute` found them itself, from `scratch.filled()` at a refresh, as today). The final block: `rain_factors` on the final world → `annual_precip` → `monthly_fields` → `annual_climate` → `Forged { climate, monthly, planetary, rain, … }`. The old `compute` / `compute_with_lakes` are no longer called by the forge (Task 11 deletes them).
- `Forged::artifact`: `climate: self.monthly.quantise(w.wrap_x)` — no model call (it is called per preview, by the seed search and by the Play forge).
- **Report.** Re-pointed, meaning kept: `sea_level_temp_c.north` / `.middle` / `.south` = the mean over the row's climate cells of the ANNUAL mean of `forged.monthly.temp_sea_c` (rows 0, `cnz / 2`, `cnz − 1`); Phase 1b's band and seam lines read `forged.monthly` where they called `monthly_placeholder`. Appended, in this order:
  - `climate.planetary_ms`, `climate.refresh_ms`, `climate.final_ms`, `climate.ebm_years`, `climate.planet_mean_c`, `climate.olr_a`;
  - `sea_level_temp_month_c.north` / `.middle` / `.south`: the twelve monthly row means on one line (`{:.1}` × 12);
  - `climate.precip_land_mean_mm`, `climate.flat_precip_gap`;
  - `seam_step_ratio_monthly_wind`, `seam_step_rank_monthly_wind`, `seam_ok_monthly_wind` (month `NORTH_MIDSUMMER_MONTH`; the step between two climate cells is the length of the difference of their wind vectors; `seam_steps(cnx, cnz, …, p99)`); `run`'s summary loop gains `"monthly_wind"`;
  - `climate.roll_identical: yes/no` — the forged world rolled by `nx / 2` columns (`h`, `ocean`, every per-cell `World` field; lakes rolled), its `planetary_for` (uncached), factors, annual precipitation and monthly fields recomputed, compared `==` with the rolled originals after `quantise` and `QRaster::fit`'s inputs as `f32` bits;
  - `west_coast.cells`, `west_coast.cs_share`, `west_coast.cs: yes/no/n/a`, `east_coast.cs_share` — fine land cells at 30° ≤ |lat| ≤ 45° within `WEST_COAST_DEG = 8` of a coast whose sea lies to the WEST (walk west along the row from the cell's model cell: the first sea model cell must come within 8° and begin a run of sea at least 10° wide; `east_coast` is the mirror); `cs_share` = the share of them classed `Csa` or `Csb`; `cs: yes` when the share is ≥ 0.05, `n/a` under 200 such cells;
  - `interior.cells`, `interior.D_share`, `interior.B_share` — fine land cells at 40° ≤ |lat| ≤ 65° more than `INTERIOR_DEG = 12` from the sea (the model-grid coast distance); the B share counts `BWk` and `BSk`;
  - `dryland_share`, `snow_months.land_mean`, `runoff_ratio` (Task 8's, from the artifact's fields).
  `koppen.*` (Task 2) now reads the model's climate with no code change.

- [ ] **Step 0: Before** — `just forge-report --size dev --cell 32 --seeds 8 > <ledger dir>/t10-before.txt`, `--size medium --cell 32 --seeds 2 > t10-before-medium.txt`; the gate's pass, quoted (the last time it passes until Task 15).
- [ ] **Step 1: Temporary tests** (`crates/worldgen/tests/forge_lib.rs`, one dev world at 64 m forged once): `forged.planetary.land` equals `grid::model_land(&forged.world)`; `forged.artifact(p).climate == forged.monthly.quantise(true)` and validates; the artifact's `precip_mm` raster summed over a climate cell's fine cells is within 1 % of 12-month sum × cells of the container there, on 200 cells; `ForgedWorld::from_bytes(..)`'s `monthly_at_ground(x, z).annual_mean_temp_c()` is within 0.07 °C of `annual_climate(x, z).0` at 1,000 points (the raster's step plus the container's); `discharge_weight` has mean 1 over land; two forges of the same params give `==` artifacts (the memo changes nothing); a forge with `--set ocean_currents=0` differs; `Knobs::default()` round-trips through `knobs_map` / `knobs_from_map` with the four new keys; `presets::matching(&presets::apply(dry)) == Some(dry)` for both rewritten presets.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the two reports again (`t10-after*.txt`). The before / after table, 8 dev seeds and 2 Medium: `koppen.A` … `.E`; `land_share` (must be IDENTICAL: the mask does not depend on climate); `river_mouths`, `lakes`, the hypsometry and river lines of Phase 2 (climate moves rivers: quote how far); the five biome shares that moved most and the count of biomes present (Ruling 8: today's classifier on the model's annual fields — a biome that vanished on every seed is NAMED for Task 13); `climate.precip_land_mean_mm`; `sea_level_temp_c.*` and `sea_level_temp_month_c.*`. **Judged here** (the ranges are Task 13's): `climate.roll_identical: yes` on every seed; `climate.flat_precip_gap ≤ 1e-3`; `sea_level_temp_c.north` and `.south` below −15 and `.middle` above 20 on every seed (Phase 1a's criterion, now from a model); the coldest polar month below −25 and the middle row's monthly range under 3 °C; `monthly_annual_temp_gap_c` p99 ≤ 0.1; every constraint of the second regime (Global Constraints). The gate WITHOUT bless: quote its message. `just forge-proto --size dev --cell 32 --out target/world-forge/p3/t10` then `just world-map --world target/world-forge/p3/t10.world --tiles --out target/world-forge/p3/t10-map`: READ the biome, temperature and rain tile maps and say in one line each what they show. `climate.planetary_ms ≤ 1500` and `climate.refresh_ms + climate.final_ms` ≤ 3 × the old climate's share (Task 7's ratio); above, a finding with numbers (Ruling 1's budget), not a block.
- [ ] **Step 4: Verify, in order** — temporary tests pass; DELETE them; `just smoke`; `just test-one worldgen "--no-run --tests --examples"`; `just web-check`.
- [ ] **Step 5: Commit** `feat(forge)!: the climate is a twelve-month model — temperature, wind and rain by month, the annual rasters derived from it`. The message says worlds should be re-forged and the gate is re-recorded at the phase's end.

### Task 11: Delete the annual model, the placeholder and their knobs

**Files:** `forge/climate/mod.rs`, `forge/knobs.rs`, `forge/presets.rs` (nothing left to change if Task 10 rewrote both presets: verify), `examples/forge_proto/{report.rs, main.rs, serve.rs}` and any other reader the compiler names, `CLAUDE.md` / the design (Decision 4's Phase 3 line as built: which knobs went, which came).

**Deleted** (search each symbol; the compiler finds the rest): `LATITUDE_PRECIP_MM`, `base_precip_mm`, `CONTINENTAL_MOISTURE`, `BAND_BLEND_DEG`, `MEANDER_MIN_WAVES`, `MEANDER_WAVES`, `MEANDER_SALT`, `meander_deg`, `meander_field`, `effective_latitudes`, `sea_level_temperature_c`, `westerly_weight`, `Wind`, `wind`, `RAIN_CROSS_BLUR_DEG`, `RAIN_ALONG_BLUR_DEG`, `orographic`, `compute`, `compute_with_lakes`, `monthly_placeholder`, `season_phase` and the placeholder's constants that Task 7 did not take over (`SEASON_AMP_*`, `CONTINENTALITY_HALF_DEG`, `SEASON_LAG_*`, `DIURNAL_CONTINENT_C`, `DIURNAL_DRY_REFERENCE_MM`, `PRECIP_SEASON_*`, `WIND_PLACEHOLDER_MS`); the knobs `equator_temp_c`, `pole_temp_c`, `meander_amplitude_deg`, `meander_wavelength_deg`, `trade_wind_edge_deg`, `polar_edge_deg`, `land_depletion_deg`, `sea_recharge_deg`, `rain_min`, `rain_max`. **Kept:** `Climate`, `latitudes`, `latitude_at`, `metres_per_degree`, `lakes_from_filled`, `box_blur_rows`, `radius_cells`, `TERRAIN_BLUR_M`, `OROGRAPHIC_RUN_M`, the climate-grid rule, `MonthlyFields` and its `impl`. The module doc of `climate/mod.rs` is rewritten to describe the two stages.
**Report lines removed** (the exemption list of this task — their subject is deleted): `monthly_temp_mean_err_c` and `monthly_precip_sum_err_mm` (both measured the placeholder against its own annual curve). Every other line stays, in order.

- [ ] **Step 1:** delete; fix every reader; `grep -rn` each deleted knob name over `crates/`, `justfile`, `website/`, `deploy/` (docs excepted) prints nothing.
- [ ] **Step 2: Evidence** — `just forge-report --size dev --cell 32 --seeds 8`: `diff` against `t10-after.txt` shows ONLY the two removed lines (`height_crc`, `artifact_rasters_crc`, `koppen.*` identical per seed: a deletion moves no world). `just forge-ui`'s knob list is not launched; instead quote `SPECS`' "Climate" group as compiled (a temporary `println!` test or `forge_proto --help` if it lists knobs) — seven entries, each with a reader named in the report.
- [ ] **Step 3: Verify, in order** — `just smoke`; `just test-one worldgen "--no-run --tests --examples"`; `just web-check`; `just clippy` for `worldgen` (dead code the deletions exposed).
- [ ] **Step 4: Commit** `refactor(forge)!: delete the annual climate, the seasonal placeholder and their ten knobs`.

### Task 12: Tune the ocean currents; prove the fallback (at most four rounds)

The least-evidenced part gets its own before / after. No new mechanism. **Files:** the constants of `climate/ocean.rs` (and `COLD_CURRENT_DRY` in `moisture.rs`); `ocean_currents`' default only by a ruling.

**Evidence commands** (each round): `just forge-report --size dev --cell 32 --seeds 8` with currents ON, and the same with `--set ocean_currents=0`; `just forge-climate --world synthetic:continent --out target/world-forge/p3/t12-r<N>` with and without `--key ocean_currents=0`.

**The table** (per round, ON beside OFF, medians over 8 seeds and each seed's value): `koppen.A` … `.E`; `west_coast.cs_share`, `west_coast.cs` (count of `yes`), `east_coast.cs_share`; `koppen.BWk + koppen.BWh` on west coasts at 15–30° (add the line `west_coast.desert_share.15_30` in round 0, a no-behaviour change); `sea_temp_jan_c.45N` minus the same line with currents off; the continent's `winter_west_minus_east_c.50N`, `cs_share.30_45.west`.

**Targets:** with currents ON — `west_coast.cs: yes` on at least 5 of the seeds that have a west coast in the band (`n/a` seeds do not count; fewer than 4 countable seeds → add 8 more seeds, `--seeds 16`); `west_coast.cs_share` ON ≥ OFF on the median; no main class moved OUT of its range by turning currents on. **The fallback:** with `ocean_currents = 0` the five main-class medians are inside the program's ranges (arid 20–40 %, cold 10–35 %, tropical 10–30 %, temperate 8–22 %, polar 6–20 %) or miss by no more than they miss with currents ON — currents may improve the map, they may not be what holds it up.

- [ ] **Step 0:** round 0 (the lines, both runs, the table). **Steps 1–4:** up to four rounds; dials `CURRENT_*_C`, `CURRENT_REACH_DEG`, `CURRENT_INLAND_REACH_DEG`, `UPWELLING_C`, `COLD_CURRENT_DRY`, `CURRENT_LAND_SMOOTH_DEG`; READ `-current.png` and the month-03 and month-09 temperature sheets each round.
- [ ] **Step 5:** the before / after table (round 0 against the last) in the report; remaining misses as rulings; if currents make the forged worlds WORSE on the Köppen ranges and no dial fixes it, the ruling lowers `ocean_currents`' default (to 0.5 or 0) and says so in `knobs.rs`' help and the Known limits.
- [ ] **Step 6: Verify** — the second regime's constraints on the last run; the gate WITHOUT bless (message quoted); `just smoke`; `just web-check`.
- [ ] **Step 7: Commit** `tune(forge): ocean-current anomalies — before / after, and the world without them`.

### Task 13: Tune to Earth's ranges — Köppen shares, the west coast, the interiors (at most four rounds)

No new mechanism. **Files:** the constants of `climate/{ebm, circulation, moisture, fine}.rs`, the defaults of the four new knobs (`knobs.rs`; `ClimateKey::DEFAULT` kept equal).

**Evidence commands** (each round): `just forge-report --size dev --cell 32 --seeds 8`; `just forge-report --size medium --cell 32 --seeds 2`; the three synthetic commands of Task 9 (a round must not undo them: their table is re-printed and any line that turned ✗ is a miss of this round).

**Targets** (unweighted shares of land; Decision 12):

| Line | Target |
|---|---|
| `koppen.B` (arid) | 0.20–0.40 |
| `koppen.D` (cold) | 0.10–0.35 |
| `koppen.A` (tropical) | 0.10–0.30 |
| `koppen.C` (temperate) | 0.08–0.22 |
| `koppen.E` (polar) | 0.06–0.20 |
| how the five are judged | the MEDIAN over the 8 dev seeds inside its range for all five; at most 2 of 8 seeds with any class more than 5 points outside; both Medium seeds within 3 points of the ranges |
| `west_coast.cs` | `yes` on at least 5 of 8 dev seeds (as counted in Task 12) and on at least 1 of the 2 Medium seeds |
| `interior.D_share`, `interior.B_share` (seeds with `interior.cells` ≥ 1 % of land) | median D ≥ 0.3, median B ≤ 0.5 |
| `climate.precip_land_mean_mm` | 550–950 on the median |
| today's biomes (Ruling 8) | at least as many biomes present per seed (median) as in `t10-before.txt`; no land biome above 0.45 of land on the median |
| Task 10's sanity lines | still hold |

**Known before it is measured** (so the first round does not chase it blind): the polar seas take every row poleward of about 76° (`ocean_margin_start` 0.84), and Peel's 12.8 % polar is nine points Antarctica — on this map `E` can only come from land between the 10 °C warmest-month line and 76°, plus mountains on Medium and Large. `koppen.E` under 0.06 on Dev is the likeliest miss; `global_mean_temp_c` and `EBM_DIFFUSION_W_M2_K` move it and move `D` and `A` with it. If it cannot be met without breaking another class, the ruling records the reachable figure — it does NOT move the polar margin (Phase 2's) or re-define the class.

**Dials:** Task 9's table, plus `RAIN_UNIT_MM` (the land mean; B against everything else), `global_mean_temp_c`'s default (A and E in opposite directions), `inland_reach_deg`'s default (B in interiors), `LEE_RECOVERY_DEG` and `ORO_FACTOR_MIN` (B in lee belts on Medium), `orographic_scale_m`'s default.

- [ ] **Step 0:** round 0 = the two reports at Task 12's last commit, the targets table filled.
- [ ] **Steps 1–4: up to four rounds.** Each: the dials and why; both reports; the table; the synthetic table; and LOOK — `just forge-proto --size dev --cell 32 --seed <the worst seed> --out target/world-forge/p3/t13-r<N>` then `just forge-climate --size dev --cell 32 --seed <same> --out target/world-forge/p3/t13-r<N>`: READ `-koppen.png`, `-rain-03.png`, `-rain-09.png`, `-temp-03.png`, `-temp-09.png`, `-pressure-03.png`, `-wind-03.png` and say per sheet what is wrong on that seed (a desert on a windward coast, a wet pole, no seasonal swing of the rain belt, …) — a round that changes dials without looking is not a round.
- [ ] **Step 5:** misses after round four as rulings, each with the 8-seed median, the range, the dial that was closest and what it broke.
- [ ] **Step 6: Verify** — the second regime's constraints; the gate WITHOUT bless (message quoted); `just smoke`; `just test-one worldgen "--no-run --tests --examples"`; `just web-check`.
- [ ] **Step 7: Commit** `tune(forge): climate against Earth's Köppen shares, the west-coast check and continental interiors`.

### Task 14: Maps, and the evidence across sizes

**Files:** `crates/worldgen/examples/world_map.rs` (`ForgedLayer` gains `Koppen` and `MonthlyWind`, named `koppen` and `monthly-wind`; the usage text and header), `justfile` (the `world-map` comment), `crates/worldgen/examples/forge_proto/climate_tool.rs` (the cross-size mode), `crates/worldgen/src/forge/render.rs` only if a preview layer for Köppen is cheap (`PreviewLayer` is shown by `forge-ui`; adding one changes `PreviewLayer::ALL`'s length — do it only if every reader is in `worldgen`; else leave it and say so).

**Interfaces:**
- `world-map --world <file> --layer koppen`: per pixel `koppen::classify(&world.monthly_at_ground(x, z))` coloured by `Koppen::rgb`, sea dimmed as the annual climate layers dim it; the whole world unless `--span` / `--center` (Phase 1b's rule for monthly layers); prints the five main-class shares of the picture's land pixels. `--layer monthly-wind --month <1..=12>`: speed as brightness, direction as hue, from `world.monthly_normals(x, z).wind_ms`; prints the vector at 75°, 45°, 15° north and south of the middle column.
- `forge_proto --climate --sizes dev,small,medium --cell 32 --seed <s>`: forges the same seed at each size and prints, per pair, `cross.koppen_l1.<a>_<b>` (Σ over the five main classes of |share_a − share_b|), `cross.biome_l1.<a>_<b>` (the same over land biomes — Phase 1a Task 18's metric), `cross.planetary_t_gap_c.<a>_<b>` and `cross.planetary_p_gap_rel.<a>_<b>` (the largest difference of the two worlds' `Planetary::t_sea_c`, and of `precip_flat_year_mm` relative to its mean: the model grid is the same, so these measure only how much the LAND MASK differs between sizes), and per size `cross.koppen.E.<size>`, `cross.max_peak_m.<size>`.

- [ ] **Step 1: Implement** (no temporary test: the evidence is the pictures and the printed numbers; the evidence run is this task's compile check for the examples).
- [ ] **Step 2: Evidence** — `just forge-proto --size dev --cell 32 --out target/world-forge/p3/final` and `--size medium --cell 32 --out target/world-forge/p3/final-medium`; for each `just world-map --world <file> --layer koppen --out <stem>`, `--layer monthly-wind --month 4` and `--month 10`, `--layer monthly-temperature --month 4` / `10`, `--layer monthly-rain --month 4` / `10`, and `--tiles`: quote the printed values and PNG paths; READ every picture of the Medium world and say in one line each what it shows. `just forge-climate --sizes dev,small,medium --cell 32 --seed <default>` and one more seed: quote every `cross.*` line. **Judged:** `cross.koppen_l1 ≤ 0.25` for the three pairs on both seeds (Ruling 7); `cross.planetary_t_gap_c ≤ 3` (the planetary climate is the same world at each size up to the coast's detail). A miss is recorded with the class that carries it (expected: `E` and `B`, by Ruling 7's argument about small worlds' low mountains) — not tuned. The second regime's constraints on a fresh `just forge-report --size dev --cell 32 --seeds 8` (nothing in the library changed: `height_crc` equal to Task 13's last run).
- [ ] **Step 3: Verify** — `just smoke`; `just test-one worldgen "--no-run --tests --examples"`.
- [ ] **Step 4: Commit** `feat(worldgen): Köppen and seasonal-wind map layers; the climate compared across world sizes`.

### Task 15: Evidence, the gate re-record, every size measured, close

**Files:** `crates/worldgen/tests/fixtures/forge_quality_planet_dev_32m.ron` (the phase's ONE re-record), `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` ("Measurements": a "Phase 3" table; Decision 4's Phase 3 line and Decision 7 as built; the Phases table row 3 gate reworded — "Köppen shares inside their ranges; three synthetic worlds; a west-coast check" — the Earth regression struck with Ruling 12's reason; "Open questions": ocean currents closed, by rule), `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Status row 3 → DONE with the plan path and commit range; Evidence: "Phase 3"; Owner hand checks; the notes this phase changes for Phases 4, 5, 6: what is in memory on `Forged` for the forge-side phases — `planetary`, `monthly` — the functions they call — `climate::derived::derive`, `forge::koppen::classify`, `forged::ground_normals` — and that nothing derived is stored), this file ("As built": commits per task, the rulings made by tasks verbatim, every tuned constant's final value beside its starting value, deferred minors), `TODO.md`, `CLAUDE.md` if a fact is still stale, `crates/worldgen/src/forge/play.rs` (`FORGE_SECS_PER_MCELL`, `FORGE_COST_EXPONENT`, `FORGE_RAM_*`, and an additive constant for the planetary stage's fixed cost) ONLY if a measured row is more than 25 % off the estimate.

- [ ] **Step 1: The phase's evidence run** — `just forge-report --size dev --cell 32 --seeds 8` and `just forge-report --size medium --cell 32 --seeds 2` (every line); the same two with `--set ocean_currents=0` (the main-class lines); the three synthetic commands (every `synth.*` line); Task 14's maps (paths). Pasted into the program's Evidence section under "Phase 3", judged against its row: the five Köppen shares per seed, the medians and the verdict by Task 13's rule; the full thirty-type table (the summary's min / median / max); the three synthetic worlds' tables; the west-coast line per seed; **the known failure as a measured line** (`interior.D_share`, `interior.B_share`, and the continent's); the seam rule for the monthly fields (`seam_pass_monthly_temp`, `_precip`, `_wind`) beside height, precip and biome; `climate.roll_identical`; the polar and equatorial sanity (`sea_level_temp_c.*`, `sea_level_temp_month_c.*`); the climate stage alone (`climate.planetary_ms`, `climate.refresh_ms`, `climate.final_ms` beside the forge's wall time); the cross-size lines. Every accepted miss is listed there with its ruling.
- [ ] **Step 2: The gate** — `FORGE_QUALITY_BLESS=1 just test-one worldgen "--test it -- --include-ignored forge_lib::forge_quality_matches_baseline"`, then the same without bless twice (it passes). A commit of its own, `test(worldgen): re-record the forge quality gate after the monthly climate`, whose message states the eleven old numbers (`git show HEAD:crates/worldgen/tests/fixtures/forge_quality_planet_dev_32m.ron` before the bless — what Phase 2 left) and the eleven new.
- [ ] **Step 3: Measure** (release, one at a time, nothing else of ours running): `just forge-bench 16384 8192 32`, then Tiny (`25600 12800`), Small (`50176 25088`) and Medium (`100352 50176`) at `16` and at `32`, and Large (`200704 100352`) at `32`; Large at 16 m only if `free -g` shows more than 20 GB available, else "not run". For each: wall seconds, max RSS, the `.world` size, and from the tool's own printout the three climate timings; note `scaling_cur_freq`.
- [ ] **Step 4: Record** the "Phase 3" table in the design beside Phase 2's rows, with the difference per row and one line attributing any row more than 25 % above Phase 2's (the planetary stage's fixed cost on the small sizes; the fine stage's four sweeps; four factor planes per fine cell). No pass / fail line: the owner judges. Refit `play.rs` only by the rule above (a temporary test: the estimate within ±25 % of every measured row).
- [ ] **Step 5: Whole suites, once:** `just test` in the foreground, then `just clippy`; quote the PASS / FAIL line per stage. A failure in this plan's files is fixed here; one outside them is recorded.
- [ ] **Step 6: Owner hand checks appended to the program:** `just run --reset` (and `just pg-reset`); the twin and orion need a re-forge to get the model climate (an older world still loads with the placeholder's); `just world-map --layer koppen` on a Medium world beside its biome map — do the climates sit where Earth would put them (deserts on west coasts near 20–30°, a Mediterranean strip poleward of them, wet east coasts, cold interiors), and do the January and July rain maps show the belts moving; today's biomes on the new climate are a stopgap until Phase 5 (name any biome the tuning could not keep); small worlds have weak rain shadows and no alpine climate (Ruling 7); the generator lost ten climate sliders and gained four; the "Dry" and "Wet" presets now work through `rainfall` and `inland_reach_deg`.
- [ ] **Step 7: Commit** `docs: Phase 3 measurements and evidence; the monthly climate closed`.

## Known limits (what the stylised model deliberately does not do)

- **It is not a circulation model.** Pressure is one cosine of a belt coordinate plus a thermal anomaly; there is no jet, no storm track, no weather. Wind is a rule's turn of the pressure gradient; Coriolis is a sign and a ramp.
- **The planetary stage reads no height.** No snow albedo over plateaus, no blocking or steering of the wind by ranges, no föhn warmth: relief acts through the lapse (at read time) and the fine stage's four axis sweeps. A range oblique to the wind casts a mixed, blurred shadow.
- **The map is flat for the air and round for the sun.** Polar rows carry as much area as equatorial ones in the diffusion and in every share; the 15 °C mean alone is cos-weighted.
- **Ocean currents are a rule on basin sides**, added after the energy balance: gyres do not close, there is no circumpolar current, straits and inland seas count as basins, the anomaly is the same all year but for upwelling. `ocean_currents = 0` is a supported world.
- **Rain regimes that need dynamics are absent**: double rainy seasons at the equator come only from the thermal equator crossing twice; no tropical cyclones, no east-coast winter storms; the monsoon is a thermal low's convergence term.
- **Inside one climate cell the months share one split** (windward and lee slopes differ in annual total, not in seasonality), and the diurnal range ignores elevation.
- **Small worlds**: mountains of a few hundred metres give weak rain shadows and no alpine `ET` / `EF`; the polar class there is latitude's alone, and the polar seas leave little land for it at any size.
- **Derived fields are forge-side functions, not stored**; Thornthwaite underestimates evapotranspiration in arid heat (capped at 26.5 °C: the corrected branch was not read). Phase 5 owns the dryland share, Phase 6 the runtime's snow.
- **Circular orbit, fixed tilt**: no eccentricity, and the tilt is a constant, not a knob.
- **No real-Earth regression** (Ruling 12) and no first-hand reading of the energy-balance literature behind the starting constants: every one of them was a starting value for Tasks 3–13.
- **No version bump**: a world forged before the switch still loads with the placeholder's climate.
- **Today's biome classifier reads the model's annual fields** until Phase 5; `game::biome`'s thresholds were set for the latitude table.
