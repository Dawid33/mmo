//! The `world-store` thread: one current-thread tokio runtime that owns the
//! pool and (SQLite) the world lock. Each request is a task on it, so a long commit
//! does not hold up a region load (SQLite WAL readers run beside the writer).
//! Commits run one after another on their own task, in the order they were
//! asked for; every write (a commit, a new player id, the artifact) holds the
//! writer lock, so two write transactions never race. A read-only store
//! answers every write with `StoreError::ReadOnly`.

use std::sync::Arc;
use std::time::Instant;
use std::sync::atomic::{AtomicU64, Ordering};

use game::{CutId, PlayerId, PlayerRecord, RegionCoords, Tick, WorldCut};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::counters::Counters;
use crate::rows::{self, CutEnd};
use crate::succession::{self, SUCCESSION_UNSUPPORTED};
use crate::two_phase::{self, TWO_PHASE_UNSUPPORTED};
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
enum ReplyTo<T> {
    Sync(crossbeam::channel::Sender<Result<T, StoreError>>),
    Async(oneshot::Sender<Result<T, StoreError>>),
}

/// A request's answer channel, stamped with when the request was made: sending the
/// answer on the store thread counts the request's end-to-end latency (the one choke
/// point of every replying request kind).
pub(crate) struct Reply<T> {
    to: ReplyTo<T>,
    sent: Instant,
}

impl<T> Reply<T> {
    pub(crate) fn sync(tx: crossbeam::channel::Sender<Result<T, StoreError>>) -> Self {
        Reply { to: ReplyTo::Sync(tx), sent: Instant::now() }
    }

    pub(crate) fn asynch(tx: oneshot::Sender<Result<T, StoreError>>) -> Self {
        Reply { to: ReplyTo::Async(tx), sent: Instant::now() }
    }

    pub(crate) fn send(self, v: Result<T, StoreError>) {
        crate::counters::record_latency(self.sent.elapsed());
        // A caller that gave up waiting is not an error of the store.
        match self.to {
            ReplyTo::Sync(tx) => {
                let _ = tx.send(v);
            }
            ReplyTo::Async(tx) => {
                let _ = tx.send(v);
            }
        }
    }
}

/// A request with the instant it was sent, for the store thread's wait counter.
pub(crate) struct Stamped {
    pub sent: Instant,
    pub req: Request,
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
    StoredRegionsSince(Tick, Reply<std::collections::BTreeMap<RegionCoords, u64>>),
    /// `pg_notify` of a committed cut (SQLite: nothing to do).
    NotifyCut(CutId, Tick, Reply<()>),
    LoadInFlight(Option<u32>, Reply<Vec<game::InFlightRow>>),
    LoadHistory(Reply<Option<Vec<u8>>>),
    CommitCut(Box<WorldCut>, Fence, Reply<CutCommitted>),
    ResolvePlayer(String, Reply<(PlayerId, Option<PlayerRecord>)>),
    ClaimSuperRegions(u32, Vec<SuperRegion>, Reply<Fence>),
    /// Claim only the absent or free super-regions of the list.
    ClaimFree(u32, Vec<SuperRegion>, Reply<Fence>),
    /// Free the fence's super-regions.
    Release(Fence, Reply<()>),
    ClaimCoordinator(Reply<CoordinatorFence>),
    SaveCoordinator(Vec<u8>, CoordinatorFence, Reply<()>),
    LoadCoordinator(Reply<Option<Vec<u8>>>),
    EnsureClockAnchor(game::ClockAnchor, Reply<game::ClockAnchor>),
    ClearClockAnchor(Reply<()>),
    SetSuperRegionRegions(u32, Reply<bool>),
    RegisterHost(crate::HostRow, Fence, game::CrashPolicy, Reply<crate::BootVerdict>),
    Hosts(Reply<Vec<crate::HostRow>>),
    WorldGeneration(Reply<u64>),
    Heartbeat(u32, u64, Reply<bool>),
    Owners(Reply<Vec<(SuperRegion, i64, u64)>>),
    OwnedBy(u32, Reply<Fence>),
    DbNowMs(Reply<u64>),
    TwoPhaseReady(Reply<()>),
    /// A host's part of a two-phase cut, at the host's world generation.
    PrepareCut(Box<WorldCut>, Fence, u64, Reply<CutCommitted>),
    DecideCut(Box<Decision>, Reply<bool>),
    AbortCut(CutId, Tick, Reply<bool>),
    ResolvePrepared(Reply<Vec<(CutId, bool)>>),
    CutDecision(CutId, Reply<Option<bool>>),
    CutDecided(CutId, Reply<Option<bool>>),
    /// R4: the coordinator declares a succession (Postgres).
    DeclareSuccession(Box<Declare>, Reply<crate::Declared>),
    /// R4: the coordinator rolls the world back (Postgres).
    RollbackWorld(CoordinatorFence, Reply<u64>),
    OpenSuccessions(Reply<Vec<crate::Succession>>),
    /// An heir reads its predecessor's in-flight rows.
    LoadInFlightOf(u32, Fence, Reply<Vec<game::InFlightRow>>),
    RegionsOfEntities(Vec<game::EntityId>, Reply<Vec<(game::EntityId, RegionCoords)>>),
    /// Finish the queued commits, then stop.
    Shutdown,
}

/// [`crate::StoreHandle::decide_cut`]'s arguments.
pub(crate) struct Decision {
    pub cut: CutId,
    pub tick: Tick,
    pub commit: bool,
    pub hosts: Vec<u32>,
    pub leaving: Vec<u32>,
    pub fence: CoordinatorFence,
    pub generation: u64,
    /// The open successions a commit settles (R4).
    pub settles: Vec<crate::SuccessionId>,
    /// Test hook: fail after this many `COMMIT PREPARED`s.
    pub stop_after: Option<u32>,
}

/// [`crate::StoreHandle::declare_succession`]'s arguments.
pub(crate) struct Declare {
    pub dead: Vec<(u32, u64)>,
    pub heirs: Vec<u32>,
    pub kind: crate::SuccessionKind,
    pub fence: CoordinatorFence,
    pub generation: u64,
}

/// Cut state shared by a handle (and its clones) and its store thread.
pub(crate) struct CutState {
    /// The cut whose part this handle prepared (or is preparing) and has not yet seen
    /// decided: every further cut is refused with `CutPending` until
    /// [`crate::StoreHandle::cut_decided`] (or a prepare that ends with nothing staged)
    /// clears it.
    pub pending: std::sync::Mutex<Option<CutId>>,
    /// `lock_timeout` of every cut transaction, ms ([`crate::CUT_LOCK_TIMEOUT_MS`]).
    pub lock_timeout_ms: AtomicU64,
    /// Committed world cuts this handle decided that hold an entity in two regions
    /// ([`two_phase::report_entities_twice`]).
    pub entities_twice_cuts: AtomicU64,
    /// Committed world cuts this handle decided whose check has run (found or not).
    pub entities_checked_cuts: AtomicU64,
}

impl CutState {
    pub(crate) fn new() -> CutState {
        CutState {
            pending: std::sync::Mutex::new(None),
            lock_timeout_ms: AtomicU64::new(crate::CUT_LOCK_TIMEOUT_MS),
            entities_twice_cuts: AtomicU64::new(0),
            entities_checked_cuts: AtomicU64::new(0),
        }
    }

    pub(crate) fn pending(&self) -> std::sync::MutexGuard<'_, Option<CutId>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Forgets `cut`'s pending part (if it is the pending one).
    pub(crate) fn settle(&self, cut: CutId) {
        let mut p = self.pending();
        if *p == Some(cut) {
            *p = None;
        }
    }
}

/// One queued cut for the committer: the cut, its fence, how it ends, the answer.
type QueuedCut = (Box<WorldCut>, Fence, CutEnd, Reply<CutCommitted>);

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
pub(crate) type Spawned = (mpsc::UnboundedSender<Stamped>, std::thread::JoinHandle<()>, u32);

/// Opens the store on a new `world-store` thread and returns it once the
/// store is open (or the open's error).
pub(crate) fn spawn(
    url: &StoreUrl,
    dials: Dials,
    mode: OpenMode,
    cuts: Arc<CutState>,
    counters: Arc<Counters>,
) -> Result<Spawned, StoreError> {
    let (tx, rx) = mpsc::unbounded_channel();
    let (opened_tx, opened_rx) = crossbeam::channel::bounded(1);
    let url = url.clone();
    let join = std::thread::Builder::new()
        .name("world-store".into())
        .spawn(move || {
            ON_STORE_THREAD.with(|f| f.set(true));
            crate::counters::install(counters.clone());
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
                        Ok(s) => match &pool {
                            Pool::Sqlite(_) => Ok((pool, guard, s, None)),
                            Pool::Pg(p) => match two_phase::world_tag(p).await {
                                Ok(tag) => Ok((pool, guard, s, Some(tag))),
                                Err(e) => Err(e),
                            },
                        },
                        Err(e) => Err(e),
                    },
                    Err(e) => Err(e),
                };
                let (pool, guard, s, world_tag) = match opened {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = opened_tx.send(Err(e));
                        return;
                    }
                };
                let _ = opened_tx.send(Ok(s));
                serve(pool, guard, s, world_tag, cuts, counters, mode == OpenMode::ReadOnly, rx).await;
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

/// `world_tag`: the Postgres database's OID (the gids of its prepared cut parts);
/// `None` on SQLite.
async fn serve(
    pool: Pool,
    guard: LockGuard,
    s: u32,
    world_tag: Option<u32>,
    cuts: Arc<CutState>,
    counters: Arc<Counters>,
    read_only: bool,
    mut rx: mpsc::UnboundedReceiver<Stamped>,
) {
    let writer = Arc::new(Mutex::new(()));
    // Commits and prepares in order on one task.
    let (commit_tx, mut commit_rx) = mpsc::unbounded_channel::<QueuedCut>();
    let committer = {
        let (pool, writer, cuts) = (pool.clone(), writer.clone(), cuts.clone());
        tokio::spawn(async move {
            while let Some((cut, fence, end, reply)) = commit_rx.recv().await {
                let _w = writer.lock().await;
                let lock_timeout_ms = cuts.lock_timeout_ms.load(Ordering::SeqCst);
                let result = on_pool!(&pool, run_cut(&cut, &fence, s, &end, lock_timeout_ms));
                let result = match (&pool, &end) {
                    (Pool::Pg(p), CutEnd::Prepare { gid, .. }) => settle_prepare(p, gid, cut.id, result, &cuts).await,
                    _ => result,
                };
                let what = match end {
                    CutEnd::Commit => "committed",
                    CutEnd::Prepare { .. } => "prepared",
                };
                match &result {
                    Ok(c) => log::debug!("[world-store] cut {} at tick {} {what} ({} regions)", c.id.0, cut.tick, c.versions.len()),
                    Err(e) => log::error!("[world-store] cut {} at tick {} ({what}) failed: {e}", cut.id.0, cut.tick),
                }
                reply.send(result);
            }
        })
    };
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    while let Some(Stamped { sent, req }) = rx.recv().await {
        // Every request kind is counted here, as the thread picks it up (a stop is no request).
        if !matches!(req, Request::Shutdown) {
            counters.record_request(sent.elapsed());
        }
        tasks.retain(|t| !t.is_finished());
        let (pool, writer) = (pool.clone(), writer.clone());
        let req = match refuse_if_read_only(req, read_only, &cuts) {
            Some(req) => req,
            None => continue,
        };
        let task = match req {
            Request::Shutdown => break,
            Request::CommitCut(cut, fence, reply) => {
                queue_cut(&commit_tx, (cut, fence, CutEnd::Commit, reply));
                continue;
            }
            Request::PrepareCut(cut, fence, generation, reply) => {
                let Some(tag) = world_tag else {
                    cuts.settle(cut.id);
                    reply.send(Err(StoreError::Unsupported(TWO_PHASE_UNSUPPORTED.into())));
                    continue;
                };
                let gid = two_phase::cut_gid(tag, cut.id, cut.host);
                queue_cut(&commit_tx, (cut, fence, CutEnd::Prepare { gid, generation }, reply));
                continue;
            }
            Request::TwoPhaseReady(r) => tokio::spawn(async move {
                r.send(match &pool {
                    Pool::Pg(p) => two_phase::two_phase_ready(p).await,
                    Pool::Sqlite(_) => Err(StoreError::Unsupported(TWO_PHASE_UNSUPPORTED.into())),
                })
            }),
            // No writer lock for the decision, the abort or the sweep: a prepare
            // queued behind a prepared part's row lock waits for the decision.
            Request::DecideCut(d, r) => {
                let cuts = cuts.clone();
                tokio::spawn(async move {
                    let (cut, tick) = (d.cut, d.tick);
                    let decided = match (&pool, world_tag) {
                        (Pool::Pg(p), Some(tag)) => {
                            let Decision { cut, tick, commit, hosts, leaving, fence, generation, settles, stop_after } = *d;
                            two_phase::decide_cut(
                                p, tag, cut, tick, commit, hosts, leaving, fence, generation, settles, stop_after,
                            )
                            .await
                        }
                        _ => Err(StoreError::Unsupported(TWO_PHASE_UNSUPPORTED.into())),
                    };
                    let committed = matches!(decided, Ok(true));
                    // Answered first: the check below never delays or fails the decision.
                    r.send(decided);
                    if let (true, Pool::Pg(p)) = (committed, &pool) {
                        if two_phase::report_entities_twice(p, cut, tick).await {
                            cuts.entities_twice_cuts.fetch_add(1, Ordering::SeqCst);
                        }
                        cuts.entities_checked_cuts.fetch_add(1, Ordering::SeqCst);
                    }
                })
            }
            Request::AbortCut(cut, tick, r) => tokio::spawn(async move {
                r.send(match &pool {
                    Pool::Pg(p) => two_phase::abort_cut(p, world_tag.expect("set on Postgres"), cut, tick).await,
                    Pool::Sqlite(_) => Err(StoreError::Unsupported(TWO_PHASE_UNSUPPORTED.into())),
                })
            }),
            Request::CutDecided(cut, r) => {
                let cuts = cuts.clone();
                tokio::spawn(async move {
                    let d = match (&pool, world_tag) {
                        (Pool::Pg(p), Some(tag)) => two_phase::cut_decided(p, tag, cut).await,
                        _ => on_pool!(&pool, cut_decision(cut)),
                    };
                    if let Ok(Some(_)) = d {
                        cuts.settle(cut);
                    }
                    r.send(d)
                })
            }
            Request::ResolvePrepared(r) => tokio::spawn(async move {
                r.send(match (&pool, world_tag) {
                    (Pool::Pg(p), Some(tag)) => two_phase::resolve_prepared(p, tag).await,
                    _ => Err(StoreError::Unsupported(TWO_PHASE_UNSUPPORTED.into())),
                })
            }),
            // No writer lock (as a decision): a declaration waits, bounded by the cut lock
            // timeout, for a staged part holding the predecessor's super-regions.
            Request::DeclareSuccession(d, r) => {
                let lock_timeout_ms = cuts.lock_timeout_ms.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    r.send(match &pool {
                        Pool::Pg(p) => {
                            let Declare { dead, heirs, kind, fence, generation } = *d;
                            succession::declare(p, dead, heirs, kind, fence, generation, lock_timeout_ms).await
                        }
                        Pool::Sqlite(_) => Err(StoreError::Unsupported(SUCCESSION_UNSUPPORTED.into())),
                    })
                })
            }
            Request::RollbackWorld(fence, r) => {
                let lock_timeout_ms = cuts.lock_timeout_ms.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    r.send(match &pool {
                        Pool::Pg(p) => succession::rollback_world(p, fence, lock_timeout_ms).await,
                        Pool::Sqlite(_) => Err(StoreError::Unsupported(SUCCESSION_UNSUPPORTED.into())),
                    })
                })
            }
            Request::OpenSuccessions(r) => tokio::spawn(async move { r.send(on_pool!(&pool, open_successions())) }),
            Request::LoadInFlightOf(host, fence, r) => {
                tokio::spawn(async move { r.send(on_pool!(&pool, load_in_flight_of(host, &fence))) })
            }
            Request::RegionsOfEntities(ids, r) => {
                tokio::spawn(async move { r.send(on_pool!(&pool, regions_of_entities(ids))) })
            }
            Request::CutDecision(cut, r) => tokio::spawn(async move { r.send(on_pool!(&pool, cut_decision(cut))) }),
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
            Request::StoredRegionsSince(tick, r) => {
                tokio::spawn(async move { r.send(on_pool!(&pool, stored_regions_since(tick))) })
            }
            // No writer lock: it writes no row. Logged here, as callers need not wait for it.
            Request::NotifyCut(cut, tick, r) => tokio::spawn(async move {
                let done = match &pool {
                    Pool::Pg(p) => rows::notify_cut(p, cut, tick).await,
                    Pool::Sqlite(_) => Ok(()),
                };
                if let Err(e) = &done {
                    log::warn!("[world-store] the notice of cut {} at tick {tick} was not sent ({e}); listeners resync by their poll", cut.0);
                }
                r.send(done)
            }),
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
                r.send(on_pool!(&pool, claim_super_regions(owner, srs, s)))
            }),
            Request::ClaimFree(owner, srs, r) => tokio::spawn(async move {
                let _w = writer.lock().await;
                r.send(on_pool!(&pool, claim_free(owner, srs, s)))
            }),
            Request::Release(fence, r) => tokio::spawn(async move {
                let _w = writer.lock().await;
                r.send(on_pool!(&pool, release(fence)))
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
            Request::EnsureClockAnchor(proposed, r) => tokio::spawn(async move {
                let _w = writer.lock().await;
                r.send(on_pool!(&pool, ensure_clock_anchor(proposed)))
            }),
            Request::ClearClockAnchor(r) => tokio::spawn(async move {
                let _w = writer.lock().await;
                r.send(on_pool!(&pool, clear_clock_anchor()))
            }),
            Request::SetSuperRegionRegions(n, r) => tokio::spawn(async move {
                let _w = writer.lock().await;
                r.send(on_pool!(&pool, set_super_region_regions(n)))
            }),
            Request::RegisterHost(row, fence, policy, r) => tokio::spawn(async move {
                let _w = writer.lock().await;
                r.send(on_pool!(&pool, register_host(row, &fence, policy)))
            }),
            Request::Hosts(r) => tokio::spawn(async move { r.send(on_pool!(&pool, hosts())) }),
            Request::WorldGeneration(r) => tokio::spawn(async move { r.send(on_pool!(&pool, world_generation())) }),
            // Postgres: no writer lock (it is in-process and a claim can hold it for a minute; a
            // late beat would lapse the lease) — the statement writes only the caller's own row.
            // SQLite has one writer at a time, so it takes the lock like every write.
            Request::Heartbeat(ordinal, boot, r) => tokio::spawn(async move {
                let _w = match &pool {
                    Pool::Sqlite(_) => Some(writer.lock().await),
                    Pool::Pg(_) => None,
                };
                r.send(on_pool!(&pool, heartbeat(ordinal, boot)))
            }),
            Request::Owners(r) => tokio::spawn(async move { r.send(on_pool!(&pool, owners())) }),
            Request::OwnedBy(owner, r) => tokio::spawn(async move { r.send(on_pool!(&pool, owned_by(owner))) }),
            Request::DbNowMs(r) => tokio::spawn(async move { r.send(on_pool!(&pool, db_now_ms())) }),
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

/// After a part's prepare: a part staged for a cut that is decided already finishes
/// itself (an abort answers `CutAborted`). The pending mark is cleared once the part
/// is certainly not staged — finished here, or a failed prepare whose gid is not in
/// `pg_prepared_xacts` (a PREPARE whose reply was lost leaves it staged: the mark
/// stays, and the caller settles it with `abort_cut` + `cut_decided`). Kept for an
/// undecided staged part.
async fn settle_prepare(
    p: &sqlx::PgPool,
    gid: &str,
    cut: CutId,
    result: Result<CutCommitted, StoreError>,
    cuts: &CutState,
) -> Result<CutCommitted, StoreError> {
    let (result, settled) = match result {
        Ok(c) => match two_phase::finish_if_decided(p, gid, cut).await {
            Ok(None) => (Ok(c), false),
            Ok(Some(true)) => (Ok(c), true),
            Ok(Some(false)) => (Err(StoreError::CutAborted(cut)), true),
            Err(e) => (Err(e), false),
        },
        Err(e) => {
            let staged = two_phase::is_staged(p, gid).await;
            if !matches!(staged, Ok(false)) {
                log::error!("[world-store] part {gid} failed ({e}) and may be staged ({staged:?}); this handle's cuts wait for cut_decided");
            }
            (Err(e), matches!(staged, Ok(false)))
        }
    };
    if settled {
        cuts.settle(cut);
    }
    result
}

/// Hands a cut to the committer task.
fn queue_cut(commit_tx: &mpsc::UnboundedSender<QueuedCut>, queued: QueuedCut) {
    if let Err(mpsc::error::SendError((_, _, _, reply))) = commit_tx.send(queued) {
        reply.send(Err(StoreError::Backend("the world-store committer has stopped".into())));
    }
}

/// On a read-only store, answers a write request with `ReadOnly` (`None`);
/// passes every other request through. A player resolve is a read unless the
/// key is new — that case is refused where it is served.
fn refuse_if_read_only(req: Request, read_only: bool, cuts: &CutState) -> Option<Request> {
    if !read_only {
        return Some(req);
    }
    match req {
        Request::CommitCut(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::PutTiled(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::ClaimSuperRegions(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::ClaimFree(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::Release(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::ClaimCoordinator(r) => r.send(Err(StoreError::ReadOnly)),
        Request::SaveCoordinator(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::EnsureClockAnchor(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::ClearClockAnchor(r) => r.send(Err(StoreError::ReadOnly)),
        Request::SetSuperRegionRegions(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::RegisterHost(_, _, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::PrepareCut(c, _, _, r) => {
            cuts.settle(c.id);
            r.send(Err(StoreError::ReadOnly))
        }
        Request::DecideCut(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::AbortCut(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::ResolvePrepared(r) => r.send(Err(StoreError::ReadOnly)),
        Request::CutDecided(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::DeclareSuccession(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::RollbackWorld(_, r) => r.send(Err(StoreError::ReadOnly)),
        // A read-only handle sends nothing, a notice included.
        Request::NotifyCut(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        req => return Some(req),
    }
    None
}
