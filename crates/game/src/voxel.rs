use std::collections::BTreeMap;

use block_mesh::ndshape::ConstShape3u32;
use block_mesh::{MergeVoxel, VoxelVisibility};

use crate::block::{Block, BlockIndex, ChiselData, CHUNK_BLOCKS, CHUNK_BLOCK_COUNT, BLOCK_VOXELS};
use crate::registry::BlockId;

/// Edge of a render/LOD brick in voxels: the dense voxel array the mesher, the
/// LOD grids and the client consume. Not the sim chunk (see `CHUNK_UNITS`).
pub const BRICK_VOXELS: usize = 32;
pub const BRICK_BLOCKS: usize = BRICK_VOXELS / BLOCK_VOXELS;
pub type BrickShape = ConstShape3u32<32, 32, 32>;

pub const BRICK_VOXEL_COUNT: usize = BRICK_VOXELS * BRICK_VOXELS * BRICK_VOXELS;

/// A chunk's authoritative block grid, `CHUNK_BLOCK_COUNT` blocks indexed by
/// [`BlockIndex`]. Always canonical: a grid whose blocks are all equal is
/// `Uniform`, never a `Dense` box, so equal contents serialize and hash
/// equally (every constructor and [`ChunkBlocks::set`] normalise).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub enum ChunkBlocks {
    /// Every block is this one.
    Uniform(Block),
    /// Length `CHUNK_BLOCK_COUNT`, at least two distinct blocks.
    Dense(Box<[Block]>),
}

impl Default for ChunkBlocks {
    /// All air.
    fn default() -> Self {
        ChunkBlocks::Uniform(Block::new(BlockId::AIR))
    }
}

impl ChunkBlocks {
    /// The canonical form of a linear block list (`len == CHUNK_BLOCK_COUNT`).
    pub fn from_vec(blocks: Vec<Block>) -> Self {
        assert_eq!(blocks.len(), CHUNK_BLOCK_COUNT, "a chunk holds exactly CHUNK_BLOCK_COUNT blocks");
        match Self::all_equal(&blocks) {
            Some(b) => ChunkBlocks::Uniform(b),
            None => ChunkBlocks::Dense(blocks.into_boxed_slice()),
        }
    }

    fn all_equal(blocks: &[Block]) -> Option<Block> {
        let first = blocks[0];
        blocks.iter().all(|b| *b == first).then_some(first)
    }

    pub fn get(&self, i: BlockIndex) -> Block {
        match self {
            ChunkBlocks::Uniform(b) => *b,
            ChunkBlocks::Dense(blocks) => blocks[i.linear()],
        }
    }

    /// Write one block, keeping the canonical form: a differing write into a
    /// `Uniform` grid densifies it; a `Dense` grid whose blocks all become
    /// equal collapses to `Uniform` (an O(`CHUNK_BLOCK_COUNT`) check — edits
    /// are rare).
    pub fn set(&mut self, i: BlockIndex, b: Block) {
        match self {
            ChunkBlocks::Uniform(u) => {
                if *u != b {
                    let mut blocks = vec![*u; CHUNK_BLOCK_COUNT].into_boxed_slice();
                    blocks[i.linear()] = b;
                    *self = ChunkBlocks::Dense(blocks);
                }
            }
            ChunkBlocks::Dense(blocks) => {
                blocks[i.linear()] = b;
                if let Some(u) = Self::all_equal(blocks) {
                    *self = ChunkBlocks::Uniform(u);
                }
            }
        }
    }

    /// The single block of a `Uniform` grid.
    pub fn uniform(&self) -> Option<Block> {
        match self {
            ChunkBlocks::Uniform(b) => Some(*b),
            ChunkBlocks::Dense(_) => None,
        }
    }

    /// Every block in `BlockIndex` order.
    pub fn iter(&self) -> impl Iterator<Item = (BlockIndex, Block)> + '_ {
        (0..CHUNK_BLOCK_COUNT).map(move |i| {
            let bi = BlockIndex::from_linear(i);
            (bi, self.get(bi))
        })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash, state_hash::StateHash)]
#[module(crate)]
pub struct Chunk {
    /// Authoritative block grid, `CHUNK_BLOCK_COUNT` blocks, canonical.
    pub blocks: ChunkBlocks,
    /// Sparse sub-block detail; a block is chiseled iff present here.
    pub chisel: BTreeMap<BlockIndex, ChiselData>,
}

impl Default for Chunk {
    /// All-air. `Component<T>` requires `T: Default`; real content comes from
    /// constructors like [`Chunk::flat_floor`].
    fn default() -> Self {
        Self { blocks: ChunkBlocks::default(), chisel: BTreeMap::new() }
    }
}

impl Chunk {
    /// Solid floor for voxel heights `y < depth` using an arbitrary non-air
    /// material — a dev/test convenience. Production worldgen uses
    /// [`Chunk::flat_floor_with`] to place a registry-resolved material.
    pub fn flat_floor(depth: u32) -> Self {
        Self::flat_floor_with(depth, BlockId(1))
    }

    /// Solid `solid`-material floor for voxel heights `y < depth`, air above,
    /// built at block granularity: whole blocks below the floor, a chiseled
    /// slab for the block straddling it, air above.
    pub fn flat_floor_with(depth: u32, solid: BlockId) -> Self {
        let depth = depth as usize;
        let mut blocks = vec![Block::new(BlockId::AIR); CHUNK_BLOCK_COUNT];
        let mut chisel = BTreeMap::new();
        for by in 0..CHUNK_BLOCKS {
            let (y0, y1) = (by * BLOCK_VOXELS, by * BLOCK_VOXELS + BLOCK_VOXELS);
            for bx in 0..CHUNK_BLOCKS {
                for bz in 0..CHUNK_BLOCKS {
                    let bi = BlockIndex::from_xyz(bx, by, bz);
                    if y1 <= depth {
                        blocks[bi.linear()] = Block::new(solid);
                    } else if y0 >= depth {
                        // stays Air
                    } else {
                        blocks[bi.linear()] = Block::new(solid);
                        let top = [BLOCK_VOXELS, depth - y0, BLOCK_VOXELS];
                        chisel.insert(bi, ChiselData::cuboid(solid, [0, 0, 0], top));
                    }
                }
            }
        }
        Self { blocks: ChunkBlocks::from_vec(blocks), chisel }
    }

    /// Block at block-granularity coords (each axis 0..CHUNK_BLOCKS).
    pub fn block_at(&self, bx: usize, by: usize, bz: usize) -> Block {
        self.blocks.get(BlockIndex::from_xyz(bx, by, bz))
    }

    /// Set a whole block, clearing any chisel detail for that block cell
    /// (whole-block granularity — sub-block chisel is not addressed here).
    pub fn set_block(&mut self, bx: usize, by: usize, bz: usize, block: Block) {
        let bi = BlockIndex::from_xyz(bx, by, bz);
        self.blocks.set(bi, block);
        self.chisel.remove(&bi);
    }

    /// A chunk uniformly filled with `block` (no chisel detail).
    pub fn uniform(block: Block) -> Self {
        Self { blocks: ChunkBlocks::Uniform(block), chisel: BTreeMap::new() }
    }

    /// True iff every block equals `block` (full Block, incl. `data`) and there
    /// is no chisel detail. O(1): the grid is canonical.
    pub fn is_uniform(&self, block: Block) -> bool {
        self.chisel.is_empty() && self.blocks.uniform() == Some(block)
    }

    /// Topmost non-air voxel of the voxel column `(vx, vz)` (each `0..CHUNK_UNITS`):
    /// its local Y and material. Read straight from blocks plus chisel — no
    /// dense voxel array. A chiseled block shows its chisel materials (air where
    /// empty); a plain block shows its id.
    pub fn column_top(&self, vx: u32, vz: u32) -> Option<(u32, BlockId)> {
        let (vx, vz) = (vx as usize, vz as usize);
        let (bx, lx) = (vx / BLOCK_VOXELS, vx % BLOCK_VOXELS);
        let (bz, lz) = (vz / BLOCK_VOXELS, vz % BLOCK_VOXELS);
        for by in (0..CHUNK_BLOCKS).rev() {
            let bi = BlockIndex::from_xyz(bx, by, bz);
            let base = (by * BLOCK_VOXELS) as u32;
            match self.chisel.get(&bi) {
                Some(chisel) => {
                    for ly in (0..BLOCK_VOXELS).rev() {
                        let m = chisel.material_at(lx, ly, lz);
                        if m != BlockId::AIR {
                            return Some((base + ly as u32, m));
                        }
                    }
                }
                None => {
                    let id = self.blocks.get(bi).id;
                    if id != BlockId::AIR {
                        return Some((base + BLOCK_VOXELS as u32 - 1, id));
                    }
                }
            }
        }
        None
    }

    /// True iff every block is air and no chisel detail remains.
    pub fn is_all_air(&self) -> bool {
        self.is_uniform(Block::new(BlockId::AIR))
    }
}

// Keep the struct's EXISTING derive list verbatim — only the `y` field type
// changes (usize -> i32). Do not add/remove derives.
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

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Copy, Clone, Hash, state_hash::StateHash, PartialEq, Eq)]
pub struct Voxel {
    pub block: BlockId,
}

impl Voxel {
    pub fn new(block: BlockId) -> Self {
        Self { block }
    }

    pub fn is_air(&self) -> bool {
        self.block == BlockId::AIR
    }
}

impl block_mesh::Voxel for Voxel {
    fn get_visibility(&self) -> VoxelVisibility {
        if self.is_air() {
            VoxelVisibility::Empty
        } else {
            VoxelVisibility::Opaque
        }
    }
}

#[cfg(test)]
mod coord_tests {
    use super::*;

    #[test]
    fn chunk_coords_allow_negative_y() {
        let c = ChunkCoords::new(3, -2, 5);
        assert_eq!(c.y, -2);
        assert_eq!(c.x, 3);
        assert_eq!(c.z, 5);
    }

    #[test]
    fn column_coords_construct() {
        let c = ColumnCoords::new(1, 7);
        assert_eq!((c.x, c.z), (1, 7));
    }
}

impl MergeVoxel for Voxel {
    type MergeValue = BlockId;
    type MergeValueFacingNeighbour = BlockId;

    fn merge_value(&self) -> Self::MergeValue {
        self.block
    }

    fn merge_value_facing_neighbour(&self) -> Self::MergeValueFacingNeighbour {
        self.block
    }
}
