//! The world's shared tick numbering: every region counts ticks on the same
//! clock, so a region started late joins at the world's current tick and the
//! regions of one world agree on what "tick N" means. Time is the manager's
//! explicit `now_ms`, never `Instant`, so this works on wasm and in tests.
use crate::{TICK_RATE, Tick};

/// Most ticks a region runs back to back while behind the world tick before
/// it services its inputs again.
pub const MAX_CATCH_UP_BURST: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorldClock {
    /// Wall time (ms) at which `anchor_tick` was due.
    pub anchor_ms: u64,
    pub anchor_tick: Tick,
}

impl WorldClock {
    pub fn starting_at(anchor_ms: u64, anchor_tick: Tick) -> Self {
        Self {
            anchor_ms,
            anchor_tick,
        }
    }

    /// The tick in progress at `now_ms`: `anchor_tick + (now_ms - anchor_ms) / TICK_RATE`,
    /// saturating at the anchor before it.
    pub fn tick_at(&self, now_ms: u64) -> Tick {
        self.anchor_tick + now_ms.saturating_sub(self.anchor_ms) / TICK_RATE
    }

    /// When `tick` is due (inverse of [`Self::tick_at`]); the anchor for earlier ticks.
    pub fn ms_of(&self, tick: Tick) -> u64 {
        self.anchor_ms + tick.saturating_sub(self.anchor_tick) * TICK_RATE
    }
}

/// One wall instant and the world tick due then, stored in the world row so
/// every process of a world numbers ticks the same way (R3: hosts are separate
/// processes). Unix ms, so it means the same thing in every process — which needs
/// hosts on different machines to have synchronised wall clocks (NTP): skew becomes tick skew.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClockAnchor {
    pub unix_ms: u64,
    pub tick: Tick,
}

impl ClockAnchor {
    /// The tick in progress at wall time `unix_ms`: `tick + (unix_ms - self.unix_ms) / TICK_RATE`,
    /// saturating at `tick` for an earlier time.
    pub fn tick_at_unix(&self, unix_ms: u64) -> Tick {
        self.tick + unix_ms.saturating_sub(self.unix_ms) / TICK_RATE
    }
}

impl WorldClock {
    /// A clock for a process started at wall time `now_unix_ms`, on the world's
    /// anchor: ms since the process's base instant -> world tick. Also returns the
    /// phase (ms) the caller back-dates its base instant by, so the tick edges fall
    /// at the same wall instants in every process: feed the clock `elapsed + phase`.
    pub fn from_anchor(anchor: ClockAnchor, now_unix_ms: u64) -> (WorldClock, u64) {
        let phase = now_unix_ms.saturating_sub(anchor.unix_ms) % TICK_RATE;
        (
            WorldClock::starting_at(0, anchor.tick_at_unix(now_unix_ms)),
            phase,
        )
    }
}

/// Ticks a region at `current` runs back to back to reach `target` (the world
/// tick): the whole gap, at most [`MAX_CATCH_UP_BURST`]. Never overshoots, so
/// no tick number is skipped.
pub fn ticks_due(current: Tick, target: Tick) -> u32 {
    target
        .saturating_sub(current)
        .min(u64::from(MAX_CATCH_UP_BURST)) as u32
}
