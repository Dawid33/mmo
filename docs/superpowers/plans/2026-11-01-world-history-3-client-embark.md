# World History 3 — Embark, Retire, Return and Client Views Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Players enter fortress mode the DF way — embark from a civilisation with chosen people onto
empty land, retire, return to a retired fortress — and see the living world: sites on the world map,
the calendar, a notification line when events touch their fortress, and a person inspector.

**Architecture:** New packets between client and `WorldManager`; the player's embark is the same
`History` Embark event (validated by plan 1's `embark_allowed`), founding at once. Client state lives in
one session-reset resource; UI in Slint (world-map marker model + an embark panel; Esc-menu Retire;
status-line notices; inspector lines in the Fortress panel).

**Tech Stack:** Rust; `game`, Bevy `client`, `slint-ui` (Slint, fortress theme).

**Spec:** `docs/superpowers/specs/2026-11-01-world-history-events-design.md` "Players and the world",
§7b, §9. **Depends on:** plans 1 and 2 merged.

## Global Constraints

- All of plan 1's Global Constraints apply.
- UI is Slint only; every root component calls `GameTheme.apply()` (existing roots do). Any new
  per-world client resource → `add_session_reset::<R>()`; new `Local`s → `SessionLocal`.
- The client test binary is a slow Bevy build: **ask the user before running**
  `just test-one client "--lib …"`. `just smoke` type-checks the client and is always fine.
- Player identity (call recorded in the design): there are no accounts; `ClientId` is per connection.
  "Return only to a fortress you founded" is implemented as "only to a **player-founded** site no other
  connected player is playing" — identical in single-player. Note it in the code where the rule lives.
- Embark party size: `EMBARK_PLAYER_PARTY_MAX = 7` (DF's seven), min 1, in `history/rules.rs`.
- The dev `PlayRegion` path stays (dev scaffolding) but the arrival map's primary action becomes Embark.

## Review Focus

1. **Embark with people who stopped being available** (died, left on a journey, now in a running
   region) between the options being sent and the request — refused with a reason, no partial embark
   (Task 1 test `embark_refuses_unavailable_people`).
2. **Two players embarking onto the same or adjacent tiles at once** — the second is refused
   `Occupied`/`TooClose` (Task 1 test `second_embark_too_close`).
3. **Retire, then reconnect** — the player has no fortress, lands on the embark/return map; the retired
   site is listed as returnable (Task 1 test `retire_then_return`).
4. **Leaving to the main menu mid-embark-panel and loading another world** — no embark options, sites,
   notices or inspector cache leak into the next world (Task 2: session-reset registration, covered by
   the existing `session reset leaves no world state behind` test pattern — extend it).
5. **Notices for a fortress the player is not in** (another player's, or an unplayed site) — never shown
   to this player (Task 1 test `notices_only_to_home_region`).

---

## File Structure

- Modify `crates/game/src/protocol.rs` — packets and payload types.
- Create `crates/game/src/history/play.rs` — player embark/retire/return, options, notices, person info.
- Modify `crates/game/src/world_manager.rs` — handlers, `fortress_of`, broadcasts.
- Modify `crates/game/src/lib.rs` (`ClientUpdateEvent` variants), `crates/client/src/{instance.rs,renderer/bridge.rs}`.
- Create `crates/client/src/renderer/history_view.rs` — `HistoryView` resource + systems.
- Modify `crates/client/src/renderer/slint_overlay.rs` — markers, embark panel, Retire, notices, inspector, F3 line.
- Modify `crates/slint-ui/ui/{world-map.slint,game.slint,menu.slint,dev-panel.slint}`; create `crates/slint-ui/ui/embark.slint`.
- Tests: `crates/game/tests/history_play.rs` (`mod` in `it.rs`); client `--lib` unit tests in `history_view.rs`.

---

### Task 1: Protocol and manager — embark, retire, return, notices, person info

**Interfaces:**
- Produces (`game::protocol` unless noted):
  - `ClientPacket::RequestEmbarkOptions`, `ClientPacket::Embark(EmbarkRequest)`,
    `ClientPacket::Retire`, `ClientPacket::ReturnTo(SiteId)`, `ClientPacket::RequestPersonInfo(PersonId)`.
  - `pub struct EmbarkRequest { pub civ: CivId, pub people: Vec<PersonId>, pub region: RegionCoords }`.
  - `ServerPacket::Sites(Vec<SiteMarker>)` — sent on connect and whenever the set or a population
    bucket changes (founded, abandoned, population crossing a multiple of 10), at most once per
    calendar day; `pub struct SiteMarker { pub site: SiteId, pub civ: CivId, pub region: RegionCoords, pub name: String, pub population: u32, pub abandoned: bool, pub returnable: bool }`.
  - `ServerPacket::Calendar(CalendarTick)` — on connect and once per calendar day.
  - `ServerPacket::Notice(String)` — a `LogEntry::describe` line, to clients whose home is the region of
    a site the entry touches (Born/Died/Married at that site; Departed from it; Arrived/Founded there).
  - `ServerPacket::EmbarkOptions(EmbarkOptions)`; `pub struct EmbarkOptions { pub civs: Vec<CivOption> }`,
    `CivOption { civ, name, sites: Vec<SiteOption> }`, `SiteOption { site, name, region, people: Vec<PersonOption> }`,
    `PersonOption { person, name, age_years: u32, sex: Sex, spouse: Option<PersonId> }` — only living
    people `AtSite` of a non-abandoned, non-running site.
  - `ServerPacket::EmbarkResult(Result<SiteId, EmbarkRefusal>)`; extend plan 1's `EmbarkRefusal` with
    `NoHistory, AlreadyHasFortress, PartySize, UnavailablePerson(PersonId), NotPlayerFounded, InUse`.
  - `ServerPacket::PersonInfo(PersonInfo)`; `pub struct PersonInfo { pub person, pub name, pub age_years, pub alive: bool, pub site: Option<String>, pub spouse: Option<String>, pub parents: Vec<String>, pub children: Vec<String> }`.
  - `history/play.rs` (`impl History`): `pub fn embark_options(&self) -> EmbarkOptions`;
    `pub fn player_embark(&mut self, req: &EmbarkRequest) -> Result<SiteId, EmbarkRefusal>` —
    validates (`embark_allowed`, party 1..=`EMBARK_PLAYER_PARTY_MAX`, every person alive, of the civ,
    `AtSite` of a non-running site), then logs `Departed` per source site and founds the site with
    `player_founded: true` **at once** (no journey; spec §7b); `pub fn returnable(&self, site) -> bool`
    (player-founded, not abandoned); `pub fn person_info(&self, p) -> Option<PersonInfo>`;
    `pub fn site_markers(&self) -> Vec<SiteMarker>`; `pub fn take_notices(&mut self) -> Vec<(RegionCoords, String)>`
    (filled as log entries are appended).
  - `WorldManager`: `fortress_of: BTreeMap<ClientId, SiteId>` (not persisted: a fortress is "played"
    only while its player is connected; on disconnect the entry stays until the process ends so a
    reconnect resumes, mirroring `homes`). Handlers:
    - `Embark` → refuse `AlreadyHasFortress` if `fortress_of` has the client; `player_embark`; on Ok:
      move the client like `play_region` (`ClientLeft` old home, `CreateClientAt(landing)` new;
      `homes`, `PlayerRegion`) **after** `ensure_running(region)` so catch-up spawns the founders;
      `fortress_of.insert`; reply `EmbarkResult`.
    - `Retire` → `fortress_of.remove`; `ClientLeft` in the home; `homes.remove`; `PlayerRegion(None)`;
      the region parks by the usual grace rule; reply a fresh `Sites`.
    - `ReturnTo(site)` → `returnable` and no other client's `fortress_of` holds it, else
      `NotPlayerFounded`/`InUse`; then as Embark's move.
    - `RequestEmbarkOptions` → `EmbarkOptions`; `RequestPersonInfo` → `PersonInfo` (ignored if unknown).
    - In `maintain`: after `advance`, broadcast `Calendar` on day change, `Sites` when changed, and
      route `take_notices()`.
- [ ] **Step 1: Failing tests** in `tests/history_play.rs` (manager with `InlineSpawner`, test world map,
  bootstrapped history `years: 0`): `embark_founds_site_with_chosen_people`, `embark_refuses_unavailable_people`,
  `second_embark_too_close`, `one_fortress_per_player`, `retire_then_return`,
  `return_refuses_other_players_fortress`, `notices_only_to_home_region`, `client_cannot_spoof_server_events`
  (existing drop arm covers the new kinds), `person_info_lists_family`.
- [ ] **Steps 2–4:** fail, implement, `just test-one game "--test it history_play::"` pass; `just smoke`.
- [ ] **Step 5: Commit** `feat(history): player embark, retire, return; sites, calendar, notices, person info`.

---

### Task 2: Client plumbing — `HistoryView`

**Files:** `crates/game/src/lib.rs` (`ClientUpdateEvent::{Sites, Calendar, Notice, EmbarkOptions, EmbarkResult, PersonInfo}`),
`crates/client/src/instance.rs` (packet → update, next to `ServerPacket::WorldMap` at ~:1496; new
`ClientRequest::{RequestEmbarkOptions, Embark(EmbarkRequest), Retire, ReturnTo(SiteId), RequestPersonInfo(PersonId)}`
next to `PlayRegion` at :39 and `handle_request` :631), `renderer/bridge.rs` (update → message, like
`WorldMapMsg` at :724), new `renderer/history_view.rs`, registration in `renderer/mod.rs` next to
`WorldMapState` (:112–115, :219).

**Interfaces:**
- Produces: `#[derive(Resource, Default)] pub struct HistoryView { pub sites: Vec<SiteMarker>, pub calendar: Option<CalendarTick>, pub notices: VecDeque<(String, f32 /*shown_at secs*/)>, pub embark_options: Option<EmbarkOptions>, pub embark_result: Option<Result<SiteId, EmbarkRefusal>>, pub people: BTreeMap<PersonId, (PersonInfo, f32 /*received secs*/)> }`,
  registered with `add_session_reset::<HistoryView>()`; `HistoryMsg` message (cleared per session);
  `apply_history_msgs` system; `pub fn current_notice(&self, now: f32) -> Option<&str>` (oldest notice
  younger than `NOTICE_SHOW_SECS = 6.0`, dropping expired ones); `pub const PERSON_INFO_REFRESH_SECS: f32 = 5.0`.
- [ ] **Step 1: Failing unit tests** in `history_view.rs` (`#[cfg(test)]`, no App needed):
  `notices_expire_in_order`, `person_info_cache_refresh_due`. Extend the session-reset test
  (`3d418f2 test(client): session reset leaves no world state behind` — find it with
  `grep -rn "session reset leaves no world state" crates/client`) to cover `HistoryView`.
- [ ] **Step 2:** `just smoke` (type-check). **Ask the user** before
  `just test-one client "--lib history_view::"`.
- [ ] **Step 3:** implement. **Step 4:** smoke (+ the client tests if approved).
- [ ] **Step 5: Commit** `feat(client): history view state from the server`.

---

### Task 3: World map — site markers and the embark flow

**Files:** `crates/slint-ui/ui/world-map.slint`, `game.slint`, new `embark.slint`;
`crates/client/src/renderer/slint_overlay.rs` (`sync_world_map` :1354, `apply_ui_actions` :992,
`connect_game_ui` :590, `arrival_map` :964).

**Interfaces / behaviour:**
- Markers (model, not raster — so they update without re-rasterising the map): Slint
  `struct SiteMark { col: int, row: int, color: color, label: string }`, `in property <[SiteMark]> sites`
  on `WorldMap`, drawn with a `for` loop like the home rectangle (world-map.slint:134–158): a small
  filled square per site, civ colour from a fixed 8-colour palette by `civ.0 % 8`, abandoned sites
  hollow. The selected tile's `tile-lines` add "Site: <name> (<civ>), <population> dwarves" when it
  holds one. Rust: `site_marks(view: &HistoryView, map: &WorldMap) -> Vec<SiteMark>` (pure, unit-tested).
- Embark panel (`embark.slint`, `export component EmbarkPanel`, embedded in `game.slint` beside the
  map, visible while the arrival map is open or after Retire): civ list → its sites → people with
  check boxes (name, age, sex, "(married to …)"), a counter "n / 7", the region readout, and an
  **Embark** button enabled when 1..=7 people are chosen and the selected tile passes the client-side
  check; a **Return to <site>** list of returnable sites. Callbacks → `UiAction::{EmbarkPickCiv(i32),
  EmbarkTogglePerson(i32, i32), Embark, ReturnTo(i32)}`.
- Client-side tile check: `game::history::SocietyGrid::from_world_map` + the same distance/occupancy
  rule as `History::embark_allowed`, factored in plan 1 as a pure function over `(grid, site regions)`
  — **extract it here** as `pub fn embark_allowed_in(grid: &SocietyGrid, sites: impl Iterator<Item = (SiteId, RegionCoords)>, region) -> Result<(), EmbarkRefusal>`
  and make `History::embark_allowed` call it. Disallowed tiles get a translucent dark overlay on hover
  and the reason in `play-reason`.
- The arrival map's primary action becomes Embark (Play stays as a dev action, visible only with the
  `dev_ui` feature). `Embarked(true)` is set on `EmbarkResult(Ok)`; a refusal shows on the map status
  line via the existing refusal mechanism (`REFUSAL_SHOW_SECS`).
- [ ] **Step 1: Failing unit tests:** `site_marks_positions_and_colours`, `embark_allowed_in_matches_history`
  (game-side test in `tests/history_play.rs`), `embark_selection_caps_at_seven` (pure selection-state
  struct `EmbarkSelection` in `history_view.rs`).
- [ ] **Steps 2–4:** smoke; ask before client tests; implement.
- [ ] **Step 5: Commit** `feat(client): world-map site markers and the embark panel`.

---

### Task 4: In-fortress views — Retire, notices, inspector, calendar

**Files:** `menu.slint`, `game.slint`, `dev-panel.slint`; `slint_overlay.rs` (`push_ui_state` :1425,
`selection_lines` :1099, game-status :1414–1420); `renderer/dev_ui.rs`.

**Behaviour:**
- Esc menu: **Retire fortress** button (visible when the player has a fortress — `HistoryView` knows
  from `EmbarkResult`/`PlayerRegion`; add `fortress: Option<SiteId>` to `HistoryView`) →
  `UiAction::Retire` → `ClientRequest::Retire`, then the arrival map opens (`Embarked(false)`).
- Notices: `game-status` shows `HistoryView::current_notice` when the map status is empty (map status
  wins while the map is open).
- Inspector: when exactly one selected entity is a `Person`, `selection_lines` shows its `PersonInfo`
  (request on selection and every `PERSON_INFO_REFRESH_SECS`): name, "age N", site, "married to …",
  "parents: …", "children: …"; until the info arrives: "Dwarf N".
- F3 dev panel: a "Date: <Date>" line from `HistoryView::calendar`.
- [ ] **Step 1:** unit tests for the pure text builders: `person_lines_full_family`, `person_lines_pending`.
- [ ] **Steps 2–4:** smoke; ask before client tests; implement.
- [ ] **Step 5:** CLAUDE.md: replace the "Play (world map)" description's player flow with embark /
  retire / return and the history views (keep the dev Play note). Commit
  `feat(client): retire, event notices, person inspector, calendar line`.
- [ ] **Step 6: Live check (with the user's OK):** `just play` → new world → arrival map shows site
  markers → pick a civ and 7 people → Embark on an empty tile 4+ regions from any site → the seven
  dwarves stand at the landing and wander; select one → inspector; wait for a notice (shorten nothing —
  just report whether any appeared); Esc → Retire → the map shows the site as returnable → Return →
  the same dwarves. Report what was seen, with screenshots if possible.
