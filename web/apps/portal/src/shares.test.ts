import { describe, expect, test } from "bun:test";

import {
  controlPrompt,
  freeSlots,
  guestCanRetry,
  guestCodec,
  guestProblem,
  guestTransports,
  internetNote,
  invitation,
  joinLabel,
  playerLabel,
  seatLine,
  shareLink,
  shareWarning,
  timeLeft,
} from "./shares";

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

  test("viewer and controller links take no slot", () => {
    const live = [
      { role: "viewer", slot: null },
      { role: "controller", slot: null },
      { role: "player", slot: 2 },
    ];
    expect(freeSlots(live)).toEqual([1, 3]);
  });

  test("each role's wording", () => {
    expect(invitation("viewer", "Alex", null)).toBe("Alex invited you to watch.");
    expect(invitation("player", "Alex", 1)).toBe("Alex invited you to play as player 2.");
    expect(invitation("controller", "Alex", null)).toContain("when they hand you the controls");
    expect(joinLabel("viewer", null)).toBe("Watch");
    expect(joinLabel("controller", null)).toBe("Join");
    expect(joinLabel("player", 2)).toBe("Join as player 3");
    expect(seatLine("viewer", 1)).toBe("You are watching.");
    expect(seatLine("player", 3)).toBe("You are player 3. Press a button on your gamepad.");
    expect(seatLine("controller", 1)).toBe("");
  });

  test("the controller warning names the app and the hand-off", () => {
    const w = shareWarning("controller", "Firefox");
    expect(w).toContain("Anyone with this link can use your keyboard and mouse in Firefox when you hand them the controls");
    expect(w).toContain("whenever you aren't holding them");
    expect(shareWarning("viewer", "Firefox")).toContain("can't use the keyboard, mouse or gamepads");
  });

  test("a guest controller holds, takes or waits", () => {
    expect(controlPrompt(true, false, "Alex")).toEqual({ kind: "holding", text: "You have the controls." });
    expect(controlPrompt(false, true, "Alex")).toEqual({ kind: "take", text: "Take control" });
    expect(controlPrompt(false, false, "Alex")).toEqual({
      kind: "wait",
      text: "Waiting for Alex to hand you the controls",
    });
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

describe("internet links", () => {
  test("an absolute link stays as the tunnel gave it", () => {
    expect(shareLink("http://portal.lan:8090", "https://x.trycloudflare.com/s/tok")).toBe("https://x.trycloudflare.com/s/tok");
    expect(shareLink("http://portal.lan:8090", "/s/tok")).toBe("http://portal.lan:8090/s/tok");
  });

  test("the switch note warns about Cloudflare, and a quick tunnel's address", () => {
    expect(internetNote("named")).toContain("Cloudflare relays the stream and can see it");
    expect(internetNote("named")).not.toContain("address changes");
    expect(internetNote("quick")).toContain("The address changes when the tunnel restarts, which ends these links.");
  });

  test("an internet guest tries WebRTC, then WebSocket", () => {
    expect(guestTransports(true)).toEqual(["webrtc", "websocket"]);
    expect(guestTransports(false)).toEqual(["webtransport", "webrtc", "websocket"]);
  });

  test("an old node is named, and not retried", () => {
    expect(guestProblem(api(409, "node_outdated"))).toBe("The host's node needs updating to stream over the internet.");
    expect(guestCanRetry(api(409, "node_outdated"))).toBe(false);
  });
});
