// Pulled voxel forward shader. Record layout mirrored from
// crates/client/src/renderer/meshing/records.rs (the source of truth):
//   w0: x:6|y:6|z:6|face:3|ao0:2|ao1:2|ao2:2|ao3:2|flip:1|cutout:1|interior:1   w1: w:6|h:6|layer:16
// The cutout bit needs no decoding: cutout records sit in their own range,
// drawn by the CUTOUT pipeline (alpha test, double-sided); the opaque
// pipeline never discards for alpha. The interior bit (the quad faces a dug
// space) drives the dug-space x-ray: the INTERIOR_PASS variant draws only
// interior quads into the view's interior depth, and the XRAY variant
// discards terrain in front of it (voxel_pull/xray.rs).
#import bevy_pbr::mesh_view_bindings::{view, lights}
#import bevy_pbr::mesh_view_types
#import bevy_pbr::fog::{linear_fog, exponential_fog, exponential_squared_fog, atmospheric_fog}
#import bevy_pbr::shadows::fetch_directional_shadow
#import bevy_pbr::view_transformations::position_world_to_clip
#import bevy_render::maths::PI

@group(1) @binding(0) var<storage, read> quads: array<vec2<u32>>;
@group(1) @binding(1) var<storage, read> chunk_transforms: array<mat4x4<f32>>;
@group(1) @binding(2) var mat_array_texture: texture_2d_array<f32>;
@group(1) @binding(3) var mat_array_texture_sampler: sampler;
// Side-face layer -> its block's top-face layer, identity elsewhere
// (BlockTextureLayers::side_to_top). Fragment stage only.
@group(1) @binding(4) var<storage, read> top_layers: array<u32>;

// Region boundary overlay, mirrored from BoundaryUniform in
// crates/client/src/renderer/boundaries.rs (the source of truth, incl. the
// meaning of every field). A home opacity (home_color.a) of 0 turns it off.
struct TerrainBoundaries {
    home_rect: vec4<f32>,
    home_color: vec4<f32>,
    shape: vec4<f32>,
    dim: vec4<f32>,
}
@group(1) @binding(5) var<uniform> boundaries: TerrainBoundaries;

// Fortress view (the z-level cutaway and the designation overlay),
// mirrored from FortressViewUniform in
// crates/client/src/renderer/fortress_view.rs (the source of truth, incl. the
// meaning of every field). The cut is read only under the CUTAWAY def, which
// the pipelines set while cut.x < f32::MAX; the overlay (on while cut.z > 0)
// in every variant. Both apply inside the home region only (cut_applies,
// overlay_bits).
struct FortressView {
    cut: vec4<f32>,
    origin: vec4<f32>,
    overlay_dims: vec4<u32>,
}
@group(1) @binding(6) var<uniform> fortress: FortressView;
// The designation overlay: one byte per home-region block cell, texel
// (x, z, y - base level) — fortress_view.rs OverlayGrid. Fragment stage only.
@group(1) @binding(7) var overlay: texture_3d<u32>;

#ifdef XRAY
// The view's interior depth (voxel_pull/xray.rs ViewXray; a never-written
// 1x1 stand-in reads 0 = nothing interior). Fragment stage only.
@group(2) @binding(0) var xray_depth: texture_depth_2d;
#endif
// Mirrored from voxel_pull/xray.rs (the source of truth, incl. meanings).
const XRAY_CEILING_GHOST: f32 = 0.1;
const XRAY_DEPTH_EPSILON: f32 = 2.0;
const XRAY_INTERIOR_AMBIENT_GAIN: f32 = 2.0;
// 4x4 ordered-dither thresholds (Bayer, 0..15) for the CEILING_GHOST fraction.
const XRAY_BAYER = array<f32, 16>(
    0.0, 8.0, 2.0, 10.0,
    12.0, 4.0, 14.0, 6.0,
    3.0, 11.0, 1.0, 9.0,
    15.0, 7.0, 13.0, 5.0,
);

// Mirrored from fortress_view.rs (the source of truth). The cap's darkening
// arrives as fortress.cut.y (CAP_DARKEN there).
const DEPTH_DARKEN_PER_LEVEL: f32 = 0.12;
const DEPTH_DARKEN_FLOOR: f32 = 0.35;
// Slack (world units) above the cut plane before geometry is clipped: a top
// face lying exactly on the plane must not flicker with interpolation error.
const CUT_EPSILON: f32 = 0.01;

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
// Baked AO fades out as LOD cells coarsen: full at scale 1, gone at
// 2^AO_FADE_LOG2 (one block per cell). An AO ramp spans one whole cell, so on
// coarse rings it is a block-wide dark band real terrain never shows.
const AO_FADE_LOG2: f32 = 4.0;

// Sky visibility of a side face relative to a top face: a vertical face sees
// half the sky dome plus ground bounce. Scales the ambient fill as
// SIDE + (1 - SIDE) * n.y — tops 1, sides SKY_SIDE, undersides 2*SIDE - 1.
const SKY_SIDE: f32 = 0.75;

// Distant side faces fade to their block's top colour and an upward lighting
// normal as one block's height shrinks from START to END pixels on screen.
// On a gentle slope every 1-block terrace riser is a grass block's side
// (~80 % dirt in grass_side.png), and at a shallow view angle the risers of
// a slope facing the camera cover most of it while the grass tops between
// them foreshorten to slivers — so the slope reads as brown contour stripes
// long before a riser gets thin enough to alias. Only a block's own side
// layer is remapped: dirt and stone below a cliff's grass lip keep theirs.
// START 24 px ≈ 36 m, END 6 px ≈ 145 m at a 45° fov, 720 px tall view.
const RISER_FADE_START_PX: f32 = 24.0;
const RISER_FADE_END_PX: f32 = 6.0;

// BEGIN cutout alpha test — identical in voxel_pull.wgsl and
// voxel_pull_depth.wgsl (test-pinned: voxel_pull/pipeline.rs). Cutout texels
// (leaves) have alpha 0 or 1; the mip chain averages it into coverage, so far
// away every texel would fall under the cutoff and leaves would thin to
// nothing. The tested alpha is scaled up by CUTOUT_MIP_ALPHA_SCALE per mip
// level the footprint spans (from the UV gradients), which keeps a crown's
// coverage roughly constant with distance.
const CUTOUT_ALPHA_CUTOFF: f32 = 0.5;
const CUTOUT_MIP_ALPHA_SCALE: f32 = 0.25;
fn cutout_alpha(alpha: f32, uv_dx: vec2<f32>, uv_dy: vec2<f32>, texels: vec2<f32>) -> f32 {
    let dx = uv_dx * texels;
    let dy = uv_dy * texels;
    let mip = max(0.0, 0.5 * log2(max(max(dot(dx, dx), dot(dy, dy)), 1e-8)));
    return alpha * (1.0 + mip * CUTOUT_MIP_ALPHA_SCALE);
}
// END cutout alpha test

// CORNER_PERM from records.rs, flattened: variant*4 + local vertex. Drawn
// indexed: 4 vertices per quad (vertex_index = quad*4 + local, base_vertex =
// record_start*4) and QUAD_INDEX_PATTERN 0,1,2, 2,1,3 from one shared buffer.
const CORNER_PERM = array<u32, 16>(
    1u, 3u, 0u, 2u,
    0u, 1u, 2u, 3u,
    1u, 0u, 3u, 2u,
    0u, 2u, 1u, 3u,
);

// Per-face (U,V) axis ids from binary::cross_axes: X faces→(1,2), Y→(0,2), Z→(0,1).
const FACE_UA = array<u32, 6>(1u, 1u, 0u, 0u, 0u, 0u);
const FACE_VA = array<u32, 6>(2u, 2u, 2u, 2u, 1u, 1u);

// fortress.cut.x at or above this means the cut is off (f32::MAX); every
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

// Whether a home-region quad is dropped by the cut (its lowest corner at
// `lowest_y`): wholly above it, or a ceiling (-Y, face 3) of a block wholly
// above the cut lying ON it, which would otherwise cap the dug room under it.
// Tops (+Y, face 2) on the cut stay: one exists only where the level above
// is air (or not a full block), which is exactly where the slice plane
// (slice_plane.wgsl) discards — no coplanar pair.
fn cut_collapses(face: u32, lowest_y: f32) -> bool {
    let cut = fortress.cut.x;
    let on_cut = lowest_y > cut - CUT_EPSILON;
    return lowest_y > cut + CUT_EPSILON || (face == 3u && on_cut);
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

// The overlay byte of the block cell holding world point `p` (0 = none):
// while the overlay is on, inside the home region and its levels.
fn overlay_bits(p: vec3<f32>) -> u32 {
    let cell = vec3<i32>(floor((p - fortress.origin.xyz) / BLOCK_VOXELS));
    let at = vec3<i32>(cell.x, cell.z, cell.y - i32(fortress.origin.w));
    let dims = vec3<i32>(fortress.overlay_dims.xyz);
    let inside = fortress.cut.z > 0.0 && all(at >= vec3<i32>(0)) && all(at < dims);
    let texel = clamp(at, vec3<i32>(0), max(dims - vec3<i32>(1), vec3<i32>(0)));
    return select(0u, textureLoad(overlay, texel, 0).r, inside);
}

// BEGIN texture_position — identical in voxel_pull.wgsl and
// voxel_pull_depth.wgsl (test-pinned: voxel_pull/pipeline.rs).
// Texture coordinates in voxels along the chunk's OWN axes (scaled), so one
// repeat is one block at every LOD factor and a voxel body's texture turns
// with it. Full-res bricks sit at whole multiples of BRICK_VOXELS and a
// body's bricks at whole blocks of its model, so their local position is
// already block-aligned. A LOD cell (scale > 1) is not — its skirt bias moves
// it -factor on y — so it adds back its origin's offset within its block
// (only that offset: the full world position would cost the interpolated UVs
// their precision far out).
fn texture_position(world_from_local: mat4x4<f32>, local_pos: vec3<f32>, scale: vec3<f32>) -> vec3<f32> {
    let origin = world_from_local[3].xyz;
    let phase = origin - BLOCK_VOXELS * floor(origin / BLOCK_VOXELS);
    return local_pos * scale + select(vec3<f32>(0.0), phase, scale.x > 1.0);
}
// END texture_position

struct PullVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) shade: f32,
    @location(4) @interpolate(flat) layer: u32,
    // 1 when the z-level cut applies to this quad (CUTAWAY only; 0 otherwise).
    @location(5) @interpolate(flat) cut_here: u32,
    // 1 when the quad faces a dug space (records.rs INTERIOR_BIT).
    @location(6) @interpolate(flat) interior: u32,
}

@vertex
fn vertex(
    @builtin(vertex_index) vi: u32,
    @builtin(instance_index) chunk_index: u32,
) -> PullVertexOutput {
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

    // Chunk transforms: terrain is translation + axis-aligned scale (full-res
    // bricks scale 1, LOD chunks their ring factor); a voxel body's bricks
    // are rotated too (scale 1).
    let world_from_local = chunk_transforms[chunk_index];
    let scale = vec3<f32>(
        length(world_from_local[0].xyz),
        length(world_from_local[1].xyz),
        length(world_from_local[2].xyz),
    );
    let tex_pos = texture_position(world_from_local, local_pos, scale);

    var out: PullVertexOutput;
    out.interior = (w0 >> 31u) & 1u;
#ifdef INTERIOR_PASS
    // The x-ray's interior depth holds only interior quads: collapse the
    // rest outside the clip volume, as the cut does.
    if (out.interior == 0u) {
        out.position = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        return out;
    }
#endif
#ifdef CUTAWAY
    // In the home region, a quad whose lowest corner (its record position:
    // extents only add) lies above the cut is collapsed to one point outside
    // the clip volume in x and y as well as z (depth clipping can be off in
    // shadow views) — a zero-area triangle pair the rasterizer drops. So are
    // the ceiling faces lying ON the cut (cut_collapses).
    let cut_here = cut_applies(quad_owner(world_from_local, record_pos, face, w1));
    let lowest = world_from_local * vec4<f32>(vec3<f32>(record_pos), 1.0);
    if (cut_here && cut_collapses(face, lowest.y)) {
        out.position = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        return out;
    }
    out.cut_here = select(0u, 1u, cut_here);
#endif
    out.world_position = world_from_local * vec4<f32>(local_pos, 1.0);
    out.position = position_world_to_clip(out.world_position.xyz);
    // Rotation + uniform scale: the transformed normal needs no
    // inverse-transpose, only renormalizing.
    out.world_normal = normalize((world_from_local * vec4<f32>(FACE_NORMALS[face], 0.0)).xyz);
    out.uv = vec2<f32>(
        dot(tex_pos, FACE_U_MASK[face]),
        dot(tex_pos, FACE_V_MASK[face]),
    ) / BLOCK_VOXELS;
    let ao = (w0 >> (21u + 2u * corner)) & 3u;
    // Must match ao::AO_CURVE / pack::shade (test-pinned there).
    let ao_strength = clamp(1.0 - log2(max(scale.x, 1.0)) / AO_FADE_LOG2, 0.0, 1.0);
    out.shade = mix(1.0, 0.55 + 0.15 * f32(ao), ao_strength);
    out.layer = (w1 >> 12u) & 65535u;
    return out;
}

// Nearest-looking magnification from a linear (anisotropic) sampler: snap
// the UV to the texel centre except within one pixel footprint of a texel
// seam, which stays linearly blended (antialiased). Minified, the footprint
// covers the whole texel and this is the plain UV. Sample with the ORIGINAL
// UV's gradients — the snapped UV's are discontinuous at seams.
fn crisp_uv(uv: vec2<f32>, texels: vec2<f32>) -> vec2<f32> {
    let px = uv * texels;
    let seam = floor(px + 0.5);
    let footprint = max(fwidth(px), vec2<f32>(1e-5));
    return (seam + clamp((px - seam) / footprint, vec2<f32>(-0.5), vec2<f32>(0.5))) / texels;
}

// Bevy's distance fog (the camera's DistanceFog, via the view uniform),
// without directional in-scattering — the terrain passes skip its extra
// shadow-map fetch. Same mode dispatch as pbr_functions::apply_fog. Bevy 0.19
// binds the fog uniform only for views with a DistanceFog (DISTANCE_FOG def).
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

// Draw the home region's perimeter onto `color` at ground point `p` (world
// xz) as a line `shape.x` world units wide (at least `shape.y` pixels) that
// starts on the edge and grows outward — nothing is drawn inside the play
// area — and dim what lies outside it.
fn apply_boundaries(color: vec3<f32>, p: vec2<f32>) -> vec3<f32> {
    let b = boundaries;
    if (b.home_color.a <= 0.0) {
        return color;
    }
    // Signed distance to the home rect (negative inside), and its screen
    // footprint, which converts it to pixels for antialiasing.
    let q = max(b.home_rect.xy - p, p - b.home_rect.zw);
    let sd = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0);
    let unit_px = 1.0 / max(fwidth(sd), 1e-4);
    let sd_px = sd * unit_px;
    var c = color;
    if (sd > 0.0) {
        // Outside home: darken and desaturate, eased in from the edge.
        let t = smoothstep(0.0, b.shape.z, sd);
        let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
        c = mix(c, vec3<f32>(luma), b.dim.y * t) * (1.0 - b.dim.x * t);
    }
    // Both antialiased edges sit outside the perimeter: the inner one fades
    // in over the first pixel past it.
    let w = max(b.shape.x * unit_px, b.shape.y);
    let line = smoothstep(0.0, 1.0, sd_px) * (1.0 - smoothstep(w, w + 1.0, sd_px));
    return mix(c, b.home_color.rgb, line * b.home_color.a);
}

#ifdef XRAY
// Whether the x-ray drops this fragment (at framebuffer position `frag`): it
// is not an interior face and lies more than XRAY_DEPTH_EPSILON in front of
// the nearest interior surface at its pixel — terrain between the eye and a
// dug space — except a XRAY_CEILING_GHOST dither of such pixels.
fn xray_hides(frag: vec4<f32>, interior: u32) -> bool {
    if (interior != 0u) {
        return false;
    }
    let size = vec2<i32>(textureDimensions(xray_depth));
    let px = clamp(vec2<i32>(frag.xy), vec2<i32>(0), size - vec2<i32>(1));
    let interior_depth = textureLoad(xray_depth, px, 0);
    if (interior_depth <= 0.0) {
        return false;
    }
    // Infinite reverse-Z perspective: depth = near / view distance.
    let near = view.clip_from_view[3][2];
    let in_front = near / max(frag.z, 1e-12) < near / interior_depth - XRAY_DEPTH_EPSILON;
    let ghost = (XRAY_BAYER[(px.y & 3) * 4 + (px.x & 3)] + 0.5) / 16.0 < XRAY_CEILING_GHOST;
    return in_front && !ghost;
}
#endif

// Side-face layer -> its block's top-face layer (identity for other layers).
fn top_layer(layer: u32) -> u32 {
    if (layer < arrayLength(&top_layers)) {
        return top_layers[layer];
    }
    return layer;
}

// Where the eye ray to `p` crosses the cut plane `cut`, when the eye is above
// it and `p` below; else `p` itself.
fn cut_crossing(p: vec3<f32>, cut: f32) -> vec3<f32> {
    let eye = view.world_position;
    let dy = p.y - eye.y;
    if (eye.y > cut && dy < 0.0) {
        return eye + (p - eye) * ((cut - eye.y) / dy);
    }
    return p;
}

// Brightness of a front face at world height `y` under the cut `cut`: full
// on the working level (its floor, the top faces at cut - one block,
// included), then DEPTH_DARKEN_PER_LEVEL less per level down to
// DEPTH_DARKEN_FLOOR.
fn depth_darkening(y: f32, cut: f32) -> f32 {
    let levels_below = max(0.0, ceil((cut - y) / BLOCK_VOXELS - 1.0 - 1e-3));
    return max(DEPTH_DARKEN_FLOOR, 1.0 - DEPTH_DARKEN_PER_LEVEL * levels_below);
}

@fragment
fn fragment(
    in: PullVertexOutput,
    @builtin(front_facing) front_facing: bool,
) -> @location(0) vec4<f32> {
    // The surface being shaded: the fragment itself, or under the cutaway a
    // back face's cap — every visible back face is one the eye ray reaches
    // after crossing the cut plane inside solid (what lies above the cut is
    // gone), so it is shaded where the ray crosses the plane: its block's top
    // texture, facing up, no AO. Depth stays the back face's: no frag_depth.
    var world_position = in.world_position;
    var face_uv = in.uv;
    var layer = in.layer;
    var shade = in.shade;
    var n = normalize(in.world_normal);
    // The block cell the overlay is read for: half a voxel back along the
    // normal from a face lands inside its solid block.
    var overlay_point = in.world_position.xyz - 0.5 * n;
#ifdef CUTOUT
    // Double-sided: a back face (the inside of a leaf, seen through the
    // front's holes) is lit from its own side.
    n = select(n, -n, !front_facing);
#endif
#ifdef CUTAWAY
    let cut = fortress.cut.x;
    let inside = in.cut_here != 0u;
#ifdef CUTOUT
    // A cutout back face is a leaf's far side, never the cut's cap.
    let cap = false;
#else
    let cap = inside && !front_facing;
#endif
    let cap_position = cut_crossing(in.world_position.xyz, cut);
    world_position = select(in.world_position, vec4<f32>(cap_position, 1.0), cap);
    face_uv = select(in.uv, cap_position.xz / BLOCK_VOXELS, cap);
    layer = select(in.layer, top_layer(in.layer), cap);
    shade = select(in.shade, 1.0, cap);
    n = select(n, vec3<f32>(0.0, 1.0, 0.0), cap);
    // A cap is the working level's cell under the cut plane.
    overlay_point = select(overlay_point, cap_position - vec3<f32>(0.0, 0.5, 0.0), cap);
#endif
    // Derivatives first, in uniform control flow.
    let texels = vec2<f32>(textureDimensions(mat_array_texture).xy);
    let uv = crisp_uv(face_uv, texels);
    let ddx = dpdx(face_uv);
    let ddy = dpdy(face_uv);
    var tex = textureSampleGrad(mat_array_texture, mat_array_texture_sampler, uv, layer, ddx, ddy);
#ifdef CUTOUT
    // Tested at the very end, after every derivative use.
    let coverage = cutout_alpha(tex.a, ddx, ddy, texels);
#endif

    // One block's height in pixels at this distance (perspective:
    // clip_from_view[1][1] = 1 / tan(fov_y / 2), viewport.w = height).
    let distance = length(world_position.xyz - view.world_position);
    let block_px = BLOCK_VOXELS * view.clip_from_view[1][1] * view.viewport.w
        / (2.0 * max(distance, 1e-3));
    let riser_fade = (1.0 - abs(n.y))
        * (1.0 - smoothstep(RISER_FADE_END_PX, RISER_FADE_START_PX, block_px));
    var light_n = n;
    if (riser_fade > 0.0) {
        let top_tex = textureSampleGrad(mat_array_texture, mat_array_texture_sampler, uv, top_layer(layer), ddx, ddy);
        tex = mix(tex, top_tex, riser_fade);
        light_n = normalize(mix(n, vec3<f32>(0.0, 1.0, 0.0), riser_fade));
    }
    // Designations tint the surface's albedo, so they light and darken with
    // it (by level below the cut too).
    let base = overlay_tint(tex.rgb, face_uv, overlay_bits(overlay_point)) * shade;

    // Cascade selection z, view-space depth of the fragment. Copied verbatim
    // from the real caller (bevy_pbr-0.18.1/src/render/pbr_functions.wgsl,
    // the directional-light block feeding fetch_directional_shadow).
    let view_z = dot(vec4<f32>(
        view.view_from_world[0].z,
        view.view_from_world[1].z,
        view.view_from_world[2].z,
        view.view_from_world[3].z,
    ), world_position);

    // Under the x-ray an interior face is lit as a room open to the sky: no
    // sun shadow (the ground above it still casts one), more ambient.
    var xray_lit = false;
#ifdef XRAY
    xray_lit = in.interior != 0u;
#endif
    var lit = lights.ambient_color.rgb * (SKY_SIDE + (1.0 - SKY_SIDE) * light_n.y)
        * select(1.0, XRAY_INTERIOR_AMBIENT_GAIN, xray_lit);
    for (var i = 0u; i < lights.n_directional_lights; i += 1u) {
        let light = lights.directional_lights[i];
        let ndotl = max(dot(light_n, light.direction_to_light), 0.0);
        var shadow = 1.0;
        if (ndotl > 0.0 && !xray_lit && (light.flags & mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u) {
            // The real face normal: it sets the shadow lookup's normal bias.
            shadow = fetch_directional_shadow(i, world_position, n, view_z, in.position.xy);
        }
        // 1/PI: the Lambertian diffuse normalization real PBR bakes into
        // Fd_Burley (pbr_lighting.wgsl) — dropping it overexposes by ~PI.
        lit += light.color.rgb * ndotl * shadow / PI;
    }
    // light.color and ambient_color carry raw illuminance (thousands, for a
    // default sun); real PBR scales the same accumulated sum by view.exposure
    // before adding emissive (pbr_functions.wgsl's apply_pbr_lighting, the
    // `view.exposure * (transmitted_light + direct_light + indirect_light)`
    // line) — without it every pixel saturates to white.
    lit *= view.exposure;
    var color = base * lit;
#ifdef CUTAWAY
    // In the home region the cap is darkened by cut.y, front faces below the
    // cut by depth; outside it nothing changes.
    let darkening = select(depth_darkening(in.world_position.y, cut), 1.0 - fortress.cut.y, cap);
    color *= select(1.0, darkening, inside);
#endif
    let overlaid = apply_boundaries(color, world_position.xz);
#ifdef CUTAWAY
    // Clip above the cut in the home region, and outside it drop back faces
    // (the CUTAWAY pipeline does not cull them). Last, after every derivative
    // (apply_boundaries' fwidth included): a discard compiled to SPIR-V
    // OpKill leaves the quad's derivatives undefined for the rest of the
    // shader. Only quads straddling the plane are clipped here — those wholly
    // above collapsed.
#ifdef CUTOUT
    // Double-sided: only the clip above the cut.
    if (inside && in.world_position.y > cut + CUT_EPSILON) {
        discard;
    }
#else
    if ((inside && in.world_position.y > cut + CUT_EPSILON) || (!inside && !front_facing)) {
        discard;
    }
#endif
#endif
#ifdef XRAY
    // Likewise after every derivative use.
    if (xray_hides(in.position, in.interior)) {
        discard;
    }
#endif
#ifdef CUTOUT
    // The alpha test, likewise last (after apply_boundaries' fwidth).
    if (coverage < CUTOUT_ALPHA_CUTOFF) {
        discard;
    }
#endif
    return vec4<f32>(apply_distance_fog(overlaid, world_position.xyz), 1.0);
}
