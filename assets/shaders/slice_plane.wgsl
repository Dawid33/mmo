// Slice plane: the cut's cap for implied (unmeshed) ground — see
// crates/client/src/renderer/slice_plane.rs (the source of truth for the
// bindings and SliceParams). A REGION_SIZE square on the cut plane over the
// home region, one cell per tile: the colour the tile mirror reports at the
// working level, lit like an up-facing terrain face and darkened like the
// cutaway cap. Only cells where the cut slices through material (the working
// level and the level above both solid, or water) carry a colour; every other
// cell has alpha 0 and is discarded.
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

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: SliceParams;
// The cap's colour per tile, x + z * REGION_BLOCKS (slice_plane.rs
// stud_slice_colors), alpha 0 where there is no cap.
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var colors: texture_2d<f32>;

// A tile's width in world units (1 m).
const TILE_UNITS: f32 = 16.0;

// Bevy's distance fog without in-scattering.
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
    let size = vec2<i32>(textureDimensions(colors));
    let cell = clamp(
        vec2<i32>(floor((p.xz - params.origin.xz) / TILE_UNITS)),
        vec2<i32>(0),
        size - vec2<i32>(1),
    );
    let cap = textureLoad(colors, cell, 0);

    // Lit as an up-facing terrain face (no AO).
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
    let color = apply_distance_fog(cap.rgb * lit * params.origin.w, p.xyz);
    // No cap: the level is open here; look down into it.
    if (cap.a == 0.0) {
        discard;
    }
    return vec4<f32>(color, 1.0);
}
