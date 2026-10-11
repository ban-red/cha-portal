import { describe, expect, test } from "bun:test";

import {
  HID_REPORT,
  INPUT_BYTE0,
  MAX_INPUT_DATAGRAM,
  PadDatagrams,
  decodeHidDatagram,
  decodePadDatagram,
  encodeHidDatagram,
  encodePadDatagram,
  type DatagramSink,
} from "./input-datagram";

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

describe("raw HID report datagrams", () => {
  const report = new Uint8Array([0x42, 1, 2, 3]);

  test("format 1: slot, sequence number big endian, then the report with its id", () => {
    const d = encodeHidDatagram(2, 0x01020304, report)!;
    expect(HID_REPORT).toBe(1);
    expect([...d]).toEqual([0x12, 1, 2, 1, 2, 3, 4, 0x42, 1, 2, 3]);
    const back = decodeHidDatagram(d)!;
    expect(back.slot).toBe(2);
    expect(back.q).toBe(0x01020304);
    expect([...back.report]).toEqual([0x42, 1, 2, 3]);
    // The same bytes the streamer's cha-proto test has (crates/cha-proto/src/input.rs).
    expect([...encodeHidDatagram(2, 0x01020304, new Uint8Array([0x42, 9, 8, 7]))!.subarray(2)]).toEqual([2, 1, 2, 3, 4, 0x42, 9, 8, 7]);
  });

  test("a sequence number past 2^31 survives", () => {
    expect(decodeHidDatagram(encodeHidDatagram(0, 0xfffffffe, report)!)!.q).toBe(0xfffffffe);
  });

  test("not a raw report: nothing", () => {
    expect(decodeHidDatagram(new Uint8Array([0x12, 0, 0, 0, 0, 0, 0, 1]))).toBeNull();
    expect(decodeHidDatagram(new Uint8Array([0x10, 1, 0, 0, 0, 0, 1, 1]))).toBeNull();
    // No report behind the header.
    expect(decodeHidDatagram(new Uint8Array([0x12, 1, 0, 0, 0, 0, 1]))).toBeNull();
    expect(encodeHidDatagram(0, 1, new Uint8Array(0))).toBeNull();
    expect(encodeHidDatagram(0, 1, new Uint8Array(MAX_INPUT_DATAGRAM))).toBeNull();
  });

  test("goes as a datagram only once the hello offers them and a transport takes them", () => {
    const log: Uint8Array[] = [];
    const path = new PadDatagrams(() => ({ send: (d) => (log.push(d), true) }));
    expect(path.sendHid(1, 5, report)).toBe(false);
    path.accepted = true;
    expect(path.sendHid(1, 5, report)).toBe(true);
    expect(decodeHidDatagram(log[0]!)).toMatchObject({ slot: 1, q: 5 });
    expect(new PadDatagrams(() => null).sendHid(1, 5, report)).toBe(false);
  });
});
