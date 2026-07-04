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
use crate::{Voxel, VoxelType, CHUNK_VOXEL_COUNT};
