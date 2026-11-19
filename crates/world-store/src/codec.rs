//! Row codecs: every stored blob is `[format byte][zstd(bincode(value))]`.
//! The format byte is per stored type (`FORMAT_*`); a change to a stored
//! type's layout bumps its constant.

use serde::{Serialize, de::DeserializeOwned};

use crate::StoreError;

pub const FORMAT_REGION_META: u8 = 1;
pub const FORMAT_CHUNK: u8 = 1;
pub const FORMAT_COLUMN_GROUND: u8 = 1;
pub const FORMAT_ENTITY_BUNDLE: u8 = 1;
/// 2: `PlayerRecord::fortress` gone.
pub const FORMAT_PLAYER_RECORD: u8 = 2;
pub const FORMAT_INTENT_MSG: u8 = 1;
/// `regions.ground`: a region's `Vec<(ColumnCoords, ColumnGround)>`.
pub const FORMAT_REGION_GROUND: u8 = 1;

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
    let (&format, body) = bytes.split_first().ok_or_else(|| StoreError::Corrupt(format!("{name}: empty blob")))?;
    if format != expected {
        return Err(StoreError::Corrupt(format!("{name}: format byte {format}, expected {expected}")));
    }
    let mut raw = Vec::new();
    zstd::stream::copy_decode(body, &mut raw).map_err(|e| StoreError::Corrupt(format!("{name}: zstd: {e}")))?;
    bincode::deserialize(&raw).map_err(|e| StoreError::Corrupt(format!("{name}: bincode: {e}")))
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
                write!(f, "row format {format}, this build reads format {FORMAT_WORLD_MAP}")
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
    let Some(&format) = bytes.first() else { return Err(WorldMapRowError::Damaged("empty row".into())) };
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
    zstd::stream::copy_decode(body, &mut raw).map_err(|e| WorldMapRowError::Damaged(format!("zstd: {e}")))?;
    bincode::deserialize(&raw).map_err(|e| WorldMapRowError::Damaged(format!("bincode: {e}")))
}
