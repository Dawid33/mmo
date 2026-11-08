//! Writing the `WorldManager`'s save cuts to the world store: every complete
//! cut is queued for commit at once (`StoreHandle::commit_cut` does not
//! block), and each answer goes back to the manager — `cut_committed` clears
//! what it carried, `cut_failed` keeps it for the next cut (nothing is lost).
//! Commits are fenced ([`HostClaims`]): a refusal as fenced makes the whole
//! process fenced (`Fenced`), and nothing more is written.

use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, TryRecvError};
use game::{CutId, RegionCoords, RegionOutput, RegionSpawner, Tick, WorldManager};
use world_store::{CutCommitted, StoreError, StoreHandle};

use crate::claims::{Fenced, HostClaims, ServerExit, FENCED_REFUSAL};

struct Pending {
    id: CutId,
    tick: Tick,
    regions: usize,
    players: usize,
    queued: Instant,
    reply: Receiver<Result<CutCommitted, StoreError>>,
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

/// The server's side of the save cuts: the store and the commits in flight.
pub struct CutWriter {
    claims: HostClaims,
    pending: Vec<Pending>,
    /// Commits failed in a row (reset by one that commits).
    failed_in_a_row: u32,
}

impl CutWriter {
    pub fn new(claims: HostClaims) -> Self {
        CutWriter { claims, pending: Vec::new(), failed_in_a_row: 0 }
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
            let (id, tick, regions, players) = (cut.id, cut.tick, cut.regions.len(), cut.players.len());
            let reply = self.claims.commit_cut(cut);
            self.pending.push(Pending { id, tick, regions, players, queued: Instant::now(), reply });
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

    fn settle<S: RegionSpawner>(&mut self, manager: &mut WorldManager<S>, p: Pending, answer: Result<CutCommitted, StoreError>) {
        if let Err(e) = &answer {
            if self.claims.fenced().note(e) {
                log::error!("[server] save cut {} was refused: {e}; this server was replaced and stops", p.id.0);
            }
        }
        match &answer {
            Ok(_) => self.failed_in_a_row = 0,
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

/// The orderly exit, in one place: a fenced process saves nothing and parks
/// nothing (its clients are refused, [`FENCED_REFUSAL`]); otherwise players
/// and regions park ([`park_for_exit`]) and the last cut is committed — unless
/// a region was given up: its state is only as of its last snapshot, and a
/// cut written without it could be inconsistent with what it left in flight,
/// so the world stays as of the last periodic cut. `cuts` is `None` when
/// nothing is saved (procedural worlds).
pub fn exit_world<S: RegionSpawner>(
    manager: &mut WorldManager<S>,
    region_out: &Receiver<(RegionCoords, RegionOutput)>,
    now_ms: impl Fn() -> u64,
    cuts: Option<CutWriter>,
    fenced: &Fenced,
    park_timeout: Duration,
    save_timeout: Duration,
    now_tick: impl Fn() -> Tick,
    idle: impl FnMut(&mut WorldManager<S>),
) -> ServerExit {
    let fenced_now = fenced.is_set() || cuts.as_ref().is_some_and(|c| c.fenced().is_set());
    if fenced_now {
        manager.refuse_all_sessions(FENCED_REFUSAL);
        match cuts {
            Some(cuts) => cuts.finish_without_cut(manager),
            None => {}
        }
        log::error!("[server] fenced: another process owns this server's super-regions; nothing more is saved");
        return ServerExit::Fenced;
    }
    let given_up = park_for_exit(manager, region_out, now_ms, park_timeout, idle);
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
        cuts.finish_without_cut(manager);
        return ServerExit::Fenced;
    }
    let fenced = cuts.fenced().clone();
    if cuts.finish(manager, now_tick(), save_timeout) {
        log::info!("[server] world saved");
    } else if fenced.is_set() {
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
            manager.cut_committed(done.id, done.versions);
        }
        Err(e) => {
            log::error!("[server] save cut {} at tick {} was not committed: {e} (the next cut carries it again)", p.id.0, p.tick);
            manager.cut_failed(p.id);
        }
    }
}
