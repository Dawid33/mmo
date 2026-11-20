# Monthly-Climate Container (Phase 1b) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Written 2026-11-20** from a code check of that day; **revised the same day** after the builder check
> (`.superpowers/sdd/2026-11-20-monthly-climate-container-1b/builder-check.md`, every BLOCKER and FIX settled in the
> text; symbols re-read at `be8ab249`, Phase 1a Task 18 landed). It is dispatched only after Phase 1a is DONE (its
> Task 19 landed). Search by the named symbol, never by line. **No step commits a test.**

**Goal:** Every forged world carries twelve monthly climate normals per climate cell — sea-level temperature, diurnal range, precipitation, wet-day fraction and wind — through the world file, the tiled store and an exact-arithmetic sampler, filled by a placeholder seasonal curve that Phase 3 replaces value for value.

**Architecture:** One new plain-data type, `game::MonthlyClimate` (a coarse grid of climate cells × 12 months × 6 integer planes, with its own exact bilinear sampler), so Phase 6's weather function in `game` can read it. The forge fills it from today's annual climate (`forge::climate::monthly_placeholder`); `WorldArtifact` and `ForgedMeta` each carry it whole (it is always resident — never tiled); `ForgedWorld` adds the world-position API (the lapse from the fine height, the fine annual precipitation as the rain-shadow modulator). Forged terrain and every existing raster stay bit-identical.

**Tech Stack:** Rust; crates `game` (the container type), `worldgen` (forge, `forged.rs`, `forged_tiles.rs`, the `forge_proto` and `world_map` examples), `world-store` (no code change expected: it stores the meta blob it is given). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` (Decisions 7–9; "Phase 1 in more detail": "Phase 1 owns the SHAPE of the monthly climate"). Program: `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Phase 1b; its Standing rules bind every task). Research: `docs/research/research_notes/Earth like world generation overhaul/climate_simulation.md` §7 (representation), `current_forge_audit.md` §9 and §13 D (storage), `runtime_seasons_weather.md` §2, §4, §9 (what the runtime reads).

Ledger: `.superpowers/sdd/2026-11-20-monthly-climate-container-1b/progress.md` (git-ignored workspace). The commit procedure, implementer and reviewer rules are Phase 1a's: `.superpowers/sdd/2026-11-14-planet-frame-1a/{common,implementer-rules,reviewer-rules}.md`.

## Global Constraints

- Do not create git worktrees; work on `develop`. No stash, reset, amend, checkout of files, clean, push. The tree
  holds the owner's unrelated uncommitted changes (among them `crates/game/src/lib.rs`, `CLAUDE.md`, `TODO.md`,
  `justfile`) and other sessions stage in the same index: each commit carries only the task's own hunks, staged
  through the index as `common.md` describes (never a pathspec commit of a mixed file). The committed-tree compile
  check (an export of HEAD type-checked in a scratch directory) is the CONTROLLER's, not a task's.
- **Tests.** The repository commits only regression tests of bugs actually seen and the existing gates. Every
  "temporary test" below is written first, seen failing, quoted in the implementer's report (name, command, failing
  then passing output) and DELETED before the commit; a commit adds no `#[test]`. An "evidence command" is a tool
  run whose output is quoted in the report. No new gate. A temporary test compares a whole container or artifact
  with `==` and prints a one-line summary on a mismatch — never `assert_eq!` on one (its `Debug` is megabytes).
- **The order inside every Verify step is fixed:** (1) the temporary tests pass; (2) DELETE them; (3) `just smoke`
  (its first step, the test audit, scans the working tree and fails on any `#[test]` without a `/// Regression:` or
  `/// Gate:` marker — a temporary test still present fails it); (4) the wasm check the task names. If smoke's audit
  fails on a file that is NOT the task's own (another session's temporary test), run
  `just test-audit <the crate directories the task touched>` and smoke's remaining steps by hand, and say so in the
  report (Phase 1a's fallback).
- **The wasm checks.** `just determinism-wasm` compiles only `game` and `physics-next`: it is run after Task 1
  (`game` changes). `just web-check` (`cargo check -p client --lib --bins --target wasm32-unknown-unknown`: the
  browser client links `worldgen`; minutes cold, seconds warm) is run after every task that changes `worldgen`'s
  library code (Tasks 2, 3, 4).
- **Forged terrain and the existing rasters do not change in this phase.** The quality gate
  `forge_quality_matches_baseline` (`crates/worldgen/tests/forge_lib.rs`, fixture
  `tests/fixtures/forge_quality_planet_dev_32m.ron`) is NOT re-recorded (the program re-records it at the end of
  1a, 2, 3, 4). The safety net, run by every task that touches `crates/worldgen/`:
  1. the gate passes UNTOUCHED: `just test-one worldgen "--test it -- --include-ignored forge_lib::"`;
  2. **the baseline report.** Task 2 Step 0 saves `just forge-report --size dev --cell 32 --seeds 2` and
     `just forge-report --size dev --cell 64 --seeds 8` to the ledger directory (`baseline-32.txt`,
     `baseline-64.txt`) BEFORE any change; Task 2 ends by saving them again as `baseline2-32.txt` /
     `baseline2-64.txt` (they then hold Task 2's lines too, among them `artifact_rasters_crc`). After each task the
     two commands are run again and every line of the newest baseline is present, unchanged and in order
     (`diff baselineK-N.txt new-N.txt | grep '^<'` prints nothing): lines are only APPENDED. Task 2 is judged
     against `baseline-*`, Tasks 3–5 against `baseline2-*`.
  3. **`artifact_rasters_crc`** (Task 2 adds the line): a crc of what the runtime reads from the artifact — every
     existing stored raster and table, the climate family excluded. `height_crc` is a crc of the FORGE's heights
     only; this one proves Tasks 3–4 moved no stored bit.
  A changed baseline line or a failing gate is BLOCKED, never a re-record.
- There is never a commit with no working, loadable world: the container reaches the world file and the tiles in
  ONE task (Task 3), with both format bumps.
- **Stored formats.** This phase bumps `ARTIFACT_VERSION` 9 → 10 and `TILED_FORMAT_VERSION` 2 → 3, each once, both
  in Task 3. `WORLD_MAP_VERSION` (3) and `META_FORMAT` (2) do NOT move: `build_world_map`'s output and
  `world.json`'s layout are untouched (`TileSummary` keeps its annual `temperature_c` / `precip_mm`). No other task
  may change a stored or wire byte layout; one that finds it must reports BLOCKED. Every world is re-forged, never
  migrated.
- `game`, `server`, `worldgen` stay Bevy-free. The container is NOT simulation state: no `#[rollback]`, no
  `StateHash`, nothing in a cut, a snapshot or a packet in this phase.
- **Runtime sampling is exact arithmetic** (add, subtract, multiply, divide, floor, sqrt) in a FIXED expression
  order — everything in `game::monthly_climate` and everything `ForgedWorld` adds. The forge side
  (`forge::climate`) may use std float math.
- Never write a region edge as a literal. Tests build at region 64, play at 256.
- Verification per task: its temporary tests; `just test-one <crate> "<filter>"` for the modules of the files it
  touched where tests exist; the safety net (worldgen tasks); `just smoke`; the wasm check. Whole suites
  (`just test`) once, in Task 6.
- Any new or changed command updates its `justfile` recipe (or its comment) in the same task; `CLAUDE.md`
  (Worldgen paragraph) and `TODO.md` change in the commit that changes the fact.
- Never launched by agents: the game window, paid generation, anything on orion or the twin, the dev Postgres
  world, benchmarks other than `just forge-bench`.
- Rulings made by a task are written in its report under "Rulings" as `Ruling: … — why — cost if wrong`.

## Shared vocabulary (decided here, used by every task)

- **Fine grid**: the forge grid as today (`nx × nz` cells of `cell_m`). **Climate grid**: `cnx × cnz` climate
  cells of `climate_cell_m`, same axes (x west → east, wraps; z north → south), row-major `c = cz · cnx + cx`, one
  value per cell CENTRE.
- **The climate cell rule** (Ruling 1): about 256 m, but never fewer than `CLIMATE_MIN_ROWS = 96` rows pole to pole
  (at most 1.9° of latitude per climate cell), and never finer than the forge cell.
  `climate_stride(cell_m, nz)`: start at `s = max(1, round(CLIMATE_CELL_M / cell_m))` (`CLIMATE_CELL_M = 256.0`);
  while `s > 1` and `nz / s < CLIMATE_MIN_ROWS`, halve it (`s /= 2`, integer). `climate_cell_m = cell_m · s`;
  `cnx = nx / s`, `cnz = nz / s`, both required to divide exactly (else the forge refuses, Task 2). The climate grid
  then spans the world exactly and its column `cnx − 1` is beside column 0 round the seam.

  | Size | `width × height` m | Climate cell | Climate grid | Degrees per cell | stride at 16 / 32 m | Raw bytes (96 B/cell) |
  |---|---|---|---|---|---|---|
  | Dev | 16,384 × 8,192 | 64 m | 256 × 128 = 32,768 | 1.41° | 4 / 2 | 3,145,728 (3.00 MiB) |
  | Tiny | 25,600 × 12,800 | 128 m | 200 × 100 = 20,000 | 1.80° | 8 / 4 | 1,920,000 (1.83 MiB) |
  | Small | 50,176 × 25,088 | 256 m | 196 × 98 = 19,208 | 1.84° | 16 / 8 | 1,843,968 (1.76 MiB) |
  | Medium | 100,352 × 50,176 | 256 m | 392 × 196 = 76,832 | 0.92° | 16 / 8 | 7,375,872 (7.03 MiB) |
  | Large | 200,704 × 100,352 | 256 m | 784 × 392 = 307,328 | 0.46° | 16 / 8 | 29,503,488 (28.1 MiB) |

  At the cells in use (8, 16, 32, 64 m) a size has ONE climate grid whatever the forge cell. A coarser forge cell
  caps it: Dev at a 128 m preview cell has a 128 × 64 grid of 128 m cells. For scale: the fine rasters are 17 B per
  fine cell — Medium at 16 m: 334 MB raw against 7.4 MB of climate (2.2 %); Dev at 32 m: 2.2 MB against 3.1 MB (the
  dev world's climate family is larger than its terrain; it is the price of 1.4° cells on a 8 km-tall planet).
  Compressed sizes are measured in Task 6 (smooth fields: expected far below raw).
- **Months.** `CLIMATE_MONTHS = 12` = the calendar's `MONTHS_PER_YEAR` (`game/src/history/calendar.rs`, a `u64`);
  month index `m` (0-based) is CALENDAR month `m + 1` (0 = Granite). A month is 28 days (`DAYS_PER_MONTH`, a
  `u64`: `as f64` where a formula uses it); a stored normal is the month's mean, anchored at mid-month. The year
  starts at the northern spring equinox, so the northern summer solstice is at
  `SOLSTICE_NORTH_SUMMER_MONTHS = 3.0` months (the boundary of months 2 | 3) and the winter one at 9.0.
  **"July"** in the program's evidence row is month 3 (`NORTH_MIDSUMMER_MONTH`), **"January"** is month 9
  (`NORTH_MIDWINTER_MONTH`) — the month that follows each solstice, as on Earth.
- **The six planes** (each `12 · cnx · cnz` values, MONTH-MAJOR: `i = m · cnx · cnz + cz · cnx + cx`). A stored
  integer `raw` decodes as `raw as f64 / DENOMINATOR` — a division by an exactly representable whole number, the
  same in every reader; the forge quantises as `round(value · DENOMINATOR)`, saturating:

  | Field | Type | Denominator (steps per unit) | Range | Worst quantisation error |
  |---|---|---|---|---|
  | `temp_sea_centi_c` | `i16` | 100 per °C (sea-level temperature) | −327.68 … 327.67 °C | 0.005 °C |
  | `diurnal_deci_c` | `u8` | 10 per °C (mean daily max − min) | 0 … 25.5 °C | 0.05 °C |
  | `precip_deci_mm` | `u16` | 10 per mm (per month) | 0 … 6,553.5 mm | 0.05 mm (0.6 mm on a year) |
  | `wet_day_255` | `u8` | 255 per 1 (share of the month's days that are wet) | 0 … 1 | 0.00196 |
  | `wind_east_q` | `i8` | 4 per m/s toward +x (east) | −32 … 31.75 m/s | 0.125 m/s |
  | `wind_south_q` | `i8` | 4 per m/s toward +z (south) | −32 … 31.75 m/s | 0.125 m/s |

  2 + 1 + 2 + 1 + 1 + 1 = 8 B per month, 96 B per climate cell. A value outside a range saturates at the bound
  (the report counts them: `climate_clamped_values`).
- **Datum.** A stored temperature is at SEA LEVEL. "At the ground" means `sea − lapse_c_per_m · ground`, `ground`
  = the bilinear interpolation of the fine cells' heights EACH FLOORED AT 0 (m above the sea; `snow_line_m`'s
  read), the lapse the world's own (`WorldGlobals::knobs().lapse_c_per_km`). The existing annual `temperature_c`
  raster stays what it is: the annual mean AT THE GROUND.
- **Map metres**: `(ax, az)`, x east from the west edge, z south from the north edge — `forged.rs`'s "artifact
  metres". The container knows only map metres; `ForgedWorld` converts engine metres (`to_artifact`).
- **Precipitation at a fine position** (the rain-shadow rule): `P_m(x) = A(x) · N_m(x) / Σ_k N_k(x)` — `A` the fine
  annual precipitation raster (bilinear), `N_m` the climate grid's monthly normals (bilinear). The twelve values
  sum to `A(x)`: the fine annual total is conserved by construction, with add, multiply and divide only.

## Rulings (made while writing this plan; each `what — why — cost if wrong`)

1. **The climate cell is 256 m, halved until the grid has at least 96 rows, never finer than the forge cell** (the
   stride rule above) — the design names "about 256 m" and that is what Small, Medium and Large get. But the
   family is PLANETARY data, and a fixed metre cell is 5.6° of latitude on Dev and 3.6° on Tiny, while Phase 3's
   planet grid is 512 × 256 to 1,024 × 512 (0.70° to 0.35° per cell) on EVERY size: the container Phase 3 fills
   would be eight times coarser than its model on Dev. The 96-row floor holds every size to at most 1.9° per cell.
   The grid cannot be "the same number of cells per degree on every size" with whole strides (the sizes' cell
   counts share no factor above 32 columns at a 32 m cell), and a resampled degree grid would be finer than the
   terrain on Dev — so the rule is a floor, not a fixed grid. **What it means for Phase 3:** the container's
   resolution, not the planet grid's, is the limit of what is stored — 1.4° to 1.9° on Dev, Tiny and Small, 0.92°
   on Medium, 0.46° on Large; Phase 3 averages its planet grid down onto it (or up, on Large). That is accepted:
   what must survive is planetary (bands 10° and wider, coast-to-interior gradients over several degrees, current
   strips a few degrees wide), and everything finer — rain shadows, the lapse — is carried by the fine rasters at
   read time. The container stores its own `nx`, `nz`, `cell_m` and a reader checks only that the grid spans the
   world, so Phase 3 may change `CLIMATE_MIN_ROWS` or the stride with NO format change — cost if wrong: one
   constant and a re-forge; 3 MiB of climate on a dev world whose terrain is 2 MiB.
2. **A world whose sides are not whole climate cells is refused at forge time** (`check_climate_grid`, beside
   `check_wrap`) — a partial last row would need area-weighted means and a second edge rule in the sampler; no
   named size, preview cell or recipe is affected (verified: every `WORLD_SIZES` / `DEV_WORLD_SIZE` side is a
   multiple of 256 m; cells are 8 … 256 m) — cost: a hand-typed `--height` that is not a multiple stops forging.
3. **The family is always RESIDENT: it lives whole in `WorldArtifact` and in `ForgedMeta`, never in tiles** — at
   most 28 MiB (Large); Phase 6 samples it every weather step from region threads and must never wait on, pin or
   fail a tile load; bilinear reads across tile borders and a second prefetch radius are avoided; no new store
   table, row or migration (the `forged_meta` blob grows; it is read once per process start, by `load_forged`, and
   blob limits are 1 GB) — cost if wrong: a far larger world or a finer climate cell wants tiling, which is one
   `TILED_FORMAT_VERSION` bump then; each host and coordinator process holds one copy (+28 MiB per pod at Large).
   The tile cache budget does not count it.
4. **The container type is `game::MonthlyClimate`, not a `worldgen` type** — the program asks for "a sampler API
   in `worldgen` that Phases 5 and 6 read", but Phase 6's weather is a pure function "in `game`", and `game`
   cannot depend on `worldgen` (`worldgen` depends on `game`; precedent: `game::CellClimate`, `game::WorldMap`).
   The data type and its map-metre sampler live in `game`; the world-position API (engine metres, lapse,
   modulator) lives on `worldgen::forged::ForgedWorld` — cost if wrong: a file move; bincode's layout does not
   depend on the crate, so no format change.
5. **The month index is the calendar month; the solstices sit at 3.0 and 9.0 months; "July" = month 3, "January"
   = month 9** — the calendar has twelve 28-day months with Dwarf Fortress's names, whose year starts in spring
   (the research's reading; nothing in `game` or `client` names a season or an equinox yet), and the stored normals
   must be indexable by the date Phase 6 derives from the world tick — cost if wrong: one constant and a re-forge
   (values, not format).
6. **Encodings as in the table: integer planes with FIXED whole-number denominators (no per-raster `min` /
   `step`, no `f32` step constants)** — a linear `u16` keeps the annual sum to 0.6 mm and needs no square root;
   96 B per climate cell is small beside the fine rasters, so the research's tighter byte encodings buy nothing;
   fixed units mean Phase 3 cannot drift the format by accident; dividing by 100, 10, 255 or 4 is exact-rule
   arithmetic with no widened-`f32` bias. The planes are `pub` and `index(month, cx, cz)` addresses them, which is
   the INTEGER access Phase 6 may want (exact integer compares on centi-degrees); the `f32` normals are the
   convenience read — cost if wrong: a field needs more range (a month above 6,553 mm, a diurnal range above
   25.5 °C) and saturates; the report counts saturated values.
7. **Wind is stored NOW: twelve months × (east, south) components, filled by today's zonal bands** — the program
   has Phase 3 "add wind direction by season" to the container, and lets Phase 6 start as soon as 1b is DONE on
   "the container's shape" while its weather "drifts with the stored seasonal wind": a shape without wind would
   make Phase 6 guess and Phase 3 bump both formats for it. Components, not an angle, interpolate bilinearly with
   no wrap-around; monthly, not four seasons, so every field has one index — cost: 24 B per cell of placeholder;
   if Phase 3 needs more than these six planes (stored snow or ice normals, say) that is its own one bump.
8. **Precipitation at a fine position is the fine annual raster times the coarse monthly SHARE** (formula above)
   — it is exact arithmetic, the twelve months sum to the annual raster every existing consumer reads (biome
   classifier, habitat, the world map), and Phase 3 needs no stored "coarse annual mean": the divisor is the sum
   of the same twelve interpolated normals. The coarse absolute values are still returned unmodulated by
   `monthly_normals` (weather per climate cell) — cost if wrong: Phase 3 wants the coarse totals to rule; it then
   writes the fine annual raster as the sum of its fine-resolved months, and this rule still holds.
9. **The lapse is taken from the bilinear interpolation of the fine cells' heights, each floored at 0 first**
   (`bilinear_cells(ax, az, |i| f64::from(q(Height, i)).max(0.0))`, exactly `snow_line_m`'s read), not from the
   bicubic detailed ground and not from `max(bilinear height, 0)` — the annual raster bakes `max(h, 0)` per CELL
   (the sea at 0), so flooring after interpolating would let a sea-floor neighbour's depth warm a coastal point;
   the read stays inside the existing tile read radius, and Phase 6 applies its own per-column lapse on top in
   `game` — cost: a column's monthly temperature ignores sub-cell relief (at most `lapse · ±half a cell's relief`).
10. **The container stores no engine origin** — `forge_proto --spawn` overrides `WorldArtifact::spawn` after the
    artifact is built (`main.rs`: `artifact.spawn = spawn`), and the engine frame is `spawn`-relative
    (`to_artifact_raw`): a stored origin would go stale. It stores `wrap_x` (the sampler needs it) and that IS
    cross-checked against `edges` at load — cost: Phase 6 hands `game` the frame offset beside the container.
11. **The placeholder's constants are named `const`s in `forge/climate.rs`, not knobs** — knobs are stored in the
    world file, shown in the generator and `forge-ui`, and Phase 3 deletes the placeholder whole; an anti-goal of
    the design is a knob that outlives its stage. Its one planetary length is in degrees of latitude
    (`CONTINENTALITY_HALF_DEG`, through `metres_per_degree`), as every climate length is since Phase 1a Task 18 —
    cost: tuning the placeholder is a code edit.
12. **Month interpolation is the caller's**: the API returns all twelve normals of a place; it takes no date —
    Phase 6 owns the date (from the world tick) and the choice of linear or periodic-cubic interpolation, in
    `game`, in integers — cost: none.
13. **The annual mean of the monthly temperatures and the annual raster are two reads of one curve and agree only
    to interpolation error** — with Ruling 1's grid the coarsest climate cell is 1.84° (Small), against the
    meander's shortest wave of 19.4° (`MEANDER_WAVES` share 0.27 of `meander_wavelength_deg` 72; 1.75° high): at
    least 10 cells per wave, so the expected gap is below 0.1 °C at every size. The report prints it
    (`monthly_annual_temp_gap_c`) so it is seen, not assumed; a p99 above 0.3 °C on any size is a finding (BLOCKED
    or a ruling), not noise — cost if wrong: a defect in the placeholder or the sampler hides as "interpolation".
14. **Nothing existing is rewired**: `QRasterId`, `ByteRasterId`, `Tile`, `CellClimate`, `game::classify`,
    `ColumnClimate`, `habitat`, `tile_map` and `TileSummary` are untouched; no consumer reads the monthly family
    in this phase except the report and one map-tool layer — that is what keeps terrain, biomes and the world map
    bit-identical (no committed gate pins a changed byte: the quality gate reads `quality(&forge(&p))`, the scene
    and tree gates run procedural) — cost: none (Phases 5 and 6 are the consumers).
15. **The seam check of the monthly fields reuses Phase 1a's rule on climate-grid columns** (per seed: ratio
    ≤ 1.5 OR rank ≥ 0.05; over 8 seeds: at most 2 `no` AND median rank ≥ 0.25), for month 3's sea-level
    temperature and precipitation — cost: none (256 columns at Dev, as the fine grid has at 64 m).
16. **Continentality is computed on the CLIMATE grid** (a climate cell is sea when at least half its fine cells are
    ocean; `coast_distance_by` over `cnx × cnz` cells of `climate_cell_m`), not by a chamfer of the fine grid — a
    fine chamfer inside `Forged::artifact` costs 4 B per fine cell and seconds at Large (315 MB at 16 m), and
    `Forged::artifact` is also called per preview by `forge-ui`, by the seed search and by the in-client Play
    forge; a distance resolved to one climate cell is all a placeholder amplitude needs — cost: the amplitude steps
    by a climate cell at coasts (then blurred by the bilinear read).
17. **The old-format refusal of a Postgres store is argued from the code, not run** (builder check, Task 3 item 6)
    — the refusal is `world_store::load_forged` comparing the `forged_meta.format_version` column it got from
    `StoreHandle::forged_head`, one function for both backends; a Postgres run would need a scratch database made
    by the old build on the dev server the owner's stacks use, for no extra code covered — cost if wrong: the
    Postgres row decoder differs from SQLite's in a way that turns the refusal into another error; it would still
    be an error, never a load.

**Research claims relied on that are recalled or inferred, not read from a source** (all only feed PLACEHOLDER
values Phase 3 replaces; none becomes a format decision): the wet-day rule "wet days = month's rain ÷ a mean
wet-day amount of about 5 mm (cool) to 14 mm (hot)" (the runtime note's inference; Geng's coefficients are NOT
used); the diurnal ranges 4 … 16 °C (the note calls its own figures "working assumptions"); the two-month lag of
the sea (assumed here; the program gives only "a one-month lag on land"); the amplitudes 7 °C (maritime) and
25 °C (continental) at the pole (chosen here to give about 10 °C and 35 °C of annual range at 45°).

## Review Focus

1. **Datum confusion** — the stored monthly temperature is sea-level, the existing annual raster is at the
   ground. `monthly_normals` returns sea level, `monthly_at_ground` the ground; any code that mixes them is wrong
   by `lapse · height` (13 °C at 2,000 m) (Tasks 1, 4).
2. **The existing world changes** — a refactor of `Forged::artifact`, `climate.rs` or `split` that moves one bit
   of a stored raster moves `artifact_rasters_crc`, a baseline line or the gate. `monthly_placeholder` must only
   READ `World`, `Climate` and `ForgeParams` (Tasks 2, 3).
3. **The seam and the poles in the coarse sampler** — x wraps by INDEX (`rem_euclid(nx)`), z clamps; a point
   within half a climate cell of the seam interpolates between columns `cnx − 1` and 0; image-invariance for an
   arbitrary x is the caller's fold (`ForgedWorld::to_artifact`), not the container's (Tasks 1, 4).
4. **Whole world ≠ tiled world** — the tiled world's container comes from the meta blob through its own
   delta / un-delta; one missed plane or a row width of the fine `nx` instead of the climate `nx` gives a tiled
   world that loads and reads wrong values (Task 3; `climate_tiles_identical` in Task 4).
5. **An old world on disk or in a store** — refused with "re-forge" wording on the file, the menu list and the
   SQLite / Postgres store; never a panic in the server, never a silent forge over saved regions, never a re-tile
   from a v9 archive (Task 3).
6. **Month and hemisphere phase** — off by one month, or the same sign in both hemispheres, still produces
   plausible numbers; the band lines (north swing positive, south negative, zero band near zero) are the check
   (Task 2).

---

### Task 1: `game::MonthlyClimate` — the container and its exact sampler

**Files:**
- Create: `crates/game/src/monthly_climate.rs`
- Modify: `crates/game/src/lib.rs` (`pub mod monthly_climate;` and `pub use monthly_climate::*;` beside `biome` — the file has the owner's uncommitted hunks: stage only these two lines). No symbol of these names exists in `game` today (the root glob re-export collides with nothing).

**Interfaces — produces** (all `pub`, re-exported at the crate root):

```rust
pub const CLIMATE_MONTHS: usize = 12; // const-asserted == history::calendar::MONTHS_PER_YEAR as usize
pub const SOLSTICE_NORTH_SUMMER_MONTHS: f64 = 3.0;
pub const NORTH_MIDSUMMER_MONTH: usize = 3;
pub const NORTH_MIDWINTER_MONTH: usize = 9;
/// Stored steps per unit: a raw value decodes as `raw as f64 / DENOMINATOR` (the plan's table).
pub const CLIMATE_TEMP_PER_C: f64 = 100.0;
pub const CLIMATE_DIURNAL_PER_C: f64 = 10.0;
pub const CLIMATE_PRECIP_PER_MM: f64 = 10.0;
pub const CLIMATE_WET_DAY_PER_ONE: f64 = 255.0;
pub const CLIMATE_WIND_PER_MS: f64 = 4.0;
/// Stored bytes per climate cell.
pub const CLIMATE_BYTES_PER_CELL: usize = 96;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonthlyClimate {
    pub nx: u32,
    pub nz: u32,
    pub cell_m: f64,
    pub wrap_x: bool,
    pub temp_sea_centi_c: Vec<i16>,
    pub diurnal_deci_c: Vec<u8>,
    pub precip_deci_mm: Vec<u16>,
    pub wet_day_255: Vec<u8>,
    pub wind_east_q: Vec<i8>,
    pub wind_south_q: Vec<i8>,
}

/// Twelve monthly values of one place. `temp_c`'s datum is the producer's
/// (sea level from `normals_at` / `cell_normals`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonthlyNormals {
    pub temp_c: [f32; CLIMATE_MONTHS],
    pub diurnal_c: [f32; CLIMATE_MONTHS],
    pub precip_mm: [f32; CLIMATE_MONTHS],
    pub wet_day: [f32; CLIMATE_MONTHS],
    /// `[east, south]` m/s.
    pub wind_ms: [[f32; 2]; CLIMATE_MONTHS],
}

impl MonthlyClimate {
    pub fn cells(&self) -> usize;                                   // nx * nz
    pub fn index(&self, month: usize, cx: i64, cz: i64) -> usize;   // x: rem_euclid(nx) when wrap_x, else clamped; z clamped
    pub fn validate(&self) -> Result<(), String>;
    pub fn cell_normals(&self, cx: i64, cz: i64) -> MonthlyNormals; // one cell, no interpolation
    pub fn normals_at(&self, ax_m: f64, az_m: f64) -> MonthlyNormals; // map metres, bilinear between cell centres
    pub fn resident_bytes(&self) -> usize;                          // the planes' heap bytes
}

impl MonthlyNormals {
    pub fn warmest_month(&self) -> usize;   // first index of the largest temp_c
    pub fn coldest_month(&self) -> usize;   // first index of the smallest
    pub fn wettest_month(&self) -> usize;   // first index of the largest precip_mm
    pub fn driest_month(&self) -> usize;
    pub fn annual_mean_temp_c(&self) -> f32; // months 0..12 added in order, then / 12.0
    pub fn annual_precip_mm(&self) -> f32;   // months 0..12 added in order
    pub fn lowered_by(self, delta_c: f32) -> MonthlyNormals; // every temp_c - delta_c; nothing else changes
}
```

- `validate`: `nx ≥ 1`, `nz ≥ 1`, `cell_m` finite and `> 0`; the plane length is
  `12usize.checked_mul(nx as usize).and_then(|n| n.checked_mul(nz as usize))` (`usize` is 32 bits in the browser:
  an overflow is an `Err` that says so, never a wrap); every plane exactly that long; the error text names the
  plane. Every bit pattern of a plane is a legal value.
- `normals_at` is `ForgedWorld::bilinear_cells`'s arithmetic, on the climate grid: `u = ax_m / cell_m − 0.5`,
  `v = az_m / cell_m − 0.5`, `iu = floor(u)`, `iv = floor(v)`, `tu = u − iu`, `tv = v − iv` (all `f64`); a cell's
  value is `raw as f64 / DENOMINATOR`; `row(iz) = at(iu, iz) + tu · (at(iu + 1, iz) − at(iu, iz))`; result
  `row(iv) + tv · (row(iv + 1) − row(iv))`, cast to `f32` last. Cells through `index`. No `mul_add`, no
  transcendental. At a cell centre it returns `cell_normals` of that cell bit for bit (`tu = tv = 0`).
- Module doc states: plain data, never hashed, not `#[rollback]` (Decision 9: weather is a pure function of it);
  the planes are public and `index` addresses them — the integer read; x wraps BY INDEX, so a caller that needs a
  point and its image one world-width away to read bit-identically folds x first (`ForgedWorld` does); month
  interpolation is the caller's.

- [ ] **Step 1: Temporary tests** (a `#[cfg(test)] mod tmp` in the new file; `just test-one game "--lib monthly_climate::"`), on a hand-built 4 × 2 grid of 256 m cells whose month-`m` temperature of cell `(cx, cz)` is `100·m + 10·cz + cx` centi-degrees:
  - `validate` is `Ok`; a plane one value short is `Err` naming it; `nx = nz = u32::MAX` is `Err` (no panic);
  - `cell_normals(1, 0).temp_c[2]` is exactly `2.01` as `f32` (`201 / 100`);
  - `normals_at(128.0, 128.0)` (the centre of cell (0, 0)) equals `cell_normals(0, 0)` bit for bit;
  - `normals_at(256.0, 128.0)` is the mean of cells (0, 0) and (1, 0) in every field;
  - with `wrap_x`: `normals_at(0.0, 128.0)` is the mean of cells (3, 0) and (0, 0); `normals_at(x, z)`,
    `normals_at(x + 1024.0, z)` and `normals_at(x − 2048.0, z)` are bit-identical for 64 points `x = k + 0.5`
    (dyadic values: the general case is the caller's fold);
  - without `wrap_x`: `normals_at(0.0, 128.0) == cell_normals(0, 0)`;
  - `normals_at(300.0, −500.0)` equals `normals_at(300.0, 0.0)` (north clamp), likewise south;
  - `index(3, −1, 5)` is month 3, column 3, row 1 when wrapping;
  - derived: for `temp_c = [5, 9, 9, 1, …]` `warmest_month() == 1` (first of a tie), `coldest_month()` the first
    smallest; `annual_precip_mm` of twelve `10.1`s is the in-order `f32` sum; `lowered_by(6.5)` lowers every
    temperature and leaves the other four fields bit-identical;
  - bincode round trip: `deserialize(serialize(c)) == c`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify, in this order** — the temporary tests pass; DELETE them; `just smoke`; `just determinism-wasm` (the module compiles for wasm32 and no pinned hash moves — if one does, BLOCKED).
- [ ] **Step 4: Commit** `feat(game): MonthlyClimate — the monthly-normals container and its exact sampler`.

### Task 2: The placeholder seasonal climate in the forge

**Files:**
- Modify: `crates/worldgen/src/forge/climate.rs` (new items below; nothing existing changes)
- Modify: `crates/worldgen/src/forge/mod.rs` (`forge_with` also panics on `climate::check_climate_grid(p)`, beside `check_wrap` — with the TOP-LEVEL `p`, never a level's `lp`)
- Modify: `crates/worldgen/examples/forge_proto/report.rs` (appended lines; `run`'s seam summary gains two fields)
- Modify: `justfile` (the comment above `forge-report` lists the new lines)

**Interfaces — consumes** (verified at `be8ab249`): `climate::Climate { temperature_c, precip_mm, latitude_deg }`, `climate::meander_deg(x_m: f64, z_m: f64, p: &ForgeParams) -> f64`, `climate::sea_level_temperature_c(lat_deg: f64, p) -> f64`, `climate::westerly_weight(lat_deg: f64, p) -> f64`, `climate::metres_per_degree(height_m: f64) -> f64`, `climate::latitudes(nz) -> Vec<f32>`, `habitat::coast_distance_by(nx, nz, cell_m, wrap_x, is_sea) -> Vec<f32>`, `par::map_range`, `World { nx, nz, dx, h, ocean, wrap_x, edges, … }`, `report.rs`'s `seam_steps(nx, nz, step, stat)` / `p99` / `run`'s `for field in ["height", "precip", "biome"]`, Task 1.

**Interfaces — produces** (in `forge/climate.rs`):

```rust
pub const CLIMATE_CELL_M: f64 = 256.0;
pub const CLIMATE_MIN_ROWS: usize = 96;
/// Fine cells per climate cell per axis (the plan's rule): max(1, round(CLIMATE_CELL_M / cell_m)),
/// halved while above 1 and nz / s < CLIMATE_MIN_ROWS.
pub fn climate_stride(cell_m: f64, nz: usize) -> usize;
/// Err when nx or nz is not a whole number of strides:
/// "this world's sides are not whole numbers of its {c} m climate cells ({w} x {h} m)".
pub fn check_climate_grid(p: &ForgeParams) -> Result<(), String>;
/// 90 at the north edge, 0 in the middle, -90 at the south edge (`latitudes`' line, at any z).
pub fn latitude_at(z_m: f64, height_m: f64) -> f64;

/// The monthly family before quantisation: six planes of 12 * nx * nz, month-major (the plan's vocabulary).
#[derive(Clone, Debug, PartialEq)]
pub struct MonthlyFields {
    pub nx: usize,
    pub nz: usize,
    pub cell_m: f64,
    pub temp_sea_c: Vec<f32>,
    pub diurnal_c: Vec<f32>,
    pub precip_mm: Vec<f32>,
    pub wet_day: Vec<f32>,
    pub wind_east_ms: Vec<f32>,
    pub wind_south_ms: Vec<f32>,
}
pub fn monthly_placeholder(w: &World, c: &Climate, p: &ForgeParams) -> MonthlyFields;
impl MonthlyFields {
    /// round(value * DENOMINATOR) in f64, saturating at each type's bounds (game's CLIMATE_*_PER_* constants).
    pub fn quantise(&self, wrap_x: bool) -> game::MonthlyClimate;
    /// How many values `quantise` saturates.
    pub fn clamped_values(&self) -> usize;
}
```

**The placeholder** (named constants, all in `climate.rs`, each with a doc line saying it is Phase 1b's placeholder and Phase 3 replaces it):
`SEASON_AMP_MARITIME_C = 7.0`, `SEASON_AMP_CONTINENTAL_C = 25.0`, `CONTINENTALITY_HALF_DEG = 8.0`,
`SEASON_LAG_LAND_MONTHS = 1.0`, `SEASON_LAG_SEA_MONTHS = 2.0`, `DIURNAL_SEA_C = 4.0`, `DIURNAL_CONTINENT_C = 6.0`,
`DIURNAL_DRY_C = 6.0`, `DIURNAL_DRY_REFERENCE_MM = 1500.0`, `PRECIP_SEASON_SHARE = 0.5`,
`PRECIP_SEASON_FULL_DEG = 15.0`, `WET_DAY_MM_COOL = 5.0`, `WET_DAY_MM_HOT = 14.0`, `WET_DAY_COOL_C = 0.0`,
`WET_DAY_HOT_C = 25.0`, `WET_DAY_MAX = 0.9`, `WIND_PLACEHOLDER_MS = 6.0`.

With `s = climate_stride(w.dx, w.nz)`, `C = w.dx · s`, `H = w.nz · w.dx`, `cnx = w.nx / s`, `cnz = w.nz / s`, for climate cell `(cx, cz)` covering fine cells `x ∈ [cx·s, (cx+1)·s)`, `z ∈ [cz·s, (cz+1)·s)`:

- `land` = the share of the cell's fine cells that are not `w.ocean`; the cell is SEA when `land < 0.5`;
- centre `(xc, zc) = ((cx + 0.5)·C, (cz + 0.5)·C)`; `lat = clamp(latitude_at(zc, H) + meander_deg(xc, zc, p), −90, 90)` (signed: north positive);
- annual sea-level mean `T = sea_level_temperature_c(lat, p)`;
- continentality `k = d / (d + CONTINENTALITY_HALF_DEG · metres_per_degree(H))`, `d` = the cell's distance to the sea ON THE CLIMATE GRID: `coast_distance_by(cnx, cnz, C, w.wrap_x, |c| sea[c])` (0 on a sea cell, so `k = 0` over open sea and `k → 1` deep inland; Ruling 16 — do not reuse `ForgedWorld`'s fine `coast_dist_m`, which is from `height <= 0`, another mask);
- `phase(m, lag) = cos(2π · (m + 0.5 − (SOLSTICE_NORTH_SUMMER_MONTHS + lag)) / 12)` — its twelve values sum to 0 (it is the cosine AT mid-month, 1.1 % above the month's true mean of a cosine: say so in its doc — a placeholder nicety, Phase 3 stores real means);
- `lag = SEASON_LAG_SEA_MONTHS + (SEASON_LAG_LAND_MONTHS − SEASON_LAG_SEA_MONTHS) · k`;
- **temperature** `temp[m] = T + sin(lat in radians) · (SEASON_AMP_MARITIME_C + (SEASON_AMP_CONTINENTAL_C − SEASON_AMP_MARITIME_C) · k) · phase(m, lag)` — `sin(lat)` is signed: zero at the equator, opposite in the two hemispheres, full at the poles; the annual mean stays `T`;
- annual precipitation of the cell `P` = the mean of `c.precip_mm` over its fine cells (sea cells included);
- **precipitation** `precip[m] = P / 12 · (1 + PRECIP_SEASON_SHARE · clamp(lat / PRECIP_SEASON_FULL_DEG, −1, 1) · phase(m, SEASON_LAG_LAND_MONTHS))` — wetter in the local summer, no season at the equator; the twelve sum to `P`;
- **diurnal range** (the same in every month) `DIURNAL_SEA_C + DIURNAL_CONTINENT_C · k + DIURNAL_DRY_C · land · clamp(1 − P / DIURNAL_DRY_REFERENCE_MM, 0, 1)`;
- **wet-day fraction** `clamp(precip[m] / (days · amount), 0, WET_DAY_MAX)`, `days = game::history::calendar::DAYS_PER_MONTH as f64` (28, named, not a literal), `amount = WET_DAY_MM_COOL + (WET_DAY_MM_HOT − WET_DAY_MM_COOL) · clamp((temp[m] − WET_DAY_COOL_C) / (WET_DAY_HOT_C − WET_DAY_COOL_C), 0, 1)`;
- **wind** (the same in every month) east `(2 · westerly_weight(lat, p) − 1) · WIND_PLACEHOLDER_MS`, south `0`.

Rows may be computed in parallel (`par::map_range`), each cell from its own inputs only, so the result does not depend on the thread count. `monthly_placeholder` reads its arguments and changes nothing. Its transient memory is six `f32` planes (288 B per climate cell: 88 MB at Large).

**Report lines appended** (bands by ROW latitude `latitude_at(zc, H)`, a band = the climate rows within 7.5° of its centre, the mean over all their cells; the monthly lines from `monthly_placeholder(&forged.world, &forged.climate, p)`):
- `artifact_rasters_crc`: 8 hex digits, crc32 (one `crc32fast::Hasher`, fed in this order) of `bincode::serialize` of each of the artifact's `height`, `uplifted`, `bedding`, `temperature_c`, `precip_mm`, `drainage`, `volcanism`, `salinity`, `biome`, `savagery`, `region`, `regions`, `rivers`, `lakes`, `spawn`, `edges`, `column`, `rock_names`, `volcanoes` — a fixed field list of `forged.artifact(p)` (the one `report` already builds), never the whole struct, so a field added later does not move it. Add the key to `NOT_NUMBERS`;
- `climate_grid`: `{cnx} × {cnz} cells of {C} m, stride {s}`;
- `climate_clamped_values`: `clamped_values()` (expected 0);
- for each band in `75N, 45N, 15N, 0, 15S, 45S, 75S`: `sea_temp_jan_c.<band>` (month `NORTH_MIDWINTER_MONTH`), `sea_temp_jul_c.<band>` (month `NORTH_MIDSUMMER_MONTH`), `seasonal_swing_c.<band>` (jul − jan), each `{:.2}`;
- `monthly_temp_mean_err_c`: the largest `|mean of the 12 temp − T|` over all cells (`{:.2e}`; expected below 1e-4);
- `monthly_precip_sum_err_mm`: the largest `|Σ precip − P|` (`{:.2e}`);
- the seam on the climate grid, with the existing `seam_steps(cnx, cnz, …, p99)`: `seam_step_ratio_monthly_temp`, `seam_step_rank_monthly_temp`, `seam_ok_monthly_temp`, and the same three `…_monthly_precip` (month `NORTH_MIDSUMMER_MONTH` of each plane); `run`'s summary loop over `["height", "precip", "biome"]` gains `"monthly_temp"` and `"monthly_precip"` (`seam_seeds_not_ok_*`, `seam_pass_*`).

- [ ] **Step 0: The first baseline, BEFORE any change.** Confirm `report.rs` holds no line marked `TEMP` (none today; if one is back, BLOCKED). Run `just forge-report --size dev --cell 32 --seeds 2 > <ledger dir>/baseline-32.txt` and `just forge-report --size dev --cell 64 --seeds 8 > <ledger dir>/baseline-64.txt`; run the gate (`just test-one worldgen "--test it -- --include-ignored forge_lib::"`) and quote its pass. The controller notes both files in the ledger under "Baseline report".
- [ ] **Step 1: Temporary tests** (`#[cfg(test)] mod tmp` in `climate.rs`; `just test-one worldgen "--lib forge::climate::tmp"`):
  - `climate_stride`: with `nz` of the Dev size (8,192 m): 8 m → 8, 16 → 4, 32 → 2, 64 → 1, 128 → 1; Tiny (12,800 m): 16 → 8, 32 → 4; Small, Medium, Large: 16 → 16, 32 → 8; a 512 m cell → 1;
  - `check_climate_grid`: `Ok` for every `WORLD_SIZES` entry and `DEV_WORLD_SIZE` at 16, 32 and 64 m, and at every `PREVIEW_CELL_CHOICES` cell; `Err` containing `climate cells` for 51,200 × 25,472 m at 32 m (796 rows: stride 8 does not divide it);
  - `latitude_at(0, H) == 90`, `latitude_at(H / 2, H) == 0`, `latitude_at(H, H) == −90`; at a row centre it equals `latitudes(nz)[z]` within 1e-4;
  - on an all-ocean wrapped `World` of 16,384 × 8,192 / 64 m with `meander_amplitude_deg = 0` (hand-built: `ocean` all true, `h` all −100; a `Climate` from `compute`): every cell has `k = 0`; in the northern rows month 4 or 5 is the warmest (lag 2) and the month six later the coldest; the same row mirrored south has them swapped; the rows nearest the equator have `max − min < 1.0`; `mean of 12 == T` within 1e-4; every wet-day value is in `[0, 0.9]`; wind is negative (from the east) at 10°, positive at 45°, negative at 75°;
  - the same grid with a hand-set land block (ocean false, `h` 100 m) in the middle of the northern half, wide enough that its centre is 20° from the sea: there `k > 0.6`, the January–July range is more than twice the open sea's at that latitude, and the warmest month is 3 or 4 (lag about 1);
  - `quantise(true)` then `cell_normals`: every field within half a step of the float plane; `clamped_values() == 0`; a plane with one temperature of 400 °C counts 1 and stores `i16::MAX`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-report --size dev --cell 64 --seeds 8`: quote the new lines of the first seed and the summary. Judged: `seasonal_swing_c` positive for `75N`, `45N`, `15N`, negative for the three southern bands, `|seasonal_swing_c.0| ≤ 1.0` (the likeliest miss: the band cancels only to first order where land sits on one side of the equator), `seasonal_swing_c.45N ≥ 5.0`, `seasonal_swing_c.45S ≤ −5.0` on all 8 seeds; `climate_grid: 256 × 128 cells of 64 m, stride 1`; `climate_clamped_values: 0`; `seam_pass_monthly_temp: yes`, `seam_pass_monthly_precip: yes`. A miss is tuned here (the constants above) or ruled, never left unquoted. Then the safety net: gate + both diffs against `baseline-*` (no `<` line).
- [ ] **Step 4: Verify, in this order** — the temporary tests pass; DELETE them; `just smoke`; `just web-check`.
- [ ] **Step 5: The second baseline** — save the two report runs of the final code as `baseline2-32.txt` and `baseline2-64.txt` in the ledger directory; quote their `artifact_rasters_crc` lines in the report (the controller copies them into the ledger: Tasks 3–5 must reproduce them).
- [ ] **Step 6: Commit** `feat(forge): a placeholder seasonal climate on the climate grid (not stored yet)`.

### Task 3: The container in the world file and the tiled store — the one format bump

**At dispatch the controller tells the owner** (who edits the client concurrently): once this task's commit lands, `worlds/debug-world`, every `just play` world and every dev store refuse to load until re-forged (`just run --reset`, `just pg-reset`).

**Files:**
- Modify: `crates/worldgen/src/forged.rs`: `ARTIFACT_VERSION` 9 → 10 (extend its doc: "v10: `climate`, the monthly-normals family"); `WorldArtifact` gains `pub climate: game::MonthlyClimate` as its LAST field (one construction site: `Forged::artifact`); `to_bytes` / `from_bytes` (the container's delta codec, below); `WorldArtifact::validate`; `Rasters` (new `climate()`); `ForgedWorld` (new `monthly_climate()`; `from_tiles_with_budget` hands the container to the cache)
- Modify: `crates/worldgen/src/forged_tiles.rs`: `TILED_FORMAT_VERSION` 2 → 3 (doc: "v3: `ForgedMeta::climate`"); `ForgedMeta` gains `pub climate: game::MonthlyClimate` (last field); `ForgedMeta::validate`; `split`; `encode_meta` / `decode_meta`; `TileCache` (holds the container; `TileCache::new`'s signature)
- Modify: `crates/worldgen/src/forge/mod.rs` (`Forged::artifact` fills `climate`)
- Modify: `crates/worldgen/examples/forge_proto/report.rs` (size lines), `crates/worldgen/examples/forge_proto/main.rs` (the forge-and-write path prints one line `artifact: {:.2} s` — the wall time of its `forged.artifact(p)` call — beside its existing "wrote …" line: Task 6 reads it)
- Modify: `CLAUDE.md` (Worldgen paragraph: the monthly family, where it lives, the versions; stage only your hunks), `TODO.md` (every dev world, the public web world, dev Postgres worlds, the twin and orion need a re-forge before they run this build; +28 MiB resident per host / coordinator pod at Large, beside the pods' memory requests), `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` (Decision 7: the cell rule with its 96-row floor, the six planes, "always resident, in `ForgedMeta`", the type in `game`) and `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Phase 3's bullet: it FILLS the stored wind, it does not add it, and it averages its planet grid onto the container's grid; Phase 1b's "Sampler API in `worldgen`": the container type is `game::MonthlyClimate`) — the program's rule: amend the design in the commit that departs from it
- No change expected in `crates/world-store/` (it encodes the `ForgedMeta` it is given and compares version numbers), `crates/worldgen/src/world_store.rs` (`META_FORMAT` stays 2) or `crates/worldgen/src/tile_map.rs` (`WORLD_MAP_VERSION` stays 3). If one is needed, BLOCKED.

**Interfaces — produces:**
- `WorldArtifact { …, pub climate: game::MonthlyClimate }`; `ForgedMeta { format_version, tile_cells, globals, lakes, pub climate: game::MonthlyClimate }`.
- The container's wire form, used by BOTH `WorldArtifact::to_bytes` and `encode_meta` — one pair of crate-private functions in `forged.rs`, `climate_to_wire(&mut MonthlyClimate)` / `climate_from_wire(&mut MonthlyClimate)`, both `#[cfg(not(target_arch = "wasm32"))]` like their four callers (else the browser build warns of dead code): each of the 12 month planes of each of the six fields is row-delta encoded on its own, rows of the CLIMATE `nx` (every value but a row's first becomes its wrapping difference to its left neighbour — `delta_rows` / `delta_rows_u8`'s rule, for `i16`, `u16`, `u8` and `i8`); then bincode and zstd with the rest of the struct, as today. Both walk a plane by `chunks_mut(nx.max(1))` and never index past it, so a plane of the wrong length (the temporary test's truncated artifact) encodes and decodes without a panic and is refused by the size check.
- `fn validate_climate(c: &game::MonthlyClimate, width_m: f64, height_m: f64, edges: &[EdgeKind; 4]) -> Result<(), ArtifactError>` (crate-private, all targets): `c.validate()`, `f64::from(c.nx) * c.cell_m == width_m`, `f64::from(c.nz) * c.cell_m == height_m`, `c.wrap_x == (edges[1] == EdgeKind::Wrap)`; every error is `ArtifactError::Format("monthly climate: …")`. Called by `WorldArtifact::validate` (which `from_bytes` already runs BEFORE its un-delta loop) and by `ForgedMeta::validate`; and **`decode_meta` calls it itself, after bincode and BEFORE `climate_from_wire`** (`decode_meta` never calls `ForgedMeta::validate` — that happens later, in `from_tiles_with_budget`).
- `split`: its `check = ForgedMeta { … }` carries `climate: artifact.climate.clone()` BEFORE `check.validate()` (the validation now needs it), and the returned meta keeps that same container.
- `Forged::artifact`: `climate: climate::monthly_placeholder(w, &self.climate, p).quantise(w.wrap_x)`.
- One resident copy in a tiled world: `TileCache::new(globals: &WorldGlobals, tile_cells: u32, climate: game::MonthlyClimate, loader: Arc<dyn TileLoader>, budget_bytes: usize) -> TileCache` (today `new(&ForgedMeta, loader, budget)`; its only caller is `ForgedWorld::from_tiles_with_budget`, which destructures the meta it owns and MOVES the container in, then moves `globals` on as today). `pub(crate) fn Rasters::climate(&self) -> &game::MonthlyClimate` (whole: the artifact's; tiled: the cache's); `pub fn ForgedWorld::monthly_climate(&self) -> &game::MonthlyClimate`. `Tile`, `Tile::decoded_bytes` and the cache budget are unchanged.
- **How an older world is refused** (no new code beyond the two constants — verify, do not rebuild):
  - a world FILE: `WorldArtifact::from_bytes` → `ArtifactError::OldVersion { found: 9 }` → "world file version 9 is older than 10: re-forge this world";
  - the MENU list: `worldgen::world_store::check_terrain` → `check_version` → "needs re-forge: …" — from the file's header when `terrain.world` exists, from `world.json`'s `artifact_version` when only the store file does;
  - a SQLite or Postgres STORE: `world_store::load_forged` compares the `forged_meta.format_version` COLUMN (2 ≠ 3) before it decodes anything → `StoreError::OtherFormat { kind: FormatKind::Tiles { archived } }`; `archived` is false because `archive_is_current` sees a v9 archive, so the message ends "it keeps no archived forge file this build can re-tile from, so re-forge the world", `kind.retileable()` is false and `server::world_file::ensure_world` returns the error (no re-tile, no forge over saved regions). One function for both backends (`StoreHandle::forged_head`): Postgres is argued from that, not run (Ruling 17);
  - `worldctl retile` on such a store fails on another path with the same instruction: `retile` → `write_tiled(archive)` → `ForgedWorld::from_bytes` → `StoreError::Corrupt("the forged world to store does not load: world file version 9 is older than 10: re-forge this world")`.
- `--report` gains: `world_file_bytes` (`artifact.to_bytes().len()`), `climate_bytes_raw` (`cells · CLIMATE_BYTES_PER_CELL`), `climate_bytes_packed` (the container alone through `climate_to_wire` + bincode + zstd at the file's level — one `#[cfg(not(target_arch = "wasm32"))] pub fn climate_packed_len(c: &game::MonthlyClimate) -> usize` in `forged.rs` for it), `climate_resident_bytes` (`resident_bytes()`).

- [ ] **Step 0: Worlds of the OLD formats, made with the build BEFORE this task's change** (kept under `target/world-forge/`, git-ignored): `just forge-proto --size dev --cell 64 --out target/world-forge/pre-1b` (a v9 `.world`) and `just region_blocks=64 worldctl forge --db target/world-forge/pre-1b.sqlite --size dev --cell 64` (a tiles-v2 store with a v9 archive; region 64, because the store records its region size and the temporary test below builds at 64). Quote both commands' last lines. `region_blocks=64` rebuilds the release `worldgen` / `world-store` units at 64 (they are shared with the owner's 256 builds): run this command and Step 3's two `worldctl` commands in as few switches as the work allows, and say in the report when each ran.
- [ ] **Step 1: Temporary tests:**
  - worldgen (`crates/worldgen/tests/forge_lib.rs`, one dev world at 64 m forged once): `WorldArtifact::from_bytes(&a.to_bytes()) == a` (the container included); `a.climate` has `nx == 256`, `nz == 128`, `cell_m == 64.0`, `wrap_x`; `ForgedWorld::from_bytes(..).monthly_climate() == &a.climate`; `split(&world, TILE_CELLS)` then `decode_meta(&encode_meta(&meta)) == meta`, and a `ForgedWorld::from_tiles` over `MemTiles` of the encoded tiles has `monthly_climate() == &a.climate`; an artifact whose `climate.nx` is halved (planes resized to match) fails `validate` with text containing `monthly climate`; one whose `climate.wrap_x` is false fails likewise; an artifact with one plane truncated by a value: `to_bytes` does not panic and `from_bytes` of it is `Err` containing `monthly climate`; the same truncation in a meta: `decode_meta(&encode_meta(..))` is `Err` (no panic);
  - worldgen: bytes with magic `VXWF` and version 9 → `ArtifactError::OldVersion` whose text contains `re-forge`; `ForgedWorld::load(&forged::from_repo_root(Path::new("target/world-forge/pre-1b.world")))` (tests run with the crate as their directory) → the same;
  - worldgen `world_store`: a world directory holding `world.json` (a `WorldMeta` with `artifact_version: 9`, serialized as `create` writes it), an empty file under the store's file name and NO `terrain.world` (with the file present `check_terrain` reads the file's header and never consults `world.json`; so do not build the directory with `create`) is listed by `list` as unusable with text containing `re-forge`; a second directory with only a v9 `terrain.world` (a copy of `pre-1b.world`) likewise;
  - world-store (a temp test in `crates/world-store/src/forged.rs`): `load_forged` on a handle opened on a COPY of `target/world-forge/pre-1b.sqlite` is `Err(StoreError::OtherFormat { kind: FormatKind::Tiles { archived: false }, found: 2 })` and its `Display` contains `re-forge`. SQLite only (Ruling 17).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just world-map --world target/world-forge/pre-1b.world --tiles --out target/world-forge/pre-1b-map` stops with the `OldVersion` text (the tool panics with the message: that is its refusal — quote it) and `just region_blocks=64 worldctl retile --db target/world-forge/pre-1b.sqlite` fails with the `Corrupt … re-forge this world` text: quote both. `just forge-report --size dev --cell 32`: quote `world_file_bytes`, `climate_bytes_raw` (3145728), `climate_bytes_packed`, `climate_resident_bytes`. `just region_blocks=64 worldctl forge --db target/world-forge/post-1b.sqlite --size dev --cell 64` succeeds, and a new store loads back (the temporary test's `load_forged` on a copy of it is `Ok`, with `monthly_climate().nx == 256`). `just forge-proto --size dev --cell 32 --out target/world-forge/post-1b` prints its `artifact:` line. The safety net: gate + both diffs against `baseline2-*`, `artifact_rasters_crc` equal on every seed.
- [ ] **Step 4: Verify, in this order** — the temporary tests pass; `just test-one worldgen "--lib -- forged:: forged_tiles:: world_store::"`; DELETE the temporary tests; `just smoke`; `just web-check`.
- [ ] **Step 5: Commit** `feat(worldgen)!: the monthly climate family in the world file and the tiled store (artifact v10, tiles v3)`. The message says every existing world must be re-forged (`just run --reset`, `just pg-reset`).

### Task 4: The world-position sampler API

**Files:**
- Modify: `crates/worldgen/src/forged.rs` (`ForgedWorld`: the functions below; one free function)
- Modify: `crates/worldgen/examples/forge_proto/report.rs` (round-trip, conservation, alias and gap lines)
- Modify: `CLAUDE.md` (Worldgen paragraph: the API and its two datums; stage only your hunks)

**Interfaces — produces:**

```rust
/// A place's monthly normals at the ground from its sea-level normals: every
/// temperature lowered by `lapse_c_per_m * ground_m` (`ground_m` is already
/// floored at 0 by the caller), and the twelve precipitations replaced by
/// `annual_precip_mm` split in the normals' own monthly shares (an equal
/// twelfth each when they sum to 0). Diurnal range, wet-day fraction and wind
/// are unchanged. Exact arithmetic, f64 inside.
pub fn ground_normals(sea: game::MonthlyNormals, ground_m: f64, annual_precip_mm: f64, lapse_c_per_m: f64)
    -> game::MonthlyNormals;

impl ForgedWorld {
    /// Cooling per metre of height the world was forged with (deg C / m): the existing private field's value.
    pub fn lapse_per_m(&self) -> f64;
    /// The annual rasters at engine metres, bilinear: (mean annual temperature AT THE GROUND in deg C,
    /// annual precipitation in mm/yr) — what `column_climate` and the habitat read.
    pub fn annual_climate(&self, x_m: f64, z_m: f64) -> (f64, f64);
    /// The climate grid's normals at engine metres: SEA-LEVEL temperature, the grid's own precipitation.
    /// Bilinear between climate cells; x folds on a world that wraps; clamped north and south. Reads no tile.
    pub fn monthly_normals(&self, x_m: f64, z_m: f64) -> game::MonthlyNormals;
    /// The normals AT THE GROUND at engine metres. Reads two fine rasters: on a tiled world it can load (or,
    /// on a store error, fail on) a tile unless the caller holds a prefetch — only `monthly_normals` is load-free.
    pub fn monthly_at_ground(&self, x_m: f64, z_m: f64) -> game::MonthlyNormals;
    /// `monthly_at_ground` at MAP metres (the function both it and a forge-side caller share).
    pub fn monthly_at_ground_map(&self, ax_m: f64, az_m: f64) -> game::MonthlyNormals;
}
```

- `lapse_per_m` is named apart from the private FIELD `lapse_c_per_m` on purpose (a method of the field's name compiles but reads ambiguously); it returns that field.
- `annual_climate`: `(self.bilinear_q(QRasterId::TemperatureC, ax, az), self.bilinear_q(QRasterId::PrecipMm, ax, az))` with `(ax, az) = self.to_artifact(x_m, z_m)` — the public door to `bilinear_q` (`pub(crate)`) that the report and the temporary tests need.
- `monthly_normals`: `let (ax, az) = self.to_artifact(x_m, z_m); self.monthly_climate().normals_at(ax, az)`.
- `monthly_at_ground_map(ax, az)`: `ground_normals(self.monthly_climate().normals_at(ax, az), ground, annual, self.lapse_c_per_m)` with `ground = self.bilinear_cells(ax, az, |i| f64::from(self.rasters.q(QRasterId::Height, i)).max(0.0))` (each cell floored at 0 BEFORE interpolating — `snow_line_m`'s read, Ruling 9) and `annual = self.bilinear_q(QRasterId::PrecipMm, ax, az)`. `monthly_at_ground(x_m, z_m)` = `monthly_at_ground_map` of `self.to_artifact(x_m, z_m)`.
- `ground_normals`' precipitation: `total` = the twelve `sea.precip_mm` added in month order as `f64`; `precip_mm[m] = (annual_precip_mm * (f64::from(sea.precip_mm[m]) / total)) as f32`, or `(annual_precip_mm / 12.0) as f32` when `total <= 0.0`. Temperature: `(f64::from(sea.temp_c[m]) - lapse_c_per_m * ground_m) as f32`.
- The derived values Phases 5 and 6 read are `game::MonthlyNormals`' methods on either result: `warmest_month`, `coldest_month`, `driest_month`, `wettest_month`, `annual_mean_temp_c`, `annual_precip_mm` (e.g. the driest month's precipitation at a place is `n.precip_mm[n.driest_month()]`).
- **For Phase 5** (the forge-side classifier works on `WorldArtifact` cells, with no `ForgedWorld`): the same numbers for fine cell `i` are `ground_normals(a.climate.normals_at(cx_m, cz_m), f64::from(a.height.get(i)).max(0.0), f64::from(a.precip_mm.get(i)), lapse)` with `(cx_m, cz_m)` the cell centre in MAP metres — bit-identical to `monthly_at_ground_map(cx_m, cz_m)`, because a bilinear read at a cell centre returns the cell. (Through ENGINE metres the identity holds only when the spawn offset round-trips exactly, which nothing guarantees — hence the map-metre function.) State this in `ground_normals`' doc; nothing calls it that way in this phase.
- **For Phase 6**: month INTERPOLATION is the caller's (Ruling 12); the per-column lapse above the bilinear ground and the engine-frame offset are Phase 6's (Rulings 9, 10). State both in the doc comment of `monthly_normals`.
- `--report` gains (one whole world from `ForgedWorld::from_bytes(&artifact.to_bytes())`, one tiled world from `split` → `encode_tile` / `encode_meta` → decode → `MemTiles`; sample points = every climate cell centre, plus a lattice of every 7th fine cell centre, plus the points 0.5 m either side of the seam in every 4th climate row; a size with more than 50,000 climate cells samples every k-th so about 50,000 remain):
  - `climate_roundtrip_max_err`: `temp {:.4} C, diurnal {:.3} C, precip {:.3} mm, wet {:.5}, wind {:.3} m/s` — the largest difference at climate cell centres between `monthly_placeholder`'s float planes and the whole world's `monthly_normals`; and `climate_roundtrip_ok: yes/no` (each within half its step plus 1e-4 of the step);
  - `climate_tiles_identical: yes/no` — `monthly_normals` and `monthly_at_ground` of the tiled world equal the whole world's bit for bit at every sample point;
  - `monthly_aliased_identical: yes/no` — both functions at `(x, z)` and at `(x + width_m, z)` are bit-identical at every sample point;
  - `monthly_precip_conserved_max_rel`: the largest `|annual_precip_mm() − A| / max(A, 1)` of `monthly_at_ground` over the sample points, `A` = `annual_climate(..).1` there (`{:.2e}`; judged ≤ 1e-5);
  - `monthly_annual_temp_gap_c`: `p99 {:.3}, max {:.3}` of `|monthly_at_ground(..).annual_mean_temp_c() − annual_climate(..).0|` over the fine lattice (Ruling 13: expected below 0.1; a p99 above 0.3 is a finding).

- [ ] **Step 1: Temporary tests** (`crates/worldgen/tests/forge_lib.rs`, one dev world at 64 m forged once, with `lapse_c_per_km = 6.5`):
  - at a climate cell centre `monthly_normals` equals `monthly_climate().cell_normals(cx, cz)`;
  - at the highest land cell's centre (height `h`): `monthly_at_ground` temperatures are `monthly_normals`' minus `0.0065 · h` within 1e-3, month for month; at the centre of a sea cell whose eight neighbours are sea the two temperatures are equal; at a point midway between a land cell of height `h > 0` and a sea-floor cell the temperature is `monthly_normals`' minus `0.0065 · h / 2` within 1e-3 (the sea floor's depth does not enter);
  - `monthly_at_ground(p).annual_precip_mm()` is within 1e-5 relative of `annual_climate(p).1` at 1,000 points (not cell centres); its monthly shares equal `monthly_normals(p)`'s shares within 1e-6;
  - `ground_normals` with all-zero precipitation normals and `annual = 120` gives twelve `10.0`s;
  - `monthly_at_ground(x, z)` and `(x + width, z)` and `(x − width, z)` are bit-identical at 256 block-centre points; `z` 500 m north of row 0 reads as row 0's edge;
  - for 64 fine cells `i`: `monthly_at_ground_map(centre of i)` equals the Phase 5 expression above on the artifact, bit for bit;
  - on a tiled world (`from_tiles_with_budget` with a budget that holds two tiles) `monthly_normals` loads NO tile (`tile_cache().unwrap().stats().loads` unchanged) and `monthly_at_ground` equals the whole world's.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-report --size dev --cell 64 --seeds 8` and `just forge-report --size dev --cell 32 --seeds 2`: quote the five new lines per seed. Judged: `climate_roundtrip_ok: yes`, `climate_tiles_identical: yes`, `monthly_aliased_identical: yes`, `monthly_precip_conserved_max_rel ≤ 1e-5`, `monthly_annual_temp_gap_c` p99 ≤ 0.3 on every seed. Then `just forge-report --size small --cell 32` (one seed, about a minute; the coarsest grid in degrees): the same lines, judged the same. The safety net: gate + both diffs against `baseline2-*`, `artifact_rasters_crc` equal on every seed.
- [ ] **Step 4: Verify, in this order** — the temporary tests pass; `just test-one worldgen "--lib -- forged:: forged_tiles::"`; DELETE the temporary tests; `just smoke`; `just web-check`.
- [ ] **Step 5: Commit** `feat(worldgen): monthly normals at a world position — sea level and at the ground`.

### Task 5: A month on the map tool

So the family can be LOOKED at before Phase 3 fills it with a model, and so the API is exercised on a loaded world file end to end.

**Files:**
- Modify: `crates/worldgen/examples/world_map.rs` (`ForgedLayer` gains `MonthlyTemperature` and `MonthlyRain`, named `monthly-temperature` and `monthly-rain`; a `--month <1..=12>` flag — 1-based, the calendar's month number, index `month − 1`; default 4 = index `NORTH_MIDSUMMER_MONTH`; `write_forged_layer` reads `world.monthly_at_ground(x, z)` per pixel; the output name — today built from `ForgedLayer::name()` — gains the month for these two layers; the usage text and the header comment)
- Modify: `justfile` (the comment above `world-map`)

**Interfaces:**
- Colours are the existing ramps — `tile_map::temperature_color(temp_c[m])` and `tile_map::precipitation_color(12 · precip_mm[m])` (a month shown at its yearly rate, so the annual layer's ramp applies); open sea dimmed as the annual climate layers dim it. Output `<out>-monthly-temperature-<MM>.png` / `<out>-monthly-rain-<MM>.png`. `--month` outside 1..=12, or given without one of the two layers, is a usage error.
- **The window is the whole world** for the two monthly layers unless `--span` / `--center` are given: the tool's defaults (8,192 m around the spawn) show only the middle latitudes of even the Dev planet. With neither flag, the span is the world's width and the centre is the middle of `world.artifact_bounds_engine_m()`.
- The tool prints, for these layers, the layer's value at five latitudes of the world's middle column, read at the ENGINE position of each latitude whatever the window: map `z = (90 − lat) / 180 · height_m` for `lat` in 75, 45, 0, −45, −75, map `x = width_m / 2`, converted with `artifact_bounds_engine_m()`.

- [ ] **Step 1: Implement** (no temporary test: the evidence is the picture and the printed numbers; the evidence run is also this task's compile check — `just smoke` does not build `worldgen`'s examples).
- [ ] **Step 2: Evidence** — `just forge-proto --size dev --cell 32 --out target/world-forge/monthly`, then `just world-map --world target/world-forge/monthly.world --layer monthly-temperature --month 4 --out target/world-forge/monthly` and the same with `--month 10`: quote the five printed values of each (75°N and 45°N warmer in month 4 than in month 10, 45°S and 75°S the reverse, the equator within 1 °C) and the PNG paths and pixel sizes (2:1, pole to pole); the same for `--layer monthly-rain`. Read the two temperature PNGs (the image reader) and say in one line each what they show. The safety net (the report's two diffs against `baseline2-*`; nothing in the library changed, so the gate is not re-run).
- [ ] **Step 3: Verify** — `just smoke`.
- [ ] **Step 4: Commit** `feat(worldgen): world-map layers for one month's temperature and rain`.

### Task 6: Measure every size; evidence; close

**Files:** `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` ("Measurements": a "Phase 1b" table), `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Status row 1b → DONE with the plan path and the commit range; Evidence: "Phase 1b"; Owner hand checks), this file ("As built": commits, rulings made by tasks, deferred minors), `TODO.md`, `CLAUDE.md` if a fact above is still stale; `crates/worldgen/src/forge/play.rs` (`FORGE_SECS_PER_MCELL`, `FORGE_COST_EXPONENT`, `FORGE_RAM_*`) ONLY if a measured row is more than 25 % off the estimate Phase 1a Task 19 refitted.

- [ ] **Step 1: Measure** (release, one at a time, nothing else of ours running): `just forge-bench 16384 8192 32`, then each of Tiny, Small and Medium at 16 m and at 32 m, and Large at 32 m. Large at 16 m only if `free -g` shows more than 20 GB available; otherwise record "not run: needs about 14 GB". For each: wall seconds and max RSS (the recipe's last line), the `.world` size (`ls -l target/forge-bench/w.world`), and **the artifact step's share** — the `artifact: … s` line Task 3 added (it now includes the placeholder: the coarse chamfer and six float planes) beside the whole run's wall time; note `scaling_cur_freq`.
- [ ] **Step 2: Sizes of the family** — for each size (at 32 m; one seed) `just forge-report --size <name> --cell 32`: `climate_grid`, `world_file_bytes`, `climate_bytes_raw`, `climate_bytes_packed`, `climate_resident_bytes` (Large at 32 m is the slowest: about four minutes).
- [ ] **Step 3: Record** the "Phase 1b" table in the design beside Phase 1a's rows: per size and cell — wall time, the artifact step's seconds, peak memory, world file size, and beside each the Phase 1a figure and the difference; per size — the climate grid, its degrees per cell, its raw, packed and resident bytes. A time or memory row more than 25 % above Phase 1a's is attributed (the artifact step, or not) in one line. No pass / fail line: the owner judges.
- [ ] **Step 4: Phase evidence** — `just forge-report --size dev --cell 32 --seeds 8` (every line) pasted into the program's Evidence section, judged against the Phase 1b row: per band, `sea_temp_jan_c`, `sea_temp_jul_c`, `seasonal_swing_c` (opposite signs in the two hemispheres, the zero band within 1.0 °C); the round-trip, tiles, alias, conservation, gap and seam lines; the world-file size per world size from Step 3; Task 5's PNG paths. State there that the quality gate was NOT re-recorded and passes untouched (quote the run), and that `artifact_rasters_crc` and both `baseline2-*` diffs are clean at the last commit.
- [ ] **Step 5: Whole suites, once:** `just test` in the foreground, then `just clippy`; quote the PASS / FAIL line per stage. A failure in this plan's files is fixed here; one outside them is recorded.
- [ ] **Step 6: Owner hand checks appended to the program:** every dev world needs `just run --reset` (and `just pg-reset` for the dev Postgres worlds); the twin and orion need a re-forge before they run this build; look at Task 5's two temperature PNGs.
- [ ] **Step 7: Commit** `docs: Phase 1b measurements and evidence`.

## Known limits (not in this plan)

- The values are a placeholder: a cosine by latitude and distance from the sea. No monsoon, no ocean currents, no seasonal shift of the wind bands, the same diurnal range and wind in every month. Phase 3 replaces every value; the format stays.
- The container is coarser than Phase 3's planet grid on Dev, Tiny, Small and Medium (1.4° to 1.9°, 0.92° on Medium, against 0.35° to 0.70°): Phase 3 averages down onto it, or raises `CLIMATE_MIN_ROWS` — a value, not a format (Ruling 1).
- Nothing in the game reads the family yet: biomes, habitat, snow and ice still read the annual rasters (Phases 5 and 6).
- The annual temperature raster is at the ground and the monthly family at sea level (Review Focus 1).
- NO client holds the family yet — not a remote client and not single-player's client either (the world lives in
  the spawned server; `crates/client` names only paths and version constants). The delta codec that makes it small
  (`climate_to_wire` / `climate_from_wire`, the row-delta wire helpers) is private to `worldgen` and compiled out on
  wasm32, so Phase 6 must move it into `game` or re-implement it there to give the family to a client (plain
  bincode is 28 MiB at Large).
- The in-game world map shows no monthly layer; only the `world_map` tool does.
- A world whose sides are not whole climate cells does not forge (Ruling 2).
- The old-format refusal of a Postgres store is argued from the shared code path, not run (Ruling 17).

## As built (2026-11-20)

Built in six tasks, each task-reviewed (Task 6's review: `task-6-review.md`), then one whole-plan review (one
reviewer on the most capable model: no Critical, 2 Important) and its one fix wave; the program's Status row is the
controller's. Ledger: `.superpowers/sdd/2026-11-20-monthly-climate-container-1b/progress.md` (git-ignored
workspace; the task reports, reviews and the safety-net baselines sit beside it). Evidence: the program's Evidence
section ("Phase 1b"); measurements: the design's Measurements ("Phase 1b").

### Commits per task

| Task | Commits |
|---|---|
| Plan | `a6fb06f9` |
| 1 `game::MonthlyClimate` and its exact sampler | `99fc8643`, `fc15f88f` (fix round 1: the sampler's index math saturates on far coordinates) |
| 2 The placeholder seasonal climate in the forge | `3c0b8856` |
| 3 The container in the world file and the tiled store (artifact v10, tiles v3) | `50ebc1ed` |
| 4 The world-position sampler API | `1fa81891` |
| 5 A month on the map tool | `c30da2a4` |
| 6 Measure, evidence, close | `057999fa` |
| Whole-plan review fix wave | `db5a8008` (the two crc lines, the summary, the gap line's wording), `40def97b` (the placeholder's assert, `bilinear_cells` total, the ground-read docs), and the docs commit that carries this section's closing text |

(`7b45b2c4`, between Tasks 3 and 4, is the Phase 3 plan's draft, not this plan's.)

### Rulings made during execution

The ledger's, verbatim, in order:

- Ruling: the monthly-vs-annual temperature gap near the poles is accepted as measured (Ruling 13's "p99 above 0.3 is a finding" is met at p99; the maxima 0.55–1.20 °C sit within half a climate cell of the north/south edges and within ~10° of a pole, where the placeholder's latitude + meander clamps at ±90°) — it is a property of the PLACEHOLDER at the polar rows, off the caps the gap is ≤ 0.08 °C, and Phase 3 replaces the values — costs: Phase 6, if started on the placeholder, sees up to ~1 °C inconsistency between annual and monthly temperature at the polar edge.
- Ruling: `monthly_annual_temp_gap_c` on Tiny at 32 m (p99 0.569, max 1.046 — above Ruling 13's 0.3 bar) is accepted for the same reason as the dev maxima: Tiny's climate cell is 128 m = 1.8° of latitude, so the sampler's clamp to the outermost climate row sits further from the polar edge and the placeholder's latitude clamp kinks there; the lapse cancels at the sampled points (Task 4's review), so it is the sea-level placeholder at the polar rows, not a datum defect — Phase 3 replaces the values and its plan states no latitude is clamped and the annual raster uses the same clamped read as the sampler — costs: until Phase 3 a Tiny world's monthly and annual temperatures disagree by up to ~1 °C within a cell of the poles.
- Ruling (Task 6's, in "As built"): the forge estimate is not refitted — Dev is 31% / 30% off but is not a menu size; every menu size is within 5.3% (time) and 21.5% (memory).

Made at the close (the controller's, after the whole-plan review):

- Ruling (1): the design's "raster lists stop being fixed arrays of five and five" is NOT built in this phase — the
  monthly family is a separate resident container, not a generalisation of the fine raster lists, and nothing needs
  variable-length lists yet — cost: a later phase that adds a FINE raster must still extend the fixed lists
  (`qrasters`, `byte_rasters`, `q_scale`) and bump the formats. The design's Artifact bullet is struck with a pointer
  here.
- Ruling (2): `artifact_rasters_crc` stays as recorded (its fixed list of 19 fields; every baseline depends on it);
  `artifact_climate_crc` (the stored monthly family: grid, `wrap_x`, the six planes) and `artifact_header_crc`
  (`seed`, `width_m`, `height_m`, `cell_m`, `nx`, `nz`, `edges`, the knob map; not the version, not `spawn`, which
  `forge_proto --spawn` rewrites after the build) are appended beside it as the report's last two lines, so Phases 2
  and 3 can prove "no stored byte changed" on all three — cost: none (appended lines; no existing line moved).
- Ruling (3): the workspace `.superpowers/sdd/2026-11-20-monthly-climate-container-1b/` is kept after the close —
  the program's Evidence cites its files — cost: a git-ignored directory on disk.

Made by the tasks (each task report's "Rulings", verbatim):

- Task 1 — Ruling: the midpoint check compares bit-exact against the mean of the `f64` decodes, not of the `f32` normals —
  the sampler interpolates in `f64` and casts last, so the `f32`-based mean double-rounds — cost if wrong: none
  (a temporary test).
- Task 2 — Ruling: the report's `monthly_temp_mean_err_c` / `monthly_precip_sum_err_mm` recompute T and P from the public
  functions instead of the placeholder exposing them — why: no extra public API beyond the brief's list, and it
  checks the formula independently — cost if wrong: a duplicate of ~10 lines to keep in step until Phase 3 deletes
  the placeholder.
- Task 2 — Ruling: a NaN value counts as clamped and stores 0 in `quantise` — why: `clamped_values` should flag any value
  the integers cannot carry — cost if wrong: none (none occur; count 0).
- Task 3 — Ruling: `delta_plane` / `undelta_plane` are one generic pair over a `fn(T, T) -> T` (`i16/u16/u8/i8::wrapping_*`)
  applied to the whole plane vector in rows of `nx`, rather than four copies of `delta_rows` per month — why: a month
  plane is exactly `nz` rows of `nx`, so row-chunking the vector IS per-month encoding, and the bytes are the same
  as `delta_rows`' rule; fewer functions to keep in step — cost if wrong: none (a format detail inside v10).
- Task 3 — Ruling: `MONTHLY_ROWS_PER_BATCH = 64` — why: bounds the placeholder's transient arrays (~12 MB at Large) while
  keeping enough parallel work per batch (64 rows of up to 784 cells); output identical (the baselines' monthly lines
  unchanged) — cost if wrong: a little parallelism at the batch boundaries (the placeholder takes milliseconds:
  `artifact: 0.04 s` at dev 32 m).
- Task 4 — Ruling: the report's fine lattice stride is `max(7, ceil(sqrt(cells / 50,000)))` — why: keeps the lines linear in
  ~50,000 points at every size (Large 16 m: stride 21) — cost if wrong: a coarser lattice at Medium/Large sees fewer
  coastal points (the lines are summaries; the temporary tests checked the properties exhaustively on dev).
- Task 4 — Ruling: `climate_roundtrip_max_err`'s wind is the max over both components — why: one number per field as the line's
  format has one wind slot — cost: none.
- Task 6 — Ruling: the forge's estimate (`forge::play`) is NOT refitted although Dev at 32 m is now 31 % slower and 30 %
  heavier than it predicts — why: Dev is not a menu size, the miss is a fixed per-world cost (writing the family's
  3 MiB through zstd level 19: +0.36 s, +36 MB, measured against the pre-1b binary) and every menu size stays within
  5.3 % on time and 21.5 % on memory — cost if wrong: the menu shows Tiny at 32 m as 125 MB where it takes 159 MB,
  far below the 12 GiB Play cap.
- Task 6 — Ruling: Large at 16 m was measured with exactly 21 GB available (the dispatch's bar: more than 20) — it
  peaked at 13.5 GB as in Phase 1a — cost: none.

### Tiny's gap: accepted by ruling

Ruling 13's bar ("a p99 above 0.3 °C on any size is a finding") is crossed on Tiny: `monthly_annual_temp_gap_c: p99
0.569, max 1.046` (`just forge-report --size tiny --cell 32`, default seed; Task 6). It is no longer open: the
ledger's second ruling above ACCEPTS it, and the whole-plan review confirmed the cause by the lattice arithmetic
(derived, not run): "the lattice takes every 7th fine row from row 0. The gap lives in the half climate cell beside
each pole (the sampler clamps z to its outermost row). Tiny at 32 m: 400 rows, half-cell = 2 rows, sampled rows 0 …
399 include BOTH row 0 and row 399 → 2 of 58 rows = 3.4 % of points, so the p99 falls inside the cap. Dev 32 m: row
0 only, 1 of 37 = 2.7 % → p99 0.24–0.30. Small / Medium / Large: row 0 only, 0.9 % / 0.6 % / 0.6 % → p99 outside
(0.148 / 0.062 / 0.051). Dev 64 m: stride 1, no half-cell → 0.050." The per-size figures at 32 m, p99 and max: Dev
0.24–0.30 (max 0.55 over 8 seeds), Tiny 0.569 (max 1.046), Small 0.148 (max 1.204), Medium 0.062 (max 0.664), Large
0.051 (max 0.342); Dev at 64 m 0.050.

### Measurement corrections (Task 6's review)

- Dev at 32 m, memory: +51 MB measured against the pre-1b binary, of which the zstd level-19 encode of the larger
  world-file body explains +36 MB; about 14 MB is not attributed. The time (+0.36 s) is fully explained by that
  encode.
- The third new run's peak memory is 127,656 KiB (the raw output), not 127,652 (that figure is another run's); the
  design's Measurements are corrected.
- The bench ran on the working tree at `c30da2a4`, which carried the owner's uncommitted `crates/game` edits
  (`crates/worldgen` itself clean), so it is not strictly `c30da2a4`; the design says so.

### Deferred minors still open (as the ledger words them)

- Task 1: minor (deferred): `read` takes `&dyn Fn` (dynamic dispatch, 5 planes × 12 months × 4 taps) → revisit in Phase 6 if `normals_at` runs per frame
- Task 1: minor (deferred): the doc's "saturates to the edge cell" for a far coordinate is exact only without wrap (with wrap: i64::MAX mod nx — arbitrary but deterministic) → final review triage
- Task 2: minor (deferred): the report's note "seed 0207's monthly precip `no` is inherited from the fine precip seam" holds at 64 m only → final review triage / Task 6 evidence wording
- Task 2: minor (deferred): `artifact_rasters_crc` (the plan's fixed list) leaves out the header fields the runtime also reads (seed, width_m, height_m, cell_m, nx, nz, knobs) — the `size:` and `seed:` report lines cover all but `knobs`; changing the crc now would invalidate baseline2 mid-phase → final review (add a `knobs_crc` line or extend the list at the phase's end, for Phase 2's safety net)
- Task 2: minor (deferred): `monthly_placeholder` peak ≈ 1.8× its planes → Task 3 was asked to fill row by row; `latitude_at` duplicates `latitudes`' formula → final review triage
- Task 3: minor (deferred): a meta's climate error reads "forged_meta: world file: monthly climate: …" (misleading prefix) → final review triage
- Task 3: minor (deferred): transient copies while WRITING a tiled world (~3–4 × 28 MiB at Large) and at `load_forged` (~2 ×) → Task 6 records it in the measurements
- Task 3: minor (deferred): the grid-span check is exact f64 equality (a hand-built artifact with a rounding-different cell is refused, not misread) → accept
- Task 4: minor (deferred): `monthly_annual_temp_gap_c` samples cell CENTRES, where the lapse cancels — it cannot detect a lapse / datum defect (Ruling 13's stated purpose) → final review fix wave (sample off-centre too, or state that the line checks the sea-level field only)
- Task 4: minor (deferred): summary prints `monthly_precip_conserved_max_rel` with float noise and loses the worst p99 of the gap across seeds → final review fix wave (cosmetic)
- Task 4: minor (deferred): `game::MonthlyNormals::lowered_by(f32)` vs `ground_normals` lowering in f64 — a Phase 6 caller using the former will not bit-match `monthly_at_ground` → final review fix wave (doc which is canonical)
- Task 5: minor (deferred): the tool loads the world file twice (bounds, then the layer); the report quotes rain for month 4 only (no month-10 rain values) → Task 6 evidence renders both; final review triage

The whole-plan review's triage (its section C) and the fix wave: fixed — the wrap clause of `normals_at`'s doc
(Task 1), the header and family crcs (Task 2, as ruling (2)), the gap line's wording (Task 4: it catches a missing or
mis-scaled lapse on land at fine-cell centres, otherwise it checks the sea-level field), the summary's two lines
(Task 4), the canonical ground read (`ground_normals`, `f64`; `lowered_by` does not bit-match — Task 4); also the
placeholder's whole-stride assert and `bilinear_cells`' saturating neighbour index (the review's new minors). Accepted
as they stand — `read`'s `&dyn Fn`, `latitude_at` beside `latitudes` (not merged: routing one through the other can
move a bit of the temperature raster), the meta error's prefix, the transient copies, the exact span check, the map
tool's double load. Already fixed before the review: the 0207 wording, the placeholder's peak memory.

What Task 6 did with the three routed to it: the 0207 wording — at 32 m seed 0207's monthly precipitation seam is
`no` (2.802, rank 0.016) while its fine precipitation seam is `yes` (0.368, rank 0.569), so it is NOT inherited
there; at 64 m both are `no` (fine 3.894, rank 0.008; monthly 4.392, rank 0.000), as the Evidence says. The transient
copies are recorded in the design's Measurements as not measured (from the code). The month-10 rain map was rendered
(`target/world-forge/phase1b-monthly-rain-10.png`) and its values are in the program's Owner hand checks.
