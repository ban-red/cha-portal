import { expect, test } from "bun:test";

import { cleanPin, expiresIn, hostAddress, pairErrorText, validPin } from "./moonlightDevices";

test("cleanPin keeps up to four digits", () => {
  expect(cleanPin("12a3 456")).toBe("1234");
  expect(cleanPin("")).toBe("");
});

test("validPin needs exactly four digits", () => {
  expect(validPin("0042")).toBe("0042");
  expect(validPin("12 34")).toBe("1234");
  expect(validPin("123")).toBeNull();
  expect(validPin("12345")).toBeNull();
  expect(validPin("12a4")).toBeNull();
});

test("expiresIn", () => {
  const now = 1_000_000_000;
  expect(expiresIn(1_000_000 + 45, now)).toBe("in 45 s");
  expect(expiresIn(1_000_000 + 300, now)).toBe("in 5 min");
  expect(expiresIn(1_000_000 - 1, now)).toBe("expired");
  expect(expiresIn(1_000_000, now)).toBe("expired");
});

test("hostAddress hides the default port", () => {
  expect(hostAddress({ address: "10.0.0.5", httpPort: 47989 })).toBe("10.0.0.5");
  expect(hostAddress({ address: "10.0.0.5", httpPort: 48000 })).toBe("10.0.0.5:48000");
});

test("pairErrorText", () => {
  expect(pairErrorText(403, "wrong_pin", "x")).toContain("doesn't match");
  expect(pairErrorText(404, "not_found", "x")).toContain("expired");
  expect(pairErrorText(504, "timeout", "x")).toContain("didn't finish");
  expect(pairErrorText(502, "node_unreachable", "Node is offline")).toBe("Node is offline");
  expect(pairErrorText(null, null, null)).toBe("Pairing failed.");
});
