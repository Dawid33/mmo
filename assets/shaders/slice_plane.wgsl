// Slice plane: the cut's cap for implied (unmeshed) ground — see
// crates/client/src/renderer/slice_plane.rs (the source of truth for the
// bindings and SliceParams). A REGION_SIZE square on the cut plane over the
// home region, one cell per block: the block the tile mirror reports at the
// working level, drawn with its top texture lit like an up-facing terrain
// face and darkened like the cutaway cap; air cells are discarded.
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::{view, lights}
#import bevy_pbr::mesh_view_types
#import bevy_pbr::fog::{linear_fog, exponential_fog, exponential_squared_fog, atmospheric_fog}
#import bevy_pbr::shadows::fetch_directional_shadow
#import bevy_render::maths::PI

struct SliceParams {
    // xz: home region world origin; y: unused; w: brightness (1 - CAP_DARKEN).
    origin: vec4<f32>,
}

// Block ids of the working level, x + z * REGION_BLOCKS (AIR = 0).
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var ids: texture_2d<u32>;
// Block id -> top-face layer of the block array texture.
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var top_layers: texture_2d<u32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var blocks: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var blocks_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var<uniform> params: SliceParams;

const BLOCK_VOXELS: f32 = 16.0;

// Copied from voxel_pull.wgsl: nearest-looking magnification from the
// linear (anisotropic) block sampler.
fn crisp_uv(uv: vec2<f32>, texels: vec2<f32>) -> vec2<f32> {
    let px = uv * texels;
    let seam = floor(px + 0.5);
    let footprint = max(fwidth(px), vec2<f32>(1e-5));
    return (seam + clamp((px - seam) / footprint, vec2<f32>(-0.5), vec2<f32>(0.5))) / texels;
}

// Copied from voxel_pull.wgsl: Bevy's distance fog without in-scattering.
fn apply_distance_fog(color: vec3<f32>, world_position: vec3<f32>) -> vec3<f32> {
#ifdef DISTANCE_FOG
    let fog = bevy_pbr::mesh_view_bindings::fog;
    let distance = length(world_position - view.world_position);
    let c = vec4<f32>(color, 1.0);
    let none = vec3<f32>(0.0);
    if (fog.mode == mesh_view_types::FOG_MODE_LINEAR) {
        return linear_fog(fog, c, distance, none).rgb;
    } else if (fog.mode == mesh_view_types::FOG_MODE_EXPONENTIAL) {
        return exponential_fog(fog, c, distance, none).rgb;
    } else if (fog.mode == mesh_view_types::FOG_MODE_EXPONENTIAL_SQUARED) {
        return exponential_squared_fog(fog, c, distance, none).rgb;
    } else if (fog.mode == mesh_view_types::FOG_MODE_ATMOSPHERIC) {
        return atmospheric_fog(fog, c, distance, none).rgb;
    }
#endif
    return color;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let p = in.world_position;
    // Derivatives first, in uniform control flow.
    let uv = p.xz / BLOCK_VOXELS;
    let texels = vec2<f32>(textureDimensions(blocks).xy);
    let crisp = crisp_uv(uv, texels);
    let ddx = dpdx(uv);
    let ddy = dpdy(uv);

    let size = vec2<i32>(textureDimensions(ids));
    let cell = clamp(
        vec2<i32>(floor((p.xz - params.origin.xz) / BLOCK_VOXELS)),
        vec2<i32>(0),
        size - vec2<i32>(1),
    );
    let id = textureLoad(ids, cell, 0).r;
    var layer = 0u;
    if (id < textureDimensions(top_layers).x) {
        layer = textureLoad(top_layers, vec2<i32>(i32(id), 0), 0).r;
    }
    let tex = textureSampleGrad(blocks, blocks_sampler, crisp, layer, ddx, ddy);

    // Lit as an up-facing terrain face in voxel_pull.wgsl (no AO).
    let n = vec3<f32>(0.0, 1.0, 0.0);
    let view_z = dot(vec4<f32>(
        view.view_from_world[0].z,
        view.view_from_world[1].z,
        view.view_from_world[2].z,
        view.view_from_world[3].z,
    ), p);
    var lit = lights.ambient_color.rgb;
    for (var i = 0u; i < lights.n_directional_lights; i += 1u) {
        let light = lights.directional_lights[i];
        let ndotl = max(dot(n, light.direction_to_light), 0.0);
        var shadow = 1.0;
        if (ndotl > 0.0 && (light.flags & mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u) {
            shadow = fetch_directional_shadow(i, p, n, view_z, in.position.xy);
        }
        lit += light.color.rgb * ndotl * shadow / PI;
    }
    lit *= view.exposure;
    let color = apply_distance_fog(tex.rgb * lit * params.origin.w, p.xyz);
    // Air: the level is open here; look down into it. Last, after every
    // derivative (see voxel_pull.wgsl's discard).
    if (id == 0u) {
        discard;
    }
    return vec4<f32>(color, 1.0);
}
