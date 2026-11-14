//! Prefabs: pre-built structures stamped into the voxel world. This module
//! holds the shape catalogue and the one rotation convention.

pub mod codec;
pub mod file;
pub mod resolve;
pub mod shapes;
pub mod turn;

pub use shapes::{Facing, Half, Shape, Sides, StairCorner};
pub use turn::{turn_voxel, turn_xz};
pub use codec::{
    prefab_from_bin, prefab_from_named_bytes, prefab_from_ron, prefab_to_bin, prefab_to_ron, BIN_MAGIC, BIN_SUFFIX,
    RON_SUFFIX,
};
pub use file::{
    FileBox, FileCarve, FileShape, FileShaped, GroundFit, ImportInfo, Marker, PrefabError, PrefabFile, PrefabMeta,
    PREFAB_FORMAT,
};
pub use resolve::{
    prefab_name_key, resolve, Cell, Problem, ResolvedPrefab, ResolvedShape, Trust, PREFAB_BUCKET_BLOCKS,
    UNTRUSTED_MAX_EXTENT, UNTRUSTED_MAX_ITEMS,
};
