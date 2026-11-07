// Static water (crates/client/src/renderer/water/material.rs): Bevy's PBR
// path over a blended StandardMaterial whose vertex colour carries the tint
// and the depth alpha, plus the z-level cut. Compiled into the binary
// (load_internal_asset!): edit and rebuild, no hot reload.
#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}

// The z-level cutaway, mirrored from FortressViewUniform in
// crates/client/src/renderer/fortress_view.rs (the source of truth for every
// field), kept in step by water/material.rs sync_water_material_cut.
struct FortressView {
    cut: vec4<f32>,
    origin: vec4<f32>,
    overlay_dims: vec4<u32>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> fortress: FortressView;

// Mirrored from voxel_texture.wgsl / fortress_view.rs.
const CUT_OFF: f32 = 3.0e38;
const CUT_EPSILON: f32 = 0.01;

// Whether the cut applies at world column `p` (x, z): inside the home
// region while the cut is on — voxel_texture.wgsl's chunk_cut_applies, per
// fragment instead of per chunk (a tile never straddles a seam either).
fn cut_applies(p: vec2<f32>) -> bool {
    let lo = fortress.origin.xz;
    let hi = lo + vec2<f32>(fortress.cut.w);
    return fortress.cut.x < CUT_OFF && all(p >= lo) && all(p < hi);
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);

    // Water above the cut is cut away in the home region. Last, after every
    // derivative use.
    if (cut_applies(in.world_position.xz) && in.world_position.y > fortress.cut.x + CUT_EPSILON) {
        discard;
    }
    return out;
}
