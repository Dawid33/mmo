use super::fortress_view::{FortressViewUniform, cutaway_active};
use super::meshing::ChunkMaterial;
use bevy::{
    mesh::{MeshVertexAttribute, MeshVertexBufferLayoutRef, VertexAttributeDescriptor},
    pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline},
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

/// `assets/shaders/voxel_texture.wgsl`, compiled into the binary
/// ([`embed_voxel_texture_shader`]) instead of loaded from `assets/`: the packed
/// vertex layout it decodes changes with the Rust that packs it (`pack.rs`), and
/// a browser serves a cached `.wgsl` across rebuilds (the dev server and the
/// deployed file server send no cache headers, so Firefox reuses an old copy
/// heuristically) — an old decoder on new vertices scatters the terrain into
/// thin striped panels. Embedded, the decoder always matches its encoder.
pub const VOXEL_TEXTURE_SHADER: Handle<Shader> =
    bevy::asset::uuid_handle!("5f0d8a3e-6c2b-4f71-9a4e-2b7d1c9e3a60");

/// Register [`VOXEL_TEXTURE_SHADER`] (call before the material is used).
pub fn embed_voxel_texture_shader(app: &mut App) {
    bevy::asset::load_internal_asset!(
        app,
        VOXEL_TEXTURE_SHADER,
        "../../../../assets/shaders/voxel_texture.wgsl",
        Shader::from_wgsl
    );
}

// `pub` (not `pub(crate)`): reachable through `ChunkMaterial`'s public handle
// field in meshing.rs, so a tighter visibility trips `private_interfaces`.
#[derive(Asset, AsBindGroup, Debug, Clone, TypePath)]
#[bind_group_data(StandardVoxelMaterialKey)]
pub struct StandardVoxelMaterial {
    #[texture(100, dimension = "2d_array")]
    #[sampler(101)]
    pub voxels_texture: Handle<Image>,
    /// The z-level cutaway (`fortress_view::FortressViewUniform`, kept in step
    /// by [`sync_chunk_material_cut`]): the per-chunk fallback's copy of what
    /// the pulled path binds as `voxel_pull.wgsl`'s `fortress`.
    #[uniform(102)]
    pub fortress: FortressViewUniform,
    /// Side layer -> top layer of the same block (`BlockTextureLayers::side_to_top`)
    /// as an R32Uint `len x 1` texture: the cut's cap shows a block's top.
    #[texture(103, sample_type = "u_int")]
    pub top_layers: Handle<Image>,
}

/// Pipeline key: while the cut is on, back faces are drawn (they are the
/// cut's cap inside the home region; the shader drops them elsewhere).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StandardVoxelMaterialKey {
    pub cutaway: bool,
}

impl From<&StandardVoxelMaterial> for StandardVoxelMaterialKey {
    fn from(m: &StandardVoxelMaterial) -> Self {
        Self {
            cutaway: cutaway_active(&m.fortress),
        }
    }
}

/// The `top_layers` texture for [`StandardVoxelMaterial`].
pub fn top_layers_image(lut: &[u32]) -> Image {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
    let lut = if lut.is_empty() { &[0u32][..] } else { lut };
    Image::new(
        Extent3d {
            width: lut.len() as u32,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        bytemuck::cast_slice(lut).to_vec(),
        TextureFormat::R32Uint,
        RenderAssetUsages::RENDER_WORLD,
    )
}

/// Copy the fortress view into the chunk material when it changes (the
/// per-chunk Mesh fallback — WebGL2 — draws terrain with it; the pulled path
/// reads the uniform itself).
pub fn sync_chunk_material_cut(
    uniform: Res<FortressViewUniform>,
    material: Option<Res<ChunkMaterial>>,
    mut materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>>,
) {
    let Some(material) = material else { return };
    let stale = materials
        .get(&material.0)
        .is_some_and(|m| m.extension.fortress != *uniform);
    if stale {
        if let Some(mut m) = materials.get_mut(&material.0) {
            m.extension.fortress = *uniform;
        }
    }
}

impl MaterialExtension for StandardVoxelMaterial {
    fn vertex_shader() -> ShaderRef {
        ShaderRef::Handle(VOXEL_TEXTURE_SHADER)
    }

    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(VOXEL_TEXTURE_SHADER)
    }

    // The prepass (shadow/depth) pipeline needs the same packed-decode vertex
    // stage — the default prepass vertex reads POSITION, which chunk meshes
    // no longer carry. The WGSL branches on PREPASS_PIPELINE.
    fn prepass_vertex_shader() -> ShaderRef {
        ShaderRef::Handle(VOXEL_TEXTURE_SHADER)
    }

    // Called for BOTH the forward and prepass pipelines: each must accept the
    // packed-only vertex buffer.
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        let vertex_layout = layout.0.get_layout(&vertex_layout())?;
        descriptor.vertex.buffers = vec![vertex_layout];
        if key.bind_group_data.cutaway {
            descriptor.primitive.cull_mode = None;
        }
        Ok(())
    }
}
