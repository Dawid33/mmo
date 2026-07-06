use std::collections::BTreeMap;

use block_mesh::ndshape::ConstShape3u32;
use block_mesh::{MergeVoxel, VoxelVisibility};

use crate::block::{Block, BlockIndex, BlockKind, ChiselData, CHUNK_BLOCKS, CHUNK_BLOCK_COUNT, BLOCK_VOXELS};

const HALF_VOXEL_SIZE: f32 = 1.0 / 2.0;
pub type ChunkShape = ConstShape3u32<32, 32, 32>;

pub const CHUNK_VOXEL_COUNT: usize = 32 * 32 * 32;

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash)]
#[module(crate)]
pub struct Chunk {
    /// Authoritative block grid, length `CHUNK_BLOCK_COUNT` (8).
    pub blocks: Vec<Block>,
    /// Sparse sub-block detail; a block is chiseled iff present here.
    pub chisel: BTreeMap<BlockIndex, ChiselData>,
}

impl Default for Chunk {
    /// All-air. `Component<T>` requires `T: Default`; real content comes from
    /// constructors like [`Chunk::flat_floor`].
    fn default() -> Self {
        Self {
            blocks: vec![Block::default(); CHUNK_BLOCK_COUNT],
            chisel: BTreeMap::new(),
        }
    }
}

impl Chunk {
    /// Solid Stone floor for voxel heights `y < depth`, air above — built at
    /// block granularity. Blocks fully below the floor are whole Stone; blocks
    /// straddling the floor become a chiseled slab; blocks above are air.
    /// Derives (via `derive_voxels`) to the exact same voxel layout the old
    /// voxel-based `flat_floor` produced.
    pub fn flat_floor(depth: u32) -> Self {
        let depth = depth as usize;
        let mut blocks = vec![Block::new(BlockKind::Air); CHUNK_BLOCK_COUNT];
        let mut chisel = BTreeMap::new();
        for by in 0..CHUNK_BLOCKS {
            let (y0, y1) = (by * BLOCK_VOXELS, by * BLOCK_VOXELS + BLOCK_VOXELS);
            for bx in 0..CHUNK_BLOCKS {
                for bz in 0..CHUNK_BLOCKS {
                    let bi = BlockIndex::from_xyz(bx, by, bz);
                    if y1 <= depth {
                        blocks[bi.0 as usize] = Block::new(BlockKind::Stone);
                    } else if y0 >= depth {
                        // stays Air
                    } else {
                        blocks[bi.0 as usize] = Block::new(BlockKind::Stone);
                        let mut c = ChiselData::new(vec![BlockKind::Stone]);
                        for vy in 0..BLOCK_VOXELS {
                            if y0 + vy < depth {
                                for vx in 0..BLOCK_VOXELS {
                                    for vz in 0..BLOCK_VOXELS {
                                        c.set(vx, vy, vz, true, 0);
                                    }
                                }
                            }
                        }
                        chisel.insert(bi, c);
                    }
                }
            }
        }
        Self { blocks, chisel }
    }
}

#[derive(
    Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, PartialOrd, Ord, Eq,
)]
pub struct ChunkCoords {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) z: usize,
}

impl ChunkCoords {
    pub fn new(x: usize, y: usize, z: usize) -> Self {
        Self { x, y, z }
    }
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Copy, Clone, Hash, PartialEq, Eq)]
pub struct Voxel {
    pub kind: VoxelType,
}

impl Voxel {
    pub fn new(kind: VoxelType) -> Self {
        Self { kind }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VoxelType {
    Black,
    Air,
}

impl Default for VoxelType {
    fn default() -> Self {
        VoxelType::Air
    }
}

impl block_mesh::Voxel for Voxel {
    fn get_visibility(&self) -> VoxelVisibility {
        if self.kind == VoxelType::Air {
            VoxelVisibility::Empty
        } else {
            VoxelVisibility::Opaque
        }
    }
}

impl MergeVoxel for Voxel {
    type MergeValue = VoxelType;
    type MergeValueFacingNeighbour = VoxelType;

    fn merge_value(&self) -> Self::MergeValue {
        self.kind
    }

    fn merge_value_facing_neighbour(&self) -> Self::MergeValueFacingNeighbour {
        self.kind
    }
}
