//! Two-phase save cuts (R3, Postgres only). Each host writes its fenced part of a
//! cut and ends the transaction with `PREPARE TRANSACTION '<gid>'`
//! ([`crate::rows::CutEnd::Prepare`]); the coordinator then records ONE decision
//! row (`cut_decisions`, first writer wins on the cut id) inside the transaction of its
//! transition ([`decide_in`]), and after that commit finishes ([`finish_staged`]:
//! `COMMIT PREPARED` / `ROLLBACK PREPARED`) every part of the cut that is staged.
//! A commit is recorded only if exactly the listed hosts' parts are staged, every
//! host whose `hosts` row is not clean is listed — but one registered after the cut's begin
//! was recorded ([`begin_in`], Elastic 1 Task 4) — and it settles exactly the open
//! successions (R4, `crate::succession`); otherwise
//! the decision becomes an abort. A host that gives up records an abort the same way
//! ([`abort_cut`]); a part that prepares after its cut was decided finishes itself
//! ([`finish_if_decided`]); a process starting up finishes or aborts whatever a crash
//! left prepared ([`resolve_prepared`]). Every finisher finishes EVERY staged part of
//! the cut, as the stored decision says.
//!
//! Locks: a prepared part keeps every row lock it took — `FOR SHARE` on its host's
//! `super_regions` rows (so a claim of them waits for the decision) and the rows it
//! wrote — but none on `world` (it reads the generation without a lock and writes no
//! world tick). The decision runs in the coordinator's transition (Elastic 2b Task 3,
//! `coordinator_tx.rs`), after its compare-and-set holds the `coordinator` row: then `world`
//! (`FOR NO KEY UPDATE`, a commit only — it never waits for a claim's `FOR KEY SHARE`),
//! `cut_decisions`, `hosts` and (settling) `successions`: no row a prepared part holds
//! (an heir's part reads `successions` without a lock), so a decision never waits
//! for a part, nor for a claim waiting for one.
//!
//! None of this takes the store's writer lock: a prepare queued behind a part that
//! holds a row it needs waits for the decision, which must not wait for that prepare.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use game::HostId;
use game::{CutId, Tick};
use sqlx::PgPool;

use crate::rows::i;
use crate::rows::pg::cut_decision;
use crate::{StoreError, SuccessionId};

/// What every store that cannot prepare a cut answers.
pub(crate) const TWO_PHASE_UNSUPPORTED: &str = "several hosts need max_prepared_transactions > 0 (Postgres)";

/// The prefix of every prepared cut part's gid.
const GID_PREFIX: &str = "mmo-cut-";
/// Postgres' longest gid, in bytes.
const GID_MAX_BYTES: usize = 200;
/// How long a `COMMIT PREPARED` / `ROLLBACK PREPARED` of a part that another process is
/// finishing at that moment ("is busy") waits before it looks again.
const PREPARED_BUSY_RETRY: Duration = Duration::from_millis(20);
/// How many times it looks before giving up (10 s in all).
const PREPARED_BUSY_TRIES: u32 = 500;
/// The tick an abort recorded by the sweep carries: it does not know the cut's (also a host's
/// start-up resolve of a crash's leftovers, `server::roles::resolve_before_claim`).
pub const SWEEP_ABORT_TICK: Tick = 0;

/// The gid of host `host`'s prepared part of `cut`. `world_tag` tells the worlds of
/// one Postgres cluster apart (gids are cluster-wide): the database's OID.
pub fn cut_gid(world_tag: u32, cut: CutId, host: HostId) -> String {
    format!("{GID_PREFIX}{world_tag}-{}-{}", cut.0, host.0)
}

/// Whether `gid` has the shape [`cut_gid`] gives: it is formatted into SQL (a gid
/// cannot be a bind parameter), so nothing else may get there.
fn gid_ok(gid: &str) -> bool {
    gid.len() <= GID_MAX_BYTES
        && gid.strip_prefix(GID_PREFIX).is_some_and(|rest| {
            let parts: Vec<&str> = rest.split('-').collect();
            parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn checked(gid: &str) -> &str {
    assert!(gid_ok(gid), "not a prepared cut part's gid: {gid:?}");
    gid
}

/// `PREPARE TRANSACTION` of a part.
pub(crate) fn prepare_sql(gid: &str) -> String {
    format!("PREPARE TRANSACTION '{}'", checked(gid))
}

/// What a gid that starts with the prefix is.
#[derive(Debug, PartialEq, Eq)]
enum Gid {
    /// A part of this world: `(cut, host)`.
    Ours(CutId, HostId),
    /// A well-formed part of another world.
    Foreign,
    /// Not of [`cut_gid`]'s shape (or out of range).
    Malformed,
}

fn parse_gid(world_tag: u32, gid: &str) -> Gid {
    if !gid_ok(gid) {
        return Gid::Malformed;
    }
    let mut parts = gid[GID_PREFIX.len()..].split('-');
    let (Some(tag), Some(cut), Some(host)) = (parts.next(), parts.next(), parts.next()) else {
        return Gid::Malformed;
    };
    match (tag.parse::<u32>(), cut.parse::<u64>(), host.parse::<u64>()) {
        (Ok(t), Ok(c), Ok(h)) if t == world_tag => Gid::Ours(CutId(c), HostId(h)),
        (Ok(_), Ok(_), Ok(_)) => Gid::Foreign,
        _ => Gid::Malformed,
    }
}

/// Every staged part of this world: `cut → (host → gid)`. `cut`: only that cut's.
/// A gid that only looks like ours is skipped with a warning.
async fn staged_parts<'c>(
    e: impl sqlx::Executor<'c, Database = sqlx::Postgres>,
    world_tag: u32,
    cut: Option<CutId>,
) -> Result<BTreeMap<CutId, BTreeMap<HostId, String>>, StoreError> {
    let pattern = match cut {
        Some(c) => format!("{GID_PREFIX}{world_tag}-{}-%", c.0),
        None => format!("{GID_PREFIX}%"),
    };
    let gids: Vec<String> =
        sqlx::query_scalar("SELECT gid FROM pg_prepared_xacts WHERE database = current_database() AND gid LIKE $1")
            .bind(pattern)
            .fetch_all(e)
            .await?;
    let mut out: BTreeMap<CutId, BTreeMap<HostId, String>> = BTreeMap::new();
    for gid in gids {
        match parse_gid(world_tag, &gid) {
            Gid::Ours(c, h) if cut.is_none_or(|want| want == c) => {
                out.entry(c).or_default().insert(h, gid);
            }
            Gid::Ours(..) | Gid::Foreign => {}
            Gid::Malformed => log::warn!("[world-store] skipping prepared transaction {gid:?}: not a cut part's gid"),
        }
    }
    Ok(out)
}

/// Every staged part of this world as `(cut, host)`, in order — found by the prepared
/// transactions' names alone ([`parse_gid`]), whatever the host ids (Elastic 1: no host
/// count says which to look for). A read: nothing is finished.
pub(crate) async fn staged_cut_parts(pool: &PgPool, world_tag: u32) -> Result<Vec<(CutId, HostId)>, StoreError> {
    let parts = staged_parts(pool, world_tag, None).await?;
    Ok(parts.into_iter().flat_map(|(cut, hosts)| hosts.into_keys().map(move |h| (cut, h))).collect())
}

/// The hosts with a staged part of any cut of this world, read on `conn` (a registration's
/// transaction, `rows.rs` `register_in`, Elastic 4 Decision 6): its prune skips them. Two
/// statements of their own — the world tag, then `pg_prepared_xacts` — before the prune's
/// `DELETE`: the prepared transactions are not read under a statement's snapshot, so a part
/// still staged at this read is skipped, and one committed before it has its `in_flight` rows
/// visible to the later `DELETE`'s snapshot (the transaction is `READ COMMITTED`). No row lock.
pub(crate) async fn staged_hosts(conn: &mut sqlx::PgConnection) -> Result<Vec<HostId>, StoreError> {
    let oid: i64 = sqlx::query_scalar("SELECT oid::bigint FROM pg_database WHERE datname = current_database()")
        .fetch_one(&mut *conn)
        .await?;
    let tag = u32::try_from(oid).map_err(|_| StoreError::Corrupt(format!("database oid {oid}")))?;
    let parts = staged_parts(&mut *conn, tag, None).await?;
    let hosts: BTreeSet<HostId> = parts.into_values().flat_map(|hosts| hosts.into_keys()).collect();
    Ok(hosts.into_iter().collect())
}

/// This database's OID: the world tag of its gids.
pub(crate) async fn world_tag(pool: &PgPool) -> Result<u32, StoreError> {
    let oid: i64 = sqlx::query_scalar("SELECT oid::bigint FROM pg_database WHERE datname = current_database()")
        .fetch_one(pool)
        .await?;
    u32::try_from(oid).map_err(|_| StoreError::Corrupt(format!("database oid {oid}")))
}

/// `Ok` if the server prepares transactions (`max_prepared_transactions > 0`).
pub(crate) async fn two_phase_ready(pool: &PgPool) -> Result<(), StoreError> {
    let max: i64 = sqlx::query_scalar("SELECT current_setting('max_prepared_transactions')::bigint")
        .fetch_one(pool)
        .await?;
    if max > 0 { Ok(()) } else { Err(StoreError::Unsupported(TWO_PHASE_UNSUPPORTED.into())) }
}

/// Records `committed` for `cut` unless it is decided; returns whether this call
/// inserted it.
async fn insert_decision<'c>(
    e: impl sqlx::Executor<'c, Database = sqlx::Postgres>,
    cut: CutId,
    tick: Tick,
    committed: bool,
) -> Result<bool, StoreError> {
    let done = sqlx::query(
        "INSERT INTO cut_decisions (cut_id, tick, committed, decided_utc) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (cut_id) DO NOTHING",
    )
    .bind(cut.0 as i64)
    .bind(tick as i64)
    .bind(i64::from(committed))
    .bind(crate::open::utc_now())
    .execute(e)
    .await?;
    Ok(done.rows_affected() == 1)
}

fn no_decision(cut: CutId) -> StoreError {
    StoreError::Corrupt(format!("cut {} has no decision after its insert", cut.0))
}

/// The coordinator's decision on `cut` (Elastic 2b Task 3: a store decision of a transition,
/// `game::coordinator::StoreDecision::DecideCut`), inside `tx` — the transaction of the
/// transition that decided it, its `coordinator` row already held by the compare-and-set
/// (`coordinator_tx.rs`): records `commit` unless the cut is decided already (a host's
/// [`abort_cut`] may have won), and returns the STORED decision. A commit is refused with
/// `StaleGeneration` (nothing recorded) unless the world is at `generation`; a commit that
/// cannot be one (a part missing or unlisted, a dirty host unlisted, other open successions)
/// is recorded as an abort. A commit this call records also writes the world tick, marks
/// `leaving` clean and settles `settles`. The staged parts are finished after the commit
/// ([`finish_staged`], `crate::AfterCommit::FinishCut`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn decide_in(
    tx: &mut sqlx::PgConnection,
    world_tag: u32,
    cut: CutId,
    tick: Tick,
    commit: bool,
    hosts: &[HostId],
    leaving: &[HostId],
    generation: u64,
    settles: &[SuccessionId],
) -> Result<bool, StoreError> {
    let decided = match cut_decision(&mut *tx, cut).await? {
        Some(d) => d,
        None => {
            let mut commit = commit;
            if commit {
                // `FOR NO KEY UPDATE`, not `FOR UPDATE`: it serialises with another
                // decision and with `register_host`'s generation bump (both take it), and
                // with `worldctl super-region` (`FOR UPDATE`), but NOT with a claim's
                // `FOR KEY SHARE` — a claim holding that while it waits for a prepared
                // part's `super_regions` locks would otherwise wait on this decision while
                // this decision waits on it: a cycle through a prepared transaction that
                // Postgres cannot detect.
                let stored: i64 = sqlx::query_scalar("SELECT generation FROM world WHERE id = 1 FOR NO KEY UPDATE")
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?;
                if stored as u64 != generation {
                    return Err(StoreError::StaleGeneration { stored: stored as u64, mine: generation });
                }
                // A commit needs exactly the listed parts staged: a part still queued,
                // mid-PREPARE or never prepared would otherwise be skipped (the cut
                // half-visible) and could commit later over newer data.
                let staged: BTreeSet<HostId> = staged_parts(&mut *tx, world_tag, Some(cut))
                    .await?
                    .remove(&cut)
                    .unwrap_or_default()
                    .into_keys()
                    .collect();
                let listed: BTreeSet<HostId> = hosts.iter().copied().collect();
                // Every live host whose row is not clean must be listed: one that registered
                // while the cut was pending (back from a clean leave, or new) runs beside
                // the cut, and an entity it acked from a listed host — or sent to one —
                // would be saved by no part, or by two. `register_host` takes the world
                // row's lock first, so it either registered before this read (→ abort; the
                // next cut lists it) or registers after the decision (and loads this cut).
                // A row marked dead (R4: declared, its land inherited) runs nothing and is
                // not expected — the coordinator's expected set reads the same predicate.
                let dirty: Vec<i64> = sqlx::query_scalar(&format!(
                    "SELECT id FROM hosts WHERE clean = 0 AND state = {} ORDER BY id",
                    crate::HOST_LIVE
                ))
                .fetch_all(&mut *tx)
                .await?;
                // Elastic 1 Task 4: a host registered AFTER the cut's begin was recorded
                // ([`record_cut_begin`]: its id is at or above the `world.next_host_id` read
                // then) owned nothing at the cut's tick and is not of it — every participant
                // freezes its session with a host outside the cut from its `BeginCut` until the
                // decision, and fails its part if it exchanged state with one not yet saved by a
                // cut (`game::world_manager::world_cut`, "Hosts outside the cut"). Such a host
                // may be unlisted; any other dirty live host must be listed. No begin recorded:
                // every one must (the rule before).
                let below: Option<i64> = sqlx::query_scalar("SELECT hosts_below FROM cut_begins WHERE cut_id = $1")
                    .bind(cut.0 as i64)
                    .fetch_optional(&mut *tx)
                    .await?;
                let joined_since = |h: i64| below.is_some_and(|b| h >= b);
                let unlisted: Vec<i64> = dirty
                    .into_iter()
                    .filter(|h| u64::try_from(*h).map_or(true, |h| !listed.contains(&HostId(h))))
                    .filter(|h| !joined_since(*h))
                    .collect();
                if !unlisted.is_empty() {
                    log::warn!(
                        "[world-store] cut {} cannot commit: a host registered while the cut was pending (hosts \
                         {unlisted:?} have a dirty row and no part in it); recording an abort, the next cut lists them",
                        cut.0
                    );
                    commit = false;
                } else if staged != listed {
                    let missing: Vec<HostId> = listed.difference(&staged).copied().collect();
                    let unexpected: Vec<HostId> = staged.difference(&listed).copied().collect();
                    log::warn!(
                        "[world-store] cut {} cannot commit: parts missing from hosts {missing:?}, unlisted parts \
                         from hosts {unexpected:?}; recording an abort",
                        cut.0
                    );
                    commit = false;
                }
                // R4: a commit settles exactly the open successions — read under the world
                // row's lock, which a declaration and a rollback take first. A commit that
                // names fewer would let the heir's adoption commit unsettled (its next boot
                // would adopt again); one that names more settles what is not open.
                if commit {
                    let open = crate::succession::open_ids(&mut *tx).await?;
                    let named: BTreeSet<u64> = settles.iter().map(|s| s.0).collect();
                    if open != named || named.len() != settles.len() {
                        log::warn!(
                            "[world-store] cut {} cannot commit: it settles successions {named:?} but {open:?} are \
                             open; recording an abort",
                            cut.0
                        );
                        commit = false;
                    }
                }
            }
            let inserted = insert_decision(&mut *tx, cut, tick, commit).await?;
            // Elastic 2a: a begin below the cut being decided is never read again (only this
            // cut's commit path reads `cut_begins`, for its own row; `resolve_prepared` reads
            // none): pruned. `cut_decisions` is kept. Lock walk: `succession.rs` module notes.
            sqlx::query("DELETE FROM cut_begins WHERE cut_id < $1").bind(cut.0 as i64).execute(&mut *tx).await?;
            // A host's abort may have won the insert meanwhile.
            let d = cut_decision(&mut *tx, cut).await?.ok_or_else(|| no_decision(cut))?;
            if inserted && d {
                sqlx::query("UPDATE world SET world_tick = $1 WHERE id = 1 AND world_tick < $1")
                    .bind(tick as i64)
                    .execute(&mut *tx)
                    .await?;
                let leaving: Vec<i64> = leaving.iter().map(|h| i(h.0)).collect();
                sqlx::query("UPDATE hosts SET clean = 1 WHERE id = ANY($1)")
                    .bind(leaving)
                    .execute(&mut *tx)
                    .await?;
                crate::succession::settle(&mut *tx, cut, settles).await?;
            }
            d
        }
    };
    Ok(decided)
}

/// The coordinator records that world cut `cut` began (Elastic 1 Task 4; Elastic 2b Task 3: a
/// store decision of the transition that starts the cut, inside its `tx`):
/// `world.next_host_id` as it reads now goes into `cut_begins` (first writer wins); every host
/// id at or above it is issued later — a host registered after this record — and [`decide_in`]
/// lets such a host be absent from the cut's decision. Returns the recorded bound.
///
/// Locks: after the coordinator row (held by the transition's compare-and-set), a PLAIN read
/// of the world row — no lock: a registration holding the world row commits its id either
/// before this read (below the bound: it must be listed) or after it (at or above: it
/// registered after the record) — and the insert of this cut's own `cut_begins` row, which no
/// other statement locks. No new edge in the lock order (`succession.rs` module notes).
pub(crate) async fn begin_in(tx: &mut sqlx::PgConnection, cut: CutId) -> Result<HostId, StoreError> {
    let next: i64 = sqlx::query_scalar("SELECT next_host_id FROM world WHERE id = 1")
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?;
    sqlx::query("INSERT INTO cut_begins (cut_id, hosts_below, begun_utc) VALUES ($1, $2, $3) ON CONFLICT (cut_id) DO NOTHING")
        .bind(cut.0 as i64)
        .bind(next)
        .bind(crate::open::utc_now())
        .execute(&mut *tx)
        .await?;
    let stored: i64 = sqlx::query_scalar("SELECT hosts_below FROM cut_begins WHERE cut_id = $1")
        .bind(cut.0 as i64)
        .fetch_one(&mut *tx)
        .await?;
    Ok(HostId(stored as u64))
}

/// The stored decision on `cut`, recording an abort first if it has none.
async fn decision_or_abort(pool: &PgPool, cut: CutId, tick: Tick) -> Result<bool, StoreError> {
    insert_decision(pool, cut, tick, false).await?;
    cut_decision(pool, cut).await?.ok_or_else(|| no_decision(cut))
}

/// A host's "abort unless decided": records an abort if `cut` has no decision, then
/// finishes every staged part of it as the stored decision says, which it returns.
/// Unfenced on purpose — the first writer wins.
pub(crate) async fn abort_cut(pool: &PgPool, world_tag: u32, cut: CutId, tick: Tick) -> Result<bool, StoreError> {
    let d = decision_or_abort(pool, cut, tick).await?;
    finish_staged(pool, world_tag, cut, d, None).await?;
    Ok(d)
}

/// The stored decision on `cut` (`None`: undecided); a decided cut's staged parts
/// are finished as it says ([`crate::StoreHandle::cut_decided`]).
pub(crate) async fn cut_decided(pool: &PgPool, world_tag: u32, cut: CutId) -> Result<Option<bool>, StoreError> {
    let d = cut_decision(pool, cut).await?;
    if let Some(d) = d {
        finish_staged(pool, world_tag, cut, d, None).await?;
    }
    Ok(d)
}

/// Right after a part prepared: if its cut is decided already (the decider's finish
/// pass may have run before the part was staged), the part finishes itself as
/// decided. Returns the decision applied (`None`: undecided, the part stays prepared).
pub(crate) async fn finish_if_decided(pool: &PgPool, gid: &str, cut: CutId) -> Result<Option<bool>, StoreError> {
    let d = cut_decision(pool, cut).await?;
    if let Some(d) = d {
        log::warn!("[world-store] part {gid} prepared after cut {} was decided; finishing it ({d})", cut.0);
        finish_part(pool, gid, d).await?;
    }
    Ok(d)
}

/// Whether the part `gid` is staged (prepared and not finished).
pub(crate) async fn is_staged(pool: &PgPool, gid: &str) -> Result<bool, StoreError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_prepared_xacts WHERE database = current_database() AND gid = $1",
    )
    .bind(gid)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// Finishes every staged part of `cut` (whoever's) as `decided` says (also the coordinator's
/// post-commit effect of a decision, `crate::AfterCommit::FinishCut`). `stop_after`: the test
/// hook — fail after that many `COMMIT PREPARED`s.
pub(crate) async fn finish_staged(
    pool: &PgPool,
    world_tag: u32,
    cut: CutId,
    decided: bool,
    stop_after: Option<u32>,
) -> Result<(), StoreError> {
    let parts = staged_parts(pool, world_tag, Some(cut)).await?.remove(&cut).unwrap_or_default();
    let mut committed = 0u32;
    for gid in parts.values() {
        finish_part(pool, gid, decided).await?;
        if decided {
            committed += 1;
            if stop_after == Some(committed) {
                return Err(StoreError::Backend("test stop".into()));
            }
        }
    }
    Ok(())
}

/// Commits (`commit`) or rolls back one prepared part. A part that is gone was
/// finished already (by this decision before, or by another process); one that is
/// "busy" is being finished by another process right now: look again.
async fn finish_part(pool: &PgPool, gid: &str, commit: bool) -> Result<(), StoreError> {
    let verb = if commit { "COMMIT PREPARED" } else { "ROLLBACK PREPARED" };
    let sql = format!("{verb} '{}'", checked(gid));
    for _ in 0..PREPARED_BUSY_TRIES {
        match sqlx::raw_sql(&sql).execute(pool).await {
            Ok(_) => return Ok(()),
            Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("42704") => return Ok(()),
            Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("55000") => {
                tokio::time::sleep(PREPARED_BUSY_RETRY).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(StoreError::Backend(format!("{verb} {gid}: still busy after {PREPARED_BUSY_TRIES} tries")))
}

/// Entities reported per cut by [`report_entities_twice`] at most.
const ENTITIES_TWICE_LISTED: i64 = 20;

/// After a COMMITTED world cut (detect only — it cannot refuse: each host's part could
/// only judge its own regions, `rows::check_stale_rows`): any entity saved at the cut's
/// tick that also has a row in another region, e.g. one a region respawned from the
/// store still holds while it lives on another host. One `error!` per cut naming the
/// entities and their regions; whether there was any. Errors of the check itself are
/// warned and ignored.
pub(crate) async fn report_entities_twice(pool: &PgPool, cut: CutId, tick: Tick) -> bool {
    let rows = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i64)>(
        // The cut's rows through its regions (every region a cut writes gets
        // `regions.saved_tick` = the cut's tick in the same upsert), then the entities'
        // key prefix `(rx, rz)` and one `entities_id` probe per row: never a scan of
        // `entities`. (`regions` has no `saved_tick` index: one row per stored region.)
        "SELECT n.birth_rx, n.birth_rz, n.serial, n.rx, n.rz, o.rx, o.rz FROM regions r \
         JOIN entities n ON n.rx = r.rx AND n.rz = r.rz \
         JOIN entities o ON o.birth_rx = n.birth_rx AND o.birth_rz = n.birth_rz AND o.serial = n.serial \
           AND NOT (o.rx = n.rx AND o.rz = n.rz) \
         WHERE r.saved_tick = $1 ORDER BY n.birth_rx, n.birth_rz, n.serial, o.rx, o.rz LIMIT $2",
    )
    .bind(tick as i64)
    .bind(ENTITIES_TWICE_LISTED)
    .fetch_all(pool)
    .await;
    match rows {
        Ok(rows) if rows.is_empty() => false,
        Ok(rows) => {
            let list: Vec<String> = rows
                .iter()
                .map(|(bx, bz, serial, rx, rz, ox, oz)| {
                    format!("entity ({bx}, {bz})#{} in ({rx}, {rz}) and ({ox}, {oz})", *serial as u64)
                })
                .collect();
            log::error!(
                "[world-store] world cut {} at tick {tick} committed an entity stored in two regions — a conservation \
                 bug the hosts' parts could not refuse (the next start loads both copies): {}",
                cut.0,
                list.join("; ")
            );
            true
        }
        Err(e) => {
            log::warn!("[world-store] world cut {}: the check for entities stored twice failed: {e}", cut.0);
            false
        }
    }
}

/// After a COMMITTED world cut that settled successions (detect only — the pairing of a
/// settle with the heir part's `adopts` is the coordinator's invariant, which the store
/// cannot refuse without a table two parts would write): a settled succession whose
/// predecessor still has `in_flight` rows under its ordinal was settled by a cut whose heir
/// part did NOT adopt them. Those rows now outlive the settle under an id no process will
/// ever run again (Elastic 1: a restarted process is a new host): the heir did not deliver
/// them, so what they carry is lost unless an operator restores it — the breach this
/// reports. One `error!` per cut; whether there was any. The check runs right after the
/// decision (the rows are the dead host's: its id is never issued again).
/// Errors of the check itself are warned and ignored.
pub(crate) async fn report_settled_unadopted(pool: &PgPool, cut: CutId) -> bool {
    let rows = sqlx::query_as::<_, (i64, i64, i64, i64)>(
        "SELECT s.id, s.predecessor, s.heir, COUNT(*) FROM successions s JOIN in_flight f ON f.host = s.predecessor \
         WHERE s.settled_cut = $1 GROUP BY s.id, s.predecessor, s.heir ORDER BY s.id",
    )
    .bind(cut.0 as i64)
    .fetch_all(pool)
    .await;
    match rows {
        Ok(rows) if rows.is_empty() => false,
        Ok(rows) => {
            let list: Vec<String> = rows
                .iter()
                .map(|(id, pred, heir, n)| format!("succession {id}: {n} in-flight row(s) still under host {pred} (heir {heir})"))
                .collect();
            log::error!(
                "[world-store] world cut {} settled a succession whose heir part did not adopt the predecessor's \
                 in-flight rows — a conservation bug of the coordinator's settle/adopt pairing (the predecessor's next \
                 boot restores those rows a second time): {}",
                cut.0,
                list.join("; ")
            );
            true
        }
        Err(e) => {
            log::warn!("[world-store] world cut {}: the check for settled successions not adopted failed: {e}", cut.0);
            false
        }
    }
}

/// Finishes every prepared part of this world that a crash left behind: a decided
/// cut's parts as decided; an undecided cut is aborted first (the abort row carries
/// [`SWEEP_ABORT_TICK`]). Safe beside other processes doing the same.
pub(crate) async fn resolve_prepared(pool: &PgPool, world_tag: u32) -> Result<Vec<(CutId, bool)>, StoreError> {
    let cuts = staged_parts(pool, world_tag, None).await?;
    let mut out = Vec::with_capacity(cuts.len());
    for cut in cuts.into_keys() {
        let decided = decision_or_abort(pool, cut, SWEEP_ABORT_TICK).await?;
        finish_staged(pool, world_tag, cut, decided, None).await?;
        log::info!("[world-store] resolved prepared cut {} ({})", cut.0, if decided { "committed" } else { "aborted" });
        out.push((cut, decided));
    }
    Ok(out)
}

