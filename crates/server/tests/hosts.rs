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
    ClientId, EntityId, HostEndpoint, InlineSpawner, Ownership, PeerHello, PeerLink, PeerMsg, PeerSession, PlayerId,
    PlayerRecord, RegionCoords, RegionOutput, ServerEvent, ServerPacket, Volatile, WorldManager,
    REGION_SIZE,
};
use server::claims::HostClaims;
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

/// A host's `WorldManager` (inline regions) with its peer sessions over real links.
struct Sim {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
}

impl Sim {
    fn new(ordinal: u32, s: u32, boot: u64, generation: u64, links: &mut PeerLinks) -> Sim {
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let mut manager = WorldManager::new(
            InlineSpawner::default().with_stud_genesis(Arc::new(stud_flat_seed)),
            out_send,
            region_out_send,
            Box::new(game::NullLodBuilder::default()),
        );
        manager.set_ownership(Ownership::Static { ordinal, hosts: HOSTS }, s);
        manager.set_endpoints(endpoint(ordinal), 0x5EED + u64::from(ordinal));
        for (other, link) in links.take() {
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
        Sim { manager, region_out, packets }
    }

    /// A keeper client whose body stands in the middle of `rc` (this host's): `rc` runs.
    fn keep(&mut self, ordinal: u32, rc: RegionCoords, now: u64) {
        let y = game::plate_units(30) + 3.0;
        let record = PlayerRecord {
            client: game::Client::default(),
            pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, y, REGION_SIZE / 2.0)),
            region: rc,
            fortress: None,
            adventure: None,
        };
        let ev = ServerEvent::ClientConnected { client: KEEPER, player: PlayerId(100 + u64::from(ordinal)), record: Some(record) };
        assert!(self.manager.handle_server_event(ev, now));
    }

    fn settle(&mut self, now: u64) {
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
/// handle their regions' outputs and pump their peers (`maintain`).
struct Pair {
    sims: Vec<Sim>,
    started: Instant,
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
        for sim in &mut self.sims {
            sim.settle(now);
            sim.manager.maintain(now);
            sim.settle(now);
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    fn until(&mut self, what: &str, done: impl Fn(&mut Pair) -> bool) {
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
        sims.push(Sim::new(ordinal, s, boot, generation, &mut links));
        all_links.push(links);
        all_claims.push(claims);
    }
    let seam = host_seam(s);
    let mut pair = Pair { sims, started: Instant::now() };
    let now = pair.now();
    for rc in [seam.0, seam.1] {
        let owner = pair.sims[0].manager.owner_of(rc);
        pair.sims[owner as usize].keep(owner, rc, now);
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
