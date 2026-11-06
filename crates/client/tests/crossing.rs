use client::harness::{Dir, SimHarness};

/// End-to-end headless crossing through the REAL WorldManager routing +
/// GameInstanceManager reconcile (the paths the original crossing panics lived
/// in, and which the hand-authored `manager_tests` crossing tests fake). Guards
/// that walking across a region seam: (1) never panics — the two fixed crossing
/// panics stay fixed; (2) keeps input live (`assert_progresses`); (3) actually
/// crosses; and (4) CONVERGES bit-exact to the authoritative server.
///
/// On (4): right at the flip tick the just-crossed home region is transiently
/// bit-inexact — the spec-accepted flip-tick rubber-band
/// (docs/superpowers/2026-07-06-seamless-local-crossing-followup.md). Root-caused
/// with the harness: it reconciles within ~1 tick once input stops (verified
/// deterministically). `assert_converged` allows that bounded, documented
/// transient to heal before its strict bit-exact check — so this asserts the
/// real accepted guarantee (converges just after input), not an impossible
/// instantaneous one. The deeper case (input held INDEFINITELY across a seam,
/// which never settles) remains the deferred protocol-level item in TODO.md.
#[test]
fn walking_across_a_region_boundary_keeps_input_and_converges() {
    let mut h = SimHarness::with_near_radius(1);
    h.connect();
    h.assert_converged(); // baseline agreement

    h.cross_boundary(Dir::East); // walk across the seam under real input

    h.assert_progresses(game::Key::KeyW); // input still moves the player post-cross
    h.assert_converged(); // converges bit-exact once the flip-tick transient heals
    assert_ne!(
        h.player_region(),
        game::SPAWN_REGION,
        "player actually crossed"
    );
}

#[test]
fn crossing_is_deterministic() {
    // Same script twice -> identical final client state hash.
    fn run() -> u32 {
        let mut h = SimHarness::with_near_radius(1);
        h.connect();
        h.cross_boundary(Dir::East);
        h.settle();
        game::state_hash(h.client_home_data())
    }
    assert_eq!(run(), run(), "harness must be deterministic");
}

/// Seeds swept by `crossing_converges_under_late_region_scheduling`.
const SCHEDULE_SEEDS: u64 = 6;

/// The same crossing, with the server's regions scheduled the way real region
/// threads schedule them rather than in lockstep: tick order permuted and, per
/// round, some regions seeing their inputs (the player's own input, the
/// `EntityArrived` handoff, ghost updates) one tick LATE. The lockstep harness
/// only ever exercises one schedule; the production server never runs that
/// one. A failure names its seed, which replays exactly.
///
/// Passing since 2026-11-06 (red with the lockstep test above from
/// 2026-09-20): predicted transfers now land in the server's slot.
///
/// `InlineSchedule::late`, not `jittery`: missed ticks desynchronise region
/// tick counts from the client, which this harness's tick-aligned
/// `assert_converged` cannot follow (server-only coverage of missed ticks is
/// `crates/game/tests/handoff_schedule.rs`).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn crossing_converges_under_late_region_scheduling() {
    for seed in 0..SCHEDULE_SEEDS {
        let run = std::panic::catch_unwind(|| {
            let mut h = SimHarness::with_near_radius(1);
            h.connect();
            h.set_schedule(game::InlineSchedule::late(seed));
            h.cross_boundary(Dir::East);
            h.assert_progresses(game::Key::KeyW);
            h.assert_converged();
            assert_ne!(h.player_region(), game::SPAWN_REGION, "player actually crossed");
        });
        assert!(run.is_ok(), "crossing broke under InlineSchedule::late({seed})");
    }
}
