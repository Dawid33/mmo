//! R2b Task 7: the world coordinator runtime — the only writer of history state, durable
//! before visible — on its own thread, joined to the host's `WorldManager` by a crossbeam
//! `ChannelWorldLink` as `server::run` wires `AllInOne`; R1 worlds migrate their `history`
//! row into a `coordinator` row.
//!
//! Its own test binary: it installs the process-global block props and runs real threads
//! against SQLite stores in temp dirs.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver};
use game::history::{AdventureRefusal, History, Location, SiteId};
use game::{
    Claim, ClientId, ClientPacket, CoordMsg, HostMsg, InlineLodBuilder, InlineSpawner, LodBuilder,
    PlayerId, RegionCoords, RegionOutput, RegionSpawner, RequestId, ServerEvent, ServerPacket, WorldLink, WorldManager, WorldMap,
    WorldReply, WorldRequest,
};
use server::claims::{HostClaims, ServerExit};
use server::coordinator_rt::{self, CoordinatorThread};
use server::player_store::{PlayerKey, PlayerStore, StorePlayers};
use server::world_file::{ensure_coordinator_state, CoordinatorSource, CoordinatorStart};
use world_store::{StoreHandle, StoreUrl};

/// The placement hop the tests' `Attach` reports their player at, and so the hop of its requests.
const ATTACHED_HOP: u64 = 1;

/// Wall time any wait below may take before the test fails.
const WAIT: Duration = Duration::from_secs(20);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmo-coordinator-rt-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_store(dir: &Path) -> StoreHandle {
    let url = StoreUrl::Sqlite(dir.join("w.sqlite"));
    StoreHandle::open(&url, world_store::Dials::BUILT, world_store::OpenMode::Writer).unwrap()
}

fn install_props() {
    let registry = game::BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
    game::install_block_props(registry.props());
}

/// A 48 × 48 grassland with a sea border (a bootstrap settles it).
fn land_map() -> WorldMap {
    let mut tiles = Vec::new();
    for z in 0..48 {
        for x in 0..48 {
            let sea = x < 2 || z < 2 || x >= 46 || z >= 46;
            tiles.push(game::TileSummary {
                ground_min_m: 0.0,
                ground_mean_m: 10.0,
                ground_max_m: 10.0,
                relief_m: 10.0,
                slope: [0.0, 0.0],
                sea_fraction: if sea { 1.0 } else { 0.0 },
                lake_fraction: 0.0,
                lake_level_m: None,
                rivers: 1,
                river_max_discharge_m2: 0.0,
                river_max_width_m: 0.0,
                surface_rock: None,
                landing: Some([256, 256]),
                biome: if sea { game::Biome::Ocean } else { game::Biome::Grassland },
                region: 0,
                temperature_c: 15.0,
                precip_mm: 800.0,
                savagery: 0,
                alignment: 0,
                volcano: false,
            });
        }
    }
    WorldMap { origin: RegionCoords::new(0, 0), width: 48, height: 48, tiles, rock_names: vec![], region_types: vec![] }
}

fn flat_floor(_rc: RegionCoords) -> Vec<(game::ColumnCoords, game::Column)> {
    let air = game::Block::new(game::BlockId::AIR);
    let mut out = Vec::new();
    for x in 0..game::REGION_CHUNKS {
        for z in 0..game::REGION_CHUNKS {
            let sub = game::SubChunk::Dense(Box::new(game::Chunk::flat_floor(game::BLOCK_VOXELS as u32)));
            out.push((game::ColumnCoords::new(x, z), game::Column::new(0, air, air, vec![sub])));
        }
    }
    out
}

/// The first populated site (not abandoned, a living member): where a player may start an
/// adventure — the state-changing world request these tests prove durability on.
fn start_site(h: &History) -> SiteId {
    h.sites()
        .find(|s| s.abandoned.is_none() && !h.living_members(s.id).is_empty())
        .map(|s| s.id)
        .expect("a populated site")
}

/// A player's request to start an adventure at `site`.
fn start_request(site: SiteId) -> WorldRequest {
    WorldRequest::StartAdventure(site)
}

/// The coordinator's state as the store holds it, decoded (`None` before its first save).
fn stored_state(store: &StoreHandle, map: &WorldMap) -> Option<(History, Vec<(PlayerId, Claim)>)> {
    let row = store.load_coordinator().unwrap()?;
    let grid = game::history::SocietyGrid::from_world_map(map);
    let c = game::Coordinator::load(&row.core, &row.history, grid, game::coordinator::ReplicaId(0), 0).expect("the projection loads");
    Some((c.history().clone(), c.claims()))
}

/// A directive or a claim-granting reply: what must never leave before the state that decided
/// it is written (since Elastic 2a no message does).
fn gated(msg: &CoordMsg) -> bool {
    match msg {
        CoordMsg::Directive { .. } => true,
        CoordMsg::Reply { reply, .. } => reply.grants_claim(),
        _ => false,
    }
}

/// One `AllInOne` server life, as `server::run` wires it: inline regions, the store's
/// claims, the coordinator thread over a channel link.
struct Life {
    manager: WorldManager<InlineSpawner>,
    claims: HostClaims,
    coordinator: Option<CoordinatorThread>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    inbox: Vec<(Option<ClientId>, ServerPacket)>,
    started: Instant,
}

impl Life {
    /// `boot`: this host process start's number in its request ids (unique per life, as
    /// `server::run`'s `host_boot`; a repeated one would be answered from the memo).
    fn start(store: &StoreHandle, map: &WorldMap, start: CoordinatorStart, boot: u64) -> Life {
        let claims = HostClaims::claim(store.clone(), &game::AllInOne, Some(map.extent()), store.super_region_regions())
            .unwrap();
        let mut regions = std::collections::BTreeMap::new();
        for rc in store.stored_regions().unwrap().into_keys() {
            regions.insert(rc, claims.load_region(rc).unwrap().expect("listed, so stored"));
        }
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
            coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
            water: Box::new(game::lod::LodWater::dry),
            out: lod_result_send,
        });
        let mut manager = WorldManager::new(
            InlineSpawner::with_portable(regions),
            Box::new(flat_floor),
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        );
        manager.set_stored_regions(store.stored_regions().unwrap());
        manager.restore_in_flight(store.load_in_flight(claims.fence().in_flight_scope()).unwrap());
        let (link, thread) = coordinator_rt::start_all_in_one(store.clone(), start, claims.fenced().clone()).unwrap();
        manager.set_world_link(Box::new(link), boot, 0);
        manager.set_world_map(map.clone());
        let mut life = Life {
            manager,
            claims,
            coordinator: Some(thread),
            region_out,
            packets,
            inbox: Vec::new(),
            started: Instant::now(),
        };
        life.until("attached", |l| l.manager.world_attached());
        life
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// One round: the regions tick, their outputs and the coordinator's messages are
    /// handled, the packets collected.
    fn round(&mut self) {
        let now = self.now_ms();
        self.manager.spawner_mut().tick_all();
        for _ in 0..3 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, now);
            }
        }
        self.manager.maintain(now);
        self.manager.pump_lod_results();
        for (to, p) in self.packets.try_iter() {
            if !matches!(
                p,
                ServerPacket::Region(..) | ServerPacket::GameEvent(_) | ServerPacket::SyncClock(..) | ServerPacket::LodData(..)
            ) {
                self.inbox.push((to, p));
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    /// Rounds until `done`, at most [`WAIT`].
    fn until(&mut self, what: &str, done: impl Fn(&mut Life) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            self.round();
        }
    }

    fn send(&mut self, ev: ServerEvent) {
        let now = self.now_ms();
        assert!(self.manager.handle_server_event(ev, now));
    }

    /// `c`'s `AdventureResult`s so far, taken out of the inbox.
    fn adventure_results(&mut self, c: ClientId) -> Vec<Result<SiteId, AdventureRefusal>> {
        let mut out = Vec::new();
        self.inbox.retain(|(to, p)| match p {
            ServerPacket::AdventureResult(r) if *to == Some(c) => {
                out.push(*r);
                false
            }
            _ => true,
        });
        out
    }

    /// `c` sends `packet`; its one `AdventureResult`.
    fn ask(&mut self, c: ClientId, packet: ClientPacket) -> Result<SiteId, AdventureRefusal> {
        self.send(ServerEvent::ClientPacket(packet, c));
        self.until("an AdventureResult", |l| l.inbox.iter().any(|(to, p)| *to == Some(c) && matches!(p, ServerPacket::AdventureResult(_))));
        let results = self.adventure_results(c);
        assert_eq!(results.len(), 1, "one AdventureResult: {results:?}");
        results[0]
    }

    /// The orderly exit `server::run` takes: park, the coordinator's last save, the cut.
    fn exit(mut self) -> ServerExit {
        let writer = server::cuts::CutWriter::new(self.claims.clone());
        let fenced = self.claims.fenced().clone();
        let now = self.manager.spawner_mut().world_tick();
        server::cuts::exit_world(
            &mut self.manager,
            server::cuts::ExitParams {
                region_out: &self.region_out,
                now_ms: || 0,
                cuts: Some(writer),
                coordinator: self.coordinator.take(),
                fenced: &fenced,
                world_generation: &server::claims::WorldGeneration::default(),
                park_timeout: Duration::from_secs(10),
                save_timeout: Duration::from_secs(10),
                now_tick: || now,
            },
            |m| m.spawner_mut().pump(),
        )
    }
}

/// An `AllInOne` life on a SQLite world: a player starts an adventure, the server exits in
/// order, the world restarts → the player's record holds the adventure, the coordinator's
/// saved claim comes back as `Adventure(site)` and holds once the player is online again (its
/// session is told it; another start of A's is refused `AlreadyPlaying`).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn state_survives_restart() {
    install_props();
    let dir = scratch("restart");
    let map = land_map();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());

    // Life 1: bootstrapped, saved at once; A starts an adventure.
    let store = open_store(&dir);
    let (a, _) = StorePlayers(store.clone()).resolve(&a_key).unwrap();
    let start = ensure_coordinator_state(&store, &map, None).unwrap().expect("a history");
    assert_eq!(start.source, CoordinatorSource::Bootstrapped);
    let site = start_site(start.coordinator.history());
    let mut life = Life::start(&store, &map, start, 1);
    life.send(ServerEvent::ClientConnected { client: 0, player: a, record: None });
    life.round();
    assert_eq!(life.ask(0, ClientPacket::StartAdventure(site)), Ok(site), "started");
    // Durable before visible: the reply came after the grant was saved.
    let (_, claims) = stored_state(&store, &map).expect("saved before the reply");
    assert_eq!(claims, vec![(a, Claim::Adventure(site))], "the claim is in the store before the player hears of it");
    for _ in 0..5 {
        life.round();
    }
    assert_eq!(life.exit(), ServerExit::Clean);

    // Life 2.
    let store = open_store(&dir);
    let (again, record) = StorePlayers(store.clone()).resolve(&a_key).unwrap();
    assert_eq!(again, a);
    let record = record.expect("the exit's cut saved A's record");
    assert_eq!(record.adventure, Some(site), "A's record holds the adventure");
    assert_eq!(store.load_history().unwrap(), None, "no cut wrote the R1 history row");
    let start = ensure_coordinator_state(&store, &map, None).unwrap().expect("a history");
    assert_eq!(start.source, CoordinatorSource::Stored);
    assert_eq!(start.coordinator.claim_of(a), Some(Claim::Adventure(site)), "the claim comes back");
    let mut life = Life::start(&store, &map, start, 2);
    life.send(ServerEvent::ClientConnected { client: 0, player: a, record: Some(record) });
    life.round();
    assert_eq!(life.adventure_results(0), vec![Ok(site)], "A is told its adventure: AdventureResult(Ok)");
    for _ in 0..5 {
        life.round();
    }
    assert_eq!(
        life.ask(0, ClientPacket::StartAdventure(site)),
        Err(AdventureRefusal::AlreadyPlaying),
        "A is online with its adventure: it may not start another"
    );
    assert_eq!(life.exit(), ServerExit::Clean);
    let store = open_store(&dir);
    let (_, claims) = stored_state(&store, &map).expect("saved");
    assert_eq!(claims, vec![(a, Claim::Adventure(site))], "still the one claim");
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// An R1 store (only the `history` row) starts: the coordinator migrates it and saves
/// it at once as its own `coordinator` row, the same history (log length); the
/// `history` row is left as it was.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn r1_world_migrates_history_row() {
    let dir = scratch("migrate");
    let map = land_map();
    let store = open_store(&dir);
    let mut history = ensure_coordinator_state(&store, &map, None).unwrap().unwrap().coordinator.history().clone();
    history.advance(game::history::CalendarTick(history.now().0 + 2 * game::history::DAYS_PER_YEAR * game::history::TICKS_PER_DAY));
    let r1 = game::history::persist::encode(&history);
    let cut = game::WorldCut {
        host: h(0),
        id: game::CutId(1),
        tick: 1,
        regions: Vec::new(),
        in_flight: Vec::new(),
        players: Vec::new(),
        history: Some(r1.clone()),
        adopts: vec![],
    };
    store.commit_cut(cut, world_store::Fence::default()).recv().unwrap().expect("the R1 cut commits");
    assert_eq!(store.load_coordinator().unwrap(), None, "an R1 world has no coordinator row");

    let start = ensure_coordinator_state(&store, &map, None).unwrap().unwrap();
    assert_eq!(start.source, CoordinatorSource::Migrated);
    let fenced = server::claims::Fenced::default();
    let (_link, thread) = coordinator_rt::start_all_in_one(store.clone(), start, fenced.clone()).unwrap();
    let deadline = Instant::now() + WAIT;
    let saved = loop {
        if let Some((h, claims)) = stored_state(&store, &map) {
            break (h, claims);
        }
        assert!(Instant::now() < deadline, "the migrated state was never saved");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(saved.0.log().len(), history.log().len(), "the same history log");
    assert!(saved.0.log().len() > 0);
    assert!(saved.1.is_empty(), "no claims");
    assert_eq!(thread.shutdown(Duration::from_secs(10)), ServerExit::Clean);
    assert!(!fenced.is_set());
    assert_eq!(store.load_history().unwrap(), Some(r1), "the history row is left as it is");
    // The next start loads the coordinator row.
    assert_eq!(ensure_coordinator_state(&store, &map, None).unwrap().unwrap().source, CoordinatorSource::Stored);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Writes that fail (`StoreHandle::fail_next_writes`) hold back every message (Elastic 2a:
/// nothing leaves before its transition is written): no directive or claim-granting reply
/// reaches the host before a write holding what decided it has committed — the adventure's
/// grant, and the birth (to a resident of the started region) whose spawn the directive
/// carries; once writes commit they all arrive, the attach echo with them.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn directive_not_sent_before_save() {
    const FAILURES: u32 = 4;
    let dir = scratch("gated");
    let map = land_map();
    let store = open_store(&dir);
    let start = ensure_coordinator_state(&store, &map, None).unwrap().unwrap();
    let site = start_site(start.coordinator.history());
    let rc = start.coordinator.history().site(site).expect("the site").region;
    store.fail_next_writes(FAILURES);
    let fenced = server::claims::Fenced::default();
    let (mut link, thread) = coordinator_rt::start_all_in_one(store.clone(), start, fenced.clone()).unwrap();
    let player = PlayerId(1);
    let id = RequestId { host: h(0), boot: 1, seq: 1 };
    let online = vec![game::OnlinePlayer { player, claim: None, claim_pending: false, report: 1, hop: ATTACHED_HOP }];
    link.send(HostMsg::Attach { host: h(0), epoch: 1, running: Vec::new(), online, generation: 0, boot: 1, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback });
    // The site's region runs: its residents are embodied there; then the player's adventure.
    link.send(HostMsg::RegionStarted { rc, incarnation: 1 });
    link.send(HostMsg::Request { id, player, hop: ATTACHED_HOP, req: start_request(site) });
    // The history runs on (month by month) until a resident of the region has a child: a
    // birth to an embodied mother is a spawn directive.
    let child = thread
        .with_core(
            move |c| {
                let h = c.history_mut();
                let from = h.now();
                for _ in 0..12 * 30 {
                    if let Some(q) = h.people().find(|q| q.born > from && q.location == Location::InRegion(rc)) {
                        return Some(q.id);
                    }
                    let next = game::history::CalendarTick(h.now().0 + 28 * game::history::TICKS_PER_DAY);
                    h.advance(next);
                }
                None
            },
            WAIT,
        )
        .expect("the coordinator answers")
        .expect("a child born in the running region within 30 years");

    let deadline = Instant::now() + WAIT;
    let (mut spawned, mut replies, mut attached) = (false, 0, false);
    while !spawned || replies == 0 {
        assert!(Instant::now() < deadline, "timed out: child spawned {spawned}, {replies} replies");
        let Some(msg) = link.recv() else {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        assert_eq!(store.failing_writes_left(), 0, "{msg:?} left while writes were still failing");
        if gated(&msg) {
            let (saved, claims) = stored_state(&store, &map).expect("a save committed");
            assert_eq!(claims, vec![(player, Claim::Adventure(site))], "{msg:?} left before the grant was durable");
            if let CoordMsg::Directive { directive, .. } = &msg {
                if let game::GameEventKind::SpawnPerson(spawn) = &directive.event {
                    assert!(saved.person(spawn.person).is_some(), "{msg:?} left before its person was durable");
                }
            }
        }
        match msg {
            CoordMsg::Attached { epoch: 1 } => attached = true,
            CoordMsg::Directive { incarnation, directive } => {
                assert_eq!(incarnation, 1);
                if matches!(&directive.event, game::GameEventKind::SpawnPerson(s) if s.person == child) {
                    assert_eq!(directive.region, rc);
                    spawned = true;
                }
            }
            CoordMsg::Reply { id: got, reply: WorldReply::AdventureStarted { site: s, region } } => {
                assert_eq!((got, s, region), (id, site, rc));
                replies += 1;
            }
            _ => {}
        }
    }
    assert!(attached, "the ungated attach echo came too");
    assert_eq!(thread.shutdown(Duration::from_secs(10)), ServerExit::Clean);
    assert!(!fenced.is_set());
    assert!(!link.is_up(), "the link goes down with the coordinator");
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A coordinator whose row another coordinator claimed is fenced at its next save: the
/// process-wide flag is set, its link goes down and the thread ends `Fenced`.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn fenced_coordinator_stops() {
    let dir = scratch("fenced");
    let map = land_map();
    let store = open_store(&dir);
    let start = ensure_coordinator_state(&store, &map, None).unwrap().unwrap();
    let site = start_site(start.coordinator.history());
    let fenced = server::claims::Fenced::default();
    let (mut link, thread) = coordinator_rt::start_all_in_one(store.clone(), start, fenced.clone()).unwrap();
    let deadline = Instant::now() + WAIT;
    while store.load_coordinator().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the bootstrap was never saved");
        std::thread::sleep(Duration::from_millis(10));
    }
    // Another coordinator takes the row; this one's next save is refused.
    store.claim_coordinator().unwrap();
    let player = PlayerId(1);
    let online = vec![game::OnlinePlayer { player, claim: None, claim_pending: false, report: 1, hop: ATTACHED_HOP }];
    link.send(HostMsg::Attach { host: h(0), epoch: 1, running: Vec::new(), online, generation: 0, boot: 1, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback });
    link.send(HostMsg::Request { id: RequestId { host: h(0), boot: 1, seq: 1 }, player, hop: ATTACHED_HOP, req: start_request(site) });
    while !thread.is_finished() {
        assert!(Instant::now() < deadline, "the fenced coordinator kept running");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(fenced.is_set(), "the process is fenced");
    assert!(!link.is_up());
    assert_eq!(thread.shutdown(Duration::from_secs(1)), ServerExit::Fenced);
    let (_, claims) = stored_state(&store, &map).unwrap();
    assert!(claims.is_empty(), "the fenced grant was never written");
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A second coordinator starts while the first still runs (an overlapping restart):
/// `ensure_coordinator_state` claims the row BEFORE it loads, so the first one — its
/// grant's save failing and retried — can commit nothing after that load: it is
/// fenced, and the state the second one runs from is the last one committed (loading
/// first would let the first commit a newer state the second never sees).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn overlapping_start_claims_before_it_loads() {
    let dir = scratch("overlap");
    let map = land_map();
    let store = open_store(&dir);
    let first = ensure_coordinator_state(&store, &map, None).unwrap().unwrap();
    let first_epoch = first.fence.epoch;
    let site = start_site(first.coordinator.history());
    let fenced = server::claims::Fenced::default();
    let (mut link, thread) = coordinator_rt::start_all_in_one(store.clone(), first, fenced.clone()).unwrap();
    let deadline = Instant::now() + WAIT;
    while store.load_coordinator().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the bootstrap was never saved");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The grant's saves fail for a while (retried every 250 ms).
    store.fail_next_writes(2);
    let player = PlayerId(1);
    let online = vec![game::OnlinePlayer { player, claim: None, claim_pending: false, report: 1, hop: ATTACHED_HOP }];
    link.send(HostMsg::Attach { host: h(0), epoch: 1, running: Vec::new(), online, generation: 0, boot: 1, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback });
    link.send(HostMsg::Request { id: RequestId { host: h(0), boot: 1, seq: 1 }, player, hop: ATTACHED_HOP, req: start_request(site) });
    while store.failing_writes_left() > 1 {
        assert!(Instant::now() < deadline, "the grant's save was never tried");
        std::thread::sleep(Duration::from_millis(5));
    }
    // The second coordinator starts now.
    let second = ensure_coordinator_state(&store, &map, None).unwrap().unwrap();
    assert!(second.fence.epoch > first_epoch, "claimed over the first");
    while !thread.is_finished() {
        assert!(Instant::now() < deadline, "the first coordinator kept running after the second claimed");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(fenced.is_set(), "the first is fenced");
    assert_eq!(thread.shutdown(Duration::from_secs(1)), ServerExit::Fenced);
    let (_, claims) = stored_state(&store, &map).unwrap();
    assert_eq!(claims, second.coordinator.claims(), "the second runs from the last committed state");
    assert!(claims.is_empty(), "the first committed nothing after the second's load");
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The coordinator thread fails (panics) mid-play: the host's exit skips its cut — the
/// coordinator's last save may lack parks and crossings that cut would record — and
/// exits `CoordinatorFailed` (process status `EXIT_COORDINATOR_FAILED`, non-zero); the
/// world restarts from its last committed saves (here: no cut ever, so no player record).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn coordinator_panic_skips_the_exit_cut() {
    install_props();
    let dir = scratch("panic");
    let map = land_map();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
    let store = open_store(&dir);
    let (a, _) = StorePlayers(store.clone()).resolve(&a_key).unwrap();
    let start = ensure_coordinator_state(&store, &map, None).unwrap().expect("a history");
    let site = start_site(start.coordinator.history());
    let mut life = Life::start(&store, &map, start, 1);
    life.send(ServerEvent::ClientConnected { client: 0, player: a, record: None });
    life.round();
    assert_eq!(life.ask(0, ClientPacket::StartAdventure(site)), Ok(site), "started");
    for _ in 0..5 {
        life.round();
    }
    let thread = life.coordinator.as_ref().expect("the coordinator");
    assert_eq!(thread.inspect(|_| -> () { panic!("test: the coordinator fails") }, Duration::from_secs(1)), None);
    life.until("the coordinator thread ended", |l| l.coordinator.as_ref().unwrap().is_finished());
    assert_eq!(life.exit(), ServerExit::CoordinatorFailed);
    assert_eq!(ServerExit::CoordinatorFailed.code(), server::claims::EXIT_COORDINATOR_FAILED);
    assert_ne!(server::claims::EXIT_COORDINATOR_FAILED, 0);

    let store = open_store(&dir);
    let (_, record) = StorePlayers(store.clone()).resolve(&a_key).unwrap();
    assert!(record.as_ref().is_none_or(|r| r.adventure.is_none()), "no exit cut wrote A's record: {record:?}");
    assert!(store.stored_regions().unwrap().is_empty(), "no exit cut wrote a region");
    let (_, claims) = stored_state(&store, &map).expect("the coordinator's own saves stand");
    assert_eq!(claims, vec![(a, Claim::Adventure(site))]);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a restart must resume exactly: the calendar (history tick and the calendar's anchor),
/// the site markers, the legends' length.
#[derive(Debug, PartialEq)]
struct Resumed {
    now: game::history::CalendarTick,
    anchor: ((u64, game::history::CalendarTick), u64),
    sites: Vec<game::SiteMarker>,
    legends: usize,
}

fn resumed(c: &game::Coordinator) -> Resumed {
    Resumed { now: c.history().now(), anchor: c.calendar_anchor(), sites: c.history().site_markers(), legends: c.history().log().len() }
}

/// Elastic 2a, single player (role `all`, SQLite): the bootstrap's first transition writes the
/// row's version 1; an adventure's grant and years of history are written moments before the
/// coordinator dies (after the commit, before its sends), and the restart resumes from exactly
/// that transition — calendar, site markers, legends' length — not from a periodic save.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn single_player_restart_resumes_history_where_it_was() {
    let dir = scratch("resume");
    let map = land_map();
    let store = open_store(&dir);
    let start = ensure_coordinator_state(&store, &map, None).unwrap().unwrap();
    assert_eq!(start.source, CoordinatorSource::Bootstrapped);
    let site = start_site(start.coordinator.history());
    let fenced = server::claims::Fenced::default();
    let (mut link, thread) = coordinator_rt::start_all_in_one(store.clone(), start, fenced.clone()).unwrap();
    let deadline = Instant::now() + WAIT;
    let first = loop {
        if let Some(row) = store.load_coordinator().unwrap() {
            break row;
        }
        assert!(Instant::now() < deadline, "the bootstrap was never written");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(first.version, 1, "the bootstrap's first transition writes version 1");
    let player = PlayerId(1);
    let id = RequestId { host: h(0), boot: 1, seq: 1 };
    let online = vec![game::OnlinePlayer { player, claim: None, claim_pending: false, report: 1, hop: ATTACHED_HOP }];
    link.send(HostMsg::Attach { host: h(0), epoch: 1, running: Vec::new(), online, generation: 0, boot: 1, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback });
    link.send(HostMsg::Request { id, player, hop: ATTACHED_HOP, req: start_request(site) });
    loop {
        assert!(Instant::now() < deadline, "the start was never answered");
        match link.recv() {
            Some(CoordMsg::Reply { id: got, .. }) if got == id => break,
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }

    // The next transition: years of history, decided while writes fail; once it commits the
    // thread dies before sending what it released.
    thread.fail_coordinator_writes(true);
    let changed_at = Instant::now();
    let target = thread
        .with_core(
            |c| {
                let h = c.history_mut();
                h.advance(game::history::CalendarTick(h.now().0 + 3 * game::history::DAYS_PER_YEAR * game::history::TICKS_PER_DAY));
                h.now()
            },
            WAIT,
        )
        .expect("the coordinator answers");
    let decided = thread.inspect(resumed, WAIT).expect("the coordinator answers");
    thread.kill_after_commit_before_send();
    thread.fail_coordinator_writes(false);
    while !thread.is_finished() {
        assert!(Instant::now() < deadline, "the transition never committed");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(thread.shutdown(Duration::from_secs(1)), ServerExit::CoordinatorFailed);
    assert!(changed_at.elapsed() < Duration::from_secs(20), "written within seconds");
    assert!(!fenced.is_set());

    let row = store.load_coordinator().unwrap().expect("written");
    let grid = game::history::SocietyGrid::from_world_map(&map);
    let last = resumed(&game::Coordinator::load(&row.core, &row.history, grid, game::coordinator::ReplicaId(0), 0).unwrap());
    // The change made just before the crash is in it (and whatever the core decided after it,
    // before the write: a day's turn at most).
    assert!(last.now >= target && last.now >= decided.now && last.legends >= decided.legends, "{last:?} vs {decided:?}");
    assert_eq!(last.sites, decided.sites);
    assert_eq!(stored_state(&store, &map).unwrap().1, vec![(player, Claim::Adventure(site))], "the grant is in it");
    store.shutdown();

    // The restart runs from exactly that transition.
    let store = open_store(&dir);
    let start = ensure_coordinator_state(&store, &map, None).unwrap().unwrap();
    assert_eq!(start.source, CoordinatorSource::Stored);
    assert_eq!(start.version, row.version);
    assert_eq!(resumed(&start.coordinator), last);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Elastic 2a: the attach and an adventure start at `site` for a channel link's host `h(0)`;
/// the request's id.
fn attach_and_start(link: &mut coordinator_rt::ChannelWorldLink, site: SiteId) -> RequestId {
    let player = PlayerId(1);
    let id = RequestId { host: h(0), boot: 1, seq: 1 };
    let online = vec![game::OnlinePlayer { player, claim: None, claim_pending: false, report: 1, hop: ATTACHED_HOP }];
    link.send(HostMsg::Attach { host: h(0), epoch: 1, running: Vec::new(), online, generation: 0, boot: 1, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback });
    link.send(HostMsg::Request { id, player, hop: ATTACHED_HOP, req: start_request(site) });
    id
}

/// Waits for the coordinator's first write (the bootstrap): its row.
fn first_write(store: &StoreHandle) -> world_store::StoredCoordinator {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(row) = store.load_coordinator().unwrap() {
            return row;
        }
        assert!(Instant::now() < deadline, "the bootstrap was never written");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Elastic 2a: a write that committed but whose answer was lost (the runtime took it as failed)
/// makes the next write a `Conflict`; the row holds the runtime's own write, so it takes its
/// version and writes again — once. The grant is written, its reply leaves exactly once, the
/// coordinator runs on.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_conflict_with_its_own_lost_commit_is_written_over_once() {
    let dir = scratch("lostanswer");
    let map = land_map();
    let store = open_store(&dir);
    let start = ensure_coordinator_state(&store, &map, None).unwrap().unwrap();
    let site = start_site(start.coordinator.history());
    let fenced = server::claims::Fenced::default();
    let (mut link, thread) = coordinator_rt::start_all_in_one(store.clone(), start, fenced.clone()).unwrap();
    assert_eq!(first_write(&store).version, 1);
    thread.lose_next_write_answer();
    let id = attach_and_start(&mut link, site);
    let deadline = Instant::now() + WAIT;
    let mut replies = 0;
    let quiet_from = loop {
        assert!(Instant::now() < deadline, "the reply never left");
        match link.recv() {
            Some(CoordMsg::Reply { id: got, .. }) if got == id => {
                replies += 1;
                break Instant::now();
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    };
    while quiet_from.elapsed() < Duration::from_millis(700) {
        match link.recv() {
            Some(CoordMsg::Reply { id: got, .. }) if got == id => replies += 1,
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    assert_eq!(replies, 1, "the reply left once");
    assert!(!thread.is_finished() && !fenced.is_set(), "the coordinator runs on");
    let row = store.load_coordinator().unwrap().unwrap();
    assert!(row.version >= 3, "the lost commit (2) was written over (3): {}", row.version);
    assert_eq!(stored_state(&store, &map).unwrap().1, vec![(PlayerId(1), Claim::Adventure(site))], "one grant");
    assert_eq!(thread.shutdown(Duration::from_secs(10)), ServerExit::Clean);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Elastic 2b (decision 1): another writer moves the coordinator row right before this
/// coordinator's next write (the test hook writes the row's own projection at the next version):
/// the write is a `Conflict` the row does not explain as this process's own lost commit. The
/// coordinator rebases onto the row, replays what it applied since its last commit, and writes
/// again: the grant commits after one rebase, its reply leaves once, the process runs on, one
/// conflict counted.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_conflict_rebases_instead_of_exiting() {
    let dir = scratch("otherwriter");
    let map = land_map();
    let store = open_store(&dir);
    let start = ensure_coordinator_state(&store, &map, None).unwrap().unwrap();
    let site = start_site(start.coordinator.history());
    let fenced = server::claims::Fenced::default();
    let (mut link, thread) = coordinator_rt::start_all_in_one(store.clone(), start, fenced.clone()).unwrap();
    let row = first_write(&store);
    thread.write_conflict_once();
    let id = attach_and_start(&mut link, site);
    let deadline = Instant::now() + WAIT;
    let mut replies = 0;
    let quiet_from = loop {
        assert!(Instant::now() < deadline, "the reply never left");
        assert!(!thread.is_finished(), "the coordinator stopped at the conflict");
        match link.recv() {
            Some(CoordMsg::Reply { id: got, .. }) if got == id => {
                replies += 1;
                break Instant::now();
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    };
    while quiet_from.elapsed() < Duration::from_millis(700) {
        match link.recv() {
            Some(CoordMsg::Reply { id: got, .. }) if got == id => replies += 1,
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    assert_eq!(replies, 1, "the reply left once");
    assert!(!thread.is_finished() && !fenced.is_set(), "the coordinator runs on");
    assert_eq!(thread.conflicts(), 1, "one conflict counted (mmo_coordinator_conflicts_total)");
    let after = store.load_coordinator().unwrap().unwrap();
    assert!(after.version >= row.version + 2, "the other write ({}) and the rebased one: {}", row.version + 1, after.version);
    assert_eq!(stored_state(&store, &map).unwrap().1, vec![(PlayerId(1), Claim::Adventure(site))], "one grant");
    assert_eq!(thread.shutdown(Duration::from_secs(10)), ServerExit::Clean);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host id from its number.
#[allow(dead_code)]
fn h(n: u64) -> game::HostId {
    game::HostId(n)
}

// ---- Elastic 1 Task 6: the coordinator's login listener -------------------------------------

const LOGIN_TEST_TOKEN: &str = "login-listener-test-token";

/// A coordinator listening for hosts and for the login service (world "w"), as
/// `run_coordinator` starts it, on a SQLite store (no host attaches in these tests).
fn listening_with_login(dir: &Path) -> (StoreHandle, coordinator_rt::ListeningCoordinator) {
    let store = open_store(dir);
    let start = ensure_coordinator_state(&store, &land_map(), None).unwrap().unwrap();
    let login = coordinator_rt::LoginListen {
        bind: "127.0.0.1:0".parse().unwrap(),
        token: LOGIN_TEST_TOKEN.into(),
        cert_hash_file: Some(dir.join("login-link.cert-sha256")),
    };
    let listening = coordinator_rt::start_listening_as(
        store.clone(),
        start,
        "127.0.0.1:0".parse().unwrap(),
        LOGIN_TEST_TOKEN.into(),
        Some(dir.join("coordinator.cert-sha256")),
        server::claims::Fenced::default(),
        None,
        game::CrashPolicy::Rollback,
        "w",
        Some(login),
    )
    .expect("the coordinator listens");
    (store, listening)
}

/// Whether a dialer of `role` to (`addr`, `pin`) is closed — it saw its link come up (the
/// hello passed), then go down — within the wait. `$in` / `$out`: the dialer's message types.
macro_rules! closed_after_up {
    ($in:ty, $out:ty, $addr:expr, $pin:expr, $role:expr) => {{
        let hello = svc_link::LinkHello::new(LOGIN_TEST_TOKEN.into(), $role, "w");
        let (_tx, events) = svc_link::LinkDialer::connect::<$in, $out>($addr, $pin.to_string(), hello);
        let deadline = Instant::now() + WAIT;
        let (mut up, mut closed) = (false, false);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match events.recv_timeout(left) {
                Ok(svc_link::LinkEvent::Up(_)) => up = true,
                Ok(svc_link::LinkEvent::Down(_)) if up => {
                    closed = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        closed
    }};
}

/// A host's hello on the coordinator's LOGIN listener is closed (only the login service is
/// served there); the login service's own hello stays up, and its order — no host attached to
/// take it — is dropped, not answered.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_host_peer_on_the_login_listener_is_closed() {
    let dir = scratch("loginlistener");
    let (store, listening) = listening_with_login(&dir);
    let (addr, pin) = listening.login.clone().expect("a login listener");
    let host = svc_link::PeerRole::Host { id: h(1), boot: 1, reg: 1 };
    assert!(closed_after_up!(login_api::HostToLogin, login_api::LoginToHost, addr, &pin, host), "a host is closed on the login listener");
    let hello = svc_link::LinkHello::new(LOGIN_TEST_TOKEN.into(), svc_link::PeerRole::Login, "w");
    let (tx, events) = svc_link::LinkDialer::connect::<login_api::HostToLogin, login_api::LoginToHost>(addr, pin.clone(), hello);
    assert!(matches!(events.recv_timeout(WAIT), Ok(svc_link::LinkEvent::Up(_))));
    tx.send(login_api::LoginToHost::SpawnPlayer { player_token: "ab".repeat(16), session_epoch: 1 }).unwrap();
    match events.recv_timeout(Duration::from_millis(700)) {
        Err(_) => {}
        Ok(other) => panic!("no host attached: nothing answers, the link stays up — got {other:?}"),
    }
    let want = coordinator_rt::LoginCounts { orders: 1, orders_dropped: 1, ends_dropped: 0 };
    assert_eq!(listening.login_counts(), Some(want), "received, and counted as dropped: no gate host");
    assert_eq!(listening.thread.shutdown(Duration::from_secs(10)), ServerExit::Clean);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The reverse: the login service's hello on the coordinator's HOST listener is closed.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_login_peer_on_the_host_listener_is_closed() {
    let dir = scratch("hostlistener");
    let (store, listening) = listening_with_login(&dir);
    let (addr, pin) = (listening.addr, listening.cert_sha256.clone());
    assert!(
        closed_after_up!(game::CoordMsg, HostMsg, addr, &pin, svc_link::PeerRole::Login),
        "the login service is closed on the hosts' listener"
    );
    assert_eq!(listening.thread.shutdown(Duration::from_secs(10)), ServerExit::Clean);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
