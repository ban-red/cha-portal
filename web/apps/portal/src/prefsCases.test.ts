// The saved-settings cases (web/packages/ui-spec/prefs-cases.json) against the browser's parsers, and
// against the generic validator as Cha Player runs it (cha-ui-spec has the same cases in Rust).
import { describe, expect, test } from "bun:test";

import { PREFS, parsePrefsText, STATS_PANEL, type Platform } from "@cha/ui-spec";
import { expectedPrefs, PREFS_CASES, runsOn, savedText } from "@cha/ui-spec/prefs-cases";

import { CORNERS, DEFAULT_PREFS, OPACITY_MIN, parsePrefs, SECTIONS } from "./statsOverlay";
import { parseToolbarPrefs } from "./toolbarPrefs";

describe("saved settings cases", () => {
  test("there are enough of them", () => {
    expect(PREFS_CASES.length).toBeGreaterThanOrEqual(20);
    expect(new Set(PREFS_CASES.map((c) => `${c.group}/${c.name}`)).size).toBe(PREFS_CASES.length);
  });
  // The app's own parsers: the cases for this platform.
  for (const c of PREFS_CASES.filter((c) => runsOn(c, "web"))) {
    test(`${c.group}: ${c.name} (web parser)`, () => {
      const parse = c.group === "stats_panel" ? parsePrefs : parseToolbarPrefs;
      expect(parse(savedText(c))).toEqual(expectedPrefs(c, "web") as never);
    });
  }
  // The validator itself, for both platforms (the native one is also run in Rust, on its real structs).
  for (const platform of ["web", "native"] as Platform[]) {
    for (const c of PREFS_CASES.filter((c) => runsOn(c, platform))) {
      test(`${c.group}: ${c.name} (${platform} validator)`, () => {
        expect(parsePrefsText(c.group, savedText(c), platform)).toEqual(expectedPrefs(c, platform));
      });
    }
  }
});

describe("saved settings spec", () => {
  test("the panel's sections and corners are the spec's", () => {
    expect(SECTIONS).toEqual(STATS_PANEL.sections.map((s) => s.id) as never);
    expect(CORNERS.length).toBe(4);
    expect(OPACITY_MIN).toBe(PREFS.stats_panel.fields.opacity!.min!);
  });
  test("the defaults are the spec's and are never shared", () => {
    expect(DEFAULT_PREFS).toEqual({ open: true, compact: false, collapsed: false, corner: "top-left", folded: [], opacity: 90, snap: true, pos: null });
    const a = parsePrefs(null);
    a.folded.push("node");
    expect(parsePrefs(null).folded).toEqual([]);
  });
});
