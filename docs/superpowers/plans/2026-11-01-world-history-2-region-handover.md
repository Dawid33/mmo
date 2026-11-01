# World History 2 — Region Hand-over Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The world event manager's people exist physically in running regions: a region that starts
is caught up (its site's people and travellers inside it spawn), manager-decided outcomes are played
out in running regions (births, deaths, parties walking out, arrivals walking in), people walking out
into a non-running region continue abstractly without waking it, a parking region hands its people
back, and the server keeps the history alive and saved alongside the world.

**Architecture:** Persons are a new `EntityKind::Person { person: PersonId }` rigged as the dwarf, so
existing seam crossing, ghosts and snapshots carry them unchanged. The manager → region direction is
new server-authored `GameEventKind`s (in the event stream like `CreateClientAt`); region → manager is
the existing `Departures` plus one new `RegionOutput::PeopleParked`. `History` gains a hand-over API
and a directive queue; `WorldManager` owns an `Option<History>` and a calendar derived from `now_ms`.
All decisions stay manager-side (spec option (a)).

**Tech Stack:** Rust, `game` (rollback sim, rapier fork), `server`; Bevy `client` only to keep it compiling.

**Spec:** `docs/superpowers/specs/2026-11-01-world-history-events-design.md` §3, §4, §6, §8, §10.
**Depends on:** plan 1 (`2026-11-01-world-history-1-event-manager-core.md`) merged.

## Global Constraints

- All of plan 1's Global Constraints apply.
- New `GameEventKind` variants are **server-authored**: `origin_client()` returns `None`, and
  `WorldManager::handle_server_event` drops them when a client sends them (add them to the existing
  "Manager-internal / server-authored" arm).
- Sim changes are rollback-safe: every mutation through `*_safe` functions; `hash(before) ==
  hash(after undo)` must hold — exercise new mutators under `HashMode::Full` in at least one test
  (tests in `it` run under the process default `Full`).
- Determinism across targets: no `f32` transcendental in sim paths (`libm::` only). Run
  `just determinism-wasm` at the end of Task 1 and Task 2 (sim changes).
- Threading tests: run `just test-threads` at the end of Task 5 (touches `WorldManager` routing and
  handoff).
- The client must keep type-checking (`just smoke` type-checks it) — exhaustive matches on
  `EntityKind`/`GameEventKind` in `crates/client` get minimal arms here; plan 3 does the real client work.

## Review Focus

1. **A person in two places** — a person spawned by catch-up while a stale body for them still sits in
   the region's parked blob, or delivered both by `EntityArrived` and a catch-up `SpawnPerson`. Expected:
   at most one body per `PersonId` per world, ever (Task 3 test `spawn_person_is_idempotent`, Task 5
   test `no_person_twice_across_park_cycles`).
2. **A directive for a region that is stopping** (Shutdown sent, `PeopleParked` not yet received) —
   must not wake it or be lost; the step is retried after the people are back (Task 4 test
   `step_for_stopping_region_is_deferred`).
3. **Player walks an adventure body into a site's region** (wakes it) — the site's people appear;
   when the player leaves and it parks, they go back abstract (Task 5 test `neighbour_wake_catches_up`).
4. **A traveller whose route region is off the world map edge or sea when it crosses a seam
   physically** (walked off-route) — rerouted from where they are, or `Wandering` (Task 4 test
   `off_route_crossing_reroutes`).
5. **Server restart** — history file saved on shutdown after regions park (people back), reloaded
   with the same people and calendar; a mismatched/old file refuses to start with a clear message
   (Task 6 test `history_saved_on_shutdown_reloads`).

---

## File Structure

- Modify `crates/game/src/state.rs` — `EntityKind::Person`, `person_entities` index.
- Create `crates/game/src/people.rs` — person spawn/despawn/lookup, idle wander + steering, walk-out.
- Modify `crates/game/src/adventure.rs` — generic `walk_entity_safe`; walkers for persons.
- Modify `crates/game/src/protocol.rs` — new `GameEventKind` variants, `PersonSpawn`, `SpawnAt`.
- Modify `crates/game/src/region.rs`, `region_runner.rs` — apply the variants; `PeopleParked` on stop.
- Create `crates/game/src/history/handover.rs` — the hand-over API + directive queue.
- Modify `crates/game/src/history/{mod.rs,chain.rs}` — directives from chain steps for running regions.
- Modify `crates/game/src/world_manager.rs` — own the history, calendar, hooks.
- Modify `crates/server/src/{lib.rs,world_file.rs}` — load/bootstrap/save the history.
- Modify (minimal) `crates/client/src/renderer/{avatar.rs,commander/selection.rs,slint_overlay.rs}` — exhaustive matches.
- Tests: `crates/game/tests/people.rs`, `crates/game/tests/history_handover.rs` (both `mod` in `it.rs`),
  `crates/server/tests/…` per Task 6.

---

### Task 1: Person entities in the sim

**Files:** `state.rs`, `people.rs` (new), `lib.rs`, client exhaustive matches; test `tests/people.rs`.

**Interfaces:**
- Produces:
  - `EntityKind::Person { person: crate::history::PersonId }` (Copy; `PersonId` already derives `StateHash`).
  - `GameData` field `#[undo(map)] person_entities: BTreeMap<PersonId, EntityKey>`.
  - `impl Rollback` in `people.rs`:
    - `pub fn spawn_person_safe(&mut self, person: PersonId, xz: (f32, f32)) -> Option<EntityKey>` —
      `None` (no change) if the person already has a body here. Body exactly like
      `create_player_at_safe` (kinematic, capsule `PLAYER_CAPSULE_*`, `Motion`, dwarf rig via
      `attach_rig_safe`) but **no camera, no client**; `SetEntityKind` emitted with undo twin;
      `person_entities` insert.
    - `pub fn despawn_person_safe(&mut self, person: PersonId) -> bool` — `remove_entity_safe` +
      index remove.
    - `pub fn person_entity(&self, person: PersonId) -> Option<EntityKey>`.
    - `pub fn people_here(&self) -> Vec<(PersonId, [f32; 3])>` — region-local body translations, id order.
  - Seam crossing: `extract_entity_safe` / `apply_arrival` must keep `person_entities` in step for
    `Person` kinds (remove on extract, insert on arrival); arrival of a person who already has a body
    is ignored (idempotent).
  - Client (minimal): `kind_label` → `"Dwarf"` for `Person`; `is_selectable` → true for `Person`;
    `attach_avatars` draws no capsule for `Person` (the rig draws it, like the rigged player).
- [ ] **Step 1: Failing tests** in `tests/people.rs` (use the flat-region helpers in `tests/common`):
  `spawn_person_is_idempotent` (second spawn → `None`, one body), `despawn_person_undo_restores_hash`
  (state hash before == after undo of despawn under Full), `person_crosses_seam_keeps_identity`
  (place a person at a region edge in a two-region `InlineSpawner` world as in `handoff_manager.rs`,
  push it across; target's `person_entity` is `Some`, source's `None`).
- [ ] **Step 2:** `just test-one game "--test it people::"` FAIL. **Step 3:** implement.
  **Step 4:** PASS; `just smoke`; `just determinism-wasm`.
- [ ] **Step 5: Commit** `feat(game): person entities (dwarf-rigged, PersonId) in the sim`.

---

### Task 2: People walk — walk-to, wander, steering

**Files:** `adventure.rs`, `people.rs`, `region.rs`; test `tests/people.rs`.

**Interfaces:**
- Produces:
  - `pub fn walk_entity_safe(&mut self, e: EntityKey, goal: Goal, here: RegionCoords, map: Option<MapExtent>) -> bool`
    — the body of `move_to_safe` after it resolves the key; `move_to_safe` calls it.
  - `tick_walkers_safe` drives walkers of adventure players **and** `Person` entities (widen the
    entity filter; keep key order deterministic).
  - `pub fn tick_people_safe(&mut self, here: RegionCoords, map: Option<MapExtent>)` called in the
    `Tick` arm right after `tick_walkers_safe`, which:
    1. **steers** every `Person` with a walker toward its next waypoint at `movement::WALK_SPEED`
       through `movement::plan` + the KCC, the way `CameraController` steers an adventurer from its
       walker (reuse that code path by extracting a shared function rather than duplicating it);
    2. **idles**: a `Person` without a walker picks, every `WANDER_MIN_TICKS + roll % WANDER_RANGE_TICKS`
       ticks (stateless roll as in `slime.rs`, subject = PersonId), a standable tile within
       `WANDER_RADIUS_BLOCKS` of its current tile and walks there. Store the next-wander tick in a
       small rollback component `Wander { next_tick: u64 }` (`ecs.wander`).
    Constants in `people.rs`: `WANDER_MIN_TICKS = 100`, `WANDER_RANGE_TICKS = 200`, `WANDER_RADIUS_BLOCKS = 8`.
  - `pub fn walk_out_safe(&mut self, person: PersonId, toward: RegionCoords, here: RegionCoords, map: Option<MapExtent>) -> bool`
    — walker with `Goal` in region `toward` (centre column), so the existing `edge_toward` exit logic
    walks them out across the seam.
- [ ] **Step 1: Failing tests:** `person_walks_to_goal` (spawn, `walk_entity_safe` to a tile 10 blocks
  away on flat ground, run 400 ticks → within `WAYPOINT_REACH_M`), `person_wanders_near_spawn` (2000
  ticks → never further than `WANDER_RADIUS_BLOCKS + 2` from spawn), `walk_out_departs`
  (`walk_out_safe` east in a 2-region world → a `Departures` entry with a `Person` kind toward x+1
  within 2000 ticks — tag slow if needed), `adventure_move_to_unchanged` (existing adventure tests
  still pass: run `just test-one game "--test it -- --include-ignored adventure::"`).
- [ ] **Steps 2–4:** fail, implement, pass; `just smoke`; `just determinism-wasm`.
- [ ] **Step 5: Commit** `feat(game): people walk, wander and walk out of a region`.

---

### Task 3: Manager→region events and region→manager parking

**Files:** `protocol.rs`, `region.rs`, `region_runner.rs`, `world_manager.rs` (drop arm only); test `tests/people.rs`.

**Interfaces:**
- Produces:
  - `pub struct PersonSpawn { pub person: PersonId, pub at: SpawnAt, pub walk_to: Option<Goal> }`
  - `pub enum SpawnAt { Column([u32; 2]), NearPerson(PersonId), Edge { from: RegionCoords, along_per_mille: u32 } }`
    — `Edge`: on this region's edge facing `from`, `along_per_mille` along it (0 = the lower x/z end),
    snapped to the nearest standable column (`pathfind::nearest_standable`); `NearPerson`: the nearest
    standable column within 2 blocks of that person's body, falling back to `Column` of the region
    landing if they have no body here.
  - `GameEventKind::SpawnPerson(PersonSpawn)`, `GameEventKind::DespawnPerson(PersonId)`,
    `GameEventKind::WalkOut(PersonId, RegionCoords)` — region applies via Task 1/2 functions.
  - `RegionOutput::PeopleParked(Vec<(PersonId, [f32; 3])>)` — sent by `RegionRunner` on `Shutdown`
    **before** `Stopped`: it applies any held inputs, then collects `people_here()`, despawns every
    person (`despawn_person_safe`, committed), then serializes. A parked blob therefore never
    contains people; they are owned by the manager while the region is parked.
- [ ] **Step 1: Failing tests** (drive `RegionRunner` directly as `tests/region_runner.rs` does):
  `spawn_person_event_spawns`, `spawn_near_person`, `spawn_at_edge_is_on_edge` (x == 0 block column for
  `from` = x−1), `shutdown_reports_and_strips_people` (two people → `PeopleParked` with both, then
  `Stopped`; restoring the blob shows no `Person` entity), `client_cannot_send_spawn_person`
  (`handle_server_event` with a `ClientPacket::GameEvent(SpawnPerson …)` → region never receives it).
- [ ] **Steps 2–4**; `just smoke`.
- [ ] **Step 5: Commit** `feat(game): server-authored person events; parking hands people back`.

---

### Task 4: History hand-over API and directives

**Files:** `history/handover.rs` (new), `history/{mod.rs,chain.rs,records.rs}`; test `tests/history_handover.rs`.

**Interfaces:**
- Produces (`impl History`):
  - `pub struct Directive { pub region: RegionCoords, pub event: GameEventKind }`;
    `pub fn take_directives(&mut self) -> Vec<Directive>`.
  - Region state: `#[serde(skip)] running: BTreeSet<RegionCoords>`, `#[serde(skip)] stopping: BTreeSet<RegionCoords>`.
  - `pub fn region_started(&mut self, rc: RegionCoords) -> Vec<GameEventKind>` — catch-up at `now`:
    - every living person `AtSite(s)` with `site(s).region == rc` → `SpawnPerson { at: Column(landing of rc, offset by a hashed ±4 blocks), walk_to: None }`;
    - every journey whose `journey_region_at(now) == rc` → its people `SpawnPerson { at: Edge { from: previous route region (or own region if first), along: hashed }, walk_to: Some(goal) }`
      with goal = the next route region's centre (walk on through) or, if `rc` is the destination,
      the destination's landing column; mark the journey `held_in = Some(rc)` (new field, `Option<RegionCoords>`);
    - those people become `Location::InRegion(rc)` (keep `site`/journey links in new fields:
      `Person::home_location: Location` stores what they were, so they can be restored).
    - `running.insert(rc)`.
  - `pub fn region_stopping(&mut self, rc)` — `running.remove`, `stopping.insert`.
  - `pub fn region_parked(&mut self, rc, people: Vec<(PersonId, [f32; 3])>)` — `stopping.remove`;
    each listed person: if their `home_location` is a journey held in `rc` → `Journey`, unhold it and
    schedule its next leg at `now + leg_ticks / 2`; else `AtSite(their site)`. Any person still
    `InRegion(rc)` but not listed (lost — must not happen) → log `warn!` and restore the same way.
  - `pub fn person_crossed(&mut self, person: PersonId, to: RegionCoords) -> bool` — called by the
    manager for every person in `Departures`. Returns **true when the manager must deliver the bundle**
    (target running: location `InRegion(to)`); false → absorbed: if on a journey whose route contains
    `to` → set `leg` to its index, unhold, location `Journey`, schedule next leg at `now + leg_ticks`;
    if `to` is off-route → re-route from `to` to the journey's destination (`grid::route`) or, with no
    route, `Wandering(to)`; a citizen not on a journey (walked off by wandering) → start a `Migrate`
    journey from `to` back to their own site.
  - Chain changes (`chain.rs`), when the people a step acts on are `InRegion(rc)`:
    - `rc` running → push a `Directive`: birth → `SpawnPerson { at: NearPerson(mother) }` for the
      child (child `InRegion(rc)`); death → `DespawnPerson`; journey start → `WalkOut(p, route[1])` for
      each traveller and `held_in = Some(rc)` (no abstract leg scheduled while held);
    - `rc` stopping → reschedule the step one day later (no directive, nothing lost).
    - A `JourneyLeg` that enters a **running** region → `SpawnPerson … Edge` directives (as in
      catch-up) and hold; if that region is the destination, arrival/founding is recorded **at
      once** (the walk in is cosmetic).
- [ ] **Step 1: Failing tests** (pure `History`, no regions — the manager is simulated by calling the API):
  `catch_up_spawns_site_people`, `catch_up_spawns_traveller_on_route`, `death_in_running_region_despawns`,
  `birth_in_running_region_spawns_near_mother`, `departure_in_running_region_walks_out`,
  `crossing_into_unloaded_region_is_absorbed`, `crossing_into_running_region_is_delivered`,
  `step_for_stopping_region_is_deferred`, `parked_people_return_to_site`, `off_route_crossing_reroutes`,
  `held_journey_does_not_advance_abstractly`.
- [ ] **Steps 2–4**; `just smoke`.
- [ ] **Step 5: Commit** `feat(history): region hand-over API and directives`.

---

### Task 5: WorldManager integration

**Files:** `world_manager.rs`; test `tests/history_handover.rs` (manager-level, `InlineSpawner`), `tests/handoff_schedule.rs` (one seeded case).

**Interfaces:**
- Produces (`impl WorldManager`):
  - `pub fn set_history(&mut self, history: History, now_ms: u64)` — stores it with
    `calendar_anchor = (now_ms, history.now())`; catches up every already-running region.
  - `pub fn calendar_now(&self, now_ms: u64) -> Option<CalendarTick>` —
    `anchor_tick + (now_ms - anchor_ms) / TICK_RATE`.
  - `pub fn history(&self) -> Option<&History>`.
  - Hooks:
    - `ensure_running`, after spawning: `history.region_started(rc)` → send each event as
      `RegionInput::Event` (before anything else is sent to it).
    - `maintain(now_ms)`: before the unload pass, `history.advance(calendar_now)`, then deliver
      `take_directives()` with `send_to_region` **only** to running, non-stopping regions (never
      `ensure_running`); a directive whose region is not running is a bug → `debug_assert!` + drop.
      When `maintain` sends `Shutdown`: `history.region_stopping(rc)`.
    - `RegionOutput::PeopleParked(list)` → `history.region_parked(rc, list)`.
    - `RegionOutput::Departures`: for a bundle whose `kind` is `Person { person }`:
      `if history.person_crossed(person, target) { (existing path) } else { skip — no ensure_running }`.
      With no history installed, persons follow the existing path.
- [ ] **Step 1: Failing tests** (two- and three-region `InlineSpawner` worlds with a forged-free
  `WorldMap` built like plan 1's `test_map`, a history with one site in region (0,0)):
  `site_region_start_spawns_its_people`, `neighbour_wake_catches_up` (a player walks into the site
  region from (1,0) and back out; people appear, then on park are `AtSite` again), `walk_out_does_not_wake_neighbour`
  (a forced departure toward (1,0); (1,0) never starts; the person is on a journey),
  `no_person_twice_across_park_cycles` (park/unpark the site region 5 times while births and a
  departure happen; at every point each living person has ≤ 1 body across all running regions and is
  never both `InRegion` and abstract), `calendar_follows_now_ms`.
  Seeded layer: add `history_departure_under_jitter` to `handoff_schedule.rs` (slow-tagged like its
  neighbours) — a departure across a seam under `InlineSchedule::jittery(seed)` for 8 seeds: person
  never duplicated or lost.
- [ ] **Steps 2–4**; `just smoke`; `just test-threads`.
- [ ] **Step 5: Commit** `feat(game): world manager runs the world history`.

---

### Task 6: Server — load, bootstrap, save

**Files:** `crates/server/src/{lib.rs,world_file.rs}`; test `crates/server/src/world_file.rs` unit tests
(+ a slow-tagged integration test if a real file round trip needs the server loop).

**Interfaces:**
- Produces:
  - `world_file::ensure_history(world_path: &Path, map: &WorldMap) -> Result<History, String>` —
    path from `history::persist::history_path_for`; exists → `persist::load` (errors → `Err` with
    "delete <path> to re-create this world's history"); missing → `history::bootstrap(map,
    &BootstrapParams::default())` with `seed = grid.world_id() as u64`, `persist::save`, log the stats
    line (`perf_event!("history_bootstrapped", "ms=… civs=… sites=… alive=…")`).
  - In `lib.rs` after `manager.set_world_map(map)`: `manager.set_history(ensure_history(..)?
    .unwrap_or_else(panic), now_ms = 0)` for forged worlds only (procedural: no history, unchanged).
  - Saving: every `HISTORY_SAVE_SECS = 60` in the main loop (`persist::save(manager.history())`), and
    after the orderly-exit loop has parked every region (people are back), a final save. Save errors
    are logged, never fatal.
- [ ] **Step 1: Failing tests:** `ensure_history_bootstraps_then_loads` (temp dir, a tiny synthetic
  world map → first call writes the file; second call loads an equal history),
  `ensure_history_refuses_other_world`; `history_saved_on_shutdown_reloads` (slow, real server loop
  if feasible — otherwise test the save-after-park ordering at the manager level in `game`).
- [ ] **Steps 2–4**; `just smoke`.
- [ ] **Step 5:** CLAUDE.md: extend the World history paragraph with the hand-over (persons,
  `PeopleParked`, absorbed crossings, catch-up, history file next to the world, save cadence).
  Commit `feat(server): world history loaded, bootstrapped and saved with the world`.
- [ ] **Step 6: Live check (with the user's OK — they run their own benches on this box):**
  `just play`, new world, Play a region that holds a site (`just history --dump worlds/<slug>/terrain.history`
  lists sites + regions): the site's dwarves stand and wander there; leave and come back — the same
  people. Report what was seen.
