//! The coordinator's transition (Elastic 2b Task 3): ONE transaction holding the
//! compare-and-set of its row and every store decision the transition carries — deciding a
//! world cut, recording a cut's begin, declaring a succession, ending a landless host, rolling
//! the world back. The core and its decisions commit or fail together: the store never holds a
//! decision no committed core knows, and a replica whose cache is stale issues nothing (its
//! compare-and-set fails first — issuing IS committing). The row's `version` is the
//! coordinator's only fence; its `epoch` column is read by nothing but the bootstrap.
//!
//! The transaction (lock walk: `succession.rs` module notes):
//! 1. `UPDATE coordinator … WHERE id = 1 AND version = $v RETURNING version, <now>` — the one
//!    row locked `FOR NO KEY UPDATE`; zero rows: rolled back, [`StoreError::Conflict`], nothing
//!    below ran.
//! 2. With decisions, `SET LOCAL lock_timeout` (the cut lock timeout), then each decision in
//!    its own savepoint, in the transition's order, its body WITHOUT a `coordinator` lock of its
//!    own (the row is held already, in a stronger mode). A refusal of the domain (`NotLapsed`,
//!    `Gone`, a rollback from a generation the world is past, …) or an error inside (a SQL
//!    error, `40P01`, a lock timeout, a stale generation) rolls back to its savepoint and is
//!    that decision's answer; the core still commits.
//! 3. The commit. Its error is the write's (whether it committed is then not known: the core
//!    looks its decisions up, never issues them again).
//!
//! After the commit the runtime runs the post-commit effects ([`AfterCommit`]): the staged
//! parts of a decided cut finished (`COMMIT PREPARED` / `ROLLBACK PREPARED`) and then — by the
//! same effect, never beside it — a committed cut announced to the terrain listeners. A replica that dies first leaves them to the existing
//! lookups (a host's `cut_decided`, a start's `resolve_prepared`, the listeners' poll).

use game::coordinator::{CallStamp, DecisionAnswer, ReplicaId, StoreDecision};
use game::{CutId, Declared, HostId, LandlessEnd, Tick};
use sqlx::{PgPool, Row};

use crate::StoreError;
use crate::succession::{self, DECLARING, ENDING, ROLLING_BACK, PG_NOW_MS};
use crate::two_phase::{self, TWO_PHASE_UNSUPPORTED};

/// One transition of the coordinator to write ([`crate::StoreHandle::write_transition_later`]):
/// its projection — `core`, and `history` unless `None` (the stored one kept) — over the row
/// as read at `version`, and the store decisions it issues, run in the same transaction.
/// `replica`: the replica writing, which the answers' stamps name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitionWrite {
    pub version: u64,
    pub core: Vec<u8>,
    pub history: Option<Vec<u8>>,
    pub decisions: Vec<StoreDecision>,
    pub replica: ReplicaId,
}

/// A transition committed: the row's new version, the database clock at the write (ms since
/// the epoch), each decision's answer (stamped `replica` at `version`, in order), and the
/// post-commit effects the runtime is to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitionWritten {
    pub version: u64,
    pub store_ms: u64,
    pub answers: Vec<DecisionAnswer>,
    pub after_commit: Vec<AfterCommit>,
}

/// What a committed transition leaves to do after its commit
/// ([`crate::StoreHandle::after_commit_later`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterCommit {
    /// Every staged part of `cut` finished as `committed` says; committed, the terrain
    /// listeners are then told (`pg_notify`: only once the parts' rows are visible) and the
    /// checks for entities stored twice and settles not adopted run.
    FinishCut { cut: CutId, tick: Tick, committed: bool },
    /// The terrain listeners told a cut committed (`pg_notify`) on its own. A committed
    /// transition never queues it (its `FinishCut` announces); kept for a caller that knows the
    /// parts are finished.
    NotifyCut { cut: CutId, tick: Tick },
}

/// What [`crate::StoreHandle::bootstrap_coordinator`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bootstrapped {
    /// This call wrote the row: at `version`, its cut ids' high half `epoch`.
    Written { version: u64, epoch: u64 },
    /// A row with a projection is there (another replica, or an earlier start): load it.
    Exists,
}

/// One decision's outcome, typed (the test helpers of the handle keep the [`StoreError`]; the
/// answers carry its text).
#[derive(Debug)]
pub(crate) enum Decided {
    Cut { cut: CutId, tick: Tick, committed: Result<bool, StoreError> },
    Begin { cut: CutId, bound: Result<HostId, StoreError> },
    Declared(Result<Declared, StoreError>),
    Ended { end: game::coordinator::EndLandless, outcome: Result<LandlessEnd, StoreError> },
    RolledBack(Result<u64, StoreError>),
    ReplicaEnded { replica: ReplicaId, #[allow(dead_code)] outcome: Result<(), StoreError> },
}

/// The dials a decision runs with (the store thread's).
#[derive(Clone, Copy, Debug)]
pub(crate) struct DecisionDials {
    /// The database's OID (the gids of its prepared cut parts).
    pub world_tag: u32,
    /// `lock_timeout` of the decisions, ms (the cut lock timeout).
    pub lock_timeout_ms: u64,
    /// The host lease a lapse is judged by, ms.
    pub lease_ms: u64,
}

fn injected() -> StoreError {
    StoreError::Backend("injected decision failure (StoreHandle::fail_next_decisions)".into())
}

/// The transition `w` (Postgres): the compare-and-set, then each decision in a savepoint (see
/// the module docs). `injected[i]`: decision `i` fails without running (a test hook). Returns
/// the new version, the database clock and each decision's outcome.
pub(crate) async fn write_pg(
    pool: &PgPool,
    w: &TransitionWrite,
    dials: DecisionDials,
    injected_failures: &[bool],
) -> Result<(u64, u64, Vec<Decided>), StoreError> {
    let mut tx = pool.begin().await?;
    let sql = format!(
        "UPDATE coordinator SET core = $1, history = COALESCE($2, history), version = version + 1, saved_utc = $3, \
         history_version = CASE WHEN $2::bytea IS NULL THEN history_version ELSE version + 1 END \
         WHERE id = 1 AND version = $4 RETURNING version, {PG_NOW_MS}"
    );
    let row = sqlx::query(&sql)
        .bind(&w.core)
        .bind(&w.history)
        .bind(crate::open::utc_now())
        .bind(crate::rows::i(w.version))
        .fetch_optional(&mut *tx)
        .await?;
    let Some(row) = row else {
        tx.rollback().await?;
        return Err(StoreError::Conflict);
    };
    let (version, store_ms): (i64, i64) = (row.try_get(0)?, row.try_get(1)?);
    let mut out = Vec::with_capacity(w.decisions.len());
    if !w.decisions.is_empty() {
        sqlx::query(&format!("SET LOCAL lock_timeout = '{}ms'", dials.lock_timeout_ms)).execute(&mut *tx).await?;
    }
    for (n, d) in w.decisions.iter().enumerate() {
        if injected_failures.get(n).copied().unwrap_or(false) {
            out.push(refused(d, injected()));
            continue;
        }
        sqlx::query(&format!("SAVEPOINT decision_{n}")).execute(&mut *tx).await?;
        let (decided, keep) = run_one(&mut tx, d, dials).await;
        let end = if keep { format!("RELEASE SAVEPOINT decision_{n}") } else { format!("ROLLBACK TO SAVEPOINT decision_{n}") };
        sqlx::query(&end).execute(&mut *tx).await?;
        out.push(decided);
    }
    tx.commit().await?;
    Ok((version as u64, store_ms as u64, out))
}

/// Decision `d` refused with `e` before it ran.
fn refused(d: &StoreDecision, e: StoreError) -> Decided {
    match d {
        StoreDecision::DecideCut(c) => Decided::Cut { cut: c.cut, tick: c.tick, committed: Err(e) },
        StoreDecision::BeginCut { cut } => Decided::Begin { cut: *cut, bound: Err(e) },
        StoreDecision::Declare(_) => Decided::Declared(Err(e)),
        StoreDecision::EndLandless(end) => Decided::Ended { end: *end, outcome: Err(e) },
        StoreDecision::Rollback { .. } => Decided::RolledBack(Err(e)),
        StoreDecision::EndReplica(replica) => Decided::ReplicaEnded { replica: *replica, outcome: Err(e) },
    }
}

/// One decision's body inside the transition's transaction (its savepoint open): its outcome,
/// and whether what it wrote stays (`false`: rolled back to the savepoint).
async fn run_one(tx: &mut sqlx::PgConnection, d: &StoreDecision, dials: DecisionDials) -> (Decided, bool) {
    match d {
        StoreDecision::DecideCut(c) => {
            let committed = two_phase::decide_in(
                &mut *tx,
                dials.world_tag,
                c.cut,
                c.tick,
                c.commit,
                &c.hosts,
                &c.leaving,
                c.generation,
                &c.settles,
            )
            .await;
            let keep = committed.is_ok();
            (Decided::Cut { cut: c.cut, tick: c.tick, committed }, keep)
        }
        StoreDecision::BeginCut { cut } => {
            let bound = two_phase::begin_in(&mut *tx, *cut).await;
            let keep = bound.is_ok();
            (Decided::Begin { cut: *cut, bound }, keep)
        }
        StoreDecision::Declare(dc) => {
            let declared = match succession::check_declare(&dc.dead, &dc.heirs) {
                Err(e) => Err(e),
                Ok(predecessors) => {
                    succession::declare_in(&mut *tx, &dc.dead, &dc.heirs, &predecessors, dc.kind, dc.generation, dials.lease_ms)
                        .await
                        .map_err(|e| e.into_store(DECLARING, dials.lock_timeout_ms))
                }
            };
            let keep = matches!(declared, Ok(Declared::Done(_)));
            (Decided::Declared(declared), keep)
        }
        StoreDecision::EndLandless(end) => {
            let outcome = succession::end_landless_in(&mut *tx, end.host, end.boot, dials.lease_ms)
                .await
                .map_err(|e| e.into_store(ENDING, dials.lock_timeout_ms));
            let keep = outcome == Ok(LandlessEnd::Ended);
            if keep {
                log::warn!(
                    "[world-store] host {} (boot {:#x}) lost its lease holding no land: its row is dead (with the \
                     coordinator's transition), no succession, the generation untouched",
                    end.host,
                    end.boot
                );
            }
            (Decided::Ended { end: *end, outcome }, keep)
        }
        StoreDecision::Rollback { from } => {
            let rolled = succession::rollback_world_in(&mut *tx, *from).await.map_err(|e| e.into_store(ROLLING_BACK, dials.lock_timeout_ms));
            let keep = matches!(rolled, Ok((_, Some(_))));
            (Decided::RolledBack(rolled.map(|(g, _)| g)), keep)
        }
        StoreDecision::EndReplica(replica) => {
            let ended = crate::replicas::end_replica_in(&mut *tx, *replica).await;
            match &ended {
                Ok(true) => log::warn!("[world-store] coordinator replica {replica} lapsed: its row is ended (with the transition)"),
                Ok(false) => log::info!("[world-store] coordinator replica {replica}: no live row to end (ended already)"),
                Err(e) => log::warn!("[world-store] coordinator replica {replica}: its row could not be ended: {e}"),
            }
            let keep = ended.is_ok();
            (Decided::ReplicaEnded { replica: *replica, outcome: ended.map(|_| ()) }, keep)
        }
    }
}

/// The answers of a committed transition `w` (at `version`) and its post-commit effects.
pub(crate) fn answers(w: &TransitionWrite, version: u64, decided: Vec<Decided>) -> (Vec<DecisionAnswer>, Vec<AfterCommit>) {
    let stamp = CallStamp { replica: w.replica, version };
    let text = |e: StoreError| e.to_string();
    let mut answers = Vec::with_capacity(decided.len());
    let mut after = Vec::new();
    for d in decided {
        answers.push(match d {
            Decided::Cut { cut, tick, committed } => {
                if let Ok(committed) = committed {
                    // One effect: a committed cut is announced by its finish, after its parts
                    // are committed (never beside it: a listener would read the old versions).
                    after.push(AfterCommit::FinishCut { cut, tick, committed });
                }
                DecisionAnswer::Cut { stamp, cut, committed: committed.map_err(text) }
            }
            Decided::Begin { cut, bound } => DecisionAnswer::Begin { cut, bound: bound.map_err(text) },
            Decided::Declared(outcome) => DecisionAnswer::Declared { stamp, outcome: outcome.map_err(text) },
            Decided::Ended { end, outcome } => DecisionAnswer::Ended { stamp, end, outcome: outcome.map_err(text) },
            Decided::RolledBack(outcome) => DecisionAnswer::RolledBack { stamp, outcome: outcome.map_err(text) },
            Decided::ReplicaEnded { replica, .. } => DecisionAnswer::ReplicaEnded { replica },
        });
    }
    (answers, after)
}

/// The post-commit effect `a` (Postgres): a cut's staged parts finished (`stop_after`: the
/// test hook, fail after that many `COMMIT PREPARED`s), or the listeners told. Whether a
/// committed cut was finished (the caller then runs its checks).
pub(crate) async fn after_commit_pg(pool: &PgPool, world_tag: Option<u32>, a: AfterCommit, stop_after: Option<u32>) -> Result<bool, StoreError> {
    match a {
        AfterCommit::FinishCut { cut, tick, committed } => {
            let tag = world_tag.ok_or_else(|| StoreError::Unsupported(TWO_PHASE_UNSUPPORTED.into()))?;
            two_phase::finish_staged(pool, tag, cut, committed, stop_after).await?;
            if committed {
                // Only now are its parts' rows visible: the terrain listeners re-read on it. A
                // failure fails the effect, which is tried again (the finish is idempotent).
                crate::rows::notify_cut(pool, cut, tick).await?;
            }
            Ok(committed)
        }
        AfterCommit::NotifyCut { cut, tick } => {
            crate::rows::notify_cut(pool, cut, tick).await?;
            Ok(false)
        }
    }
}
