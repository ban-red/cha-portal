import { describe, expect, test } from "bun:test";

import { backoffDelay, classifyConnectError } from "./reconnect";

const api = (status: number, code: string, message = "nope") => Object.assign(new Error(message), { status, code });

describe("backoffDelay", () => {
  test("1, 2, 4, 8, then 15 s for good", () => {
    expect([1, 2, 3, 4, 5, 6, 50].map(backoffDelay)).toEqual([1000, 2000, 4000, 8000, 15000, 15000, 15000]);
  });
  test("a bad attempt number still gives the first wait", () => {
    expect(backoffDelay(0)).toBe(1000);
    expect(backoffDelay(-3)).toBe(1000);
  });
});

describe("classifyConnectError", () => {
  test("network errors and timeouts retry", () => {
    expect(classifyConnectError(new TypeError("Failed to fetch")).kind).toBe("retry");
    expect(classifyConnectError(new Error("timed out")).kind).toBe("retry");
    expect(classifyConnectError("odd").kind).toBe("retry");
  });
  test("5xx, 408 and 429 retry", () => {
    for (const s of [500, 502, 503, 504, 408, 429]) expect(classifyConnectError(api(s, "x")).kind).toBe("retry");
  });
  test("401 goes to sign-in", () => {
    expect(classifyConnectError(api(401, "unauthorized")).kind).toBe("login");
  });
  test("403, 404, 400 and no_node stop with the reason", () => {
    for (const [s, c] of [[403, "forbidden"], [404, "not_found"], [400, "bad_codec"], [409, "no_node"]] as const) {
      expect(classifyConnectError(api(s, c, "why"))).toEqual({ kind: "stop", reason: "why" });
    }
  });
  test("not_running waits for the environment", () => {
    expect(classifyConnectError(api(409, "not_running", "stopped")).kind).toBe("wait-running");
  });
});
