use client::harness::{Dir, SimHarness};

#[test]
fn walking_across_a_region_boundary_keeps_input_and_converges() {
    let mut h = SimHarness::new();
    h.connect();
    h.assert_converged(); // baseline agreement

    h.cross_boundary(Dir::East); // walk across the seam under real input

    // Pre-fix this panicked (rollback hash-verify on ecs.kind, or SyncClock
    // unwrap on a released region). Post-fix: no panic, input still applies,
    // states still agree.
    h.assert_progresses(game::Key::KeyW);
    h.assert_converged();
    assert_ne!(h.player_region(), game::SPAWN_REGION, "player actually crossed");
}

#[test]
fn crossing_is_deterministic() {
    // Same script twice -> identical final client state hash.
    fn run() -> u32 {
        let mut h = SimHarness::new();
        h.connect();
        h.cross_boundary(Dir::East);
        h.settle();
        game::state_hash(h.client_home_data())
    }
    assert_eq!(run(), run(), "harness must be deterministic");
}
