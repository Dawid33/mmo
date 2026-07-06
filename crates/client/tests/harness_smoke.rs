use client::harness::SimHarness;

#[test]
fn harness_connects_and_ticks_without_panic() {
    let mut h = SimHarness::new();
    h.connect();
    // The client must have loaded its home region from the server snapshot.
    assert!(h.client_region_loaded(game::SPAWN_REGION), "home region loaded after connect");
    // Advancing must not panic and must advance the client sim clock.
    let t0 = h.client_tick();
    h.step_n(20);
    assert!(h.client_tick() > t0, "ticks advanced");
}

#[test]
fn releasing_a_key_stops_the_player() {
    // Regression: release() must emit the key-up edge, else the sim keeps the
    // key `Held` forever and a released movement key drives the player forever.
    let mut h = SimHarness::new();
    h.connect();
    // fps-cam on (movement applies only in fps-cam mode): press KeyE, tick, release.
    h.press(game::Key::KeyE);
    h.step();
    h.release(game::Key::KeyE);
    h.step();
    // Walk forward, then release.
    h.press(game::Key::KeyW);
    h.step_n(3);
    h.release(game::Key::KeyW);
    h.step(); // this step carries the up-edge
    let a = h.player_pos();
    h.step_n(3);
    let b = h.player_pos();
    assert_eq!(a, b, "player must stop after key release (up-edge sent)");
}
