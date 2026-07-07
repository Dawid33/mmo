# In-Engine Deterministic UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Drive in-game UI (first target: inventory/hotbar) from engine-owned data, rendered with Bevy UI, reusing the existing sim→bridge→render pipeline.

**Architecture:** The `game` crate owns a widget vocabulary and a pure `build_player_ui` builder plus a `diff_ui` function. On the client only, after each settled tick, the instance projects the local player's authoritative data into a `WidgetTree`, diffs it against the previous tree, and pushes `UiUpdate` deltas down a dedicated crossbeam channel. A new `ui_bridge` Bevy system maps those deltas to `bevy::ui::Node` widgets under a UI root; Bevy UI's built-in taffy lays them out. Interaction (slice 2) is logical-space: a pick maps a widget to a `UiAction`, routed to the sim like `PlayerInput`.

**Tech Stack:** Rust (mixed editions), Bevy 0.18 (`bevy_ui`/`bevy_text`/`bevy_picking`), crossbeam channels, the `#[rollback]` macro machinery in `crates/macros`.

## Global Constraints

- `crates/game` and `crates/server` MUST stay Bevy-free and windowing-free. Widget/UI types live in `game` as plain data; all `bevy::ui` mapping lives in `crates/client`.
- The widget tree is a **client-side projection**, never part of hashed/serialized rollback state and never sent over the wire. Only inventory *data* (on `Client`) is authoritative sim state, synced through the existing snapshot/prediction path.
- Widget identity MUST be a deterministic, stable `WidgetId` (not a freshly-allocated slotmap key), so `diff_ui` can match nodes across rebuilds.
- `UiUpdate` consumption in the bridge MUST be idempotent / last-write-wins and MUST tolerate an unmapped `WidgetId` with a `warn!` + continue (same robustness rule as `drain_region_updates` in `crates/client/src/renderer/bridge.rs`).
- Tests run under existing recipes: `cargo test -p game` (`just test`) and `cargo test -p client` (`just test-client`). No new justfile recipes.
- Bevy API drift is a known hazard — verify each `bevy::ui` type/method against 0.18 as you go (`Node`, `Text`, `ImageNode`, `ChildOf`, `add_children`).

---

## File Structure

- `crates/game/src/ui.rs` (new) — `WidgetId`, `Style`, `Val`, `Content`, `Widget`, `WidgetTree`, `UiUpdate`, `UiAction`, and `diff_ui`. Pure, engine-neutral. Also `build_player_ui`.
- `crates/game/src/state.rs` (modify) — add `Inventory`/`ItemStack`/`ItemKind`; add `inventory: Inventory` field to `Client`; seed items; add `UiAction` application (slice 2).
- `crates/game/src/lib.rs` (modify) — `pub mod ui; pub use ui::*;` and a new `ClientUpdateEvent::UiChannel(...)` variant.
- `crates/game/src/region.rs` (modify, slice 2) — handle `GameEventKind::UiAction`.
- `crates/game/src/protocol.rs` (modify, slice 2) — `GameEventKind::UiAction(ClientId, UiAction)`.
- `crates/client/src/instance.rs` (modify) — create the UI channel, run the post-tick projection (`project_ui`).
- `crates/client/src/renderer/ui_bridge.rs` (new) — `UiWidgetMap`, `UiRoot`, `drain_ui_updates`, `style_to_node`, cursor grab from `UiUpdate::SetCursorGrab`.
- `crates/client/src/renderer/ui_input.rs` (new, slice 2) — pick → `UiAction` → `GameEvents`.
- `crates/client/src/renderer/mod.rs` (modify) — register `ui_bridge`/`ui_input` resources and systems.
- `TODO.md` (modify) — point the UI line at the spec.

---

# SLICE 1 — Render-only vertical slice

## Task 1: Widget vocabulary types (`game/src/ui.rs`)

**Files:**
- Create: `crates/game/src/ui.rs`
- Modify: `crates/game/src/lib.rs` (add `pub mod ui;` and `pub use ui::*;`)
- Test: inline `#[cfg(test)]` in `crates/game/src/ui.rs`

**Interfaces:**
- Produces: `WidgetId(pub u64)`; `Val { Auto, Px(f32), Percent(f32) }`; `FlexDirection { Row, Column }`; `JustifyContent { Start, Center, End, SpaceBetween }`; `AlignItems { Start, Center, End, Stretch }`; `Color { r,g,b,a: f32 }`; `Rect { left, right, top, bottom: Val }`; `Style { direction, width, height, gap, padding, justify, align, background: Option<Color> }`; `Content { None, Text(String), Icon(IconId) }`; `IconId(pub u32)`; `Widget { style: Style, content: Content, action: Option<UiAction>, children: Vec<WidgetId> }`; `WidgetTree { nodes: BTreeMap<WidgetId, Widget>, root: WidgetId }`; `UiAction` (empty enum body for slice 1, one variant added in slice 2). `WidgetTree::empty() -> WidgetTree`.

- [ ] **Step 1: Write the failing test**

Add to `crates/game/src/ui.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tree_has_only_root() {
        let t = WidgetTree::empty();
        assert_eq!(t.root, WidgetId(0));
        assert_eq!(t.nodes.len(), 1);
        assert!(t.nodes.contains_key(&WidgetId(0)));
        assert!(t.nodes[&WidgetId(0)].children.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game ui::tests::empty_tree_has_only_root`
Expected: FAIL to compile ("cannot find ... ui", "WidgetTree not found").

- [ ] **Step 3: Write minimal implementation**

Create `crates/game/src/ui.rs` (above the test module):

```rust
//! Engine-owned UI: a widget vocabulary plus a pure projection of authoritative
//! data into a widget tree, and a diff producing `UiUpdate` deltas. This is a
//! CLIENT-SIDE projection — it is never hashed, serialized, or sent over the
//! wire. `game` stays Bevy-free: these are plain data types the client maps to
//! `bevy::ui`. See docs/superpowers/specs/2026-07-07-in-engine-ui-design.md.
use std::collections::BTreeMap;

/// Deterministic, stable widget identity. Assigned by `build_player_ui` from a
/// widget's logical role (NOT a slotmap key), so `diff_ui` can match nodes
/// across rebuilds. Root is always `WidgetId(0)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WidgetId(pub u64);

/// Icon handle the client resolves to a texture. For inventory, derived from
/// `ItemKind` (see `IconId::of`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconId(pub u32);

/// Layout length, mirrors `bevy::ui::Val`. Plain `f32`: the widget tree is a
/// client-side projection, so no `OrderedFloat`/determinism burden.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Val {
    Auto,
    Px(f32),
    Percent(f32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlexDirection {
    Row,
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JustifyContent {
    Start,
    Center,
    End,
    SpaceBetween,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlignItems {
    Start,
    Center,
    End,
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub left: Val,
    pub right: Val,
    pub top: Val,
    pub bottom: Val,
}

impl Rect {
    pub fn all(v: Val) -> Self {
        Self { left: v, right: v, top: v, bottom: v }
    }
}

/// Flexbox layout intent for one widget, mapped 1:1 to `bevy::ui::Node`.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub direction: FlexDirection,
    pub width: Val,
    pub height: Val,
    pub gap: Val,
    pub padding: Rect,
    pub justify: JustifyContent,
    pub align: AlignItems,
    pub background: Option<Color>,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            direction: FlexDirection::Row,
            width: Val::Auto,
            height: Val::Auto,
            gap: Val::Px(0.0),
            padding: Rect::all(Val::Px(0.0)),
            justify: JustifyContent::Start,
            align: AlignItems::Start,
            background: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    None,
    Text(String),
    Icon(IconId),
}

/// Logical-space click payload (slice 2 adds the first real variant). Kept as a
/// non-empty enum so `Option<UiAction>` fields and matches compile today.
#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    /// Placeholder so the type is inhabited before slice 2. Never emitted.
    Noop,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Widget {
    pub style: Style,
    pub content: Content,
    pub action: Option<UiAction>,
    pub children: Vec<WidgetId>,
}

impl Default for Widget {
    fn default() -> Self {
        Self {
            style: Style::default(),
            content: Content::None,
            action: None,
            children: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WidgetTree {
    pub nodes: BTreeMap<WidgetId, Widget>,
    pub root: WidgetId,
}

impl WidgetTree {
    /// A tree containing only an empty root widget (`WidgetId(0)`).
    pub fn empty() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(WidgetId(0), Widget::default());
        Self { nodes, root: WidgetId(0) }
    }
}
```

Add to `crates/game/src/lib.rs`, in the `pub mod` block (after `pub mod state;`):

```rust
pub mod ui;
```

and in the glob re-export block (after `pub use state::*;`):

```rust
pub use ui::*;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p game ui::tests::empty_tree_has_only_root`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/ui.rs crates/game/src/lib.rs
git commit -m "feat(ui): widget vocabulary types in game crate"
```

---

## Task 2: `UiUpdate` enum and `diff_ui`

**Files:**
- Modify: `crates/game/src/ui.rs`
- Test: inline `#[cfg(test)]` in `crates/game/src/ui.rs`

**Interfaces:**
- Consumes: `WidgetId`, `Widget`, `Style`, `Content`, `UiAction`, `WidgetTree` (Task 1).
- Produces: `enum UiUpdate { CreateWidget(WidgetId), SetStyle(WidgetId, Style), SetContent(WidgetId, Content), SetAction(WidgetId, Option<UiAction>), SetChildren(WidgetId, Vec<WidgetId>), RemoveWidget(WidgetId), SetCursorGrab(bool) }` and `pub fn diff_ui(prev: &WidgetTree, next: &WidgetTree) -> Vec<UiUpdate>`.
- Ordering contract (so the bridge never references an unmapped id): `diff_ui` emits, in order — (1) `CreateWidget` for every id in `next` not in `prev`; (2) `SetStyle`/`SetContent`/`SetAction` for created ids and for surviving ids whose value changed; (3) `SetChildren` for every id whose child list differs (or that was just created with children); (4) `RemoveWidget` for every id in `prev` not in `next`. `SetCursorGrab` is NOT produced by `diff_ui` (the projector emits it separately, Task 5).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/game/src/ui.rs`:

```rust
    fn leaf(text: &str) -> Widget {
        Widget { content: Content::Text(text.to_string()), ..Widget::default() }
    }

    #[test]
    fn diff_creates_new_subtree_before_wiring_children() {
        let prev = WidgetTree::empty();

        let mut next = WidgetTree::empty();
        next.nodes.get_mut(&WidgetId(0)).unwrap().children = vec![WidgetId(1)];
        next.nodes.insert(WidgetId(1), leaf("hi"));

        let ups = diff_ui(&prev, &next);

        // CreateWidget(1) must precede SetChildren(root, [1]).
        let create_pos = ups.iter().position(|u| matches!(u, UiUpdate::CreateWidget(WidgetId(1)))).unwrap();
        let wire_pos = ups.iter().position(|u| matches!(u, UiUpdate::SetChildren(WidgetId(0), c) if c == &vec![WidgetId(1)])).unwrap();
        assert!(create_pos < wire_pos, "create before wire: {ups:?}");
        assert!(ups.iter().any(|u| matches!(u, UiUpdate::SetContent(WidgetId(1), Content::Text(t)) if t == "hi")));
    }

    #[test]
    fn diff_removes_and_updates() {
        let mut prev = WidgetTree::empty();
        prev.nodes.get_mut(&WidgetId(0)).unwrap().children = vec![WidgetId(1), WidgetId(2)];
        prev.nodes.insert(WidgetId(1), leaf("a"));
        prev.nodes.insert(WidgetId(2), leaf("b"));

        let mut next = WidgetTree::empty();
        next.nodes.get_mut(&WidgetId(0)).unwrap().children = vec![WidgetId(1)];
        next.nodes.insert(WidgetId(1), leaf("A")); // content changed

        let ups = diff_ui(&prev, &next);
        assert!(ups.iter().any(|u| matches!(u, UiUpdate::SetContent(WidgetId(1), Content::Text(t)) if t == "A")));
        assert!(ups.iter().any(|u| matches!(u, UiUpdate::RemoveWidget(WidgetId(2)))));
        // Unchanged style must NOT be re-emitted for a surviving node.
        assert!(!ups.iter().any(|u| matches!(u, UiUpdate::SetStyle(WidgetId(1), _))));
    }

    #[test]
    fn diff_identical_trees_is_empty() {
        let mut a = WidgetTree::empty();
        a.nodes.get_mut(&WidgetId(0)).unwrap().children = vec![WidgetId(1)];
        a.nodes.insert(WidgetId(1), leaf("x"));
        assert!(diff_ui(&a, &a).is_empty());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game ui::tests::diff`
Expected: FAIL to compile ("cannot find function `diff_ui`", "no variant `CreateWidget`").

- [ ] **Step 3: Write minimal implementation**

Add to `crates/game/src/ui.rs` (before the tests module):

```rust
/// A single change to apply to the rendered widget tree. Consumed idempotently
/// by the client `ui_bridge`. `SetCursorGrab` rides this channel too (emitted
/// by the projector, not `diff_ui`).
#[derive(Debug, Clone, PartialEq)]
pub enum UiUpdate {
    CreateWidget(WidgetId),
    SetStyle(WidgetId, Style),
    SetContent(WidgetId, Content),
    SetAction(WidgetId, Option<UiAction>),
    SetChildren(WidgetId, Vec<WidgetId>),
    RemoveWidget(WidgetId),
    SetCursorGrab(bool),
}

/// Minimal delta set turning `prev` into `next`. See the ordering contract in
/// the plan: creates, then property sets, then child wiring, then removes.
pub fn diff_ui(prev: &WidgetTree, next: &WidgetTree) -> Vec<UiUpdate> {
    let mut out = Vec::new();

    // (1) creates
    for id in next.nodes.keys() {
        if !prev.nodes.contains_key(id) {
            out.push(UiUpdate::CreateWidget(*id));
        }
    }
    // (2) property sets for created + changed survivors
    for (id, w) in &next.nodes {
        let old = prev.nodes.get(id);
        let created = old.is_none();
        if created || old.unwrap().style != w.style {
            out.push(UiUpdate::SetStyle(*id, w.style.clone()));
        }
        if created || old.unwrap().content != w.content {
            out.push(UiUpdate::SetContent(*id, w.content.clone()));
        }
        if created || old.unwrap().action != w.action {
            out.push(UiUpdate::SetAction(*id, w.action.clone()));
        }
    }
    // (3) child wiring
    for (id, w) in &next.nodes {
        let old = prev.nodes.get(id);
        let created = old.is_none();
        if created || old.unwrap().children != w.children {
            out.push(UiUpdate::SetChildren(*id, w.children.clone()));
        }
    }
    // (4) removes
    for id in prev.nodes.keys() {
        if !next.nodes.contains_key(id) {
            out.push(UiUpdate::RemoveWidget(*id));
        }
    }
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p game ui::tests::diff`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/ui.rs
git commit -m "feat(ui): UiUpdate enum and diff_ui with create-before-wire ordering"
```

---

## Task 3: Inventory data model on `Client`

**Files:**
- Modify: `crates/game/src/state.rs`
- Test: inline `#[cfg(test)]` in `crates/game/src/state.rs`

**Interfaces:**
- Produces: `ItemKind { Empty, Dirt, Stone }` (derives `Default=Empty`); `ItemStack { kind: ItemKind, count: u32 }`; `Inventory { slots: Vec<Option<ItemStack>> }` with `Inventory::hotbar() -> Inventory` (9 empty slots) and `Inventory::seeded() -> Inventory` (9 slots, a couple filled); new field `pub inventory: Inventory` on `Client`. Derive set on all three: `#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize, Hash, PartialEq)]` — mirrors `Client`'s existing non-Partial field `InputState`, so no `borrow::Partial` derive is needed (Client derives Partial but its fields do not).

- [ ] **Step 1: Write the failing test**

Add to `crates/game/src/state.rs` (a new `#[cfg(test)] mod inventory_tests`):

```rust
#[cfg(test)]
mod inventory_tests {
    use super::*;

    #[test]
    fn seeded_inventory_has_items_in_a_fixed_hotbar() {
        let inv = Inventory::seeded();
        assert_eq!(inv.slots.len(), 9);
        assert!(inv.slots.iter().filter(|s| s.is_some()).count() >= 2);
        // Client carries an inventory and it round-trips by value.
        let c = Client { inventory: inv.clone(), ..Client::default() };
        assert_eq!(c.inventory, inv);
    }

    #[test]
    fn inventory_mutation_rolls_back_bit_exact() {
        // Client is a tracked UndoMap value: get_mut logs the whole prior
        // Client, so an inventory edit is covered by one delta and inverts.
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        rb.clients.insert(0, Client { inventory: Inventory::seeded(), ..Client::default() });
        rb.forget();
        let before = crate::state_hash(&rb);

        rb.new_transaction();
        {
            let c = rb.clients.get_mut(&0).unwrap();
            c.inventory.slots[0] = Some(ItemStack { kind: ItemKind::Stone, count: 5 });
        }
        rb.rollback();
        assert_eq!(before, crate::state_hash(&rb), "inventory edit must invert bit-exact");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game inventory_tests`
Expected: FAIL to compile ("no field `inventory` on `Client`", "cannot find `Inventory`").

- [ ] **Step 3: Write minimal implementation**

In `crates/game/src/state.rs`, add above the `Client` struct:

```rust
/// What an inventory slot holds. `Empty` is the default so `ItemStack` and
/// `Inventory` derive `Default` cleanly.
#[derive(Debug, Default, Clone, Copy, serde::Serialize, serde::Deserialize, Hash, PartialEq, Eq)]
pub enum ItemKind {
    #[default]
    Empty,
    Dirt,
    Stone,
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize, serde::Deserialize, Hash, PartialEq)]
pub struct ItemStack {
    pub kind: ItemKind,
    pub count: u32,
}

/// A fixed-size slot list. Authoritative sim state, carried on `Client` so it
/// follows the player across region handoffs (Client rides `EntityBundle`).
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize, Hash, PartialEq)]
pub struct Inventory {
    pub slots: Vec<Option<ItemStack>>,
}

impl Inventory {
    /// Nine empty hotbar slots.
    pub fn hotbar() -> Self {
        Self { slots: vec![None; 9] }
    }

    /// Nine slots seeded with test items (slice-1 render target).
    pub fn seeded() -> Self {
        let mut inv = Self::hotbar();
        inv.slots[0] = Some(ItemStack { kind: ItemKind::Dirt, count: 42 });
        inv.slots[3] = Some(ItemStack { kind: ItemKind::Stone, count: 7 });
        inv
    }
}
```

Then extend the `Client` struct (currently `input`, `fps_cam_mode`):

```rust
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash, PartialEq)]
#[module(crate)]
pub struct Client {
    pub input: InputState,
    pub fps_cam_mode: bool,
    pub inventory: Inventory,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p game inventory_tests`
Expected: PASS. If the `::borrow::Partial` derive on `Client` errors about the new field, confirm you did NOT add `Partial` to `Inventory` (its sibling field `InputState` is non-Partial and compiles — match that).

- [ ] **Step 5: Run the full game suite to prove no rollback regression**

Run: `cargo test -p game`
Expected: PASS (existing rollback/hash suites still green with the widened `Client`).

- [ ] **Step 6: Seed the inventory on player creation**

In `crates/game/src/region.rs`, the `CreateClient` arm currently does:

```rust
            GameEventKind::CreateClient(client_id) => {
                info!("{:?}", event);
                self.data.clients.insert(client_id, Client::default());
                self.data.create_player_safe(client_id);
            }
```

Change the insert to seed items:

```rust
            GameEventKind::CreateClient(client_id) => {
                info!("{:?}", event);
                self.data.clients.insert(
                    client_id,
                    Client { inventory: crate::Inventory::seeded(), ..Client::default() },
                );
                self.data.create_player_safe(client_id);
            }
```

- [ ] **Step 7: Run to verify still green**

Run: `cargo test -p game`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/game/src/state.rs crates/game/src/region.rs
git commit -m "feat(ui): inventory model on Client, seeded on player creation"
```

---

## Task 4: `build_player_ui` projection

**Files:**
- Modify: `crates/game/src/ui.rs`
- Test: inline `#[cfg(test)]` in `crates/game/src/ui.rs`

**Interfaces:**
- Consumes: `GameData`, `ClientId`, `Client`, `Inventory`, `ItemKind` (crate root); `WidgetTree`, `Widget`, `WidgetId`, `Style`, `Content`, `IconId`, `Val`, `FlexDirection`, `Color` (Task 1).
- Produces: `pub fn build_player_ui(data: &crate::GameData, client: crate::ClientId) -> WidgetTree` and `impl IconId { pub fn of(kind: crate::ItemKind) -> Option<IconId> }`. Deterministic id scheme: root `WidgetId(0)`; hotbar container `WidgetId(1)`; slot `i` → `WidgetId(100 + i as u64)`; slot `i`'s count label → `WidgetId(200 + i as u64)`. Empty slots still get a slot widget (empty), so the grid is stable; a filled slot's `Content` is `Icon`, with a child count label.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/game/src/ui.rs`:

```rust
    #[test]
    fn build_player_ui_projects_seeded_inventory() {
        // A minimal GameData with one client that has a seeded inventory.
        let mut rb = crate::Rollback::new(None);
        rb.new_transaction();
        rb.clients.insert(
            0,
            crate::Client { inventory: crate::Inventory::seeded(), ..crate::Client::default() },
        );
        rb.forget();

        let tree = build_player_ui(&rb, 0);

        // Root → hotbar container → 9 slot widgets.
        assert_eq!(tree.root, WidgetId(0));
        assert_eq!(tree.nodes[&WidgetId(0)].children, vec![WidgetId(1)]);
        let hotbar = &tree.nodes[&WidgetId(1)];
        assert_eq!(hotbar.children.len(), 9);
        assert_eq!(hotbar.children[0], WidgetId(100));

        // Slot 0 is seeded (Dirt x42): Icon content + a count-label child.
        let slot0 = &tree.nodes[&WidgetId(100)];
        assert!(matches!(slot0.content, Content::Icon(_)));
        assert_eq!(slot0.children, vec![WidgetId(200)]);
        assert!(matches!(&tree.nodes[&WidgetId(200)].content, Content::Text(t) if t == "42"));

        // Slot 1 is empty: present but no icon, no children.
        let slot1 = &tree.nodes[&WidgetId(101)];
        assert!(matches!(slot1.content, Content::None));
        assert!(slot1.children.is_empty());
    }

    #[test]
    fn build_player_ui_unknown_client_is_empty() {
        let rb = crate::Rollback::new(None);
        assert_eq!(build_player_ui(&rb, 999), WidgetTree::empty());
    }
```

Note: `build_player_ui` takes `&crate::GameData`; `Rollback` derefs to `GameData`, so passing `&rb` coerces.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game ui::tests::build_player_ui`
Expected: FAIL to compile ("cannot find function `build_player_ui`").

- [ ] **Step 3: Write minimal implementation**

Add to `crates/game/src/ui.rs` (before the tests module):

```rust
impl IconId {
    /// Map an item kind to a client icon handle. `Empty` has no icon.
    pub fn of(kind: crate::ItemKind) -> Option<IconId> {
        match kind {
            crate::ItemKind::Empty => None,
            crate::ItemKind::Dirt => Some(IconId(1)),
            crate::ItemKind::Stone => Some(IconId(2)),
        }
    }
}

fn slot_style() -> Style {
    Style {
        width: Val::Px(48.0),
        height: Val::Px(48.0),
        justify: JustifyContent::End,
        align: AlignItems::End,
        background: Some(Color { r: 0.1, g: 0.1, b: 0.1, a: 0.6 }),
        ..Style::default()
    }
}

/// Project the local player's authoritative data into a widget tree. Pure and
/// deterministic; run client-side only. Unknown client → empty tree.
pub fn build_player_ui(data: &crate::GameData, client: crate::ClientId) -> WidgetTree {
    let Some(c) = data.clients.get(&client) else {
        return WidgetTree::empty();
    };
    let mut tree = WidgetTree::empty();

    // Root: bottom-anchored full-width row.
    tree.nodes.get_mut(&WidgetId(0)).unwrap().style = Style {
        width: Val::Percent(100.0),
        height: Val::Percent(100.0),
        justify: JustifyContent::Center,
        align: AlignItems::End,
        ..Style::default()
    };
    tree.nodes.get_mut(&WidgetId(0)).unwrap().children = vec![WidgetId(1)];

    // Hotbar container.
    let mut hotbar = Widget {
        style: Style {
            direction: FlexDirection::Row,
            gap: Val::Px(4.0),
            padding: Rect::all(Val::Px(6.0)),
            background: Some(Color { r: 0.0, g: 0.0, b: 0.0, a: 0.4 }),
            ..Style::default()
        },
        ..Widget::default()
    };

    for (i, slot) in c.inventory.slots.iter().enumerate() {
        let slot_id = WidgetId(100 + i as u64);
        hotbar.children.push(slot_id);

        let mut slot_widget = Widget { style: slot_style(), ..Widget::default() };
        if let Some(stack) = slot {
            if let Some(icon) = IconId::of(stack.kind) {
                slot_widget.content = Content::Icon(icon);
                let count_id = WidgetId(200 + i as u64);
                slot_widget.children = vec![count_id];
                tree.nodes.insert(
                    count_id,
                    Widget {
                        content: Content::Text(stack.count.to_string()),
                        ..Widget::default()
                    },
                );
            }
        }
        tree.nodes.insert(slot_id, slot_widget);
    }

    tree.nodes.insert(WidgetId(1), hotbar);
    tree
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p game ui::tests::build_player_ui`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/ui.rs
git commit -m "feat(ui): build_player_ui projects inventory into a hotbar widget tree"
```

---

## Task 5: Client-side projection wiring (`instance.rs` + `ClientUpdateEvent::UiChannel`)

**Files:**
- Modify: `crates/game/src/lib.rs` (new `ClientUpdateEvent::UiChannel` variant)
- Modify: `crates/client/src/instance.rs`
- Test: inline `#[cfg(test)] mod manager_tests` in `crates/client/src/instance.rs`

**Interfaces:**
- Consumes: `game::UiUpdate`, `game::WidgetTree`, `game::build_player_ui`, `game::diff_ui` (Tasks 1–4); existing `World::data`, `home_region`, `client_id`.
- Produces: `ClientUpdateEvent::UiChannel(crossbeam::channel::Receiver<game::UiUpdate>)`; new `GameInstanceManager` fields `ui_update_send: Sender<game::UiUpdate>`, `prev_ui_tree: game::WidgetTree`; method `fn project_ui(&mut self)`. The UI receiver is announced once via `client_event_send` in `new()`.

- [ ] **Step 1: Write the failing test**

Add to `crates/client/src/instance.rs`'s `manager_tests` module:

```rust
    /// After a tick with a seeded local player, the manager must announce a UI
    /// channel and emit widget updates onto it (the projection ran).
    #[test]
    fn projection_emits_widget_updates_after_tick() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager =
            GameInstanceManager::new(game_send.clone(), game_recv, client_send, dummy_addr);

        // Load a region whose snapshot already contains player 0 (seeded inv).
        let region_id = RegionCoords::new(0, 0);
        let mut world = game::World::new();
        let mut home = Region::from_chunks(region_id, Vec::new());
        home.handle_event(GameEventKind::CreateClient(0)).unwrap();
        home.forget_last_event();
        world.load(&region_id, home);
        server_send.send(ServerPacket::PlayerRegion(Some(region_id), 0)).unwrap();
        server_send.send(world.build_region_server_packet(&region_id)).unwrap();
        assert!(manager.pump(&server_recv).unwrap());

        // A UI channel must have been announced.
        let ui_recv = {
            let mut found = None;
            while let Ok(ev) = client_recv.try_recv() {
                if let ClientUpdateEvent::UiChannel(r) = ev {
                    found = Some(r);
                }
            }
            found.expect("UiChannel announced")
        };

        // Tick once → projection runs → widget updates appear.
        manager.is_caught_up = true;
        manager.send_tick();
        assert!(manager.pump(&server_recv).unwrap());

        let ups: Vec<game::UiUpdate> = ui_recv.try_iter().collect();
        assert!(
            ups.iter().any(|u| matches!(u, game::UiUpdate::CreateWidget(_))),
            "projection must emit widget creates, got {ups:?}"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p client projection_emits_widget_updates_after_tick`
Expected: FAIL to compile ("no variant `UiChannel`").

- [ ] **Step 3: Add the `UiChannel` variant**

In `crates/game/src/lib.rs`, extend `ClientUpdateEvent`:

```rust
    /// One-shot: the receiver half of the client-side UI projection channel.
    /// Sent once by the manager; consumed by the ui_bridge. Widget-only —
    /// separate from the per-region `GameDataUpdate` channel because widgets
    /// hang off a screen-space UI root, not a region's world-space root.
    UiChannel(crossbeam::channel::Receiver<UiUpdate>),
```

- [ ] **Step 4: Wire the projection into the manager**

In `crates/client/src/instance.rs`:

Add fields to the `GameInstanceManager` struct (after `last_hud_status`):

```rust
    /// Sender half of the client-side UI projection channel (widget deltas).
    ui_update_send: Sender<game::UiUpdate>,
    /// Last widget tree emitted, for diffing the next projection.
    prev_ui_tree: game::WidgetTree,
```

In `new()`, before `Self { ... }`, create + announce the channel:

```rust
        let (ui_update_send, ui_update_recv) = crossbeam::channel::unbounded();
        // Announce the UI receiver to the bridge once. Buffered channel: safe
        // to send before the bridge's resources exist.
        let _ = client_event_send.send(game::ClientUpdateEvent::UiChannel(ui_update_recv));
```

and add the two fields to the `Self { ... }` initializer:

```rust
            ui_update_send,
            prev_ui_tree: game::WidgetTree::empty(),
```

Add the `project_ui` method to the `impl GameInstanceManager` block (near `emit_hud_status`):

```rust
    /// Client-only: project the local player's authoritative data into a widget
    /// tree, diff against the last emitted tree, and push widget deltas to the
    /// ui_bridge. Runs once per tick after the sim has settled. No-op until the
    /// local player + home region are known and loaded.
    fn project_ui(&mut self) {
        let (Some(home), Some(client_id)) = (self.home_region, self.client_id) else {
            return;
        };
        let tree = {
            let Some(world) = self.world.as_ref() else { return };
            if !world.region_exists(&home) {
                return;
            }
            game::build_player_ui(world.data(&home), client_id)
        };
        for update in game::diff_ui(&self.prev_ui_tree, &tree) {
            let _ = self.ui_update_send.send(update);
        }
        self.prev_ui_tree = tree;
    }
```

Call it at the end of the `Tick` arm in `handle_game_event`, after `self.update_window();`:

```rust
                self.apply_local_transfers();
                self.update_window();
                self.project_ui();
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p client projection_emits_widget_updates_after_tick`
Expected: PASS.

- [ ] **Step 6: Run both suites to confirm no regression**

Run: `cargo test -p game && cargo test -p client`
Expected: PASS (the widened `ClientUpdateEvent` compiles everywhere it is matched; the bridge's `drain_client_updates` match must be updated in Task 6, so if `client` fails now with a non-exhaustive-match error on `UiChannel`, add a temporary `ClientUpdateEvent::UiChannel(_) => {}` arm in `crates/client/src/renderer/bridge.rs` `drain_client_updates` and note it — Task 6 replaces it).

- [ ] **Step 7: Commit**

```bash
git add crates/game/src/lib.rs crates/client/src/instance.rs crates/client/src/renderer/bridge.rs
git commit -m "feat(ui): client-side per-tick UI projection into a dedicated channel"
```

---

## Task 6: Render bridge (`ui_bridge.rs`) — deltas → bevy_ui

**Files:**
- Create: `crates/client/src/renderer/ui_bridge.rs`
- Modify: `crates/client/src/renderer/mod.rs` (declare module, register resources + systems, consume `UiChannel`)
- Test: inline `#[cfg(test)]` in `crates/client/src/renderer/ui_bridge.rs`

**Interfaces:**
- Consumes: `game::{UiUpdate, WidgetId, Style, Content, Val, FlexDirection, JustifyContent, AlignItems, Color, IconId}`; `ClientUpdateEvent::UiChannel`; existing `ClientUpdates` resource.
- Produces: resources `UiUpdates(pub Option<Receiver<UiUpdate>>)`, `UiWidgetMap(pub BTreeMap<WidgetId, Entity>)`, `UiRoot(pub Option<Entity>)`; systems `receive_ui_channel` (moves the receiver out of `ClientUpdates` stream into `UiUpdates`), `drain_ui_updates`; helper `style_to_node(&Style) -> bevy::ui::Node`. Widgets spawn under a screen-space `UiRoot` node; children wired via `SetChildren`.

- [ ] **Step 1: Write the failing test**

Create `crates/client/src/renderer/ui_bridge.rs` with a test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::prelude::*;
    use game::{Content, Style, UiUpdate, WidgetId};

    fn ui_app() -> (App, crossbeam::channel::Sender<UiUpdate>) {
        let (send, recv) = crossbeam::channel::unbounded();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(UiUpdates(Some(recv)))
            .init_resource::<UiWidgetMap>()
            .init_resource::<UiRoot>()
            .add_systems(Update, drain_ui_updates);
        (app, send)
    }

    #[test]
    fn create_wire_and_content_build_a_node_tree() {
        let (mut app, send) = ui_app();
        // root(0) -> label(1)
        send.send(UiUpdate::CreateWidget(WidgetId(1))).unwrap();
        send.send(UiUpdate::SetStyle(WidgetId(1), Style::default())).unwrap();
        send.send(UiUpdate::SetContent(WidgetId(1), Content::Text("hi".into()))).unwrap();
        send.send(UiUpdate::SetChildren(WidgetId(0), vec![WidgetId(1)])).unwrap();
        app.update();

        let map = app.world().resource::<UiWidgetMap>();
        let e1 = *map.0.get(&WidgetId(1)).expect("widget 1 mapped");
        assert!(app.world().entity(e1).contains::<Node>(), "widget is a UI Node");
        assert!(app.world().entity(e1).contains::<Text>(), "text content applied");
    }

    #[test]
    fn remove_widget_despawns_and_unmaps() {
        let (mut app, send) = ui_app();
        send.send(UiUpdate::CreateWidget(WidgetId(5))).unwrap();
        app.update();
        let e = *app.world().resource::<UiWidgetMap>().0.get(&WidgetId(5)).unwrap();
        send.send(UiUpdate::RemoveWidget(WidgetId(5))).unwrap();
        app.update();
        assert!(app.world().resource::<UiWidgetMap>().0.get(&WidgetId(5)).is_none());
        assert!(app.world().get_entity(e).is_err(), "despawned");
    }

    #[test]
    fn unmapped_set_is_tolerated() {
        let (mut app, send) = ui_app();
        send.send(UiUpdate::SetContent(WidgetId(77), Content::Text("x".into()))).unwrap();
        app.update(); // must not panic
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p client ui_bridge`
Expected: FAIL to compile (module contents not yet written).

- [ ] **Step 3: Write minimal implementation**

At the top of `crates/client/src/renderer/ui_bridge.rs` (above the test module):

```rust
//! Client-side render bridge for engine-driven UI. Consumes `game::UiUpdate`
//! deltas (produced by the instance's per-tick projection) and maps each
//! `WidgetId` to a `bevy::ui::Node` under a screen-space UI root. Mirrors the
//! world-entity bridge in `bridge.rs`: a `WidgetId -> Entity` map, idempotent
//! application, and unmapped ids tolerated with a warn. Bevy UI's built-in
//! taffy lays the nodes out against the real viewport.
use std::collections::BTreeMap;

use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use crossbeam::channel::Receiver;
use game::{Color as UiColor, Content, FlexDirection, JustifyContent as UiJustify,
           AlignItems as UiAlign, Style, UiUpdate, Val as UiVal, WidgetId};

/// Receiver half of the UI projection channel, once the manager announces it.
#[derive(Resource, Default)]
pub struct UiUpdates(pub Option<Receiver<UiUpdate>>);

/// WidgetId → Bevy entity, the UI analogue of `SimEntityMap`.
#[derive(Resource, Default)]
pub struct UiWidgetMap(pub BTreeMap<WidgetId, Entity>);

/// The screen-space root all widgets hang under. Spawned lazily on first use.
#[derive(Resource, Default)]
pub struct UiRoot(pub Option<Entity>);

fn to_val(v: UiVal) -> Val {
    match v {
        UiVal::Auto => Val::Auto,
        UiVal::Px(p) => Val::Px(p),
        UiVal::Percent(p) => Val::Percent(p),
    }
}

/// Map an engine `Style` to a `bevy::ui::Node`.
pub fn style_to_node(s: &Style) -> Node {
    Node {
        flex_direction: match s.direction {
            FlexDirection::Row => bevy::ui::FlexDirection::Row,
            FlexDirection::Column => bevy::ui::FlexDirection::Column,
        },
        width: to_val(s.width),
        height: to_val(s.height),
        column_gap: to_val(s.gap),
        row_gap: to_val(s.gap),
        padding: UiRect {
            left: to_val(s.padding.left),
            right: to_val(s.padding.right),
            top: to_val(s.padding.top),
            bottom: to_val(s.padding.bottom),
        },
        justify_content: match s.justify {
            UiJustify::Start => bevy::ui::JustifyContent::FlexStart,
            UiJustify::Center => bevy::ui::JustifyContent::Center,
            UiJustify::End => bevy::ui::JustifyContent::FlexEnd,
            UiJustify::SpaceBetween => bevy::ui::JustifyContent::SpaceBetween,
        },
        align_items: match s.align {
            UiAlign::Start => bevy::ui::AlignItems::FlexStart,
            UiAlign::Center => bevy::ui::AlignItems::Center,
            UiAlign::End => bevy::ui::AlignItems::FlexEnd,
            UiAlign::Stretch => bevy::ui::AlignItems::Stretch,
        },
        ..Node::default()
    }
}

fn to_color(c: UiColor) -> Color {
    Color::srgba(c.r, c.g, c.b, c.a)
}

/// Move the UI receiver out of the ClientUpdates stream into `UiUpdates`. The
/// manager sends `ClientUpdateEvent::UiChannel` once; the world-entity bridge's
/// `drain_client_updates` ignores that variant (see its match), so we capture
/// it here to avoid coupling the two bridges.
pub fn receive_ui_channel(
    updates: Res<crate::renderer::ClientUpdates>,
    mut ui: ResMut<UiUpdates>,
) {
    if ui.0.is_some() {
        return;
    }
    // Non-destructive peek is not possible on a shared receiver; instead the
    // manager announces UiChannel exactly once and `drain_client_updates`
    // forwards it here via a resource. See mod.rs wiring.
    let _ = &updates; // placeholder: real capture happens in drain_client_updates (mod.rs)
}

fn ensure_root(commands: &mut Commands, root: &mut UiRoot) -> Entity {
    if let Some(e) = root.0 {
        return e;
    }
    let e = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..Node::default()
            },
            Name::new("ui-root"),
        ))
        .id();
    root.0 = Some(e);
    e
}

pub fn drain_ui_updates(
    mut commands: Commands,
    updates: Res<UiUpdates>,
    mut map: ResMut<UiWidgetMap>,
    mut root: ResMut<UiRoot>,
    mut windows: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    let Some(receiver) = updates.0.as_ref() else { return };
    // Ensure the screen-space root and the root widget (WidgetId(0)) exist.
    let root_entity = ensure_root(&mut commands, &mut root);
    map.0.entry(WidgetId(0)).or_insert(root_entity);

    while let Ok(update) = receiver.try_recv() {
        match update {
            UiUpdate::CreateWidget(id) => {
                if map.0.contains_key(&id) {
                    continue;
                }
                let e = commands.spawn((Node::default(), Name::new(format!("widget {:?}", id)))).id();
                map.0.insert(id, e);
            }
            UiUpdate::SetStyle(id, style) => {
                if let Some(&e) = map.0.get(&id) {
                    let mut node = style_to_node(&style);
                    if id == WidgetId(0) {
                        node.position_type = PositionType::Absolute;
                    }
                    commands.entity(e).insert(node);
                    match style.background {
                        Some(c) => { commands.entity(e).insert(BackgroundColor(to_color(c))); }
                        None => { commands.entity(e).remove::<BackgroundColor>(); }
                    }
                } else {
                    warn!("ui_bridge: SetStyle for unmapped {:?}", id);
                }
            }
            UiUpdate::SetContent(id, content) => {
                if let Some(&e) = map.0.get(&id) {
                    match content {
                        Content::None => {
                            commands.entity(e).remove::<Text>();
                            commands.entity(e).remove::<ImageNode>();
                        }
                        Content::Text(t) => {
                            commands.entity(e).remove::<ImageNode>();
                            commands.entity(e).insert(Text::new(t));
                        }
                        Content::Icon(_icon) => {
                            // Icon → texture resolution lands in a follow-up;
                            // for now mark with a tinted background so the slot
                            // is visibly filled. Keeps the pipeline honest
                            // without an asset dependency.
                            commands.entity(e).remove::<Text>();
                            commands.entity(e).insert(BackgroundColor(Color::srgb(0.4, 0.3, 0.2)));
                        }
                    }
                } else {
                    warn!("ui_bridge: SetContent for unmapped {:?}", id);
                }
            }
            UiUpdate::SetAction(_id, _action) => {
                // Slice 2 (ui_input) reads actions off a component; no-op here.
            }
            UiUpdate::SetChildren(id, children) => {
                let Some(&parent) = map.0.get(&id) else {
                    warn!("ui_bridge: SetChildren for unmapped parent {:?}", id);
                    continue;
                };
                let child_entities: Vec<Entity> =
                    children.iter().filter_map(|c| map.0.get(c).copied()).collect();
                // Ordered reparent. add_children preserves the given order.
                commands.entity(parent).add_children(&child_entities);
            }
            UiUpdate::RemoveWidget(id) => {
                if let Some(e) = map.0.remove(&id) {
                    commands.entity(e).despawn();
                } else {
                    warn!("ui_bridge: RemoveWidget for unmapped {:?}", id);
                }
            }
            UiUpdate::SetCursorGrab(grab) => {
                if let Ok(mut cursor) = windows.single_mut() {
                    if grab {
                        cursor.grab_mode = CursorGrabMode::Locked;
                        cursor.visible = false;
                    } else {
                        cursor.grab_mode = CursorGrabMode::None;
                        cursor.visible = true;
                    }
                }
            }
        }
    }
}
```

Remove the placeholder `receive_ui_channel` function (it was a stub to explain the wiring; the real capture happens in `drain_client_updates`). Delete that `pub fn receive_ui_channel(...) { ... }` block before moving on.

- [ ] **Step 4: Wire the module + capture the channel in `mod.rs`**

In `crates/client/src/renderer/mod.rs`:

Declare the module (near the other `mod` lines):

```rust
pub(crate) mod ui_bridge;
```

In `drain_client_updates` (in `bridge.rs`), the `UiChannel` variant must route the receiver into the `UiUpdates` resource. Add a `ResMut<ui_bridge::UiUpdates>` parameter to `drain_client_updates` and handle the variant. In `crates/client/src/renderer/bridge.rs`, change the signature:

```rust
pub fn drain_client_updates(
    mut commands: Commands,
    updates: Res<ClientUpdates>,
    mut player: ResMut<LocalPlayer>,
    mut regions: ResMut<Regions>,
    mut roots: ResMut<RegionRoots>,
    mut map: ResMut<SimEntityMap>,
    mut hud_status: ResMut<HudStatus>,
    mut ui_updates: ResMut<super::ui_bridge::UiUpdates>,
) {
```

and add the match arm (replacing any temporary `UiChannel(_) => {}` from Task 5):

```rust
            ClientUpdateEvent::UiChannel(recv) => {
                ui_updates.0 = Some(recv);
            }
```

Register resources + systems in `SimBridgePlugin::build`:

```rust
            .init_resource::<ui_bridge::UiUpdates>()
            .init_resource::<ui_bridge::UiWidgetMap>()
            .init_resource::<ui_bridge::UiRoot>()
```

and add `ui_bridge::drain_ui_updates` to the `PreUpdate` chain after the existing bridge drains:

```rust
                    (bridge::drain_client_updates, bridge::drain_region_updates, bridge::dedupe_ghosts, ui_bridge::drain_ui_updates).chain(),
```

(Placing `drain_ui_updates` after `drain_client_updates` guarantees the `UiChannel` is captured into `UiUpdates` before we try to drain it.)

- [ ] **Step 5: Run the bridge tests**

Run: `cargo test -p client ui_bridge`
Expected: PASS (3 tests).

- [ ] **Step 6: Run the whole client suite**

Run: `cargo test -p client`
Expected: PASS (the new `drain_client_updates` signature compiles; the `UiChannel` match is now exhaustive).

- [ ] **Step 7: Commit**

```bash
git add crates/client/src/renderer/ui_bridge.rs crates/client/src/renderer/mod.rs crates/client/src/renderer/bridge.rs
git commit -m "feat(ui): ui_bridge maps UiUpdate deltas to bevy_ui nodes"
```

---

## Task 7: Manual verification of slice 1 (render-only)

**Files:** none (verification only).

- [ ] **Step 1: Build and run server + client**

Run: `just run` (or `just adapter=6700 run` to pin the eGPU).
Expected: On connect, a bottom-centered hotbar of 9 dark slots appears, with slots 0 and 3 tinted brown (filled). No panic in either log.

- [ ] **Step 2: Confirm rollback resilience by moving across a region boundary**

Walk the player toward and across a region edge (free-cam via `E`, then movement keys). The hotbar must remain stable (it follows the player because `Inventory` rides `Client` across handoff) and must not flicker or duplicate.

- [ ] **Step 3: Commit (docs only, if you took notes)**

No code change expected. If you adjusted styling constants while verifying, commit them:

```bash
git add -A
git commit -m "chore(ui): slice-1 styling tweaks from manual verification"
```

---

# SLICE 2 — Interaction (click → logical action → sim → UI)

## Task 8: `UiAction` protocol event + sim handler

**Files:**
- Modify: `crates/game/src/ui.rs` (real `UiAction` variants)
- Modify: `crates/game/src/protocol.rs` (`GameEventKind::UiAction`)
- Modify: `crates/game/src/region.rs` (handle it)
- Test: inline `#[cfg(test)]` in `crates/game/src/region.rs`

**Interfaces:**
- Produces: `UiAction::MoveItem { from: usize, to: usize }`, `UiAction::ToggleInventory` (replacing `Noop`); `GameEventKind::UiAction(ClientId, UiAction)`; region handling that swaps inventory slots / flips an open flag on the addressed `Client`.
- Consumes: `Client`, `Inventory` (Task 3); existing `origin_client`/`matches_prediction` on `GameEventKind`.

- [ ] **Step 1: Write the failing test**

Add to `crates/game/src/region.rs` (new `#[cfg(test)] mod ui_action_tests`):

```rust
#[cfg(test)]
mod ui_action_tests {
    use super::*;
    use crate::{Inventory, ItemKind, ItemStack, UiAction};

    #[test]
    fn move_item_swaps_slots_and_rolls_back() {
        let mut region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
        region.handle_event(GameEventKind::CreateClient(0)).unwrap();
        region.forget_last_event();
        let before = crate::state_hash(region.data());

        region
            .handle_event(GameEventKind::UiAction(0, UiAction::MoveItem { from: 0, to: 1 }))
            .unwrap();
        // Slot 0 (seeded Dirt) moved to slot 1.
        let inv: &Inventory = &region.data().clients.get(&0).unwrap().inventory;
        assert_eq!(inv.slots[0], None);
        assert_eq!(inv.slots[1], Some(ItemStack { kind: ItemKind::Dirt, count: 42 }));

        region.rollback_last_event();
        assert_eq!(before, crate::state_hash(region.data()), "UiAction must invert bit-exact");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game ui_action_tests`
Expected: FAIL to compile ("no variant `UiAction`" / "no variant `MoveItem`").

- [ ] **Step 3: Replace the `UiAction` placeholder**

In `crates/game/src/ui.rs`, replace the `UiAction` enum:

```rust
/// Logical-space UI action (never pixels). Routed to the sim like PlayerInput.
#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    /// Swap two inventory slots (drag/drop resolves to this).
    MoveItem { from: usize, to: usize },
    /// Open/close the inventory (drives cursor grab).
    ToggleInventory,
}
```

Remove the `Noop` references (there were none outside its definition).

- [ ] **Step 4: Add the protocol event**

In `crates/game/src/protocol.rs`, add to `GameEventKind`:

```rust
    /// A logical-space UI action from a client (inventory move, toggle, ...).
    UiAction(ClientId, crate::UiAction),
```

Import is via the crate root; `crate::UiAction` resolves. Update the two exhaustive matches in `protocol.rs`:

In `origin_client`:

```rust
            GameEventKind::PlayerInput(id, _)
            | GameEventKind::CreateClient(id)
            | GameEventKind::UiAction(id, _) => Some(*id),
```

(Leave `matches_prediction`'s `_ => self == other` fallback as-is; `UiAction` matches by full equality, which is correct.)

- [ ] **Step 5: Handle it in the region**

In `crates/game/src/region.rs` `handle_event`, add an arm (after the `PlayerInput` arm):

```rust
            GameEventKind::UiAction(client_id, action) => {
                let data: &mut GameData = self.data.deref_mut();
                if let Some(c) = data.clients.get_mut(&client_id) {
                    match action {
                        crate::UiAction::MoveItem { from, to } => {
                            if from < c.inventory.slots.len() && to < c.inventory.slots.len() {
                                c.inventory.slots.swap(from, to);
                            }
                        }
                        crate::UiAction::ToggleInventory => {
                            c.inventory_open = !c.inventory_open;
                        }
                    }
                }
            }
```

This references a new `inventory_open` flag on `Client`. Add it in `crates/game/src/state.rs`:

```rust
pub struct Client {
    pub input: InputState,
    pub fps_cam_mode: bool,
    pub inventory: Inventory,
    pub inventory_open: bool,
}
```

- [ ] **Step 6: Run the test**

Run: `cargo test -p game ui_action_tests`
Expected: PASS.

- [ ] **Step 7: Run the whole game suite**

Run: `cargo test -p game`
Expected: PASS (new exhaustive arms compile; existing reconcile/rollback suites green).

- [ ] **Step 8: Commit**

```bash
git add crates/game/src/ui.rs crates/game/src/protocol.rs crates/game/src/region.rs crates/game/src/state.rs
git commit -m "feat(ui): UiAction protocol event + inventory move/toggle handler"
```

---

## Task 9: Cursor grab + inventory-open in the projection

**Files:**
- Modify: `crates/game/src/ui.rs` (`build_player_ui` marks actions; projection helper for cursor)
- Modify: `crates/client/src/instance.rs` (`project_ui` emits `SetCursorGrab`)
- Test: inline `#[cfg(test)]` in `crates/client/src/instance.rs`

**Interfaces:**
- Produces: slot widgets carry `action: Some(UiAction::MoveItem{..})`? No — click targets are resolved client-side (Task 10); here `build_player_ui` attaches `UiAction::ToggleInventory` to nothing yet. This task only wires cursor state: `project_ui` emits `UiUpdate::SetCursorGrab(!inventory_open)` when the local client's `inventory_open` changes.
- Consumes: `Client::inventory_open` (Task 8); `UiUpdate::SetCursorGrab` (Task 2); the `ui_bridge` cursor handler (Task 6).

- [ ] **Step 1: Write the failing test**

Add to `crates/client/src/instance.rs` `manager_tests`:

```rust
    /// Opening the inventory (inventory_open=true) must emit SetCursorGrab(false)
    /// so the cursor is released for pointing.
    #[test]
    fn projection_emits_cursor_release_when_inventory_opens() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager =
            GameInstanceManager::new(game_send.clone(), game_recv, client_send, dummy_addr);

        let region_id = RegionCoords::new(0, 0);
        let mut world = game::World::new();
        let mut home = Region::from_chunks(region_id, Vec::new());
        home.handle_event(GameEventKind::CreateClient(0)).unwrap();
        home.forget_last_event();
        world.load(&region_id, home);
        server_send.send(ServerPacket::PlayerRegion(Some(region_id), 0)).unwrap();
        server_send.send(world.build_region_server_packet(&region_id)).unwrap();
        manager.pump(&server_recv).unwrap();
        let ui_recv = {
            let mut r = None;
            while let Ok(ev) = client_recv.try_recv() {
                if let ClientUpdateEvent::UiChannel(rx) = ev { r = Some(rx); }
            }
            r.unwrap()
        };
        manager.is_caught_up = true;

        // Baseline tick.
        manager.send_tick();
        manager.pump(&server_recv).unwrap();
        while ui_recv.try_recv().is_ok() {}

        // Toggle the inventory open, tick, and expect a cursor-release update.
        game_send
            .send(GameEventKind::UiAction(0, game::UiAction::ToggleInventory))
            .unwrap();
        manager.send_tick();
        manager.pump(&server_recv).unwrap();

        let ups: Vec<game::UiUpdate> = ui_recv.try_iter().collect();
        assert!(
            ups.iter().any(|u| matches!(u, game::UiUpdate::SetCursorGrab(false))),
            "opening inventory releases the cursor, got {ups:?}"
        );
    }
```

Note: `GameEventKind::UiAction` routes through `handle_game_event`'s `PlayerInput`-style path. It is currently unhandled there — Step 3 adds routing.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p client projection_emits_cursor_release_when_inventory_opens`
Expected: FAIL (no `SetCursorGrab` emitted; and/or `UiAction` not routed to the sim).

- [ ] **Step 3: Route `UiAction` through the manager and emit cursor state**

In `crates/client/src/instance.rs` `handle_game_event`, add a `UiAction` arm alongside `PlayerInput` (same predicted-send discipline, gated on `is_caught_up`):

```rust
            e @ GameEventKind::UiAction(_, _) if !self.is_caught_up => { let _ = e; }
            e @ GameEventKind::UiAction(_, _) => {
                if let Some(home) = self.home_region {
                    if self.world.as_ref().unwrap().region_exists(&home) {
                        let event = self.world.as_mut().unwrap().handle_region_event(e, home)?;
                        self.server_game_send
                            .send(game::ClientPacket::GameEvent(event))
                            .unwrap();
                    }
                }
            }
```

Add an `inventory_open` tracker field to `GameInstanceManager` (after `prev_ui_tree`):

```rust
    /// Last-emitted cursor-grab intent, for edge-triggered SetCursorGrab.
    last_cursor_grab: bool,
```

initialize it to `true` in `new()`'s `Self { ... }` (game starts grabbed):

```rust
            last_cursor_grab: true,
```

Extend `project_ui` to also emit cursor grab on change (append before `self.prev_ui_tree = tree;`):

```rust
        // Cursor grab follows inventory-open: released while a menu is open.
        let want_grab = {
            let world = self.world.as_ref().unwrap();
            let open = world
                .data(&home)
                .clients
                .get(&client_id)
                .map(|c| c.inventory_open)
                .unwrap_or(false);
            !open
        };
        if want_grab != self.last_cursor_grab {
            let _ = self.ui_update_send.send(game::UiUpdate::SetCursorGrab(want_grab));
            self.last_cursor_grab = want_grab;
        }
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p client projection_emits_cursor_release_when_inventory_opens`
Expected: PASS.

- [ ] **Step 5: Run both suites**

Run: `cargo test -p game && cargo test -p client`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/instance.rs
git commit -m "feat(ui): route UiAction to sim + emit cursor grab from inventory-open"
```

---

## Task 10: Picking → `UiAction` (`ui_input.rs`)

**Files:**
- Create: `crates/client/src/renderer/ui_input.rs`
- Modify: `crates/client/src/renderer/ui_bridge.rs` (attach a `WidgetAction` component from `SetAction`)
- Modify: `crates/client/src/renderer/mod.rs` (register the system/observer; enable a picking backend)
- Modify: root `Cargo.toml` if `bevy_ui_picking_backend` is not already in the `bevy` feature list
- Test: inline `#[cfg(test)]` in `crates/client/src/renderer/ui_input.rs`

**Interfaces:**
- Consumes: `game::UiAction`; `GameEvents` resource (`Sender<GameEventKind>`); `LocalPlayer`; a `WidgetAction(pub game::UiAction)` component set by `ui_bridge` on `SetAction`.
- Produces: `fn emit_action(action: &game::UiAction, local: Option<game::ClientId>, out: &Sender<GameEventKind>)` (pure-ish, testable) and a Bevy `on_click` observer that calls it. Because headless picking can't synthesize real pointer events reliably, the test exercises `emit_action` directly.

- [ ] **Step 1: Write the failing test**

Create `crates/client/src/renderer/ui_input.rs` with:

```rust
//! Turns UI picks into logical `UiAction`s routed to the sim (like input).
use bevy::prelude::*;
use crossbeam::channel::Sender;
use game::{GameEventKind, UiAction};

use crate::renderer::{GameEvents, LocalPlayer};

/// The logical action a widget fires when clicked. Set by ui_bridge on SetAction.
#[derive(Component, Clone)]
pub struct WidgetAction(pub UiAction);

/// Send a widget's action to the sim as `GameEventKind::UiAction`, addressed to
/// the local player. No-op before the local player is known.
pub fn emit_action(action: &UiAction, local: Option<game::ClientId>, out: &Sender<GameEventKind>) {
    let Some(client) = local else { return };
    let _ = out.send(GameEventKind::UiAction(client, action.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_action_addresses_local_player() {
        let (send, recv) = crossbeam::channel::unbounded();
        emit_action(&UiAction::ToggleInventory, Some(7), &send);
        match recv.try_recv().unwrap() {
            GameEventKind::UiAction(7, UiAction::ToggleInventory) => {}
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[test]
    fn emit_action_noop_without_local_player() {
        let (send, recv) = crossbeam::channel::unbounded();
        emit_action(&UiAction::ToggleInventory, None, &send);
        assert!(recv.try_recv().is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p client ui_input`
Expected: FAIL to compile (module not declared).

- [ ] **Step 3: Declare the module + register the observer**

In `crates/client/src/renderer/mod.rs`:

```rust
pub(crate) mod ui_input;
```

In `ui_bridge.rs` `drain_ui_updates`, replace the `SetAction` no-op with a real component write:

```rust
            UiUpdate::SetAction(id, action) => {
                if let Some(&e) = map.0.get(&id) {
                    match action {
                        Some(a) => { commands.entity(e).insert(crate::renderer::ui_input::WidgetAction(a)); }
                        None => { commands.entity(e).remove::<crate::renderer::ui_input::WidgetAction>(); }
                    }
                } else {
                    warn!("ui_bridge: SetAction for unmapped {:?}", id);
                }
            }
```

Add a click observer. In `ui_input.rs`, add below `emit_action`:

```rust
/// Observer: on a UI click, emit the widget's action to the sim.
pub fn on_widget_click(
    click: On<Pointer<Click>>,
    actions: Query<&WidgetAction>,
    events: Res<GameEvents>,
    local: Res<LocalPlayer>,
) {
    if let Ok(action) = actions.get(click.entity()) {
        emit_action(&action.0, local.0, &events.0);
    }
}
```

Register it in `SimBridgePlugin::build` (observers are added with `add_observer`):

```rust
            .add_observer(ui_input::on_widget_click)
```

Note: verify the 0.18 picking-event API names as you wire this (`On<Pointer<Click>>` / `click.entity()`); adjust to the exact 0.18 signatures if they differ. The compile-and-test loop will catch mismatches.

- [ ] **Step 4: Ensure a UI picking backend is enabled**

`bevy_picking` core is on with `DefaultPlugins`, but the UI backend that hit-tests `Node`s may be a separate feature. In the root `Cargo.toml` `bevy` dependency `features` list, ensure `bevy_ui_picking_backend` is present (alongside the existing `bevy_ui`, `bevy_ui_render`). If it is already implied by `bevy_ui`, no change is needed — confirm by checking that `on_widget_click` fires in Task 11's manual run.

- [ ] **Step 5: Run the unit tests**

Run: `cargo test -p client ui_input`
Expected: PASS (2 tests).

- [ ] **Step 6: Run the whole client suite**

Run: `cargo test -p client`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/client/src/renderer/ui_input.rs crates/client/src/renderer/ui_bridge.rs crates/client/src/renderer/mod.rs Cargo.toml
git commit -m "feat(ui): pick widgets to emit logical UiActions to the sim"
```

---

## Task 11: End-to-end round-trip test + manual verification

**Files:**
- Test: `crates/game/tests/` — a new integration test `ui_roundtrip.rs` (sim-level, no window)

**Interfaces:**
- Consumes: `game::{World, Region, GameEventKind, UiAction, build_player_ui, Content, WidgetId, RegionCoords}`.

- [ ] **Step 1: Write the failing test**

Create `crates/game/tests/ui_roundtrip.rs`:

```rust
//! Full logical round-trip with no renderer: a UiAction mutates authoritative
//! inventory, and the re-projected widget tree reflects the change.
use game::{build_player_ui, Content, GameEventKind, Region, RegionCoords, UiAction, WidgetId};

#[test]
fn move_item_action_changes_the_projected_tree() {
    let mut region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.forget_last_event();

    // Before: slot 0 filled (icon), slot 1 empty (no content).
    let before = build_player_ui(region.data(), 0);
    assert!(matches!(before.nodes[&WidgetId(100)].content, Content::Icon(_)));
    assert!(matches!(before.nodes[&WidgetId(101)].content, Content::None));

    region
        .handle_event(GameEventKind::UiAction(0, UiAction::MoveItem { from: 0, to: 1 }))
        .unwrap();

    // After: slot 0 empty, slot 1 filled.
    let after = build_player_ui(region.data(), 0);
    assert!(matches!(after.nodes[&WidgetId(100)].content, Content::None));
    assert!(matches!(after.nodes[&WidgetId(101)].content, Content::Icon(_)));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test ui_roundtrip`
Expected: FAIL only if a wiring bug exists; if Tasks 3–8 are correct it may already PASS. Either way, running it locks the behavior in.

- [ ] **Step 3: Make it pass**

If it fails, the fault is in `build_player_ui` (Task 4) or the `MoveItem` handler (Task 8) — fix there; no new code should be needed in the test.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p game --test ui_roundtrip`
Expected: PASS.

- [ ] **Step 5: Manual verification (interaction)**

Run: `just run`. Confirm: clicking a filled slot then an empty slot moves the item (requires a two-click move UX, or wire a temporary key/`ToggleInventory` button first). At minimum, verify a click on a widget with an action releases/regrabs the cursor via `ToggleInventory` and that the cursor is released while the inventory is open (Task 9). No panic in logs.

- [ ] **Step 6: Update `TODO.md`**

Replace the trailing UI line in `TODO.md`:

```
The UI needs to be driven deterministically from inside the engine. This for in game ui's and UI elements that depend on ingame data.
```

with:

```
In-engine UI (spec docs/superpowers/specs/2026-07-07-in-engine-ui-design.md,
plan docs/superpowers/plans/2026-07-07-in-engine-ui.md): inventory/hotbar landed
(engine-owned data → client-side widget projection → bevy_ui). Follow-ups: real
item icons/textures, drag-drop UX, additional screens, and the deferred diegetic
world-space panel case (needs in-engine taffy + deterministic text measurement).
```

- [ ] **Step 7: Commit**

```bash
git add crates/game/tests/ui_roundtrip.rs TODO.md
git commit -m "test(ui): end-to-end UiAction round-trip; point TODO at spec+plan"
```

---

## Self-Review notes (for the executor)

- **Icon rendering is stubbed** (Task 6 Step 3): a filled slot shows a tinted background, not a real texture. Real `IconId → texture atlas` resolution is an intentional follow-up (spec's deferred "full item/icon system"), not part of this plan's acceptance.
- **Two-click vs drag move UX** is not built; `UiAction::MoveItem` exists and is tested, but a client gesture that produces `{from,to}` from two clicks is left to the follow-up UX pass. Task 11's manual step notes this.
- **`add_children` ordering / `On<Pointer<Click>>` API**: verify against Bevy 0.18 exactly; the compile/test loop is the safety net for API drift.
- If any `game` reconcile/rollback suite regresses after Task 3 or Task 8 (widening `Client`), the cause is almost certainly a missing/incorrect derive on the new field type — align its derives with `InputState`'s.
