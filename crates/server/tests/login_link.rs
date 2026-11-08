//! The login service's push to a world host (`login_link`): a JWT `Hello` is admitted only
//! against a pending `SpawnPlayer`, once; a newer spawn order parks the player's online
//! session before the newer ticket exists (one body, one record); `Kick` parks and revokes.
//! A real service link on localhost, the real connect edge (`resolve_session`) and, where a
//! session must exist, a real `WorldManager` over region threads.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use auth::{Signer, SigningKey};
use crossbeam::channel::{unbounded, Receiver, Sender};
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, Credential, InlineLodBuilder, LodBuilder,
    PlayerToken, RegionCoords, RegionOutput, ServerEvent, ServerPacket, SubChunk, WorldManager, LOGGED_IN_ELSEWHERE,
    SPAWN_REGION,
};
use login_api::{HostToLogin, JoinClaims, LoginToHost, HOST_AUDIENCE, JOIN_JWT_TTL_SECS};
use server::auth_edge::{Authenticator, Clock, JwksCache, REFUSE_SUPERSEDED};
use server::login_link::{self, LoginLink, SpawnOrders, REFUSE_NO_SPAWN_ORDER};
use server::player_store::{self, HelloSession, SharedPlayerStore};
use server::region_threads::ThreadRegionSpawner;
use svc_link::{LinkDialer, LinkEvent, LinkHello, PeerRole};

const NOW: u64 = 1_800_000_000;
const WORLD: &str = "alpha";
const SERVICE_TOKEN: &str = "test-service-token";
/// How long a test waits for something to cross the real link.
const LINK_WAIT: Duration = Duration::from_secs(5);
const REGION_STARTUP_BUDGET: Duration = Duration::from_secs(10);
const KICK_REASON: &str = "signed out everywhere";

/// A host's admission state with its login link listening on localhost.
struct Host {
    link: Option<LoginLink>,
    orders: Arc<SpawnOrders>,
    auth: Authenticator,
    players: SharedPlayerStore,
    /// What the link sends the manager (supersedes).
    events: Receiver<ServerEvent>,
    signer: Signer,
    runtime: tokio::runtime::Runtime,
}

impl Host {
    fn start() -> Host {
        let clock: Clock = Arc::new(|| NOW);
        let orders = Arc::new(SpawnOrders::new(clock.clone()));
        let signer = Signer::new(vec![SigningKey::generate().0]).unwrap_or_else(|_| unreachable!());
        let auth = Authenticator::jwt(WORLD, JwksCache::fixed(signer.jwks()), clock).with_spawn_orders(orders.clone());
        let players = player_store::shared(player_store::MemoryPlayerStore::default());
        let (events_tx, events) = unbounded();
        let link = login_link::start(
            "127.0.0.1:0".parse().unwrap(),
            SERVICE_TOKEN.into(),
            None,
            orders.clone(),
            players.clone(),
            events_tx,
        )
        .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        Host { link: Some(link), orders, auth, players, events, signer, runtime }
    }

    fn ticket(&self, player: &PlayerToken, epoch: u64) -> String {
        self.signer.sign(&JoinClaims {
            sub: "42".into(),
            world: WORLD.into(),
            player_token: player.0.clone(),
            session_epoch: epoch,
            aud: HOST_AUDIENCE.into(),
            iat: NOW,
            exp: NOW + JOIN_JWT_TTL_SECS,
        })
    }

    /// The connect edge's handshake for a ticket of `player` at `epoch`.
    fn hello(&self, player: &PlayerToken, epoch: u64) -> Result<HelloSession, String> {
        let hello = ClientPacket::Hello { credential: Credential::Jwt(self.ticket(player, epoch)) };
        self.runtime.block_on(player_store::resolve_session(&self.players, &self.auth, hello))
    }

    /// Everything the link has sent the manager so far.
    fn drain_events(&self) -> Vec<ServerEvent> {
        self.events.try_iter().collect()
    }

    fn login(&self, role: PeerRole) -> Login {
        let link = self.link.as_ref().unwrap();
        let hello = LinkHello::new(SERVICE_TOKEN.into(), role);
        let (tx, events) = LinkDialer::connect(link.local_addr(), link.cert_sha256().to_string(), hello);
        match events.recv_timeout(LINK_WAIT) {
            Ok(LinkEvent::Up(_)) => {}
            other => panic!("the login link did not come up: {other:?}"),
        }
        Login { tx, events }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        drop(self.link.take()); // sync code: the listener drops its runtime
    }
}

/// The login service's end of the link.
struct Login {
    tx: Sender<LoginToHost>,
    events: Receiver<LinkEvent<HostToLogin>>,
}

impl Login {
    /// The next message from the host within `wait`.
    fn next(&self, wait: Duration) -> Option<HostToLogin> {
        let deadline = Instant::now() + wait;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(LinkEvent::Msg(msg)) => return Some(msg),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
        None
    }

    /// Push `SpawnPlayer` and wait for its ack; whether it was ok.
    fn spawn(&self, player: &PlayerToken, epoch: u64) -> bool {
        self.tx.send(LoginToHost::SpawnPlayer { player_token: player.0.clone(), session_epoch: epoch }).unwrap();
        match self.next(LINK_WAIT) {
            Some(HostToLogin::SpawnAck { player_token, session_epoch, ok }) => {
                assert_eq!((player_token.as_str(), session_epoch), (player.0.as_str(), epoch));
                ok
            }
            other => panic!("no ack for epoch {epoch}: {other:?}"),
        }
    }

    fn kick(&self, player: &PlayerToken, reason: &str) {
        self.tx.send(LoginToHost::Kick { player_token: player.0.clone(), reason: reason.into() }).unwrap();
    }
}

fn token(n: u8) -> PlayerToken {
    PlayerToken::generate([n; 16])
}

/// Wait until the link has sent the manager something (a supersede), at most `LINK_WAIT`.
fn next_event(host: &Host) -> ServerEvent {
    host.events.recv_timeout(LINK_WAIT).expect("an event from the login link")
}

#[test]
fn jwt_without_spawn_order_refused() {
    let host = Host::start();
    let _login = host.login(PeerRole::Login);
    let alice = token(1);
    assert_eq!(host.hello(&alice, 1).unwrap_err(), REFUSE_NO_SPAWN_ORDER);
    // An order for another epoch does not admit this one.
    assert!(_login.spawn(&alice, 2));
    assert_eq!(host.hello(&alice, 3).unwrap_err(), REFUSE_NO_SPAWN_ORDER);
    assert_eq!(host.hello(&alice, 1).unwrap_err(), REFUSE_SUPERSEDED);
}

#[test]
fn spawn_then_connect_accepted_once() {
    let host = Host::start();
    let login = host.login(PeerRole::Login);
    let alice = token(1);
    assert!(login.spawn(&alice, 1));
    let session = host.hello(&alice, 1).expect("the ordered ticket connects");
    let ticket = session.ticket.clone().expect("a spawn-order session");
    assert_eq!((ticket.player_token.as_str(), ticket.session_epoch), (alice.0.as_str(), 1));
    // The order is consumed: the same ticket again is refused.
    assert_eq!(host.hello(&alice, 1).unwrap_err(), REFUSE_NO_SPAWN_ORDER);
    // The order asked the manager to park any session of the player (it had none).
    match next_event(&host) {
        ServerEvent::Supersede { player, reason } => {
            assert_eq!((player, reason.as_str()), (session.player, LOGGED_IN_ELSEWHERE));
        }
        other => panic!("{other:?}"),
    }
    // A resend of the same order (at-least-once link) is acked and parks nobody.
    assert!(login.spawn(&alice, 1));
    assert!(host.drain_events().is_empty(), "a repeated order supersedes nothing");
    assert_eq!(host.hello(&alice, 1).unwrap_err(), REFUSE_NO_SPAWN_ORDER, "nor re-opens the ticket");
    // The session ends: the login service hears of it.
    host.auth.session_ended(&ticket);
    assert_eq!(
        login.next(LINK_WAIT),
        Some(HostToLogin::SessionEnded { player_token: alice.0.clone(), session_epoch: 1 })
    );
}

#[test]
fn only_the_login_service_may_order() {
    let host = Host::start();
    let impostor = host.login(PeerRole::Coordinator);
    let alice = token(1);
    impostor.tx.send(LoginToHost::SpawnPlayer { player_token: alice.0.clone(), session_epoch: 1 }).unwrap();
    // A real login peer's order after it is answered; the impostor's never was.
    let login = host.login(PeerRole::Login);
    assert!(login.spawn(&token(2), 1));
    assert_eq!(impostor.next(Duration::from_millis(50)), None);
    assert!(!host.orders.pending().by_token.contains_key(&alice.0));
    assert_eq!(host.hello(&alice, 1).unwrap_err(), REFUSE_NO_SPAWN_ORDER);
}

// ---- With a world: real region threads -----------------------------------------------------

fn floor_columns(_rc: RegionCoords) -> Vec<(ColumnCoords, Column)> {
    // The chunk under the spawn column.
    let edge = game::CHUNK_UNITS as f32;
    let span = |a: f32, b: f32| (a / edge).floor() as usize..(b / edge).ceil() as usize;
    span(128.0, 160.0)
        .flat_map(|x| {
            span(128.0, 160.0).map(move |z| {
                (
                    ColumnCoords::new(x, z),
                    Column::new(
                        0,
                        Block::new(BlockId::AIR),
                        Block::new(BlockId::AIR),
                        vec![SubChunk::Dense(Box::new(Chunk::flat_floor(8)))],
                    ),
                )
            })
        })
        .collect()
}

/// A manager over real region threads, fed by the host's link events.
struct World {
    manager: WorldManager<ThreadRegionSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    start: Instant,
}

impl World {
    fn new() -> World {
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
            coarse: Box::new(|_| Vec::new()),
            water: Box::new(game::lod::LodWater::dry),
            out: lod_result_send,
        });
        let manager = WorldManager::new(
            ThreadRegionSpawner::default(),
            Box::new(floor_columns),
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        );
        World { manager, region_out, packets, start: Instant::now() }
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn event(&mut self, ev: ServerEvent) {
        let now = self.now_ms();
        assert!(self.manager.handle_server_event(ev, now));
    }

    /// A session of `player` as the edge announces it, asking for its region. In the server
    /// the link's supersedes and the edge's connects share one channel, and an order's
    /// supersede is queued before its ack (so before its ticket can connect): what the link
    /// sent so far is handled first.
    fn connect(&mut self, host: &Host, client: ClientId, session: &HelloSession) {
        for ev in host.drain_events() {
            self.event(ev);
        }
        self.event(ServerEvent::ClientConnected { client, player: session.player, record: session.record.clone() });
        self.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), client));
    }

    /// Feed the link's events and the region threads' outputs to the manager until `done`
    /// says so of a packet (or of nothing: `None`), or the budget runs out. Every packet
    /// lands in `log`.
    fn pump_until(
        &mut self,
        host: &Host,
        log: &mut Vec<(Option<ClientId>, ServerPacket)>,
        done: impl FnMut(&WorldManager<ThreadRegionSpawner>, Option<&(Option<ClientId>, ServerPacket)>) -> bool,
    ) -> bool {
        self.pump_for(host, REGION_STARTUP_BUDGET, log, done)
    }

    /// [`pump_until`](Self::pump_until) within `budget`.
    fn pump_for(
        &mut self,
        host: &Host,
        budget: Duration,
        log: &mut Vec<(Option<ClientId>, ServerPacket)>,
        mut done: impl FnMut(&WorldManager<ThreadRegionSpawner>, Option<&(Option<ClientId>, ServerPacket)>) -> bool,
    ) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            for ev in host.drain_events() {
                self.event(ev);
            }
            if let Ok((rc, output)) = self.region_out.recv_timeout(Duration::from_millis(20)) {
                let now = self.now_ms();
                self.manager.handle_region_output(rc, output, now);
            }
            for packet in self.packets.try_iter() {
                let hit = done(&self.manager, Some(&packet));
                log.push(packet);
                if hit {
                    return true;
                }
            }
            if done(&self.manager, None) {
                return true;
            }
        }
        false
    }

    /// The bodies and sessions in the next snapshot of the spawn region sent to `client`.
    fn snapshot_for(&mut self, host: &Host, client: ClientId) -> (usize, BTreeSet<ClientId>) {
        let mut log = Vec::new();
        let mut seen = None;
        let found = self.pump_until(host, &mut log, |_, p| match p {
            Some((Some(to), ServerPacket::Region(rc, rb))) if *to == client && *rc == SPAWN_REGION => {
                seen = Some((rb.player_entites.len(), rb.clients.keys().copied().collect()));
                true
            }
            _ => false,
        });
        assert!(found, "no snapshot of the spawn region for client {client}");
        seen.unwrap()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.manager.shutdown_all();
    }
}

fn refusals(log: &[(Option<ClientId>, ServerPacket)], client: ClientId) -> Vec<String> {
    log.iter()
        .filter_map(|(to, p)| match p {
            ServerPacket::Refused(why) if *to == Some(client) => Some(why.clone()),
            _ => None,
        })
        .collect()
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn second_spawn_parks_first_session_before_new_hello() {
    let host = Host::start();
    let login = host.login(PeerRole::Login);
    let mut world = World::new();
    let alice = token(1);

    // Client A joins with epoch 1 and stands in the spawn region.
    assert!(login.spawn(&alice, 1));
    let a = host.hello(&alice, 1).unwrap();
    world.connect(&host, 0, &a);
    assert_eq!(world.snapshot_for(&host, 0), (1, BTreeSet::from([0])));
    assert!(world.manager.parked_record(a.player).is_none());

    // A second login: the order (epoch 2) parks A before any new Hello reaches the host.
    assert!(login.spawn(&alice, 2));
    let mut log = Vec::new();
    let parked = world.pump_until(&host, &mut log, |m, _| m.parked_record(a.player).is_some());
    assert!(parked, "A's record was parked");
    assert_eq!(refusals(&log, 0), vec![LOGGED_IN_ELSEWHERE.to_string()], "A is told once");
    assert_eq!(world.manager.player_of(0), None);
    assert_eq!(world.manager.depths().sessions, 0);
    // A's connection ends: the login service hears of epoch 1's end.
    host.auth.session_ended(a.ticket.as_ref().unwrap());
    assert_eq!(
        login.next(LINK_WAIT),
        Some(HostToLogin::SessionEnded { player_token: alice.0.clone(), session_epoch: 1 })
    );

    // The epoch-1 ticket connecting late is refused; epoch 2's connects as the same player.
    assert_eq!(host.hello(&alice, 1).unwrap_err(), REFUSE_SUPERSEDED);
    let b = host.hello(&alice, 2).unwrap();
    assert_eq!(b.player, a.player);
    world.connect(&host, 1, &b);
    // One body: the parked one, restored for B; A's is gone.
    assert_eq!(world.snapshot_for(&host, 1), (1, BTreeSet::from([1])));
    assert!(world.manager.parked_record(a.player).is_none(), "the record was taken by B's restore");
    // A resend of epoch 2's order does not park B.
    assert!(login.spawn(&alice, 2));
    let mut log = Vec::new();
    world.pump_for(&host, Duration::from_millis(300), &mut log, |_, _| false);
    assert!(refusals(&log, 1).is_empty(), "{log:?}");
    assert_eq!(world.manager.player_of(1), Some(a.player));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn kick_parks_and_refuses() {
    let host = Host::start();
    let login = host.login(PeerRole::Login);
    let mut world = World::new();
    let alice = token(1);
    assert!(login.spawn(&alice, 1));
    let a = host.hello(&alice, 1).unwrap();
    world.connect(&host, 0, &a);
    assert_eq!(world.snapshot_for(&host, 0), (1, BTreeSet::from([0])));

    // An order for a second login is pending when the account is signed out everywhere.
    assert!(login.spawn(&alice, 2));
    login.kick(&alice, KICK_REASON);
    let mut log = Vec::new();
    let parked = world.pump_until(&host, &mut log, |m, _| m.parked_record(a.player).is_some());
    assert!(parked, "A's record was parked");
    // Parked once: the order's takeover ended A, the kick found no session left.
    assert_eq!(refusals(&log, 0), vec![LOGGED_IN_ELSEWHERE.to_string()]);
    assert_eq!(world.manager.depths().sessions, 0);
    // Both tickets are revoked, and a late resend of the revoked order does not re-open it.
    assert_eq!(host.hello(&alice, 2).unwrap_err(), REFUSE_NO_SPAWN_ORDER);
    assert_eq!(host.hello(&alice, 1).unwrap_err(), REFUSE_SUPERSEDED);
    assert!(!login.spawn(&alice, 2));
    assert_eq!(host.hello(&alice, 2).unwrap_err(), REFUSE_NO_SPAWN_ORDER);

    // A new login (epoch 3) is ordered and connects; a kick then refuses it with the reason.
    assert!(login.spawn(&alice, 3));
    let c = host.hello(&alice, 3).unwrap();
    world.connect(&host, 2, &c);
    assert_eq!(world.snapshot_for(&host, 2), (1, BTreeSet::from([2])));
    login.kick(&alice, KICK_REASON);
    let mut log = Vec::new();
    let parked = world.pump_until(&host, &mut log, |m, _| m.parked_record(c.player).is_some());
    assert!(parked, "the kicked session's record was parked");
    assert_eq!(refusals(&log, 2), vec![KICK_REASON.to_string()]);
    assert_eq!(world.manager.depths().sessions, 0);
    assert_eq!(host.hello(&alice, 3).unwrap_err(), REFUSE_NO_SPAWN_ORDER);
}
