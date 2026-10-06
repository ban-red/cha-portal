import { describe, expect, test } from "bun:test";

import { overlayAnswer, parseOverlay } from "./overlay";

describe("overlay", () => {
  test("levels 0 to 4 and custom are reported; anything else means no overlay", () => {
    for (const level of [0, 1, 2, 3, 4]) expect(parseOverlay(level)).toBe(level as 0);
    expect(parseOverlay("custom")).toBe("custom");
    for (const none of [undefined, null, 5, -1, 1.5, "2", "off", {}]) expect(parseOverlay(none)).toBeNull();
  });

  test("an answer carries the level now, with the reason when it didn't change", () => {
    expect(overlayAnswer({ level: 3 })).toEqual({ level: 3, error: undefined });
    expect(overlayAnswer({ level: 1, error: "overlay level 9 isn't one of 0 to 4" })).toEqual({
      level: 1,
      error: "overlay level 9 isn't one of 0 to 4",
    });
    // An app without an overlay: no level, only the reason.
    expect(overlayAnswer({ error: "this app has no performance overlay" })).toEqual({
      level: null,
      error: "this app has no performance overlay",
    });
  });
});
