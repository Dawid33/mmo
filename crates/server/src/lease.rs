//! A host's lease on its `hosts` row (R4): the process's registration ([`register`]),
//! and the heartbeat its MAIN loop pumps ([`Heartbeat`]). The
//! coordinator declares a host dead from the beats it stops reading (`game::LeaseWatch`).

use game::HostId;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, TryRecvError};
use game::{CrashPolicy, HOST_HEARTBEAT_MS};
use crate::claims::Fenced;
use world_store::{Beat, NewHost, Registered, StoreError, StoreHandle};

/// At most one warning per this many ms about heartbeats that fail.
pub const HEARTBEAT_WARN_MS: u64 = 10_000;

/// What a heartbeat pump knows of the lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseState {
    /// No beat said otherwise: run on (a beat that failed is not a loss).
    Held,
    /// The row is no longer this live boot (a takeover marked it dead, or another boot of
    /// the ordinal registered): the process is fenced.
    Replaced,
}

/// The lease beat of one host boot. Pumped from the host's main loop every pass — never from
/// a timer thread: a hung main loop must stop beating although its sockets live, so that the
/// coordinator sees it lapse. Never blocks: one beat in flight at a time, its answer read
/// when it is there.
pub struct Heartbeat {
    store: StoreHandle,
    id: HostId,
    boot: u64,
    /// When the next beat is due (`None`: at the first pump).
    next_at: Option<Instant>,
    pending: Option<Receiver<Result<Beat, StoreError>>>,
    warned_at: Option<Instant>,
    replaced: bool,
    /// The drain mark the last successful beat reported ([`Self::drain_in_ms`]).
    drain_in_ms: Option<i64>,
    /// Between two beats ([`HOST_HEARTBEAT_MS`]; a real-time test shortens it with the lease).
    period: Duration,
}

impl Heartbeat {
    pub fn new(store: StoreHandle, id: HostId, boot: u64) -> Self {
        let period = Duration::from_millis(HOST_HEARTBEAT_MS);
        Heartbeat { store, id, boot, next_at: None, pending: None, warned_at: None, replaced: false, drain_in_ms: None, period }
    }

    /// The row's drain mark as the last successful beat reported it (`worldctl drain`): ms
    /// until this host is to leave, computed by the database at that beat (negative: past due);
    /// `None`: not draining, or no beat answered yet. It is as old as that beat (at most about
    /// [`HOST_HEARTBEAT_MS`] while beats get through); a failed beat leaves it as it was. Read
    /// it from the main loop after [`pump_or_fence`]; no store read of its own.
    pub fn drain_in_ms(&self) -> Option<i64> {
        self.drain_in_ms
    }

    /// Test hook: a beat every `period` instead of [`HOST_HEARTBEAT_MS`] (with a lease the test
    /// shortened in the store and the coordinator too).
    #[cfg(feature = "test-hooks")]
    pub fn set_period(&mut self, period: Duration) {
        self.period = period;
    }

    /// One beat NOW, waited for (the boot's long steps between its registration and the main
    /// loop's first pump — re-claiming its rows behind a lock, the directory, the stored
    /// regions, the in-flight restore — may together take longer than the lease: a boot
    /// declared dead before it ever ran). Never from the main loop. `Replaced`: the row is no
    /// longer this live boot; a store error is logged (`what`: the step just done) and is
    /// not `Replaced`.
    pub fn beat_now(&mut self, what: &str) -> LeaseState {
        if self.replaced {
            return LeaseState::Replaced;
        }
        match self.store.heartbeat(self.id, self.boot, game::HostLoad::default()) {
            Ok(Beat { live: true, drain_in_ms }) => {
                self.drain_in_ms = drain_in_ms;
                LeaseState::Held
            }
            Ok(Beat { live: false, .. }) => {
                self.replaced = true;
                LeaseState::Replaced
            }
            Err(e) => {
                log::warn!("[lease] host {}: the heartbeat after {what} failed ({e}); the lease lapses unless one gets through", self.id);
                LeaseState::Held
            }
        }
    }

    /// Reads a beat's answer if it is there and sends the next beat when one is due (every
    /// [`HOST_HEARTBEAT_MS`], none while one is in flight). `Replaced` once a beat answered
    /// that the row is not this live boot (it stays so); a store error is logged (at most
    /// once per [`HEARTBEAT_WARN_MS`]) and is not `Replaced`.
    ///
    /// `load` is called only when a beat is sent; what it returns is the row's load and the
    /// `/metrics` gauges (`mmo_host_load_*`).
    pub fn pump(&mut self, now: Instant, load: &mut dyn FnMut() -> game::HostLoad) -> LeaseState {
        if self.replaced {
            return LeaseState::Replaced;
        }
        if let Some(rx) = &self.pending {
            let failed = match rx.try_recv() {
                Err(TryRecvError::Empty) => return LeaseState::Held,
                Ok(Ok(Beat { live: true, drain_in_ms })) => {
                    self.drain_in_ms = drain_in_ms;
                    None
                }
                Ok(Ok(Beat { live: false, .. })) => {
                    self.pending = None;
                    self.replaced = true;
                    return LeaseState::Replaced;
                }
                Ok(Err(e)) => Some(e.to_string()),
                Err(TryRecvError::Disconnected) => Some("the world-store thread has stopped".to_string()),
            };
            self.pending = None;
            if let Some(why) = failed {
                if self.warned_at.is_none_or(|at| now >= at + Duration::from_millis(HEARTBEAT_WARN_MS)) {
                    self.warned_at = Some(now);
                    log::warn!("[lease] host {}: a heartbeat failed ({why}); the lease lapses unless one gets through", self.id);
                }
            }
        }
        if self.next_at.is_none_or(|at| now >= at) {
            self.next_at = Some(now + self.period);
            let load = load();
            crate::health::Health::global().host_load(&load);
            self.pending = Some(self.store.heartbeat_later(self.id, self.boot, load));
        }
        LeaseState::Held
    }
}

/// The host main loop's lease step: pumps `heartbeat` (if this process holds a lease) and,
/// once it answers `Replaced`, logs it and sets `fenced` (the exit then refuses its sessions
/// with `FENCED_REFUSAL` and saves nothing). `true`: the loop must end.
pub fn pump_or_fence(
    heartbeat: &mut Option<Heartbeat>,
    fenced: &Fenced,
    now: Instant,
    load: &mut dyn FnMut() -> game::HostLoad,
) -> bool {
    let Some(beat) = heartbeat.as_mut() else { return false };
    if beat.pump(now, load) == LeaseState::Held {
        return false;
    }
    log::error!(
        "[server] replaced: host {}'s hosts row is no longer this live boot (declared dead, or another boot \
         of its ordinal registered); stopping",
        beat.id
    );
    fenced.set();
    true
}

/// A booting host's lease step after a long start-up step (`what`): one waited beat
/// ([`Heartbeat::beat_now`]), so the time from its registration to its main loop's first
/// pump cannot outlast the lease. A boot replaced meanwhile exits fenced here.
pub fn beat_at_boot(heartbeat: &mut Option<Heartbeat>, what: &str) {
    let Some(beat) = heartbeat.as_mut() else { return };
    if beat.beat_now(what) == LeaseState::Replaced {
        log::error!(
            "[server] replaced while starting (after {what}): host {}'s hosts row is no longer this live boot; stopping",
            beat.id
        );
        std::process::exit(crate::claims::EXIT_FENCED);
    }
}

/// Registers `host` (the process's ONE `register_host`) under `policy`: the store issues a
/// new id, never one a process had before, and a boot never waits (Elastic 1). Everything
/// keyed by a host — the fence, the ownership, the heartbeat, the hellos, the cut parts — is
/// built from the returned id. `stop` is looked at once before it (`Ok(None)`: the caller
/// exits 0).
pub fn register(
    store: &StoreHandle,
    host: NewHost,
    policy: CrashPolicy,
    stop: &AtomicBool,
) -> Result<Option<Registered>, StoreError> {
    if stop.load(Ordering::SeqCst) {
        log::info!("[lease] host {:?}: stop signal before registering; exiting", host.name);
        return Ok(None);
    }
    crate::health::Health::global().set_waiting(None);
    store.register_host(host, policy).map(Some)
}

/// The start-up error `server::run` stops with (exit 1) when this process's registration is
/// refused: a `static` host beside a live row (`StoreError::NotAlone`) names
/// `VOXEL_OWNERSHIP=claim` through the store's message.
pub fn registration_refusal(e: &StoreError) -> String {
    format!("registering this host: {e}")
}

