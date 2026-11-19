//! The host's end of its link to the world coordinator over the coordinator's HTTP API
//! (`coordinator_api.rs`): an implementation of [`game::WorldLink`], so the host's
//! `WorldManager` sees the same link as over the QUIC service link.
//!
//! One thread with a current-thread tokio runtime (the networking edge) runs two loops; the
//! main loop talks to it over channels that never block it:
//!
//! - **The send loop** drains the outgoing messages into one [`HostBatch`] (at most
//!   [`LINK_BATCH_MAX`]) and POSTs it — ONE POST in flight, under [`LINK_POST_TIMEOUT_MS`].
//!   Each `HostMsg::Attach` passing through sets the `(boot, epoch)` stamped on its batch and
//!   every batch after it; an `Attach` is always the first message of its batch. With nothing
//!   to send for [`LINK_IDLE_POST_MS`] it posts an empty batch (how a detached host learns it).
//! - **The mailbox loop** long-polls `GET …/mailbox` from a cursor kept for the process's life.
//!   A row reaches [`WorldLink::recv`] only from the row `CoordMsg::Attached { epoch }` of the
//!   link's current `Attach` on; every row before it is acknowledged (the cursor moves past it)
//!   and never delivered — a row decided for an older attach never reaches the manager. Each
//!   row is delivered once, in `seq` order.
//!
//! **Down / up.** The link starts down. Down, it drops what it is given and probes with empty
//! batches (stamped with the last `Attach`'s pair, `(0, 0)` before any) under the retry backoff
//! [`svc_link::LINK_RETRY_MIN_MS`] … [`svc_link::LINK_RETRY_MAX_MS`]. A probe answered 200 —
//! attached or not: after a drop the coordinator may have detached the host — makes it **up**:
//! a new [`WorldLink::generation`], and nothing but an `Attach` is carried until the manager
//! sends its new one (it re-attaches on every new generation). Once that `Attach`'s batch is
//! answered `attached: true` the link is **steady**. A failed POST, a non-200, an `Attach` batch
//! answered `attached: false`, or `attached: false` while steady → **down**: what is queued is
//! dropped (the link is not a queue — the manager's outbox and its next `Attach` cover it) and
//! the mailbox gate closes until the next `Attach`'s `Attached` row.
//!
//! Several URLs (`VOXEL_COORDINATOR_ADDR` as a comma list): a failed request moves both loops to
//! the next one. Trust is the coordinator's pinned certificate (`svc_link::pinned_client_config`);
//! the pin is read again at each client (re)build. 401 / 412 are configuration errors: logged as
//! such, the link stays down and keeps retrying. The service token is never logged.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, Sender};
use game::coordinator::{BatchAck, CoordMsg, HostBatch, HostMsg, MailboxPage};
use game::{HostId, WorldLink};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use tokio::sync::{mpsc, oneshot, Notify};

use crate::coordinator_api::{HEADER_LINK_VERSION, HEADER_WORLD, MAILBOX_WAIT_MS, PATH_HOST_MAILBOX, PATH_HOST_MESSAGES};

/// The longest one POST of a batch may take.
pub const LINK_POST_TIMEOUT_MS: u64 = 5_000;
/// With nothing to send for this long the link posts an empty batch.
pub const LINK_IDLE_POST_MS: u64 = 2_000;
/// Messages per batch at most.
pub const LINK_BATCH_MAX: usize = 256;
/// The longest one mailbox GET may take: the wait it asks for
/// ([`MAILBOX_WAIT_MS`]) and some room for the answer.
pub const LINK_MAILBOX_TIMEOUT_MS: u64 = MAILBOX_WAIT_MS + 5_000;
/// How often the pin is read again while it cannot be read (the coordinator never started).
const PIN_RETRY_MS: u64 = 500;

/// What the link's thread tells the main loop, in order.
enum Event {
    /// A probe was answered: a new connection generation.
    Up(u64),
    /// The link dropped.
    Down,
    /// A mailbox row past the gate.
    Msg(CoordMsg),
}

/// The host's link to the coordinator over HTTPS ([`module docs`](self)). Its state moves only
/// in [`WorldLink::pump`]; dropping it ends its thread.
pub struct HttpWorldLink {
    /// `(the generation it was sent in, the message)`.
    out: mpsc::UnboundedSender<(u64, HostMsg)>,
    events: Receiver<Event>,
    /// An `Up` / `Down` taken while messages before it were still unread.
    held: Option<Event>,
    inbox: VecDeque<CoordMsg>,
    up: bool,
    generation: u64,
    /// Whether this generation's `Attach` was sent (nothing else goes before it).
    attach_sent: bool,
    wake: Receiver<()>,
    /// Dropped with the link: the thread ends.
    _stop: oneshot::Sender<()>,
}

impl HttpWorldLink {
    /// Link host `host` to the coordinator at `urls` (each `https://host:port`, tried in turn),
    /// pinned to the certificate whose SHA-256 `pin` reads (retried until it answers, and read
    /// again at each client rebuild); every request carries `token`, `world` and the link
    /// protocol version.
    pub fn connect(
        urls: Vec<String>,
        pin: impl Fn() -> io::Result<String> + Send + 'static,
        host: HostId,
        token: String,
        world: String,
    ) -> HttpWorldLink {
        assert!(!urls.is_empty(), "the world link needs at least one coordinator URL");
        let urls: Vec<String> = urls.into_iter().map(|u| u.trim().trim_end_matches('/').to_string()).collect();
        let (out, out_rx) = mpsc::unbounded_channel();
        let (events_tx, events) = crossbeam::channel::unbounded();
        let (wake_tx, wake) = crossbeam::channel::bounded(1);
        let (stop, stop_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("world-link".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        log::error!("[world-link] no runtime: {e}");
                        return;
                    }
                };
                let link = Rc::new(Thread {
                    host,
                    token,
                    world,
                    pin: Box::new(pin),
                    events: events_tx,
                    wake: wake_tx,
                    mailbox_go: Notify::new(),
                    state: RefCell::new(State {
                        urls,
                        url: 0,
                        client: None,
                        pin_said: false,
                        generation: 0,
                        up: false,
                        steady: false,
                        stamp: None,
                        gate_epoch: None,
                        gate_open: false,
                        cursor: 0,
                    }),
                });
                runtime.block_on(async {
                    tokio::select! {
                        _ = stop_rx => {}
                        _ = send_loop(&link, out_rx) => {}
                        _ = mailbox_loop(&link) => {}
                    }
                });
            })
            .expect("spawn the world link thread");
        HttpWorldLink {
            out,
            events,
            held: None,
            inbox: VecDeque::new(),
            up: false,
            generation: 0,
            attach_sent: false,
            wake,
            _stop: stop,
        }
    }

    /// Ready when the link has something (a message, up or down): a host loop that blocks on
    /// its other channels selects on it too. Its messages carry nothing; the manager pumps.
    pub fn wake(&self) -> Receiver<()> {
        self.wake.clone()
    }
}

impl WorldLink for HttpWorldLink {
    fn send(&mut self, msg: HostMsg) {
        if !self.up {
            log::debug!("world link down: {} lost", crate::coordinator_rt::describe_host(&msg));
            return;
        }
        let attach = matches!(msg, HostMsg::Attach { .. });
        if !attach && !self.attach_sent {
            log::debug!("world link not attached yet: {} lost", crate::coordinator_rt::describe_host(&msg));
            return;
        }
        self.attach_sent |= attach;
        let _ = self.out.send((self.generation, msg));
    }

    fn recv(&mut self) -> Option<CoordMsg> {
        self.inbox.pop_front()
    }

    fn is_up(&self) -> bool {
        self.up
    }

    fn generation(&self) -> u64 {
        self.generation
    }

    fn pump(&mut self, _now_ms: u64) {
        loop {
            let event = match self.held.take() {
                Some(event) => event,
                None => match self.events.try_recv() {
                    Ok(event) => event,
                    Err(_) => return,
                },
            };
            match event {
                Event::Msg(msg) => self.inbox.push_back(msg),
                change if !self.inbox.is_empty() => {
                    // Read what came before the change first, as the link stood.
                    self.held = Some(change);
                    return;
                }
                Event::Up(generation) => {
                    self.up = true;
                    self.generation = generation;
                    self.attach_sent = false;
                }
                Event::Down => self.up = false,
            }
        }
    }
}

/// The link thread's fixed parts.
struct Thread {
    host: HostId,
    /// Never logged.
    token: String,
    world: String,
    pin: Box<dyn Fn() -> io::Result<String> + Send>,
    events: Sender<Event>,
    wake: Sender<()>,
    /// Wakes the mailbox loop once there is a boot to poll for.
    mailbox_go: Notify,
    state: RefCell<State>,
}

/// What both loops share (one thread: never borrowed across an `.await`).
struct State {
    urls: Vec<String>,
    /// The URL in use; a failed request moves to the next.
    url: usize,
    /// Built at the first request and after a transport failure (the pin read again).
    client: Option<reqwest::Client>,
    /// The pin's wait was logged.
    pin_said: bool,
    /// The connection generation (each `Up`).
    generation: u64,
    up: bool,
    /// The current generation's `Attach` was answered `attached: true`.
    steady: bool,
    /// `(boot, epoch)` of the last `Attach` sent: every batch's stamp.
    stamp: Option<(u64, u64)>,
    /// The epoch whose `Attached` row opens the gate; `None` while down.
    gate_epoch: Option<u64>,
    gate_open: bool,
    /// The last mailbox row taken (delivered or dropped); kept for the process's life.
    cursor: u64,
}

impl State {
    /// A mailbox page's rows through the cursor and the gate: each row past the cursor moves
    /// it; those from the current attach's `Attached` row on are returned (to deliver), in
    /// order, the others dropped. A row at or below the cursor (a page read twice) is skipped.
    fn take_rows(&mut self, rows: Vec<(u64, CoordMsg)>) -> Vec<CoordMsg> {
        let mut deliver = Vec::new();
        for (seq, msg) in rows {
            if seq <= self.cursor {
                continue;
            }
            self.cursor = seq;
            if !self.gate_open {
                if let CoordMsg::Attached { epoch } = &msg {
                    self.gate_open = self.gate_epoch == Some(*epoch);
                }
            }
            if self.gate_open {
                deliver.push(msg);
            } else {
                log::debug!("[world-link] mailbox row {seq} from before the attach dropped");
            }
        }
        deliver
    }
}

/// Why a request failed.
enum Failure {
    /// No client: the pin cannot be read yet.
    NoPin,
    /// The request did not complete (connect, TLS, timeout): the client is rebuilt.
    Transport(String),
    /// A non-200 answer.
    Status(StatusCode, String),
    /// A 200 whose body does not decode.
    Body(String),
}

impl Failure {
    fn describe(&self, url: &str) -> String {
        match self {
            Failure::NoPin => "the coordinator's certificate pin cannot be read yet".into(),
            Failure::Transport(e) => format!("{url}: {e}"),
            Failure::Status(s, body) if matches!(*s, StatusCode::UNAUTHORIZED | StatusCode::PRECONDITION_FAILED) => {
                format!("{url} refused this host's configuration ({s} {body}): check VOXEL_SERVICE_TOKEN, VOXEL_WORLD_ID and that both run one build")
            }
            Failure::Status(s, body) => format!("{url}: {s} {body}"),
            Failure::Body(e) => format!("{url}: undecodable answer: {e}"),
        }
    }

    fn is_config(&self) -> bool {
        matches!(self, Failure::Status(s, _) if matches!(*s, StatusCode::UNAUTHORIZED | StatusCode::PRECONDITION_FAILED))
    }
}

impl Thread {
    /// The client, built if there is none; `None` while the pin cannot be read.
    fn client(&self) -> Option<reqwest::Client> {
        let mut st = self.state.borrow_mut();
        if let Some(c) = &st.client {
            return Some(c.clone());
        }
        let pin = match (self.pin)() {
            Ok(pin) => pin,
            Err(e) => {
                if !st.pin_said {
                    log::info!("[world-link] waiting for the world coordinator's certificate pin: {e}");
                    st.pin_said = true;
                }
                return None;
            }
        };
        match build_client(&pin) {
            Ok(c) => {
                st.client = Some(c.clone());
                Some(c)
            }
            Err(e) => {
                log::error!("[world-link] the HTTPS client could not be built: {e}");
                None
            }
        }
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", self.token)) {
            h.insert(AUTHORIZATION, v);
        }
        if let Ok(v) = HeaderValue::from_str(&self.world) {
            h.insert(HEADER_WORLD, v);
        }
        h.insert(HEADER_LINK_VERSION, HeaderValue::from(svc_link::LINK_PROTOCOL_VERSION));
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
        h
    }

    fn url(&self) -> String {
        let st = self.state.borrow();
        st.urls[st.url % st.urls.len()].clone()
    }

    /// After a failed request: the next URL; a transport failure also drops the client.
    fn failed(&self, failure: &Failure, url_used: &str) {
        let mut st = self.state.borrow_mut();
        if matches!(failure, Failure::Transport(_)) {
            st.client = None;
        }
        // Only if the other loop has not moved on already.
        if st.urls[st.url % st.urls.len()] == url_used {
            st.url = (st.url + 1) % st.urls.len();
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
        let _ = self.wake.try_send(());
    }

    /// POST one batch at `url`. The answer and the remote address.
    async fn post(&self, url: &str, batch: &HostBatch) -> Result<(BatchAck, Option<std::net::SocketAddr>), Failure> {
        let client = self.client().ok_or(Failure::NoPin)?;
        let path = PATH_HOST_MESSAGES.replace("{host}", &self.host.0.to_string());
        let body = game::encode_packet(batch).map_err(|e| Failure::Body(format!("encode: {e}")))?;
        let resp = client
            .post(format!("{url}{path}"))
            .headers(self.headers())
            .timeout(Duration::from_millis(LINK_POST_TIMEOUT_MS))
            .body(body)
            .send()
            .await
            .map_err(|e| Failure::Transport(error_chain(&e)))?;
        let remote = resp.remote_addr();
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| Failure::Transport(error_chain(&e)))?;
        if status != StatusCode::OK {
            return Err(Failure::Status(status, String::from_utf8_lossy(&bytes).chars().take(64).collect()));
        }
        let ack = game::decode_packet::<BatchAck>(&bytes).map_err(|e| Failure::Body(e.to_string()))?;
        Ok((ack, remote))
    }

    /// GET one mailbox page at `url` for `boot` past `after`.
    async fn mailbox(&self, url: &str, boot: u64, after: u64) -> Result<MailboxPage, Failure> {
        let client = self.client().ok_or(Failure::NoPin)?;
        let path = PATH_HOST_MAILBOX.replace("{host}", &self.host.0.to_string());
        let resp = client
            .get(format!("{url}{path}?boot={boot}&after={after}&wait_ms={MAILBOX_WAIT_MS}"))
            .headers(self.headers())
            .timeout(Duration::from_millis(LINK_MAILBOX_TIMEOUT_MS))
            .send()
            .await
            .map_err(|e| Failure::Transport(error_chain(&e)))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| Failure::Transport(error_chain(&e)))?;
        if status != StatusCode::OK {
            return Err(Failure::Status(status, String::from_utf8_lossy(&bytes).chars().take(64).collect()));
        }
        game::decode_packet::<MailboxPage>(&bytes).map_err(|e| Failure::Body(e.to_string()))
    }

    /// The link goes down (if it was up): the gate closes, the main loop is told.
    fn down(&self, why: &str) {
        let mut st = self.state.borrow_mut();
        st.steady = false;
        st.gate_epoch = None;
        st.gate_open = false;
        if st.up {
            st.up = false;
            drop(st);
            log::warn!("[world-link] down: {why}");
            self.emit(Event::Down);
        }
    }
}

/// The client for `pin`: rustls with the pin verifier (ring), ALPN `h2`, HTTP/2 only, no proxy.
fn build_client(pin: &str) -> io::Result<reqwest::Client> {
    let mut tls = svc_link::pinned_client_config(pin)?;
    tls.alpn_protocols = vec![b"h2".to_vec()];
    reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .http2_prior_knowledge()
        .no_proxy()
        .build()
        .map_err(|e| io::Error::other(error_chain(&e)))
}

/// An error with its sources (reqwest's top-level message alone says little).
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    s
}

/// Logs a failure of a down period at once, then at most every
/// [`svc_link::LINK_DIAL_LOG_EVERY_SECS`].
struct FailureLog {
    last: Option<Instant>,
}

impl FailureLog {
    fn note(&mut self, what: &str, failure: &Failure, url: &str) {
        if matches!(failure, Failure::NoPin) {
            return; // said once by `Thread::client`
        }
        if self.last.is_some_and(|t| t.elapsed() < Duration::from_secs(svc_link::LINK_DIAL_LOG_EVERY_SECS)) {
            return;
        }
        self.last = Some(Instant::now());
        let line = failure.describe(url);
        if failure.is_config() {
            log::error!("[world-link] {what}: {line}");
        } else {
            log::warn!("[world-link] {what}: {line}");
        }
    }

    fn reset(&mut self) {
        self.last = None;
    }
}

fn next_backoff(ms: u64) -> u64 {
    (ms * 2).min(svc_link::LINK_RETRY_MAX_MS)
}

/// Takes the messages of the current generation already queued; ends at the first `Attach` that
/// would not be first (kept in `carry`), at [`LINK_BATCH_MAX`], or when the queue is empty.
fn fill_batch(
    link: &Thread,
    out: &mut mpsc::UnboundedReceiver<(u64, HostMsg)>,
    carry: &mut Option<HostMsg>,
    msgs: &mut Vec<HostMsg>,
) {
    let generation = link.state.borrow().generation;
    while msgs.len() < LINK_BATCH_MAX {
        let msg = match carry.take() {
            Some(m) => m,
            None => match out.try_recv() {
                Ok((g, m)) if g == generation => m,
                Ok(_) => continue, // sent before the main loop saw this generation
                Err(_) => return,
            },
        };
        let attach = matches!(msg, HostMsg::Attach { .. });
        if attach && !msgs.is_empty() {
            *carry = Some(msg);
            return;
        }
        if !attach && msgs.is_empty() && link.state.borrow().stamp.is_none() {
            continue; // nothing to stamp it with (the main loop never sends one)
        }
        msgs.push(msg);
    }
}

/// Drops every message queued (the link is down: not a queue).
fn drain(out: &mut mpsc::UnboundedReceiver<(u64, HostMsg)>, carry: &mut Option<HostMsg>) -> usize {
    let mut n = usize::from(carry.take().is_some());
    while out.try_recv().is_ok() {
        n += 1;
    }
    n
}

async fn send_loop(link: &Thread, mut out: mpsc::UnboundedReceiver<(u64, HostMsg)>) {
    let mut carry: Option<HostMsg> = None;
    let mut backoff = svc_link::LINK_RETRY_MIN_MS;
    let mut fails = FailureLog { last: None };
    loop {
        if !link.state.borrow().up {
            // Probing: nothing queued is carried.
            drain(&mut out, &mut carry);
            let (boot, epoch) = link.state.borrow().stamp.unwrap_or((0, 0));
            let url = link.url();
            match link.post(&url, &HostBatch { boot, epoch, msgs: Vec::new() }).await {
                Ok((_, remote)) => {
                    // Whatever was queued while the probe was out is of the old generation.
                    drain(&mut out, &mut carry);
                    let generation = {
                        let mut st = link.state.borrow_mut();
                        st.generation += 1;
                        st.up = true;
                        st.steady = false;
                        st.generation
                    };
                    let ip = remote.map_or_else(|| "?".to_string(), |a| a.ip().to_string());
                    log::info!("[world-link] up to {url} ({ip})");
                    link.emit(Event::Up(generation));
                    backoff = svc_link::LINK_RETRY_MIN_MS;
                    fails.reset();
                }
                Err(f) => {
                    fails.note("probe failed", &f, &url);
                    link.failed(&f, &url);
                    let wait = if matches!(f, Failure::NoPin) { PIN_RETRY_MS } else { backoff };
                    backoff = next_backoff(backoff);
                    tokio::time::sleep(Duration::from_millis(wait)).await;
                }
            }
            continue;
        }

        // Up: wait for the first message (or the idle period), then take what is queued.
        let mut msgs = Vec::new();
        if carry.is_none() {
            match tokio::time::timeout(Duration::from_millis(LINK_IDLE_POST_MS), out.recv()).await {
                Ok(Some((g, m))) => {
                    if g == link.state.borrow().generation {
                        carry = Some(m);
                    }
                }
                Ok(None) => return,
                Err(_) => {}
            }
        }
        fill_batch(link, &mut out, &mut carry, &mut msgs);
        let attach = match msgs.first() {
            Some(HostMsg::Attach { boot, epoch, .. }) => {
                let mut st = link.state.borrow_mut();
                st.stamp = Some((*boot, *epoch));
                st.gate_epoch = Some(*epoch);
                st.gate_open = false;
                st.steady = false;
                true
            }
            _ => false,
        };
        if attach {
            link.mailbox_go.notify_one();
        }
        let (boot, epoch) = link.state.borrow().stamp.unwrap_or((0, 0));
        let url = link.url();
        match link.post(&url, &HostBatch { boot, epoch, msgs }).await {
            Ok((BatchAck { attached: true }, _)) => {
                if attach {
                    link.state.borrow_mut().steady = true;
                }
            }
            Ok((BatchAck { attached: false }, _)) => {
                let steady = link.state.borrow().steady;
                if attach || steady {
                    let n = drain(&mut out, &mut carry);
                    link.down(&format!("the coordinator does not hold this host's attach {epoch} (boot {boot:#x}); {n} queued dropped"));
                }
                // Before the manager's `Attach`: a probe of the old pair, as expected.
            }
            Err(f) => {
                let n = drain(&mut out, &mut carry);
                link.failed(&f, &url);
                link.down(&format!("{}; {n} queued dropped", f.describe(&url)));
                fails.reset(); // the first probe failure of this down period is logged at once
                tokio::time::sleep(Duration::from_millis(backoff)).await;
                backoff = next_backoff(backoff);
            }
        }
    }
}

async fn mailbox_loop(link: &Thread) {
    let mut backoff = svc_link::LINK_RETRY_MIN_MS;
    let mut fails = FailureLog { last: None };
    loop {
        let Some((boot, _)) = link.state.borrow().stamp else {
            link.mailbox_go.notified().await;
            continue;
        };
        let after = link.state.borrow().cursor;
        let url = link.url();
        match link.mailbox(&url, boot, after).await {
            Ok(page) => {
                backoff = svc_link::LINK_RETRY_MIN_MS;
                fails.reset();
                let deliver = link.state.borrow_mut().take_rows(page.rows);
                for msg in deliver {
                    link.emit(Event::Msg(msg));
                }
            }
            Err(f) => {
                fails.note("mailbox read failed", &f, &url);
                link.failed(&f, &url);
                let wait = if matches!(f, Failure::NoPin) { PIN_RETRY_MS } else { backoff };
                backoff = next_backoff(backoff);
                tokio::time::sleep(Duration::from_millis(wait)).await;
            }
        }
    }
}
