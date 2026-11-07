use client::harness::SimHarness;

#[test]
fn harness_connects_and_ticks_without_panic() {
    let mut h = SimHarness::new();
    h.connect();
    // The client must have loaded its home region from the server snapshot.
    assert!(
        h.client_region_loaded(game::SPAWN_REGION),
        "home region loaded after connect"
    );
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
    // Controls on (input applies only in fps-cam mode): press KeyE, tick, release.
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
    // Horizontal only: gravity may still be settling the body vertically.
    assert_eq!((a[0], a[2]), (b[0], b[2]), "player must stop after key release (up-edge sent)");
}

#[test]
fn static_world_converges_client_and_server() {
    let mut h = SimHarness::new();
    h.connect();
    h.step_n(30); // no input; both sides advance
    h.assert_converged(); // client home-region state == server home-region state, bit-exact
}

#[test]
fn held_input_moves_the_player() {
    let mut h = SimHarness::new();
    h.connect();
    // fps-cam on (KeyE), let it take effect, then walk.
    h.press(game::Key::KeyE);
    h.step();
    h.release(game::Key::KeyE);
    h.step();
    h.assert_progresses(game::Key::KeyW); // holding W advances tick AND moves the body
}

#[test]
fn streamed_factor2_lod_arrives_for_ring_regions() {
    // End-to-end factor-2 check (the old far ring's replacement): the
    // instance's ring assignment must request factor-2 LOD for a region just
    // outside the near window, and the server's LodData reply must reach the
    // bridge channel with decodable, non-empty grids. Only the factor-2 ring
    // is enabled — the production table would inline-build hundreds of keys
    // through the LocalServer (minutes of wall clock).
    let mut h = SimHarness::with_lod_table(&[(2, 2)]);
    h.connect();
    h.step_n(10);
    let lod: Vec<(game::lod::LodKey, game::LodPayload)> = h
        .drain_bridge_events()
        .into_iter()
        .filter_map(|e| match e {
            game::ClientUpdateEvent::LodData(key, payload, _) => Some((key, payload)),
            _ => None,
        })
        .collect();
    // The viewer sits in SPAWN_REGION (0,0); the home-only near window is
    // excluded, so the distance-2 ring renders at factor 2.
    let key = game::lod::LodKey::for_region(2, game::RegionCoords::new(2, 0));
    let payload = lod
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, p)| p)
        .expect("factor-2 LodData for the distance-2 ring region (2,0)");
    assert!(!payload.is_empty(), "flat worldgen floor produces cells");
    for (_, grid) in payload {
        assert!(
            grid.decode().is_some(),
            "well-formed RLE grids over the wire"
        );
    }
}
