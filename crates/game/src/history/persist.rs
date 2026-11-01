//! The history file: magic, version, world id, then the lz4-compressed bincode of a [`History`].

use super::{History, SocietyGrid};
use std::fmt;
use std::path::{Path, PathBuf};

/// 2: `Person::home_location` and `Journey::held_in` (region hand-over).
pub const HISTORY_FILE_VERSION: u32 = 2;
pub const HISTORY_MAGIC: [u8; 8] = *b"MMOHIST\0";

/// Largest decompressed history body a file may claim; a bigger size prefix is refused
/// before anything is allocated.
pub const MAX_HISTORY_BYTES: usize = 512 << 20;

const HEADER_LEN: usize = 8 + 4 + 4;

#[derive(Debug)]
pub enum HistoryLoadError {
    NotAHistory,
    OldVersion { found: u32 },
    OtherWorld,
    Corrupt(String),
    Io(String),
}

impl fmt::Display for HistoryLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAHistory => write!(f, "not a history file"),
            Self::OldVersion { found } => {
                write!(f, "history file version {found}, expected {HISTORY_FILE_VERSION}")
            }
            Self::OtherWorld => write!(f, "history belongs to a different world"),
            Self::Corrupt(e) => write!(f, "corrupt history file: {e}"),
            Self::Io(e) => write!(f, "history file i/o error: {e}"),
        }
    }
}

impl std::error::Error for HistoryLoadError {}

pub fn encode(h: &History) -> Vec<u8> {
    let body = bincode::serialize(h).expect("history serializes");
    let mut out = Vec::with_capacity(HEADER_LEN + body.len() / 2);
    out.extend_from_slice(&HISTORY_MAGIC);
    out.extend_from_slice(&HISTORY_FILE_VERSION.to_le_bytes());
    out.extend_from_slice(&h.grid().world_id().to_le_bytes());
    out.extend_from_slice(&lz4_flex::compress_prepend_size(&body));
    out
}

pub fn decode(bytes: &[u8], grid: SocietyGrid) -> Result<History, HistoryLoadError> {
    if bytes.len() < HISTORY_MAGIC.len() || bytes[..HISTORY_MAGIC.len()] != HISTORY_MAGIC {
        return Err(HistoryLoadError::NotAHistory);
    }
    if bytes.len() < HEADER_LEN {
        return Err(HistoryLoadError::Corrupt(format!("{} bytes, shorter than the header", bytes.len())));
    }
    let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().expect("4 bytes"));
    let found = u32_at(8);
    if found != HISTORY_FILE_VERSION {
        return Err(HistoryLoadError::OldVersion { found });
    }
    if u32_at(12) != grid.world_id() {
        return Err(HistoryLoadError::OtherWorld);
    }
    let rest = &bytes[HEADER_LEN..];
    if rest.len() < 4 {
        return Err(HistoryLoadError::Corrupt("no body size".into()));
    }
    let size = u32::from_le_bytes(rest[..4].try_into().expect("4 bytes")) as usize;
    if size > MAX_HISTORY_BYTES {
        return Err(HistoryLoadError::Corrupt(format!("body of {size} bytes, more than {MAX_HISTORY_BYTES}")));
    }
    let body = lz4_flex::decompress(&rest[4..], size).map_err(|e| HistoryLoadError::Corrupt(e.to_string()))?;
    if body.len() != size {
        return Err(HistoryLoadError::Corrupt(format!("body is {} bytes, header says {size}", body.len())));
    }
    let mut h: History =
        bincode::deserialize(&body).map_err(|e| HistoryLoadError::Corrupt(e.to_string()))?;
    h.set_grid(grid);
    Ok(h)
}

/// Write atomically: a temp file in the same directory, then a rename.
#[cfg(not(target_arch = "wasm32"))]
pub fn save(h: &History, path: &Path) -> std::io::Result<()> {
    let name = path.file_name().map_or_else(|| "history".into(), |n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    let written = std::fs::write(&tmp, encode(h)).and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load(path: &Path, grid: SocietyGrid) -> Result<History, HistoryLoadError> {
    let bytes = std::fs::read(path).map_err(|e| HistoryLoadError::Io(e.to_string()))?;
    decode(&bytes, grid)
}

/// `worlds/<slug>/terrain.world` -> `worlds/<slug>/terrain.history`.
pub fn history_path_for(world_file: &Path) -> PathBuf {
    world_file.with_extension("history")
}
