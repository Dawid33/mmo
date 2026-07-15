// Pulled voxel forward shader. Record layout mirrored from
// crates/client/src/renderer/meshing/records.rs (the source of truth):
//   w0: x:6|y:6|z:6|face:3|ao0:2|ao1:2|ao2:2|ao3:2|flip:1   w1: w:6|h:6|layer:16
#import bevy_pbr::mesh_view_bindings::{view, lights}
#import bevy_pbr::mesh_view_types
#import bevy_pbr::shadows::fetch_directional_shadow
#import bevy_pbr::view_transformations::position_world_to_clip
#import bevy_render::maths::PI

@group(1) @binding(0) var<storage, read> quads: array<vec2<u32>>;
@group(1) @binding(1) var<storage, read> chunk_transforms: array<mat4x4<f32>>;
@group(1) @binding(2) var mat_array_texture: texture_2d_array<f32>;
@group(1) @binding(3) var mat_array_texture_sampler: sampler;

// Locked to Face::index() order (PosX, NegX, PosY, NegY, PosZ, NegZ) — same
// tables as voxel_texture.wgsl (copied: WGSL has no cross-file consts here).
const FACE_NORMALS = array<vec3<f32>, 6>(
    vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(-1.0, 0.0, 0.0),
    vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, -1.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, -1.0),
);
const FACE_U_MASK = array<vec3<f32>, 6>(
    vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(1.0, 0.0, 0.0),
    vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(1.0, 0.0, 0.0),
);
const FACE_V_MASK = array<vec3<f32>, 6>(
    vec3<f32>(0.0, -1.0, 0.0), vec3<f32>(0.0, -1.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(0.0, -1.0, 0.0), vec3<f32>(0.0, -1.0, 0.0),
);
const BLOCK_VOXELS: f32 = 16.0;

// CORNER_SLOTS from records.rs, flattened: variant*6 + slot.
const CORNER_SLOTS = array<u32, 24>(
    0u, 1u, 3u, 0u, 3u, 2u,
    0u, 1u, 2u, 1u, 3u, 2u,
    0u, 3u, 1u, 0u, 2u, 3u,
    0u, 2u, 1u, 1u, 2u, 3u,
);

// Per-face (U,V) axis ids from binary::cross_axes: X faces→(1,2), Y→(0,2), Z→(0,1).
const FACE_UA = array<u32, 6>(1u, 1u, 0u, 0u, 0u, 0u);
const FACE_VA = array<u32, 6>(2u, 2u, 2u, 2u, 1u, 1u);

struct PullVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) shade: f32,
    @location(4) @interpolate(flat) layer: u32,
}

@vertex
fn vertex(
    @builtin(vertex_index) vi: u32,
    @builtin(instance_index) chunk_index: u32,
) -> PullVertexOutput {
    let quad = vi / 6u;
    let slot = vi % 6u;
    let rec = quads[quad];
    let w0 = rec.x;
    let w1 = rec.y;
    let face = (w0 >> 18u) & 7u;
    let flip = (w0 >> 29u) & 1u;
    // Reversed winding faces: NegX(1), PosY(2), NegZ(5).
    var reversed = 0u;
    if (face == 1u || face == 2u || face == 5u) { reversed = 1u; }
    let corner = CORNER_SLOTS[(reversed * 2u + flip) * 6u + slot];
    let cu = corner & 1u;
    let cv = corner >> 1u;

    var pos = vec3<u32>(w0 & 63u, (w0 >> 6u) & 63u, (w0 >> 12u) & 63u);
    pos[FACE_UA[face]] += cu * (w1 & 63u);
    pos[FACE_VA[face]] += cv * ((w1 >> 6u) & 63u);
    let local_pos = vec3<f32>(pos);

    var out: PullVertexOutput;
    out.world_position = chunk_transforms[chunk_index] * vec4<f32>(local_pos, 1.0);
    out.position = position_world_to_clip(out.world_position.xyz);
    // Chunk transforms are translation+uniform-scale only; the normal needs no
    // inverse-transpose.
    out.world_normal = FACE_NORMALS[face];
    out.uv = vec2<f32>(
        dot(local_pos, FACE_U_MASK[face]),
        dot(local_pos, FACE_V_MASK[face]),
    ) / BLOCK_VOXELS;
    let ao = (w0 >> (21u + 2u * corner)) & 3u;
    // Must match ao::AO_CURVE / pack::shade (test-pinned there).
    out.shade = 0.55 + 0.15 * f32(ao);
    out.layer = (w1 >> 12u) & 65535u;
    return out;
}

@fragment
fn fragment(in: PullVertexOutput) -> @location(0) vec4<f32> {
    let tex = textureSample(mat_array_texture, mat_array_texture_sampler, in.uv, in.layer);
    let base = tex.rgb * in.shade;
    let n = normalize(in.world_normal);

    // Cascade selection z, view-space depth of the fragment. Copied verbatim
    // from the real caller (bevy_pbr-0.18.1/src/render/pbr_functions.wgsl,
    // the directional-light block feeding fetch_directional_shadow).
    let view_z = dot(vec4<f32>(
        view.view_from_world[0].z,
        view.view_from_world[1].z,
        view.view_from_world[2].z,
        view.view_from_world[3].z,
    ), in.world_position);

    var lit = lights.ambient_color.rgb;
    for (var i = 0u; i < lights.n_directional_lights; i += 1u) {
        let light = lights.directional_lights[i];
        let ndotl = max(dot(n, light.direction_to_light), 0.0);
        var shadow = 1.0;
        if (ndotl > 0.0 && (light.flags & mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u) {
            shadow = fetch_directional_shadow(i, in.world_position, n, view_z);
        }
        // 1/PI: the Lambertian diffuse normalization real PBR bakes into
        // Fd_Burley (pbr_lighting.wgsl) — dropping it overexposes by ~PI.
        lit += light.color.rgb * ndotl * shadow / PI;
    }
    // light.color and ambient_color carry raw illuminance (thousands, for a
    // default sun); real PBR scales the same accumulated sum by view.exposure
    // before adding emissive (pbr_functions.wgsl's apply_pbr_lighting, the
    // `view.exposure * (transmitted_light + direct_light + indirect_light)`
    // line) — without it every pixel saturates to white.
    lit *= view.exposure;
    return vec4<f32>(base * lit, 1.0);
}
