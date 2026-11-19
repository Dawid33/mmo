//! The countdown shown while the player's home host drains (`ServerPacket::HostDraining`).
//!
//! The game thread (`instance.rs`) owns the deadline and the predicted world tick and sends
//! `ClientUpdateEvent::HostDraining { remaining_secs }` only when the whole seconds left change
//! (so once per second, never per frame, never from a wall clock). This resource is the render
//! side's copy, reset with the session; the overlay pushes its line into `drain-line`.

use bevy::prelude::Resource;

/// Whole seconds until the home host leaves; `None`: no drain is known.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DrainNotice {
    pub remaining_secs: Option<u64>,
}

impl DrainNotice {
    /// The line to draw (`""` draws nothing).
    pub fn line(&self) -> String {
        self.remaining_secs.map(drain_line).unwrap_or_default()
    }
}

/// The game thread's side: the deadline (a world tick) the home host leaves at, and what the
/// render side was last told. Pure state, no Bevy and no clock.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainDeadline {
    at_tick: Option<u64>,
    told: Option<u64>,
}

impl DrainDeadline {
    /// A deadline from the HOME connection. Taken only when none is known or it is earlier
    /// (a mark cannot be removed; the deadline only moves earlier). Returns whether it was taken.
    pub fn offer(&mut self, at_tick: u64) -> bool {
        if self.at_tick.is_some_and(|cur| at_tick >= cur) {
            return false;
        }
        self.at_tick = Some(at_tick);
        true
    }

    /// The home moved or the session was lost: no drain is known. The next [`poll`](Self::poll)
    /// tells the render side.
    pub fn clear(&mut self) {
        self.at_tick = None;
    }

    /// What to tell the render side now, if it changed: `Some(Some(secs))` when the whole
    /// seconds left differ from the last told, `Some(None)` after a clear, `None` otherwise.
    /// `now_tick` is the predicted world tick (`None` before the clock starts).
    pub fn poll(&mut self, now_tick: Option<u64>) -> Option<Option<u64>> {
        let want = match (self.at_tick, now_tick) {
            (Some(at), Some(now)) => Some(game::drain_remaining_secs(at, now)),
            (Some(_), None) => return None,
            (None, _) => None,
        };
        if want == self.told {
            return None;
        }
        self.told = want;
        Some(want)
    }
}

/// The line for `remaining_secs` seconds left (`0`: the move is under way).
pub fn drain_line(remaining_secs: u64) -> String {
    if remaining_secs == 0 {
        return "This area is moving to another server\u{2026}".to_string();
    }
    format!(
        "This area moves to another server in {}:{:02}. You will reconnect for a moment.",
        remaining_secs / 60,
        remaining_secs % 60
    )
}

