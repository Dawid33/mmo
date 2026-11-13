//! Cheap counters of the `world-store` thread: how long requests waited for it and how
//! much CPU it spent encoding / decoding rows. Atomics shared between the handle and
//! the thread; nothing allocates per request. Timing uses `Instant` (monotonic).
//! Kept in nanoseconds; [`StoreCounters`] reports milliseconds (waits rounded to the
//! nearest; CPU and latency rounded up, so a nonzero value never reads 0 and, as every
//! codec call happens inside a request, `latency >= cpu` survives the rounding).

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// What the store thread did since the last [`crate::StoreHandle::take_counters`].
///
/// Three different numbers, read together:
/// * `wait` is the time in the channel before the thread's loop took the request. The
///   loop only dequeues and spawns, so it shows a blocked thread only for requests that
///   arrive DURING a decode; a burst sent at once reads about zero.
/// * `latency` is what the caller waited: sent to answer sent (database time, queueing
///   behind other tasks' decodes, and its own decode). Counted for requests that reply.
/// * `cpu` is encode / decode time on the store thread.
///
/// Reading rule: high latency with a high `cpu_ms_sum` over the window means decoding is
/// the bottleneck (the `spawn_blocking` follow-up); high latency with a low `cpu_ms_sum`
/// means the database.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreCounters {
    /// Requests the thread picked up.
    pub requests: u64,
    /// Longest wait of a request, from being sent to being picked up by the thread, ms.
    pub wait_ms_max: u64,
    /// Sum of those waits, ms.
    pub wait_ms_sum: u64,
    /// Longest end-to-end latency of a replying request, ms.
    pub latency_ms_max: u64,
    /// Sum of those latencies, ms.
    pub latency_ms_sum: u64,
    /// CPU spent in encode / decode calls (never across an `.await`), ms.
    pub cpu_ms_sum: u64,
    /// The longest single encode / decode call, ms: how long it held the thread.
    pub cpu_ms_max: u64,
}

#[derive(Default)]
pub(crate) struct Counters {
    requests: AtomicU64,
    wait_ns_max: AtomicU64,
    wait_ns_sum: AtomicU64,
    latency_ns_sum: AtomicU64,
    latency_ns_max: AtomicU64,
    cpu_ns_sum: AtomicU64,
    cpu_ns_max: AtomicU64,
}

fn ns(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// Nearest millisecond.
fn ms(ns: u64) -> u64 {
    (ns + 500_000) / 1_000_000
}

/// Millisecond rounded up: a CPU cost that happened never reads as zero, so any codec
/// call in a window reads as at least 1 ms.
fn ms_up(ns: u64) -> u64 {
    ns.div_ceil(1_000_000)
}

impl Counters {
    pub(crate) fn record_request(&self, wait: Duration) {
        let w = ns(wait);
        self.requests.fetch_add(1, Relaxed);
        self.wait_ns_sum.fetch_add(w, Relaxed);
        self.wait_ns_max.fetch_max(w, Relaxed);
    }

    fn record_latency(&self, d: Duration) {
        let l = ns(d);
        self.latency_ns_sum.fetch_add(l, Relaxed);
        self.latency_ns_max.fetch_max(l, Relaxed);
    }

    fn record_cpu(&self, d: Duration) {
        let c = ns(d);
        self.cpu_ns_sum.fetch_add(c, Relaxed);
        self.cpu_ns_max.fetch_max(c, Relaxed);
    }

    pub(crate) fn take(&self) -> StoreCounters {
        StoreCounters {
            requests: self.requests.swap(0, Relaxed),
            wait_ms_max: ms(self.wait_ns_max.swap(0, Relaxed)),
            wait_ms_sum: ms(self.wait_ns_sum.swap(0, Relaxed)),
            latency_ms_max: ms_up(self.latency_ns_max.swap(0, Relaxed)),
            latency_ms_sum: ms_up(self.latency_ns_sum.swap(0, Relaxed)),
            cpu_ms_sum: ms_up(self.cpu_ns_sum.swap(0, Relaxed)),
            cpu_ms_max: ms_up(self.cpu_ns_max.swap(0, Relaxed)),
        }
    }
}

thread_local! {
    /// The counters of the store thread, set on it at start; `None` on every other thread
    /// (codec calls there are not timed).
    static ON_THREAD: RefCell<Option<Arc<Counters>>> = const { RefCell::new(None) };
    /// Nesting depth of [`CpuSpan`]s: only the outermost span of a call counts.
    static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Counts one answered request's latency (a no-op off the store thread).
pub(crate) fn record_latency(d: Duration) {
    ON_THREAD.with(|c| {
        if let Some(c) = c.borrow().as_ref() {
            c.record_latency(d);
        }
    });
}

/// Makes the calling thread the store thread of `counters`.
pub(crate) fn install(counters: Arc<Counters>) {
    ON_THREAD.with(|c| *c.borrow_mut() = Some(counters));
}

/// Times one synchronous encode / decode call on the store thread (a no-op elsewhere).
/// Spans nest (a wrapper calling `decode`); only the outermost is counted.
pub(crate) struct CpuSpan(Option<Instant>);

impl CpuSpan {
    pub(crate) fn start() -> CpuSpan {
        if ON_THREAD.with(|c| c.borrow().is_none()) {
            return CpuSpan(None);
        }
        let outer = DEPTH.with(|d| {
            d.set(d.get() + 1);
            d.get() == 1
        });
        CpuSpan(outer.then(Instant::now))
    }
}

impl Drop for CpuSpan {
    fn drop(&mut self) {
        // `None` and `Some` both mean the span entered DEPTH, unless it was off-thread.
        if ON_THREAD.with(|c| c.borrow().is_none()) {
            return;
        }
        DEPTH.with(|d| d.set(d.get() - 1));
        if let Some(t0) = self.0 {
            let d = t0.elapsed();
            ON_THREAD.with(|c| {
                if let Some(c) = c.borrow().as_ref() {
                    c.record_cpu(d);
                }
            });
        }
    }
}
