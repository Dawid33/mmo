//! Writing the `WorldManager`'s save cuts to the world store: every complete
//! cut is queued for commit at once (`StoreHandle::commit_cut` does not
//! block), and each answer goes back to the manager — `cut_committed` clears
//! what it carried, `cut_failed` keeps it for the next cut (nothing is lost).
//! Commits are fenced ([`HostClaims`]): a refusal as fenced makes the whole
//! process fenced (`Fenced`), and nothing more is written.
//!
//! **World cuts** (several hosts, R3 Task 13; [`CutWriter::with_world_cuts`]): the
//! manager's part of a world cut is staged, not committed (`StoreHandle::prepare_cut`,
//! at this process's world generation): staged → the manager tells the coordinator
//! (`CutPrepared`); refused or failed → `CutFailed { Store }`. The part waits for the
//! coordinator's `CutDone`; then `cut_decided` settles the store's pending mark FIRST
//! and only then the manager hears `cut_committed` / `cut_failed` (store obligations 2
//! and 4: until it is settled, the handle refuses every further cut). A part that may be
//! staged although its prepare failed, or whose `CutDone` never comes, is settled the
//! same way by asking the store every [`WORLD_PART_POLL`]. Told "aborted" while the store
//! has no decision yet, or hearing nothing [`game::WORLD_CUT_DEADLINE_MS`] after the
//! prepare, the host records the abort itself (`abort_cut`: the first decision written
//! wins) and settles. A `CutPending` from another cut's part makes that part the one to
//! settle. A `StaleGeneration` notes the newer generation (the host exits for a world
//! rollback); `CutAborted` needs no settling. Every store call here is answered on a
//! receiver: the main loop never waits. A part that keeps the next cuts from being staged
//! counts toward [`CUT_FAILURES_BEFORE_STUCK`]. With one host nothing of this runs.
//!
//! **Leaving** (several hosts, R3 Task 14; [`exit_world`]): an exiting host asks for a leave
//! cut (`LeaveRequest`); on the `BeginCut` naming it leaving its manager parks everything
//! (the regions' `Stopped` answers are the cut's) and the part is staged like any other.
//! Committed: its `hosts` row is clean, it exits 0 and its return keeps the world
//! generation. An aborted leave cut is asked for again; no committed one within
//! [`game::LEAVE_CUT_TIMEOUT_SECS`] of the first request: it exits 0 without a cut ([`LEFT_WITHOUT_A_CUT`]) — its row stays dirty, so its return
//! raises the generation and the world rolls back to the last cut.
//!
//! **Other hosts' saves** (several hosts, R3 Task 19; [`TerrainWatch`]): every committed
//! cut is announced (`StoreHandle::notify_cut`: the one-phase commit here, the world cut's
//! decision in the coordinator runtime); each host listens (`StoreHandle::listen_cuts`) and
//! hands the versions the store then holds to `WorldManager::stored_terrain_changed`, which
//! re-seeds the halos of other hosts' parked regions from the store.
//!
//! **Start-up** ([`resolve_staged_cuts`]): every process on Postgres (any role) finishes the
//! parts a crash left staged (as their cut's decision says; an undecided cut is aborted) before it
//! claims, registers or loads anything.

use game::HostId;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, TryRecvError};
use game::{CutFailure, CutId, RegionCoords, RegionOutput, RegionSpawner, Tick, WorldCut, WorldManager};
use world_store::{CutCommitted, CutNotice, Fence, NotifyMode, StoreError, StoreHandle};

use crate::claims::{Fenced, HostClaims, ServerExit, WorldGeneration, FENCED_REFUSAL, LEAVING_REFUSAL, ROLLBACK_REFUSAL};
use crate::coordinator_rt::CoordinatorThread;

struct Pending {
    id: CutId,
    tick: Tick,
    regions: usize,
    players: usize,
    queued: Instant,
    reply: Receiver<Result<CutCommitted, StoreError>>,
    /// The fence the commit was checked against: a refusal is classified against it.
    fence: Arc<Fence>,
}

/// Consecutive failed commits after which world saving is reported stuck
/// (once, at error level, plus a `world_save_stuck` perf event). Nothing is
/// dropped: every later cut still carries everything uncommitted.
pub const CUT_FAILURES_BEFORE_STUCK: u32 = 3;

/// How long the orderly exit waits (wall time) for every region to park
/// before it saves without the ones that have not.
pub const EXIT_PARK_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the exit's park loop re-checks while no region output arrives.
const EXIT_PARK_POLL: Duration = Duration::from_millis(20);

/// How often a world part that may be staged is settled by asking the store
/// (`cut_decided`) when no `CutDone` came for it.
pub const WORLD_PART_POLL: Duration = Duration::from_secs(1);

/// At most one warning per this long about failing reads of a world part's decision.
pub const WORLD_PART_WARN: Duration = Duration::from_secs(10);

/// What an exiting host of several logs when it leaves with no committed cut.
pub const LEFT_WITHOUT_A_CUT: &str = "left without a cut: the world rolls back to the last cut when this host returns";

/// Start-up recovery of a split-role process on Postgres, before it claims, registers or
/// loads anything: every world cut part a crash left staged is finished as its cut's
/// decision says, an undecided cut aborted (`StoreHandle::resolve_prepared`). Logged per
/// cut.
pub fn resolve_staged_cuts(store: &StoreHandle) -> Result<Vec<(CutId, bool)>, StoreError> {
    let resolved = store.resolve_prepared()?;
    for (cut, committed) in &resolved {
        log::warn!(
            "[server] world cut {} was left staged by a crash: {}",
            cut.0,
            if *committed { "committed (it was decided)" } else { "rolled back (undecided: aborted)" }
        );
    }
    Ok(resolved)
}

/// A store answer on its way.
type Answer<T> = Receiver<Result<T, StoreError>>;

/// This host's part of a world cut, from its prepare until the store has settled it.
struct WorldPart {
    id: CutId,
    tick: Tick,
    /// The fence its prepare was checked against (`None`: not prepared by this writer): a
    /// refusal is classified against it, not against a fence changed since.
    fence: Option<Arc<Fence>>,
    /// The prepare's answer, while it has not come.
    reply: Option<Answer<CutCommitted>>,
    /// Staged (the prepare answered `Ok`): its region versions.
    versions: Option<Vec<(RegionCoords, u64)>>,
    /// The store may hold the part staged (a prepare that succeeded, or failed with the
    /// pending mark possibly kept): settled with `cut_decided`.
    maybe_staged: bool,
    /// The coordinator's `CutDone` came (its outcome).
    done: Option<bool>,
    /// The store was last asked about it at.
    polled: Option<Instant>,
    /// `cut_decided`'s answer on its way.
    deciding: Option<Answer<Option<bool>>>,
    /// This host's own `abort_cut` on its way.
    aborting: Option<Answer<bool>>,
    /// When the part was queued.
    since: Instant,
}

impl WorldPart {
    fn new(id: CutId, tick: Tick, reply: Option<Answer<CutCommitted>>) -> Self {
        WorldPart {
            id,
            tick,
            fence: None,
            reply,
            versions: None,
            maybe_staged: true,
            done: None,
            polled: None,
            deciding: None,
            aborting: None,
            since: Instant::now(),
        }
    }
}

/// The server's side of the save cuts: the store and the commits in flight.
pub struct CutWriter {
    claims: HostClaims,
    pending: Vec<Pending>,
    /// Commits failed in a row (reset by one that commits).
    failed_in_a_row: u32,
    /// World cuts (several hosts): this process's world generation.
    world: Option<WorldGeneration>,
    /// The world part not settled yet (one at a time: the store refuses another meanwhile).
    part: Option<WorldPart>,
    /// When a failing read of a world part's decision was last warned about.
    warned: Option<Instant>,
    /// How long after its prepare a part with no decision stored is aborted by this host
    /// ([`game::WORLD_CUT_DEADLINE_MS`]; a test shortens it).
    part_deadline: Duration,
    /// How long the exit waits for the `BeginCut` naming this host leaving
    /// ([`game::LEAVE_CUT_TIMEOUT_SECS`]; a test shortens it).
    leave_timeout: Duration,
    /// This host's own world-cut period (`VOXEL_SAVE_SECS`; `None`: not told): no world cut
    /// committed for [`CUT_FAILURES_BEFORE_STUCK`] + 1 periods is reported stuck too — a cut
    /// the coordinator never begins fails nothing. The coordinator's period may differ: the
    /// gap between the last two commits seen here counts when it is longer.
    world_period: Option<Duration>,
    /// When a world cut last committed (or this writer started).
    world_saved: Instant,
    /// When the last world cut committed, if one did.
    last_commit: Option<Instant>,
    /// The gap between the last two committed world cuts (the coordinator's period, as seen).
    commit_gap: Option<Duration>,
    /// When absent world cuts were last reported.
    absent_warned: Option<Instant>,
    /// The `hosts` read an absent-cut warning waits for (who has a dirty row).
    absent_read: Option<Answer<Vec<world_store::HostRow>>>,
}

/// Why no world cut commits, as far as a host can tell (the coordinator's log says more).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AbsentCause {
    /// The coordinator has not applied this host's latest `Attach` (it is down, or the link is).
    pub coordinator_detached: bool,
    /// Successions this host answers or adopts that are not over (no cut begins meanwhile).
    pub successions: Vec<u64>,
    /// Other hosts with a dirty live `hosts` row that this host cannot reach either.
    pub dirty_unattached: Vec<HostId>,
}

/// The host's "no world cut in N periods" warning: `stalled` without a commit, cuts expected
/// about every `every` (this host's period, or the gap it saw between two commits), and each
/// cause it can name.
pub fn absent_cut_warning(stalled: Duration, every: Duration, cause: &AbsentCause) -> String {
    let mut out = format!(
        "[server] world saving is stuck: no world cut has committed in {} s (expected about every {} s)",
        stalled.as_secs(),
        every.as_secs()
    );
    let mut named = false;
    if cause.coordinator_detached {
        named = true;
        out.push_str("; the coordinator is not attached (it is not running, or this host's link to it is down): no cut begins without it");
    }
    if !cause.successions.is_empty() {
        named = true;
        out.push_str(&format!(
            "; a succession is running (successions {:?}): no cut begins until every survivor applied it and its heir adopted",
            cause.successions
        ));
    }
    for h in &cause.dirty_unattached {
        named = true;
        out.push_str(&format!(
            "; host {0} has a dirty row and is not attached (it may hold unsaved state): start it, or retire it for good \
             with `worldctl retire-host <url> {0}`",
            h.0
        ));
    }
    if !named {
        out.push_str("; the coordinator's log says why");
    }
    out
}

impl CutWriter {
    pub fn new(claims: HostClaims) -> Self {
        CutWriter {
            claims,
            pending: Vec::new(),
            failed_in_a_row: 0,
            world: None,
            part: None,
            warned: None,
            part_deadline: Duration::from_millis(game::WORLD_CUT_DEADLINE_MS),
            leave_timeout: Duration::from_secs(game::LEAVE_CUT_TIMEOUT_SECS),
            world_period: None,
            world_saved: Instant::now(),
            last_commit: None,
            commit_gap: None,
            absent_warned: None,
            absent_read: None,
        }
    }

    /// The world-cut period: no world cut committed for [`CUT_FAILURES_BEFORE_STUCK`] + 1
    /// of them is reported (at error level, once per that span).
    pub fn set_world_cut_period(&mut self, every: Duration) {
        self.world_period = Some(every);
    }

    /// A writer of a host of several (`VOXEL_HOSTS` > 1): its world cut parts are staged
    /// at `generation` and decided by the coordinator (see the module docs).
    pub fn with_world_cuts(claims: HostClaims, generation: WorldGeneration) -> Self {
        CutWriter { world: Some(generation), ..CutWriter::new(claims) }
    }

    /// Whether this writer stages world cut parts (several hosts).
    pub fn world_cuts(&self) -> bool {
        self.world.is_some()
    }

    /// Test hook: the deadline after which this host aborts a part with no decision stored.
    #[doc(hidden)]
    pub fn set_world_part_deadline(&mut self, deadline: Duration) {
        self.part_deadline = deadline;
    }

    /// Test hook: how long the exit waits for its leave cut to begin.
    #[doc(hidden)]
    pub fn set_leave_timeout(&mut self, timeout: Duration) {
        self.leave_timeout = timeout;
    }

    /// The world part not settled yet, if any.
    pub fn world_part(&self) -> Option<CutId> {
        self.part.as_ref().map(|p| p.id)
    }

    /// Commits failed in a row since the last one that committed.
    pub fn failed_in_a_row(&self) -> u32 {
        self.failed_in_a_row
    }

    pub fn store(&self) -> &StoreHandle {
        self.claims.store()
    }

    /// Commits still unanswered.
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// Queue the manager's complete cut (if any) for commit, and hand every
    /// answer that has come back to the manager. Never blocks.
    pub fn pump<S: RegionSpawner>(&mut self, manager: &mut WorldManager<S>) {
        while let Some(cut) = manager.take_cut() {
            if self.world.is_some() && manager.world_cut() == Some(cut.id) {
                self.prepare(manager, cut);
                continue;
            }
            let (id, tick, regions, players) = (cut.id, cut.tick, cut.regions.len(), cut.players.len());
            let fence = self.claims.fence();
            let reply = self.claims.store().commit_cut(cut, (*fence).clone());
            self.pending.push(Pending { id, tick, regions, players, queued: Instant::now(), reply, fence });
        }
        if self.world.is_some() {
            self.pump_world(manager);
        }
        let mut i = 0;
        while i < self.pending.len() {
            match self.pending[i].reply.try_recv() {
                Err(TryRecvError::Empty) => i += 1,
                Ok(answer) => {
                    let p = self.pending.remove(i);
                    self.settle(manager, p, answer);
                }
                Err(TryRecvError::Disconnected) => {
                    let p = self.pending.remove(i);
                    self.settle(manager, p, Err(StoreError::Backend("the world-store thread has stopped".into())));
                }
            }
        }
    }

    /// The orderly exit's save, once every region has parked: a last cut
    /// (complete at once — nothing runs), committed, and every commit's
    /// answer waited for (at most `timeout`); then the store is shut down.
    /// Returns whether the last cut committed.
    ///
    /// With several hosts the exit's save is the leave cut instead ([`exit_world`]).
    pub fn finish<S: RegionSpawner>(mut self, manager: &mut WorldManager<S>, now: Tick, timeout: Duration) -> bool {
        // A cut still pending (waiting for a region that never answered) is
        // given up by the manager at its deadline; a complete one goes first.
        self.pump(manager);
        let last = manager.begin_cut(now);
        if last.is_none() {
            log::error!("[server] no save cut could be started at exit (one is still pending); the world is saved as of the last commit");
        }
        self.pump(manager);
        let deadline = Instant::now() + timeout;
        let mut committed = false;
        while let Some(p) = self.pending.first() {
            let left = deadline.saturating_duration_since(Instant::now());
            let answer = match p.reply.recv_timeout(left) {
                Ok(answer) => answer,
                Err(crossbeam::channel::RecvTimeoutError::Timeout) => {
                    log::error!("[server] the world store did not answer the save at exit within {timeout:?}");
                    break;
                }
                Err(crossbeam::channel::RecvTimeoutError::Disconnected) => {
                    Err(StoreError::Backend("the world-store thread has stopped".into()))
                }
            };
            let p = self.pending.remove(0);
            let id = p.id;
            let ok = answer.is_ok();
            self.settle(manager, p, answer);
            if Some(id) == last {
                committed = ok;
            }
        }
        // Commits still queued are drained before the store thread stops.
        self.claims.store().clone().shutdown();
        committed
    }

    /// The exit when no last cut may be written (a region was given up, or
    /// the host was fenced): commits already queued are drained and answered,
    /// then the store is shut down. The world is as of its last committed cut.
    pub fn finish_without_cut<S: RegionSpawner>(mut self, manager: &mut WorldManager<S>) {
        self.pump(manager);
        self.claims.store().clone().shutdown();
    }

    /// Whether this process was fenced.
    pub fn fenced(&self) -> &Fenced {
        self.claims.fenced()
    }

    /// Stage the manager's part of world cut `cut.id`; refused at once while an earlier
    /// part is not settled (store obligation 4: never queued behind it).
    fn prepare<S: RegionSpawner>(&mut self, manager: &mut WorldManager<S>, cut: WorldCut) {
        let id = cut.id;
        if let Some(p) = &self.part {
            let why = format!("the part of cut {} is still undecided", p.id.0);
            log::warn!("[server] world cut {}: {why}; failed", id.0);
            self.world_failure(id, cut.tick, &why);
            manager.world_cut_failed(id, CutFailure::Store(why));
            return;
        }
        let generation = self.world.as_ref().expect("world cuts").mine();
        let tick = cut.tick;
        // The manager's cut as it is: `adopts` included (an heir's parts adopt its
        // predecessor from its `SuccessionAdopted` until the settle, R4).
        let fence = self.claims.fence();
        let reply = self.claims.store().prepare_cut(cut, (*fence).clone(), generation);
        let mut part = WorldPart::new(id, tick, Some(reply));
        part.fence = Some(fence);
        self.part = Some(part);
    }

    /// A world part failed to be staged, or could not be: counted toward the stuck report.
    fn world_failure(&mut self, id: CutId, tick: Tick, why: &str) {
        self.failed_in_a_row += 1;
        if self.failed_in_a_row == CUT_FAILURES_BEFORE_STUCK {
            log::error!(
                "[server] world saving is stuck: {CUT_FAILURES_BEFORE_STUCK} world cut parts in a row could not be staged, \
                 the last with: {why} (nothing is dropped; every cut retries what is uncommitted)"
            );
            game::perf_event!("world_save_stuck", "failed={} cut={} tick={}", self.failed_in_a_row, id.0, tick);
        }
    }

    /// The world part's prepare answer, the coordinator's `CutDone`s, and the settling.
    fn pump_world<S: RegionSpawner>(&mut self, manager: &mut WorldManager<S>) {
        let mut failure: Option<(CutId, Tick, StoreError, Option<Arc<Fence>>)> = None;
        if let Some(part) = self.part.as_mut() {
            let answer = match part.reply.as_ref().map(|r| r.try_recv()) {
                Some(Ok(answer)) => Some(answer),
                Some(Err(TryRecvError::Disconnected)) => Some(Err(StoreError::Backend("the world-store thread has stopped".into()))),
                Some(Err(TryRecvError::Empty)) | None => None,
            };
            if let Some(answer) = answer {
                part.reply = None;
                // Asked again only after `WORLD_PART_POLL` (or its `CutDone`).
                part.polled = Some(Instant::now());
                match answer {
                    Ok(staged) => {
                        part.versions = Some(staged.versions);
                        manager.world_cut_prepared(part.id);
                    }
                    Err(e) => {
                        // Only a `Backend` error (a lost lock conflict among them) may leave the
                        // part staged (its mark kept).
                        part.maybe_staged = matches!(e, StoreError::Backend(_) | StoreError::LockConflict { .. });
                        failure = Some((part.id, part.tick, e, part.fence.clone()));
                    }
                }
            }
        }
        if let Some((id, tick, e, fence)) = failure {
            let fence = fence.unwrap_or_else(|| self.claims.fence());
            if self.claims.note_refusal_under(&fence, &e) {
                log::error!("[server] world cut {} was refused as fenced; this server was replaced and stops", id.0);
            }
            if let StoreError::StaleGeneration { stored, .. } = &e {
                self.world.as_ref().expect("world cuts").note(*stored);
            }
            log::warn!("[server] world cut {}: the part was not staged: {e}", id.0);
            self.world_failure(id, tick, &e.to_string());
            manager.world_cut_failed(id, CutFailure::Store(e.to_string()));
            match &e {
                // Another cut's part is undecided on this handle: that one is settled first
                // (obligation 4), with the decision stored for it.
                StoreError::CutPending(other) => {
                    log::error!("[server] world cut {}: the store still holds part of cut {} undecided", id.0, other.0);
                    self.part = Some(WorldPart::new(*other, tick, None));
                }
                _ if !self.part.as_ref().is_some_and(|p| p.maybe_staged) => self.part = None,
                _ => {}
            }
        }
        for (cut, committed) in manager.take_world_cut_done() {
            match self.part.as_mut() {
                Some(part) if part.id == cut => {
                    part.done = Some(committed);
                    part.polled = None;
                }
                _ => {
                    log::debug!("[server] world cut {} decided ({committed}); nothing of it staged here", cut.0);
                    manager.cut_failed(cut);
                }
            }
        }
        self.settle_world(manager);
        self.warn_absent_cuts(manager);
    }

    /// No world cut committed for [`CUT_FAILURES_BEFORE_STUCK`] + 1 periods (this host's, or
    /// the gap it saw between two commits when longer): reported once per that span, naming
    /// what this host can see of the cause ([`absent_cut_warning`]) — the `hosts` rows read
    /// first, on the store thread (the loop never waits).
    fn warn_absent_cuts<S: RegionSpawner>(&mut self, manager: &WorldManager<S>) {
        let Some(period) = self.world_period else { return };
        let every = period.max(self.commit_gap.unwrap_or_default());
        let span = every * (CUT_FAILURES_BEFORE_STUCK + 1);
        if self.world_saved.elapsed() < span || self.absent_warned.is_some_and(|at| at.elapsed() < span) {
            self.absent_read = None;
            return;
        }
        let rows = match self.absent_read.as_ref().map(|rx| rx.try_recv()) {
            None => {
                self.absent_read = Some(self.claims.store().hosts_later());
                return;
            }
            Some(Err(TryRecvError::Empty)) => return,
            Some(Ok(Ok(rows))) => rows,
            // Unread: warned without the rows.
            Some(Ok(Err(_)) | Err(TryRecvError::Disconnected)) => Vec::new(),
        };
        self.absent_read = None;
        self.absent_warned = Some(Instant::now());
        let me = self.claims.fence().owner;
        let cause = AbsentCause {
            coordinator_detached: !manager.world_attached(),
            successions: manager.successions().into_iter().map(|(id, _)| id.0).collect(),
            dirty_unattached: rows
                .iter()
                .filter(|r| r.id != me && !r.clean && r.state == world_store::HOST_LIVE)
                .filter(|r| !manager.peer_is_reachable(r.id))
                .map(|r| r.id)
                .collect(),
        };
        log::error!("{}", absent_cut_warning(self.world_saved.elapsed(), every, &cause));
    }

    /// Settle the world part once it is decided: the store first (`cut_decided`, which
    /// finishes the part and clears the handle's pending mark), then the manager. Told it
    /// is aborted (or past the deadline) while the store has no decision: this host
    /// records the abort itself (`abort_cut`), then settles.
    fn settle_world<S: RegionSpawner>(&mut self, manager: &mut WorldManager<S>) {
        let Some(part) = self.part.as_mut() else { return };
        if part.reply.is_some() {
            return;
        }
        if let Some(rx) = &part.aborting {
            let failed = match rx.try_recv() {
                Err(TryRecvError::Empty) => return,
                Ok(Ok(committed)) => {
                    log::info!("[server] world cut {}: decided {committed} (this host's abort, or an earlier decision)", part.id.0);
                    None
                }
                Ok(Err(e)) => Some(e.to_string()),
                Err(TryRecvError::Disconnected) => Some("the world-store thread has stopped".into()),
            };
            part.aborting = None;
            match failed {
                // Recorded: read the decision at once.
                None => part.polled = None,
                // Not recorded: asked again after a poll interval, warned at most once per
                // `WORLD_PART_WARN`.
                Some(e) => {
                    part.polled = Some(Instant::now());
                    let id = part.id;
                    if self.warned.is_none_or(|at| at.elapsed() >= WORLD_PART_WARN) {
                        self.warned = Some(Instant::now());
                        log::warn!("[server] world cut {}: this host's abort could not be recorded ({e}); trying again", id.0);
                    }
                    return;
                }
            }
        }
        if let Some(rx) = &part.deciding {
            let answer = match rx.try_recv() {
                Err(TryRecvError::Empty) => return,
                Ok(answer) => answer,
                Err(TryRecvError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
            };
            part.deciding = None;
            let id = part.id;
            match answer {
                Ok(Some(true)) => {
                    let versions = part.versions.take().unwrap_or_else(|| {
                        log::warn!("[server] world cut {} committed with this host's part, whose prepare answer was lost", id.0);
                        Vec::new()
                    });
                    self.part = None;
                    self.failed_in_a_row = 0;
                    self.world_saved = Instant::now();
                    if let Some(prev) = self.last_commit.replace(Instant::now()) {
                        self.commit_gap = Some(prev.elapsed());
                    }
                    game::perf_event!("world_saved", "cut={} world=1", id.0);
                    crate::health::Health::global().world_saved();
                    manager.cut_committed(id, versions);
                }
                Ok(Some(false)) => {
                    self.part = None;
                    manager.cut_failed(id);
                }
                Ok(None) => {
                    // Undecided in the store. Told it is aborted, or nothing heard past the
                    // deadline: abort it here (the first decision written wins).
                    let late = part.since.elapsed() >= self.part_deadline;
                    if part.done == Some(false) || late {
                        if self.warned.is_none_or(|at| at.elapsed() >= WORLD_PART_WARN) {
                            self.warned = Some(Instant::now());
                            log::warn!("[server] world cut {}: no decision stored; this host aborts it", id.0);
                        }
                        part.aborting = Some(self.claims.store().abort_cut_later(id, part.tick));
                    } else if part.done == Some(true) {
                        log::error!("[server] world cut {}: told committed, the store has no decision; asking again", id.0);
                    }
                }
                Err(e) => {
                    if self.warned.is_none_or(|at| at.elapsed() >= WORLD_PART_WARN) {
                        self.warned = Some(Instant::now());
                        log::warn!("[server] world cut {}: its decision could not be read ({e}); asking again", id.0);
                    }
                }
            }
            return;
        }
        if part.polled.is_none_or(|at| at.elapsed() >= WORLD_PART_POLL) {
            part.polled = Some(Instant::now());
            part.deciding = Some(self.claims.store().cut_decided_later(part.id));
        }
    }

    fn settle<S: RegionSpawner>(&mut self, manager: &mut WorldManager<S>, p: Pending, answer: Result<CutCommitted, StoreError>) {
        if let Err(e) = &answer {
            if self.claims.note_refusal_under(&p.fence, e) {
                log::error!("[server] save cut {} was refused: {e}; this server was replaced and stops", p.id.0);
            }
        }
        match &answer {
            Ok(_) => {
                self.failed_in_a_row = 0;
                // After the commit, never inside it; not waited for (a lost notice is
                // covered by the listeners' poll).
                let _ = self.claims.store().notify_cut_later(p.id, p.tick);
            }
            Err(e) => {
                self.failed_in_a_row += 1;
                if self.failed_in_a_row == CUT_FAILURES_BEFORE_STUCK {
                    log::error!(
                        "[server] world saving is stuck: {CUT_FAILURES_BEFORE_STUCK} save cuts in a row failed, \
                         the last with: {e} (nothing is dropped; every cut retries what is uncommitted)"
                    );
                    game::perf_event!("world_save_stuck", "failed={} cut={} tick={}", self.failed_in_a_row, p.id.0, p.tick);
                }
            }
        }
        settle(manager, p, answer);
    }
}

/// The orderly exit's park: every player, then every region, parked, the
/// region outputs handled as they come, for at most `timeout` (wall time —
/// the world tick need not move at exit). `idle` runs every round (the
/// server's region threads need nothing; an inline spawner is pumped there).
/// A region still not parked at the deadline is given up
/// ([`WorldManager::give_up_unparked`]): the exit's cut saves the rest.
/// Returns the regions given up.
pub fn park_for_exit<S: RegionSpawner>(
    manager: &mut WorldManager<S>,
    region_out: &Receiver<(RegionCoords, RegionOutput)>,
    now_ms: impl Fn() -> u64,
    timeout: Duration,
    mut idle: impl FnMut(&mut WorldManager<S>),
) -> Vec<RegionCoords> {
    manager.park_all_players(now_ms());
    manager.shutdown_all();
    let deadline = Instant::now() + timeout;
    while !manager.running_regions().is_empty() && Instant::now() < deadline {
        idle(manager);
        if let Ok((rc, output)) = region_out.recv_timeout(EXIT_PARK_POLL) {
            manager.handle_region_output(rc, output, now_ms());
            while let Ok((rc, output)) = region_out.try_recv() {
                manager.handle_region_output(rc, output, now_ms());
            }
        }
        // A park queued for a region that was mid-stop woke it again.
        manager.shutdown_all();
    }
    if manager.running_regions().is_empty() {
        return Vec::new();
    }
    let stuck = manager.give_up_unparked();
    log::error!(
        "[server] {} regions did not park within {timeout:?} and are saved as of their last snapshot (if any): {stuck:?}",
        stuck.len()
    );
    stuck
}

/// The exit of a host of several (world cuts): its players park, it asks for a leave cut
/// and waits — the regions' outputs, the upkeep (peers, coordinator) and the cut writer
/// running, `idle` every round — until that cut is settled committed; an aborted one is
/// asked for again while the leave timeout (from the first request) lasts, and past it, or
/// with no `BeginCut` within it, the host exits without a cut (see the module docs). A fence or a newer world generation seen
/// meanwhile ends it as in [`exit_world`].
#[allow(clippy::too_many_arguments)]
fn leave_world<S: RegionSpawner>(
    manager: &mut WorldManager<S>,
    mut cuts: CutWriter,
    region_out: &Receiver<(RegionCoords, RegionOutput)>,
    now_ms: &impl Fn() -> u64,
    fenced: &Fenced,
    world_generation: &WorldGeneration,
    save_timeout: Duration,
    mut idle: impl FnMut(&mut WorldManager<S>),
) -> ServerExit {
    // The players park at once (as an exit that saves on its own parks them first): their
    // clients' input is no longer read. Their records are in the leave cut (or, before it,
    // in any cut, as every park's are).
    // Told why once the leave cut is committed (`LEAVING_REFUSAL`): parking ends them here.
    let clients = manager.session_ids();
    manager.park_all_players(now_ms());
    manager.request_leave();
    let asked = Instant::now();
    let mut begun: Option<Instant> = None;
    loop {
        idle(manager);
        if let Ok((rc, output)) = region_out.recv_timeout(EXIT_PARK_POLL) {
            manager.handle_region_output(rc, output, now_ms());
            while let Ok((rc, output)) = region_out.try_recv() {
                manager.handle_region_output(rc, output, now_ms());
            }
        }
        manager.maintain(now_ms());
        if let Some(generation) = manager.take_higher_generation() {
            world_generation.note(generation);
        }
        if manager.leave_parked() {
            // A region an arrival woke parks again.
            manager.shutdown_all();
        }
        match manager.leaving_cut() {
            Some(_) => {
                begun.get_or_insert_with(Instant::now);
            }
            None => begun = None,
        }
        cuts.pump(manager);
        if fenced.is_set() || cuts.fenced().is_set() {
            manager.refuse_sessions(&clients, FENCED_REFUSAL);
            cuts.finish_without_cut(manager);
            log::error!("[server] fenced while leaving: nothing more is saved");
            return ServerExit::Fenced;
        }
        if world_generation.is_set() {
            manager.refuse_sessions(&clients, ROLLBACK_REFUSAL);
            cuts.finish_without_cut(manager);
            log::error!("[server] world rolled back while leaving: another host restarted dirty; nothing more is saved");
            return ServerExit::WorldRolledBack;
        }
        let settled = cuts.world_part().is_none();
        match manager.leave_outcome() {
            Some(true) if settled => {
                log::info!("[server] left the world in cut {}: saved", manager.leaving_cut().map_or(0, |c| c.0));
                // Only now, the cut committed with every player parked in it: a client told
                // earlier could rejoin elsewhere before its record was saved (lost progress).
                manager.refuse_sessions(&clients, LEAVING_REFUSAL);
                cuts.finish_without_cut(manager);
                return ServerExit::Clean;
            }
            // Aborted (or this host could not take part): asked again while the leave
            // timeout (from the first request) lasts, once the coordinator's `CutDone` came.
            Some(false) if settled && asked.elapsed() < cuts.leave_timeout => {
                if manager.retry_leave() {
                    begun = None;
                }
            }
            Some(false) if settled => {
                log::error!("[server] the leave cut was aborted: {LEFT_WITHOUT_A_CUT}");
                // Stopping either way: the client rejoins the same way.
                manager.refuse_sessions(&clients, LEAVING_REFUSAL);
                cuts.finish_without_cut(manager);
                return ServerExit::Clean;
            }
            _ => {}
        }
        let late = match begun {
            None => asked.elapsed() >= cuts.leave_timeout,
            // Named, never decided (the coordinator gone): past the cut's deadline and a save.
            Some(at) => at.elapsed() >= Duration::from_millis(game::WORLD_CUT_DEADLINE_MS) + save_timeout,
        };
        if late {
            let what = if begun.is_some() { "the leave cut was never decided" } else { "no leave cut began" };
            log::error!("[server] {what}: {LEFT_WITHOUT_A_CUT}");
            manager.refuse_sessions(&clients, LEAVING_REFUSAL);
            cuts.finish_without_cut(manager);
            return ServerExit::Clean;
        }
    }
}

/// What [`exit_world`] works with.
pub struct ExitParams<'a, NowMs, NowTick> {
    /// The region threads' outputs (their parks).
    pub region_out: &'a Receiver<(RegionCoords, RegionOutput)>,
    /// Wall ms on the manager's clock.
    pub now_ms: NowMs,
    /// The host's save cuts; `None` when nothing is saved (procedural worlds).
    pub cuts: Option<CutWriter>,
    /// The in-process world coordinator (`AllInOne`); `None` for a host whose
    /// coordinator is another process, or a world without one.
    pub coordinator: Option<CoordinatorThread>,
    /// The process's fenced flag.
    pub fenced: &'a Fenced,
    /// The process's world generation: a higher one seen than its own is a rollback exit.
    pub world_generation: &'a WorldGeneration,
    /// How long the regions may take to park ([`EXIT_PARK_TIMEOUT`]).
    pub park_timeout: Duration,
    /// How long each last save may take.
    pub save_timeout: Duration,
    /// The world tick the exit's cut is taken at.
    pub now_tick: NowTick,
}

/// The orderly exit, in one place: a fenced process saves nothing and parks
/// nothing (its clients are refused, [`FENCED_REFUSAL`]; the coordinator stops
/// without a last save); otherwise players and regions park ([`park_for_exit`]),
/// the world coordinator (`AllInOne`: the thread in `coordinator`) handles their
/// parks and saves its state once more, and the host's last cut is committed —
/// unless a region was given up: its state is only as of its last snapshot, and
/// a cut written without it could be inconsistent with what it left in flight,
/// so the world stays as of the last periodic cut (the coordinator's save is
/// still taken: history ahead of the regions is safe, R2b Decision 7). The
/// coordinator saves before the host's cut because the cut writer shuts the
/// shared store down; both come after every park, so neither holds anything
/// the other lacks. A coordinator save refused as fenced makes the exit fenced; a
/// coordinator thread that failed (panicked) makes it
/// [`ServerExit::CoordinatorFailed`] with no exit cut. A process whose world rolled back to
/// an earlier generation ([`WorldGeneration`]) is treated like a fenced one (clients refused with
/// [`ROLLBACK_REFUSAL`]); fenced wins when both are set. A host of several (world cuts)
/// leaves instead of saving on its own ([`leave_world`], the module docs).
pub fn exit_world<S: RegionSpawner>(
    manager: &mut WorldManager<S>,
    params: ExitParams<'_, impl Fn() -> u64, impl Fn() -> Tick>,
    idle: impl FnMut(&mut WorldManager<S>),
) -> ServerExit {
    let ExitParams { region_out, now_ms, mut cuts, coordinator, fenced, world_generation, park_timeout, save_timeout, now_tick } = params;
    let fenced_now = fenced.is_set() || cuts.as_ref().is_some_and(|c| c.fenced().is_set());
    if fenced_now {
        manager.refuse_all_sessions(FENCED_REFUSAL);
        if let Some(coordinator) = coordinator {
            coordinator.stop_without_save();
        }
        match cuts {
            Some(cuts) => cuts.finish_without_cut(manager),
            None => {}
        }
        log::error!("[server] fenced: another process owns this server's super-regions; nothing more is saved");
        return ServerExit::Fenced;
    }
    if world_generation.is_set() {
        // Another host rebooted from a crash: the world restarts from its last committed saves,
        // so nothing of this process's is saved (a cut now would be ahead of that save).
        manager.refuse_all_sessions(ROLLBACK_REFUSAL);
        if let Some(coordinator) = coordinator {
            coordinator.stop_without_save();
        }
        if let Some(cuts) = cuts {
            cuts.finish_without_cut(manager);
        }
        log::error!("[server] world rolled back: another host restarted dirty; nothing more is saved");
        return ServerExit::WorldRolledBack;
    }
    if let Some(cuts) = cuts.take_if(|c| c.world_cuts()) {
        if let Some(coordinator) = coordinator {
            // A host of several has no coordinator of its own.
            log::error!("[server] a host of several with an in-process coordinator: it stops without a save");
            coordinator.stop_without_save();
        }
        return leave_world(manager, cuts, region_out, &now_ms, fenced, world_generation, save_timeout, idle);
    }
    // A refusal after the parks below reaches the sessions they ended only by id.
    let clients = manager.session_ids();
    let given_up = park_for_exit(manager, region_out, now_ms, park_timeout, idle);
    let coordinator_exit = coordinator.map_or(ServerExit::Clean, |c| c.shutdown(save_timeout));
    if coordinator_exit == ServerExit::CoordinatorFailed {
        // Its last save may lack parks and crossings it handled: a cut holding them
        // would put the regions ahead of a history that never heard of them.
        log::error!(
            "[server] the world coordinator failed: no exit save; the world restarts from its last committed saves"
        );
        if let Some(cuts) = cuts {
            cuts.finish_without_cut(manager);
        }
        return ServerExit::CoordinatorFailed;
    }
    if coordinator_exit == ServerExit::Fenced || fenced.is_set() {
        manager.refuse_sessions(&clients, FENCED_REFUSAL);
        if let Some(cuts) = cuts {
            cuts.finish_without_cut(manager);
        }
        log::error!("[server] fenced: another process took over this world's coordinator; nothing more is saved");
        return ServerExit::Fenced;
    }
    let Some(mut cuts) = cuts else { return ServerExit::Clean };
    if !given_up.is_empty() {
        log::error!(
            "[server] no exit save: {} region(s) did not park ({given_up:?}); the world restarts from its last committed save",
            given_up.len()
        );
        cuts.finish_without_cut(manager);
        return ServerExit::Clean;
    }
    // A fence refusal while parking (a cut answered late) still counts.
    cuts.pump(manager);
    if cuts.fenced().is_set() {
        manager.refuse_sessions(&clients, FENCED_REFUSAL);
        cuts.finish_without_cut(manager);
        return ServerExit::Fenced;
    }
    let fenced = cuts.fenced().clone();
    if cuts.finish(manager, now_tick(), save_timeout) {
        log::info!("[server] world saved");
    } else if fenced.is_set() {
        manager.refuse_sessions(&clients, FENCED_REFUSAL);
        return ServerExit::Fenced;
    } else {
        log::error!("[server] the world was not saved at exit; it restarts from its last committed save");
    }
    ServerExit::Clean
}

fn settle<S: RegionSpawner>(manager: &mut WorldManager<S>, p: Pending, answer: Result<CutCommitted, StoreError>) {
    match answer {
        Ok(done) => {
            game::perf_event!(
                "world_saved",
                "cut={} tick={} regions={} players={} ms={}",
                p.id.0,
                p.tick,
                p.regions,
                p.players,
                game::perf::ms(p.queued.elapsed())
            );
            crate::health::Health::global().world_saved();
            manager.cut_committed(done.id, done.versions);
        }
        Err(e) => {
            log::error!("[server] save cut {} at tick {} was not committed: {e} (the next cut carries it again)", p.id.0, p.tick);
            manager.cut_failed(p.id);
        }
    }
}

/// At most one warning per this long about failing reads of the stored regions' versions.
pub const TERRAIN_WATCH_WARN: Duration = Duration::from_secs(30);

/// What a [`TerrainWatch`] has still to read: the regions saved since a cut's tick, or all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerrainRead {
    Since(Tick),
    All,
}

impl TerrainRead {
    /// One read covering both.
    fn merge(self, other: TerrainRead) -> TerrainRead {
        match (self, other) {
            (TerrainRead::Since(a), TerrainRead::Since(b)) => TerrainRead::Since(a.min(b)),
            _ => TerrainRead::All,
        }
    }
}

/// A host's view of the other hosts' saves (several hosts): the store's cut notices
/// ([`StoreHandle::listen_cuts`], its own `terrain-listener` thread) become reads of the
/// stored regions' terrain versions — `stored_regions_since(tick)` for a `Cut`,
/// `stored_regions()` for a `Resync` — answered on receivers (the main loop never waits;
/// one read at a time, later notices merged into the next), and each answer goes to
/// `WorldManager::stored_terrain_changed`. A failed read is warned about and left to the
/// next notice or the poll's `Resync` ([`world_store::TERRAIN_POLL_SECS`]).
pub struct TerrainWatch {
    store: StoreHandle,
    notices: Receiver<CutNotice>,
    reading: Option<Receiver<Result<BTreeMap<RegionCoords, u64>, StoreError>>>,
    next: Option<TerrainRead>,
    warned: Option<Instant>,
}

impl TerrainWatch {
    /// Starts listening on `store` in `mode` (`VOXEL_TERRAIN_NOTIFY`).
    pub fn start(store: StoreHandle, mode: NotifyMode) -> TerrainWatch {
        let notices = store.listen_cuts(mode);
        TerrainWatch { store, notices, reading: None, next: None, warned: None }
    }

    /// Takes the notices that came, hands an answered read to the manager, and starts the
    /// next read. Never blocks.
    pub fn pump<S: RegionSpawner>(&mut self, manager: &mut WorldManager<S>) {
        while let Ok(notice) = self.notices.try_recv() {
            let read = match notice {
                CutNotice::Cut { tick } => TerrainRead::Since(tick),
                CutNotice::Resync => TerrainRead::All,
            };
            self.next = Some(self.next.map_or(read, |n| n.merge(read)));
        }
        if let Some(rx) = &self.reading {
            let answer = match rx.try_recv() {
                Err(TryRecvError::Empty) => return,
                Ok(answer) => answer,
                Err(TryRecvError::Disconnected) => Err(StoreError::Backend("the world-store thread has stopped".into())),
            };
            self.reading = None;
            match answer {
                Ok(versions) => manager.stored_terrain_changed(versions),
                Err(e) => {
                    if self.warned.is_none_or(|at| at.elapsed() >= TERRAIN_WATCH_WARN) {
                        self.warned = Some(Instant::now());
                        log::warn!("[server] reading the stored regions' versions failed ({e}); the next notice or poll reads them again");
                    }
                }
            }
        }
        if let Some(read) = self.next.take() {
            self.reading = Some(match read {
                TerrainRead::Since(tick) => self.store.stored_regions_since_later(tick),
                TerrainRead::All => self.store.stored_regions_later(),
            });
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

    /// The host's absent-cut warning names each cause it can see, apart: the coordinator not
    /// attached, a succession running, a host with a dirty row that is not attached — and
    /// says when it sees none.
    #[test]
    fn absent_cut_warning_names_its_cause() {
        let (stalled, every) = (Duration::from_secs(240), Duration::from_secs(60));
        let detached = absent_cut_warning(stalled, every, &AbsentCause { coordinator_detached: true, ..AbsentCause::default() });
        let succession = absent_cut_warning(stalled, every, &AbsentCause { successions: vec![4], ..AbsentCause::default() });
        let dirty = absent_cut_warning(stalled, every, &AbsentCause { dirty_unattached: vec![h(2)], ..AbsentCause::default() });
        let unknown = absent_cut_warning(stalled, every, &AbsentCause::default());
        for w in [&detached, &succession, &dirty, &unknown] {
            assert!(w.contains("no world cut has committed in 240 s") && w.contains("about every 60 s"), "{w}");
        }
        assert!(detached.contains("the coordinator is not attached"), "{detached}");
        assert!(succession.contains("a succession is running") && succession.contains("[4]"), "{succession}");
        assert!(dirty.contains("host 2 has a dirty row and is not attached") && dirty.contains("worldctl retire-host <url> 2"), "{dirty}");
        assert!(unknown.contains("the coordinator's log says why"), "{unknown}");
        // Each names only its own cause.
        let causes = ["the coordinator is not attached", "a succession is running", "has a dirty row and is not attached"];
        for (w, own) in [(&detached, 0), (&succession, 1), (&dirty, 2)] {
            for (i, c) in causes.iter().enumerate() {
                assert_eq!(w.contains(c), i == own, "{w}");
            }
            assert!(!w.contains("log says why"), "{w}");
        }
    }
}
