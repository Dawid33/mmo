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
use game::history::{EmbarkRefusal, History, SiteId};
use game::{
    Claim, ClientId, ClientPacket, CoordMsg, EmbarkRequest, HostMsg, InlineLodBuilder, InlineSpawner, LodBuilder,
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

/// A player-founded site's request: two members of the first site, into the first
/// region (in a row from (2, 2)) where a player may found one.
fn embark_request(h: &History) -> EmbarkRequest {
    let s = h.sites().next().expect("a site");
    let region = (2..46)
        .flat_map(|z| (2..46).map(move |x| RegionCoords::new(x, z)))
        .find(|r| h.player_embark_allowed(*r).is_ok())
        .expect("room to embark");
    EmbarkRequest { civ: s.civ, people: s.members.iter().copied().take(2).collect(), region }
}

fn player_sites(h: &History) -> Vec<SiteId> {
    h.sites().filter(|s| s.player_founded).map(|s| s.id).collect()
}

/// The coordinator's state as the store holds it, decoded (`None` before its first save).
fn stored_state(store: &StoreHandle, map: &WorldMap) -> Option<(History, Vec<(PlayerId, Claim)>)> {
    let bytes = store.load_coordinator().unwrap()?;
    let state = game::CoordinatorState::decode(&bytes).expect("the saved state decodes");
    let grid = game::history::SocietyGrid::from_world_map(map);
    let history = game::history::persist::decode(&state.history, grid).expect("its history decodes");
    Some((history, state.claims))
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

    /// `c`'s `EmbarkResult`s so far, taken out of the inbox.
    fn embark_results(&mut self, c: ClientId) -> Vec<Result<SiteId, EmbarkRefusal>> {
        let mut out = Vec::new();
        self.inbox.retain(|(to, p)| match p {
            ServerPacket::EmbarkResult(r) if *to == Some(c) => {
                out.push(*r);
                false
            }
            _ => true,
        });
        out
    }

    /// `c` sends `packet`; its one `EmbarkResult`.
    fn ask(&mut self, c: ClientId, packet: ClientPacket) -> Result<SiteId, EmbarkRefusal> {
        self.send(ServerEvent::ClientPacket(packet, c));
        self.until("an EmbarkResult", |l| l.inbox.iter().any(|(to, p)| *to == Some(c) && matches!(p, ServerPacket::EmbarkResult(_))));
        let results = self.embark_results(c);
        assert_eq!(results.len(), 1, "one EmbarkResult: {results:?}");
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

/// An `AllInOne` life on a SQLite world: a player embarks, the server exits in order,
/// the world restarts → the site exists, the player's record holds the fortress, the
/// coordinator's saved claim comes back as `Fortress(site)` and holds once the player
/// is online again (another player's return to it is refused `InUse`).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn state_survives_restart() {
    install_props();
    let dir = scratch("restart");
    let map = land_map();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
    let b_key = PlayerKey::Local(game::PlayerName::parse("b").unwrap());

    // Life 1: bootstrapped, saved at once; A embarks.
    let store = open_store(&dir);
    let (a, _) = StorePlayers(store.clone()).resolve(&a_key).unwrap();
    let (b, _) = StorePlayers(store.clone()).resolve(&b_key).unwrap();
    let start = ensure_coordinator_state(&store, &map, None).unwrap().expect("a history");
    assert_eq!(start.source, CoordinatorSource::Bootstrapped);
    let req = embark_request(start.coordinator.history());
    let mut life = Life::start(&store, &map, start, 1);
    life.send(ServerEvent::ClientConnected { client: 0, player: a, record: None });
    life.round();
    let site = life.ask(0, ClientPacket::Embark(req.clone())).expect("embarked");
    // Durable before visible: the reply came after the founding was saved.
    let (saved, claims) = stored_state(&store, &map).expect("saved before the reply");
    assert_eq!(player_sites(&saved), vec![site], "the site is in the store before the player hears of it");
    assert_eq!(claims, vec![(a, Claim::Fortress(site))]);
    for _ in 0..5 {
        life.round();
    }
    assert_eq!(life.exit(), ServerExit::Clean);

    // Life 2.
    let store = open_store(&dir);
    let (again, record) = StorePlayers(store.clone()).resolve(&a_key).unwrap();
    assert_eq!(again, a);
    let record = record.expect("the exit's cut saved A's record");
    assert_eq!(record.fortress, Some(site), "A's record holds the fortress");
    assert_eq!(store.load_history().unwrap(), None, "no cut wrote the R1 history row");
    let start = ensure_coordinator_state(&store, &map, None).unwrap().expect("a history");
    assert_eq!(start.source, CoordinatorSource::Stored);
    assert_eq!(player_sites(start.coordinator.history()), vec![site], "the site exists after the restart");
    assert!(start.coordinator.history().returnable(site));
    assert_eq!(start.coordinator.claim_of(a), Some(Claim::Fortress(site)), "the claim comes back");
    let mut life = Life::start(&store, &map, start, 2);
    life.send(ServerEvent::ClientConnected { client: 0, player: a, record: Some(record) });
    life.round();
    assert!(
        life.inbox.iter().any(|(to, p)| *to == Some(0)
            && matches!(p, ServerPacket::Fortress(Some(held)) if held.site == site)),
        "A is told its fortress: Fortress(Some)"
    );
    life.send(ServerEvent::ClientConnected { client: 1, player: b, record: None });
    for _ in 0..5 {
        life.round();
    }
    assert_eq!(
        life.ask(1, ClientPacket::ReturnTo(site)),
        Err(EmbarkRefusal::InUse),
        "A is online with its fortress: B may not take it"
    );
    assert_eq!(life.exit(), ServerExit::Clean);
    let store = open_store(&dir);
    let (saved, claims) = stored_state(&store, &map).expect("saved");
    assert_eq!(player_sites(&saved), vec![site], "still one site");
    assert_eq!(claims, vec![(a, Claim::Fortress(site))]);
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
        host: 0,
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

/// Saves that fail (`StoreHandle::fail_next_writes`) hold back every directive and
/// claim-granting reply: none reaches the host before a save holding the founding has
/// committed; once one commits they all arrive. Ungated messages (the attach echo, the
/// view, the region's people) are not held while nothing gated is ahead of them.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn directive_not_sent_before_save() {
    const FAILURES: u32 = 4;
    let dir = scratch("gated");
    let map = land_map();
    let store = open_store(&dir);
    let start = ensure_coordinator_state(&store, &map, None).unwrap().unwrap();
    let req = embark_request(start.coordinator.history());
    store.fail_next_writes(FAILURES);
    let fenced = server::claims::Fenced::default();
    let (mut link, thread) = coordinator_rt::start_all_in_one(store.clone(), start, fenced.clone()).unwrap();
    let player = PlayerId(1);
    let id = RequestId { host: 0, boot: 1, seq: 1 };
    let online = vec![game::OnlinePlayer { player, claim: None, claim_pending: false, report: 1, hop: ATTACHED_HOP }];
    link.send(HostMsg::Attach { host: 0, epoch: 1, running: Vec::new(), online, generation: 0, boot: 1, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() });
    // The fortress region runs: the founding embodies the party there by directives.
    link.send(HostMsg::RegionStarted { rc: req.region, incarnation: 1 });
    link.send(HostMsg::Request { id, player, hop: ATTACHED_HOP, req: WorldRequest::Embark(req.clone()) });

    let deadline = Instant::now() + WAIT;
    let (mut directives, mut replies, mut attached) = (0, 0, false);
    while directives < req.people.len() || replies == 0 {
        assert!(Instant::now() < deadline, "timed out: {directives} directives, {replies} replies");
        let Some(msg) = link.recv() else {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        if msg.is_gated() {
            assert_eq!(store.failing_writes_left(), 0, "{msg:?} left while saves were still failing");
            let (saved, _) = stored_state(&store, &map).expect("a save committed");
            assert_eq!(player_sites(&saved).len(), 1, "{msg:?} left before the founding was durable");
        }
        match msg {
            CoordMsg::Attached { epoch: 1 } => attached = true,
            CoordMsg::Directive { incarnation, .. } => {
                assert_eq!(incarnation, 1);
                directives += 1;
            }
            CoordMsg::Reply { id: got, reply: WorldReply::Embarked { region, .. } } => {
                assert_eq!((got, region), (id, req.region));
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
    let req = embark_request(start.coordinator.history());
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
    link.send(HostMsg::Attach { host: 0, epoch: 1, running: Vec::new(), online, generation: 0, boot: 1, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() });
    link.send(HostMsg::Request { id: RequestId { host: 0, boot: 1, seq: 1 }, player, hop: ATTACHED_HOP, req: WorldRequest::Embark(req) });
    while !thread.is_finished() {
        assert!(Instant::now() < deadline, "the fenced coordinator kept running");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(fenced.is_set(), "the process is fenced");
    assert!(!link.is_up());
    assert_eq!(thread.shutdown(Duration::from_secs(1)), ServerExit::Fenced);
    let (saved, _) = stored_state(&store, &map).unwrap();
    assert!(player_sites(&saved).is_empty(), "the fenced founding was never written");
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A second coordinator starts while the first still runs (an overlapping restart):
/// `ensure_coordinator_state` claims the row BEFORE it loads, so the first one — its
/// founding's save failing and retried — can commit nothing after that load: it is
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
    let req = embark_request(first.coordinator.history());
    let fenced = server::claims::Fenced::default();
    let (mut link, thread) = coordinator_rt::start_all_in_one(store.clone(), first, fenced.clone()).unwrap();
    let deadline = Instant::now() + WAIT;
    while store.load_coordinator().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the bootstrap was never saved");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The founding's saves fail for a while (retried every 250 ms).
    store.fail_next_writes(2);
    let player = PlayerId(1);
    let online = vec![game::OnlinePlayer { player, claim: None, claim_pending: false, report: 1, hop: ATTACHED_HOP }];
    link.send(HostMsg::Attach { host: 0, epoch: 1, running: Vec::new(), online, generation: 0, boot: 1, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() });
    link.send(HostMsg::Request { id: RequestId { host: 0, boot: 1, seq: 1 }, player, hop: ATTACHED_HOP, req: WorldRequest::Embark(req) });
    while store.failing_writes_left() > 1 {
        assert!(Instant::now() < deadline, "the founding's save was never tried");
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
    let (saved, _) = stored_state(&store, &map).unwrap();
    assert_eq!(
        player_sites(&saved),
        player_sites(second.coordinator.history()),
        "the second runs from the last committed state"
    );
    assert!(player_sites(&saved).is_empty(), "the first committed nothing after the second's load");
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
    let req = embark_request(start.coordinator.history());
    let mut life = Life::start(&store, &map, start, 1);
    life.send(ServerEvent::ClientConnected { client: 0, player: a, record: None });
    life.round();
    let site = life.ask(0, ClientPacket::Embark(req)).expect("embarked");
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
    assert!(record.as_ref().is_none_or(|r| r.fortress.is_none()), "no exit cut wrote A's record: {record:?}");
    assert!(store.stored_regions().unwrap().is_empty(), "no exit cut wrote a region");
    let (saved, _) = stored_state(&store, &map).expect("the coordinator's own saves stand");
    assert_eq!(player_sites(&saved), vec![site]);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
