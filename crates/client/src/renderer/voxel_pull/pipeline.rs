//! The specialized forward pipeline for the pulled render path. The bind group
//! layout here is LOCKED to `assets/shaders/voxel_pull.wgsl` group(1); group(0)
//! is the mesh view layout, reused verbatim so `SetMeshViewBindGroup<0>` binds a
//! compatible view bind group.

use bevy::{
    core_pipeline::core_3d::CORE_3D_DEPTH_FORMAT,
    pbr::{MeshPipelineViewLayoutKey, MeshPipelineViewLayouts, PrepassPipeline},
    prelude::*,
    render::{
        render_resource::{binding_types as bt, *},
        renderer::RenderDevice,
        view::ViewTarget,
    },
    shader::ShaderDefVal,
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
    /// The group(0) view-layout key. Must be derived from the view's *actual*
    /// prepass state (MSAA + which prepass textures exist), exactly as
    /// `prepare_mesh_view_bind_groups` does (bevy_pbr-0.18.1/src/render/
    /// mesh_view_bindings.rs) — the view bind group `SetMeshViewBindGroup<0>`
    /// binds is built for that layout, and our pipeline's group(0) must match it
    /// or wgpu rejects the draw. Our camera runs `DepthPrepass`, so this carries
    /// the `DEPTH_PREPASS` bit (adds the depth-texture binding to the layout);
    /// omitting it — as the first cut did — only worked because the pull-smoke
    /// probe had no prepass, reducing the key to bare MSAA.
    pub view_layout_key: MeshPipelineViewLayoutKey,
}

/// `shadows.wgsl`'s `sample_shadow_map` (bevy_pbr-0.18.1/src/render/shadow_sampling.wgsl)
/// is gated on one `SHADOW_FILTER_METHOD_*` def with no library-level default —
/// `MeshPipeline::specialize` derives it per-view from the camera's optional
/// `ShadowFilteringMethod` component (bevy_pbr-0.18.1/src/render/mesh.rs). We
/// have no per-view key plumbing here, so hardcode the method's own default,
/// `ShadowFilteringMethod::Gaussian` (bevy_light-0.18.1/src/lib.rs), which is
/// what any camera without an explicit override — ours included — renders with.
fn shadow_filter_shader_defs() -> Vec<ShaderDefVal> {
    vec!["SHADOW_FILTER_METHOD_GAUSSIAN".into()]
}

impl SpecializedRenderPipeline for VoxelPullPipeline {
    type Key = VoxelPullPipelineKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        // The group-0 layout must match the view bind group `SetMeshViewBindGroup`
        // binds for this view. `queue_voxel_pull` derives `view_layout_key` from
        // the view's real prepass state (mirroring `prepare_mesh_view_bind_groups`),
        // so this takes the matching `main_layout` directly.
        let view_layout = self
            .view_layouts
            .get_view_layout(key.view_layout_key)
            .main_layout
            .clone();
        RenderPipelineDescriptor {
            label: Some("voxel_pull_forward".into()),
            layout: vec![view_layout, self.chunk_layout_desc.clone()],
            vertex: VertexState {
                shader: self.forward_shader.clone(),
                entry_point: Some("vertex".into()),
                shader_defs: shadow_filter_shader_defs(),
                buffers: vec![], // no vertex buffers — geometry is pulled from the storage buffer
                ..default()
            },
            fragment: Some(FragmentState {
                shader: self.forward_shader.clone(),
                entry_point: Some("fragment".into()),
                shader_defs: shadow_filter_shader_defs(),
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

/// Vertex-only depth-prepass pipeline for the pulled path. Writes only depth so
/// the camera's HZB still sees pulled terrain (mesh entities behind it keep
/// being occlusion-culled). Bound against the PREPASS view layout at group(0);
/// reuses the forward path's group(1) chunk layout verbatim so the existing
/// `GpuChunkQuads.bind_group` stays valid.
///
/// Built from `PrepassPipeline`, which only exists after `RenderStartup` (it is
/// inserted by `init_prepass_pipeline`, a system, not a `FromWorld`), so unlike
/// `VoxelPullPipeline` this resource is populated by `init_voxel_pull_depth_pipeline`
/// rather than at `finish()` time.
#[derive(Resource)]
pub struct VoxelPullDepthPipeline {
    /// Same per-chunk layout `VoxelPullPipeline` builds — cloned so both
    /// pipelines describe an identical group(1) and share one bind group.
    pub chunk_layout_desc: BindGroupLayoutDescriptor,
    /// The prepass view layout (`PrepassPipeline::view_layout_no_motion_vectors`),
    /// which `SetPrepassViewBindGroup<0>` binds against.
    pub prepass_view_layout: BindGroupLayoutDescriptor,
    pub depth_shader: Handle<Shader>,
    /// Whether the render device exposes `DEPTH_CLIP_CONTROL`, cloned from
    /// `PrepassPipeline` (bevy_pbr-0.18.1/src/prepass/mod.rs). Decides, for
    /// directional-cascade shadow views, between native unclipped depth and the
    /// `UNCLIPPED_DEPTH_ORTHO_EMULATION` fragment-shader emulation.
    pub depth_clip_control_supported: bool,
}

/// Populates [`VoxelPullDepthPipeline`] once `PrepassPipeline` exists. Ordered
/// `after(init_prepass_pipeline)` in the plugin so the prepass view layout is
/// ready to clone.
pub fn init_voxel_pull_depth_pipeline(
    mut commands: Commands,
    forward: Res<VoxelPullPipeline>,
    prepass: Res<PrepassPipeline>,
    asset_server: Res<AssetServer>,
) {
    commands.insert_resource(VoxelPullDepthPipeline {
        chunk_layout_desc: forward.chunk_layout_desc.clone(),
        prepass_view_layout: prepass.view_layout_no_motion_vectors.clone(),
        depth_shader: asset_server.load("shaders/voxel_pull_depth.wgsl"),
        depth_clip_control_supported: prepass.depth_clip_control_supported,
    });
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoxelPullDepthPipelineKey {
    /// The prepass depth attachment is allocated at the view's MSAA sample count,
    /// so the depth pipeline must match it (mirrors `PrepassPipeline::specialize`,
    /// which uses `mesh_key.msaa_samples()`). Shadow-map views are single-sampled.
    pub msaa_samples: u32,
    /// Set for directional-light cascade shadow views, mirroring the
    /// `MeshPipelineKey::UNCLIPPED_DEPTH_ORTHO` bit bevy sets in
    /// `check_views_lights_need_specialization` (bevy_pbr-0.18.1/src/render/light.rs).
    /// Enables unclipped depth (native or emulated) so shadow casters between the
    /// light and the cascade near plane are not clipped away. Always `false` for
    /// the camera depth prepass.
    pub unclipped_depth_ortho: bool,
}

impl SpecializedRenderPipeline for VoxelPullDepthPipeline {
    type Key = VoxelPullDepthPipelineKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        // Directional cascade shadow views want unclipped depth (see the key doc).
        // Mirror `PrepassPipeline::specialize` (bevy_pbr-0.18.1/src/prepass/mod.rs
        // lines 429-442): use the native `DEPTH_CLIP_CONTROL` device feature when
        // present, otherwise emulate by clamping clip-space z in the vertex shader
        // and writing the true depth to `@builtin(frag_depth)` in a fragment stage
        // gated on `UNCLIPPED_DEPTH_ORTHO_EMULATION`. The camera prepass never sets
        // `unclipped_depth_ortho`, so both are false there and this stays vertex-only.
        let emulate_unclipped_depth =
            key.unclipped_depth_ortho && !self.depth_clip_control_supported;
        let unclipped_depth = key.unclipped_depth_ortho && self.depth_clip_control_supported;

        let mut shader_defs = Vec::new();
        if emulate_unclipped_depth {
            shader_defs.push(ShaderDefVal::from("UNCLIPPED_DEPTH_ORTHO_EMULATION"));
        }

        // Emulation needs a fragment stage to write `frag_depth`; otherwise the
        // pass stays depth-only with no color targets — matching
        // `PrepassPipeline::specialize` when all prepass targets are absent.
        let fragment = emulate_unclipped_depth.then(|| FragmentState {
            shader: self.depth_shader.clone(),
            entry_point: Some("fragment".into()),
            shader_defs: shader_defs.clone(),
            targets: vec![],
            ..default()
        });

        RenderPipelineDescriptor {
            label: Some("voxel_pull_depth".into()),
            layout: vec![
                self.prepass_view_layout.clone(),
                self.chunk_layout_desc.clone(),
            ],
            vertex: VertexState {
                shader: self.depth_shader.clone(),
                entry_point: Some("vertex".into()),
                shader_defs,
                buffers: vec![], // geometry is pulled from the storage buffer
                ..default()
            },
            fragment,
            primitive: PrimitiveState {
                cull_mode: Some(Face::Back),
                unclipped_depth,
                ..default()
            },
            // Stencil/bias copied verbatim from `PrepassPipeline::specialize` so
            // the depth attachment's write path matches the mesh prepass.
            depth_stencil: Some(DepthStencilState {
                format: CORE_3D_DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: CompareFunction::GreaterEqual, // Bevy reverse-Z
                stencil: StencilState {
                    front: StencilFaceState::IGNORE,
                    back: StencilFaceState::IGNORE,
                    read_mask: 0,
                    write_mask: 0,
                },
                bias: DepthBiasState {
                    constant: 0,
                    slope_scale: 0.0,
                    clamp: 0.0,
                },
            }),
            multisample: MultisampleState {
                count: key.msaa_samples,
                ..default()
            },
            ..default()
        }
    }
}
