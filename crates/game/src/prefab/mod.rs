//! Prefabs: pre-built structures stamped into the voxel world. `shapes` is the catalogue of named block
//! shapes and `turn` the one rotation convention; `file` and `codec` are the on-disk form (RON text, lz4
//! binary); `resolve` turns a file into block ids with a spatial index; `layout` lays a resolved prefab
//! out one chunk at a time; `builtin` is the game's own set, embedded at build time or read from disk.

pub mod builtin;
pub mod codec;
pub mod file;
pub mod layout;
pub mod resolve;
pub mod shapes;
pub mod turn;

pub use builtin::{
    BUILTIN_PREFAB_DIR, PrefabSet, embedded_prefab_sources, install_prefabs, load_builtin_prefabs,
    load_prefab_dirs, prefabs,
};
pub use codec::{
    BIN_MAGIC, BIN_SUFFIX, RON_SUFFIX, prefab_from_bin, prefab_from_named_bytes, prefab_from_ron,
    prefab_to_bin, prefab_to_ron,
};
pub use file::{
    FileBox, FileCarve, FileShape, FileShaped, GroundFit, ImportInfo, Marker, PREFAB_FORMAT,
    PrefabError, PrefabFile, PrefabMeta,
};
pub use layout::{
    ChunkStamp, Placement, cell_of_world, cell_write, chunks_touched, column_bottom_world,
    column_top_world, layout_chunk, marker_in_world, placement_bounds, world_of_cell,
};
pub use resolve::{
    Cell, MARKER_MARGIN, PREFAB_BUCKET_BLOCKS, Problem, ResolvedPrefab, ResolvedShape, Trust,
    UNTRUSTED_MAX_BUCKET_REFS, UNTRUSTED_MAX_COLUMN_WORK, UNTRUSTED_MAX_EXTENT,
    UNTRUSTED_MAX_ITEMS, prefab_name_key, resolve,
};
pub use shapes::{Facing, Half, Shape, Sides, StairCorner};
pub use turn::{turn_voxel, turn_xz};
