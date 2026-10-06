import { describe, expect, test } from "bun:test";

import { contextAction, MAX_RESUME_TRIES, PeakWindow, STALL_MS, StallDetector } from "./audio-watch";

describe("StallDetector", () => {
  test("pushed but never played, while running, is a stall after STALL_MS", () => {
    const d = new StallDetector();
    expect(d.observe(0, 480, 0, true)).toBe(false);
    expect(d.observe(1000, 48_000, 0, true)).toBe(false);
    expect(d.observe(STALL_MS - 1, 96_000, 0, true)).toBe(false);
    expect(d.observe(STALL_MS, 100_000, 0, true)).toBe(true);
  });
  test("playing resets it", () => {
    const d = new StallDetector();
    d.observe(0, 480, 0, true);
    d.observe(1900, 5000, 0, true);
    expect(d.observe(2000, 6000, 480, true)).toBe(false);
    expect(d.observe(3900, 9000, 480, true)).toBe(false);
  });
  test("a context that isn't running, or nothing pushed, is not a stall", () => {
    const d = new StallDetector();
    for (let t = 0; t < 10_000; t += 1000) expect(d.observe(t, t, 0, false)).toBe(false);
    const q = new StallDetector();
    for (let t = 0; t < 10_000; t += 1000) expect(q.observe(t, 0, 0, true)).toBe(false);
  });
});

describe("PeakWindow", () => {
  test("is the largest value in the window, null when empty", () => {
    const p = new PeakWindow(1000);
    expect(p.peak(0)).toBeNull();
    p.note(0, 0.5);
    p.note(500, 0.2);
    expect(p.peak(600)).toBe(0.5);
    expect(p.peak(1200)).toBe(0.2);
    expect(p.peak(2000)).toBeNull();
  });
});

describe("contextAction", () => {
  const base = { state: "suspended", wantSound: true, visible: true, hasRun: true, resumeTries: 0 };
  test("running needs nothing; closed is rebuilt", () => {
    expect(contextAction({ ...base, state: "running" })).toBe("none");
    expect(contextAction({ ...base, state: "closed", wantSound: false })).toBe("rebuild");
  });
  test("a suspended context is resumed, then rebuilt when resuming keeps failing", () => {
    expect(contextAction(base)).toBe("resume");
    expect(contextAction({ ...base, state: "interrupted" })).toBe("resume");
    expect(contextAction({ ...base, resumeTries: MAX_RESUME_TRIES })).toBe("rebuild");
  });
  test("muted or hidden is left alone", () => {
    expect(contextAction({ ...base, wantSound: false })).toBe("none");
    expect(contextAction({ ...base, visible: false })).toBe("none");
  });
  test("one that never ran (autoplay) is retried, never rebuilt", () => {
    expect(contextAction({ ...base, hasRun: false, resumeTries: 99 })).toBe("resume");
  });
});
