//! The row SQL, written once and instantiated per engine (`sqlite`, `pg`):
//! runtime `sqlx::query` only, `$N` placeholders (SQLite reads them too).
//! u64 values (ticks, serials, player ids) are stored as i64 by bit cast.
//!
//! Writes go through the store thread's writer lock (`thread.rs`), so two
//! write transactions never race on SQLite's deferred upgrade. Fenced writes
//! and loads check the owner epochs first in their own transaction (`fence.rs`).

use std::collections::BTreeMap;

use game::{
    ColumnCoords, CutId, EntityBundle, EntityId, EntityKind, InFlightRow, IntentMsg, PlayerId, PlayerRecord,
    RegionCoords, RegionMeta, StoredRegion, StudChunkCoords, StudChunkRecord, StudColumnGround, TerrainSnapshot, Tick,
    WorldCut,
};

use crate::codec::*;
use crate::fence::{CoordinatorFence, Fence, SuperRegion, commit_check_set, load_check};
use crate::forged::{ForgedHead, TiledRows};
use game::ClockAnchor;
use crate::{CutCommitted, HostBoot, HostRow, StoreError, TerrainKind, WorldInfo};

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
fn i(v: u64) -> i64 {
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

fn stud_chunk_of(rc: RegionCoords, cx: i64, cy: i64, cz: i64) -> Result<StudChunkCoords, StoreError> {
    let bad = || StoreError::Corrupt(format!("region {rc:?}: stud chunk ({cx}, {cy}, {cz}) out of range"));
    let y = i32::try_from(cy).ok().filter(|y| game::stud_layer_ok(*y)).ok_or_else(bad)?;
    let side = |v: i64| usize::try_from(v).ok().filter(|v| *v < game::REGION_CHUNKS).ok_or_else(bad);
    Ok(StudChunkCoords { x: side(cx)?, y, z: side(cz)? })
}

/// `regions.stud_ground` of a save: the recipes' blob, NULL for a region with none.
fn stud_ground_blob(ground: Option<&Vec<(ColumnCoords, StudColumnGround)>>) -> Option<Vec<u8>> {
    ground.filter(|g| !g.is_empty()).map(|g| encode(FORMAT_STUD_GROUND, g))
}

fn terrain_of(code: Option<i64>) -> Result<Option<TerrainKind>, StoreError> {
    match code {
        None => Ok(None),
        Some(c) => TerrainKind::from_code(c)
            .map(Some)
            .ok_or_else(|| StoreError::Corrupt(format!("world.terrain {c} is no terrain kind"))),
    }
}

/// An entity insert failing on the primary key is the same entity in two
/// regions of one cut (or twice in one): a conservation bug, kept loud.
fn entity_insert_error(e: sqlx::Error, id: EntityId, rc: RegionCoords, tick: Tick) -> StoreError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => StoreError::Corrupt(format!(
            "entity {id:?} in two regions of the cut at tick {tick} (again in {rc:?}): an entity was \
             saved twice — a conservation bug; the cut was not committed"
        )),
        _ => e.into(),
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
    ($module:ident, $db:ty, $read_prelude:expr, $share_lock:expr, $update_lock:expr, $lock_timeout:expr) => {
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

            /// Claims every super-region of `srs` for `owner` in one transaction:
            /// a new row gets epoch 1, a known one epoch + 1 (whoever held it is
            /// fenced). Rows are upserted in key order. Callers hold the writer lock.
            pub(crate) async fn claim_super_regions(
                pool: &Pool,
                owner: u32,
                srs: Vec<SuperRegion>,
            ) -> Result<Fence, StoreError> {
                let srs: std::collections::BTreeSet<SuperRegion> = srs.into_iter().collect();
                let mut tx = pool.begin().await?;
                let mut epochs = BTreeMap::new();
                for (sx, sz) in srs {
                    let epoch: i64 = sqlx::query(
                        "INSERT INTO super_regions (sx, sz, owner, epoch) VALUES ($1, $2, $3, 1) \
                         ON CONFLICT (sx, sz) DO UPDATE SET owner = excluded.owner, epoch = super_regions.epoch + 1 \
                         RETURNING epoch",
                    )
                    .bind(i64::from(sx))
                    .bind(i64::from(sz))
                    .bind(i64::from(owner))
                    .fetch_one(&mut *tx)
                    .await?
                    .try_get(0)?;
                    epochs.insert((sx, sz), u(epoch));
                }
                tx.commit().await?;
                Ok(Fence { owner, epochs, all_hosts: false })
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

            /// Registers a host boot (R3): in one transaction, with the fence checked over the fence's own
            /// super-regions first and then the world row locked (Postgres: `FOR UPDATE`; SQLite
            /// has one writer) — the lock order of `commit_cut`. If the ordinal's row exists, is not clean and
            /// still holds the world's generation, the previous boot died without a clean
            /// save: `world.generation` goes up (`bumped`). The row is then upserted with
            /// the new boot, the world's generation and `clean = 0`. `row.generation` and
            /// `row.clean` are ignored. Callers hold the writer lock.
            pub(crate) async fn register_host(pool: &Pool, row: HostRow, fence: &Fence) -> Result<HostBoot, StoreError> {
                let mut tx = pool.begin().await?;
                let srs: std::collections::BTreeSet<SuperRegion> = fence.epochs.keys().copied().collect();
                check_fence(&mut *tx, fence, &srs).await?;
                let world: i64 = sqlx::query(concat!("SELECT generation FROM world WHERE id = 1", $update_lock))
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?
                    .try_get(0)?;
                let prior = sqlx::query("SELECT generation, clean FROM hosts WHERE ordinal = $1")
                    .bind(i64::from(row.ordinal))
                    .fetch_optional(&mut *tx)
                    .await?;
                let mut generation = world;
                let mut bumped = false;
                if let Some(prior) = prior {
                    let (prior_generation, prior_clean): (i64, i64) = (prior.try_get(0)?, prior.try_get(1)?);
                    if prior_clean == 0 && prior_generation == world {
                        generation = world + 1;
                        bumped = true;
                        sqlx::query("UPDATE world SET generation = $1 WHERE id = 1").bind(generation).execute(&mut *tx).await?;
                    }
                }
                sqlx::query(
                    "INSERT INTO hosts (ordinal, boot, generation, clean, peer_addr, peer_cert, quic, webtransport, wt_cert_hash, updated_utc) \
                     VALUES ($1, $2, $3, 0, $4, $5, $6, $7, $8, $9) \
                     ON CONFLICT (ordinal) DO UPDATE SET boot = excluded.boot, generation = excluded.generation, clean = 0, \
                     peer_addr = excluded.peer_addr, peer_cert = excluded.peer_cert, quic = excluded.quic, \
                     webtransport = excluded.webtransport, wt_cert_hash = excluded.wt_cert_hash, updated_utc = excluded.updated_utc",
                )
                .bind(i64::from(row.ordinal))
                .bind(i(row.boot))
                .bind(generation)
                .bind(&row.peer_addr)
                .bind(&row.peer_cert)
                .bind(&row.quic)
                .bind(&row.webtransport)
                .bind(&row.wt_cert_hash)
                .bind(crate::open::utc_now())
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(HostBoot { generation: u(generation), bumped })
            }

            /// Every host row, by ordinal.
            pub(crate) async fn hosts(pool: &Pool) -> Result<Vec<HostRow>, StoreError> {
                let rows = sqlx::query(
                    "SELECT ordinal, boot, generation, clean, peer_addr, peer_cert, quic, webtransport, wt_cert_hash \
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
                        })
                    })
                    .collect()
            }

            /// The highest epoch of the super-regions `owner` holds: its claim epoch
            /// (`Fence::claim_epoch`) while its claim stands; `None` if it holds none.
            pub(crate) async fn owner_claim_epoch(pool: &Pool, owner: u32) -> Result<Option<u64>, StoreError> {
                let e: Option<i64> = sqlx::query("SELECT MAX(epoch) FROM super_regions WHERE owner = $1")
                    .bind(i64::from(owner))
                    .fetch_one(pool)
                    .await?
                    .try_get(0)?;
                Ok(e.map(u))
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

            /// The region's stud rows and recipes. Any row that does not decode, lies
            /// outside the region, or is a pristine marker with no recipe to derive
            /// it from is `Corrupt`: stud terrain is never loaded partly.
            async fn load_studs(
                tx: &mut Tx,
                rc: RegionCoords,
                ground: Option<Vec<u8>>,
            ) -> Result<(Vec<(StudChunkCoords, StudChunkRecord)>, Vec<(ColumnCoords, StudColumnGround)>), StoreError> {
                let ground: Vec<(ColumnCoords, StudColumnGround)> = match ground {
                    Some(blob) => decode_stud_ground(&blob)
                        .map_err(|e| StoreError::Corrupt(format!("region {rc:?} stud ground: {e}")))?,
                    None => Vec::new(),
                };
                let rows = sqlx::query("SELECT cx, cy, cz, data FROM stud_chunks WHERE rx = $1 AND rz = $2")
                    .bind(rc.x as i64)
                    .bind(rc.z as i64)
                    .fetch_all(&mut **tx)
                    .await?;
                let columns: std::collections::BTreeSet<ColumnCoords> = ground.iter().map(|(c, _)| *c).collect();
                let mut chunks = Vec::with_capacity(rows.len());
                for r in rows {
                    let coords = stud_chunk_of(rc, r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)?;
                    let data: Vec<u8> = r.try_get(3)?;
                    let record: StudChunkRecord = decode(FORMAT_STUD_CHUNK, &data)
                        .map_err(|e| StoreError::Corrupt(format!("region {rc:?} stud chunk {coords:?}: {e}")))?;
                    if record == StudChunkRecord::Pristine && !columns.contains(&ColumnCoords::new(coords.x, coords.z)) {
                        return Err(StoreError::Corrupt(format!(
                            "region {rc:?} stud chunk {coords:?} is stored as pristine but its column has no ground recipe"
                        )));
                    }
                    chunks.push((coords, record));
                }
                chunks.sort_by_key(|(c, _)| *c);
                Ok((chunks, ground))
            }

            /// The `regions` row's `(meta, stud_ground)` blobs, if the region was ever saved.
            async fn region_row(tx: &mut Tx, rc: RegionCoords) -> Result<Option<(Vec<u8>, Option<Vec<u8>>)>, StoreError> {
                let row = sqlx::query("SELECT meta, stud_ground FROM regions WHERE rx = $1 AND rz = $2")
                    .bind(rc.x as i64)
                    .bind(rc.z as i64)
                    .fetch_optional(&mut **tx)
                    .await?;
                Ok(match row {
                    Some(r) => Some((r.try_get(0)?, r.try_get(1)?)),
                    None => None,
                })
            }

            /// `world.terrain`: the kind the world is tied to, if any yet.
            pub(crate) async fn terrain_kind(pool: &Pool) -> Result<Option<TerrainKind>, StoreError> {
                let code: Option<i64> = sqlx::query("SELECT terrain FROM world WHERE id = 1")
                    .fetch_optional(pool)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?
                    .try_get(0)?;
                terrain_of(code)
            }

            /// Ties the world to `kind` if it is tied to none (the update's own
            /// `WHERE`: set once, never changed), then checks what it is tied to.
            /// Callers hold the writer lock.
            pub(crate) async fn bind_terrain(pool: &Pool, kind: TerrainKind) -> Result<(), StoreError> {
                sqlx::query("UPDATE world SET terrain = $1 WHERE id = 1 AND terrain IS NULL")
                    .bind(kind.code())
                    .execute(pool)
                    .await?;
                match terrain_kind(pool).await? {
                    Some(stored) if stored == kind => Ok(()),
                    Some(stored) => Err(StoreError::OtherTerrain { stored, wanted: kind }),
                    None => Err(StoreError::Corrupt("world.terrain is unset after it was written".into())),
                }
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
                let Some((meta, stud_ground)) = region_row(&mut tx, rc).await? else {
                    tx.commit().await?;
                    return Ok(None);
                };
                let meta: RegionMeta = decode_region_meta(&meta)
                    .map_err(|e| StoreError::Corrupt(format!("region {rc:?} meta: {e}")))?;
                let (stud_chunks, stud_ground) = load_studs(&mut tx, rc, stud_ground).await?;
                let rows = sqlx::query("SELECT bundle FROM entities WHERE rx = $1 AND rz = $2")
                    .bind(rc.x as i64)
                    .bind(rc.z as i64)
                    .fetch_all(&mut *tx)
                    .await?;
                tx.commit().await?;
                let mut entities = Vec::with_capacity(rows.len());
                for r in rows {
                    let blob: Vec<u8> = r.try_get(0)?;
                    let bundle: EntityBundle = decode_entity_bundle(&blob)
                        .map_err(|e| StoreError::Corrupt(format!("region {rc:?} entity: {e}")))?;
                    entities.push(bundle);
                }
                entities.sort_by_key(|b| b.id);
                Ok(Some(StoredRegion { meta, entities, stud_chunks, stud_ground }))
            }

            pub(crate) async fn load_terrain(pool: &Pool, rc: RegionCoords) -> Result<Option<TerrainSnapshot>, StoreError> {
                let mut tx = begin_read(pool).await?;
                let Some((_, stud_ground)) = region_row(&mut tx, rc).await? else {
                    tx.commit().await?;
                    return Ok(None);
                };
                // Only the edge columns, which is all a neighbour's
                // halo seed reads (`game::stud_halo`).
                let (stud_chunks, stud_ground) = load_studs(&mut tx, rc, stud_ground).await?;
                tx.commit().await?;
                let stud = game::StudEdge::from_stored(
                    &stud_ground,
                    stud_chunks.into_iter().map(|(c, record)| match record {
                        StudChunkRecord::Pristine => (c, None),
                        StudChunkRecord::Edited(chunk) => (c, Some(chunk)),
                    }),
                );
                Ok(Some(TerrainSnapshot { stud }))
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

            /// The in-flight rows of host `host` (`None`: of every host), in order.
            pub(crate) async fn load_in_flight(pool: &Pool, host: Option<u32>) -> Result<Vec<InFlightRow>, StoreError> {
                let rows = sqlx::query(
                    "SELECT seq, kind, target_rx, target_rz, stamp, payload FROM in_flight \
                     WHERE $1 IS NULL OR host = $1 ORDER BY seq",
                )
                .bind(host.map(i64::from))
                .fetch_all(pool)
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
                            bundle: decode_entity_bundle(&payload).map_err(bad)?,
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
                        decode_player_record(&blob)
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
                // generation under `FOR UPDATE`.
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
                // Every region's entities go first, so an entity that moved between two
                // regions of the cut is not seen twice.
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
                    if region.stud_ground.is_none() {
                        // `None` = "unchanged since the stored ground": a region's first
                        // row must carry its ground, or it would be stored without one.
                        let known = sqlx::query("SELECT 1 FROM regions WHERE rx = $1 AND rz = $2")
                            .bind(rx)
                            .bind(rz)
                            .fetch_optional(&mut *conn)
                            .await?;
                        if known.is_none() {
                            return Err(StoreError::Corrupt(format!(
                                "region {rc:?}'s first save carries no ground (the store would hold it without one); \
                                 the cut was not committed"
                            )));
                        }
                    }
                    let bump = i64::from(!region.stud_chunks.is_empty());
                    let version: i64 = sqlx::query(
                        "INSERT INTO regions (rx, rz, saved_tick, terrain_version, meta, stud_ground) \
                         VALUES ($1, $2, $3, $4, $5, $6) \
                         ON CONFLICT (rx, rz) DO UPDATE SET \
                           saved_tick = excluded.saved_tick, \
                           terrain_version = regions.terrain_version + excluded.terrain_version, \
                           meta = excluded.meta, \
                           stud_ground = CASE WHEN $7 = 1 THEN excluded.stud_ground ELSE regions.stud_ground END \
                         RETURNING terrain_version",
                    )
                    .bind(rx)
                    .bind(rz)
                    .bind(t)
                    .bind(bump)
                    .bind(encode(FORMAT_REGION_META, &region.meta))
                    .bind(stud_ground_blob(region.stud_ground.as_ref()))
                    .bind(i64::from(region.stud_ground.is_some()))
                    .fetch_one(&mut *conn)
                    .await?
                    .try_get(0)?;
                    versions.push((*rc, u(version)));
                    // Stud rows: a record is written, `None` deletes the row (the chunk
                    // is what its recipe gives again).
                    for (c, record) in &region.stud_chunks {
                        let (cx, cy, cz) = (c.x as i64, c.y as i64, c.z as i64);
                        match record {
                            Some(record) => {
                                sqlx::query(
                                    "INSERT INTO stud_chunks (rx, rz, cx, cy, cz, data) VALUES ($1, $2, $3, $4, $5, $6) \
                                     ON CONFLICT (rx, rz, cx, cy, cz) DO UPDATE SET data = excluded.data",
                                )
                                .bind(rx)
                                .bind(rz)
                                .bind(cx)
                                .bind(cy)
                                .bind(cz)
                                .bind(encode(FORMAT_STUD_CHUNK, record))
                                .execute(&mut *conn)
                                .await?;
                            }
                            None => {
                                sqlx::query(
                                    "DELETE FROM stud_chunks WHERE rx = $1 AND rz = $2 AND cx = $3 AND cy = $4 AND cz = $5",
                                )
                                .bind(rx)
                                .bind(rz)
                                .bind(cx)
                                .bind(cy)
                                .bind(cz)
                                .execute(&mut *conn)
                                .await?;
                            }
                        }
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
engine_rows!(sqlite, sqlx::Sqlite, None, "", "", false);
engine_rows!(pg, sqlx::Postgres, Some("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ"), " FOR SHARE", " FOR UPDATE", true);
