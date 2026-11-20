// The shared rig of the multi-host end-to-end tests (`hosts.rs`, `takeover.rs`): a world
// and its store, a host's manager over real peer links (`Sim`, `Pair`), a real coordinator
// (`World`), and the crash helpers (kill, restart, exit). Included (`include!`) by each
// test file: one copy of the code, each test binary its own process-global state.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver};
use game::{
    HostId, RegionSpawner, ClientId, CrashPolicy, HostEndpoint, InlineSpawner, PeerHello, PeerLink, PeerMsg, PeerSession, PlayerId,
    PlayerRecord, RegionCoords, RegionOutput, ServerEvent, ServerPacket, WorldManager,
    REGION_SIZE,
};
use login_api::{HostToLogin, LoginToHost};
use server::claims::HostClaims;
use server::login_link::{LoginRelay, SpawnOrders};
use server::player_store::{PlayerKey, PlayerStore};
use server::peer_link::{PeerLinkConfig, PeerLinks, PeerListener};
use world_store::{StoreHandle, StoreUrl};

/// Wall time any wait below may take before the test fails.
const WAIT: Duration = Duration::from_secs(30);
const TOKEN: &str = "hosts-test-service-token";
/// A host id from its number.
fn h(n: u64) -> HostId {
    HostId(n)
}

const HOSTS: u32 = 2;
/// The keeper client of each host: its body keeps that host's seam region running.
const KEEPER: ClientId = 0;

// ---- world and store ------------------------------------------------------------------

struct PgDb(pg_test_support::PgTestDb);

impl PgDb {
    /// A throwaway world database whose ids are issued from 0 (`world.next_host_id` set to 0
    /// before anyone registers, as a world migrated from fixed ordinals continues): the first
    /// two hosts registered are `h(0)` and `h(1)`, as these tests' fixed layout names them; a
    /// restarted process gets the next id (2, 3, …).
    fn create(tag: &str) -> Option<PgDb> {
        let db = pg_test_support::PgTestDb::create(&format!("mmo_hosts_{tag}"));
        if db.is_none() {
            eprintln!("{} (MMO_TEST_DATABASE_URL=postgres://mmo:mmo@127.0.0.1:55432/postgres)", pg_test_support::SKIP_MSG);
        }
        let db = db.map(PgDb)?;
        let store = db.open();
        exec(&db, "UPDATE world SET next_host_id = 0");
        store.shutdown();
        Some(db)
    }

    /// A handle as one process opens it.
    fn open(&self) -> StoreHandle {
        StoreHandle::open(&StoreUrl::Postgres(self.0.url.clone()), world_store::Dials::BUILT, world_store::OpenMode::Writer)
            .unwrap()
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmo-hosts-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn install_props() {
    let registry = game::BlockRegistry::from_ron(include_str!("../../../../assets/blocks/blocks.ron")).unwrap();
    game::install_block_props(registry.props());
}

/// Blocks of ground under every column of the flat fixture: its top is this many metres
/// up (6 m).
const GROUND_BLOCKS: usize = 6;

/// The y (units) a keeper's body starts at: a little above the ground's top.
fn above_ground_y() -> f32 {
    (GROUND_BLOCKS * game::BLOCK_VOXELS) as f32 + 3.0
}

/// Flat ground under every chunk column of every region, ground without worldgen: grass
/// over dirt over stone, its top [`GROUND_BLOCKS`] up, stone below the band.
fn flat_floor(_rc: RegionCoords) -> Vec<(game::ColumnCoords, game::Column)> {
    static IDS: std::sync::OnceLock<[game::BlockId; 3]> = std::sync::OnceLock::new();
    let [grass, dirt, stone] = *IDS.get_or_init(|| {
        let registry = game::BlockRegistry::from_ron(include_str!("../../../../assets/blocks/blocks.ron")).unwrap();
        ["grass", "dirt", "stone"].map(|name| registry.id_of(name).expect("block in the manifest"))
    });
    let mut chunk = game::Chunk::default();
    for x in 0..game::CHUNK_BLOCKS {
        for z in 0..game::CHUNK_BLOCKS {
            for y in 0..GROUND_BLOCKS {
                let id = match GROUND_BLOCKS - 1 - y {
                    0 => grass,
                    1 => dirt,
                    _ => stone,
                };
                chunk.set_block(x, y, z, game::Block::new(id));
            }
        }
    }
    let (air, stone) = (game::Block::new(game::BlockId::AIR), game::Block::new(stone));
    let n = game::REGION_CHUNKS;
    (0..n)
        .flat_map(|x| (0..n).map(move |z| (x, z)))
        .map(|(x, z)| {
            let sub = game::SubChunk::Dense(Box::new(chunk.clone()));
            (game::ColumnCoords::new(x, z), game::Column::new(0, stone, air, vec![sub]))
        })
        .collect()
}

/// A host's manager on `spawner`: never-run regions start from [`flat_floor`]; far terrain
/// is empty (an inline LOD builder with no coarse data).
fn flat_manager(
    spawner: InlineSpawner,
    out: crossbeam::channel::Sender<(Option<ClientId>, ServerPacket)>,
    region_out: crossbeam::channel::Sender<(RegionCoords, RegionOutput)>,
) -> WorldManager<InlineSpawner> {
    let (lod_result_send, lod_result_recv) = unbounded();
    let lod_builder: Box<dyn game::LodBuilder> = Box::new(game::InlineLodBuilder {
        coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
        water: Box::new(game::lod::LodWater::dry),
        out: lod_result_send,
    });
    WorldManager::new(spawner, Box::new(flat_floor), out, region_out, lod_builder, lod_result_recv)
}

/// Where host `ordinal` says players reach it.
fn endpoint(ordinal: HostId) -> HostEndpoint {
    HostEndpoint {
        id: ordinal,
        quic: Some(format!("127.0.0.1:{}", 7400 + ordinal.0)),
        webtransport: Some(format!("127.0.0.1:{}", 7500 + ordinal.0)),
        cert_hash: None,
    }
}

/// Host `me` of the rig's fixed table `h(0)..h(HOSTS)` (R3's layout; several hosts under a
/// fixed table exist only in tests since Elastic 1).
fn fixed(me: HostId) -> game::Static {
    game::Static { me, layout: (0..u64::from(HOSTS)).map(HostId).collect() }
}

/// The owner of region `rc` in the rig's fixed table.
fn fixed_owner(rc: RegionCoords, s: u32) -> HostId {
    fixed(h(0)).owner_of(rc, s)
}

/// The id the store issues to the next registration (`world.next_host_id`).
fn next_id(db: &PgDb) -> HostId {
    HostId(query_i64s(db, "SELECT next_host_id FROM world")[0] as u64)
}

/// A host process's start as `server::run` makes it, up to its peer links: its peer listener
/// bound on `bind` (its pin in `dir`, named `name`), then REGISTERED — the store issues its id
/// (`want`: the id it must be) — then its links started under that id. Registered as `claim`
/// does (any number of hosts), so several rig hosts run beside each other whatever ownership
/// their managers use. Its links and registration.
fn boot_links(store: &StoreHandle, want: HostId, name: &str, boot: u64, token: &str, dir: &std::path::Path, bind: &str) -> (PeerLinks, world_store::Registered) {
    let config = PeerLinkConfig {
        bind: bind.parse().unwrap(),
        cert_hash_file: dir.join(format!("{name}-{boot}.peer.cert-sha256")),
        delay: Duration::ZERO,
        world: String::new(),
    };
    let listener = PeerListener::bind(config, token.into()).expect("the peer listener binds");
    let (peer_addr, peer_cert) = listener.row_fields();
    let addresses = endpoint(want);
    let host = world_store::NewHost {
        name: name.into(),
        boot,
        peer_addr: Some(peer_addr),
        peer_cert: Some(peer_cert),
        quic: addresses.quic,
        webtransport: addresses.webtransport,
        wt_cert_hash: None,
    };
    let registered = store.register_host(host, CrashPolicy::Succession).expect("registered");
    assert_eq!(registered.id, want, "ids are issued in order");
    let links = PeerLinks::start(listener, registered.id, boot, store.clone()).expect("the peer links start");
    (links, registered)
}

// ---- a host's manager -------------------------------------------------------------------

/// A host's `WorldManager` (inline regions) with its peer sessions over real links, and
/// (world cut tests) its cut writer.
struct Sim {
    ordinal: HostId,
    boot: u64,
    manager: WorldManager<InlineSpawner>,
    /// The links of newer hosts that dial in (`PeerLinks::take_arrived`), set as peers when
    /// they come.
    arrivals: Receiver<(HostId, Box<dyn PeerLink + Send>)>,
    /// Its world generation (the hello of a peer arriving later).
    generation: u64,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    cuts: Option<server::cuts::CutWriter>,
    /// The login relay's server half (login tests: [`login_hosts`]).
    login: Option<SimLogin>,
    /// Its peer links' gate (a test takes them down and up).
    gate: PeerGate,
    /// Its listener for other hosts' saves (Task 19 tests).
    terrain: Option<server::cuts::TerrainWatch>,
    /// While set, every event its regions processed, in order (a replay's input).
    tap: Option<Vec<(RegionCoords, game::GameEvent)>>,
    /// Its claim loop under `claim` ownership (R4 Task 5 tests).
    keeper: Option<server::claims::DirectoryKeeper>,
    /// Its lease beat, pumped every round as `server::run`'s main loop does (R4 takeover
    /// tests), and the process's fenced flag it sets.
    heartbeat: Option<server::lease::Heartbeat>,
    fenced: server::claims::Fenced,
    /// Its adoption reads, as `server::run`'s main loop pumps them (R4 takeover tests).
    adopter: Option<server::claims::AdoptionReader>,
    /// A hung main loop (R4 takeover tests): until then the host sits rounds out entirely —
    /// no tick, no output handled, no beat, no peer pumped.
    frozen_until: Option<Instant>,
}

/// A switch over every peer link of a host: down, its links say so, send nothing and drop
/// what arrives (a dropped connection loses what was on its way); up again, each is a new
/// connection.
#[derive(Clone, Default)]
struct PeerGate(Arc<(std::sync::atomic::AtomicBool, std::sync::atomic::AtomicU64)>);

impl PeerGate {

    fn is_down(&self) -> bool {
        self.0 .0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A peer link behind a [`PeerGate`].
struct GatedLink {
    inner: Box<dyn PeerLink + Send>,
    gate: PeerGate,
}

impl PeerLink for GatedLink {
    fn send(&mut self, msg: PeerMsg) {
        if !self.gate.is_down() {
            self.inner.send(msg);
        }
    }
    fn recv(&mut self) -> Option<PeerMsg> {
        if self.gate.is_down() {
            while self.inner.recv().is_some() {}
            return None;
        }
        self.inner.recv()
    }
    fn is_up(&self) -> bool {
        !self.gate.is_down() && self.inner.is_up()
    }
    fn connection(&self) -> u64 {
        self.inner.connection() + 1_000_000 * self.gate.0 .1.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn pump(&mut self, now_ms: u64) {
        self.inner.pump(now_ms);
    }
    fn close(&mut self) {
        self.inner.close();
    }
}

/// A host's admission state and login relay, as `server::run` makes them for several hosts,
/// with the manager's event channel they write to.
struct SimLogin {
    relay: LoginRelay,
    events: Receiver<ServerEvent>,
}

impl Sim {
    /// A host's manager on `spawner` (a restarted host's: its regions from the store), its
    /// peer sessions on `links`.
    fn with_spawner(ordinal: HostId, s: u32, boot: u64, generation: u64, links: &mut PeerLinks, spawner: InlineSpawner) -> Sim {
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let mut manager = flat_manager(spawner, out_send, region_out_send);
        manager.set_ownership(Box::new(fixed(ordinal)), s);
        manager.set_endpoints(endpoint(ordinal), game::seeded_nonces(0x5EED + ordinal.0));
        let gate = PeerGate::default();
        for (other, link) in links.take() {
            let link: Box<dyn PeerLink + Send> = Box::new(GatedLink { inner: link, gate: gate.clone() });
            let hello = PeerHello {
                id: ordinal,
                boot,
                generation,
                endpoint: endpoint(ordinal),
                received_through: 0,
                received_from_boot: None,
            };
            manager.set_peer(other, PeerSession::new(hello, other), link);
        }
        manager.set_host_generation(generation);
        let arrivals = links.arrivals();
        Sim { ordinal, boot, manager, arrivals, generation, region_out, packets, cuts: None, login: None, gate, terrain: None, tap: None, keeper: None, heartbeat: None, fenced: Default::default(), adopter: None, frozen_until: None }
    }

    /// A keeper client whose body stands in the middle of `rc` (this host's): `rc` runs.
    fn keep(&mut self, player: PlayerId, rc: RegionCoords, now: u64) {
        self.keep_as(KEEPER, player, rc, now);
    }

    /// [`Sim::keep`] as session `client` (a host keeping several regions).
    fn keep_as(&mut self, client: ClientId, player: PlayerId, rc: RegionCoords, now: u64) {
        let y = above_ground_y();
        let record = PlayerRecord {
            client: game::Client::default(),
            pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, y, REGION_SIZE / 2.0)),
            region: rc,
            adventure: None,
        };
        let ev = ServerEvent::ClientConnected {
            client,
            player,
            record: Some(record),
            editor_allowed: false,
        };
        assert!(self.manager.handle_server_event(ev, now));
    }

    /// A newer host dialed in: its session, over its link behind this host's gate.
    fn take_arrivals(&mut self) {
        for (other, link) in self.arrivals.try_iter().collect::<Vec<_>>() {
            let link: Box<dyn PeerLink + Send> = Box::new(GatedLink { inner: link, gate: self.gate.clone() });
            let hello = PeerHello {
                id: self.ordinal,
                boot: self.boot,
                generation: self.generation,
                endpoint: endpoint(self.ordinal),
                received_through: 0,
                received_from_boot: None,
            };
            self.manager.set_peer(other, PeerSession::new(hello, other), link);
        }
    }

    fn settle(&mut self, now: u64) {
        self.take_arrivals();
        if let Some(login) = &self.login {
            for ev in login.events.try_iter() {
                assert!(self.manager.handle_server_event(ev, now));
            }
        }
        for _ in 0..3 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                if let (Some(tap), RegionOutput::EventProcessed(ev) | RegionOutput::EventProcessedHashed(ev, ..)) =
                    (self.tap.as_mut(), &out)
                {
                    tap.push((rc, ev.clone()));
                }
                self.manager.handle_region_output(rc, out, now);
            }
        }
        for _ in self.packets.try_iter() {}
    }

    /// Its main loop is hung at `at` ([`Sim::frozen_until`]).
    fn frozen(&self, at: Instant) -> bool {
        self.frozen_until.is_some_and(|until| at < until)
    }
}

/// Two hosts of one world: both tick every round (their world ticks stay equal), then
/// handle their regions' outputs and pump their peers (`maintain`) and their cut writers;
/// `tick` is what a test coordinator reads as the world tick.
struct Pair {
    sims: Vec<Sim>,
    started: Instant,
    tick: Arc<std::sync::atomic::AtomicU64>,
}

impl Pair {
    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn round(&mut self) {
        let now = self.now();
        let at = Instant::now();
        for sim in self.sims.iter_mut().filter(|s| !s.frozen(at)) {
            sim.manager.spawner_mut().tick_all();
        }
        if let Some(sim) = self.sims.iter_mut().find(|s| !s.frozen(at)) {
            let tick = sim.manager.spawner_mut().world_tick();
            self.tick.store(tick, std::sync::atomic::Ordering::SeqCst);
        }
        for sim in self.sims.iter_mut().filter(|s| !s.frozen(at)) {
            sim.settle(now);
            // The lease step of `server::run`'s loop (no heartbeat: nothing).
            let _ = server::lease::pump_or_fence(&mut sim.heartbeat, &sim.fenced, at, &mut game::HostLoad::default);
            // Its drain step (Elastic 1 Task 8): the beat's mark becomes the manager's deadline.
            let drain_in_ms = sim.heartbeat.as_ref().and_then(server::lease::Heartbeat::drain_in_ms);
            let tick = sim.manager.spawner_mut().world_tick();
            server::drain::note_drain(&mut sim.manager, drain_in_ms, tick);
            sim.manager.maintain(now);
            if let Some(cuts) = sim.cuts.as_mut() {
                cuts.pump(&mut sim.manager);
            }
            if let Some(watch) = sim.terrain.as_mut() {
                watch.pump(&mut sim.manager);
            }
            if let Some(keeper) = sim.keeper.as_mut() {
                keeper.pump(&mut sim.manager);
            }
            if let Some(adopter) = sim.adopter.as_mut() {
                adopter.pump(&mut sim.manager);
            }
            if let Some(login) = sim.login.as_mut() {
                login.relay.pump(&mut sim.manager);
            }
            sim.settle(now);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The adjacent pair of claimed regions nearest the origin whose owners differ: `(a, b)`,
/// `b` one step +x of `a`.
fn host_seam(s: u32) -> (RegionCoords, RegionCoords) {
    let owner = |rc: RegionCoords| fixed_owner(rc, s);
    for z in 1..47 {
        for x in 1..46 {
            let (a, b) = (RegionCoords::new(x, z), RegionCoords::new(x + 1, z));
            if owner(a) != owner(b) {
                return (a, b);
            }
        }
    }
    panic!("no host seam in the map (s = {s})");
}

/// The world cut period of the test coordinator.
const CUT_EVERY: Duration = Duration::from_millis(500);

/// The coordinator's map: a 48 × 48 grassland with a sea border (the claimed extent).
fn land_map() -> game::WorldMap {
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
    game::WorldMap { origin: RegionCoords::new(0, 0), width: 48, height: 48, tiles, rock_names: vec![], region_types: vec![], wrap_x: false }
}

/// Runs `sql` on its own connection: the first column of every row.
fn query_i64s(db: &PgDb, sql: &str) -> Vec<i64> {
    let (url, sql) = (db.0.url.clone(), sql.to_string());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use sqlx::Connection;
            let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
            let v: Vec<i64> = sqlx::query_scalar(&sql).fetch_all(&mut c).await.unwrap();
            c.close().await.unwrap();
            v
        })
    })
    .join()
    .unwrap()
}

/// The prepared transactions of the test's database.
fn prepared(db: &PgDb) -> i64 {
    query_i64s(db, "SELECT COUNT(*) FROM pg_prepared_xacts WHERE database = current_database()")[0]
}

/// The world id of the rig's links (`VOXEL_WORLD_ID`): the coordinator's API and login
/// listener, the hosts' batches and the login service's hello (its registry row's `world_id`) all
/// name it, so a real login service's hello is admitted.
const RIG_WORLD: &str = "hosts";

/// Host `ordinal`'s link to the coordinator's HTTP API at `urls` (one per replica; its pin in
/// `cert_file`).
fn http_world_link(urls: Vec<String>, cert_file: PathBuf, ordinal: HostId) -> server::world_http::HttpWorldLink {
    server::world_http::HttpWorldLink::connect(
        urls,
        move || svc_link::read_cert_hash("MMO_HOSTS_TEST_UNSET_PIN", &cert_file),
        ordinal,
        TOKEN.into(),
        RIG_WORLD.into(),
    )
}

/// A host's coordinator link a test can drop and make again (a manager installs its link
/// once): `None` = down, its connection closed; each new link is a new generation.
#[derive(Clone, Default)]
struct SwitchLink(Arc<std::sync::Mutex<(Option<server::world_http::HttpWorldLink>, u64)>>);

impl SwitchLink {
    fn lock(&self) -> std::sync::MutexGuard<'_, (Option<server::world_http::HttpWorldLink>, u64)> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The connection drops (the link is closed).
    fn drop_link(&self) {
        self.lock().0 = None;
    }

    /// A new connection.
    fn replace(&self, link: server::world_http::HttpWorldLink) {
        let mut g = self.lock();
        g.0 = Some(link);
        g.1 += 1;
    }
}

impl game::WorldLink for SwitchLink {
    fn send(&mut self, msg: game::HostMsg) {
        if let Some(l) = self.lock().0.as_mut() {
            l.send(msg);
        }
    }
    fn recv(&mut self) -> Option<game::CoordMsg> {
        self.lock().0.as_mut().and_then(|l| l.recv())
    }
    fn is_up(&self) -> bool {
        self.lock().0.as_ref().is_some_and(|l| l.is_up())
    }
    fn generation(&self) -> u64 {
        let g = self.lock();
        g.1 * 1_000_000 + g.0.as_ref().map_or(0, |l| l.generation())
    }
    fn pump(&mut self, now_ms: u64) {
        if let Some(l) = self.lock().0.as_mut() {
            l.pump(now_ms);
        }
    }
}

/// Two real hosts with a real coordinator ([`world`]).
struct World {
    pair: Pair,
    seam: (RegionCoords, RegionCoords),
    /// `None` while it is down (killed, not restarted yet).
    coordinator: Option<server::coordinator_rt::ListeningCoordinator>,
    cert: PathBuf,
    /// Each host's peer links (`None`: the host is down).
    links: Vec<Option<PeerLinks>>,
    claims: Vec<HostClaims>,
    /// The coordinator's own store handle.
    store: StoreHandle,
    switches: Vec<SwitchLink>,
    /// Each host's world generation (its rollback flag).
    generations: Vec<server::claims::WorldGeneration>,
}

impl World {
    fn stop(self, dir: &std::path::Path) {
        if let Some(c) = self.coordinator {
            assert_eq!(c.thread.shutdown(Duration::from_secs(10)), server::claims::ServerExit::Clean);
        }
        drop(self.pair);
        drop(self.links);
        self.store.shutdown();
        for c in self.claims {
            c.store().clone().shutdown();
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A coordinator process as `run_coordinator` starts it: serving its HTTP API to hosts and the
/// login service (its pin in `cert`; `ListeningCoordinator::url` names where to call), world
/// cuts `cuts`, crash policy `policy`.
fn listening_coordinator(
    store: StoreHandle,
    start: server::world_file::CoordinatorStart,
    cert: &std::path::Path,
    cuts: server::coordinator_rt::WorldCuts,
    fenced: server::claims::Fenced,
    policy: game::CrashPolicy,
) -> server::coordinator_rt::ListeningCoordinator {
    server::coordinator_rt::start_listening_as(
        store,
        start,
        "127.0.0.1:0".parse().unwrap(),
        TOKEN.into(),
        cert.to_path_buf(),
        fenced,
        Some(cuts),
        policy,
        RIG_WORLD,
    )
    .expect("the coordinator listens")
}

/// Slot `i` of a per-host table (`World`'s, by host id): grown with `fill` up to it. Ids are
/// issued in order, so a new host's slot is the next one.
fn slot<T: Clone>(v: &mut Vec<T>, id: HostId, fill: T) -> &mut T {
    let i = id.0 as usize;
    while v.len() <= i {
        v.push(fill.clone());
    }
    &mut v[i]
}

/// Runs `sql` on its own connection.
fn exec(db: &PgDb, sql: &str) {
    let (url, sql) = (db.0.url.clone(), sql.to_string());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use sqlx::Connection;
            let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
            sqlx::query(&sql).execute(&mut c).await.unwrap();
            c.close().await.unwrap();
        })
    })
    .join()
    .unwrap()
}

/// Host `h` crashes: its manager and cut writer go with nothing saved (what it staged
/// stays staged), its peer listener and its link to the coordinator drop. Its store
/// handle is shut down (a dead process's connections close).
fn kill_host(w: &mut World, h: HostId) {
    let i = w.pair.sims.iter().position(|s| s.ordinal == h).expect("the host is up");
    drop(w.pair.sims.remove(i));
    w.switches[h.0 as usize].drop_link();
    w.links[h.0 as usize] = None;
}

