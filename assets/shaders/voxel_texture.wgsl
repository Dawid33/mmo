#import bevy_pbr::{
    mesh_functions,
    view_transformations::position_world_to_clip
}
#ifdef PREPASS_PIPELINE
// Vertex-only in the prepass: the depth/shadow fragment stage stays Bevy's
// default. prepass_io::FragmentOutput is gated behind PREPASS_FRAGMENT, so it
// must not be imported here.
#import bevy_pbr::prepass_io::VertexOutput
#else
#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::view,
}
#endif

@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var mat_array_texture: texture_2d_array<f32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(101)
var mat_array_texture_sampler: sampler;

// The z-level cutaway, mirrored from FortressViewUniform in
// crates/client/src/renderer/fortress_view.rs (the source of truth for every
// field), kept in step by voxel_material.rs sync_chunk_material_cut. Same
// rules as voxel_pull.wgsl's CUTAWAY, applied per vertex instead of per quad
// (a vertex here does not know its quad): inside the home region, horizontal
// faces above the cut — and ceilings lying ON it — collapse, vertical faces
// are clamped down to it (exactly the clipped quad), in every pass so cut-away
// rock casts no shadow; back faces become the cut's darkened cap (the
// pipeline stops culling while the cut is on, StandardVoxelMaterialKey).
struct FortressView {
    cut: vec4<f32>,
    origin: vec4<f32>,
    overlay_dims: vec4<u32>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> fortress: FortressView;

// Mirrored from voxel_pull.wgsl / fortress_view.rs.
const CUT_OFF: f32 = 3.0e38;
const CUT_EPSILON: f32 = 0.01;
const DEPTH_DARKEN_PER_LEVEL: f32 = 0.12;
const DEPTH_DARKEN_FLOOR: f32 = 0.35;

// Whether the cut applies to the chunk whose origin (min corner) is `origin`:
// chunks never straddle a region seam, so the whole mesh agrees.
fn chunk_cut_applies(origin: vec3<f32>) -> bool {
    let p = origin.xz + vec2<f32>(0.5);
    let lo = fortress.origin.xz;
    let hi = lo + vec2<f32>(fortress.cut.w);
    return fortress.cut.x < CUT_OFF && all(p >= lo) && all(p < hi);
}

// The chunk-local corner `local_pos` after the cut (see FortressView above).
// `scale_y`/`origin_y`: the chunk's vertical scale and world origin.
fn cut_local_position(local_pos: vec3<f32>, face: u32, origin: vec3<f32>, scale_y: f32) -> vec3<f32> {
    if (!chunk_cut_applies(origin)) {
        return local_pos;
    }
    let cut = fortress.cut.x;
    let y = origin.y + local_pos.y * scale_y;
    // +Y above the cut, or -Y on / above it (a ceiling on the cut would roof
    // over the dug room under it): collapse the whole quad onto one point.
    if ((face == 2u && y > cut + CUT_EPSILON) || (face == 3u && y > cut - CUT_EPSILON)) {
        return vec3<f32>(0.0);
    }
    var p = local_pos;
    p.y = min(p.y, (cut - origin.y) / scale_y);
    return p;
}

// Packed voxel vertex — the mesh's ONLY attribute (8 bytes/vertex); bit
// layout mirrored by crates/client/src/renderer/meshing/pack.rs (the Rust
// source of truth):
//   w0: x:9 | y:9 | z:9 | face:3 | ao:2   (positions chunk-local on a merged
//                                          sim-chunk mesh, brick-local otherwise)
//   w1: w:6 | h:6 | layer:16
struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(8) packed: vec2<u32>,
};

fn unpack_local_position(w0: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(w0 & 511u),
        f32((w0 >> 9u) & 511u),
        f32((w0 >> 18u) & 511u),
    );
}

fn packed_face(w0: u32) -> u32 {
    return (w0 >> 27u) & 7u;
}

// Indexed by the packed face id — order locked to Face::index() in binary.rs.
const FACE_NORMALS = array<vec3<f32>, 6>(
    vec3<f32>(1.0, 0.0, 0.0),
    vec3<f32>(-1.0, 0.0, 0.0),
    vec3<f32>(0.0, 1.0, 0.0),
    vec3<f32>(0.0, -1.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(0.0, 0.0, -1.0),
);

// Voxels per block edge — one texture repeat spans one block face. Must match
// game::block::BLOCK_VOXELS.
const BLOCK_VOXELS: f32 = 16.0;
// Baked AO fades out as LOD cells coarsen — see voxel_pull.wgsl.
const AO_FADE_LOG2: f32 = 4.0;
// Hemisphere ambient — locked to voxel_pull.wgsl's SKY_SIDE. (Its distant
// riser fade needs the pull path's top-layer table; this fallback path has
// none, and gets distance fog from PBR's post-lighting step instead.)
const SKY_SIDE: f32 = 0.75;

// Per-face UV sampling axes, indexed by the packed face id. This is a
// sampling-orientation choice (nothing else reads these UVs), NOT tied to
// binary.rs::cross_axes(), which only orders quad extents. Side faces (X/Z)
// sample upright: U = the face's horizontal axis, V = -Y so image-up is
// world-up (texture v=0 is the image's top row; the Repeat sampler wraps
// negative v). Y faces map U=X, V=Z. Opposite side faces mirror horizontally
// — irrelevant for natural art; revisit if blocks ever carry glyphs.
const FACE_U_MASK = array<vec3<f32>, 6>(
    vec3<f32>(0.0, 0.0, 1.0),  // +X: u = z
    vec3<f32>(0.0, 0.0, 1.0),  // -X: u = z
    vec3<f32>(1.0, 0.0, 0.0),  // +Y: u = x
    vec3<f32>(1.0, 0.0, 0.0),  // -Y: u = x
    vec3<f32>(1.0, 0.0, 0.0),  // +Z: u = x
    vec3<f32>(1.0, 0.0, 0.0),  // -Z: u = x
);
const FACE_V_MASK = array<vec3<f32>, 6>(
    vec3<f32>(0.0, -1.0, 0.0), // +X: v = -y (upright)
    vec3<f32>(0.0, -1.0, 0.0), // -X: v = -y (upright)
    vec3<f32>(0.0, 0.0, 1.0),  // +Y: v = z
    vec3<f32>(0.0, 0.0, 1.0),  // -Y: v = z
    vec3<f32>(0.0, -1.0, 0.0), // +Z: v = -y (upright)
    vec3<f32>(0.0, -1.0, 0.0), // -Z: v = -y (upright)
);

#ifdef PREPASS_PIPELINE

// Prepass/shadow vertex: decode the corner position from the packed word and
// populate prepass_io::VertexOutput — only the fields the active shader defs
// declare, under the same #ifdefs prepass_io.wgsl gates them with (see
// bevy_pbr's prepass.wgsl for the reference implementation).
@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let w0 = vertex.packed.x;
    var model = mesh_functions::get_world_from_local(vertex.instance_index);
    let local_pos = cut_local_position(
        unpack_local_position(w0), packed_face(w0), model[3].xyz, length(model[1].xyz));
    out.world_position = mesh_functions::mesh_position_local_to_world(
        model, vec4<f32>(local_pos, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.unclipped_depth = out.position.z;
    out.position.z = min(out.position.z, 1.0); // Clamp depth to avoid clipping
#endif
#ifdef NORMAL_PREPASS_OR_DEFERRED_PREPASS
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        FACE_NORMALS[packed_face(w0)], vertex.instance_index);
#endif
#ifdef MOTION_VECTOR_PREPASS
    // Chunk vertices don't animate; only the model transform can move.
    out.previous_world_position = mesh_functions::mesh_position_local_to_world(
        mesh_functions::get_previous_world_from_local(vertex.instance_index),
        vec4<f32>(local_pos, 1.0));
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif
#ifdef VISIBILITY_RANGE_DITHER
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        vertex.instance_index, model[3]);
#endif
    return out;
}

#else // PREPASS_PIPELINE

// Nearest-looking magnification from the linear sampler — copy of
// voxel_pull.wgsl's crisp_uv.
fn crisp_uv(uv: vec2<f32>, texels: vec2<f32>) -> vec2<f32> {
    let px = uv * texels;
    let seam = floor(px + 0.5);
    let footprint = max(fwidth(px), vec2<f32>(1e-5));
    return (seam + clamp((px - seam) / footprint, vec2<f32>(-0.5), vec2<f32>(0.5))) / texels;
}

struct CustomVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) shade: f32,
    @location(4) @interpolate(flat) layer: u32,
    // 1 when the z-level cut applies to this chunk (chunk_cut_applies).
    @location(5) @interpolate(flat) cut_here: u32,
    @location(6) @interpolate(flat) instance_index: u32,
}

// Side layer -> the same block's top layer (BlockTextureLayers::side_to_top):
// a cap shows the top of the block the cut slices.
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var top_layers: texture_2d<u32>;

fn top_layer(layer: u32) -> u32 {
    let len = textureDimensions(top_layers).x;
    return select(layer, textureLoad(top_layers, vec2<u32>(layer, 0u), 0).r, layer < len);
}

fn view_is_orthographic() -> bool {
    return view.clip_from_view[3][3] == 1.0;
}

fn view_forward() -> vec3<f32> {
    return -vec3<f32>(view.view_from_world[0].z, view.view_from_world[1].z, view.view_from_world[2].z);
}

// Copy of voxel_pull.wgsl's cut_crossing: where the eye ray to `p` crosses
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

// Copy of voxel_pull.wgsl's depth_darkening.
fn depth_darkening(y: f32, cut: f32) -> f32 {
    let levels_below = max(0.0, ceil((cut - y) / BLOCK_VOXELS - 1.0 - 1e-3));
    return max(DEPTH_DARKEN_FLOOR, 1.0 - DEPTH_DARKEN_PER_LEVEL * levels_below);
}

@vertex
fn vertex(vertex: Vertex) -> CustomVertexOutput {
    var out: CustomVertexOutput;
    let w0 = vertex.packed.x;
    let w1 = vertex.packed.y;
    let face = packed_face(w0);
    let ao = (w0 >> 30u) & 3u;

    var model = mesh_functions::get_world_from_local(vertex.instance_index);
    let local_pos = cut_local_position(
        unpack_local_position(w0), face, model[3].xyz, length(model[1].xyz));
    out.cut_here = select(0u, 1u, chunk_cut_applies(model[3].xyz));
    out.world_position = mesh_functions::mesh_position_local_to_world(
        model, vec4<f32>(local_pos, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        FACE_NORMALS[face], vertex.instance_index);
    // Block-cell UVs from the corner position in WORLD voxels (local cells
    // times the chunk's scale — LOD chunks scale by their ring factor — plus
    // the origin's offset within its block; same rule as voxel_pull.wgsl):
    // affine in position, so interpolation across greedy quads of any extent
    // is exact, and block bounds land on integer UVs so the Repeat sampler
    // wraps the texture once per block face at every LOD factor. The packed
    // cu/cv/w/h fields are no longer read here (layout unchanged, see pack.rs).
    let scale = vec3<f32>(length(model[0].xyz), length(model[1].xyz), length(model[2].xyz));
    let origin = model[3].xyz;
    let tex_pos = local_pos * scale + (origin - BLOCK_VOXELS * floor(origin / BLOCK_VOXELS));
    out.uv = vec2<f32>(
        dot(tex_pos, FACE_U_MASK[face]),
        dot(tex_pos, FACE_V_MASK[face]),
    ) / BLOCK_VOXELS;
    // Baked AO — must match ao::AO_CURVE (0.55 + 0.15 * ao); test-pinned in pack.rs.
    let ao_strength = clamp(1.0 - log2(max(scale.x, 1.0)) / AO_FADE_LOG2, 0.0, 1.0);
    out.shade = mix(1.0, 0.55 + 0.15 * f32(ao), ao_strength);
    out.layer = (w1 >> 12u) & 65535u;
    out.instance_index = vertex.instance_index;
    return out;
}

@fragment
fn fragment(
    in: CustomVertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    // The cut's cap: inside the home region a visible back face is where the
    // eye ray enters solid ground below the cut — drawn as that block's top
    // lying on the cut plane (as voxel_pull.wgsl does). Back faces exist only
    // while the cut is on (the pipeline culls them otherwise).
    let cut = fortress.cut.x;
    let inside = in.cut_here != 0u;
    let cap = inside && !is_front;
    let cap_position = cut_crossing(in.world_position.xyz, cut);
    let world_position = select(in.world_position, vec4<f32>(cap_position, 1.0), cap);
    let uv = select(in.uv, cap_position.xz / BLOCK_VOXELS, cap);
    let layer = select(in.layer, top_layer(in.layer), cap);
    let shade = select(in.shade, 1.0, cap);
    let normal = select(in.world_normal, vec3<f32>(0.0, 1.0, 0.0), cap);

    // Derivatives first, in uniform control flow.
    let texels = vec2<f32>(textureDimensions(mat_array_texture).xy);
    let tex = textureSampleGrad(
        mat_array_texture, mat_array_texture_sampler,
        crisp_uv(uv, texels), layer, dpdx(uv), dpdy(uv));

    var standard_in: VertexOutput;
    standard_in.position = in.position;
    standard_in.world_normal = normal;
    standard_in.world_position = world_position;
    standard_in.instance_index = in.instance_index;
    // A cap is lit as the up-facing top it shows, never flipped as a back face.
    var pbr_input = pbr_input_from_standard_material(standard_in, is_front || cap);

    // In the home region the cap is darkened by cut.y, front faces below the
    // cut by depth; outside it nothing changes.
    let darkening = select(depth_darkening(in.world_position.y, cut), 1.0 - fortress.cut.y, cap);
    let lit_shade = shade * select(1.0, darkening, inside);
    pbr_input.diffuse_occlusion = vec3<f32>(SKY_SIDE + (1.0 - SKY_SIDE) * normalize(normal).y);
    pbr_input.material.base_color = tex * vec4<f32>(lit_shade, lit_shade, lit_shade, 1.0);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);

    // Outside the home region back faces are not caps: drop them. Last, after
    // every derivative use.
    if (!inside && !is_front) {
        discard;
    }
    return out;
}

#endif // PREPASS_PIPELINE
