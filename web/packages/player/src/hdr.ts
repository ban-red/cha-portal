// HDR10 to the screen, the reference for the shaders (docs/plans/hdr-and-vrr.md, step 7.1): the
// same functions as crates/cha-upscale/src/hdr.wgsl, which Cha Player draws with and the browser's
// PyroWave path copies, line for line, in double precision. The WGSL is checked against the same
// values on a GPU (crates/cha-upscale/tests/hdr.rs).

const PQ_M1 = 2610 / 16384;
const PQ_M2 = (2523 / 4096) * 128;
const PQ_C1 = 3424 / 4096;
const PQ_C2 = (2413 / 4096) * 32;
const PQ_C3 = (2392 / 4096) * 32;

/** BT.2408's reference (diffuse and graphics) white, cd/m²: where SDR white sits in an HDR picture. */
export const REFERENCE_WHITE_NITS = 203;

/** SMPTE ST 2084: a PQ signal (0..1) as cd/m². */
export function pqToNits(e: number): number {
  const p = Math.min(Math.max(e, 0), 1) ** (1 / PQ_M2);
  return 10000 * (Math.max(p - PQ_C1, 0) / (PQ_C2 - PQ_C3 * p)) ** (1 / PQ_M1);
}

/** The inverse: cd/m² as a PQ signal. */
export function nitsToPq(nits: number): number {
  const y = Math.min(Math.max(nits / 10000, 0), 1) ** PQ_M1;
  return ((PQ_C1 + PQ_C2 * y) / (1 + PQ_C3 * y)) ** PQ_M2;
}

export type Rgb = [number, number, number];

/** Linear BT.2020 to linear BT.709 (BT.2087); negatives kept, for extended-range surfaces. */
export function bt2020ToBt709([r, g, b]: Rgb): Rgb {
  return [
    1.660491 * r - 0.587641 * g - 0.07285 * b,
    -0.12455 * r + 1.1329 * g - 0.008349 * b,
    -0.018151 * r - 0.100579 * g + 1.11873 * b,
  ];
}

/**
 * BT.2390's EETF with the blacks at zero: a source peaking at `srcPeak` cd/m² shown on a display
 * peaking at `dstPeak`. Below the knee a value passes unchanged; above it a Hermite spline in PQ
 * takes the source's peak to the display's.
 */
export function eetf(nits: number, srcPeak: number, dstPeak: number): number {
  if (dstPeak >= srcPeak) return Math.min(nits, srcPeak);
  const lw = nitsToPq(srcPeak);
  const e1 = Math.min(nitsToPq(nits) / lw, 1);
  const maxLum = nitsToPq(dstPeak) / lw;
  const ks = Math.max(1.5 * maxLum - 0.5, 0);
  let e2 = e1;
  if (e1 > ks) {
    const t = (e1 - ks) / (1 - ks);
    const t2 = t * t;
    const t3 = t2 * t;
    e2 = (2 * t3 - 3 * t2 + 1) * ks + (t3 - 2 * t2 + t) * (1 - ks) + (-2 * t3 + 3 * t2) * maxLum;
  }
  return pqToNits(Math.min(e2, maxLum) * lw);
}

/**
 * A PQ pixel (BT.2020) as linear BT.709 with 1.0 at the reference white, its highlights rolled off
 * to the display's peak by their largest channel (so hues hold). On an extended-range surface, 1.0
 * is SDR white and the display's headroom is `dstPeak / refWhite`.
 */
export function hdr10ToDisplay(pq: Rgb, refWhite: number, srcPeak: number, dstPeak: number): Rgb {
  const rgb = bt2020ToBt709([pqToNits(pq[0]), pqToNits(pq[1]), pqToNits(pq[2])]);
  const m = Math.max(...rgb);
  const scale = m > 0 ? eetf(m, srcPeak, dstPeak) / m : 1;
  return rgb.map((c) => (c * scale) / refWhite) as Rgb;
}

/** The sRGB curve, mirrored for negatives: for float canvases whose values are sRGB-encoded. */
export function srgbEncode(x: number): number {
  const a = Math.abs(x);
  const y = a <= 0.0031308 ? 12.92 * a : 1.055 * a ** (1 / 2.4) - 0.055;
  return Math.sign(x) * y;
}
