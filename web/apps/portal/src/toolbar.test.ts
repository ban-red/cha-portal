// The toolbar's shared cases (web/packages/ui-spec/toolbar-cases.json) run against the browser's side
// of the model, plus what a case can't say: that the spec is wired to the renderer.
import { describe, expect, test } from "bun:test";

import { buildToolbar, TOOLBAR } from "@cha/ui-spec";
import { capsOf, describeToolbar, expectedFor, runsOn, stateOf, TOOLBAR_CASES } from "@cha/ui-spec/toolbar-cases";

describe("toolbar cases", () => {
  test("there are enough of them", () => {
    expect(TOOLBAR_CASES.length).toBeGreaterThanOrEqual(20);
    expect(new Set(TOOLBAR_CASES.map((c) => c.name)).size).toBe(TOOLBAR_CASES.length);
  });
  // Both platforms' toolbars come from the same spec and function, so both run here (and in cha-ui-spec).
  for (const platform of ["web", "native"] as const) {
    for (const c of TOOLBAR_CASES.filter((c) => runsOn(c, platform))) {
      test(`${c.name} (${platform})`, () => {
        const want = expectedFor(c, platform);
        const model = buildToolbar(TOOLBAR, stateOf(c, platform), capsOf(c), platform);
        expect(describeToolbar(model, want)).toEqual(want);
      });
    }
  }
});
