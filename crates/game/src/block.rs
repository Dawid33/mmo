//! Grid constants and addresses shared by the whole sim.
//!
//! The names are kept from the voxel world on purpose (stud world plan, D4 —
//! the rename is open): a "block" is 1 m of width, i.e. one tile column
//! (`tiles.rs`); `CHUNK_BLOCKS` is the sim chunk's edge in those; a
//! `BlockId` (`registry.rs`) is an item / material id, mapped to a stud
//! material by `stud_material_of_block`.

/// Sim units per metre along x and z: the width of one tile (`tiles.rs`) and of
/// one world cell (`WorldBlockPos`). Two studs (`TILE_STUDS * STUD_UNITS`).
pub const BLOCK_UNITS: usize = 16;
/// Metres (tile columns) per axis of a sim chunk — the unit that owns one
/// entity, one rapier body and one collider set.
pub const CHUNK_BLOCKS: usize = 16;
/// Sim-chunk edge in sim units.
pub const CHUNK_UNITS: usize = CHUNK_BLOCKS * BLOCK_UNITS;

/// Region-local address of a sim chunk column slot: `x`/`z` in
/// `0..REGION_CHUNKS`, signed `y`.
#[derive(
    Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, PartialOrd, Ord, Eq,
)]
pub struct ChunkCoords {
    pub x: usize,
    pub y: i32,
    pub z: usize,
}

impl ChunkCoords {
    pub fn new(x: usize, y: i32, z: usize) -> Self {
        Self { x, y, z }
    }
}

/// Region-local (x, z) address of a vertical column, 0..REGION_CHUNKS.
#[derive(
    Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash, state_hash::StateHash, PartialOrd, Ord,
)]
pub struct ColumnCoords {
    pub x: usize,
    pub z: usize,
}

impl ColumnCoords {
    pub fn new(x: usize, z: usize) -> Self {
        Self { x, z }
    }
}

/// A region-local cell `[x, y, z]` as a struct: a tile column and a level
/// (what [`crate::WorldBlockPos::from_local`] takes and `to_local` gives).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPos {
    pub fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }
}

/// Bits of a tree id (worldgen's `tree_id`: two lattice axes of half as many
/// bits each); a stud brick's `tag` holds it.
pub const TREE_ID_BITS: u32 = 18;
pub const TREE_ID_MASK: u32 = (1 << TREE_ID_BITS) - 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_coords_allow_negative_y() {
        let c = ChunkCoords::new(3, -2, 5);
        assert_eq!(c.y, -2);
    }
}
