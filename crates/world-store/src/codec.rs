//! Row codecs: every stored blob is `[format byte][zstd(bincode(value))]`.
//! The format byte is per stored type (`FORMAT_*`); a change to a stored
//! type's layout bumps its constant, and the golden blobs in
//! `tests/fixtures/` pin the current encoding (see `tests/codec.rs`).

use serde::{Serialize, de::DeserializeOwned};

use crate::StoreError;

/// `regions.meta`: a `game::RegionMeta`. Format 2 dropped the voxel world's water
/// bodies, format 3 stores designations by box of stud cells instead of by tile;
/// format 1 and 2 rows are still read ([`decode_region_meta`]).
pub const FORMAT_REGION_META: u8 = 3;
/// The region meta layout with designations by tile (`game::RegionMetaV2`).
pub const FORMAT_REGION_META_V2: u8 = 2;
/// The region meta layout with (empty) voxel water bodies (`game::RegionMetaV1`).
pub const FORMAT_REGION_META_V1: u8 = 1;
/// `entities.bundle` and in-flight transfers: a `game::EntityBundle`. Format 2 added
/// `Carried::stud_body`, format 3 dropped the voxel body slot; formats 1 and 2 are
/// still read ([`decode_entity_bundle`]).
pub const FORMAT_ENTITY_BUNDLE: u8 = 3;
/// The entity bundle layout with the (empty) voxel body slot (`game::EntityBundleV2`).
pub const FORMAT_ENTITY_BUNDLE_V2: u8 = 2;
/// The entity bundle layout before stud bodies (`game::EntityBundleV1`).
pub const FORMAT_ENTITY_BUNDLE_V1: u8 = 1;
pub const FORMAT_PLAYER_RECORD: u8 = 1;
pub const FORMAT_INTENT_MSG: u8 = 1;
/// `stud_chunks.data`: a `game::StudChunkRecord`.
pub const FORMAT_STUD_CHUNK: u8 = 1;
/// `regions.stud_ground`: a region's `Vec<(ColumnCoords, StudColumnGround)>`. Format 2
/// added the recipes' apron (the surfaces just outside each chunk column's side
/// faces); format 1 rows are still read ([`decode_stud_ground`]).
pub const FORMAT_STUD_GROUND: u8 = 2;
/// The recipe layout before the apron (`game::StudColumnGroundV1`).
pub const FORMAT_STUD_GROUND_V1: u8 = 1;

/// `forged_world_map.bytes`: the forged world's `game::WorldMap`, framed by
/// [`encode_world_map`] (not [`encode`]): format 1 was the plain
/// `[format][zstd(bincode)]` blob, format 2 adds the map's
/// `worldgen::tile_map::WORLD_MAP_VERSION` and a crc32.
pub const FORMAT_WORLD_MAP: u8 = 2;

/// zstd level of every stored blob.
pub const ZSTD_LEVEL: i32 = 3;

/// `format` byte + zstd(bincode(`v`)).
pub fn encode<T: Serialize>(format: u8, v: &T) -> Vec<u8> {
    let raw = bincode::serialize(v).expect("bincode serializes a stored type");
    let body = zstd::bulk::compress(&raw, ZSTD_LEVEL).expect("zstd compresses in memory");
    let mut out = Vec::with_capacity(body.len() + 1);
    out.push(format);
    out.extend_from_slice(&body);
    out
}

/// The inverse of [`encode`]; a wrong format byte or a bad body is `Corrupt`.
pub fn decode<T: DeserializeOwned>(expected: u8, bytes: &[u8]) -> Result<T, StoreError> {
    let name = std::any::type_name::<T>();
    let (&format, body) = bytes.split_first().ok_or_else(|| StoreError::Corrupt(format!("{name}: empty blob")))?;
    if format != expected {
        return Err(StoreError::Corrupt(format!("{name}: format byte {format}, expected {expected}")));
    }
    let mut raw = Vec::new();
    zstd::stream::copy_decode(body, &mut raw).map_err(|e| StoreError::Corrupt(format!("{name}: zstd: {e}")))?;
    bincode::deserialize(&raw).map_err(|e| StoreError::Corrupt(format!("{name}: bincode: {e}")))
}

/// A stored entity bundle of any format: the current one, or format 2 (the voxel
/// body slot, `game::EntityBundleV2`) or 1 (no stud body, `game::EntityBundleV1`)
/// converted. Rows are rewritten in the current format by the next save that writes
/// them. An old row that holds a voxel body does not decode (`Corrupt`): voxel
/// worlds are refused at open.
pub fn decode_entity_bundle(bytes: &[u8]) -> Result<game::EntityBundle, StoreError> {
    match bytes.first() {
        Some(&FORMAT_ENTITY_BUNDLE_V1) => {
            decode::<game::EntityBundleV1>(FORMAT_ENTITY_BUNDLE_V1, bytes).map(game::EntityBundle::from)
        }
        Some(&FORMAT_ENTITY_BUNDLE_V2) => {
            decode::<game::EntityBundleV2>(FORMAT_ENTITY_BUNDLE_V2, bytes).map(game::EntityBundle::from)
        }
        _ => decode(FORMAT_ENTITY_BUNDLE, bytes),
    }
}

/// A region's stored recipes of any format: the current one, or format 1
/// (`game::StudColumnGroundV1`, no apron) converted — each recipe's apron is taken from
/// the neighbouring chunk columns of the same row (`game::link_stud_aprons`) and stays
/// closed along the region's edge (no cliff bricks, no wall layers derived there). The
/// row is written once per region, so a format 1 row is read this way every time.
pub fn decode_stud_ground(bytes: &[u8]) -> Result<Vec<(game::ColumnCoords, game::StudColumnGround)>, StoreError> {
    match bytes.first() {
        Some(&FORMAT_STUD_GROUND_V1) => {
            let old: Vec<(game::ColumnCoords, game::StudColumnGroundV1)> = decode(FORMAT_STUD_GROUND_V1, bytes)?;
            let mut ground: Vec<_> = old.into_iter().map(|(c, g)| (c, game::StudColumnGround::from(g))).collect();
            game::link_stud_aprons(&mut ground);
            Ok(ground)
        }
        _ => decode(FORMAT_STUD_GROUND, bytes),
    }
}

/// A stored region meta of any format: the current one, format 2
/// (`game::RegionMetaV2`: each tile designation becomes its cell box) or format 1
/// (`game::RegionMetaV1`, whose voxel water bodies must be empty) converted.
pub fn decode_region_meta(bytes: &[u8]) -> Result<game::RegionMeta, StoreError> {
    match bytes.first() {
        Some(&FORMAT_REGION_META_V1) => {
            decode::<game::RegionMetaV1>(FORMAT_REGION_META_V1, bytes).map(game::RegionMeta::from)
        }
        Some(&FORMAT_REGION_META_V2) => {
            decode::<game::RegionMetaV2>(FORMAT_REGION_META_V2, bytes).map(game::RegionMeta::from)
        }
        _ => decode(FORMAT_REGION_META, bytes),
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
