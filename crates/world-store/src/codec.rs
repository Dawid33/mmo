//! Row codecs: every stored blob is `[format byte][zstd(bincode(value))]`.
//! The format byte is per stored type (`FORMAT_*`); a change to a stored
//! type's layout bumps its constant, and the golden blobs in
//! `tests/fixtures/` pin the current encoding (see `tests/codec.rs`).

use serde::{Serialize, de::DeserializeOwned};

use crate::StoreError;

pub const FORMAT_REGION_META: u8 = 1;
pub const FORMAT_CHUNK: u8 = 1;
pub const FORMAT_COLUMN_GROUND: u8 = 1;
pub const FORMAT_ENTITY_BUNDLE: u8 = 1;
pub const FORMAT_PLAYER_RECORD: u8 = 1;
pub const FORMAT_INTENT_MSG: u8 = 1;
/// `regions.ground`: a region's `Vec<(ColumnCoords, ColumnGround)>`.
pub const FORMAT_REGION_GROUND: u8 = 1;

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
