//! R3 Task 9: the real link between region hosts. Each host binds a service-link
//! listener, writes its address and pin into its `hosts` row, and dials every LOWER
//! ordinal it finds there ([`PeerLinks`]); the managers' peer sessions run over the
//! resulting [`server::peer_link::QuicPeerLink`]s. Two "processes" run in this test
//! process — each its own store handle, claims and `WorldManager<InlineSpawner>` — over
//! real links on localhost, against Postgres (`MMO_TEST_DATABASE_URL`; skipped with a
//! message when unset). Its own test binary: process-global block props, real threads
//! and sockets.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver};
use game::{
    RegionSpawner, ClientId, EntityId, HostEndpoint, InlineSpawner, Ownership, PeerHello, PeerLink, PeerMsg, PeerSession, PlayerId,
    PlayerRecord, RegionCoords, RegionOutput, ServerEvent, ServerPacket, Volatile, WorldManager,
    REGION_SIZE,
};
use login_api::{HostToLogin, LoginToHost};
use server::claims::HostClaims;
use server::login_link::{LoginLink, LoginRelay, SpawnOrders};
use server::player_store::{PlayerKey, PlayerStore};
use server::peer_link::{PeerLinkConfig, PeerLinks};
use server::roles::{check_several_hosts, Role, SEVERAL_HOSTS_NEED_LOGIN, SEVERAL_HOSTS_NEED_ROLE_HOST, SPLIT_ROLES_NEED_POSTGRES};
use world_store::{StoreHandle, StoreUrl};

/// Wall time any wait below may take before the test fails.
const WAIT: Duration = Duration::from_secs(30);
const TOKEN: &str = "hosts-test-service-token";
const HOSTS: u32 = 2;
/// The keeper client of each host: its body keeps that host's seam region running.
const KEEPER: ClientId = 0;

// ---- world and store ------------------------------------------------------------------

struct PgDb(pg_test_support::PgTestDb);

impl PgDb {
    fn create(tag: &str) -> Option<PgDb> {
        let db = pg_test_support::PgTestDb::create(&format!("mmo_hosts_{tag}"));
        if db.is_none() {
            eprintln!("{} (MMO_TEST_DATABASE_URL=postgres://mmo:mmo@127.0.0.1:55432/postgres)", pg_test_support::SKIP_MSG);
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
    let dir = std::env::temp_dir().join(format!("mmo-hosts-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn install_props() {
    let registry = game::BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
    game::install_block_props(registry.props());
}

/// The claimed map: 48 × 48 regions from the origin.
fn extent() -> game::MapExtent {
    game::MapExtent { origin: RegionCoords::new(0, 0), width: 48, height: 48 }
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

/// Where host `ordinal` says players reach it.
fn endpoint(ordinal: u32) -> HostEndpoint {
    HostEndpoint {
        ordinal,
        quic: Some(format!("127.0.0.1:{}", 7400 + ordinal)),
        webtransport: Some(format!("127.0.0.1:{}", 7500 + ordinal)),
        cert_hash: None,
    }
}

/// One host process's store handle and claims (as `server::run` makes them).
fn claim(db: &PgDb, ordinal: u32) -> (StoreHandle, HostClaims) {
    let store = db.open();
    server::world_file::bind_world_terrain(&store).expect("a stud world");
    let s = store.super_region_regions();
    let claims = HostClaims::claim(store.clone(), Ownership::Static { ordinal, hosts: HOSTS }, Some(extent()), s).unwrap();
    (store, claims)
}

/// Host `ordinal`'s peer links on `bind`, its pin in `dir`, with `token`.
fn start_links(
    claims: &HostClaims,
    ordinal: u32,
    boot: u64,
    token: &str,
    dir: &std::path::Path,
    bind: &str,
) -> PeerLinks {
    let config = PeerLinkConfig {
        bind: bind.parse().unwrap(),
        cert_hash_file: dir.join(format!("host-{ordinal}.peer.cert-sha256")),
        delay: Duration::ZERO,
    };
    PeerLinks::start(ordinal, HOSTS, boot, endpoint(ordinal), token.into(), claims.store().clone(), claims.fence(), config)
        .expect("the peer links start")
}

/// The one link `links` hands out (two hosts: the other one).
fn only_link(links: &mut PeerLinks) -> (u32, Box<dyn PeerLink + Send>) {
    let mut all = links.take();
    assert_eq!(all.len(), 1, "one link per other host");
    all.pop().unwrap()
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
    ordinal: u32,
    boot: u64,
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    cuts: Option<server::cuts::CutWriter>,
    /// The login relay's server half (login tests: [`login_hosts`]).
    login: Option<SimLogin>,
    /// Its peer links' gate (a test takes them down and up).
    gate: PeerGate,
    /// Its listener for other hosts' saves (Task 19 tests).
    terrain: Option<server::cuts::TerrainWatch>,
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
    fn with_spawner(ordinal: u32, s: u32, boot: u64, generation: u64, links: &mut PeerLinks, spawner: InlineSpawner) -> Sim {
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let mut manager = WorldManager::new(
            spawner.with_stud_genesis(Arc::new(stud_flat_seed)),
            out_send,
            region_out_send,
            Box::new(game::NullLodBuilder::default()),
        );
        manager.set_ownership(Ownership::Static { ordinal, hosts: HOSTS }, s);
        manager.set_endpoints(endpoint(ordinal), game::seeded_nonces(0x5EED + u64::from(ordinal)));
        let gate = PeerGate::default();
        for (other, link) in links.take() {
            let link: Box<dyn PeerLink + Send> = Box::new(GatedLink { inner: link, gate: gate.clone() });
            let hello = PeerHello {
                ordinal,
                boot,
                generation,
                endpoint: endpoint(ordinal),
                received_through: 0,
                received_from_boot: None,
            };
            manager.set_peer(other, PeerSession::new(hello, other), link);
        }
        manager.set_host_generation(generation);
        Sim { ordinal, boot, manager, region_out, packets, cuts: None, login: None, gate, terrain: None }
    }

    /// A keeper client whose body stands in the middle of `rc` (this host's): `rc` runs.
    fn keep(&mut self, player: PlayerId, rc: RegionCoords, now: u64) {
        let y = game::plate_units(30) + 3.0;
        let record = PlayerRecord {
            client: game::Client::default(),
            pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, y, REGION_SIZE / 2.0)),
            region: rc,
            fortress: None,
            adventure: None,
        };
        let ev = ServerEvent::ClientConnected { client: KEEPER, player, record: Some(record) };
        assert!(self.manager.handle_server_event(ev, now));
    }

    fn settle(&mut self, now: u64) {
        if let Some(login) = &self.login {
            for ev in login.events.try_iter() {
                assert!(self.manager.handle_server_event(ev, now));
            }
        }
        for _ in 0..3 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, now);
            }
        }
        for _ in self.packets.try_iter() {}
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
        for sim in &mut self.sims {
            sim.manager.spawner_mut().tick_all();
        }
        let tick = self.sims[0].manager.spawner_mut().world_tick();
        self.tick.store(tick, std::sync::atomic::Ordering::SeqCst);
        for sim in &mut self.sims {
            sim.settle(now);
            sim.manager.maintain(now);
            if let Some(cuts) = sim.cuts.as_mut() {
                cuts.pump(&mut sim.manager);
            }
            if let Some(watch) = sim.terrain.as_mut() {
                watch.pump(&mut sim.manager);
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
    let owner = |rc: RegionCoords| Ownership::Static { ordinal: 0, hosts: HOSTS }.owner_of(rc, s);
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
    for ordinal in 0..HOSTS {
        let (store, claims) = claim(db, ordinal);
        s = store.super_region_regions();
        let boot = 1000 + u64::from(ordinal);
        let mut links = start_links(&claims, ordinal, boot, TOKEN, dir, "127.0.0.1:0");
        let generation = links.registered().generation;
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
        let owner = pair.sims[0].manager.owner_of(rc);
        // A player id the store gave out (a cut writes only records of those).
        let (player, _) = all_claims[owner as usize].store().resolve_player(&format!("hosts-test-keeper-{owner}")).unwrap();
        pair.sims[owner as usize].keep(player, rc, now);
    }
    pair.until("every peer session up", |p| p.sims.iter().all(|sim| sim.manager.reaches(seam.0) && sim.manager.reaches(seam.1)));
    (pair, seam, all_links, all_claims)
}

// ---- tests ------------------------------------------------------------------------------

/// Each host finds the other through the `hosts` table (address and pin of the lower
/// ordinal, dialed by the higher), and over the link they learn which regions the other
/// runs (`Volatile::Running` on Up, `RegionUp`).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn two_hosts_find_each_other_through_the_hosts_table_and_exchange_running_sets() {
    install_props();
    let Some(db) = PgDb::create("find") else { return };
    let dir = scratch("find");
    let (mut pair, (a, b), links, claims) = two_hosts(&db, &dir);

    let rows = claims[0].store().hosts().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for (row, links) in rows.iter().zip(&links) {
        assert_eq!(row.peer_addr.as_deref(), Some(links.local_addr().to_string().as_str()), "{row:?}");
        assert_eq!(row.peer_cert.as_deref(), Some(links.cert_sha256()), "{row:?}");
        assert_eq!(row.quic, endpoint(row.ordinal).quic);
        assert_eq!(row.webtransport, endpoint(row.ordinal).webtransport);
    }

    let (ha, hb) = (pair.sims[0].manager.owner_of(a) as usize, pair.sims[0].manager.owner_of(b) as usize);
    pair.until("each host knows the other's seam region runs", |p| {
        p.sims[ha].manager.foreign_running().contains(&b) && p.sims[hb].manager.foreign_running().contains(&a)
    });
    drop(pair);
    drop(links);
    for c in claims {
        c.store().clone().shutdown();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A slime past the seam on one real host lands on the other, exactly once, and
/// nothing is left unacknowledged.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn slime_crosses_between_real_hosts() {
    install_props();
    let Some(db) = PgDb::create("slime") else { return };
    let dir = scratch("slime");
    let (mut pair, (a, b), links, claims) = two_hosts(&db, &dir);
    let (ha, hb) = (pair.sims[0].manager.owner_of(a) as usize, pair.sims[0].manager.owner_of(b) as usize);
    // Both know the other's region runs (a transfer is sent only to a running owner's region).
    pair.until("running sets exchanged", |p| {
        p.sims[ha].manager.foreign_running().contains(&b) && p.sims[hb].manager.foreign_running().contains(&a)
    });
    let mut id = None;
    pair.sims[ha].manager.spawner_mut().with_region(a, |region| {
        let tick = region.current_tick();
        let pos = game::parry::math::Vector::new(REGION_SIZE + game::FLIP_HYSTERESIS + 2.0, game::plate_units(30) + 3.0, REGION_SIZE / 2.0);
        region.with_data(|d| {
            let e = d.spawn_slime_safe(pos, 1, 0.0, tick);
            id = *d.ecs.id.try_get(e);
        });
    });
    let id = id.expect("the slime has an EntityId");
    pair.until("the slime owned by b on its host, nothing unacknowledged", |p| {
        p.sims[hb].holders(id) == [b]
            && p.sims[ha].holders(id).is_empty()
            && p.sims.iter().all(|s| s.manager.peer_unreceived() == 0)
    });
    for _ in 0..20 {
        pair.round();
    }
    assert_eq!(pair.sims[hb].holders(id), [b], "still there, once");
    assert!(pair.sims[ha].holders(id).is_empty(), "not on the sending host");
    assert_eq!(pair.sims[ha].manager.pending_arrival_total(), 0, "nothing waits on the sending host");
    assert_eq!(pair.sims[ha].manager.in_flight_count(), 0, "nothing in flight on the sending host");
    drop(pair);
    drop(links);
    for c in claims {
        c.store().clone().shutdown();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host dialing with another service token never comes up on either side.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn peer_with_a_wrong_token_is_refused() {
    let Some(db) = PgDb::create("token") else { return };
    let dir = scratch("token");
    let (_s0, c0) = claim(&db, 0);
    let (_s1, c1) = claim(&db, 1);
    let mut l0 = start_links(&c0, 0, 1, TOKEN, &dir, "127.0.0.1:0");
    let mut l1 = start_links(&c1, 1, 2, "not-the-token", &dir, "127.0.0.1:0");
    let (o0, mut to1) = only_link(&mut l0);
    let (o1, mut to0) = only_link(&mut l1);
    assert_eq!((o0, o1), (1, 0));
    // Long enough for the dialer to find the row and try a few times.
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        to1.pump(0);
        to0.pump(0);
        assert!(!to1.is_up() && !to0.is_up(), "a peer with a wrong token came up");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!((to1.connection(), to0.connection()), (0, 0));
    drop((to1, to0, l0, l1));
    c0.store().clone().shutdown();
    c1.store().clone().shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lower host restarts on another port (same pin file, a new boot, a new row): the
/// higher one's dialer picks the new row up and is connected again within 5 s, as a new
/// connection, and frames flow.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn restarted_listener_is_found_again() {
    let Some(db) = PgDb::create("restart") else { return };
    let dir = scratch("restart");
    let (_s0, c0) = claim(&db, 0);
    let (_s1, c1) = claim(&db, 1);
    let mut l0 = start_links(&c0, 0, 1, TOKEN, &dir, "127.0.0.1:0");
    let mut l1 = start_links(&c1, 1, 2, TOKEN, &dir, "127.0.0.1:0");
    let old_addr = l0.local_addr();
    let (_, mut to1) = only_link(&mut l0);
    let (_, mut to0) = only_link(&mut l1);
    pump_until("both ends up", WAIT, &mut [&mut to1, &mut to0], |l| l.iter().all(|l| l.is_up()));
    assert_eq!(to0.connection(), 1);
    to0.send(PeerMsg::Volatile(Volatile::Running(vec![RegionCoords::new(1, 2)])));
    pump_until("a frame from 1 to 0", WAIT, &mut [&mut to1], |l| {
        l[0].recv() == Some(PeerMsg::Volatile(Volatile::Running(vec![RegionCoords::new(1, 2)])))
    });

    // Host 0 stops: its listener goes, its port is freed.
    drop(to1);
    drop(l0);
    c0.store().clone().shutdown();
    pump_until("host 1 sees the link down", WAIT, &mut [&mut to0], |l| !l[0].is_up());

    // Host 0 starts again: a new claim, a new boot, another port.
    let new_bind = loop {
        let addr = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        if addr != old_addr {
            break addr;
        }
    };
    let (_s0, c0) = claim(&db, 0);
    let mut l0 = start_links(&c0, 0, 3, TOKEN, &dir, &new_bind.to_string());
    assert_eq!(l0.local_addr(), new_bind);
    let (_, mut to1) = only_link(&mut l0);
    pump_until("found again within 5 s", Duration::from_secs(5), &mut [&mut to1, &mut to0], |l| l.iter().all(|l| l.is_up()));
    assert_eq!(to0.connection(), 2, "a new connection");
    to0.send(PeerMsg::Ack { through: 7 });
    pump_until("a frame over the new connection", WAIT, &mut [&mut to1], |l| l[0].recv() == Some(PeerMsg::Ack { through: 7 }));
    drop((to1, to0, l0, l1));
    c0.store().clone().shutdown();
    c1.store().clone().shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Several hosts start only as `host`, on Postgres, with the login link (`jwt` with a
/// service token); one host (or `all`) as before.
#[test]
fn several_hosts_refuse_local_auth() {
    assert_eq!(check_several_hosts(Role::Host, 2, true, false), Err(SEVERAL_HOSTS_NEED_LOGIN.to_string()));
    assert_eq!(SEVERAL_HOSTS_NEED_LOGIN, "several hosts need the login link: ids and placement come from it");
    assert_eq!(check_several_hosts(Role::Host, 2, true, true), Ok(()));
    assert_eq!(check_several_hosts(Role::All, 2, true, true), Err(SEVERAL_HOSTS_NEED_ROLE_HOST.to_string()));
    assert_eq!(check_several_hosts(Role::Host, 3, false, true), Err(SPLIT_ROLES_NEED_POSTGRES.to_string()));
    for role in [Role::All, Role::Host] {
        assert_eq!(check_several_hosts(role, 1, false, false), Ok(()), "{role:?}: one host as before");
    }
}

// ---- world cuts (Task 13) ----------------------------------------------------------------

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

/// The decided cuts: `(cut, tick, committed)`.
fn decisions(db: &PgDb) -> Vec<(i64, i64, bool)> {
    let ids = query_i64s(db, "SELECT cut_id FROM cut_decisions ORDER BY cut_id");
    let ticks = query_i64s(db, "SELECT tick FROM cut_decisions ORDER BY cut_id");
    let committed = query_i64s(db, "SELECT committed FROM cut_decisions ORDER BY cut_id");
    ids.into_iter().zip(ticks).zip(committed).map(|((c, t), k)| (c, t, k != 0)).collect()
}

/// Host `ordinal`'s link to the coordinator at `addr` (its pin in `cert_file`), its hello
/// carrying the host's claim `epoch`.
fn quic_world_link(addr: std::net::SocketAddr, cert_file: PathBuf, ordinal: u32, boot: u64, epoch: u64) -> server::roles::QuicWorldLink {
    let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Host { ordinal, boot, epoch });
    server::roles::QuicWorldLink::connect(addr, move || svc_link::read_cert_hash("MMO_HOSTS_TEST_UNSET_PIN", &cert_file), hello)
}

/// A host's coordinator link a test can drop and make again (a manager installs its link
/// once): `None` = down, its connection closed; each new link is a new generation.
#[derive(Clone)]
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
    let coordinator = server::coordinator_rt::start_listening_with(
        store.clone(),
        start,
        "127.0.0.1:0".parse().unwrap(),
        TOKEN.into(),
        Some(cert.clone()),
        server::claims::Fenced::default(),
        Some(cuts),
    )
    .expect("the coordinator listens");
    let mut switches = Vec::new();
    let mut generations = Vec::new();
    for (ordinal, sim) in pair.sims.iter_mut().enumerate() {
        let c = &claims[ordinal];
        let link = quic_world_link(coordinator.addr, cert.clone(), ordinal as u32, 1000 + ordinal as u64, c.fence().claim_epoch());
        let switch = SwitchLink(Arc::new(std::sync::Mutex::new((Some(link), 0))));
        switches.push(switch.clone());
        sim.manager.set_world_link(Box::new(switch), 1000 + ordinal as u64, 0);
        sim.manager.set_world_map(land_map());
        let generation = server::claims::WorldGeneration::new(links[ordinal].registered().generation);
        generations.push(generation.clone());
        sim.cuts = Some(server::cuts::CutWriter::with_world_cuts(c.clone(), generation));
    }
    pair.until("both hosts attached", |p| p.sims.iter().all(|s| s.manager.world_attached()));
    let links = links.into_iter().map(Some).collect();
    World { pair, seam, coordinator: Some(coordinator), cert, links, claims, store, switches, generations }
}

/// Two real hosts and a real coordinator: a world cut is picked by the coordinator,
/// staged by both hosts and committed through one decision — both hosts' seam regions
/// carry its tick, one committed row is in `cut_decisions`, and nothing stays prepared.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn two_hosts_commit_one_world_cut() {
    install_props();
    let Some(db) = PgDb::create("wcut") else { return };
    let dir = scratch("wcut");
    let w = world(&db, &dir);
    let (a, b) = w.seam;
    let World { mut pair, coordinator, links, claims, store, .. } = w;
    let coordinator = coordinator.unwrap();
    pair.until("a world cut committed", |_| decisions(&db).iter().any(|(_, _, c)| *c));
    pair.until("both hosts settled it", |p| p.sims.iter().all(|s| s.cuts.as_ref().unwrap().world_part().is_none()));
    let committed: Vec<(i64, i64, bool)> = decisions(&db).into_iter().filter(|(_, _, c)| *c).collect();
    let (cut, tick, _) = committed[0];
    for rc in [a, b] {
        let saved = query_i64s(&db, &format!("SELECT saved_tick FROM regions WHERE rx = {} AND rz = {}", rc.x, rc.z));
        assert!(!saved.is_empty(), "{rc:?} is stored");
        assert!(saved[0] >= tick, "{rc:?} carries the cut's tick ({} < {tick})", saved[0]);
    }
    let first: Vec<i64> = committed.iter().filter(|(c, _, _)| *c == cut).map(|(_, t, _)| *t).collect();
    assert_eq!(first.len(), 1, "one decision row for cut {cut}");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    // Each committed cut's tick is on both seam regions as of that cut (the latest one).
    let latest = committed.last().unwrap().1;
    for rc in [a, b] {
        let saved = query_i64s(&db, &format!("SELECT saved_tick FROM regions WHERE rx = {} AND rz = {}", rc.x, rc.z))[0];
        let any_later = decisions(&db).iter().any(|(_, t, c)| *c && *t > latest);
        assert!(saved == latest || any_later, "{rc:?} saved at {saved}, the latest committed cut is at {latest}");
    }
    assert_eq!(coordinator.thread.shutdown(Duration::from_secs(10)), server::claims::ServerExit::Clean);
    drop(pair);
    drop(links);
    store.shutdown();
    for c in claims {
        c.store().clone().shutdown();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Host 1's link to the coordinator drops while a world cut runs: the cut is aborted
/// (a decision row with `committed = 0`), host 0's part — staged or not — is rolled back
/// (nothing stays prepared) and settled on host 0; once host 1 is back the next cut
/// commits.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_cut_with_one_host_detached_is_aborted_and_leaves_nothing_staged() {
    install_props();
    let Some(db) = PgDb::create("wdet") else { return };
    let dir = scratch("wdet");
    let World { mut pair, coordinator, cert, links, claims, store, switches, .. } = world(&db, &dir);
    let coordinator = coordinator.unwrap();
    pair.until("host 0 takes part in a world cut", |p| p.sims[0].manager.world_cut().is_some());
    let running = coordinator.thread.inspect(|c| c.world_cut_running(), WAIT).flatten().expect("a world cut runs");
    switches[1].drop_link();
    pair.until("the cut is decided", |_| decisions(&db).iter().any(|(c, _, _)| *c as u64 == running.0 .0));
    let row = decisions(&db).into_iter().find(|(c, _, _)| *c as u64 == running.0 .0).unwrap();
    assert!(!row.2, "the cut with a host detached is aborted");
    pair.until("host 0 settled it", |p| {
        p.sims[0].cuts.as_ref().unwrap().world_part().is_none() && p.sims[0].manager.world_cut() != Some(running.0)
    });
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    // Host 1 back: the next cut commits.
    switches[1].replace(quic_world_link(coordinator.addr, cert, 1, 1001, claims[1].fence().claim_epoch()));
    pair.until("a later world cut committed", |_| decisions(&db).iter().any(|(c, _, k)| *k && *c as u64 > running.0 .0));
    pair.until("both hosts settled it", |p| p.sims.iter().all(|s| s.cuts.as_ref().unwrap().world_part().is_none()));
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    assert_eq!(coordinator.thread.shutdown(Duration::from_secs(10)), server::claims::ServerExit::Clean);
    drop(pair);
    drop(links);
    store.shutdown();
    for c in claims {
        c.store().clone().shutdown();
    }
    let _ = std::fs::remove_dir_all(&dir);
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

/// Important 2: the coordinator's decision write and its first abort both fail: it keeps
/// trying until an abort is stored, announces only that, and the next cut commits on
/// both hosts — nothing stays staged.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_failed_decision_does_not_stall_later_cuts() {
    install_props();
    let Some(db) = PgDb::create("wfail") else { return };
    let dir = scratch("wfail");
    let mut w = world(&db, &dir);
    w.store.fail_next_decisions(2);
    w.pair.until("a world cut decided", |_| !decisions(&db).is_empty());
    let first = decisions(&db)[0];
    assert!(!first.2, "the cut whose decision failed is stored aborted");
    assert_eq!(w.store.fail_next_decisions_left(), 0, "the decision and one abort failed first");
    w.until_committed_after(&db, first.0);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Important 2: a host told its cut is aborted while the store has no decision (the
/// coordinator cannot write one) records the abort itself and settles; the coordinator's
/// later decision finds it; the next cut commits once the store works again.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_told_aborted_without_a_stored_decision_aborts_it_itself() {
    install_props();
    let Some(db) = PgDb::create("wself") else { return };
    let dir = scratch("wself");
    let mut w = world(&db, &dir);
    w.store.fail_next_decisions(u32::MAX);
    let cut = until_part(&mut w, 0, &["prepared"]);
    assert_eq!(decision_of(&db, cut), None, "undecided");
    w.coordinator.as_ref().unwrap().thread.with_core(move |c| c.cut_outcome(0, cut, false), WAIT).expect("the core answers");
    w.pair.until("host 0 settled its part", |p| p.sims[0].cuts.as_ref().unwrap().world_part().is_none());
    assert_eq!(decision_of(&db, cut), Some(false), "host 0 recorded the abort");
    w.pair.until("host 1 settled its part too", |p| p.sims[1].cuts.as_ref().unwrap().world_part().is_none());
    w.store.fail_next_decisions(0);
    w.until_committed_after(&db, cut.0 as i64);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Criterion D: a host's handle still holds an undecided part of another cut (`CutPending`):
/// its world part fails, and once that other cut is decided the host settles it and the
/// next cut commits.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cut_pending_on_a_host_settles_that_cut_first() {
    install_props();
    let Some(db) = PgDb::create("wpend") else { return };
    let dir = scratch("wpend");
    let mut w = world(&db, &dir);
    let other = game::CutId(77);
    let generation = w.generations[0].mine();
    let empty = game::WorldCut { id: other, host: 0, tick: 1, regions: vec![], in_flight: vec![], players: vec![], history: None };
    let staged = w.claims[0].store().prepare_cut(empty, w.claims[0].fence().clone(), generation).recv().unwrap();
    assert!(staged.is_ok(), "the other cut's part is staged: {staged:?}");
    w.pair.until("a world cut decided", |_| decisions(&db).iter().any(|(c, _, _)| *c as u64 != other.0));
    let failed = decisions(&db).into_iter().find(|(c, _, _)| *c as u64 != other.0).unwrap();
    assert!(!failed.2, "host 0 could not stage its part: the cut is aborted");
    assert_eq!(w.pair.sims[0].cuts.as_ref().unwrap().world_part(), Some(other), "host 0 now settles cut {}", other.0);
    w.store.abort_cut(other, 1).unwrap();
    w.until_committed_after(&db, failed.0);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Criterion D: the world moved to a newer generation while a cut was pending: the host's
/// prepare is refused `StaleGeneration`, the host notes the newer generation (it exits
/// for the rollback), and the cut is aborted.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stale_generation_on_prepare_is_noted() {
    install_props();
    let Some(db) = PgDb::create("wstale") else { return };
    let dir = scratch("wstale");
    let mut w = world(&db, &dir);
    let cut = until_part(&mut w, 0, &["pending"]);
    exec(&db, "UPDATE world SET generation = generation + 1");
    w.pair.until("the cut decided", |_| decision_of(&db, cut).is_some());
    assert_eq!(decision_of(&db, cut), Some(false), "aborted");
    w.pair.until("host 0 noted the newer generation", |_| w.generations[0].is_set());
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Criterion D: host 1's prepare waits on a lock another transaction holds past its lock
/// timeout: the part fails (nothing staged), the cut is aborted, and once the lock is
/// gone the next cut commits.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lock_timeout_on_prepare_fails_the_part_and_the_next_cut_commits() {
    install_props();
    let Some(db) = PgDb::create("wlock") else { return };
    let dir = scratch("wlock");
    let mut w = world(&db, &dir);
    w.claims[1].store().set_cut_lock_timeout_ms(200);
    // A transaction holding host 1's super-region rows until released.
    let (locked_tx, locked) = crossbeam::channel::bounded::<()>(1);
    let (release_tx, release) = crossbeam::channel::bounded::<()>(1);
    let url = db.0.url.clone();
    let holder = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use sqlx::Connection;
            let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
            let mut tx = c.begin().await.unwrap();
            sqlx::query("SELECT * FROM super_regions WHERE owner = 1 FOR UPDATE").execute(&mut *tx).await.unwrap();
            locked_tx.send(()).unwrap();
            let _ = release.recv();
            tx.rollback().await.unwrap();
        })
    });
    locked.recv().unwrap();
    w.pair.until("a world cut decided", |_| !decisions(&db).is_empty());
    let first = decisions(&db)[0];
    assert!(!first.2, "host 1's part timed out on the lock: aborted");
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    w.claims[1].store().set_cut_lock_timeout_ms(world_store::CUT_LOCK_TIMEOUT_MS);
    w.until_committed_after(&db, first.0);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Criterion D: host 1's `CutDone` is lost (its coordinator link dropped right after it
/// staged its part): its poll finds the stored decision and settles while the link is
/// still down.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lost_cut_done_is_settled_from_the_store() {
    install_props();
    let Some(db) = PgDb::create("wlost") else { return };
    let dir = scratch("wlost");
    let mut w = world(&db, &dir);
    let cut = until_part(&mut w, 1, &["prepared"]);
    w.switches[1].drop_link();
    w.pair.until("the cut decided", |_| decision_of(&db, cut).is_some());
    w.pair.until("host 1 settled its part with its link down", |p| p.sims[1].cuts.as_ref().unwrap().world_part().is_none());
    assert!(!w.pair.sims[1].manager.world_attached(), "host 1's link is still down");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    let link = quic_world_link(w.coordinator.as_ref().unwrap().addr, w.cert.clone(), 1, 1001, w.claims[1].fence().claim_epoch());
    w.switches[1].replace(link);
    w.until_committed_after(&db, cut.0 as i64);
    w.stop(&dir);
}

/// Fix round 2: a host that hears nothing about its staged part past the deadline (the
/// coordinator cannot store a decision, so it announces none) records the abort itself
/// and settles; once decisions work again the next cut commits.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_hearing_nothing_past_the_deadline_aborts_the_cut_itself() {
    install_props();
    let Some(db) = PgDb::create("wdead") else { return };
    let dir = scratch("wdead");
    let mut w = world(&db, &dir);
    w.store.fail_next_decisions(u32::MAX);
    w.pair.sims[0].cuts.as_mut().unwrap().set_world_part_deadline(Duration::from_secs(2));
    let cut = until_part(&mut w, 0, &["prepared"]);
    assert_eq!(decision_of(&db, cut), None, "undecided");
    w.pair.until("host 0 settled its part on its own", |p| p.sims[0].cuts.as_ref().unwrap().world_part().is_none());
    assert_eq!(decision_of(&db, cut), Some(false), "host 0 recorded the abort past its deadline");
    w.store.fail_next_decisions(0);
    w.until_committed_after(&db, cut.0 as i64);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

// ---- leaving and crashes (Task 14) -----------------------------------------------------

/// The world tick of the first host still up (what a restarted host's clock joins).
fn live_tick(w: &mut World) -> u64 {
    w.pair.sims.first_mut().map_or(0, |s| s.manager.spawner_mut().world_tick())
}

/// The seam region host `h` owns.
fn seam_of(w: &World, h: u32) -> RegionCoords {
    let (a, b) = w.seam;
    let owner = Ownership::Static { ordinal: h, hosts: HOSTS };
    if owner.owns(a, w.claims[h as usize].store().super_region_regions()) {
        a
    } else {
        b
    }
}

/// Host `h`'s simulation, if it is up.
fn sim(w: &mut World, h: u32) -> &mut Sim {
    w.pair.sims.iter_mut().find(|s| s.ordinal == h).expect("the host is up")
}

/// Host `h` crashes: its manager and cut writer go with nothing saved (what it staged
/// stays staged), its peer listener and its link to the coordinator drop. Its store
/// handle is shut down (a dead process's connections close).
fn kill_host(w: &mut World, h: u32) {
    let i = w.pair.sims.iter().position(|s| s.ordinal == h).expect("the host is up");
    drop(w.pair.sims.remove(i));
    w.switches[h as usize].drop_link();
    w.links[h as usize] = None;
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
    let coordinator = server::coordinator_rt::start_listening_with(
        store.clone(),
        start,
        "127.0.0.1:0".parse().unwrap(),
        TOKEN.into(),
        Some(w.cert.clone()),
        server::claims::Fenced::default(),
        Some(cuts),
    )
    .expect("the coordinator listens");
    let old = std::mem::replace(&mut w.store, store);
    old.shutdown();
    for sim in &w.pair.sims {
        let epoch = w.claims[sim.ordinal as usize].fence().claim_epoch();
        w.switches[sim.ordinal as usize].replace(quic_world_link(coordinator.addr, w.cert.clone(), sim.ordinal, sim.boot, epoch));
    }
    w.coordinator = Some(coordinator);
}

/// Host `h` starts again as boot `boot`, in `server::run`'s order: the parts a crash left
/// staged resolved, its super-regions claimed, its row registered (with its peer
/// listener), its regions and in-flight rows from the store, its clock joining the live
/// hosts', a link to the coordinator (if one is up), its cut writer, its keeper. Its
/// registration. With no host up its clock starts where the world's stopped.
fn restart_host(w: &mut World, db: &PgDb, dir: &std::path::Path, h: u32, boot: u64) -> world_store::HostBoot {
    let first = db.open();
    server::roles::recover_staged_cuts(&first, HOSTS).expect("the staged parts are resolved");
    first.shutdown();
    let (store, claims) = claim(db, h);
    let mut links = start_links(&claims, h, boot, TOKEN, dir, "127.0.0.1:0");
    let registered = links.registered();
    let s = store.super_region_regions();
    let owner = Ownership::Static { ordinal: h, hosts: HOSTS };
    let stored = store.stored_regions().unwrap();
    let regions: std::collections::BTreeMap<RegionCoords, game::StoredRegion> = stored
        .keys()
        .filter(|rc| owner.owns(**rc, s))
        .filter_map(|rc| claims.load_region(*rc).unwrap().map(|r| (*rc, r)))
        .collect();
    let mut spawner = InlineSpawner::with_portable(regions);
    // With no host up (every host left), the world tick they left at.
    let target = live_tick(w).max(w.pair.tick.load(std::sync::atomic::Ordering::SeqCst));
    while spawner.world_tick() < target {
        spawner.tick_all();
    }
    let mut sim = Sim::with_spawner(h, s, boot, registered.generation, &mut links, spawner);
    sim.manager.set_stored_regions(stored);
    sim.manager.restore_in_flight(store.load_in_flight(claims.fence().in_flight_scope()).unwrap());
    let switch = SwitchLink(Arc::new(std::sync::Mutex::new((None, 0))));
    if let Some(c) = &w.coordinator {
        switch.replace(quic_world_link(c.addr, w.cert.clone(), h, boot, claims.fence().claim_epoch()));
    }
    w.switches[h as usize] = switch.clone();
    sim.manager.set_world_link(Box::new(switch), boot, 0);
    sim.manager.set_world_map(land_map());
    let generation = server::claims::WorldGeneration::new(registered.generation);
    w.generations[h as usize] = generation.clone();
    sim.cuts = Some(server::cuts::CutWriter::with_world_cuts(claims.clone(), generation));
    let rc = seam_of(w, h);
    let (player, _) = store.resolve_player(&format!("hosts-test-keeper-{h}")).unwrap();
    let now = w.pair.now();
    sim.keep(player, rc, now);
    let i = w.pair.sims.iter().position(|s| s.ordinal > h).unwrap_or(w.pair.sims.len());
    w.pair.sims.insert(i, sim);
    w.links[h as usize] = Some(links);
    let old = std::mem::replace(&mut w.claims[h as usize], claims);
    old.store().clone().shutdown();
    registered
}

/// Host `h` exits (`cuts::exit_world`), the other hosts still running round by round
/// meanwhile; its outcome. Its peer listener and coordinator link go with it.
fn exit_host(w: &mut World, h: u32) -> server::claims::ServerExit {
    exit_host_with(w, h, None, |_, _| {})
}

/// [`exit_host`] with a leave timeout (`None`: the default) and `each` run every round
/// with the exiting manager and the other hosts' claims.
fn exit_host_with(
    w: &mut World,
    h: u32,
    leave_timeout: Option<Duration>,
    mut each: impl FnMut(&WorldManager<InlineSpawner>, &[HostClaims]),
) -> server::claims::ServerExit {
    let i = w.pair.sims.iter().position(|s| s.ordinal == h).expect("the host is up");
    let mut sim = w.pair.sims.remove(i);
    let fenced = w.claims[h as usize].fenced().clone();
    let generation = w.generations[h as usize].clone();
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
    w.switches[h as usize].drop_link();
    w.links[h as usize] = None;
    exit
}

/// Rounds until host `h` saw a newer world generation (a peer's hello, or a `BeginCut`):
/// its rollback flag is set.
fn until_rolled_back(w: &mut World, h: u32) {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(g) = sim(w, h).manager.take_higher_generation() {
            w.generations[h as usize].note(g);
        }
        if w.generations[h as usize].is_set() {
            return;
        }
        assert!(Instant::now() < deadline, "host {h} never saw the newer world generation");
        w.pair.round();
    }
}

/// The running regions of every host up holding `id`, with their host.
fn holders_all(w: &mut World, id: EntityId) -> Vec<(u32, RegionCoords)> {
    let mut out = Vec::new();
    for sim in &mut w.pair.sims {
        let h = sim.ordinal;
        out.extend(sim.holders(id).into_iter().map(|rc| (h, rc)));
    }
    out
}

/// A slime spawned in the middle of `rc` (host `h`'s); its id.
fn spawn_slime_in(w: &mut World, h: u32, rc: RegionCoords, x: f32) -> EntityId {
    let mut id = None;
    sim(w, h).manager.spawner_mut().with_region(rc, |region| {
        let tick = region.current_tick();
        let pos = game::parry::math::Vector::new(x, game::plate_units(30) + 3.0, REGION_SIZE / 2.0);
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

/// The other host (two hosts).
fn other(h: u32) -> u32 {
    1 - h
}

/// After host `dead` came back dirty (its generation one higher): the other host sees the
/// newer generation, exits `WorldRolledBack` (once), and starts again; both run from the
/// last committed cut and stay up for 5 s with the world's generation unchanged since.
fn roll_back_and_restart_the_other(w: &mut World, db: &PgDb, dir: &std::path::Path, dead: u32, generation: u64) {
    let live = other(dead);
    until_rolled_back(w, live);
    assert_eq!(exit_host(w, live), server::claims::ServerExit::WorldRolledBack, "host {live} rolls back");
    let again = restart_host(w, db, dir, live, 3000 + u64::from(live));
    assert_eq!((again.generation, again.bumped), (generation, false), "host {live} joins the new generation");
    w.pair.until("both hosts attached again", |p| p.sims.len() == 2 && p.sims.iter().all(|s| s.manager.world_attached()));
    let up = Instant::now();
    while up.elapsed() < Duration::from_secs(5) {
        w.pair.round();
        for h in 0..HOSTS {
            assert_eq!(sim(w, h).manager.take_higher_generation(), None, "host {h} sees no newer generation");
            assert!(!w.generations[h as usize].is_set(), "host {h} stays up");
        }
    }
    assert_eq!(w.claims[0].store().world_generation().unwrap(), generation, "raised exactly once");
}

/// Crash table: host 1 dies after its part is staged and the coordinator holds every
/// `CutPrepared` (before the decision): the cut commits whole; host 1 reboots dirty (the
/// generation + 1), host 0 rolls back, and both restart from that cut.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_killed_after_prepare_cut_commits_and_world_restarts_from_it() {
    install_props();
    let Some(db) = PgDb::create("kprep") else { return };
    let dir = scratch("kprep");
    let mut w = world(&db, &dir);
    let generation = w.claims[0].store().world_generation().unwrap();
    // Host 1's part staged, and the coordinator holds every `CutPrepared` of it (or has
    // already recorded the commit: the race with the decision's write is the test's).
    let mut cut = None;
    let deadline = Instant::now() + WAIT;
    while cut.is_none() {
        assert!(Instant::now() < deadline, "no cut had every part staged");
        if let Some((c, "prepared")) = sim(&mut w, 1).manager.world_cut_state() {
            let progress = w.coordinator.as_ref().unwrap().thread.inspect(|c| c.world_cut_progress(), WAIT).flatten();
            let all_at_core = progress.is_some_and(|(r, prepared, _)| r == c && prepared == [0, 1]);
            if all_at_core || decision_of(&db, c) == Some(true) {
                cut = Some(c);
                break;
            }
        }
        w.pair.round();
    }
    let cut = cut.unwrap();
    kill_host(&mut w, 1);
    w.pair.until("the cut decided", |_| decision_of(&db, cut).is_some());
    assert_eq!(decision_of(&db, cut), Some(true), "every part was staged: committed");
    let (_, tick) = last_committed(&db);
    w.pair.until("host 0 settled it", |p| p.sims[0].cuts.as_ref().unwrap().world_part().is_none());
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    let again = restart_host(&mut w, &db, &dir, 1, 3001);
    assert_eq!((again.generation, again.bumped), (generation + 1, true), "host 1 came back dirty");
    roll_back_and_restart_the_other(&mut w, &db, &dir, 1, generation + 1);
    let (a, b) = w.seam;
    let (last, _) = last_committed(&db);
    if last as u64 == cut.0 {
        for rc in [a, b] {
            assert_eq!(saved_tick(&db, rc), tick, "{rc:?} is as of the cut");
        }
    }
    w.until_committed_after(&db, last);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Crash table: host 1 dies before its part is staged (host 0's may be): the cut is
/// aborted (host 1's detach), nothing stays prepared; host 1 reboots dirty and every host
/// rolls back to the previous cut.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_killed_before_prepare_cut_is_absent_and_the_world_rolls_back() {
    install_props();
    let Some(db) = PgDb::create("kpend") else { return };
    let dir = scratch("kpend");
    let mut w = world(&db, &dir);
    let generation = w.claims[0].store().world_generation().unwrap();
    w.until_committed_after(&db, 0);
    let cut = until_part(&mut w, 1, &["pending", "marked"]);
    kill_host(&mut w, 1);
    w.pair.until("the cut decided", |_| decision_of(&db, cut).is_some());
    assert_eq!(decision_of(&db, cut), Some(false), "a part never staged: aborted");
    w.pair.until("host 0 settled it", |p| p.sims[0].cuts.as_ref().unwrap().world_part().is_none());
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    let (last, tick) = last_committed(&db);
    let again = restart_host(&mut w, &db, &dir, 1, 3001);
    assert_eq!((again.generation, again.bumped), (generation + 1, true), "host 1 came back dirty");
    roll_back_and_restart_the_other(&mut w, &db, &dir, 1, generation + 1);
    if last_committed(&db).0 == last {
        let (a, b) = w.seam;
        for rc in [a, b] {
            assert_eq!(saved_tick(&db, rc), tick, "{rc:?} is as of the previous cut");
        }
    }
    w.until_committed_after(&db, last);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Crash table: the coordinator dies with every part staged and no decision stored (its
/// decision writes fail): its restart resolves the staged parts first — an abort is
/// stored, nothing stays prepared — and the hosts, re-attached, hear `CutDone(false)`,
/// keep everything uncommitted and commit the next cut.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn coordinator_killed_before_the_decision_cut_is_absent() {
    install_props();
    let Some(db) = PgDb::create("kcdec") else { return };
    let dir = scratch("kcdec");
    let mut w = world(&db, &dir);
    w.store.fail_next_decisions(u32::MAX);
    let cut = until_part(&mut w, 0, &["prepared"]);
    w.pair.until("host 1 staged its part too", |p| matches!(p.sims[1].manager.world_cut_state(), Some((c, "prepared")) if c == cut));
    assert_eq!(decision_of(&db, cut), None, "undecided");
    assert_eq!(prepared(&db), 2, "both parts staged");
    kill_coordinator(&mut w);
    let store = db.open();
    server::roles::recover_staged_cuts(&store, HOSTS).expect("the staged parts are resolved");
    assert_eq!(decision_of(&db, cut), Some(false), "the restart aborted the undecided cut");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    restart_coordinator_listening(&mut w, store);
    w.pair.until("both hosts heard the outcome and settled", |p| {
        p.sims.iter().all(|s| s.manager.world_attached() && s.manager.world_cut() != Some(cut) && s.cuts.as_ref().unwrap().world_part().is_none())
    });
    w.until_committed_after(&db, cut.0 as i64);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Crash table: the coordinator records a commit and dies after the first `COMMIT
/// PREPARED` (`stop_after_commit_prepared(1)`): one part is still prepared. Its restart
/// commits the rest before it does anything else — nothing stays prepared and both
/// hosts' seam regions carry the cut's tick — and the hosts settle it as committed.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn coordinator_killed_between_commit_prepareds_cut_is_whole() {
    install_props();
    let Some(db) = PgDb::create("kccom") else { return };
    let dir = scratch("kccom");
    let mut w = world(&db, &dir);
    w.store.stop_after_commit_prepared(1);
    // The failed decision is never retried (the coordinator is to die right after it).
    w.coordinator.as_ref().unwrap().thread.hold_decision_retries();
    // Both parts staged: no host round from here on (a host's settling would finish the
    // rest itself); the coordinator decides on its own thread.
    let c = until_part(&mut w, 0, &["prepared"]);
    w.pair.until("host 1 staged its part too", |p| matches!(p.sims[1].manager.world_cut_state(), Some((k, "prepared")) if k == c));
    let deadline = Instant::now() + WAIT;
    let (cut, tick) = loop {
        assert!(Instant::now() < deadline, "no commit was recorded");
        if let Some((c, t, _)) = decisions(&db).into_iter().find(|(_, _, k)| *k) {
            break (c, t);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(cut as u64, c.0, "the staged cut committed");
    // The decision's write failed after one COMMIT PREPARED; nothing finishes the other.
    let deadline = Instant::now() + Duration::from_secs(2);
    while prepared(&db) != 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    kill_coordinator(&mut w);
    assert_eq!(prepared(&db), 1, "the coordinator died with one part still prepared");
    let store = db.open();
    server::roles::recover_staged_cuts(&store, HOSTS).expect("the staged parts are resolved");
    assert_eq!(prepared(&db), 0, "the restart committed the rest");
    let (a, b) = w.seam;
    for rc in [a, b] {
        assert!(saved_tick(&db, rc) >= tick, "{rc:?} carries the cut's tick");
    }
    restart_coordinator_listening(&mut w, store);
    w.pair.until("both hosts settled it", |p| {
        p.sims.iter().all(|s| s.manager.world_cut().map_or(true, |c| c.0 as i64 != cut) && s.cuts.as_ref().unwrap().world_part().is_none())
    });
    w.until_committed_after(&db, cut);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Crash table: host 1 dies between cuts (nothing staged) with a slime it took over since
/// the last cut, and restarts: the generation is raised exactly once, host 0 exits
/// `WorldRolledBack` once and restarts, both stay up, and the slime is in the world
/// exactly once (where the last cut has it).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn dirty_restart_rolls_every_host_back_once() {
    install_props();
    let Some(db) = PgDb::create("kdirty") else { return };
    let dir = scratch("kdirty");
    let mut w = world(&db, &dir);
    let generation = w.claims[0].store().world_generation().unwrap();
    let a0 = seam_of(&w, 0);
    let id = spawn_slime_in(&mut w, 0, a0, REGION_SIZE / 2.0);
    // A cut with the slime in host 0's region.
    w.until_committed_after(&db, 0);
    let (first, _) = last_committed(&db);
    w.until_committed_after(&db, first);
    // Over the seam to host 1, then host 1 dies between cuts.
    let b1 = seam_of(&w, 1);
    let past = if b1.x > a0.x { REGION_SIZE + game::FLIP_HYSTERESIS + 2.0 } else { -game::FLIP_HYSTERESIS - 2.0 };
    let along_x = b1.x != a0.x;
    sim(&mut w, 0).manager.spawner_mut().with_region(a0, |region| {
        let key = region.data().id_index[&id];
        let y = game::plate_units(30) + 3.0;
        let at = if along_x {
            game::rapier3d::math::Vector::new(past, y, REGION_SIZE / 2.0)
        } else {
            game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, y, past)
        };
        region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
    });
    w.pair.until("the slime on host 1, no cut pending", |p| {
        let on_1 = p.sims[1].holders(id) == [b1];
        on_1 && p.sims.iter().all(|s| s.manager.world_cut().is_none() && s.manager.peer_unreceived() == 0)
    });
    kill_host(&mut w, 1);
    let again = restart_host(&mut w, &db, &dir, 1, 3001);
    assert_eq!((again.generation, again.bumped), (generation + 1, true), "host 1 came back dirty: + 1");
    roll_back_and_restart_the_other(&mut w, &db, &dir, 1, generation + 1);
    w.pair.until("the slime back in a running region", |p| {
        p.sims.iter_mut().map(|s| s.holders(id).len()).sum::<usize>() > 0
    });
    for _ in 0..40 {
        w.pair.round();
    }
    assert_eq!(holders_all(&mut w, id).len(), 1, "the slime once: {:?}", holders_all(&mut w, id));
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Host 1 leaves cleanly (`exit_world` with several hosts: a leave cut): it exits 0 with
/// its row clean. A slime sent toward it meanwhile waits in host 0's session and is host
/// 0's row in its next cut. Host 1 returns: the generation is unchanged, and the slime
/// arrives there once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn clean_leave_and_return_keeps_the_generation_and_delivers_waiting_transfers() {
    install_props();
    let Some(db) = PgDb::create("leave") else { return };
    let dir = scratch("leave");
    let mut w = world(&db, &dir);
    let generation = w.claims[0].store().world_generation().unwrap();
    w.until_committed_after(&db, 0);
    assert_eq!(exit_host(&mut w, 1), server::claims::ServerExit::Clean, "host 1 left");
    let clean: Vec<i64> = query_i64s(&db, "SELECT clean FROM hosts WHERE ordinal = 1");
    assert_eq!(clean, [1], "host 1's row is clean");
    let (left_in, _) = last_committed(&db);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    // A slime toward host 1's region: it waits on host 0, a row of host 0's next cut.
    let (a0, b1) = (seam_of(&w, 0), seam_of(&w, 1));
    let past = if b1.x != a0.x {
        if b1.x > a0.x { REGION_SIZE + game::FLIP_HYSTERESIS + 2.0 } else { -game::FLIP_HYSTERESIS - 2.0 }
    } else {
        REGION_SIZE / 2.0
    };
    let id = spawn_slime_in(&mut w, 0, a0, REGION_SIZE / 2.0);
    sim(&mut w, 0).manager.spawner_mut().with_region(a0, |region| {
        let key = region.data().id_index[&id];
        let y = game::plate_units(30) + 3.0;
        let at = if b1.x != a0.x {
            game::rapier3d::math::Vector::new(past, y, REGION_SIZE / 2.0)
        } else {
            let z = if b1.z > a0.z { REGION_SIZE + game::FLIP_HYSTERESIS + 2.0 } else { -game::FLIP_HYSTERESIS - 2.0 };
            game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, y, z)
        };
        region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
    });
    w.pair.until("the slime waits in host 0's session", |p| p.sims[0].manager.peer_unreceived_ids().iter().any(|(_, i, _)| *i == id));
    w.until_committed_after(&db, left_in);
    let rows = query_i64s(
        &db,
        &format!("SELECT COUNT(*) FROM in_flight WHERE host = 0 AND kind = 0 AND target_rx = {} AND target_rz = {}", b1.x, b1.z),
    );
    assert_eq!(rows, [1], "host 0's cut holds the waiting slime as a row");
    // Host 1 returns.
    let again = restart_host(&mut w, &db, &dir, 1, 3001);
    assert_eq!((again.generation, again.bumped), (generation, false), "a clean return keeps the generation");
    w.pair.until("the slime arrived on host 1", |p| {
        p.sims.iter().all(|s| s.manager.peer_unreceived() == 0) && p.sims[1].holders(id) == [b1]
    });
    for _ in 0..40 {
        w.pair.round();
    }
    // It arrived on host 1 (above); slimes wander, so only once is asserted now.
    assert_eq!(holders_all(&mut w, id).len(), 1, "once: {:?}", holders_all(&mut w, id));
    for h in 0..HOSTS {
        assert!(!w.generations[h as usize].is_set(), "no rollback");
    }
    w.until_committed_after(&db, last_committed(&db).0);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Final review I1: host 1 returns from a clean leave while a cut of host 0 alone is
/// pending (begun, host 0's part not staged yet): that cut is aborted — by the coordinator
/// on host 1's attach, or by the store's decision (host 1's row is dirty and unlisted) —
/// the next cut takes both and commits, and a slime of host 0's is held and stored once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_returning_beside_a_cut_that_excludes_it_aborts_that_cut() {
    install_props();
    let Some(db) = PgDb::create("lback") else { return };
    let dir = scratch("lback");
    let mut w = world(&db, &dir);
    let generation = w.claims[0].store().world_generation().unwrap();
    w.until_committed_after(&db, 0);
    assert_eq!(exit_host(&mut w, 1), server::claims::ServerExit::Clean, "host 1 left");
    assert_eq!(query_i64s(&db, "SELECT clean FROM hosts WHERE ordinal = 1"), [1]);
    let a0 = seam_of(&w, 0);
    let id = spawn_slime_in(&mut w, 0, a0, REGION_SIZE / 2.0);
    // A cut of host 0 alone begins; nothing of host 0 runs from here until host 1 is back.
    let cut = until_part(&mut w, 0, &["pending", "marked"]);
    assert_eq!(decision_of(&db, cut), None);
    let again = restart_host(&mut w, &db, &dir, 1, 3001);
    assert_eq!((again.generation, again.bumped), (generation, false), "a clean return");
    w.pair.until("the cut that excludes host 1 is decided", |_| decision_of(&db, cut).is_some());
    assert_eq!(decision_of(&db, cut), Some(false), "the cut of host 0 alone is aborted");
    w.until_committed_after(&db, cut.0 as i64);
    assert_eq!(holders_all(&mut w, id).len(), 1, "held once: {:?}", holders_all(&mut w, id));
    let (birth, serial) = (id.birth_region, id.serial);
    let stored = query_i64s(
        &db,
        &format!("SELECT COUNT(*) FROM entities WHERE birth_rx = {} AND birth_rz = {} AND serial = {serial}", birth.x, birth.z),
    );
    assert_eq!(stored, [1], "stored once");
    for h in 0..HOSTS {
        assert!(!w.generations[h as usize].is_set(), "no rollback");
    }
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Final review I3: a multi-host stack killed with a part staged, then the same Postgres
/// world started as role `all`: its start-up resolves the orphan part before the claim,
/// which then proceeds (it would wait on the part's `super_regions` locks for ever).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn role_all_on_postgres_resolves_an_orphan_part_before_its_claim() {
    install_props();
    let Some(db) = PgDb::create("allorph") else { return };
    let (store, claims) = claim(&db, 0);
    let generation = store.world_generation().unwrap();
    let srs: Vec<_> = claims.fence().epochs.keys().copied().collect();
    let orphan = game::WorldCut { id: game::CutId(9), host: 0, tick: 1, regions: vec![], in_flight: vec![], players: vec![], history: None };
    store.prepare_cut(orphan, claims.fence().clone(), generation).recv().unwrap().expect("staged");
    // The host process dies: the part stays prepared.
    drop(claims);
    store.shutdown();
    assert_eq!(prepared(&db), 1);
    // Role `all` on the same world.
    let all = db.open();
    server::roles::resolve_before_claim(true, &all, 1).expect("the start-up resolves staged parts");
    let (tx, rx) = crossbeam::channel::bounded(1);
    let claimer = {
        let all = all.clone();
        std::thread::spawn(move || {
            let _ = tx.send(all.claim_super_regions(0, srs));
        })
    };
    let claimed = rx.recv_timeout(Duration::from_secs(10));
    if claimed.is_err() {
        // Let the stuck claim finish before failing: nothing stays prepared.
        db.open().resolve_prepared().unwrap();
    }
    claimer.join().unwrap();
    claimed.expect("the claim proceeds").expect("claimed");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    all.shutdown();
}

/// A host of several exiting with no coordinator to cut for it waits its leave timeout
/// and exits 0 without a cut: its row stays dirty, so its return raises the generation.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn leave_without_a_coordinator_exits_dirty() {
    install_props();
    let Some(db) = PgDb::create("nocoord") else { return };
    let dir = scratch("nocoord");
    let (mut pair, _seam, mut links, claims) = two_hosts(&db, &dir);
    let generation = claims[1].store().world_generation().unwrap();
    let g1 = server::claims::WorldGeneration::new(links[1].registered().generation);
    let mut cuts = server::cuts::CutWriter::with_world_cuts(claims[1].clone(), g1.clone());
    cuts.set_leave_timeout(Duration::from_secs(2));
    let mut host1 = pair.sims.remove(1);
    let region_out = host1.region_out.clone();
    let started = pair.started;
    let fenced = claims[1].fenced().clone();
    let begun = Instant::now();
    let exit = server::cuts::exit_world(
        &mut host1.manager,
        server::cuts::ExitParams {
            region_out: &region_out,
            now_ms: || started.elapsed().as_millis() as u64,
            cuts: Some(cuts),
            coordinator: None,
            fenced: &fenced,
            world_generation: &g1,
            park_timeout: server::cuts::EXIT_PARK_TIMEOUT,
            save_timeout: Duration::from_secs(10),
            now_tick: || 0,
        },
        |m| {
            m.spawner_mut().tick_all();
            m.spawner_mut().pump();
            pair.round();
        },
    );
    assert_eq!(exit, server::claims::ServerExit::Clean, "exits 0");
    assert!(begun.elapsed() >= Duration::from_secs(2), "after its leave timeout");
    assert_eq!(query_i64s(&db, "SELECT clean FROM hosts WHERE ordinal = 1"), [0], "its row stays dirty");
    drop(host1);
    links.remove(1);
    // Its return raises the generation.
    let (_, c1) = claim(&db, 1);
    let back = start_links(&c1, 1, 4001, TOKEN, &dir, "127.0.0.1:0");
    assert_eq!((back.registered().generation, back.registered().bumped), (generation + 1, true));
    drop(back);
    drop(pair);
    drop(links);
    c1.store().clone().shutdown();
    for c in claims {
        c.store().clone().shutdown();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Host 1 leaves; host 0's store fails to stage its part of the leave cut (one write):
/// `exit_world` sees host 1's part settled aborted, asks again, and the second leave cut
/// commits — host 1 exits clean with its row clean.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn aborted_leave_cut_is_asked_for_again_and_commits() {
    install_props();
    let Some(db) = PgDb::create("lretry") else { return };
    let dir = scratch("lretry");
    let mut w = world(&db, &dir);
    w.until_committed_after(&db, 0);
    let mut first = None;
    let exit = exit_host_with(&mut w, 1, None, |m, claims| {
        if first.is_none() {
            if let Some(c) = m.leaving_cut() {
                first = Some(c);
                claims[0].store().fail_next_writes(1);
            }
        }
    });
    assert_eq!(exit, server::claims::ServerExit::Clean, "host 1 left");
    let first = first.expect("a leave cut began");
    assert_eq!(decision_of(&db, first), Some(false), "the first leave cut was aborted");
    assert!(decisions(&db).iter().any(|(c, _, k)| *k && *c as u64 > first.0), "a later leave cut committed");
    assert_eq!(query_i64s(&db, "SELECT clean FROM hosts WHERE ordinal = 1"), [1], "host 1's row is clean");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// The leave timeout of [`aborted_leave_cut_past_the_leave_timeout_exits_dirty`]: long
/// enough for its first leave cut to begin within it.
const PAST_LEAVE_TIMEOUT: Duration = Duration::from_secs(3);

/// The same failed leave cut with a leave timeout already past when it is aborted: host 1
/// exits without a cut, its row dirty. Deterministic in its timing: once the first leave
/// cut began (within the timeout), the exiting host's loop is held in its idle step until
/// the timeout has passed since its leave request — nothing of either host runs meanwhile —
/// so that cut's abort can only settle after it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn aborted_leave_cut_past_the_leave_timeout_exits_dirty() {
    install_props();
    let Some(db) = PgDb::create("lpast") else { return };
    let dir = scratch("lpast");
    let mut w = world(&db, &dir);
    w.until_committed_after(&db, 0);
    let mut first = None;
    // The first idle step runs right after the leave request: no earlier than it.
    let mut asked: Option<Instant> = None;
    let exit = exit_host_with(&mut w, 1, Some(PAST_LEAVE_TIMEOUT), |m, claims| {
        let asked = *asked.get_or_insert_with(Instant::now);
        if first.is_none() {
            if let Some(c) = m.leaving_cut() {
                assert!(asked.elapsed() < PAST_LEAVE_TIMEOUT, "the first leave cut began within the timeout");
                first = Some(c);
                claims[0].store().fail_next_writes(1);
                std::thread::sleep((asked + PAST_LEAVE_TIMEOUT).saturating_duration_since(Instant::now()));
            }
        }
    });
    assert_eq!(exit, server::claims::ServerExit::Clean, "exits 0");
    let first = first.expect("a leave cut began within the timeout");
    assert_eq!(decision_of(&db, first), Some(false), "the leave cut was aborted");
    assert!(!decisions(&db).iter().any(|(c, _, k)| *k && *c as u64 > first.0), "no later cut");
    assert_eq!(query_i64s(&db, "SELECT clean FROM hosts WHERE ordinal = 1"), [0], "its row stays dirty");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// The bound on both hosts' clean leave when they are stopped together: well inside the
/// supervisor's 95 s deadline (the leave batch, a cut's lead and its decision take ~3 s).
const BOTH_LEAVE_WITHIN: Duration = Duration::from_secs(15);

/// Slime `id` in host `h`'s seam region, put just past the seam toward the other host's.
fn push_over_the_seam(w: &mut World, h: u32, id: EntityId) {
    let (a, _) = w.seam;
    let from = seam_of(w, h);
    // The seam is `b = a + x`: from `a` over +x, from `b` over -x.
    let x = if from == a { REGION_SIZE + game::FLIP_HYSTERESIS + 2.0 } else { -game::FLIP_HYSTERESIS - 2.0 };
    sim(w, h).manager.spawner_mut().with_region(from, |region| {
        let key = region.data().id_index[&id];
        let at = game::rapier3d::math::Vector::new(x, game::plate_units(30) + 3.0, REGION_SIZE / 2.0);
        region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
    });
}

/// What one host's `exit_world` came to: its outcome, how long it took from the shared
/// stop signal, and the leave cut that named it (`leaving_cut`).
struct Exited {
    exit: server::claims::ServerExit,
    took: Duration,
    leaving_cut: Option<game::CutId>,
}

/// Every host gets the stop signal at the same moment (as the dev stack's supervisor
/// TERMs them all): each runs the real `exit_world` on its own thread, its regions ticking
/// at its own pace, the coordinator reading the highest world tick. Their outcomes, by
/// ordinal; their peer listeners and coordinator links go with them.
fn exit_every_host_at_once(w: &mut World) -> Vec<Exited> {
    let mut sims: Vec<Sim> = std::mem::take(&mut w.pair.sims);
    sims.sort_by_key(|s| s.ordinal);
    let started = w.pair.started;
    let tick = w.pair.tick.clone();
    let stop = std::sync::Barrier::new(sims.len());
    let params: Vec<(server::claims::Fenced, server::claims::WorldGeneration)> =
        sims.iter().map(|s| (w.claims[s.ordinal as usize].fenced().clone(), w.generations[s.ordinal as usize].clone())).collect();
    let exited = std::thread::scope(|scope| {
        let handles: Vec<_> = sims
            .iter_mut()
            .zip(&params)
            .map(|(sim, (fenced, generation))| {
                let (tick, stop) = (tick.clone(), &stop);
                scope.spawn(move || {
                    let region_out = sim.region_out.clone();
                    let cuts = sim.cuts.take();
                    stop.wait();
                    let at = Instant::now();
                    let exit = server::cuts::exit_world(
                        &mut sim.manager,
                        server::cuts::ExitParams {
                            region_out: &region_out,
                            now_ms: || started.elapsed().as_millis() as u64,
                            cuts,
                            coordinator: None,
                            fenced,
                            world_generation: generation,
                            park_timeout: server::cuts::EXIT_PARK_TIMEOUT,
                            save_timeout: Duration::from_secs(10),
                            now_tick: || 0,
                        },
                        |m| {
                            m.spawner_mut().tick_all();
                            m.spawner_mut().pump();
                            tick.fetch_max(m.spawner_mut().world_tick(), std::sync::atomic::Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(5));
                        },
                    );
                    Exited { exit, took: at.elapsed(), leaving_cut: sim.manager.leaving_cut() }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("an exit thread panicked")).collect::<Vec<_>>()
    });
    for sim in sims {
        let h = sim.ordinal;
        drop(sim);
        w.switches[h as usize].drop_link();
        w.links[h as usize] = None;
    }
    exited
}

/// Task 14b: both hosts are stopped at the same moment with the coordinator up (the dev
/// stack's shutdown), slimes crossing the seam both ways as they go. Their leave requests
/// fall in one batch, so the design yields ONE leave cut naming both leaving (there is no
/// stayer): both exit clean well inside the supervisor's deadline, that cut is committed,
/// both `hosts` rows are clean and nothing stays prepared. Restarting both keeps the world
/// generation (no rollback), and each slime is in the world exactly once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn both_hosts_stopped_together_leave_cleanly() {
    install_props();
    let Some(db) = PgDb::create("bothleave") else { return };
    let dir = scratch("bothleave");
    let mut w = world(&db, &dir);
    let generation = w.claims[0].store().world_generation().unwrap();
    w.until_committed_after(&db, 0);
    let (before, _) = last_committed(&db);
    let slimes: Vec<EntityId> = (0..HOSTS)
        .map(|h| {
            let rc = seam_of(&w, h);
            spawn_slime_in(&mut w, h, rc, REGION_SIZE / 2.0)
        })
        .collect();
    w.pair.round();
    for h in 0..HOSTS {
        push_over_the_seam(&mut w, h, slimes[h as usize]);
    }
    let exited = exit_every_host_at_once(&mut w);
    for (h, e) in exited.iter().enumerate() {
        assert_eq!(e.exit, server::claims::ServerExit::Clean, "host {h} exits 0");
        assert!(e.took < BOTH_LEAVE_WITHIN, "host {h} left in {:?}", e.took);
    }
    let cut = exited[0].leaving_cut.expect("a leave cut named host 0");
    assert_eq!(exited[1].leaving_cut, Some(cut), "one leave cut named both hosts leaving");
    assert_eq!(decision_of(&db, cut), Some(true), "it committed");
    // A periodic cut running at the stop signal may commit before it; none after it, and
    // no leave cut was aborted (asked for again).
    let since: Vec<(i64, i64, bool)> = decisions(&db).into_iter().filter(|(c, _, _)| *c > before).collect();
    assert!(since.iter().all(|(_, _, k)| *k), "no cut aborted since the stop: {since:?}");
    assert_eq!(since.last().map(|(c, _, _)| *c as u64), Some(cut.0), "the leave cut is the last decided: {since:?}");
    assert_eq!(query_i64s(&db, "SELECT clean FROM hosts ORDER BY ordinal"), [1, 1], "both rows are clean");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    // Both return: no generation bump, no rollback, each slime once.
    for h in 0..HOSTS {
        let again = restart_host(&mut w, &db, &dir, h, 3000 + u64::from(h));
        assert_eq!((again.generation, again.bumped), (generation, false), "host {h}'s clean return keeps the generation");
    }
    w.pair.until("both hosts attached again", |p| p.sims.len() == 2 && p.sims.iter().all(|s| s.manager.world_attached()));
    w.pair.until("every slime back in a running region, nothing owed", |p| {
        p.sims.iter().all(|s| s.manager.peer_unreceived() == 0)
            && slimes.iter().all(|id| p.sims.iter_mut().map(|s| s.holders(*id).len()).sum::<usize>() > 0)
    });
    for _ in 0..40 {
        w.pair.round();
    }
    for id in &slimes {
        assert_eq!(holders_all(&mut w, *id).len(), 1, "{id:?} once: {:?}", holders_all(&mut w, *id));
    }
    for h in 0..HOSTS {
        assert!(!w.generations[h as usize].is_set(), "host {h}: no rollback");
        assert_eq!(sim(&mut w, h).manager.take_higher_generation(), None, "host {h} sees no newer generation");
    }
    assert_eq!(w.claims[0].store().world_generation().unwrap(), generation, "the generation is unchanged");
    w.until_committed_after(&db, cut.0 as i64);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Task 14b finding (fixed by Task 12b): a slime stored in host 0's seam region by one
/// committed cut crosses into host 1's region before the next. Host 0's part deletes its
/// region's `entities` rows and host 1's part inserts into its own region's; with one key
/// per entity world-wide that was two prepared transactions on one row — host 1 waited for
/// host 0's prepared part until its lock timeout (or hit a unique violation if it went
/// first), the cut aborted, and so did every later one: world saving stalled for good.
/// Rows are keyed per region now: the later cut commits, the slime stored once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn entity_crossing_hosts_between_cuts_does_not_stall_saving() {
    install_props();
    let Some(db) = PgDb::create("xcross") else { return };
    let dir = scratch("xcross");
    let mut w = world(&db, &dir);
    let a0 = seam_of(&w, 0);
    let id = spawn_slime_in(&mut w, 0, a0, REGION_SIZE / 2.0);
    w.until_committed_after(&db, 0);
    let (first, _) = last_committed(&db);
    w.until_committed_after(&db, first);
    let (stored, _) = last_committed(&db);
    let b1 = seam_of(&w, 1);
    push_over_the_seam(&mut w, 0, id);
    w.pair.until("the slime on host 1", |p| p.sims[1].holders(id) == [b1]);
    w.until_committed_after(&db, stored);
    let rows = |column: &str| {
        format!(
            "SELECT {column} FROM entities WHERE birth_rx = {} AND birth_rz = {} AND serial = {}",
            id.birth_region.x, id.birth_region.z, id.serial as i64
        )
    };
    let stored_in: Vec<(i64, i64)> = query_i64s(&db, &rows("rx")).into_iter().zip(query_i64s(&db, &rows("rz"))).collect();
    assert_eq!(stored_in, [(i64::from(b1.x), i64::from(b1.z))], "the slime is stored once, in host 1's region");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

// ---- the login relay (Task 15) ------------------------------------------------------------

/// The login service's end of the entry host's login link.
struct Login {
    tx: crossbeam::channel::Sender<LoginToHost>,
    events: Receiver<svc_link::LinkEvent<HostToLogin>>,
    /// Everything the host sent so far, in order.
    seen: Vec<HostToLogin>,
}

impl Login {
    /// What arrived since the last look, appended to `seen`.
    fn drain(&mut self) {
        for ev in self.events.try_iter() {
            if let svc_link::LinkEvent::Msg(msg) = ev {
                self.seen.push(msg);
            }
        }
    }
}

/// The players by token, with their stored records (a test's player store).
type Roster = Arc<std::sync::Mutex<std::collections::BTreeMap<String, (PlayerId, Option<PlayerRecord>)>>>;

/// A player store over a [`Roster`]: an unknown token is an error.
struct RosterPlayers(Roster);

impl PlayerStore for RosterPlayers {
    fn resolve(&mut self, key: &PlayerKey) -> std::io::Result<(PlayerId, Option<PlayerRecord>)> {
        let PlayerKey::Online(token) = key else { return Err(std::io::Error::other("not a login")) };
        self.0.lock().unwrap().get(&token.0).cloned().ok_or_else(|| std::io::Error::other("an unknown token"))
    }
}

/// Two hosts ([`two_hosts`]) whose login relays run (host 0 the entry host, its login link
/// listening, the login service dialed in), over the players of `roster`.
struct LoginWorld {
    pair: Pair,
    seam: (RegionCoords, RegionCoords),
    links: Vec<PeerLinks>,
    claims: Vec<HostClaims>,
    link: Option<LoginLink>,
    login: Login,
    roster: Roster,
}

/// The entry host of the login tests.
const ENTRY: u32 = 0;

fn login_hosts(db: &PgDb, dir: &std::path::Path) -> LoginWorld {
    let (mut pair, seam, links, claims) = two_hosts(db, dir);
    let roster: Roster = Arc::default();
    let players = server::player_store::shared(RosterPlayers(roster.clone()));
    let mut link = None;
    for sim in &mut pair.sims {
        let (events_tx, events) = unbounded();
        let orders = Arc::new(SpawnOrders::new(server::auth_edge::system_clock()));
        let relay = LoginRelay::new(orders.clone(), players.clone(), events_tx.clone());
        sim.manager.set_entry_host(ENTRY);
        if sim.ordinal == ENTRY {
            let pin = dir.join("host-link.cert-sha256");
            link = Some(
                server::login_link::start_relaying(
                    "127.0.0.1:0".parse().unwrap(),
                    TOKEN.into(),
                    Some(pin),
                    orders.clone(),
                    players.clone(),
                    events_tx,
                    ENTRY,
                )
                .expect("the login link listens"),
            );
        }
        sim.login = Some(SimLogin { orders, relay, events });
    }
    let l: &LoginLink = link.as_ref().unwrap();
    let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Login);
    let (tx, events) = svc_link::LinkDialer::connect::<HostToLogin, LoginToHost>(l.local_addr(), l.cert_sha256().to_string(), hello);
    match events.recv_timeout(WAIT) {
        Ok(svc_link::LinkEvent::Up(_)) => {}
        other => panic!("the login link did not come up: {other:?}"),
    }
    let login = Login { tx, events, seen: Vec::new() };
    // Each host's first sync of the other is done (its `Running` came).
    pair.until("every peer synced", |p| p.sims.iter().all(|s| s.manager.peers_synced()));
    LoginWorld { pair, seam, links, claims, link, login, roster }
}

impl LoginWorld {
    fn stop(mut self, dir: &std::path::Path) {
        drop(self.link.take());
        drop(self.pair);
        drop(self.links);
        for c in self.claims {
            c.store().clone().shutdown();
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A login token of `player` whose stored record stands in the middle of `rc`
    /// (`None`: no record).
    fn player(&self, n: u8, player: PlayerId, rc: Option<RegionCoords>) -> String {
        let token = game::PlayerToken::generate([n; 16]).0;
        let record = rc.map(|rc| PlayerRecord {
            client: game::Client::default(),
            pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(
                REGION_SIZE / 2.0,
                game::plate_units(30) + 3.0,
                REGION_SIZE / 2.0,
            )),
            region: rc,
            fortress: None,
            adventure: None,
        });
        self.roster.lock().unwrap().insert(token.clone(), (player, record));
        token
    }

    fn sim(&mut self, h: u32) -> &mut Sim {
        self.pair.sims.iter_mut().find(|s| s.ordinal == h).expect("the host is up")
    }

    fn orders(&mut self, h: u32) -> Arc<SpawnOrders> {
        self.sim(h).login.as_ref().unwrap().orders.clone()
    }

    /// Rounds until the login service has received a message `want` holds of (counting
    /// from `from` in `seen`); its index.
    fn until_login(&mut self, what: &str, from: usize, want: impl Fn(&HostToLogin) -> bool) -> usize {
        let deadline = Instant::now() + WAIT;
        loop {
            self.login.drain();
            if let Some(i) = self.login.seen.iter().skip(from).position(&want) {
                return from + i;
            }
            assert!(Instant::now() < deadline, "timed out waiting for the login service to hear: {what} (seen {:?})", self.login.seen);
            self.pair.round();
        }
    }

    /// `rounds` rounds, the login service listening.
    fn rounds(&mut self, rounds: usize) {
        for _ in 0..rounds {
            self.pair.round();
            self.login.drain();
        }
    }

    /// The login service orders a spawn of `token` at `epoch` and waits for the ack.
    fn join(&mut self, token: &str, epoch: u64) -> HostToLogin {
        let from = self.login.seen.len();
        self.login.tx.send(LoginToHost::SpawnPlayer { player_token: token.into(), session_epoch: epoch }).unwrap();
        let i = self.until_login("the spawn ack", from, |m| {
            matches!(m, HostToLogin::SpawnAck { player_token, session_epoch, .. } if player_token == token && *session_epoch == epoch)
        });
        self.login.seen[i].clone()
    }

    /// The client of the ticket (`token`, `epoch`) connects to host `h`, as its edge admits
    /// and announces it; its session id.
    fn connect(&mut self, h: u32, token: &str, epoch: u64) -> ClientId {
        let (player, record) = self.roster.lock().unwrap()[token].clone();
        let orders = self.orders(h);
        orders.admit(token, epoch).expect("the ticket is admitted");
        let client = game::session_client_id(player, epoch);
        let now = self.pair.now();
        let manager = &mut self.sim(h).manager;
        let ev = ServerEvent::LoginConnected { client, player, record, player_token: token.into(), session_epoch: epoch };
        orders.announce(token, epoch, || assert!(manager.handle_server_event(ev, now))).expect("announced");
        client
    }

    /// The client's connection to host `h` closes, as its edge reports it.
    fn disconnect(&mut self, h: u32, client: ClientId, token: &str, epoch: u64) {
        let now = self.pair.now();
        assert!(self.sim(h).manager.handle_server_event(ServerEvent::ClientDisconnected(client), now));
        self.orders(h).closed(token, epoch);
    }

    /// Where `client`'s body is: `(host, region)` of every running region holding it.
    fn bodies(&mut self, client: ClientId) -> Vec<(u32, RegionCoords)> {
        let mut out = Vec::new();
        for sim in &mut self.pair.sims {
            for rc in sim.manager.running_regions() {
                sim.manager.spawner_mut().with_region(rc, |region| {
                    if region.data().player_entites.contains_key(&client) {
                        out.push((sim.ordinal, rc));
                    }
                });
            }
        }
        out
    }

    /// Push `client`'s body (in `from`, on host `h`) past the seam into `to`.
    fn push_across(&mut self, h: u32, client: ClientId, from: RegionCoords, to: RegionCoords) {
        let y = game::plate_units(30) + 3.0;
        let past = |d: i32| match d {
            1 => REGION_SIZE + game::FLIP_HYSTERESIS + 2.0,
            -1 => -game::FLIP_HYSTERESIS - 2.0,
            _ => REGION_SIZE / 2.0,
        };
        let at = game::rapier3d::math::Vector::new(past(to.x - from.x), y, past(to.z - from.z));
        let mut pushed = false;
        self.sim(h).manager.spawner_mut().with_region(from, |region| {
            if let Some(&key) = region.data().player_entites.get(&client) {
                region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
                pushed = true;
            }
        });
        assert!(pushed, "{from:?} on host {h} holds client {client}'s body");
    }

    /// The SessionEnded messages for (`token`, `epoch`) the login service heard.
    fn ended(&self, token: &str, epoch: u64) -> usize {
        let want = HostToLogin::SessionEnded { player_token: token.into(), session_epoch: epoch };
        self.login.seen.iter().filter(|m| **m == want).count()
    }
}

/// `(the region host h owns, the other)` of the seam.
fn sides(seam: (RegionCoords, RegionCoords), h: u32, w: &LoginWorld) -> (RegionCoords, RegionCoords) {
    if w.pair.sims[0].manager.owner_of(seam.0) == h { (seam.0, seam.1) } else { (seam.1, seam.0) }
}

fn ack_quic(ack: &HostToLogin) -> (bool, Option<String>) {
    match ack {
        HostToLogin::SpawnAck { ok, quic, .. } => (*ok, quic.clone()),
        other => panic!("not an ack: {other:?}"),
    }
}

/// A login whose stored record lies in host 1's region: the entry host (0) relays the
/// order, host 1 takes it, and the ack names host 1's address. Host 0 holds no order.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn join_places_the_player_on_the_host_owning_its_record_region() {
    install_props();
    let Some(db) = PgDb::create("ljoin") else { return };
    let dir = scratch("ljoin");
    let mut w = login_hosts(&db, &dir);
    let (b1, _) = sides(w.seam, 1, &w);
    let token = w.player(1, PlayerId(900), Some(b1));
    let ack = w.join(&token, 1);
    assert_eq!(ack_quic(&ack), (true, endpoint(1).quic), "host 1 took it");
    assert!(w.orders(1).pending().by_token.contains_key(&token), "host 1 holds the pending spawn");
    assert!(w.orders(0).pending().by_token.is_empty(), "host 0 holds none");
    // The client connects there and plays there.
    let client = w.connect(1, &token, 1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(client).is_empty() {
        assert!(Instant::now() < deadline, "the player never got a body");
        w.rounds(1);
    }
    assert_eq!(w.bodies(client), [(1, b1)]);
    w.stop(&dir);
}

/// The player walked from host 0 to host 1 and its client disconnected; no cut since (its
/// stored record still in host 0's region). A new login follows host 0's `departed`
/// pointer to host 1, lands there, and the player has one body.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn rejoin_within_a_cut_follows_the_departed_pointer() {
    install_props();
    let Some(db) = PgDb::create("lrejoin") else { return };
    let dir = scratch("lrejoin");
    let mut w = login_hosts(&db, &dir);
    let (a0, b1) = sides(w.seam, 0, &w);
    let token = w.player(2, PlayerId(901), Some(a0));
    assert_eq!(ack_quic(&w.join(&token, 1)), (true, endpoint(0).quic));
    let c1 = w.connect(0, &token, 1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(c1).is_empty() {
        assert!(Instant::now() < deadline, "the player never got a body");
        w.rounds(1);
    }
    w.push_across(0, c1, a0, b1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(c1) != [(1, b1)] || w.pair.sims.iter().any(|s| s.manager.peer_unreceived() > 0) {
        assert!(Instant::now() < deadline, "the player never crossed: {:?}", w.bodies(c1));
        w.rounds(1);
    }
    assert_eq!(w.sim(0).manager.departed().get(&PlayerId(901)), Some(&1));
    w.disconnect(0, c1, &token, 1);
    let ack = w.join(&token, 2);
    assert_eq!(ack_quic(&ack), (true, endpoint(1).quic), "the login lands on host 1");
    let c2 = w.connect(1, &token, 2);
    let deadline = Instant::now() + WAIT;
    while w.bodies(c2).is_empty() {
        assert!(Instant::now() < deadline, "the new login never got a body");
        w.rounds(1);
    }
    w.rounds(20);
    assert_eq!(w.bodies(c2), [(1, b1)], "one body, on host 1");
    assert!(w.bodies(c1).is_empty(), "the old session's body is gone");
    for h in 0..HOSTS {
        assert!(w.sim(h).manager.parked_record(PlayerId(901)).is_none(), "no parked copy on host {h}");
    }
    w.stop(&dir);
}

/// A session on host 1 (not the entry host): the login service's kick, sent to host 0,
/// parks it there, and is acknowledged once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn kick_reaches_a_session_on_a_non_entry_host_and_is_acked_once() {
    install_props();
    let Some(db) = PgDb::create("lkick") else { return };
    let dir = scratch("lkick");
    let mut w = login_hosts(&db, &dir);
    let (b1, _) = sides(w.seam, 1, &w);
    let token = w.player(3, PlayerId(902), Some(b1));
    assert_eq!(ack_quic(&w.join(&token, 1)), (true, endpoint(1).quic));
    let client = w.connect(1, &token, 1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(client).is_empty() {
        assert!(Instant::now() < deadline, "the player never got a body");
        w.rounds(1);
    }
    let from = w.login.seen.len();
    w.login.tx.send(LoginToHost::Kick { player_token: token.clone(), session_epoch: 1, reason: "signed out".into() }).unwrap();
    let ack = HostToLogin::KickAck { player_token: token.clone(), session_epoch: 1 };
    w.until_login("the kick ack", from, |m| *m == ack);
    w.rounds(20);
    assert_eq!(w.login.seen[from..].iter().filter(|m| **m == ack).count(), 1, "acknowledged once");
    assert!(w.bodies(client).is_empty(), "the session's body is gone");
    assert!(w.sim(1).manager.parked_record(PlayerId(902)).is_some(), "parked on host 1");
    assert_eq!(w.orders(1).revoked(&token, 1).as_deref(), Some("signed out"), "host 1 revoked the login");
    assert_eq!(w.ended(&token, 1), 1, "the kicked session's end reached the login service once");
    w.stop(&dir);
}

/// The host holding a player's record region cannot be reached: the entry host refuses the
/// spawn order (the login service answers 503 `world_unavailable`).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn spawn_for_an_unreachable_owner_is_refused() {
    install_props();
    let Some(db) = PgDb::create("lunreach") else { return };
    let dir = scratch("lunreach");
    let mut w = login_hosts(&db, &dir);
    let (b1, _) = sides(w.seam, 1, &w);
    let token = w.player(4, PlayerId(903), Some(b1));
    // Host 1 is gone: its manager and its links.
    let i = w.pair.sims.iter().position(|s| s.ordinal == 1).unwrap();
    drop(w.pair.sims.remove(i));
    drop(w.links.remove(1));
    w.pair.until("host 0 no longer reaches host 1", |p| !p.sims[0].manager.reaches(b1));
    let ack = w.join(&token, 1);
    assert_eq!(ack_quic(&ack), (false, None), "refused");
    assert!(w.orders(0).pending().by_token.is_empty(), "nothing ordered on host 0");
    // The login service itself, over its own link to host 0: the join answers 503.
    let (status, code) = login_join(&mut w, &token);
    assert_eq!(status, 503);
    assert_eq!(code, login_api::ApiErrorCode::WorldUnavailable);
    assert!(w.orders(0).pending().by_token.is_empty(), "nothing ordered on host 0");
    w.stop(&dir);
}

/// A real login service (its HTTP API in-process, an in-memory store, its own link to the
/// entry host) joins the account that owns `token` to the world while the hosts run round
/// by round: the join's HTTP status and, for a failure, its error code.
fn login_join(w: &mut LoginWorld, token: &str) -> (u16, login_api::ApiErrorCode) {
    use tower::ServiceExt;
    const WORLD: &str = "hosts";
    const NOW: u64 = 1_800_000_000;
    let link = w.link.as_ref().unwrap();
    let (addr, pin) = (link.local_addr().to_string(), link.cert_sha256().to_string());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let token = token.to_string();
    let joined = rt.spawn(async move {
        let store = login::LoginStore::open("sqlite::memory:").await.unwrap();
        store
            .put_world(&login::WorldRow {
                world_id: WORLD.into(),
                name: "Hosts".into(),
                status: login_api::WorldStatus::Open,
                quic: Some("world-row.example:6466".into()),
                webtransport: None,
                cert_hash: None,
                created_at: 1,
                host_link: Some(addr),
                host_link_cert: Some(pin),
            })
            .await
            .unwrap();
        let account = store.ensure_verified_account("dwarf@example.org", 1).await.unwrap();
        assert_eq!(store.player_token_for(account, WORLD, 1, move || token.clone()).await.unwrap().len(), 32);
        let refresh = "hosts-test-refresh-token";
        let expires = (NOW + 3600) as i64;
        store.insert_refresh(login::api::token_hash(refresh), account, [7; 16], expires).await.unwrap();
        let clock: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(|| NOW);
        let (key, _) = auth::SigningKey::generate();
        let state = login::AppState::new(
            store.clone(),
            Arc::new(auth::Signer::new(vec![key]).ok().unwrap()),
            Arc::new(login::LoginConfig::from_lookup(|_| None).unwrap()),
            clock.clone(),
        )
        .with_host_links(login::host_link::HostLinks::new(Some(TOKEN.into()), store, clock));
        let body = serde_json::to_vec(&login_api::JoinRequest { refresh_token: refresh.into(), world: WORLD.into() }).unwrap();
        let req = axum::http::Request::post(login_api::PATH_JOIN)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body))
            .unwrap();
        let resp = login::router(state).oneshot(req).await.unwrap();
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let err: login_api::ApiError = serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{e}: {bytes:?}"));
        (status, err.code)
    });
    let deadline = Instant::now() + WAIT;
    while !joined.is_finished() {
        assert!(Instant::now() < deadline, "the join never answered");
        w.rounds(1);
    }
    let out = rt.block_on(joined).unwrap();
    drop(rt);
    out
}

/// A host that left cleanly (a committed leave cut, its row clean) is what a booting host
/// reads as left (`peer_link::left_cleanly`, seeding `note_left_peers`); once it is back
/// (its new boot registered) it is not, and a crashed host (its row dirty) never is.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn hosts_that_left_cleanly_are_read_at_boot() {
    install_props();
    let Some(db) = PgDb::create("lclean") else { return };
    let dir = scratch("lclean");
    let mut w = world(&db, &dir);
    let store0 = w.claims[0].store().clone();
    assert_eq!(server::peer_link::left_cleanly(&store0, 0).unwrap(), [], "both run");
    w.until_committed_after(&db, 0);
    assert_eq!(exit_host(&mut w, 1), server::claims::ServerExit::Clean, "host 1 left");
    assert_eq!(server::peer_link::left_cleanly(&store0, 0).unwrap(), [(1, 1001)], "host 1 left as boot 1001");
    assert_eq!(server::peer_link::left_cleanly(&store0, 1).unwrap(), [], "its own row is not a peer's");
    restart_host(&mut w, &db, &dir, 1, 3001);
    assert_eq!(server::peer_link::left_cleanly(&store0, 0).unwrap(), [], "back: its new boot cleared the row");
    kill_host(&mut w, 1);
    assert_eq!(server::peer_link::left_cleanly(&store0, 0).unwrap(), [], "crashed: its row stays dirty");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// A session on host 1 disconnects: its end reaches the login service through host 0.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn session_ended_on_a_non_entry_host_reaches_login() {
    install_props();
    let Some(db) = PgDb::create("lended") else { return };
    let dir = scratch("lended");
    let mut w = login_hosts(&db, &dir);
    let (b1, _) = sides(w.seam, 1, &w);
    let token = w.player(5, PlayerId(904), Some(b1));
    assert_eq!(ack_quic(&w.join(&token, 1)), (true, endpoint(1).quic));
    let client = w.connect(1, &token, 1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(client).is_empty() {
        assert!(Instant::now() < deadline, "the player never got a body");
        w.rounds(1);
    }
    assert_eq!(w.ended(&token, 1), 0);
    w.disconnect(1, client, &token, 1);
    let want = HostToLogin::SessionEnded { player_token: token.clone(), session_epoch: 1 };
    w.until_login("the session's end", 0, |m| *m == want);
    w.rounds(20);
    assert_eq!(w.ended(&token, 1), 1, "once");
    w.stop(&dir);
}

/// A player handed from host 0 to host 1 is still in the world: host 0 does not report its
/// session ended — not at the hand-off, not when the client's connection to host 0 closes
/// — so a kick the login service holds for it (queued before the crossing, resent now)
/// still parks the player on host 1. Its end is reported once, when that park takes it out
/// of the world.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_does_not_end_the_login_session() {
    install_props();
    let Some(db) = PgDb::create("lhandoff") else { return };
    let dir = scratch("lhandoff");
    let mut w = login_hosts(&db, &dir);
    let (a0, b1) = sides(w.seam, 0, &w);
    let token = w.player(6, PlayerId(905), Some(a0));
    assert_eq!(ack_quic(&w.join(&token, 1)), (true, endpoint(0).quic));
    let client = w.connect(0, &token, 1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(client).is_empty() {
        assert!(Instant::now() < deadline, "the player never got a body");
        w.rounds(1);
    }
    w.push_across(0, client, a0, b1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(client) != [(1, b1)] || w.pair.sims.iter().any(|s| s.manager.peer_unreceived() > 0) {
        assert!(Instant::now() < deadline, "the player never crossed: {:?}", w.bodies(client));
        w.rounds(1);
    }
    w.disconnect(0, client, &token, 1);
    w.rounds(10);
    assert_eq!(w.ended(&token, 1), 0, "a hand-off and a closed connection to the host it left end nothing");
    let from = w.login.seen.len();
    w.login.tx.send(LoginToHost::Kick { player_token: token.clone(), session_epoch: 1, reason: "signed out".into() }).unwrap();
    let ack = HostToLogin::KickAck { player_token: token.clone(), session_epoch: 1 };
    w.until_login("the kick ack", from, |m| *m == ack);
    w.rounds(20);
    assert!(w.bodies(client).is_empty(), "the kick parked the player");
    assert!(w.sim(1).manager.parked_record(PlayerId(905)).is_some(), "on host 1");
    assert_eq!(w.ended(&token, 1), 1, "its end reported once, by the park");
    w.stop(&dir);
}

// ---- other hosts' saved terrain (Task 19) ------------------------------------------------

/// Stud columns along a region's edge.
const REGION_STUDS: i32 = (game::REGION_CHUNKS * game::STUD_CHUNK_STUDS) as i32;

/// Host 0 digs at the border of its seam region while the peer link is down (the dig's
/// halo delta, the region's final halo and its `RegionParked` are all lost with the link),
/// then parks the region. Once a world cut has saved the park, host 1 hears the cut's
/// notice (`LISTEN`) and its running seam region's halo of the parked one is re-seeded from
/// the store: the dig shows there.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn edit_saved_by_host_0_reaches_host_1s_halo_after_the_cut() {
    install_props();
    let Some(db) = PgDb::create("terrain") else { return };
    let dir = scratch("terrain");
    let mut w = world(&db, &dir);
    let (a, b) = w.seam;
    let (ha, hb) = (w.pair.sims[0].manager.owner_of(a) as usize, w.pair.sims[0].manager.owner_of(b) as usize);
    for sim in &mut w.pair.sims {
        let store = w.claims[sim.ordinal as usize].store().clone();
        sim.terrain = Some(server::cuts::TerrainWatch::start(store, world_store::NotifyMode::Listen));
    }
    w.pair.until("running sets exchanged", |p| {
        p.sims[ha].manager.foreign_running().contains(&b) && p.sims[hb].manager.foreign_running().contains(&a)
    });
    // The top plate of `a`'s ground on its edge facing `b` (`b` is +x of `a`), and that
    // cell as `b` sees it (its halo).
    let (lo, hi) = ([REGION_STUDS - 4, 29, 20], [REGION_STUDS - 2, 30, 22]);
    let seen_from_b = |p: &mut Pair| {
        let mut m = None;
        p.sims[hb].manager.spawner_mut().with_region(b, |r| {
            m = Some(r.data().stud_material_seen([(lo[0] - REGION_STUDS) as i64, lo[1] as i64, lo[2] as i64]));
        });
        m.expect("b runs")
    };
    assert!(seen_from_b(&mut w.pair).is_some(), "b's halo holds a's ground");

    // The peer link drops.
    for sim in &w.pair.sims {
        sim.gate.set_down(true);
    }
    w.pair.until("the link is seen down", |p| !p.sims[hb].manager.foreign_running().contains(&a));
    // Host 0 digs at the border and parks the region.
    let mut dug_at = 0;
    w.pair.sims[ha].manager.spawner_mut().with_region(a, |r| {
        dug_at = r.current_tick() as i64;
        r.with_data(|d| assert!(!d.carve_stud_safe(lo, hi).is_empty(), "the dig"));
    });
    let now = w.pair.now();
    assert!(w.pair.sims[ha].manager.handle_server_event(ServerEvent::ClientDisconnected(KEEPER), now));
    w.pair.until("a parked", |p| !p.sims[ha].manager.running_regions().contains(&a));
    for _ in 0..10 {
        w.pair.round();
    }
    // Not timing-dependent: no world cut holding the dig can commit while the link is down
    // (each needs every participant's mark over it; one that is unreachable aborts the
    // cut, `world_cut.rs`), and the dig came after the link was seen down — checked here.
    assert!(
        decisions(&db).iter().all(|(_, t, k)| !*k || *t <= dug_at),
        "no cut after the dig committed while the link was down: {:?} (dug at {dug_at})",
        decisions(&db)
    );
    let saved = query_i64s(&db, &format!("SELECT saved_tick FROM regions WHERE rx = {} AND rz = {}", a.x, a.z));
    assert!(saved.iter().all(|t| *t <= dug_at), "the store does not hold the dig yet: {saved:?} (dug at {dug_at})");
    assert!(seen_from_b(&mut w.pair).is_some(), "the dig's halo traffic was lost with the link");
    let parked_after = decisions(&db).iter().map(|(c, _, _)| *c).max().unwrap_or(0);

    // The link comes back; nothing on it carries a's parked terrain (a does not run).
    for sim in &w.pair.sims {
        sim.gate.set_down(false);
    }
    w.pair.until("the dig reaches b's halo from the store", |p| seen_from_b(p).is_none());
    assert!(
        decisions(&db).iter().any(|(c, _, k)| *k && *c > parked_after),
        "brought by a world cut committed after the park"
    );
    let below = [(lo[0] - REGION_STUDS) as i64, (lo[1] - 1) as i64, lo[2] as i64];
    let mut under = None;
    w.pair.sims[hb].manager.spawner_mut().with_region(b, |r| under = r.data().stud_material_seen(below));
    assert!(under.is_some(), "the ground under the dig is still there");
    w.until_committed_after(&db, 0);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}
