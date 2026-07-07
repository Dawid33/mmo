# In-Engine Deterministic UI — Design

**Status:** Design approved, pending implementation plan
**Date:** 2026-07-07
**Related:** `2026-07-03-undo-api-redesign-design.md` (rollback/emit machinery this reuses), `2026-07-05-multi-region-world` (region/client model)

## Problem

In-game UI (inventory, hotbar, menus, HUD) depends on in-game data and must be
driven from inside the engine so that what the player sees is a faithful,
deterministic function of authoritative simulation state — not a separate,
drifting client-side model. The engine crates (`game`, `server`) must remain
Bevy-free and windowing-free; only the `client` crate uses Bevy 0.18.

## Core decision: engine owns structure, client owns layout/paint

The engine owns the UI's **structure and logic** — a widget vocabulary and a
pure builder function, both engine-neutral data. The client owns **layout and
paint** — it maps engine widgets to `bevy::ui` nodes and lets Bevy UI's
built-in taffy compute layout against the client's real viewport.

This mirrors how world entities already render: the sim owns the model, a
one-way stream of `GameDataUpdate` deltas crosses into the Bevy bridge, and the
bridge maps them to Bevy entities/components (`crates/client/src/renderer/bridge.rs`).
Widgets reuse that exact client-facing path.

### Why layout is NOT computed in the engine

Rollback determinism exists to make server and client reconcile bit-exact
(`hash(before) == hash(after undo)`). That bar applies to authoritative
simulation state that must reconcile across the wire. **Pixel layout is
neither.** Layout is a pure function of `layout(data, viewport, fonts, dpi)`,
and viewport/fonts/dpi are client-local facts the server neither has nor should
have. Computing layout in the sim would force those client facts into synced
rollback state — backwards. So the engine describes *layout intent* (flex
direction, sizes, gaps as engine-neutral style fields) and Bevy UI's taffy — a
dependency already present via `bevy_ui` — executes it on the client.

Running taffy *inside* `game` is reserved for the deferred diegetic-panel case
(§ Deferred), where UI geometry is shared, world-space, and server-hit-tested;
that case additionally needs deterministic text measurement (a vendored
font + shaper), which is the genuinely hard part and out of scope here.

### Why the widget tree is a client-side projection, not rollback state

The widget tree is a **client-side deterministic projection** of authoritative
data, not authoritative state itself.

Alternative considered — widgets as authoritative rollback state in `GameData`
with `#[undo(slotmap)]`/`#[emit]` — is maximally uniform with the entity
pattern but has a disqualifying cost: anything in hashed/serialized rollback
state must be computed identically on server and client or snapshots won't
match. That forces the headless server to build and roll back render-only
widget trees for every player, bloats region snapshots, and imposes
exact-invertibility on every UI edit. We would pay determinism costs for pixels
the server never draws.

The projection avoids all of it: the server does zero UI work, snapshots are
unchanged, and there is no invertibility burden. It remains "deterministic,
driven from inside the engine" in the sense that matters — the structure and
logic are engine-owned shared code and a pure function of deterministic data.

## Architecture

### Data flow (end to end)

```
authoritative inventory (GameData rollback state)
   │  synced to client via EXISTING snapshot + prediction path (no new wire protocol for data)
   ▼
client sim thread, once per settled tick:
   build_player_ui(data, local_player) -> WidgetTree   (pure, in `game`)
   diff_ui(prev_tree, new_tree) -> Vec<UiUpdate>
   │  pushed into a DEDICATED UI update channel (not the per-region
   │  GameDataUpdate channel: widgets hang off a screen-space UI root)
   ▼
ui_bridge (Bevy): WidgetId -> Entity map, dispatch deltas -> bevy::ui::Node tree
   │  Bevy UI's taffy lays it out against the real viewport
   ▼
rendered UI on the local player's IsDefaultUiCamera

interaction (reverse):
   bevy_picking click -> Entity -> WidgetId -> UiAction
   -> GameEventKind::UiAction(ClientId, UiAction)  (routed like PlayerInput)
   -> sim mutates inventory authoritatively -> projection re-derives -> UI updates
```

The projection runs **client-side only**, invoked from the client `Instance`
after each settled tick over the local player's data. The server never calls
it.

### Rollback interaction

No widget-specific compensation is needed. When the sim rolls back, inventory
data reverts through the normal machinery; the next post-settle projection
re-derives the tree from the reverted data and diffs against the last-emitted
tree, emitting corrective deltas. The bridge is idempotent / last-write-wins —
the same invariant that makes `SimTarget` safe across rollback bursts
(`bridge.rs` `SimTarget` doc comment). The diff always compares
current-desired against last-emitted (what the bridge currently shows), so it is
correct regardless of rollback.

## Components

Each is an independently testable unit with one responsibility.

### `crates/game/src/ui.rs` (new) — widget vocabulary + diff

Engine-neutral, Bevy-free. Because the widget tree is a client-side projection
(never hashed, never serialized over the wire), style floats are plain `f32` —
no `OrderedFloat`/`Hash` determinism burden.

Widget identity is a **deterministic, stable `WidgetId`** assigned by
`build_player_ui` from each widget's logical role (root is always `WidgetId(0)`),
NOT a freshly-allocated slotmap key — so `diff_ui` can match nodes across
rebuilds.

```rust
struct WidgetId(u64)

struct WidgetTree { nodes: BTreeMap<WidgetId, Widget>, root: WidgetId }

struct Widget {
    style: Style,
    content: Content,
    action: Option<UiAction>,   // Some => clickable, sends this on click
    children: Vec<WidgetId>,    // ordered
}

enum Content { None, Text(String), Icon(IconId) }

struct Style {
    // flex layout intent, mapped 1:1 to bevy::ui::Node in the client. v1 keeps
    // the field set minimal; extend as later screens need it (position_type,
    // margin, etc.).
    direction: FlexDirection,
    width: Val, height: Val,
    gap: Val,
    padding: Rect,
    justify: JustifyContent,
    align: AlignItems,
    background: Option<Color>,
}
```

- `Val` — engine-neutral `Px(f32) | Percent(f32) | Auto` (mirrors `bevy::ui::Val`).
- `IconId` — engine-neutral handle the client resolves to a texture/atlas entry
  (for inventory, derived from `ItemKind`).
- `UiAction` — logical-space click payload (see interaction).
- **Diff:** `fn diff_ui(prev: &WidgetTree, next: &WidgetTree) -> Vec<UiUpdate>`
  producing the minimal set of widget deltas, ordered create → property-set →
  child-wire → remove so the bridge never references an unmapped id. Pure and
  unit-tested.

### `crates/game/src/ui.rs` — `build_player_ui`

`fn build_player_ui(data: &GameData, local_player: ClientId) -> WidgetTree`.
Pure projection from authoritative data to a widget tree. Grows one arm per
screen; v1 builds the inventory/hotbar grid. Unit-tested (data in → expected
tree out).

### `crates/game` — minimal inventory model (prerequisite)

Genuine authoritative sim state, added as a field on the existing tracked
`Client` struct: `inventory: Inventory` (and `inventory_open: bool`), where
`Inventory` is a fixed set of slots of `Option<ItemStack>` and `ItemStack { kind:
ItemKind, count: u32 }`. Attaching to `Client` (rather than a new `GameData` map)
means the inventory **follows the player across region handoffs** for free —
`Client` already rides `EntityBundle`. `Client` is an `#[undo(map)]` value, so an
edit via `clients.get_mut` logs the whole prior `Client` and inverts bit-exact
(the same mechanism the fps-cam toggle already uses). Derives mirror `Client`'s
existing non-Partial field `InputState`; no `borrow::Partial` needed on the new
types. Syncs to the client through the **existing** snapshot + prediction path —
no new wire protocol for the data. Scope kept minimal: 9 hotbar slots, one
mutation (move/swap), seeded with test items on player join. Not a full item
system.

`UiAction` handling lands in `crates/game/src/region.rs` alongside the existing
`handle_event` arms (routed like `PlayerInput`, by the manager's `homes` map).

### `UiUpdate` enum + `UiAction` protocol event

New `UiUpdate` enum in `crates/game/src/ui.rs` (in-process only, `Clone + Debug`,
no serde — it never crosses the wire): `CreateWidget`, `SetStyle`, `SetContent`,
`SetAction`, `SetChildren`, `RemoveWidget`, `SetCursorGrab`.

New engine-neutral, serializable protocol event
`GameEventKind::UiAction(ClientId, UiAction)` in `crates/game/src/protocol.rs`,
routed through the same path as `PlayerInput`.

### `crates/client/src/renderer/ui_bridge.rs` (new) — deltas → bevy_ui

Mirrors `bridge.rs`. Holds a `WidgetId -> Entity` map (like `SimEntityMap`) and a
screen-space UI root `Node`. Drains the `UiUpdate` variants from the dedicated UI
channel and maps each to spawning/despawning/restyling a `bevy::ui::Node`:
`Content::Text -> Text`, `Content::Icon -> ImageNode` (icon-texture resolution is
a follow-up; v1 tints the slot), `Style -> Node` fields via a `style_to_node`
helper. Bevy's taffy lays it out. Unmapped-id updates `warn!` and continue (same
robustness rule as `drain_region_updates`).

### `crates/client/src/renderer/ui_input.rs` (new) — picking → action

Requires enabling a `bevy_picking` UI backend (currently **off** —
prerequisite). A click on a widget entity maps `Entity -> WidgetId ->
UiAction`, then sends `GameEventKind::UiAction` down the existing `GameEvents`
channel (predicted locally + sent to server, like `forward_input`). Actions are
logical (`UiAction::MoveItem { from: SlotId, to: SlotId }`), never pixels; drag
gestures are tracked client-side and emit the final logical action.

### client `Instance` — post-tick projection hook

After each settled tick, the client `Instance`
(`crates/client/src/instance.rs`) invokes `build_player_ui` +
`diff` for the local player and pushes the resulting deltas into the region's
`GameDataUpdate` sender. Client-only; the server's instance never calls it.

### Cursor coexistence

"Inventory open" is a sim flag (like `Client::fps_cam_mode`). Toggling it
releases the cursor grab through the **existing** `SetFreeCam`-style mechanism
in `bridge.rs` (cursor grab/visibility is already a function of sim state):
open-inventory = free cursor, closed = grabbed. No new cursor pathway.

## Staging

1. **Render-only vertical slice.** Minimal inventory model seeded with test
   items; `build_player_ui` + diff; widget `GameDataUpdate` variants;
   `ui_bridge` rendering a static-but-data-bound hotbar/inventory grid; post-tick
   projection hook. Proves model → projection → bridge → bevy_ui end to end with
   no interaction.
2. **Interaction.** Enable `bevy_picking` UI backend; `ui_input` picking → 
   `UiAction`; `UiAction` handling mutating inventory; inventory-open flag +
   cursor release. Proves the full click → logical action → sim → projection →
   UI round-trip.

## Testing

- **`game` (headless, deterministic):** unit-test `build_player_ui`
  (inventory data → expected tree), the tree `diff` (tree A→B → minimal correct
  delta set), and `UiAction` → inventory mutation.
- **`client` (headless, `MinimalPlugins`/`AssetPlugin`, like the existing 26
  tests):** feed widget `GameDataUpdate`s → assert the `Node`/`Text`/`ImageNode`
  hierarchy; simulate a pick → assert the right `UiAction` is emitted. Reuse
  `crates/client/src/bevy_harness.rs`.
- **Round-trip (sim level, no window):** click → `UiAction` → inventory change →
  projection → updated tree.

## Deferred (YAGNI)

- **Diegetic world-space, server-hit-tested panels** — the only case that
  genuinely needs real in-engine taffy plus deterministic text measurement
  (vendored font + shaper at a fixed reference resolution). Hit-testing there is
  still resolved in logical/world coordinates, not pixels.
- Scrolling, text input fields, animations/transitions.
- A full item/icon system (v1 uses a minimal seeded inventory).
- Multiple simultaneous windows / window manager.

## Justfile / docs

No new build/run commands are introduced; tests run under the existing
`just test` (`cargo test -p game`) and `just test-client` (`cargo test -p
client`). Update `TODO.md` to point the in-engine-UI line at this spec.
