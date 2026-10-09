import { describe, expect, test } from "bun:test";
import { cropSize, viewBox } from "./crop";

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
