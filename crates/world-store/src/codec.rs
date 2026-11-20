//! Row codecs: every stored blob is `[format byte][zstd(bincode(value))]`.
//! The format byte is per stored type (`FORMAT_*`); a change to a stored
//! type's layout bumps its constant.

use serde::{Serialize, de::DeserializeOwned};

use crate::StoreError;

pub const FORMAT_REGION_META: u8 = 1;
pub const FORMAT_CHUNK: u8 = 1;
pub const FORMAT_COLUMN_GROUND: u8 = 1;
/// 2: the player `Client` carries authoritative editor capability/state.
pub const FORMAT_ENTITY_BUNDLE: u8 = 2;
/// 2: `PlayerRecord::fortress` gone. 3: authoritative editor capability/state on `Client`.
pub const FORMAT_PLAYER_RECORD: u8 = 3;
pub const FORMAT_INTENT_MSG: u8 = 1;
/// `regions.ground`: a region's `Vec<(ColumnCoords, ColumnGround)>`.
pub const FORMAT_REGION_GROUND: u8 = 1;
/// `regions.tree_state`: the complete `game::RegionTrees` at this cut tick.
pub const FORMAT_REGION_TREES: u8 = 1;
/// `regions.water_state`: 2 adds sparse absolute-zero overrides and persisted
/// bounded scheduler state. Format 1 is decoded and migrated by
/// [`decode_region_water`].
pub const FORMAT_REGION_WATER: u8 = 2;
/// `chunks.top`: the chunk's `game::map_tile::ChunkTop`, written beside `data`.
pub const FORMAT_CHUNK_TOP: u8 = 1;

/// `forged_world_map.bytes`: the forged world's `game::WorldMap`, framed by
/// [`encode_world_map`] (not [`encode`]): format 1 was the plain
/// `[format][zstd(bincode)]` blob, format 2 adds the map's
/// `worldgen::tile_map::WORLD_MAP_VERSION` and a crc32.
pub const FORMAT_WORLD_MAP: u8 = 2;

/// zstd level of every stored blob.
pub const ZSTD_LEVEL: i32 = 3;

/// `format` byte + zstd(bincode(`v`)).
pub fn encode<T: Serialize>(format: u8, v: &T) -> Vec<u8> {
    let _cpu = crate::counters::CpuSpan::start();
    let raw = bincode::serialize(v).expect("bincode serializes a stored type");
    let body = zstd::bulk::compress(&raw, ZSTD_LEVEL).expect("zstd compresses in memory");
    let mut out = Vec::with_capacity(body.len() + 1);
    out.push(format);
    out.extend_from_slice(&body);
    out
}

/// The inverse of [`encode`]; a wrong format byte or a bad body is `Corrupt`.
pub fn decode<T: DeserializeOwned>(expected: u8, bytes: &[u8]) -> Result<T, StoreError> {
    let _cpu = crate::counters::CpuSpan::start();
    let name = std::any::type_name::<T>();
    let (&format, body) = bytes
        .split_first()
        .ok_or_else(|| StoreError::Corrupt(format!("{name}: empty blob")))?;
    if format != expected {
        return Err(StoreError::Corrupt(format!(
            "{name}: format byte {format}, expected {expected}"
        )));
    }
    let mut raw = Vec::new();
    zstd::stream::copy_decode(body, &mut raw)
        .map_err(|e| StoreError::Corrupt(format!("{name}: zstd: {e}")))?;
    bincode::deserialize(&raw).map_err(|e| StoreError::Corrupt(format!("{name}: bincode: {e}")))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct WaterRegionV1 {
    activated: std::collections::BTreeMap<game::WorldChunkPos, ()>,
    cells: std::collections::BTreeMap<game::WorldBlockPos, u16>,
}

/// Decode current water state, or migrate format 1's "activated means absent
/// is dry" chunks to the format-2 absolute-override interpretation.
pub fn decode_region_water(
    region: game::RegionCoords,
    ground: &[(game::ColumnCoords, game::ColumnGround)],
    bytes: &[u8],
) -> Result<game::WaterRegion, StoreError> {
    let Some(&format) = bytes.first() else {
        return Err(StoreError::Corrupt("game::WaterRegion: empty blob".into()));
    };
    if format == FORMAT_REGION_WATER {
        return decode(FORMAT_REGION_WATER, bytes);
    }
    if format != 1 {
        return Err(StoreError::Corrupt(format!(
            "game::WaterRegion: format byte {format}, expected 1 or {FORMAT_REGION_WATER}"
        )));
    }

    let old: WaterRegionV1 = decode(1, bytes)?;
    let ground: std::collections::BTreeMap<_, _> = ground
        .iter()
        .map(|(coords, column)| (coords, column))
        .collect();
    let mut cells = old.cells;
    let cb = game::CHUNK_BLOCKS;
    for chunk in old.activated.keys() {
        let (owner, local) = chunk.to_local();
        if owner != region {
            return Err(StoreError::Corrupt(format!(
                "region {region:?} water activation belongs to {owner:?}"
            )));
        }
        let Some(column) = ground.get(&game::ColumnCoords::new(local.x, local.z)) else {
            continue;
        };
        for by in 0..cb {
            let y = chunk.y * cb as i32 + by as i32;
            for bx in 0..cb {
                for bz in 0..cb {
                    let units = game::static_water_units(
                        game::BlockId::AIR,
                        column.recipe_water(bx, bz),
                        y,
                    );
                    if units != 0 {
                        let pos = game::WorldBlockPos::new(
                            chunk.x * cb as i64 + bx as i64,
                            y,
                            chunk.z * cb as i64 + bz as i64,
                        );
                        cells.entry(pos).or_insert(0);
                    }
                }
            }
        }
    }
    let mut volume_delta = 0_i64;
    for (&pos, &units) in &cells {
        let (owner, local) = pos.to_local();
        if owner != region {
            return Err(StoreError::Corrupt(format!(
                "region {region:?} water override belongs to {owner:?}"
            )));
        }
        let column = ground.get(&game::ColumnCoords::new(
            (local.x / cb as i32) as usize,
            (local.z / cb as i32) as usize,
        ));
        let recipe = column
            .and_then(|g| {
                g.recipe_water(
                    local.x.rem_euclid(cb as i32) as usize,
                    local.z.rem_euclid(cb as i32) as usize,
                )
            })
            .is_some_and(|(lo, hi)| (lo..=hi).contains(&local.y));
        let baseline = if recipe {
            i64::from(game::WATER_FULL)
        } else {
            0
        };
        volume_delta = volume_delta
            .checked_add(i64::from(units) - baseline)
            .ok_or_else(|| StoreError::Corrupt("water volume delta overflow".into()))?;
    }
    Ok(game::WaterRegion {
        activated: old.activated,
        cells,
        scheduler: game::WaterScheduler::default(),
        volume_delta,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: 18 activated regions in the debug world's pre-bounded
    /// format-1 water blobs used absence to mean dry; the v2 decoder must
    /// materialise only those zero overrides at load, not refill recipe water.
    #[test]
    fn migrates_format_one_absent_cells_to_zero_overrides() {
        let region = game::RegionCoords::new(0, 0);
        let mut ground = game::ColumnGround::plain(
            vec![0; game::COLUMN_FOOTPRINT].into_boxed_slice(),
            0,
            game::BlockId(2),
            game::BlockId(6),
        );
        ground.water_top.fill(1);
        let chunk = game::WorldChunkPos::new(0, 0, 0);
        let wet = game::WorldBlockPos::new(0, 1, 0);
        let old = WaterRegionV1 {
            activated: [(chunk, ())].into(),
            cells: [(wet, 1024)].into(),
        };
        let bytes = encode(1, &old);

        let water = decode_region_water(region, &[(game::ColumnCoords::new(0, 0), ground)], &bytes)
            .expect("format-one water migrates");
        assert_eq!(water.cells[&wet], 1024);
        assert_eq!(
            water.cells.get(&game::WorldBlockPos::new(1, 1, 0)),
            Some(&0)
        );
        assert_eq!(water.cells.len(), game::COLUMN_FOOTPRINT);
        assert_eq!(
            water.volume_delta,
            i64::from(1024) - i64::from(game::WATER_FULL) * game::COLUMN_FOOTPRINT as i64
        );
        assert!(!water.scheduler.bootstrap_complete);
    }
}

/// Bytes of the world-map row before its zstd body: the format byte, the
/// `WORLD_MAP_VERSION` (u32 LE) and the crc32 of the body (u32 LE).
const WORLD_MAP_HEADER: usize = 9;

/// The `forged_world_map` row: `[FORMAT_WORLD_MAP][WORLD_MAP_VERSION u32
/// LE][crc32(body) u32 LE][body = zstd(bincode(map))]`. The crc makes a
/// damaged row a load error, never a silently wrong map (the tiles' framing);
/// the version says which `build_world_map` made it.
pub fn encode_world_map(map: &game::WorldMap) -> Vec<u8> {
    let _cpu = crate::counters::CpuSpan::start();
    let raw = bincode::serialize(map).expect("bincode serializes a WorldMap");
    let body = zstd::bulk::compress(&raw, ZSTD_LEVEL).expect("zstd compresses in memory");
    let mut out = Vec::with_capacity(WORLD_MAP_HEADER + body.len());
    out.push(FORMAT_WORLD_MAP);
    out.extend_from_slice(&worldgen::tile_map::WORLD_MAP_VERSION.to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(&body).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Why a stored world-map row is not served ([`decode_world_map`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldMapRowError {
    /// Another row layout (format byte), e.g. format 1's unversioned blob.
    Layout { format: u8 },
    /// Built by another `WORLD_MAP_VERSION` of `build_world_map`.
    Version { found: u32 },
    /// The crc or the body does not check out.
    Damaged(String),
}

impl std::fmt::Display for WorldMapRowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorldMapRowError::Layout { format } => {
                write!(
                    f,
                    "row format {format}, this build reads format {FORMAT_WORLD_MAP}"
                )
            }
            WorldMapRowError::Version { found } => write!(
                f,
                "built by world-map version {found}, this build builds version {}",
                worldgen::tile_map::WORLD_MAP_VERSION
            ),
            WorldMapRowError::Damaged(why) => write!(f, "damaged: {why}"),
        }
    }
}

/// The inverse of [`encode_world_map`].
pub fn decode_world_map(bytes: &[u8]) -> Result<game::WorldMap, WorldMapRowError> {
    let _cpu = crate::counters::CpuSpan::start();
    let Some(&format) = bytes.first() else {
        return Err(WorldMapRowError::Damaged("empty row".into()));
    };
    if format != FORMAT_WORLD_MAP {
        return Err(WorldMapRowError::Layout { format });
    }
    if bytes.len() < WORLD_MAP_HEADER {
        return Err(WorldMapRowError::Damaged("shorter than its header".into()));
    }
    let version = u32::from_le_bytes(bytes[1..5].try_into().unwrap());
    if version != worldgen::tile_map::WORLD_MAP_VERSION {
        return Err(WorldMapRowError::Version { found: version });
    }
    let crc = u32::from_le_bytes(bytes[5..9].try_into().unwrap());
    let body = &bytes[WORLD_MAP_HEADER..];
    if crc32fast::hash(body) != crc {
        return Err(WorldMapRowError::Damaged("crc32 mismatch".into()));
    }
    let mut raw = Vec::new();
    zstd::stream::copy_decode(body, &mut raw)
        .map_err(|e| WorldMapRowError::Damaged(format!("zstd: {e}")))?;
    bincode::deserialize(&raw).map_err(|e| WorldMapRowError::Damaged(format!("bincode: {e}")))
}
