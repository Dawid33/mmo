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
//! What is left of the crash table is the regression a twin run found; a failure names what
//! replays it (the round, the kill point).


include!("common/hosts_rig.rs");

use std::sync::atomic::{AtomicU64, Ordering};

/// The host lease of these tests (every party: the store's checks, the coordinator's watch).
const LEASE_MS: u64 = 3_000;
/// Between two heartbeats.
const BEAT_MS: u64 = 500;
/// A succession's deadline (the product's).
const DEADLINE_MS: u64 = game::SUCCESSION_DEADLINE_MS;

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

// ---- hosts under `claim` ----------------------------------------------------------------

/// A host process's boot under `claim`, up to its registration — `server::run`'s order: its
/// peer listener bound, REGISTERED (the store issues a new id: a boot never waits), then what
/// the registration gave it (nothing, or a total crash's land) taken into its fence.
struct Booted {
    ordinal: HostId,
    boot: u64,
    links: PeerLinks,
    claims: HostClaims,
}

fn register(db: &PgDb, dir: &std::path::Path, named: HostId, boot: u64) -> Booted {
    let store = open(db);
    let want = next_id(db);
    let (links, registered) = boot_links(&store, want, &format!("host-{}", named.0), boot, TOKEN, dir, "127.0.0.1:0");
    let ordinal = registered.id;
    let claims = HostClaims::none(store.clone(), ordinal);
    claims.reclaim_own(boot).expect("what the registration gave it taken");
    Booted { ordinal, boot, links, claims }
}

/// The rest of the boot: its manager on a `PgClaim` directory read from the store, its
/// regions read from the store as they start, its journal from the last committed cut, its
/// in-flight rows, heartbeat, keeper, adoption reader, cut writer and coordinator link.
fn build(t: &mut Takeover, b: Booted, replaces: bool) -> Sim {
    let Booted { ordinal, boot, mut links, claims } = b;
    let store = claims.store().clone();
    let s = store.super_region_regions();
    let generation = store.world_generation().unwrap();
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
    sim.manager.set_ownership(Box::new(game::PgClaim::new(ordinal)), s);
    sim.manager.start_peer_journal(store.last_committed_cut().unwrap());
    let (rows, live, draining) = server::claims::read_directory(&store, ordinal).expect("the directory is read");
    sim.manager.set_draining(draining);
    sim.manager.directory_read(rows, live);
    sim.manager.set_stored_regions(store.stored_regions().unwrap());
    sim.manager.restore_in_flight(store.load_in_flight(claims.fence().in_flight_scope()).unwrap());
    // Before any placement, as `server::run`: the hosts that left cleanly hold nobody.
    sim.manager.note_left_peers(server::peer_link::left_cleanly(&store, ordinal).unwrap());
    sim.keeper = Some(server::claims::DirectoryKeeper::new(claims.clone()));
    sim.adopter = Some(server::claims::AdoptionReader::new(claims.clone()));
    let mut beat = server::lease::Heartbeat::new(store.clone(), ordinal, boot);
    beat.set_period(Duration::from_millis(BEAT_MS));
    sim.heartbeat = Some(beat);
    sim.fenced = claims.fenced().clone();
    let switch = SwitchLink(Arc::new(std::sync::Mutex::new((None, 0))));
    if let Some(c) = &t.w.coordinator {
        switch.replace(http_world_link(vec![c.url()], t.w.cert.clone(), ordinal));
    }
    sim.manager.set_world_link(Box::new(switch.clone()), boot, 0);
    sim.manager.set_world_map(land_map());
    let generation = server::claims::WorldGeneration::new(generation);
    sim.cuts = Some(server::cuts::CutWriter::with_world_cuts(claims.clone(), generation.clone()));
    if let Some(login) = &t.login {
        sim.login = Some(login.host_side(ordinal));
    }
    let i = ordinal.0 as usize;
    *slot(&mut t.w.switches, ordinal, SwitchLink::default()) = switch;
    *slot(&mut t.w.generations, ordinal, Default::default()) = generation;
    while t.w.links.len() <= i {
        t.w.links.push(None);
    }
    if let Some(old) = t.w.links[i].replace(links) {
        drop(old);
    }
    let fill = claims.clone();
    let old = std::mem::replace(slot(&mut t.w.claims, ordinal, fill), claims);
    if replaces {
        // The previous process's connections close with it.
        old.store().clone().shutdown();
    }
    sim
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
    let c = listening_coordinator(store, start, &cert, cuts, server::claims::Fenced::default(), policy);
    c.thread.with_core(|core| core.set_lease_dials(LEASE_MS, DEADLINE_MS), WAIT).expect("the core answers");
    c
}

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

/// The login service's side: its connection to the coordinator's login listener (Elastic 1
/// Task 6: the coordinator relays each order to the gate host and the answers back), each
/// host's relay.
struct LoginSide {
    players: server::player_store::SharedPlayerStore,
    events_of: std::collections::BTreeMap<HostId, (Arc<SpawnOrders>, crossbeam::channel::Sender<ServerEvent>, Receiver<ServerEvent>)>,
    tx: crossbeam::channel::Sender<LoginToHost>,
    events: Receiver<svc_link::LinkEvent<HostToLogin>>,
    seen: Vec<HostToLogin>,
    roster: Roster,
}

impl LoginSide {
    fn host_side(&self, ordinal: HostId) -> SimLogin {
        let (orders, events_tx, events) = self.events_of.get(&ordinal).cloned().unwrap_or_else(|| {
            let (tx, rx) = unbounded();
            (Arc::new(SpawnOrders::new(server::auth_edge::system_clock())), tx, rx)
        });
        let relay = LoginRelay::new(orders, self.players.clone(), events_tx);
        SimLogin { relay, events }
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
    let owner = |rc: RegionCoords| game::rendezvous_among(game::super_region_of(rc, s), (0..u64::from(n)).map(HostId)).expect("an owner");
    (0..u64::from(n))
        .map(HostId)
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
        // Ids issued from 0 in order (the rig's database): the first `n` hosts are h(0)..h(n).
        let booted: Vec<Booted> = (0..u64::from(n)).map(HostId).map(|h| register(db, dir, h, 2000 + h.0)).collect();
        assert!(booted.iter().enumerate().all(|(i, b)| b.ordinal == HostId(i as u64)), "ids issued in order");
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
            let first = game::rendezvous_among(game::super_region_of(a, s), (0..u64::from(n)).map(HostId)).expect("an owner");
            if first == h(0) { vec![a, b] } else { vec![b, a] }
        } else {
            seam_regions(s, n)
        };
        let mut t = Takeover { w, n, login: None, seams };
        t.w.seam = (t.seams[0], t.seams[1]);
        if login {
            let at = t.w.coordinator.as_ref().unwrap().login.clone().expect("the coordinator listens for the login service");
            t.login = Some(start_login(db, at, n));
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
            for h in (0..u64::from(t.n)).map(HostId) {
                let rc = seams[h.0 as usize];
                if t.sim(h).manager.owner_of(rc) != game::Owner::Me {
                    all = false;
                    t.watch(h, rc);
                }
            }
            all
        });
        let now = t.w.pair.now();
        for h in (0..u64::from(n)).map(HostId) {
            let rc = t.seams[h.0 as usize];
            let (player, _) = t.w.claims[h.0 as usize].store().resolve_player(&format!("takeover-keeper-{}", h.0)).unwrap();
            t.sim(h).keep(player, rc, now);
        }
        t.until("every keeper region running on its host, every peer synced and reachable", WAIT, |t| {
            t.w.pair.sims.iter().all(|sim| {
                sim.manager.world_attached()
                    && sim.manager.peers_synced()
                    && sim.manager.running_regions().contains(&seams[sim.ordinal.0 as usize])
                    && seams.iter().all(|rc| sim.manager.reaches(*rc))
            })
        });
        t
    }

    fn sim(&mut self, h: HostId) -> &mut Sim {
        self.w.pair.sims.iter_mut().find(|s| s.ordinal == h).unwrap_or_else(|| panic!("host {h} is up"))
    }

    /// One round of every host up (as `server::run`'s loop), then what each host's manager
    /// saw of a newer world generation noted (its rollback flag).
    fn round(&mut self) {
        self.w.pair.round();
        for sim in &mut self.w.pair.sims {
            if let Some(g) = sim.manager.take_higher_generation() {
                self.w.generations[sim.ordinal.0 as usize].note(g);
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

    fn stop(mut self, db: &PgDb, dir: &std::path::Path) {
        drop(self.login.take());
        assert_eq!(prepared(db), 0, "nothing stays prepared");
        self.w.stop(dir);
    }

    /// Host `h` crashes: its manager, peer listener, coordinator link and lease go at once.
    fn kill(&mut self, h: HostId) {
        kill_host(&mut self.w, h);
    }

    /// The process of host `h` starts again as `boot`, under `h`'s name: a NEW host (the
    /// returned id), never `h` again — `h`'s old row is lapsed by the name.
    fn restart(&mut self, db: &PgDb, dir: &std::path::Path, h: HostId, boot: u64) -> HostId {
        let b = register(db, dir, h, boot);
        let id = b.ordinal;
        let sim = build(self, b, false);
        let i = self.w.pair.sims.iter().position(|s| s.ordinal > id).unwrap_or(self.w.pair.sims.len());
        self.w.pair.sims.insert(i, sim);
        id
    }

    /// `rc` runs on its owner `h` (an observer client of host `h` subscribes to it).
    fn watch(&mut self, h: HostId, rc: RegionCoords) {
        let (visitor, player) = (90 + rc.x as u64 * 7 + rc.z as u64, PlayerId(9000 + h.0));
        let now = self.w.pair.now();
        let sim = self.sim(h);
        let _ = sim.manager.handle_server_event(ServerEvent::ObserverConnected { client: visitor, player }, now);
        let _ = sim.manager.handle_server_event(ServerEvent::ClientPacket(game::ClientPacket::RequestRegionConnection(rc), visitor), now);
    }
}

/// The login service's link to the coordinator's login listener at `at` (its pin), and the
/// relay state of every host `0..n`.
fn start_login(db: &PgDb, at: (std::net::SocketAddr, String), n: u32) -> LoginSide {
    let roster: Roster = Arc::default();
    let players = server::player_store::shared(RosterThenStore { roster: roster.clone(), store: open(db) });
    let mut events_of = std::collections::BTreeMap::new();
    for h in (0..u64::from(n)).map(HostId) {
        let (tx, rx) = unbounded();
        events_of.insert(h, (Arc::new(SpawnOrders::new(server::auth_edge::system_clock())), tx, rx));
    }
    let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Login, RIG_WORLD);
    let (tx, events) = svc_link::LinkDialer::connect::<HostToLogin, LoginToHost>(at.0, at.1, hello);
    match events.recv_timeout(WAIT) {
        Ok(svc_link::LinkEvent::Up(_)) => {}
        other => panic!("the login link did not come up: {other:?}"),
    }
    LoginSide { players, events_of, tx, events, seen: Vec::new(), roster }
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
            adventure: None,
        };
        self.login.as_ref().unwrap().roster.lock().unwrap().insert(token.clone(), record);
        let key = PlayerKey::Online(game::PlayerToken(token.clone())).store_key();
        let (player, _) = self.w.store.resolve_player(&key).unwrap();
        (token, player)
    }

    /// The login service orders a spawn of `token` at `epoch` (a `POST /v1/join`) and waits
    /// for the ack: `(ok, the host it names)`.
    fn join(&mut self, token: &str, epoch: u64) -> (bool, Option<HostId>) {
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
        let host = quic.and_then(|q| (0..u64::from(self.n)).map(HostId).find(|h| endpoint(*h).quic.as_deref() == Some(q.as_str())));
        (ok, host)
    }
}

// ---- the crash table -------------------------------------------------------------------------

// ---- drain: a cleanly departed host's land on demand (R4 Task 11) ---------------------------

impl Takeover {
}

/// Regression: k3d twin phase 7 (Task 10): a pruned peer was never forgotten, so peers_synced never held and every join was 503 (b58d2379 message)
/// Elastic 2b Task 10b (the twin's phase 7): a landless process X starts, the gate (host 0)
/// links to it and syncs, then X is force-killed. Its row is dead (the coordinator's
/// `end_landless_host`, done here by hand in the same instant so no read of the gate sees it
/// dead first) and a new host's registration prunes it (dead, owning nothing). The gate's next
/// directory read does not show X — its id below the read's `next_host_id` — so the gate
/// forgets it: it stops waiting for X's sync and a join answers ok (a 200, not the 503 of a
/// gate awaiting a peer for ever). The world generation never moves.
#[test]
#[ignore = "slow: >100ms — run via just test-r4"]
fn a_pruned_landless_peer_is_forgotten_and_the_gate_places_again() {
    install_props();
    let Some(db) = PgDb::create("tk_pruned_peer") else { return };
    let dir = scratch("tk_pruned_peer");
    let mut t = Takeover::start(&db, &dir, 2, CrashPolicy::Succession, true);
    let generation = t.w.store.world_generation().unwrap();
    let s0 = t.seams[0];
    let (first, _) = t.player(41, s0);
    assert!(t.join(&first, 1).0, "a join before anything died is placed");
    // X: a landless process under a name of its own, linked to both hosts.
    let x = t.restart(&db, &dir, h(9), 4100);
    assert!(t.w.claims[x.0 as usize].fence().epochs.is_empty(), "X owns nothing");
    t.until("the gate synced with X", WAIT, |t| {
        let m = &t.sim(h(0)).manager;
        m.peer_is_reachable(x) && m.peers_synced()
    });
    t.kill(x);
    exec(&db, &format!("UPDATE hosts SET state = {}, seen_ms = 0 WHERE id = {}", game::HOST_DEAD, x.0));
    // Another process registers: X's dead row, owning nothing, is pruned.
    let y = t.restart(&db, &dir, h(10), 4101);
    assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM hosts WHERE id = {}", x.0)), 0, "X's row is pruned");
    t.until("Y attached and every peer of the gate synced", WAIT, |t| {
        t.sim(y).manager.world_attached() && t.sim(h(0)).manager.peers_synced()
    });
    assert!(t.sim(h(0)).manager.is_forgotten(x), "the gate forgot X");
    let (second, _) = t.player(42, s0);
    let (ok, at) = t.join(&second, 1);
    assert!(ok, "a join after X's row was pruned is placed (not refused: the gate awaits no sync of X)");
    assert_eq!(at, Some(h(0)), "on the host holding the record's region");
    assert_eq!(t.w.store.world_generation().unwrap(), generation, "the generation never moves");
    t.stop(&db, &dir);
}

