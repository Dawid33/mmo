//! The login service's side of the worlds' coordinator API (HTTPS, HTTP/2; the coordinator's
//! `coordinator_api.rs`, or a role-`all` server's own login routes): [`WorldClients`] keeps, per
//! world whose registry row names a `host_link`, ONE pinned client and ONE standing poll of the
//! world's answers.
//!
//! - `host_link` is a URL or a comma list of URLs (`https://coordinator.<ns>.svc.cluster.local:6480`;
//!   a dev world with several coordinator replicas lists each); `host_link_cert` is the
//!   coordinator's pin (`svc_link::pinned_http2_client`). A failed request moves to the next URL.
//! - **Orders**: a join POSTs [`LoginToHost::SpawnPlayer`] to [`PATH_COORD_LOGIN_ORDERS`] — 202
//!   once its row is committed in the gate host's mailbox — and waits for the
//!   [`HostToLogin::SpawnAck`] matched by `(player_token, session_epoch)`, all within
//!   [`SPAWN_ACK_TIMEOUT_MS`]. A 503 [`COORD_REFUSED_STARTING`] ends it at once with
//!   [`SpawnError::Starting`] (503 `world_starting`), a 503 [`COORD_REFUSED_NO_GATE`] with
//!   [`SpawnError::NoGate`] (503); a transport failure is tried once more on the next URL. Epochs
//!   only grow, so a late ack for an epoch that already answered 503 never completes a later join.
//! - **Answers**: one task per world long-polls [`PATH_COORD_LOGIN_ANSWERS`] from a cursor kept in
//!   memory, starting at 0 when the world's client is made (first use, or the kick sweeper for a
//!   world with pending kicks): the coordinator keeps its answers (`login_outbox`) for ten
//!   minutes, and every login pod reads all of them — an answer may come back through another
//!   replica, and another login pod may have sent the order. A restarted pod reads them all
//!   again, so every kind is idempotent here: a `SpawnAck` nobody waits for is ignored, a
//!   [`HostToLogin::KickAck`] deletes the pending kicks up to its epoch (deleted already: nothing),
//!   a [`HostToLogin::SessionEnded`] ends the session of exactly that epoch (ended already:
//!   nothing; a newer login has a newer epoch). The poll never ends while the world's client
//!   lives: a failure waits [`ANSWERS_RETRY_MIN_MS`] … [`ANSWERS_RETRY_MAX_MS`] (doubling) and
//!   tries the next URL.
//!
//! Delivery is at-least-once: the hosts treat a repeated epoch as the same order (re-ack,
//! nothing parked again).
//!
//! **Kicks are durable** (revocation must not fail open): a logout-everywhere queues them in
//! `pending_kicks` in the revocation's own transaction
//! ([`LoginStore::revoke_account_with_kicks`]), and the kick sweeper ([`WorldClients::start`])
//! POSTs every pending kick every [`KICK_RESEND_MS`] (and when woken) until a
//! [`HostToLogin::KickAck`] deletes it. A kick revokes up to an epoch, so a resend is harmless
//! (the host applies it once, and never to a newer login). The sweeper also makes the clients of
//! worlds with pending kicks, so kicks queued by the admin ctl (another process) or before a
//! restart go out and their acks are read.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use login_api::{
    COORD_REFUSED_NO_GATE, COORD_REFUSED_STARTING, HEADER_COORD_LINK_VERSION, HEADER_COORD_WORLD, HostToLogin, LoginPage,
    LoginToHost, PATH_COORD_LOGIN_ANSWERS, PATH_COORD_LOGIN_ORDERS, SPAWN_ACK_TIMEOUT_MS,
};
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use tokio::sync::{Notify, oneshot};

use crate::metrics::Metrics;
use crate::{LoginStore, WorldRow};

/// How often pending kicks are posted again (and new ones looked for).
pub const KICK_RESEND_MS: u64 = 2000;
/// A kick the host never acknowledges is dropped (with a warning) after this long.
pub const PENDING_KICK_TTL_SECS: u64 = 86_400;
/// The wait an answers poll asks for (the coordinator caps it at its own `MAILBOX_WAIT_MS`).
pub const ANSWERS_WAIT_MS: u64 = 20_000;
/// One answers poll may take this much longer than its wait before it counts as failed.
pub const ANSWERS_POLL_SLACK_MS: u64 = 10_000;
/// After a failed answers poll the next waits this long, doubling up to [`ANSWERS_RETRY_MAX_MS`].
pub const ANSWERS_RETRY_MIN_MS: u64 = 250;
pub const ANSWERS_RETRY_MAX_MS: u64 = 5000;
/// One kick's POST may take this long.
pub const KICK_POST_TIMEOUT_MS: u64 = SPAWN_ACK_TIMEOUT_MS;

/// Why a spawn order was not acknowledged (the join answers 503).
#[derive(Debug, PartialEq, Eq)]
pub enum SpawnError {
    /// This login service has no service token (`VOXEL_SERVICE_TOKEN[_FILE]`).
    NoServiceToken,
    /// The registry row's `host_link` / `host_link_cert` is unusable.
    BadLink(String),
    /// The host answered `ok: false` (it could not take the order).
    Refused,
    /// No ack within [`SPAWN_ACK_TIMEOUT_MS`] (the coordinator unreachable or refusing, or the
    /// host slow).
    Timeout,
    /// No host of the world takes logins (503 `no_gate`): the join answers 503.
    NoGate,
    /// No host takes logins and the world is starting one (Elastic 4; 503 `world_starting`): the
    /// join answers 503 `world_starting` and the client repeats it.
    Starting,
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::NoServiceToken => write!(f, "no service token (set VOXEL_SERVICE_TOKEN or VOXEL_SERVICE_TOKEN_FILE)"),
            SpawnError::BadLink(why) => write!(f, "bad host link: {why}"),
            SpawnError::Refused => write!(f, "the host refused the spawn order"),
            SpawnError::Timeout => write!(f, "no ack within {SPAWN_ACK_TIMEOUT_MS} ms"),
            SpawnError::NoGate => write!(f, "no host of the world takes logins"),
            SpawnError::Starting => write!(f, "the world is starting a host (no host takes logins)"),
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

/// The URLs of a `host_link` (a URL or a comma list of them), in order; each must be `https://`.
pub fn link_urls(link: &str) -> Result<Vec<String>, String> {
    let urls: Vec<String> =
        link.split(',').map(str::trim).filter(|u| !u.is_empty()).map(|u| u.trim_end_matches('/').to_string()).collect();
    if urls.is_empty() {
        return Err("names no URL".into());
    }
    for url in &urls {
        let Some(rest) = url.strip_prefix("https://") else {
            return Err(format!(
                "{url:?} is not a URL: the link is the world coordinator's HTTPS API, e.g. https://coordinator.<ns>.svc.cluster.local:6480 \
                 (a comma list for several replicas)"
            ));
        };
        if rest.is_empty() || rest.contains('/') {
            return Err(format!("{url:?}: expected https://host:port, without a path"));
        }
    }
    Ok(urls)
}

/// Why one request failed.
enum Failure {
    /// It did not complete (connect, TLS, timeout): the next URL is tried.
    Transport(String),
    /// A non-success answer (status, the start of its body).
    Status(StatusCode, String),
    /// A body that does not decode.
    Body(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Transport(e) => write!(f, "{e}"),
            Failure::Status(s, body) if matches!(*s, StatusCode::UNAUTHORIZED | StatusCode::PRECONDITION_FAILED) => write!(
                f,
                "{s} {body}: the coordinator refused this login service's configuration (the service token, the world id, or \
                 the build)"
            ),
            Failure::Status(s, body) => write!(f, "{s} {body}"),
            Failure::Body(e) => write!(f, "undecodable answer: {e}"),
        }
    }
}

/// What a join waits for.
enum Answer {
    Ack(bool, SpawnPlace),
}

/// One world's client: its URLs, its pinned client, the joins waiting for answers.
struct WorldClient {
    world: String,
    /// The row's `host_link` and `host_link_cert` it was made for.
    target: String,
    cert: String,
    urls: Vec<String>,
    /// The URL in use (a failed request moves to the next).
    url: AtomicUsize,
    client: reqwest::Client,
    token: String,
    /// The joins waiting for an answer, by `(player_token, session_epoch)`.
    waiters: Mutex<BTreeMap<(String, u64), oneshot::Sender<Answer>>>,
    /// Each URL's last request: whether it was answered.
    states: Mutex<BTreeMap<String, bool>>,
}

impl WorldClient {
    fn url(&self) -> String {
        self.urls[self.url.load(Ordering::SeqCst) % self.urls.len()].clone()
    }

    /// `used` failed: the next URL, unless another request moved on already.
    fn failed(&self, used: &str) {
        let at = self.url.load(Ordering::SeqCst);
        if self.urls[at % self.urls.len()] == used {
            let _ = self.url.compare_exchange(at, (at + 1) % self.urls.len(), Ordering::SeqCst, Ordering::SeqCst);
        }
    }

    fn note(&self, url: &str, answered: bool) {
        self.states.lock().unwrap_or_else(|p| p.into_inner()).insert(url.to_string(), answered);
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", self.token)) {
            h.insert(AUTHORIZATION, v);
        }
        if let Ok(v) = HeaderValue::from_str(&self.world) {
            h.insert(HEADER_COORD_WORLD, v);
        }
        h.insert(HEADER_COORD_LINK_VERSION, HeaderValue::from(svc_link::LINK_PROTOCOL_VERSION));
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
        h
    }

    /// POST `order` to `url` within `timeout`.
    async fn post_at(&self, url: &str, order: &LoginToHost, timeout: Duration) -> Result<(), Failure> {
        let body = game::encode_packet(order).map_err(|e| Failure::Body(format!("encode: {e}")))?;
        let resp = self
            .client
            .post(format!("{url}{PATH_COORD_LOGIN_ORDERS}"))
            .headers(self.headers())
            .timeout(timeout)
            .body(body)
            .send()
            .await
            .map_err(|e| Failure::Transport(svc_link::error_chain(&e)))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| Failure::Transport(svc_link::error_chain(&e)))?;
        if status == StatusCode::ACCEPTED {
            return Ok(());
        }
        Err(Failure::Status(status, String::from_utf8_lossy(&bytes).chars().take(64).collect()))
    }

    /// POST `order` within `budget`: on a transport failure, or a 503 of a replica that does not
    /// serve (`not_ready`, `fenced`), once more on the next URL.
    async fn post(&self, order: &LoginToHost, budget: Duration) -> Result<(), Failure> {
        let deadline = tokio::time::Instant::now() + budget;
        let mut tries = 0;
        loop {
            tries += 1;
            let url = self.url();
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let result = self.post_at(&url, order, left).await;
            let replica_only = match &result {
                Ok(()) => false,
                Err(Failure::Transport(_)) => true,
                Err(Failure::Status(s, body)) => *s == StatusCode::SERVICE_UNAVAILABLE && (body == "not_ready" || body == "fenced"),
                Err(_) => false,
            };
            self.note(&url, !matches!(result, Err(Failure::Transport(_))));
            if replica_only {
                self.failed(&url);
            }
            if !replica_only || tries >= 2 || tokio::time::Instant::now() >= deadline {
                return result;
            }
        }
    }

    /// GET one page of answers past `after` from `url`.
    async fn answers(&self, url: &str, after: u64) -> Result<LoginPage, Failure> {
        let resp = self
            .client
            .get(format!("{url}{PATH_COORD_LOGIN_ANSWERS}?after={after}&wait_ms={ANSWERS_WAIT_MS}"))
            .headers(self.headers())
            .timeout(Duration::from_millis(ANSWERS_WAIT_MS + ANSWERS_POLL_SLACK_MS))
            .send()
            .await
            .map_err(|e| Failure::Transport(svc_link::error_chain(&e)))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| Failure::Transport(svc_link::error_chain(&e)))?;
        if status != StatusCode::OK {
            return Err(Failure::Status(status, String::from_utf8_lossy(&bytes).chars().take(64).collect()));
        }
        game::decode_packet::<LoginPage>(&bytes).map_err(|e| Failure::Body(e.to_string()))
    }
}

/// What a world's answers poll needs besides its client.
#[derive(Clone)]
struct PollCtx {
    store: LoginStore,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    metrics: Metrics,
}

/// The standing poll of `world`'s answers, from cursor 0, for as long as its client lives (the
/// map's entry, or a join holding it).
async fn poll_answers(world: Weak<WorldClient>, ctx: PollCtx) {
    let mut after = 0u64;
    let mut backoff = ANSWERS_RETRY_MIN_MS;
    let mut up: Option<bool> = None;
    loop {
        let Some(w) = world.upgrade() else { return };
        let url = w.url();
        let page = w.answers(&url, after).await;
        let id = w.world.clone();
        match page {
            Ok(page) => {
                backoff = ANSWERS_RETRY_MIN_MS;
                w.note(&url, true);
                if up != Some(true) {
                    log::info!("[login] world {id}: reading its answers at {url}");
                    ctx.metrics.host_link(&id, true);
                    up = Some(true);
                }
                // The page's last row is the cursor (rows come in order; a cursor past every row
                // the coordinator kept — a re-forged world — is answered from the start).
                if let Some((last, _)) = page.rows.last() {
                    after = *last;
                }
                for (_, msg) in page.rows {
                    handle(&w, &ctx, msg).await;
                }
            }
            Err(f) => {
                if matches!(f, Failure::Transport(_)) {
                    w.failed(&url);
                }
                w.note(&url, !matches!(f, Failure::Transport(_)));
                if up != Some(false) {
                    log::warn!("[login] world {id}: its answers cannot be read at {url}: {f} (retrying)");
                    ctx.metrics.host_link(&id, false);
                    up = Some(false);
                }
                drop(w);
                tokio::time::sleep(Duration::from_millis(backoff)).await;
                backoff = (backoff * 2).min(ANSWERS_RETRY_MAX_MS);
            }
        }
    }
}

/// One answer of `w`'s world: an ack to its join, a kick ack or a session end into the store.
async fn handle(w: &WorldClient, ctx: &PollCtx, msg: HostToLogin) {
    let world = &w.world;
    match msg {
        HostToLogin::SpawnAck { player_token, session_epoch, ok, quic, webtransport, cert_hash } => {
            let waiter = w.waiters.lock().unwrap_or_else(|p| p.into_inner()).remove(&(player_token, session_epoch));
            match waiter {
                Some(tx) => {
                    let _ = tx.send(Answer::Ack(ok, SpawnPlace { quic, webtransport, cert_hash }));
                }
                // Answered already (a resend's second ack), given up on (503), another login pod's
                // join, or read again from the start.
                None => log::debug!("[login] world {world}: ack for epoch {session_epoch} nobody here waits for"),
            }
        }
        HostToLogin::KickAck { player_token, session_epoch } => match ctx.store.kick_acked(world, &player_token, session_epoch).await {
            Ok(true) => log::info!("[login] world {world}: kick (epoch {session_epoch}) acknowledged"),
            Ok(false) => {}
            Err(e) => log::error!("[login] world {world}: recording a kick ack: {e}"),
        },
        HostToLogin::SessionEnded { player_token, session_epoch } => {
            let now = i64::try_from((ctx.clock)()).unwrap_or(i64::MAX);
            if let Err(e) = ctx.store.end_session(world, &player_token, session_epoch, now).await {
                log::error!("[login] world {world}: recording a session end: {e}");
            }
        }
        // Unsent since link protocol 12 (the API answers 503 instead).
        HostToLogin::NoGate { session_epoch, .. } | HostToLogin::Starting { session_epoch, .. } => {
            log::debug!("[login] world {world}: an old-style refusal for epoch {session_epoch}: ignored")
        }
    }
}

/// The clients of the worlds' coordinator APIs, by world id. Shared by every handler.
pub struct WorldClients {
    token: Option<String>,
    store: LoginStore,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ack_timeout: Duration,
    worlds: Mutex<BTreeMap<String, Arc<WorldClient>>>,
    /// Wakes the kick sweeper: kicks were queued.
    kicks: Arc<Notify>,
    /// Per world, why its kicks cannot be sent (as last logged): the sweeper warns once per
    /// change, not every round.
    kick_blocked: Mutex<BTreeMap<String, String>>,
    metrics: Metrics,
}

impl WorldClients {
    /// Clients that call with the service `token` (`None`: every world with a `host_link` is
    /// unavailable). `store` records kick acks and session ends; `clock` is seconds since the
    /// Unix epoch.
    pub fn new(token: Option<String>, store: LoginStore, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> WorldClients {
        WorldClients {
            token,
            store,
            clock,
            ack_timeout: Duration::from_millis(SPAWN_ACK_TIMEOUT_MS),
            worlds: Mutex::default(),
            kicks: Arc::new(Notify::new()),
            kick_blocked: Mutex::default(),
            metrics: Metrics::new(),
        }
    }

    /// Report the worlds' answer polls and pending kicks to `metrics` (before any client is made).
    pub fn with_metrics(mut self, metrics: Metrics) -> WorldClients {
        self.metrics = metrics;
        self
    }

    /// Wait at most `timeout` for an ack. Tests.
    pub fn with_ack_timeout(mut self, timeout: Duration) -> WorldClients {
        self.ack_timeout = timeout;
        self
    }

    /// Start the kick sweeper on the current runtime (it ends when these clients are dropped).
    /// Without a runtime (or a service token) there is nothing to start.
    pub fn start(self: &Arc<Self>) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
        if self.token.is_none() {
            return;
        }
        let (clients, wake) = (Arc::downgrade(self), self.kicks.clone());
        runtime.spawn(async move {
            loop {
                let Some(clients) = clients.upgrade() else { return };
                clients.send_pending_kicks().await;
                drop(clients);
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

    /// Every pending kick POSTed to its world's coordinator (its client made if need be: its
    /// answers poll reads the `KickAck`). A kick stays pending until its ack, whatever the POST
    /// answered.
    async fn send_pending_kicks(&self) {
        let now = (self.clock)();
        match self.store.expire_pending_kicks(now, PENDING_KICK_TTL_SECS).await {
            // Unacknowledged for at least the TTL (the sweep runs every `KICK_RESEND_MS`).
            Ok(expired) => {
                for kick in expired {
                    // Never the player token in a log: the world and the kick's epoch name it.
                    log::warn!(
                        "[login] kick for world {} (epoch {}) dropped after at least {} s unacknowledged",
                        kick.world_id,
                        kick.session_epoch,
                        PENDING_KICK_TTL_SECS
                    );
                }
            }
            Err(e) => log::error!("[login] expiring pending kicks: {e}"),
        }
        let kicks = match self.store.pending_kicks().await {
            Ok(kicks) => kicks,
            Err(e) => return log::error!("[login] reading pending kicks: {e}"),
        };
        self.metrics.pending_kicks(kicks.len() as u64);
        let mut world: Option<(String, Option<Arc<WorldClient>>)> = None;
        for kick in kicks {
            if world.as_ref().is_none_or(|(id, _)| *id != kick.world_id) {
                let client = match self.store.world(&kick.world_id).await {
                    Ok(Some(row)) => match self.world_client(&row) {
                        Ok(client) => client,
                        Err(e) => {
                            self.kick_blocked(&kick.world_id, Some(e.to_string()));
                            None
                        }
                    },
                    // A removed world: its kicks wait (and go if it is registered again).
                    Ok(None) => None,
                    Err(e) => return log::error!("[login] reading world {}: {e}", kick.world_id),
                };
                world = Some((kick.world_id.clone(), client));
            }
            let Some((_, Some(client))) = &world else { continue };
            let order = LoginToHost::Kick {
                player_token: kick.player_token,
                session_epoch: kick.session_epoch,
                reason: kick.reason,
            };
            match client.post(&order, Duration::from_millis(KICK_POST_TIMEOUT_MS)).await {
                Ok(()) => self.kick_blocked(&kick.world_id, None),
                Err(f) => self.kick_blocked(&kick.world_id, Some(f.to_string())),
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

    /// `world`'s client, made (its answers poll started) on first use or when the row's
    /// `host_link` / `host_link_cert` changed. `Ok(None)`: the world has no host link (R2a:
    /// nothing is pushed).
    fn world_client(&self, world: &WorldRow) -> Result<Option<Arc<WorldClient>>, SpawnError> {
        let Some(target) = &world.host_link else {
            // The row lost its link: the old client goes (its poll ends) and the world reads down.
            if self.worlds.lock().unwrap_or_else(|p| p.into_inner()).remove(&world.world_id).is_some() {
                self.metrics.host_link(&world.world_id, false);
            }
            return Ok(None);
        };
        let token = self.token.clone().ok_or(SpawnError::NoServiceToken)?;
        let cert = world.host_link_cert.clone().ok_or_else(|| SpawnError::BadLink("host_link_cert is not set".into()))?;
        let mut worlds = self.worlds.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(w) = worlds.get(&world.world_id).filter(|w| w.target == *target && w.cert == cert) {
            return Ok(Some(w.clone()));
        }
        let urls = link_urls(target).map_err(|e| SpawnError::BadLink(format!("host_link {target:?}: {e}")))?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| SpawnError::BadLink("world clients are used from the async runtime only".into()))?;
        let client = svc_link::pinned_http2_client(&cert).map_err(|e| SpawnError::BadLink(format!("the HTTPS client: {e}")))?;
        let w = Arc::new(WorldClient {
            world: world.world_id.clone(),
            target: target.clone(),
            cert,
            urls,
            url: AtomicUsize::new(0),
            client,
            token,
            waiters: Mutex::default(),
            states: Mutex::default(),
        });
        log::info!("[login] world {}: its coordinator is {}", world.world_id, w.urls.join(", "));
        // Not answered yet: the world's series follows its poll from now on.
        self.metrics.host_link(&world.world_id, false);
        let ctx = PollCtx { store: self.store.clone(), clock: self.clock.clone(), metrics: self.metrics.clone() };
        runtime.spawn(poll_answers(Arc::downgrade(&w), ctx));
        worlds.insert(world.world_id.clone(), w.clone());
        Ok(Some(w))
    }

    /// Each URL `world`'s row names, with whether its last request was answered (its client made
    /// as a join makes it), in the row's order. Tests and diagnostics.
    pub async fn link_states(&self, world: &WorldRow) -> Result<Vec<(String, bool)>, SpawnError> {
        let Some(w) = self.world_client(world)? else { return Ok(Vec::new()) };
        let states = w.states.lock().unwrap_or_else(|p| p.into_inner());
        Ok(w.urls.iter().map(|u| (u.clone(), states.get(u).copied().unwrap_or(false))).collect())
    }

    /// POST `SpawnPlayer` to `world`'s coordinator and wait (at most [`SPAWN_ACK_TIMEOUT_MS`]
    /// in all) for its ack among the world's answers: where the client connects, as the ack named
    /// it. `Ok` with no address at once for a world without a host link;
    /// [`SpawnError::Starting`] / [`SpawnError::NoGate`] at once when the coordinator says so.
    pub async fn spawn(&self, world: &WorldRow, player_token: &str, session_epoch: u64) -> Result<SpawnPlace, SpawnError> {
        let Some(w) = self.world_client(world)? else { return Ok(SpawnPlace::default()) };
        let key = (player_token.to_string(), session_epoch);
        let (tx, rx) = oneshot::channel();
        // Before the POST: the ack may be read before the POST's answer arrives.
        w.waiters.lock().unwrap_or_else(|p| p.into_inner()).insert(key.clone(), tx);
        let order = LoginToHost::SpawnPlayer { player_token: player_token.to_string(), session_epoch };
        let answer = tokio::time::timeout(self.ack_timeout, async {
            match w.post(&order, self.ack_timeout).await {
                Ok(()) => rx.await.map_err(|_| SpawnError::Timeout),
                Err(Failure::Status(StatusCode::SERVICE_UNAVAILABLE, body)) if body == COORD_REFUSED_STARTING => {
                    Err(SpawnError::Starting)
                }
                Err(Failure::Status(StatusCode::SERVICE_UNAVAILABLE, body)) if body == COORD_REFUSED_NO_GATE => Err(SpawnError::NoGate),
                Err(f) => {
                    log::warn!("[login] world {}: a spawn order (epoch {session_epoch}) was not taken: {f}", world.world_id);
                    Err(SpawnError::Timeout)
                }
            }
        })
        .await;
        w.waiters.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
        match answer {
            Ok(Ok(Answer::Ack(true, place))) => Ok(place),
            Ok(Ok(Answer::Ack(false, _))) => Err(SpawnError::Refused),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(SpawnError::Timeout),
        }
    }
}
