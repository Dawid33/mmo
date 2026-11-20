# Runtime Seasons and Weather (Phase 6) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Written 2026-11-20** from a code check of that day (tree at `caca3f68` plus the owner's uncommitted work), while
> Phase 2 was being built in `crates/worldgen`. NOT yet run past a builder check. Search by the named symbol, never by
> line. Symbols marked *(verify at dispatch)* were not read first-hand (list: the last section). **No step commits a
> test.** One task (17) is HELD: it cannot land without re-recording gates this program may not re-record.

**Goal:** The world has a year. Every process that runs or predicts a region derives the date from the world tick and can ask, for any place and tick, the temperature, the precipitation, the snow on the ground and the ice on the water — one pure integer function in `game`, the same bits on server, native client and wasm32, never streamed per tick, never hashed, never saved. The first-person client's sun, sky, a HUD line and the near ground follow it.

**Architecture:** Four moves. (1) **The date** is `game::season::sim_date(tick)`: integer arithmetic on the world tick and the one day-length constant (`history::calendar::TICKS_PER_DAY`); sun geometry is `game::solar` (`libm`). (2) **The climate reaches every runner as configuration**, exactly like `map_extent`: a `game::WorldClimate` (the `MonthlyClimate` family behind an `Arc`, plus an integer frame: seed, map origin, lapse, sea level) set on a `Region` by `RegionInput::SetClimate`, `#[serde(skip)] #[state_hash(skip)]` on `GameData`; the server builds it from its installed forged world, a client receives it ONCE per session in a new `ServerPacket::WorldClimate` (row-delta packed; the codec moves into `game`). (3) **Weather** is `game::weather::weather_at(&WorldClimate, MapPoint, height_m, tick) -> WeatherSample`: month-interpolated normals, a mean-zero diurnal table, Gaussian value noise on global lattices in degrees and days mapped to a uniform value, advection by the stored wind, hashed storm objects — integers only. (4) **Snow and ice** are `game::overlay`: a bounded daily replay of that function from each cell's snow-free date, memoised outside all state; the client draws them as a tint over the near ground and water. Nothing existing changes a hash, a stored byte or a link message; the one wire change is one appended `ServerPacket` variant.

**Tech Stack:** Rust; crates `game` (everything sim-side: `season.rs`, `solar.rs`, `world_climate.rs`, `climate_wire.rs`, `weather/`, `overlay.rs`), `worldgen` (one constructor on `ForgedWorld`, the codec call sites, one new example), `server` (install + send), `client` (Bevy 0.19.1, Slint: `instance.rs`, `regions/`, `renderer/{atmosphere,skybox,hud,dev_ui}.rs`, a new `renderer/seasons.rs`). No new dependencies (`libm`, `lz4_flex`, `crc32fast` are in `game` already).

**Spec:** `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` (Goal 5; the weather anti-goal; Decisions 6, 8, 9; Phases row 6). Program: `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Phase 6; its Standing rules bind every task). Research: `docs/research/research_notes/Earth like world generation overhaul/runtime_seasons_weather.md` (all of it), the report's "Runtime seasons are a pure function of tick and place" and its evidence ledger.

Ledger: `.superpowers/sdd/2026-11-20-runtime-seasons-and-weather-6/progress.md` (git-ignored workspace). Commit procedure, implementer and reviewer rules: Phase 1a's, `.superpowers/sdd/2026-11-14-planet-frame-1a/{common,implementer-rules,reviewer-rules}.md`.

## Dependencies (what is built, what is planned, what is re-run)

- **Phase 1a, 1b — BUILT.** Read in the tree 2026-11-20: `game::MonthlyClimate { nx, nz, cell_m, wrap_x, temp_sea_centi_c, diurnal_deci_c, precip_deci_mm, wet_day_255, wind_east_q, wind_south_q }`, `MonthlyClimate::{cells, index, validate, cell_normals, normals_at, resident_bytes}`, `CLIMATE_MONTHS`, `NORTH_MIDSUMMER_MONTH = 3`, `NORTH_MIDWINTER_MONTH = 9`, the `CLIMATE_*_PER_*` denominators; `worldgen::forged::ForgedWorld::{monthly_climate, monthly_normals, monthly_at_ground, monthly_at_ground_map, annual_climate, lapse_per_m, column_climate, globals, artifact_bounds_engine_m}`, `ground_normals`, the private `to_artifact_raw` (`x − ENGINE_SPAWN_M + globals.spawn[0]`), `fold_engine_x`; `pipeline::{SEA_LEVEL = 64.0, SEA_TOP_BLOCK}`; `climate_to_wire` / `climate_from_wire` / `delta_plane` / `undelta_plane` (crate-private, `#[cfg(not(target_arch = "wasm32"))]`); `MapExtent`, `GameData::map_extent` (`#[serde(skip)] #[state_hash(skip)]`, written raw by `Region::set_route_map`), `RegionInput::SetRouteMap`, the client's `RegionCmd::SetRouteMap`.
- **Phase 3 — PLANNED, not built.** It replaces the container's VALUES (real seasonal temperature, precipitation, wet-day fraction, diurnal range, wind per month), no format change; its derived fields (snow months, sea-ice months) are forge-side functions, none stored; a model month is a twelfth of a year, and only the wet-day fraction counts the calendar's 28 days. **Every task here runs on the placeholder values and needs no code change when Phase 3 lands.** Its snow constants are reused on purpose (`SNOW_ALL_C −1.0`, `SNOW_NONE_C 3.0`, `MELT_MM_PER_C_DAY 4.0`, `SEA_FREEZE_C −1.8`) so the runtime's snow agrees with Phase 3's `snow_months`.
- **Phase 4 — PLANNED, being revised.** Present-day glacier ice becomes a stored thickness raster rendered as terrain blocks (`ColumnClimate::ice_m` → `glacier_blocks`); "Sea and lake ICE blocks keep today's annual-temperature rule (Phase 6's overlays)"; seasonal river levels, tides, a lake's `ice_months` and seasonal range are stored, not played. This plan reads none of it and changes none of it (Ruling 10).
- **The program's rule:** Phase 6 "may start as soon as Phase 1b is DONE … if started early it is still finished after Phase 3". Tasks 1–16 may run now. **Task 18 (the close) runs only after Phase 3 is DONE**, and re-runs, on a world forged by the Phase 3 build: the year summary for five places (Task 15) with its acceptance ranges; the snow-cover days against Phase 3's `snow_months` line (within ±1 month at the four non-desert places); the storm share of annual precipitation (Ruling 8); the bytes on the wire per size (Task 11 — real fields pack differently from a cosine); the calibration tables are NOT re-run (the noise does not read the values). Nothing else is value-dependent. After Phase 4: nothing to re-run; the close notes whether baked sea and lake ice changed.

## Global Constraints

- Do not create git worktrees; work on `develop`. No stash, reset, amend, checkout of files, clean, push. The tree holds the owner's uncommitted changes (among them `crates/game/src/{lib,state,region,protocol}.rs`, `crates/client/src/{instance,session}.rs`, `renderer/{atmosphere,hud,dev_ui,bridge}.rs`, `CLAUDE.md`, `TODO.md`, `justfile`) and other sessions stage in the same index: each commit carries only the task's own hunks, staged through the index as `common.md` describes (memory: `partial-staging-beside-owner-hunks`, `commit-only-own-paths`).
- **`crates/worldgen` is being rewritten by Phase 2 while this plan runs.** Tasks 2, 3 and 15 touch it in small, named hunks only; each re-reads its symbols at dispatch and reports BLOCKED if a named function no longer exists.
- **Tests.** Only regression tests of bugs actually seen and the existing gates are committed. Every "temporary test" is written first, seen failing, quoted in the implementer's report (name, command, failing then passing output) and DELETED before `just smoke`. An "evidence command" is a tool run whose output is quoted. **No new gate. No gate re-recorded** — this phase forges no terrain: `forge_quality_matches_baseline` is untouched.
- **Verify, in this order, in every task:** (1) the temporary tests pass; (2) DELETE them; (3) `just smoke` (cold: `just smoke_timeout=1200 smoke`; if its audit fails on another session's file: `just test-audit <own crate dirs>` + the remaining smoke steps by hand, said in the report); (4) `just determinism-wasm` whenever `crates/game/src` changed; (5) `just web-check` whenever `client` or anything the browser build links changed (`game`, `worldgen`). Client lib tests are run targeted (`just test-one client "--lib <filter>"`). `just test` once, in Task 18.
- **`just determinism-wasm` in this checkout:** its first stage (`game --test wasm_determinism`, `scene_hash_native`) fails on the owner's uncommitted `crates/game` work (`native scene digest 0xE6CD22F6 != golden 0x38D86635` on 2026-11-20). The rule: **the printed digest and state hash of every scene are the SAME before and after the task** (record them before the first edit: `VOXEL_PRINT_HASH`-style output of the test, *(verify the switch's name at dispatch: `print_hash()` in `wasm_determinism.rs`)*); the recipe stops at the first failure, so its remaining three commands are run by hand and quoted. A value that moves is BLOCKED, never a re-record.
- **Nothing in this phase is simulation state** (Tasks 1–16): no `#[rollback]` field, no `#[undo]`, no `StateHash`, nothing in a snapshot, a cut part, a `Transfer`, a `HostMsg` / `CoordMsg`, the undo log. `WorldClimate` is configuration, written raw. **No sim rule reads weather, snow or ice in this phase** (Ruling 12): the functions exist, the sim does not call them from a tick.
- **Arithmetic.** `game::weather`, `game::overlay`, `game::season`: integers only (`i32` / `i64` / `u64`, fixed point as named), every division by a positive divisor written with `div_euclid` / `rem_euclid` where the dividend can be negative, no `f32` / `f64` at all. `game::solar`: `f64` with `libm` (`sin`, `cos`, `asin`, `acos`, `atan2`) in a fixed expression order. Randomness only as `crate::slime::roll(tick, subject, salt)` *(the `game`-level `roll(tick: u64, subject: u64, salt: u64) -> u64`; if it is private at dispatch, make it `pub(crate)` — it is the same mix as `history::rng::roll`)*. Tables are `const` arrays of integers committed in the source, never built with float math at start-up. Tunables are named `const`s with a doc line saying *sourced*, *recalled* or *untested* (the research ledger's words).
- **Everything is in calendar days.** No constant in this plan is in ticks or milliseconds; every conversion goes through `history::calendar::{TICKS_PER_DAY, DAYS_PER_MONTH, MONTHS_PER_YEAR, DAYS_PER_YEAR}`. Changing `DAY_MS` rescales the weather and nothing else. Nothing divides by a number of ticks per anything other than `TICKS_PER_DAY` (const-asserted `>= 1`); nothing animates per tick.
- **Global addressing.** Every lookup takes a `MapPoint` (map metres in Q8, x folded when the family wraps) and the world tick. No region-local origin, seed or cache key anywhere in `weather` or `overlay`.
- `game`, `server`, `worldgen` stay Bevy-free. Never write a region edge as a literal. UI is Slint only. Any new per-world client resource is registered for session reset (`SessionResetExt::add_session_reset` / `SessionLocal` / `SessionScoped`).
- **Formats.** No stored format moves (`ARTIFACT_VERSION` 10, `TILED_FORMAT_VERSION` 3, `WORLD_MAP_VERSION`, `META_FORMAT`, `COORDINATOR_STATE_VERSION` 6, no migration). `LINK_PROTOCOL_VERSION` stays 11 (no link message changes). `ServerPacket` gains ONE variant, appended last (Task 11): client and server must be one build, as for every packet change — an older client fails to decode the frame and its session ends; a newer client on an older server simply never receives a climate and shows today's fixed sun and no weather.
- Never launched by agents: the game window (`just run`, `play`, `pull-smoke`), paid generation, anything on orion or the twin, benchmarks. What needs eyes is under "Owner hand checks".
- Any new or changed command gets its `justfile` recipe in the same task; `CLAUDE.md` and `TODO.md` change in the commit that changes the fact. Rulings made by a task go in its report as `Ruling: … — why — cost if wrong`.

## Shared vocabulary (decided here, used by every task)

- **World tick** `t: game::Tick` (`u64`): the one world-numbered tick every region counts (`WorldClock`). **Day** `D = t / TICKS_PER_DAY` (`u64`, days since tick 0). **Time in days, Q16** `tq = (t · 65536) / TICKS_PER_DAY` as `i64` (`t < 2^47` asserted in debug). **Day fraction** `df = tq & 0xFFFF` (Q16, 0 = the global midnight).
- **The sim date** (Task 1): `year = D / 336`, `day_of_year = D % 336` (0-based), `month = day_of_year / 28` (0-based; index `m` is calendar month `m + 1`, `MONTH_NAMES[m]`), `day = day_of_year % 28` (0-based). Day 0 of the year is the northern spring equinox (Phase 1b, Ruling 5). **Year fraction** `yq = ((t % TICKS_PER_YEAR) · 65536) / TICKS_PER_YEAR` (Q16), `TICKS_PER_YEAR = TICKS_PER_DAY · DAYS_PER_YEAR`. **Month position** `mq = yq · 12` (Q16 months since the year's start; a stored normal is anchored at `m + 0.5`).
- **Map metres, Q8** `MapPoint { x: i64, z: i64 }`: 1/256 m, x east from the west edge, z south from the north edge (Phase 1b's axes). From a canonical world block `(bx, bz)` (`GameData::world_block`): `x = bx · 256 + 128 + frame.origin_q8[0]`, likewise z (the block's centre); folded `x.rem_euclid(frame.width_q8)` when `frame.wrap_x`. **Height** `height_m: i32` = metres above the sea, floored at 0: `(block_y − frame.sea_top_block).max(0)`.
- **Longitude and latitude, Q16 degrees:** `lon = x · 360 · 65536 / width_q8` (0 … 360), `lat = 90·65536 − z · 180 · 65536 / height_q8` (north positive, clamped to ±90). All planetary lengths below are in degrees (the design: never metres), so every world size has the same weather in map fractions.
- **Local solar time** (Ruling 5): `sf = (df + lon / 360) mod 1` (Q16; 0 = local midnight, 0x8000 = local noon). The date is global; only the sun's phase follows the longitude.
- **Units:** temperature `i32` centi-°C; precipitation rate `u32` µm of water per calendar day (1000 = 1 mm/day); wind `[i32; 2]` in 1/64 m/s (east, south — the planes' sign convention); cloud `u8` 0–255; snow water equivalent and ice in µm (`i64`); snow depth in mm.
- **Earth days.** Literature rates per day (melt factors, freezing degree-days) are per EARTH day; a calendar day is `EARTH_DAYS_PER_GAME_DAY = 365.25 / 336`, carried as the exact ratio `1461 / 1344` in integer arithmetic.
- **Climate cell `(cx, cz)`** and **band** `b = height_m / OVERLAY_BAND_M` (`OVERLAY_BAND_M = 100`): the snow and ice lattice is the climate grid × 100 m height bands.

## Rulings (made while writing this plan; each `what — why — cost if wrong`; the rejected alternative named)

1. **The family reaches `game` as configuration set on the region, never as state; a remote client gets it once per session in one packet.**
   - *Server regions:* `server` builds ONE `Arc<game::WorldClimate>` at `install_world` from `worldgen::forged::chosen()` (`ForgedWorld::world_climate()`, Task 3), hands it to `WorldManager::set_world_climate`, which keeps it (a new field, classified `other` in `host_fields!`) and sends `RegionInput::SetClimate(Some(arc))` to every running region and to each region it starts — the path `route_map` takes. `Region::set_climate` writes `GameData::climate` raw (`#[serde(skip)] #[state_hash(skip)]`, no undo): setting it at any time is safe and no rollback reverts it. Procedural worlds and every test rig have `None`: every query returns `None` and nothing else differs — **the no-climate case is today's behaviour, bit for bit**.
   - *Multi-host and observers:* every host loads the same `forged_meta` blob from the one store, so every host builds the same `WorldClimate`; nothing crosses a peer or coordinator link. A host sends the packet to HOME sessions only (where it sends `WorldMap`); an observer connection carries none. The coordinator and the `fleet` role build none.
   - *Clients (single-player too — its world lives in the spawned server):* `ServerPacket::WorldClimate(WorldClimateWire)`, sent right after `ServerPacket::WorldMap`, before any `Region` packet of the session. The wire form is the frame plus the six planes row-delta encoded (`game::climate_wire`, moved out of `worldgen`, built on wasm32), then the ordinary `encode_packet` (bincode + lz4). **Bytes on the wire: measured in Task 11**; expected from Phase 1b's zstd-19 figures (0.38 / 0.39 / 1.04 / 2.64 MB for Small / Tiny / Medium / Large) times lz4's usual 1.5–2.5: about 0.6–1 MB on Tiny and Small, 1.6–2.6 MB on Medium, 4–6.6 MB on Large, once per session — under one near region snapshot. The server encodes the frame ONCE per process and keeps the bytes (`Arc<[u8]>`).
   - *wasm:* predicted regions run INLINE on the game thread in the browser (`RegionHost::Inline`), so they share one `Arc`; no worker receives the family. Mesh and compute workers never read it: what a job needs (a snow value) is computed on the main thread and carried in the job or drawn from a texture (Ruling 11). `ClientUpdateEvent::WorldClimate(Arc<WorldClimate>)` hands the same `Arc` to the render side (an in-process channel on both targets — *verify at dispatch*; if the browser's game instance is NOT in the renderer's memory, the event carries the packed bytes and the render side decodes a second copy: a ruling in Task 11's report).
   - *Memory per process:* server, role `all` and each host: the `ForgedWorld`'s resident copy plus ONE more inside the `Arc` (3.0 MiB Dev, 1.8 Tiny / Small, 7.0 Medium, 28.1 Large) plus the encoded packet (≤ about 7 MiB); client: one copy, and transiently the packed bytes and the bincode buffer while decoding (about 2 ×).
   - *Session reset:* the client's instance drops its `Arc` with the session; the render-side resource is registered for reset (Task 12).
   - *Rejected:* streaming only the cells around the player like LOD (a second prefetch radius, a stall or a guess whenever weather is asked past it, and far snow needs the far cells anyway); a per-world disk / IndexedDB cache keyed by the family's crc (worth it only if the measured bytes hurt a join — a Known limit, the crc is in the frame so it can be added without a format change); sharing the `ForgedWorld`'s own copy through an `Arc` inside `WorldArtifact` / `TileCache` (touches `worldgen`'s stored types while Phase 2 rewrites the crate).
   - *Cost if wrong:* +28 MiB per server process at Large (the pods' memory requests already carry an owner hand check after Phase 1b); a Large join downloads up to about 7 MB more.
2. **Weather and seasons follow the WORLD TICK and nothing else; the coordinator's calendar is not consulted, not changed, and no longer the date a player reads in play.**
   - *The date function:* the vocabulary's — pure integer arithmetic on `t` and `TICKS_PER_DAY`. Tick 0 is day 0 of year 0: the northern spring equinox, 1st `Granite`.
   - *Why the world tick:* it is the only clock every region, every host and every predicting client already agrees on bit for bit, the only one a rollback re-simulates with, and it is in every snapshot. The coordinator's calendar (`Coordinator::calendar_now`) is a different count: anchored in STORE milliseconds, known to a host only as a daily `ServerPacket::Calendar`, and **not caught up after downtime** (`CALENDAR_GAP_MS`, `calendar_resume`) — a value that changes by a message cannot be an input of a pure function evaluated inside a predicted tick.
   - *Where the two disagree* (read in `coordinator/mod.rs`, `coordinator/clock.rs`, `world_clock.rs`, `server/src/roles.rs` `start_clock`): they agree at a world's creation — the world tick starts at 0 and history's bootstrap starts at `CalendarTick::from_days(HISTORY_START_YEAR · DAYS_PER_YEAR)` and runs whole years *(verify at dispatch that `BootstrapParams::years` is whole years: the day-of-year of `history.now()` at the first start must be 0)* — and both advance one tick per `TICK_RATE` ms while everything runs. They drift ONLY at restarts: (a) role `all`: the world tick resumes from the SAVED `world_tick` (`WorldClock::starting_at(0, world_tick)`), the calendar from where it last stood — the calendar gains up to one save interval per crash, nothing at a clean stop; (b) split roles: the world tick is wall-anchored (`ClockAnchor`, first writer wins) and RUNS THROUGH downtime, the calendar does not — the world tick gains the whole of every outage longer than 30 s in which no coordinator replica ran; `worldctl clock-reset` re-anchors the world tick at the saved tick.
   - *What a player sees:* the HUD's date line and the sky show the SIM date — month, day and local season, **no year number** (Task 13). The history view and the dev pane keep showing the coordinator's `CalendarTick` (a year count of the world's history). After downtime on a split world the two month names differ; a restart never makes the season jump RELATIVE TO THE WORLD STATE (the weather a region sees is always the weather of its own tick), it makes it differ from the history's date.
   - *Rejected:* (i) the calendar as the weather's clock (needs a durable offset delivered to every region and client at one stamped tick each time the coordinator re-anchors: a world-level event, state, a message — everything the phase forbids); (ii) deriving the coordinator's calendar from the world tick (history would then simulate every day of an outage — 8,640 days for one day down at a 10 s day — which is exactly what Elastic 2a's "downtime is not caught up" exists to prevent; and it rewrites `projection!` state under an elastic build the owner has not accepted); (iii) a phase lock — the coordinator re-anchoring FORWARD by less than a year so the two keep one day-of-year (plausible, bounded history catch-up, but a change to `reanchor_calendar` and its replicas' rebase rules: the owner's decision, listed under "Owner hand checks", not built).
   - *Cost if wrong:* a player who compares the legends' dates with the sky finds two calendars after an outage; the cure is (iii), a coordinator change that needs no change here.
3. **The weather function is integer fixed point, end to end; `libm` only in the sun geometry.** Integers are bit-identical on native and wasm32 by construction (no `mul_add`, no rounding mode, no `f32` widening question), exact compares on centi-degrees decide freezing, and a hash roll is already an integer. The normals are read from the container's public integer planes through `index` (never `normals_at`, whose `f64` path is the convenience read). **No bit-identity with `worldgen::forged::ground_normals` is needed or claimed** — that function (`f64`) classifies biomes in Phase 5 from twelve monthly MEANS; the runtime produces instantaneous values whose long-run monthly means converge to the same normals. The agreement is statistical and is evidence (Task 15), not arithmetic. The lapse is the world's own, carried in the frame as micro-°C per metre (`round(lapse_c_per_km · 1000)`, one `f64` multiply and round done once by the server; a client receives the integer). *Rejected:* `f32` + `libm` (works, but every expression order becomes a determinism review item and thresholds stop being exact); `f64` exact arithmetic like `forged.rs` (no transcendental is needed anyway). *Cost if wrong:* none found; fixed-point overflow is the risk and each task's temporary tests sweep the extremes.
4. **Month interpolation: temperature and wind are piecewise linear between SHARPENED mid-month control points; precipitation and wet-day share are piecewise CONSTANT per month.** A stored normal is a month's mean. Plain linear interpolation between mid-months lowers every peak: its mean over month `m` is `(N[m−1] + 6·N[m] + N[m+1]) / 8`. The control points `c[m] = N[m] + (2·N[m] − N[m−1] − N[m+1]) / 8` (cyclic in `m`, integers) undo that to first order: for an annual sinusoid the month mean is then off by 0.11 % of the amplitude (0.03 °C on a 30 °C swing), for a semi-annual one by 1.6 %. Precipitation steps at month boundaries because the expectation proof (Ruling 7) is then exact per month and a step in a probability is invisible. *Rejected:* a periodic cubic (overshoots below zero for precipitation, more multiplies, no better mean); solving the cyclic tridiagonal system exactly per call (12 unknowns per read for 0.03 °C). *Cost if wrong:* a monsoon-shaped temperature curve is flattened by up to its second difference ÷ 64.
5. **The sun rises in the east: local solar time is a function of longitude (`sf = df + lon/360`); the DATE is global.** The world is a planet seen as a cylinder, Phase 3 models it rotating, and the function is pure in `x`, continuous across the world seam (a full turn is one day) and costs one add. At the default Medium size a point 4 km east is 14 minutes ahead; opposite sides of the world have opposite day and night. The calendar day changes everywhere at one tick (at different local hours, like a UTC date). The diurnal temperature term and the client's sun both use `sf`. *Rejected:* one time everywhere (simpler to meet at dawn, but then the diurnal cycle of a "planet" ignores its longitude and Phase 3's zonal means have no meaning at runtime). *Cost if wrong:* one `const SOLAR_TIME_BY_LONGITUDE: bool` in `season.rs`, read by both callers; flagged for the owner with the day length.
6. **Noise is Gaussian value noise on global lattices in degrees and days, variance-normalised, mapped to a uniform value by a committed Φ table.** A lattice corner's value is the sum of the four 16-bit chunks of `roll(lattice_t, corner_id, salt ^ seed_mix)`, centred (Irwin–Hall 4: mean 0, known variance). A sample is the trilinear blend of 8 corners with integer smoothstep weights (`3s² − 2s³`, Q16), DIVIDED by `isqrt(Σ w²)` so its variance is 1 at every phase of the lattice; then `u = PHI_Q16[g]` (a 513-entry table of the normal CDF over ±4σ, linear between entries). *Why not the research's "apply the noise's CDF as a lookup table":* plain interpolated uniform noise has a different distribution at a lattice corner (uniform) than at a cell centre (bell-shaped), so a fixed place would keep a biased wet-day share for as long as the wind does not move the field; normalised Gaussian sums are near-normal at every phase. The x lattice wraps by `rem_euclid` of the lattice count when the family wraps (the cell sizes below divide 360°). **This is the researcher's untested design: Task 5 measures it before anything trusts it** (uniformity, the wet share at fixed phases, the lag-1-day autocorrelation). *Cost if wrong:* the calibration misses its bars; the fallback is a second table (the measured CDF per phase class), still integers.
7. **Precipitation: one uniform field decides wet or dry against the month's wet-day share, and the same value gives the amount.** At `(P, t)`: `π = wet_day_255 / 255` of the month (piecewise constant, bilinear in space), `π' = clamp(π · f_year, 0, WET_SHARE_MAX)`, `f_year` the per-year factor (Ruling 9). Wet when `u < π'`. On a wet point `v = 1 − u / π'` (uniform in (0, 1], 0 at the edge of the rain, near 1 at its core) and `rate = (P_month / (28 · π)) · Q(v)` per day, `Q` the exponential quantile `−ln(1 − v)` as a 257-entry Q16 table whose discrete mean is exactly 1. **Expectation:** `E[rate | year] = π' · (P_month / (28·π)) · E[Q] = f_year · P_month / 28`, and `E[f_year] = 1` (a symmetric clamped Gaussian), so the long-run daily mean is `P_month / 28` and a month's total converges to its normal — exactly, up to the uniformity of `u` (Task 5's bar) and the storms' share (Ruling 8). A month with `π = 0` is dry; `P_month > 0` with `π = 0` cannot be represented and is counted by the summary tool (`dry_month_with_rain`). **The field:** two octaves (`SYSTEM_CELL_DEG = 12`, `SYSTEM_DAYS = 3`; the second at half both, weight ½, the pair re-normalised), sampled at a position ADVECTED by the stored wind with the flow-map trick: two samples whose displacement `−wind · (time since that clock's restart)` restarts every `ADVECT_PERIOD_DAYS = 6`, half a period apart, cross-faded with triangular weights and re-normalised by `isqrt(w² + (1−w)²)`. Wind → degrees per day: `WIND_DEG_PER_DAY_PER_MS = 0.777` (Earth's 86.4 km per day per m/s ÷ 111.2 km per degree — a planetary speed, like every length here). The wind is the bilinear, month-interpolated stored wind AT THE SAMPLE'S OWN POINT: continuous in space, bounded shear (≤ one period of displacement), closed form, no state. **The rain-shadow modulator is not applied at runtime** (`game` has no fine raster): weather is at climate-cell resolution (Known limit). *Rejected:* a Markov chain per cell (state, no spatial coherence); thresholding un-normalised noise (Ruling 6); a gamma amount (a table per shape class for no visible gain). *Cost if wrong:* persistence — wet spells too short or long — which is two constants and Task 5's measurement.
8. **Storms are hashed objects on a coarse space–time grid with straight closed-form tracks; they ADD rain and wind, and their share of the annual total is measured, not assumed.** Epochs of `STORM_EPOCH_DAYS = 4`, cells of `STORM_CELL_DEG = 30` (12 around, 6 pole to pole). For each (epoch, cell): one roll decides birth against a per-mille rate scaled by the birth point's monthly wet-day share (`STORM_BIRTH_PER_MILLE_AT_FULL_WET = 600`: a place with a 0.5 wet-day share spawns one in 30 % of its epoch-cells), further rolls give the birth point inside the cell, the birth time inside the epoch, lifetime 2–6 days, radius 3–8°, peak intensity. The track is `birth + velocity · (t − t_birth)`, `velocity` = the stored wind at the birth point for the birth month × `WIND_DEG_PER_DAY_PER_MS`, its length clamped so `STORM_MAX_TRAVEL_DEG (22) + STORM_MAX_RADIUS_DEG (8) ≤ STORM_CELL_DEG` (const-asserted): a storm can only reach the 3 × 3 cells around its birth cell, so every evaluation enumerates exactly 3 × 3 cells × 3 epochs = 27 candidates — the same 27 from any region, from both sides of any seam (x neighbours by `rem_euclid`; distance in x by the nearest image; a track that leaves the map north or south simply ends there: no storm crosses a pole). Inside a storm `s = envelope(t) · (1 − (d/r)²)²` (Q16): the point is forced wet with `rate += s · STORM_RATE_UM_PER_DAY`, cloud 255, wind += a tangential term. The label (`StormKind::{Tropical, Gale, Blizzard}`) is by the local temperature at evaluation and is for the HUD only. **Storm rain is extra** (it breaks Ruling 7's exactness on purpose): the summary tool prints its share and the constants are tuned until the 30-year mean share is ≤ 5 % at every one of the five places. *Rejected:* storms as a region of the field itself (not objects — the program asks for objects with tracks); recurving tracks (no evidence, more closed form to get wrong). *Cost if wrong:* annual totals a few per cent high where storms cluster.
9. **Years differ by one slow field: Gaussian value noise over (60° × 60° cells, YEARS), continuous in the year coordinate.** `g_wet`, `g_warm` (two salts) at `(lon, lat, year + yq)`; `f_year = 1 + YEAR_WET_SHARE (0.25) · clamp(g_wet, −2, 2)` scales the wet-day share (a dry year has fewer wet days, the same rain per wet day); the temperature offset is `YEAR_WARM_CENTI_C (80) · clamp(g_warm, −2, 2)`. Nothing jumps at New Year (the lattice is interpolated across it). No multi-year oscillation, no rare-event rolls (Known limits). *Cost if wrong:* two constants.
10. **Snow and ice are a BOUNDED REPLAY of the weather function at daily steps from each lattice point's snow-free date — a pure function, memoised outside all state; they are overlays OVER the baked terrain and never replace it.**
    - *The definition (uncached):* for climate cell `(cx, cz)`, band `b` and day `D`: the cell's **reset day** `d0` is the first day of the month after its warmest month (by the twelve sea-level normals, the first of a tie) — in the south that falls half a year later, by itself. The **snow year** of `D` starts at the latest `d0 ≤ D`. For each day `k` of it: `T_k` = the daily mean temperature at the cell centre and the band's mid height (seasonal + year + slow anomaly, NO diurnal term — its mean is zero by construction), `p_k` = the day's precipitation = the mean of `rate` at the four times `(k + 1/8, 3/8, 5/8, 7/8)`. Snowfall `= p_k · share(T_k)`, `share` linear from 1 at `SNOW_ALL_CENTI_C = −100` to 0 at `SNOW_NONE_CENTI_C = 300` (Phase 3's, *untested*); melt `= MELT_UM_PER_CENTI_C_DAY (40, i.e. 4.0 mm/°C/Earth day — recalled, a search summary) · max(T_k, 0) · 1461/1344`. `a_k = snowfall − melt`, prefix sums `A(k)`, and **`SWE(D) = A(D) − min_{d0−1 ≤ j ≤ D} A(j)`** (`A(d0−1) = 0`): the floored recurrence in closed form over stored prefixes. Depth `= SWE · SNOW_DEPTH_PER_SWE (5)`. Ice on a water column, same shape: index `I(D) = F(D) − min F(j)` with `f_k = (T_freeze − T_k) · 1461/1344` in centi-°C·days (`T_freeze` = `SEA_FREEZE_CENTI_C −180` on a sea column — `water_top == frame.sea_top_block` — else 0), thickness `= ICE_UM_PER_SQRT_CDAY (2_000: 2.0 cm per √(°C·day), the middle of the sourced-by-summary 1.4–2.5) · isqrt(I / 100)`, capped at `ICE_MAX_UM` (2 m). Melt is the same index falling (symmetric — *untested*, marked).
    - *Permanent snow and ice:* if the MEAN year at that cell and band (the same recurrence on the twelve normals, 28 equal days per month, no noise) ends its year with `A > 0`, the point is permanent: `SWE = SWE_CAP_UM` (5,000 mm) and the replay is skipped. Otherwise the replay starts from 0 at `d0`. An unusually cold year can leave snow at `d0` that the reset then drops in one day at high bands — a Known limit, measured by the summary tool (`snow_at_reset_mm`).
    - *At a column:* bilinear between the four climate cell centres around the point, linear between the two bands around `height_m` — 8 lattice values. Identical from any region: every input is global.
    - *The cache:* `game::overlay::OverlayCache` holds, per `(cx, cz, snow-year)`, the cell's daily series (`T_k` at sea level, `p_k`) filled lazily to the day asked, and per band the prefix sums and running minima. It is a plain struct, never in `GameData`, never hashed or serialized; `cached == uncached` is checked bit for bit (Task 9). A woken region or a new client recomputes from nothing: at most 336 days × 5 weather calls per cell (about 1 ms per cell at the 1 µs budget), then one integer step per band and day. The server holds no cache at all (nothing there asks per tick).
    - *The baked terrain stays:* today's genesis blocks — the snowy ground cover above the ANNUAL snow line and in tundra patches, glacier ice, the ice blocks on cold seas and lakes (`biome_column`: `ice_from`, `ice_blocks`, `glacier_blocks`) — are the PERENNIAL state and are not changed by this phase (changing a genesis block changes worldgen's output under Phase 2's hands). The overlays are the SEASONAL state drawn over them: snow over any dry column top (a baked snowy cover just looks the same), ice ONLY over a column whose top recipe water cell is open water. Phase 4's stored glacier thickness is terrain and is never read here. Where the two models disagree (a baked ice sheet at the edge of the zone the replay calls ice-free in summer) the baked blocks win: a Known limit until a later phase re-bakes from the monthly family.
    - *Rejected:* a stored or forge-computed twelve-value normal curve (cheaper, but yesterday's snowfall leaves no trace and "weather, snow and ice agree" becomes a matter of opinion; it also wants a format change if stored); a per-column replay (336 × columns); the hybrid of the two (two models to keep in step). *Cost if wrong:* a cold start costs milliseconds per climate cell on the client — amortised per frame (Task 14) — and the reset snap above.
11. **Snow and ice are NOT collidable, NOT blocks, and nothing in the sim reads them in this phase: they are queryable values and a client-side tint.** "In the manner of `water_top`": recipe water has no collider either — the sim reads it through one function (`Rollback::water_units_at`). Snow you sink into needs no collider; ice you can WALK on needs a body, a collider that appears and disappears at a tick edge inside rapier's undo log, and the swim and drown rules to know a lid exists — that is the water engine's territory, which this program excludes ("stored but not played … floating ice"). So in this phase: `GameData::overlay_at` answers depth and thickness for any column and tick; the client tints the near ground and water from it; no tick of the sim calls it. The overlay therefore cannot move a hash, a golden or a desync check — and it never re-meshes anything: the tint is a small world-space texture refreshed when the calendar day or the camera's chunk changes (Task 14), not a mesh input. *Rejected:* snow layers as geometry in the mesher (a re-mesh of every near brick each time a depth step crosses — a storm of them at a 10 s day); snow or ice as real blocks (mass edits: rollback, save and network cost — the research's and the audit's shared verdict). *Cost if wrong:* walkable ice and deep snow that slows a body are later features that will be sim-visible; they then need the family on the client BEFORE its first predicted tick — the ordering Task 11 already establishes.
12. **What is real state: nothing, in Tasks 1–16. Crops, soil moisture and every gameplay effect of weather are OUT of this phase.** The design's Decision 8 names crops and soil moisture as "real region state" in the same breath; the program's Phase 6 section does not list them, there is no farming in the game to hang them on, and they are region state with their own save, seam and catch-up rules. Likewise out: cold or heat damage, wet ground, slipping, visibility for creatures, fire weather, river level, tides. Each is a Known limit.
13. **Player edits as sparse deltas with an expiry are DESIGNED here and HELD (Task 17): they cannot land without re-recording gates this program may not re-record.** They are real region state by the program's own words, and any hashed, serialized field added to `GameData` — even an empty `BTreeMap` — changes `state_hash` (the map's length is hashed: `state-hash`'s `BTreeMap` impl feeds `hash_len`) and the bincode bytes of every `SerializedRegion`, so `wasm_determinism`'s three pinned scenes (digest AND state hash) and `scene_hash_golden` all move, and every saved region blob stops decoding. Re-recording any gate but the forge quality gate is outside what starting the program approved. The design (so that approval is the only thing missing): `GameData::overlay_clears: BTreeMap<(i32, i32), Tick>` (`#[undo(map)]`, hashed, in the snapshot — so in the owning region's cut part and nowhere else), keyed by the region-LOCAL block column, written by the existing terrain-edit events (a block mined or placed in a column stamps that column with the event's tick: no new input, no protocol change); a reader treats a stamp `c` as "zero at day `c`": `SWE = A(D) − min_{max(d0−1, c) ≤ j ≤ D} A(j)` — the same prefixes, a later lower bound; **expiry is `tick ≥ stamp + OVERLAY_CLEAR_TTL_DAYS (336) · TICKS_PER_DAY`**, a pure function of the tick (no timer, no climate read — so the sim never depends on the family's values); bounded by `OVERLAY_CLEARS_MAX = 4096` per region, the oldest stamp (ties: the smaller key) evicted on insert; a parked region keeps its stamps in its saved blob, a regenerated one has none (nothing was ever edited there); a stamp never crosses a seam (a column belongs to one region; the halo copy of a neighbour's border columns shows the neighbour's uncleared snow in the one-chunk halo band — a Known limit of the held task). *Cost of holding:* the phase ships without "a cleared path": digging does not clear snow. *Rejected:* a hash- and serde-transparent field (a custom `StateHash` that feeds nothing when empty still changes the bincode layout; `skip_serializing_if` does not work with bincode), deriving a stamp from the edited chunk (no time in it, so no expiry).
14. **The client's minimum, and what is left out.** IN: the sun's direction and strength and the sky's brightness follow the date, the latitude and the local solar time (polar day and night included) (Task 12); one HUD line — date, local season name, temperature, sky state — and the dev pane's numbers (Task 13); snow and ice as a tint on the near ground and water (Task 14). OUT (Known limits): rain and snow particles, clouds, weather fog, wind on trees, lightning, sound, snow on the far LOD rings, foliage colour and leaf fall, a forecast UI, the world map's season layer. The evidence needs none of the visuals; the HUD line is what makes a hand check possible.
15. **Season names are by hemisphere and by the place's own climate.** `season_name(lat, month_normals, date)`: if the place's annual temperature range is under `SEASONLESS_RANGE_CENTI_C (500)` — `Wet season` / `Dry season` when the wettest month has over three times the driest month's rain (the season is wet while the current month's normal is above the year's mean), else no name; otherwise the four seasons by quarter of the year from the spring equinox, shifted by two quarters when `lat < 0`. Thresholds are the research's inference (*untested*).
16. **Performance budget and where the function runs.** Server: nowhere per tick (Ruling 12); on demand only (tools, a future sim reader). Client: once per frame at the camera for the HUD and sky (one `weather_at`), and the overlay texture's fill — budgeted `OVERLAY_CELLS_PER_FRAME` cold cells per frame. Budget: `weather_at` ≤ 1 µs native release (16 plane reads per interpolated normal set, about 75 rolls, no allocation, no `dyn`); a cold overlay cell ≤ 2 ms; recorded in Task 18, no pass line — a figure over twice the budget is a finding. `MonthlyClimate::read`'s `&dyn Fn` (Phase 1b's deferred minor) is not on this path: the integer sampler reads the planes directly.
17. **The wasm determinism gate is not extended; the weather function is compared across targets by a temporary test pair run with the gate's own two commands.** A new pinned hash is a new gate, which needs the owner. Task 16 adds, temporarily, a native `#[test]` and a `#[wasm_bindgen_test]` to `crates/game/tests/wasm_determinism.rs` that print one crc32 over `weather_at`, `overlay` and `solar` on a lattice of (tick, point, height) with a synthetic hash-filled `WorldClimate`; the two printed values must be equal; both tests are deleted. `just determinism-wasm` itself proves only that no pinned value moved and that `game` builds for wasm32.

**Research claims this plan builds on that were never tested or never read** (each gets its evidence step): stateless occurrence and amount by thresholded noise (Task 5: uniformity, wet share, persistence); storms as epoch-hashed objects (Task 8: enumeration completeness; Task 15: share); bounded snow replay (Task 9: `cached == uncached`, the reset snap; Task 15: cover days); the degree-day factor, the Stefan coefficient, the snow-share ramp, the diurnal shape and its cloud damping, every amplitude of the anomaly fields — *recalled or working assumptions*, named constants, judged only by the year summary. Geng's coefficients are not used.

## Review Focus (where this phase is most likely to go wrong)

1. **A region-local input sneaks in** — a lattice origin, a cache key, a point computed from local block coordinates without `world_block` and the frame, x not folded before the lattice lookup. Symptom: weather differs across a region seam or the world seam. Every public function takes `MapPoint`; only `GameData::weather_at` / `overlay_at` convert (Tasks 4, 10).
2. **Configuration treated as state** — `GameData::climate` hashed, serialized, logged for undo, cloned into a snapshot packet, or `carry_components` / `portable.rs` / `host_fields!` not updated. Symptom: a pinned digest moves, or a 28 MiB `Arc` content is serialized per region (Task 4).
3. **Fixed-point overflow and negative division** — `lon · 360 · 65536` on a Q8 x of a 200 km world fits `i64` only in the order written; `/` on a negative centi-degree rounds toward zero and breaks continuity at 0 °C; a Q16 × Q16 product needs `i64`. Symptom: a step in temperature at 0 °C or at the seam (Tasks 3, 6, 7).
4. **The two clocks** — any use of `CalendarTick`, `Coordinator::calendar_now` or `ServerPacket::Calendar` in weather, sky or the HUD's date. They are the history's (Ruling 2).
5. **Datum** — the container's temperature is sea level; the lapse is applied once, from `height_m` floored at 0, in integers; the snow lattice uses the band's mid height. A double or missing lapse is 13 °C at 2,000 m and still looks plausible.
6. **Month and hemisphere phase** — month index 3 is the northern midsummer; the control points are cyclic; the south's seasons and reset day must come out half a year later with no sign flip in code. The five-place summary (a pole and the equator) is the check.
7. **The expectation** — any term that multiplies `rate` without a matching term in the proof (cloud damping, storm blending, the cross-fade without re-normalisation) biases monthly totals. The 30-year means in Task 15 are the check; storms are the one deliberate exception and are measured.
8. **Day-length independence** — a constant in ticks, an animation per tick, a cache keyed by tick instead of day, a division by `TICKS_PER_DAY / n`. Temporary tests run the pure functions at two synthetic day lengths where the API allows (the functions take `tq`, not ticks, below the top level).

---

### Task 1: The date and the sun — `game::season`, `game::solar`

**Files:**
- Create: `crates/game/src/season.rs`, `crates/game/src/solar.rs`
- Modify: `crates/game/src/lib.rs` (`pub mod season; pub mod solar;` and `pub use season::*; pub use solar::*;` — the owner's uncommitted hunks are in this file: stage only these lines; check the root glob for name collisions first: `Date` exists in `history::calendar` and is NOT re-exported at the root *(verify)* — the new type is `SimDate`)

**Interfaces — produces:**

```rust
// season.rs — integers only
pub const TICKS_PER_YEAR: u64 = TICKS_PER_DAY * DAYS_PER_YEAR;      // const-assert TICKS_PER_DAY >= 1
pub const SOLAR_TIME_BY_LONGITUDE: bool = true;                     // Ruling 5
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimDate { pub year: u64, pub month: u8 /*0..12*/, pub day: u8 /*0..28*/, pub day_of_year: u16 /*0..336*/,
                     pub day_frac_q16: u16, pub year_frac_q16: u16 }
pub fn sim_date(tick: Tick) -> SimDate;
pub fn days_q16(tick: Tick) -> i64;                                  // tq
pub fn solar_frac_q16(day_frac_q16: u16, lon_q16: i64) -> u16;       // sf; = day_frac when !SOLAR_TIME_BY_LONGITUDE
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Season { Spring, Summer, Autumn, Winter, Wet, Dry }
/// Ruling 15. `temp_centi_c` / `precip_deci_mm`: the place's twelve normals (bilinear, sea level is fine).
pub fn season_at(lat_q16: i64, temp_centi_c: &[i32; 12], precip_deci_mm: &[i32; 12], date: SimDate) -> Option<Season>;
impl Season { pub fn name(self) -> &'static str; }
impl std::fmt::Display for SimDate;                                  // "12 Granite" — day + 1, MONTH_NAMES[month], NO year

// solar.rs — f64 + libm, fixed expression order
pub const AXIAL_TILT_DEG: f64 = 23.44;                               // sourced (Wikipedia: Position of the Sun)
/// Declination (rad): `tilt · sin(2π · year_frac)`; the year starts at the northern spring equinox.
pub fn declination_rad(year_frac_q16: u16) -> f64;
/// sin(elevation) = sin φ sin δ + cos φ cos δ cos h, h = 2π · (solar_frac − 0.5).
pub fn sun_elevation_sin(lat_q16: i64, year_frac_q16: u16, solar_frac_q16: u16) -> f64;
/// Unit vector TOWARD the sun in engine axes (+x east, +y up, +z south).
pub fn sun_direction(lat_q16: i64, year_frac_q16: u16, solar_frac_q16: u16) -> [f64; 3];
/// Daylight share of the day in 0..=1: acos(clamp(−tan φ tan δ, −1, 1)) / π (1 = polar day, 0 = polar night).
pub fn daylight_share(lat_q16: i64, year_frac_q16: u16) -> f64;
```

- [ ] **Step 1: Temporary tests** (`#[cfg(test)] mod tmp` in each file; `just test-one game "--lib -- season:: solar::"`): `sim_date(0)` is year 0, month 0, day 0; the tick `TICKS_PER_DAY · (336 + 28·3 + 5) + TICKS_PER_DAY / 2` is year 1, month 3, day 5, `day_frac_q16 == 0x8000`; `year_frac_q16` is monotone and wraps once per `TICKS_PER_YEAR`; `Display` prints `6 Hematite`; `solar_frac_q16(0, 180·65536) == 0x8000`, and `lon = 0` and `lon = 360·65536` agree; `declination_rad` is 0 at 0, `+tilt` at a quarter year, `−tilt` at three quarters (1e-9); at latitude 0 on an equinox `daylight_share` is 0.5 and the noon sun is overhead (`sun_direction` y within 1e-6 of 1); at 80° N at the northern summer solstice `daylight_share == 1.0` and at the winter one `0.0`; daylight at `φ` and `−φ` sums to 1 on 64 (lat, date) pairs; `sun_direction` at local 06:00 on an equinox points east (+x) at any latitude; `season_at` gives `Summer` at 45° N in month 3 and `Winter` at 45° S in the same month, `Wet` / `Dry` for a flat-temperature place with a 5:1 rain ratio, `None` for a flat place with flat rain.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify, in order** — temporary tests pass; DELETE; `just smoke`; `just determinism-wasm` (values unchanged).
- [ ] **Step 4: Commit** `feat(game): the sim date from the world tick; sun geometry by date and latitude`.

### Task 2: The family's wire codec moves into `game`

**Files:**
- Create: `crates/game/src/climate_wire.rs`; modify `crates/game/src/lib.rs` (module + re-export, own lines only)
- Modify: `crates/worldgen/src/forged.rs` (`climate_to_wire`, `climate_from_wire`, `delta_plane`, `undelta_plane` and their `#[cfg(not(target_arch = "wasm32"))]`: the bodies become calls into `game`, or the functions are deleted and their four call sites — `WorldArtifact::to_bytes` / `from_bytes`, `encode_meta` / `decode_meta` in `forged_tiles.rs` — call `game` directly). Phase 2 edits this file: stage only these hunks; if the helpers moved or changed, re-derive and say so.

**Interfaces — produces** (all targets, wasm32 included):

```rust
impl MonthlyClimate {
    /// Row-delta encodes every month plane in place (rows of `nx`; each value but a row's first becomes its
    /// wrapping difference to its left neighbour). Never indexes past a plane (`chunks_mut(nx.max(1))`).
    pub fn delta_encode(&mut self);
    /// The inverse. Call `validate` on untrusted input FIRST.
    pub fn delta_decode(&mut self);
    /// crc32 of the grid, `wrap_x` and the six planes (little-endian), the same on every target.
    pub fn content_crc(&self) -> u32;
}
```

- The bytes `worldgen` writes must not change: this is a move, not a new format.

- [ ] **Step 0: Baseline** — `just forge-report --size dev --cell 32`: save the lines `world_file_bytes`, `climate_bytes_packed`, `artifact_climate_crc`, `artifact_rasters_crc`, `artifact_header_crc` to the ledger directory (`codec-before.txt`).
- [ ] **Step 1: Temporary tests** (`game --lib climate_wire::`): on a hand-built 5 × 3 grid with hash-filled planes `delta_decode(delta_encode(c)) == c`; a plane truncated by one value encodes and decodes without a panic; `content_crc` differs when one value of any plane, `nx` or `wrap_x` changes; (worldgen, `--lib forged::`, one dev world at 64 m) `WorldArtifact::from_bytes(&a.to_bytes()) == a`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence** — the Step 0 command again: the five lines identical (`diff`); the quality gate untouched: `just test-one worldgen "--test it -- --include-ignored forge_lib::"`.
- [ ] **Step 4: Verify, in order** — temporary tests pass; DELETE; `just smoke`; `just determinism-wasm`; `just web-check`.
- [ ] **Step 5: Commit** `refactor(game): the monthly family's row-delta codec lives in game (builds on wasm32)`.

### Task 3: `game::WorldClimate` — the frame and the integer normals

**Files:**
- Create: `crates/game/src/world_climate.rs`; modify `crates/game/src/lib.rs` (own lines)
- Modify: `crates/worldgen/src/forged.rs` (ONE new `pub fn` on `ForgedWorld`, below `monthly_climate`)

**Interfaces — produces:**

```rust
/// Everything a pure weather read needs beside the tick: the monthly family and an integer frame.
/// Configuration, never state: not hashed, not in a snapshot (it is sent once, in its own packet).
#[derive(Clone, Debug, PartialEq)]
pub struct WorldClimate { pub frame: ClimateFrame, pub monthly: std::sync::Arc<MonthlyClimate> }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClimateFrame {
    pub seed: u64,
    /// Map metres (Q8) of engine metre (0, 0): `round((globals.spawn − ENGINE_SPAWN_M) · 256)`.
    pub origin_q8: [i64; 2],
    pub width_q8: i64, pub height_q8: i64, pub cell_q8: i64,
    pub wrap_x: bool,
    /// Cooling per metre of height, micro-°C: `round(lapse_c_per_km · 1000)`.
    pub lapse_uc_per_m: i64,
    /// Block Y of the sea's top water block (`pipeline::SEA_TOP_BLOCK`).
    pub sea_top_block: i32,
    /// `monthly.content_crc()`.
    pub monthly_crc: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapPoint { pub x: i64, pub z: i64 }

/// One place's month-interpolated normals at a date (Ruling 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NormalsNow { pub temp_sea_centi_c: i32, pub diurnal_centi_c: i32, pub precip_um_per_month: i64,
                        pub wet_q16: i32, pub wind_q6: [i32; 2] /* 1/64 m/s east, south */ }

impl WorldClimate {
    pub fn new(frame: ClimateFrame, monthly: Arc<MonthlyClimate>) -> Result<Self, String>; // validate + the frame's checks
    pub fn point_of_block(&self, world_block_x: i64, world_block_z: i64) -> MapPoint;      // block centre, x folded
    pub fn fold(&self, p: MapPoint) -> MapPoint;
    pub fn lon_q16(&self, p: MapPoint) -> i64;  pub fn lat_q16(&self, p: MapPoint) -> i64;
    pub fn height_m(&self, block_y: i32) -> i32;
    /// The four cells around `p` and their Q16 weights (cell centres; x by `index`'s wrap, z clamped).
    pub fn cell_weights(&self, p: MapPoint) -> ([(i64, i64); 4], [i64; 4]);
    pub fn normals_now(&self, p: MapPoint, month_q16: i64) -> NormalsNow;
    /// The twelve bilinear normals of a place (season names, the summary tool): sea-level centi-°C, deci-mm.
    pub fn normals_year(&self, p: MapPoint) -> ([i32; 12], [i32; 12]);
    pub fn resident_bytes(&self) -> usize;
}
// worldgen
impl ForgedWorld { pub fn world_climate(&self) -> Result<game::WorldClimate, String>; }
```

- `WorldClimate::new` refuses: `monthly.validate()` fails; `cell_m · 256` is not a whole number or `nx · cell_q8 != width_q8` (likewise z); `monthly_crc` mismatch; `width_q8 > 2^40`.
- `ForgedWorld::world_climate`: the frame from `globals()` (`seed`, `width_m`, `height_m`, `spawn`), `lapse_per_m()`, `SEA_TOP_BLOCK`, `monthly_climate().clone()` into the `Arc` (the one extra copy of Ruling 1); refuses (an `Err` the server logs once, then runs with no climate) when `(spawn − ENGINE_SPAWN_M) · 256` is not a whole number.
- `normals_now`: `m0 = floor(month_q16/65536 − 0.5)` cyclic, weight `w` toward `m0 + 1`; temperature and wind from the sharpened control points `c[m] = N[m] + (2N[m] − N[m−1] − N[m+1]) / 8` (computed AFTER the bilinear blend of each of the four months `m0−1 … m0+2`, in `i64`, floor division); precipitation and wet share from month `floor(month_q16/65536)` alone; `wet_q16 = wet_day_255 · 65536 / 255`.

- [ ] **Step 1: Temporary tests** (`game --lib world_climate::`, a hand-built 8 × 4 wrapping grid of 256 m cells): at a cell centre and mid-month of a year whose normals are constant, `normals_now` returns the raw cell values; `point_of_block(bx)` and `point_of_block(bx + width)` and `(bx − 3·width)` are EQUAL for 256 `bx` (integers: no "nearly"); a point 1 m west of the seam interpolates between columns 7 and 0; north of row 0 clamps; with `temp[m] = round(1000 · cos(2π(m + 0.5 − 3.5)/12))` centi-°C the mean of `normals_now` over 2,800 evenly spaced times of each month is within 3 of `N[m]` (plain linear fails this at the peak by about 34 — assert that too, on a private unsharpened path, to see the test bite); precipitation is constant inside a month and equals `N[m] · 100` µm; `lon_q16` of the east edge is `360·65536`; `lat_q16` is `+90·65536` at `z = 0` and `−90·65536` at `z = height`; `new` refuses a frame whose crc is wrong; (worldgen, one dev world at 64 m) `world_climate()` is `Ok`, its `monthly` equals `monthly_climate()`, and for 64 block centres `normals_year(point_of_block(bx, bz))` agrees with `monthly_normals(bx + 0.5, bz + 0.5)` within one stored step per field (the engine→map conversion and the fold agree with `to_artifact`).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify, in order** — temporary tests pass; DELETE; `just smoke`; `just determinism-wasm`; `just web-check`.
- [ ] **Step 4: Commit** `feat(game): WorldClimate — the monthly family behind an integer frame, with month-interpolated normals`.

### Task 4: The climate as region configuration

**Files:**
- Modify: `crates/game/src/state.rs` (`GameData` gains `climate`, beside `map_extent`, same attributes and doc), `crates/game/src/region.rs` (`Region::set_climate`, `Region::climate`), `crates/game/src/region_runner.rs` (`RegionInput::SetClimate`), `crates/game/src/halo.rs` (`GameData::set_climate` for a render copy, beside `set_map`), `crates/game/src/portable.rs` (the destructure that names `map_extent: _` gains `climate: _`), `crates/game/src/world_manager/mod.rs` (`WorldManager` field `climate`, `set_world_climate`, the send beside each `RegionInput::SetRouteMap`), `crates/game/src/world_manager/succession.rs` (`host_fields!`: `climate` classified `other` — it names no host)
- Modify: `crates/server/src/world_file.rs` (`install_world` also returns the climate, or a sibling `world_climate()`), `crates/server/src/lib.rs` (hands it to the manager where it hands the world map)

**Interfaces — produces:**

```rust
// state.rs, in GameData
#[serde(skip)] #[state_hash(skip)]
climate: Option<std::sync::Arc<crate::WorldClimate>>,
// region.rs
impl Region { pub fn set_climate(&mut self, c: Option<Arc<WorldClimate>>); pub fn climate(&self) -> Option<&Arc<WorldClimate>>; }
// region_runner.rs
RegionInput::SetClimate(Option<std::sync::Arc<crate::WorldClimate>>)   // applied on receipt, like SetRouteMap
// world_manager
impl WorldManager { pub fn set_world_climate(&mut self, c: Option<Arc<WorldClimate>>); pub fn world_climate(&self) -> Option<&Arc<WorldClimate>>; }
```

- Written RAW, never logged (the `map_extent` pattern: `*self.data.climate.raw_mut() = …`, or the plain field if the macro leaves an attribute-less field plain — *verify how `map_extent` is declared at dispatch and copy it exactly*). A region built from a snapshot gets it again from its runner; `Rollback` clones share the `Arc`.
- Every place the manager sends `SetRouteMap` to a region it starts or re-seeds sends `SetClimate` too (search `RegionInput::SetRouteMap`).

- [ ] **Step 1: Temporary tests** (`crates/game/tests/` on the existing manager rig, in an existing `it` file): (a) **the no-move check** — a region with `set_climate(Some(..))` and one without, same snapshot, same 200 ticks of the same inputs: `state_hash` and `SerializedRegion::from_rollback(..).0` are byte-identical at every tick; (b) a region reconciled / rolled back 50 times keeps its climate (`climate().is_some()`); (c) a region the manager starts after `set_world_climate` has it before its first tick; (d) `forget_host` / `mentions` still compile and a host forgotten leaves the manager's climate in place.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify, in order** — temporary tests pass; `just test-one game "--test it -- world_manager"` *(the rig's filter; with `--features test-hooks` where the memory note says)*; DELETE; `just smoke`; `just determinism-wasm` (every printed digest and state hash unchanged — this is the task that would move them if the field were state).
- [ ] **Step 4: Commit** `feat(game,server): the world's climate reaches every region as configuration`.

### Task 5: Noise, tables and the calibration evidence

**Files:**
- Create: `crates/game/src/weather/mod.rs` (module root, salts), `crates/game/src/weather/noise.rs`, `crates/game/src/weather/tables.rs`, `crates/game/examples/weather_calibrate.rs`
- Modify: `crates/game/src/lib.rs` (own lines), `justfile` (recipe `weather-calibrate`: `{{cap}} {{cargo}} run --release -p game --example weather_calibrate -- {{args}}`)

**Interfaces — produces:**

```rust
// noise.rs
/// One lattice: cell sizes in Q16 degrees and Q16 days; `nx_cells` Some(n) wraps x by rem_euclid(n).
pub struct Lattice { pub cell_lon_q16: i64, pub cell_lat_q16: i64, pub period_q16: i64, pub nx_cells: Option<i64>, pub salt: u64 }
/// Unit-variance Gaussian value noise, Q12 (4096 = 1σ), clamped to ±4σ.
pub fn gauss(seed: u64, l: &Lattice, lon_q16: i64, lat_q16: i64, t_q16: i64) -> i32;
/// Two blended unit Gaussians re-normalised to unit variance; `w_q16` is the first one's weight.
pub fn blend(a_q12: i32, b_q12: i32, w_q16: i64) -> i32;
pub fn isqrt(v: u64) -> u64;
// tables.rs — committed const arrays, generated ONCE by the example (`--emit-tables`) and pasted
pub const PHI_Q16: [u16; 513];        // normal CDF over −4σ..=4σ
pub const EXP_QUANTILE_Q16: [u32; 257]; // −ln(1−v), discrete mean exactly 65536 (the last entry absorbs the tail)
pub const DIURNAL_SHAPE_Q15: [i16; 24]; // mean exactly 0, max +32767 at 15:00, min near 05:00 (untested shape)
pub fn uniform_q16(g_q12: i32) -> i32;   // PHI lookup, linear between entries; 0..=65535
pub fn exp_quantile_q16(v_q16: i32) -> i64;
pub fn diurnal_q15(solar_frac_q16: u16) -> i32;
```

- A corner's value: `r = roll(lattice_t as u64, corner_id, l.salt ^ seed.rotate_left(17))`, `corner_id = ix (wrapped) · 0x9E37_79B9 + iz · 0x85EB_CA6B` mixed into one `u64` *(any fixed injective mix of the two `i32` indices; state it in the doc)*; value = (the sum of `r`'s four 16-bit chunks) − 131,070. Weights: smoothstep of each fraction in Q16; the sample is `Σ w·c` scaled by `1 / isqrt(Σ w²)` and by the constant that makes one corner 1σ.
- The example (`weather_calibrate`, no world needed): (1) `--emit-tables` prints the three tables as Rust source (this is the one place std float math is allowed: an offline generator; the committed integers are the truth); (2) default mode prints the calibration report below for the precipitation lattice of Ruling 7 at zero wind and at a 6 m/s wind.

- [ ] **Step 1: Temporary tests** (`game --lib weather::`): `gauss` is identical for `lon` and `lon + 360·65536` on a wrapping lattice (1,024 points × 16 times); continuous (two points 1/256° apart differ by under 64 in Q12); the same from two call orders; `isqrt` exact on squares and `u64::MAX`; `uniform_q16` is monotone, `uniform_q16(0) == 32768 ± 1`; the mean of `EXP_QUANTILE_Q16` is 65536 exactly; the mean of `DIURNAL_SHAPE_Q15` is 0 exactly; `blend(a, b, w)` has unit variance within 2 % over 10⁵ rolled pairs for `w` in {0, ¼, ½, ¾, 1}.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence — the prototype the research never ran** (`just weather-calibrate`; quote all of it). Over 10⁶ samples at random (lon, lat, t): `uniform.ks` (the largest gap between the empirical CDF of `u` and the diagonal) — **bar ≤ 0.01**. At 64 FIXED places chosen at lattice corners, edge midpoints and cell centres, 300 years of four-hourly samples each, for `π` in {0.05, 0.1, 0.2, 0.3, 0.5, 0.8}: `wet_share.max_abs_err` (the worst |share of samples with `u < π` − π|) — **bar ≤ 0.015 at zero wind, ≤ 0.01 at 6 m/s**. `lag1_day_autocorr` of the wet indicator at `π = 0.26` — **target 0.2–0.45** (Kansas: 0.21; the research's 0.2–0.4), with `mean_wet_spell_days` (target 1.5–3) and `mean_dry_spell_days` printed beside the Markov values for the same `π` (1.7 and 4.9). A missed bar is tuned here (`SYSTEM_DAYS`, the octave weight, the Φ table's range) or, for the fixed-place bias, fixed by the fallback of Ruling 6 — never carried forward as "close enough".
- [ ] **Step 4: Verify, in order** — temporary tests pass; DELETE; `just smoke`; `just determinism-wasm`.
- [ ] **Step 5: Commit** `feat(game): weather noise — normalised Gaussian value noise, the uniform and quantile tables, their calibration tool`.

### Task 6: Temperature

**Files:** create `crates/game/src/weather/temperature.rs`; modify `weather/mod.rs`.

**Interfaces — produces:**

```rust
pub const ANOMALY_CELL_DEG: i64 = 20;          pub const ANOMALY_DAYS: i64 = 8;        // untested (research: 5–10 day features)
pub const ANOMALY_CENTI_C_PER_SIGMA_MIN: i32 = 150; pub const ANOMALY_CENTI_C_PER_SIGMA_MAX: i32 = 450; // untested
pub const YEAR_CELL_DEG: i64 = 60;             pub const YEAR_WARM_CENTI_C: i32 = 80;  // Ruling 9
pub const DIURNAL_CLOUD_DAMPING_255: i32 = 153; // a full overcast keeps 40 % of the range (untested)
/// The year's two slow Gaussians at a place: (wet, warm), Q12, each clamped to ±2σ.
pub fn year_anomaly(wc: &WorldClimate, p: MapPoint, tick: Tick) -> (i32, i32);
/// Daily-mean temperature at sea level: sharpened seasonal + year offset + slow anomaly. No diurnal term, no lapse.
pub fn temp_daily_sea_centi_c(wc: &WorldClimate, p: MapPoint, tq: i64) -> i32;
/// The instantaneous temperature at `height_m`: the above + the diurnal term − the lapse.
pub fn temperature_centi_c(wc: &WorldClimate, p: MapPoint, height_m: i32, tick: Tick, cloud_255: u8) -> i32;
```

- Slow anomaly amplitude per σ: linear in the place's annual range of the control points (the continental winter is more variable): `MIN` at a range of 0, `MAX` at 30 °C or more.
- Diurnal term: `(diurnal_centi_c / 2) · diurnal_q15(sf) / 32767 · (255 − DIURNAL_CLOUD_DAMPING_255 · cloud / 255) / 255 · day_gate`, `day_gate` = 1 unless the place has no sunrise or no sunset that day, where it is 0 — decided in integers from `lat`, `yq` by a committed 91-entry table of `tan` in Q16 (the product `tan φ · tan δ` against 1), not by `solar.rs`'s floats.
- Lapse: `− lapse_uc_per_m · height_m / 10_000` (centi-°C; floor division).

- [ ] **Step 1: Temporary tests** (`game --lib weather::temperature::`, the hand-built climate of Task 3 with a cosine year): the mean of `temp_daily_sea_centi_c` over 30 years of daily samples is within 20 centi-°C of each month's normal at 16 places; the mean of `temperature_centi_c − temp_daily_sea_centi_c` over one day (96 samples) is within 2 centi-°C of `−lapse` at any height (the diurnal term is mean-zero); at 2,000 m with a 6.5 °C/km lapse the value is 1,300 lower than at 0 m, exactly; identical at `p` and its image one width away; continuous across midnight, a month boundary and New Year (adjacent ticks differ by under 5 centi-°C over 10⁴ consecutive ticks at four places); at 85° in midwinter the diurnal term is exactly 0; `year_anomaly` at year `y + 0.999` and `y + 1.001` differ by under 1 % of σ.
- [ ] **Step 2: Implement.** — [ ] **Step 3: Verify, in order** (temporary tests; DELETE; `just smoke`; `just determinism-wasm`). — [ ] **Step 4: Commit** `feat(game): runtime temperature — season, year and slow anomaly, diurnal cycle, lapse`.

### Task 7: Precipitation, cloud and the weather sample

**Files:** create `crates/game/src/weather/precipitation.rs`; modify `weather/mod.rs`.

**Interfaces — produces:**

```rust
pub const SYSTEM_CELL_DEG: i64 = 12;  pub const SYSTEM_DAYS: i64 = 3;  pub const ADVECT_PERIOD_DAYS: i64 = 6;
pub const WIND_DEG_PER_DAY_PER_MS_Q16: i64 = 50_921;   // 0.777 (86.4 km/day per m/s ÷ 111.2 km/°)
pub const YEAR_WET_SHARE_Q16: i64 = 16_384;            // 0.25 (Ruling 9)
pub const WET_SHARE_MAX_Q16: i64 = 64_225;             // 0.98
pub const CLOUD_MARGIN_Q16: i64 = 19_661;              // cloud fades from 255 to 0 as u goes from π' to π' + 0.3 (untested)
/// The uniform field at a place and time (0..=65535), advected by the stored wind (Ruling 7).
pub fn wet_field_q16(wc: &WorldClimate, p: MapPoint, tq: i64) -> i32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precip { None, Rain, Sleet, Snow }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeatherSample {
    pub temp_centi_c: i32,
    pub precip_um_per_day: u32,
    pub precip: Precip,              // by temp: Snow ≤ SNOW_ALL_CENTI_C (−100) < Sleet < SNOW_NONE_CENTI_C (300) ≤ Rain
    pub cloud_255: u8,
    pub wind_q6: [i32; 2],
    pub storm: Option<crate::weather::StormKind>,   // None until Task 8
}
/// THE weather function: pure in (climate, place, height, world tick).
pub fn weather_at(wc: &WorldClimate, p: MapPoint, height_m: i32, tick: Tick) -> WeatherSample;
/// Background precipitation only (no storm), µm/day — what the expectation proof covers.
pub fn background_rate_um_per_day(wc: &WorldClimate, p: MapPoint, tq: i64) -> u32;
```

- Order inside `weather_at`: normals now → year anomaly → `u` → wet / rate / cloud → (Task 8: storms) → temperature with that cloud → `precip` kind.
- `rate = precip_um_per_month · exp_quantile_q16(v) / (28 · wet_q16)` in `i128`-free `i64` arithmetic: divide before the last multiply where needed and state the order; saturate at `u32::MAX`.

- [ ] **Step 1: Temporary tests** (`game --lib weather::precipitation::`; a climate with `π = 0.3`, 84 mm per month everywhere, zero wind; then a 6 m/s westerly): over 30 years of 3-hourly samples at 16 places the share of wet samples is `0.3 ± 0.02` and the mean rate is `3,000 µm/day ± 4 %` (the expectation: 84 mm / 28); in ONE year whose `f_year` is known (read `year_anomaly`) the mean follows `f_year` within 15 %; with `π = 0` the rate is always 0; the rate is 0 exactly where `u ≥ π'` and grows as `u → 0`; `wet_field_q16` is identical at `p` and its image, and continuous in space (1 m apart: under 256 of 65,536) and in time across the advection clocks' restarts (10⁴ consecutive ticks spanning a restart: no step above 512); with the westerly, the lag correlation of the wet indicator between a place and one 7.77° east peaks one day later (within half a day); `precip` is `Snow` at −5 °C, `Rain` at +10 °C; the sample is the same from two call orders and from a clone of the `Arc`.
- [ ] **Step 2: Implement.** — [ ] **Step 3: Verify, in order** (temporary tests; DELETE; `just smoke`; `just determinism-wasm`). — [ ] **Step 4: Commit** `feat(game): runtime precipitation — one advected uniform field for wet days and amounts; the weather sample`.

### Task 8: Storms

**Files:** create `crates/game/src/weather/storms.rs`; modify `weather/{mod,precipitation}.rs` (`weather_at` applies them).

**Interfaces — produces:** the constants of Ruling 8 (`STORM_EPOCH_DAYS`, `STORM_CELL_DEG`, `STORM_MAX_TRAVEL_DEG`, `STORM_MAX_RADIUS_DEG`, `STORM_BIRTH_PER_MILLE_AT_FULL_WET`, `STORM_RATE_UM_PER_DAY = 40_000`, `STORM_WIND_Q6 = 1_280` — all *untested*), the `const` assertion, and

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StormKind { Tropical, Gale, Blizzard }       // ≥ 2000 centi-°C / else / ≤ 0, at the place asked
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Storm { pub id: u64, pub born_tq: i64, pub ends_tq: i64, pub birth_lon_q16: i64, pub birth_lat_q16: i64,
                   pub vel_deg_per_day_q16: [i64; 2], pub radius_q16: i64, pub peak_q16: i64 }
/// The storm of (epoch, cell), if one is born there. Pure; reads the family at the birth point only.
pub fn storm_of(wc: &WorldClimate, epoch: i64, cell: (i64, i64)) -> Option<Storm>;
/// The strongest influence at a place and time: (intensity Q16, the storm), over the 27 candidates.
pub fn storm_at(wc: &WorldClimate, p: MapPoint, tq: i64) -> Option<(i64, Storm)>;
/// Every storm alive at `tq` (the summary tool, a future map layer).
pub fn storms_alive(wc: &WorldClimate, tq: i64) -> Vec<Storm>;
```

- [ ] **Step 1: Temporary tests:** **completeness** — for 2,000 random (place, time) pairs `storm_at` equals a brute-force scan of EVERY cell × the 3 epochs (the 27-candidate enumeration misses nothing); a storm's influence is 0 at its radius and at its birth and end instants (no step when one appears); identical at `p` and its image; a storm born in the easternmost cell and moving east is found from a place just east of the seam; a track clamped at 22° never leaves its 3 × 3; no storm exists poleward of ±90° and none is born where the month's wet share is 0; `weather_at` inside a storm's core is wet with cloud 255 and `storm.is_some()`.
- [ ] **Step 2: Implement.** — [ ] **Step 3: Verify, in order** (temporary tests; DELETE; `just smoke`; `just determinism-wasm`). — [ ] **Step 4: Commit** `feat(game): storms — hashed objects with closed-form tracks`.

### Task 9: Snow and ice — the bounded replay and its cache

**Files:** create `crates/game/src/overlay.rs`; modify `crates/game/src/lib.rs` (own lines).

**Interfaces — produces** (constants as in Ruling 10, each with its *recalled* / *untested* doc line):

```rust
pub const OVERLAY_BAND_M: i32 = 100;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Overlay { pub snow_swe_um: i64, pub snow_depth_mm: i32, pub ice_um: i64 }
/// The cell's reset day-of-year (Ruling 10) — the first day of the month after its warmest month.
pub fn reset_day_of_year(wc: &WorldClimate, cx: i64, cz: i64) -> u16;
/// One day's inputs at a cell centre, at sea level: (daily mean temp centi-°C, precipitation µm).
pub fn day_series(wc: &WorldClimate, cx: i64, cz: i64, day: u64) -> (i32, u32);
/// Lattice values, UNCACHED: the definition. `sea` picks the freezing point for ice.
pub fn snow_swe_um(wc: &WorldClimate, cx: i64, cz: i64, band: i32, day: u64) -> i64;
pub fn ice_um(wc: &WorldClimate, cx: i64, cz: i64, band: i32, day: u64, sea: bool) -> i64;
pub fn permanent_snow(wc: &WorldClimate, cx: i64, cz: i64, band: i32) -> bool;
/// At a place: bilinear over 4 cells, linear over 2 bands. `water`: None = dry column (no ice);
/// Some(sea) = a column whose top recipe cell is open water. Uncached.
pub fn overlay_at(wc: &WorldClimate, p: MapPoint, height_m: i32, water: Option<bool>, tick: Tick) -> Overlay;

/// A memo of the lattice. Never state: not hashed, not serialized; dropping it changes nothing.
#[derive(Default)]
pub struct OverlayCache { /* BTreeMap<(cx, cz, snow_year), CellSeries>, bounded */ }
impl OverlayCache {
    pub const MAX_CELLS: usize = 4096;                       // least-recently-asked evicted
    pub fn overlay_at(&mut self, wc: &WorldClimate, p: MapPoint, height_m: i32, water: Option<bool>, tick: Tick) -> Overlay;
    /// How many cold (cell, day) series steps the last call computed — the client's per-frame budget reads it.
    pub fn last_cost_days(&self) -> u32;
    pub fn clear(&mut self);
}
```

- The value depends on the DAY only (`tick / TICKS_PER_DAY`): the overlay steps once per calendar day, everywhere at the same tick.
- The cache is keyed by its inputs and returns exactly the uncached value; eviction can never change a result.

- [ ] **Step 1: Temporary tests** (`game --lib overlay::`; a hand-built climate with a cold winter band and a warm band): **`OverlayCache::overlay_at == overlay_at` bit for bit** at 2,000 random (place, height, day) in random order, with `MAX_CELLS` forced to 4 so eviction happens; the uncached value at day `D` equals a naive day-by-day floored recurrence written in the test; `snow_swe_um` is 0 on the reset day at a non-permanent point and the south's reset day is 168 days from the north's in a mirrored climate; a point whose mean year never melts is `permanent_snow` and returns the cap on every day; identical at `p` and its image; continuous in space (1 m apart: under 0.1 % apart) and across a band edge (99 m and 101 m: within the lapse's 0.65 °C worth); ice on a sea column starts later than on a lake column of the same cell (−1.8 against 0 °C); `ice_um` is `2_000 · isqrt(I/100)` for a hand-computed `I`; a dry column has `ice_um == 0`; one cold cell-year costs at most `336 · 5` weather calls (count through a test-only counter).
- [ ] **Step 2: Implement.** — [ ] **Step 3: Verify, in order** (temporary tests; DELETE; `just smoke`; `just determinism-wasm`). — [ ] **Step 4: Commit** `feat(game): snow and ice overlays — a bounded daily replay of the weather, memoised outside state`.

### Task 10: The region's read API, and the seam and sleep evidence

**Files:** modify `crates/game/src/halo.rs` or a new `crates/game/src/climate_read.rs` (the `impl GameData` block below).

**Interfaces — produces:**

```rust
impl GameData {
    pub fn climate(&self) -> Option<&Arc<WorldClimate>>;
    /// The weather over local block `local` at `tick`: `world_block(local)` → the frame → `weather_at`,
    /// height from `local[1]`. `None` without a climate. Reads no state but the region's coordinates.
    pub fn weather_at(&self, local: [i32; 3], tick: Tick) -> Option<WeatherSample>;
    /// Snow and ice over the block column of `local`: height and open water from the column's ground recipe
    /// (`ColumnGround::surface`, `recipe_water`, and the top recipe cell being air), UNCACHED — never per tick.
    pub fn overlay_at(&self, local: [i32; 3], tick: Tick) -> Option<Overlay>;
}
```

- No sim path calls either (Ruling 12): say so in both docs, with the reason (a client without the family would desync).

- [ ] **Step 1: The seam and sleep check — temporary tests on the game rig** (an existing `it` file of `crates/game/tests/`; the rig world gets a hand-built wrapping `WorldClimate` whose width is the rig map's and a `MapExtent` that wraps; tests build at region 64):
  - **A region seam:** two adjacent running regions A and B; for every block column of the two border chunk rows and 16 heights, `A.weather_at` (through A's local coordinates, the far columns addressed one past its edge) `==` `B.weather_at` (through B's own), and likewise `overlay_at` where both hold the column's ground (own or halo); at 5 ticks spread over two years.
  - **The world seam:** the same between the easternmost and the westernmost region of the wrapping map.
  - **A region woken after 1,000 simulated days:** region A runs throughout; region B is parked at tick `T` and woken when the rig's clock (the manager's explicit `now_ms`) stands `1,000 · DAY_MS` later; then for 256 columns `weather_at` and `overlay_at` of B at its current tick equal A's at the same tick for the shared border columns, and equal the direct `weather::weather_at` / `overlay::overlay_at` everywhere. If driving 200,000 ticks through the rig takes over 10 minutes, the fallback is 100 days through the rig plus the 1,000-day point by building B from its parked snapshot with the rig's start-tick path — a ruling in the report.
  - **No state:** `state_hash` of A and of B is unchanged by 10⁴ calls of both functions.
  Quote the test names, the command and the passing output: this IS the program's seam and sleep evidence.
- [ ] **Step 2: Implement.** — [ ] **Step 3: Verify, in order** (temporary tests; DELETE; `just smoke`; `just determinism-wasm`). — [ ] **Step 4: Commit** `feat(game): a region reads the weather, snow and ice of its columns`.

### Task 11: The climate on the wire

**Files:**
- Modify: `crates/game/src/protocol.rs` (`ServerPacket::WorldClimate`, appended LAST — the file has the owner's hunks), `crates/game/src/world_climate.rs` (`WorldClimateWire`), `crates/game/src/world_manager/mod.rs` (send after each `ServerPacket::WorldMap` send to a home session — two sites today: the connect path and `set_world_map`'s late arrival *(verify)*; the packed frame cached in the manager), `crates/game/src/lib.rs` (`ClientUpdateEvent::WorldClimate` if the enum lives there *(verify)*)
- Modify: `crates/client/src/instance.rs` (the packet arm, beside `ServerPacket::WorldMap`'s; the `Arc` kept beside `route_map`; passed to `host.spawn`), `crates/client/src/regions/{runner,host}.rs` (`RegionCmd::SetClimate`, `build_region` sets it on the region and on the render copy)

**Interfaces — produces:**

```rust
/// The climate as one packet: the frame and the six planes row-delta encoded.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorldClimateWire { pub frame: ClimateFrame, pub monthly: MonthlyClimate /* delta-encoded */ }
impl WorldClimate {
    pub fn to_wire(&self) -> WorldClimateWire;
    /// Validates the grid BEFORE decoding, checks `frame.monthly_crc` after: untrusted input never panics.
    pub fn from_wire(w: WorldClimateWire) -> Result<WorldClimate, String>;
}
ServerPacket::WorldClimate(Arc<[u8]>)   // the bincode of WorldClimateWire, encoded once per process; or the struct itself —
                                        // choose so the manager never re-serializes 28 MiB per session; say which in the report
```

- A client that receives it: decode → `Arc` → `host.broadcast(RegionCmd::SetClimate)` → `ClientUpdateEvent::WorldClimate(arc)`. A decode error is logged once and the session goes on without a climate. A second packet of the same `monthly_crc` is ignored.
- Sent to home sessions only; never on an observer connection; the server with no climate sends nothing.

- [ ] **Step 1: Temporary tests:** (`game --lib`) `from_wire(to_wire(c)) == c`; a wire whose plane is truncated, whose `nx` is doubled or whose crc is wrong is `Err`, no panic; `decode_packet(encode_packet(ServerPacket::WorldClimate(..)))` round-trips; (`client --lib`, an existing headless harness — tiny world, `set_lod_table(&[])`, near radius 0; **ask before running client tests: say so first**) after `WorldMap` + `WorldClimate` + a `Region` snapshot the predicted region has the climate and its `weather_at` equals the server region's at the same tick.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Evidence — bytes on the wire** (a temporary test or the example of Task 15 with `--wire`): for the dev world at 32 m the length of `encode_packet(ServerPacket::WorldClimate(..))` against `climate_bytes_packed` of `just forge-report --size dev --cell 32`; quote both and the encode and decode milliseconds. Every size is measured in Task 18.
- [ ] **Step 4: Verify, in order** — temporary tests pass; DELETE; `just smoke`; `just determinism-wasm`; `just web-check`.
- [ ] **Step 5: Commit** `feat(game,client)!: the world's climate is sent once per session; predicted regions hold it` (the message says: client and server must be one build).

### Task 12: The client's sun and sky follow the date and the latitude

**Files:** create `crates/client/src/renderer/seasons.rs`; modify `renderer/mod.rs` (plugin wiring), `renderer/bridge.rs` (`ClientUpdateEvent::WorldClimate` → the resource; the home region's world tick → the resource — *verify at dispatch how the bridge learns the current tick; `block_damage` projects with `region.current_tick()` in `regions/runner.rs`: if no event carries the tick, add `ClientUpdateEvent::WorldTick(Tick)` sent once per home tick*), `renderer/atmosphere.rs` (`spawn_lights` keeps `SUN_DIRECTION` as the no-climate default; a new system `follow_sun`), `renderer/skybox.rs` (a brightness scale).

**Interfaces — produces:**

```rust
/// Per-world: the climate, the latest world tick, the camera's place. Registered for session reset.
#[derive(Resource, Default)]
pub struct SeasonState { pub climate: Option<Arc<game::WorldClimate>>, pub tick: Option<game::Tick>,
                         pub cache: game::OverlayCache, pub here: Option<HereNow> }
pub struct HereNow { pub date: game::SimDate, pub season: Option<game::Season>, pub sample: game::WeatherSample,
                     pub overlay: game::Overlay, pub sun_dir: Vec3, pub daylight: f32 }
pub const NIGHT_AMBIENT_SHARE: f32 = 0.08;   // the night never goes black (untested, owner's eyes)
pub const TWILIGHT_SIN: f32 = 0.10;          // sun fades between elevation sin −0.10 and +0.10
```

- `follow_sun` (every frame, cheap): the camera's world position → canonical world block (through the client's wrap frame, *verify: `WrapFrame`*) → `MapPoint`; `game::sun_direction(lat, yq, sf)`; the directional light looks along `−sun_dir`; illuminance `SUN_ILLUMINANCE · smoothstep(−TWILIGHT_SIN, TWILIGHT_SIN, sin_elevation)`; ambient and skybox brightness scaled between `NIGHT_AMBIENT_SHARE` and 1 by the same factor. With no climate or no tick: exactly today's constants (the menu, the asset viewer, a procedural world).
- The time fed to the sun is the world tick (20 steps a second); between ticks the direction is interpolated on the render side with the frame's tick fraction if the bridge has one, else stepped. **Nothing here assumes a day length:** at `DAY_MS = 10_000` the sun circles in 10 s — the owner's to change; the system must not divide by, or wait on, a number of frames per day.
- Shadows: the cascades follow the light as they do today; below the horizon the light's illuminance is 0 and shadows are disabled (`shadows_enabled`), so a night costs no shadow pass.

- [ ] **Step 1: Temporary tests** (`client --lib seasons::` — pure helpers only, no app: a `fn sun_light(climate, tick, block) -> (Vec3, f32)` extracted for the test): noon at the equator on an equinox → straight down, full strength; midnight → strength 0; 80° N at the northern summer solstice → strength > 0 at every hour; at the winter one → 0 at every hour; with `climate = None` → `SUN_DIRECTION` and `SUN_ILLUMINANCE` exactly. **Ask before running client tests.**
- [ ] **Step 2: Implement**, registering `SeasonState` with `add_session_reset` (or `SessionLocal` if that is the pattern for bridge-fed resources — *verify in `session.rs`*).
- [ ] **Step 3: Verify, in order** — temporary tests pass; DELETE; `just smoke`; `just web-check`.
- [ ] **Step 4: Commit** `feat(client): the sun and the sky follow the date, the latitude and the local hour`.

### Task 13: The HUD line and the dev pane

**Files:** modify `crates/slint-ui/ui/hud.slint` (one text line; never bind `visible` / `opacity` on a component's root), the generated-crate glue it needs *(verify which of `slint-ui-game` / `-frontend` owns `hud.slint`)*, `crates/client/src/renderer/hud.rs` (feeds it from `SeasonState::here`, only when the text changes), `crates/client/src/renderer/dev_ui.rs` (F3 pane: sim date with day fraction, lat / lon, temperature, rate, cloud, wind, storm, snow depth, ice, `daylight`, and — beside it, labelled "history calendar" — the coordinator's `CalendarTick` it already shows), `crates/client/src/renderer/seasons.rs` (a system that fills `here` at most 4 times a second).

**Interfaces:** the line reads `{date} · {season} · {temp} °C · {sky}` — e.g. `12 Granite · Spring · 7 °C · Light rain`; `{sky}` from the sample: `Clear` (cloud < 64), `Cloudy`, `Overcast` (≥ 192), `Light rain` / `Rain` / `Heavy rain` (rate under 5 / under 25 / from 25 mm per day), `Sleet`, `Snow` / `Heavy snow`, and the storm's name when in one (`Storm`, `Tropical storm`, `Blizzard`). No season → the part is omitted. No climate → the line is empty and takes no room. Temperature is rounded to whole degrees for the player; the dev pane shows centi-degrees. **No year, and never the coordinator's calendar** (Ruling 2).

- [ ] **Step 1: Temporary tests** (`client --lib` — the pure formatter `fn hud_line(&HereNow) -> String`): the example above; a seasonless place; a blizzard; `None`. **Ask before running client tests.**
- [ ] **Step 2: Implement.** — [ ] **Step 3: Verify, in order** (temporary tests; DELETE; `just smoke`; `just web-check`). — [ ] **Step 4: Commit** `feat(client): the date, the local season and the weather on the HUD and in the dev pane`.

### Task 14: Snow and ice on the near ground and water

**The one task whose rendering path was NOT read for this plan** (`renderer/voxel_pull/`, `assets/shaders/voxel_pull.wgsl`, `renderer/water/`): its implementer starts with a code check and writes the binding plan into the report before editing. It is separable: if the check finds no way to add one texture binding to the pulled terrain pass on all three backends (`per-chunk | direct | gpu`) without touching the 8-byte quad record layout, the task ends there as a recorded ruling and "snow and ice are not drawn" moves to Known limits — Tasks 15–18 do not depend on it.

**Files (expected):** `crates/client/src/renderer/seasons.rs` (the overlay texture and its fill), `renderer/voxel_pull/` (one more binding: the texture, its world-space origin and texel size), `assets/shaders/voxel_pull.wgsl` (whiten faces whose normal points up by the snow value; compiled in — rebuild), `renderer/water/` and its shader (the ice value over open water), `renderer/config.rs` if a switch is added (`VOXEL_SEASON_OVERLAY=on|off`, default on).

**Design:** one `R8G8` texture over the near view, one texel per `OVERLAY_TEXEL_M = 4` m, centred on the camera's chunk and re-centred when it changes; R = snow cover 0–255 (`min(depth_mm, SNOW_FULL_DEPTH_MM = 50) · 255 / 50`), G = ice 0–255 (`min(ice_um, ICE_FULL_UM = 100_000) · 255 / …`). A texel's value is `SeasonState::cache.overlay_at` at the texel's column — height and open water from the client's tile mirror of the ground recipes (`GameData::ground_recipes`, what the commander's `height.rs` reads). Refilled when the calendar day or the centre changes, spread over frames: at most `OVERLAY_TEXELS_PER_FRAME` texels and `OVERLAY_COLD_DAYS_PER_FRAME` cold replay days per frame (`OverlayCache::last_cost_days`), the old texture shown until the new one is whole. No mesh is ever rebuilt for snow. No worker is used (Ruling 1).

- [ ] **Step 0: Code check and binding plan** (in the report). — [ ] **Step 1: Temporary tests** (pure: texel ↔ world column mapping across the wrap frame; the fill visits every texel exactly once over N frames; a `None` climate produces an all-zero texture and the shader path costs one sample). — [ ] **Step 2: Implement.** — [ ] **Step 3: Verify, in order** (temporary tests — ask first; DELETE; `just smoke`; `just web-check`). The shader on a real adapter is the owner's (`just pull-smoke`, `just run`): listed under hand checks. — [ ] **Step 4: Commit** `feat(client): seasonal snow and ice tint the near ground and water`.

### Task 15: The year's summary for five places

**Files:** create `crates/worldgen/examples/weather_report.rs` (a new file; it links `worldgen` to LOAD a world and `game` for everything else); modify `justfile` (`weather-report *args`: `{{cap}} {{cargo}} run --release -p worldgen --example weather_report -- {{args}}`, with a comment listing its lines).

**Interfaces:** `just weather-report --world <file.world> [--years 30] [--year 0] [--place name:lat,lon]… [--wire]`.

- **The five places, chosen by rule on a 2° lattice of the world** (land = `column_climate(x, z).biome != Biome::Ocean`; every choice printed with its lat, lon and ground height): **pole** — 85° N on the middle meridian; **interior** — the land point between 35° and 55° (either hemisphere) farthest, in lattice steps with x wrapping, from any ocean point; **west coast** — a land point between 30° and 45° whose neighbour two steps west is ocean and two steps east is land (the first in scan order from the north-west); **desert** — the land point between 10° and 40° with the least annual precipitation; **equator** — the land point nearest 0° (the sea point on the middle meridian if none). A rule that finds nothing prints `place.<name>: none` and the run goes on.
- **Per place, per month, for `--year` (default 0) and as the mean of `--years`:** mean / min / max temperature (four-hourly samples at the ground) against the normal at the ground (`normals_year` − lapse); wet days (days whose total is above 0.1 mm) against `π · 28`; the month's precipitation against the normal, and the storms' part of it; snow-cover days (depth ≥ 10 mm at the place's height); ice days (ice ≥ 10 mm on a notional lake at that height); `daylight` on the 14th; the local season name.
- **Per place, summary lines:** `annual_precip_mm` (year, N-year mean, normal, ratio), `storm_share`, `temp_mean_err_c.max` (the worst month of the N-year mean against the normal), `wet_days_err.max`, `snow_cover_days`, `snow_at_reset_mm` (max over the years), `ice_days`, `dry_month_with_rain` (months with `π = 0` and `P > 0`), `warmest` / `coldest` month names.
- **Acceptance ranges** (on the N = 30 means, at every place, on the placeholder world today and again after Phase 3): `temp_mean_err_c.max ≤ 0.5`; wet days within `max(1.0 day, 10 %)` of `π · 28` in every month; `annual_precip_mm` ratio 0.95–1.08 (the storms' up to 5 % on top); `storm_share ≤ 0.05`; in a SINGLE year the ratio is 0.6–1.5; at the pole `snow_cover_days` ≥ 300 or permanent; at the equator at sea level 0; the north's coldest month is 8, 9 or 10 and the mirrored southern place's is 2, 3 or 4. A miss is tuned in the constants of Tasks 6–9 inside this task, or recorded as a ruling with the number.
- `--wire`: prints, for the loaded world, `wire_bytes` (the encoded `ServerPacket::WorldClimate`), `encode_ms`, `decode_ms`, `resident_bytes`.

- [ ] **Step 1: Implement** (no temporary test: the evidence run is its compile check — `just smoke` does not build `worldgen`'s examples).
- [ ] **Step 2: Evidence** — `just forge-proto --size dev --cell 32 --out target/world-forge/phase6` then `just weather-report --world target/world-forge/phase6.world --years 30 --wire`: quote all of it; judge every acceptance line; tune or rule. Then the same on a Medium world (`--size medium --cell 32`, about 40 s to forge): quote the summary lines.
- [ ] **Step 3: Verify** — `just smoke`. — [ ] **Step 4: Commit** `feat(worldgen): weather-report — a year's weather, snow and ice at five places of a world`.

### Task 16: Native against wasm32, on a lattice

No file is committed by this task; it is evidence (Ruling 17).

- [ ] **Step 1:** add, TEMPORARILY, to `crates/game/tests/wasm_determinism.rs` one function `weather_crc() -> u32` and its two callers in the file's own pattern (a native `#[test]`, a `#[wasm_bindgen_test]`), each logging `WEATHER_CRC = 0x…` through the file's `log`: a synthetic `WorldClimate` (a 24 × 12 wrapping grid, planes filled from `roll`, a non-zero origin, a 6.5 °C/km lapse), and a crc32 over the little-endian bytes of: `weather_at` at 4,096 (tick, point, height) triples spread over 40 years, both sides of the seam and both polar rows; `overlay::overlay_at` at 512 of them (dry, lake and sea); `storm_at`; `sim_date`; and the `to_bits()` of `declination_rad`, `sun_elevation_sin`, `daylight_share`, `sun_direction` at 512 (lat, date, hour) triples.
- [ ] **Step 2:** run the recipe's two `game` commands by hand (native: `{{cargo}} test -p game --test wasm_determinism -- weather --nocapture`; wasm32: the same with `--target wasm32-unknown-unknown` and the runner variable, as `determinism-wasm` sets them). Quote both `WEATHER_CRC` lines: **they must be equal.** A difference is a determinism bug in this plan's code: fix it in the task that owns the function, re-run.
- [ ] **Step 3:** DELETE the temporary code (`git diff crates/game/tests/wasm_determinism.rs` shows only the owner's hunks, if any); `just smoke`; `just determinism-wasm` (unchanged values).

### Task 17 (HELD — do not dispatch): player clears as sparse deltas with an expiry

Dispatch only when the owner has approved re-recording `wasm_determinism`'s goldens and `scene_hash_golden`, and a wipe of every saved region (Ruling 13 — the design is there, in full). When approved, the task is: `GameData::overlay_clears` (`#[undo(map)]`, hashed, serialized) and its bound and eviction; the stamp written by the existing terrain-edit events *(verify the event names in `edit.rs` / `mining.rs` at dispatch)*; `overlay::overlay_at_since` (the later lower bound) and `GameData::overlay_at` using it; the client's texture reading the stamps of its predicted regions; the rollback bar checked by a temporary test (`hash(before) == hash(after undo)` over insert, evict and re-insert); `carry_components` / `Client::arrive` unaffected (a column does not travel) — state why in the code; the gates re-recorded in ONE commit of their own that states the old and new values; `CLAUDE.md` and `TODO.md` (worlds wiped). Until then the phase's row carries "player clears: HELD, needs the owner".

### Task 18: Close — after Phase 3 is DONE

**Files:** `docs/superpowers/plans/2026-11-19-earth-like-world-program.md` (Status row 6; Evidence: "Phase 6"; Owner hand checks), `docs/superpowers/specs/2026-11-14-earth-like-world-overhaul-design.md` (Measurements: "Phase 6 (runtime)"; Decision 8 amended: what is derived, what is held; Decision 6: the two clocks), this file ("As built"), `CLAUDE.md` (a "Seasons and weather" paragraph under Architecture rules: the date is the world tick's; `WorldClimate` is configuration; weather, snow and ice are pure and never read by a sim tick; the packet; the two calendars), `TODO.md` (the held task; the phase-lock question; the Known limits worth a line).

- [ ] **Step 1: Re-run on a Phase 3 world** — `just weather-report` on a dev and a Medium world forged by the Phase 3 build (30 years): every acceptance line judged again; `snow_cover_days` against Phase 3's `model.snow_months` at the four non-desert places (within one month); `storm_share`. Tune constants or rule.
- [ ] **Step 2: Measurements (runtime, not forge)** — `weather_at` ns per call and a cold overlay cell's ms, native release, by a `--bench` mode of `weather_report` (10⁶ calls at random places; say the machine, `scaling_cur_freq` before and after — a timing loop in a tool, not a benchmark run of the game); per world size (`--wire` on a world of each size at 32 m): wire bytes, encode / decode ms, resident bytes per server process (two copies + the packet) and per client. No pass line; a figure over twice Ruling 16's budget is a finding.
- [ ] **Step 3: The evidence, pasted into the program:** `just determinism-wasm` (the PASS lines, or the same pre-existing digest difference before and after, as each task recorded); Task 10's seam and sleep outputs; Task 16's two crc lines; Task 5's calibration; Task 15's five-place summary (dev world, year 0 printed whole, the 30-year summary lines for both sizes).
- [ ] **Step 4: Whole suites, once:** `just test` in the foreground (say so first), then `just clippy` for the crates touched (`--cap-lints warn` as Phase 1b did if the known failures elsewhere stop the workspace run); quote the PASS / FAIL line per stage; a failure in this plan's files is fixed here, one outside is recorded.
- [ ] **Step 5: Docs** as listed; "As built" (commits per task, rulings made by tasks, deferred minors, the held task).
- [ ] **Step 6: Commit** `docs: Phase 6 evidence, measurements and as-built`.

## Known limits (not in this plan)

- **Player clears are HELD** (Task 17): digging does not clear snow, nothing is stored, until the owner approves the gate re-records.
- **Nothing in the sim reads weather.** No cold, heat, wetness, slipping, crop growth, soil moisture, fire weather, visibility for creatures; snow is not deep, ice is not walkable and has no collider; rivers and lakes keep their genesis level; no tides. Each later feature that reads weather in a tick needs the family on the client before its first predicted tick (Task 11's ordering is there for it) and a decision on what a client does while it has none.
- **Two calendars** (Ruling 2): the HUD's date follows the world tick, the history's follows the coordinator; they agree at creation and part at restarts — by up to a save interval per crash in role `all`, by the whole of an outage on a split world. The cure (a phase lock in the coordinator) is the owner's.
- **Perennial snow and ice are still the baked genesis blocks** of the annual rule; the seasonal overlays draw over them and never remove them. The two can disagree at the edge of the permanent zone. Phase 4's glacier thickness is terrain.
- **The reset snap:** a non-permanent point that still has snow on its reset day loses it in one day (`snow_at_reset_mm` says how much).
- **Weather is at climate-cell resolution** (64–256 m cells, bilinear): the fine rain-shadow modulator is not applied at runtime, so a lee slope gets its cell's rain; only the lapse is per column.
- **Not drawn:** rain and snow particles, clouds, weather fog, lightning, wind on trees, snow on the far LOD rings, snow depth as geometry, foliage colour and leaf fall, a forecast, a season layer on the world map; if Task 14 is ruled out, no snow or ice at all.
- **Years differ by one field only:** no multi-year oscillation, no rare events with return periods, no volcanic winters, no weather that depends on an entity.
- **Storms** travel in straight lines at their birth wind, never cross a pole, and add up to about 5 % to a place's rain.
- **A client downloads the family on every join** (no disk or IndexedDB cache); each server process holds it twice.
- **At `DAY_MS = 10_000` the sun circles in 10 s** and the diurnal cycle is noise; nothing is damped for it — the day length is the owner's.
- **Every constant marked untested or recalled** (snow share ramp, melt factor, Stefan coefficient, symmetric ice melt, diurnal shape and cloud damping, anomaly amplitudes, storm rates, season-name thresholds) is a starting value judged only by the five-place summary.
- Under a closed seam (every role but `all`) the region one past the east or west edge is a copy of the far edge (Phase 1a's limit) and so is its weather.

## Owner hand checks (none blocks the run)

1. **Decide the day length** (the program's standing item) — and with it, whether local solar time should follow longitude (`SOLAR_TIME_BY_LONGITUDE`, Ruling 5): at 10 s the sky strobes and opposite sides of a Medium world are 5 s apart.
2. **Decide the two calendars** (Ruling 2): keep them apart (as built), or have the coordinator re-anchor forward by less than a year so both share one day of the year (a coordinator change, not planned here).
3. **Approve or decline the held task** (Ruling 13): player clears need `wasm_determinism`'s goldens and `scene_hash_golden` re-recorded and saved regions wiped. (The scene digest in this checkout already differs from its pin because of your uncommitted `crates/game` work — a re-record is due on your side anyway.)
4. `just run --reset`, then stand still through a year (56 minutes at today's day; or set `DAY_MS` for the session): the sun's arc rising and falling with the season; the HUD line's date, season, temperature and sky changing; rain and snow named when they fall; snow whitening the ground in winter and going in spring; ice on a lake.
5. Walk to a high latitude in its winter and its summer: polar night and polar day; walk south across the equator: the season name flips.
6. Walk east across a region seam and the world seam while it snows: no line in the snow tint, no jump in the HUD's numbers.
7. `just pull-smoke` and `just run` on a real adapter after Task 14 (the terrain and water shaders gained a binding; shader tests compose against stubs); `just web` in a browser: the climate arrives (the dev pane shows numbers) and the join is not visibly slower on a Medium world.
8. Memory: each server process now holds the family twice (+28 MiB at Large) — beside Phase 1b's item on the pods' requests.

## Symbols named here that were NOT read first-hand (verify at dispatch)

`ClientUpdateEvent` (where the enum lives; that it is an in-process channel in the browser); how the render side learns the current world tick; `WrapFrame`'s API; `session.rs`'s exact registration call for a bridge-fed resource; `renderer/voxel_pull/`, `voxel_pull.wgsl`, `renderer/water/` (Task 14 — not read at all); `hud.slint`'s owner crate; the terrain-edit event names (Task 17); `BootstrapParams::years` being whole years; the manager rig's file and filter names in `crates/game/tests/`; `wasm_determinism.rs`'s `print_hash` switch; whether `slime::roll` is `pub`; the second send site of `ServerPacket::WorldMap`; how `map_extent` is spelled inside the `#[rollback]` struct (the `raw_mut` wrapper) — Task 4 copies it exactly.

## As built

(Filled by Task 18.)
