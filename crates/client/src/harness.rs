//! Headless, deterministic, tick-by-tick driver: a real LocalServer
//! (WorldManager<InlineSpawner>) + a real GameInstanceManager client, wired
//! over the existing channels and advanced in lockstep. No Bevy, no threads,
//! no wall clock. See docs/superpowers/specs/2026-07-06-headless-sim-test-harness-design.md.

use std::collections::{BTreeSet, HashMap, VecDeque};

use crossbeam::channel::{unbounded, Receiver, Sender};
use game::{
    state_hash, ClientId, ClientPacket, ClientUpdateEvent, Credential, GameEventKind, InputEvent, Key, PlayerId,
    RegionCoords, ServerPacket,
};

use crate::connections::{ConnEvent, ConnHandle, ConnOpener, LOGIN_CONN};
use crate::{GameInstanceManager, LocalServer, LOCAL_CLIENT_ID};

/// Regions per super-region side in [`SimHarness::with_hosts`]: one, so host seams lie
/// between neighbouring regions and a 3x3 window spans several hosts.
pub const HARNESS_SUPER_REGION_REGIONS: u32 = 1;
/// Most server rounds [`SimHarness::with_hosts`] waits for every peer session to be up.
const PEER_CONNECT_ROUNDS: usize = 40;

/// The client's connection to one host (each host gets at most one at a time).
struct FakeConn {
    /// The host (index into `servers`).
    host: usize,
    /// Its key on the client's event channel (the one the client opened it with).
    key: u32,
    /// What the client sends on it.
    out: Receiver<ClientPacket>,
    /// Its session started at the host (the home's at once; an observer's once the
    /// host holds a matching observer order, as a server's edge admits it).
    admitted: bool,
    /// The observer ticket it presented: `(client, nonce)`.
    ticket: Option<(ClientId, u64)>,
}

/// A connection the client asked for ([`HarnessOpener`]).
struct Opened {
    key: u32,
    ordinal: u32,
    credential: Credential,
    out: Receiver<ClientPacket>,
}

/// The harness's transport for connections to other hosts: each `open` is
/// picked up by the next step.
struct HarnessOpener(Sender<Opened>);

impl ConnOpener for HarnessOpener {
    fn open(&mut self, key: u32, endpoint: &game::HostEndpoint, credential: Credential) -> ConnHandle {
        let (send, out) = unbounded();
        let _ = self.0.send(Opened { key, ordinal: endpoint.ordinal, credential, out });
        ConnHandle { send }
    }
}

pub struct SimHarness {
    /// One per host of the world (one, unless [`Self::with_hosts`]).
    servers: Vec<LocalServer>,
    /// The host the client logs in to.
    home_host: usize,
    client: GameInstanceManager,
    /// Each host's packets to the client.
    server_out: Vec<Receiver<ServerPacket>>,
    bridge_recv: Receiver<ClientUpdateEvent>,
    /// Debug toggles shared with the manager — tests flip these the way the
    /// render thread's F3 dev panel would.
    pub debug: game::DebugFlags,
    held: BTreeSet<Key>,
    /// Keys released since the last `step()`, awaiting their one-shot key-up
    /// edge. The sim's `InputState` only leaves `Held` on a `pressed:false`
    /// event, so a release MUST send that edge or the key stays held forever
    /// (movement reads `key_held`).
    released: BTreeSet<Key>,
    /// Authoritative server region state hashes, keyed by (region, that
    /// region's tick), each taken at the START of the slot — right after the
    /// Tick event, before the slot's held inputs and transfers (the state the
    /// desync check hashes, and what a client region holds at that tick: it
    /// predicts a slot's inputs and releases its held transfers only on its
    /// way to the next tick). Recorded every `step()` by `server.tick()`. The
    /// client is a bit-exact but ~1-tick-delayed mirror of the server in this
    /// lockstep model (the server snapshots a region then immediately ticks it
    /// forward before the client observes the snapshot), so convergence is
    /// checked tick-aligned: the client's `(region, tick)` must equal the
    /// server's authoritative hash recorded FOR THAT SAME TICK — not the
    /// server's live (already-advanced) hash. (That describes a client that
    /// trails; one that leads is aligned by `catch_up`.)
    server_hashes: HashMap<(RegionCoords, game::Tick), u32>,
    /// What the client actually reads: each host's packets keyed by the client's
    /// connection to it, forwarded by `pump_client`, which can withhold
    /// `SyncClock` (see `catch_up`).
    client_in_send: Sender<(u32, ConnEvent)>,
    client_in: Receiver<(u32, ConnEvent)>,
    /// The client's regions run on their own threads (`threaded`); every
    /// pump then waits for them (`settle_regions`), so a step stays lockstep.
    threaded: bool,
    /// The client's connections, one per host at most (the home's first).
    conns: Vec<FakeConn>,
    /// Connections the client opened since the last step.
    opened: Receiver<Opened>,
    /// Each host's inbound client packets.
    server_in: Vec<Sender<ClientPacket>>,
    /// Observer orders each host received from its peers: `(client, player, nonce, the
    /// step its edge has it from)`.
    orders: Vec<Vec<(ClientId, PlayerId, u64, usize)>>,
    /// Ticks added to each host's `SyncClock` samples (`skew_clock`).
    clock_skew: Vec<i64>,
    /// One-way delay in steps (`set_latency`), every connection alike; 0 = the
    /// lockstep default.
    latency: usize,
    /// Packets in flight, each with the step it is delivered at (and the host /
    /// connection key it goes to).
    to_server: VecDeque<(usize, usize, ClientPacket)>,
    to_client: VecDeque<(usize, u32, ServerPacket)>,
    /// Steps taken (the clock `latency` counts in).
    steps: usize,
    /// Extra steps for the next step's client packets only (`delay_next_send`).
    extra_delay: usize,
    /// Hosts whose packets to the client are held back (`hold_host`), and what is held.
    held_hosts: BTreeSet<usize>,
    held_packets: Vec<(usize, u32, ServerPacket)>,
    /// Steps an observer order takes from its host's manager to its edge
    /// (`orders_late`): an observer connection whose ticket the edge has no order for
    /// is then refused, as a server's edge refuses it; with 0 it waits for the order.
    order_delay: usize,
    /// Observer connections refused or closed so far, by host (`observer_closes`).
    closes: Vec<usize>,
    /// UI requests to the client (`request`).
    requests: Sender<crate::instance::ClientRequest>,
    /// `RequestLod` / `ReleaseLod` packets each host got from the client (`lod_traffic`).
    lod_traffic: Vec<(usize, usize)>,
}

/// Most server-only ticks `catch_up` runs — comfortably more than the
/// client's lead band (`CLIENT_LEAD_MS` in `regions::clock`).
const CATCH_UP_MAX_TICKS: usize = 8;

/// Ticks `cross_boundary` spends ascending before it heads for the seam
/// (20 ticks of creative flight ≈ 7.5 blocks).
const CROSSING_CLIMB_TICKS: usize = 20;

impl SimHarness {
    /// Wire a client to a fresh local authoritative server. Mirrors the
    /// local_server.rs test setup. LOD ring subscriptions are OFF (empty
    /// table): every key the client requests costs the LocalServer a
    /// synchronous full-region build, minutes of wall clock at the
    /// production table's radius. LOD-focused tests opt back in with
    /// `set_lod_table`.
    pub fn new() -> Self {
        Self::with_config(&[], 0)
    }

    /// `new`, with an explicit LOD ring table (see `new` for why the
    /// default is empty).
    pub fn with_lod_table(lod_table: &'static [(i32, u32)]) -> Self {
        Self::with_config(lod_table, 0)
    }

    /// Build a harness with an explicit near-region radius. Crossing tests
    /// opt into radius 1; home-only tests keep the one-region default.
    pub fn with_near_radius(near_radius: i32) -> Self {
        Self::with_config(&[], near_radius)
    }

    /// `with_near_radius`, with the client's regions on their own threads
    /// as in native play. Region state is then only reachable through the
    /// regions themselves (`client_home_data` is unavailable).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn threaded(near_radius: i32) -> Self {
        let mut h = Self::with_config(&[], near_radius);
        h.client.use_region_threads();
        h.threaded = true;
        h
    }

    /// `with_near_radius`, with the server's regions seeded by `genesis`
    /// instead of the lightweight flat world.
    pub fn with_genesis(near_radius: i32, genesis: game::InlineStudGenesis) -> Self {
        Self::build(&[], near_radius, genesis)
    }

    fn with_config(lod_table: &'static [(i32, u32)], near_radius: i32) -> Self {
        Self::build(lod_table, near_radius, crate::local_server::lightweight_test_genesis())
    }

    fn build(lod_table: &'static [(i32, u32)], near_radius: i32, genesis: game::InlineStudGenesis) -> Self {
        game::set_hash_mode(game::HashMode::Full); // enforce hash(before)==hash(after undo) along the driven path
        let (game_event_send, game_event_recv) = unbounded::<GameEventKind>();
        let (bridge_send, bridge_recv) = unbounded::<ClientUpdateEvent>();
        // Dummy address: native netcode is never constructed in the harness.
        let addr = "127.0.0.1:0".parse().unwrap();
        let debug = game::DebugFlags::default();
        let mut client = GameInstanceManager::new(
            game_event_send,
            game_event_recv,
            bridge_send,
            addr,
            debug.clone(),
        );
        client.set_lod_table(lod_table);
        client.set_near_radius(near_radius);

        let (opener, opened) = unbounded();
        client.set_conn_opener(Box::new(HarnessOpener(opener)));
        let (requests, request_recv) = unbounded();
        client.set_request_recv(request_recv);
        let home = FakeConn { host: 0, key: LOGIN_CONN, out: client.client_packet_recv(), admitted: true, ticket: None };
        let (server_in, client_to_server) = unbounded::<ClientPacket>();
        let (server_to_client_send, server_to_client) = unbounded::<ServerPacket>();
        let (client_in_send, client_in) = unbounded();
        let server = LocalServer::new_with_genesis(client_to_server, server_to_client_send, genesis).expect("local server");
        let mut server = server;
        server.record_slot_start_hashes();

        Self {
            servers: vec![server],
            home_host: 0,
            client,
            server_out: vec![server_to_client],
            bridge_recv,
            debug,
            held: BTreeSet::new(),
            released: BTreeSet::new(),
            server_hashes: HashMap::new(),
            client_in_send,
            client_in,
            threaded: false,
            conns: vec![home],
            opened,
            server_in: vec![server_in],
            orders: vec![Vec::new()],
            clock_skew: vec![0],
            latency: 0,
            to_server: VecDeque::new(),
            to_client: VecDeque::new(),
            steps: 0,
            extra_delay: 0,
            held_hosts: BTreeSet::new(),
            held_packets: Vec::new(),
            order_delay: 0,
            closes: vec![0],
            requests,
            lod_traffic: vec![(0, 0)],
        }
    }

    /// `hosts` hosts of one world (`Ownership::Static`,
    /// [`HARNESS_SUPER_REGION_REGIONS`] regions per super-region side), each an
    /// in-process manager on the lightweight flat world, every pair joined by an
    /// `InlinePeerLink` (no delay). The client logs in to the host that owns the spawn
    /// region and holds one fake connection per host it is sent to — its home's from
    /// the start, another's once its home's `RegionElsewhere` sends it there; the
    /// same `set_latency` delays every connection. Near radius 1 (the window spans
    /// host seams), LOD off.
    pub fn with_hosts(hosts: u32) -> Self {
        assert!(hosts >= 1);
        let s = HARNESS_SUPER_REGION_REGIONS;
        let mut h = Self::with_config(&[], 1);
        let home = game::Ownership::Static { ordinal: 0, hosts }.owner_of(game::SPAWN_REGION, s) as usize;
        // Rebuild the servers as `hosts` unconnected hosts: the one-host server
        // `build` made has the local client's session and no peers.
        let mut servers = Vec::new();
        let mut server_out = Vec::new();
        let mut server_in = Vec::new();
        for ordinal in 0..hosts {
            let (to_server, from_client) = unbounded::<ClientPacket>();
            let (to_client, from_server) = unbounded::<ServerPacket>();
            let mut server =
                LocalServer::new_unconnected(from_client, to_client, crate::local_server::lightweight_test_genesis())
                    .expect("local server");
            let manager = server.manager_mut();
            manager.set_ownership(game::Ownership::Static { ordinal, hosts }, s);
            manager.set_endpoints(harness_endpoint(ordinal), game::seeded_nonces(0x5eed + u64::from(ordinal)));
            server.record_slot_start_hashes();
            servers.push(server);
            server_out.push(from_server);
            server_in.push(to_server);
        }
        for a in 0..hosts {
            for b in a + 1..hosts {
                let (ea, eb) = game::InlinePeerLink::pair(0xabcd + u64::from(a * hosts + b), 0);
                servers[a as usize].manager_mut().set_peer(b, game::PeerSession::new(harness_hello(a), b), Box::new(ea));
                servers[b as usize].manager_mut().set_peer(a, game::PeerSession::new(harness_hello(b), a), Box::new(eb));
            }
        }
        h.servers = servers;
        h.server_out = server_out;
        h.server_in = server_in;
        h.orders = vec![Vec::new(); hosts as usize];
        h.clock_skew = vec![0; hosts as usize];
        h.closes = vec![0; hosts as usize];
        h.lod_traffic = vec![(0, 0); hosts as usize];
        h.home_host = home;
        h.conns[0].host = home;
        // Every peer session up before the client logs in: its window's
        // regions on other hosts are then reachable from the home.
        let window = game::SPAWN_REGION.window_3x3();
        for _ in 0..PEER_CONNECT_ROUNDS {
            if h.servers.iter_mut().all(|server| window.iter().all(|rc| server.manager_mut().reaches(*rc))) {
                break;
            }
            h.tick_server();
        }
        assert!(
            h.servers.iter_mut().all(|server| window.iter().all(|rc| server.manager_mut().reaches(*rc))),
            "the harness hosts' peer sessions never came up"
        );
        // What the hosts said before the client logged in is no one's.
        for out in &h.server_out {
            for _ in out.try_iter() {}
        }
        h.servers[home].connect_local_client();
        h
    }

    /// The host that owns `rc` (index into the harness's hosts).
    pub fn owner(&mut self, rc: RegionCoords) -> u32 {
        self.servers[0].manager_mut().owner_of(rc)
    }

    /// The host the client logged in to.
    pub fn home_host(&self) -> u32 {
        self.home_host as u32
    }

    /// The host of the client connection `rc`'s traffic counts from (`None` before
    /// the home named itself).
    pub fn client_region_host(&self, rc: RegionCoords) -> Option<u32> {
        self.client.region_host(rc)
    }

    /// The host of the client's home connection.
    pub fn client_home_host(&self) -> Option<u32> {
        self.client.home_host()
    }

    /// The hosts the client holds a connection to (as the client has them).
    pub fn client_connections(&self) -> Vec<u32> {
        let mut hosts: Vec<u32> = self.client.connection_hosts().into_iter().filter_map(|(_, host)| host).collect();
        hosts.sort();
        hosts
    }

    /// The kind of the client's session on host `host`, if it has one there.
    pub fn server_session(&mut self, host: u32) -> Option<game::SessionKind> {
        self.servers[host as usize].manager_mut().session_kind(LOCAL_CLIENT_ID)
    }

    /// Host `host`'s manager (scenario setup: a world map, ownership).
    pub fn host_manager(&mut self, host: u32) -> &mut game::WorldManager<game::InlineSpawner> {
        self.servers[host as usize].manager_mut()
    }

    /// A UI request, as the client's UI sends it (handled at the next step).
    pub fn request(&mut self, request: crate::instance::ClientRequest) {
        let _ = self.requests.send(request);
    }

    /// Change the client's near window radius (picked up at the next tick).
    pub fn set_client_near_radius(&mut self, radius: i32) {
        self.client.set_near_radius(radius);
    }

    /// The client's LOD ring table from now on (see `with_lod_table`).
    pub fn set_client_lod_table(&mut self, table: &'static [(i32, u32)]) {
        self.client.set_lod_table(table);
    }

    /// `(RequestLod, ReleaseLod)` packets host `host` got from the client so far.
    pub fn lod_traffic(&self, host: u32) -> (usize, usize) {
        self.lod_traffic[host as usize]
    }

    /// The LOD keys the client holds subscribed.
    pub fn client_lod_keys(&self) -> usize {
        self.client.lod_subscriptions()
    }

    /// Home-region corrections over the session: how many, the largest (world units).
    pub fn corrections(&self) -> (u32, f32) {
        self.client.corrections()
    }

    /// Hold back everything host `host` sends the client (`true`) until released
    /// (`false`: delivered then, in order, behind what other hosts sent meanwhile) —
    /// one connection late against the others.
    pub fn hold_host(&mut self, host: u32, hold: bool) {
        if hold {
            self.held_hosts.insert(host as usize);
            return;
        }
        self.held_hosts.remove(&(host as usize));
        let (release, keep): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.held_packets).into_iter().partition(|(h, ..)| *h == host as usize);
        self.held_packets = keep;
        for (_, key, packet) in release {
            self.to_client.push_back((self.steps, key, packet));
        }
    }

    /// Every observer order reaches its host's edge `steps` steps after its manager got
    /// it, and a connection presenting a ticket the edge has no order for yet is
    /// refused, as a server's edge refuses it (the ticket came before the order). 0
    /// (the default): the connection waits for its order instead.
    pub fn orders_late(&mut self, steps: usize) {
        self.order_delay = steps;
    }

    /// How many of the client's connections to `host` were refused or closed so far.
    pub fn observer_closes(&self, host: u32) -> usize {
        self.closes[host as usize]
    }

    /// Host `host` closes the client's (observer) connection to it, as an edge does:
    /// the session there ends, and the client is told the connection closed.
    pub fn close_connection(&mut self, host: u32) {
        let host = host as usize;
        assert_ne!(host, self.home_host_now(), "close_connection is for an observer connection");
        let i = self.conns.iter().position(|c| c.host == host).expect("a connection to that host");
        let conn = self.conns.remove(i);
        if conn.admitted {
            self.servers[host].client_disconnected();
        }
        self.closes[host] += 1;
        let _ = self.client_in_send.send((conn.key, ConnEvent::Closed("closed by the host".to_string())));
    }

    /// The host the client's home connection is to now (the login host until it moves).
    fn home_host_now(&self) -> usize {
        self.client.home_host().map_or(self.home_host, |h| h as usize)
    }

    /// Every `SyncClock` host `host` sends from now on reads `ticks` later than
    /// its world tick (a host whose samples must not steer the client's clock).
    pub fn skew_clock(&mut self, host: u32, ticks: i64) {
        self.clock_skew[host as usize] = ticks;
    }

    /// Host `host`'s tick of its running region `rc` (0 if it does not run it).
    pub fn server_region_tick(&mut self, host: u32, rc: RegionCoords) -> game::Tick {
        self.servers[host as usize].region_tick(rc)
    }

    /// The server that runs `rc` (its owner's; the home's when none does).
    fn server_for(&mut self, rc: RegionCoords) -> &mut LocalServer {
        let owner = self.servers.iter_mut().position(|server| server.running_regions().contains(&rc));
        let i = owner.unwrap_or(self.home_host);
        &mut self.servers[i]
    }

    /// The client's connections opened since the last step join the harness;
    /// each waiting one is admitted once its host holds the observer order its
    /// ticket matches (a server's edge refuses it until then; here it waits).
    fn admit_connections(&mut self) {
        while let Ok(Opened { key, ordinal, credential, out }) = self.opened.try_recv() {
            let ticket = match credential {
                Credential::Observer { client, nonce } => Some((client, nonce)),
                _ => None,
            };
            assert!(
                self.conns.iter().all(|c| c.host != ordinal as usize),
                "the client opened a second connection to host {ordinal}"
            );
            assert!(self.conns.iter().all(|c| c.key != key), "the client reused connection key {key}");
            self.conns.push(FakeConn { host: ordinal as usize, key, out, admitted: false, ticket });
        }
        for (host, server) in self.servers.iter_mut().enumerate() {
            for (client, player, nonce, _ttl) in server.manager_mut().take_observer_orders() {
                self.orders[host].push((client, player, nonce, self.steps + self.order_delay));
            }
        }
        let home = self.home_host_now();
        let mut refused = Vec::new();
        for (i, conn) in self.conns.iter_mut().enumerate().filter(|(_, c)| !c.admitted) {
            assert_ne!(conn.host, home, "an observer ticket presented to the home host");
            let Some((client, nonce)) = conn.ticket else { continue };
            let found = self.orders[conn.host]
                .iter()
                .find(|(c, _, n, from)| *c == client && *n == nonce && *from <= self.steps)
                .copied();
            match found {
                Some((client, player, ..)) => {
                    self.servers[conn.host].observer_connected(client, player);
                    conn.admitted = true;
                }
                None if self.order_delay > 0 => refused.push(i),
                None => {}
            }
        }
        // Refused at the edge: the client is told why, and the connection ends.
        for i in refused.into_iter().rev() {
            let conn = self.conns.remove(i);
            self.closes[conn.host] += 1;
            let why = "observer ticket not valid on this host".to_string();
            let _ = self.client_in_send.send((conn.key, ConnEvent::Packet(ServerPacket::Refused(why))));
            let _ = self.client_in_send.send((conn.key, ConnEvent::Closed("refused".to_string())));
        }
    }

    /// The client's connections it closed (dropped) since the last step end, as a
    /// transport sees it: the session at that host ends; the client is told the
    /// connection closed (a late event of a connection it forgot already).
    fn reap_closed_connections(&mut self) {
        let mut i = 0;
        while i < self.conns.len() {
            let closed = matches!(self.conns[i].out.try_recv(), Err(crossbeam::channel::TryRecvError::Disconnected));
            if !closed {
                i += 1;
                continue;
            }
            let conn = self.conns.remove(i);
            if conn.admitted {
                self.servers[conn.host].client_disconnected();
            }
            self.closes[conn.host] += 1;
            let _ = self.client_in_send.send((conn.key, ConnEvent::Closed("closed by the client".to_string())));
        }
    }

    /// Delay every packet by `steps` steps each way (one step = one tick),
    /// and report the round trip in `SyncClock`, as a network would — the
    /// client then leads the server by it. 0 (the default) is the lockstep
    /// harness. `assert_converged` and `settle` expect 0: set it back first.
    pub fn set_latency(&mut self, steps: usize) {
        self.latency = steps;
        if steps == 0 {
            // Deliver everything still in flight.
            for (_, host, p) in self.to_server.drain(..) {
                let _ = self.server_in[host].send(p);
            }
            for (_, key, p) in self.to_client.drain(..) {
                let _ = self.client_in_send.send((key, ConnEvent::Packet(p)));
            }
        }
    }

    /// Client packets whose delivery step has come, on to the server.
    /// What the client sends during the next step reaches the server
    /// `steps` later than the latency says (a one-off late packet: the
    /// server applies its inputs late, its sim falls behind the client's
    /// prediction). Later packets keep their order behind it.
    pub fn delay_next_send(&mut self, steps: usize) {
        self.extra_delay = steps;
    }

    fn deliver_to_server(&mut self) {
        let extra = std::mem::take(&mut self.extra_delay);
        let due = self.steps + self.latency + extra;
        // What a connection the client then closed sent first still goes out.
        for conn in self.conns.iter().filter(|c| c.admitted) {
            for p in conn.out.try_iter() {
                self.to_server.push_back((due, conn.host, p));
            }
        }
        self.reap_closed_connections();
        self.admit_connections();
        for conn in self.conns.iter().filter(|c| c.admitted) {
            for p in conn.out.try_iter() {
                self.to_server.push_back((due, conn.host, p));
            }
        }
        while self.to_server.front().is_some_and(|(due, ..)| *due <= self.steps) {
            let (_, host, p) = self.to_server.pop_front().expect("checked");
            match p {
                ClientPacket::RequestLod(_) => self.lod_traffic[host].0 += 1,
                ClientPacket::ReleaseLod(_) => self.lod_traffic[host].1 += 1,
                _ => {}
            }
            let _ = self.server_in[host].send(p);
        }
    }

    /// Handshake: client requests its region and pumps until the configured
    /// near window has loaded (home-only by default).
    pub fn connect(&mut self) {
        self.client.start();
        // Drive a handful of steps so the RequestPlayerRegion -> PlayerRegion ->
        // Region-snapshot handshake and the initial window settle.
        for _ in 0..8 {
            self.step();
        }
    }

    /// Drain everything the manager has sent to the (absent) render bridge.
    pub fn drain_bridge_events(&mut self) -> Vec<ClientUpdateEvent> {
        self.bridge_recv.try_iter().collect()
    }

    /// Jitter the server's region scheduling from a seed, standing in for
    /// real region threads. Use `InlineSchedule::late` (or `shuffled`): they
    /// keep every region's tick count in lockstep with the client, which the
    /// tick-aligned `assert_converged` needs. `jittery` makes regions miss
    /// ticks, and this harness's client cannot pace itself against that.
    pub fn set_schedule(&mut self, schedule: game::InlineSchedule) {
        for server in &mut self.servers {
            server.set_schedule(Some(schedule.clone()));
        }
    }

    /// Send an `Order` as the local client (predicted at the next `step`).
    pub fn order(&mut self, order: game::Order) {
        let _ = self.client.push_game_event(GameEventKind::Order(LOCAL_CLIENT_ID, order));
    }

    pub fn press(&mut self, key: Key) {
        self.held.insert(key);
    }
    pub fn release(&mut self, key: Key) {
        // Only queue an up-edge if it was actually held, so release() without a
        // prior press() is a no-op (matches real edge-triggered input).
        if self.held.remove(&key) {
            self.released.insert(key);
        }
    }

    /// One deterministic tick: client predict -> server ingest+tick -> client reconcile.
    pub fn step(&mut self) {
        // 1. Client input + predict. Held keys re-send `pressed:true` each tick
        // (idempotent: InputState treats repeated press on a Held key as a
        // no-op); released keys send exactly one `pressed:false` up-edge so the
        // sim actually stops treating them as held.
        for &key in &self.held {
            let _ = self.client.push_game_event(GameEventKind::PlayerInput(
                LOCAL_CLIENT_ID,
                InputEvent::Key { key, pressed: true },
            ));
        }
        for &key in &self.released {
            let _ = self.client.push_game_event(GameEventKind::PlayerInput(
                LOCAL_CLIENT_ID,
                InputEvent::Key {
                    key,
                    pressed: false,
                },
            ));
        }
        self.released.clear();
        self.client.send_tick();
        self.pump_client(true);

        // 2. Server ingest + advance.
        self.tick_server();

        // 3. Client reconcile.
        self.pump_client(true);
        self.steps += 1;
    }

    /// Server ingest + one authoritative tick, recording each region's
    /// slot-start hash at every tick it ran so convergence can be checked
    /// tick-aligned (see `server_hashes` / `assert_converged`).
    fn tick_server(&mut self) {
        self.deliver_to_server();
        // Every host ticks every step: their world ticks stay equal.
        for server in &mut self.servers {
            server.pump().expect("server pump");
            server.tick();
            // Regions never run on two hosts: one map holds them all.
            for (rc, t, hash) in server.take_slot_start_hashes() {
                self.server_hashes.insert((rc, t), hash);
            }
        }
        // Link delay 0: what a host's step sent its peers (a departure over a host seam,
        // an observer order) reaches them before their next tick, as a departure over a
        // thread seam reaches its target region. Twice: one host's answer to another's
        // message goes back the same step.
        if self.servers.len() > 1 {
            for _ in 0..2 {
                for server in &mut self.servers {
                    server.pump_peers();
                }
            }
        }
    }

    /// Deliver the server's packets and let the client process them (plus any
    /// queued `Tick`). `with_clock: false` withholds `SyncClock`, so the
    /// client reconciles without re-pacing itself against the server.
    fn pump_client(&mut self, with_clock: bool) {
        let rtt = std::time::Duration::from_millis(2 * self.latency as u64 * game::TICK_RATE);
        for (host, out) in self.server_out.iter().enumerate() {
            let conn = self.conns.iter().find(|c| c.host == host && c.admitted);
            let Some(conn) = conn else {
                // No session of the client there yet: nothing is for it.
                for _ in out.try_iter() {}
                continue;
            };
            let skew = self.clock_skew[host];
            for packet in out.try_iter() {
                let packet = match packet {
                    ServerPacket::SyncClock(rc, rate, tick, _) => {
                        ServerPacket::SyncClock(rc, rate, tick.saturating_add_signed(skew), rtt)
                    }
                    p => p,
                };
                let key = conn.key;
                if !with_clock && matches!(packet, ServerPacket::SyncClock(..)) {
                    continue;
                }
                if self.held_hosts.contains(&host) {
                    self.held_packets.push((host, key, packet));
                } else {
                    self.to_client.push_back((self.steps + self.latency, key, packet));
                }
            }
        }
        while self.to_client.front().is_some_and(|(due, ..)| *due <= self.steps) {
            let (_, key, p) = self.to_client.pop_front().expect("checked");
            let _ = self.client_in_send.send((key, ConnEvent::Packet(p)));
        }
        self.client.pump(&self.client_in).expect("client pump");
        if self.threaded {
            self.client.settle_regions().expect("client regions");
            // What settling routed may have queued more (events, ticks).
            self.client.pump(&self.client_in).expect("client pump");
        }
    }

    pub fn step_n(&mut self, n: usize) {
        for _ in 0..n {
            self.step();
        }
    }

    // --- inspectors used by tests ---
    pub fn client_tick(&self) -> game::Tick {
        if self.threaded {
            return self.client.home_tick().expect("home region reports a tick");
        }
        let rc = self.client_home();
        self.client.world_ref().current_tick(&rc)
    }
    pub fn client_region_count(&self) -> usize {
        self.client.loaded_regions().len()
    }
    pub fn client_region_loaded(&self, rc: RegionCoords) -> bool {
        self.client.loaded_regions().contains(&rc)
    }

    /// Every client region's `(tick, state hash)`: read directly on the
    /// inline host, asked of the regions on threads.
    fn client_regions(&mut self) -> HashMap<RegionCoords, (game::Tick, u32)> {
        if self.threaded {
            return self.client.region_probe().expect("client regions").into_iter().collect();
        }
        let world = self.client.world_ref();
        world
            .loaded_regions()
            .into_iter()
            .map(|rc| (rc, (world.current_tick(&rc), state_hash(world.data(&rc)))))
            .collect()
    }
    /// Local player's body translation (region-local). Panics if called before
    /// the player exists (i.e. before `connect`).
    pub fn player_pos(&self) -> [f32; 3] {
        self.client
            .local_player_translation()
            .expect("player exists after connect")
    }
    /// The local body's WORLD translation, from the home region's view
    /// (`None` while the home holds no body: a crossing not yet landed).
    pub fn player_world_pos(&self) -> Option<[f32; 3]> {
        let t = self.client.local_player_translation()?;
        let off = self.client_home().world_offset();
        Some([t[0] + off[0], t[1] + off[1], t[2] + off[2]])
    }
    fn client_home(&self) -> RegionCoords {
        self.client.home_region()
    }

    /// Step with no new input until the client has drained all buffered server
    /// events (no snapshot/event still in flight). Client and server ticks
    /// never line up by construction (the client trails a fresh snapshot by a
    /// tick, then holds a lead over the server), so we settle on "nothing
    /// pending", not on tick equality.
    pub fn settle(&mut self) {
        for _ in 0..64 {
            self.step();
            if self.pending_events_empty() {
                return;
            }
        }
        // Not fatal on its own; assert_converged surfaces any real divergence.
    }

    /// Bit-exact convergence: after settling, each region the client holds must
    /// match the server's authoritative state RECORDED AT THE CLIENT'S TICK for
    /// that region (the client is an exact but ~1-tick-delayed mirror — see
    /// `server_hashes`). Fails if a held region diverges, or if nothing could
    /// be tick-aligned at all (convergence would otherwise be vacuous).
    ///
    /// Right after a boundary crossing a region can be
    /// bit-inexact for a FEW ticks (the just-flipped home; or a neighbour
    /// whose server copy got a transfer a slot late, which the client's
    /// prediction for that slot cannot know until the server closes it) — the spec-accepted transient flip-tick
    /// rubber-band (docs/superpowers/2026-07-06-seamless-local-crossing-followup.md),
    /// which reconciles within ~1 tick once input stops. `settle()` returns as
    /// soon as `pending_events_empty()` (true at the flip tick), so before the
    /// strict check we step idle up to a small bound to let that transient
    /// heal. This does NOT weaken the check — it still demands bit-exact
    /// agreement; it only permits the documented, bounded reconciliation the
    /// authority guarantees. Callers must not hold input across this call (the
    /// heal steps are meant to be idle); the tests release before asserting.
    /// A genuinely non-healing divergence (e.g. an input left stuck in a
    /// region's log) still exhausts the bound and fails loudly below.
    pub fn assert_converged(&mut self) {
        self.settle();
        const HEAL_BOUND: usize = 8;
        let home = self.client.home_region();
        let mut aligned = self.catch_up();
        for _ in 0..HEAL_BOUND {
            // Every aligned region healed (the home among them); none is
            // skipped by the strict check below either.
            if aligned.contains_key(&home) && aligned.values().all(|(_, client, server)| client == server) {
                break;
            }
            self.step();
            aligned = self.catch_up();
        }
        for rc in self.client.loaded_regions() {
            match aligned.get(&rc) {
                Some(&(t, client_hash, server_hash)) => {
                    assert_eq!(
                        client_hash, server_hash,
                        "client region {:?} @tick {} diverges from the server's authoritative state at that tick",
                        rc, t
                    );
                }
                None => {
                    // The home region is the region under test — it MUST be
                    // tick-alignable, or convergence is unverified there. A
                    // window neighbour we can't yet align (just-subscribed,
                    // its tick not yet recorded) is tolerated; the home region
                    // is not. Without this, a static neighbour matching would
                    // let a genuinely diverging fresh home region pass silently
                    // (e.g. right after cross_boundary / teleport_player).
                    assert!(
                        rc != home,
                        "home region {:?} @tick {} could not be tick-aligned with the server within {} catch-up ticks — out of sync with the authority (convergence unverified)",
                        rc,
                        self.client.world_ref().current_tick(&rc),
                        CATCH_UP_MAX_TICKS
                    );
                }
            }
        }
        assert!(
            aligned.contains_key(&home),
            "home region {:?} was not tick-aligned+verified against the server — convergence is vacuous",
            home
        );
    }

    /// Every loaded client region stands within one tick of every other: they
    /// all run to the one predicted world tick (`regions::clock`). Call after
    /// a settle — a region still catching up a fresh snapshot is behind.
    pub fn assert_regions_in_step(&mut self) {
        let ticks: Vec<(RegionCoords, game::Tick)> =
            self.client_regions().into_iter().map(|(rc, (t, _))| (rc, t)).collect();
        let lo = ticks.iter().map(|(_, t)| *t).min().expect("a loaded region");
        let hi = ticks.iter().map(|(_, t)| *t).max().expect("a loaded region");
        assert!(hi - lo <= 1, "client regions not on one world tick: {ticks:?}");
    }

    /// Liveness: holding `key` advances the sim tick AND moves the player body.
    pub fn assert_progresses(&mut self, key: Key) {
        let t0 = self.client_tick();
        let p0 = self.player_pos();
        self.press(key);
        self.step_n(4);
        self.release(key);
        assert!(
            self.client_tick() > t0,
            "sim tick did not advance while holding {:?}",
            key
        );
        assert!(
            self.player_pos() != p0,
            "player did not move while holding {:?} (input frozen?)",
            key
        );
    }

    /// Server-side state surgery on a running region, e.g. before
    /// `connect()` so the client's snapshot already holds the change.
    pub fn with_server_region(&mut self, rc: RegionCoords, f: impl FnOnce(&mut game::Region)) {
        let owner = self.servers[0].manager_mut().owner_of(rc) as usize;
        self.servers[owner].with_region(rc, f);
    }

    /// A client region's rollback state, if loaded (inline host only).
    pub fn client_region_data(&self, rc: RegionCoords) -> Option<&game::Rollback> {
        self.client.world_ref().regions.contains_key(&rc).then(|| self.client.world_ref().data(&rc))
    }

    /// The client's mispredictions over the whole session.
    pub fn total_mispredictions(&self) -> u64 {
        self.client.total_mispredictions()
    }

    pub fn player_region(&self) -> RegionCoords {
        self.client.home_region()
    }

    /// Authoritative teleport for scenario setup (server-side, undo-safe).
    pub fn teleport_player(&mut self, pos: [f32; 3]) {
        self.servers[self.home_host].teleport_local_player(pos);
        self.settle();
    }

    /// Fly in `dir` and step (bounded) until the client's
    /// home region changes AND the player entity is actually queryable there
    /// — i.e. the player crossed a seam and the crossing has fully landed.
    ///
    /// `home_region` flips as soon as the authoritative `PlayerRegion` packet
    /// is handled, which can land one tick before the predicted
    /// `apply_local_transfers` path creates the player entity in the new
    /// region's data (`GameInstanceManager::local_player_translation` briefly
    /// returns `None` in that window). Waiting on both conditions makes
    /// "crossed" mean what callers actually need (`player_pos()` etc. are
    /// safe to call immediately after), without loosening any assertion.
    ///
    /// The final `settle()` gives the just-crossed region's authoritative
    /// confirmation one more round to land: the exact tick this loop breaks
    /// on can be transiently one step behind reconciliation (same class of
    /// lag as the home-flip/entity-queryable gap above), which otherwise
    /// makes an `assert_converged()` called immediately after `cross_boundary`
    /// spuriously see a same-tick hash mismatch that self-heals on the very
    /// next `step()`.
    pub fn cross_boundary(&mut self, dir: Dir) {
        let start = self.player_region();
        let key = match dir {
            Dir::North => Key::KeyW,
            Dir::South => Key::KeyS,
            Dir::East => Key::KeyD,
            Dir::West => Key::KeyA,
        };
        // fps-cam must be on for movement; toggle it first.
        self.press(Key::KeyE);
        self.step();
        self.release(Key::KeyE);
        self.step();
        // Fly there: a walking body stops at the first ledge taller than a
        // step. Double-tap Space enters flight; holding the second tap climbs
        // clear of the terrain around the spawn.
        self.press(Key::Space);
        self.step();
        self.release(Key::Space);
        self.step();
        self.press(Key::Space);
        self.step_n(CROSSING_CLIMB_TICKS);
        self.release(Key::Space);
        self.step();
        self.press(key);
        for _ in 0..400 {
            self.step();
            if self.player_region() != start && self.client.local_player_translation().is_some() {
                break;
            }
        }
        self.release(key);
        assert_ne!(
            self.player_region(),
            start,
            "player never crossed a boundary"
        );
        assert!(
            self.client.local_player_translation().is_some(),
            "player entity not queryable in the new home region after crossing"
        );
        self.settle();
    }

    /// Unconfirmed local-player inputs (PlayerInput / UiAction / Order) in
    /// the client's home region's log.
    pub fn pending_local_inputs(&self) -> usize {
        let home = self.client.home_region();
        self.client
            .world_ref()
            .regions
            .get(&home)
            .map_or(0, |r| r.pending_events().filter(|e| e.kind.input_client() == Some(LOCAL_CLIENT_ID)).count())
    }

    pub fn pending_events_empty(&self) -> bool {
        self.client.pending_events_empty()
    }

    /// The client's home region's full rollback state, for determinism checks
    /// (`state_hash(h.client_home_data())`).
    pub fn client_home_data(&self) -> &game::Rollback {
        self.client.world_ref().data(&self.client.home_region())
    }

    /// Does the client's home region currently match the server's authoritative
    /// hash recorded at the client's home tick? `Some(true|false)`, or `None` if
    /// the server never recorded that region at that tick.
    /// Tick-align client and server and return, per client region,
    /// `(tick, client hash, server hash)` AT THE CLIENT'S CURRENT TICK.
    ///
    /// The client holds a lead over the server (`CLIENT_LEAD_MS` in
    /// `regions::clock`; every region runs to the one predicted tick), so a
    /// region's client tick is usually one the server has not reached. The
    /// client's past states can't be read back, and a hash noted when a tick
    /// was first predicted is no substitute — the server's per-tick events (a
    /// halo mirror refreshing while the player stands in a seam band)
    /// rewrite every predicted tick on arrival. So the CLIENT is frozen while
    /// the server alone ticks up to it, the client reconciling what arrives;
    /// `SyncClock` is withheld for the duration, or the client would step
    /// its clock forward to win its lead back. Both sides then hold final state
    /// for the same tick, and the comparison is as strict as it ever was.
    /// Afterwards the client predicts the same number of ticks alone, which
    /// restores the lead it had — the run continues as if uninterrupted.
    fn catch_up(&mut self) -> HashMap<RegionCoords, (game::Tick, u32, u32)> {
        let mut steps = 0;
        let mut client = self.client_regions();
        while steps < CATCH_UP_MAX_TICKS
            && client.iter().any(|(rc, (t, _))| {
                !self.server_hashes.contains_key(&(*rc, *t)) && self.server_for(*rc).region_tick(*rc) < *t
            })
        {
            self.tick_server();
            self.pump_client(false);
            steps += 1;
            client = self.client_regions();
        }

        let aligned = client
            .into_iter()
            .filter_map(|(rc, (t, hash))| {
                let server = *self.server_hashes.get(&(rc, t))?;
                Some((rc, (t, hash, server)))
            })
            .collect();

        for _ in 0..steps {
            self.client.send_tick();
            self.pump_client(false);
        }
        aligned
    }
}

/// Cardinal directions for `SimHarness::cross_boundary`.
#[derive(Copy, Clone, Debug)]
pub enum Dir {
    East,
    West,
    North,
    South,
}

impl Default for SimHarness {
    fn default() -> Self {
        Self::new()
    }
}

/// Host `ordinal`'s endpoint in [`SimHarness::with_hosts`] (no addresses: the harness
/// connects in process).
fn harness_endpoint(ordinal: u32) -> game::HostEndpoint {
    game::HostEndpoint { ordinal, quic: None, webtransport: None, cert_hash: None }
}

/// The hello host `ordinal` of [`SimHarness::with_hosts`] starts its peer sessions with.
fn harness_hello(ordinal: u32) -> game::PeerHello {
    game::PeerHello {
        ordinal,
        boot: 1000 + u64::from(ordinal),
        generation: 1,
        endpoint: harness_endpoint(ordinal),
        received_through: 0,
        received_from_boot: None,
    }
}
