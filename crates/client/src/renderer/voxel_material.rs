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

pub const ATTRIBUTE_TEX_INDEX: MeshVertexAttribute =
    MeshVertexAttribute::new("TextureIndex", 989640910, VertexFormat::Uint32x3);

pub fn vertex_layout() -> Vec<VertexAttributeDescriptor> {
    vec![
        Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
        Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
        Mesh::ATTRIBUTE_UV_0.at_shader_location(2),
        // The shader unconditionally reads `vertex.color` / `in.color`, so
        // `VERTEX_COLORS` must always be defined, which bevy_pbr's mesh
        // specialization only does when the mesh actually carries
        // `Mesh::ATTRIBUTE_COLOR` (see bevy_pbr::render::mesh, shader_defs.push
        // ("VERTEX_COLORS"...)). Keep the single location-5 entry the WGSL
        // declares; the mesher emits a matching filler attribute.
        Mesh::ATTRIBUTE_COLOR.at_shader_location(5),
        ATTRIBUTE_TEX_INDEX.at_shader_location(8),
    ]
}

#[derive(Asset, AsBindGroup, Debug, Clone, TypePath)]
pub(crate) struct StandardVoxelMaterial {
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
