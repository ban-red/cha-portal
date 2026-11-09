// HDR10 to the screen (docs/plans/hdr-and-vrr.md, step 7.1): the functions a
// renderer needs to show a BT.2020 / PQ picture on an extended-range surface,
// written once. Cha Player draws with this file and the browser's PyroWave
// path with its copy (web/packages/pyrowave-webgpu/src/shaders/hdr.wgsl); the
// TypeScript reference with its tests is web/packages/player/src/hdr.ts.
//
// - pq_to_nits / nits_to_pq: SMPTE ST 2084 (BT.2100's PQ), in cd/m².
// - bt2020_to_bt709: linear BT.2020 to linear BT.709 (BT.2087), negatives
//   kept: an extended-range surface shows the wider gamut through them.
// - eetf: BT.2390's roll-off, from a source peak to a display peak, in PQ.
// - hdr10_to_display: a PQ pixel to linear BT.709, 1.0 at the reference
//   white (BT.2408's 203 cd/m² by default), highlights rolled off to the
//   display's peak by their largest channel, so hues hold.
// - srgb_encode: the sRGB curve, mirrored for negatives, for float canvases
//   whose values are sRGB-encoded.

const PQ_M1: f32 = 0.1593017578125;
const PQ_M2: f32 = 78.84375;
const PQ_C1: f32 = 0.8359375;
const PQ_C2: f32 = 18.8515625;
const PQ_C3: f32 = 18.6875;

fn pq_to_nits(e: f32) -> f32 {
    let p = pow(clamp(e, 0.0, 1.0), 1.0 / PQ_M2);
    let y = pow(max(p - PQ_C1, 0.0) / (PQ_C2 - PQ_C3 * p), 1.0 / PQ_M1);
    return 10000.0 * y;
}

fn nits_to_pq(nits: f32) -> f32 {
    let y = pow(clamp(nits / 10000.0, 0.0, 1.0), PQ_M1);
    return pow((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y), PQ_M2);
}

fn bt2020_to_bt709(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(vec3<f32>(1.660491, -0.587641, -0.072850), rgb),
        dot(vec3<f32>(-0.124550, 1.132900, -0.008349), rgb),
        dot(vec3<f32>(-0.018151, -0.100579, 1.118730), rgb),
    );
}

// BT.2390's EETF with the blacks at zero: below the knee a value passes
// unchanged, above it a Hermite spline takes the source's peak to the
// display's. A display at least as bright as the source changes nothing.
fn eetf(nits: f32, src_peak: f32, dst_peak: f32) -> f32 {
    if (dst_peak >= src_peak) {
        return min(nits, src_peak);
    }
    let lw = nits_to_pq(src_peak);
    let e1 = min(nits_to_pq(nits) / lw, 1.0);
    let max_lum = nits_to_pq(dst_peak) / lw;
    let ks = max(1.5 * max_lum - 0.5, 0.0);
    var e2 = e1;
    if (e1 > ks) {
        let t = (e1 - ks) / (1.0 - ks);
        let t2 = t * t;
        let t3 = t2 * t;
        e2 = (2.0 * t3 - 3.0 * t2 + 1.0) * ks + (t3 - 2.0 * t2 + t) * (1.0 - ks)
            + (-2.0 * t3 + 3.0 * t2) * max_lum;
    }
    return pq_to_nits(min(e2, max_lum) * lw);
}

fn hdr10_to_display(pq: vec3<f32>, ref_white: f32, src_peak: f32, dst_peak: f32) -> vec3<f32> {
    let nits = vec3<f32>(pq_to_nits(pq.r), pq_to_nits(pq.g), pq_to_nits(pq.b));
    let rgb = bt2020_to_bt709(nits);
    let m = max(max(rgb.r, rgb.g), rgb.b);
    var scale = 1.0;
    if (m > 0.0) {
        scale = eetf(m, src_peak, dst_peak) / m;
    }
    return rgb * (scale / ref_white);
}

fn srgb_encode(x: f32) -> f32 {
    let a = abs(x);
    var y = 12.92 * a;
    if (a > 0.0031308) {
        y = 1.055 * pow(a, 1.0 / 2.4) - 0.055;
    }
    return sign(x) * y;
}
