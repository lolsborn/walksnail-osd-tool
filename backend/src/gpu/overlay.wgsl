// Draws textured quads (OSD glyphs from the font atlas, or the SRT text strip) onto the frame
// texture with standard "over" alpha blending. One instance per quad, 6 vertices per instance.

struct Uniforms {
    viewport: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var quad_texture: texture_2d<f32>;
@group(1) @binding(1) var quad_sampler: sampler;

struct Instance {
    @location(0) pos: vec2<f32>,    // top-left corner in pixels
    @location(1) size: vec2<f32>,   // size in pixels
    @location(2) uv_min: vec2<f32>,
    @location(3) uv_max: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32, instance: Instance) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vertex_index];
    let pixel = instance.pos + corner * instance.size;
    let ndc = vec2<f32>(
        pixel.x / uniforms.viewport.x * 2.0 - 1.0,
        1.0 - pixel.y / uniforms.viewport.y * 2.0,
    );
    var out: VertexOutput;
    out.clip_position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = instance.uv_min + corner * (instance.uv_max - instance.uv_min);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(quad_texture, quad_sampler, in.uv);
}
