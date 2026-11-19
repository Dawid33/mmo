//! The server's side of the login service's orders: `SpawnPlayer` and `Kick` in, `SpawnAck`,
//! `KickAck` and `SessionEnded` out.
//!
//! A role-`all` `jwt` server that has a service token serves the coordinator API's two login
//! routes itself ([`start`], on [`HOST_LINK_BIND_ENV`], HTTPS with its pinned identity, the same
//! auth headers as the coordinator's API — `coordinator_api::check_auth`): `POST`
//! [`login_api::PATH_COORD_LOGIN_ORDERS`] takes an order (202 once queued), `GET`
//! [`login_api::PATH_COORD_LOGIN_ANSWERS`] long-polls its answers from an in-memory queue
//! ([`LoginAnswers`], at most [`LOGIN_ANSWERS_HELD_MAX`]: no store table — SQLite has none). So the
//! login service has one transport for both a split world and a role-`all` one. It admits a login
//! ticket only against a pending spawn order ([`SpawnOrders`]): the login service posts
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
//! Delivery is at-least-once: an order repeated with an epoch already known (ordered, live, or
//! used within [`SPAWN_PENDING_TTL_SECS`]) is acked again and changes nothing (no second park of
//! the session it admitted, no re-armed ticket).
//!
//! `SessionEnded` joins the answer queue like the acks; one dropped past
//! [`LOGIN_ANSWERS_HELD_MAX`] (the login service not reading) leaves the session open in its
//! registry (a kick for it later is a no-op here).
//!
//! The order is consumed and the supersede queued on the orders' thread BEFORE the ack is
//! queued, and the supersede shares the manager's event channel with the connects: the new
//! session's `ClientConnected` always comes after the old one's park was asked for.
//!
//! **Split roles** (Elastic 1 Task 6; over HTTP since the coordinator HTTP API plan): a
//! role-`host` process never serves the login service. The login service calls the
//! COORDINATOR's API, which converts each order ([`order_from`]) and relays it to one live host,
//! the gate host (`game::CoordMsg::Login`), as a row of its mailbox. There the main loop plays the server's
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
//! ([`answer_from`]) and writes it as a `login_outbox` row, which the login service reads.

use game::HostId;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use crossbeam::channel::{Receiver, Sender};
use game::{PlayerToken, ServerEvent, LOGGED_IN_ELSEWHERE};
use login_api::{HostToLogin, LoginPage, LoginToHost, SPAWN_PENDING_TTL_SECS};
use tokio::sync::watch;

use crate::coordinator_api::{AnswersQuery, Auth, MAILBOX_PAGE_MAX};

use crate::auth_edge::{Clock, REFUSE_SUPERSEDED};
use crate::player_store::{PlayerKey, Resolving, SharedPlayerStore};

/// Where a role-`all` server serves the login service's routes (`ip:port`, TCP, HTTPS).
pub const HOST_LINK_BIND_ENV: &str = "VOXEL_HOST_LINK_BIND";
pub const DEFAULT_HOST_LINK_BIND: &str = "127.0.0.1:6490";
/// Where the server writes its login routes' certificate SHA-256 pin (the login registry's
/// `host_link_cert`); its cert and key persist beside it (keep it under a git-ignored dir).
pub const HOST_LINK_CERT_HASH_FILE_ENV: &str = "VOXEL_HOST_LINK_CERT_HASH_FILE";
pub const DEFAULT_HOST_LINK_CERT_HASH_FILE: &str = "target/dev-r2/host-0.cert-sha256";

/// At most this many answers wait in a role-`all` server's queue for the login service
/// ([`LoginAnswers`]); beyond it the oldest is dropped (warned at most once per
/// [`LOGIN_ANSWERS_DROP_WARN_MS`]).
pub const LOGIN_ANSWERS_HELD_MAX: usize = 4096;
/// The warning about dropped answers comes at most this often.
pub const LOGIN_ANSWERS_DROP_WARN_MS: u64 = 60_000;

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

/// A role-`all` server's answers to the login service, in order, numbered: what `GET`
/// [`login_api::PATH_COORD_LOGIN_ANSWERS`] reads past a cursor (nothing is deleted by a read; at
/// most [`LOGIN_ANSWERS_HELD_MAX`] are kept, the oldest dropped first). The numbers start at this
/// process's start time in µs-ish units (ms × 1000), so a restarted server's answers number past
/// the previous one's; a cursor past every number issued (a clock that went back) reads from the
/// start. Every answer kind is idempotent at the login service, so a re-read costs nothing.
pub struct LoginAnswers {
    inner: Mutex<AnswersInner>,
    /// The newest number issued: what wakes a waiting poll.
    version: watch::Sender<u64>,
}

struct AnswersInner {
    rows: VecDeque<(u64, HostToLogin)>,
    /// The newest number issued.
    newest: u64,
    dropped: u64,
    warned_at: Option<Instant>,
}

impl LoginAnswers {
    fn new() -> LoginAnswers {
        let start = crate::roles::unix_ms_now().saturating_mul(1000);
        LoginAnswers {
            inner: Mutex::new(AnswersInner { rows: VecDeque::new(), newest: start, dropped: 0, warned_at: None }),
            version: watch::channel(start).0,
        }
    }

    /// `msg` for the login service, after every answer queued before it.
    fn push(&self, msg: HostToLogin) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.rows.len() >= LOGIN_ANSWERS_HELD_MAX {
            inner.rows.pop_front();
            inner.dropped += 1;
            if inner.warned_at.is_none_or(|at| at.elapsed() >= Duration::from_millis(LOGIN_ANSWERS_DROP_WARN_MS)) {
                log::warn!(
                    "[login-link] {LOGIN_ANSWERS_HELD_MAX} answers wait for the login service: the oldest dropped ({} since start)",
                    inner.dropped
                );
                inner.warned_at = Some(Instant::now());
            }
        }
        inner.newest += 1;
        let seq = inner.newest;
        inner.rows.push_back((seq, msg));
        drop(inner);
        self.version.send_replace(seq);
    }

    /// At most `limit` answers numbered past `after` (from the start when `after` is past the
    /// newest number issued).
    fn read(&self, after: u64, limit: usize) -> Vec<(u64, HostToLogin)> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let after = if after > inner.newest { 0 } else { after };
        inner.rows.iter().filter(|(seq, _)| *seq > after).take(limit).cloned().collect()
    }
}

/// The admission state of a host the login service pushes to. Shared by the link's thread
/// (orders, kicks) and the connect edge ([`admit`](Self::admit), [`ended`](Self::ended)).
pub struct SpawnOrders {
    state: Mutex<OrderState>,
    now: Clock,
    /// Set once the login routes serve (role `all`): where `SessionEnded` goes.
    login: OnceLock<Arc<LoginAnswers>>,
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

    /// `msg` for the login service (its answer queue; none served: dropped).
    fn tell_login(&self, msg: HostToLogin) {
        let Some(login) = self.login.get() else {
            // `HostToLogin`'s `Debug` hides the player token.
            log::warn!("[login-link] {msg:?} for the login service, which this server does not serve: dropped");
            return;
        };
        login.push(msg);
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

    /// `SessionEnded` to the login service (not served: nothing).
    fn tell_ended(&self, token: &str, epoch: u64) {
        if self.login.get().is_some() {
            self.tell_login(HostToLogin::SessionEnded { player_token: token.to_string(), session_epoch: epoch });
        }
    }
}

/// Role `all`'s login routes, serving: their address and pin. Dropping it stops them.
pub struct LoginLink {
    addr: SocketAddr,
    pin: String,
    _stop: watch::Sender<bool>,
}

impl LoginLink {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL the login registry needs (`world add --link`).
    pub fn url(&self) -> String {
        format!("https://{}", self.addr)
    }

    /// The pin the login registry needs (`world add --link-cert`).
    pub fn cert_sha256(&self) -> &str {
        &self.pin
    }
}

/// What the login routes share.
struct RouteState {
    auth: Auth,
    orders: Sender<LoginToHost>,
    answers: Arc<LoginAnswers>,
}

/// Serve the login routes on `bind` (port 0 picks one) to callers presenting the service `token`
/// and naming world `world`; the identity `<cert_hash_file>.cert` / `.key`, its pin written to
/// `cert_hash_file` (its directory is created). Orders land in `orders` (on a thread of their
/// own, in arrival order); players are resolved through `players`; supersedes go to the manager
/// on `events`.
pub fn start(
    bind: SocketAddr,
    token: String,
    world: String,
    cert_hash_file: PathBuf,
    orders: Arc<SpawnOrders>,
    players: SharedPlayerStore,
    events: Sender<ServerEvent>,
) -> io::Result<LoginLink> {
    if orders.relays() {
        return Err(io::Error::other("relaying spawn orders (role host) take the coordinator's orders, not the login service's"));
    }
    if token.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "the service token is empty"));
    }
    if let Some(dir) = cert_hash_file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let answers = Arc::new(LoginAnswers::new());
    if orders.login.set(answers.clone()).is_err() {
        return Err(io::Error::other("these spawn orders already serve the login service"));
    }
    let (tls, pin) = svc_link::pinned_server_config(&cert_hash_file, &[b"h2"])?;
    let listener = std::net::TcpListener::bind(bind)?;
    let addr = listener.local_addr()?;
    let (orders_tx, orders_rx) = crossbeam::channel::unbounded();
    let state = Arc::new(RouteState { auth: Auth { token, world }, orders: orders_tx, answers: answers.clone() });
    let router = Router::new()
        .route(login_api::PATH_COORD_LOGIN_ORDERS, post(take_order))
        .route(login_api::PATH_COORD_LOGIN_ANSWERS, get(read_answers))
        .layer(axum::extract::DefaultBodyLimit::max(crate::coordinator_api::API_BODY_MAX_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), auth_layer))
        .with_state(state);
    std::thread::Builder::new()
        .name("login-link".into())
        .spawn(move || serve(orders_rx, answers, orders, players, events))?;
    let stop = crate::coordinator_api::serve_tls("login-link-api", listener, tls, router)?;
    log::info!("[login-link] serving the login service on https://{addr} (pin {pin})");
    Ok(LoginLink { addr, pin, _stop: stop })
}

async fn auth_layer(State(s): State<Arc<RouteState>>, req: Request, next: Next) -> Response {
    if let Err(refused) = crate::coordinator_api::check_auth(&s.auth, req.headers()) {
        return refused;
    }
    next.run(req).await
}

/// `POST` an order: 202 once queued for the orders' thread (its answer comes as a queued answer).
async fn take_order(State(s): State<Arc<RouteState>>, body: Bytes) -> Response {
    let Ok(msg) = game::decode_packet::<LoginToHost>(&body) else {
        return crate::coordinator_api::text(StatusCode::BAD_REQUEST, "order");
    };
    if s.orders.send(msg).is_err() {
        return crate::coordinator_api::text(StatusCode::SERVICE_UNAVAILABLE, "not_ready");
    }
    StatusCode::ACCEPTED.into_response()
}

/// `GET` the answers past `after`, long-polled as the coordinator's.
async fn read_answers(State(s): State<Arc<RouteState>>, Query(q): Query<AnswersQuery>) -> Response {
    let versions = s.answers.version.subscribe();
    let read = || {
        let rows = s.answers.read(q.after, MAILBOX_PAGE_MAX as usize);
        async move { Ok::<_, std::convert::Infallible>(rows) }
    };
    let Ok(rows) = crate::coordinator_api::long_poll(&versions, q.wait_ms, read).await;
    crate::coordinator_api::packet(StatusCode::OK, &LoginPage { rows })
}

/// The orders' thread: each order applied in arrival order, its answer queued.
fn serve(
    orders_rx: Receiver<LoginToHost>,
    answers: Arc<LoginAnswers>,
    orders: Arc<SpawnOrders>,
    players: SharedPlayerStore,
    events: Sender<ServerEvent>,
) {
    for msg in orders_rx.iter() {
        match msg {
            LoginToHost::SpawnPlayer { player_token, session_epoch } => {
                let ok = spawn(&orders, &players, &events, &player_token, session_epoch);
                answers.push(spawn_ack(player_token, session_epoch, ok, None));
            }
            LoginToHost::Kick { player_token, session_epoch, reason } => {
                if kick(&orders, &players, &events, &player_token, session_epoch, reason) {
                    answers.push(HostToLogin::KickAck { player_token, session_epoch });
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

