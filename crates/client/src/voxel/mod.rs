pub mod chunk;
pub mod chunk_map;
pub mod configuration;
pub mod debug_draw;
pub mod mesh_cache;
pub mod meshing;
pub mod plugin;
pub mod voxel;
pub mod voxel_material;
pub mod voxel_traversal;
pub mod voxel_world;
pub mod voxel_world_internal;

pub mod prelude {
    pub use crate::voxel::chunk::{Chunk, NeedsDespawn};
    pub use crate::voxel::configuration::*;
    pub use crate::voxel::plugin::VoxelWorldPlugin;
    pub use crate::voxel::voxel::{VoxelFace, WorldVoxel, VOXEL_SIZE};
    pub use crate::voxel::voxel_world::{
        get_chunk_voxel_position, VoxelRaycastResult, VoxelWorld, VoxelWorldCamera,
    };
    pub use crate::voxel::voxel_world::{
        ChunkWillDespawn, ChunkWillRemesh, ChunkWillSpawn, ChunkWillUpdate,
    };
}

pub mod custom_meshing {
    pub use crate::voxel::chunk::PaddedChunkShape;
    pub use crate::voxel::chunk::CHUNK_SIZE_F;
    pub use crate::voxel::chunk::CHUNK_SIZE_I;
    pub use crate::voxel::chunk::CHUNK_SIZE_U;
    pub use crate::voxel::meshing::generate_chunk_mesh;
    pub use crate::voxel::meshing::mesh_from_quads;
    pub use crate::voxel::meshing::VoxelArray;
}

pub mod debug {
    pub use crate::voxel::debug_draw::*;
}

pub mod rendering {
    pub use crate::voxel::plugin::VoxelWorldMaterialHandle;
    pub use crate::voxel::voxel_material::vertex_layout;
    pub use crate::voxel::voxel_material::ATTRIBUTE_TEX_INDEX;
    pub use crate::voxel::voxel_material::VOXEL_TEXTURE_SHADER_HANDLE;
}

pub mod traversal_alg {
    pub use crate::voxel::voxel_traversal::*;
}
