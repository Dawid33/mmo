//! Elastic 4: the hosts the world wants (`desired_hosts`, migration `0019_desired_hosts`) —
//! [`game::coordinator::StoreDecision::AddHost`], `DrainHost` and `RemoveHost`, run inside the
//! coordinator's transition, each in a savepoint after the compare-and-set holds the
//! `coordinator` row (`coordinator_tx.rs`). Nothing else writes the table: every writer holds
//! `coordinator`, so no statement here waits for another writer's uncommitted row. None needs a
//! mark: each is idempotent to issue and to apply twice. Postgres only (SQLite: a transition
//! with decisions is `Unsupported`; the table stays empty). The lock walks: `succession.rs`'s
//! module notes.

use game::HOST_LIVE;
use game::coordinator::{
    AddHost, DesiredHost, DrainHost, HostAdded, HostDrained, HostRemoved, RemoveHost,
};
use sqlx::Row;

use crate::StoreError;
use crate::rows::i;
use crate::succession::PG_NOW_MS;

/// `desired_hosts.state` of a wanted host.
pub(crate) const DESIRED_WANTED: i64 = 0;
/// `desired_hosts.state` of a draining host.
pub(crate) const DESIRED_DRAINING: i64 = 1;

/// [`AddHost`]'s statements inside `tx` (anything but `Done` is rolled back by the caller, to
/// its savepoint). In this order:
/// 1. `SELECT next_desired_host FROM world WHERE id = 1 FOR NO KEY UPDATE` (the declaration's
///    prefix: `coordinator` → `world`; a registration holding `world` is waited for);
/// 2. a PLAIN read of every row's slot and state: wanted ≠ `expect_wanted` →
///    [`HostAdded::NotNeeded`]; rows ≥ `max` → [`HostAdded::Full`];
/// 3. the `INSERT` of `h<n>` at the lowest free slot, wanted, at the store's clock;
/// 4. `UPDATE world SET next_desired_host = n + 1`.
pub(crate) async fn add_host_in(
    tx: &mut sqlx::PgConnection,
    a: &AddHost,
) -> Result<HostAdded, StoreError> {
    let next: Option<i64> =
        sqlx::query_scalar("SELECT next_desired_host FROM world WHERE id = 1 FOR NO KEY UPDATE")
            .fetch_optional(&mut *tx)
            .await?;
    let Some(next) = next else {
        return Err(StoreError::Backend(
            "add host: the store has no world row".into(),
        ));
    };
    let rows = sqlx::query("SELECT slot, state FROM desired_hosts ORDER BY slot")
        .fetch_all(&mut *tx)
        .await?;
    let mut slots = Vec::with_capacity(rows.len());
    let mut wanted = 0u64;
    for r in &rows {
        slots.push(r.try_get::<i64, _>(0)?);
        if r.try_get::<i64, _>(1)? == DESIRED_WANTED {
            wanted += 1;
        }
    }
    if wanted != u64::from(a.expect_wanted) {
        return Ok(HostAdded::NotNeeded);
    }
    if slots.len() as u64 >= u64::from(a.max) {
        return Ok(HostAdded::Full);
    }
    // The lowest slot no row holds (`slots` ascending, distinct: the column is unique).
    let mut slot = 0i64;
    for s in slots {
        if s == slot {
            slot += 1;
        } else if s > slot {
            break;
        }
    }
    let slot = u32::try_from(slot)
        .map_err(|_| StoreError::Backend(format!("add host: slot {slot} out of range")))?;
    let name = format!("h{next}");
    sqlx::query(&format!("INSERT INTO desired_hosts (name, slot, state, created_ms) VALUES ($1, $2, {DESIRED_WANTED}, {PG_NOW_MS})"))
        .bind(&name)
        .bind(i64::from(slot))
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE world SET next_desired_host = $1 WHERE id = 1")
        .bind(next + 1)
        .execute(&mut *tx)
        .await?;
    log::info!(
        "[world-store] desired host {name} added at slot {slot} (with the coordinator's transition)"
    );
    Ok(HostAdded::Done { name, slot })
}

/// [`DrainHost`]'s statements inside `tx`. In this order:
/// 1. `UPDATE desired_hosts SET state = draining WHERE name = $1`: no row →
///    [`HostDrained::Unknown`], nothing written;
/// 2. only with `host`: `UPDATE hosts SET drain_at_ms = LEAST(drain_at_ms, now + in_ms) WHERE
///    id = $h AND boot = $b AND clean = 0 AND state = LIVE` (a mark only moves earlier; Postgres'
///    `LEAST` skips a NULL) → [`HostDrained::Marked`]; no such row (or no `host`) →
///    [`HostDrained::RowOnly`].
pub(crate) async fn drain_host_in(
    tx: &mut sqlx::PgConnection,
    d: &DrainHost,
) -> Result<HostDrained, StoreError> {
    if d.in_ms > crate::DRAIN_MAX_SECS * 1000 {
        return Err(StoreError::Unsupported(format!(
            "a drain notice of {} ms is longer than the longest, {} s (DRAIN_MAX_SECS)",
            d.in_ms,
            crate::DRAIN_MAX_SECS
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE desired_hosts SET state = {DESIRED_DRAINING} WHERE name = $1"
    ))
    .bind(&d.name)
    .execute(&mut *tx)
    .await?;
    if row.rows_affected() == 0 {
        return Ok(HostDrained::Unknown);
    }
    let Some((host, boot)) = d.host else {
        return Ok(HostDrained::RowOnly);
    };
    let marked = sqlx::query(&format!(
        "UPDATE hosts SET drain_at_ms = LEAST(drain_at_ms, {PG_NOW_MS} + $3) \
         WHERE id = $1 AND boot = $2 AND clean = 0 AND state = {HOST_LIVE}"
    ))
    .bind(i(host.0))
    .bind(i(boot))
    .bind(i(d.in_ms))
    .execute(&mut *tx)
    .await?;
    Ok(if marked.rows_affected() == 1 {
        log::warn!(
            "[world-store] desired host {} draining: host {host} marked, it leaves within {} ms",
            d.name,
            d.in_ms
        );
        HostDrained::Marked
    } else {
        log::info!(
            "[world-store] desired host {} draining (no live row of host {host} boot {boot:#x} to mark)",
            d.name
        );
        HostDrained::RowOnly
    })
}

/// [`RemoveHost`]'s one statement inside `tx`: `DELETE FROM desired_hosts WHERE name = $1` →
/// [`HostRemoved::Done`], or [`HostRemoved::Gone`] (no such row, nothing written).
pub(crate) async fn remove_host_in(
    tx: &mut sqlx::PgConnection,
    r: &RemoveHost,
) -> Result<HostRemoved, StoreError> {
    let gone = sqlx::query("DELETE FROM desired_hosts WHERE name = $1")
        .bind(&r.name)
        .execute(&mut *tx)
        .await?;
    Ok(if gone.rows_affected() == 1 {
        log::info!(
            "[world-store] desired host {} removed (its slot is free)",
            r.name
        );
        HostRemoved::Done
    } else {
        HostRemoved::Gone
    })
}

/// A `desired_hosts` row as read (`rows.rs` `desired_hosts`, both engines).
pub(crate) fn desired_of(
    name: String,
    slot: i64,
    state: i64,
    created_ms: i64,
) -> Result<DesiredHost, StoreError> {
    let slot = u32::try_from(slot).map_err(|_| {
        StoreError::Backend(format!("desired host {name}: slot {slot} out of range"))
    })?;
    Ok(DesiredHost {
        name,
        slot,
        draining: state == DESIRED_DRAINING,
        created_ms: u64::try_from(created_ms).unwrap_or(0),
    })
}
