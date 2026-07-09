use bevy::{
    mesh::{MeshVertexAttribute, MeshVertexBufferLayoutRef, VertexAttributeDescriptor},
    pbr::{MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline},
    prelude::*,
    reflect::TypePath,
    render::render_resource::{
        AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError, VertexFormat,
    },
    shader::{ShaderDefVal, ShaderRef},
};

/// The packed voxel vertex word pair — layout documented in
/// crates/client/src/renderer/meshing/pack.rs and mirrored by the decode in
/// assets/shaders/voxel_texture.wgsl.
pub const ATTRIBUTE_VOXEL_PACKED: MeshVertexAttribute =
    MeshVertexAttribute::new("VoxelPacked", 988540917, VertexFormat::Uint32x2);

pub fn vertex_layout() -> Vec<VertexAttributeDescriptor> {
    vec![
        Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
        ATTRIBUTE_VOXEL_PACKED.at_shader_location(8),
    ]
}

// `pub` (not `pub(crate)`): reachable through `ChunkMaterial`'s public handle
// field in meshing.rs, so a tighter visibility trips `private_interfaces`.
#[derive(Asset, AsBindGroup, Debug, Clone, TypePath)]
pub struct StandardVoxelMaterial {
    #[texture(100, dimension = "2d_array")]
    #[sampler(101)]
    pub voxels_texture: Handle<Image>,
}

impl MaterialExtension for StandardVoxelMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/voxel_texture.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/voxel_texture.wgsl".into()
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if descriptor
            .vertex
            .shader_defs
            .contains(&ShaderDefVal::Bool("PREPASS_PIPELINE".into(), true))
        {
            return Ok(());
        }

        let vertex_layout = layout.0.get_layout(&vertex_layout())?;
        descriptor.vertex.buffers = vec![vertex_layout];
        Ok(())
    }
}
