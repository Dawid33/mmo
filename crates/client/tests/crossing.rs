use client::harness::{Dir, SimHarness};

/// KNOWN-FAILING, intentionally left red — a deterministic headless repro of
/// the ALREADY-KNOWN, spec-accepted "seamless local crossing under active
/// input" divergence (TODO.md; docs/superpowers/2026-07-06-seamless-local-crossing-followup.md,
/// whose proper fix needs a protocol-level input-routing / predicted-authority
/// design). This test is the harness earning its keep: it reproduces that
/// flip-tick divergence deterministically and headlessly — region (1,0)
/// @tick 33, identical client/server hash mismatch every run, single-threaded
/// lockstep so not a race.
///
/// What passes today: no panic through the crossing (the two fixed panics stay
/// fixed), input still moves the player (`assert_progresses`), and the player
/// actually crosses — i.e. the original "input stops when crossing" symptom is
/// gone. What FAILS: the final bit-exact `assert_converged()` at the flip tick.
/// Note the harness's strict tick-aligned check reports it as NOT self-healing
/// within the test's steps, which is sharper than TODO.md's "transient
/// rubber-band" wording — reconciling those (truly transient vs. sharper
/// variant) is part of the deferred protocol-level fix, not a harness change.
/// Remove this comment / flip to strict when the crossing converges.
#[test]
fn walking_across_a_region_boundary_keeps_input_and_converges() {
    let mut h = SimHarness::new();
    h.connect();
    h.assert_converged(); // baseline agreement

    h.cross_boundary(Dir::East); // walk across the seam under real input

    // No panic + input still applies + player crossed all hold today; the final
    // convergence check is the one that fails (the tracked divergence above).
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
