//! End-to-end headless tests for the Bevy client input glue: driving a real
//! KeyCode::KeyE through forward_input -> GameInstanceManager -> sim ->
//! SetFreeCam -> cursor grab. See
//! docs/superpowers/specs/2026-07-06-bevy-input-harness-design.md.

use bevy::window::CursorGrabMode;
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

#[test]
fn e_press_forwards_and_toggles_sim() {
    let mut h = BevyInputHarness::new();
    h.connect();
    h.press_e();
    // First step predicts the toggle (and grabs the cursor), but the client
    // rolls the prediction back on reconcile until the server confirms the E
    // press ~1 tick later — the same lockstep lag SimHarness settles for. Step
    // again so fps_cam_mode reaches its authoritative value.
    h.step();
    h.step();
    assert!(
        h.e_forwarded_count() >= 1,
        "the E press must be forwarded by forward_input (LocalPlayer gate); suspects (b)/(c)"
    );
    assert_eq!(
        h.sim_fps_cam_mode(),
        Some(true),
        "the E press must survive the manager gates and toggle fps_cam_mode in the sim; suspect (b)"
    );
}

#[test]
fn pressing_e_grabs_and_releases_cursor() {
    let mut h = BevyInputHarness::new();
    h.connect();
    assert_eq!(
        h.cursor_grab_mode(),
        CursorGrabMode::None,
        "cursor starts ungrabbed before any E press"
    );

    // First E press: fps-cam ON -> cursor grabbed.
    h.press_e();
    h.step();
    assert_eq!(
        h.cursor_grab_mode(),
        CursorGrabMode::Locked,
        "pressing E must grab/lock the cursor (the reported bug)"
    );

    // Release, then a second press: fps-cam OFF -> cursor released. (The sim
    // toggles on the press edge, so the second toggle needs release-then-press.)
    h.release_e();
    h.step();
    h.press_e();
    h.step();
    assert_eq!(
        h.cursor_grab_mode(),
        CursorGrabMode::None,
        "pressing E again must release the cursor"
    );
}
