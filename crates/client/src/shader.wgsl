@group(0) @binding(0)
var<uniform> view_proj: mat4x4<f32>;
@group(0) @binding(1)
var<uniform> view_matrix: mat4x4<f32>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) tex_coord: vec2<f32>,
    @location(2) color: vec3<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec3<f32>,
}

@vertex
fn vs_main(
    model: VertexInput,
) -> VertexOutput {
    var out: VertexOutput;
    out.color = model.color;
    out.clip_position = view_proj * view_matrix * vec4<f32>(model.position, 1.0);
    out.tex_coord = model.tex_coord;
    return out;
}

@group(1) @binding(0)
var texture_array_top: binding_array<texture_2d<f32>>;
@group(1) @binding(1)
var sampler_array: binding_array<sampler>;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return
        textureSampleLevel(
        texture_array_top[0],
        sampler_array[0],
        in.tex_coord,
        0.0
    ).rgba;
}


