//! Writing the `WorldManager`'s save cuts to the world store: every complete
//! cut is queued for commit at once (`StoreHandle::commit_cut` does not
//! block), and each answer goes back to the manager — `cut_committed` clears
//! what it carried, `cut_failed` keeps it for the next cut (nothing is lost).

use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, TryRecvError};
use game::{CutId, RegionSpawner, Tick, WorldManager};
use world_store::{CutCommitted, StoreError, StoreHandle};

struct Pending {
    id: CutId,
    tick: Tick,
    regions: usize,
    players: usize,
    queued: Instant,
    reply: Receiver<Result<CutCommitted, StoreError>>,
}

/// The server's side of the save cuts: the store and the commits in flight.
pub struct CutWriter {
    store: StoreHandle,
    pending: Vec<Pending>,
}

impl CutWriter {
    pub fn new(store: StoreHandle) -> Self {
        CutWriter { store, pending: Vec::new() }
    }

    pub fn store(&self) -> &StoreHandle {
        &self.store
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
            let reply = self.store.commit_cut(cut);
            self.pending.push(Pending { id, tick, regions, players, queued: Instant::now(), reply });
        }
        let mut i = 0;
        while i < self.pending.len() {
            match self.pending[i].reply.try_recv() {
                Err(TryRecvError::Empty) => i += 1,
                Ok(answer) => {
                    let p = self.pending.remove(i);
                    settle(manager, p, answer);
                }
                Err(TryRecvError::Disconnected) => {
                    let p = self.pending.remove(i);
                    settle(manager, p, Err(StoreError::Backend("the world-store thread has stopped".into())));
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
            settle(manager, p, answer);
            if Some(id) == last {
                committed = ok;
            }
        }
        // Commits still queued are drained before the store thread stops.
        self.store.shutdown();
        committed
    }
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
