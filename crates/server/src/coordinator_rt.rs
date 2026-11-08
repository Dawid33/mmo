//! The world coordinator's runtime: the threadless core ([`game::Coordinator`]) on its own
//! `coordinator` thread, persisting its state into the world store under its own fence
//! ([`CoordinatorFence`]) — the **only writer** of history state. *Durable before
//! visible*: the core holds back every directive and claim-granting reply until a save
//! holding the state that produced it is committed ([`Coordinator::take_save`] →
//! [`StoreHandle::save_coordinator`] → [`Coordinator::saved`]), one save in flight at a
//! time; a failed save is retried ([`COORDINATOR_SAVE_RETRY_MS`]) and nothing gated leaves
//! meanwhile.
//!
//! In `AllInOne` the host's `WorldManager` reaches it over a crossbeam
//! [`ChannelWorldLink`] ([`start_all_in_one`]); the inline link
//! ([`game::InlineWorldLink`]) stays for tests and wasm single-player.
//!
//! A save refused as `Fenced` (another coordinator claimed the row) fences the whole
//! process: the shared [`Fenced`] flag is set — the server's main loop sees it and exits
//! [`crate::claims::EXIT_FENCED`] through `cuts::exit_world` — the link goes down and the
//! thread ends with [`ServerExit::Fenced`]. Library code never calls `process::exit`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, RecvTimeoutError, Select, Sender, TryRecvError};
use game::{CoordMsg, Coordinator, HostMsg, WorldLink};
use world_store::{CoordinatorFence, StoreError, StoreHandle};

use crate::claims::{Fenced, ServerExit};
use crate::world_file::CoordinatorStart;

/// The coordinator thread's longest wait for a host message before it runs its
/// upkeep (the calendar, the history's directives, a due save) anyway.
pub const COORDINATOR_TICK_MS: u64 = 50;

/// How long the coordinator waits after a failed save before it tries again (the
/// state stays waiting: nothing gated leaves until a save commits).
pub const COORDINATOR_SAVE_RETRY_MS: u64 = 250;

/// Failed saves in a row after which the coordinator reports saving stuck (once, at
/// error level, plus a `coordinator_save_stuck` perf event).
pub const COORDINATOR_FAILURES_BEFORE_STUCK: u32 = 3;

/// The host's end of an in-process link to the coordinator thread. FIFO each way. Down
/// once the coordinator thread stops (fenced, or its orderly stop): sends are then lost
/// and nothing is read, as on a dropped connection.
pub struct ChannelWorldLink {
    tx: Sender<HostMsg>,
    rx: Receiver<CoordMsg>,
    up: Arc<AtomicBool>,
    /// Pinged (at most one pending) after the coordinator sends a batch.
    wake: Receiver<()>,
}

impl ChannelWorldLink {
    /// A receiver that becomes ready when the coordinator has sent something: a host
    /// loop that blocks on its other channels selects on it too, so a reply does not
    /// wait for the loop's timeout. Its messages carry nothing — drain the link with
    /// [`WorldLink::recv`].
    pub fn wake(&self) -> Receiver<()> {
        self.wake.clone()
    }
}

impl WorldLink for ChannelWorldLink {
    fn send(&mut self, msg: HostMsg) {
        if !self.is_up() {
            log::debug!("coordinator link down: {msg:?} lost");
            return;
        }
        if self.tx.send(msg).is_err() {
            log::warn!("coordinator link: the coordinator thread is gone");
            self.up.store(false, Ordering::SeqCst);
        }
    }

    fn recv(&mut self) -> Option<CoordMsg> {
        if !self.is_up() {
            return None;
        }
        self.rx.try_recv().ok()
    }

    fn is_up(&self) -> bool {
        self.up.load(Ordering::SeqCst)
    }
}

/// The coordinator's end of one host's [`ChannelWorldLink`].
pub struct HostEnd {
    rx: Receiver<HostMsg>,
    tx: Sender<CoordMsg>,
    up: Arc<AtomicBool>,
    wake: Sender<()>,
    /// The host dropped its end.
    gone: bool,
}

/// A connected pair: the host's [`ChannelWorldLink`] (up) and the coordinator's [`HostEnd`].
pub fn channel_link() -> (ChannelWorldLink, HostEnd) {
    let (host_tx, host_rx) = crossbeam::channel::unbounded();
    let (coord_tx, coord_rx) = crossbeam::channel::unbounded();
    let (wake_tx, wake_rx) = crossbeam::channel::bounded(1);
    let up = Arc::new(AtomicBool::new(true));
    (
        ChannelWorldLink { tx: host_tx, rx: coord_rx, up: up.clone(), wake: wake_rx },
        HostEnd { rx: host_rx, tx: coord_tx, up, wake: wake_tx, gone: false },
    )
}

/// The hosts a coordinator serves. R2 runs one host per world (`AllInOne`, or one
/// `Host` process): every outgoing message goes to each host in the set. Per-host routing
/// is R3's.
pub struct LinkSet {
    hosts: Vec<HostEnd>,
}

impl LinkSet {
    pub fn one(host: HostEnd) -> Self {
        LinkSet { hosts: vec![host] }
    }
}

/// How the coordinator thread is asked to stop.
enum Stop {
    /// Handle what the hosts sent before the stop, save once more (forced), and wait for
    /// the save at most this long.
    Save(Duration),
    /// Stop at once: no final save (a fenced process; the next start loads the last save).
    WithoutSave,
}

/// The running coordinator thread.
pub struct CoordinatorThread {
    join: std::thread::JoinHandle<ServerExit>,
    stop: Sender<Stop>,
}

impl CoordinatorThread {
    /// The orderly stop: every host message sent before this call is handled, the state
    /// is saved once more (waiting at most `timeout` for it), and the link goes down.
    /// [`ServerExit::Fenced`] if a save was refused as fenced (now or earlier).
    pub fn shutdown(self, timeout: Duration) -> ServerExit {
        let _ = self.stop.send(Stop::Save(timeout));
        self.join()
    }

    /// Stop without a final save (the process was fenced, or a test drops the
    /// coordinator mid-play); the store keeps its last committed save.
    pub fn stop_without_save(self) -> ServerExit {
        let _ = self.stop.send(Stop::WithoutSave);
        self.join()
    }

    /// The thread ended on its own (fenced, or it panicked).
    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }

    fn join(self) -> ServerExit {
        self.join.join().unwrap_or_else(|_| {
            log::error!("[coordinator] the coordinator thread panicked");
            ServerExit::Clean
        })
    }
}

/// The coordinator for an `AllInOne` process: claims the coordinator row (fencing any
/// coordinator that held it), and runs `start` on its thread over a fresh
/// [`ChannelWorldLink`] — the host's end, to install with `WorldManager::set_world_link`.
pub fn start_all_in_one(
    store: StoreHandle,
    start: CoordinatorStart,
    fenced: Fenced,
) -> Result<(ChannelWorldLink, CoordinatorThread), String> {
    let fence = store.claim_coordinator().map_err(|e| format!("claiming the world coordinator: {e}"))?;
    log::info!("[coordinator] claimed the world coordinator (epoch {}), state {:?}", fence.epoch, start.source);
    let (link, host) = channel_link();
    let thread = CoordinatorRuntime::spawn(store, fence, start, LinkSet::one(host), fenced);
    Ok((link, thread))
}

/// The coordinator core with its store and fence (see the module docs).
pub struct CoordinatorRuntime {
    core: Coordinator,
    store: StoreHandle,
    fence: CoordinatorFence,
    /// The answer of the save in flight.
    pending_save: Option<Receiver<Result<(), StoreError>>>,
    links: LinkSet,
    fenced: Fenced,
    /// The core's `now_ms` 0 (its calendar was anchored there).
    started: Instant,
    /// No save is taken before this `now_ms` (after a failure).
    retry_at_ms: u64,
    failed_in_a_row: u32,
}

impl CoordinatorRuntime {
    /// Runs `start`'s coordinator on a new `coordinator` thread serving `links`, saving
    /// into `store` under `fence`. A migrated or bootstrapped state is saved at once.
    pub fn spawn(
        store: StoreHandle,
        fence: CoordinatorFence,
        start: CoordinatorStart,
        links: LinkSet,
        fenced: Fenced,
    ) -> CoordinatorThread {
        let (stop_tx, stop_rx) = crossbeam::channel::bounded(1);
        let mut core = start.coordinator;
        if start.source.needs_save() {
            core.request_save();
        }
        let runtime = CoordinatorRuntime {
            core,
            store,
            fence,
            pending_save: None,
            links,
            fenced,
            started: Instant::now(),
            retry_at_ms: 0,
            failed_in_a_row: 0,
        };
        let join = std::thread::Builder::new()
            .name("coordinator".into())
            .spawn(move || runtime.run(stop_rx))
            .expect("spawn the coordinator thread");
        CoordinatorThread { join, stop: stop_tx }
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn run(mut self, stop: Receiver<Stop>) -> ServerExit {
        loop {
            self.wait(&stop);
            // The stop is read before the hosts are drained: whatever a host sent
            // before asking to stop is already queued and is handled below.
            let stopping = match stop.try_recv() {
                Ok(s) => Some(s),
                Err(TryRecvError::Empty) => None,
                // The handle is gone (its owner unwound): stop as an orderly exit would.
                Err(TryRecvError::Disconnected) => Some(Stop::Save(crate::EXIT_SAVE_TIMEOUT)),
            };
            let now = self.now_ms();
            self.drain_hosts(now);
            self.core.maintain(now);
            if self.pump_save(now).is_err() {
                return self.fence_out();
            }
            self.send_out();
            match stopping {
                None => {}
                Some(Stop::WithoutSave) => {
                    log::info!("[coordinator] stopped without a final save");
                    self.down();
                    return ServerExit::Clean;
                }
                Some(Stop::Save(timeout)) => return self.finish(timeout),
            }
        }
    }

    /// Block until a host message, the save's answer or a stop arrives, at most
    /// [`COORDINATOR_TICK_MS`]. Nothing is taken off the channels here.
    fn wait(&self, stop: &Receiver<Stop>) {
        let mut sel = Select::new();
        sel.recv(stop);
        for h in self.links.hosts.iter().filter(|h| !h.gone) {
            sel.recv(&h.rx);
        }
        if let Some(save) = &self.pending_save {
            sel.recv(save);
        }
        let _ = sel.ready_timeout(Duration::from_millis(COORDINATOR_TICK_MS));
    }

    /// Hand every queued host message to the core, in each host's order.
    fn drain_hosts(&mut self, now: u64) {
        for i in 0..self.links.hosts.len() {
            loop {
                let host = &mut self.links.hosts[i];
                if host.gone {
                    break;
                }
                match host.rx.try_recv() {
                    Ok(msg) => self.core.handle(msg, now),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        log::info!("[coordinator] a host closed its link");
                        host.gone = true;
                    }
                }
            }
        }
    }

    /// Settle the save in flight (if answered) and start the next one when due. `Err`:
    /// fenced.
    fn pump_save(&mut self, now: u64) -> Result<(), ()> {
        if let Some(reply) = &self.pending_save {
            let answer = match reply.try_recv() {
                Err(TryRecvError::Empty) => return Ok(()),
                Ok(answer) => answer,
                Err(TryRecvError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
            };
            self.pending_save = None;
            self.settle(answer, now)?;
        }
        if now < self.retry_at_ms {
            return Ok(());
        }
        if let Some(state) = self.core.take_save() {
            self.pending_save = Some(self.store.save_coordinator(state.encode(), self.fence));
        }
        Ok(())
    }

    /// The answer of the save in flight. `Err`: fenced.
    fn settle(&mut self, answer: Result<(), StoreError>, now: u64) -> Result<(), ()> {
        match answer {
            Ok(()) => {
                self.core.saved();
                self.failed_in_a_row = 0;
                Ok(())
            }
            Err(StoreError::Fenced { .. }) => Err(()),
            Err(e) => {
                self.core.save_failed();
                self.failed_in_a_row += 1;
                self.retry_at_ms = now + COORDINATOR_SAVE_RETRY_MS;
                if self.failed_in_a_row == COORDINATOR_FAILURES_BEFORE_STUCK {
                    log::error!(
                        "[coordinator] saving is stuck: {} saves in a row failed, the last with: {e} \
                         (directives and claim replies wait until one commits)",
                        self.failed_in_a_row
                    );
                    game::perf_event!("coordinator_save_stuck", "failed={}", self.failed_in_a_row);
                } else {
                    log::warn!("[coordinator] save failed ({} in a row): {e}; retrying", self.failed_in_a_row);
                }
                Ok(())
            }
        }
    }

    /// What the core lets out, to every host, in order.
    fn send_out(&mut self) {
        let out = self.core.take_out();
        if out.is_empty() {
            return;
        }
        for host in self.links.hosts.iter().filter(|h| !h.gone) {
            for msg in &out {
                let _ = host.tx.send(msg.clone());
            }
            let _ = host.wake.try_send(());
        }
    }

    /// The orderly stop's save: the save in flight is settled, then one more (forced)
    /// holding everything handled, each waited for until `timeout` runs out.
    fn finish(mut self, timeout: Duration) -> ServerExit {
        let deadline = Instant::now() + timeout;
        let mut forced = false;
        loop {
            if let Some(reply) = self.pending_save.take() {
                let left = deadline.saturating_duration_since(Instant::now());
                let answer = match reply.recv_timeout(left) {
                    Ok(answer) => answer,
                    Err(RecvTimeoutError::Timeout) => {
                        log::error!("[coordinator] the world store did not answer the save at exit within {timeout:?}");
                        break;
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        Err(StoreError::Backend("the world-store thread has stopped".into()))
                    }
                };
                let failed = answer.is_err();
                if self.settle(answer, 0).is_err() {
                    return self.fence_out();
                }
                if forced {
                    if failed {
                        log::error!("[coordinator] the coordinator state was not saved at exit; it restarts from its last save");
                    } else {
                        log::info!("[coordinator] coordinator state saved");
                    }
                    break;
                }
            }
            self.core.request_save();
            match self.core.take_save() {
                Some(state) => {
                    self.pending_save = Some(self.store.save_coordinator(state.encode(), self.fence));
                    forced = true;
                }
                None => break,
            }
        }
        // Whatever the last save released (the hosts may still read it while they stop).
        self.send_out();
        self.down();
        ServerExit::Clean
    }

    fn fence_out(&mut self) -> ServerExit {
        log::error!("[coordinator] the coordinator's save was refused as fenced: another coordinator took over; stopping");
        self.fenced.set();
        self.down();
        ServerExit::Fenced
    }

    /// Every host's link goes down.
    fn down(&self) {
        for host in &self.links.hosts {
            host.up.store(false, Ordering::SeqCst);
        }
    }
}
