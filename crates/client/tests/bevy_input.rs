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

/// Put the local player in fps-cam mode the way a player does — press E — and
/// wait until the window holds the pointer: mouse motion only turns the camera
/// while it does.
fn grab_pointer(h: &mut BevyInputHarness) {
    h.press_e();
    h.step();
    h.release_e();
    h.step();
    assert_eq!(h.cursor_grab_mode(), CursorGrabMode::Locked, "E must grab the pointer");
}

/// Raw motion arrives from the device whether or not the window holds the
/// pointer. Without a grab (menus, another window focused, an autopilot run
/// that enabled fps-cam with `InputEvent::FpsCam`) it must not reach the sim,
/// or moving the mouse over another window turns the player.
#[test]
fn motion_without_a_grabbed_pointer_is_not_forwarded() {
    let mut h = BevyInputHarness::new();
    h.connect();
    h.set_mouse_coalesce_window(Duration::ZERO);
    for _ in 0..4 {
        h.move_mouse(Vec2::new(5.0, 5.0));
        h.step();
    }
    assert!(h.mouse_motions().is_empty(), "ungrabbed motion forwarded: {:?}", h.mouse_motions());
}

/// The pre-change contract: with the window collapsed, every frame with motion
/// still produces exactly one MouseMotion event. Guards against the coalescer
/// dropping deltas outright.
#[test]
fn zero_window_emits_one_motion_event_per_frame() {
    let mut h = BevyInputHarness::new();
    h.connect();
    grab_pointer(&mut h);
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
    grab_pointer(&mut h);
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

/// Live report 2026-11-05: "items not rendering". A thrown item must reach
/// the pile renderer as an `ItemPileMsg::Set` with a non-empty view.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn thrown_item_reaches_the_pile_renderer() {
    use client::renderer::item_piles::ItemPileMsg;
    use game::{GameEventKind, InputEvent, Key, Order, PlayMode};
    let reg = game::BlockRegistry::from_ron(&client::blocks::manifest_src()).unwrap();
    game::install_block_props(reg.props());
    let mut h = BevyInputHarness::new();
    h.app_mut().add_message::<ItemPileMsg>();
    h.connect();
    let me = h.local_player().expect("local player");
    h.push_sim_event(GameEventKind::Order(me, Order::SetMode { mode: PlayMode::Adventure, at: None }));
    h.push_sim_event(GameEventKind::PlayerInput(me, InputEvent::FpsCam(true)));
    for _ in 0..4 {
        h.step();
    }
    h.push_sim_event(GameEventKind::PlayerInput(me, InputEvent::Key { key: Key::KeyQ, pressed: true }));
    h.step();
    h.push_sim_event(GameEventKind::PlayerInput(me, InputEvent::Key { key: Key::KeyQ, pressed: false }));
    let mut seen = Vec::new();
    for _ in 0..10 {
        h.step();
        let mut msgs = h.app_mut().world_mut().resource_mut::<bevy::ecs::message::Messages<ItemPileMsg>>();
        seen.extend(msgs.drain().map(|m| format!("{m:?}")));
    }
    eprintln!("pile msgs: {seen:#?}");
    assert!(
        seen.iter().any(|m| m.starts_with("Set") && m.contains("stacks")),
        "a thrown item must reach the pile renderer; got {seen:?}"
    );
}
