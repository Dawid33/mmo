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

use game::HostId;
use game::{CutId, PlayerId, PlayerRecord, RegionCoords, Tick, WorldCut};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::counters::Counters;
use crate::rows::{self, i, CutEnd};
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

/// What a test asks of the store thread itself ([`Request::TestHook`]).
pub(crate) enum TestHook {
    /// [`crate::StoreHandle::set_fenced_tx_idle_ms`].
    FencedTxIdleMs(u64),
    /// [`crate::StoreHandle::freeze_in_fenced_tx`].
    FreezeInFencedTx(Fence, u64),
    /// [`crate::StoreHandle::hold_pool_connections`].
    HoldConnections(u32, u64),
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
    LoadInFlight(Option<HostId>, Reply<Vec<game::InFlightRow>>),
    LoadHistory(Reply<Option<Vec<u8>>>),
    CommitCut(Box<WorldCut>, Fence, Reply<CutCommitted>),
    ResolvePlayer(String, Reply<(PlayerId, Option<PlayerRecord>)>),
    /// Claim the list; with a boot, only while that boot's `hosts` row is live.
    ClaimSuperRegions(HostId, Vec<SuperRegion>, Option<u64>, Reply<Fence>),
    /// Claim only the absent or free super-regions of the list, as the live boot given.
    ClaimFree(HostId, u64, Vec<SuperRegion>, Reply<Fence>),
    /// Free the fence's super-regions.
    Release(Fence, Reply<()>),
    ClaimCoordinator(Reply<CoordinatorFence>),
    SaveCoordinator(Vec<u8>, CoordinatorFence, Reply<()>),
    LoadCoordinator(Reply<Option<Vec<u8>>>),
    EnsureClockAnchor(game::ClockAnchor, Reply<game::ClockAnchor>),
    ClearClockAnchor(Reply<()>),
    SetSuperRegionRegions(u32, Reply<bool>),
    RegisterHost(crate::NewHost, game::CrashPolicy, Reply<crate::Registered>),
    Hosts(Reply<Vec<crate::HostRow>>),
    WorldGeneration(Reply<u64>),
    Heartbeat(HostId, u64, Reply<crate::Beat>),
    DrainHost(HostId, u64, Reply<()>),
    Owners(Reply<Vec<(SuperRegion, i64, u64)>>),
    OwnedBy(HostId, Reply<Fence>),
    DbNowMs(Reply<u64>),
    TwoPhaseReady(Reply<()>),
    /// A host's part of a two-phase cut, at the host's world generation.
    PrepareCut(Box<WorldCut>, Fence, u64, Reply<CutCommitted>),
    DecideCut(Box<Decision>, Reply<bool>),
    AbortCut(CutId, Tick, Reply<bool>),
    /// Elastic 1: the coordinator records a world cut's begin (Postgres).
    RecordCutBegin(CutId, crate::CoordinatorFence, Reply<HostId>),
    ResolvePrepared(Reply<Vec<(CutId, bool)>>),
    StagedCutParts(Reply<Vec<(CutId, HostId)>>),
    CutDecision(CutId, Reply<Option<bool>>),
    CutDecided(CutId, Reply<Option<bool>>),
    LastCommittedCut(Reply<CutId>),
    /// R4: the coordinator declares a succession (Postgres).
    DeclareSuccession(Box<Declare>, Reply<crate::Declared>),
    /// R4: the coordinator rolls the world back (Postgres).
    RollbackWorld(CoordinatorFence, Reply<u64>),
    /// R4: the operator rolls the world back (`worldctl rollback`; Postgres): `(generation,
    /// successions abandoned)`.
    RollbackWorldOperator(Reply<(u64, u64)>),
    /// R4: the operator retires a host while no host runs (`worldctl retire-host`; Postgres).
    RetireOffline(HostId, Reply<crate::Retired>),
    /// R4: the operator vouches for a lapsed host's death (`worldctl retire-host`; Postgres).
    VouchHost(HostId, Reply<crate::Vouched>),
    OpenSuccessions(Reply<Vec<crate::Succession>>),
    /// An heir reads its predecessor's in-flight rows.
    LoadInFlightOf(HostId, Fence, Reply<Vec<game::InFlightRow>>),
    RegionsOfEntities(Vec<game::EntityId>, Reply<Vec<(game::EntityId, RegionCoords)>>),
    /// Test support (the handle's `#[doc(hidden)]` hooks).
    TestHook(TestHook, Reply<()>),
    /// Finish the queued commits, then stop.
    Shutdown,
}

/// [`crate::StoreHandle::decide_cut`]'s arguments.
pub(crate) struct Decision {
    pub cut: CutId,
    pub tick: Tick,
    pub commit: bool,
    pub hosts: Vec<HostId>,
    pub leaving: Vec<HostId>,
    pub fence: CoordinatorFence,
    pub generation: u64,
    /// The open successions a commit settles (R4).
    pub settles: Vec<crate::SuccessionId>,
    /// Test hook: fail after this many `COMMIT PREPARED`s.
    pub stop_after: Option<u32>,
}

/// [`crate::StoreHandle::declare_succession`]'s arguments.
pub(crate) struct Declare {
    pub dead: Vec<(HostId, u64)>,
    pub heirs: Vec<HostId>,
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
    /// `lock_timeout` of every claim, release and registration, ms
    /// ([`crate::CLAIM_LOCK_TIMEOUT_MS`]; a test hook lowers it,
    /// [`crate::StoreHandle::set_claim_lock_timeout_ms`]).
    pub claim_lock_timeout_ms: AtomicU64,
    /// The host lease a declaration and a boot judge a row's `seen_ms` by, ms
    /// ([`game::HOST_LEASE_MS`]; a test hook shortens it, [`crate::StoreHandle::set_host_lease_ms`]).
    pub host_lease_ms: AtomicU64,
    /// Committed world cuts this handle decided that hold an entity in two regions
    /// ([`two_phase::report_entities_twice`]).
    pub entities_twice_cuts: AtomicU64,
    /// Committed world cuts this handle decided whose check has run (found or not).
    pub entities_checked_cuts: AtomicU64,
    /// Committed world cuts this handle decided that settled a succession whose
    /// predecessor's in-flight rows were not adopted ([`two_phase::report_settled_unadopted`]).
    pub settled_unadopted_cuts: AtomicU64,
}

impl CutState {
    pub(crate) fn new() -> CutState {
        CutState {
            pending: std::sync::Mutex::new(None),
            lock_timeout_ms: AtomicU64::new(crate::CUT_LOCK_TIMEOUT_MS),
            claim_lock_timeout_ms: AtomicU64::new(crate::CLAIM_LOCK_TIMEOUT_MS),
            host_lease_ms: AtomicU64::new(game::HOST_LEASE_MS),
            entities_twice_cuts: AtomicU64::new(0),
            entities_checked_cuts: AtomicU64::new(0),
            settled_unadopted_cuts: AtomicU64::new(0),
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
            // The heartbeat's own connection (Postgres): never one of the shared pool's.
            let beat = match &url {
                StoreUrl::Postgres(u) => u.parse::<sqlx::postgres::PgConnectOptions>().ok().map(|o| o.application_name(HEARTBEAT_APP)),
                _ => None,
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
                serve(pool, guard, s, world_tag, cuts, counters, mode == OpenMode::ReadOnly, beat, rx).await;
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
    beat: Option<sqlx::postgres::PgConnectOptions>,
    mut rx: mpsc::UnboundedReceiver<Stamped>,
) {
    let writer = Arc::new(Mutex::new(()));
    // Postgres: the lease beats go out on one connection of their own, one at a time, each
    // under a client-side deadline (`beats`).
    let (beat_tx, beater) = match beat {
        Some(options) => {
            let (tx, rx) = mpsc::unbounded_channel::<BeatJob>();
            (Some(tx), Some(tokio::spawn(beats(options, rx))))
        }
        None => (None, None),
    };
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
                    // Answered first: the checks below never delay or fail the decision.
                    r.send(decided);
                    if let (true, Pool::Pg(p)) = (committed, &pool) {
                        if two_phase::report_settled_unadopted(p, cut).await {
                            cuts.settled_unadopted_cuts.fetch_add(1, Ordering::SeqCst);
                        }
                        if two_phase::report_entities_twice(p, cut, tick).await {
                            cuts.entities_twice_cuts.fetch_add(1, Ordering::SeqCst);
                        }
                        cuts.entities_checked_cuts.fetch_add(1, Ordering::SeqCst);
                    }
                })
            }
            Request::RecordCutBegin(cut, fence, r) => tokio::spawn(async move {
                r.send(match &pool {
                    Pool::Pg(p) => two_phase::record_cut_begin(p, cut, fence).await,
                    Pool::Sqlite(_) => Err(StoreError::Unsupported(TWO_PHASE_UNSUPPORTED.into())),
                })
            }),
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
            // A read of the prepared transactions' names; SQLite stages nothing.
            Request::StagedCutParts(r) => tokio::spawn(async move {
                r.send(match (&pool, world_tag) {
                    (Pool::Pg(p), Some(tag)) => two_phase::staged_cut_parts(p, tag).await,
                    _ => Ok(Vec::new()),
                })
            }),
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
                let lease_ms = cuts.host_lease_ms.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    r.send(match &pool {
                        Pool::Pg(p) => {
                            let Declare { dead, heirs, kind, fence, generation } = *d;
                            succession::declare(p, dead, heirs, kind, fence, generation, lock_timeout_ms, lease_ms).await
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
            Request::RollbackWorldOperator(r) => {
                let lock_timeout_ms = cuts.lock_timeout_ms.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    r.send(match &pool {
                        Pool::Pg(p) => crate::operator::rollback_world_operator(p, lock_timeout_ms).await,
                        Pool::Sqlite(_) => Err(StoreError::Unsupported(SUCCESSION_UNSUPPORTED.into())),
                    })
                })
            }
            // No writer lock (as a declaration): it may wait, bounded, behind a staged part.
            Request::RetireOffline(ordinal, r) => {
                let lock_timeout_ms = cuts.lock_timeout_ms.load(Ordering::SeqCst);
                let lease_ms = cuts.host_lease_ms.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    r.send(match &pool {
                        Pool::Pg(p) => crate::operator::retire_offline(p, ordinal, lock_timeout_ms, lease_ms).await,
                        Pool::Sqlite(_) => Err(StoreError::Unsupported(SUCCESSION_UNSUPPORTED.into())),
                    })
                })
            }
            Request::VouchHost(ordinal, r) => {
                let lease_ms = cuts.host_lease_ms.load(Ordering::SeqCst);
                let lock_timeout_ms = cuts.lock_timeout_ms.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    r.send(match &pool {
                        Pool::Pg(p) => crate::operator::vouch_host(p, ordinal, lease_ms, lock_timeout_ms).await,
                        Pool::Sqlite(_) => Err(StoreError::Unsupported(SUCCESSION_UNSUPPORTED.into())),
                    })
                })
            }
            // Postgres: no writer lock (as the heartbeat and the vouch: one statement over one
            // `hosts` row, bounded by the cut lock timeout). SQLite: the writer lock, as every write.
            Request::DrainHost(id, in_ms, r) => {
                let lock_ms = cuts.lock_timeout_ms.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    let _w = match &pool {
                        Pool::Sqlite(_) => Some(writer.lock().await),
                        Pool::Pg(_) => None,
                    };
                    r.send(on_pool!(&pool, drain_host(id, in_ms, lock_ms)))
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
            Request::LastCommittedCut(r) => tokio::spawn(async move { r.send(on_pool!(&pool, last_committed_cut())) }),
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
            Request::ClaimSuperRegions(owner, srs, live, r) => {
                let lock_ms = cuts.claim_lock_timeout_ms.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    let _w = writer.lock().await;
                    r.send(on_pool!(&pool, claim_super_regions(owner, srs, s, live, lock_ms)))
                })
            }
            // Postgres: no writer lock (as the heartbeat) — a claim may wait up to its lock
            // timeout for another claimer's row, and this host's own cut part must not queue
            // behind it in-process (it could be the very part that other claimer's cut waits
            // for: a cycle Postgres cannot see). The row locks serialise it.
            Request::ClaimFree(owner, boot, srs, r) => {
                let lock_ms = cuts.claim_lock_timeout_ms.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    let _w = match &pool {
                        Pool::Sqlite(_) => Some(writer.lock().await),
                        Pool::Pg(_) => None,
                    };
                    r.send(on_pool!(&pool, claim_free(owner, boot, srs, s, lock_ms)))
                })
            }
            Request::Release(fence, r) => {
                let lock_ms = cuts.claim_lock_timeout_ms.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    let _w = writer.lock().await;
                    r.send(on_pool!(&pool, release(fence, lock_ms)))
                })
            }
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
            Request::RegisterHost(host, policy, r) => {
                let lease_ms = cuts.host_lease_ms.load(Ordering::SeqCst);
                let lock_ms = cuts.claim_lock_timeout_ms.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    let _w = writer.lock().await;
                    r.send(on_pool!(&pool, register_host(host, policy, lease_ms, lock_ms)))
                })
            }
            Request::Hosts(r) => tokio::spawn(async move { r.send(on_pool!(&pool, hosts())) }),
            Request::WorldGeneration(r) => tokio::spawn(async move { r.send(on_pool!(&pool, world_generation())) }),
            // Postgres: no writer lock (it is in-process and a claim can hold it for a minute; a
            // late beat would lapse the lease) — the statement writes only the caller's own row.
            // SQLite has one writer at a time, so it takes the lock like every write.
            // Postgres: neither a connection of the shared pool (a burst of region loads, a
            // long part and a claim waiting on a lock can hold all of them for longer than
            // the lease) — the beat task's own connection, under its deadline.
            Request::Heartbeat(ordinal, boot, r) => match (&pool, &beat_tx) {
                (Pool::Pg(_), Some(beat_tx)) => {
                    if let Err(mpsc::error::SendError((_, _, r))) = beat_tx.send((ordinal, boot, r)) {
                        r.send(Err(StoreError::Backend("the heartbeat task has stopped".into())));
                    }
                    continue;
                }
                _ => tokio::spawn(async move {
                    let _w = match &pool {
                        Pool::Sqlite(_) => Some(writer.lock().await),
                        Pool::Pg(_) => None,
                    };
                    r.send(on_pool!(&pool, heartbeat(ordinal, boot)))
                }),
            },
            Request::TestHook(hook, r) => match hook {
                TestHook::FencedTxIdleMs(ms) => {
                    crate::set_fenced_tx_idle_ms_here(ms);
                    r.send(Ok(()));
                    continue;
                }
                TestHook::FreezeInFencedTx(fence, hold_ms) => tokio::spawn(async move {
                    match &pool {
                        Pool::Pg(p) => match rows::pg::fenced_tx_held(p, &fence).await {
                            Ok(tx) => {
                                r.send(Ok(()));
                                // Frozen: nothing more is sent on this transaction.
                                tokio::time::sleep(std::time::Duration::from_millis(hold_ms)).await;
                                drop(tx);
                            }
                            Err(e) => r.send(Err(e)),
                        },
                        Pool::Sqlite(_) => r.send(Err(StoreError::Unsupported("a frozen fenced transaction needs Postgres".into()))),
                    }
                }),
                TestHook::HoldConnections(n, hold_ms) => tokio::spawn(async move {
                    match &pool {
                        Pool::Pg(p) => {
                            let mut held = Vec::new();
                            for _ in 0..n {
                                match p.acquire().await {
                                    Ok(c) => held.push(c),
                                    Err(e) => return r.send(Err(e.into())),
                                }
                            }
                            r.send(Ok(()));
                            tokio::time::sleep(std::time::Duration::from_millis(hold_ms)).await;
                            drop(held);
                        }
                        Pool::Sqlite(_) => r.send(Err(StoreError::Unsupported("held pool connections need Postgres".into()))),
                    }
                }),
            },
            Request::Owners(r) => tokio::spawn(async move { r.send(on_pool!(&pool, owners())) }),
            Request::OwnedBy(owner, r) => tokio::spawn(async move { r.send(on_pool!(&pool, owned_by(owner))) }),
            Request::DbNowMs(r) => tokio::spawn(async move { r.send(on_pool!(&pool, db_now_ms())) }),
        };
        tasks.push(task);
    }
    // Drain: every commit queued so far, then the requests still running.
    drop(commit_tx);
    let _ = committer.await;
    drop(beat_tx);
    if let Some(beater) = beater {
        let _ = beater.await;
    }
    for t in tasks {
        let _ = t.await;
    }
    match &pool {
        Pool::Sqlite(p) => p.close().await,
        Pool::Pg(p) => p.close().await,
    }
    drop(guard);
}

/// A lease beat for the beat task: `(ordinal, boot, answer)`.
type BeatJob = (HostId, u64, Reply<crate::Beat>);

/// `application_name` of a store's heartbeat connection.
pub(crate) const HEARTBEAT_APP: &str = "mmo-heartbeat";

/// The lease beats of a Postgres store (`Request::Heartbeat`), in order, on ONE connection
/// of their own — never the shared pool's, which a burst of region loads, a long cut part
/// and a claim waiting on a row lock can hold whole for longer than the lease. Each beat —
/// connecting included — has a client-side deadline ([`crate::HEARTBEAT_DEADLINE_MS`]): one
/// that fails or runs out answers an error ("no beat this time", never `live: false`) and its
/// connection is DROPPED, so the next beat dials a new one instead of waiting on a socket
/// that retransmits into a black hole. A short transaction whose `lock_timeout` is
/// [`crate::HEARTBEAT_LOCK_TIMEOUT_MS`] (`SET LOCAL`: nothing outlives the transaction, also
/// behind a connection pooler): a beat behind a registration or a declaration of its own row
/// fails fast. One statement over the caller's own `hosts` row, UNFENCED on purpose (as
/// `rows`' `heartbeat`, which SQLite still uses).
async fn beats(options: sqlx::postgres::PgConnectOptions, mut rx: mpsc::UnboundedReceiver<BeatJob>) {
    use sqlx::{ConnectOptions, Connection, Row};
    let deadline = std::time::Duration::from_millis(crate::HEARTBEAT_DEADLINE_MS);
    let sql = crate::rows::heartbeat_sql(crate::succession::PG_NOW_MS);
    let mut conn: Option<sqlx::PgConnection> = None;
    while let Some((ordinal, boot, reply)) = rx.recv().await {
        let beat = async {
            if conn.is_none() {
                conn = Some(options.connect().await?);
            }
            let mut tx = conn.as_mut().expect("connected above").begin().await?;
            sqlx::query(&format!("SET LOCAL lock_timeout = '{}ms'", crate::HEARTBEAT_LOCK_TIMEOUT_MS)).execute(&mut *tx).await?;
            let row = sqlx::query(&sql).bind(i(ordinal.0)).bind(crate::rows::i(boot)).fetch_optional(&mut *tx).await?;
            tx.commit().await?;
            Ok::<crate::Beat, sqlx::Error>(crate::rows::beat_of(row.map(|r| r.try_get::<Option<i64>, _>(0)).transpose()?))
        };
        let answer = match tokio::time::timeout(deadline, beat).await {
            Ok(Ok(beaten)) => Ok(beaten),
            Ok(Err(e)) => {
                // Whatever failed (a lock timeout leaves an aborted transaction behind): a
                // new connection for the next beat.
                conn = None;
                Err(e.into())
            }
            Err(_) => {
                conn = None;
                Err(StoreError::Backend(format!(
                    "a heartbeat got no answer within {} ms (HEARTBEAT_DEADLINE_MS); its connection is dropped, the next \
                     beat dials a new one",
                    crate::HEARTBEAT_DEADLINE_MS
                )))
            }
        };
        reply.send(answer);
    }
    if let Some(conn) = conn {
        let _ = conn.close().await;
    }
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
        Request::ClaimSuperRegions(_, _, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::ClaimFree(_, _, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::Release(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::ClaimCoordinator(r) => r.send(Err(StoreError::ReadOnly)),
        Request::SaveCoordinator(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::EnsureClockAnchor(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::ClearClockAnchor(r) => r.send(Err(StoreError::ReadOnly)),
        Request::SetSuperRegionRegions(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::RegisterHost(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::PrepareCut(c, _, _, r) => {
            cuts.settle(c.id);
            r.send(Err(StoreError::ReadOnly))
        }
        Request::DecideCut(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::AbortCut(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::RecordCutBegin(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        Request::ResolvePrepared(r) => r.send(Err(StoreError::ReadOnly)),
        Request::CutDecided(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::DeclareSuccession(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::RollbackWorld(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::RollbackWorldOperator(r) => r.send(Err(StoreError::ReadOnly)),
        Request::RetireOffline(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::VouchHost(_, r) => r.send(Err(StoreError::ReadOnly)),
        Request::DrainHost(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        // A read-only handle sends nothing, a notice included.
        Request::NotifyCut(_, _, r) => r.send(Err(StoreError::ReadOnly)),
        req => return Some(req),
    }
    None
}
