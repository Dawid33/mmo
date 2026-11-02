# Fortress HUD panels Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the F9 Fortress panel with an EU5-style top-left fortress panel (header + Governance / People / Designations / Stocks tabs) and a top-right menu column (Settings, Encyclopedia, Factorio-style context box).

**Architecture:** All of it lives in the one `GameUi` Slint window (`crates/slint-ui/ui/game.slint`, compiled into `slint-ui-game`), split into new `.slint` files per panel. The client pushes plain string models into it from `renderer/slint_overlay.rs`; the data shaping (designation rows, governance lines) is pure functions in a new `renderer/fortress_panel.rs`. Settings and Encyclopedia are two new entries in the existing `Screens` modal set.

**Tech Stack:** Rust, Bevy 0.19, Slint (software renderer), the `@widgets` library from `~/Projects/ui` (fortress theme).

**Spec:** `docs/superpowers/specs/2026-11-02-fortress-hud-panels-design.md`

## Global Constraints

- Header labels and placeholder values exactly: `Fortress value` `0`, `Average happiness` `0`, `Food stocks` `0` (literal `0`, no dash).
- Tab names and order exactly: `Governance`, `People`, `Designations`, `Stocks`. Stocks body text: `No stocks yet`. Governance without a fortress: `No fortress`. Unknown person name: `Person #<id>`.
- Settings / Encyclopedia windows: title `Settings` / `Encyclopedia`, body `Nothing here yet`.
- Both panels 12 px from their corner; drawn only when `!map-modal` (the GameUi layer is already commander-only).
- No `visible`/`opacity` binding on a component ROOT in new `.slint` components (CLAUDE.md codegen rule); use `if` or bind on an inner element.
- Every new per-world client `Local` is a `crate::session::SessionLocal`; every new resource is registered with `add_session_reset`.
- F9 and `SLINT_PANEL_KEY` are removed, not reassigned. The Fortress/Adventure and First person buttons are removed; `ToggleModeRequest` / `ToggleFirstPersonRequest` messages and F5 stay.
- No commits: the working tree carries Dawid's uncommitted edits in the same files (`game.slint`, `slint_overlay.rs`); leave the result uncommitted.
- Verification: `just test-one slint-ui "--test it -- --include-ignored <file>::"`, `just smoke`. Client unit tests (`just test-one client "--lib fortress_panel::"`) build a Bevy test binary — run only if Dawid OKs it; `just smoke` type-checks them.

## Review Focus

1. **Tab collapse clicks**: clicking the open tab collapses (body gone, header + tabs stay); clicking another tab while collapsed opens it — pinned in Task 1 and Task 2 tests.
2. **Pointer leak**: a transparent pixel of either panel must not swallow clicks meant for the world — the collapsed fortress panel and an empty right column must leave the area below them non-opaque (Task 2/3 tests assert `!opaque_at` below the panels).
3. **Context box absence**: nothing selected and no tool → the box draws no pixel (Task 3 test).
4. **Esc ordering with the new windows**: Esc closes Settings/Encyclopedia before opening the Esc menu; M opens the map and closes them (Task 5 `Screens` unit tests).
5. **Long lists**: 200 people / 200 orders must not push the panel off the bottom of a 720 px window — the list body is a `PickList` capped by `max-visible` (Task 2 test with 200 rows asserts nothing opaque below y = 700).

---

### Task 1: Collapsible `Tabs`

**Files:**
- Modify: `crates/slint-ui/ui/controls.slint` (`Tabs`, lines ~238–295; `ControlsHarness` for the probe)
- Test: `crates/slint-ui/tests/controls.rs`

**Interfaces:**
- Produces: `Tabs.collapsible: bool` (default `false`, existing users unchanged). With it set, clicking the selected tab sets `selected = -1` and fires `changed(-1)`; with `selected == -1` the content area (`@children` rectangle) is not drawn and the component's preferred height is the tab row plus the frame's top/bottom padding only (`out property <length> collapsed-height`).

- [ ] **Step 1: failing test** `tabs_collapse_on_second_click` in `controls.rs`, using a `Tabs` added to `ControlsHarness` with `collapsible: true` and three tabs (expose `harness-tabs-selected` as `out` and the first tab's centre as probes): click tab 0 → selected 0; click tab 0 again → selected −1; click tab 1 → selected 1.
- [ ] **Step 2:** `just test-one slint-ui "--test it -- --include-ignored controls::tabs_collapse"` → FAIL (property missing).
- [ ] **Step 3:** implement in `Tabs`: in `clicked`, `if i >= 0 && i == root.selected && root.collapsible { root.selected = -1; root.changed(-1); }`; wrap the content `Rectangle` in `if root.selected >= 0:`.
- [ ] **Step 4:** same command → PASS; also `controls::` whole file passes (non-collapsible behaviour unchanged).

### Task 2: Top-left fortress panel (Slint)

**Files:**
- Create: `crates/slint-ui/ui/fortress-panel.slint` (`FortressPanel`)
- Modify: `crates/slint-ui/ui/game.slint` (remove the F9 `panel := FrameFace {…}` block and `panel-open`, `home-region`, `connection` properties; instantiate `FortressPanel`)
- Test: `crates/slint-ui/tests/fortress_panel.rs` (new, add `mod fortress_panel;` to `tests/it.rs`)

**Interfaces:**
- Produces on `GameUi` (all `in` unless noted):
  - `fortress-value: string` (default `"0"`), `fortress-happiness: string` (`"0"`), `fortress-food: string` (`"0"`)
  - `fortress-tab: int` — `in-out`, default −1 (collapsed); 0..3 = Governance, People, Designations, Stocks; bound TWO-WAY to `Tabs.selected` (controls.slint binding rule)
  - `governance-lines: [string]`, `people-names: [string]`, `designation-lines: [string]`
  - `callback person-picked(int)` (index into `people-names`)
  - probes for tests: `out fortress-tab-x(i)`… — simplest: `out property <length> fortress-tabs-y` and `fortress-tab-pitch`, `fortress-first-tab-x` (centre of tab 0)
- Body per tab: Governance = one `LabelFace` per line; People / Designations = `PickList { items: …; max-visible: 12; }` (People's `picked(i)` → `person-picked(i)`; Designations' picks ignored); Stocks = `LabelFace { text: "No stocks yet"; }`.
- Header: one `HorizontalFlow` of three `LabelFace`s `"Fortress value " + root.value` etc., above the `Tabs { collapsible: true; }`, inside one `FrameFace` (no title), at x = 12px, y = 12px, drawn `if !root.map-modal`.

- [ ] **Step 1: failing tests** in `fortress_panel.rs` (1280×720 `Overlay<GameUi>`, `click` helper as in `game_panel.rs`; tag slow):
  - `header_shows_placeholders`: render; assert opaque at (24, 24) and the header text is reachable via `get_fortress_value() == "0"` etc.
  - `tabs_open_and_collapse`: click first tab → `get_fortress_tab() == 0`; click it again → `-1`; after collapse assert `!opaque_at(x, y)` for every x in 12..300 step 6 at y = collapsed bottom + 40.
  - `people_row_click_picks`: `set_fortress_tab(1)`, `set_people_names(["Urist", "Bomrek"])`, click the second row (`first-item-y + 1.5 * row-height` from the PickList probes forwarded as `people-first-item-y` / `people-row-height`) → `person-picked(1)` fired once.
  - `long_list_stays_on_screen`: 200 designation lines, tab 2 open → nothing opaque at y ≥ 700 for x in 12..400.
  - `hidden_on_the_arrival_map`: `set_map_modal(true)` → `!opaque_at(24, 24)`.
- [ ] **Step 2:** `just test-one slint-ui "--test it -- --include-ignored fortress_panel::"` → FAIL (properties missing).
- [ ] **Step 3:** write `FortressPanel` in `fortress-panel.slint` (import `Tabs`, `PickList` from `controls.slint`), wire it into `GameUi`, delete the F9 block and its three properties. Fix the callers of the removed `set_panel_open`: delete those lines in `tests/world_map.rs` (×4) and `tests/wasm_images.rs` (×1).
- [ ] **Step 4:** same command → PASS; `just test-one slint-ui "--test it -- --include-ignored world_map::"` → PASS.

### Task 3: Top-right menu column, context box, stub windows (Slint)

**Files:**
- Create: `crates/slint-ui/ui/menu-column.slint` (`MenuColumn`: button row + context box), `crates/slint-ui/ui/stub-window.slint` (`StubWindow`: centred `FrameFace` with `title`, `Nothing here yet`, Close button)
- Modify: `crates/slint-ui/ui/game.slint` (remove `adventure`-button, `first-person`, `toggle-mode`, `first-person-clicked` — keep `adventure` and `tools-available`, still read by the wheel; instantiate `MenuColumn` and two `StubWindow`s)
- Rewrite: `crates/slint-ui/tests/game_panel.rs` → tests the menu column

**Interfaces:**
- Produces on `GameUi`:
  - `in-out settings-open: bool`, `in-out encyclopedia-open: bool`
  - `callback open-settings()`, `callback open-encyclopedia()`, `callback close-window()` (either stub's Close)
  - `tool-name: string` (`""` = no tool) — context box tool section shows `tool-name`, `"Priority " + priority + "  [1–7]"`, `level-text`
  - existing `selected-count`, `selection-lines`, `following`, `clear-selection()`, `follow-clicked()` move into the box's selection section unchanged
- Layout: a `VerticalLayout` pinned at x = width − w − 12px, y = 12px: row `Button "Settings"`, `Button "Encyclopedia"`; under it `if root.tool-name != "" || root.selected-count > 0: FrameFace {…}` (tool section first, then selection section). Column width `max(240px, preferred)`.
- Stub windows draw over the panels (declared after them, before `menu :=`), `if root.settings-open` / `if root.encyclopedia-open`.

- [ ] **Step 1: failing tests** (rewrite `game_panel.rs`, slow-tagged, scanning the top-right 400×400 like today):
  - `menu_buttons_open_windows`: scan clicks → `open-settings` and `open-encyclopedia` each fired ≥ 1; nothing opaque in the last 4 px columns (keep the old `MARGIN` check).
  - `context_box_absent_when_idle`: `selected-count 0`, `tool-name ""` → `!opaque_at` everywhere in x ∈ [W−400, W), y ∈ [80, 400).
  - `selection_controls_reachable`: `selected-count 1`, `following` false then true → `clear-selection` and `follow-clicked` each fired ≥ 1.
  - `stub_window_closes`: `set_settings_open(true)`, click its Close (probe `stub-close-x/y`) → `close-window` fired.
- [ ] **Step 2:** `just test-one slint-ui "--test it -- --include-ignored game_panel::"` → FAIL.
- [ ] **Step 3:** implement `MenuColumn`, `StubWindow`, wire into `GameUi`, delete the removed properties/callbacks.
- [ ] **Step 4:** same command → PASS; then `just test-one slint-ui "--test it -- --include-ignored"` (whole slint-ui `it`, incl. `fortress_wheel::`, `hud::`, `dev_panel::`) → PASS.

### Task 4: Dev panel out of the right column

**Files:**
- Modify: `crates/slint-ui/ui/dev-panel.slint:50` (x)
- Test: `crates/slint-ui/tests/dev_panel.rs` (existing probes keep working)

- [ ] **Step 1:** add `out property <length> panel-right` probe; test `dev_panel_clear_of_menu_column`: at 1280 wide, `panel-right <= 1280 - 12 - 240` (the menu column's minimum width + margin).
- [ ] **Step 2:** run `just test-one slint-ui "--test it -- --include-ignored dev_panel::"` → FAIL.
- [ ] **Step 3:** `x: root.width - self.width - 8px - DevLayout.right-reserve;` with `global DevLayout { out property <length> right-reserve: 264px; }` (240 column + 12 margin + 12 gap).
- [ ] **Step 4:** same command → PASS.

### Task 5: Client — pure data + screens

**Files:**
- Create: `crates/client/src/renderer/fortress_panel.rs` (declare in `renderer/mod.rs`)
- Modify: `crates/client/src/renderer/slint_overlay.rs` (`Screens`, `UiAction`)

**Interfaces:**
- Produces in `fortress_panel.rs`:
  - `pub fn designation_lines(designations: &BTreeMap<TilePos, Designation>) -> Vec<String>` — one line per `OpId`, in `OpId` order, `"<Kind> · priority <p> · <n> tiles"` (`1 tile` singular); kind names `Dig`, `Deconstruct`, `Build`, `Chop` (Build ignores its block id).
  - `pub fn governance_lines(calendar: Option<CalendarTick>, fortress: Option<&SiteMarker>) -> Vec<String>` — `["Date <c.date()>", "Residents <population>"]`; no fortress → `["Date …", "No fortress"]` with the date line only when `calendar` is `Some`.
  - `pub fn person_name(person: PersonId, info: Option<&PersonInfo>) -> String` — info name, else `"Person #<id>"`.
  - `pub const PERSON_NAME_REQUESTS_PER_SEC: u32 = 4;`
- Produces in `slint_overlay.rs`: `Screens { menu, map, wheel, settings, encyclopedia }`; `escape()` order: wheel → settings/encyclopedia → map → menu toggle; `toggle_map` and `open_map` clear both new flags; new `Screens::open_settings()` / `open_encyclopedia()` (exclusive, like `open_map`). `UiAction` gains `OpenSettings`, `OpenEncyclopedia`, `CloseWindow`, `PickPerson(i32)`; loses `ToggleMode`, `ToggleFirstPerson`.

- [ ] **Step 1: failing unit tests** in `fortress_panel.rs`: `designation_lines_group_by_order` (two ops, 3 + 1 tiles → `["Dig · priority 4 · 3 tiles", "Chop · priority 2 · 1 tile"]`), `governance_without_fortress`, `governance_with_fortress` (population 7 → `"Residents 7"`), `person_name_falls_back`; in `slint_overlay.rs` tests: `escape_closes_settings_before_menu`, `map_key_closes_encyclopedia`.
- [ ] **Step 2:** `just smoke` → FAIL to type-check (functions missing).
- [ ] **Step 3:** implement the functions and the `Screens` changes; `Screens::of` reads the two new UI flags and `set_screens` writes them.
- [ ] **Step 4:** `just smoke` → PASS. (Running them: `just test-one client "--lib fortress_panel::"` and `"--lib slint_overlay::"` — only with Dawid's OK.)

### Task 6: Client — wiring, F9 removal

**Files:**
- Modify: `crates/client/src/renderer/slint_overlay.rs` (`connect_game_ui`, `apply_ui_actions`, `push_ui_state`, `sync_toolbar`, plugin systems), `crates/client/tests/session_reset.rs` (only if it reads a removed property)

**Interfaces:**
- Consumes Task 5's functions and `Screens`; Task 2/3 properties.

- [ ] **Step 1:** remove `SLINT_PANEL_KEY`, `toggle_panel` (and its registration), `ToggleModeRequest`/`ToggleFirstPersonRequest` writers and the tuple parameter, the `home_region`/`connection` pushes, the `first-person` sync; `just smoke` must still pass after this step.
- [ ] **Step 2:** connect `open-settings` → `OpenSettings`, `open-encyclopedia` → `OpenEncyclopedia`, `close-window` → `CloseWindow`, `person-picked(i)` → `PickPerson(i)`; handle them in `apply_ui_actions` (`to = Screens::open_settings()` etc.; `CloseWindow` clears both flags; `PickPerson(i)` replaces the `Selection` with the i-th row's `(region, key)` — keep the rows' `(RegionId, EntityKey, PersonId)` in a `SessionLocal<Vec<…>>` filled by Step 3).
- [ ] **Step 3:** in `push_ui_state`, each only on change: `tool-name` from `DesignateState::tool.map(Tool::label)`; `governance-lines` when `HistoryView` changed; `designation-lines` when `TileMirror` changed; `people-names` from the `(SimEntity, SimKind)` query filtered to `EntityKind::Person` in `HudStatus::home_region`, sorted by `PersonId`, compared against the last pushed list. While `get_fortress_tab() == 1`, send `ClientRequest::RequestPersonInfo` for rows with no cached info (or `person_refresh_due`), at most `PERSON_NAME_REQUESTS_PER_SEC` per second (a `SessionLocal<(f32, u32)>` window counter).
- [ ] **Step 4:** `just smoke` → PASS; `just test-one slint-ui "--test it -- --include-ignored"` → PASS.

### Task 7: Docs

**Files:**
- Modify: `CLAUDE.md` (every F9 / "Fortress panel" mention: the designation tools' "level and priority readouts in the F9 Fortress panel", the slint-ui "F9 toggles the panel", the adventure "panel's mode button" / "First person button"), `crates/client/src/renderer/commander/adventure.rs` and `first_person.rs` module docs that name the panel buttons.

- [ ] **Step 1:** replace those mentions with the new panels (one sentence in the `crates/slint-ui` paragraph describing the fortress panel, menu column and stubs; note Adventure has no UI entry for now).
- [ ] **Step 2:** `grep -rn "F9" CLAUDE.md crates/client/src crates/slint-ui/ui` → no hit referring to the panel.
