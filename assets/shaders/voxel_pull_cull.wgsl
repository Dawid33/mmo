struct ChunkMeta {
    center: vec4<f32>,
    half_extents: vec4<f32>,
    record_start: u32,
    quad_count: u32,
    transform_slot: u32,
    cave_active: u32,
    // records::face_ends: cumulative per-face range ends (PosX, NegX, PosY,
    // NegY, PosZ, NegZ) relative to record_start, then the unsorted flag.
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
    // Eye position; w = 1 culls back-facing face ranges (perspective views),
    // w = 0 keeps every face (shadow cascades).
    camera: vec4<f32>,
};

// `commands` first: indirect-count draws read the command count at offset 0.
struct DrawCount {
    commands: atomic<u32>,
    visible_quads: atomic<u32>,
    visible_chunks: atomic<u32>,
    pad: u32,
};

// Up to one command per face direction per chunk slot.
const FACES_PER_SLOT: u32 = 6u;

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
    return !(all_left || all_right || all_bottom || all_top || all_near);
}

fn face_end(chunk: ChunkMeta, face: u32) -> u32 {
    var end = 0u;
    if face < 4u {
        end = chunk.face_ends_lo[face];
    } else {
        end = chunk.face_ends_hi[face - 4u];
    }
    return min(end, chunk.quad_count);
}

// Whether any quad of `face` in this chunk can face the camera. Every face
// plane of direction +X lies at or above the AABB's min x, so with the eye at
// or below it all +X faces are seen from behind or edge-on (and likewise for
// the other five). Conservative; the rasterizer still back-face culls within
// kept ranges.
fn face_may_be_visible(chunk: ChunkMeta, face: u32) -> bool {
    if view.camera.w == 0.0 {
        return true;
    }
    let axis = face / 2u;
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

fn emit_command(slot: u32, chunk: ChunkMeta, visible: bool) {
    // Unsorted records (flag) draw as one whole range, never face-culled.
    let unsorted = chunk.face_ends_hi.z != 0u;
    var emitted_quads = 0u;
    var start = 0u;
    for (var face = 0u; face < FACES_PER_SLOT; face += 1u) {
        var end = face_end(chunk, face);
        if unsorted {
            end = select(0u, chunk.quad_count, face == 0u);
            start = 0u;
        }
        let count = end - min(start, end);
        let keep = visible && count > 0u && (unsorted || face_may_be_visible(chunk, face));
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
