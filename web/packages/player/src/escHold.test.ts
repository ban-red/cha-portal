import { describe, expect, test } from "bun:test";

import { TOOLBAR } from "@cha/ui-spec";
import { ESC_CASES, ESC_TIMING } from "@cha/ui-spec/capture-cases";

import { EscHold } from "./escHold";

const { release_hold_ms: HOLD, release_hint_ms: HINT } = TOOLBAR.timing;

// The shared cases (web/packages/ui-spec/capture-cases.json) run here and in crates/cha-ui-spec/src/esc_hold.rs.
describe("EscHold, shared cases", () => {
  test("the cases were worked out with the spec's timing", () => {
    expect(ESC_TIMING).toEqual({ release_hold_ms: HOLD, release_hint_ms: HINT });
  });

  test("there are enough cases", () => {
    expect(ESC_CASES.length).toBeGreaterThanOrEqual(10);
  });

  for (const c of ESC_CASES) {
    test(c.name, () => {
      const esc = new EscHold(HOLD, HINT);
      c.steps.forEach((s, n) => {
        const at = `step ${n} (${s.ev} at ${s.t})`;
        let step = { host: null as "down" | "up" | null, release: false };
        switch (s.ev) {
          case "down":
            step = esc.keyDown(s.t, false, s.captured ?? true);
            break;
          case "repeat":
            step = esc.keyDown(s.t, true, s.captured ?? true);
            break;
          case "up":
            step = esc.keyUp(s.t);
            break;
          case "tick":
            step = esc.tick(s.t);
            break;
          case "blur":
            step = esc.blur();
            break;
          case "uncapture":
            esc.uncapture();
            break;
        }
        expect(step.host, `${at}: host`).toBe(s.host ?? null);
        expect(step.release, `${at}: release`).toBe(s.release ?? false);
        const hint = esc.hint(s.t);
        if (s.hint === undefined) expect(hint, `${at}: hint`).toBeNull();
        else expect(hint, `${at}: hint`).toBeCloseTo(s.hint, 9);
      });
    });
  }
});

describe("EscHold, timers", () => {
  test("says when the hint and the release are due", () => {
    const esc = new EscHold(HOLD, HINT);
    expect(esc.hintDue()).toBeNull();
    expect(esc.due()).toBeNull();
    esc.keyDown(5000, false, true);
    expect(esc.holding).toBe(true);
    expect(esc.hintDue()).toBe(5000 + HINT);
    expect(esc.due()).toBe(5000 + HOLD);
    esc.keyUp(5100);
    expect(esc.holding).toBe(false);
    expect(esc.due()).toBeNull();
  });

  test("the hint's progress never goes past 1", () => {
    const esc = new EscHold(HOLD, HINT);
    esc.keyDown(0, false, true);
    expect(esc.hint(HOLD * 3)).toBe(1);
  });
});
