//! A host's lease on its `hosts` row (R4): the boot's registration, which may have to wait
//! ([`register_or_wait`]), and the heartbeat its MAIN loop pumps ([`Heartbeat`]). The
//! coordinator declares a host dead from the beats it stops reading (`game::LeaseWatch`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, TryRecvError};
use game::{CrashPolicy, HOST_HEARTBEAT_MS};
use crate::claims::Fenced;
use world_store::{BootVerdict, Fence, HostBoot, HostRow, StoreError, StoreHandle, WaitFor};

/// At most one warning per this many ms about heartbeats that fail.
pub const HEARTBEAT_WARN_MS: u64 = 10_000;
/// How long a waiting boot waits between two registrations.
pub const REGISTER_RETRY_MS: u64 = 1_000;
/// At most one log line per this many ms about why a boot waits.
pub const REGISTER_WAIT_LOG_MS: u64 = 5_000;
/// How often a waiting boot looks at the stop signal.
const STOP_POLL_MS: u64 = 100;

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
    ordinal: u32,
    boot: u64,
    /// When the next beat is due (`None`: at the first pump).
    next_at: Option<Instant>,
    pending: Option<Receiver<Result<bool, StoreError>>>,
    warned_at: Option<Instant>,
    replaced: bool,
    /// Between two beats ([`HOST_HEARTBEAT_MS`]; a real-time test shortens it with the lease).
    period: Duration,
}

impl Heartbeat {
    pub fn new(store: StoreHandle, ordinal: u32, boot: u64) -> Self {
        let period = Duration::from_millis(HOST_HEARTBEAT_MS);
        Heartbeat { store, ordinal, boot, next_at: None, pending: None, warned_at: None, replaced: false, period }
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
        match self.store.heartbeat(self.ordinal, self.boot) {
            Ok(true) => LeaseState::Held,
            Ok(false) => {
                self.replaced = true;
                LeaseState::Replaced
            }
            Err(e) => {
                log::warn!("[lease] host {}: the heartbeat after {what} failed ({e}); the lease lapses unless one gets through", self.ordinal);
                LeaseState::Held
            }
        }
    }

    /// Reads a beat's answer if it is there and sends the next beat when one is due (every
    /// [`HOST_HEARTBEAT_MS`], none while one is in flight). `Replaced` once a beat answered
    /// that the row is not this live boot (it stays so); a store error is logged (at most
    /// once per [`HEARTBEAT_WARN_MS`]) and is not `Replaced`.
    pub fn pump(&mut self, now: Instant) -> LeaseState {
        if self.replaced {
            return LeaseState::Replaced;
        }
        if let Some(rx) = &self.pending {
            let failed = match rx.try_recv() {
                Err(TryRecvError::Empty) => return LeaseState::Held,
                Ok(Ok(true)) => None,
                Ok(Ok(false)) => {
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
                    log::warn!("[lease] host {}: a heartbeat failed ({why}); the lease lapses unless one gets through", self.ordinal);
                }
            }
        }
        if self.next_at.is_none_or(|at| now >= at) {
            self.next_at = Some(now + self.period);
            self.pending = Some(self.store.heartbeat_later(self.ordinal, self.boot));
        }
        LeaseState::Held
    }
}

/// The host main loop's lease step: pumps `heartbeat` (if this process holds a lease) and,
/// once it answers `Replaced`, logs it and sets `fenced` (the exit then refuses its sessions
/// with `FENCED_REFUSAL` and saves nothing). `true`: the loop must end.
pub fn pump_or_fence(heartbeat: &mut Option<Heartbeat>, fenced: &Fenced, now: Instant) -> bool {
    let Some(beat) = heartbeat.as_mut() else { return false };
    if beat.pump(now) == LeaseState::Held {
        return false;
    }
    log::error!(
        "[server] replaced: host {}'s hosts row is no longer this live boot (declared dead, or another boot \
         of its ordinal registered); stopping",
        beat.ordinal
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
            beat.ordinal
        );
        std::process::exit(crate::claims::EXIT_FENCED);
    }
}

/// Registers `row` (the boot's ONE successful `register_host`) under `policy`, asking again
/// every [`REGISTER_RETRY_MS`] while the verdict is `Wait` and logging (at most once per
/// [`REGISTER_WAIT_LOG_MS`]) why. `Ok(None)`: `stop` was set while waiting — the caller exits 0.
/// A store error (a fence refusal among them) ends the wait. `stop` is only looked at between
/// attempts: one attempt can block up to [`world_store::CLAIM_LOCK_TIMEOUT_MS`] (its lock timeout).
pub fn register_or_wait(
    store: &StoreHandle,
    row: HostRow,
    fence: &Fence,
    policy: CrashPolicy,
    stop: &AtomicBool,
) -> Result<Option<HostBoot>, StoreError> {
    let ordinal = row.ordinal;
    let mut logged_at: Option<Instant> = None;
    let health = crate::health::Health::global();
    loop {
        let verdict = store.register_host(row.clone(), fence, policy);
        let why = match verdict {
            Ok(BootVerdict::Registered(boot)) => {
                health.set_waiting(None);
                return Ok(Some(boot));
            }
            Err(e) => {
                health.set_waiting(None);
                return Err(e);
            }
            Ok(BootVerdict::Wait(WaitFor::Succession)) => {
                health.set_waiting(Some("succession"));
                "an open succession is inheriting this ordinal's land; waiting for it to settle"
            }
            Ok(BootVerdict::Wait(WaitFor::Vouched)) => {
                health.set_waiting(Some("vouched"));
                "the previous process of this ordinal has not been declared dead yet; waiting for the coordinator"
            }
        };
        let now = Instant::now();
        if logged_at.is_none_or(|at| now >= at + Duration::from_millis(REGISTER_WAIT_LOG_MS)) {
            logged_at = Some(now);
            log::warn!("[lease] host {ordinal}: {why}");
        }
        let retry_at = now + Duration::from_millis(REGISTER_RETRY_MS);
        while Instant::now() < retry_at {
            if stop.load(Ordering::SeqCst) {
                health.set_waiting(None);
                log::info!("[lease] host {ordinal}: stop signal while waiting to register; exiting");
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(STOP_POLL_MS));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ordinal: u32, boot: u64) -> HostRow {
        HostRow {
            ordinal,
            boot,
            generation: 0,
            clean: false,
            state: game::HOST_LIVE,
            seen_ms: 0,
            reg: 0,
            peer_addr: None,
            peer_cert: None,
            quic: None,
            webtransport: None,
            wt_cert_hash: None,
        }
    }

    /// Final review A-M5: a boot beats its lease between its long start-up steps — a waited
    /// beat that moves `seen_ms` for the live boot and answers `Replaced` (for good) once the
    /// row is another boot's.
    #[test]
    fn a_boot_beats_between_its_start_up_steps() {
        let store = StoreHandle::open(&world_store::StoreUrl::Memory, world_store::Dials::BUILT, world_store::OpenMode::Writer).unwrap();
        let verdict = store.register_host(row(0, 10), &Fence::default(), CrashPolicy::Rollback).unwrap();
        assert!(matches!(verdict, BootVerdict::Registered(_)), "{verdict:?}");
        let seen = || store.hosts().unwrap()[0].seen_ms;
        let first = seen();
        std::thread::sleep(Duration::from_millis(5));
        let mut beat = Heartbeat::new(store.clone(), 0, 10);
        assert_eq!(beat.beat_now("a step"), LeaseState::Held);
        assert!(seen() > first, "the waited beat moved seen_ms");
        // Another boot of the ordinal registered meanwhile: replaced, and it stays so.
        store.register_host(row(0, 11), &Fence::default(), CrashPolicy::Rollback).unwrap();
        assert_eq!(beat.beat_now("a later step"), LeaseState::Replaced);
        assert_eq!(beat.pump(Instant::now()), LeaseState::Replaced);
        store.shutdown();
    }
}
