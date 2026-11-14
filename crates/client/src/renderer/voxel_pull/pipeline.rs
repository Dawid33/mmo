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
        let xray_entries = BindGroupLayoutEntries::single(ShaderStages::FRAGMENT, bt::texture_depth_2d());
        let xray_layout = device.create_bind_group_layout("voxel_pull_xray_layout", &xray_entries);
        let xray_layout_desc = BindGroupLayoutDescriptor::new("voxel_pull_xray_layout", &xray_entries);
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
            label: Some(if key.cutout { "voxel_pull_forward_cutout" } else { "voxel_pull_forward" }.into()),
            layout: vec![view_layout, self.chunk_layout_desc.clone(), self.xray_layout_desc.clone()],
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
    fn interior_pass_descriptor(&self, view_layout: BindGroupLayoutDescriptor, key: VoxelPullPipelineKey) -> RenderPipelineDescriptor {
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
            multisample: MultisampleState { count: 1, ..default() },
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
        let fragment = (emulate_unclipped_depth || key.cutaway || key.cutout).then(|| FragmentState {
            shader: self.depth_shader.clone(),
            entry_point: Some("fragment".into()),
            shader_defs: shader_defs.clone(),
            targets: vec![],
            ..default()
        });

        RenderPipelineDescriptor {
            label: Some(if key.cutout { "voxel_pull_depth_cutout" } else { "voxel_pull_depth" }.into()),
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

#[cfg(test)]
pub(crate) mod tests {
    use bevy::render::render_resource::Face;

    /// Compose `source` against stub `imports` and run naga validation on the
    /// result. Shared with `slice_plane`'s shader test.
    pub(crate) fn validate_shader(
        source: String,
        shader_defs: &[bevy::shader::ShaderDefVal],
        imports: &[(&str, &str)],
    ) -> Result<(), bevy::shader::ShaderCacheError> {
        use bevy::asset::{uuid::Uuid, AssetId};
        use bevy::render::render_resource::{DownlevelFlags, WgpuFeatures};
        use bevy::shader::{Shader, ShaderCache, ShaderCacheError, ShaderCacheSource, ValidateShader};

        fn accept_module(
            _: &(),
            source: ShaderCacheSource<'_>,
            _: &ValidateShader,
        ) -> Result<(), ShaderCacheError> {
            let module = match source {
                ShaderCacheSource::Naga(module) => module,
                ShaderCacheSource::Wgsl(source) => naga::front::wgsl::parse_str(&source)
                    .map_err(|error| ShaderCacheError::CreateShaderModule(error.to_string()))?,
                ShaderCacheSource::SpirV(_) => {
                    return Err(ShaderCacheError::CreateShaderModule(
                        "unexpected SPIR-V shader in WGSL validation test".into(),
                    ));
                }
            };
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .map(|_| ())
            .map_err(|error| ShaderCacheError::CreateShaderModule(error.to_string()))
        }

        let mut cache =
            ShaderCache::new((), WgpuFeatures::empty(), DownlevelFlags::all(), accept_module);
        for (index, (path, source)) in imports.iter().enumerate() {
            cache.set_shader(
                AssetId::Uuid {
                    uuid: Uuid::from_u128(index as u128 + 1),
                },
                Shader::from_wgsl((*source).to_owned(), (*path).to_owned()),
            );
        }
        let shader_id = AssetId::Uuid {
            uuid: Uuid::from_u128(imports.len() as u128 + 1),
        };
        cache.set_shader(shader_id, Shader::from_wgsl(source, "shader_under_test.wgsl"));
        cache.get(0, shader_id, shader_defs).map(drop)
    }

    /// Minimal stand-ins for the Bevy modules `voxel_pull.wgsl` imports:
    /// only the names and types it uses, so a typo or a type/uniformity error
    /// in the shader's own code fails here instead of at pipeline creation.
    pub(crate) const IMPORTS: [(&str, &str); 6] = [
        (
            "mesh_view_types.wgsl",
            r#"
#define_import_path bevy_pbr::mesh_view_types
struct Fog { base_color: vec4<f32>, be: vec3<f32>, mode: u32 }
const FOG_MODE_OFF: u32 = 0u;
const FOG_MODE_LINEAR: u32 = 1u;
const FOG_MODE_EXPONENTIAL: u32 = 2u;
const FOG_MODE_EXPONENTIAL_SQUARED: u32 = 3u;
const FOG_MODE_ATMOSPHERIC: u32 = 4u;
const DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT: u32 = 1u;
"#,
        ),
        (
            "mesh_view_bindings.wgsl",
            r#"
#define_import_path bevy_pbr::mesh_view_bindings
#import bevy_pbr::mesh_view_types::Fog
struct TestView {
    clip_from_view: mat4x4<f32>,
    view_from_world: mat4x4<f32>,
    viewport: vec4<f32>,
    world_position: vec3<f32>,
    exposure: f32,
}
struct TestDirectionalLight { color: vec4<f32>, direction_to_light: vec3<f32>, flags: u32 }
struct TestLights {
    ambient_color: vec4<f32>,
    n_directional_lights: u32,
    directional_lights: array<TestDirectionalLight, 16>,
}
@group(0) @binding(0) var<uniform> view: TestView;
@group(0) @binding(1) var<uniform> lights: TestLights;
@group(0) @binding(12) var<uniform> fog: Fog;
"#,
        ),
        (
            "fog.wgsl",
            r#"
#define_import_path bevy_pbr::fog
#import bevy_pbr::mesh_view_types::Fog
fn linear_fog(f: Fog, c: vec4<f32>, d: f32, s: vec3<f32>) -> vec4<f32> { return c; }
fn exponential_fog(f: Fog, c: vec4<f32>, d: f32, s: vec3<f32>) -> vec4<f32> { return c; }
fn exponential_squared_fog(f: Fog, c: vec4<f32>, d: f32, s: vec3<f32>) -> vec4<f32> { return c; }
fn atmospheric_fog(f: Fog, c: vec4<f32>, d: f32, s: vec3<f32>) -> vec4<f32> { return c; }
"#,
        ),
        (
            "shadows.wgsl",
            r#"
#define_import_path bevy_pbr::shadows
fn fetch_directional_shadow(light_id: u32, frag_position: vec4<f32>, surface_normal: vec3<f32>, view_z: f32, frag_coord_xy: vec2<f32>) -> f32 {
    return 1.0;
}
"#,
        ),
        (
            "view_transformations.wgsl",
            r#"
#define_import_path bevy_pbr::view_transformations
fn position_world_to_clip(position: vec3<f32>) -> vec4<f32> { return vec4(position, 1.0); }
"#,
        ),
        (
            "maths.wgsl",
            "#define_import_path bevy_render::maths\nconst PI: f32 = 3.141592653589793;\n",
        ),
    ];

    #[test]
    fn forward_shader_is_valid_wgsl_after_imports() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/shaders/voxel_pull.wgsl"
        ))
        .expect("voxel_pull.wgsl");
        validate_shader(source, &[], &IMPORTS).expect("voxel_pull.wgsl must compose and validate");
    }

    #[test]
    fn depth_shader_is_valid_wgsl_after_imports() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/shaders/voxel_pull_depth.wgsl"
        ))
        .expect("voxel_pull_depth.wgsl");
        validate_shader(source, &[], &IMPORTS)
            .expect("voxel_pull_depth.wgsl must compose and validate");
    }

    /// The z-level cutaway variant (`CUTAWAY`, `renderer/fortress_view.rs`).
    #[test]
    fn forward_shader_is_valid_wgsl_with_cutaway() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/shaders/voxel_pull.wgsl"
        ))
        .expect("voxel_pull.wgsl");
        for defs in [vec!["CUTAWAY"], vec!["CUTAWAY", "DISTANCE_FOG"]] {
            let defs: Vec<bevy::shader::ShaderDefVal> =
                defs.into_iter().map(Into::into).collect();
            validate_shader(source.clone(), &defs, &IMPORTS)
                .unwrap_or_else(|e| panic!("voxel_pull.wgsl with {defs:?}: {e:?}"));
        }
        assert!(source.contains("#ifdef CUTAWAY"), "the forward shader implements CUTAWAY");
        // R10: the cut is confined to the home region (and off at f32::MAX).
        assert!(source.contains("fn cut_applies"), "the forward shader gates the cut to the home region");
        // R20: of the faces lying on the cut only ceilings (face 3 = NegY)
        // collapse — they would cap the dug room below. Tops (face 2) stay:
        // the slice plane discards wherever a top face can lie on the cut.
        assert!(
            source.contains("(face == 3u && on_cut)"),
            "the forward shader collapses ceiling faces on the cut"
        );
        assert!(
            !source.contains("(face == 2u || face == 3u) && on_cut"),
            "the forward shader keeps top faces on the cut"
        );
        // The designation overlay at binding 7, read in every variant.
        assert!(
            source.contains("@group(1) @binding(7) var overlay: texture_3d<u32>;"),
            "the forward shader binds the designation overlay at 7"
        );
        assert!(source.contains("overlay_tint("), "the forward shader tints by the overlay");
    }

    /// The top-down view (`commander/top_down.rs`) is orthographic: the
    /// cut's cap and the riser fade must not assume a perspective eye.
    #[test]
    fn ortho_paths_are_in_the_forward_shader() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/shaders/voxel_pull.wgsl"
        ))
        .expect("voxel_pull.wgsl");
        assert!(source.contains("fn view_is_orthographic"), "the forward shader tells an orthographic view");
        assert!(source.contains("fn view_forward"), "the cap's eye ray follows the view under ortho");
        let defs: Vec<bevy::shader::ShaderDefVal> = vec!["CUTAWAY".into()];
        validate_shader(source, &defs, &IMPORTS).expect("voxel_pull.wgsl with CUTAWAY");
    }

    /// The depth/shadow pipelines with the cutaway, with and without the
    /// unclipped-depth emulation's fragment stage.
    #[test]
    fn depth_shader_is_valid_wgsl_with_cutaway() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/shaders/voxel_pull_depth.wgsl"
        ))
        .expect("voxel_pull_depth.wgsl");
        for defs in [vec!["CUTAWAY"], vec!["CUTAWAY", "UNCLIPPED_DEPTH_ORTHO_EMULATION"]] {
            let defs: Vec<bevy::shader::ShaderDefVal> =
                defs.into_iter().map(Into::into).collect();
            validate_shader(source.clone(), &defs, &IMPORTS)
                .unwrap_or_else(|e| panic!("voxel_pull_depth.wgsl with {defs:?}: {e:?}"));
        }
        assert!(source.contains("#ifdef CUTAWAY"), "the depth shader implements CUTAWAY");
        // R10: the cut is confined to the home region (and off at f32::MAX).
        assert!(source.contains("fn cut_applies"), "the depth shader gates the cut to the home region");
        // R20: of the faces lying on the cut only ceilings (face 3 = NegY)
        // collapse — they would cap the dug room below. Tops (face 2) stay:
        // the slice plane discards wherever a top face can lie on the cut.
        assert!(
            source.contains("(face == 3u && on_cut)"),
            "the depth shader collapses ceiling faces on the cut"
        );
        assert!(
            !source.contains("(face == 2u || face == 3u) && on_cut"),
            "the depth shader keeps top faces on the cut"
        );
    }

    fn shader_source(name: &str) -> String {
        std::fs::read_to_string(format!("{}/../../assets/shaders/{name}", env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    fn validate_variants(name: &str, variants: &[&[&str]]) {
        let source = shader_source(name);
        for defs in variants {
            let defs: Vec<bevy::shader::ShaderDefVal> = defs.iter().map(|d| (*d).into()).collect();
            validate_shader(source.clone(), &defs, &IMPORTS)
                .unwrap_or_else(|e| panic!("{name} with {defs:?}: {e:?}"));
        }
    }

    /// The cutout pass (leaves): alpha test, double-sided, with and without
    /// the cutaway and fog.
    #[test]
    fn forward_shader_is_valid_wgsl_with_cutout() {
        validate_variants(
            "voxel_pull.wgsl",
            &[&["CUTOUT"], &["CUTOUT", "DISTANCE_FOG"], &["CUTOUT", "CUTAWAY"], &["CUTOUT", "CUTAWAY", "DISTANCE_FOG"]],
        );
        let source = shader_source("voxel_pull.wgsl");
        assert!(source.contains("#ifdef CUTOUT"), "the forward shader implements CUTOUT");
        // The alpha test is the shader's last discard, after every derivative use.
        let test = source.find("coverage < CUTOUT_ALPHA_CUTOFF").expect("the alpha test");
        let boundaries = source.find("let overlaid = apply_boundaries(").expect("apply_boundaries call");
        assert!(test > boundaries, "the alpha discard must follow apply_boundaries' fwidth");
    }

    /// The dug-space x-ray: the forward variants that read the interior depth
    /// (opaque and cutout, with fog) and the interior pass's vertex stage.
    #[test]
    fn forward_shader_is_valid_wgsl_with_xray_and_interior_pass() {
        validate_variants(
            "voxel_pull.wgsl",
            &[
                &["XRAY"],
                &["XRAY", "DISTANCE_FOG"],
                &["XRAY", "CUTOUT"],
                &["XRAY", "CUTOUT", "DISTANCE_FOG"],
                &["INTERIOR_PASS"],
            ],
        );
        let source = shader_source("voxel_pull.wgsl");
        // Its discard follows every derivative use, like the cut's.
        let discard = source.find("if (xray_hides(in.position, in.interior))").expect("the x-ray discard");
        let boundaries = source.find("let overlaid = apply_boundaries(").expect("apply_boundaries call");
        assert!(discard > boundaries, "the x-ray discard must follow apply_boundaries' fwidth");
        assert!(source.contains("out.interior = (w0 >> 31u) & 1u;"), "INTERIOR_BIT is w0 bit 31");
        assert_eq!(crate::renderer::meshing::records::INTERIOR_BIT, 31);
    }

    /// The interior pass culls back faces (the near side of a space is its
    /// interior faces seen from behind) and the x-ray forward variant culls
    /// like the plain one.
    #[test]
    fn xray_variants_cull_back_faces() {
        assert_eq!(super::cull_mode(false, false), Some(Face::Back));
    }

    /// The cutout depth prepass / shadow variants: a fragment stage that
    /// samples the texture (bindings 2/3 of the shared layout) — alone, with
    /// the cutaway, and with the unclipped-depth emulation.
    #[test]
    fn depth_shader_is_valid_wgsl_with_cutout() {
        validate_variants(
            "voxel_pull_depth.wgsl",
            &[
                &["CUTOUT"],
                &["CUTOUT", "CUTAWAY"],
                &["CUTOUT", "UNCLIPPED_DEPTH_ORTHO_EMULATION"],
                &["CUTOUT", "CUTAWAY", "UNCLIPPED_DEPTH_ORTHO_EMULATION"],
            ],
        );
        let source = shader_source("voxel_pull_depth.wgsl");
        assert!(source.contains("@group(1) @binding(2) var mat_array_texture: texture_2d_array<f32>;"));
        assert!(source.contains("@group(1) @binding(3) var mat_array_texture_sampler: sampler;"));
    }

    /// The colour and depth passes must agree on which texels a leaf drops,
    /// or shadows and prepass depth show holes the colour pass fills.
    #[test]
    fn cutout_alpha_test_is_identical_in_both_shaders() {
        fn block(source: &str) -> &str {
            let start = source.find("// BEGIN cutout alpha test").expect("BEGIN marker");
            let end = source.find("// END cutout alpha test").expect("END marker");
            &source[start..end]
        }
        let (forward, depth) = (shader_source("voxel_pull.wgsl"), shader_source("voxel_pull_depth.wgsl"));
        assert_eq!(block(&forward), block(&depth));
    }

    /// Both passes texture a quad from the same position (or the cutout
    /// depth pass drops other texels than the colour pass draws), and that
    /// position follows the chunk's own axes so a rotated voxel body's
    /// texture turns with it; the forward pass rotates the face normal too.
    #[test]
    fn texture_position_is_identical_in_both_shaders_and_normals_follow_the_transform() {
        fn block(source: &str) -> &str {
            let start = source.find("// BEGIN texture_position").expect("BEGIN marker");
            let end = source.find("// END texture_position").expect("END marker");
            &source[start..end]
        }
        let (forward, depth) = (shader_source("voxel_pull.wgsl"), shader_source("voxel_pull_depth.wgsl"));
        assert_eq!(block(&forward), block(&depth));
        for source in [&forward, &depth] {
            assert!(source.contains("let tex_pos = texture_position(world_from_local, local_pos, scale);"));
        }
        assert!(forward.contains(
            "out.world_normal = normalize((world_from_local * vec4<f32>(FACE_NORMALS[face], 0.0)).xyz);"
        ));
    }

    /// The opaque pipeline gains no alpha discard, and only cutout draws
    /// double-sided.
    #[test]
    fn only_the_cutout_variant_is_double_sided() {
        assert_eq!(super::cull_mode(false, false), Some(Face::Back));
        assert_eq!(super::cull_mode(false, true), None);
        assert_eq!(super::cull_mode(true, false), None);
    }

    /// Every def combination the four cull pipelines compile with.
    #[test]
    fn cull_shader_is_valid_wgsl_for_every_pipeline_variant() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/shaders/voxel_pull_cull.wgsl"
        ))
        .expect("voxel_pull_cull.wgsl");
        let variants: [&[&str]; 4] = [&[], &["INDIRECT_COUNT"], &["HZB"], &["HZB", "INDIRECT_COUNT"]];
        for defs in variants {
            let defs: Vec<bevy::shader::ShaderDefVal> =
                defs.iter().map(|d| (*d).into()).collect();
            validate_shader(source.clone(), &defs, &[])
                .unwrap_or_else(|e| panic!("cull shader with {defs:?}: {e:?}"));
        }
    }
}
