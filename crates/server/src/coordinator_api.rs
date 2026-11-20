//! The coordinator's HTTP API (HTTPS, HTTP/2): what hosts and the login service call instead
//! of the QUIC service link. One listener per coordinator replica; any replica serves any
//! caller. Bodies are `game::encode_packet` / `decode_packet`, `Content-Type:
//! application/octet-stream`.
//!
//! Every request carries `Authorization: Bearer <service token>` (401), `X-World` (412
//! `world`; absent = `""`) and `X-Link-Version` (412 `version`). Routes:
//!
//! | Route | Body in -> out |
//! |---|---|
//! | `POST /v1/hosts/{host}/messages` | [`game::coordinator::HostBatch`] -> 200 [`game::coordinator::BatchAck`] |
//! | `GET /v1/hosts/{host}/mailbox?boot=&after=&wait_ms=` | -> 200 [`game::coordinator::MailboxPage`] |
//! | `POST /v1/login/orders` | `login_api::LoginToHost` -> 202 |
//! | `GET /v1/login/answers?after=&wait_ms=` | -> 200 `login_api::LoginPage` |
//!
//! Batches and login orders go to the coordinator's own thread as an [`ApiCall`] (crossbeam,
//! answered on a oneshot): the core stays threadless, tokio stays at this edge. The two reads
//! never touch the core: they read the store's mailbox directly and long-poll — woken by the
//! replica's `coordinator_row` notice (`row_version`) or every [`MAILBOX_RECHECK_MS`], capped
//! at [`MAILBOX_WAIT_MS`].
//!
//! Refusals: [`ApiRefusal`] -> 503 with the body [`ApiRefusal::body`]; a coordinator thread
//! that dropped the answer -> 503 `not_ready`; an undecodable body -> 400; a `{host}` that is
//! not a host id -> 404; a store failure -> 503 `store`.
//!
//! Like `health.rs` the listener runs on its own thread with its own current-thread tokio
//! runtime. TLS is rustls with the **ring** provider (named explicitly in `svc-link`), ALPN `h2`
//! only, over the coordinator identity stored beside [`ApiConfig::cert_file`]
//! (`<file>.cert` / `.key`; dialers pin its SHA-256).

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use game::coordinator::{BatchAck, CoordMsg, HostBatch, MailboxPage};
use game::{HostId, LoginOrder};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use login_api::{HostToLogin, LoginPage, LoginToHost};
use tokio::sync::{oneshot, watch, Semaphore};
use world_store::StoreHandle;

/// The longest a mailbox or answers poll waits; a client's `wait_ms` is capped at it (and the
/// client asks for exactly this much).
pub const MAILBOX_WAIT_MS: u64 = 20_000;
/// A waiting poll re-reads the store at least this often (the `coordinator_row` notice is the
/// wake, this is the backstop).
pub const MAILBOX_RECHECK_MS: u64 = 1_000;
/// Rows per page.
pub const MAILBOX_PAGE_MAX: u32 = 256;
/// The largest request body accepted.
pub const API_BODY_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Open connections at most (long polls hold one each); one more is dropped at accept.
pub const API_CONNECTIONS_MAX: usize = 1024;
/// A TLS handshake that takes longer is dropped.
pub const API_TLS_HANDSHAKE_MS: u64 = 10_000;
/// HTTP/2 PING every this often on an idle connection, so a vanished peer's poll is released.
pub const API_H2_PING_MS: u64 = 10_000;

/// `POST`: a host's batch of messages; the path's `{host}` is its id.
pub const PATH_HOST_MESSAGES: &str = "/v1/hosts/{host}/messages";
/// `GET`: a host's mailbox page.
pub const PATH_HOST_MAILBOX: &str = "/v1/hosts/{host}/mailbox";
/// The request header carrying the world id.
pub const HEADER_WORLD: &str = login_api::HEADER_COORD_WORLD;
/// The request header carrying `svc_link::LINK_PROTOCOL_VERSION`.
pub const HEADER_LINK_VERSION: &str = login_api::HEADER_COORD_LINK_VERSION;
/// The 412 body of a world mismatch.
pub const REFUSED_WORLD: &str = "world";
/// The 412 body of a protocol version mismatch.
pub const REFUSED_VERSION: &str = "version";

/// What the coordinator's thread is asked to do for a caller; it answers on `done`. A dropped
/// `done` (the thread went away) is a 503.
pub enum ApiCall {
    /// A host's batch (`POST /v1/hosts/{host}/messages`): apply it as inputs, answer when its
    /// transition committed.
    Batch { host: HostId, batch: HostBatch, done: oneshot::Sender<Result<BatchAck, ApiRefusal>> },
    /// A login order (`POST /v1/login/orders`): answer `Ok` once its mailbox row committed for
    /// the gate host.
    LoginOrder { order: LoginOrder, done: oneshot::Sender<Result<(), ApiRefusal>> },
}

/// Why the coordinator's thread refused a call; every one is a 503.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiRefusal {
    /// This replica is not Ready (its `ReplicaJoined` has not committed).
    NotReady,
    /// This replica is fenced.
    Fenced,
    /// No host takes logins.
    NoGate,
    /// No host takes logins and the world is adding one.
    Starting,
}

impl ApiRefusal {
    fn code(self) -> u8 {
        match self {
            ApiRefusal::NotReady => 1,
            ApiRefusal::Fenced => 2,
            ApiRefusal::NoGate => 3,
            ApiRefusal::Starting => 4,
        }
    }

    fn from_code(code: u8) -> Option<ApiRefusal> {
        [ApiRefusal::NotReady, ApiRefusal::Fenced, ApiRefusal::NoGate, ApiRefusal::Starting].into_iter().find(|r| r.code() == code)
    }

    /// The 503 body.
    pub fn body(self) -> &'static str {
        match self {
            ApiRefusal::NotReady => "not_ready",
            ApiRefusal::Fenced => "fenced",
            ApiRefusal::NoGate => login_api::COORD_REFUSED_NO_GATE,
            ApiRefusal::Starting => login_api::COORD_REFUSED_STARTING,
        }
    }
}

/// The listener's settings.
pub struct ApiConfig {
    pub bind: SocketAddr,
    /// `VOXEL_SERVICE_TOKEN`; never logged.
    pub token: String,
    /// `VOXEL_WORLD_ID`.
    pub world: String,
    /// The identity: `<cert_file>.cert` / `.key` (generated when missing), the pin beside it.
    pub cert_file: PathBuf,
}

/// A running listener. Dropping it stops accepting and ends its thread.
pub struct ApiListener {
    addr: SocketAddr,
    pin: String,
    refusal: Arc<AtomicU8>,
    _stop: watch::Sender<bool>,
}

impl ApiListener {
    /// The bound address (port 0 picks one).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The certificate's SHA-256 (hex): what dialers pin.
    pub fn cert_sha256(&self) -> &str {
        &self.pin
    }

    /// `Some(r)`: every authenticated request, on every route, is answered 503 with `r`'s body
    /// (`NotReady` until the replica's `ReplicaJoined` committed, `Fenced` once fenced); `None`:
    /// serving. The listener STARTS as `Some(ApiRefusal::NotReady)`: the runtime calls
    /// `set_refusal(None)` once it is Ready.
    pub fn set_refusal(&self, refusal: Option<ApiRefusal>) {
        self.refusal.store(refusal.map_or(0, ApiRefusal::code), Ordering::SeqCst);
    }
}

/// What every caller must present: the service token and the world id ([`check_auth`]).
pub(crate) struct Auth {
    /// Never logged.
    pub(crate) token: String,
    pub(crate) world: String,
}

struct Shared {
    auth: Auth,
    store: StoreHandle,
    calls: crossbeam::channel::Sender<ApiCall>,
    row_version: watch::Receiver<u64>,
    refusal: Arc<AtomicU8>,
}

/// Binds the listener and serves on its own thread, refusing everything `NotReady` until
/// [`ApiListener::set_refusal`]`(None)`. `calls` reaches the coordinator's thread (sent with
/// `try_send`; a closed or full channel answers 503 `not_ready` — the runtime's is unbounded,
/// so a handler never waits); `row_version` carries
/// the versions of the replica's `coordinator_row` notices — if its sender is dropped, waiting
/// polls degrade to re-reading every [`MAILBOX_RECHECK_MS`].
pub fn serve(
    cfg: ApiConfig,
    store: StoreHandle,
    calls: crossbeam::channel::Sender<ApiCall>,
    row_version: watch::Receiver<u64>,
) -> io::Result<ApiListener> {
    if cfg.token.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "the service token is empty"));
    }
    let (tls, pin) = svc_link::pinned_server_config(&cfg.cert_file, &[b"h2"])?;
    let listener = std::net::TcpListener::bind(cfg.bind)?;
    let addr = listener.local_addr()?;
    let refusal = Arc::new(AtomicU8::new(ApiRefusal::NotReady.code()));
    let auth = Auth { token: cfg.token, world: cfg.world };
    let shared = Arc::new(Shared { auth, store, calls, row_version, refusal: refusal.clone() });
    let stop = serve_tls("coordinator-api", listener, tls, router(shared))?;
    log::info!("[coordinator-api] listening on {addr} (cert sha256 {pin})");
    Ok(ApiListener { addr, pin, refusal, _stop: stop })
}

/// Serves `router` over HTTPS (HTTP/2, `tls`) on `listener` from a thread of its own (named
/// `name`, a current-thread tokio runtime): at most [`API_CONNECTIONS_MAX`] connections, each
/// handshake within [`API_TLS_HANDSHAKE_MS`], an HTTP/2 PING every [`API_H2_PING_MS`]. Dropping
/// the returned sender stops accepting and ends the thread. Shared by the coordinator's API and
/// role `all`'s login routes (`login_link.rs`).
pub(crate) fn serve_tls(
    name: &'static str,
    listener: std::net::TcpListener,
    tls: Arc<tokio_rustls::rustls::ServerConfig>,
    router: Router,
) -> io::Result<watch::Sender<bool>> {
    listener.set_nonblocking(true)?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
    let (stop_tx, mut stop_rx) = watch::channel(false);
    std::thread::Builder::new().name(name.into()).spawn(move || {
        runtime.block_on(async move {
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    log::error!("[{name}] the listener: {e}; not serving");
                    return;
                }
            };
            let slots = Arc::new(Semaphore::new(API_CONNECTIONS_MAX));
            loop {
                let (tcp, peer) = tokio::select! {
                    _ = stop_rx.changed() => return,
                    r = listener.accept() => match r {
                        Ok(a) => a,
                        Err(e) => {
                            log::warn!("[{name}] accept: {e}");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                    },
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    log::warn!("[{name}] refusing {peer}: {API_CONNECTIONS_MAX} connections open");
                    continue;
                };
                let (acceptor, router) = (acceptor.clone(), router.clone());
                tokio::spawn(async move {
                    let _permit = permit;
                    let tls = match tokio::time::timeout(Duration::from_millis(API_TLS_HANDSHAKE_MS), acceptor.accept(tcp)).await {
                        Ok(Ok(t)) => t,
                        Ok(Err(e)) => return log::debug!("[{name}] TLS from {peer}: {e}"),
                        Err(_) => return log::debug!("[{name}] TLS from {peer}: timed out"),
                    };
                    let mut http = hyper::server::conn::http2::Builder::new(TokioExecutor::new());
                    http.timer(TokioTimer::new()).keep_alive_interval(Some(Duration::from_millis(API_H2_PING_MS)));
                    if let Err(e) = http.serve_connection(TokioIo::new(tls), TowerToHyperService::new(router)).await {
                        log::debug!("[{name}] connection from {peer}: {e}");
                    }
                });
            }
        })
    })?;
    Ok(stop_tx)
}

fn router(shared: Arc<Shared>) -> Router {
    let api = Router::new()
        .route(PATH_HOST_MESSAGES, post(host_messages))
        .route(PATH_HOST_MAILBOX, get(host_mailbox))
        .route(login_api::PATH_COORD_LOGIN_ORDERS, post(login_orders))
        .route(login_api::PATH_COORD_LOGIN_ANSWERS, get(login_answers))
        .layer(axum::extract::DefaultBodyLimit::max(API_BODY_MAX_BYTES))
        .layer(middleware::from_fn_with_state(shared.clone(), guard))
        .with_state(shared);
    // The world map's tiles: a route group of its own, merged AFTER the guard layer so it is not
    // behind the service token (players reach it through the `web` proxy; `map_api.rs`).
    match crate::map_api::installed_router() {
        Some(map) => api.merge(map),
        None => api,
    }
}

pub(crate) fn text(status: StatusCode, body: &'static str) -> Response {
    (status, body).into_response()
}

pub(crate) fn packet<T: serde::Serialize>(status: StatusCode, value: &T) -> Response {
    match game::encode_packet(value) {
        Ok(bytes) => (status, [(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response(),
        Err(e) => {
            log::error!("[coordinator-api] a response does not encode: {e}");
            text(StatusCode::INTERNAL_SERVER_ERROR, "encode")
        }
    }
}

fn refusal(r: ApiRefusal) -> Response {
    text(StatusCode::SERVICE_UNAVAILABLE, r.body())
}

/// The auth checks every route of this API (and role `all`'s login routes) makes: token (401),
/// world (412), link version (412). `Err`: the answer.
pub(crate) fn check_auth(auth: &Auth, headers: &axum::http::HeaderMap) -> Result<(), Response> {
    let bearer = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    // Compared without an early exit (an unset header compares as empty and never matches:
    // the token is non-empty).
    if !svc_link::ct_eq(bearer.as_bytes(), auth.token.as_bytes()) {
        return Err(text(StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    let world = headers.get(HEADER_WORLD).and_then(|v| v.to_str().ok()).unwrap_or("");
    if world != auth.world {
        return Err(text(StatusCode::PRECONDITION_FAILED, REFUSED_WORLD));
    }
    let version = headers.get(HEADER_LINK_VERSION).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u32>().ok());
    if version != Some(svc_link::LINK_PROTOCOL_VERSION) {
        return Err(text(StatusCode::PRECONDITION_FAILED, REFUSED_VERSION));
    }
    Ok(())
}

/// The one auth layer: [`check_auth`], then whether this replica serves at all (503). Every
/// answer is counted in `mmo_coordinator_api_requests_total` by [`route_of`] and status.
async fn guard(State(s): State<Arc<Shared>>, req: Request, next: Next) -> Response {
    let route = route_of(req.uri().path());
    let response = if let Err(refused) = check_auth(&s.auth, req.headers()) {
        refused
    } else if let Some(r) = ApiRefusal::from_code(s.refusal.load(Ordering::SeqCst)) {
        refusal(r)
    } else {
        next.run(req).await
    };
    crate::health::Health::global().api_request(route, response.status().as_u16());
    response
}

/// The index in [`crate::health::API_ROUTES`] of a request path: one of the four routes, else
/// `other`. A fixed set (never the path itself: `{host}` would make a label per host).
fn route_of(path: &str) -> usize {
    let host_route = |suffix: &str| {
        path.strip_prefix("/v1/hosts/").and_then(|rest| rest.strip_suffix(suffix)).is_some_and(|id| !id.is_empty() && !id.contains('/'))
    };
    if host_route("/messages") {
        0
    } else if host_route("/mailbox") {
        1
    } else if path == login_api::PATH_COORD_LOGIN_ORDERS {
        2
    } else if path == login_api::PATH_COORD_LOGIN_ANSWERS {
        3
    } else {
        4
    }
}

fn host_id(raw: &str) -> Option<HostId> {
    raw.parse::<u64>().ok().map(HostId)
}

/// Sends `call` to the coordinator's thread; false when it is gone.
fn send_call(s: &Shared, call: ApiCall) -> bool {
    // The runtime's channel is unbounded (a handler never waits to send); `try_send` fails only
    // when the coordinator's thread is gone (503 `not_ready`).
    s.calls.try_send(call).is_ok()
}

async fn host_messages(State(s): State<Arc<Shared>>, Path(host): Path<String>, body: Bytes) -> Response {
    let Some(host) = host_id(&host) else { return text(StatusCode::NOT_FOUND, "host") };
    let Ok(batch) = game::decode_packet::<HostBatch>(&body) else { return text(StatusCode::BAD_REQUEST, "batch") };
    let (done, answer) = oneshot::channel();
    if !send_call(&s, ApiCall::Batch { host, batch, done }) {
        return refusal(ApiRefusal::NotReady);
    }
    match answer.await {
        Ok(Ok(ack)) => packet(StatusCode::OK, &ack),
        Ok(Err(r)) => refusal(r),
        Err(_) => refusal(ApiRefusal::NotReady),
    }
}

async fn login_orders(State(s): State<Arc<Shared>>, body: Bytes) -> Response {
    let Ok(msg) = game::decode_packet::<LoginToHost>(&body) else { return text(StatusCode::BAD_REQUEST, "order") };
    let order = crate::login_link::order_from(msg);
    let (done, answer) = oneshot::channel();
    if !send_call(&s, ApiCall::LoginOrder { order, done }) {
        return refusal(ApiRefusal::NotReady);
    }
    match answer.await {
        Ok(Ok(())) => StatusCode::ACCEPTED.into_response(),
        Ok(Err(r)) => refusal(r),
        Err(_) => refusal(ApiRefusal::NotReady),
    }
}

#[derive(serde::Deserialize)]
struct MailboxQuery {
    boot: u64,
    after: u64,
    wait_ms: Option<u64>,
}

/// The query of `GET` [`login_api::PATH_COORD_LOGIN_ANSWERS`] (role `all`'s too).
#[derive(serde::Deserialize)]
pub(crate) struct AnswersQuery {
    pub(crate) after: u64,
    pub(crate) wait_ms: Option<u64>,
}

/// Reads with `read` until it returns rows or `wait_ms` (capped at [`MAILBOX_WAIT_MS`]) has
/// passed, waking on `versions` or every [`MAILBOX_RECHECK_MS`]. The version is marked seen
/// BEFORE each read, so a notice arriving during it wakes the next wait at once. Shared with role
/// `all`'s login answers (`login_link.rs`).
pub(crate) async fn long_poll<T, E, F, Fut>(versions: &watch::Receiver<u64>, wait_ms: Option<u64>, mut read: F) -> Result<Vec<T>, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Vec<T>, E>>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms.unwrap_or(0).min(MAILBOX_WAIT_MS));
    let mut versions = versions.clone();
    loop {
        versions.borrow_and_update();
        let rows = read().await?;
        let now = tokio::time::Instant::now();
        if !rows.is_empty() || now >= deadline {
            return Ok(rows);
        }
        let nap = (deadline - now).min(Duration::from_millis(MAILBOX_RECHECK_MS));
        if let Ok(Err(_)) = tokio::time::timeout(nap, versions.changed()).await {
            // The sender is gone: `changed()` would now return at once, every time. Degrade
            // to the recheck interval.
            tokio::time::sleep(nap).await;
        }
    }
}

fn store_failed(what: &str, e: world_store::StoreError) -> Response {
    log::warn!("[coordinator-api] {what}: {e}");
    text(StatusCode::SERVICE_UNAVAILABLE, "store")
}

/// Decodes stored rows as `T`; a row that does not decode is a bug (never silently skipped:
/// delivery is exactly once, in order) and answers 500.
fn decode_rows<T: serde::de::DeserializeOwned>(what: &str, rows: Vec<(u64, Vec<u8>)>) -> Result<Vec<(u64, T)>, Response> {
    rows.into_iter()
        .map(|(seq, bytes)| match game::decode_packet::<T>(&bytes) {
            Ok(m) => Ok((seq, m)),
            Err(e) => {
                log::error!("[coordinator-api] {what} row {seq} does not decode: {e}");
                Err(text(StatusCode::INTERNAL_SERVER_ERROR, "row"))
            }
        })
        .collect()
}

async fn host_mailbox(State(s): State<Arc<Shared>>, Path(host): Path<String>, Query(q): Query<MailboxQuery>) -> Response {
    let Some(host) = host_id(&host) else { return text(StatusCode::NOT_FOUND, "host") };
    let rows = long_poll(&s.row_version, q.wait_ms, || s.store.mailbox_read(host, q.boot, q.after, MAILBOX_PAGE_MAX)).await;
    match rows {
        Ok(rows) => match decode_rows::<CoordMsg>("mailbox", rows) {
            Ok(rows) => packet(StatusCode::OK, &MailboxPage { rows }),
            Err(r) => r,
        },
        Err(e) => store_failed("mailbox read", e),
    }
}

/// `login_outbox` rows are `game::encode_packet` of [`HostToLogin`] (written by the
/// coordinator's transitions). A cursor past every row the store ever issued is another
/// incarnation's (a re-forged world): the store answers from the start (`login_answers_read`).
async fn login_answers(State(s): State<Arc<Shared>>, Query(q): Query<AnswersQuery>) -> Response {
    let rows = long_poll(&s.row_version, q.wait_ms, || s.store.login_answers_read(q.after, MAILBOX_PAGE_MAX)).await;
    match rows {
        Ok(rows) => match decode_rows::<HostToLogin>("login answers", rows) {
            Ok(rows) => packet(StatusCode::OK, &LoginPage { rows }),
            Err(r) => r,
        },
        Err(e) => store_failed("login answers read", e),
    }
}
