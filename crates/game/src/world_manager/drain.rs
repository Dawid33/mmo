//! A host marked to drain (Elastic 1 Task 8; `worldctl drain`): the deadline it leaves the
//! world at, as a world tick, and the home sessions told it ([`ServerPacket::HostDraining`]).
//! Only that: the server's main loop computes the deadline from the heartbeat's mark
//! ([`WorldManager::drain_at`]), and once it is due ([`WorldManager::drain_due`]) leaves
//! through the existing leave path (`server::cuts`) — the players park, a leave cut, the
//! leaving refusal; the land goes to another host through a `Leave` succession.
//!
//! **The deadline only moves earlier.** The mark the heartbeat reports ages by up to one beat
//! between beats, so a deadline recomputed from it comes out a little LATER each pass (ignored)
//! and, at a fresh beat, a little earlier than the one held: an earlier one within
//! [`DRAIN_RETELL_TOLERANCE_TICKS`] is that ageing, not a new notice, and is ignored too (the
//! clients are not told again every beat). An operator who shortens the countdown
//! (`worldctl drain` again) moves it earlier by more: every told session is told again.
//!
//! **Who is told.** Every session whose home is this host ([`SessionKind::Home`]), once per
//! deadline, at the upkeep after it became so (a join, a hand-off in, an observer session the
//! body came to): [`WorldManager::maintain`] sweeps them. Never an observer session; a session
//! handed off to another host (it became an observer here) is forgotten and told nothing more;
//! a session that ends is forgotten (`end_session`). The drain is manager state, not region
//! sim state: nothing of it is hashed, saved or sent to another host.

use std::collections::BTreeMap;

use super::{RegionSpawner, SessionKind, WorldManager};
use crate::{ClientId, ServerPacket, Tick, HOST_HEARTBEAT_MS, TICK_RATE};

/// An earlier deadline within this many ticks of the one held is the heartbeat mark's ageing
/// (one beat period's worth), not a shortened notice: kept as it is, nobody told again.
pub const DRAIN_RETELL_TOLERANCE_TICKS: Tick = HOST_HEARTBEAT_MS / TICK_RATE;

/// Whole seconds from world tick `now_tick` to the drain deadline `at_tick` (`TICK_RATE` is a
/// tick's period in ms), rounded up; 0 once it has passed.
pub fn drain_remaining_secs(at_tick: u64, now_tick: u64) -> u64 {
    at_tick.saturating_sub(now_tick).saturating_mul(TICK_RATE).div_ceil(1000)
}

/// The drain of this host ([`WorldManager::drain_at`]).
#[derive(Debug, Default)]
pub(super) struct Drain {
    /// The world tick this host leaves at.
    at_tick: Tick,
    /// Home sessions told, with the deadline each was told.
    told: BTreeMap<ClientId, Tick>,
}

impl<S: RegionSpawner> WorldManager<S> {
    /// This host leaves the world at world tick `at_tick` (the server's main loop, from each
    /// beat's drain mark). Idempotent; only ever moves the deadline earlier, and an earlier one
    /// within [`DRAIN_RETELL_TOLERANCE_TICKS`] is ignored (see the module docs). Tells the home
    /// sessions at once.
    pub fn drain_at(&mut self, at_tick: Tick) {
        let now = self.spawner.world_tick();
        match self.drain.as_mut() {
            None => {
                log::info!(
                    "[server] this host is draining: it leaves the world at world tick {at_tick} (in {} s); its players are told",
                    drain_remaining_secs(at_tick, now)
                );
                self.drain = Some(Drain { at_tick, told: BTreeMap::new() });
            }
            Some(d) if at_tick.saturating_add(DRAIN_RETELL_TOLERANCE_TICKS) < d.at_tick => {
                log::info!(
                    "[server] the drain deadline moved earlier: world tick {at_tick} (in {} s, was {}); its players are told again",
                    drain_remaining_secs(at_tick, now),
                    d.at_tick
                );
                d.at_tick = at_tick;
            }
            // Later, or earlier by no more than the mark's ageing between beats.
            Some(_) => return,
        }
        self.tell_drain();
    }

    /// The deadline of this host's drain (a world tick); `None`: not draining.
    pub fn draining(&self) -> Option<Tick> {
        self.drain.as_ref().map(|d| d.at_tick)
    }

    /// Whether this host is to leave now: the world tick reached the drain deadline AND the
    /// coordinator has applied its latest `Attach` (a leave is asked of the coordinator; with
    /// the link down nothing parks — the host keeps simulating and its players keep the notice).
    pub fn drain_due(&self) -> bool {
        self.drain.as_ref().is_some_and(|d| self.spawner.world_tick() >= d.at_tick) && self.world_attached()
    }

    /// Test support: the sessions told the drain, with the deadline each was told.
    #[doc(hidden)]
    pub fn drain_told(&self) -> Vec<(ClientId, Tick)> {
        self.drain.as_ref().map(|d| d.told.iter().map(|(c, t)| (*c, *t)).collect()).unwrap_or_default()
    }

    /// Every home session not told the current deadline is told it; told sessions that are no
    /// longer a home here are forgotten. Called by [`Self::maintain`] and [`Self::drain_at`].
    pub(super) fn tell_drain(&mut self) {
        let Some(d) = self.drain.as_mut() else { return };
        let sessions = &self.sessions;
        d.told.retain(|c, _| sessions.get(c).is_some_and(|s| s.kind == SessionKind::Home));
        for (&client, session) in sessions {
            if session.kind != SessionKind::Home || d.told.get(&client) == Some(&d.at_tick) {
                continue;
            }
            d.told.insert(client, d.at_tick);
            let _ = self.out.send((Some(client), ServerPacket::HostDraining { at_tick: d.at_tick }));
        }
    }

    /// Session `client` ended: forgotten.
    pub(super) fn drain_forget(&mut self, client: ClientId) {
        if let Some(d) = self.drain.as_mut() {
            d.told.remove(&client);
        }
    }
}
