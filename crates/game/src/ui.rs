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
