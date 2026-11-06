//! Boundary handoff under scheduling jitter. `InlineSpawner`'s default is one
//! fixed schedule (every region ticks once per round, every output lands
//! before the neighbour's next tick); real region threads each run their own
//! timer. `InlineSchedule` replays that jitter from a seed — permuted tick
//! order, inputs that arrive a tick late, missed ticks — over a straight seam
//! crossing and a diagonal one through a region corner, so a schedule that
//! breaks the handoff is a seed to replay rather than a flaky thread test
//! (`crates/server/tests/threaded_world.rs` keeps the real-thread plumbing
//! check). Run via `just test-threads`.
use crossbeam::channel::{unbounded, Receiver};
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, GameEvent, GameEventKind,
    Goal, InlineLodBuilder, InlineSchedule, InlineSpawner, InputEvent, Key, LodBuilder, Order, PlayMode,
    RegionCoords, RegionOutput, ServerEvent, ServerPacket, SubChunk, TilePos, Walker, WorldManager,
    REGION_BLOCKS, REGION_SIZE, SPAWN_REGION, TICK_RATE,
};

use crate::common;
use game::EntityId;
use std::collections::BTreeMap;

/// The conservation invariant of a crossing: every `EntityId` is owned by
/// exactly one running region or sits exactly once in the manager's
/// in-flight table — never both, never twice, and (people aside, whom the
/// history may absorb or end) never lost. Called every round.
fn assert_conserved(rig: &mut Rig) {
    let mut places: BTreeMap<EntityId, Vec<Option<RegionCoords>>> = BTreeMap::new();
    let mut people = std::collections::BTreeSet::new();
    for rc in rig.manager.running_regions() {
        rig.manager.spawner_mut().with_region(rc, |region| {
            let data = region.data();
            for (id, &key) in data.id_index.iter() {
                places.entry(*id).or_default().push(Some(rc));
                if matches!(data.ecs.kind.try_get(key), Some(game::EntityKind::Person { .. })) {
                    people.insert(*id);
                }
            }
        });
    }
    for (id, _target) in rig.manager.in_flight_ids() {
        places.entry(id).or_default().push(None);
    }
    for (id, at) in &places {
        assert_eq!(at.len(), 1, "{id:?} is in several places at once (None = in flight): {at:?}");
    }
    for (id, last) in &rig.tracked {
        assert!(places.contains_key(id), "{id:?} vanished (last seen {last:?}, None = in flight)");
    }
    rig.tracked.retain(|id, _| !people.contains(id));
    for (id, at) in places {
        if !people.contains(&id) {
            rig.tracked.insert(id, at[0]);
        }
    }
}

/// Seeds swept per schedule mode.
const SEEDS: u64 = 12;
/// Round budget for one leg of the walk. Lockstep needs ~40 (130 units at
/// walk speed); missed ticks and late inputs stretch it.
const LEG_ROUNDS: usize = 160;
/// Rounds walked past the seam before turning back, so the return leg starts
/// clear of the flip hysteresis.
const PAST_SEAM_ROUNDS: usize = 6;

const CLIENT: ClientId = 0;

fn north() -> RegionCoords {
    RegionCoords::new(0, -1)
}

fn north_west() -> RegionCoords {
    RegionCoords::new(-1, -1)
}

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

fn floor_column(x: usize, z: usize) -> (ColumnCoords, Column) {
    let air = Block::new(BlockId::AIR);
    let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
    (ColumnCoords::new(x, z), Column::new(0, air, air, vec![sub]))
}

/// Floor columns over the world-unit rectangle `xs` x `zs`: every chunk
/// column the rectangle overlaps.
fn floor_columns(xs: std::ops::Range<f32>, zs: std::ops::Range<f32>) -> Vec<(ColumnCoords, Column)> {
    common::chunks_over(xs, zs).into_iter().map(|(x, z)| floor_column(x, z)).collect()
}

/// Floor under the whole square between the spawn point (x=128,z=128) and the
/// region's -x/-z corner, continuing 128 units deep into
/// the three neighbours around that corner — so every route is walked
/// grounded, with colliders on both sides of each handoff. Stated in world
/// units, so it holds at any chunk size.
fn seam_floor_columns(rc: RegionCoords) -> Vec<(ColumnCoords, Column)> {
    let near = 0.0..5.0 * 64.0;
    let far = REGION_SIZE - 128.0..REGION_SIZE;
    let (xs, zs) = if rc == SPAWN_REGION {
        (near.clone(), near)
    } else if rc == north() {
        (near, far)
    } else if rc == RegionCoords::new(-1, 0) {
        (far, near)
    } else if rc == north_west() {
        (far.clone(), far)
    } else {
        // Far regions: just the chunk under the spawn column.
        return floor_columns(128.0..160.0, 128.0..160.0);
    };
    floor_columns(xs, zs)
}

struct Rig {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    now_ms: u64,
    /// Latest authoritative `PlayerRegion` push — what a client would believe.
    pushed_home: Option<RegionCoords>,
    /// [`Rig::start_trace`]: each traced region's tick, and what was seen.
    trace: Option<(std::collections::BTreeMap<RegionCoords, u64>, Vec<(RegionCoords, Seen)>)>,
    /// Every non-person entity [`assert_conserved`] has seen, and where it
    /// was last (`None` = in flight): it must never vanish.
    tracked: std::collections::BTreeMap<EntityId, Option<RegionCoords>>,
    /// Latest region snapshot sent to the client, per region: whether it
    /// held the client's player.
    snapshot_has_player: std::collections::BTreeMap<RegionCoords, bool>,
}

impl Rig {
    fn new(schedule: Option<InlineSchedule>) -> Self {
        Self::with_floor(schedule, seam_floor_columns)
    }

    fn with_floor(schedule: Option<InlineSchedule>, floor: fn(RegionCoords) -> Vec<(ColumnCoords, Column)>) -> Self {
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
            coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
            out: lod_result_send,
        });
        let spawner = schedule.map_or_else(InlineSpawner::default, InlineSpawner::with_schedule);
        Rig {
            manager: WorldManager::new(
                spawner,
                Box::new(floor),
                out_send,
                region_out_send,
                lod_builder,
                lod_result_recv,
            ),
            region_out,
            packets,
            now_ms: 0,
            pushed_home: None,
            trace: None,
            tracked: Default::default(),
            snapshot_has_player: Default::default(),
        }
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.trace_output(rc, &out);
                self.manager.handle_region_output(rc, out, self.now_ms);
            }
        }
        for (_, packet) in self.packets.try_iter() {
            match packet {
                ServerPacket::PlayerRegion(home, CLIENT) => self.pushed_home = home,
                ServerPacket::Region(rc, state) => {
                    self.snapshot_has_player.insert(rc, state.player_entites.contains_key(&CLIENT));
                }
                _ => {}
            }
        }
    }

    fn event(&mut self, ev: ServerEvent) {
        assert!(self.manager.handle_server_event(ev, self.now_ms));
        self.settle();
    }

    /// One scheduling round, then the ownership invariant: the player is
    /// owned by at most one region (none while a handoff is in flight).
    fn round(&mut self) {
        self.now_ms += TICK_RATE;
        self.manager.spawner_mut().tick_all();
        self.settle();
        let owners = self.owners();
        assert!(owners.len() <= 1, "player owned by several regions at once: {owners:?}");
        assert_conserved(self);
    }

    /// Test support: kill `rc`'s runner (a dead region thread). Its entities
    /// die with it, except what is in flight to it.
    fn kill(&mut self, rc: RegionCoords) {
        self.manager.spawner_mut().kill(rc);
        self.tracked.retain(|_, at| *at != Some(rc));
    }

    fn key(&mut self, key: Key, pressed: bool) {
        // Stamped with the spawn region on purpose: the manager routes input
        // by the player's home, not by the client's stamp.
        let ev = GameEvent::new(
            GameEventKind::PlayerInput(CLIENT, InputEvent::Key { key, pressed }),
            0,
            SPAWN_REGION,
        );
        self.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), CLIENT));
    }

    fn order(&mut self, order: Order) {
        let ev = GameEvent::new(GameEventKind::Order(CLIENT, order), 0, SPAWN_REGION);
        self.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), CLIENT));
    }

    /// The player's walk (if any) and feet tile in `rc`, which owns it.
    fn walk_state(&mut self, rc: RegionCoords) -> (Option<Walker>, TilePos) {
        let mut out = None;
        self.manager.spawner_mut().with_region(rc, |region| {
            let data = region.data();
            let e = *data.player_entites.get(&CLIENT).expect("player owned here");
            let handle = *data.ecs.rigidbody.get(e);
            let t = data.physics.bodies.get(handle).unwrap().translation();
            out = Some((data.ecs.walker.try_get(e).clone(), game::feet_tile(game::feet_of(t))));
        });
        out.expect("region is running")
    }

    /// Regions that currently own the player entity.
    fn owners(&mut self) -> Vec<RegionCoords> {
        let mut owners = Vec::new();
        for rc in self.manager.running_regions() {
            self.manager.spawner_mut().with_region(rc, |region| {
                if region.data().player_entites.contains_key(&CLIENT) {
                    owners.push(rc);
                }
            });
        }
        owners
    }

    fn player_xz(&mut self, rc: RegionCoords) -> (f32, f32) {
        let mut xz = None;
        self.manager.spawner_mut().with_region(rc, |region| {
            let data = region.data();
            let e = *data.player_entites.get(&CLIENT).expect("player owned here");
            let handle = *data.ecs.rigidbody.get(e);
            let t = data.physics.bodies.get(handle).unwrap().translation();
            xz = Some((t.x, t.z));
        });
        xz.expect("region is running")
    }

    /// Rounds until `rc` owns the player; panics past the leg budget.
    fn walk_until_owned_by(&mut self, rc: RegionCoords, what: &str) {
        for _ in 0..LEG_ROUNDS {
            self.round();
            if self.owners() == [rc] {
                return;
            }
        }
        panic!("{what}: player never reached {rc:?}; owners now {:?}", self.owners());
    }

    /// (region, tick, state hash) of every running region, in region order.
    fn fingerprint(&mut self) -> Vec<(RegionCoords, u64, u32)> {
        let mut regions = self.manager.running_regions();
        regions.sort();
        let mut out = Vec::new();
        for rc in regions {
            self.manager.spawner_mut().with_region(rc, |region| {
                out.push((rc, region.current_tick(), game::state_hash(region.data())));
            });
        }
        out
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

    // Let ghost mirrors and late inputs drain, then the player must still be
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
/// as far into the east neighbour: it must hand off, re-plan there, and
/// arrive. Panics with the seed on failure.
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
    rig.order(Order::MoveTo(Goal { region: EAST, column, y: None }));
    for _ in 0..ADVENTURE_ROUNDS {
        rig.round();
        if rig.owners() == [EAST] {
            let (walker, tile) = rig.walk_state(EAST);
            // Re-planned on arrival: a path local to EAST, straight to the
            // goal — not the carried edge path, nor a stuck re-plan.
            if let Some(w) = &walker {
                assert!(w.exit.is_none() && !w.replanned, "seed {seed}: walk not re-planned on arrival: {w:?}");
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

/// One entry of a region's authoritative output stream, as the held-key
/// test reads it: the region's tick when it happened.
#[derive(Debug, Clone, Copy)]
enum Seen {
    /// A marker input (`Resized { width: seq }`) was applied.
    Marker { seq: u32, tick: u64 },
    /// The player's body arrived (or was created) here.
    Arrived,
    /// The player's body left here, during the tick that brought the
    /// region to `stamp`.
    Departed { stamp: u64 },
}

impl Rig {
    /// Start recording what the running regions output (see [`Seen`]).
    fn start_trace(&mut self) {
        let mut ticks = std::collections::BTreeMap::new();
        for rc in self.manager.running_regions() {
            self.manager.spawner_mut().with_region(rc, |region| {
                ticks.insert(rc, region.current_tick());
            });
        }
        self.trace = Some((ticks, Vec::new()));
    }

    fn trace_output(&mut self, rc: RegionCoords, out: &RegionOutput) {
        let Some((ticks, seen)) = self.trace.as_mut() else { return };
        let Some(tick) = ticks.get_mut(&rc) else { return };
        let events = match out {
            RegionOutput::EventProcessed(e) | RegionOutput::EventProcessedHashed(e, ..) => vec![&e.kind],
            RegionOutput::Departures(list, stamp) => {
                if list.iter().any(|(b, _)| b.client.as_ref().map(|(c, _)| *c) == Some(CLIENT)) {
                    seen.push((rc, Seen::Departed { stamp: *stamp }));
                }
                vec![]
            }
            _ => vec![],
        };
        for kind in events {
            match kind {
                GameEventKind::Tick => *tick += 1,
                GameEventKind::PlayerInput(CLIENT, InputEvent::Resized { width, .. }) => {
                    seen.push((rc, Seen::Marker { seq: *width, tick: *tick }))
                }
                GameEventKind::EntityArrived(b) if b.client.as_ref().map(|(c, _)| *c) == Some(CLIENT) => {
                    seen.push((rc, Seen::Arrived))
                }
                _ => {}
            }
        }
    }

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
    asked: Vec<RegionCoords>,
    got: BTreeMap<RegionCoords, game::RegionSave>,
    checked: usize,
}

impl SaveCheck {
    fn round(&mut self, rig: &mut Rig) {
        self.got.extend(rig.manager.take_saves());
        if !self.asked.iter().all(|rc| self.got.contains_key(rc)) {
            return;
        }
        if !self.asked.is_empty() {
            let mut at = Vec::new();
            for (rc, save) in &self.got {
                let game::RegionSave::Live(state, transfers) = save else { panic!("nothing parks here") };
                if state.id_index.contains_key(&self.id) {
                    at.push((*rc, "state"));
                }
                at.extend(transfers.iter().filter(|(b, _)| b.id == self.id).map(|_| (*rc, "transfer")));
            }
            assert_eq!(at.len(), 1, "save {}: the player is saved in {at:?}", self.checked);
            self.checked += 1;
        }
        self.got.clear();
        self.asked = rig.manager.running_regions();
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
    let mut id = None;
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
        let data = region.data();
        id = *data.ecs.id.try_get(*data.player_entites.get(&CLIENT).unwrap());
    });
    let id = id.expect("the player has an EntityId");
    let mut saves = SaveCheck { id, asked: Vec::new(), got: BTreeMap::new(), checked: 0 };
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
