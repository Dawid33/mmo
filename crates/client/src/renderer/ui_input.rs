//! Turns UI picks into logical `UiAction`s routed to the sim (like input).
use bevy::picking::events::{Click, Pointer};
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

/// Observer: on a UI click, emit the widget's action to the sim.
///
/// Bevy 0.18 observer params use `On<E, B>` (not `Trigger<E>`); `On` derefs to
/// the event `E`. `Pointer<Click>` is an `EntityEvent` whose target entity is
/// exposed via the `EntityEvent::event_target()` trait method (backed by its
/// `entity` field).
pub fn on_widget_click(
    click: On<Pointer<Click>>,
    actions: Query<&WidgetAction>,
    events: Res<GameEvents>,
    local: Res<LocalPlayer>,
) {
    if let Ok(action) = actions.get(click.event_target()) {
        emit_action(&action.0, local.0, &events.0);
    }
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
