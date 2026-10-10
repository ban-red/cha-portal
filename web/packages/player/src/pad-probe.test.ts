import { expect, test } from "bun:test";
import { isPadGreen, summarize } from "./pad-probe";

test("pad green is told from the dark backdrop and the white click flash", () => {
  expect(isPadGreen(0, 255, 0)).toBe(true);
  expect(isPadGreen(20, 230, 30)).toBe(true);
  expect(isPadGreen(20, 22, 40)).toBe(false);
  expect(isPadGreen(255, 255, 255)).toBe(false);
});

test("summarize gives percentiles and handles no samples", () => {
  const s = summarize(Array.from({ length: 100 }, (_, i) => i + 1));
  expect(s.min).toBe(1);
  expect(s.max).toBe(100);
  expect(s.p50).toBe(51);
  expect(summarize([]).p50).toBeNull();
});
