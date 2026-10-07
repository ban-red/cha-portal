// Draws decoded PyroWave planes straight from the decoder's storage buffer:
// YCbCr to RGB in a fragment shader, no copies or readback. Port of the shader in
// web/packages/pyrowave-webgpu/src/render.ts, with the video placed in a viewport
// rectangle instead of covering the whole target.

struct Params {
    video_size: vec2<u32>,
    chroma420: u32,
    full_range: u32,
    viewport: vec4<f32>, // x, y, width, height in target pixels
    bt2020: u32,
    to_linear: u32,
    pad0: u32,
    pad1: u32,
    offsets: vec4<u32>,
    strides: vec4<u32>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> planes: array<u32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

fn sample_plane(plane: u32, x: u32, y: u32) -> f32 {
    let word = planes[p.offsets[plane] + y * p.strides[plane] + (x >> 2u)];
    return f32((word >> ((x & 3u) * 8u)) & 0xffu);
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = (pos.xy - p.viewport.xy) / p.viewport.zw;
    let px = min(vec2<u32>(max(uv, vec2<f32>(0.0)) * vec2<f32>(p.video_size)), p.video_size - vec2<u32>(1u));
    var c = px;
    if (p.chroma420 == 1u) { c = px / 2u; }
    var y = sample_plane(0u, px.x, px.y);
    var cb = sample_plane(1u, c.x, c.y) - 128.0;
    var cr = sample_plane(2u, c.x, c.y) - 128.0;
    if (p.full_range == 1u) {
        y = y / 255.0; cb = cb / 255.0; cr = cr / 255.0;
    } else {
        y = (y - 16.0) / 219.0; cb = cb / 224.0; cr = cr / 224.0;
    }
    var rgb: vec3<f32>;
    if (p.bt2020 == 1u) {
        rgb = vec3<f32>(y + 1.4746 * cr, y - 0.16455 * cb - 0.57135 * cr, y + 1.8814 * cb);
    } else {
        rgb = vec3<f32>(y + 1.5748 * cr, y - 0.1873 * cb - 0.4681 * cr, y + 1.8556 * cb);
    }
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    if (p.to_linear == 1u) { rgb = srgb_to_linear(rgb); }
    return vec4<f32>(rgb, 1.0);
}
