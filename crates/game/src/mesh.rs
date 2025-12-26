use std::slice::{ChunkBy, ChunksExactMut};

use block_mesh::ndshape::{ConstShape, ConstShape3u32};
use block_mesh::{
    greedy_quads, GreedyQuadsBuffer, MergeVoxel, VoxelVisibility, RIGHT_HANDED_Y_UP_CONFIG,
};
use bytemuck::NoUninit;
use log::info;
use parry3d::transformation::voxelization::VoxelSet;
use rapier3d::prelude::{
    CCDSolver, ColliderBuilder, ColliderHandle, ColliderSet, InverseKinematicsOption,
};
use rollback::{Voxel, VoxelType, CHUNK_VOXEL_COUNT};

const HALF_VOXEL_SIZE: f32 = 1.0 / 2.0;
pub type ChunkShape = ConstShape3u32<32, 32, 32>;

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
