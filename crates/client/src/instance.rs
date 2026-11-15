//! `GameInstanceManager`: coordinates networking / rollback for the game.
//! Moved out of `main.rs` so it's reachable from lib consumers (headless
//! tests, the harness) without pulling in Bevy/windowing.
use crossbeam::channel::{Receiver, Sender};
#[cfg(not(target_arch = "wasm32"))]
use crossbeam::select;
use crate::connections::{Closed, ConnEvent, ConnHandle, ConnOpener, ConnectionSet, Named, Outgoing};
use crate::regions::runner::{RegionCmd, RegionReport};
use game::{ClientId, ReconcileStats, RegionCoords, RegionId, Rollback, ServerPacket};
use game::{
    ClientPacket, ClientUpdateEvent, GameError, GameEvent, GameEventKind,
};
#[cfg(test)]
use game::Region;
use log::{error, info, warn};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

/// Game-thread half of the join timeline (`game::perf`). Every flag latches:
/// each milestone is reported once per process, so steady-state play (window
/// moves, LOD rebuild pushes, clock-drift `ready` flips) adds no lines.
#[derive(Default)]
struct JoinTimeline {
    /// LOD keys whose payload has arrived at least once.
    lod_received: std::collections::BTreeSet<game::lod::LodKey>,
    lod_requests_done: bool,
    lod_all_received: bool,
    connection_ready: bool,
}

/// Requests from the UI that go to the server rather than into the sim
/// (they are not predicted events).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ClientRequest {
    /// Play this region: the server moves our session there (or refuses,
    /// answered as `ClientUpdateEvent::PlayRefused`).
    PlayRegion(RegionCoords),
    /// Ask who may embark (answered as `ClientUpdateEvent::EmbarkOptions`).
    RequestEmbarkOptions,
    /// Found a fortress (answered as `ClientUpdateEvent::EmbarkResult`).
    Embark(game::EmbarkRequest),
    /// Stop playing the fortress.
    Retire,
    /// Play a retired fortress again (answered as `EmbarkResult`).
    ReturnTo(game::history::SiteId),
    /// Ask for one person (answered as `ClientUpdateEvent::PersonInfo`).
    RequestPersonInfo(game::history::PersonId),
    /// Leaving the world (back to the main menu): tell the server now, so it
    /// drops our session at once instead of at the connection's idle timeout.
    /// Followed by `GameEventKind::Quit`, which sends any request still queued
    /// before the session ends.
    Leave,
    /// Ask for a page of the legends log (answered as `ClientUpdateEvent::Legends`).
    RequestLegends(game::history::LegendsQuery),
    /// Start an adventure from a populated site (answered as `AdventureResult`).
    StartAdventure(game::history::SiteId),
    /// Stop the adventure.
    EndAdventure,
}

/// Command sent from the client (Bevy app) to the game thread.
#[cfg(not(target_arch = "wasm32"))]
pub enum Command {
    /// Connect to a server, sync and start running game sim.
    ConnectToServerAndScene(
        Sender<GameEventKind>,
        Receiver<GameEventKind>,
        Sender<ClientUpdateEvent>,
        Receiver<ClientRequest>,
        SocketAddr,
        game::DebugFlags,
        crate::world_scale::WorldScale,
        crate::netcode::ConnectPolicy,
        // What the Hello carries; `None` = the local player name.
        Option<game::Credential>,
    ),
    /// Quit the game thread. Should only be send when quitting the application.
    Quit,
}

/// Spawn the game thread: it runs one session per `ConnectToServerAndScene`
/// and waits for the next command when a session ends.
#[cfg(not(target_arch = "wasm32"))]
pub fn start_game_thread() -> Sender<Command> {
    let (command_send, command_recv) = crossbeam::channel::unbounded();
    std::thread::spawn(move || loop {
        match command_recv.recv() {
            Ok(Command::ConnectToServerAndScene(
                sender,
                receiver,
                client_sender,
                request_recv,
                server,
                debug_flags,
                world_scale,
                connect_policy,
                credential,
            )) => {
                let mut manager =
                    GameInstanceManager::new(sender, receiver, client_sender, server, debug_flags);
                manager.set_world_scale(world_scale);
                manager.set_request_recv(request_recv);
                manager.set_connect_policy(connect_policy);
                manager.set_credential(credential);
                // The manager owns `client_sender`; keep a handle to report
                // how the session ended. Exactly one report per session,
                // then wait for the next command: a lost server must not
                // take the thread down (the menu starts the next session).
                let report = manager.client_event_send.clone();
                match manager.connect_and_run() {
                    Ok(()) | Err(GameError::QuitRequested) => {}
                    Err(e) => {
                        warn!("Game session ended: {:?}", e);
                        let _ = report.send(ClientUpdateEvent::GameCrash(e));
                    }
                }
            }
            Ok(Command::Quit) => {
                log::trace!("Game thread recieved quit command.");
                break;
            }
            Err(_e) => {
                warn!(
                    "Game thread stoped receiving command events, stopping game thread. Client probably crashed or was closed incorrectly."
                );
                break;
            }
        }
    });
    command_send
}

/// Wrapper struct for coordinating networking / rollback for the game.
pub struct GameInstanceManager {
    game_event_send: Sender<GameEventKind>,
    game_event_recv: Receiver<GameEventKind>,
    /// UI requests for the server (`ClientRequest`). `never()` until
    /// `set_request_recv`: harnesses and the offline world send none.
    request_recv: Receiver<ClientRequest>,
    client_event_send: Sender<ClientUpdateEvent>,
    /// Only read by the native netcode path (`connect_and_run`); kept on wasm
    /// so `new()` has one signature on both targets.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    server: SocketAddr,
    /// How `connect_and_run` retries the initial connect (tests shorten it).
    #[cfg(not(target_arch = "wasm32"))]
    connect_policy: crate::netcode::ConnectPolicy,
    /// The Hello's credential (`None` = the local player name).
    #[cfg(not(target_arch = "wasm32"))]
    credential: Option<game::Credential>,

    /// Everything this session sends — the manager's own packets and the region
    /// runners' predictions — on its way to `conns` (`flush_outgoing`).
    out_send: Sender<Outgoing>,
    out_recv: Receiver<Outgoing>,
    /// One connection per server host this session talks to; the first is the home
    /// (`connections`). Lives and dies with the session, like the manager.
    conns: ConnectionSet,
    /// What goes out on the first (login) connection: its transport reads it.
    first_conn_recv: Receiver<ClientPacket>,
    /// Opens connections to other hosts (`None`: this transport has only the one).
    opener: Option<Box<dyn ConnOpener + Send>>,
    /// Regions whose observer connection closed, asked for again through the home
    /// once `conn_clock_ms` reaches the due time (`connections::observer_retry_delay_ms`),
    /// or sooner when the body comes near the region (`retry_observer_regions`):
    /// `(due, scheduled at)`.
    observer_retry: BTreeMap<RegionCoords, (u64, u64)>,
    /// How many times in a row each host's observer connection ended with regions to
    /// ask for again (by host ordinal; reset by a snapshot from that host): the retry
    /// backs off.
    observer_failures: BTreeMap<u32, u32>,
    /// Session time in ms: `TICK_RATE` per client tick (no wall clock: harnesses and
    /// wasm run it too).
    conn_clock_ms: u64,
    /// A region the home sent the client to on another host unasked
    /// (`RegionElsewhere` for a region it did not subscribe): a move's target, held in
    /// the window until the `PlayerRegion` of the move comes.
    pending_move: Option<RegionCoords>,
    /// Regions whose subscribes went out through the home and were not all answered
    /// yet (by a snapshot or a `RegionElsewhere`): how many are unanswered, and when
    /// (`conn_clock_ms`) the last went. A `RegionElsewhere` for one released meanwhile
    /// is a late answer, not a move — however many requests and releases came between.
    asked_home: BTreeMap<RegionCoords, (u32, u64)>,
    /// Regions subscribed on another host's connection (`RegionElsewhere`) and not held
    /// yet, with when (`conn_clock_ms`): one that connection has not served within
    /// `connections::OBSERVER_RETRY_MAX_MS` is let go there and asked of the home again
    /// (`ask_unanswered_again`; R4: an observer session waits rather than redirect).
    asked_observer: BTreeMap<RegionCoords, u64>,
    /// Regions released from the window while another host's connection served them, and
    /// when: the home may still answer for one (a re-answer after a succession crossing
    /// the release, R4), which reads like a move ([`AMBIGUOUS_MOVE_HOLD_MS`]).
    released_elsewhere: BTreeMap<RegionCoords, u64>,
    /// A `pending_move` taken from such an answer, and until when it is held without the
    /// move's `PlayerRegion`.
    ambiguous_move: Option<(RegionCoords, u64)>,
    /// Since when (`conn_clock_ms`) the predicted body — or a move — is on another
    /// host than the home's with no `PlayerRegion` from there, and whether that was
    /// logged ([`HOME_UNCONFIRMED_WARN_MS`]).
    home_unconfirmed_since: Option<u64>,
    home_unconfirmed_warned: bool,
    /// LOD keys still to be asked of the home again since it moved hosts (paced like
    /// new keys).
    lod_resend: std::collections::BTreeSet<game::lod::LodKey>,
    /// LOD keys still to be released on the connection of a former home (paced like
    /// the requests).
    lod_release: std::collections::VecDeque<(u32, game::lod::LodKey)>,
    /// Where the predicted regions run (inline here; one thread each in
    /// native play, `use_region_threads`). The manager never touches a
    /// `Region` directly: `RegionCmd` in, `RegionReport` out.
    host: crate::regions::host::RegionHost,
    /// Set by the first region snapshot (today's "world exists").
    world_started: bool,
    /// Each region's latest view of the local client (`RegionReport::View`):
    /// the home region's is what the camera, HUD and UI read.
    views: BTreeMap<RegionId, crate::regions::runner::HomeView>,
    /// The one predicted world tick every region runs to (`regions::clock`).
    clock: crate::regions::clock::PredictedClock,
    /// `(tick, state hash)` per region from the last `region_probe`.
    probe: BTreeMap<RegionId, (game::Tick, u32)>,
    /// The finite world's extent and coarse-route map, from the server's
    /// `WorldMap`: every predicted region is bounded by it and routes walks
    /// over it, as the server's do (`Region::set_route_map`). `None` =
    /// unbounded (procedural).
    route_map: Option<std::sync::Arc<game::RouteMap>>,
    buffer: Vec<GameEvent>,
    tick_rate: Arc<AtomicU64>,
    ready: bool,
    is_caught_up: bool,
    /// Each region's tick as its last `Ticked` report left it: how far it
    /// runs ahead of the clock (held) feeds the crossing probe.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    region_ticks: BTreeMap<RegionId, game::Tick>,
    client_id: Option<ClientId>,
    home_region: Option<RegionCoords>,
    /// Sticky centre of the near window + LOD rings; see `advance_window_center`.
    window_center: Option<RegionCoords>,
    /// Regions we have requested (and not released) — the desired window.
    subscribed: std::collections::BTreeSet<RegionCoords>,
    /// Last ring-factor assignment per region (the stickiness `prev` for the
    /// next `desired_lod` call).
    lod_assigned: BTreeMap<RegionCoords, u32>,
    /// Ring table driving `update_lod_window` — a field (defaulting to
    /// `LOD_RING_TABLE`) so test harnesses can shrink it: every key it
    /// yields costs the server a real build, which headless suites pay
    /// synchronously through `InlineLodBuilder`.
    lod_table: &'static [(i32, u32)],
    /// Radius of the near (authoritative) window: 1 = 3x3 here; the binaries
    /// apply the production `WorldScale` (5x5) via `set_world_scale`.
    /// Headless suites shrink it to 0 — each region costs a full worldgen +
    /// collider genesis on the server AND a deserialized copy on the client.
    near_radius: i32,
    /// Radius of the simulated neighbourhood around `fortress_region`
    /// (`WorldScale::fortress_radius`, applied by `set_world_scale`; 0 here so
    /// headless suites keep their one-region world).
    fortress_radius: i32,
    /// The region of the fortress this client plays (`ServerPacket::Fortress`).
    fortress_region: Option<RegionCoords>,
    /// LOD keys we have requested and not released.
    lod_subscribed: std::collections::BTreeSet<game::lod::LodKey>,
    /// One-shot join milestones for the `[perf]` timeline.
    join: JoinTimeline,
    /// Events for subscribed regions whose snapshot hasn't arrived yet.
    pending_events: BTreeMap<RegionCoords, Vec<GameEvent>>,
    /// Last HUD status sent, for edge-triggered emission (avoid channel spam).
    last_hud_status: Option<(RegionCoords, RegionCoords, game::ConnectionState)>,
    /// Sender half of the client-side UI projection channel (widget deltas).
    ui_update_send: Sender<game::UiUpdate>,
    /// Last hotbar emitted; the next projection is sent only if it differs.
    prev_hotbar: game::HotbarView,
    prev_inventory_view: game::InventoryView,
    prev_mining: Option<u32>,
    prev_play_mode: game::PlayMode,
    /// Last-emitted cursor-grab intent, for edge-triggered SetCursorGrab.
    last_cursor_grab: bool,
    /// Shared debug toggles; the render thread's F3 dev panel flips them.
    /// Process-local — not sim state.
    debug_flags: game::DebugFlags,
    /// Rolling reconciliation / clock-drift keep-up counters.
    recon: ReconMetrics,
    /// Rolling wall-time budget attribution (native only).
    #[cfg(not(target_arch = "wasm32"))]
    perf: PerfBudget,
    #[cfg(not(target_arch = "wasm32"))]
    crossing: CrossingProbe,
    /// Per-region forward-tick wall times for the F3 Debug tab (native only).
    #[cfg(not(target_arch = "wasm32"))]
    tick_times: TickTimes,
}

/// The last [`game::perf::TICK_TIME_WINDOW`] forward-tick times of each live
/// region (µs), summarised to the render thread every
/// [`game::perf::TICK_TIME_REPORT_INTERVAL`].
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct TickTimes {
    windows: BTreeMap<RegionId, std::collections::VecDeque<u32>>,
    last_sent: Option<Instant>,
}

#[cfg(not(target_arch = "wasm32"))]
impl TickTimes {
    fn record(&mut self, id: RegionId, took: Duration) {
        let window = self.windows.entry(id).or_default();
        if window.len() == game::perf::TICK_TIME_WINDOW {
            window.pop_front();
        }
        window.push_back(took.as_micros().min(u32::MAX as u128) as u32);
    }

    /// Every region in `live` with samples, summarised (in `RegionId`
    /// order); windows of regions no longer live are dropped. `None` until
    /// the interval has passed.
    fn report(&mut self, live: &[RegionId], now: Instant) -> Option<Vec<(RegionId, game::perf::TickTimeStats)>> {
        if self.last_sent.is_some_and(|t| now.duration_since(t) < game::perf::TICK_TIME_REPORT_INTERVAL) {
            return None;
        }
        self.last_sent = Some(now);
        self.windows.retain(|id, _| live.contains(id));
        Some(
            self.windows
                .iter()
                .filter_map(|(id, w)| Some((*id, game::perf::TickTimeStats::from_samples(w.iter().copied())?)))
                .collect(),
        )
    }
}

/// Death-spiral tripwire tunables. The spiral's early signature (root-caused
/// live 2026-07-08) is the home region's unconfirmed prediction log rising
/// across metric windows: replay cost grows with depth, which slows
/// confirmation, which grows depth. Depth is sampled once per
/// `METRICS_REPORT_INTERVAL`; a strictly-rising run of `KEEPUP_TREND_WINDOWS`
/// samples ending at or above `KEEPUP_WARN_MIN_DEPTH` warns, as does any
/// single sample at or above `KEEPUP_WARN_HARD_DEPTH`.
const KEEPUP_TREND_WINDOWS: usize = 3;
const KEEPUP_WARN_MIN_DEPTH: usize = 24;
const KEEPUP_WARN_HARD_DEPTH: usize = 64;

/// New `RequestLod` subscriptions sent per client tick. The production ring
/// table wants ~1,080 keys at join (the default-size 1 km horizon);
/// firing them in one burst floods the server's LOD builders and the QUIC
/// stream-credit window (100 concurrent uni streams) with replies the client
/// then has to ingest at once. 32/tick, nearest ring first, drains the full
/// table in ~2s at the 50ms tick and leaves any table up to 32 keys unpaced.
pub const LOD_REQUESTS_PER_TICK: usize = 32;

/// How long the predicted body (or a move the home announced) may stand on another
/// host than the home's with no `PlayerRegion` from that host before the client logs
/// it as stranded (once per episode; it keeps going: the hosts tell it if the body was
/// parked, `PLAYER_PARKED_ELSEWHERE`). Above the hosts' hand-off session grace
/// (`game::HANDOFF_SESSION_GRACE_MS`).
pub const HOME_UNCONFIRMED_WARN_MS: u64 = 15_000;
const _: () = assert!(HOME_UNCONFIRMED_WARN_MS > game::HANDOFF_SESSION_GRACE_MS);

/// How long a subscribe sent through the home is remembered unanswered
/// (`GameInstanceManager::asked_home`) once its region is held or left the window. One
/// still wanted and not held is never forgotten: it is asked again every
/// `connections::OBSERVER_RETRY_MAX_MS` (a home that cannot answer yet remembers it and
/// answers when it can, R4; this is the backstop for an answer lost on the way).
const ASKED_HOME_FORGET_MS: u64 = 10_000;

/// How long a move read from a `RegionElsewhere` for a region released from the window
/// less than `ASKED_HOME_FORGET_MS` ago (while another host served it) is held without the
/// move's `PlayerRegion` (`GameInstanceManager::ambiguous_move`): it may be the home's late
/// re-answer for that region after a succession, not a move. A real move's `PlayerRegion`
/// comes within the hosts' hand-off grace, or its body is parked there and the session is
/// told; so the hold is dropped after this and the window lets the region go.
pub const AMBIGUOUS_MOVE_HOLD_MS: u64 = 10_000;
const _: () = assert!(AMBIGUOUS_MOVE_HOLD_MS > game::HANDOFF_SESSION_GRACE_MS);

/// Client-side rolling counters for reconciliation load and clock drift,
/// flushed to one INFO line every `METRICS_REPORT_INTERVAL`. The window start
/// is lazily set on the first native report so construction stays wasm-safe
/// (`Instant::now()` panics on `wasm32-unknown-unknown`).
#[derive(Default)]
struct ReconMetrics {
    window_start: Option<std::time::Instant>,
    mispredictions: u32,
    /// Every misprediction since the session started (never reset by the
    /// metrics window): the harness reads it (`total_mispredictions`).
    total_mispredictions: u64,
    /// [`Self::total_mispredictions`] by (predicted kind, server kind), never
    /// reset either (`total_mispredictions_by_kind`).
    total_by_kinds: [[u64; 3]; 3],
    replay_depth_sum: usize,
    max_replay_depth: usize,
    /// Latest RTT-adjusted client-vs-server tick drift, in ms (negative =
    /// client is behind the server). Snapshotted from each `SyncClock`.
    last_drift_ms: isize,
    /// Mispredictions by (predicted kind, server kind); see
    /// `ReconcileStats::by_kinds`.
    by_kinds: [[u32; 3]; 3],
    /// Mispredictions in the home region: the ones that can move the player.
    home_mispredictions: u32,
    /// How far home-region reconcile passes moved the local player from
    /// where it was predicted (world units): count, sum, largest.
    corrections: u32,
    correction_sum: f32,
    correction_max: f32,
    /// The same over the whole session (never reset by the metrics window): the
    /// harness reads them (`corrections`).
    total_corrections: u32,
    total_correction_max: f32,
    /// World-clock steps (`regions::clock`): how many, and the ticks the
    /// predicted tick moved over them.
    align_steps: u32,
    align_ticks: usize,
    /// Server state hashes checked against the confirmed state, and how many
    /// differed (`ServerPacket::HashedGameEvent`).
    state_hash_checks: u32,
    state_hash_mismatches: u32,
    /// Home-region prediction-log depth, sampled once per report window;
    /// keeps the last `KEEPUP_TREND_WINDOWS` samples for the tripwire.
    log_depth_samples: Vec<usize>,
}

/// Wall-time attribution for the game-loop thread, flushed to one
/// `[client-budget]` DEBUG line alongside the reconciliation metrics every
/// `METRICS_REPORT_INTERVAL`. Directs optimization work by showing where the
/// loop's budget goes. Categories are measured at their call sites; `other`
/// is derived at report time as window − idle − (tick + reconcile +
/// snapshot), so the line always sums to the window. `idle` is only
/// accumulated by `connect_and_run`'s select! — in pump()-driven harnesses
/// (headless tests) time spent outside the manager lands in `other` instead,
/// so read `util` there as an upper bound.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct PerfBudget {
    /// `progress_world_one_tick`: whole-world sim advance, per client tick.
    tick_total: Duration,
    tick_count: u32,
    tick_max: Duration,
    /// Server-event reconcile passes (buffer drain + rollback/replay), plus
    /// replay of events that raced ahead of a region snapshot.
    reconcile_total: Duration,
    /// `ServerPacket::Region` handling: region teardown + snapshot load
    /// (Rollback clone, render-bridge NewRegion, world insert).
    snapshot_total: Duration,
    snapshot_count: u32,
    /// Time blocked in `connect_and_run`'s select! waiting for work.
    idle_total: Duration,
    /// Per-region sim cost from `RegionReport::Ticked` (includes reconcile
    /// replays, which run through the same tick path).
    sim: BTreeMap<RegionId, game::RegionTickPerf>,
}

#[cfg(not(target_arch = "wasm32"))]
impl PerfBudget {
    fn record_tick(&mut self, d: Duration) {
        self.tick_count += 1;
        self.tick_total += d;
        if d > self.tick_max {
            self.tick_max = d;
        }
    }
}

/// How long after a home-region flip the game thread keeps attributing work
/// to that crossing (matches the render side's `REGION_CROSSING_WINDOW`).
#[cfg(not(target_arch = "wasm32"))]
const CROSSING_WINDOW: Duration = Duration::from_secs(2);

/// Game-thread view of one region crossing — a handoff of the local player
/// between two region threads, which also shifts the near window by a row.
/// Opened at the home flip (predicted or authoritative), reported once as
/// `[perf] event=region_crossing` after [`CROSSING_WINDOW`]. The thread also
/// runs prediction, so `tick_gap_max_ms` is how long the player's own pose
/// went without a tick; `before_tick_gap_max_ms` is the same measure over the
/// flight since the previous crossing.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct CrossingProbe {
    open: Option<(Instant, RegionCoords, RegionCoords)>,
    /// The hosts serving the region left and the region entered (`None`: not known),
    /// as the client's connections had them at the flip: a host flip when they differ.
    hosts: (Option<u32>, Option<u32>),
    /// Ticks the new home still had to sit out (ahead of the clock) at the flip.
    stalled_at_flip: usize,
    last_home_tick: Option<Instant>,
    tick_gap_max: Duration,
    before_tick_gap_max: Duration,
    /// Home ticks sat out (ahead of the clock) inside the window.
    home_stalled_ticks: u32,
    /// World-clock steps inside the window: steps, and ticks the clock moved
    /// forward (+) or back (-).
    home_align_steps: u32,
    home_align_ticks: isize,
    corrections: u32,
    correction_max: f32,
    snapshots: u32,
    snapshot_time: Duration,
    snapshot_max: Duration,
    /// Flip to the server's `PlayerRegion` naming the new home.
    confirm: Option<Duration>,
}

#[cfg(not(target_arch = "wasm32"))]
impl CrossingProbe {
    fn flip(&mut self, from: RegionCoords, to: RegionCoords, stalled_at_flip: usize, hosts: (Option<u32>, Option<u32>)) {
        // Crossed again inside the window: close the old one early.
        self.report();
        *self = CrossingProbe {
            open: Some((Instant::now(), from, to)),
            hosts,
            stalled_at_flip,
            last_home_tick: self.last_home_tick,
            before_tick_gap_max: self.before_tick_gap_max,
            ..Default::default()
        };
    }

    fn home_tick(&mut self, stalled: bool) {
        let now = Instant::now();
        if stalled {
            if self.open.is_some() {
                self.home_stalled_ticks += 1;
            }
            return;
        }
        if let Some(last) = self.last_home_tick {
            let gap = now - last;
            if self.open.is_some() {
                self.tick_gap_max = self.tick_gap_max.max(gap);
            } else {
                self.before_tick_gap_max = self.before_tick_gap_max.max(gap);
            }
        }
        self.last_home_tick = Some(now);
    }

    fn snapshot(&mut self, took: Duration) {
        if self.open.is_some() {
            self.snapshots += 1;
            self.snapshot_time += took;
            self.snapshot_max = self.snapshot_max.max(took);
        }
    }

    fn correction(&mut self, moved: f32) {
        if self.open.is_some() {
            self.corrections += 1;
            self.correction_max = self.correction_max.max(moved);
        }
    }

    fn align_home(&mut self, error_ms: isize, ticks: usize) {
        if self.open.is_some() {
            self.home_align_steps += 1;
            self.home_align_ticks += if error_ms > 0 { -(ticks as isize) } else { ticks as isize };
        }
    }

    fn confirmed(&mut self, home: RegionCoords) {
        if let Some((at, _, to)) = self.open {
            if to == home && self.confirm.is_none() {
                self.confirm = Some(at.elapsed());
            }
        }
    }

    fn maybe_report(&mut self) {
        if self.open.is_some_and(|(at, _, _)| at.elapsed() >= CROSSING_WINDOW) {
            self.report();
        }
    }

    fn report(&mut self) {
        let Some((at, from, to)) = self.open.take() else { return };
        game::perf_event!(
            "region_crossing",
            "from={},{} to={},{} window_ms={} confirm_ms={} stalled_at_flip={} tick_gap_max_ms={} \
             before_tick_gap_max_ms={} home_stalled_ticks={} home_align={}({:+}t) corrections={} \
             correction_max={:.2}u snapshots={} snapshot_ms={} snapshot_max_ms={} host_flip={} from_host={} \
             to_host={}",
            from.x,
            from.z,
            to.x,
            to.z,
            game::perf::ms(at.elapsed()),
            self.confirm.map_or(-1.0, game::perf::ms),
            self.stalled_at_flip,
            game::perf::ms(self.tick_gap_max),
            game::perf::ms(self.before_tick_gap_max),
            self.home_stalled_ticks,
            self.home_align_steps,
            self.home_align_ticks,
            self.corrections,
            self.correction_max,
            self.snapshots,
            game::perf::ms(self.snapshot_time),
            game::perf::ms(self.snapshot_max),
            u8::from(self.hosts.0 != self.hosts.1),
            self.hosts.0.map_or(-1, i64::from),
            self.hosts.1.map_or(-1, i64::from)
        );
        // The next crossing's baseline starts now.
        self.before_tick_gap_max = Duration::ZERO;
    }
}

impl ReconMetrics {
    /// Fold in the cost of one `reconcile()` pass.
    fn record(&mut self, s: ReconcileStats) {
        self.mispredictions += s.mispredictions;
        self.total_mispredictions += u64::from(s.mispredictions);
        self.state_hash_checks += s.state_hash_checks;
        self.state_hash_mismatches += s.state_hash_mismatches;
        for (p, row) in s.by_kinds.iter().enumerate() {
            for (k, &n) in row.iter().enumerate() {
                self.by_kinds[p][k] += n;
                self.total_by_kinds[p][k] += u64::from(n);
            }
        }
        self.replay_depth_sum += s.replay_depth_sum;
        if s.max_replay_depth > self.max_replay_depth {
            self.max_replay_depth = s.max_replay_depth;
        }
    }
}

/// How far into a neighbouring region the viewer must be before the near
/// window (and the LOD rings) recentre on it. Hovering near a seam then keeps
/// one window; flying straight shifts it this much later. Under half a region,
/// or the band around one seam would reach the next.
/// `VOXEL_WINDOW_MARGIN_M` overrides it at startup (`0` = recentre at the seam,
/// the old behaviour — for A/B runs of `just shuttle-probe`), clamped under the
/// half region.
const WINDOW_RECENTER_MARGIN_M: f32 = 16.0;
const _: () = assert!(WINDOW_RECENTER_MARGIN_M * game::movement::UNITS_PER_METER < game::REGION_SIZE / 2.0);

/// [`WINDOW_RECENTER_MARGIN_M`] or its env override, in world units.
fn window_recenter_margin() -> f32 {
    static MARGIN: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *MARGIN.get_or_init(|| {
        let metres = std::env::var("VOXEL_WINDOW_MARGIN_M")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(WINDOW_RECENTER_MARGIN_M);
        let units = metres.max(0.0) * game::movement::UNITS_PER_METER;
        let max = game::REGION_SIZE / 2.0 - 1.0;
        if units > max {
            log::warn!("VOXEL_WINDOW_MARGIN_M={metres} is past half a region; clamped");
        }
        units.min(max)
    })
}


/// The client's desired subscription set: the near window around the viewer,
/// plus the home region. Home must NEVER be released while the free-cam
/// roams — PlayerInput routes to it (World::handle_region_event unwraps on a
/// missing region) and it anchors viewer_region's pose read; this also
/// mirrors the server's keep-alive rule, which pins a client's home region
/// regardless of its window.
///
/// Plus the **fortress neighbourhood**: the fortress region and every region
/// within `fortress_radius` of it, in both play modes — wherever the viewer
/// (the adventurer, in Adventure) is, the fortress and its surroundings keep
/// running on the server and predicted here.
fn desired_window(
    center: RegionCoords,
    home: Option<RegionCoords>,
    radius: i32,
    fortress: Option<RegionCoords>,
    fortress_radius: i32,
) -> std::collections::BTreeSet<RegionCoords> {
    let mut desired: std::collections::BTreeSet<RegionCoords> =
        center.window_radius(radius).into_iter().collect();
    if let Some(home) = home {
        desired.insert(home);
    }
    if let Some(fortress) = fortress {
        desired.extend(fortress.window_radius(fortress_radius.max(0)));
    }
    desired
}

/// Ring assignment for every region in view: per-region factor via the
/// shared `ring_factor_in` (sticky against `prev`), minus `excluded`
/// (near-window members render authoritative state). Returns the
/// region->factor map (the next `prev`) and the deduped key set to hold
/// subscriptions for.
pub fn desired_lod(
    viewer: RegionCoords,
    excluded: &std::collections::BTreeSet<RegionCoords>,
    prev: &BTreeMap<RegionCoords, u32>,
    table: &[(i32, u32)],
    band: i32,
) -> (
    BTreeMap<RegionCoords, u32>,
    std::collections::BTreeSet<game::lod::LodKey>,
) {
    let radius = table.last().map_or(0, |&(bound, _)| bound) + band;
    let mut assigned = BTreeMap::new();
    let mut keys = std::collections::BTreeSet::new();
    let mut candidates: std::collections::BTreeSet<RegionCoords> = prev.keys().copied().collect();
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            candidates.insert(RegionCoords::new(viewer.x + dx, viewer.z + dz));
        }
    }
    for rc in candidates {
        if excluded.contains(&rc) {
            continue;
        }
        let dist = (rc.x - viewer.x).abs().max((rc.z - viewer.z).abs());
        if let Some(factor) = game::lod::ring_factor_in(table, band, dist, prev.get(&rc).copied()) {
            assigned.insert(rc, factor);
            keys.insert(game::lod::LodKey::for_region(factor, rc));
        }
    }
    (assigned, keys)
}

impl GameInstanceManager {
    /// Create new GameInstanceManager
    ///
    /// - Talk to ingress to figure out which regions to load for a given player.
    /// - Start listening game events and log them.
    /// - Download game data
    /// - Re-simulate game based on logged events.
    /// - Send spawn player event and enter main loop
    pub fn new(
        game_event_send: Sender<GameEventKind>,
        game_event_recv: Receiver<GameEventKind>,
        client_event_send: Sender<ClientUpdateEvent>,
        server: SocketAddr,
        debug_flags: game::DebugFlags,
    ) -> Self {
        let (out_send, out_recv) = crossbeam::channel::unbounded();
        let (first_conn_send, first_conn_recv) = crossbeam::channel::unbounded();
        let (ui_update_send, ui_update_recv) = crossbeam::channel::unbounded();
        // Announce the UI receiver to the bridge once. Buffered channel: safe
        // to send before the bridge's resources exist.
        let _ = client_event_send.send(game::ClientUpdateEvent::UiChannel(ui_update_recv));
        Self {
            game_event_recv,
            game_event_send,
            request_recv: crossbeam::channel::never(),
            client_event_send,
            server,
            #[cfg(not(target_arch = "wasm32"))]
            connect_policy: crate::netcode::ConnectPolicy::default(),
            #[cfg(not(target_arch = "wasm32"))]
            credential: None,
            host: crate::regions::host::RegionHost::inline(),
            world_started: false,
            views: BTreeMap::new(),
            probe: BTreeMap::new(),
            clock: Default::default(),
            route_map: None,
            buffer: Vec::new(),
            tick_rate: Arc::new(AtomicU64::new(game::TICK_RATE)),
            ready: false,
            is_caught_up: false,
            region_ticks: BTreeMap::new(),
            out_send,
            out_recv,
            conns: ConnectionSet::new(ConnHandle { send: first_conn_send }),
            first_conn_recv,
            opener: None,
            observer_retry: BTreeMap::new(),
            observer_failures: BTreeMap::new(),
            conn_clock_ms: 0,
            pending_move: None,
            asked_home: BTreeMap::new(),
            asked_observer: BTreeMap::new(),
            released_elsewhere: BTreeMap::new(),
            ambiguous_move: None,
            home_unconfirmed_since: None,
            home_unconfirmed_warned: false,
            lod_resend: std::collections::BTreeSet::new(),
            lod_release: std::collections::VecDeque::new(),
            client_id: None,
            home_region: None,
            window_center: None,
            subscribed: std::collections::BTreeSet::new(),
            lod_assigned: BTreeMap::new(),
            lod_table: game::lod::LOD_RING_TABLE,
            near_radius: 1,
            fortress_radius: 0,
            fortress_region: None,
            lod_subscribed: std::collections::BTreeSet::new(),
            join: JoinTimeline::default(),
            pending_events: BTreeMap::new(),
            last_hud_status: None,
            ui_update_send,
            prev_hotbar: game::HotbarView::default(),
            prev_inventory_view: game::InventoryView::default(),
            prev_mining: None,
            prev_play_mode: game::PlayMode::default(),
            last_cursor_grab: true,
            debug_flags,
            recon: ReconMetrics::default(),
            #[cfg(not(target_arch = "wasm32"))]
            perf: PerfBudget::default(),
            #[cfg(not(target_arch = "wasm32"))]
            tick_times: TickTimes::default(),
            #[cfg(not(target_arch = "wasm32"))]
            crossing: CrossingProbe::default(),
        }
    }

    /// Kick off the connection handshake: ask the server which region the
    /// player belongs to. Both the native thread loop and the wasm frame
    /// driver call this exactly once before their first pump/select.
    pub fn start(&mut self) {
        self.send_out(Outgoing::Home(ClientPacket::RequestPlayerRegion));
    }

    /// Open connections to other hosts of the world with `opener` (a transport that
    /// can: native netcode, the multi-host harness). Without one, a region another
    /// host serves is not loaded.
    pub fn set_conn_opener(&mut self, opener: Box<dyn ConnOpener + Send>) {
        self.opener = Some(opener);
    }

    /// Send `out` where it goes, behind everything the region runners queued
    /// before it (inline, they sent it first).
    fn send_out(&mut self, out: Outgoing) {
        self.flush_outgoing();
        self.conns.route(out);
    }

    /// Route everything queued on `out_send` (the region runners' predictions).
    fn flush_outgoing(&mut self) {
        while let Ok(out) = self.out_recv.try_recv() {
            self.conns.route(out);
        }
    }

    /// The host of the connection that serves `rc` (harness hook; `None` before the
    /// home named itself).
    pub fn region_host(&self, rc: RegionCoords) -> Option<u32> {
        self.conns.ordinal_of(self.conns.connection_of(rc))
    }

    /// The home connection's host (harness hook; `None` before it named itself).
    pub fn home_host(&self) -> Option<u32> {
        self.conns.home_ordinal()
    }

    /// The hosts this session holds a connection to, by connection key (harness
    /// hook).
    pub fn connection_hosts(&self) -> Vec<(u32, Option<u32>)> {
        self.conns.keys().into_iter().map(|key| (key, self.conns.ordinal_of(key))).collect()
    }

    /// Override how the initial connect is retried (default in production).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_connect_policy(&mut self, policy: crate::netcode::ConnectPolicy) {
        self.connect_policy = policy;
    }

    /// Identify with this credential instead of the local player name
    /// (a join JWT from the login service).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_credential(&mut self, credential: Option<game::Credential>) {
        self.credential = credential;
    }

    /// Take UI requests (`ClientRequest`) from this channel.
    pub fn set_request_recv(&mut self, request_recv: Receiver<ClientRequest>) {
        self.request_recv = request_recv;
    }

    /// Forward one UI request to the server.
    fn handle_request(&mut self, request: ClientRequest) {
        match request {
            ClientRequest::PlayRegion(rc) => {
                info!("[client] requesting Play in region {rc:?}");
                self.send_out(Outgoing::Home(ClientPacket::PlayRegion(rc)));
            }
            ClientRequest::RequestEmbarkOptions => {
                self.send_out(Outgoing::Home(ClientPacket::RequestEmbarkOptions));
            }
            ClientRequest::Embark(req) => {
                info!("[client] embarking in region {:?}", req.region);
                self.send_out(Outgoing::Home(ClientPacket::Embark(req)));
            }
            ClientRequest::RequestLegends(q) => {
                self.send_out(Outgoing::Home(ClientPacket::RequestLegends(q)));
            }
            ClientRequest::StartAdventure(site) => {
                self.send_out(Outgoing::Home(ClientPacket::StartAdventure(site)));
            }
            ClientRequest::EndAdventure => {
                self.send_out(Outgoing::Home(ClientPacket::EndAdventure));
            }
            ClientRequest::Retire => {
                self.send_out(Outgoing::Home(ClientPacket::Retire));
            }
            ClientRequest::ReturnTo(site) => {
                self.send_out(Outgoing::Home(ClientPacket::ReturnTo(site)));
            }
            ClientRequest::RequestPersonInfo(person) => {
                self.send_out(Outgoing::Home(ClientPacket::RequestPersonInfo(person)));
            }
            ClientRequest::Leave => {
                info!("[client] leaving the world");
                self.send_out(Outgoing::Home(ClientPacket::Leave));
            }
        }
    }

    /// Inject a client-side tick (wasm frame driver replaces the native
    /// tick-generator thread with this).
    pub fn send_tick(&self) {
        let _ = self.game_event_send.send(GameEventKind::Tick);
    }

    /// Current adaptive tick interval in milliseconds.
    pub fn tick_rate_ms(&self) -> u64 {
        self.tick_rate.load(Ordering::SeqCst)
    }

    /// The channel the first (login, home) connection's transport — quinn on
    /// native, WebTransport on wasm, LocalServer in the harnesses — reads its
    /// outgoing ClientPackets from. Connections to other hosts get their own
    /// (`ConnOpener`).
    pub fn client_packet_recv(&self) -> Receiver<ClientPacket> {
        self.first_conn_recv.clone()
    }

    /// Override the LOD ring table (test/harness hook): headless suites pay
    /// every requested key as a synchronous server-side build, so they shrink
    /// (or empty) the table unless LOD is what they're testing.
    pub fn set_lod_table(&mut self, table: &'static [(i32, u32)]) {
        self.lod_table = table;
    }

    /// Shrink the near window (test/harness hook). Radius 0 subscribes to the
    /// home region alone instead of the production 3x3, cutting the world a
    /// headless test pays for — on both sides of the channel — by ~9x.
    pub fn set_near_radius(&mut self, radius: i32) {
        self.near_radius = radius;
    }

    /// Apply the binary's resolved world-size knobs (`VOXEL_WORLD` & co.).
    /// Call before `start()`: the join burst reads both halves.
    pub fn set_world_scale(&mut self, scale: crate::world_scale::WorldScale) {
        info!("[client] world scale: {scale}");
        self.lod_table = scale.lod_table;
        self.near_radius = scale.near_radius;
        self.fortress_radius = scale.fortress_radius;
    }

    /// Simulate `radius` regions around the fortress (test/harness hook; the
    /// binaries take it from `WorldScale` via `set_world_scale`).
    pub fn set_fortress_radius(&mut self, radius: i32) {
        self.fortress_radius = radius;
    }

    /// Push a game event as if it came from local input (test/harness hook).
    pub fn push_game_event(
        &self,
        ev: GameEventKind,
    ) -> Result<(), crossbeam::channel::SendError<GameEventKind>> {
        self.game_event_send.send(ev)
    }

    /// Read-only access to the loaded sim world (test/harness hook).
    pub fn world_ref(&self) -> &game::World {
        self.host.world().expect("world_ref needs the inline region host")
    }

    /// Mutable access to the loaded sim world (test/harness hook — state
    /// surgery like teleports in convergence tests).
    pub fn world_mut(&mut self) -> &mut game::World {
        self.host.world_mut().expect("world_mut needs the inline region host")
    }

    /// Mispredictions over the whole session, every region (test/harness
    /// hook).
    pub fn total_mispredictions(&self) -> u64 {
        self.recon.total_mispredictions
    }

    /// [`Self::total_mispredictions`] by `[predicted kind][server kind]`
    /// (`game::ReconcileStats::kind_slot`; test/harness hook).
    pub fn total_mispredictions_by_kind(&self) -> [[u64; 3]; 3] {
        self.recon.total_by_kinds
    }

    /// How many LOD keys this session holds subscribed (test/harness hook).
    pub fn lod_subscriptions(&self) -> usize {
        self.lod_subscribed.len()
    }

    /// Home-region corrections over the whole session: how many, and the largest
    /// (world units) (test/harness hook).
    pub fn corrections(&self) -> (u32, f32) {
        (self.recon.total_corrections, self.recon.total_correction_max)
    }

    /// The client's current home region (test/harness hook).
    pub fn home_region(&self) -> RegionCoords {
        self.home_region.expect("home region set")
    }

    /// True once every buffered server event has been reconciled into a
    /// loaded region (no snapshot still in flight). Test/harness hook.
    pub fn pending_events_empty(&self) -> bool {
        self.pending_events.values().all(|v| v.is_empty())
    }

    /// Handle one client-side game event. Returns Ok(false) if the game
    /// should quit. Events arriving before the first region loads are
    /// dropped, matching the pre-refactor select! loop.
    fn handle_game_event(&mut self, event: GameEventKind) -> Result<bool, GameError> {
        if let GameEventKind::Quit = event {
            // Honoured before the first region too (leaving while the world
            // is still loading). Requests queued before the Quit (the
            // `Leave` the UI sends first) go out before the session ends:
            // they arrive on another channel, so `select!` may see the Quit
            // first.
            while let Ok(request) = self.request_recv.try_recv() {
                self.handle_request(request);
            }
            return Ok(false);
        }
        if !self.world_started {
            return Ok(true);
        }
        match event {
            GameEventKind::Quit => return Ok(false),
            GameEventKind::Tick => {
                self.conn_clock_ms += game::TICK_RATE;
                self.retry_observer_regions();
                self.maintain_connections();
                #[cfg(not(target_arch = "wasm32"))]
                let tick_start = Instant::now();
                let Some(target) = self.clock.advance() else {
                    return Ok(true);
                };
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(home) = self.home_region {
                    self.crossing.home_tick(self.held_ticks(home, target) > 0);
                }
                self.host.broadcast(|| RegionCmd::Tick(target));
                #[cfg(not(target_arch = "wasm32"))]
                self.perf.record_tick(tick_start.elapsed());
                // The ticks' transfers are routed here (`drain_reports`), and
                // a predicted home flip must land before update_window recenters.
                self.drain_reports()?;
                self.update_window();
                self.update_lod_window();
                self.project_ui();
                self.emit_collider_debug_lines();
            }
            GameEventKind::PlayerInput(_, _) if !self.is_caught_up => {
                // Don't handle player events until the sim has caught up with
                // the server (join/replay window). `ready` deliberately does
                // NOT gate input: it flaps with SyncClock sampling jitter
                // (diff of ±1 tick at rtt≈0) and its job is tuning the
                // adaptive tick rate, not input admission — gating on it ate
                // a 500ms window of keypresses per flap (261:1 measured in
                // the browser).
            }
            e @ GameEventKind::PlayerInput(_, _) => {
                if let Some(home) = self.home_region {
                    // The home region predicts it and sends it to the server
                    // itself. A home not loaded yet (the body crossed into a
                    // region outside the window; desired_window pins it, so
                    // its snapshot is on the way) cannot predict it, but the
                    // server must still get it — a lost key release would
                    // leave the key held there.
                    if !self.host.contains(&home) {
                        self.send_unpredicted(home, e);
                        return Ok(true);
                    }
                    self.host.send(home, RegionCmd::Predict(e));
                }
            }
            kind @ (GameEventKind::CreateClient(_)
            | GameEventKind::CreateClientAt(..)
            | GameEventKind::ClientLeft(_)
            | GameEventKind::ParkPlayer(_)
            | GameEventKind::RestorePlayer(..)
            | GameEventKind::SpawnPerson(_)
            | GameEventKind::DespawnPerson(_)
            | GameEventKind::WalkOut(..)) => {
                // Players (and people) are created (and moved) by the server: on
                // connection, and when Play moves a session.
                warn!("ignoring locally-originated {kind:?}");
            }
            // Transfer events are relayed into regions directly (server) or
            // client-predicted (later task); never locally-originated here.
            GameEventKind::EntityArrived(_)
            | GameEventKind::HaloEntity(_)
            | GameEventKind::HaloEntityGone(_)
            | GameEventKind::HaloSourceGone(_)
            | GameEventKind::HaloUpdate(_)
            | GameEventKind::Intent(_) => {}
            GameEventKind::UiAction(_, _) | GameEventKind::Order(_, _) if !self.is_caught_up => {
                // Same admission rule as PlayerInput: drop until the sim has
                // caught up with the server (join/replay window).
            }
            e @ (GameEventKind::UiAction(_, _) | GameEventKind::Order(_, _)) => {
                if let Some(home) = self.home_region {
                    if !self.host.contains(&home) {
                        self.send_unpredicted(home, e);
                        return Ok(true);
                    }
                    self.host.send(home, RegionCmd::Predict(e));
                }
            }
        }
        Ok(true)
    }

    /// One of this client's inputs (or orders) for a home region it does not
    /// predict (not loaded yet): sent to the server unpredicted, stamped with
    /// the world clock's tick like a predicted one, so the server applies it
    /// in that slot. The server routes it by the player's home, not by the
    /// stamp's region; the region's snapshot, when it arrives, holds its effect.
    fn send_unpredicted(&mut self, home: RegionCoords, kind: GameEventKind) {
        log::info!("[client] {kind:?} for home {home:?}, not loaded: sent to the server unpredicted");
        let event = game::GameEvent::new(kind, 0, home);
        let packet = match self.clock.tick() {
            Some(tick) => ClientPacket::GameEventAfterTick(event, tick),
            None => ClientPacket::GameEvent(event),
        };
        self.send_out(Outgoing::Home(packet));
    }

    /// Route every report the regions produced since the last drain, until
    /// none is left (routing an arrival can produce more). Each `Ticked`
    /// report's transfers are relayed as it is routed, on either host, and
    /// land in their target at the source's stamp. A region's failure ends
    /// the session, as the inline `?` did.
    fn drain_reports(&mut self) -> Result<(), GameError> {
        loop {
            let batch = self.host.drain();
            if !batch.is_empty() {
                self.route_reports(batch)?;
                continue;
            }
            return Ok(());
        }
    }

    /// Apply one world-clock decision: the tick rate, `ready`, the metrics
    /// (the step itself is already on the clock).
    fn on_clock_decision(&mut self, d: crate::regions::clock::ClockDecision) {
        self.recon.last_drift_ms = d.drift_ms;
        let (rate, ready) = d.rate;
        self.ready = ready;
        self.tick_rate.store(rate, Ordering::SeqCst);
        if let Some((_error_ms, ticks)) = d.step.filter(|(_, t)| *t > 0) {
            self.recon.align_steps += 1;
            self.recon.align_ticks += ticks;
            #[cfg(not(target_arch = "wasm32"))]
            self.crossing.align_home(_error_ms, ticks);
        }
    }

    /// Ticks region `id` sits out before the clock (at `target`) reaches it.
    #[cfg(not(target_arch = "wasm32"))]
    fn held_ticks(&self, id: RegionId, target: game::Tick) -> usize {
        self.region_ticks.get(&id).map_or(0, |t| t.saturating_sub(target) as usize)
    }

    /// Run every region on its own thread (native play), like the server.
    /// Call before the first region snapshot arrives.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn use_region_threads(&mut self) {
        assert!(!self.world_started, "use_region_threads after a region was loaded");
        self.host = crate::regions::host::RegionHost::threaded();
    }

    /// Whether the regions run on their own threads.
    pub fn region_threads(&self) -> bool {
        self.host.is_threaded()
    }

    /// Wait until every region has run every command sent to it, routing
    /// what they report, until routing sends nothing new (harness hook: a
    /// lockstep step on threads).
    pub fn settle_regions(&mut self) -> Result<(), GameError> {
        loop {
            self.host.settle();
            let batch = self.host.drain();
            // `settle` itself answers with a `Pong` per region: a batch of
            // nothing else means routing queued no new work.
            let quiet = batch.iter().all(|(_, r)| matches!(r, RegionReport::Pong(_)));
            self.route_reports(batch)?;
            if quiet {
                return Ok(());
            }
        }
    }

    /// Every loaded region's `(tick, state hash)`, asked of the regions
    /// themselves (harness hook: works on either host).
    pub fn region_probe(&mut self) -> Result<BTreeMap<RegionId, (game::Tick, u32)>, GameError> {
        self.probe.clear();
        self.host.broadcast(|| RegionCmd::StateHash);
        self.settle_regions()?;
        Ok(std::mem::take(&mut self.probe))
    }

    /// The regions currently held (harness hook).
    pub fn loaded_regions(&self) -> Vec<RegionId> {
        self.host.ids()
    }

    /// The home region's tick as it last reported it (harness hook).
    pub fn home_tick(&self) -> Option<game::Tick> {
        self.home_view().map(|v| v.tick)
    }

    /// Route one batch of reports (each tick's transfers as its `Ticked`
    /// report comes), then the batch's forwarded inputs.
    fn route_reports(&mut self, batch: Vec<(RegionId, RegionReport)>) -> Result<(), GameError> {
        {
            // Re-sent once this batch's departures have flipped home.
            let mut forwarded = Vec::new();
            for (id, report) in batch {
                let is_home = self.home_region == Some(id);
                match report {
                    RegionReport::Ticked {
                        sat_out,
                        departures: d,
                        halo_entities: g,
                        halo: h,
                        intents,
                        stamp,
                        #[cfg(not(target_arch = "wasm32"))]
                        perf,
                        #[cfg(not(target_arch = "wasm32"))]
                        took,
                    } => {
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            self.perf.sim.entry(id).or_default().merge(&perf);
                            if !sat_out {
                                self.tick_times.record(id, took);
                            }
                        }
                        let _ = sat_out; // read natively only (tick times)
                        self.region_ticks.insert(id, stamp);
                        // This tick's transfers, now (both hosts).
                        self.route_transfers(id, d, g, h, stamp);
                        self.route_intents(intents);
                    }
                    RegionReport::Reconciled {
                        stats,
                        correction_m,
                        #[cfg(not(target_arch = "wasm32"))]
                        took,
                    } => {
                        self.recon.record(stats);
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            self.perf.reconcile_total += took;
                        }
                        if is_home && stats.mispredictions > 0 {
                            self.recon.home_mispredictions += stats.mispredictions;
                            if let Some(moved) = correction_m {
                                self.recon.corrections += 1;
                                self.recon.correction_sum += moved;
                                self.recon.correction_max = self.recon.correction_max.max(moved);
                                self.recon.total_corrections += 1;
                                self.recon.total_correction_max = self.recon.total_correction_max.max(moved);
                                #[cfg(not(target_arch = "wasm32"))]
                                self.crossing.correction(moved);
                            }
                        }
                    }
                    RegionReport::View(Some(view)) => {
                        self.views.insert(id, view);
                    }
                    RegionReport::View(None) => {
                        self.views.remove(&id);
                    }
                    RegionReport::Forward(kind) => forwarded.push((id, kind)),
                    RegionReport::Pong(_) => {}
                    RegionReport::StateHash { tick, hash } => {
                        self.probe.insert(id, (tick, hash));
                    }
                    RegionReport::Failed(e) => {
                        warn!("region {id:?} failed: {e:?}");
                        return Err(e);
                    }
                }
            }
            // Inputs follow the body: the new home holds them behind the
            // arrival if that is still held (the server's region forwards its
            // own late ones the same way).
            for (id, kind) in forwarded {
                match self.home_region {
                    Some(home) if home != id && self.host.contains(&home) => {
                        self.host.send(home, RegionCmd::Predict(kind));
                    }
                    // The new home is not loaded: the server still gets it.
                    Some(home) if home != id => self.send_unpredicted(home, kind),
                    _ => warn!("dropping {kind:?}: predicted in {id:?} after the player left it"),
                }
            }
        }
        Ok(())
    }

    /// Mirror of the server manager's relay, on the predicted timeline:
    /// inject one tick's transfers from `source` into loaded sibling regions
    /// as predicted events, stamped like the server's `Transfer`s so each
    /// lands in the same slot (`stamp` = the source's tick after that tick).
    /// Targets outside the local window are dropped — their authoritative
    /// streams cover them.
    fn route_transfers(
        &mut self,
        source: RegionCoords,
        departures: Vec<(game::EntityBundle, RegionCoords)>,
        halo_entities: Vec<(game::HaloEntityMsg, RegionCoords)>,
        halo: Vec<(game::HaloUpdate, RegionCoords)>,
        stamp: game::Tick,
    ) {
        for (bundle, target) in departures {
            let is_local_player = bundle.client.as_ref().map(|(c, _)| *c) == self.client_id
                && self.client_id.is_some();
            if self.host.contains(&target) {
                let mut b = bundle;
                b.rebase(source, target);
                self.host.send(target, RegionCmd::Arrival(b, stamp));
            }
            if is_local_player {
                // Predicted home flip: reroute input now; the server's
                // PlayerRegion push confirms (or corrects) it.
                #[cfg(not(target_arch = "wasm32"))]
                if self.home_region != Some(target) {
                    let stalled = self.clock.tick().map_or(0, |t| self.held_ticks(target, t));
                    let hosts = (self.region_host(source), self.region_host(target));
                    self.crossing.flip(source, target, stalled, hosts);
                }
                self.home_region = Some(target);
            }
        }
        for (msg, target) in halo_entities {
            if self.host.contains(&target) {
                let mut m = msg;
                m.rebase(source, target);
                self.host.send(target, RegionCmd::HaloEntity(m, stamp));
            }
        }
        for (update, target) in halo {
            if self.host.contains(&target) {
                self.host.send(target, RegionCmd::Halo(update, stamp));
            }
        }
    }

    /// Mirror of the server manager's intent routing, on the predicted
    /// timeline: each intent or reply to the loaded region it is addressed to
    /// (a position target's owner, a reply's `hint` — the client has no
    /// location directory), held there to its due tick. Others — an owner
    /// outside the local window, an intent to an entity — are dropped: the
    /// authoritative streams cover them (an owner the server does not run
    /// answers with a refusal the sender's stream brings).
    fn route_intents(&mut self, intents: Vec<game::IntentMsg>) {
        for msg in intents {
            match msg.region_hint() {
                Some(target) if self.host.contains(&target) => {
                    self.host.send(target, RegionCmd::Intent(msg));
                }
                _ => log::debug!("[client] predicted {msg:?} not relayed: its region is not loaded here"),
            }
        }
    }

    /// Drain all pending server packets, then all pending game events,
    /// without blocking. Returns Ok(false) once Quit has been consumed.
    pub fn pump(&mut self, incoming: &Receiver<(u32, ConnEvent)>) -> Result<bool, GameError> {
        let going_on = self.pump_inner(incoming);
        self.flush_outgoing();
        going_on
    }

    /// [`Self::pump`] for a transport with one connection (`LocalServer` harnesses):
    /// every packet is the login connection's.
    pub fn pump_packets(&mut self, packets: &Receiver<ServerPacket>) -> Result<bool, GameError> {
        let (send, incoming) = crossbeam::channel::unbounded();
        for packet in packets.try_iter() {
            let _ = send.send((crate::connections::LOGIN_CONN, ConnEvent::Packet(packet)));
        }
        self.pump(&incoming)
    }

    fn pump_inner(&mut self, incoming: &Receiver<(u32, ConnEvent)>) -> Result<bool, GameError> {
        self.drain_reports()?;
        while let Ok((conn, event)) = incoming.try_recv() {
            self.on_conn_event(conn, event)?;
            self.drain_reports()?;
        }
        while let Ok(event) = self.game_event_recv.try_recv() {
            if !self.handle_game_event(event)? {
                return Ok(false);
            }
            self.drain_reports()?;
        }
        while let Ok(request) = self.request_recv.try_recv() {
            self.handle_request(request);
        }
        self.emit_hud_status();
        // Surface the metrics/budget lines from pump()-driven harnesses too
        // (soak test); on wasm there is no Instant, so no reporting.
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.maybe_report_metrics();
            self.crossing.maybe_report();
            self.maybe_report_tick_times();
        }
        Ok(true)
    }

    /// Build render-bridge + local-world state for a freshly received region
    /// snapshot. Hoisted out of a closure (was `new_region` inline) so
    /// `handle_server` can call `drop_region` first without fighting the
    /// borrow checker over `self`.
    fn new_region(&mut self, id: RegionId, raw_game_data: Rollback) {
        let Some(client_id) = self.client_id else {
            // PlayerRegion always precedes region snapshots; a region built
            // without our id would act as the server (cell-hash checks off).
            log::error!("region {id:?} snapshot before PlayerRegion: dropped");
            return;
        };
        let ctx = crate::regions::runner::RunnerCtx {
            id,
            client_id,
            server_send: self.out_send.clone(),
            client_event_send: self.client_event_send.clone(),
            debug_flags: self.debug_flags.clone(),
        };
        // The first snapshot starts the clock at its world tick; every
        // region, this one too, runs to the clock from the next tick on.
        self.clock.start_at(*raw_game_data.tick);
        // The region is built where it runs (its thread, when threaded): the
        // bridge's `NewRegion` copy and the `Region` itself.
        self.host.spawn(ctx, raw_game_data, self.route_map.clone());
        self.world_started = true;
        info!("Region recieved and loaded!");
    }

    /// The viewer's current region, from the local player's sim body in the
    /// home region (region-local pose + home offset). Free-cam local coords
    /// run past the region bounds on purpose; from_world floor-divides them
    /// into the right neighbour. Falls back to home when the player entity
    /// isn't readable yet.
    fn viewer_region(&self) -> Option<RegionCoords> {
        let home = self.home_region?;
        Some(match self.viewer_world_xz() {
            Some((x, z)) => RegionCoords::from_world(x, z),
            None => home,
        })
    }

    /// The viewer's world-space (x, z): the local player's sim body in the
    /// home region, rebased by home's offset. `None` until that body exists.
    fn viewer_world_xz(&self) -> Option<(f32, f32)> {
        let home = self.home_region?;
        let t = self.local_player_translation()?;
        let off = home.world_offset();
        Some((t[0] + off[0], t[2] + off[2]))
    }

    /// The home region's view of the local client (`None` until it holds it).
    fn home_view(&self) -> Option<&crate::regions::runner::HomeView> {
        self.views.get(&self.home_region?)
    }

    /// The simulated set for a window centred on `center` (see
    /// [`desired_window`]); `update_window` and `update_lod_window` share it,
    /// so streamed LOD never covers a simulated region.
    fn window_for(&self, center: RegionCoords) -> std::collections::BTreeSet<RegionCoords> {
        let mut window = desired_window(
            center,
            self.home_region,
            self.near_radius,
            self.fortress_region,
            self.fortress_radius,
        );
        // A move's target, until its `PlayerRegion` makes it the home.
        window.extend(self.pending_move);
        window
    }

    /// Centre of the near window and the LOD rings: the viewer's region, but
    /// sticky — it moves only once the viewer is [`WINDOW_RECENTER_MARGIN_M`]
    /// into another region (`RegionCoords::recentered`). Without it every
    /// crossing of a seam released and re-requested a whole row, and going back
    /// and forth over one reloaded five regions per crossing
    /// (`just shuttle-probe`, 2026-09-23). Updated once per tick by
    /// `update_window`; `update_lod_window` reads the same value, so the near
    /// window and the rings' exclusion never disagree.
    fn advance_window_center(&mut self) -> Option<RegionCoords> {
        let center = match self.viewer_world_xz() {
            Some((x, z)) => Some(RegionCoords::recentered(
                self.window_center,
                x,
                z,
                window_recenter_margin(),
            )),
            // No readable body yet (join) or mid-handoff: keep what we have.
            None => self.window_center.or(self.home_region),
        };
        self.window_center = center;
        center
    }

    /// Local player's body translation in its home region's local space
    /// (region-local; enough for movement-delta assertions). `None` until the
    /// player entity + body exist in a loaded home region. Test/harness hook.
    pub fn local_player_translation(&self) -> Option<[f32; 3]> {
        self.home_view()?.translation
    }

    /// While the debug flag is on: wireframe every collider near the viewer,
    /// one buffer per loaded region, and ship them to the render bridge.
    /// Lines are region-local; the bridge adds each region's world offset.
    /// Runs after the tick (read-only), so it never touches the rollback log.
    fn emit_collider_debug_lines(&mut self) {
        if !self.debug_flags.collider_wireframes() {
            return;
        }
        // Viewer = local player's body, rebased to world space via home.
        let (Some(local), Some(home)) = (self.local_player_translation(), self.home_region) else {
            // no player body yet: no lines beats wrong lines — the render side
            // keeps drawing the last buffers until the body is readable again
            // (debug-only, self-healing).
            return;
        };
        let hoff = home.world_offset();
        let world_pos = [local[0] + hoff[0], local[1] + hoff[1], local[2] + hoff[2]];
        // Each region collects its own lines (region-local) and ships them.
        self.host.broadcast(|| RegionCmd::DebugLines(world_pos));
    }

    /// The local player's fps-cam toggle state in its home region, if the
    /// client + home region are readable yet. `None` before the handshake
    /// settles. Test/harness hook.
    pub fn local_player_fps_cam_mode(&self) -> Option<bool> {
        Some(self.home_view()?.fps_cam_mode)
    }

    /// Current connection/sync state for the HUD, from the ready flags.
    fn connection_state(&self) -> game::ConnectionState {
        if !self.ready {
            game::ConnectionState::Connecting
        } else if !self.is_caught_up {
            game::ConnectionState::CatchingUp
        } else {
            game::ConnectionState::Ready
        }
    }

    /// Push region + connection status to the render bridge, but only when it
    /// changed since the last push (edge-triggered). No-op until we know our
    /// home region and can resolve the viewer region.
    fn emit_hud_status(&mut self) {
        let (Some(home), Some(viewer)) = (self.home_region, self.viewer_region()) else {
            return;
        };
        let conn = self.connection_state();
        if conn == game::ConnectionState::Ready && !self.join.connection_ready {
            // First time clock-synced AND caught up on the event stream.
            self.join.connection_ready = true;
            game::perf_event!("connection_ready", "home={},{}", home.x, home.z);
        }
        let snapshot = (home, viewer, conn);
        if self.last_hud_status == Some(snapshot) {
            return;
        }
        self.last_hud_status = Some(snapshot);
        let _ = self.client_event_send.send(ClientUpdateEvent::HudStatus {
            home_region: home,
            viewer_region: viewer,
            connection: conn,
        });
    }

    /// Client-only: project the local player's authoritative data into a widget
    /// tree, diff against the last emitted tree, and push widget deltas to the
    /// ui_bridge. Runs once per tick after the sim has settled. No-op until the
    /// local player + home region are known and loaded.
    fn project_ui(&mut self) {
        let (Some(home), Some(_)) = (self.home_region, self.client_id) else {
            return;
        };
        if !self.host.contains(&home) {
            return;
        }
        // No view = the client is not in the home region's data right now.
        let view = self.views.get(&home);
        let hotbar = view.map(|v| v.hotbar.clone()).unwrap_or_default();
        if hotbar != self.prev_hotbar {
            let _ = self.ui_update_send.send(game::UiUpdate::Hotbar(hotbar.clone()));
            self.prev_hotbar = hotbar;
        }
        if let Some(v) = view {
            let screen = game::build_inventory_view(v.inventory_open, &v.inventory, &v.cursor, v.container.as_ref());
            if screen != self.prev_inventory_view {
                let _ = self.ui_update_send.send(game::UiUpdate::Inventory(screen.clone()));
                self.prev_inventory_view = screen;
            }
            let mining = v.mining.map(|m| m.progress_per_mille);
            if mining != self.prev_mining {
                let _ = self.ui_update_send.send(game::UiUpdate::Mining(mining));
                self.prev_mining = mining;
            }
        }
        // Mid-handoff the client can be briefly absent from the home region's
        // data; reading that as Fortress would flicker the body hidden, so
        // keep the last mode until it is back.
        if let Some(mode) = view.map(|v| v.mode) {
            if mode != self.prev_play_mode {
                self.prev_play_mode = mode;
                let _ = self.ui_update_send.send(game::UiUpdate::PlayMode(mode));
            }
        }

        // Cursor grab follows inventory-open: released while a menu is open.
        let want_grab = !view.is_some_and(|v| v.inventory_open);
        if want_grab != self.last_cursor_grab {
            let _ = self
                .ui_update_send
                .send(game::UiUpdate::SetCursorGrab(want_grab));
            self.last_cursor_grab = want_grab;
        }
    }

    /// Diff the desired window against current subscriptions; request
    /// the new, release the stale, and tear stale regions out of the local
    /// world + render bridge. Cheap when nothing changed (set compare).
    fn update_window(&mut self) {
        let Some(center) = self.advance_window_center() else {
            return;
        };
        let desired = self.window_for(center);
        if desired == self.subscribed {
            return;
        }
        let wanted: Vec<RegionCoords> = desired.difference(&self.subscribed).copied().collect();
        for rc in wanted {
            // The home, unless another host already serves it (it answers with
            // `RegionElsewhere` for a region another host owns).
            self.request_region(rc);
        }
        let stale: Vec<RegionCoords> = self.subscribed.difference(&desired).copied().collect();
        for rc in stale {
            self.release_region(rc);
        }
        self.subscribed = desired;
    }

    /// The home's `RegionElsewhere` for `region` (`ConnectionSet::on_region_named` acts on
    /// it). One no longer wanted that answers a subscribe is late, as is one naming the home
    /// (a move comes as `PlayerRegion`); an unasked one naming another host is a move — the
    /// home moved the player to that host's region (a Play, an embark there): subscribed
    /// there and held in the window until that host's `PlayerRegion` makes it the home
    /// (only for [`AMBIGUOUS_MOVE_HOLD_MS`] when the region was just released while another
    /// host served it: the home's late re-answer reads the same).
    fn region_elsewhere(&mut self, region: RegionCoords, endpoint: game::HostEndpoint, ticket: game::ObserverTicket) {
        let asked = self.answered_home(region);
        let wanted = self.subscribed.contains(&region);
        let ordinal = endpoint.ordinal;
        if !wanted && (asked || Some(ordinal) == self.conns.home_ordinal()) {
            log::debug!("[client] {region:?} is host {ordinal}'s, but no longer wanted");
            return;
        }
        let opener = self.opener.as_deref_mut().map(|o| o as &mut dyn ConnOpener);
        match self.conns.on_region_named(region, endpoint, ticket, opener) {
            Named::Home => {
                // Asked of the home like any subscribe through it (`request_region`).
                info!("[client] {region:?} is served by the home (host {ordinal}) now");
                let asked = self.asked_home.entry(region).or_insert((0, 0));
                *asked = (asked.0 + 1, self.conn_clock_ms);
            }
            Named::Observer(_) => {
                self.asked_observer.insert(region, self.conn_clock_ms);
                if !wanted {
                    info!("[client] moved to {region:?}, host {ordinal}'s");
                    self.pending_move = Some(region);
                    self.subscribed.insert(region);
                    self.ambiguous_move = self
                        .released_elsewhere
                        .contains_key(&region)
                        .then_some((region, self.conn_clock_ms + AMBIGUOUS_MOVE_HOLD_MS));
                } else {
                    info!("[client] {region:?} is served by host {ordinal}");
                }
            }
            Named::Unrouted => {}
        }
    }

    /// `rc` left the window: released where it is served — and at the home too when
    /// another host serves it (the home remembers where it sent the client for it, and
    /// answers again after a succession of that host, R4, until it is released there) —
    /// and dropped here. The caller takes it out of `subscribed`.
    fn release_region(&mut self, rc: RegionCoords) {
        let served_elsewhere = self.conns.connection_of(rc) != self.conns.home();
        self.send_out(Outgoing::Region(rc, ClientPacket::ReleaseRegionConnection(rc)));
        if served_elsewhere {
            self.send_out(Outgoing::Home(ClientPacket::ReleaseRegionConnection(rc)));
            self.released_elsewhere.insert(rc, self.conn_clock_ms);
        }
        self.conns.release(rc);
        self.observer_retry.remove(&rc);
        self.drop_region(rc);
    }

    /// Remove a region from the local world and the render bridge.
    fn drop_region(&mut self, rc: RegionCoords) {
        self.pending_events.remove(&rc);
        self.views.remove(&rc);
        self.region_ticks.remove(&rc);
        // The host takes it off the render bridge (after all it sent there).
        self.host.stop(rc);
    }

    /// Diff the desired LOD key set against current subscriptions. Runs
    /// right after `update_window`, so the near-window exclusion set it
    /// subtracts is already current — streamed LOD and simulated regions
    /// never cover the same region.
    fn update_lod_window(&mut self) {
        let Some(center) = self.window_center.or_else(|| self.viewer_region()) else {
            return;
        };
        let excluded = self.window_for(center);
        let (assigned, desired) = desired_lod(
            center,
            &excluded,
            &self.lod_assigned,
            self.lod_table,
            game::lod::LOD_STICKY_BAND,
        );
        self.lod_assigned = assigned;
        // A former home lets go of the keys it held, paced like the requests.
        let mut released = 0;
        while released < LOD_REQUESTS_PER_TICK {
            let Some((conn, key)) = self.lod_release.pop_front() else { break };
            if self.conns.is_live(conn) {
                self.conns.route(Outgoing::Conn(conn, ClientPacket::ReleaseLod(key)));
                released += 1;
            }
        }
        // Keys held since before the home moved hosts: asked of the new home again,
        // paced like new ones (and ahead of them).
        self.lod_resend.retain(|key| desired.contains(key));
        let resend: Vec<game::lod::LodKey> = self.lod_resend.iter().take(LOD_REQUESTS_PER_TICK).copied().collect();
        for key in &resend {
            self.lod_resend.remove(key);
            self.send_out(Outgoing::Home(ClientPacket::RequestLod(*key)));
        }
        if desired == self.lod_subscribed {
            if !self.join.lod_requests_done && !desired.is_empty() {
                // The paced RequestLod diff has fully drained for the first time.
                self.join.lod_requests_done = true;
                game::perf_event!(
                    "lod_requests_done",
                    "keys={} per_tick={LOD_REQUESTS_PER_TICK}",
                    desired.len()
                );
            }
            return;
        }
        let stale: Vec<game::lod::LodKey> =
            self.lod_subscribed.difference(&desired).copied().collect();
        for key in stale {
            self.send_out(Outgoing::Home(ClientPacket::ReleaseLod(key)));
            let near_takeover = key.regions().iter().any(|rc| excluded.contains(rc));
            let _ = self
                .client_event_send
                .send(ClientUpdateEvent::RemoveLod { key, near_takeover });
            self.lod_subscribed.remove(&key);
        }
        // Paced: the rest of the diff is picked up by the following ticks.
        // `LodKey` orders by factor first, and factors grow with ring
        // distance, so the set's own order is nearest-first.
        let wanted: Vec<game::lod::LodKey> = desired
            .difference(&self.lod_subscribed)
            .take(LOD_REQUESTS_PER_TICK - resend.len())
            .copied()
            .collect();
        for key in wanted {
            self.send_out(Outgoing::Home(ClientPacket::RequestLod(key)));
            self.lod_subscribed.insert(key);
        }
    }

    /// Route one server-authoritative game event to the region it targets:
    /// reconcile there (with the server's state hash, if it sent one) when
    /// loaded, buffer if the snapshot is still in flight, drop if the region
    /// was released/never wanted.
    fn route_server_game_event(&mut self, event: GameEvent, hash: Option<(game::Tick, u32)>) {
        if self.host.contains(&event.region_id) {
            self.host.send(event.region_id, RegionCmd::ServerEvent(event, hash));
            return;
        }
        if hash.is_some() {
            // Not held yet (snapshot in flight): its snapshot bakes it in.
            log::debug!("state hash for {:?} id={} not checked: region not held", event.region_id, event.id);
        }
        if self.subscribed.contains(&event.region_id) {
            // Snapshot still in flight: hold the event for replay.
            self.pending_events.entry(event.region_id).or_default().push(event);
        } else {
            // Released/never-wanted region: steady-state noise.
            log::debug!("dropping event for region {:?}", event.region_id);
        }
    }

    /// One server event (`hash`: the state hash a `HashedGameEvent` carried).
    fn on_server_event(&mut self, event: GameEvent, hash: Option<(game::Tick, u32)>) {
        if !self.world_started {
            // No region yet: nothing to check a hash against, the snapshot
            // bakes it in.
            self.buffer.push(event);
            self.is_caught_up = false;
            return;
        }
        for buffered in std::mem::take(&mut self.buffer) {
            self.route_server_game_event(buffered, None);
        }
        self.route_server_game_event(event, hash);
        self.is_caught_up = true;
    }

    /// One event from connection `conn`.
    fn on_conn_event(&mut self, conn: u32, event: ConnEvent) -> Result<(), GameError> {
        if !self.conns.is_live(conn) {
            // Closed here already: what was still on its way is not this session's.
            log::debug!("[client] an event from connection {conn}, closed already: dropped");
            return Ok(());
        }
        match event {
            ConnEvent::Packet(packet) => self.handle_packet(conn, packet),
            ConnEvent::Closed(reason) if conn == self.conns.home() => {
                self.conns.on_closed(conn);
                Err(GameError::Disconnected(reason))
            }
            ConnEvent::Closed(reason) => {
                self.close_observer(conn, &reason, false);
                Ok(())
            }
        }
    }

    /// One server packet, as if from the home connection (test/harness hook).
    pub fn handle_server(&mut self, packet: ServerPacket) -> Result<(), GameError> {
        let home = self.conns.home();
        let result = self.handle_packet(home, packet);
        self.flush_outgoing();
        result
    }

    /// One packet from connection `conn`. Region traffic counts only from the
    /// connection that serves the region; everything else only from the home,
    /// but for what another host's connection itself is about (its `Host`, its
    /// refusal). The world clock follows the home's `SyncClock` samples alone.
    fn handle_packet(&mut self, conn: u32, packet: ServerPacket) -> Result<(), GameError> {
        let region = match &packet {
            ServerPacket::Region(id, _) => Some(*id),
            ServerPacket::GameEvent(event) | ServerPacket::HashedGameEvent(event, ..) => Some(event.region_id),
            _ => None,
        };
        if let Some(rc) = region {
            if !self.conns.accepts(conn, rc) {
                log::debug!("[client] {rc:?} traffic from connection {conn}, which does not serve it: dropped");
                return Ok(());
            }
            return self.apply_packet(conn, packet);
        }
        match packet {
            // The player's hop on that host: for the `PlayerRegion` behind it.
            ServerPacket::HomeHop(hop) => {
                self.conns.on_home_hop(conn, hop);
                return Ok(());
            }
            // The home's counts; another host's makes it the home, unless it is older
            // than a home already taken (`ConnectionSet::on_player_region`).
            ServerPacket::PlayerRegion(..) => {
                let old = self.conns.home();
                let with_hop = self.conns.has_hop(conn);
                if !self.conns.on_player_region(conn) {
                    let why = if with_hop { "older than the home's" } else { "with no HomeHop (not a hand-off's)" };
                    info!(
                        "[client] a PlayerRegion from host {:?}, {why} (home: host {:?}): ignored",
                        self.conns.ordinal_of(conn),
                        self.conns.home_ordinal()
                    );
                    return Ok(());
                }
                if self.conns.home() != old {
                    self.home_moved(old);
                }
                return self.apply_packet(conn, packet);
            }
            packet if conn == self.conns.home() => return self.apply_packet(conn, packet),
            ServerPacket::Host(endpoint) => {
                if !self.conns.on_host(conn, endpoint.ordinal) {
                    self.close_observer(conn, "its Host packet names another host", false);
                }
            }
            ServerPacket::Refused(why) => return self.observer_refused(conn, why),
            // Only the home's samples steer the clock (one host's world tick
            // numbering is every host's, but its round trip is not).
            ServerPacket::SyncClock(..) => {}
            _ => log::debug!("[client] a non-region packet on connection {conn} (not the home): ignored"),
        }
        Ok(())
    }

    /// The home moved from connection `old` to another host's (its `PlayerRegion`
    /// came): input, UI requests and the clock follow `conns.home()` from now on; the
    /// old connection stays for the regions it serves. The LOD subscriptions move too —
    /// released on the old host (a session holding only LOD subscriptions there would
    /// be idle-closed) and asked of the new home again, paced (`update_lod_window`).
    fn home_moved(&mut self, old: u32) {
        info!(
            "[client] the home moved from host {:?} to host {:?}",
            self.conns.ordinal_of(old),
            self.conns.home_ordinal()
        );
        // The subscribes left unanswered at the old home get no answer the client reads
        // any more (it reads only region traffic there; a `RegionElsewhere` from it is
        // dropped): each region still wanted and not held is released there (the old home
        // drops it; a late snapshot from it is not the mapped connection's any more) and
        // asked of the new home. The old entries are forgotten first, so an unasked
        // `RegionElsewhere` from the new home is never taken for their answer.
        let pending: Vec<RegionCoords> = self
            .asked_home
            .keys()
            .copied()
            .filter(|rc| self.subscribed.contains(rc) && !self.host.contains(rc) && self.conns.connection_of(*rc) == old)
            .collect();
        self.asked_home.clear();
        for rc in pending {
            self.send_out(Outgoing::Conn(old, ClientPacket::ReleaseRegionConnection(rc)));
            self.conns.release(rc);
            self.request_region(rc);
        }
        // A release still queued for the new home (it was a former home itself) would
        // land after the requests below and drop them: purged, as are those for a
        // connection that closed. No release is ever queued for the home, so every
        // (host, key) ends on what the client holds. Releases for a key no longer held
        // stay: that host still holds it.
        let new = self.conns.home();
        let conns = &self.conns;
        self.lod_release.retain(|(conn, _)| *conn != new && conns.is_live(*conn));
        // Released there and asked here again, both paced (`update_lod_window`).
        self.lod_release.extend(self.lod_subscribed.iter().map(|key| (old, *key)));
        self.lod_resend = self.lod_subscribed.clone();
    }

    /// Another host refused (and closes) its connection: its regions are asked
    /// for again through the home after the retry wait (an idle close, a
    /// ticket that reached that host after the client did). A player who is no
    /// longer this session's anywhere ends the session: log in again.
    fn observer_refused(&mut self, conn: u32, why: String) -> Result<(), GameError> {
        if why == game::PLAYER_PARKED_ELSEWHERE || why == game::PLAYER_ELSEWHERE_REFUSAL {
            error!("[client] host {:?} refused the connection: {why}", self.conns.ordinal_of(conn));
            return Err(GameError::Disconnected(format!("The server refused the connection: {why}")));
        }
        // An observer session with no region subscription is closed by its host:
        // routine (the window left that host's regions).
        let routine = why == game::OBSERVER_IDLE_REFUSAL;
        self.close_observer(conn, &format!("refused: {why}"), routine);
        Ok(())
    }

    /// Close another host's connection (or forget one that closed, `why`): its regions
    /// are dropped here and asked for again through the home, after a wait that doubles
    /// with each end of that host's connection in a row
    /// (`connections::observer_retry_delay_ms`). A `routine` end, or one of a
    /// connection that served no region, is logged at info, others as a warning.
    fn close_observer(&mut self, conn: u32, why: &str, routine: bool) {
        let ordinal = self.conns.ordinal_of(conn);
        let Closed::Observer { regions } = self.conns.on_closed(conn) else {
            error!("[client] close_observer on the home connection");
            return;
        };
        let lost: Vec<RegionCoords> = regions.into_iter().filter(|rc| self.subscribed.contains(rc)).collect();
        if lost.is_empty() {
            info!("[client] the connection to host {ordinal:?} closed ({why}); it served no region");
            return;
        }
        // A routine end (the host closed an idle session) is no failure of that host.
        let failures = match ordinal {
            Some(_) if routine => 0,
            Some(o) => {
                let n = self.observer_failures.entry(o).or_insert(0);
                *n += 1;
                *n
            }
            None => 1,
        };
        // The body is in, or next to, a region that host served (or moving to one):
        // the host may get it any moment and must see a session within its grace.
        let near = lost.iter().any(|rc| self.near_body(*rc));
        let wait = crate::connections::observer_retry_delay_ms(failures, near);
        if routine {
            info!("[client] the connection to host {ordinal:?} closed ({why}); {lost:?} asked for again in {wait} ms");
        } else {
            warn!("[client] the connection to host {ordinal:?} closed ({why}); {lost:?} asked for again in {wait} ms");
        }
        let (now, due) = (self.conn_clock_ms, self.conn_clock_ms + wait);
        for rc in lost {
            self.drop_region(rc);
            self.observer_retry.insert(rc, (due, now));
        }
    }

    /// Whether `rc` is the body's region, next to it, or a move's target: the body may
    /// be handed to its host any moment (`connections::OBSERVER_RETRY_NEAR_MAX_MS`).
    fn near_body(&self, rc: RegionCoords) -> bool {
        Some(rc) == self.pending_move
            || self.home_region.is_some_and(|home| (rc.x - home.x).abs() <= 1 && (rc.z - home.z).abs() <= 1)
    }

    /// Ask the home again for the regions of a closed observer connection whose
    /// retry is due and that are still wanted.
    fn retry_observer_regions(&mut self) {
        let now = self.conn_clock_ms;
        // A long wait set while the region was far is cut short once the body is near
        // it now: its host must see a session within the hand-off grace.
        let near_max = crate::connections::OBSERVER_RETRY_NEAR_MAX_MS;
        let due: Vec<RegionCoords> = self
            .observer_retry
            .iter()
            .filter(|(rc, (due, at))| {
                let due = if self.near_body(**rc) { (*due).min(at + near_max) } else { *due };
                due <= now
            })
            .map(|(rc, _)| *rc)
            .collect();
        for rc in due {
            self.observer_retry.remove(&rc);
            if self.subscribed.contains(&rc) && !self.host.contains(&rc) {
                self.request_region(rc);
            }
        }
    }

    /// Subscribe `rc` where it is served (through the home unless another host's
    /// connection serves it); one through the home is remembered until answered
    /// (`asked_home`).
    fn request_region(&mut self, rc: RegionCoords) {
        if self.conns.connection_of(rc) == self.conns.home() {
            let asked = self.asked_home.entry(rc).or_insert((0, 0));
            *asked = (asked.0 + 1, self.conn_clock_ms);
        }
        self.send_out(Outgoing::Region(rc, ClientPacket::RequestRegionConnection(rc)));
    }

    /// Every region still wanted and not held whose last subscribe through the home went
    /// `connections::OBSERVER_RETRY_MAX_MS` ago unanswered is asked of the home again
    /// (R4: a home that could not answer remembers it, but an answer may be lost — a
    /// redirect to a host that died before the client got there). One waiting for its
    /// observer retry (`observer_retry`) is left to it.
    fn ask_unanswered_again(&mut self) {
        let now = self.conn_clock_ms;
        let home = self.conns.home();
        let again: Vec<RegionCoords> = self
            .asked_home
            .iter()
            .filter(|(rc, (_, at))| {
                now.saturating_sub(*at) >= crate::connections::OBSERVER_RETRY_MAX_MS
                    && self.subscribed.contains(*rc)
                    && !self.host.contains(*rc)
                    && self.conns.connection_of(**rc) == home
                    && !self.observer_retry.contains_key(*rc)
            })
            .map(|(rc, _)| *rc)
            .collect();
        if !again.is_empty() {
            info!(
                "[client] {} region(s) the home has not answered for {} ms, asked again: {again:?}",
                again.len(),
                crate::connections::OBSERVER_RETRY_MAX_MS
            );
            for rc in again {
                self.request_region(rc);
            }
        }
        // Another host's connection that has not served a region subscribed there: let go
        // there and asked of the home, which says again who serves it (an observer session
        // waits rather than redirect: the region may be a third host's by now).
        let (subscribed, host, conns) = (&self.subscribed, &self.host, &self.conns);
        self.asked_observer.retain(|rc, _| {
            let conn = conns.connection_of(*rc);
            subscribed.contains(rc) && !host.contains(rc) && conn != home && conns.is_live(conn)
        });
        let stuck: Vec<(RegionCoords, u32)> = self
            .asked_observer
            .iter()
            .filter(|(_, at)| now.saturating_sub(**at) >= crate::connections::OBSERVER_RETRY_MAX_MS)
            .map(|(rc, _)| (*rc, self.conns.connection_of(*rc)))
            .collect();
        if stuck.is_empty() {
            return;
        }
        info!(
            "[client] {} region(s) another host's connection has not served for {} ms, asked of the home again: {stuck:?}",
            stuck.len(),
            crate::connections::OBSERVER_RETRY_MAX_MS
        );
        for (rc, conn) in stuck {
            self.asked_observer.remove(&rc);
            self.send_out(Outgoing::Conn(conn, ClientPacket::ReleaseRegionConnection(rc)));
            self.conns.release(rc);
            self.request_region(rc);
        }
    }

    /// One subscribe of `rc` through the home was answered (a snapshot or a
    /// `RegionElsewhere`): whether one was outstanding.
    fn answered_home(&mut self, rc: RegionCoords) -> bool {
        let Some(asked) = self.asked_home.get_mut(&rc) else { return false };
        asked.0 -= 1;
        if asked.0 == 0 {
            self.asked_home.remove(&rc);
        }
        true
    }

    /// Once per client tick: close connections idle for `CONN_IDLE_CLOSE_MS`, ask the
    /// home again for regions still wanted that it left unanswered, forget the other
    /// unanswered subscribes, and log once when the body (or a move) stays on another
    /// host than the home's with no `PlayerRegion` from there.
    fn maintain_connections(&mut self) {
        let now = self.conn_clock_ms;
        for key in self.conns.close_idle(now) {
            info!("[client] connection {key} served no region for {} ms: closed", crate::connections::CONN_IDLE_CLOSE_MS);
        }
        self.ask_unanswered_again();
        self.released_elsewhere.retain(|_, at| now.saturating_sub(*at) < ASKED_HOME_FORGET_MS);
        if let Some((rc, until)) = self.ambiguous_move {
            if self.pending_move != Some(rc) {
                self.ambiguous_move = None;
            } else if now >= until {
                info!("[client] no PlayerRegion for {rc:?} within {AMBIGUOUS_MOVE_HOLD_MS} ms: a late answer, not a move");
                self.ambiguous_move = None;
                self.pending_move = None;
            }
        }
        let (subscribed, host) = (&self.subscribed, &self.host);
        self.asked_home.retain(|rc, (_, at)| {
            now.saturating_sub(*at) < ASKED_HOME_FORGET_MS || (subscribed.contains(rc) && !host.contains(rc))
        });
        let home_host = self.conns.home_ordinal();
        let body_elsewhere = self.home_region.is_some_and(|rc| self.region_host(rc) != home_host);
        if !(body_elsewhere || self.pending_move.is_some()) {
            self.home_unconfirmed_since = None;
            self.home_unconfirmed_warned = false;
            return;
        }
        let since = *self.home_unconfirmed_since.get_or_insert(now);
        if !self.home_unconfirmed_warned && now.saturating_sub(since) >= HOME_UNCONFIRMED_WARN_MS {
            self.home_unconfirmed_warned = true;
            let what = match self.pending_move {
                Some(target) => format!("a move to {target:?} is pending"),
                None => format!(
                    "the body is in {:?}, served by host {:?}",
                    self.home_region,
                    self.home_region.and_then(|rc| self.region_host(rc))
                ),
            };
            warn!(
                "[client] {what}, and no host has said it is the home for {HOME_UNCONFIRMED_WARN_MS} ms \
                 (home: host {home_host:?}); waiting"
            );
        }
    }

    /// One packet the connection it came on may deliver (see `handle_packet`).
    fn apply_packet(&mut self, conn: u32, server_msg: ServerPacket) -> Result<(), GameError> {
        match server_msg {
            game::ServerPacket::SyncClock(_region, server_tick_rate, server_tick, rtt) => {
                // Every region reports the one world tick numbering, so any
                // region's sample steers the one clock — held or not (one
                // released mid-shift, or whose snapshot is still in flight,
                // still runs on the server's world clock). The home
                // connection's only (`handle_packet`): inputs go there, so its
                // round trip is the one the lead must cover.
                let input = crate::regions::clock::ClockInput {
                    server_tick,
                    server_rate_ms: server_tick_rate,
                    rtt_ms: rtt.as_millis() as u64,
                };
                if let Some(d) = self.clock.sample(&input) {
                    self.on_clock_decision(d);
                }
            }
            game::ServerPacket::HashedGameEvent(game_event, tick, hash) => {
                // The server's state hash after this event: the region checks
                // it when reconcile confirms the event.
                self.on_server_event(game_event, Some((tick, hash)));
            }
            game::ServerPacket::GameEvent(game_event) => self.on_server_event(game_event, None),
            game::ServerPacket::PlayerRegion(id, client_id) => {
                if let Err(e) = self
                    .client_event_send
                    .send(ClientUpdateEvent::SetPlayer(client_id))
                {
                    // Receiver gone means the render/client-bridge thread has exited;
                    // nothing left to notify, so just log and keep going.
                    warn!("client_event_send closed while sending SetPlayer: {:?}", e);
                }
                self.client_id = Some(client_id);

                let home = id.unwrap_or(RegionCoords::new(0, 0));
                let first_home = self.home_region.is_none();
                // A move's `PlayerRegion` (or any later one) ends the wait for it.
                self.pending_move = None;
                #[cfg(not(target_arch = "wasm32"))]
                match self.home_region {
                    // The server flipped first (no predicted crossing yet).
                    Some(prev) if prev != home => {
                        let stalled = self.clock.tick().map_or(0, |t| self.held_ticks(home, t));
                        let hosts = (self.region_host(prev), self.conns.ordinal_of(conn));
                        self.crossing.flip(prev, home, stalled, hosts);
                        self.crossing.confirmed(home);
                    }
                    _ => self.crossing.confirmed(home),
                }
                self.home_region = Some(home);
                if first_home {
                    game::perf_event!(
                        "player_region",
                        "home={},{} near_radius={}",
                        home.x,
                        home.z,
                        self.near_radius
                    );
                    // Initial join: ask for the whole near window up front;
                    // update_window keeps it in sync from then on. Later
                    // pushes (handoff confirmations) must NOT re-burst —
                    // that would resnapshot nine regions per crossing.
                    for rc in home.window_radius(self.near_radius) {
                        self.request_region(rc);
                        self.subscribed.insert(rc);
                    }
                }
            }
            game::ServerPacket::Region(id, raw_game_data) => {
                self.answered_home(id);
                if conn != self.conns.home() {
                    // That host serves again: its next connection end waits the
                    // shortest retry.
                    if let Some(o) = self.conns.ordinal_of(conn) {
                        self.observer_failures.remove(&o);
                    }
                }
                if !self.subscribed.contains(&id) {
                    // Window moved on while the snapshot was in flight: the
                    // connection it came on lets it go.
                    self.send_out(Outgoing::Conn(conn, ClientPacket::ReleaseRegionConnection(id)));
                    return Ok(());
                }
                // Capture events that raced ahead of the snapshot BEFORE
                // drop_region — its first act is to clear this region's
                // pending buffer, which would silently discard them.
                let pending = self.pending_events.remove(&id);
                #[cfg(not(target_arch = "wasm32"))]
                let snap_start = Instant::now();
                // Replace-on-re-receipt: crash-respawn/resubscribe resyncs.
                self.drop_region(id);
                self.new_region(id, raw_game_data);
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.perf.snapshot_count += 1;
                    self.perf.snapshot_total += snap_start.elapsed();
                    self.crossing.snapshot(snap_start.elapsed());
                    // Game-thread cost of one snapshot: teardown + the two
                    // `Rollback` clones in `new_region` + world insert.
                    game::perf_event!(
                        "region_loaded",
                        "region={},{} load_ms={} loaded={} subscribed={}",
                        id.x,
                        id.z,
                        game::perf::ms(snap_start.elapsed()),
                        self.perf.snapshot_count,
                        self.subscribed.len()
                    );
                }
                // Replay events that raced ahead of the snapshot; reconcile
                // skips anything already baked in (base_event_id).
                for ev in pending.into_iter().flatten() {
                    self.host.send(id, RegionCmd::ServerEvent(ev, None));
                }
            }
            game::ServerPacket::WorldMap(map) => {
                info!("[client] world map received: {}x{} tiles", map.width, map.height);
                self.route_map = Some(std::sync::Arc::new(game::RouteMap::from_world_map(&map)));
                let route = self.route_map.clone();
                self.host.broadcast(|| RegionCmd::SetRouteMap(route.clone()));
                let _ = self
                    .client_event_send
                    .send(ClientUpdateEvent::WorldMap(Box::new(map)));
            }
            game::ServerPacket::Refused(why) => {
                // The server closes the connection after this: end the session
                // with its reason.
                error!("[client] the server refused the connection: {why}");
                return Err(GameError::Disconnected(format!("The server refused the connection: {why}")));
            }
            game::ServerPacket::PlayRefused(rc, why) => {
                warn!("[client] Play in region {rc:?} refused: {why:?}");
                let _ = self
                    .client_event_send
                    .send(ClientUpdateEvent::PlayRefused(rc, why));
            }
            game::ServerPacket::LodData(key, payload, water) => {
                if !self.lod_subscribed.contains(&key) {
                    // Raced a release: tell the server we really don't want it.
                    self.send_out(Outgoing::Home(ClientPacket::ReleaseLod(key)));
                    return Ok(());
                }
                if !self.join.lod_all_received {
                    if self.join.lod_received.is_empty() {
                        game::perf_event!("lod_first_received", "factor={}", key.factor);
                    }
                    self.join.lod_received.insert(key);
                    if self.join.lod_requests_done
                        && self.lod_subscribed.is_subset(&self.join.lod_received)
                    {
                        self.join.lod_all_received = true;
                        game::perf_event!("lod_all_received", "keys={}", self.lod_subscribed.len());
                        self.join.lod_received.clear();
                    }
                }
                let _ = self
                    .client_event_send
                    .send(ClientUpdateEvent::LodData(key, payload, water));
            }
            game::ServerPacket::Sites(v) => {
                let _ = self.client_event_send.send(ClientUpdateEvent::Sites(v));
            }
            game::ServerPacket::Calendar(c) => {
                let _ = self.client_event_send.send(ClientUpdateEvent::Calendar(c));
            }
            game::ServerPacket::Fortress(f) => {
                // The window diff picks the neighbourhood up on the next tick.
                self.fortress_region = f.map(|f| f.region);
                let _ = self.client_event_send.send(ClientUpdateEvent::Fortress(f));
            }
            game::ServerPacket::Civs(c) => {
                let _ = self.client_event_send.send(ClientUpdateEvent::Civs(c));
            }
            game::ServerPacket::Legends(p) => {
                let _ = self.client_event_send.send(ClientUpdateEvent::Legends(p));
            }
            game::ServerPacket::AdventureResult(r) => {
                let _ = self.client_event_send.send(ClientUpdateEvent::AdventureResult(r));
            }
            game::ServerPacket::Notice(n) => {
                let _ = self.client_event_send.send(ClientUpdateEvent::Notice(n));
            }
            game::ServerPacket::EmbarkOptions(o) => {
                let _ = self.client_event_send.send(ClientUpdateEvent::EmbarkOptions(o));
            }
            game::ServerPacket::EmbarkResult(r) => {
                let _ = self.client_event_send.send(ClientUpdateEvent::EmbarkResult(r));
            }
            game::ServerPacket::PersonInfo(p) => {
                let _ = self.client_event_send.send(ClientUpdateEvent::PersonInfo(p));
            }
            // Unreachable: `handle_packet` takes every `HomeHop` before it gets here.
            game::ServerPacket::HomeHop(_) => debug_assert!(false, "HomeHop reached apply_packet"),
            // The first packet of every session: which host the home is.
            game::ServerPacket::Host(endpoint) => {
                info!("[client] the home connection is to host {}", endpoint.ordinal);
                if !self.conns.on_host(conn, endpoint.ordinal) {
                    warn!("[client] the home connection's Host packet names host {}: kept as it was", endpoint.ordinal);
                }
            }
            // Only the home sends these (`handle_packet`): a client dials no
            // address from anywhere else.
            game::ServerPacket::RegionElsewhere { region, endpoint, ticket } => self.region_elsewhere(region, endpoint, ticket),
        }
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl GameInstanceManager {
    /// Recieve and process game events from the client and network, in that order.
    ///
    /// - If received a client event:
    ///   - Simulate the event and push it on to a buffer.
    ///   - Send it to the players authoritative region.
    ///   - The client buffer should always be larger than the
    ///     network buffer by at least the number of ticks the client is ahead of
    ///     the server.
    /// - If recieved a network event:
    ///   - Check if the network and client event on the front of each buffer match.
    ///   - Keep doing this until the network buffer is empty. Reconcile the region
    ///     each time the comparison succeeds.
    ///   - Any time it doesn't succeed, rollback the client, apply the network
    ///     event and then re-apply the client events that were just rolled back.
    ///
    /// If sync clock event: send client update to window with desired input delay
    /// and update tick time.
    pub fn connect_and_run(&mut self) -> Result<(), GameError> {
        let tick_sender = self.game_event_send.clone();
        let tick_thread_tick_rate = self.tick_rate.clone();
        // Generate ticks
        std::thread::spawn(move || loop {
            // TODO: Sync ticks with server.
            if let Err(_) = tick_sender.send(GameEventKind::Tick) {
                return;
            };
            let rate = tick_thread_tick_rate.load(Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(rate));
        });

        // Every connection's events, keyed by connection (`connections`).
        let (events_send, events) = crossbeam::channel::unbounded();
        // Held for as long as this session runs: dropping it on any return
        // (Quit, a lost server) tells a connection still retrying its connect
        // to give up, so it cannot reach the next world's server.
        let (_netcode_cancel, cancel_recv) = crossbeam::channel::bounded::<()>(0);
        let net = crate::netcode::NetThread::start(events_send, cancel_recv, self.connect_policy)
            .map_err(|e| GameError::Disconnected(format!("netcode runtime failed to start: {e}")))?;
        // The login connection: the home until told otherwise.
        net.connect(
            crate::connections::LOGIN_CONN,
            crate::netcode::ConnTarget::Addr(self.server),
            self.credential.clone(),
            self.first_conn_recv.clone(),
        );
        // Other hosts' connections run on the same netcode runtime.
        self.opener = Some(Box::new(net));

        // Regions on their own threads, like the server's, unless
        // VOXEL_CLIENT_REGION_THREADS=0 keeps them on this one (A/B runs).
        let threads = std::env::var("VOXEL_CLIENT_REGION_THREADS").map_or(true, |v| v.trim() != "0");
        if threads {
            self.use_region_threads();
        }
        info!("[client] region threads: {}", if threads { "on" } else { "off" });
        let region_reports = self.host.report_recv();

        self.start();

        loop {
            let wait_start = Instant::now();
            select! {
                // Recieve and handle every connection's packets (the home's
                // closing ends the session with its reason).
                recv(events) -> event => {
                    self.perf.idle_total += wait_start.elapsed();
                    match event {
                        Ok((conn, event)) => self.on_conn_event(conn, event)?,
                        Err(_) => return Err(GameError::Disconnected("netcode stopped".to_string())),
                    }
                },
                // A region thread's prediction to send (routed below).
                recv(self.out_recv) -> out => {
                    self.perf.idle_total += wait_start.elapsed();
                    if let Ok(out) = out {
                        self.conns.route(out);
                    }
                },
                // Recieve client game events from either the player or from
                // client-side game tick timer.
                recv(self.game_event_recv) -> game_event => {
                    self.perf.idle_total += wait_start.elapsed();
                    match game_event {
                        Ok(event) => {
                            if !self.handle_game_event(event)? {
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            return Err(GameError::Disconnected(format!(
                                "game event channel closed: {e}"
                            )))
                        }
                    }
                }
                // A region thread reported (handed to the host, routed below).
                recv(region_reports) -> report => {
                    self.perf.idle_total += wait_start.elapsed();
                    if let Ok(report) = report {
                        self.host.accept(report);
                    }
                }
                // UI requests for the server (Play). A disconnected channel
                // (UI gone) just stops the arm from ever firing again.
                recv(self.request_recv) -> request => {
                    self.perf.idle_total += wait_start.elapsed();
                    match request {
                        Ok(request) => self.handle_request(request),
                        Err(_) => self.request_recv = crossbeam::channel::never(),
                    }
                }
            }
            self.drain_reports()?;
            self.flush_outgoing();
            self.emit_hud_status();
            self.maybe_report_metrics();
            self.crossing.maybe_report();
            self.maybe_report_tick_times();
        }
    }

    /// Flush the rolling reconciliation / drift counters to one DEBUG line once
    /// `METRICS_REPORT_INTERVAL` has elapsed, then reset the window. Called
    /// every loop iteration; cheap until the window fires. The `mispredicts/s`
    /// and `avg_replay` numbers show how hard the client is working to stay in
    /// sync; `drift` shows whether it is ahead of or behind the server.
    /// Sends the live regions' tick times to the render thread every
    /// [`game::perf::TICK_TIME_REPORT_INTERVAL`] (the F3 Debug tab).
    #[cfg(not(target_arch = "wasm32"))]
    fn maybe_report_tick_times(&mut self) {
        let live = self.host.ids();
        if let Some(stats) = self.tick_times.report(&live, Instant::now()) {
            let _ = self.client_event_send.send(ClientUpdateEvent::RegionTickTimes(stats));
        }
    }

    fn maybe_report_metrics(&mut self) {
        let now = std::time::Instant::now();
        let start = *self.recon.window_start.get_or_insert(now);
        let elapsed = now.duration_since(start);
        if elapsed < game::METRICS_REPORT_INTERVAL {
            return;
        }
        // Home-region prediction-log depth: the direct death-spiral signal.
        let log_depth = self.home_view().map(|v| v.log_len).unwrap_or(0);
        let m = &self.recon;
        let secs = elapsed.as_secs_f32();
        let per_sec = if secs > 0.0 {
            m.mispredictions as f32 / secs
        } else {
            0.0
        };
        let avg_replay = if m.mispredictions > 0 {
            m.replay_depth_sum as f32 / m.mispredictions as f32
        } else {
            0.0
        };
        log::debug!(
            target: game::perf::BUDGET_TARGET,
            "[client] mispredicts/s={:.2} avg_replay={:.1} max_replay={} evlog={} tick_rate={}ms drift={}ms aligned={}({}t)",
            per_sec,
            avg_replay,
            m.max_replay_depth,
            log_depth,
            self.tick_rate.load(Ordering::SeqCst),
            m.last_drift_ms,
            m.align_steps,
            m.align_ticks,
        );
        if m.mispredictions > 0 {
            // What was mispredicted (predicted>server), and how far the ones
            // in the home region actually moved the player.
            let names = ReconcileStats::KIND_NAMES;
            let kinds: Vec<String> = (0..3)
                .flat_map(|p| (0..3).map(move |s| (p, s)))
                .filter(|&(p, s)| m.by_kinds[p][s] > 0)
                .map(|(p, s)| format!("{}>{}={}", names[p], names[s], m.by_kinds[p][s]))
                .collect();
            log::debug!(
                target: game::perf::BUDGET_TARGET,
                "[client-mispredict] {} | home={} of {} | corrected n={} avg={:.3}u max={:.3}u",
                kinds.join(" "),
                m.home_mispredictions,
                m.mispredictions,
                m.corrections,
                if m.corrections > 0 { m.correction_sum / m.corrections as f32 } else { 0.0 },
                m.correction_max,
            );
        }
        if m.state_hash_checks > 0 {
            // Info, not the perf_budget target: the checks only run when the
            // server enabled VOXEL_STATE_HASH_INTERVAL, and their verdict is
            // the point. Each mismatch was also logged as an error.
            log::info!(
                "[client-state-hash] checked={} mismatched={} (server VOXEL_STATE_HASH_INTERVAL)",
                m.state_hash_checks,
                m.state_hash_mismatches,
            );
        }
        // ---- perf-budget attribution: where the game-loop wall time went ----
        // Drain the per-region sim accumulators. These include reconcile
        // replays, so `sim n=` exceeds `tick n=` × loaded regions whenever the
        // client is replaying; the gap is the replay multiplier.
        let mut sim = game::RegionTickPerf::default();
        let mut worst: Option<(RegionCoords, Duration)> = None;
        for (id, a) in std::mem::take(&mut self.perf.sim) {
            if a.ticks > 0 {
                let avg = a.total / a.ticks;
                if worst.map_or(true, |(_, b)| avg > b) {
                    worst = Some((id, avg));
                }
            }
            sim.merge(&a);
        }
        let ms = |d: Duration| d.as_secs_f32() * 1000.0;
        let p = &self.perf;
        let categorized = p.tick_total + p.reconcile_total + p.snapshot_total;
        let busy = elapsed.saturating_sub(p.idle_total);
        let other = busy.saturating_sub(categorized);
        let tick_avg_ms = if p.tick_count > 0 {
            ms(p.tick_total) / p.tick_count as f32
        } else {
            0.0
        };
        let worst_sim = worst
            .map(|(id, avg)| format!("({},{})@{:.2}ms", id.x, id.z, ms(avg)))
            .unwrap_or_else(|| "none".into());
        log::debug!(
            target: game::perf::BUDGET_TARGET,
            "[client-budget] host={} window={:.1}s util={:.1}% | tick n={} total={:.0}ms avg={:.2}ms max={:.2}ms | recon total={:.0}ms replayed={} | snap n={} total={:.0}ms | idle={:.0}ms other={:.0}ms | sim n={} ctrl={:.0}ms input={:.1}ms scan={:.0}ms worst={}",
            if self.host.is_threaded() { "threads" } else { "inline" },
            secs,
            busy.as_secs_f32() / secs * 100.0,
            p.tick_count,
            ms(p.tick_total),
            tick_avg_ms,
            ms(p.tick_max),
            ms(p.reconcile_total),
            self.recon.replay_depth_sum,
            p.snapshot_count,
            ms(p.snapshot_total),
            ms(p.idle_total),
            ms(other),
            sim.ticks,
            ms(sim.controllers),
            ms(sim.input),
            ms(sim.scan),
            worst_sim,
        );
        self.perf = PerfBudget::default();
        self.recon.log_depth_samples.push(log_depth);
        if self.recon.log_depth_samples.len() > KEEPUP_TREND_WINDOWS {
            self.recon.log_depth_samples.remove(0);
        }
        let s = &self.recon.log_depth_samples;
        let rising = s.len() == KEEPUP_TREND_WINDOWS
            && s.windows(2).all(|w| w[1] > w[0])
            && *s.last().unwrap() >= KEEPUP_WARN_MIN_DEPTH;
        if rising || log_depth >= KEEPUP_WARN_HARD_DEPTH {
            warn!(
                "[client] KEEP-UP WARNING: prediction log depth {} (samples {:?}) — \
                 reconcile is falling behind the server stream (death-spiral \
                 signature). Replay costs ~1 region tick per event; profile the \
                 region tick / see crates/client/tests/keepup_budget.rs",
                log_depth, s,
            );
        }
        self.recon.window_start = Some(now);
        self.recon.mispredictions = 0;
        self.recon.replay_depth_sum = 0;
        self.recon.max_replay_depth = 0;
        self.recon.align_steps = 0;
        self.recon.align_ticks = 0;
        self.recon.by_kinds = Default::default();
        self.recon.home_mispredictions = 0;
        self.recon.corrections = 0;
        self.recon.correction_sum = 0.0;
        self.recon.correction_max = 0.0;
    }
}

#[cfg(test)]
mod manager_tests {
    use super::*;
    use game::ClientUpdateEvent;
    use std::net::{Ipv4Addr, SocketAddrV4};

    fn minimal_world() -> game::World {
        let home = RegionCoords::new(0, 0);
        let mut world = game::World::new();
        world.load(&home, Region::from_chunks(home, Vec::new()));
        world
    }

    /// pump() must: load a region from a ServerPacket, then advance the sim
    /// on a Tick game event — all without blocking.
    #[test]
    fn pump_loads_region_and_ticks() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager = GameInstanceManager::new(
            game_send.clone(),
            game_recv,
            client_send,
            dummy_addr,
            game::DebugFlags::default(),
        );

        // Fake the server side using the same code the real server uses.
        let world = minimal_world();
        let region_id = RegionCoords::new(0, 0);
        server_send
            .send(ServerPacket::PlayerRegion(Some(region_id), 0))
            .unwrap();
        server_send
            .send(world.build_region_server_packet(&region_id))
            .unwrap();

        assert!(manager.pump_packets(&server_recv).unwrap());
        let tick_before = manager.world_ref().current_tick(&region_id);

        manager.send_tick();
        assert!(manager.pump_packets(&server_recv).unwrap());
        let tick_after = manager.world_ref().current_tick(&region_id);
        assert_eq!(tick_after, tick_before + 1);

        // The render bridge must have been told about the new region + player.
        let mut saw_region = false;
        let mut saw_player = false;
        while let Ok(ev) = client_recv.try_recv() {
            match ev {
                ClientUpdateEvent::NewRegion(..) => saw_region = true,
                ClientUpdateEvent::SetPlayer(..) => saw_player = true,
                _ => {}
            }
        }
        assert!(saw_region && saw_player);

        // Quit terminates the pump.
        game_send.send(GameEventKind::Quit).unwrap();
        assert!(!manager.pump_packets(&server_recv).unwrap());
    }

    /// `ready` only tunes the adaptive tick rate; it must NOT gate input.
    /// Regression test for the browser input-drop: SyncClock sampling flaps
    /// `ready` to false whenever the client samples one tick behind (rtt=0),
    /// and the old `!ready && is_caught_up` gate then ate every keypress for
    /// the following 500ms window — measured live at 261 dropped : 1 sent.
    /// Only a sim that has not yet caught up with the server may drop input.
    #[test]
    fn player_input_flows_while_not_ready_once_caught_up() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager = GameInstanceManager::new(
            game_send.clone(),
            game_recv,
            client_send,
            dummy_addr,
            game::DebugFlags::default(),
        );
        let outgoing = manager.client_packet_recv();

        let world = minimal_world();
        let region_id = RegionCoords::new(0, 0);
        server_send
            .send(ServerPacket::PlayerRegion(Some(region_id), 0))
            .unwrap();
        server_send
            .send(world.build_region_server_packet(&region_id))
            .unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        // Keep the render-bridge receiver alive so region emits don't hit a
        // closed channel (see offline_handshake_loads_region).
        let mut bridge_recv = None;
        while let Ok(ev) = client_recv.try_recv() {
            if let ClientUpdateEvent::NewRegion(_, _, recv) = ev {
                bridge_recv = Some(recv);
            }
        }
        // Drain the handshake packets the manager itself sent.
        while outgoing.try_recv().is_ok() {}

        // The flapping state observed live: caught up, but sampled behind.
        manager.is_caught_up = true;
        manager.ready = false;

        game_send
            .send(GameEventKind::PlayerInput(
                0,
                game::InputEvent::Key {
                    key: game::Key::KeyE,
                    pressed: true,
                },
            ))
            .unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        assert!(
            matches!(outgoing.try_recv(), Ok(ClientPacket::GameEventAfterTick(..))),
            "PlayerInput must be predicted + sent while ready=false once caught up"
        );

        // The join/replay window is still protected: before catch-up, drop.
        manager.is_caught_up = false;
        game_send
            .send(GameEventKind::PlayerInput(
                0,
                game::InputEvent::Key {
                    key: game::Key::KeyE,
                    pressed: false,
                },
            ))
            .unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        assert!(
            outgoing.try_recv().is_err(),
            "PlayerInput must be dropped until the sim has caught up"
        );
        drop(bridge_recv);
    }

    fn manager_with_player() -> (
        GameInstanceManager,
        crossbeam::channel::Sender<ServerPacket>,
        Receiver<ServerPacket>,
        Receiver<ClientUpdateEvent>,
        crossbeam::channel::Sender<GameEventKind>,
    ) {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let manager = GameInstanceManager::new(
            game_send.clone(),
            game_recv,
            client_send,
            dummy_addr,
            game::DebugFlags::default(),
        );
        (manager, server_send, server_recv, client_recv, game_send)
    }

    /// Opens nothing real: counts the opens.
    struct CountingOpener(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl ConnOpener for CountingOpener {
        fn open(&mut self, _key: u32, _endpoint: &game::HostEndpoint, _credential: game::Credential) -> ConnHandle {
            self.0.fetch_add(1, Ordering::SeqCst);
            ConnHandle { send: crossbeam::channel::unbounded().0 }
        }
    }

    /// Keeps every opened connection's outgoing packets readable.
    #[derive(Clone, Default)]
    struct RecordingOpener(std::sync::Arc<std::sync::Mutex<Vec<Receiver<ClientPacket>>>>);

    impl ConnOpener for RecordingOpener {
        fn open(&mut self, _key: u32, _endpoint: &game::HostEndpoint, _credential: game::Credential) -> ConnHandle {
            let (send, recv) = crossbeam::channel::unbounded();
            self.0.lock().unwrap().push(recv);
            ConnHandle { send }
        }
    }

    /// The LOD keys a host holds for the client after the packets it got, in order.
    fn lod_held(packets: impl IntoIterator<Item = ClientPacket>, held: &mut std::collections::BTreeSet<game::lod::LodKey>) {
        for p in packets {
            match p {
                ClientPacket::RequestLod(key) => {
                    held.insert(key);
                }
                ClientPacket::ReleaseLod(key) => {
                    held.remove(&key);
                }
                _ => {}
            }
        }
    }

    /// Fix round 2 (Task 18 I1, m7): the home flips A -> B -> A -> B while the paced
    /// LOD releases of the earlier flips are still queued (more keys than one tick
    /// sends): no release reaches a host after its keys were asked of it again — at the
    /// end B holds every key the client holds and A none. The subscribes left
    /// unanswered at the old home are forgotten at each flip (one still wanted is asked
    /// of the new home).
    #[test]
    fn lod_keys_end_on_the_home_after_quick_flips() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let opener = RecordingOpener::default();
        manager.set_conn_opener(Box::new(opener.clone()));
        manager.set_lod_table(game::lod::LOD_RING_TABLE);
        let home = RegionCoords::new(0, 0);
        let b_region = RegionCoords::new(1, 0);
        let endpoint = |ordinal| game::HostEndpoint { ordinal, quic: None, webtransport: None, cert_hash: None };
        server_send.send(ServerPacket::Host(endpoint(0))).unwrap();
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        server_send
            .send(ServerPacket::RegionElsewhere {
                region: b_region,
                endpoint: endpoint(1),
                ticket: game::ObserverTicket { client: 0, nonce: 1 },
            })
            .unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        for _ in 0..200 {
            manager.update_lod_window();
        }
        let keys = manager.lod_subscribed.clone();
        assert!(keys.len() > 2 * LOD_REQUESTS_PER_TICK, "more keys than a tick sends: {}", keys.len());
        let b = manager.conns.conn_to(1).expect("a connection to host 1");
        let a = crate::connections::LOGIN_CONN;
        let (events, incoming) = crossbeam::channel::unbounded();
        let mut flip = |manager: &mut GameInstanceManager, conn: u32, hop: u64| {
            events.send((conn, ConnEvent::Packet(ServerPacket::HomeHop(hop)))).unwrap();
            events.send((conn, ConnEvent::Packet(ServerPacket::PlayerRegion(Some(home), 0)))).unwrap();
            assert!(manager.pump(&incoming).unwrap());
            assert_eq!(manager.conns.home(), conn);
        };
        manager.request_region(RegionCoords::new(-1, 0));
        flip(&mut manager, b, 1);
        // Forgotten there: what is left was asked of the new home (a region still wanted).
        let asked_forgotten = manager.asked_home.keys().all(|rc| manager.conns.connection_of(*rc) == b);
        manager.update_lod_window();
        flip(&mut manager, a, 2);
        manager.update_lod_window();
        flip(&mut manager, b, 3);
        for _ in 0..200 {
            manager.update_lod_window();
        }
        assert_eq!(manager.lod_subscribed, keys);
        let mut on_a = std::collections::BTreeSet::new();
        lod_held(manager.client_packet_recv().try_iter(), &mut on_a);
        let mut on_b = std::collections::BTreeSet::new();
        lod_held(opener.0.lock().unwrap()[0].try_iter(), &mut on_b);
        assert_eq!(on_b, keys, "host B holds every key the client holds ({} missing)", keys.difference(&on_b).count());
        assert!(on_a.is_empty(), "host A holds none: {} left", on_a.len());
        assert!(asked_forgotten, "the old home's unanswered subscribes are forgotten (or asked of the new home) at the flip");
    }

    /// Final review I4: region R is asked of home A; the home flips to B before A's answer
    /// (which the client would no longer read): R is released on A and asked of B, a late
    /// snapshot from A is dropped, B's loads it, and nothing of R is left subscribed on A.
    #[test]
    fn a_region_pending_on_the_old_home_is_asked_of_the_new_one() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let opener = RecordingOpener::default();
        manager.set_conn_opener(Box::new(opener.clone()));
        let home = RegionCoords::new(0, 0);
        let b_region = RegionCoords::new(1, 0);
        let r = RegionCoords::new(-1, 0);
        let endpoint = |ordinal| game::HostEndpoint { ordinal, quic: None, webtransport: None, cert_hash: None };
        server_send.send(ServerPacket::Host(endpoint(0))).unwrap();
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        server_send
            .send(ServerPacket::RegionElsewhere {
                region: b_region,
                endpoint: endpoint(1),
                ticket: game::ObserverTicket { client: 0, nonce: 1 },
            })
            .unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        let a = crate::connections::LOGIN_CONN;
        let b = manager.conns.conn_to(1).expect("a connection to host 1");
        // R is wanted and asked of home A; no answer yet.
        manager.subscribed.insert(r);
        manager.request_region(r);
        manager.flush_outgoing();
        let _ = manager_packets(&manager);
        let on_b = opener.0.lock().unwrap()[0].clone();
        let _ = on_b.try_iter().count();
        // B's HomeHop + PlayerRegion arrive before A's answer.
        let (events, incoming) = crossbeam::channel::unbounded();
        events.send((b, ConnEvent::Packet(ServerPacket::HomeHop(1)))).unwrap();
        events.send((b, ConnEvent::Packet(ServerPacket::PlayerRegion(Some(home), 0)))).unwrap();
        assert!(manager.pump(&incoming).unwrap());
        assert_eq!(manager.conns.home(), b);
        let sent_a = manager_packets(&manager);
        assert!(
            sent_a.iter().any(|p| matches!(p, ClientPacket::ReleaseRegionConnection(rc) if *rc == r)),
            "released on the old home: {sent_a:?}"
        );
        let sent_b: Vec<ClientPacket> = on_b.try_iter().collect();
        assert!(
            sent_b.iter().any(|p| matches!(p, ClientPacket::RequestRegionConnection(rc) if *rc == r)),
            "asked of the new home: {sent_b:?}"
        );
        assert_eq!(manager.conns.connection_of(r), b);
        // A's late snapshot is dropped; B's loads R.
        let mut world = game::World::new();
        world.load(&r, Region::from_chunks(r, Vec::new()));
        events.send((a, ConnEvent::Packet(world.build_region_server_packet(&r)))).unwrap();
        assert!(manager.pump(&incoming).unwrap());
        assert!(!manager.loaded_regions().contains(&r), "not from the old home");
        events.send((b, ConnEvent::Packet(world.build_region_server_packet(&r)))).unwrap();
        assert!(manager.pump(&incoming).unwrap());
        assert!(manager.loaded_regions().contains(&r), "loaded from the new home");
        // The client's own packets to A from here on hold no subscribe of R.
        assert!(!manager_packets(&manager).iter().any(|p| matches!(p, ClientPacket::RequestRegionConnection(rc) if *rc == r)));
    }

    /// The packets the client sent on its login connection since the last call.
    fn manager_packets(manager: &GameInstanceManager) -> Vec<ClientPacket> {
        manager.client_packet_recv().try_iter().collect()
    }

    /// Fix round 2 (Task 18 m5): a region's retry was set to a long wait while it was far
    /// from the body; once the body is next to it, the retry fires within
    /// `OBSERVER_RETRY_NEAR_MAX_MS` of when it was set, not at the long due time.
    #[test]
    fn a_long_retry_wait_is_cut_short_when_the_body_comes_near() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let endpoint = game::HostEndpoint { ordinal: 0, quic: None, webtransport: None, cert_hash: None };
        server_send.send(ServerPacket::Host(endpoint)).unwrap();
        server_send.send(ServerPacket::PlayerRegion(Some(RegionCoords::new(0, 0)), 0)).unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        let _ = manager.client_packet_recv().try_iter().count();
        let target = RegionCoords::new(4, 0);
        manager.subscribed.insert(target);
        let near_max = crate::connections::OBSERVER_RETRY_NEAR_MAX_MS;
        let long = crate::connections::observer_retry_delay_ms(8, false);
        assert!(long > near_max);
        let at = manager.conn_clock_ms;
        manager.observer_retry.insert(target, (at + long, at));
        let asked = |m: &GameInstanceManager| {
            m.client_packet_recv().try_iter().any(|p| matches!(p, ClientPacket::RequestRegionConnection(rc) if rc == target))
        };
        manager.conn_clock_ms = at + near_max;
        manager.retry_observer_regions();
        assert!(!asked(&manager), "far from the body: the long wait holds");
        // The body walked next to it.
        manager.home_region = Some(RegionCoords::new(3, 0));
        manager.retry_observer_regions();
        assert!(asked(&manager), "near the body: asked for again within the near cap");
        assert!(manager.observer_retry.is_empty());
    }

    /// R4 Task 16: a subscribe through the home that is never answered (the home could not
    /// reach the region's owner, and the answer it gave later was lost) is not forgotten
    /// after `ASKED_HOME_FORGET_MS` while the region is still wanted and not held: it is
    /// asked again every `OBSERVER_RETRY_MAX_MS` until the region is held, then forgotten.
    /// One that left the window is forgotten and never asked again. A region whose observer
    /// connection closed keeps being asked the same way after its first retry.
    #[test]
    fn an_unanswered_region_is_asked_again() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let opener = RecordingOpener::default();
        manager.set_conn_opener(Box::new(opener.clone()));
        let endpoint = |ordinal| game::HostEndpoint { ordinal, quic: None, webtransport: None, cert_hash: None };
        server_send.send(ServerPacket::Host(endpoint(0))).unwrap();
        server_send.send(ServerPacket::PlayerRegion(Some(RegionCoords::new(0, 0)), 0)).unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        let (wanted, gone, observed) = (RegionCoords::new(5, 0), RegionCoords::new(-5, 0), RegionCoords::new(0, 5));
        let max = crate::connections::OBSERVER_RETRY_MAX_MS;
        let t0 = manager.conn_clock_ms;
        for rc in [wanted, gone, observed] {
            manager.subscribed.insert(rc);
            manager.request_region(rc);
        }
        // `observed` is host 1's; its connection closes before it served anything.
        server_send
            .send(ServerPacket::RegionElsewhere {
                region: observed,
                endpoint: endpoint(1),
                ticket: game::ObserverTicket { client: 0, nonce: 1 },
            })
            .unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        let one = manager.conns.conn_to(1).expect("a connection to host 1");
        manager.close_observer(one, "closed in the test", false);
        // `gone` leaves the window.
        manager.subscribed.remove(&gone);
        let _ = manager_packets(&manager);
        // The home's packets since the last call: how often `rc` was asked.
        let asked = |m: &GameInstanceManager, rc: RegionCoords| {
            manager_packets(m).iter().filter(|p| matches!(p, ClientPacket::RequestRegionConnection(r) if *r == rc)).count()
        };
        let at = |m: &mut GameInstanceManager, ms: u64| {
            m.conn_clock_ms = t0 + ms;
            m.retry_observer_regions();
            m.maintain_connections();
        };
        at(&mut manager, crate::connections::OBSERVER_RETRY_MS);
        let first = manager_packets(&manager);
        assert!(
            first.iter().any(|p| matches!(p, ClientPacket::RequestRegionConnection(r) if *r == observed)),
            "the closed connection's region is asked of the home: {first:?}"
        );
        at(&mut manager, ASKED_HOME_FORGET_MS);
        assert!(manager.asked_home.contains_key(&wanted), "a region still wanted is not forgotten");
        assert!(!manager.asked_home.contains_key(&gone), "one that left the window is");
        assert!(manager_packets(&manager).is_empty(), "nothing asked before OBSERVER_RETRY_MAX_MS");
        at(&mut manager, max);
        let sent = manager_packets(&manager);
        let count = |rc: RegionCoords| sent.iter().filter(|p| matches!(p, ClientPacket::RequestRegionConnection(r) if *r == rc)).count();
        assert_eq!((count(wanted), count(gone), count(observed)), (1, 0, 0), "asked again after OBSERVER_RETRY_MAX_MS: {sent:?}");
        at(&mut manager, crate::connections::OBSERVER_RETRY_MS + max);
        assert_eq!(asked(&manager, observed), 1, "the closed connection's region too");
        // `wanted` is held now: never asked again, then forgotten.
        let mut world = game::World::new();
        world.load(&wanted, Region::from_chunks(wanted, Vec::new()));
        server_send.send(world.build_region_server_packet(&wanted)).unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        assert!(manager.loaded_regions().contains(&wanted));
        at(&mut manager, 3 * max);
        assert_eq!(asked(&manager, wanted), 0, "a held region is not asked again");
        at(&mut manager, 3 * max + ASKED_HOME_FORGET_MS);
        assert!(!manager.asked_home.contains_key(&wanted), "and forgotten");
        assert!(manager.asked_home.contains_key(&observed), "one not held stays");
    }

    /// The bridge and game-event ends a test keeps alive (dropped, the manager's sends fail).
    type KeptEnds = (Receiver<ClientUpdateEvent>, crossbeam::channel::Sender<GameEventKind>);

    /// A manager whose home is host 0 (its `Host` and `PlayerRegion` came), recording the
    /// connections it opens; the home's packets drained.
    fn manager_on_host_0() -> (GameInstanceManager, crossbeam::channel::Sender<ServerPacket>, Receiver<ServerPacket>, RecordingOpener, KeptEnds) {
        let (mut manager, server_send, server_recv, client_recv, game) = manager_with_player();
        let opener = RecordingOpener::default();
        manager.set_conn_opener(Box::new(opener.clone()));
        server_send.send(ServerPacket::Host(host_endpoint(0))).unwrap();
        server_send.send(ServerPacket::PlayerRegion(Some(RegionCoords::new(0, 0)), 0)).unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        let _ = manager_packets(&manager);
        (manager, server_send, server_recv, opener, (client_recv, game))
    }

    fn host_endpoint(ordinal: u32) -> game::HostEndpoint {
        game::HostEndpoint { ordinal, quic: None, webtransport: None, cert_hash: None }
    }

    fn elsewhere(region: RegionCoords, ordinal: u32) -> ServerPacket {
        ServerPacket::RegionElsewhere { region, endpoint: host_endpoint(ordinal), ticket: game::ObserverTicket { client: 0, nonce: 1 } }
    }

    /// Whether `packets` hold a subscribe (`true`) or release (`false`) of `rc`.
    fn has(packets: &[ClientPacket], rc: RegionCoords, subscribe: bool) -> bool {
        packets.iter().any(|p| match p {
            ClientPacket::RequestRegionConnection(r) => subscribe && *r == rc,
            ClientPacket::ReleaseRegionConnection(r) => !subscribe && *r == rc,
            _ => false,
        })
    }

    /// R4 Task 16, fix round 1: the manager's arm for a `RegionElsewhere` naming the home —
    /// a wanted region host 1 served is released there, served by the home connection and
    /// subscribed there (remembered as asked of the home); no connection is opened. One no
    /// longer wanted is ignored (never a move).
    #[test]
    fn region_elsewhere_naming_the_home_is_subscribed_at_home() {
        let (mut manager, server_send, server_recv, opener, _kept) = manager_on_host_0();
        let r = RegionCoords::new(5, 0);
        manager.subscribed.insert(r);
        manager.request_region(r);
        server_send.send(elsewhere(r, 1)).unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        let _ = manager_packets(&manager);
        let on_one = opener.0.lock().unwrap()[0].clone();
        assert!(has(&on_one.try_iter().collect::<Vec<_>>(), r, true), "subscribed on host 1 first");
        assert_eq!(manager.region_host(r), Some(1));
        server_send.send(elsewhere(r, 0)).unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        assert!(has(&on_one.try_iter().collect::<Vec<_>>(), r, false), "released on host 1");
        assert!(has(&manager_packets(&manager), r, true), "subscribed at home");
        assert_eq!(manager.region_host(r), Some(0));
        assert!(manager.asked_home.contains_key(&r), "asked of the home, until answered");
        assert_eq!(opener.0.lock().unwrap().len(), 1, "no connection opened to the home host");
        // Not wanted: nothing.
        let gone = RegionCoords::new(-5, 0);
        server_send.send(elsewhere(gone, 0)).unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        assert!(manager_packets(&manager).is_empty());
        assert!(!manager.subscribed.contains(&gone) && manager.pending_move.is_none(), "never a move");
    }

    /// R4 Task 16, fix round 1: a region subscribed on host 1's connection that host 1
    /// never serves (its observer session waits: the region may be a third host's by now) is
    /// let go there after `OBSERVER_RETRY_MAX_MS` and asked of the home again; a region it
    /// serves is left alone.
    #[test]
    fn a_region_an_observer_connection_does_not_serve_is_asked_of_the_home() {
        let (mut manager, server_send, server_recv, opener, _kept) = manager_on_host_0();
        let (stuck, served) = (RegionCoords::new(5, 0), RegionCoords::new(5, 1));
        let t0 = manager.conn_clock_ms;
        for rc in [stuck, served] {
            manager.subscribed.insert(rc);
            manager.request_region(rc);
            server_send.send(elsewhere(rc, 1)).unwrap();
        }
        assert!(manager.pump_packets(&server_recv).unwrap());
        let one = manager.conns.conn_to(1).expect("a connection to host 1");
        let on_one = opener.0.lock().unwrap()[0].clone();
        let _ = on_one.try_iter().count();
        let _ = manager_packets(&manager);
        // Host 1 serves `served`.
        let mut world = game::World::new();
        world.load(&served, Region::from_chunks(served, Vec::new()));
        let (events, incoming) = crossbeam::channel::unbounded();
        events.send((one, ConnEvent::Packet(world.build_region_server_packet(&served)))).unwrap();
        assert!(manager.pump(&incoming).unwrap());
        assert!(manager.loaded_regions().contains(&served));
        let max = crate::connections::OBSERVER_RETRY_MAX_MS;
        manager.conn_clock_ms = t0 + max - 1;
        manager.maintain_connections();
        assert!(on_one.try_iter().next().is_none() && manager_packets(&manager).is_empty(), "not before OBSERVER_RETRY_MAX_MS");
        manager.conn_clock_ms = t0 + max;
        manager.maintain_connections();
        let sent_one: Vec<ClientPacket> = on_one.try_iter().collect();
        let sent_home = manager_packets(&manager);
        assert!(has(&sent_one, stuck, false), "let go on host 1: {sent_one:?}");
        assert!(has(&sent_home, stuck, true), "asked of the home: {sent_home:?}");
        assert_eq!(manager.region_host(stuck), Some(0));
        assert!(!has(&sent_one, served, false) && !has(&sent_home, served, true), "a served region is left alone");
        assert_eq!(manager.region_host(served), Some(1));
    }

    /// R4 Task 16, fix round 1: a region host 1 served leaves the window; the home's
    /// re-answer for it (crossing the release) reads like a move. It is held only
    /// `AMBIGUOUS_MOVE_HOLD_MS` without the move's `PlayerRegion`; a real move for a region
    /// never released is held until its `PlayerRegion`.
    #[test]
    fn a_late_answer_for_a_just_released_region_is_not_held_as_a_move() {
        let (mut manager, server_send, server_recv, _opener, _kept) = manager_on_host_0();
        let r = RegionCoords::new(5, 0);
        manager.subscribed.insert(r);
        manager.request_region(r);
        server_send.send(elsewhere(r, 1)).unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        let t0 = manager.conn_clock_ms;
        manager.release_region(r);
        manager.subscribed.remove(&r);
        assert!(has(&manager_packets(&manager), r, false), "released at the home too");
        server_send.send(elsewhere(r, 1)).unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        assert_eq!(manager.pending_move, Some(r), "it reads like a move");
        manager.conn_clock_ms = t0 + AMBIGUOUS_MOVE_HOLD_MS - 1;
        manager.maintain_connections();
        assert_eq!(manager.pending_move, Some(r));
        manager.conn_clock_ms = t0 + AMBIGUOUS_MOVE_HOLD_MS;
        manager.maintain_connections();
        assert_eq!(manager.pending_move, None, "no PlayerRegion came: a late answer, let go");
        // A real move: held until its PlayerRegion.
        let m = RegionCoords::new(9, 0);
        server_send.send(elsewhere(m, 1)).unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        assert_eq!(manager.pending_move, Some(m));
        manager.conn_clock_ms += 10 * AMBIGUOUS_MOVE_HOLD_MS;
        manager.maintain_connections();
        assert_eq!(manager.pending_move, Some(m), "a move into a region never released waits for its PlayerRegion");
    }

    /// Fix round (Task 18 m4): a region subscribed and released twice through the home,
    /// both `RegionElsewhere` answers late — each is recognised as the answer to a
    /// request, not read as a move; an unasked one after them is a move.
    #[test]
    fn late_region_elsewhere_answers_are_never_read_as_a_move() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let opens = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        manager.set_conn_opener(Box::new(CountingOpener(opens.clone())));
        let home = RegionCoords::new(0, 0);
        let far = RegionCoords::new(5, 0);
        let endpoint = game::HostEndpoint { ordinal: 1, quic: None, webtransport: None, cert_hash: None };
        let elsewhere = || ServerPacket::RegionElsewhere {
            region: far,
            endpoint: endpoint.clone(),
            ticket: game::ObserverTicket { client: 0, nonce: 1 },
        };
        server_send.send(ServerPacket::Host(game::HostEndpoint { ordinal: 0, ..endpoint.clone() })).unwrap();
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        for _ in 0..2 {
            // Requested through the home, then released before its answer came.
            manager.request_region(far);
            manager.send_out(Outgoing::Region(far, ClientPacket::ReleaseRegionConnection(far)));
            manager.conns.release(far);
        }
        for _ in 0..2 {
            server_send.send(elsewhere()).unwrap();
            assert!(manager.pump_packets(&server_recv).unwrap());
            assert_eq!(manager.pending_move, None, "a late answer is not a move");
            assert!(!manager.subscribed.contains(&far));
        }
        assert_eq!(opens.load(Ordering::SeqCst), 0, "nothing connected for an unwanted region");
        server_send.send(elsewhere()).unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());
        assert_eq!(manager.pending_move, Some(far), "an unasked one is a move");
        assert!(manager.subscribed.contains(&far));
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }

    /// The desired window must keep the home region subscribed even when
    /// the free-cam viewer roams outside home's 3x3 neighbourhood: input
    /// routing and viewer-pose reads both anchor on home.
    #[test]
    fn desired_window_pins_home_outside_3x3() {
        let home = RegionCoords::new(0, 0);
        let far = RegionCoords::new(5, -3);
        let desired = desired_window(far, Some(home), 1, None, 0);
        assert!(desired.contains(&home), "home must never leave the window");
        assert_eq!(desired.len(), 10, "3x3 around viewer plus distant home");
        for rc in far.window_3x3() {
            assert!(desired.contains(&rc));
        }
    }

    /// Home inside (or at the centre of) the 3x3 must not duplicate, and no
    /// home yields the plain 3x3.
    #[test]
    fn desired_window_home_inside_does_not_duplicate() {
        let home = RegionCoords::new(0, 0);
        // Home adjacent to centre: inside the 3x3.
        let desired = desired_window(RegionCoords::new(1, 0), Some(home), 1, None, 0);
        assert_eq!(desired.len(), 9);
        assert!(desired.contains(&home));
        // Home at centre.
        let desired = desired_window(home, Some(home), 1, None, 0);
        assert_eq!(desired.len(), 9);
        // No home yet: plain 3x3.
        assert_eq!(desired_window(RegionCoords::new(2, 2), None, 1, None, 0).len(), 9);
    }

    /// The fortress neighbourhood is simulated whatever the viewer does:
    /// Fortress mode (viewer at the fortress, one-region near window) gets the
    /// 3x3, and an adventurer far away keeps it next to its own window.
    #[test]
    fn desired_window_keeps_fortress_neighbourhood() {
        let fort = RegionCoords::new(4, 4);
        let at_home = desired_window(fort, Some(fort), 0, Some(fort), 1);
        let expected: std::collections::BTreeSet<_> = fort.window_3x3().into_iter().collect();
        assert_eq!(at_home, expected);

        let away = RegionCoords::new(9, 4);
        let roaming = desired_window(away, Some(away), 0, Some(fort), 1);
        assert_eq!(roaming.len(), 10, "3x3 around the fortress plus the adventurer's region");
        assert!(roaming.contains(&away));
        assert!(expected.is_subset(&roaming));

        // Next door: the adventurer's region is part of the neighbourhood.
        let next = RegionCoords::new(5, 4);
        assert_eq!(desired_window(next, Some(next), 0, Some(fort), 1), expected);
        // Radius 0 / no fortress: only the near window.
        assert_eq!(desired_window(away, Some(away), 0, Some(fort), 0).len(), 2);
        assert_eq!(desired_window(away, Some(away), 0, None, 1).len(), 1);
    }

    /// PlayerRegion must trigger subscription requests for the full 3x3
    /// window around home, not just home.
    #[test]
    fn player_region_requests_full_window() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        server_send
            .send(ServerPacket::PlayerRegion(Some(home), 0))
            .unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());

        let mut requested = std::collections::BTreeSet::new();
        while let Ok(p) = manager.client_packet_recv().try_recv() {
            if let ClientPacket::RequestRegionConnection(rc) = p {
                requested.insert(rc);
            }
        }
        let expected: std::collections::BTreeSet<_> = home.window_3x3().into_iter().collect();
        assert_eq!(requested, expected);
    }

    /// Events for a subscribed-but-not-yet-loaded region are buffered and
    /// applied when the snapshot arrives, not dropped.
    #[test]
    fn events_for_pending_region_are_buffered_until_snapshot() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        server_send
            .send(ServerPacket::PlayerRegion(Some(home), 0))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();

        // Load home so `world` exists.
        let world = minimal_world();
        server_send
            .send(world.build_region_server_packet(&home))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();

        // A neighbour's tick arrives before its snapshot.
        let neighbour = RegionCoords::new(1, 0);
        let tick_from_neighbour = GameEvent::new(GameEventKind::Tick, 0, neighbour);
        server_send
            .send(ServerPacket::GameEvent(tick_from_neighbour))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();
        assert!(!manager.world_ref().region_exists(&neighbour));

        // Snapshot arrives: region loads, then the buffered event must be
        // replayed through reconcile — Tick id 0 lands in the region's
        // server-input buffer, waiting for the matching local prediction.
        let mut neighbour_world = game::World::new();
        neighbour_world.load(&neighbour, Region::from_chunks(neighbour, Vec::new()));
        server_send
            .send(neighbour_world.build_region_server_packet(&neighbour))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();
        assert!(manager.world_ref().region_exists(&neighbour));

        // Prove the replay actually happened (not just that the region
        // loaded): tick locally once — the neighbour predicts Tick id 0 —
        // then deliver server Tick id 1. Reconcile pops the *replayed* id 0
        // from the input buffer, matches it against the prediction, and
        // forgets it. If the replay was dropped, id 1 mismatches prediction
        // id 0 and the prediction stays stuck in the event log.
        manager.send_tick();
        manager.pump_packets(&server_recv).unwrap();
        server_send
            .send(ServerPacket::GameEvent(GameEvent::new(
                GameEventKind::Tick,
                1,
                neighbour,
            )))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();
        let unconfirmed = manager
            .world_ref()
            .regions
            .get(&neighbour)
            .unwrap()
            .pending_event_ids();
        assert!(
            unconfirmed.is_empty(),
            "buffered event was not replayed: prediction(s) {:?} never confirmed",
            unconfirmed
        );
    }

    /// A snapshot for an already-loaded region replaces it (crash-respawn /
    /// resubscribe path), emitting RemoveRegion then NewRegion to the bridge.
    #[test]
    fn region_re_receipt_replaces_and_signals_bridge() {
        let (mut manager, server_send, server_recv, client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        let world = minimal_world();
        server_send
            .send(ServerPacket::PlayerRegion(Some(home), 0))
            .unwrap();
        server_send
            .send(world.build_region_server_packet(&home))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();
        while client_recv.try_recv().is_ok() {} // clear initial events

        server_send
            .send(world.build_region_server_packet(&home))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();

        let events: Vec<ClientUpdateEvent> = client_recv.try_iter().collect();
        assert!(
            matches!(events[0], ClientUpdateEvent::RemoveRegion(rc) if rc == home),
            "teardown precedes rebuild, got {:?}",
            events
        );
        assert!(matches!(events[1], ClientUpdateEvent::NewRegion(rc, _, _) if rc == home));
    }

    /// A snapshot for a region outside the desired window is released
    /// immediately, never loaded (window moved on before it arrived).
    #[test]
    fn stale_snapshot_is_released_not_loaded() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        let world = minimal_world();
        server_send
            .send(ServerPacket::PlayerRegion(Some(home), 0))
            .unwrap();
        server_send
            .send(world.build_region_server_packet(&home))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();
        while manager.client_packet_recv().try_recv().is_ok() {}

        let far = RegionCoords::new(50, 50);
        let mut far_world = game::World::new();
        far_world.load(&far, Region::from_chunks(far, Vec::new()));
        server_send
            .send(far_world.build_region_server_packet(&far))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();

        assert!(!manager.world_ref().region_exists(&far));
        let released: Vec<ClientPacket> = manager.client_packet_recv().try_iter().collect();
        assert!(released
            .iter()
            .any(|p| matches!(p, ClientPacket::ReleaseRegionConnection(rc) if *rc == far)));
    }

    /// A predicted departure in one local region synthesizes a predicted
    /// arrival in the (loaded) target and flips home_region immediately.
    #[test]
    fn predicted_crossing_synthesizes_arrival_and_flips_home() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        let target = RegionCoords::new(1, 0);
        server_send
            .send(ServerPacket::PlayerRegion(Some(home), 0))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();

        // Load home (with the player) and the empty target region.
        let mut world = game::World::new();
        let mut home_region = Region::from_chunks(home, Vec::new());
        home_region
            .handle_event(GameEventKind::CreateClient(0))
            .unwrap();
        home_region.forget_last_event();
        world.load(&home, home_region);
        server_send
            .send(world.build_region_server_packet(&home))
            .unwrap();
        let mut target_world = game::World::new();
        target_world.load(&target, Region::from_chunks(target, Vec::new()));
        server_send
            .send(target_world.build_region_server_packet(&target))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();
        manager.is_caught_up = true;

        // Teleport the local player's predicted body past the boundary.
        {
            let w = manager.world_mut();
            let key = *w.data(&home).player_entites.get(&0).unwrap();
            w.regions.get_mut(&home).unwrap().with_data(|d| {
                d.set_body_pose_safe(
                    key,
                    game::Pose::translation(game::REGION_SIZE + game::FLIP_HYSTERESIS + 2.0, 26.0, 128.0),
                )
            });
        }

        manager.send_tick();
        manager.pump_packets(&server_recv).unwrap();

        assert_eq!(
            manager.home_region,
            Some(target),
            "home flipped on prediction"
        );
        assert!(
            !manager.world_ref().data(&target).player_entites.contains_key(&0),
            "the arrival is held to the end of its slot"
        );
        // The next tick closes the slot: the arrival lands right before it.
        manager.send_tick();
        manager.pump_packets(&server_recv).unwrap();
        let w = manager.world_ref();
        assert!(
            w.data(&target).player_entites.contains_key(&0),
            "predicted arrival applied in the target region"
        );
        assert!(
            !w.data(&home).player_entites.contains_key(&0),
            "predicted extraction removed the player from the old home"
        );
    }

    /// A `Forward`ed input in the same report batch as the local player's
    /// departure (a region thread ran the Tick and the queued Predict back
    /// to back) is re-sent to the NEW home once the batch's transfers are
    /// routed — never dropped against the old one — and predicted there
    /// right after the (held) arrival.
    #[test]
    fn forwarded_input_in_departure_batch_reaches_new_home() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        let target = RegionCoords::new(1, 0);
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        manager.pump_packets(&server_recv).unwrap();
        let mut world = game::World::new();
        let mut home_region = Region::from_chunks(home, Vec::new());
        home_region.handle_event(GameEventKind::CreateClient(0)).unwrap();
        home_region.forget_last_event();
        world.load(&home, home_region);
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        let mut target_world = game::World::new();
        target_world.load(&target, Region::from_chunks(target, Vec::new()));
        server_send.send(target_world.build_region_server_packet(&target)).unwrap();
        manager.pump_packets(&server_recv).unwrap();
        manager.is_caught_up = true;
        while manager.client_packet_recv().try_recv().is_ok() {}

        // The old home's tick: the player leaves (a real departure bundle).
        let (departures, halo_entities, stamp) = {
            let w = manager.world_mut();
            let key = *w.data(&home).player_entites.get(&0).unwrap();
            let region = w.regions.get_mut(&home).unwrap();
            region.with_data(|d| {
                d.set_body_pose_safe(
                    key,
                    game::Pose::translation(game::REGION_SIZE + game::FLIP_HYSTERESIS + 2.0, 26.0, 128.0),
                )
            });
            region.handle_event(GameEventKind::Tick).unwrap();
            let (departures, halo_entities) = region.take_transfers();
            (departures, halo_entities, region.current_tick())
        };
        assert!(departures.iter().any(|(b, _)| b.client.as_ref().map(|(c, _)| *c) == Some(0)));
        let input = GameEventKind::PlayerInput(0, game::InputEvent::Key { key: game::Key::KeyW, pressed: false });
        let batch = vec![
            (
                home,
                RegionReport::Ticked {
                    sat_out: false,
                    departures,
                    halo_entities,
                    halo: Vec::new(),
                    intents: Vec::new(),
                    stamp,
                    perf: game::RegionTickPerf::default(),
                    took: Duration::ZERO,
                },
            ),
            (home, RegionReport::Forward(input.clone())),
        ];
        manager.route_reports(batch).unwrap();
        manager.drain_reports().unwrap();
        // Held behind the arrival until the tick that closes its slot (B,
        // fresh from its snapshot, is a tick behind A here).
        for _ in 0..2 {
            manager.send_tick();
            manager.pump_packets(&server_recv).unwrap();
        }

        assert_eq!(manager.home_region, Some(target), "home flipped");
        let sent: Vec<ClientPacket> = manager.client_packet_recv().try_iter().collect();
        assert!(
            sent.iter().any(|p| matches!(
                p,
                ClientPacket::GameEventAfterTick(ev, _) if ev.kind == input && ev.region_id == target
            )),
            "the forwarded input was predicted in the new home and sent: {sent:?}"
        );
    }

    /// The body crosses into a region the client does not hold (outside its
    /// window, e.g. at near radius 0): the key release forwarded from the
    /// old home, and inputs sent before the new home's snapshot arrives,
    /// still reach the server — unpredicted, stamped — so the server's body
    /// never keeps walking on a lost release; and the new home is requested.
    #[test]
    fn inputs_for_an_unloaded_home_still_reach_the_server() {
        let (mut manager, server_send, server_recv, _client_recv, game_send) = manager_with_player();
        // The play config: a one-region window, so the neighbour is not held.
        manager.set_lod_table(&[]);
        manager.set_near_radius(0);
        let home = RegionCoords::new(0, 0);
        let target = RegionCoords::new(1, 0);
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        manager.pump_packets(&server_recv).unwrap();
        let mut world = game::World::new();
        let mut home_region = Region::from_chunks(home, Vec::new());
        home_region.handle_event(GameEventKind::CreateClient(0)).unwrap();
        home_region.forget_last_event();
        world.load(&home, home_region);
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        manager.pump_packets(&server_recv).unwrap();
        manager.is_caught_up = true;
        while manager.client_packet_recv().try_recv().is_ok() {}
        assert!(!manager.host.contains(&target), "the target is not loaded");

        let (departures, halo_entities, stamp) = {
            let w = manager.world_mut();
            let key = *w.data(&home).player_entites.get(&0).unwrap();
            let region = w.regions.get_mut(&home).unwrap();
            region.with_data(|d| {
                d.set_body_pose_safe(
                    key,
                    game::Pose::translation(game::REGION_SIZE + game::FLIP_HYSTERESIS + 2.0, 26.0, 128.0),
                )
            });
            region.handle_event(GameEventKind::Tick).unwrap();
            let (departures, halo_entities) = region.take_transfers();
            (departures, halo_entities, region.current_tick())
        };
        let release = GameEventKind::PlayerInput(0, game::InputEvent::Key { key: game::Key::KeyW, pressed: false });
        let batch = vec![
            (
                home,
                RegionReport::Ticked {
                    sat_out: false,
                    departures,
                    halo_entities,
                    halo: Vec::new(),
                    intents: Vec::new(),
                    stamp,
                    perf: game::RegionTickPerf::default(),
                    took: Duration::ZERO,
                },
            ),
            (home, RegionReport::Forward(release.clone())),
        ];
        manager.route_reports(batch).unwrap();
        manager.drain_reports().unwrap();
        assert_eq!(manager.home_region, Some(target), "home flipped");
        // A fresh input while the new home is still not loaded.
        let press = GameEventKind::PlayerInput(0, game::InputEvent::Key { key: game::Key::KeyS, pressed: true });
        game_send.send(press.clone()).unwrap();
        manager.pump_packets(&server_recv).unwrap();
        manager.send_tick();
        manager.pump_packets(&server_recv).unwrap();

        let sent: Vec<ClientPacket> = manager.client_packet_recv().try_iter().collect();
        let reached = |kind: &GameEventKind| {
            sent.iter().any(|p| matches!(p, ClientPacket::GameEventAfterTick(ev, _) if ev.kind == *kind))
        };
        assert!(reached(&release), "the forwarded release reached the server: {sent:?}");
        assert!(reached(&press), "the input sent before the snapshot reached the server: {sent:?}");
        assert!(
            sent.iter().any(|p| matches!(p, ClientPacket::RequestRegionConnection(rc) if *rc == target)),
            "the new home is requested: {sent:?}"
        );
    }

    /// PlayerRegion after the first one must NOT re-request the whole 3x3
    /// window (that would resnapshot 9 regions on every crossing).
    #[test]
    fn player_region_push_updates_home_without_window_burst() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        server_send
            .send(ServerPacket::PlayerRegion(Some(home), 0))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();
        while manager.client_packet_recv().try_recv().is_ok() {}

        let target = RegionCoords::new(1, 0);
        server_send
            .send(ServerPacket::PlayerRegion(Some(target), 0))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();

        assert_eq!(manager.home_region, Some(target));
        let bursts: Vec<ClientPacket> = manager.client_packet_recv().try_iter().collect();
        assert!(
            !bursts
                .iter()
                .any(|p| matches!(p, ClientPacket::RequestRegionConnection(_))),
            "no re-subscription burst on a home push: {:?}",
            bursts
        );
    }

    /// The local player's crossing into the TARGET region, with inputs in
    /// the lag window: they follow the body. The client holds each input that
    /// reaches B while its predicted arrival is still held and predicts it
    /// right after the arrival; the server's regions put the same inputs in
    /// the same place (the old home hands on what it held or receives late).
    /// So B's predicted log is the authoritative stream: the arrival, then
    /// the inputs in their slots, and B drains bit-exact — no orphan inputs
    /// to evict, no rubber band.
    #[test]
    fn predicted_crossing_inputs_follow_the_body() {
        let (mut manager, server_send, server_recv, _client_recv, game_send) = manager_with_player();
        let home = RegionCoords::new(0, 0); // A
        let target = RegionCoords::new(1, 0); // B
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        manager.pump_packets(&server_recv).unwrap();

        // Load A (with the player) and the empty target B.
        let mut world = game::World::new();
        let mut home_region = Region::from_chunks(home, Vec::new());
        home_region.handle_event(GameEventKind::CreateClient(0)).unwrap();
        home_region.forget_last_event();
        world.load(&home, home_region);
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        let mut target_world = game::World::new();
        target_world.load(&target, Region::from_chunks(target, Vec::new()));
        server_send.send(target_world.build_region_server_packet(&target)).unwrap();
        manager.pump_packets(&server_recv).unwrap();
        manager.is_caught_up = true;

        let player_key = *manager.world_ref().data(&home).player_entites.get(&0).unwrap();
        // Deep into B: local x rebases to 128 (interior — no halo entity back to A,
        // no re-departure from B), just past A's +x boundary.
        let cross_iso = game::Pose::translation(game::REGION_SIZE + 128.0, 26.0, 128.0);
        manager
            .world_mut()
            .regions
            .get_mut(&home)
            .unwrap()
            .with_data(|d| d.set_body_pose_safe(player_key, cross_iso));

        // Crossing tick: predicted extraction + held arrival + home flip.
        manager.send_tick();
        manager.pump_packets(&server_recv).unwrap();
        assert_eq!(manager.home_region, Some(target), "home flipped on prediction");

        // Lag window: an input before each tick. The first reaches B while
        // the arrival is still held there.
        let inputs = [game::Key::KeyW, game::Key::KeyA, game::Key::KeyS, game::Key::KeyD]
            .map(|key| GameEventKind::PlayerInput(0, game::InputEvent::Key { key, pressed: true }));
        for input in &inputs {
            game_send.send(input.clone()).unwrap();
            manager.send_tick();
            manager.pump_packets(&server_recv).unwrap();
        }
        let b = manager.world_ref().regions.get(&target).unwrap();
        let predicted: Vec<GameEventKind> = b.pending_events().map(|e| e.kind.clone()).collect();
        let arrival = predicted
            .iter()
            .position(|k| matches!(k, GameEventKind::EntityArrived(_)))
            .expect("predicted arrival in B");
        assert_eq!(predicted[arrival + 1], inputs[0], "the held input right after the arrival: {predicted:?}");
        let input_at: Vec<usize> =
            inputs.iter().map(|i| predicted.iter().position(|k| k == i).expect("every input predicted in B")).collect();
        assert!(input_at.iter().all(|&i| i > arrival), "every input after the arrival: {predicted:?}");

        // The server's B: the same events, the arrival carrying the server's
        // own extraction from A (a real server-side region, faithful ids).
        let mut server_a = Region::from_chunks(home, Vec::new());
        server_a.handle_event(GameEventKind::CreateClient(0)).unwrap();
        server_a.forget_last_event();
        let sa_key = *server_a.data().player_entites.get(&0).unwrap();
        server_a.with_data(|d| d.set_body_pose_safe(sa_key, cross_iso));
        server_a.handle_event(GameEventKind::Tick).unwrap();
        let (departures, _halo) = server_a.take_transfers();
        let (mut bundle, to) = departures.into_iter().next().expect("server-side player departed A");
        assert_eq!(to, target);
        bundle.rebase(home, target);
        let mut server_b = Region::from_chunks(target, Vec::new());
        let auth: Vec<GameEvent> = predicted
            .into_iter()
            .map(|k| match k {
                GameEventKind::EntityArrived(_) => GameEventKind::EntityArrived(bundle.clone()),
                k => k,
            })
            .map(|k| server_b.handle_event(k).unwrap())
            .collect();
        for ev in auth {
            server_send.send(ServerPacket::GameEvent(ev)).unwrap();
        }
        server_send.send(ServerPacket::PlayerRegion(Some(target), 0)).unwrap();
        manager.pump_packets(&server_recv).unwrap();
        manager.pump_packets(&server_recv).unwrap();

        let b = manager.world_ref().regions.get(&target).unwrap();
        assert!(b.pending_event_ids().is_empty(), "B drains: {:?}", b.pending_event_ids());
        assert!(b.data().player_entites.contains_key(&0), "player in B");
        assert_eq!(b.current_tick(), server_b.current_tick());
        assert_eq!(
            game::state_hash(b.data()),
            game::state_hash(server_b.data()),
            "client B bit-matches the authoritative B"
        );
    }

    #[test]
    fn connection_state_maps_ready_flags() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, _client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager = GameInstanceManager::new(
            game_send,
            game_recv,
            client_send,
            dummy_addr,
            game::DebugFlags::default(),
        );

        manager.ready = false;
        manager.is_caught_up = false;
        assert_eq!(
            manager.connection_state(),
            game::ConnectionState::Connecting
        );

        manager.ready = true;
        manager.is_caught_up = false;
        assert_eq!(
            manager.connection_state(),
            game::ConnectionState::CatchingUp
        );

        manager.ready = true;
        manager.is_caught_up = true;
        assert_eq!(manager.connection_state(), game::ConnectionState::Ready);
    }

    #[test]
    fn emit_hud_status_noop_without_home() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager = GameInstanceManager::new(
            game_send,
            game_recv,
            client_send,
            dummy_addr,
            game::DebugFlags::default(),
        );

        // Drain the UiChannel announcement sent unconditionally by new().
        assert!(matches!(
            client_recv.try_recv(),
            Ok(ClientUpdateEvent::UiChannel(_))
        ));

        // No home_region / world yet: nothing to report, no event sent.
        manager.emit_hud_status();
        assert!(client_recv.try_recv().is_err());
    }

    /// The join burst is paced: each tick requests at most
    /// `LOD_REQUESTS_PER_TICK` new keys, nearest ring first, and the full
    /// production set is still reached with no key requested twice.
    #[test]
    fn lod_requests_are_paced_nearest_first() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, _client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager = GameInstanceManager::new(
            game_send,
            game_recv,
            client_send,
            dummy_addr,
            game::DebugFlags::default(),
        );
        let home = RegionCoords::new(0, 0);
        manager.home_region = Some(home);
        let packets = manager.client_packet_recv();
        let excluded = desired_window(home, Some(home), manager.near_radius, None, 0);
        let (_, expected) = desired_lod(
            home,
            &excluded,
            &BTreeMap::new(),
            game::lod::LOD_RING_TABLE,
            game::lod::LOD_STICKY_BAND,
        );
        assert!(expected.len() > LOD_REQUESTS_PER_TICK, "table must need pacing");

        let mut requested = Vec::new();
        let max_ticks = expected.len().div_ceil(LOD_REQUESTS_PER_TICK);
        for _ in 0..max_ticks {
            manager.update_lod_window();
            let batch: Vec<_> = packets
                .try_iter()
                .map(|packet| match packet {
                    ClientPacket::RequestLod(key) => key,
                    _ => panic!("only RequestLod expected"),
                })
                .collect();
            assert!(!batch.is_empty() && batch.len() <= LOD_REQUESTS_PER_TICK);
            requested.extend(batch);
        }
        // Settled: a further tick sends nothing.
        manager.update_lod_window();
        assert!(packets.try_recv().is_err());

        assert!(
            requested.windows(2).all(|pair| pair[0].factor <= pair[1].factor),
            "nearest (smallest-factor) rings must be requested first"
        );
        assert_eq!(requested.len(), expected.len(), "no key requested twice");
        assert_eq!(
            requested.into_iter().collect::<std::collections::BTreeSet<_>>(),
            expected
        );
    }

    /// `set_world_scale` drives both halves of the subscription window.
    #[test]
    fn world_scale_tiny_subscribes_home_only_and_no_lod() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, _client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager = GameInstanceManager::new(
            game_send,
            game_recv,
            client_send,
            dummy_addr,
            game::DebugFlags::default(),
        );
        manager.set_world_scale(crate::world_scale::WorldScale::TINY);
        let home = RegionCoords::new(0, 0);
        manager.home_region = Some(home);
        let packets = manager.client_packet_recv();
        manager.update_window();
        manager.update_lod_window();
        let sent: Vec<_> = packets.try_iter().collect();
        assert!(
            matches!(sent.as_slice(), [ClientPacket::RequestRegionConnection(rc)] if *rc == home),
            "tiny world must request the home region alone, got {} packets",
            sent.len()
        );
    }

    /// After a tick with a seeded local player, the manager must announce a UI
    /// channel and emit widget updates onto it (the projection ran).
    #[test]
    fn projection_emits_the_hotbar_after_tick() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager = GameInstanceManager::new(
            game_send.clone(),
            game_recv,
            client_send,
            dummy_addr,
            game::DebugFlags::default(),
        );

        // Load a region whose snapshot already contains player 0 (seeded inv).
        let region_id = RegionCoords::new(0, 0);
        let mut world = game::World::new();
        let mut home = Region::from_chunks(region_id, Vec::new());
        home.handle_event(GameEventKind::CreateClient(0)).unwrap();
        home.forget_last_event();
        world.load(&region_id, home);
        server_send
            .send(ServerPacket::PlayerRegion(Some(region_id), 0))
            .unwrap();
        server_send
            .send(world.build_region_server_packet(&region_id))
            .unwrap();
        assert!(manager.pump_packets(&server_recv).unwrap());

        // A UI channel must have been announced. Keep every other drained
        // event alive (rather than dropping it): NewRegion carries the
        // region's GameDataUpdate receiver, and dropping it here would
        // disconnect the sender the physics controller writes to on every
        // tick, panicking the *next* tick with an unrelated SendError.
        let mut kept_events = Vec::new();
        let ui_recv = {
            let mut found = None;
            while let Ok(ev) = client_recv.try_recv() {
                match ev {
                    ClientUpdateEvent::UiChannel(r) => found = Some(r),
                    other => kept_events.push(other),
                }
            }
            found.expect("UiChannel announced")
        };

        // Tick once → projection runs → the hotbar appears.
        manager.is_caught_up = true;
        manager.send_tick();
        assert!(manager.pump_packets(&server_recv).unwrap());

        let ups: Vec<game::UiUpdate> = ui_recv.try_iter().collect();
        assert!(
            ups.iter().any(|u| matches!(u, game::UiUpdate::Hotbar(_))),
            "projection must emit the hotbar, got {ups:?}"
        );

        // Another tick with nothing changed: the hotbar is not re-sent.
        manager.send_tick();
        assert!(manager.pump_packets(&server_recv).unwrap());
        let again: Vec<game::UiUpdate> = ui_recv.try_iter().collect();
        assert!(
            !again.iter().any(|u| matches!(u, game::UiUpdate::Hotbar(_))),
            "unchanged hotbar re-sent: {again:?}"
        );
    }

    /// Opening the inventory (inventory_open=true) must emit SetCursorGrab(false)
    /// so the cursor is released for pointing.
    #[test]
    fn projection_emits_cursor_release_when_inventory_opens() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager = GameInstanceManager::new(
            game_send.clone(),
            game_recv,
            client_send,
            dummy_addr,
            game::DebugFlags::default(),
        );

        let region_id = RegionCoords::new(0, 0);
        let mut world = game::World::new();
        let mut home = Region::from_chunks(region_id, Vec::new());
        home.handle_event(GameEventKind::CreateClient(0)).unwrap();
        home.forget_last_event();
        world.load(&region_id, home);
        server_send
            .send(ServerPacket::PlayerRegion(Some(region_id), 0))
            .unwrap();
        server_send
            .send(world.build_region_server_packet(&region_id))
            .unwrap();
        manager.pump_packets(&server_recv).unwrap();
        // Keep every other drained event alive (not just UiChannel):
        // NewRegion carries the region's GameDataUpdate receiver, and
        // dropping it here disconnects the sender the physics controller
        // writes to on every tick, panicking a later tick with an unrelated
        // SendError (see projection_emits_widget_updates_after_tick).
        let mut kept_events = Vec::new();
        let ui_recv = {
            let mut r = None;
            while let Ok(ev) = client_recv.try_recv() {
                match ev {
                    ClientUpdateEvent::UiChannel(rx) => r = Some(rx),
                    other => kept_events.push(other),
                }
            }
            r.unwrap()
        };
        manager.is_caught_up = true;

        // Baseline tick.
        manager.send_tick();
        manager.pump_packets(&server_recv).unwrap();
        while ui_recv.try_recv().is_ok() {}

        // Toggle the inventory open, tick, and expect a cursor-release update.
        game_send
            .send(GameEventKind::UiAction(0, game::UiAction::ToggleInventory))
            .unwrap();
        manager.send_tick();
        manager.pump_packets(&server_recv).unwrap();

        let ups: Vec<game::UiUpdate> = ui_recv.try_iter().collect();
        assert!(
            ups.iter()
                .any(|u| matches!(u, game::UiUpdate::SetCursorGrab(false))),
            "opening inventory releases the cursor, got {ups:?}"
        );
    }
}

#[cfg(test)]
mod lod_window_tests {
    use super::*;
    use game::lod::{LodKey, LOD_STICKY_BAND};
    use game::RegionCoords;
    use std::collections::{BTreeMap, BTreeSet};

    /// The default-size production table, as a fixture: these tests pin the
    /// assignment algorithm, and `game::lod::LOD_RING_TABLE` is re-cut per
    /// `REGION_CHUNKS`.
    const LOD_RING_TABLE: &[(i32, u32)] = &[(2, 2), (3, 4), (6, 8), (13, 16), (27, 32), (32, 64)];

    #[test]
    fn desired_lod_assigns_by_table_and_excludes() {
        let viewer = RegionCoords::new(0, 0);
        let mut excluded = BTreeSet::new();
        excluded.insert(RegionCoords::new(0, 0)); // near window member
        let (assigned, keys) = desired_lod(viewer, &excluded, &BTreeMap::new(), LOD_RING_TABLE, 0);
        assert!(!assigned.contains_key(&RegionCoords::new(0, 0)), "excluded");
        assert_eq!(
            assigned[&RegionCoords::new(0, 1)],
            2,
            "distance 1 -> factor 2"
        );
        assert_eq!(assigned[&RegionCoords::new(0, 3)], 4);
        assert_eq!(assigned[&RegionCoords::new(0, 6)], 8);
        assert_eq!(assigned[&RegionCoords::new(0, 13)], 16);
        assert_eq!(assigned[&RegionCoords::new(0, 27)], 32);
        assert_eq!(assigned[&RegionCoords::new(0, 32)], 64);
        assert!(
            !assigned.contains_key(&RegionCoords::new(0, 33)),
            "past radius"
        );
        // Region-keyed factors produce one key per region; tile factors dedup.
        assert!(keys.contains(&LodKey::for_region(2, RegionCoords::new(0, 1))));
        assert!(keys.contains(&LodKey::for_region(64, RegionCoords::new(0, 30))));
        let f64_keys = keys.iter().filter(|k| k.factor == 64).count();
        let f64_regions = assigned.values().filter(|&&f| f == 64).count();
        assert!(
            f64_keys * 2 < f64_regions,
            "2x2 tiles dedup regions into fewer keys"
        );
    }

    #[test]
    fn desired_lod_is_sticky_across_a_boundary_step() {
        let viewer = RegionCoords::new(0, 0);
        let none = BTreeSet::new();
        let (prev, _) = desired_lod(
            viewer,
            &none,
            &BTreeMap::new(),
            LOD_RING_TABLE,
            LOD_STICKY_BAND,
        );
        // Step the viewer +1x: region (-3, 0) is now distance 4 = bound 3 + band 1,
        // held at factor 4 by stickiness even though fresh assignment says 8.
        let (next, _) = desired_lod(
            RegionCoords::new(1, 0),
            &none,
            &prev,
            LOD_RING_TABLE,
            LOD_STICKY_BAND,
        );
        assert_eq!(
            next[&RegionCoords::new(-3, 0)],
            4,
            "held by the sticky band"
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn tick_times_window_and_live_regions() {
        let (a, b) = (RegionCoords::new(0, 0), RegionCoords::new(1, 0));
        let mut t = TickTimes::default();
        for i in 0..(game::perf::TICK_TIME_WINDOW as u64 + 50) {
            t.record(a, Duration::from_micros(i));
        }
        t.record(b, Duration::from_micros(10));
        let now = Instant::now();
        let stats = t.report(&[a, b], now).expect("first report is immediate");
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].1.samples as usize, game::perf::TICK_TIME_WINDOW, "the window keeps the newest ticks");
        assert_eq!(stats[0].1.max_us, game::perf::TICK_TIME_WINDOW as u32 + 49);
        assert!(t.report(&[a, b], now).is_none(), "throttled");
        let later = now + game::perf::TICK_TIME_REPORT_INTERVAL;
        let stats = t.report(&[b], later).unwrap();
        assert_eq!(stats.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [b], "a stopped region drops out");
    }
    use crate::netcode::ConnectPolicy;

    /// Send one connect to a port nothing listens on and wait for the
    /// session's `Disconnected` report.
    fn connect_to_dead_port(commands: &Sender<Command>) -> GameError {
        let dead = std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        let (_request_send, request_recv) = crossbeam::channel::unbounded();
        let scale = crate::world_scale::WorldScale::default();
        commands
            .send(Command::ConnectToServerAndScene(
                game_send,
                game_recv,
                client_send,
                request_recv,
                dead,
                game::DebugFlags::default(),
                scale,
                ConnectPolicy {
                    attempt_timeout: Duration::from_millis(50),
                    retry_interval: Duration::from_millis(10),
                    deadline: Duration::from_millis(150),
                },
                None,
            ))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match client_recv.recv_timeout(left) {
                Ok(ClientUpdateEvent::GameCrash(error)) => return error,
                Ok(_) => continue,
                Err(e) => panic!("no session-ended report within 2 s: {e:?}"),
            }
        }
    }

    /// A connect that gives up must end the session with one
    /// `Disconnected` report, and the game thread must take the next
    /// connect (the main menu starts sessions on the same thread).
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn connect_failure_reports_disconnected_and_thread_survives() {
        let commands = start_game_thread();
        for session in 0..2 {
            match connect_to_dead_port(&commands) {
                GameError::Disconnected(reason) => assert!(
                    reason.contains("could not connect"),
                    "session {session}: unexpected reason {reason:?}"
                ),
                other => panic!("session {session}: expected Disconnected, got {other:?}"),
            }
        }
        let _ = commands.send(Command::Quit);
    }

    /// Quit while the connect is still retrying (Back on "Connecting…"):
    /// the netcode must stop at once instead of retrying until its deadline,
    /// or it could attach to the next world's server on the same port.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn quit_during_connect_retry_stops_connect_attempts() {
        use crate::netcode::tests::datagrams_within;
        // Never answers, so the client keeps retrying; counts its Initials.
        let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let commands = start_game_thread();
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        let (_request_send, request_recv) = crossbeam::channel::unbounded();
        commands
            .send(Command::ConnectToServerAndScene(
                game_send.clone(),
                game_recv,
                client_send,
                request_recv,
                silent.local_addr().unwrap(),
                game::DebugFlags::default(),
                crate::world_scale::WorldScale::default(),
                ConnectPolicy {
                    attempt_timeout: Duration::from_millis(50),
                    retry_interval: Duration::from_millis(10),
                    deadline: Duration::from_secs(60),
                },
                None,
            ))
            .unwrap();
        assert!(
            datagrams_within(&silent, Duration::from_millis(500)) > 0,
            "the client never tried to connect"
        );

        game_send.send(GameEventKind::Quit).unwrap();
        // Let the session end and anything in flight land; then silence.
        let _ = datagrams_within(&silent, Duration::from_millis(250));
        assert_eq!(
            datagrams_within(&silent, Duration::from_millis(400)),
            0,
            "connect attempts continued after Quit"
        );
        // A Quit is not a crash: no session-ended report.
        assert!(!client_recv
            .try_iter()
            .any(|e| matches!(e, ClientUpdateEvent::GameCrash(_))));
        let _ = commands.send(Command::Quit);
    }
}
