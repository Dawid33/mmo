//! R3 Task 16: observer sessions at the server's connect edge. A client whose home host
//! (A) sent it on to another host (B, `RegionElsewhere`) connects to B with the ticket A
//! vouched for (`Credential::Observer`): B admits it against A's order, under the id A knows
//! the session by, places no body, lets it subscribe to B's regions, and makes it the home
//! session when its body arrives by hand-off.
//!
//! Two hosts in this process — A and B, each a `WorldManager<InlineSpawner>` with a shared
//! inline coordinator and an in-process peer link — and B's real connect edge
//! (`WorldIngress`: QUIC, the edge's sinks and writer). The test drives both managers and
//! sits between B's manager and its edge, logging what passes. Its own test binary:
//! process-global block props, sockets.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver, Sender};
use game::{
    ClientId, ClientPacket, Credential, HostEndpoint, InlinePeerLink, InlineSpawner, Ownership, PeerHello, PeerSession,
    PlayerId, PlayerRecord, RegionCoords, RegionOutput, ServerEvent, ServerPacket, WorldManager, OBSERVER_IDLE_CLOSE_MS,
    OBSERVER_IDLE_REFUSAL, REGION_SIZE,
};
use server::auth_edge::{Authenticator, Clock, ObserverOrders, OBSERVER_ORDERS_MAX, OBSERVER_ORDERS_PER_CLIENT, REFUSE_OBSERVER};
use server::player_store::{self, REFUSE_REPLACED};

/// Wall time any wait below may take before the test fails.
const WAIT: Duration = Duration::from_secs(30);
const HOSTS: u32 = 2;
/// Regions per super-region side.
const S: u32 = 4;
/// The player the tests connect, and its session id (the same on both hosts).
const PLAYER: PlayerId = PlayerId(5);
const CLIENT: ClientId = (5 << 24) | 1;

// ---- the world ------------------------------------------------------------------------

fn install_props() {
    let registry = game::BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
    game::install_block_props(registry.props());
}

/// Blocks of ground under every column of the flat fixture: its top is this many metres
/// up (6 m).
const GROUND_BLOCKS: usize = 6;

/// Flat ground under every chunk column of every region, ground without worldgen: grass
/// over dirt over stone, its top [`GROUND_BLOCKS`] up, stone below the band.
fn flat_floor(_rc: RegionCoords) -> Vec<(game::ColumnCoords, game::Column)> {
    static IDS: std::sync::OnceLock<[game::BlockId; 3]> = std::sync::OnceLock::new();
    let [grass, dirt, stone] = *IDS.get_or_init(|| {
        let registry = game::BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
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

/// A 16 × 16 grassland from the origin, every tile landing in its middle.
fn land_map() -> game::WorldMap {
    let mid = game::REGION_BLOCKS as u32 / 2;
    let tile = game::TileSummary {
        ground_min_m: 0.0,
        ground_mean_m: 10.0,
        ground_max_m: 10.0,
        relief_m: 10.0,
        slope: [0.0, 0.0],
        sea_fraction: 0.0,
        lake_fraction: 0.0,
        lake_level_m: None,
        rivers: 1,
        river_max_discharge_m2: 0.0,
        river_max_width_m: 0.0,
        surface_rock: None,
        landing: Some([mid, mid]),
        biome: game::Biome::Grassland,
        region: 0,
        temperature_c: 15.0,
        precip_mm: 800.0,
        savagery: 0,
        alignment: 0,
        volcano: false,
    };
    let tiles = vec![tile; 16 * 16];
    game::WorldMap { origin: RegionCoords::new(0, 0), width: 16, height: 16, tiles, rock_names: vec![], region_types: vec![] }
}

/// The adjacent pair of regions nearest the origin whose owners differ: `(a, b)`, `a` host
/// 0's (A), `b` host 1's (B).
fn host_seam() -> (RegionCoords, RegionCoords) {
    let owner = |rc: RegionCoords| Ownership::Static { ordinal: 0, hosts: HOSTS }.owner_of(rc, S);
    for z in 1..15 {
        for x in 1..14 {
            let (p, q) = (RegionCoords::new(x, z), RegionCoords::new(x + 1, z));
            match (owner(p), owner(q)) {
                (0, 1) => return (p, q),
                (1, 0) => return (q, p),
                _ => {}
            }
        }
    }
    panic!("no host seam in the map");
}

/// Where host `ordinal` says players reach it.
fn endpoint(ordinal: u32, port: u16) -> HostEndpoint {
    HostEndpoint {
        ordinal,
        quic: Some(format!("127.0.0.1:{port}")),
        webtransport: None,
        cert_hash: Some(format!("{ordinal:064x}")),
    }
}

fn hello(ordinal: u32, port: u16) -> PeerHello {
    PeerHello {
        ordinal,
        boot: 100 + u64::from(ordinal),
        generation: 0,
        endpoint: endpoint(ordinal, port),
        received_through: 0,
        received_from_boot: None,
    }
}

/// One host: its manager (inline regions) and what it sends clients.
struct Host {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    /// Every packet the manager sent, in order.
    log: Vec<(Option<ClientId>, ServerPacket)>,
}

impl Host {
    fn new(ordinal: u32, port: u16) -> Host {
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn game::LodBuilder> = Box::new(game::InlineLodBuilder {
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
        manager.set_ownership(Ownership::Static { ordinal, hosts: HOSTS }, S);
        manager.set_endpoints(endpoint(ordinal, port), game::seeded_nonces(0x5EED + u64::from(ordinal)));
        manager.set_world_map(land_map());
        Host { manager, region_out, packets, log: Vec::new() }
    }

    fn settle(&mut self, now: u64) {
        for _ in 0..3 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, now);
            }
        }
    }

    fn event(&mut self, ev: ServerEvent, now: u64) {
        assert!(self.manager.handle_server_event(ev, now), "the server must not quit");
    }

    /// What went to `client`, in order.
    fn to(&self, client: ClientId) -> Vec<&ServerPacket> {
        self.log.iter().filter(|(to, _)| *to == Some(client)).map(|(_, p)| p).collect()
    }
}

/// B's real connect edge: QUIC on `port`, its event channel, and the writer's input.
struct Edge {
    port: u16,
    orders: Arc<ObserverOrders>,
    events: Receiver<ServerEvent>,
    to_writer: Sender<(Option<ClientId>, ServerPacket)>,
    sinks: Arc<dashmap::DashMap<ClientId, server::ClientSink>>,
    /// Every event the edge sent B's manager, in order.
    log: Vec<ServerEvent>,
}

/// Two hosts with a shared coordinator and a peer link, B's edge listening.
struct World {
    a: Host,
    b: Host,
    edge: Edge,
    seam: (RegionCoords, RegionCoords),
    started: Instant,
    /// Added to B's clock (an idle observer's 30 s pass at once).
    skew_b: u64,
    /// While set, B's packets are kept from its edge's writer (in order), as a writer that
    /// lags behind its manager.
    hold_writer: bool,
    held: Vec<(Option<ClientId>, ServerPacket)>,
    /// Last: dropped after the edge's writer input (its writer task blocks on it).
    rt: tokio::runtime::Runtime,
}

impl World {
    fn new() -> World {
        install_props();
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let port = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut a = Host::new(0, 1);
        let mut b = Host::new(1, port);
        let (to_a, to_b) = InlinePeerLink::pair(7, 0);
        a.manager.set_peer(1, PeerSession::new(hello(0, 1), 1), Box::new(to_a));
        b.manager.set_peer(0, PeerSession::new(hello(1, port), 0), Box::new(to_b));
        // One coordinator over a history with nobody in it.
        let start = game::history::CalendarTick::from_days(
            game::history::rules::HISTORY_START_YEAR * game::history::DAYS_PER_YEAR,
        );
        let history =
            game::history::History::new(9, start, game::history::SocietyGrid::from_world_map(&land_map()));
        let core = game::coordinator::SharedCore::new(game::Coordinator::new(history, 0));
        let links: Vec<_> = (0..HOSTS).map(|h| game::InlineWorldLink::shared(core.clone(), h)).collect();
        for (h, link) in links.into_iter().enumerate() {
            let host = if h == 0 { &mut a } else { &mut b };
            host.manager.set_world_link(Box::new(link), 4000 + h as u64, 0);
        }
        // B's edge: any clock; observer tickets against the orders A sends.
        let orders = Arc::new(ObserverOrders::new(server::auth_edge::system_clock()));
        let auth = Arc::new(Authenticator::local().with_observer_orders(orders.clone()));
        let (event_send, events) = unbounded();
        let (to_writer, writer_recv) = unbounded();
        let players = player_store::shared(player_store::MemoryPlayerStore::default());
        let mut ingress = server::WorldIngress::new("127.0.0.1".parse().unwrap(), port, false, players, auth);
        let sinks = ingress.sinks();
        rt.spawn(async move { ingress.listen(event_send, writer_recv).await });
        let edge = Edge { port, orders, events, to_writer, sinks, log: Vec::new() };
        let mut world =
            World { a, b, edge, seam: host_seam(), started: Instant::now(), skew_b: 0, hold_writer: false, held: Vec::new(), rt };
        let (sa, sb) = world.seam;
        world.until("both hosts attached and reaching each other", |w| {
            w.a.manager.world_attached()
                && w.b.manager.world_attached()
                && w.a.manager.reaches(sb)
                && w.b.manager.reaches(sa)
        });
        world
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn now_b(&self) -> u64 {
        self.now() + self.skew_b
    }

    /// Both hosts tick once; events from B's edge go to B; both maintain; B's edge records
    /// the observer orders; packets are logged (B's go on to its edge).
    fn round(&mut self) {
        let (now, now_b) = (self.now(), self.now_b());
        self.a.manager.spawner_mut().tick_all();
        self.b.manager.spawner_mut().tick_all();
        for ev in self.edge.events.try_iter().collect::<Vec<_>>() {
            self.edge.log.push(clone_event(&ev));
            self.b.event(ev, now_b);
        }
        self.a.settle(now);
        self.b.settle(now_b);
        self.a.manager.maintain(now);
        self.b.manager.maintain(now_b);
        server::observer_pass(&mut self.b.manager, Some(&self.edge.orders), &self.edge.sinks);
        self.a.settle(now);
        self.b.settle(now_b);
        self.a.log.extend(self.a.packets.try_iter());
        for (to, packet) in self.b.packets.try_iter() {
            // The writer gets what the log keeps (`ServerPacket` is not `Clone`).
            let copy: ServerPacket = game::decode_packet(&game::encode_packet(&packet).unwrap()).unwrap();
            self.held.push((to, copy));
            self.b.log.push((to, packet));
        }
        if !self.hold_writer {
            for out in self.held.drain(..) {
                self.edge.to_writer.send(out).unwrap();
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    fn until(&mut self, what: &str, mut done: impl FnMut(&mut World) -> bool) {
        let deadline = Instant::now() + WAIT;
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

    /// `CLIENT`'s player, its body standing in the middle of `a` on host A.
    fn connect_home_on_a(&mut self) {
        let a = self.seam.0;
        let y = (GROUND_BLOCKS * game::BLOCK_VOXELS) as f32 + 3.0;
        let record = PlayerRecord {
            client: game::Client::default(),
            pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, y, REGION_SIZE / 2.0)),
            region: a,
            fortress: None,
            adventure: None,
        };
        let now = self.now();
        self.a.event(ServerEvent::ClientConnected { client: CLIENT, player: PLAYER, record: Some(record) }, now);
        self.until("the body stands on A", |w| w.a.manager.home_of(CLIENT) == Some(a));
    }

    /// The client asks A for `b` (B's): A answers `RegionElsewhere` with B's endpoint and a
    /// ticket, and orders B to expect it. The ticket, once B's edge holds the order.
    fn ticket_from_a(&mut self) -> game::ObserverTicket {
        let b = self.seam.1;
        let now = self.now();
        self.a.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(b), CLIENT), now);
        self.until("A sends the client on to B", |w| {
            w.a.to(CLIENT).iter().any(|p| matches!(p, ServerPacket::RegionElsewhere { .. }))
        });
        let ticket = self
            .a
            .to(CLIENT)
            .iter()
            .find_map(|p| match p {
                ServerPacket::RegionElsewhere { region, endpoint, ticket } => {
                    assert_eq!((*region, endpoint.ordinal), (b, 1), "B's region, B's endpoint");
                    Some(game::ObserverTicket { client: ticket.client, nonce: ticket.nonce })
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(ticket.client, CLIENT, "the ticket names the session's id");
        self.until("B's edge holds A's order", |w| !w.edge.orders.is_empty());
        ticket
    }

    /// A QUIC connection to B's edge whose first packet is `credential`'s `Hello`.
    fn dial(&mut self, credential: Credential) -> Client {
        let port = self.edge.port;
        let connection = self.rt.block_on(quic::connect(port));
        let (tx, packets) = unbounded();
        self.rt.spawn(read_packets(connection.clone(), tx));
        let client = Client { connection, packets, seen: Vec::new() };
        client.send(&self.rt, ClientPacket::Hello { credential });
        client
    }

    /// [`dial`](Self::dial) with `ticket`, and wait until B knows the observer session.
    fn observe(&mut self, ticket: &game::ObserverTicket) -> Client {
        let before = self.observer_connects();
        let client = self.dial(Credential::Observer { client: ticket.client, nonce: ticket.nonce });
        self.until("B's edge announces the observer", |w| w.observer_connects() > before);
        client
    }

    fn observer_connects(&self) -> usize {
        self.edge.log.iter().filter(|e| matches!(e, ServerEvent::ObserverConnected { .. })).count()
    }

    /// Rounds until `client` has received a packet `want` matches (or fail).
    fn until_received(&mut self, client: &mut Client, what: &str, mut want: impl FnMut(&ServerPacket) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            client.drain();
            if client.seen.iter().any(&mut want) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting: {what}; got {:?}", names(&client.seen));
            self.round();
        }
    }

    /// Rounds until `client`'s connection is closed (or fail).
    fn until_closed(&mut self, client: &Client, what: &str) {
        let deadline = Instant::now() + WAIT;
        while client.connection.close_reason().is_none() {
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            self.round();
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.a.manager.shutdown_all();
        self.b.manager.shutdown_all();
    }
}

/// The events the test logs (`ServerEvent` is not `Clone`; only these reach B from its edge).
fn clone_event(ev: &ServerEvent) -> ServerEvent {
    match ev {
        ServerEvent::ObserverConnected { client, player } => ServerEvent::ObserverConnected { client: *client, player: *player },
        ServerEvent::ClientDisconnected(c) => ServerEvent::ClientDisconnected(*c),
        ServerEvent::ClientConnected { client, player, record } => {
            ServerEvent::ClientConnected { client: *client, player: *player, record: record.clone() }
        }
        ServerEvent::ClientPacket(p, c) => {
            ServerEvent::ClientPacket(game::decode_packet(&game::encode_packet(p).unwrap()).unwrap(), *c)
        }
        other => panic!("unexpected event from the edge: {other:?}"),
    }
}

/// A test client's QUIC connection to B and what it received.
struct Client {
    connection: quinn::Connection,
    packets: Receiver<ServerPacket>,
    seen: Vec<ServerPacket>,
}

impl Client {
    fn send(&self, rt: &tokio::runtime::Runtime, packet: ClientPacket) {
        let connection = self.connection.clone();
        rt.block_on(async move {
            let mut stream = connection.open_uni().await.unwrap();
            stream.write_all(&game::encode_packet(&packet).unwrap()).await.unwrap();
            stream.finish().unwrap();
        });
    }

    fn drain(&mut self) {
        self.seen.extend(self.packets.try_iter());
    }
}

/// Each uni stream is one packet, accepted in the order the server opened them.
async fn read_packets(connection: quinn::Connection, tx: Sender<ServerPacket>) {
    while let Ok(mut stream) = connection.accept_uni().await {
        let Ok(bytes) = stream.read_to_end(256 << 20).await else { return };
        if let Ok(packet) = game::decode_packet::<ServerPacket>(&bytes) {
            let _ = tx.send(packet);
        }
    }
}

fn name(p: &ServerPacket) -> String {
    format!("{p:?}").chars().take_while(|c| c.is_alphanumeric()).collect()
}

fn names(ps: &[ServerPacket]) -> Vec<String> {
    ps.iter().map(name).collect()
}

// ---- tests ------------------------------------------------------------------------------

/// A ticket A vouched for admits the client on B under the id A knows it by: B hears
/// `ObserverConnected` (never `ClientConnected`), places no body and sends no world map;
/// the first packet on the connection is B's `Host`.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn observer_ticket_admits_without_placing_a_body() {
    let mut w = World::new();
    w.connect_home_on_a();
    let ticket = w.ticket_from_a();
    let mut client = w.observe(&ticket);
    w.until_received(&mut client, "B's Host packet", |p| matches!(p, ServerPacket::Host(_)));
    w.rounds(20);
    client.drain();
    assert!(
        matches!(&client.seen[0], ServerPacket::Host(e) if *e == endpoint(1, w.edge.port)),
        "the first packet is B's Host: {:?}",
        names(&client.seen)
    );
    assert!(
        matches!(&w.edge.log[..], [ServerEvent::ObserverConnected { client: CLIENT, player: PLAYER }]),
        "B heard only the observer: {:?}",
        w.edge.log
    );
    assert_eq!(w.b.manager.session_kind(CLIENT), Some(game::SessionKind::Observer));
    assert_eq!(w.b.manager.home_of(CLIENT), None, "no body on B");
    assert!(w.b.manager.running_regions().is_empty(), "nothing runs on B for an observer that subscribed nothing");
    for p in w.b.to(CLIENT) {
        assert!(
            !matches!(p, ServerPacket::WorldMap(_) | ServerPacket::PlayerRegion(..) | ServerPacket::Civs(_)),
            "an observer gets no join state: {}",
            name(p)
        );
    }
    assert_eq!(w.a.manager.home_of(CLIENT), Some(w.seam.0), "the body stays on A");
}

/// The edge's order book: an unknown client, a wrong nonce and an expired order are refused
/// alike; a valid ticket is admitted any number of times until it expires; the book is
/// bounded.
#[test]
fn unknown_or_expired_nonce_is_refused() {
    let clock_now = Arc::new(AtomicU64::new(1_000));
    let reading = clock_now.clone();
    let clock: Clock = Arc::new(move || reading.load(Ordering::SeqCst));
    let orders = ObserverOrders::new(clock.clone());
    orders.record(CLIENT, PLAYER, 42, 1_010);
    assert_eq!(orders.admit(CLIENT, 42, 1_000), Ok(PLAYER));
    assert_eq!(orders.admit(CLIENT, 42, 1_009), Ok(PLAYER), "again: valid until it expires");
    assert_eq!(orders.admit(CLIENT, 43, 1_000), Err(REFUSE_OBSERVER), "a wrong nonce");
    assert_eq!(orders.admit(CLIENT + 1, 42, 1_000), Err(REFUSE_OBSERVER), "another client");
    assert_eq!(orders.admit(CLIENT, 42, 1_010), Err(REFUSE_OBSERVER), "expired");

    // Through the authenticator and the connect edge's handshake: the same refusal, no session.
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let orders = Arc::new(orders);
    let auth = Authenticator::local().with_observer_orders(orders.clone());
    let store = player_store::shared(player_store::MemoryPlayerStore::default());
    let hello = |nonce| ClientPacket::Hello { credential: Credential::Observer { client: CLIENT, nonce } };
    let admitted = rt.block_on(player_store::resolve_session(&store, &auth, hello(42))).expect("admitted");
    assert_eq!((admitted.player, admitted.observer, admitted.record.is_none()), (PLAYER, Some(CLIENT), true));
    assert_eq!(admitted.client_id(&AtomicU64::new(0)), CLIENT, "the ticket's id is the session's");
    assert_eq!(rt.block_on(player_store::resolve_session(&store, &auth, hello(41))).unwrap_err(), REFUSE_OBSERVER);
    clock_now.store(1_010, Ordering::SeqCst);
    assert_eq!(rt.block_on(player_store::resolve_session(&store, &auth, hello(42))).unwrap_err(), REFUSE_OBSERVER);
    // A host with no order book refuses every observer ticket alike.
    let plain = Authenticator::local();
    assert_eq!(rt.block_on(player_store::resolve_session(&store, &plain, hello(42))).unwrap_err(), REFUSE_OBSERVER);

    // Bounded: expired orders go on the next record, a client keeps its newest few, the book
    // its latest-expiring.
    let book = ObserverOrders::new(clock);
    clock_now.store(2_000, Ordering::SeqCst);
    book.record(1, PLAYER, 1, 2_001);
    clock_now.store(2_001, Ordering::SeqCst);
    book.record(2, PLAYER, 2, 3_000);
    assert_eq!(book.len(), 1, "the expired order is pruned");
    for nonce in 0..(OBSERVER_ORDERS_PER_CLIENT as u64 + 3) {
        book.record(3, PLAYER, 100 + nonce, 3_000 + nonce);
    }
    assert_eq!(book.len(), 1 + OBSERVER_ORDERS_PER_CLIENT);
    assert_eq!(book.admit(3, 100, 2_001), Err(REFUSE_OBSERVER), "the client's oldest went");
    assert_eq!(book.admit(3, 100 + OBSERVER_ORDERS_PER_CLIENT as u64 + 2, 2_001), Ok(PLAYER));
    for client in 10..(10 + OBSERVER_ORDERS_MAX as u64 + 10) {
        book.record(client, PLAYER, client, 4_000 + client);
    }
    assert_eq!(book.len(), OBSERVER_ORDERS_MAX, "the book is capped");
    assert_eq!(book.admit(2, 2, 2_001), Err(REFUSE_OBSERVER), "the first to expire went first");

    // A peer's ttl is clamped to the protocol's.
    let book = ObserverOrders::new(Arc::new(|| 5_000));
    book.record_all(vec![(CLIENT, PLAYER, 9, u64::MAX)]);
    assert_eq!(book.admit(CLIENT, 9, 5_000 + game::OBSERVER_ORDER_TTL_SECS - 1), Ok(PLAYER));
    assert_eq!(book.admit(CLIENT, 9, 5_000 + game::OBSERVER_ORDER_TTL_SECS), Err(REFUSE_OBSERVER), "clamped");
    // A revoked client's orders go.
    book.revoke(CLIENT);
    assert_eq!(book.admit(CLIENT, 9, 5_000), Err(REFUSE_OBSERVER), "revoked");
}

/// The peer message that carries a nonce never prints it.
#[test]
fn observer_order_debug_hides_the_nonce() {
    let nonce = 0x1234_5678_9ABC_DEF0_u64;
    let order = game::Reliable::ObserverOrder { client: CLIENT, player: PLAYER, nonce: game::Nonce(nonce), ttl_secs: 120 };
    let shown = format!("{order:?} {order:#?}");
    for form in [format!("{nonce}"), format!("{nonce:x}"), format!("{nonce:X}")] {
        assert!(!shown.contains(&form), "{shown}");
    }
}

/// The edge never seats an observer over a Home session of its id (login-admitted or
/// promoted): the observer is handed back, nothing is told the manager, the occupant stays.
#[test]
fn seat_observer_never_replaces_a_home_session() {
    let sinks = dashmap::DashMap::new();
    let (send, events) = unbounded();
    sinks.insert(CLIENT, ("home", true));
    let seated = player_store::seat_observer(&sinks, &send, CLIENT, PLAYER, ("newcomer", false), |s| s.1);
    assert_eq!(seated, Err(("newcomer", false)), "refused");
    assert_eq!(*sinks.get(&CLIENT).unwrap(), ("home", true), "the occupant stays");
    assert!(events.try_recv().is_err(), "the manager hears nothing");
    // An observer replaces an observer.
    sinks.insert(CLIENT, ("older", false));
    let seated = player_store::seat_observer(&sinks, &send, CLIENT, PLAYER, ("newer", false), |s| s.1);
    assert_eq!(seated, Ok(Some(("older", false))));
    assert!(matches!(events.try_recv(), Ok(ServerEvent::ClientDisconnected(CLIENT))));
    assert!(matches!(events.try_recv(), Ok(ServerEvent::ObserverConnected { client: CLIENT, .. })));
}

/// Over the wire: a ticket with a wrong nonce is refused with the one refusal, and B hears
/// of no session.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn wrong_nonce_is_refused_at_the_edge() {
    let mut w = World::new();
    w.connect_home_on_a();
    let ticket = w.ticket_from_a();
    let mut client = w.dial(Credential::Observer { client: ticket.client, nonce: ticket.nonce ^ 1 });
    w.until_received(&mut client, "a refusal", |p| matches!(p, ServerPacket::Refused(_)));
    assert!(matches!(&client.seen[..], [ServerPacket::Refused(why)] if why == REFUSE_OBSERVER), "{:?}", client.seen);
    w.until_closed(&client, "the edge closes the refused connection");
    w.rounds(5);
    assert!(w.edge.log.is_empty(), "B never heard of it: {:?}", w.edge.log);
    assert_eq!(w.b.manager.session_kind(CLIENT), None);
}

/// An observer may only subscribe and send input for its own body: `PlayRegion`, `Embark`,
/// embark options, `Leave` and a `Quit` produce nothing on B and touch nothing on A.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn observer_cannot_play_or_embark() {
    let mut w = World::new();
    w.connect_home_on_a();
    let ticket = w.ticket_from_a();
    let mut client = w.observe(&ticket);
    w.until_received(&mut client, "B's Host packet", |p| matches!(p, ServerPacket::Host(_)));
    w.rounds(5);
    let (a, b) = w.seam;
    let before = w.b.to(CLIENT).len();
    let quit = game::GameEvent { kind: game::GameEventKind::Quit, id: 0, region_id: b };
    for packet in [
        ClientPacket::PlayRegion(b),
        ClientPacket::Embark(game::EmbarkRequest { civ: game::history::CivId(0), people: Vec::new(), region: b }),
        ClientPacket::RequestEmbarkOptions,
        ClientPacket::RequestPlayerRegion,
        ClientPacket::GameEventAfterTick(quit, 0),
        ClientPacket::Leave,
    ] {
        client.send(&w.rt, packet);
    }
    w.until("B got the six packets", |w| {
        w.edge.log.iter().filter(|e| matches!(e, ServerEvent::ClientPacket(..))).count() >= 6
    });
    w.rounds(20);
    // Only the calendar every session hears.
    let answered: Vec<String> =
        w.b.to(CLIENT)[before..].iter().map(|p| name(p)).filter(|n| n != "Calendar").collect();
    assert!(answered.is_empty(), "nothing answered: {answered:?}");
    assert_eq!(w.b.manager.session_kind(CLIENT), Some(game::SessionKind::Observer), "still an observer (Leave dropped)");
    assert_eq!(w.b.manager.home_of(CLIENT), None);
    assert!(w.b.manager.running_regions().is_empty(), "no body made on B");
    assert_eq!(w.a.manager.home_of(CLIENT), Some(a), "the body stays on A");
}

/// An observer subscribes to a region B owns and gets its snapshot over the connection.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn observer_subscribes_to_an_owned_region_and_gets_its_snapshot() {
    let mut w = World::new();
    w.connect_home_on_a();
    let ticket = w.ticket_from_a();
    let mut client = w.observe(&ticket);
    let b = w.seam.1;
    client.send(&w.rt, ClientPacket::RequestRegionConnection(b));
    w.until_received(&mut client, "b's snapshot", |p| matches!(p, ServerPacket::Region(rc, _) if *rc == b));
    assert_eq!(w.b.manager.home_of(CLIENT), None, "still no body on B");
}

/// A second connection with the same ticket replaces the first: the first is refused
/// ("replaced by a newer connection") and closed; B hears the old session end before the
/// new one starts, and the old connection's close ends nothing more.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn second_connection_with_the_same_ticket_replaces_the_first() {
    let mut w = World::new();
    w.connect_home_on_a();
    let ticket = w.ticket_from_a();
    let mut first = w.observe(&ticket);
    w.until_received(&mut first, "the first connection's Host", |p| matches!(p, ServerPacket::Host(_)));
    // Traffic for the first session is on its way when the second connects.
    let b = w.seam.1;
    first.send(&w.rt, ClientPacket::RequestRegionConnection(b));
    w.until_received(&mut first, "b's snapshot", |p| matches!(p, ServerPacket::Region(rc, _) if *rc == b));
    // The writer lags: b's events for the first session are still queued when the second
    // is seated (and its Host queued behind them).
    w.hold_writer = true;
    w.rounds(5);
    let mut second = w.observe(&ticket);
    w.rounds(3);
    w.hold_writer = false;
    w.until_received(&mut first, "the first connection's refusal", |p| matches!(p, ServerPacket::Refused(_)));
    assert!(
        first.seen.iter().any(|p| matches!(p, ServerPacket::Refused(why) if why == REFUSE_REPLACED)),
        "{:?}",
        names(&first.seen)
    );
    w.until_closed(&first, "the first connection closed");
    w.rounds(20);
    let sessions: Vec<String> = w
        .edge
        .log
        .iter()
        .filter_map(|e| match e {
            ServerEvent::ObserverConnected { client: CLIENT, .. } => Some("connected".to_string()),
            ServerEvent::ClientDisconnected(CLIENT) => Some("disconnected".to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(sessions, ["connected", "disconnected", "connected"], "the old one ends first, once");
    assert_eq!(w.b.manager.session_kind(CLIENT), Some(game::SessionKind::Observer), "the new session lives");
    // The new connection is the live one.
    second.send(&w.rt, ClientPacket::RequestRegionConnection(b));
    w.until_received(&mut second, "b's snapshot on the new connection", |p| matches!(p, ServerPacket::Region(rc, _) if *rc == b));
    assert!(matches!(second.seen[0], ServerPacket::Host(_)), "{:?}", names(&second.seen));
}

/// An observer with no subscription (and no body on its way) for
/// [`OBSERVER_IDLE_CLOSE_MS`] is closed; one with a subscription is not.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn idle_observer_is_closed() {
    let mut w = World::new();
    w.connect_home_on_a();
    let ticket = w.ticket_from_a();
    let mut client = w.observe(&ticket);
    let b = w.seam.1;
    client.send(&w.rt, ClientPacket::RequestRegionConnection(b));
    w.until_received(&mut client, "b's snapshot", |p| matches!(p, ServerPacket::Region(rc, _) if *rc == b));
    w.skew_b += OBSERVER_IDLE_CLOSE_MS + 1_000;
    w.rounds(10);
    assert_eq!(w.b.manager.session_kind(CLIENT), Some(game::SessionKind::Observer), "subscribed: kept");
    client.send(&w.rt, ClientPacket::ReleaseRegionConnection(b));
    w.until("B got the release", |w| {
        w.edge.log.iter().any(|e| matches!(e, ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(_), _)))
    });
    w.skew_b += OBSERVER_IDLE_CLOSE_MS / 2;
    w.rounds(10);
    assert_eq!(w.b.manager.session_kind(CLIENT), Some(game::SessionKind::Observer), "idle for less: kept");
    w.skew_b += OBSERVER_IDLE_CLOSE_MS / 2 + 1_000;
    w.until_received(&mut client, "the idle refusal", |p| matches!(p, ServerPacket::Refused(_)));
    assert!(
        client.seen.iter().any(|p| matches!(p, ServerPacket::Refused(why) if why == OBSERVER_IDLE_REFUSAL)),
        "{:?}",
        names(&client.seen)
    );
    w.until_closed(&client, "the idle connection closed");
    assert_eq!(w.b.manager.session_kind(CLIENT), None, "the session ended");
    assert_eq!(w.a.manager.home_of(CLIENT), Some(w.seam.0), "the body on A is untouched");
}

/// The body arrives on B by hand-off (the player plays B's region from A): B's observer
/// session becomes the home and is told `PlayerRegion`, then `Fortress`, `Calendar`,
/// `Sites` — never the world map or the civs again.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn observer_becomes_home_when_the_body_arrives() {
    let mut w = World::new();
    w.connect_home_on_a();
    let ticket = w.ticket_from_a();
    let mut client = w.observe(&ticket);
    w.until_received(&mut client, "B's Host packet", |p| matches!(p, ServerPacket::Host(_)));
    let b = w.seam.1;
    let now = w.now();
    w.a.event(ServerEvent::ClientPacket(ClientPacket::PlayRegion(b), CLIENT), now);
    w.until("the session on B is the home", |w| {
        w.b.manager.session_kind(CLIENT) == Some(game::SessionKind::Home) && w.b.manager.home_of(CLIENT) == Some(b)
    });
    w.until_received(&mut client, "the home state", |p| matches!(p, ServerPacket::Sites(_)));
    let sent: Vec<String> = w.b.to(CLIENT).into_iter().map(name).collect();
    let at = sent.iter().position(|n| n == "PlayerRegion").expect("PlayerRegion sent");
    assert_eq!(sent[at..at + 4], ["PlayerRegion", "Fortress", "Calendar", "Sites"], "{sent:?}");
    assert!(!sent.iter().any(|n| n == "WorldMap" || n == "Civs"), "no world map or civs: {sent:?}");
    assert_eq!(w.a.manager.session_kind(CLIENT), Some(game::SessionKind::Observer), "A's session is the observer now");
    assert_eq!(w.a.manager.home_of(CLIENT), None, "no body left on A");
    let received = names(&client.seen);
    let at = received.iter().position(|n| n == "PlayerRegion").expect("PlayerRegion received");
    assert_eq!(received[at..at + 4], ["PlayerRegion", "Fortress", "Calendar", "Sites"], "{received:?}");
}

/// The body arrives (B's observer session is promoted to Home); then the same ticket is
/// presented again: B's edge refuses it (the order was revoked on promotion), the Home session
/// is not disconnected, nothing parks, and the newcomer hears nothing of the player.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn ticket_cannot_replace_a_promoted_home_session() {
    let mut w = World::new();
    w.connect_home_on_a();
    let ticket = w.ticket_from_a();
    let mut client = w.observe(&ticket);
    w.until_received(&mut client, "B's Host packet", |p| matches!(p, ServerPacket::Host(_)));
    let b = w.seam.1;
    let now = w.now();
    w.a.event(ServerEvent::ClientPacket(ClientPacket::PlayRegion(b), CLIENT), now);
    w.until("the session on B is the home", |w| w.b.manager.session_kind(CLIENT) == Some(game::SessionKind::Home));
    w.rounds(5);
    let disconnects = |w: &World| w.edge.log.iter().filter(|e| matches!(e, ServerEvent::ClientDisconnected(_))).count();
    let before = disconnects(&w);
    let mut thief = w.dial(Credential::Observer { client: ticket.client, nonce: ticket.nonce });
    w.until_received(&mut thief, "a refusal", |p| matches!(p, ServerPacket::Refused(_)));
    w.until_closed(&thief, "the edge closes the newcomer");
    w.rounds(20);
    thief.drain();
    assert!(matches!(&thief.seen[..], [ServerPacket::Refused(_)]), "the newcomer got only its refusal: {:?}", names(&thief.seen));
    assert_eq!(disconnects(&w), before, "the Home session was not disconnected");
    assert_eq!(w.b.manager.session_kind(CLIENT), Some(game::SessionKind::Home));
    assert_eq!(w.b.manager.home_of(CLIENT), Some(b), "nothing parked");
    assert!(client.connection.close_reason().is_none(), "the home connection is open");
}

/// Once promoted, the session's own input is accepted (it is no observer any more).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn promoted_session_input_is_accepted() {
    let mut w = World::new();
    w.connect_home_on_a();
    let ticket = w.ticket_from_a();
    let mut client = w.observe(&ticket);
    w.until_received(&mut client, "B's Host packet", |p| matches!(p, ServerPacket::Host(_)));
    let b = w.seam.1;
    let now = w.now();
    w.a.event(ServerEvent::ClientPacket(ClientPacket::PlayRegion(b), CLIENT), now);
    let body_on_b = |w: &mut World| {
        let mut mode = None;
        w.b.manager.spawner_mut().with_region(b, |r| mode = r.data().clients.get(&CLIENT).map(|c| c.mode));
        mode
    };
    w.until("the body stands in b", |w| w.b.manager.session_kind(CLIENT) == Some(game::SessionKind::Home) && body_on_b(w).is_some());
    let order = game::GameEventKind::Order(CLIENT, game::Order::SetMode { mode: game::PlayMode::Adventure, at: None });
    let tick = game::RegionSpawner::world_tick(w.b.manager.spawner_mut());
    client.send(&w.rt, ClientPacket::GameEventAfterTick(game::GameEvent { kind: order, id: 0, region_id: b }, tick));
    w.until("the order applied on B", |w| body_on_b(w) == Some(game::PlayMode::Adventure));
}

/// A test QUIC client that accepts any certificate (the server's is self-signed).
mod quic {
    use std::sync::Arc;

    use quinn::crypto::rustls::QuicClientConfig;
    use quinn::rustls::{
        self,
        client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        pki_types::{CertificateDer, ServerName, UnixTime},
        DigitallySignedStruct, SignatureScheme,
    };

    #[derive(Debug)]
    struct AnyCert(Arc<rustls::crypto::CryptoProvider>);

    impl ServerCertVerifier for AnyCert {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
        }
        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    pub async fn connect(port: u16) -> quinn::Connection {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let crypto = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCert(provider)))
            .with_no_client_auth();
        let config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto).unwrap()));
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(config);
        endpoint
            .connect(format!("127.0.0.1:{port}").parse().unwrap(), "localhost")
            .unwrap()
            .await
            .expect("connect")
    }
}
