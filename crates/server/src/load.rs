//! A host's tick load (Elastic 3 Task 1): the worst tick compute time its region threads saw
//! since the last heartbeat, reported in the `hosts` row ([`game::HostLoad::tick_permille`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The most a window reports: ten times the tick period.
pub const TICK_PERMILLE_MAX: u32 = 10_000;

/// The worst tick compute time (µs) of the current window, shared by every region thread of
/// the process.
#[derive(Clone, Default)]
pub struct TickLoad(Arc<AtomicU64>);

impl TickLoad {
    /// A region thread calls this once per tick with the tick's compute time.
    pub fn note(&self, compute: Duration) {
        let us = u64::try_from(compute.as_micros()).unwrap_or(u64::MAX);
        self.0.fetch_max(us, Ordering::Relaxed);
    }

    /// The worst tick since the last call, over `period_ms`, ×1000, capped at 10_000. Resets the window.
    pub fn take_permille(&self, period_ms: u64) -> u32 {
        let us = self.0.swap(0, Ordering::Relaxed);
        let permille = us.saturating_mul(1000) / period_ms.max(1).saturating_mul(1000);
        u32::try_from(permille.min(u64::from(TICK_PERMILLE_MAX))).unwrap_or(TICK_PERMILLE_MAX)
    }
}

/// The load one heartbeat reports: sessions held, regions running, and the worst tick of the
/// window since the last beat.
pub fn host_load<S: game::RegionSpawner>(manager: &game::WorldManager<S>, tick: &TickLoad) -> game::HostLoad {
    let depths = manager.depths();
    game::HostLoad {
        players: u32::try_from(depths.sessions).unwrap_or(u32::MAX),
        regions: u32::try_from(depths.running).unwrap_or(u32::MAX),
        tick_permille: tick.take_permille(game::TICK_RATE),
    }
}

