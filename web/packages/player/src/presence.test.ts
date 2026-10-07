import { describe, expect, test } from "bun:test";

import { presenceActive } from "./presence";

describe("presenceActive", () => {
  test("a visible tab is in use, heard or not", () => {
    expect(presenceActive(true, false)).toBe(true);
    expect(presenceActive(true, true)).toBe(true);
  });

  test("a hidden tab is in use only while its sound plays", () => {
    expect(presenceActive(false, true)).toBe(true);
    expect(presenceActive(false, false)).toBe(false);
  });
});
