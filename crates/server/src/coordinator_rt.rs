//! The world coordinator's runtime: the threadless core ([`game::Coordinator`]) on its own
//! `coordinator` thread, persisting its state into the world store under its own fence
//! ([`CoordinatorFence`]) — the **only writer** of history state. *Durable before
//! visible* (Elastic 2a): every pass takes the core's transition
//! ([`Coordinator::take_transition`]: its whole projection when anything durable changed) and
//! writes it with a compare-and-set on the row's version
//! ([`StoreHandle::write_coordinator_later`]); the messages it released are sent only once
//! that write committed. One write in flight at a time: whatever is decided meanwhile waits
//! for the next transition. A failed write hands its transition back to the core (its
//! messages wait; the inputs stay applied once, never again) and is retried after
//! [`COORDINATOR_SAVE_RETRY_MS`]. A write refused as a `Conflict` (the row's version moved, its
//! epoch did not) re-reads the row: if it holds this process's own last unacknowledged write (its
//! commit's answer was lost), the version is taken from it and the cached core — that write plus
//! every input applied since, each once — is written again at once; anything else, or a second
//! conflict before a write commits, ends the process as fenced (another writer under the same
//! epoch is a bug: the next start loads the row). Counted (`mmo_coordinator_*` in
//! `/metrics`, the periodic `[coordinator] window=…` line): transitions written, their bytes,
//! conflicts.
//! Login orders are the exception (decision 1: a process-local relay of the login service's
//! own state): each is relayed at once to the gate host chosen from the core's attachment state,
//! which may not be written yet — a host takes orders in any attach state, and a lost order is
//! the login service's retry.
//!
//! In `AllInOne` the host's `WorldManager` reaches it over a crossbeam
//! [`ChannelWorldLink`] ([`start_all_in_one`]); as its own process
//! (`VOXEL_ROLE=coordinator`) it serves the host processes on a service-link listener
//! ([`start_listening`], the hosts' end is [`crate::roles::QuicWorldLink`]); the inline
//! link ([`game::InlineWorldLink`]) stays for tests and wasm single-player.
//!
//! **World cuts** (`claim` hosts, any number of them; R3 Task 13; [`WorldCuts`]): the core picks the tick and
//! collects the hosts' parts; the runtime feeds it the world tick and the hosts the cut
//! needs (those whose `hosts` row is not clean), records each decision in the store
//! ([`StoreHandle::decide_cut`], under the coordinator's fence) and broadcasts the
//! decision the STORE returned. A `decide_cut` that fails (a stale generation, a store
//! error) is followed by `abort_cut`, retried ([`WORLD_CUT_DECISION_RETRY_MS`], slower
//! past the cut's deadline) until a decision is stored: no outcome is ever announced that
//! the store does not hold. A host's question about a cut the core does not remember is
//! answered from the store (`cut_decision`; an undecided one aborted). Every store call of
//! the world cuts is answered on a receiver: the loop never waits for the database. While
//! the coordinator is down no cut runs.
//!
//! Each decision stored as committed is announced to the hosts' terrain listeners
//! ([`StoreHandle::notify_cut`], after the decision's transaction; Task 19).
//!
//! A save refused as `Fenced` (another coordinator claimed the row) fences the whole
//! process: the shared [`Fenced`] flag is set — the server's main loop sees it and exits
//! [`crate::claims::EXIT_FENCED`] through `cuts::exit_world` — the link goes down and the
//! thread ends with [`ServerExit::Fenced`]. Library code never calls `process::exit`.

use game::HostId;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, RecvTimeoutError, Select, Sender, TryRecvError};
use game::coordinator::Target;
use game::{CoordMsg, Coordinator, HostMsg, WorldLink};
use login_api::{HostToLogin, LoginToHost};
use svc_link::{LinkEvent, LinkListener, PeerId, PeerRole, PeerSenders};
use world_store::{CoordinatorFence, CoordinatorWritten, StoreError, StoreHandle};

use crate::claims::{Fenced, ServerExit};
use crate::world_file::CoordinatorStart;

/// The coordinator thread's longest wait for a host message before it runs its
/// upkeep (the calendar, the history's directives, a due save) anyway.
pub const COORDINATOR_TICK_MS: u64 = 50;

/// How long the coordinator waits after a failed write before it tries again (the
/// transition is handed back to the core: nothing it decided leaves until a write commits).
pub const COORDINATOR_SAVE_RETRY_MS: u64 = 250;

/// Failed saves in a row after which the coordinator reports saving stuck (once, at
/// error level, plus a `coordinator_save_stuck` perf event).
pub const COORDINATOR_FAILURES_BEFORE_STUCK: u32 = 3;

/// At most this many failed writes since the last commit are remembered (by their core's
/// hash) to recognise one whose commit's answer was lost when the next write meets a
/// `Conflict`. Older ones are forgotten: such a conflict then ends the process as fenced
/// (a restart, never a guess).
pub const COORDINATOR_UNACKED_MAX: usize = 64;

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
    ordinal: HostId,
}

/// A connected pair: the host's [`ChannelWorldLink`] (up) and the coordinator's [`HostEnd`].
pub fn channel_link() -> (ChannelWorldLink, HostEnd) {
    let (host_tx, host_rx) = crossbeam::channel::unbounded();
    let (coord_tx, coord_rx) = crossbeam::channel::unbounded();
    let (wake_tx, wake_rx) = crossbeam::channel::bounded(1);
    let up = Arc::new(AtomicBool::new(true));
    (
        ChannelWorldLink { tx: host_tx, rx: coord_rx, up: up.clone(), wake: wake_rx },
        HostEnd { rx: host_rx, tx: coord_tx, up, wake: wake_tx, gone: false, ordinal: HostId::SOLO },
    )
}

/// The hosts a coordinator serves: in-process channel links (`AllInOne`, tests), or the
/// host processes connected to its service-link listener (`VOXEL_ROLE=coordinator`).
pub struct LinkSet {
    links: Links,
    /// The login service's listener (role `coordinator`, Elastic 1 Task 6; `None`: no login).
    login: Option<LoginSide>,
}

/// At most this many `SessionEnded` wait at the coordinator while no login service is KNOWN to
/// be linked (an ack is not kept: the login service resends its order or kick; one sent onto a
/// connection that drops right after is lost with it); beyond it the oldest is dropped and
/// counted ([`LoginCounts::ends_dropped`]; the login service then lists that session as open,
/// as a lost `SessionEnded` always did).
pub const LOGIN_ENDS_HELD_MAX: usize = 4096;

/// Where the coordinator listens for the login service ([`start_listening_as`]): the address
/// (`VOXEL_LOGIN_LINK_BIND`), the service token, the pin file
/// (`VOXEL_LOGIN_LINK_CERT_HASH_FILE`).
pub struct LoginListen {
    pub bind: std::net::SocketAddr,
    pub token: String,
    pub cert_hash_file: Option<std::path::PathBuf>,
}

/// What the coordinator's login relay counted since it started
/// ([`ListeningCoordinator::login_counts`]); the drops are logged as counts, at most once per
/// [`WORLD_CUT_RETRY_LOG_MS`], never one line per message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoginCounts {
    /// Orders of a connected login service received.
    pub orders: u64,
    /// Of those, dropped: no gate host attached.
    pub orders_dropped: u64,
    /// `SessionEnded` dropped past [`LOGIN_ENDS_HELD_MAX`].
    pub ends_dropped: u64,
}

/// [`LoginCounts`], shared with the [`ListeningCoordinator`] handle.
#[derive(Default)]
struct LoginTally {
    orders: std::sync::atomic::AtomicU64,
    orders_dropped: std::sync::atomic::AtomicU64,
    ends_dropped: std::sync::atomic::AtomicU64,
}

impl LoginTally {
    fn counts(&self) -> LoginCounts {
        LoginCounts {
            orders: self.orders.load(Ordering::SeqCst),
            orders_dropped: self.orders_dropped.load(Ordering::SeqCst),
            ends_dropped: self.ends_dropped.load(Ordering::SeqCst),
        }
    }
}

/// The coordinator's login listener: every order of a connected login service goes to the
/// gate host of its moment ([`Coordinator::login_order`]); every host's answer
/// ([`HostMsg::Login`]) goes to every connected login service, unchanged. No order is kept.
struct LoginSide {
    listener: Option<LinkListener>,
    events: Receiver<(PeerId, LinkEvent<LoginToHost>)>,
    senders: PeerSenders<HostToLogin>,
    /// The connected login services (peers whose hello said `PeerRole::Login`).
    peers: BTreeSet<PeerId>,
    /// `SessionEnded` held while no login service is connected ([`LOGIN_ENDS_HELD_MAX`]).
    held: VecDeque<HostToLogin>,
    tally: Arc<LoginTally>,
    /// The drops as last logged, and when ([`Self::report`]).
    logged: LoginCounts,
    logged_ms: Option<u64>,
}

impl LoginSide {
    fn new(listener: LinkListener, events: Receiver<(PeerId, LinkEvent<LoginToHost>)>, senders: PeerSenders<HostToLogin>) -> Self {
        LoginSide {
            listener: Some(listener),
            events,
            senders,
            peers: BTreeSet::new(),
            held: VecDeque::new(),
            tally: Arc::default(),
            logged: LoginCounts::default(),
            logged_ms: None,
        }
    }

    /// `msg` to every connected login service; none: a `SessionEnded` is held, an ack dropped.
    fn tell(&mut self, msg: HostToLogin) {
        if self.peers.is_empty() {
            match msg {
                HostToLogin::SessionEnded { .. } => {
                    if self.held.len() >= LOGIN_ENDS_HELD_MAX {
                        self.held.pop_front();
                        self.tally.ends_dropped.fetch_add(1, Ordering::SeqCst);
                    }
                    self.held.push_back(msg);
                }
                // `HostToLogin`'s `Debug` hides the player token.
                other => log::debug!("[coordinator] no login service is linked: {other:?} dropped"),
            }
            return;
        }
        for peer in &self.peers {
            self.senders.send(*peer, msg.clone());
        }
    }

    /// The drops since the last report, as one warning, at most once per
    /// [`WORLD_CUT_RETRY_LOG_MS`] (`now_ms`).
    fn report(&mut self, now_ms: u64) {
        let c = self.tally.counts();
        let new = (c.orders_dropped - self.logged.orders_dropped, c.ends_dropped - self.logged.ends_dropped);
        if new == (0, 0) || self.logged_ms.is_some_and(|at| now_ms < at + WORLD_CUT_RETRY_LOG_MS) {
            return;
        }
        log::warn!(
            "[coordinator] login relay: {} orders dropped with no host attached (the login service answers 503), {} session \
             ends dropped past {LOGIN_ENDS_HELD_MAX} held, since the last report (total {} / {})",
            new.0,
            new.1,
            c.orders_dropped,
            c.ends_dropped
        );
        self.logged = c;
        self.logged_ms = Some(now_ms);
    }
}

enum Links {
    /// Every outgoing message goes to each host (R2: one host per process).
    Channels(Vec<HostEnd>),
    Listener(ListenerHosts),
}

impl LinkSet {
    pub fn one(host: HostEnd) -> Self {
        LinkSet { links: Links::Channels(vec![host]), login: None }
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
            login: None,
        }
    }
}

/// Which connection(s) a coordinator message goes to.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// The live connection of a host ordinal.
    Host(HostId),
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
        CoordMsg::ClaimIs { player, claim, report } => format!("ClaimIs {player:?} {claim:?} (report {report})"),
        CoordMsg::HopIs { player, reported, hop } => format!("HopIs {player:?} {reported} -> {hop}"),
        CoordMsg::BeginCut { cut, tick, hosts, leaving, .. } => {
            format!("BeginCut {} at {tick} of {hosts:?} leaving {leaving:?}", cut.0)
        }
        CoordMsg::CutDone { cut, committed } => format!("CutDone {} (committed {committed})", cut.0),
        CoordMsg::Succession { id, predecessor, heir, base_cut, moved, survivors, .. } => format!(
            "Succession {} (host {predecessor} → heir {heir}, base cut {}, {} super-regions, survivors {survivors:?})",
            id.0,
            base_cut.0,
            moved.len()
        ),
        CoordMsg::SuccessionDone { id, settled } => format!("SuccessionDone {} (settled {settled})", id.0),
        CoordMsg::Generation(g) => format!("Generation({g})"),
        CoordMsg::CutPeerCount { cut, from, received_through, boot } => {
            format!("CutPeerCount {} from host {from}: {received_through} of boot {boot:#x}", cut.0)
        }
        // Its `Debug` hides the player token.
        CoordMsg::Login(order) => format!("Login({order:?})"),
    }
}

/// A host message's kind and region, for logs (an `Attach` or `Positions` can be large).
pub(crate) fn describe_host(msg: &HostMsg) -> String {
    match msg {
        HostMsg::Attach { host, epoch, running, online, generation, boot, settled_below, handed, pending_cut, successions, policy } => {
            format!(
                "Attach {{ host: {host}, epoch: {epoch}, generation: {generation}, boot: {boot}, settled_below: {settled_below}, \
                 pending_cut: {pending_cut:?}, successions: {successions:?}, policy: {policy:?} }} ({} regions, {} online, {} handed)",
                running.len(),
                online.len(),
                handed.len()
            )
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
        HostMsg::Settled { boot, below } => format!("Settled {{ boot: {boot}, below: {below} }}"),
        HostMsg::PlayerOnline(p) => format!("PlayerOnline {:?} hop {}", p.player, p.hop),
        HostMsg::PlayerOffline { player, hop } => format!("PlayerOffline {player:?} hop {hop}"),
        HostMsg::ReleaseClaim { player } => format!("ReleaseClaim {player:?}"),
        HostMsg::PlayerHandedTo { player, to, hop } => format!("PlayerHandedTo {player:?} to host {to} hop {hop}"),
        HostMsg::CutPrepared { cut } => format!("CutPrepared {}", cut.0),
        HostMsg::CutFailed { cut, why } => format!("CutFailed {} ({why:?})", cut.0),
        HostMsg::LeaveRequest => "LeaveRequest".to_string(),
        HostMsg::SuccessionApplied { id } => format!("SuccessionApplied {}", id.0),
        HostMsg::SuccessionAdopted { id } => format!("SuccessionAdopted {}", id.0),
        HostMsg::LandWanted { sr } => format!("LandWanted {sr:?}"),
        HostMsg::CutPeerCount { cut, peer, received_through, boot } => {
            format!("CutPeerCount {} toward host {peer}: {received_through} of boot {boot:#x}", cut.0)
        }
        // Its `Debug` hides the player token.
        HostMsg::Login(out) => format!("Login({out:?})"),
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
    hosts: BTreeMap<PeerId, HostId>,
    /// The live connection of each ordinal, with the registration number its hello carried.
    live: BTreeMap<HostId, (PeerId, u64)>,
    /// `Attach`es handled whose echo has not been routed yet: (connection, epoch), in order.
    attaches: VecDeque<(PeerId, u64)>,
    /// The process boot each host peer's hello named: its `Attach` names the same one
    /// (a mismatch is only logged).
    boots: BTreeMap<PeerId, u64>,
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
    /// A host peer said hello as `ordinal` with registration number `reg`; `stored` is the
    /// ordinal's `hosts` row as read now (its `reg` and `state`; `None`: no row, or it could
    /// not be read). A hello whose `reg` is not the row's, or whose row is not live (its
    /// lease lapsed and it was declared dead), is a stale host process — another boot of the
    /// ordinal registered since, or it is being succeeded — and is refused; read from the
    /// store, so this holds across a coordinator restart. Otherwise the newer connection of
    /// an ordinal replaces the older. (Registration numbers replace R3's claim epochs: under
    /// `claim` a host's claims change during its life, its registration never does.)
    fn admit(&mut self, peer: PeerId, ordinal: HostId, reg: u64, stored: Option<(u64, i64)>) -> Admit {
        let current = matches!(stored, Some((stored_reg, state)) if stored_reg == reg && state == game::HOST_LIVE);
        if !current {
            log::error!(
                "[coordinator] refusing host {ordinal} (peer {}) with registration {reg}: its hosts row is {stored:?} \
                 ((reg, state); a stale host process, or its row unreadable)",
                peer.0
            );
            self.refused.insert(peer);
            return Admit::Refused;
        }
        let replaced = match self.live.get(&ordinal).copied() {
            Some((live, live_reg)) => {
                log::warn!(
                    "[coordinator] host {ordinal}: peer {} (registration {reg}) replaces peer {} (registration {live_reg}); \
                     the old connection is closed and its messages ignored",
                    peer.0,
                    live.0
                );
                self.refuse(live);
                Some(live)
            }
            None => None,
        };
        self.live.insert(ordinal, (peer, reg));
        self.hosts.insert(peer, ordinal);
        Admit::Admitted { replaced }
    }

    /// `peer` is refused from now on (replaced, or its `Attach` was refused): its messages
    /// are ignored and its pending attach echo dropped. The ordinal it served, if any.
    fn refuse(&mut self, peer: PeerId) -> Option<HostId> {
        self.refused.insert(peer);
        self.attaches.retain(|(p, _)| *p != peer);
        let ordinal = self.hosts.remove(&peer)?;
        if self.live.get(&ordinal).is_some_and(|(p, _)| *p == peer) {
            self.live.remove(&ordinal);
        }
        Some(ordinal)
    }

    /// `peer`'s connection ended.
    fn gone(&mut self, peer: PeerId) -> Option<HostId> {
        self.refused.remove(&peer);
        self.boots.remove(&peer);
        let ordinal = self.hosts.remove(&peer)?;
        if self.live.get(&ordinal).is_some_and(|(p, _)| *p == peer) {
            self.live.remove(&ordinal);
        }
        Some(ordinal)
    }

    /// Note what routes later messages from `msg`, sent by host `ordinal` on `peer`.
    fn note(&mut self, peer: PeerId, ordinal: HostId, msg: &HostMsg) {
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
    fn peer_of(&self, ordinal: HostId) -> Option<PeerId> {
        self.hosts.iter().rev().find(|(_, o)| **o == ordinal).map(|(p, _)| *p)
    }
}

/// `value`, after a beat of the split coordinator's loop ([`crate::health::Health::loop_beat`]).
/// Wraps the two synchronous store reads of its pass ([`stored_registration`],
/// [`generation_current`]), both called only for a listener (role `coordinator`): each may wait
/// up to the pool's acquire timeout (30 s) while the database is away, and several hellos or
/// `Attach`es drained in one pass would otherwise add up past `/livez`'s stall limit — a
/// coordinator restarted for a database outage. A read that returned is progress, not a stall.
fn after_store_read<T>(value: T) -> T {
    crate::health::Health::global().loop_beat();
    value
}

/// Host `ordinal`'s `hosts` row as stored — `(reg, state)` — for its hello's admission;
/// `None` (refused: "wait, never guess" — the host redials) when it has no row or the store
/// cannot be read. Since Elastic 1 the id is the registration number (issued once, never
/// again), so `reg` is the row's id.
fn stored_registration(store: &StoreHandle, ordinal: HostId) -> Option<(u64, i64)> {
    match store.hosts() {
        Ok(rows) => rows.into_iter().find(|r| r.id == ordinal).map(|r| (r.id.0, r.state)),
        Err(e) => {
            log::error!("[coordinator] host {ordinal}'s hosts row could not be read ({e}); its hello is refused");
            None
        }
    }
}

/// Whether host `ordinal`'s `Attach` of world `generation` may be applied: not lower than
/// the store's. A lower one is a stale process: `Err(Some(stored))` — it is told the store's
/// generation ([`CoordMsg::Generation`]: it exits for the rollback; nothing else may ever
/// tell it, e.g. while no peer of the newer generation dials it) and ignored from then on.
/// A store that cannot be read refuses it too (`Err(None)`: its link is closed, the host
/// redials and attaches again).
fn generation_current(store: &StoreHandle, ordinal: HostId, generation: u64) -> Result<(), Option<u64>> {
    match store.world_generation() {
        Ok(stored) if generation < stored => {
            log::error!(
                "[coordinator] host {ordinal}'s Attach runs world generation {generation}, older than the store's \
                 {stored}: refused (a stale host process; told to restart)"
            );
            Err(Some(stored))
        }
        Ok(_) => Ok(()),
        Err(e) => {
            log::error!("[coordinator] the world generation could not be read ({e}); host {ordinal}'s Attach is refused");
            Err(None)
        }
    }
}

/// Whether a host whose crashes cost `host` (its `Attach`'s policy, from its
/// `VOXEL_OWNERSHIP`) may attach to a coordinator started with policy `coordinator`: the two
/// must agree, whatever the number of hosts (Elastic 1: no host count). Beside a `Rollback`
/// coordinator a `claim` host would save in world cuts nobody runs and its crash would never
/// be declared (its dirty row blocking the world for ever), and beside a `Succession`
/// coordinator a `static` host's crash would be given to an heir its fixed layout does not
/// know.
fn policy_agrees(coordinator: game::CrashPolicy, host: game::CrashPolicy) -> Result<(), String> {
    let (wants, declares) = (host == game::CrashPolicy::Succession, coordinator == game::CrashPolicy::Succession);
    if wants == declares {
        return Ok(());
    }
    let word = |succession: bool| if succession { "claim" } else { "static (or unset)" };
    Err(format!(
        "the host runs {env}={}, this coordinator {env}={}: set the same {env} on the coordinator and on every host",
        word(wants),
        word(declares),
        env = game::ownership::OWNERSHIP_ENV
    ))
}

/// How the coordinator thread is asked to stop.
enum Stop {
    /// Handle what the hosts sent before the stop, write the last transition, and wait for
    /// the write at most this long.
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
    /// [`CoordinatorThread::hold_decision_retries`] is armed.
    #[cfg(feature = "test-hooks")]
    hold_decision_retries: Arc<AtomicBool>,
    /// [`CoordinatorThread::hold_hosts_reads`] is armed.
    #[cfg(feature = "test-hooks")]
    hold_hosts_reads: Arc<AtomicBool>,
    #[cfg(feature = "test-hooks")]
    hooks: WriteHooks,
}

/// The test hooks of the coordinator's writes ([`CoordinatorThread::fail_coordinator_writes`],
/// [`CoordinatorThread::kill_after_commit_before_send`]).
#[cfg(feature = "test-hooks")]
#[derive(Clone, Default)]
struct WriteHooks {
    fail: Arc<AtomicBool>,
    kill_after_commit: Arc<AtomicBool>,
    lose_answer: Arc<AtomicBool>,
}

impl CoordinatorThread {
    /// Test hook: while `fail`, every coordinator write is answered with a store error without
    /// reaching the store (a database that refuses them): nothing the coordinator decides leaves.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn fail_coordinator_writes(&self, fail: bool) {
        self.hooks.fail.store(fail, Ordering::SeqCst);
    }

    /// Test hook: the next transition taken from now on is the coordinator's last act — once its
    /// write commits, the thread ends ([`ServerExit::CoordinatorFailed`], its links down) before
    /// anything that transition released is sent. Wait for it with [`Self::is_finished`].
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn kill_after_commit_before_send(&self) {
        self.hooks.kill_after_commit.store(true, Ordering::SeqCst);
    }

    /// Test hook: the next write that commits is answered as failed (its commit's answer lost on
    /// the way back): the coordinator's next write meets a `Conflict` with its own write.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn lose_next_write_answer(&self) {
        self.hooks.lose_answer.store(true, Ordering::SeqCst);
    }

    /// Test hook: while `held`, the `hosts` read in flight is not answered (one is issued if
    /// none is) — a database that hangs: the lease watch sees nothing until it is released,
    /// and the answer then counts from when that read was issued.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn hold_hosts_reads(&self, held: bool) {
        self.hold_hosts_reads.store(held, Ordering::SeqCst);
    }

    /// Test hook: a world cut decision whose write failed is not tried again (no abort is
    /// recorded for it) — a coordinator that dies right after the failure.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn hold_decision_retries(&self) {
        self.hold_decision_retries.store(true, Ordering::SeqCst);
    }

    /// The orderly stop: every host message sent before this call is handled, the last
    /// transition is written (waiting at most `timeout` for it), and the link goes down.
    /// [`ServerExit::Fenced`] if a write was refused as fenced (now or earlier).
    pub fn shutdown(self, timeout: Duration) -> ServerExit {
        let _ = self.stop.send(Stop::Save(timeout));
        self.join()
    }

    /// Stop without writing the last transition (the process was fenced): the thread returns as
    /// soon as it sees the stop, before it handles, writes or sends anything more; the store keeps
    /// the last committed transition (a write already handed to the store may still commit).
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
    /// `history_mut`); what it decides goes out as the core's own decisions do (after the
    /// transition holding it is written). `None` if the thread does not answer within `timeout`.
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

/// How often the runtime re-reads which hosts a world cut needs (their `hosts` rows).
pub const WORLD_CUT_HOSTS_REFRESH_MS: u64 = 1_000;
/// After a failed decision write, the wait before the next `abort_cut` while the cut is
/// within [`game::WORLD_CUT_DEADLINE_MS`] of its decision being taken.
pub const WORLD_CUT_DECISION_RETRY_MS: u64 = 250;
/// The same past that deadline (the store is down: keep trying, slowly).
pub const WORLD_CUT_DECISION_SLOW_RETRY_MS: u64 = 5_000;
/// The wait before a failed lookup of a cut's outcome is tried again.
pub const WORLD_CUT_LOOKUP_RETRY_MS: u64 = 1_000;
/// At most one warning per this many ms about failing world-cut store calls.
pub const WORLD_CUT_RETRY_LOG_MS: u64 = 10_000;
/// A succession's declaration that kept failing this long (lock timeouts behind a staged
/// part are ordinary for a moment) is warned about, throttled by [`WORLD_CUT_RETRY_LOG_MS`].
pub const DECLARE_FAILING_WARN_AFTER_MS: u64 = 3_000;

/// A store answer on its way.
type Answer<T> = Receiver<Result<T, StoreError>>;

/// A decision of the core being recorded: its `decide_cut`, then (if that failed)
/// `abort_cut` until one is stored.
struct Recording {
    d: game::CutDecide,
    /// The answer awaited (`None`: waiting to try `abort_cut` at `retry_at`).
    rx: Option<Answer<bool>>,
    aborting: bool,
    taken_ms: u64,
    retry_at: u64,
}

/// A host's question about a cut's outcome, being answered from the store.
struct Lookup {
    host: HostId,
    cut: game::CutId,
    rx: Option<Answer<Option<bool>>>,
    /// The cut was undecided: `abort_cut`'s answer.
    abort_rx: Option<Answer<bool>>,
    retry_at: u64,
}

/// World cuts for a coordinator of `claim` hosts, any number ([`start_listening_with`]).
pub struct WorldCuts {
    /// The period between cuts (`VOXEL_SAVE_SECS`).
    pub every: Duration,
    /// The world tick now ([`crate::roles::CoordinatorClock`] in a coordinator process;
    /// a test's own clock).
    pub tick: Box<dyn Fn() -> game::Tick + Send>,
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
    let thread = CoordinatorRuntime::spawn(store, start, LinkSet::one(host), fenced, None);
    Ok((link, thread))
}

/// A coordinator serving host processes over the service link
/// ([`start_listening`]): its thread, and where and with which pin hosts dial it.
pub struct ListeningCoordinator {
    pub thread: CoordinatorThread,
    pub addr: std::net::SocketAddr,
    /// The listener certificate's SHA-256 (also written to the cert-hash file, if one).
    pub cert_sha256: String,
    /// Where the login service dials and with which pin (the registry's `host_link` /
    /// `host_link_cert`), if it listens for one ([`LoginListen`]).
    pub login: Option<(std::net::SocketAddr, String)>,
    login_tally: Option<Arc<LoginTally>>,
}

impl ListeningCoordinator {
    /// What its login relay counted so far (`None`: it does not listen for the login service).
    pub fn login_counts(&self) -> Option<LoginCounts> {
        self.login_tally.as_ref().map(|t| t.counts())
    }
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
    start_listening_with(store, start, bind, token, cert_hash_file, fenced, None)
}

/// [`start_listening`], with world cuts of the hosts it serves (`cuts`: `claim` hosts).
pub fn start_listening_with(
    store: StoreHandle,
    start: CoordinatorStart,
    bind: std::net::SocketAddr,
    token: String,
    cert_hash_file: Option<std::path::PathBuf>,
    fenced: Fenced,
    cuts: Option<WorldCuts>,
) -> Result<ListeningCoordinator, String> {
    start_listening_as(store, start, bind, token, cert_hash_file, fenced, cuts, game::CrashPolicy::Rollback, "", None)
}

/// [`start_listening_with`] for a world whose hosts' crashes cost `policy`: under
/// `CrashPolicy::Succession` (`claim`, any number of hosts) the coordinator declares and runs
/// successions ([`CoordinatorRuntime`]'s succession pump); under `Rollback` it never does.
/// Its listeners admit only peers of world `world` ([`svc_link::LinkHello::world`]); with
/// `login` it also listens for the login service (a second listener: one message pair per
/// listener) and relays its orders to the gate host.
#[allow(clippy::too_many_arguments)]
pub fn start_listening_as(
    store: StoreHandle,
    start: CoordinatorStart,
    bind: std::net::SocketAddr,
    token: String,
    cert_hash_file: Option<std::path::PathBuf>,
    fenced: Fenced,
    cuts: Option<WorldCuts>,
    policy: game::CrashPolicy,
    world: &str,
    login: Option<LoginListen>,
) -> Result<ListeningCoordinator, String> {
    let login = match login {
        Some(l) => {
            if let Some(dir) = l.cert_hash_file.as_deref().and_then(std::path::Path::parent).filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            let (listener, events, senders) =
                LinkListener::bind::<LoginToHost, HostToLogin>(l.bind, l.token, world.to_string(), l.cert_hash_file.clone())
                    .map_err(|e| format!("the coordinator's login link on {}: {e}", l.bind))?;
            log::info!(
                "[coordinator] the login service dials {} (pin {}{})",
                listener.local_addr(),
                listener.cert_sha256(),
                l.cert_hash_file.map(|f| format!(", in {}", f.display())).unwrap_or_default()
            );
            Some(LoginSide::new(listener, events, senders))
        }
        None => None,
    };
    let login_at = login.as_ref().and_then(|l| l.listener.as_ref()).map(|l| (l.local_addr(), l.cert_sha256().to_string()));
    let login_tally = login.as_ref().map(|l| l.tally.clone());
    let (listener, events, senders) =
        LinkListener::bind::<HostMsg, CoordMsg>(bind, token, world.to_string(), cert_hash_file.clone())
            .map_err(|e| format!("the coordinator's service link on {bind}: {e}"))?;
    let (addr, cert_sha256) = (listener.local_addr(), listener.cert_sha256().to_string());
    log::info!(
        "[coordinator] the world coordinator (epoch {}), state {:?}; hosts dial {addr} (pin {cert_sha256}{})",
        start.fence.epoch,
        start.source,
        cert_hash_file.map(|f| format!(", in {}", f.display())).unwrap_or_default()
    );
    let mut links = LinkSet::listener(listener, events, senders);
    links.login = login;
    let thread = CoordinatorRuntime::spawn_as(store, start, links, fenced, cuts, policy);
    Ok(ListeningCoordinator { thread, addr, cert_sha256, login: login_at, login_tally })
}

/// The coordinator core with its store and fence (see the module docs).
pub struct CoordinatorRuntime {
    core: Coordinator,
    store: StoreHandle,
    fence: CoordinatorFence,
    /// The transition being written and the store's answer on its way (one at a time).
    writing: Option<(game::coordinator::Transition, Receiver<Result<CoordinatorWritten, StoreError>>)>,
    /// The coordinator row's version as last read or written: what the next write names.
    version: u64,
    /// The hashes of the cores of the writes that failed since the last commit (at most
    /// [`COORDINATOR_UNACKED_MAX`]): one of them may have committed with its answer lost.
    unacked: VecDeque<u64>,
    /// A `Conflict` was taken as this process's own lost commit and the write retried; another
    /// before a write commits ends the process.
    conflict_retried: bool,
    /// The writes' counts since the last `[coordinator] window=…` line.
    tally: WriteTally,
    #[cfg(feature = "test-hooks")]
    hooks: WriteHooks,
    /// The write in flight was taken with [`CoordinatorThread::kill_after_commit_before_send`] armed.
    #[cfg(feature = "test-hooks")]
    kill_at_commit: bool,
    links: LinkSet,
    fenced: Fenced,
    /// Test hook ([`CoordinatorThread::crash_before_claim_reply`]).
    #[cfg(feature = "test-hooks")]
    crash_before_claim_reply: Arc<AtomicBool>,
    /// Test hook ([`CoordinatorThread::hold_decision_retries`]).
    #[cfg(feature = "test-hooks")]
    hold_decision_retries: Arc<AtomicBool>,
    /// Test hook ([`CoordinatorThread::hold_hosts_reads`]).
    #[cfg(feature = "test-hooks")]
    hold_hosts_reads: Arc<AtomicBool>,
    /// The monotonic clock's zero: the instant of the start's store-clock sample
    /// ([`CoordinatorStart::store_time`]).
    started: Instant,
    /// The core's clock: store ms, extrapolated by the monotonic clock between samples (the
    /// start's, then each `hosts` read's), never backwards (Elastic 2a, decision 6).
    clock: game::coordinator::StoreClock,
    /// No transition is written before this `now_ms` (after a failed write).
    retry_at_ms: u64,
    failed_in_a_row: u32,
    /// World cuts of the `claim` hosts (`None`: off).
    cuts: Option<WorldCuts>,
    /// `now_ms` the expected hosts were last read at.
    hosts_read_ms: Option<u64>,
    /// The hosts read in flight, with the `now_ms` it was issued at.
    hosts_rx: Option<(u64, Answer<world_store::HostsRead>)>,
    /// The decision being recorded (one at a time: the core runs one cut at a time).
    recording: Option<Recording>,
    /// The world cut whose begin was last sent to the store (`record_cut_begin`, Elastic 1
    /// Task 4), and its answer while it is on its way.
    begin_recorded: Option<(game::CutId, Option<Answer<HostId>>)>,
    /// Lookups of cut outcomes in flight or waiting to be tried again.
    lookups: Vec<Lookup>,
    /// `now_ms` of the last warning about a failing world-cut store call.
    warned_ms: Option<u64>,
    /// The successions' store calls (`CrashPolicy::Succession` with world cuts; `None`: the
    /// coordinator never declares one).
    successions: Option<SuccessionPump>,
    /// What a host crash costs the world, as this coordinator was started
    /// (`VOXEL_OWNERSHIP`): every host's `Attach` must name the same ([`policy_agrees`]).
    policy: game::CrashPolicy,
}

/// The store calls of the core's successions ([`CoordinatorRuntime::pump_successions`]):
/// each answered on a receiver, at most one of each kind in flight, a failure tried again
/// after [`WORLD_CUT_DECISION_RETRY_MS`].
#[derive(Default)]
struct SuccessionPump {
    /// The open successions were read at start ([`Coordinator::successions_open`]).
    open_known: bool,
    open_rx: Option<Answer<Vec<game::Succession>>>,
    open_retry_at: u64,
    declare_rx: Option<Answer<game::Declared>>,
    declare_retry_at: u64,
    /// `now_ms` since which every declaration failed (`None`: the last one was answered).
    declare_failing_since: Option<u64>,
    rollback_rx: Option<Answer<u64>>,
    rollback_retry_at: u64,
    /// The owners read for the super-regions hosts asked for (`LandWanted`), with them.
    land_rx: Option<(Vec<game::SuperRegion>, Answer<Vec<(game::SuperRegion, i64, u64)>>)>,
    /// The end of a lapsed landless host in flight (Elastic 2a, `end_landless_host`), with it.
    end_rx: Option<(game::coordinator::EndLandless, Answer<game::LandlessEnd>)>,
    end_retry_at: u64,
}

/// The coordinator's writes in the window of its periodic line ([`CoordinatorRuntime::report_writes`]).
struct WriteTally {
    since: Instant,
    transitions: u64,
    core_bytes: u64,
    history_bytes: u64,
    conflicts: u64,
    failures: u64,
}

impl WriteTally {
    fn new() -> Self {
        WriteTally { since: Instant::now(), transitions: 0, core_bytes: 0, history_bytes: 0, conflicts: 0, failures: 0 }
    }
}

/// Why the coordinator thread ends from inside a pass.
enum End {
    /// A write (or another fenced store call) was refused as fenced.
    Fenced,
    /// A `Conflict` this process cannot explain as its own lost commit, or a second one.
    Conflict,
    /// Test hook: [`CoordinatorThread::kill_after_commit_before_send`].
    #[cfg(feature = "test-hooks")]
    Killed,
}

/// The answer of a transition's write, as the runtime takes it ([`CoordinatorRuntime::settle`]).
enum Settled {
    /// Committed: what it released, to send now.
    Released(Vec<(Target, CoordMsg)>),
    /// Not committed: the transition is back in the core; `now` = retry at once (a `Conflict`
    /// with this process's own lost commit), else after [`COORDINATOR_SAVE_RETRY_MS`].
    Held { now: bool },
    End(End),
}

/// A hash of a written core: what a re-read row is compared with ([`CoordinatorRuntime::unacked`]).
fn core_hash(core: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    core.hash(&mut h);
    h.finish()
}

impl CoordinatorRuntime {
    /// Runs `start`'s coordinator on a new `coordinator` thread serving `links`, saving
    /// into `store` under its fence. A migrated or bootstrapped state is written by its first transition.
    pub fn spawn(
        store: StoreHandle,
        start: CoordinatorStart,
        links: LinkSet,
        fenced: Fenced,
        cuts: Option<WorldCuts>,
    ) -> CoordinatorThread {
        Self::spawn_as(store, start, links, fenced, cuts, game::CrashPolicy::Rollback)
    }

    /// [`Self::spawn`] for a world whose hosts' crashes cost `policy`: `Succession` with
    /// world cuts (`claim`, any number of hosts) runs successions; without world cuts (a lone
    /// `static` host) or under `Rollback`, none is ever declared. The lease watch is fed with
    /// every `hosts` read (`feed_cuts`), which runs exactly when world cuts do — so successions
    /// are driven whenever they can be needed. Every `Attach` must name `policy`
    /// ([`policy_agrees`]).
    pub fn spawn_as(
        store: StoreHandle,
        start: CoordinatorStart,
        links: LinkSet,
        fenced: Fenced,
        cuts: Option<WorldCuts>,
        policy: game::CrashPolicy,
    ) -> CoordinatorThread {
        let (stop_tx, stop_rx) = crossbeam::channel::bounded(1);
        #[cfg(feature = "test-hooks")]
        let (inspect_tx, inspect_rx) = crossbeam::channel::unbounded();
        #[cfg(feature = "test-hooks")]
        let crash_before_claim_reply = Arc::new(AtomicBool::new(false));
        #[cfg(feature = "test-hooks")]
        let hold_decision_retries = Arc::new(AtomicBool::new(false));
        #[cfg(feature = "test-hooks")]
        let hold_hosts_reads = Arc::new(AtomicBool::new(false));
        #[cfg(feature = "test-hooks")]
        let hooks = WriteHooks::default();
        let fence = start.fence;
        let (store_ms, started) = start.store_time;
        let mut clock = game::coordinator::StoreClock::default();
        clock.sample(store_ms, 0);
        // A migrated or bootstrapped core is wholly unwritten: its first transition writes it.
        let mut core = start.coordinator;
        let version = start.version;
        if let Some(c) = &cuts {
            core.set_cut_epoch(fence.epoch);
            core.set_world_cuts(c.every.as_millis() as u64);
        }
        let successions = (cuts.is_some() && policy == game::CrashPolicy::Succession).then(|| {
            core.set_crash_policy(policy);
            // Nothing is declared and no cut begins until the open ones are read.
            core.successions_unknown();
            SuccessionPump::default()
        });
        let runtime = CoordinatorRuntime {
            core,
            store,
            fence,
            writing: None,
            version,
            unacked: VecDeque::new(),
            conflict_retried: false,
            tally: WriteTally::new(),
            #[cfg(feature = "test-hooks")]
            hooks: hooks.clone(),
            #[cfg(feature = "test-hooks")]
            kill_at_commit: false,
            links,
            fenced,
            #[cfg(feature = "test-hooks")]
            crash_before_claim_reply: crash_before_claim_reply.clone(),
            #[cfg(feature = "test-hooks")]
            hold_decision_retries: hold_decision_retries.clone(),
            #[cfg(feature = "test-hooks")]
            hold_hosts_reads: hold_hosts_reads.clone(),
            started,
            clock,
            retry_at_ms: 0,
            failed_in_a_row: 0,
            cuts,
            hosts_read_ms: None,
            hosts_rx: None,
            recording: None,
            begin_recorded: None,
            lookups: Vec::new(),
            warned_ms: None,
            successions,
            policy,
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
            #[cfg(feature = "test-hooks")]
            hold_decision_retries,
            #[cfg(feature = "test-hooks")]
            hold_hosts_reads,
            #[cfg(feature = "test-hooks")]
            hooks,
        }
    }

    /// The process's monotonic clock, ms since [`Self::started`].
    fn mono_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// The core's `now_ms`: store time ([`game::coordinator::StoreClock`]).
    fn now_ms(&self) -> u64 {
        self.clock.now(self.mono_ms())
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
                    // Nothing more is handled, written or sent.
                    log::info!("[coordinator] stopped without writing its last transition");
                    self.down();
                    return ServerExit::Clean;
                }
                Ok(Stop::Save(timeout)) => Some(timeout),
                Err(TryRecvError::Empty) => None,
                // The handle is gone (its owner unwound): stop as an orderly exit would.
                Err(TryRecvError::Disconnected) => Some(crate::EXIT_SAVE_TIMEOUT),
            };
            let now = self.now_ms();
            let health = crate::health::Health::global();
            // The split coordinator's loop (role `coordinator`); in-process (role `all`) the
            // host's main loop beats.
            if matches!(self.links.links, Links::Listener(_)) {
                health.loop_beat();
            }
            self.drain_hosts(now);
            self.drain_login(now);
            #[cfg(feature = "test-hooks")]
            for f in inspect.try_iter() {
                f(&mut self.core);
            }
            self.feed_cuts(now);
            self.core.maintain(now);
            if let Some(blocked) = self.core.take_cuts_blocked() {
                self.warn_cuts(now, &blocked.to_string());
            }
            if self.pump_cuts(now).is_err() || self.pump_successions(now).is_err() {
                return self.fence_out();
            }
            // Last: what this pass decided (the cuts' and successions' answers too) is in this
            // pass's transition.
            let out = match self.pump_transition(now) {
                Ok(out) => out,
                Err(end) => return self.end(end),
            };
            self.report_writes();
            health.world_generation(self.core.world_generation());
            health.successions_open(self.core.successions_open_count() as u64);
            #[cfg(feature = "test-hooks")]
            if self.crash_before_claim_reply.load(Ordering::SeqCst)
                && out.iter().any(|(_, m)| matches!(m, CoordMsg::Reply { reply, .. } if reply.grants_claim()))
            {
                // Its transition was written; it never leaves.
                log::warn!("[coordinator] test crash before a claim-granting reply left");
                self.down();
                return ServerExit::CoordinatorFailed;
            }
            self.send(out);
            self.forward_login();
            if let Some(timeout) = stopping {
                return self.finish(timeout);
            }
        }
    }

    /// Block until a host message, the write's answer or a stop arrives, at most
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
        if let Some(login) = &self.links.login {
            sel.recv(&login.events);
        }
        if let Some((_, written)) = &self.writing {
            sel.recv(written);
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
                                self.core.host_detached(host.ordinal);
                            }
                        }
                    }
                }
            }
            Links::Listener(l) => {
                while let Ok((peer, event)) = l.events.try_recv() {
                    match event {
                        LinkEvent::Up(hello) => match hello.role {
                            PeerRole::Host { id: ordinal, boot, reg } => match l.routes.admit(
                                peer,
                                ordinal,
                                reg,
                                after_store_read(stored_registration(&self.store, ordinal)),
                            ) {
                                Admit::Admitted { replaced } => {
                                    log::info!(
                                        "[coordinator] host {ordinal} connected (peer {}, boot {boot:#x}, registration {reg})",
                                        peer.0
                                    );
                                    l.routes.boots.insert(peer, boot);
                                    if let Some(old) = replaced {
                                        l.senders.close(old);
                                        self.core.host_detached(ordinal);
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
                                if let HostMsg::Attach { generation, boot, policy, .. } = &msg {
                                    let hello = l.routes.boots.get(&peer).copied();
                                    if hello.is_some_and(|b| b != *boot) {
                                        log::warn!(
                                            "[coordinator] host {ordinal}'s Attach names boot {boot:#x}, its hello {:#x}",
                                            hello.unwrap_or_default()
                                        );
                                    }
                                    match after_store_read(generation_current(&self.store, ordinal, *generation)) {
                                        Ok(()) => {}
                                        Err(stored) => {
                                            l.routes.refuse(peer);
                                            match stored {
                                                // Told why, the link left open for the message to
                                                // arrive (a close could overtake it); the process
                                                // exits and its connection ends with it.
                                                Some(stored) => {
                                                    l.senders.send(peer, CoordMsg::Generation(stored));
                                                }
                                                None => {
                                                    l.senders.close(peer);
                                                }
                                            }
                                            self.core.host_detached(ordinal);
                                            continue;
                                        }
                                    }
                                    if let Err(why) = policy_agrees(self.policy, *policy) {
                                        log::error!("[coordinator] host {ordinal}'s Attach is refused: {why}");
                                        l.routes.refuse(peer);
                                        l.senders.close(peer);
                                        self.core.host_detached(ordinal);
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
                                self.core.host_detached(ordinal);
                            }
                        }
                    }
                }
            }
        }
    }

    /// The login service's connections and orders: an order goes to the gate host of this
    /// moment ([`Coordinator::login_order`]) or, with none, is dropped (the login service
    /// answers 503 at its ack timeout and the client retries; a kick is resent). Only a peer
    /// whose hello said `PeerRole::Login` is served; any other is closed.
    fn drain_login(&mut self, now: u64) {
        let Some(login) = self.links.login.as_mut() else { return };
        let mut relay = Vec::new();
        while let Ok((peer, event)) = login.events.try_recv() {
            match event {
                LinkEvent::Up(hello) if hello.role == PeerRole::Login => {
                    log::info!("[coordinator] the login service connected (peer {})", peer.0);
                    login.peers.insert(peer);
                    if !login.held.is_empty() {
                        log::info!("[coordinator] {} login session ends held for the login service: sent", login.held.len());
                    }
                    for msg in std::mem::take(&mut login.held) {
                        login.senders.send(peer, msg);
                    }
                }
                LinkEvent::Up(hello) => {
                    log::warn!("[coordinator] login link peer {} is {:?}, not the login service: closed", peer.0, hello.role);
                    login.senders.close(peer);
                }
                LinkEvent::Down(why) => {
                    if login.peers.remove(&peer) {
                        log::info!("[coordinator] the login service (peer {}) left: {why}", peer.0);
                    }
                }
                LinkEvent::Msg(msg) if login.peers.contains(&peer) => {
                    login.tally.orders.fetch_add(1, Ordering::SeqCst);
                    match self.core.login_order(crate::login_link::order_from(msg)) {
                        Some((gate, msg)) => relay.push((Target::Host(gate), msg)),
                        // Counted; reported once per period below.
                        None => {
                            login.tally.orders_dropped.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
                LinkEvent::Msg(_) => log::warn!("[coordinator] a message from login link peer {} that is not the login service: ignored", peer.0),
            }
        }
        login.report(now);
        self.send(relay);
    }

    /// What the hosts told the login service ([`HostMsg::Login`]) to every connected login
    /// service, unchanged ([`LoginSide::tell`]).
    fn forward_login(&mut self) {
        let out = self.core.take_login_out();
        match self.links.login.as_mut() {
            Some(login) => {
                for answer in out {
                    login.tell(crate::login_link::answer_from(answer));
                }
            }
            None if !out.is_empty() => log::debug!("[coordinator] {} answers for a login service this coordinator does not serve: dropped", out.len()),
            None => {}
        }
    }

    /// The world tick, and (every [`WORLD_CUT_HOSTS_REFRESH_MS`]) the hosts a cut needs:
    /// those whose `hosts` row is not clean and live. A read that fails stops cuts until a good
    /// one ([`Coordinator::expected_hosts_unknown`]). Every read feeds the lease watch
    /// ([`game::coordinator::LeaseWatch`]); a failed one makes it forget what it saw.
    fn feed_cuts(&mut self, now: u64) {
        let Some(cuts) = &self.cuts else { return };
        self.core.set_world_tick((cuts.tick)());
        #[cfg(feature = "test-hooks")]
        if self.hold_hosts_reads.load(Ordering::SeqCst) && self.hosts_rx.is_some() {
            return;
        }
        if let Some((issued_ms, rx)) = &self.hosts_rx {
            let issued_ms = *issued_ms;
            match rx.try_recv() {
                Err(TryRecvError::Empty) => return,
                Ok(Ok(world_store::HostsRead { rows, store_ms })) => {
                    // The store's clock, taken as of this receipt: it lags the store by the
                    // answer's way back, never leads it (a correction for the round trip would
                    // count a stalled read's queueing as time, a forward step `StoreClock` keeps
                    // and the lease watch's spans would grow by). The core's `now` follows from
                    // the next pass.
                    self.clock.sample(store_ms, self.mono_ms());
                    // A host that left and registered again (another boot) is expected again.
                    self.core.set_host_boots(rows.iter().map(|r| (r.id, r.boot)).collect());
                    // A row marked dead (a takeover declared it) runs nothing: not expected
                    // (`decide_cut` reads the same predicate).
                    let live: BTreeSet<HostId> =
                        rows.iter().filter(|r| !r.clean && r.state == world_store::HOST_LIVE).map(|r| r.id).collect();
                    crate::health::Health::global().hosts_live(live.len() as u64);
                    self.core.set_expected_hosts(live);
                    let seen: Vec<game::coordinator::HostSeen> = rows
                        .iter()
                        .map(|r| game::coordinator::HostSeen {
                            ordinal: r.id,
                            boot: r.boot,
                            clean: r.clean,
                            state: r.state,
                            seen_ms: r.seen_ms,
                            draining: r.drain_in_ms.is_some(),
                            landed: r.owns_land,
                        })
                        .collect();
                    // Issue-to-receive stamps: an answer that waited in the channel through a
                    // stall of this thread never stretches the span (`LeaseWatch::observe`).
                    self.core.observe_leases(&seen, issued_ms, now);
                }
                Ok(Err(e)) => {
                    self.core.expected_hosts_unknown();
                    self.core.leases_unread();
                    self.warn_cuts(now, &format!("the hosts could not be read ({e}); no world cut until they are"));
                }
                Err(TryRecvError::Disconnected) => {
                    self.core.expected_hosts_unknown();
                    self.core.leases_unread();
                    self.warn_cuts(now, "the world-store thread has stopped; no world cut");
                }
            }
            self.hosts_rx = None;
        }
        if self.hosts_read_ms.is_none_or(|at| now >= at + WORLD_CUT_HOSTS_REFRESH_MS) {
            self.hosts_read_ms = Some(now);
            self.hosts_rx = Some((now, self.store.hosts_later()));
        }
    }

    /// A warning about a failing world-cut store call, at most once per
    /// [`WORLD_CUT_RETRY_LOG_MS`].
    fn warn_cuts(&mut self, now: u64, what: &str) {
        if self.warned_ms.is_none_or(|at| now >= at + WORLD_CUT_RETRY_LOG_MS) {
            self.warned_ms = Some(now);
            log::warn!("[coordinator] {what}");
        }
    }

    /// Record the core's world-cut decision in the store and tell the core the STORED
    /// decision; answer the hosts' questions about cuts the core does not remember. Never
    /// waits. `Err`: fenced.
    fn pump_cuts(&mut self, now: u64) -> Result<(), ()> {
        if self.cuts.is_none() {
            return Ok(());
        }
        // Every cut's begin is recorded in the store (Elastic 1 Task 4): the decision may then
        // leave out a host registered after it (it owned nothing at the cut's tick, and the
        // participants froze their sessions with it). Not waited for: a decision taken before
        // the record lands lists every dirty host, as before — safe, a newcomer then aborts it.
        if let Some((cut, _)) = self.core.world_cut_running() {
            if self.begin_recorded.as_ref().is_none_or(|(c, _)| *c != cut) {
                self.begin_recorded = Some((cut, Some(self.store.record_cut_begin_later(cut, self.fence))));
            }
        }
        if let Some((cut, Some(rx))) = self.begin_recorded.as_mut() {
            let cut = *cut;
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                Ok(Ok(bound)) => {
                    log::debug!("[coordinator] world cut {}: begin recorded (hosts below {bound} must take part)", cut.0);
                    self.begin_recorded = Some((cut, None));
                }
                Ok(Err(StoreError::Fenced { .. })) => return Err(()),
                Ok(Err(e)) => {
                    self.begin_recorded = Some((cut, None));
                    self.warn_cuts(now, &format!("world cut {}: its begin could not be recorded ({e}); a host joining during it aborts it", cut.0));
                }
                Err(TryRecvError::Disconnected) => self.begin_recorded = Some((cut, None)),
            }
        }
        if self.recording.is_none() {
            if let Some(d) = self.core.take_decision() {
                // `settles`: the open successions this cut settles if it commits — the core names
                // them only once every survivor applied and the heir ADOPTED (the heir's part then
                // carries `WorldCut::adopts`, Task 9); the store does not check that pairing.
                let rx = self.store.decide_cut_later(
                    d.cut,
                    d.tick,
                    d.commit,
                    d.hosts.clone(),
                    d.leaving.clone(),
                    self.fence,
                    d.generation,
                    d.settles.clone(),
                );
                self.recording = Some(Recording { d, rx: Some(rx), aborting: false, taken_ms: now, retry_at: now });
            }
        }
        if let Some(r) = self.recording.as_mut() {
            let answer = match r.rx.as_ref().map(|rx| rx.try_recv()) {
                None | Some(Err(TryRecvError::Empty)) => None,
                Some(Ok(answer)) => Some(answer),
                Some(Err(TryRecvError::Disconnected)) => Some(Err(StoreError::Backend("the world-store thread has stopped".into()))),
            };
            match answer {
                None => {}
                Some(Ok(stored)) => {
                    let d = self.recording.take().expect("recording").d;
                    if d.commit && !stored {
                        log::warn!("[coordinator] world cut {}: a commit was asked, the store recorded an abort", d.cut.0);
                    }
                    game::perf_event!("world_cut_decided", "cut={} tick={} committed={stored}", d.cut.0, d.tick);
                    let health = crate::health::Health::global();
                    health.cut_decided(stored);
                    if stored {
                        // Its commit settles the successions it names (`settles`).
                        for _ in &d.settles {
                            health.succession_settled();
                        }
                    }
                    if stored {
                        // The hosts' listeners re-read what it saved (Task 19). After the
                        // decision's own transaction, not waited for: a lost notice is
                        // covered by their poll, a duplicate costs a read.
                        let _ = self.store.notify_cut_later(d.cut, d.tick);
                    }
                    self.core.decided(d.cut, stored);
                }
                Some(Err(StoreError::Fenced { .. })) => return Err(()),
                Some(Err(e)) => {
                    if matches!(e, StoreError::StaleGeneration { .. }) {
                        // The world rolled back without this coordinator (`worldctl rollback`):
                        // the successions it runs may be abandoned.
                        self.core.successions_reread();
                    }
                    let (cut, aborting) = (r.d.cut.0, r.aborting);
                    let slow = now.saturating_sub(r.taken_ms) >= game::WORLD_CUT_DEADLINE_MS;
                    r.rx = None;
                    r.aborting = true;
                    r.retry_at = now + if slow { WORLD_CUT_DECISION_SLOW_RETRY_MS } else { WORLD_CUT_DECISION_RETRY_MS };
                    let what = if aborting { "its abort" } else { "the decision" };
                    self.warn_cuts(now, &format!("world cut {cut}: {what} could not be recorded ({e}); aborting it again"));
                }
            }
        }
        #[cfg(feature = "test-hooks")]
        let held = self.hold_decision_retries.load(Ordering::SeqCst);
        #[cfg(not(feature = "test-hooks"))]
        let held = false;
        if let Some(r) = self.recording.as_mut().filter(|r| !held && r.rx.is_none() && now >= r.retry_at) {
            r.rx = Some(self.store.abort_cut_later(r.d.cut, r.d.tick));
        }
        for (host, cut) in self.core.take_cut_lookups() {
            if !self.lookups.iter().any(|l| l.host == host && l.cut == cut) {
                self.lookups.push(Lookup { host, cut, rx: Some(self.store.cut_decision_later(cut)), abort_rx: None, retry_at: now });
            }
        }
        let mut answered = Vec::new();
        let mut failed = Vec::new();
        for (i, l) in self.lookups.iter_mut().enumerate() {
            if l.rx.is_none() && l.abort_rx.is_none() {
                if now >= l.retry_at {
                    l.rx = Some(self.store.cut_decision_later(l.cut));
                }
                continue;
            }
            if let Some(rx) = &l.rx {
                match rx.try_recv() {
                    Err(TryRecvError::Empty) => {}
                    Ok(Ok(Some(stored))) => answered.push((i, stored)),
                    Ok(Ok(None)) => {
                        l.rx = None;
                        l.abort_rx = Some(self.store.abort_cut_later(l.cut, 0));
                    }
                    Ok(Err(e)) => failed.push((i, e.to_string())),
                    Err(TryRecvError::Disconnected) => failed.push((i, "the world-store thread has stopped".into())),
                }
            } else if let Some(rx) = &l.abort_rx {
                match rx.try_recv() {
                    Err(TryRecvError::Empty) => {}
                    Ok(Ok(stored)) => answered.push((i, stored)),
                    Ok(Err(e)) => failed.push((i, e.to_string())),
                    Err(TryRecvError::Disconnected) => failed.push((i, "the world-store thread has stopped".into())),
                }
            }
        }
        for (i, e) in &failed {
            let l = &mut self.lookups[*i];
            l.rx = None;
            l.abort_rx = None;
            l.retry_at = now + WORLD_CUT_LOOKUP_RETRY_MS;
            let (cut, host) = (l.cut.0, l.host);
            self.warn_cuts(now, &format!("the outcome of cut {cut} for host {host} could not be read ({e}); asking again"));
        }
        for (i, stored) in answered.iter().rev() {
            let l = self.lookups.remove(*i);
            self.core.cut_outcome(l.host, l.cut, *stored);
        }
        Ok(())
    }

    /// The successions' store calls (`CrashPolicy::Succession`; the core decides, see
    /// `game::coordinator::succession`): first the open successions are read (a restart
    /// re-sends their orders); then a wanted rollback is executed (`rollback_world`, the core
    /// tells every host the new generation); else the core's next declaration is executed
    /// (`declare_succession`) and its answer handed back. Never waits; a failure (a lock
    /// timeout behind a staged part, the store down) is tried again after
    /// [`WORLD_CUT_DECISION_RETRY_MS`]. `Err`: fenced.
    fn pump_successions(&mut self, now: u64) -> Result<(), ()> {
        let Some(p) = self.successions.as_mut() else { return Ok(()) };
        // The core wants the open successions read again (the store changed them behind its
        // back: an offline `worldctl retire-host`, a `worldctl rollback`): a read in flight is
        // dropped — it may predate the change.
        if self.core.take_successions_reread() {
            p.open_known = false;
            p.open_rx = None;
        }
        if !p.open_known {
            if p.open_rx.is_none() && now >= p.open_retry_at {
                p.open_rx = Some(self.store.open_successions_later());
            }
            let answer = p.open_rx.as_ref().map(|rx| rx.try_recv());
            let failed = match answer {
                None | Some(Err(TryRecvError::Empty)) => None,
                Some(Ok(Ok(list))) => {
                    p.open_known = true;
                    p.open_rx = None;
                    log::info!("[coordinator] {} open succession(s) in the store", list.len());
                    self.core.successions_open(list, now);
                    None
                }
                Some(Ok(Err(e))) => Some(e.to_string()),
                Some(Err(TryRecvError::Disconnected)) => Some("the world-store thread has stopped".into()),
            };
            if let Some(e) = failed {
                p.open_rx = None;
                p.open_retry_at = now + WORLD_CUT_DECISION_RETRY_MS;
                self.warn_cuts(now, &format!("the open successions could not be read ({e}); no cut and no succession until they are"));
            }
            return Ok(());
        }
        // Land of a host that left, asked for (`LandWanted`): the owners read (a plain read),
        // each owner told to the core, which gives it away at its next declaration between cuts
        // (no heir wait: a host that left never returns as itself). A failed read is
        // dropped: the hosts ask again every `LAND_WANTED_EVERY_MS`.
        let mut failed = None;
        if let Some((srs, rx)) = &p.land_rx {
            let answer = match rx.try_recv() {
                Err(TryRecvError::Empty) => None,
                Ok(answer) => Some(answer),
                Err(TryRecvError::Disconnected) => Some(Err(StoreError::Backend("the world-store thread has stopped".into()))),
            };
            if let Some(answer) = answer {
                match answer {
                    Ok(rows) => {
                        let owners: BTreeMap<game::SuperRegion, i64> = rows.into_iter().map(|(sr, o, _)| (sr, o)).collect();
                        let wanted: BTreeSet<HostId> =
                            srs.iter().filter_map(|sr| owners.get(sr)).filter_map(|o| u64::try_from(*o).ok().map(HostId)).collect();
                        for owner in wanted {
                            self.core.land_wanted(owner, now);
                        }
                    }
                    Err(e) => failed = Some(e),
                }
                p.land_rx = None;
            }
        }
        if p.land_rx.is_none() {
            let srs = self.core.take_land_lookups();
            if !srs.is_empty() {
                p.land_rx = Some((srs, self.store.owners_later()));
            }
        }
        if let Some(e) = failed {
            // Throttled; the rest of the pump runs on as usual.
            self.warn_cuts(now, &format!("the owners of land asked for could not be read ({e}); the hosts ask again"));
        }
        let Some(p) = self.successions.as_mut() else { return Ok(()) };
        if self.core.rollback_wanted() {
            if p.rollback_rx.is_none() && now >= p.rollback_retry_at {
                let from = self.core.rollback_from().unwrap_or_default();
                p.rollback_rx = Some(self.store.rollback_world_later(self.fence, from));
            }
            let answer = match p.rollback_rx.as_ref().map(|rx| rx.try_recv()) {
                None | Some(Err(TryRecvError::Empty)) => return Ok(()),
                Some(Ok(answer)) => answer,
                Some(Err(TryRecvError::Disconnected)) => Err(StoreError::Backend("the world-store thread has stopped".into())),
            };
            p.rollback_rx = None;
            match answer {
                Ok(generation) => {
                    game::perf_event!("world_rollback", "generation={generation}");
                    crate::health::Health::global().world_generation(generation);
                    self.core.rolled_back(generation);
                }
                Err(StoreError::Fenced { .. }) => return Err(()),
                Err(e) => {
                    p.rollback_retry_at = now + WORLD_CUT_DECISION_RETRY_MS;
                    self.warn_cuts(now, &format!("the world rollback could not be recorded ({e}); trying again"));
                }
            }
            return Ok(());
        }
        // A lapsed landless host is ended (Elastic 2a): its own store call, beside a declaration
        // (the store serialises the two on the world row) — never a succession, never a rollback.
        if let Some((end, rx)) = &p.end_rx {
            let end = *end;
            let answer = match rx.try_recv() {
                Err(TryRecvError::Empty) => None,
                Ok(answer) => Some(answer),
                Err(TryRecvError::Disconnected) => Some(Err(StoreError::Backend("the world-store thread has stopped".into()))),
            };
            if let Some(answer) = answer {
                p.end_rx = None;
                match answer {
                    Ok(outcome) => {
                        if outcome == game::LandlessEnd::Ended {
                            self.close_succeeded(&[(end.host, end.boot)]);
                        }
                        self.core.landless_ended(end, outcome, now);
                    }
                    Err(StoreError::Fenced { .. }) => return Err(()),
                    Err(e) => {
                        self.core.end_failed();
                        let Some(p) = self.successions.as_mut() else { return Ok(()) };
                        p.end_retry_at = now + WORLD_CUT_DECISION_RETRY_MS;
                        self.warn_cuts(now, &format!("host {} could not be ended ({e}); trying again", end.host));
                    }
                }
            }
        }
        let Some(p) = self.successions.as_mut() else { return Ok(()) };
        if p.end_rx.is_none() && now >= p.end_retry_at {
            if let Some(end) = self.core.take_end_landless(now) {
                p.end_rx = Some((end, self.store.end_landless_host_later(end.host, end.boot, self.fence)));
            }
        }
        if let Some(rx) = &p.declare_rx {
            let answer = match rx.try_recv() {
                Err(TryRecvError::Empty) => return Ok(()),
                Ok(answer) => answer,
                Err(TryRecvError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
            };
            p.declare_rx = None;
            match answer {
                Ok(declared) => {
                    p.declare_failing_since = None;
                    if let game::Declared::Done(list) = &declared {
                        for _ in list {
                            crate::health::Health::global().succession_declared();
                        }
                        let dead: Vec<(HostId, u64)> = list.iter().map(|s| (s.predecessor, s.predecessor_boot)).collect();
                        self.close_succeeded(&dead);
                    }
                    self.core.declared(declared, now);
                }
                Err(StoreError::Fenced { .. }) => return Err(()),
                Err(e) => {
                    // A lock timeout behind a staged part is a retry, not an error — but one
                    // that keeps failing is why nothing is saved and nobody inherits (a host
                    // frozen inside a transaction, a store that is down): said, throttled.
                    self.core.declare_failed();
                    p.declare_retry_at = now + WORLD_CUT_DECISION_RETRY_MS;
                    let since = *p.declare_failing_since.get_or_insert(now);
                    if now.saturating_sub(since) >= DECLARE_FAILING_WARN_AFTER_MS {
                        let what = format!(
                            "a succession could not be declared for {} s ({e}); still trying — until it is, no world cut \
                             begins and the dead host's land has no owner",
                            now.saturating_sub(since) / 1000
                        );
                        self.warn_cuts(now, &what);
                    } else {
                        log::debug!("[coordinator] a succession could not be declared yet ({e}); trying again");
                    }
                }
            }
            return Ok(());
        }
        if now >= p.declare_retry_at {
            if let Some(d) = self.core.declare_wanted(now) {
                let generation = self.core.world_generation();
                p.declare_rx = Some(self.store.declare_succession_later(d.dead, d.heirs, d.kind, self.fence, generation));
            }
        }
        Ok(())
    }

    /// The live connections of hosts just succeeded (`(ordinal, boot)`), if a zombie of that
    /// boot is still connected (hung, or cut off from the store only): closed, its messages
    /// ignored from now on. A fresh hello of it is refused by [`Routes::admit`] (its `hosts`
    /// row is `HOST_DEAD` now), and the core drops whatever of it is still queued.
    fn close_succeeded(&mut self, dead: &[(HostId, u64)]) {
        let Links::Listener(l) = &mut self.links.links else { return };
        for &(ordinal, boot) in dead {
            let Some(peer) = l.routes.peer_of(ordinal) else { continue };
            if l.routes.boots.get(&peer) == Some(&boot) {
                log::warn!("[coordinator] host {ordinal} (boot {boot:#x}) was succeeded: its connection is closed");
                l.routes.refuse(peer);
                l.senders.close(peer);
            }
        }
    }

    /// The transitions (Elastic 2a, decision 2): the write in flight, if answered, releases its
    /// messages (or, failed, hands the transition back to the core); with no write in flight the
    /// core's next transition is written. With nothing durable pending, what the core decided
    /// leaves at once. Returns what may be sent now, in order; `Err`: the thread ends.
    fn pump_transition(&mut self, now: u64) -> Result<Vec<(Target, CoordMsg)>, End> {
        let mut out = Vec::new();
        if let Some((_, written)) = &self.writing {
            let answer = match written.try_recv() {
                Err(TryRecvError::Empty) => return Ok(out),
                Ok(answer) => answer,
                Err(TryRecvError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
            };
            let (t, _) = self.writing.take().expect("checked");
            match self.settle(t, answer, now) {
                Settled::Released(released) => out = released,
                Settled::Held { .. } => {}
                Settled::End(end) => return Err(end),
            }
        }
        if self.writing.is_some() || now < self.retry_at_ms {
            return Ok(out);
        }
        if !self.write_next() {
            out.extend(self.core.take_out());
        }
        Ok(out)
    }

    /// Hands the core's next transition to the store (one at a time: none may be in flight);
    /// `false` when nothing durable changed.
    fn write_next(&mut self) -> bool {
        debug_assert!(self.writing.is_none());
        let Some(t) = self.core.take_transition() else { return false };
        #[cfg(feature = "test-hooks")]
        {
            self.kill_at_commit = self.hooks.kill_after_commit.load(Ordering::SeqCst);
            if self.hooks.fail.load(Ordering::SeqCst) {
                let (tx, rx) = crossbeam::channel::bounded(1);
                let _ = tx.send(Err(StoreError::Backend("test hook: coordinator writes fail".into())));
                self.writing = Some((t, rx));
                return true;
            }
        }
        let written = self.store.write_coordinator_later(self.fence, self.version, t.core.clone(), t.history.clone());
        self.writing = Some((t, written));
        true
    }

    /// The answer of the write of transition `t` (decision 2): committed → its messages, the
    /// store clock sampled from the commit; `Fenced` → the end; `Conflict` → the row re-read: this
    /// process's own lost commit is written over at once (once), anything else is the end; any
    /// other failure hands `t` back to the core — its messages wait, its inputs stay applied
    /// once — and the next write is tried after [`COORDINATOR_SAVE_RETRY_MS`].
    fn settle(&mut self, t: game::coordinator::Transition, answer: Result<CoordinatorWritten, StoreError>, now: u64) -> Settled {
        #[cfg(feature = "test-hooks")]
        let answer = match answer {
            Ok(_) if self.hooks.lose_answer.swap(false, Ordering::SeqCst) => {
                Err(StoreError::Backend("test hook: the commit's answer was lost".into()))
            }
            answer => answer,
        };
        match answer {
            Ok(w) => {
                self.core.transition_written();
                self.version = w.version;
                self.failed_in_a_row = 0;
                self.unacked.clear();
                self.conflict_retried = false;
                let mono = self.mono_ms();
                self.clock.sample(w.store_ms, mono);
                let (core, history) = (t.core.len() as u64, t.history.as_ref().map_or(0, |h| h.len() as u64));
                self.tally.transitions += 1;
                self.tally.core_bytes += core;
                self.tally.history_bytes += history;
                crate::health::Health::global().coordinator_written(core, history);
                #[cfg(feature = "test-hooks")]
                if std::mem::take(&mut self.kill_at_commit) {
                    return Settled::End(End::Killed);
                }
                Settled::Released(t.out)
            }
            Err(StoreError::Fenced { .. }) => Settled::End(End::Fenced),
            Err(StoreError::Conflict) => {
                self.tally.conflicts += 1;
                crate::health::Health::global().coordinator_conflict();
                if self.conflict_retried {
                    log::error!(
                        "[coordinator] a second write conflict (version {}) before any write committed: another writer \
                         holds the coordinator row under this epoch; stopping as fenced",
                        self.version
                    );
                    return Settled::End(End::Conflict);
                }
                match self.store.load_coordinator() {
                    Ok(Some(row)) if row.version == self.version + 1 && self.unacked.contains(&core_hash(&row.core)) => {
                        log::warn!(
                            "[coordinator] write conflict: the row holds this process's own write at version {} (its \
                             commit's answer was lost); writing again over it",
                            row.version
                        );
                        self.version = row.version;
                        self.conflict_retried = true;
                        self.core.transition_failed(t);
                        self.retry_at_ms = now;
                        Settled::Held { now: true }
                    }
                    Ok(row) => {
                        log::error!(
                            "[coordinator] write conflict: the row is at version {:?}, written by another writer under \
                             this epoch (this process wrote version {}); stopping as fenced — the next start loads it",
                            row.map(|r| r.version),
                            self.version
                        );
                        Settled::End(End::Conflict)
                    }
                    // Not known yet: as any failed write (the next one meets the conflict again).
                    Err(e) => self.failed(t, &format!("a conflict, and the row could not be re-read: {e}"), now),
                }
            }
            Err(e) => self.failed(t, &e.to_string(), now),
        }
    }

    /// A write of `t` that failed with `why` (not fenced): `t` back in the core, the next write
    /// after [`COORDINATOR_SAVE_RETRY_MS`], saving reported stuck after
    /// [`COORDINATOR_FAILURES_BEFORE_STUCK`] in a row.
    fn failed(&mut self, t: game::coordinator::Transition, why: &str, now: u64) -> Settled {
        if self.unacked.len() == COORDINATOR_UNACKED_MAX {
            self.unacked.pop_front();
        }
        self.unacked.push_back(core_hash(&t.core));
        self.core.transition_failed(t);
        self.failed_in_a_row += 1;
        self.tally.failures += 1;
        self.retry_at_ms = now + COORDINATOR_SAVE_RETRY_MS;
        if self.failed_in_a_row == COORDINATOR_FAILURES_BEFORE_STUCK {
            log::error!(
                "[coordinator] writing is stuck: {} writes in a row failed, the last with: {why} \
                 (nothing the coordinator decided leaves until one commits)",
                self.failed_in_a_row
            );
            game::perf_event!("coordinator_save_stuck", "failed={}", self.failed_in_a_row);
        } else {
            log::warn!("[coordinator] write failed ({} in a row): {why}; retrying", self.failed_in_a_row);
        }
        Settled::Held { now: false }
    }

    /// The periodic `[coordinator] window=…` line (every [`game::METRICS_REPORT_INTERVAL`], only
    /// when anything was written or failed): the transitions written in the window, their core
    /// and history bytes, the conflicts and failed writes (Elastic 2a, decision 8: E2b's measure).
    fn report_writes(&mut self) {
        let window = self.tally.since.elapsed();
        if window < game::METRICS_REPORT_INTERVAL {
            return;
        }
        let t = std::mem::replace(&mut self.tally, WriteTally::new());
        if t.transitions == 0 && t.conflicts == 0 && t.failures == 0 {
            return;
        }
        log::info!(
            "[coordinator] window={:.1}s transitions={} core_bytes={} history_bytes={} conflicts={} write_failures={} version={}",
            window.as_secs_f32(),
            t.transitions,
            t.core_bytes,
            t.history_bytes,
            t.conflicts,
            t.failures,
            self.version
        );
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

    /// The orderly stop: the write in flight is settled, then the last transition (whatever
    /// was handled since) is written, all within `timeout`; what the commits released is sent
    /// (the hosts may still read it while they stop). There is no other "final save".
    fn finish(mut self, timeout: Duration) -> ServerExit {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        // Bounded: the write in flight, the last transition, and one retry after a lost commit.
        for _ in 0..4 {
            if self.writing.is_none() && !self.write_next() {
                break;
            }
            let (t, written) = self.writing.take().expect("a write in flight");
            let left = deadline.saturating_duration_since(Instant::now());
            let answer = match written.recv_timeout(left) {
                Ok(answer) => answer,
                Err(RecvTimeoutError::Timeout) => {
                    // Not committed (as far as anyone knows): nothing it decided, nor anything
                    // decided after it, leaves (the core's `take_out` refuses while it is in flight).
                    log::error!("[coordinator] the world store did not answer the write at exit within {timeout:?}");
                    break;
                }
                Err(RecvTimeoutError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
            };
            match self.settle(t, answer, 0) {
                Settled::Released(released) => out.extend(released),
                Settled::Held { now: true } => {}
                Settled::Held { now: false } => {
                    log::error!("[coordinator] the coordinator state was not written at exit; it restarts from its last write");
                    break;
                }
                Settled::End(end) => return self.end(end),
            }
        }
        // Nothing durable pending (all written): what was decided with no durable change leaves too.
        out.extend(self.core.take_out());
        self.send(out);
        self.down();
        ServerExit::Clean
    }

    /// The thread ends for `end`: fenced (also a conflict it cannot explain — another writer
    /// under its epoch; the process exits [`crate::claims::EXIT_FENCED`]), or a test's kill.
    fn end(&mut self, end: End) -> ServerExit {
        match end {
            End::Fenced => self.fence_out(),
            End::Conflict => {
                self.fenced.set();
                self.down();
                ServerExit::Fenced
            }
            #[cfg(feature = "test-hooks")]
            End::Killed => {
                log::warn!("[coordinator] test kill: the transition committed, nothing it released leaves");
                self.down();
                ServerExit::CoordinatorFailed
            }
        }
    }

    fn fence_out(&mut self) -> ServerExit {
        log::error!("[coordinator] the coordinator's write was refused as fenced: another coordinator took over; stopping");
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
        if let Some(login) = self.links.login.as_mut() {
            drop(login.listener.take());
        }
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use game::HostId;

    /// A host id from its number.
    #[allow(dead_code)]
    fn h(n: u64) -> HostId {
        HostId(n)
    }

    use super::*;
    use game::RequestId;

    /// Ruling 3 of Elastic 1 Task 6: with no login service linked, a `SessionEnded` is held (at
    /// most [`LOGIN_ENDS_HELD_MAX`], the oldest dropped) and an ack dropped (the login service
    /// resends its order or kick); once one is linked, answers go to it at once.
    #[test]
    fn session_ends_wait_for_the_login_service_and_acks_do_not() {
        let (listener, events, senders) =
            LinkListener::bind::<LoginToHost, HostToLogin>("127.0.0.1:0".parse().unwrap(), "t".into(), String::new(), None).unwrap();
        let mut login = LoginSide::new(listener, events, senders);
        let ended = |epoch| HostToLogin::SessionEnded { player_token: "tok".into(), session_epoch: epoch };
        login.tell(HostToLogin::KickAck { player_token: "tok".into(), session_epoch: 1 });
        login.tell(HostToLogin::SpawnAck { player_token: "tok".into(), session_epoch: 1, ok: true, quic: None, webtransport: None, cert_hash: None });
        assert!(login.held.is_empty(), "acks are not held");
        for epoch in 0..=LOGIN_ENDS_HELD_MAX as u64 {
            login.tell(ended(epoch));
        }
        assert_eq!(login.held.len(), LOGIN_ENDS_HELD_MAX, "bounded");
        assert_eq!(login.held.front(), Some(&ended(1)), "the oldest went");
        assert_eq!(login.held.back(), Some(&ended(LOGIN_ENDS_HELD_MAX as u64)));
        assert_eq!(login.tally.counts(), LoginCounts { orders: 0, orders_dropped: 0, ends_dropped: 1 }, "counted, not logged one by one");
        // Reported once per period: the first report takes the count; until the period ends
        // nothing more is said, whatever is dropped.
        login.report(1_000);
        assert_eq!((login.logged, login.logged_ms), (login.tally.counts(), Some(1_000)));
        login.tell(ended(LOGIN_ENDS_HELD_MAX as u64 + 1));
        login.report(1_000 + WORLD_CUT_RETRY_LOG_MS - 1);
        assert_eq!(login.logged.ends_dropped, 1, "not within the period");
        login.report(1_000 + WORLD_CUT_RETRY_LOG_MS);
        assert_eq!(login.logged.ends_dropped, 2, "the next period reports the new drop");
        assert_eq!(login.held.back(), Some(&ended(LOGIN_ENDS_HELD_MAX as u64 + 1)), "the newest is kept");
        // A linked login service (a peer id the senders do not know is simply not reached):
        // nothing more is held.
        login.peers.insert(PeerId(99));
        let held = login.held.len();
        login.tell(ended(9_999_999));
        assert_eq!(login.held.len(), held, "sent, not held");
        drop(login.listener.take());
    }

    fn attach(host: HostId, epoch: u64) -> HostMsg {
        HostMsg::Attach { host, epoch, running: Vec::new(), online: Vec::new(), generation: 0, boot: 1, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback }
    }

    /// The same `VOXEL_OWNERSHIP` word on both sides, whatever the number of hosts (no count).
    #[test]
    fn a_coordinator_takes_only_its_own_crash_policy() {
        use game::CrashPolicy::{Rollback, Succession};
        assert!(policy_agrees(Succession, Succession).is_ok());
        assert!(policy_agrees(Rollback, Rollback).is_ok());
        let forgot = policy_agrees(Rollback, Succession).unwrap_err();
        assert!(forgot.contains("VOXEL_OWNERSHIP=claim") && forgot.contains("VOXEL_OWNERSHIP=static"), "{forgot}");
        assert!(policy_agrees(Succession, Rollback).is_err());
    }

    /// A `hosts` row of registration `reg`, live.
    fn live(reg: u64) -> Option<(u64, i64)> {
        Some((reg, game::HOST_LIVE))
    }

    /// Any ordinal is served, each with one live connection; a hello whose registration is
    /// not its `hosts` row's — or whose row is not live, or missing — is refused (a stale
    /// process), first to connect or not.
    #[test]
    fn two_ordinals_are_served() {
        let mut r = Routes::default();
        assert_eq!(r.admit(PeerId(2), h(4), 1, live(2)), Admit::Refused, "a stale registration is refused, first or not");
        assert_eq!(r.admit(PeerId(3), h(0), 1, live(1)), Admit::Admitted { replaced: None }, "host 0 is served");
        assert_eq!(r.admit(PeerId(4), h(1), 3, live(3)), Admit::Admitted { replaced: None }, "and host 1");
        assert_eq!(r.admit(PeerId(5), h(0), 1, live(1)), Admit::Admitted { replaced: Some(PeerId(3)) }, "host 0 reconnecting");
        assert_eq!(r.admit(PeerId(6), h(7), 1, None), Admit::Refused, "a host with no hosts row");
        assert_eq!(r.admit(PeerId(7), h(8), 2, Some((2, game::HOST_DEAD))), Admit::Refused, "a host declared dead");
        assert_eq!(r.peer_of(h(0)), Some(PeerId(5)));
        assert_eq!(r.peer_of(h(1)), Some(PeerId(4)));
        assert_eq!(r.peer_of(h(4)), None);
        assert_eq!(r.peer_of(h(7)), None);
        assert!(r.refused.contains(&PeerId(2)) && r.refused.contains(&PeerId(3)));
        assert_eq!(r.route(Target::Host(h(0)), &CoordMsg::Attached { epoch: 9 }), Route::Nobody, "an unmatched echo goes nowhere");
    }

    /// The core names the host; the attach echo goes to the connection that attached.
    #[test]
    fn messages_route_to_the_host_they_are_for() {
        let mut r = Routes::default();
        // Host 3 connected twice (the newer peer wins), host 5 once; neither is peer 0.
        r.hosts.insert(PeerId(4), h(3));
        r.hosts.insert(PeerId(7), h(5));
        r.hosts.insert(PeerId(9), h(3));
        r.note(PeerId(9), h(3), &attach(h(3), 1));
        r.note(PeerId(7), h(5), &attach(h(5), 1));
        assert_eq!(r.peer_of(h(3)), Some(PeerId(9)));
        assert_eq!(r.peer_of(h(5)), Some(PeerId(7)));
        assert_eq!(r.peer_of(h(1)), None);
        // Both attached with epoch 1: the echoes go back in the order handled.
        assert_eq!(r.route(Target::Host(h(3)), &CoordMsg::Attached { epoch: 1 }), Route::Peer(PeerId(9)));
        assert_eq!(r.route(Target::Host(h(5)), &CoordMsg::Attached { epoch: 1 }), Route::Peer(PeerId(7)));
        let reply = CoordMsg::Reply { id: RequestId { host: h(5), boot: 1, seq: 1 }, reply: game::WorldReply::PersonInfo(None) };
        assert_eq!(r.route(Target::Host(h(5)), &reply), Route::Host(h(5)));
        let spawn = CoordMsg::SpawnPeople { rc: game::RegionCoords::new(1, 1), incarnation: 1, events: Vec::new() };
        assert_eq!(r.route(Target::Host(h(3)), &spawn), Route::Host(h(3)));
        assert_eq!(r.route(Target::Every, &CoordMsg::Calendar(game::history::CalendarTick(1))), Route::Every);
    }

    /// One live connection per ordinal, admitted by the `hosts` row's registration number:
    /// a stale process is refused even with nobody live (a restarted coordinator has no
    /// memory of who connected); the replaced connection is refused, its pending attach echo
    /// dropped, and only the live one is routed to.
    #[test]
    fn one_live_peer_per_ordinal_by_registration() {
        let mut r = Routes::default();
        assert_eq!(r.admit(PeerId(1), h(0), 5, live(5)), Admit::Admitted { replaced: None });
        r.note(PeerId(1), h(0), &attach(h(0), 3));
        // The ordinal registered again (registration 6): the old process is stale.
        assert_eq!(r.admit(PeerId(2), h(0), 5, live(6)), Admit::Refused);
        assert!(r.refused.contains(&PeerId(2)));
        assert_eq!(r.peer_of(h(0)), Some(PeerId(1)));
        // Its replacement takes over: the old connection is refused and its attach echo
        // goes nowhere.
        assert_eq!(r.admit(PeerId(3), h(0), 6, live(6)), Admit::Admitted { replaced: Some(PeerId(1)) });
        assert!(r.refused.contains(&PeerId(1)) && !r.hosts.contains_key(&PeerId(1)));
        assert_eq!(r.route(Target::Host(h(0)), &CoordMsg::Attached { epoch: 3 }), Route::Nobody);
        r.note(PeerId(3), h(0), &attach(h(0), 1));
        assert_eq!(r.route(Target::Host(h(0)), &CoordMsg::Attached { epoch: 1 }), Route::Peer(PeerId(3)));
        assert_eq!(r.peer_of(h(0)), Some(PeerId(3)));
        // The same registration reconnecting (a dropped link of the live process): the newer wins.
        assert_eq!(r.admit(PeerId(4), h(0), 6, live(6)), Admit::Admitted { replaced: Some(PeerId(3)) });
        // The old connection's Down does not unseat the live one.
        assert_eq!(r.gone(PeerId(3)), None);
        assert_eq!(r.peer_of(h(0)), Some(PeerId(4)));
        assert_eq!(r.gone(PeerId(4)), Some(h(0)));
        assert_eq!(r.peer_of(h(0)), None);
        // With nobody live, a stale registration is still refused; the stored one is admitted
        // — unless its row is no longer live (declared dead).
        assert_eq!(r.admit(PeerId(5), h(0), 5, live(6)), Admit::Refused);
        assert_eq!(r.admit(PeerId(9), h(0), 6, Some((6, game::HOST_DEAD))), Admit::Refused);
        assert_eq!(r.admit(PeerId(6), h(0), 6, live(6)), Admit::Admitted { replaced: None });
        // A refused Attach (an old world generation) closes the live connection.
        r.note(PeerId(6), h(0), &attach(h(0), 2));
        assert_eq!(r.refuse(PeerId(6)), Some(h(0)));
        assert_eq!(r.peer_of(h(0)), None);
        assert_eq!(r.route(Target::Host(h(0)), &CoordMsg::Attached { epoch: 2 }), Route::Nobody);
    }
}
