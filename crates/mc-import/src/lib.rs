//! Minecraft Sponge schematic (`.schem`) reader. Input is untrusted: every failure is an
//! [`ImportError`] value, and no length field in a file drives an allocation before it is checked.

pub mod classify;
pub mod nbt;
pub mod sponge;
pub mod table;
pub mod volume;

use std::fmt;

pub use classify::{classify, Class, Classified};
pub use nbt::{parse_gzip_nbt, parse_nbt, Tag, NBT_MAX_DEPTH};
pub use sponge::{read_schem, SCHEM_MAX_BYTES, SCHEM_MAX_CELLS};
pub use table::{Table, MAPPING_VERSION};
pub use volume::{BlockEntity, BlockState, BlockVolume};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The input does not start with the gzip magic bytes.
    NotGzip,
    /// The decompressed data exceeds the size cap.
    TooLarge,
    /// The input ends before a declared length is satisfied.
    Truncated,
    /// Malformed NBT.
    BadNbt(String),
    /// Valid NBT that is not a (consistent) Sponge schematic.
    NotASchematic(String),
    /// A schematic version or feature this reader does not handle.
    Unsupported(String),
    Io(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::NotGzip => write!(f, "not a gzip file"),
            ImportError::TooLarge => write!(f, "decompressed data is too large"),
            ImportError::Truncated => write!(f, "input is truncated"),
            ImportError::BadNbt(m) => write!(f, "bad NBT: {m}"),
            ImportError::NotASchematic(m) => write!(f, "not a schematic: {m}"),
            ImportError::Unsupported(m) => write!(f, "unsupported: {m}"),
            ImportError::Io(m) => write!(f, "i/o error: {m}"),
        }
    }
}

impl std::error::Error for ImportError {}
