import { describe, expect, test } from "bun:test";

import { normalizePairingCode } from "./pairingCode";

describe("normalizePairingCode", () => {
  test("accepts digits, a dash or whitespace", () => {
    expect(normalizePairingCode("48219375")).toBe("4821-9375");
    expect(normalizePairingCode("4821-9375")).toBe("4821-9375");
    expect(normalizePairingCode(" 4821 9375\n")).toBe("4821-9375");
  });
  test("rejects anything else", () => {
    expect(normalizePairingCode("")).toBeNull();
    expect(normalizePairingCode("4821937")).toBeNull();
    expect(normalizePairingCode("482193751")).toBeNull();
    expect(normalizePairingCode("4821-93a5")).toBeNull();
  });
});
