//! [`StoreHandle`]: how every other thread reaches the `world-store` thread.
//! Cloneable and `Send + Sync`; its methods block on the answer unless noted.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use game::{
    CutId, InFlightRow, PlayerId, PlayerRecord, RegionCoords, StoredRegion, TerrainSnapshot, Tick,
    WorldCut,
};
use tokio::sync::{mpsc, oneshot};

use crate::counters::{Counters, StoreCounters};
use crate::thread::{self, Reply, Request, Stamped};
use crate::{CoordinatorFence, Dials, Fence, OpenMode, StoreError, StoreUrl, SuperRegion};

/// The world row and whether a forged artifact is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldInfo {
    /// The stored world's content crc (`None` when there is no artifact).
    pub content_crc: Option<u32>,
    /// The tick of the last committed cut (0 for a new world).
    pub world_tick: Tick,
    pub has_artifact: bool,
}

/// A cut that is in the database, with each of its regions' `terrain_version`
/// after it (in the cut's region order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutCommitted {
    pub id: CutId,
    pub versions: Vec<(RegionCoords, u64)>,
}

/// One host's row in `hosts` (R3). `boot` is a random per-process-start number, stored
/// as BIGINT by a bit cast (`as i64` / `as u64`, like every u64 of the store).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRow {
    pub ordinal: u32,
    pub boot: u64,
    /// The world generation this boot runs at (set by [`StoreHandle::register_host`]).
    pub generation: u64,
    /// The host's last save cut left it clean (cleared by every boot).
    pub clean: bool,
    pub peer_addr: Option<String>,
    pub peer_cert: Option<String>,
    pub quic: Option<String>,
    pub webtransport: Option<String>,
    pub wt_cert_hash: Option<String>,
}

/// What [`StoreHandle::register_host`] decided: the generation this boot runs at, and
/// whether the boot raised the world's (its previous boot died without a clean save,
/// so every other host must restart from the same save).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostBoot {
    pub generation: u64,
    pub bumped: bool,
}

/// A handle on the open store (see the crate docs and `thread.rs`).
#[derive(Clone)]
pub struct StoreHandle {
    tx: mpsc::UnboundedSender<Stamped>,
    join: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// `world.super_region_regions`, read at open.
    super_region_regions: u32,
    /// Test hook ([`StoreHandle::fail_next_writes`]): queued writes still to fail.
    fail_writes: Arc<AtomicU32>,
    /// Test hook ([`StoreHandle::stop_after_commit_prepared`]): the next decision's stop.
    stop_commits: Arc<Mutex<Option<u32>>>,
    /// Test hook ([`StoreHandle::fail_next_decisions`]): decisions still to fail.
    fail_decisions: Arc<AtomicU32>,
    /// The pending prepared part and the cut lock timeout, shared with the store thread.
    cuts: Arc<thread::CutState>,
    /// Where the store is: what [`StoreHandle::listen_cuts`] listens on (Postgres).
    url: StoreUrl,
    /// [`StoreHandle::listen_cuts`]' poll period in ms (test setter:
    /// [`StoreHandle::set_terrain_poll_period`]).
    poll_ms: Arc<AtomicU64>,
    /// Wait and codec-CPU counters of the store thread, shared with it.
    counters: Arc<Counters>,
}

fn stopped() -> StoreError {
    StoreError::Backend("the world-store thread has stopped".into())
}

impl StoreHandle {
    /// Opens the store at `url` on a new `world-store` thread; returns once it
    /// is open (a [`OpenMode::Writer`] creates a missing store and migrates it).
    pub fn open(url: &StoreUrl, dials: Dials, mode: OpenMode) -> Result<StoreHandle, StoreError> {
        let cuts = Arc::new(thread::CutState::new());
        let counters = Arc::new(Counters::default());
        let (tx, join, super_region_regions) = thread::spawn(url, dials, mode, cuts.clone(), counters.clone())?;
        Ok(StoreHandle {
            tx,
            join: Arc::new(Mutex::new(Some(join))),
            super_region_regions,
            fail_writes: Arc::new(AtomicU32::new(0)),
            stop_commits: Arc::new(Mutex::new(None)),
            fail_decisions: Arc::new(AtomicU32::new(0)),
            cuts,
            url: url.clone(),
            poll_ms: crate::listen::default_poll_ms(),
            counters,
        })
    }

    /// Test hook: the next `n` queued writes — cut commits ([`StoreHandle::commit_cut`])
    /// and coordinator saves ([`StoreHandle::save_coordinator`]), whichever come first —
    /// fail with a `Backend` error (not `Fenced`) and write nothing. Shared by every clone.
    #[doc(hidden)]
    pub fn fail_next_writes(&self, n: u32) {
        self.fail_writes.store(n, Ordering::SeqCst);
    }

    /// Test hook: writes [`StoreHandle::fail_next_writes`] has still to fail.
    #[doc(hidden)]
    pub fn failing_writes_left(&self) -> u32 {
        self.fail_writes.load(Ordering::SeqCst)
    }

    /// Test hook: the next [`StoreHandle::decide_cut`] (of any clone) stops after `n`
    /// `COMMIT PREPARED`s and returns `Backend("test stop")`, as a coordinator that
    /// dies between them would leave the cut.
    #[doc(hidden)]
    pub fn stop_after_commit_prepared(&self, n: u32) {
        *self.stop_commits.lock().unwrap_or_else(|e| e.into_inner()) = Some(n);
    }

    /// Test hook: the next `n` cut decisions — [`StoreHandle::decide_cut`] and
    /// [`StoreHandle::abort_cut`] (and their `_later` forms), whichever come first — fail
    /// with a `Backend` error and record nothing. Shared by every clone.
    #[doc(hidden)]
    pub fn fail_next_decisions(&self, n: u32) {
        self.fail_decisions.store(n, Ordering::SeqCst);
    }

    /// Test hook: decisions [`StoreHandle::fail_next_decisions`] has still to fail.
    #[doc(hidden)]
    pub fn fail_next_decisions_left(&self) -> u32 {
        self.fail_decisions.load(Ordering::SeqCst)
    }

    /// Takes one of [`StoreHandle::fail_next_decisions`]' failures, if any are left.
    fn injected_decision_failure(&self) -> Option<StoreError> {
        self.fail_decisions
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .ok()
            .map(|_| StoreError::Backend("injected decision failure (StoreHandle::fail_next_decisions)".into()))
    }

    /// Test hook: the `lock_timeout` of this handle's later cut transactions, in ms
    /// (default [`crate::CUT_LOCK_TIMEOUT_MS`]). Shared by every clone.
    #[doc(hidden)]
    pub fn set_cut_lock_timeout_ms(&self, ms: u64) {
        self.cuts.lock_timeout_ms.store(ms, Ordering::SeqCst);
    }

    /// How many committed world cuts this handle (and its clones) decided were found to
    /// hold an entity stored in two regions (each reported once with `error!`).
    pub fn cuts_with_entities_twice(&self) -> u64 {
        self.cuts.entities_twice_cuts.load(Ordering::SeqCst)
    }

    /// Test hook: how many committed world cuts this handle decided have had that check
    /// run (it runs after the decision is answered).
    #[doc(hidden)]
    pub fn cuts_checked_for_entities_twice(&self) -> u64 {
        self.cuts.entities_checked_cuts.load(Ordering::SeqCst)
    }

    /// Takes one of [`StoreHandle::fail_next_writes`]' failures, if any are left.
    fn injected_failure(&self) -> Option<StoreError> {
        self.fail_writes
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .ok()
            .map(|_| StoreError::Backend("injected write failure (StoreHandle::fail_next_writes)".into()))
    }

    /// Regions per super-region side (`world.super_region_regions`): what
    /// `game::super_region_of` takes for this store's fences.
    pub fn super_region_regions(&self) -> u32 {
        self.super_region_regions
    }

    /// Counters of the store thread since the last call, reset by it: requests picked
    /// up, their wait from being sent to being picked up, and the CPU the thread spent
    /// in encode / decode ([`StoreCounters`]). Shared by every clone of the handle.
    pub fn take_counters(&self) -> StoreCounters {
        self.counters.take()
    }

    /// Sends a request stamped with now; a stopped thread gives it back.
    fn send(&self, req: Request) -> Result<(), Request> {
        self.tx.send(Stamped { sent: std::time::Instant::now(), req }).map_err(|e| e.0.req)
    }

    fn ask<T>(&self, make: impl FnOnce(Reply<T>) -> Request) -> Result<T, StoreError> {
        if thread::on_store_thread() {
            // Its answer would come from this very thread: refuse rather than hang.
            return Err(StoreError::Backend(
                "a blocking world-store call was made on the world-store thread itself".into(),
            ));
        }
        let (tx, rx) = crossbeam::channel::bounded(1);
        self.send(make(Reply::sync(tx))).map_err(|_| stopped())?;
        rx.recv().map_err(|_| stopped())?
    }

    /// [`Self::ask`] without waiting: the answer arrives on the receiver (a stopped store
    /// thread: the receiver disconnects).
    fn ask_later<T>(&self, make: impl FnOnce(Reply<T>) -> Request) -> crossbeam::channel::Receiver<Result<T, StoreError>> {
        let (tx, rx) = crossbeam::channel::bounded(1);
        let _ = self.send(make(Reply::sync(tx)));
        rx
    }

    /// An answer already known, on a receiver.
    fn answered<T>(v: Result<T, StoreError>) -> crossbeam::channel::Receiver<Result<T, StoreError>> {
        let (tx, rx) = crossbeam::channel::bounded(1);
        let _ = tx.send(v);
        rx
    }

    pub fn world_info(&self) -> Result<WorldInfo, StoreError> {
        self.ask(Request::WorldInfo)
    }

    /// The original forge file of the stored forged world (re-tiling, export);
    /// hosts never read it.
    pub fn forged_archive(&self) -> Result<Option<Vec<u8>>, StoreError> {
        self.ask(Request::ForgedArchive)
    }

    /// Writes the tiled forged world in one transaction ([`crate::write_tiled`]).
    pub(crate) fn put_tiled(&self, rows: Box<crate::forged::TiledRows>) -> Result<(), StoreError> {
        self.ask(|r| Request::PutTiled(rows, r))
    }

    pub(crate) fn forged_head(&self) -> Result<Option<crate::forged::ForgedHead>, StoreError> {
        self.ask(Request::ForgedHead)
    }

    /// One encoded tile row, `None` if the store holds no such tile.
    pub(crate) fn forged_tile(&self, t: worldgen::forged_tiles::TileCoords) -> Result<Option<Vec<u8>>, StoreError> {
        self.ask(|r| Request::ForgedTile(t, r))
    }

    pub(crate) fn forged_world_map(&self) -> Result<Option<Vec<u8>>, StoreError> {
        self.ask(Request::ForgedWorldMap)
    }

    /// The region as last committed; `None` if it never was (it never ran).
    /// Fenced: refused with `Fenced` unless `fence` still holds its super-region.
    pub fn load_region(&self, rc: RegionCoords, fence: &Fence) -> Result<Option<StoredRegion>, StoreError> {
        let fence = fence.clone();
        self.ask(|r| Request::LoadRegion(rc, fence, r))
    }

    /// Only the region's terrain — its stud edge (`game::StudEdge`: the edge columns'
    /// recipes and stud rows); `None` if never committed.
    /// Unfenced: what LOD and halo seeding read of regions this process need not own.
    pub fn load_terrain(&self, rc: RegionCoords) -> Result<Option<TerrainSnapshot>, StoreError> {
        self.ask(|r| Request::LoadTerrain(rc, r))
    }

    /// Every stored region with its `terrain_version`.
    pub fn stored_regions(&self) -> Result<BTreeMap<RegionCoords, u64>, StoreError> {
        self.ask(Request::StoredRegions)
    }

    /// [`Self::stored_regions`], answered on the receiver (a main loop never waits).
    pub fn stored_regions_later(&self) -> crossbeam::channel::Receiver<Result<BTreeMap<RegionCoords, u64>, StoreError>> {
        self.ask_later(Request::StoredRegions)
    }

    /// The stored regions saved at `tick` or later (by a cut at `tick` or a later one), with
    /// their `terrain_version`: what a [`crate::CutNotice::Cut`] at `tick` may have changed.
    pub fn stored_regions_since(&self, tick: Tick) -> Result<BTreeMap<RegionCoords, u64>, StoreError> {
        self.ask(|r| Request::StoredRegionsSince(tick, r))
    }

    /// [`Self::stored_regions_since`], answered on the receiver.
    pub fn stored_regions_since_later(
        &self,
        tick: Tick,
    ) -> crossbeam::channel::Receiver<Result<BTreeMap<RegionCoords, u64>, StoreError>> {
        self.ask_later(|r| Request::StoredRegionsSince(tick, r))
    }

    /// Tells every listener of this world ([`Self::listen_cuts`]) that cut `cut` at `tick`
    /// committed: `pg_notify` in a transaction of its own — call it only after the cut's
    /// commit (or its decision), never inside a cut's transaction. A no-op on SQLite;
    /// refused with `ReadOnly` by a read-only handle. No fence: it writes no row — a notice
    /// is a hint to re-read committed rows, so a stale notifier costs a read, nothing more.
    pub fn notify_cut(&self, cut: CutId, tick: Tick) -> Result<(), StoreError> {
        self.ask(|r| Request::NotifyCut(cut, tick, r))
    }

    /// [`Self::notify_cut`] without waiting (a failure is logged by the store thread).
    pub fn notify_cut_later(&self, cut: CutId, tick: Tick) -> crossbeam::channel::Receiver<Result<(), StoreError>> {
        self.ask_later(|r| Request::NotifyCut(cut, tick, r))
    }

    /// Committed cuts as they are announced ([`Self::notify_cut`]), on a `terrain-listener`
    /// thread of their own (`listen.rs`): in [`crate::NotifyMode::Listen`] one direct,
    /// unpooled connection — to [`crate::WORLD_DB_DIRECT_ENV`]'s URL if set, else the store's
    /// — says `Cut` per notice and `Resync` on every (re)connect; in both modes a `Resync`
    /// every [`crate::TERRAIN_POLL_SECS`]. SQLite: a receiver that never says anything. The
    /// thread ends once the receiver is dropped.
    pub fn listen_cuts(&self, mode: crate::NotifyMode) -> crossbeam::channel::Receiver<crate::CutNotice> {
        let url = match &self.url {
            StoreUrl::Postgres(url) => Some(
                std::env::var(crate::WORLD_DB_DIRECT_ENV).ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| url.clone()),
            ),
            StoreUrl::Sqlite(_) | StoreUrl::Memory => None,
        };
        crate::listen::start(url, mode, &self.poll_ms)
    }

    /// Test hook: the poll period of this handle's later [`Self::listen_cuts`] (default
    /// [`crate::TERRAIN_POLL_SECS`]). Shared by every clone.
    #[doc(hidden)]
    pub fn set_terrain_poll_period(&self, period: std::time::Duration) {
        self.poll_ms.store(period.as_millis().max(1) as u64, Ordering::SeqCst);
    }

    /// The in-flight rows the last committed cuts left, in their order: host `host`'s
    /// (`None`: every host's — pass [`crate::Fence::in_flight_scope`]).
    pub fn load_in_flight(&self, host: Option<u32>) -> Result<Vec<InFlightRow>, StoreError> {
        self.ask(|r| Request::LoadInFlight(host, r))
    }

    pub fn load_history(&self) -> Result<Option<Vec<u8>>, StoreError> {
        self.ask(Request::LoadHistory)
    }

    /// Queues the cut's commit (one transaction, all or nothing) and returns at
    /// once; the answer arrives on the receiver. Commits run in call order.
    /// Fenced: `Fenced` (nothing written) unless `fence` still holds every
    /// super-region it claimed and every one the cut touches
    /// ([`crate::touched_super_regions`]). An empty fence with a cut that
    /// touches no super-region performs no check at all. Refused at once with
    /// `CutPending` while this handle has an undecided prepared part.
    pub fn commit_cut(
        &self,
        cut: WorldCut,
        fence: Fence,
    ) -> crossbeam::channel::Receiver<Result<CutCommitted, StoreError>> {
        let (tx, rx) = crossbeam::channel::bounded(1);
        if let Some(pending) = *self.cuts.pending() {
            let _ = tx.send(Err(StoreError::CutPending(pending)));
            return rx;
        }
        if let Some(e) = self.injected_failure() {
            let _ = tx.send(Err(e));
            return rx;
        }
        if let Err(req) = self.send(Request::CommitCut(Box::new(cut), fence, Reply::sync(tx))) {
            if let Request::CommitCut(_, _, reply) = req {
                reply.send(Err(stopped()));
            }
        }
        rx
    }

    /// Whether this store can save cuts in two phases (several hosts): Postgres with
    /// `max_prepared_transactions > 0`; otherwise `Unsupported`.
    pub fn two_phase_ready(&self) -> Result<(), StoreError> {
        self.ask(Request::TwoPhaseReady)
    }

    /// Queues host `cut.host`'s part of a two-phase cut and returns at once; the
    /// answer arrives on the receiver. Runs in call order with
    /// [`StoreHandle::commit_cut`]s, and writes what a commit writes, fenced the same
    /// way, except the world tick (the decision writes it) — but ends with
    /// `PREPARE TRANSACTION`: nothing is visible until [`StoreHandle::decide_cut`]
    /// commits it. Refused with `StaleGeneration` (nothing staged) unless the world is
    /// at `generation`. The prepared part keeps its `FOR SHARE` on the host's
    /// super-regions, so another process's claim of them waits for the decision.
    /// A part that prepares after its cut was decided finishes itself: committed, the
    /// answer is `Ok`; aborted, `CutAborted` (nothing written).
    ///
    /// From the call until the part is settled, this handle refuses every further cut
    /// (`commit_cut` / `prepare_cut`) at once with `CutPending`. The part is settled by
    /// [`StoreHandle::cut_decided`] once its cut is decided, or by an answer that left
    /// nothing staged (any error but `Backend`, or a self-finished part). Refused at
    /// once with `Unsupported` for a fence with `all_hosts` (its in-flight replace would
    /// lock every host's rows) or a cut carrying the history (the coordinator's).
    /// Postgres only (`Unsupported` on SQLite). Counts as a cut commit for
    /// [`StoreHandle::fail_next_writes`].
    pub fn prepare_cut(
        &self,
        cut: WorldCut,
        fence: Fence,
        generation: u64,
    ) -> crossbeam::channel::Receiver<Result<CutCommitted, StoreError>> {
        let (tx, rx) = crossbeam::channel::bounded(1);
        let refused = if fence.all_hosts {
            Some(StoreError::Unsupported("a two-phase cut needs a per-host fence".into()))
        } else if cut.history.is_some() {
            Some(StoreError::Unsupported("the history is the coordinator's; a host's cut part never carries it".into()))
        } else {
            None
        };
        if let Some(e) = refused {
            let _ = tx.send(Err(e));
            return rx;
        }
        {
            let mut pending = self.cuts.pending();
            if let Some(p) = *pending {
                let _ = tx.send(Err(StoreError::CutPending(p)));
                return rx;
            }
            if let Some(e) = self.injected_failure() {
                let _ = tx.send(Err(e));
                return rx;
            }
            *pending = Some(cut.id);
        }
        let req = Request::PrepareCut(Box::new(cut), fence, generation, Reply::sync(tx));
        if let Err(Request::PrepareCut(c, _, _, reply)) = self.send(req) {
            self.cuts.settle(c.id);
            reply.send(Err(stopped()));
        }
        rx
    }

    /// The coordinator's decision on `cut`, whose parts hosts `hosts` prepared: one
    /// transaction, fenced by `fence` (`Fenced { sr: None }`, nothing recorded or
    /// finished, if another coordinator claimed the row since), records `commit` unless
    /// the cut is decided already (a host's [`StoreHandle::abort_cut`] may have won).
    /// A commit is refused with `StaleGeneration` (nothing recorded) unless the world is
    /// at `generation`. If this call records a commit it also writes the world tick
    /// (`tick`, never backwards) and marks `leaving` clean. A commit is recorded only if
    /// exactly the parts of `hosts` are staged and every host whose `hosts` row is not
    /// clean is among `hosts` (one that registered while the cut was pending runs beside
    /// it unsaved); otherwise an ABORT is recorded (logged)
    /// and the call returns `false`. Then every staged part of the cut — listed or not —
    /// is committed or rolled back as the STORED decision says (a part already finished
    /// counts as done). Returns the stored decision. Postgres only.
    #[allow(clippy::too_many_arguments)]
    pub fn decide_cut(
        &self,
        cut: CutId,
        tick: Tick,
        commit: bool,
        hosts: Vec<u32>,
        leaving: Vec<u32>,
        fence: CoordinatorFence,
        generation: u64,
    ) -> Result<bool, StoreError> {
        if let Some(e) = self.injected_decision_failure() {
            return Err(e);
        }
        let stop_after = self.stop_commits.lock().unwrap_or_else(|e| e.into_inner()).take();
        let d = thread::Decision { cut, tick, commit, hosts, leaving, fence, generation, stop_after };
        self.ask(|r| Request::DecideCut(Box::new(d), r))
    }

    /// [`Self::decide_cut`], answered on the receiver (the coordinator's loop never waits).
    #[allow(clippy::too_many_arguments)]
    pub fn decide_cut_later(
        &self,
        cut: CutId,
        tick: Tick,
        commit: bool,
        hosts: Vec<u32>,
        leaving: Vec<u32>,
        fence: CoordinatorFence,
        generation: u64,
    ) -> crossbeam::channel::Receiver<Result<bool, StoreError>> {
        if let Some(e) = self.injected_decision_failure() {
            return Self::answered(Err(e));
        }
        let stop_after = self.stop_commits.lock().unwrap_or_else(|e| e.into_inner()).take();
        let d = thread::Decision { cut, tick, commit, hosts, leaving, fence, generation, stop_after };
        self.ask_later(|r| Request::DecideCut(Box::new(d), r))
    }

    /// A host's "abort unless decided": records an abort of `cut` if it has no
    /// decision yet, finishes every staged part of the cut as the stored decision says,
    /// and returns it (`true`: it was committed). Not fenced — the first decision
    /// written wins. Postgres only.
    pub fn abort_cut(&self, cut: CutId, tick: Tick) -> Result<bool, StoreError> {
        if let Some(e) = self.injected_decision_failure() {
            return Err(e);
        }
        self.ask(|r| Request::AbortCut(cut, tick, r))
    }

    /// [`Self::abort_cut`], answered on the receiver.
    pub fn abort_cut_later(&self, cut: CutId, tick: Tick) -> crossbeam::channel::Receiver<Result<bool, StoreError>> {
        if let Some(e) = self.injected_decision_failure() {
            return Self::answered(Err(e));
        }
        self.ask_later(|r| Request::AbortCut(cut, tick, r))
    }

    /// The recovery sweep (each process at start): every part of this world still
    /// prepared is committed or rolled back as its cut's decision says; an undecided
    /// cut is aborted first. Safe beside other processes sweeping. Returns each cut it
    /// found with the decision applied, by cut id. Postgres only.
    pub fn resolve_prepared(&self) -> Result<Vec<(CutId, bool)>, StoreError> {
        self.ask(Request::ResolvePrepared)
    }

    /// The stored decision on `cut` (`Some(true)`: committed), `None` while undecided.
    pub fn cut_decision(&self, cut: CutId) -> Result<Option<bool>, StoreError> {
        self.ask(|r| Request::CutDecision(cut, r))
    }

    /// [`Self::cut_decision`], answered on the receiver.
    pub fn cut_decision_later(&self, cut: CutId) -> crossbeam::channel::Receiver<Result<Option<bool>, StoreError>> {
        self.ask_later(|r| Request::CutDecision(cut, r))
    }

    /// A host has heard that `cut` is decided (`CutDone`): re-reads the decision; if
    /// there is one, finishes any part of the cut still staged as it says and settles
    /// this handle's pending part, so its next cut is accepted. Returns the decision
    /// (`None`: undecided — the pending part stays).
    pub fn cut_decided(&self, cut: CutId) -> Result<Option<bool>, StoreError> {
        self.ask(|r| Request::CutDecided(cut, r))
    }

    /// [`Self::cut_decided`], answered on the receiver (a host's main loop never waits).
    pub fn cut_decided_later(&self, cut: CutId) -> crossbeam::channel::Receiver<Result<Option<bool>, StoreError>> {
        self.ask_later(|r| Request::CutDecided(cut, r))
    }

    /// The player id and saved record of `key` (the `players.token` column:
    /// the caller's namespaced player key — `server::player_store::PlayerKey`).
    /// A key never seen gets the next id, written at once (the one write
    /// outside cuts); its record is `None` until a cut writes one.
    pub fn resolve_player(&self, key: &str) -> Result<(PlayerId, Option<PlayerRecord>), StoreError> {
        let key = key.to_string();
        self.ask(|r| Request::ResolvePlayer(key, r))
    }

    /// [`StoreHandle::resolve_player`] for async callers (the network edge).
    pub async fn resolve_player_async(
        &self,
        key: &str,
    ) -> Result<(PlayerId, Option<PlayerRecord>), StoreError> {
        let (tx, rx) = oneshot::channel();
        self.send(Request::ResolvePlayer(key.to_string(), Reply::asynch(tx))).map_err(|_| stopped())?;
        rx.await.map_err(|_| stopped())?
    }

    /// Claims `srs` for `owner` (one transaction): each super-region's epoch
    /// goes up by one (a new one starts at 1), fencing whoever held it.
    pub fn claim_super_regions(&self, owner: u32, srs: Vec<SuperRegion>) -> Result<Fence, StoreError> {
        self.ask(|r| Request::ClaimSuperRegions(owner, srs, r))
    }

    /// Claims the coordinator row (its epoch up by one), fencing the coordinator that held it.
    pub fn claim_coordinator(&self) -> Result<CoordinatorFence, StoreError> {
        self.ask(Request::ClaimCoordinator)
    }

    /// Queues a write of the coordinator's state and returns at once; the
    /// answer (`Fenced` when another coordinator claimed the row since)
    /// arrives on the receiver.
    pub fn save_coordinator(
        &self,
        state: Vec<u8>,
        fence: CoordinatorFence,
    ) -> crossbeam::channel::Receiver<Result<(), StoreError>> {
        let (tx, rx) = crossbeam::channel::bounded(1);
        if let Some(e) = self.injected_failure() {
            let _ = tx.send(Err(e));
            return rx;
        }
        if let Err(req) = self.send(Request::SaveCoordinator(state, fence, Reply::sync(tx))) {
            if let Request::SaveCoordinator(_, _, reply) = req {
                reply.send(Err(stopped()));
            }
        }
        rx
    }

    /// The coordinator's last saved state; `None` before the first save.
    pub fn load_coordinator(&self) -> Result<Option<Vec<u8>>, StoreError> {
        self.ask(Request::LoadCoordinator)
    }

    /// Ties the world to terrain `kind`: a world no server has served yet takes
    /// it (written once, never changed); one already tied to the other kind is
    /// refused with `OtherTerrain` and nothing is written. Call before any
    /// region is loaded or saved. Servers bind [`crate::TerrainKind::Studs`], the
    /// only terrain stored; a world saved on voxel terrain never gets this far (it
    /// is refused at open), so `Voxels` is only ever the mark that gets a world refused.
    pub fn bind_terrain(&self, kind: crate::TerrainKind) -> Result<(), StoreError> {
        self.ask(|r| Request::BindTerrain(kind, r))
    }

    /// The world's clock anchor (R3): stores `proposed` if the world has none, and
    /// returns the stored anchor either way — the first writer wins, so every host
    /// and the coordinator of a world number ticks from the same instant.
    pub fn ensure_clock_anchor(&self, proposed: game::ClockAnchor) -> Result<game::ClockAnchor, StoreError> {
        self.ask(|r| Request::EnsureClockAnchor(proposed, r))
    }

    /// Forgets the clock anchor (`worldctl clock-reset`, and a single-process
    /// start): the next [`StoreHandle::ensure_clock_anchor`] stores a new one.
    pub fn clear_clock_anchor(&self) -> Result<(), StoreError> {
        self.ask(Request::ClearClockAnchor)
    }

    /// Sets the world's super-region side (`worldctl super-region`) if no host has claimed
    /// a super-region yet; returns whether it did (`false`: `super_regions` is not empty,
    /// nothing written). This handle keeps the side it read at open: reopen to use the new one.
    pub fn set_super_region_regions(&self, s: u32) -> Result<bool, StoreError> {
        self.ask(|r| Request::SetSuperRegionRegions(s, r))
    }

    /// Registers this process's boot as host `row.ordinal` (once per boot, right after its
    /// claim): one fence-checked transaction that bumps `world.generation` when the host's
    /// previous boot never left clean (see [`HostBoot`]). `row.generation` and `row.clean`
    /// are ignored; the row is stored with the world's generation and `clean = false`.
    pub fn register_host(&self, row: HostRow, fence: &Fence) -> Result<HostBoot, StoreError> {
        self.ask(|r| Request::RegisterHost(row, fence.clone(), r))
    }

    /// Every registered host, by ordinal.
    pub fn hosts(&self) -> Result<Vec<HostRow>, StoreError> {
        self.ask(Request::Hosts)
    }

    /// [`Self::hosts`], answered on the receiver.
    pub fn hosts_later(&self) -> crossbeam::channel::Receiver<Result<Vec<HostRow>, StoreError>> {
        self.ask_later(Request::Hosts)
    }

    /// The world's generation: how many dirty host restarts the world has seen.
    pub fn world_generation(&self) -> Result<u64, StoreError> {
        self.ask(Request::WorldGeneration)
    }

    /// Host `owner`'s claim epoch as stored: the highest epoch of the super-regions it
    /// holds (`None`: it holds none). A process whose claim was taken over has a lower one.
    pub fn owner_claim_epoch(&self, owner: u32) -> Result<Option<u64>, StoreError> {
        self.ask(|r| Request::OwnerClaimEpoch(owner, r))
    }

    /// The terrain kind the world is tied to; `None` before the first
    /// [`StoreHandle::bind_terrain`].
    pub fn terrain_kind(&self) -> Result<Option<crate::TerrainKind>, StoreError> {
        self.ask(Request::TerrainKind)
    }

    /// Commits everything queued before this call, then stops the thread and
    /// joins it (closing the pool and releasing SQLite's world lock). Other clones'
    /// later calls fail with a `Backend` error.
    pub fn shutdown(self) {
        let _ = self.send(Request::Shutdown);
        let join = self.join.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(join) = join {
            if join.join().is_err() {
                log::error!("[world-store] the store thread panicked");
            }
        }
    }
}
