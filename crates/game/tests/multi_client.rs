//! Multi-client reconcile: foreign events are insertions into the local
//! timeline, not mispredictions. See
//! docs/superpowers/specs/2026-07-04-multi-client-players-design.md
use game::{GameEventKind, InputEvent, Key};

#[test]
fn origin_client_classifies_event_kinds() {
    let input = InputEvent::Key { key: Key::KeyW, pressed: true };
    assert_eq!(GameEventKind::PlayerInput(7, input).origin_client(), Some(7));
    assert_eq!(GameEventKind::CreateClient(3).origin_client(), Some(3));
    assert_eq!(GameEventKind::Tick.origin_client(), None);
    assert_eq!(GameEventKind::Quit.origin_client(), None);
}
