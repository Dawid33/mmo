use bytemuck::NoUninit;
use game::GameData;

#[repr(C)]
#[derive(Copy, Clone, Debug, NoUninit)]
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

pub struct ChunkMesh {
    pub vertices: Vec<Vertex>,
}

impl ChunkMesh {
    pub fn new(_data: &GameData) -> Self {
        // TODO: Meshing algorithm
        // for (x, length) in data.chunk.voxels.iter().enumerate() {
        //     for (y, width) in length.iter().enumerate() {
        //         for (z, v) in width.iter().enumerate() {
        //             info!("{:?}, {:?}, {:?}", x, y, z)
        //         }
        //     }
        // }
        Self {
            vertices: Vec::from([
                Vertex {
                    position: [-0.5, 0.5, 0.0],
                    color: [1.0, 0.0, 0.0],
                },
                Vertex {
                    position: [-0.5, -0.5, 0.0],
                    color: [0.0, 1.0, 0.0],
                },
                Vertex {
                    position: [0.5, 0.5, 0.0],
                    color: [0.0, 0.0, 1.0],
                },
                Vertex {
                    position: [-0.5, -0.5, 0.0],
                    color: [0.0, 1.0, 0.0],
                },
                Vertex {
                    position: [0.5, -0.5, 0.0],
                    color: [1.0, 0.0, 0.0],
                },
                Vertex {
                    position: [0.5, 0.5, 0.0],
                    color: [0.0, 0.0, 1.0],
                },
            ]),
        }
    }
}
