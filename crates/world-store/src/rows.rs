//! The row SQL, written once and instantiated per engine (`sqlite`, `pg`):
//! runtime `sqlx::query` only, `$N` placeholders (SQLite reads them too).
//! u64 values (ticks, serials, player ids) are stored as i64 by bit cast.
//!
//! Writes go through the store thread's writer lock (`thread.rs`), so two
//! write transactions never race on SQLite's deferred upgrade. Fenced writes
//! and loads check the owner epochs first in their own transaction (`fence.rs`).

use std::collections::BTreeMap;

use game::{
    Chunk, ChunkCoords, ColumnCoords, ColumnGround, CutId, EntityBundle, EntityId, EntityKind, InFlightRow,
    IntentMsg, PlayerId, PlayerRecord, RegionCoords, RegionMeta, StoredRegion, TerrainSnapshot, Tick, WorldCut,
};

use crate::codec::*;
use crate::fence::{CoordinatorFence, Fence, SuperRegion, commit_check_set, load_check};
use crate::forged::{ForgedHead, TiledRows};
use game::ClockAnchor;
use crate::{CutCommitted, HostBoot, HostRow, StoreError, WorldInfo};

/// Super-regions checked per fence query (two binds each).
const FENCE_CHECK_BATCH: usize = 200;

/// The fence query over `n` super-regions, `lock` appended (Postgres:
/// `FOR SHARE`, in key order so it never deadlocks with a claim's upserts).
fn fence_check_sql(n: usize, lock: &str) -> String {
    let wanted: Vec<String> = (0..n).map(|k| format!("(sx = ${} AND sz = ${})", 2 * k + 1, 2 * k + 2)).collect();
    format!("SELECT sx, sz, owner, epoch FROM super_regions WHERE {} ORDER BY sx, sz{lock}", wanted.join(" OR "))
}

/// A fence check that lost a race with a claim (Postgres: a serialization
/// failure under REPEATABLE READ, or a deadlock) is fenced: claims always
/// bump the epoch, so the row it waited on no longer holds this claim.
fn fence_race(e: sqlx::Error, sr: SuperRegion) -> StoreError {
    match &e {
        sqlx::Error::Database(d) if matches!(d.code().as_deref(), Some("40001") | Some("40P01")) => {
            StoreError::Fenced { sr: Some(sr) }
        }
        _ => e.into(),
    }
}

/// u64 → i64, bit for bit.
pub(crate) fn i(v: u64) -> i64 {
    v as i64
}

/// i64 → u64, bit for bit.
fn u(v: i64) -> u64 {
    v as u64
}

/// `in_flight.kind` of a transfer.
const IN_FLIGHT_TRANSFER: i64 = 0;
/// `in_flight.kind` of an intent or a reply.
const IN_FLIGHT_INTENT: i64 = 1;

/// `entities.kind`: the bundle's kind, for queries that never decode it.
fn kind_code(kind: &EntityKind) -> i64 {
    match kind {
        EntityKind::Player => 0,
        EntityKind::Slime { .. } => 1,
        EntityKind::Person { .. } => 2,
        EntityKind::Body => 3,
    }
}

fn region_of(rx: i64, rz: i64) -> Result<RegionCoords, StoreError> {
    let x = i32::try_from(rx).map_err(|_| StoreError::Corrupt(format!("region x {rx} out of range")))?;
    let z = i32::try_from(rz).map_err(|_| StoreError::Corrupt(format!("region z {rz} out of range")))?;
    Ok(RegionCoords::new(x, z))
}

fn chunk_of(rc: RegionCoords, cx: i64, cy: i64, cz: i64) -> Result<ChunkCoords, StoreError> {
    let bad = || StoreError::Corrupt(format!("region {rc:?}: chunk ({cx}, {cy}, {cz}) out of range"));
    Ok(ChunkCoords::new(
        usize::try_from(cx).map_err(|_| bad())?,
        i32::try_from(cy).map_err(|_| bad())?,
        usize::try_from(cz).map_err(|_| bad())?,
    ))
}

/// The same entity in two regions of one cut (or twice in one): a conservation bug,
/// kept loud.
fn entity_twice(id: EntityId, rc: RegionCoords, tick: Tick) -> StoreError {
    StoreError::Corrupt(format!(
        "entity {id:?} in two regions of the cut at tick {tick} (again in {rc:?}): an entity was \
         saved twice — a conservation bug; the cut was not committed"
    ))
}

/// Refuses a cut (one host's part) that holds an entity in two of its regions, or
/// twice in one, before anything is written. `entities` is keyed per region (migration
/// 0009: two hosts' parts never touch one row), so the database no longer refuses it;
/// across parts the cut-ownership protocol keeps every entity in exactly one part.
fn check_entities_once(cut: &WorldCut) -> Result<(), StoreError> {
    let mut seen = std::collections::BTreeSet::new();
    for (rc, region) in &cut.regions {
        for b in &region.entities {
            if !seen.insert(b.id) {
                return Err(entity_twice(b.id, *rc, cut.tick));
            }
        }
    }
    Ok(())
}

/// An entity of the cut still stored in `stored`, a region the cut does not rewrite
/// ([`pg::run_cut`]'s stale-row guard): saving it would keep it in two regions.
fn entity_stale(id: EntityId, saved: RegionCoords, stored: RegionCoords, tick: Tick) -> StoreError {
    StoreError::Corrupt(format!(
        "entity {id:?} in two regions of the cut at tick {tick}: saved in {saved:?}, still stored in \
         {stored:?}, which the cut does not rewrite — an entity would be saved twice, a conservation \
         bug; the cut was not committed"
    ))
}

/// The guard against a stale row: an entity this cut writes (`n`, a row of a region of
/// the cut — every such row was just written) with a row in another region (`o`, by
/// [`check_entities_once`] not one of the cut's), for a batch of `n` of the cut's
/// regions (`$1` on); `owned`: only rows in this host's regions (a prepared part:
/// owner and side bound after the regions). One row is enough to refuse.
fn stale_row_sql(n: usize, owned: bool) -> String {
    let regions: Vec<String> = (0..n).map(|k| format!("(n.rx = ${} AND n.rz = ${})", 2 * k + 1, 2 * k + 2)).collect();
    // Postgres only (a prepared part): the other row's region lies in a super-region
    // owned by `$o` (`super_region_of`: floor division by `$s`, the side).
    let owned = if owned {
        let (o, side) = (2 * n + 1, 2 * n + 2);
        format!(
            " AND EXISTS (SELECT 1 FROM super_regions sr WHERE sr.owner = ${o} \
               AND sr.sx = FLOOR(o.rx::numeric / ${side}::numeric)::bigint \
               AND sr.sz = FLOOR(o.rz::numeric / ${side}::numeric)::bigint)"
        )
    } else {
        String::new()
    };
    format!(
        "SELECT n.birth_rx, n.birth_rz, n.serial, n.rx, n.rz, o.rx, o.rz FROM entities n \
         JOIN entities o ON o.birth_rx = n.birth_rx AND o.birth_rz = n.birth_rz AND o.serial = n.serial \
           AND NOT (o.rx = n.rx AND o.rz = n.rz) \
         WHERE ({}){owned} LIMIT 1",
        regions.join(" OR ")
    )
}

/// An entity insert failing on the primary key `(rx, rz, birth…, serial)` is the same
/// entity twice in one region of the cut — [`check_entities_once`] refuses that before
/// anything is written; this is the backstop.
fn entity_insert_error(e: sqlx::Error, id: EntityId, rc: RegionCoords, tick: Tick) -> StoreError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => entity_twice(id, rc, tick),
        _ => e.into(),
    }
}

fn ground_blob(ground: Option<&Vec<(ColumnCoords, ColumnGround)>>) -> Vec<u8> {
    match ground {
        Some(g) => encode(FORMAT_REGION_GROUND, g),
        None => encode(FORMAT_REGION_GROUND, &Vec::<(ColumnCoords, ColumnGround)>::new()),
    }
}

/// How a cut's transaction ends ([`pg::run_cut`]).
pub(crate) enum CutEnd {
    /// One phase: the cut commits and writes the world tick.
    Commit,
    /// Two phase (Postgres): `world.generation` must be `generation`; the world tick
    /// is left to the decision; the transaction ends with `PREPARE TRANSACTION 'gid'`.
    Prepare { gid: String, generation: u64 },
}

macro_rules! engine_rows {
    ($module:ident, $db:ty, $read_prelude:expr, $share_lock:expr, $update_lock:expr, $lock_timeout:expr, $claims_lock:expr, $now_ms:expr) => {
        pub(crate) mod $module {
            use sqlx::Row;

            use super::*;

            type Pool = sqlx::Pool<$db>;
            type Tx = sqlx::Transaction<'static, $db>;
            type Conn = <$db as sqlx::Database>::Connection;

            /// A read transaction: one consistent view for multi-statement loads.
            async fn begin_read(pool: &Pool) -> Result<Tx, StoreError> {
                let mut tx = pool.begin().await?;
                let prelude: Option<&str> = $read_prelude;
                if let Some(sql) = prelude {
                    sqlx::query(sql).execute(&mut *tx).await?;
                }
                Ok(tx)
            }

            pub(crate) async fn world_info(pool: &Pool) -> Result<WorldInfo, StoreError> {
                let mut tx = begin_read(pool).await?;
                let row = sqlx::query("SELECT content_crc, world_tick FROM world WHERE id = 1")
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?;
                let crc: i64 = row.try_get(0)?;
                let tick: i64 = row.try_get(1)?;
                let artifacts: i64 = sqlx::query("SELECT COUNT(*) FROM forged_archive")
                    .fetch_one(&mut *tx)
                    .await?
                    .try_get(0)?;
                tx.commit().await?;
                let has_artifact = artifacts > 0;
                let content_crc = if has_artifact {
                    Some(u32::try_from(crc).map_err(|_| StoreError::Corrupt(format!("content_crc {crc}")))?)
                } else {
                    None
                };
                Ok(WorldInfo { content_crc, world_tick: u(tick), has_artifact })
            }

            /// `world.super_region_regions`: the super-region side the fences use.
            pub(crate) async fn super_region_regions(pool: &Pool) -> Result<u32, StoreError> {
                let s: i64 = sqlx::query("SELECT super_region_regions FROM world WHERE id = 1")
                    .fetch_optional(pool)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?
                    .try_get(0)?;
                u32::try_from(s)
                    .ok()
                    .filter(|s| *s > 0)
                    .ok_or_else(|| StoreError::Corrupt(format!("world.super_region_regions {s}")))
            }

            /// Begins a claim's transaction (also [`release`]'s): bounded (Postgres) by
            /// [`crate::CLAIM_LOCK_TIMEOUT_MS`] — a prepared cut part holds its host's rows `FOR
            /// SHARE` until its cut is decided; one nobody decides would hold the claim for ever.
            async fn begin_claim(pool: &Pool) -> Result<Tx, StoreError> {
                let mut tx = pool.begin().await?;
                cut_lock_timeout(&mut *tx, crate::CLAIM_LOCK_TIMEOUT_MS).await?;
                Ok(tx)
            }

            /// Re-reads the super-region side in a claim's transaction (Postgres: `FOR KEY
            /// SHARE`, the row [`set_super_region_regions`] locks first — it waits only for that
            /// `FOR UPDATE`, not for a cut's ordinary update of the world row) and refuses with
            /// [`StoreError::OtherSuperRegionSide`] if it is not `opened`, the side the handle read
            /// at open (`worldctl super-region` changed it since).
            async fn check_side(tx: &mut Tx, opened: u32) -> Result<(), StoreError> {
                let key_share = if $share_lock.is_empty() { "" } else { " FOR KEY SHARE" };
                let sql = format!("SELECT super_region_regions FROM world WHERE id = 1{key_share}");
                let stored: i64 = sqlx::query(&sql)
                    .fetch_optional(&mut **tx)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?
                    .try_get(0)?;
                if stored != i64::from(opened) {
                    let stored = u32::try_from(stored).unwrap_or(0);
                    return Err(StoreError::OtherSuperRegionSide { stored, opened });
                }
                Ok(())
            }

            /// A claim's (or release's) statement error: a `lock_timeout` (Postgres `55P03`)
            /// ends the transaction and names the prepared transactions of this world that may
            /// hold the rows ([`crate::open::claim_timeout_message`]).
            async fn claim_error(pool: &Pool, tx: Tx, e: sqlx::Error) -> StoreError {
                match &e {
                    sqlx::Error::Database(d) if d.code().as_deref() == Some("55P03") => {
                        drop(tx);
                        let gids: Vec<String> = sqlx::query_scalar(
                            "SELECT gid FROM pg_prepared_xacts WHERE database = current_database() ORDER BY gid",
                        )
                        .fetch_all(pool)
                        .await
                        .unwrap_or_default();
                        StoreError::Backend(crate::open::claim_timeout_message(crate::CLAIM_LOCK_TIMEOUT_MS, &gids))
                    }
                    _ => e.into(),
                }
            }

            /// Claims every super-region of `srs` for `owner` in one transaction:
            /// a new row gets epoch 1, a known one epoch + 1 (whoever held it is
            /// fenced). Rows are upserted in key order. Callers hold the writer lock.
            ///
            /// `opened` is the super-region side this handle read at open (`srs` are of that
            /// side): see [`check_side`]. `live`: the claimer's boot — the claim is refused
            /// unless that boot's `hosts` row is live ([`check_live`]); `None` (a boot claim
            /// before registration, `AllInOne` / `Static`, tools) checks nothing.
            pub(crate) async fn claim_super_regions(
                pool: &Pool,
                owner: u32,
                srs: Vec<SuperRegion>,
                opened: u32,
                live: Option<u64>,
            ) -> Result<Fence, StoreError> {
                let srs: std::collections::BTreeSet<SuperRegion> = srs.into_iter().collect();
                let mut tx = begin_claim(pool).await?;
                check_side(&mut tx, opened).await?;
                let mut epochs = BTreeMap::new();
                for (sx, sz) in srs {
                    let row = sqlx::query(
                        "INSERT INTO super_regions (sx, sz, owner, epoch) VALUES ($1, $2, $3, 1) \
                         ON CONFLICT (sx, sz) DO UPDATE SET owner = excluded.owner, epoch = super_regions.epoch + 1 \
                         RETURNING epoch",
                    )
                    .bind(i64::from(sx))
                    .bind(i64::from(sz))
                    .bind(i64::from(owner))
                    .fetch_one(&mut *tx)
                    .await;
                    let row = match row {
                        Ok(row) => row,
                        Err(e) => return Err(claim_error(pool, tx, e).await),
                    };
                    let epoch: i64 = row.try_get(0)?;
                    epochs.insert((sx, sz), u(epoch));
                }
                if let Some(boot) = live {
                    tx = check_live(pool, tx, owner, boot).await?;
                }
                tx.commit().await?;
                Ok(Fence { owner, epochs, all_hosts: false })
            }

            /// Refuses a claim by `owner` (boot `boot`) unless that boot's `hosts` row is live:
            /// a host declared dead (R4, its land moved to an heir) or replaced by another boot
            /// wins nothing — `Fenced { sr: None }`, the transaction rolled back. The LAST
            /// statement of the claim (Postgres: the row `FOR SHARE`): the claim never holds
            /// the `hosts` row while it waits behind a staged part's rows (the heartbeat's
            /// short lock timeout would starve), and `FOR SHARE` serialises with a
            /// declaration's `FOR NO KEY UPDATE` of the row — a claim that locks first commits
            /// before the declaration moves the predecessor's rows (the newly won ones too); one
            /// that locks after sees the row dead.
            async fn check_live(pool: &Pool, mut tx: Tx, owner: u32, boot: u64) -> Result<Tx, StoreError> {
                let sql = format!(
                    "SELECT 1 FROM hosts WHERE ordinal = $1 AND boot = $2 AND state = {}{}",
                    crate::HOST_LIVE,
                    $share_lock
                );
                let row = sqlx::query(&sql).bind(i64::from(owner)).bind(i(boot)).fetch_optional(&mut *tx).await;
                match row {
                    Ok(Some(_)) => Ok(tx),
                    Ok(None) => {
                        tx.rollback().await?;
                        log::warn!(
                            "[world-store] host {owner} boot {boot} claimed super-regions, but its hosts row is not that \
                             live boot (declared dead or replaced): refused, nothing claimed"
                        );
                        Err(StoreError::Fenced { sr: None })
                    }
                    Err(e) => Err(claim_error(pool, tx, e).await),
                }
            }

            /// Claims for `owner` the super-regions of `srs` that are absent or free (owner
            /// [`crate::NO_OWNER`]), in one transaction like [`claim_super_regions`] (same
            /// `lock_timeout`, same side check, rows in key order): a new row gets epoch 1, a free
            /// one epoch + 1; a row another owner holds is left alone. Fenced first-writer-wins: the
            /// upsert's `WHERE owner = -1` is evaluated on the row's newest version (Postgres
            /// re-checks it after waiting for another claimer's row lock), so of two claimers
            /// exactly one takes a row. The fence holds the rows won (possibly none). Refused
            /// (`Fenced { sr: None }`, nothing claimed) unless `boot`'s `hosts` row is live
            /// ([`check_live`]). Callers hold the writer lock.
            pub(crate) async fn claim_free(
                pool: &Pool,
                owner: u32,
                boot: u64,
                srs: Vec<SuperRegion>,
                opened: u32,
            ) -> Result<Fence, StoreError> {
                let srs: std::collections::BTreeSet<SuperRegion> = srs.into_iter().collect();
                let mut tx = begin_claim(pool).await?;
                check_side(&mut tx, opened).await?;
                let sql = format!(
                    "INSERT INTO super_regions (sx, sz, owner, epoch) VALUES ($1, $2, $3, 1) \
                     ON CONFLICT (sx, sz) DO UPDATE SET owner = excluded.owner, epoch = super_regions.epoch + 1 \
                     WHERE super_regions.owner = {} \
                     RETURNING epoch",
                    crate::NO_OWNER
                );
                let mut epochs = BTreeMap::new();
                for (sx, sz) in srs {
                    let row = sqlx::query(&sql)
                        .bind(i64::from(sx))
                        .bind(i64::from(sz))
                        .bind(i64::from(owner))
                        .fetch_optional(&mut *tx)
                        .await;
                    let row = match row {
                        Ok(row) => row,
                        Err(e) => return Err(claim_error(pool, tx, e).await),
                    };
                    if let Some(row) = row {
                        let epoch: i64 = row.try_get(0)?;
                        epochs.insert((sx, sz), u(epoch));
                    }
                }
                let tx = check_live(pool, tx, owner, boot).await?;
                tx.commit().await?;
                Ok(Fence { owner, epochs, all_hosts: false })
            }

            /// Frees every super-region of `fence` (owner [`crate::NO_OWNER`], epoch + 1, so
            /// `fence` and anything claimed before it is fenced) in one transaction: first
            /// [`check_fence`] over the fence's own rows (refused with `Fenced` unless they are
            /// all still `fence`'s), then one update per row in key order. Only the fence's rows
            /// are freed: a release never skips the fence. `lock_timeout`
            /// [`crate::CLAIM_LOCK_TIMEOUT_MS`] like a claim. Callers hold the writer lock.
            pub(crate) async fn release(pool: &Pool, fence: Fence) -> Result<(), StoreError> {
                let srs: std::collections::BTreeSet<SuperRegion> = fence.epochs.keys().copied().collect();
                let mut tx = begin_claim(pool).await?;
                // `FOR SHARE` never waits for a staged part (it holds its rows `FOR SHARE` too);
                // the updates below do.
                check_fence(&mut *tx, &fence, &srs).await?;
                for (sx, sz) in srs {
                    let done = sqlx::query(&format!(
                        "UPDATE super_regions SET owner = {}, epoch = epoch + 1 WHERE sx = $1 AND sz = $2 AND owner = $3",
                        crate::NO_OWNER
                    ))
                    .bind(i64::from(sx))
                    .bind(i64::from(sz))
                    .bind(i64::from(fence.owner))
                    .execute(&mut *tx)
                    .await;
                    match done {
                        // Held `FOR SHARE` since the check (Postgres), under the writer lock (SQLite).
                        Ok(d) if d.rows_affected() == 1 => {}
                        Ok(_) => return Err(StoreError::Fenced { sr: Some((sx, sz)) }),
                        Err(e) => return Err(claim_error(pool, tx, e).await),
                    }
                }
                tx.commit().await?;
                Ok(())
            }

            /// Locks (Postgres: `FOR SHARE`) and checks every super-region of
            /// `srs` in `tx`: each must be claimed in `fence` and stored with its
            /// owner and epoch. The first that is not → `Fenced`.
            async fn check_fence(
                conn: &mut Conn,
                fence: &Fence,
                srs: &std::collections::BTreeSet<SuperRegion>,
            ) -> Result<(), StoreError> {
                let srs: Vec<SuperRegion> = srs.iter().copied().collect();
                for batch in srs.chunks(FENCE_CHECK_BATCH) {
                    let sql = fence_check_sql(batch.len(), $share_lock);
                    let mut q = sqlx::query(&sql);
                    for (sx, sz) in batch {
                        q = q.bind(i64::from(*sx)).bind(i64::from(*sz));
                    }
                    let rows = q.fetch_all(&mut *conn).await.map_err(|e| fence_race(e, batch[0]))?;
                    let mut stored = BTreeMap::new();
                    for r in rows {
                        let (sx, sz): (i64, i64) = (r.try_get(0)?, r.try_get(1)?);
                        let (owner, epoch): (i64, i64) = (r.try_get(2)?, r.try_get(3)?);
                        stored.insert((sx, sz), (owner, u(epoch)));
                    }
                    for &(sx, sz) in batch {
                        let held = fence.epochs.get(&(sx, sz)).copied();
                        let row = stored.get(&(i64::from(sx), i64::from(sz))).copied();
                        let ok = matches!((held, row), (Some(e), Some((o, se))) if o == i64::from(fence.owner) && se == e);
                        if !ok {
                            return Err(StoreError::Fenced { sr: Some((sx, sz)) });
                        }
                    }
                }
                Ok(())
            }

            /// The database clock in ms since the epoch (the one expression per engine).
            const NOW_MS: &str = $now_ms;

            /// Registers a host boot (R3, R4): in one transaction (Postgres: `lock_timeout`
            /// [`crate::CLAIM_LOCK_TIMEOUT_MS`] — it waits for what a claim waits for), with the fence
            /// checked over the fence's own super-regions first and then the world row locked
            /// (Postgres: `FOR NO KEY UPDATE`; SQLite has one writer) — the lock order of `commit_cut`.
            /// The ordinal's prior row decides, if it is dirty and holds the world's generation (its
            /// boot died without a clean save): `Rollback` → `world.generation` goes up (`bumped`);
            /// `Succession` → marked dead: `Wait(Succession)`, nothing written; live while another
            /// dirty live row beat within [`game::HOST_LEASE_MS`] (database clock): the prior row's
            /// `seen_ms = 0` (vouched), `Wait(Vouched)`; nobody alive: bumped as `Rollback`. Otherwise
            /// (no prior row, a clean one, an older generation) it registers at the world's
            /// generation. Registering upserts the row with the new boot, the generation, `clean = 0`,
            /// `state` live, `seen_ms` now and `reg` one more than the highest of any row (safe: the
            /// world-row lock serialises registrations). `row.generation`, `row.clean`, `row.state`,
            /// `row.seen_ms` and `row.reg` are ignored. Callers hold the writer lock.
            pub(crate) async fn register_host(
                pool: &Pool,
                row: HostRow,
                fence: &Fence,
                policy: game::CrashPolicy,
                lease_ms: u64,
            ) -> Result<crate::BootVerdict, StoreError> {
                let mut tx = pool.begin().await?;
                cut_lock_timeout(&mut *tx, crate::CLAIM_LOCK_TIMEOUT_MS).await?;
                let srs: std::collections::BTreeSet<SuperRegion> = fence.epochs.keys().copied().collect();
                check_fence(&mut *tx, fence, &srs).await?;
                // Postgres `FOR NO KEY UPDATE` (as `decide_cut`): serialises the generation
                // with a decision and another boot, never waits for a claim's `FOR KEY SHARE`.
                let no_key = if $update_lock.is_empty() { "" } else { " FOR NO KEY UPDATE" };
                let sql = format!("SELECT generation FROM world WHERE id = 1{no_key}");
                let world: i64 = sqlx::query(&sql)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?
                    .try_get(0)?;
                let prior = sqlx::query("SELECT generation, clean, state, boot FROM hosts WHERE ordinal = $1")
                    .bind(i64::from(row.ordinal))
                    .fetch_optional(&mut *tx)
                    .await?;
                let mut generation = world;
                let mut bumped = false;
                if let Some(prior) = prior {
                    let (prior_generation, prior_clean, prior_state, prior_boot): (i64, i64, i64, i64) =
                        (prior.try_get(0)?, prior.try_get(1)?, prior.try_get(2)?, prior.try_get(3)?);
                    if prior_clean == 0 && prior_generation == world {
                        if policy == game::CrashPolicy::Succession {
                            if prior_state != crate::HOST_LIVE {
                                // Declared dead: its land is being inherited. Nothing written.
                                tx.rollback().await?;
                                return Ok(crate::BootVerdict::Wait(crate::WaitFor::Succession));
                            }
                            // Alive: dirty, live, of this generation (a stale row of an older one
                            // that beat lately runs a world that is gone) and beaten within a lease.
                            let sql = format!(
                                "SELECT COUNT(*) FROM hosts WHERE ordinal <> $1 AND clean = 0 AND state = {live} \
                                 AND generation = $2 AND {NOW_MS} - seen_ms <= $3",
                                live = crate::HOST_LIVE,
                            );
                            let alive: i64 = sqlx::query(&sql)
                                .bind(i64::from(row.ordinal))
                                .bind(world)
                                .bind(i(lease_ms))
                                .fetch_one(&mut *tx)
                                .await?
                                .try_get(0)?;
                            if alive > 0 {
                                // Another host runs: it inherits; this boot vouches for the death.
                                // The boot judged dead above, and only it.
                                sqlx::query("UPDATE hosts SET seen_ms = 0 WHERE ordinal = $1 AND boot = $2")
                                    .bind(i64::from(row.ordinal))
                                    .bind(prior_boot)
                                    .execute(&mut *tx)
                                    .await?;
                                tx.commit().await?;
                                return Ok(crate::BootVerdict::Wait(crate::WaitFor::Vouched));
                            }
                            // Nobody alive: a total crash restarts the world from its last cut.
                        }
                        generation = world + 1;
                        bumped = true;
                        sqlx::query("UPDATE world SET generation = $1 WHERE id = 1").bind(generation).execute(&mut *tx).await?;
                    }
                }
                let reg: i64 = sqlx::query("SELECT COALESCE(MAX(reg), 0) + 1 FROM hosts")
                    .fetch_one(&mut *tx)
                    .await?
                    .try_get(0)?;
                let sql = format!(
                    "INSERT INTO hosts (ordinal, boot, generation, clean, state, seen_ms, reg, peer_addr, peer_cert, quic, webtransport, wt_cert_hash, updated_utc) \
                     VALUES ($1, $2, $3, 0, {live}, {NOW_MS}, $4, $5, $6, $7, $8, $9, $10) \
                     ON CONFLICT (ordinal) DO UPDATE SET boot = excluded.boot, generation = excluded.generation, clean = 0, \
                     state = excluded.state, seen_ms = excluded.seen_ms, reg = excluded.reg, \
                     peer_addr = excluded.peer_addr, peer_cert = excluded.peer_cert, quic = excluded.quic, \
                     webtransport = excluded.webtransport, wt_cert_hash = excluded.wt_cert_hash, updated_utc = excluded.updated_utc",
                    live = crate::HOST_LIVE,
                );
                sqlx::query(&sql)
                .bind(i64::from(row.ordinal))
                .bind(i(row.boot))
                .bind(generation)
                .bind(reg)
                .bind(&row.peer_addr)
                .bind(&row.peer_cert)
                .bind(&row.quic)
                .bind(&row.webtransport)
                .bind(&row.wt_cert_hash)
                .bind(crate::open::utc_now())
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(crate::BootVerdict::Registered(HostBoot { generation: u(generation), bumped, reg: u(reg) }))
            }

            /// Every host row, by ordinal.
            pub(crate) async fn hosts(pool: &Pool) -> Result<Vec<HostRow>, StoreError> {
                let rows = sqlx::query(
                    "SELECT ordinal, boot, generation, clean, peer_addr, peer_cert, quic, webtransport, wt_cert_hash, state, seen_ms, reg \
                     FROM hosts ORDER BY ordinal",
                )
                .fetch_all(pool)
                .await?;
                rows.into_iter()
                    .map(|r| {
                        let ordinal: i64 = r.try_get(0)?;
                        Ok(HostRow {
                            ordinal: u32::try_from(ordinal)
                                .map_err(|_| StoreError::Corrupt(format!("hosts.ordinal {ordinal}")))?,
                            boot: u(r.try_get(1)?),
                            generation: u(r.try_get(2)?),
                            clean: r.try_get::<i64, _>(3)? != 0,
                            peer_addr: r.try_get(4)?,
                            peer_cert: r.try_get(5)?,
                            quic: r.try_get(6)?,
                            webtransport: r.try_get(7)?,
                            wt_cert_hash: r.try_get(8)?,
                            state: r.try_get(9)?,
                            seen_ms: u(r.try_get(10)?),
                            reg: u(r.try_get(11)?),
                        })
                    })
                    .collect()
            }

            /// One lease beat. UNFENCED on purpose: one statement over the caller's own `hosts` row.
            /// Postgres: a short transaction with `lock_timeout` [`crate::HEARTBEAT_LOCK_TIMEOUT_MS`]
            /// — if a registration or a decision holds the row, the beat FAILS (`Err`: "no beat this
            /// time"), it neither hangs nor answers `false`. It touches neither `world` nor
            /// `super_regions`. `false`: no live row of that boot (the caller was replaced).
            pub(crate) async fn heartbeat(pool: &Pool, ordinal: u32, boot: u64) -> Result<bool, StoreError> {
                let sql = format!(
                    "UPDATE hosts SET seen_ms = {NOW_MS} WHERE ordinal = $1 AND boot = $2 AND state = {}",
                    crate::HOST_LIVE
                );
                let mut tx = pool.begin().await?;
                cut_lock_timeout(&mut *tx, crate::HEARTBEAT_LOCK_TIMEOUT_MS).await?;
                let done = sqlx::query(&sql).bind(i64::from(ordinal)).bind(i(boot)).execute(&mut *tx).await?;
                tx.commit().await?;
                Ok(done.rows_affected() == 1)
            }

            /// Every `super_regions` row, by super-region. A plain read, no locks.
            pub(crate) async fn owners(pool: &Pool) -> Result<Vec<(SuperRegion, i64, u64)>, StoreError> {
                let rows = sqlx::query("SELECT sx, sz, owner, epoch FROM super_regions ORDER BY sx, sz")
                    .fetch_all(pool)
                    .await?;
                rows.into_iter()
                    .map(|r| {
                        let (sx, sz): (i64, i64) = (r.try_get(0)?, r.try_get(1)?);
                        let sr = (
                            i32::try_from(sx).map_err(|_| StoreError::Corrupt(format!("super_regions.sx {sx}")))?,
                            i32::try_from(sz).map_err(|_| StoreError::Corrupt(format!("super_regions.sz {sz}")))?,
                        );
                        Ok((sr, r.try_get(2)?, u(r.try_get(3)?)))
                    })
                    .collect()
            }

            /// The rows `owner` holds, as a fence. A plain read, no locks.
            pub(crate) async fn owned_by(pool: &Pool, owner: u32) -> Result<Fence, StoreError> {
                let rows = sqlx::query("SELECT sx, sz, epoch FROM super_regions WHERE owner = $1")
                    .bind(i64::from(owner))
                    .fetch_all(pool)
                    .await?;
                let mut epochs = BTreeMap::new();
                for r in rows {
                    let (sx, sz): (i64, i64) = (r.try_get(0)?, r.try_get(1)?);
                    let sr = (
                        i32::try_from(sx).map_err(|_| StoreError::Corrupt(format!("super_regions.sx {sx}")))?,
                        i32::try_from(sz).map_err(|_| StoreError::Corrupt(format!("super_regions.sz {sz}")))?,
                    );
                    epochs.insert(sr, u(r.try_get(2)?));
                }
                Ok(Fence { owner, epochs, all_hosts: false })
            }

            /// The database clock, ms since the epoch.
            pub(crate) async fn db_now_ms(pool: &Pool) -> Result<u64, StoreError> {
                let sql = format!("SELECT {NOW_MS}");
                let now: i64 = sqlx::query(&sql).fetch_one(pool).await?.try_get(0)?;
                Ok(u(now))
            }

            /// `world.generation`.
            pub(crate) async fn world_generation(pool: &Pool) -> Result<u64, StoreError> {
                let g: i64 = sqlx::query("SELECT generation FROM world WHERE id = 1")
                    .fetch_optional(pool)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?
                    .try_get(0)?;
                Ok(u(g))
            }

            /// Claims the coordinator row: epoch 1 on a new store, else epoch + 1
            /// (a coordinator that held it is fenced). Callers hold the writer lock.
            pub(crate) async fn claim_coordinator(pool: &Pool) -> Result<CoordinatorFence, StoreError> {
                let epoch: i64 = sqlx::query(
                    "INSERT INTO coordinator (id, epoch) VALUES (1, 1) \
                     ON CONFLICT (id) DO UPDATE SET epoch = coordinator.epoch + 1 RETURNING epoch",
                )
                .fetch_one(pool)
                .await?
                .try_get(0)?;
                Ok(CoordinatorFence { epoch: u(epoch) })
            }

            /// Writes the coordinator's state if `fence` still holds its row (the
            /// check is the update's own `WHERE`). Callers hold the writer lock.
            pub(crate) async fn save_coordinator(
                pool: &Pool,
                state: Vec<u8>,
                fence: CoordinatorFence,
            ) -> Result<(), StoreError> {
                let done = sqlx::query("UPDATE coordinator SET state = $1, saved_utc = $2 WHERE id = 1 AND epoch = $3")
                    .bind(state)
                    .bind(crate::open::utc_now())
                    .bind(i(fence.epoch))
                    .execute(pool)
                    .await?;
                if done.rows_affected() == 0 {
                    return Err(StoreError::Fenced { sr: None });
                }
                Ok(())
            }

            pub(crate) async fn load_coordinator(pool: &Pool) -> Result<Option<Vec<u8>>, StoreError> {
                let row = sqlx::query("SELECT state FROM coordinator WHERE id = 1").fetch_optional(pool).await?;
                Ok(match row {
                    Some(r) => r.try_get(0)?,
                    None => None,
                })
            }

            pub(crate) async fn forged_archive(pool: &Pool) -> Result<Option<Vec<u8>>, StoreError> {
                let row = sqlx::query("SELECT bytes FROM forged_archive WHERE id = 1").fetch_optional(pool).await?;
                Ok(match row {
                    Some(r) => Some(r.try_get(0)?),
                    None => None,
                })
            }

            /// The tiled forged world, all of it in one transaction: the meta, every
            /// tile (the old ones first deleted), the world map, the archive and
            /// `world.content_crc`. Callers hold the writer lock.
            pub(crate) async fn put_tiled(pool: &Pool, rows: Box<TiledRows>) -> Result<(), StoreError> {
                let TiledRows { format_version, meta, tiles, world_map, archive, content_crc } = *rows;
                let mut tx = pool.begin().await?;
                sqlx::query(
                    "INSERT INTO forged_meta (id, format_version, bytes) VALUES (1, $1, $2) \
                     ON CONFLICT (id) DO UPDATE SET format_version = excluded.format_version, bytes = excluded.bytes",
                )
                .bind(i64::from(format_version))
                .bind(meta)
                .execute(&mut *tx)
                .await?;
                sqlx::query("DELETE FROM forged_tiles").execute(&mut *tx).await?;
                for (t, bytes) in tiles {
                    sqlx::query("INSERT INTO forged_tiles (tx, tz, bytes) VALUES ($1, $2, $3)")
                        .bind(i64::from(t.tx))
                        .bind(i64::from(t.tz))
                        .bind(bytes)
                        .execute(&mut *tx)
                        .await?;
                }
                sqlx::query(
                    "INSERT INTO forged_world_map (id, bytes) VALUES (1, $1) \
                     ON CONFLICT (id) DO UPDATE SET bytes = excluded.bytes",
                )
                .bind(world_map)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "INSERT INTO forged_archive (id, bytes) VALUES (1, $1) \
                     ON CONFLICT (id) DO UPDATE SET bytes = excluded.bytes",
                )
                .bind(archive)
                .execute(&mut *tx)
                .await?;
                sqlx::query("UPDATE world SET content_crc = $1 WHERE id = 1")
                    .bind(i64::from(content_crc))
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                Ok(())
            }

            /// The tiled world's meta row with the world's content crc and whether
            /// the archive row exists (one read transaction), or `None`.
            pub(crate) async fn forged_head(pool: &Pool) -> Result<Option<ForgedHead>, StoreError> {
                let mut tx = begin_read(pool).await?;
                let row = sqlx::query("SELECT format_version, bytes FROM forged_meta WHERE id = 1")
                    .fetch_optional(&mut *tx)
                    .await?;
                let Some(row) = row else {
                    tx.commit().await?;
                    return Ok(None);
                };
                let format_version: i64 = row.try_get(0)?;
                let meta: Vec<u8> = row.try_get(1)?;
                let crc: i64 = sqlx::query("SELECT content_crc FROM world WHERE id = 1")
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?
                    .try_get(0)?;
                let archived: i64 = sqlx::query("SELECT COUNT(*) FROM forged_archive")
                    .fetch_one(&mut *tx)
                    .await?
                    .try_get(0)?;
                tx.commit().await?;
                let content_crc =
                    u32::try_from(crc).map_err(|_| StoreError::Corrupt(format!("content_crc {crc}")))?;
                let format_version = u32::try_from(format_version)
                    .map_err(|_| StoreError::Corrupt(format!("forged_meta.format_version {format_version}")))?;
                Ok(Some(ForgedHead { format_version, meta, content_crc, archived: archived > 0 }))
            }

            pub(crate) async fn forged_tile(
                pool: &Pool,
                t: worldgen::forged_tiles::TileCoords,
            ) -> Result<Option<Vec<u8>>, StoreError> {
                let row = sqlx::query("SELECT bytes FROM forged_tiles WHERE tx = $1 AND tz = $2")
                    .bind(i64::from(t.tx))
                    .bind(i64::from(t.tz))
                    .fetch_optional(pool)
                    .await?;
                Ok(match row {
                    Some(r) => Some(r.try_get(0)?),
                    None => None,
                })
            }

            pub(crate) async fn forged_world_map(pool: &Pool) -> Result<Option<Vec<u8>>, StoreError> {
                let row = sqlx::query("SELECT bytes FROM forged_world_map WHERE id = 1").fetch_optional(pool).await?;
                Ok(match row {
                    Some(r) => Some(r.try_get(0)?),
                    None => None,
                })
            }

            async fn load_chunks(tx: &mut Tx, rc: RegionCoords) -> Result<Vec<(ChunkCoords, Chunk)>, StoreError> {
                let rows = sqlx::query("SELECT cx, cy, cz, data FROM chunks WHERE rx = $1 AND rz = $2")
                    .bind(rc.x as i64)
                    .bind(rc.z as i64)
                    .fetch_all(&mut **tx)
                    .await?;
                let mut chunks = Vec::with_capacity(rows.len());
                for r in rows {
                    let coords = chunk_of(rc, r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)?;
                    let data: Vec<u8> = r.try_get(3)?;
                    let chunk: Chunk = decode(FORMAT_CHUNK, &data)
                        .map_err(|e| StoreError::Corrupt(format!("region {rc:?} chunk {coords:?}: {e}")))?;
                    chunks.push((coords, chunk));
                }
                chunks.sort_by_key(|(c, _)| *c);
                Ok(chunks)
            }

            /// The `regions` row's `(meta, ground)` blobs, if the region was ever saved.
            async fn region_row(tx: &mut Tx, rc: RegionCoords) -> Result<Option<(Vec<u8>, Vec<u8>)>, StoreError> {
                let row = sqlx::query("SELECT meta, ground FROM regions WHERE rx = $1 AND rz = $2")
                    .bind(rc.x as i64)
                    .bind(rc.z as i64)
                    .fetch_optional(&mut **tx)
                    .await?;
                Ok(match row {
                    Some(r) => Some((r.try_get(0)?, r.try_get(1)?)),
                    None => None,
                })
            }

            fn decode_ground(rc: RegionCoords, blob: &[u8]) -> Result<Vec<(ColumnCoords, ColumnGround)>, StoreError> {
                decode(FORMAT_REGION_GROUND, blob)
                    .map_err(|e| StoreError::Corrupt(format!("region {rc:?} ground: {e}")))
            }

            /// Stores `proposed` as the world's clock anchor if it has none (the
            /// update's own `WHERE`: first writer wins), then returns the stored one.
            /// Callers hold the writer lock.
            pub(crate) async fn ensure_clock_anchor(pool: &Pool, proposed: ClockAnchor) -> Result<ClockAnchor, StoreError> {
                sqlx::query(
                    "UPDATE world SET clock_anchor_unix_ms = $1, clock_anchor_tick = $2 \
                     WHERE id = 1 AND clock_anchor_unix_ms IS NULL",
                )
                .bind(proposed.unix_ms as i64)
                .bind(proposed.tick as i64)
                .execute(pool)
                .await?;
                let row = sqlx::query("SELECT clock_anchor_unix_ms, clock_anchor_tick FROM world WHERE id = 1")
                    .fetch_optional(pool)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?;
                let unix_ms: Option<i64> = row.try_get(0)?;
                let tick: Option<i64> = row.try_get(1)?;
                match (unix_ms, tick) {
                    (Some(unix_ms), Some(tick)) => Ok(ClockAnchor { unix_ms: u(unix_ms), tick: u(tick) }),
                    _ => Err(StoreError::Corrupt("world.clock_anchor is unset after it was written".into())),
                }
            }

            /// Sets `world.super_region_regions` to `s` if no super-region is claimed yet;
            /// returns whether it did. One transaction: it locks the world row (Postgres:
            /// `FOR UPDATE`; a claim takes it `FOR KEY SHARE` first, so the two serialise in the
            /// same order and never deadlock), then `super_regions` against inserts (Postgres:
            /// `SHARE ROW EXCLUSIVE`), checks that table is empty and writes. A claim committed
            /// first makes this refuse; a claim after it sees the new side and refuses
            /// ([`claim_super_regions`]). SQLite: the process's file lock already excludes
            /// every other writer. Callers hold the writer lock.
            pub(crate) async fn set_super_region_regions(pool: &Pool, s: u32) -> Result<bool, StoreError> {
                // The side divides region coordinates (`super_region_of`, the stale-row
                // guard's SQL); [`super_region_regions`] refuses a stored 0 at open too.
                if s == 0 {
                    return Err(StoreError::Unsupported("a super-region side of 0 regions (it must be >= 1)".into()));
                }
                let mut tx = pool.begin().await?;
                let sql = format!("SELECT super_region_regions FROM world WHERE id = 1{}", $update_lock);
                sqlx::query(&sql)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?;
                let lock: Option<&str> = $claims_lock;
                if let Some(lock) = lock {
                    sqlx::query(lock).execute(&mut *tx).await?;
                }
                let claimed: i64 = sqlx::query("SELECT COUNT(*) FROM super_regions").fetch_one(&mut *tx).await?.try_get(0)?;
                if claimed > 0 {
                    tx.rollback().await?;
                    return Ok(false);
                }
                sqlx::query("UPDATE world SET super_region_regions = $1 WHERE id = 1")
                    .bind(i64::from(s))
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                Ok(true)
            }

            /// Forgets the world's clock anchor. Callers hold the writer lock.
            pub(crate) async fn clear_clock_anchor(pool: &Pool) -> Result<(), StoreError> {
                sqlx::query("UPDATE world SET clock_anchor_unix_ms = NULL, clock_anchor_tick = NULL WHERE id = 1")
                    .execute(pool)
                    .await?;
                Ok(())
            }

            /// The region as last committed, if `fence` still holds its
            /// super-region (checked first, in the same read transaction).
            pub(crate) async fn load_region(
                pool: &Pool,
                rc: RegionCoords,
                fence: &Fence,
                s: u32,
            ) -> Result<Option<StoredRegion>, StoreError> {
                let mut tx = begin_read(pool).await?;
                check_fence(&mut *tx, fence, &[load_check(rc, s)].into()).await?;
                let Some((meta, ground)) = region_row(&mut tx, rc).await? else {
                    tx.commit().await?;
                    return Ok(None);
                };
                let meta: RegionMeta = decode(FORMAT_REGION_META, &meta)
                    .map_err(|e| StoreError::Corrupt(format!("region {rc:?} meta: {e}")))?;
                let ground = decode_ground(rc, &ground)?;
                let chunks = load_chunks(&mut tx, rc).await?;
                let rows = sqlx::query("SELECT bundle FROM entities WHERE rx = $1 AND rz = $2")
                    .bind(rc.x as i64)
                    .bind(rc.z as i64)
                    .fetch_all(&mut *tx)
                    .await?;
                tx.commit().await?;
                let mut entities = Vec::with_capacity(rows.len());
                for r in rows {
                    let blob: Vec<u8> = r.try_get(0)?;
                    let bundle: EntityBundle = decode(FORMAT_ENTITY_BUNDLE, &blob)
                        .map_err(|e| StoreError::Corrupt(format!("region {rc:?} entity: {e}")))?;
                    entities.push(bundle);
                }
                entities.sort_by_key(|b| b.id);
                Ok(Some(StoredRegion { meta, chunks, ground, entities }))
            }

            pub(crate) async fn load_terrain(pool: &Pool, rc: RegionCoords) -> Result<Option<TerrainSnapshot>, StoreError> {
                let mut tx = begin_read(pool).await?;
                let Some((_, ground)) = region_row(&mut tx, rc).await? else {
                    tx.commit().await?;
                    return Ok(None);
                };
                let ground = decode_ground(rc, &ground)?;
                let chunks = load_chunks(&mut tx, rc).await?;
                tx.commit().await?;
                Ok(Some(TerrainSnapshot { chunks, ground }))
            }

            pub(crate) async fn stored_regions(pool: &Pool) -> Result<BTreeMap<RegionCoords, u64>, StoreError> {
                let rows = sqlx::query("SELECT rx, rz, terrain_version FROM regions").fetch_all(pool).await?;
                let mut out = BTreeMap::new();
                for r in rows {
                    let version: i64 = r.try_get(2)?;
                    out.insert(region_of(r.try_get(0)?, r.try_get(1)?)?, u(version));
                }
                Ok(out)
            }

            /// The regions saved at `tick` or later (`saved_tick >= tick`: what a world cut
            /// at `tick` and every later one wrote), with their terrain versions.
            pub(crate) async fn stored_regions_since(pool: &Pool, tick: Tick) -> Result<BTreeMap<RegionCoords, u64>, StoreError> {
                let rows = sqlx::query("SELECT rx, rz, terrain_version FROM regions WHERE saved_tick >= $1")
                    .bind(i(tick))
                    .fetch_all(pool)
                    .await?;
                let mut out = BTreeMap::new();
                for r in rows {
                    let version: i64 = r.try_get(2)?;
                    out.insert(region_of(r.try_get(0)?, r.try_get(1)?)?, u(version));
                }
                Ok(out)
            }

            /// The in-flight rows of host `host` (`None`: of every host), in order.
            pub(crate) async fn load_in_flight(pool: &Pool, host: Option<u32>) -> Result<Vec<InFlightRow>, StoreError> {
                let mut tx = begin_read(pool).await?;
                let rows = in_flight_rows(&mut tx, host).await?;
                tx.commit().await?;
                Ok(rows)
            }

            /// The in-flight rows of host `host` for its heir `fence.owner` (R4): refused with
            /// `Unsupported` unless an open succession makes `fence.owner` the heir of `host`
            /// (checked in the same read transaction).
            pub(crate) async fn load_in_flight_of(
                pool: &Pool,
                host: u32,
                fence: &Fence,
            ) -> Result<Vec<InFlightRow>, StoreError> {
                let mut tx = begin_read(pool).await?;
                check_adopts(&mut *tx, fence.owner, &[host]).await?;
                let rows = in_flight_rows(&mut tx, Some(host)).await?;
                tx.commit().await?;
                Ok(rows)
            }

            /// Refuses (`Unsupported`) unless, for each of `adopts`, an open succession names
            /// `heir` its heir. A plain read (no row lock): a staged part holding a lock on
            /// `successions` would block the decision that settles it.
            async fn check_adopts(conn: &mut Conn, heir: u32, adopts: &[u32]) -> Result<(), StoreError> {
                for predecessor in adopts {
                    let open: i64 = sqlx::query(
                        "SELECT COUNT(*) FROM successions WHERE predecessor = $1 AND heir = $2 \
                         AND settled_cut IS NULL AND abandoned = 0",
                    )
                    .bind(i64::from(*predecessor))
                    .bind(i64::from(heir))
                    .fetch_one(&mut *conn)
                    .await?
                    .try_get(0)?;
                    if open == 0 {
                        return Err(StoreError::Unsupported(format!(
                            "host {heir} adopts host {predecessor}'s in-flight rows, but no open succession makes \
                             it host {predecessor}'s heir"
                        )));
                    }
                }
                Ok(())
            }

            /// Every open succession (not settled, not abandoned), by id.
            pub(crate) async fn open_successions(pool: &Pool) -> Result<Vec<crate::Succession>, StoreError> {
                let rows = sqlx::query(
                    "SELECT id, predecessor, predecessor_boot, heir, kind, base_cut, survivors, moved FROM successions \
                     WHERE settled_cut IS NULL AND abandoned = 0 ORDER BY id",
                )
                .fetch_all(pool)
                .await?;
                rows.into_iter()
                    .map(|r| {
                        let id = u(r.try_get(0)?);
                        let ordinal = |v: i64| {
                            u32::try_from(v).map_err(|_| StoreError::Corrupt(format!("successions {id}: ordinal {v}")))
                        };
                        let kind: i64 = r.try_get(4)?;
                        let survivors: String = r.try_get(6)?;
                        let moved: String = r.try_get(7)?;
                        Ok(crate::Succession {
                            id: crate::SuccessionId(id),
                            predecessor: ordinal(r.try_get(1)?)?,
                            predecessor_boot: u(r.try_get(2)?),
                            heir: ordinal(r.try_get(3)?)?,
                            kind: crate::SuccessionKind::from_code(kind)
                                .ok_or_else(|| StoreError::Corrupt(format!("successions {id}: kind {kind}")))?,
                            base_cut: CutId(u(r.try_get(5)?)),
                            survivors: crate::succession::decode_survivors(id, &survivors)?,
                            moved: crate::succession::decode_moved(id, &moved)?,
                        })
                    })
                    .collect()
            }

            /// The region of every stored row of each of `ids` (through `entities_id`, migration
            /// 0010): an id stored in two regions gives two entries, an unknown one none. In the
            /// order of `ids`, then by region.
            pub(crate) async fn regions_of_entities(
                pool: &Pool,
                ids: Vec<EntityId>,
            ) -> Result<Vec<(EntityId, RegionCoords)>, StoreError> {
                let mut tx = begin_read(pool).await?;
                let mut out = Vec::new();
                for id in ids {
                    let rows = sqlx::query(
                        "SELECT rx, rz FROM entities WHERE birth_rx = $1 AND birth_rz = $2 AND serial = $3 ORDER BY rx, rz",
                    )
                    .bind(id.birth_region.x as i64)
                    .bind(id.birth_region.z as i64)
                    .bind(i(id.serial))
                    .fetch_all(&mut *tx)
                    .await?;
                    for r in rows {
                        out.push((id, region_of(r.try_get(0)?, r.try_get(1)?)?));
                    }
                }
                tx.commit().await?;
                Ok(out)
            }

            async fn in_flight_rows(tx: &mut Tx, host: Option<u32>) -> Result<Vec<InFlightRow>, StoreError> {
                let rows = sqlx::query(
                    "SELECT seq, kind, target_rx, target_rz, stamp, payload FROM in_flight \
                     WHERE $1 IS NULL OR host = $1 ORDER BY seq",
                )
                .bind(host.map(i64::from))
                .fetch_all(&mut **tx)
                .await?;
                let mut out = Vec::with_capacity(rows.len());
                for r in rows {
                    let seq: i64 = r.try_get(0)?;
                    let kind: i64 = r.try_get(1)?;
                    let target = region_of(r.try_get(2)?, r.try_get(3)?)?;
                    let stamp: i64 = r.try_get(4)?;
                    let payload: Vec<u8> = r.try_get(5)?;
                    let bad = |e: StoreError| StoreError::Corrupt(format!("in_flight row {seq}: {e}"));
                    out.push(match kind {
                        IN_FLIGHT_TRANSFER => InFlightRow::Transfer {
                            target,
                            bundle: decode(FORMAT_ENTITY_BUNDLE, &payload).map_err(bad)?,
                            stamp: u(stamp),
                        },
                        IN_FLIGHT_INTENT => InFlightRow::Intent {
                            target,
                            msg: decode::<IntentMsg>(FORMAT_INTENT_MSG, &payload).map_err(bad)?,
                        },
                        k => return Err(StoreError::Corrupt(format!("in_flight row {seq}: unknown kind {k}"))),
                    });
                }
                Ok(out)
            }

            pub(crate) async fn load_history(pool: &Pool) -> Result<Option<Vec<u8>>, StoreError> {
                let row = sqlx::query("SELECT bytes FROM history WHERE id = 1").fetch_optional(pool).await?;
                Ok(match row {
                    Some(r) => Some(r.try_get(0)?),
                    None => None,
                })
            }

            /// The token's id and record if the token is known; reads only, so it
            /// needs no writer lock (a joining client does not wait behind a commit).
            pub(crate) async fn find_player(
                pool: &Pool,
                token: &str,
            ) -> Result<Option<(PlayerId, Option<PlayerRecord>)>, StoreError> {
                let row = sqlx::query("SELECT player_id, record FROM players WHERE token = $1")
                    .bind(token)
                    .fetch_optional(pool)
                    .await?;
                let Some(r) = row else { return Ok(None) };
                let id = PlayerId(u(r.try_get(0)?));
                let record: Option<Vec<u8>> = r.try_get(1)?;
                let record = match record {
                    Some(blob) => Some(
                        decode::<PlayerRecord>(FORMAT_PLAYER_RECORD, &blob)
                            .map_err(|e| StoreError::Corrupt(format!("player {}: {e}", id.0)))?,
                    ),
                    None => None,
                };
                Ok(Some((id, record)))
            }

            /// The token's id and record; a new token gets an id from the database
            /// (identity column / rowid), written at once. Safe across processes and
            /// threads: the insert is a no-op for a known token, so concurrent first
            /// resolves of one token agree.
            pub(crate) async fn resolve_player(
                pool: &Pool,
                token: &str,
            ) -> Result<(PlayerId, Option<PlayerRecord>), StoreError> {
                sqlx::query("INSERT INTO players (token) VALUES ($1) ON CONFLICT (token) DO NOTHING")
                    .bind(token)
                    .execute(pool)
                    .await?;
                find_player(pool, token)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt(format!("player token {token:?} vanished after its insert")))
            }

            /// The stored decision on `cut` (`cut_decisions`), if any.
            pub(crate) async fn cut_decision<'c>(
                e: impl sqlx::Executor<'c, Database = $db>,
                cut: CutId,
            ) -> Result<Option<bool>, StoreError> {
                let d: Option<i64> = sqlx::query_scalar("SELECT committed FROM cut_decisions WHERE cut_id = $1")
                    .bind(i(cut.0))
                    .fetch_optional(e)
                    .await?;
                Ok(d.map(|d| d != 0))
            }

            /// The newest committed world cut (`cut_decisions`), `CutId(0)` before any: where
            /// a host's peer journal starts at boot. A plain read.
            pub(crate) async fn last_committed_cut(pool: &Pool) -> Result<CutId, StoreError> {
                let c: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cut_id), 0) FROM cut_decisions WHERE committed = 1")
                    .fetch_one(pool)
                    .await?;
                Ok(CutId(u(c)))
            }

            /// The whole cut in one transaction, ended as `end` says
            /// ([`CutEnd`]); any error rolls all of it back. On Postgres the transaction
            /// waits at most `lock_timeout_ms` for any row lock. Callers hold the writer lock.
            pub(crate) async fn run_cut(
                pool: &Pool,
                cut: &WorldCut,
                fence: &Fence,
                s: u32,
                end: &CutEnd,
                lock_timeout_ms: u64,
            ) -> Result<CutCommitted, StoreError> {
                match end {
                    CutEnd::Commit => {
                        let mut tx = pool.begin().await?;
                        cut_lock_timeout(&mut *tx, lock_timeout_ms).await?;
                        let done = cut_rows(&mut *tx, cut, fence, s, end).await?;
                        tx.commit().await?;
                        Ok(done)
                    }
                    CutEnd::Prepare { gid, generation } => {
                        // A raw transaction on one connection: a `sqlx::Transaction` would
                        // send a ROLLBACK when dropped, after the PREPARE. Every error
                        // before the PREPARE rolls back here, on the same connection; a
                        // panic or cancellation in between closes the connection instead
                        // of handing it back inside the transaction.
                        let prepare = crate::two_phase::prepare_sql(gid);
                        let mut conn = OpenTx { conn: Some(pool.acquire().await?), open: true };
                        let c = conn.conn.as_mut().expect("set above");
                        sqlx::Executor::execute(&mut **c, "BEGIN").await?;
                        let staged = prepare_rows(&mut **c, cut, fence, s, end, *generation, &prepare, lock_timeout_ms).await;
                        if staged.is_ok() {
                            conn.open = false;
                        } else if let Err(e) = sqlx::Executor::execute(&mut **c, "ROLLBACK").await {
                            // Never hand a connection with an open transaction back.
                            log::error!("[world-store] rollback of a failed prepare: {e}; closing the connection");
                        } else {
                            conn.open = false;
                        }
                        staged
                    }
                }
            }

            /// A pooled connection with a raw transaction that may still be open:
            /// dropped while `open`, it is closed, never returned to the pool.
            struct OpenTx {
                conn: Option<sqlx::pool::PoolConnection<$db>>,
                open: bool,
            }

            impl Drop for OpenTx {
                fn drop(&mut self) {
                    if self.open {
                        if let Some(c) = self.conn.as_mut() {
                            c.close_on_drop();
                        }
                    }
                }
            }

            /// The first statement of a cut's transaction (Postgres): no row lock
            /// is waited for longer than `ms`.
            async fn cut_lock_timeout(conn: &mut Conn, ms: u64) -> Result<(), StoreError> {
                if $lock_timeout {
                    sqlx::Executor::execute(&mut *conn, format!("SET LOCAL lock_timeout = '{ms}ms'").as_str()).await?;
                }
                Ok(())
            }

            /// A prepared part's statements after its `BEGIN`: the generation check,
            /// the cut's rows and the `PREPARE TRANSACTION`.
            #[allow(clippy::too_many_arguments)]
            async fn prepare_rows(
                conn: &mut Conn,
                cut: &WorldCut,
                fence: &Fence,
                s: u32,
                end: &CutEnd,
                generation: u64,
                prepare: &str,
                lock_timeout_ms: u64,
            ) -> Result<CutCommitted, StoreError> {
                cut_lock_timeout(&mut *conn, lock_timeout_ms).await?;
                // No row lock: a prepared part holding one on the world row would
                // block the decision's world tick update. The decision re-checks the
                // generation under `FOR NO KEY UPDATE`.
                let stored: i64 = sqlx::query("SELECT generation FROM world WHERE id = 1")
                    .fetch_optional(&mut *conn)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?
                    .try_get(0)?;
                if u(stored) != generation {
                    return Err(StoreError::StaleGeneration { stored: u(stored), mine: generation });
                }
                let done = cut_rows(&mut *conn, cut, fence, s, end).await?;
                sqlx::Executor::execute(&mut *conn, prepare).await?;
                Ok(done)
            }

            /// Refuses the cut if an entity it saved still has a row in a region the cut
            /// cannot be deleting (that row is visible here: committed, or this
            /// transaction's): one phase — any other region; a prepared part — another
            /// region OWNED BY THIS HOST (the claim rows of `fence.owner`, read in this
            /// transaction): a row in a peer's region may be deleted by that peer's part of
            /// the same cut and is judged after the decision instead
            /// (`two_phase::report_entities_twice`). One query per batch of regions, the
            /// other row found through `entities_id` (migration 0010).
            async fn check_stale_rows(
                conn: &mut Conn,
                cut: &WorldCut,
                fence: &Fence,
                s: u32,
                end: &CutEnd,
            ) -> Result<(), StoreError> {
                let owned = matches!(end, CutEnd::Prepare { .. });
                let regions: Vec<RegionCoords> = cut.regions.iter().filter(|(_, r)| !r.entities.is_empty()).map(|(rc, _)| *rc).collect();
                for batch in regions.chunks(FENCE_CHECK_BATCH) {
                    let sql = stale_row_sql(batch.len(), owned);
                    let mut q = sqlx::query(&sql);
                    for rc in batch {
                        q = q.bind(rc.x as i64).bind(rc.z as i64);
                    }
                    if owned {
                        q = q.bind(i64::from(fence.owner)).bind(i64::from(s));
                    }
                    if let Some(r) = q.fetch_optional(&mut *conn).await? {
                        let id = EntityId {
                            birth_region: region_of(r.try_get(0)?, r.try_get(1)?)?,
                            serial: u(r.try_get(2)?),
                        };
                        let saved = region_of(r.try_get(3)?, r.try_get(4)?)?;
                        let stored = region_of(r.try_get(5)?, r.try_get(6)?)?;
                        return Err(entity_stale(id, saved, stored, cut.tick));
                    }
                }
                Ok(())
            }

            /// The rows of a cut, inside the caller's transaction. First the fence:
            /// every super-region the cut touches and every one `fence` claimed must
            /// still be held ([`commit_check_set`]). The world tick is written only
            /// by a one-phase commit (a prepared part leaves it to the decision).
            async fn cut_rows(
                conn: &mut Conn,
                cut: &WorldCut,
                fence: &Fence,
                s: u32,
                end: &CutEnd,
            ) -> Result<CutCommitted, StoreError> {
                let t = i(cut.tick);
                check_fence(conn, fence, &commit_check_set(cut, fence, s)).await?;
                if !cut.adopts.is_empty() {
                    // An heir writes as itself: its cut, its fence (an all-in-one fence runs every host).
                    if !fence.all_hosts && cut.host != fence.owner {
                        return Err(StoreError::Unsupported(format!(
                            "host {}'s cut adopts host rows under host {}'s fence",
                            cut.host, fence.owner
                        )));
                    }
                    check_adopts(&mut *conn, cut.host, &cut.adopts).await?;
                }
                check_entities_once(cut)?;
                // Every region's entities go first. Only this cut's (this part's) own
                // regions' rows are deleted and inserted, by region: the rows are keyed
                // per region, so another host's part of the same cut never touches one
                // of them (an entity that crossed between the two hosts' regions is
                // deleted from one region's rows and inserted into the other's).
                for (rc, _) in &cut.regions {
                    sqlx::query("DELETE FROM entities WHERE rx = $1 AND rz = $2")
                        .bind(rc.x as i64)
                        .bind(rc.z as i64)
                        .execute(&mut *conn)
                        .await?;
                }
                let mut versions = Vec::with_capacity(cut.regions.len());
                for (rc, region) in &cut.regions {
                    let (rx, rz) = (rc.x as i64, rc.z as i64);
                    if region.ground.is_none() {
                        // `None` = "unchanged since the stored ground": a region's first
                        // row must carry its ground, or it would be stored empty.
                        let known = sqlx::query("SELECT 1 FROM regions WHERE rx = $1 AND rz = $2")
                            .bind(rx)
                            .bind(rz)
                            .fetch_optional(&mut *conn)
                            .await?;
                        if known.is_none() {
                            return Err(StoreError::Corrupt(format!(
                                "region {rc:?}'s first save carries no ground (the store would hold it empty); \
                                 the cut was not committed"
                            )));
                        }
                    }
                    let bump = i64::from(!region.chunks.is_empty());
                    let version: i64 = sqlx::query(
                        "INSERT INTO regions (rx, rz, saved_tick, terrain_version, meta, ground) \
                         VALUES ($1, $2, $3, $4, $5, $6) \
                         ON CONFLICT (rx, rz) DO UPDATE SET \
                           saved_tick = excluded.saved_tick, \
                           terrain_version = regions.terrain_version + excluded.terrain_version, \
                           meta = excluded.meta, \
                           ground = CASE WHEN $7 = 1 THEN excluded.ground ELSE regions.ground END \
                         RETURNING terrain_version",
                    )
                    .bind(rx)
                    .bind(rz)
                    .bind(t)
                    .bind(bump)
                    .bind(encode(FORMAT_REGION_META, &region.meta))
                    .bind(ground_blob(region.ground.as_ref()))
                    .bind(i64::from(region.ground.is_some()))
                    .fetch_one(&mut *conn)
                    .await?
                    .try_get(0)?;
                    versions.push((*rc, u(version)));
                    for (c, chunk) in &region.chunks {
                        sqlx::query(
                            "INSERT INTO chunks (rx, rz, cx, cy, cz, data) VALUES ($1, $2, $3, $4, $5, $6) \
                             ON CONFLICT (rx, rz, cx, cy, cz) DO UPDATE SET data = excluded.data",
                        )
                        .bind(rx)
                        .bind(rz)
                        .bind(c.x as i64)
                        .bind(c.y as i64)
                        .bind(c.z as i64)
                        .bind(encode(FORMAT_CHUNK, chunk))
                        .execute(&mut *conn)
                        .await?;
                    }
                    for b in &region.entities {
                        sqlx::query(
                            "INSERT INTO entities (birth_rx, birth_rz, serial, rx, rz, saved_tick, kind, bundle) \
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                        )
                        .bind(b.id.birth_region.x as i64)
                        .bind(b.id.birth_region.z as i64)
                        .bind(i(b.id.serial))
                        .bind(rx)
                        .bind(rz)
                        .bind(t)
                        .bind(kind_code(&b.kind))
                        .bind(encode(FORMAT_ENTITY_BUNDLE, b))
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| entity_insert_error(e, b.id, *rc, cut.tick))?;
                    }
                }
                check_stale_rows(&mut *conn, cut, fence, s, end).await?;
                for (id, record) in &cut.players {
                    let done = sqlx::query("UPDATE players SET record = $1, saved_tick = $2 WHERE player_id = $3")
                        .bind(encode(FORMAT_PLAYER_RECORD, record))
                        .bind(t)
                        .bind(i(id.0))
                        .execute(&mut *conn)
                        .await?;
                    if done.rows_affected() == 0 {
                        return Err(StoreError::Corrupt(format!(
                            "player {} has no token row (ids come only from resolve_player); the cut was not committed",
                            id.0
                        )));
                    }
                }
                // The rows this process adopted at start ([`Fence::in_flight_scope`]): its
                // own host's, or (all-in-one) every host's.
                sqlx::query("DELETE FROM in_flight WHERE $1 IS NULL OR host = $1")
                    .bind(fence.in_flight_scope().map(|_| cut.host as i64))
                    .execute(&mut *conn)
                    .await?;
                // An heir's part also replaces the rows of the predecessors it adopts (checked
                // above): what it kept of them is in `cut.in_flight`, now under its own host.
                for predecessor in &cut.adopts {
                    sqlx::query("DELETE FROM in_flight WHERE host = $1")
                        .bind(i64::from(*predecessor))
                        .execute(&mut *conn)
                        .await?;
                }
                // `seq` is the table's key across all hosts: each host owns a range
                // (`host << 32`), so concurrent commits never collide and need no MAX read.
                let base = (cut.host as i64) << 32;
                for (seq, row) in cut.in_flight.iter().enumerate() {
                    let (kind, target, stamp, payload) = match row {
                        InFlightRow::Transfer { target, bundle, stamp } => {
                            (IN_FLIGHT_TRANSFER, target, *stamp, encode(FORMAT_ENTITY_BUNDLE, bundle))
                        }
                        InFlightRow::Intent { target, msg } => {
                            (IN_FLIGHT_INTENT, target, msg.due(), encode(FORMAT_INTENT_MSG, msg))
                        }
                    };
                    sqlx::query(
                        "INSERT INTO in_flight (seq, kind, target_rx, target_rz, stamp, payload, host) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7)",
                    )
                    .bind(base | seq as i64)
                    .bind(kind)
                    .bind(target.x as i64)
                    .bind(target.z as i64)
                    .bind(i(stamp))
                    .bind(payload)
                    .bind(cut.host as i64)
                    .execute(&mut *conn)
                    .await?;
                }
                if let Some(bytes) = &cut.history {
                    sqlx::query(
                        "INSERT INTO history (id, bytes) VALUES (1, $1) \
                         ON CONFLICT (id) DO UPDATE SET bytes = excluded.bytes",
                    )
                    .bind(bytes.as_slice())
                    .execute(&mut *conn)
                    .await?;
                }
                if matches!(end, CutEnd::Commit) {
                    sqlx::query("UPDATE world SET world_tick = $1 WHERE id = 1 AND world_tick < $1").bind(t).execute(&mut *conn).await?;
                }
                Ok(CutCommitted { id: cut.id, versions })
            }
        }
    };
}

// SQLite needs no row lock: claims and commits all hold the store's writer
// lock and one writer process holds the file lock.
// `$lock_timeout`: whether a cut's transaction sets a `lock_timeout` (Postgres).
// `$now_ms`: the engine's SQL for the database clock in ms since the epoch (`seen_ms`).
engine_rows!(sqlite, sqlx::Sqlite, None, "", "", false, None, "CAST(unixepoch('subsec') * 1000 AS INTEGER)");
engine_rows!(
    pg,
    sqlx::Postgres,
    Some("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ"),
    " FOR SHARE",
    " FOR UPDATE",
    true,
    Some("LOCK TABLE super_regions IN SHARE ROW EXCLUSIVE MODE"),
    // clock_timestamp(), not now(): now() is the transaction's start, and a registration or a
    // heartbeat transaction may have waited for locks before it stamps.
    "(extract(epoch from clock_timestamp()) * 1000)::bigint"
);

/// Announces committed cut `cut` at `tick` to every listener of this world (Postgres:
/// `pg_notify` on [`crate::listen::WORLD_CUT_CHANNEL`], in a transaction of its own —
/// delivered when it commits). Writes no row, so it needs no fence: a notice is only a
/// hint to re-read committed rows.
pub(crate) async fn notify_cut(pool: &sqlx::PgPool, cut: CutId, tick: Tick) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(crate::listen::WORLD_CUT_CHANNEL)
        .bind(crate::listen::payload(cut, tick))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
