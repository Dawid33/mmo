//! The row SQL, written once and instantiated per engine (`sqlite`, `pg`):
//! runtime `sqlx::query` only, `$N` placeholders (SQLite reads them too).
//! u64 values (ticks, serials, player ids) are stored as i64 by bit cast.
//!
//! Writes go through the store thread's writer lock (`thread.rs`), so two
//! write transactions never race on SQLite's deferred upgrade.

use std::collections::BTreeMap;

use game::{
    Chunk, ChunkCoords, ColumnCoords, ColumnGround, EntityBundle, EntityId, EntityKind, InFlightRow, IntentMsg,
    PlayerId, PlayerRecord, RegionCoords, RegionMeta, StoredRegion, TerrainSnapshot, Tick, WorldCut,
};

use crate::codec::*;
use crate::forged::{ForgedHead, TiledRows};
use crate::{CutCommitted, StoreError, WorldInfo};

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

fn chunk_of(rc: RegionCoords, cx: i64, cy: i64, cz: i64) -> Result<ChunkCoords, StoreError> {
    let bad = || StoreError::Corrupt(format!("region {rc:?}: chunk ({cx}, {cy}, {cz}) out of range"));
    Ok(ChunkCoords::new(
        usize::try_from(cx).map_err(|_| bad())?,
        i32::try_from(cy).map_err(|_| bad())?,
        usize::try_from(cz).map_err(|_| bad())?,
    ))
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

fn ground_blob(ground: Option<&Vec<(ColumnCoords, ColumnGround)>>) -> Vec<u8> {
    match ground {
        Some(g) => encode(FORMAT_REGION_GROUND, g),
        None => encode(FORMAT_REGION_GROUND, &Vec::<(ColumnCoords, ColumnGround)>::new()),
    }
}

macro_rules! engine_rows {
    ($module:ident, $db:ty, $read_prelude:expr) => {
        pub(crate) mod $module {
            use sqlx::Row;

            use super::*;

            type Pool = sqlx::Pool<$db>;
            type Tx = sqlx::Transaction<'static, $db>;

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

            pub(crate) async fn load_region(pool: &Pool, rc: RegionCoords) -> Result<Option<StoredRegion>, StoreError> {
                let mut tx = begin_read(pool).await?;
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

            pub(crate) async fn load_in_flight(pool: &Pool) -> Result<Vec<InFlightRow>, StoreError> {
                let rows = sqlx::query(
                    "SELECT seq, kind, target_rx, target_rz, stamp, payload FROM in_flight ORDER BY seq",
                )
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

            /// The whole cut in one transaction; any error rolls all of it back.
            pub(crate) async fn commit_cut(pool: &Pool, cut: &WorldCut) -> Result<CutCommitted, StoreError> {
                let t = i(cut.tick);
                let mut tx = pool.begin().await?;
                // Every region's entities go first, so an entity that moved between two
                // regions of the cut is not seen twice.
                for (rc, _) in &cut.regions {
                    sqlx::query("DELETE FROM entities WHERE rx = $1 AND rz = $2")
                        .bind(rc.x as i64)
                        .bind(rc.z as i64)
                        .execute(&mut *tx)
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
                            .fetch_optional(&mut *tx)
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
                    .fetch_one(&mut *tx)
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
                        .execute(&mut *tx)
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
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| entity_insert_error(e, b.id, *rc, cut.tick))?;
                    }
                }
                for (id, record) in &cut.players {
                    let done = sqlx::query("UPDATE players SET record = $1, saved_tick = $2 WHERE player_id = $3")
                        .bind(encode(FORMAT_PLAYER_RECORD, record))
                        .bind(t)
                        .bind(i(id.0))
                        .execute(&mut *tx)
                        .await?;
                    if done.rows_affected() == 0 {
                        return Err(StoreError::Corrupt(format!(
                            "player {} has no token row (ids come only from resolve_player); the cut was not committed",
                            id.0
                        )));
                    }
                }
                sqlx::query("DELETE FROM in_flight WHERE host = $1").bind(cut.host as i64).execute(&mut *tx).await?;
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
                    .execute(&mut *tx)
                    .await?;
                }
                if let Some(bytes) = &cut.history {
                    sqlx::query(
                        "INSERT INTO history (id, bytes) VALUES (1, $1) \
                         ON CONFLICT (id) DO UPDATE SET bytes = excluded.bytes",
                    )
                    .bind(bytes.as_slice())
                    .execute(&mut *tx)
                    .await?;
                }
                sqlx::query("UPDATE world SET world_tick = $1 WHERE id = 1 AND world_tick < $1").bind(t).execute(&mut *tx).await?;
                tx.commit().await?;
                Ok(CutCommitted { id: cut.id, versions })
            }
        }
    };
}

engine_rows!(sqlite, sqlx::Sqlite, None);
engine_rows!(pg, sqlx::Postgres, Some("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ"));
