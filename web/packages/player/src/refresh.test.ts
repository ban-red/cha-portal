import { describe, expect, test } from "bun:test";
import { fitOf, refreshFromIntervals } from "./refresh";

describe("a frame rate on a screen", () => {
  test("on a 120 Hz ProMotion panel, 60 and 120 fit and 90 judders", () => {
    expect(fitOf(60, 120)).toBe("");
    expect(fitOf(120, 120)).toBe("");
    expect(fitOf(90, 120)).toBe("uneven");
  });

  test("on a 60 Hz screen, 90 and 120 are more than it shows", () => {
    expect(fitOf(60, 60)).toBe("");
    expect(fitOf(90, 60)).toBe("over");
    expect(fitOf(120, 60)).toBe("over");
  });

  test("other panels", () => {
    // 144 Hz: none of the three fit evenly.
    expect([60, 90, 120].map((f) => fitOf(f, 144))).toEqual(["uneven", "uneven", "uneven"]);
    // 180 and 90 Hz: 90 fits; 240 Hz: 60 and 120 do.
    expect(fitOf(90, 180)).toBe("");
    expect(fitOf(90, 90)).toBe("");
    expect([60, 90, 120].map((f) => fitOf(f, 240))).toEqual(["", "uneven", ""]);
  });

  test("a measured rate a little off still fits", () => {
    expect(fitOf(60, 119.6)).toBe("");
    expect(fitOf(120, 119.6)).toBe("");
  });

  test("a variable-refresh screen follows any rate up to its own", () => {
    expect(fitOf(90, 120, true)).toBe("");
    expect(fitOf(120, 60, true)).toBe("over");
  });

  test("an unmeasured screen says nothing", () => {
    expect(fitOf(90, null)).toBe("");
    expect(fitOf(90, 0)).toBe("");
  });
});

describe("the screen's refresh rate", () => {
  const frames = (ms: number, n = 60) => Array.from({ length: n }, () => ms);

  test("a ProMotion panel at 120 Hz and a 60 Hz one", () => {
    expect(refreshFromIntervals(frames(8.33))).toBe(120);
    expect(refreshFromIntervals(frames(16.7))).toBe(60);
  });

  test("a few late frames don't move the median", () => {
    expect(refreshFromIntervals([...frames(8.34, 50), 16.7, 25, 33.4, 16.7])).toBe(120);
  });

  test("an uncommon rate is rounded, and too few samples say nothing", () => {
    expect(refreshFromIntervals(frames(1000 / 85))).toBe(85);
    expect(refreshFromIntervals(frames(8.33, 5))).toBeNull();
    // A background tab's throttled rAF (1 s apart) is not a refresh rate.
    expect(refreshFromIntervals(frames(1000))).toBeNull();
  });
});
