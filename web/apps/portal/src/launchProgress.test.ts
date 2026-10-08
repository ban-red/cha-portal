import { describe, expect, test } from "bun:test";

import { progressPercent, progressText } from "./launchProgress";

const bytes = (done: number, total: number) => ({ done, total, unit: "bytes" });

describe("progressText", () => {
  test("megabytes below a gigabyte", () => {
    expect(progressText(bytes(412 * 1e6, 890 * 1e6))).toBe("412 of 890 MB");
  });
  test("gigabytes with one decimal, from a gigabyte up", () => {
    expect(progressText(bytes(1.2 * 1e9, 3.4 * 1e9))).toBe("1.2 of 3.4 GB");
    expect(progressText(bytes(0, 2 * 1e9))).toBe("0 of 2 GB");
  });
  test("other units print as counted", () => {
    expect(progressText({ done: 3, total: 9, unit: "layers" })).toBe("3 of 9 layers");
  });
});

describe("progressPercent", () => {
  test("is the fraction, kept within 0 to 100", () => {
    expect(progressPercent(bytes(1, 4))).toBe(25);
    expect(progressPercent(bytes(9, 4))).toBe(100);
    expect(progressPercent(bytes(1, 0))).toBe(0);
  });
});
