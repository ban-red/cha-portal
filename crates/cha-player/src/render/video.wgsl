// A bi-planar YCbCr picture (luma R, chroma RG at half size) drawn as RGB over
// the whole viewport; the caller sets the viewport to the letterboxed rect.

struct Params {
    row0: vec4<f32>,
    row1: vec4<f32>,
    row2: vec4<f32>,
    // xyz: the offset subtracted from the samples; w: raw sample scale.
    offset: vec4<f32>,
}

@group(0) @binding(0) var luma: texture_2d<f32>;
@group(0) @binding(1) var chroma: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> params: Params;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs(@builtin(vertex_index) index: u32) -> Out {
    // One oversized triangle covers the viewport.
    let p = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: Out;
    out.position = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(p.x, 1.0 - p.y);
    return out;
}

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    let y = textureSample(luma, samp, in.uv).r;
    let c = textureSample(chroma, samp, in.uv).rg;
    let yuv = vec3<f32>(y, c.x, c.y) * params.offset.w - params.offset.xyz;
    let rgb = vec3<f32>(dot(params.row0.xyz, yuv), dot(params.row1.xyz, yuv), dot(params.row2.xyz, yuv));
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
