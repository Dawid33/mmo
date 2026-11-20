# Plates and Aged Relief (Phase 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Written 2026-11-20** from a code check of that day at `a6fb06f9` (Phase 1a Task 18 landed, Task 19 in flight;
> Phase 1b planned, NOT built). It is dispatched only after Phase 1b is DONE: every symbol this plan takes from
> Phase 1b (`WorldArtifact::climate`, `ForgedMeta::climate`, `validate_climate`, `TileCache::new`'s five arguments,
> the report line `artifact_rasters_crc`, `ARTIFACT_VERSION` 10, `TILED_FORMAT_VERSION` 3) is named as that plan
> specifies it and is RE-READ by the controller at dispatch. Search by the named symbol, never by line.
> **No step commits a test.** **Revised the same day** after the builder check
> (`.superpowers/sdd/2026-11-20-plates-and-aged-relief-2/builder-check.md`, read-only at `d67f5211`): every BLOCKER and
> FIX is settled in the text; the three rejected points are Rulings 12–14.

**Goal:** Every forged world's continents, mountain belts and volcanoes come out of one plate simulation: several continents, one or more ranges per continent, and ranges that differ by what built them and how long ago — young ones high and sharp, old ones lower and rounder, never flat.

**Architecture:** Three moves. (1) A plate simulation on a fixed 512 × 256 **planet grid** (`forge/plate_sim.rs`), run ONCE per forge from the seed and the plate knobs alone, scored and re-run with a derived seed inside the forge; its per-cell record (crust kind, age, thickness; orogeny kind, age, strength; fold direction) is reduced to a handful of smooth planet-grid fields (`forge/planet.rs`) that every erosion level SAMPLES — nothing planetary is re-derived per resolution. (2) `World::new` takes land, initial height, uplift and volcano sites from those fields; the "continents" formula, the static Voronoi plates, the interior ranges and their knobs are deleted. (3) Age enters the stream-power run as INPUTS — present uplift by belt kind and age, initial height from crust thickness, isostatic rebound, an incision threshold, erodibility and diffusion by age, a rock column per geological province — and the height fit moves from one final scale into rate space. The record is stored (resident, like Phase 1b's climate family); the runtime reads only the province, with integer arithmetic.

**Tech Stack:** Rust; crate `worldgen` (the forge, `forged.rs`, `forged_tiles.rs`, the `forge_proto` and `world_map` examples); mirrors of knob names in `client` (`frontend/generator.rs`), `panel` (`forge.rs`), `world-store` (`ctl.rs`). `game` is NOT touched (`game::ColumnGround` stays as it is). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` (Goals 2, 6–8; Anti-goals; Approach A; Decision 4; the Phases table; "Open questions": the polar edges). Program: `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Phase 2; its Standing rules bind every task). Research: `docs/research/research_notes/Earth like world generation overhaul/plate_tectonics.md` (all), `mountains_erosion_glaciers.md` §1–2, §7–8, `current_forge_audit.md` §2, §4, §7, §12, and the report's evidence ledger.

Ledger: `.superpowers/sdd/2026-11-20-plates-and-aged-relief-2/progress.md` (git-ignored workspace). The commit procedure, implementer and reviewer rules are Phase 1a's: `.superpowers/sdd/2026-11-14-planet-frame-1a/{common,implementer-rules,reviewer-rules}.md`.

## What was read, and what is relied on second-hand

| Source | Status for THIS plan |
|---|---|
| Tzathas, Gailleton, Steer, Cordonnier 2024, "Physically-based analytical erosion for fast terrain generation" (CGF 43(2)) | **Read in full** (author version, fetched 2026-11-20). What it is: `z(x, t) = z0(D) + ∫_D^x u/a ds` with `a = k·A^m`, `n = 1`, `m = 0.4`, along the river tree; `t` enters as the travel time `T(D, x) = ∫ ds/a = t` of the base-level signal; when `D` reaches the boundary the cell IS at steady state. Uplift varies in space, NOT in time; no isostasy; hillslope and a 30° talus are folded into `a`. Their parameters (`u` 10⁻³ m/yr, `k` 2·10⁻⁵, `kh` 0.1 m²/yr) put steady state at 1.6–4.6 My for 5–25 km mountains. Its "time" is time since uplift began over an initial terrain — hundreds of thousands of years to a few million. It does not make a 300 My old range; with uplift off the terrain retreats to flat. **Consequence (Ruling 4):** the forge keeps its implicit solver; an old range is a different steady state, not a longer run. Taken from it: the response time `T` as the measure of how far a run is from steady state (a report line, Task 9) |
| Cortial, Peytavie, Galin, Guérin 2019, "Procedural Tectonic Planets" | **Sections 4.1–4.5 and Appendix A read first-hand** (author PDF, 2026-11-20): `ũ = u0·f(d)·g(v)·h(z̃)`, `g = v/v0`, `h = z̃²`, `z̃ = (z − zt)/(zc − zt)`; collision radius `r = rc·sqrt(v/v0 · A/A0)`, surge `Δz = Δc·A·(1 − (d/r)²)²`; rifting `P = λe^(−λ)`, `λ = λ0·f(xP)·A/A0`, 2–4 fragments; `z −= (z/zc)·εc·δt`; constants `δt` 2 My, `zr` −1 km, `za` −6 km, `zt` −10 km, `zc` 10 km, `rs` 1,800 km, `rc` 4,200 km, `Δc` 1.3·10⁻⁵ km⁻¹, `v0` 100 mm/yr, `εo` 0.04, `εc` 0.03, `εt` 0.3, `u0` 0.6 mm/yr. The paper gives NO value for `f`'s control distance, `λ0`, `β`, `ε` |
| PlaTec / `plate-tectonics` source; Tectonics.js posts; Worldbuilding Pasta; Frozen Fractal | Second-hand: the research note's reading (code and posts were read by the researcher, not re-read here) |
| Baldwin, Whipple & Tucker 2003 (decay 1–10 My; rebound ≤ ×6, threshold ≈ ×20) | **Not read**: the publisher served a wall on 2026-11-20. The multipliers stay abstract-level, as the report's ledger marks them |
| Viitanen 2012 (thesis), Braun & Willett 2013, any flexural-isostasy source | Not fetched. The forge already implements Braun & Willett (`erosion.rs`); the rebound term is the mountains note's own recipe |

**Untested designs this plan builds** (each has an evidence step before anything depends on it; none is trusted from the notes): Cortial's rules on a flat, x-wrapping raster (Tasks 1–4, judged in Task 5); the polar edges (Task 3, judged in Task 5); orogeny strength as a decaying scalar with closed-form elevation instead of integrated elevation (Ruling 1); every decay time and threshold (`belt_decay_my`, `ROOT_DECAY_MY` apart — that one is Cortial's `zc/εc`); the rebound low-pass, the incision threshold and the age multipliers (Tasks 10–12, judged in Task 15); the stacked province column (Task 6, proven bit-identical for one province before Task 13 uses several). Numbers marked *recalled* in the notes (relief and slope targets by age class) are NOT used as targets: the targets below are ratios this plan defines.

## Global Constraints

- Do not create git worktrees; work on `develop`. No stash, reset, amend, checkout of files, clean, push. The tree
  holds the owner's unrelated uncommitted changes and other sessions stage in the same index: each commit carries
  only the task's own hunks, staged through the index as `common.md` describes (never a pathspec commit of a mixed
  file). The committed-tree compile check is the CONTROLLER's.
- **Tests.** The repository commits only regression tests of bugs actually seen and the existing gates. Every
  "temporary test" below is written first, seen failing, quoted in the implementer's report (name, command, failing
  then passing output) and DELETED before the commit; a commit adds no `#[test]`. An "evidence command" is a tool
  run whose output is quoted in the report. No new gate.
- **The order inside every Verify step is fixed:** (1) the temporary tests pass; (2) DELETE them; (3) `just smoke`
  (if its audit fails on a file that is NOT the task's own — another session's temporary test — run
  `just test-audit crates/worldgen` plus the task's other crate directories and smoke's remaining steps by hand, and
  say so); (4) `just web-check` when `worldgen` library code changed (`just determinism-wasm` compiles only `game`
  and `physics-next`: it is run only if a task finds it must touch `game`, which is then BLOCKED first);
  (5) **every task that touches `crates/worldgen/examples/` or `crates/worldgen/tests/`, or deletes or changes a
  public `worldgen` symbol** (all but Tasks 5 and 15) also runs `just test-one worldgen "--no-run --tests --examples"`
  — `just smoke` compiles neither the examples (`forge_proto`, `world_map`, `cross_section`, `seed_scan`) nor the
  integration tests, and the recipe is capped (`cap`), which raw `cargo` is not.
- **Two regimes.**
  - *Tasks 1–6 change no world.* The safety net, run by each: (1) the gate passes UNTOUCHED
    (`just test-one worldgen "--test it -- --include-ignored forge_lib::"`); (2) `just forge-report --size dev --cell 32
    --seeds 8` and `just forge-report --size dev --cell 64 --seeds 8` reproduce every line of `baseline-32.txt` /
    `baseline-64.txt` (saved in the ledger directory by Task 1 Step 0 BEFORE any change), unchanged and in order —
    lines are only APPENDED (`diff baseline-N.txt new-N.txt | grep '^<'` prints nothing); in particular `height_crc`
    and `artifact_rasters_crc` per seed. A changed line or a failing gate is BLOCKED, never a re-record.
    **Tasks 1–5 change no stored byte at all** (no knob is added to `SPECS` before Task 7, so the stored knob map and
    Phase 1b's `world_file_bytes` do not move). **The one exemption:** in Task 6 the line `world_file_bytes` grows by
    the planet record and the province raster — quote old → new per seed; no other baseline line may move.
  - *Tasks 7–15 change worlds on purpose* (8 and 14 change none, and prove it by the previous task's `height_crc`s).
    Evidence is the before/after report table of the task. On every run
    these must still hold: the seam rule (Phase 1a's: per field at most `seeds / 4` seeds `seam_ok_*: no` AND median
    `seam_step_rank_*` ≥ 0.25 — `seam_pass_*: yes` for height, precip, biome and Phase 1b's two monthly fields),
    `polar_rows_all_sea: yes`, `drains_to_sea: 1.000`, `aliased_regions_identical: yes`, `world_map_wraps: yes`.
    One that fails is BLOCKED.
- **The quality gate** `forge_quality_matches_baseline` (`crates/worldgen/tests/forge_lib.rs`, fixture
  `tests/fixtures/forge_quality_planet_dev_32m.ron`) is re-recorded exactly ONCE, in Task 16, in a commit of its
  own stating old → new numbers. From Task 7's commit until then it FAILS if run (it is `#[ignore]`d as slow, so
  `just smoke` and `just test` pass; `just test-full` shows it failing): Tasks 7–15 run it WITHOUT bless and quote
  its message (Ruling 8). `quality::within` names only the metrics out of tolerance, so the before / after NUMBERS
  come from the report's own lines (Task 9 adds the hypsometry and river ones). **The gate guards land share,
  hypsometry and rivers only** (Phase 1a's whole-plan review, section D point 6) — not the seam, the wrap, the polar
  seas, biomes, tiles, the runtime sampler or any other size: in this phase those are carried by each task's
  evidence commands, never by the gate passing.
- **Stored formats.** This phase bumps `ARTIFACT_VERSION` 10 → 11 and `TILED_FORMAT_VERSION` 3 → 4, each once, both
  in Task 6. `WORLD_MAP_VERSION` (3) and `META_FORMAT` (2) do not move. No other task may change a stored or wire
  byte layout; one that finds it must reports BLOCKED. Every world is re-forged, never migrated.
- There is never a commit with no working, loadable world.
- **The planet grid is fixed** (512 × 256) and the plate stage reads ONLY the seed and the knobs named in
  "Shared vocabulary": never `width_m`, `height_m` or `cell_m`. Every planetary length is in degrees of latitude
  (`climate::metres_per_degree(height_m)` converts at USE, on the fine grid). True-scale lengths stay in metres.
- **The forge may use std float math** (its output is a stored artifact); it stays independent of the thread count
  (the plate stage is serial; per-cell maps go through `par::map_range`). **Runtime sampling in `forged.rs` stays
  exact arithmetic** (add, subtract, multiply, divide, floor, sqrt) as today's runtime is: what this phase adds there
  is one nearest-cell read at the existing `dithered` point (`f64` add, multiply, floor and the existing dither
  noise) and integer shifts — no new transcendental, no new noise.
- `game`, `server`, `worldgen` stay Bevy-free. Never write a region edge as a literal.
- Forge tunables a player or the tuning page should move are knobs in `forge/knobs.rs`, each with a reader; the
  structural constants of the plate rules are named `const`s in `forge/plate_sim.rs` / `forge/planet.rs`, each
  with a doc line giving its source or "untested design". **A knob enters `SPECS` in the task that gives it a
  reader in the forge, never earlier** (the generator shows every `SPECS` entry as a slider, and `knobs_map` stores
  every one in the world file). No knob may be left with no reader at the end of a task, except the seven old ones
  across Tasks 7 → 8 (Ruling 8).
- **Tool facts from Phase 1a's fix wave:** `--set <knob>=<value>` REFUSES a value outside the knob's range at parse
  time (every `--set` in this plan is inside its range: check a new one before writing it); the `--report`
  summary's seam verdict allows `seeds / 4` seeds `no` for `seeds ≥ 4` (2 of 8, 4 of 16) with a median rank ≥ 0.25;
  `SocietyGrid::world_id` does not depend on `wrap_x`. The forge files' "land edge" comments were reworded: this
  plan quotes SYMBOLS, never comment text.
- Forge cost is measured, not capped. The plate stage's design budget is in Ruling 1.
- Any new or changed command updates its `justfile` recipe (or its comment) in the same task; `CLAUDE.md`
  (Worldgen paragraph) and `TODO.md` change in the commit that changes the fact.
- Never launched by agents: the game window, paid generation, anything on orion or the twin, the dev Postgres
  world, benchmarks other than `just forge-bench`.
- Rulings made by a task are written in its report under "Rulings" as `Ruling: … — why — cost if wrong`.

## Shared vocabulary (decided here, used by every task)

- **Planet grid.** `PLANET_NX = 512`, `PLANET_NZ = 256`; cell `c = cz · PLANET_NX + cx`; x wraps (column 511 is
  beside 0), z does not (row 0 is the north pole's edge). `PLANET_DEG_PER_CELL = 180.0 / 256.0 = 0.703125`
  (degrees of latitude per cell, both ways on a 2:1 world). A cell's centre in **map fractions** is
  `((cx + 0.5) / 512, (cz + 0.5) / 256)`; a fine cell at metres `(mx, mz)` is at fractions
  `(mx / width_m, mz / height_m)`. One planet cell is 32 m on Dev, 50 m on Tiny, 98 m on Small, 196 m on Medium,
  392 m on Large.
- **Model time** in millions of years (My), `t = 0` at the start; a run is `steps = round(plate_duration_my /
  PLATE_DT_MY)` steps and `T_END = steps · PLATE_DT_MY` (451 My → 301 steps → 451.5): every age is taken from `T_END`. **Model height**
  in metres on Cortial's scale (`TRENCH_M = −10_000`, `CONT_MAX_M = 10_000`); it is planet data, never a fine-grid
  height.
- **The plate state** (`plate_sim::PlateState`, one value per planet cell, struct of arrays):
  `plate: u8` (`NO_PLATE = 255`), `crust: u8` (`CrustKind { Oceanic = 0, Continental = 1 }`), `born_my: f32`
  (when the crust formed; initial continental crust is born at −600 … −2,500 My by a noise), `base_m: f32`
  (continental base elevation), `strength: f32` (0 … 1: how strong the last orogeny was), `orogeny_my: f32`
  (when it last acted; `f32::NEG_INFINITY` = never), `kind: u8`
  (`OrogenyKind { None = 0, Arc = 1, Andean = 2, Collision = 3, Plateau = 4, RiftShoulder = 5, Hotspot = 6 }` —
  stored bytes: append only), `fold: [f32; 2]` (unit direction of convergence, east and south components; the fold
  AXIS is perpendicular to it; `[0, 0]` = none). Per plate: `vel: [f64; 2]` (cells per step, each component in
  −1 … 1), `acc: [f64; 2]` (the fractional remainder), `alive`.
- **Ages** at the end: `crust_age_my = T_END − born_my`, `orogeny_age_my = T_END − orogeny_my` (infinite for
  `None`). **Age classes of a belt:** young `< YOUNG_MAX_MY = 80`, old `≥ OLD_MIN_MY = 200`, middle between.
  `age_weight(age) = smoothstep(80, 200, age)` (0 young, 1 old).
- **Two decay laws of one strength** (closed forms — nothing is integrated, so nothing can drift or pile up):
  - `activity(age) = BELT_RESIDUAL + (1 − BELT_RESIDUAL) · exp(−age / k.belt_decay_my)`, `BELT_RESIDUAL = 0.12`,
    `belt_decay_my` 120 — how much of a belt's uplift RATE is still acting (untested design);
  - `root_keep(age) = ROOT_RESIDUAL + (1 − ROOT_RESIDUAL) · exp(−age / ROOT_DECAY_MY)`, `ROOT_RESIDUAL = 0.15`,
    `ROOT_DECAY_MY = 333.0` — how much of its crustal ROOT is left. 333 My is `zc / εc` = 10 km ÷ 0.03 mm/yr,
    Cortial's own continental-erosion e-folding (read).
- **Derived per planet cell** (`planet::Planet`):
  - `elevation_m`: oceanic `OCEAN_ABYSS_M + (OCEAN_RIDGE_M − OCEAN_ABYSS_M) · exp(−crust_age / OCEAN_SUBSIDE_MY)`
    (−1,000 at the ridge, −6,000 abyssal: Cortial's `zr`, `za`; `OCEAN_SUBSIDE_MY = 40`, a shape only — deep
    bathymetry is data) plus an arc's `OROGEN_HEIGHT_M[kind] · strength · root_keep`; continental
    `base_m + OROGEN_HEIGHT_M[kind] · strength · root_keep(orogeny_age)`. `OROGEN_HEIGHT_M` = 0, 3,000, 6,000,
    6,000, 6,000, 800, 7,000 by kind. A `Hotspot` cell (either crust) adds `7,000 · strength ·
    exp(−orogeny_age / HOTSPOT_SUBSIDE_MY)` instead (`HOTSPOT_SUBSIDE_MY = 25`, untested): over the abyss a new mark
    stands near +1,000 m — an island — and the chain behind it sinks to seamounts within about 50 My. After the sea-level solve it is stored RELATIVE to the solved sea level;
  - `thickness_km`: oceanic 7; continental `35 + (elevation above base) / (1,000 · AIRY_SURFACE_SHARE)`,
    `AIRY_SURFACE_SHARE = 0.18` (1 − 2.7/3.3: the notes' arithmetic from standard densities);
  - `land: bool` (elevation above the solved sea level and inside the polar margin);
  - `coast_sd_deg`: signed distance to the coast in degrees, positive inland, smoothed (Task 4);
  - `uplift_now` (0 … 1) = `KIND_UPLIFT[kind] · strength · activity(orogeny_age)` (`KIND_UPLIFT` = 0, 0.6, 1.0,
    1.0, 0.8, 0.3, 0.7; a hotspot uses `HOTSPOT_DECAY_MY = 12` in place of `belt_decay_my`);
  - `root_rel` (0 … 1) = `(thickness_km − 35) / 33`, clamped — the inherited relief (Task 10);
  - `age_w` = `age_weight(orogeny_age)` where `kind ∈ {Arc, Andean, Collision, Plateau}`, else 0 — "old" means an
    old BELT. Lowlands, platforms and shields get no age modifier (their continental crust is 600–2,500 My old, so
    a crust-age weight would switch every modifier on over all non-belt land and stop lowland channels cutting);
    shields differ by their rock (Task 13), not by an erosion law;
  - `volcanic` (0 … 1): `strength` on `Arc` / `Andean` cells younger than 30 My, `HOTSPOT_VOLCANIC = 0.5` on
    `Hotspot` cells younger than 10 My, else 0 (0.5 is past `VOLCANISM_FLANK`'s ramp and under `VOLCANISM_CORE`'s
    damp, so a hotspot gets the full hot-spot term in `VolcanismNoise::at`; 1.0 would be damped to 0.3);
  - `province: u8` (Task 13; 0 until then);
  - labels: `continent: u8` (`NO_LABEL = 255`), `belt: u16`, and the lists `continents`, `belts` (Task 4);
  - `sea_level_m`, `attempt: u8`, `ok: bool`, `score: PlanetScore`.
- **Continent** = a 4-connected component of `land` cells (x wraps) holding at least `CONTINENT_MIN_SHARE = 0.03`
  of the planet grid's cells. **Belt** = an 8-connected component (x wraps) of land cells of ONE age class with
  `kind ∈ {Arc, Andean, Collision, Plateau}` and `strength · root_keep ≥ BELT_MIN_REL = 0.12`, of at least
  `BELT_MIN_CELLS = 10` cells (about 5 square degrees); its continent is the one most of its cells lie on (none:
  an island belt).
- **A world passes** (`PlanetScore::ok`) when: at least 2 continents; every continent has at least one belt; at
  least one young belt and at least one old belt on the map.
- **Sampling the planet on a fine grid** (`planet::Sampler`, Task 7): `bicubic` (Catmull-Rom; x wraps by index,
  z clamps) for `coast_sd_deg`, `uplift_now`, `root_rel`; `bilinear` for `age_w`, `volcanic`; `nearest` for
  `kind`, `province`, the belt label. Belt, age and province reads are ALL taken at one WARPED position (the
  surviving `Warp` noises, amplitude `PLANET_WARP_DEG = 1.5`), so a fine cell's uplift, modifiers, rock and belt
  label agree; the coast and the continent label are not warped — the coast has its own noises.
- **Landness** (Task 7; today's unit kept): `landness = min(coast_sd_deg / LANDNESS_UNIT_DEG, 1.0) +
  (coast_wander_deg · lo + coast_detail_deg · hi) / LANDNESS_UNIT_DEG`, `LANDNESS_UNIT_DEG = 90.0` — one unit is a
  half-height, exactly what `ContinentalMass::at` returns today, capped at 1 as `CONTINENT_INTERIOR_MASS` caps it
  (so the polar margin's `− 2 · smoothstep` still drowns anything). Every constant of `World::new` written in
  landness therefore keeps today's width (table below).
- **Knobs after this phase** (group, default, range; the task that adds each is the one that gives it a reader).
  New — Task 7: "Land" `land_fraction` 0.32 [0.15, 0.5], `coast_wander_deg` 6.0 [0, 20], `coast_detail_deg` 1.5
  [0, 6]; "Plates" `plate_duration_my` 450 [250, 700], `rift_rate` 1.0 [0.25, 4.0], `microcontinents` 4 [0, 10],
  `belt_decay_my` 120 [40, 400]. Task 9: "Uplift" `mountain_aspect` 0.6 [0.2, 2.0]. Task 11: "Erosion"
  `rebound_share` 0.8 [0, 0.9]. Task 12: "Erosion" `incision_threshold` 0.1 [0, 1] (a share of a full belt's
  fitted uplift rate, Ruling 4), `old_erodibility` 0.7 [0.3, 1.5], `old_diffusion` 3.0 [1, 4].
  Deleted — Task 7: `coast_noise_lo`, `coast_noise_hi` (their unit, a half-height, cannot express a coast drawn by
  the plates: replaced by the two `coast_*_deg`). Task 8: `continent_count`, `continent_sea_width`,
  `continent_coast`, `plate_count`, `plate_belt_width`, `belt_length_km`, `interior_ranges` (and
  `Knobs::plate_total`). Task 9: `uplift` (the rate fit sets the rate: Ruling 5).
  Kept, with their reader: `ocean_margin_start` (`World::new`'s polar margin and the planet's `inside`),
  `belt_uplift` (belt rate over lowland rate), `lowland_uplift_share`, `volcano_count_max`, `volcanism_strength`,
  `max_peak_m`, every "Geology" knob. Until Task 7 the plate stage reads `planet::KEY_DEFAULTS` (the same five
  numbers as constants) and the tool's `--key name=value`.
  **The plate stage reads exactly:** the seed, `land_fraction`, `plate_duration_my`, `rift_rate`,
  `microcontinents`, `belt_decay_my`, `ocean_margin_start` (`planet::PlanetKey`).
- **Lengths** (Ruling 3). Planetary, in degrees, converted with `metres_per_degree(height_m)` = 45.5 m (Dev),
  71.1 (Tiny), 139.4 (Small), 278.8 (Medium), 557.5 (Large):

  | Constant | Degrees | Source | Dev | Tiny | Small | Medium | Large |
  |---|---|---|---|---|---|---|---|
  | `SUBDUCTION_REACH_DEG` (a subduction belt's width) | 16.2 | Cortial `rs` 1,800 km | 737 m | 1.15 km | 2.26 km | 4.52 km | 9.03 km |
  | collision radius (typical … `COLLISION_RADIUS_MAX_DEG`) | 10–25 … 37.8 | Cortial `rc` 4,200 km | 0.46–1.14 km | 0.71–1.78 km | 1.4–3.5 km | 2.8–7.0 km | 5.6–13.9 km |
  | `SUTURE_DEPTH_DEG` | 2.7 | Cortial 300 km | 123 m | 192 m | 376 m | 753 m | 1.5 km |
  | `POLAR_FADE_DEG` | 12 | untested design | 546 m | 853 m | 1.67 km | 3.35 km | 6.69 km |
  | `coast_wander_deg` (amplitude; its noise's wavelength is 92°) | 6 | replaces `coast_noise_lo` | 273 m (4.2 km) | 427 m (6.6 km) | 836 m (12.8 km) | 1.67 km (25.7 km) | 3.35 km (51.4 km) |
  | `coast_detail_deg` (amplitude; wavelength 11.5°) | 1.5 | replaces `coast_noise_hi` | 68 m (524 m) | 107 m (819 m) | 209 m (1.6 km) | 418 m (3.2 km) | 836 m (6.4 km) |
  | coast taper `smoothstep(0, 0.05, landness)` | 4.5 | today's constant, unit kept | 205 m | 320 m | 627 m | 1.25 km | 2.51 km |
  | land ramp `smoothstep(0, 0.5, landness)` (+45 m) | 45 | today's constant | 2.0 km | 3.2 km | 6.3 km | 12.5 km | 25.1 km |
  | shelf to −90 m (`160 · shape − 1`) | 50 | today's constant | 2.3 km | 3.6 km | 7.0 km | 13.9 km | 27.9 km |
  | `VOLCANO_MIN_LANDNESS` 0.15 | 13.5 | today's constant | 614 m | 960 m | 1.9 km | 3.8 km | 7.5 km |
  | one planet cell (the plate raster under the coast) | 0.703 | Ruling 2 | 32 m | 50 m | 98 m | 196 m | 392 m |
  | `REBOUND_RADIUS_DEG` | 3 | untested (notes: 50–150 km on Earth) | 137 m | 213 m | 418 m | 836 m | 1.67 km |
  | `VOLCANO_SPACING_DEG` | 8 | replaces 8,000 m | 364 m | 569 m | 1.12 km | 2.23 km | 4.46 km |
  | `RELIEF_WINDOW_DEG` (evidence only) | 2 | this plan | 91 m | 142 m | 279 m | 558 m | 1.12 km |
  | peak target for a belt 8° deep (`mountain_aspect` 0.6) | — | Ruling 3 | 218 m | 341 m | 669 m | 1,338 m | 2,676 m |
  | peak target for a belt 20° deep | — | Ruling 3 | 546 m | 853 m | 1,673 m | 2,800 m (cap) | 2,800 m (cap) |

  **What a coastline's detail is, per size** (the smallest features, in metres and in 16 m forge cells): the
  plates draw the outline down to about 2 planet cells (64 m = 4 cells on Dev … 784 m = 49 cells on Large); the
  wander noise bends it by its amplitude over a quarter of its wavelength (273 m over 1 km on Dev … 3.3 km over
  13 km on Large); the detail noise adds coves of 68 m = 4 cells (Dev), 107 m = 7 (Tiny), 209 m = 13 (Small),
  418 m = 26 (Medium), 836 m = 52 (Large). Below that a coast is as smooth as today's (today's finest coast noise
  is the same 11.5° wave) until Phase 4 floods eroded valleys. The two amplitudes are starting values (the noises'
  standard deviation is about 0.5, so a typical excursion is half the amplitude); Task 15 tunes them.

  True scale, in metres, unchanged: `hillslope_area_m2`, `diffusivity`, `river_*`, `floodplain_*`, `lake_*`, the
  fold wavelength (2,048 m), the coast and belt noises' wavelengths (they already scale with `span_m`), the
  volcano cone radius (`clamp(4° in metres, 250, 3_000)`: 250 m on Dev, 284, 558, 1,115, 2,230 m on Large).
- **Province** (Task 6 container, Task 13 values): `u8` per FINE cell, an index into `columns` (at most
  `PROVINCE_MAX = 5` rock columns). `PROVINCE_PLATFORM = 0`, `PROVINCE_FOLD_BELT = 1`, `PROVINCE_SHIELD = 2`,
  `PROVINCE_VOLCANIC = 3`, `PROVINCE_OCEAN = 4`. Until Task 13 there is one column and every cell is 0 — the
  platform, whose stack shift is 0 (`PROVINCE_SHIFT_BLOCKS = [0, 8_192, −8_192, 16_384, −16_384]`).
- **Report lines** are appended at the END of `report::report`'s rows, in task order; plate-stage lines start
  `planet.`.

## Rulings (made while writing this plan; each `what — why — cost if wrong`, with the alternative rejected)

1. **The plate model is PlaTec's bookkeeping with Cortial's rules: one world-sized record per cell, each plate a
   translation of whole cells; steps of `PLATE_DT_MY = 1.5` My; `round(plate_duration_my / 1.5)` steps (300 at the
   default).** Each step a plate's fractional position accumulates `vel` and it moves by a whole `(dx, dz)` with
   each component in {−1, 0, 1}: a move copies records, so crust is never resampled, blurred or lost to rounding.
   Where two cells land on one: the older oceanic one is consumed (subduction; oceanic always under continental);
   continental under continental is consumed too and counted (a collision in progress). Where no cell lands: new
   oceanic crust, owned by the plate that stood there before the step. Rows past the north or south edge are
   destroyed. x wraps by index (`rem_euclid`; `PeriodicX` is for functions of metres — the simulation is on cell
   indices, and `PeriodicX`'s noises are used only at the fine-grid sampling, Task 7). Speed is capped at one cell
   per step = 0.47° per My (5.2 cm/yr at Earth's 111 km per degree; Cortial's `v0` is 10 cm/yr): `g(v) = v / cap`.
   **Rotation is left out** — on a strip periodic in x a rigid rotation of the plane is not even defined for a plate
   that reaches round the world, and inverse-mapped rotation is the research's own untested proposal. **Elevation
   is not integrated**: a cell keeps `strength` and `orogeny_my`, and elevation, thickness and present uplift are
   closed forms of them (vocabulary) — that removes the two failure modes the sources report (mountains on every
   continental cell; 15 km peaks) by construction. **Budget:** 131,072 cells × 300 steps = 39 M cell-steps, about
   2.5 linear passes per step (the distance field is refreshed every 4th step, ages are stored as birth times so
   there is no ageing pass): designed to 0.4 s per attempt on the dev box in release, single-threaded. That is
   +40 % on a Dev / 32 m forge (1.04 s measured in Phase 1a), +4 % on Small / 32 m, +1 % on Medium / 32 m, under
   0.3 % on Medium / 16 m; under 10 % of the forge from Small / 32 m up. These are PER ATTEMPT and count the
   simulation's passes only: the per-suture floods and chamfers, the derivation (Task 4) and up to five retries come
   on top — a seed that fails every attempt costs six times this, and the menu preview's first forge after a
   plate-knob change pays it once (the memo of Ruling 9 covers the rest). It is measured (`planet.stage_ms` per
   attempt, `planet.attempts_ms` in all), not capped — why: the simplest model that has every rule the program lists, lossless, deterministic, and cheap
   enough to score and re-run — rejected: (a) per-plate rasters with an affine transform and a bake every few
   steps (the research's sketch: rotation and resampling loss for no visible gain at 0.7° cells), (b) Cortial's
   mesh on a sphere (projection, a design non-goal), (c) a field model (Tectonics.js v2: least documented) — cost
   if wrong: belts run straight and parallel to plate edges (the sampling warp and the along-strike noise are the
   only curvature); arcs do not curve. A later phase could add rotation behind the same `Planet` output.
2. **ONE planet grid, 512 × 256, for every world size and every forge cell; the planet is a pure function of
   `PlanetKey`** — the design allows 512 × 256 to 1,024 × 512; the low end is taken, for all sizes, because (i) the
   same seed then gives the same continents and belts at every size and at every preview cell (the menu preview at
   128 m shows the world Play forges at 16 m; the seed search's coarse run predicts the fine one), (ii) plate-scale
   features are at least 3 cells wide at 0.7°, and detail below a planet cell comes from the coast noises and from
   erosion, (iii) 1,024 × 512 at the same degree speed cap is 8× the cost. The fields the fine grid samples are
   SMOOTH by construction (a signed distance, a profile of distance, a blurred weight), so bicubic sampling leaves
   no cell-edge crease even where one planet cell is 24 fine cells (Large at 16 m); categories (`kind`,
   `province`) are read nearest at a noise-warped position. **Relation to the other grids:** the erosion schedule's
   levels each sample the same `Planet`; Phase 1b's climate grid (`cnx × cnz`, 196 × 98 to 784 × 392) is a
   different, world-size-dependent grid and stays so; Phase 3's planet-grid climate model may adopt
   `PLANET_NX × PLANET_NZ` — cost if wrong: Large's coasts and belt edges show the 392 m planet lattice through
   the noise; then `PLANET_NX/NZ` double for Large only (a constant, a re-forge, and the same seed no longer gives
   the same continents on Large).
3. **Belt widths are Cortial's distances read as degrees; mountains may be at most `mountain_aspect` (0.6) as
   high as their belt is deep, so small worlds have lower mountains** — the design says belts are map fractions
   and "a Tiny world has belts a kilometre or two wide and so lower mountains". Today a Dev world builds 2 km
   peaks on belts 1.2 km in half-width (mean flank slope above 55°), because stream-power relief hardly depends on
   belt width. The cap is the threshold hillslope angle the notes cite (30° talus, Cordonnier 2016, read; 30 ± 5°,
   Larsen & Montgomery, search summary): `tan 31° = 0.6`. The world's **peak target** is
   `clamp(mountain_aspect · belt_depth_deg · metres_per_degree, PEAK_TARGET_MIN_M = 150, max_peak_m)`, with
   `belt_depth_deg` = the largest distance from a belt cell to its belt's edge (`Planet::belt_depth_deg`). The
   table in the vocabulary follows — rejected: keeping `max_peak_m` as the target at every size (needle ranges on
   Dev and Tiny) — cost if wrong: **Dev and Tiny have no ground above 1,200 m, so no `Mountain` biome and no snow
   peaks until Phase 5's treeline rule** — every consumer that assumes kilometre relief is listed under "Small
   worlds" below with what this plan does about it; the owner may find the debug world too gentle —
   `mountain_aspect` is a knob for exactly that (2.0 restores today's proportions), and it is an Owner hand check.
4. **The erosion run is not a clock: age enters as the steady state a belt is eroded TOWARD, plus four
   modifiers** — the run models 0.8 My at erodibilities that put its response time near 1 My at every world size
   (the integral `∫ ds / (K·√A)` from Tzathas's formulation, with Hack's law, grows only as `L^0.17`), so within
   it every belt is close to the steady state of its inputs; a 300 My old belt cannot be "run longer". So:
   - **present uplift** `U = rate · coast_taper · (lowland_uplift_share + (belt_uplift − lowland_uplift_share) ·
     max(uplift_now · along, cone))` — `uplift_now` carries kind and age (Task 7); `rate` is today's `uplift` knob
     until Task 9 and from then `UPLIFT_START_M_PER_YR · relief_scale · uplift_fit` (Ruling 5);
   - **initial height** `h0 += INITIAL_ROOT_SHARE (0.5) · peak_target · root_rel · coast_taper` and
     **initial exhumation** `uplifted0 = h0_root + (−basement_top_s + EXHUMED_MARGIN_M (300)) · age_w · root_rel`
     — an old belt starts high (its root outlasts its uplift: 333 My against 120 My) and, at full strength, with
     its basement at the surface whatever the `basement_top_s` knob says (Task 10);
   - **isostatic rebound** every `REBOUND_EVERY_ITERS = 5` iterations: the rock eroded since the last time
     (`max(0, (h_ref + uplifted − uplifted_ref) − h)` per land cell), block-averaged onto the planet grid,
     Gaussian-blurred to `REBOUND_RADIUS_DEG`, times `rebound_share` (0.8), sampled back bilinearly and added to
     `h` and `uplifted` (Task 11). At long wavelengths it multiplies the effective uplift by up to
     `1 / (1 − 0.8) = 5` (the rate fit of Ruling 5 absorbs the mean), lifts summits beside deep valleys, and
     slows the decay of inherited relief five-fold;
   - **incision threshold**: after the implicit solve, `incision = (h + lift) − h*`,
     `h_new = h + lift − max(0, incision − dt · threshold)`, with `threshold = incision_threshold · belt_uplift ·
     rate · age_w` — a SHARE (0.1) of a full belt's fitted uplift rate, so it is scaled by `relief_scale ·
     uplift_fit` exactly as the uplift it competes with and means the same on Dev and on Large (an absolute m/yr
     value would be twice an old belt's uplift on Dev and a fifth of it on Large). An old belt's own rate is about
     `(0.04 + 1.36 · 0.13) = 0.22` of `rate` against a threshold of `0.1 · 1.4 = 0.14` of it: its small channels
     stop cutting, its steady relief rises by roughly the ratio (Task 12);
   - **erodibility and diffusion by age**: `K · (1 + (old_erodibility − 1) · age_w)`,
     `D · (1 + (old_diffusion − 1) · age_w)` (Task 12).
   `age_w` is 0 outside belts (vocabulary): none of the last three touches lowlands, which keep today's law.
   **What "old" must look like, and the numbers that show it:** lower (belt mean height), gentler (mean slope),
   rounder and less dissected (local relief in a `RELIEF_WINDOW_DEG` window), but not flat. Measured per age
   class on the fine grid (Task 9's lines): `young_old_slope_ratio ≥ 1.5`, `young_old_relief_ratio ≥ 1.5`,
   `old_lowland_relief_ratio ≥ 2.0` — why: the only formulation that gives the same young / old contrast on a
   16 km world and a 200 km one — rejected: an uplift history run through epochs (the mountains note's recipe:
   hundreds of coarse steps of real time, size-dependent, and its "old" result rests on the same untested
   threshold and rebound) and Tzathas's analytical solver (its time is 0.1–5 My; no decay over hundreds of My) —
   cost if wrong: old belts differ from young ones only in height; then `belt_decay_my`, the threshold and the two
   multipliers are the dials (Task 15), and what cannot be tuned is recorded.
5. **The height fit moves into RATE space, once, after the coarsest level; the final fit is a soft knee** —
   today `fit_height_band` multiplies all land by one factor when the highest peak overshoots, so one tall young
   summit lowers every old range and lowland with it, and a world that comes out too low is never raised. New:
   when the coarsest level ends, `f = clamp(FIT_AIM (0.9) · peak_target / peak, 0.25, 4.0)`; if `|f − 1| > 0.05`
   land `h` and `uplifted` are scaled by `f` AND every later level's uplift RATE is scaled by `f` (steady relief
   is linear in the rate for `n = 1`), so the finer levels refine a consistent world; the incision threshold is a
   share of the same rate and is scaled with it. **The `uplift` knob is deleted with this** (Task 9): the fitted rate
   is `0.9 · target / peak0` with `peak0` itself proportional to the starting rate, so the knob would cancel out and
   become a slider that does nothing; the starting rate is the constant `UPLIFT_START_M_PER_YR = 5.0e-3`.
   `mountain_aspect` and `max_peak_m` set how high, `belt_uplift` and `lowland_uplift_share` the contrast between
   belts and lowlands, `erodibility` how far the run gets toward its steady state and how dissected it is (no longer
   how high) — their help texts and the three presets that set `uplift` are rewritten in Task 9. `fit_height_band` becomes a
   compress above `knee = 0.85 · max_peak_m` only: `h' = knee + (h − knee) / (1 + (h − knee) / (max_peak_m −
   knee))` — monotone (every drainage path, lake and divide stays), untouched below the knee — why: young and old
   keep the contrast the plate stage gave them, and old ranges' absolute height no longer depends on one summit —
   rejected: fitting each belt to its own target (breaks drainage across belt edges) — cost if wrong: the
   coarsest level's peak predicts the final one badly (the code's own comment: 30 %); the knee then flattens the
   highest summits, which Task 15's maps would show.
6. **Sea level is solved on the planet grid for `land_fraction` (0.32), and the coast is ONE function of a
   continuous landness** — the level is the quantile of `elevation_m` that leaves `land_fraction` of ALL planet
   cells above it, counting only cells inside the polar margin. Default 0.32: Earth's 0.29 plus the share the
   coast noise and the fine margin take back (Phase 1a's accepted worlds were 0.26–0.37; the gate world 0.31).
   The fine grid reads the vocabulary's `landness` (today's half-height unit, capped at 1) and a cell is sea when
   `landness ≤ sea_stand` — `World::new_at(p, planet, sea_stand)`, with `World::new(p, planet) = new_at(p, planet,
   0.0)`. **For Phase 4** (erode against a sea about 120 m lower, then flood): `sea_stand` is the hook — a negative
   value moves the shore seaward along the same continuous field. Its VALUE and the shelf profile are Phase 4's:
   today's shelf (`160 · shape − 1`, floored at −90 m) is shallower than 120 m everywhere, so "−120 m" has no
   landness yet. Nothing in the plate stage or in the stored record changes when Phase 4 decides it — rejected: thresholding the planet's `elevation_m` directly on the fine grid (the jump
   between continental and oceanic crust would put the coast on planet-cell edges: 32 m stairs on Dev) — cost if
   wrong: the shelf slope is a formula, not a result; Phase 4 owns that.
7. **What is stored: the planet record, resident, in the world file and in `ForgedMeta`; a province byte per fine
   cell in the tiles; the rock columns in the globals. What the runtime reads: the province only** — the record
   (`forged::PlanetRecord`, 13 bytes per planet cell = 1.70 MB raw, the same on every world size) is what Phases 3
   to 5 consume: crust kind, age and thickness, orogeny kind, age and strength, fold direction, plate id, deep
   bathymetry (`elevation_m`), and `attempt` / `ok`. It lives in `worldgen`, not `game`: no simulation code reads
   it (Phase 1b put the climate family in `game` only because Phase 6's weather does). **Rock with exact
   arithmetic and no change to `game::ColumnGround`:** the columns are STACKED in the stratigraphic coordinate —
   province `p`'s column is shifted by `province_shift_blocks(p) = PROVINCE_SHIFT_BLOCKS[p]` (0, +8,192, −8,192,
   +16,384, −16,384 blocks: the platform, province 0, is not shifted, so with one province no `geo` and no strata
   top moves at all), and a block column's `geo` is lowered by the same shift, so `s = y − geo` lands in the
   province's own band of the one `strata` list. The shifts are integers and the province read is exact arithmetic; `i16` holds it for 5 provinces (`geo` within about −17,300 … 27,300 for heights up to 3,500 m, tops within
   −23,776 … 17,184 — the forge clamps the stored `uplifted` so a surface `s` stays inside its band, which changes
   no rock: Task 6).
   The province of a block column is the byte of the cell nearest its DITHERED point (the biome border's
   `dithered`), so contacts wander at block scale — rejected: a per-column strata list in `ColumnGround` (changes
   a `game` type that is hashed and sent), or deriving the province from the planet record at runtime (392 m
   stairs on Large, and the forge's erodibility would not match the rock the player digs) — cost if wrong: a
   16 × 16-block footprint that straddles two provinces carries every formation between their bands (a few
   hundred entries, on borders only: `strata_footprint_max` is reported); if that hurts, `footprint_strata` is
   made a union of ranges.
8. **Order: simulate beside the old formula → tune the plates on the planet grid → store → switch → delete →
   one erosion input per task → geology → score terms → tune the relief → close. No temporary knob selects old
   or new land** — Tasks 1–5 add the simulation, its pictures and its report lines while `forge` never calls it
   and no knob is added (the worlds' crcs and `world_file_bytes` prove it), and the plate look is tuned THERE, where an 8-seed round costs seconds. Task 6
   stores the record and the province container with one province (rock bit-identical). Task 7 switches
   `World::new` in one commit; Task 8, dispatched immediately after, deletes what Task 7 left unread — for those
   two consecutive commits `continent_count`, `continent_sea_width`, `continent_coast`, `plate_count`,
   `plate_belt_width`, `belt_length_km`, `interior_ranges` exist with no reader, and `ContinentalShape::mass` is built and never read
   (a dead-code warning, not an error) (the only exception to the no-dead-knob rule; a flag choosing old or new land would itself be a knob to delete and would double every
   later evidence run) — cost if wrong: none beyond a two-commit window. **The gate** fails from Task 7 to Task 16
   (Global Constraints): the program allows one re-record at the end of the phase; Phase 1a ruled the same for two
   commits, here it is nine tasks — cost: `just test-full` shows one failing gate while the phase is in flight.
9. **The score and the re-run are in the forge, at the plate stage** — `planet::planet_for(p)` runs attempt 0
   with the seed itself and attempt `a` (1 … `PLATE_ATTEMPTS_MAX − 1 = 5`) with
   `plate_seed(seed, a)` (a 64-bit mix pinned in Task 4); the first attempt whose `PlanetScore::ok` holds
   wins; if none does, the attempt with the most criteria met (ties: the lowest `a`) is used and `ok = false`.
   Every caller of `forge` / `forge_with` gets it — the menu's preview and Play forge, `worldctl forge`, the
   server's empty-store forge, the panel's Job, every tool. The three criteria (continents, a belt per continent,
   both age classes) are all on the planet grid, so a failed seed costs 0.4 s, not a forge. **Seed → world stays a
   function:** the sequence of attempts depends only on `PlanetKey`. Only the PLATE stage is re-seeded: coast
   noise, geology, climate and the runtime's noises keep the input seed, which is what `WorldArtifact::seed`
   stays; `PlanetRecord::attempt` and `ok` record what happened. A one-entry process-wide memo
   (`PlanetKey → Arc<Planet>`) keeps the menu preview and `forge-ui` from re-simulating on every slider that is
   not a plate knob (a pure function of the key: the memo is not observable). The finished world is checked
   against the plate-stage score in the REPORT (`planet_matches_fine`), and `quality::world_score` (the
   `--search` tool) gains the terms (Task 14) — rejected: scoring after erosion and re-forging (minutes per
   retry on Medium, and the menu's Play would silently take several times its estimate) — cost if wrong: a world
   passes on the planet grid and fails after the coast noise (a continent just under 3 %); the evidence counts how
   often, and `CONTINENT_MIN_SHARE` gets a margin if it is more than 1 seed in 8.
10. **The polar edges: destroy, fill, fade — with a soft wall as the named fallback** — as the research proposes:
    a cell moved past row 0 or row 255 is destroyed; a gap is new ocean crust; every orogeny gain is multiplied by
    `smoothstep` over the last `POLAR_FADE_DEG` to each pole; the supercontinent's centre starts within 30° of
    the equator and microcontinents within 60°; the fine grid's `ocean_margin_start` still forces the polar seas.
    Judged in Task 5 by maps and three numbers (`planet.polar_lost_continental_share ≤ 0.05`, at most 1 seed in 8
    with a belt hugging the fade zone, `margin_drowned_share ≤ 0.05` after Task 7). **Fallback:** a soft wall — a
    plate whose continental centroid is poleward of 60° has the poleward part of its velocity reversed at its next
    redirect — cost if wrong: continents are sliced flat along the margin or the land drifts into it and the
    world ends up sea; the fallback is 20 lines in `plate_sim.rs`.
11. **Tuning is planned work with a cap** — two tuning tasks (5: the plates, on the planet grid; 15: the relief,
    on forged worlds), each at most `4` rounds; a round = change defaults or constants, re-run the 8-seed
    evidence, LOOK at the contact sheets with the image reader, write one line per change in the report. A target
    still missed after the fourth round is recorded as `Ruling: accepted miss — … — …` with its numbers, and the
    phase goes on.
12. **Cortial's `h(z̃)` is the SUBDUCTING plate's relief, as the plan has it** (builder check, Task 2 note 6,
    rejected) — the paper, §4.1, re-read: "the elevation of the plate Pi plunging beneath Pj", `h(z̃_i) = z̃_i²`,
    while the uplift is applied to `P_j` — cost: none.
13. **Hotspot marks do not fold their decayed strength** (`strength = max(strength, HOTSPOT_STRENGTH)`; builder
    check, Task 3 note 6) — a hotspot only writes cells whose kind is `None` or `Hotspot`, never a belt, so Review
    Focus 4's rule (one strength, two decays) has nothing to protect there — cost: none.
14. **Task 14 runs no client test** (builder check, Task 14.1, accepted in substance) — `just smoke` type-checks the
    client, and the generator only enumerates `PreviewLayer::ALL`; the Phase 1a ruling that targeted client tests
    may run without asking is not relied on.

## Small worlds: who assumed kilometre relief (Ruling 3's bill)

| Consumer | On Dev / Tiny after Task 9 | What this plan does |
|---|---|---|
| `game::biome::MOUNTAIN_MIN_M` 1,200 m → `Biome::Mountain`, `RegionType::Mountains` | none (peaks 200–850 m); some on Small; as today on Medium and Large | Known limit + Owner hand check (Task 16); Phase 5 retires the height rule. `mountain_aspect` is the slider |
| Snow cover (only on `Mountain` / `Glacier`, `forged::SNOW_LINE_C`); the score's "snow peaks" | no snow peaks away from the polar ice | Known limit; Task 14 keeps the term and says it reads low |
| The score's "mountain starts" (`mountain_regions_bordering_land`) | 0 | Task 14 keeps it (the `--search` tool is run at the size asked for); Known limit |
| `tile_map::MOUNTAINS_MIN_M` 1,200 / `MOUNTAINS_RELIEF_M` 250 (the world map's terrain class) | Dev tiles class as hills or lowland | Known limit + hand check; no change (`WORLD_MAP_VERSION` stays) |
| Orographic rain (`orographic_scale_m` 650 m) | rain shadows nearly vanish; biome shares move toward the latitude table | Task 9's evidence quotes the five biome shares that moved most; climate is Phase 3's |
| Trees: `(Biome::Mountain, …)` densities and species lists | never read | none needed (falls away with the biome) |
| History: `CAPACITY_RELIEF_PENALTY_PER_100M` (less relief = more capacity) | more people and sites on small worlds | Known limit; Task 16's hand check names it (history cost is not measured by `forge-bench`) |
| `spawn::pick` (a flat dry coast) | unaffected | — |
| The quality gate's `height_p90_m`, `height_p99_m`, `max_peak_m` (about 5× lower on the gate's Dev world) | — | re-recorded once, Task 16 |
| The debug world (`client debug world`, 16 × 8 km) | gentle | Owner hand check |

## Review Focus

1. **A world that changes before Task 7.** `forge` must not call the plate stage in Tasks 1–5, and Task 6's
   stacked column must be bit-identical for one province. The proof is the two baseline diffs and
   `artifact_rasters_crc`, not a reading of the diff.
2. **Crust that appears or vanishes.** Every step, each source cell is moved, consumed (a counted reason) or
   destroyed at a pole, and every destination holds exactly one cell. `planet.continental_area_change` and
   `planet.polar_lost_continental_share` are the watch; a leak shows as continents that shrink over the run.
3. **The seam and the poles in the simulation and in the sampler.** x by `rem_euclid` everywhere (stamp, chamfer,
   flood fills, blur, bicubic taps); z never wraps and never reads out of range. A component cut by the seam must
   count once (`planet.continents`), and `seam_pass_height` must hold after Task 7.
4. **One strength, two decays.** `uplift_now` uses `activity`, elevation and `root_rel` use `root_keep`; a
   re-activated cell folds its decayed strength in before adding (`s·root_keep(age) + gain`). Mixing the two makes
   old belts either flat (both fast) or as high as young ones (both slow).
5. **Rate-space fit and the per-level rebuild.** `World::new` is called once per level: the factor must reach
   EVERY later level's `uplift` AND its `threshold` (both are rates; `World::scale_uplift` scales the two together
   — Ruling 4) and nothing else (not `k_mult`, not `d_mult`, not the lowland share twice), and `upsample_onto` must
   keep taking `uplift`, the masks and the new per-cell fields from the fine world.
6. **The province stack.** A surface `s` outside its band reads another province's rock silently; a first
   formation whose top is not raised to the band ceiling turns the ground above it into the next province's
   basement. The forge clamps the stored `uplifted` into the band (0 cells on Dev; many on Large, from the regional dip — harmless
   only while the two rules on formation tops hold), and the forge-side and runtime rock must agree
   (`province_rock_agrees`).
7. **Costs that hide.** The diffusion sub-step count follows the LARGEST `d_mult` (8 sub-steps at 16 m cells with
   `old_diffusion` 3, against 3 today); the rebound adds two passes every fifth iteration; four per-cell arrays
   add about 13 bytes per cell. Task 16 measures them; none is capped.

---

### Task 1: The planet grid, the supercontinent, plates that move

**Files:**
- Create: `crates/worldgen/src/forge/planet.rs`, `crates/worldgen/src/forge/plate_sim.rs`
- Modify: `crates/worldgen/src/forge/mod.rs` (`pub mod planet; pub mod plate_sim;` — nothing else: `forge_with` is not touched)
- NOT modified: `forge/knobs.rs` — no knob is added before Task 7 (Global Constraints). `PlanetKey::of(p)` fills the five plate numbers from `planet::KEY_DEFAULTS` for now.
- Modify: `crates/worldgen/examples/forge_proto/main.rs` (`--planet`, `--frames <n>`, `--key <name>=<value>` — sets one `PlanetKey` field by its name, refused outside the vocabulary's range for that name; Task 4 adds `--no-retry`; Task 7 deletes `--key`), create `crates/worldgen/examples/forge_proto/planet_view.rs`
- Modify: `crates/worldgen/examples/forge_proto/report.rs` (extract `pub(crate) fn print_summary(all: &[Vec<(String, String)>], seeds: usize)` from `run` — today the summary, seam verdict included, is inline there; `run` calls it; its output is byte-identical, which the baseline diff proves)
- Modify: `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Phase 2, first bullet — the amendment Ruling 1 needs, in this commit: replace "rigid plates as rasters with a transform" by "rigid plates as whole-cell translations of one record per planet cell (no rotation, no resampling)"; and "(512 × 256 to 1,024 × 512, wrapping in x)" by "(512 × 256 on every world size, wrapping in x)") and `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` ("Left to the builder": "the planet grid size (512 × 256 to 1,024 × 512)" becomes "the planet grid is 512 × 256 on every size (Phase 2, Ruling 2)")
- Modify: `justfile` (`forge-planet *args`: `forge_proto --planet {{args}}`, under `forge-report`, with its comment)

**Interfaces — consumes** (verified at `a6fb06f9`): `world::hash01(seed: u64, a: u64, b: u64) -> f64`, `world::smoothstep(lo, hi, x) -> f64`, `crate::noise::OctaveNoise::new(seed, first_octave: i32, amplitudes: Vec<f64>)` / `sample2(x, z) -> f64`, `forge::render::{Rgba, encode_png}`, `Knobs` (the `knobs!` table).

**Interfaces — produces:**

```rust
// planet.rs
pub const PLANET_NX: usize = 512;
pub const PLANET_NZ: usize = 256;
pub const PLANET_CELLS: usize = PLANET_NX * PLANET_NZ;
pub const PLANET_DEG_PER_CELL: f64 = 180.0 / PLANET_NZ as f64;
/// The cell `dx` columns east and `dz` rows south of `c`: x wraps, `None` past a pole.
pub fn planet_step(c: usize, dx: i32, dz: i32) -> Option<usize>;
/// Latitude of row `cz`'s centre, degrees, north positive.
pub fn planet_lat_deg(cz: usize) -> f64;
#[repr(u8)] pub enum CrustKind { Oceanic = 0, Continental = 1 }
#[repr(u8)] pub enum OrogenyKind { None = 0, Arc = 1, Andean = 2, Collision = 3, Plateau = 4, RiftShoulder = 5, Hotspot = 6 }
/// What the plate stage reads: nothing else may reach it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlanetKey { pub seed: u64, pub land_fraction: f64, pub plate_duration_my: f64, pub rift_rate: f64,
                       pub microcontinents: f64, pub belt_decay_my: f64, pub ocean_margin_start: f64 }
/// The five plate numbers until they are knobs (Task 7): land_fraction 0.32, plate_duration_my 450.0,
/// rift_rate 1.0, microcontinents 4.0, belt_decay_my 120.0.
pub const KEY_DEFAULTS: [(&str, f64); 5];
impl PlanetKey { pub fn of(p: &ForgeParams) -> PlanetKey; /* seed and ocean_margin_start from p; the rest KEY_DEFAULTS until Task 7 */ }
/// Degrees east of the west edge / south of the north edge of a cell's centre: ((cx + 0.5) * DEG, (cz + 0.5) * DEG).
pub fn planet_deg(c: usize) -> (f64, f64);
/// A noise on the planet: `OctaveNoise::new(seed ^ salt, -5, vec![1.0, 0.5])` (first wavelength 32 units) sampled
/// at `(lon_deg * 32.0 / wavelength_deg, lat_deg * 32.0 / wavelength_deg)` and made periodic in longitude by
/// `PeriodicX::for_width(360.0).for_noise(wavelength_deg).sample(lon_deg, lat_deg, …)`.
pub struct PlanetNoise { /* … */ }
impl PlanetNoise { pub fn new(seed: u64, salt: u64, wavelength_deg: f64) -> PlanetNoise; pub fn at(&self, lon_deg: f64, lat_deg: f64) -> f64; }
/// The 64-bit finaliser `world::hash01` uses, without its last shift: z = seed ^ a*0x9E3779B97F4A7C15 ^
/// b*0xC2B2AE3D27D4EB4F; z = (z ^ z>>30)*0xBF58476D1CE4E5B9; z = (z ^ z>>27)*0x94D049BB133111EB; z ^ z>>31.
pub fn mix64(seed: u64, a: u64, b: u64) -> u64;

// plate_sim.rs
pub const PLATE_DT_MY: f64 = 1.5;
pub const PLATES_INITIAL: usize = 9;
pub const PLATES_MAX: usize = 24;
pub const NO_PLATE: u8 = 255;
pub struct PlateState { /* the vocabulary's arrays and per-plate vectors, all `pub` */ pub step: u32, pub counters: Counters }
#[derive(Default, Clone, Debug)]
pub struct Counters { pub subducted: u64, pub collided: u64, pub created: u64, pub polar_lost_oceanic: u64,
                      pub polar_lost_continental: u64, pub sutures: u32, pub rifts: u32 }
impl PlateState {
    pub fn initial(key: &PlanetKey) -> PlateState;
    /// One step of `PLATE_DT_MY`: move, resolve overlaps, fill gaps (Tasks 2 and 3 add to it).
    pub fn advance(&mut self, key: &PlanetKey);
    pub fn time_my(&self) -> f64;
}
/// `initial`, then `advance` until `key.plate_duration_my`; `on_frame` sees the state every `every` steps (0 = never).
pub fn simulate(key: &PlanetKey, every: u32, on_frame: &mut dyn FnMut(&PlateState)) -> PlateState;
```

**Pinned, so two builders make the same world.** Coordinates: `(lon, lat_s) = planet_deg(c)` (degrees east, degrees south of the north edge); latitude `lat = 90 − lat_s`; a longitude difference is always the shorter way round (`PeriodicX::for_width(360.0).dx`). Every loop over cells is row-major, every loop over plates ascending id. Random draws are `hash01(key.seed, SALT, index)` with these salts (constants in `plate_sim.rs`) and noises `PlanetNoise::new(key.seed, SALT, wavelength)`:

| Name | Salt | Use |
|---|---|---|
| `N_SHAPE` (noise, 40°) | `0x5C01` | supercontinent and microcontinent outlines, plate-site warp (with `N_SHAPE2`), rift cuts |
| `N_SHAPE2` (noise, 40°) | `0x5C02` | the second warp component |
| `N_BASE` (noise, 25°) | `0x5C03` | `base_m` |
| `N_AGE` (noise, 60°) | `0x5C04` | continental `born_my` |
| `H_SUPER` | `0x5D01` | index 0 `lon0`, 1 `lat0` |
| `H_MICRO` | `0x5D02` | index `8·m + j`, `j` = 0 lon, 1 lat, 2 share, per attempt `+ 3·t` |
| `H_SUTURE` | `0x5D03` | index `4·k + j`: 0 offset along, 1 angle, 2 length |
| `H_SITE` | `0x5D04` | index `2·attempt + j`: 0 lon, 1 lat |
| `H_VEL` | `0x5D05` | index `2·plate + j`: 0 direction, 1 speed |
| `H_OCEAN_AGE` | `0x5D06` | index = cell |
| `H_RIFT`, `H_RIFT_ANGLE` | `0x5D07`, `0x5D08` | index `step · 64 + plate` (Task 3) |
| `H_REDIRECT_T`, `H_REDIRECT_A` | `0x5D09`, `0x5D0A` | index `64 · n + plate`, `n` = that plate id's redirect count so far (Task 3) |
| `H_HOTSPOT` | `0x5D0B` | index `2·k + j`: 0 lon, 1 lat (Task 3) |

**The initial state** (`initial`):
- Supercontinent: `lon0 = 360 · hash01(H_SUPER, 0)`, `lat0 = −30 + 60 · hash01(H_SUPER, 1)`; a cell is in it when `1 − (dlon / (100 · σ))² − ((lat − lat0) / (58 · σ))² + 0.35 · N_SHAPE(c) > 0`. `σ` by bisection on the bracket `[0.2, 3.0]`, 24 halvings, moving the upper end down when the mask holds more than `SUPERCONTINENT_SHARE = 0.26` of the cells, else the lower end up; the mask is that of the final LOWER end.
- Microcontinents: `m` = 0 … `round(key.microcontinents) − 1`, each up to 32 attempts `t`: centre `lon = 360 · h0`, `lat = −60 + 120 · h1`, area share `0.008 + 0.012 · h2`, an ellipse with its long axis east–west, axes ratio 1.5, the half-axes from the share (`π · a · b = share · 64,800` square degrees, `a = 1.5 · b`), outline `1 − (dlon/a)² − (dlat/b)² + 0.35 · N_SHAPE(c) > 0`; an attempt is taken when none of its cells is continental already or within 6 cells (chamfer) of one; after 32 failures the microcontinent is skipped.
- `base_m = CONT_BASE_M (400) + 350 · N_BASE(c)` on continental cells, 0 on oceanic. `born_my`: continental `−600 − 1,900 · clamp(0.5 + 0.5 · N_AGE(c), 0, 1)`; oceanic `−150 · hash01(H_OCEAN_AGE, c)` (so the first step has ocean floor of mixed age and the older-subducts rule has something to compare).
- `ANCIENT_SUTURES = 2` straight segments `k` across the supercontinent: midpoint = the supercontinent's centroid (longitude by circular mean) moved `(−25 + 50 · h0)°` east; direction angle `π · h1`; length `(60 + 50 · h2)°`. A continental cell at distance `d` (degrees, flat, the shorter way round) from the segment, displaced first by `(3 · N_SHAPE(c), 3 · N_SHAPE2(c))` degrees, with `d < 6` gets `kind = Collision`, `strength = max(strength, 0.9 · (1 − (d/6)²)²)`, `orogeny_my = 0`, `fold` = the segment's unit normal — the supercontinent was just assembled, so at `T_END` these belts are as old as the run: every world has an OLD belt class to start from.
- Plates: `PLATES_INITIAL` sites by rejection sampling: attempt `t` = 1, 2, … gives `lon = 360 · h0`, `lat = −90 + 180 · h1` (never past a pole); it is kept when it is at least `PLATE_SITE_GAP_DEG = 33` (today's `0.55 · span / sqrt(count)` in degrees: `0.55 · 180 / 3`) from every kept site, or when `t > 4,000`. A cell belongs to the nearest site (ties: the lower index) from its position displaced by `(8 · N_SHAPE(c), 8 · N_SHAPE2(c))` degrees. Velocity of plate `i`: direction `2π · hash01(H_VEL, 2i)`; speed `(0.15 + 0.30 · hash01(H_VEL, 2i + 1))` cells per step when more than 30 % of its cells are continental, else `(0.40 + 0.60 · …)`.

**One step** (`advance`), in this order:
1. Per live plate: `acc += vel`; `(dx, dz)` = each component of `acc` truncated toward zero (so in {−1, 0, 1}); `acc −= (dx, dz)`.
2. Snapshot per plate, at the start of the step: its cell count and its continental cell count (used for the whole step). Clear the destination arrays (`plate = NO_PLATE`). **Pass A** (row-major over source cells): destination `planet_step(c, dx_p, dz_p)`; `None` → destroyed (`polar_lost_*`); else the source competes for the destination under ONE total order, so the winner never depends on arrival order — continental beats oceanic; between continental cells the one whose plate has the larger snapshot continental count wins (tie: the lower plate id); between oceanic cells the one with the larger `born_my` (the younger) wins (tie: the lower plate id). `winner[dest]` holds the best source seen. **Pass B** (row-major over source cells): a source that is its destination's winner moves its record there; any other is consumed and charged against the WINNER's plate (so with three or more cells on one destination each loser is charged `(winner's plate, its own plate)`): a consumed oceanic cell counts `subducted`, a consumed continental cell `collided`. (Tasks 2 and 3 hook the events in pass B.)
3. One pass over the destinations: a cell still `NO_PLATE` becomes new oceanic crust — `plate` = the plate that held this cell BEFORE the step if its snapshot cell count was above 0; else the plate of the nearest assigned cell found scanning east along the row (x wraps); else (a whole row of gaps) the lowest live plate id. `born_my = time` (the step's end time); `base_m = 0`, `strength = 0`, `kind = None`; `created += 1`.
4. Swap the arrays; `step += 1`.

**The tool** (`planet_view.rs`, `--planet`): for each of `--seeds N` seeds (default 1; seed, seed + 1, …) it runs `simulate` and prints `seed: …` and the `planet.` lines it has (this task: `planet.steps`, `planet.plates_final`, `planet.continental_share_start`, `planet.continental_share_end`, `planet.continental_area_change` (end ÷ start − 1), `planet.ocean_age_p99_my`, `planet.polar_lost_continental_share` (of the starting continental cells), `planet.stage_ms`), then a summary (`report::print_summary`, extracted in this task). With `--out <stem>` it writes contact sheets, one PNG per layer, the seeds side by side in rows of 4, each 512 × 256: `<stem>-planet-<layer>.png` for `plates` (one colour per plate id, continental cells brighter) and `crust-age` (a ramp over 0 … 500 My, continental grey). `--frames n` (one seed) writes `n` evenly spaced `plates` frames `<stem>-planet-t<My>.png`.

- [ ] **Step 0: The baseline, BEFORE any change.** `just forge-report --size dev --cell 32 --seeds 8 > <ledger dir>/baseline-32.txt`, `just forge-report --size dev --cell 64 --seeds 8 > <ledger dir>/baseline-64.txt`; run the gate and quote its pass. The controller notes both files in the ledger.
- [ ] **Step 1: Temporary tests** (`#[cfg(test)] mod tmp` in `plate_sim.rs`; `just test-one worldgen "--lib forge::plate_sim::tmp"`): `planet_step(0, -1, 0) == Some(511)`, `planet_step(5, 0, -1) == None`; `initial` holds 0.255–0.265 of its cells continental plus the microcontinents, every cell has a live plate, exactly `PLATES_INITIAL` plates; two `initial` calls with one key are equal, with `seed + 1` they differ; after one `advance` with every velocity set to `(1, 0)` each record is one column east of where it was (the last column in column 0) and no counter moved; with two plates set head-on over open ocean, after 10 steps `subducted > 0`, `created > 0` and every destination cell has a plate; over 300 steps of a default key `subducted + collided + polar_lost_oceanic + polar_lost_continental == created` (the grid is always full: every cell consumed or lost is matched by exactly one created) and no continental cell was ever consumed by an oceanic one (a counter added for the test and deleted with it); with three plates' cells driven onto one destination the winner and every counter are the same whichever order the source rows are visited in (run pass A over a reversed copy of the source order: same destination arrays).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-planet --seeds 8 --out target/world-forge/p2/t1`: quote the lines of the first seed and the summary; READ `t1-planet-plates.png` and `t1-planet-crust-age.png` and say in two lines what they show (the supercontinent broken into pieces that moved apart? young crust between them?). `just forge-planet --frames 6 --out target/world-forge/p2/t1f`: read the frames. Quote `planet.stage_ms` (no target yet: Tasks 2 and 3 add passes). The safety net (gate + both diffs).
- [ ] **Step 4: Verify, in order** — temporary tests pass; DELETE them; `just smoke`; `just web-check`.
- [ ] **Step 5: Commit** `feat(forge): a plate simulation on the planet grid — crust that moves, subducts and is made (not used yet)`.

### Task 2: Orogeny — subduction fronts, collisions and sutures

**Files:** `crates/worldgen/src/forge/plate_sim.rs`, `crates/worldgen/examples/forge_proto/planet_view.rs`.

**Interfaces — produces** (constants in `plate_sim.rs`, each with its source in its doc):
`SUBDUCTION_REACH_DEG = 16.2` (Cortial `rs`), `SUBDUCTION_PEAK_SHARE = 0.25` (untested: the paper gives no control distance), `SUBDUCTION_U0_M_PER_MY = 600.0` (Cortial `u0`), `TRENCH_M = -10_000.0`, `CONT_MAX_M = 10_000.0`, `FRONT_TTL_STEPS = 8`, `FIELD_EVERY_STEPS = 4`, `ARC_EMERGE_STRENGTH = 0.25`, `SUTURE_CONSUMED_CELLS = 24`, `COLLISION_RADIUS_MAX_DEG = 37.8` (Cortial `rc`), `COLLISION_RADIUS_MIN_DEG = 3.0`, `COLLISION_GAIN_PER_DEG2 = 0.0267` (Cortial `Δc` 1.3·10⁻⁵ km⁻¹ × 12,321 km² per square degree ÷ `OROGEN_HEIGHT_M` 6,000), `PLATEAU_MIN_DEG2 = 300.0`, `COLLISION_BRAKE = 0.5`; `pub fn root_keep(age_my: f64) -> f64`, `pub fn polar_fade(lat_deg: f64) -> f64` (1 away from the poles; Task 3 gives it its ramp — here it returns 1), `pub fn elevation_m(state: &PlateState, c: usize) -> f64` (the vocabulary's law at the state's current time).

**Marks** are per-cell fields of the state, moved with the cell's record in pass B and dropped with it when the cell is consumed or destroyed: `mark_ttl: u8` (0 = no mark), `mark_contact: bool`, `mark_over: u8`, `mark_under: u8`, `mark_speed: f32`, `mark_relief: f32`, `mark_dir: [f32; 2]`. A new mark on a cell overwrites the old one; `ttl` falls as Task 3's step list says. `unit(v)` of a zero vector is `[0, 0]` everywhere in this plan (two plates with equal velocities can still consume a cell when their `acc` phases differ).

**Subduction.** When pass B consumes an oceanic cell of plate `U` whose destination's winner belongs to plate `O`, the WINNER's cell gets a **front mark**: `over = O`, `under = U`, `ttl = FRONT_TTL_STEPS`, `contact = false`, `speed = min(1, |vel_U − vel_O| / 2)` (the relative speed of two plates each capped at 1 cell per step, as a share of the largest possible: the plan's `g`), `relief = z̃²` with `z̃ = (elevation_m(consumed) − TRENCH_M) / (CONT_MAX_M − TRENCH_M)` (Cortial's `h`, of the plate going DOWN: Ruling 12), `dir = unit(vel_U − vel_O)`. Every `FIELD_EVERY_STEPS` steps: a chamfer over the planet grid from the cells with a live front mark — distance 0 there, steps of `PLANET_DEG_PER_CELL` to the 4 edge neighbours and `√2 ·` that to the 4 diagonal ones, a forward row-major pass (west, north-west, north, north-east) then a backward one (east, south-east, south, south-west), x wrapping, the pair repeated until nothing changes (at most 3 pairs); a cell takes a neighbour's distance and source only when STRICTLY smaller, so ties keep what was written first; each cell carries its nearest mark; for a cell `c` with `plate[c] == mark.over` and distance `d` (degrees) below the reach: `gain = (SUBDUCTION_U0_M_PER_MY / 6_000) · f(d) · mark.speed · mark.relief · polar_fade(lat) · (FIELD_EVERY_STEPS · PLATE_DT_MY)`, `f` rising as `smoothstep` over `[0, 0.25 · reach]` and falling as `1 − smoothstep` over `[0.25 · reach, reach]`. If `gain > 1e-5`: `strength ← min(1, strength · root_keep(time − orogeny_my) + gain)` (a never-active cell's old strength is 0), `orogeny_my ← time`, `fold ← mark.dir`, `kind ← Andean` on continental crust, `Arc` on oceanic. An oceanic cell whose `strength` reaches `ARC_EMERGE_STRENGTH` becomes continental crust (`base_m = 0`): an island arc, which can later collide as a terrane.

**Collision.** Each continental-under-continental consumption between plates `(O, U)` (the kept cell's plate, the consumed cell's) adds 1 to `pending[(O, U)]` and leaves a **contact mark** at the destination (`ttl` as above). When `pending[(O, U)] ≥ SUTURE_CONSUMED_CELLS`, a suture, at the end of that step:
- the terrane `R` = the 8-connected continental cells of plate `U` reachable from any cell of `U` that is an 8-neighbour of a live contact mark of the pair (a contact mark is a mark with `contact = true`, `over = O`, `under = U`, written on the winner's cell like a front mark); `A` = its area in square degrees (cells × `PLANET_DEG_PER_CELL²`); its centroid `q` (x by circular mean);
- `r = clamp(COLLISION_RADIUS_MAX_DEG · sqrt(speed · A / A0), 3.0, 37.8)` degrees, `speed` as above for the two plates, `A0 = 64,800 / PLATES_INITIAL`;
- a chamfer distance `d` from `R` (0 inside it); for every continental cell of `O` or of `R` with `d < r`: `Δ = min(1, COLLISION_GAIN_PER_DEG2 · A) · (1 − (d/r)²)² · polar_fade(lat)`; `strength ← min(1, strength · root_keep(age) + Δ)`, `orogeny_my ← time`, `kind ← Plateau` if `A ≥ PLATEAU_MIN_DEG2` else `Collision`, `fold ←` the unit vector from `q` to the cell (shortest way round);
- `R`'s cells go to plate `O`; a plate left with no cell dies; `vel_O ← (vel_O · n_O + vel_U · n_R) / (n_O + n_R)` (continental cell counts) then `· COLLISION_BRAKE`; `vel_U` is unchanged (its ocean floor goes on subducting under the new margin); `pending[(O, U)] ← 0`; `sutures += 1`. Pairs are tested in ascending `(O, U)` order at the end of the step; `fold` at the centroid's own cell is `[0, 0]`.

Doc lines the constants must carry: `COLLISION_GAIN_PER_DEG2 · A` reaches 1 at 37.5 square degrees (about 76 cells), so with `SUTURE_CONSUMED_CELLS = 24` nearly every suture has a full-strength core — in this model a terrane's AREA sets the belt's radius (`r ∝ sqrt(A)`), not its peak; `mark_speed` is a relative speed, not Ruling 1's per-plate cap.

**Tool:** layers `orogeny-kind` (a colour per kind on grey continental / dark oceanic crust), `orogeny-age` (ramp 0 … 500 My where `kind ≠ None`), `strength`; lines `planet.sutures`, `planet.kind_share.<kind>` (share of continental cells per kind, all six), `planet.strength_p50` / `_p90` (over cells with `kind ≠ None`), `planet.orogeny_age_p10_my` / `_p50_my` / `_p90_my`.

- [ ] **Step 1: Temporary tests:** a hand-built state — an oceanic plate driven east into a continental plate at rest for 60 steps: every `Andean` cell is on the continental plate within the reach of the contact, its `fold` points east (x component > 0.9), the strength profile along a row peaks between 2° and 7° from the coast and is 0 past the reach, no oceanic cell of the overriding plate is `Andean`; two oceanic plates head-on with ages 10 and 100 My: the 100 My one is consumed and the other gets `Arc` cells; two continental blocks driven together along a contact 12 cells long: `collided` rises, a suture fires within 40 steps, afterwards one plate holds both blocks, cells near the contact are `Collision` with `orogeny_my` at the suture time, and the smaller block lost at least `SUTURE_CONSUMED_CELLS` and at most `SUTURE_CONSUMED_CELLS + 12` cells (the threshold is tested at the end of a step, and one step consumes at most one cell per contact row); two plates with EQUAL velocities and different `acc` phases that consume a cell leave a mark whose `dir` is `[0, 0]`, never NaN; a block of 700 square degrees gives `Plateau`; a cell with `strength 0.8`, `orogeny_my` 300 My ago that gains 0.1 ends at `0.8 · root_keep(300) + 0.1` within 1e-6.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-planet --seeds 8 --out target/world-forge/p2/t2`: quote the first seed and the summary; READ the three new sheets and say what they show (belts along the leading coasts? any suture belts inside continents? ancient sutures still there?). The safety net.
- [ ] **Step 4: Verify, in order** — as Task 1.
- [ ] **Step 5: Commit** `feat(forge): orogeny in the plate simulation — subduction belts, island arcs, collisions and sutures`.

### Task 3: Rifting, plate reorganisation, hotspots, the polar edges

**Files:** `crates/worldgen/src/forge/plate_sim.rs`, `crates/worldgen/examples/forge_proto/planet_view.rs`.

**Interfaces — produces** (constants, each "untested design" unless a source is given): `RIFT_RATE_PER_MY = 1.0 / 150.0` (a fully continental plate of the initial mean area rifts once per 150 My at `rift_rate` 1; Cortial gives the form, no rate), `RIFT_MIN_CONT_DEG2 = 400.0`, `RIFT_SPEED = 0.35` (cells per step), `RIFT_MARGIN_DEG = 4.0`, `RIFT_MARGIN_DROP_M = 900.0`, `RIFT_SHOULDER_DEG = (2.0, 5.0)`, `RIFT_SHOULDER_STRENGTH = 0.15`, `REDIRECT_MY = (60.0, 120.0)`, `REDIRECT_TURN_DEG = 60.0`, `SLAB_PULL_PER_MY = 0.01`, `HOTSPOTS = 12` (the notes: 10–20 active on Earth, a hobbyist synthesis), `HOTSPOT_STRENGTH = 1.0`, `HOTSPOT_SUBSIDE_MY = 25.0`, `POLAR_FADE_DEG = 12.0`, `SUPERCONTINENT_LAT_DEG = 30.0`, `MICRO_LAT_DEG = 60.0`.

- **Rifting** (end of a step, after sutures; plates in ascending id; at most one new plate per step): a plate whose continental area is at least `RIFT_MIN_CONT_DEG2` rifts when `hash01(seed, RIFT_SALT, step · 64 + plate) < RIFT_RATE_PER_MY · key.rift_rate · PLATE_DT_MY · (A_cont / A0) · (continental share of the plate)`, if a plate id is free. The cut: the line through the plate's continental centroid at a hashed angle, its normal `n`; a cell is on the far side when `(cell − centroid) · n + 4° · noise(cell) > 0` (the 40° noise): those cells get the new plate id. `vel_new = vel + RIFT_SPEED · n`, `vel_old = vel − RIFT_SPEED · n`, each component clamped to ±1; both `acc` zeroed. Continental cells within `RIFT_MARGIN_DEG` of the cut (a chamfer from the cells that have a 4-neighbour on the other side): `base_m −= RIFT_MARGIN_DROP_M · (1 − d / 4°)²` — the passive margin, which the sea-level solve turns into shelf; cells between 2° and 5° with `kind == None`: `kind = RiftShoulder`, `strength = RIFT_SHOULDER_STRENGTH · sin(π · (d − 2) / 3)`, `orogeny_my = time`. `rifts += 1`. The new plate takes the LOWEST id below `PLATES_MAX` that has no cell (a dead plate's id is reused: its `pending` entries are cleared and its redirect count restarts at 0); with none free the rift does not happen. The cut's angle is `π · hash01(H_RIFT_ANGLE, step · 64 + plate)`.
- **Reorganisation**: a plate id's `n`-th redirect comes `60 + 60 · hash01(H_REDIRECT_T, 64 · n + plate)` My after the previous one (the first after `t = 0`, or after the step that created the plate by a rift); at the first step whose time reaches it `vel` turns by `(−60 + 120 · hash01(H_REDIRECT_A, 64 · n + plate))°` and keeps its length. **Slab pull**: every `FIELD_EVERY_STEPS`, a plate that is the `under` of `m > 0` live front marks gets `vel += SLAB_PULL_PER_MY · PLATE_DT_MY · FIELD_EVERY_STEPS · min(1, m / 64) · unit(sum of those marks' dir)`, then each component is clamped to ±1.
- **Hotspots**: `HOTSPOTS` fixed cells, hotspot `k` at `lon = 360 · hash01(H_HOTSPOT, 2k)`, `lat = −70 + 140 · hash01(H_HOTSPOT, 2k + 1)`. Each step the cell under each, if its `kind` is `None` or `Hotspot`: `kind = Hotspot`, `strength = max(strength, HOTSPOT_STRENGTH)` (no fold of the decayed strength: Ruling 13), `orogeny_my = time`. As the plate moves on, the marks left behind age: a chain (islands near the hotspot, seamounts behind: the vocabulary's hotspot term).
- **Polar edges** (Ruling 10): `polar_fade(lat) = smoothstep(0, POLAR_FADE_DEG, 90 − |lat|)`; the initial centres as the constants say. The destroy and fill rules are Task 1's.

**The whole step, in order** (this list is the definition; Tasks 1 and 2 describe its parts): (1) advance each live plate's `acc`, take its whole-cell move; (2) snapshot the plate counts; (3) pass A and pass B — moves, consumptions, counters, front and contact marks, `pending`; (4) gap fill; (5) `time += PLATE_DT_MY`, `step += 1`; every live mark's `ttl −= 1` except those written in this step; (6) hotspots stamp the cells now under them; (7) when `step % FIELD_EVERY_STEPS == 0`: the front chamfer and the subduction gains, then slab pull; (8) sutures, pairs ascending; (9) at most one rift, plates ascending; (10) redirects, plates ascending.

**Tool:** lines `planet.rifts`, `planet.plates_final`, `planet.hotspot_cells` (cells of kind `Hotspot` younger than 30 My), `planet.hotspot_island_cells` (those of them on oceanic crust whose `elevation_m` is above the continental base `CONT_BASE_M` — a stand-in for "above the sea" until Task 4 solves it), `planet.belt_cells_in_polar_fade_share` (cells with `strength ≥ 0.12` and `kind ∈ {Arc, Andean, Collision, Plateau}` within `POLAR_FADE_DEG` of a pole ÷ all such cells).

- [ ] **Step 1: Temporary tests:** with `rift_rate` forced so the first roll passes, one continental plate becomes two whose velocities differ by `2 · RIFT_SPEED` along the cut's normal, the union of their cells is the old plate's, cells on the cut have a lower `base_m` than cells 6° away, and some cells 2–5° from it are `RiftShoulder`; with `rift_rate` at its minimum over 300 steps `rifts` is smaller than with the maximum (8 seeds, summed); a plate moved east at 1 cell per step for 20 steps over one hotspot leaves at least 15 `Hotspot` cells in one row whose `orogeny_my` falls toward the east (the oldest mark has travelled farthest with the plate); `polar_fade(90) == 0`, `polar_fade(78) == 1`, `polar_fade(84)` strictly between; a subduction contact placed at 88° (`polar_fade` 0.074) gives less than a tenth of the strength of the same contact at 40° at equal distance from the front, and one at 84° (0.5) between 0.4 and 0.6 of it; a plate's `vel` has the same length before and after a redirect.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-planet --seeds 8 --out target/world-forge/p2/t3` and `--frames 8` for two seeds: quote every `planet.` line of the first seed and the summary; READ the `plates`, `orogeny-kind` and `orogeny-age` sheets and the frames, and answer in the report: do continents separate and later meet? are there belts of clearly different ages on one map? does anything hug the top or bottom edge? Quote `planet.stage_ms` — design budget 400 ms per run (Ruling 1); above 600 ms say where the time goes (a per-pass timing printed under `--frames`) and fix what is cheap. The safety net.
- [ ] **Step 4: Verify, in order** — as Task 1.
- [ ] **Step 5: Commit** `feat(forge): rifting, plate reorganisation, hotspot chains and polar edges in the plate simulation`.

### Task 4: The planet — elevation, sea level, coast, belts, the score and the re-run

**Files:**
- Modify: `crates/worldgen/src/forge/planet.rs` (`Planet`, `planet_for`, the memo), create `crates/worldgen/src/forge/belts.rs` (`pub mod belts;` in `forge/mod.rs`)
- Modify: `crates/worldgen/examples/forge_proto/planet_view.rs` (`--no-retry`; the layers and lines below), `crates/worldgen/examples/forge_proto/report.rs` (the `planet.` lines join `--report`)
- Modify: `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Phase 2, the scoring bullet — the amendment Ruling 9 needs, in this commit: "Finished worlds are scored and failed seeds re-run with a derived seed (the forge already has a seed score)" becomes "The plate stage's world is scored on the planet grid inside the forge and a failed seed's PLATE STAGE is re-run with a derived seed, up to six attempts; the finished world is compared with that score in the report (`planet_matches_fine`), and the seed search's score gains the same terms")

**Interfaces — produces:**

```rust
// planet.rs
pub const YOUNG_MAX_MY: f64 = 80.0;
pub const OLD_MIN_MY: f64 = 200.0;
pub const BELT_RESIDUAL: f64 = 0.12;
pub const HOTSPOT_DECAY_MY: f64 = 12.0;
pub const PLATE_ATTEMPTS_MAX: u8 = 6;
pub fn activity(age_my: f64, decay_my: f64) -> f64;
pub fn age_weight(age_my: f64) -> f64;
/// Attempt 0 is the seed itself; attempt a > 0 is `mix64(seed, 0x9A7E, a as u64)` (Task 1's `mix64`).
pub fn plate_seed(seed: u64, attempt: u8) -> u64;
pub struct Planet {
    pub key: PlanetKey, pub attempt: u8, pub ok: bool, pub score: belts::PlanetScore, pub sea_level_m: f32,
    pub plate: Vec<u8>, pub crust: Vec<u8>, pub crust_age_my: Vec<f32>, pub thickness_km: Vec<f32>,
    pub kind: Vec<u8>, pub orogeny_age_my: Vec<f32>, pub strength: Vec<f32>, pub fold: Vec<[f32; 2]>,
    pub elevation_m: Vec<f32>,          // relative to the solved sea level
    pub land: Vec<bool>, pub coast_sd_deg: Vec<f32>, pub uplift_now: Vec<f32>, pub root_rel: Vec<f32>,
    pub age_w: Vec<f32>, pub volcanic: Vec<f32>, pub province: Vec<u8>,
    pub continent: Vec<u8>, pub belt: Vec<u16>, pub continents: Vec<belts::Continent>, pub belts: Vec<belts::Belt>,
    pub belt_depth_deg: f64, pub counters: plate_sim::Counters,
    pub stage_ms: f64,      // this attempt: simulate + derive + label
    pub attempts_ms: f64,   // every attempt `planet_for` ran for this key (= stage_ms for `build`)
}
impl Planet {
    /// One attempt: simulate with `plate_seed(key.seed, attempt)`, derive every field, label and score.
    pub fn build(key: &PlanetKey, attempt: u8) -> Planet;
}
/// The planet of `p`: the first of up to `PLATE_ATTEMPTS_MAX` attempts that passes, else the best (Ruling 9).
/// Memoised on the last `PlanetKey`.
pub fn planet_for(p: &ForgeParams) -> std::sync::Arc<Planet>;
/// `planet_for` without the retries and without the memo (the tool's `--no-retry`).
pub fn planet_first_attempt(p: &ForgeParams) -> Planet;

// belts.rs
pub const CONTINENT_MIN_SHARE: f64 = 0.03;
pub const BELT_MIN_REL: f64 = 0.12;
pub const BELT_MIN_CELLS: usize = 10;
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum AgeClass { Young, Middle, Old }
pub struct Continent { pub cells: u32, pub belts: Vec<u16> }
pub struct Belt { pub class: AgeClass, pub kind: u8 /* the commonest */, pub cells: u32, pub continent: u8,
                  pub median_age_my: f32, pub depth_deg: f32 }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanetScore { pub continents: u32, pub continents_without_belt: u32, pub young_belts: u32,
                         pub middle_belts: u32, pub old_belts: u32, pub land_share: f64 }
impl PlanetScore { pub fn ok(&self) -> bool; /* criteria met, 0..=3, for picking the best failure */ pub fn met(&self) -> u32; }
pub fn label(planet: &mut Planet);   // fills continent, belt, continents, belts, belt_depth_deg, score
```

**Derivation** (`build`, after `simulate`), on the planet grid:
1. Ages, `elevation_m` (absolute), `thickness_km` by the vocabulary's laws.
2. **Sea level:** `inside(cz)` = the row's `|rz| = |2 · (cz + 0.5)/256 − 1|` is below `key.ocean_margin_start`. The level `L` = the value such that the count of cells with `inside` and `elevation_m > L` is `round(key.land_fraction · PLANET_CELLS)` (sort the inside cells' elevations). `elevation_m −= L`; `sea_level_m = L`; `land = inside && elevation_m > 0`.
3. **Coast:** `land` smoothed once by a 3 × 3 majority (x wraps; a cell is land when at least 5 of the 9 are, rows past a pole counting as sea) — the smoothed mask REPLACES `land` from here on (continents, belts and the coast all use it; `score.land_share` is its share, so it may differ from `land_fraction` by the smoothing and by ties of equal elevations at the level); `coast_sd_deg` = chamfer distance (Task 2's weights and pass order) (repeat pass pairs as Task 2) from each cell to the nearest cell of the other class, minus half a cell, in degrees, positive on land; then two 3 × 3 box blurs.
4. `uplift_now`, `root_rel`, `age_w`, `volcanic` by the vocabulary; each then one 3 × 3 box blur (x wraps) so a bicubic read has no step to ring on. `province` all 0.
5. `belts::label`: continents (4-connected, x wraps, ≥ 3 % of the cells); belts per age class (vocabulary); a belt's `depth_deg` = the largest chamfer distance from one of its cells to a non-belt cell; `belt_depth_deg` = the largest over young and middle belts (if none: over all; if no belt: 4.0); the score.

**Report / tool lines** (both `--planet` and `--report` print them, from `planet_for(p)`; `--no-retry` from `planet_first_attempt`): `planet.attempt`, `planet.ok` (yes/no), `planet.land_share`, `planet.sea_level_m`, `planet.continents`, `planet.land_components_over_1pct`, `planet.belts.young` / `.middle` / `.old`, `planet.belts` (all belts), `planet.belts_per_continent` (e.g. `3,1,2`, largest continent first; add the key to `NOT_NUMBERS`), `planet.belts_per_continent_by_class` (e.g. `2y0m1o,1y0m0o,1y1m0o`, the same order — the program's "belts per continent by age class"; `NOT_NUMBERS`), `planet.continents_with_2_belts`, `planet.attempts_ms`, `planet.belt_area_ratio` (largest ÷ smallest belt, cells), `planet.belt_depth_deg`, `planet.crc` (crc32 of the bytes of `elevation_m`, `kind`, `strength`; `NOT_NUMBERS`), plus Tasks 1–3's lines. Layers added: `elevation` (hypsometric: blue ramp below 0, green to brown to white above), `land`, `belts` (young red, middle orange, old blue over land grey / sea dark), `uplift-now`.

- [ ] **Step 1: Temporary tests** (`belts.rs` / `planet.rs`): a hand-built `Planet` with two land blobs of 5 % and 2 % of the cells → 1 continent, and with the smaller blob moved across the seam so it is cut by x = 0 it is still ONE component; a blob with a 40-cell strip of `Andean`, `strength 0.5`, age 10 My and a 40-cell strip of `Collision`, age 300 My → one young and one old belt on that continent, `ok` false until a second 3 % continent with a belt exists, then true; `plate_seed(s, 0) == s`, attempts 0 … 5 distinct; `planet_for` twice with one `ForgeParams` returns planets whose `elevation_m`, `kind`, `attempt` are equal (compare by VALUE — the memo is one process-wide entry and another test thread may evict it; the memo's hit is checked once, in a test run alone with `-- --test-threads=1`: the second call's `attempts_ms` is the first's, bit for bit) and `Planet::build(key, planet.attempt)` reproduces its `elevation_m` exactly; `plate_seed(7, 1) == mix64(7, 0x9A7E, 1)`; `planet_for` with another `width_m`, `height_m` and `cell_m` but the same seed and knobs has the same `planet.crc`; the count of cells above the solved level BEFORE the majority pass is `round(land_fraction · PLANET_CELLS)` or more by at most the number of cells tied at the level; `coast_sd_deg` is positive on every land cell that has only land 4-neighbours and negative likewise at sea; `activity(0, 120) == 1`, `activity(1e9, 120) == BELT_RESIDUAL`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-planet --seeds 8 --no-retry --out target/world-forge/p2/t4raw` and `just forge-planet --seeds 8 --out target/world-forge/p2/t4`: quote both summaries (how many seeds pass on attempt 0; which attempt each takes with retries; `planet.stage_ms` with retries). READ the `elevation`, `land` and `belts` sheets. `just forge-report --size dev --cell 32 --seeds 8` and `--size medium --cell 64` (one seed): `planet.crc` of seed 0 is the same on both. No target is enforced here — Task 5 tunes. The safety net (the baselines' lines unchanged; the `planet.` lines only appended).
- [ ] **Step 4: Verify, in order** — as Task 1.
- [ ] **Step 5: Commit** `feat(forge): the planet — sea level by land fraction, continents and belts by age class, a scored re-run of the plate stage`.

### Task 5: Tune the plates on the planet grid; decide the polar edges

No forged world depends on the plate stage yet, so a round costs seconds. At most 4 rounds (Ruling 11).

**Files:** `crates/worldgen/src/forge/plate_sim.rs`, `planet.rs` (`KEY_DEFAULTS` among them), `belts.rs` (defaults and constants only; the soft wall of Ruling 10 if it is needed; `knobs.rs` is still untouched), `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` ("Open questions": the polar-edge answer).

**Targets** (16 seeds: `just forge-planet --seeds 16 --no-retry …` for the raw rate, `--seeds 16` for the result; a default is changed in `planet::KEY_DEFAULTS` — the knobs come in Task 7 with these values — and tried first with `--key`):

| Line | Target |
|---|---|
| `planet.ok` on attempt 0 (`--no-retry`) | at least 10 of 16 |
| `planet.ok` with retries; `planet.attempt` | 16 of 16 (15 with a ruling); median attempt 0, max ≤ 3 |
| `planet.continents` | ≥ 2 on every seed; at least 3 different values over the 16 seeds or a median of 3 (not always the same world) |
| `planet.land_components_over_1pct` | median ≥ 3 |
| `planet.belts.young`, `.old` | each ≥ 1 on every passing seed |
| `planet.continents_with_2_belts` | ≥ 1 on at least 8 of 16 |
| `planet.belts` | median 4–12; no seed above 24 (confetti) |
| `planet.belts_per_continent_by_class` | quoted for every seed (the program's evidence line) |
| `planet.hotspot_island_cells` | above 0 on at least 8 of 16 (some chain reaches the surface) |
| `planet.belt_area_ratio` | median ≥ 3 (ranges of different sizes) |
| `planet.belt_depth_deg` | median 6–20 |
| `planet.continental_area_change` | −0.15 … +0.10 |
| `planet.ocean_age_p99_my` | ≤ 300 (ocean floor is recycled: Tectonics.js's failure) |
| `planet.polar_lost_continental_share` | ≤ 0.05 on every seed |
| `planet.belt_cells_in_polar_fade_share` | ≤ 0.10 on at least 15 of 16 |
| `planet.plates_final` | 6–20 |
| `planet.stage_ms` (one attempt), `planet.attempts_ms` | quoted, with the attempt count; design budget 400 per attempt |

For scale: by this plan's definition Earth has 2 continents above 3 % of its surface (Africa-Eurasia about 16.5 %, the Americas about 8.3 %) and 4 land components above 1 % — areas recalled, not looked up; they only say that "≥ 2 above 3 %" is the program's own bar and not a low one.

**Dials, in the order to try them:** too few continents or one supercontinent at the end → `rift_rate` default, `RIFT_SPEED`, `PLATES_INITIAL`, the initial speeds; confetti → `RIFT_MIN_CONT_DEG2`, `rift_rate` down; no young belt → the speeds, `SUBDUCTION_U0_M_PER_MY`'s effective gain, `FRONT_TTL_STEPS`; no old belt → `ANCIENT_SUTURES`, `ROOT_RESIDUAL`, `BELT_MIN_REL`; belts everywhere → `SUBDUCTION_REACH_DEG` stays (it is Cortial's), lower the gain; old ocean floor → the oceanic speeds, slab pull; land at the poles → `SUPERCONTINENT_LAT_DEG`, then the soft wall.

- [ ] **Step 1: Round 0** — run both commands at the committed defaults; fill the table; READ the `plates`, `elevation`, `belts`, `orogeny-age` sheets of all 16 seeds and `--frames 8` of the two worst.
- [ ] **Step 2: Rounds 1–4** — one change set per round, the table and one line per change after each. Stop at the first round with every target met.
- [ ] **Step 3: The polar edges** — state the verdict of Ruling 10 from the last round's numbers and sheets: kept as proposed, or the soft wall added (then one more run of the table). Amend the design's open question with the answer and its numbers.
- [ ] **Step 4: Misses** — every target still missed: `Ruling: accepted miss — <line, numbers> — <why further tuning is not expected to help> — <cost>`.
- [ ] **Step 5: Verify** — the safety net (worlds still untouched); `just smoke`; `just web-check`.
- [ ] **Step 6: Commit** `tune(forge): the plate simulation's defaults — continents, belts by age, polar edges` (the message carries the final table's summary line per target).

### Task 6: Store the planet record and the province container — the one format bump

**At dispatch the controller tells the owner:** once this commit lands, `worlds/debug-world`, every `just play` world and every dev store refuse to load until re-forged (`just run --reset`, `just pg-reset`).

**Files:**
- Modify: `crates/worldgen/src/forged.rs`: `ARTIFACT_VERSION` 10 → 11 (doc: "v11: `planet` (the plate record), `province` per cell, `columns` (a rock column per province)"); `WorldArtifact`: `column: Vec<(f32, u8)>` becomes `columns: Vec<Vec<(f32, u8)>>`, new `province: Vec<u8>` (after `savagery`), new `planet: PlanetRecord` (the LAST field, after Phase 1b's `climate`); `WorldGlobals` (`column` → `columns`); `ByteRasterId::Province` appended (`ALL` becomes 6 long; `byte_rasters` / `byte_rasters_mut` return 6); `validate_tables` (takes `&[Vec<(f32, u8)>]`); `WorldArtifact::validate`; `Rasters` (new `planet()`); `ForgedWorld` (`geo_offset_blocks`, `strata_blocks`, `surface_rock`, new `province_at`, `planet`, `planet_cell`)
- Modify: `crates/worldgen/src/forged_tiles.rs`: `TILED_FORMAT_VERSION` 3 → 4 (doc: "v4: `Tile::bytes` holds six rasters (`Province`); `ForgedMeta::planet`"); `Tile::bytes: [Vec<u8>; 6]`; `ForgedMeta { …, pub planet: PlanetRecord }` (last); `ForgedMeta::validate`; `split`; `encode_meta` / `decode_meta`; `encode_tile` / `decode_tile`; `TileCache` (holds the record beside the climate container: `TileCache::new` gains it as an argument after `climate`)
- Modify: `crates/worldgen/src/forge/mod.rs` (`Forged::artifact` fills the three new fields; it calls `planet::planet_for(p)` — `forge_with` still does not)
- Temporary test only (no committed change expected): `crates/world-store/src/forged.rs` (`load_forged`, `StoreError` live there; `just test-one world-store "--lib forged::tmp"`)
- Modify: `crates/worldgen/src/tile_map.rs` (the two readers of `a.rock_names` / `surface_rock` compile against the new shapes; no logic change, so `WORLD_MAP_VERSION` stays 3 — say so in the report)
- Modify: `crates/worldgen/examples/world_map.rs` (`ForgedLayer` gains `Crust`, `OrogenyKind`, `OrogenyAge`, `Province`: names `crust`, `orogeny-kind`, `orogeny-age`, `province`; window = the whole world unless `--span` / `--center` are given, as Phase 1b's monthly layers), `crates/worldgen/examples/forge_proto/report.rs`, `justfile` (the `world-map` comment)
- Modify: `CLAUDE.md` (Worldgen paragraph: the planet record, where it lives, the province byte and the stacked columns, the versions; stage only your hunks), `TODO.md` (re-forge every dev world, the public web world, dev Postgres worlds, the twin and orion)

**Interfaces — produces** (in `forged.rs`):

```rust
/// The plate simulation's record on the planet grid (`forge::planet`), row-major, x wraps. Resident.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PlanetRecord {
    pub nx: u32, pub nz: u32,
    /// Which attempt of the plate stage made this world (0 = the seed itself) and whether it passed the score.
    pub attempt: u8, pub ok: bool,
    /// The solved sea level on the model's height scale (m).
    pub sea_level_m: f32,
    pub plate: Vec<u8>,
    pub crust: Vec<u8>,              // forge::planet::CrustKind
    pub crust_age_my: Vec<u16>,      // saturating
    pub thickness_half_km: Vec<u8>,  // 0.5 km steps
    pub orogeny_kind: Vec<u8>,       // forge::planet::OrogenyKind
    pub orogeny_age_my: Vec<u16>,    // u16::MAX = never
    pub strength: Vec<u8>,           // 255 per 1
    pub fold_east: Vec<i8>, pub fold_south: Vec<i8>,   // 127 per 1; (0, 0) = none
    pub elevation_m: Vec<i16>,       // model metres above the solved sea level, saturating (deep bathymetry)
}
impl PlanetRecord {
    pub fn from_planet(p: &crate::forge::planet::Planet) -> PlanetRecord;
    /// The cell holding map fractions (fx, fz): x wraps by index, z clamps. Integer and floor arithmetic only.
    pub fn cell_at(&self, fx: f64, fz: f64) -> usize;
}
pub const PROVINCE_MAX: usize = 5;
pub const PROVINCE_BAND_BLOCKS: i32 = 8_192;
/// A province's band of the stratigraphic coordinate reaches this far above its shift (m = blocks).
pub const PROVINCE_BAND_TOP_S: i32 = 800;
/// Province 0 (the platform) is not shifted; the others alternate above and below it.
pub const PROVINCE_SHIFT_BLOCKS: [i32; PROVINCE_MAX] = [0, 8_192, -8_192, 16_384, -16_384];
pub fn province_shift_blocks(province: u8) -> i32;   // PROVINCE_SHIFT_BLOCKS[province as usize]
impl ForgedWorld {
    pub fn planet(&self) -> &PlanetRecord;
    /// The planet cell under engine metres (x, z).
    pub fn planet_cell(&self, x_m: f64, z_m: f64) -> usize;
    /// The geological province of the block column at engine metres (x, z): the stored byte of the cell
    /// nearest the DITHERED point (`dithered`, the biome border's displacement).
    pub fn province_at(&self, x_m: f64, z_m: f64) -> u8;
}
```

- `fn validate_planet(r: &PlanetRecord) -> Result<(), ArtifactError>` (crate-private): `nx`, `nz` equal the build's `PLANET_NX`, `PLANET_NZ`; every plane is `nx · nz` long; every `crust` < 2 and `orogeny_kind` ≤ 6; errors are `ArtifactError::Format("planet record: …")`. Called by `WorldArtifact::validate`, `ForgedMeta::validate` and by `decode_meta` itself.
- `validate_tables`: 1 ≤ `columns.len()` ≤ `PROVINCE_MAX`; each column non-empty, sorted by descending top, rocks known, its first top ≤ `PROVINCE_BAND_TOP_S`, every later top below `PROVINCE_BAND_TOP_S − UNDERGROUND_DEPTH − 8` and its last top above `PROVINCE_BAND_TOP_S − PROVINCE_BAND_BLOCKS + UNDERGROUND_DEPTH + 8` (today's column: first top 700, basement top −1,500 by default, −4,500 at the knob's limit); and (artifact / tile side) every `province` byte < `columns.len()`.
- **The stack.** `strata_blocks(mats)`: for the provinces present, in order of DESCENDING shift (3, 1, 0, 2, 4), each column's formations with `top = round(top_s) + province_shift_blocks(p)`, the column's FIRST formation's top replaced by `PROVINCE_BAND_TOP_S + province_shift_blocks(p)` (the top formation extends up to the band's ceiling); the result is sorted by descending top by construction; every top is asserted to fit `i16`. With one province (shift 0) the list differs from today's only in its first top (700 → 800), which no lookup can tell apart (above the first formation everything reads it), and every `geo` is today's value exactly. `geo_offset_blocks(x, z)`: `round(geo_offset_m) + (SEA_TOP_BLOCK + 1) − province_shift_blocks(province_at(x, z))`, clamped to `i16` as today. `surface_rock(x, z)`: the province's own column (`globals.columns[province_at(x, z)]`) looked up at `s = bicubic − geo_offset_m` with today's rule. `lib.rs` (`strata_rock`, `footprint_strata`, the `ColumnGround` construction) and `game` are NOT changed.
- **The forge side** (`Forged::artifact`): `columns: vec![self.geology.column()…]` (one province: the platform, index 0), `province: vec![0; cells]`, `planet: PlanetRecord::from_planet(&planet::planet_for(p))`. Before quantising `uplifted`: per land cell clamp `uplifted` so that `s = h − uplifted − bedding` lies in `(PROVINCE_BAND_TOP_S − PROVINCE_BAND_BLOCKS + UNDERGROUND_DEPTH + 8, PROVINCE_BAND_TOP_S − 8)` = (−7,324, 792); the count of cells it moved is the report's `province_band_clamped_cells`. The clamp changes NO rock: above a column's top every `s` reads its top formation and below its basement top its basement, and the bounds leave `UNDERGROUND_DEPTH` (60 blocks) of the same formation under a clamped surface (hence the two rules on tops in `validate_tables`). It is needed because `bedding`'s regional dip alone spans ±2,000 m on a Large world (`regional_dip` 0.04 × half of 100 km): there the count is large and expected; on Dev (dip ±164 m) at the default knobs it must be 0, which is what keeps `artifact_rasters_crc` unmoved in this task (a run with far more iterations exhumes past the band on any size).
- `artifact_rasters_crc` (Phase 1b's line) feeds `columns[0]` where it fed `column`, and nothing new: with one province the line does not move. New lines: `artifact_planet_crc` (crc32 of `bincode` of `planet`; `NOT_NUMBERS`), `planet_bytes_packed` (the record alone through bincode + zstd at the file's level), `province_band_clamped_cells`, `strata_stack_len` (`strata_blocks(..).len()`), `strata_footprint_max` (the longest `ColumnGround::strata` over the 4 + 8 regions `runtime_seam` already generates), `planet_tiles_identical` (a tiled world from `split` → encode → decode → `MemTiles`: `planet()` equals the whole world's and `province_at` agrees at 4,096 points — yes/no).
- **An older world** is refused exactly as Phase 1b's Task 3 lists (file: `OldVersion { found: 10 }`; menu: `check_terrain`; store: `load_forged` on the `format_version` column; `worldctl retile`: the archive does not load) — two constants changed, no new code; verify, do not rebuild.

- [ ] **Step 0: Old-format worlds, made BEFORE the change:** `just forge-proto --size dev --cell 64 --out target/world-forge/pre-p2` and `just region_blocks=64 worldctl forge --db target/world-forge/pre-p2.sqlite --size dev --cell 64`; quote both last lines. Keep one dev / 64 m region's generated columns for Step 1: a temporary test that writes the block ids of 4 fixed regions of the default seed to `target/world-forge/pre-p2-blocks.bin` (run it once now, at the old code).
- [ ] **Step 1: Temporary tests:** `WorldArtifact::from_bytes(&a.to_bytes()) == a` (compare with `==`, print one line on a mismatch); `a.planet.nx == 512`, `a.columns.len() == 1`, every `province` byte 0; `split` then `decode_meta(&encode_meta(&m)) == m`, and a `ForgedWorld::from_tiles` over the encoded tiles has the same `planet()`; the 4 regions of Step 0 generate the SAME block ids as `pre-p2-blocks.bin` (the stack with one province is bit-identical); on a hand-built two-province artifact (province 1 on the east half, its column all basalt): `surface_rock` west of the border is the first column's rock and east of it basalt, `strata_blocks` is sorted by descending top, a `ColumnGround` generated across the border reads the right rock on each side at three depths, and every `geo` fits `i16` with heights up to 3,500 m and `uplifted` up to 7,000 m; a `province` byte of 3 with two columns fails `validate` with text containing `province`; a record with one plane truncated fails with `planet record`; bytes with version 10 → `OldVersion` with `re-forge`; in `world-store`: `load_forged` on a COPY of `pre-p2.sqlite` is `Err(StoreError::OtherFormat { kind: FormatKind::Tiles { archived: false }, found: 3 })`; the `geo` of every column of the 4 regions is EQUAL to the old code's (write them into `pre-p2-blocks.bin` beside the block ids).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-proto --size dev --cell 32 --out target/world-forge/post-p2`; `just world-map --world target/world-forge/post-p2.world --layer orogeny-kind --out target/world-forge/post-p2` and the same for `crust`, `orogeny-age`: READ them (they show the plate stage's world, which the terrain does not follow yet — say so). `just world-map --world target/world-forge/pre-p2.world --tiles --out target/world-forge/x` stops with the `OldVersion` text: quote it. `just forge-report --size dev --cell 32 --seeds 8`: quote the new lines; `province_band_clamped_cells: 0` (Dev), `planet_tiles_identical: yes`, `strata_stack_len` equal to the column's length. The safety net: gate + both diffs, `height_crc` and `artifact_rasters_crc` equal on every seed; `world_file_bytes` is the ONE line allowed to move (quote old → new per seed, and `planet_bytes_packed` beside it).
- [ ] **Step 4: Verify, in order** — temporary tests pass; `just test-one worldgen "--lib -- forged:: forged_tiles:: world_store::"`; DELETE the temporary tests and `pre-p2-blocks.bin`'s writer; `just smoke`; `just web-check`.
- [ ] **Step 5: Commit** `feat(worldgen)!: the plate record and a geological province per cell in the world file and the tiled store (artifact v11, tiles v4)`. The message says every existing world must be re-forged.

### Task 7: The switch — land, uplift and volcanoes from the planet

From this commit every forged world is a different world. The gate fails until Task 16 (Ruling 8).

**Files:**
- Modify: `crates/worldgen/src/forge/knobs.rs`: ADD `land_fraction`, `coast_wander_deg`, `coast_detail_deg` ("Land") and `plate_duration_my`, `rift_rate`, `microcontinents`, `belt_decay_my` ("Plates") with the vocabulary's defaults — replaced by Task 5's tuned `KEY_DEFAULTS` where that task moved one — and ranges; DELETE `coast_noise_lo`, `coast_noise_hi`. `planet::KEY_DEFAULTS` and the tool's `--key` are deleted; `PlanetKey::of` reads the knobs; `--set` replaces `--key` in every command from here on
- Modify: `crates/worldgen/src/forge/world.rs`: `World::new` → `World::new(p: &ForgeParams, planet: &Planet)` and `World::new_at(p, planet, sea_stand: f64)`; `ContinentalShape` reads the planet (its `mass` field and `ContinentalMass` are no longer used — Task 8 deletes them); `volcano_sites(p, planet)`; `World::upsample(&self, p_fine, planet)`
- Modify: `crates/worldgen/src/forge/planet.rs` (`Sampler`; `Warp` and `ALONG_STRIKE_VARIATION` — MOVED here from `plates.rs` with the three noises, the warp's amplitude now `PLANET_WARP_DEG`), `crates/worldgen/src/forge/mod.rs` (`forge_with` computes `planet_for(p)` once after `check_wrap` and hands it to every `World::new`; `Forged` gains `pub planet: Arc<Planet>`; `Forged::artifact` uses it; the `initial` literal), `crates/worldgen/src/forge/fields.rs` (`compute`, `volcanism`, `volcanism_grid`, `volcano_sites_on`, `volcanoes`, `cone`)
- Modify: `crates/worldgen/examples/forge_proto/report.rs` (Task 9 adds the belt lines; here `margin_drowned_share`, `planet_matches_fine`), `crates/worldgen/examples/forge_proto/serve.rs` and `main.rs` only if a signature they call changed
- Modify: `CLAUDE.md` (Worldgen paragraph: land and belts come from `forge/planet.rs`; the plate stage is re-run on a failed score; `just forge-planet`), `TODO.md` (one entry: "the forge quality gate (`forge_quality_matches_baseline`) is expected RED from this commit until Phase 2's closing re-record — do not bless it; `just test-full` shows it failing")

**Interfaces — consumes:** `World::new(p: &ForgeParams) -> World`, `ContinentalShape::{new, at}`, `volcano_sites(p) -> Vec<[f32; 2]>`, `fields::volcano_sites_on(p, eligible)`, `fields::volcanism(seed, w, plates: &PlateField, k)`, `fields::volcanism_grid(seed, (width_m, height_m), (nx, nz), plates, k, wrap)`, `fields::cone(sites, mx, mz)`, `fields::VolcanismNoise::at(mx, mz, convergence, k)`, `plates::Warp` (private), `PeriodicX::{for_world, for_noise, sample, with_images, dx}`, `par::map_range`, `climate::metres_per_degree`.

**Interfaces — produces:**

```rust
// planet.rs
pub const PLANET_WARP_DEG: f64 = 1.5;
/// Reads of a `Planet` at map fractions; x wraps by index, z clamps.
pub struct Sampler<'a> { /* planet, the Warp, metres per degree, width, height */ }
impl<'a> Sampler<'a> {
    pub fn new(planet: &'a Planet, p: &ForgeParams) -> Sampler<'a>;
    pub fn coast_sd_deg(&self, mx: f64, mz: f64) -> f64;      // bicubic, unwarped
    pub fn uplift_now(&self, mx: f64, mz: f64) -> f64;        // bicubic at the warped point, clamped 0..=1, times the along-strike factor
    pub fn root_rel(&self, mx: f64, mz: f64) -> f64;          // bicubic at the warped point, clamped
    pub fn age_w(&self, mx: f64, mz: f64) -> f64;             // bilinear at the warped point
    pub fn volcanic(&self, mx: f64, mz: f64) -> f64;          // bilinear at the warped point
    pub fn kind(&self, mx: f64, mz: f64) -> u8;               // nearest at the warped point
    pub fn province(&self, mx: f64, mz: f64) -> u8;           // nearest at the warped point
    pub fn cell(&self, mx: f64, mz: f64) -> usize;            // nearest, unwarped (the continent label)
    pub fn belt_cell(&self, mx: f64, mz: f64) -> usize;       // nearest at the warped point (the belt label: where uplift_now was read)
}
// world.rs
impl World {
    pub fn new(p: &ForgeParams, planet: &Planet) -> World;                       // new_at(p, planet, 0.0)
    pub fn new_at(p: &ForgeParams, planet: &Planet, sea_stand: f64) -> World;    // Ruling 6: the hook for Phase 4
}
pub fn volcano_sites(p: &ForgeParams, planet: &Planet) -> Vec<[f32; 2]>;
```

**The new initial condition** (everything not named stays as it is):
- `ContinentalShape::at(mx, mz)`: `shape = (sampler.coast_sd_deg(mx, mz) / LANDNESS_UNIT_DEG).min(CONTINENT_INTERIOR_MASS) + (k.coast_wander_deg · lo + k.coast_detail_deg · hi) / LANDNESS_UNIT_DEG` (`planet::LANDNESS_UNIT_DEG = 90.0`; the same `shape_lo`, `shape_hi` noises, periodic as today — their wavelengths, 8,192 m and 1,024 m at the reference size, are 92° and 11.5° on every world); the polar margin term and the returned pair `(shape, land)` as today. A cell is sea when `land ≤ sea_stand`. Sea floor (`160 · shape − 1`, floored at −90), shoals, `margin_drowned`, `coast_taper = smoothstep(0, 0.05, land)`, the land ramp `smoothstep(0, 0.5, land)` and `VOLCANO_MIN_LANDNESS`: today's formulas and constants, UNCHANGED — the unit is today's, so each keeps its width (the vocabulary's table). `CONTINENT_INTERIOR_MASS` stays (Task 8 does not delete it).
- Land height: today's `(1.0 + 45.0 · smoothstep(0.0, 0.5, land) + 3.0 · rough_at).max(0.5)` — Task 10 adds the root.
- Uplift: `belt = sampler.uplift_now(mx, mz)` in place of `(field.convergence[i]).max(interior)`; the along-strike factor (`1 − ALONG_STRIKE_VARIATION · …`, today inside `plate_field`) is applied inside `Sampler::uplift_now`; the rest of the expression is today's (`coast_taper`, `lowland_uplift_share`, `belt_uplift`, `fields::cone`).
- `Warp::at` keeps its two 8 km noises (scaled by `span_m / REFERENCE_SIZE_M`, periodic) with `amplitude = PLANET_WARP_DEG · metres_per_degree(height_m)`.
- Volcanoes: `VolcanismNoise::at`'s `convergence` argument becomes `sampler.volcanic(mx, mz)` (the `VOLCANISM_FLANK` / `_CORE` / `_SPOT` constants keep their meaning: hot spots on active arcs and over hotspots); `volcanism_grid` and `volcanism` take `&Sampler` in place of `&PlateField`; `volcano_sites_on` no longer calls `plate_field`. `VOLCANO_SPACING_M` and `VOLCANO_CONE_RADIUS_M` become `fields::volcano_spacing_m(height_m) = VOLCANO_SPACING_DEG (8.0) · metres_per_degree` and `fields::volcano_cone_radius_m(height_m) = (4.0 · metres_per_degree).clamp(250.0, 3_000.0)`; `cone` takes the radius, and `fields::volcanoes(seed, volcanism, eligible, (nx, nz), (cell_x, cell_z), k, wrap)` gains a `spacing_m: f64` argument in place of the constant it reads today.
- `forge_with`: `let planet = planet::planet_for(p);` once; the three `World::new` sites and `fields::compute` take it. The `initial` literal (`let initial = World { … ..fine }`) is unchanged here — Tasks 12 and 13 add `Vec` fields to `World` and must list them in it.
- `--report`: `margin_drowned_share` (`World::margin_drowned` of the INITIAL fine world ÷ land cells), `planet_matches_fine` (yes when `planet.continents` equals the existing `land_components_over_3pct`).

- [ ] **Step 0: Before** — save `just forge-report --size dev --cell 32 --seeds 8 > <ledger dir>/t7-before.txt` and the gate's pass.
- [ ] **Step 1: Temporary tests:** `Sampler` on a hand-built planet whose `coast_sd_deg` is `+3` on the west half and `−3` on the east: the read is positive at 10 % of the width and negative at 60 %, and equal at `mx` and `mx + width_m` and `mx − width_m`; a read at `mz = −50` equals the read at `mz = 0`; `uplift_now` of a planet with one cell at 1 and the rest 0 stays within `[0, 1]`; on a forged dev / 64 m world of the default seed: `w.ocean` is false on at least 90 % of the fine cells whose planet cell is `land` with `coast_sd_deg > 12`, and true on at least 90 % of those with `coast_sd_deg < −12` (the two noises move the coast by up to about 9°); `World::new` at 128 m and at 64 m agree on `ocean` for at least 97 % of the coarse cells' centres (the coast is the same world at two cells); `World::new_at(p, planet, −0.74)` has strictly more land than `new_at(…, 0.0)`; the volcano sites lie on land and at least `volcano_spacing_m` apart the shorter way round; on a planet with one young `Hotspot` cell on land and no arc, `volcanism` reaches 100 somewhere within 2° of it (the 0.5 is not damped); `shape` never exceeds `1 + (20 + 6) / 90 · 1.5` and every cell of rows 0 and `nz − 1` is sea with both `coast_*_deg` at their maxima.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-report --size dev --cell 32 --seeds 8 > <ledger dir>/t7-after.txt`: a before / after table of `land_share`, `land_components_over_1pct`, `land_components_over_3pct`, `max_peak_m`, `river_mouths`, `lakes`, the five biome shares that moved most, `planet_matches_fine`, `margin_drowned_share`; the constraints of the second regime (seam, polar rows, drainage, aliases, map wrap) — all hold. Judged: `land_share` 0.26–0.38 on every seed; `land_components_over_3pct ≥ 2` on at least 6 of 8; `planet_matches_fine: yes` on at least 6 of 8; `margin_drowned_share ≤ 0.05` on every seed (Ruling 10's number). A miss is tuned here with `coast_wander_deg`, `coast_detail_deg`, `land_fraction` (defaults) or ruled — Task 15 tunes the look, this task only has to hand it a world inside these bounds. `just forge-proto --size dev --cell 32 --out target/world-forge/p2/t7` and `just world-map --world target/world-forge/p2/t7.world --tiles --out target/world-forge/p2/t7-map`: READ the relief and biome maps beside `--layer orogeny-kind`: do the ranges stand where the belts are? The gate, run without bless: quote its message.
- [ ] **Step 4: Verify, in order** — temporary tests pass; DELETE them; `just smoke`; `just web-check`.
- [ ] **Step 5: Commit** `feat(forge)!: continents, mountain belts and volcanoes come from the plate simulation`.

### Task 8: Delete the static plates, the continents formula and their knobs

Dispatched immediately after Task 7. It changes no world: the per-seed `height_crc` of `t7-after.txt` must be reproduced.

**Files:**
- Delete: `crates/worldgen/src/forge/plates.rs` (`plates`, `Plate`, `PlateField`, `plate_field`, `Boundary`, `convergent_boundaries`, `segment_distance`, `belt_profile`, `InteriorRange`, `interior_ranges`, `interior_strength`, every constant in it — `Warp` and `ALONG_STRIKE_VARIATION` already moved in Task 7); `pub mod plates;` in `forge/mod.rs`; the module header of `forge/mod.rs` and of `examples/forge_proto/main.rs` (pipeline text)
- Modify: `forge/world.rs`: delete `ContinentalMass`, `edge_proximity`'s use by it (keep the function: the margin reads it), `COAST_DISTANCE_VARIATION`, `CONTINENT_CENTRE_BOX`, `CONTINENT_CENTRE_GAP`, `ContinentalShape::mass` (`CONTINENT_INTERIOR_MASS` stays: Task 7's cap)
- Modify: `forge/knobs.rs`: delete `continent_count`, `continent_sea_width`, `continent_coast`, `plate_count`, `plate_belt_width`, `belt_length_km`, `interior_ranges`, `Knobs::plate_total`; the help texts of `belt_uplift`, `volcano_count_max`, `volcanism_strength` say what they do now
- Modify: `forge/presets.rs` ("Earth-like": the description; "Great ranges" and "Highlands": the `plate_belt_width` override removed), `crates/panel/src/forge.rs` (only if it names a deleted knob; `PRESETS` names are unchanged), `crates/client/src/frontend/generator.rs` (`TABS`: "Plates" stays under "Mountains"; a key override only if a deleted knob is named), `crates/world-store/src/ctl.rs` (`USAGE`, if it names one), `crates/worldgen/examples/forge_proto/{serve.rs,ui.html,main.rs}` (any deleted name)
- Modify: `justfile` (comments naming plates or continents counts), `CLAUDE.md`, `TODO.md`

**Interfaces:** stored knobs of a deleted name are ignored on load (`knobs_from_map` skips unknown keys — verify, no change).

- [ ] **Step 1: Confirm by grep** that no reader of each deleted name is left (`rg -n "<name>"` over `crates/`, `justfile`, `CLAUDE.md`, `TODO.md`): list each hit and what happened to it.
- [ ] **Step 2: Temporary tests:** `SPECS` holds none of the seven names; every preset override names an existing knob in range (`presets::apply` in a debug build asserts it: call it for all six); `knobs_from_map` of a map holding `plate_count = 7` equals the defaults.
- [ ] **Step 3: Implement** the deletions.
- [ ] **Step 4: Evidence** — `just forge-report --size dev --cell 32 --seeds 8`: `height_crc` per seed equals `t7-after.txt`'s (the deletion changes no world). `just test-one worldgen "--no-run --tests --examples"` compiles with no warning naming a deleted symbol (quote its tail); the whole-workspace `just clippy` is Task 16's.
- [ ] **Step 5: Verify, in order** — temporary tests pass; DELETE them; `just smoke`; `just web-check`.
- [ ] **Step 6: Commit** `refactor(forge)!: the static plates, the continents formula and their seven knobs are gone`.

### Task 9: The relief scale — the peak target, the rate-space fit, the soft knee; the belt evidence lines

**Files:** `crates/worldgen/src/forge/post.rs` (`fit_height_band`, new `fit_uplift_rate`), `forge/mod.rs` (`forge_with`: the fit after the coarsest level, and — when `!p.quiet` — one printed line with the target and the two fits; `Forged` gains `pub uplift_fit: f64`, `pub peak_target_m: f64`), `forge/planet.rs` (`peak_target_m`, `UPLIFT_START_M_PER_YR`), `forge/world.rs` (the rate), `forge/knobs.rs` (ADD `mountain_aspect` in "Uplift"; DELETE `uplift`; rewrite the help of `max_peak_m`, `belt_uplift` ("a belt's uplift rate as a multiple of the lowlands' …"), `lowland_uplift_share`, `erodibility` ("how far the run gets toward its steady state and how dissected the land is; height is set by the mountain aspect"), `iters`), `forge/presets.rs` ("Great ranges": `uplift` → `mountain_aspect` 0.9; "Highlands": `uplift` → `mountain_aspect` 0.8; "Lowlands": `uplift` → `mountain_aspect` 0.35 — each description re-read against what its overrides now do), `crates/worldgen/examples/forge_proto/main.rs` (the `--uplift` flag is deleted with the knob), `crates/worldgen/examples/forge_proto/report.rs`, and the amendment Ruling 3 needs, in this commit: `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md`, Decision 3, after "A Tiny world has belts a kilometre or two wide and so lower mountains.": "Since Phase 2 a world's highest range is at most `mountain_aspect` (0.6) times as high as its belt is deep: about 200–550 m on the development size, 350–850 m on Tiny, 0.7–1.7 km on Small, 1.3–2.8 km on Medium, up to `max_peak_m` on Large."

**Interfaces — produces:**

```rust
// planet.rs
pub const PEAK_TARGET_MIN_M: f64 = 150.0;
/// clamp(mountain_aspect * planet.belt_depth_deg * metres_per_degree(height_m), PEAK_TARGET_MIN_M, max_peak_m)
pub fn peak_target_m(planet: &Planet, p: &ForgeParams) -> f64;
// post.rs
pub const FIT_AIM: f64 = 0.9;
pub const FIT_RANGE: (f64, f64) = (0.25, 4.0);
pub const FIT_DEADBAND: f64 = 0.05;
pub const FIT_KNEE_SHARE: f64 = 0.85;
// planet.rs: the rate every level starts from before the two scales (was the `uplift` knob's default).
pub const UPLIFT_START_M_PER_YR: f64 = 5.0e-3;
/// After the coarsest level: scales land `h` and `uplifted` by f = clamp(FIT_AIM * target / peak, FIT_RANGE)
/// unless |f - 1| <= FIT_DEADBAND or the world has no land (returns 1.0 = untouched). The caller scales later
/// levels' uplift by it.
pub fn fit_uplift_rate(w: &mut World, peak_target_m: f64) -> f64;
/// The final fit: heights above FIT_KNEE_SHARE * max_peak_m are compressed toward max_peak_m (monotone);
/// returns new peak / old peak (1.0 = untouched). `uplifted` is lowered by the same amount per cell.
pub fn fit_height_band(w: &mut World, k: &Knobs) -> f64;
// world.rs
impl World { pub fn scale_uplift(&mut self, f: f64); }   // every cell's `uplift` times f (and, from Task 12, its `threshold`)
```

- `forge_with`: `target = planet::peak_target_m(&planet, p)`, `relief_scale = target / k.max_peak_m` (a first guess, so the coarsest level starts near its target). In `World::new_at` the factor `k.uplift` becomes `planet::UPLIFT_START_M_PER_YR` and nothing else changes; `forge_with` calls `scale_uplift` on every level's fresh world BEFORE `upsample_onto`: level 0 with `relief_scale`. When level 0's iterations end: `uplift_fit = fit_uplift_rate(&mut lw, target)`; every LATER level's fresh world gets `scale_uplift(relief_scale · uplift_fit)`. With a single level (a preview at the coarsest cell) the fit still runs after that level's iterations — it scales the heights and is recorded; there is no later level to carry the rate.
- The final `post::fit_height_band` call stays where it is; `Forged::height_fit` keeps its meaning (now the knee's ratio).
- **The belt lines** (`--report`, from `Forged::planet`'s belt labels read through `Sampler::belt_cell` per fine land cell — the warped point, where that cell's uplift was read — and the final `w.h`): slope of a cell = `fields::ground_slope(w, &|_| false, i)`; local relief of a cell = max − min of `h` over the square window of radius `max(2 cells, RELIEF_WINDOW_DEG (2.0) · metres_per_degree / dx)` (separable running max / min, x wraps), sea cells reading 0. Lines: `peak_target_m`, `uplift_fit`, `height_fit`, `belt_cells.young` / `.middle` / `.old` (fine land cells whose planet cell is in a belt of the class), `belt_height_mean_m.<class>`, `belt_slope_mean.<class>`, `belt_relief_mean_m.<class>`, `lowland_relief_mean_m` (land cells in no belt and with `uplift_now < 0.05`), `young_old_slope_ratio`, `young_old_relief_ratio`, `old_lowland_relief_ratio` (a class with no cell prints `nan`, which the summary counts as non-numeric), the hypsometry and river numbers of `quality(forged)` that later tasks judge on — `height_p50_m`, `height_p90_m`, `height_p99_m`, `concavity_theta`, `hack_exponent`, `river_km`, `max_strahler` — and — the Tzathas response time — `response_time_my` = the largest, over the 16 highest-discharge river mouths, of `Σ receiver_dist / (K · sqrt(max(q, hillslope_area · rain)))` along the longest upstream path (`K = k.erodibility`; one pass over `Analysis::routing`'s order): how far the run's 0.8 My is from steady state on this world.

- [ ] **Step 0: Before** — the 8-seed dev / 32 m report at the commit before, saved as `t9-before.txt` (with the belt lines added first as a no-behaviour change, so "before" has them: implement the lines, save, then implement the fits).
- [ ] **Step 1: Temporary tests:** `fit_height_band` on a world with heights 0 … 4,000 and `max_peak_m` 2,800: nothing below 2,380 moves, the new maximum is below 2,800, the order of any two cells' heights is preserved, and `h − uplifted` is unchanged per cell; `fit_uplift_rate` with peak 1,000 and target 500 returns 0.45 and multiplies every land height by 0.45 (a little under half); on an all-sea world it returns 1.0; with peak 460 and target 500 (`f` = 0.978, inside the dead band) it returns 1.0 and changes nothing; `peak_target_m` for `belt_depth_deg = 8` on Dev is 218 ± 1, on Large 2,676 ± 5, and never above `max_peak_m`; a dev / 64 m forge with `mountain_aspect = 2.0` has a higher `max_peak_m` line than with 0.6.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-report --size dev --cell 32 --seeds 8` and `just forge-report --size medium --cell 32 --seeds 2`: the before / after table of `max_peak_m`, `peak_target_m`, `uplift_fit`, `height_fit`, the three ratios, `response_time_my`; judged: `max_peak_m` within 0.7–1.05 of `peak_target_m` on at least 7 of 8 dev seeds and both Medium seeds; `height_fit ≥ 0.9` on at least 7 of 8 (the knee rarely bites). Quote the five biome shares that moved most on Dev (lower relief means weaker rain shadows: `orographic_scale_m` is 650 m — "Small worlds"). The three presets at dev / 64 m, one seed each, against "Earth-like": "Great ranges" and "Highlands" have a higher `max_peak_m`, "Lowlands" a lower one — else their overrides are changed here. Note for Task 15: on Dev the relief window is 3 cells (hillslope texture), on Medium 17 (valley relief) — the same 1.5 is asked of both and the two are reported apart. The second regime's constraints. The gate's message, unblessed.
- [ ] **Step 4: Verify, in order** — temporary tests pass; DELETE them; `just smoke`; `just web-check`.
- [ ] **Step 5: Commit** `feat(forge): mountains as high as their belt is deep — a peak target per world, the fit in rate space, a soft knee`.

### Task 10: Initial height and exhumation from crust thickness

**Files:** `crates/worldgen/src/forge/world.rs` (`World::new_at`: `h`, `uplifted`), `forge/planet.rs` (two constants), `forge/mod.rs` (only if `initial` needs it).

**Interfaces — produces:** `planet::INITIAL_ROOT_SHARE: f64 = 0.5`, `planet::EXHUMED_MARGIN_M: f64 = 300.0`; `exhumed_old_m = −k.basement_top_s + EXHUMED_MARGIN_M` (1,800 m at the default knob; the granite's top is at or below `basement_top_s` — `Geology::new` ends its sedimentary loop there — so a fully old, full-strength belt starts with granite at the surface for any knob value, short of the bedding's own relief). Per land cell, with `root = sampler.root_rel(mx, mz)`, `age = sampler.age_w(mx, mz)`, `taper = smoothstep(0.0, 0.3, land)`, `target = planet::peak_target_m(planet, p)`:
`h = today's + INITIAL_ROOT_SHARE · target · root · taper`; `uplifted = INITIAL_ROOT_SHARE · target · root · taper + exhumed_old_m · age · root` (was 0). `upsample_onto` is unchanged: it carries the eroded `h` and `uplifted` up, so only the coarsest level starts from this.

- [ ] **Step 0: Before** — `t10-before.txt` (dev / 32 m, 8 seeds).
- [ ] **Step 1: Temporary tests:** on a hand-built planet with one old belt (`root_rel` 1, `age_w` 1) and one young (`root_rel` 0.5, `age_w` 0): the initial `h` at the old belt's core is `0.5 · target` above the lowland's within 5 %, at the young one's `0.25 · target`; `uplifted` at the old core is `0.5 · target + 1,800` within 5 % and at the young core `0.25 · target`; with `regional_dip = 0` and `fold_amplitude_m = 0` (flat bedding: the test is then deterministic) `s = h − uplifted − bedding` at the old core is below the column's last top (`Geology::column().last()`), so `surface_rock` there is granite, and at the young core it is not; the same holds with `basement_top_s = −3,000`; no ocean cell's `h` or `uplifted` changed.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the 8-seed before / after table of the belt lines (`belt_height_mean_m.old`, `belt_relief_mean_m.old`, the three ratios, `max_peak_m`) plus `just forge-report --size large --cell 64` for one seed before and after (the largest response time: where inherited height should show most; about 40 s each). Judged: `belt_height_mean_m.old` does not fall against `t10-before.txt` on at least 6 of 8 seeds and rises on the Large seed; `young_old_slope_ratio` and `young_old_relief_ratio` are no more than 0.1 under Task 9's; the granite share of surface rock on old-belt cells (a new line, `belt_rock_share.old.granite`, from `Geology::surface_rock` over those cells) rises. `just forge-proto --size dev --cell 32 --out target/world-forge/p2/t10` and READ its `-geology` PNG beside `--layer orogeny-age`: do old belts show the basement colour? The second regime's constraints; the gate's message.
- [ ] **Step 4: Verify, in order** — as Task 9.
- [ ] **Step 5: Commit** `feat(forge): a belt starts from its crustal root — inherited height, and basement already exhumed under old belts`.

### Task 11: Isostatic rebound

**Files:** create `crates/worldgen/src/forge/rebound.rs` (`pub mod rebound;`), modify `forge/mod.rs` (`forge_with`'s loop), `forge/knobs.rs` (ADD `rebound_share` in "Erosion"), `crates/worldgen/examples/forge_proto/report.rs` (`shore_step_p99_m`), `forge/climate.rs` (only to make `box_blur_rows` `pub(crate)` if it is reused).

**Interfaces — produces:**

```rust
pub const REBOUND_EVERY_ITERS: usize = 5;
pub const REBOUND_RADIUS_DEG: f64 = 3.0;
/// The reference of the last rebound (or of the level's start).
pub struct ReboundState { h_ref: Vec<f64>, uplifted_ref: Vec<f64> }
impl ReboundState {
    pub fn new(w: &World) -> ReboundState;
    /// Adds `share` of the rock eroded since the reference, low-passed, to `h` and `uplifted` of every
    /// non-outlet cell, then moves the reference. Returns the mean rebound over land (m).
    pub fn apply(&mut self, w: &mut World, height_m: f64, share: f64) -> f64;
}
```

`apply`: `eroded[i] = max(0, (h_ref[i] + (w.uplifted[i] − uplifted_ref[i])) − w.h[i])` on land, 0 at sea. A coarse grid: `PLANET_NX × PLANET_NZ` when the level has at least that many cells each way, else the level's own grid; each coarse cell = the mean of the fine cells whose centres fall in it. Three circular-in-x box blurs along x and three along z, each of radius `r = round(σ − 0.5)` cells with `σ = REBOUND_RADIUS_DEG / (coarse degrees per cell)` — `REBOUND_RADIUS_DEG` is the Gaussian's SIGMA, and three boxes of radius `r` have variance `r · (r + 1)`, so `σ ≈ r + 0.5` (`r < 1` → no blur); along z the box is clipped at the first and last row. The fine cell gets the bilinear read (x wraps, z clamps) times `share`, added to `h` and to `uplifted`. `forge_with`: a `ReboundState` per level, created after `upsample_onto`; `if iter % REBOUND_EVERY_ITERS == 0 && k.rebound_share > 0.0 { state.apply(..) }` after the step. Nothing else reads it. Cost: two linear passes and a 131 k-cell blur every fifth iteration. Two things to expect and report, not fix: on Dev at 32 m the coarse grid IS the level and `r` is 4 cells (128 m) — close to the channel spacing, so there the rebound is nearly local (Airy) and Dev's rows will not stand for Medium's (836 m); and the rebound lifts coastal land beside a deeply eroded belt while the sea mask is fixed (the line `shore_step_p99_m`: the 99th percentile of land height over cells with a sea 4-neighbour, added here).

- [ ] **Step 0: Before** — `t11-before.txt`.
- [ ] **Step 1: Temporary tests:** on a flat 256 × 128 land world where a 20-cell-wide trench was just cut 100 m deep (reference = before the cut): `apply` with share 0.8 raises every cell, the total volume added is 0.8 of the volume removed within 3 % (the trench is kept at least `3r` rows from the north and south rows, which are sea, so the clipped z blur loses nothing), the rise at the trench's axis is less than 80 m and the rise 5 cells outside it is more than 0 (it is spread); a second `apply` with nothing eroded since adds 0; a cell that silted up (`h` above the reference) contributes 0; the read at column 0 and column `nx − 1` of a trench cut across the seam is symmetric within 1 %; ocean cells are untouched.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the 8-seed dev / 32 m table with `--set rebound_share=0` and at the default, on the same commit, plus `--size medium --cell 32` (2 seeds) at both: `uplift_fit` (expected to fall: the rebound amplifies, the rate fit absorbs it), `max_peak_m`, the belt lines and ratios, `lowland_relief_mean_m`, `height_p50_m` (Task 9's line), `shore_step_p99_m`. Judged: the world's peak still meets Task 9's band; `old_lowland_relief_ratio` does not fall; lowlands do not turn into a plateau (`height_p50_m` within ±50 % of the share-0 run after the fit). If lowlands rise out of band, lower the `lowland_uplift_share` default here and say so. `--set rebound_share=0` and the default forge wall times (the tool's own timing line), to state the cost. The second regime's constraints; the gate's message.
- [ ] **Step 4: Verify, in order** — as Task 9.
- [ ] **Step 5: Commit** `feat(forge): isostatic rebound — four fifths of every eroded metre comes back, spread over the flexural radius`.

### Task 12: The incision threshold; erodibility and diffusion by age

**Files:** `crates/worldgen/src/forge/world.rs` (`World` gains `pub k_mult: Vec<f32>`, `pub d_mult: Vec<f32>`, `pub threshold: Vec<f32>`; `new_at` fills them; `upsample_onto` keeps the fine world's; the `initial` literal in `forge/mod.rs` lists them as `Vec::new()`), `forge/erosion.rs` (`evolve`), `forge/knobs.rs` (ADD `incision_threshold`, `old_erodibility`, `old_diffusion` in "Erosion"), `forge/world.rs` (`scale_uplift` also scales `threshold`).

**Interfaces — produces:** per land cell, `a = sampler.age_w(mx, mz)`: `k_mult = 1 + (old_erodibility − 1) · a`, `d_mult = 1 + (old_diffusion − 1) · a`, `threshold = k.incision_threshold · k.belt_uplift · UPLIFT_START_M_PER_YR · a` (m/yr at scale 1; `World::scale_uplift(f)` multiplies `uplift` AND `threshold` by `f`, so at every level the threshold is `incision_threshold` of a full belt's fitted rate — Ruling 4, Review Focus 5); 1, 1, 0 at sea and wherever `a = 0` (all land outside belts: lowlands erode by today's law). In `evolve`:
- incision: `erodibility = k.erodibility · geo.erodibility_at(i, hi, before) · f64::from(k_mult[i])`; with `h_star = (hi + lift + f · base) / (1.0 + f)` (today's value before the floor), `cut = (hi + lift − h_star).max(0.0)`, `kept = (cut − dt · f64::from(threshold[i])).max(0.0)`, `h_new = (hi + lift − kept).max(LAND_FLOOR_M)`. With `threshold == 0` this is today's expression EXACTLY (write it so: `if threshold[i] == 0.0 { today's line }`), so `--set incision_threshold=0 --set old_erodibility=1 --set old_diffusion=1` reproduces the previous commit's `height_crc` — the task's own safety check. `h_new ≥ h_star > base` keeps the downstream-first order valid.
- diffusion (per-cell factor on the cell's own Laplacian — not the conservative `∇·(D∇h)`; a little mass is gained or lost where `d_mult` changes, at belt edges: say so in the doc line): the coefficient per cell is `c · f64::from(d_mult[i])`; `substeps = ceil(coeff · max(d_mult) / 0.2)`; both stencils (interior and the wrapping columns) multiply their `lap` by the cell's own factor. With every `d_mult == 1` the sub-step count and every expression are today's.
- Memory: 12 bytes per fine cell on the final level (plus the same on the level being built).

- [ ] **Step 0: Before** — `t12-before.txt`; note its `height_crc`s.
- [ ] **Step 1: Temporary tests:** the three `--set` values above reproduce `t12-before.txt`'s `height_crc` on 2 seeds at dev / 64 m (a forged comparison, not a unit test: run `just forge-report` twice and diff); `parallel` vs serial is unchanged — one erosion step of a dev / 128 m world with the defaults gives bit-identical `h` under `RAYON_NUM_THREADS=1` and unset (run the tool twice with the env var and compare `height_crc`); on a hand-built slope with `threshold` above the stream power of its upper half, 50 steps leave the upper half within 1 m of its uplifted height while the lower half cuts down; after `scale_uplift(0.5)` every `uplift` and every `threshold` is half what it was and `k_mult`, `d_mult` are unchanged; a land cell whose planet cell has `kind == None` has `k_mult == 1`, `d_mult == 1`, `threshold == 0`; a cell with `d_mult = 3` on a ridge loses height at least twice as fast as its `d_mult = 1` twin in 10 steps of diffusion only (erodibility 0).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — on one commit, dev / 32 m × 8 seeds and medium / 32 m × 2 seeds, four runs: all three at neutral; the threshold alone at its default; the two multipliers alone; all at their defaults. The table: the belt lines and the three ratios, `max_peak_m`, `river_mouths`, `concavity_theta` and `max_strahler` (Task 9's lines), `lowland_relief_mean_m` (it must not move between the four runs by more than 5 %: the modifiers do not reach lowlands), the tool's wall time per run. The comparison is made on Dev AND on Medium because the share-of-rate threshold is meant to act alike on both: quote `belt_relief_mean_m.old` threshold-on ÷ threshold-off for each size; if the two quotients differ by more than a factor 1.5, say why (the response time, the relief window) before tuning anything. Judged: the three ratios move toward Ruling 4's targets with all three on, against neutral; old belts keep `old_lowland_relief_ratio ≥ 2`. A term that moves a ratio the WRONG way at its default gets its default changed here (say to what, and why) or, if no value in its range helps, is ruled out and DELETED with its knob in this task (`Ruling: … dropped — …`), never left as a knob that does nothing. The second regime's constraints; the gate's message.
- [ ] **Step 4: Verify, in order** — as Task 9.
- [ ] **Step 5: Commit** `feat(forge): old ground erodes differently — an incision threshold, erodibility and hillslope diffusion by orogeny age`.

### Task 13: Geology in space — a rock column per province, strata that follow the folds

No format change: Task 6's container gets its values.

**Files:** `crates/worldgen/src/forge/geology.rs` (`Geology`), `forge/planet.rs` (`province` filled in `Planet::build`), `forge/world.rs` (`World` gains `pub province: Vec<u8>`, filled by `Sampler::province`; `upsample_onto` keeps the fine world's; the `initial` literal), `forge/mod.rs` (`Forged::artifact`: `columns`, `province`), `forge/erosion.rs` and `forge/hydrology.rs`, `forge/fields.rs`, `forge/render.rs` (their `geo.erodibility_at` / `surface_rock` calls, if a signature changes), `crates/worldgen/examples/forge_proto/report.rs`.

**Interfaces — produces:**

```rust
// planet.rs — the province of a planet cell
pub const PROVINCE_PLATFORM: u8 = 0; pub const PROVINCE_FOLD_BELT: u8 = 1; pub const PROVINCE_SHIELD: u8 = 2;
pub const PROVINCE_VOLCANIC: u8 = 3; pub const PROVINCE_OCEAN: u8 = 4;
pub const SHIELD_MIN_CRUST_AGE_MY: f64 = 1_500.0;
// geology.rs
impl Geology {
    pub fn new(p: &ForgeParams, w: &World) -> Geology;               // signature unchanged; reads w.province
    pub fn columns(&self) -> &[Vec<(f64, u8)>];                      // replaces column()
    pub fn rock_at(&self, province: u8, s: f64) -> u8;               // replaces rock_at_s
    pub fn surface_rock(&self, w: &World, i: usize) -> u8;           // unchanged signature
    pub fn erodibility_at(&self, i: usize, h: f64, uplifted: f64) -> f64;   // unchanged signature; keeps a copy of w.province
}
```

- **Province of a planet cell** (first rule that holds): oceanic crust → `OCEAN`; `kind ∈ {Arc, Hotspot}`, or `Andean` with `orogeny_age < 30` and `volcanic > 0.5` → `VOLCANIC`; `kind ∈ {Andean, Collision, Plateau}` with `strength · root_keep ≥ BELT_MIN_REL` → `FOLD_BELT`; continental with `crust_age ≥ SHIELD_MIN_CRUST_AGE_MY` and `kind == None` → `SHIELD`; else `PLATFORM`. Then one 3 × 3 majority pass (x wraps).
- **The five columns** (all from the existing "Geology" knobs and the six `ROCKS`; no new rock, no new block; every random draw salted by the province so each has its own sequence):
  - `PLATFORM`: today's column exactly (the same `hash01` salts: a world of one platform province has today's rock);
  - `FOLD_BELT`: today's weights with formations 1.5× as thick and the basement at `basement_top_s`;
  - `SHIELD`: 2 to 4 formations (60–200 m in all) of sandstone and conglomerate over granite: the basement top at −200 m at most;
  - `VOLCANIC`: basalt 60–140 m thick alternating with sandstone or conglomerate 10–30 m thick down to `basement_top_s`, then granite;
  - `OCEAN`: 20–60 m of shale and limestone, then basalt without limit.
  Each satisfies Task 6's `validate_tables` bounds (first top ≤ 800, last top above the band floor).
- **Bedding:** today's `dip_z · (z − half_z) + fold_amplitude_m · 2 · folds` everywhere; in `FOLD_BELT` cells with a stored fold direction the fold noise is sampled in a frame aligned with the fold AXIS (perpendicular to `fold`, read nearest from the planet and smoothed by a 5 × 5 planet-cell vector mean): `folds.sample2(u, v / FOLD_STRETCH)` with `u` across the axis, `v` along it, `FOLD_STRETCH = 4.0`, amplitude × `FOLD_BELT_AMPLITUDE = 2.0` — long strike ridges where hard beds outcrop. The two frames are blended by the smoothed belt weight so there is no step at the belt's edge. **Fallback (decided by Step 3's picture):** if the rotated frame shears (curved streaks where the direction turns), keep isotropic folds with the doubled amplitude in belts and record the ruling; `fold` stays stored for Phases 3–5 either way.
- `Forged::artifact`: `columns` = the five columns (as `f32`), `province` = `w.province`. `--report`: `province_share.<name>` (of land), `province_rock_agrees` (for 4,096 land cell centres: `ForgedWorld::surface_rock` equals `Geology::surface_rock` for at least 99 % — the runtime interpolates the ground, so the last per cent may differ: yes/no and the share), `province_band_clamped_cells`, `strata_stack_len`, `strata_footprint_max`.

- [ ] **Step 0: Before** — `t13-before.txt`.
- [ ] **Step 1: Temporary tests:** each of the five columns passes `validate_tables`; a `World` whose every cell is `PLATFORM` gets from `Geology::new` the SAME platform column and bedding as the previous commit's `Geology::new` (copy the old constructor into the test as the oracle); `rock_at(SHIELD, −300)` is granite, `rock_at(OCEAN, −500)` basalt; on a forged dev / 64 m world the artifact round-trips, `province_rock_agrees` holds, and a region generated across a province border (found by scanning `province`) has no block of a rock that neither province's column holds within 60 blocks of the surface; every `geo` of 16 regions fits `i16` unclamped.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — dev / 32 m × 8 seeds before / after: `province_share.*`, `province_rock_agrees: yes`, `province_band_clamped_cells` (0 on Dev; quote Medium's), `strata_footprint_max` (quote; above 400 is a finding for Ruling 7's cost), the belt lines and ratios (rock moved the erodibility: say how much), the forge's rock shares (`Stats::rock_shares`, already printed by `print_report`). `just forge-proto --size dev --cell 32 --out target/world-forge/p2/t13` and `--size medium --cell 32`: Judged: every `province_share.*` above 0 on at least 6 of 8 seeds; `province_share.ocean` (land cells reading the ocean province: the coast noise and the warp put some on it — a basalt rim on some coasts is expected) at most 0.03. READ the `-geology` PNGs: are there provinces (granite shields, basalt arcs and sea floor, banded fold belts)? do the fold belts' bands run along the belts (else the fallback)? `just cross-section` on a point inside a fold belt and one on a shield (the recipe's existing flags; quote the PNG paths and READ them). The second regime's constraints; the gate's message.
- [ ] **Step 4: Verify, in order** — temporary tests pass; `just test-one worldgen "--lib -- forged:: forged_tiles::"`; DELETE the temporary tests; `just smoke`; `just web-check`.
- [ ] **Step 5: Commit** `feat(forge): geology varies in space — a rock column per province, strata folded along the belts`.

### Task 14: The seed score knows continents and belts

**Files:** `crates/worldgen/src/forge/quality.rs` (`LAND_BAND`, `BiomeSummary`, `summarise`, `world_score`), `crates/worldgen/examples/forge_proto/search.rs` (the table's columns), `crates/worldgen/src/forge/render.rs` (`PreviewLayer` gains `Plates` — the `belts` picture of Task 4 on the fine grid's shape — so the menu preview and `forge-ui` can show it; `label`, `ALL`), `crates/worldgen/examples/forge_proto/{serve.rs,ui.html}` and `crates/client/src/frontend/generator.rs` only where they enumerate `PreviewLayer::ALL` by hand.

**Interfaces — produces:** `LAND_BAND = ((0.15, 0.25), (0.45, 0.60))` (full marks from 25 % to 45 % land: the solved `land_fraction` 0.32 sits in the middle; was 30–60 %); `BiomeSummary { …, pub continents: u32, pub young_belts: u32, pub old_belts: u32, pub continents_without_belt: u32 }` filled by `summarise` from `a.planet` (re-labelling the stored record: `belts::label_record(&PlanetRecord) -> PlanetScore`, a thin wrapper that rebuilds the fields `label` reads); `world_score` gains the terms `("continents", min(1, continents / 2))`, `("ranges", 1 when no continent lacks a belt, else 0.5)`, `("young and old", 1 / 0.5 / 0 for both / one / neither class)`. "snow peaks" and "mountain starts" stay (they will read low on Dev and Tiny by Ruling 3 — the search prints them; Phase 5 owns the mountain rule).

- [ ] **Step 1: Temporary tests:** `belts::label_record` of `PlanetRecord::from_planet(&planet)` equals `planet.score` on 4 seeds (quantisation does not move a belt across `BELT_MIN_REL`: if it does, label from the quantised values in BOTH places and say so); `world_score` of a world with 1 continent is lower than the same summary with 2; the land term is 1.0 at 0.32 and below 1 at 0.5.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — `just forge-proto --search 16 --size dev`: quote the table (the new terms appear as weakest criteria where they should); `just forge-ui` is NOT launched (it serves a page); instead `just forge-proto --size dev --cell 64 --out target/world-forge/p2/t14` writes the new preview layer's PNG beside the others (add it to the tool's default PNG set): READ it. `height_crc` per seed equals Task 13's (no world changed).
- [ ] **Step 4: Verify, in order** — temporary tests pass; DELETE them; `just smoke` (it type-checks the client: enough for an enumeration of `PreviewLayer::ALL`; no client test is run — Ruling 14); `just web-check`.
- [ ] **Step 5: Commit** `feat(forge): the seed score counts continents and belts by age; a plates layer in the preview`.

### Task 15: Tune the relief by age; look at the drainage numbers

At most 4 rounds (Ruling 11). A round is dev / 32 m × 8 seeds (about a quarter of a minute of forging) plus medium / 32 m × 2 seeds (about a minute and a half).

**Files:** `crates/worldgen/src/forge/knobs.rs` (defaults), `forge/planet.rs`, `forge/plate_sim.rs`, `forge/post.rs`, `forge/rebound.rs` (constants), `forge/presets.rs` (an override that no longer does what its description says).

**Targets:**

| Line | Target |
|---|---|
| `planet.ok` | 8 of 8 dev seeds (7 with a ruling); `planet_matches_fine: yes` on at least 7 of 8 |
| `land_share` | 0.26–0.38 on every seed |
| `land_components_over_3pct` | ≥ 2 on at least 7 of 8 |
| `young_old_slope_ratio`, `young_old_relief_ratio` | each ≥ 1.5 on at least 7 of the seeds that have both classes, on Dev AND on both Medium seeds (reported apart: on Dev the relief window is hillslope texture, on Medium valley relief) |
| `old_lowland_relief_ratio` | ≥ 2.0 on at least 6 of 8 (old ranges are not flat) |
| `belt_height_mean_m.old ÷ .young` | 0.2–0.6 (lower, not gone) |
| `max_peak_m` | 0.7–1.05 × `peak_target_m` on at least 7 of 8; `height_fit ≥ 0.9` on at least 7 of 8 |
| `margin_drowned_share` | ≤ 0.05 on every seed |
| the second regime's constraints | all hold |
| `province_rock_agrees` | yes on every seed, Dev and Medium |

**Dials:** young / old contrast → `belt_decay_my`, `BELT_RESIDUAL`, `incision_threshold`, `old_erodibility`, `old_diffusion`; old ranges flat → `ROOT_RESIDUAL`, `INITIAL_ROOT_SHARE`, `rebound_share`, `incision_threshold` up; old ranges as high as young → `BELT_RESIDUAL` down, `belt_decay_my` down; peaks off target → `FIT_AIM`, `mountain_aspect`; coasts too smooth or too ragged, or belts drowned by the coast's wander → `coast_wander_deg`, `coast_detail_deg`; belts featureless → `ALONG_STRIKE_VARIATION`, `PLANET_WARP_DEG`.

- [ ] **Step 1: Round 0** — the table at the committed defaults (dev and Medium). `just forge-proto --size medium --cell 32 --seed <each of the 2> --out target/world-forge/p2/t15-m<k>` and `just world-map --world … --tiles --out …`: READ the relief, biome and rain maps beside `--layer orogeny-age` and `--layer province`; for 4 dev seeds the same. Write, per world, one line answering the owner's question: several continents? more than one range on some continent, of different sizes? can the old ranges be told from the young ones on the relief map?
- [ ] **Step 2: Rounds 1–4** — one change set per round; the table and one line per change after each; the maps of the 2 Medium worlds re-read when a round moved a ratio by more than 0.2.
- [ ] **Step 3: The drainage numbers** (the Phase 1a ledger's note: θ 0.26 and Strahler 2 on the dev gate world) — one table of `concavity_theta`, `hack_exponent`, `max_strahler`, `river_km`, `response_time_my` for dev / 32 m (8 seeds), small / 32 m (2) and medium / 32 m (2) at the final defaults. If θ and the order rise with world size toward the old island's 0.67 and 3, the dev values are the size's (a whole planet in 16 km has kilometre-long rivers): record that as a ruling with the table. If they stay low at Medium, it is an erosion-input defect: find which of Tasks 9–12's terms moved them (the `--set` neutral values) and fix or rule here.
- [ ] **Step 4: Presets, re-checked** (Task 9 rewrote them) — forge "Great ranges", "Highlands", "Lowlands" at dev / 64 m and medium / 64 m (one seed each) and compare `max_peak_m`, `belt_relief_mean_m.young`, `lakes` with "Earth-like" after this task's retuning: each still does what its description says, or its overrides are changed here.
- [ ] **Step 5: Misses** — as Task 5 Step 4.
- [ ] **Step 6: Verify** — `just smoke`; `just web-check`; the gate's message, unblessed.
- [ ] **Step 7: Commit** `tune(forge): relief by belt age — the defaults after the evidence rounds` (the message carries the final table).

### Task 16: Evidence, the gate re-record, every size measured, close

**Files:** `crates/worldgen/tests/fixtures/forge_quality_planet_dev_32m.ron` (the phase's ONE re-record), `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` ("Measurements": a "Phase 2" table; Decision 4's Phase 2 line as built; "Left to the builder": the planet grid is 512 × 256 on every size; "Open questions": the polar edges closed), `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Status row 2 → DONE with the plan path and commit range; Evidence: "Phase 2"; Owner hand checks; a check that the amendments of Tasks 1, 4 and 9 are in place and say what was built — whole-cell translation, the 512 × 256 grid on every size, the plate-stage score, the small worlds' mountain heights — and the design's Phases table row 2 gate reworded: "belts per continent by age class (`planet.belts_per_continent_by_class`); mean slope and local relief differ between young and old belts (means and their ratios — no histogram was built)"; the Phase 3, 4, 5 notes this phase changes: what the stored record holds and where — `forged::PlanetRecord`, `ForgedWorld::planet` — and `World::new_at`'s `sea_stand` for Phase 4), this file ("As built": commits per task, the rulings made by tasks verbatim, deferred minors), `TODO.md`, `CLAUDE.md` if a fact above is still stale, `crates/worldgen/src/forge/play.rs` (`FORGE_SECS_PER_MCELL`, `FORGE_COST_EXPONENT`, `FORGE_RAM_*`, and a new additive constant for the plate stage's fixed cost) ONLY if a measured row is more than 25 % off the estimate.

- [ ] **Step 1: The phase's evidence run** — `just forge-report --size dev --cell 32 --seeds 8` and `just forge-report --size medium --cell 32 --seeds 2` (every line), `just forge-planet --seeds 8 --out target/world-forge/p2/final` (the sheets' paths), `just forge-proto` + `just world-map --tiles` for one dev and one Medium world (PNG paths). Paste into the program's Evidence section under "Phase 2", judged against its row: continents per world (`planet.continents`, `land_components_over_3pct`); belts per continent by age class (`planet.belts_per_continent_by_class`, every seed's value); mean slope and local relief of young against old belts with the ratios (the design's "slope histograms differ" is answered by these means: say so); land share; the seam rule's three `seam_pass_*` lines; `planet.stage_ms` (the plate stage alone, per attempt and with retries). Every accepted miss is listed there with its ruling.
- [ ] **Step 2: The gate** — `FORGE_QUALITY_BLESS=1 just test-one worldgen "--test it -- --include-ignored forge_lib::forge_quality_matches_baseline"`, then the same without bless twice (it passes). A commit of its own, `test(worldgen): re-record the forge quality gate after plates and aged relief`, whose message states the eleven old numbers (`git show HEAD:crates/worldgen/tests/fixtures/forge_quality_planet_dev_32m.ron` before the bless: land 0.3098, p50 / p90 / p99 33.1 / 628.4 / 1,641.6 m, peak 1,992.1 m, θ 0.262, Hack 0.434, 14 mouths, 15.84 km, Strahler 2, 1 lake — or whatever Phase 1b left there) and the eleven new.
- [ ] **Step 3: Measure** (release, one at a time, nothing else of ours running): `just forge-bench 16384 8192 32`, then Tiny (`25600 12800`), Small (`50176 25088`) and Medium (`100352 50176`) at `16` and at `32`, and Large (`200704 100352`) at `32` — the recipe takes width, height and cell, no size names; Large at 16 m only if `free -g` shows more than 20 GB available, else "not run: needs about 14.5 GB". For each: wall seconds, max RSS, the `.world` size, and from the tool's own lines the plate stage's milliseconds and attempt count; note `scaling_cur_freq`.
- [ ] **Step 4: Record** the "Phase 2" table in the design beside Phase 1b's rows, with the difference per row and one line attributing any row more than 25 % above Phase 1b's (the plate stage and its retries; diffusion sub-steps; the rebound; the per-cell arrays; the memoised `Planet`, about 9 MB for the life of the process). No pass / fail line: the owner judges. Refit `play.rs` only by the rule above (a temporary test: the estimate within ±25 % of every measured row).
- [ ] **Step 5: Whole suites, once:** `just test` in the foreground, then `just clippy`; quote the PASS / FAIL line per stage. A failure in this plan's files is fixed here; one outside them is recorded.
- [ ] **Step 6: Owner hand checks appended to the program:** `just run --reset` (and `just pg-reset`); the twin and orion need a re-forge before they run this build; `just world-map` on a Medium world — do the continents and ranges read as a small Earth, and can old ranges be told from young ones; **Dev and Tiny worlds have mountains of a few hundred metres by Ruling 3 — no `Mountain` biome or mountain regions, no snow peaks, world-map tiles classed as hills, weaker rain shadows, and more history capacity (less relief penalty), each row of "Small worlds" — `mountain_aspect` is the slider if the debug world should be steeper**; the generator lost ten sliders (the seven static-plate ones, the two coast noises, `uplift`) and gained twelve; the three relief presets now work through `mountain_aspect`.
- [ ] **Step 7: Commit** `docs: Phase 2 measurements and evidence; plates and aged relief closed`.

## Known limits (not in this plan)

- **Plates only translate.** No rotation about a pole, so belts and coasts get their curvature from the sampling warp and the coast noises alone; arcs are straight (Ruling 1).
- **The supercontinent cycle is one breakup.** A run is 250–700 My from one assembled supercontinent; "old" belts are the assembly's sutures and whatever collided early. There is no second cycle and nothing older than the start (cratons are a noise of crust age, not a history).
- **No failed rift arms, no craton-avoiding rift lines, no back-arc basins, no large igneous provinces, no subduction polarity flip** — the research names them; none of the simulators it read models them either. Shelves are a formula of the rifted margin's thinning, not a result.
- **The fold direction is stored and bends the strata in fold belts; it does not yet orient ridges through the uplift field or the erosion.** Phases 3–5 read it from the record.
- **Elevation on the planet grid is a closed form of strength and age**, not an integrated history: a belt re-activated twice remembers only its decayed strength. Deep bathymetry is a function of crust age alone (no trenches as such — `TRENCH_M` only scales the subduction rule).
- **Age is a steady state plus modifiers, not elapsed erosion** (Ruling 4): no rejuvenation pulse, no sediment deposition (Phase 4), no threshold-slope / talus pass, no drainage inherited from an older surface.
- **The sea is still a fixed mask** from the solved level and the coast noise; rias, estuaries and fjords need Phase 4's low stand (`World::new_at`'s `sea_stand` is the hook).
- **Climate is still Phase 1a's annual model** on the new land; the Köppen checks are Phase 3's. Small worlds lose the `Mountain` biome, snow peaks, mountain regions and most rain shadows, and gain history capacity, until Phases 3 and 5 (Ruling 3; "Small worlds").
- **Age modifiers act on belts only**: lowlands, platforms and shields erode by one law and differ by rock.
- **Hotspot chains over the ocean are islands only near the hotspot** (a closed-form subsidence); over continents they are volcano sites.
- **One planet grid for all sizes**: on Large a planet cell is 392 m, and the plate stage's world does not get finer with the world (Ruling 2).
- **A world that fails the score on all six attempts is forged anyway** with `PlanetRecord::ok == false`; nothing refuses it.
