//! Prefab text forms: the RON file and the export string.
//!
//! An export string is `MMOPF1:` + base64(lz4 with a prepended size(bincode(file form))).
//! It comes from other people, so decoding checks every size before it allocates.

use base64::Engine;
use bincode::Options;

use super::{PrefabFile, Problem};

/// What every export string starts with (the `1` is the string format).
pub const EXPORT_PREFIX: &str = "MMOPF1:";
/// Longer strings are refused before any decoding.
pub const EXPORT_MAX_CHARS: usize = 1 << 20;
/// A larger declared decompressed size is refused before decompressing.
pub const EXPORT_MAX_BYTES: usize = 4 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefabError {
    /// The RON does not parse as a prefab.
    Syntax(String),
    /// Not an export string at all (no prefix).
    NotAPrefabString,
    /// Over a size limit.
    TooLarge,
    /// An export string that does not decode.
    Corrupt,
    /// It parses, and is not a valid prefab.
    Problems(Vec<Problem>),
    Io(String),
}

impl std::fmt::Display for PrefabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrefabError::Syntax(e) => write!(f, "not a prefab file: {e}"),
            PrefabError::NotAPrefabString => write!(f, "not a prefab string (it must start with {EXPORT_PREFIX})"),
            PrefabError::TooLarge => write!(f, "the prefab string is too large"),
            PrefabError::Corrupt => write!(f, "the prefab string is damaged"),
            PrefabError::Problems(p) => {
                write!(f, "invalid prefab: ")?;
                for (i, problem) in p.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{problem}")?;
                }
                Ok(())
            }
            PrefabError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PrefabError {}

/// The file text: pretty RON in field order, with a trailing newline. Stable: writing
/// what [`prefab_from_ron`] read gives the same bytes.
pub fn prefab_to_ron(file: &PrefabFile) -> String {
    let mut s = ron::ser::to_string_pretty(file, ron::ser::PrettyConfig::default())
        .expect("a prefab file always serializes");
    s.push('\n');
    s
}

pub fn prefab_from_ron(src: &str) -> Result<PrefabFile, PrefabError> {
    ron::from_str(src).map_err(|e| PrefabError::Syntax(e.to_string()))
}

/// Fixed-width bincode that refuses to read or allocate past the size limit.
fn export_bincode() -> impl Options {
    bincode::DefaultOptions::new().with_fixint_encoding().with_limit(EXPORT_MAX_BYTES as u64)
}

pub fn prefab_to_export(file: &PrefabFile) -> String {
    // No size limit on the way out: a prefab too large to import is refused there.
    let raw = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .serialize(file)
        .expect("a prefab file always serializes");
    let packed = lz4_flex::block::compress_prepend_size(&raw);
    format!("{EXPORT_PREFIX}{}", base64::engine::general_purpose::STANDARD.encode(packed))
}

/// Decodes an export string (surrounding whitespace ignored). Never panics; memory is
/// bounded by [`EXPORT_MAX_CHARS`] and [`EXPORT_MAX_BYTES`].
pub fn prefab_from_export(s: &str) -> Result<PrefabFile, PrefabError> {
    let body = s.trim().strip_prefix(EXPORT_PREFIX).ok_or(PrefabError::NotAPrefabString)?;
    if body.len() > EXPORT_MAX_CHARS {
        return Err(PrefabError::TooLarge);
    }
    let packed = base64::engine::general_purpose::STANDARD.decode(body).map_err(|_| PrefabError::Corrupt)?;
    let (header, block) = packed.split_first_chunk::<4>().ok_or(PrefabError::Corrupt)?;
    let size = u32::from_le_bytes(*header) as usize;
    if size > EXPORT_MAX_BYTES {
        return Err(PrefabError::TooLarge);
    }
    let raw = lz4_flex::block::decompress(block, size).map_err(|_| PrefabError::Corrupt)?;
    export_bincode().deserialize(&raw).map_err(|_| PrefabError::Corrupt)
}
