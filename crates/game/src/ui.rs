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

/// Logical-space UI action (never pixels). Routed to the sim like PlayerInput.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum UiAction {
    /// Swap two inventory slots (drag/drop resolves to this).
    MoveItem { from: usize, to: usize },
    /// Open/close the inventory (drives cursor grab).
    ToggleInventory,
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
}
