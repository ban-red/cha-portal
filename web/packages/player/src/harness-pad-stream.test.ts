import { expect, test } from "bun:test";

import { Harness } from "./harness";

const stubVideo = () =>
  ({
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 640, height: 360 }),
    focus: () => {},
    dispatchEvent: () => true,
  }) as unknown as HTMLVideoElement;

test("a stream sends each step as a full state, then lets go after the hold", async () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  await h.padStream([{ buttons: { b: 1 }, axes: { lx: 0.5 } }, { buttons: { rt: 1 } }], 5, 10);

  expect(sent).toHaveLength(3); // two steps, then the watchdog's neutral
  expect(sent[0].b[1]).toBe(1);
  expect(sent[0].a[0]).toBe(0.5);
  expect(sent[1].b[1]).toBe(0); // b is not named in the second step: released
  expect(sent[1].b[7]).toBe(1);
  expect(sent[1].a[0]).toBe(0); // and so is the stick
  expect(sent[2].b.every((v: number) => v === 0)).toBe(true);
});

test("a newer stream takes over: the older one stops and never lets go underneath it", async () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  const first = h.padStream([{ buttons: { a: 1 } }, { buttons: { a: 1 } }, { buttons: { a: 1 } }, { buttons: { a: 1 } }], 20, 5);
  await new Promise((r) => setTimeout(r, 25));
  const second = h.padStream([{ buttons: { x: 1 } }], 5, 5);
  await Promise.all([first, second]);

  // The older run played only the steps before the takeover, and only the newer one released.
  const aSteps = sent.filter((m) => m.b[0] === 1).length;
  expect(aSteps).toBeLessThan(4);
  expect(sent.filter((m) => m.b[2] === 1)).toHaveLength(1);
  const last = sent[sent.length - 1];
  expect(last.b.every((v: number) => v === 0)).toBe(true);
});

test("an unknown name throws before anything is sent", () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  expect(() => h.padStream([{ buttons: { triangle: 1 } }])).toThrow("unknown pad button");
  expect(() => h.padStream([{ axes: { zz: 1 } }])).toThrow("unknown pad axis");
  expect(sent).toEqual([]);
});

test("releaseAll cancels a running stream", async () => {
  const sent: any[] = [];
  const h = new Harness(stubVideo(), (m) => sent.push(m));
  const run = h.padStream([{ buttons: { a: 1 } }, { buttons: { a: 1 } }, { buttons: { a: 1 } }], 20, 50);
  await new Promise((r) => setTimeout(r, 5));
  h.releaseAll();
  await run;
  expect(sent.filter((m) => m.b[0] === 1).length).toBeLessThan(3);
  expect(sent[sent.length - 1].b.every((v: number) => v === 0)).toBe(true);
});
