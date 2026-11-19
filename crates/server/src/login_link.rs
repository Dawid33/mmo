//! The host's side of the login service's push (`svc-link`, role `Login`): `SpawnPlayer` and
//! `Kick` in, `SpawnAck`, `KickAck` and `SessionEnded` out.
//!
//! A `jwt` host that has a service token listens on [`HOST_LINK_BIND_ENV`] and admits a login
//! ticket only against a pending spawn order ([`SpawnOrders`]): the login service pushes
//! `SpawnPlayer { player_token, session_epoch }` on every join and waits for the ack before it
//! hands the ticket out. So:
//! - a ticket the login service never ordered (or whose order expired, after
//!   [`SPAWN_PENDING_TTL_SECS`]) is refused ([`REFUSE_NO_SPAWN_ORDER`]);
//! - an order is consumed by its first connect (a second connect with the same ticket is refused);
//! - an order for a newer epoch parks the player's online session at once
//!   ([`ServerEvent::Supersede`], `Refused(LOGGED_IN_ELSEWHERE)`) — before the newer ticket
//!   exists, so newest-wins holds across login replicas and processes — and an older ticket is
//!   refused ([`REFUSE_SUPERSEDED`](crate::auth_edge::REFUSE_SUPERSEDED));
//! - `Kick { up to epoch }` parks the session with its reason and drops (and fences off) any
//!   order up to that epoch; a newer login is untouched. It is acked (`KickAck`) only once it
//!   is applied — the revoked session's park queued, or no session of it admitted here: the
//!   login service resends it until then (kicks are durable there; a player store failure
//!   leaves it unacked), and a copy applies nothing new. A connect between its admission and
//!   its `ClientConnected` when the kick lands is refused at its announcement
//!   ([`SpawnOrders::announce`]).
//!
//! Newest wins by the lock, not by timing: the edge sends a session's `ClientConnected` while
//! holding the orders' lock ([`SpawnOrders::announce`]) and refuses it there if a kick revoked
//! it or a newer login of the player is known; a newer order's supersede and a kick's
//! supersede are sent under the same lock, the kick's only while the session it revoked is
//! still the player's admitted one. So a supersede never parks a newer session, and an older
//! session never takes over a newer one.
//!
//! The link is at-least-once around reconnects: an order repeated with an epoch already known
//! (ordered, live, or used within [`SPAWN_PENDING_TTL_SECS`]) is acked again and changes
//! nothing (no second park of the session it admitted, no re-armed ticket).
//!
//! `SessionEnded` is best effort: it goes to the login peers connected at that moment and is
//! lost while the link is down (the login service then keeps the session open in its
//! registry; a kick for it later is a no-op here).
//!
//! The order is consumed and the supersede queued on this link's thread BEFORE the ack goes
//! out, and the supersede shares the manager's event channel with the connects: the new
//! session's `ClientConnected` always comes after the old one's park was asked for.
//!
//! **Split roles** (Elastic 1 Task 6): a role-`host` process never listens for the login
//! service. The login service links to the COORDINATOR (`coordinator_rt`'s login listener,
//! [`LOGIN_LINK_BIND_ENV`]), which converts each order ([`order_from`]) and relays it to one
//! live host, the gate host (`game::CoordMsg::Login`). There the main loop plays the server's
//! half ([`LoginRelay::pump`]): it resolves the order ([`OrderResolver`]: the player's store
//! read never waited on — the main loop beats the lease — and at most
//! [`LOGIN_ORDERS_IN_FLIGHT_MAX`] waiting) into a
//! [`ServerEvent::SpawnOrder`] the manager places — here, or on the host that holds the player,
//! over the peer stream (`game`'s `placement.rs`) — or a `Kick`, applied here first and then
//! relayed to every host. Every host records the spawn orders its manager takes (as above: the
//! supersede first) and applies the kicks relayed to it, and sends what its manager answered up
//! its own coordinator link (`game::HostMsg::Login`): `SpawnAck` naming the host that took the
//! order (from the gate host, the order's origin), `KickAck` once every host applied a kick
//! (from the gate host), `SessionEnded` only when a session left the world, never on a
//! hand-off to another host (from the host it ended on). The coordinator converts each answer
//! ([`answer_from`]) and forwards it unchanged.

use game::HostId;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crossbeam::channel::{Receiver, Sender};
use game::{PlayerToken, ServerEvent, LOGGED_IN_ELSEWHERE};
use login_api::{HostToLogin, LoginToHost, SPAWN_PENDING_TTL_SECS};
use svc_link::{LinkEvent, LinkListener, PeerId, PeerRole, PeerSenders};

use crate::auth_edge::{Clock, REFUSE_SUPERSEDED};
use crate::player_store::{PlayerKey, Resolving, SharedPlayerStore};

/// Where a role-`all` server listens for the login service (`ip:port`).
pub const HOST_LINK_BIND_ENV: &str = "VOXEL_HOST_LINK_BIND";
pub const DEFAULT_HOST_LINK_BIND: &str = "127.0.0.1:6490";
/// Where the host writes its link certificate's SHA-256 pin (the login registry's
/// `host_link_cert`); its cert and key persist beside it (keep it under a git-ignored dir).
pub const HOST_LINK_CERT_HASH_FILE_ENV: &str = "VOXEL_HOST_LINK_CERT_HASH_FILE";
pub const DEFAULT_HOST_LINK_CERT_HASH_FILE: &str = "target/dev-r2/host-0.cert-sha256";
/// Where the coordinator (role `coordinator`) listens for the login service (`ip:port`).
pub const LOGIN_LINK_BIND_ENV: &str = "VOXEL_LOGIN_LINK_BIND";
pub const DEFAULT_LOGIN_LINK_BIND: &str = "127.0.0.1:6490";
/// Where the coordinator writes its login listener's SHA-256 pin (the login registry's
/// `host_link_cert`); its cert and key persist beside it.
pub const LOGIN_LINK_CERT_HASH_FILE_ENV: &str = "VOXEL_LOGIN_LINK_CERT_HASH_FILE";
pub const DEFAULT_LOGIN_LINK_CERT_HASH_FILE: &str = "target/dev-r2/login-link.cert-sha256";

/// The refusal of a ticket no pending spawn order matches.
pub const REFUSE_NO_SPAWN_ORDER: &str = "no spawn order for this login — log in again";

/// The service token, if one is configured (`VOXEL_SERVICE_TOKEN` or
/// `VOXEL_SERVICE_TOKEN_FILE` set): a `jwt` host with one listens for the login service's
/// push; without one it keeps R2a's admission (no link). A configured token that cannot be
/// read is an error.
pub fn service_token_if_configured(lookup: impl Fn(&str) -> Option<String>) -> Result<Option<String>, String> {
    let set = |name: &str| lookup(name).is_some_and(|v| !v.trim().is_empty());
    if !set(svc_link::SERVICE_TOKEN_ENV) && !set(svc_link::SERVICE_TOKEN_FILE_ENV) {
        return Ok(None);
    }
    svc_link::service_token_from_env().map(Some).map_err(|e| format!("the login link's service token: {e}"))
}

/// Names the dev-only admission fallback of a `jwt` host without a service token.
pub const JWT_ADMISSION_ENV: &str = "VOXEL_JWT_ADMISSION";
/// The one accepted value of [`JWT_ADMISSION_ENV`].
pub const JWT_ADMISSION_NEWEST: &str = "newest";

/// How a `jwt` host admits tickets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JwtAdmission {
    /// Against the spawn orders the login service pushes over the login link.
    SpawnOrders,
    /// By newest epoch, no orders and no kicks (dev only).
    Newest,
}

/// Fail closed: a `jwt` host runs without the login link only when told to. `env` is the
/// [`JWT_ADMISSION_ENV`] value (the caller reads it); whitespace-only counts as unset.
pub fn jwt_admission(token_configured: bool, env: Option<&str>) -> Result<JwtAdmission, String> {
    let env = env.map(str::trim).filter(|v| !v.is_empty());
    match (token_configured, env) {
        (true, None) => Ok(JwtAdmission::SpawnOrders),
        (true, Some(other)) if other != JWT_ADMISSION_NEWEST => Err(format!(
            "auth: {JWT_ADMISSION_ENV}={other:?} is set together with a service token, so spawn orders are used: \
             unset {JWT_ADMISSION_ENV}"
        )),
        (true, Some(_)) => Err(format!(
            "auth: {JWT_ADMISSION_ENV} is set together with a service token ({} / {}): the login link is on, \
             so there is nothing to fall back from — unset one of them",
            svc_link::SERVICE_TOKEN_ENV,
            svc_link::SERVICE_TOKEN_FILE_ENV
        )),
        (false, Some(JWT_ADMISSION_NEWEST)) => Ok(JwtAdmission::Newest),
        (false, Some(other)) => Err(format!("auth: {JWT_ADMISSION_ENV}={other:?} is not understood; the accepted value is {JWT_ADMISSION_NEWEST:?}")),
        (false, None) => Err(format!(
            "auth: jwt needs the login link — set VOXEL_SERVICE_TOKEN[_FILE] (a world registered with a host link), \
             or {JWT_ADMISSION_ENV}={JWT_ADMISSION_NEWEST} to admit tickets by newest epoch without spawn orders or kicks (dev only)"
        )),
    }
}

/// The listen address from an env value; unset or empty is [`DEFAULT_HOST_LINK_BIND`].
pub fn bind_from(value: Option<&str>) -> Result<SocketAddr, String> {
    addr_from(HOST_LINK_BIND_ENV, value, DEFAULT_HOST_LINK_BIND)
}

/// The pin file from an env value; unset or empty is [`DEFAULT_HOST_LINK_CERT_HASH_FILE`].
pub fn cert_hash_file_from(value: Option<&str>) -> PathBuf {
    PathBuf::from(value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(DEFAULT_HOST_LINK_CERT_HASH_FILE))
}

/// The coordinator's login listen address from an env value; unset or empty is
/// [`DEFAULT_LOGIN_LINK_BIND`].
pub fn login_bind_from(value: Option<&str>) -> Result<SocketAddr, String> {
    addr_from(LOGIN_LINK_BIND_ENV, value, DEFAULT_LOGIN_LINK_BIND)
}

/// The coordinator's login pin file from an env value; unset or empty is
/// [`DEFAULT_LOGIN_LINK_CERT_HASH_FILE`].
pub fn login_cert_hash_file_from(value: Option<&str>) -> PathBuf {
    PathBuf::from(value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(DEFAULT_LOGIN_LINK_CERT_HASH_FILE))
}

fn addr_from(env: &str, value: Option<&str>, default: &str) -> Result<SocketAddr, String> {
    let value = value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(default);
    value.parse().map_err(|_| format!("{env}={value:?} is not an address (ip:port)"))
}

/// The world id this process's service links name (`VOXEL_WORLD_ID`, trimmed; unset: empty —
/// a dev world with no id, whose links admit only peers with no id either).
pub fn link_world_from(value: Option<&str>) -> String {
    value.map(str::trim).unwrap_or_default().to_string()
}

/// The login service's message, as the coordinator relays it to the gate host.
pub fn order_from(msg: LoginToHost) -> game::LoginOrder {
    match msg {
        LoginToHost::SpawnPlayer { player_token, session_epoch } => game::LoginOrder::Spawn { player_token, session_epoch },
        LoginToHost::Kick { player_token, session_epoch, reason } => game::LoginOrder::Kick { player_token, session_epoch, reason },
    }
}

/// A coordinator replica's answer to `msg` when no host takes logins
/// ([`game::Coordinator::gate_host`] is `None`; the gate is world-wide, so no other replica has
/// one either): the join answers 503, a kick is resent.
pub fn no_gate(msg: &LoginToHost) -> HostToLogin {
    let (LoginToHost::SpawnPlayer { player_token, session_epoch } | LoginToHost::Kick { player_token, session_epoch, .. }) = msg;
    HostToLogin::NoGate { player_token: player_token.clone(), session_epoch: *session_epoch }
}

/// A coordinator replica's answer to a spawn order when no host takes logins and the world adds
/// its own hosts ([`game::coordinator::Gate::Nobody`], fleet dials
/// set; Elastic 4): the world is starting — the login service answers the join 503
/// `world_starting` at once; the replica asks for a host ([`game::coordinator::Input::StartWanted`]).
pub fn starting(msg: &LoginToHost) -> HostToLogin {
    let (LoginToHost::SpawnPlayer { player_token, session_epoch } | LoginToHost::Kick { player_token, session_epoch, .. }) = msg;
    HostToLogin::Starting { player_token: player_token.clone(), session_epoch: *session_epoch }
}

/// A host's answer, as the coordinator forwards it to the login service.
pub fn answer_from(out: game::LoginOut) -> HostToLogin {
    match out {
        game::LoginOut::SpawnAck { player_token, session_epoch, ok, endpoint } => spawn_ack(player_token, session_epoch, ok, endpoint),
        game::LoginOut::KickAck { player_token, session_epoch } => HostToLogin::KickAck { player_token, session_epoch },
        game::LoginOut::SessionEnded { player_token, session_epoch } => HostToLogin::SessionEnded { player_token, session_epoch },
    }
}

/// At most this many of the coordinator's login orders wait at the gate host for their
/// player's store read ([`OrderResolver`]); one beyond is dropped and counted (unanswered: the
/// login service answers 503 at its timeout and the client retries; a kick is resent). A join
/// burst — every player of the world rejoining after a restart, all through this one host —
/// cannot queue without limit.
pub const LOGIN_ORDERS_IN_FLIGHT_MAX: usize = 256;

/// What one of the coordinator's orders became at the gate host ([`OrderResolver::poll`]).
#[derive(Debug)]
pub enum OrderOutcome {
    /// For the manager, on its event channel: a spawn to place ([`ServerEvent::SpawnOrder`]),
    /// or a kick applied here and to be relayed ([`ServerEvent::Kick`]; a revoked session's
    /// supersede went on the channel first).
    Event(ServerEvent),
    /// A spawn whose player could not be resolved: refused
    /// ([`game::WorldManager::login_order_refused`]).
    Refused { player_token: String, session_epoch: u64 },
    /// A kick not applied (a player store failure): unanswered, the login service resends it.
    Unapplied,
}

/// One order in [`OrderResolver`]'s queue.
enum Waiting {
    /// A spawn whose player is being read (`None`: a malformed token, refused).
    Spawn { player_token: String, session_epoch: u64, read: Option<Resolving> },
    /// A kick: applied to the admission state when it reaches the front of the queue; a
    /// session it revoked then reads its player before its supersede goes out.
    Kick { player_token: String, session_epoch: u64, reason: String, step: KickStep },
}

/// Where a kick at the front of [`OrderResolver`]'s queue stands.
enum KickStep {
    /// Not applied yet.
    Due,
    /// It revoked a session whose player is being read.
    Reading(Resolving),
    /// It revoked a session whose token is malformed: not applied.
    Unreadable,
}

/// The gate host's resolution of the orders the coordinator relays (A-I1 of Elastic 1's final
/// review): each spawn's player is read from the player store WITHOUT waiting
/// ([`crate::player_store::PlayerStore::resolve_later`]) — the main loop beats the host's
/// lease and must never sit on a store read — and the orders come out in the order they came,
/// each once its read answered (one still reading holds back those behind it, so a player's
/// orders keep their order; a kick's supersede goes out before its `Kick`). At most
/// [`LOGIN_ORDERS_IN_FLIGHT_MAX`] wait.
pub struct OrderResolver {
    queue: std::collections::VecDeque<Waiting>,
    /// Orders dropped because [`LOGIN_ORDERS_IN_FLIGHT_MAX`] were waiting, since start.
    dropped: u64,
}

impl Default for OrderResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderResolver {
    pub fn new() -> OrderResolver {
        OrderResolver { queue: std::collections::VecDeque::new(), dropped: 0 }
    }

    /// Orders waiting for their player's read (or behind one that is).
    pub fn waiting(&self) -> usize {
        self.queue.len()
    }

    /// Orders dropped at the bound, since start.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// `order` joins the queue — a spawn's player read asked of `players` at once — or, with
    /// [`LOGIN_ORDERS_IN_FLIGHT_MAX`] waiting, is dropped (`false`, counted).
    pub fn push(&mut self, order: game::LoginOrder, players: &SharedPlayerStore) -> bool {
        if self.queue.len() >= LOGIN_ORDERS_IN_FLIGHT_MAX {
            self.dropped += 1;
            return false;
        }
        self.queue.push_back(match order {
            game::LoginOrder::Spawn { player_token, session_epoch } => {
                let read = resolve_later(players, &player_token);
                Waiting::Spawn { player_token, session_epoch, read }
            }
            game::LoginOrder::Kick { player_token, session_epoch, reason } => {
                Waiting::Kick { player_token, session_epoch, reason, step: KickStep::Due }
            }
        });
        true
    }

    /// The orders at the front of the queue whose reads have answered, in order (never
    /// waits): a spawn as its [`ServerEvent::SpawnOrder`] from `me`; a kick applied to
    /// `orders` when it reaches the front — a revoked session's player read without waiting,
    /// its supersede sent on `events` once that read answered — then its [`ServerEvent::Kick`].
    pub fn poll(
        &mut self,
        orders: &SpawnOrders,
        players: &SharedPlayerStore,
        events: &Sender<ServerEvent>,
        me: HostId,
    ) -> Vec<OrderOutcome> {
        let mut out = Vec::new();
        while let Some(front) = self.queue.front_mut() {
            let outcome = match front {
                Waiting::Spawn { player_token, session_epoch, read } => match read.as_mut().map(Resolving::poll) {
                    None => Some(OrderOutcome::Refused { player_token: std::mem::take(player_token), session_epoch: *session_epoch }),
                    Some(None) => None,
                    Some(Some(Ok((player, record)))) => Some(OrderOutcome::Event(ServerEvent::SpawnOrder {
                        origin: me,
                        hops: 0,
                        player_token: std::mem::take(player_token),
                        session_epoch: *session_epoch,
                        player,
                        record_region: record.map(|r| r.region),
                    })),
                    Some(Some(Err(e))) => {
                        log::error!("[login-link] player store: {e}");
                        Some(OrderOutcome::Refused { player_token: std::mem::take(player_token), session_epoch: *session_epoch })
                    }
                },
                Waiting::Kick { player_token, session_epoch, reason, step } => {
                    let up_to = *session_epoch;
                    let kicked = |token: &mut String, reason: &mut String| {
                        let (player_token, reason) = (std::mem::take(token), std::mem::take(reason));
                        OrderOutcome::Event(ServerEvent::Kick { player_token, session_epoch: up_to, reason })
                    };
                    if matches!(step, KickStep::Due) {
                        match orders.kick(player_token, up_to, reason) {
                            KickApplied::NoSession => {}
                            KickApplied::NewerKept => {
                                log::info!("[login-link] kick up to epoch {up_to}: the live session is a newer login; kept")
                            }
                            KickApplied::Revoked => {
                                *step = resolve_later(players, player_token).map_or(KickStep::Unreadable, KickStep::Reading)
                            }
                        }
                    }
                    let unapplied = || {
                        log::warn!("[login-link] kick up to epoch {up_to}: its session's player is unknown now; not acked");
                        Some(OrderOutcome::Unapplied)
                    };
                    match step {
                        // Nothing revoked: applied as it stands.
                        KickStep::Due => Some(kicked(player_token, reason)),
                        KickStep::Unreadable => unapplied(),
                        KickStep::Reading(read) => match read.poll() {
                            None => None,
                            Some(Ok((player, _))) => {
                                park_revoked(orders, events, player_token, up_to, reason, player);
                                Some(kicked(player_token, reason))
                            }
                            Some(Err(e)) => {
                                log::error!("[login-link] player store: {e}");
                                unapplied()
                            }
                        },
                    }
                }
            };
            match outcome {
                Some(o) => {
                    out.push(o);
                    self.queue.pop_front();
                }
                None => break,
            }
        }
        out
    }
}

/// The spawn orders a host holds and has not seen a matching ticket for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingSpawns {
    /// `player_token` → (`session_epoch`, expires at, seconds since the Unix epoch).
    pub by_token: BTreeMap<String, (u64, u64)>,
}

/// How a `SpawnPlayer` relates to what the host already knows of the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnOrder {
    /// A newer epoch: recorded (replacing an older order); the player's session is superseded.
    New,
    /// The epoch already ordered or admitted (a resend): acked again, nothing changes.
    Repeat,
    /// An older epoch than one already ordered or admitted, or one a `Kick` revoked: refused.
    Stale,
}

/// What a `Kick` found ([`SpawnOrders::kick`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KickApplied {
    /// No session of the player is admitted here: nothing to park.
    NoSession,
    /// The admitted session is a newer login's: kept.
    NewerKept,
    /// The admitted session is revoked: park it ([`SpawnOrders::supersede_revoked`]).
    Revoked,
}

/// A `Kick` the host applied: every epoch up to `up_to` is revoked, until `expires`.
struct Kicked {
    up_to: u64,
    expires: u64,
    reason: String,
}

#[derive(Default)]
struct OrderState {
    pending: PendingSpawns,
    /// The epoch of each player's live session (admitted, not ended).
    admitted: BTreeMap<String, u64>,
    /// The newest epoch whose ticket was used, after its session ended, until it expires
    /// (a very late resend of that order must not re-arm the used ticket).
    used: BTreeMap<String, (u64, u64)>,
    /// After a `Kick`: what it revoked, until it expires (a late resend of a revoked order
    /// must not re-open it).
    kicked: BTreeMap<String, Kicked>,
}

impl OrderState {
    /// The newest epoch known for `token` (ordered, admitted, used or kicked).
    fn known(&self, token: &str, now: u64) -> Option<u64> {
        let pending = self.pending.by_token.get(token).map(|(epoch, _)| *epoch);
        let admitted = self.admitted.get(token).copied();
        let used = self.used.get(token).filter(|(_, expires)| now < *expires).map(|(epoch, _)| *epoch);
        let kicked = self.kicked.get(token).filter(|k| now < k.expires).map(|k| k.up_to);
        pending.max(admitted).max(used).max(kicked)
    }

    fn prune(&mut self, now: u64) {
        self.pending.by_token.retain(|_, (_, expires)| now < *expires);
        self.used.retain(|_, (_, expires)| now < *expires);
        self.kicked.retain(|_, k| now < k.expires);
    }
}

/// The login peers `SessionEnded` goes to.
struct LoginPeers {
    senders: PeerSenders<HostToLogin>,
    peers: Mutex<BTreeSet<PeerId>>,
}

/// The admission state of a host the login service pushes to. Shared by the link's thread
/// (orders, kicks) and the connect edge ([`admit`](Self::admit), [`ended`](Self::ended)).
pub struct SpawnOrders {
    state: Mutex<OrderState>,
    now: Clock,
    /// Set once the link listens.
    login: OnceLock<LoginPeers>,
    /// Role `host` ([`Self::relay_through`]): the manager's event channel.
    relay: OnceLock<Sender<ServerEvent>>,
}

impl SpawnOrders {
    pub fn new(now: Clock) -> SpawnOrders {
        SpawnOrders { state: Mutex::default(), now, login: OnceLock::new(), relay: OnceLock::new() }
    }

    /// Role `host` (any number of hosts): login sessions are announced with their login
    /// ([`ServerEvent::LoginConnected`]) and their ends are the manager's to report — a
    /// session refused at the edge goes to it as [`ServerEvent::SessionEnded`]. Set once,
    /// before anything connects; `false` if it was set already.
    pub fn relay_through(&self, events: Sender<ServerEvent>) -> bool {
        self.relay.set(events).is_ok()
    }

    /// Whether [`Self::relay_through`] was set.
    pub fn relays(&self) -> bool {
        self.relay.get().is_some()
    }

    /// `msg` to every login peer connected now (none: lost, best effort).
    fn tell_login(&self, msg: HostToLogin) {
        let Some(login) = self.login.get() else {
            // `HostToLogin`'s `Debug` hides the player token.
            log::warn!("[login-link] {msg:?} for the login service, which is not linked to this host: dropped");
            return;
        };
        let peers = login.peers.lock().unwrap_or_else(|p| p.into_inner()).clone();
        for peer in peers {
            login.senders.send(peer, msg.clone());
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, OrderState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A copy of the pending orders (tests, diagnostics).
    pub fn pending(&self) -> PendingSpawns {
        self.state().pending.clone()
    }

    /// Record `SpawnPlayer { token, epoch }`.
    pub fn order(&self, token: &str, epoch: u64) -> SpawnOrder {
        self.order_then(token, epoch, || {})
    }

    /// [`order`](Self::order), calling `on_new` for a [`SpawnOrder::New`] while the lock is
    /// held: the supersede of the player's session goes out before any session can be
    /// announced after it (an announcement takes the same lock).
    pub fn order_then(&self, token: &str, epoch: u64, on_new: impl FnOnce()) -> SpawnOrder {
        let now = (self.now)();
        let mut state = self.state();
        state.prune(now);
        if state.kicked.get(token).is_some_and(|k| epoch <= k.up_to) {
            return SpawnOrder::Stale;
        }
        match state.known(token, now) {
            Some(known) if epoch < known => SpawnOrder::Stale,
            Some(known) if epoch == known => SpawnOrder::Repeat,
            _ => {
                let expires = now.saturating_add(SPAWN_PENDING_TTL_SECS);
                state.pending.by_token.insert(token.to_string(), (epoch, expires));
                on_new();
                SpawnOrder::New
            }
        }
    }

    /// `Kick { token, up_to, reason }`: a pending order up to `up_to` is dropped, and no order
    /// or ticket up to it is taken for [`SPAWN_PENDING_TTL_SECS`]. What it found of the
    /// player's admitted session: none, a newer login's (kept), or one it revoked (to be parked
    /// with [`supersede_revoked`](Self::supersede_revoked)). Applying the same kick again
    /// changes nothing.
    pub fn kick(&self, token: &str, up_to: u64, reason: &str) -> KickApplied {
        let now = (self.now)();
        let mut state = self.state();
        state.prune(now);
        let ceiling = state.kicked.get(token).map_or(up_to, |k| k.up_to.max(up_to));
        let expires = now.saturating_add(SPAWN_PENDING_TTL_SECS);
        state.kicked.insert(token.to_string(), Kicked { up_to: ceiling, expires, reason: reason.to_string() });
        if state.pending.by_token.get(token).is_some_and(|(epoch, _)| *epoch <= up_to) {
            state.pending.by_token.remove(token);
        }
        match state.admitted.get(token) {
            None => KickApplied::NoSession,
            Some(epoch) if *epoch <= up_to => KickApplied::Revoked,
            Some(_) => KickApplied::NewerKept,
        }
    }

    /// Call `park` (the revoked session's supersede) under the lock if the player's admitted
    /// session is still one a kick up to `up_to` revoked; false if it is gone or a newer
    /// login's by now (a newer order already superseded the old session — parking now would
    /// park the newer one).
    pub fn supersede_revoked(&self, token: &str, up_to: u64, park: impl FnOnce()) -> bool {
        let state = self.state();
        if state.admitted.get(token).is_some_and(|epoch| *epoch <= up_to) {
            park();
            true
        } else {
            false
        }
    }

    /// Why the session admitted with (`token`, `epoch`) may no longer play: a kick's reason
    /// if one revoked it, [`LOGGED_IN_ELSEWHERE`] if a newer login of the player is known.
    pub fn revoked(&self, token: &str, epoch: u64) -> Option<String> {
        let now = (self.now)();
        Self::revoked_in(&self.state(), token, epoch, now)
    }

    fn revoked_in(state: &OrderState, token: &str, epoch: u64, now: u64) -> Option<String> {
        if let Some(k) = state.kicked.get(token).filter(|k| now < k.expires && epoch <= k.up_to) {
            return Some(k.reason.clone());
        }
        state.known(token, now).filter(|known| *known > epoch).map(|_| LOGGED_IN_ELSEWHERE.to_string())
    }

    /// Announce the session admitted with (`token`, `epoch`): `connected` (its
    /// `ClientConnected`) is called under the lock unless the session was revoked (the reason
    /// is the refusal; nothing is announced). Under the lock, every order and kick is either
    /// seen here or sends its supersede after `connected`.
    pub fn announce(&self, token: &str, epoch: u64, connected: impl FnOnce()) -> Result<(), String> {
        let now = (self.now)();
        let state = self.state();
        if let Some(why) = Self::revoked_in(&state, token, epoch, now) {
            return Err(why);
        }
        connected();
        Ok(())
    }

    /// A verified ticket of `token` with `epoch` connects: admitted (its order consumed) only
    /// if it is the pending order's epoch and the order has not expired; else the refusal.
    /// Checked and consumed under one lock: of two connects with one ticket only one wins.
    pub fn admit(&self, token: &str, epoch: u64) -> Result<(), &'static str> {
        let now = (self.now)();
        let mut state = self.state();
        state.prune(now);
        match state.pending.by_token.get(token).copied() {
            Some((ordered, _)) if ordered == epoch => {
                state.pending.by_token.remove(token);
                state.admitted.insert(token.to_string(), epoch);
                Ok(())
            }
            Some((ordered, _)) if epoch < ordered => Err(REFUSE_SUPERSEDED),
            _ if state.known(token, now).is_some_and(|known| epoch < known) => Err(REFUSE_SUPERSEDED),
            _ => Err(REFUSE_NO_SPAWN_ORDER),
        }
    }

    /// The session admitted with (`token`, `epoch`) ended without ever playing (refused at
    /// its announcement, or its player unresolved): forgotten (unless a newer one of the
    /// player is live) and reported to the login service (role `host`: through the
    /// manager, [`ServerEvent::SessionEnded`]).
    pub fn ended(&self, token: &str, epoch: u64) {
        self.forget(token, epoch);
        match self.relay.get() {
            Some(events) => {
                let _ = events.send(ServerEvent::SessionEnded { player_token: token.to_string(), session_epoch: epoch });
            }
            None => self.tell_ended(token, epoch),
        }
    }

    /// The connection of the announced session admitted with (`token`, `epoch`) closed:
    /// forgotten as [`Self::ended`]. One host reports it to the login service here; with
    /// several the manager does, only once the session left the world (a session whose
    /// player was handed to another host has not: a kick for it must stay pending).
    pub fn closed(&self, token: &str, epoch: u64) {
        self.forget(token, epoch);
        if !self.relays() {
            self.tell_ended(token, epoch);
        }
    }

    fn forget(&self, token: &str, epoch: u64) {
        let now = (self.now)();
        let mut state = self.state();
        if state.admitted.get(token) == Some(&epoch) {
            state.admitted.remove(token);
        }
        let newest = state.used.get(token).map_or(epoch, |(used, _)| (*used).max(epoch));
        state.used.insert(token.to_string(), (newest, now.saturating_add(SPAWN_PENDING_TTL_SECS)));
    }

    /// `SessionEnded` to the login peers connected now (none linked: nothing).
    fn tell_ended(&self, token: &str, epoch: u64) {
        if self.login.get().is_some() {
            self.tell_login(HostToLogin::SessionEnded { player_token: token.to_string(), session_epoch: epoch });
        }
    }
}

/// The running link: the listener (drop it from synchronous code) and its address and pin.
pub struct LoginLink {
    listener: LinkListener,
}

impl LoginLink {
    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr()
    }

    /// The pin the login registry needs (`world add --link-cert`).
    pub fn cert_sha256(&self) -> &str {
        self.listener.cert_sha256()
    }
}

/// Listen for the login service on `bind` (port 0 picks one) with the service `token`; the
/// pin goes to `cert_hash_file` (its directory is created). Orders land in `orders`; players
/// are resolved through `players`; supersedes go to the manager on `events`.
pub fn start(
    bind: SocketAddr,
    token: String,
    cert_hash_file: Option<PathBuf>,
    orders: Arc<SpawnOrders>,
    players: SharedPlayerStore,
    events: Sender<ServerEvent>,
) -> io::Result<LoginLink> {
    start_with(bind, token, String::new(), cert_hash_file, orders, players, events)
}

/// [`start`] for the login service of world `world` ([`svc_link::LinkHello::world`]).
pub fn start_in_world(
    bind: SocketAddr,
    token: String,
    world: String,
    cert_hash_file: Option<PathBuf>,
    orders: Arc<SpawnOrders>,
    players: SharedPlayerStore,
    events: Sender<ServerEvent>,
) -> io::Result<LoginLink> {
    start_with(bind, token, world, cert_hash_file, orders, players, events)
}

fn start_with(
    bind: SocketAddr,
    token: String,
    world: String,
    cert_hash_file: Option<PathBuf>,
    orders: Arc<SpawnOrders>,
    players: SharedPlayerStore,
    events: Sender<ServerEvent>,
) -> io::Result<LoginLink> {
    if orders.relays() {
        return Err(io::Error::other("relaying spawn orders (role host) take the coordinator's orders, not a login link"));
    }
    if let Some(dir) = cert_hash_file.as_deref().and_then(std::path::Path::parent).filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let (listener, link_events, senders) = LinkListener::bind::<LoginToHost, HostToLogin>(bind, token, world, cert_hash_file)?;
    if orders.login.set(LoginPeers { senders: senders.clone(), peers: Mutex::default() }).is_err() {
        return Err(io::Error::other("these spawn orders already have a login link"));
    }
    log::info!("[login-link] listening for the login service on {}", listener.local_addr());
    std::thread::Builder::new()
        .name("login-link".into())
        .spawn(move || serve(link_events, senders, orders, players, events))?;
    Ok(LoginLink { listener })
}

fn serve(
    link_events: Receiver<(PeerId, LinkEvent<LoginToHost>)>,
    senders: PeerSenders<HostToLogin>,
    orders: Arc<SpawnOrders>,
    players: SharedPlayerStore,
    events: Sender<ServerEvent>,
) {
    let login_peers = |f: &mut dyn FnMut(&mut BTreeSet<PeerId>)| {
        if let Some(login) = orders.login.get() {
            f(&mut login.peers.lock().unwrap_or_else(|p| p.into_inner()));
        }
    };
    for (peer, event) in link_events.iter() {
        match event {
            LinkEvent::Up(hello) if hello.role == PeerRole::Login => {
                log::info!("[login-link] login service connected (peer {})", peer.0);
                login_peers(&mut |p| {
                    p.insert(peer);
                });
            }
            LinkEvent::Up(hello) => {
                log::error!("[login-link] peer {} is {:?}, not the login service: closed", peer.0, hello.role);
                senders.close(peer);
            }
            LinkEvent::Down(why) => {
                login_peers(&mut |p| {
                    p.remove(&peer);
                });
                log::info!("[login-link] peer {} gone: {why}", peer.0);
            }
            LinkEvent::Msg(msg) => {
                let mut is_login = false;
                login_peers(&mut |p| is_login = p.contains(&peer));
                if !is_login {
                    log::warn!("[login-link] message from peer {} that is not the login service: ignored", peer.0);
                    continue;
                }
                match msg {
                    LoginToHost::SpawnPlayer { player_token, session_epoch } => {
                        let ok = spawn(&orders, &players, &events, &player_token, session_epoch);
                        senders.send(peer, spawn_ack(player_token, session_epoch, ok, None));
                    }
                    LoginToHost::Kick { player_token, session_epoch, reason } => {
                        if kick(&orders, &players, &events, &player_token, session_epoch, reason) {
                            senders.send(peer, HostToLogin::KickAck { player_token, session_epoch });
                        }
                    }
                }
            }
        }
    }
    log::info!("[login-link] stopped");
}

/// The acknowledgement of a spawn order; `endpoint`: the host that took it (`None`: the
/// world row's addresses).
fn spawn_ack(player_token: String, session_epoch: u64, ok: bool, endpoint: Option<game::HostEndpoint>) -> HostToLogin {
    let (quic, webtransport, cert_hash) = match endpoint {
        Some(e) => (e.quic, e.webtransport, e.cert_hash),
        None => (None, None, None),
    };
    HostToLogin::SpawnAck { player_token, session_epoch, ok, quic, webtransport, cert_hash }
}

/// The player of a login token and its stored record, through the player store (created
/// if new). `None` (logged) for a malformed token or a store failure.
fn resolve(players: &SharedPlayerStore, token: &str) -> Option<(game::PlayerId, Option<game::PlayerRecord>)> {
    let token = PlayerToken(token.to_string());
    if !token.is_valid() {
        log::warn!("[login-link] a malformed player token");
        return None;
    }
    let resolved = players.lock().unwrap_or_else(|p| p.into_inner()).resolve(&PlayerKey::Online(token));
    match resolved {
        Ok(found) => Some(found),
        Err(e) => {
            log::error!("[login-link] player store: {e}");
            None
        }
    }
}

/// [`resolve`] without waiting for the store ([`crate::player_store::PlayerStore::resolve_later`]);
/// `None` (logged) for a malformed token.
fn resolve_later(players: &SharedPlayerStore, token: &str) -> Option<Resolving> {
    let token = PlayerToken(token.to_string());
    if !token.is_valid() {
        log::warn!("[login-link] a malformed player token");
        return None;
    }
    Some(players.lock().unwrap_or_else(|p| p.into_inner()).resolve_later(&PlayerKey::Online(token)))
}

/// The player of a login token ([`resolve`]).
fn player_of(players: &SharedPlayerStore, token: &str) -> Option<game::PlayerId> {
    resolve(players, token).map(|(player, _)| player)
}

/// `SpawnPlayer`: whether it is acked ok.
fn spawn(
    orders: &SpawnOrders,
    players: &SharedPlayerStore,
    events: &Sender<ServerEvent>,
    token: &str,
    epoch: u64,
) -> bool {
    let Some(player) = player_of(players, token) else { return false };
    order_player(orders, events, token, epoch, player)
}

/// A spawn order of `player` (resolved) recorded: whether it is acked ok.
fn order_player(orders: &SpawnOrders, events: &Sender<ServerEvent>, token: &str, epoch: u64, player: game::PlayerId) -> bool {
    // Any session of the player parks now, before the new ticket is handed out (and, sent
    // under the orders' lock, before any session can be announced after the order).
    let supersede = || {
        let _ = events.send(ServerEvent::Supersede { player, reason: LOGGED_IN_ELSEWHERE.to_string() });
    };
    match orders.order_then(token, epoch, supersede) {
        SpawnOrder::New => {
            log::info!("[login-link] spawn order for player {} (epoch {epoch})", player.0);
            true
        }
        SpawnOrder::Repeat => true,
        SpawnOrder::Stale => {
            log::info!("[login-link] stale spawn order for player {} (epoch {epoch}): refused", player.0);
            false
        }
    }
}

/// `Kick`: orders up to `up_to` are dropped and a revoked session parked with `reason`.
/// Whether it is applied (to be acked): the revoked session's park queued, or none to park.
/// A session it revoked whose player cannot be resolved (a player store failure) leaves it
/// unapplied — unacked, the login service resends it. The player is resolved only when a
/// session is admitted (its row exists: no row is made for an unknown token).
fn kick(
    orders: &SpawnOrders,
    players: &SharedPlayerStore,
    events: &Sender<ServerEvent>,
    token: &str,
    up_to: u64,
    reason: String,
) -> bool {
    match orders.kick(token, up_to, &reason) {
        KickApplied::NoSession => true,
        KickApplied::NewerKept => {
            log::info!("[login-link] kick up to epoch {up_to}: the live session is a newer login; kept");
            true
        }
        KickApplied::Revoked => {
            let Some(player) = player_of(players, token) else {
                log::warn!("[login-link] kick up to epoch {up_to}: its session's player is unknown now; not acked");
                return false;
            };
            park_revoked(orders, events, token, up_to, &reason, player);
            true
        }
    }
}

/// A kick revoked `player`'s session (`token`, up to `up_to`): its supersede goes on `events`,
/// unless a newer login took the session over meanwhile ([`SpawnOrders::supersede_revoked`]).
fn park_revoked(orders: &SpawnOrders, events: &Sender<ServerEvent>, token: &str, up_to: u64, reason: &str, player: game::PlayerId) {
    let parked = orders.supersede_revoked(token, up_to, || {
        let _ = events.send(ServerEvent::Supersede { player, reason: reason.to_string() });
    });
    if parked {
        log::info!("[login-link] kick of player {} (up to epoch {up_to}): {reason}", player.0);
    }
}

/// The server's half of the login relay on every role-`host` process (any number of hosts):
/// the login service's orders the coordinator gave this host (the gate host) are resolved
/// without waiting on the store ([`OrderResolver`]), the spawn orders its manager takes are
/// recorded (the player's sessions superseded first, as one host does), the kicks relayed to it
/// applied, and what its manager answered goes up its own coordinator link
/// (`game::HostMsg::Login`).
pub struct LoginRelay {
    orders: Arc<SpawnOrders>,
    players: SharedPlayerStore,
    events: Sender<ServerEvent>,
    resolver: OrderResolver,
}

impl LoginRelay {
    /// The relay over `orders` (from now on they relay: [`SpawnOrders::relay_through`]);
    /// supersedes and edge-ended sessions go to the manager on `events`.
    pub fn new(orders: Arc<SpawnOrders>, players: SharedPlayerStore, events: Sender<ServerEvent>) -> LoginRelay {
        if !orders.relay_through(events.clone()) {
            log::warn!("[login-link] these spawn orders relay already");
        }
        LoginRelay { orders, players, events, resolver: OrderResolver::new() }
    }

    /// Orders waiting for their player's store read, and orders dropped at the bound since
    /// start ([`LOGIN_ORDERS_IN_FLIGHT_MAX`]).
    pub fn resolving(&self) -> (usize, u64) {
        (self.resolver.waiting(), self.resolver.dropped())
    }

    /// After the manager's upkeep: the coordinator's orders queued for resolution and those
    /// resolved handed on (they reach the manager on its event channel, with the supersedes a
    /// kick sent first) — never waiting on the store —, what it took and was relayed handled,
    /// and what it answered sent up its coordinator link.
    pub fn pump<S: game::RegionSpawner>(&mut self, manager: &mut game::WorldManager<S>) {
        let me = manager.host_id();
        let dropped_before = self.resolver.dropped();
        for order in manager.take_login_orders() {
            self.resolver.push(order, &self.players);
        }
        let dropped = self.resolver.dropped() - dropped_before;
        if dropped > 0 {
            log::warn!(
                "[login-link] {dropped} login order(s) dropped: {LOGIN_ORDERS_IN_FLIGHT_MAX} wait for their player's store read \
                 (the login service answers 503 at its timeout; the client retries)"
            );
        }
        for outcome in self.resolver.poll(&self.orders, &self.players, &self.events, me) {
            match outcome {
                OrderOutcome::Event(ev) => {
                    let _ = self.events.send(ev);
                }
                OrderOutcome::Refused { player_token, session_epoch } => manager.login_order_refused(player_token, session_epoch),
                // Logged without the token; unanswered, the login service resends it.
                OrderOutcome::Unapplied => {}
            }
        }
        for order in manager.take_spawn_here() {
            let ok = order_player(&self.orders, &self.events, &order.player_token, order.session_epoch, order.player);
            manager.spawn_answered(order, ok);
        }
        for k in manager.take_kicks_in() {
            let applied = kick(&self.orders, &self.players, &self.events, &k.player_token, k.session_epoch, k.reason.clone());
            // A revoking kick resolves its player with a synchronous store read (`player_of`),
            // which may wait up to the pool's acquire timeout (30 s) while the database is away;
            // two in one pass would pass `/livez`'s stall limit and restart a host for a database
            // outage. The read returned: the main loop is alive (this runs on it).
            crate::health::Health::global().loop_beat();
            manager.kick_applied(k, applied);
        }
        manager.send_login_out();
    }
}

