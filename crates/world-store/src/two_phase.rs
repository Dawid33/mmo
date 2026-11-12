//! Two-phase save cuts (R3, Postgres only). Each host writes its fenced part of a
//! cut and ends the transaction with `PREPARE TRANSACTION '<gid>'`
//! ([`crate::rows::CutEnd::Prepare`]); the coordinator then records ONE decision
//! row (`cut_decisions`, first writer wins on the cut id) and issues
//! `COMMIT PREPARED` / `ROLLBACK PREPARED` for every part. A host that gives up
//! records an abort the same way ([`abort_cut`]); a process starting up finishes or
//! aborts whatever a crash left prepared ([`resolve_prepared`]).
//!
//! Locks: a prepared part keeps every row lock it took — `FOR SHARE` on its host's
//! `super_regions` rows (so a claim of them waits for the decision) and the rows it
//! wrote — but none on `world` (it reads the generation without a lock and writes no
//! world tick). The decision locks `coordinator` (`FOR SHARE`), then `world`
//! (`FOR UPDATE`, a commit only), `cut_decisions` and `hosts`: no row a prepared part
//! holds, so a decision never waits for a part.
//!
//! None of this takes the store's writer lock: a prepare queued behind a part that
//! holds a row it needs waits for the decision, which must not wait for that prepare.

use std::collections::BTreeMap;
use std::time::Duration;

use game::{CutId, Tick};
use sqlx::PgPool;

use crate::{CoordinatorFence, StoreError};

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

/// The gid of host `host`'s prepared part of `cut`. `world_tag` tells the worlds of
/// one Postgres cluster apart (gids are cluster-wide): the database's OID.
pub fn cut_gid(world_tag: u32, cut: CutId, host: u32) -> String {
    format!("{GID_PREFIX}{world_tag}-{}-{host}", cut.0)
}

/// Whether `gid` has the shape [`cut_gid`] gives: it is formatted into SQL (a gid
/// cannot be a bind parameter), so nothing else may get there.
fn gid_ok(gid: &str) -> bool {
    gid.len() <= GID_MAX_BYTES
        && gid.strip_prefix(GID_PREFIX).is_some_and(|rest| {
            !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit() || b == b'-')
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

/// `(cut, host)` of a gid of world `world_tag`; `None` for any other gid.
fn parse_gid(world_tag: u32, gid: &str) -> Option<(CutId, u32)> {
    let rest = gid.strip_prefix(GID_PREFIX)?;
    let mut parts = rest.split('-');
    let (tag, cut, host) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || tag.parse::<u32>().ok()? != world_tag {
        return None;
    }
    Some((CutId(cut.parse().ok()?), host.parse().ok()?))
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
/// inserted it. In `tx` (the decision) or on its own (an abort).
async fn insert_decision(
    conn: &mut sqlx::PgConnection,
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
    .execute(&mut *conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

async fn stored_decision(conn: &mut sqlx::PgConnection, cut: CutId) -> Result<Option<bool>, StoreError> {
    let d: Option<i64> = sqlx::query_scalar("SELECT committed FROM cut_decisions WHERE cut_id = $1")
        .bind(cut.0 as i64)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(d.map(|d| d != 0))
}

/// The coordinator's decision on `cut` (see [`crate::StoreHandle::decide_cut`]).
/// `stop_after`: the test hook — fail after that many `COMMIT PREPARED`s.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn decide_cut(
    pool: &PgPool,
    world_tag: u32,
    cut: CutId,
    tick: Tick,
    commit: bool,
    hosts: Vec<u32>,
    leaving: Vec<u32>,
    fence: CoordinatorFence,
    generation: u64,
    stop_after: Option<u32>,
) -> Result<bool, StoreError> {
    let mut tx = pool.begin().await?;
    // Held to the end: a newer coordinator's claim waits for this decision.
    let epoch: Option<i64> = sqlx::query_scalar("SELECT epoch FROM coordinator WHERE id = 1 FOR SHARE")
        .fetch_optional(&mut *tx)
        .await?;
    if epoch.map(|e| e as u64) != Some(fence.epoch) {
        return Err(StoreError::Fenced { sr: None });
    }
    let decided = match stored_decision(&mut tx, cut).await? {
        Some(d) => d,
        None => {
            if commit {
                let stored: i64 = sqlx::query_scalar("SELECT generation FROM world WHERE id = 1 FOR UPDATE")
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?;
                if stored as u64 != generation {
                    return Err(StoreError::StaleGeneration { stored: stored as u64, mine: generation });
                }
            }
            let inserted = insert_decision(&mut tx, cut, tick, commit).await?;
            // A host's abort may have won the insert meanwhile.
            let d = stored_decision(&mut tx, cut)
                .await?
                .ok_or_else(|| StoreError::Corrupt(format!("cut {} has no decision after its insert", cut.0)))?;
            if inserted && d {
                sqlx::query("UPDATE world SET world_tick = $1 WHERE id = 1 AND world_tick < $1")
                    .bind(tick as i64)
                    .execute(&mut *tx)
                    .await?;
                let leaving: Vec<i64> = leaving.iter().map(|h| i64::from(*h)).collect();
                sqlx::query("UPDATE hosts SET clean = 1 WHERE ordinal = ANY($1)")
                    .bind(leaving)
                    .execute(&mut *tx)
                    .await?;
            }
            d
        }
    };
    tx.commit().await?;
    let mut committed = 0u32;
    for host in hosts {
        finish_part(pool, &cut_gid(world_tag, cut, host), decided).await?;
        if decided {
            committed += 1;
            if stop_after == Some(committed) {
                return Err(StoreError::Backend("test stop".into()));
            }
        }
    }
    Ok(decided)
}

/// A host's "abort unless decided": records an abort if `cut` has no decision and
/// returns the stored one. Unfenced on purpose — the first writer wins.
pub(crate) async fn abort_cut(pool: &PgPool, cut: CutId, tick: Tick) -> Result<bool, StoreError> {
    let mut conn = pool.acquire().await?;
    insert_decision(&mut conn, cut, tick, false).await?;
    stored_decision(&mut conn, cut)
        .await?
        .ok_or_else(|| StoreError::Corrupt(format!("cut {} has no decision after its insert", cut.0)))
}

/// Commits (`commit`) or rolls back one prepared part. A part that is gone was
/// finished already (by this decision before, or by another process's sweep); one
/// that is "busy" is being finished by another process right now: look again.
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

/// Finishes every prepared part of this world that a crash left behind: a decided
/// cut's parts as decided; an undecided cut is aborted first (the sweep does not know
/// its tick: the abort row says 0). Safe beside other processes doing the same.
pub(crate) async fn resolve_prepared(pool: &PgPool, world_tag: u32) -> Result<Vec<(CutId, bool)>, StoreError> {
    let gids: Vec<String> = sqlx::query_scalar(
        "SELECT gid FROM pg_prepared_xacts WHERE database = current_database() AND gid LIKE 'mmo-cut-%'",
    )
    .fetch_all(pool)
    .await?;
    let mut cuts: BTreeMap<CutId, Vec<String>> = BTreeMap::new();
    for gid in gids {
        if let Some((cut, _)) = parse_gid(world_tag, &gid) {
            cuts.entry(cut).or_default().push(gid);
        }
    }
    let mut out = Vec::with_capacity(cuts.len());
    for (cut, gids) in cuts {
        let decided = match stored_decision(&mut *pool.acquire().await?, cut).await? {
            Some(d) => d,
            None => abort_cut(pool, cut, 0).await?,
        };
        for gid in gids {
            finish_part(pool, &gid, decided).await?;
        }
        log::info!("[world-store] resolved prepared cut {} ({})", cut.0, if decided { "committed" } else { "aborted" });
        out.push((cut, decided));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gids_round_trip_and_stay_in_their_world() {
        let gid = cut_gid(4242, CutId(u64::MAX), u32::MAX);
        assert!(gid_ok(&gid) && gid.len() <= GID_MAX_BYTES);
        assert_eq!(parse_gid(4242, &gid), Some((CutId(u64::MAX), u32::MAX)));
        assert_eq!(parse_gid(4243, &gid), None, "another world's");
        assert_eq!(parse_gid(4242, "mmo-cut-4242-1"), None);
        assert!(!gid_ok("mmo-cut-1'; DROP TABLE world; --"));
        assert!(!gid_ok("mmo-cut-"));
    }
}
