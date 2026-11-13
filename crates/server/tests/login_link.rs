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
    ClientId, ClientPacket, Credential, PlayerToken, RegionCoords, RegionOutput, ServerEvent, ServerPacket,
    WorldManager, LOGGED_IN_ELSEWHERE, SPAWN_REGION,
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
    events_tx: Sender<ServerEvent>,
    signer: Signer,
    runtime: tokio::runtime::Runtime,
}

impl Host {
    fn start() -> Host {
        let mut host = Self::unlinked();
        host.listen("127.0.0.1:0".parse().unwrap(), None);
        host
    }

    /// The link to the login service is not listening yet ([`listen`](Self::listen)).
    fn unlinked() -> Host {
        let clock: Clock = Arc::new(|| NOW);
        let orders = Arc::new(SpawnOrders::new(clock.clone()));
        let signer = Signer::new(vec![SigningKey::generate().0]).unwrap_or_else(|_| unreachable!());
        let auth = Authenticator::jwt(WORLD, JwksCache::fixed(signer.jwks()), clock).with_spawn_orders(orders.clone());
        let players = player_store::shared(player_store::MemoryPlayerStore::default());
        let (events_tx, events) = unbounded();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        Host { link: None, orders, auth, players, events, events_tx, signer, runtime }
    }

    /// Start listening for the login service (once).
    fn listen(&mut self, bind: std::net::SocketAddr, pin_file: Option<std::path::PathBuf>) {
        let link = login_link::start(
            bind,
            SERVICE_TOKEN.into(),
            pin_file,
            self.orders.clone(),
            self.players.clone(),
            self.events_tx.clone(),
        )
        .unwrap();
        self.link = Some(link);
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
            Some(HostToLogin::SpawnAck { player_token, session_epoch, ok, .. }) => {
                assert_eq!((player_token.as_str(), session_epoch), (player.0.as_str(), epoch));
                ok
            }
            other => panic!("no ack for epoch {epoch}: {other:?}"),
        }
    }

    /// Push `Kick` up to `epoch` and wait for its ack (sent once it was applied).
    fn kick(&self, player: &PlayerToken, epoch: u64, reason: &str) {
        let kick = LoginToHost::Kick { player_token: player.0.clone(), session_epoch: epoch, reason: reason.into() };
        self.tx.send(kick).unwrap();
        match self.next(LINK_WAIT) {
            Some(HostToLogin::KickAck { player_token, session_epoch }) => {
                assert_eq!((player_token.as_str(), session_epoch), (player.0.as_str(), epoch));
            }
            other => panic!("no kick ack for epoch {epoch}: {other:?}"),
        }
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
fn jwt_session_gets_session_client_id() {
    use std::sync::atomic::AtomicU64;
    let host = Host::start();
    let login = host.login(PeerRole::Login);
    let alice = token(1);
    assert!(login.spawn(&alice, 7));
    let session = host.hello(&alice, 7).unwrap();
    let local_ids = AtomicU64::new(0);
    let id = session.client_id(&local_ids);
    assert_eq!(id, game::session_client_id(session.player, 7));
    assert_eq!(local_ids.load(std::sync::atomic::Ordering::SeqCst), 0, "a login session spends no local id");
    // The announcement carries it.
    let (edge, announced) = unbounded();
    player_store::announce_session(&edge, id, &session, &host.auth).unwrap();
    match announced.try_recv().unwrap() {
        ServerEvent::ClientConnected { client, player, .. } => {
            assert_eq!((client, player), (game::session_client_id(session.player, 7), session.player));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn client_ids_of_other_epochs_and_local_sessions() {
    use std::sync::atomic::AtomicU64;
    let alice = HelloSession { player: game::PlayerId(3), record: None, ticket: None, session_epoch: Some(1), observer: None };
    let next = HelloSession { session_epoch: Some(2), player: game::PlayerId(3), record: None, ticket: None, observer: None };
    let local_ids = AtomicU64::new(0);
    assert_ne!(alice.client_id(&local_ids), next.client_id(&local_ids));
    // A local session takes the process counter, from 0.
    let local = HelloSession { player: game::PlayerId(3), record: None, ticket: None, session_epoch: None, observer: None };
    assert_eq!((local.client_id(&local_ids), local.client_id(&local_ids)), (0, 1));
}

/// `claim_sink` is generic over the sink: a `ClientSink` needs a live connection, so the
/// tests claim with a marker value in the real `DashMap<ClientId, _>` type.
#[test]
fn claim_sink_refuses_a_live_id_and_keeps_the_first_sink() {
    use dashmap::DashMap;
    let sinks: DashMap<ClientId, &'static str> = DashMap::new();
    assert_eq!(player_store::claim_sink(&sinks, 9, "first"), Ok(()));
    assert_eq!(player_store::claim_sink(&sinks, 9, "second"), Err("second"), "the sink comes back");
    assert_eq!(*sinks.get(&9).unwrap(), "first", "the live sink is untouched");
    assert_eq!(player_store::claim_sink(&sinks, 10, "other"), Ok(()), "another id is free");
    assert_eq!(sinks.len(), 2);
}

#[test]
fn concurrent_claims_of_one_id_have_one_winner() {
    use dashmap::DashMap;
    use std::sync::Barrier;
    for round in 0..200u64 {
        let sinks: Arc<DashMap<ClientId, u32>> = Arc::new(DashMap::new());
        let barrier = Arc::new(Barrier::new(2));
        let threads: Vec<_> = (0..2u32)
            .map(|n| {
                let (sinks, barrier) = (sinks.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    player_store::claim_sink(&sinks, round, n).is_ok()
                })
            })
            .collect();
        let wins = threads.into_iter().map(|t| t.join().unwrap()).filter(|won| *won).count();
        assert_eq!(wins, 1, "round {round}: exactly one claim wins");
    }
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
        let manager = WorldManager::new(
            ThreadRegionSpawner::default().with_genesis(Arc::new(stud_flat_seed)),
            out_send,
            region_out_send,
            Box::new(game::NullLodBuilder::default()),
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
        self.try_connect(host, client, session).expect("announced");
    }

    /// [`connect`](Self::connect), or the edge's refusal (nothing announced).
    fn try_connect(&mut self, host: &Host, client: ClientId, session: &HelloSession) -> Result<(), String> {
        for ev in host.drain_events() {
            self.event(ev);
        }
        // The edge's own announcement (refused if the login was revoked meanwhile).
        let (edge, announced) = unbounded();
        let result = player_store::announce_session(&edge, client, session, &host.auth);
        for ev in announced.try_iter() {
            self.event(ev);
        }
        result?;
        self.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), client));
        Ok(())
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
    login.kick(&alice, 2, KICK_REASON);
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
    // A late copy of the old kick (up to epoch 2) keeps the newer login.
    login.kick(&alice, 2, KICK_REASON);
    let mut log = Vec::new();
    world.pump_for(&host, Duration::from_millis(200), &mut log, |_, _| false);
    assert!(refusals(&log, 2).is_empty(), "{log:?}");
    login.kick(&alice, 3, KICK_REASON);
    let mut log = Vec::new();
    let parked = world.pump_until(&host, &mut log, |m, _| m.parked_record(c.player).is_some());
    assert!(parked, "the kicked session's record was parked");
    // A copy of the kick (the login service resends until acked) changes nothing.
    login.kick(&alice, 3, KICK_REASON);
    world.pump_for(&host, Duration::from_millis(200), &mut log, |_, _| false);
    assert_eq!(refusals(&log, 2), vec![KICK_REASON.to_string()], "refused and parked once");
    assert!(world.manager.parked_record(c.player).is_some());
    assert_eq!(world.manager.depths().sessions, 0);
    assert_eq!(host.hello(&alice, 3).unwrap_err(), REFUSE_NO_SPAWN_ORDER);
}

/// A kick landing between a ticket's admission and its session's announcement: the edge
/// refuses the session at its announcement with the kick's reason — it never plays.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn kick_during_connect_refuses_the_session() {
    let host = Host::start();
    let login = host.login(PeerRole::Login);
    let mut world = World::new();
    let alice = token(1);
    assert!(login.spawn(&alice, 1));
    let a = host.hello(&alice, 1).unwrap(); // admitted, not announced yet
    login.kick(&alice, 1, KICK_REASON);
    assert_eq!(world.try_connect(&host, 0, &a), Err(KICK_REASON.to_string()));
    let mut log = Vec::new();
    world.pump_for(&host, Duration::from_millis(200), &mut log, |_, _| false);
    assert_eq!(world.manager.player_of(0), None, "never announced");
    assert_eq!(world.manager.depths().sessions, 0);
}

/// A player store that fails while `failing` is set.
struct FlakyPlayers {
    inner: player_store::MemoryPlayerStore,
    failing: Arc<std::sync::atomic::AtomicBool>,
}

impl player_store::PlayerStore for FlakyPlayers {
    fn resolve(&mut self, key: &player_store::PlayerKey) -> std::io::Result<(game::PlayerId, Option<game::PlayerRecord>)> {
        if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::other("the player store is down"));
        }
        self.inner.resolve(key)
    }
}

/// C-I1: a kick whose revoked session's player cannot be resolved (the player store fails)
/// is not acked — the login service keeps it and resends — and nothing claims it applied;
/// the resend once the store is back parks the session and is acked.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn kick_is_not_acked_until_its_session_is_parked() {
    let failing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut host = Host::unlinked();
    host.players = player_store::shared(FlakyPlayers { inner: Default::default(), failing: failing.clone() });
    host.listen("127.0.0.1:0".parse().unwrap(), None);
    let login = host.login(PeerRole::Login);
    let mut world = World::new();
    let alice = token(1);
    assert!(login.spawn(&alice, 1));
    let a = host.hello(&alice, 1).unwrap();
    world.connect(&host, 0, &a);
    assert_eq!(world.snapshot_for(&host, 0), (1, BTreeSet::from([0])));

    failing.store(true, std::sync::atomic::Ordering::SeqCst);
    let kick = LoginToHost::Kick { player_token: alice.0.clone(), session_epoch: 1, reason: KICK_REASON.into() };
    login.tx.send(kick.clone()).unwrap();
    assert_eq!(login.next(Duration::from_millis(700)), None, "no ack while the session could not be parked");
    assert!(host.drain_events().is_empty(), "nothing parked it");

    failing.store(false, std::sync::atomic::Ordering::SeqCst);
    login.kick(&alice, 1, KICK_REASON); // the login service's resend
    let mut log = Vec::new();
    let parked = world.pump_until(&host, &mut log, |m, _| m.parked_record(a.player).is_some());
    assert!(parked, "the resend parked the session: {log:?}");
    assert_eq!(refusals(&log, 0), vec![KICK_REASON.to_string()]);
}

/// End to end with the login service's own code (store + `HostLinks`): the account is
/// signed out everywhere while the host's link is down; the kick is durable, goes out when
/// the link comes up, parks the session and revokes the pending ticket, and its ack clears
/// the login service's queue.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn kick_queued_while_the_link_is_down_parks_once_it_is_up() {
    // The host's link address and pin, known before it listens (a host that ran before).
    let dir = std::env::temp_dir().join(format!("login-link-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pin_file = dir.join("host.cert-sha256");
    let (bind, pin) = {
        let (probe, _events, _senders) = svc_link::LinkListener::bind::<LoginToHost, HostToLogin>(
            "127.0.0.1:0".parse().unwrap(),
            SERVICE_TOKEN.into(),
            Some(pin_file.clone()),
        )
        .unwrap();
        (probe.local_addr(), probe.cert_sha256().to_string())
    };

    // Alice plays (epoch 1) and has a second join ordered (epoch 2, not connected yet).
    let mut host = Host::unlinked();
    let mut world = World::new();
    let alice = token(1);
    assert_eq!(host.orders.order(&alice.0, 1), login_link::SpawnOrder::New);
    let a = host.hello(&alice, 1).unwrap();
    world.connect(&host, 0, &a);
    assert_eq!(world.snapshot_for(&host, 0), (1, BTreeSet::from([0])));
    assert_eq!(host.orders.order(&alice.0, 2), login_link::SpawnOrder::New);

    // The login service knows the same: a world with the host's link, two joins.
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let (store, links) = rt.block_on(async {
        let store = login::LoginStore::open("sqlite::memory:").await.unwrap();
        store
            .put_world(&login::WorldRow {
                world_id: WORLD.into(),
                name: "Alpha".into(),
                status: login_api::WorldStatus::Open,
                quic: None,
                webtransport: None,
                cert_hash: None,
                created_at: 1,
                host_link: Some(bind.to_string()),
                host_link_cert: Some(pin),
            })
            .await
            .unwrap();
        let account = store.create_account("alice@example.org", "x", 1).await.unwrap().unwrap();
        let token = alice.0.clone();
        assert_eq!(store.player_token_for(account, WORLD, 1, move || token).await.unwrap(), alice.0);
        store.bump_session(account, WORLD, 1).await.unwrap();
        assert_eq!(store.bump_session(account, WORLD, 2).await.unwrap(), 2);
        let links = Arc::new(login::host_link::HostLinks::new(
            Some(SERVICE_TOKEN.into()),
            store.clone(),
            Arc::new(|| NOW),
        ));
        links.start();
        // Signed out everywhere while the host is not reachable.
        assert_eq!(store.revoke_account_with_kicks(account, NOW as i64, KICK_REASON).await.unwrap(), 1);
        links.wake_kicks();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(store.pending_kicks().await.unwrap().len(), 1, "the kick waits, durably");
        (store, links)
    });
    let mut log = Vec::new();
    world.pump_for(&host, Duration::from_millis(100), &mut log, |_, _| false);
    assert!(refusals(&log, 0).is_empty(), "nothing reached the host yet");

    // The host's link comes up: the kick arrives, parks Alice and revokes the epoch-2 ticket.
    host.listen(bind, Some(pin_file));
    let parked = world.pump_until(&host, &mut log, |m, _| m.parked_record(a.player).is_some());
    assert!(parked, "Alice's record was parked: {log:?}");
    assert_eq!(refusals(&log, 0), vec![KICK_REASON.to_string()]);
    assert_eq!(host.hello(&alice, 2).unwrap_err(), REFUSE_NO_SPAWN_ORDER);
    assert_eq!(host.hello(&alice, 1).unwrap_err(), REFUSE_SUPERSEDED);
    // Its ack clears the queue (no more resends).
    let acked = rt.block_on(async {
        let deadline = tokio::time::Instant::now() + LINK_WAIT;
        while !store.pending_kicks().await.unwrap().is_empty() {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        true
    });
    assert!(acked, "the host's ack deleted the pending kick");
    drop(links);
    drop(rt);
    let _ = std::fs::remove_dir_all(&dir);
}
