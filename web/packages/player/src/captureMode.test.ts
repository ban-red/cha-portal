import { describe, expect, test } from "bun:test";

import { CaptureMode } from "./captureMode";

describe("CaptureMode", () => {
  test("idle until the mouse is captured; clicks pass", () => {
    const m = new CaptureMode();
    expect(m.state).toBe("idle");
    expect(m.pointerDown(0)).toBe("pass");
    expect(m.pointerUp(0)).toBe("pass");
    m.unlocked(); // a lock we never saw
    expect(m.state).toBe("idle");
  });

  test("capturing, released by the browser, then recaptured by a click", () => {
    const m = new CaptureMode();
    m.locked();
    expect(m.state).toBe("capturing");
    m.unlocked();
    expect(m.view).toEqual({ state: "released", recapture: true, hint: true });
    expect(m.pointerDown(0)).toBe("recapture");
    m.locked();
    expect(m.view).toEqual({ state: "capturing", recapture: false, hint: false });
    expect(m.pointerUp(0)).toBe("swallow");
    expect(m.pointerUp(0)).toBe("pass");
  });

  test("only the primary button recaptures; others pass", () => {
    const m = new CaptureMode();
    m.locked();
    m.unlocked();
    expect(m.pointerDown(2)).toBe("pass");
    expect(m.pointerUp(2)).toBe("pass");
    expect(m.state).toBe("released");
  });

  test("the hint hides but clicks still recapture", () => {
    const m = new CaptureMode();
    m.locked();
    m.unlocked();
    m.hideHint();
    expect(m.view.hint).toBe(false);
    expect(m.pointerDown(0)).toBe("recapture");
  });

  test("turning off sends clicks to the stream again", () => {
    const m = new CaptureMode();
    m.locked();
    m.unlocked();
    m.turnOff();
    expect(m.view).toEqual({ state: "idle", recapture: false, hint: false });
    expect(m.pointerDown(0)).toBe("pass");
    m.locked();
    m.unlocked();
    expect(m.state).toBe("released"); // capturing again starts over
  });

  test("a release by us is not recapturable", () => {
    const m = new CaptureMode();
    m.locked();
    m.releasedByUs();
    m.unlocked();
    expect(m.state).toBe("idle");
  });
});
