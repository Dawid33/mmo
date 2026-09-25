// Native far-field brick raymarcher. Compiled twice from one source:
// FAR_CULL builds visible-cell compaction, FAR_RENDER builds the proxy draw.

#ifdef FAR_CULL

struct GpuFarCell {
    origin: vec3<f32>,
    factor: u32,
    root_handle: u32,
    is_active: u32,
    header_padding: vec2<u32>,
    neighbor_slots: array<u32, 26>,
    tail_padding: vec2<u32>,
}

struct CullUniform {
    planes: array<vec4<f32>, 6>,
    cell_capacity: u32,
}

struct DrawIndirectArgs {
    vertex_count: u32,
    instance_count: atomic<u32>,
    first_vertex: u32,
    first_instance: u32,
}

@group(0) @binding(0) var<uniform> cull_uniform: CullUniform;
@group(0) @binding(1) var<storage, read> cells: array<GpuFarCell>;
@group(0) @binding(2) var<storage, read_write> visible_cell_slots: array<u32>;
@group(0) @binding(3) var<storage, read_write> draw_args: DrawIndirectArgs;

@compute @workgroup_size(64)
fn cull(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let cell_slot = invocation.x;
    if cell_slot >= cull_uniform.cell_capacity {
        return;
    }
    let cell = cells[cell_slot];
    // 1 = drawn; 2 = hidden but still solid to its neighbours (home-region
    // LOD under active cells); 0 = free slot.
    if cell.is_active != 1u || cell.factor == 0u {
        return;
    }

    let half_extent = 16.0 * f32(cell.factor);
    let center = cell.origin + vec3(half_extent);
    for (var plane_index = 0u; plane_index < 5u; plane_index += 1u) {
        let plane = cull_uniform.planes[plane_index];
        let radius = dot(abs(plane.xyz), vec3(half_extent));
        if dot(plane.xyz, center) + plane.w + radius < 0.0 {
            return;
        }
    }

    let visible_index = atomicAdd(&draw_args.instance_count, 1u);
    visible_cell_slots[visible_index] = cell_slot;
}

#else // FAR_RENDER

#import bevy_pbr::mesh_view_bindings as view_bindings
#import bevy_pbr::mesh_view_types
#import bevy_pbr::fog::{linear_fog, exponential_fog, exponential_squared_fog, atmospheric_fog}
#import bevy_pbr::view_transformations::{
    frag_coord_to_ndc,
    position_ndc_to_world,
    position_world_to_clip,
}
#import bevy_render::maths::PI

const CELL_EDGE: i32 = 32;
const BRICK_EDGE: i32 = 8;
const BRICKS_PER_AXIS: i32 = 4;
const INVALID_FAR_SLOT: u32 = 0xffffffffu;
const ROOT_PAYLOAD_MASK: u32 = 0x3fffffffu;
const RootEntryKind_EMPTY: u32 = 0u;
const RootEntryKind_UNIFORM: u32 = 1u;
const RootEntryKind_PAGE_0: u32 = 2u;
const RootEntryKind_PAGE_1: u32 = 3u;
const DDA_EPSILON: f32 = 0.00001;
// LOD depth is written this far BEHIND its true hit (world units: a constant
// plus a fraction of the ray length), so full-detail terrain wins every tie
// where the two overlap — the seam between loaded cells and the home region's
// LOD z-fought at distance, where depth precision can't separate coplanar
// surfaces computed two different ways. Colour and fog use the true hit.
const FAR_DEPTH_PUSH_MIN: f32 = 0.5;
const FAR_DEPTH_PUSH_REL: f32 = 0.002;
const AO_CURVE: array<f32, 4> = array(0.55, 0.70, 0.85, 1.0);

struct GpuFarCell {
    origin: vec3<f32>,
    factor: u32,
    root_handle: u32,
    is_active: u32,
    header_padding: vec2<u32>,
    neighbor_slots: array<u32, 26>,
    tail_padding: vec2<u32>,
}

struct BrickRoot {
    entries: array<u32, 64>,
}

struct MixedBrick {
    occupancy: array<u32, 16>,
    materials: array<u32, 256>,
}

@group(1) @binding(0) var<storage, read> cells: array<GpuFarCell>;
@group(1) @binding(1) var<storage, read> roots: array<BrickRoot>;
@group(1) @binding(2) var<storage, read> mixed_page_0: array<MixedBrick>;
@group(1) @binding(3) var<storage, read> mixed_page_1: array<MixedBrick>;
@group(1) @binding(4) var<storage, read> visible_cell_slots: array<u32>;
@group(1) @binding(5) var material_layer_lut: texture_2d<u32>;
@group(1) @binding(6) var block_texture: texture_2d_array<f32>;
@group(1) @binding(7) var block_sampler: sampler;

const PROXY_VERTICES: array<vec3<f32>, 36> = array(
    vec3(0.0, 0.0, 0.0), vec3(0.0, 1.0, 0.0), vec3(1.0, 1.0, 0.0),
    vec3(0.0, 0.0, 0.0), vec3(1.0, 1.0, 0.0), vec3(1.0, 0.0, 0.0),
    vec3(1.0, 0.0, 1.0), vec3(1.0, 1.0, 1.0), vec3(0.0, 1.0, 1.0),
    vec3(1.0, 0.0, 1.0), vec3(0.0, 1.0, 1.0), vec3(0.0, 0.0, 1.0),
    vec3(0.0, 0.0, 1.0), vec3(0.0, 1.0, 1.0), vec3(0.0, 1.0, 0.0),
    vec3(0.0, 0.0, 1.0), vec3(0.0, 1.0, 0.0), vec3(0.0, 0.0, 0.0),
    vec3(1.0, 0.0, 0.0), vec3(1.0, 1.0, 0.0), vec3(1.0, 1.0, 1.0),
    vec3(1.0, 0.0, 0.0), vec3(1.0, 1.0, 1.0), vec3(1.0, 0.0, 1.0),
    vec3(0.0, 0.0, 1.0), vec3(0.0, 0.0, 0.0), vec3(1.0, 0.0, 0.0),
    vec3(0.0, 0.0, 1.0), vec3(1.0, 0.0, 0.0), vec3(1.0, 0.0, 1.0),
    vec3(0.0, 1.0, 0.0), vec3(0.0, 1.0, 1.0), vec3(1.0, 1.0, 1.0),
    vec3(0.0, 1.0, 0.0), vec3(1.0, 1.0, 1.0), vec3(1.0, 1.0, 0.0),
);

struct ProxyVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) proxy_world: vec3<f32>,
    @location(1) @interpolate(flat) cell_slot: u32,
}

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> ProxyVertexOutput {
    let cell_slot = visible_cell_slots[instance_index];
    let cell = cells[cell_slot];
    let proxy_world = cell.origin
        + PROXY_VERTICES[vertex_index] * (32.0 * f32(cell.factor));
    var out: ProxyVertexOutput;
    out.clip_position = position_world_to_clip(proxy_world);
    out.proxy_world = proxy_world;
    out.cell_slot = cell_slot;
    return out;
}

struct SlabHit {
    valid: bool,
    entry_t: f32,
    exit_t: f32,
    entry_normal: vec3<i32>,
}

fn axis_normal(axis: u32, sign: i32) -> vec3<i32> {
    var normal = vec3<i32>(0);
    normal[axis] = sign;
    return normal;
}

fn intersect_cell_aabb(
    ray_origin: vec3<f32>,
    ray_direction: vec3<f32>,
    minimum: vec3<f32>,
    maximum: vec3<f32>,
) -> SlabHit {
    var entry_t = -1e30;
    var exit_t = 1e30;
    var entry_normal = vec3<i32>(0);
    for (var axis = 0u; axis < 3u; axis += 1u) {
        let origin = ray_origin[axis];
        let direction = ray_direction[axis];
        if abs(direction) <= 1e-8 {
            if origin < minimum[axis] || origin > maximum[axis] {
                return SlabHit(false, 0.0, 0.0, vec3<i32>(0));
            }
        } else {
            let a = (minimum[axis] - origin) / direction;
            let b = (maximum[axis] - origin) / direction;
            let near_t = min(a, b);
            let far_t = max(a, b);
            if near_t > entry_t {
                entry_t = near_t;
                entry_normal = axis_normal(axis, select(1, -1, direction > 0.0));
            }
            exit_t = min(exit_t, far_t);
            if entry_t > exit_t {
                return SlabHit(false, 0.0, 0.0, vec3<i32>(0));
            }
        }
    }
    if exit_t < 0.0 {
        return SlabHit(false, 0.0, 0.0, vec3<i32>(0));
    }
    if entry_t < 0.0 {
        entry_t = 0.0;
        entry_normal = vec3<i32>(0);
    }
    return SlabHit(true, entry_t, exit_t, entry_normal);
}

struct DdaState {
    coord: vec3<i32>,
    step: vec3<i32>,
    t_max: vec3<f32>,
    t_delta: vec3<f32>,
    t: f32,
    normal: vec3<i32>,
}

fn dda_new(
    ray_origin: vec3<f32>,
    ray_direction: vec3<f32>,
    grid_origin: vec3<f32>,
    cell_size: f32,
    dimension: i32,
    entry_t: f32,
    entry_normal: vec3<i32>,
) -> DdaState {
    let point = ray_origin + ray_direction * entry_t;
    let relative = (point - grid_origin) / cell_size;
    var coord = vec3<i32>(floor(relative));
    for (var axis = 0u; axis < 3u; axis += 1u) {
        let rounded = round(relative[axis]);
        if ray_direction[axis] < 0.0
            && abs(relative[axis] - rounded) <= DDA_EPSILON {
            coord[axis] -= 1;
        }
        coord[axis] = clamp(coord[axis], 0, dimension - 1);
    }

    var step = vec3<i32>(0);
    var t_max = vec3<f32>(1e30);
    var t_delta = vec3<f32>(1e30);
    for (var axis = 0u; axis < 3u; axis += 1u) {
        let direction = ray_direction[axis];
        if direction > 1e-8 {
            step[axis] = 1;
            let boundary = grid_origin[axis] + f32(coord[axis] + 1) * cell_size;
            t_max[axis] = (boundary - ray_origin[axis]) / direction;
            t_delta[axis] = cell_size / direction;
        } else if direction < -1e-8 {
            step[axis] = -1;
            let boundary = grid_origin[axis] + f32(coord[axis]) * cell_size;
            t_max[axis] = (boundary - ray_origin[axis]) / direction;
            t_delta[axis] = -cell_size / direction;
        }
    }
    return DdaState(coord, step, t_max, t_delta, entry_t, entry_normal);
}

fn dda_axis(state: DdaState) -> u32 {
    if state.t_max.x <= state.t_max.y && state.t_max.x <= state.t_max.z {
        return 0u;
    }
    if state.t_max.y <= state.t_max.z {
        return 1u;
    }
    return 2u;
}

fn dda_advance(state_in: DdaState) -> DdaState {
    var state = state_in;
    let axis = dda_axis(state);
    state.t = state.t_max[axis];
    state.coord[axis] += state.step[axis];
    state.t_max[axis] += state.t_delta[axis];
    state.normal = axis_normal(axis, -state.step[axis]);
    return state;
}

fn in_bounds(coord: vec3<i32>, dimension: i32) -> bool {
    return all(coord >= vec3<i32>(0)) && all(coord < vec3<i32>(dimension));
}

fn local_index(local: vec3<i32>) -> u32 {
    return u32(local.x + BRICK_EDGE * local.y + BRICK_EDGE * BRICK_EDGE * local.z);
}

fn root_index(brick: vec3<i32>) -> u32 {
    return u32(brick.x + BRICKS_PER_AXIS * brick.y
        + BRICKS_PER_AXIS * BRICKS_PER_AXIS * brick.z);
}

fn mixed_material(page: u32, slot: u32, local: u32) -> u32 {
    var occupancy_word = 0u;
    var material_word = 0u;
    if page == 0u {
        occupancy_word = mixed_page_0[slot].occupancy[local / 32u];
        material_word = mixed_page_0[slot].materials[local / 2u];
    } else {
        occupancy_word = mixed_page_1[slot].occupancy[local / 32u];
        material_word = mixed_page_1[slot].materials[local / 2u];
    }
    if (occupancy_word & (1u << (local % 32u))) == 0u {
        return 0u;
    }
    return (material_word >> ((local % 2u) * 16u)) & 0xffffu;
}

fn material_at(cell_slot: u32, voxel: vec3<i32>) -> u32 {
    if !in_bounds(voxel, CELL_EDGE) {
        return 0u;
    }
    let cell = cells[cell_slot];
    let brick = voxel / BRICK_EDGE;
    let entry = roots[cell.root_handle].entries[root_index(brick)];
    let kind = entry >> 30u;
    if kind == RootEntryKind_EMPTY {
        return 0u;
    }
    if kind == RootEntryKind_UNIFORM {
        return entry & 0xffffu;
    }
    let local = voxel - brick * BRICK_EDGE;
    let page = select(1u, 0u, kind == RootEntryKind_PAGE_0);
    return mixed_material(page, entry & ROOT_PAYLOAD_MASK, local_index(local));
}

struct TraceResult {
    valid: bool,
    t: f32,
    voxel: vec3<i32>,
    normal: vec3<i32>,
    material: u32,
}

fn fallback_inside_normal(normal: vec3<i32>, direction: vec3<f32>) -> vec3<i32> {
    if any(normal != vec3<i32>(0)) {
        return normal;
    }
    let magnitude = abs(direction);
    var axis = 0u;
    if magnitude.y > magnitude.x && magnitude.y >= magnitude.z {
        axis = 1u;
    } else if magnitude.z > magnitude.x && magnitude.z > magnitude.y {
        axis = 2u;
    }
    return axis_normal(axis, select(1, -1, direction[axis] >= 0.0));
}

fn trace_brick(
    cell_slot: u32,
    brick: vec3<i32>,
    ray_origin: vec3<f32>,
    ray_direction: vec3<f32>,
    entry_t: f32,
    exit_t: f32,
    entry_normal: vec3<i32>,
) -> TraceResult {
    let cell = cells[cell_slot];
    var voxels = dda_new(
        ray_origin,
        ray_direction,
        cell.origin,
        f32(cell.factor),
        CELL_EDGE,
        entry_t,
        entry_normal,
    );
    // A ray crosses at most 3 * 8 - 2 voxels in one 8^3 brick.
    for (var iteration = 0u; iteration < 24u; iteration += 1u) {
        if !in_bounds(voxels.coord, CELL_EDGE)
            || any(voxels.coord / BRICK_EDGE != brick)
            || voxels.t > exit_t + DDA_EPSILON {
            break;
        }
        let material = material_at(cell_slot, voxels.coord);
        if material != 0u {
            return TraceResult(
                true,
                voxels.t,
                voxels.coord,
                fallback_inside_normal(voxels.normal, ray_direction),
                material,
            );
        }
        let axis = dda_axis(voxels);
        if voxels.t_max[axis] > exit_t + DDA_EPSILON {
            break;
        }
        voxels = dda_advance(voxels);
    }
    return TraceResult(false, 0.0, vec3<i32>(0), vec3<i32>(0), 0u);
}

fn trace_cell(
    cell_slot: u32,
    ray_origin: vec3<f32>,
    ray_direction: vec3<f32>,
    t_limit: f32,
) -> TraceResult {
    let cell = cells[cell_slot];
    let maximum = cell.origin + vec3(32.0 * f32(cell.factor));
    let slab = intersect_cell_aabb(ray_origin, ray_direction, cell.origin, maximum);
    if !slab.valid || slab.entry_t > t_limit {
        return TraceResult(false, 0.0, vec3<i32>(0), vec3<i32>(0), 0u);
    }
    let exit_t = min(slab.exit_t, t_limit);
    var bricks = dda_new(
        ray_origin,
        ray_direction,
        cell.origin,
        8.0 * f32(cell.factor),
        BRICKS_PER_AXIS,
        slab.entry_t,
        slab.entry_normal,
    );
    // A ray crosses at most 3 * 4 - 2 cells in the 4^3 brick grid.
    for (var iteration = 0u; iteration < 12u; iteration += 1u) {
        if !in_bounds(bricks.coord, BRICKS_PER_AXIS)
            || bricks.t > exit_t + DDA_EPSILON {
            break;
        }
        let entry = roots[cell.root_handle].entries[root_index(bricks.coord)];
        let axis = dda_axis(bricks);
        let brick_exit = min(bricks.t_max[axis], exit_t);
        if (entry >> 30u) != RootEntryKind_EMPTY {
            let hit = trace_brick(
                cell_slot,
                bricks.coord,
                ray_origin,
                ray_direction,
                bricks.t,
                brick_exit,
                bricks.normal,
            );
            if hit.valid {
                return hit;
            }
        }
        if bricks.t_max[axis] > exit_t + DDA_EPSILON {
            break;
        }
        bricks = dda_advance(bricks);
    }
    return TraceResult(false, 0.0, vec3<i32>(0), vec3<i32>(0), 0u);
}

fn floor_div_32(value: i32) -> i32 {
    return select(value / 32, (value - 31) / 32, value < 0);
}

fn sample_same_factor(cell_slot: u32, voxel: vec3<i32>) -> u32 {
    if in_bounds(voxel, CELL_EDGE) {
        return material_at(cell_slot, voxel);
    }
    let offset = vec3<i32>(
        floor_div_32(voxel.x),
        floor_div_32(voxel.y),
        floor_div_32(voxel.z),
    );
    if any(abs(offset) > vec3<i32>(1)) {
        return 0u;
    }
    let linear = u32(offset.x + 1 + 3 * (offset.y + 1) + 9 * (offset.z + 1));
    if linear == 13u {
        return 0u;
    }
    let neighbor_index = linear - select(0u, 1u, linear > 13u);
    let source = cells[cell_slot];
    let neighbor_slot = source.neighbor_slots[neighbor_index];
    if neighbor_slot == INVALID_FAR_SLOT || neighbor_slot >= arrayLength(&cells) {
        return 0u;
    }
    let neighbor = cells[neighbor_slot];
    let expected_origin = source.origin
        + vec3<f32>(offset) * (32.0 * f32(source.factor));
    if neighbor.is_active == 0u || neighbor.factor != source.factor
        || any(abs(neighbor.origin - expected_origin) > vec3(0.001 * f32(source.factor))) {
        return 0u;
    }
    let local = voxel - offset * CELL_EDGE;
    return material_at(neighbor_slot, local);
}

fn vertex_ao(side_u: bool, side_v: bool, corner: bool) -> u32 {
    if side_u && side_v {
        return 0u;
    }
    return 3u - u32(side_u) - u32(side_v) - u32(corner);
}

fn normal_axis(normal: vec3<i32>) -> u32 {
    if normal.x != 0 {
        return 0u;
    }
    if normal.y != 0 {
        return 1u;
    }
    return 2u;
}

fn corner_ao_levels(
    cell_slot: u32,
    voxel: vec3<i32>,
    normal: vec3<i32>,
) -> vec4<u32> {
    let axis = normal_axis(normal);
    var u_axis = 0u;
    var v_axis = 1u;
    if axis == 0u {
        u_axis = 1u;
        v_axis = 2u;
    } else if axis == 1u {
        u_axis = 0u;
        v_axis = 2u;
    }
    let layer = voxel + normal;
    var levels = vec4<u32>(0u);
    for (var corner_index = 0u; corner_index < 4u; corner_index += 1u) {
        let du = select(-1, 1, (corner_index & 1u) != 0u);
        let dv = select(-1, 1, (corner_index & 2u) != 0u);
        var side_u = layer;
        var side_v = layer;
        var diagonal = layer;
        side_u[u_axis] += du;
        side_v[v_axis] += dv;
        diagonal[u_axis] += du;
        diagonal[v_axis] += dv;
        levels[corner_index] = vertex_ao(
            sample_same_factor(cell_slot, side_u) != 0u,
            sample_same_factor(cell_slot, side_v) != 0u,
            sample_same_factor(cell_slot, diagonal) != 0u,
        );
    }
    return levels;
}

fn ao_shade(levels: vec4<u32>, local_position: vec3<f32>, normal: vec3<i32>) -> f32 {
    let axis = normal_axis(normal);
    var uv = vec2(local_position.x, local_position.y);
    if axis == 0u {
        uv = vec2(local_position.y, local_position.z);
    } else if axis == 1u {
        uv = vec2(local_position.x, local_position.z);
    }
    uv = fract(uv);
    let a = mix(AO_CURVE[levels.x], AO_CURVE[levels.y], uv.x);
    let b = mix(AO_CURVE[levels.z], AO_CURVE[levels.w], uv.x);
    return mix(a, b, uv.y);
}

// Same fade as voxel_pull.wgsl: baked AO gone at one block per cell.
const AO_FADE_LOG2: f32 = 4.0;
// Hemisphere ambient and distant-riser fade — locked to voxel_pull.wgsl.
const SKY_SIDE: f32 = 0.75;
const RISER_FADE_START_PX: f32 = 24.0;
const RISER_FADE_END_PX: f32 = 6.0;

// Bevy's distance fog without directional in-scattering — copy of
// voxel_pull.wgsl's apply_distance_fog.
fn apply_distance_fog(color: vec3<f32>, world_position: vec3<f32>) -> vec3<f32> {
#ifdef DISTANCE_FOG
    let fog = view_bindings::fog;
    let distance = length(world_position - view_bindings::view.world_position);
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

fn face_uv(local_position: vec3<f32>, normal: vec3<i32>) -> vec2<f32> {
    let axis = normal_axis(normal);
    if axis == 0u {
        return vec2(local_position.z, -local_position.y) / 16.0;
    }
    if axis == 1u {
        return vec2(local_position.x, local_position.z) / 16.0;
    }
    return vec2(local_position.x, -local_position.y) / 16.0;
}

fn material_layer(material: u32, normal: vec3<i32>) -> u32 {
    let texel = textureLoad(
        material_layer_lut,
        vec2<i32>(i32(material & 255u), i32(material >> 8u)),
        0,
    );
    if normal.y > 0 {
        return texel.x;
    }
    if normal.y < 0 {
        return texel.z;
    }
    return texel.y;
}

struct FragmentOutput {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

@fragment
fn fragment(
    in: ProxyVertexOutput,
    @builtin(sample_index) sample_index: u32,
) -> FragmentOutput {
    let cell = cells[in.cell_slot];
    let ray_origin = view_bindings::view.world_position;
    let ray_direction = normalize(in.proxy_world - ray_origin);
    var t_limit = 1e30;

#ifdef DEPTH_PREPASS
#ifdef MULTISAMPLED
    let near_depth = textureLoad(
        view_bindings::depth_prepass_texture,
        vec2<i32>(in.clip_position.xy),
        i32(sample_index),
    );
#else
    let near_depth = textureLoad(
        view_bindings::depth_prepass_texture,
        vec2<i32>(in.clip_position.xy),
        0,
    );
#endif
    if near_depth > 0.0 {
        var near_ndc = frag_coord_to_ndc(in.clip_position);
        near_ndc.z = near_depth;
        let near_world = position_ndc_to_world(near_ndc);
        let near_t = dot(near_world - ray_origin, ray_direction);
        if near_t >= 0.0 {
            t_limit = near_t + DDA_EPSILON * f32(cell.factor);
        }
    }
#endif

    let hit = trace_cell(in.cell_slot, ray_origin, ray_direction, t_limit);
    if !hit.valid {
        discard;
    }
    let world_position = ray_origin + ray_direction * hit.t;
    let local_position = (world_position - cell.origin) / f32(cell.factor);
    let levels = corner_ao_levels(in.cell_slot, hit.voxel, hit.normal);
    // AO fades out as cells coarsen, and texture coordinates are in WORLD
    // voxels so one repeat is one block at every factor — both locked to the
    // raster path's rule (voxel_pull.wgsl).
    let cell_scale = f32(cell.factor);
    let ao_strength = clamp(1.0 - log2(cell_scale) / AO_FADE_LOG2, 0.0, 1.0);
    let shade = mix(1.0, ao_shade(levels, local_position, hit.normal), ao_strength);
    let layer = material_layer(hit.material, hit.normal);
    let tex_position = local_position * cell_scale + (cell.origin - 16.0 * floor(cell.origin / 16.0));
    let uv = face_uv(tex_position, hit.normal);
    var tex = textureSample(block_texture, block_sampler, uv, i32(layer));
    // Sampled unconditionally: implicit-derivative sampling must stay in
    // uniform control flow. Equals `tex` on top faces.
    let top_tex = textureSample(
        block_texture, block_sampler, uv, i32(material_layer(hit.material, vec3<i32>(0, 1, 0))));
    let normal = normalize(vec3<f32>(hit.normal));
    let block_px = 16.0 * view_bindings::view.clip_from_view[1][1] * view_bindings::view.viewport.w
        / (2.0 * max(hit.t, 1e-3));
    let riser_fade = (1.0 - abs(normal.y))
        * (1.0 - smoothstep(RISER_FADE_END_PX, RISER_FADE_START_PX, block_px));
    tex = mix(tex, top_tex, riser_fade);
    let light_n = normalize(mix(normal, vec3<f32>(0.0, 1.0, 0.0), riser_fade));

    var lit = view_bindings::lights.ambient_color.rgb * (SKY_SIDE + (1.0 - SKY_SIDE) * light_n.y);
    for (var i = 0u; i < view_bindings::lights.n_directional_lights; i += 1u) {
        let light = view_bindings::lights.directional_lights[i];
        let ndotl = max(dot(light_n, light.direction_to_light), 0.0);
        lit += light.color.rgb * ndotl / PI;
    }
    lit *= view_bindings::view.exposure;

    let depth_position =
        world_position + ray_direction * (FAR_DEPTH_PUSH_MIN + FAR_DEPTH_PUSH_REL * hit.t);
    let clip_position = position_world_to_clip(depth_position);
    var out: FragmentOutput;
    out.color = vec4(apply_distance_fog(tex.rgb * shade * lit, world_position), 1.0);
    out.depth = clip_position.z / clip_position.w;
    return out;
}

#endif // FAR_CULL
