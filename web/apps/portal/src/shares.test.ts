import { describe, expect, test } from "bun:test";

import { freeSlots, guestCanRetry, guestCodec, guestProblem, playerLabel, shareLink, timeLeft } from "./shares";

const api = (status: number, code: string) => Object.assign(new Error("x"), { status, code });

describe("shares", () => {
  test("slot 1 is player 2", () => {
    expect([1, 2, 3].map(playerLabel)).toEqual(["player 2", "player 3", "player 4"]);
  });

  test("the link joins the origin and the portal's path", () => {
    expect(shareLink("https://portal.lan", "/s/abc")).toBe("https://portal.lan/s/abc");
    expect(shareLink("https://portal.lan/", "/s/abc")).toBe("https://portal.lan/s/abc");
    expect(shareLink("http://x:8090", "s/abc")).toBe("http://x:8090/s/abc");
  });

  test("time left", () => {
    const now = 1_000_000 * 1000;
    expect(timeLeft(1_000_000 - 1, now)).toBeNull();
    expect(timeLeft(1_000_000 + 30, now)).toBe("under a minute");
    expect(timeLeft(1_000_000 + 45 * 60, now)).toBe("45 min");
    expect(timeLeft(1_000_000 + 90 * 60, now)).toBe("1 h 30 min");
    expect(timeLeft(1_000_000 + 24 * 3600, now)).toBe("24 h");
  });

  test("free slots are the ones without a live link", () => {
    expect(freeSlots([])).toEqual([1, 2, 3]);
    expect(freeSlots([{ slot: 2 }])).toEqual([1, 3]);
    expect(freeSlots([{ slot: 1 }, { slot: 2 }, { slot: 3 }])).toEqual([]);
  });

  test("the guest page's two plain answers", () => {
    expect(guestProblem(api(404, "unknown_share"))).toBe("This link has expired or was revoked.");
    expect(guestProblem(api(409, "not_running"))).toBe("The game isn't running right now.");
    expect(guestProblem(new TypeError("Failed to fetch"))).toContain("Couldn't reach");
    expect(guestCanRetry(api(404, "unknown_share"))).toBe(false);
    expect(guestCanRetry(api(409, "not_running"))).toBe(false);
    expect(guestCanRetry(api(502, "node_error"))).toBe(true);
    expect(guestCanRetry(new TypeError("x"))).toBe(true);
  });
});

describe("guestCodec", () => {
  test("the browser's first that the device encodes", () => {
    expect(guestCodec(["hevc", "h264", "av1"], ["h264", "hevc"])).toBe("hevc");
    expect(guestCodec(["hevc", "h264"], ["h264"])).toBe("h264");
  });
  test("unknown device codecs: the browser's first; nothing shared: H.264", () => {
    expect(guestCodec(["hevc", "h264"], null)).toBe("hevc");
    expect(guestCodec(["av1"], ["h264"])).toBe("h264");
  });
});
