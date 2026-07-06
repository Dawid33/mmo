//! End-to-end headless tests for the Bevy client input glue: driving a real
//! KeyCode::KeyE through forward_input -> GameInstanceManager -> sim ->
//! SetFreeCam -> cursor grab. See
//! docs/superpowers/specs/2026-07-06-bevy-input-harness-design.md.

use client::bevy_harness::BevyInputHarness;

#[test]
fn local_player_is_set_after_connect() {
    let mut h = BevyInputHarness::new();
    h.connect();
    assert!(
        h.local_player().is_some(),
        "LocalPlayer must be set after connect (SetPlayer -> drain_client_updates); suspect (c)"
    );
}
