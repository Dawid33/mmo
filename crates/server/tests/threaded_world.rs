//! Threaded smoke test: real region threads + the manager core, driven by a
//! scripted client over channels (bypassing quinn). Asserts the running-
//! region set tracks the client's window as it roams, and that event
//! streams keep flowing across the whole window.
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver};
use game::movement;
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, EntityBundle, GameEvent,
    GameEventKind, InlineLodBuilder, InputEvent, Key, LodBuilder, RegionCoords, RegionOutput,
    ServerEvent, ServerPacket, SubChunk, WorldManager, FLIP_HYSTERESIS, REGION_SIZE, SPAWN_REGION,
    UNLOAD_GRACE_MS,
};
use server::region_threads::ThreadRegionSpawner;

const REGION_STARTUP_BUDGET: Duration = Duration::from_secs(10);

/// Voxel height of every fixture floor.
const FLOOR_DEPTH: u32 = 8;

fn floor_column(x: usize, z: usize) -> (ColumnCoords, Column) {
    (
        ColumnCoords::new(x, z),
        Column::new(
            0,
            Block::new(BlockId::AIR),
            Block::new(BlockId::AIR),
            vec![SubChunk::Dense(Box::new(Chunk::flat_floor(FLOOR_DEPTH)))],
        ),
    )
}

/// Floor columns over the world-unit rectangle `xs` x `zs`: every chunk
/// column it overlaps (the chunk edge is `CHUNK_UNITS`; negative edges clamp).
fn floor_columns(xs: std::ops::Range<f32>, zs: std::ops::Range<f32>) -> Vec<(ColumnCoords, Column)> {
    let edge = game::CHUNK_UNITS as f32;
    let span = |r: std::ops::Range<f32>| (r.start.max(0.0) / edge).floor() as usize..(r.end / edge).ceil() as usize;
    let zs = span(zs);
    span(xs).flat_map(|x| zs.clone().map(move |z| floor_column(x, z))).collect()
}

fn lightweight_columns() -> Vec<(ColumnCoords, Column)> {
    // Just the chunk under the spawn column.
    floor_columns(128.0..160.0, 128.0..160.0)
}

/// Floor under the whole square between the spawn point (x=128,z=128) and the
/// region's -x/-z corner, continuing 128 units deep into the three
/// neighbours around that corner. Every walk in this file therefore crosses
/// GROUNDED, with voxel colliders on both sides. Stated in world units, so it
/// holds at any chunk size.
fn seam_floor_columns(rc: RegionCoords) -> Vec<(ColumnCoords, Column)> {
    let near = 0.0..5.0 * 64.0;
    let far = REGION_SIZE - 128.0..REGION_SIZE;
    let (xs, zs) = if rc == SPAWN_REGION {
        (near.clone(), near)
    } else if rc == RegionCoords::new(0, -1) {
        (near, far)
    } else if rc == RegionCoords::new(-1, 0) {
        (far, near)
    } else if rc == RegionCoords::new(-1, -1) {
        (far.clone(), far)
    } else {
        return lightweight_columns();
    };
    floor_columns(xs, zs)
}

/// A key-down `PlayerInput` from client 0. Stamped with the spawn region on
/// purpose: the manager routes input by the player's HOME, not by the stamp.
fn key_event(key: Key, pressed: bool) -> ServerEvent {
    ServerEvent::ClientPacket(
        ClientPacket::GameEvent(GameEvent::new(
            GameEventKind::PlayerInput(0, InputEvent::Key { key, pressed }),
            0,
            SPAWN_REGION,
        )),
        0,
    )
}

struct Rig {
    manager: WorldManager<ThreadRegionSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    start: Instant,
    /// Every region id whose `Snapshot` output we have drained, no matter
    /// which drain saw it. On fast hardware a region can finish building
    /// inside a `settle()` window, so `settle()` — not `wait_for_snapshots`
    /// — is the one that pulls its snapshot off `region_out`. Recording here
    /// (in both drains) means a snapshot eaten by `settle()` still counts, so
    /// `wait_for_snapshots` can't hang waiting for one that already arrived.
    seen_snapshots: BTreeSet<RegionCoords>,
}

impl Rig {
    fn new() -> Self {
        Self::with_columns(|_| lightweight_columns())
    }

    fn with_columns(
        columns: impl FnMut(RegionCoords) -> Vec<(ColumnCoords, Column)> + Send + 'static,
    ) -> Self {
        Self::with_spawner(ThreadRegionSpawner::default(), columns)
    }

    /// The server's own spawn path: region threads run worldgen themselves
    /// (`RegionSeed::Deferred`) instead of being handed ready-made columns.
    fn with_on_thread_worldgen(
        columns: impl Fn(RegionCoords) -> Vec<(ColumnCoords, Column)> + Send + Sync + Clone + 'static,
    ) -> Self {
        let spawner = ThreadRegionSpawner::with_region_generator(std::sync::Arc::new(columns.clone()));
        Self::with_spawner(spawner, columns)
    }

    fn with_spawner(
        spawner: ThreadRegionSpawner,
        columns: impl FnMut(RegionCoords) -> Vec<(ColumnCoords, Column)> + Send + 'static,
    ) -> Self {
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
            coarse: Box::new(|_| Vec::new()),
            out: lod_result_send,
        });
        let manager = WorldManager::new(
            spawner,
            Box::new(columns),
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        );
        Rig {
            manager,
            region_out,
            packets,
            start: Instant::now(),
            seen_snapshots: BTreeSet::new(),
        }
    }

    /// Hand one drained region output to the manager, first recording it if
    /// it is a snapshot. Both `settle()` and `wait_for_snapshots()` funnel
    /// through here so snapshot bookkeeping is drain-site-independent.
    fn absorb(&mut self, rc: RegionCoords, output: RegionOutput) {
        if let RegionOutput::Snapshot(..) = &output {
            self.seen_snapshots.insert(rc);
        }
        let now = self.now_ms();
        self.manager.handle_region_output(rc, output, now);
    }
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
    fn event(&mut self, ev: ServerEvent) {
        assert!(self.manager.handle_server_event(ev, self.now_ms()));
    }
    /// Drain ALL pending packets once and return the set of region ids that
    /// got a snapshot. (try_iter consumes; never call this expecting packets
    /// to survive for a second look.)
    fn snapshot_regions(&mut self) -> BTreeSet<RegionCoords> {
        self.packets
            .try_iter()
            .filter_map(|(_, p)| match p {
                ServerPacket::Region(id, _) => Some(id),
                _ => None,
            })
            .collect()
    }
    /// Keep region-thread outputs flowing into the manager until `seen`
    /// accepts one of the packets bound for the client, or `timeout` passes.
    /// Returns whether it did; every packet observed lands in `log`.
    fn pump_until(
        &mut self,
        timeout: Duration,
        log: &mut Vec<String>,
        mut seen: impl FnMut(&ServerPacket) -> bool,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        let mut done = false;
        while !done && Instant::now() < deadline {
            if let Ok((rc, output)) = self.region_out.recv_timeout(Duration::from_millis(50)) {
                self.absorb(rc, output);
            }
            for (_to, p) in self.packets.try_iter() {
                done |= seen(&p);
                log.push(format!("{p:?}"));
            }
        }
        done
    }

    /// Block (bounded by `timeout`) until every region in `want` has
    /// produced a snapshot, draining `region_out` into the manager as
    /// outputs arrive. Panics with the still-missing set if the deadline
    /// passes first — brand-new regions build (worldgen + physics colliders)
    /// on their own thread before they can answer anything.
    fn wait_for_snapshots(&mut self, want: &[RegionCoords], timeout: Duration) {
        let want: BTreeSet<RegionCoords> = want.iter().copied().collect();
        let deadline = Instant::now() + timeout;
        while !want.is_subset(&self.seen_snapshots) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                remaining > Duration::ZERO,
                "regions never snapshotted: missing {:?}",
                want.difference(&self.seen_snapshots).collect::<Vec<_>>()
            );
            if let Ok((rc, output)) = self
                .region_out
                .recv_timeout(remaining.min(Duration::from_millis(200)))
            {
                self.absorb(rc, output);
            }
        }
    }
}

// These tests exercise real OS-thread routing and lifecycle behavior with a
// one-column terrain fixture; production worldgen has its own focused suite.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn roaming_client_cycles_regions_across_threads() {
    let mut rig = Rig::new();
    rig.event(ServerEvent::connected(0));

    // Subscribe the 3x3 window around home.
    let window = SPAWN_REGION.window_3x3();
    for rc in &window {
        rig.event(ServerEvent::ClientPacket(
            ClientPacket::RequestRegionConnection(*rc),
            0,
        ));
    }
    let mut running = rig.manager.running_regions();
    running.sort();
    let mut expected = window.clone();
    expected.sort();
    assert_eq!(running, expected, "3x3 window running on real threads");

    // Give the 9 freshly-spawned region threads time to finish building and
    // answer the snapshot request queued by `subscribe()`.
    rig.wait_for_snapshots(&window, REGION_STARTUP_BUDGET);
    let snapshots = rig.snapshot_regions();
    for rc in &window {
        assert!(snapshots.contains(rc), "snapshot for {:?}", rc);
    }

    // Region threads tick on their own: SyncClock/GameEvent packets arrive
    // without the test doing anything. All 9 are already up (snapshots
    // arrived above), so steady-state ticking is fast.
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut ticked = 0;
    while ticked < 9 && Instant::now() < deadline {
        if let Ok((rc, output)) = rig.region_out.recv_timeout(Duration::from_millis(50)) {
            rig.absorb(rc, output);
        }
        ticked += rig
            .packets
            .try_iter()
            .filter(|(_, p)| matches!(p, ServerPacket::GameEvent(_)))
            .count();
    }
    assert!(
        ticked >= 9,
        "all 9 regions tick independently (saw {ticked} events)"
    );

    // Roam east: window shifts from center (0,0) to center (1,0).
    let old_column: Vec<RegionCoords> = (-1..=1).map(|z| RegionCoords::new(-1, z)).collect();
    let new_column: Vec<RegionCoords> = (-1..=1).map(|z| RegionCoords::new(2, z)).collect();
    for rc in &old_column {
        rig.event(ServerEvent::ClientPacket(
            ClientPacket::ReleaseRegionConnection(*rc),
            0,
        ));
    }
    for rc in &new_column {
        rig.event(ServerEvent::ClientPacket(
            ClientPacket::RequestRegionConnection(*rc),
            0,
        ));
    }
    for rc in &new_column {
        assert!(rig.manager.running_regions().contains(rc));
    }
    // The new column are brand-new regions too — let them finish building
    // before the test process exits (join() in reap() is not otherwise
    // waited on within this test, but leaving them mid-build would race the
    // process teardown).
    rig.wait_for_snapshots(&new_column, REGION_STARTUP_BUDGET);

    // The released column parks after the grace period. Instead of sleeping
    // 5s, drive maintain with a future timestamp — time is an explicit input.
    let future = rig.now_ms() + UNLOAD_GRACE_MS + 1;
    rig.manager.maintain(future);
    // Stopped outputs come back over the channel from real threads. These
    // regions are already up and running (built seconds ago), so
    // shutdown-and-serialize is fast — no build cost involved.
    let deadline = Instant::now() + Duration::from_secs(5);
    while old_column
        .iter()
        .any(|rc| rig.manager.running_regions().contains(rc))
    {
        assert!(Instant::now() < deadline, "released regions failed to stop");
        if let Ok((rc, output)) = rig.region_out.recv_timeout(Duration::from_millis(100)) {
            rig.manager.handle_region_output(rc, output, future);
        }
    }
    for rc in &old_column {
        assert!(rig.manager.parked_regions().contains(rc), "{:?} parked", rc);
    }
}

/// End-to-end crossing on real region threads: a scripted client walks its
/// player out of `SPAWN_REGION` (0,0) across the -z boundary into (0,-1) using
/// real input, and we observe the authoritative handoff — the `PlayerRegion`
/// home flip AND the `EntityArrived` fan-out — arrive over the packet channel.
/// The arriving bundle is checked for STATE, not just existence: the floor
/// spans the seam (`seam_floor_columns`), so the player must cross grounded,
/// land just past the boundary in the arrival region's frame, and still carry
/// its client, camera and movement state.
///
/// Held-input note: `InputState::step` (run once per region tick) demotes a
/// `Pressed` key to `Held` but never clears it, and `key_held` reports both.
/// So a single `KeyW` press keeps the player walking every tick until a
/// release — no per-tick re-send is needed. `KeyE` enables fps-cam (the mode
/// the walk controller requires); the toggle fires on the first tick that
/// sees it `Pressed`. Both inputs are stamped to (0,0): the manager routes
/// `PlayerInput` by the player's *home*, which is (0,0) until the flip lands.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn regions_share_tick_numbers_on_real_threads() {
    use game::RegionSpawner;
    let mut rig = Rig::new();
    rig.event(ServerEvent::connected(0));
    let pair = [SPAWN_REGION, RegionCoords::new(0, -1)];
    for rc in pair {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0));
    }
    rig.wait_for_snapshots(&pair, REGION_STARTUP_BUDGET);
    // Wait (bounded) until both regions have reported a few times. A report is
    // stamped when emitted and drained later, so the only drain-delay-proof
    // claims are: a region is never ahead of the world clock, and both regions
    // count on the SAME numbers (reports land on multiples of 10).
    let mut reported: std::collections::BTreeMap<RegionCoords, BTreeSet<u64>> = Default::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !pair.iter().all(|rc| reported.get(rc).is_some_and(|t| t.len() >= 3)) {
        if let Ok((rc, output)) = rig.region_out.recv_timeout(Duration::from_millis(20)) {
            if let RegionOutput::SyncClock { tick, .. } = &output {
                let world = rig.manager.spawner_mut().world_tick();
                assert!(*tick <= world, "{rc:?} reports tick {tick} ahead of the world tick {world}");
                assert_eq!(tick % 10, 0, "{rc:?} reports an off-cadence tick {tick}");
                reported.entry(rc).or_default().insert(*tick);
            }
            rig.absorb(rc, output);
        }
    }
    for rc in pair {
        let n = reported.get(&rc).map_or(0, |t| t.len());
        assert!(n >= 3, "{rc:?} reported only {n} SyncClocks within 10 s");
    }
    let common = reported[&pair[0]].intersection(&reported[&pair[1]]).count();
    assert!(common >= 2, "regions share no tick numbers: {reported:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walking_input_crosses_a_boundary_on_real_threads() {
    let mut rig = Rig::with_columns(seam_floor_columns);
    rig.event(ServerEvent::connected(0));

    // Subscribe the 3x3 window so both the departing (0,0) and arriving
    // (0,-1) regions are up and relaying to us.
    let window = SPAWN_REGION.window_3x3();
    for rc in &window {
        rig.event(ServerEvent::ClientPacket(
            ClientPacket::RequestRegionConnection(*rc),
            0,
        ));
    }
    rig.wait_for_snapshots(&window, REGION_STARTUP_BUDGET);
    // Clear the snapshot/sync backlog so the diagnostic list below stays
    // focused on what happened after we started walking.
    let _ = rig.packets.try_iter().count();

    // Player spawns at region-local x=128,z=128 on the floor, facing -z. Enable
    // controls, then hold W: walk speed along -z from z=128 crosses the -z
    // boundary (past -FLIP_HYSTERESIS) after ~40 ticks, walking the floor
    // strip the whole way.
    rig.event(key_event(Key::KeyE, true));
    rig.event(key_event(Key::KeyW, true));

    let arrival_region = RegionCoords::new(0, -1);
    // ~40 ticks of walking plus handoff propagation.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen_flip = false;
    let mut arrival: Option<EntityBundle> = None;
    let mut observed: Vec<String> = Vec::new();
    while (!seen_flip || arrival.is_none()) && Instant::now() < deadline {
        // Keep region-thread outputs flowing into the manager: the crossing
        // surfaces as a `Departures` output that the manager turns into the
        // `PlayerRegion` push and the `EntityArrived` relay.
        if let Ok((rc, output)) = rig.region_out.recv_timeout(Duration::from_millis(50)) {
            let now = rig.now_ms();
            rig.manager.handle_region_output(rc, output, now);
        }
        for (_to, p) in rig.packets.try_iter() {
            match &p {
                ServerPacket::PlayerRegion(Some(rc), 0) if *rc == arrival_region => {
                    seen_flip = true;
                }
                ServerPacket::GameEvent(ev) if ev.region_id == arrival_region => {
                    if let GameEventKind::EntityArrived(bundle) = &ev.kind {
                        arrival = Some(bundle.clone());
                    }
                }
                _ => {}
            }
            observed.push(format!("{p:?}"));
        }
    }

    assert!(
        seen_flip,
        "no PlayerRegion flip to {arrival_region:?} within deadline; packets seen: {observed:#?}"
    );
    let Some(bundle) = arrival else {
        panic!(
            "no EntityArrived GameEvent for {arrival_region:?} within deadline; packets seen: {observed:#?}"
        );
    };

    // The relayed arrival is the target's confirmation: nothing in flight.
    assert_eq!(rig.manager.in_flight_count(), 0, "the confirmed crossing left the in-flight table");
    // The handoff carried the player's identity and state, not just an event.
    assert_eq!(bundle.id.birth_region, SPAWN_REGION);
    assert_eq!(bundle.client.as_ref().map(|(id, _)| *id), Some(0), "client rides the bundle");
    assert!(bundle.carried.camera.is_some(), "camera rides the bundle");
    // Pose is rebased into the arrival frame: x untouched, z just inside the
    // far (+z) edge — past the hysteresis line by at most one tick of walking.
    let (x, z) = (f32::from(bundle.body.pose.translation.x), f32::from(bundle.body.pose.translation.z));
    let tick_walk = movement::WALK_SPEED * movement::UNITS_PER_METER * movement::TICK_DT;
    assert!((x - 128.0).abs() < 0.5, "walked straight along -z, x={x}");
    assert!(
        z <= REGION_SIZE - FLIP_HYSTERESIS && z > REGION_SIZE - FLIP_HYSTERESIS - tick_walk - 0.5,
        "arrived within one walking tick past the flip line, z={z}"
    );
    // The floor spans the seam, so the crossing happened on the ground.
    let motion = bundle.carried.motion.expect("player carries movement state");
    assert!(motion.grounded && !motion.flying, "crossed grounded, got {motion:?}");
}

/// A player-carrying `EntityArrived` relayed from `region`, if `p` is one.
fn arrival_in(p: &ServerPacket, region: RegionCoords) -> Option<&EntityBundle> {
    match p {
        ServerPacket::GameEvent(ev) if ev.region_id == region => match &ev.kind {
            GameEventKind::EntityArrived(bundle) if bundle.client.is_some() => Some(bundle),
            _ => None,
        },
        _ => None,
    }
}

/// Diagonal crossing through a region corner on real threads: W+A from the
/// spawn point, which is equidistant from the -x and -z seams. Four region
/// threads are involved — the home, both edge neighbours (which get halo
/// mirrors, and may briefly own the player if one seam is crossed a tick
/// before the other) and the diagonal neighbour, where the player must end up.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn diagonal_walk_crosses_a_corner_on_real_threads() {
    let mut rig = Rig::with_columns(seam_floor_columns);
    rig.event(ServerEvent::connected(0));
    let window = SPAWN_REGION.window_3x3();
    for rc in &window {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(*rc), 0));
    }
    rig.wait_for_snapshots(&window, REGION_STARTUP_BUDGET);
    let _ = rig.packets.try_iter().count();

    rig.event(key_event(Key::KeyE, true));
    rig.event(key_event(Key::KeyW, true));
    rig.event(key_event(Key::KeyA, true));

    let corner = RegionCoords::new(-1, -1);
    let mut log = Vec::new();
    let mut home = None;
    let mut arrival: Option<EntityBundle> = None;
    rig.pump_until(Duration::from_secs(30), &mut log, |p| {
        if let ServerPacket::PlayerRegion(Some(rc), 0) = p {
            home = Some(*rc);
        }
        if let Some(bundle) = arrival_in(p, corner) {
            arrival = Some(bundle.clone());
        }
        home == Some(corner) && arrival.is_some()
    });

    assert_eq!(home, Some(corner), "home never settled on the diagonal region; packets: {log:#?}");
    let bundle = arrival.expect("EntityArrived relayed from the diagonal region");
    assert_eq!(rig.manager.in_flight_count(), 0, "every crossing on the way was confirmed");
    let (x, z) = (f32::from(bundle.body.pose.translation.x), f32::from(bundle.body.pose.translation.z));
    assert!(
        x > REGION_SIZE - 32.0 && z > REGION_SIZE - 32.0,
        "arrived near the corner in the diagonal region's frame, got x={x} z={z}"
    );
    let motion = bundle.carried.motion.expect("player carries movement state");
    assert!(motion.grounded, "crossed the corner grounded, got {motion:?}");
}

/// Walking into a region nobody is subscribed to, on the server's production
/// spawn path (`ThreadRegionSpawner::with_region_generator`): the departure has to
/// WAKE the target, whose brand-new thread runs worldgen + collider genesis
/// before it can look at the `EntityArrived` already queued for it. Then the
/// player walks back, so the return `EntityArrived` — sourced from the woken
/// region — proves the entity really lived there, and covers the round trip
/// (flip hysteresis in both directions) on real threads.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walking_into_a_cold_region_wakes_its_thread_and_returns() {
    let mut rig = Rig::with_on_thread_worldgen(seam_floor_columns);
    rig.event(ServerEvent::connected(0));
    // Home only: the region to the north is cold.
    rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0));
    rig.wait_for_snapshots(&[SPAWN_REGION], REGION_STARTUP_BUDGET);
    let north = RegionCoords::new(0, -1);
    assert!(!rig.manager.running_regions().contains(&north), "north starts cold");

    rig.event(key_event(Key::KeyE, true));
    rig.event(key_event(Key::KeyW, true));
    let mut log = Vec::new();
    let flipped = rig.pump_until(Duration::from_secs(30), &mut log, |p| {
        matches!(p, ServerPacket::PlayerRegion(Some(rc), 0) if *rc == north)
    });
    assert!(flipped, "no home flip into the cold region; packets: {log:#?}");
    assert!(rig.manager.running_regions().contains(&north), "the arrival woke the cold region");

    // Subscribe now, so the woken region relays to us; its snapshot proves
    // the new thread finished genesis and is answering.
    rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(north), 0));
    rig.wait_for_snapshots(&[north], REGION_STARTUP_BUDGET);

    // Turn around — still stamping the old home; routing goes by `homes`.
    rig.event(key_event(Key::KeyW, false));
    rig.event(key_event(Key::KeyS, true));
    let mut back: Option<EntityBundle> = None;
    let mut home_again = false;
    rig.pump_until(Duration::from_secs(30), &mut log, |p| {
        home_again |= matches!(p, ServerPacket::PlayerRegion(Some(rc), 0) if *rc == SPAWN_REGION);
        if let Some(bundle) = arrival_in(p, SPAWN_REGION) {
            back = Some(bundle.clone());
        }
        home_again && back.is_some()
    });
    assert!(home_again, "no home flip back to the spawn region; packets: {log:#?}");
    let bundle = back.expect("EntityArrived relayed from the spawn region on return");
    assert_eq!(bundle.id.birth_region, SPAWN_REGION, "the same identity comes back");
    assert_eq!(bundle.client.as_ref().map(|(id, _)| *id), Some(0));
    let z = f32::from(bundle.body.pose.translation.z);
    assert!(
        z >= FLIP_HYSTERESIS && z < 32.0,
        "returned just past the flip line in the spawn region's frame, z={z}"
    );
}

/// World autosave on the server's spawn path: a running region's save goes to
/// the store; a fresh manager (a restarted server) wakes that region from the
/// file on its own thread — same clock, and only the new session's player.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn saved_region_is_restored_by_a_restarted_server() {
    use server::region_store::RegionStore;
    let dir = std::env::temp_dir().join(format!("mmo-threaded-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = RegionStore::new(dir.clone(), 42);
    let spawner = || ThreadRegionSpawner::with_region_generator(std::sync::Arc::new(|_| lightweight_columns())).with_store(store.clone());

    let mut rig = Rig::with_spawner(spawner(), |_| lightweight_columns());
    rig.event(ServerEvent::connected(0));
    rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0));
    rig.wait_for_snapshots(&[SPAWN_REGION], REGION_STARTUP_BUDGET);
    // Let the clock run so a regenerated region (tick ~0) is told apart.
    let ran_until = Instant::now() + Duration::from_millis(20 * game::TICK_RATE);
    while Instant::now() < ran_until {
        if let Ok((rc, output)) = rig.region_out.recv_timeout(Duration::from_millis(50)) {
            rig.absorb(rc, output);
        }
    }
    rig.manager.request_save();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saves = Vec::new();
    while saves.is_empty() && Instant::now() < deadline {
        if let Ok((rc, output)) = rig.region_out.recv_timeout(Duration::from_millis(50)) {
            rig.absorb(rc, output);
        }
        saves = rig.manager.take_saves();
    }
    let (rc, save) = saves.pop().expect("the running region answered Save");
    assert_eq!(rc, SPAWN_REGION);
    let game::RegionSave::Live(live, _) = &save else { panic!("a running region saves its live state") };
    let saved_tick = *live.tick;
    assert!(saved_tick >= 10, "the region ran: tick {saved_tick}");
    store.save(rc, &save.into_serialized()).unwrap();
    rig.manager.shutdown_all();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !rig.manager.running_regions().is_empty() && Instant::now() < deadline {
        if let Ok((rc, output)) = rig.region_out.recv_timeout(Duration::from_millis(50)) {
            rig.absorb(rc, output);
        }
    }
    drop(rig);

    // A new process: client ids start at 0 again.
    let mut rig = Rig::with_spawner(spawner(), |_| lightweight_columns());
    rig.event(ServerEvent::connected(0));
    rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0));
    let mut log = Vec::new();
    let mut restored = None;
    rig.pump_until(REGION_STARTUP_BUDGET, &mut log, |p| match p {
        ServerPacket::Region(id, rb) if *id == SPAWN_REGION => {
            restored = Some((*rb.tick, rb.player_entites.len(), rb.clients.len()));
            true
        }
        _ => false,
    });
    let (tick, players, clients) = restored.expect("snapshot of the restored region");
    assert!(tick >= saved_tick, "restored from the save (tick {tick} < saved {saved_tick})");
    assert_eq!((players, clients), (1, 1), "the old session's player was dropped, the new one created");
    rig.manager.shutdown_all();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A player's body and inventory outlive the server on its own spawn path: the
/// orderly exit parks the player (its record goes through the player store) and
/// every region (saved to the region store); a restarted server strips the old
/// session's body from the region file and restores the player, with its
/// inventory, from the record the store resolves for its token.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn player_is_restored_by_a_restarted_server() {
    use server::player_store::{MemoryPlayerStore, PlayerStore};
    use server::region_store::RegionStore;
    let dir = std::env::temp_dir().join(format!("mmo-threaded-players-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = RegionStore::new(dir.clone(), 42);
    let spawner = || ThreadRegionSpawner::with_region_generator(std::sync::Arc::new(|_| lightweight_columns())).with_store(store.clone());
    let mut players = MemoryPlayerStore::default();
    let token = game::PlayerToken::generate([7; game::PLAYER_TOKEN_BYTES]);
    let (player, record) = players.resolve(&token).unwrap();
    assert!(record.is_none(), "a new player");

    // The first session starts from a record that carries a main-slot stack
    // and a cursor stack (items cannot be injected into a live thread).
    let mut seeded = game::Client::default();
    seeded.inventory.slots[20] = Some(game::ItemStack { item: game::ItemId(5), count: 33 });
    seeded.cursor = Some(game::ItemStack { item: game::ItemId(6), count: 2 });
    let first = game::PlayerRecord {
        client: seeded,
        pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(512.0, 2000.0, 512.0)),
        region: SPAWN_REGION,
        fortress: None,
        adventure: None,
    };
    let mut rig = Rig::with_spawner(spawner(), |_| lightweight_columns());
    rig.event(ServerEvent::ClientConnected { client: 0, player, record: Some(first) });
    rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0));
    rig.wait_for_snapshots(&[SPAWN_REGION], REGION_STARTUP_BUDGET);
    let swap = GameEventKind::Order(0, game::Order::SetMode { mode: game::PlayMode::Adventure, at: None });
    rig.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(GameEvent::new(swap, 0, SPAWN_REGION)), 0));
    // The server's orderly exit.
    rig.manager.park_all_players(rig.now_ms());
    rig.manager.shutdown_all();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !rig.manager.running_regions().is_empty() && Instant::now() < deadline {
        if let Ok((rc, output)) = rig.region_out.recv_timeout(Duration::from_millis(50)) {
            rig.absorb(rc, output);
        }
    }
    assert!(rig.manager.running_regions().is_empty(), "every region parked");
    rig.manager.request_save();
    for (rc, save) in rig.manager.take_saves() {
        store.save(rc, &save.into_serialized()).unwrap();
    }
    players.store(rig.manager.take_player_saves()).unwrap();
    drop(rig);

    // A new process: client ids start at 0 again; the edge resolves the token.
    let (again, record) = players.resolve(&token).unwrap();
    assert_eq!(again, player);
    let record = record.expect("the player's record was stored");
    assert!(record.client.mode == game::PlayMode::Adventure);
    assert_eq!(record.client.inventory.slots[20].map(|s| s.count), Some(33));
    // A park puts the cursor stack back into the inventory.
    let cursor_stack = Some(game::ItemStack { item: game::ItemId(6), count: 2 });
    assert_eq!(record.client.cursor, None);
    assert!(record.client.inventory.slots.contains(&cursor_stack), "{:?}", record.client.inventory);
    let mut rig = Rig::with_spawner(spawner(), |_| lightweight_columns());
    rig.event(ServerEvent::ClientConnected { client: 0, player, record: Some(record) });
    rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0));
    let mut log = Vec::new();
    let mut restored = None;
    rig.pump_until(REGION_STARTUP_BUDGET, &mut log, |p| match p {
        ServerPacket::Region(id, rb) if *id == SPAWN_REGION => {
            restored = Some((rb.player_entites.len(), rb.clients.get(&0).cloned()));
            true
        }
        _ => false,
    });
    let (bodies, client) = restored.expect("snapshot of the restored region");
    assert_eq!(bodies, 1, "one body: the restored player's");
    let client = client.expect("client 0 restored");
    assert!(client.mode == game::PlayMode::Adventure, "{client:?}");
    assert_eq!(client.inventory.slots[20].map(|s| s.count), Some(33), "{client:?}");
    assert_eq!(client.cursor, None, "{client:?}");
    assert!(client.inventory.slots.contains(&cursor_stack), "{client:?}");
    rig.manager.shutdown_all();
    let _ = std::fs::remove_dir_all(&dir);
}
