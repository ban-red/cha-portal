import { describe, expect, test } from "bun:test";
import { fpsLabel } from "./appFps";

describe("the app card's frame rates", () => {
  test("on a 120 Hz Mac, 90 fps is marked uneven", () => {
    expect([60, 90, 120].map((f) => fpsLabel(f, 120))).toEqual(["60 fps", "90 fps (uneven at 120 Hz)", "120 fps"]);
  });

  test("on a 60 Hz screen, 90 and 120 fps are more than it shows", () => {
    expect([60, 90, 120].map((f) => fpsLabel(f, 60))).toEqual([
      "60 fps",
      "90 fps (more than 60 Hz shows)",
      "120 fps (more than 60 Hz shows)",
    ]);
  });

  test("before the screen is measured, the rates are plain", () => {
    expect([60, 90, 120].map((f) => fpsLabel(f, null))).toEqual(["60 fps", "90 fps", "120 fps"]);
  });
});
