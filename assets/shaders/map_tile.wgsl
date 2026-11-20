// The world map's tiles (crates/client/src/renderer/map/pass.rs; compiled in, no hot reload).
// One quad per tile, scissored to its view's rect, drawn under the Slint layers.
// `Quad` and group(1) are LOCKED to pass.rs (`QuadUniform`, `MapPipeline`).

struct Quad {
    // Screen rect (physical px): min x, min y, max x, max y.
    rect: vec4<f32>,
    // The tile's sub-rect shown, in samples.
    uv: vec4<f32>,
    // Target size (px), then the drawn tile's min block (x, z).
    target_origin: vec4<f32>,
    // Metres per sample of the tile, metres per pixel of the view, layer (-1 background, 0 surface,
    // 1+ theme), region blocks.
    params: vec4<f32>,
    // The theme raster's frame: origin region x, z, width, height.
    theme_frame: vec4<f32>,
}

@group(0) @binding(0) var<uniform> quad: Quad;
@group(1) @binding(0) var height_tex: texture_2d<i32>;
@group(1) @binding(1) var block_tex: texture_2d<u32>;
@group(1) @binding(2) var water_flags_tex: texture_2d<u32>;
@group(1) @binding(3) var palette_tex: texture_2d<f32>;
@group(1) @binding(4) var theme_tex: texture_2d<f32>;

// `game::map_tile::sample_flags`.
const ICE: u32 = 1u;
const OUTSIDE: u32 = 4u;
const EDGE: i32 = 256; // MAP_TILE_SAMPLES

// Linear RGB. The background (`MAP_BACKGROUND_RGB`), deep water (`tile_map::sea_color` at 12 m, in
// sRGB 56 132 195) and ice.
const BACKGROUND: vec3<f32> = vec3<f32>(0.09, 0.10, 0.12);
const DEEP_WATER: vec3<f32> = vec3<f32>(0.040, 0.231, 0.546);
const SHALLOW_WATER: vec3<f32> = vec3<f32>(0.156, 0.514, 0.680);
const ICE_RGB: vec3<f32> = vec3<f32>(0.80, 0.88, 0.95);
// Deeper than this the water is the deep colour.
const DEEP_M: f32 = 12.0;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> VertexOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let c = corners[index];
    let px = mix(quad.rect.xy, quad.rect.zw, c);
    let ndc = px / quad.target_origin.xy * 2.0 - 1.0;
    var out: VertexOut;
    out.position = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.uv = mix(quad.uv.xy, quad.uv.zw, c);
    return out;
}

fn clamp_texel(t: vec2<i32>) -> vec2<i32> {
    return clamp(t, vec2<i32>(0), vec2<i32>(EDGE - 1));
}

fn height_at(t: vec2<i32>) -> f32 {
    return f32(textureLoad(height_tex, clamp_texel(t), 0).r);
}

// Hill shade of texel `t`: the sun from the north-west, 1 on flat ground.
fn shade(t: vec2<i32>) -> f32 {
    let s = quad.params.x;
    let dx = height_at(t + vec2<i32>(1, 0)) - height_at(t - vec2<i32>(1, 0));
    let dz = height_at(t + vec2<i32>(0, 1)) - height_at(t - vec2<i32>(0, 1));
    let n = normalize(vec3<f32>(-dx, 2.0 * s, -dz));
    let sun = normalize(vec3<f32>(-1.0, 1.4, -1.0));
    return clamp(dot(n, sun) / sun.y, 0.45, 1.35);
}

// One sample's colour.
fn texel_colour(t0: vec2<i32>) -> vec3<f32> {
    let t = clamp_texel(t0);
    let wf = textureLoad(water_flags_tex, t, 0).rg;
    let flags = wf.g;
    if (flags & OUTSIDE) != 0u {
        return BACKGROUND;
    }
    var c: vec3<f32>;
    let layer = quad.params.z;
    if layer >= 0.5 {
        // A thematic layer: the world map's tile under this sample, by world position.
        let rb = quad.params.w;
        let pos = quad.target_origin.zw + (vec2<f32>(t) + 0.5) * quad.params.x;
        let frame = quad.theme_frame;
        var r = vec2<i32>(floor(pos / rb)) - vec2<i32>(frame.xy);
        let w = i32(frame.z);
        if w > 0 {
            r.x = ((r.x % w) + w) % w;
        }
        if r.y < 0 || r.y >= i32(frame.w) {
            return BACKGROUND;
        }
        c = textureLoad(theme_tex, r, 0).rgb * shade(t);
        return c;
    }
    let block = textureLoad(block_tex, t, 0).r;
    c = textureLoad(palette_tex, vec2<i32>(i32(block), 0), 0).rgb * shade(t);
    let water = f32(wf.r);
    if water > 0.0 {
        let depth = clamp(water / DEEP_M, 0.0, 1.0);
        let tint = mix(SHALLOW_WATER, DEEP_WATER, depth);
        c = mix(c, tint, 0.45 + 0.55 * sqrt(depth));
        if (flags & ICE) != 0u {
            c = mix(c, ICE_RGB, 0.75);
        }
    }
    return c;
}

@fragment
fn fragment(in: VertexOut) -> @location(0) vec4<f32> {
    if quad.params.z < -0.5 {
        return vec4<f32>(BACKGROUND, 1.0);
    }
    let base = vec2<i32>(floor(in.uv));
    // Nearest while a sample covers at least a pixel; a 2×2 box once it is smaller.
    if quad.params.y <= quad.params.x {
        return vec4<f32>(texel_colour(base), 1.0);
    }
    let b = vec2<i32>(floor(in.uv - 0.5));
    let c = texel_colour(b)
        + texel_colour(b + vec2<i32>(1, 0))
        + texel_colour(b + vec2<i32>(0, 1))
        + texel_colour(b + vec2<i32>(1, 1));
    return vec4<f32>(c * 0.25, 1.0);
}
