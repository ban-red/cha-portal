import { describe, expect, test } from "bun:test";

import { capturesDevices, inputAllowed, readsPads, sendsControls } from "./inputMode";
import { goneMessage, padMessage } from "./controllers/manager";

describe("input mode", () => {
  test("all follows the controls, as before", () => {
    expect(inputAllowed("all", true, { k: "key", code: "KeyA" })).toBe(true);
    expect(inputAllowed("all", false, { k: "key", code: "KeyA" })).toBe(false);
    expect(inputAllowed("all", false, goneMessage(0))).toBe(false);
    expect(capturesDevices("all")).toBe(true);
    expect(sendsControls("all")).toBe(true);
  });

  test("pads sends gamepads without the controls and nothing else", () => {
    for (const hasControl of [false, true]) {
      expect(inputAllowed("pads", hasControl, goneMessage(0))).toBe(true);
      expect(inputAllowed("pads", hasControl, { k: "pad", i: 0, b: [], a: [] })).toBe(true);
      for (const msg of [{ k: "key" }, { k: "mouse" }, { k: "wheel" }, { k: "move" }, {}]) {
        expect(inputAllowed("pads", hasControl, msg)).toBe(false);
      }
    }
    expect(capturesDevices("pads")).toBe(false);
    expect(sendsControls("pads")).toBe(false);
    expect(typeof padMessage).toBe("function");
  });

  test("none sends nothing at all, with the controls or without", () => {
    for (const hasControl of [false, true]) {
      for (const msg of [goneMessage(0), { k: "pad", i: 0, b: [], a: [] }, { k: "key" }, { k: "move" }, {}]) {
        expect(inputAllowed("none", hasControl, msg)).toBe(false);
      }
    }
    expect(capturesDevices("none")).toBe(false);
    expect(sendsControls("none")).toBe(false);
    expect(readsPads("none")).toBe(false);
    expect(readsPads("pads") && readsPads("all")).toBe(true);
  });
});
