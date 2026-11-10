// Slice plane: the cut's cap for implied (unmeshed) ground — see
// crates/client/src/renderer/slice_plane.rs (the source of truth for the
// bindings and SliceParams). A REGION_SIZE square on the cut plane over the
// home region, one cell per block: the block the tile mirror reports at the
// working level, drawn with its top texture lit like an up-facing terrain
// face and darkened like the cutaway cap. Only cells where the cut slices
// through material (the working level and the level above both solid) carry
// an id; every other cell is 0 and discarded.
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::{view, lights}
#import bevy_pbr::mesh_view_types
#import bevy_pbr::fog::{linear_fog, exponential_fog, exponential_squared_fog, atmospheric_fog}
#import bevy_pbr::shadows::fetch_directional_shadow
#import bevy_render::maths::PI

struct SliceParams {
    // xz: home region world origin; y: unused; w: brightness (1 - CAP_DARKEN).
    origin: vec4<f32>,
    // x: the working level's layer in the overlay texture; y: overlay on (0/1);
    // z: 1 = stud terrain (cells coloured from `colors`, not `ids`).
    overlay: vec4<u32>,
}

// Block ids of the working level, x + z * REGION_BLOCKS (AIR = 0).
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var ids: texture_2d<u32>;
// Block id -> top-face layer of the block array texture.
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var top_layers: texture_2d<u32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var blocks: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var blocks_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var<uniform> params: SliceParams;
// The designation overlay voxel_pull.wgsl reads (fortress_view.rs
// OverlayGrid): texel (x, z, layer); the plane's cells are the working level's.
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var overlay: texture_3d<u32>;
// Stud terrain: the cap's colour per tile (slice_plane.rs stud_slice_colors),
// alpha 0 where there is no cap. 1 x 1 and unread on voxel terrain.
@group(#{MATERIAL_BIND_GROUP}) @binding(6) var colors: texture_2d<f32>;

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

// BEGIN fortress overlay patterns — identical in voxel_pull.wgsl and
// slice_plane.wgsl (test-pinned: fortress_view.rs). The overlay byte's bits
// are mirrored from fortress_view.rs (the source of truth).
const OVERLAY_KIND_MASK: u32 = 7u;
const OVERLAY_DIG: u32 = 1u;
const OVERLAY_DECONSTRUCT: u32 = 2u;
const OVERLAY_CHOP: u32 = 3u;
const OVERLAY_PREVIEW: u32 = 16u;
const OVERLAY_INVALID: u32 = 32u;
// Pattern repeats per block (one metre): a 0.25 m period.
const OVERLAY_PATTERN_PER_BLOCK: f32 = 4.0;
// Dig: amber diagonal stripes, half of each period wide.
const OVERLAY_DIG_COLOR = vec3<f32>(1.0, 0.62, 0.08);
const OVERLAY_DIG_WIDTH: f32 = 0.5;
// Deconstruct: a red cross-hatch of thinner lines.
const OVERLAY_DECONSTRUCT_COLOR = vec3<f32>(0.9, 0.1, 0.06);
const OVERLAY_HATCH_WIDTH: f32 = 0.22;
// Chop: green stripes on the other diagonal, thinner than dig's.
const OVERLAY_CHOP_COLOR = vec3<f32>(0.3, 0.85, 0.15);
const OVERLAY_CHOP_WIDTH: f32 = 0.35;
// How strongly a pattern line replaces the surface colour.
const OVERLAY_PATTERN_ALPHA: f32 = 0.75;
// Preview: 35 % toward white; invalid: 50 % toward red.
const OVERLAY_PREVIEW_WHITEN: f32 = 0.35;
const OVERLAY_INVALID_COLOR = vec3<f32>(1.0, 0.0, 0.0);
const OVERLAY_INVALID_MIX: f32 = 0.5;

// Coverage (0..1) of lines `width` of each unit period of `s` wide,
// antialiased over one pixel and fading to the mean coverage where a period
// shrinks under two pixels. Must run in uniform control flow (fwidth).
fn overlay_lines(s: f32, width: f32) -> f32 {
    let fw = max(fwidth(s), 1e-5);
    let d = abs(fract(s) - 0.5);
    let edge = 0.5 * width;
    let lines = 1.0 - smoothstep(edge - 0.5 * fw, edge + 0.5 * fw, d);
    return mix(lines, width, smoothstep(0.25, 0.5, fw));
}

// `color` tinted by overlay byte `bits` at surface coordinates `uv` (in
// blocks). Uniform control flow only (derivatives); a byte of 0 returns
// `color` unchanged.
fn overlay_tint(color: vec3<f32>, uv: vec2<f32>, bits: u32) -> vec3<f32> {
    let p = uv * OVERLAY_PATTERN_PER_BLOCK;
    let stripes = overlay_lines(p.x + p.y, OVERLAY_DIG_WIDTH);
    let hatch = max(overlay_lines(p.x + p.y, OVERLAY_HATCH_WIDTH), overlay_lines(p.x - p.y, OVERLAY_HATCH_WIDTH));
    let chop = overlay_lines(p.x - p.y, OVERLAY_CHOP_WIDTH);
    let kind = bits & OVERLAY_KIND_MASK;
    var c = color;
    c = mix(c, OVERLAY_DIG_COLOR, OVERLAY_PATTERN_ALPHA * stripes * f32(kind == OVERLAY_DIG));
    c = mix(c, OVERLAY_DECONSTRUCT_COLOR, OVERLAY_PATTERN_ALPHA * hatch * f32(kind == OVERLAY_DECONSTRUCT));
    c = mix(c, OVERLAY_CHOP_COLOR, OVERLAY_PATTERN_ALPHA * chop * f32(kind == OVERLAY_CHOP));
    c = mix(c, vec3<f32>(1.0), OVERLAY_PREVIEW_WHITEN * f32((bits & OVERLAY_PREVIEW) != 0u));
    c = mix(c, OVERLAY_INVALID_COLOR, OVERLAY_INVALID_MIX * f32((bits & OVERLAY_INVALID) != 0u));
    return c;
}
// END fortress overlay patterns

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
    // Designations on the working level, as voxel_pull.wgsl tints a cap
    // (same uv: world xz in blocks) — implied rock has no face to carry them.
    let marked = textureLoad(overlay, vec3<i32>(cell, i32(params.overlay.x)), 0).r;
    // Stud terrain has no block textures: a flat colour per tile instead.
    let stud = params.overlay.z != 0u;
    let stud_cell = clamp(cell, vec2<i32>(0), vec2<i32>(textureDimensions(colors)) - vec2<i32>(1));
    let stud_color = textureLoad(colors, stud_cell, 0);
    let surface = select(tex.rgb, stud_color.rgb, stud);
    let albedo = overlay_tint(surface, uv, select(0u, marked, params.overlay.y != 0u));

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
    let color = apply_distance_fog(albedo * lit * params.origin.w, p.xyz);
    // Air: the level is open here; look down into it. Last, after every
    // derivative (see voxel_pull.wgsl's discard).
    if ((!stud && id == 0u) || (stud && stud_color.a == 0.0)) {
        discard;
    }
    return vec4<f32>(color, 1.0);
}
