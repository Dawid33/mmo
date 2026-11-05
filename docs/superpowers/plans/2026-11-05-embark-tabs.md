# Embark Tabs (Fortress / Adventure / Legends) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The arrival map's embark panel gets three tabs — Fortress (today's panel), Adventure (start the player's body in Adventure mode at a populated site of a chosen civ) and Legends (paged, filtered world-history log beside the map).

**Architecture:** Legends is a pure query on the server's `History` (`history/legends.rs`) answered page by page over two new packets. Adventure is a server-side move (`move_client`) followed by a server-originated `Order::SetMode(Adventure)` event in the target region, tracked in `WorldManager::adventurer_of` and mutually exclusive with `fortress_of`. The client mirrors both in `HistoryView` and the Slint `EmbarkPanel` becomes a `Tabs` row over three tab components.

**Tech Stack:** Rust, bincode packets (`game::protocol`), Bevy client (`renderer/`), Slint (`crates/slint-ui/ui/`).

**Spec:** `docs/superpowers/specs/2026-11-05-embark-tabs-design.md`

## Global Constraints

- Work on `develop` in the main checkout; never create worktrees (CLAUDE.md). Commit only when the user says so — the commit steps below are staging points; if commits are not authorised, skip them and leave the work in the tree.
- `game` and `server` stay Bevy-free. History containers stay `BTreeMap`/`Vec` (never `HashMap`).
- New packet variants are **appended** to `ClientPacket` / `ServerPacket` / `ClientRequest` / `ClientUpdateEvent` (bincode tags).
- Verify with `just test-one <crate> "<args>"` and `just smoke`; never whole suites. Client lib tests (Bevy test binary) are an explicit opt-in — ask before running them.
- No sim/hashed type changes in this plan (`PlayMode`, `Order` untouched) → `just determinism-wasm` not needed. If one changes anyway, run it.
- Tunables are named consts: `LEGENDS_PAGE_MAX: u16 = 200`, `LEGENDS_PAGE_DEFAULT: u16 = 100`.
- One at a time: a client holds a fortress **or** an adventure; a retired fortress stays returnable after an adventure ends.
- Adventure offers only **populated** sites: not abandoned and `population > 0`.
- Legends keeps the world map visible beside the widened panel.

## Review Focus

1. A client disconnecting while adventuring: `adventurer_of` must be dropped, and on reconnect the client gets `Fortress(None)` and the arrival map — not a stuck "already playing" refusal. (Task 3 test `adventure_dropped_on_disconnect`.)
2. A site depopulated or abandoned between the list and Start: the server refuses `Unpopulated` / `SiteAbandoned` and the refusal shows on the map status line; nothing moves. (Task 3 test `adventure_refuses_unpopulated_and_abandoned`.)
3. Legends queries naming an unknown civ / site / person, or a `before` cursor past the log's end: an empty or clamped page, `total` correct, never a panic. (Task 1 test `legends_unknown_filters_and_cursor_clamp`.)
4. Changing the Legends filter while a page is in flight: the late page for the old filter must be dropped, not appended under the new header. (Task 4: `LegendsState::accept` returns `false` for a page whose `query.filter` differs.)
5. Embark / ReturnTo while adventuring and StartAdventure while holding a fortress: refused, no state change. (Task 3 test `fortress_and_adventure_exclusive`.)

---

### Task 1: Legends query in `game::history`

**Files:**
- Create: `crates/game/src/history/legends.rs`
- Modify: `crates/game/src/history/mod.rs` (add `pub mod legends;`, re-export the types), `crates/game/src/history/log.rs` (add `subjects`)
- Test: `crates/game/tests/history.rs` (existing `it` module `history`)

**Interfaces:**
- Produces (all `Clone, Debug, PartialEq, Eq, Serialize, Deserialize`, re-exported as `game::history::*`):
  - `enum LegendsFilter { All, Civ(CivId), Site(SiteId), Person(PersonId) }`
  - `struct LegendsQuery { filter: LegendsFilter, before: Option<u32>, limit: u16 }`
  - `struct LegendLine { index: u32, text: String, sites: Vec<SiteId>, people: Vec<PersonId> }`
  - `struct LegendsPage { query: LegendsQuery, lines: Vec<LegendLine>, next: Option<u32>, total: u32 }`
  - `pub const LEGENDS_PAGE_MAX: u16 = 200; pub const LEGENDS_PAGE_DEFAULT: u16 = 100;`
  - `impl LogEntry { pub fn subjects(&self) -> (Vec<PersonId>, Vec<SiteId>) }`
  - `impl History { pub fn legends(&self, q: &LegendsQuery) -> LegendsPage }`

Semantics: `index` = position in `History::log`. Lines newest first, starting strictly below `before` (`None` = from the end; `before > log.len()` clamps to the end). `limit` clamped to `1..=LEGENDS_PAGE_MAX`. `next = Some(index of last returned line)` when older matches remain, else `None`. `total` = all matching entries in the whole log. Matching: `All` everything; `Person(p)` entries whose people contain `p`; `Site(s)` entries whose sites contain `s` or whose people include a current citizen of `s` (`Person::site == Some(s)`); `Civ(c)` entries naming a site with `Site::civ == c` or a person whose citizenship site is of `c`. `subjects` per `LogKind`: Born → person, mother, father / site; Died, Married, Departed (+ from, and `Purpose::Migrate { to }`), Arrived (+ site), SiteFounded (founders / site, from), SiteAbandoned (site). `text` = `LogEntry::describe`.

- [ ] **Step 1: Write failing tests** in `crates/game/tests/history.rs`, on a bootstrapped history (reuse the file's existing bootstrap helper):
  - `legends_all_pages_newest_first`: `legends(All, None, 10)` → 10 lines, indices strictly decreasing, first = `log.len()-1`; following `next` until `None` visits every index exactly once; `total == log.len()`.
  - `legends_site_filter_matches_subjects`: for a site `s`, every line has `s` in `sites` or a person whose `site == Some(s)`; `total` equals a brute-force count.
  - `legends_civ_filter_matches_its_sites`: likewise for a civ.
  - `legends_person_filter`: a person with a Born entry → that entry is in the page.
  - `legends_unknown_filters_and_cursor_clamp`: `Civ(CivId(9999))` → `lines.is_empty() && total == 0 && next.is_none()`; `before: Some(u32::MAX)` behaves as `None`; `limit: 0` returns 1 line, `limit: 5000` returns ≤ 200.
- [ ] **Step 2:** `just test-one game "--test it history::legends"` → FAIL (unresolved names).
- [ ] **Step 3:** Implement `subjects` and `legends.rs` as above (one linear scan from the newest end; count `total` in the same pass).
- [ ] **Step 4:** `just test-one game "--test it history::legends"` → PASS; `just test-one game "--test it history::"` still green.
- [ ] **Step 5: Commit** `feat(history): paged legends query over the event log`

### Task 2: Protocol + server: civ names and legends

**Files:**
- Modify: `crates/game/src/protocol.rs` (packets), `crates/game/src/world_manager.rs` (connect ~745, packet match ~770)
- Test: `crates/game/tests/history_play.rs` (its `World` harness: `connect`, `packet`, `take`)

**Interfaces:**
- Consumes: Task 1 types, `History::legends`.
- Produces:
  - `pub struct CivMarker { pub civ: CivId, pub name: String }` in `protocol.rs` (re-exported like `SiteMarker`)
  - `ServerPacket::Civs(Vec<CivMarker>)` — sent on connect next to `Sites`, from `history.civs` in id order (add `History::civ_markers(&self) -> Vec<CivMarker>` in `history/play.rs`).
  - `ClientPacket::RequestLegends(LegendsQuery)` → `ServerPacket::Legends(LegendsPage)` (answered straight from `self.history`; no history = no answer).

- [ ] **Step 1: Failing tests:** `connect_sends_civ_names` (after `connect(A)`, `take(A)` holds one `Civs` whose ids equal `history().civs` keys and names match); `legends_request_answered` (`packet(A, RequestLegends(q))` → exactly one `Legends(page)` with `page == history().legends(&q)`).
- [ ] **Step 2:** `just test-one game "--test it history_play::"` → FAIL.
- [ ] **Step 3:** Add packets, `civ_markers`, the connect send and the `RequestLegends` arm.
- [ ] **Step 4:** Tests PASS; `just test-one game "--test it packet_codec::"` green.
- [ ] **Step 5: Commit** `feat(server): civ names on connect, legends pages on request`

### Task 3: Server: start / end adventure

**Files:**
- Modify: `crates/game/src/protocol.rs`, `crates/game/src/history/chain.rs` (`AdventureRefusal` beside `EmbarkRefusal`), `crates/game/src/world_manager.rs` (`embark`, `return_to`, `client_disconnected`, new fns)
- Test: `crates/game/tests/history_play.rs`

**Interfaces:**
- Produces:
  - `pub enum AdventureRefusal { NoHistory, UnknownSite, SiteAbandoned, Unpopulated, AlreadyPlaying }` (+ `fn describe(&self) -> &'static str` for the status line, matching `EmbarkRefusal`'s style)
  - `ClientPacket::StartAdventure(SiteId)`, `ClientPacket::EndAdventure`
  - `ServerPacket::AdventureResult(Result<SiteId, AdventureRefusal>)`
  - `WorldManager` field `adventurer_of: BTreeMap<ClientId, SiteId>`; `fn start_adventure(&mut self, id, site, now_ms)`, `fn end_adventure(&mut self, id, now_ms)`

`start_adventure`: refuse in this order — no history `NoHistory`; unknown site `UnknownSite`; `abandoned.is_some()` `SiteAbandoned`; `members` with no living person `Unpopulated`; `fortress_of` or `adventurer_of` holds `id` → `AlreadyPlaying`. Then `move_client(id, region, landing_in(region), now_ms)`, `send_play_event(region, GameEventKind::Order(id, Order::SetMode { mode: PlayMode::Adventure, at: None }))`, insert `adventurer_of`, send `AdventureResult(Ok(site))`, `send_fortress(id)` (sends `None`), `perf_event!("adventure_start", "client={id} site={} to={},{}", …)`. `end_adventure`: no entry = no-op; else `send_play_event(home, Order(id, SetMode { Fortress, at: None }))`, remove entry, `move_client(id, SPAWN_REGION, landing_in(SPAWN_REGION), now_ms)`, send `Sites`. `embark` / `return_to` refuse with the existing `EmbarkRefusal::AlreadyHasFortress` while `adventurer_of` holds `id`. `client_disconnected` removes `adventurer_of[id]`.

- [ ] **Step 1: Failing tests** (helper `fn adventure(&mut self, c, site) -> Result<SiteId, AdventureRefusal>` mirroring `embark`):
  - `adventure_starts_at_populated_site`: `Ok(site)`; `player_region` of `take(A)` is the site's region; after `settle()`, the region's `Client` for `A` has `mode == PlayMode::Adventure`; a `Fortress(None)` packet was sent.
  - `adventure_refuses_unpopulated_and_abandoned`: an abandoned site → `SiteAbandoned`; a site whose members all died (kill them through the history API used elsewhere in the file) → `Unpopulated`; `SiteId(9999)` → `UnknownSite`; no `PlayerRegion` sent for any.
  - `fortress_and_adventure_exclusive`: embark then `StartAdventure` → `AlreadyPlaying`; a second client adventuring then `Embark` → `Err(AlreadyHasFortress)`.
  - `end_adventure_returns_to_spawn_in_fortress_mode`: after End, home is `SPAWN_REGION`, mode Fortress, and a following `Embark` succeeds.
  - `adventure_dropped_on_disconnect`: adventure, disconnect, reconnect the same id → `StartAdventure` succeeds again.
- [ ] **Step 2:** `just test-one game "--test it history_play::"` → FAIL.
- [ ] **Step 3:** Implement as above.
- [ ] **Step 4:** PASS; then `just test-one game "--test it -- --include-ignored adventure::"` and `just smoke` green.
- [ ] **Step 5: Commit** `feat(server): start and end an adventure at a civ's site`

### Task 4: Client plumbing and `HistoryView` state

**Files:**
- Modify: `crates/client/src/instance.rs` (`ClientRequest` + send arms ~720, server packet arms ~1654), `crates/game/src/lib.rs` (`ClientUpdateEvent` ~237), `crates/client/src/renderer/bridge.rs` (~825), `crates/client/src/renderer/history_view.rs`

**Interfaces:**
- Consumes: Tasks 1–3 packets.
- Produces:
  - `ClientRequest::{RequestLegends(LegendsQuery), StartAdventure(SiteId), EndAdventure}` (serde already carries it to the wasm sim worker)
  - `ClientUpdateEvent::{Civs(Vec<CivMarker>), Legends(LegendsPage), AdventureResult(Result<SiteId, AdventureRefusal>)}` and matching `HistoryMsg` variants
  - `HistoryView` fields: `civs: Vec<CivMarker>`, `tab: EmbarkTab` (`enum EmbarkTab { Fortress, Adventure, Legends }`, default Fortress), `adventure: AdventureSelection`, `adventure_result: Option<Result<SiteId, AdventureRefusal>>`, `adventure_pending: bool`, `adventure_refusal: Option<(AdventureRefusal, f32)>`, `adventuring: Option<SiteId>`, `legends: LegendsState`
  - `struct AdventureSelection { civ: Option<CivId>, site: Option<SiteId> }` with `fn civs(sites: &[SiteMarker], civs: &[CivMarker]) -> Vec<CivMarker>` (civs having ≥ 1 populated site, id order), `fn sites(&self, sites: &[SiteMarker]) -> Vec<SiteMarker>` (the picked civ's populated sites: `!abandoned && population > 0`), `fn pick_civ(&mut self, CivId)` (clears `site` on change), `fn pick_site(&mut self, SiteId)`, `fn retain_offered(&mut self, sites: &[SiteMarker])` (drops a site no longer populated)
  - `struct LegendsState { filter: LegendsFilter, lines: Vec<LegendLine>, next: Option<u32>, total: u32, pending: Option<LegendsQuery> }` with `fn query_first(&mut self, filter) -> LegendsQuery` (resets lines, sets pending, `before: None`, `limit: LEGENDS_PAGE_DEFAULT`), `fn query_next(&mut self) -> Option<LegendsQuery>` (`None` while pending or `next` is `None`), `fn accept(&mut self, page: LegendsPage) -> bool` (`false` and ignored when `page.query != pending`; else appends, clears pending)

- [ ] **Step 1: Failing unit tests** in `history_view.rs`'s `#[cfg(test)]` module: `adventure_lists_only_populated_sites`, `adventure_pick_civ_clears_site`, `adventure_retain_drops_depopulated_site`, `legends_paging_appends_and_stops`, `legends_stale_page_dropped` (query A pending, filter changed to B, page for A → `accept == false`, lines empty).
- [ ] **Step 2:** Ask the user before running the client lib test binary; if approved `just test-one client "--lib history_view::"` → FAIL. Otherwise `just smoke` must fail to type-check.
- [ ] **Step 3:** Implement the types, the request/packet arms, the bridge forwarding and `apply_history_msgs` arms (`Civs` → `view.civs`; `Legends` → `view.legends.accept`; `AdventureResult` → `view.adventure_result`).
- [ ] **Step 4:** `just smoke` green (+ the client test run if approved).
- [ ] **Step 5: Commit** `feat(client): legends / adventure requests and HistoryView state`

### Task 5: Slint: tabbed embark panel

**Files:**
- Modify: `crates/slint-ui/ui/embark.slint`, `crates/slint-ui/ui/game.slint` (`EmbarkLayout`, properties ~163–175, panel ~390), `crates/slint-ui/ui/menu.slint` (End adventure button)
- Create: `crates/slint-ui/tests/embark.rs`; Modify: `crates/slint-ui/tests/it.rs` (`mod embark;`)

**Interfaces:**
- Produces on `GameUi` (names exact; the client wires them in Task 6):
  - `in-out property <int> embark-tab` (0 Fortress, 1 Adventure, 2 Legends); `callback embark-tab-changed(int)`
  - Adventure: `in property <[string]> adventure-civs`, `in property <int> adventure-civ-picked: -1`, `in property <[string]> adventure-sites` (`"<site> — pop N"`), `in property <int> adventure-site-picked: -1`, `in property <string> adventure-region`, `in property <bool> adventure-enabled`; callbacks `adventure-pick-civ(int)`, `adventure-pick-site(int)`, `adventure-start()`
  - Legends: `in property <[LegendsFilterRow]> legends-filters` (struct `LegendsFilterRow { text: string, depth: int, dimmed: bool }`: "All events", civs at depth 0, their sites at depth 1, abandoned dimmed), `in property <int> legends-filter-picked: 0`, `in property <string> legends-header`, `in property <[string]> legends-lines`, `in property <bool> legends-more`; callbacks `legends-pick-filter(int)`, `legends-load-more()`, `legends-line-clicked(int)`
  - Menu: `in property <bool> can-end-adventure`, `callback end-adventure()` (button "End adventure", shown like Retire)
  - `global LegendsLayout { out property <length> width: 2 * EmbarkLayout.width; }`; the map's `reserve-left` uses it while `embark-tab == 2`.

`EmbarkPanel` = `FrameFace "Embark"` → `Tabs { tabs: ["Fortress", "Adventure", "Legends"]; }` → one of `FortressTab` (today's body moved unchanged), `AdventureTab`, `LegendsTab` (filter column left ~⅓, `Flickable` of lines right; `legends-load-more` fires when the viewport reaches the bottom and `legends-more` is true).

- [ ] **Step 1: Failing headless tests** in `tests/embark.rs` (same pattern as `tests/world_map.rs`, tagged slow with the crate's ignore message): `tabs_switch_bodies` (click the Adventure tab → `embark-tab == 1`, `embark-tab-changed(1)` fired); `adventure_start_enabled_only_with_site`; `legends_filter_click_reports_index`; `menu_shows_end_adventure`.
- [ ] **Step 2:** `just test-one slint-ui "--test it -- --include-ignored embark::"` → FAIL.
- [ ] **Step 3:** Implement the components and properties.
- [ ] **Step 4:** PASS; `just test-one slint-ui "--test it -- --include-ignored world_map::"` still green.
- [ ] **Step 5: Commit** `feat(ui): Fortress / Adventure / Legends tabs on the embark panel`

### Task 6: Client UI wiring

**Files:**
- Modify: `crates/client/src/renderer/slint_overlay.rs` (push of embark state, `UiAction`s ~1150–1260, `apply_embark_result` ~1265), `crates/client/src/renderer/world_map.rs` if the refusal text lives there

**Interfaces:**
- Consumes: Task 4 state and Task 5 properties.
- Produces: `UiAction::{EmbarkTab(i32), AdventurePickCiv(i32), AdventurePickSite(i32), AdventureStart, EndAdventure, LegendsPickFilter(i32), LegendsMore, LegendsLine(i32)}`; system `apply_adventure_result` (registered next to `apply_embark_result`).

Behaviour:
- Push the Adventure lists from `AdventureSelection::civs/sites`; picking a site selects its tile on the map (`map_tile(map, site.region)` → `selected-col/row`) and centres it; in the Adventure and Legends tabs, tile refusal darkening is off.
- Start sends `ClientRequest::StartAdventure(site)` once (`adventure_pending`). `apply_adventure_result`: `Ok` → `adventuring = Some(site)`, `Embarked(true)`, `cam.placed = false`, close the map (as `apply_embark_result`); `Err` → `adventure_refusal` on the map status line for `REFUSAL_SHOW_SECS`.
- Esc menu: `can-end-adventure = adventuring.is_some()`; End sends `EndAdventure`, clears `adventuring`, `Embarked(false)` and reopens the arrival map (same steps as the Retire arm).
- Legends: first switch to the tab (or a filter pick) sends `query_first`; `LegendsMore` sends `query_next`; header `"<filter name> — N events"`; a line click with exactly one site in `sites` selects that site's tile.

- [ ] **Step 1:** Implement; `just smoke` green.
- [ ] **Step 2:** Ask before running `just test-one client "--lib slint_overlay::"` (existing tests there must stay green).
- [ ] **Step 3: Commit** `feat(client): wire the embark tabs, adventure start/end and legends`

### Task 7: Docs and live check

**Files:** `CLAUDE.md` (the "Embark, retire, return" paragraph and the arrival-map paragraph in `crates/slint-ui`), the spec's Status line.

- [ ] **Step 1:** Document: the three tabs, `StartAdventure` / `EndAdventure` / `AdventureResult`, `Civs`, `RequestLegends` / `Legends` + `history::legends`, the populated-site rule, one-at-a-time rule; set spec status to implemented.
- [ ] **Step 2: Live check** (ask first — the user runs their own sessions on this box): `just adapter=Intel run --reset` → Fortress tab embarks as before; Retire; Adventure tab lists only populated sites, Start lands the body in Adventure (selectable, F5 works); Esc → End adventure → arrival map; Embark works afterwards; Legends lists events, filters by civ/site, scrolls to older pages, line click selects the site.
- [ ] **Step 3: Commit** `docs: embark tabs`
