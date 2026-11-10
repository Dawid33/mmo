// Stud terrain (crates/client/src/renderer/stud/material.rs): Bevy's PBR path
// over a StandardMaterial tinted by vertex colours, plus the fortress view —
// the z-level cut, its cap and the darkening of levels below it. One file for
// the forward pass and the depth / shadow prepass (PREPASS_PIPELINE), so
// cut-away bricks cast no shadow. Compiled into the binary
// (load_internal_asset!): edit and rebuild, no hot reload.
//
// Unlike the voxel shaders nothing is done per vertex: a stud mesh is baked
// part triangles (studs, slopes), so the cut is a per-fragment discard at the
// plane. While the cut is on, the opaque material stops culling
// (StudFortressKey) and a back face seen inside the home region is drawn as
// the cut's cap — shaded where the eye ray crosses the plane, facing up, in
// its brick's colour, darkened by fortress.cut.y — exactly the rule of
// voxel_texture.wgsl.

#ifdef PREPASS_PIPELINE
#import bevy_pbr::prepass_io
#ifdef MOTION_VECTOR_PREPASS
#import bevy_pbr::pbr_prepass_functions
#endif
#else
#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::view,
}
#endif

// The z-level cutaway, mirrored from FortressViewUniform in
// crates/client/src/renderer/fortress_view.rs (the source of truth for every
// field), kept in step by stud/material.rs sync_stud_materials.
struct FortressView {
    cut: vec4<f32>,
    origin: vec4<f32>,
    overlay_dims: vec4<u32>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> fortress: FortressView;

// Mirrored from StudFortressParams in stud/material.rs. The numbers come from
// the Rust consts the voxel shaders mirror (fortress_view.rs) and from
// TerrainKind, so nothing is restated here.
struct StudFortressParams {
    // x: a level's height (one tile), world units; y: brightness lost per
    // level below the cut; z: the darkest a level gets; w: unused.
    levels: vec4<f32>,
    // x: 1 = back faces are the cut's cap (the opaque material), 0 = none
    // (glass); y: vertex alpha under which a fragment is a seam line, which
    // never caps; zw: unused.
    flags: vec4<f32>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var<uniform> params: StudFortressParams;

// The home region's boundary line and the dimming outside it, mirrored from
// BoundaryUniform in crates/client/src/renderer/boundaries.rs (the source of
// truth, incl. the meaning of every field). A home opacity of 0 turns it off.
struct TerrainBoundaries {
    home_rect: vec4<f32>,
    home_color: vec4<f32>,
    shape: vec4<f32>,
    dim: vec4<f32>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> boundaries: TerrainBoundaries;

// Mirrored from voxel_texture.wgsl / fortress_view.rs.
const CUT_OFF: f32 = 3.0e38;
const CUT_EPSILON: f32 = 0.01;

fn cut_on() -> bool {
    return fortress.cut.x < CUT_OFF;
}

// Whether the cut applies at world column `p` (x, z): inside the home region
// while the cut is on (water.wgsl's cut_applies).
fn cut_applies(p: vec2<f32>) -> bool {
    let lo = fortress.origin.xz;
    let hi = lo + vec2<f32>(fortress.cut.w);
    return cut_on() && all(p >= lo) && all(p < hi);
}

// Whether the fragment at world `p` is cut away.
fn cut_away(p: vec3<f32>) -> bool {
    return cut_applies(p.xz) && p.y > fortress.cut.x + CUT_EPSILON;
}

#ifdef PREPASS_PIPELINE

#ifdef PREPASS_FRAGMENT

@fragment
fn fragment(in: prepass_io::VertexOutput) -> prepass_io::FragmentOutput {
    var out: prepass_io::FragmentOutput;
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.frag_depth = in.unclipped_depth;
#endif
#ifdef NORMAL_PREPASS
    out.normal = vec4<f32>(normalize(in.world_normal) * 0.5 + vec3<f32>(0.5), 1.0);
#endif
#ifdef MOTION_VECTOR_PREPASS
    out.motion_vector = pbr_prepass_functions::calculate_motion_vector(
        in.world_position, in.previous_world_position);
#endif
    if (cut_away(in.world_position.xyz)) {
        discard;
    }
    return out;
}
#else // PREPASS_FRAGMENT
@fragment
fn fragment(in: prepass_io::VertexOutput) {
    if (cut_away(in.world_position.xyz)) {
        discard;
    }
}
#endif // PREPASS_FRAGMENT

#else // PREPASS_PIPELINE

fn view_is_orthographic() -> bool {
    return view.clip_from_view[3][3] == 1.0;
}

fn view_forward() -> vec3<f32> {
    return -vec3<f32>(view.view_from_world[0].z, view.view_from_world[1].z, view.view_from_world[2].z);
}

// Copy of voxel_texture.wgsl's cut_crossing: where the eye ray to `p` crosses
// the cut plane, when the eye is above it and `p` below; else `p`.
fn cut_crossing(p: vec3<f32>, cut: f32) -> vec3<f32> {
    if (view_is_orthographic()) {
        let fwd = view_forward();
        if (p.y < cut && fwd.y < 0.0) {
            return p + fwd * ((cut - p.y) / fwd.y);
        }
        return p;
    }
    let eye = view.world_position;
    let dy = p.y - eye.y;
    if (eye.y > cut && dy < 0.0) {
        return eye + (p - eye) * ((cut - eye.y) / dy);
    }
    return p;
}

// Copy of voxel_pull.wgsl's apply_boundaries: the home region's perimeter drawn
// onto `color` at ground point `p` (world xz) as a line `shape.x` world units
// wide (at least `shape.y` pixels) starting on the edge and growing outward,
// and what lies outside it dimmed. Uniform control flow only (fwidth).
fn apply_boundaries(color: vec3<f32>, p: vec2<f32>) -> vec3<f32> {
    let b = boundaries;
    let q = max(b.home_rect.xy - p, p - b.home_rect.zw);
    let sd = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0);
    let unit_px = 1.0 / max(fwidth(sd), 1e-4);
    let sd_px = sd * unit_px;
    let t = smoothstep(0.0, b.shape.z, sd);
    let luma = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    let dimmed = mix(color, vec3<f32>(luma), b.dim.y * t) * (1.0 - b.dim.x * t);
    let c = select(color, dimmed, sd > 0.0);
    let w = max(b.shape.x * unit_px, b.shape.y);
    let line = smoothstep(0.0, 1.0, sd_px) * (1.0 - smoothstep(w, w + 1.0, sd_px));
    let on = b.home_color.a > 0.0;
    return select(color, mix(c, b.home_color.rgb, line * b.home_color.a), on);
}

// voxel_texture.wgsl's depth_darkening with the level height and the two
// constants from `params` (stud/material.rs depth_darkening is its Rust twin).
fn depth_darkening(y: f32, cut: f32) -> f32 {
    let levels_below = max(0.0, ceil((cut - y) / params.levels.x - 1.0 - 1e-3));
    return max(params.levels.z, 1.0 - params.levels.y * levels_below);
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    let cut = fortress.cut.x;
    let inside = cut_applies(in.world_position.xz);
    let caps = params.flags.x != 0.0;
    // Back faces are drawn only by the opaque material while the cut is on.
    let back = caps && cut_on() && !is_front;
    let cap = inside && back;

    var shaded = in;
    if (cap) {
        shaded.world_position = vec4<f32>(cut_crossing(in.world_position.xyz, cut), 1.0);
        shaded.world_normal = vec3<f32>(0.0, 1.0, 0.0);
    }
    // A cap is lit as the up-facing top it shows, never flipped as a back face.
    var pbr_input = pbr_input_from_standard_material(shaded, is_front || cap);

    // In the home region the cap is darkened by cut.y, faces below the cut by
    // depth; outside it nothing changes.
    let darkening = select(depth_darkening(in.world_position.y, cut), 1.0 - fortress.cut.y, cap);
    let shade = select(1.0, darkening, inside);
    let base = pbr_input.material.base_color;
    pbr_input.material.base_color = alpha_discard(pbr_input.material, vec4<f32>(base.rgb * shade, base.a));

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    // The boundary line hugs the terrain, under the fog (as voxel_pull.wgsl).
    out.color = vec4<f32>(apply_boundaries(out.color.rgb, in.world_position.xz), out.color.a);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);

    // Last, after every derivative use. Above the cut: cut away.
    if (cut_away(in.world_position.xyz)) {
        discard;
    }
    // A back face is a cap only inside the home region, and never a seam
    // line (a thin quad lying on its brick's face).
#ifdef VERTEX_COLORS
    let seam = in.color.a < params.flags.y;
#else
    let seam = false;
#endif
    if (back && (!inside || seam)) {
        discard;
    }
    return out;
}

#endif // PREPASS_PIPELINE
