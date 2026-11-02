# Fortress HUD panels (EU5-style) — design

Date 2026-11-02. Status: draft for Dawid's review.

## Intent

Give fortress mode a fixed, EU5-like frame: management on the top left, game menus and
"what am I doing right now" on the top right. This replaces the F9 "Fortress" panel, which mixes
camera/mode controls, selection details and dev information in one box.

The panels are the home for *management* UI, not for all UI. Other windows (world map, Esc menu,
tool wheel, Settings, Encyclopedia, later dialogs) stay separate modals.

Background: `docs/superpowers/specs/2026-11-02-intuitive-ui-research/00-synthesis.md`, which covers
always-visible key data and a context box for the current selection or action.

## Goals

1. **Top-left fortress panel**:
   - **Tab row**: Governance, People, Designations, Stocks, with the three key fortress
     figures on the same row to the right of the last tab (Dawid, after the first look).
   - **Tab body**: the content of the open tab.
2. **Top-right menu column**:
   - A row of menu buttons: Settings and Encyclopedia.
   - Under the buttons, an **ephemeral context box** (Factorio-style) showing details of the
     current selection or current action. It is absent when there is neither.
3. **Remove the F9 panel** and its key binding. Everything on it gets a new home (table below).

## Non-goals

- Real figures for value, happiness or food. They are placeholders reading 0 until those
  systems exist.
- Fleshed-out People / Designations content. Dawid expects these to be redone; they are plain
  lists for now (below).
- Real Settings or Encyclopedia content. Both buttons open empty stub windows.
- Sorting, search, filters, click-to-jump on list rows. They are wanted later (research
  principle 4/8), but not in this version.

## Anti-goals (would make it a failure even if it works)

- Losing a control the F9 panel had, other than the two Dawid chose to drop (below). Follow,
  Clear selection and the selected dwarf's details must stay reachable.
- The panels covering each other or the minimap, or taking clicks/pointer where nothing is drawn.
  The existing `SlintPointer` / `CursorPolicy::ui_pointer` contract must hold.
- Showing during the embark step. Today's `map-modal` hides the F9 panel and status line; the
  new panels follow the same rule.

## Layout

```
┌──────────────────────────────┐                                 ┌──────────────────────┐
│ Value 0 · Happiness 0 · Food 0│        (status line, centre)    │ [Settings] [Encyclo.]│
├──────────────────────────────┤                                 ├──────────────────────┤
│[Governance][People][Desig.][Stocks]                            │ context box          │
├──────────────────────────────┤                                 │ (only when something │
│ open tab's body              │                                 │  is selected or a    │
│                              │                                 │  tool is active)     │
└──────────────────────────────┘                                 └──────────────────────┘

 minimap (bottom-left, unchanged)
```

- Both panels sit flush in their corner (no margin — Dawid, after the first look); the open
  fortress panel stops 12 px short of the window's bottom.
- The top-centre status line stays where it is. The panels must leave it room at small window
  widths; if they cannot, the status line wins.

## Top-left fortress panel

### Header (decided)

| label | value now | later source |
|---|---|---|
| Fortress value | `0` | wealth system |
| Average happiness | `0` | needs/mood system |
| Food stocks | `0` | stocks system |

Placeholders show a literal `0`, not a dash, as Dawid asked.

### Tabs (decided: order and names)

1. **Governance**: an overview of current information.
   - **Date**: from `HistoryView::calendar`, formatted as today's F3 `Date` row.
   - **Residents**: the population of the player's fortress site (`HistoryView::fortress` → its
     `SiteMarker::population`).
   - More rows will be added in future.
2. **People**: a plain list of the people in the home region, one row each with the name.
   *(My call: every `EntityKind::Person` in the home region, not filtered to fortress members —
   filtering needs each person's `PersonInfo.site` or a new roster packet, not worth it for a list
   that will be redone. Client-only, no protocol change.)* Names come from the `PersonInfo` cache
   (`HistoryView::people`, `ClientRequest::RequestPersonInfo`), requested only while the People
   tab is open, at most a few new requests per second, reusing the inspector's refresh interval;
   rows show "Person #id" until the answer arrives. Clicking a row selects
   that dwarf, so the context box shows it. *(Builder's call — remove it if it is costly.)*
3. **Designations**: a plain list of the current orders, one row per order (`OpId`): kind,
   priority, tile count. Source: `TileMirror::designations()`, grouped by `Designation::op`.
4. **Stocks**: an empty placeholder ("No stocks yet").

### Tab behaviour (my call, not discussed)

- The header and tab row are always visible in fortress mode.
- Clicking a tab opens its body. Clicking the open tab again collapses the body, leaving the
  header and tab row. The panel starts collapsed.
- There are no tab hotkeys for now. F9 is freed, not reassigned.

## Top-right menu column

### Menu buttons (decided)

- **Settings**: opens a Settings window, a stub ("Nothing here yet").
- **Encyclopedia**: opens an Encyclopedia window, also a stub.

These are modal windows like the world map; Esc closes them. *(Modal vs non-blocking: my call;
the stubs make the difference moot for now.)*

### Context box (decided: purpose and position)

It sits directly under the menu buttons. It shows details of the current selection or action,
and takes no pixels when there is neither.

Content, my call, mapping what the F9 panel showed:

| situation | the box shows |
|---|---|
| a designation tool is active | tool name, `Priority N [1–7]`, the working-level readout (today's `level-text`) |
| one dwarf selected | today's `person_lines` inspector, Follow / Clear selection |
| several entities selected | today's `selection_lines` counts, Clear selection |
| nothing selected, no tool | not drawn |

If a tool is active *and* something is selected, the tool section comes first and the selection
below it. *(My call.)*

**Dropped (Dawid's decision):** the Fortress/Adventure mode button is removed entirely, with no
new home. Adventure mode itself (sim `Order::SetMode`, client `adventure.rs`, `first_person.rs`)
stays in the code but has no UI entry point until one is designed. The First person [F5] button
goes with it, since it only works in Adventure; the F5 key binding is unchanged.

## Where the F9 panel's contents go

| F9 item | new home |
|---|---|
| Region line | F3 dev panel |
| Connection line | F3 dev panel |
| Working-level text | context box, while a tool is active |
| Priority readout | context box, while a tool is active |
| Fortress/Adventure mode button | removed (Dawid) — Adventure has no UI entry for now |
| First person [F5] button | removed with it; F5 key unchanged |
| Selection count + lines | context box |
| Clear selection, Follow | context box |

## Other existing UI affected

- **F3 dev panel**: today pinned top-right (`dev-panel.slint`, x = width − w − 8), where the new
  column goes. It moves so the two don't overlap. *(My call: left of the right column.)*
- **Status line**: unchanged. It is hidden under the open map, as now.
- **Esc menu, world map, tool wheel, drag readout**: unchanged. While the map or Esc menu is open,
  the two panels stay drawn but under the modal (same layer order as today's F9 panel).
- **First person**: the commander-only rule for the game UI stays; the panels draw only in
  commander mode.

## When the panels show (my call, after the builder check)

- Both panels draw whenever the commander camera is on: Fortress and Adventure mode alike, not in
  first person, not during the embark step (`map-modal`).
- With no fortress held (dev Play, a procedural world, right after Retire), the header still
  reads 0s and Governance shows "No fortress" in place of the date / residents rows that need a
  site. The date is shown whenever `calendar` is known.

## Constraints

- **Slint, fortress theme, existing widgets library** (`FrameFace`, `Button`, `LabelFace`, flows).
  The panels go in `crates/slint-ui/ui/game.slint` or new files under `crates/slint-ui/ui/`
  compiled into `slint-ui-game`. A new root component must call `GameTheme.apply()`; widgets must
  not bind `visible`/`opacity` on a component root (codegen-size rule in CLAUDE.md).
- **Client wiring** in `renderer/slint_overlay.rs`: push properties only on change; turn clicks
  into `UiAction`s; respect `SlintPointer` ownership. The F9 key (`SLINT_PANEL_KEY`) goes.
- **Session reset**: any new resource or `Local` (open tab, Settings/Encyclopedia open flags) is
  registered with `add_session_reset` / `SessionLocal` so it does not leak into the next world.
- **UI scale**: works at `VOXEL_UI_SCALE` 1 and 2.
- **Tests**: the F9 panel's headless tests (`crates/slint-ui/tests/game_panel.rs`) are rewritten
  for the new panels (tab click opens/collapses, context box present only when there is
  something, menu buttons open the stubs). The pure helpers (designation grouping, header text)
  get unit tests. Verify with `just test-one slint-ui "--test it game_panel::"`,
  `just smoke`, and the client lib filters touched (ask before running client unit tests, per
  CLAUDE.md).

## Left to the builder

- Exact widths, paddings, tab widget choice (the widgets library's tab component if it has one,
  else buttons styled as tabs).
- Row formatting in the lists, and list height limit or scrolling (cap the body height and scroll
  if the widgets support it; otherwise cap row count with "… N more").
- Property/model shape between Rust and Slint.

## Open questions for Dawid

None left. Builder-check questions were settled as marked: mode switch removed (Dawid); panel
visibility, People source, Designations owners (my calls).
