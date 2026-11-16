// The shared rig of the multi-host end-to-end tests (`hosts.rs`, `takeover.rs`): a world
// and its store, a host's manager over real peer links (`Sim`, `Pair`), a real coordinator
// (`World`), and the crash helpers (kill, restart, exit). Included (`include!`) by each
// test file: one copy of the code, each test binary its own process-global state.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver};
use game::{
    HostId, RegionSpawner, ClientId, CrashPolicy, EntityId, HostEndpoint, InlineSpawner, PeerHello, PeerLink, PeerMsg, PeerSession, PlayerId,
    PlayerRecord, RegionCoords, RegionOutput, ServerEvent, ServerPacket, Volatile, WorldManager,
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

/// The claimed map: 48 × 48 regions from the origin.
fn extent() -> game::MapExtent {
    game::MapExtent { origin: RegionCoords::new(0, 0), width: 48, height: 48 }
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

/// One host process's store handle and claims of its share of the fixed table, claimed as
/// `ordinal` without a registration (a part staged and orphaned by a crash, no peers).
fn claim(db: &PgDb, ordinal: HostId) -> (StoreHandle, HostClaims) {
    let store = db.open();
    let s = store.super_region_regions();
    let claims = HostClaims::claim(store.clone(), &fixed(ordinal), Some(extent()), s).unwrap();
    (store, claims)
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

/// One host process of the fixed table: its store handle, links and claims, in `server::run`'s
/// order — registered as the next id, then its share of the table claimed under that id.
fn fixed_host(db: &PgDb, dir: &std::path::Path, boot: u64, token: &str, bind: &str) -> (StoreHandle, PeerLinks, HostClaims) {
    let store = db.open();
    let want = next_id(db);
    let (links, _) = boot_links(&store, want, &format!("host-{}", want.0), boot, token, dir, bind);
    let s = store.super_region_regions();
    let claims = HostClaims::claim(store.clone(), &fixed(want), Some(extent()), s).unwrap();
    (store, links, claims)
}

/// The one link `links` hands out (two hosts: the other one) — made at its start (a lower
/// host it dials) or when the other host dialed in (a higher one), waited for.
fn only_link(links: &mut PeerLinks) -> (HostId, Box<dyn PeerLink + Send>) {
    let deadline = Instant::now() + WAIT;
    loop {
        let mut all = links.take();
        all.extend(links.take_arrived());
        if let Some(link) = all.pop() {
            assert!(all.is_empty(), "one link per other host");
            return link;
        }
        assert!(Instant::now() < deadline, "no link to the other host");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Pump `links` until `done` holds of them, or fail after `within`.
fn pump_until(
    what: &str,
    within: Duration,
    links: &mut [&mut Box<dyn PeerLink + Send>],
    mut done: impl FnMut(&mut [&mut Box<dyn PeerLink + Send>]) -> bool,
) {
    let deadline = Instant::now() + within;
    loop {
        for l in links.iter_mut() {
            l.pump(0);
        }
        if done(links) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---- a host's manager -------------------------------------------------------------------

/// A host's `WorldManager` (inline regions) with its peer sessions over real links, and
/// (world cut tests) its cut writer.
struct Sim {
    ordinal: HostId,
    boot: u64,
    /// Its registration number (`hosts.reg`): what its coordinator hello carries.
    reg: u64,
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
    fn set_down(&self, down: bool) {
        use std::sync::atomic::Ordering::SeqCst;
        if self.0 .0.swap(down, SeqCst) && !down {
            self.0 .1.fetch_add(1, SeqCst);
        }
    }

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
    orders: Arc<SpawnOrders>,
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
        let reg = ordinal.0;
        let arrivals = links.arrivals();
        Sim { ordinal, boot, reg, manager, arrivals, generation, region_out, packets, cuts: None, login: None, gate, terrain: None, tap: None, keeper: None, heartbeat: None, fenced: Default::default(), adopter: None, frozen_until: None }
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
            fortress: None,
            adventure: None,
        };
        let ev = ServerEvent::ClientConnected { client, player, record: Some(record) };
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

    /// The running regions holding `id`.
    fn holders(&mut self, id: EntityId) -> Vec<RegionCoords> {
        let mut out = Vec::new();
        for rc in self.manager.running_regions() {
            self.manager.spawner_mut().with_region(rc, |region| {
                if region.data().id_index.contains_key(&id) {
                    out.push(rc);
                }
            });
        }
        out
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
            let _ = server::lease::pump_or_fence(&mut sim.heartbeat, &sim.fenced, at);
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
            if let Some(login) = &sim.login {
                login.relay.pump(&mut sim.manager);
            }
            sim.settle(now);
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    fn until(&mut self, what: &str, mut done: impl FnMut(&mut Pair) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            self.round();
        }
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

/// Two hosts started like `server::run` starts them, each with a keeper in its side of
/// the seam `(a, b)`, every peer session up.
fn two_hosts(db: &PgDb, dir: &std::path::Path) -> (Pair, (RegionCoords, RegionCoords), Vec<PeerLinks>, Vec<HostClaims>) {
    let mut all_links = Vec::new();
    let mut all_claims = Vec::new();
    let mut sims = Vec::new();
    let mut s = 0;
    for ordinal in (0..u64::from(HOSTS)).map(HostId) {
        let boot = 1000 + ordinal.0;
        let (store, mut links, claims) = fixed_host(db, dir, boot, TOKEN, "127.0.0.1:0");
        s = store.super_region_regions();
        let generation = store.world_generation().unwrap();
        // Its regions' halo re-seeds read the store (Task 19), as a region thread's do.
        let spawner = InlineSpawner::default().with_stored_terrain(Arc::new(world_store::TerrainFromStore(store.clone())));
        sims.push(Sim::with_spawner(ordinal, s, boot, generation, &mut links, spawner));
        all_links.push(links);
        all_claims.push(claims);
    }
    let seam = host_seam(s);
    let mut pair = Pair { sims, started: Instant::now(), tick: Arc::new(std::sync::atomic::AtomicU64::new(0)) };
    let now = pair.now();
    for rc in [seam.0, seam.1] {
        let owner = pair.sims[0].manager.owner_host(rc).expect("an owned region");
        // A player id the store gave out (a cut writes only records of those).
        let (player, _) = all_claims[owner.0 as usize].store().resolve_player(&format!("hosts-test-keeper-{}", owner.0)).unwrap();
        pair.sims[owner.0 as usize].keep(player, rc, now);
    }
    pair.until("every peer session up", |p| p.sims.iter().all(|sim| sim.manager.reaches(seam.0) && sim.manager.reaches(seam.1)));
    (pair, seam, all_links, all_claims)
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
    game::WorldMap { origin: RegionCoords::new(0, 0), width: 48, height: 48, tiles, rock_names: vec![], region_types: vec![] }
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

/// Entity ids stored in more than one region. `entities` is keyed per region
/// (world-store migration 0009: two hosts' cut parts never touch one row), so the
/// database no longer refuses one entity in two regions; the cut-ownership protocol
/// keeps it once, and this pins it over the committed rows.
fn entities_stored_twice(db: &PgDb) -> i64 {
    query_i64s(
        db,
        "SELECT COUNT(*) FROM (SELECT birth_rx, birth_rz, serial FROM entities \
         GROUP BY birth_rx, birth_rz, serial HAVING COUNT(*) > 1) twice",
    )[0]
}

/// The rows of every entity stored in more than one region:
/// `(birth_rx, birth_rz, serial, rx, rz, saved_tick)`, ordered by entity then region.
fn entity_rows_stored_twice(db: &PgDb) -> Vec<[i64; 6]> {
    let url = db.0.url.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use sqlx::Connection;
            let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
            let v: Vec<(i64, i64, i64, i64, i64, i64)> = sqlx::query_as(
                "SELECT e.birth_rx, e.birth_rz, e.serial, e.rx, e.rz, e.saved_tick FROM entities e \
                 JOIN (SELECT birth_rx, birth_rz, serial FROM entities GROUP BY birth_rx, birth_rz, serial \
                       HAVING COUNT(*) > 1) twice USING (birth_rx, birth_rz, serial) \
                 ORDER BY e.birth_rx, e.birth_rz, e.serial, e.rx, e.rz",
            )
            .fetch_all(&mut c)
            .await
            .unwrap();
            c.close().await.unwrap();
            v.into_iter().map(|(a, b, c, d, e, f)| [a, b, c, d, e, f]).collect()
        })
    })
    .join()
    .unwrap()
}

/// The decided cuts: `(cut, tick, committed)`.
fn decisions(db: &PgDb) -> Vec<(i64, i64, bool)> {
    let ids = query_i64s(db, "SELECT cut_id FROM cut_decisions ORDER BY cut_id");
    let ticks = query_i64s(db, "SELECT tick FROM cut_decisions ORDER BY cut_id");
    let committed = query_i64s(db, "SELECT committed FROM cut_decisions ORDER BY cut_id");
    ids.into_iter().zip(ticks).zip(committed).map(|((c, t), k)| (c, t, k != 0)).collect()
}

/// The world id of the rig's service links (`VOXEL_WORLD_ID`): the coordinator's listeners,
/// the hosts' coordinator hellos and the login service's (its registry row's `world_id`) all
/// name it, so a real login service's hello is admitted.
const RIG_WORLD: &str = "hosts";

/// Host `ordinal`'s link to the coordinator at `addr` (its pin in `cert_file`), its hello
/// carrying the host's registration number `reg`.
fn quic_world_link(addr: std::net::SocketAddr, cert_file: PathBuf, ordinal: HostId, boot: u64, reg: u64) -> server::roles::QuicWorldLink {
    let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Host { id: ordinal, boot, reg }, RIG_WORLD);
    server::roles::QuicWorldLink::connect(addr, move || svc_link::read_cert_hash("MMO_HOSTS_TEST_UNSET_PIN", &cert_file), hello)
}

/// A host's coordinator link a test can drop and make again (a manager installs its link
/// once): `None` = down, its connection closed; each new link is a new generation.
#[derive(Clone, Default)]
struct SwitchLink(Arc<std::sync::Mutex<(Option<server::roles::QuicWorldLink>, u64)>>);

impl SwitchLink {
    fn lock(&self) -> std::sync::MutexGuard<'_, (Option<server::roles::QuicWorldLink>, u64)> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The connection drops (the link is closed).
    fn drop_link(&self) {
        self.lock().0 = None;
    }

    /// A new connection.
    fn replace(&self, link: server::roles::QuicWorldLink) {
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

    /// Rounds until a world cut decided after `after` (cut id) is committed and both hosts
    /// settled it.
    fn until_committed_after(&mut self, db: &PgDb, after: i64) {
        self.pair.until("a later world cut committed", |_| decisions(db).iter().any(|(c, _, k)| *k && *c > after));
        self.pair.until("both hosts settled it", |p| p.sims.iter().all(|s| s.cuts.as_ref().unwrap().world_part().is_none()));
        assert_eq!(entities_stored_twice(db), 0, "an entity is stored in two regions after the committed cuts");
    }
}

/// Two real hosts ([`two_hosts`]) and a real coordinator runtime over real links, world
/// cuts every [`CUT_EVERY`] at the hosts' world tick, each host with its cut writer
/// (staging world parts).
fn world(db: &PgDb, dir: &std::path::Path) -> World {
    let (mut pair, seam, links, claims) = two_hosts(db, dir);
    let store = db.open();
    let start = server::world_file::ensure_coordinator_state(&store, &land_map(), None).unwrap().expect("a history");
    let tick = pair.tick.clone();
    let cuts = server::coordinator_rt::WorldCuts {
        every: CUT_EVERY,
        tick: Box::new(move || tick.load(std::sync::atomic::Ordering::SeqCst)),
    };
    let cert = dir.join("coordinator.cert-sha256");
    let coordinator = listening_coordinator(store.clone(), start, &cert, cuts, server::claims::Fenced::default(), game::CrashPolicy::Rollback);
    let mut switches = Vec::new();
    let mut generations = Vec::new();
    for (ordinal, sim) in pair.sims.iter_mut().enumerate() {
        let c = &claims[ordinal];
        let link = quic_world_link(coordinator.addr, cert.clone(), HostId(ordinal as u64), 1000 + ordinal as u64, sim.reg);
        let switch = SwitchLink(Arc::new(std::sync::Mutex::new((Some(link), 0))));
        switches.push(switch.clone());
        sim.manager.set_world_link(Box::new(switch), 1000 + ordinal as u64, 0);
        sim.manager.set_world_map(land_map());
        let generation = server::claims::WorldGeneration::new(c.store().world_generation().unwrap());
        generations.push(generation.clone());
        sim.cuts = Some(server::cuts::CutWriter::with_world_cuts(c.clone(), generation));
    }
    pair.until("both hosts attached", |p| p.sims.iter().all(|s| s.manager.world_attached()));
    let links = links.into_iter().map(Some).collect();
    World { pair, seam, coordinator: Some(coordinator), cert, links, claims, store, switches, generations }
}

/// A coordinator process as `run_coordinator` starts it: listening for hosts (its pin in
/// `cert`) and for the login service (Elastic 1 Task 6: its pin beside, `login-link.cert-sha256`;
/// `ListeningCoordinator::login` names where to dial), world cuts `cuts`, crash policy `policy`.
fn listening_coordinator(
    store: StoreHandle,
    start: server::world_file::CoordinatorStart,
    cert: &std::path::Path,
    cuts: server::coordinator_rt::WorldCuts,
    fenced: server::claims::Fenced,
    policy: game::CrashPolicy,
) -> server::coordinator_rt::ListeningCoordinator {
    let login = server::coordinator_rt::LoginListen {
        bind: "127.0.0.1:0".parse().unwrap(),
        token: TOKEN.into(),
        cert_hash_file: Some(cert.with_file_name("login-link.cert-sha256")),
    };
    server::coordinator_rt::start_listening_as(
        store,
        start,
        "127.0.0.1:0".parse().unwrap(),
        TOKEN.into(),
        Some(cert.to_path_buf()),
        fenced,
        Some(cuts),
        policy,
        RIG_WORLD,
        Some(login),
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

/// The cut a host takes part in, once its part is in `state` (`world_cut_state`).
fn until_part(w: &mut World, h: usize, states: &[&str]) -> game::CutId {
    let mut cut = None;
    w.pair.until("the host's part", |p| {
        cut = p.sims[h].manager.world_cut_state().filter(|(_, s)| states.contains(s)).map(|(c, _)| c);
        cut.is_some()
    });
    cut.unwrap()
}

/// The decision row of `cut`, if any.
fn decision_of(db: &PgDb, cut: game::CutId) -> Option<bool> {
    decisions(db).into_iter().find(|(c, _, _)| *c as u64 == cut.0).map(|(_, _, k)| k)
}

/// The world tick of the first host still up (what a restarted host's clock joins).
fn live_tick(w: &mut World) -> u64 {
    w.pair.sims.first_mut().map_or(0, |s| s.manager.spawner_mut().world_tick())
}

/// The seam region host `h` owns.
fn seam_of(w: &World, h: HostId) -> RegionCoords {
    let (a, b) = w.seam;
    if fixed(h).owns(a, w.claims[0].store().super_region_regions()) {
        a
    } else {
        b
    }
}

/// Host `h`'s simulation, if it is up.
fn sim(w: &mut World, h: HostId) -> &mut Sim {
    w.pair.sims.iter_mut().find(|s| s.ordinal == h).expect("the host is up")
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

/// The coordinator process crashes (nothing more handled, saved or sent; its hosts'
/// links drop).
fn kill_coordinator(w: &mut World) {
    w.coordinator.take().expect("the coordinator is up").thread.kill();
    for switch in &w.switches {
        switch.drop_link();
    }
}

/// A coordinator process starts again on `store`, once that store's staged parts were
/// resolved (as `run_coordinator` does first, [`server::roles::recover_staged_cuts`]): its
/// row claimed, it listens; every host up gets a link to it.
fn restart_coordinator_listening(w: &mut World, store: StoreHandle) {
    let start = server::world_file::ensure_coordinator_state(&store, &land_map(), None).unwrap().expect("a history");
    let tick = w.pair.tick.clone();
    let cuts = server::coordinator_rt::WorldCuts {
        every: CUT_EVERY,
        tick: Box::new(move || tick.load(std::sync::atomic::Ordering::SeqCst)),
    };
    let coordinator = listening_coordinator(store.clone(), start, &w.cert, cuts, server::claims::Fenced::default(), game::CrashPolicy::Rollback);
    let old = std::mem::replace(&mut w.store, store);
    old.shutdown();
    for sim in &w.pair.sims {
        w.switches[sim.ordinal.0 as usize].replace(quic_world_link(coordinator.addr, w.cert.clone(), sim.ordinal, sim.boot, sim.reg));
    }
    w.coordinator = Some(coordinator);
}

/// A host process named after `as_slot` starts again as boot `boot`, in `server::run`'s order:
/// the parts a crash left staged resolved, REGISTERED — a new id, the next one, never the id of
/// a host before — owning nothing: it is in no fixed table (the table of `h(0)..h(HOSTS)` names
/// every owner), so it claims nothing and runs no region of its own; a link to every live host
/// and to the coordinator (if one is up), its cut writer. Its registration.
fn restart_host(w: &mut World, db: &PgDb, dir: &std::path::Path, as_slot: HostId, boot: u64) -> world_store::Registered {
    let first = db.open();
    server::roles::resolve_before_claim(true, &first, &format!("host-{}", as_slot.0)).expect("the staged parts are resolved");
    first.shutdown();
    let store = db.open();
    let want = next_id(db);
    let (mut links, registered) = boot_links(&store, want, &format!("host-{}", as_slot.0), boot, TOKEN, dir, "127.0.0.1:0");
    let h = registered.id;
    let s = store.super_region_regions();
    let claims = HostClaims::none(store.clone(), h);
    let mut spawner = InlineSpawner::default();
    let target = live_tick(w).max(w.pair.tick.load(std::sync::atomic::Ordering::SeqCst));
    while spawner.world_tick() < target {
        spawner.tick_all();
    }
    let mut sim = Sim::with_spawner(h, s, boot, registered.generation, &mut links, spawner);
    sim.manager.set_ownership(Box::new(game::Static { me: h, layout: (0..u64::from(HOSTS)).map(HostId).collect() }), s);
    sim.manager.set_stored_regions(store.stored_regions().unwrap());
    let switch = SwitchLink(Arc::new(std::sync::Mutex::new((None, 0))));
    if let Some(c) = &w.coordinator {
        switch.replace(quic_world_link(c.addr, w.cert.clone(), h, boot, registered.id.0));
    }
    *slot(&mut w.switches, h, SwitchLink::default()) = switch.clone();
    sim.manager.set_world_link(Box::new(switch), boot, 0);
    sim.manager.set_world_map(land_map());
    let generation = server::claims::WorldGeneration::new(registered.generation);
    *slot(&mut w.generations, h, Default::default()) = generation.clone();
    sim.cuts = Some(server::cuts::CutWriter::with_world_cuts(claims.clone(), generation));
    let i = w.pair.sims.iter().position(|s| s.ordinal > h).unwrap_or(w.pair.sims.len());
    w.pair.sims.insert(i, sim);
    while w.links.len() <= h.0 as usize {
        w.links.push(None);
    }
    w.links[h.0 as usize] = Some(links);
    let fill = claims.clone();
    *slot(&mut w.claims, h, fill) = claims;
    registered
}

/// A fixed-table world after a host crash, Elastic 1's way: a `static` world is ONE host,
/// so it recovers as a total crash. Every host still up dies too (the crash of a static world
/// is the whole world's), then ONE process starts as boot `boot`: the parts left staged
/// resolved, registered with nobody else live (its handle's lease 0: every other row is
/// lapsed — they are dead) — the total-crash rule gives the new id every dirty row's land at
/// `epoch + 1` and raises the generation once — and it runs the whole map alone (`static`
/// with a table of itself) from the store, the keepers of both seam regions back. Its
/// registration.
fn restart_alone(w: &mut World, db: &PgDb, dir: &std::path::Path, boot: u64) -> world_store::Registered {
    for h in w.pair.sims.iter().map(|s| s.ordinal).collect::<Vec<_>>() {
        kill_host(w, h);
    }
    let first = db.open();
    // Every host died a lease ago (the registration below judges by a lease of 0 too).
    first.set_host_lease_ms(0);
    server::roles::resolve_before_claim(true, &first, "").expect("the staged parts are resolved");
    first.shutdown();
    let store = db.open();
    store.set_host_lease_ms(0);
    let want = next_id(db);
    let (mut links, registered) = boot_links(&store, want, "host-alone", boot, TOKEN, dir, "127.0.0.1:0");
    store.set_host_lease_ms(game::HOST_LEASE_MS);
    let me = registered.id;
    let s = store.super_region_regions();
    let alone = game::Static { me, layout: vec![me] };
    // Its whole extent under a per-host fence (this rig's coordinator runs two-phase world
    // cuts, which an all-hosts fence cannot stage); the total crash moved every dirty row's
    // in-flight rows to it.
    let srs: Vec<game::SuperRegion> = game::Ownership::claim_set(&alone, Some(extent()), s).into_iter().collect();
    let claims = HostClaims::claim_srs(store.clone(), me, srs).unwrap();
    let stored = store.stored_regions().unwrap();
    let regions: std::collections::BTreeMap<RegionCoords, game::StoredRegion> =
        stored.keys().filter_map(|rc| claims.load_region(*rc).unwrap().map(|r| (*rc, r))).collect();
    let mut spawner = InlineSpawner::with_portable(regions);
    let target = w.pair.tick.load(std::sync::atomic::Ordering::SeqCst);
    while spawner.world_tick() < target {
        spawner.tick_all();
    }
    let mut sim = Sim::with_spawner(me, s, boot, registered.generation, &mut links, spawner);
    sim.manager.set_ownership(Box::new(alone), s);
    sim.manager.set_stored_regions(stored);
    sim.manager.restore_in_flight(store.load_in_flight(claims.fence().in_flight_scope()).unwrap());
    let switch = SwitchLink(Arc::new(std::sync::Mutex::new((None, 0))));
    if let Some(c) = &w.coordinator {
        switch.replace(quic_world_link(c.addr, w.cert.clone(), me, boot, me.0));
    }
    *slot(&mut w.switches, me, SwitchLink::default()) = switch.clone();
    sim.manager.set_world_link(Box::new(switch), boot, 0);
    sim.manager.set_world_map(land_map());
    let generation = server::claims::WorldGeneration::new(registered.generation);
    *slot(&mut w.generations, me, Default::default()) = generation.clone();
    sim.cuts = Some(server::cuts::CutWriter::with_world_cuts(claims.clone(), generation));
    let now = w.pair.now();
    for rc in [w.seam.0, w.seam.1] {
        let owner = fixed_owner(rc, s);
        let (player, _) = store.resolve_player(&format!("hosts-test-keeper-{}", owner.0)).unwrap();
        sim.keep_as(KEEPER + owner.0 as ClientId, player, rc, now);
    }
    w.pair.sims.push(sim);
    while w.links.len() <= me.0 as usize {
        w.links.push(None);
    }
    w.links[me.0 as usize] = Some(links);
    let fill = claims.clone();
    *slot(&mut w.claims, me, fill) = claims;
    registered
}

/// Host `h` exits (`cuts::exit_world`), the other hosts still running round by round
/// meanwhile; its outcome. Its peer listener and coordinator link go with it.
fn exit_host(w: &mut World, h: HostId) -> server::claims::ServerExit {
    exit_host_with(w, h, None, |_, _| {})
}

/// [`exit_host`] with a leave timeout (`None`: the default) and `each` run every round
/// with the exiting manager and the other hosts' claims.
fn exit_host_with(
    w: &mut World,
    h: HostId,
    leave_timeout: Option<Duration>,
    each: impl FnMut(&WorldManager<InlineSpawner>, &[HostClaims]),
) -> server::claims::ServerExit {
    exit_host_leaving(w, h, leave_timeout, each).0
}

/// [`exit_host_with`], and the exiting manager's leave outcome when its exit returned
/// (`WorldManager::leave_outcome`: `Some(true)` = its leave cut committed) — what
/// `server::run` decides a drained host's next step by (`server::drain::stays_drained`).
fn exit_host_leaving(
    w: &mut World,
    h: HostId,
    leave_timeout: Option<Duration>,
    mut each: impl FnMut(&WorldManager<InlineSpawner>, &[HostClaims]),
) -> (server::claims::ServerExit, Option<bool>) {
    let i = w.pair.sims.iter().position(|s| s.ordinal == h).expect("the host is up");
    let mut sim = w.pair.sims.remove(i);
    let fenced = w.claims[h.0 as usize].fenced().clone();
    let generation = w.generations[h.0 as usize].clone();
    let started = w.pair.started;
    let region_out = sim.region_out.clone();
    let mut cuts = sim.cuts.take();
    if let (Some(c), Some(t)) = (cuts.as_mut(), leave_timeout) {
        c.set_leave_timeout(t);
    }
    let claims = w.claims.clone();
    let pair = &mut w.pair;
    let exit = server::cuts::exit_world(
        &mut sim.manager,
        server::cuts::ExitParams {
            region_out: &region_out,
            now_ms: || started.elapsed().as_millis() as u64,
            cuts,
            coordinator: None,
            fenced: &fenced,
            world_generation: &generation,
            park_timeout: server::cuts::EXIT_PARK_TIMEOUT,
            save_timeout: Duration::from_secs(10),
            now_tick: || 0,
        },
        |m| {
            // Its own regions tick with the others' (one world tick everywhere).
            m.spawner_mut().tick_all();
            m.spawner_mut().pump();
            pair.round();
            each(m, &claims);
        },
    );
    w.switches[h.0 as usize].drop_link();
    w.links[h.0 as usize] = None;
    (exit, sim.manager.leave_outcome())
}

/// The running regions of every host up holding `id`, with their host.
fn holders_all(w: &mut World, id: EntityId) -> Vec<(HostId, RegionCoords)> {
    let mut out = Vec::new();
    for sim in &mut w.pair.sims {
        let h = sim.ordinal;
        out.extend(sim.holders(id).into_iter().map(|rc| (h, rc)));
    }
    out
}

/// A slime spawned in the middle of `rc` (host `h`'s); its id.
fn spawn_slime_in(w: &mut World, h: HostId, rc: RegionCoords, x: f32) -> EntityId {
    let mut id = None;
    sim(w, h).manager.spawner_mut().with_region(rc, |region| {
        let tick = region.current_tick();
        let pos = game::parry::math::Vector::new(x, above_ground_y(), REGION_SIZE / 2.0);
        region.with_data(|d| {
            let e = d.spawn_slime_safe(pos, 1, 0.0, tick);
            id = *d.ecs.id.try_get(e);
        });
    });
    id.expect("the slime has an EntityId")
}

/// `saved_tick` of `rc`'s stored row.
fn saved_tick(db: &PgDb, rc: RegionCoords) -> i64 {
    query_i64s(db, &format!("SELECT saved_tick FROM regions WHERE rx = {} AND rz = {}", rc.x, rc.z))[0]
}

/// The newest committed cut: `(cut, tick)`.
fn last_committed(db: &PgDb) -> (i64, i64) {
    decisions(db).into_iter().filter(|(_, _, k)| *k).map(|(c, t, _)| (c, t)).last().expect("a committed cut")
}

/// A host booted as `server::run` boots it under `claim`: registered FIRST (the next id),
/// holding nothing but what the registration gave it (a total crash's land, taken into its
/// fence), its manager on a `PgClaim` directory read from the store, its claim loop
/// ([`server::claims::DirectoryKeeper`]).
fn claim_host(db: &PgDb, dir: &std::path::Path, boot: u64) -> (Sim, PeerLinks, HostClaims) {
    let store = db.open();
    let s = store.super_region_regions();
    let want = next_id(db);
    let (mut links, registered) = boot_links(&store, want, &format!("host-{}", want.0), boot, TOKEN, dir, "127.0.0.1:0");
    let ordinal = registered.id;
    let claims = HostClaims::none(store.clone(), ordinal);
    claims.reclaim_own(boot).expect("what the registration gave it taken");
    let mut sim = Sim::with_spawner(ordinal, s, boot, registered.generation, &mut links, InlineSpawner::default());
    sim.manager.set_ownership(Box::new(game::PgClaim::new(ordinal)), s);
    let (rows, live, draining) = server::claims::read_directory(&store, ordinal).expect("the directory is read");
    sim.manager.set_draining(draining);
    sim.manager.directory_read(rows, live);
    sim.keeper = Some(server::claims::DirectoryKeeper::new(claims.clone(), boot));
    (sim, links, claims)
}

/// The first region (lowest corner) of each of `n` super-regions of the claimed map.
fn super_region_corners(s: u32, n: usize) -> Vec<(game::SuperRegion, RegionCoords)> {
    let e = extent();
    let side = (e.width as i32 + s as i32 - 1) / s as i32;
    let all: Vec<(game::SuperRegion, RegionCoords)> = (0..side)
        .flat_map(|sx| (0..side).map(move |sz| (sx, sz)))
        .map(|(sx, sz)| ((sx, sz), RegionCoords::new(sx * s as i32, sz * s as i32)))
        .collect();
    assert!(all.len() >= n, "the map has {} super-regions, not {n}", all.len());
    all.into_iter().take(n).collect()
}

/// Slime `id` in host `h`'s seam region, put just past the seam toward the other host's.
fn push_over_the_seam(w: &mut World, h: HostId, id: EntityId) {
    let (a, _) = w.seam;
    let from = seam_of(w, h);
    // The seam is `b = a + x`: from `a` over +x, from `b` over -x.
    let x = if from == a { REGION_SIZE + game::FLIP_HYSTERESIS + 2.0 } else { -game::FLIP_HYSTERESIS - 2.0 };
    sim(w, h).manager.spawner_mut().with_region(from, |region| {
        let key = region.data().id_index[&id];
        let at = game::rapier3d::math::Vector::new(x, above_ground_y(), REGION_SIZE / 2.0);
        region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
    });
}

