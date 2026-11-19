//! The coordinator core's clock is the store's (Elastic 2a, decision 6): every `now_ms` the
//! runtime feeds the core — the calendar's anchor, every deadline, the cut period — is store
//! milliseconds (Postgres `clock_timestamp()`, SQLite `unixepoch('subsec')`), the clock the
//! lease already runs on, so a coordinator restarted elsewhere reads the same time line.
//!
//! The core reads no clock itself: the runtime keeps a [`StoreClock`] — the offset between
//! the store's clock and its own monotonic one, re-sampled from store answers — and passes
//! `clock.now(mono)` wherever it passes `now_ms`.
//!
//! **Downtime is not caught up.** A store clock that jumped by more than [`CALENDAR_GAP_MS`]
//! since the calendar's last sign of life — a day turn, a view handed out (a paused or
//! restarted coordinator) — re-anchors the calendar where it last stood, never earlier than a
//! host was told (`Coordinator::calendar_now`), and a stored deadline more than the gap in the
//! past restarts its count ([`rebase_deadline`], [`rebase_since`]).
//!
//! **The lease watch is not on this clock** (Elastic 2a final review, S1): its spans run on the
//! process's monotonic clock (`LeaseWatch`). A forward step of the store's clock is taken here
//! at once (never bounded: a sample is the store's word): the calendar re-anchors past the gap,
//! and a deadline it overtakes fires early — a cut aborts, an open succession's deadline rolls
//! the world back — a rollback, never a loss.

/// Three day turns ([`crate::history::DAY_MS`]): a running coordinator turns a day well
/// within it, so a longer silence of the calendar is downtime, not play. Measured from the last
/// day turn: a running coordinator whose loop stalls more than ~20 s after a turn loses that time too.
pub const CALENDAR_GAP_MS: u64 = 30_000;

/// Store time from the process's monotonic clock: the offset of the last sample, and never
/// backwards — a sample that would move it back (a slow store answer, a store clock stepped
/// back) holds it where it was until the monotonic clock catches up.
#[derive(Clone, Copy, Debug, Default)]
pub struct StoreClock {
    /// `store_ms - mono_ms` of the last sample (0: never sampled — the monotonic clock as it is).
    offset: i64,
    /// The latest time handed out before the last sample: no later answer is below it.
    last: u64,
}

impl StoreClock {
    /// The store answered `store_ms` at the process's monotonic `mono_ms`.
    pub fn sample(&mut self, store_ms: u64, mono_ms: u64) {
        self.last = self.now(mono_ms);
        self.offset = store_ms as i64 - mono_ms as i64;
    }

    /// Store ms at monotonic `mono_ms`: never below anything handed out before the last
    /// sample (for `mono_ms` not earlier than that sample's).
    pub fn now(&self, mono_ms: u64) -> u64 {
        let extrapolated = (mono_ms as i64).saturating_add(self.offset).max(0) as u64;
        extrapolated.max(self.last)
    }
}

/// A stored deadline `deadline_ms` of a count of `span_ms`, at store time `now_ms`: one more
/// than [`CALENDAR_GAP_MS`] in the past is of a coordinator that was gone — the count starts
/// over from `now_ms` (conservative, as a restart always did). Anything else stands: a deadline
/// in the future, `u64::MAX` (none), or one just past (it fires). Never moves a deadline earlier.
pub fn rebase_deadline(deadline_ms: u64, span_ms: u64, now_ms: u64) -> u64 {
    if now_ms.saturating_sub(deadline_ms) > CALENDAR_GAP_MS {
        now_ms.saturating_add(span_ms).max(deadline_ms)
    } else {
        deadline_ms
    }
}

/// A stored start of a period (the cut period's `last_ms`), at store time `now_ms`: one more
/// than [`CALENDAR_GAP_MS`] in the past starts over at `now_ms` (the period restarts its count,
/// as at a restart). Never moves it earlier.
pub fn rebase_since(since_ms: u64, now_ms: u64) -> u64 {
    if now_ms.saturating_sub(since_ms) > CALENDAR_GAP_MS { now_ms.max(since_ms) } else { since_ms }
}

