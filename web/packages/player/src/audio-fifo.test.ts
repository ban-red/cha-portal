import { describe, expect, test } from "bun:test";

import { DEFAULT_PREFILL_FRAMES, PACKET_FRAMES, QUANTUM_FRAMES, SoundFifo } from "./audio-fifo";

const ramp = (n: number, from = 1) => Float32Array.from({ length: n }, (_, i) => (from + i) / 100_000);
const quantum = () => [new Float32Array(QUANTUM_FRAMES), new Float32Array(QUANTUM_FRAMES)] as const;

describe("SoundFifo", () => {
  test("outputs silence until the prefill is queued", () => {
    const f = new SoundFifo({ prefillFrames: 600 });
    f.push(ramp(PACKET_FRAMES), ramp(PACKET_FRAMES));
    const [l, r] = quantum();
    l.fill(9);
    expect(f.pull(l, r)).toBe(0);
    expect(l.every((v) => v === 0)).toBe(true);
    expect(f.counters().underruns).toBe(0);
    f.push(ramp(PACKET_FRAMES), ramp(PACKET_FRAMES));
    expect(f.pull(l, r)).toBe(QUANTUM_FRAMES);
  });

  test("plays frames in order, per channel, in 128-frame quanta", () => {
    const f = new SoundFifo({ prefillFrames: 256 });
    f.push(ramp(256, 1), ramp(256, 1001));
    const [l, r] = quantum();
    f.pull(l, r);
    expect(l[0]).toBeCloseTo(1 / 100_000, 9);
    expect(r[0]).toBeCloseTo(1001 / 100_000, 9);
    expect(l[127]).toBeCloseTo(128 / 100_000, 9);
    f.pull(l, r);
    expect(l[0]).toBeCloseTo(129 / 100_000, 9);
    expect(f.counters()).toMatchObject({ played: 256, pushed: 256, queued: 0, underruns: 0 });
  });

  test("an empty queue is an underrun: the rest is silence, and it waits for the prefill again", () => {
    const f = new SoundFifo({ prefillFrames: 200 });
    f.push(ramp(200), ramp(200));
    const [l, r] = quantum();
    expect(f.pull(l, r)).toBe(128);
    l.fill(9);
    expect(f.pull(l, r)).toBe(72);
    expect(l[71]).not.toBe(0);
    expect(l[72]).toBe(0);
    expect(f.counters().underruns).toBe(1);
    // Below the prefill again: silence, not another underrun.
    f.push(ramp(100), ramp(100));
    expect(f.pull(l, r)).toBe(0);
    expect(f.counters().underruns).toBe(1);
    f.push(ramp(100), ramp(100));
    expect(f.pull(l, r)).toBe(128);
  });

  test("steady 10 ms packets at the default prefill never underrun", () => {
    const f = new SoundFifo();
    const [l, r] = quantum();
    let queued = 0;
    // 1 s of 480-frame packets against 128-frame quanta, the packets 1 quantum off phase.
    let owed = 0;
    for (let t = 0; t < 48_000 / QUANTUM_FRAMES; t++) {
      owed += QUANTUM_FRAMES;
      while (owed >= PACKET_FRAMES - 100) {
        f.push(new Float32Array(PACKET_FRAMES).fill(0.1), new Float32Array(PACKET_FRAMES).fill(0.1));
        owed -= PACKET_FRAMES;
        queued += PACKET_FRAMES;
      }
      f.pull(l, r);
    }
    expect(queued).toBeGreaterThan(40_000);
    expect(f.counters().underruns).toBe(0);
    expect(f.counters().drops).toBe(0);
    expect(DEFAULT_PREFILL_FRAMES).toBe(608);
  });

  test("past the cap the oldest frames are dropped, and counted", () => {
    const f = new SoundFifo({ prefillFrames: 480, capFrames: 2000, catchUpFrames: 960 });
    for (let i = 0; i < 4; i++) f.push(ramp(480, 1 + i * 480), ramp(480, 1 + i * 480)); // 1920 queued
    expect(f.counters()).toMatchObject({ drops: 0, queued: 1920 });
    f.push(ramp(480, 1 + 4 * 480), ramp(480, 1 + 4 * 480));
    const c = f.counters();
    expect(c.drops).toBe(1);
    expect(c.queued).toBe(960);
    expect(c.droppedFrames).toBe(1920 + 480 - 960);
    // What is left is the newest: frame 1441 onwards.
    const [l, r] = quantum();
    f.pull(l, r);
    expect(l[0]).toBeCloseTo(1441 / 100_000, 9);
  });

  test("the default cap is the prefill plus 60 ms", () => {
    const f = new SoundFifo();
    for (let i = 0; i < 6; i++) f.push(new Float32Array(480), new Float32Array(480)); // 2880 queued
    expect(f.counters().drops).toBe(0);
    for (let i = 0; i < 4; i++) f.push(new Float32Array(480), new Float32Array(480)); // past 608 + 2880
    expect(f.counters().drops).toBe(1);
    expect(f.counters().queued).toBeLessThan(608 + 2880);
  });

  test("the peak is the largest played sample since last asked, then resets", () => {
    const f = new SoundFifo({ prefillFrames: 128 });
    const quiet = new Float32Array(128).fill(0.1);
    const loud = new Float32Array(128).fill(0.1);
    loud[50] = -0.8;
    f.push(quiet, quiet);
    f.push(loud, quiet);
    const [l, r] = quantum();
    f.pull(l, r);
    f.pull(l, r);
    expect(f.takePeak()).toBeCloseTo(0.8, 6);
    expect(f.takePeak()).toBe(0);
  });

  test("the ring wraps around", () => {
    const f = new SoundFifo({ prefillFrames: 128, capFrames: 7000 });
    const [l, r] = quantum();
    for (let i = 0; i < 100; i++) {
      f.push(ramp(480, 1 + i * 480), ramp(480, 1 + i * 480));
      for (let k = 0; k < 3; k++) f.pull(l, r);
    }
    // 100 × 480 pushed, 300 × 128 pulled: 9600 left queued would exceed the ring, so the cap trimmed it, never corrupting order.
    f.pull(l, r);
    for (let i = 1; i < l.length; i++) expect(l[i]! - l[i - 1]!).toBeCloseTo(1 / 100_000, 7);
  });
});
