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
use game::{CoordMsg, Coordinator, HostMsg, RegionCoords, WorldLink};
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

/// Where a coordinator message goes.
#[derive(Debug, PartialEq, Eq)]
enum Target {
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
    fn send(&mut self, msg: CoordMsg) {
        match self.routes.route(&msg) {
            Target::Host(ordinal) => match self.routes.peer_of(ordinal) {
                Some(peer) => {
                    if !self.senders.send(peer, msg) {
                        log::debug!("[coordinator] host {ordinal} left; a message for it is lost");
                    }
                }
                None => log::debug!("[coordinator] host {ordinal} is not connected; {} lost", describe(&msg)),
            },
            Target::Peer(peer) => {
                if !self.senders.send(peer, msg) {
                    log::debug!("[coordinator] peer {} left; its attach echo is lost", peer.0);
                }
            }
            Target::Every => {
                for peer in self.routes.hosts.keys() {
                    self.senders.send(*peer, msg.clone());
                }
            }
            // `route` said why.
            Target::Nobody => {}
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
        CoordMsg::FortressRefused { player, site } => format!("FortressRefused {player:?} {site:?}"),
    }
}

/// A host message's kind and region, for logs (an `Attach` or `Positions` can be large).
pub(crate) fn describe_host(msg: &HostMsg) -> String {
    match msg {
        HostMsg::Attach { host, epoch, running, online } => {
            format!("Attach {{ host: {host}, epoch: {epoch} }} ({} regions, {} online)", running.len(), online.len())
        }
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

/// The one host ordinal a coordinator serves until R3: the core is single-host — another
/// host's `Attach` would park every region it does not list, host 0's running ones
/// included — so any other ordinal is refused. Fixed, not "the first to connect": a stray
/// host winning the race to a restarted coordinator must not take the world over.
pub const SERVED_HOST_ORDINAL: u32 = 0;

/// What routes a coordinator message to one of the connected hosts: a reply to the host
/// its request id names, a region's messages to the host that announced the region (its
/// `Attach` / `RegionStarted`), the attach echo to the connection whose `Attach` it
/// answers, the world's view to every host. A message for a host that is not connected is
/// lost (it re-attaches when it is back: its `Attach` and resent requests cover it).
///
/// One live connection per host ordinal ([`Routes::admit`]): a host process that is
/// replaced (a restart, a second process of the ordinal) keeps running until its next cut
/// is fenced, and its messages — a park, a crossing, positions — would otherwise be
/// applied beside its replacement's (two bodies of one person).
#[derive(Default)]
struct Routes {
    /// Peers refused or replaced ([`Routes::admit`]); their messages are ignored.
    refused: BTreeSet<PeerId>,
    /// Connected host peers and their ordinals (a reconnect is a new peer).
    hosts: BTreeMap<PeerId, u32>,
    /// The live connection of each ordinal, with the claim epoch its hello carried.
    live: BTreeMap<u32, (PeerId, u64)>,
    /// The highest claim epoch each ordinal's hello has carried (kept after its connection
    /// ends: a stale process must not take the ordinal while its replacement reconnects).
    best_epoch: BTreeMap<u32, u64>,
    /// Which host runs each region, as announced.
    region_host: BTreeMap<RegionCoords, u32>,
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
    /// A host peer said hello as `ordinal` with claim `epoch`. Refused unless it is
    /// [`SERVED_HOST_ORDINAL`] (logged with both ordinals). The ordinal's highest claim
    /// epoch wins, ties to the newer connection: a process that claimed its super-regions
    /// later replaces the live one; one with a lower epoch is a stale process (its claims
    /// taken over, fenced at its next cut) and is refused, so it cannot take the ordinal
    /// back.
    fn admit(&mut self, peer: PeerId, ordinal: u32, epoch: u64) -> Admit {
        if ordinal != SERVED_HOST_ORDINAL {
            log::error!(
                "[coordinator] refusing host {ordinal} (peer {}): this coordinator serves host {SERVED_HOST_ORDINAL} \
                 only (one host per world until R3; another host's Attach would park host \
                 {SERVED_HOST_ORDINAL}'s regions)",
                peer.0
            );
            self.refused.insert(peer);
            return Admit::Refused;
        }
        let best = self.best_epoch.get(&ordinal).copied().unwrap_or(0);
        if epoch < best {
            log::error!(
                "[coordinator] refusing host {ordinal} (peer {}) with claim epoch {epoch}: epoch {best} has \
                 connected (a stale host process, fenced at its next cut)",
                peer.0
            );
            self.refused.insert(peer);
            return Admit::Refused;
        }
        self.best_epoch.insert(ordinal, epoch);
        let replaced = match self.live.get(&ordinal).copied() {
            Some((live, live_epoch)) => {
                log::warn!(
                    "[coordinator] host {ordinal}: peer {} (claim epoch {epoch}) replaces peer {} (epoch {live_epoch}); \
                     the old connection is closed and its messages ignored",
                    peer.0,
                    live.0
                );
                self.hosts.remove(&live);
                self.refused.insert(live);
                self.attaches.retain(|(p, _)| *p != live);
                Some(live)
            }
            None => None,
        };
        self.live.insert(ordinal, (peer, epoch));
        self.hosts.insert(peer, ordinal);
        Admit::Admitted { replaced }
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
        match msg {
            HostMsg::Attach { host, epoch, running, .. } => {
                if *host != ordinal {
                    log::warn!("[coordinator] host {ordinal}'s Attach names host {host}; routed by its link");
                }
                self.region_host.retain(|_, h| *h != ordinal);
                for r in running {
                    self.region_host.insert(r.rc, ordinal);
                }
                self.attaches.push_back((peer, *epoch));
            }
            HostMsg::RegionStarted { rc, .. } => {
                self.region_host.insert(*rc, ordinal);
            }
            HostMsg::RegionParked { rc, .. } => {
                if self.region_host.get(rc) == Some(&ordinal) {
                    self.region_host.remove(rc);
                }
            }
            _ => {}
        }
    }

    fn route(&mut self, msg: &CoordMsg) -> Target {
        match msg {
            CoordMsg::Reply { id, .. } => Target::Host(id.host),
            CoordMsg::Attached { epoch } => match self.attaches.iter().position(|(_, e)| e == epoch) {
                Some(i) => Target::Peer(self.attaches.remove(i).expect("found").0),
                None => {
                    log::warn!("[coordinator] attach echo {epoch} matches no Attach; dropped");
                    Target::Nobody
                }
            },
            CoordMsg::SpawnPeople { rc, .. } => self.region_target(*rc),
            CoordMsg::Directive { directive, .. } => self.region_target(directive.region),
            CoordMsg::Notice { region, .. } => self.region_target(*region),
            CoordMsg::View { .. } | CoordMsg::Calendar(_) | CoordMsg::Sites(_) | CoordMsg::SiteMarkers(_) => {
                Target::Every
            }
            // About a player online on the host that reported it: R2's one host ordinal
            // (R3 routes it by the player's host).
            CoordMsg::FortressRefused { .. } => Target::Host(SERVED_HOST_ORDINAL),
        }
    }

    /// The host that announced `rc`; with none, the only connected host (R2's one host
    /// per world), else nobody (several hosts and an unannounced region: R3 routes it).
    fn region_target(&self, rc: RegionCoords) -> Target {
        if let Some(h) = self.region_host.get(&rc) {
            return Target::Host(*h);
        }
        let ordinals: BTreeSet<u32> = self.hosts.values().copied().collect();
        match ordinals.len() {
            1 => Target::Every,
            _ => {
                log::warn!("[coordinator] no host is known to run {rc:?}; its message is dropped (R3 routes it)");
                Target::Nobody
            }
        }
    }

    /// The newest connection of host `ordinal`.
    fn peer_of(&self, ordinal: u32) -> Option<PeerId> {
        self.hosts.iter().rev().find(|(_, o)| **o == ordinal).map(|(p, _)| *p)
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
                && out.iter().any(|m| matches!(m, CoordMsg::Reply { reply, .. } if reply.grants_claim()))
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
            Links::Listener(l) => {
                while let Ok((peer, event)) = l.events.try_recv() {
                    match event {
                        LinkEvent::Up(hello) => match hello.role {
                            PeerRole::Host { ordinal, boot, epoch } => match l.routes.admit(peer, ordinal, epoch) {
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
                                l.routes.note(peer, ordinal, &msg);
                                self.core.handle(msg, now);
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

    fn send(&mut self, out: Vec<CoordMsg>) {
        if out.is_empty() {
            return;
        }
        match &mut self.links.links {
            Links::Channels(hosts) => {
                for host in hosts.iter().filter(|h| !h.gone) {
                    for msg in &out {
                        let _ = host.tx.send(msg.clone());
                    }
                    let _ = host.wake.try_send(());
                }
            }
            Links::Listener(l) => {
                for msg in out {
                    l.send(msg);
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
    use game::history::{Directive, PersonId};
    use game::{RequestId, RunningRegion};

    fn rc(x: i32, z: i32) -> RegionCoords {
        RegionCoords::new(x, z)
    }

    fn attach(host: u32, epoch: u64, regions: &[RegionCoords]) -> HostMsg {
        let running = regions.iter().map(|r| RunningRegion { rc: *r, incarnation: 1, people: Vec::new() }).collect();
        HostMsg::Attach { host, epoch, running, online: Vec::new() }
    }

    #[test]
    fn only_host_ordinal_zero_is_served() {
        let mut r = Routes::default();
        // A stray ordinal first to connect (a restarted coordinator's race) is refused...
        assert_eq!(r.admit(PeerId(2), 4, 1), Admit::Refused, "ordinal 4 is refused, first or not");
        assert_eq!(r.admit(PeerId(3), 0, 1), Admit::Admitted { replaced: None }, "host 0 is served");
        assert_eq!(r.admit(PeerId(4), 1, 1), Admit::Refused, "another ordinal is refused");
        assert_eq!(r.admit(PeerId(5), 0, 1), Admit::Admitted { replaced: Some(PeerId(3)) }, "host 0 reconnecting");
        assert_eq!(r.peer_of(0), Some(PeerId(5)));
        assert_eq!(r.peer_of(4), None);
        assert_eq!(r.peer_of(1), None);
        assert!(r.refused.contains(&PeerId(2)) && r.refused.contains(&PeerId(4)));
        assert_eq!(r.route(&CoordMsg::Attached { epoch: 9 }), Target::Nobody, "an unmatched echo goes nowhere");
    }

    #[test]
    fn messages_route_to_the_host_they_are_for() {
        let mut r = Routes::default();
        // Host 3 connected twice (the newer peer wins), host 5 once; neither is peer 0.
        r.hosts.insert(PeerId(4), 3);
        r.hosts.insert(PeerId(7), 5);
        r.hosts.insert(PeerId(9), 3);
        r.note(PeerId(9), 3, &attach(3, 1, &[rc(1, 1)]));
        r.note(PeerId(7), 5, &attach(5, 1, &[rc(2, 2)]));
        r.note(PeerId(7), 5, &HostMsg::RegionStarted { rc: rc(4, 4), incarnation: 2 });
        assert_eq!(r.peer_of(3), Some(PeerId(9)));
        assert_eq!(r.peer_of(5), Some(PeerId(7)));
        assert_eq!(r.peer_of(1), None);
        // Both attached with epoch 1: the echoes go back in the order handled.
        assert_eq!(r.route(&CoordMsg::Attached { epoch: 1 }), Target::Peer(PeerId(9)));
        assert_eq!(r.route(&CoordMsg::Attached { epoch: 1 }), Target::Peer(PeerId(7)));
        let reply = |host| CoordMsg::Reply {
            id: RequestId { host, boot: 1, seq: 1 },
            reply: game::WorldReply::PersonInfo(None),
        };
        assert_eq!(r.route(&reply(5)), Target::Host(5));
        assert_eq!(r.route(&reply(3)), Target::Host(3));
        let spawn = |at| CoordMsg::SpawnPeople { rc: at, incarnation: 1, events: Vec::new() };
        assert_eq!(r.route(&spawn(rc(1, 1))), Target::Host(3));
        assert_eq!(r.route(&spawn(rc(4, 4))), Target::Host(5));
        let directive = CoordMsg::Directive {
            directive: Directive { region: rc(2, 2), event: game::GameEventKind::DespawnPerson(PersonId(1)) },
            incarnation: 1,
        };
        assert_eq!(r.route(&directive), Target::Host(5));
        assert_eq!(r.route(&CoordMsg::Notice { region: rc(1, 1), line: "x".into() }), Target::Host(3));
        assert_eq!(r.route(&CoordMsg::Calendar(game::history::CalendarTick(1))), Target::Every);
        // An unannounced region with two hosts: nobody (R3); a park forgets the region.
        assert_eq!(r.route(&spawn(rc(9, 9))), Target::Nobody);
        r.note(PeerId(7), 5, &HostMsg::RegionParked { rc: rc(4, 4), people: Vec::new() });
        assert_eq!(r.route(&spawn(rc(4, 4))), Target::Nobody);
        // One host left: everything unannounced goes to it.
        r.hosts.retain(|_, o| *o == 3);
        assert_eq!(r.route(&spawn(rc(9, 9))), Target::Every);
        // A re-attach replaces the host's regions.
        r.note(PeerId(9), 3, &attach(3, 2, &[rc(6, 6)]));
        r.hosts.insert(PeerId(10), 5);
        assert_eq!(r.route(&spawn(rc(1, 1))), Target::Nobody);
        assert_eq!(r.route(&spawn(rc(6, 6))), Target::Host(3));
    }

    /// One live connection per ordinal: the highest claim epoch wins (ties: the newer
    /// connection); the replaced or stale connection is refused, its pending attach echo
    /// dropped, and only the live one is routed to.
    #[test]
    fn one_live_peer_per_ordinal_by_claim_epoch() {
        let mut r = Routes::default();
        assert_eq!(r.admit(PeerId(1), 0, 5), Admit::Admitted { replaced: None });
        r.note(PeerId(1), 0, &attach(0, 3, &[rc(1, 1)]));
        // A stale process (lower epoch) cannot take the ordinal back.
        assert_eq!(r.admit(PeerId(2), 0, 4), Admit::Refused);
        assert!(r.refused.contains(&PeerId(2)));
        assert_eq!(r.peer_of(0), Some(PeerId(1)));
        // Its replacement (higher epoch) takes over: the old connection is refused and its
        // attach echo goes nowhere.
        assert_eq!(r.admit(PeerId(3), 0, 6), Admit::Admitted { replaced: Some(PeerId(1)) });
        assert!(r.refused.contains(&PeerId(1)) && !r.hosts.contains_key(&PeerId(1)));
        assert_eq!(r.route(&CoordMsg::Attached { epoch: 3 }), Target::Nobody);
        r.note(PeerId(3), 0, &attach(0, 1, &[rc(2, 2)]));
        assert_eq!(r.route(&CoordMsg::Attached { epoch: 1 }), Target::Peer(PeerId(3)));
        assert_eq!(r.peer_of(0), Some(PeerId(3)));
        // The same epoch reconnecting (a dropped link of the live process): the newer wins.
        assert_eq!(r.admit(PeerId(4), 0, 6), Admit::Admitted { replaced: Some(PeerId(3)) });
        // The old connection's Down does not unseat the live one.
        assert_eq!(r.gone(PeerId(3)), None);
        assert_eq!(r.peer_of(0), Some(PeerId(4)));
        assert_eq!(r.gone(PeerId(4)), Some(0));
        assert_eq!(r.peer_of(0), None);
        // With nobody live, a stale epoch is still refused; the live epoch is admitted.
        assert_eq!(r.admit(PeerId(5), 0, 5), Admit::Refused);
        assert_eq!(r.admit(PeerId(6), 0, 6), Admit::Admitted { replaced: None });
    }
}
