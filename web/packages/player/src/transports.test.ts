import { describe, expect, test } from "bun:test";
import { transportOrder, type TransportAvailability } from "./transports";

const all: TransportAvailability = { webtransport: true, webrtc: true, websocket: true };

describe("transportOrder", () => {
  test("auto and unset are WebTransport, then WebRTC", () => {
    expect(transportOrder({}, false, all)).toEqual(["webtransport", "webrtc"]);
    expect(transportOrder({ transport: "auto" }, false, all)).toEqual(["webtransport", "webrtc"]);
  });

  test("a single transport is the only one", () => {
    expect(transportOrder({ transport: "webrtc" }, false, all)).toEqual(["webrtc"]);
    expect(transportOrder({ transport: "websocket" }, false, all)).toEqual(["websocket"]);
  });

  test("transports wins over transport and keeps its order", () => {
    expect(transportOrder({ transport: "webrtc", transports: ["webrtc", "websocket"] }, false, all)).toEqual(["webrtc", "websocket"]);
    expect(transportOrder({ transports: ["webtransport", "webrtc", "websocket"] }, false, all)).toEqual(["webtransport", "webrtc", "websocket"]);
  });

  test("unavailable transports are skipped", () => {
    const noWt = { ...all, webtransport: false };
    expect(transportOrder({ transports: ["webtransport", "webrtc", "websocket"] }, false, noWt)).toEqual(["webrtc", "websocket"]);
    expect(transportOrder({}, false, noWt)).toEqual(["webrtc"]);
  });

  test("nothing usable falls to WebRTC, as before", () => {
    expect(transportOrder({ transport: "webtransport" }, false, { ...all, webtransport: false })).toEqual(["webrtc"]);
  });

  test("PyroWave is WebTransport only", () => {
    expect(transportOrder({ transports: ["webrtc", "websocket"] }, true, all)).toEqual(["webtransport"]);
  });

  test("repeats are dropped", () => {
    expect(transportOrder({ transports: ["webrtc", "webrtc", "websocket"] }, false, all)).toEqual(["webrtc", "websocket"]);
  });
});
