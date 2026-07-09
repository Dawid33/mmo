use bevy::{
    mesh::{MeshVertexAttribute, MeshVertexBufferLayoutRef, VertexAttributeDescriptor},
    pbr::{MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline},
    prelude::*,
    reflect::TypePath,
    render::render_resource::{
        AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError, VertexFormat,
    },
    shader::ShaderRef,
};

/// The packed voxel vertex word pair — layout documented in
/// crates/client/src/renderer/meshing/pack.rs and mirrored by the decode in
/// assets/shaders/voxel_texture.wgsl.
pub const ATTRIBUTE_VOXEL_PACKED: MeshVertexAttribute =
    MeshVertexAttribute::new("VoxelPacked", 988540917, VertexFormat::Uint32x2);

/// The sole vertex attribute chunk meshes carry: 8 bytes/vertex, decoded
/// entirely in-shader (both the forward and prepass vertex stages).
pub fn vertex_layout() -> Vec<VertexAttributeDescriptor> {
    vec![ATTRIBUTE_VOXEL_PACKED.at_shader_location(8)]
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

    // The prepass (shadow/depth) pipeline needs the same packed-decode vertex
    // stage — the default prepass vertex reads POSITION, which chunk meshes
    // no longer carry. The WGSL branches on PREPASS_PIPELINE.
    fn prepass_vertex_shader() -> ShaderRef {
        "shaders/voxel_texture.wgsl".into()
    }

    // Called for BOTH the forward and prepass pipelines: each must accept the
    // packed-only vertex buffer.
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        let vertex_layout = layout.0.get_layout(&vertex_layout())?;
        descriptor.vertex.buffers = vec![vertex_layout];
        Ok(())
    }
}
