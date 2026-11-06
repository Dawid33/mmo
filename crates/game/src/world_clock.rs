//! The world's shared tick numbering: every region counts ticks on the same
//! clock, so a region started late joins at the world's current tick and the
//! regions of one world agree on what "tick N" means. Time is the manager's
//! explicit `now_ms`, never `Instant`, so this works on wasm and in tests.
use crate::{Tick, TICK_RATE};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorldClock {
    /// Wall time (ms) at which `anchor_tick` was due.
    pub anchor_ms: u64,
    pub anchor_tick: Tick,
}

impl WorldClock {
    pub fn starting_at(anchor_ms: u64, anchor_tick: Tick) -> Self {
        Self { anchor_ms, anchor_tick }
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
