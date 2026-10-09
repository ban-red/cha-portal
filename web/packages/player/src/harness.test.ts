import { expect, test } from "bun:test";

import {
  centroidOf,
  codeForChar,
  fittsMs,
  frameDiff,
  gridOf,
  keyFor,
  lognormal,
  matchFraction,
  meanRgb,
  minJerk,
  pathSteps,
  rng,
  type ChannelMatch,
  type Frame,
  typeDelay,
  wheelNotches,
} from "./harness";

test("the same seed replays the same sequence", () => {
  const a = rng(7);
  const b = rng(7);
  expect([a(), a(), a()]).toEqual([b(), b(), b()]);
  expect(rng(8)()).not.toBe(rng(7)());
});

test("the minimum-jerk profile runs 0 to 1 and eases at the ends", () => {
  expect(minJerk(0)).toBe(0);
  expect(minJerk(1)).toBeCloseTo(1, 10);
  expect(minJerk(0.1)).toBeLessThan(0.1);
  expect(minJerk(0.9)).toBeGreaterThan(0.9);
});

test("a path's steps sum to the move, bowed or not", () => {
  for (const bow of [0, 0.05, -0.05]) {
    const steps = pathSteps(240, -40, 30, bow);
    expect(steps).toHaveLength(30);
    expect(steps.reduce((s, [x]) => s + x, 0)).toBeCloseTo(240, 6);
    expect(steps.reduce((s, [, y]) => s + y, 0)).toBeCloseTo(-40, 6);
  }
});

const solid = (w: number, h: number, rgb: [number, number, number]): Frame => {
  const data = new Uint8ClampedArray(w * h * 4);
  for (let i = 0; i < w * h; i++) data.set([...rgb, 255], i * 4);
  return { w, h, data };
};

test("the mean colour of a region reads that region only", () => {
  const f = solid(4, 4, [0, 0, 0]);
  for (let y = 0; y < 2; y++) for (let x = 0; x < 2; x++) f.data.set([200, 100, 50, 255], (y * 4 + x) * 4);
  expect(meanRgb(f, { x: 0, y: 0, w: 0.5, h: 0.5 })).toEqual({ r: 200, g: 100, b: 50 });
  expect(meanRgb(f, { x: 0.5, y: 0.5, w: 0.5, h: 0.5 })).toEqual({ r: 0, g: 0, b: 0 });
  expect(meanRgb(f).r).toBe(50);
});

test("frame difference is zero for equal frames and grows with the change", () => {
  const a = solid(8, 4, [10, 100, 10]);
  expect(frameDiff(a, solid(8, 4, [10, 100, 10]))).toBe(0);
  expect(frameDiff(a, solid(8, 4, [10, 140, 10]))).toBe(40);
  expect(frameDiff(a, solid(4, 4, [10, 100, 10]))).toBe(255);
});

test("keyFor gives the values a real keyboard sends", () => {
  expect(keyFor("KeyW")).toBe("w");
  expect(keyFor("KeyW", true)).toBe("W");
  expect(keyFor("Digit4")).toBe("4");
  expect(keyFor("Digit4", true)).toBe("$");
  expect(keyFor("Space")).toBe(" ");
  expect(keyFor("ShiftLeft")).toBe("Shift");
  expect(keyFor("ArrowUp")).toBe("ArrowUp");
  expect(keyFor("Quote", true)).toBe('"');
  expect(keyFor("F5")).toBe("F5");
  expect(keyFor("SomethingOdd")).toBe("SomethingOdd");
});

test("codeForChar round-trips through keyFor", () => {
  const text = "Ab Zz 19 !? <>:~ +_() \"'";
  for (const ch of text) {
    const entry = codeForChar(ch);
    expect(entry).not.toBeNull();
    const [code, shift] = entry!;
    expect(keyFor(code, shift)).toBe(ch);
  }
  expect(codeForChar("é")).toBeNull();
});

test("typing delays: repeats slowest, alternations quickest, boundaries apart", () => {
  expect(typeDelay("a", "a")).toBeGreaterThan(typeDelay("m", "e")); // same finger vs hands
  expect(typeDelay("m", "e")).toBeLessThan(typeDelay("m", "n")); // alternation vs same class
  expect(typeDelay("a", " ")).toBe(150); // a word boundary
});

test("fittsMs grows with distance, by less per equal step", () => {
  expect(fittsMs(0)).toBeCloseTo(170, 10);
  expect(fittsMs(100)).toBeLessThan(fittsMs(400));
  expect(fittsMs(400) - fittsMs(300)).toBeLessThan(fittsMs(200) - fittsMs(100));
  expect(fittsMs(-5)).toBe(fittsMs(0));
});

test("lognormal is seeded, positive, and clamped", () => {
  const a = rng(7);
  const b = rng(7);
  expect([lognormal(a, 100), lognormal(a, 100)]).toEqual([lognormal(b, 100), lognormal(b, 100)]);
  for (let i = 0; i < 200; i++) {
    const v = lognormal(rng(i + 1), 100);
    expect(v).toBeGreaterThan(0);
    expect(v).toBeLessThanOrEqual(300);
  }
  // A degenerate generator with u≈0 pushes the multiplier to its 3× clamp.
  expect(lognormal(() => 1e-9, 100)).toBeCloseTo(300, 6);
  // cos(π/2) ≈ 0: the Gaussian term vanishes and the median comes back whole.
  const q = (() => {
    const seq = [0.9, 0.25];
    return () => seq.shift()!;
  })();
  expect(lognormal(q, 100)).toBeCloseTo(100, 10);
});

test("wheel notches sum to the delta, sign kept, all about a notch wide", () => {
  expect(wheelNotches(0)).toEqual([]);
  for (const total of [1, 100, 121, 250, -360, 5000]) {
    const notches = wheelNotches(total);
    expect(notches.length).toBeGreaterThan(0);
    expect(notches.reduce((a, b) => a + b, 0)).toBe(total);
    // The remainder notch rides a little over 120; nothing is ever one giant delta.
    for (const n of notches) expect(Math.abs(n)).toBeLessThanOrEqual(125);
    expect(notches.every((n) => Math.sign(n) === Math.sign(total))).toBe(true);
  }
});

const halfRed = (w: number, h: number): Frame => {
  const f = { w, h, data: new Uint8ClampedArray(w * h * 4) };
  for (let y = 0; y < h; y++)
    for (let x = 0; x < w; x++) f.data.set(x < w / 2 ? [200, 10, 10, 255] : [5, 5, 5, 255], (y * w + x) * 4);
  return f;
};

const red: ChannelMatch = { channel: "r", min: 100 };

test("matchFraction reads only the region it is given", () => {
  const f = halfRed(8, 4);
  expect(matchFraction(f, { x: 0, y: 0, w: 0.25, h: 1 }, red)).toBe(1); // all red there
  expect(matchFraction(f, { x: 0.75, y: 0, w: 0.25, h: 1 }, red)).toBe(0); // all dark there
  expect(matchFraction(f, { x: 0, y: 0, w: 1, h: 1 }, red)).toBe(0.5);
});

test("centroidOf finds where the matches sit, and null when they are gone", () => {
  const f = halfRed(8, 4);
  // Pixel-index mean: the red pixels sit at x 0..3, y 0..3.
  expect(centroidOf(f, { x: 0, y: 0, w: 1, h: 1 }, red)).toEqual({ x: 0.1875, y: 0.375, n: 16 });
  expect(centroidOf(f, { x: 0.5, y: 0, w: 0.5, h: 1 }, red)).toBeNull();
});

test("gridOf lays cells out row-major at the frame's aspect", () => {
  const f = halfRed(8, 4);
  const g = gridOf(f, 4);
  expect(g.cols).toBe(4);
  expect(g.rows).toBe(2);
  expect(g.cells).toHaveLength(8);
  expect(g.cells[0]!.r).toBe(200); // top left: red half
  expect(g.cells[3]!.r).toBe(5); // top right: dark half
  expect(g.cells[0]!.r).toBe(g.cells[4]!.r); // rows alike in this picture
});
