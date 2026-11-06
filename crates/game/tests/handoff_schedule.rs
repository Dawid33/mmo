//! Boundary handoff under scheduling jitter. `InlineSpawner`'s default is one
//! fixed schedule (every region ticks once per round, every output lands
//! before the neighbour's next tick); real region threads each run their own
//! timer. `InlineSchedule` replays that jitter from a seed — permuted tick
//! order, inputs that arrive a tick late, missed ticks — over a straight seam
//! crossing and a diagonal one through a region corner, so a schedule that
//! breaks the handoff is a seed to replay rather than a flaky thread test
//! (`crates/server/tests/threaded_world.rs` keeps the real-thread plumbing
//! check). Run via `just test-threads`.
use game::{
    Block, BlockId, Chunk, ClientPacket, Column, ColumnCoords, GameEvent, GameEventKind, Goal, InlineSchedule,
    InputEvent, Key, Order, PlayMode, RegionCoords, ServerEvent, SubChunk, REGION_BLOCKS, REGION_SIZE,
    SPAWN_REGION, TICK_RATE,
};

use crate::common;
use crate::common::rig::{north, north_west, seam_floor_columns, Rig, Seen, CLIENT, LEG_ROUNDS};
use game::EntityId;
use std::collections::BTreeMap;

/// Seeds swept per schedule mode.
const SEEDS: u64 = 12;
/// Rounds walked past the seam before turning back, so the return leg starts
/// clear of the flip hysteresis.
const PAST_SEAM_ROUNDS: usize = 6;

/// One scripted walk out of the spawn region and back.
#[derive(Clone, Copy)]
struct Route {
    name: &'static str,
    target: RegionCoords,
    out: &'static [Key],
    back: &'static [Key],
}

/// Straight across one seam.
const NORTH: Route =
    Route { name: "north", target: RegionCoords { x: 0, z: -1 }, out: &[Key::KeyW], back: &[Key::KeyS] };
/// Diagonally through the corner. The spawn point is equidistant from both
/// seams, so the walk reaches them together: whether the player hops through
/// an edge neighbour first or lands in the diagonal one directly is up to the
/// schedule — it must end up owned by the diagonal region either way.
const CORNER: Route = Route {
    name: "corner",
    target: RegionCoords { x: -1, z: -1 },
    out: &[Key::KeyW, Key::KeyA],
    back: &[Key::KeyS, Key::KeyD],
};

impl Rig {
    fn new(schedule: Option<InlineSchedule>) -> Self {
        Self::with_floor(schedule, seam_floor_columns)
    }
}

/// Walk `route` out of the spawn region, a little further, then back home.
/// Returns the final fingerprint so callers can compare runs.
fn walk_there_and_back(
    route: Route,
    schedule: Option<InlineSchedule>,
) -> Vec<(RegionCoords, u64, u32)> {
    let mut rig = Rig::new(schedule);
    rig.event(ServerEvent::connected(CLIENT));
    for rc in SPAWN_REGION.window_3x3() {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), CLIENT));
    }
    // A late spawn region creates the player a round or two after connect.
    rig.walk_until_owned_by(SPAWN_REGION, "spawn");

    rig.key(Key::KeyE, true);
    rig.key(Key::KeyE, false);
    for &key in route.out {
        rig.key(key, true);
    }
    rig.walk_until_owned_by(route.target, "outbound");
    assert_eq!(rig.pushed_home, Some(route.target), "client told about its new home");

    // Input follows the player across the flip: the keys are still held, so
    // it keeps walking -z in the new region without any new key event.
    let (_, z_arrived) = rig.player_xz(route.target);
    for _ in 0..PAST_SEAM_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.owners(), [route.target], "no thrash back across the seam");
    let (_, z_walked) = rig.player_xz(route.target);
    assert!(z_walked < z_arrived, "held input died at the handoff: z {z_arrived} -> {z_walked}");

    // Turn around. These key events are still stamped with the OLD home.
    for &key in route.out {
        rig.key(key, false);
    }
    for &key in route.back {
        rig.key(key, true);
    }
    rig.walk_until_owned_by(SPAWN_REGION, "return");
    assert_eq!(rig.pushed_home, Some(SPAWN_REGION), "client told it is home again");
    for &key in route.back {
        rig.key(key, false);
    }

    // Let halo mirrors and late inputs drain, then the player must still be
    // exactly where ownership says it is.
    for _ in 0..PAST_SEAM_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.owners(), [SPAWN_REGION]);
    rig.fingerprint()
}

/// Sweeps `SEEDS` seeds of one schedule mode over every route; returns how
/// many runs ended in a different state than lockstep, so callers can prove
/// the jitter bit.
fn sweep(mode: &str, schedule: fn(u64) -> InlineSchedule) -> usize {
    let mut perturbed = 0;
    for route in [NORTH, CORNER] {
        let lockstep = walk_there_and_back(route, None);
        for seed in 0..SEEDS {
            let run = || {
                std::panic::catch_unwind(|| walk_there_and_back(route, Some(schedule(seed))))
                    .unwrap_or_else(|_| {
                        panic!("{} handoff broke under schedule `{mode}`, seed {seed}", route.name)
                    })
            };
            let first = run();
            assert_eq!(
                first,
                run(),
                "{} under schedule `{mode}` seed {seed} is not reproducible",
                route.name
            );
            perturbed += usize::from(first != lockstep);
        }
    }
    perturbed
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lockstep_walk_there_and_back() {
    walk_there_and_back(NORTH, None);
    walk_there_and_back(CORNER, None);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_survives_permuted_tick_order() {
    // Order alone may legitimately end bit-identical to lockstep: regions
    // share no state, so only the manager's output order moves.
    sweep("shuffled", InlineSchedule::shuffled);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_survives_late_inputs() {
    let perturbed = sweep("late", InlineSchedule::late);
    assert!(perturbed > 0, "late inputs never changed the outcome — schedule is a no-op");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_survives_missed_ticks() {
    let perturbed = sweep("jittery", InlineSchedule::jittery);
    assert!(perturbed > 0, "missed ticks never changed the outcome — schedule is a no-op");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn missed_ticks_catch_up_to_world_tick() {
    use game::RegionSpawner;
    let mut rig = Rig::new(Some(InlineSchedule::jittery(7)));
    rig.event(ServerEvent::connected(CLIENT));
    for rc in SPAWN_REGION.window_3x3() {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), CLIENT));
    }
    for _ in 0..200 {
        rig.round();
        let world = rig.manager.spawner_mut().world_tick();
        for rc in rig.manager.running_regions() {
            rig.manager.spawner_mut().with_region(rc, |region| {
                // Consecutive skips may stack; the burst bound is what catch-up repairs.
                assert!(
                    world - region.current_tick() < u64::from(game::MAX_CATCH_UP_BURST),
                    "{rc:?} fell too far behind"
                );
            });
        }
    }
    rig.manager.spawner_mut().set_schedule(None);
    rig.round();
    let world = rig.manager.spawner_mut().world_tick();
    let running = rig.manager.running_regions();
    assert_eq!(running.len(), 9, "3x3 window running");
    for rc in running {
        rig.manager.spawner_mut().with_region(rc, |region| {
            assert_eq!(region.current_tick(), world, "{rc:?} caught up to the world tick");
        });
    }
}

// --- Adventure walks across a seam ------------------------------------------

/// The neighbour an adventurer walks into.
const EAST: RegionCoords = RegionCoords { x: 1, z: 0 };
/// Blocks between the adventurer's start and the +x seam, and between the
/// seam and the goal column.
const SEAM_GAP_BLOCKS: i32 = 6;
/// Block z of the walk (the spawn column's).
const WALK_Z_BLOCK: i32 = 8;
/// Round budget for the whole walk (~12 m at walk speed is ~60 lockstep
/// rounds; missed ticks and late inputs stretch it).
const ADVENTURE_ROUNDS: usize = 400;

/// A one-block floor over the spawn region's strip `z < 320` units, and the
/// first chunk column of the east neighbour beside it; far regions get the
/// chunk under the spawn column.
fn adventure_floor_columns(rc: RegionCoords) -> Vec<(ColumnCoords, Column)> {
    let floor = |(x, z): (usize, usize)| {
        let air = Block::new(BlockId::AIR);
        let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(game::BLOCK_VOXELS as u32)));
        (ColumnCoords::new(x, z), Column::new(0, air, air, vec![sub]))
    };
    let zs = 0.0..320.0;
    let xs = if rc == SPAWN_REGION {
        0.0..REGION_SIZE
    } else if rc == EAST {
        0.0..REGION_SIZE / 4.0
    } else {
        128.0..160.0
    };
    let zs = if rc == SPAWN_REGION || rc == EAST { zs } else { 128.0..160.0 };
    common::chunks_over(xs, zs).into_iter().map(floor).collect()
}

/// An adventurer `SEAM_GAP_BLOCKS` short of the +x seam is sent to a column
/// as far into the east neighbour: it must hand off, walk on along its path
/// there, and arrive. Panics with the seed on failure.
fn walk_into_east(seed: u64, schedule: InlineSchedule) {
    let mut rig = Rig::with_floor(Some(schedule), adventure_floor_columns);
    rig.event(ServerEvent::connected(CLIENT));
    for rc in SPAWN_REGION.window_3x3() {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), CLIENT));
    }
    rig.walk_until_owned_by(SPAWN_REGION, "spawn");
    let n = REGION_BLOCKS as i32;
    rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some([n - SEAM_GAP_BLOCKS, 1, WALK_Z_BLOCK]) });
    for _ in 0..PAST_SEAM_ROUNDS {
        rig.round();
    }
    let column = [SEAM_GAP_BLOCKS, WALK_Z_BLOCK];
    rig.order(Order::MoveTo(Goal::local(EAST, column, None)));
    for _ in 0..ADVENTURE_ROUNDS {
        rig.round();
        if rig.owners() == [EAST] {
            let (walker, tile) = rig.walk_state(EAST);
            // The goal is in the halo: one path straight to it, carried
            // over the seam and walked on — never a stuck re-plan.
            if let Some(w) = &walker {
                assert!(w.to_goal && !w.replanned, "seed {seed}: walk not straight to the goal: {w:?}");
            }
            if walker.is_none() {
                assert_eq!([tile[0], tile[2]], column, "seed {seed}: walk ended off the goal column");
                assert_eq!(rig.pushed_home, Some(EAST), "seed {seed}: client told about its new home");
                return;
            }
        }
    }
    let owners = rig.owners();
    let state = owners.first().map(|&rc| rig.walk_state(rc));
    panic!("seed {seed}: walk never arrived; owners {owners:?}, walk state {state:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walk_into_neighbour_region() {
    let modes: [fn(u64) -> InlineSchedule; 3] =
        [InlineSchedule::shuffled, InlineSchedule::late, InlineSchedule::jittery];
    for seed in 0..8u64 {
        let schedule = modes[seed as usize % modes.len()](seed);
        if std::panic::catch_unwind(|| walk_into_east(seed, schedule)).is_err() {
            panic!("adventure walk into {EAST:?} broke under seed {seed}");
        }
    }
}

// --- A history traveller crosses a seam ---------------------------------------

use crate::history_handover::{assert_one_place, bodies, floored_region, land_map, start, years_before};
use game::history::{History, Location, Purpose, Race, Sex, SiteId, SocietyGrid};

/// Where people land, in block columns: a few blocks short of the spawn region's −z seam.
const NORTH_LANDING: [u32; 2] = [32, 3];
/// Round budget for the traveller to walk over the seam and be delivered.
const TRAVEL_ROUNDS: usize = 400;

/// A traveller of the world history walks from a site in the spawn region over the −z
/// seam to a site in the running north region: one body throughout (none while in
/// flight), delivered and agreed on by the history. Panics with the seed on failure.
fn travel_north(seed: u64) {
    let mut map = land_map(3, 3);
    map.origin = RegionCoords::new(-1, -1);
    for t in &mut map.tiles {
        t.landing = Some(NORTH_LANDING);
    }
    let s = start();
    let mut h = History::new(seed, s, SocietyGrid::from_world_map(&map));
    let civ = h.add_civ(Race::Dwarf);
    let traveller = h.add_founder(Sex::Female, years_before(s, 20));
    let host = h.add_founder(Sex::Male, years_before(s, 20));
    h.found_site(civ, SPAWN_REGION, &[traveller], None, false);
    h.found_site(civ, north(), &[host], None, false);

    let mut rig = Rig::with_floor(Some(InlineSchedule::jittery(seed)), floored_region);
    rig.manager.set_history(h, rig.now_ms);
    rig.event(ServerEvent::connected(CLIENT));
    for rc in SPAWN_REGION.window_3x3() {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), CLIENT));
    }
    rig.walk_until_owned_by(SPAWN_REGION, "spawn");
    let j = rig
        .manager
        .history_mut()
        .unwrap()
        .start_journey(vec![traveller], SiteId(0), north(), Purpose::Migrate { to: SiteId(1) })
        .expect("a route north");
    let mut delivered = false;
    for round in 0..TRAVEL_ROUNDS {
        rig.round();
        rig.manager.maintain(rig.now_ms);
        rig.settle();
        let census = bodies(&mut rig.manager);
        for (p, at) in &census {
            assert!(at.len() <= 1, "seed {seed} round {round}: {p:?} has bodies in {at:?}");
        }
        let h = rig.manager.history().unwrap();
        let loc = h.person(traveller).unwrap().location;
        if !matches!(loc, Location::InRegion(_)) {
            panic!("seed {seed} round {round}: the traveller was absorbed ({loc:?})");
        }
        if loc == Location::InRegion(north()) && census.get(&traveller) == Some(&vec![north()]) {
            delivered = true;
            break;
        }
    }
    assert!(delivered, "seed {seed}: the traveller never reached {:?}", north());
    for _ in 0..PAST_SEAM_ROUNDS {
        rig.round();
        rig.manager.maintain(rig.now_ms);
        rig.settle();
    }
    let census = bodies(&mut rig.manager);
    let h = rig.manager.history().unwrap();
    assert_one_place(h, &census, &format!("seed {seed}: delivered"));
    assert!(h.journey(j).is_none(), "seed {seed}: the journey ended on arrival");
    let p = h.person(traveller).unwrap();
    assert_eq!((p.location, p.home_location), (Location::InRegion(north()), Location::AtSite(SiteId(1))));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn history_departure_under_jitter() {
    for seed in 0..8u64 {
        if std::panic::catch_unwind(|| travel_north(seed)).is_err() {
            panic!("history traveller across the seam broke under seed {seed}");
        }
    }
}

// --- A key held across a seam: inputs follow the body --------------------------

/// Rounds the key stays held once the player has crossed (3 s).
const HELD_AFTER_CROSSING_ROUNDS: usize = (3000 / TICK_RATE) as usize;
/// How far ahead of the region it believes it is in the scripted client
/// stamps its inputs (its prediction lead).
const INPUT_LEAD_TICKS: u64 = 2;

impl Rig {
    /// A client input stamped the way a client stamps it: with the region it
    /// believes it is in and that region's tick plus its lead.
    fn stamped(&mut self, kind: GameEventKind) {
        let home = self.pushed_home.unwrap_or(SPAWN_REGION);
        let mut tick = 0;
        self.manager.spawner_mut().with_region(home, |region| tick = region.current_tick());
        let ev = GameEvent::new(kind, 0, home);
        self.event(ServerEvent::ClientPacket(ClientPacket::GameEventAfterTick(ev, tick + INPUT_LEAD_TICKS), CLIENT));
    }
}

/// Holds W across the north seam and for 3 s beyond, a stamped marker input
/// every round. The player stays owned by `north()` from the crossing on, and
/// every marker is applied exactly once, in the region that held the body at
/// that moment — none lost to the region it just left.
fn hold_key_across_seam(schedule: Option<InlineSchedule>) {
    let mut rig = Rig::new(schedule);
    rig.event(ServerEvent::connected(CLIENT));
    for rc in SPAWN_REGION.window_3x3() {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), CLIENT));
    }
    rig.walk_until_owned_by(SPAWN_REGION, "spawn");
    rig.key(Key::KeyE, true);
    rig.key(Key::KeyE, false);
    rig.start_trace();
    rig.stamped(GameEventKind::PlayerInput(CLIENT, InputEvent::Key { key: Key::KeyW, pressed: true }));

    let mut seq = 0u32;
    let mut marker = |rig: &mut Rig| {
        seq += 1;
        rig.stamped(GameEventKind::PlayerInput(CLIENT, InputEvent::Resized { width: seq, height: 1 }));
    };
    let mut crossed = false;
    for _ in 0..LEG_ROUNDS {
        marker(&mut rig);
        rig.round();
        if rig.owners() == [north()] {
            crossed = true;
            break;
        }
    }
    assert!(crossed, "player never reached {:?}; owners {:?}", north(), rig.owners());
    for round in 0..HELD_AFTER_CROSSING_ROUNDS {
        marker(&mut rig);
        rig.round();
        assert_eq!(rig.owners(), [north()], "ownership left {:?} {round} rounds after the crossing", north());
    }
    // Let the last markers land.
    for _ in 0..PAST_SEAM_ROUNDS {
        rig.round();
    }
    let sent = seq;

    let (_, seen) = rig.trace.take().expect("tracing");
    let mut count = vec![0u32; sent as usize + 1];
    // The departure is reported after the tick it happened in, so find it first.
    let left_spawn_at = seen.iter().find_map(|(rc, s)| match s {
        Seen::Departed { stamp } if *rc == SPAWN_REGION => Some(*stamp),
        _ => None,
    });
    let mut arrived_north = false;
    for (rc, s) in &seen {
        match *s {
            Seen::Arrived if *rc == north() => arrived_north = true,
            Seen::Marker { seq, tick } => {
                count[seq as usize] += 1;
                if *rc == SPAWN_REGION {
                    assert!(
                        left_spawn_at.map_or(true, |d| tick < d),
                        "marker {seq} applied in the spawn region at tick {tick}, after the body left (at {left_spawn_at:?})"
                    );
                } else {
                    assert_eq!(*rc, north(), "marker {seq} applied in {rc:?}");
                    assert!(arrived_north, "marker {seq} applied in {rc:?} before the body arrived");
                }
            }
            _ => {}
        }
    }
    assert!(left_spawn_at.is_some() && arrived_north, "the crossing was traced: {seen:?}");
    for seq in 1..=sent {
        assert_eq!(count[seq as usize], 1, "marker {seq} applied {} times", count[seq as usize]);
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn held_key_across_seam_settles() {
    hold_key_across_seam(None);
    for seed in 0..SEEDS {
        if std::panic::catch_unwind(|| hold_key_across_seam(Some(InlineSchedule::jittery(seed)))).is_err() {
            panic!("held key across the seam broke under InlineSchedule::jittery({seed})");
        }
    }
}

// --- Crossing faults: lag, a double crossing, a parked or dead target ---------

/// Walking ticks short of the seam where the fault tests set their fault up.
const FAULT_LEAD_TICKS: f32 = 3.0;
/// Round budget for a held region to be released / a fault to play out.
const FAULT_ROUNDS: usize = 200;

/// A rig with the player standing in the spawn region, controls on, the 3x3
/// window subscribed.
fn rig_at_spawn(schedule: Option<InlineSchedule>) -> Rig {
    let mut rig = Rig::new(schedule);
    rig.event(ServerEvent::connected(CLIENT));
    for rc in SPAWN_REGION.window_3x3() {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), CLIENT));
    }
    rig.walk_until_owned_by(SPAWN_REGION, "spawn");
    rig.key(Key::KeyE, true);
    rig.key(Key::KeyE, false);
    rig
}

/// Hold W until the player is `FAULT_LEAD_TICKS` of walking short of the
/// north seam, still owned by the spawn region.
fn approach_north_seam(rig: &mut Rig) {
    use game::movement::{TICK_DT, UNITS_PER_METER, WALK_SPEED};
    rig.key(Key::KeyW, true);
    let margin = FAULT_LEAD_TICKS * WALK_SPEED * UNITS_PER_METER * TICK_DT;
    for _ in 0..LEG_ROUNDS {
        rig.round();
        if rig.owners() == [SPAWN_REGION] && rig.player_xz(SPAWN_REGION).1 < margin {
            return;
        }
    }
    panic!("player never got near the north seam; owners {:?}", rig.owners());
}

/// Rounds until the manager has the player's transfer in flight.
fn until_in_flight(rig: &mut Rig) {
    for _ in 0..FAULT_ROUNDS {
        rig.round();
        if rig.manager.in_flight_count() > 0 {
            return;
        }
    }
    panic!("no transfer went in flight; owners {:?}", rig.owners());
}

/// After the crossing: settled in `rc`, nothing left in flight.
fn assert_settled_in(rig: &mut Rig, rc: RegionCoords) {
    for _ in 0..PAST_SEAM_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.owners(), [rc]);
    assert_eq!(rig.pushed_home, Some(rc), "client told about its new home");
    assert_eq!(rig.manager.in_flight_count(), 0, "the arrival was confirmed");
}

/// The source region misses 5 rounds around the crossing (its thread fell
/// behind): it catches up in a burst, crossing on the way, and the transfer
/// is accounted for every round.
fn lagging_source(schedule: Option<InlineSchedule>) {
    let mut rig = rig_at_spawn(schedule);
    approach_north_seam(&mut rig);
    rig.manager.spawner_mut().hold(SPAWN_REGION, 5);
    rig.walk_until_owned_by(north(), "lagging source");
    assert_settled_in(&mut rig, north());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lagging_source_transfer_conserved() {
    lagging_source(None);
    for seed in 0..4 {
        if std::panic::catch_unwind(|| lagging_source(Some(InlineSchedule::jittery(seed)))).is_err() {
            panic!("lagging source broke under InlineSchedule::jittery({seed})");
        }
    }
}

/// A sprint diagonally through the -x/-z corner: two seams within a couple
/// of ticks, whichever order the schedule makes of them.
fn corner_sprint(schedule: Option<InlineSchedule>) {
    let mut rig = rig_at_spawn(schedule);
    for key in [Key::ControlLeft, Key::KeyW, Key::KeyA] {
        rig.key(key, true);
    }
    rig.walk_until_owned_by(north_west(), "corner sprint");
    for key in [Key::KeyW, Key::KeyA] {
        rig.key(key, false);
    }
    assert_settled_in(&mut rig, north_west());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn double_crossing_at_corner() {
    corner_sprint(None);
    for seed in 0..6 {
        if std::panic::catch_unwind(|| corner_sprint(Some(InlineSchedule::jittery(seed)))).is_err() {
            panic!("corner sprint broke under InlineSchedule::jittery({seed})");
        }
    }
}

/// The target is stopping (Shutdown sent, `Stopped` not back) when the
/// departure arrives: the transfer waits for it to park, wakes it again,
/// and the body is there for the resubscribe.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn park_target_mid_transfer() {
    let mut rig = rig_at_spawn(None);
    approach_north_seam(&mut rig);
    rig.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(north()), CLIENT));
    rig.now_ms += game::UNLOAD_GRACE_MS;
    rig.manager.maintain(rig.now_ms);
    // The Shutdown sits unread in north's queue until released.
    rig.manager.spawner_mut().hold(north(), u32::MAX);
    until_in_flight(&mut rig);
    assert!(rig.owners().is_empty(), "the body is in flight: {:?}", rig.owners());
    rig.manager.spawner_mut().hold(north(), 0);
    rig.walk_until_owned_by(north(), "after the target parked");
    rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(north()), CLIENT));
    assert_eq!(rig.snapshot_has_player.get(&north()), Some(&true), "the resubscribe sees the body");
    assert_settled_in(&mut rig, north());
}

/// The target's thread dies with the arrival unread: the respawned target is
/// sent the transfer again, and the body exists exactly once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn dead_target_redelivers_in_flight() {
    let mut rig = rig_at_spawn(None);
    approach_north_seam(&mut rig);
    rig.manager.spawner_mut().hold(north(), u32::MAX);
    until_in_flight(&mut rig);
    rig.kill(north());
    // Player input goes to the new home: the send finds the thread dead.
    rig.key(Key::KeyW, false);
    assert!(rig.manager.running_regions().contains(&north()), "respawned");
    rig.walk_until_owned_by(north(), "after the respawn");
    assert_settled_in(&mut rig, north());
}

/// One autosave at a time: the next is requested once every region asked
/// for the last has answered, and then the last is checked.
struct SaveCheck {
    id: EntityId,
    requested: bool,
    got: BTreeMap<RegionCoords, game::RegionSave>,
    checked: usize,
}

impl SaveCheck {
    fn new(rig: &mut Rig) -> Self {
        let mut id = None;
        rig.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
            let data = region.data();
            id = *data.ecs.id.try_get(*data.player_entites.get(&CLIENT).unwrap());
        });
        Self { id: id.expect("the player has an EntityId"), requested: false, got: BTreeMap::new(), checked: 0 }
    }

    fn round(&mut self, rig: &mut Rig) {
        self.got.extend(rig.manager.take_saves());
        if !rig.manager.awaiting_save().is_empty() {
            return;
        }
        if self.requested {
            let mut at = Vec::new();
            for (rc, save) in std::mem::take(&mut self.got) {
                let (state, transfers) = match save {
                    game::RegionSave::Live(state, transfers) => (*state, transfers),
                    parked => {
                        let saved = parked.into_serialized().to_saved().expect("parked blob decodes");
                        (saved.state, saved.transfers)
                    }
                };
                if state.id_index.contains_key(&self.id) {
                    at.push((rc, "state"));
                }
                at.extend(transfers.iter().filter(|(b, _)| b.id == self.id).map(|_| (rc, "transfer")));
            }
            assert_eq!(at.len(), 1, "save {}: the player is saved in {at:?}", self.checked);
            self.checked += 1;
        }
        self.got.clear();
        self.requested = true;
        rig.manager.request_save();
    }
}

/// Autosaves through the crossing, one after another (each once every
/// region asked has answered): the player's `EntityId` is in exactly one of
/// them each time — in a saved state, or as a transfer saved with its
/// target (or, when the target answered before the departure, rebased back
/// into its source).
fn saves_through_crossing(schedule: Option<InlineSchedule>, lagging: Option<RegionCoords>) {
    let mut rig = rig_at_spawn(schedule);
    let mut saves = SaveCheck::new(&mut rig);
    use game::movement::{TICK_DT, UNITS_PER_METER, WALK_SPEED};
    let margin = FAULT_LEAD_TICKS * WALK_SPEED * UNITS_PER_METER * TICK_DT;
    rig.key(Key::KeyW, true);
    let mut lagging = lagging;
    let mut settled_at = None;
    for round in 0..LEG_ROUNDS {
        saves.round(&mut rig);
        if rig.owners() == [SPAWN_REGION] && rig.player_xz(SPAWN_REGION).1 < margin {
            if let Some(rc) = lagging.take() {
                rig.manager.spawner_mut().hold(rc, 4);
            }
        }
        rig.round();
        if rig.owners() == [north()] && rig.manager.in_flight_count() == 0 {
            let at = *settled_at.get_or_insert(round);
            if round > at + 12 {
                saves.round(&mut rig);
                assert!(saves.checked > 10, "only {} saves checked", saves.checked);
                return;
            }
        }
    }
    panic!("player never settled in {:?}; owners {:?}", north(), rig.owners());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn autosave_holds_each_transfer_exactly_once() {
    saves_through_crossing(None, None);
    saves_through_crossing(None, Some(north()));
    saves_through_crossing(None, Some(SPAWN_REGION));
    for seed in 0..SEEDS {
        if std::panic::catch_unwind(|| saves_through_crossing(Some(InlineSchedule::jittery(seed)), None)).is_err() {
            panic!("autosave through a crossing broke under InlineSchedule::jittery({seed})");
        }
    }
}

/// Autosaves while the crossing waits on a stopping target: the body departs
/// in one save round towards a region that is mid-stop (never asked to save),
/// the next round starts before that region has parked, and only then is it
/// woken with the body. Every round's saves still hold the player once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn autosave_holds_a_transfer_to_a_stopping_target() {
    let mut rig = rig_at_spawn(None);
    approach_north_seam(&mut rig);
    rig.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(north()), CLIENT));
    rig.now_ms += game::UNLOAD_GRACE_MS;
    rig.manager.maintain(rig.now_ms);
    rig.manager.spawner_mut().hold(north(), u32::MAX);
    let mut saves = SaveCheck::new(&mut rig);
    let mut in_flight_rounds = 0;
    for _ in 0..FAULT_ROUNDS {
        saves.round(&mut rig);
        rig.round();
        if rig.manager.in_flight_count() > 0 {
            in_flight_rounds += 1;
            // Several save rounds pass while the target is still stopping.
            if in_flight_rounds == 4 {
                break;
            }
        }
    }
    assert_eq!(in_flight_rounds, 4, "the crossing never went in flight");
    let checked_in_flight = saves.checked;
    rig.manager.spawner_mut().hold(north(), 0);
    for _ in 0..PAST_SEAM_ROUNDS * 2 {
        saves.round(&mut rig);
        rig.round();
    }
    saves.round(&mut rig);
    assert_eq!(rig.owners(), [north()]);
    assert!(saves.checked > checked_in_flight + 2, "saves checked after the wake: {}", saves.checked);
}

// --- Seam-translation invariance ----------------------------------------------

use crate::common::seam_translate::{
    assert_seam_invariant, offset, spawn_tile, Scenario, Tolerances, JITTER_TOLERANCES,
};
use game::WorldBlockPos;

/// Rounds a freshly placed body is left to land before a scenario starts.
const PLACE_ROUNDS: usize = 6;

/// Puts the client's body on `at` (a world block in the spawn region) by
/// entering Adventure there; `fortress` then switches back to Fortress mode
/// with key control on (the held-key walk's mode).
fn place_player(rig: &mut Rig, at: WorldBlockPos, fortress: bool) {
    rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some(spawn_tile(at)) });
    if fortress {
        rig.order(Order::SetMode { mode: PlayMode::Fortress, at: None });
        rig.key(Key::KeyE, true);
        rig.key(Key::KeyE, false);
    }
    for _ in 0..PLACE_ROUNDS {
        rig.round();
    }
}

/// `walk_there_and_back` on the harness: W+A held for a leg (toward −z/−x,
/// through the origin), then S+D for a leg back, then released.
struct KeyWalk;

/// Blocks from the origin, along +x and +z, the key walk starts at.
const KEY_WALK_START_BLOCKS: i64 = 6;
/// Rounds each leg's keys are held (~13 blocks of diagonal walking).
const KEY_WALK_LEG_ROUNDS: u32 = 60;
/// Rounds after the release, for the body to stop.
const KEY_WALK_STOP_ROUNDS: u32 = 20;

impl Scenario for KeyWalk {
    fn setup(&self, rig: &mut Rig, origin: WorldBlockPos) {
        let d = KEY_WALK_START_BLOCKS;
        place_player(rig, offset(origin, d, 0, d), true);
    }

    fn drive(&self, rig: &mut Rig, round: u32) {
        let (out, back) = ([Key::KeyW, Key::KeyA], [Key::KeyS, Key::KeyD]);
        if round == 0 {
            out.iter().for_each(|&k| rig.key(k, true));
        } else if round == KEY_WALK_LEG_ROUNDS {
            out.iter().for_each(|&k| rig.key(k, false));
            back.iter().for_each(|&k| rig.key(k, true));
        } else if round == 2 * KEY_WALK_LEG_ROUNDS {
            back.iter().for_each(|&k| rig.key(k, false));
        }
    }

    fn rounds(&self) -> u32 {
        2 * KEY_WALK_LEG_ROUNDS + KEY_WALK_STOP_ROUNDS
    }
}

/// An adventurer sent with `Order::MoveTo` diagonally through the origin.
struct AdventureWalk;

/// Blocks from the origin, along each axis, the adventurer starts (+x/+z)
/// and stops (−x/−z) at.
const ADVENTURE_WALK_BLOCKS: i64 = 8;
/// Round budget of the walk (lockstep arrives in ~60; jitter stretches it).
const ADVENTURE_WALK_ROUNDS: u32 = 160;

impl Scenario for AdventureWalk {
    fn setup(&self, rig: &mut Rig, origin: WorldBlockPos) {
        let d = ADVENTURE_WALK_BLOCKS;
        place_player(rig, offset(origin, d, 0, d), false);
        let goal = offset(origin, -d, 0, -d);
        rig.order(Order::MoveTo(Goal { column: [goal.x, goal.z], y: None }));
    }

    fn drive(&self, _rig: &mut Rig, _round: u32) {}

    fn rounds(&self) -> u32 {
        ADVENTURE_WALK_ROUNDS
    }

    /// A walk ends on its goal tile whatever the lag: tiles stay exact.
    fn jitter_tolerances(&self) -> Tolerances {
        Tolerances { tiles: true, ..JITTER_TOLERANCES }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walk_is_seam_invariant() {
    let end = assert_seam_invariant(&KeyWalk, None);
    let p = end.player();
    assert!(p.walk_goal.is_none());
    // There and back: it ends near where it started, having crossed the origin.
    let d = KEY_WALK_START_BLOCKS as f32;
    assert!((p.pos[0] - d).abs() < 2.0 && (p.pos[2] - d).abs() < 2.0, "ended at {:?}", p.pos);
    assert_seam_invariant(&KeyWalk, Some(InlineSchedule::jittery));
}

/// A walk split by a seam takes the centred run's path: it is planned over
/// the region ∪ halo in world tiles and walked on after the hand-off
/// (`path_plan.rs`). Before that it stopped at an edge tile, steered a block
/// past it and re-planned on arrival, resting 0.17 blocks off at Seam(X).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn adventure_walk_is_seam_invariant() {
    assert_seam_invariant(&AdventureWalk, None);
}

/// The walk's discrete outcome — the goal tile, the walk over — holds in
/// every placement under missed ticks and late inputs.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn adventure_walk_reaches_its_goal_in_every_placement() {
    let end = assert_seam_invariant(&AdventureWalk, Some(InlineSchedule::jittery));
    let p = end.player();
    let d = ADVENTURE_WALK_BLOCKS;
    assert_eq!(p.walk_goal, None, "the walk finished");
    assert_eq!(p.tile, [-d, 0, -d], "the walk ended on its goal (relative to the origin)");
}

// --- A tumbling body crosses a seam ---------------------------------------------

use crate::common::seam_translate::{
    compare, run, spread, Axis, EntitySnap, Placement, WorldSnapshot, JITTER_SEEDS, LOCKSTEP_TOLERANCES, PLACEMENTS,
};
use game::rapier3d::math::Vector;

/// Horizontal launch speed, m/s, diagonally toward −x/−z (through the
/// placement's origin: across the seam, or both seams at the corner).
const BODY_LAUNCH_SPEED_M_PER_S: f32 = 8.0;
/// Spin about Y at launch, rad/s.
const BODY_SPIN_RAD_PER_S: f32 = 6.0;
/// Rounds of a launch: the flight and slide (< 1 s), then rapier's stillness
/// before a body sleeps, with room for jitter's delays.
const BODY_LAUNCH_ROUNDS: u32 = 100;
/// Blocks from the origin, along +x and +z, the player stands at: behind the
/// launch, never touched by the body.
const BODY_PLAYER_BLOCKS: i64 = 8;

/// Rest position tolerance of a tossed body against the centred lockstep
/// run, per axis, blocks (1 block = 1 m; the October start value was 0.02).
/// The toss crosses the seam in the air, lands about a block past it and
/// rests 1.3 blocks past it, clear of every chunk collider edge, so the
/// handoff carries a body with no contacts and only float rounding differs. Measured ×2: the largest spread
/// (2026-11-06, REGION_BLOCKS 64, CHUNK_BLOCKS 16) was 1.14e-5 blocks
/// (Seam(Z)), lockstep and under the shuffled / late / jittery schedules
/// alike (4 seeds each, every placement): a late arrival only pauses the body
/// in the air for the ticks its target is behind, which leaves its path in
/// space unchanged.
const BODY_REST_POS_TOL: f32 = 2.3e-5;
/// … and rest rotation, degrees (October start value 1°). Measured: 0° (the
/// quaternions agree to f32 rounding) in every run above.
const BODY_REST_ANGLE_TOL: f32 = 0.01;

/// A 1 m cube launched toward −x/−z from `start` blocks (each axis) past the
/// placement's origin, `lift` blocks above the floor and `up` m/s upward,
/// spinning about Y.
#[derive(Clone, Copy)]
struct BodyLaunch {
    start: f32,
    lift: f32,
    up: f32,
}

/// Tossed: crosses the seam in flight, lands about a block past it.
const TOSS: BodyLaunch = BodyLaunch { start: 1.0, lift: 1.5, up: 4.0 };
/// Slid along the floor from half a block short of the seam.
const SLIDE: BodyLaunch = BodyLaunch { start: 0.5, lift: 0.0, up: 0.0 };

impl Scenario for BodyLaunch {
    fn setup(&self, rig: &mut Rig, origin: WorldBlockPos) {
        let b = game::BLOCK_VOXELS as f32;
        // The player first: its placement runs rounds.
        let p = BODY_PLAYER_BLOCKS;
        place_player(rig, offset(origin, p, 0, p), false);
        // Resting on the floor (its bottom at the stand level), a hair above
        // it, then lifted.
        let y = origin.y as f32 * b + b / 2.0 + 0.01 + self.lift * b;
        let d = self.start * b;
        let local = Vector::new(
            origin.x as f32 * b + d - SPAWN_REGION.x as f32 * REGION_SIZE,
            y,
            origin.z as f32 * b + d - SPAWN_REGION.z as f32 * REGION_SIZE,
        );
        let units = game::movement::UNITS_PER_METER;
        let v = BODY_LAUNCH_SPEED_M_PER_S * units / std::f32::consts::SQRT_2;
        let linvel = Vector::new(-v, self.up * units, -v);
        let angvel = Vector::new(0.0, BODY_SPIN_RAD_PER_S, 0.0);
        let cube = game::ColliderSpec::Cuboid { half_extents: Vector::splat(b / 2.0) };
        rig.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
            region.with_data(|d| {
                d.spawn_body_safe(game::Pose::from_translation(local), cube, linvel, angvel);
            });
        });
    }

    fn drive(&self, _rig: &mut Rig, _round: u32) {}

    fn rounds(&self) -> u32 {
        BODY_LAUNCH_ROUNDS
    }

    fn crosser(&self) -> Option<game::EntityKind> {
        Some(game::EntityKind::Body)
    }

    fn lockstep_tolerances(&self) -> Tolerances {
        Tolerances { pose_blocks: BODY_REST_POS_TOL, angle_deg: BODY_REST_ANGLE_TOL, ..LOCKSTEP_TOLERANCES }
    }
}

/// The body in a snapshot (the only `EntityKind::Body`).
fn the_body(s: &WorldSnapshot) -> &EntitySnap {
    let mut bodies = s.entities.values().filter(|e| e.kind == game::EntityKind::Body);
    let b = bodies.next().expect("the body");
    assert!(bodies.next().is_none(), "one body");
    b
}

/// The body of `snap` against `reference`'s (the lockstep centred run's)
/// within `tol`, asleep: returns its (pose, rotation) spread, logged.
fn assert_body_matches(reference: &EntitySnap, snap: &WorldSnapshot, tol: &Tolerances, what: &str) -> (f32, f32) {
    let body = the_body(snap);
    assert!(body.sleeping, "{what}: asleep");
    let only = |b: &EntitySnap| WorldSnapshot {
        entities: [(game::EntityId { birth_region: SPAWN_REGION, serial: u64::MAX }, b.clone())].into(),
        terrain: Default::default(),
        piles: Default::default(),
        containers: Default::default(),
    };
    let (pose, _, turn) = spread(&only(reference), &only(body));
    eprintln!("[body] {what}: pose spread {pose} blocks, rotation {turn}°");
    if let Err(e) = compare(&only(reference), &only(body), tol) {
        panic!("{what}: the body differs from lockstep Centre: {e}");
    }
    (pose, turn)
}

/// The toss in every placement (centred, across each straight seam —
/// Seam(Z) into `north()` — and through the corner into `north_west()`)
/// ends at the centred run's rest pose, asleep, owned by the region it rests
/// in. `run` asserts the body reached the region past the seam or corner and
/// was never owned twice in a round, `snapshot` that its owner is the region
/// it lies in, and `Rig::round` that it was in exactly one place every round.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn body_toss_is_seam_invariant() {
    let end = assert_seam_invariant(&TOSS, None);
    let body = the_body(&end);
    assert!(body.sleeping, "the body comes to rest asleep: {body:?}");
    assert!(body.pos[0] < -0.25 && body.pos[2] < -0.25, "rests past the origin: {:?}", body.pos);
    for (placement, owner) in [(Placement::Seam(Axis::Z), north()), (Placement::Corner, north_west())] {
        let s = run(&TOSS, placement, None);
        let o = placement.origin();
        let p = the_body(&s).pos;
        let world = [o.x as f32 + p[0], o.z as f32 + p[2]];
        let n = REGION_BLOCKS as f32;
        let at = RegionCoords::new((world[0] / n).floor() as i32, (world[1] / n).floor() as i32);
        assert_eq!(at, owner, "{placement:?}: rests in (and so is owned by) {owner:?}");
    }
}

/// The toss under each schedule mode: per seed and placement it is
/// reproducible (the same snapshot, bit for bit), ends with the lockstep
/// centred run's entities, tiles and walks (the player within
/// `JITTER_TOLERANCES`: its landing from the placement is lag-sensitive),
/// and its body asleep at the centred rest pose within the body tolerances;
/// every round the body is in exactly one place (`Rig::round`).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn body_toss_survives_jitter() {
    let reference = run(&TOSS, Placement::Centre, None);
    let body_ref = the_body(&reference).clone();
    let body_tol = TOSS.lockstep_tolerances();
    let modes: [(&str, fn(u64) -> InlineSchedule); 3] =
        [("shuffled", InlineSchedule::shuffled), ("late", InlineSchedule::late), ("jittery", InlineSchedule::jittery)];
    let mut worst = (0.0f32, 0.0f32);
    for (mode, schedule) in modes {
        for seed in 0..JITTER_SEEDS {
            for placement in PLACEMENTS {
                let what = format!("{mode} seed {seed} {placement:?}");
                let a = run(&TOSS, placement, Some(schedule(seed)));
                assert_eq!(a, run(&TOSS, placement, Some(schedule(seed))), "{what} is not reproducible");
                let tol = Tolerances { tiles: true, ..JITTER_TOLERANCES };
                if let Err(e) = compare(&reference, &a, &tol) {
                    panic!("{what} differs from lockstep Centre: {e}");
                }
                let (pose, turn) = assert_body_matches(&body_ref, &a, &body_tol, &what);
                worst = (worst.0.max(pose), worst.1.max(turn));
            }
        }
    }
    eprintln!("[body-jitter] worst: pose {} blocks, rotation {}°", worst.0, worst.1);
}

/// [`SLIDE`] with its body re-arrived in its OWN region (extracted and
/// arrived again, as a seam handoff does, no seam involved) in the round its
/// centre passes `FLIP_HYSTERESIS` beyond the origin — when the seam
/// placements hand it off. It measures what a handoff costs a body in
/// contact: its contact state (warm-start impulses, manifolds) is not in
/// the bundle, so it restarts from none.
struct SlideReArrived {
    origin: std::cell::Cell<Option<WorldBlockPos>>,
    done: std::cell::Cell<bool>,
}

impl Scenario for SlideReArrived {
    fn setup(&self, rig: &mut Rig, origin: WorldBlockPos) {
        self.origin.set(Some(origin));
        SLIDE.setup(rig, origin);
    }

    fn drive(&self, rig: &mut Rig, _round: u32) {
        if self.done.get() {
            return;
        }
        let origin = self.origin.get().expect("set up");
        let b = game::BLOCK_VOXELS as f32;
        let rc = rig.owners_of(game::EntityKind::Body)[0];
        rig.manager.spawner_mut().with_region(rc, |region| {
            region.with_data(|d| {
                let key = *d
                    .data
                    .id_index
                    .values()
                    .find(|k| *d.data.ecs.kind.try_get(**k) == Some(game::EntityKind::Body))
                    .expect("the body");
                let t = d.data.physics.bodies.get(d.data.ecs.rigidbody.try_get(key).unwrap()).unwrap().translation();
                let past = |local: f32, region: i32, block: i64| {
                    region as f32 * REGION_SIZE + local - block as f32 * b < -game::FLIP_HYSTERESIS
                };
                if past(t.x, rc.x, origin.x) || past(t.z, rc.z, origin.z) {
                    let bundle = d.extract_entity_safe(key);
                    d.apply_arrival(bundle).expect("re-arrived");
                    self.done.set(true);
                }
            });
        });
    }

    fn rounds(&self) -> u32 {
        SLIDE.rounds()
    }

    fn crosser(&self) -> Option<game::EntityKind> {
        SLIDE.crosser()
    }
}

/// Rest position tolerance of the floor SLIDE against the centred lockstep
/// run, per axis, blocks — loose on purpose, and measured ×2 (2026-11-06,
/// REGION_BLOCKS 64, CHUNK_BLOCKS 16): lockstep Seam(X) 0.0388, Seam(Z)
/// 0.0158, Corner 0.0124; the jittery seeds 0–3 the same in every
/// placement (the slide is over before jitter shifts anything); the
/// [`SlideReArrived`] control (the handoff's contact-state reset with no
/// seam) 0.0024. The seam spreads exceed the control: before any handoff, a
/// cube touching the seam already starts differently, because its floor
/// there is the neighbour's halo chunk, a collider inserted after the
/// region's own, so the contact pairs are solved in another order (first
/// tick spin 2.68 vs 1.41 rad/s centred) — and the chunk-edge ghost contacts
/// amplify any such difference (seam-free: 0.14 blocks, see the test).
const SLIDE_POSE_TOL_BLOCKS: f32 = 0.078;
/// … and rest rotation, degrees. Measured ×2: lockstep Seam(X) 6.96°,
/// Corner 2.31°, Seam(Z) 0.26°; jittery seeds the same; the control 0°.
const SLIDE_ANGLE_TOL_DEG: f32 = 14.0;

/// The slide along the floor through the seam — the ground-contact
/// crossing (a rolling log, a pushed crate). It crosses, is owned by one
/// region at a time, ends asleep past the origin in the region it lies in,
/// and rests within [`SLIDE_POSE_TOL_BLOCKS`] / [`SLIDE_ANGLE_TOL_DEG`] of the
/// centred run, lockstep and under jittery seeds. Those bounds are far
/// looser than the toss's, for two measured reasons: the handoff drops the
/// body's contact state (the [`SlideReArrived`] control, run here, measures
/// it with no seam), and a body sliding over the boundary between two
/// terrain colliders (every chunk edge, so every region seam) catches on the
/// far collider's edge — "ghost" contacts, each chunk's `Voxels` shape knows
/// nothing of its neighbour — which makes the slide chaotic with no region
/// seam at all: the same slide centred at block 16 and at block 32 (identical
/// geometry, only the coordinates' float rounding differs) rests 0.14 blocks
/// apart, and other launch speeds tip the cube onto another face at a seam
/// or stop it dead at a chunk edge. A handoff regression beyond that cost
/// (a lost velocity, weight or spin) still fails.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn body_slide_is_seam_invariant_within_its_handoff_cost() {
    let reference = run(&SLIDE, Placement::Centre, None);
    let body_ref = the_body(&reference).clone();
    assert!(body_ref.pos[0] < -0.25 && body_ref.pos[2] < -0.25, "rests past the origin: {:?}", body_ref.pos);
    let tol = Tolerances { pose_blocks: SLIDE_POSE_TOL_BLOCKS, angle_deg: SLIDE_ANGLE_TOL_DEG, ..LOCKSTEP_TOLERANCES };
    let control = SlideReArrived { origin: Default::default(), done: Default::default() };
    let snap = run(&control, Placement::Centre, None);
    assert!(control.done.get(), "the control re-arrived the body");
    let mut worst = assert_body_matches(&body_ref, &snap, &tol, "control: re-arrived at Centre");
    let mut check = |snap: &WorldSnapshot, what: &str| {
        let (pose, turn) = assert_body_matches(&body_ref, snap, &tol, what);
        let body = the_body(snap);
        assert!(body.pos[0] < -0.25 && body.pos[2] < -0.25, "{what}: rests past the origin: {:?}", body.pos);
        worst = (worst.0.max(pose), worst.1.max(turn));
    };
    for placement in &PLACEMENTS[1..] {
        let snap = run(&SLIDE, *placement, None);
        if let Err(e) = compare(&reference, &snap, &tol) {
            panic!("lockstep {placement:?} differs from Centre: {e}");
        }
        check(&snap, &format!("lockstep {placement:?}"));
    }
    for seed in 0..JITTER_SEEDS {
        for placement in PLACEMENTS {
            let snap = run(&SLIDE, placement, Some(InlineSchedule::jittery(seed)));
            if let Err(e) = compare(&reference, &snap, &Tolerances { tiles: true, ..JITTER_TOLERANCES }) {
                panic!("jittery seed {seed} {placement:?} differs from lockstep Centre: {e}");
            }
            check(&snap, &format!("jittery seed {seed} {placement:?}"));
        }
    }
    eprintln!("[body] slide worst: pose {} blocks, rotation {}°", worst.0, worst.1);
}

// --- A tree felled across a seam ------------------------------------------------

/// The felled tree's id (any non-zero id; the floor holds no other tree).
const FELLED_TREE_ID: u32 = 7;
/// Its trunk, in logs above the floor: tall enough (`TALL_PART_BLOCKS`) to
/// topple over its butt, and to lay its centre of mass down past the seam.
const FELLED_TRUNK_BLOCKS: i32 = 12;
/// Blocks from the origin, along +x and +z, of the trunk: 2 from the seam.
const FELLED_TRUNK_OFFSET: i64 = 2;
/// Rounds for the fall and the rest that settles it (`REST_TICKS` still
/// ticks), well under its `SETTLE_TIMEOUT_TICKS`, with room for jitter.
const FELLED_ROUNDS: u32 = 120;

/// A tree 2 blocks short of the seam (the corner, at `Corner`) leaning
/// toward −x/−z across it: an oak trunk with a limb of logs and a few
/// leaves reaching over the origin. Setup chops it at the floor as an
/// Adventure player does (`player_chop_safe`: the cut drops at the stump,
/// the rest falls as a `DropItems` voxel body). It tips along its lean,
/// crosses the seam as an `EntityKind::Body` while it falls, and its logs
/// are put down where it rests, by the region that then owns it.
struct FelledTree;

impl FelledTree {
    /// The tree's blocks, (offset from the origin — on the stand level —,
    /// block, limb).
    fn blocks(log: BlockId, leaves: BlockId) -> Vec<([i64; 3], BlockId, u8)> {
        let t = FELLED_TRUNK_OFFSET;
        let mut out: Vec<([i64; 3], BlockId, u8)> =
            (0..FELLED_TRUNK_BLOCKS).map(|y| ([t, y as i64, t], log, 0)).collect();
        let top = FELLED_TRUNK_BLOCKS as i64 - 1;
        for (d, dy) in [(1, -3), (1, -2), (2, -2), (2, -1), (1, -1)] {
            out.push(([t - d, top + dy, t - d], log, 1));
        }
        for (d, dy) in [(2, 0), (1, 0)] {
            out.push(([t - d, top + dy, t - d], leaves, 1));
        }
        out
    }

    fn ids() -> (BlockId, BlockId) {
        let reg = game::BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
        (reg.id_of("oak_log").unwrap(), reg.id_of("oak_leaves").unwrap())
    }

    /// Every item the tree is worth: the drops of its blocks.
    fn worth() -> BTreeMap<game::ItemId, u32> {
        common::install_real_block_props();
        let (log, leaves) = Self::ids();
        let mut out = BTreeMap::new();
        for (_, id, limb) in Self::blocks(log, leaves) {
            if let Some(s) = game::drop_of(Block::with_tree(id, FELLED_TREE_ID, limb)) {
                *out.entry(s.item).or_default() += s.count;
            }
        }
        out
    }
}

impl Scenario for FelledTree {
    fn setup(&self, rig: &mut Rig, origin: WorldBlockPos) {
        common::install_real_block_props();
        let p = BODY_PLAYER_BLOCKS;
        place_player(rig, offset(origin, p, 0, p), false);
        let (log, leaves) = Self::ids();
        let blocks = Self::blocks(log, leaves);
        let cut = spawn_tile(offset(origin, FELLED_TRUNK_OFFSET, 0, FELLED_TRUNK_OFFSET));
        rig.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
            region.with_data(|d| {
                for (o, id, limb) in &blocks {
                    let at = spawn_tile(offset(origin, o[0], o[1] as i32, o[2]));
                    assert!(d.set_block_safe(at, Block::with_tree(*id, FELLED_TREE_ID, *limb)), "tree block at {at:?}");
                }
                let tick = *d.data.tick;
                assert!(d.player_chop_safe(cut, tick), "the chop fells the tree");
            });
        });
    }

    fn drive(&self, _rig: &mut Rig, _round: u32) {}

    fn rounds(&self) -> u32 {
        FELLED_ROUNDS
    }

    fn crosser(&self) -> Option<game::EntityKind> {
        Some(game::EntityKind::Body)
    }
}

/// Every pile's items summed per item.
fn pile_totals(s: &WorldSnapshot) -> BTreeMap<game::ItemId, u32> {
    let mut out = BTreeMap::new();
    for stack in s.piles.values().flatten() {
        *out.entry(stack.item).or_default() += stack.count;
    }
    out
}

/// A tree felled 2 blocks from a seam, leaning across it, falls across it
/// in every placement: the body crosses (`run` asserts it was owned past the
/// seam / corner, and by one region at a time), settles there, and its logs
/// lie in piles owned by the regions they lie in (`snapshot` asserts it),
/// past the origin — every item the tree was worth, once (the stump's pile
/// included), lockstep and under jittery seeds. The piles' cells are the
/// discrete outcome compared against the centred run; see the body below.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tree_felled_across_seam() {
    let worth = FelledTree::worth();
    assert!(!worth.is_empty(), "the tree is worth something");
    let reference = run(&FelledTree, Placement::Centre, None);
    let check = |snap: &WorldSnapshot, what: &str| {
        assert!(
            !snap.entities.values().any(|e| e.kind == game::EntityKind::Body),
            "{what}: the felled part settled"
        );
        assert_eq!(pile_totals(snap), worth, "{what}: every item, once: {:?}", snap.piles);
        let past = snap.piles.keys().filter(|c| c[0] < 0 && c[2] < 0).count();
        assert!(past > 0, "{what}: logs lie past the origin: {:?}", snap.piles);
    };
    check(&reference, "lockstep Centre");
    for placement in &PLACEMENTS[1..] {
        let snap = run(&FelledTree, *placement, None);
        check(&snap, &format!("lockstep {placement:?}"));
        if let Err(e) = compare(&reference, &snap, &LOCKSTEP_TOLERANCES) {
            panic!("lockstep {placement:?} differs from Centre: {e}");
        }
    }
    for seed in 0..JITTER_SEEDS {
        for placement in PLACEMENTS {
            let what = format!("jittery seed {seed} {placement:?}");
            let snap = run(&FelledTree, placement, Some(InlineSchedule::jittery(seed)));
            check(&snap, &what);
            if let Err(e) = compare(&reference, &snap, &Tolerances { tiles: true, ..JITTER_TOLERANCES }) {
                panic!("{what} differs from lockstep Centre: {e}");
            }
        }
    }
}
