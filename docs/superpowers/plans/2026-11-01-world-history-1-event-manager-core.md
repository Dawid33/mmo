# World History 1 — Event Manager Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A deterministic, threadless world event manager (`game::history`) that holds the world's
people, sites, civs and journeys on one calendar, runs the first event chain (marriage, birth, death,
migration, embark/founding, abandonment) abstractly, bootstraps and fast-forwards a world's history
from its `WorldMap`, saves/loads it, and reports statistics — with no region integration yet.

**Architecture:** A new `crates/game/src/history/` module: plain-data records in ordered containers, a
discrete-event queue keyed by calendar tick (never per-tick polling), stateless hashed randomness, and
integer rules whose tunables are named constants. Inputs come only from the `WorldMap` tile summaries
(through a derived `SocietyGrid`). Plan 2 wires it to regions; Plan 3 to the client.

**Tech Stack:** Rust (dated nightly per `rust-toolchain.toml`), `game` crate (serde + bincode + lz4_flex,
already dependencies), `worldgen` example for the tool.

**Spec:** `docs/superpowers/specs/2026-11-01-world-history-events-design.md` (read "Intent", "The
design" §1, §2, §3, §5, §7, §8, §10). This is plan 1 of 3:
1. this plan — the core, all-abstract;
2. `2026-11-01-world-history-2-region-handover.md` — regions, person bodies, catch-up, persistence in
   the server;
3. `2026-11-01-world-history-3-client-embark.md` — protocol, embark/retire/return, client views.

## Global Constraints

- Work on `develop` in the main checkout. **No git worktrees.** `git add` only the files a task
  changed; `git status` before each commit (Dawid has unrelated uncommitted work).
- `game` stays Bevy-free and windowing-free.
- **Deterministic:** same inputs ⇒ same history. Only `BTreeMap`/`BTreeSet`/`Vec` containers (never
  iterate a `HashMap`), randomness only via `history::rng::roll(at, subject, salt)` (stateless), rules
  in **integers** (per-mille, days, ticks) — no float math in rule decisions except converting
  `TileSummary` fields once into the integer `SocietyGrid`.
- The calendar unit is the region tick: one `CalendarTick` = `game::TICK_RATE` ms (50 ms).
- Every tunable is a named `pub const` in `history/rules.rs` (user preference: tunables as named
  constants), shared by every resolver.
- Dwarves are the only race (`Race::Dwarf`).
- Tests: integration tests go in ONE new module file `crates/game/tests/history.rs`, registered in
  `crates/game/tests/it.rs` (`mod history;`). Run with `just test-one game "--test it history::"`.
  Tag any test slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- Verification default: `just smoke` (60 s cap) at the end of each task in addition to the task's own
  `just test-one`. Never run whole crate suites.
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **A civ whose every site dies out mid-history** — sites are abandoned, the civ has no living members,
   nothing panics, no step stays scheduled for a dead person or abandoned site (Task 5 test
   `extinct_civ_leaves_no_live_steps`).
2. **Embark with no suitable target** (all land within reach taken or under the minimum distance, or
   the site is on an island) — no party leaves; the site just stays crowded (Task 5 test
   `crowded_site_with_no_target_stays`).
3. **Population cap** — a civ never exceeds `CIV_POPULATION_CAP` living people, even under long
   fast-forward (Task 6 test `fast_forward_respects_civ_cap`).
4. **`advance` called with a time earlier than `now`, or twice with the same time** — no-op, no
   double-processing (Task 3 test `advance_is_idempotent_and_monotonic`).
5. **Loading a history file from a different version or a different world** — refused with a clear
   error, never a panic or a silent mix (Task 7 tests `load_refuses_other_version`,
   `load_refuses_other_world`).

---

## File Structure

- Create `crates/game/src/history/mod.rs` — `History` (the manager core), re-exports.
- Create `crates/game/src/history/calendar.rs` — `CalendarTick`, `Date`, calendar constants.
- Create `crates/game/src/history/records.rs` — ids, `Person`, `Site`, `Civ`, `Journey`, `Location`,
  enums.
- Create `crates/game/src/history/rng.rs` — `roll`, salts.
- Create `crates/game/src/history/rules.rs` — tunables + pure rule functions.
- Create `crates/game/src/history/grid.rs` — `SocietyGrid` from `WorldMap`; world-map A* routing.
- Create `crates/game/src/history/log.rs` — `LogEntry`, `LogKind`, text rendering.
- Create `crates/game/src/history/chain.rs` — the event chain's step handlers (site month, death,
  journey legs, migration/embark/founding).
- Create `crates/game/src/history/bootstrap.rs` — civ placement, founders, `fast_forward`, rejection.
- Create `crates/game/src/history/persist.rs` — file format, save/load (native only).
- Create `crates/game/src/history/stats.rs` — `HistoryStats`.
- Modify `crates/game/src/lib.rs` — `pub mod history;` (no glob re-export; callers use `game::history::…`).
- Create `crates/game/tests/history.rs`; modify `crates/game/tests/it.rs`.
- Create `crates/worldgen/examples/history.rs`; modify `justfile` (recipe `history`); modify `CLAUDE.md`.

---

### Task 1: Calendar, ids and records

**Files:**
- Create: `crates/game/src/history/{mod.rs,calendar.rs,records.rs}`
- Modify: `crates/game/src/lib.rs`
- Test: `crates/game/tests/history.rs`, `crates/game/tests/it.rs`

**Interfaces:**
- Produces (`game::history`):
  - `pub struct CalendarTick(pub u64)` — `Copy, Ord, Hash, Debug, Serialize, Deserialize, Default`;
    `const fn from_days(d: u64) -> Self`, `fn plus_days(self, d: u64) -> Self`,
    `fn plus_ticks(self, t: u64) -> Self`, `fn date(self) -> Date`, `fn years_since(self, earlier: CalendarTick) -> u32`.
  - `pub const DAY_MS: u64 = 10_000;` `pub const TICKS_PER_DAY: u64 = DAY_MS / crate::TICK_RATE;`
    `pub const DAYS_PER_MONTH: u64 = 28;` `pub const MONTHS_PER_YEAR: u64 = 12;`
    `pub const DAYS_PER_YEAR: u64 = DAYS_PER_MONTH * MONTHS_PER_YEAR;` (a year ≈ 56 min of play, near
    DF's ~1 h).
  - `pub struct Date { pub year: u32, pub month: u8 /*1..=12*/, pub day: u8 /*1..=28*/ }` with
    `Display` as `"{day} {MONTH_NAME} {year}"`, month names DF's: Granite, Slate, Felsite, Hematite,
    Malachite, Galena, Limestone, Sandstone, Timber, Moonstone, Opal, Obsidian.
  - `pub struct PersonId(pub u32)`, `SiteId(pub u32)`, `CivId(pub u32)`, `JourneyId(pub u32)` — all
    `Copy, Ord, Eq, Hash, Debug, Serialize, Deserialize, StateHash` (PersonId will ride in sim state
    in plan 2).
  - `pub enum Race { Dwarf }`, `pub enum Sex { Female, Male }`,
    `pub enum DeathCause { OldAge }` (more later).
  - `pub enum Location { AtSite(SiteId), Journey(JourneyId), InRegion(crate::RegionCoords) }`
    (`InRegion` unused until plan 2).
  - `pub struct Person { pub id, pub race, pub sex, pub name: String, pub born: CalendarTick,
    pub died: Option<(CalendarTick, DeathCause)>, pub mother: Option<PersonId>, pub father: Option<PersonId>,
    pub spouse: Option<PersonId>, pub children: Vec<PersonId>, pub site: Option<SiteId> /*citizenship*/,
    pub location: Location, pub last_birth: Option<CalendarTick> }` with `fn alive(&self) -> bool`,
    `fn age_years(&self, now: CalendarTick) -> u32`.
  - `pub struct Site { pub id, pub civ: CivId, pub name: String, pub region: crate::RegionCoords,
    pub founded: CalendarTick, pub founders: Vec<PersonId>, pub from: Option<SiteId>,
    pub abandoned: Option<CalendarTick>, pub capacity: u32, pub members: BTreeSet<PersonId>,
    pub player_founded: bool }`.
  - `pub struct Civ { pub id, pub race, pub name: String, pub sites: Vec<SiteId> }`.
  - `pub struct Journey { pub id, pub people: Vec<PersonId>, pub purpose: Purpose, pub route: Vec<crate::RegionCoords>,
    pub departed: CalendarTick, pub leg_ticks: u64, pub leg: u32 /*index into route of the current region*/ }`
    and `pub enum Purpose { Migrate { to: SiteId }, Embark { civ: CivId, from: SiteId } }`.
  - Names (placeholder per spec non-goals): `fn person_name(id: PersonId) -> String` → `"Dwarf {id}"`,
    `fn site_name(id: SiteId) -> String` → `"Site {id}"`, `fn civ_name(id: CivId) -> String` → `"Civ {id}"`.

- [ ] **Step 1: Write the failing tests** in `crates/game/tests/history.rs` (add `mod history;` to `it.rs`):

```rust
use game::history::*;

#[test]
fn calendar_dates() {
    assert_eq!(CalendarTick(0).date(), Date { year: 0, month: 1, day: 1 });
    let one_year = CalendarTick::from_days(DAYS_PER_YEAR);
    assert_eq!(one_year.date(), Date { year: 1, month: 1, day: 1 });
    assert_eq!(CalendarTick::from_days(DAYS_PER_MONTH + 2).date(), Date { year: 0, month: 2, day: 3 });
    assert_eq!(one_year.years_since(CalendarTick(0)), 1);
    assert_eq!(TICKS_PER_DAY, 200);
    assert_eq!(CalendarTick::from_days(2).date().to_string(), "3 Granite 0");
}

#[test]
fn person_age_and_alive() {
    let p = Person::newborn(PersonId(1), Sex::Female, CalendarTick(0), None, None, None, Location::AtSite(SiteId(0)));
    assert!(p.alive());
    assert_eq!(p.age_years(CalendarTick::from_days(DAYS_PER_YEAR * 30 + 5)), 30);
}
```

  (`Person::newborn(id, sex, born, mother, father, site, location) -> Person` is part of this task:
  name from `person_name`, everything else empty/None.)

- [ ] **Step 2:** `just test-one game "--test it history::"` — expect a compile failure (`history` missing).
- [ ] **Step 3:** Implement the types above; `pub mod history;` in `lib.rs`.
- [ ] **Step 4:** `just test-one game "--test it history::"` — PASS; `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(history): calendar and world records`.

---

### Task 2: Randomness, tunables, SocietyGrid and routing

**Files:**
- Create: `crates/game/src/history/{rng.rs,rules.rs,grid.rs}`
- Test: `crates/game/tests/history.rs`

**Interfaces:**
- Produces:
  - `rng::roll(at: CalendarTick, subject: u64, salt: u64) -> u64` — same mixing as `slime::roll` (copy
    `mix`; do not depend on slime). `rng::chance_per_mille(at, subject, salt, per_mille: u32) -> bool`.
    Salts as `pub const SALT_*: u64` (MARRY, BIRTH, SEX, DEATH_AGE, MIGRATE, EMBARK, PLACE).
  - `rules.rs` tunables (defaults; the builder may retune in Task 6 by statistics and must record the
    final values in the commit message):
    `ADULT_AGE_YEARS = 12`, `MARRY_PER_MILLE_MONTHLY = 40`, `FERTILE_MAX_AGE_YEARS = 80`,
    `BIRTH_PER_MILLE_MONTHLY = 25`, `BIRTH_MIN_GAP_DAYS = DAYS_PER_YEAR`,
    `OLD_AGE_MIN_YEARS = 80`, `OLD_AGE_MAX_YEARS = 160`,
    `CROWDED_PER_MILLE = 900` (members/capacity at which a site is crowded),
    `MIGRATE_PER_MILLE_MONTHLY = 150` (per crowded site per month),
    `EMBARK_PER_MILLE_MONTHLY = 60` (crowded, no civ site with room),
    `EMBARK_BASE_PER_MILLE_MONTHLY = 2` (crowded but a civ site has room),
    `EMBARK_PARTY_MIN = 7`, `EMBARK_PARTY_MAX = 20`,
    `EMBARK_MIN_DISTANCE_REGIONS = 4` (Chebyshev; for AI and player embarks),
    `EMBARK_MAX_SEARCH_REGIONS = 16`, `CIV_POPULATION_CAP = 2000`,
    `CAPACITY_BASE = 40`, `CAPACITY_PER_RIVER = 10`, `CAPACITY_LAKE = 15`, `CAPACITY_MAX = 200`,
    `CAPACITY_RELIEF_PENALTY_PER_100M = 10`, `MIN_DRY_FRACTION_PER_MILLE = 300`.
  - `rules::death_age_days(person: PersonId, born: CalendarTick) -> u64` — uniform in
    `[OLD_AGE_MIN_YEARS, OLD_AGE_MAX_YEARS]` years via `roll(born, person.0, SALT_DEATH_AGE)`.
  - `rules::birth_per_mille(members: u32, capacity: u32) -> u32` — `BIRTH_PER_MILLE_MONTHLY` scaled by
    `(1000 - crowding_per_mille).max(0)` / 1000; 0 at or above capacity.
  - `rules::crowding_per_mille(members: u32, capacity: u32) -> u32` (capacity 0 ⇒ 1000+).
  - `rules::walk_ticks_per_region() -> u64` — the abstract travel time across one region, **derived
    from the physical walk speed** so abstract and played travel agree:
    `REGION_SIZE` units / (`movement::WALK_SPEED` units per second) → ticks. (Read the walk-speed
    constant's actual name and unit in `crates/game/src/movement.rs`; document the derivation.)
  - `grid::SocietyGrid` — built once from a `WorldMap`:
    `pub fn from_world_map(map: &crate::WorldMap) -> SocietyGrid`;
    `fn contains(&self, rc: RegionCoords) -> bool`; `fn capacity(&self, rc) -> u32` (0 = cannot host
    a site: no `landing`, or dry fraction (`1 - sea_fraction - lake_fraction`) below
    `MIN_DRY_FRACTION_PER_MILLE`); `fn passable(&self, rc) -> bool` (dry fraction > 0 and a landing);
    `fn step_cost(&self, rc) -> u32` (1000 + relief penalty); `fn regions(&self) -> impl Iterator<Item = RegionCoords>` in row-major order;
    `fn world_id(&self) -> u32` (crc32 of the bincode of the `WorldMap` — used by persistence to refuse
    a history from another world).
    Capacity = `CAPACITY_BASE + rivers*CAPACITY_PER_RIVER + lake?CAPACITY_LAKE - relief_m/100*PENALTY`,
    clamped `0..=CAPACITY_MAX`, computed once, stored as `u32`.
  - `grid::route(grid: &SocietyGrid, from: RegionCoords, to: RegionCoords, max_nodes: u32) -> Option<Vec<RegionCoords>>`
    — deterministic A* over 8-neighbour region tiles on `passable`, cost `step_cost` (diagonal ×1414/1000),
    Chebyshev heuristic × 1000, ties broken by `(f, g, z, x)`; route includes both ends.
  - `pub const ROUTE_MAX_NODES: u32 = 20_000;`

- [ ] **Step 1: Write the failing tests.** Add a test helper in `tests/history.rs`:
  `fn test_map(width: u32, height: u32, sea: impl Fn(i32, i32) -> bool) -> game::WorldMap` building
  tiles with `landing: Some([256, 256])`, `sea_fraction` 1.0 for sea tiles else 0.0, `rivers: 1`, relief 50 m.
  Tests:
  - `roll_is_stateless`: `roll(t, 5, SALT_MARRY) == roll(t, 5, SALT_MARRY)` and differs for subject 6.
  - `death_age_in_range`: for ids 0..1000, `death_age_days` / `DAYS_PER_YEAR` in `[80, 160]`.
  - `birth_rate_falls_with_crowding`: `birth_per_mille(10, 100) > birth_per_mille(80, 100)`,
    `birth_per_mille(100, 100) == 0`, `birth_per_mille(5, 0) == 0`.
  - `capacity_from_tiles`: sea tile capacity 0 and not passable; land tile capacity `40 + 10 = 50`.
  - `route_avoids_sea`: 10×3 map with a sea column at x=5 except row z=2 → route from (0,0) to (9,0)
    exists and every step is passable and passes through (5,2).
  - `route_unreachable`: a full sea column → `None`.
  - `route_deterministic`: two calls equal.
- [ ] **Step 2:** run — FAIL (missing items).
- [ ] **Step 3:** implement.
- [ ] **Step 4:** `just test-one game "--test it history::"` PASS; `just smoke` PASS.
- [ ] **Step 5: Commit** `feat(history): hashed rolls, tunables, society grid and world-map routing`.

---

### Task 3: History core — queue, log, people, sites, births, deaths, marriages

**Files:**
- Modify: `crates/game/src/history/mod.rs`
- Create: `crates/game/src/history/{log.rs,chain.rs}`
- Test: `crates/game/tests/history.rs`

**Interfaces:**
- Produces:
  - `log::LogEntry { pub at: CalendarTick, pub kind: LogKind }`;
    `log::LogKind`: `Born { person, mother: Option<PersonId>, father: Option<PersonId>, site: Option<SiteId> }`,
    `Died { person, cause: DeathCause }`, `Married { a: PersonId, b: PersonId }`,
    `Departed { people: Vec<PersonId>, from: SiteId, purpose: Purpose }`,
    `Arrived { people: Vec<PersonId>, site: SiteId }` (migrants), `SiteFounded { site, civ, founders: Vec<PersonId>, from: Option<SiteId> }`,
    `SiteAbandoned { site }`. `fn describe(&self, h: &History) -> String` (one English line, uses names
    and the date, e.g. `"12 Slate 40: Dwarf 7 was born at Site 2 to Dwarf 3 and Dwarf 4"`).
  - `pub enum Step { SiteMonth(SiteId), Death(PersonId), JourneyLeg(JourneyId) }` (internal to the
    module but `pub(crate)`).
  - `pub struct History` (Serialize/Deserialize) fields: `seed: u64`, `now: CalendarTick`,
    `people: BTreeMap<PersonId, Person>`, `sites: BTreeMap<SiteId, Site>`, `civs: BTreeMap<CivId, Civ>`,
    `journeys: BTreeMap<JourneyId, Journey>`, `queue: BTreeMap<(CalendarTick, u64), Step>`, `next_seq: u64`,
    `next_ids: [u32; 4]`, `log: Vec<LogEntry>`, `#[serde(skip)] grid: Option<SocietyGrid>`.
  - `impl History`:
    - `pub fn new(seed: u64, start: CalendarTick, grid: SocietyGrid) -> Self`
    - `pub fn set_grid(&mut self, grid: SocietyGrid)` (after load); `pub fn grid(&self) -> &SocietyGrid` (panics if unset)
    - `pub fn now(&self) -> CalendarTick`
    - `pub fn add_civ(&mut self, race: Race) -> CivId`
    - `pub fn add_founder(&mut self, sex: Sex, born: CalendarTick) -> PersonId` — a person with no
      parents, not yet at a site (`Location::AtSite` is set by `found_site`); schedules `Death` at
      `born + death_age_days`.
    - `pub fn found_site(&mut self, civ: CivId, region: RegionCoords, founders: &[PersonId], from: Option<SiteId>, player_founded: bool) -> SiteId`
      — capacity from the grid, sets citizenship + location of founders, logs `SiteFounded`, schedules
      the site's first `SiteMonth` one month later.
    - `pub fn advance(&mut self, to: CalendarTick)` — pops and handles every queued step with
      `at <= to` in `(at, seq)` order, setting `now` to each step's time; finally `now = max(now, to)`.
    - `pub fn person(&self, id) -> Option<&Person>`, `site`, `civ`, `journey`, `people()`, `sites()`,
      `civs()` iterators, `log(&self) -> &[LogEntry]`, `living_members(&self, site) -> Vec<PersonId>`.
    - `pub(crate) fn schedule(&mut self, at: CalendarTick, step: Step)`.
    - `pub fn record_death(&mut self, person: PersonId, cause: DeathCause)` — the public form of
      `on_death` (tests use it; plan 2's emergent region facts will too). Dead already: no-op.
  - `chain.rs` handlers (`impl History`, `pub(crate)`):
    - `on_death(id)`: if alive → `died = (now, OldAge)`, widow the spouse, remove from site members,
      log; if the site now has no living members → abandon it (log, no more `SiteMonth`).
    - `on_site_month(site)`: skip abandoned. In this order: (1) marriages — iterate unmarried adults
      in id order; for each, `chance_per_mille(now, id, SALT_MARRY, MARRY_PER_MILLE_MONTHLY)` pairs
      them with the lowest-id unmarried adult of the other sex at the site who is not a parent,
      child or sibling; (2) births — for each married couple with a fertile mother
      (`age <= FERTILE_MAX_AGE_YEARS`, `last_birth` gap ≥ `BIRTH_MIN_GAP_DAYS`), roll
      `birth_per_mille(members, capacity)`, unless the civ is at `CIV_POPULATION_CAP`; sex by
      `roll(.., SALT_SEX) & 1`; (3) migration/embark (Task 5 adds; leave a call to
      `self.consider_leaving(site)` that does nothing yet); (4) re-schedule `SiteMonth` one month on.
- [ ] **Step 1: Write the failing tests:**
  - `site_grows_by_births`: map 8×8 land; civ; 5 couples (founders aged 20 years, `add_founder(sex, start - 20y)`),
    found a site; `advance(start + 10 years)`; assert living members > 10, the log has ≥ 1 `Married`
    and ≥ 1 `Born` entries, every `Born` child has `mother.spouse == father`.
  - `old_people_die`: one founder born 170 years before start; `advance(start + 1 day)` → dead with
    `OldAge`, `Died` logged, site abandoned (`SiteAbandoned` logged).
  - `history_is_deterministic`: build the `site_grows_by_births` world twice, advance 20 years, assert
    `bincode::serialize(&a) == bincode::serialize(&b)` (grid is skipped).
  - `advance_is_idempotent_and_monotonic`: advance to T, clone, advance to T again and to T−100 →
    serialized equal to the clone; `now() == T`.
  - `no_marriage_between_siblings`: two siblings (same mother/father set via a born child pair) only
    at a site → never married after 10 years.
- [ ] **Step 2:** run — FAIL.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** tests PASS (tag any >100 ms test slow); `just smoke` PASS.
- [ ] **Step 5: Commit** `feat(history): event queue, log, births, deaths and marriages`.

---

### Task 4: Journeys

**Files:**
- Modify: `crates/game/src/history/{mod.rs,chain.rs}`
- Test: `crates/game/tests/history.rs`

**Interfaces:**
- Produces:
  - `pub fn start_journey(&mut self, people: Vec<PersonId>, from: SiteId, to_region: RegionCoords, purpose: Purpose) -> Option<JourneyId>`
    — `None` (no change) if `route` finds no path. Otherwise: removes the people from their site's
    members (citizenship stays for `Embark` until founding; for `Migrate` it moves on arrival), sets
    `Location::Journey`, logs `Departed`, schedules `JourneyLeg` at `departed + leg_ticks`
    (`leg_ticks = walk_ticks_per_region()` × the route step's `step_cost`/1000). A route of one region
    (same region) arrives at the next step.
  - `pub fn journey_region_at(&self, j: JourneyId, at: CalendarTick) -> Option<RegionCoords>` — the
    route region occupied at `at` (plan 2 uses it for catch-up), and
    `pub fn journey_progress_in_leg(&self, j, at) -> u32` (per-mille through the current leg).
  - `on_journey_leg(j)`: `leg += 1`; at the route's end → `arrive(j)`; else schedule the next leg.
    Plan 2 adds a hook here (`self.entered_region(j, region)`), so put the "entered new region" point
    in its own private method now (no-op body).
  - `arrive(j)`: `Migrate { to }` — if `to` is abandoned or full, they still join (a site may exceed
    capacity); set site/location, members, log `Arrived`. `Embark` → handled in Task 5
    (`found_site(…, from: Some(from), false)` with the party as founders). Remove the journey.
  - A person who dies on a journey is removed from it; an empty journey is removed.
- [ ] **Step 1: Write the failing tests:**
  - `migrants_arrive_after_travel_time`: two sites 6 regions apart on land; start a `Migrate` journey
    of 2 people; at `departed + 3 * leg` `journey_region_at` is the 4th route region; after
    `advance(departed + 7 * leg)` both are members of the target, the log has `Departed` then `Arrived`.
  - `journey_without_route_is_refused`: target across a full sea column → `None`, people unchanged.
  - `death_on_journey`: one traveller born 170 years ago dies en route; the other arrives; no step left
    referencing the dead journey once it ends (`queue` has no `JourneyLeg` for a missing journey).
- [ ] **Step 2–4:** fail, implement, pass, `just smoke`.
- [ ] **Step 5: Commit** `feat(history): journeys over the world map`.

---

### Task 5: Migration, embark and founding

**Files:**
- Modify: `crates/game/src/history/chain.rs`, `rules.rs`
- Test: `crates/game/tests/history.rs`

**Interfaces:**
- Produces:
  - `rules::household_of(h: &History, person: PersonId) -> Vec<PersonId>` — the person, living spouse,
    and living children under `ADULT_AGE_YEARS` at the same site; id order.
  - `pub fn embark_target(&self, civ: CivId, from_region: RegionCoords) -> Option<RegionCoords>` —
    the best region within `EMBARK_MAX_SEARCH_REGIONS` (Chebyshev) of `from_region` with
    `capacity > 0`, no site (abandoned sites count as taken), Chebyshev distance ≥
    `EMBARK_MIN_DISTANCE_REGIONS` from every non-abandoned site of any civ, and a route; score =
    `capacity * 1000 / (distance + 1)`; ties by `(z, x)`. Pure and deterministic.
  - `pub fn embark_allowed(&self, region: RegionCoords) -> Result<(), EmbarkRefusal>` with
    `pub enum EmbarkRefusal { OffMap, NotHabitable, Occupied, TooClose { site: SiteId } }` — the
    shared validity rule (plan 3's player embark uses it too).
  - `consider_leaving(site)` (from Task 3): if `crowding_per_mille ≥ CROWDED_PER_MILLE`:
    - a civ site with `crowding < CROWDED_PER_MILLE` reachable by route → with
      `MIGRATE_PER_MILLE_MONTHLY` the household of the lowest-id adult (by roll-chosen start index)
      starts a `Migrate` journey to the least-crowded such site; and with
      `EMBARK_BASE_PER_MILLE_MONTHLY` an embark is considered too;
    - otherwise with `EMBARK_PER_MILLE_MONTHLY`: gather whole households (roll-chosen starting
      household, then id order) until ≥ `EMBARK_PARTY_MIN` people, never more than `EMBARK_PARTY_MAX`;
      at least one adult of each sex; if `embark_target` is `Some`, start an `Embark` journey;
      else nobody leaves.
  - `arrive` for `Embark`, in order of preference:
    1. `embark_allowed(region)` still holds → `found_site(civ, region, party, Some(from), false)`;
    2. else (taken meanwhile) → re-target with `embark_target(civ, region)` and continue the journey;
    3. else → `Migrate` to the least-crowded living civ site reachable by route;
    4. else → every member gets `site: None` and `Location::Wandering(region)` (add this variant to
       `Location` in this task), and takes no further part; log `Departed` already records them.
    Test `embark_party_with_nowhere_to_go_wanders` covers 4: a narrow 1-tile-wide land strip; the
    civ's only site sends an embark party to the strip's far end; while they travel, the test founds
    a second civ's site on the target by hand (2 fails: no other free region on the strip) and kills
    the origin's remaining people by hand (`record_death`) so the origin is abandoned (3 fails). On
    arrival every party member is `Wandering` and no step references them except their `Death`.
- [ ] **Step 1: Write the failing tests:**
  - `crowded_site_sends_migrants`: civ with two sites 6 apart; site A at capacity, site B nearly
    empty → within 2 years ≥ 1 `Departed { purpose: Migrate }` from A and B gains members.
  - `crowded_civ_embarks_and_founds`: one crowded site (members = capacity) on a large land map →
    within 5 years a `SiteFounded` with `from: Some(A)`, its founders ≥ 7, its region at Chebyshev
    distance ≥ 4 from A.
  - `crowded_site_with_no_target_stays`: single site on a 5×5 island (sea around) → after 5 years no
    `Departed`, no new site.
  - `embark_allowed_rules`: off-map → `OffMap`; sea → `NotHabitable`; a site's region → `Occupied`;
    3 regions from a site → `TooClose`; 4 regions → `Ok`.
  - `extinct_civ_leaves_no_live_steps`: a site of 2 old founders (born 159 years ago) → after 3 years
    the civ has no living members, every site abandoned, the queue holds no step for a dead person,
    abandoned site, or removed journey.
- [ ] **Step 2–4:** fail, implement, pass, `just smoke`.
- [ ] **Step 5: Commit** `feat(history): migration, embark parties and site founding`.

---

### Task 6: Bootstrap, fast-forward, statistics

**Files:**
- Create: `crates/game/src/history/{bootstrap.rs,stats.rs}`
- Test: `crates/game/tests/history.rs`

**Interfaces:**
- Produces:
  - `pub struct BootstrapParams { pub seed: u64, pub civs: u32, pub founders_per_civ: u32, pub years: u32, pub civ_min_spacing_regions: u32, pub max_attempts: u32 }`
    with `Default`: `civs: 4, founders_per_civ: 30, years: 100, civ_min_spacing_regions: 10, max_attempts: 8`.
  - `pub fn bootstrap(map: &crate::WorldMap, p: &BootstrapParams) -> Result<History, BootstrapError>`:
    for attempt `a` in `0..max_attempts` with sub-seed `seed ^ roll(CalendarTick(a), a, SALT_PLACE)`:
    1. `History::new(sub_seed, CalendarTick(0), SocietyGrid::from_world_map(map))`;
    2. place `civs` civs, each on the highest-capacity free region (ties by hashed order) whose
       Chebyshev distance to placed civs ≥ `civ_min_spacing_regions` (fewer civs placed when land
       runs out; zero placed ⇒ `BootstrapError::NoHabitableLand`);
    3. per civ, `founders_per_civ` founders as couples aged 15–60 years (hashed) plus children
       aged 0–11 filling the rest, linked as families; `found_site`;
    4. `advance(CalendarTick::from_days(years * DAYS_PER_YEAR))`;
    5. accept if ≥ 1 civ has living members, else next attempt. All attempts dead ⇒
       `BootstrapError::AllCivsDied { attempts }`.
    Play then starts at the history's `now()`.
  - `stats::HistoryStats { pub now: CalendarTick, pub civs: u32, pub civs_alive: u32, pub sites: u32, pub sites_alive: u32, pub alive: u32, pub dead: u32, pub per_civ: Vec<(CivId, u32 /*alive*/, u32 /*sites alive*/)>, pub events: BTreeMap<&'static str, u32> }`
    and `History::stats(&self) -> HistoryStats` with `Display` (multi-line summary like the forge's).
- [ ] **Step 1: Write the failing tests** (a 48×48 land map with a sea border 2 tiles wide):
  - `bootstrap_places_spaced_civs`: `years: 0` → 4 civs, pairwise Chebyshev ≥ 10, each with 30 people,
    each person's parents (when present) at the same site.
  - `fast_forward_spreads_civs` (slow-tagged if > 100 ms): `years: 100` → ≥ 1 civ alive, `sites > 4`,
    some site has `from: Some(_)`.
  - `fast_forward_respects_civ_cap`: `years: 300`, `founders_per_civ: 200` → every civ ≤ `CIV_POPULATION_CAP` alive at the end, and at every yearly check (advance year by year and assert).
  - `bootstrap_is_deterministic`: two runs serialize equal.
  - `bootstrap_no_land`: all-sea map → `Err(NoHabitableLand)`.
- [ ] **Step 2–4:** fail, implement, pass. **Tune** the Task 2 tunables until `fast_forward_spreads_civs`
  ends with roughly 3–8 sites per living civ and no civ above the cap on the test map; record the
  final values in the commit message. `just smoke`.
- [ ] **Step 5: Commit** `feat(history): bootstrap, fast-forward and statistics`.

---

### Task 7: Persistence

**Files:**
- Create: `crates/game/src/history/persist.rs`
- Test: `crates/game/tests/history.rs`

**Interfaces:**
- Produces (native only: `#[cfg(not(target_arch = "wasm32"))]` on the file functions; the byte
  functions are on every target):
  - `pub const HISTORY_FILE_VERSION: u32 = 1;` `pub const HISTORY_MAGIC: [u8; 8] = *b"MMOHIST\0";`
  - `pub fn encode(h: &History) -> Vec<u8>` — magic, version (u32 LE), `grid.world_id()` (u32 LE), then
    `lz4_flex::compress_prepend_size(bincode(h))`.
  - `pub fn decode(bytes: &[u8], grid: SocietyGrid) -> Result<History, HistoryLoadError>` — checks
    magic, version (`OldVersion { found }`), world id (`OtherWorld`), then deserializes and
    `set_grid(grid)`.
  - `pub enum HistoryLoadError { NotAHistory, OldVersion { found: u32 }, OtherWorld, Corrupt(String), Io(String) }` with `Display`.
  - `pub fn save(h: &History, path: &Path) -> std::io::Result<()>` — temp file in the same dir +
    rename (atomic, like `WorldArtifact::save`).
  - `pub fn load(path: &Path, grid: SocietyGrid) -> Result<History, HistoryLoadError>`.
  - `pub fn history_path_for(world_file: &Path) -> PathBuf` — same path with extension `history`
    (`worlds/<slug>/terrain.world` → `worlds/<slug>/terrain.history`; `worlds/default.world` →
    `worlds/default.history`).
- [ ] **Step 1: Write the failing tests:** `history_roundtrip` (bootstrap years 20 → encode → decode
  with the same map's grid → serialize equal, and continuing both 10 more years stays equal),
  `load_refuses_other_version` (patch the version bytes → `OldVersion { found: 99 }`),
  `load_refuses_other_world` (decode with a grid from a different map → `OtherWorld`),
  `garbage_is_not_a_history`, `save_is_atomic` (save into a temp dir; no `.tmp` file left; load equal),
  `history_path_for_world_files`.
- [ ] **Step 2–4:** fail, implement, pass, `just smoke`.
- [ ] **Step 5: Commit** `feat(history): history file save/load`.

---

### Task 8: The `just history` tool and docs

**Files:**
- Create: `crates/worldgen/examples/history.rs`
- Modify: `justfile`, `CLAUDE.md`

**Interfaces:**
- Consumes: `worldgen::forged::ForgedWorld::load`, `worldgen::tile_map::build_world_map`,
  `game::history::{bootstrap, BootstrapParams, persist}`.
- The example: flags `--world <file>` (default `worlds/default.world`), `--civs N`, `--years N`,
  `--founders N`, `--seed N`, `--log N` (print the last N log lines, default 40), `--save` (write the
  history next to the world via `history_path_for`; refuses to overwrite an existing file unless
  `--force`), `--dump <history file>` (load an existing history and print stats + log instead of
  bootstrapping). Prints timing, `HistoryStats`, and the log lines via `LogEntry::describe`.
- [ ] **Step 1:** write the example; add the recipe (release, `play` region size, like `cross-section`):

```just
# World history: bootstrap + fast-forward a world's history from its world file and
# print statistics and the log (`--save` writes <world>.history next to it, `--dump
# <file>` reads one). Flags: --world --civs --years --founders --seed --log --save --force.
history *args:
    {{cap}} {{cargo}} run --release -p worldgen --example history -- {{args}}
```

  (Copy how `cross-section` passes the play region size — `VOXEL_REGION_BLOCKS=512` — so region
  coordinates match the played world.)
- [ ] **Step 2:** run `just history --years 100` on `worlds/default.world` (if missing, say so and use
  `--world` with any forged file, or skip with a note). Expected: stats printed, ≥ 1 civ alive, time
  under a minute. Record the stats in the commit message.
- [ ] **Step 3:** CLAUDE.md: under `crates/game`'s description add one paragraph: "**World history**
  (`history/`, spec `docs/superpowers/specs/2026-11-01-world-history-events-design.md`): …" covering
  the calendar, records, queue, the rule that every world change is an event that can happen in play,
  the chain, bootstrap, persistence, `just history`, tests `just test-one game "--test it history::"`.
- [ ] **Step 4:** `just smoke` PASS.
- [ ] **Step 5: Commit** `feat(history): just history tool; docs`.
