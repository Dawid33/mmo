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
//! since the calendar last turned a day (a paused or restarted coordinator) re-anchors the
//! calendar at the history's own `now` (`Coordinator::calendar_now`), and a stored deadline
//! more than the gap in the past restarts its count ([`rebase_deadline`], [`rebase_since`]).

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_clock_never_runs_backwards() {
        let mut c = StoreClock::default();
        assert_eq!(c.now(5), 5, "unsampled: the monotonic clock");
        c.sample(1_000_000, 10);
        assert_eq!(c.now(10), 1_000_000);
        assert_eq!(c.now(110), 1_000_100, "extrapolated by the monotonic clock");
        // A slow answer: the store's time is behind what was handed out. Held, not stepped back.
        let before = c.now(200);
        c.sample(999_000, 200);
        assert_eq!(c.now(200), before, "a sample never moves it back");
        let mut prev = c.now(200);
        for m in (200..3_000).step_by(37) {
            let n = c.now(m);
            assert!(n >= prev, "monotonic: {n} after {prev}");
            prev = n;
        }
        // It runs again once the monotonic clock passes the held time on the new offset.
        assert_eq!(c.now(1_300), before);
        assert_eq!(c.now(1_500), 1_000_300);
        // A sample ahead moves it forward at once.
        c.sample(2_000_000, 1_600);
        assert_eq!(c.now(1_600), 2_000_000);
        assert_eq!(c.now(1_700), 2_000_100);
    }

    #[test]
    fn a_stored_deadline_older_than_the_gap_restarts_its_count() {
        const NOW: u64 = 1_763_000_000_000;
        const SPAN: u64 = 30_000;
        // Long past (a coordinator gone an hour): the count starts over.
        assert_eq!(rebase_deadline(NOW - 3_600_000, SPAN, NOW), NOW + SPAN);
        assert_eq!(rebase_deadline(NOW - CALENDAR_GAP_MS - 1, SPAN, NOW), NOW + SPAN);
        // Just past (within the gap): it fires, as it would have.
        assert_eq!(rebase_deadline(NOW - CALENDAR_GAP_MS, SPAN, NOW), NOW - CALENDAR_GAP_MS);
        assert_eq!(rebase_deadline(NOW - 1, SPAN, NOW), NOW - 1);
        // In the future, or none: untouched.
        assert_eq!(rebase_deadline(NOW + 5, SPAN, NOW), NOW + 5);
        assert_eq!(rebase_deadline(u64::MAX, SPAN, NOW), u64::MAX);
        // Never earlier, whatever the inputs.
        for d in [0, 1, NOW - 3_600_000, NOW - 30_001, NOW - 30_000, NOW, NOW + 1, u64::MAX - 1, u64::MAX] {
            for span in [0, 1, SPAN, u64::MAX] {
                for now in [0, NOW, u64::MAX] {
                    assert!(rebase_deadline(d, span, now) >= d, "{d} {span} {now}");
                }
            }
            assert!(rebase_since(d, NOW) >= d);
        }
        // A period's start: long past starts over now, otherwise kept.
        assert_eq!(rebase_since(NOW - 3_600_000, NOW), NOW);
        assert_eq!(rebase_since(NOW - 1_000, NOW), NOW - 1_000);
    }
}
