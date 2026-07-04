use block_mesh::ndshape::{ConstShape, ConstShape3u32};
use block_mesh::{MergeVoxel, VoxelVisibility};
use rapier3d::prelude::ColliderHandle;

const HALF_VOXEL_SIZE: f32 = 1.0 / 2.0;
pub type ChunkShape = ConstShape3u32<32, 32, 32>;

pub const CHUNK_VOXEL_COUNT: usize = 32 * 32 * 32;

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash)]
#[module(crate)]
pub struct Chunk {
    pub voxels: Vec<Voxel>,
    pub collider: Vec<ColliderHandle>,
}

impl Default for Chunk {
    fn default() -> Self {
        let mut voxels = Vec::with_capacity(ChunkShape::SIZE as usize);
        for i in 0..ChunkShape::SIZE {
            let [mut x, mut y, mut z] = ChunkShape::delinearize(i);

            let v = if x > 0 && y > 0 && z > 0 && y < 31 && z < 31 && x < 31 {
                if y == 1 {
                    Voxel::new(VoxelType::Black)
                } else {
                    Voxel::new(VoxelType::Air)
                }
            } else {
                Voxel::new(VoxelType::Air)
            };
            voxels.push(v);
        }

        Self {
            collider: Vec::new(),
            voxels,
        }
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

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Copy, Clone, Hash)]
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
