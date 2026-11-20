//! A host's tick load (Elastic 3 Task 1): the worst tick compute time its region threads saw
//! since the last heartbeat, reported in the `hosts` row ([`game::HostLoad::tick_permille`]);
//! since Elastic 5 per region too, which the beat groups by super-region
//! ([`game::HostLoad::land`]).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The most a window reports: ten times the tick period.
pub const TICK_PERMILLE_MAX: u32 = 10_000;

/// The worst tick compute time (µs) of the current window, shared by every region thread of
/// the process, and each region's own (a slot per region thread, [`TickLoad::slot`]).
#[derive(Clone, Default)]
pub struct TickLoad(
    Arc<AtomicU64>,
    Arc<Mutex<BTreeMap<game::RegionCoords, Arc<AtomicU64>>>>,
);

/// One region thread's window ([`TickLoad::slot`]): noted every tick, taken by the beat.
#[derive(Clone)]
pub struct RegionTickLoad {
    host: Arc<AtomicU64>,
    region: Arc<AtomicU64>,
}

impl RegionTickLoad {
    /// A region thread calls this once per tick with the tick's compute time.
    pub fn note(&self, compute: Duration) {
        let us = u64::try_from(compute.as_micros()).unwrap_or(u64::MAX);
        self.host.fetch_max(us, Ordering::Relaxed);
        self.region.fetch_max(us, Ordering::Relaxed);
    }
}

fn permille_of(us: u64, period_ms: u64) -> u32 {
    let permille = us.saturating_mul(1000) / period_ms.max(1).saturating_mul(1000);
    u32::try_from(permille.min(u64::from(TICK_PERMILLE_MAX))).unwrap_or(TICK_PERMILLE_MAX)
}

impl TickLoad {
    /// The window of region `rc`'s thread (a thread started again for the same region shares
    /// the slot; a slot nobody holds any more is dropped at the next take).
    pub fn slot(&self, rc: game::RegionCoords) -> RegionTickLoad {
        let mut slots = self.1.lock().unwrap_or_else(|e| e.into_inner());
        let region = slots.entry(rc).or_default().clone();
        RegionTickLoad {
            host: self.0.clone(),
            region,
        }
    }

    /// A region thread calls this once per tick with the tick's compute time (no region slot).
    pub fn note(&self, compute: Duration) {
        let us = u64::try_from(compute.as_micros()).unwrap_or(u64::MAX);
        self.0.fetch_max(us, Ordering::Relaxed);
    }

    /// The worst tick since the last call, over `period_ms`, ×1000, capped at 10_000. Resets the window.
    pub fn take_permille(&self, period_ms: u64) -> u32 {
        permille_of(self.0.swap(0, Ordering::Relaxed), period_ms)
    }

    /// Each region's worst tick since the last call, as [`Self::take_permille`]; resets them.
    pub fn take_regions(&self, period_ms: u64) -> BTreeMap<game::RegionCoords, u32> {
        let mut slots = self.1.lock().unwrap_or_else(|e| e.into_inner());
        slots.retain(|_, a| Arc::strong_count(a) > 1 || a.load(Ordering::Relaxed) != 0);
        slots
            .iter()
            .map(|(rc, a)| (*rc, permille_of(a.swap(0, Ordering::Relaxed), period_ms)))
            .collect()
    }
}

/// The load one heartbeat reports: sessions held, regions running, and the worst tick of the
/// window since the last beat.
pub fn host_load<S: game::RegionSpawner>(
    manager: &game::WorldManager<S>,
    tick: &TickLoad,
) -> game::HostLoad {
    let depths = manager.depths();
    game::HostLoad {
        players: u32::try_from(depths.sessions).unwrap_or(u32::MAX),
        regions: u32::try_from(depths.running).unwrap_or(u32::MAX),
        tick_permille: tick.take_permille(game::TICK_RATE),
        land: manager.land_load(&tick.take_regions(game::TICK_RATE)),
    }
}
