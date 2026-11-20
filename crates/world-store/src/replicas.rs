//! The coordinator's replicas' rows (Elastic 2b Task 4, migration `0017`): `coordinator_replicas`
//! — one row per coordinator process start, its id issued from `coordinator.next_replica` and
//! never reused — and the reads a replica runs beside its transitions (the row's version, the
//! row with its history only when newer).
//!
//! **A registration** ([`StoreHandle::register_replica`]): ONE transaction. Its lock walk (the
//! module notes of `succession.rs`): the `coordinator` row FIRST (the upsert that issues the id;
//! Postgres `FOR NO KEY UPDATE` as a transition's compare-and-set takes it), then
//! `coordinator_replicas` rows (the lapse of the stale live ones, the prune, the insert). Every
//! statement that locks `coordinator` takes it first, and nothing that holds a
//! `coordinator_replicas` row waits for `coordinator`: no cycle.
//!
//! **A beat** ([`StoreHandle::beat_replica`]): one `UPDATE` of the caller's own live row, on the
//! store's beat connection (Postgres: never one of the shared pool's) under a 1 s
//! `lock_timeout` ([`crate::HEARTBEAT_LOCK_TIMEOUT_MS`]); it holds that row and wants nothing
//! after it. It waits at most for a transition's `EndReplica` or a registration's lapse holding
//! the row; once either commits it answers "not live" (the replica exits fenced).
//!
//! **The end** (`coordinator_tx.rs`, [`game::coordinator::StoreDecision::EndReplica`]): inside a
//! transition, after its compare-and-set holds `coordinator` — one `UPDATE` of that replica's row.
//!
//! SQLite (role `all`): the one replica registers; nothing beats, reads the rows or ends one.

use game::coordinator::{REPLICA_DEAD, REPLICA_LIVE, ReplicaId, ReplicaRow};

use crate::StoreError;

/// What [`crate::StoreHandle::register_replica`] decided: the id issued, and whether no other
/// replica was live (none beaten within the lease — those rows were marked dead: a total replica
/// crash, or the first replica of the world).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplicaRegistered {
    pub replica: ReplicaId,
    pub alone: bool,
}

/// Dead `coordinator_replicas` rows older than this (by the database clock, ms since their
/// last beat) are pruned at a registration: a day.
pub const REPLICA_PRUNE_MS: u64 = 24 * 3600 * 1000;

fn u(v: i64) -> u64 {
    v as u64
}

fn i(v: u64) -> i64 {
    v as i64
}

macro_rules! engine_replicas {
    ($module:ident, $db:ty, $lock_timeout:expr, $now_ms:expr) => {
        pub(crate) mod $module {
            use sqlx::Row;

            use super::*;

            type Pool = sqlx::Pool<$db>;

            /// The database clock in ms since the epoch.
            const NOW_MS: &str = $now_ms;

            /// [`crate::StoreHandle::register_replica`] (the module notes): the id issued from
            /// `coordinator.next_replica` (the row created, empty, if there is none yet: the
            /// bootstrap fills it), every OTHER live row not beaten within `lease_ms` marked dead
            /// (`alone` when none is left live), the dead rows older than [`REPLICA_PRUNE_MS`]
            /// deleted, the new row inserted live and seen now.
            pub(crate) async fn register_replica(
                pool: &Pool,
                addr: &str,
                lease_ms: u64,
                lock_ms: u64,
            ) -> Result<ReplicaRegistered, StoreError> {
                let mut tx = pool.begin().await?;
                if $lock_timeout {
                    sqlx::query(&format!("SET LOCAL lock_timeout = '{lock_ms}ms'")).execute(&mut *tx).await?;
                }
                // 1. `coordinator` first: the id. A row not there yet is made empty (no core:
                // `load_coordinator` sees none, the bootstrap writes it at epoch 1, version 1).
                let id: i64 = sqlx::query_scalar(
                    "INSERT INTO coordinator (id, epoch, version, next_replica) VALUES (1, 0, 0, 2) \
                     ON CONFLICT (id) DO UPDATE SET next_replica = coordinator.next_replica + 1 \
                     RETURNING next_replica - 1",
                )
                .fetch_one(&mut *tx)
                .await?;
                // 2. The lapsed live rows: dead (their next beat answers "not live").
                let lapse = format!(
                    "UPDATE coordinator_replicas SET state = {REPLICA_DEAD} WHERE state = {REPLICA_LIVE} AND {NOW_MS} - seen_ms > $1"
                );
                let lapsed = sqlx::query(&lapse).bind(i(lease_ms)).execute(&mut *tx).await?.rows_affected();
                let live: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM coordinator_replicas WHERE state = {REPLICA_LIVE}"))
                    .fetch_one(&mut *tx)
                    .await?;
                // 3. Rows nothing reads any more.
                let prune = format!("DELETE FROM coordinator_replicas WHERE state = {REPLICA_DEAD} AND {NOW_MS} - seen_ms > $1");
                sqlx::query(&prune).bind(i(REPLICA_PRUNE_MS)).execute(&mut *tx).await?;
                // 4. The new row.
                let insert = format!(
                    "INSERT INTO coordinator_replicas (replica, seen_ms, state, addr, registered_utc) \
                     VALUES ($1, {NOW_MS}, {REPLICA_LIVE}, $2, {NOW_MS})"
                );
                sqlx::query(&insert).bind(id).bind(addr).execute(&mut *tx).await?;
                tx.commit().await?;
                let registered = ReplicaRegistered { replica: ReplicaId(u(id)), alone: live == 0 };
                log::info!(
                    "[world-store] registered coordinator replica {} at {addr:?}{}{}",
                    registered.replica,
                    if registered.alone { " alone" } else { "" },
                    if lapsed > 0 { format!(" ({lapsed} lapsed replica row(s) marked dead)") } else { String::new() },
                );
                Ok(registered)
            }

            /// One beat of replica `me`'s row (SQLite; Postgres beats on the beat connection):
            /// whether it is still live.
            #[allow(dead_code)]
            pub(crate) async fn beat_replica(pool: &Pool, me: ReplicaId) -> Result<bool, StoreError> {
                let row = sqlx::query(&super::beat_sql(NOW_MS)).bind(i(me.0)).fetch_optional(pool).await?;
                Ok(row.is_some())
            }

            /// Every `coordinator_replicas` row, by id.
            pub(crate) async fn replicas(pool: &Pool) -> Result<Vec<ReplicaRow>, StoreError> {
                let rows = sqlx::query("SELECT replica, seen_ms, state, addr FROM coordinator_replicas ORDER BY replica")
                    .fetch_all(pool)
                    .await?;
                rows.into_iter()
                    .map(|r| {
                        Ok(ReplicaRow {
                            replica: ReplicaId(u(r.try_get(0)?)),
                            seen_ms: u(r.try_get(1)?),
                            live: r.try_get::<i64, _>(2)? == REPLICA_LIVE,
                            addr: r.try_get(3)?,
                        })
                    })
                    .collect()
            }

            /// The `coordinator` row's version (0: no row).
            pub(crate) async fn row_version(pool: &Pool) -> Result<u64, StoreError> {
                let v: Option<i64> = sqlx::query_scalar("SELECT version FROM coordinator WHERE id = 1").fetch_optional(pool).await?;
                Ok(v.map_or(0, u))
            }

            /// The coordinator's projection with its history only if a transition wrote it after
            /// version `history_version` (else `history` empty); `None`: no projection.
            pub(crate) async fn load_coordinator_since(
                pool: &Pool,
                history_version: u64,
            ) -> Result<Option<crate::StoredCoordinator>, StoreError> {
                let row = sqlx::query(
                    "SELECT version, core, history_version, CASE WHEN history_version > $1 THEN history ELSE NULL END \
                     FROM coordinator WHERE id = 1",
                )
                .bind(i(history_version))
                .fetch_optional(pool)
                .await?;
                let Some(r) = row else { return Ok(None) };
                let core: Option<Vec<u8>> = r.try_get(1)?;
                let Some(core) = core else { return Ok(None) };
                let history: Option<Vec<u8>> = r.try_get(3)?;
                Ok(Some(crate::StoredCoordinator {
                    version: u(r.try_get(0)?),
                    core,
                    history: history.unwrap_or_default(),
                    history_version: u(r.try_get(2)?),
                }))
            }
        }
    };
}

/// A replica's beat (both engines; Postgres runs it on the beat connection): `seen_ms` moves for
/// the caller's LIVE row only. No row returned: ended.
pub(crate) fn beat_sql(now_ms: &str) -> String {
    format!(
        "UPDATE coordinator_replicas SET seen_ms = {now_ms} WHERE replica = $1 AND state = {REPLICA_LIVE} RETURNING replica"
    )
}

/// The end of replica `replica`'s row inside a transition ([`game::coordinator::StoreDecision::EndReplica`]):
/// whether a live row was ended (`false`: none — gone, or ended already).
pub(crate) async fn end_replica_in(
    tx: &mut sqlx::PgConnection,
    replica: ReplicaId,
) -> Result<bool, StoreError> {
    let sql = format!(
        "UPDATE coordinator_replicas SET state = {REPLICA_DEAD} WHERE replica = $1 AND state = {REPLICA_LIVE}"
    );
    Ok(sqlx::query(&sql)
        .bind(i(replica.0))
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0)
}

engine_replicas!(
    sqlite,
    sqlx::Sqlite,
    false,
    "CAST(unixepoch('subsec') * 1000 AS INTEGER)"
);
engine_replicas!(pg, sqlx::Postgres, true, crate::succession::PG_NOW_MS);
