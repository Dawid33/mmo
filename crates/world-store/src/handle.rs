//! [`StoreHandle`]: how every other thread reaches the `world-store` thread.
//! Cloneable and `Send + Sync`; its methods block on the answer unless noted.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use game::{
    CutId, HostId, EntityId, InFlightRow, PlayerId, PlayerRecord, RegionCoords, StoredRegion, TerrainSnapshot, Tick,
    WorldCut,
};
use tokio::sync::{mpsc, oneshot};

use game::coordinator::StoreDecision;

use crate::coordinator_tx::Decided;
use crate::counters::{Counters, StoreCounters};
use crate::thread::{self, Reply, Request, Stamped};
use crate::{Declared, Dials, Fence, OpenMode, StoreError, StoreUrl, Succession, SuccessionId, SuccessionKind, SuperRegion};

/// A lone decision's outcome of another kind than asked (a bug of the store thread).
fn unexpected(d: Decided) -> StoreError {
    StoreError::Backend(format!("a lone decision answered as another kind: {d:?}"))
}

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

/// One host's row in `hosts` (R3; ids issued since migration 0014). `boot` is a random
/// per-process-start number, stored as BIGINT by a bit cast (`as i64` / `as u64`, like every
/// u64 of the store).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRow {
    /// Issued by [`StoreHandle::register_host`] and never issued again (a row of a world saved
    /// before migration 0014 keeps its old ordinal as its id).
    pub id: HostId,
    /// What people call the process (`VOXEL_HOST_NAME`); never a key. Empty: none.
    pub name: String,
    pub boot: u64,
    /// The world generation this host runs at (set by [`StoreHandle::register_host`]).
    pub generation: u64,
    /// The host's last save cut left it clean.
    pub clean: bool,
    /// [`HOST_LIVE`] or [`HOST_DEAD`] (a takeover, or the first registration after a total
    /// crash, marks a host dead).
    pub state: i64,
    /// The database clock (ms since the epoch) at the host's registration or last heartbeat.
    pub seen_ms: u64,
    /// `drain_at_ms` minus the database clock at the read: ms until a draining host leaves
    /// (negative: past due). `None`: not draining.
    pub drain_in_ms: Option<i64>,
    pub peer_addr: Option<String>,
    pub peer_cert: Option<String>,
    pub quic: Option<String>,
    pub webtransport: Option<String>,
    pub wt_cert_hash: Option<String>,
    /// The host holds land at the read: a `super_regions` row it owns or an `in_flight` row of
    /// its id (Elastic 2a: only such a host is a succession's survivor; a landless lapse is
    /// ended, [`StoreHandle::end_landless_host_later`]).
    pub owns_land: bool,
    /// An open succession has this row's boot as its predecessor. A dead row that is not (its
    /// succession settled, or it was ended holding no land) is gone for good: a host forgets it
    /// (`WorldManager::forget_dead_hosts`).
    pub succeeding: bool,
    /// What the host's last beat reported ([`StoreHandle::heartbeat`]); all zero until one did.
    pub load: game::HostLoad,
}

/// A read of every `hosts` row ([`StoreHandle::hosts_later`]) with the database clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostsRead {
    /// Every host row, by id.
    pub rows: Vec<HostRow>,
    /// The database clock (ms since the epoch, the lease's clock) right after the rows were
    /// read: the world coordinator's clock is re-sampled from it (`game::coordinator::StoreClock`).
    pub store_ms: u64,
    /// `world.next_host_id`, read BEFORE the rows (Elastic 2b Task 6, audit H7): every id below
    /// it was issued before the rows were read, so one absent from them was pruned; an id at or
    /// above it may have registered after.
    pub next_host_id: u64,
    /// Every `desired_hosts` row (Elastic 4), by slot, read right after the rows (SQLite: none).
    pub desired: Vec<game::coordinator::DesiredHost>,
}

/// The world coordinator's row as stored ([`StoreHandle::load_coordinator`], Elastic 2a): its
/// projection, `core` and `history`, at `version` (what the next write must name).
/// `history_version`: the version whose transition last wrote `history` (Elastic 2b Task 4); a
/// [`StoreHandle::load_coordinator_since`] that found it no newer than asked leaves `history`
/// empty (a written history never is).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCoordinator {
    pub version: u64,
    pub core: Vec<u8>,
    pub history: Vec<u8>,
    pub history_version: u64,
}

/// What one lease beat ([`StoreHandle::heartbeat`]) found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Beat {
    /// The row is still the caller's live boot (its `seen_ms` moved). `false`: replaced.
    pub live: bool,
    /// The row's drain mark (`worldctl drain`): ms until the host is to leave, computed by the
    /// database at the beat (`drain_at_ms - seen_ms`; negative: past due). `None`: not draining,
    /// or not `live`.
    pub drain_in_ms: Option<i64>,
}

/// What a process registers as ([`StoreHandle::register_host`]): everything of its row but
/// what the store decides (the id, the generation, `clean`, `state`, `seen_ms`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewHost {
    /// [`HostRow::name`]: a label; a non-empty one also lapses other dirty live rows of the
    /// same name (a restarted process's old id is declared at the coordinator's next read).
    pub name: String,
    pub boot: u64,
    pub peer_addr: Option<String>,
    pub peer_cert: Option<String>,
    pub quic: Option<String>,
    pub webtransport: Option<String>,
    pub wt_cert_hash: Option<String>,
}

/// What [`StoreHandle::register_host`] decided: the id issued, the generation the host runs
/// at, and whether this registration raised the world's (a total crash: it took every dirty
/// row's land and in-flight rows, and every process of the old generation is refused).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registered {
    pub id: HostId,
    pub generation: u64,
    pub bumped: bool,
}

/// The highest id [`StoreHandle::register_host`] issues: the in-flight rows' key packs the
/// host into the high 32 bits of a signed 64-bit `seq` (`rows.rs`), so a larger id would
/// overflow it. A registration that would issue more is refused (`Corrupt`), nothing written.
pub const HOST_ID_MAX: u64 = (1 << 31) - 1;

/// [`HostRow::state`]: [`HOST_LIVE`] (the host holds its lease) or [`HOST_DEAD`] (its lease
/// lapsed and a takeover marked it dead). Defined in `game` (the coordinator's lease watch
/// reads them too).
pub use game::{HOST_DEAD, HOST_LIVE};

/// `super_regions.owner` of a free row (no host holds it); defined in `game` (its
/// directory mirrors the column).
pub use game::NO_OWNER;

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
    /// and coordinator writes ([`StoreHandle::write_coordinator_later`]), whichever come first —
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

    /// Test hook: the host lease (ms) this handle's declarations and registrations judge a
    /// `hosts` row's `seen_ms` by — a row lapsed past it, a booting host counts another alive
    /// within it (default [`game::HOST_LEASE_MS`]). Shared by every clone. Real-time tests
    /// shorten it with the coordinator's watch and the heartbeat period, all together.
    #[doc(hidden)]
    pub fn set_host_lease_ms(&self, ms: u64) {
        self.cuts.host_lease_ms.store(ms, Ordering::SeqCst);
    }

    /// The host lease (ms) this handle judges `hosts` rows by ([`game::HOST_LEASE_MS`] unless a
    /// test shortened it, [`Self::set_host_lease_ms`]).
    pub fn host_lease_ms(&self) -> u64 {
        self.cuts.host_lease_ms.load(Ordering::SeqCst)
    }

    /// Test hook: the `lock_timeout` of this handle's later cut transactions, in ms
    /// (default [`crate::CUT_LOCK_TIMEOUT_MS`]). Shared by every clone.
    #[doc(hidden)]
    pub fn set_cut_lock_timeout_ms(&self, ms: u64) {
        self.cuts.lock_timeout_ms.store(ms, Ordering::SeqCst);
    }

    /// How many committed world cuts this handle (and its clones) decided settled a
    /// succession whose predecessor still had in-flight rows under its ordinal — the heir's
    /// part did not adopt them (each reported once with `error!`; the settle↔adopts pairing
    /// is the coordinator's invariant, the store only detects a breach).
    pub fn cuts_settled_unadopted(&self) -> u64 {
        self.cuts.settled_unadopted_cuts.load(Ordering::SeqCst)
    }

    /// Test hook: the idle limit of this store's later fenced transactions, in ms (default
    /// [`crate::FENCED_TX_IDLE_MS`]).
    #[doc(hidden)]
    pub fn set_fenced_tx_idle_ms(&self, ms: u64) -> Result<(), StoreError> {
        self.ask(|r| Request::TestHook(crate::thread::TestHook::FencedTxIdleMs(ms), r))
    }

    /// Test hook: a FROZEN host — this store opens a fenced transaction over `fence`'s
    /// super-regions (the real fence check: `FOR SHARE` on them) and then sends nothing more
    /// on it for `hold_ms`, as a process stopped inside a cut part or a region load. Answers
    /// once the locks are held. (Postgres.)
    #[doc(hidden)]
    pub fn freeze_in_fenced_tx(&self, fence: Fence, hold_ms: u64) -> Result<(), StoreError> {
        self.ask(|r| Request::TestHook(crate::thread::TestHook::FreezeInFencedTx(fence, hold_ms), r))
    }

    /// Test hook: `n` connections of this store's shared pool are taken and held for
    /// `hold_ms` (a burst of region loads, a long cut part). Answers once they are held.
    #[doc(hidden)]
    pub fn hold_pool_connections(&self, n: u32, hold_ms: u64) -> Result<(), StoreError> {
        self.ask(|r| Request::TestHook(crate::thread::TestHook::HoldConnections(n, hold_ms), r))
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

    /// Only the region's terrain (chunks + ground); `None` if never committed.
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
        let period = crate::listen::poll_period(&self.poll_ms);
        crate::listen::start(self.listen_url(), "terrain-listener", crate::WORLD_CUT_CHANNEL, mode, period, crate::listen::cut_notices())
    }

    /// The notices on `channel` (`listen.rs`): in [`crate::NotifyMode::Listen`] one direct,
    /// unpooled connection — as [`Self::listen_cuts`]'s — says [`crate::Notice::Payload`] per
    /// notice and [`crate::Notice::Resync`] on every (re)connect; in both modes a `Resync` every
    /// poll period: [`crate::COORDINATOR_POLL_MS`] for [`crate::COORDINATOR_ROW_CHANNEL`], the
    /// terrain poll for any other. SQLite: a receiver that never says anything. The thread ends
    /// once the receiver is dropped.
    pub fn listen_channel(&self, channel: &'static str, mode: crate::NotifyMode) -> crossbeam::channel::Receiver<crate::Notice> {
        let period = if channel == crate::COORDINATOR_ROW_CHANNEL {
            std::time::Duration::from_millis(crate::COORDINATOR_POLL_MS)
        } else {
            crate::listen::poll_period(&self.poll_ms)
        };
        crate::listen::start(self.listen_url(), "row-listener", channel, mode, period, |n| n)
    }

    /// Where a listener connects: [`crate::WORLD_DB_DIRECT_ENV`]'s URL if set, else the store's
    /// (`None`: SQLite, which cannot listen).
    fn listen_url(&self) -> Option<String> {
        match &self.url {
            StoreUrl::Postgres(url) => Some(
                std::env::var(crate::WORLD_DB_DIRECT_ENV).ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| url.clone()),
            ),
            StoreUrl::Sqlite(_) | StoreUrl::Memory => None,
        }
    }

    /// Takes the outbox rows other replicas left for `for_hosts` (Elastic 2b Task 5: this
    /// replica's hosts, each with its link's boot) at or below row version `upto` (the version of
    /// the cache that found them attached to this replica, final review S-I2: a newer row stays
    /// for the replica its host attached to since) between transitions — one `DELETE … RETURNING`
    /// of the rows only (`coordinator_tx.rs`), answered on the receiver in `seq` (commit) order; a
    /// row of another boot is deleted unsent. The caller issues it only while no transition of its
    /// own is in flight (a transition takes the same rows inside its own transaction). SQLite:
    /// nothing (one replica).
    pub fn take_outbox_later(
        &self,
        for_hosts: Vec<(HostId, u64)>,
        upto: u64,
    ) -> crossbeam::channel::Receiver<Result<Vec<crate::OutboxRow>, StoreError>> {
        self.ask_later(|r| Request::TakeOutbox(for_hosts, upto, r))
    }

    /// Test hook: the poll period of this handle's later [`Self::listen_cuts`] (default
    /// [`crate::TERRAIN_POLL_SECS`]). Shared by every clone.
    #[doc(hidden)]
    pub fn set_terrain_poll_period(&self, period: std::time::Duration) {
        self.poll_ms.store(period.as_millis().max(1) as u64, Ordering::SeqCst);
    }

    /// The in-flight rows the last committed cuts left, in their order: host `host`'s
    /// (`None`: every host's — pass [`crate::Fence::in_flight_scope`]).
    pub fn load_in_flight(&self, host: Option<HostId>) -> Result<Vec<InFlightRow>, StoreError> {
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
    /// `CutPending` while this handle has an undecided prepared part. A cut with
    /// [`WorldCut::adopts`] also replaces those hosts' in-flight rows, and is refused with
    /// `Unsupported` (nothing written) unless an open succession makes `cut.host` the heir
    /// of each (R4).
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
    /// `adopts` as for [`StoreHandle::commit_cut`] (read without a lock: the decision that
    /// settles the succession must not wait for this part).
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

    /// Test support (Elastic 2b Task 3: the coordinator decides only inside its transition,
    /// [`Self::write_transition_later`]): the decision on `cut`, whose parts hosts `hosts`
    /// prepared, in a transition of its own over the row as it stands (`decide_in`): records `commit` unless
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
    ///
    /// R4: a commit is also recorded as an abort unless `settles` is exactly the set of open
    /// successions (read under the world row's lock); the commit this call records then
    /// settles them in the same transaction — each gets `settled_cut = cut` and its
    /// predecessor's row (of the boot that died) turns clean. Pass an empty `settles` while
    /// none is open.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn decide_cut(
        &self,
        cut: CutId,
        tick: Tick,
        commit: bool,
        hosts: Vec<HostId>,
        leaving: Vec<HostId>,
        generation: u64,
        settles: Vec<SuccessionId>,
    ) -> Result<bool, StoreError> {
        let d = StoreDecision::DecideCut(game::coordinator::CutDecide { cut, tick, commit, hosts, leaving, generation, settles });
        match self.decide_alone(d)? {
            Decided::Cut { committed, .. } => committed,
            other => Err(unexpected(other)),
        }
    }

    /// Test support: records that world cut `cut` began (Elastic 1 Task 4), in a transition of
    /// its own (`begin_in`): the bound under which every host id was issued before now. A host
    /// whose id is at or above it may be absent from the cut's decision. First writer wins.
    /// Postgres only.
    #[doc(hidden)]
    pub fn record_cut_begin(&self, cut: CutId) -> Result<HostId, StoreError> {
        match self.decide_alone(StoreDecision::BeginCut { cut })? {
            Decided::Begin { bound, .. } => bound,
            other => Err(unexpected(other)),
        }
    }

    /// One store decision in a transition of its own over the row as it stands (test support;
    /// the decision test hooks apply as to the coordinator's).
    fn decide_alone(&self, d: StoreDecision) -> Result<Decided, StoreError> {
        let cut = matches!(d, StoreDecision::DecideCut(_));
        let injected = cut && self.injected_decision_failure().is_some();
        let stop_after = if cut { self.stop_commits.lock().unwrap_or_else(|e| e.into_inner()).take() } else { None };
        self.ask(|reply| Request::DecideAlone { d: Box::new(d), injected, stop_after, reply })
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

    /// Every cut part staged in this world now, as `(cut, host)` in order, found by the
    /// prepared transactions' names whatever the host ids (no host count). A read: nothing
    /// is finished ([`Self::resolve_prepared`] finishes them). SQLite: none, ever.
    pub fn staged_cut_parts(&self) -> Result<Vec<(CutId, HostId)>, StoreError> {
        self.ask(Request::StagedCutParts)
    }

    /// The newest committed world cut (`CutId(0)` before any, and on a store whose cuts are
    /// one-phase — a single host, SQLite — which records no decisions): a host's peer journal
    /// counts from it at boot (`WorldManager::start_peer_journal`).
    pub fn last_committed_cut(&self) -> Result<CutId, StoreError> {
        self.ask(Request::LastCommittedCut)
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

    /// Test support (Elastic 2b Task 3: the coordinator declares only inside its transition):
    /// declares hosts `dead` (`(ordinal, boot)`: the boot it judged) and
    /// gives each one's land to the heir at the same index of `heirs`, in ONE transaction of its
    /// own over the row as it stands (`lock_timeout` this handle's cut lock timeout,
    /// [`crate::CUT_LOCK_TIMEOUT_MS`]), under the world row's lock at `generation`
    /// (`StaleGeneration`). Any open succession → [`Declared::SuccessionOpen`]. Each
    /// predecessor row must be of its boot and live, not succeeded before (else `Gone`); a
    /// `Crash` needs it dirty and lapsed — `seen_ms = 0` (vouched) or older than
    /// [`game::HOST_LEASE_MS`] by the database clock — and a `Leave` needs it clean (else
    /// `NotLapsed`); each heir must be live, dirty and beating (else `Gone`). Then: a crashed
    /// row is marked [`HOST_DEAD`], each succession row is inserted (based on the last
    /// committed cut; `survivors` = every other live dirty host that holds land — a
    /// `super_regions` or `in_flight` row — or is an heir of the call, read under the lock
    /// `register_host` takes) and every super-region of the predecessor moves to its heir at
    /// `epoch + 1` — the heir reads its new fence with [`Self::owned_by`]; the predecessor's
    /// next fenced write or load is refused. The moved rows wait for a staged part holding
    /// them: declare only between cuts; a lock timeout (a `Backend` error, nothing written)
    /// is a retry. Postgres only (`Unsupported` on SQLite). Nothing but `Done` writes.
    #[doc(hidden)]
    pub fn declare_succession(
        &self,
        dead: Vec<(HostId, u64)>,
        heirs: Vec<HostId>,
        kind: SuccessionKind,
        generation: u64,
    ) -> Result<Declared, StoreError> {
        match self.decide_alone(StoreDecision::Declare(game::coordinator::Declare { dead, heirs, kind, generation }))? {
            Decided::Declared(declared) => declared,
            other => Err(unexpected(other)),
        }
    }

    /// Test support (the coordinator rolls back only inside its transition): the world restarts
    /// from its last committed cut — `world.generation + 1` (every host of the old generation is
    /// refused from then on) and every open succession abandoned (the land stays with the
    /// heirs), in one transaction of its own under the world row's lock — only from generation
    /// `from`, the one the coordinator decided the rollback at (Elastic 2a): a world past it was
    /// rolled back already, nothing is written. Idempotent. Returns the world's generation after
    /// it. Postgres only.
    #[doc(hidden)]
    pub fn rollback_world(&self, from: u64) -> Result<u64, StoreError> {
        match self.decide_alone(StoreDecision::Rollback { from })? {
            Decided::RolledBack(generation) => generation,
            other => Err(unexpected(other)),
        }
    }

    /// R4, `worldctl rollback` (the operator, who holds no coordinator fence): as
    /// [`Self::rollback_world`] — `world.generation + 1`, every open succession abandoned (the
    /// land stays with the heirs, the predecessors' in-flight rows go to them too) — under the
    /// world row's lock alone, refusing nothing. Returns `(generation, successions
    /// abandoned)`. A running coordinator is not fenced: its next decision at the old
    /// generation is refused (`StaleGeneration`, recorded as an abort), and the hosts' parts
    /// are refused the same way (a host then exits with the world-rollback code, 5). Postgres
    /// only.
    pub fn rollback_world_operator(&self) -> Result<(u64, u64), StoreError> {
        self.ask(Request::RollbackWorldOperator)
    }

    /// Elastic 2a, the crash-loop rule: host `host`'s row of boot `boot` lapsed and it holds no
    /// land — its row is marked [`HOST_DEAD`] with NO succession and the world generation
    /// untouched (nobody inherits, nobody reports, nothing rolls back) — test support: the
    /// coordinator ends one only inside its transition — in one transaction of its own over the
    /// row as it stands, under the `world` row's lock (the declaration's), bounded by the cut
    /// lock timeout. Refused, nothing written: [`crate::LandlessEnd::Gone`] (no row, another
    /// boot's, not live), [`crate::LandlessEnd::NotLapsed`] (it beat within the lease by the
    /// database clock and was not vouched), [`crate::LandlessEnd::OwnsLand`] (a `super_regions`
    /// or `in_flight` row of it), [`crate::LandlessEnd::Recipient`] (an open succession names it
    /// heir or survivor). The lock walk is in `succession.rs`. Postgres only (`Unsupported` on
    /// SQLite).
    #[doc(hidden)]
    pub fn end_landless_host(&self, host: HostId, boot: u64) -> Result<crate::LandlessEnd, StoreError> {
        match self.decide_alone(StoreDecision::EndLandless(game::coordinator::EndLandless { host, boot }))? {
            Decided::Ended { outcome, .. } => outcome,
            other => Err(unexpected(other)),
        }
    }

    /// Elastic 3: super-region `a.sr` given to host `a.host` of boot `a.boot` if the row is
    /// absent (epoch 1) or free (epoch + 1) and that boot's `hosts` row is live, dirty and not
    /// draining — test support: the coordinator assigns only inside its transition
    /// ([`StoreDecision::AssignLand`]); here in a transition of its own over the row as it
    /// stands. Refused, nothing written: [`game::coordinator::Assigned::Owned`],
    /// [`game::coordinator::Assigned::HostNotLive`], [`game::coordinator::Assigned::HostDraining`].
    /// The lock walk is in `succession.rs`. Postgres only (`Unsupported` on SQLite).
    #[doc(hidden)]
    pub fn assign_land(&self, a: game::coordinator::AssignLand) -> Result<game::coordinator::Assigned, StoreError> {
        match self.decide_alone(StoreDecision::AssignLand(a))? {
            Decided::Assigned { outcome, .. } => outcome,
            other => Err(unexpected(other)),
        }
    }

    /// Elastic 4: one more `desired_hosts` row ([`StoreDecision::AddHost`]) — test support: the
    /// coordinator adds one only inside its transition; here in a transition of its own over the
    /// row as it stands. Only `Done` writes. The lock walk is in `succession.rs`. Postgres only.
    #[doc(hidden)]
    pub fn add_desired_host(&self, a: game::coordinator::AddHost) -> Result<game::coordinator::HostAdded, StoreError> {
        match self.decide_alone(StoreDecision::AddHost(a))? {
            Decided::HostAdded { outcome, .. } => outcome,
            other => Err(unexpected(other)),
        }
    }

    /// Elastic 4: a `desired_hosts` row marked draining, and its host's `hosts` row
    /// ([`StoreDecision::DrainHost`]) — test support, as [`Self::add_desired_host`].
    #[doc(hidden)]
    pub fn drain_desired_host(&self, d: game::coordinator::DrainHost) -> Result<game::coordinator::HostDrained, StoreError> {
        match self.decide_alone(StoreDecision::DrainHost(d))? {
            Decided::HostDrained { outcome, .. } => outcome,
            other => Err(unexpected(other)),
        }
    }

    /// Elastic 4: a `desired_hosts` row deleted ([`StoreDecision::RemoveHost`]) — test support,
    /// as [`Self::add_desired_host`].
    #[doc(hidden)]
    pub fn remove_desired_host(&self, r: game::coordinator::RemoveHost) -> Result<game::coordinator::HostRemoved, StoreError> {
        match self.decide_alone(StoreDecision::RemoveHost(r))? {
            Decided::HostRemoved { outcome, .. } => outcome,
            other => Err(unexpected(other)),
        }
    }

    /// R4, `worldctl retire-host` while other hosts live: host `ordinal`'s dirty live row gets
    /// `seen_ms = 0` (vouched dead, as a boot of its ordinal would), unless it beat within the
    /// lease ([`crate::Vouched::Running`]) — one statement under the row's lock, so a beat
    /// either comes first (refused) or overwrites the vouch (the coordinator's declaration
    /// re-checks the lapse: `NotLapsed`, harmless). The coordinator declares a vouched row at
    /// its next read. Postgres only.
    pub fn vouch_host(&self, ordinal: HostId) -> Result<crate::Vouched, StoreError> {
        self.ask(|r| Request::VouchHost(ordinal, r))
    }

    /// Test hook: the `lock_timeout` of this handle's later claims and registrations,
    /// in ms (instead of [`crate::CLAIM_LOCK_TIMEOUT_MS`]).
    #[doc(hidden)]
    pub fn set_claim_lock_timeout_ms(&self, ms: u64) {
        self.cuts.claim_lock_timeout_ms.store(ms, Ordering::SeqCst);
    }

    /// Every open succession (not settled, not abandoned), by id: what a restarted
    /// coordinator re-sends. Empty on SQLite.
    pub fn open_successions(&self) -> Result<Vec<Succession>, StoreError> {
        self.ask(Request::OpenSuccessions)
    }

    /// [`Self::open_successions`], answered on the receiver.
    pub fn open_successions_later(&self) -> crossbeam::channel::Receiver<Result<Vec<Succession>, StoreError>> {
        self.ask_later(Request::OpenSuccessions)
    }

    /// The in-flight rows the last committed cuts left for host `host`, read by its heir
    /// `fence.owner`: refused with `Unsupported` unless an open succession makes it the heir
    /// of `host` (as a cut part's [`WorldCut::adopts`]).
    pub fn load_in_flight_of(&self, host: HostId, fence: &Fence) -> Result<Vec<InFlightRow>, StoreError> {
        let fence = fence.clone();
        self.ask(|r| Request::LoadInFlightOf(host, fence, r))
    }

    /// [`Self::load_in_flight_of`], answered on the receiver (an heir's adoption read, from
    /// its main loop).
    pub fn load_in_flight_of_later(
        &self,
        host: HostId,
        fence: &Fence,
    ) -> crossbeam::channel::Receiver<Result<Vec<InFlightRow>, StoreError>> {
        let fence = fence.clone();
        self.ask_later(|r| Request::LoadInFlightOf(host, fence, r))
    }

    /// The region of every stored row of each id (an id stored twice gives two entries, an
    /// unknown one none), in the order of `ids`, then by region.
    pub fn regions_of_entities(&self, ids: Vec<EntityId>) -> Result<Vec<(EntityId, RegionCoords)>, StoreError> {
        self.ask(|r| Request::RegionsOfEntities(ids, r))
    }

    /// [`Self::regions_of_entities`], answered on the receiver.
    pub fn regions_of_entities_later(
        &self,
        ids: Vec<EntityId>,
    ) -> crossbeam::channel::Receiver<Result<Vec<(EntityId, RegionCoords)>, StoreError>> {
        self.ask_later(|r| Request::RegionsOfEntities(ids, r))
    }

    /// The player id and saved record of `key` (the `players.token` column:
    /// the caller's namespaced player key — `server::player_store::PlayerKey`).
    /// A key never seen gets the next id, written at once (the one write
    /// outside cuts); its record is `None` until a cut writes one.
    pub fn resolve_player(&self, key: &str) -> Result<(PlayerId, Option<PlayerRecord>), StoreError> {
        let key = key.to_string();
        self.ask(|r| Request::ResolvePlayer(key, r))
    }

    /// [`StoreHandle::resolve_player`] without waiting: the answer comes on the receiver (the
    /// gate host's login relay, which runs on the lease-critical main loop).
    pub fn resolve_player_later(
        &self,
        key: &str,
    ) -> crossbeam::channel::Receiver<Result<(PlayerId, Option<PlayerRecord>), StoreError>> {
        let key = key.to_string();
        self.ask_later(|r| Request::ResolvePlayer(key, r))
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
    /// Unguarded: a boot's claim before it registered (`AllInOne`, `Static`), tools and tests.
    pub fn claim_super_regions(&self, owner: HostId, srs: Vec<SuperRegion>) -> Result<Fence, StoreError> {
        self.ask(|r| Request::ClaimSuperRegions(owner, srs, None, r))
    }

    /// [`Self::claim_super_regions`] by a registered boot (`boot`): refused with `Fenced { sr:
    /// None }` (nothing claimed) unless host `owner`'s `hosts` row is still that live boot —
    /// a boot declared dead (R4) cannot take its rows back from the heir. What a `claim` host
    /// re-claims its own rows with.
    pub fn claim_super_regions_live(&self, owner: HostId, boot: u64, srs: Vec<SuperRegion>) -> Result<Fence, StoreError> {
        self.ask(|r| Request::ClaimSuperRegions(owner, srs, Some(boot), r))
    }

    /// The coordinator row's first write (Elastic 2b Task 3): `core` and `history` at version 1,
    /// epoch 1 — or [`crate::Bootstrapped::Exists`] when a row with a projection is there
    /// (first writer wins; nothing written: load it). There is no claim: the row's version is
    /// the coordinator's fence.
    pub fn bootstrap_coordinator(&self, core: Vec<u8>, history: Vec<u8>) -> Result<crate::Bootstrapped, StoreError> {
        self.ask(|r| Request::BootstrapCoordinator(core, history, r))
    }

    /// Raises the coordinator row's epoch past its own and past the high half of every recorded
    /// cut decision's id (final review T3): the cut ids' high half of a core that REPLACES an
    /// unreadable stored one, so no cut id of the replaced core is made again. The new epoch;
    /// `None`: no row. The row's version is untouched.
    pub fn raise_coordinator_epoch(&self) -> Result<Option<u64>, StoreError> {
        self.ask(Request::RaiseCoordinatorEpoch)
    }

    /// Queues a coordinator transition (Elastic 2b Task 3, `coordinator_tx.rs`) and returns at
    /// once: its projection written with a compare-and-set on `w.version` and its store
    /// decisions run in the same transaction. The answer arrives on the receiver: the new
    /// version, the database clock, each decision's answer and the post-commit effects;
    /// `Conflict` when the version moved (nothing written, no decision run); any other error:
    /// whether it committed is not known. SQLite: `Unsupported` for a transition with
    /// decisions.
    pub fn write_transition_later(&self, w: crate::TransitionWrite) -> crossbeam::channel::Receiver<Result<crate::TransitionWritten, StoreError>> {
        let (tx, rx) = crossbeam::channel::bounded(1);
        if let Some(e) = self.injected_failure() {
            let _ = tx.send(Err(e));
            return rx;
        }
        let injected: Vec<bool> = w
            .decisions
            .iter()
            .map(|d| matches!(d, StoreDecision::DecideCut(_)) && self.injected_decision_failure().is_some())
            .collect();
        let req = Request::WriteTransition { w: Box::new(w), injected, reply: Reply::sync(tx) };
        if let Err(Request::WriteTransition { reply, .. }) = self.send(req) {
            reply.send(Err(stopped()));
        }
        rx
    }

    /// A committed transition's post-commit effect ([`crate::AfterCommit`]): a decided cut's
    /// staged parts finished (the test hook [`Self::stop_after_commit_prepared`] applies), or a
    /// committed cut announced. Answered on the receiver; a failure is retried by the caller (or
    /// left to the lookups: a host's `cut_decided`, a start's `resolve_prepared`, the listeners'
    /// poll).
    pub fn after_commit_later(&self, a: crate::AfterCommit) -> crossbeam::channel::Receiver<Result<(), StoreError>> {
        let stop_after = match a {
            crate::AfterCommit::FinishCut { .. } => self.stop_commits.lock().unwrap_or_else(|e| e.into_inner()).take(),
            crate::AfterCommit::NotifyCut { .. } => None,
        };
        self.ask_later(|r| Request::AfterCommit(a, stop_after, r))
    }

    /// The coordinator's last written projection; `None` before the first write.
    pub fn load_coordinator(&self) -> Result<Option<StoredCoordinator>, StoreError> {
        self.ask(Request::LoadCoordinator)
    }

    /// [`Self::load_coordinator`], answered on the receiver (a caller bounded by a deadline).
    pub fn load_coordinator_later(&self) -> crossbeam::channel::Receiver<Result<Option<StoredCoordinator>, StoreError>> {
        self.ask_later(Request::LoadCoordinator)
    }

    /// The coordinator's projection with its history only if a transition wrote it after
    /// version `history_version` (else [`StoredCoordinator::history`] is empty: the caller's
    /// history is the row's); `None` before the first write (Elastic 2b Task 4).
    pub fn load_coordinator_since(&self, history_version: u64) -> Result<Option<StoredCoordinator>, StoreError> {
        self.ask(|r| Request::LoadCoordinatorSince(history_version, r))
    }

    /// [`Self::load_coordinator_since`], answered on the receiver.
    pub fn load_coordinator_since_later(
        &self,
        history_version: u64,
    ) -> crossbeam::channel::Receiver<Result<Option<StoredCoordinator>, StoreError>> {
        self.ask_later(|r| Request::LoadCoordinatorSince(history_version, r))
    }

    /// The `coordinator` row's version (0: no row), answered on the receiver: a replica's
    /// version check of the outputs it releases without a write (Elastic 2b departs 5).
    pub fn row_version_later(&self) -> crossbeam::channel::Receiver<Result<u64, StoreError>> {
        self.ask_later(Request::RowVersion)
    }

    /// Registers a coordinator replica (Elastic 2b Task 4; once per coordinator process start,
    /// `addr` where it listens for hosts — a label). ONE transaction (`replicas.rs`): the next id
    /// issued from `coordinator.next_replica` (never one issued before; the row created empty if
    /// there is none), every other live row not beaten within [`game::coordinator::REPLICA_LEASE_MS`]
    /// by the database clock marked dead — `alone` when no other row is left live — the dead rows
    /// older than a day pruned, the new row inserted live.
    pub fn register_replica(&self, addr: &str) -> Result<crate::ReplicaRegistered, StoreError> {
        let addr = addr.to_string();
        self.ask(|r| Request::RegisterReplica(addr, r))
    }

    /// One beat of replica `me`'s row (Postgres: on the beat connection, under a 1 s
    /// `lock_timeout` and the beat deadline): `false` when it is no longer live (ended — the
    /// replica exits fenced). An error is "no beat this time". Never called on SQLite.
    pub fn beat_replica(&self, me: game::coordinator::ReplicaId) -> Result<bool, StoreError> {
        self.ask(|r| Request::BeatReplica(me, r))
    }

    /// [`Self::beat_replica`], answered on the receiver.
    pub fn beat_replica_later(&self, me: game::coordinator::ReplicaId) -> crossbeam::channel::Receiver<Result<bool, StoreError>> {
        self.ask_later(|r| Request::BeatReplica(me, r))
    }

    /// Every `coordinator_replicas` row, by id, answered on the receiver (a replica's lapse watch).
    pub fn replicas_later(&self) -> crossbeam::channel::Receiver<Result<Vec<crate::ReplicaRow>, StoreError>> {
        self.ask_later(Request::Replicas)
    }

    /// Every `coordinator_replicas` row, by id.
    pub fn replicas(&self) -> Result<Vec<crate::ReplicaRow>, StoreError> {
        self.ask(Request::Replicas)
    }

    /// Test hook: the replica lease a registration judges the other rows by, ms (default
    /// [`game::coordinator::REPLICA_LEASE_MS`]). Shared by every clone.
    #[doc(hidden)]
    pub fn set_replica_lease_ms(&self, ms: u64) {
        self.cuts.replica_lease_ms.store(ms, Ordering::SeqCst);
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

    /// Registers a new host (once per process start): ONE transaction under the world row's
    /// lock (`rows.rs` `register_host`; its locks in the module notes of `succession.rs`) that
    /// issues the next id from `world.next_host_id` — never one issued before — and stores the
    /// row dirty, live and seen now. A new host owns nothing and never waits:
    /// - first, other dirty live rows of the same non-empty `name` get `seen_ms = 0` (vouched:
    ///   the coordinator declares them at its next read) and are not live below — a restarted
    ///   process's own old row never counts as another host;
    /// - `Rollback` (`static`, one host) beside another live row → [`StoreError::NotAlone`],
    ///   nothing written;
    /// - no other live row and at least one dirty one (a total crash, or a lone host restarted
    ///   under its name): every open succession is abandoned (as a world rollback does), every
    ///   dirty row's `super_regions` rows (at `epoch + 1`) and `in_flight` rows become the new
    ///   host's, those rows are marked dead and `world.generation` goes up (`bumped`) — under
    ///   every policy.
    ///
    /// Then every clean or dead row that owns no `super_regions` and no `in_flight` row and
    /// that no open succession names is deleted (its id is never issued again).
    pub fn register_host(&self, host: NewHost, policy: game::CrashPolicy) -> Result<Registered, StoreError> {
        self.ask(|r| Request::RegisterHost(host, policy, r))
    }

    /// Every registered host, by id.
    pub fn hosts(&self) -> Result<Vec<HostRow>, StoreError> {
        self.ask(Request::Hosts).map(|read: HostsRead| read.rows)
    }

    /// Every `desired_hosts` row (Elastic 4), by slot: a plain read — also on a
    /// [`crate::OpenMode::ReadOnly`] handle (the reconciler's, `worldctl desired`). SQLite: none.
    pub fn desired_hosts(&self) -> Result<Vec<game::coordinator::DesiredHost>, StoreError> {
        self.ask(Request::DesiredHosts)
    }

    /// [`Self::hosts`] with the database clock, answered on the receiver.
    pub fn hosts_later(&self) -> crossbeam::channel::Receiver<Result<HostsRead, StoreError>> {
        self.ask_later(Request::Hosts)
    }

    /// One lease beat: host `ordinal`'s row gets `seen_ms` = the database clock, if it is
    /// still boot `boot` and live, carries `load` (the host's own report) and the beat reports the row's drain mark ([`Beat`]).
    /// `live: false`: no such live row — the caller was replaced.
    /// With Postgres a row lock held by a registration or a decision makes the beat fail with an error
    /// after [`crate::HEARTBEAT_LOCK_TIMEOUT_MS`] (not `live: false`). Unfenced on purpose: it writes only the caller's own row and proves nothing but
    /// liveness (ownership is what the fence proves, and every write still checks it).
    pub fn heartbeat(&self, ordinal: HostId, boot: u64, load: game::HostLoad) -> Result<Beat, StoreError> {
        self.ask(|r| Request::Heartbeat(ordinal, boot, load, r))
    }

    /// [`Self::heartbeat`], answered on the receiver.
    pub fn heartbeat_later(
        &self,
        ordinal: HostId,
        boot: u64,
        load: game::HostLoad,
    ) -> crossbeam::channel::Receiver<Result<Beat, StoreError>> {
        self.ask_later(|r| Request::Heartbeat(ordinal, boot, load, r))
    }

    /// Elastic 1, `worldctl drain`: marks host `id` draining — its `drain_at_ms` becomes the
    /// database clock + `in_ms`, or stays where it is if that is earlier (a mark only moves
    /// earlier: no postponement, no un-drain). ONE `UPDATE` of that one `hosts` row, unfenced
    /// like the heartbeat (it changes no ownership; the lock notes are in `operator.rs`).
    /// Refused ([`StoreError::NotDrainable`], nothing written) unless the row is dirty, live and
    /// beaten within the lease (a lapsed row's process is gone, not declared yet).
    /// A draining host is assigned no new land (`AssignLand` answers `HostDraining`) and is not the
    /// coordinator's gate host; it learns the mark at its next beat ([`Beat::drain_in_ms`]).
    pub fn drain_host(&self, id: HostId, in_ms: u64) -> Result<(), StoreError> {
        self.ask(|r| Request::DrainHost(id, in_ms, r))
    }

    /// Every super-region row, by super-region: its owner (`NO_OWNER` when free) and epoch.
    pub fn owners(&self) -> Result<Vec<(SuperRegion, i64, u64)>, StoreError> {
        self.ask(Request::Owners)
    }

    /// [`Self::owners`], answered on the receiver.
    pub fn owners_later(&self) -> crossbeam::channel::Receiver<Result<Vec<(SuperRegion, i64, u64)>, StoreError>> {
        self.ask_later(Request::Owners)
    }

    /// The rows `owner` holds now, as a fence (`all_hosts: false`). A plain read: a write
    /// under it still checks the stored epoch.
    pub fn owned_by(&self, owner: HostId) -> Result<Fence, StoreError> {
        self.ask(|r| Request::OwnedBy(owner, r))
    }

    /// [`Self::owned_by`], answered on the receiver.
    pub fn owned_by_later(&self, owner: HostId) -> crossbeam::channel::Receiver<Result<Fence, StoreError>> {
        self.ask_later(|r| Request::OwnedBy(owner, r))
    }

    /// The database's clock, in ms since the epoch (what `seen_ms` is stamped with).
    pub fn db_now_ms(&self) -> Result<u64, StoreError> {
        self.ask(Request::DbNowMs)
    }

    /// [`Self::db_now_ms`], answered on the receiver.
    pub fn db_now_ms_later(&self) -> crossbeam::channel::Receiver<Result<u64, StoreError>> {
        self.ask_later(Request::DbNowMs)
    }

    /// The world's generation: how many dirty host restarts the world has seen.
    pub fn world_generation(&self) -> Result<u64, StoreError> {
        self.ask(Request::WorldGeneration)
    }

    /// [`Self::world_generation`], answered on the receiver (the coordinator's lookup of a
    /// rollback whose answer was lost).
    pub fn world_generation_later(&self) -> crossbeam::channel::Receiver<Result<u64, StoreError>> {
        self.ask_later(Request::WorldGeneration)
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
