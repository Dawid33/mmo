//! The client's predicted regions on their own threads (native play's
//! `RegionHost::Threaded`), driven through the same lockstep harness as the
//! inline host: every pump waits for the region threads (`settle_regions`).

use client::harness::{Dir, SimHarness};

/// The 3×3 window, nine region threads: after a stretch of walking, every
/// client region matches the server's authoritative state bit for bit.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn threaded_regions_converge_bit_exact() {
    let mut h = SimHarness::threaded(1);
    h.connect();
    assert_eq!(h.client_region_count(), 9, "the 3x3 window runs on threads");
    h.assert_converged();
    h.assert_progresses(game::Key::KeyW);
    h.assert_converged();
    h.assert_regions_in_step(); // one world clock on threads as inline
}

/// Input keeps flowing across a predicted crossing on threads: the home
/// flips and the player still moves under input afterwards (an input caught
/// between the old home's departure and the flip is re-sent, not lost).
/// Bit-exact convergence after a crossing is the known-red TODO.md
/// "REGRESSED" item on the inline host too, so it is not asserted here.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn threaded_input_survives_crossing() {
    let mut h = SimHarness::threaded(1);
    h.connect();
    h.cross_boundary(Dir::East);
    assert_ne!(h.player_region(), game::SPAWN_REGION, "player actually crossed");
    h.assert_progresses(game::Key::KeyW);
    h.settle();
    h.assert_regions_in_step();
}
