import { expect, test } from "bun:test";

import { padRamp, Harness } from "./harness";

const stubVideo = () =>
  ({
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 640, height: 360 }),
    focus: () => {},
    dispatchEvent: () => true,
  }) as unknown as HTMLVideoElement;

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

test("holding a button sends a pad message for the session's own pad, then a neutral one", async () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  await h.padHold({ a: 1, lt: 0.5 }, { lx: -0.25, ry: 2 }, 10);

  expect(sent).toHaveLength(2);
  const [down, up] = sent;
  expect(down).toMatchObject({ k: "pad", i: 0, ty: "xbox" });
  expect(down.b).toHaveLength(24);
  expect(down.a).toHaveLength(4);
  expect(down.b[0]).toBe(1);
  expect(down.b[6]).toBe(0.5);
  expect(down.a[0]).toBe(-0.25);
  expect(down.a[3]).toBe(1); // clamped to -1..1
  expect(up.b.every((v: number) => v === 0)).toBe(true);
  expect(up.a.every((v: number) => v === 0)).toBe(true);
});

test("pad values go on the wire at three decimals", async () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  await h.padHold({}, { lx: 0.123456 }, 5);
  expect(sent[0]!.a[0]).toBe(0.123);
});

test("an unknown button or axis throws before anything is sent", async () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  await expect(h.padHold({ triangle: 1 }, {}, 10)).rejects.toThrow("unknown pad button");
  await expect(h.padHold({}, { zz: 1 }, 10)).rejects.toThrow("unknown pad axis");
  expect(sent).toEqual([]);
});

test("with no pad channel, a pad action says so", async () => {
  const h = new Harness(stubVideo());
  await expect(h.padHold({ a: 1 }, {}, 10)).rejects.toThrow("no pad channel");
});

test("releaseAll sends nothing when no pad is held", () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  h.releaseAll();
  expect(sent).toEqual([]);
});

test("an overlapping hold keeps its own: one ending never drops the other", async () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  const long = h.padHold({ a: 1 }, {}, 120);
  await sleep(20);
  await h.padHold({ b: 1 }, {}, 20);
  // The short one is done: b is loose again, a is still held.
  expect(h.padHeld).toBe(true);
  const last = sent.at(-1)!;
  expect(last.b[0]).toBe(1);
  expect(last.b[1]).toBe(0);
  await long;
  expect(h.padHeld).toBe(false);
  expect(sent.at(-1)!.b.every((v: number) => v === 0)).toBe(true);
});

test("padStick shapes a move: rises, holds under the peak, ends at centre", async () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  await h.padStick({ lx: -0.8 }, 60);

  expect(sent.length).toBeGreaterThanOrEqual(4);
  const pushed = sent.map((m) => m.a[0] as number);
  expect(pushed.at(-1)).toBe(0); // settled at centre
  expect(Math.max(...pushed.map(Math.abs))).toBeLessThanOrEqual(0.9); // never past the thumb's target
  expect(sent.every((m) => Number.isFinite(m.a[0]))).toBe(true);
});

test("releaseAll stops a stick run in flight: nothing is sent after the neutral", async () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  const run = h.padStick({ ly: 1 }, 3000);
  await sleep(40);
  h.releaseAll();
  await run;
  await sleep(120);
  const after = sent.length;
  expect(sent.at(-1)!.a).toEqual([0, 0, 0, 0]);
  expect(h.padHeld).toBe(false);
  await sleep(150); // a stale loop would keep pushing updates
  expect(sent.length).toBe(after);
});

test("padTap mashes with rests between presses", async () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  await h.padTap("a", 3);
  const presses = sent.map((m) => m.b[0]);
  expect(sent.length).toBeGreaterThanOrEqual(6);
  expect(presses.filter((v) => v === 1)).toHaveLength(3);
  expect(presses.at(-1)).toBe(0);
});

test("padPull ramps a trigger up and eases it off", async () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  await h.padPull("rt", 20, 0.75);
  const pull = sent.map((m) => m.b[7]);
  expect(pull.at(-1)).toBe(0);
  expect(Math.max(...pull)).toBeLessThanOrEqual(0.76);
  expect(Math.max(...pull)).toBeGreaterThanOrEqual(0.5); // the ramp actually got there
});

test("padRamp ends exactly where it is pointed, from either side", () => {
  for (const [from, to] of [[0, 1], [1, -0.8], [0.3, 0.3]] as const) {
    const steps = padRamp(from, to, 5);
    expect(steps).toHaveLength(5);
    expect(steps.at(-1)).toBe(to);
    const dir = Math.sign(to - from);
    for (let i = 1; i < steps.length; i++) {
      const d = Math.sign(steps[i]! - steps[i - 1]!);
      if (dir !== 0) expect(d).toBe(dir); // monotone along the way
    }
  }
});
