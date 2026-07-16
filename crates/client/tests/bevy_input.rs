//! End-to-end headless tests for the Bevy client input glue: driving a real
//! KeyCode::KeyE through forward_input -> GameInstanceManager -> sim ->
//! SetFreeCam -> cursor grab. See
//! docs/superpowers/specs/2026-07-06-bevy-input-harness-design.md.

use std::time::Duration;

use bevy::math::Vec2;
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

/// The pre-change contract: with the window collapsed, every frame with motion
/// still produces exactly one MouseMotion event. Guards against the coalescer
/// dropping deltas outright.
#[test]
fn zero_window_emits_one_motion_event_per_frame() {
    let mut h = BevyInputHarness::new();
    h.connect();
    h.set_mouse_coalesce_window(Duration::ZERO);

    for i in 0..4 {
        h.move_mouse(Vec2::new(1.0 + i as f32, -1.0));
        h.step();
    }

    assert_eq!(
        h.mouse_motions(),
        &[(1.0, -1.0), (2.0, -1.0), (3.0, -1.0), (4.0, -1.0)],
        "a zero window must forward every frame's delta untouched"
    );
}

/// The point of the change: a window longer than the test withholds every delta
/// after the leading edge, and the withheld motion is summed rather than lost —
/// so N frames of motion cost 1 packet, not N.
#[test]
fn long_window_coalesces_frames_into_one_packet() {
    let mut h = BevyInputHarness::new();
    h.connect();
    h.set_mouse_coalesce_window(Duration::from_secs(3600));

    for _ in 0..8 {
        h.move_mouse(Vec2::new(2.0, 3.0));
        h.step();
    }

    assert_eq!(
        h.mouse_motions(),
        &[(2.0, 3.0)],
        "only the leading edge escapes a window longer than the test; the other \
         7 frames must be held, summed, and awaiting the window's expiry"
    );
}
