// Depth-prepass variant of the pulled voxel shader: the same vertex geometry
// pull as voxel_pull.wgsl, but vertex-only (no fragment entry) so it writes just
// depth. Keeps the camera's HZB fed with pulled terrain so mesh entities behind
// terrain stay occlusion-culled. Record layout mirrored from
// crates/client/src/renderer/meshing/records.rs (the source of truth):
//   w0: x:6|y:6|z:6|face:3|ao0:2|ao1:2|ao2:2|ao3:2|flip:1   w1: w:6|h:6|layer:16
//
// Bound against the PREPASS view layout (SetPrepassViewBindGroup<0>), whose
// binding 0 is the same `View` uniform mesh_view_bindings exposes — so importing
// only `view` (not the whole mesh_view_bindings set) matches that layout, exactly
// as bevy_pbr-0.18.1/src/prepass/prepass.wgsl does.
#import bevy_pbr::mesh_view_bindings::view
#import bevy_pbr::view_transformations::position_world_to_clip

struct ChunkUniform {
    world_from_local: mat4x4<f32>,
    quad_count: u32,
}

// group(1) is the same per-chunk bind group the forward path builds; the depth
// pipeline reuses that exact layout (so GpuChunkQuads.bind_group stays valid),
// and simply leaves bindings 2/3 (texture/sampler) undeclared — a shader may
// reference fewer bindings than its layout provides.
@group(1) @binding(0) var<storage, read> quads: array<vec2<u32>>;
@group(1) @binding(1) var<uniform> chunk: ChunkUniform;

// Locked to Face::index() order (PosX, NegX, PosY, NegY, PosZ, NegZ).
// Per-face (U,V) axis ids from binary::cross_axes: X faces→(1,2), Y→(0,2), Z→(0,1).
const FACE_UA = array<u32, 6>(1u, 1u, 0u, 0u, 0u, 0u);
const FACE_VA = array<u32, 6>(2u, 2u, 2u, 2u, 1u, 1u);

// CORNER_SLOTS from records.rs, flattened: variant*6 + slot.
const CORNER_SLOTS = array<u32, 24>(
    0u, 1u, 3u, 0u, 3u, 2u,
    0u, 1u, 2u, 1u, 3u, 2u,
    0u, 3u, 1u, 0u, 2u, 3u,
    0u, 2u, 1u, 1u, 2u, 3u,
);

@vertex
fn vertex(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
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

    let world_position = chunk.world_from_local * vec4<f32>(local_pos, 1.0);
    return position_world_to_clip(world_position.xyz);
}
