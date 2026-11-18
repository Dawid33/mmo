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
use crate::fence::{Fence, SuperRegion, commit_check_set, load_check};
use crate::forged::{ForgedHead, TiledRows};
use game::HostId;
use game::ClockAnchor;
use crate::{CutCommitted, HostRow, StoreError, WorldInfo};

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

/// The lease beat's one statement, the database clock being `now_ms` (shared by `rows`'
/// `heartbeat` and the Postgres beat task): `seen_ms` moves for the caller's live boot only,
/// and the row answers its drain mark as ms from that same clock reading (`drain_at_ms -
/// seen_ms`, NULL when not draining). No row returned: not that live boot.
pub(crate) fn heartbeat_sql(now_ms: &str) -> String {
    format!(
        "UPDATE hosts SET seen_ms = {now_ms} WHERE id = $1 AND boot = $2 AND state = {} RETURNING drain_at_ms - seen_ms",
        crate::HOST_LIVE
    )
}

/// A [`heartbeat_sql`] answer as a [`crate::Beat`]: `None` = no row (replaced); `Some(d)` = the
/// live row and its remaining drain time.
pub(crate) fn beat_of(row: Option<Option<i64>>) -> crate::Beat {
    match row {
        None => crate::Beat { live: false, drain_in_ms: None },
        Some(drain_in_ms) => crate::Beat { live: true, drain_in_ms },
    }
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
            /// `lock_ms` ([`crate::CLAIM_LOCK_TIMEOUT_MS`] unless a test lowered it) — a prepared
            /// cut part holds its host's rows `FOR SHARE` until its cut is decided; one nobody
            /// decides would hold the claim for ever.
            async fn begin_claim(pool: &Pool, lock_ms: u64) -> Result<Tx, StoreError> {
                let mut tx = pool.begin().await?;
                cut_lock_timeout(&mut *tx, lock_ms).await?;
                Ok(tx)
            }

            /// Re-reads the super-region side in a claim's transaction (Postgres: `FOR KEY
            /// SHARE`, the row [`set_super_region_regions`] locks first — it waits only for that
            /// `FOR UPDATE`, not for a cut's ordinary update of the world row) and refuses with
            /// [`StoreError::OtherSuperRegionSide`] if it is not `opened`, the side the handle read
            /// at open (`worldctl super-region` changed it since). A lock timeout on that read
            /// is explained as any claim's is ([`claim_error`]).
            async fn check_side(pool: &Pool, mut tx: Tx, opened: u32, lock_ms: u64) -> Result<Tx, StoreError> {
                let key_share = if $share_lock.is_empty() { "" } else { " FOR KEY SHARE" };
                let sql = format!("SELECT super_region_regions FROM world WHERE id = 1{key_share}");
                let row = match sqlx::query(&sql).fetch_optional(&mut *tx).await {
                    Ok(row) => row,
                    Err(e) => return Err(claim_error(pool, tx, e, lock_ms).await),
                };
                let stored: i64 = row.ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?.try_get(0)?;
                if stored != i64::from(opened) {
                    let stored = u32::try_from(stored).unwrap_or(0);
                    return Err(StoreError::OtherSuperRegionSide { stored, opened });
                }
                Ok(tx)
            }

            /// A claim's (or release's) statement error: a `lock_timeout` (Postgres `55P03`)
            /// ends the transaction and names the prepared transactions of this world that may
            /// hold the rows ([`crate::open::claim_timeout_message`]).
            async fn claim_error(pool: &Pool, tx: Tx, e: sqlx::Error, lock_ms: u64) -> StoreError {
                claim_error_as(pool, tx, e, lock_ms, crate::open::CLAIMING).await
            }

            /// [`claim_error`] for what `what` names (a registration: [`crate::open::REGISTERING`]).
            async fn claim_error_as(pool: &Pool, tx: Tx, e: sqlx::Error, lock_ms: u64, what: &str) -> StoreError {
                match &e {
                    sqlx::Error::Database(d) if d.code().as_deref() == Some("55P03") => {
                        drop(tx);
                        let gids: Vec<String> =
                            sqlx::query_scalar(crate::open::PREPARED_GIDS_SQL).fetch_all(pool).await.unwrap_or_default();
                        StoreError::LockConflict {
                            sqlstate: "55P03".into(),
                            detail: crate::open::claim_timeout_message(what, lock_ms, &gids),
                        }
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
            /// unless that boot's `hosts` row is live ([`check_live`]); `None` (`AllInOne`, a
            /// `Static` host's start-up claim — made after it registered, alone —, tools)
            /// checks nothing.
            pub(crate) async fn claim_super_regions(
                pool: &Pool,
                owner: HostId,
                srs: Vec<SuperRegion>,
                opened: u32,
                live: Option<u64>,
                lock_ms: u64,
            ) -> Result<Fence, StoreError> {
                let srs: std::collections::BTreeSet<SuperRegion> = srs.into_iter().collect();
                let tx = begin_claim(pool, lock_ms).await?;
                let mut tx = check_side(pool, tx, opened, lock_ms).await?;
                let mut epochs = BTreeMap::new();
                for (sx, sz) in srs {
                    let row = sqlx::query(
                        "INSERT INTO super_regions (sx, sz, owner, epoch) VALUES ($1, $2, $3, 1) \
                         ON CONFLICT (sx, sz) DO UPDATE SET owner = excluded.owner, epoch = super_regions.epoch + 1 \
                         RETURNING epoch",
                    )
                    .bind(i64::from(sx))
                    .bind(i64::from(sz))
                    .bind(i(owner.0))
                    .fetch_one(&mut *tx)
                    .await;
                    let row = match row {
                        Ok(row) => row,
                        Err(e) => return Err(claim_error(pool, tx, e, lock_ms).await),
                    };
                    let epoch: i64 = row.try_get(0)?;
                    epochs.insert((sx, sz), u(epoch));
                }
                if let Some(boot) = live {
                    // A draining host keeps re-claiming its OWN rows (it runs them until it
                    // leaves): the drain mark is not looked at here, only in `claim_free`.
                    (tx, _) = check_live(pool, tx, owner, boot, lock_ms).await?;
                }
                tx.commit().await?;
                Ok(Fence { owner, epochs, all_hosts: false })
            }

            /// Refuses a claim by `owner` (boot `boot`) unless that boot's `hosts` row is live
            /// and dirty: a host declared dead (R4, its land moved to an heir), replaced by
            /// another boot, or that left cleanly (its leave cut committed: `clean = 1` — a
            /// `Leave` succession does not mark the row dead, so `state` alone would let a
            /// departed boot whose process still runs claim again) wins nothing —
            /// `Fenced { sr: None }`, the transaction rolled back. A new boot registers dirty
            /// and claims normally. The LAST
            /// statement of the claim (Postgres: the row `FOR SHARE`): the claim never holds
            /// the `hosts` row while it waits behind a staged part's rows (the heartbeat's
            /// short lock timeout would starve), and `FOR SHARE` serialises with a
            /// declaration's `FOR NO KEY UPDATE` of the row — a claim that locks first commits
            /// before the declaration moves the predecessor's rows (the newly won ones too); one
            /// that locks after sees the row dead. The second value: the row is marked
            /// draining (`drain_at_ms` set) — read under the same lock, so a drain mark (one
            /// `UPDATE` of the row) lands either before it (seen) or after the claim commits.
            async fn check_live(pool: &Pool, mut tx: Tx, owner: HostId, boot: u64, lock_ms: u64) -> Result<(Tx, bool), StoreError> {
                let sql = format!(
                    "SELECT drain_at_ms FROM hosts WHERE id = $1 AND boot = $2 AND state = {} AND clean = 0{}",
                    crate::HOST_LIVE,
                    $share_lock
                );
                let row = sqlx::query(&sql).bind(i(owner.0)).bind(i(boot)).fetch_optional(&mut *tx).await;
                match row {
                    Ok(Some(r)) => {
                        let draining = r.try_get::<Option<i64>, _>(0)?.is_some();
                        Ok((tx, draining))
                    }
                    Ok(None) => {
                        tx.rollback().await?;
                        log::warn!(
                            "[world-store] host {owner} boot {boot} claimed super-regions, but its hosts row is not that \
                             live, dirty boot (declared dead, replaced, or left cleanly): refused, nothing claimed"
                        );
                        Err(StoreError::Fenced { sr: None })
                    }
                    Err(e) => Err(claim_error(pool, tx, e, lock_ms).await),
                }
            }

            /// Claims for `owner` the super-regions of `srs` that are absent or free (owner
            /// [`crate::NO_OWNER`]), in one transaction like [`claim_super_regions`] (same
            /// `lock_timeout`, same side check, rows in key order): a new row gets epoch 1, a free
            /// one epoch + 1; a row another owner holds is left alone — and never LOCKED: two
            /// statements per super-region, an `INSERT … ON CONFLICT DO NOTHING` (an absent row)
            /// then, if that inserted nothing, an `UPDATE … WHERE owner = -1` (a free one; a row
            /// that does not match is not locked). An upsert's `ON CONFLICT DO UPDATE … WHERE`
            /// would lock the conflicting row BEFORE its `WHERE` — a row another host holds
            /// `FOR SHARE` in a staged cut part, so the claim would wait for that cut's decision
            /// (which may wait for this host's own part). Fenced first-writer-wins: the update's
            /// `WHERE owner = -1` is evaluated on the row's newest version (Postgres
            /// re-checks it after waiting for another claimer's row lock, and a second insert
            /// waits for the first's commit and then inserts nothing), so of two claimers
            /// exactly one takes a row. The fence holds the rows won (possibly none). Refused
            /// (`Fenced { sr: None }`, nothing claimed) unless `boot`'s `hosts` row is live
            /// ([`check_live`]). A live row marked draining (`worldctl drain`) wins NOTHING but
            /// is not refused: the transaction rolls back and the fence holds no rows — to the
            /// caller exactly "nothing was free" (Elastic 1: a draining host takes no new land;
            /// its own rows it still re-claims, [`claim_super_regions`]). SQLite callers hold
            /// the writer lock; Postgres needs none (the row locks serialise it, and holding the
            /// in-process lock while waiting for a row would hold up this host's own cut part).
            pub(crate) async fn claim_free(
                pool: &Pool,
                owner: HostId,
                boot: u64,
                srs: Vec<SuperRegion>,
                opened: u32,
                lock_ms: u64,
            ) -> Result<Fence, StoreError> {
                let srs: std::collections::BTreeSet<SuperRegion> = srs.into_iter().collect();
                let tx = begin_claim(pool, lock_ms).await?;
                let mut tx = check_side(pool, tx, opened, lock_ms).await?;
                // A plain read of the drain mark first (no lock): a host already marked
                // draining locks no free row for nothing. The locked re-check stays last.
                let marked = sqlx::query("SELECT drain_at_ms FROM hosts WHERE id = $1 AND boot = $2")
                    .bind(i(owner.0))
                    .bind(i(boot))
                    .fetch_optional(&mut *tx)
                    .await;
                match marked {
                    Ok(Some(r)) if r.try_get::<Option<i64>, _>(0)?.is_some() => {
                        tx.rollback().await?;
                        log::debug!("[world-store] host {owner} is draining: its claim of free land wins nothing");
                        return Ok(Fence { owner, epochs: BTreeMap::new(), all_hosts: false });
                    }
                    Ok(_) => {}
                    Err(e) => return Err(claim_error(pool, tx, e, lock_ms).await),
                }
                let insert = "INSERT INTO super_regions (sx, sz, owner, epoch) VALUES ($1, $2, $3, 1) \
                              ON CONFLICT (sx, sz) DO NOTHING RETURNING epoch";
                let take_free = format!(
                    "UPDATE super_regions SET owner = $3, epoch = epoch + 1 WHERE sx = $1 AND sz = $2 AND owner = {} \
                     RETURNING epoch",
                    crate::NO_OWNER
                );
                let mut epochs = BTreeMap::new();
                for (sx, sz) in srs {
                    let mut won = None;
                    for sql in [insert, take_free.as_str()] {
                        let row = sqlx::query(sql)
                            .bind(i64::from(sx))
                            .bind(i64::from(sz))
                            .bind(i(owner.0))
                            .fetch_optional(&mut *tx)
                            .await;
                        match row {
                            Ok(Some(row)) => {
                                won = Some(row.try_get::<i64, _>(0)?);
                                break;
                            }
                            Ok(None) => {}
                            Err(e) => return Err(claim_error(pool, tx, e, lock_ms).await),
                        }
                    }
                    if let Some(epoch) = won {
                        epochs.insert((sx, sz), u(epoch));
                    }
                }
                let (tx, draining) = check_live(pool, tx, owner, boot, lock_ms).await?;
                if draining {
                    tx.rollback().await?;
                    // Debug only: the server paces such claims and says so once.
                    log::debug!(
                        "[world-store] host {owner} is draining: it wins no new land ({} free super-region(s) left free)",
                        epochs.len()
                    );
                    return Ok(Fence { owner, epochs: BTreeMap::new(), all_hosts: false });
                }
                tx.commit().await?;
                Ok(Fence { owner, epochs, all_hosts: false })
            }

            /// Frees every super-region of `fence` (owner [`crate::NO_OWNER`], epoch + 1, so
            /// `fence` and anything claimed before it is fenced) in one transaction: first
            /// [`check_fence`] over the fence's own rows (refused with `Fenced` unless they are
            /// all still `fence`'s), then one update per row in key order. Only the fence's rows
            /// are freed: a release never skips the fence. `lock_timeout` `lock_ms`
            /// ([`crate::CLAIM_LOCK_TIMEOUT_MS`]) like a claim. Callers hold the writer lock.
            pub(crate) async fn release(pool: &Pool, fence: Fence, lock_ms: u64) -> Result<(), StoreError> {
                let srs: std::collections::BTreeSet<SuperRegion> = fence.epochs.keys().copied().collect();
                let mut tx = begin_claim(pool, lock_ms).await?;
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
                    .bind(i(fence.owner.0))
                    .execute(&mut *tx)
                    .await;
                    match done {
                        // Held `FOR SHARE` since the check (Postgres), under the writer lock (SQLite).
                        Ok(d) if d.rows_affected() == 1 => {}
                        Ok(_) => return Err(StoreError::Fenced { sr: Some((sx, sz)) }),
                        Err(e) => return Err(claim_error(pool, tx, e, lock_ms).await),
                    }
                }
                tx.commit().await?;
                Ok(())
            }

            /// Locks (Postgres: `FOR SHARE`) and checks every super-region of
            /// `srs` in `tx`: each must be claimed in `fence` and stored with its
            /// owner and epoch. The first that is not → `Fenced`. Postgres: before it locks
            /// anything the transaction's idle limit is set ([`crate::FENCED_TX_IDLE_MS`]) —
            /// a client that freezes holding these locks loses its session, and the locks
            /// with it, after a lease.
            async fn check_fence(
                conn: &mut Conn,
                fence: &Fence,
                srs: &std::collections::BTreeSet<SuperRegion>,
            ) -> Result<(), StoreError> {
                let srs: Vec<SuperRegion> = srs.iter().copied().collect();
                if $lock_timeout && !srs.is_empty() {
                    let ms = crate::fenced_tx_idle_ms();
                    sqlx::Executor::execute(
                        &mut *conn,
                        format!("SET LOCAL idle_in_transaction_session_timeout = '{ms}ms'").as_str(),
                    )
                    .await?;
                }
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
                        let ok = matches!((held, row), (Some(e), Some((o, se))) if o == i(fence.owner.0) && se == e);
                        if !ok {
                            return Err(StoreError::Fenced { sr: Some((sx, sz)) });
                        }
                    }
                }
                Ok(())
            }

            /// Test support ([`crate::StoreHandle::freeze_in_fenced_tx`]): a transaction that
            /// passed the real fence check over all of `fence`'s super-regions (Postgres: they
            /// are held `FOR SHARE`) and is handed back OPEN, for the caller to sit on.
            #[allow(dead_code)]
            pub(crate) async fn fenced_tx_held(pool: &Pool, fence: &Fence) -> Result<Tx, StoreError> {
                let mut tx = pool.begin().await?;
                let srs: std::collections::BTreeSet<SuperRegion> = fence.epochs.keys().copied().collect();
                check_fence(&mut *tx, fence, &srs).await?;
                Ok(tx)
            }

            /// The database clock in ms since the epoch (the one expression per engine).
            const NOW_MS: &str = $now_ms;

            /// Registers a new host (Elastic 1): ONE transaction (Postgres: `lock_timeout` `lock_ms`,
            /// [`crate::CLAIM_LOCK_TIMEOUT_MS`] — a total crash's take waits for what a claim waits
            /// for, a staged part's rows; a timeout is explained as a claim's, worded for a
            /// registration) that issues the next id and never waits for anything else to happen.
            /// UNFENCED: it is what creates the identity a fence is built from; each statement's
            /// safety and the lock walk are in the module notes of `succession.rs`. In order:
            /// 1. the world row (Postgres `FOR NO KEY UPDATE`, as every declaration, end of a landless host,
            ///    rollback and decision; SQLite has one writer): `id = next_host_id`, refused above
            ///    [`crate::HOST_ID_MAX`]; `next_host_id = id + 1`;
            /// 2. a non-empty `name`: other dirty live rows of that name get `seen_ms = 0` (so they
            ///    are not live below: a restarted process's own old row never is);
            /// 3. `Rollback` (`static`) and another row live → [`StoreError::NotAlone`];
            /// 4. no live row and some dirty one (a total crash) → [`total_crash`];
            /// 5. [`prune_hosts`];
            /// 6. the new row: dirty, live, seen now, at the world's generation.
            ///
            /// "Live" is [`live_ids`]. Nothing of a refused registration is written: the
            /// transaction rolls back, `next_host_id` with it. Callers hold the writer lock.
            pub(crate) async fn register_host(
                pool: &Pool,
                host: crate::NewHost,
                policy: game::CrashPolicy,
                lease_ms: u64,
                lock_ms: u64,
            ) -> Result<crate::Registered, StoreError> {
                let mut tx = pool.begin().await?;
                cut_lock_timeout(&mut *tx, lock_ms).await?;
                let done = register_in(&mut tx, &host, policy, lease_ms).await;
                match done {
                    Ok(Ok(registered)) => {
                        tx.commit().await?;
                        log::info!(
                            "[world-store] registered host {} ({:?}, boot {}) at world generation {}{}",
                            registered.id,
                            host.name,
                            host.boot,
                            registered.generation,
                            if registered.bumped { " (raised: a total crash; it took every dirty host's land)" } else { "" }
                        );
                        Ok(registered)
                    }
                    Ok(Err(live)) => {
                        tx.rollback().await?;
                        Err(StoreError::NotAlone { live })
                    }
                    Err(crate::succession::DeclareError::Store(e)) => Err(e),
                    Err(crate::succession::DeclareError::Sql(e)) => {
                        Err(claim_error_as(pool, tx, e, lock_ms, crate::open::REGISTERING).await)
                    }
                }
            }

            /// A `hosts.id` (or a column holding one) as a [`HostId`].
            fn host_id(v: i64) -> Result<HostId, StoreError> {
                u64::try_from(v).map(HostId).map_err(|_| StoreError::Corrupt(format!("host id {v}")))
            }

            /// The ids of the live rows, by id: dirty, `state` live, at the world's generation
            /// `world` (a row of an older one runs a world that is gone) and beaten within
            /// `lease_ms` by the database clock ([`NOW_MS`]; a vouched row, `seen_ms = 0`, is not).
            async fn live_ids(tx: &mut Tx, world: i64, lease_ms: u64) -> Result<Vec<HostId>, crate::succession::DeclareError> {
                let sql = format!(
                    "SELECT id FROM hosts WHERE clean = 0 AND state = {live} AND generation = $1 \
                     AND {NOW_MS} - seen_ms <= $2 ORDER BY id",
                    live = crate::HOST_LIVE,
                );
                let ids: Vec<i64> = sqlx::query_scalar(&sql).bind(world).bind(i(lease_ms)).fetch_all(&mut **tx).await?;
                Ok(ids.into_iter().map(host_id).collect::<Result<_, _>>()?)
            }

            /// [`register_host`]'s statements; `Ok(Err(live))`: refused, `NotAlone`.
            async fn register_in(
                tx: &mut Tx,
                host: &crate::NewHost,
                policy: game::CrashPolicy,
                lease_ms: u64,
            ) -> Result<Result<crate::Registered, Vec<HostId>>, crate::succession::DeclareError> {
                // 1. The world row: serialises the counter, the generation and every statement
                // below with every other registration, declaration, end of a landless host, rollback and
                // decision. `FOR NO KEY UPDATE` never waits for a claim's `FOR KEY SHARE`.
                let no_key = if $update_lock.is_empty() { "" } else { " FOR NO KEY UPDATE" };
                let sql = format!("SELECT generation, next_host_id FROM world WHERE id = 1{no_key}");
                let row = sqlx::query(&sql)
                    .fetch_optional(&mut **tx)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?;
                let (world, next): (i64, i64) = (row.try_get(0)?, row.try_get(1)?);
                let id = u64::try_from(next)
                    .ok()
                    .filter(|id| *id <= crate::HOST_ID_MAX)
                    .map(HostId)
                    .ok_or_else(|| {
                        StoreError::Corrupt(format!(
                            "world.next_host_id is {next}: above the highest host id {} (the in-flight rows' key packs \
                             the host into 32 bits); no id issued",
                            crate::HOST_ID_MAX
                        ))
                    })?;
                sqlx::query("UPDATE world SET next_host_id = $1 WHERE id = 1").bind(next + 1).execute(&mut **tx).await?;
                // 2. A restarted process's old id, by name, FIRST: lapsed now (declared at the
                // coordinator's next read instead of a lease later), and never counted as live
                // below — a lone host restarted within its lease is a total crash that takes its
                // own land back, not a host beside itself. A wrongly vouched live host is fenced
                // (its land moves at `epoch + 1`, here or by its succession), never duplicated.
                if !host.name.is_empty() {
                    let sql = format!(
                        "UPDATE hosts SET seen_ms = 0 WHERE name = $1 AND clean = 0 AND state = {live}",
                        live = crate::HOST_LIVE,
                    );
                    let vouched = sqlx::query(&sql).bind(&host.name).execute(&mut **tx).await?.rows_affected();
                    if vouched > 0 {
                        log::warn!(
                            "[world-store] host {id} registers as {:?}: {vouched} older dirty row(s) of that name lapsed \
                             (seen_ms = 0)",
                            host.name
                        );
                    }
                }
                // 3. `static` runs one host: refused beside another live row.
                let mut live = live_ids(tx, world, lease_ms).await?;
                if policy == game::CrashPolicy::Rollback && !live.is_empty() {
                    return Ok(Err(live));
                }
                // 4. A total crash: nobody live, somebody dirty. Lock the open successions, then
                // every dirty row (id order), and judge again under those locks — a beat that
                // landed meanwhile makes its row live after all (a beat waits for these locks
                // from here on).
                let mut generation = world;
                // "Somebody dirty" is a dirty row that is live, or holds land (a `super_regions`
                // or `in_flight` row), or is the predecessor of an open succession — never an
                // ENDED row (dead, landless, named by no open run: Elastic 2a final review S-M1),
                // which step 5 prunes: after a clean stop it is no total crash. A plain read
                // under the world row's lock (step 1), which every writer of these rows takes.
                let sql = format!(
                    "SELECT COUNT(*) FROM hosts WHERE clean = 0 AND (state = {live} \
                     OR id IN (SELECT owner FROM super_regions) OR id IN (SELECT host FROM in_flight) \
                     OR id IN (SELECT predecessor FROM successions WHERE settled_cut IS NULL AND abandoned = 0))",
                    live = crate::HOST_LIVE,
                );
                let dirty: i64 = sqlx::query_scalar(&sql).fetch_one(&mut **tx).await?;
                if live.is_empty() && dirty > 0 {
                    if !$update_lock.is_empty() {
                        sqlx::query(
                            "SELECT id FROM successions WHERE settled_cut IS NULL AND abandoned = 0 ORDER BY id FOR UPDATE",
                        )
                        .execute(&mut **tx)
                        .await?;
                        sqlx::query("SELECT id FROM hosts WHERE clean = 0 ORDER BY id FOR NO KEY UPDATE")
                            .execute(&mut **tx)
                            .await?;
                    }
                    live = live_ids(tx, world, lease_ms).await?;
                    if live.is_empty() {
                        total_crash(tx, id, world).await?;
                        generation = world + 1;
                    } else if policy == game::CrashPolicy::Rollback {
                        return Ok(Err(live));
                    }
                }
                // 5. Rows nothing needs any more.
                prune_hosts(tx).await?;
                // 6. The new row. `reg` (no longer read) gets the id: also a registration number.
                let sql = format!(
                    "INSERT INTO hosts (id, name, boot, generation, clean, state, seen_ms, reg, drain_at_ms, peer_addr, \
                     peer_cert, quic, webtransport, wt_cert_hash, updated_utc) \
                     VALUES ($1, $2, $3, $4, 0, {live}, {NOW_MS}, $1, NULL, $5, $6, $7, $8, $9, $10)",
                    live = crate::HOST_LIVE,
                );
                sqlx::query(&sql)
                    .bind(i(id.0))
                    .bind(&host.name)
                    .bind(i(host.boot))
                    .bind(generation)
                    .bind(&host.peer_addr)
                    .bind(&host.peer_cert)
                    .bind(&host.quic)
                    .bind(&host.webtransport)
                    .bind(&host.wt_cert_hash)
                    .bind(crate::open::utc_now())
                    .execute(&mut **tx)
                    .await?;
                Ok(Ok(crate::Registered { id, generation: u(generation), bumped: generation != world }))
            }

            /// A total crash at registration (decision 4; [`register_host`] step 3, the dirty rows
            /// and the open successions already locked): every dirty row's `super_regions` rows
            /// locked in key order (as a declaration does); every open succession abandoned as a
            /// world rollback abandons it ([`crate::succession::rollback_in`]'s statements: each
            /// predecessor's in-flight rows to its heir); then every dirty row's `super_regions`
            /// and `in_flight` rows are `me`'s — the super-regions at `epoch + 1`, so every old
            /// process is fenced — those rows marked dead, and `world.generation` = `world + 1`.
            async fn total_crash(tx: &mut Tx, me: HostId, world: i64) -> Result<(), crate::succession::DeclareError> {
                // The dirty rows' land first (key order, as a declaration), then everything in
                // `in_flight`: the walk's order (`succession.rs`), the one a cut part takes too.
                let dirty = "SELECT id FROM hosts WHERE clean = 0";
                let lock = if $update_lock.is_empty() { "" } else { " FOR NO KEY UPDATE" };
                sqlx::query(&format!("SELECT 1 FROM super_regions WHERE owner IN ({dirty}) ORDER BY sx, sz{lock}"))
                    .execute(&mut **tx)
                    .await?;
                // Ordered by id: a predecessor's rows end with the heir of its LAST open succession.
                let mut abandoned: Vec<(i64, i64, i64)> = sqlx::query_as(
                    "UPDATE successions SET abandoned = 1 WHERE settled_cut IS NULL AND abandoned = 0 \
                     RETURNING id, predecessor, heir",
                )
                .fetch_all(&mut **tx)
                .await?;
                abandoned.sort();
                for (_, predecessor, heir) in &abandoned {
                    sqlx::query("UPDATE in_flight SET host = $1 WHERE host = $2")
                        .bind(heir)
                        .bind(predecessor)
                        .execute(&mut **tx)
                        .await?;
                }
                let land = sqlx::query(&format!(
                    "UPDATE super_regions SET owner = $1, epoch = epoch + 1 WHERE owner IN ({dirty})"
                ))
                .bind(i(me.0))
                .execute(&mut **tx)
                .await?
                .rows_affected();
                let rows = sqlx::query(&format!("UPDATE in_flight SET host = $1 WHERE host IN ({dirty})"))
                    .bind(i(me.0))
                    .execute(&mut **tx)
                    .await?
                    .rows_affected();
                let dead: Vec<i64> = sqlx::query_scalar(&format!(
                    "UPDATE hosts SET state = {dead} WHERE clean = 0 AND state = {live} RETURNING id",
                    dead = crate::HOST_DEAD,
                    live = crate::HOST_LIVE,
                ))
                .fetch_all(&mut **tx)
                .await?;
                sqlx::query("UPDATE world SET generation = $1 WHERE id = 1").bind(world + 1).execute(&mut **tx).await?;
                log::warn!(
                    "[world-store] total crash: host {me} takes {land} super-region(s) and {rows} in-flight row(s) of every \
                     dirty host, marks hosts {dead:?} dead, abandons {} open succession(s); world generation {} → {}",
                    abandoned.len(),
                    world,
                    world + 1
                );
                Ok(())
            }

            /// Deletes every `hosts` row nothing refers to any more: clean or dead, owning no
            /// `super_regions` row and no `in_flight` row, and named by no open succession (as
            /// predecessor, heir or survivor). Its id is never issued again (`world.next_host_id`
            /// only grows), so nothing that still carries it — a coordinator's saved state, a
            /// journal, a prepared part's gid — can ever match a new host.
            async fn prune_hosts(tx: &mut Tx) -> Result<(), crate::succession::DeclareError> {
                let sql = format!(
                    "DELETE FROM hosts WHERE (clean = 1 OR state = {dead}) \
                     AND NOT EXISTS (SELECT 1 FROM super_regions sr WHERE sr.owner = hosts.id) \
                     AND NOT EXISTS (SELECT 1 FROM in_flight f WHERE f.host = hosts.id) \
                     AND NOT EXISTS (SELECT 1 FROM successions s WHERE s.settled_cut IS NULL AND s.abandoned = 0 \
                     AND (s.predecessor = hosts.id OR s.heir = hosts.id \
                     OR (',' || s.survivors || ',') LIKE ('%,' || CAST(hosts.id AS TEXT) || ',%'))) \
                     RETURNING id",
                    dead = crate::HOST_DEAD,
                );
                let mut pruned: Vec<i64> = sqlx::query_scalar(&sql).fetch_all(&mut **tx).await?;
                if !pruned.is_empty() {
                    pruned.sort();
                    log::info!("[world-store] pruned the rows of hosts {pruned:?} (clean or dead, owning nothing)");
                }
                Ok(())
            }

            /// Every host row, by id, each with whether it holds land ([`HostRow::owns_land`]) and
            /// whether an open succession has it as predecessor ([`HostRow::succeeding`]); the
            /// subqueries read without locks. With the database clock read right after the rows
            /// ([`crate::HostsRead::store_ms`]: the coordinator's clock is the store's).
            pub(crate) async fn hosts(pool: &Pool) -> Result<crate::HostsRead, StoreError> {
                // First, a plain read of the next id (no lock; Elastic 2b Task 6): every id below
                // it is issued before the rows below are read — absent from them, it was pruned.
                let next: i64 = sqlx::query("SELECT next_host_id FROM world WHERE id = 1")
                    .fetch_optional(pool)
                    .await?
                    .map(|r| r.try_get(0))
                    .transpose()?
                    .unwrap_or(0);
                let sql = format!(
                    "SELECT id, boot, generation, clean, peer_addr, peer_cert, quic, webtransport, wt_cert_hash, state, \
                     seen_ms, name, drain_at_ms - {NOW_MS}, \
                     (EXISTS (SELECT 1 FROM super_regions WHERE owner = hosts.id) \
                      OR EXISTS (SELECT 1 FROM in_flight WHERE host = hosts.id)), \
                     EXISTS (SELECT 1 FROM successions WHERE predecessor = hosts.id \
                             AND predecessor_boot = hosts.boot AND settled_cut IS NULL AND abandoned = 0) \
                     FROM hosts ORDER BY id"
                );
                let rows = sqlx::query(&sql).fetch_all(pool).await?;
                let rows = rows
                    .into_iter()
                    .map(|r| {
                        Ok(HostRow {
                            id: host_id(r.try_get(0)?)?,
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
                            name: r.try_get(11)?,
                            drain_in_ms: r.try_get(12)?,
                            owns_land: r.try_get(13)?,
                            succeeding: r.try_get(14)?,
                        })
                    })
                    .collect::<Result<Vec<HostRow>, StoreError>>()?;
                let store_ms = db_now_ms(pool).await?;
                Ok(crate::HostsRead { rows, store_ms, next_host_id: u(next) })
            }

            /// One lease beat. UNFENCED on purpose: one statement over the caller's own `hosts` row.
            /// Postgres: a short transaction with `lock_timeout` [`crate::HEARTBEAT_LOCK_TIMEOUT_MS`]
            /// — if a registration or a decision holds the row, the beat FAILS (`Err`: "no beat this
            /// time"), it neither hangs nor answers `live: false`. It touches neither `world` nor
            /// `super_regions`. `live: false`: no live row of that boot (the caller was
            /// replaced); a live one also answers its drain mark ([`super::heartbeat_sql`]).
            pub(crate) async fn heartbeat(pool: &Pool, ordinal: HostId, boot: u64) -> Result<crate::Beat, StoreError> {
                let sql = super::heartbeat_sql(NOW_MS);
                let mut tx = pool.begin().await?;
                cut_lock_timeout(&mut *tx, crate::HEARTBEAT_LOCK_TIMEOUT_MS).await?;
                let row = sqlx::query(&sql).bind(i(ordinal.0)).bind(i(boot)).fetch_optional(&mut *tx).await?;
                tx.commit().await?;
                Ok(super::beat_of(row.map(|r| r.try_get::<Option<i64>, _>(0)).transpose()?))
            }

            /// The drain mark (`worldctl drain`, [`crate::StoreHandle::drain_host`]): ONE `UPDATE`
            /// of host `id`'s row, UNFENCED like the heartbeat — it writes one `hosts` row's
            /// `drain_at_ms` and changes no ownership (lock notes: `operator.rs`). The new
            /// deadline is the database clock + `in_ms`, unless the row's mark is earlier
            /// already: a mark only moves earlier. Only a dirty live row beaten within `lease_ms`
            /// by the database clock (a lapsed one's process is gone, not declared yet: a
            /// countdown for it would mean nothing; a vouched one, `seen_ms` 0, likewise);
            /// otherwise [`StoreError::NotDrainable`] naming why (read after the update matched
            /// nothing), nothing written. Postgres: `lock_timeout` `lock_ms`.
            pub(crate) async fn drain_host(pool: &Pool, id: HostId, in_ms: u64, lock_ms: u64, lease_ms: u64) -> Result<(), StoreError> {
                // One clock reading per statement: Postgres' `LEAST` skips a NULL; SQLite's
                // two-argument `MIN` is NULL then (and `unixepoch` is fixed within a statement).
                let deadline = if $share_lock.is_empty() {
                    format!("COALESCE(MIN(drain_at_ms, {NOW_MS} + $2), {NOW_MS} + $2)")
                } else {
                    format!("LEAST(drain_at_ms, {NOW_MS} + $2)")
                };
                let sql = format!(
                    "UPDATE hosts SET drain_at_ms = {deadline} WHERE id = $1 AND clean = 0 AND state = {} \
                     AND {NOW_MS} - seen_ms <= $3",
                    crate::HOST_LIVE
                );
                if in_ms > crate::DRAIN_MAX_SECS * 1000 {
                    return Err(StoreError::Unsupported(format!(
                        "a drain notice of {in_ms} ms is longer than the longest, {} s (DRAIN_MAX_SECS)",
                        crate::DRAIN_MAX_SECS
                    )));
                }
                let in_ms = i(in_ms);
                let mut tx = pool.begin().await?;
                cut_lock_timeout(&mut *tx, lock_ms).await?;
                let done = sqlx::query(&sql).bind(i(id.0)).bind(in_ms).bind(i(lease_ms)).execute(&mut *tx).await?;
                if done.rows_affected() == 1 {
                    tx.commit().await?;
                    log::warn!("[world-store] host {} marked draining: it leaves within {in_ms} ms", id.0);
                    return Ok(());
                }
                let read = format!("SELECT clean, state, {NOW_MS} - seen_ms FROM hosts WHERE id = $1");
                let row = sqlx::query(&read).bind(i(id.0)).fetch_optional(&mut *tx).await?;
                tx.rollback().await?;
                let n = id.0;
                let why = match row {
                    None => format!("host {n} has no hosts row: nothing to drain (`worldctl hosts` lists the ids)"),
                    Some(r) if r.try_get::<i64, _>(1)? != crate::HOST_LIVE => {
                        format!("host {n} is dead (declared, its land inherited): nothing to drain")
                    }
                    Some(r) if r.try_get::<i64, _>(0)? == 0 => format!(
                        "host {n} has not beaten its lease for {} ms: its process is gone (not declared dead yet) — \
                         nothing to drain; its land goes to an heir once it is declared",
                        r.try_get::<i64, _>(2)?
                    ),
                    Some(_) => format!("host {n} left cleanly: nothing to drain"),
                };
                Err(StoreError::NotDrainable(why))
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
            pub(crate) async fn owned_by(pool: &Pool, owner: HostId) -> Result<Fence, StoreError> {
                let rows = sqlx::query("SELECT sx, sz, epoch FROM super_regions WHERE owner = $1")
                    .bind(i(owner.0))
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

            /// Bootstraps the coordinator row (Elastic 2b Task 3): writes `core` and `history`
            /// at version 1, epoch 1 — unless a row with a projection exists (first writer wins:
            /// [`crate::Bootstrapped::Exists`], nothing written; the caller loads it). A row with
            /// no projection (claimed by a build before Elastic 2b and never written) is filled
            /// at its next version and epoch, so no cut id of its older epochs is made again. One
            /// statement on the one `coordinator` row (lock notes: `succession.rs`). Callers hold
            /// the writer lock.
            pub(crate) async fn bootstrap_coordinator(
                pool: &Pool,
                core: Vec<u8>,
                history: Vec<u8>,
            ) -> Result<crate::Bootstrapped, StoreError> {
                let row = sqlx::query(
                    "INSERT INTO coordinator (id, epoch, version, core, history, saved_utc, history_version) \
                     VALUES (1, 1, 1, $1, $2, $3, 1) \
                     ON CONFLICT (id) DO UPDATE SET epoch = coordinator.epoch + 1, version = coordinator.version + 1, \
                     core = excluded.core, history = excluded.history, saved_utc = excluded.saved_utc, \
                     history_version = coordinator.version + 1 \
                     WHERE coordinator.core IS NULL RETURNING version, epoch",
                )
                .bind(core)
                .bind(history)
                .bind(crate::open::utc_now())
                .fetch_optional(pool)
                .await?;
                Ok(match row {
                    Some(r) => crate::Bootstrapped::Written { version: u(r.try_get(0)?), epoch: u(r.try_get(1)?) },
                    None => crate::Bootstrapped::Exists,
                })
            }

            /// Raises the existing coordinator row's `epoch` past every cut id's high half made
            /// before (final review T3): past its own epoch and past the high half of every
            /// recorded `cut_decisions` id — the epoch a core REPLACING an unreadable one (another
            /// world's, an older format's) makes its cut ids under, so that no id of the replaced
            /// core is made again (a stored decision of a reused id would be returned unchecked).
            /// The new epoch; `None`: no row. One statement on the one `coordinator` row (lock
            /// notes: `succession.rs`; `cut_decisions` is read without a lock). The version is
            /// untouched (the replacing core's first transition writes at it). Callers hold the
            /// writer lock.
            pub(crate) async fn raise_coordinator_epoch(pool: &Pool) -> Result<Option<u64>, StoreError> {
                let row = sqlx::query(
                    "UPDATE coordinator SET epoch = 1 + CASE \
                     WHEN epoch >= COALESCE((SELECT MAX(cut_id) FROM cut_decisions), 0) / 4294967296 THEN epoch \
                     ELSE COALESCE((SELECT MAX(cut_id) FROM cut_decisions), 0) / 4294967296 END \
                     WHERE id = 1 RETURNING epoch",
                )
                .fetch_optional(pool)
                .await?;
                Ok(match row {
                    Some(r) => Some(u(r.try_get(0)?)),
                    None => None,
                })
            }

            /// Writes the coordinator's projection (Elastic 2a) with NO store decision (on
            /// SQLite every write is one: a transition with decisions is refused before this):
            /// `core`, and `history` unless `None` (the stored one is kept), if the row is at
            /// `version` — the compare-and-set is the update's own `WHERE` (Elastic 2b Task 3: the
            /// version is the coordinator's fence, no epoch is compared); the version goes up by
            /// one. Returns the new version and the database clock; no row matched (the version
            /// moved: a second writer, or no row) is [`StoreError::Conflict`]. One statement on the
            /// one `coordinator` row (lock notes: `succession.rs`). Callers hold the writer lock.
            pub(crate) async fn write_coordinator_cas(
                pool: &Pool,
                version: u64,
                core: Vec<u8>,
                history: Option<Vec<u8>>,
            ) -> Result<(u64, u64), StoreError> {
                let sql = format!(
                    "UPDATE coordinator SET core = $1, history = COALESCE($2, history), version = version + 1, \
                     history_version = CASE WHEN $2 IS NULL THEN history_version ELSE version + 1 END, \
                     saved_utc = $3 WHERE id = 1 AND version = $4 RETURNING version, {NOW_MS}"
                );
                let row = sqlx::query(&sql)
                    .bind(core)
                    .bind(history)
                    .bind(crate::open::utc_now())
                    .bind(i(version))
                    .fetch_optional(pool)
                    .await?;
                match row {
                    Some(r) => Ok((u(r.try_get(0)?), u(r.try_get(1)?))),
                    None => Err(StoreError::Conflict),
                }
            }

            /// The coordinator row's projection; `None` when there is no row or its `core` was
            /// never written (a claimed row before its first write).
            pub(crate) async fn load_coordinator(pool: &Pool) -> Result<Option<crate::StoredCoordinator>, StoreError> {
                let row = sqlx::query("SELECT version, core, history, history_version FROM coordinator WHERE id = 1")
                    .fetch_optional(pool)
                    .await?;
                let Some(r) = row else { return Ok(None) };
                let core: Option<Vec<u8>> = r.try_get(1)?;
                let Some(core) = core else { return Ok(None) };
                let history: Option<Vec<u8>> = r.try_get(2)?;
                Ok(Some(crate::StoredCoordinator {
                    version: u(r.try_get(0)?),
                    core,
                    history: history.unwrap_or_default(),
                    history_version: u(r.try_get(3)?),
                }))
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
            pub(crate) async fn load_in_flight(pool: &Pool, host: Option<HostId>) -> Result<Vec<InFlightRow>, StoreError> {
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
                host: HostId,
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
            async fn check_adopts(conn: &mut Conn, heir: HostId, adopts: &[HostId]) -> Result<(), StoreError> {
                for predecessor in adopts {
                    let open: i64 = sqlx::query(
                        "SELECT COUNT(*) FROM successions WHERE predecessor = $1 AND heir = $2 \
                         AND settled_cut IS NULL AND abandoned = 0",
                    )
                    .bind(i(predecessor.0))
                    .bind(i(heir.0))
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
                            u64::try_from(v).map(HostId).map_err(|_| StoreError::Corrupt(format!("successions {id}: ordinal {v}")))
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

            async fn in_flight_rows(tx: &mut Tx, host: Option<HostId>) -> Result<Vec<InFlightRow>, StoreError> {
                let rows = sqlx::query(
                    "SELECT seq, kind, target_rx, target_rz, stamp, payload FROM in_flight \
                     WHERE $1 IS NULL OR host = $1 ORDER BY seq",
                )
                .bind(host.map(|h| i(h.0)))
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
                        q = q.bind(i(fence.owner.0)).bind(i64::from(s));
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
                    .bind(fence.in_flight_scope().map(|_| i(cut.host.0)))
                    .execute(&mut *conn)
                    .await?;
                // An heir's part also replaces the rows of the predecessors it adopts (checked
                // above): what it kept of them is in `cut.in_flight`, now under its own host.
                for predecessor in &cut.adopts {
                    sqlx::query("DELETE FROM in_flight WHERE host = $1")
                        .bind(i(predecessor.0))
                        .execute(&mut *conn)
                        .await?;
                }
                // `seq` is the table's key across all hosts: each host owns a range
                // (`host << 32`), so concurrent commits never collide and need no MAX read.
                let base = (i(cut.host.0)) << 32;
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
                    .bind(i(cut.host.0))
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
