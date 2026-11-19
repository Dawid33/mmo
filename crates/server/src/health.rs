//! The process's health and metrics over HTTP (`VOXEL_HEALTH_BIND`, unset: no listener at
//! all): `GET /livez`, `GET /readyz` and `GET /metrics` (Prometheus text) for Kubernetes
//! probes and scraping.
//!
//! The endpoint only READS: every value is an atomic the process's own loops store through
//! [`Health::global`]'s hooks (an atomic store each — no lock, no allocation, no channel, so
//! they are called unconditionally, listener or not). It never touches the store and never
//! beats the lease: a host's liveness toward the world is its lease ([`crate::lease`]); this
//! only tells Kubernetes what the process already knows. "Tokio stays at the networking edge":
//! the listener runs on its own thread with its own current-thread runtime.
//!
//! Every metric carries one label `role` (`host` — role `all` included — or `coordinator`);
//! the others come from fixed sets only.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use prometheus::{Encoder, Gauge, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder};

/// The health listener's address (`ip:port`). Unset or empty: no listener, no thread.
pub const HEALTH_BIND_ENV: &str = "VOXEL_HEALTH_BIND";

/// `/livez` fails once a registered process's loop has not passed for this long: 3 ×
/// [`game::HOST_LEASE_MS`], so the lease (the coordinator declaring the host dead) always acts
/// before Kubernetes restarts the pod.
pub const LIVEZ_STALL_MS: u64 = 30_000;
const _: () = assert!(LIVEZ_STALL_MS == 3 * game::HOST_LEASE_MS);

/// What a process not yet registered waits for ([`Health::set_waiting`]); the index + 1 is
/// stored, 0 = nothing. `forge`: an empty world store under `VOXEL_EMPTY_STORE=wait`;
/// `drained`: a host that left the world after a drain (Elastic 1 Task 8) and waits to be
/// removed ([`Health::set_drained`]). (Elastic 1 removed `succession` and `vouched`: a host no
/// longer waits at its registration — a restarted process is a new host.)
const WAITING: [&str; 2] = ["forge", DRAINED];

/// [`WAITING`]'s word for a drained host: `/readyz`'s body, `mmo_waiting{why="drained"}`.
pub const DRAINED: &str = "drained";

/// How long a host stays Ready after its coordinator attachment lapsed: one host lease
/// (`HOST_LEASE_MS`, 10 s), so a coordinator replica's restart does not unready every host.
pub const ATTACH_GRACE_MS: i64 = 10_000;

/// `last_beat_ms` before the first [`Health::loop_beat`].
const NEVER: i64 = i64::MIN;

/// The process's health, as its loops report it. One per process ([`Health::global`]).
pub struct Health {
    /// What `last_beat_ms` counts from.
    epoch: Instant,
    /// The last loop pass, ms after `epoch` ([`NEVER`]: none yet).
    last_beat_ms: AtomicI64,
    registered: AtomicBool,
    /// A host (not role `all`, not the coordinator): `/readyz` also waits for the attach
    /// ([`Health::require_attach`]).
    requires_attach: AtomicBool,
    /// A coordinator replica has acknowledged the host's latest `Attach`, now.
    attached: AtomicBool,
    /// The last pass that saw it attached, ms after `epoch` ([`NEVER`]: never).
    attached_ms: AtomicI64,
    /// [`WAITING`] index + 1; 0: not waiting.
    waiting: AtomicU8,
    cuts_committed: AtomicU64,
    cuts_aborted: AtomicU64,
    /// Unix seconds of the last committed cut decision; 0: none.
    cut_last_committed_s: AtomicU64,
    world_saved: AtomicU64,
    successions_declared: AtomicU64,
    successions_settled: AtomicU64,
    successions_open: AtomicU64,
    world_generation: AtomicU64,
    hosts_live: AtomicU64,
    load_players: AtomicU64,
    load_regions: AtomicU64,
    load_tick_permille: AtomicU64,
    coordinator_transitions: AtomicU64,
    coordinator_core_bytes: AtomicU64,
    coordinator_history_bytes: AtomicU64,
    coordinator_conflicts: AtomicU64,
    coordinator_replicas_live: AtomicU64,
    coordinator_is_ticker: AtomicBool,
}

impl Health {
    fn new() -> Health {
        Health {
            epoch: Instant::now(),
            last_beat_ms: AtomicI64::new(NEVER),
            registered: AtomicBool::new(false),
            requires_attach: AtomicBool::new(false),
            attached: AtomicBool::new(false),
            attached_ms: AtomicI64::new(NEVER),
            waiting: AtomicU8::new(0),
            cuts_committed: AtomicU64::new(0),
            cuts_aborted: AtomicU64::new(0),
            cut_last_committed_s: AtomicU64::new(0),
            world_saved: AtomicU64::new(0),
            successions_declared: AtomicU64::new(0),
            successions_settled: AtomicU64::new(0),
            successions_open: AtomicU64::new(0),
            world_generation: AtomicU64::new(0),
            hosts_live: AtomicU64::new(0),
            load_players: AtomicU64::new(0),
            load_regions: AtomicU64::new(0),
            load_tick_permille: AtomicU64::new(0),
            coordinator_transitions: AtomicU64::new(0),
            coordinator_core_bytes: AtomicU64::new(0),
            coordinator_history_bytes: AtomicU64::new(0),
            coordinator_conflicts: AtomicU64::new(0),
            coordinator_replicas_live: AtomicU64::new(0),
            coordinator_is_ticker: AtomicBool::new(false),
        }
    }

    /// The process's one instance.
    pub fn global() -> &'static Health {
        static HEALTH: OnceLock<Health> = OnceLock::new();
        HEALTH.get_or_init(Health::new)
    }

    fn now_ms(&self) -> i64 {
        i64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(i64::MAX)
    }

    /// One pass of the process's loop: the host's main loop, the split coordinator's thread.
    pub fn loop_beat(&self) {
        self.last_beat_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    /// A host: `register_host` answered `Registered` (role `all`: its main loop starts); the
    /// coordinator: its store is open and its listener bound. `false`: stopping.
    pub fn set_registered(&self, yes: bool) {
        self.registered.store(yes, Ordering::Relaxed);
    }

    /// A registered host is Ready only once a coordinator replica acknowledged its `Attach`,
    /// and stays Ready for [`ATTACH_GRACE_MS`] after it detaches (Elastic 2b Task 9).
    pub fn require_attach(&self) {
        self.requires_attach.store(true, Ordering::Relaxed);
    }

    /// The host's main loop, each pass: whether its attach is acknowledged now.
    pub fn set_attached(&self, yes: bool) {
        if yes {
            self.attached_ms.store(self.now_ms(), Ordering::Relaxed);
        }
        self.attached.store(yes, Ordering::Relaxed);
    }

    fn ready(&self) -> bool {
        if !self.registered.load(Ordering::Relaxed) {
            return false;
        }
        if !self.requires_attach.load(Ordering::Relaxed) || self.attached.load(Ordering::Relaxed) {
            return true;
        }
        let at = self.attached_ms.load(Ordering::Relaxed);
        at != NEVER && self.now_ms() - at <= ATTACH_GRACE_MS
    }

    /// Why the process is not registered yet: `"forge"` or [`DRAINED`] (anything else is taken
    /// as `None`); `None`: not waiting.
    pub fn set_waiting(&self, why: Option<&'static str>) {
        let code = why.and_then(|w| WAITING.iter().position(|&x| x == w)).map_or(0, |i| i as u8 + 1);
        self.waiting.store(code, Ordering::Relaxed);
    }

    /// A host left the world after a drain and idles until it is removed (Elastic 1 Task 8):
    /// not ready (`/readyz` 503 [`DRAINED`], `mmo_waiting{why="drained"}`, `mmo_registered` 0),
    /// still judged alive by its idle loop's beat ([`Self::loop_beat`]). It beats first: the
    /// leave before it may have outlasted [`LIVEZ_STALL_MS`] (unregistered, nothing judged it),
    /// and being drained must not inherit that loop age for an instant.
    pub fn set_drained(&self) {
        self.loop_beat();
        self.set_registered(false);
        self.set_waiting(Some(DRAINED));
    }

    fn drained(&self) -> bool {
        self.waiting() == Some(DRAINED)
    }

    fn waiting(&self) -> Option<&'static str> {
        match self.waiting.load(Ordering::Relaxed) {
            0 => None,
            n => WAITING.get(usize::from(n) - 1).copied(),
        }
    }

    /// The coordinator recorded a world cut's decision (the decision the STORE holds).
    pub fn cut_decided(&self, committed: bool) {
        if committed {
            self.cuts_committed.fetch_add(1, Ordering::Relaxed);
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            self.cut_last_committed_s.store(now, Ordering::Relaxed);
        } else {
            self.cuts_aborted.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A host: a save of its committed — its part of a world cut, or its own save cut.
    pub fn world_saved(&self) {
        self.world_saved.fetch_add(1, Ordering::Relaxed);
    }

    /// The coordinator declared a succession (the store opened it).
    pub fn succession_declared(&self) {
        self.successions_declared.fetch_add(1, Ordering::Relaxed);
    }

    /// The coordinator recorded the commit of a cut that settles a succession.
    pub fn succession_settled(&self) {
        self.successions_settled.fetch_add(1, Ordering::Relaxed);
    }

    /// The successions open now, as the coordinator knows them (declared, read back at a
    /// start, minus settled and abandoned).
    pub fn successions_open(&self, n: u64) {
        self.successions_open.store(n, Ordering::Relaxed);
    }

    pub fn world_generation(&self, generation: u64) {
        self.world_generation.store(generation, Ordering::Relaxed);
    }

    /// The coordinator's last read of the `hosts` rows: those dirty and live.
    pub fn hosts_live(&self, n: u64) {
        self.hosts_live.store(n, Ordering::Relaxed);
    }

    /// What the last heartbeat sent as this host's load (Elastic 3).
    pub fn host_load(&self, load: game::HostLoad) {
        self.load_players.store(u64::from(load.players), Ordering::Relaxed);
        self.load_regions.store(u64::from(load.regions), Ordering::Relaxed);
        self.load_tick_permille.store(u64::from(load.tick_permille), Ordering::Relaxed);
    }

    /// The coordinator committed a transition: its row's `core` and `history` (0: not written)
    /// bytes (Elastic 2a, decision 8).
    pub fn coordinator_written(&self, core_bytes: u64, history_bytes: u64) {
        self.coordinator_transitions.fetch_add(1, Ordering::Relaxed);
        self.coordinator_core_bytes.fetch_add(core_bytes, Ordering::Relaxed);
        self.coordinator_history_bytes.fetch_add(history_bytes, Ordering::Relaxed);
    }

    /// A coordinator write was refused as a `Conflict` (the row's version moved under its epoch).
    pub fn coordinator_conflict(&self) {
        self.coordinator_conflicts.fetch_add(1, Ordering::Relaxed);
    }

    /// The coordinator replica's view of the live set (Elastic 2b Task 4): how many replicas its
    /// core holds live, and whether it is their ticker.
    pub fn coordinator_replicas(&self, live: u64, ticker: bool) {
        self.coordinator_replicas_live.store(live, Ordering::Relaxed);
        self.coordinator_is_ticker.store(ticker, Ordering::Relaxed);
    }

    /// Milliseconds since the last loop pass; `None` before the first.
    fn loop_age_ms(&self) -> Option<u64> {
        match self.last_beat_ms.load(Ordering::Relaxed) {
            NEVER => None,
            at => Some(u64::try_from(self.now_ms().saturating_sub(at)).unwrap_or(0)),
        }
    }

    /// Whether `/livez` fails: registered (or drained: its idle loop beats), and the loop's last
    /// pass older than [`LIVEZ_STALL_MS`]. Before the first pass (a boot's long steps after its
    /// registration) the lease's boot beats watch it, not this.
    fn stalled(&self) -> bool {
        (self.registered.load(Ordering::Relaxed) || self.drained()) && self.loop_age_ms().is_some_and(|age| age > LIVEZ_STALL_MS)
    }

    /// Test support (`test-hooks`): a `Health` of its own, never the process's — tests run in
    /// parallel, the global one is shared.
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub fn for_test() -> &'static Health {
        Box::leak(Box::new(Health::new()))
    }

    /// The Prometheus text exposition of every metric, labelled `role`. A registry of its own
    /// per scrape: the hooks stay plain atomics, and the role is known only to [`serve`].
    fn render(&self, role: &'static str) -> String {
        let registry = Registry::new();
        let opts = |name: &str, help: &str| Opts::new(name, help).const_label("role", role);
        let register = |c: Box<dyn prometheus::core::Collector>| registry.register(c).expect("metric registered once");
        let load = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let gauge = |name: &str, help: &str, value: u64| {
            let g = IntGauge::with_opts(opts(name, help)).expect("valid metric");
            g.set(i64::try_from(value).unwrap_or(i64::MAX));
            register(Box::new(g));
        };
        let counter = |name: &str, help: &str, value: u64| {
            let c = IntCounter::with_opts(opts(name, help)).expect("valid metric");
            c.inc_by(value);
            register(Box::new(c));
        };

        let registered = u64::from(self.registered.load(Ordering::Relaxed));
        gauge("mmo_registered", "1 once a host registered, or a coordinator listens on its open store.", registered);
        let waiting = IntGaugeVec::new(opts("mmo_waiting", "1 for what the unregistered process waits for."), &["why"])
            .expect("valid metric");
        let now = self.waiting();
        for why in WAITING {
            waiting.with_label_values(&[why]).set(i64::from(now == Some(why)));
        }
        register(Box::new(waiting));
        let age = Gauge::with_opts(opts("mmo_loop_age_seconds", "Seconds since the loop last passed (0 before its first pass)."))
            .expect("valid metric");
        age.set(self.loop_age_ms().unwrap_or(0) as f64 / 1000.0);
        register(Box::new(age));
        let cuts = IntCounterVec::new(
            opts("mmo_cuts_decided_total", "World cut decisions the coordinator recorded, by the stored outcome."),
            &["outcome"],
        )
        .expect("valid metric");
        cuts.with_label_values(&["committed"]).inc_by(load(&self.cuts_committed));
        cuts.with_label_values(&["aborted"]).inc_by(load(&self.cuts_aborted));
        register(Box::new(cuts));
        gauge(
            "mmo_cut_last_committed_timestamp_seconds",
            "Unix time of the last committed world cut decision (0: none yet).",
            load(&self.cut_last_committed_s),
        );
        counter("mmo_world_saved_total", "Saves of this host that committed (world cut parts, own save cuts).", load(&self.world_saved));
        counter("mmo_successions_declared_total", "Successions the coordinator declared.", load(&self.successions_declared));
        counter("mmo_successions_settled_total", "Successions settled by a committed world cut.", load(&self.successions_settled));
        gauge("mmo_successions_open", "Successions open now, as the coordinator knows them.", load(&self.successions_open));
        gauge("mmo_world_generation", "The world generation this process runs.", load(&self.world_generation));
        gauge("mmo_host_load_players", "Sessions this host reported in its last heartbeat.", load(&self.load_players));
        gauge("mmo_host_load_regions", "Regions this host reported running in its last heartbeat.", load(&self.load_regions));
        gauge(
            "mmo_host_load_tick_permille",
            "Worst tick compute time of the heartbeat window over the tick period, x1000, as last reported.",
            load(&self.load_tick_permille),
        );
        gauge("mmo_hosts_live", "Hosts whose `hosts` row is dirty and live at the coordinator's last read.", load(&self.hosts_live));
        counter(
            "mmo_coordinator_transitions_total",
            "Coordinator transitions written (each one compare-and-set of the coordinator row).",
            load(&self.coordinator_transitions),
        );
        let written = IntCounterVec::new(
            opts("mmo_coordinator_written_bytes_total", "Bytes of the coordinator row's blobs written, by part."),
            &["part"],
        )
        .expect("valid metric");
        written.with_label_values(&["core"]).inc_by(load(&self.coordinator_core_bytes));
        written.with_label_values(&["history"]).inc_by(load(&self.coordinator_history_bytes));
        register(Box::new(written));
        counter(
            "mmo_coordinator_conflicts_total",
            "Coordinator writes refused as a conflict (the row's version moved under its epoch).",
            load(&self.coordinator_conflicts),
        );
        gauge(
            "mmo_coordinator_replicas_live",
            "Coordinator replicas live, as this replica's core holds them (Elastic 2b).",
            load(&self.coordinator_replicas_live),
        );
        gauge(
            "mmo_coordinator_is_ticker",
            "1 while this coordinator replica is the ticker (the lowest live replica).",
            u64::from(self.coordinator_is_ticker.load(Ordering::Relaxed)),
        );
        let build = IntGaugeVec::new(
            opts("mmo_build_info", "The running build (always 1)."),
            &["region_blocks", "chunk_blocks", "revision"],
        )
        .expect("valid metric");
        build
            .with_label_values(&[
                game::REGION_BLOCKS.to_string().as_str(),
                game::CHUNK_BLOCKS.to_string().as_str(),
                option_env!("VOXEL_BUILD_REVISION").unwrap_or("dev"),
            ])
            .set(1);
        register(Box::new(build));

        let mut out = Vec::new();
        if let Err(e) = TextEncoder::new().encode(&registry.gather(), &mut out) {
            log::error!("[health] encoding metrics: {e}");
        }
        String::from_utf8(out).unwrap_or_default()
    }
}

/// The health listener's address from the value of [`HEALTH_BIND_ENV`]: unset or empty =
/// `None` (no listener); otherwise an `ip:port` (a host name is refused: a pod binds an
/// address, `0.0.0.0:9100`).
pub fn health_bind_from(value: Option<&str>) -> Result<Option<SocketAddr>, String> {
    let value = value.map(str::trim).unwrap_or("");
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<SocketAddr>()
        .map(Some)
        .map_err(|_| format!("invalid {HEALTH_BIND_ENV} value `{value}` (expected ip:port, e.g. 0.0.0.0:9100)"))
}

/// Starts the endpoint for a process of `role` if [`HEALTH_BIND_ENV`] names an address;
/// unset: nothing (no thread, no socket). A bad value or a bind that fails is an error.
pub fn start_from_env(role: &'static str) -> Result<(), String> {
    let Some(bind) = health_bind_from(std::env::var(HEALTH_BIND_ENV).ok().as_deref())? else { return Ok(()) };
    serve(bind, role).map_err(|e| format!("the health endpoint on {bind} ({HEALTH_BIND_ENV}): {e}"))
}

#[derive(Clone, Copy)]
struct Served {
    health: &'static Health,
    role: &'static str,
}

/// Serves [`Health::global`] on `bind` for a process of `role` (`host` or `coordinator`):
/// bound here (an error is the caller's), served on a thread of its own with a current-thread
/// tokio runtime. Returns once bound.
pub fn serve(bind: SocketAddr, role: &'static str) -> io::Result<()> {
    let listener = std::net::TcpListener::bind(bind)?;
    log::info!("[health] /livez, /readyz and /metrics on {}", listener.local_addr()?);
    serve_listener(listener, role, Health::global())
}

/// Test support (`test-hooks`): serves `health` (not the process's) on `listener`.
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub fn serve_for_test(listener: std::net::TcpListener, role: &'static str, health: &'static Health) -> io::Result<()> {
    serve_listener(listener, role, health)
}

fn serve_listener(listener: std::net::TcpListener, role: &'static str, health: &'static Health) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_io().build()?;
    let served = Served { health, role };
    std::thread::Builder::new().name("health".into()).spawn(move || {
        runtime.block_on(async move {
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    log::error!("[health] the listener: {e}; no health endpoint");
                    return;
                }
            };
            if let Err(e) = axum::serve(listener, router(served)).await {
                log::error!("[health] the endpoint stopped: {e}");
            }
        })
    })?;
    Ok(())
}

fn router(served: Served) -> Router {
    Router::new()
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .with_state(served)
}

async fn livez(State(s): State<Served>) -> impl IntoResponse {
    if s.health.stalled() {
        (StatusCode::SERVICE_UNAVAILABLE, "stalled")
    } else {
        (StatusCode::OK, "ok")
    }
}

async fn readyz(State(s): State<Served>) -> impl IntoResponse {
    if s.health.ready() {
        (StatusCode::OK, "ok")
    } else if s.health.registered.load(Ordering::Relaxed) {
        (StatusCode::SERVICE_UNAVAILABLE, "attaching")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, s.health.waiting().unwrap_or("starting"))
    }
}

async fn metrics(State(s): State<Served>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], s.health.render(s.role))
}

