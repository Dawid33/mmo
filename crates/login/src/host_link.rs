//! The login service's side of the world hosts' service links (`svc-link`, role `Login`):
//! [`HostLinks`] keeps one [`LinkDialer`] per world whose registry row names a `host_link`,
//! pushes [`LoginToHost::SpawnPlayer`] on a join (and waits for its ack) and
//! [`LoginToHost::Kick`] on revocation, and records [`HostToLogin::SessionEnded`].
//!
//! The dialer is crossbeam (sync); the HTTP handlers are async. Each link has a pump thread
//! that turns the dialer's events into a `watch` of "up" and `oneshot` answers for the waiting
//! joins, so no handler ever blocks the runtime. An ack is matched by `(player_token,
//! session_epoch)`: epochs only grow, so a late ack for an epoch that already answered 503
//! never completes a later join.
//!
//! Delivery is at-least-once around reconnects (`svc-link`): a join resends its spawn order
//! when the link comes back before the ack, and the host treats a repeated epoch as the same
//! order (re-ack, nothing parked again).
//!
//! **Kicks are durable** (revocation must not fail open): a logout-everywhere / password
//! reset queues them in `pending_kicks` in the revocation's own transaction
//! ([`LoginStore::revoke_account_with_kicks`]), and the kick sweeper ([`HostLinks::start`])
//! sends every pending kick on each link `Up`, and again every [`KICK_RESEND_MS`] while up,
//! until the host's [`HostToLogin::KickAck`] deletes it. A kick revokes up to an epoch, so a
//! resend is harmless (the host applies it once, and never to a newer login). The sweeper also
//! dials the links of worlds with pending kicks, so kicks queued by the admin ctl (another
//! process) or before a restart go out too.
//!
//! `SessionEnded` is best effort: one sent while the link is down is lost, and the session
//! then stays open in the registry (a later kick for it is a no-op on the host).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam::channel::{Receiver, Sender};
use login_api::{HostToLogin, LoginToHost, SPAWN_ACK_TIMEOUT_MS};
use svc_link::{LinkDialer, LinkEvent, LinkHello, PeerRole};
use tokio::sync::{Notify, oneshot, watch};

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
    /// The registry row's `host_link` / `host_link_cert` is unusable.
    BadLink(String),
    /// The host answered `ok: false` (it could not take the order).
    Refused,
    /// No ack within [`SPAWN_ACK_TIMEOUT_MS`] (the link is down or the host is slow).
    Timeout,
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::NoServiceToken => write!(f, "no service token (set VOXEL_SERVICE_TOKEN or VOXEL_SERVICE_TOKEN_FILE)"),
            SpawnError::BadLink(why) => write!(f, "bad host link: {why}"),
            SpawnError::Refused => write!(f, "the host refused the spawn order"),
            SpawnError::Timeout => write!(f, "no ack within {SPAWN_ACK_TIMEOUT_MS} ms"),
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

type Waiters = Arc<Mutex<BTreeMap<(String, u64), oneshot::Sender<(bool, SpawnPlace)>>>>;

/// One world's link: the dialer's sender, whether it is up, and the joins waiting for acks.
struct HostLink {
    addr: SocketAddr,
    cert: String,
    tx: Sender<LoginToHost>,
    up: watch::Receiver<bool>,
    waiters: Waiters,
    /// Whether this is still the world's link; its pump writes the gauge only while it is.
    /// Held across the check and the write, so a replacement cannot interleave.
    current: Arc<Mutex<bool>>,
}

impl HostLink {
    /// The world got another link (or none): this one's pump no longer reports.
    fn retire(&self) {
        *self.current.lock().unwrap_or_else(|p| p.into_inner()) = false;
    }
}

/// The links to the world hosts, by world id. Shared by every handler.
pub struct HostLinks {
    token: Option<String>,
    store: LoginStore,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ack_timeout: Duration,
    links: Mutex<BTreeMap<String, Arc<HostLink>>>,
    /// Wakes the kick sweeper: a link came up, or kicks were queued.
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

    /// Every pending kick, over its world's link (dialed if need be) if that link is up.
    async fn send_pending_kicks(&self) {
        let now = (self.clock)();
        match self.store.expire_pending_kicks(now, PENDING_KICK_TTL_SECS).await {
            // Unacknowledged for at least the TTL (the sweep runs every `KICK_RESEND_MS`).
            Ok(expired) => for kick in expired {
                log::warn!(
                    "[login] kick for world {} player {} dropped after at least {} s unacknowledged",
                    kick.world_id, kick.player_token, PENDING_KICK_TTL_SECS
                );
            },
            Err(e) => log::error!("[login] expiring pending kicks: {e}"),
        }
        let kicks = match self.store.pending_kicks().await {
            Ok(kicks) => kicks,
            Err(e) => return log::error!("[login] reading pending kicks: {e}"),
        };
        self.metrics.pending_kicks(kicks.len() as u64);
        let mut world: Option<(String, Option<Arc<HostLink>>)> = None;
        for kick in kicks {
            if world.as_ref().is_none_or(|(id, _)| *id != kick.world_id) {
                let link = match self.store.world(&kick.world_id).await {
                    Ok(Some(row)) => match self.link(&row) {
                        Ok(link) => {
                            self.kick_blocked(&kick.world_id, None);
                            link
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
                world = Some((kick.world_id.clone(), link));
            }
            if let Some((_, Some(link))) = &world
                && *link.up.borrow()
            {
                let _ = link.tx.send(LoginToHost::Kick {
                    player_token: kick.player_token,
                    session_epoch: kick.session_epoch,
                    reason: kick.reason,
                });
            }
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

    /// The link to `world`'s host (dialed on first use; redialed when its row changed).
    /// `Ok(None)`: the world has no host link (R2a: nothing is pushed).
    fn link(&self, world: &WorldRow) -> Result<Option<Arc<HostLink>>, SpawnError> {
        let Some(addr) = &world.host_link else {
            // The row lost its link: the old one is retired and reads as down, once.
            let mut links = self.links.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(old) = links.remove(&world.world_id) {
                old.retire();
                self.metrics.host_link(&world.world_id, false);
            }
            return Ok(None);
        };
        let token = self.token.clone().ok_or(SpawnError::NoServiceToken)?;
        let addr: SocketAddr = addr.parse().map_err(|e| SpawnError::BadLink(format!("host_link {addr:?}: {e}")))?;
        let cert = world
            .host_link_cert
            .clone()
            .ok_or_else(|| SpawnError::BadLink("host_link_cert is not set".into()))?;
        let mut links = self.links.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(link) = links.get(&world.world_id).filter(|l| l.addr == addr && l.cert == cert) {
            return Ok(Some(link.clone()));
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| SpawnError::BadLink("host links are used from the async runtime only".into()))?;
        let hello = LinkHello::new(token, PeerRole::Login);
        let (tx, events) = LinkDialer::connect::<HostToLogin, LoginToHost>(addr, cert.clone(), hello);
        let (up_tx, up) = watch::channel(false);
        let waiters: Waiters = Arc::default();
        let current = Arc::new(Mutex::new(true));
        let pump = Pump {
            world: world.world_id.clone(),
            events,
            up: up_tx,
            waiters: waiters.clone(),
            store: self.store.clone(),
            clock: self.clock.clone(),
            runtime,
            kicks: self.kicks.clone(),
            metrics: self.metrics.clone(),
            current: current.clone(),
        };
        std::thread::Builder::new()
            .name(format!("host-link-{}", world.world_id))
            .spawn(move || pump.run())
            .map_err(|e| SpawnError::BadLink(format!("spawn the link thread: {e}")))?;
        log::info!("[login] host link for world {} dials {addr}", world.world_id);
        let link = Arc::new(HostLink { addr, cert, tx, up, waiters, current });
        // A replaced link's dialer stops when its last sender (in a finishing join) drops.
        if let Some(old) = links.insert(world.world_id.clone(), link.clone()) {
            old.retire();
        }
        // The new link is not up yet: the series follows it from now on.
        self.metrics.host_link(&world.world_id, false);
        Ok(Some(link))
    }

    /// Push `SpawnPlayer` to `world`'s host and wait (at most [`SPAWN_ACK_TIMEOUT_MS`]) for its
    /// ack: where the client connects, as the ack named it. `Ok` with no address at once for a
    /// world without a host link.
    pub async fn spawn(&self, world: &WorldRow, player_token: &str, session_epoch: u64) -> Result<SpawnPlace, SpawnError> {
        let Some(link) = self.link(world)? else { return Ok(SpawnPlace::default()) };
        let key = (player_token.to_string(), session_epoch);
        let (ack_tx, mut ack) = oneshot::channel();
        link.waiters.lock().unwrap_or_else(|p| p.into_inner()).insert(key.clone(), ack_tx);
        let order = LoginToHost::SpawnPlayer { player_token: player_token.to_string(), session_epoch };
        let mut up = link.up.clone();
        let answer = tokio::time::timeout(self.ack_timeout, async {
            loop {
                if up.wait_for(|u| *u).await.is_err() {
                    return None; // the pump ended: no ack can come
                }
                if link.tx.send(order.clone()).is_err() {
                    return None;
                }
                // The ack, or the link going down first: then resend once it is back.
                tokio::select! {
                    ok = &mut ack => return ok.ok(),
                    down = up.wait_for(|u| !*u) => if down.is_err() { return None },
                }
            }
        })
        .await;
        link.waiters.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
        match answer {
            Ok(Some((true, place))) => Ok(place),
            Ok(Some((false, _))) => Err(SpawnError::Refused),
            Ok(None) | Err(_) => Err(SpawnError::Timeout),
        }
    }
}

/// A link's event pump (its own thread): up / down into the watch, acks to their joins,
/// session ends into the store (on the runtime).
struct Pump {
    world: String,
    events: Receiver<LinkEvent<HostToLogin>>,
    up: watch::Sender<bool>,
    waiters: Waiters,
    store: LoginStore,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    runtime: tokio::runtime::Handle,
    /// Woken on every `Up`: pending kicks go out at once.
    kicks: Arc<Notify>,
    metrics: Metrics,
    /// See [`HostLink::current`].
    current: Arc<Mutex<bool>>,
}

impl Pump {
    /// Set the gauge, unless this link was replaced meanwhile.
    fn report(&self, up: bool) {
        let current = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if *current {
            self.metrics.host_link(&self.world, up);
        }
    }

    fn run(self) {
        for event in self.events.iter() {
            match event {
                LinkEvent::Up(_) => {
                    log::info!("[login] host link for world {} up", self.world);
                    self.up.send_replace(true);
                    self.report(true);
                    self.kicks.notify_one();
                }
                LinkEvent::Down(why) => {
                    if *self.up.borrow() {
                        log::warn!("[login] host link for world {} down: {why}", self.world);
                    }
                    self.up.send_replace(false);
                    self.report(false);
                }
                LinkEvent::Msg(HostToLogin::SpawnAck { player_token, session_epoch, ok, quic, webtransport, cert_hash }) => {
                    let waiter = self.waiters.lock().unwrap_or_else(|p| p.into_inner()).remove(&(player_token, session_epoch));
                    match waiter {
                        Some(waiter) => {
                            let _ = waiter.send((ok, SpawnPlace { quic, webtransport, cert_hash }));
                        }
                        // Answered already (a resend's second ack) or given up on (503).
                        None => log::debug!("[login] world {}: ack for epoch {session_epoch} nobody waits for", self.world),
                    }
                }
                LinkEvent::Msg(HostToLogin::KickAck { player_token, session_epoch }) => {
                    let (store, world) = (self.store.clone(), self.world.clone());
                    self.runtime.spawn(async move {
                        match store.kick_acked(&world, &player_token, session_epoch).await {
                            Ok(true) => log::info!("[login] world {world}: kick (epoch {session_epoch}) acknowledged"),
                            Ok(false) => {}
                            Err(e) => log::error!("[login] world {world}: recording a kick ack: {e}"),
                        }
                    });
                }
                LinkEvent::Msg(HostToLogin::SessionEnded { player_token, session_epoch }) => {
                    let (store, world) = (self.store.clone(), self.world.clone());
                    let now = i64::try_from((self.clock)()).unwrap_or(i64::MAX);
                    self.runtime.spawn(async move {
                        if let Err(e) = store.end_session(&world, &player_token, session_epoch, now).await {
                            log::error!("[login] world {world}: recording a session end: {e}");
                        }
                    });
                }
            }
        }
        // The dialer is gone (its sender dropped): nothing more can be acked.
        self.up.send_replace(false);
        self.report(false);
    }
}
