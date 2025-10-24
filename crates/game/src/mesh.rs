use bytemuck::NoUninit;
use log::info;
use rapier3d::prelude::{
    CCDSolver, ColliderBuilder, ColliderHandle, ColliderSet, InverseKinematicsOption,
};

pub type ChunkVoxels = [[[Voxel; 2]; 2]; 2];

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

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial)]
#[module(crate)]
pub struct Chunk {
    pub voxels: ChunkVoxels,
    pub collider: Vec<ColliderHandle>,
}

impl Default for Chunk {
    fn default() -> Self {
        let voxels: [[[Voxel; 2]; 2]; 2] = Default::default();
        Self {
            collider: Vec::new(),
            voxels,
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
    pub vertices: Vec<Vertex>,
}

const HALF_VOXEL_SIZE: f32 = 1.0 / 2.0;

impl ChunkMesh {
    pub fn new(data: &[[[Voxel; 2]; 2]; 2]) -> Self {
        let mut vertices: Vec<Vertex> = Vec::new();
        for (x_usize, length) in data.iter().enumerate() {
            for (y_usize, width) in length.iter().enumerate() {
                for (z_usize, v) in width.iter().enumerate() {
                    let x = x_usize as f32;
                    let y = y_usize as f32;
                    let z = z_usize as f32;
                    if v.kind == VoxelType::Air {
                        info!("air: {:?}, {:?}, {:?}, {:?}", x, y, z, v);
                        continue;
                    }

                    info!("{:?}, {:?}, {:?}, {:?}", x, y, z, v);
                    let face = [
                        Vertex {
                            position: [-HALF_VOXEL_SIZE, HALF_VOXEL_SIZE, 0.0],
                            color: [0.0, 0.0, 1.0],
                        },
                        Vertex {
                            position: [-HALF_VOXEL_SIZE, -HALF_VOXEL_SIZE, 0.0],
                            color: [0.0, 0.0, 1.0],
                        },
                        Vertex {
                            position: [HALF_VOXEL_SIZE, HALF_VOXEL_SIZE, 0.0],
                            color: [0.0, 0.0, 1.0],
                        },
                        Vertex {
                            position: [-HALF_VOXEL_SIZE, -HALF_VOXEL_SIZE, 0.0],
                            color: [0.0, 0.0, 1.0],
                        },
                        Vertex {
                            position: [HALF_VOXEL_SIZE, -HALF_VOXEL_SIZE, 0.0],
                            color: [0.0, 0.0, 1.0],
                        },
                        Vertex {
                            position: [HALF_VOXEL_SIZE, HALF_VOXEL_SIZE, 0.0],
                            color: [0.0, 0.0, 1.0],
                        },
                    ];

                    // Front
                    if (z_usize + 1 < length.len()
                        && VoxelType::Air == width.get(z_usize + 1).unwrap().kind)
                        || z_usize == length.len() - 1
                    {
                        for mut v in face.clone() {
                            v.position[0] += x;
                            v.position[1] += y;
                            v.position[2] = HALF_VOXEL_SIZE + z;
                            vertices.push(v);
                        }
                    }

                    // Back
                    if (z_usize > 0 && VoxelType::Air == width.get(z_usize - 1).unwrap().kind)
                        || z_usize == 0
                    {
                        for mut v in face.clone().into_iter().rev() {
                            v.position[0] += x;
                            v.position[1] += y;
                            v.position[2] = -HALF_VOXEL_SIZE + z;
                            vertices.push(v);
                        }
                    }

                    // Right
                    if (x_usize + 1 < length.len()
                        && VoxelType::Air
                            == data
                                .get(x_usize + 1)
                                .unwrap()
                                .get(y_usize)
                                .unwrap()
                                .get(z_usize)
                                .unwrap()
                                .kind)
                        || x_usize == length.len() - 1
                    {
                        for mut v in face.clone().into_iter().rev() {
                            v.position[2] = v.position[0] + z;
                            v.position[0] = HALF_VOXEL_SIZE + x;
                            v.position[1] += y;
                            v.color = [1.0, 0.0, 0.0];
                            vertices.push(v);
                        }
                    }

                    // Left
                    if (x_usize > 0
                        && VoxelType::Air
                            == data
                                .get(x_usize - 1)
                                .unwrap()
                                .get(y_usize)
                                .unwrap()
                                .get(z_usize)
                                .unwrap()
                                .kind)
                        || x_usize == 0
                    {
                        for mut v in face.clone() {
                            v.position[2] = v.position[0] + z;
                            v.position[0] = -HALF_VOXEL_SIZE + x;
                            v.position[1] += y;
                            v.color = [1.0, 0.0, 0.0];
                            vertices.push(v);
                        }
                    }

                    // Top
                    if (y_usize + 1 < length.len()
                        && VoxelType::Air
                            == length.get(y_usize + 1).unwrap().get(z_usize).unwrap().kind)
                        || y_usize == length.len() - 1
                    {
                        info!("making top1, {:?}", y_usize);
                        for mut v in face.clone().into_iter().rev() {
                            v.position[2] = v.position[1] + z;
                            v.position[1] = HALF_VOXEL_SIZE + y;
                            v.position[0] += x;
                            v.color = [0.0, 1.0, 0.0];
                            vertices.push(v);
                        }
                    }

                    // Bottom
                    if (y_usize > 0
                        && VoxelType::Air
                            == length.get(y_usize - 1).unwrap().get(z_usize).unwrap().kind)
                        || y_usize == 0
                    {
                        for mut v in face.clone() {
                            v.position[2] = v.position[1] + z;
                            v.position[1] = -HALF_VOXEL_SIZE + y;
                            v.position[0] += x;
                            v.color = [0.0, 1.0, 0.0];
                            vertices.push(v);
                        }
                    }
                }
            }
        }

        Self { vertices }
    }
}
