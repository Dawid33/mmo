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
}
#endif

@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var mat_array_texture: texture_2d_array<f32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(101)
var mat_array_texture_sampler: sampler;

// Packed voxel vertex — the mesh's ONLY attribute (8 bytes/vertex); bit
// layout mirrored by crates/client/src/renderer/meshing/pack.rs (the Rust
// source of truth):
//   w0: x:6 | y:6 | z:6 | face:3 | ao:2 | cu:1 | cv:1
//   w1: w:6 | h:6 | layer:16
struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(8) packed: vec2<u32>,
};

fn unpack_local_position(w0: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(w0 & 63u),
        f32((w0 >> 6u) & 63u),
        f32((w0 >> 12u) & 63u),
    );
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
    let local_pos = unpack_local_position(w0);
    var model = mesh_functions::get_world_from_local(vertex.instance_index);
    out.world_position = mesh_functions::mesh_position_local_to_world(
        model, vec4<f32>(local_pos, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.unclipped_depth = out.position.z;
    out.position.z = min(out.position.z, 1.0); // Clamp depth to avoid clipping
#endif
#ifdef NORMAL_PREPASS_OR_DEFERRED_PREPASS
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        FACE_NORMALS[(w0 >> 18u) & 7u], vertex.instance_index);
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
    @location(6) @interpolate(flat) instance_index: u32,
}

@vertex
fn vertex(vertex: Vertex) -> CustomVertexOutput {
    var out: CustomVertexOutput;
    let w0 = vertex.packed.x;
    let w1 = vertex.packed.y;
    let face = (w0 >> 18u) & 7u;
    let ao = (w0 >> 21u) & 3u;

    var model = mesh_functions::get_world_from_local(vertex.instance_index);
    let local_pos = unpack_local_position(w0);
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
    // Derivatives first, in uniform control flow.
    let texels = vec2<f32>(textureDimensions(mat_array_texture).xy);
    let tex = textureSampleGrad(
        mat_array_texture, mat_array_texture_sampler,
        crisp_uv(in.uv, texels), in.layer, dpdx(in.uv), dpdy(in.uv));

    var standard_in: VertexOutput;
    standard_in.position = in.position;
    standard_in.world_normal = in.world_normal;
    standard_in.world_position = in.world_position;
    standard_in.instance_index = in.instance_index;
    var pbr_input = pbr_input_from_standard_material(standard_in, is_front);

    pbr_input.diffuse_occlusion = vec3<f32>(SKY_SIDE + (1.0 - SKY_SIDE) * normalize(in.world_normal).y);
    pbr_input.material.base_color = tex * vec4<f32>(in.shade, in.shade, in.shade, 1.0);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);

    return out;
}

#endif // PREPASS_PIPELINE
