//! The `world-store` thread: one current-thread tokio runtime that owns the
//! pool and (SQLite) the world lock. Each request is a task on it, so a long commit
//! does not hold up a region load (SQLite WAL readers run beside the writer).
//! Commits run one after another on their own task, in the order they were
//! asked for; every write (a commit, a new player id, the artifact) holds the
//! writer lock, so two write transactions never race. A read-only store
//! answers every write with `StoreError::ReadOnly`.

use std::sync::Arc;

use game::{PlayerId, PlayerRecord, RegionCoords, WorldCut};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::rows;
use crate::{
    CoordinatorFence, CutCommitted, Dials, Fence, LockGuard, OpenMode, Pool, StoreError, StoreUrl, SuperRegion,
    WorldInfo, open_pool,
};

thread_local! {
    /// Set on the `world-store` thread: a blocking [`crate::StoreHandle`] call
    /// made there would wait on the very thread that has to answer it.
    static ON_STORE_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the caller is the `world-store` thread itself.
pub(crate) fn on_store_thread() -> bool {
    ON_STORE_THREAD.with(|f| f.get())
}

/// Where a request's answer goes: a blocking caller's channel or an async one's.
pub(crate) enum Reply<T> {
    Sync(crossbeam::channel::Sender<Result<T, StoreError>>),
    Async(oneshot::Sender<Result<T, StoreError>>),
}

impl<T> Reply<T> {
    pub(crate) fn send(self, v: Result<T, StoreError>) {
        // A caller that gave up waiting is not an error of the store.
        match self {
            Reply::Sync(tx) => {
                let _ = tx.send(v);
            }
            Reply::Async(tx) => {
                let _ = tx.send(v);
            }
        }
    }
}

pub(crate) enum Request {
    WorldInfo(Reply<WorldInfo>),
    ForgedArchive(Reply<Option<Vec<u8>>>),
    PutTiled(Box<crate::forged::TiledRows>, Reply<()>),
    ForgedHead(Reply<Option<crate::forged::ForgedHead>>),
    ForgedTile(worldgen::forged_tiles::TileCoords, Reply<Option<Vec<u8>>>),
    ForgedWorldMap(Reply<Option<Vec<u8>>>),
    LoadRegion(RegionCoords, Fence, Reply<Option<game::StoredRegion>>),
    LoadTerrain(RegionCoords, Reply<Option<game::TerrainSnapshot>>),
    StoredRegions(Reply<std::collections::BTreeMap<RegionCoords, u64>>),
    LoadInFlight(Option<u32>, Reply<Vec<game::InFlightRow>>),
    LoadHistory(Reply<Option<Vec<u8>>>),
    CommitCut(Box<WorldCut>, Fence, Reply<CutCommitted>),
    ResolvePlayer(String, Reply<(PlayerId, Option<PlayerRecord>)>),
    ClaimSuperRegions(u32, Vec<SuperRegion>, Reply<Fence>),
    ClaimCoordinator(Reply<CoordinatorFence>),
    SaveCoordinator(Vec<u8>, CoordinatorFence, Reply<()>),
    LoadCoordinator(Reply<Option<Vec<u8>>>),
    BindTerrain(crate::TerrainKind, Reply<()>),
    TerrainKind(Reply<Option<crate::TerrainKind>>),
    /// Finish the queued commits, then stop.
    Shutdown,
}

/// Calls `rows::<engine>::$f(pool, args…)` on whichever engine `pool` is.
macro_rules! on_pool {
    ($pool:expr, $f:ident($($arg:expr),*)) => {
        match $pool {
            Pool::Sqlite(p) => rows::sqlite::$f(p, $($arg),*).await,
            Pool::Pg(p) => rows::pg::$f(p, $($arg),*).await,
        }
    };
}

/// What [`spawn`] returns: the request sender, the thread, and the store's
/// `world.super_region_regions`.
pub(crate) type Spawned = (mpsc::UnboundedSender<Request>, std::thread::JoinHandle<()>, u32);

/// Opens the store on a new `world-store` thread and returns it once the
/// store is open (or the open's error).
pub(crate) fn spawn(url: &StoreUrl, dials: Dials, mode: OpenMode) -> Result<Spawned, StoreError> {
    let (tx, rx) = mpsc::unbounded_channel();
    let (opened_tx, opened_rx) = crossbeam::channel::bounded(1);
    let url = url.clone();
    let join = std::thread::Builder::new()
        .name("world-store".into())
        .spawn(move || {
            ON_STORE_THREAD.with(|f| f.set(true));
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = opened_tx.send(Err(StoreError::Backend(format!("store runtime: {e}"))));
                    return;
                }
            };
            rt.block_on(async move {
                let opened = match open_pool(&url, dials, mode).await {
                    Ok((pool, guard)) => match on_pool!(&pool, super_region_regions()) {
                        Ok(s) => Ok((pool, guard, s)),
                        Err(e) => Err(e),
                    },
                    Err(e) => Err(e),
                };
                let (pool, guard, s) = match opened {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = opened_tx.send(Err(e));
                        return;
                    }
                };
                let _ = opened_tx.send(Ok(s));
                serve(pool, guard, s, mode == OpenMode::ReadOnly, rx).await;
            });
        })
        .map_err(|e| StoreError::Backend(format!("spawn the world-store thread: {e}")))?;
    match opened_rx.recv() {
        Ok(Ok(s)) => Ok((tx, join, s)),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(_) => {
            let _ = join.join();
            Err(StoreError::Backend("the world-store thread died while opening".into()))
        }
    }
}

async fn serve(pool: Pool, guard: LockGuard, s: u32, read_only: bool, mut rx: mpsc::UnboundedReceiver<Request>) {
    let writer = Arc::new(Mutex::new(()));
    // Commits in order on one task.
    let (commit_tx, mut commit_rx) = mpsc::unbounded_channel::<(Box<WorldCut>, Fence, Reply<CutCommitted>)>();
    let committer = {
        let (pool, writer) = (pool.clone(), writer.clone());
        tokio::spawn(async move {
            while let Some((cut, fence, reply)) = commit_rx.recv().await {
                let _w = writer.lock().await;
                let result = on_pool!(&pool, commit_cut(&cut, &fence, s));
                match &result {
                    Ok(c) => log::debug!("[world-store] cut {} at tick {} committed ({} regions)", c.id.0, cut.tick, c.versions.len()),
                    Err(e) => log::error!("[world-store] cut {} at tick {} failed: {e}", cut.id.0, cut.tick),
                }
                reply.send(result);
            }
        })
    };
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    while let Some(req) = rx.recv().await {
        tasks.retain(|t| !t.is_finished());
        let (pool, writer) = (pool.clone(), writer.clone());
        let req = match refuse_if_read_only(req, read_only) {
            Some(req) => req,
            None => continue,
        };
        let task = match req {
            Request::Shutdown => break,
            Request::CommitCut(cut, fence, reply) => {
                if let Err(mpsc::error::SendError((_, _, reply))) = commit_tx.send((cut, fence, reply)) {
                    reply.send(Err(StoreError::Backend("the world-store committer has stopped".into())));
                }
                continue;
            }
            Request::WorldInfo(r) => tokio::spawn(async move { r.send(on_pool!(&pool, world_info())) }),
            Request::ForgedArchive(r) => tokio::spawn(async move { r.send(on_pool!(&pool, forged_archive())) }),
            Request::PutTiled(rows, r) => tokio::spawn(async move {
                let _w = writer.lock().await;
                r.send(on_pool!(&pool, put_tiled(rows)))
            }),
            Request::ForgedHead(r) => tokio::spawn(async move { r.send(on_pool!(&pool, forged_head())) }),
            Request::ForgedTile(t, r) => tokio::spawn(async move { r.send(on_pool!(&pool, forged_tile(t))) }),
            Request::ForgedWorldMap(r) => tokio::spawn(async move { r.send(on_pool!(&pool, forged_world_map())) }),
            Request::LoadRegion(rc, fence, r) => {
                tokio::spawn(async move { r.send(on_pool!(&pool, load_region(rc, &fence, s))) })
            }
            Request::LoadTerrain(rc, r) => tokio::spawn(async move { r.send(on_pool!(&pool, load_terrain(rc))) }),
            Request::StoredRegions(r) => tokio::spawn(async move { r.send(on_pool!(&pool, stored_regions())) }),
            Request::LoadInFlight(host, r) => {
                tokio::spawn(async move { r.send(on_pool!(&pool, load_in_flight(host))) })
            }
            Request::LoadHistory(r) => tokio::spawn(async move { r.send(on_pool!(&pool, load_history())) }),
            Request::ResolvePlayer(token, r) => tokio::spawn(async move {
                // A known token is a read: no wait behind a long commit. A new one
                // inserts and reads back; the database assigns the id. On SQLite the
                // insert takes the writer lock: a write beside a commit would either wait
                // out the busy timeout or (the commit's transaction having read first) fail
                // the commit with SQLITE_BUSY_SNAPSHOT. Postgres needs none.
                match on_pool!(&pool, find_player(&token)) {
                    Ok(Some(found)) => return r.send(Ok(found)),
                    Ok(None) if read_only => return r.send(Err(StoreError::ReadOnly)),
                    Ok(None) => {}
                    Err(e) => return r.send(Err(e)),
                }
                let _w = match &pool {
                    Pool::Sqlite(_) => Some(writer.lock().await),
                    Pool::Pg(_) => None,
                };
                r.send(on_pool!(&pool, resolve_player(&token)))
            }),
            Request::ClaimSuperRegions(owner, srs, r) => tokio::spawn(async move {
                let _w = writer.lock().await;
                r.send(on_pool!(&pool, claim_super_regions(owner, srs)))
            }),
            Request::ClaimCoordinator(r) => tokio::spawn(async move {
                let _w = writer.lock().await;
                r.send(on_pool!(&pool, claim_coordinator()))
            }),
            Request::SaveCoordinator(state, fence, r) => tokio::spawn(async move {
                let _w = writer.lock().await;
                r.send(on_pool!(&pool, save_coordinator(state, fence)))
            }),
            Request::LoadCoordinator(r) => tokio::spawn(async move { r.send(on_pool!(&pool, load_coordinator())) }),
            Request::BindTerrain(kind, r) => tokio::spawn(async move {
                let _w = writer.lock().await;
                r.send(on_pool!(&pool, bind_terrain(kind)))
            }),
            Request::TerrainKind(r) => tokio::spawn(async move { r.send(on_pool!(&pool, terrain_kind())) }),
        };
        tasks.push(task);
    }
    // Drain: every commit queued so far, then the requests still running.
    drop(commit_tx);
    let _ = committer.await;
    for t in tasks {
        let _ = t.await;
    }
    match &pool {
        Pool::Sqlite(p) => p.close().await,
        Pool::Pg(p) => p.close().await,
    }
    drop(guard);
}

/// On a read-only store, answers a write request with `ReadOnly` (`None`);
/// passes every other request through. A player resolve is a read unless the
/// key is new — that case is refused where it is served.
fn refuse_if_read_only(req: Request, read_only: bool) -> Option<Request> {
    if !read_only {
        return Some(req);
    }
    match req {
        Request::CommitCut(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::PutTiled(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::ClaimSuperRegions(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::ClaimCoordinator(r) => r.send(Err(StoreError::ReadOnly)),
        Request::SaveCoordinator(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::BindTerrain(_, r) => r.send(Err(StoreError::ReadOnly)),
        req => return Some(req),
    }
    None
}
