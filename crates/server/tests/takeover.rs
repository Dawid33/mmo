//! R4 Task 10: the real-process crash table of a takeover. Each host is a "process" in this
//! test process — its own store handle, claims, `WorldManager<InlineSpawner>` (its regions
//! read from Postgres as a region thread reads them), lease heartbeat, claim keeper and
//! adoption reader, pumped every round as `server::run`'s main loop pumps them — over real
//! peer links and a real coordinator runtime (`CrashPolicy::Succession`, `claim`) on
//! localhost, against Postgres (`MMO_TEST_DATABASE_URL`; skipped with a message when unset).
//! The lease is shortened for every party at once (store, coordinator watch, heartbeat:
//! [`LEASE_MS`], [`BEAT_MS`]) so a death is declared in seconds. Its own test binary:
//! process-global block props, real threads and sockets. Every test is slow (`just test-r4`).
//!
//! The rows of the crash table (plan Task 10) each have a test below; a failure names what
//! replays it (the round, the kill point).

#![allow(dead_code, unused_imports)]

include!("common/hosts_rig.rs");

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The host lease of these tests (every party: the store's checks, the coordinator's watch).
const LEASE_MS: u64 = 3_000;
/// Between two heartbeats.
const BEAT_MS: u64 = 500;
/// A succession's deadline (the product's).
const DEADLINE_MS: u64 = game::SUCCESSION_DEADLINE_MS;
/// Longer waits than [`WAIT`]: a succession, then its settle cut.
const LONG: Duration = Duration::from_secs(60);

// ---- the store, as each process opens it ----------------------------------------------

/// A store handle as one process of these tests opens it: the shortened lease.
fn open(db: &PgDb) -> StoreHandle {
    let store = db.open();
    store.set_host_lease_ms(LEASE_MS);
    store
}

fn count(db: &PgDb, sql: &str) -> i64 {
    query_i64s(db, sql).first().copied().unwrap_or(0)
}

/// Rows of `id` in `entities`.
fn stored_rows(db: &PgDb, id: EntityId) -> Vec<RegionCoords> {
    let xs = query_i64s(
        db,
        &format!(
            "SELECT rx FROM entities WHERE birth_rx = {} AND birth_rz = {} AND serial = {} ORDER BY rx, rz",
            id.birth_region.x, id.birth_region.z, id.serial
        ),
    );
    let zs = query_i64s(
        db,
        &format!(
            "SELECT rz FROM entities WHERE birth_rx = {} AND birth_rz = {} AND serial = {} ORDER BY rx, rz",
            id.birth_region.x, id.birth_region.z, id.serial
        ),
    );
    xs.into_iter().zip(zs).map(|(x, z)| RegionCoords::new(x as i32, z as i32)).collect()
}

/// In-flight transfer rows of `id` (every host's).
fn in_flight_rows(store: &StoreHandle, id: EntityId) -> usize {
    store
        .load_in_flight(None)
        .unwrap()
        .iter()
        .filter(|r| matches!(r, game::InFlightRow::Transfer { bundle, .. } if bundle.id == id))
        .count()
}

/// The successions: `(id, predecessor, heir, base_cut, settled_cut or -1, abandoned)`.
fn successions(db: &PgDb) -> Vec<[i64; 6]> {
    let col = |c: &str| query_i64s(db, &format!("SELECT {c} FROM successions ORDER BY id"));
    let (id, p, h, b, s, a) =
        (col("id"), col("predecessor"), col("heir"), col("base_cut"), col("COALESCE(settled_cut, -1)"), col("abandoned"));
    (0..id.len()).map(|i| [id[i], p[i], h[i], b[i], s[i], a[i]]).collect()
}

/// `(boot, clean, state)` of host `h`'s row.
fn host_row(db: &PgDb, h: u32) -> (i64, i64, i64) {
    let one = |c: &str| count(db, &format!("SELECT {c} FROM hosts WHERE ordinal = {h}"));
    (one("boot"), one("clean"), one("state"))
}

// ---- hosts under `claim` ----------------------------------------------------------------

/// A host process's boot under `claim`, up to its registration (which may wait for its
/// predecessor's succession: run it on its own thread then): registered with an empty
/// fence, then its own rows re-claimed — `server::run`'s order.
struct Booted {
    ordinal: u32,
    boot: u64,
    links: PeerLinks,
    claims: HostClaims,
}

fn register(db: &PgDb, dir: &std::path::Path, n: u32, ordinal: u32, boot: u64) -> Booted {
    let store = open(db);
    let claims = HostClaims::none(store.clone(), ordinal);
    let config = PeerLinkConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        cert_hash_file: dir.join(format!("host-{ordinal}-{boot}.peer.cert-sha256")),
        delay: Duration::ZERO,
    };
    let stop = AtomicBool::new(false);
    let links = PeerLinks::start(ordinal, n, boot, endpoint(ordinal), TOKEN.into(), store, &claims.fence(), config, CrashPolicy::Succession, &stop)
        .expect("the peer links start");
    claims.reclaim_own(boot).expect("its own rows re-claimed");
    Booted { ordinal, boot, links, claims }
}

/// The rest of the boot: its manager on a `PgClaim` directory read from the store, its
/// regions read from the store as they start, its journal from the last committed cut, its
/// in-flight rows, heartbeat, keeper, adoption reader, cut writer and coordinator link.
fn build(t: &mut Takeover, b: Booted, replaces: bool) -> Sim {
    let Booted { ordinal, boot, mut links, claims } = b;
    let store = claims.store().clone();
    let s = store.super_region_regions();
    let generation = links.registered().generation;
    let reg = links.registered().reg;
    let c = claims.clone();
    let read: game::RegionStoreRead = Arc::new(move |rc| c.load_region(rc).map_err(|e| e.to_string()));
    // The drop set reaches the claims, whose `load_region` filters it: the server's filter.
    let c = claims.clone();
    let forget: game::RegionStoreForget = Arc::new(move |ids| c.forget_entities(ids));
    let mut spawner = InlineSpawner::default()
        .with_stored_terrain(Arc::new(world_store::TerrainFromStore(store.clone())))
        .with_region_store(read)
        .with_region_store_forget(forget);
    spawner.set_world_tick(t.w.pair.tick.load(Ordering::SeqCst));
    let mut sim = Sim::with_spawner(ordinal, s, boot, generation, &mut links, spawner);
    sim.manager.set_ownership(Box::new(game::PgClaim::new(ordinal, t.n)), s);
    sim.manager.start_peer_journal(store.last_committed_cut().unwrap());
    let (rows, live) = server::claims::read_directory(&store, ordinal).expect("the directory is read");
    sim.manager.directory_read(rows, live);
    sim.manager.set_stored_regions(store.stored_regions().unwrap());
    sim.manager.restore_in_flight(store.load_in_flight(claims.fence().in_flight_scope()).unwrap());
    sim.manager.set_entry_host(ENTRY);
    sim.keeper = Some(server::claims::DirectoryKeeper::new(claims.clone(), boot));
    sim.adopter = Some(server::claims::AdoptionReader::new(claims.clone()));
    let mut beat = server::lease::Heartbeat::new(store.clone(), ordinal, boot);
    beat.set_period(Duration::from_millis(BEAT_MS));
    sim.heartbeat = Some(beat);
    sim.fenced = claims.fenced().clone();
    let switch = SwitchLink(Arc::new(std::sync::Mutex::new((None, 0))));
    if let Some(c) = &t.w.coordinator {
        switch.replace(quic_world_link(c.addr, t.w.cert.clone(), ordinal, boot, reg));
    }
    sim.manager.set_world_link(Box::new(switch.clone()), boot, 0);
    sim.manager.set_world_map(land_map());
    let generation = server::claims::WorldGeneration::new(generation);
    sim.cuts = Some(server::cuts::CutWriter::with_world_cuts(claims.clone(), generation.clone()));
    if let Some(login) = &t.login {
        sim.login = Some(login.host_side(ordinal));
    }
    let i = ordinal as usize;
    t.w.switches[i] = switch;
    t.w.generations[i] = generation;
    if let Some(old) = t.w.links[i].replace(links) {
        drop(old);
    }
    let old = std::mem::replace(&mut t.w.claims[i], claims);
    if replaces {
        // The previous process's connections close with it.
        old.store().clone().shutdown();
    }
    sim
}

/// The coordinator of these tests: listening, `Succession` (or `policy`), world cuts every
/// [`CUT_EVERY`] at the hosts' tick, the shortened lease.
fn start_coordinator(store: StoreHandle, cert: PathBuf, tick: Arc<AtomicU64>, policy: CrashPolicy) -> server::coordinator_rt::ListeningCoordinator {
    start_coordinator_every(store, cert, tick, policy, CUT_EVERY)
}

/// [`start_coordinator`] with world cuts every `every`.
fn start_coordinator_every(
    store: StoreHandle,
    cert: PathBuf,
    tick: Arc<AtomicU64>,
    policy: CrashPolicy,
    every: Duration,
) -> server::coordinator_rt::ListeningCoordinator {
    let start = server::world_file::ensure_coordinator_state(&store, &land_map(), None).unwrap().expect("a history");
    let cuts = server::coordinator_rt::WorldCuts { every, tick: Box::new(move || tick.load(Ordering::SeqCst)) };
    let c = server::coordinator_rt::start_listening_as(
        store,
        start,
        "127.0.0.1:0".parse().unwrap(),
        TOKEN.into(),
        Some(cert),
        server::claims::Fenced::default(),
        Some(cuts),
        policy,
    )
    .expect("the coordinator listens");
    c.thread.with_core(|core| core.set_lease_dials(LEASE_MS, DEADLINE_MS), WAIT).expect("the core answers");
    c
}

/// The entry host of the login side.
const ENTRY: u32 = 0;

// ---- the login service (rejoins after a takeover) -----------------------------------------

/// The record each login token first spawns from (a fresh account's).
type Roster = Arc<std::sync::Mutex<std::collections::BTreeMap<String, PlayerRecord>>>;

/// The world store's players (`server::player_store::StorePlayers`), a fresh one starting
/// from its [`Roster`] record: once a cut wrote a record of it, the store's.
struct RosterThenStore {
    roster: Roster,
    store: StoreHandle,
}

impl PlayerStore for RosterThenStore {
    fn resolve(&mut self, key: &PlayerKey) -> std::io::Result<(PlayerId, Option<PlayerRecord>)> {
        let PlayerKey::Online(token) = key else { return Err(std::io::Error::other("not a login")) };
        let first = self.roster.lock().unwrap().get(&token.0).cloned().ok_or_else(|| std::io::Error::other("an unknown token"))?;
        let (player, stored) = self.store.resolve_player(&key.store_key()).map_err(std::io::Error::other)?;
        Ok((player, Some(stored.unwrap_or(first))))
    }
}

/// The login service's side: host 0's login link (it relays), each host's relay, the
/// service's own connection to it.
struct LoginSide {
    link: LoginLink,
    players: server::player_store::SharedPlayerStore,
    events_of: std::collections::BTreeMap<u32, (Arc<SpawnOrders>, crossbeam::channel::Sender<ServerEvent>, Receiver<ServerEvent>)>,
    tx: crossbeam::channel::Sender<LoginToHost>,
    events: Receiver<svc_link::LinkEvent<HostToLogin>>,
    seen: Vec<HostToLogin>,
    roster: Roster,
}

impl LoginSide {
    fn host_side(&self, ordinal: u32) -> SimLogin {
        let (orders, events_tx, events) = self.events_of.get(&ordinal).cloned().unwrap_or_else(|| {
            let (tx, rx) = unbounded();
            (Arc::new(SpawnOrders::new(server::auth_edge::system_clock())), tx, rx)
        });
        let relay = LoginRelay::new(orders.clone(), self.players.clone(), events_tx);
        SimLogin { orders, relay, events }
    }

    fn drain(&mut self) {
        for ev in self.events.try_iter() {
            if let svc_link::LinkEvent::Msg(msg) = ev {
                self.seen.push(msg);
            }
        }
    }
}

// ---- the world ----------------------------------------------------------------------------

/// `n` hosts under `claim` and a real coordinator; the world rig's [`World`] (indexed by
/// ordinal) with what a takeover test needs besides.
struct Takeover {
    w: World,
    n: u32,
    login: Option<LoginSide>,
    /// Each host's seam region (`seams[h]`, one step from another host's).
    seams: Vec<RegionCoords>,
}

/// Seams of `n` hosts under the rendezvous layout (every host live, as a fresh world lays
/// out): for each host the first region of its own (by z, then x) next to another host's.
fn seam_regions(s: u32, n: u32) -> Vec<RegionCoords> {
    let owner = |rc: RegionCoords| game::rendezvous_owner(game::super_region_of(rc, s), n);
    (0..n)
        .map(|h| {
            (4..44)
                .flat_map(|z| (4..44).map(move |x| RegionCoords::new(x, z)))
                .find(|&rc| {
                    owner(rc) == h
                        && [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dz)| owner(RegionCoords::new(rc.x + dx, rc.z + dz)) != h)
                })
                .unwrap_or_else(|| panic!("no seam region of host {h} (s = {s}, {n} hosts)"))
        })
        .collect()
}

impl Takeover {
    /// `n` fresh hosts under `claim` (each its keeper in its seam region, claimed on demand),
    /// a real coordinator (`policy`), the login service (`login`), every session up.
    fn start(db: &PgDb, dir: &std::path::Path, n: u32, policy: CrashPolicy, login: bool) -> Takeover {
        Takeover::start_every(db, dir, n, policy, login, CUT_EVERY)
    }

    /// [`Takeover::start`] with world cuts every `every`.
    fn start_every(db: &PgDb, dir: &std::path::Path, n: u32, policy: CrashPolicy, login: bool, every: Duration) -> Takeover {
        let booted: Vec<Booted> = (0..n).map(|h| register(db, dir, n, h, 2000 + u64::from(h))).collect();
        let store = open(db);
        let tick = Arc::new(AtomicU64::new(0));
        let cert = dir.join("coordinator.cert-sha256");
        let coordinator = start_coordinator_every(store.clone(), cert.clone(), tick.clone(), policy, every);
        let s = store.super_region_regions();
        let pair = Pair { sims: Vec::new(), started: Instant::now(), tick };
        let w = World {
            pair,
            seam: (RegionCoords::new(0, 0), RegionCoords::new(0, 0)),
            coordinator: Some(coordinator),
            cert,
            links: (0..n).map(|_| None).collect(),
            claims: booted.iter().map(|b| b.claims.clone()).collect(),
            store,
            switches: (0..n).map(|_| SwitchLink(Arc::new(std::sync::Mutex::new((None, 0))))).collect(),
            generations: (0..n).map(|_| server::claims::WorldGeneration::default()).collect(),
        };
        let seams = if n == 2 {
            let (a, b) = host_seam(s);
            let first = game::rendezvous_owner(game::super_region_of(a, s), n);
            if first == 0 { vec![a, b] } else { vec![b, a] }
        } else {
            seam_regions(s, n)
        };
        let mut t = Takeover { w, n, login: None, seams };
        t.w.seam = (t.seams[0], t.seams[1]);
        if login {
            t.login = Some(start_login(db, dir, n));
        }
        for b in booted {
            let sim = build(&mut t, b, false);
            t.w.pair.sims.push(sim);
        }
        // Claimed on demand: a visitor of each host asks for its seam region (as a client
        // re-asks) until the host owns it; then its keeper stands there.
        let seams = t.seams.clone();
        t.until("every host owns its seam region", WAIT, |t| {
            let mut all = true;
            for h in 0..t.n {
                let rc = seams[h as usize];
                if t.sim(h).manager.owner_of(rc) != game::Owner::Me {
                    all = false;
                    t.watch(h, rc);
                }
            }
            all
        });
        let now = t.w.pair.now();
        for h in 0..n {
            let rc = t.seams[h as usize];
            let (player, _) = t.w.claims[h as usize].store().resolve_player(&format!("takeover-keeper-{h}")).unwrap();
            t.sim(h).keep(player, rc, now);
        }
        t.until("every keeper region running on its host, every peer synced and reachable", WAIT, |t| {
            t.w.pair.sims.iter().all(|sim| {
                sim.manager.world_attached()
                    && sim.manager.peers_synced()
                    && sim.manager.running_regions().contains(&seams[sim.ordinal as usize])
                    && seams.iter().all(|rc| sim.manager.reaches(*rc))
            })
        });
        t
    }

    fn sim(&mut self, h: u32) -> &mut Sim {
        self.w.pair.sims.iter_mut().find(|s| s.ordinal == h).unwrap_or_else(|| panic!("host {h} is up"))
    }

    fn up(&self, h: u32) -> bool {
        self.w.pair.sims.iter().any(|s| s.ordinal == h)
    }

    /// One round of every host up (as `server::run`'s loop), then what each host's manager
    /// saw of a newer world generation noted (its rollback flag).
    fn round(&mut self) {
        self.w.pair.round();
        for sim in &mut self.w.pair.sims {
            if let Some(g) = sim.manager.take_higher_generation() {
                self.w.generations[sim.ordinal as usize].note(g);
            }
        }
        if let Some(login) = self.login.as_mut() {
            login.drain();
        }
    }

    fn until(&mut self, what: &str, within: Duration, mut done: impl FnMut(&mut Takeover) -> bool) {
        let deadline = Instant::now() + within;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            self.round();
        }
    }

    fn rounds(&mut self, n: usize) {
        for _ in 0..n {
            self.round();
        }
    }

    fn stop(mut self, db: &PgDb, dir: &std::path::Path) {
        if let Some(login) = self.login.take() {
            drop(login.link);
        }
        assert_eq!(prepared(db), 0, "nothing stays prepared");
        self.w.stop(dir);
    }

    /// Rounds until a world cut decided after `after` is committed and every host up settled
    /// it; no entity is ever stored twice (checked then).
    fn until_committed_after(&mut self, db: &PgDb, after: i64) -> i64 {
        self.until("a later world cut committed", LONG, |_| decisions(db).iter().any(|(c, _, k)| *k && *c > after));
        self.until("every host settled it", WAIT, |t| t.w.pair.sims.iter().all(|s| s.cuts.as_ref().unwrap().world_part().is_none()));
        assert_eq!(entities_stored_twice(db), 0, "an entity is stored in two regions: {:?}", entity_rows_stored_twice(db));
        decisions(db).into_iter().filter(|(_, _, k)| *k).map(|(c, _, _)| c).max().unwrap()
    }

    /// Host `h` crashes: its manager, peer listener, coordinator link and lease go at once.
    fn kill(&mut self, h: u32) {
        kill_host(&mut self.w, h);
    }

    /// Rounds until a succession of `pred` is settled; the row.
    fn until_settled(&mut self, db: &PgDb, pred: u32) -> [i64; 6] {
        self.until(&format!("a succession of host {pred} settled"), LONG, |_| {
            successions(db).iter().any(|r| r[1] == i64::from(pred) && r[4] >= 0)
        });
        let row = successions(db).into_iter().find(|r| r[1] == i64::from(pred) && r[4] >= 0).unwrap();
        // The heir's own view: nothing held, nothing pinned any more.
        let heir = row[2] as u32;
        self.until("the heir forgot the succession", WAIT, |t| {
            let m = &t.sim(heir).manager;
            m.inheriting().is_empty() && m.succession_pinned().is_empty() && m.successions().is_empty()
        });
        row
    }

    /// Host `h` boots again as `boot` (its registration on this thread: it may wait).
    fn restart(&mut self, db: &PgDb, dir: &std::path::Path, h: u32, boot: u64) {
        let b = register(db, dir, self.n, h, boot);
        let sim = build(self, b, true);
        let i = self.w.pair.sims.iter().position(|s| s.ordinal > h).unwrap_or(self.w.pair.sims.len());
        self.w.pair.sims.insert(i, sim);
    }

    /// `rc` runs on its owner `h` (an observer client of host `h` subscribes to it).
    fn watch(&mut self, h: u32, rc: RegionCoords) {
        let (visitor, player) = (90 + rc.x as u64 * 7 + rc.z as u64, PlayerId(9000 + u64::from(h)));
        let now = self.w.pair.now();
        let sim = self.sim(h);
        let _ = sim.manager.handle_server_event(ServerEvent::ObserverConnected { client: visitor, player }, now);
        let _ = sim.manager.handle_server_event(ServerEvent::ClientPacket(game::ClientPacket::RequestRegionConnection(rc), visitor), now);
    }

    /// Every running region of every host up that holds `id`.
    fn live(&mut self, id: EntityId) -> Vec<(u32, RegionCoords)> {
        holders_all(&mut self.w, id)
    }
}

/// The login service's link to the entry host, and a relay on every host.
fn start_login(db: &PgDb, dir: &std::path::Path, n: u32) -> LoginSide {
    let roster: Roster = Arc::default();
    let players = server::player_store::shared(RosterThenStore { roster: roster.clone(), store: open(db) });
    let mut events_of = std::collections::BTreeMap::new();
    for h in 0..n {
        let (tx, rx) = unbounded();
        events_of.insert(h, (Arc::new(SpawnOrders::new(server::auth_edge::system_clock())), tx, rx));
    }
    let (orders, events_tx, _) = events_of[&ENTRY].clone();
    // A relaying link needs relaying orders: the entry host's relay is made first.
    let _entry = LoginRelay::new(orders.clone(), players.clone(), events_tx.clone());
    let link = server::login_link::start_relaying(
        "127.0.0.1:0".parse().unwrap(),
        TOKEN.into(),
        Some(dir.join("host-link.cert-sha256")),
        orders,
        players.clone(),
        events_tx,
        ENTRY,
    )
    .expect("the login link listens");
    let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Login);
    let (tx, events) = svc_link::LinkDialer::connect::<HostToLogin, LoginToHost>(link.local_addr(), link.cert_sha256().to_string(), hello);
    match events.recv_timeout(WAIT) {
        Ok(svc_link::LinkEvent::Up(_)) => {}
        other => panic!("the login link did not come up: {other:?}"),
    }
    LoginSide { link, players, events_of, tx, events, seen: Vec::new(), roster }
}

impl Takeover {
    /// A login token whose first record stands in the middle of `rc`, and its player (the
    /// store gives the id: a cut writes records only of those).
    fn player(&mut self, n: u8, rc: RegionCoords) -> (String, PlayerId) {
        let token = game::PlayerToken::generate([n; 16]).0;
        let record = PlayerRecord {
            client: game::Client::default(),
            pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, above_ground_y(), REGION_SIZE / 2.0)),
            region: rc,
            fortress: None,
            adventure: None,
        };
        self.login.as_ref().unwrap().roster.lock().unwrap().insert(token.clone(), record);
        let key = PlayerKey::Online(game::PlayerToken(token.clone())).store_key();
        let (player, _) = self.w.store.resolve_player(&key).unwrap();
        (token, player)
    }

    /// The login service orders a spawn of `token` at `epoch` (a `POST /v1/join`) and waits
    /// for the ack: `(ok, the host it names)`.
    fn join(&mut self, token: &str, epoch: u64) -> (bool, Option<u32>) {
        let from = self.login.as_ref().unwrap().seen.len();
        let login = self.login.as_ref().unwrap();
        login.tx.send(LoginToHost::SpawnPlayer { player_token: token.into(), session_epoch: epoch }).unwrap();
        let mut found = None;
        let token = token.to_string();
        self.until("the spawn ack", WAIT, |t| {
            let l = t.login.as_mut().unwrap();
            l.drain();
            found = l.seen.iter().skip(from).find_map(|m| match m {
                HostToLogin::SpawnAck { player_token, session_epoch, ok, quic, .. } if *player_token == token && *session_epoch == epoch => {
                    Some((*ok, quic.clone()))
                }
                _ => None,
            });
            found.is_some()
        });
        let (ok, quic) = found.unwrap();
        let host = quic.and_then(|q| (0..self.n).find(|h| endpoint(*h).quic.as_deref() == Some(q.as_str())));
        (ok, host)
    }

    /// The client of (`token`, `epoch`) connects to host `h`; its session id.
    fn connect(&mut self, h: u32, token: &str, epoch: u64) -> ClientId {
        let mut players = self.login.as_ref().unwrap().players.lock().unwrap();
        let (player, record) =
            players.resolve(&PlayerKey::Online(game::PlayerToken(token.to_string()))).expect("a known token");
        drop(players);
        let orders = self.sim(h).login.as_ref().unwrap().orders.clone();
        orders.admit(token, epoch).expect("the ticket is admitted");
        let client = game::session_client_id(player, epoch);
        let now = self.w.pair.now();
        let manager = &mut self.sim(h).manager;
        let ev = ServerEvent::LoginConnected { client, player, record, player_token: token.into(), session_epoch: epoch };
        orders.announce(token, epoch, || assert!(manager.handle_server_event(ev, now))).expect("announced");
        client
    }

    /// Where `client`'s body is: `(host, region)` of every running region holding it.
    fn bodies(&mut self, client: ClientId) -> Vec<(u32, RegionCoords)> {
        let mut out = Vec::new();
        for sim in &mut self.w.pair.sims {
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

    /// Hosts up holding a parked record of `player`.
    fn parked(&mut self, player: PlayerId) -> Vec<u32> {
        self.w.pair.sims.iter().filter(|s| s.manager.parked_record(player).is_some()).map(|s| s.ordinal).collect()
    }
}

/// A slime in the middle of `rc` (host `h`'s), `dx` metres off its centre along x.
fn slime(t: &mut Takeover, h: u32, rc: RegionCoords, dx: f32) -> EntityId {
    spawn_slime_in(&mut t.w, h, rc, REGION_SIZE / 2.0 + dx)
}

/// Pushes `id` (in `from`, on host `h`) past the seam into `to` (one step along x or z).
fn push(t: &mut Takeover, h: u32, id: EntityId, from: RegionCoords, to: RegionCoords) -> bool {
    let y = above_ground_y();
    let past = |d: i32| match d {
        1 => REGION_SIZE + game::FLIP_HYSTERESIS + 2.0,
        -1 => -game::FLIP_HYSTERESIS - 2.0,
        _ => REGION_SIZE / 2.0,
    };
    let at = game::rapier3d::math::Vector::new(past(to.x - from.x), y, past(to.z - from.z));
    let mut pushed = false;
    t.sim(h).manager.spawner_mut().with_region(from, |region| {
        if let Some(&key) = region.data().id_index.get(&id) {
            region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
            pushed = true;
        }
    });
    pushed
}

/// Moves `id` (in `rc`, on host `h`) to the middle of `rc`.
fn centre(t: &mut Takeover, h: u32, id: EntityId, rc: RegionCoords) {
    let at = game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, above_ground_y(), REGION_SIZE / 2.0);
    let mut moved = false;
    t.sim(h).manager.spawner_mut().with_region(rc, |region| {
        if let Some(&key) = region.data().id_index.get(&id) {
            region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
            moved = true;
        }
    });
    assert!(moved, "{id:?} is in {rc:?} on host {h}");
}

/// `id` is in the world exactly once by the store (one `entities` row, or one in-flight
/// row), after a committed cut: what a restart would see.
fn stored_once(t: &Takeover, db: &PgDb, id: EntityId, what: &str) {
    let rows = stored_rows(db, id);
    let flying = in_flight_rows(&t.w.store, id);
    assert_eq!(rows.len() + flying, 1, "{what}: {id:?} is stored {} time(s) (rows {rows:?}, in flight {flying})", rows.len() + flying);
}

// ---- the crash table -------------------------------------------------------------------------

/// Row 1: a host dies between cuts. Its lease lapses, the coordinator declares it, the
/// survivor reports, the heir adopts, the settle cut commits — and nobody restarts: the world
/// generation and host 0's boot are unchanged, every super-region is host 0's, a slime on
/// each side is in the world once (stored once, and live once where it runs), the succession
/// is settled and host 1's row clean. A player of host 1 joins again: the login service
/// places it on the heir, once.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn host_killed_between_cuts_is_succeeded_and_nobody_restarts() {
    install_props();
    let Some(db) = PgDb::create("tk_between") else { return };
    let dir = scratch("tk_between");
    let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, true);
    let generation = t.w.store.world_generation().unwrap();
    let (a0, b1) = (t.seams[0], t.seams[1]);
    let on_0 = slime(&mut t, 0, a0, -3.0);
    let on_1 = slime(&mut t, 1, b1, -3.0);
    let also_1 = slime(&mut t, 1, b1, 3.0);
    // A player of host 1, playing there.
    let (token, player) = t.player(1, b1);
    assert_eq!(t.join(&token, 1), (true, Some(1)), "placed on host 1, which owns its record's region");
    let client = t.connect(1, &token, 1);
    t.until("the player has a body on host 1", WAIT, |t| t.bodies(client) == [(1, b1)]);
    let k = t.until_committed_after(&db, 0);
    let k = t.until_committed_after(&db, k);
    // Between cuts: none running anywhere.
    t.until("no cut running", WAIT, |t| t.w.pair.sims.iter().all(|s| s.manager.world_cut().is_none()));
    t.kill(1);
    let killed = Instant::now();
    // The heir's hold window: from applying the succession (its mask up) to its adoption.
    let mut applied_at: Option<Instant> = None;
    let mut window: Option<Duration> = None;
    t.until("host 0 applied and adopted", LONG, |t| {
        let inheriting = !t.sim(0).manager.inheriting().is_empty();
        if inheriting && applied_at.is_none() {
            applied_at = Some(Instant::now());
        }
        if !inheriting && applied_at.is_some() && window.is_none() {
            window = Some(applied_at.unwrap().elapsed());
        }
        window.is_some()
    });
    eprintln!(
        "[takeover] the heir held spawn orders {} ms (applied → adopted); the death was declared {} ms after the kill",
        window.unwrap().as_millis(),
        applied_at.unwrap().duration_since(killed).as_millis()
    );
    let row = t.until_settled(&db, 1);
    assert_eq!((row[2], row[5]), (0, 0), "host 0 is the heir, nothing abandoned: {row:?}");
    assert!(row[3] >= k, "based on the last committed cut ({row:?}, K = {k})");
    // Nobody restarted.
    assert_eq!(t.w.store.world_generation().unwrap(), generation, "the generation is unchanged");
    assert!(!t.w.generations[0].is_set(), "host 0 never saw a newer generation");
    assert!(!t.sim(0).fenced.is_set(), "host 0 is not fenced");
    assert_eq!(host_row(&db, 0).0, 2000, "host 0's boot is unchanged");
    let (_, clean, state) = host_row(&db, 1);
    assert_eq!((clean, state), (1, game::HOST_DEAD), "host 1's row: settled clean, dead");
    let owners = t.w.store.owners().unwrap();
    assert!(!owners.is_empty() && owners.iter().all(|(_, o, _)| *o == 0), "every super-region is host 0's: {owners:?}");
    // Each slime once: by the store after a later cut, and live where it runs.
    t.watch(0, b1);
    t.until("b1 runs on the heir", WAIT, |t| t.sim(0).manager.running_regions().contains(&b1));
    let after = t.until_committed_after(&db, row[4]);
    let _ = t.until_committed_after(&db, after);
    for id in [on_0, on_1, also_1] {
        assert_eq!(t.live(id).len(), 1, "{id:?} live once: {:?}", t.live(id));
        stored_once(&t, &db, id, "after the settle");
    }
    // The player joins again: the heir places it, once.
    let (ok, host) = t.join(&token, 2);
    assert_eq!((ok, host), (true, Some(0)), "the rejoin lands on the heir");
    let again = t.connect(0, &token, 2);
    t.until("the rejoined player has a body", WAIT, |t| !t.bodies(again).is_empty());
    t.rounds(20);
    assert_eq!(t.bodies(again), [(0, b1)], "one body, on the heir");
    assert!(t.parked(player).is_empty(), "no parked copy left");
    t.stop(&db, &dir);
}

/// A small seeded generator (splitmix64): a failing round names its seed.
struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// The client id of the shuttling player of [`host_killed_mid_crossing_keeps_every_id_once`].
const TRAVELLER: ClientId = 7;

/// Row 1 under traffic: slimes shuttle across the seam between the two hosts, and a player
/// crosses too; host 1 is killed at a seeded point of the shuttle (8 rounds, a fresh world
/// each). Every round, every slime is live in at most one region and the player has at most
/// one body; after the settle every slime is stored exactly once (one `entities` row or one
/// in-flight row) and the player is in the world once (one body, or one parked record, or
/// only its stored record). A failure names the round and the kill point.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn host_killed_mid_crossing_keeps_every_id_once() {
    install_props();
    for round in 0..8u64 {
        let mut rng = Seeded(0x7A4E_0000 + round);
        let kill_at = 20 + rng.below(140) as usize;
        let period = 8 + rng.below(12) as usize;
        let replay = format!("round {round} (seed {:#x}): kill after {kill_at} rounds, a push every {period}", 0x7A4E_0000 + round);
        let Some(db) = PgDb::create(&format!("tk_mid{round}")) else { return };
        let dir = scratch(&format!("tk_mid{round}"));
        let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, false);
        let (a0, b1) = (t.seams[0], t.seams[1]);
        let mut ids = Vec::new();
        for dx in [-4.0, 4.0] {
            ids.push(slime(&mut t, 0, a0, dx));
            ids.push(slime(&mut t, 1, b1, dx));
        }
        let (player, _) = t.w.store.resolve_player("takeover-traveller").unwrap();
        let record = PlayerRecord {
            client: game::Client::default(),
            pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, above_ground_y(), REGION_SIZE / 4.0)),
            region: a0,
            fortress: None,
            adventure: None,
        };
        let now = t.w.pair.now();
        assert!(t.sim(0).manager.handle_server_event(ServerEvent::ClientConnected { client: TRAVELLER, player, record: Some(record) }, now));
        t.until("the traveller has a body", WAIT, |t| !t.bodies(TRAVELLER).is_empty());
        let _ = t.until_committed_after(&db, 0);
        let traveller_crosses_at = rng.below(kill_at as u64) as usize;
        for r in 0..kill_at {
            if r % period == 0 {
                for &id in &ids {
                    // Wherever it is now, over the seam to the other side.
                    for (h, rc) in t.live(id) {
                        let to = if rc == a0 { b1 } else if rc == b1 { a0 } else { continue };
                        push(&mut t, h, id, rc, to);
                    }
                }
            }
            if r == traveller_crosses_at {
                if let [(h, rc)] = t.bodies(TRAVELLER)[..] {
                    if rc == a0 || rc == b1 {
                        let to = if rc == a0 { b1 } else { a0 };
                        let y = above_ground_y();
                        let past = if to.x > rc.x { REGION_SIZE + game::FLIP_HYSTERESIS + 2.0 } else { -game::FLIP_HYSTERESIS - 2.0 };
                        t.sim(h).manager.spawner_mut().with_region(rc, |region| {
                            if let Some(&key) = region.data().player_entites.get(&TRAVELLER) {
                                let at = game::rapier3d::math::Vector::new(past, y, REGION_SIZE / 2.0);
                                region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
                            }
                        });
                    }
                }
            }
            t.round();
            for &id in &ids {
                let live = t.live(id);
                assert!(live.len() <= 1, "{replay}: {id:?} live twice before the kill: {live:?}");
            }
            assert!(t.bodies(TRAVELLER).len() <= 1, "{replay}: two bodies of the traveller: {:?}", t.bodies(TRAVELLER));
        }
        t.kill(1);
        let deadline = Instant::now() + LONG;
        while !successions(&db).iter().any(|r| r[1] == 1 && r[4] >= 0) {
            assert!(Instant::now() < deadline, "{replay}: the succession never settled: {:?}", successions(&db));
            t.round();
            for &id in &ids {
                let live = t.live(id);
                assert!(live.len() <= 1, "{replay}: {id:?} live twice during the succession: {live:?}");
            }
            assert!(t.bodies(TRAVELLER).len() <= 1, "{replay}: two bodies of the traveller: {:?}", t.bodies(TRAVELLER));
        }
        let row = t.until_settled(&db, 1);
        assert_eq!(t.w.store.world_generation().unwrap(), t.w.generations[0].mine(), "{replay}: nobody restarted");
        let k1 = t.until_committed_after(&db, row[4]);
        let _ = t.until_committed_after(&db, k1);
        for &id in &ids {
            let live = t.live(id);
            assert!(live.len() <= 1, "{replay}: {id:?} live twice after the settle: {live:?}");
            stored_once(&t, &db, id, &replay);
        }
        let bodies = t.bodies(TRAVELLER).len();
        let parked = t.parked(player).len();
        assert!(bodies + parked <= 1, "{replay}: the traveller has {bodies} body(ies) and {parked} parked record(s)");
        if bodies + parked == 0 {
            assert!(
                t.w.store.resolve_player("takeover-traveller").unwrap().1.is_some(),
                "{replay}: the traveller is nowhere: no body, no parked record, no stored record"
            );
        }
        t.stop(&db, &dir);
    }
}

/// Row 2: host 1 dies with its part of a running cut not prepared (host 0's may be): the
/// coordinator aborts that cut, then declares the succession based on the previous committed
/// cut; the abort leaves the journals as they were — a slime that crossed from host 1 to
/// host 0 after that cut is once (were the journal truncated, the heir would restore it from
/// host 1's stored region too).
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn host_killed_before_prepare_is_succeeded_from_the_previous_cut() {
    install_props();
    let Some(db) = PgDb::create("tk_beforeprep") else { return };
    let dir = scratch("tk_beforeprep");
    // Cuts every 3 s: the crossing below falls between two cuts, the kill into the next.
    let mut t = Takeover::start_every(&db, &dir, 2, CrashPolicy::Succession, false, Duration::from_secs(3));
    let generation = t.w.store.world_generation().unwrap();
    let (a0, b1) = (t.seams[0], t.seams[1]);
    let crosser = slime(&mut t, 1, b1, 0.0);
    let stays = slime(&mut t, 1, b1, -5.0);
    let k = t.until_committed_after(&db, 0);
    let k = t.until_committed_after(&db, k);
    // Over the seam to host 0 after K (its journal entries are then the only record of it).
    assert!(push(&mut t, 1, crosser, b1, a0));
    t.until("the crosser lives on host 0", WAIT, |t| t.live(crosser) == [(0, a0)]);
    // Away from the seam: it stays in a0 (a copy of it restored in b1 would be a second body,
    // not one collapsing onto the same id in one region).
    centre(&mut t, 0, crosser, a0);
    let landed_after = decisions(&db).into_iter().filter(|(_, _, k)| *k).map(|(c, _, _)| c).max().unwrap();
    let cut = until_part(&mut t.w, 1, &["pending", "marked"]);
    let before = decisions(&db).into_iter().filter(|(c, _, k)| *k && *c < cut.0 as i64).map(|(c, _, _)| c).max().unwrap();
    assert!(before >= k);
    assert_eq!(before, landed_after, "the crossing is after the last committed cut (the case under test)");
    t.kill(1);
    t.until("the cut decided", LONG, |_| decision_of(&db, cut).is_some());
    assert_eq!(decision_of(&db, cut), Some(false), "a part never staged: the cut is aborted");
    let row = t.until_settled(&db, 1);
    assert_eq!(row[3], before, "the succession is based on the cut before the aborted one: {row:?}");
    assert!(row[4] > cut.0 as i64, "settled by a later cut");
    assert_eq!(t.w.store.world_generation().unwrap(), generation, "nobody restarted");
    assert_eq!(host_row(&db, 0).0, 2000, "host 0's boot is unchanged");
    assert!(!t.w.generations[0].is_set() && !t.sim(0).fenced.is_set(), "host 0 runs on");
    t.watch(0, b1);
    t.until("b1 runs on the heir", WAIT, |t| t.sim(0).manager.running_regions().contains(&b1));
    let k2 = t.until_committed_after(&db, row[4]);
    let _ = t.until_committed_after(&db, k2);
    for id in [crosser, stays] {
        assert_eq!(t.live(id).len(), 1, "{id:?} live once: {:?}", t.live(id));
        stored_once(&t, &db, id, "after the settle");
    }
    t.stop(&db, &dir);
}

/// Row 3: host 1 dies with its part prepared and the coordinator holding every
/// `CutPrepared` (or the commit already recorded): the cut commits; the succession is based
/// on THAT cut; host 1's region is as of it (the tick it helped to stage) until the heir
/// writes it again; every slime once.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn host_killed_after_prepare_is_succeeded_from_that_cut() {
    install_props();
    let Some(db) = PgDb::create("tk_afterprep") else { return };
    let dir = scratch("tk_afterprep");
    let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, false);
    let generation = t.w.store.world_generation().unwrap();
    let (a0, b1) = (t.seams[0], t.seams[1]);
    let ids = [slime(&mut t, 0, a0, 0.0), slime(&mut t, 1, b1, 0.0), slime(&mut t, 1, b1, 5.0)];
    let _ = t.until_committed_after(&db, 0);
    let mut cut = None;
    let deadline = Instant::now() + WAIT;
    while cut.is_none() {
        assert!(Instant::now() < deadline, "no cut had every part staged");
        if let Some((c, "prepared")) = t.sim(1).manager.world_cut_state() {
            let progress = t.w.coordinator.as_ref().unwrap().thread.inspect(|c| c.world_cut_progress(), WAIT).flatten();
            if progress.is_some_and(|(r, prepared, _)| r == c && prepared == [0, 1]) || decision_of(&db, c) == Some(true) {
                cut = Some(c);
                break;
            }
        }
        t.round();
    }
    let cut = cut.unwrap();
    t.kill(1);
    t.until("the cut decided", LONG, |_| decision_of(&db, cut).is_some());
    assert_eq!(decision_of(&db, cut), Some(true), "every part was staged: committed");
    let tick = decisions(&db).into_iter().find(|(c, _, _)| *c as u64 == cut.0).unwrap().1;
    let row = t.until_settled(&db, 1);
    assert_eq!(row[3], cut.0 as i64, "the succession is based on the cut host 1 helped to stage: {row:?}");
    assert_eq!(saved_tick(&db, b1), tick, "host 1's region is as of that cut");
    assert_eq!(t.w.store.world_generation().unwrap(), generation, "nobody restarted");
    assert_eq!(host_row(&db, 0).0, 2000, "host 0's boot is unchanged");
    assert!(!t.w.generations[0].is_set() && !t.sim(0).fenced.is_set(), "host 0 runs on");
    t.watch(0, b1);
    t.until("b1 runs on the heir", WAIT, |t| t.sim(0).manager.running_regions().contains(&b1));
    let k = t.until_committed_after(&db, row[4]);
    let _ = t.until_committed_after(&db, k);
    for id in ids {
        assert_eq!(t.live(id).len(), 1, "{id:?} live once: {:?}", t.live(id));
        stored_once(&t, &db, id, "after the settle");
    }
    t.stop(&db, &dir);
}

/// Rows 4 and 5: three hosts; host 2 dies, its heir applies the succession but dies before
/// it adopts (no settle cut): the coordinator sees a second lapse with the succession open
/// and rolls the world back ONCE — the generation + 1, the succession abandoned, the land
/// still the heir's. The third host exits `WorldRolledBack`; every host restarts from the
/// last committed cut, and every slime is in the world once (the dead host's from its stored
/// region, now on the heir's land).
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn heir_killed_before_the_settle_rolls_the_world_back_once() {
    install_props();
    let Some(db) = PgDb::create("tk_heirdies") else { return };
    let dir = scratch("tk_heirdies");
    let mut t = Takeover::start(&db, &dir, 3, CrashPolicy::Succession, false);
    let generation = t.w.store.world_generation().unwrap();
    let pred = 2;
    let heir = game::heir_of(pred, &[0, 1].into_iter().collect()).expect("an heir");
    let other = 1 - heir;
    let seams = t.seams.clone();
    let ids: Vec<EntityId> = (0..3).map(|h| slime(&mut t, h, seams[h as usize], 0.0)).collect();
    let _ = t.until_committed_after(&db, 0);
    let k = t.until_committed_after(&db, 0);
    // The heir will never adopt: its adoption reads are off.
    t.sim(heir).adopter = None;
    t.until("no cut running", WAIT, |t| t.w.pair.sims.iter().all(|s| s.manager.world_cut().is_none()));
    t.kill(pred);
    t.until("the heir applied the succession", LONG, |t| !t.sim(heir).manager.inheriting().is_empty());
    let row = successions(&db)[0];
    assert_eq!((row[1], row[2]), (i64::from(pred), i64::from(heir)), "{row:?}");
    t.kill(heir);
    t.until("the other host saw the rollback", LONG, |t| t.w.generations[other as usize].is_set());
    assert_eq!(exit_host(&mut t.w, other), server::claims::ServerExit::WorldRolledBack, "host {other} rolls back");
    assert_eq!(t.w.store.world_generation().unwrap(), generation + 1, "the generation raised once");
    let row = successions(&db)[0];
    assert_eq!((row[4], row[5]), (-1, 1), "the succession is abandoned, never settled: {row:?}");
    let last = decisions(&db).into_iter().filter(|(_, _, k)| *k).map(|(c, _, _)| c).max().unwrap();
    assert!(last >= k);
    assert_eq!(last, row[3], "nothing committed after the base cut: the world restarts from it");
    let s = t.w.store.super_region_regions();
    let owners = t.w.store.owners().unwrap();
    let pred_sr = game::super_region_of(seams[pred as usize], s);
    assert_eq!(
        owners.iter().find(|(sr, _, _)| *sr == pred_sr).map(|(_, o, _)| *o),
        Some(i64::from(heir)),
        "the dead host's land stays with the heir: {owners:?}"
    );
    // Every host starts again, at the new generation; none raises it again.
    for h in [other, heir, pred] {
        t.restart(&db, &dir, h, 4000 + u64::from(h));
    }
    t.until("every host attached", WAIT, |t| t.w.pair.sims.len() == 3 && t.w.pair.sims.iter().all(|s| s.manager.world_attached()));
    for (h, rc) in [(other, seams[other as usize]), (heir, seams[heir as usize]), (heir, seams[pred as usize])] {
        t.watch(h, rc);
    }
    t.until("every slime's region runs", WAIT, |t| {
        let running: BTreeSet<RegionCoords> = t.w.pair.sims.iter().flat_map(|s| s.manager.running_regions()).collect();
        seams.iter().all(|rc| running.contains(rc))
    });
    let after = t.until_committed_after(&db, last);
    let _ = t.until_committed_after(&db, after);
    for &id in &ids {
        assert_eq!(t.live(id).len(), 1, "{id:?} live once: {:?}", t.live(id));
        stored_once(&t, &db, id, "after the rollback");
    }
    assert_eq!(t.w.store.world_generation().unwrap(), generation + 1, "raised exactly once");
    assert_eq!(successions(&db).len(), 1, "no succession declared since");
    t.stop(&db, &dir);
}

/// Row 6: the coordinator dies with a succession declared (its row open, the land moved)
/// and not adopted yet. Its restart reads the open successions and sends their orders
/// again; the hosts answer from memory; the heir adopts and the next cut — the first
/// committed since the declaration — settles it. The generation is unchanged.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn coordinator_killed_mid_succession_finishes_it_after_its_restart() {
    install_props();
    let Some(db) = PgDb::create("tk_coorddies") else { return };
    let dir = scratch("tk_coorddies");
    let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, false);
    let generation = t.w.store.world_generation().unwrap();
    let (a0, b1) = (t.seams[0], t.seams[1]);
    let ids = [slime(&mut t, 0, a0, 0.0), slime(&mut t, 1, b1, 0.0)];
    let _ = t.until_committed_after(&db, 0);
    // The heir holds its adoption until the coordinator is back.
    let adopter = t.sim(0).adopter.take();
    t.until("no cut running", WAIT, |t| t.w.pair.sims.iter().all(|s| s.manager.world_cut().is_none()));
    t.kill(1);
    t.until("host 0 applied the succession", LONG, |t| !t.sim(0).manager.inheriting().is_empty());
    t.rounds(20);
    let declared = successions(&db)[0];
    kill_coordinator(&mut t.w);
    let store = open(&db);
    server::roles::recover_staged_cuts(&store, 2).expect("the staged parts are resolved");
    let tick = t.w.pair.tick.clone();
    let c = start_coordinator(store.clone(), t.w.cert.clone(), tick, CrashPolicy::Succession);
    let old = std::mem::replace(&mut t.w.store, store);
    old.shutdown();
    for sim in &t.w.pair.sims {
        t.w.switches[sim.ordinal as usize].replace(quic_world_link(c.addr, t.w.cert.clone(), sim.ordinal, sim.boot, sim.reg));
    }
    t.w.coordinator = Some(c);
    t.until("host 0 attached to the new coordinator", WAIT, |t| t.sim(0).manager.world_attached());
    t.rounds(20);
    t.sim(0).adopter = adopter;
    let row = t.until_settled(&db, 1);
    assert_eq!(row[0], declared[0], "the succession declared before the crash is the one settled");
    let first_after = decisions(&db).into_iter().filter(|(c, _, k)| *k && *c > row[3]).map(|(c, _, _)| c).min();
    assert_eq!(first_after, Some(row[4]), "no cut committed between the declaration and the settle");
    assert_eq!(t.w.store.world_generation().unwrap(), generation, "no rollback");
    t.watch(0, b1);
    t.until("b1 runs on the heir", WAIT, |t| t.sim(0).manager.running_regions().contains(&b1));
    let k = t.until_committed_after(&db, row[4]);
    let _ = t.until_committed_after(&db, k);
    for id in ids {
        assert_eq!(t.live(id).len(), 1, "{id:?} live once: {:?}", t.live(id));
        stored_once(&t, &db, id, "after the settle");
    }
    t.stop(&db, &dir);
}

/// Row 7: the coordinator dies BEFORE declaring — host 1's row lapsed, nothing declared. Its
/// restart remembers nothing of the lapse: it observes a whole lease again from its own first
/// read, then declares; the succession settles as if nothing had happened, later. The
/// generation is unchanged and every slime is once.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn coordinator_killed_before_declaring_observes_a_whole_lease_again() {
    install_props();
    let Some(db) = PgDb::create("tk_coordfirst") else { return };
    let dir = scratch("tk_coordfirst");
    let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, false);
    let generation = t.w.store.world_generation().unwrap();
    let (a0, b1) = (t.seams[0], t.seams[1]);
    let ids = [slime(&mut t, 0, a0, 0.0), slime(&mut t, 1, b1, 0.0)];
    let _ = t.until_committed_after(&db, 0);
    t.until("no cut running", WAIT, |t| t.w.pair.sims.iter().all(|s| s.manager.world_cut().is_none()));
    // Host 1 dies and the coordinator with it, before any lapse could be seen.
    t.kill(1);
    kill_coordinator(&mut t.w);
    // Host 1's row lapses with nobody to declare it.
    let dead = Instant::now();
    while dead.elapsed() < Duration::from_millis(LEASE_MS + 1_000) {
        t.round();
    }
    assert!(successions(&db).is_empty(), "nothing declared without a coordinator");
    let store = open(&db);
    server::roles::recover_staged_cuts(&store, 2).expect("the staged parts are resolved");
    let restarted = Instant::now();
    let c = start_coordinator(store.clone(), t.w.cert.clone(), t.w.pair.tick.clone(), CrashPolicy::Succession);
    let old = std::mem::replace(&mut t.w.store, store);
    old.shutdown();
    let (o, boot, reg) = (0, t.sim(0).boot, t.sim(0).reg);
    t.w.switches[o].replace(quic_world_link(c.addr, t.w.cert.clone(), 0, boot, reg));
    t.w.coordinator = Some(c);
    t.until("the succession declared", LONG, |_| !successions(&db).is_empty());
    let declared_after = restarted.elapsed();
    eprintln!("[takeover] the restarted coordinator declared {} ms after its start (lease {LEASE_MS} ms)", declared_after.as_millis());
    assert!(
        declared_after >= Duration::from_millis(LEASE_MS),
        "the restart observed a whole lease of its own before declaring ({} ms)",
        declared_after.as_millis()
    );
    let row = t.until_settled(&db, 1);
    assert_eq!((row[2], row[5]), (0, 0), "{row:?}");
    assert_eq!(t.w.store.world_generation().unwrap(), generation, "no rollback");
    assert_eq!(host_row(&db, 0).0, 2000, "host 0 never restarted");
    assert!(!t.w.generations[0].is_set());
    t.watch(0, b1);
    let k = t.until_committed_after(&db, row[4]);
    let _ = t.until_committed_after(&db, k);
    for id in ids {
        assert_eq!(t.live(id).len(), 1, "{id:?} live once: {:?}", t.live(id));
        stored_once(&t, &db, id, "after the settle");
    }
    t.stop(&db, &dir);
}

/// Row 8: host 1's process restarts before its old boot was declared: the new boot finds a
/// live host and vouches for its predecessor (`seen_ms = 0`) and waits; the coordinator
/// declares at its next read (no whole lease), the succession settles, and only then does
/// the new boot register — owning nothing; nobody restarts.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn restarted_process_of_a_dead_ordinal_waits_then_returns_owning_nothing() {
    reboot_of_a_dead_ordinal(1, "tk_reboot");
}

/// Row 8 with the LOWER ordinal dead: host 0 dies and host 1 inherits. Host 1 dials host 0
/// (the higher dials the lower), so this is the run in which a survivor's dialer meets a dead
/// boot: once the row is declared dead it holds no dialer for it (a zombie listening there is
/// never redialed); host 0's next boot registers after the settle at a new address, is
/// dialed there and is reachable, owning nothing.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn restarted_process_of_dead_host_0_waits_then_returns_owning_nothing() {
    reboot_of_a_dead_ordinal(0, "tk_reboot0");
}

fn reboot_of_a_dead_ordinal(dead: u32, tag: &str) {
    install_props();
    let Some(db) = PgDb::create(tag) else { return };
    let dir = scratch(tag);
    let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, false);
    let heir = 1 - dead;
    let generation = t.w.store.world_generation().unwrap();
    let heir_boot = host_row(&db, heir).0;
    let (s0, s1) = (t.seams[0], t.seams[1]);
    let ids = [slime(&mut t, 0, s0, 0.0), slime(&mut t, 1, s1, 0.0)];
    let _ = t.until_committed_after(&db, 0);
    t.until("no cut running", WAIT, |t| t.w.pair.sims.iter().all(|s| s.manager.world_cut().is_none()));
    // The heir adopts only once the test has seen its dialer let go of a dead lower host.
    let adopter = t.sim(heir).adopter.take();
    t.kill(dead);
    let killed = Instant::now();
    let (booted, declared_after, settled_when_registered) = std::thread::scope(|scope| {
        let (db, dir) = (&db, &dir);
        let boot = scope.spawn(move || register(db, dir, 2, dead, 3000 + u64::from(dead)));
        let mut declared_after = None;
        let mut adopter = adopter;
        let mut idle_since: Option<Instant> = None;
        while !boot.is_finished() {
            assert!(killed.elapsed() < LONG, "the new boot of host {dead} never registered");
            t.round();
            if declared_after.is_none() && !successions(db).is_empty() {
                declared_after = Some(killed.elapsed());
            }
            if declared_after.is_some() && adopter.is_some() {
                // Host 0 dead: host 1's dialer must drop the dead boot (and stay idle) before
                // the adoption lets the new boot register. Host 1 dead: nothing dials it.
                let dialing = t.w.links[heir as usize].as_ref().unwrap().dialing(dead);
                match dialing {
                    Some(true) => idle_since = None,
                    Some(false) => {
                        idle_since.get_or_insert_with(Instant::now);
                    }
                    None => idle_since = Some(Instant::now() - Duration::from_secs(10)),
                }
                let idle_long = idle_since.is_some_and(|at| at.elapsed() > Duration::from_secs(server::peer_link::PEER_DISCOVERY_SECS + 1));
                assert!(
                    killed.elapsed() < Duration::from_secs(20),
                    "host {heir} still dials the dead boot of host {dead} ({dialing:?}) 20 s after the kill"
                );
                if idle_long {
                    t.sim(heir).adopter = adopter.take();
                }
            }
        }
        let settled = successions(db).iter().any(|r| r[1] == i64::from(dead) && r[4] >= 0);
        (boot.join().expect("the boot thread"), declared_after, settled)
    });
    let declared_after = declared_after.expect("declared before the boot registered");
    eprintln!("[takeover] the vouched death of host {dead} was declared {} ms after the kill (lease {LEASE_MS} ms)", declared_after.as_millis());
    assert!(declared_after < Duration::from_millis(LEASE_MS), "a vouched death is declared before a lease could lapse");
    assert!(settled_when_registered, "the new boot registered only after the succession settled");
    assert_eq!(booted.links.registered().generation, generation, "registered without raising the generation");
    assert!(!booted.links.registered().bumped);
    assert!(booted.claims.fence().epochs.is_empty(), "the new boot owns nothing");
    let sim = build(&mut t, booted, true);
    let i = t.w.pair.sims.iter().position(|s| s.ordinal > dead).unwrap_or(t.w.pair.sims.len());
    t.w.pair.sims.insert(i, sim);
    t.until("the new boot attached and the peers reach each other", WAIT, |t| {
        t.up(dead)
            && t.sim(dead).manager.world_attached()
            && t.sim(dead).manager.peer_is_reachable(heir)
            && t.sim(heir).manager.peer_is_reachable(dead)
    });
    if dead == 0 {
        assert_eq!(t.w.links[1].as_ref().unwrap().dialing(0), Some(true), "host 1 dials host 0's new boot");
    }
    let owners = t.w.store.owners().unwrap();
    assert!(owners.iter().all(|(_, o, _)| *o == i64::from(heir)), "every super-region is host {heir}'s: {owners:?}");
    assert_eq!(host_row(&db, heir).0, heir_boot, "host {heir} never restarted");
    assert!(!t.w.generations[heir as usize].is_set());
    t.watch(heir, t.seams[dead as usize]);
    let row = successions(&db)[0];
    assert_eq!((row[1], row[2]), (i64::from(dead), i64::from(heir)), "{row:?}");
    let k = t.until_committed_after(&db, row[4]);
    let _ = t.until_committed_after(&db, k);
    for id in ids {
        assert_eq!(t.live(id).len(), 1, "{id:?} live once: {:?}", t.live(id));
        stored_once(&t, &db, id, "after the return");
    }
    assert_eq!(t.w.store.world_generation().unwrap(), generation);
    t.stop(&db, &dir);
}

/// Row 9: host 1's main loop hangs (no tick, no beat) past its lease while its sockets live:
/// it is declared dead and succeeded; when it wakes, its heartbeat matches no row and it
/// stops fenced (exit 3) — peers refuse its boot and nothing more of it is saved. Its player
/// joins again and lands on the heir. No entity is stored twice at any check, ever.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn hung_host_is_replaced_and_exits_fenced_when_it_wakes() {
    install_props();
    let Some(db) = PgDb::create("tk_hung") else { return };
    let dir = scratch("tk_hung");
    let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, true);
    let (a0, b1) = (t.seams[0], t.seams[1]);
    let ids = [slime(&mut t, 0, a0, 0.0), slime(&mut t, 1, b1, 0.0), slime(&mut t, 1, b1, 4.0)];
    let (token, _player) = t.player(2, b1);
    assert_eq!(t.join(&token, 1), (true, Some(1)));
    let client = t.connect(1, &token, 1);
    t.until("the player has a body on host 1", WAIT, |t| t.bodies(client) == [(1, b1)]);
    let k = t.until_committed_after(&db, 0);
    let _ = t.until_committed_after(&db, k);
    let hang = Duration::from_millis(LEASE_MS + 3_000);
    let woke_at = Instant::now() + hang;
    t.sim(1).frozen_until = Some(woke_at);
    let mut checked = Instant::now();
    let strict = |db: &PgDb| assert_eq!(entities_stored_twice(db), 0, "an entity is stored twice: {:?}", entity_rows_stored_twice(db));
    t.until("the succession of the hung host settled", LONG, |_| {
        if checked.elapsed() > Duration::from_millis(500) {
            checked = Instant::now();
            strict(&db);
        }
        successions(&db).iter().any(|r| r[1] == 1 && r[4] >= 0)
    });
    let b1_saved = saved_tick(&db, b1);
    // It wakes: its next beat says it was replaced.
    t.until("host 1 woke and knows it was replaced", LONG, |t| {
        strict(&db);
        Instant::now() >= woke_at && t.sim(1).fenced.is_set()
    });
    let replaced_after = woke_at.elapsed();
    assert!(replaced_after < Duration::from_secs(5), "fenced {} ms after it woke", replaced_after.as_millis());
    assert!(!t.sim(0).manager.peer_is_reachable(1), "host 0 refuses the zombie's boot");
    assert_eq!(exit_host(&mut t.w, 1), server::claims::ServerExit::Fenced, "the zombie exits fenced");
    strict(&db);
    // Nothing wrote b1 since the settle: not the zombie (fenced), not the heir (it does not
    // run b1 until watched below).
    assert!(!t.sim(0).manager.running_regions().contains(&b1), "the heir does not run b1 yet");
    assert_eq!(saved_tick(&db, b1), b1_saved, "the zombie saved nothing of b1 after it woke");
    let row = t.until_settled(&db, 1);
    t.watch(0, b1);
    let k = t.until_committed_after(&db, row[4]);
    let _ = t.until_committed_after(&db, k);
    strict(&db);
    for id in ids {
        assert_eq!(t.live(id).len(), 1, "{id:?} live once: {:?}", t.live(id));
        stored_once(&t, &db, id, "after the zombie's exit");
    }
    // Its client joins again: the heir.
    let (ok, host) = t.join(&token, 2);
    assert_eq!((ok, host), (true, Some(0)), "the rejoin lands on the heir");
    let again = t.connect(0, &token, 2);
    t.until("the rejoined player has a body", WAIT, |t| !t.bodies(again).is_empty());
    t.rounds(20);
    assert_eq!(t.bodies(again).len(), 1, "one body: {:?}", t.bodies(again));
    assert!(t.bodies(client).is_empty(), "the old session's body is gone");
    strict(&db);
    t.stop(&db, &dir);
}

/// Row 10: every host dies (the coordinator lives on: it declares nothing, no host is
/// alive to inherit). The first boot finds nobody alive and raises the generation; the
/// other registers at it without raising it again. Each re-claims its own land; the world is
/// the last committed cut's — every slime once — and no succession was ever written.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn every_host_killed_restarts_from_the_cut_with_its_own_land() {
    install_props();
    let Some(db) = PgDb::create("tk_allkilled") else { return };
    let dir = scratch("tk_allkilled");
    let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, false);
    let generation = t.w.store.world_generation().unwrap();
    let (a0, b1) = (t.seams[0], t.seams[1]);
    let ids = [slime(&mut t, 0, a0, 0.0), slime(&mut t, 1, b1, 0.0)];
    let k = t.until_committed_after(&db, 0);
    let k = t.until_committed_after(&db, k);
    let owners_before = t.w.store.owners().unwrap();
    t.kill(0);
    t.kill(1);
    // Past every lease: nobody is alive.
    let dead = Instant::now();
    while dead.elapsed() < Duration::from_millis(LEASE_MS + 1_500) {
        t.round();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(successions(&db).is_empty(), "nobody alive to inherit: nothing declared");
    t.restart(&db, &dir, 0, 5000);
    assert_eq!(t.w.store.world_generation().unwrap(), generation + 1, "the first boot raised the generation");
    t.restart(&db, &dir, 1, 5001);
    assert_eq!(t.w.store.world_generation().unwrap(), generation + 1, "the second did not raise it again");
    t.until("both hosts attached", WAIT, |t| t.w.pair.sims.len() == 2 && t.w.pair.sims.iter().all(|s| s.manager.world_attached()));
    let owners_after = t.w.store.owners().unwrap();
    let who = |o: &[(game::SuperRegion, i64, u64)]| o.iter().map(|(sr, h, _)| (*sr, *h)).collect::<Vec<_>>();
    assert_eq!(who(&owners_after), who(&owners_before), "each host has its own land back");
    t.watch(0, a0);
    t.watch(1, b1);
    t.until("both seam regions run", WAIT, |t| {
        t.sim(0).manager.running_regions().contains(&a0) && t.sim(1).manager.running_regions().contains(&b1)
    });
    let after = t.until_committed_after(&db, k);
    let _ = t.until_committed_after(&db, after);
    for id in ids {
        assert_eq!(t.live(id).len(), 1, "{id:?} live once: {:?}", t.live(id));
        stored_once(&t, &db, id, "after the total crash");
    }
    assert!(successions(&db).is_empty(), "no succession was ever written");
    assert_eq!(t.w.store.world_generation().unwrap(), generation + 1);
    t.stop(&db, &dir);
}

/// Row 11: the database "is down" for longer than the lease — the coordinator's `hosts`
/// read hangs, no host beats, decisions and saves fail — and comes back: the watch starts
/// its observation again from the first good read, so nobody is declared, nobody is fenced,
/// the generation is unchanged, and cuts commit again.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn outage_longer_than_the_lease_declares_nobody() {
    install_props();
    let Some(db) = PgDb::create("tk_outage") else { return };
    let dir = scratch("tk_outage");
    let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, false);
    let generation = t.w.store.world_generation().unwrap();
    let k = t.until_committed_after(&db, 0);
    let coordinator = &t.w.coordinator.as_ref().unwrap().thread;
    coordinator.hold_hosts_reads(true);
    t.w.store.fail_next_writes(u32::MAX);
    t.w.store.fail_next_decisions(u32::MAX);
    let beats: Vec<(u32, Option<server::lease::Heartbeat>)> =
        t.w.pair.sims.iter_mut().map(|s| (s.ordinal, s.heartbeat.take())).collect();
    let down = Instant::now();
    while down.elapsed() < Duration::from_secs(15) {
        t.round();
    }
    for (h, beat) in beats {
        t.sim(h).heartbeat = beat;
    }
    t.w.store.fail_next_writes(0);
    t.w.store.fail_next_decisions(0);
    t.w.coordinator.as_ref().unwrap().thread.hold_hosts_reads(false);
    let back = Instant::now();
    while back.elapsed() < Duration::from_millis(2 * LEASE_MS + 2_000) {
        t.round();
        assert!(successions(&db).is_empty(), "an outage declared somebody: {:?}", successions(&db));
    }
    for h in 0..2 {
        assert!(!t.sim(h).fenced.is_set(), "host {h} is not fenced");
        assert!(!t.w.generations[h as usize].is_set(), "host {h} saw no rollback");
    }
    assert_eq!(t.w.store.world_generation().unwrap(), generation);
    let _ = t.until_committed_after(&db, k);
    assert!(successions(&db).is_empty());
    t.stop(&db, &dir);
}

/// [`World`] of two `static` hosts ([`two_hosts`]) whose coordinator runs the crash policy
/// `policy` and whose hosts beat their leases (as `server::run`'s do), the lease shortened
/// for every party as in the `claim` tests.
fn static_world(db: &PgDb, dir: &std::path::Path, policy: CrashPolicy) -> World {
    let (mut pair, seam, links, claims) = two_hosts(db, dir);
    for (i, sim) in pair.sims.iter_mut().enumerate() {
        claims[i].store().set_host_lease_ms(LEASE_MS);
        let mut beat = server::lease::Heartbeat::new(claims[i].store().clone(), i as u32, sim.boot);
        beat.set_period(Duration::from_millis(BEAT_MS));
        sim.heartbeat = Some(beat);
        sim.fenced = claims[i].fenced().clone();
    }
    let store = open(db);
    let cert = dir.join("coordinator.cert-sha256");
    let coordinator = start_coordinator(store.clone(), cert.clone(), pair.tick.clone(), policy);
    let mut switches = Vec::new();
    let mut generations = Vec::new();
    for (ordinal, sim) in pair.sims.iter_mut().enumerate() {
        let link = quic_world_link(coordinator.addr, cert.clone(), ordinal as u32, sim.boot, sim.reg);
        let switch = SwitchLink(Arc::new(std::sync::Mutex::new((Some(link), 0))));
        switches.push(switch.clone());
        sim.manager.set_world_link(Box::new(switch), sim.boot, 0);
        sim.manager.set_world_map(land_map());
        let generation = server::claims::WorldGeneration::new(links[ordinal].registered().generation);
        generations.push(generation.clone());
        sim.cuts = Some(server::cuts::CutWriter::with_world_cuts(claims[ordinal].clone(), generation));
    }
    pair.until("both hosts attached", |p| p.sims.iter().all(|s| s.manager.world_attached()));
    let links = links.into_iter().map(Some).collect();
    World { pair, seam, coordinator: Some(coordinator), cert, links, claims, store, switches, generations }
}

/// Static ownership keeps R3's crash policy. The coordinator's policy is what production
/// derives for `VOXEL_OWNERSHIP=static` with two hosts ([`server::roles::coordinator_crash_policy`]),
/// the hosts beat their leases, and host 1 dies: its row lapses with host 0 beating — exactly
/// what a `Succession` coordinator declares — yet no `successions` row is ever written. Host
/// 1 restarts dirty and the world rolls back once (R3's `dirty_restart_rolls_every_host_back_once`
/// in `hosts.rs` is the R3 test).
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn static_ownership_still_rolls_the_world_back() {
    install_props();
    let Some(db) = PgDb::create("tk_static") else { return };
    let dir = scratch("tk_static");
    let policy = server::roles::coordinator_crash_policy(Some("static"), 2).expect("a valid ownership");
    let mut w = static_world(&db, &dir, policy);
    let generation = w.claims[0].store().world_generation().unwrap();
    let a0 = seam_of(&w, 0);
    let id = spawn_slime_in(&mut w, 0, a0, REGION_SIZE / 2.0);
    w.until_committed_after(&db, 0);
    let (first, _) = last_committed(&db);
    w.until_committed_after(&db, first);
    kill_host(&mut w, 1);
    // Past the (shortened) lease with host 1 gone and host 0 beating: a `Succession`
    // coordinator would have declared by now.
    let dead = Instant::now();
    while dead.elapsed() < Duration::from_millis(3 * LEASE_MS) {
        w.pair.round();
    }
    assert_eq!(count(&db, "SELECT COUNT(*) FROM successions"), 0, "static never declares a succession");
    let again = restart_host(&mut w, &db, &dir, 1, 3001);
    assert_eq!((again.generation, again.bumped), (generation + 1, true), "host 1 came back dirty: + 1");
    roll_back_and_restart_the_other(&mut w, &db, &dir, 1, generation + 1);
    w.pair.until("the slime back in a running region", |p| p.sims.iter_mut().map(|s| s.holders(id).len()).sum::<usize>() > 0);
    for _ in 0..40 {
        w.pair.round();
    }
    assert_eq!(holders_all(&mut w, id).len(), 1, "the slime once: {:?}", holders_all(&mut w, id));
    assert_eq!(count(&db, "SELECT COUNT(*) FROM successions"), 0, "no successions row is ever written under static");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Task 9's carried check on a real region thread: `ThreadRegionSpawner` restores a region
/// from the world store leaving out the drop set (`forget_entities`, through the claims it
/// loads with), and only it.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn thread_spawner_leaves_the_drop_set_out_of_a_store_load() {
    install_props();
    let Some(db) = PgDb::create("tk_forget") else { return };
    let dir = scratch("tk_forget");
    let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, false);
    let b1 = t.seams[1];
    let dropped = slime(&mut t, 1, b1, -4.0);
    let kept = slime(&mut t, 1, b1, 4.0);
    let k = t.until_committed_after(&db, 0);
    let _ = t.until_committed_after(&db, k);
    assert_eq!(stored_rows(&db, dropped), [b1]);
    assert_eq!(stored_rows(&db, kept), [b1]);
    // A reader with host 1's fence (a read writes nothing).
    let claims = HostClaims::none(open(&db), 1);
    claims.grow_to(claims.store().owned_by(1).unwrap());
    let mut spawner = server::region_threads::ThreadRegionSpawner::default().with_store(claims.clone());
    spawner.forget_entities(&[dropped].into_iter().collect());
    let (out, outputs) = unbounded();
    let input = spawner.spawn(b1, game::RegionSeed::Deferred(None), out);
    input.send(game::RegionInput::Shutdown).unwrap();
    let deadline = Instant::now() + WAIT;
    let portable = loop {
        match outputs.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((_, RegionOutput::Stopped(_, _, _, portable))) => break portable,
            Ok((_, RegionOutput::LoadFailed(why))) => panic!("the load failed: {why}"),
            Ok(_) => {}
            Err(e) => panic!("no Stopped from the region thread: {e}"),
        }
    };
    spawner.reap(b1);
    let ids: Vec<EntityId> = portable.entities.iter().map(|b| b.id).collect();
    assert!(!ids.contains(&dropped), "the dropped slime is left out: {ids:?}");
    assert!(ids.contains(&kept), "the other slime is restored: {ids:?}");
    claims.store().clone().shutdown();
    t.stop(&db, &dir);
}

// ---- drain: a cleanly departed host's land on demand (R4 Task 11) ---------------------------

/// The heir wait of the drain test (instead of `HEIR_WAIT_MS`, through the core's test hook).
const HEIR_WAIT_TEST_MS: u64 = 500;

impl Takeover {
    /// The coordinator's heir wait, ms (its test hook).
    fn heir_wait(&mut self, ms: u64) {
        let c = self.w.coordinator.as_ref().expect("the coordinator is up");
        c.thread.with_core(move |core| core.set_heir_wait_ms(ms), WAIT).expect("the core answers");
    }

    /// Host `h` exits as `server::run` does (`cuts::exit_world`: a leave cut), its lease
    /// beating meanwhile, every other host running round by round; its outcome and the
    /// packets its manager sent its clients on the way out. Its peer listener and
    /// coordinator link go with it.
    fn leave(&mut self, h: u32) -> (server::claims::ServerExit, Vec<(Option<ClientId>, ServerPacket)>) {
        let i = self.w.pair.sims.iter().position(|s| s.ordinal == h).expect("the host is up");
        let mut sim = self.w.pair.sims.remove(i);
        for _ in sim.packets.try_iter() {}
        let fenced = sim.fenced.clone();
        let generation = self.w.generations[h as usize].clone();
        let started = self.w.pair.started;
        let region_out = sim.region_out.clone();
        let cuts = sim.cuts.take();
        let mut beat = sim.heartbeat.take();
        let pair = &mut self.w.pair;
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
                m.spawner_mut().tick_all();
                m.spawner_mut().pump();
                let _ = server::lease::pump_or_fence(&mut beat, &fenced, Instant::now());
                pair.round();
            },
        );
        self.w.switches[h as usize].drop_link();
        self.w.links[h as usize] = None;
        let packets = sim.packets.try_iter().collect();
        (exit, packets)
    }
}

/// The successions' `kind`s, by id.
fn succession_kinds(db: &PgDb) -> Vec<i64> {
    query_i64s(db, "SELECT kind FROM successions ORDER BY id")
}

/// Host 1 leaves in a committed cut (its player parked and saved first, then refused with
/// `LEAVING_REFUSAL`); nothing is given away while nobody needs its land. A slime of host 0
/// sent toward host 1's region waits in host 0's session; a player of host 1 logs in again:
/// host 0 asks for the land, and after the heir wait (500 ms by the test hook) the
/// coordinator gives ALL of host 1's land to host 0 as a `Leave` succession. While it waits
/// a login is refused (the login service's 503) and retried, as the client does. The player
/// stands where the leave cut parked them, once; the slime arrives once; every id is stored
/// once; nobody restarted.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn drained_host_is_inherited_on_demand_and_nothing_is_lost() {
    install_props();
    let Some(db) = PgDb::create("tk_drain") else { return };
    let dir = scratch("tk_drain");
    let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, true);
    t.heir_wait(HEIR_WAIT_TEST_MS);
    let generation = t.w.store.world_generation().unwrap();
    let (a0, b1) = (t.seams[0], t.seams[1]);
    let stayed = slime(&mut t, 1, b1, -3.0);
    let (token, player) = t.player(11, b1);
    assert_eq!(t.join(&token, 1), (true, Some(1)), "placed on host 1, which owns its record's region");
    let client = t.connect(1, &token, 1);
    t.until("the player has a body on host 1", WAIT, |t| t.bodies(client) == [(1, b1)]);
    let _ = t.until_committed_after(&db, 0);
    let boot1 = host_row(&db, 1).0;
    let (exit, packets) = t.leave(1);
    assert_eq!(exit, server::claims::ServerExit::Clean, "host 1 left");
    assert!(
        packets.iter().any(|(c, p)| *c == Some(client) && matches!(p, ServerPacket::Refused(why) if why == server::claims::LEAVING_REFUSAL)),
        "the player's client is told the server is stopping: {packets:?}"
    );
    assert_eq!(host_row(&db, 1), (boot1, 1, game::HOST_LIVE), "host 1's row: clean, live, the boot that left");
    let (left_in, _) = last_committed(&db);
    let key = PlayerKey::Online(game::PlayerToken(token.clone())).store_key();
    let parked = t.w.store.resolve_player(&key).unwrap().1.expect("the leave cut saved the player");
    assert_eq!(parked.region, b1);
    t.rounds(20);
    assert!(successions(&db).is_empty(), "nothing is given away while nobody needs it");
    // A slime toward host 1's region: it waits in host 0's session.
    let crosser = slime(&mut t, 0, a0, 0.0);
    assert!(push(&mut t, 0, crosser, a0, b1));
    t.until("the slime waits for host 1", WAIT, |t| t.sim(0).manager.peer_unreceived_ids().iter().any(|(p, i, _)| *p == 1 && *i == crosser));
    // The player logs in again; refused while the land waits for its heir, retried.
    let asked = Instant::now();
    let mut epoch = 2;
    let mut refused = 0;
    let host = loop {
        match t.join(&token, epoch) {
            (true, host) => break host,
            (false, _) => {
                refused += 1;
                assert!(successions(&db).is_empty() || refused < 20, "refused {refused} times after the succession");
                assert!(asked.elapsed() < LONG, "never placed");
                epoch += 1;
            }
        }
    };
    eprintln!("[takeover] the login on departed land was placed after {refused} refusal(s), {} ms", asked.elapsed().as_millis());
    assert_eq!(host, Some(0), "placed on the heir");
    assert_eq!(succession_kinds(&db), [game::SuccessionKind::Leave.code()], "one Leave succession");
    let row = successions(&db)[0];
    assert_eq!((row[1], row[2], row[5]), (1, 0, 0), "host 1's land to host 0: {row:?}");
    assert!(row[3] >= left_in, "based on the leave cut or a later one: {row:?}, left in {left_in}");
    let again = t.connect(0, &token, epoch);
    t.until("the player has a body on the heir", WAIT, |t| !t.bodies(again).is_empty());
    t.rounds(20);
    assert_eq!(t.bodies(again), [(0, parked.region)], "one body, where the leave cut parked the player");
    assert!(t.parked(player).is_empty(), "no parked copy left");
    let row = t.until_settled(&db, 1);
    assert_eq!(t.w.store.world_generation().unwrap(), generation, "nobody restarted");
    assert!(!t.w.generations[0].is_set() && !t.sim(0).fenced.is_set());
    assert_eq!(host_row(&db, 1), (boot1, 1, game::HOST_LIVE), "a leave marks no row dead");
    let owners = t.w.store.owners().unwrap();
    assert!(!owners.is_empty() && owners.iter().all(|(_, o, _)| *o == 0), "every super-region is host 0's: {owners:?}");
    t.until("the slime arrived once", WAIT, |t| t.live(crosser).len() == 1 && t.sim(0).manager.peer_unreceived() == 0);
    let after = t.until_committed_after(&db, row[4]);
    let _ = t.until_committed_after(&db, after);
    for id in [stayed, crosser] {
        assert_eq!(t.live(id).len(), 1, "{id:?} live once: {:?}", t.live(id));
        stored_once(&t, &db, id, "after the drain");
    }
    assert_eq!(t.bodies(again).len(), 1, "the player once");
    t.stop(&db, &dir);
}

/// R3's `clean_leave_and_return…` under `claim`: host 1 leaves in a committed cut; a slime
/// toward its region waits on host 0, which asks for the land; host 1 comes back (another
/// boot) within the heir wait — it re-claims what the directory still says is its own, the
/// coordinator forgets the request (nothing is given away, also after the wait ran out), the
/// held session is released by the new boot's hello and the slime arrives once.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn a_leaver_that_returns_in_time_keeps_its_land() {
    /// Long enough for the return, short enough to watch it run out.
    const WAIT_MS: u64 = 8_000;
    install_props();
    let Some(db) = PgDb::create("tk_return") else { return };
    let dir = scratch("tk_return");
    let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, false);
    t.heir_wait(WAIT_MS);
    let generation = t.w.store.world_generation().unwrap();
    let (a0, b1) = (t.seams[0], t.seams[1]);
    let _ = t.until_committed_after(&db, 0);
    let own_before: BTreeSet<game::SuperRegion> = t.w.store.owned_by(1).unwrap().epochs.keys().copied().collect();
    assert!(!own_before.is_empty());
    assert_eq!(t.leave(1).0, server::claims::ServerExit::Clean, "host 1 left");
    assert_eq!(host_row(&db, 1).1, 1, "its row is clean");
    let id = slime(&mut t, 0, a0, 0.0);
    assert!(push(&mut t, 0, id, a0, b1));
    t.until("the slime waits for host 1", WAIT, |t| t.sim(0).manager.peer_unreceived_ids().iter().any(|(p, i, _)| *p == 1 && *i == id));
    let (left_in, _) = last_committed(&db);
    let _ = t.until_committed_after(&db, left_in);
    let rows = count(&db, &format!("SELECT COUNT(*) FROM in_flight WHERE host = 0 AND kind = 0 AND target_rx = {} AND target_rz = {}", b1.x, b1.z));
    assert_eq!(rows, 1, "host 0's cut holds the waiting slime as a row");
    // Host 1 returns before the heir wait ran out.
    let back = Instant::now();
    t.restart(&db, &dir, 1, 3001);
    let own_after: BTreeSet<game::SuperRegion> = t.w.store.owned_by(1).unwrap().epochs.keys().copied().collect();
    assert_eq!(own_after, own_before, "it re-claimed its land");
    // Its region runs again (a visitor, as host 1's keeper made it run before it left).
    t.watch(1, b1);
    t.until("the slime arrived on host 1", WAIT, |t| t.w.pair.sims.iter().all(|s| s.manager.peer_unreceived() == 0) && t.sim(1).holders(id) == [b1]);
    // Past the heir wait (from the return, so past it from any request too): nothing given away.
    t.until("the heir wait ran out", LONG, |_| back.elapsed() > Duration::from_millis(WAIT_MS + 2_000));
    assert!(successions(&db).is_empty(), "the return cancelled the wait: {:?}", successions(&db));
    let own_now: BTreeSet<game::SuperRegion> = t.w.store.owned_by(1).unwrap().epochs.keys().copied().collect();
    assert_eq!(own_now, own_before, "host 1 keeps its land");
    let k = t.until_committed_after(&db, last_committed(&db).0);
    let _ = t.until_committed_after(&db, k);
    assert_eq!(t.live(id).len(), 1, "once: {:?}", t.live(id));
    stored_once(&t, &db, id, "after the return");
    assert_eq!(t.w.store.world_generation().unwrap(), generation, "a clean return keeps the generation");
    for h in 0..2 {
        assert!(!t.w.generations[h].is_set(), "no rollback");
    }
    t.stop(&db, &dir);
}
