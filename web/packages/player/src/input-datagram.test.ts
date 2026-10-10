import { describe, expect, test } from "bun:test";

import { INPUT_BYTE0, MAX_INPUT_DATAGRAM, PadDatagrams, decodePadDatagram, encodePadDatagram, type DatagramSink } from "./input-datagram";

const pad = { t: "input", k: "pad", i: 1, q: 7, b: [1, 0], a: [0, 0, 0, 0] };

describe("input datagram framing", () => {
  test("version/kind byte, format byte, then the pad message as JSON", () => {
    const d = encodePadDatagram(pad)!;
    expect(d[0]).toBe(0x12);
    expect(INPUT_BYTE0).toBe(0x12);
    expect(d[1]).toBe(0);
    expect(new TextDecoder().decode(d.subarray(2))).toBe(JSON.stringify(pad));
    expect(decodePadDatagram(d)).toEqual(pad);
  });

  test("not a datagram of ours: nothing", () => {
    expect(decodePadDatagram(new Uint8Array([0x10, 0, 1]))).toBeNull();
    expect(decodePadDatagram(new Uint8Array([0x12, 9, 1]))).toBeNull();
    expect(decodePadDatagram(new Uint8Array([0x12]))).toBeNull();
    expect(decodePadDatagram(new Uint8Array([0x12, 0, 0x7b]))).toBeNull();
  });

  test("one that wouldn't fit a datagram is refused, to go reliably", () => {
    expect(encodePadDatagram({ ...pad, junk: "x".repeat(MAX_INPUT_DATAGRAM) })).toBeNull();
  });
});

describe("negotiation", () => {
  const sink = (log: Uint8Array[], ok = true): DatagramSink => ({
    send: (d) => (ok && log.push(d), ok),
  });

  test("before the streamer's hello offers them, pad state is reliable", () => {
    const log: Uint8Array[] = [];
    const path = new PadDatagrams(() => sink(log));
    expect(path.send(pad)).toBe(false);
    expect(log).toEqual([]);
  });

  test("once the hello offers them and a transport takes them, they go as datagrams", () => {
    const log: Uint8Array[] = [];
    const path = new PadDatagrams(() => sink(log));
    path.accepted = true;
    expect(path.send(pad)).toBe(true);
    expect(log).toHaveLength(1);
    expect(decodePadDatagram(log[0]!)).toEqual(pad);
    expect(path.sent).toBe(1);
  });

  test("a streamer that offers them but a transport that can't (no channel open, no writer) falls back", () => {
    const path = new PadDatagrams(() => null);
    path.accepted = true;
    expect(path.send(pad)).toBe(false);
    const closed = new PadDatagrams(() => sink([], false));
    closed.accepted = true;
    expect(closed.send(pad)).toBe(false);
    expect(closed.sent).toBe(0);
  });

  test("a message too big for a datagram falls back", () => {
    const log: Uint8Array[] = [];
    const path = new PadDatagrams(() => sink(log));
    path.accepted = true;
    expect(path.send({ ...pad, junk: "x".repeat(2000) })).toBe(false);
    expect(log).toEqual([]);
  });
});
