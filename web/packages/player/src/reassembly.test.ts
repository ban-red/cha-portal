import { beforeAll, describe, expect, test } from "bun:test";
import type { FromWorker } from "./reassembly";

// The module posts to the worker's parent; collect instead.
const out: FromWorker[] = [];
(globalThis as { postMessage?: unknown }).postMessage = (m: FromWorker) => void out.push(m);

let Reassembler: typeof import("./reassembly").Reassembler;
let before: typeof import("./reassembly").before;
beforeAll(async () => {
  ({ Reassembler, before } = await import("./reassembly"));
});

/** A `cha-stream/1` datagram: kind, flags, stream, fec, id, index, total, send ts. */
function datagram(o: { kind?: number; flags?: number; id: number; index?: number; total?: number; payload: number[] }): Uint8Array {
  const d = new Uint8Array(16 + o.payload.length);
  const v = new DataView(d.buffer);
  v.setUint8(0, o.kind ?? 0);
  v.setUint8(1, o.flags ?? 0);
  v.setUint32(4, o.id, true);
  v.setUint16(8, o.index ?? 0, true);
  v.setUint16(10, o.total ?? 1, true);
  d.set(o.payload, 16);
  return d;
}

describe("Reassembler", () => {
  test("hands a keyframe over, in order, with fragments joined", () => {
    out.length = 0;
    const rx = new Reassembler(() => {});
    rx.datagram(datagram({ flags: 1, id: 7, index: 1, total: 2, payload: [3, 4] }));
    rx.datagram(datagram({ flags: 1, id: 7, index: 0, total: 2, payload: [1, 2] }));
    const video = out.filter((m) => m.type === "video");
    expect(video).toHaveLength(1);
    const f = video[0]!;
    if (f.type !== "video") throw new Error("not video");
    expect(f.id).toBe(7);
    expect(f.key).toBe(true);
    expect([...new Uint8Array(f.data)]).toEqual([1, 2, 3, 4]);
  });

  test("a delta frame waits for a keyframe and asks for one", () => {
    out.length = 0;
    const sent: string[] = [];
    const rx = new Reassembler((l) => sent.push(l));
    rx.datagram(datagram({ id: 3, payload: [9] }));
    expect(out.filter((m) => m.type === "video")).toHaveLength(0);
    expect(sent.map((l) => JSON.parse(l).t)).toEqual(["keyframe"]);
  });

  test("audio passes through", () => {
    out.length = 0;
    const rx = new Reassembler(() => {});
    rx.datagram(datagram({ kind: 1, id: 5, payload: [1, 2, 3] }));
    const a = out.find((m) => m.type === "audio");
    expect(a && a.type === "audio" && a.id).toBe(5);
  });

  test("short datagrams are ignored", () => {
    out.length = 0;
    new Reassembler(() => {}).datagram(new Uint8Array(4));
    expect(out).toHaveLength(0);
  });

  test("frame ids compare with wraparound", () => {
    expect(before(1, 2)).toBe(true);
    expect(before(0xffffffff, 0)).toBe(true);
    expect(before(2, 2)).toBe(false);
  });
});
