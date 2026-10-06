import { describe, expect, test } from "bun:test";

import { clampPos, codecTag, cornerByArrow, DEFAULT_PREFS, nearestCorner, num, parsePrefs } from "./statsOverlay";

describe("statsOverlay", () => {
  test("defaults for missing or broken storage", () => {
    expect(parsePrefs(null)).toEqual(DEFAULT_PREFS);
    expect(parsePrefs("{nope")).toEqual(DEFAULT_PREFS);
    expect(parsePrefs('{"corner":"middle","open":"yes"}')).toEqual(DEFAULT_PREFS);
  });
  test("reads valid fields", () => {
    expect(parsePrefs('{"open":false,"compact":true,"collapsed":true,"corner":"bottom-right","folded":["node","bogus","stream"]}')).toEqual({
      open: false,
      compact: true,
      collapsed: true,
      corner: "bottom-right",
      folded: ["stream", "node"],
      opacity: 90,
      snap: true,
      pos: null,
    });
  });
  test("opacity, snapping and position", () => {
    expect(parsePrefs('{"opacity":10,"snap":false,"pos":{"left":40,"top":8}}')).toMatchObject({ opacity: 30, snap: false, pos: { left: 40, top: 8 } });
    expect(parsePrefs('{"opacity":250,"pos":{"left":"x","top":1}}')).toMatchObject({ opacity: 100, pos: null });
  });
  test("clamp", () => {
    expect(clampPos({ left: 900, top: -5 }, 200, 100, 1000, 600)).toEqual({ left: 800, top: 0 });
    expect(clampPos({ left: 50, top: 50 }, 2000, 900, 1000, 600)).toEqual({ left: 0, top: 0 });
  });
  test("nearest corner", () => {
    expect(nearestCorner(10, 10, 100, 100)).toBe("top-left");
    expect(nearestCorner(90, 80, 100, 100)).toBe("bottom-right");
  });
  test("arrows", () => {
    expect(cornerByArrow("top-left", "ArrowRight")).toBe("top-right");
    expect(cornerByArrow("top-left", "ArrowDown")).toBe("bottom-left");
    expect(cornerByArrow("bottom-right", "ArrowLeft")).toBe("bottom-left");
    expect(cornerByArrow("bottom-right", "Enter")).toBe("bottom-right");
  });
  test("text", () => {
    expect(num(null)).toBe("–");
    expect(num(18.66)).toBe("18.7");
    expect(codecTag("hevc", "webtransport")).toBe("HEVC/WT");
    expect(codecTag("HEVC · WebTransport", "webtransport")).toBe("HEVC/WT");
    expect(codecTag(null, null)).toBe("");
  });
});
