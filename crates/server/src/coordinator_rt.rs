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
//! [`ChannelWorldLink`] ([`start_all_in_one`]); as its own process
//! (`VOXEL_ROLE=coordinator`) it serves the host processes on a service-link listener
//! ([`start_listening`], the hosts' end is [`crate::roles::QuicWorldLink`]); the inline
//! link ([`game::InlineWorldLink`]) stays for tests and wasm single-player.
//!
//! A save refused as `Fenced` (another coordinator claimed the row) fences the whole
//! process: the shared [`Fenced`] flag is set — the server's main loop sees it and exits
//! [`crate::claims::EXIT_FENCED`] through `cuts::exit_world` — the link goes down and the
//! thread ends with [`ServerExit::Fenced`]. Library code never calls `process::exit`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, RecvTimeoutError, Select, Sender, TryRecvError};
use game::coordinator::Target;
use game::{CoordMsg, Coordinator, HostMsg, WorldLink};
use svc_link::{LinkEvent, LinkListener, PeerId, PeerRole, PeerSenders};
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
            log::debug!("coordinator link down: {} lost", describe_host(&msg));
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
    /// The host's ordinal, as its `Attach` names it (0 before one).
    ordinal: u32,
}

/// A connected pair: the host's [`ChannelWorldLink`] (up) and the coordinator's [`HostEnd`].
pub fn channel_link() -> (ChannelWorldLink, HostEnd) {
    let (host_tx, host_rx) = crossbeam::channel::unbounded();
    let (coord_tx, coord_rx) = crossbeam::channel::unbounded();
    let (wake_tx, wake_rx) = crossbeam::channel::bounded(1);
    let up = Arc::new(AtomicBool::new(true));
    (
        ChannelWorldLink { tx: host_tx, rx: coord_rx, up: up.clone(), wake: wake_rx },
        HostEnd { rx: host_rx, tx: coord_tx, up, wake: wake_tx, gone: false, ordinal: 0 },
    )
}

/// The hosts a coordinator serves: in-process channel links (`AllInOne`, tests), or the
/// host processes connected to its service-link listener (`VOXEL_ROLE=coordinator`).
pub struct LinkSet {
    links: Links,
}

enum Links {
    /// Every outgoing message goes to each host (R2: one host per process).
    Channels(Vec<HostEnd>),
    Listener(ListenerHosts),
}

impl LinkSet {
    pub fn one(host: HostEnd) -> Self {
        LinkSet { links: Links::Channels(vec![host]) }
    }

    /// The hosts that connect to `listener`; its events and senders are the ones
    /// [`LinkListener::bind`] returned. The listener is dropped (the port freed) when the
    /// coordinator stops.
    pub fn listener(
        listener: LinkListener,
        events: Receiver<(PeerId, LinkEvent<HostMsg>)>,
        senders: PeerSenders<CoordMsg>,
    ) -> Self {
        LinkSet {
            links: Links::Listener(ListenerHosts {
                listener: Some(listener),
                events,
                senders,
                routes: Routes::default(),
            }),
        }
    }
}

/// Which connection(s) a coordinator message goes to.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// The live connection of a host ordinal.
    Host(u32),
    /// One connection (an attach echo goes to the connection whose `Attach` it answers).
    Peer(PeerId),
    Every,
    Nobody,
}

/// The hosts on the coordinator's service-link listener.
struct ListenerHosts {
    listener: Option<LinkListener>,
    events: Receiver<(PeerId, LinkEvent<HostMsg>)>,
    senders: PeerSenders<CoordMsg>,
    routes: Routes,
}

impl ListenerHosts {
    fn send(&mut self, to: Target, msg: CoordMsg) {
        match self.routes.route(to, &msg) {
            Route::Host(ordinal) => match self.routes.peer_of(ordinal) {
                Some(peer) => {
                    if !self.senders.send(peer, msg) {
                        log::debug!("[coordinator] host {ordinal} left; a message for it is lost");
                    }
                }
                None => log::debug!("[coordinator] host {ordinal} is not connected; {} lost", describe(&msg)),
            },
            Route::Peer(peer) => {
                if !self.senders.send(peer, msg) {
                    log::debug!("[coordinator] peer {} left; its attach echo is lost", peer.0);
                }
            }
            Route::Every => {
                for peer in self.routes.hosts.keys() {
                    self.senders.send(*peer, msg.clone());
                }
            }
            // `route` said why.
            Route::Nobody => {}
        }
    }
}

/// A coordinator message's kind and region, for logs (a `SpawnPeople` or `View` can be
/// large).
fn describe(msg: &CoordMsg) -> String {
    match msg {
        CoordMsg::View { sites, .. } => format!("View ({} sites)", sites.len()),
        CoordMsg::SpawnPeople { rc, incarnation, events } => {
            format!("SpawnPeople for {rc:?} incarnation {incarnation} ({} events)", events.len())
        }
        CoordMsg::Directive { directive, incarnation } => {
            format!("Directive for {:?} incarnation {incarnation}", directive.region)
        }
        CoordMsg::Reply { id, .. } => format!("Reply to {id:?}"),
        CoordMsg::Calendar(t) => format!("Calendar({})", t.0),
        CoordMsg::Sites(s) => format!("Sites ({})", s.len()),
        CoordMsg::SiteMarkers(s) => format!("SiteMarkers ({})", s.len()),
        CoordMsg::Notice { region, .. } => format!("Notice for {region:?}"),
        CoordMsg::Attached { epoch } => format!("Attached {{ epoch: {epoch} }}"),
        CoordMsg::ClaimIs { player, claim } => format!("ClaimIs {player:?} {claim:?}"),
    }
}

/// A host message's kind and region, for logs (an `Attach` or `Positions` can be large).
pub(crate) fn describe_host(msg: &HostMsg) -> String {
    match msg {
        HostMsg::Attach { host, epoch, running, online, generation } => format!(
            "Attach {{ host: {host}, epoch: {epoch}, generation: {generation} }} ({} regions, {} online)",
            running.len(),
            online.len()
        ),
        HostMsg::RegionStarted { rc, incarnation } => format!("RegionStarted {rc:?} incarnation {incarnation}"),
        HostMsg::RegionStopping { rc } => format!("RegionStopping {rc:?}"),
        HostMsg::RegionParked { rc, people } => format!("RegionParked {rc:?} ({} people)", people.len()),
        HostMsg::PersonCrossed { person, to, delivered } => {
            format!("PersonCrossed {person:?} into {to:?} (delivered {delivered})")
        }
        HostMsg::Positions(people) => format!("Positions ({})", people.len()),
        HostMsg::DirectiveBounced { directive, incarnation } => {
            format!("DirectiveBounced for {:?} incarnation {incarnation}", directive.region)
        }
        HostMsg::Request { id, .. } => format!("Request {id:?}"),
        HostMsg::Settle { id, .. } => format!("Settle {id:?}"),
        HostMsg::PlayerOnline(p) => format!("PlayerOnline {:?}", p.player),
        HostMsg::PlayerOffline { player } => format!("PlayerOffline {player:?}"),
        HostMsg::ReleaseClaim { player } => format!("ReleaseClaim {player:?}"),
    }
}

/// What routes a coordinator message to one of the connected hosts: the core names the
/// host each message is for ([`Target`]: a reply to the host that asked, a region's
/// messages to the host that runs it, the world's view to every host); the attach echo
/// goes to the connection whose `Attach` it answers. A message for a host that is not
/// connected is lost (it re-attaches when it is back: its `Attach` and resent requests
/// cover it).
///
/// Any host ordinal is served. One live connection per ordinal ([`Routes::admit`]): a
/// host process that is replaced (a restart, a second process of the ordinal) keeps
/// running until its next cut is fenced, and its messages — a park, a crossing,
/// positions — would otherwise be applied beside its replacement's (two bodies of one
/// person).
#[derive(Default)]
struct Routes {
    /// Peers refused or replaced ([`Routes::admit`]); their messages are ignored.
    refused: BTreeSet<PeerId>,
    /// Connected host peers and their ordinals (a reconnect is a new peer).
    hosts: BTreeMap<PeerId, u32>,
    /// The live connection of each ordinal, with the claim epoch its hello carried.
    live: BTreeMap<u32, (PeerId, u64)>,
    /// `Attach`es handled whose echo has not been routed yet: (connection, epoch), in order.
    attaches: VecDeque<(PeerId, u64)>,
}

/// What [`Routes::admit`] decided.
#[derive(Debug, PartialEq, Eq)]
enum Admit {
    /// Refused: its connection is closed, its messages ignored.
    Refused,
    /// The ordinal's live connection now; `replaced` is the one it took over from (closed,
    /// its messages ignored from now on).
    Admitted { replaced: Option<PeerId> },
}

impl Routes {
    /// A host peer said hello as `ordinal` with claim `epoch`; `stored_epoch` is the
    /// ordinal's claim epoch in the store (`StoreHandle::owner_claim_epoch`; `None`: it
    /// claims nothing, epoch 0). A hello whose epoch is not the stored one is a stale host
    /// process (its claims taken over, fenced at its next cut) and is refused — read from
    /// the store, so this holds across a coordinator restart. Otherwise the newer
    /// connection of an ordinal replaces the older.
    fn admit(&mut self, peer: PeerId, ordinal: u32, epoch: u64, stored_epoch: Option<u64>) -> Admit {
        let stored = stored_epoch.unwrap_or(0);
        if epoch != stored {
            log::error!(
                "[coordinator] refusing host {ordinal} (peer {}) with claim epoch {epoch}: the store has epoch {stored} \
                 (a stale host process, fenced at its next cut)",
                peer.0
            );
            self.refused.insert(peer);
            return Admit::Refused;
        }
        let replaced = match self.live.get(&ordinal).copied() {
            Some((live, live_epoch)) => {
                log::warn!(
                    "[coordinator] host {ordinal}: peer {} (claim epoch {epoch}) replaces peer {} (epoch {live_epoch}); \
                     the old connection is closed and its messages ignored",
                    peer.0,
                    live.0
                );
                self.refuse(live);
                Some(live)
            }
            None => None,
        };
        self.live.insert(ordinal, (peer, epoch));
        self.hosts.insert(peer, ordinal);
        Admit::Admitted { replaced }
    }

    /// `peer` is refused from now on (replaced, or its `Attach` was refused): its messages
    /// are ignored and its pending attach echo dropped. The ordinal it served, if any.
    fn refuse(&mut self, peer: PeerId) -> Option<u32> {
        self.refused.insert(peer);
        self.attaches.retain(|(p, _)| *p != peer);
        let ordinal = self.hosts.remove(&peer)?;
        if self.live.get(&ordinal).is_some_and(|(p, _)| *p == peer) {
            self.live.remove(&ordinal);
        }
        Some(ordinal)
    }

    /// `peer`'s connection ended.
    fn gone(&mut self, peer: PeerId) -> Option<u32> {
        self.refused.remove(&peer);
        let ordinal = self.hosts.remove(&peer)?;
        if self.live.get(&ordinal).is_some_and(|(p, _)| *p == peer) {
            self.live.remove(&ordinal);
        }
        Some(ordinal)
    }

    /// Note what routes later messages from `msg`, sent by host `ordinal` on `peer`.
    fn note(&mut self, peer: PeerId, ordinal: u32, msg: &HostMsg) {
        if let HostMsg::Attach { host, epoch, .. } = msg {
            if *host != ordinal {
                log::warn!("[coordinator] host {ordinal}'s Attach names host {host}; routed by its link");
            }
            self.attaches.push_back((peer, *epoch));
        }
    }

    /// The connection(s) `msg`, for `to`, goes to.
    fn route(&mut self, to: Target, msg: &CoordMsg) -> Route {
        match (msg, to) {
            (CoordMsg::Attached { epoch }, _) => match self.attaches.iter().position(|(_, e)| e == epoch) {
                Some(i) => Route::Peer(self.attaches.remove(i).expect("found").0),
                None => {
                    log::warn!("[coordinator] attach echo {epoch} matches no Attach; dropped");
                    Route::Nobody
                }
            },
            (_, Target::Host(h)) => Route::Host(h),
            (_, Target::Every) => Route::Every,
        }
    }

    /// The newest connection of host `ordinal`.
    fn peer_of(&self, ordinal: u32) -> Option<PeerId> {
        self.hosts.iter().rev().find(|(_, o)| **o == ordinal).map(|(p, _)| *p)
    }
}

/// Host `ordinal`'s claim epoch in the store, for its hello's admission; `Some(u64::MAX)`
/// (refused: "wait, never guess" — the host redials) when the store cannot be read.
fn stored_claim_epoch(store: &StoreHandle, ordinal: u32) -> Option<u64> {
    store.owner_claim_epoch(ordinal).unwrap_or_else(|e| {
        log::error!("[coordinator] host {ordinal}'s claim epoch could not be read ({e}); its hello is refused");
        Some(u64::MAX)
    })
}

/// Whether host `ordinal`'s `Attach` of world `generation` may be applied: not lower than
/// the store's (a lower one is a stale process, about to exit: its link is closed). A
/// store that cannot be read refuses it too (the host redials and attaches again).
fn generation_current(store: &StoreHandle, ordinal: u32, generation: u64) -> bool {
    match store.world_generation() {
        Ok(stored) if generation < stored => {
            log::error!(
                "[coordinator] host {ordinal}'s Attach runs world generation {generation}, older than the store's \
                 {stored}: refused (a stale host process)"
            );
            false
        }
        Ok(_) => true,
        Err(e) => {
            log::error!("[coordinator] the world generation could not be read ({e}); host {ordinal}'s Attach is refused");
            false
        }
    }
}

/// How the coordinator thread is asked to stop.
enum Stop {
    /// Handle what the hosts sent before the stop, save once more (forced), and wait for
    /// the save at most this long.
    Save(Duration),
    /// Stop at once, before handling, saving or sending anything more (a fenced process;
    /// a test's crash): the next start loads the last committed save.
    WithoutSave,
}

/// A look at the running core ([`CoordinatorThread::inspect`]).
#[cfg(feature = "test-hooks")]
type Inspect = Box<dyn FnOnce(&mut Coordinator) + Send>;

/// The running coordinator thread.
pub struct CoordinatorThread {
    join: std::thread::JoinHandle<ServerExit>,
    stop: Sender<Stop>,
    #[cfg(feature = "test-hooks")]
    inspect: Sender<Inspect>,
    /// [`CoordinatorThread::crash_before_claim_reply`] is armed.
    #[cfg(feature = "test-hooks")]
    crash_before_claim_reply: Arc<AtomicBool>,
}

impl CoordinatorThread {
    /// The orderly stop: every host message sent before this call is handled, the state
    /// is saved once more (waiting at most `timeout` for it), and the link goes down.
    /// [`ServerExit::Fenced`] if a save was refused as fenced (now or earlier).
    pub fn shutdown(self, timeout: Duration) -> ServerExit {
        let _ = self.stop.send(Stop::Save(timeout));
        self.join()
    }

    /// Stop without a final save (the process was fenced): the thread returns as soon as
    /// it sees the stop, before it handles, saves or sends anything more; the store keeps
    /// its last committed save (a save already handed to the store may still commit).
    pub fn stop_without_save(self) -> ServerExit {
        let _ = self.stop.send(Stop::WithoutSave);
        self.join()
    }

    /// Test hook: the coordinator process crashes — [`Self::stop_without_save`]: nothing
    /// handled after this is saved or sent, and its hosts' links drop.
    #[doc(hidden)]
    pub fn kill(self) {
        let _ = self.stop_without_save();
    }

    /// Test hook: the coordinator crashes right after the save that releases its next
    /// claim-granting reply commits, before that reply (or anything released with it)
    /// leaves; the thread ends [`ServerExit::CoordinatorFailed`]. Wait for it with [`Self::is_finished`].
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn crash_before_claim_reply(&self) {
        self.crash_before_claim_reply.store(true, Ordering::SeqCst);
    }

    /// Test hook: `f` of the core as it stands after the host messages handled so far;
    /// `None` if the thread does not answer within `timeout` (stopped).
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn inspect<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Coordinator) -> R + Send + 'static,
        timeout: Duration,
    ) -> Option<R> {
        self.with_core(move |core: &mut Coordinator| f(core), timeout)
    }

    /// Test hook: `f` of the core, to change it (a test forcing history — the core's
    /// `history_mut`); what it decides goes out as the core's own decisions do (gated
    /// directives after their save). `None` if the thread does not answer within `timeout`.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn with_core<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Coordinator) -> R + Send + 'static,
        timeout: Duration,
    ) -> Option<R> {
        let (tx, rx) = crossbeam::channel::bounded(1);
        let ask: Inspect = Box::new(move |core| {
            let _ = tx.send(f(core));
        });
        self.inspect.send(ask).ok()?;
        rx.recv_timeout(timeout).ok()
    }

    /// The thread ended on its own (fenced, or it panicked).
    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }

    /// The thread's end; a panic is [`ServerExit::CoordinatorFailed`].
    fn join(self) -> ServerExit {
        self.join.join().unwrap_or_else(|_| {
            log::error!("[coordinator] the coordinator thread panicked");
            ServerExit::CoordinatorFailed
        })
    }
}

/// The coordinator for an `AllInOne` process: runs `start` (its row claimed before its
/// state was loaded, [`CoordinatorStart::fence`]) on its thread over a fresh
/// [`ChannelWorldLink`] — the host's end, to install with `WorldManager::set_world_link`.
pub fn start_all_in_one(
    store: StoreHandle,
    start: CoordinatorStart,
    fenced: Fenced,
) -> Result<(ChannelWorldLink, CoordinatorThread), String> {
    log::info!("[coordinator] the world coordinator (epoch {}), state {:?}", start.fence.epoch, start.source);
    let (link, host) = channel_link();
    let thread = CoordinatorRuntime::spawn(store, start, LinkSet::one(host), fenced);
    Ok((link, thread))
}

/// A coordinator serving host processes over the service link
/// ([`start_listening`]): its thread, and where and with which pin hosts dial it.
pub struct ListeningCoordinator {
    pub thread: CoordinatorThread,
    pub addr: std::net::SocketAddr,
    /// The listener certificate's SHA-256 (also written to the cert-hash file, if one).
    pub cert_sha256: String,
}

/// The coordinator for a `VOXEL_ROLE=coordinator` process (its row claimed before its
/// state was loaded, [`CoordinatorStart::fence`]): listens on `bind` for hosts presenting
/// `token` (the certificate's pin written to `cert_hash_file`, its cert and key kept
/// beside it), and runs `start` on its thread serving them.
pub fn start_listening(
    store: StoreHandle,
    start: CoordinatorStart,
    bind: std::net::SocketAddr,
    token: String,
    cert_hash_file: Option<std::path::PathBuf>,
    fenced: Fenced,
) -> Result<ListeningCoordinator, String> {
    let (listener, events, senders) = LinkListener::bind::<HostMsg, CoordMsg>(bind, token, cert_hash_file.clone())
        .map_err(|e| format!("the coordinator's service link on {bind}: {e}"))?;
    let (addr, cert_sha256) = (listener.local_addr(), listener.cert_sha256().to_string());
    log::info!(
        "[coordinator] the world coordinator (epoch {}), state {:?}; hosts dial {addr} (pin {cert_sha256}{})",
        start.fence.epoch,
        start.source,
        cert_hash_file.map(|f| format!(", in {}", f.display())).unwrap_or_default()
    );
    let thread = CoordinatorRuntime::spawn(store, start, LinkSet::listener(listener, events, senders), fenced);
    Ok(ListeningCoordinator { thread, addr, cert_sha256 })
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
    /// Test hook ([`CoordinatorThread::crash_before_claim_reply`]).
    #[cfg(feature = "test-hooks")]
    crash_before_claim_reply: Arc<AtomicBool>,
    /// The core's `now_ms` 0 (its calendar was anchored there).
    started: Instant,
    /// No save is taken before this `now_ms` (after a failure).
    retry_at_ms: u64,
    failed_in_a_row: u32,
}

impl CoordinatorRuntime {
    /// Runs `start`'s coordinator on a new `coordinator` thread serving `links`, saving
    /// into `store` under its fence. A migrated or bootstrapped state is saved at once.
    pub fn spawn(
        store: StoreHandle,
        start: CoordinatorStart,
        links: LinkSet,
        fenced: Fenced,
    ) -> CoordinatorThread {
        let (stop_tx, stop_rx) = crossbeam::channel::bounded(1);
        #[cfg(feature = "test-hooks")]
        let (inspect_tx, inspect_rx) = crossbeam::channel::unbounded();
        #[cfg(feature = "test-hooks")]
        let crash_before_claim_reply = Arc::new(AtomicBool::new(false));
        let fence = start.fence;
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
            #[cfg(feature = "test-hooks")]
            crash_before_claim_reply: crash_before_claim_reply.clone(),
            started: Instant::now(),
            retry_at_ms: 0,
            failed_in_a_row: 0,
        };
        let join = std::thread::Builder::new()
            .name("coordinator".into())
            .spawn(move || {
                runtime.run(
                    stop_rx,
                    #[cfg(feature = "test-hooks")]
                    inspect_rx,
                )
            })
            .expect("spawn the coordinator thread");
        CoordinatorThread {
            join,
            stop: stop_tx,
            #[cfg(feature = "test-hooks")]
            inspect: inspect_tx,
            #[cfg(feature = "test-hooks")]
            crash_before_claim_reply,
        }
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn run(mut self, stop: Receiver<Stop>, #[cfg(feature = "test-hooks")] inspect: Receiver<Inspect>) -> ServerExit {
        loop {
            self.wait(
                &stop,
                #[cfg(feature = "test-hooks")]
                &inspect,
            );
            // The stop is read before the hosts are drained: whatever a host sent
            // before asking to stop is already queued and is handled below.
            let stopping = match stop.try_recv() {
                Ok(Stop::WithoutSave) => {
                    // Nothing more is handled, saved or sent.
                    log::info!("[coordinator] stopped without a final save");
                    self.down();
                    return ServerExit::Clean;
                }
                Ok(Stop::Save(timeout)) => Some(timeout),
                Err(TryRecvError::Empty) => None,
                // The handle is gone (its owner unwound): stop as an orderly exit would.
                Err(TryRecvError::Disconnected) => Some(crate::EXIT_SAVE_TIMEOUT),
            };
            let now = self.now_ms();
            self.drain_hosts(now);
            #[cfg(feature = "test-hooks")]
            for f in inspect.try_iter() {
                f(&mut self.core);
            }
            self.core.maintain(now);
            if self.pump_save(now).is_err() {
                return self.fence_out();
            }
            let out = self.core.take_out();
            #[cfg(feature = "test-hooks")]
            if self.crash_before_claim_reply.load(Ordering::SeqCst)
                && out.iter().any(|(_, m)| matches!(m, CoordMsg::Reply { reply, .. } if reply.grants_claim()))
            {
                // Its save committed (the reply is gated); it never leaves.
                log::warn!("[coordinator] test crash before a claim-granting reply left");
                self.down();
                return ServerExit::CoordinatorFailed;
            }
            self.send(out);
            if let Some(timeout) = stopping {
                return self.finish(timeout);
            }
        }
    }

    /// Block until a host message, the save's answer or a stop arrives, at most
    /// [`COORDINATOR_TICK_MS`]. Nothing is taken off the channels here.
    fn wait(&self, stop: &Receiver<Stop>, #[cfg(feature = "test-hooks")] inspect: &Receiver<Inspect>) {
        let mut sel = Select::new();
        sel.recv(stop);
        #[cfg(feature = "test-hooks")]
        sel.recv(inspect);
        match &self.links.links {
            Links::Channels(hosts) => {
                for h in hosts.iter().filter(|h| !h.gone) {
                    sel.recv(&h.rx);
                }
            }
            Links::Listener(l) => {
                sel.recv(&l.events);
            }
        }
        if let Some(save) = &self.pending_save {
            sel.recv(save);
        }
        let _ = sel.ready_timeout(Duration::from_millis(COORDINATOR_TICK_MS));
    }

    /// Hand every queued host message to the core, in each host's order.
    fn drain_hosts(&mut self, now: u64) {
        match &mut self.links.links {
            Links::Channels(hosts) => {
                for host in hosts.iter_mut() {
                    while !host.gone {
                        match host.rx.try_recv() {
                            Ok(msg) => {
                                if let HostMsg::Attach { host: ordinal, .. } = &msg {
                                    host.ordinal = *ordinal;
                                }
                                self.core.handle(host.ordinal, msg, now)
                            }
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => {
                                log::info!("[coordinator] a host closed its link");
                                host.gone = true;
                            }
                        }
                    }
                }
            }
            Links::Listener(l) => {
                while let Ok((peer, event)) = l.events.try_recv() {
                    match event {
                        LinkEvent::Up(hello) => match hello.role {
                            PeerRole::Host { ordinal, boot, epoch } => match l.routes.admit(
                                peer,
                                ordinal,
                                epoch,
                                stored_claim_epoch(&self.store, ordinal),
                            ) {
                                Admit::Admitted { replaced } => {
                                    log::info!(
                                        "[coordinator] host {ordinal} connected (peer {}, boot {boot:#x}, claim epoch {epoch})",
                                        peer.0
                                    );
                                    if let Some(old) = replaced {
                                        l.senders.close(old);
                                    }
                                }
                                Admit::Refused => {
                                    l.senders.close(peer);
                                }
                            },
                            other => {
                                log::warn!("[coordinator] peer {} is {other:?}, not a host: closed", peer.0);
                                l.senders.close(peer);
                            }
                        },
                        LinkEvent::Msg(msg) => match l.routes.hosts.get(&peer).copied() {
                            Some(ordinal) => {
                                if let HostMsg::Attach { generation, .. } = &msg {
                                    if !generation_current(&self.store, ordinal, *generation) {
                                        l.routes.refuse(peer);
                                        l.senders.close(peer);
                                        continue;
                                    }
                                }
                                l.routes.note(peer, ordinal, &msg);
                                self.core.handle(ordinal, msg, now);
                            }
                            None if l.routes.refused.contains(&peer) => {
                                log::debug!("[coordinator] a message from refused peer {} ignored", peer.0)
                            }
                            None => log::warn!("[coordinator] a message from non-host peer {} ignored", peer.0),
                        },
                        LinkEvent::Down(why) => {
                            if let Some(ordinal) = l.routes.gone(peer) {
                                log::warn!("[coordinator] host {ordinal} (peer {}) left: {why}", peer.0);
                            }
                        }
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
        self.send(out);
    }

    fn send(&mut self, out: Vec<(Target, CoordMsg)>) {
        if out.is_empty() {
            return;
        }
        match &mut self.links.links {
            // One host per process (`AllInOne`): everything goes to it.
            Links::Channels(hosts) => {
                for host in hosts.iter().filter(|h| !h.gone) {
                    for (_, msg) in &out {
                        let _ = host.tx.send(msg.clone());
                    }
                    let _ = host.wake.try_send(());
                }
            }
            Links::Listener(l) => {
                for (to, msg) in out {
                    l.send(to, msg);
                }
            }
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

    /// Every host's link goes down (the listener stops: its port is freed).
    fn down(&mut self) {
        match &mut self.links.links {
            Links::Channels(hosts) => {
                for host in hosts.iter() {
                    host.up.store(false, Ordering::SeqCst);
                }
            }
            Links::Listener(l) => drop(l.listener.take()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::RequestId;

    fn attach(host: u32, epoch: u64) -> HostMsg {
        HostMsg::Attach { host, epoch, running: Vec::new(), online: Vec::new(), generation: 0 }
    }

    /// Any ordinal is served, each with one live connection; a hello whose claim epoch is
    /// not the store's is refused (a stale process), first to connect or not.
    #[test]
    fn two_ordinals_are_served() {
        let mut r = Routes::default();
        assert_eq!(r.admit(PeerId(2), 4, 1, Some(2)), Admit::Refused, "a stale epoch is refused, first or not");
        assert_eq!(r.admit(PeerId(3), 0, 1, Some(1)), Admit::Admitted { replaced: None }, "host 0 is served");
        assert_eq!(r.admit(PeerId(4), 1, 3, Some(3)), Admit::Admitted { replaced: None }, "and host 1");
        assert_eq!(r.admit(PeerId(5), 0, 1, Some(1)), Admit::Admitted { replaced: Some(PeerId(3)) }, "host 0 reconnecting");
        assert_eq!(r.admit(PeerId(6), 7, 0, None), Admit::Admitted { replaced: None }, "a host claiming nothing: epoch 0");
        assert_eq!(r.peer_of(0), Some(PeerId(5)));
        assert_eq!(r.peer_of(1), Some(PeerId(4)));
        assert_eq!(r.peer_of(4), None);
        assert!(r.refused.contains(&PeerId(2)) && r.refused.contains(&PeerId(3)));
        assert_eq!(r.route(Target::Host(0), &CoordMsg::Attached { epoch: 9 }), Route::Nobody, "an unmatched echo goes nowhere");
    }

    /// The core names the host; the attach echo goes to the connection that attached.
    #[test]
    fn messages_route_to_the_host_they_are_for() {
        let mut r = Routes::default();
        // Host 3 connected twice (the newer peer wins), host 5 once; neither is peer 0.
        r.hosts.insert(PeerId(4), 3);
        r.hosts.insert(PeerId(7), 5);
        r.hosts.insert(PeerId(9), 3);
        r.note(PeerId(9), 3, &attach(3, 1));
        r.note(PeerId(7), 5, &attach(5, 1));
        assert_eq!(r.peer_of(3), Some(PeerId(9)));
        assert_eq!(r.peer_of(5), Some(PeerId(7)));
        assert_eq!(r.peer_of(1), None);
        // Both attached with epoch 1: the echoes go back in the order handled.
        assert_eq!(r.route(Target::Host(3), &CoordMsg::Attached { epoch: 1 }), Route::Peer(PeerId(9)));
        assert_eq!(r.route(Target::Host(5), &CoordMsg::Attached { epoch: 1 }), Route::Peer(PeerId(7)));
        let reply = CoordMsg::Reply { id: RequestId { host: 5, boot: 1, seq: 1 }, reply: game::WorldReply::PersonInfo(None) };
        assert_eq!(r.route(Target::Host(5), &reply), Route::Host(5));
        let spawn = CoordMsg::SpawnPeople { rc: game::RegionCoords::new(1, 1), incarnation: 1, events: Vec::new() };
        assert_eq!(r.route(Target::Host(3), &spawn), Route::Host(3));
        assert_eq!(r.route(Target::Every, &CoordMsg::Calendar(game::history::CalendarTick(1))), Route::Every);
    }

    /// One live connection per ordinal, admitted by the store's claim epoch: a stale
    /// process is refused even with nobody live (a restarted coordinator has no memory of
    /// who connected); the replaced connection is refused, its pending attach echo
    /// dropped, and only the live one is routed to.
    #[test]
    fn one_live_peer_per_ordinal_by_claim_epoch() {
        let mut r = Routes::default();
        assert_eq!(r.admit(PeerId(1), 0, 5, Some(5)), Admit::Admitted { replaced: None });
        r.note(PeerId(1), 0, &attach(0, 3));
        // The ordinal was claimed again (epoch 6): the old process is stale.
        assert_eq!(r.admit(PeerId(2), 0, 5, Some(6)), Admit::Refused);
        assert!(r.refused.contains(&PeerId(2)));
        assert_eq!(r.peer_of(0), Some(PeerId(1)));
        // Its replacement takes over: the old connection is refused and its attach echo
        // goes nowhere.
        assert_eq!(r.admit(PeerId(3), 0, 6, Some(6)), Admit::Admitted { replaced: Some(PeerId(1)) });
        assert!(r.refused.contains(&PeerId(1)) && !r.hosts.contains_key(&PeerId(1)));
        assert_eq!(r.route(Target::Host(0), &CoordMsg::Attached { epoch: 3 }), Route::Nobody);
        r.note(PeerId(3), 0, &attach(0, 1));
        assert_eq!(r.route(Target::Host(0), &CoordMsg::Attached { epoch: 1 }), Route::Peer(PeerId(3)));
        assert_eq!(r.peer_of(0), Some(PeerId(3)));
        // The same epoch reconnecting (a dropped link of the live process): the newer wins.
        assert_eq!(r.admit(PeerId(4), 0, 6, Some(6)), Admit::Admitted { replaced: Some(PeerId(3)) });
        // The old connection's Down does not unseat the live one.
        assert_eq!(r.gone(PeerId(3)), None);
        assert_eq!(r.peer_of(0), Some(PeerId(4)));
        assert_eq!(r.gone(PeerId(4)), Some(0));
        assert_eq!(r.peer_of(0), None);
        // With nobody live, a stale epoch is still refused; the stored epoch is admitted.
        assert_eq!(r.admit(PeerId(5), 0, 5, Some(6)), Admit::Refused);
        assert_eq!(r.admit(PeerId(6), 0, 6, Some(6)), Admit::Admitted { replaced: None });
        // A refused Attach (an old world generation) closes the live connection.
        r.note(PeerId(6), 0, &attach(0, 2));
        assert_eq!(r.refuse(PeerId(6)), Some(0));
        assert_eq!(r.peer_of(0), None);
        assert_eq!(r.route(Target::Host(0), &CoordMsg::Attached { epoch: 2 }), Route::Nobody);
    }
}
