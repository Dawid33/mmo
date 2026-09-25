use bevy::{
    core_pipeline::core_3d::CORE_3D_DEPTH_FORMAT,
    pbr::{MeshPipelineViewLayoutKey, MeshPipelineViewLayouts},
    prelude::*,
    render::{
        render_resource::{binding_types as bt, *},
        renderer::{RenderAdapter, RenderDevice},
    },
    shader::ShaderDefVal,
};

use crate::renderer::block_textures::BlockTextureLayers;

pub const PROXY_VERTEX_COUNT: u32 = 36;
pub const MATERIAL_LUT_EDGE: usize = 256;
pub const MATERIAL_LUT_TEXELS: usize = MATERIAL_LUT_EDGE * MATERIAL_LUT_EDGE;
pub const FAR_PASS_REQUIRED_STORAGE_BUFFERS_PER_STAGE: u32 = 8;
pub const FAR_CULL_WORKGROUP_SIZE: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FarPassDeviceCapabilities {
    pub compute_shaders: bool,
    pub indirect_execution: bool,
    pub max_storage_buffers_per_shader_stage: u32,
    pub max_compute_workgroup_size_x: u32,
    pub max_compute_invocations_per_workgroup: u32,
    pub max_compute_workgroups_per_dimension: u32,
    pub max_texture_dimension_2d: u32,
}

impl FarPassDeviceCapabilities {
    pub const fn supports_far_pass(self) -> bool {
        self.compute_shaders
            && self.indirect_execution
            && self.max_storage_buffers_per_shader_stage
                >= FAR_PASS_REQUIRED_STORAGE_BUFFERS_PER_STAGE
            && self.max_compute_workgroup_size_x >= FAR_CULL_WORKGROUP_SIZE
            && self.max_compute_invocations_per_workgroup >= FAR_CULL_WORKGROUP_SIZE
            && self.max_compute_workgroups_per_dimension > 0
            && self.max_texture_dimension_2d >= MATERIAL_LUT_EDGE as u32
    }
}

pub fn far_pass_device_capabilities(
    device: &RenderDevice,
    adapter: &RenderAdapter,
) -> FarPassDeviceCapabilities {
    let limits = device.limits();
    let downlevel = adapter.get_downlevel_capabilities().flags;
    FarPassDeviceCapabilities {
        compute_shaders: downlevel.contains(DownlevelFlags::COMPUTE_SHADERS),
        indirect_execution: downlevel.contains(DownlevelFlags::INDIRECT_EXECUTION),
        max_storage_buffers_per_shader_stage: limits.max_storage_buffers_per_shader_stage,
        max_compute_workgroup_size_x: limits.max_compute_workgroup_size_x,
        max_compute_invocations_per_workgroup: limits.max_compute_invocations_per_workgroup,
        max_compute_workgroups_per_dimension: limits.max_compute_workgroups_per_dimension,
        max_texture_dimension_2d: limits.max_texture_dimension_2d,
    }
}

pub const fn empty_proxy_draw_args() -> [u32; 4] {
    [PROXY_VERTEX_COUNT, 0, 0, 0]
}

pub const fn material_lut_bytes() -> usize {
    MATERIAL_LUT_TEXELS * std::mem::size_of::<[u16; 4]>()
}

pub fn encode_material_lut(layers: &BlockTextureLayers) -> Vec<[u16; 4]> {
    let mut encoded = vec![[0; 4]; MATERIAL_LUT_TEXELS];
    for (block, [top, side, bottom]) in &layers.0 {
        encoded[block.0 as usize] = [
            (*top).min(u16::MAX as u32) as u16,
            (*side).min(u16::MAX as u32) as u16,
            (*bottom).min(u16::MAX as u32) as u16,
            0,
        ];
    }
    encoded
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindingContract {
    pub storage: bool,
    pub stages: ShaderStages,
}

pub fn far_render_binding_contract() -> [BindingContract; 8] {
    [
        BindingContract {
            storage: true,
            stages: ShaderStages::VERTEX_FRAGMENT,
        },
        BindingContract {
            storage: true,
            stages: ShaderStages::FRAGMENT,
        },
        BindingContract {
            storage: true,
            stages: ShaderStages::FRAGMENT,
        },
        BindingContract {
            storage: true,
            stages: ShaderStages::FRAGMENT,
        },
        BindingContract {
            storage: true,
            stages: ShaderStages::VERTEX,
        },
        BindingContract {
            storage: false,
            stages: ShaderStages::FRAGMENT,
        },
        BindingContract {
            storage: false,
            stages: ShaderStages::FRAGMENT,
        },
        BindingContract {
            storage: false,
            stages: ShaderStages::FRAGMENT,
        },
    ]
}

fn far_layout_entries() -> Vec<BindGroupLayoutEntry> {
    let entries = BindGroupLayoutEntries::sequential(
        ShaderStages::VERTEX_FRAGMENT,
        (
            bt::storage_buffer_read_only_sized(false, None),
            bt::storage_buffer_read_only_sized(false, None),
            bt::storage_buffer_read_only_sized(false, None),
            bt::storage_buffer_read_only_sized(false, None),
            bt::storage_buffer_read_only_sized(false, None),
            bt::texture_2d(TextureSampleType::Uint),
            bt::texture_2d_array(TextureSampleType::Float { filterable: true }),
            bt::sampler(SamplerBindingType::Filtering),
        ),
    );
    let mut entries = entries.to_vec();
    for (index, contract) in far_render_binding_contract().into_iter().enumerate() {
        entries[index].visibility = contract.stages;
    }
    entries
}

fn cull_layout_entries() -> Vec<BindGroupLayoutEntry> {
    BindGroupLayoutEntries::sequential(
        ShaderStages::COMPUTE,
        (
            bt::uniform_buffer_sized(false, None),
            bt::storage_buffer_read_only_sized(false, None),
            bt::storage_buffer_sized(false, None),
            bt::storage_buffer_sized(false, None),
        ),
    )
    .to_vec()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FarVoxelPipelineKey {
    pub msaa_samples: u32,
    /// The view's colour target format (Bevy 0.19 dropped the `hdr` flag).
    pub target_format: TextureFormat,
    pub view_layout_key: MeshPipelineViewLayoutKey,
}

pub fn cull_shader_defs() -> Vec<ShaderDefVal> {
    vec!["FAR_CULL".into()]
}

pub fn render_shader_defs(key: FarVoxelPipelineKey) -> Vec<ShaderDefVal> {
    let mut defs = vec!["FAR_RENDER".into()];
    if key
        .view_layout_key
        .contains(MeshPipelineViewLayoutKey::DEPTH_PREPASS)
    {
        defs.push("DEPTH_PREPASS".into());
    }
    if key.msaa_samples > 1 {
        defs.push("MULTISAMPLED".into());
    }
    // Bevy 0.19 binds the fog uniform only for views with `DistanceFog`.
    if key
        .view_layout_key
        .contains(MeshPipelineViewLayoutKey::DISTANCE_FOG)
    {
        defs.push("DISTANCE_FOG".into());
    }
    defs
}

pub fn far_proxy_primitive_state() -> PrimitiveState {
    PrimitiveState {
        // Retirement can keep a stale LOD root alive while a moving camera
        // enters its cell. Two-sided proxies preserve the shader's supported
        // inside-AABB ray path until replacement coverage is ready.
        cull_mode: None,
        ..default()
    }
}

#[derive(Resource)]
pub struct FarVoxelPipeline {
    pub far_layout: BindGroupLayout,
    pub far_layout_desc: BindGroupLayoutDescriptor,
    pub cull_layout: BindGroupLayout,
    pub cull_pipeline: CachedComputePipelineId,
    pub shader: Handle<Shader>,
    pub view_layouts: MeshPipelineViewLayouts,
}

impl FromWorld for FarVoxelPipeline {
    fn from_world(world: &mut World) -> Self {
        let device = world.resource::<RenderDevice>();
        let far_entries = far_layout_entries();
        // Tighten per-binding visibility so the fragment stage sees four far
        // storage buffers and the vertex stage sees two. Together with Bevy's
        // MeshView layout this fits the native eight-buffer capability gate.
        let far_layout = device.create_bind_group_layout("far_voxel_render", &far_entries);
        let far_layout_desc = BindGroupLayoutDescriptor::new("far_voxel_render", &far_entries);

        let cull_entries = cull_layout_entries();
        let cull_layout = device.create_bind_group_layout("far_voxel_cull", &cull_entries);
        let cull_layout_desc = BindGroupLayoutDescriptor::new("far_voxel_cull", &cull_entries);
        let shader = world.load_asset("shaders/far_voxel.wgsl");
        let cull_pipeline =
            world
                .resource::<PipelineCache>()
                .queue_compute_pipeline(ComputePipelineDescriptor {
                    label: Some("far_voxel_cull".into()),
                    layout: vec![cull_layout_desc],
                    shader: shader.clone(),
                    shader_defs: cull_shader_defs(),
                    entry_point: Some("cull".into()),
                    ..default()
                });
        Self {
            far_layout,
            far_layout_desc,
            cull_layout,
            cull_pipeline,
            shader,
            view_layouts: world.resource::<MeshPipelineViewLayouts>().clone(),
        }
    }
}

impl SpecializedRenderPipeline for FarVoxelPipeline {
    type Key = FarVoxelPipelineKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        let view_layout = self
            .view_layouts
            .get_view_layout(key.view_layout_key)
            .main_layout
            .clone();
        let shader_defs = render_shader_defs(key);
        RenderPipelineDescriptor {
            label: Some("far_voxel_proxy".into()),
            layout: vec![view_layout, self.far_layout_desc.clone()],
            vertex: VertexState {
                shader: self.shader.clone(),
                entry_point: Some("vertex".into()),
                shader_defs: shader_defs.clone(),
                buffers: vec![],
            },
            fragment: Some(FragmentState {
                shader: self.shader.clone(),
                entry_point: Some("fragment".into()),
                shader_defs,
                targets: vec![Some(ColorTargetState {
                    format: key.target_format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: far_proxy_primitive_state(),
            depth_stencil: Some(DepthStencilState {
                format: CORE_3D_DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(CompareFunction::GreaterEqual),
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

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;

    use bevy::{
        asset::{uuid::Uuid, AssetId},
        render::render_resource::{DownlevelFlags, ShaderStages, WgpuFeatures},
        shader::{ShaderCacheError, ShaderCache, ShaderCacheSource, ValidateShader},
    };
    use game::BlockId;

    use super::*;
    use crate::renderer::block_textures::BlockTextureLayers;

    fn shader_source() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/shaders/far_voxel.wgsl"
        ))
        .expect("Task 5 must provide the far voxel shader")
    }

    /// Compose `source` against stub `imports` and run naga validation on the
    /// result. Shared with `voxel_pull::pipeline`'s shader test.
    pub(crate) fn validate_shader(
        source: String,
        shader_defs: &[ShaderDefVal],
        imports: &[(&str, &str)],
    ) -> Result<(), ShaderCacheError> {
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
        cache.set_shader(shader_id, Shader::from_wgsl(source, "far_voxel_test.wgsl"));
        cache.get(0, shader_id, shader_defs).map(drop)
    }

    fn render_validation_imports() -> [(&'static str, &'static str); 5] {
        [
            (
                "mesh_view_bindings.wgsl",
                r#"
#define_import_path bevy_pbr::mesh_view_bindings

#import bevy_pbr::mesh_view_types::Fog

struct TestView {
    clip_from_view: mat4x4<f32>,
    viewport: vec4<f32>,
    world_position: vec3<f32>,
    exposure: f32,
}

struct TestDirectionalLight {
    color: vec4<f32>,
    direction_to_light: vec3<f32>,
    padding: f32,
}

struct TestLights {
    ambient_color: vec4<f32>,
    n_directional_lights: u32,
    padding: vec3<u32>,
    directional_lights: array<TestDirectionalLight, 16>,
}

@group(0) @binding(0) var<uniform> view: TestView;
@group(0) @binding(1) var<uniform> lights: TestLights;
@group(0) @binding(12) var<uniform> fog: Fog;
#ifdef DEPTH_PREPASS
#ifdef MULTISAMPLED
@group(0) @binding(2) var depth_prepass_texture: texture_depth_multisampled_2d;
#else
@group(0) @binding(2) var depth_prepass_texture: texture_depth_2d;
#endif
#endif
"#,
            ),
            (
                "mesh_view_types.wgsl",
                r#"
#define_import_path bevy_pbr::mesh_view_types

struct Fog {
    base_color: vec4<f32>,
    be: vec3<f32>,
    mode: u32,
}

const FOG_MODE_OFF: u32 = 0u;
const FOG_MODE_LINEAR: u32 = 1u;
const FOG_MODE_EXPONENTIAL: u32 = 2u;
const FOG_MODE_EXPONENTIAL_SQUARED: u32 = 3u;
const FOG_MODE_ATMOSPHERIC: u32 = 4u;
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
                "view_transformations.wgsl",
                r#"
#define_import_path bevy_pbr::view_transformations

fn frag_coord_to_ndc(position: vec4<f32>) -> vec3<f32> {
    return position.xyz;
}

fn position_ndc_to_world(position: vec3<f32>) -> vec3<f32> {
    return position;
}

fn position_world_to_clip(position: vec3<f32>) -> vec4<f32> {
    return vec4(position, 1.0);
}
"#,
            ),
            (
                "maths.wgsl",
                "#define_import_path bevy_render::maths\nconst PI: f32 = 3.141592653589793;\n",
            ),
        ]
    }

    #[test]
    fn native_capability_gate_requires_compute_indirect_and_eight_storage_bindings() {
        let supported = FarPassDeviceCapabilities {
            compute_shaders: true,
            indirect_execution: true,
            max_storage_buffers_per_shader_stage: 8,
            max_compute_workgroup_size_x: 64,
            max_compute_invocations_per_workgroup: 64,
            max_compute_workgroups_per_dimension: 1,
            max_texture_dimension_2d: 256,
        };
        assert!(supported.supports_far_pass());

        for unsupported in [
            FarPassDeviceCapabilities {
                compute_shaders: false,
                ..supported
            },
            FarPassDeviceCapabilities {
                indirect_execution: false,
                ..supported
            },
            FarPassDeviceCapabilities {
                max_storage_buffers_per_shader_stage: 7,
                ..supported
            },
            FarPassDeviceCapabilities {
                max_compute_workgroup_size_x: 63,
                ..supported
            },
            FarPassDeviceCapabilities {
                max_compute_invocations_per_workgroup: 63,
                ..supported
            },
            FarPassDeviceCapabilities {
                max_compute_workgroups_per_dimension: 0,
                ..supported
            },
            FarPassDeviceCapabilities {
                max_texture_dimension_2d: 255,
                ..supported
            },
        ] {
            assert!(!unsupported.supports_far_pass());
        }
    }

    #[test]
    fn proxy_draw_contract_is_one_indirect_cube_draw() {
        assert_eq!(PROXY_VERTEX_COUNT, 36);
        assert_eq!(empty_proxy_draw_args(), [36, 0, 0, 0]);
    }

    #[test]
    fn material_lut_is_fixed_size_and_preserves_top_side_bottom_layers() {
        let layers = BlockTextureLayers(BTreeMap::from([
            (BlockId(1), [11, 12, 13]),
            (BlockId(u16::MAX), [21, 22, 23]),
        ]));

        let lut = encode_material_lut(&layers);

        assert_eq!(lut.len(), MATERIAL_LUT_TEXELS);
        assert_eq!(lut[0], [0; 4]);
        assert_eq!(lut[1], [11, 12, 13, 0]);
        assert_eq!(lut[u16::MAX as usize], [21, 22, 23, 0]);
        assert_eq!(material_lut_bytes(), 256 * 256 * 8);
    }

    #[test]
    fn render_layout_keeps_fragment_storage_count_within_native_gate() {
        let bindings = far_render_binding_contract();
        assert_eq!(bindings.len(), 8);
        assert_eq!(bindings.iter().filter(|binding| binding.storage).count(), 5);
        assert_eq!(
            bindings
                .iter()
                .filter(|binding| {
                    binding.storage && binding.stages.contains(ShaderStages::FRAGMENT)
                })
                .count(),
            4,
        );
        assert_eq!(
            bindings
                .iter()
                .filter(|binding| {
                    binding.storage && binding.stages.contains(ShaderStages::VERTEX)
                })
                .count(),
            2,
        );
    }

    #[test]
    fn layout_descriptors_use_static_offsets_and_correct_storage_access() {
        fn assert_storage(entry: &BindGroupLayoutEntry, expected_read_only: bool) {
            let BindingType::Buffer {
                ty: BufferBindingType::Storage { read_only },
                has_dynamic_offset,
                ..
            } = entry.ty
            else {
                panic!("binding {} must be a storage buffer", entry.binding);
            };
            assert_eq!(read_only, expected_read_only, "binding {}", entry.binding);
            assert!(!has_dynamic_offset, "binding {}", entry.binding);
        }

        let far_entries = far_layout_entries();
        assert_eq!(far_entries.len(), 8);
        for entry in &far_entries[..5] {
            assert_storage(entry, true);
        }

        let cull_entries = cull_layout_entries();
        assert_eq!(cull_entries.len(), 4);
        assert!(matches!(
            cull_entries[0].ty,
            BindingType::Buffer {
                ty: BufferBindingType::Uniform,
                has_dynamic_offset: false,
                ..
            }
        ));
        assert_storage(&cull_entries[1], true);
        assert_storage(&cull_entries[2], false);
        assert_storage(&cull_entries[3], false);
    }

    #[test]
    fn shader_definitions_match_cull_and_view_specializations() {
        use bevy::pbr::MeshPipelineViewLayoutKey;
        use bevy::shader::ShaderDefVal;

        assert_eq!(cull_shader_defs(), vec![ShaderDefVal::from("FAR_CULL")]);

        let depth_key = FarVoxelPipelineKey {
            msaa_samples: 1,
            target_format: TextureFormat::Rgba16Float,
            view_layout_key: MeshPipelineViewLayoutKey::DEPTH_PREPASS,
        };
        assert_eq!(
            render_shader_defs(depth_key),
            vec![
                ShaderDefVal::from("FAR_RENDER"),
                ShaderDefVal::from("DEPTH_PREPASS"),
            ]
        );

        let multisampled = FarVoxelPipelineKey {
            msaa_samples: 4,
            target_format: TextureFormat::Bgra8UnormSrgb,
            view_layout_key: MeshPipelineViewLayoutKey::DEPTH_PREPASS
                | MeshPipelineViewLayoutKey::NORMAL_PREPASS,
        };
        assert_eq!(
            render_shader_defs(multisampled),
            vec![
                ShaderDefVal::from("FAR_RENDER"),
                ShaderDefVal::from("DEPTH_PREPASS"),
                ShaderDefVal::from("MULTISAMPLED"),
            ]
        );
    }

    #[test]
    fn shader_compacts_frustum_visible_cells_into_one_indirect_draw() {
        let shader = shader_source();
        for required in [
            "@compute @workgroup_size(64)",
            "atomicAdd(&draw_args.instance_count",
            "visible_cell_slots[visible_index] = cell_slot",
            "cell.is_active != 1u",
            "for (var plane_index = 0u; plane_index < 5u",
        ] {
            assert!(
                shader.contains(required),
                "missing shader contract: {required}"
            );
        }
    }

    #[test]
    fn cull_shader_is_valid_wgsl_after_shader_def_preprocessing() {
        validate_shader(
            shader_source(),
            &cull_shader_defs(),
            &render_validation_imports(),
        )
        .expect("far voxel cull shader must compose and validate");
    }

    #[test]
    fn render_shader_variants_are_valid_wgsl_after_imports_and_preprocessing() {
        let imports = render_validation_imports();
        for defs in [
            vec![ShaderDefVal::from("FAR_RENDER")],
            vec![
                ShaderDefVal::from("FAR_RENDER"),
                ShaderDefVal::from("DEPTH_PREPASS"),
            ],
            vec![
                ShaderDefVal::from("FAR_RENDER"),
                ShaderDefVal::from("DEPTH_PREPASS"),
                ShaderDefVal::from("MULTISAMPLED"),
            ],
        ] {
            validate_shader(shader_source(), &defs, &imports)
                .expect("far voxel render shader variant must compose and validate");
        }
    }

    #[test]
    fn shader_raymarches_bricks_then_voxels_and_writes_reverse_z_depth() {
        let shader = shader_source();
        for required in [
            "fn intersect_cell_aabb",
            "fn trace_brick",
            "iteration < 24u",
            "iteration < 12u",
            "fn trace_cell",
            "RootEntryKind",
            "depth_prepass_texture",
            "@builtin(frag_depth)",
            "clip_position.z / clip_position.w",
        ] {
            assert!(
                shader.contains(required),
                "missing shader contract: {required}"
            );
        }
        assert!(!shader.contains("iteration < 768u"));
        assert!(!shader.contains("iteration < 192u"));
    }

    #[test]
    fn proxy_boxes_remain_visible_when_the_camera_enters_a_retiring_cell() {
        assert_eq!(far_proxy_primitive_state().cull_mode, None);
    }

    #[test]
    fn shader_uses_neighbor_ao_textures_and_unshadowed_directional_lambert() {
        let shader = shader_source();
        for required in [
            "neighbor_slots",
            "fn sample_same_factor",
            "fn vertex_ao",
            "material_layer_lut",
            "textureSample(block_texture",
            "lights.ambient_color",
            "light.direction_to_light",
            "view.exposure",
        ] {
            assert!(
                shader.contains(required),
                "missing shader contract: {required}"
            );
        }
        assert!(!shader.contains("fetch_directional_shadow"));
        assert!(!shader.contains("directional_shadow_textures"));
    }
}
