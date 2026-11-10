//! Test support for the compiled-in WGSL shaders: compose a shader against
//! stub Bevy imports and run naga validation on the result, so a typo or a
//! type / uniformity error fails in a unit test instead of at pipeline creation.

use bevy::{
    asset::{uuid::Uuid, AssetId},
    render::render_resource::{DownlevelFlags, WgpuFeatures},
    shader::{Shader, ShaderCache, ShaderCacheError, ShaderCacheSource, ShaderDefVal, ValidateShader},
};

/// Compose `source` against stub `imports` and run naga validation on the result.
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
    cache.set_shader(shader_id, Shader::from_wgsl(source, "shader_test.wgsl"));
    cache.get(0, shader_id, shader_defs).map(drop)
}

/// Minimal stand-ins for the Bevy modules the terrain shaders import: only
/// the names and types they use.
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
