# Embark screen: Fortress / Adventure / Legends tabs — design + plan

Status: **implemented 2026-11-05 (uncommitted), live check pending.** v1 cuts: `CivMarker` has no `race`; Legends does not auto-refresh on a new calendar day (re-pick a filter); the client/server convergence test for the server-injected `SetMode` (§4) is not written yet.

## Intent

The arrival world map's embark panel (`crates/slint-ui/ui/embark.slint` `EmbarkPanel`, fed from
`renderer/history_view.rs` `HistoryView` by `renderer/slint_overlay.rs`) is today the only way into a
world: found a fortress or return to one. Following Dwarf Fortress's start screen, it becomes a
three-tab panel:

- **Fortress** — exactly today's panel (civ → people ≤ 7 → Embark on the selected tile, Return to …).
- **Adventure** — pick a civilisation, then one of its sites; Start drops the player's body there in
  Adventure mode. No character creation: the body is the ordinary player body (rigged as the dwarf),
  not a history `Person`.
- **Legends** — browse the world history (`History::log`, `LogEntry::describe`) as text, filterable
  by civ and site.

### Non-goals

- Character creation, picking a history person to play, adventurer stats/skills.
- Travel between regions beyond what Adventure already does (`Order::MoveTo` across seams).
- Legends maps, charts, family trees, or historical figures beyond what the log names.
- Opening Legends from inside a running game (later: an Esc-menu / Encyclopedia entry could reuse it).

## Current state (what we build on)

- **Server**: `WorldManager` handles `ClientPacket::{RequestEmbarkOptions, Embark, Retire, ReturnTo}`
  (`crates/game/src/world_manager.rs` ~598–680); `move_client(id, region, landing)` sends `ClientLeft` /
  `CreateClientAt` and `PlayerRegion`; `landing_in(rc)` picks the dry landing column; `fortress_of`
  maps client → held site; `send_fortress` sends `ServerPacket::Fortress`.
- **Adventure mode exists in the sim** (`crates/game/src/adventure.rs`): per-client `PlayMode`,
  switched by `Order::SetMode { mode, at }` (a `GameEventKind::Order(ClientId, Order)` event); entering
  Adventure with `at` places the body on the nearest standable tile. Nothing in the UI switches the
  mode since the F9 panel went (`ToggleModeRequest` remains). The client mirrors it in
  `commander::adventure::ClientPlayMode`; F5 = first person in Adventure.
- **History log**: `History::log: Vec<LogEntry>` — append-only, persisted; `LogKind` = Born, Died,
  Married, Departed, Arrived, SiteFounded, SiteAbandoned; `describe(&History)` gives
  `"<date>: <line>"`. Default bootstrap (4 civs × 30 founders × 100 years) makes thousands of entries,
  so the client cannot be sent the whole log on every open. `Person` has no civ — only `site`
  (citizenship); `Site` has `civ`.
- **Client flow**: `session::Embarked(bool)` gates the arrival map; `apply_embark_result` sets it on
  `EmbarkResult(Ok)` and closes the map; Retire (Esc menu) clears it and reopens the map.
- **UI**: `controls.slint` already has a `Tabs` component (used by the fortress panel).

## Design

### 1. Tabbed panel (Slint)

`EmbarkPanel` gains `in-out property <int> tab` (0 Fortress, 1 Adventure, 2 Legends) and a `Tabs`
row (`tabs: ["Fortress", "Adventure", "Legends"]`, not collapsible) under the frame title; each tab's
body is its own component in `embark.slint`:

- `FortressTab` — today's body, moved unchanged.
- `AdventureTab` — civ buttons (civs with at least one populated site), then the picked civ's **populated** sites (not abandoned, population > 0) as
  toggle buttons (`"<site> — pop N"`), a caption with the picked site's region, and **Start
  adventure** (enabled with a site picked and no request pending).
- `LegendsTab` — see §3.

The tab lives in Slint and is mirrored to the client as `GameUi.embark-tab` (`changed` callback →
`UiAction::EmbarkTab(i)`), since the map and requests depend on it.

**Map interplay per tab** *(my call)*:
- Fortress: as today (tile selection, refused tiles darkened, site markers).
- Adventure: no tile-refusal darkening; picking a site selects its tile on the map and centres it;
  clicking a site marker on the map picks that site if it belongs to the picked civ (or switches civ).
- Legends: the panel widens to `LegendsLayout.width` (≈ 2× the embark width, reserved via the map's
  existing `reserve-left`) because event lines are long; the map stays visible to its right; picking
  a site filter selects its tile like Adventure.

### 2. Adventure start (protocol + server)

New packets (`crates/game/src/protocol.rs`):

```rust
ClientPacket::StartAdventure(SiteId)
ServerPacket::AdventureResult(Result<SiteId, AdventureRefusal>)
// game::history (or protocol): AdventureRefusal { NoHistory, UnknownSite, SiteAbandoned,
//                                                Unpopulated, AlreadyPlaying }
```

The site list comes from `Sites(Vec<SiteMarker>)` (civ, region, population, abandoned), grouped by
civ — **not** `EmbarkOptions`, which drops sites with nobody free to leave and civs with no such site
(`History::embark_options`). Civ names are not on `SiteMarker`, so add one small packet
`ServerPacket::Civs(Vec<CivMarker { civ, name, race }>)`, sent on connect (civs never change after
bootstrap today); Legends' filter column uses it too.

`WorldManager::start_adventure(id, site, now_ms)`:
1. Refuse when there is no history, the site is unknown, abandoned or has no living members
   (`Unpopulated`), or the client already holds a fortress or an adventure (`AlreadyPlaying`).
2. `landing = landing_in(site.region)`; `move_client(id, region, landing, now_ms)`.
3. Right after it, `send_play_event(region, GameEventKind::Order(id, Order::SetMode { mode: Adventure,
   at: None }))` — a **server-originated** order, sequenced after `CreateClientAt` in the same region,
   so client and server agree deterministically. `at: None` because `CreateClientAt` already put the
   body on the landing column. `send_play_event` already injects arbitrary `GameEventKind`s into a
   region (that is how `CreateClientAt` / `ClientLeft` arrive), so the order needs no new path; the
   only risk is the client having predicted nothing for it — it arrives as a confirmed server event
   exactly like `CreateClientAt`. *(Alternative: a `CreateClientAt` carrying the mode — rejected,
   it changes a sim event shared by every path.)*
4. Record `adventurer_of: BTreeMap<ClientId, SiteId>` (sorted map, like `fortress_of`), answer
   `AdventureResult(Ok(site))`, `perf_event!("adventure_start", …)`.

**Ending an adventure** *(my call)*: the Esc menu shows **End adventure** in place of Retire while an
adventure is held → `ClientPacket::EndAdventure` → server sets the mode back to Fortress, moves the
player to `SPAWN_REGION` like `retire`, drops `adventurer_of`, client returns to the arrival map. On
disconnect the entry is dropped (no persistence — an adventurer is not a history object yet).

**Fortress neighbourhood**: an adventurer holds no fortress, so `ServerPacket::Fortress(None)` and the
client subscribes only its near window (at the play recipes' 256 m regions: the one region + LOD).
Walking out across a seam already works (near window follows the body; `commander/bounds.rs`).

**Client** (`history_view.rs`, `slint_overlay.rs`):
- `HistoryView` gains `adventure: AdventureSelection { civ: Option<usize>, site: Option<SiteId> }`,
  `adventure_result`, `adventure_pending`, `adventuring: Option<SiteId>`.
- `apply_adventure_result`: on `Ok` → `Embarked(true)`, close the map, camera `placed = false`;
  `ClientPlayMode` follows the sim's mode as today, so the adventure HUD (body selectable, fortress
  tools down, F5) needs no new code. A refusal shows on the map status line like embark refusals.
- `ClientRequest::StartAdventure(SiteId)` / `EndAdventure` through the game thread
  (`sim_driver` / `netcode` as `Embark` is plumbed; wasm sim worker too — `ToSim` carries
  `ClientRequest` already).

### 3. Legends (server query, paged)

The log stays on the server; the client asks for one page at a time.

```rust
ClientPacket::RequestLegends(LegendsQuery)
ServerPacket::Legends(LegendsPage)

pub enum LegendsFilter { All, Civ(CivId), Site(SiteId), Person(PersonId) }
pub struct LegendsQuery { pub filter: LegendsFilter, pub before: Option<u32>, pub limit: u16 }
pub struct LegendsPage {
    pub query: LegendsQuery,          // echoed, so a stale page is recognised
    pub lines: Vec<LegendLine>,       // newest first
    pub next: Option<u32>,            // `before` for the next (older) page
    pub total: u32,                   // matching entries, for "1–200 of 3,412"
}
pub struct LegendLine { pub index: u32, pub text: String, pub sites: Vec<SiteId>, pub people: Vec<PersonId> }
```

- `index` = position in `History::log` (append-only, so a stable cursor across pages and saves).
- `History::legends(&LegendsQuery) -> LegendsPage` (new `history/legends.rs`, pure, unit-tested):
  `LogEntry::subjects() -> (Vec<PersonId>, Vec<SiteId>)` from each `LogKind`; `Civ(c)` matches entries
  naming a site of `c` or a person whose citizenship site is of `c`; `Site(s)` entries naming `s` or a
  person who is (or was) its citizen *(my call: citizenship now, not historical — simpler, slightly
  wrong for migrants)*; `Person(p)` entries naming `p`. Scan newest → oldest from `before`, cap
  `limit` at `LEGENDS_PAGE_MAX` (200); `total` by a full count *(log of a few thousand entries — a
  linear scan per request is fine; revisit with an index if logs reach 10⁵+)*.
- `WorldManager` answers directly from `self.history` (no region involved); no server rate limit — the
  client keeps at most one query in flight (like person-info requests, which are not limited either).

**Legends tab UI**:
- Left: a filter column — "All events", then civs, each expandable to its sites (abandoned sites
  greyed). Picking one sends a fresh query (page 1).
- Right: the event lines in a `Flickable`, newest first; scrolling to the bottom requests `next`
  (append). A header line `"Events of <filter> — N"`.
- Clicking a line whose subjects include one site selects that site's tile on the map *(my call;
  name hyperlinks → per-person filter is a follow-up, as is a person search box)*.
- `HistoryView.legends: LegendsState { filter, lines, next, total, pending }`, reset with the session
  (`HistoryView` is already `add_session_reset`, `renderer/mod.rs`) and refreshed when a new calendar day
  arrives only if the user scrolls back to the top (no live push in v1).

### 4. Tests

- `game`: `history::legends` — filters per kind, paging cursor stable across appends, newest-first,
  limit cap (`just test-one game "--test it history::"`).
- `game`: `world_manager` — `StartAdventure` moves the client, sets Adventure, refusals; `EndAdventure`
  restores Fortress at spawn; embark refused while adventuring and vice versa.
- `game` (seeded): the server-originated `SetMode` after `CreateClientAt` converges client and server
  (extend an existing adventure convergence test rather than a new harness).
- `client` lib: `AdventureSelection` / `LegendsState` pure logic (`--lib history_view::`) — opt-in, ask
  before running (Bevy test binary).
- `slint-ui`: tab switching + each tab's callbacks headless (`just test-one slint-ui "--test it --
  --include-ignored embark::"`).
- `just smoke`; `just determinism-wasm` only if a hashed/sim type changes (it does not, by this
  design: `PlayMode` and `Order` are unchanged).

## Plan (tasks, in order)

1. **Legends core** — `history/legends.rs`: `LegendsFilter/Query/Page/Line`, `LogEntry::subjects`,
   `History::legends`; tests. (No protocol yet.)
2. **Protocol** — add `Civs`, `RequestLegends`/`Legends`, `StartAdventure`/`EndAdventure`/`AdventureResult`,
   `AdventureRefusal` (+ `describe` text for the status line). Bincode wire only; append variants at
   the end of the enums.
3. **Server** — `WorldManager`: legends answer, `start_adventure`, `end_adventure`,
   `adventurer_of` cleanup on disconnect, mutual exclusion with `fortress_of`; tests.
4. **Client plumbing** — `ClientRequest` variants through native netcode + wasm sim worker;
   `ClientUpdateEvent` → `HistoryMsg` for `Legends` and `AdventureResult`; `HistoryView` state.
5. **Slint** — split `EmbarkPanel` into `Tabs` + `FortressTab` (pure move) + `AdventureTab` +
   `LegendsTab`; new `GameUi` properties/callbacks; `EmbarkLayout`/`LegendsLayout` widths; slint-ui
   tests.
6. **Client UI wiring** — `slint_overlay.rs`: push tab state, adventure site list, legends lines;
   `UiAction`s for tab, pick site, start adventure, legends filter / next page / line click; map
   selection per tab; `apply_adventure_result`; Esc menu "End adventure".
7. **Docs** — CLAUDE.md (embark/arrival map paragraph, protocol list), this file's status.
8. **Live check** — `just run --reset` on Intel: each tab; start an adventure at another civ's site,
   walk, F5, End adventure, embark a fortress afterwards.

Steps 1–3 and 5 are independent and can go in parallel; 4 needs 2; 6 needs 4 and 5.

## Decisions (user, 2026-11-05)

1. One at a time: a player holds either a fortress or an adventure; a retired fortress stays
   returnable after an adventure ends.
2. Legends keeps the world map beside the (widened) panel.
3. Adventure offers only populated sites (not abandoned, living members > 0); the server refuses
   others with `Unpopulated`.
