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
    },
    shader::ShaderDefVal,
};

use crate::renderer::boundaries::BoundaryUniform;
use crate::renderer::fortress_view::FortressViewUniform;

#[derive(Resource)]
pub struct VoxelPullPipeline {
    /// Real layout used to build the shared arena bind group in `prepare`.
    pub chunk_layout: BindGroupLayout,
    /// Descriptor form of the same layout, for the pipeline's `layout` vec
    /// (0.18 pipelines describe layouts, the cache realizes them).
    pub chunk_layout_desc: BindGroupLayoutDescriptor,
    /// group(2) of the forward pipelines: the x-ray's interior depth
    /// (`voxel_pull.wgsl` `xray_depth`, read under `XRAY` only; bound for
    /// every forward draw — a 1×1 stand-in when the view has none —
    /// `xray::SetXrayBindGroup`).
    pub xray_layout: BindGroupLayout,
    pub xray_layout_desc: BindGroupLayoutDescriptor,
    pub forward_shader: Handle<Shader>,
    pub view_layouts: MeshPipelineViewLayouts,
}

impl FromWorld for VoxelPullPipeline {
    fn from_world(world: &mut World) -> Self {
        let device = world.resource::<RenderDevice>();
        // Binding order LOCKED to voxel_pull.wgsl group(1):
        //   0 storage quads, 1 storage transforms, 2 texture_2d_array, 3 sampler,
        //   4 storage top_layers (fragment only — keeps the vertex stage at two
        //   storage buffers), 5 uniform TerrainBoundaries (fragment only; the
        //   region boundary overlay, `renderer/boundaries.rs`), 6 uniform
        //   FortressView (vertex + fragment; the z-level cutaway,
        //   `renderer/fortress_view.rs`), 7 texture_3d<u32> designation
        //   overlay (fragment only, R8Uint, no sampler: textureLoad; the
        //   same file's `OverlayTexture`).
        let entries = BindGroupLayoutEntries::sequential(
            ShaderStages::VERTEX_FRAGMENT,
            (
                bt::storage_buffer_read_only::<[u32; 2]>(false),
                bt::storage_buffer_read_only::<Mat4>(false),
                bt::texture_2d_array(TextureSampleType::Float { filterable: true }),
                bt::sampler(SamplerBindingType::Filtering),
                bt::storage_buffer_read_only::<u32>(false).visibility(ShaderStages::FRAGMENT),
                bt::uniform_buffer::<BoundaryUniform>(false).visibility(ShaderStages::FRAGMENT),
                bt::uniform_buffer::<FortressViewUniform>(false),
                bt::texture_3d(TextureSampleType::Uint).visibility(ShaderStages::FRAGMENT),
            ),
        );
        let chunk_layout = device.create_bind_group_layout("voxel_pull_chunk_layout", &entries);
        let chunk_layout_desc = BindGroupLayoutDescriptor::new("voxel_pull_chunk_layout", &entries);
        let xray_entries =
            BindGroupLayoutEntries::single(ShaderStages::FRAGMENT, bt::texture_depth_2d());
        let xray_layout = device.create_bind_group_layout("voxel_pull_xray_layout", &xray_entries);
        let xray_layout_desc =
            BindGroupLayoutDescriptor::new("voxel_pull_xray_layout", &xray_entries);
        Self {
            chunk_layout,
            chunk_layout_desc,
            xray_layout,
            xray_layout_desc,
            forward_shader: super::VOXEL_PULL_SHADER,
            view_layouts: world.resource::<MeshPipelineViewLayouts>().clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoxelPullPipelineKey {
    pub msaa_samples: u32,
    /// The view's colour target format (Bevy 0.19 dropped the `hdr` flag).
    pub target_format: TextureFormat,
    /// The group(0) view-layout key. Must be derived from the view's *actual*
    /// prepass state (MSAA + which prepass textures exist), exactly as
    /// `prepare_mesh_view_bind_groups` does (bevy_pbr-0.18.1/src/render/
    /// mesh_view_bindings.rs) — the view bind group `SetMeshViewBindGroup<0>`
    /// binds is built for that layout, and our pipeline's group(0) must match it
    /// or wgpu rejects the draw. With `VOXEL_OCCLUSION=on` the camera runs
    /// `DepthPrepass`, so this carries the `DEPTH_PREPASS` bit (adds the depth-texture binding to the layout);
    /// omitting it — as the first cut did — only worked because the pull-smoke
    /// probe had no prepass, reducing the key to bare MSAA.
    pub view_layout_key: MeshPipelineViewLayoutKey,
    /// The z-level cut is on (`fortress_view::cutaway_active`): `CUTAWAY`
    /// shader def and no back-face culling (the cut's cap is back faces).
    pub cutaway: bool,
    /// The cutout pass (`AlphaMask3d`): draws the chunks' cutout record
    /// ranges with the `CUTOUT` def (alpha test, double-sided). The opaque
    /// pipeline never discards for alpha.
    pub cutout: bool,
    /// The dug-space x-ray (cut off, `xray.rs`): `XRAY` — terrain in front
    /// of the view's interior depth is discarded (dithered), interior faces
    /// lit as `xray::INTERIOR_AMBIENT_GAIN` says. Never with `cutaway`.
    pub xray: bool,
    /// The x-ray's interior depth pass itself (`INTERIOR_PASS`): only quads
    /// with `records::INTERIOR_BIT`, depth only, single-sampled into the
    /// view's own interior depth texture (`xray::ViewXray`); `msaa_samples`
    /// is 1 and `target_format` unused.
    pub interior_pass: bool,
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

/// Forward-pass defs: the shadow filter plus `DISTANCE_FOG` when the view
/// binds a fog uniform (Bevy 0.19 only binds it for views with `DistanceFog`),
/// and `CUTAWAY` while the z-level cut is on.
fn forward_shader_defs(key: VoxelPullPipelineKey) -> Vec<ShaderDefVal> {
    let mut defs = shadow_filter_shader_defs();
    if key
        .view_layout_key
        .contains(MeshPipelineViewLayoutKey::DISTANCE_FOG)
    {
        defs.push("DISTANCE_FOG".into());
    }
    if key.cutaway {
        defs.push("CUTAWAY".into());
    }
    if key.cutout {
        defs.push("CUTOUT".into());
    }
    if key.xray {
        defs.push("XRAY".into());
    }
    if key.interior_pass {
        defs.push("INTERIOR_PASS".into());
    }
    defs
}

/// Back faces are culled except under the cutaway, whose cap is drawn from
/// the back faces showing through the cut, and for cutout quads, which are
/// double-sided: through a leaf's holes the far side of the crown shows.
fn cull_mode(cutaway: bool, cutout: bool) -> Option<Face> {
    (!cutaway && !cutout).then_some(Face::Back)
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
        if key.interior_pass {
            return self.interior_pass_descriptor(view_layout, key);
        }
        RenderPipelineDescriptor {
            label: Some(
                if key.cutout {
                    "voxel_pull_forward_cutout"
                } else {
                    "voxel_pull_forward"
                }
                .into(),
            ),
            layout: vec![
                view_layout,
                self.chunk_layout_desc.clone(),
                self.xray_layout_desc.clone(),
            ],
            vertex: VertexState {
                shader: self.forward_shader.clone(),
                entry_point: Some("vertex".into()),
                shader_defs: forward_shader_defs(key),
                buffers: vec![], // no vertex buffers — geometry is pulled from the storage buffer
                ..default()
            },
            fragment: Some(FragmentState {
                shader: self.forward_shader.clone(),
                entry_point: Some("fragment".into()),
                shader_defs: forward_shader_defs(key),
                targets: vec![Some(ColorTargetState {
                    format: key.target_format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
                ..default()
            }),
            primitive: PrimitiveState {
                cull_mode: cull_mode(key.cutaway, key.cutout),
                ..default()
            },
            depth_stencil: Some(DepthStencilState {
                format: CORE_3D_DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(CompareFunction::GreaterEqual), // Bevy reverse-Z
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

impl VoxelPullPipeline {
    /// The x-ray's interior depth pass: the forward shader's vertex stage
    /// (collapsing every non-interior quad), no fragment stage, back faces
    /// culled, into a single-sampled `CORE_3D_DEPTH_FORMAT` target. Its layout
    /// stops at group(1): group(2) samples the very texture it renders into.
    fn interior_pass_descriptor(
        &self,
        view_layout: BindGroupLayoutDescriptor,
        key: VoxelPullPipelineKey,
    ) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("voxel_pull_xray_interior".into()),
            layout: vec![view_layout, self.chunk_layout_desc.clone()],
            vertex: VertexState {
                shader: self.forward_shader.clone(),
                entry_point: Some("vertex".into()),
                shader_defs: forward_shader_defs(key),
                buffers: vec![],
                ..default()
            },
            fragment: None,
            primitive: PrimitiveState {
                cull_mode: Some(Face::Back),
                ..default()
            },
            depth_stencil: Some(DepthStencilState {
                format: CORE_3D_DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(CompareFunction::GreaterEqual), // Bevy reverse-Z
                stencil: default(),
                bias: default(),
            }),
            multisample: MultisampleState {
                count: 1,
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
    forward: Option<Res<VoxelPullPipeline>>,
    prepass: Res<PrepassPipeline>,
) {
    // Absent on a device that cannot run the pull path (`VoxelPullPlugin::finish`).
    let Some(forward) = forward else {
        return;
    };
    commands.insert_resource(VoxelPullDepthPipeline {
        chunk_layout_desc: forward.chunk_layout_desc.clone(),
        prepass_view_layout: prepass.view_layout_no_motion_vectors.clone(),
        depth_shader: super::VOXEL_PULL_DEPTH_SHADER,
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
    /// The z-level cut is on: `CUTAWAY` (vertex collapse + fragment discard
    /// above the cut) and no back-face culling, as in the forward pass.
    pub cutaway: bool,
    /// The cutout ranges (`AlphaMask3dPrepass` / the cutout `Shadow` item):
    /// `CUTOUT`, a fragment stage that samples the block texture (group(1)
    /// bindings 2/3) and discards the transparent texels; double-sided.
    pub cutout: bool,
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
        if key.cutaway {
            shader_defs.push(ShaderDefVal::from("CUTAWAY"));
        }
        if key.cutout {
            shader_defs.push(ShaderDefVal::from("CUTOUT"));
        }

        // Emulation needs a fragment stage to write `frag_depth`, the cutaway
        // one to discard above the cut, the cutout one to discard transparent
        // texels; otherwise the pass stays depth-only with no color targets —
        // matching `PrepassPipeline::specialize` when all prepass targets are
        // absent.
        let fragment =
            (emulate_unclipped_depth || key.cutaway || key.cutout).then(|| FragmentState {
                shader: self.depth_shader.clone(),
                entry_point: Some("fragment".into()),
                shader_defs: shader_defs.clone(),
                targets: vec![],
                ..default()
            });

        RenderPipelineDescriptor {
            label: Some(
                if key.cutout {
                    "voxel_pull_depth_cutout"
                } else {
                    "voxel_pull_depth"
                }
                .into(),
            ),
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
                cull_mode: cull_mode(key.cutaway, key.cutout),
                unclipped_depth,
                ..default()
            },
            // Stencil/bias copied verbatim from `PrepassPipeline::specialize` so
            // the depth attachment's write path matches the mesh prepass.
            depth_stencil: Some(DepthStencilState {
                format: CORE_3D_DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(CompareFunction::GreaterEqual), // Bevy reverse-Z
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
