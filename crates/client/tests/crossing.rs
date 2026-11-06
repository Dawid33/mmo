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
/// instantaneous one. Input held across a seam under a real round trip is
/// `held_key_crossing_has_no_rubber_band` below (inputs follow the body).
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

/// One-way delay of `held_key_crossing_has_no_rubber_band`, in steps (one
/// tick each): a 100 ms round trip.
const RUBBER_BAND_LATENCY_STEPS: usize = 1;
/// Idle steps after the latency is switched on, for the client's clock to
/// take the round trip into its lead (`SyncClock` every 10 ticks).
const CLOCK_SETTLE_STEPS: usize = 60;
/// Steps the key stays held past the crossing (3 s).
const HELD_PAST_CROSSING_STEPS: usize = (3000 / game::TICK_RATE) as usize;
/// Sprint is pressed / released every this many steps of the walk, so the
/// inputs around the crossing change the body's speed: one lost or applied
/// in the wrong region on either side shows as a correction along the walk.
/// Several periods, so some toggle lands in the crossing's slots.
const SPRINT_TOGGLE_PERIODS: [usize; 4] = [1, 2, 3, 4];
/// The most the predicted body may move back along the walk between two
/// steps (one world unit, 1/16 block).
const MAX_STEP_BACK_UNITS: f32 = 1.0;

/// W held across the north seam under a 100 ms round trip, sprint toggling
/// all the while: the inputs the client predicts around the crossing land in
/// the same place on the server (they follow the body), so a reconcile never
/// pulls the predicted body back along its walk — no rubber band. Then the
/// regions converge bit-exact.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn held_key_crossing_has_no_rubber_band() {
    for period in SPRINT_TOGGLE_PERIODS {
        hold_w_across_north_seam(period);
    }
}

fn hold_w_across_north_seam(toggle_period: usize) {
    use game::Key;
    let mut h = SimHarness::with_near_radius(1);
    h.connect();
    h.set_latency(RUBBER_BAND_LATENCY_STEPS);
    h.step_n(CLOCK_SETTLE_STEPS);
    h.press(Key::KeyE);
    h.step();
    h.release(Key::KeyE);
    h.step();
    // Fly (double-tap Space, climb) so no ledge stops the walk.
    h.press(Key::Space);
    h.step();
    h.release(Key::Space);
    h.step();
    h.press(Key::Space);
    h.step_n(20);
    h.release(Key::Space);
    h.step_n(4);

    let start = h.player_region();
    h.press(Key::KeyW);
    // North is -z: z only ever falls.
    let mut last = h.player_world_pos().expect("body before the walk");
    let mut past = None;
    for step in 0..400 + HELD_PAST_CROSSING_STEPS {
        if step % toggle_period == 0 {
            if (step / toggle_period) % 2 == 0 {
                h.press(Key::ControlLeft);
            } else {
                h.release(Key::ControlLeft);
            }
        }
        h.step();
        let Some(now) = h.player_world_pos() else { continue };
        assert!(
            now[2] <= last[2] + MAX_STEP_BACK_UNITS,
            "sprint every {toggle_period}, step {step}: the predicted body moved back along the walk, z {} -> {} (home {:?})",
            last[2],
            now[2],
            h.player_region()
        );
        last = now;
        if past.is_none() && h.player_region() != start {
            past = Some(step);
        }
        if past.is_some_and(|p| step >= p + HELD_PAST_CROSSING_STEPS) {
            break;
        }
    }
    assert_eq!(h.player_region(), game::RegionCoords::new(0, -1), "crossed north");
    h.release(Key::ControlLeft);
    h.release(Key::KeyW);
    h.set_latency(0);
    h.assert_converged();
}
