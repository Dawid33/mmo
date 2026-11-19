struct ChunkMeta {
    center: vec4<f32>,
    half_extents: vec4<f32>,
    record_start: u32,
    quad_count: u32,
    transform_slot: u32,
    cave_active: u32,
    // records::face_ends: cumulative per-face OPAQUE range ends (PosX, NegX,
    // PosY, NegY, PosZ, NegZ) relative to record_start, the unsorted flag,
    // then where the cutout range starts (it runs to quad_count).
    face_ends_lo: vec4<u32>,
    face_ends_hi: vec4<u32>,
};

// WebGPU's indexed indirect draw ABI (5 words). Every command indexes the
// shared quad index buffer from 0; base_vertex selects the chunk's records.
struct DrawIndexedIndirectArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
};

struct CullView {
    clip_from_world: mat4x4<f32>,
    // viewport width, viewport height, chunk capacity, HZB mip count.
    viewport_and_capacity: vec4<u32>,
    // Face-range culling (gpu.rs GpuNearCullView::camera): w = 1: xyz is the
    // eye of a perspective view; w = 2: xyz is the unit direction toward the
    // viewer of an orthographic one (shadow cascades, top-down); w = 0 keeps
    // every face.
    camera: vec4<f32>,
    // The z-level cut (gpu.rs GpuNearCullView::cut): x, y = home region world
    // x, z origin, z = its extent, w = cut plane y (>= CUT_OFF: no cut).
    cut: vec4<f32>,
    // Near detail bands (gpu.rs GpuNearCullView::detail): x, z = the MAIN
    // camera's eye (every view, shadow cascades included, so the camera and
    // its shadows pick the same mesh), w = the switch distance (<= 0: off).
    detail: vec4<f32>,
    // Shadow-caster culling (gpu.rs GpuNearCullView::receivers): the MAIN
    // camera's view volume out to the shadow distance, as inward half-spaces
    // (xyz = unit normal, dot(n, p) + w >= 0 inside); receiver_count.x of
    // them are set — in the sun's shadow views only (0: off).
    // receiver_count.y: 1 in every shadow view (frustum_visible).
    receivers: array<vec4<f32>, 6>,
    receiver_count: vec4<u32>,
};

// World units a caster may sit outside the lit view volume and still draw:
// shadow filtering reads a few texels around a receiver.
const RECEIVER_MARGIN: f32 = 64.0;

// Can this chunk shadow anything the main camera sees? The sun's shadow
// views only (camera.w = 2: camera.xyz is the unit direction toward the
// light; no other view is given receivers). A
// caster's shadow runs away from the light, so a half-space of the view
// volume whose inward normal has a component TOWARD the light bounds the
// casters too: a chunk wholly outside it only moves further out along its
// shadow. Half-spaces facing away from the light bound nothing (a caster
// outside them shadows into the volume) and are skipped; the silhouette
// planes between the two kinds are left out, which only keeps a few more.
fn casts_into_view(chunk: ChunkMeta) -> bool {
    if view.camera.w < 1.5 {
        return true;
    }
    let toward_light = view.camera.xyz;
    let count = min(view.receiver_count.x, 6u);
    for (var i = 0u; i < count; i += 1u) {
        let plane = view.receivers[i];
        if dot(plane.xyz, toward_light) <= 0.0 {
            continue;
        }
        let reach = dot(abs(plane.xyz), chunk.half_extents.xyz) + RECEIVER_MARGIN;
        if dot(plane.xyz, chunk.center.xyz) + plane.w + reach < 0.0 {
            return false;
        }
    }
    return true;
}

// ChunkMeta.center.w: which detail band a slot belongs to (voxel_pull
// DetailBand). A sim chunk with a coarse twin draws within the switch
// distance, its twin beyond it; everything else always.
const BAND_ALWAYS: u32 = 0u;
const BAND_FINE: u32 = 1u;
const BAND_COARSE: u32 = 2u;

// Horizontal distance from the main eye to the slot's xz rect decides the
// band. A chunk and its twin share that rect exactly, so in every view
// exactly one of the two draws. Twins are padded with their neighbours'
// boundary blocks (meshing/coarse.rs), so the z-level cut's cap (back faces
// below the plane) works through them as through full-detail chunks.
fn band_visible(chunk: ChunkMeta) -> bool {
    let band = u32(chunk.center.w + 0.5);
    if band == BAND_ALWAYS {
        return true;
    }
    if view.detail.w <= 0.0 {
        return band == BAND_FINE;
    }
    let lo = chunk.center.xz - chunk.half_extents.xz;
    let hi = chunk.center.xz + chunk.half_extents.xz;
    let eye = view.detail.xz;
    let gap = max(max(lo - eye, eye - hi), vec2<f32>(0.0));
    let near = dot(gap, gap) < view.detail.w * view.detail.w;
    return select(!near, near, band == BAND_FINE);
}

// voxel_pull_depth.wgsl's CUT_OFF / CUT_EPSILON.
const CUT_OFF: f32 = 3.0e38;
const CUT_EPSILON: f32 = 0.01;
// Below this |component| of the viewer direction a face axis is (nearly)
// edge-on to an orthographic view: keep both of its directions.
const EDGE_ON: f32 = 1.0e-4;

// How the z-level cut treats a chunk.
const CUT_NONE: u32 = 0u;   // outside the home region (or no cut): as usual
const CUT_ALL_FACES: u32 = 1u; // in the home region, not above the cut: its back faces are the cap
const CUT_HIDDEN: u32 = 2u; // wholly in the home region and above the cut: every quad collapses

fn cut_state(chunk: ChunkMeta) -> u32 {
    if view.cut.w >= CUT_OFF {
        return CUT_NONE;
    }
    let lo = view.cut.xy;
    let hi = lo + vec2<f32>(view.cut.z);
    let cmin = chunk.center.xyz - chunk.half_extents.xyz;
    let cmax = chunk.center.xyz + chunk.half_extents.xyz;
    // The shaders judge a quad by a point inside its own block, in [lo, hi).
    let overlaps = all(cmax.xz > lo) && all(cmin.xz < hi);
    if !overlaps {
        return CUT_NONE;
    }
    let inside = all(cmin.xz >= lo) && all(cmax.xz <= hi);
    if inside && cmin.y > view.cut.w + CUT_EPSILON {
        return CUT_HIDDEN;
    }
    return CUT_ALL_FACES;
}

// `commands` first: indirect-count draws read the opaque command count at
// offset 0, the cutout one at offset 12 (gpu.rs CUTOUT_COUNT_OFFSET).
struct DrawCount {
    commands: atomic<u32>,
    visible_quads: atomic<u32>,
    visible_chunks: atomic<u32>,
    cutout_commands: atomic<u32>,
};

// Up to one command per face direction per chunk slot.
const FACES_PER_SLOT: u32 = 6u;
// Then, after every slot's opaque commands, one cutout command per slot
// (gpu.rs CUTOUT_COMMANDS_PER_SLOT): its whole cutout range, not
// face-culled (the cutout pass is double-sided).

@group(0) @binding(0) var<storage, read> chunks: array<ChunkMeta>;
@group(0) @binding(1) var<storage, read_write> commands: array<DrawIndexedIndirectArgs>;
@group(0) @binding(2) var<storage, read_write> draw_count: DrawCount;
@group(0) @binding(3) var<uniform> view: CullView;

#ifdef HZB
@group(0) @binding(4) var depth_pyramid: texture_2d<f32>;
#endif

fn corner(chunk: ChunkMeta, index: u32) -> vec3<f32> {
    let signs = vec3<f32>(
        select(-1.0, 1.0, (index & 1u) != 0u),
        select(-1.0, 1.0, (index & 2u) != 0u),
        select(-1.0, 1.0, (index & 4u) != 0u),
    );
    return chunk.center.xyz + signs * chunk.half_extents.xyz;
}

// Homogeneous AABB test for WebGPU's x/y [-w,+w], z [0,+w] clip volume.
// The z >= 0 far half-space is deliberately omitted: Bevy cameras use an
// infinite reverse-Z projection, matching the CPU pulled-terrain cull.
fn frustum_visible(chunk: ChunkMeta) -> bool {
    if chunk.cave_active == 0u || chunk.quad_count == 0u {
        return false;
    }
    var all_left = true;
    var all_right = true;
    var all_bottom = true;
    var all_top = true;
    var all_near = true;
    for (var i = 0u; i < 8u; i += 1u) {
        let clip = view.clip_from_world * vec4<f32>(corner(chunk, i), 1.0);
        all_left = all_left && clip.x < -clip.w;
        all_right = all_right && clip.x > clip.w;
        all_bottom = all_bottom && clip.y < -clip.w;
        all_top = all_top && clip.y > clip.w;
        all_near = all_near && clip.z > clip.w;
    }
    // A shadow view (receiver_count.y) keeps what lies before its near
    // plane: a cascade is fitted to the slice of the world the camera sees,
    // and a caster between it and the sun — a tree behind or above the view
    // — still shadows into it (its depth is clamped, not clipped). Culling
    // those made a shadow vanish as soon as its caster left the view.
    all_near = all_near && view.receiver_count.y == 0u;
    return !(all_left || all_right || all_bottom || all_top || all_near);
}

// Where the chunk's cutout records start (the opaque ones end).
fn cutout_start(chunk: ChunkMeta) -> u32 {
    return min(chunk.face_ends_hi.w, chunk.quad_count);
}

fn face_end(chunk: ChunkMeta, face: u32) -> u32 {
    var end = 0u;
    if face < 4u {
        end = chunk.face_ends_lo[face];
    } else {
        end = chunk.face_ends_hi[face - 4u];
    }
    return min(end, cutout_start(chunk));
}

// Whether any quad of `face` in this chunk can face the camera. Every face
// plane of direction +X lies at or above the AABB's min x, so with the eye at
// or below it all +X faces are seen from behind or edge-on (and likewise for
// the other five). Conservative; the rasterizer still back-face culls within
// kept ranges.
fn face_may_be_visible(chunk: ChunkMeta, face: u32, cut: u32) -> bool {
    if view.camera.w == 0.0 || cut == CUT_ALL_FACES {
        return true;
    }
    let axis = face / 2u;
    if view.camera.w > 1.5 {
        // Orthographic: a +axis face looks at the viewer iff the direction
        // toward it has a positive component on that axis.
        let toward = view.camera[axis];
        if (face & 1u) == 0u {
            return toward > -EDGE_ON;
        }
        return toward < EDGE_ON;
    }
    let lo = chunk.center[axis] - chunk.half_extents[axis];
    let hi = chunk.center[axis] + chunk.half_extents[axis];
    let eye = view.camera[axis];
    if (face & 1u) == 0u {
        return eye > lo;
    }
    return eye < hi;
}

fn write_command(index: u32, chunk: ChunkMeta, start: u32, count: u32) {
    commands[index] = DrawIndexedIndirectArgs(
        count * 6u,
        1u,
        0u,
        i32((chunk.record_start + start) * 4u),
        chunk.transform_slot,
    );
}

fn emit_command(slot: u32, chunk: ChunkMeta, frustum_ok: bool) {
    let cut = cut_state(chunk);
    let visible = frustum_ok && cut != CUT_HIDDEN && band_visible(chunk) && casts_into_view(chunk);
    // Unsorted records (flag) draw as one whole range, never face-culled.
    let unsorted = chunk.face_ends_hi.z != 0u;
    var emitted_quads = 0u;
    var start = 0u;
    for (var face = 0u; face < FACES_PER_SLOT; face += 1u) {
        var end = face_end(chunk, face);
        if unsorted {
            end = select(0u, cutout_start(chunk), face == 0u);
            start = 0u;
        }
        let count = end - min(start, end);
        let keep = visible && count > 0u && (unsorted || face_may_be_visible(chunk, face, cut));
#ifdef INDIRECT_COUNT
        if keep {
            let output = atomicAdd(&draw_count.commands, 1u);
            write_command(output, chunk, start, count);
        }
#else
        write_command(slot * FACES_PER_SLOT + face, chunk, start, select(0u, count, keep));
        if keep {
            atomicAdd(&draw_count.commands, 1u);
        }
#endif
        if keep {
            emitted_quads += count;
        }
        if !unsorted {
            start = end;
        }
    }
    // The cutout range: one command per slot, after all opaque commands.
    let cutout_first = cutout_start(chunk);
    let cutout_count = chunk.quad_count - cutout_first;
    let cutout_keep = visible && cutout_count > 0u;
    let cutout_base = view.viewport_and_capacity.z * FACES_PER_SLOT;
#ifdef INDIRECT_COUNT
    if cutout_keep {
        let output = atomicAdd(&draw_count.cutout_commands, 1u);
        write_command(cutout_base + output, chunk, cutout_first, cutout_count);
    }
#else
    write_command(cutout_base + slot, chunk, cutout_first, select(0u, cutout_count, cutout_keep));
    if cutout_keep {
        atomicAdd(&draw_count.cutout_commands, 1u);
    }
#endif
    if cutout_keep {
        emitted_quads += cutout_count;
    }
    if visible {
        atomicAdd(&draw_count.visible_chunks, 1u);
        atomicAdd(&draw_count.visible_quads, emitted_quads);
    }
}

fn hzb_visible(chunk: ChunkMeta) -> bool {
#ifndef HZB
    return true;
#else
    let mip_count = view.viewport_and_capacity.w;
    if mip_count == 0u {
        return true;
    }
    var uv_min = vec2<f32>(1e30, 1e30);
    var uv_max = vec2<f32>(-1e30, -1e30);
    var nearest_reverse_z = 0.0;
    for (var i = 0u; i < 8u; i += 1u) {
        let clip = view.clip_from_world * vec4<f32>(corner(chunk, i), 1.0);
        // Near-plane/camera intersections are ambiguous in screen space and
        // therefore conservatively survive.
        if clip.w <= 0.0001 || clip.z > clip.w {
            return true;
        }
        let ndc = clip.xyz / clip.w;
        // WebGPU textures use a top-left origin: flip projected Y before
        // reducing bounds so min/max describe the actual texture footprint.
        let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
        uv_min = min(uv_min, uv);
        uv_max = max(uv_max, uv);
        nearest_reverse_z = max(nearest_reverse_z, ndc.z);
    }
    if any(uv_min < vec2<f32>(0.0)) || any(uv_max > vec2<f32>(1.0)) {
        return true;
    }

    let base_size = vec2<f32>(textureDimensions(depth_pyramid, 0));
    let pixel_span = max((uv_max - uv_min) * base_size, vec2<f32>(1.0));
    let selected_mip = min(
        u32(max(0.0, ceil(log2(max(pixel_span.x, pixel_span.y))))),
        mip_count - 1u,
    );
    let mip_size = vec2<i32>(textureDimensions(depth_pyramid, i32(selected_mip)));
    let max_texel = mip_size - vec2<i32>(1);
    let texel_min = clamp(vec2<i32>(floor(uv_min * vec2<f32>(mip_size))), vec2<i32>(0), max_texel);
    let texel_max = clamp(vec2<i32>(floor(uv_max * vec2<f32>(mip_size))), vec2<i32>(0), max_texel);
    let bias = 0.0005;
    var definitely_occluded = true;
    // ceil(log2(span)) guarantees at most a 2x2 raster footprint. Sample
    // every covered texel: missing even one would make rejection non-conservative.
    for (var y = texel_min.y; y <= texel_max.y; y += 1) {
        for (var x = texel_min.x; x <= texel_max.x; x += 1) {
            definitely_occluded = definitely_occluded &&
                textureLoad(depth_pyramid, vec2<i32>(x, y), i32(selected_mip)).x >
                    nearest_reverse_z + bias;
        }
    }
    return !definitely_occluded;
#endif
}

@compute @workgroup_size(64)
fn frustum_cull(@builtin(global_invocation_id) id: vec3<u32>) {
    let slot = id.x;
    if slot >= view.viewport_and_capacity.z {
        return;
    }
    let chunk = chunks[slot];
    emit_command(slot, chunk, frustum_visible(chunk));
}

@compute @workgroup_size(64)
fn hzb_cull(@builtin(global_invocation_id) id: vec3<u32>) {
    let slot = id.x;
    if slot >= view.viewport_and_capacity.z {
        return;
    }
    let chunk = chunks[slot];
    let visible = frustum_visible(chunk) && hzb_visible(chunk);
    emit_command(slot, chunk, visible);
}
