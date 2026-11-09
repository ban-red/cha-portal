// Draws a picture into a rectangle of the target, stretched by the GPU's bilinear
// filter or by SGSR 1 (Snapdragon Game Super Resolution 1): a 12-tap Lanczos-like
// filter with adaptive sharpening, in one pass from one texture.
//
// SGSR 1 is ported from SnapdragonStudios/snapdragon-gsr at d926f07,
// sgsr/v1/include/glsl/sgsr1_shader_mobile.frag (RGBA mode, operation mode 1),
// which carries this notice:
//
//   Copyright (c) 2025, Qualcomm Innovation Center, Inc. All rights reserved.
//   SPDX-License-Identifier: BSD-3-Clause
//
// The licence's text is in LICENSE-SGSR beside this file. What changed: GLSL to
// WGSL; the configuration (edge threshold and sharpness) comes from a uniform
// instead of #defines; the picture is placed in a destination rectangle of the
// target (the caller's letterbox) instead of covering it; mode 0 is the
// bilinear sample alone, so one pipeline draws both; and the result can be
// converted to linear for an sRGB target. The filter itself is unchanged.
//
// This file is vendored byte for byte into
// web/packages/pyrowave-webgpu/src/shaders/sgsr1.wgsl; a test in cha-upscale
// keeps the two the same.

struct Params {
    // xy: one texel in uv units (1 / the source's size); zw: the source's size.
    // SGSR's ViewportInfo.
    viewport_info: vec4<f32>,
    // xy: the destination rectangle's top left in target pixels; zw: its size.
    dest: vec4<f32>,
    // x: 1 for SGSR, 0 for the bilinear sample alone; y: 1 to convert the result
    // to linear (an sRGB target); z: edge sharpness (1 to 2); w: edge threshold.
    mode: vec4<f32>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle covers the viewport.
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

fn fast_lanczos2(x: f32) -> f32 {
    var wa = x - 4.0;
    let wb = x * wa - wa;
    wa = wa * wa;
    return wb * wa;
}

// `spread` is SGSR's `std` (a reserved word in WGSL).
fn weight_y(dx: f32, dy: f32, c: f32, spread: f32) -> vec2<f32> {
    let x = ((dx * dx) + (dy * dy)) * 0.55 + clamp(abs(c) * spread, 0.0, 1.0);
    let w = fast_lanczos2(x);
    return vec2<f32>(w, w * c);
}

// SGSR 1 at `uv`, from the bilinear sample there. The edge test and the taps use
// the green channel (RGBA mode); the correction goes to all three.
fn sgsr(uv: vec2<f32>, bilinear: vec3<f32>) -> vec3<f32> {
    let texel = p.viewport_info.xy;
    let img_coord = uv * p.viewport_info.zw + vec2<f32>(-0.5, 0.5);
    let img_coord_pixel = floor(img_coord);
    var coord = img_coord_pixel * texel;
    let pl = img_coord - img_coord_pixel;
    var left = textureGather(1, src, samp, coord);

    let g = bilinear.y;
    let edge_vote = abs(left.z - left.y) + abs(g - left.y) + abs(g - left.z);
    if (edge_vote <= p.mode.w) {
        return bilinear;
    }

    coord.x += texel.x;
    var right = textureGather(1, src, samp, coord + vec2<f32>(texel.x, 0.0));
    var up_down = vec4<f32>(
        textureGather(1, src, samp, coord + vec2<f32>(0.0, -texel.y)).wz,
        textureGather(1, src, samp, coord + vec2<f32>(0.0, texel.y)).yx,
    );

    let mean = (left.y + left.z + right.x + right.w) * 0.25;
    left = left - vec4<f32>(mean);
    right = right - vec4<f32>(mean);
    up_down = up_down - vec4<f32>(mean);
    let center = g - mean;

    let sum = (((((abs(left.x) + abs(left.y)) + abs(left.z)) + abs(left.w))
        + (((abs(right.x) + abs(right.y)) + abs(right.z)) + abs(right.w)))
        + (((abs(up_down.x) + abs(up_down.y)) + abs(up_down.z)) + abs(up_down.w)));
    let spread = 2.181818 / sum;

    var awy = weight_y(pl.x, pl.y + 1.0, up_down.x, spread);
    awy += weight_y(pl.x - 1.0, pl.y + 1.0, up_down.y, spread);
    awy += weight_y(pl.x - 1.0, pl.y - 2.0, up_down.z, spread);
    awy += weight_y(pl.x, pl.y - 2.0, up_down.w, spread);
    awy += weight_y(pl.x + 1.0, pl.y - 1.0, left.x, spread);
    awy += weight_y(pl.x, pl.y - 1.0, left.y, spread);
    awy += weight_y(pl.x, pl.y, left.z, spread);
    awy += weight_y(pl.x + 1.0, pl.y, left.w, spread);
    awy += weight_y(pl.x - 1.0, pl.y - 1.0, right.x, spread);
    awy += weight_y(pl.x - 2.0, pl.y - 1.0, right.y, spread);
    awy += weight_y(pl.x - 2.0, pl.y, right.z, spread);
    awy += weight_y(pl.x - 1.0, pl.y, right.w, spread);

    var final_y = awy.y / awy.x;
    let max_y = max(max(left.y, left.z), max(right.x, right.w));
    let min_y = min(min(left.y, left.z), min(right.x, right.w));
    final_y = clamp(p.mode.z * final_y, min_y, max_y);

    var delta_y = final_y - center;
    // Smooth high contrast input.
    delta_y = clamp(delta_y, -23.0 / 255.0, 23.0 / 255.0);
    return clamp(bilinear + vec3<f32>(delta_y), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = (pos.xy - p.dest.xy) / p.dest.zw;
    var color = textureSampleLevel(src, samp, uv, 0.0).xyz;
    if (p.mode.x > 0.5) {
        color = sgsr(uv, color);
    }
    if (p.mode.y > 0.5) {
        color = srgb_to_linear(color);
    }
    return vec4<f32>(color, 1.0);
}
