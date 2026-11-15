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

/// Top of [`floored`]'s ground, in voxels = world units (half a block).
const FLOOR_TOP_VOXELS: u32 = 8;

/// A floor [`FLOOR_TOP_VOXELS`] high under every chunk column of every region:
/// ground everywhere, so a slime hops wherever it is spawned.
fn floored(_: game::RegionCoords) -> Vec<(game::ColumnCoords, game::Column)> {
    let air = game::Block::new(game::BlockId::AIR);
    let mut out = Vec::new();
    for x in 0..game::REGION_CHUNKS {
        for z in 0..game::REGION_CHUNKS {
            let sub = game::SubChunk::Dense(Box::new(game::Chunk::flat_floor(FLOOR_TOP_VOXELS)));
            out.push((game::ColumnCoords::new(x, z), game::Column::new(0, air, air, vec![sub])));
        }
    }
    out
}

/// A one-block floor under every chunk column of the regions at x <= 0; the
/// rest is void, so no slime spawns there.
fn floored_west(rc: game::RegionCoords) -> Vec<(game::ColumnCoords, game::Column)> {
    let air = game::Block::new(game::BlockId::AIR);
    let mut out = Vec::new();
    if rc.x > 0 {
        return out;
    }
    for x in 0..game::REGION_CHUNKS {
        for z in 0..game::REGION_CHUNKS {
            let sub = game::SubChunk::Dense(Box::new(game::Chunk::flat_floor(game::BLOCK_VOXELS as u32)));
            out.push((game::ColumnCoords::new(x, z), game::Column::new(0, air, air, vec![sub])));
        }
    }
    out
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
    let mut h = SimHarness::with_generator(1, Box::new(floored_west));
    let bv = game::BLOCK_VOXELS as f32;
    let size = game::REGION_SIZE;
    let east = game::RegionCoords::new(game::SPAWN_REGION.x + 1, game::SPAWN_REGION.z);
    let mut id = None;
    // Before the client loads the regions, so its snapshots hold the slime.
    h.with_server_region(game::SPAWN_REGION, |r| {
        let mut key = None;
        let tick = r.current_tick();
        r.with_data(|d| {
            let pos = game::rapier3d::math::Vector::new(size - 7.0 * bv, 2.5 * bv, size * 3.0 / 4.0);
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

/// A `BreakBlock` intent the home region's player sends at a block of the
/// WEST region (`game::intents`): the client relays it between its predicted
/// regions like the server's manager, held to the same due tick — the break
/// in the west region, its reply back in the home — so nothing mispredicts,
/// and both ends converge with the server.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn predicted_intent_across_regions_does_not_mispredict() {
    let mut h = SimHarness::with_generator(1, Box::new(floored_west));
    let west = game::RegionCoords::new(game::SPAWN_REGION.x - 1, game::SPAWN_REGION.z);
    let mid = game::REGION_BLOCKS as i32 / 2;
    let target = game::WorldBlockPos::from_local(west, game::BlockPos::new(mid, 0, mid));
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
        h.client_region_data(west)
            .and_then(|d| d.block_at([mid, 0, mid]))
            .is_some_and(|b| b.id != game::BlockId::AIR)
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
    assert!(!solid(&h), "the client's west region broke the block");
    assert_eq!(granted, 1, "the grant reached the sender once");
    assert_eq!(h.total_mispredictions() - before, 0, "the cross-region intent mispredicted");
    h.set_latency(0);
    h.assert_regions_in_step();
    h.assert_converged();
}

/// Steps the two-source scenes are watched for.
const TWO_SOURCES_WATCH_STEPS: usize = 200;
/// Fewest watched steps that must see both bodies mirrored in one target
/// (observed 107–190 of 200).
const MIN_CONTENDED_STEPS: usize = TWO_SOURCES_WATCH_STEPS / 4;
/// Server schedules `two_hosts_shuffled_server_order_does_not_mispredict` runs.
const SHUFFLED_SERVER_SEEDS: u64 = 6;

/// `ReconcileStats::kind_slot` of everything relayed between regions.
const TRANSFER_KIND: usize = 2;

/// Two sources mirrored into the same regions in one slot, slot after slot: a
/// slime hopping by the spawn region's north-west corner, and the player hovering
/// in the north-west region by its south-east corner. Both are in the entity
/// halo of the west and the north region, so each of those gets a halo message
/// from two regions in most slots (the slime's mirror also ends and comes back
/// as it hops out of the band and in). Returns the `transfer>transfer`
/// mispredictions over the watch, all mispredictions, and the steps the server's
/// west or north region mirrored both bodies. Converged before and after.
/// `h` stands on [`floored`] ground, so the slime hops.
fn two_sources_in_one_slot(mut h: SimHarness) -> (u64, u64, usize) {
    let bv = game::BLOCK_VOXELS as f32;
    let ground = FLOOR_TOP_VOXELS as f32;
    let spawn = game::SPAWN_REGION;
    let north_west = game::RegionCoords::new(spawn.x - 1, spawn.z - 1);
    let targets = [game::RegionCoords::new(spawn.x - 1, spawn.z), game::RegionCoords::new(spawn.x, spawn.z - 1)];
    // Before the client loads the regions, so its snapshots hold the slime.
    let mut slime = None;
    h.with_server_region(spawn, |r| {
        let tick = r.current_tick();
        let mut key = None;
        r.with_data(|d| {
            let pos = game::rapier3d::math::Vector::new(7.0 * bv, ground + 1.5 * bv, 7.0 * bv);
            key = Some(d.spawn_slime_safe(pos, 1, 0.0, tick));
        });
        slime = r.data().entity_id(key.unwrap());
    });
    let slime = slime.expect("the slime has an EntityId");
    h.connect();
    load_spawn_window(&mut h);
    h.step_n(HALO_JOIN_SETTLE_STEPS);
    // Fly north over the seam, then west over the next: the body hovers in
    // the north-west region by its south-east corner. (Walked, not teleported:
    // a server-side teleport leaves the client's spawn region diverged.)
    take_off(&mut h);
    walk_over_a_seam(&mut h, Dir::North);
    assert_eq!(walk_over_a_seam(&mut h, Dir::West), north_west, "the player hovers in the north-west region");
    // The window follows the home; what it left parks after the grace.
    h.step_n(HALO_JOIN_SETTLE_STEPS);
    assert_eq!(h.player_region(), north_west, "the player stays in the north-west region");
    h.assert_converged();
    let mut player = None;
    h.with_server_region(north_west, |r| {
        let d = r.data();
        player = d.player_entites.get(&client::LOCAL_CLIENT_ID).and_then(|&k| d.entity_id(k));
    });
    let player = player.expect("the player's id");
    let before = h.total_mispredictions_by_kind();
    let total_before = h.total_mispredictions();
    let mut contended = 0;
    for _ in 0..TWO_SOURCES_WATCH_STEPS {
        h.step();
        let mut both = false;
        for rc in targets {
            h.with_server_region(rc, |r| {
                both |= r.data().halo_entity(slime).is_some() && r.data().halo_entity(player).is_some();
            });
        }
        contended += usize::from(both);
    }
    let after = h.total_mispredictions_by_kind();
    h.assert_converged();
    (
        after[TRANSFER_KIND][TRANSFER_KIND] - before[TRANSFER_KIND][TRANSFER_KIND],
        h.total_mispredictions() - total_before,
        contended,
    )
}

/// Two neighbours' halo messages for one region in one slot reach the client's
/// region threads in whatever order the threads report them; the region applies
/// them in the server's canonical order (`game::SlotInbox`), so the order never
/// mispredicts.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn two_neighbours_halo_in_one_slot_does_not_mispredict() {
    let (transfers, total, contended) = two_sources_in_one_slot(SimHarness::threaded_with_generator(1, Box::new(floored)));
    assert!(contended >= MIN_CONTENDED_STEPS, "only {contended} steps had both bodies mirrored in the west or the north region");
    assert_eq!(transfers, 0, "transfer>transfer mispredictions ({total} in all, {contended} contended steps)");
    assert_eq!(total, 0, "mispredictions of other kinds ({contended} contended steps)");
}

/// The same two sources on two hosts (the spawn on one, the north-west region on
/// the other), each host ticking its regions in a shuffled order: the server's
/// order of a slot's messages is canonical whatever the schedule, and the
/// client's matches it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn two_hosts_shuffled_server_order_does_not_mispredict() {
    for seed in 0..SHUFFLED_SERVER_SEEDS {
        let mut h = SimHarness::with_hosts_with_generator(2, || Box::new(floored)).with_server_schedule(seed);
        let north_west = game::RegionCoords::new(game::SPAWN_REGION.x - 1, game::SPAWN_REGION.z - 1);
        assert_ne!(h.owner(game::SPAWN_REGION), h.owner(north_west), "the two sources run on different hosts");
        let (transfers, total, contended) = two_sources_in_one_slot(h);
        assert!(contended >= MIN_CONTENDED_STEPS, "seed {seed}: only {contended} steps had both bodies mirrored in the west or the north region");
        assert_eq!(
            transfers, 0,
            "seed {seed}: transfer>transfer mispredictions ({total} in all, {contended} contended steps)"
        );
        assert_eq!(total, 0, "seed {seed}: mispredictions of other kinds ({contended} contended steps)");
    }
}

/// Steps the crossing with a third party walks past the seam.
const THIRD_PARTY_PAST_STEPS: usize = 20;

/// The player flies north over the spawn's seam with W held (under a 100 ms
/// round trip: the client leads, so its inputs around the crossing reach the
/// server's old home and travel in the bundle) while a slime hops by the
/// spawn's north seam, mirrored into the north region — a halo message of a
/// third body in the slot of the player's arrival there. The server applies
/// the crossing body's inputs directly after its arrival, before that halo
/// message; the client predicts them there too, so neither the inputs nor the
/// transfers mispredict, and the regions converge.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn crossing_with_a_third_party_halo_in_the_slot_does_not_mispredict() {
    let bv = game::BLOCK_VOXELS as f32;
    let ground = FLOOR_TOP_VOXELS as f32;
    let spawn = game::SPAWN_REGION;
    let north = neighbour(spawn, Dir::North);
    let mut h = SimHarness::with_generator(1, Box::new(floored));
    let mut slime = None;
    h.with_server_region(spawn, |r| {
        let tick = r.current_tick();
        let mut key = None;
        r.with_data(|d| {
            let pos = game::rapier3d::math::Vector::new(7.0 * bv, ground + 1.5 * bv, 4.0 * bv);
            key = Some(d.spawn_slime_safe(pos, 1, 0.0, tick));
        });
        slime = r.data().entity_id(key.unwrap());
    });
    let slime = slime.expect("the slime has an EntityId");
    h.connect();
    load_spawn_window(&mut h);
    h.step_n(HALO_JOIN_SETTLE_STEPS);
    h.set_latency(RUBBER_BAND_LATENCY_STEPS);
    h.step_n(CLOCK_SETTLE_STEPS);
    take_off(&mut h);
    let before = h.total_mispredictions_by_kind();
    let mut mirrored = Vec::new();
    let mut crossed = None;
    h.press(game::Key::KeyW);
    for step in 0..400 + THIRD_PARTY_PAST_STEPS {
        h.step();
        let mut m = false;
        h.with_server_region(north, |r| m = r.data().halo_entity(slime).is_some());
        mirrored.push(m);
        if crossed.is_none() && h.player_region() == north {
            crossed = Some(step);
        }
        if crossed.is_some_and(|c| step >= c + THIRD_PARTY_PAST_STEPS) {
            break;
        }
    }
    h.release(game::Key::KeyW);
    let crossed = crossed.expect("the walk crossed north");
    assert!(
        mirrored[crossed.saturating_sub(2)..=crossed + 2].iter().all(|m| *m),
        "the slime was not mirrored in the north region around the crossing: {:?}",
        &mirrored[crossed.saturating_sub(2)..=crossed + 2]
    );
    let after = h.total_mispredictions_by_kind();
    let mut wrong = Vec::new();
    for p in 0..3 {
        for k in 0..3 {
            if (p != 0 || k != 0) && after[p][k] != before[p][k] {
                wrong.push(format!("{}>{}={}", game::ReconcileStats::KIND_NAMES[p], game::ReconcileStats::KIND_NAMES[k], after[p][k] - before[p][k]));
            }
        }
    }
    assert!(wrong.is_empty(), "input / transfer mispredictions across the crossing: {wrong:?}");
    h.set_latency(0);
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
        let owners: BTreeSet<game::HostId> = window.iter().map(|rc| h.owner(*rc)).collect();
        assert_eq!(owners.len(), 2, "the spawn window spans a host seam");
        for rc in &window {
            let owner = h.owner(*rc);
            assert!(h.client_region_loaded(*rc), "{rc:?} (host {owner}'s) never loaded");
            assert_eq!(h.client_region_host(*rc), Some(owner), "{rc:?}'s traffic counts from its owner's connection");
        }
        let mut hosts = h.client_connections();
        hosts.sort();
        assert_eq!(hosts, vec![game::HostId(0), game::HostId(1)], "one connection per host");
        h.assert_regions_in_step();
        h.assert_converged();
    }

    /// A host's `SyncClock` samples read this many ticks ahead
    /// (`clock_follows_the_home_connection_only`).
    const SKEW_TICKS: i64 = 40;
    /// The most the lead may stray from the unskewed run's, per step, in ticks.
    const LEAD_SLACK_TICKS: i64 = 1;

    /// The client's lead over host `host`'s region `rc` after each of `steps` steps,
    /// with host `skewed`'s samples read [`SKEW_TICKS`] ahead from the start (`None`:
    /// no skew).
    fn leads(h: &mut SimHarness, skewed: Option<game::HostId>, host: game::HostId, rc: game::RegionCoords, steps: usize) -> Vec<i64> {
        if let Some(skewed) = skewed {
            h.skew_clock(skewed, SKEW_TICKS);
        }
        (0..steps)
            .map(|_| {
                h.step();
                h.client_tick() as i64 - h.server_region_tick(host, rc) as i64
            })
            .collect()
    }

    /// Every lead of `skewed` within [`LEAD_SLACK_TICKS`] of the same step's lead in
    /// `baseline` (the same run with no skew).
    fn assert_leads_match(baseline: &[i64], skewed: &[i64], what: &str) {
        for (step, (b, s)) in baseline.iter().zip(skewed).enumerate() {
            assert!(
                (s - b).abs() <= LEAD_SLACK_TICKS,
                "step {step}: the client's clock leads by {s} ticks, {b} with no skew ({what}); \
                 baseline {baseline:?}, skewed {skewed:?}"
            );
        }
    }

    /// Only the home connection's `SyncClock` samples steer the predicted clock:
    /// another host's, read 40 ticks ahead here, never move it — the lead over the
    /// home host is the same, step for step, as in the same run with no skew.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn clock_follows_the_home_connection_only() {
        let run = |skew: bool| {
            let mut h = SimHarness::with_hosts(2);
            h.connect();
            let window = load_window(&mut h);
            let home = h.home_host();
            let other = game::HostId(1 - home.0);
            assert!(window.iter().any(|rc| h.owner(*rc) == other), "the window reaches the other host");
            leads(&mut h, skew.then_some(other), home, game::SPAWN_REGION, SKEW_TICKS as usize)
        };
        let baseline = run(false);
        assert_leads_match(&baseline, &run(true), "another host's samples steered it");
    }

    /// Once the body crossed into the other host's region and its connection became the
    /// home, the clock follows that host: the old home's samples, read 40 ticks ahead, no
    /// longer move it (the lead is the unskewed run's), and the NEW home's, read 40 ticks
    /// ahead, do (the lead over the new home grows toward the skew) — a clock steered by
    /// neither would pass the first half only.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn clock_follows_the_new_home_after_a_host_flip() {
        #[derive(Clone, Copy, PartialEq)]
        enum Skew {
            None,
            OldHome,
            NewHome,
        }
        let run = |skew: Skew| {
            let mut h = SimHarness::with_hosts(2);
            h.connect();
            load_window(&mut h);
            let old = h.home_host();
            let dir = super::foreign_dir(&mut h).expect("a cardinal neighbour on the other host");
            h.cross_boundary(dir);
            let new = h.client_home_host().expect("a home host");
            assert_ne!(new, old, "the home moved hosts");
            let rc = h.player_region();
            let skewed = match skew {
                Skew::None => None,
                Skew::OldHome => Some(old),
                Skew::NewHome => Some(new),
            };
            leads(&mut h, skewed, new, rc, SKEW_TICKS as usize)
        };
        let baseline = run(Skew::None);
        assert_leads_match(&baseline, &run(Skew::OldHome), "the old home's samples steered it");
        let followed = run(Skew::NewHome);
        let (end_base, end_new) = (*baseline.last().unwrap(), *followed.last().unwrap());
        assert!(
            end_new - end_base >= SKEW_TICKS / 2,
            "the new home's samples, {SKEW_TICKS} ticks ahead, moved the lead only from {end_base} to {end_new} \
             (baseline {baseline:?}, new home skewed {followed:?})"
        );
    }

    /// Most steps the regions of a dead host take to come back from its heir
    /// (`observed_host_dies_and_its_regions_come_back_from_the_heir`).
    const HEIR_STEPS: usize = 60;
    /// Steps the observed host stays dead before its succession is declared.
    const DEAD_STEPS: usize = 10;

    /// R4 Task 16: the client's window spans its home host and another it only observes.
    /// The observed host dies; the client's connection to it goes silent (a transport
    /// notices only later), so nothing of the client changes until the coordinator's
    /// succession gives the dead host's land to its heir — here the home, the only host
    /// left. Once the heir adopted that land, the home names itself for every region it had
    /// sent the client elsewhere for: the client re-maps each to the home connection
    /// (released on the dead one), subscribes there, and the home runs and serves it. The
    /// session never ended.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn observed_host_dies_and_its_regions_come_back_from_the_heir() {
        let mut h = SimHarness::with_claimed_hosts(2);
        h.connect();
        let window = load_window(&mut h);
        let home = h.home_host();
        let other = game::HostId(1 - home.0);
        let observed: Vec<game::RegionCoords> = window.iter().copied().filter(|rc| h.owner(*rc) == other).collect();
        assert!(!observed.is_empty(), "the window reaches the other host");
        for rc in &observed {
            assert!(h.client_region_loaded(*rc), "{rc:?} never loaded");
            assert_eq!(h.client_region_host(*rc), Some(other), "{rc:?} comes from its owner");
        }
        h.kill_host(other);
        h.step_n(DEAD_STEPS);
        for rc in &observed {
            assert_eq!(h.client_region_host(*rc), Some(other), "nothing is guessed before the succession");
        }
        assert_eq!(h.run_succession(other), home, "the home is the only host left: the heir");
        let back = |h: &mut SimHarness| {
            observed.iter().all(|rc| {
                h.client_region_host(*rc) == Some(home)
                    && h.client_region_loaded(*rc)
                    && h.host_manager(home).running_regions().contains(rc)
            })
        };
        for _ in 0..HEIR_STEPS {
            if back(&mut h) {
                break;
            }
            h.step();
        }
        for rc in &observed {
            assert_eq!(h.client_region_host(*rc), Some(home), "{rc:?}'s traffic counts from the home (the heir) now");
            assert!(h.host_manager(home).running_regions().contains(rc), "the heir runs {rc:?}");
            assert!(h.client_region_loaded(*rc), "{rc:?} is held");
        }
        assert_eq!(h.client_home_host(), Some(home), "the session lives on, on its home");
        assert_eq!(h.server_session(home), Some(game::SessionKind::Home));
        h.step_n(DEAD_STEPS);
        h.assert_regions_in_step();
    }
}

/// The first cardinal neighbour of the spawn region the other host of `h` owns (the
/// direction a walk crosses a host seam in).
fn foreign_dir(h: &mut SimHarness) -> Option<Dir> {
    let home = h.home_host();
    [Dir::East, Dir::West, Dir::North, Dir::South].into_iter().find(|dir| h.owner(neighbour(game::SPAWN_REGION, *dir)) != home)
}

/// The region next to `rc` in `dir` (north is -z).
fn neighbour(rc: game::RegionCoords, dir: Dir) -> game::RegionCoords {
    match dir {
        Dir::East => game::RegionCoords::new(rc.x + 1, rc.z),
        Dir::West => game::RegionCoords::new(rc.x - 1, rc.z),
        Dir::North => game::RegionCoords::new(rc.x, rc.z - 1),
        Dir::South => game::RegionCoords::new(rc.x, rc.z + 1),
    }
}

/// The key that walks the body in `dir`.
fn walk_key(dir: Dir) -> game::Key {
    match dir {
        Dir::North => game::Key::KeyW,
        Dir::South => game::Key::KeyS,
        Dir::East => game::Key::KeyD,
        Dir::West => game::Key::KeyA,
    }
}

/// How far along `dir` a world position is (grows as the body walks that way).
fn progress(pos: [f32; 3], dir: Dir) -> f32 {
    match dir {
        Dir::East => pos[0],
        Dir::West => -pos[0],
        Dir::North => -pos[2],
        Dir::South => pos[2],
    }
}

/// Most steps a walk waits for the window of `with_hosts` to load (both hosts'
/// regions) before the walk starts; every compared run steps the same number.
const HOSTS_LOAD_STEPS: usize = 60;

/// Steps after the crossing over which its corrections are counted.
const AFTER_CROSSING_STEPS: usize = 40;

/// Steps until every region of the spawn window is loaded (bounded).
fn load_spawn_window(h: &mut SimHarness) {
    for _ in 0..HOSTS_LOAD_STEPS {
        if game::SPAWN_REGION.window_3x3().iter().all(|rc| h.client_region_loaded(*rc)) {
            return;
        }
        h.step();
    }
    panic!("the spawn window never loaded");
}

/// Toggle the first-person camera on and take off (double-tap Space, climb): a
/// walk then never stops at a ledge.
fn take_off(h: &mut SimHarness) {
    use game::Key;
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
}

/// Hold `dir`'s key until the predicted body is in another region with a readable
/// pose (bounded), then release it; the region it reached.
fn walk_over_a_seam(h: &mut SimHarness, dir: Dir) -> game::RegionCoords {
    let start = h.player_region();
    h.press(walk_key(dir));
    for _ in 0..400 {
        h.step();
        if h.player_region() != start && h.player_world_pos().is_some() {
            break;
        }
    }
    h.release(walk_key(dir));
    assert_ne!(h.player_region(), start, "the walk never crossed a seam");
    h.player_region()
}

/// The same walk from the spawn over its `dir` seam — a thread seam in a one-host
/// world, a host seam with two hosts — and the home corrections it caused.
fn walk_and_count(mut h: SimHarness, dir: Dir) -> (u32, f32, game::HostId) {
    h.connect();
    let waited = (0..HOSTS_LOAD_STEPS)
        .take_while(|_| {
            let loaded = game::SPAWN_REGION.window_3x3().iter().all(|rc| h.client_region_loaded(*rc));
            if !loaded {
                h.step();
            }
            !loaded
        })
        .count();
    // Both runs walk at the same world tick.
    h.step_n(HOSTS_LOAD_STEPS - waited);
    take_off(&mut h);
    let before = h.corrections().0;
    let reached = walk_over_a_seam(&mut h, dir);
    assert_eq!(reached, neighbour(game::SPAWN_REGION, dir));
    h.step_n(AFTER_CROSSING_STEPS);
    let (n, max) = h.corrections();
    (n - before, max, h.client_home_host().expect("a home host"))
}

/// A host seam is no seam to the player: the same walk over the spawn's seam
/// toward the other host's region with no link delay corrects the predicted body
/// exactly as often, and as far, as the walk over a thread seam of one host.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_flip_matches_thread_flip_without_delay() {
    let mut probe = SimHarness::with_hosts(2);
    let dir = foreign_dir(&mut probe).expect("a cardinal neighbour on the other host");
    let other = game::HostId(1 - probe.home_host().0);
    drop(probe);
    let (thread_n, thread_max, _) = walk_and_count(SimHarness::with_near_radius(1), dir);
    let (host_n, host_max, home) = walk_and_count(SimHarness::with_hosts(2), dir);
    assert_eq!(home, other, "the two-host walk crossed a host seam (its home moved)");
    assert_eq!(
        host_n, thread_n,
        "corrections: {host_n} over the host seam, {thread_n} over the thread seam (max {host_max} vs {thread_max})"
    );
    assert!(
        (host_max - thread_max).abs() <= 0.01,
        "largest correction: {host_max} over the host seam, {thread_max} over the thread seam"
    );
}

/// W (or the spawn's host-seam direction) held across a host seam under a 100 ms
/// round trip, sprint toggling: the inputs around the crossing land where the client
/// predicted them (the old host forwards those it still gets), so no reconcile pulls
/// the body back along its walk; then everything converges.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn held_key_across_a_host_seam_no_rubber_band() {
    for period in SPRINT_TOGGLE_PERIODS {
        let mut h = SimHarness::with_hosts(2);
        let dir = foreign_dir(&mut h).expect("a cardinal neighbour on the other host");
        let other = game::HostId(1 - h.home_host().0);
        h.connect();
        load_spawn_window(&mut h);
        h.set_latency(RUBBER_BAND_LATENCY_STEPS);
        h.step_n(CLOCK_SETTLE_STEPS);
        take_off(&mut h);
        let start = h.player_region();
        h.press(walk_key(dir));
        let mut last = h.player_world_pos().expect("body before the walk");
        let mut past = None;
        for step in 0..400 + HELD_PAST_CROSSING_STEPS {
            if step % period == 0 {
                if (step / period) % 2 == 0 {
                    h.press(game::Key::ControlLeft);
                } else {
                    h.release(game::Key::ControlLeft);
                }
            }
            h.step();
            let Some(now) = h.player_world_pos() else { continue };
            assert!(
                progress(now, dir) >= progress(last, dir) - MAX_STEP_BACK_UNITS,
                "sprint every {period}, step {step}: the predicted body moved back along the walk, {} -> {} \
                 (home {:?}, host {:?})",
                progress(last, dir),
                progress(now, dir),
                h.player_region(),
                h.client_home_host()
            );
            last = now;
            if past.is_none() && h.player_region() != start {
                past = Some(step);
            }
            if past.is_some_and(|p| step >= p + HELD_PAST_CROSSING_STEPS) {
                break;
            }
        }
        assert_eq!(h.player_region(), neighbour(start, dir), "crossed the seam");
        assert_eq!(h.client_home_host(), Some(other), "the home moved to the other host");
        h.release(game::Key::ControlLeft);
        h.release(walk_key(dir));
        h.set_latency(0);
        h.assert_converged();
    }
}

/// The body crosses into the other host's region: that host's `PlayerRegion` (on its
/// observer connection) makes its connection the home — the hosts agree on which
/// session is the home — and the old home's connection stays, serving its regions.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn home_follows_player_region_from_the_new_connection() {
    let mut h = SimHarness::with_hosts(2);
    let dir = foreign_dir(&mut h).expect("a cardinal neighbour on the other host");
    let (old, other) = (h.home_host(), game::HostId(1 - h.home_host().0));
    h.connect();
    load_spawn_window(&mut h);
    assert_eq!(h.client_home_host(), Some(old));
    h.cross_boundary(dir);
    assert_eq!(h.client_home_host(), Some(other), "the home followed the body to host {other}");
    assert_eq!(h.server_session(other), Some(game::SessionKind::Home));
    assert_eq!(h.server_session(old), Some(game::SessionKind::Observer));
    assert_eq!(h.client_connections(), vec![game::HostId(0), game::HostId(1)], "the old home's connection stays");
    assert_eq!(h.client_region_host(game::SPAWN_REGION), Some(old), "it serves the regions it served");
    h.assert_progresses(game::Key::KeyW);
    h.assert_converged();
    h.assert_regions_in_step();
}

/// A -> B -> A, and B's packets reach the client only after A took the body back:
/// B's `PlayerRegion`, older than A's, must not take the home there.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn late_player_region_after_a_ping_pong_keeps_the_home() {
    let mut h = SimHarness::with_hosts(2);
    let dir = foreign_dir(&mut h).expect("a cardinal neighbour on the other host");
    let back = match dir {
        Dir::East => Dir::West,
        Dir::West => Dir::East,
        Dir::North => Dir::South,
        Dir::South => Dir::North,
    };
    let (a, b) = (h.home_host(), game::HostId(1 - h.home_host().0));
    h.connect();
    load_spawn_window(&mut h);
    take_off(&mut h);
    h.hold_host(b, true);
    walk_over_a_seam(&mut h, dir);
    // Over there on the server too (the hand-off acknowledged), before turning back.
    for _ in 0..40 {
        if h.server_session(b) == Some(game::SessionKind::Home) {
            break;
        }
        h.step();
    }
    assert_eq!(h.server_session(b), Some(game::SessionKind::Home), "host {b} took the body");
    assert_eq!(h.client_home_host(), Some(a), "B's PlayerRegion is held back");
    assert_eq!(walk_over_a_seam(&mut h, back), game::SPAWN_REGION);
    // On a little: the prediction ran without B's corrections, the body there may
    // trail it.
    h.press(walk_key(back));
    h.step_n(8);
    h.release(walk_key(back));
    for _ in 0..40 {
        if h.server_session(a) == Some(game::SessionKind::Home) {
            break;
        }
        h.step();
    }
    assert_eq!(h.server_session(a), Some(game::SessionKind::Home), "host {a} took the body back");
    h.step_n(4);
    h.hold_host(b, false);
    h.step_n(10);
    assert_eq!(h.client_home_host(), Some(a), "B's late PlayerRegion did not take the home back");
    assert_eq!(h.player_region(), game::SPAWN_REGION);
    h.assert_converged();
}

/// Play into a region of the other host far outside the window: the home moves the
/// player there (a move, Task 8) and sends the client on with an unasked
/// `RegionElsewhere`; the client connects, subscribes, and that host's `PlayerRegion`
/// makes it the home.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn teleport_to_a_foreign_host_switches_home() {
    const MAP_RADIUS: i32 = 4;
    let mut h = SimHarness::with_hosts(2);
    let other = game::HostId(1 - h.home_host().0);
    let target = (2..=MAP_RADIUS)
        .flat_map(|x| [game::RegionCoords::new(x, 0), game::RegionCoords::new(-x, 0)])
        .find(|rc| h.owner(*rc) == other)
        .expect("a region of the other host on the map, outside the window");
    let side = 2 * MAP_RADIUS as u32 + 1;
    for host in (0..2u64).map(game::HostId) {
        h.host_manager(host).set_world_map(game::WorldMap {
            origin: game::RegionCoords::new(-MAP_RADIUS, -MAP_RADIUS),
            width: side,
            height: side,
            tiles: (0..side * side).map(|_| play_tile()).collect(),
            rock_names: vec![],
            region_types: vec![],
        });
    }
    h.connect();
    load_spawn_window(&mut h);
    h.request(client::instance::ClientRequest::PlayRegion(target));
    let mut moved = false;
    for _ in 0..200 {
        h.step();
        if h.client_home_host() == Some(other) && h.player_region() == target && h.client_region_loaded(target) {
            moved = true;
            break;
        }
    }
    assert!(moved, "never moved to {target:?} on host {other}: home {:?} on host {:?}", h.player_region(), h.client_home_host());
    assert_eq!(h.server_session(other), Some(game::SessionKind::Home));
    assert_eq!(h.client_region_host(target), Some(other));
    h.settle();
    h.assert_converged();
}

/// A tile anyone can play: dry, flat, landing at its corner.
fn play_tile() -> game::TileSummary {
    game::TileSummary {
        ground_min_m: 0.0,
        ground_mean_m: 0.0,
        ground_max_m: 0.0,
        relief_m: 0.0,
        slope: [0.0, 0.0],
        sea_fraction: 0.0,
        lake_fraction: 0.0,
        lake_level_m: None,
        rivers: 0,
        river_max_discharge_m2: 0.0,
        river_max_width_m: 0.0,
        surface_rock: None,
        landing: Some([2, 2]),
        biome: game::Biome::Grassland,
        region: 0,
        temperature_c: 10.0,
        precip_mm: 700.0,
        savagery: 50,
        alignment: 0,
        volcano: false,
    }
}

/// The window leaves the other host's regions (the near radius drops to the home
/// alone): its connection, serving nothing, closes after `CONN_IDLE_CLOSE_MS` and the
/// session there ends. A later window over that host opens a new connection — the
/// old one's late `Closed` does not touch it — and its regions load again.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn idle_observer_connection_is_closed() {
    let idle_steps = (client::connections::CONN_IDLE_CLOSE_MS / game::TICK_RATE) as usize;
    let mut h = SimHarness::with_hosts(2);
    let (home, other) = (h.home_host(), game::HostId(1 - h.home_host().0));
    h.connect();
    load_spawn_window(&mut h);
    assert_eq!(h.client_connections(), vec![game::HostId(0), game::HostId(1)]);
    h.set_client_near_radius(0);
    h.step_n(idle_steps / 2);
    assert_eq!(h.client_connections(), vec![game::HostId(0), game::HostId(1)], "not idle long enough yet");
    h.step_n(idle_steps / 2 + 4);
    assert_eq!(h.client_connections(), vec![home], "the idle connection to host {other} closed");
    assert_eq!(h.server_session(other), None, "its session there ended");
    assert_eq!(h.observer_closes(other), 1);
    h.set_client_near_radius(1);
    load_spawn_window(&mut h);
    let mut hosts = h.client_connections();
    hosts.sort();
    assert_eq!(hosts, vec![game::HostId(0), game::HostId(1)], "a new connection to host {other}");
    for rc in game::SPAWN_REGION.window_3x3() {
        assert_eq!(h.client_region_host(rc), Some(h.owner(rc)));
    }
    assert_eq!(h.server_session(other), Some(game::SessionKind::Observer));
    h.assert_converged();
}

/// Carry (Task 17): the manager's half of an observer connection's end. A ticket that
/// reaches the far host before its order is refused there; the client asks for the
/// regions again through the home after the retry wait and gets in. Later that host
/// closes the connection: its regions are dropped, asked for again, and reload.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn refused_and_closed_observer_connections_reload_their_regions() {
    let mut h = SimHarness::with_hosts(2);
    let other = game::HostId(1 - h.home_host().0);
    let theirs: Vec<game::RegionCoords> =
        game::SPAWN_REGION.window_3x3().into_iter().filter(|rc| h.owner(*rc) == other).collect();
    h.orders_late(4);
    h.connect();
    load_spawn_window(&mut h);
    assert!(h.observer_closes(other) >= 1, "the early ticket was refused");
    h.orders_late(0);
    let refused = h.observer_closes(other);
    h.close_connection(other);
    h.step();
    assert!(theirs.iter().all(|rc| !h.client_region_loaded(*rc)), "its regions were dropped");
    load_spawn_window(&mut h);
    assert_eq!(h.observer_closes(other), refused + 1);
    for rc in &theirs {
        assert_eq!(h.client_region_host(*rc), Some(other), "{rc:?} served by host {other} again");
    }
    h.assert_converged();
}

/// LOD rings of `lod_subscriptions_follow_the_home` (the smallest: one ring).
const LOD_TABLE: &[(i32, u32)] = &[(2, 2)];

/// The home moves hosts: the LOD keys the client holds are released on the old home
/// (a session there holding only LOD subscriptions would be idle-closed) and asked of
/// the new one, which then serves every LOD request.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lod_subscriptions_follow_the_home() {
    let mut h = SimHarness::with_hosts(2);
    let dir = foreign_dir(&mut h).expect("a cardinal neighbour on the other host");
    let (old, other) = (h.home_host(), game::HostId(1 - h.home_host().0));
    h.set_client_lod_table(LOD_TABLE);
    h.connect();
    load_spawn_window(&mut h);
    h.step_n(4);
    let held = h.client_lod_keys();
    assert!(held > 0, "the client holds LOD keys");
    assert_eq!(h.lod_traffic(other), (0, 0), "LOD goes to the home only");
    h.cross_boundary(dir);
    assert_eq!(h.client_home_host(), Some(other));
    h.step_n(4);
    let (asked, released) = h.lod_traffic(old);
    assert_eq!(asked, released, "every key held on host {old} was released there");
    let (asked_new, released_new) = h.lod_traffic(other);
    assert_eq!(asked_new - released_new, h.client_lod_keys(), "the new home holds every key the client holds");
    let before = h.lod_traffic(old);
    h.step_n(20);
    assert_eq!(h.lod_traffic(old), before, "nothing more about LOD goes to host {old}");
}
