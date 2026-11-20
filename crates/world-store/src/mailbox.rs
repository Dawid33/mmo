//! The host mailbox and the login answers (coordinator HTTP API, migration `0020_mailbox`).
//!
//! `outbox` holds, per host, the messages coordinator transitions decided for it; `login_outbox`
//! the coordinator's answers to the login service. Rows of both are inserted ONLY inside a
//! transition (`coordinator_tx.rs` `write_pg`), after its compare-and-set locked the
//! `coordinator` row — so `seq` order is commit order, and a reader past `seq > after` never
//! misses a lower `seq` committing later. Nothing here takes the `coordinator` row (lock walk:
//! `succession.rs`'s notes, "The mailbox").
//!
//! - [`mailbox_read_pg`]: a host's poll. Its rows at or below its cursor `after` are deleted
//!   (delivered: the host acknowledges them by asking past them) and the next ones returned, in
//!   one statement — delivery at least once, the host drops `seq <= ` its cursor: exactly
//!   once, in order. The delete touches only `(host, seq <= after)`.
//! - [`mailbox_oldest_pg`]: per host with unread rows, the boot and `at_ms` of its oldest and
//!   how many it has (how long a host has not polled — the ticker's stall rule — and the
//!   mailbox gauges).
//! - [`login_answers_read_pg`]: the login answers past `after` (from the start when `after` is
//!   past the newest row: another incarnation's cursor); never deleted by a read (a transition
//!   prunes them by age).
//!
//! Postgres only: SQLite (one process, role `all`) answers `Unsupported`.

use game::HostId;
use sqlx::{PgPool, Row};

use crate::StoreError;

/// One host's unread mailbox, as [`mailbox_oldest_pg`] reads it: `(host, boot, at_ms, rows)` — the
/// boot and `at_ms` of its oldest row, and how many rows it has.
pub type MailboxHead = (HostId, u64, u64, u64);

/// What SQLite answers for every mailbox call.
pub(crate) const MAILBOX_UNSUPPORTED: &str =
    "the host mailbox and the login answers need Postgres (the coordinator's HTTP API)";

/// Host `host`'s rows at or below `after` deleted, then at most `limit` of its rows after `after`
/// (in `seq` order) of its boot `boot` — `(seq, msg)`. Rows of another boot are not returned (a
/// row for an older boot is left for the next `Attach`'s drop, or deleted with the cursor).
pub(crate) async fn mailbox_read_pg(
    pool: &PgPool,
    host: HostId,
    boot: u64,
    after: u64,
    limit: u32,
) -> Result<Vec<(u64, Vec<u8>)>, StoreError> {
    // ONE statement: the delete (a data-modifying CTE) and the read share its snapshot; they
    // touch disjoint rows (`seq <= after` vs `seq > after`).
    let rows = sqlx::query(
        "WITH delivered AS (DELETE FROM outbox WHERE host = $1 AND seq <= $3) \
         SELECT seq, msg FROM outbox WHERE host = $1 AND boot = $2 AND seq > $3 ORDER BY seq LIMIT $4",
    )
    .bind(crate::rows::i(host.0))
    .bind(crate::rows::i(boot))
    .bind(crate::rows::i(after))
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| Ok((r.try_get::<i64, _>(0)? as u64, r.try_get(1)?)))
        .collect()
}

/// One row per host with unread rows: `(host, boot, at_ms, rows)` — the boot and `at_ms` of its
/// oldest (lowest `seq`) and how many rows it has (every boot's). One plain read, no lock.
pub(crate) async fn mailbox_oldest_pg(pool: &PgPool) -> Result<Vec<MailboxHead>, StoreError> {
    let rows = sqlx::query(
        "SELECT DISTINCT ON (host) host, boot, at_ms, COUNT(*) OVER (PARTITION BY host) FROM outbox ORDER BY host, seq",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok((
                HostId(r.try_get::<i64, _>(0)? as u64),
                r.try_get::<i64, _>(1)? as u64,
                r.try_get::<i64, _>(2)? as u64,
                r.try_get::<i64, _>(3)? as u64,
            ))
        })
        .collect()
}

/// At most `limit` login answers after `after`, in `seq` order — `(seq, msg)`. A cursor past the
/// newest row kept is from another incarnation of the table (the world re-forged into a new
/// database while the login service ran: its sequence started again) or the table is empty
/// (everything pruned): the answers are read from the start. A cursor of THIS table is never past
/// its newest row while one is kept — the prune deletes the oldest first.
pub(crate) async fn login_answers_read_pg(
    pool: &PgPool,
    after: u64,
    limit: u32,
) -> Result<Vec<(u64, Vec<u8>)>, StoreError> {
    let rows = sqlx::query(
        "SELECT seq, msg FROM login_outbox \
         WHERE seq > (CASE WHEN $1 > COALESCE((SELECT MAX(seq) FROM login_outbox), 0) THEN 0 ELSE $1 END) \
         ORDER BY seq LIMIT $2",
    )
    .bind(crate::rows::i(after))
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| Ok((r.try_get::<i64, _>(0)? as u64, r.try_get(1)?)))
        .collect()
}
