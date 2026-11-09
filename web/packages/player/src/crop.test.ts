import { describe, expect, test } from "bun:test";
import { cropSize, shownSize, viewBox } from "./crop";

describe("cropSize", () => {
  test("crops AMD's padded AV1: width to 64, height to 16", () => {
    expect(cropSize({ w: 1216, h: 1440 }, { w: 1192, h: 1440 })).toEqual({ w: 1192, h: 1440 });
    expect(cropSize({ w: 1920, h: 1082 }, { w: 1920, h: 1080 })).toEqual({ w: 1920, h: 1080 });
    expect(cropSize({ w: 1280, h: 736 }, { w: 1272, h: 728 })).toEqual({ w: 1272, h: 728 });
  });
  test("is a no-op when the sizes match", () => {
    expect(cropSize({ w: 2560, h: 1440 }, { w: 2560, h: 1440 })).toBeNull();
  });
  test("shows the frame as is without a size, or with a bigger one", () => {
    expect(cropSize({ w: 1216, h: 1440 }, null)).toBeNull();
    expect(cropSize({ w: 1216, h: 1440 }, undefined)).toBeNull();
    expect(cropSize({ w: 1216, h: 1440 }, { w: 1920, h: 1440 })).toBeNull();
    expect(cropSize({ w: 1216, h: 1440 }, { w: 1216, h: 1600 })).toBeNull();
    expect(cropSize({ w: 1216, h: 1440 }, { w: 0, h: 0 })).toBeNull();
    expect(cropSize({ w: 1216, h: 1440 }, { w: NaN, h: 1440 })).toBeNull();
  });
  test("leaves a frame from before a resize alone", () => {
    expect(cropSize({ w: 2560, h: 1440 }, { w: 800, h: 600 })).toBeNull();
    expect(cropSize({ w: 1280, h: 720 }, { w: 1216, h: 720 })).toBeNull(); // 64 is not padding
  });
});

describe("viewBox", () => {
  test("insets the right and bottom as percentages", () => {
    expect(viewBox({ w: 1216, h: 1440 }, { w: 1192, h: 1440 })).toBe("inset(0 1.9737% 0.0000% 0)");
    expect(viewBox({ w: 1920, h: 1082 }, { w: 1920, h: 1080 })).toBe("inset(0 0.0000% 0.1848% 0)");
  });
  test("is empty when there is nothing to crop", () => {
    expect(viewBox({ w: 1920, h: 1080 }, { w: 1920, h: 1080 })).toBe("");
    expect(viewBox({ w: 1920, h: 1080 }, null)).toBe("");
  });
});

describe("shownSize", () => {
  test("a 1440p cap stretched to a MacBook panel", () => {
    // requestSize asks 2216×1440 of a full-screen 1512×982 element at 2× (3024×1964).
    expect(shownSize({ w: 2216, h: 1440 }, { width: 1512, height: 982 }, 2)).toEqual({ w: 3022, h: 1964 });
  });
  test("letterboxes by the tighter side", () => {
    expect(shownSize({ w: 1920, h: 1080 }, { width: 1000, height: 1000 }, 1)).toEqual({ w: 1000, h: 563 });
  });
  test("a picture shown at its own size, give or take a pixel of rounding, isn't scaled", () => {
    expect(shownSize({ w: 2560, h: 1440 }, { width: 1280, height: 720 }, 2)).toBeNull();
    expect(shownSize({ w: 2560, h: 1440 }, { width: 1280.4, height: 720.2 }, 2)).toBeNull();
  });
  test("a smaller window shrinks it", () => {
    expect(shownSize({ w: 2560, h: 1440 }, { width: 640, height: 360 }, 2)).toEqual({ w: 1280, h: 720 });
  });
  test("nothing laid out or no picture yet", () => {
    expect(shownSize({ w: 2560, h: 1440 }, { width: 0, height: 0 }, 2)).toBeNull();
    expect(shownSize({ w: 0, h: 0 }, { width: 640, height: 360 }, 2)).toBeNull();
  });
});
