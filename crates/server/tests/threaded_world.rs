//! Threaded smoke test: real region threads + the manager core, driven by a
//! scripted client over channels (bypassing quinn). Asserts the running-
//! region set tracks the client's window as it roams, and that event
//! streams keep flowing across the whole window.
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver};
use game::{
    ClientId, ClientPacket, RegionCoords, RegionOutput, ServerEvent, ServerPacket, WorldManager,
    SPAWN_REGION, UNLOAD_GRACE_MS,
};
use server::region_threads::ThreadRegionSpawner;

/// A fresh region's first build (worldgen chunks -> physics colliders, one
/// per solid voxel — see `Region::from_chunks`/`attach_voxels_collider_safe`)
/// costs low single-digit seconds even for a flat floor; 9 of them in
/// parallel on real OS threads still take a few seconds wall-clock. Steady
/// state (ticking, shutdown of already-running regions) is fast — only the
/// initial spawn wait needs a generous budget.
const REGION_STARTUP_BUDGET: Duration = Duration::from_secs(20);

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
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let manager = WorldManager::new(
            ThreadRegionSpawner::default(),
            Box::new(worldgen::generate_region),
            out_send,
            region_out_send,
        );
        Rig { manager, region_out, packets, start: Instant::now(), seen_snapshots: BTreeSet::new() }
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
        self.settle();
    }
    /// Drain region outputs until quiet for 100ms OR a 500ms total budget
    /// runs out, whichever comes first. The budget is load-bearing: running
    /// regions tick every 50ms on their own threads, so once anything is
    /// ticking the channel is NEVER quiet for 100ms and an unbounded
    /// wait-for-quiet would spin forever. Only meaningful for steady-state
    /// traffic — a region's first build takes far longer than this window,
    /// so brand-new spawns need `wait_for_snapshots` instead.
    fn settle(&mut self) {
        let budget = Instant::now() + Duration::from_millis(500);
        while Instant::now() < budget {
            match self.region_out.recv_timeout(Duration::from_millis(100)) {
                Ok((rc, output)) => self.absorb(rc, output),
                Err(_) => return,
            }
        }
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
            if let Ok((rc, output)) = self.region_out.recv_timeout(remaining.min(Duration::from_millis(200)))
            {
                self.absorb(rc, output);
            }
        }
    }
}

#[test]
fn roaming_client_cycles_regions_across_threads() {
    let mut rig = Rig::new();
    rig.event(ServerEvent::ClientConnected(0));

    // Subscribe the 3x3 window around home.
    let window = SPAWN_REGION.window_3x3();
    for rc in &window {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(*rc), 0));
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
    std::thread::sleep(Duration::from_millis(600));
    rig.settle();
    let ticked = rig
        .packets
        .try_iter()
        .filter(|(_, p)| matches!(p, ServerPacket::GameEvent(_)))
        .count();
    assert!(ticked >= 9, "all 9 regions tick independently (saw {ticked} events)");

    // Roam east: window shifts from center (0,0) to center (1,0).
    let old_column: Vec<RegionCoords> = (-1..=1).map(|z| RegionCoords::new(-1, z)).collect();
    let new_column: Vec<RegionCoords> = (-1..=1).map(|z| RegionCoords::new(2, z)).collect();
    for rc in &old_column {
        rig.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(*rc), 0));
    }
    for rc in &new_column {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(*rc), 0));
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
    while old_column.iter().any(|rc| rig.manager.running_regions().contains(rc)) {
        assert!(Instant::now() < deadline, "released regions failed to stop");
        if let Ok((rc, output)) = rig.region_out.recv_timeout(Duration::from_millis(100)) {
            rig.manager.handle_region_output(rc, output, future);
        }
    }
    for rc in &old_column {
        assert!(rig.manager.parked_regions().contains(rc), "{:?} parked", rc);
    }
}
