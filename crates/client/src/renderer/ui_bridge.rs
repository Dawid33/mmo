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
use game::{
    AlignItems as UiAlign, Color as UiColor, Content, FlexDirection, JustifyContent as UiJustify,
    Style, UiUpdate, Val as UiVal, WidgetId,
};

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

/// Drain the `UiUpdates` channel (once `bridge::drain_client_updates` has
/// routed the `ClientUpdateEvent::UiChannel` receiver into it) and apply each
/// delta to the `WidgetId -> Entity` map / `bevy::ui` node tree. Idempotent
/// and tolerant of updates that target an unmapped widget (warns, does not
/// panic) — mirrors the world-entity bridge's contract.
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
                        Some(c) => {
                            commands.entity(e).insert(BackgroundColor(to_color(c)));
                        }
                        None => {
                            commands.entity(e).remove::<BackgroundColor>();
                        }
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
                // Full replace (not additive): `UiUpdate::SetChildren` always
                // carries the widget's complete, ordered child list, so
                // `replace_children` is the correct semantic — `add_children`
                // would accumulate duplicates across repeated deltas for the
                // same parent (e.g. an inventory slot's count label toggling
                // on and off).
                commands.entity(parent).replace_children(&child_entities);
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

#[cfg(test)]
mod tests {
    use super::*;
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
