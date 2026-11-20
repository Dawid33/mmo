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
//! Every metric carries one label `role` (`host` — role `all` included —, `coordinator` or
//! `fleet`, the reconciler of Elastic 4); the others come from fixed sets only.

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
/// removed ([`Health::set_drained`]); `stopped`: the fleet reconciler while the world's
/// coordinator Deployment is scaled to 0 ([`STOPPED`]). (Elastic 1 removed `succession` and
/// `vouched`: a host no longer waits at its registration — a restarted process is a new host.)
const WAITING: [&str; 3] = ["forge", DRAINED, STOPPED];

/// [`WAITING`]'s word for the fleet reconciler while the world is stopped (its `coordinator`
/// Deployment at 0 replicas): `/readyz`'s body, `mmo_waiting{why="stopped"}`.
pub const STOPPED: &str = "stopped";

/// `/readyz`'s body for a fleet reconciler whose last pass without a failed action is older than
/// [`crate::fleet::FLEET_READY_MS`] (or that has had none yet).
pub const FLEET_FAILING: &str = "failing";

/// [`WAITING`]'s word for a drained host: `/readyz`'s body, `mmo_waiting{why="drained"}`.
pub const DRAINED: &str = "drained";

/// How long a host stays Ready after its coordinator attachment lapsed: one host lease
/// (`HOST_LEASE_MS`, 10 s), so a coordinator replica's restart does not unready every host.
pub const ATTACH_GRACE_MS: i64 = 10_000;

/// `last_beat_ms` before the first [`Health::loop_beat`].
const NEVER: i64 = i64::MIN;

/// The map tile route's status codes, the `code` label of `mmo_map_requests_total` (any other: `other`).
pub const MAP_CODES: [u16; 5] = [200, 304, 400, 404, 503];

/// The coordinator HTTP API's routes, the `route` label of `mmo_coordinator_api_requests_total`
/// ([`Health::api_request`]); a path that is none of the four is `other`.
pub const API_ROUTES: [&str; 5] = ["host_messages", "host_mailbox", "login_orders", "login_answers", "other"];

/// The status codes the coordinator HTTP API answers, the `code` label of
/// `mmo_coordinator_api_requests_total`; any other code is counted as `other` (the last slot).
pub const API_CODES: [u16; 9] = [200, 202, 400, 401, 404, 412, 413, 500, 503];

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
    host_land: AtomicU64,
    land_released: AtomicU64,
    land_assigned: AtomicU64,
    fleet_wanted: AtomicU64,
    fleet_draining: AtomicU64,
    fleet_added: AtomicU64,
    fleet_drained: AtomicU64,
    fleet_removed: AtomicU64,
    fleet_full: AtomicU64,
    coordinator_transitions: AtomicU64,
    coordinator_core_bytes: AtomicU64,
    coordinator_history_bytes: AtomicU64,
    coordinator_conflicts: AtomicU64,
    coordinator_replicas_live: AtomicU64,
    /// The ticker's last mailbox read (coordinator HTTP API): unread rows of every host, and the
    /// age (store ms) of the oldest; 0 and 0 on a replica that is not the ticker.
    coordinator_mailbox_rows: AtomicU64,
    coordinator_mailbox_oldest_ms: AtomicU64,
    /// Hosts the ticker detached as stalled (a mailbox row unread past `MAILBOX_STALL_MS`).
    coordinator_stalled: AtomicU64,
    /// Per [`API_ROUTES`] × ([`API_CODES`] + `other`): requests the coordinator's API answered.
    api_requests: [[AtomicU64; API_CODES.len() + 1]; API_ROUTES.len()],
    /// The world map's tile route (`map_api.rs`): requests per [`MAP_CODES`] (+ `other`), tiles built,
    /// their build time (ms, summed), the reader's cache.
    map_requests: [AtomicU64; MAP_CODES.len() + 1],
    map_built: AtomicU64,
    map_build_ms: AtomicU64,
    map_cache_bytes: AtomicU64,
    /// A host's link to the coordinator (`world_http.rs`): up now.
    world_link_up: AtomicBool,
    /// `Attach` batches the host's link posted after its first (each a re-attach).
    world_link_reattach: AtomicU64,
    coordinator_is_ticker: AtomicBool,
    /// The fleet reconciler (`VOXEL_ROLE=fleet`): `/readyz` follows its passes
    /// ([`Health::require_fleet_passes`]).
    fleet_role: AtomicBool,
    /// Its last pass that completed without a failed action, ms after `epoch` ([`NEVER`]: none).
    fleet_ok_ms: AtomicI64,
    fleet_desired: AtomicU64,
    fleet_deployments: AtomicU64,
    fleet_created: AtomicU64,
    fleet_deleted: AtomicU64,
    fleet_errors: AtomicU64,
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
            host_land: AtomicU64::new(0),
            land_released: AtomicU64::new(0),
            land_assigned: AtomicU64::new(0),
            fleet_wanted: AtomicU64::new(0),
            fleet_draining: AtomicU64::new(0),
            fleet_added: AtomicU64::new(0),
            fleet_drained: AtomicU64::new(0),
            fleet_removed: AtomicU64::new(0),
            fleet_full: AtomicU64::new(0),
            coordinator_transitions: AtomicU64::new(0),
            coordinator_core_bytes: AtomicU64::new(0),
            coordinator_history_bytes: AtomicU64::new(0),
            coordinator_conflicts: AtomicU64::new(0),
            coordinator_replicas_live: AtomicU64::new(0),
            coordinator_mailbox_rows: AtomicU64::new(0),
            coordinator_mailbox_oldest_ms: AtomicU64::new(0),
            coordinator_stalled: AtomicU64::new(0),
            api_requests: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
            map_requests: std::array::from_fn(|_| AtomicU64::new(0)),
            map_built: AtomicU64::new(0),
            map_build_ms: AtomicU64::new(0),
            map_cache_bytes: AtomicU64::new(0),
            world_link_up: AtomicBool::new(false),
            world_link_reattach: AtomicU64::new(0),
            coordinator_is_ticker: AtomicBool::new(false),
            fleet_role: AtomicBool::new(false),
            fleet_ok_ms: AtomicI64::new(NEVER),
            fleet_desired: AtomicU64::new(0),
            fleet_deployments: AtomicU64::new(0),
            fleet_created: AtomicU64::new(0),
            fleet_deleted: AtomicU64::new(0),
            fleet_errors: AtomicU64::new(0),
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
        if self.fleet_role.load(Ordering::Relaxed) {
            return self.waiting().is_none() && self.fleet_pass_age_ms().is_some_and(|age| age <= crate::fleet::FLEET_READY_MS);
        }
        if !self.requires_attach.load(Ordering::Relaxed) || self.attached.load(Ordering::Relaxed) {
            return true;
        }
        let at = self.attached_ms.load(Ordering::Relaxed);
        at != NEVER && self.now_ms() - at <= ATTACH_GRACE_MS
    }

    /// Why the process is not registered yet: `"forge"`, [`DRAINED`] or [`STOPPED`] (anything else
    /// is taken as `None`); `None`: not waiting. (The fleet reconciler is registered while
    /// [`STOPPED`]: its loop still deletes, and `/livez` still judges it.)
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
    pub fn host_load(&self, load: &game::HostLoad) {
        self.load_players.store(u64::from(load.players), Ordering::Relaxed);
        self.load_regions.store(u64::from(load.regions), Ordering::Relaxed);
        self.load_tick_permille.store(u64::from(load.tick_permille), Ordering::Relaxed);
    }

    /// A host's land (Elastic 3): the super-regions its fence holds now, and how many it has
    /// released since it started ([`game::WorldManager::land_released_total`]).
    pub fn host_land(&self, land: u64, released_total: u64) {
        self.host_land.store(land, Ordering::Relaxed);
        self.land_released.store(released_total, Ordering::Relaxed);
    }

    /// A coordinator replica applied a store decision's `Assigned::Done` answer (Elastic 3).
    pub fn land_assigned(&self) {
        self.land_assigned.fetch_add(1, Ordering::Relaxed);
    }

    /// The ticker's last `hosts` read showed this many `desired_hosts` rows wanted and draining
    /// (Elastic 4; 0 and 0 on a replica that is not the ticker).
    pub fn fleet_rows(&self, wanted: u64, draining: u64) {
        self.fleet_wanted.store(wanted, Ordering::Relaxed);
        self.fleet_draining.store(draining, Ordering::Relaxed);
    }

    /// A coordinator replica applied a fleet decision's `HostAdded::Done` answer (Elastic 4).
    pub fn fleet_added(&self) {
        self.fleet_added.fetch_add(1, Ordering::Relaxed);
    }

    /// A coordinator replica applied a fleet decision's `HostDrained::Marked` / `RowOnly` answer.
    pub fn fleet_drained(&self) {
        self.fleet_drained.fetch_add(1, Ordering::Relaxed);
    }

    /// A coordinator replica applied a fleet decision's `HostRemoved::Done` answer.
    pub fn fleet_removed(&self) {
        self.fleet_removed.fetch_add(1, Ordering::Relaxed);
    }

    /// A coordinator replica applied a fleet decision's `HostAdded::Full` answer (every slot held).
    pub fn fleet_full(&self) {
        self.fleet_full.fetch_add(1, Ordering::Relaxed);
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

    /// The ticker's last read of the hosts' mailboxes (coordinator HTTP API): unread rows of every
    /// host, and the age in store ms of the oldest (0 with none). A replica that is not the ticker
    /// sets 0 and 0 (it reads nothing).
    pub fn mailbox_seen(&self, rows: u64, oldest_ms: u64) {
        self.coordinator_mailbox_rows.store(rows, Ordering::Relaxed);
        self.coordinator_mailbox_oldest_ms.store(oldest_ms, Ordering::Relaxed);
    }

    /// The ticker detached `n` hosts as stalled (their mailbox unread past `MAILBOX_STALL_MS`).
    pub fn hosts_stalled(&self, n: u64) {
        self.coordinator_stalled.fetch_add(n, Ordering::Relaxed);
    }

    /// The map route answered `status`.
    pub fn map_request(&self, status: u16) {
        let code = MAP_CODES.iter().position(|&c| c == status).unwrap_or(MAP_CODES.len());
        self.map_requests[code].fetch_add(1, Ordering::Relaxed);
    }

    /// The map route built a tile in `ms`, its reader's cache now `cache_bytes`.
    pub fn map_built(&self, ms: u64, cache_bytes: u64) {
        self.map_built.fetch_add(1, Ordering::Relaxed);
        self.map_build_ms.fetch_add(ms, Ordering::Relaxed);
        self.map_cache_bytes.store(cache_bytes, Ordering::Relaxed);
    }

    /// The coordinator's API answered a request on route `route` (an index of [`API_ROUTES`];
    /// out of range: `other`) with `status`. One atomic add, no allocation.
    pub fn api_request(&self, route: usize, status: u16) {
        let route = route.min(API_ROUTES.len() - 1);
        let code = API_CODES.iter().position(|&c| c == status).unwrap_or(API_CODES.len());
        self.api_requests[route][code].fetch_add(1, Ordering::Relaxed);
    }

    /// A host's link to the coordinator went up (`true`) or down.
    pub fn world_link_up(&self, up: bool) {
        self.world_link_up.store(up, Ordering::Relaxed);
    }

    /// A host's link posted an `Attach` after its first one (a re-attach).
    pub fn world_link_reattached(&self) {
        self.world_link_reattach.fetch_add(1, Ordering::Relaxed);
    }

    /// The fleet reconciler (`VOXEL_ROLE=fleet`): Ready only while registered, not waiting, and its
    /// last pass without a failed action is at most [`crate::fleet::FLEET_READY_MS`] old.
    pub fn require_fleet_passes(&self) {
        self.fleet_role.store(true, Ordering::Relaxed);
    }

    /// A fleet pass ended: `ok` when every read succeeded and no action failed.
    pub fn fleet_pass_done(&self, ok: bool) {
        if ok {
            self.fleet_ok_ms.store(self.now_ms(), Ordering::Relaxed);
        }
    }

    /// What the fleet pass read: `desired_hosts` rows, and host Deployments labelled for this world.
    pub fn fleet_seen(&self, desired: u64, deployments: u64) {
        self.fleet_desired.store(desired, Ordering::Relaxed);
        self.fleet_deployments.store(deployments, Ordering::Relaxed);
    }

    /// The fleet reconciler created an object (a Kubernetes answer of 201; a conflict is not counted).
    pub fn fleet_created(&self) {
        self.fleet_created.fetch_add(1, Ordering::Relaxed);
    }

    /// The fleet reconciler deleted an object (an object already gone is counted too).
    pub fn fleet_deleted(&self) {
        self.fleet_deleted.fetch_add(1, Ordering::Relaxed);
    }

    /// A fleet read or action failed.
    pub fn fleet_error(&self) {
        self.fleet_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Milliseconds since the fleet reconciler's last pass without a failed action; `None` before
    /// the first.
    fn fleet_pass_age_ms(&self) -> Option<u64> {
        match self.fleet_ok_ms.load(Ordering::Relaxed) {
            NEVER => None,
            at => Some(u64::try_from(self.now_ms().saturating_sub(at)).unwrap_or(0)),
        }
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
        gauge("mmo_host_land", "Super-regions this host's fence holds (its land).", load(&self.host_land));
        counter("mmo_land_released_total", "Super-regions this host released (idle land given back) since it started.", load(&self.land_released));
        counter("mmo_land_assigned_total", "Land assignments (Assigned::Done answers) this coordinator replica applied.", load(&self.land_assigned));
        gauge("mmo_hosts_live", "Hosts whose `hosts` row is dirty and live at the coordinator's last read.", load(&self.hosts_live));
        gauge("mmo_fleet_wanted", "Desired hosts wanted at the ticker's last hosts read (0 on a replica that is not the ticker).", load(&self.fleet_wanted));
        gauge(
            "mmo_fleet_draining",
            "Desired hosts draining at the ticker's last hosts read (0 on a replica that is not the ticker).",
            load(&self.fleet_draining),
        );
        counter("mmo_fleet_added_total", "Desired hosts this coordinator replica added (HostAdded::Done answers).", load(&self.fleet_added));
        counter("mmo_fleet_drained_total", "Desired hosts this coordinator replica marked draining (HostDrained answers that wrote).", load(&self.fleet_drained));
        counter("mmo_fleet_removed_total", "Desired hosts this coordinator replica removed (HostRemoved::Done answers).", load(&self.fleet_removed));
        counter(
            "mmo_fleet_full_total",
            "Hosts this coordinator replica wanted to add while every slot was held (HostAdded::Full answers).",
            load(&self.fleet_full),
        );
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
        // The coordinator HTTP API's (constant 0 on the other roles).
        gauge(
            "mmo_coordinator_mailbox_rows",
            "Unread host mailbox rows at the ticker's last read (0 on a replica that is not the ticker).",
            load(&self.coordinator_mailbox_rows),
        );
        gauge(
            "mmo_coordinator_mailbox_oldest_ms",
            "Age (store ms) of the oldest unread host mailbox row at the ticker's last read (0 with none, and on a replica that is not the ticker).",
            load(&self.coordinator_mailbox_oldest_ms),
        );
        counter(
            "mmo_coordinator_stalled_total",
            "Hosts this coordinator replica, as the ticker, detached as stalled (a mailbox row unread past MAILBOX_STALL_MS).",
            load(&self.coordinator_stalled),
        );
        let requests = IntCounterVec::new(
            opts("mmo_coordinator_api_requests_total", "Requests the coordinator's HTTP API answered, by route and status code."),
            &["route", "code"],
        )
        .expect("valid metric");
        for (r, route) in API_ROUTES.iter().enumerate() {
            for (c, n) in self.api_requests[r].iter().enumerate() {
                let code = API_CODES.get(c).map_or_else(|| "other".to_string(), u16::to_string);
                requests.with_label_values(&[route, code.as_str()]).inc_by(load(n));
            }
        }
        register(Box::new(requests));
        // The world map's tile route (constant 0 where it is not served).
        counter("mmo_map_tiles_built_total", "Map tiles the tile route built (cache misses).", load(&self.map_built));
        counter("mmo_map_build_ms_total", "Milliseconds spent building map tiles (with mmo_map_tiles_built_total: the mean).", load(&self.map_build_ms));
        gauge("mmo_map_cache_bytes", "Bytes of encoded tiles in the tile route's cache.", load(&self.map_cache_bytes));
        let map_requests = IntCounterVec::new(opts("mmo_map_requests_total", "Requests the map tile route answered, by status code."), &["code"])
            .expect("valid metric");
        for (c, n) in self.map_requests.iter().enumerate() {
            let code = MAP_CODES.get(c).map_or_else(|| "other".to_string(), u16::to_string);
            map_requests.with_label_values(&[code.as_str()]).inc_by(load(n));
        }
        register(Box::new(map_requests));
        // A host's link to the coordinator (constant 0 on the other roles, and on role `all`).
        gauge("mmo_world_link_up", "1 while this host's link to the world coordinator is up.", u64::from(self.world_link_up.load(Ordering::Relaxed)));
        counter(
            "mmo_world_link_reattach_total",
            "Attach batches this host's link to the coordinator posted after its first (re-attaches).",
            load(&self.world_link_reattach),
        );
        // The fleet reconciler's (constant 0 on the other roles).
        gauge("mmo_fleet_desired", "desired_hosts rows the fleet reconciler's last successful read showed.", load(&self.fleet_desired));
        gauge(
            "mmo_fleet_deployments",
            "Host Deployments labelled for this world at the fleet reconciler's last successful list.",
            load(&self.fleet_deployments),
        );
        counter("mmo_fleet_created_total", "Kubernetes objects the fleet reconciler created.", load(&self.fleet_created));
        counter("mmo_fleet_deleted_total", "Kubernetes objects the fleet reconciler deleted.", load(&self.fleet_deleted));
        counter("mmo_fleet_errors_total", "Fleet reads and actions that failed (each retried by the next pass).", load(&self.fleet_errors));
        let pass_age = if self.fleet_role.load(Ordering::Relaxed) {
            // Before the first good pass: since the process started.
            self.fleet_pass_age_ms().unwrap_or_else(|| u64::try_from(self.now_ms()).unwrap_or(0))
        } else {
            0
        };
        gauge(
            "mmo_fleet_pass_age_ms",
            "Milliseconds since the fleet reconciler's last pass without a failed read or action (0 on the other roles).",
            pass_age,
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

/// Serves [`Health::global`] on `bind` for a process of `role` (`host`, `coordinator` or `fleet`):
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
    } else if s.health.fleet_role.load(Ordering::Relaxed) && s.health.registered.load(Ordering::Relaxed) {
        (StatusCode::SERVICE_UNAVAILABLE, s.health.waiting().unwrap_or(FLEET_FAILING))
    } else if s.health.registered.load(Ordering::Relaxed) {
        (StatusCode::SERVICE_UNAVAILABLE, "attaching")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, s.health.waiting().unwrap_or("starting"))
    }
}

async fn metrics(State(s): State<Served>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], s.health.render(s.role))
}

