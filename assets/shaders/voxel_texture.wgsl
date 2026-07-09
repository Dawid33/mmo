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
    let cu = (w0 >> 23u) & 1u;
    let cv = (w0 >> 24u) & 1u;
    let quad_w = w1 & 63u;
    let quad_h = (w1 >> 6u) & 63u;

    var model = mesh_functions::get_world_from_local(vertex.instance_index);
    let local_pos = unpack_local_position(w0);
    out.world_position = mesh_functions::mesh_position_local_to_world(
        model, vec4<f32>(local_pos, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        FACE_NORMALS[face], vertex.instance_index);
    // Quad-extent UV tiling (repeat sampler wraps it per voxel cell).
    out.uv = vec2<f32>(f32(cu * quad_w), f32(cv * quad_h));
    // Baked AO — must match ao::AO_CURVE (0.55 + 0.15 * ao); test-pinned in pack.rs.
    out.shade = 0.55 + 0.15 * f32(ao);
    out.layer = (w1 >> 12u) & 65535u;
    out.instance_index = vertex.instance_index;
    return out;
}

@fragment
fn fragment(
    in: CustomVertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var standard_in: VertexOutput;
    standard_in.position = in.position;
    standard_in.world_normal = in.world_normal;
    standard_in.world_position = in.world_position;
    standard_in.instance_index = in.instance_index;
    var pbr_input = pbr_input_from_standard_material(standard_in, is_front);

    let tex = textureSample(mat_array_texture, mat_array_texture_sampler, in.uv, in.layer);
    pbr_input.material.base_color = tex * vec4<f32>(in.shade, in.shade, in.shade, 1.0);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);

    return out;
}

#endif // PREPASS_PIPELINE
