//! The world coordinator's runtime: the threadless core ([`game::Coordinator`]) on its own
//! `coordinator` thread, persisting its state into the world store — the **only writer** of
//! history state. *Durable before visible* (Elastic 2a): every pass takes the core's transition
//! ([`Coordinator::take_transition`]: its whole projection when anything durable changed, and
//! the store decisions it issues) and writes it in ONE transaction: a compare-and-set on the
//! row's version, the decisions after it, each in a savepoint
//! ([`StoreHandle::write_transition_later`], Elastic 2b Task 3 — the version is the
//! coordinator's only fence); the messages it released are sent only once that write
//! committed, the decisions' answers are applied as inputs at once, and the post-commit effects
//! (a decided cut's staged parts finished, its notice) run after. One write in flight at a time:
//! whatever is decided meanwhile waits for the next transition. A failed write (not known to
//! have committed) hands its transition back to the core (its messages wait; the inputs stay
//! applied once, never again; its decisions are looked up, never issued again) and is retried
//! after [`COORDINATOR_SAVE_RETRY_MS`]. A write refused as a `Conflict` (the row's version moved)
//! re-reads the row: if it holds this process's own unacknowledged write (its commit's answer
//! was lost), the version is taken from it and the cached core — that write plus every input
//! applied since, each once — is written again at once (the conflicting transition did not
//! commit: its decisions go out again with it). Anything else is another writer (Elastic 2b):
//! the core is REBASED onto the row ([`Coordinator::rebase`]: the newer projection, every input
//! logged since the last commit applied again) and written again — up to
//! [`COORDINATOR_CONFLICT_RETRIES`] in a row, then after [`COORDINATOR_SAVE_RETRY_MS`]. Only a
//! core that cannot be rebased without a loss ends the process as fenced, as Elastic 2a did (the
//! next start loads the row): one holding a change made outside its log since its last commit (a
//! store READ's answer: Elastic 2b Task 6 makes those inputs), or one with a failed write since
//! its last commit (that write may be in the row: its inputs would be applied twice). Counted
//! (`mmo_coordinator_*` in `/metrics`, the periodic `[coordinator] window=…` line): transitions
//! written, their bytes, conflicts.
//!
//! **Several replicas** (Elastic 2b). Every replica is a cache of the row: on a `coordinator_row`
//! notice of a newer version (the row listener, `listen_channel`) its cache is refreshed. A host
//! message about a cut the cache cannot know yet asks a refresh
//! ([`Coordinator::take_refresh_wanted`]), never a lookup.
//!
//! **Two ways in and out.** In `AllInOne` the host's `WorldManager` reaches the core over a
//! crossbeam [`ChannelWorldLink`] ([`start_all_in_one`]) and the core runs in
//! [`Delivery::Direct`]: what a transition released is sent on that channel once it committed. As
//! its own process (`VOXEL_ROLE=coordinator`, [`start_listening_as`]) it serves the coordinator's
//! HTTP API ([`crate::coordinator_api`], the hosts' end is [`crate::world_http::HttpWorldLink`])
//! and runs in [`Delivery::Mailbox`]: nothing is sent — every message for a host is a row of its
//! mailbox (the store's `outbox`), written by the transition that decided it, read by the host
//! from any replica. A transition that changed nothing durable but carries rows is written
//! WITHOUT a core (the compare-and-set bumps the version). A host's batch
//! ([`ApiCall::Batch`]) is applied as its messages' inputs, each carrying the batch's
//! `(boot, epoch)` as the link of the `Attach` it was stamped with; its POST is answered once the
//! transition covering those inputs committed (or, nothing durable changed, once the version check
//! passed) with whether the host is still attached as that pair. The inline link
//! ([`game::InlineWorldLink`]) stays for tests and wasm single-player.
//!
//! **The login service** calls the same API ([`ApiCall::LoginOrder`]): each order is queued for
//! the world's gate host of its moment and leaves as a row of its mailbox with the next
//! transition; it is held in the runtime — never logged — until that transition commits (then its
//! POST is answered 202), and issued again (the gate chosen again) after a rebase or a refresh
//! dropped it ([`HeldOrder`]). The hosts' answers ([`HostMsg::Login`], logged inputs) are
//! `login_outbox` rows of the transition holding them, which the login service long-polls from
//! any replica; the ticker prunes those older than [`LOGIN_ANSWER_TTL_MS`].
//!
//! **World cuts** (`claim` hosts, any number of them; R3 Task 13; [`WorldCuts`]): the core picks the tick and
//! collects the hosts' parts; the runtime feeds it the world tick and the hosts the cut
//! needs (those whose `hosts` row is not clean); the core's decision is recorded inside its
//! transition and the core broadcasts the decision the STORE returned. A decision refused
//! there, or whose answer was lost, is looked up (`cut_decision`; an undecided one aborted by
//! `abort_cut`, retried [`WORLD_CUT_LOOKUP_RETRY_MS`]) until a decision is stored: no outcome is
//! ever announced that the store does not hold. A host's question about a cut the core does not
//! remember is answered from the store the same way. Every store call of the world cuts is
//! answered on a receiver: the loop never waits for the database. While the coordinator is down
//! no cut runs.
//!
//! Each decision stored as committed is announced to the hosts' terrain listeners
//! (by its [`AfterCommit::FinishCut`], once the cut's staged parts are committed; Task 19).
//!
//! A core that cannot be rebased onto a newer row ends the process as fenced: the shared
//! [`Fenced`] flag is set — the server's main loop sees it and exits
//! [`crate::claims::EXIT_FENCED`] through `cuts::exit_world` — the link goes down (the API
//! answers 503 `fenced`) and the thread ends with [`ServerExit::Fenced`]. Library code never calls
//! `process::exit`.

use game::HostId;
use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, RecvTimeoutError, Select, Sender, TryRecvError};
use game::coordinator::{BatchAck, Delivery, Gate, Input, Target};
use game::{CoordMsg, Coordinator, HostMsg, WorldLink};
use world_store::{AfterCommit, StoreError, StoreHandle, TransitionWrite, TransitionWritten};

use crate::coordinator_api::{ApiCall, ApiListener, ApiRefusal};

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

/// Conflicts in a row a write rebases through at once (Elastic 2b); past them the next write
/// waits [`COORDINATOR_SAVE_RETRY_MS`], then the count starts again. Never an exit.
pub const COORDINATOR_CONFLICT_RETRIES: u32 = 8;

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
    /// The `(boot, epoch)` its latest `Attach` named (what its link's end detaches:
    /// `Input::HostStalled`).
    attach: Option<(u64, u64)>,
}

/// A connected pair: the host's [`ChannelWorldLink`] (up) and the coordinator's [`HostEnd`].
pub fn channel_link() -> (ChannelWorldLink, HostEnd) {
    let (host_tx, host_rx) = crossbeam::channel::unbounded();
    let (coord_tx, coord_rx) = crossbeam::channel::unbounded();
    let (wake_tx, wake_rx) = crossbeam::channel::bounded(1);
    let up = Arc::new(AtomicBool::new(true));
    (
        ChannelWorldLink { tx: host_tx, rx: coord_rx, up: up.clone(), wake: wake_rx },
        HostEnd { rx: host_rx, tx: coord_tx, up, wake: wake_tx, gone: false, ordinal: HostId::SOLO, attach: None },
    )
}

/// The hosts a coordinator serves: an in-process channel link (`AllInOne`, [`Delivery::Direct`]),
/// or the host processes calling its HTTP API (`VOXEL_ROLE=coordinator`, [`Delivery::Mailbox`]).
enum Hosts {
    /// Every outgoing message goes to each host (one host per process).
    Channels(Vec<HostEnd>),
    Api(ApiHosts),
}

/// The coordinator's HTTP API as its thread serves it ([`crate::coordinator_api`]).
struct ApiHosts {
    /// Dropped when the coordinator stops (its port freed).
    listener: Option<ApiListener>,
    /// The calls of the API's handlers (unbounded: a handler never waits to send).
    calls: Receiver<ApiCall>,
    /// The row versions the replica learnt (each `coordinator_row` notice, each own commit): what
    /// wakes the API's waiting mailbox polls.
    row_version: tokio::sync::watch::Sender<u64>,
    /// The batches applied and not answered yet, in arrival order ([`BatchWaiter`]).
    waiters: VecDeque<BatchWaiter>,
    /// The number of the last batch applied (each batch with messages takes the next).
    applied: u64,
    /// The batches the write in flight covers (those numbered up to it): its commit answers them.
    writing_covers: u64,
    /// The version check of the batches that changed nothing durable (with more than one live
    /// replica): the batches it covers, and the row's version on its way.
    clean_check: Option<(u64, Answer<u64>)>,
    /// The login orders waiting for the transition carrying their row ([`HeldOrder`]), in
    /// arrival order; at most [`LOGIN_ORDERS_HELD_MAX`].
    login_held: Vec<HeldOrder>,
}

/// A host's batch whose inputs were applied, waiting for its answer: once the transition covering
/// them committed (or, nothing durable changed, once the version check passed), whether the host
/// is still attached as `(boot, epoch)` — the pair the batch was stamped with.
struct BatchWaiter {
    /// Its number ([`ApiHosts::applied`]).
    seq: u64,
    host: HostId,
    boot: u64,
    epoch: u64,
    done: tokio::sync::oneshot::Sender<Result<BatchAck, ApiRefusal>>,
}

impl ApiHosts {
    /// Every waiter numbered up to `covers` answered from `core` (whether its host is still
    /// attached as its pair: an input dropped as a superseded attach's leaves it `false`).
    fn answer_through(&mut self, covers: u64, core: &Coordinator) {
        while self.waiters.front().is_some_and(|w| w.seq <= covers) {
            let w = self.waiters.pop_front().expect("checked");
            let attached = attached_as(core, w.host, w.boot, w.epoch);
            let _ = w.done.send(Ok(BatchAck { attached }));
        }
    }

    /// Every waiter and every held login order refused with `why` (the thread ends).
    fn refuse_all(&mut self, why: ApiRefusal) {
        for w in self.waiters.drain(..) {
            let _ = w.done.send(Err(why));
        }
        for held in self.login_held.drain(..) {
            let _ = held.done.send(Err(why));
        }
    }
}

/// Whether `core` holds host `host` attached as `(boot, epoch)`: its newest applied `Attach` is
/// exactly that pair and nothing detached it since.
fn attached_as(core: &Coordinator, host: HostId, boot: u64, epoch: u64) -> bool {
    core.attachment(host) == Some(boot) && core.newest_attach(host) == Some((boot, epoch))
}

/// At most this many login orders wait in a replica for the transition carrying their mailbox
/// row ([`HeldOrder`]); one more is answered 503 `not_ready` at once (the login service answers
/// the join 503, a kick is resent by its sweeper).
pub const LOGIN_ORDERS_HELD_MAX: usize = 4096;

/// How long a login answer (`login_outbox`) is kept: the ticker's transitions prune the rows
/// older than this. A login service that starts reads every answer still kept (its cursor starts
/// at 0): each kind is idempotent there.
pub const LOGIN_ANSWER_TTL_MS: u64 = 600_000;

/// The ticker prunes the login answers with at most one transition per this period.
pub const LOGIN_ANSWER_PRUNE_EVERY_MS: u64 = 60_000;

/// A login order of the API ([`ApiCall::LoginOrder`]) whose row is queued for the gate host of
/// its moment: held in the runtime — never in the core's input log — until the transition
/// carrying that row commits (then `done` is answered `Ok`: the order is durable in the gate's
/// mailbox). A rebase or a refresh drops what the core had queued: the order is issued again,
/// the gate chosen again from the new core (a host takes a repeated epoch as the same order).
struct HeldOrder {
    order: game::LoginOrder,
    done: tokio::sync::oneshot::Sender<Result<(), ApiRefusal>>,
    /// Its row is in the transition being written.
    in_write: bool,
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
        CoordMsg::Assigned { rows } => format!("Assigned {rows:?}"),
        CoordMsg::LandIs { sr, owner, epoch } => format!("LandIs {sr:?}: host {owner} at epoch {epoch}"),
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
        HostMsg::LandNeeded { sr } => format!("LandNeeded {sr:?}"),
    }
}

/// `value`, after a beat of the split coordinator's loop ([`crate::health::Health::loop_beat`]).
/// Wraps the two synchronous store reads of its pass ([`stored_registration`],
/// [`generation_current`]), both called only for the HTTP API (role `coordinator`), at a batch's
/// `Attach`: each may wait up to the pool's acquire timeout (30 s) while the database is away, and
/// several `Attach`es drained in one pass would otherwise add up past `/livez`'s stall limit — a
/// coordinator restarted for a database outage. A read that returned is progress, not a stall.
fn after_store_read<T>(value: T) -> T {
    crate::health::Health::global().loop_beat();
    value
}

/// Host `ordinal`'s `hosts` row as stored — `(reg, state)` — for its `Attach`'s admission;
/// `None` (refused: "wait, never guess" — the host attaches again after its backoff) when it has
/// no row or the store cannot be read. Since Elastic 1 the id is the registration number (issued
/// once, never again), so `reg` is the row's id.
fn stored_registration(store: &StoreHandle, ordinal: HostId) -> Option<(u64, i64)> {
    match store.hosts() {
        Ok(rows) => rows.into_iter().find(|r| r.id == ordinal).map(|r| (r.id.0, r.state)),
        Err(e) => {
            log::error!("[coordinator] host {ordinal}'s hosts row could not be read ({e}); its Attach is refused");
            None
        }
    }
}

/// Whether host `ordinal`'s `Attach` of world `generation` may be applied: not lower than
/// the store's. A lower one is a stale process: `Err(Some(stored))` — it is told the store's
/// generation ([`CoordMsg::Generation`]: it exits for the rollback; nothing else may ever
/// tell it, e.g. while no peer of the newer generation dials it) and ignored from then on.
/// A store that cannot be read refuses it too (`Err(None)`: answered `attached: false`, the
/// host attaches again).
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
    /// [`CoordinatorThread::hold_decision_retries`] is armed.
    #[cfg(feature = "test-hooks")]
    hold_decision_retries: Arc<AtomicBool>,
    /// [`CoordinatorThread::hold_hosts_reads`] is armed.
    #[cfg(feature = "test-hooks")]
    hold_hosts_reads: Arc<AtomicBool>,
    #[cfg(feature = "test-hooks")]
    hooks: WriteHooks,
    /// [`CoordinatorThread::hold_main_loop`] is armed.
    #[cfg(feature = "test-hooks")]
    hold_main_loop: Arc<AtomicBool>,
}

/// The test hooks of the coordinator's writes ([`CoordinatorThread::fail_coordinator_writes`],
/// [`CoordinatorThread::kill_after_commit_before_send`]) and of its store clock
/// ([`CoordinatorThread::step_store_clock`]).
#[cfg(feature = "test-hooks")]
#[derive(Clone, Default)]
struct WriteHooks {
    fail: Arc<AtomicBool>,
    kill_after_commit: Arc<AtomicBool>,
    lose_answer: Arc<AtomicBool>,
    /// [`CoordinatorThread::write_conflict_once`] is armed.
    conflict_once: Arc<AtomicBool>,
    /// The conflicts this coordinator met ([`CoordinatorThread::conflicts`]).
    conflicts: Arc<std::sync::atomic::AtomicU64>,
    /// Ms added to every store-clock sample from now on.
    store_clock_step: Arc<std::sync::atomic::AtomicU64>,
    /// [`CoordinatorThread::hold_write_answers`] is armed.
    hold_answers: Arc<AtomicBool>,
    /// Writes whose answer [`CoordinatorThread::hold_write_answers`] held.
    answers_held: Arc<std::sync::atomic::AtomicU64>,
    /// Passes of the main loop so far ([`CoordinatorThread::passes`]).
    passes: Arc<std::sync::atomic::AtomicU64>,
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

    /// Test hook: right before the coordinator's next write, another writer moves the row (it
    /// writes the row's own projection at the next version): that write meets a `Conflict`
    /// with a row that is not this process's own write.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn write_conflict_once(&self) {
        self.hooks.conflict_once.store(true, Ordering::SeqCst);
    }

    /// Test hook: the conflicts this coordinator's writes met so far (what it adds to
    /// `mmo_coordinator_conflicts_total`, which tests running in parallel share).
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn conflicts(&self) -> u64 {
        self.hooks.conflicts.load(Ordering::SeqCst)
    }

    /// Test hook: the store's clock steps `ms` forward (an NTP step on the database host) —
    /// every store time the coordinator samples from now on is `ms` later (cumulative): its
    /// `now` jumps at the next sample.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn step_store_clock(&self, ms: u64) {
        self.hooks.store_clock_step.fetch_add(ms, Ordering::SeqCst);
    }

    /// Test hook (final review S-I1): while `held`, the answer of every write issued is held back
    /// on its way (a write waiting on the row lock, a database stall): the write stays in flight
    /// until released.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn hold_write_answers(&self, held: bool) {
        self.hooks.hold_answers.store(held, Ordering::SeqCst);
    }

    /// Test hook: the writes whose answer [`Self::hold_write_answers`] held so far.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn answers_held(&self) -> u64 {
        self.hooks.answers_held.load(Ordering::SeqCst)
    }

    /// Test hook: the passes of the coordinator's main loop so far.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn passes(&self) -> u64 {
        self.hooks.passes.load(Ordering::SeqCst)
    }

    /// Test hook: while `held`, the `hosts` read in flight is not answered (one is issued if
    /// none is) — a database that hangs: the lease watch sees nothing until it is released,
    /// and the answer then counts from when that read was issued.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn hold_hosts_reads(&self, held: bool) {
        self.hold_hosts_reads.store(held, Ordering::SeqCst);
    }

    /// Test hook: while `held`, the coordinator's main loop makes no pass — no input handled,
    /// nothing written or sent, no replica beat (a hung process, Elastic 2b Task 4). Released, it
    /// runs on from where it stood.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn hold_main_loop(&self, held: bool) {
        self.hold_main_loop.store(held, Ordering::SeqCst);
    }

    /// Test hook: a decided cut's post-commit finish (its staged parts' `COMMIT PREPARED`) that
    /// failed is not tried again — a coordinator that dies right after the failure.
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

/// A post-commit effect of a committed transition ([`AfterCommit`]), run until it succeeds.
struct AfterJob {
    a: AfterCommit,
    /// The answer awaited (`None`: waiting to try again at `retry_at`).
    rx: Option<Answer<()>>,
    retry_at: u64,
}

/// A question about a cut's outcome, being answered from the store: a host's (`host`), or the
/// running cut's own decision whose answer was lost (`None`, the core's
/// [`Coordinator::take_decision_lookup`]).
struct Lookup {
    host: Option<HostId>,
    cut: game::CutId,
    tick: game::Tick,
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

/// The coordinator for an `AllInOne` process: runs `start` (its row read or bootstrapped at
/// [`CoordinatorStart::version`]) on its thread over a fresh
/// [`ChannelWorldLink`] — the host's end, to install with `WorldManager::set_world_link`.
pub fn start_all_in_one(
    store: StoreHandle,
    start: CoordinatorStart,
    fenced: Fenced,
) -> Result<(ChannelWorldLink, CoordinatorThread), String> {
    log::info!("[coordinator] the world coordinator (row version {}), state {:?}", start.version, start.source);
    let (link, host) = channel_link();
    let thread = CoordinatorRuntime::spawn(store, start, Hosts::Channels(vec![host]), fenced);
    Ok((link, thread))
}

/// A coordinator serving host processes over its HTTP API ([`start_listening_as`]): its thread,
/// and where and with which pin hosts reach it.
pub struct ListeningCoordinator {
    pub thread: CoordinatorThread,
    /// Where its HTTPS API listens ([`Self::url`]).
    pub addr: std::net::SocketAddr,
    /// The API certificate's SHA-256 (also written to the cert-hash file): what hosts and the
    /// login service pin (the registry's `host_link_cert`).
    pub cert_sha256: String,
}

impl ListeningCoordinator {
    /// The URL hosts and the login service reach its API at (`VOXEL_COORDINATOR_ADDR`'s form).
    pub fn url(&self) -> String {
        format!("https://{}", self.addr)
    }
}

/// The coordinator for a `VOXEL_ROLE=coordinator` process (its row read or bootstrapped at
/// [`CoordinatorStart::version`]): serves the coordinator's HTTP API on `bind` to callers
/// presenting `token` and naming world `world` (its identity `<cert_hash_file>.cert` / `.key`,
/// the pin written to `cert_hash_file`) — the hosts and the login service alike — and runs
/// `start` on its thread in [`Delivery::Mailbox`]. With world cuts `cuts` (`claim` hosts); under
/// `CrashPolicy::Succession` (`claim`, any number of hosts) the coordinator declares and runs
/// successions ([`CoordinatorRuntime`]'s succession pump), under `Rollback` it never does.
#[allow(clippy::too_many_arguments)]
pub fn start_listening_as(
    store: StoreHandle,
    start: CoordinatorStart,
    bind: std::net::SocketAddr,
    token: String,
    cert_hash_file: std::path::PathBuf,
    fenced: Fenced,
    cuts: Option<WorldCuts>,
    policy: game::CrashPolicy,
    world: &str,
) -> Result<ListeningCoordinator, String> {
    if let Some(dir) = cert_hash_file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    // Unbounded: the API's handlers `try_send` and never wait on the coordinator's thread.
    let (calls_tx, calls) = crossbeam::channel::unbounded();
    let (row_version, row_version_rx) = tokio::sync::watch::channel(start.version);
    let config = crate::coordinator_api::ApiConfig { bind, token, world: world.to_string(), cert_file: cert_hash_file.clone() };
    let listener = crate::coordinator_api::serve(config, store.clone(), calls_tx, row_version_rx)
        .map_err(|e| format!("the coordinator's HTTP API on {bind}: {e}"))?;
    let (addr, cert_sha256) = (listener.local_addr(), listener.cert_sha256().to_string());
    log::info!(
        "[coordinator] the world coordinator (row version {}), state {:?}; hosts reach https://{addr} (pin {cert_sha256}, in {})",
        start.version,
        start.source,
        cert_hash_file.display()
    );
    let hosts = Hosts::Api(ApiHosts {
        listener: Some(listener),
        calls,
        row_version,
        waiters: VecDeque::new(),
        applied: 0,
        writing_covers: 0,
        clean_check: None,
        login_held: Vec::new(),
    });
    let thread = CoordinatorRuntime::spawn_as(store, start, hosts, fenced, cuts, policy);
    Ok(ListeningCoordinator { thread, addr, cert_sha256 })
}

/// The coordinator core with its store (see the module docs).
pub struct CoordinatorRuntime {
    core: Coordinator,
    store: StoreHandle,
    /// The transition being written and the store's answer on its way (one at a time).
    writing: Option<(game::coordinator::Transition, Receiver<Result<TransitionWritten, StoreError>>)>,
    /// The writes that failed since the last commit (at most [`COORDINATOR_UNACKED_MAX`]), by
    /// their cores' hashes: one of them may have committed with its answer lost. While any is
    /// remembered a rebase could apply that write's inputs twice: none is done.
    unacked: VecDeque<Unacked>,
    /// Conflicts rebased through since the last commit (or the last wait,
    /// [`COORDINATOR_CONFLICT_RETRIES`]).
    conflicts_in_a_row: u32,
    /// The writes' counts since the last `[coordinator] window=…` line.
    tally: WriteTally,
    #[cfg(feature = "test-hooks")]
    hooks: WriteHooks,
    /// The write in flight was taken with [`CoordinatorThread::kill_after_commit_before_send`] armed.
    #[cfg(feature = "test-hooks")]
    kill_at_commit: bool,
    /// The hosts it serves (a channel link, or its HTTP API — the login service's too).
    hosts: Hosts,
    /// How the core delivers to hosts (`Mailbox` with the API, `Direct` with a channel link): set
    /// on the core loaded at the start, kept by every rebase and refresh (`kept` in the core's
    /// `projection!`); checked before every write.
    delivery: Delivery,
    fenced: Fenced,
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
    /// Its first field is the MONOTONIC ms ([`Self::mono_ms`]) the read was issued at: the
    /// lease watch's clock (Elastic 2a final review, S1).
    hosts_rx: Option<(u64, Answer<world_store::HostsRead>)>,
    /// With the `hosts` read in flight (`Delivery::Mailbox`): each host's oldest unread mailbox
    /// row ([`Input::HostsRead`]'s `mailbox_oldest`), on its way, then as answered.
    /// Each element `(host, boot, at_ms, rows)` (`rows`: the host's unread rows, for the gauges).
    mailbox_rx: Option<Answer<Vec<world_store::MailboxHead>>>,
    mailbox_read: Option<Result<Vec<world_store::MailboxHead>, String>>,
    /// Post-commit effects of committed transitions, in flight or waiting to be tried again.
    after: Vec<AfterJob>,
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
    /// The row version whose transition last wrote the history the core holds (a refresh reads
    /// the row's history only when newer).
    history_version: u64,
    /// The replica lease (a split coordinator; `None` in-process).
    replication: Option<Replication>,
    /// The version check in flight ([`Self::pump_version_check`]).
    version_rx: Option<Answer<u64>>,
    /// The row re-read after a version check found it moved.
    refresh_rx: Option<Answer<Option<world_store::StoredCoordinator>>>,
    /// This replica's `ReplicaJoined` committed ([`Self::joined`]).
    joined: bool,
    /// The `coordinator_row` notices (a split coordinator on Postgres; Elastic 2b Task 5).
    notices: Option<Receiver<world_store::Notice>>,
    /// `now_ms` of the last transition that pruned the login answers ([`LOGIN_ANSWER_PRUNE_EVERY_MS`]).
    login_pruned_ms: Option<u64>,
    /// A refresh of the cache is due (a notice of a newer version, a held row, a stale cut).
    refresh_wanted: bool,
    /// The notices may have been missed (`Notice::Resync`, Task 5 review m1): the row's version is
    /// read, and the cache refreshed if the row moved — also on a cache that believes it is alone
    /// (another replica's join may be the notice it missed).
    resync_wanted: bool,
    /// That version read, in flight.
    resync_rx: Option<Answer<u64>>,
    /// The core asked a refresh for a cut it cannot know ([`Coordinator::take_refresh_wanted`])
    /// that no refresh issued since covers; the refresh in flight covers one.
    stale_refresh: bool,
    refresh_for_stale: bool,
    /// Test hook ([`CoordinatorThread::hold_main_loop`]).
    #[cfg(feature = "test-hooks")]
    hold_main_loop: Arc<AtomicBool>,
}

/// The store reads of the core's successions ([`CoordinatorRuntime::pump_successions`]) and
/// the pacing of its hand-outs (the declarations, ends and rollbacks themselves run inside the
/// transitions): each read answered on a receiver, at most one of each kind in flight, a
/// failure tried again after [`WORLD_CUT_DECISION_RETRY_MS`].
#[derive(Default)]
struct SuccessionPump {
    /// No read of the open successions is wanted ([`Coordinator::take_successions_reread`]): the
    /// last one wanted was answered ([`Input::SuccessionsOpen`]), or this replica is not the
    /// ticker.
    open_known: bool,
    /// The read in flight, with the row version this cache stood at when it was issued.
    open_rx: Option<(u64, Answer<Vec<game::Succession>>)>,
    open_retry_at: u64,
    /// No declaration is handed out before this (after a refused one).
    declare_retry_at: u64,
    /// `now_ms` since which every declaration failed (`None`: the last one was answered).
    declare_failing_since: Option<u64>,
    /// The world generation read for a rollback whose answer was lost (the core's
    /// [`Coordinator::take_generation_lookup`]).
    generation_rx: Option<Answer<u64>>,
    generation_retry_at: u64,
    /// The owners read for the super-regions hosts asked for (`LandNeeded`), with
    /// them and their askers.
    land_rx: Option<(Vec<(game::SuperRegion, Vec<HostId>)>, Answer<Vec<(game::SuperRegion, i64, u64)>>)>,
    /// No end of a landless host is handed out before this (after a refused one).
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
    /// Land assignments (`Assigned::Done` answers) this replica applied (Elastic 3).
    assigned: u64,
    /// Fleet decisions that wrote (Elastic 4): hosts added (`HostAdded::Done`), drained
    /// (`Marked` / `RowOnly`), removed (`HostRemoved::Done`).
    added: u64,
    drained: u64,
    removed: u64,
    /// The load rules' decisions that wrote, in answer order (Elastic 4 Task 7): `up` (rule 5,
    /// a host added) or `down` (rule 6, one drained) — the window line's `scale=`.
    scale: Vec<&'static str>,
    /// Coordinator HTTP API: hosts the ticker detached as stalled (`stalled=`); mailbox and login
    /// answer rows the committed transitions inserted (`mailbox_rows=`, `login_rows=`), and how
    /// many of those transitions wrote no core (`core_less=`).
    stalled: u64,
    mailbox_rows: u64,
    login_rows: u64,
    core_less: u64,
    /// Refreshes (`refreshes=`): this cache rebased onto a newer row it did not write, without a
    /// conflict (a notice, a version check) — each a whole core read, decoded and replayed onto;
    /// another replica's core-less version bump costs this one such (the final review's question 8).
    refreshes: u64,
}

impl WriteTally {
    fn new() -> Self {
        WriteTally {
            since: Instant::now(),
            transitions: 0,
            core_bytes: 0,
            history_bytes: 0,
            conflicts: 0,
            failures: 0,
            assigned: 0,
            added: 0,
            drained: 0,
            removed: 0,
            scale: Vec::new(),
            stalled: 0,
            mailbox_rows: 0,
            login_rows: 0,
            core_less: 0,
            refreshes: 0,
        }
    }
}

/// Why the coordinator thread ends from inside a pass.
enum End {
    /// A store call was refused as fenced (none of the coordinator's is any more: kept for a
    /// store that answers so).
    Fenced,
    /// A `Conflict` with another writer that the core cannot be rebased through without a loss
    /// (a failed write since its last commit that may be in the row: its inputs would apply
    /// twice), or a row that is gone or does not load: the next start loads the row.
    Conflict,
    /// Test hook: [`CoordinatorThread::kill_after_commit_before_send`].
    #[cfg(feature = "test-hooks")]
    Killed,
    /// Another replica ended this one (Elastic 2b Task 4): its beat answered "not live", or the
    /// core it rebased onto no longer holds it. It writes nothing more; the process exits fenced.
    Evicted,
}

/// A split coordinator's replica lease (Elastic 2b Task 4, [`CoordinatorRuntime::pump_replica`]).
struct Replication {
    beat_rx: Option<Answer<bool>>,
    /// The monotonic ms of the next beat.
    next_beat_mono: u64,
    /// The replicas read in flight, with the monotonic ms it was issued at.
    read_rx: Option<(u64, Answer<Vec<world_store::ReplicaRow>>)>,
    next_read_mono: u64,
    watch: game::coordinator::ReplicaWatch,
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

/// The error a write refused by the test hook [`CoordinatorThread::fail_coordinator_writes`]
/// carries: it never reached the store (certainly not committed — its decisions go out again).
const HOOK_REFUSED: &str = "test hook: coordinator writes fail (refused before the store)";

/// A write that failed and may have committed ([`CoordinatorRuntime::unacked`]): what its
/// recognition as this process's own lost commit needs (final review S-I3).
struct Unacked {
    /// [`core_hash`] of its core.
    hash: u64,
    /// Its mailbox rows, drops and login answers: in the store already if it committed — the head
    /// of each in the core then (a write handed back puts them first), not applied again.
    shape: game::coordinator::WriteShape,
}

/// Whether `msg` may leave in a transition written WITHOUT a core ([`Delivery::Mailbox`], nothing
/// durable changed). Such a write that fails is never remembered as one that may be in the row
/// (its core is the row's: a lost commit of it cannot be told from another replica's), so a
/// conflict after it rebases and the replay decides its rows again — if it did commit, they are
/// in the mailbox twice. Only messages a host takes twice without harm are decided with nothing
/// durable changed: an answer replayed from the memo or to a query (`Reply`), an answer about land
/// (`LandIs`, an `Assigned` repeated for a row the host already owns), a player's claim or hop as
/// known (`ClaimIs`, `HopIs`), a cut's stored outcome (`CutDone`), a relayed peer count
/// (`CutPeerCount`), a history notice (`Notice`), a relayed login order (`Login`: a repeated epoch
/// is the same order), the generation told a refused `Attach` (`Generation`), and the world's
/// snapshots (`View`, `Calendar`, `Sites`, `SiteMarkers`). Anything else (a `SpawnPeople`, a
/// `Directive`, an `Attached`, a cut's or a succession's order) comes with a durable change.
///
/// A core-less transition may also carry `login_outbox` rows (a host's [`HostMsg::Login`] changes
/// no core): written twice the same way, each kind is idempotent at the login service — a second
/// `SpawnAck` of an epoch finds no join waiting for it (ignored), a `KickAck` deletes the pending
/// kicks up to its epoch (gone already), a `SessionEnded` ends the session of exactly that epoch
/// (ended already; a newer login has a newer epoch).
fn core_less_kind(msg: &CoordMsg) -> bool {
    matches!(
        msg,
        CoordMsg::Reply { .. }
            | CoordMsg::LandIs { .. }
            | CoordMsg::Assigned { .. }
            | CoordMsg::ClaimIs { .. }
            | CoordMsg::HopIs { .. }
            | CoordMsg::CutDone { .. }
            | CoordMsg::CutPeerCount { .. }
            | CoordMsg::Notice { .. }
            | CoordMsg::Login(_)
            | CoordMsg::Generation(_)
            | CoordMsg::View { .. }
            | CoordMsg::Calendar(_)
            | CoordMsg::Sites(_)
            | CoordMsg::SiteMarkers(_)
    )
}

/// Whether store decision `d` may be issued in a transition written WITHOUT a core (final review
/// M3). Such a write that fails is never remembered (above), and a conflict after it rebases: if it
/// did commit, the replay may decide the same decision again, and the store runs it twice. A
/// decision that sets a mark in the core (a cut's `DecideCut` — `run.taken`; its `BeginCut` — the
/// new `cuts.run`; `Declare` — `declaring`; `EndLandless` — `ending`; `Rollback` — `rolling`) or
/// changes it otherwise (`EndReplica` — the live set; re-issued after a failed write only beside
/// a core, `hand_back` clears `committed_raw`) always rides with a core. Only the decisions that
/// touch no durable field are core-less, each idempotent in the store and answered again by a
/// later read or ask:
/// - `AssignLand`: takes only an absent or free row (`WHERE owner = -1`); a second issue finds it
///   owned (`Owned`, the askers told `LandIs` / the assignee `Assigned` again: [`core_less_kind`]);
///   its askers and provisional counts are process fields.
/// - `AddHost { expect_wanted }`: adds only while the wanted rows number `expect_wanted`; a second
///   issue meets the first's row (`NotNeeded`).
/// - `DrainHost`: the row `draining` again, `drain_at_ms = LEAST(…)`: the same mark.
/// - `RemoveHost`: a second finds no row (`Gone`).
///
/// The fleet's decisions keep no durable state (process fields in `projection!`).
fn core_less_decision(d: &game::coordinator::StoreDecision) -> bool {
    use game::coordinator::StoreDecision;
    matches!(d, StoreDecision::AssignLand(_) | StoreDecision::AddHost(_) | StoreDecision::DrainHost(_) | StoreDecision::RemoveHost(_))
}

/// A transition about to be written without a core carries only [`core_less_kind`] messages and
/// [`core_less_decision`] decisions; any other is logged (and stops a debug build: a message could
/// reach its host twice, a decision run twice in the store).
fn check_core_less(t: &game::coordinator::Transition) {
    for o in t.outbox.iter().filter(|o| !core_less_kind(&o.msg)) {
        log::error!(
            "[coordinator] a transition with nothing durable changed carries {} for host {}: it could reach the host twice",
            describe(&o.msg),
            o.host
        );
        debug_assert!(false, "a core-less transition carries {}", describe(&o.msg));
    }
    for d in t.decisions.iter().filter(|d| !core_less_decision(d)) {
        log::error!("[coordinator] a transition with nothing durable changed carries the store decision {d:?}: it could run twice");
        debug_assert!(false, "a core-less transition carries the store decision {d:?}");
    }
}

/// A hash of a written core: what a re-read row is compared with ([`CoordinatorRuntime::unacked`]).
fn core_hash(core: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    core.hash(&mut h);
    h.finish()
}

impl CoordinatorRuntime {
    /// Runs `start`'s coordinator on a new `coordinator` thread serving `hosts` (role `all`'s
    /// channel link: no world cuts, no successions), saving into `store` under its fence. A
    /// migrated or bootstrapped state is written by its first transition.
    fn spawn(store: StoreHandle, start: CoordinatorStart, hosts: Hosts, fenced: Fenced) -> CoordinatorThread {
        Self::spawn_as(store, start, hosts, fenced, None, game::CrashPolicy::Rollback)
    }

    /// [`Self::spawn`] for a world whose hosts' crashes cost `policy`: `Succession` with
    /// world cuts (`claim`, any number of hosts) runs successions; without world cuts (a lone
    /// `static` host) or under `Rollback`, none is ever declared. The lease watch is fed with
    /// every `hosts` read (`feed_cuts`), which runs exactly when world cuts do — so successions
    /// are driven whenever they can be needed. Every `Attach` must name `policy`
    /// ([`policy_agrees`]).
    fn spawn_as(
        store: StoreHandle,
        start: CoordinatorStart,
        hosts: Hosts,
        fenced: Fenced,
        cuts: Option<WorldCuts>,
        policy: game::CrashPolicy,
    ) -> CoordinatorThread {
        let (stop_tx, stop_rx) = crossbeam::channel::bounded(1);
        #[cfg(feature = "test-hooks")]
        let (inspect_tx, inspect_rx) = crossbeam::channel::unbounded();
        #[cfg(feature = "test-hooks")]
        let hold_decision_retries = Arc::new(AtomicBool::new(false));
        #[cfg(feature = "test-hooks")]
        let hold_hosts_reads = Arc::new(AtomicBool::new(false));
        #[cfg(feature = "test-hooks")]
        let hooks = WriteHooks::default();
        #[cfg(feature = "test-hooks")]
        let hold_main_loop = Arc::new(AtomicBool::new(false));
        let (store_ms, started) = start.store_time;
        // A split coordinator beats its replica row and watches the others' (Elastic 2b Task 4).
        let split = matches!(hosts, Hosts::Api(_));
        let replication = split.then(|| Replication {
            beat_rx: None,
            next_beat_mono: 0,
            read_rx: None,
            next_read_mono: 0,
            watch: game::coordinator::ReplicaWatch::default(),
        });
        let history_version = start.history_version;
        let mut clock = game::coordinator::StoreClock::default();
        clock.sample(store_ms, 0);
        // A bootstrapped core is written whole again by its first transition (its encoding
        // bootstrapped the row; its outputs leave with that transition). Its cut ids' high half
        // was set once, at the bootstrap (`world_file::ensure_coordinator_state`).
        let mut core = start.coordinator;
        core.set_version(start.version);
        // The API's hosts read their mailboxes (from any replica): every message for a host is a
        // row, written by the transition that decided it. Role `all`'s channel link gets what a
        // committed transition released. Set on the core loaded here, BEFORE its first input; a
        // rebase and a refresh keep it (only a fresh load starts at `Direct`, and this runtime
        // loads none after this).
        let delivery = if split { Delivery::Mailbox } else { Delivery::Direct };
        core.set_delivery(delivery);
        // Its first input: this replica registered (Elastic 2b Task 4) — live from its first
        // transition on; alone, every replica registered before it is gone.
        let me = core.replica();
        // In-process (role `all`) there is one coordinator process: every replica registered
        // before it is a previous start of it, dead — alone whatever the registration found
        // (nothing beats or watches in-process to drop it later; fix round 1).
        let alone = start.alone || !split;
        core.apply(Input::ReplicaJoined { me, alone }, store_ms);
        if let Some(c) = &cuts {
            core.set_world_cuts(c.every.as_millis() as u64);
        }
        let successions = (cuts.is_some() && policy == game::CrashPolicy::Succession).then(|| {
            core.set_crash_policy(policy);
            // Nothing is declared and no cut begins until the open ones are read.
            core.successions_unknown();
            SuccessionPump::default()
        });
        // A split coordinator hears every other replica's commits (Elastic 2b Task 5; SQLite: a
        // receiver that never says anything).
        let notices = split.then(|| {
            let mode = world_store::NotifyMode::from_env().unwrap_or_else(|e| {
                log::warn!("[coordinator] {e}; listening for row notices");
                world_store::NotifyMode::Listen
            });
            store.listen_channel(world_store::COORDINATOR_ROW_CHANNEL, mode)
        });
        let runtime = CoordinatorRuntime {
            core,
            store,
            writing: None,
            unacked: VecDeque::new(),
            conflicts_in_a_row: 0,
            tally: WriteTally::new(),
            #[cfg(feature = "test-hooks")]
            hooks: hooks.clone(),
            #[cfg(feature = "test-hooks")]
            kill_at_commit: false,
            hosts,
            delivery,
            fenced,
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
            mailbox_rx: None,
            mailbox_read: None,
            after: Vec::new(),
            lookups: Vec::new(),
            warned_ms: None,
            successions,
            policy,
            history_version,
            replication,
            version_rx: None,
            refresh_rx: None,
            joined: false,
            notices,
            login_pruned_ms: None,
            refresh_wanted: false,
            resync_wanted: false,
            resync_rx: None,
            stale_refresh: false,
            refresh_for_stale: false,
            #[cfg(feature = "test-hooks")]
            hold_main_loop: hold_main_loop.clone(),
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
            hold_decision_retries,
            #[cfg(feature = "test-hooks")]
            hold_hosts_reads,
            #[cfg(feature = "test-hooks")]
            hooks,
            #[cfg(feature = "test-hooks")]
            hold_main_loop,
        }
    }

    /// The process's monotonic clock, ms since [`Self::started`].
    fn mono_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// The store answered `store_ms` now: the core's clock is sampled
    /// ([`game::coordinator::StoreClock::sample`]).
    fn sample_store_clock(&mut self, store_ms: u64) {
        #[cfg(feature = "test-hooks")]
        let store_ms = store_ms + self.hooks.store_clock_step.load(Ordering::SeqCst);
        let mono = self.mono_ms();
        self.clock.sample(store_ms, mono);
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
            #[cfg(feature = "test-hooks")]
            self.hooks.passes.fetch_add(1, Ordering::SeqCst);
            // The stop is read before the hosts are drained: whatever a host sent
            // before asking to stop is already queued and is handled below.
            let stopping = match stop.try_recv() {
                Ok(Stop::WithoutSave) => {
                    // Nothing more is handled, written or sent.
                    log::info!("[coordinator] stopped without writing its last transition");
                    self.down(ApiRefusal::NotReady);
                    return ServerExit::Clean;
                }
                Ok(Stop::Save(timeout)) => Some(timeout),
                Err(TryRecvError::Empty) => None,
                // The handle is gone (its owner unwound): stop as an orderly exit would.
                Err(TryRecvError::Disconnected) => Some(crate::EXIT_SAVE_TIMEOUT),
            };
            #[cfg(feature = "test-hooks")]
            if self.hold_main_loop.load(Ordering::SeqCst) && stopping.is_none() {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            let now = self.now_ms();
            let health = crate::health::Health::global();
            // The split coordinator's loop (role `coordinator`); in-process (role `all`) the
            // host's main loop beats.
            if matches!(self.hosts, Hosts::Api(_)) {
                health.loop_beat();
            }
            // The replica's lease first (a beat answering "not live": ended — nothing more).
            if let Err(end) = self.pump_replica(now) {
                return self.end(end);
            }
            self.drain_hosts(now);
            self.drain_api(now);
            #[cfg(feature = "test-hooks")]
            for f in inspect.try_iter() {
                f(&mut self.core);
            }
            self.feed_cuts(now);
            self.core.apply(Input::Maintain, now);
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
            if self.core.evicted() {
                // The row it rebased onto ended it: what it decided never leaves.
                return self.end(End::Evicted);
            }
            self.report_writes();
            if matches!(self.hosts, Hosts::Api(_)) {
                let live = self.core.replicas_live();
                health.coordinator_replicas(live.len() as u64, self.core.ticker() == Some(self.core.replica()));
            }
            health.world_generation(self.core.world_generation());
            health.successions_open(self.core.successions_open_count() as u64);
            self.send(out);
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
        match &self.hosts {
            Hosts::Channels(hosts) => {
                for h in hosts.iter().filter(|h| !h.gone) {
                    sel.recv(&h.rx);
                }
            }
            Hosts::Api(api) => {
                sel.recv(&api.calls);
                if let Some((_, rx)) = &api.clean_check {
                    sel.recv(rx);
                }
            }
        }
        if let Some((_, written)) = &self.writing {
            sel.recv(written);
        }
        if let Some(notices) = &self.notices {
            sel.recv(notices);
        }
        let _ = sel.ready_timeout(Duration::from_millis(COORDINATOR_TICK_MS));
    }

    /// Hand every queued message of a channel link's host to the core, in its order (role `all`).
    fn drain_hosts(&mut self, now: u64) {
        let Hosts::Channels(hosts) = &mut self.hosts else { return };
        for host in hosts.iter_mut() {
            while !host.gone {
                match host.rx.try_recv() {
                    Ok(msg) => {
                        if let HostMsg::Attach { host: ordinal, boot, epoch, .. } = &msg {
                            host.ordinal = *ordinal;
                            host.attach = Some((*boot, *epoch));
                        }
                        self.core.apply(Input::Host { host: host.ordinal, msg }, now)
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        log::info!("[coordinator] a host closed its link");
                        host.gone = true;
                        if let Some((boot, epoch)) = host.attach {
                            self.core.apply(Input::HostStalled { host: host.ordinal, boot, epoch }, now);
                        }
                    }
                }
            }
        }
    }

    /// The API's calls (role `coordinator`), in arrival order: each host's batch applied — every
    /// message an [`Input::Host`] carrying the batch's `(boot, epoch)` as the link of the `Attach`
    /// it was stamped with (as the QUIC link's messages carried the `Attach` their link opened
    /// with: the superseded-attach rule drops an older link's at apply and at replay) — and its
    /// answer kept until the transition covering it committed ([`BatchWaiter`]). An EMPTY batch
    /// (the host's idle post, or its probe after a drop) applies nothing: answered `attached: true`
    /// at once when the core holds the host attached as that pair (and `false` at once with one
    /// live replica); otherwise it waits like a batch that changed nothing durable, so that a
    /// `false` comes only from a cache confirmed current (final review M1). A batch opening with an `Attach` is
    /// admitted first ([`Self::admit_attach`]); one refused is answered `attached: false` at once
    /// and nothing of it applied. A batch of an attachment the core holds DETACHED (a stalled
    /// mailbox) is applied as any other: the core drops its messages (`superseded`, at apply and at
    /// replay) and its answer says `attached: false` — the host attaches again.
    fn drain_api(&mut self, now: u64) {
        let Hosts::Api(api) = &self.hosts else { return };
        let calls: Vec<ApiCall> = api.calls.try_iter().collect();
        for call in calls {
            match call {
                ApiCall::Batch { host, batch, done } => {
                    let game::coordinator::HostBatch { boot, epoch, msgs } = batch;
                    if msgs.is_empty() {
                        // `attached: true` at once: the core holds that very pair. `false` only
                        // from a current cache (final review M1): with more than one live replica
                        // this cache may not have seen the host's `Attach` committed elsewhere, and
                        // a `false` makes a healthy host re-attach (aborting a running cut) — so it
                        // waits as a batch changing nothing durable does (`pump_clean_batches`: the
                        // row's version read, a refresh first if it moved; or the covering commit).
                        let attached = attached_as(&self.core, host, boot, epoch);
                        if attached || self.core.replicas_live().len() <= 1 {
                            let _ = done.send(Ok(BatchAck { attached }));
                        } else {
                            self.apply_batch(host, boot, epoch, msgs, done, now);
                        }
                        continue;
                    }
                    match msgs.first() {
                        Some(HostMsg::Attach { boot: b, epoch: e, generation, policy, .. }) => {
                            if (*b, *e) != (boot, epoch) {
                                log::warn!(
                                    "[coordinator] host {host}'s batch is stamped ({boot:#x}, {epoch}), its Attach names ({b:#x}, {e}): taken as the Attach's"
                                );
                            }
                            let (b, e) = (*b, *e);
                            if !self.admit_attach(host, b, e, *generation, *policy, now) {
                                let _ = done.send(Ok(BatchAck { attached: false }));
                                continue;
                            }
                            self.apply_batch(host, b, e, msgs, done, now);
                        }
                        _ => {
                            // A batch of an attachment the core holds detached is dropped by the
                            // core itself (its superseded rule, at apply and at replay) and
                            // answered `attached: false` with the others. Its messages come on the link that `Attach` opened, wherever it was
                            // applied (another replica, or an earlier pass here).
                            self.core.link_opened(host, boot, epoch);
                            self.apply_batch(host, boot, epoch, msgs, done, now);
                        }
                    }
                }
                ApiCall::LoginOrder { order, done } => {
                    let Hosts::Api(api) = &self.hosts else { return };
                    if api.login_held.len() >= LOGIN_ORDERS_HELD_MAX {
                        log::warn!("[coordinator] {LOGIN_ORDERS_HELD_MAX} login orders wait for their transition: one more answered not_ready");
                        let _ = done.send(Err(ApiRefusal::NotReady));
                        continue;
                    }
                    self.issue_login_order(HeldOrder { order, done, in_write: false }, now);
                }
            }
        }
    }

    /// Host `host`'s batch `msgs`, stamped `(boot, epoch)`: each message applied in order, the
    /// batch's answer waiting for the transition that covers them.
    fn apply_batch(
        &mut self,
        host: HostId,
        boot: u64,
        epoch: u64,
        msgs: Vec<HostMsg>,
        done: tokio::sync::oneshot::Sender<Result<BatchAck, ApiRefusal>>,
        now: u64,
    ) {
        for msg in msgs {
            self.core.apply(Input::Host { host, msg }, now);
        }
        let Hosts::Api(api) = &mut self.hosts else { return };
        api.applied += 1;
        api.waiters.push_back(BatchWaiter { seq: api.applied, host, boot, epoch, done });
    }

    /// Whether host `host`'s `Attach` of `(boot, epoch)`, world generation `generation`, crash
    /// policy `policy`, may be applied — the checks the QUIC link made at its hello and its
    /// `Attach`: its `hosts` row is live (else a stale process: another boot registered since, or
    /// it is being succeeded; read from the store, so it holds across a coordinator restart), its
    /// generation is not older than the store's (else it is told the store's,
    /// [`CoordMsg::Generation`], as a row of its mailbox at that boot: it exits for the rollback),
    /// and its policy is this coordinator's ([`policy_agrees`]). Refused, the attachment it held
    /// as that boot, if any, is detached (as its link's close detached it) and the reason logged.
    fn admit_attach(&mut self, host: HostId, boot: u64, epoch: u64, generation: u64, policy: game::CrashPolicy, now: u64) -> bool {
        let refused = match after_store_read(stored_registration(&self.store, host)) {
            Some((_, state)) if state == game::HOST_LIVE => None,
            stored => {
                log::error!(
                    "[coordinator] refusing host {host}'s Attach {epoch} (boot {boot:#x}): its hosts row is {stored:?} \
                     ((id, state); a stale host process, or its row unreadable)"
                );
                Some(())
            }
        };
        let refused = refused.or_else(|| match after_store_read(generation_current(&self.store, host, generation)) {
            Ok(()) => None,
            Err(stored) => {
                if let Some(stored) = stored {
                    self.core.tell_refused_attach(host, boot, CoordMsg::Generation(stored));
                }
                Some(())
            }
        });
        let refused = refused.or_else(|| match policy_agrees(self.policy, policy) {
            Ok(()) => None,
            Err(why) => {
                log::error!("[coordinator] host {host}'s Attach is refused: {why}");
                Some(())
            }
        });
        if refused.is_none() {
            return true;
        }
        // Only an `Attach` at least as new as the newest applied detaches it: a late POST of an
        // older one must not end the host's current attachment.
        if let Some((b, e)) = self.core.newest_attach(host).filter(|(b, e)| *b == boot && epoch >= *e) {
            self.core.apply(Input::HostStalled { host, boot: b, epoch: e }, now);
        }
        false
    }

    /// A login order of the API, for the world's gate host of this moment
    /// ([`Coordinator::relay_login_order`]: a row of its mailbox with the next transition), HELD
    /// until that transition commits ([`HeldOrder`]). With no gate host anywhere it is answered at
    /// once: a spawn, with fleet dials set, `Starting` (the join answers 503 `world_starting`, the
    /// client waits) and this replica asks for a host ([`Input::StartWanted`], logged: written with
    /// its next transition); otherwise `NoGate` (the join answers 503; a kick is resent by the
    /// login service's sweeper).
    fn issue_login_order(&mut self, held: HeldOrder, now: u64) {
        let spawn = matches!(held.order, game::LoginOrder::Spawn { .. });
        match self.core.relay_login_order(held.order.clone()) {
            Some(_gate) => {
                if let Hosts::Api(api) = &mut self.hosts {
                    api.login_held.push(HeldOrder { in_write: false, ..held });
                }
            }
            None if spawn && self.core.fleet_dials().is_some() && self.core.login_gate() == Gate::Nobody => {
                let _ = held.done.send(Err(ApiRefusal::Starting));
                self.core.apply(Input::StartWanted, now);
            }
            None => {
                log::debug!("[coordinator] a login order with no host taking logins: answered no_gate");
                let _ = held.done.send(Err(ApiRefusal::NoGate));
            }
        }
    }

    /// The core this replica holds was rebased or refreshed: what it had queued is gone, the
    /// held login orders' rows with it (those of the dropped transition too). Each is issued again
    /// for the gate host of the new core's moment ([`Self::issue_login_order`]).
    fn reissue_login_orders(&mut self, now: u64) {
        let Hosts::Api(api) = &mut self.hosts else { return };
        let held = std::mem::take(&mut api.login_held);
        if !held.is_empty() {
            log::debug!("[coordinator] {} login order(s) issued again after a rebase", held.len());
        }
        for h in held {
            self.issue_login_order(h, now);
        }
    }

    /// The transition just taken carries every held order's row (each was queued before it was
    /// taken: `take_transition` routes the whole queue).
    fn login_orders_in_write(&mut self) {
        if let Hosts::Api(api) = &mut self.hosts {
            for h in &mut api.login_held {
                h.in_write = true;
            }
        }
    }

    /// The transition being written committed: the held orders it carried are answered `Ok` (their
    /// rows are in the gate's mailbox).
    fn login_orders_committed(&mut self) {
        if let Hosts::Api(api) = &mut self.hosts {
            for h in std::mem::take(&mut api.login_held) {
                if h.in_write {
                    let _ = h.done.send(Ok(()));
                } else {
                    api.login_held.push(h);
                }
            }
        }
    }

    /// The transition being written was handed back to the core (a failed or refused write, our
    /// own lost commit met by a conflict): its rows go with the next transition, the held orders'
    /// too.
    fn login_orders_handed_back(&mut self) {
        if let Hosts::Api(api) = &mut self.hosts {
            for h in &mut api.login_held {
                h.in_write = false;
            }
        }
    }

    /// The world tick, and (every [`WORLD_CUT_HOSTS_REFRESH_MS`]) the ticker's read of the
    /// `hosts` rows ([`Input::HostsRead`]: the hosts a cut needs — those whose row is not clean
    /// and live —, the lease watch [`game::coordinator::LeaseWatch`], the drain marks, the ids
    /// pruned). A read that fails stops cuts until a good one
    /// ([`Coordinator::expected_hosts_unknown`]) and makes the watch forget what it saw. A
    /// replica that is not the ticker reads nothing (Elastic 2b Task 6): a read in flight is
    /// dropped, and a new ticker's watch counts from zero.
    ///
    /// Elastic 4: the read also carries the `desired_hosts` rows (the fleet's rules' table), and
    /// runs whenever fleet dials are set — under `static` too, which has no world cuts (its world
    /// tick is then not the core's to know: 0, which nothing reads without cuts).
    ///
    /// Coordinator HTTP API (`Delivery::Mailbox`, whatever the ownership): the read also carries
    /// each host's oldest unread mailbox row (`mailbox_oldest`, read beside it — the core detaches
    /// a host whose row waited past `MAILBOX_STALL_MS`, a dead process's attachment never kept);
    /// the read waits for both answers. A failed mailbox read is passed as none (no stall seen
    /// this time), warned about.
    fn feed_cuts(&mut self, now: u64) {
        let fleet = self.core.fleet_dials().is_some();
        let mailbox = self.delivery == Delivery::Mailbox;
        let world_tick = match &self.cuts {
            Some(cuts) => {
                let world_tick = (cuts.tick)();
                self.core.set_world_tick(world_tick);
                world_tick
            }
            None if fleet || mailbox => 0,
            None => return,
        };
        if !self.core.is_ticker() {
            self.hosts_rx = None;
            self.mailbox_rx = None;
            self.mailbox_read = None;
            self.hosts_read_ms = None;
            if fleet {
                // Only the ticker reads the table: another replica's gauges say nothing.
                crate::health::Health::global().fleet_rows(0, 0);
            }
            if mailbox {
                crate::health::Health::global().mailbox_seen(0, 0);
            }
            return;
        }
        #[cfg(feature = "test-hooks")]
        if self.hold_hosts_reads.load(Ordering::SeqCst) && self.hosts_rx.is_some() {
            return;
        }
        if let Some(rx) = &self.mailbox_rx {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                Ok(answer) => {
                    self.mailbox_read = Some(answer.map_err(|e| e.to_string()));
                    self.mailbox_rx = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.mailbox_read = Some(Err("the world-store thread has stopped".into()));
                    self.mailbox_rx = None;
                }
            }
        }
        if self.mailbox_rx.is_some() {
            // The `hosts` read waits for the mailbox read issued with it.
            return;
        }
        if let Some((issued_mono, rx)) = &self.hosts_rx {
            let issued_mono = *issued_mono;
            match rx.try_recv() {
                Err(TryRecvError::Empty) => return,
                Ok(Ok(world_store::HostsRead { rows, store_ms, next_host_id, desired })) => {
                    // The lease watch's receipt stamp: the monotonic clock, taken before the
                    // sample below (a forward step of the store's clock never reaches the watch).
                    let received_mono = self.mono_ms();
                    // The store's clock, taken as of this receipt: it lags the store by the
                    // answer's way back, never leads it (a correction for the round trip would
                    // count a stalled read's queueing as time, a forward step `StoreClock` keeps
                    // and the lease watch's spans would grow by). The core's `now` follows from
                    // the next pass.
                    self.sample_store_clock(store_ms);
                    let live = rows.iter().filter(|r| !r.clean && r.state == world_store::HOST_LIVE).count();
                    crate::health::Health::global().hosts_live(live as u64);
                    let seen: Vec<game::coordinator::HostSeen> = rows
                        .iter()
                        .map(|r| game::coordinator::HostSeen {
                            ordinal: r.id,
                            name: r.name.clone(),
                            boot: r.boot,
                            clean: r.clean,
                            state: r.state,
                            seen_ms: r.seen_ms,
                            draining: r.drain_in_ms.is_some(),
                            load: r.load,
                            landed: r.owns_land,
                        })
                        .collect();
                    // Issue-to-receive stamps: an answer that waited in the channel through a
                    // stall of this thread never stretches the span (`LeaseWatch::observe`). On
                    // the MONOTONIC clock (Elastic 2a final review, S1): a forward step of the
                    // store's clock between two reads never stretches it either; the store's
                    // own lapse check at the declaration stays the second gate. A host that
                    // left and registered again (another boot) is expected again; a row marked
                    // dead runs nothing (`decide_in` reads the same predicate).
                    if fleet {
                        let draining = desired.iter().filter(|d| d.draining).count() as u64;
                        crate::health::Health::global().fleet_rows(desired.len() as u64 - draining, draining);
                    }
                    // Under `Delivery::Direct` there is no mailbox (none read).
                    let mailbox_oldest: Vec<(HostId, u64, u64)> = match self.mailbox_read.take() {
                        Some(Ok(oldest)) => {
                            let rows = oldest.iter().map(|&(_, _, _, n)| n).sum();
                            let first = oldest.iter().map(|&(_, _, at, _)| at).min();
                            crate::health::Health::global().mailbox_seen(rows, first.map_or(0, |at| now.saturating_sub(at)));
                            oldest.into_iter().map(|(h, b, at, _)| (h, b, at)).collect()
                        }
                        Some(Err(e)) => {
                            self.warn_cuts(now, &format!("the hosts' mailboxes could not be read ({e}); no stalled host seen this time"));
                            Vec::new()
                        }
                        None => Vec::new(),
                    };
                    // The hosts the read may find stalled: attached as the boot of a waiting row.
                    // Only the stall rule detaches while a `hosts` read is applied, so those of
                    // them detached after it are the stalled ones (`stalled=`, the counter).
                    let stall_candidates: Vec<(HostId, u64)> =
                        mailbox_oldest.iter().filter(|&&(h, b, _)| self.core.attachment(h) == Some(b)).map(|&(h, b, _)| (h, b)).collect();
                    let read = Input::HostsRead {
                        rows: seen,
                        desired,
                        mailbox_oldest,
                        next_host_id,
                        issued_mono,
                        received_mono,
                        world_tick,
                    };
                    self.core.apply(read, now);
                    let stalled = stall_candidates.iter().filter(|&&(h, b)| self.core.attachment(h) != Some(b)).count() as u64;
                    if stalled > 0 {
                        self.tally.stalled += stalled;
                        crate::health::Health::global().hosts_stalled(stalled);
                    }
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
            self.mailbox_read = None;
        }
        if self.hosts_read_ms.is_none_or(|at| now >= at + WORLD_CUT_HOSTS_REFRESH_MS) {
            self.hosts_read_ms = Some(now);
            self.hosts_rx = Some((self.mono_ms(), self.store.hosts_later()));
            self.mailbox_read = None;
            self.mailbox_rx = mailbox.then(|| self.store.mailbox_oldest_later());
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

    /// The world cuts' store work outside the transitions: the core's decision is handed out
    /// (it leaves with the next transition and is recorded in its transaction); a decision whose
    /// answer was lost or refused, and the hosts' questions about cuts the core does not
    /// remember, are looked up; the post-commit effects of committed decisions run (retried
    /// until they succeed). Never waits. `Err`: fenced.
    fn pump_cuts(&mut self, now: u64) -> Result<(), ()> {
        if self.cuts.is_none() {
            return Ok(());
        }
        // `settles`: the open successions this cut settles if it commits — the core names them
        // only once every survivor applied and the heir ADOPTED (the heir's part then carries
        // `WorldCut::adopts`, Task 9); the store does not check that pairing.
        if let Some(d) = self.core.take_decision() {
            log::debug!("[coordinator] world cut {}: its decision ({}) goes with the next transition", d.cut.0, d.commit);
        }
        self.pump_after_commit(now);
        if let Some((cut, tick)) = self.core.take_decision_lookup() {
            if !self.lookups.iter().any(|l| l.host.is_none() && l.cut == cut) {
                let rx = Some(self.store.cut_decision_later(cut));
                self.lookups.push(Lookup { host: None, cut, tick, rx, abort_rx: None, retry_at: now });
            }
        }
        for (host, cut) in self.core.take_cut_lookups() {
            if !self.lookups.iter().any(|l| l.host == Some(host) && l.cut == cut) {
                let rx = Some(self.store.cut_decision_later(cut));
                self.lookups.push(Lookup { host: Some(host), cut, tick: 0, rx, abort_rx: None, retry_at: now });
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
                        l.abort_rx = Some(self.store.abort_cut_later(l.cut, l.tick));
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
            let (cut, host) = (l.cut.0, l.host.map_or_else(|| "the coordinator".to_string(), |h| format!("host {h}")));
            self.warn_cuts(now, &format!("the outcome of cut {cut} for {host} could not be read ({e}); asking again"));
        }
        for (i, stored) in answered.iter().rev() {
            let l = self.lookups.remove(*i);
            if l.host.is_none() {
                // The running cut's own decision, as stored: its post-commit effects may not
                // have run (the answer that carried them was lost) — run again, idempotent.
                self.cut_decided(l.cut, l.tick, *stored, &[]);
                // A committed one is announced by the finish, once its parts are committed.
                self.after.push(AfterJob { a: AfterCommit::FinishCut { cut: l.cut, tick: l.tick, committed: *stored }, rx: None, retry_at: now });
            }
            // A recorded decision never changes: a logged input (Elastic 2b Task 6).
            self.core.apply(Input::CutLookedUp { host: l.host, cut: l.cut, committed: *stored }, now);
        }
        Ok(())
    }

    /// A decision on cut `cut` stored as `committed` reached this runtime (with its transition,
    /// or by a lookup): counted. `settles`: the successions its commit settles (when known).
    fn cut_decided(&mut self, cut: game::CutId, tick: game::Tick, committed: bool, settles: &[game::SuccessionId]) {
        game::perf_event!("world_cut_decided", "cut={} tick={tick} committed={committed}", cut.0);
        let health = crate::health::Health::global();
        health.cut_decided(committed);
        if committed {
            for _ in settles {
                health.succession_settled();
            }
        }
    }

    /// The post-commit effects ([`AfterCommit`]): each run until it succeeds, a failure tried
    /// again after [`WORLD_CUT_DECISION_RETRY_MS`] (never, under the test hook
    /// [`CoordinatorThread::hold_decision_retries`]). One that never runs is left to the
    /// lookups: a host's `cut_decided`, a start's `resolve_prepared`, the listeners' poll.
    fn pump_after_commit(&mut self, now: u64) {
        #[cfg(feature = "test-hooks")]
        let held = self.hold_decision_retries.load(Ordering::SeqCst);
        #[cfg(not(feature = "test-hooks"))]
        let held = false;
        let mut failed = Vec::new();
        self.after.retain_mut(|job| {
            if let Some(rx) = &job.rx {
                let answer = match rx.try_recv() {
                    Err(TryRecvError::Empty) => return true,
                    Ok(answer) => answer,
                    Err(TryRecvError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
                };
                job.rx = None;
                match answer {
                    Ok(()) => return false,
                    Err(e) => {
                        failed.push(format!("{:?}: {e}", job.a));
                        if held {
                            return false;
                        }
                        job.retry_at = now + WORLD_CUT_DECISION_RETRY_MS;
                        return true;
                    }
                }
            }
            true
        });
        for e in failed {
            self.warn_cuts(now, &format!("a decided cut's post-commit effect failed ({e}); trying again"));
        }
        for job in self.after.iter_mut().filter(|j| j.rx.is_none() && now >= j.retry_at) {
            job.rx = Some(self.store.after_commit_later(job.a));
        }
    }

    /// The successions' store calls (`CrashPolicy::Succession`; the core decides, see
    /// `game::coordinator::succession`): first the open successions are read (a restart
    /// re-sends their orders) and the owners of land asked for; a rollback whose answer was lost
    /// is looked up (the world generation); then a wanted rollback is handed out, else the core's
    /// next end of a landless host and next declaration — each runs in the next transition, its
    /// answer comes with the commit ([`Self::answered`]). Never waits; a read that fails, or a
    /// decision refused (a lock timeout behind a staged part, the store down), is tried again
    /// after [`WORLD_CUT_DECISION_RETRY_MS`]. `Err`: fenced.
    fn pump_successions(&mut self, now: u64) -> Result<(), ()> {
        let Some(p) = self.successions.as_mut() else { return Ok(()) };
        // The ticker's work (Elastic 2b Task 6): another replica reads, looks up and hands out
        // nothing (its core refuses the answers anyway); a new ticker owes the read below.
        let ticker = self.core.is_ticker();
        // The core wants the open successions read (again): it owes one (a start, a rebase, a
        // new ticker) or the store changed them behind its back (an offline `worldctl
        // retire-host`, a `worldctl rollback`) — a read in flight is dropped: it may predate the
        // change.
        if self.core.take_successions_reread() {
            p.open_known = false;
            p.open_rx = None;
        }
        if !ticker {
            p.open_known = true;
            p.open_rx = None;
            return Ok(());
        }
        if !p.open_known {
            if p.open_rx.is_none() && now >= p.open_retry_at {
                // Issued at this cache's version: a run opened later is no abandoned one (H5).
                p.open_rx = Some((self.core.version(), self.store.open_successions_later()));
            }
            let answer = p.open_rx.as_ref().map(|(at, rx)| (*at, rx.try_recv()));
            let failed = match answer {
                None | Some((_, Err(TryRecvError::Empty))) => None,
                Some((issued_at_version, Ok(Ok(list)))) => {
                    p.open_known = true;
                    p.open_rx = None;
                    log::info!("[coordinator] {} open succession(s) in the store", list.len());
                    self.core.apply(Input::SuccessionsOpen { list, issued_at_version }, now);
                    None
                }
                Some((_, Ok(Err(e)))) => Some(e.to_string()),
                Some((_, Err(TryRecvError::Disconnected))) => Some("the world-store thread has stopped".into()),
            };
            if let Some(e) = failed {
                p.open_rx = None;
                p.open_retry_at = now + WORLD_CUT_DECISION_RETRY_MS;
                self.warn_cuts(now, &format!("the open successions could not be read ({e}); no cut and no succession until they are"));
            }
            return Ok(());
        }
        // Land asked for (`LandNeeded`): the owners read (a plain read), the rows of
        // the asked super-regions told to the core (Elastic 3: it assigns a free one, names a live
        // owner, and wants a departed owner's land given away — a `Leave` succession between
        // cuts, once a cut later than its leave cut committed). A failed read is dropped: the
        // hosts ask again every second.
        let mut failed = None;
        if let Some((asked, rx)) = &mut p.land_rx {
            let answer = match rx.try_recv() {
                Err(TryRecvError::Empty) => None,
                Ok(answer) => Some(answer),
                Err(TryRecvError::Disconnected) => Some(Err(StoreError::Backend("the world-store thread has stopped".into()))),
            };
            if let Some(answer) = answer {
                match answer {
                    Ok(rows) => {
                        let srs: BTreeSet<game::SuperRegion> = asked.iter().map(|(sr, _)| *sr).collect();
                        let rows: Vec<(game::SuperRegion, i64, u64)> = rows.into_iter().filter(|(sr, _, _)| srs.contains(sr)).collect();
                        self.core.apply(Input::LandOwners { asked: std::mem::take(asked), rows }, now);
                    }
                    Err(e) => failed = Some(e),
                }
                p.land_rx = None;
            }
        }
        if p.land_rx.is_none() {
            let asked = self.core.take_land_lookups();
            if !asked.is_empty() {
                p.land_rx = Some((asked, self.store.owners_later()));
            }
        }
        if let Some(e) = failed {
            // Throttled; the rest of the pump runs on as usual.
            self.warn_cuts(now, &format!("the owners of land asked for could not be read ({e}); the hosts ask again"));
        }
        let Some(p) = self.successions.as_mut() else { return Ok(()) };
        // A rollback whose answer was lost: the world generation is read (the rollback is
        // conditional on its generation: the core hands it out again only if it did not commit).
        if let Some(rx) = &p.generation_rx {
            let answer = match rx.try_recv() {
                Err(TryRecvError::Empty) => return Ok(()),
                Ok(answer) => answer,
                Err(TryRecvError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
            };
            p.generation_rx = None;
            match answer {
                Ok(generation) => self.core.generation_looked_up(generation),
                Err(e) => {
                    p.generation_retry_at = now + WORLD_CUT_DECISION_RETRY_MS;
                    // Asked again: the core's lookup is taken once.
                    self.core.generation_lookup_failed();
                    self.warn_cuts(now, &format!("the world generation could not be read ({e}); asking again"));
                }
            }
            return Ok(());
        }
        if now >= p.generation_retry_at {
            if let Some(from) = self.core.take_generation_lookup() {
                log::info!("[coordinator] a rollback from generation {from} lost its answer: reading the world generation");
                p.generation_rx = Some(self.store.world_generation_later());
                return Ok(());
            }
        }
        if self.core.rollback_wanted() {
            // Handed out (paced by the core after a refused one); it runs in the next transition.
            if let Some(from) = self.core.take_rollback() {
                log::warn!("[coordinator] the world rolls back from generation {from} with the next transition");
            }
            return Ok(());
        }
        // A lapsed landless host is ended (Elastic 2a), beside a declaration (the store
        // serialises the two on the world row) — never a succession, never a rollback. Both are
        // handed out here and run in the next transition; their answers come with its commit
        // ([`Self::answered`]). The core holds one of each in flight at most.
        if now >= p.end_retry_at {
            if let Some(end) = self.core.take_end_landless(now) {
                log::debug!("[coordinator] host {} is ended with the next transition", end.host);
            }
        }
        let Some(p) = self.successions.as_mut() else { return Ok(()) };
        if now >= p.declare_retry_at {
            if let Some(d) = self.core.declare_wanted(now) {
                log::debug!("[coordinator] hosts {:?} are declared with the next transition", d.dead);
            }
        }
        Ok(())
    }

    /// The answers of a committed transition's store decisions, as the runtime takes them
    /// before the core applies them (inputs): counted, a refused declaration or end paced.
    fn answered(&mut self, answers: &[game::coordinator::DecisionAnswer], now: u64) {
        use game::coordinator::DecisionAnswer;
        for a in answers {
            match a {
                DecisionAnswer::Cut { cut, committed: Ok(committed), .. } => {
                    let settles = self.core.settles_of_running(*cut);
                    let tick = self.core.world_cut_running().map_or(0, |(_, t)| t);
                    self.cut_decided(*cut, tick, *committed, &settles);
                }
                DecisionAnswer::Cut { cut, committed: Err(e), .. } => {
                    self.warn_cuts(now, &format!("world cut {}: its decision could not be recorded ({e}); looking it up", cut.0));
                }
                DecisionAnswer::Assigned { outcome: Ok(game::coordinator::Assigned::Done { .. }), .. } => {
                    crate::health::Health::global().land_assigned();
                    self.tally.assigned += 1;
                }
                DecisionAnswer::Begin { .. } | DecisionAnswer::ReplicaEnded { .. } | DecisionAnswer::Assigned { .. } => {}
                // Elastic 4: the fleet's decisions that wrote, counted (the core still awaits the
                // answer here: `fleet_why` is the rule it came from).
                DecisionAnswer::HostAdded { outcome: Ok(game::coordinator::HostAdded::Done { .. }), .. } => {
                    crate::health::Health::global().fleet_added();
                    self.tally.added += 1;
                    self.tally.scale.extend(self.core.fleet_why().and_then(|w| w.scale_word()));
                }
                // Every slot held when a host is wanted (rule 4, or a starting login) — draining
                // hosts hold the slots; rule 5 never asks at the maximum (`WorldFleetNoFreeSlot`).
                DecisionAnswer::HostAdded { outcome: Ok(game::coordinator::HostAdded::Full), .. } => {
                    crate::health::Health::global().fleet_full();
                }
                DecisionAnswer::HostDrained {
                    outcome: Ok(game::coordinator::HostDrained::Marked | game::coordinator::HostDrained::RowOnly),
                    ..
                } => {
                    crate::health::Health::global().fleet_drained();
                    self.tally.drained += 1;
                    self.tally.scale.extend(self.core.fleet_why().and_then(|w| w.scale_word()));
                }
                DecisionAnswer::HostRemoved { outcome: Ok(game::coordinator::HostRemoved::Done), .. } => {
                    crate::health::Health::global().fleet_removed();
                    self.tally.removed += 1;
                }
                DecisionAnswer::HostAdded { .. } | DecisionAnswer::HostDrained { .. } | DecisionAnswer::HostRemoved { .. } => {}
                DecisionAnswer::Declared { outcome, .. } => {
                    let Some(p) = self.successions.as_mut() else { continue };
                    match outcome {
                        Ok(declared) => {
                            p.declare_failing_since = None;
                            // A zombie of a declared boot still posting is refused by the core
                            // (its messages dropped) and, detached, answered `attached: false`.
                            if let game::Declared::Done(list) = declared {
                                for _ in list {
                                    crate::health::Health::global().succession_declared();
                                }
                            }
                        }
                        Err(e) => {
                            // A lock timeout behind a staged part is a retry, not an error — but one
                            // that keeps failing is why nothing is saved and nobody inherits (a host
                            // frozen inside a transaction, a store that is down): said, throttled.
                            p.declare_retry_at = now + WORLD_CUT_DECISION_RETRY_MS;
                            let since = *p.declare_failing_since.get_or_insert(now);
                            if now.saturating_sub(since) >= DECLARE_FAILING_WARN_AFTER_MS {
                                let what = format!(
                                    "a succession could not be declared for {} s ({e}); still trying — until it is, no world \
                                     cut begins and the dead host's land has no owner",
                                    now.saturating_sub(since) / 1000
                                );
                                self.warn_cuts(now, &what);
                            } else {
                                log::debug!("[coordinator] a succession could not be declared yet ({e}); trying again");
                            }
                        }
                    }
                }
                DecisionAnswer::Ended { end, outcome, .. } => match outcome {
                    Ok(_) => {}
                    Err(e) => {
                        if let Some(p) = self.successions.as_mut() {
                            p.end_retry_at = now + WORLD_CUT_DECISION_RETRY_MS;
                        }
                        self.warn_cuts(now, &format!("host {} could not be ended ({e}); trying again", end.host));
                    }
                },
                DecisionAnswer::RolledBack { outcome, .. } => match outcome {
                    Ok(generation) => {
                        game::perf_event!("world_rollback", "generation={generation}");
                        crate::health::Health::global().world_generation(*generation);
                    }
                    Err(e) => self.warn_cuts(now, &format!("the world rollback could not be recorded ({e}); trying again")),
                },
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
                Err(TryRecvError::Empty) => {
                    // Final review S-I1: whatever `wait` selects on is taken off its channel before
                    // this early return — a notice left there would wake every wait at once (a
                    // busy loop logging an upkeep per spin for as long as the write is out). The
                    // notices only set what is due; nothing is issued while the write is out.
                    self.pump_notices();
                    return Ok(out);
                }
                Ok(answer) => answer,
                Err(TryRecvError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
            };
            let (t, _) = self.writing.take().expect("checked");
            match self.settle(t, answer, now, None) {
                Settled::Released(released) => out = released,
                Settled::Held { .. } => {}
                Settled::End(end) => return Err(end),
            }
        }
        // Every pass, whatever is in flight: the notices only set what is due, and one left in
        // its channel would wake every wait at once (a busy loop while a write retries).
        self.pump_notices();
        if self.writing.is_some() || now < self.retry_at_ms {
            return Ok(out);
        }
        if !self.write_next() {
            if self.core.evicted() {
                // Ended by the row (final review M2): nothing it applied leaves, no batch is
                // answered from it — `run` ends the thread next, and `down` refuses every waiter.
                return Ok(out);
            }
            if self.core.replicas_live().contains(&self.core.replica()) {
                self.joined();
            }
            self.pump_version_check(now)?;
            if !self.pump_clean_batches() {
                // The batches' inputs stay in the log until the check confirms the cache.
                return Ok(out);
            }
            out.extend(self.core.take_out());
        }
        Ok(out)
    }

    /// The API's batches applied since the last transition was taken, when the next one found
    /// nothing durable to write: they changed nothing the row holds, and are answered from the
    /// cache — at once with one live replica, else once a read of the row's version confirmed this
    /// cache is current (decision 5 of the plan: a replica may answer from a stale cache; a row that
    /// moved is refreshed first — the batches' inputs, still in the log, replayed onto it). Whether
    /// the core may release its log now ([`Coordinator::take_out`]): not while a check is out or
    /// covers only some of the batches waiting (their inputs must survive a refresh).
    fn pump_clean_batches(&mut self) -> bool {
        let several = self.core.replicas_live().len() > 1;
        let version = self.core.version();
        let Hosts::Api(api) = &mut self.hosts else { return true };
        if api.waiters.is_empty() {
            api.clean_check = None;
            return true;
        }
        if !several {
            api.clean_check = None;
            api.answer_through(api.applied, &self.core);
            return true;
        }
        let Some((covers, rx)) = &api.clean_check else {
            api.clean_check = Some((api.applied, self.store.row_version_later()));
            return false;
        };
        let covers = *covers;
        let answer = match rx.try_recv() {
            Err(TryRecvError::Empty) => return false,
            Ok(answer) => answer,
            Err(TryRecvError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
        };
        api.clean_check = None;
        match answer {
            Ok(v) if v == version => {
                api.answer_through(covers, &self.core);
                // Only when it covered every batch waiting: a later one's inputs stay in the log.
                covers == api.applied
            }
            Ok(v) => {
                log::info!("[coordinator] batch answers: the row is at {v}, this cache at {version}: refreshing first");
                self.refresh_wanted = true;
                false
            }
            Err(e) => {
                log::warn!("[coordinator] batch answers: the row's version could not be read ({e}); asking again");
                false
            }
        }
    }

    /// The `coordinator_row` notices (a newer version: a refresh) and the core's own finding of a
    /// stale cache (a cut it cannot know).
    fn pump_notices(&mut self) {
        let version = self.core.version();
        if let Some(rx) = &self.notices {
            let mut notices = Vec::new();
            let gone = loop {
                match rx.try_recv() {
                    Ok(n) => notices.push(n),
                    Err(TryRecvError::Empty) => break false,
                    Err(TryRecvError::Disconnected) => break true,
                }
            };
            if gone {
                // Its listener ended: a disconnected receiver would wake every wait at once (a
                // busy loop). Without notices a resync read stands in.
                log::warn!("[coordinator] the coordinator_row listener stopped; relying on the backstop poll");
                self.notices = None;
                notices.push(world_store::Notice::Resync);
            }
            for n in notices {
                match n {
                    world_store::Notice::Payload(p) => match p.trim().parse::<u64>() {
                        Ok(v) => {
                            if v > version {
                                self.refresh_wanted = true;
                            }
                            // Any replica's commit may have inserted mailbox rows: the API's
                            // waiting polls read again.
                            self.note_row_version(v);
                        }
                        Err(_) => log::debug!("[coordinator] a malformed coordinator_row notice ({p:?}) ignored"),
                    },
                    world_store::Notice::Resync => {
                        // Notices may have been missed: the row's version read (a refresh if it
                        // moved), and every waiting poll reads again.
                        self.resync_wanted = true;
                        if let Hosts::Api(api) = &self.hosts {
                            api.row_version.send_modify(|_| {});
                        }
                    }
                }
            }
        }
        if self.core.take_refresh_wanted() {
            self.stale_refresh = true;
            self.refresh_wanted = true;
        }
    }

    /// The row is at `version` or later (a notice, an own commit): the API's waiting mailbox polls
    /// are woken (the watch only moves forward).
    fn note_row_version(&self, version: u64) {
        if let Hosts::Api(api) = &self.hosts {
            api.row_version.send_if_modified(|v| {
                let newer = version > *v;
                if newer {
                    *v = version;
                }
                newer
            });
        }
    }

    /// This replica's `ReplicaJoined` is committed (Elastic 2b Task 4): a split coordinator is
    /// ready (`/readyz` 200) and its API serves.
    fn joined(&mut self) {
        if self.joined {
            return;
        }
        self.joined = true;
        log::info!("[coordinator] replica {} joined the live set (live {:?})", self.core.replica(), self.core.replicas_live());
        if let Hosts::Api(api) = &self.hosts {
            crate::health::Health::global().set_registered(true);
            if let Some(l) = &api.listener {
                l.set_refusal(None);
            }
        }
    }

    /// The version check of what leaves without a write (Elastic 2b departs 5; only while more
    /// than one replica is live — the core asks for none otherwise): the row's version read; at
    /// this cache's version the outputs the check covers may leave ([`Coordinator::take_out`]),
    /// at another the row is read again (its history only if newer) and the core rebased onto it
    /// first. One read in flight; the loop never waits for it.
    fn pump_version_check(&mut self, now: u64) -> Result<(), End> {
        if let Some(rx) = &self.refresh_rx {
            let answer = match rx.try_recv() {
                Err(TryRecvError::Empty) => return Ok(()),
                Ok(answer) => answer,
                Err(TryRecvError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
            };
            self.refresh_rx = None;
            let for_stale = std::mem::take(&mut self.refresh_for_stale);
            match answer {
                Ok(Some(row)) if row.version > self.core.version() => {
                    if !self.unacked.is_empty() {
                        // Not rebased without a loss now: asked again once its change is written
                        // (that write meets the newer row: a conflict decides).
                        self.refresh_wanted = true;
                        self.stale_refresh |= for_stale;
                    } else {
                        self.refresh_onto(row, now)?;
                    }
                }
                Ok(_) => {
                    if for_stale {
                        // Read after the host's message: the row holds no such cut either.
                        self.core.refresh_not_newer();
                    }
                }
                Err(e) => {
                    log::warn!("[coordinator] the row could not be read for a refresh: {e}");
                    self.refresh_wanted = true;
                    self.stale_refresh |= for_stale;
                }
            }
            return Ok(());
        }
        if let Some(rx) = &self.version_rx {
            let answer = match rx.try_recv() {
                Err(TryRecvError::Empty) => return Ok(()),
                Ok(answer) => answer,
                Err(TryRecvError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
            };
            self.version_rx = None;
            match answer {
                // Confirmed: what the check covers leaves with the core's `take_out`.
                Ok(v) if self.core.version_checked(v) => {}
                Ok(v) => {
                    log::info!(
                        "[coordinator] version check: the row is at {v}, this cache at {}: refreshing before anything leaves",
                        self.core.version()
                    );
                    self.refresh_wanted = true;
                }
                Err(e) => {
                    // Nothing leaves on it; the next pass asks again.
                    self.core.version_checked(u64::MAX);
                    log::warn!("[coordinator] version check failed: {e}");
                }
            }
            let _ = now;
            return Ok(());
        }
        if let Some(rx) = &self.resync_rx {
            let answer = match rx.try_recv() {
                Err(TryRecvError::Empty) => return Ok(()),
                Ok(answer) => answer,
                Err(TryRecvError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
            };
            self.resync_rx = None;
            match answer {
                Ok(v) if v > self.core.version() => {
                    log::info!("[coordinator] after missed notices the row is at {v}, this cache at {}: refreshing", self.core.version());
                    self.refresh_wanted = true;
                }
                Ok(_) => {}
                Err(e) => {
                    log::warn!("[coordinator] the row's version could not be read after missed notices ({e}); asking again");
                    self.resync_wanted = true;
                }
            }
            return Ok(());
        }
        if self.resync_wanted {
            self.resync_wanted = false;
            self.resync_rx = Some(self.store.row_version_later());
            return Ok(());
        }
        if self.refresh_wanted {
            self.refresh_wanted = false;
            self.refresh_for_stale = std::mem::take(&mut self.stale_refresh);
            let since = if self.core.history_dirty() { 0 } else { self.history_version };
            self.refresh_rx = Some(self.store.load_coordinator_since_later(since));
            return Ok(());
        }
        if self.core.version_check_wanted().is_some() {
            self.version_rx = Some(self.store.row_version_later());
        }
        Ok(())
    }

    /// A newer row a version check found (decision 1's rebase, without a conflict): the core
    /// rebased onto it — its history kept unless the row's is newer — under the same condition
    /// as after a conflict (a write that may be in the row cannot be rebased through: its inputs
    /// would apply twice — the process stops as fenced, the next start loads the row).
    fn refresh_onto(&mut self, row: world_store::StoredCoordinator, now: u64) -> Result<(), End> {
        if row.version <= self.core.version() {
            // Read before this cache's own later commit: older, never rebased onto (fix round 1).
            return Ok(());
        }
        if !self.unacked.is_empty() {
            log::error!(
                "[coordinator] the row moved to version {} (this cache at {}) and the core cannot be rebased without a loss; \
                 stopping as fenced — the next start loads it",
                row.version,
                self.core.version()
            );
            return Err(End::Conflict);
        }
        let history = (!row.history.is_empty()).then_some(row.history.as_slice());
        match self.core.refresh(&row.core, history, row.version) {
            Ok(true) => {
                self.tally.refreshes += 1;
                self.reissue_login_orders(now)
            }
            Ok(false) => {}
            Err(e) => {
                log::error!("[coordinator] the row at version {} does not load ({e}); stopping as fenced", row.version);
                return Err(End::Conflict);
            }
        }
        // The API's waiting mailbox polls read what the row now holds.
        self.note_row_version(row.version);
        if history.is_some() {
            self.history_version = row.history_version;
        }
        Ok(())
    }

    /// The replica's lease (Elastic 2b Task 4; a split coordinator only): its row beaten every
    /// [`game::coordinator::REPLICA_BEAT_MS`] from this main loop (a beat answering "not live":
    /// another replica ended it — the end), the replicas' rows read as often into its own
    /// [`game::coordinator::ReplicaWatch`], and — when it is the lowest replica its watch shows
    /// live — every replica the watch shows lapsed and the core still holds live ended
    /// ([`Input::ReplicaLapsed`], its row ended in that transition). The calls are answered on
    /// receivers: the loop never waits for them.
    fn pump_replica(&mut self, now: u64) -> Result<(), End> {
        let mono = self.mono_ms();
        let me = self.core.replica();
        let Some(r) = self.replication.as_mut() else { return Ok(()) };
        if let Some(rx) = &r.beat_rx {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                Ok(Ok(true)) => r.beat_rx = None,
                Ok(Ok(false)) => {
                    log::error!("[coordinator] replica {me}'s row is ended (another replica found it lapsed): stopping as fenced");
                    return Err(End::Evicted);
                }
                Ok(Err(e)) => {
                    log::warn!("[coordinator] replica {me}'s beat failed ({e}): no beat this time");
                    r.beat_rx = None;
                }
                Err(TryRecvError::Disconnected) => r.beat_rx = None,
            }
        }
        if r.beat_rx.is_none() && mono >= r.next_beat_mono {
            r.beat_rx = Some(self.store.beat_replica_later(me));
            r.next_beat_mono = mono + game::coordinator::REPLICA_BEAT_MS;
        }
        if let Some((issued, rx)) = &r.read_rx {
            let issued = *issued;
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                Ok(Ok(rows)) => {
                    r.watch.observe(&rows, issued);
                    r.read_rx = None;
                }
                Ok(Err(e)) => {
                    log::warn!("[coordinator] the replicas could not be read ({e}); the watch starts over");
                    r.watch.observe_failed();
                    r.read_rx = None;
                }
                Err(TryRecvError::Disconnected) => r.read_rx = None,
            }
        }
        if r.read_rx.is_none() && mono >= r.next_read_mono {
            r.read_rx = Some((mono, self.store.replicas_later()));
            r.next_read_mono = mono + game::coordinator::REPLICA_BEAT_MS;
        }
        if r.watch.ends_lapsed(me, mono) {
            let lapsed: Vec<_> =
                r.watch.lapsed(mono).into_iter().filter(|l| *l != me && self.core.replicas_live().contains(l)).collect();
            for l in lapsed {
                log::warn!("[coordinator] replica {l}'s row stood still for a lease: ending it");
                self.core.apply(Input::ReplicaLapsed(l), now);
            }
            // Fix round 1 (I1): a replica a registration ended (its row dead, or pruned) that no
            // watch saw stand still — dropped too (its row is dead already: the end is a no-op).
            for g in r.watch.gone(self.core.replicas_live(), me) {
                log::warn!("[coordinator] replica {g}'s row is ended (a registration found it lapsed): dropping it");
                self.core.apply(Input::ReplicaLapsed(g), now);
            }
        }
        Ok(())
    }

    /// Hands the core's next transition to the store (one at a time: none may be in flight);
    /// `false` when nothing durable changed.
    fn write_next(&mut self) -> bool {
        debug_assert!(self.writing.is_none());
        debug_assert_eq!(self.core.delivery(), self.delivery, "the core's delivery is the runtime's (a rebase keeps it)");
        let Some(t) = self.core.take_transition() else { return false };
        self.login_orders_in_write();
        if let Hosts::Api(api) = &mut self.hosts {
            // Every batch applied so far is in this transition's inputs: its commit answers them.
            api.writing_covers = api.applied;
            // A check of the cache it was taken from answers about an older cache.
            api.clean_check = None;
        }
        // The transition carries every output (its compare-and-set is their check): a version
        // check or a refresh asked before it answers about an older cache — dropped (fix round 1).
        self.version_rx = None;
        // A write meets any commit a missed notice was about (its compare-and-set conflicts).
        self.resync_rx = None;
        self.resync_wanted = false;
        if self.refresh_rx.take().is_some() {
            // Asked again after the write (if still needed: the write's conflict refreshes anyway).
            self.stale_refresh |= std::mem::take(&mut self.refresh_for_stale);
        }
        #[cfg(feature = "test-hooks")]
        {
            self.kill_at_commit = self.hooks.kill_after_commit.load(Ordering::SeqCst);
            if self.hooks.fail.load(Ordering::SeqCst) {
                let (tx, rx) = crossbeam::channel::bounded(1);
                let _ = tx.send(Err(StoreError::Backend(HOOK_REFUSED.into())));
                self.writing = Some((t, rx));
                return true;
            }
            if self.hooks.conflict_once.swap(false, Ordering::SeqCst) {
                // Another writer moves the row first: its own projection, at the next version.
                if let Ok(Some(row)) = self.store.load_coordinator() {
                    let foreign = TransitionWrite {
                        version: row.version,
                        core: Some(row.core),
                        history: None,
                        decisions: Vec::new(),
                        replica: game::coordinator::ReplicaId(u64::MAX),
                        ..Default::default()
                    };
                    let moved = self.store.write_transition_later(foreign).recv();
                    log::warn!("[coordinator] test hook: another writer moved the row from version {}: {moved:?}", row.version);
                }
            }
        }
        // Mailbox rows as `game::encode_packet` bytes (the wire's framing): none under
        // `Delivery::Direct`. Under `Delivery::Mailbox` a transition that changed nothing durable
        // (`t.core_changed` false: rows, drops, login answers or unmarked decisions only) is
        // written WITHOUT a core — the compare-and-set bumps the version, the row's core stays.
        // The login answers are `login_outbox` rows (`login_api::HostToLogin`, encoded), read by
        // the login service from any replica — under `Mailbox` only: role `all` serves the login
        // service itself (`login_link.rs`; its host sends no `HostMsg::Login`), and SQLite has no
        // `login_outbox`. The ticker's transitions prune the answers older than
        // [`LOGIN_ANSWER_TTL_MS`], at most once per [`LOGIN_ANSWER_PRUNE_EVERY_MS`].
        //
        // Rows are inserted in this vector's order, after every drop (the store deletes the drops'
        // hosts' rows first). A transition handed back after a failed write puts its rows ahead of
        // anything routed since, and an `Attach` applied since routes its `CoordMsg::Attached`
        // (with everything decided after it) at the END: no row decided for a host's older
        // attachment can follow its `Attached` in `seq` order within one write, so the host's
        // gate (it reads only from its current `Attached` on) never delivers one.
        let core_less = self.delivery == Delivery::Mailbox && !t.core_changed;
        if core_less {
            check_core_less(&t);
        }
        let outbox = t
            .outbox
            .iter()
            .map(|o| (o.host, o.boot, game::encode_packet(&o.msg).expect("a coordinator message encodes")))
            .collect();
        let mailbox = self.delivery == Delivery::Mailbox;
        let login_outbox: Vec<Vec<u8>> = if mailbox {
            t.login_outbox
                .iter()
                .map(|o| game::encode_packet(&crate::login_link::answer_from(o.clone())).expect("a login answer encodes"))
                .collect()
        } else {
            if !t.login_outbox.is_empty() {
                log::debug!("[coordinator] {} login answer(s) with no login service reading this coordinator: dropped", t.login_outbox.len());
            }
            Vec::new()
        };
        let now = self.now_ms();
        let login_prune_before_ms = (mailbox
            && self.core.is_ticker()
            && self.login_pruned_ms.is_none_or(|at| now >= at + LOGIN_ANSWER_PRUNE_EVERY_MS))
        .then(|| {
            self.login_pruned_ms = Some(now);
            now.saturating_sub(LOGIN_ANSWER_TTL_MS)
        });
        let w = TransitionWrite {
            version: self.core.version(),
            core: (!core_less).then(|| t.core.clone()),
            history: t.history.clone(),
            decisions: t.decisions.clone(),
            replica: self.core.replica(),
            outbox,
            outbox_drop: t.outbox_drop.clone(),
            login_outbox,
            login_prune_before_ms,
            ..Default::default()
        };
        let written = self.store.write_transition_later(w);
        #[cfg(feature = "test-hooks")]
        let written = if self.hooks.hold_answers.load(Ordering::SeqCst) {
            // The answer waits on its way until the hook is released.
            self.hooks.answers_held.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = crossbeam::channel::bounded(1);
            let hold = self.hooks.hold_answers.clone();
            std::thread::spawn(move || {
                let answer = written.recv();
                while hold.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if let Ok(answer) = answer {
                    let _ = tx.send(answer);
                }
            });
            rx
        } else {
            written
        };
        self.writing = Some((t, written));
        true
    }

    /// The row as it stands now (a `Conflict`'s re-read), waiting at most until `deadline` (the
    /// orderly stop's; `None`: as long as the store takes).
    fn reread_row(&self, deadline: Option<Instant>) -> Result<Option<world_store::StoredCoordinator>, StoreError> {
        let rx = self.store.load_coordinator_later();
        let stopped = || StoreError::Backend("the world-store thread has stopped".into());
        match deadline {
            None => rx.recv().unwrap_or_else(|_| Err(stopped())),
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(answer) => answer,
                Err(RecvTimeoutError::Timeout) => Err(StoreError::Backend("the row was not re-read before the stop's deadline".into())),
                Err(RecvTimeoutError::Disconnected) => Err(stopped()),
            },
        }
    }

    /// The answer of the write of transition `t` (decision 2): committed → its messages, the
    /// store clock sampled from the commit, its decisions' answers applied as inputs at once and
    /// its post-commit effects queued; `Fenced` → the end; `Conflict` → the row re-read (until
    /// `deadline`, if any): this process's own lost commit is written over at once; another
    /// writer's row is rebased onto ([`Self::rebase_onto`]); any other failure hands `t` back to
    /// the core — its messages wait, its inputs stay applied once, its decisions are looked up
    /// (whether it committed is not known) — and the next write is tried after
    /// [`COORDINATOR_SAVE_RETRY_MS`].
    fn settle(
        &mut self,
        t: game::coordinator::Transition,
        answer: Result<TransitionWritten, StoreError>,
        now: u64,
        deadline: Option<Instant>,
    ) -> Settled {
        #[cfg(feature = "test-hooks")]
        let answer = match answer {
            Ok(_) if self.hooks.lose_answer.swap(false, Ordering::SeqCst) => {
                Err(StoreError::Backend("test hook: the commit's answer was lost".into()))
            }
            answer => answer,
        };
        match answer {
            Ok(w) => {
                self.core.transition_written(w.version);
                self.failed_in_a_row = 0;
                self.unacked.clear();
                self.conflicts_in_a_row = 0;
                self.sample_store_clock(w.store_ms);
                // A core-less write wrote no core bytes.
                let core_less = self.delivery == Delivery::Mailbox && !t.core_changed;
                let core = if core_less { 0 } else { t.core.len() as u64 };
                let history = t.history.as_ref().map_or(0, |h| h.len() as u64);
                self.tally.transitions += 1;
                self.tally.core_bytes += core;
                self.tally.history_bytes += history;
                if self.delivery == Delivery::Mailbox {
                    self.tally.mailbox_rows += t.outbox.len() as u64;
                    self.tally.login_rows += t.login_outbox.len() as u64;
                    self.tally.core_less += u64::from(core_less);
                }
                crate::health::Health::global().coordinator_written(core, history);
                #[cfg(feature = "test-hooks")]
                if std::mem::take(&mut self.kill_at_commit) {
                    // Its decisions' answers and post-commit effects die with the process.
                    return Settled::End(End::Killed);
                }
                if t.history.is_some() {
                    self.history_version = w.version;
                }
                if self.core.replicas_live().contains(&self.core.replica()) {
                    self.joined();
                }
                // The decisions' answers: inputs of the core (logged), applied before anything
                // else — each one's side effects only if the core awaits it (an answer whose
                // stamp is not the mark's changes nothing, and is counted for nothing); then
                // their post-commit effects.
                for a in w.answers {
                    if self.core.answer_awaited(&a) {
                        self.answered(std::slice::from_ref(&a), now);
                    } else {
                        log::debug!("[coordinator] a decision's answer no mark awaits: no side effects ({a:?})");
                    }
                    self.core.apply(Input::Decision(a), now);
                }
                self.after.extend(w.after_commit.into_iter().map(|a| AfterJob { a, rx: None, retry_at: now }));
                // Durable now: the login orders whose rows it carried are answered (the login
                // answers it held are `login_outbox` rows, read by the login service).
                self.login_orders_committed();
                // The API's batches it covers are answered (from the core as it stands now, its
                // decisions' answers applied), and the waiting mailbox polls read its rows.
                self.note_row_version(w.version);
                if let Hosts::Api(api) = &mut self.hosts {
                    let covers = api.writing_covers;
                    api.answer_through(covers, &self.core);
                }
                Settled::Released(t.out)
            }
            Err(StoreError::Fenced { .. }) => Settled::End(End::Fenced),
            Err(StoreError::Conflict) => {
                self.tally.conflicts += 1;
                crate::health::Health::global().coordinator_conflict();
                #[cfg(feature = "test-hooks")]
                self.hooks.conflicts.fetch_add(1, Ordering::SeqCst);
                let mine = self.core.version();
                match self.reread_row(deadline) {
                    Ok(Some(row))
                        if row.version == mine + 1 && self.unacked.iter().any(|u| u.hash == core_hash(&row.core)) =>
                    {
                        // Lost-commit recognition (decision 1): our own write; the cached core
                        // holds it and every input applied since — written over it at once. (That
                        // write's decisions were looked up at its failure; `t` met the conflict,
                        // so it did not commit: its own decisions go out again with the next.)
                        log::warn!(
                            "[coordinator] write conflict: the row holds this process's own write at version {} (its \
                             commit's answer was lost); writing again over it",
                            row.version
                        );
                        // Its entry stays remembered (as every failed write's, until a commit): the
                        // log still holds that write's inputs, so a rebase now would apply them twice.
                        let hash = core_hash(&row.core);
                        let committed = self.unacked.iter_mut().find(|u| u.hash == hash).map(|u| std::mem::take(&mut u.shape));
                        self.core.set_version(row.version);
                        self.history_version = row.history_version;
                        self.core.transition_refused(t);
                        self.login_orders_handed_back();
                        if let Some(shape) = committed {
                            // Its mailbox rows, drops and login answers are in the store already:
                            // not written twice.
                            let _ = self.core.outbox_committed(shape);
                        }
                        self.retry_at_ms = now;
                        Settled::Held { now: true }
                    }
                    Ok(Some(row)) => self.rebase_onto(t, row, now),
                    Ok(None) => {
                        log::error!("[coordinator] write conflict, and the coordinator row is gone; stopping as fenced");
                        Settled::End(End::Conflict)
                    }
                    // Not known yet: `t` met the conflict, so it provably did not commit — handed
                    // back without being remembered as a write that may be in the row (the next
                    // write meets the conflict again).
                    Err(e) => self.refused(t, &format!("a conflict, and the row could not be re-read: {e}"), now),
                }
            }
            Err(StoreError::Backend(e)) if e == HOOK_REFUSED => self.refused(t, &e, now),
            Err(e) => self.failed(t, &e.to_string(), now),
        }
    }

    /// Another writer's `row` (decision 1): the core is rebased onto it — `t` dropped, its
    /// messages decided again by the replay — and written again, at once up to
    /// [`COORDINATOR_CONFLICT_RETRIES`] conflicts in a row, then after
    /// [`COORDINATOR_SAVE_RETRY_MS`]. What the core changed outside its log (the ticker's reads
    /// and hand-outs, Elastic 2b Task 6) is dropped and derived again. A failed write that may be
    /// in the row (its inputs would apply twice) cannot be rebased through: the process stops as
    /// Elastic 2a did, and the next start loads the row.
    fn rebase_onto(&mut self, t: game::coordinator::Transition, row: world_store::StoredCoordinator, now: u64) -> Settled {
        let mine = self.core.version();
        if !self.unacked.is_empty() {
            log::error!(
                "[coordinator] write conflict: the row is at version {}, written by another writer (this process holds \
                 version {mine}); it cannot be rebased onto it: a write of it failed since its last commit and may be in \
                 the row; stopping as fenced — the next start loads it",
                row.version
            );
            return Settled::End(End::Conflict);
        }
        drop(t);
        if let Err(e) = self.core.rebase(&row.core, Some(&row.history), row.version) {
            log::error!("[coordinator] write conflict: the row at version {} does not load ({e}); stopping as fenced", row.version);
            return Settled::End(End::Conflict);
        }
        // The dropped transition's login order rows, and those queued since, are decided again.
        self.reissue_login_orders(now);
        self.history_version = row.history_version;
        self.conflicts_in_a_row += 1;
        log::warn!(
            "[coordinator] write conflict: another writer moved the row from version {mine} to {}; rebased onto it ({} in a row)",
            row.version,
            self.conflicts_in_a_row
        );
        if self.conflicts_in_a_row >= COORDINATOR_CONFLICT_RETRIES {
            self.conflicts_in_a_row = 0;
            self.retry_at_ms = now + COORDINATOR_SAVE_RETRY_MS;
            Settled::Held { now: false }
        } else {
            self.retry_at_ms = now;
            Settled::Held { now: true }
        }
    }

    /// A write of `t` that failed with `why` and may have committed (not fenced, not a
    /// conflict): remembered by its core's hash (a later conflict may find it in the row), `t`
    /// back in the core (its decisions looked up, never issued again), the next write after
    /// [`COORDINATOR_SAVE_RETRY_MS`].
    fn failed(&mut self, t: game::coordinator::Transition, why: &str, now: u64) -> Settled {
        if self.delivery == Delivery::Mailbox && !t.core_changed {
            // Written without a core: its core is the row's, so a commit of it could not be told
            // from another replica's — NEVER remembered. A conflict after it is a foreign row:
            // rebased onto, its inputs replayed, its rows decided again (twice in a mailbox if it
            // did commit: only messages a host takes twice without harm are core-less,
            // [`core_less_kind`]). Its unmarked decisions are idempotent in the store.
            self.core.transition_failed(t);
            self.login_orders_handed_back();
            return self.held_back(why, now);
        }
        if self.unacked.len() == COORDINATOR_UNACKED_MAX {
            self.unacked.pop_front();
        }
        self.unacked.push_back(Unacked { hash: core_hash(&t.core), shape: t.shape() });
        self.core.transition_failed(t);
        self.login_orders_handed_back();
        self.held_back(why, now)
    }

    /// A write of `t` that provably did not commit (it met a conflict, or never reached the
    /// store): `t` back in the core, its decisions with it, NOT remembered as a write that may be
    /// in the row; the next write after [`COORDINATOR_SAVE_RETRY_MS`].
    fn refused(&mut self, t: game::coordinator::Transition, why: &str, now: u64) -> Settled {
        self.core.transition_refused(t);
        self.login_orders_handed_back();
        self.held_back(why, now)
    }

    /// A write that did not commit (as far as this process knows): counted; saving reported
    /// stuck after [`COORDINATOR_FAILURES_BEFORE_STUCK`] in a row.
    fn held_back(&mut self, why: &str, now: u64) -> Settled {
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
    /// and history bytes, the conflicts and failed writes (Elastic 2a, decision 8: E2b's measure),
    /// the land assignments applied (`assigned=`, Elastic 3) and the fleet's decisions that wrote
    /// (`added=`, `drained=`, `removed=`, Elastic 4) — and of those, the load rules' (`scale=`,
    /// `up` / `down` in answer order, `-` for none); under the HTTP API's mailbox delivery also the
    /// hosts detached as stalled (`stalled=`), the mailbox and login-answer rows the committed
    /// transitions inserted (`mailbox_rows=`, `login_rows=`) and how many transitions wrote no
    /// core (`core_less=`) — the write volume of the coordinator HTTP API plan's Review Focus 5 —
    /// and the refreshes onto another replica's row (`refreshes=`, what each version bump costs
    /// the other replicas).
    fn report_writes(&mut self) {
        let window = self.tally.since.elapsed();
        if window < game::METRICS_REPORT_INTERVAL {
            return;
        }
        let t = std::mem::replace(&mut self.tally, WriteTally::new());
        if t.transitions == 0
            && t.conflicts == 0
            && t.failures == 0
            && t.assigned == 0
            && t.added + t.drained + t.removed == 0
            && t.stalled == 0
            && t.refreshes == 0
        {
            return;
        }
        log::info!(
            "[coordinator] window={:.1}s transitions={} core_bytes={} history_bytes={} conflicts={} write_failures={} version={} \
             assigned={} added={} drained={} removed={} scale={} stalled={} mailbox_rows={} login_rows={} core_less={} refreshes={}",
            window.as_secs_f32(),
            t.transitions,
            t.core_bytes,
            t.history_bytes,
            t.conflicts,
            t.failures,
            self.core.version(),
            t.assigned,
            t.added,
            t.drained,
            t.removed,
            if t.scale.is_empty() { "-".to_string() } else { t.scale.join(",") },
            t.stalled,
            t.mailbox_rows,
            t.login_rows,
            t.core_less,
            t.refreshes
        );
    }

    fn send(&mut self, out: Vec<(Target, CoordMsg)>) {
        if out.is_empty() {
            return;
        }
        match &mut self.hosts {
            // One host per process (`AllInOne`): everything goes to it.
            Hosts::Channels(hosts) => {
                for host in hosts.iter().filter(|h| !h.gone) {
                    for (_, msg) in &out {
                        let _ = host.tx.send(msg.clone());
                    }
                    let _ = host.wake.try_send(());
                }
            }
            // `Delivery::Mailbox`: every message is a mailbox row, nothing is released here.
            Hosts::Api(_) => {
                debug_assert!(false, "{} message(s) released under Delivery::Mailbox", out.len());
                log::error!("[coordinator] {} message(s) released outside a transition under the mailbox delivery: dropped", out.len());
            }
        }
    }

    /// The orderly stop: the write in flight is settled, then the last transition (whatever
    /// was handled since) is written, all within `timeout` — a conflict's re-read of the row
    /// too; what the commits released is sent (the hosts may still read it while they stop).
    /// There is no other "final save".
    fn finish(mut self, timeout: Duration) -> ServerExit {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        // Bounded: the write in flight, the last transition, a retry after a lost commit, and
        // the rebases of a run of conflicts (all within the deadline).
        for _ in 0..COORDINATOR_CONFLICT_RETRIES + 3 {
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
            match self.settle(t, answer, 0, Some(deadline)) {
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
        self.down(ApiRefusal::NotReady);
        ServerExit::Clean
    }

    /// The thread ends for `end`: fenced (also a conflict it cannot rebase through without a
    /// loss; the process exits [`crate::claims::EXIT_FENCED`]), or a test's kill.
    fn end(&mut self, end: End) -> ServerExit {
        match end {
            End::Fenced => self.fence_out(),
            End::Conflict => {
                self.fenced.set();
                self.down(ApiRefusal::Fenced);
                ServerExit::Fenced
            }
            #[cfg(feature = "test-hooks")]
            End::Killed => {
                log::warn!("[coordinator] test kill: the transition committed, nothing it released leaves");
                self.down(ApiRefusal::NotReady);
                ServerExit::CoordinatorFailed
            }
            End::Evicted => {
                log::error!("[coordinator] replica {} was ended by another replica: it writes nothing more; stopping as fenced", self.core.replica());
                self.fenced.set();
                self.down(ApiRefusal::Fenced);
                ServerExit::Fenced
            }
        }
    }

    fn fence_out(&mut self) -> ServerExit {
        log::error!("[coordinator] the coordinator's write was refused as fenced: another coordinator took over; stopping");
        self.fenced.set();
        self.down(ApiRefusal::Fenced);
        ServerExit::Fenced
    }

    /// Every host's link goes down: a channel link's end says so; the API refuses everything
    /// with `why` (`Fenced` for a fenced end, `NotReady` for a stop), every batch and held login
    /// order still waiting is answered so, and the listener stops (its port freed).
    fn down(&mut self, why: ApiRefusal) {
        match &mut self.hosts {
            Hosts::Channels(hosts) => {
                for host in hosts.iter() {
                    host.up.store(false, Ordering::SeqCst);
                }
            }
            Hosts::Api(api) => {
                if let Some(l) = &api.listener {
                    l.set_refusal(Some(why));
                }
                api.refuse_all(why);
                // Calls queued but not taken: answered too (a dropped answer is a 503 anyway).
                for call in api.calls.try_iter() {
                    match call {
                        ApiCall::Batch { done, .. } => {
                            let _ = done.send(Err(why));
                        }
                        ApiCall::LoginOrder { done, .. } => {
                            let _ = done.send(Err(why));
                        }
                    }
                }
                drop(api.listener.take());
            }
        }
    }
}

