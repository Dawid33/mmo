//! The two encodings of a [`PrefabFile`]: RON text and a compressed binary.

use bincode::Options;
use ron::ser::PrettyConfig;

use super::file::{PREFAB_FORMAT, PrefabError, PrefabFile};

pub const BIN_MAGIC: &[u8; 7] = b"MMOPF2\n";
pub const RON_SUFFIX: &str = ".prefab.ron";
pub const BIN_SUFFIX: &str = ".prefab.bin";

/// lz4 expands at most this much per compressed byte.
const LZ4_MAX_RATIO: u64 = 255;
const LZ4_SLACK: u64 = 64;

pub fn prefab_to_ron(file: &PrefabFile) -> String {
    let mut s =
        ron::ser::to_string_pretty(file, PrettyConfig::new()).expect("a prefab file serializes");
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

pub fn prefab_from_ron(src: &str) -> Result<PrefabFile, PrefabError> {
    let file: PrefabFile = ron::from_str(src).map_err(|e| PrefabError::Syntax(e.to_string()))?;
    check_format(file)
}

fn check_format(file: PrefabFile) -> Result<PrefabFile, PrefabError> {
    if file.format == PREFAB_FORMAT {
        Ok(file)
    } else {
        Err(PrefabError::NotAPrefab)
    }
}

fn bin_options(limit: u64) -> impl Options {
    bincode::options()
        .with_fixint_encoding()
        .with_limit(limit)
        .reject_trailing_bytes()
}

pub fn prefab_to_bin(file: &PrefabFile) -> Vec<u8> {
    let raw = bin_options(u64::MAX)
        .serialize(file)
        .expect("a prefab file serializes");
    let packed = lz4_flex::block::compress(&raw);
    let mut out = Vec::with_capacity(BIN_MAGIC.len() + 8 + packed.len());
    out.extend_from_slice(BIN_MAGIC);
    out.extend_from_slice(&(raw.len() as u64).to_le_bytes());
    out.extend_from_slice(&packed);
    out
}

/// `max_bytes`: the largest decompressed length accepted (None = no limit).
pub fn prefab_from_bin(bytes: &[u8], max_bytes: Option<usize>) -> Result<PrefabFile, PrefabError> {
    let rest = bytes
        .strip_prefix(BIN_MAGIC.as_slice())
        .ok_or(PrefabError::NotAPrefab)?;
    let (len_bytes, packed) = rest.split_at_checked(8).ok_or(PrefabError::Corrupt)?;
    let declared = u64::from_le_bytes(len_bytes.try_into().map_err(|_| PrefabError::Corrupt)?);
    if let Some(max) = max_bytes {
        if declared > max as u64 {
            return Err(PrefabError::TooLarge);
        }
    }
    let reachable = LZ4_MAX_RATIO
        .saturating_mul(packed.len() as u64)
        .saturating_add(LZ4_SLACK);
    if declared > reachable {
        return Err(PrefabError::Corrupt);
    }
    let declared = usize::try_from(declared).map_err(|_| PrefabError::Corrupt)?;
    let raw = lz4_flex::block::decompress(packed, declared).map_err(|_| PrefabError::Corrupt)?;
    if raw.len() != declared {
        return Err(PrefabError::Corrupt);
    }
    let file: PrefabFile = bin_options(declared as u64)
        .deserialize(&raw)
        .map_err(|_| PrefabError::Corrupt)?;
    check_format(file)
}

/// Picks the decoder by suffix; `None` for a file name that is not a prefab's.
pub fn prefab_from_named_bytes(
    file_name: &str,
    bytes: &[u8],
    max_bytes: Option<usize>,
) -> Option<Result<PrefabFile, PrefabError>> {
    if file_name.ends_with(RON_SUFFIX) {
        Some(match std::str::from_utf8(bytes) {
            Ok(s) => prefab_from_ron(s),
            Err(_) => Err(PrefabError::NotAPrefab),
        })
    } else if file_name.ends_with(BIN_SUFFIX) {
        Some(prefab_from_bin(bytes, max_bytes))
    } else {
        None
    }
}
