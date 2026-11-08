//! [`StoreHandle`]: how every other thread reaches the `world-store` thread.
//! Cloneable and `Send + Sync`; its methods block on the answer unless noted.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use game::{
    CutId, InFlightRow, PlayerId, PlayerRecord, RegionCoords, StoredRegion, TerrainSnapshot, Tick,
    WorldCut,
};
use tokio::sync::{mpsc, oneshot};

use crate::thread::{self, Reply, Request};
use crate::{Dials, StoreError, StoreUrl};

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
}

fn stopped() -> StoreError {
    StoreError::Backend("the world-store thread has stopped".into())
}

impl StoreHandle {
    /// Opens (creating it if missing) the store at `url` on a new
    /// `world-store` thread; returns once it is open and migrated.
    pub fn open(url: &StoreUrl, dials: Dials) -> Result<StoreHandle, StoreError> {
        let (tx, join) = thread::spawn(url, dials)?;
        Ok(StoreHandle { tx, join: Arc::new(Mutex::new(Some(join))) })
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
    pub fn load_region(&self, rc: RegionCoords) -> Result<Option<StoredRegion>, StoreError> {
        self.ask(|r| Request::LoadRegion(rc, r))
    }

    /// Only the region's terrain (chunks + ground); `None` if never committed.
    pub fn load_terrain(&self, rc: RegionCoords) -> Result<Option<TerrainSnapshot>, StoreError> {
        self.ask(|r| Request::LoadTerrain(rc, r))
    }

    /// Every stored region with its `terrain_version`.
    pub fn stored_regions(&self) -> Result<BTreeMap<RegionCoords, u64>, StoreError> {
        self.ask(Request::StoredRegions)
    }

    /// The last committed cut's in-flight rows, in their order.
    pub fn load_in_flight(&self) -> Result<Vec<InFlightRow>, StoreError> {
        self.ask(Request::LoadInFlight)
    }

    pub fn load_history(&self) -> Result<Option<Vec<u8>>, StoreError> {
        self.ask(Request::LoadHistory)
    }

    /// Queues the cut's commit (one transaction, all or nothing) and returns at
    /// once; the answer arrives on the receiver. Commits run in call order.
    pub fn commit_cut(&self, cut: WorldCut) -> crossbeam::channel::Receiver<Result<CutCommitted, StoreError>> {
        let (tx, rx) = crossbeam::channel::bounded(1);
        if let Err(mpsc::error::SendError(req)) = self.tx.send(Request::CommitCut(Box::new(cut), Reply::Sync(tx))) {
            if let Request::CommitCut(_, reply) = req {
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

    /// Commits everything queued before this call, then stops the thread and
    /// joins it (closing the pool and releasing the world lock). Other clones'
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
