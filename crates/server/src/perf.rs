//! The periodic `[server]` counters line: what the per-region tick line cannot
//! see — manager-thread worldgen, the LOD pipeline, the shared writer task, and
//! channel backlogs. Same cadence and style as `[region (x,z)]`
//! (`region_threads.rs`), so the two interleave in one log.
//!
//! ```text
//! [server] window=5.0s sessions=1 regions=9 parked=0 | gen n=9 sum=812.0ms max=120.3ms
//!   | lod hit=0 sub=570 built=412 inflight=158 wait_terrain=0 cache=412 latency avg=310.0ms
//!   max=2210.4ms build avg=4.1ms max=30.2ms | tx pkts=450 bytes=91234567 send sum=900.1ms
//!   max=210.0ms timeouts=0 total_timeouts=0 | backlog in=0 region=0 tx=12
//!   | tiles resident=212.4MB pinned=48.0MB budget=256MB loads=3 load_errors=0
//! ```
//! (one line in the log; wrapped here.)
//!
//! * `gen` — synchronous worldgen calls on the manager thread. While one runs
//!   no packet is routed for anyone, so `max` is a routing stall.
//! * `lod latency` is submit → result (queue wait + build); `lod build` is the
//!   worker's own time. Latency far above build means the pool is the
//!   bottleneck (`LOD_BUILD_THREADS`), not the downsampler.
//! * `tx send` is time the ONE writer task spent awaiting client sends;
//!   `timeouts` counts `send to client timed out; skipping` — each one is a
//!   packet a client will never get, i.e. a silently incomplete join.
//! * `backlog` — queued-but-unhandled messages: client→manager, region→manager,
//!   manager→writer. Anything that keeps growing is the stage that cannot keep up.
//! * `tiles` — the served forged world's tile cache (only for a tiled world):
//!   decoded MB resident (pinned ones included), MB pinned by running jobs, the
//!   budget (`VOXEL_TILE_CACHE_MB`), and tiles loaded / failed loads in the
//!   window. Steady `loads` means the budget is too small for the regions in
//!   play (tiles evicted and read again); any `load_errors` is a region that
//!   failed to start.
//!
//! The counters are process-wide atomics rather than channel messages: they are
//! observability, not simulation state, and must cost nothing on the send path.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

pub struct EdgeCounters {
    tx_packets: AtomicU64,
    tx_bytes: AtomicU64,
    tx_send_ns: AtomicU64,
    tx_send_max_ns: AtomicU64,
    send_timeouts: AtomicU64,
    send_timeouts_total: AtomicU64,
    lod_builds: AtomicU64,
    lod_build_ns: AtomicU64,
    lod_build_max_ns: AtomicU64,
}

/// Fed by the writer task and the LOD workers, drained by [`ServerReport`].
pub static EDGE: EdgeCounters = EdgeCounters {
    tx_packets: AtomicU64::new(0),
    tx_bytes: AtomicU64::new(0),
    tx_send_ns: AtomicU64::new(0),
    tx_send_max_ns: AtomicU64::new(0),
    send_timeouts: AtomicU64::new(0),
    send_timeouts_total: AtomicU64::new(0),
    lod_builds: AtomicU64::new(0),
    lod_build_ns: AtomicU64::new(0),
    lod_build_max_ns: AtomicU64::new(0),
};

impl EdgeCounters {
    /// One packet handed to one client sink (a broadcast counts per sink).
    pub fn record_send(&self, bytes: usize, took: Duration, timed_out: bool) {
        let ns = took.as_nanos() as u64;
        self.tx_packets.fetch_add(1, Relaxed);
        self.tx_bytes.fetch_add(bytes as u64, Relaxed);
        self.tx_send_ns.fetch_add(ns, Relaxed);
        self.tx_send_max_ns.fetch_max(ns, Relaxed);
        if timed_out {
            self.send_timeouts.fetch_add(1, Relaxed);
            self.send_timeouts_total.fetch_add(1, Relaxed);
        }
    }

    /// One LOD job finished on a worker thread.
    pub fn record_lod_build(&self, took: Duration) {
        let ns = took.as_nanos() as u64;
        self.lod_builds.fetch_add(1, Relaxed);
        self.lod_build_ns.fetch_add(ns, Relaxed);
        self.lod_build_max_ns.fetch_max(ns, Relaxed);
    }
}

/// Queued-but-unhandled message counts, read off the crossbeam channels.
pub struct Backlog {
    pub client_events: usize,
    pub region_outputs: usize,
    pub writer: usize,
}

pub struct ServerReport {
    window_start: Instant,
    /// The tile cache's cumulative `(loads, load_errors)` at the last line.
    tiles_seen: (u64, u64),
}

impl ServerReport {
    pub fn new() -> Self {
        ServerReport {
            window_start: Instant::now(),
            tiles_seen: (0, 0),
        }
    }

    /// The `| tiles …` section of the line: empty unless the served world is
    /// a tiled forged world.
    fn tiles_section(&mut self) -> String {
        let Some(stats) = worldgen::forged::chosen().and_then(|w| w.tile_cache()).map(|c| c.stats()) else {
            return String::new();
        };
        let (loads, errors) = self.tiles_seen;
        self.tiles_seen = (stats.loads, stats.load_errors);
        let mb = |b: usize| b as f64 / (1u64 << 20) as f64;
        format!(
            " | tiles resident={:.1}MB pinned={:.1}MB budget={:.0}MB loads={} load_errors={}",
            mb(stats.resident_bytes),
            mb(stats.pinned_bytes),
            mb(stats.budget_bytes),
            stats.loads.saturating_sub(loads),
            stats.load_errors.saturating_sub(errors),
        )
    }

    /// Emit the line once per `METRICS_REPORT_INTERVAL` and reset the window.
    /// Polled from the manager loop, which wakes at least every 200 ms.
    pub fn maybe_report<S: game::RegionSpawner>(
        &mut self,
        manager: &mut game::WorldManager<S>,
        backlog: Backlog,
    ) {
        let window = self.window_start.elapsed();
        if window < game::METRICS_REPORT_INTERVAL {
            return;
        }
        self.window_start = Instant::now();
        let m = manager.take_perf_counters();
        let d = manager.depths();
        let take = |a: &AtomicU64| a.swap(0, Relaxed);
        let ns_ms = |ns: u64| ns as f64 / 1e6;
        let avg = |sum_ms: f64, n: u64| if n > 0 { sum_ms / n as f64 } else { 0.0 };

        let tx_packets = take(&EDGE.tx_packets);
        // An idle server (nobody connected, nothing running, nothing sent) says
        // nothing, like the region line when it has no ticks to report.
        if d.sessions == 0 && d.running == 0 && tx_packets == 0 && m.generated == 0 {
            return;
        }
        let lod_builds = take(&EDGE.lod_builds);
        let lod_build_ms = ns_ms(take(&EDGE.lod_build_ns));
        let tiles = self.tiles_section();
        log::info!(
            "[server] window={:.1}s sessions={} regions={} parked={} transfers_inflight={} pending_arrivals={} arrivals_despawned={} \
             items_destroyed={} intent_replies_lost={} \
             | gen n={} sum={:.1}ms max={:.1}ms \
             | lod hit={} sub={} built={} inflight={} wait_terrain={} cache={} \
             latency avg={:.1}ms max={:.1}ms build avg={:.1}ms max={:.1}ms \
             | tx pkts={} bytes={} send sum={:.1}ms max={:.1}ms timeouts={} total_timeouts={} \
             | backlog in={} region={} tx={}{}",
            window.as_secs_f32(),
            d.sessions,
            d.running,
            d.parked,
            d.transfers_in_flight,
            d.pending_arrivals,
            d.arrivals_despawned,
            d.items_destroyed,
            d.intent_replies_lost,
            m.generated,
            game::perf::ms(m.generate_total),
            game::perf::ms(m.generate_max),
            m.lod_cache_hits,
            m.lod_submitted,
            m.lod_built,
            d.lod_in_flight,
            d.lod_waiting_terrain,
            d.lod_cache,
            avg(game::perf::ms(m.lod_latency_total), m.lod_built as u64),
            game::perf::ms(m.lod_latency_max),
            avg(lod_build_ms, lod_builds),
            ns_ms(take(&EDGE.lod_build_max_ns)),
            tx_packets,
            take(&EDGE.tx_bytes),
            ns_ms(take(&EDGE.tx_send_ns)),
            ns_ms(take(&EDGE.tx_send_max_ns)),
            take(&EDGE.send_timeouts),
            EDGE.send_timeouts_total.load(Relaxed),
            backlog.client_events,
            backlog.region_outputs,
            backlog.writer,
            tiles,
        );
    }
}

impl Default for ServerReport {
    fn default() -> Self {
        Self::new()
    }
}
