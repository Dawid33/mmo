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
