import { describe, expect, test } from "bun:test";

import { parseToolbarPrefs, toolbarKey } from "./toolbarPrefs";

describe("toolbarPrefs", () => {
  test("a key per user and app type", () => {
    expect(toolbarKey("u1", "steam")).not.toBe(toolbarKey("u1", "chrome"));
    expect(toolbarKey("u1", "steam")).not.toBe(toolbarKey("u2", "steam"));
  });
  test("nothing or junk gives nothing", () => {
    expect(parseToolbarPrefs(null)).toEqual({});
    expect(parseToolbarPrefs("{nope")).toEqual({});
  });
  test("keeps well formed fields and drops the rest", () => {
    expect(parseToolbarPrefs('{"codec":"hevc","transport":"udp","muted":true,"volume":250,"mouse":false,"fps":120,"overlay":2.5}')).toEqual({
      codec: "hevc",
      muted: true,
      volume: 100,
      mouse: false,
      fps: 120,
    });
  });
});
