//! R2b Task 9: process roles. A world split into a coordinator process (the
//! history, claims and world requests behind a service-link listener) and a host process
//! (regions, the client edge, its save cuts) dialing it with a `QuicWorldLink` plays like
//! the same world in one process (`all`); the split roles refuse a SQLite store; a host
//! that starts before its coordinator simulates, queues its players' requests and
//! attaches once the coordinator is up.
//!
//! Both "processes" run in this test process — the coordinator runtime on its thread
//! behind a real `svc-link` listener on localhost, the host's `WorldManager` (inline
//! regions) over the dialer — against Postgres (`MMO_TEST_DATABASE_URL`, e.g.
//! `just test-r2`; skipped with a message when unset). The test client is a local-auth
//! player (`PlayerKey::Local`) whose session the rig opens on the manager directly.
//! Its own test binary: process-global block props, real threads and sockets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver};
use game::history::log::LogKind;
use game::history::{
    CalendarTick, EmbarkRefusal, History, LegendsFilter, LegendsQuery, Location, PersonId, Sex, SiteId,
};
use game::{
    ClientId, ClientPacket, EmbarkRequest, InlineSpawner, Ownership, PlayerId,
    RegionCoords, RegionOutput, RegionSpawner, ServerEvent, ServerPacket, WorldLink, WorldManager, WorldMap,
};
use server::claims::{Fenced, HostClaims, ServerExit};
use server::coordinator_rt::{self, CoordinatorThread};
use server::player_store::{PlayerKey, PlayerStore, StorePlayers};
use server::roles::{QuicWorldLink, Role, SPLIT_ROLES_NEED_POSTGRES};
use server::world_file::{ensure_coordinator_state, WorldSource};
use world_store::{StoreHandle, StoreUrl};

/// Wall time any wait below may take before the test fails (a dialer's reconnect
/// backoff tops out at 5 s).
const WAIT: Duration = Duration::from_secs(30);
const TOKEN: &str = "roles-test-service-token";
const A: ClientId = 0;

// ---- world and store ------------------------------------------------------------------

/// A throwaway Postgres database from `MMO_TEST_DATABASE_URL` (`pg-test-support`).
struct PgDb(pg_test_support::PgTestDb);

impl PgDb {
    fn create(tag: &str) -> Option<PgDb> {
        let db = pg_test_support::PgTestDb::create(&format!("mmo_roles_{tag}"));
        if db.is_none() {
            eprintln!("{} (run `just test-r2`)", pg_test_support::SKIP_MSG);
        }
        db.map(PgDb)
    }

    /// A handle as one process opens it.
    fn open(&self) -> StoreHandle {
        StoreHandle::open(&StoreUrl::Postgres(self.0.url.clone()), world_store::Dials::BUILT, world_store::OpenMode::Writer)
            .unwrap()
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmo-roles-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
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

/// Flat stud ground under every chunk column of every region: ground without worldgen.
fn stud_flat_seed(_rc: RegionCoords) -> game::StudRegionSeed {
    let soil = game::StudProfile { cover: game::MAT_GRASS, cover_plates: 1, soil: game::MAT_DIRT, soil_plates: 5 };
    let n = game::REGION_CHUNKS;
    let ground = (0..n)
        .flat_map(|x| {
            (0..n).map(move |z| (game::ColumnCoords::new(x, z), game::StudColumnGround::flat(30, soil, game::MAT_STONE, -2000)))
        })
        .collect();
    game::StudRegionSeed { ground, features: Vec::new(), ..Default::default() }
}

/// Two members of the first site, into the first region (in a row from (2, 2)) where a
/// player may found one.
fn embark_request(h: &History) -> EmbarkRequest {
    let s = h.sites().next().expect("a site");
    let region = (2..46)
        .flat_map(|z| (2..46).map(move |x| RegionCoords::new(x, z)))
        .find(|r| h.player_embark_allowed(*r).is_ok())
        .expect("room to embark");
    EmbarkRequest { civ: s.civ, people: s.members.iter().copied().take(2).collect(), region }
}

/// The coordinator's saved history.
fn stored_history(store: &StoreHandle, map: &WorldMap) -> History {
    let bytes = store.load_coordinator().unwrap().expect("a saved coordinator state");
    let state = game::CoordinatorState::decode(&bytes).expect("the saved state decodes");
    let grid = game::history::SocietyGrid::from_world_map(map);
    game::history::persist::decode(&state.history, grid).expect("its history decodes")
}

/// The player-founded sites in the coordinator's saved state.
fn stored_player_sites(store: &StoreHandle, map: &WorldMap) -> Vec<SiteId> {
    let Some(bytes) = store.load_coordinator().unwrap() else { return Vec::new() };
    let state = game::CoordinatorState::decode(&bytes).expect("the saved state decodes");
    let grid = game::history::SocietyGrid::from_world_map(map);
    let history = game::history::persist::decode(&state.history, grid).expect("its history decodes");
    history.sites().filter(|s| s.player_founded).map(|s| s.id).collect()
}

// ---- the coordinator process ------------------------------------------------------------

/// The coordinator process's half: its own store handle, the runtime behind a listener.
struct Coordinator {
    thread: CoordinatorThread,
    store: StoreHandle,
}

impl Coordinator {
    /// Starts it on `bind`, writing its pin to `cert_file` (as `VOXEL_ROLE=coordinator`).
    fn start(db: &PgDb, map: &WorldMap, bind: std::net::SocketAddr, cert_file: PathBuf) -> (Coordinator, std::net::SocketAddr) {
        let store = db.open();
        let start = ensure_coordinator_state(&store, map, None).unwrap().expect("a history");
        let listening =
            coordinator_rt::start_listening(store.clone(), start, bind, TOKEN.into(), Some(cert_file), Fenced::default())
                .expect("the coordinator listens");
        (Coordinator { thread: listening.thread, store }, listening.addr)
    }

    fn stop(self) {
        assert_eq!(self.thread.shutdown(Duration::from_secs(10)), ServerExit::Clean);
        self.store.shutdown();
    }
}

/// Host `ordinal`'s link to the coordinator at `addr`, its pin read from `cert_file`
/// (waited for); its hello carries the host's real claim `epoch` (the coordinator admits
/// only the stored one).
fn quic_link(addr: std::net::SocketAddr, cert_file: PathBuf, ordinal: u32, boot: u64, epoch: u64) -> QuicWorldLink {
    let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Host { ordinal, boot, epoch });
    QuicWorldLink::connect(addr, move || svc_link::read_cert_hash("MMO_ROLES_TEST_UNSET_PIN", &cert_file), hello)
}

/// A localhost UDP port nobody holds right now.
fn free_udp_addr() -> std::net::SocketAddr {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap()
}

// ---- the host process -------------------------------------------------------------------

/// A host's server life over the link `link` makes of its claim epoch (inline regions, its
/// claims, no coordinator thread unless `coordinator` is the `all` role's), with the test
/// client's packets collected.
struct Host {
    manager: WorldManager<InlineSpawner>,
    claims: HostClaims,
    coordinator: Option<CoordinatorThread>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    /// Every packet but the regions' own stream and the daily broadcasts.
    inbox: Vec<(Option<ClientId>, ServerPacket)>,
    started: Instant,
}

impl Host {
    fn start(
        store: &StoreHandle,
        map: &WorldMap,
        ownership: Ownership,
        link: impl FnOnce(u64) -> Box<dyn WorldLink + Send>,
        coordinator: Option<CoordinatorThread>,
        boot: u64,
    ) -> Host {
        // As `server::run`: the store is tied to stud terrain before any region loads.
        server::world_file::bind_world_terrain(store).expect("a stud world");
        let claims = HostClaims::claim(store.clone(), ownership, Some(map.extent()), store.super_region_regions()).unwrap();
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let mut manager = WorldManager::new(
            InlineSpawner::default().with_stud_genesis(std::sync::Arc::new(stud_flat_seed)),
            out_send,
            region_out_send,
            Box::new(game::NullLodBuilder::default()),
        );
        manager.set_ownership(ownership, store.super_region_regions());
        manager.set_stored_regions(store.stored_regions().unwrap());
        manager.restore_in_flight(store.load_in_flight(claims.fence().in_flight_scope()).unwrap());
        manager.set_world_link(link(claims.fence().claim_epoch()), boot, 0);
        manager.set_world_map(map.clone());
        Host { manager, claims, coordinator, region_out, packets, inbox: Vec::new(), started: Instant::now() }
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

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
            // The regions' own stream, and what follows the wall-clock calendar (the
            // day's turn: calendar, sites, notices), differ between any two runs.
            if !matches!(
                p,
                ServerPacket::Region(..)
                    | ServerPacket::GameEvent(_)
                    | ServerPacket::HashedGameEvent(..)
                    | ServerPacket::SyncClock(..)
                    | ServerPacket::Calendar(_)
                    | ServerPacket::Sites(_)
                    | ServerPacket::Notice(_)
            ) {
                self.inbox.push((to, p));
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    fn until(&mut self, what: &str, done: impl Fn(&mut Host) -> bool) {
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

    fn has(&self, c: ClientId, pred: impl Fn(&ServerPacket) -> bool) -> bool {
        self.inbox.iter().any(|(to, p)| *to == Some(c) && pred(p))
    }

    fn embark_results(&self, c: ClientId) -> Vec<Result<SiteId, EmbarkRefusal>> {
        self.inbox
            .iter()
            .filter_map(|(to, p)| match p {
                ServerPacket::EmbarkResult(r) if *to == Some(c) => Some(*r),
                _ => None,
            })
            .collect()
    }

    fn legends_pages(&self, c: ClientId) -> usize {
        self.inbox.iter().filter(|(to, p)| *to == Some(c) && matches!(p, ServerPacket::Legends(_))).count()
    }

    /// The orderly exit: park, the in-process coordinator's last save (if any), the cut.
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

/// The script both runs play: A connects, embarks, asks the founded site's legends,
/// retires, asks them again. Returns every packet A got (bar the regions' stream and
/// the day's broadcasts), in order, as text.
fn play_script(host: &mut Host, player: PlayerId, req: &EmbarkRequest) -> Vec<String> {
    host.until("attached", |h| h.manager.world_attached());
    host.send(ServerEvent::ClientConnected { client: A, player, record: None });
    for _ in 0..3 {
        host.round();
    }
    host.send(ServerEvent::ClientPacket(ClientPacket::Embark(req.clone()), A));
    host.until("an EmbarkResult", |h| !h.embark_results(A).is_empty());
    let site = host.embark_results(A)[0].expect("embarked");
    let query = LegendsQuery { filter: LegendsFilter::Site(site), before: None, limit: 20 };
    host.send(ServerEvent::ClientPacket(ClientPacket::RequestLegends(query.clone()), A));
    host.until("a Legends page", |h| h.legends_pages(A) == 1);
    host.send(ServerEvent::ClientPacket(ClientPacket::Retire, A));
    host.until("Fortress(None) after retiring", |h| h.has(A, |p| matches!(p, ServerPacket::Fortress(None))));
    host.send(ServerEvent::ClientPacket(ClientPacket::RequestLegends(query), A));
    host.until("a second Legends page", |h| h.legends_pages(A) == 2);
    for _ in 0..5 {
        host.round();
    }
    host.inbox
        .iter()
        .filter(|(to, _)| *to == Some(A) || to.is_none())
        .map(|(to, p)| match p {
            // The map is the same value in both runs; its text is large.
            ServerPacket::WorldMap(m) => format!("{to:?} WorldMap {}x{}", m.width, m.height),
            p => format!("{to:?} {p:?}"),
        })
        .collect()
}

/// The world split in two processes plays like the same world in one: the same script
/// (connect, embark, legends, retire, legends) gets the same packets, and the
/// coordinator process — the only writer of history state — saved the one site.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn split_world_plays_like_all_in_one() {
    install_props();
    let map = land_map();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());

    // `all`: the coordinator on a thread, joined by a channel link.
    let Some(all_db) = PgDb::create("all") else { return };
    let store = all_db.open();
    let (a, _) = StorePlayers(store.clone()).resolve(&a_key).unwrap();
    let start = ensure_coordinator_state(&store, &map, None).unwrap().expect("a history");
    let req = embark_request(start.coordinator.history());
    let claims_fenced = Fenced::default();
    let (link, thread) = coordinator_rt::start_all_in_one(store.clone(), start, claims_fenced).unwrap();
    let mut host = Host::start(&store, &map, Ownership::AllInOne, |_| Box::new(link), Some(thread), 1);
    let all_packets = play_script(&mut host, a, &req);
    assert_eq!(host.exit(), ServerExit::Clean);
    let store = all_db.open();
    let all_sites = stored_player_sites(&store, &map);
    store.shutdown();

    // Split: a coordinator process and a host process over the service link.
    let Some(split_db) = PgDb::create("split") else { return };
    let dir = scratch("split");
    let cert_file = dir.join("coordinator.cert-sha256");
    let (coordinator, addr) = Coordinator::start(&split_db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
    let host_store = split_db.open();
    let (a_split, _) = StorePlayers(host_store.clone()).resolve(&a_key).unwrap();
    assert_eq!(a_split, a, "the same player id in both worlds");
    let link = |epoch| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file, 0, 1, epoch)) };
    let mut host = Host::start(&host_store, &map, Ownership::Static { ordinal: 0, hosts: 1 }, link, None, 1);
    let split_packets = play_script(&mut host, a, &req);
    assert_eq!(host.exit(), ServerExit::Clean);
    host_store.shutdown();
    coordinator.stop();
    let split_store = split_db.open();
    let split_sites = stored_player_sites(&split_store, &map);
    assert_eq!(split_store.load_history().unwrap(), None, "the host wrote no history row");
    split_store.shutdown();

    assert!(all_packets.iter().any(|p| p.contains("EmbarkResult(Ok(")), "{all_packets:#?}");
    assert_eq!(split_packets, all_packets, "the split world sends the client what the one-process world sends");
    assert_eq!(all_sites.len(), 1, "one site founded: {all_sites:?}");
    assert_eq!(split_sites, all_sites, "the coordinator process saved the same site");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `host` and `coordinator` refuse a world that is not in Postgres (SQLite, in-memory,
/// procedural): several processes write it; `all` takes any.
#[test]
fn sqlite_refuses_split_roles() {
    let sqlite = WorldSource::Store { url: StoreUrl::Sqlite("worlds/x.sqlite".into()), import: None };
    let memory = WorldSource::Store { url: StoreUrl::Memory, import: None };
    let pg = WorldSource::Store { url: StoreUrl::Postgres("postgres://u@h/db".into()), import: None };
    for role in [Role::Host, Role::Coordinator] {
        for source in [&sqlite, &memory, &WorldSource::Procedural] {
            assert_eq!(role.check_store(source), Err(SPLIT_ROLES_NEED_POSTGRES.to_string()), "{role:?} on {source:?}");
        }
        assert_eq!(role.check_store(&pg), Ok(()));
    }
    for source in [&sqlite, &memory, &WorldSource::Procedural, &pg] {
        assert_eq!(Role::All.check_store(source), Ok(()));
    }
    assert_eq!(SPLIT_ROLES_NEED_POSTGRES, "split roles need a Postgres world (VOXEL_WORLD_DB=postgres://…)");
}

/// A host started before its coordinator (no pin written yet) simulates — its player
/// connects, its regions tick — and queues the player's requests; once the coordinator
/// comes up the host attaches and every request is answered once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_survives_coordinator_absence_at_start() {
    install_props();
    let Some(db) = PgDb::create("absent") else { return };
    let map = land_map();
    let dir = scratch("absent");
    let cert_file = dir.join("coordinator.cert-sha256");
    let addr = free_udp_addr();

    // The bootstrap the coordinator will load, read ahead for the request.
    let req = {
        let store = db.open();
        let start = ensure_coordinator_state(&store, &map, None).unwrap().expect("a history");
        store.shutdown();
        embark_request(start.coordinator.history())
    };
    let host_store = db.open();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
    let (a, _) = StorePlayers(host_store.clone()).resolve(&a_key).unwrap();
    let link = |epoch| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file.clone(), 0, 7, epoch)) };
    let mut host = Host::start(&host_store, &map, Ownership::Static { ordinal: 0, hosts: 1 }, link, None, 7);
    host.send(ServerEvent::ClientConnected { client: A, player: a, record: None });
    let tick_before = host.manager.spawner_mut().world_tick();
    for _ in 0..20 {
        host.round();
    }
    assert!(!host.manager.running_regions().is_empty(), "the host runs its player's region");
    assert!(host.manager.spawner_mut().world_tick() > tick_before, "and simulates without a coordinator");
    host.send(ServerEvent::ClientPacket(ClientPacket::Embark(req.clone()), A));
    let query = LegendsQuery { filter: LegendsFilter::All, before: None, limit: 5 };
    host.send(ServerEvent::ClientPacket(ClientPacket::RequestLegends(query), A));
    for _ in 0..10 {
        host.round();
    }
    assert!(!host.manager.world_attached(), "no coordinator: not attached");
    assert_eq!(host.manager.pending_world_requests(), 2, "both requests wait");
    assert_eq!(host.manager.world_outbox_len(), 3, "in the outbox, behind the player's PlayerOnline");
    assert!(host.embark_results(A).is_empty() && host.legends_pages(A) == 0, "nothing answered yet");

    // The coordinator comes up (writing its pin): the host attaches, the queue drains.
    let (coordinator, _) = Coordinator::start(&db, &map, addr, cert_file);
    host.until("attached to the late coordinator", |h| h.manager.world_attached());
    host.until("the embark answered", |h| !h.embark_results(A).is_empty());
    host.until("the legends answered", |h| h.legends_pages(A) == 1);
    for _ in 0..10 {
        host.round();
    }
    let results = host.embark_results(A);
    assert_eq!(results.len(), 1, "one EmbarkResult: {results:?}");
    let site = results[0].expect("embarked");
    assert_eq!(host.legends_pages(A), 1, "one Legends page");
    assert_eq!(host.manager.pending_world_requests(), 0);
    assert_eq!(stored_player_sites(&coordinator.store, &map), vec![site], "founded once, saved by the coordinator");
    assert_eq!(host.exit(), ServerExit::Clean);
    host_store.shutdown();
    coordinator.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Rounds of both hosts until `done` holds (at most [`WAIT`]).
fn until_both(h0: &mut Host, h1: &mut Host, what: &str, done: impl Fn(&mut Host, &mut Host) -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done(h0, h1) {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        h0.round();
        h1.round();
    }
}

/// A record standing in `region` (falling onto its floor).
fn standing_in(region: RegionCoords) -> game::PlayerRecord {
    let half = game::REGION_SIZE / 2.0;
    game::PlayerRecord {
        client: game::Client::default(),
        pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(half, 40.0, half)),
        region,
        fortress: None,
        adventure: None,
    }
}

/// Two members of the first site, not taken in `taken`, into the first region from (2, 2)
/// where a player may found one and host `ordinal` (of two) owns.
fn embark_request_on(h: &History, s: u32, ordinal: u32, taken: &[PersonId]) -> EmbarkRequest {
    let site = h.sites().next().expect("a site");
    let owner = |rc: RegionCoords| Ownership::Static { ordinal: 0, hosts: 2 }.owner_of(rc, s);
    let region = (2..46)
        .flat_map(|z| (2..46).map(move |x| RegionCoords::new(x, z)))
        .find(|r| owner(*r) == ordinal && h.player_embark_allowed(*r).is_ok())
        .expect("room to embark on the host");
    let people = site.members.iter().copied().filter(|p| !taken.contains(p)).take(2).collect();
    EmbarkRequest { civ: site.civ, people, region }
}

/// A coordinator serves two hosts (R3): both attach — host 1's `Attach` parks nothing of
/// host 0's — and each one's player embarks into a region that host owns: the founding
/// party of each fortress gets its bodies on the host that runs it, and keeps them.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn two_hosts_attach_and_both_get_their_people() {
    install_props();
    let Some(db) = PgDb::create("twohosts") else { return };
    let map = land_map();
    let dir = scratch("twohosts");
    let cert_file = dir.join("coordinator.cert-sha256");
    let (coordinator, addr) = Coordinator::start(&db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
    let deadline = Instant::now() + WAIT;
    while coordinator.store.load_coordinator().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the bootstrap was never saved");
        std::thread::sleep(Duration::from_millis(10));
    }
    let (store0, store1) = (db.open(), db.open());
    let s = store0.super_region_regions();
    let key = |n: &str| PlayerKey::Local(game::PlayerName::parse(n).unwrap());
    let (a, _) = StorePlayers(store0.clone()).resolve(&key("a")).unwrap();
    let (b, _) = StorePlayers(store1.clone()).resolve(&key("b")).unwrap();
    let link = |ordinal: u32| {
        let cert_file = cert_file.clone();
        move |epoch| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file, ordinal, 10 + u64::from(ordinal), epoch)) }
    };
    let mut h0 = Host::start(&store0, &map, Ownership::Static { ordinal: 0, hosts: 2 }, link(0), None, 10);
    h0.until("host 0 attached", |h| h.manager.world_attached());

    // Host 0's player embarks into a region of host 0.
    let req0 = embark_request_on(&stored_history(&coordinator.store, &map), s, 0, &[]);
    let owner = |rc: RegionCoords| Ownership::Static { ordinal: 0, hosts: 2 }.owner_of(rc, s);
    let spawn0 = (2..46).map(|x| RegionCoords::new(x, 2)).find(|r| owner(*r) == 0).expect("a region of host 0");
    h0.send(ServerEvent::ClientConnected { client: A, player: a, record: Some(standing_in(spawn0)) });
    h0.send(ServerEvent::ClientPacket(ClientPacket::Embark(req0.clone()), A));
    h0.until("host 0's embark", |h| !h.embark_results(A).is_empty());
    let site0 = h0.embark_results(A)[0].expect("host 0's player embarked");
    let party0 = req0.people.clone();
    h0.until("host 0's party", |h| {
        let census = bodies(&mut h.manager);
        party0.iter().all(|p| census.get(p) == Some(&vec![req0.region]))
    });

    // Host 1 attaches: nothing of host 0's parks.
    let mut h1 = Host::start(&store1, &map, Ownership::Static { ordinal: 1, hosts: 2 }, link(1), None, 11);
    until_both(&mut h0, &mut h1, "host 1 attached", |_, h1| h1.manager.world_attached());
    let req1 = {
        let taken = req0.people.clone();
        coordinator.thread.inspect(move |c| embark_request_on(c.history(), s, 1, &taken), WAIT).expect("the coordinator answers")
    };
    let spawn1 = (2..46).map(|x| RegionCoords::new(x, 2)).find(|r| owner(*r) == 1).expect("a region of host 1");
    h1.send(ServerEvent::ClientConnected { client: A, player: b, record: Some(standing_in(spawn1)) });
    h1.send(ServerEvent::ClientPacket(ClientPacket::Embark(req1.clone()), A));
    until_both(&mut h0, &mut h1, "host 1's embark", |_, h1| !h1.embark_results(A).is_empty());
    let site1 = h1.embark_results(A)[0].expect("host 1's player embarked");
    assert_ne!(site0, site1);
    let (party0, party1) = (req0.people.clone(), req1.people.clone());
    until_both(&mut h0, &mut h1, "both parties stand in their fortresses", |h0, h1| {
        let (c0, c1) = (bodies(&mut h0.manager), bodies(&mut h1.manager));
        party0.iter().all(|p| c0.get(p) == Some(&vec![req0.region]) && !c1.contains_key(p))
            && party1.iter().all(|p| c1.get(p) == Some(&vec![req1.region]) && !c0.contains_key(p))
    });
    for _ in 0..20 {
        h0.round();
        h1.round();
    }
    let (c0, c1) = (bodies(&mut h0.manager), bodies(&mut h1.manager));
    for p in &req0.people {
        assert_eq!((c0.get(p), c1.get(p)), (Some(&vec![req0.region]), None), "{p:?} stays on host 0");
    }
    for p in &req1.people {
        assert_eq!((c0.get(p), c1.get(p)), (None, Some(&vec![req1.region])), "{p:?} stays on host 1");
    }
    let (r0, r1, all) = (req0.region, req1.region, [req0.people.clone(), req1.people.clone()]);
    let locations = coordinator
        .thread
        .inspect(move |c| all.map(|ps| ps.iter().map(|p| c.history().person(*p).map(|q| q.location)).collect::<Vec<_>>()), WAIT)
        .expect("the coordinator answers");
    assert!(locations[0].iter().all(|l| *l == Some(Location::InRegion(r0))), "{locations:?}");
    assert!(locations[1].iter().all(|l| *l == Some(Location::InRegion(r1))), "{locations:?}");
    coordinator.stop();
    drop((h0, h1));
    store0.shutdown();
    store1.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host process whose claim was taken over (ordinal 0 claimed twice) is refused by a
/// coordinator that started after both claims — it remembers no connection, the store's
/// claim epoch decides: the stale hello's `Attach` gets nothing; the current claim's is
/// echoed.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stale_epoch_host_is_refused_by_a_restarted_coordinator() {
    install_props();
    let Some(db) = PgDb::create("staleepoch") else { return };
    let map = land_map();
    let dir = scratch("staleepoch");
    let cert_file = dir.join("coordinator.cert-sha256");
    let store = db.open();
    server::world_file::bind_world_terrain(&store).expect("a stud world");
    let claim = || {
        HostClaims::claim(store.clone(), Ownership::Static { ordinal: 0, hosts: 1 }, Some(map.extent()), store.super_region_regions())
            .unwrap()
            .fence()
            .claim_epoch()
    };
    let (first, second) = (claim(), claim());
    assert!(second > first, "a second claim bumps the epoch: {first} then {second}");
    // A coordinator that ran before is gone; this one starts fresh.
    let (c1, addr) = Coordinator::start(&db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
    c1.stop();
    let (coordinator, _) = Coordinator::start(&db, &map, addr, cert_file.clone());
    let pin = svc_link::read_cert_hash("MMO_ROLES_TEST_UNSET_PIN", &cert_file).unwrap();
    let dial = |boot: u64, epoch: u64| {
        let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Host { ordinal: 0, boot, epoch });
        let (link, events) = svc_link::LinkDialer::connect::<game::CoordMsg, game::HostMsg>(addr, pin.clone(), hello);
        let deadline = Instant::now() + WAIT;
        loop {
            match events.recv_timeout(Duration::from_millis(50)) {
                Ok(svc_link::LinkEvent::Up(_)) => break,
                _ => assert!(Instant::now() < deadline, "the link never came up"),
            }
        }
        link.send(game::HostMsg::Attach { host: 0, epoch: 1, running: Vec::new(), online: Vec::new(), generation: 0, boot: 1, settled_below: 0 }).unwrap();
        (link, events)
    };
    let (stale, stale_events) = dial(1, first);
    let (live, live_events) = dial(2, second);
    let deadline = Instant::now() + WAIT;
    loop {
        match live_events.recv_timeout(Duration::from_millis(50)) {
            Ok(svc_link::LinkEvent::Msg(game::CoordMsg::Attached { epoch: 1 })) => break,
            _ => assert!(Instant::now() < deadline, "the current claim's Attach was never echoed"),
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !stale_events.try_iter().any(|e| matches!(e, svc_link::LinkEvent::Msg(_))),
        "the stale host is told nothing (no attach echo, no view)"
    );
    drop((stale, live));
    coordinator.stop();
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- coordinator restart and downtime (R2b Task 10) -------------------------------------

/// Each person's bodies on the host, by region.
fn bodies(manager: &mut WorldManager<InlineSpawner>) -> BTreeMap<PersonId, Vec<RegionCoords>> {
    let mut out: BTreeMap<PersonId, Vec<RegionCoords>> = BTreeMap::new();
    for r in manager.running_regions() {
        manager.spawner_mut().with_region(r, |region| {
            for (p, _) in region.data().people_here() {
                out.entry(p).or_default().push(r);
            }
        });
    }
    out
}

/// What must hold at every moment: at most one body per person, none for the dead or
/// the unknown.
fn never_twice(h: &History, bodies: &BTreeMap<PersonId, Vec<RegionCoords>>) -> Result<(), String> {
    for (p, at) in bodies {
        if at.len() > 1 {
            return Err(format!("{p:?} has bodies in {at:?}"));
        }
        if !h.person(*p).is_some_and(|q| q.alive()) {
            return Err(format!("a body for {p:?}, who is not alive"));
        }
    }
    Ok(())
}

/// The settled agreement: [`never_twice`]; every living person embodied in the history
/// has their one body where the history has it, everyone else none; and the fortress
/// region's population is its site's — every living member of `site` stands in `fortress`
/// (a resident left abstract in its running fortress is a person missing).
fn settled(
    h: &History,
    bodies: &BTreeMap<PersonId, Vec<RegionCoords>>,
    site: SiteId,
    fortress: RegionCoords,
) -> Result<(), String> {
    never_twice(h, bodies)?;
    for q in h.people().filter(|q| q.alive()) {
        let at = bodies.get(&q.id).map_or(&[][..], Vec::as_slice);
        match q.location {
            Location::InRegion(r) if at != [r] => {
                return Err(format!("{:?} is in {r:?} in the history, bodies in {at:?}", q.id));
            }
            Location::InRegion(_) => {}
            loc if !at.is_empty() => return Err(format!("{:?} is abstract ({loc:?}) but has a body in {at:?}", q.id)),
            _ => {}
        }
    }
    let s = h.site(site).ok_or("the fortress site is gone")?;
    let members: BTreeSet<PersonId> =
        s.members.iter().copied().filter(|p| h.person(*p).is_some_and(|q| q.alive())).collect();
    let here: BTreeSet<PersonId> =
        bodies.iter().filter(|(_, at)| at.contains(&fortress)).map(|(p, _)| *p).collect();
    if !members.is_subset(&here) {
        return Err(format!("site members {members:?} vs bodies in the fortress {here:?}"));
    }
    Ok(())
}

/// The births logged at `site`, each child once.
fn births_at(h: &History, site: SiteId) -> Result<Vec<PersonId>, String> {
    let mut seen = BTreeSet::new();
    for e in h.log() {
        if let LogKind::Born { person, site: Some(s), .. } = e.kind {
            if s == site && !seen.insert(person) {
                return Err(format!("{person:?} born twice"));
            }
        }
    }
    Ok(seen.into_iter().collect())
}

/// An embark of a young married couple (both alive, members of one site) into the first
/// region a player may found one in: the fortress can have children.
fn couple_embark_request(h: &History) -> EmbarkRequest {
    let region = (2..46)
        .flat_map(|z| (2..46).map(move |x| RegionCoords::new(x, z)))
        .find(|r| h.player_embark_allowed(*r).is_ok())
        .expect("room to embark");
    for s in h.sites() {
        for &m in &s.members {
            let q = h.person(m).unwrap();
            let Some(f) = q.spouse else { continue };
            if q.sex == Sex::Female && q.alive() && q.age_years(h.now()) <= 35 && s.members.contains(&f) {
                return EmbarkRequest { civ: s.civ, people: vec![m, f], region };
            }
        }
    }
    panic!("no young couple to embark");
}

impl Coordinator {
    /// The coordinator process crashes: nothing more is handled, saved or sent, its
    /// links drop. Its store handle is dropped, not shut down — nothing waits for it to
    /// drain: a save the runtime had already handed the store still commits whenever its
    /// thread gets to it (as one that reached the database before the crash would), before
    /// or after the restart's claim (after it, refused as fenced). A save lost with the
    /// process is the `fail_next_writes` scenario.
    fn kill(self) {
        self.thread.kill();
        drop(self.store);
    }

    /// Asserts `check` of its live history, or fails naming `when`.
    fn check(&self, when: &str, check: impl FnOnce(&History) -> Result<(), String> + Send + 'static) {
        let result = self.thread.inspect(move |c| check(c.history()), WAIT).expect("the coordinator answers");
        if let Err(e) = result {
            panic!("{when}: {e}");
        }
    }
}

impl Host {
    /// Rounds until the settled agreement holds against `coordinator` (at most [`WAIT`]),
    /// [`never_twice`] checked along the way.
    fn until_settled(&mut self, coordinator: &Coordinator, when: &str, site: SiteId, fortress: RegionCoords) {
        let deadline = Instant::now() + WAIT;
        loop {
            let census = bodies(&mut self.manager);
            let result = coordinator
                .thread
                .inspect(move |c| settled(c.history(), &census, site, fortress).map_err(|e| (e, never_twice(c.history(), &census))), WAIT)
                .expect("the coordinator answers");
            match result {
                Ok(()) => return,
                Err((_, Err(twice))) => panic!("{when}: {twice}"),
                Err((e, Ok(()))) => assert!(Instant::now() < deadline, "{when}: never settled: {e}"),
            }
            for _ in 0..5 {
                self.round();
            }
        }
    }
}

/// The scenario seeds of `coordinator_crash_mid_play_conserves_people`; one may be
/// replayed alone with `MMO_ROLES_SEED=<seed>`.
fn seeds(n: u64) -> Vec<u64> {
    match std::env::var("MMO_ROLES_SEED").ok().and_then(|s| s.parse().ok()) {
        Some(seed) => vec![seed],
        None => (0..n).collect(),
    }
}

/// The coordinator process crashes while a host runs a populated fortress — after a
/// save, and after a birth directive (seed 0: killed at once, the birth's save racing the
/// crash; seed 1: after the child's body stands; seed 2: with saves failing, so the birth
/// was never durable) — and restarts: after the host re-attaches every living person of
/// the site has exactly one body (in the running fortress) or is abstract elsewhere, no
/// dead person has a body, and no birth is applied twice.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn coordinator_crash_mid_play_conserves_people() {
    install_props();
    for seed in seeds(3) {
        eprintln!("coordinator_crash_mid_play_conserves_people: seed {seed} (replay: MMO_ROLES_SEED={seed})");
        crash_mid_play(seed);
    }
}

fn crash_mid_play(seed: u64) {
    let Some(db) = PgDb::create(&format!("crash{seed}")) else { return };
    let map = land_map();
    let dir = scratch(&format!("crash{seed}"));
    let cert_file = dir.join("coordinator.cert-sha256");
    let (c1, addr) = Coordinator::start(&db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
    let req = c1.thread.inspect(|c| couple_embark_request(c.history()), WAIT).expect("the coordinator answers");
    let host_store = db.open();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
    let (a, _) = StorePlayers(host_store.clone()).resolve(&a_key).unwrap();
    let link = |epoch| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file.clone(), 0, 1, epoch)) };
    let mut host = Host::start(&host_store, &map, Ownership::Static { ordinal: 0, hosts: 1 }, link, None, 1);
    host.until("attached", |h| h.manager.world_attached());
    host.send(ServerEvent::ClientConnected { client: A, player: a, record: None });
    host.send(ServerEvent::ClientPacket(ClientPacket::Embark(req.clone()), A));
    // The embark's reply is gated: once it is here, its save committed.
    host.until("embarked", |h| !h.embark_results(A).is_empty());
    let site = host.embark_results(A)[0].unwrap_or_else(|e| panic!("seed {seed}: embark refused: {e:?}"));
    let fortress = req.region;
    host.until_settled(&c1, &format!("seed {seed}: the founded fortress"), site, fortress);

    // A birth: the history runs on (month by month) until the couple has a child.
    if seed == 2 {
        c1.store.fail_next_writes(1_000_000);
    }
    let born = c1
        .thread
        .with_core(
            move |c| {
                let h = c.history_mut();
                for _ in 0..12 * 30 {
                    let next = CalendarTick(h.now().0 + 28 * game::history::TICKS_PER_DAY);
                    h.advance(next);
                    if !births_at(h, site).unwrap().is_empty() {
                        break;
                    }
                }
                births_at(h, site).unwrap()
            },
            WAIT,
        )
        .expect("the coordinator answers");
    assert!(!born.is_empty(), "seed {seed}: the couple had a child within 30 years");
    match seed {
        0 => {}
        1 => {
            let child = born[0];
            host.until("the child's body", |h| bodies(&mut h.manager).get(&child).is_some());
            for _ in 0..10 {
                host.round();
            }
        }
        _ => {
            for _ in 0..40 {
                host.round();
                let census = bodies(&mut host.manager);
                assert!(born.iter().all(|p| !census.contains_key(p)), "seed {seed}: a body for a birth never saved");
            }
        }
    }
    let census = bodies(&mut host.manager);
    c1.check(&format!("seed {seed}: before the crash"), move |h| never_twice(h, &census));
    c1.kill();
    host.until("the link drops", |h| !h.manager.world_attached());
    for _ in 0..20 {
        host.round();
    }

    // Restarted on the same address (the same pin): the host re-attaches.
    let (c2, _) = Coordinator::start(&db, &map, addr, cert_file);
    host.until("re-attached", |h| h.manager.world_attached());
    host.until_settled(&c2, &format!("seed {seed}: after the restart"), site, fortress);
    for _ in 0..20 {
        host.round();
    }
    host.until_settled(&c2, &format!("seed {seed}: settled after the restart"), site, fortress);
    let born_before = born.clone();
    c2.check(&format!("seed {seed}: births"), move |h| {
        let now = births_at(h, site)?;
        match seed {
            // Never durable: the restart's history has no such birth.
            2 => (now.is_empty()).then_some(()).ok_or(format!("a birth never saved came back: {now:?}")),
            // Durable (the save raced the crash) or not; never twice.
            0 => (now.is_empty() || now == born_before).then_some(()).ok_or(format!("{now:?} vs {born_before:?}")),
            _ => (now == born_before).then_some(()).ok_or(format!("{now:?} vs {born_before:?}")),
        }
    });
    let results = host.embark_results(A);
    assert_eq!(results.len(), 1, "seed {seed}: one EmbarkResult: {results:?}");
    assert_eq!(host.exit(), ServerExit::Clean);
    host_store.shutdown();
    c2.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The coordinator is down (crashed) when the client embarks — and the client connected
/// during the downtime too, so its region started unannounced: the request waits in the
/// outbox; the coordinator restarts within 10 s and exactly one site is founded, one
/// `EmbarkResult(Ok)` arrives.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn embark_during_downtime_completes_once() {
    install_props();
    let Some(db) = PgDb::create("downtime") else { return };
    let map = land_map();
    let dir = scratch("downtime");
    let cert_file = dir.join("coordinator.cert-sha256");
    let (c1, addr) = Coordinator::start(&db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
    let req = c1.thread.inspect(|c| embark_request(c.history()), WAIT).expect("the coordinator answers");
    let host_store = db.open();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
    let (a, _) = StorePlayers(host_store.clone()).resolve(&a_key).unwrap();
    let link = |epoch| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file.clone(), 0, 1, epoch)) };
    let mut host = Host::start(&host_store, &map, Ownership::Static { ordinal: 0, hosts: 1 }, link, None, 1);
    host.until("attached", |h| h.manager.world_attached());

    c1.kill();
    host.until("the link drops", |h| !h.manager.world_attached());
    let down_at = Instant::now();
    host.send(ServerEvent::ClientConnected { client: A, player: a, record: None });
    for _ in 0..10 {
        host.round();
    }
    host.send(ServerEvent::ClientPacket(ClientPacket::Embark(req.clone()), A));
    for _ in 0..20 {
        host.round();
    }
    assert!(host.embark_results(A).is_empty(), "nothing answers while the coordinator is down");
    assert_eq!(host.manager.pending_world_requests(), 1, "the embark waits");

    let (c2, _) = Coordinator::start(&db, &map, addr, cert_file);
    assert!(down_at.elapsed() < Duration::from_secs(10), "the coordinator is back within 10 s");
    host.until("re-attached", |h| h.manager.world_attached());
    host.until("the embark answered", |h| !h.embark_results(A).is_empty());
    for _ in 0..20 {
        host.round();
    }
    let results = host.embark_results(A);
    assert_eq!(results.len(), 1, "one EmbarkResult: {results:?}");
    let site = results[0].expect("embarked");
    host.until_settled(&c2, "after the embark", site, req.region);
    assert_eq!(c2.thread.inspect(|c| player_sites(c.history()), WAIT).unwrap(), vec![site], "one site founded");
    assert_eq!(host.exit(), ServerExit::Clean);
    host_store.shutdown();
    c2.stop();
    let store = db.open();
    assert_eq!(stored_player_sites(&store, &map), vec![site], "and saved once");
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

fn player_sites(h: &History) -> Vec<SiteId> {
    h.sites().filter(|s| s.player_founded).map(|s| s.id).collect()
}

/// The embark's reply was decided and its save committed, but the coordinator crashed
/// before the reply left: after the restart the host's replayed request is answered from
/// the saved memo — the site is not founded again, one `EmbarkResult(Ok)` of that site.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn embark_answered_then_crash_is_not_refounded() {
    install_props();
    let Some(db) = PgDb::create("memo") else { return };
    let map = land_map();
    let dir = scratch("memo");
    let cert_file = dir.join("coordinator.cert-sha256");
    let (c1, addr) = Coordinator::start(&db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
    let req = c1.thread.inspect(|c| embark_request(c.history()), WAIT).expect("the coordinator answers");
    let host_store = db.open();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
    let (a, _) = StorePlayers(host_store.clone()).resolve(&a_key).unwrap();
    let link = |epoch| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file.clone(), 0, 1, epoch)) };
    let mut host = Host::start(&host_store, &map, Ownership::Static { ordinal: 0, hosts: 1 }, link, None, 1);
    host.until("attached", |h| h.manager.world_attached());
    host.send(ServerEvent::ClientConnected { client: A, player: a, record: None });
    for _ in 0..10 {
        host.round();
    }

    c1.thread.crash_before_claim_reply();
    host.send(ServerEvent::ClientPacket(ClientPacket::Embark(req.clone()), A));
    host.until("the coordinator crashed", |_| c1.thread.is_finished());
    c1.kill();
    let saved = {
        let store = db.open();
        let sites = stored_player_sites(&store, &map);
        store.shutdown();
        sites
    };
    assert_eq!(saved.len(), 1, "the founding's save committed before the crash: {saved:?}");
    host.until("the link drops", |h| !h.manager.world_attached());
    assert!(host.embark_results(A).is_empty(), "the reply never left");

    let (c2, _) = Coordinator::start(&db, &map, addr, cert_file);
    host.until("re-attached", |h| h.manager.world_attached());
    host.until("the embark answered", |h| !h.embark_results(A).is_empty());
    for _ in 0..20 {
        host.round();
    }
    assert_eq!(host.embark_results(A), vec![Ok(saved[0])], "one EmbarkResult, of the saved site");
    host.until_settled(&c2, "after the replayed embark", saved[0], req.region);
    assert_eq!(c2.thread.inspect(|c| player_sites(c.history()), WAIT).unwrap(), saved, "not founded again");
    assert_eq!(host.exit(), ServerExit::Clean);
    host_store.shutdown();
    c2.stop();
    let store = db.open();
    assert_eq!(stored_player_sites(&store, &map), saved, "one site saved");
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The coordinator stays down longer than the request timeout (shortened here by the
/// manager's test dial): the embark is answered `WorldUnavailable` once and forgotten —
/// no site after the coordinator returns — and a later embark works.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn request_timeout_answers_world_unavailable() {
    /// The test's request timeout (production: `WORLD_REQUEST_TIMEOUT_SECS`).
    const TIMEOUT_MS: u64 = 500;
    install_props();
    let Some(db) = PgDb::create("timeout") else { return };
    let map = land_map();
    let dir = scratch("timeout");
    let cert_file = dir.join("coordinator.cert-sha256");
    let (c1, addr) = Coordinator::start(&db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
    let req = c1.thread.inspect(|c| embark_request(c.history()), WAIT).expect("the coordinator answers");
    let host_store = db.open();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
    let (a, _) = StorePlayers(host_store.clone()).resolve(&a_key).unwrap();
    let link = |epoch| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file.clone(), 0, 1, epoch)) };
    let mut host = Host::start(&host_store, &map, Ownership::Static { ordinal: 0, hosts: 1 }, link, None, 1);
    host.manager.set_world_request_timeout_ms(TIMEOUT_MS);
    host.until("attached", |h| h.manager.world_attached());
    host.send(ServerEvent::ClientConnected { client: A, player: a, record: None });
    for _ in 0..10 {
        host.round();
    }

    c1.kill();
    host.until("the link drops", |h| !h.manager.world_attached());
    host.send(ServerEvent::ClientPacket(ClientPacket::Embark(req.clone()), A));
    let asked = Instant::now();
    host.until("the embark times out", |h| !h.embark_results(A).is_empty());
    assert!(asked.elapsed() >= Duration::from_millis(TIMEOUT_MS), "answered no earlier than the timeout");
    let until = Instant::now() + Duration::from_millis(2 * TIMEOUT_MS);
    while Instant::now() < until {
        host.round();
    }
    assert_eq!(host.embark_results(A), vec![Err(EmbarkRefusal::WorldUnavailable)], "WorldUnavailable, once");
    assert_eq!(host.manager.pending_world_requests(), 0, "the request is forgotten");

    let (c2, _) = Coordinator::start(&db, &map, addr, cert_file);
    host.until("re-attached", |h| h.manager.world_attached());
    for _ in 0..40 {
        host.round();
    }
    assert_eq!(host.embark_results(A).len(), 1, "no late answer to the timed-out embark");
    assert!(c2.thread.inspect(|c| player_sites(c.history()), WAIT).unwrap().is_empty(), "no site founded");
    host.send(ServerEvent::ClientPacket(ClientPacket::Embark(req.clone()), A));
    host.until("the second embark answered", |h| h.embark_results(A).len() == 2);
    let site = host.embark_results(A)[1].expect("the later embark works");
    assert_eq!(c2.thread.inspect(|c| player_sites(c.history()), WAIT).unwrap(), vec![site]);
    assert_eq!(host.exit(), ServerExit::Clean);
    host_store.shutdown();
    c2.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

