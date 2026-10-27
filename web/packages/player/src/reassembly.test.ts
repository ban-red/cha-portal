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

// PyroWave: frames flagged INTRA stand alone; a packet split over datagrams is
// flagged CONTINUES on all but its last part and CONTINUED on all but its first.
const KEY_INTRA = 1 | (1 << 5);
const CONTINUES = 1 << 3;
const CONTINUED = 1 << 4;

describe("Reassembler, intra (PyroWave)", () => {
  /** Runs `f` with the worker's clock at `ms`, then puts the clock back. */
  function at(ms: number, f: () => void): void {
    const real = performance.now;
    performance.now = () => ms;
    try {
      f();
    } finally {
      performance.now = real;
    }
  }
  const expire = (rx: InstanceType<typeof Reassembler>) => (rx as unknown as { expire(): void }).expire();
  const videos = () => out.flatMap((m) => (m.type === "video" ? [m] : []));

  test("a frame goes out complete, in part order, as soon as its last part comes", () => {
    out.length = 0;
    const rx = new Reassembler(() => {});
    at(1000, () => {
      // Packet A whole in part 0; packet B split over parts 1 and 2.
      rx.datagram(datagram({ flags: KEY_INTRA | CONTINUED, id: 4, index: 2, total: 3, payload: [5, 6] }));
      rx.datagram(datagram({ flags: KEY_INTRA, id: 4, index: 0, total: 3, payload: [1, 2] }));
      expect(videos()).toHaveLength(0);
      rx.datagram(datagram({ flags: KEY_INTRA | CONTINUES, id: 4, index: 1, total: 3, payload: [3, 4] }));
    });
    const [f] = videos();
    expect(f?.id).toBe(4);
    expect(f?.partial).toBe(false);
    expect([...new Uint8Array(f!.data)]).toEqual([1, 2, 3, 4, 5, 6]);
  });

  test("at its deadline, a frame goes out with its whole packets; a split one missing a part is left out", () => {
    out.length = 0;
    const rx = new Reassembler(() => {});
    at(1000, () => {
      rx.datagram(datagram({ flags: KEY_INTRA, id: 9, index: 0, total: 4, payload: [1] }));
      // Packet B: parts 1 and 2, part 2 lost.
      rx.datagram(datagram({ flags: KEY_INTRA | CONTINUES, id: 9, index: 1, total: 4, payload: [2] }));
      rx.datagram(datagram({ flags: KEY_INTRA, id: 9, index: 3, total: 4, payload: [4] }));
    });
    at(1059, () => expire(rx));
    expect(videos()).toHaveLength(0);
    at(1060, () => expire(rx));
    const [f] = videos();
    expect(f?.partial).toBe(true);
    expect([...new Uint8Array(f!.data)]).toEqual([1, 4]);
    expect(out.some((m) => m.type === "partial")).toBe(true);
  });

  test("a frame with no whole packet by its deadline is lost", () => {
    out.length = 0;
    const rx = new Reassembler(() => {});
    at(1000, () => rx.datagram(datagram({ flags: KEY_INTRA | CONTINUES, id: 2, index: 0, total: 2, payload: [7] })));
    at(1060, () => expire(rx));
    expect(videos()).toHaveLength(0);
    expect(out.some((m) => m.type === "lost")).toBe(true);
  });
});
