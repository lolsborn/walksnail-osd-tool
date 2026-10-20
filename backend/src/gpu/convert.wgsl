// Color conversion passes between NV12 planes and the RGBA frame texture.
// BT.709 limited range ("tv"), the matrix used by HD video. Video pixels go YUV -> RGB -> YUV and
// land back where they started up to rounding; the matrix only affects the colors of the overlay.

@group(0) @binding(0) var y_texture: texture_2d<f32>;    // R8: luma, full resolution
@group(0) @binding(1) var uv_texture: texture_2d<f32>;   // RG8: chroma, half resolution
@group(0) @binding(2) var rgb_texture: texture_2d<f32>;  // RGBA8: the composited frame

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
};

// One triangle covering the whole viewport
@vertex
fn fullscreen_vs(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0),
    );
    var out: VertexOutput;
    out.clip_position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return out;
}

fn yuv_to_rgb(y: f32, u: f32, v: f32) -> vec3<f32> {
    let yy = (y - 16.0 / 255.0) * (255.0 / 219.0);
    let cb = (u - 128.0 / 255.0) * (255.0 / 224.0);
    let cr = (v - 128.0 / 255.0) * (255.0 / 224.0);
    return clamp(vec3<f32>(
        yy + 1.5748 * cr,
        yy - 0.1873 * cb - 0.4681 * cr,
        yy + 1.8556 * cb,
    ), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn rgb_to_y(rgb: vec3<f32>) -> f32 {
    let y = 0.2126 * rgb.r + 0.7152 * rgb.g + 0.0722 * rgb.b;
    return 16.0 / 255.0 + y * (219.0 / 255.0);
}

fn rgb_to_uv(rgb: vec3<f32>) -> vec2<f32> {
    let y = 0.2126 * rgb.r + 0.7152 * rgb.g + 0.0722 * rgb.b;
    let cb = (rgb.b - y) / 1.8556;
    let cr = (rgb.r - y) / 1.5748;
    return vec2<f32>(128.0 / 255.0, 128.0 / 255.0) + vec2<f32>(cb, cr) * (224.0 / 255.0);
}

// NV12 -> RGBA frame. Chroma is replicated per 2x2 block (nearest), like ffmpeg's unscaled
// yuv420p -> rgba conversion.
@fragment
fn nv12_to_rgb_fs(in: VertexOutput) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(in.clip_position.xy);
    let y = textureLoad(y_texture, pixel, 0).r;
    let uv = textureLoad(uv_texture, pixel / 2, 0).rg;
    return vec4<f32>(yuv_to_rgb(y, uv.r, uv.g), 1.0);
}

// RGBA frame -> Y plane
@fragment
fn rgb_to_y_fs(in: VertexOutput) -> @location(0) f32 {
    let pixel = vec2<i32>(in.clip_position.xy);
    return rgb_to_y(textureLoad(rgb_texture, pixel, 0).rgb);
}

// RGBA frame -> UV plane. Each chroma sample is the average of its 2x2 block of pixels.
@fragment
fn rgb_to_uv_fs(in: VertexOutput) -> @location(0) vec2<f32> {
    let base = vec2<i32>(in.clip_position.xy) * 2;
    let rgb = (textureLoad(rgb_texture, base, 0).rgb
        + textureLoad(rgb_texture, base + vec2<i32>(1, 0), 0).rgb
        + textureLoad(rgb_texture, base + vec2<i32>(0, 1), 0).rgb
        + textureLoad(rgb_texture, base + vec2<i32>(1, 1), 0).rgb) * 0.25;
    return rgb_to_uv(rgb);
}
