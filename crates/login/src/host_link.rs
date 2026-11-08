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

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam::channel::{Receiver, Sender};
use login_api::{HostToLogin, LoginToHost, SPAWN_ACK_TIMEOUT_MS};
use svc_link::{LinkDialer, LinkEvent, LinkHello, PeerRole};
use tokio::sync::{oneshot, watch};

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

type Waiters = Arc<Mutex<BTreeMap<(String, u64), oneshot::Sender<bool>>>>;

/// One world's link: the dialer's sender, whether it is up, and the joins waiting for acks.
struct HostLink {
    addr: SocketAddr,
    cert: String,
    tx: Sender<LoginToHost>,
    up: watch::Receiver<bool>,
    waiters: Waiters,
}

/// The links to the world hosts, by world id. Shared by every handler.
pub struct HostLinks {
    token: Option<String>,
    store: LoginStore,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ack_timeout: Duration,
    links: Mutex<BTreeMap<String, Arc<HostLink>>>,
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
        let Some(addr) = &world.host_link else { return Ok(None) };
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
        let pump = Pump {
            world: world.world_id.clone(),
            events,
            up: up_tx,
            waiters: waiters.clone(),
            store: self.store.clone(),
            clock: self.clock.clone(),
            runtime,
        };
        std::thread::Builder::new()
            .name(format!("host-link-{}", world.world_id))
            .spawn(move || pump.run())
            .map_err(|e| SpawnError::BadLink(format!("spawn the link thread: {e}")))?;
        log::info!("[login] host link for world {} dials {addr}", world.world_id);
        let link = Arc::new(HostLink { addr, cert, tx, up, waiters });
        // A replaced link's dialer stops when its last sender (in a finishing join) drops.
        links.insert(world.world_id.clone(), link.clone());
        Ok(Some(link))
    }

    /// Push `SpawnPlayer` to `world`'s host and wait (at most [`SPAWN_ACK_TIMEOUT_MS`]) for its
    /// ack. `Ok(())` at once for a world without a host link.
    pub async fn spawn(&self, world: &WorldRow, player_token: &str, session_epoch: u64) -> Result<(), SpawnError> {
        let Some(link) = self.link(world)? else { return Ok(()) };
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
            Ok(Some(true)) => Ok(()),
            Ok(Some(false)) => Err(SpawnError::Refused),
            Ok(None) | Err(_) => Err(SpawnError::Timeout),
        }
    }

    /// Send `Kick` to `world`'s host once its link is up (waiting at most
    /// [`SPAWN_ACK_TIMEOUT_MS`]). Whether it was handed to the link; a world without a host
    /// link has nothing to kick (`false`).
    pub async fn kick(&self, world: &WorldRow, player_token: &str, reason: &str) -> bool {
        let link = match self.link(world) {
            Ok(Some(link)) => link,
            Ok(None) => return false,
            Err(e) => {
                log::warn!("[login] kick in world {}: {e}", world.world_id);
                return false;
            }
        };
        let mut up = link.up.clone();
        let sent = tokio::time::timeout(self.ack_timeout, up.wait_for(|u| *u)).await.is_ok_and(|r| r.is_ok())
            && link
                .tx
                .send(LoginToHost::Kick { player_token: player_token.to_string(), reason: reason.to_string() })
                .is_ok();
        if !sent {
            log::warn!("[login] kick in world {}: its host link is down; not sent", world.world_id);
        }
        sent
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
}

impl Pump {
    fn run(self) {
        for event in self.events.iter() {
            match event {
                LinkEvent::Up(_) => {
                    log::info!("[login] host link for world {} up", self.world);
                    self.up.send_replace(true);
                }
                LinkEvent::Down(why) => {
                    if *self.up.borrow() {
                        log::warn!("[login] host link for world {} down: {why}", self.world);
                    }
                    self.up.send_replace(false);
                }
                LinkEvent::Msg(HostToLogin::SpawnAck { player_token, session_epoch, ok }) => {
                    let waiter = self.waiters.lock().unwrap_or_else(|p| p.into_inner()).remove(&(player_token, session_epoch));
                    match waiter {
                        Some(waiter) => {
                            let _ = waiter.send(ok);
                        }
                        // Answered already (a resend's second ack) or given up on (503).
                        None => log::debug!("[login] world {}: ack for epoch {session_epoch} nobody waits for", self.world),
                    }
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
    }
}
