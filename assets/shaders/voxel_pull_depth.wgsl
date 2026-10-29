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

// group(1) is the same shared arena bind group used by the forward path. The
// depth pipeline reuses that exact layout and simply leaves bindings 2/3
// (texture/sampler) undeclared — a shader may reference fewer bindings than its
// layout provides.
@group(1) @binding(0) var<storage, read> quads: array<vec2<u32>>;
@group(1) @binding(1) var<storage, read> chunk_transforms: array<mat4x4<f32>>;

// The z-level cutaway, as in voxel_pull.wgsl (mirrored from
// FortressViewUniform in crates/client/src/renderer/fortress_view.rs, the
// source of truth): geometry above the cut casts no shadow and writes no
// prepass depth either.
struct FortressView {
    cut: vec4<f32>,
    origin: vec4<f32>,
    overlay_dims: vec4<u32>,
}
@group(1) @binding(6) var<uniform> fortress: FortressView;
// Same slack as voxel_pull.wgsl's CUT_EPSILON.
const CUT_EPSILON: f32 = 0.01;

// Locked to Face::index() order (PosX, NegX, PosY, NegY, PosZ, NegZ).
// Per-face (U,V) axis ids from binary::cross_axes: X faces→(1,2), Y→(0,2), Z→(0,1).
const FACE_UA = array<u32, 6>(1u, 1u, 0u, 0u, 0u, 0u);
const FACE_VA = array<u32, 6>(2u, 2u, 2u, 2u, 1u, 1u);

// The cutaway's region gate, copied from voxel_pull.wgsl (see there).// fortress.cut.x at or above this means the cut is off (f32::MAX); every
// cutaway effect is then a no-op even in a CUTAWAY pipeline.
const CUT_OFF: f32 = 3.0e38;

// Whether the cut applies at world point `p`: the cut is on and `p` lies in
// the home region's xz rect [origin.xz, origin.xz + cut.w). Outside it the
// terrain draws as without the cut.
fn cut_applies(p: vec3<f32>) -> bool {
    let lo = fortress.origin.xz;
    let hi = lo + vec2<f32>(fortress.cut.w);
    return fortress.cut.x < CUT_OFF && all(p.xz >= lo) && all(p.xz < hi);
}

// The point a quad is judged by, so all four of its vertices agree: its
// centre, pulled half a cell back along the face normal into the block the
// face belongs to (a face lying exactly on the region edge then counts for
// the region of its block). Quads are chunk-local, so never span the edge.
fn quad_owner(world_from_local: mat4x4<f32>, record_pos: vec3<u32>, face: u32, w1: u32) -> vec3<f32> {
    var mid = vec3<f32>(record_pos);
    mid[FACE_UA[face]] += 0.5 * f32(w1 & 63u);
    mid[FACE_VA[face]] += 0.5 * f32((w1 >> 6u) & 63u);
    var normal = vec3<f32>(0.0);
    normal[face / 2u] = select(-1.0, 1.0, face % 2u == 0u);
    return (world_from_local * vec4<f32>(mid - 0.5 * normal, 1.0)).xyz;
}

// CORNER_PERM from records.rs, flattened: variant*4 + local vertex. Drawn
// indexed: 4 vertices per quad (vertex_index = quad*4 + local, base_vertex =
// record_start*4) and QUAD_INDEX_PATTERN 0,1,2, 2,1,3 from one shared buffer.
const CORNER_PERM = array<u32, 16>(
    1u, 3u, 0u, 2u,
    0u, 1u, 2u, 3u,
    1u, 0u, 3u, 2u,
    0u, 2u, 1u, 3u,
);

// Directional-light cascade shadow views on GPUs without DEPTH_CLIP_CONTROL render
// with unclipped depth emulated in the fragment stage (mirrors bevy's prepass_io.wgsl
// / pbr_prepass.wgsl): the vertex clamps clip-space z so the near plane doesn't clip
// the caster, and carries the true pre-clamp depth to the fragment, which writes it to
// @builtin(frag_depth). Without the def this stays a bare-position vertex-only shader.
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    @location(0) unclipped_depth: f32,
#endif // UNCLIPPED_DEPTH_ORTHO_EMULATION
#ifdef CUTAWAY
    @location(1) world_y: f32,
    @location(2) @interpolate(flat) cut_here: u32,
#endif // CUTAWAY
}

@vertex
fn vertex(
    @builtin(vertex_index) vi: u32,
    @builtin(instance_index) chunk_index: u32,
) -> VertexOutput {
    let quad = vi / 4u;
    let local = vi % 4u;
    let rec = quads[quad];
    let w0 = rec.x;
    let w1 = rec.y;
    let face = (w0 >> 18u) & 7u;
    let flip = (w0 >> 29u) & 1u;
    // Reversed winding faces: NegX(1), PosY(2), NegZ(5).
    var reversed = 0u;
    if (face == 1u || face == 2u || face == 5u) { reversed = 1u; }
    let corner = CORNER_PERM[(reversed * 2u + flip) * 4u + local];
    let cu = corner & 1u;
    let cv = corner >> 1u;

    let record_pos = vec3<u32>(w0 & 63u, (w0 >> 6u) & 63u, (w0 >> 12u) & 63u);
    var pos = record_pos;
    pos[FACE_UA[face]] += cu * (w1 & 63u);
    pos[FACE_VA[face]] += cv * ((w1 >> 6u) & 63u);
    let local_pos = vec3<f32>(pos);

    let world_from_local = chunk_transforms[chunk_index];
    let world_position = world_from_local * vec4<f32>(local_pos, 1.0);
    var out: VertexOutput;
#ifdef CUTAWAY
    // Collapse a home-region quad wholly above the cut (see voxel_pull.wgsl).
    let cut_here = cut_applies(quad_owner(world_from_local, record_pos, face, w1));
    let lowest = world_from_local * vec4<f32>(vec3<f32>(record_pos), 1.0);
    if (cut_here && lowest.y > fortress.cut.x + CUT_EPSILON) {
        out.position = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        return out;
    }
    out.world_y = world_position.y;
    out.cut_here = select(0u, 1u, cut_here);
#endif // CUTAWAY
    out.position = position_world_to_clip(world_position.xyz);
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.unclipped_depth = out.position.z;
    out.position.z = min(out.position.z, 1.0); // Clamp depth to avoid clipping
#endif // UNCLIPPED_DEPTH_ORTHO_EMULATION
    return out;
}

#ifdef CUTAWAY
// In the home region: clipped above the cut. Outside it: back faces dropped,
// as the Back cull mode the CUTAWAY pipeline gives up would.
fn cut_discards(in: VertexOutput, front_facing: bool) -> bool {
    let inside = in.cut_here != 0u;
    return (inside && in.world_y > fortress.cut.x + CUT_EPSILON) || (!inside && !front_facing);
}
#endif // CUTAWAY

#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) front_facing: bool) -> @builtin(frag_depth) f32 {
#ifdef CUTAWAY
    if (cut_discards(in, front_facing)) {
        discard;
    }
#endif // CUTAWAY
    return in.unclipped_depth;
}
#else // UNCLIPPED_DEPTH_ORTHO_EMULATION
#ifdef CUTAWAY
// No depth to write: only the cutaway's discard.
@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) front_facing: bool) {
    if (cut_discards(in, front_facing)) {
        discard;
    }
}
#endif // CUTAWAY
#endif // UNCLIPPED_DEPTH_ORTHO_EMULATION
