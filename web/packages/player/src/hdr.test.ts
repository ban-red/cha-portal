import { describe, expect, test } from "bun:test";
import { bt2020ToBt709, eetf, hdr10ToDisplay, nitsToPq, pqToNits, REFERENCE_WHITE_NITS, srgbEncode } from "./hdr";

const near = (got: number, want: number, digits = 4) => expect(got).toBeCloseTo(want, digits);

describe("PQ (SMPTE ST 2084)", () => {
  test("BT.2408's reference white is 58% of the signal, and the published anchors hold", () => {
    near(nitsToPq(REFERENCE_WHITE_NITS), 0.580689);
    near(nitsToPq(100), 0.508078);
    near(nitsToPq(1000), 0.751827);
    expect(nitsToPq(10000)).toBe(1);
    near(pqToNits(0.5), 92.245709, 3);
    expect(pqToNits(1)).toBe(10000);
    expect(pqToNits(0)).toBe(0);
  });

  test("the two directions undo each other", () => {
    for (const nits of [0.05, 1, 80, 203, 600, 1000, 4000]) expect(pqToNits(nitsToPq(nits))).toBeCloseTo(nits, 6);
  });

  test("signals outside 0..1 are clamped", () => {
    expect(pqToNits(1.5)).toBe(10000);
    expect(pqToNits(-0.1)).toBe(0);
  });
});

describe("BT.2020 to BT.709", () => {
  test("white stays white, and BT.2020's green lies outside BT.709", () => {
    for (const c of bt2020ToBt709([1, 1, 1])) near(c, 1, 5);
    const [r, g, b] = bt2020ToBt709([0, 1, 0]);
    expect(r).toBeLessThan(0);
    expect(b).toBeLessThan(0);
    near(g, 1.1329);
  });
});

describe("BT.2390's roll-off", () => {
  test("a 4000 cd/m² master on a 1000 cd/m² display", () => {
    expect(eetf(100, 4000, 1000)).toBeCloseTo(100, 6); // below the knee: unchanged
    near(eetf(4000, 4000, 1000), 1000, 3); // the source's peak to the display's
    near(eetf(2000, 4000, 1000), 974.937174, 3);
    // Above the source's peak nothing gets brighter.
    near(eetf(10000, 4000, 1000), 1000, 3);
  });

  test("it rises smoothly and never passes the display's peak", () => {
    let last = -1;
    for (let nits = 0; nits <= 4000; nits += 25) {
      const out = eetf(nits, 4000, 1000);
      expect(out).toBeGreaterThanOrEqual(last);
      expect(out).toBeLessThanOrEqual(1000 + 1e-6);
      last = out;
    }
  });

  test("a display at least as bright as the master changes nothing", () => {
    expect(eetf(500, 1000, 1000)).toBe(500);
    expect(eetf(800, 1000, 1600)).toBe(800);
  });
});

describe("a PQ pixel on an extended-range surface", () => {
  test("reference white comes out as SDR white", () => {
    const w = nitsToPq(REFERENCE_WHITE_NITS);
    for (const c of hdr10ToDisplay([w, w, w], REFERENCE_WHITE_NITS, 1000, 1000)) near(c, 1, 5);
  });

  test("a 1000 cd/m² highlight on a display with 2x headroom is held to it, with its hue", () => {
    const pq = nitsToPq(1000);
    const [r, g, b] = hdr10ToDisplay([pq, pq * 0.95, pq * 0.9], REFERENCE_WHITE_NITS, 1000, 2 * REFERENCE_WHITE_NITS);
    expect(Math.max(r, g, b)).toBeLessThanOrEqual(2 + 1e-6);
    // Scaled by the brightest channel, so the channels keep their order.
    expect(r).toBeGreaterThan(g);
    expect(g).toBeGreaterThan(b);
  });

  test("black is black", () => {
    expect(hdr10ToDisplay([0, 0, 0], REFERENCE_WHITE_NITS, 1000, 1000)).toEqual([0, 0, 0]);
  });
});

describe("the signed sRGB curve", () => {
  test("it matches sRGB on 0..1 and keeps going and mirrors outside it", () => {
    expect(srgbEncode(0)).toBe(0);
    near(srgbEncode(1), 1, 6);
    near(srgbEncode(0.5), 0.735357);
    near(srgbEncode(2), 1.353256);
    near(srgbEncode(-0.5), -0.735357);
  });
});
