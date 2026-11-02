//! Structured performance timeline shared by client and server.
//!
//! Both binaries mark load/join milestones with one greppable line each:
//!
//! ```text
//! [perf] event=<name> t_ms=<ms since this process started> key=value ...
//! ```
//!
//! `process_start` additionally carries `unix_ms`, which is what lets
//! `scripts/perf_capture_report.py` put client and server events on one clock.
//! Values must not contain spaces (the report splits on whitespace); write
//! region coordinates as `x,z`.
//!
//! The lines are INFO and always on, so volume is part of the contract: an
//! event may fire once per process, per region, or per batch — never per
//! chunk, per LOD key or per frame. High-rate work is summed into a periodic
//! counters line instead (`[server]`, `[client-budget]`), or reported only when
//! it crosses [`STALL_THRESHOLD`].
//!
//! `std::time::Instant` panics on `wasm32-unknown-unknown`, so there the clock
//! reads 0: events still appear, in order, without timings.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

/// Log target of the `[perf]` lines. Its own target (rather than the emitting
/// module's) so one filter directive addresses every event on either side.
pub const TARGET: &str = "perf";

/// Log target of the client's DEBUG-level periodic budget lines
/// (`[client-budget]`, `[client] mispredicts/s`). `PERF_LOG=1` raises exactly
/// this target to DEBUG; see `client::main`.
pub const BUDGET_TARGET: &str = "perf_budget";

/// A single main-thread system invocation longer than this is reported as a
/// `main_stall` event: half a 60 Hz frame, so anything that shows up here has
/// by itself cost a visible hitch. Steady-state systems sit far below it; the
/// join burst is what this exists to catch.
pub const STALL_THRESHOLD: std::time::Duration = std::time::Duration::from_millis(8);

/// Events emitted before [`init`] are held here (at most this many) and
/// flushed by it, because the client's logger is only installed once Bevy's
/// `LogPlugin` builds — after the game/netcode threads are already running.
/// Harnesses and tests never call `init`; the cap keeps that from leaking.
const EARLY_EVENT_CAP: usize = 256;

#[cfg(not(target_arch = "wasm32"))]
static PROCESS_START: OnceLock<Instant> = OnceLock::new();
/// `Some(buffer)` until `init` flushes it, `None` afterwards.
static EARLY_EVENTS: Mutex<Option<Vec<String>>> = Mutex::new(Some(Vec::new()));

/// Pin the timeline's origin. Call first thing in `main`, before any logger
/// exists; idempotent. Without it the first event pins the origin instead.
pub fn pin_origin() {
    #[cfg(not(target_arch = "wasm32"))]
    PROCESS_START.get_or_init(Instant::now);
}

/// Announce the timeline once the logger is installed: emits `process_start`
/// (whose `unix_ms` is the wall-clock time of the ORIGIN, not of this call)
/// and flushes every event buffered since. `side` is `client` or `server`.
pub fn init(side: &str) {
    let Some(early) = EARLY_EVENTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    else {
        return; // already initialised
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let unix_now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let unix_ms = unix_now.saturating_sub(t_ms());
        log::info!(
            target: TARGET,
            "[perf] event=process_start t_ms=0 side={side} unix_ms={unix_ms} pid={}",
            std::process::id()
        );
    }
    #[cfg(target_arch = "wasm32")]
    log::info!(target: TARGET, "[perf] event=process_start t_ms=0 side={side}");
    for line in early {
        log::info!(target: TARGET, "{line}");
    }
}

/// Backend of [`perf_event!`](crate::perf_event): stamps the event now, then
/// logs it — or buffers it if [`init`] has not run yet.
pub fn emit(event: &str, fields: std::fmt::Arguments) {
    let line = format!("[perf] event={event} t_ms={} {fields}", t_ms());
    {
        let mut early = EARLY_EVENTS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(buffer) = early.as_mut() {
            if buffer.len() < EARLY_EVENT_CAP {
                buffer.push(line);
            }
            return;
        }
    }
    log::info!(target: TARGET, "{line}");
}

/// Milliseconds since the origin ([`pin_origin`], else the first read).
/// Always 0 on wasm.
pub fn t_ms() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        PROCESS_START
            .get_or_init(Instant::now)
            .elapsed()
            .as_millis() as u64
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// `Instant` that is safe to construct on wasm, where it reads zero. For timing
/// code in `game` paths the embedded wasm server also runs (`WorldManager`).
pub struct Stopwatch {
    #[cfg(not(target_arch = "wasm32"))]
    start: Instant,
}

impl Stopwatch {
    pub fn start() -> Self {
        Stopwatch {
            #[cfg(not(target_arch = "wasm32"))]
            start: Instant::now(),
        }
    }

    pub fn elapsed(&self) -> std::time::Duration {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.start.elapsed()
        }
        #[cfg(target_arch = "wasm32")]
        {
            std::time::Duration::ZERO
        }
    }
}

/// Cap on `main_stall` lines per process. A join that stalls every frame for
/// a minute stays readable; the report collapses them to count / sum / max.
pub const MAIN_STALL_MAX_LINES: u32 = 2_000;

static MAIN_STALL_LINES: AtomicU32 = AtomicU32::new(0);

/// Times one invocation of a main-thread system and, on drop, reports it as
/// `main_stall system=<name> ms=<took> work=<items>` if it ran longer than
/// [`STALL_THRESHOLD`]. `work` is whatever the system iterates (events,
/// changed chunks, LOD payloads), so a stall reads as "N items cost M ms".
/// Below the threshold it costs two clock reads and logs nothing.
pub struct StallGuard {
    system: &'static str,
    watch: Stopwatch,
    work: u32,
}

impl StallGuard {
    pub fn new(system: &'static str) -> Self {
        StallGuard {
            system,
            watch: Stopwatch::start(),
            work: 0,
        }
    }

    pub fn add_work(&mut self, items: u32) {
        self.work += items;
    }
}

impl Drop for StallGuard {
    fn drop(&mut self) {
        let took = self.watch.elapsed();
        if took < STALL_THRESHOLD {
            return;
        }
        if MAIN_STALL_LINES.fetch_add(1, Ordering::Relaxed) >= MAIN_STALL_MAX_LINES {
            return;
        }
        crate::perf_event!(
            "main_stall",
            "system={} ms={} work={}",
            self.system,
            ms(took),
            self.work
        );
    }
}

/// Whether `PERF_LOG` asks for the DEBUG budget lines. Any value but `0`/empty.
pub fn budget_lines_requested() -> bool {
    std::env::var("PERF_LOG")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// Ticks each live region's tick-time window keeps (the client's F3 Debug
/// tab): 200 ticks at the 50 ms tick = the last 10 s.
pub const TICK_TIME_WINDOW: usize = 200;
/// How often the client's game thread sends the windows' summaries to the
/// render thread (`ClientUpdateEvent::RegionTickTimes`).
pub const TICK_TIME_REPORT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// One region's recent forward-tick wall times, in microseconds: the mean
/// and nearest-rank percentiles over its last [`TICK_TIME_WINDOW`] ticks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TickTimeStats {
    pub samples: u32,
    pub mean_us: u32,
    pub p50_us: u32,
    pub p95_us: u32,
    pub p99_us: u32,
    pub max_us: u32,
}

impl TickTimeStats {
    /// `None` for no samples.
    pub fn from_samples(samples_us: impl IntoIterator<Item = u32>) -> Option<Self> {
        let mut sorted: Vec<u32> = samples_us.into_iter().collect();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_unstable();
        let n = sorted.len();
        // Nearest rank: the smallest sample with at least p of them at or below it.
        let rank = |p: u32| sorted[((n as u64 * p as u64).div_ceil(100) as usize).clamp(1, n) - 1];
        let sum: u64 = sorted.iter().map(|&s| s as u64).sum();
        Some(Self {
            samples: n as u32,
            mean_us: (sum / n as u64) as u32,
            p50_us: rank(50),
            p95_us: rank(95),
            p99_us: rank(99),
            max_us: sorted[n - 1],
        })
    }
}

/// Milliseconds with one decimal, the unit every `*_ms` field uses.
pub fn ms(d: std::time::Duration) -> f64 {
    (d.as_secs_f64() * 10_000.0).round() / 10.0
}

/// Emit one timeline line: `perf_event!("name")` or
/// `perf_event!("name", "key={} other={}", a, b)`.
#[macro_export]
macro_rules! perf_event {
    ($event:expr) => {
        $crate::perf::emit($event, format_args!(""))
    };
    ($event:expr, $($fields:tt)+) => {
        $crate::perf::emit($event, format_args!($($fields)+))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_time_stats_nearest_rank() {
        assert_eq!(TickTimeStats::from_samples([]), None);
        let one = TickTimeStats::from_samples([7]).unwrap();
        assert_eq!((one.mean_us, one.p50_us, one.p99_us, one.max_us), (7, 7, 7, 7));
        // 1..=100: p50 = 50, p95 = 95, p99 = 99, mean 50 (floor of 50.5).
        let s = TickTimeStats::from_samples((1..=100).rev()).unwrap();
        assert_eq!(s, TickTimeStats { samples: 100, mean_us: 50, p50_us: 50, p95_us: 95, p99_us: 99, max_us: 100 });
        // One slow tick in 20 shows at p95's edge and in max, not in p50.
        let mut v = vec![1_000; 19];
        v.push(9_000);
        let s = TickTimeStats::from_samples(v).unwrap();
        assert_eq!((s.p50_us, s.p95_us, s.max_us, s.mean_us), (1_000, 1_000, 9_000, 1_400));
    }

    #[test]
    fn clock_is_monotonic_and_survives_a_missing_init() {
        let a = t_ms();
        let b = t_ms();
        assert!(b >= a);
    }

    #[test]
    fn ms_rounds_to_one_decimal() {
        assert_eq!(ms(std::time::Duration::from_micros(1_234)), 1.2);
        assert_eq!(ms(std::time::Duration::from_micros(1_250)), 1.3);
        assert_eq!(ms(std::time::Duration::ZERO), 0.0);
    }
}
