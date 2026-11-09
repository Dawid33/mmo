//! [`StoreHandle`]: how every other thread reaches the `world-store` thread.
//! Cloneable and `Send + Sync`; its methods block on the answer unless noted.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use game::{
    CutId, InFlightRow, PlayerId, PlayerRecord, RegionCoords, StoredRegion, TerrainSnapshot, Tick,
    WorldCut,
};
use tokio::sync::{mpsc, oneshot};

use crate::thread::{self, Reply, Request};
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

/// A handle on the open store (see the crate docs and `thread.rs`).
#[derive(Clone)]
pub struct StoreHandle {
    tx: mpsc::UnboundedSender<Request>,
    join: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// `world.super_region_regions`, read at open.
    super_region_regions: u32,
    /// Test hook ([`StoreHandle::fail_next_writes`]): queued writes still to fail.
    fail_writes: Arc<AtomicU32>,
}

fn stopped() -> StoreError {
    StoreError::Backend("the world-store thread has stopped".into())
}

impl StoreHandle {
    /// Opens the store at `url` on a new `world-store` thread; returns once it
    /// is open (a [`OpenMode::Writer`] creates a missing store and migrates it).
    pub fn open(url: &StoreUrl, dials: Dials, mode: OpenMode) -> Result<StoreHandle, StoreError> {
        let (tx, join, super_region_regions) = thread::spawn(url, dials, mode)?;
        Ok(StoreHandle {
            tx,
            join: Arc::new(Mutex::new(Some(join))),
            super_region_regions,
            fail_writes: Arc::new(AtomicU32::new(0)),
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

    fn ask<T>(&self, make: impl FnOnce(Reply<T>) -> Request) -> Result<T, StoreError> {
        if thread::on_store_thread() {
            // Its answer would come from this very thread: refuse rather than hang.
            return Err(StoreError::Backend(
                "a blocking world-store call was made on the world-store thread itself".into(),
            ));
        }
        let (tx, rx) = crossbeam::channel::bounded(1);
        self.tx.send(make(Reply::Sync(tx))).map_err(|_| stopped())?;
        rx.recv().map_err(|_| stopped())?
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
    /// touches no super-region performs no check at all.
    pub fn commit_cut(
        &self,
        cut: WorldCut,
        fence: Fence,
    ) -> crossbeam::channel::Receiver<Result<CutCommitted, StoreError>> {
        let (tx, rx) = crossbeam::channel::bounded(1);
        if let Some(e) = self.injected_failure() {
            let _ = tx.send(Err(e));
            return rx;
        }
        if let Err(mpsc::error::SendError(req)) = self.tx.send(Request::CommitCut(Box::new(cut), fence, Reply::Sync(tx))) {
            if let Request::CommitCut(_, _, reply) = req {
                reply.send(Err(stopped()));
            }
        }
        rx
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
        self.tx.send(Request::ResolvePlayer(key.to_string(), Reply::Async(tx))).map_err(|_| stopped())?;
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
        if let Err(mpsc::error::SendError(req)) = self.tx.send(Request::SaveCoordinator(state, fence, Reply::Sync(tx))) {
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

    /// Commits everything queued before this call, then stops the thread and
    /// joins it (closing the pool and releasing SQLite's world lock). Other clones'
    /// later calls fail with a `Backend` error.
    pub fn shutdown(self) {
        let _ = self.tx.send(Request::Shutdown);
        let join = self.join.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(join) = join {
            if join.join().is_err() {
                log::error!("[world-store] the store thread panicked");
            }
        }
    }
}
