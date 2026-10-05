import { describe, expect, test } from "bun:test";

import { gamepadName, parseGamepadId } from "./gamepad-api";

describe("Gamepad ids", () => {
  test("Chrome's", () => {
    const id = "Xbox 360 pad (STANDARD GAMEPAD Vendor: 045e Product: 028e)";
    expect(parseGamepadId(id)).toEqual({ vendorId: 0x045e, productId: 0x028e });
    expect(gamepadName(id)).toBe("Xbox 360 pad");
    expect(gamepadName("8BitDo Pro 2 (Vendor: 2dc8 Product: 6003)")).toBe("8BitDo Pro 2");
  });
  test("Firefox's", () => {
    const id = "2dc8-6003-8BitDo Pro 2";
    expect(parseGamepadId(id)).toEqual({ vendorId: 0x2dc8, productId: 0x6003 });
    expect(gamepadName(id)).toBe("8BitDo Pro 2");
  });
  test("no ids", () => {
    expect(parseGamepadId("Some Pad")).toBeNull();
    expect(gamepadName("Some Pad")).toBe("Some Pad");
  });
});
