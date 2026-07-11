//! The specialized forward pipeline for the pulled render path. The bind group
//! layout here is LOCKED to `assets/shaders/voxel_pull.wgsl` group(1); group(0)
//! is the mesh view layout, reused verbatim so `SetMeshViewBindGroup<0>` binds a
//! compatible view bind group.

use bevy::{
    core_pipeline::core_3d::CORE_3D_DEPTH_FORMAT,
    pbr::{MeshPipelineKey, MeshPipelineViewLayouts},
    prelude::*,
    render::{
        render_resource::{binding_types as bt, *},
        renderer::RenderDevice,
        view::ViewTarget,
    },
};

/// One `mat4x4 + u32`, `std140`-laid-out to match the WGSL `ChunkUniform`.
#[derive(Clone, Copy, ShaderType)]
pub struct ChunkUniform {
    pub world_from_local: Mat4,
    pub quad_count: u32,
}

#[derive(Resource)]
pub struct VoxelPullPipeline {
    /// Real layout used to build per-chunk bind groups in `prepare`.
    pub chunk_layout: BindGroupLayout,
    /// Descriptor form of the same layout, for the pipeline's `layout` vec
    /// (0.18 pipelines describe layouts, the cache realizes them).
    pub chunk_layout_desc: BindGroupLayoutDescriptor,
    pub forward_shader: Handle<Shader>,
    pub view_layouts: MeshPipelineViewLayouts,
}

impl FromWorld for VoxelPullPipeline {
    fn from_world(world: &mut World) -> Self {
        let device = world.resource::<RenderDevice>();
        // Binding order LOCKED to voxel_pull.wgsl group(1):
        //   0 storage quads, 1 uniform ChunkUniform, 2 texture_2d_array, 3 sampler.
        let entries = BindGroupLayoutEntries::sequential(
            ShaderStages::VERTEX_FRAGMENT,
            (
                bt::storage_buffer_read_only::<[u32; 2]>(false),
                bt::uniform_buffer::<ChunkUniform>(false),
                bt::texture_2d_array(TextureSampleType::Float { filterable: true }),
                bt::sampler(SamplerBindingType::Filtering),
            ),
        );
        let chunk_layout = device.create_bind_group_layout("voxel_pull_chunk_layout", &entries);
        let chunk_layout_desc = BindGroupLayoutDescriptor::new("voxel_pull_chunk_layout", &entries);
        Self {
            chunk_layout,
            chunk_layout_desc,
            forward_shader: world.load_asset("shaders/voxel_pull.wgsl"),
            view_layouts: world.resource::<MeshPipelineViewLayouts>().clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoxelPullPipelineKey {
    pub msaa_samples: u32,
    pub hdr: bool,
}

impl SpecializedRenderPipeline for VoxelPullPipeline {
    type Key = VoxelPullPipelineKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        // The group-0 layout must match the view bind group `SetMeshViewBindGroup`
        // binds for this view — derive the same key bevy_pbr does and take its
        // `main_layout` (bevy_pbr-0.18.1/src/render/mesh.rs, MeshPipeline::specialize).
        let mesh_key = MeshPipelineKey::from_msaa_samples(key.msaa_samples)
            | MeshPipelineKey::from_hdr(key.hdr);
        let view_layout = self
            .view_layouts
            .get_view_layout(mesh_key.into())
            .main_layout
            .clone();
        RenderPipelineDescriptor {
            label: Some("voxel_pull_forward".into()),
            layout: vec![view_layout, self.chunk_layout_desc.clone()],
            vertex: VertexState {
                shader: self.forward_shader.clone(),
                entry_point: Some("vertex".into()),
                buffers: vec![], // no vertex buffers — geometry is pulled from the storage buffer
                ..default()
            },
            fragment: Some(FragmentState {
                shader: self.forward_shader.clone(),
                entry_point: Some("fragment".into()),
                targets: vec![Some(ColorTargetState {
                    format: if key.hdr {
                        ViewTarget::TEXTURE_FORMAT_HDR
                    } else {
                        TextureFormat::bevy_default()
                    },
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
                ..default()
            }),
            primitive: PrimitiveState {
                cull_mode: Some(Face::Back),
                ..default()
            },
            depth_stencil: Some(DepthStencilState {
                format: CORE_3D_DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: CompareFunction::GreaterEqual, // Bevy reverse-Z
                stencil: default(),
                bias: default(),
            }),
            multisample: MultisampleState {
                count: key.msaa_samples,
                ..default()
            },
            ..default()
        }
    }
}
