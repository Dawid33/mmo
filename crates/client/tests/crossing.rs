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
    h.assert_regions_in_step(); // the new home and its window on one world tick
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

/// How much later than the latency that one input reaches the server: more
/// than the client's lead (round trip + 1.5 ticks), so the server applies it
/// after its slot.
const LATE_INPUT_EXTRA_STEPS: usize = 5;
/// Walk steps over which `server_crossing_later_converges` delays one input.
const LATE_INPUT_STEPS: std::ops::Range<usize> = 0..24;

/// The client predicts its crossing a slot EARLIER than the server: one
/// sprint press reaches the server a step late, so the server's body lags
/// the predicted one and crosses after it. The inputs the client then
/// predicts in the new home in the slots before the server's arrival are
/// applied by the server in the old region (the body is still there); the
/// arrival must take them out of the new home's log. Swept over where in
/// the walk the late press falls, so some sweep step lands next to the
/// crossing. Each run converges bit-exact with no local input left pending.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn server_crossing_later_converges() {
    for late_at in LATE_INPUT_STEPS {
        let run = std::panic::catch_unwind(|| server_crosses_later(late_at));
        assert!(run.is_ok(), "the late sprint press at walk step {late_at} broke convergence");
    }
}

fn server_crosses_later(late_at: usize) {
    use game::Key;
    let mut h = SimHarness::with_near_radius(1);
    h.connect();
    h.set_latency(RUBBER_BAND_LATENCY_STEPS);
    h.step_n(CLOCK_SETTLE_STEPS);
    h.press(Key::KeyE);
    h.step();
    h.release(Key::KeyE);
    h.step();
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
    let mut past = None;
    for step in 0..400 {
        if step == late_at {
            h.press(Key::ControlLeft);
            h.delay_next_send(LATE_INPUT_EXTRA_STEPS);
        }
        h.step();
        if past.is_none() && h.player_region() != start {
            past = Some(step);
        }
        if past.is_some_and(|p| step >= p + 10) {
            break;
        }
    }
    assert_eq!(h.player_region(), game::RegionCoords::new(0, -1), "crossed north");
    h.release(Key::ControlLeft);
    h.release(Key::KeyW);
    h.set_latency(0);
    h.assert_converged();
    assert_eq!(h.pending_local_inputs(), 0, "a local input stuck in the new home's log");
}

/// How long `predicted_halo_updates_do_not_mispredict` watches the slime:
/// 6 s, which in this scene sees it leave the band and come back.
const HALO_WATCH_STEPS: usize = (6000 / game::TICK_RATE) as usize;
/// Steps for its join to settle: past the unload grace, when any region the
/// server ran for the join that the client's window does not hold parks and
/// reseeds its neighbours' terrain halos — server events no client can
/// predict.
const HALO_JOIN_SETTLE_STEPS: usize = (game::UNLOAD_GRACE_MS / game::TICK_RATE) as usize + 10;

/// Flat ground under every chunk column of the regions at x <= 0; the rest is
/// void, so no slime spawns there.
fn floored_west(rc: game::RegionCoords) -> game::StudRegionSeed {
    if rc.x > 0 {
        return game::StudRegionSeed { ground: Vec::new(), features: Vec::new(), ..Default::default() };
    }
    client::local_server::lightweight_test_genesis()(rc)
}

/// A slime hopping in and out of the HOME region's band along its east seam,
/// mirrored into the east neighbour: the halo entity updates (and ends) the
/// client predicts for the east region are the server's, slot for slot, so
/// nothing mispredicts once the join has settled. That needs the home and
/// its neighbours on one predicted world tick (`regions::clock`): a
/// neighbour ahead of the home would close the slot of every transfer the
/// home sends it before it arrives — a late landing, one misprediction each.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn predicted_halo_updates_do_not_mispredict() {
    let mut h = SimHarness::with_genesis(1, std::sync::Arc::new(floored_west));
    let bv = game::BLOCK_UNITS as f32;
    let size = game::REGION_SIZE;
    let east = game::RegionCoords::new(game::SPAWN_REGION.x + 1, game::SPAWN_REGION.z);
    let mut id = None;
    // Before the client loads the regions, so its snapshots hold the slime.
    h.with_server_region(game::SPAWN_REGION, |r| {
        let mut key = None;
        let tick = r.current_tick();
        r.with_data(|d| {
            // A block and a half over the ground.
            let ground = game::plate_units(client::local_server::TEST_GROUND_TOP_PLATES);
            let pos = game::rapier3d::math::Vector::new(size - 7.0 * bv, ground + 1.5 * bv, size * 3.0 / 4.0);
            key = Some(d.spawn_slime_safe(pos, 1, 0.0, tick));
        });
        id = r.data().entity_id(key.unwrap());
    });
    let id = id.expect("the slime has an EntityId");
    h.connect();
    h.step_n(HALO_JOIN_SETTLE_STEPS);
    assert_eq!(h.player_region(), game::SPAWN_REGION, "the player's home holds the slime");
    h.assert_converged();
    h.assert_regions_in_step();
    let before = h.total_mispredictions();
    let mut mirrored = 0;
    let mut ended = 0;
    let mut entered = 0;
    let mut was = h.client_region_data(east).is_some_and(|d| d.halo_entity(id).is_some());
    for _ in 0..HALO_WATCH_STEPS {
        h.step();
        let now = h.client_region_data(east).is_some_and(|d| d.halo_entity(id).is_some());
        mirrored += usize::from(now);
        ended += usize::from(was && !now);
        entered += usize::from(!was && now);
        was = now;
    }
    assert!(mirrored > 0, "the client's east region never mirrored the slime");
    // Not just steady updates: the mirror was ended and made again while
    // watched.
    assert!(entered > 0 && ended > 0, "the slime did not both leave and enter the band ({entered} entries, {ended} ends)");
    assert_eq!(
        h.total_mispredictions() - before,
        0,
        "halo entity traffic mispredicted ({mirrored} steps mirrored, {entered} entries, {ended} ends)"
    );
    h.assert_regions_in_step();
    h.assert_converged();
}

/// A `BreakBlock` intent the home region's player sends at a tile of the
/// WEST region (`game::intents`): the client relays it between its predicted
/// regions like the server's manager, held to the same due tick — the break
/// in the west region, its reply back in the home — so nothing mispredicts,
/// and both ends converge with the server.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn predicted_intent_across_regions_does_not_mispredict() {
    let mut h = SimHarness::with_genesis(1, std::sync::Arc::new(floored_west));
    let west = game::RegionCoords::new(game::SPAWN_REGION.x - 1, game::SPAWN_REGION.z);
    let mid = game::REGION_BLOCKS as i32 / 2;
    // The ground's top tile.
    let top = client::local_server::TEST_GROUND_TOP_PLATES / game::TILE_PLATES as i32 - 1;
    let target = game::WorldBlockPos::from_local(west, game::BlockPos::new(mid, top, mid));
    h.connect();
    h.step_n(HALO_JOIN_SETTLE_STEPS);
    assert_eq!(h.player_region(), game::SPAWN_REGION);
    h.assert_converged();
    // A real round trip: the client leads the server, so its order lands in
    // the slot it was predicted in (in the lockstep harness the client
    // trails by a tick and the order itself would mispredict).
    h.set_latency(RUBBER_BAND_LATENCY_STEPS);
    h.step_n(CLOCK_SETTLE_STEPS);
    let solid = |h: &SimHarness| {
        h.client_region_data(west).is_some_and(|d| d.stud_tile([mid, top, mid]).fill() != game::TileFill::Open)
    };
    assert!(solid(&h), "the west region's floor is there to break");
    let before = h.total_mispredictions();
    h.order(game::Order::DevBreakBlock(target));
    let mut granted = 0;
    for _ in 0..10 + 2 * RUBBER_BAND_LATENCY_STEPS {
        h.step();
        let home = h.client_home_data();
        let me = home.player_entites.get(&client::LOCAL_CLIENT_ID).and_then(|&k| home.entity_id(k)).expect("the player's id");
        granted += home
            .intent_inbox(me)
            .iter()
            .filter(|r| matches!(r, game::IntentReply::Granted(game::Intent::BreakBlock { .. })))
            .count();
    }
    assert!(!solid(&h), "the client's west region broke the tile");
    assert_eq!(granted, 1, "the grant reached the sender once");
    assert_eq!(h.total_mispredictions() - before, 0, "the cross-region intent mispredicted");
    h.set_latency(0);
    h.assert_regions_in_step();
    h.assert_converged();
}

/// R3: one world on several hosts. The client logs in to the host that owns
/// its spawn region; the regions of its window another host owns come over a
/// connection to that host of their own (`client::connections`).
mod two_hosts {
    use client::harness::SimHarness;
    use std::collections::BTreeSet;

    /// Most steps `load_window` waits for the spawn window.
    const LOAD_STEPS: usize = 60;

    /// Step until every region of the spawn window is loaded (bounded); the window.
    fn load_window(h: &mut SimHarness) -> Vec<game::RegionCoords> {
        let window = game::SPAWN_REGION.window_3x3();
        for _ in 0..LOAD_STEPS {
            if window.iter().all(|rc| h.client_region_loaded(*rc)) {
                break;
            }
            h.step();
        }
        window
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn window_spanning_a_host_seam_loads_regions_from_both() {
        let mut h = SimHarness::with_hosts(2);
        h.connect();
        let window = load_window(&mut h);
        let owners: BTreeSet<u32> = window.iter().map(|rc| h.owner(*rc)).collect();
        assert_eq!(owners.len(), 2, "the spawn window spans a host seam");
        for rc in &window {
            let owner = h.owner(*rc);
            assert!(h.client_region_loaded(*rc), "{rc:?} (host {owner}'s) never loaded");
            assert_eq!(h.client_region_connection(*rc), owner, "{rc:?}'s traffic counts from its owner's connection");
        }
        let mut hosts = h.client_connections();
        hosts.sort();
        assert_eq!(hosts, vec![0, 1], "one connection per host");
        h.assert_regions_in_step();
        h.assert_converged();
    }

    /// Only the home connection's `SyncClock` samples steer the predicted clock:
    /// another host's, read 40 ticks ahead here, never move it.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn clock_follows_the_home_connection_only() {
        const SKEW_TICKS: i64 = 40;
        let mut h = SimHarness::with_hosts(2);
        h.connect();
        let window = load_window(&mut h);
        let home = h.home_host();
        let other = 1 - home;
        assert!(window.iter().any(|rc| h.owner(*rc) == other), "the window reaches the other host");
        h.skew_clock(other, SKEW_TICKS);
        for step in 0..SKEW_TICKS as usize {
            h.step();
            let lead = h.client_tick() as i64 - h.server_region_tick(home, game::SPAWN_REGION) as i64;
            assert!(
                (-3..=6).contains(&lead),
                "step {step}: the client's clock leads the home host by {lead} ticks (another host's samples steered it)"
            );
        }
    }
}
