//! The login service's side of the worlds' service links (`svc-link`, role `Login`):
//! [`HostLinks`] keeps, per world whose registry row names a `host_link`, one [`LinkDialer`]
//! per address that link names — a world's coordinator replicas (split roles; each relays to
//! a gate host of its own and back; Elastic 2b), or a role-`all` server. `host_link` may name
//! several (a comma list, or a DNS name with several answers); it is re-resolved at every join
//! and every kick sweep: a new address is dialled, a gone one's link retired. Each hello names
//! the row's `world_id`. The service pushes [`LoginToHost::SpawnPlayer`] on a join (and waits
//! for its ack) and [`LoginToHost::Kick`] on revocation, and records
//! [`HostToLogin::SessionEnded`].
//!
//! An order goes out on ONE link (the first up one, in the row's order). A replica with no
//! gate host of its own answers [`HostToLogin::NoGate`] at once, and the order moves to the
//! next link; every link `NoGate` = no host takes logins (the join answers 503). Acks are
//! matched by `(player_token, session_epoch)` on ANY link of the world (an order's answer may
//! come back through another replica): epochs only grow, so a late ack for an epoch that
//! already answered 503 never completes a later join.
//!
//! The dialers are crossbeam (sync); the HTTP handlers are async. Each link has a pump thread
//! that turns the dialer's events into a `watch` of "up" and answers for the waiting joins, so
//! no handler ever blocks the runtime.
//!
//! Delivery is at-least-once around reconnects (`svc-link`): a join resends its spawn order
//! when its link drops before the ack (on whichever link is up then), and the hosts treat a
//! repeated epoch as the same order (re-ack, nothing parked again).
//!
//! **Kicks are durable** (revocation must not fail open): a logout-everywhere / password
//! reset queues them in `pending_kicks` in the revocation's own transaction
//! ([`LoginStore::revoke_account_with_kicks`]), and the kick sweeper ([`HostLinks::start`])
//! sends every pending kick on each link `Up`, and again every [`KICK_RESEND_MS`] while up,
//! until a [`HostToLogin::KickAck`] (on any link of its world) deletes it. Each kick goes on one
//! up link: the first that has not answered it `NoGate` (a `NoGate` wakes the sweeper, so it
//! moves to the next at once; once every up link said `NoGate`, a new round starts at most every
//! [`KICK_RESEND_MS`]). Which link is only in memory: the kick itself stays in the store. A kick
//! revokes up to an epoch, so a resend is harmless (the host applies it once, and never to a
//! newer login). The sweeper also dials the links of worlds with pending kicks, so kicks queued
//! by the admin ctl (another process) or before a restart go out too.
//!
//! `SessionEnded` is best effort: a replica holds the ones its hosts send while no login
//! service is linked to IT (a bounded queue), and one lost past that leaves the session open in
//! the registry (a later kick for it is a no-op on the host).

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, Sender};
use login_api::{HostToLogin, LoginToHost, SPAWN_ACK_TIMEOUT_MS};
use svc_link::{LinkDialer, LinkEvent, LinkHello, LinkTarget, PeerRole};
use tokio::sync::{Notify, mpsc, watch};

/// How often pending kicks are resent while their link is up (and new ones looked for).
pub const KICK_RESEND_MS: u64 = 2000;
/// A kick the host never acknowledges is dropped (with a warning) after this long.
pub const PENDING_KICK_TTL_SECS: u64 = 86_400;

use crate::metrics::Metrics;
use crate::{LoginStore, WorldRow};

/// Why a spawn order was not acknowledged (the join answers 503 `WorldUnavailable`).
#[derive(Debug, PartialEq, Eq)]
pub enum SpawnError {
    /// This login service has no service token (`VOXEL_SERVICE_TOKEN[_FILE]`).
    NoServiceToken,
    /// The registry row's `host_link` / `host_link_cert` is unusable (or names no address).
    BadLink(String),
    /// The host answered `ok: false` (it could not take the order).
    Refused,
    /// No ack within [`SPAWN_ACK_TIMEOUT_MS`] (the links are down or the host is slow), or
    /// every replica answered `NoGate` (no host of the world takes logins).
    Timeout,
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::NoServiceToken => write!(f, "no service token (set VOXEL_SERVICE_TOKEN or VOXEL_SERVICE_TOKEN_FILE)"),
            SpawnError::BadLink(why) => write!(f, "bad host link: {why}"),
            SpawnError::Refused => write!(f, "the host refused the spawn order"),
            SpawnError::Timeout => write!(f, "no ack within {SPAWN_ACK_TIMEOUT_MS} ms (or no replica has a gate host)"),
        }
    }
}

/// Where an acknowledged spawn order sends the client: the addresses of the host that took
/// it, as its [`HostToLogin::SpawnAck`] named them (all `None`: the world row's).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnPlace {
    pub quic: Option<String>,
    pub webtransport: Option<String>,
    pub cert_hash: Option<String>,
}

impl SpawnPlace {
    /// Whether the acknowledgement named any address (else the world row's are used).
    pub fn named(&self) -> bool {
        self.quic.is_some() || self.webtransport.is_some()
    }
}

/// What a link's pump tells a waiting join.
enum Answer {
    /// The order's ack (from any link of the world).
    Ack(bool, SpawnPlace),
    /// The replica at this address has no gate host: it relayed nothing.
    NoGate(SocketAddr),
}

/// Where a pending kick goes next ([`Linked::kick_link`]): the links that answered it
/// `NoGate` this round, and when the current round began.
#[derive(Default)]
struct KickRoute {
    said_no: BTreeSet<SocketAddr>,
    round_at: Option<Instant>,
}

/// What a world's links share with their pumps (and the joins that hold a [`Linked`]).
struct WorldShared {
    world: String,
    /// The joins waiting for an answer, by `(player_token, session_epoch)`: any link's ack.
    waiters: Mutex<BTreeMap<(String, u64), mpsc::UnboundedSender<Answer>>>,
    /// The pending kicks sent, by `(player_token, session_epoch)`.
    kick_routes: Mutex<BTreeMap<(String, u64), KickRoute>>,
    /// Bumped whenever a link of the world goes up or down.
    changed: watch::Sender<u64>,
    /// Each current link's state, for the world's gauge (up while any link is).
    states: Mutex<BTreeMap<SocketAddr, bool>>,
}

impl WorldShared {
    fn changed(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// Set the world's gauge from the current links' states.
    fn report(&self, states: &BTreeMap<SocketAddr, bool>, metrics: &Metrics) {
        metrics.host_link(&self.world, states.values().any(|up| *up));
    }
}

/// One replica's link: the dialer's sender and whether it is up.
struct HostLink {
    addr: SocketAddr,
    tx: Sender<LoginToHost>,
    up: watch::Receiver<bool>,
    /// Whether this is still one of the world's links; its pump reports only while it is.
    /// Held across the check and the write, so a retirement cannot interleave.
    current: Arc<Mutex<bool>>,
}

impl HostLink {
    fn is_up(&self) -> bool {
        *self.up.borrow()
    }
}

/// One world's links: the addresses its row resolved to (in the row's order) and a link per
/// address.
struct WorldLinks {
    /// The row's `host_link` and `host_link_cert` they were made for.
    target: String,
    cert: String,
    addrs: Vec<SocketAddr>,
    links: BTreeMap<SocketAddr, Arc<HostLink>>,
    shared: Arc<WorldShared>,
}

/// A world's links at one moment, in the row's order (what a join or a sweep works with).
struct Linked {
    shared: Arc<WorldShared>,
    links: Vec<Arc<HostLink>>,
}

impl Linked {
    /// The link the pending kick `key` goes on now: the first up link that has not answered it
    /// `NoGate` this round; once every up link has, a new round — at most one per
    /// [`KICK_RESEND_MS`] (`None` meanwhile, and while no link is up).
    fn kick_link(&self, key: (String, u64), now: Instant) -> Option<Arc<HostLink>> {
        let up: Vec<&Arc<HostLink>> = self.links.iter().filter(|l| l.is_up()).collect();
        let first = (*up.first()?).clone();
        let mut routes = self.shared.kick_routes.lock().unwrap_or_else(|p| p.into_inner());
        let route = routes.entry(key).or_default();
        if let Some(link) = up.iter().find(|l| !route.said_no.contains(&l.addr)) {
            return Some((*link).clone());
        }
        if route.round_at.is_some_and(|at| now.duration_since(at) < Duration::from_millis(KICK_RESEND_MS)) {
            return None;
        }
        route.said_no.clear();
        route.round_at = Some(now);
        Some(first)
    }
}

/// The links to the worlds' coordinator replicas (or role-`all` servers), by world id. Shared
/// by every handler.
pub struct HostLinks {
    token: Option<String>,
    store: LoginStore,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ack_timeout: Duration,
    links: Mutex<BTreeMap<String, WorldLinks>>,
    /// Wakes the kick sweeper: a link came up, kicks were queued, or a replica answered one
    /// `NoGate`.
    kicks: Arc<Notify>,
    /// Per world, why its kicks cannot be sent (as last logged): the sweeper warns once per
    /// change, not every round.
    kick_blocked: Mutex<BTreeMap<String, String>>,
    metrics: Metrics,
}

impl HostLinks {
    /// Links that dial with the service `token` (`None`: every world with a `host_link` is
    /// unavailable). `store` records `SessionEnded`; `clock` is seconds since the Unix epoch.
    pub fn new(token: Option<String>, store: LoginStore, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> HostLinks {
        HostLinks {
            token,
            store,
            clock,
            ack_timeout: Duration::from_millis(SPAWN_ACK_TIMEOUT_MS),
            links: Mutex::default(),
            kicks: Arc::new(Notify::new()),
            kick_blocked: Mutex::default(),
            metrics: Metrics::new(),
        }
    }

    /// Report link state and pending kicks to `metrics` (before links are dialed).
    pub fn with_metrics(mut self, metrics: Metrics) -> HostLinks {
        self.metrics = metrics;
        self
    }

    /// Start the kick sweeper on the current runtime (it ends when these links are dropped).
    /// Without a runtime (or a service token) there is nothing to start.
    pub fn start(self: &Arc<Self>) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
        if self.token.is_none() {
            return;
        }
        let (links, wake) = (Arc::downgrade(self), self.kicks.clone());
        runtime.spawn(async move {
            loop {
                let Some(links) = links.upgrade() else { return };
                links.send_pending_kicks().await;
                drop(links);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(KICK_RESEND_MS)) => {}
                    _ = wake.notified() => {}
                }
            }
        });
    }

    /// Kicks were queued: send them now.
    pub fn wake_kicks(&self) {
        self.kicks.notify_one();
    }

    /// Every pending kick, over one up link of its world (the links dialed if need be).
    async fn send_pending_kicks(&self) {
        let now = (self.clock)();
        match self.store.expire_pending_kicks(now, PENDING_KICK_TTL_SECS).await {
            // Unacknowledged for at least the TTL (the sweep runs every `KICK_RESEND_MS`).
            Ok(expired) => for kick in expired {
                // Never the player token in a log: the world and the kick's epoch name it.
                log::warn!(
                    "[login] kick for world {} (epoch {}) dropped after at least {} s unacknowledged",
                    kick.world_id, kick.session_epoch, PENDING_KICK_TTL_SECS
                );
            },
            Err(e) => log::error!("[login] expiring pending kicks: {e}"),
        }
        let kicks = match self.store.pending_kicks().await {
            Ok(kicks) => kicks,
            Err(e) => return log::error!("[login] reading pending kicks: {e}"),
        };
        self.metrics.pending_kicks(kicks.len() as u64);
        let swept = Instant::now();
        let mut pending: BTreeMap<String, BTreeSet<(String, u64)>> = BTreeMap::new();
        let mut world: Option<(String, Option<Linked>)> = None;
        for kick in kicks {
            if world.as_ref().is_none_or(|(id, _)| *id != kick.world_id) {
                let linked = match self.store.world(&kick.world_id).await {
                    Ok(Some(row)) => match self.world_links(&row).await {
                        Ok(linked) => {
                            self.kick_blocked(&kick.world_id, None);
                            linked
                        }
                        Err(e) => {
                            self.kick_blocked(&kick.world_id, Some(e.to_string()));
                            None
                        }
                    },
                    // A removed world: its kicks wait (and go if it is registered again).
                    Ok(None) => None,
                    Err(e) => return log::error!("[login] reading world {}: {e}", kick.world_id),
                };
                world = Some((kick.world_id.clone(), linked));
            }
            let key = (kick.player_token.clone(), kick.session_epoch);
            pending.entry(kick.world_id.clone()).or_default().insert(key.clone());
            if let Some((_, Some(linked))) = &world
                && let Some(link) = linked.kick_link(key, swept)
            {
                let _ = link.tx.send(LoginToHost::Kick {
                    player_token: kick.player_token,
                    session_epoch: kick.session_epoch,
                    reason: kick.reason,
                });
            }
        }
        // A kick acknowledged (or expired) needs no route any more.
        let links = self.links.lock().unwrap_or_else(|p| p.into_inner());
        for (id, w) in links.iter() {
            let keep = pending.get(id);
            w.shared.kick_routes.lock().unwrap_or_else(|p| p.into_inner()).retain(|k, _| keep.is_some_and(|s| s.contains(k)));
        }
    }

    /// Note why `world`'s kicks cannot go (`None`: they can): warned once per change.
    fn kick_blocked(&self, world: &str, why: Option<String>) {
        let mut blocked = self.kick_blocked.lock().unwrap_or_else(|p| p.into_inner());
        match why {
            Some(why) if blocked.get(world) != Some(&why) => {
                log::warn!("[login] kicks for world {world} cannot be sent: {why} (they wait)");
                blocked.insert(world.to_string(), why);
            }
            Some(_) => {}
            None => {
                if blocked.remove(world).is_some() {
                    log::info!("[login] kicks for world {world} can be sent again");
                }
            }
        }
    }

    /// Wait at most `timeout` for an ack (or for a link to come up before a kick). Tests.
    pub fn with_ack_timeout(mut self, timeout: Duration) -> HostLinks {
        self.ack_timeout = timeout;
        self
    }

    /// `world`'s links, one per address its row's `host_link` resolves to now: a new address
    /// is dialed, a gone one's link retired (a changed `host_link_cert` redials them all).
    /// A failed resolution keeps the links of an unchanged row. `Ok(None)`: the world has no
    /// host link (R2a: nothing is pushed).
    async fn world_links(&self, world: &WorldRow) -> Result<Option<Linked>, SpawnError> {
        let Some(target) = &world.host_link else {
            // The row lost its link: the old ones are retired and the world reads down, once.
            let mut links = self.links.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(old) = links.remove(&world.world_id) {
                for link in old.links.values() {
                    self.retire(&old.shared, link);
                }
                self.metrics.host_link(&world.world_id, false);
            }
            return Ok(None);
        };
        let token = self.token.clone().ok_or(SpawnError::NoServiceToken)?;
        let parsed = LinkTarget::parse(target).map_err(|e| SpawnError::BadLink(format!("host_link {target:?}: {e}")))?;
        let cert = world
            .host_link_cert
            .clone()
            .ok_or_else(|| SpawnError::BadLink("host_link_cert is not set".into()))?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| SpawnError::BadLink("host links are used from the async runtime only".into()))?;
        let resolved = parsed.resolve_all().await;
        let mut all = self.links.lock().unwrap_or_else(|p| p.into_inner());
        let entry = all.entry(world.world_id.clone()).or_insert_with(|| WorldLinks {
            target: String::new(),
            cert: String::new(),
            addrs: Vec::new(),
            links: BTreeMap::new(),
            shared: Arc::new(WorldShared {
                world: world.world_id.clone(),
                waiters: Mutex::default(),
                kick_routes: Mutex::default(),
                changed: watch::channel(0).0,
                states: Mutex::default(),
            }),
        });
        let addrs = match resolved {
            Ok(found) => {
                let mut addrs = Vec::new();
                for addr in found {
                    if !addrs.contains(&addr) {
                        addrs.push(addr);
                    }
                }
                addrs
            }
            Err(e) if entry.target == *target && entry.cert == cert && !entry.addrs.is_empty() => {
                log::debug!("[login] world {}: resolving {target}: {e} (its links are kept)", world.world_id);
                entry.addrs.clone()
            }
            Err(e) => return Err(SpawnError::BadLink(format!("host_link {target:?}: {e}"))),
        };
        let shared = entry.shared.clone();
        if entry.cert != cert {
            for link in std::mem::take(&mut entry.links).values() {
                self.retire(&shared, link);
            }
        }
        let gone: Vec<SocketAddr> = entry.links.keys().filter(|a| !addrs.contains(a)).copied().collect();
        for addr in gone {
            if let Some(link) = entry.links.remove(&addr) {
                log::info!("[login] world {}: {addr} is no longer in its host link: retired", world.world_id);
                self.retire(&shared, &link);
            }
        }
        for addr in &addrs {
            if !entry.links.contains_key(addr) {
                let link = self.dial(&shared, *addr, &cert, token.clone(), runtime.clone())?;
                entry.links.insert(*addr, link);
            }
        }
        (entry.target, entry.cert, entry.addrs) = (target.clone(), cert, addrs);
        let links = entry.addrs.iter().map(|a| entry.links[a].clone()).collect();
        Ok(Some(Linked { shared, links }))
    }

    /// A new link to the replica at `addr` of `shared`'s world, its pump started.
    fn dial(
        &self,
        shared: &Arc<WorldShared>,
        addr: SocketAddr,
        cert: &str,
        token: String,
        runtime: tokio::runtime::Handle,
    ) -> Result<Arc<HostLink>, SpawnError> {
        // The world row's id: the coordinator (or a role-`all` server) admits only its own world.
        let hello = LinkHello::new(token, PeerRole::Login, shared.world.clone());
        let (tx, events) = LinkDialer::connect::<HostToLogin, LoginToHost>(addr, cert.to_string(), hello);
        let (up_tx, up) = watch::channel(false);
        let current = Arc::new(Mutex::new(true));
        let pump = Pump {
            addr,
            events,
            up: up_tx,
            shared: shared.clone(),
            store: self.store.clone(),
            clock: self.clock.clone(),
            runtime,
            kicks: self.kicks.clone(),
            metrics: self.metrics.clone(),
            current: current.clone(),
        };
        std::thread::Builder::new()
            .name(format!("host-link-{}", shared.world))
            .spawn(move || pump.run())
            .map_err(|e| SpawnError::BadLink(format!("spawn the link thread: {e}")))?;
        log::info!("[login] host link for world {} dials {addr}", shared.world);
        // The new link is not up yet: the world's series follows it from now on.
        let mut states = shared.states.lock().unwrap_or_else(|p| p.into_inner());
        states.insert(addr, false);
        shared.report(&states, &self.metrics);
        // Its dialer stops when its last sender (in a finishing join) drops.
        Ok(Arc::new(HostLink { addr, tx, up, current }))
    }

    /// `link` is no longer one of its world's: its pump stops reporting, the world's series
    /// follows the others.
    fn retire(&self, shared: &WorldShared, link: &HostLink) {
        *link.current.lock().unwrap_or_else(|p| p.into_inner()) = false;
        let mut states = shared.states.lock().unwrap_or_else(|p| p.into_inner());
        states.remove(&link.addr);
        shared.report(&states, &self.metrics);
        drop(states);
        shared.changed();
    }

    /// The links `world`'s row names now (dialed, and gone ones retired, as a join does), each
    /// address with whether it is up, in the row's order. Tests and diagnostics.
    pub async fn link_states(&self, world: &WorldRow) -> Result<Vec<(String, bool)>, SpawnError> {
        let linked = self.world_links(world).await?;
        Ok(linked.map(|l| l.links.iter().map(|k| (k.addr.to_string(), k.is_up())).collect()).unwrap_or_default())
    }

    /// Push `SpawnPlayer` to `world`'s replicas — one link at a time, the next after a
    /// `NoGate` — and wait (at most [`SPAWN_ACK_TIMEOUT_MS`]) for its ack on any link: where the
    /// client connects, as the ack named it. `Ok` with no address at once for a world without
    /// a host link; [`SpawnError::Timeout`] at once when every replica answered `NoGate`.
    pub async fn spawn(&self, world: &WorldRow, player_token: &str, session_epoch: u64) -> Result<SpawnPlace, SpawnError> {
        let Some(linked) = self.world_links(world).await? else { return Ok(SpawnPlace::default()) };
        let key = (player_token.to_string(), session_epoch);
        let (answer_tx, mut answers) = mpsc::unbounded_channel();
        linked.shared.waiters.lock().unwrap_or_else(|p| p.into_inner()).insert(key.clone(), answer_tx);
        let order = LoginToHost::SpawnPlayer { player_token: player_token.to_string(), session_epoch };
        let mut changed = linked.shared.changed.subscribe();
        let answer = tokio::time::timeout(self.ack_timeout, async {
            let mut said_no: BTreeSet<SocketAddr> = BTreeSet::new();
            loop {
                let open: Vec<&Arc<HostLink>> = linked.links.iter().filter(|l| !said_no.contains(&l.addr)).collect();
                if open.is_empty() {
                    log::debug!("[login] world {}: every replica answered NoGate (epoch {session_epoch})", world.world_id);
                    return None;
                }
                let Some(link) = open.into_iter().find(|l| l.is_up()) else {
                    // No open link is up: wait for one (an earlier send's ack may come meanwhile).
                    tokio::select! {
                        a = answers.recv() => match a {
                            Some(Answer::Ack(ok, place)) => return Some((ok, place)),
                            Some(Answer::NoGate(from)) => { said_no.insert(from); }
                            None => return None,
                        },
                        c = changed.changed() => if c.is_err() { return None },
                    }
                    continue;
                };
                if link.tx.send(order.clone()).is_err() {
                    said_no.insert(link.addr); // its dialer is gone
                    continue;
                }
                // The ack (on any link), this link's NoGate, or this link going down first: then
                // the order goes out again on whichever open link is up.
                let mut up = link.up.clone();
                loop {
                    tokio::select! {
                        a = answers.recv() => match a {
                            Some(Answer::Ack(ok, place)) => return Some((ok, place)),
                            Some(Answer::NoGate(from)) => {
                                said_no.insert(from);
                                if from == link.addr {
                                    break;
                                }
                            }
                            None => return None,
                        },
                        _ = up.wait_for(|u| !*u) => break,
                    }
                }
            }
        })
        .await;
        linked.shared.waiters.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
        match answer {
            Ok(Some((true, place))) => Ok(place),
            Ok(Some((false, _))) => Err(SpawnError::Refused),
            Ok(None) | Err(_) => Err(SpawnError::Timeout),
        }
    }
}

/// A link's event pump (its own thread): up / down into the watch, answers to their joins (or
/// a kick's route), kick acks and session ends into the store (on the runtime).
struct Pump {
    addr: SocketAddr,
    events: Receiver<LinkEvent<HostToLogin>>,
    up: watch::Sender<bool>,
    shared: Arc<WorldShared>,
    store: LoginStore,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    runtime: tokio::runtime::Handle,
    /// Woken on every `Up` (pending kicks go out at once) and on a kick's `NoGate`.
    kicks: Arc<Notify>,
    metrics: Metrics,
    /// See [`HostLink::current`].
    current: Arc<Mutex<bool>>,
}

impl Pump {
    /// Set this link's state and the world's gauge, unless this link was retired meanwhile.
    fn report(&self, up: bool) {
        let current = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if *current {
            let mut states = self.shared.states.lock().unwrap_or_else(|p| p.into_inner());
            states.insert(self.addr, up);
            self.shared.report(&states, &self.metrics);
        }
    }

    fn set_up(&self, up: bool) {
        self.up.send_replace(up);
        self.report(up);
        self.shared.changed();
    }

    fn answer(&self, key: &(String, u64), answer: Answer) -> bool {
        let waiters = self.shared.waiters.lock().unwrap_or_else(|p| p.into_inner());
        waiters.get(key).is_some_and(|w| w.send(answer).is_ok())
    }

    fn run(self) {
        let world = self.shared.world.clone();
        let addr = self.addr;
        for event in self.events.iter() {
            match event {
                LinkEvent::Up(_) => {
                    log::info!("[login] host link for world {world} ({addr}) up");
                    self.set_up(true);
                    self.kicks.notify_one();
                }
                LinkEvent::Down(why) => {
                    if *self.up.borrow() {
                        log::warn!("[login] host link for world {world} ({addr}) down: {why}");
                    }
                    self.set_up(false);
                }
                LinkEvent::Msg(HostToLogin::SpawnAck { player_token, session_epoch, ok, quic, webtransport, cert_hash }) => {
                    let key = (player_token, session_epoch);
                    if !self.answer(&key, Answer::Ack(ok, SpawnPlace { quic, webtransport, cert_hash })) {
                        // Answered already (a resend's second ack) or given up on (503).
                        log::debug!("[login] world {world}: ack for epoch {session_epoch} nobody waits for");
                    }
                }
                LinkEvent::Msg(HostToLogin::NoGate { player_token, session_epoch }) => {
                    let key = (player_token, session_epoch);
                    let join = self.answer(&key, Answer::NoGate(addr));
                    let kick = {
                        let mut routes = self.shared.kick_routes.lock().unwrap_or_else(|p| p.into_inner());
                        routes.get_mut(&key).map(|r| r.said_no.insert(addr)).is_some()
                    };
                    if kick {
                        // The kick moves to the next link now.
                        self.kicks.notify_one();
                    }
                    if !join && !kick {
                        log::debug!("[login] world {world}: NoGate from {addr} for epoch {session_epoch} nobody waits for");
                    }
                }
                LinkEvent::Msg(HostToLogin::KickAck { player_token, session_epoch }) => {
                    let (store, world) = (self.store.clone(), world.clone());
                    self.runtime.spawn(async move {
                        match store.kick_acked(&world, &player_token, session_epoch).await {
                            Ok(true) => log::info!("[login] world {world}: kick (epoch {session_epoch}) acknowledged"),
                            Ok(false) => {}
                            Err(e) => log::error!("[login] world {world}: recording a kick ack: {e}"),
                        }
                    });
                }
                LinkEvent::Msg(HostToLogin::SessionEnded { player_token, session_epoch }) => {
                    let (store, world) = (self.store.clone(), world.clone());
                    let now = i64::try_from((self.clock)()).unwrap_or(i64::MAX);
                    self.runtime.spawn(async move {
                        if let Err(e) = store.end_session(&world, &player_token, session_epoch, now).await {
                            log::error!("[login] world {world}: recording a session end: {e}");
                        }
                    });
                }
            }
        }
        // The dialer is gone (its sender dropped): nothing more comes on this link.
        self.set_up(false);
    }
}
