import { describe, expect, test } from "bun:test";

import { judgeLiveness, shouldReconnectOnResume } from "./liveness";

describe("judgeLiveness", () => {
  test("recent data is fine", () => {
    expect(judgeLiveness({ silentMs: 1200, tickGapMs: 1000, visible: true })).toBe("ok");
    expect(judgeLiveness({ silentMs: 3999, tickGapMs: 1000, visible: true })).toBe("ok");
  });
  test("4 s of silence on a visible page is a dead link", () => {
    expect(judgeLiveness({ silentMs: 4000, tickGapMs: 1000, visible: true })).toBe("dead");
  });
  test("a hidden page is never judged", () => {
    expect(judgeLiveness({ silentMs: 60_000, tickGapMs: 1000, visible: false })).toBe("ok");
  });
  test("a clock jump (sleep) is not the link's fault", () => {
    expect(judgeLiveness({ silentMs: 30_000, tickGapMs: 30_000, visible: true })).toBe("frozen");
  });
});

describe("shouldReconnectOnResume", () => {
  test("only when the last answer is older than 3 s", () => {
    expect(shouldReconnectOnResume(500)).toBe(false);
    expect(shouldReconnectOnResume(3000)).toBe(false);
    expect(shouldReconnectOnResume(3001)).toBe(true);
  });
});
