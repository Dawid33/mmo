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
    AdventureRefusal, CalendarTick, History, LegendsFilter, LegendsQuery, Location, PersonId, Sex, SiteId,
};
use game::{
    Claim, ClientId, HostId, ClientPacket, InlineLodBuilder, InlineSpawner, LodBuilder, Ownership, PlayerId,
    RegionCoords, RegionOutput, RegionSpawner, ServerEvent, ServerPacket, WorldLink, WorldManager, WorldMap,
};
use server::claims::{Fenced, HostClaims, ServerExit};
use server::coordinator_rt::{self, CoordinatorThread};
use server::player_store::{PlayerKey, PlayerStore, StorePlayers};
use server::roles::{QuicWorldLink, Role, SPLIT_ROLES_NEED_POSTGRES};
use server::world_file::{ensure_coordinator_state, WorldSource};
use world_store::{StoreHandle, StoreUrl};

#[path = "common/coordinator.rs"]
mod coordinator_common;
use coordinator_common::{attach_msg, resumed};

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
        // Ids issued from 0 (`world.next_host_id` set to 0 before anyone registers, as a world
        // migrated from fixed ordinals continues): the first host registered is `h(0)`.
        let db = db.map(PgDb)?;
        let store = db.open();
        let url = db.0.url.clone();
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                use sqlx::Connection;
                let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
                sqlx::query("UPDATE world SET next_host_id = 0").execute(&mut c).await.unwrap();
                c.close().await.unwrap();
            })
        })
        .join()
        .unwrap();
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

/// An adventure start at a populated site: the state-changing world request the downtime,
/// memo, write-before-send and conservation guarantees are proven on.
#[derive(Clone, Copy, Debug)]
struct Start {
    site: SiteId,
    /// The site's region: where the player goes, and the region that runs its people.
    region: RegionCoords,
}

impl Start {
    fn packet(&self) -> ClientPacket {
        ClientPacket::StartAdventure(self.site)
    }
}

/// Whether `site` may be adventured at: not abandoned, a living member.
fn populated(h: &History, site: SiteId) -> bool {
    h.site(site).is_some_and(|s| s.abandoned.is_none()) && !h.living_members(site).is_empty()
}

/// An adventure at the first populated site.
fn start_request(h: &History) -> Start {
    let s = h.sites().find(|s| populated(h, s.id)).expect("a populated site");
    Start { site: s.id, region: s.region }
}

/// The coordinator's saved history.
fn stored_history(store: &StoreHandle, map: &WorldMap) -> History {
    let row = store.load_coordinator().unwrap().expect("a saved coordinator state");
    let grid = game::history::SocietyGrid::from_world_map(map);
    game::history::persist::decode(&row.history, grid).expect("its history decodes")
}

/// The players' claims in the coordinator's saved state (none before its first write).
fn stored_claims(store: &StoreHandle, map: &WorldMap) -> Vec<(PlayerId, Claim)> {
    if store.load_coordinator().unwrap().is_none() {
        return Vec::new();
    }
    stored_core(store, map).claims()
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
/// (waited for); its hello carries the host's registration number `reg` (the coordinator
/// admits only its `hosts` row's, while live).
fn quic_link(addr: std::net::SocketAddr, cert_file: PathBuf, ordinal: HostId, boot: u64, reg: u64) -> QuicWorldLink {
    let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Host { id: ordinal, boot, reg }, "");
    QuicWorldLink::connect(addr, move || svc_link::read_cert_hash("MMO_ROLES_TEST_UNSET_PIN", &cert_file), hello)
}

/// Registers boot `boot` as a host named after `ordinal` (as a `host` process does once per
/// boot): its registration. Elastic 1 task 2: the store issues the id (Task 3 keys the rest of
/// these tests by it).
/// Registers a host process named `host-<n>` (`ordinal`: its name's number, not its id —
/// the store issues the id) as `static`.
fn register_host(store: &StoreHandle, ordinal: HostId, boot: u64) -> world_store::Registered {
    register_as(store, ordinal, boot, game::CrashPolicy::Rollback)
}

/// [`register_host`] under `policy`.
fn register_as(store: &StoreHandle, ordinal: HostId, boot: u64, policy: game::CrashPolicy) -> world_store::Registered {
    let host = world_store::NewHost {
        name: format!("host-{}", ordinal.0),
        boot,
        peer_addr: None,
        peer_cert: None,
        quic: None,
        webtransport: None,
        wt_cert_hash: None,
    };
    store.register_host(host, policy).unwrap_or_else(|e| panic!("host {ordinal} not registered: {e}"))
}

/// A `static` host alone (its table is itself).
fn alone(me: HostId) -> game::Static {
    game::Static { me, layout: vec![me] }
}

/// Host `me` of the fixed table `h(0)..h(n)` (several hosts of a fixed table exist only in
/// tests since Elastic 1).
fn table(me: HostId, n: u64) -> game::Static {
    game::Static { me, layout: (0..n).map(HostId).collect() }
}

/// A localhost UDP port nobody holds right now.
fn free_udp_addr() -> std::net::SocketAddr {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap()
}

// ---- the host process -------------------------------------------------------------------

/// A host's server life over the link `link` makes of its registration number (inline regions, its
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
        ownership: impl Fn(HostId) -> Box<dyn Ownership>,
        link: impl FnOnce(u64) -> Box<dyn WorldLink + Send>,
        coordinator: Option<CoordinatorThread>,
        boot: u64,
    ) -> Host {
        // Registered FIRST, once per boot, as a `host` process is (`server::run`'s order: the
        // coordinator admits its hello by the issued id); the ownership and claims are the
        // issued id's; it runs the generation it registered at. A host alone (`static`, or the
        // whole world) registers as `static` does (`Rollback`: refused beside a live row); a
        // host of a several-host table — tests only — as `claim` does, so two run side by side.
        let policy = if ownership(HostId::SOLO).runs_every_host() { game::CrashPolicy::Rollback } else { game::CrashPolicy::Succession };
        let registered = register_as(store, h(boot), boot, policy);
        let ownership = ownership(registered.id);
        let claims = HostClaims::claim(store.clone(), ownership.as_ref(), Some(map.extent()), store.super_region_regions()).unwrap();
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
            coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
            water: Box::new(game::lod::LodWater::dry),
            out: lod_result_send,
        });
        let mut manager = WorldManager::new(
            InlineSpawner::default(),
            Box::new(flat_floor),
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        );
        manager.set_ownership(ownership, store.super_region_regions());
        manager.set_stored_regions(store.stored_regions().unwrap());
        manager.restore_in_flight(store.load_in_flight(claims.fence().in_flight_scope()).unwrap());
        manager.set_host_generation(registered.generation);
        manager.set_world_link(link(registered.id.0), boot, 0);
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
                    | ServerPacket::LodData(..)
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

    fn adventure_results(&self, c: ClientId) -> Vec<Result<SiteId, AdventureRefusal>> {
        self.inbox
            .iter()
            .filter_map(|(to, p)| match p {
                ServerPacket::AdventureResult(r) if *to == Some(c) => Some(*r),
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

/// The script both runs play: A connects, starts an adventure, asks the site's legends,
/// ends the adventure, asks them again. Returns every packet A got (bar the regions' stream
/// and the day's broadcasts), in order, as text, and the claims `store` held once the start
/// was answered (its reply leaves only after its write).
fn play_script(
    host: &mut Host,
    player: PlayerId,
    start: Start,
    store: &StoreHandle,
    map: &WorldMap,
) -> (Vec<String>, Vec<(PlayerId, Claim)>) {
    host.until("attached", |h| h.manager.world_attached());
    host.send(ServerEvent::ClientConnected { client: A, player, record: None });
    for _ in 0..3 {
        host.round();
    }
    host.send(ServerEvent::ClientPacket(start.packet(), A));
    host.until("an AdventureResult", |h| !h.adventure_results(A).is_empty());
    let site = host.adventure_results(A)[0].expect("started");
    let claims = stored_claims(store, map);
    let query = LegendsQuery { filter: LegendsFilter::Site(site), before: None, limit: 20 };
    host.send(ServerEvent::ClientPacket(ClientPacket::RequestLegends(query.clone()), A));
    host.until("a Legends page", |h| h.legends_pages(A) == 1);
    host.send(ServerEvent::ClientPacket(ClientPacket::EndAdventure, A));
    host.until("the claim released after the end", |_| stored_claims(store, map).is_empty());
    host.send(ServerEvent::ClientPacket(ClientPacket::RequestLegends(query), A));
    host.until("a second Legends page", |h| h.legends_pages(A) == 2);
    for _ in 0..5 {
        host.round();
    }
    let packets = host
        .inbox
        .iter()
        .filter(|(to, _)| *to == Some(A) || to.is_none())
        .map(|(to, p)| match p {
            // The map is the same value in both runs; its text is large.
            ServerPacket::WorldMap(m) => format!("{to:?} WorldMap {}x{}", m.width, m.height),
            p => format!("{to:?} {p:?}"),
        })
        .collect();
    (packets, claims)
}

/// The world split in two processes plays like the same world in one: the same script
/// (connect, start an adventure, legends, end it, legends) gets the same packets, and the
/// coordinator process — the only writer of history state — saved the same claim before
/// answering, and its release.
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
    let adventure = start_request(start.coordinator.history());
    let claims_fenced = Fenced::default();
    let (link, thread) = coordinator_rt::start_all_in_one(store.clone(), start, claims_fenced).unwrap();
    let mut host = Host::start(&store, &map, |_| Box::new(game::AllInOne), |_| Box::new(link), Some(thread), 1);
    let (all_packets, all_claims) = play_script(&mut host, a, adventure, &store, &map);
    assert_eq!(host.exit(), ServerExit::Clean);
    let store = all_db.open();
    let all_after = stored_claims(&store, &map);
    store.shutdown();

    // Split: a coordinator process and a host process over the service link.
    let Some(split_db) = PgDb::create("split") else { return };
    let dir = scratch("split");
    let cert_file = dir.join("coordinator.cert-sha256");
    let (coordinator, addr) = Coordinator::start(&split_db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
    let host_store = split_db.open();
    let (a_split, _) = StorePlayers(host_store.clone()).resolve(&a_key).unwrap();
    assert_eq!(a_split, a, "the same player id in both worlds");
    let link = |reg| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file, h(0), 1, reg)) };
    let mut host = Host::start(&host_store, &map, |me| Box::new(alone(me)), link, None, 1);
    let (split_packets, split_claims) = play_script(&mut host, a, adventure, &coordinator.store, &map);
    assert_eq!(host.exit(), ServerExit::Clean);
    host_store.shutdown();
    coordinator.stop();
    let split_store = split_db.open();
    let split_after = stored_claims(&split_store, &map);
    assert_eq!(split_store.load_history().unwrap(), None, "the host wrote no history row");
    split_store.shutdown();

    assert!(all_packets.iter().any(|p| p.contains("AdventureResult(Ok(")), "{all_packets:#?}");
    assert_eq!(split_packets, all_packets, "the split world sends the client what the one-process world sends");
    assert_eq!(all_claims, vec![(a, Claim::Adventure(adventure.site))], "the start's claim, saved before its answer");
    assert_eq!(split_claims, all_claims, "the coordinator process saved the same claim");
    assert_eq!((all_after.len(), split_after.len()), (0, 0), "the end's release saved: {all_after:?} {split_after:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Elastic 1 (was `hosts::several_hosts_refuse_local_auth`): `claim` means any number of
/// hosts, so a `claim` host starts only as `host`, on Postgres, with the login link (`jwt`
/// with a service token) — one host included; `static` / `allinone` need none of it here.
#[test]
fn a_claim_host_without_postgres_or_jwt_is_refused() {
    use game::CrashPolicy::{Rollback, Succession};
    use server::roles::{check_claim_needs, CLAIM_NEEDS_LOGIN, CLAIM_NEEDS_ROLE_HOST};
    assert_eq!(check_claim_needs(Role::Host, Succession, true, false), Err(CLAIM_NEEDS_LOGIN.to_string()));
    assert!(CLAIM_NEEDS_LOGIN.contains("VOXEL_AUTH=jwt"), "{CLAIM_NEEDS_LOGIN}");
    assert_eq!(check_claim_needs(Role::Host, Succession, false, true), Err(SPLIT_ROLES_NEED_POSTGRES.to_string()));
    assert_eq!(check_claim_needs(Role::All, Succession, true, true), Err(CLAIM_NEEDS_ROLE_HOST.to_string()));
    assert_eq!(check_claim_needs(Role::Host, Succession, true, true), Ok(()));
    for role in [Role::All, Role::Host] {
        assert_eq!(check_claim_needs(role, Rollback, false, false), Ok(()), "{role:?} without claim");
    }
}

/// Elastic 1: a `static` host is alone. A second `static` host started beside a live one is
/// refused at its registration — what `server::run` turns into its start-up error (exit 1) —
/// with a message naming `VOXEL_OWNERSHIP=claim`; nothing is issued or written for it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_second_static_host_is_refused_at_registration() {
    let Some(db) = PgDb::create("secondstatic") else { return };
    let store = db.open();
    let first = register_host(&store, h(1), 1);
    let host = world_store::NewHost {
        name: "another".into(),
        boot: 2,
        peer_addr: None,
        peer_cert: None,
        quic: None,
        webtransport: None,
        wt_cert_hash: None,
    };
    let stop = std::sync::atomic::AtomicBool::new(false);
    let err = server::lease::register(&store, host, game::CrashPolicy::Rollback, &stop).expect_err("refused");
    assert!(matches!(&err, world_store::StoreError::NotAlone { live } if *live == vec![first.id]), "{err:?}");
    assert_eq!(
        server::lease::registration_refusal(&err),
        format!(
            "registering this host: a static host registers only when no other host runs, but hosts {} hold live rows; \
             several hosts need VOXEL_OWNERSHIP=claim",
            first.id
        ),
        "the start-up error `server::run` exits 1 with"
    );
    assert_eq!(store.hosts().unwrap().iter().map(|r| r.id).collect::<Vec<_>>(), vec![first.id], "nothing written");
    store.shutdown();
}

/// Elastic 1 final review A-I2: the checks a host needs no id for fail BEFORE it registers — a
/// wrong `VOXEL_SUPER_REGION_REGIONS` or a bad `VOXEL_OWNERSHIP` word is refused naming the
/// variable, and nothing is issued or written: no `hosts` row, `next_host_id` unchanged (`run`
/// calls this right before `lease::register`). The right settings pass.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_misconfigured_host_fails_before_it_registers() {
    let Some(db) = PgDb::create("preflight") else { return };
    let store = db.open();
    let next = scalar(&db, "SELECT next_host_id FROM world");
    let side = store.super_region_regions();
    let check = |word: &str, side: u32| {
        server::roles::check_before_registration(server::roles::Role::Host, Some(&store), true, word, true, side, 0)
    };
    let err = check("claim", side + 1).expect_err("a wrong super-region side");
    assert!(err.contains(server::claims::SUPER_REGION_REGIONS_ENV), "{err}");
    let err = check("claimz", side).expect_err("a bad ownership word");
    assert!(err.contains(game::ownership::OWNERSHIP_ENV), "{err}");
    assert!(store.hosts().unwrap().is_empty(), "no hosts row");
    assert_eq!(scalar(&db, "SELECT next_host_id FROM world"), next, "no id issued");
    check("claim", side).expect("the right settings pass");
    store.shutdown();
}

/// Runs `sql` on its own connection: the first column of the first row.
fn scalar(db: &PgDb, sql: &str) -> i64 {
    let (url, sql) = (db.0.url.clone(), sql.to_string());
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            use sqlx::Connection;
            let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
            let v: i64 = sqlx::query_scalar(&sql).fetch_one(&mut c).await.unwrap();
            c.close().await.unwrap();
            v
        })
    })
    .join()
    .unwrap()
}

/// How often `id` is in `host`'s running world: a running region's entity, or in flight /
/// waiting for its region on the manager.
fn live_on(host: &mut Host, id: game::EntityId) -> usize {
    let mut n = host.manager.in_flight_ids().iter().filter(|(i, _)| *i == id).count();
    n += host.manager.pending_arrival_ids().iter().filter(|(i, _)| *i == id).count();
    for rc in host.manager.running_regions() {
        host.manager.spawner_mut().with_region(rc, |region| {
            if region.data().id_index.contains_key(&id) {
                n += 1;
            }
        });
    }
    n
}

/// A lone `static` host's process in `db` (named `host-1` both times it starts: the store's
/// same-name vouch applies), on the production path — `Host::start` with `alone(me)`: an
/// all-hosts fence, the one-phase `CutWriter` at its exit, registered as `static`.
struct LoneStatic {
    db: PgDb,
    map: WorldMap,
    dir: PathBuf,
    cert_file: PathBuf,
    addr: std::net::SocketAddr,
    coordinator: Coordinator,
    player: PlayerId,
}

impl LoneStatic {
    fn new(tag: &str) -> Option<LoneStatic> {
        install_props();
        let db = PgDb::create(tag)?;
        let map = land_map();
        let dir = scratch(tag);
        let cert_file = dir.join("coordinator.cert-sha256");
        let (coordinator, addr) = Coordinator::start(&db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
        let store = db.open();
        let key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
        let (player, _) = StorePlayers(store.clone()).resolve(&key).unwrap();
        store.shutdown();
        Some(LoneStatic { db, map, dir, cert_file, addr, coordinator, player })
    }

    /// The host process starts (named `host-1`: boot 1 every time).
    fn start(&self) -> Host {
        let store = self.db.open();
        let (addr, cert_file) = (self.addr, self.cert_file.clone());
        let link = move |reg: u64| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file, HostId(reg), 1, reg)) };
        let mut host = Host::start(&store, &self.map, |me| Box::new(alone(me)), link, None, 1);
        assert_eq!(host.claims.fence().in_flight_scope(), None, "a lone static host's fence adopts every host's rows");
        host.until("attached", |h| h.manager.world_attached());
        host
    }

    /// Its player stands in `r` (r runs); a slime there crosses into `r` + x, a region of its own
    /// that does not run: it waits in flight. The slime.
    fn transfer_in_flight(&self, host: &mut Host, r: RegionCoords) -> game::EntityId {
        let next = RegionCoords::new(r.x + 1, r.z);
        host.send(ServerEvent::ClientConnected { client: A, player: self.player, record: Some(standing_in(r)) });
        host.until("r runs", |h| h.manager.running_regions().contains(&r));
        let mut id = None;
        host.manager.spawner_mut().with_region(r, |region| {
            let tick = region.current_tick();
            let pos = game::parry::math::Vector::new(game::REGION_SIZE / 2.0, 30.0, game::REGION_SIZE / 2.0);
            region.with_data(|d| {
                let e = d.spawn_slime_safe(pos, 1, 0.0, tick);
                id = *d.ecs.id.try_get(e);
            });
        });
        let id = id.expect("the slime has an EntityId");
        for _ in 0..3 {
            host.round();
        }
        host.manager.spawner_mut().with_region(r, |region| {
            let key = region.data().id_index[&id];
            let at = game::rapier3d::math::Vector::new(game::REGION_SIZE + game::FLIP_HYSTERESIS + 2.0, 30.0, game::REGION_SIZE / 2.0);
            region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
        });
        host.until("the slime waits in flight for its region", |h| {
            h.manager.in_flight_ids().iter().any(|(i, _)| *i == id) || h.manager.pending_arrival_ids().iter().any(|(i, _)| *i == id)
        });
        assert!(!host.manager.running_regions().contains(&next), "its target region does not run");
        assert_eq!(live_on(host, id), 1);
        id
    }

    /// The store's rows of `id` in regions (entity rows).
    fn stored(&self, id: game::EntityId) -> i64 {
        scalar(
            &self.db,
            &format!(
                "SELECT COUNT(*) FROM entities WHERE birth_rx = {} AND birth_rz = {} AND serial = {}",
                id.birth_region.x, id.birth_region.z, id.serial
            ),
        )
    }

    /// In-flight transfers of `id` under host `h`, as another process reads them.
    fn in_flight(&self, h: HostId, id: game::EntityId) -> usize {
        let store = self.db.open();
        let n = store
            .load_in_flight(Some(h))
            .unwrap()
            .iter()
            .filter(|row| matches!(row, game::InFlightRow::Transfer { bundle, .. } if bundle.id == id))
            .count();
        store.shutdown();
        n
    }

    fn generation(&self) -> u64 {
        let store = self.db.open();
        let g = store.world_generation().unwrap();
        store.shutdown();
        g
    }

    /// The new process's player stands in `next`: the slime arrives there, delivered once; the
    /// host exits (its one-phase cut) — the slime stored once, nothing left under `old`.
    fn deliver_and_check(&self, mut host: Host, id: game::EntityId, next: RegionCoords, old: HostId) {
        host.send(ServerEvent::ClientConnected { client: A, player: self.player, record: Some(standing_in(next)) });
        host.until("the slime arrived in its region", |h| {
            let mut on = false;
            if h.manager.running_regions().contains(&next) {
                h.manager.spawner_mut().with_region(next, |region| on = region.data().id_index.contains_key(&id));
            }
            on
        });
        for _ in 0..10 {
            host.round();
        }
        assert_eq!(live_on(&mut host, id), 1, "delivered exactly once");
        assert_eq!(host.exit(), ServerExit::Clean);
        assert_eq!(self.stored(id), 1, "stored exactly once");
        assert_eq!(scalar(&self.db, &format!("SELECT COUNT(*) FROM in_flight WHERE host = {}", old.0)), 0, "nothing left under {old}");
    }

    fn stop(self) {
        self.coordinator.stop();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Elastic 1 fix round 2 — the REAL production restart of a lone `static` host (`just dev-r2`'s
/// host): it exits through its one-phase leave with a transfer in flight (its last cut stores it
/// as an in-flight row under its id; its row stays DIRTY — a one-phase exit never marks it clean)
/// and starts again under its name. The name lapses the old row, nobody else is live and a dirty
/// row exists: the total-crash rule — a NEW id, the generation + 1, the old row's in-flight row
/// re-keyed to the new id. The slime is delivered exactly once and stored exactly once, and
/// nothing is left under the old id. (Before Elastic 1 this restart raised the generation too:
/// the dirty prior row of the same ordinal bumped it.)
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_lone_static_host_restarted_takes_its_dirty_row_and_delivers_its_in_flight_once() {
    let Some(w) = LoneStatic::new("staticrestart") else { return };
    let (r, next) = (RegionCoords::new(10, 10), RegionCoords::new(11, 10));
    let mut host = w.start();
    let old = host.claims.fence().owner;
    let id = w.transfer_in_flight(&mut host, r);
    assert_eq!(host.exit(), ServerExit::Clean);
    assert_eq!(w.in_flight(old, id), 1, "the last cut holds the slime in flight under {old}");
    assert_eq!(w.stored(id), 0, "in no region row");
    assert_eq!(scalar(&w.db, &format!("SELECT clean FROM hosts WHERE id = {}", old.0)), 0, "a one-phase exit leaves the row dirty");
    let generation = w.generation();

    let host = w.start();
    let new = host.claims.fence().owner;
    assert!(new != old, "a new id: {new}");
    assert_eq!(w.generation(), generation + 1, "a total crash: the generation + 1");
    assert_eq!((w.in_flight(old, id), w.in_flight(new, id)), (0, 1), "the in-flight row is the new id's");
    w.deliver_and_check(host, id, next, old);
    assert_eq!(w.generation(), generation + 1, "raised once");
    w.stop();
}

/// Elastic 1 fix round 1, renamed in fix round 2 — what the all-hosts fence of a lone `static`
/// host is for: in-flight rows under a CLEAN row of another id, which no total crash re-keys.
/// A lone static host never makes that state itself (its one-phase exit leaves its row dirty:
/// see the test above). It arises when a world whose `claim` hosts left in a committed leave
/// cut — the two-phase decision marks their rows clean, a leave cut holding transfers for land
/// that does not run as in-flight rows — is started again as ONE `static` host (`dev-r3` stopped,
/// then `dev-r2` on the same world). Here the SQL `clean = 1` stands in for that leave cut's
/// decision over the earlier process's row. The new static host registers at the same generation
/// (no dirty row: not bumped), and only its all-hosts fence loads the clean row's in-flight row:
/// delivered exactly once, stored exactly once, nothing left under the old id. (Reverting
/// `Static::runs_every_host` makes it time out waiting for the slime: the mutation check.)
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_lone_static_host_delivers_a_clean_leavers_in_flight_rows_once() {
    let Some(w) = LoneStatic::new("staticflight") else { return };
    let (r, next) = (RegionCoords::new(10, 10), RegionCoords::new(11, 10));
    let mut host = w.start();
    let old = host.claims.fence().owner;
    let id = w.transfer_in_flight(&mut host, r);
    assert_eq!(host.exit(), ServerExit::Clean);
    assert_eq!(w.in_flight(old, id), 1, "the last cut holds the slime in flight under {old}");
    assert_eq!(w.stored(id), 0, "in no region row");
    // Stand-in for a two-phase leave decision (a `claim` world's): the row is clean.
    scalar(&w.db, &format!("WITH u AS (UPDATE hosts SET clean = 1 WHERE id = {} RETURNING 1) SELECT COUNT(*) FROM u", old.0));
    let generation = w.generation();

    let host = w.start();
    let new = host.claims.fence().owner;
    assert!(new != old, "a new id: {new}");
    assert_eq!(w.generation(), generation, "no dirty row: not bumped");
    assert_eq!(w.in_flight(old, id), 1, "the clean row's in-flight row stays under it: only the fence adopts it");
    w.deliver_and_check(host, id, next, old);
    w.stop();
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
    let adventure = {
        let store = db.open();
        let start = ensure_coordinator_state(&store, &map, None).unwrap().expect("a history");
        store.shutdown();
        start_request(start.coordinator.history())
    };
    let host_store = db.open();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
    let (a, _) = StorePlayers(host_store.clone()).resolve(&a_key).unwrap();
    let link = |reg| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file.clone(), h(0), 7, reg)) };
    let mut host = Host::start(&host_store, &map, |me| Box::new(alone(me)), link, None, 7);
    host.send(ServerEvent::ClientConnected { client: A, player: a, record: None });
    let tick_before = host.manager.spawner_mut().world_tick();
    for _ in 0..20 {
        host.round();
    }
    assert!(!host.manager.running_regions().is_empty(), "the host runs its player's region");
    assert!(host.manager.spawner_mut().world_tick() > tick_before, "and simulates without a coordinator");
    host.send(ServerEvent::ClientPacket(adventure.packet(), A));
    let query = LegendsQuery { filter: LegendsFilter::All, before: None, limit: 5 };
    host.send(ServerEvent::ClientPacket(ClientPacket::RequestLegends(query), A));
    for _ in 0..10 {
        host.round();
    }
    assert!(!host.manager.world_attached(), "no coordinator: not attached");
    assert_eq!(host.manager.pending_world_requests(), 2, "both requests wait");
    assert_eq!(host.manager.world_outbox_len(), 3, "in the outbox, behind the player's PlayerOnline");
    assert!(host.adventure_results(A).is_empty() && host.legends_pages(A) == 0, "nothing answered yet");

    // The coordinator comes up (writing its pin): the host attaches, the queue drains.
    let (coordinator, _) = Coordinator::start(&db, &map, addr, cert_file);
    host.until("attached to the late coordinator", |h| h.manager.world_attached());
    host.until("the start answered", |h| !h.adventure_results(A).is_empty());
    host.until("the legends answered", |h| h.legends_pages(A) == 1);
    for _ in 0..10 {
        host.round();
    }
    let results = host.adventure_results(A);
    assert_eq!(results, vec![Ok(adventure.site)], "one AdventureResult");
    assert_eq!(host.legends_pages(A), 1, "one Legends page");
    assert_eq!(host.manager.pending_world_requests(), 0);
    assert_eq!(
        stored_claims(&coordinator.store, &map),
        vec![(a, Claim::Adventure(adventure.site))],
        "granted once, saved by the coordinator"
    );
    assert_eq!(host.exit(), ServerExit::Clean);
    host_store.shutdown();
    coordinator.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host's `/readyz` follows its attach (Elastic 2b Task 9): registered but unattached it
/// answers 503 `attaching`; once the coordinator acknowledged its `Attach` (the loop's
/// `set_attached(manager.world_attached())`) it answers 200.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_host_is_ready_only_after_its_attach_is_acknowledged() {
    use std::io::{Read, Write};
    install_props();
    let Some(db) = PgDb::create("readyattach") else { return };
    let map = land_map();
    let dir = scratch("readyattach");
    let cert_file = dir.join("coordinator.cert-sha256");
    let addr = free_udp_addr();
    let get = |http: std::net::SocketAddr| -> (u16, String) {
        let mut stream = std::net::TcpStream::connect(http).expect("connect");
        stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
        write!(stream, "GET /readyz HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").expect("request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("response");
        let status = response.split(' ').nth(1).and_then(|s| s.parse().ok()).expect("a status line");
        (status, response.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default())
    };
    let health = server::health::Health::for_test();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let http = listener.local_addr().unwrap();
    server::health::serve_for_test(listener, "host", health).unwrap();
    health.require_attach();
    health.set_registered(true);

    let host_store = db.open();
    let link = |reg| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file.clone(), h(0), 7, reg)) };
    let mut host = Host::start(&host_store, &map, |me| Box::new(alone(me)), link, None, 7);
    for _ in 0..10 {
        host.round();
        health.set_attached(host.manager.world_attached());
    }
    assert!(!host.manager.world_attached(), "no coordinator yet");
    assert_eq!(get(http), (503, "attaching".to_string()), "registered, not attached");

    let (coordinator, _) = Coordinator::start(&db, &map, addr, cert_file);
    host.until("attached to the coordinator", |h| {
        health.set_attached(h.manager.world_attached());
        h.manager.world_attached()
    });
    assert_eq!(get(http).0, 200, "attached: ready");
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
        adventure: None,
    }
}

/// An adventure at the first populated site whose region host `ordinal` (of two) owns.
fn start_request_on(h: &History, s: u32, ordinal: HostId) -> Start {
    let owner = |rc: RegionCoords| table(HostId(0), 2).owner_of(rc, s);
    let site = h.sites().find(|x| populated(h, x.id) && owner(x.region) == ordinal).expect("a populated site on the host");
    Start { site: site.id, region: site.region }
}

/// Where each started site's people are: every person the coordinator's history embodies in
/// the site's region has one body, there, on the host of that census and on no other; no
/// living member is left abstract at the site while its region runs; someone is there.
fn placed(
    coordinator: &Coordinator,
    censuses: &[(BTreeMap<PersonId, Vec<RegionCoords>>, Start)],
) -> Result<(), String> {
    let starts: Vec<Start> = censuses.iter().map(|(_, s)| *s).collect();
    let people = coordinator
        .thread
        .inspect(
            move |c| {
                let h = c.history();
                starts
                    .iter()
                    .map(|s| {
                        let embodied: Vec<PersonId> = h
                            .people()
                            .filter(|q| q.alive() && q.location == Location::InRegion(s.region))
                            .map(|q| q.id)
                            .collect();
                        let abstract_here: Vec<PersonId> = h
                            .living_members(s.site)
                            .into_iter()
                            .filter(|p| h.person(*p).is_some_and(|q| q.location == Location::AtSite(s.site)))
                            .collect();
                        (embodied, abstract_here)
                    })
                    .collect::<Vec<_>>()
            },
            WAIT,
        )
        .expect("the coordinator answers");
    for (k, ((census, start), (embodied, abstract_here))) in censuses.iter().zip(people).enumerate() {
        if embodied.is_empty() {
            return Err(format!("nobody embodied in {:?}", start.region));
        }
        if !abstract_here.is_empty() {
            return Err(format!("{abstract_here:?} left abstract at {:?} while its region runs", start.site));
        }
        for p in &embodied {
            if census.get(p) != Some(&vec![start.region]) {
                let at = census.get(p);
                return Err(format!("{p:?} is in {:?} in the history, bodies on host {k}: {at:?}", start.region));
            }
            if let Some((other, _)) = censuses.iter().enumerate().find(|(j, (c, _))| *j != k && c.contains_key(p)) {
                return Err(format!("{p:?} of host {k}'s site has a body on host {other}"));
            }
        }
    }
    Ok(())
}

/// A coordinator serves two hosts (R3): both attach — host 1's `Attach` parks nothing of
/// host 0's — and each one's player starts an adventure at a populated site in a region that
/// host owns: the site's people get their bodies on the host that runs it, and keep them.
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
    let link = |ordinal: HostId| {
        let cert_file = cert_file.clone();
        move |reg| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file, ordinal, 10 + ordinal.0, reg)) }
    };
    let mut h0 = Host::start(&store0, &map, |me| Box::new(table(me, 2)), link(h(0)), None, 10);
    h0.until("host 0 attached", |h| h.manager.world_attached());

    // Host 0's player starts an adventure at a site of host 0.
    let start0 = start_request_on(&stored_history(&coordinator.store, &map), s, h(0));
    let owner = |rc: RegionCoords| table(h(0), 2).owner_of(rc, s);
    let spawn0 = (2..46).map(|x| RegionCoords::new(x, 2)).find(|r| owner(*r) == h(0)).expect("a region of host 0");
    h0.send(ServerEvent::ClientConnected { client: A, player: a, record: Some(standing_in(spawn0)) });
    h0.send(ServerEvent::ClientPacket(start0.packet(), A));
    h0.until("host 0's start", |h| !h.adventure_results(A).is_empty());
    assert_eq!(h0.adventure_results(A), vec![Ok(start0.site)], "host 0's player started");
    h0.until("host 0's site people", |h| placed(&coordinator, &[(bodies(&mut h.manager), start0)]).is_ok());

    // Host 1 attaches: nothing of host 0's parks.
    let mut h1 = Host::start(&store1, &map, |me| Box::new(table(me, 2)), link(h(1)), None, 11);
    until_both(&mut h0, &mut h1, "host 1 attached", |_, h1| h1.manager.world_attached());
    let start1 =
        coordinator.thread.inspect(move |c| start_request_on(c.history(), s, h(1)), WAIT).expect("the coordinator answers");
    assert_ne!(start0.region, start1.region);
    let spawn1 = (2..46).map(|x| RegionCoords::new(x, 2)).find(|r| owner(*r) == h(1)).expect("a region of host 1");
    h1.send(ServerEvent::ClientConnected { client: A, player: b, record: Some(standing_in(spawn1)) });
    h1.send(ServerEvent::ClientPacket(start1.packet(), A));
    until_both(&mut h0, &mut h1, "host 1's start", |_, h1| !h1.adventure_results(A).is_empty());
    assert_eq!(h1.adventure_results(A), vec![Ok(start1.site)], "host 1's player started");
    let both = |h0: &mut Host, h1: &mut Host| {
        placed(&coordinator, &[(bodies(&mut h0.manager), start0), (bodies(&mut h1.manager), start1)])
    };
    until_both(&mut h0, &mut h1, "both sites' people stand on their hosts", |h0, h1| both(h0, h1).is_ok());
    for _ in 0..20 {
        h0.round();
        h1.round();
    }
    if let Err(e) = both(&mut h0, &mut h1) {
        panic!("after 20 more rounds: {e}");
    }
    assert_eq!(h0.adventure_results(A).len() + h1.adventure_results(A).len(), 2, "one answer each");
    coordinator.stop();
    drop((h0, h1));
    store0.shutdown();
    store1.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host process whose registration was superseded (ordinal 0 registered twice: a newer
/// boot) is refused by a coordinator that started after both — it remembers no connection,
/// the `hosts` row's registration number decides: the stale hello's `Attach` gets nothing;
/// the current registration's is echoed.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_host_with_a_stale_registration_is_refused_by_the_coordinator() {
    install_props();
    let Some(db) = PgDb::create("stalereg") else { return };
    let map = land_map();
    let dir = scratch("stalereg");
    let cert_file = dir.join("coordinator.cert-sha256");
    let store = db.open();
    let (first, second) = (register_host(&store, h(0), 1), register_host(&store, h(0), 2));
    // The process restarted under its name: a new id at once (Elastic 1); its old row was
    // lapsed by the name and, nobody else live, the restart took it (a total crash: the
    // generation raised, the old row gone). Both Attach at the world's generation now: only
    // the id tells them apart — the old one has no row.
    let generation = second.generation;
    let (first, second) = (first.id.0, second.id.0);
    assert!(second > first, "a second registration has the higher number: {first} then {second}");
    // A coordinator that ran before is gone; this one starts fresh.
    let (c1, addr) = Coordinator::start(&db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
    c1.stop();
    let (coordinator, _) = Coordinator::start(&db, &map, addr, cert_file.clone());
    let pin = svc_link::read_cert_hash("MMO_ROLES_TEST_UNSET_PIN", &cert_file).unwrap();
    let dial = |boot: u64, reg: u64| {
        // A host's hello names the id the store issued it (its registration number too).
        let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Host { id: HostId(reg), boot, reg }, "");
        let (link, events) = svc_link::LinkDialer::connect::<game::CoordMsg, game::HostMsg>(addr, pin.clone(), hello);
        let deadline = Instant::now() + WAIT;
        loop {
            match events.recv_timeout(Duration::from_millis(50)) {
                Ok(svc_link::LinkEvent::Up(_)) => break,
                _ => assert!(Instant::now() < deadline, "the link never came up"),
            }
        }
        link.send(bare_attach(HostId(reg), generation, 1, game::CrashPolicy::Rollback)).unwrap();
        (link, events)
    };
    let (stale, stale_events) = dial(1, first);
    let (live, live_events) = dial(2, second);
    let deadline = Instant::now() + WAIT;
    loop {
        match live_events.recv_timeout(Duration::from_millis(50)) {
            Ok(svc_link::LinkEvent::Msg(game::CoordMsg::Attached { epoch: 1 })) => break,
            _ => assert!(Instant::now() < deadline, "the current registration's Attach was never echoed"),
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

/// A raw host link to the coordinator at `addr` (host 0, registration `reg`), up.
fn raw_host_link(
    addr: std::net::SocketAddr,
    cert_file: &std::path::Path,
    boot: u64,
    reg: u64,
) -> (crossbeam::channel::Sender<game::HostMsg>, Receiver<svc_link::LinkEvent<game::CoordMsg>>) {
    let pin = svc_link::read_cert_hash("MMO_ROLES_TEST_UNSET_PIN", cert_file).unwrap();
    let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Host { id: HostId(reg), boot, reg }, "");
    let (link, events) = svc_link::LinkDialer::connect::<game::CoordMsg, game::HostMsg>(addr, pin, hello);
    let deadline = Instant::now() + WAIT;
    loop {
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(svc_link::LinkEvent::Up(_)) => break,
            _ => assert!(Instant::now() < deadline, "the link never came up"),
        }
    }
    (link, events)
}

/// An empty `Attach` of host `host` at `generation` under `policy`.
fn bare_attach(host: HostId, generation: u64, boot: u64, policy: game::CrashPolicy) -> game::HostMsg {
    attach_msg(host, 1, Vec::new(), generation, boot, None, policy)
}

/// A coordinator of SEVERAL hosts started without `VOXEL_OWNERSHIP=claim` (crash policy
/// `Rollback`) refuses the `Attach` of a host that runs `claim` (`Succession`): it would
/// never declare the succession that host's restart waits for (the fix wave's guard for the
/// dev recipes that forgot the coordinator's ownership word). The link is closed, nothing is
/// echoed; a host under the coordinator's own policy attaches.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_host_under_another_crash_policy_is_refused_by_the_coordinator() {
    install_props();
    let Some(db) = PgDb::create("policymismatch") else { return };
    let map = land_map();
    let dir = scratch("policymismatch");
    let cert_file = dir.join("coordinator.cert-sha256");
    let store = db.open();
    let registered = register_host(&store, h(0), 1);
    let start = ensure_coordinator_state(&store, &map, None).unwrap().expect("a history");
    let cuts = coordinator_rt::WorldCuts { every: Duration::from_secs(3600), tick: Box::new(|| 0) };
    let listening = coordinator_rt::start_listening_as(
        store.clone(),
        start,
        "127.0.0.1:0".parse().unwrap(),
        TOKEN.into(),
        Some(cert_file.clone()),
        Fenced::default(),
        Some(cuts),
        game::CrashPolicy::Rollback,
        "",
        None,
    )
    .expect("the coordinator listens");
    let (link, events) = raw_host_link(listening.addr, &cert_file, 1, registered.id.0);
    link.send(bare_attach(registered.id, registered.generation, 1, game::CrashPolicy::Succession)).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(svc_link::LinkEvent::Down(_)) => break,
            Ok(svc_link::LinkEvent::Msg(game::CoordMsg::Attached { .. })) => {
                panic!("a `claim` host attached to a coordinator that never declares a succession")
            }
            _ => assert!(Instant::now() < deadline, "the mismatched host's link was never closed"),
        }
    }
    drop(link);
    // The coordinator's own policy attaches.
    let (link, events) = raw_host_link(listening.addr, &cert_file, 1, registered.id.0);
    link.send(bare_attach(registered.id, registered.generation, 1, game::CrashPolicy::Rollback)).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(svc_link::LinkEvent::Msg(game::CoordMsg::Attached { epoch: 1 })) => break,
            _ => assert!(Instant::now() < deadline, "the matching host's Attach was never echoed"),
        }
    }
    drop(link);
    assert_eq!(listening.thread.shutdown(Duration::from_secs(10)), ServerExit::Clean);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host whose `Attach` runs an OLDER world generation than the store's is refused — and
/// told the generation (`CoordMsg::Generation`), so the stale process exits for the world
/// rollback instead of redialing for ever when no peer tells it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_host_of_an_older_generation_is_told_the_generation() {
    install_props();
    let Some(db) = PgDb::create("oldgen") else { return };
    let map = land_map();
    let dir = scratch("oldgen");
    let cert_file = dir.join("coordinator.cert-sha256");
    let store = db.open();
    // A process restarted under its name, nobody else live: a total crash raises the
    // generation (a new id takes the old row's land).
    let (first, second) = (register_host(&store, h(0), 1), register_host(&store, h(0), 2));
    assert!(second.generation > first.generation);
    let (coordinator, addr) = Coordinator::start(&db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
    let (link, events) = raw_host_link(addr, &cert_file, 2, second.id.0);
    link.send(bare_attach(second.id, first.generation, 2, game::CrashPolicy::Rollback)).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(svc_link::LinkEvent::Msg(game::CoordMsg::Generation(g))) => {
                assert_eq!(g, second.generation);
                break;
            }
            Ok(svc_link::LinkEvent::Msg(game::CoordMsg::Attached { .. })) => panic!("an older generation attached"),
            _ => assert!(Instant::now() < deadline, "the stale host was never told the world generation"),
        }
    }
    drop(link);
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

/// What must hold at every moment: at most one body per person, none for the unknown. (A
/// body of someone the history has just let die stands until its despawn directive arrives:
/// none for the dead is [`settled`]'s.)
fn never_twice(h: &History, bodies: &BTreeMap<PersonId, Vec<RegionCoords>>) -> Result<(), String> {
    for (p, at) in bodies {
        if at.len() > 1 {
            return Err(format!("{p:?} has bodies in {at:?}"));
        }
        if h.person(*p).is_none() {
            return Err(format!("a body for {p:?}, whom the history does not know"));
        }
    }
    Ok(())
}

/// The settled agreement: [`never_twice`]; no body for the dead; every living person
/// embodied in the history has their one body where the history has it, everyone else none;
/// and the played region's population is its site's — no living member of `site` is left
/// abstract at it while `played` runs (a resident left abstract in a running region is a
/// person missing), and someone stands there.
fn settled(
    h: &History,
    bodies: &BTreeMap<PersonId, Vec<RegionCoords>>,
    site: SiteId,
    played: RegionCoords,
) -> Result<(), String> {
    never_twice(h, bodies)?;
    if let Some(p) = bodies.keys().find(|p| !h.person(**p).is_some_and(|q| q.alive())) {
        return Err(format!("a body for {p:?}, who is not alive"));
    }
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
    h.site(site).ok_or("the played site is gone")?;
    let left: Vec<PersonId> = h
        .living_members(site)
        .into_iter()
        .filter(|p| h.person(*p).is_some_and(|q| q.location == Location::AtSite(site)))
        .collect();
    if !left.is_empty() {
        return Err(format!("site members {left:?} left abstract while {played:?} runs"));
    }
    if !bodies.values().any(|at| at.contains(&played)) {
        return Err(format!("nobody stands in {played:?}"));
    }
    Ok(())
}

/// An adventure at the smallest populated site with a young married couple among its members
/// (both alive, she at most 35): few bodies to run, and children to come.
fn start_at_young_couple(h: &History) -> Start {
    let young_couple = |site: SiteId| {
        h.living_members(site).into_iter().any(|m| {
            let q = h.person(m).unwrap();
            q.sex == Sex::Female
                && q.age_years(h.now()) <= 35
                && q.spouse.is_some_and(|f| h.living_members(site).contains(&f))
        })
    };
    let s = h
        .sites()
        .filter(|s| populated(h, s.id) && young_couple(s.id))
        .min_by_key(|s| h.living_members(s.id).len())
        .expect("a populated site with a young couple");
    Start { site: s.id, region: s.region }
}

/// The births logged at `site` after `after`, each child once: (child, birth tick).
fn births_at(h: &History, site: SiteId, after: CalendarTick) -> Result<Vec<(PersonId, CalendarTick)>, String> {
    let mut seen = BTreeSet::new();
    for e in h.log() {
        if let LogKind::Born { person, site: Some(s), .. } = e.kind {
            if s == site && !seen.insert(person) {
                return Err(format!("{person:?} born twice"));
            }
        }
    }
    Ok(seen
        .into_iter()
        .filter_map(|p| h.person(p).map(|q| (p, q.born)))
        .filter(|(_, born)| *born > after)
        .collect())
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
    fn until_settled(&mut self, coordinator: &Coordinator, when: &str, site: SiteId, played: RegionCoords) {
        let deadline = Instant::now() + WAIT;
        loop {
            let census = bodies(&mut self.manager);
            let result = coordinator
                .thread
                .inspect(move |c| settled(c.history(), &census, site, played).map_err(|e| (e, never_twice(c.history(), &census))), WAIT)
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

/// The coordinator process crashes while a host runs a populated site's region (its
/// player's adventure there) — after a save, and after a birth directive (seed 0: killed at
/// once, the birth's save racing the crash; seed 1: after the child's body stands; seed 2:
/// with saves failing, so the birth was never durable) — and restarts: after the host
/// re-attaches every living person has exactly one body (the site's residents in the running
/// region) or is abstract elsewhere, no dead person has a body, and no birth is applied twice.
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
    let adventure = c1.thread.inspect(|c| start_at_young_couple(c.history()), WAIT).expect("the coordinator answers");
    let host_store = db.open();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
    let (a, _) = StorePlayers(host_store.clone()).resolve(&a_key).unwrap();
    let link = |reg| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file.clone(), h(0), 1, reg)) };
    let mut host = Host::start(&host_store, &map, |me| Box::new(alone(me)), link, None, 1);
    host.until("attached", |h| h.manager.world_attached());
    host.send(ServerEvent::ClientConnected { client: A, player: a, record: None });
    host.send(ServerEvent::ClientPacket(adventure.packet(), A));
    // The start's reply is gated: once it is here, its save committed.
    host.until("started", |h| !h.adventure_results(A).is_empty());
    let site = host.adventure_results(A)[0].unwrap_or_else(|e| panic!("seed {seed}: start refused: {e:?}"));
    assert_eq!(site, adventure.site);
    let played = adventure.region;
    host.until_settled(&c1, &format!("seed {seed}: the played site"), site, played);

    // A birth: the history runs on (month by month) until a resident of the played region
    // has a child, who gets a body beside her.
    if seed == 2 {
        c1.store.fail_next_writes(1_000_000);
    }
    let (from, born, child) = c1
        .thread
        .with_core(
            move |c| {
                let h = c.history_mut();
                let from = h.now();
                let embodied_birth = |h: &History| {
                    births_at(h, site, from)
                        .unwrap()
                        .into_iter()
                        .map(|(p, _)| p)
                        .find(|p| h.person(*p).is_some_and(|q| q.alive() && q.location == Location::InRegion(played)))
                };
                for _ in 0..12 * 30 {
                    if embodied_birth(h).is_some() {
                        break;
                    }
                    let next = CalendarTick(h.now().0 + 28 * game::history::TICKS_PER_DAY);
                    h.advance(next);
                }
                (from, births_at(h, site, from).unwrap(), embodied_birth(h))
            },
            WAIT,
        )
        .expect("the coordinator answers");
    let child = child.unwrap_or_else(|| panic!("seed {seed}: no child born to a resident of {played:?} within 30 years"));
    match seed {
        0 => {}
        1 => {
            host.until("the child's body", |h| bodies(&mut h.manager).get(&child).is_some());
            for _ in 0..10 {
                host.round();
            }
        }
        _ => {
            for _ in 0..40 {
                host.round();
                let census = bodies(&mut host.manager);
                assert!(born.iter().all(|(p, _)| !census.contains_key(p)), "seed {seed}: a body for a birth never saved");
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
    host.until_settled(&c2, &format!("seed {seed}: after the restart"), site, played);
    for _ in 0..20 {
        host.round();
    }
    host.until_settled(&c2, &format!("seed {seed}: settled after the restart"), site, played);
    let born_before: BTreeSet<(PersonId, CalendarTick)> = born.iter().copied().collect();
    c2.check(&format!("seed {seed}: births"), move |h| {
        // A birth the restarted history made on its own (a day's turn) has another tick: only
        // the births of the run-on count.
        let now: BTreeSet<(PersonId, CalendarTick)> = births_at(h, site, from)?.into_iter().collect();
        let kept: BTreeSet<_> = now.intersection(&born_before).copied().collect();
        match seed {
            // Never durable: the restart's history has no such birth.
            2 => kept.is_empty().then_some(()).ok_or(format!("a birth never saved came back: {kept:?}")),
            // Durable (the save raced the crash) or not; never twice.
            0 => (kept.is_empty() || kept == born_before).then_some(()).ok_or(format!("{kept:?} vs {born_before:?}")),
            _ => (kept == born_before).then_some(()).ok_or(format!("{kept:?} vs {born_before:?}")),
        }
    });
    let results = host.adventure_results(A);
    assert_eq!(results.len(), 1, "seed {seed}: one AdventureResult: {results:?}");
    assert_eq!(host.exit(), ServerExit::Clean);
    host_store.shutdown();
    c2.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The coordinator is down (crashed) when the client starts an adventure — and the client
/// connected during the downtime too, so its region started unannounced: the request waits
/// in the outbox; the coordinator restarts within 10 s and the adventure is granted exactly
/// once: one `AdventureResult(Ok)` arrives, one claim is saved.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn start_during_downtime_completes_once() {
    install_props();
    let Some(db) = PgDb::create("downtime") else { return };
    let map = land_map();
    let dir = scratch("downtime");
    let cert_file = dir.join("coordinator.cert-sha256");
    let (c1, addr) = Coordinator::start(&db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
    let adventure = c1.thread.inspect(|c| start_request(c.history()), WAIT).expect("the coordinator answers");
    let host_store = db.open();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
    let (a, _) = StorePlayers(host_store.clone()).resolve(&a_key).unwrap();
    let link = |reg| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file.clone(), h(0), 1, reg)) };
    let mut host = Host::start(&host_store, &map, |me| Box::new(alone(me)), link, None, 1);
    host.until("attached", |h| h.manager.world_attached());

    c1.kill();
    host.until("the link drops", |h| !h.manager.world_attached());
    let down_at = Instant::now();
    host.send(ServerEvent::ClientConnected { client: A, player: a, record: None });
    for _ in 0..10 {
        host.round();
    }
    host.send(ServerEvent::ClientPacket(adventure.packet(), A));
    for _ in 0..20 {
        host.round();
    }
    assert!(host.adventure_results(A).is_empty(), "nothing answers while the coordinator is down");
    assert_eq!(host.manager.pending_world_requests(), 1, "the start waits");

    let (c2, _) = Coordinator::start(&db, &map, addr, cert_file);
    assert!(down_at.elapsed() < Duration::from_secs(10), "the coordinator is back within 10 s");
    host.until("re-attached", |h| h.manager.world_attached());
    host.until("the start answered", |h| !h.adventure_results(A).is_empty());
    for _ in 0..20 {
        host.round();
    }
    let results = host.adventure_results(A);
    assert_eq!(results, vec![Ok(adventure.site)], "one AdventureResult");
    host.until_settled(&c2, "after the start", adventure.site, adventure.region);
    let granted = vec![(a, Claim::Adventure(adventure.site))];
    assert_eq!(c2.thread.inspect(|c| c.claims(), WAIT).unwrap(), granted, "one claim granted");
    assert_eq!(host.manager.pending_world_requests(), 0, "nothing waits");
    assert_eq!(host.exit(), ServerExit::Clean);
    host_store.shutdown();
    c2.stop();
    let store = db.open();
    assert_eq!(stored_claims(&store, &map), granted, "and saved");
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The start's reply was decided and its save committed, but the coordinator crashed before
/// the reply left: after the restart the host's replayed request is answered from the saved
/// memo — not decided again (a second decision would refuse it `AlreadyPlaying`): one
/// `AdventureResult(Ok)` of that site, the one claim.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn start_answered_then_crash_is_not_granted_twice() {
    install_props();
    let Some(db) = PgDb::create("memo") else { return };
    let map = land_map();
    let dir = scratch("memo");
    let cert_file = dir.join("coordinator.cert-sha256");
    let (c1, addr) = Coordinator::start(&db, &map, "127.0.0.1:0".parse().unwrap(), cert_file.clone());
    let adventure = c1.thread.inspect(|c| start_request(c.history()), WAIT).expect("the coordinator answers");
    let host_store = db.open();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
    let (a, _) = StorePlayers(host_store.clone()).resolve(&a_key).unwrap();
    let link = |reg| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file.clone(), h(0), 1, reg)) };
    let mut host = Host::start(&host_store, &map, |me| Box::new(alone(me)), link, None, 1);
    host.until("attached", |h| h.manager.world_attached());
    host.send(ServerEvent::ClientConnected { client: A, player: a, record: None });
    for _ in 0..10 {
        host.round();
    }

    c1.thread.crash_before_claim_reply();
    host.send(ServerEvent::ClientPacket(adventure.packet(), A));
    host.until("the coordinator crashed", |_| c1.thread.is_finished());
    c1.kill();
    let saved = {
        let store = db.open();
        let claims = stored_claims(&store, &map);
        store.shutdown();
        claims
    };
    assert_eq!(saved, vec![(a, Claim::Adventure(adventure.site))], "the grant's save committed before the crash");
    host.until("the link drops", |h| !h.manager.world_attached());
    assert!(host.adventure_results(A).is_empty(), "the reply never left");

    let (c2, _) = Coordinator::start(&db, &map, addr, cert_file);
    host.until("re-attached", |h| h.manager.world_attached());
    host.until("the start answered", |h| !h.adventure_results(A).is_empty());
    for _ in 0..20 {
        host.round();
    }
    assert_eq!(host.adventure_results(A), vec![Ok(adventure.site)], "one AdventureResult, of the saved grant");
    host.until_settled(&c2, "after the replayed start", adventure.site, adventure.region);
    assert_eq!(c2.thread.inspect(|c| c.claims(), WAIT).unwrap(), saved, "not granted again");
    assert_eq!(host.exit(), ServerExit::Clean);
    host_store.shutdown();
    c2.stop();
    let store = db.open();
    assert_eq!(stored_claims(&store, &map), saved, "one claim saved");
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The coordinator stays down longer than the request timeout (shortened here by the
/// manager's test dial): the start is answered `WorldUnavailable` once and forgotten — no
/// claim after the coordinator returns, no late grant — and a later start works.
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
    let adventure = c1.thread.inspect(|c| start_request(c.history()), WAIT).expect("the coordinator answers");
    let host_store = db.open();
    let a_key = PlayerKey::Local(game::PlayerName::parse("a").unwrap());
    let (a, _) = StorePlayers(host_store.clone()).resolve(&a_key).unwrap();
    let link = |reg| -> Box<dyn WorldLink + Send> { Box::new(quic_link(addr, cert_file.clone(), h(0), 1, reg)) };
    let mut host = Host::start(&host_store, &map, |me| Box::new(alone(me)), link, None, 1);
    host.manager.set_world_request_timeout_ms(TIMEOUT_MS);
    host.until("attached", |h| h.manager.world_attached());
    host.send(ServerEvent::ClientConnected { client: A, player: a, record: None });
    for _ in 0..10 {
        host.round();
    }

    c1.kill();
    host.until("the link drops", |h| !h.manager.world_attached());
    host.send(ServerEvent::ClientPacket(adventure.packet(), A));
    let asked = Instant::now();
    host.until("the start times out", |h| !h.adventure_results(A).is_empty());
    assert!(asked.elapsed() >= Duration::from_millis(TIMEOUT_MS), "answered no earlier than the timeout");
    let until = Instant::now() + Duration::from_millis(2 * TIMEOUT_MS);
    while Instant::now() < until {
        host.round();
    }
    assert_eq!(host.adventure_results(A), vec![Err(AdventureRefusal::WorldUnavailable)], "WorldUnavailable, once");
    assert_eq!(host.manager.pending_world_requests(), 0, "the request is forgotten");

    let (c2, _) = Coordinator::start(&db, &map, addr, cert_file);
    host.until("re-attached", |h| h.manager.world_attached());
    for _ in 0..40 {
        host.round();
    }
    assert_eq!(host.adventure_results(A).len(), 1, "no late answer to the timed-out start");
    assert!(c2.thread.inspect(|c| c.claims(), WAIT).unwrap().is_empty(), "no late grant");
    host.send(ServerEvent::ClientPacket(adventure.packet(), A));
    host.until("the second start answered", |h| h.adventure_results(A).len() == 2);
    assert_eq!(host.adventure_results(A)[1], Ok(adventure.site), "the later start works");
    assert_eq!(c2.thread.inspect(|c| c.claims(), WAIT).unwrap(), vec![(a, Claim::Adventure(adventure.site))]);
    assert_eq!(host.exit(), ServerExit::Clean);
    host_store.shutdown();
    c2.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- Elastic 2a Task 6: every change is written before anything it decided is sent -------

/// The player a raw host reports online, and the placement hop it reports it at.
const RAW_PLAYER: PlayerId = PlayerId(1);
const RAW_HOP: u64 = 1;

/// A raw host's `Attach` of `epoch`: its player online, a claim-changing request of it
/// unsettled when `pending` (a host that sent one and has no answer yet keeps it pending),
/// `pending_cut` as the host's last unanswered cut.
fn raw_attach(host: &world_store::Registered, epoch: u64, pending: bool, pending_cut: Option<game::CutId>) -> game::HostMsg {
    let online = vec![game::OnlinePlayer { player: RAW_PLAYER, claim: None, claim_pending: pending, report: epoch, hop: RAW_HOP }];
    attach_msg(host.id, epoch, online, host.generation, 1, pending_cut, game::CrashPolicy::Rollback)
}

/// The raw host's request `seq`: its player's adventure at `start`'s site.
fn raw_start(host: &world_store::Registered, seq: u64, start: Start) -> (game::RequestId, game::HostMsg) {
    let id = game::RequestId { host: host.id, boot: 1, seq };
    (id, game::HostMsg::Request { id, player: RAW_PLAYER, hop: RAW_HOP, req: game::WorldRequest::StartAdventure(start.site) })
}

/// The coordinator state the store holds, loaded as a restart loads it.
fn stored_core(store: &StoreHandle, map: &WorldMap) -> game::Coordinator {
    coordinator_common::stored_core(store, map).expect("a written coordinator state")
}

/// A coordinator listening for one host whose link is read raw: no `WorldManager`, so what
/// the coordinator sends — and when — is seen as it is. Registered `static` (no lease is
/// beaten, no succession is ever declared). With `cuts` the coordinator runs world cuts every
/// 300 ms on a world tick that continues across its restarts.
struct RawWorld {
    db: PgDb,
    map: WorldMap,
    dir: PathBuf,
    cert_file: PathBuf,
    addr: std::net::SocketAddr,
    cuts: Option<Instant>,
    coordinator: Option<Coordinator>,
    host: world_store::Registered,
    link: crossbeam::channel::Sender<game::HostMsg>,
    events: Receiver<svc_link::LinkEvent<game::CoordMsg>>,
    /// Every message read since the start (or the last restart).
    seen: Vec<game::CoordMsg>,
}

impl RawWorld {
    fn new(tag: &str, cuts: bool) -> Option<RawWorld> {
        install_props();
        let db = PgDb::create(tag)?;
        let map = land_map();
        let dir = scratch(tag);
        let cert_file = dir.join("coordinator.cert-sha256");
        let store = db.open();
        let host = register_host(&store, h(0), 1);
        store.shutdown();
        let cuts = cuts.then(Instant::now);
        let (coordinator, addr) = Self::start(&db, &map, "127.0.0.1:0".parse().unwrap(), &cert_file, cuts);
        let (link, events) = raw_host_link(addr, &cert_file, 1, host.id.0);
        let mut w = RawWorld { db, map, dir, cert_file, addr, cuts, coordinator: Some(coordinator), host, link, events, seen: Vec::new() };
        let attach = raw_attach(&w.host, 1, false, None);
        w.attach(attach);
        Some(w)
    }

    fn start(
        db: &PgDb,
        map: &WorldMap,
        bind: std::net::SocketAddr,
        cert_file: &std::path::Path,
        cuts: Option<Instant>,
    ) -> (Coordinator, std::net::SocketAddr) {
        let store = db.open();
        let start = ensure_coordinator_state(&store, map, None).unwrap().expect("a history");
        let cuts = cuts.map(|t0| coordinator_rt::WorldCuts {
            every: Duration::from_millis(300),
            tick: Box::new(move || t0.elapsed().as_millis() as u64 / game::TICK_RATE),
        });
        let listening = coordinator_rt::start_listening_as(
            store.clone(),
            start,
            bind,
            TOKEN.into(),
            Some(cert_file.to_path_buf()),
            Fenced::default(),
            cuts,
            game::CrashPolicy::Rollback,
            "",
            None,
        )
        .expect("the coordinator listens");
        (Coordinator { thread: listening.thread, store }, listening.addr)
    }

    fn thread(&self) -> &CoordinatorThread {
        &self.coordinator.as_ref().expect("the coordinator runs").thread
    }

    /// Sends `attach` and reads until its echo.
    fn attach(&mut self, attach: game::HostMsg) {
        let game::HostMsg::Attach { epoch, .. } = attach else { unreachable!("an Attach") };
        self.link.send(attach).unwrap();
        self.read_until("the attach echo", |m| matches!(m, game::CoordMsg::Attached { epoch: e } if *e == epoch));
    }

    /// Reads whatever arrives for `d`.
    fn read_for(&mut self, d: Duration) {
        let until = Instant::now() + d;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(svc_link::LinkEvent::Msg(m)) => self.seen.push(m),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }

    /// Reads until a message `pred` holds of arrives (at most [`WAIT`]); that message.
    fn read_until(&mut self, what: &str, pred: impl Fn(&game::CoordMsg) -> bool) -> game::CoordMsg {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.checked_duration_since(Instant::now()).unwrap_or_else(|| panic!("timed out waiting: {what}"));
            if let Ok(svc_link::LinkEvent::Msg(m)) = self.events.recv_timeout(left) {
                self.seen.push(m.clone());
                if pred(&m) {
                    return m;
                }
            }
        }
    }

    fn replies(&self, id: game::RequestId) -> Vec<game::WorldReply> {
        self.seen
            .iter()
            .filter_map(|m| match m {
                game::CoordMsg::Reply { id: got, reply } if *got == id => Some(reply.clone()),
                _ => None,
            })
            .collect()
    }

    /// Until `done` of the running core holds (at most [`WAIT`]).
    fn until_core(&self, what: &str, done: impl Fn(&game::Coordinator) -> bool + Send + Clone + 'static) {
        let deadline = Instant::now() + WAIT;
        while !self.thread().inspect(done.clone(), WAIT).expect("the coordinator answers") {
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The coordinator's next transition is the last thing it does: it commits, and the thread
    /// ends before anything that transition released leaves. Writes fail while `prepare` runs
    /// (it decides what that transition holds), then resume.
    fn kill_after_the_commit(&mut self, prepare: impl FnOnce(&mut RawWorld)) {
        self.thread().fail_coordinator_writes(true);
        prepare(self);
        self.thread().kill_after_commit_before_send();
        self.thread().fail_coordinator_writes(false);
        let deadline = Instant::now() + WAIT;
        while !self.thread().is_finished() {
            assert!(Instant::now() < deadline, "the coordinator never committed its transition");
            std::thread::sleep(Duration::from_millis(10));
        }
        self.coordinator.take().expect("it ran").kill();
    }

    /// The coordinator starts again on the same address and pin; the raw link comes up again.
    fn restart(&mut self) {
        let (c, _) = Self::start(&self.db, &self.map, self.addr, &self.cert_file, self.cuts);
        self.coordinator = Some(c);
        self.seen.clear();
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.checked_duration_since(Instant::now()).expect("the raw link never came up again");
            if let Ok(svc_link::LinkEvent::Up(_)) = self.events.recv_timeout(left) {
                break;
            }
        }
    }

    fn stop(mut self) {
        self.coordinator.take().expect("the coordinator runs").stop();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Elastic 2a: with the coordinator's writes failing, nothing it decides leaves — a host's
/// `Request` gets no `Reply`, a `RegionStarted` no `SpawnPeople`, though the core decided both.
/// Once writes commit again exactly one `Reply` arrives, and the stored memo holds it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_output_leaves_before_its_state_is_written() {
    let Some(mut w) = RawWorld::new("e2aheld", false) else { return };
    let start = w.thread().inspect(|c| start_request(c.history()), WAIT).expect("the coordinator answers");
    let (id, ask) = raw_start(&w.host, 1, start);
    w.thread().fail_coordinator_writes(true);
    w.link.send(game::HostMsg::RegionStarted { rc: start.region, incarnation: 1 }).unwrap();
    w.link.send(ask).unwrap();
    w.until_core("the grant decided", move |c| c.memo_ids().contains(&id));
    w.read_for(Duration::from_millis(1_500));
    assert!(w.replies(id).is_empty(), "a Reply left before its state was written: {:?}", w.seen);
    assert!(
        !w.seen.iter().any(|m| matches!(m, game::CoordMsg::SpawnPeople { .. })),
        "SpawnPeople left before its state was written: {:?}",
        w.seen
    );
    assert!(!stored_core(&w.coordinator.as_ref().unwrap().store, &w.map).memo_ids().contains(&id), "nothing was written");

    w.thread().fail_coordinator_writes(false);
    w.read_until("the reply", |m| matches!(m, game::CoordMsg::Reply { id: got, .. } if *got == id));
    let spawn = move |m: &game::CoordMsg| matches!(m, game::CoordMsg::SpawnPeople { rc, .. } if *rc == start.region);
    if !w.seen.iter().any(spawn) {
        w.read_until("the region's people", spawn);
    }
    w.read_for(Duration::from_millis(500));
    let replies = w.replies(id);
    assert_eq!(replies.len(), 1, "exactly one Reply: {replies:?}");
    assert_eq!(replies, vec![game::WorldReply::AdventureStarted { site: start.site, region: start.region }]);
    assert!(stored_core(&w.coordinator.as_ref().unwrap().store, &w.map).memo_ids().contains(&id), "the stored memo holds it");
    w.stop();
}

/// Elastic 2a: the coordinator dies right after the commit of a transition that granted a
/// claim and recorded a world cut's decision, before either message left. After the restart
/// the host re-attaches (its cut unanswered); its resent `Request` is answered from the memo,
/// the same reply, with no second grant, and the lost `CutDone` is answered as the store
/// decided it. Nobody is lost or doubled: every person the history had is still there, each
/// in at most one site.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_coordinator_killed_after_the_commit_before_the_sends_loses_nothing() {
    let Some(mut w) = RawWorld::new("e2akill", true) else { return };
    let start = w.thread().inspect(|c| start_request(c.history()), WAIT).expect("the coordinator answers");
    let people_before: BTreeSet<PersonId> =
        w.thread().inspect(|c| c.history().people().map(|p| p.id).collect(), WAIT).expect("the coordinator answers");
    let cut = match w.read_until("a world cut begins", |m| matches!(m, game::CoordMsg::BeginCut { .. })) {
        game::CoordMsg::BeginCut { cut, .. } => cut,
        _ => unreachable!(),
    };
    let (id, ask) = raw_start(&w.host, 1, start);
    w.kill_after_the_commit(|w| {
        // Its part "prepared" (none is staged: the store records an abort), and the start.
        w.link.send(game::HostMsg::CutPrepared { cut }).unwrap();
        w.link.send(ask.clone()).unwrap();
        w.until_core("the grant decided", move |c| c.memo_ids().contains(&id));
        // The cut's decision is taken in the core; it is recorded only with the transition that
        // carries it (Elastic 2b Task 3) — not while the writes fail.
        w.until_core("the cut's verdict", move |c| c.world_cut_progress().is_some_and(|(c, _, v)| c == cut && v.is_some()));
        w.read_for(Duration::from_millis(600));
        assert_eq!(
            scalar(&w.db, &format!("SELECT COUNT(*) FROM cut_decisions WHERE cut_id = {}", cut.0)),
            0,
            "no decision without its transition"
        );
    });
    // The killed transition carried the grant AND the cut's decision: both committed together.
    assert_eq!(
        scalar(&w.db, &format!("SELECT COUNT(*) FROM cut_decisions WHERE cut_id = {}", cut.0)),
        1,
        "the decision committed with its transition"
    );
    w.read_for(Duration::from_millis(300));
    assert!(w.replies(id).is_empty(), "the reply never left: {:?}", w.seen);
    assert!(
        !w.seen.iter().any(|m| matches!(m, game::CoordMsg::CutDone { cut: c, .. } if *c == cut)),
        "the CutDone never left: {:?}",
        w.seen
    );
    let decided = scalar(&w.db, &format!("SELECT committed::bigint FROM cut_decisions WHERE cut_id = {}", cut.0)) != 0;
    let granted = vec![(RAW_PLAYER, Claim::Adventure(start.site))];
    let (claims, memo) = {
        let store = w.db.open();
        let c = stored_core(&store, &w.map);
        store.shutdown();
        (c.claims(), c.memo_ids())
    };
    assert_eq!(claims, granted, "the grant was committed");
    assert!(memo.contains(&id), "with its reply memoised");

    w.restart();
    w.attach(raw_attach(&w.host, 2, true, Some(cut)));
    w.link.send(ask).unwrap();
    let done = w.read_until("the lost CutDone", |m| matches!(m, game::CoordMsg::CutDone { cut: c, .. } if *c == cut));
    assert!(matches!(done, game::CoordMsg::CutDone { committed, .. } if committed == decided), "{done:?} vs the store's {decided}");
    w.read_until("the replayed reply", |m| matches!(m, game::CoordMsg::Reply { id: got, .. } if *got == id));
    w.read_for(Duration::from_millis(1_000));
    assert_eq!(
        w.replies(id),
        vec![game::WorldReply::AdventureStarted { site: start.site, region: start.region }],
        "one reply, from the memo (a second decision would refuse it AlreadyPlaying)"
    );
    let (claims, people, twice) = w
        .thread()
        .inspect(
            |c| {
                let h = c.history();
                let mut member: BTreeMap<PersonId, usize> = BTreeMap::new();
                for s in h.sites() {
                    for p in &s.members {
                        *member.entry(*p).or_default() += 1;
                    }
                }
                let twice: Vec<PersonId> = member.into_iter().filter(|(_, n)| *n > 1).map(|(p, _)| p).collect();
                (c.claims(), h.people().map(|p| p.id).collect::<BTreeSet<PersonId>>(), twice)
            },
            WAIT,
        )
        .expect("the coordinator answers");
    assert_eq!(claims, granted, "no second grant: the one claim");
    assert!(people_before.is_subset(&people), "nobody lost: {:?}", people_before.difference(&people).collect::<Vec<_>>());
    assert!(twice.is_empty(), "in two sites: {twice:?}");
    w.stop();
}

/// Elastic 2a: a restart resumes from the last committed transition — the calendar tick (and
/// anchor), the site markers and the legends' length are those of the transition written
/// moments before the crash (an adventure's grant, then years of history), not of a periodic save.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_restart_resumes_exactly_where_the_last_transition_left() {
    let Some(mut w) = RawWorld::new("e2aresume", false) else { return };
    let start = w.thread().inspect(|c| start_request(c.history()), WAIT).expect("the coordinator answers");
    let (id, ask) = raw_start(&w.host, 1, start);
    w.link.send(ask).unwrap();
    w.read_until("the reply", |m| matches!(m, game::CoordMsg::Reply { id: got, .. } if *got == id));
    let changed_at = Instant::now();
    let mut decided = None;
    w.kill_after_the_commit(|w| {
        let target = w
            .thread()
            .with_core(
                |c| {
                    let h = c.history_mut();
                    h.advance(CalendarTick(h.now().0 + 3 * game::history::DAYS_PER_YEAR * game::history::TICKS_PER_DAY));
                    h.now()
                },
                WAIT,
            )
            .expect("the coordinator answers");
        decided = Some((target, w.thread().inspect(resumed, WAIT).expect("the coordinator answers")));
    });
    let (target, decided) = decided.expect("prepared");
    assert!(changed_at.elapsed() < Duration::from_secs(20), "the change was written within seconds");

    let store = w.db.open();
    let last = resumed(&stored_core(&store, &w.map));
    // The transition holds the change made just before the crash (and anything the core decided
    // after it, before its write: a day's turn at most).
    assert!(last.now >= target && last.legends >= decided.legends && last.now >= decided.now, "{last:?} vs {decided:?}");
    assert_eq!(last.sites, decided.sites);
    // What a restart runs from is exactly that transition.
    let start = ensure_coordinator_state(&store, &w.map, None).unwrap().expect("a history");
    assert_eq!(resumed(&start.coordinator), last);
    drop(start);
    store.shutdown();
    w.restart();
    w.attach(raw_attach(&w.host, 2, false, None));
    w.stop();
}

/// A host id from its number.
#[allow(dead_code)]
fn h(n: u64) -> game::HostId {
    game::HostId(n)
}
