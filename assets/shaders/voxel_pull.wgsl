// Pulled voxel forward shader. Record layout mirrored from
// crates/client/src/renderer/meshing/records.rs (the source of truth):
//   w0: x:6|y:6|z:6|face:3|ao0:2|ao1:2|ao2:2|ao3:2|flip:1   w1: w:6|h:6|layer:16
#import bevy_pbr::mesh_view_bindings::view
#import bevy_pbr::view_transformations::position_world_to_clip

struct ChunkUniform {
    world_from_local: mat4x4<f32>,
    quad_count: u32,
}

@group(1) @binding(0) var<storage, read> quads: array<vec2<u32>>;
@group(1) @binding(1) var<uniform> chunk: ChunkUniform;
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
fn vertex(@builtin(vertex_index) vi: u32) -> PullVertexOutput {
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
    out.world_position = chunk.world_from_local * vec4<f32>(local_pos, 1.0);
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
    // Unlit v0 — Task 5 replaces this with directional light + shadows + ambient.
    return vec4<f32>(tex.rgb * in.shade, 1.0);
}
