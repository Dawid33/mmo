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
//! 3. The outbox (Elastic 2b Task 5): the messages for hosts linked to another replica inserted
//!    (`INSERT INTO outbox`, at the version it commits as); the rows of the hosts whose `Attach`
//!    it applied deleted unsent; this replica's own hosts' rows taken (`DELETE … RETURNING`, in
//!    `seq` order — commit order, as every row is inserted under the row's lock).
//! 4. `pg_notify('coordinator_row', '<version>')`: the other replicas' caches refresh on it.
//! 5. The commit. Its error is the write's (whether it committed is then not known: the core
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
///
/// The outbox (Elastic 2b Task 5): `outbox` = the messages the transition decided for hosts
/// linked to ANOTHER replica, `(host, its boot, game::encode_packet of the message)`, inserted
/// at the version it commits as; `discard_for` = the hosts whose `Attach` it applied: their rows
/// (decided before that `Attach`, which reconciles them) are deleted unsent; `take_for` = this
/// replica's own hosts with their links' boots: their rows are taken (deleted and returned, in
/// commit order — a row of another boot is deleted unsent). Postgres only (SQLite runs one
/// replica: a transition with an outbox message is `Unsupported` there).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransitionWrite {
    pub version: u64,
    pub core: Vec<u8>,
    pub history: Option<Vec<u8>>,
    pub decisions: Vec<StoreDecision>,
    pub replica: ReplicaId,
    pub outbox: Vec<(HostId, u64, Vec<u8>)>,
    pub take_for: Vec<(HostId, u64)>,
    pub discard_for: Vec<HostId>,
}

/// A transition committed: the row's new version, the database clock at the write (ms since
/// the epoch), each decision's answer (stamped `replica` at `version`, in order), the
/// post-commit effects the runtime is to run, and the outbox rows it took for this replica's
/// hosts (in `seq` order: commit order) — sent BEFORE the transition's own messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitionWritten {
    pub version: u64,
    pub store_ms: u64,
    pub answers: Vec<DecisionAnswer>,
    pub after_commit: Vec<AfterCommit>,
    pub taken: Vec<OutboxRow>,
}

/// One `outbox` row (Elastic 2b Task 5): a message a transition committed at `version` decided
/// for host `host` at its boot `boot`, linked to another replica than the writer — `msg` is
/// `game::encode_packet` of the [`game::CoordMsg`]. `seq` orders the rows by commit (they are
/// inserted only inside a transition, under the `coordinator` row's lock).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxRow {
    pub seq: u64,
    pub version: u64,
    pub host: HostId,
    pub boot: u64,
    pub msg: Vec<u8>,
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

/// The transition `w` (Postgres): the compare-and-set, each decision in a savepoint, then the
/// outbox — its messages inserted, the rows of the hosts it attached discarded, its own hosts'
/// rows taken — and the notice of the new version (see the module docs). `injected[i]`:
/// decision `i` fails without running (a test hook). Returns the new version, the database
/// clock, each decision's outcome and the rows taken.
pub(crate) async fn write_pg(
    pool: &PgPool,
    w: &TransitionWrite,
    dials: DecisionDials,
    injected_failures: &[bool],
) -> Result<(u64, u64, Vec<Decided>, Vec<OutboxRow>), StoreError> {
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
    // The outbox (Elastic 2b Task 5; lock walk: `succession.rs`'s notes). Its rows are locked
    // only by coordinator statements, after the `coordinator` row: no edge.
    for (host, boot, msg) in &w.outbox {
        sqlx::query("INSERT INTO outbox (version, host, boot, msg) VALUES ($1, $2, $3, $4)")
            .bind(version)
            .bind(crate::rows::i(host.0))
            .bind(crate::rows::i(*boot))
            .bind(msg)
            .execute(&mut *tx)
            .await?;
    }
    if !w.discard_for.is_empty() {
        // Before the take: the rows of a host whose `Attach` this transition applied were decided
        // before it (any later one would have met this transition's compare-and-set): its
        // `Attach` reconciles what they carried — discarded, never delivered.
        let hosts: Vec<i64> = w.discard_for.iter().map(|h| crate::rows::i(h.0)).collect();
        let gone = sqlx::query("DELETE FROM outbox WHERE host = ANY($1)").bind(&hosts).execute(&mut *tx).await?.rows_affected();
        if gone > 0 {
            log::debug!("[world-store] {gone} outbox row(s) older than their hosts' Attach discarded");
        }
    }
    // Under the compare-and-set this transaction IS the row's newest version: every row is at or
    // below it.
    let taken = take_in(&mut tx, &w.take_for, version as u64).await?;
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(crate::listen::COORDINATOR_ROW_CHANNEL)
        .bind(version.to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((version as u64, store_ms as u64, out, taken))
}

/// The outbox rows of `for_hosts` (each host with its link's boot) at or below row version
/// `upto`, taken on `conn` — one `DELETE … RETURNING` of the rows only (no other lock): returned
/// in `seq` order (commit order); a row whose boot is not its link's is deleted unsent (that boot
/// is gone). `upto` is the version of the cache that decided `for_hosts` are attached to the
/// taker (final review S-I2): a row newer than that cache may have been decided after its host
/// attached to another replica, whose relay it is — it stays in the table.
pub(crate) async fn take_in(
    conn: &mut sqlx::PgConnection,
    for_hosts: &[(HostId, u64)],
    upto: u64,
) -> Result<Vec<OutboxRow>, StoreError> {
    if for_hosts.is_empty() {
        return Ok(Vec::new());
    }
    let hosts: Vec<i64> = for_hosts.iter().map(|(h, _)| crate::rows::i(h.0)).collect();
    let rows = sqlx::query("DELETE FROM outbox WHERE host = ANY($1) AND version <= $2 RETURNING seq, version, host, boot, msg")
        .bind(&hosts)
        .bind(crate::rows::i(upto))
        .fetch_all(&mut *conn)
        .await?;
    let mut taken = Vec::with_capacity(rows.len());
    let mut stale = 0;
    for r in rows {
        let row = OutboxRow {
            seq: r.try_get::<i64, _>(0)? as u64,
            version: r.try_get::<i64, _>(1)? as u64,
            host: HostId(r.try_get::<i64, _>(2)? as u64),
            boot: r.try_get::<i64, _>(3)? as u64,
            msg: r.try_get(4)?,
        };
        if for_hosts.iter().any(|(h, b)| *h == row.host && *b == row.boot) {
            taken.push(row);
        } else {
            stale += 1;
        }
    }
    if stale > 0 {
        log::debug!("[world-store] {stale} outbox row(s) of another boot than their host's link deleted unsent");
    }
    taken.sort_by_key(|r| r.seq);
    Ok(taken)
}

/// [`take_in`] on its own (between transitions: a replica relaying what other replicas left for
/// its hosts).
pub(crate) async fn take_outbox_pg(pool: &PgPool, for_hosts: &[(HostId, u64)], upto: u64) -> Result<Vec<OutboxRow>, StoreError> {
    let mut conn = pool.acquire().await?;
    take_in(&mut conn, for_hosts, upto).await
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
