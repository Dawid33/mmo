use std::slice::ChunksExactMut;

use crate::parry::transformation::voxelization::VoxelSet;
use crate::rapier::prelude::{
    CCDSolver, ColliderBuilder, ColliderHandle, ColliderSet, InverseKinematicsOption,
};
use block_mesh::ndshape::{ConstShape, ConstShape3u32};
use block_mesh::{
    greedy_quads, GreedyQuadsBuffer, MergeVoxel, VoxelVisibility, RIGHT_HANDED_Y_UP_CONFIG,
};
use bytemuck::NoUninit;
use log::info;

const HALF_VOXEL_SIZE: f32 = 1.0 / 2.0;
type ChunkShape = ConstShape3u32<100, 100, 100>;

pub type ChunkVoxels = [Voxel; 2 * 2 * 2];

#[derive(Debug, serde::Serialize, serde::Deserialize, Copy, Clone, PartialEq, Eq)]
pub enum VoxelType {
    Blue,
    Air,
}

impl Default for VoxelType {
    fn default() -> Self {
        VoxelType::Air
    }
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Copy, Clone)]
pub struct Voxel {
    pub kind: VoxelType,
}

impl block_mesh::Voxel for VoxelType {
    fn get_visibility(&self) -> VoxelVisibility {
        if *self == VoxelType::Air {
            VoxelVisibility::Empty
        } else {
            VoxelVisibility::Opaque
        }
    }
}

impl MergeVoxel for VoxelType {
    type MergeValue = Self;
    type MergeValueFacingNeighbour = VoxelType;

    fn merge_value(&self) -> Self::MergeValue {
        *self
    }

    fn merge_value_facing_neighbour(&self) -> Self::MergeValueFacingNeighbour {
        *self
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial)]
#[module(crate)]
pub struct Chunk {
    pub voxels: ChunkVoxels,
    pub collider: Vec<ColliderHandle>,
}

impl Default for Chunk {
    fn default() -> Self {
        Self {
            collider: Vec::new(),
            voxels: Default::default(),
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, NoUninit, serde::Serialize, serde::Deserialize)]
pub struct Vertex {
    position: [f32; 3],
    color: [f32; 3],
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Vertex::ATTRIBS,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ChunkMesh {
    buffer: GreedyQuadsBuffer,
    pub indices: Vec<u32>,
    pub vertices: Vec<Vertex>,
    pub normals: Vec<[f32; 3]>,
}

impl ChunkMesh {
    pub fn new(data: &ChunkVoxels) -> Self {
        let mut voxels = [VoxelType::Air; ChunkShape::SIZE as usize];
        for i in 0..ChunkShape::SIZE {
            let [mut x, mut y, mut z] = ChunkShape::delinearize(i);

            voxels[i as usize] = if x > 0 && y > 0 && z > 0 && y < 99 && z < 99 && x < 99 {
                if y == 1 {
                    VoxelType::Blue
                } else {
                    VoxelType::Air
                }
            } else {
                VoxelType::Air
            }
        }

        let mut buffer = GreedyQuadsBuffer::new(voxels.len());
        greedy_quads(
            &voxels,
            &ChunkShape {},
            [0; 3],
            [99, 9, 99],
            &RIGHT_HANDED_Y_UP_CONFIG.faces,
            &mut buffer,
        );
        let num_indices = buffer.quads.num_quads() * 6;
        let num_vertices = buffer.quads.num_quads() * 4;
        let mut indices = Vec::with_capacity(num_indices);
        let mut vertices = Vec::with_capacity(num_vertices);
        let mut normals = Vec::with_capacity(num_vertices);
        for (group, face) in buffer
            .quads
            .groups
            .iter()
            .zip(RIGHT_HANDED_Y_UP_CONFIG.faces.into_iter())
        {
            for quad in group.into_iter() {
                indices.extend_from_slice(&face.quad_mesh_indices(vertices.len() as u32));
                vertices.extend_from_slice(&face.quad_mesh_positions(&quad, 0.1).map(|position| {
                    Vertex {
                        position,
                        color: [0.0, 0.0, 0.0],
                    }
                }));
                normals.extend_from_slice(&face.quad_mesh_normals());
            }
        }

        Self {
            buffer,
            indices,
            vertices,
            normals,
        }
    }
}
