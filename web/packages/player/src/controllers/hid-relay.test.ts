import { describe, expect, test } from "bun:test";

import { HidRelay, RAW_RESEND_MS, RAW_REST_RESENDS, STEAM_LIZARD_REPORTS, fromBase64, parseRawHid, toBase64, type RelayLink } from "./hid-relay";
import type { HidDevice } from "./hid-types";
import { NEVER_UPDATE_TIME, PUCK_GET_RETRY, RELAYED_COMMANDS, rewriteBuildTimes } from "./steam-protocol";

/** A device that does what it is told, in the order it is told, and can be slow or broken. */
class FakeDevice {
  vendorId = 0x28de;
  productId = 0x1302;
  productName = "Steam Controller";
  collections = [];
  opened = true;
  readonly log: string[] = [];
  /** The next operation waits for this. */
  gate: Promise<void> | null = null;
  failing = false;
  feature = new Uint8Array([0x83, 5, 1, 2]);
  /** Answers of the next reads, in order (an Error fails the read); then `feature` again. */
  script: (Uint8Array | Error)[] = [];

  private async op(name: string) {
    this.log.push(name);
    const gate = this.gate;
    this.gate = null;
    await gate;
    if (this.failing) throw new Error("the device is gone");
  }
  async receiveFeatureReport(id: number) {
    await this.op(`get ${id}`);
    const next = this.script.shift();
    if (next instanceof Error) throw next;
    return new DataView((next ?? this.feature).buffer.slice(0));
  }
  async sendFeatureReport(id: number, data: BufferSource) {
    await this.op(`set ${id} ${[...new Uint8Array(data as ArrayBuffer)].join(",")}`);
  }
  async sendReport(id: number, data: BufferSource) {
    await this.op(`out ${id} ${[...new Uint8Array(data as ArrayBuffer)].join(",")}`);
  }
  addEventListener() {}
  removeEventListener() {}
}

function setup(datagrams = true) {
  const device = new FakeDevice();
  const sent: Record<string, unknown>[] = [];
  const datagram: { slot: number; q: number; report: number[] }[] = [];
  const link = { datagrams };
  const relayLink: RelayLink = {
    send: (m) => void sent.push(m),
    sendReport: (slot, q, report) => {
      if (!link.datagrams) return false;
      datagram.push({ slot, q, report: [...report] });
      return true;
    },
  };
  let q = 0;
  let clock = 1000;
  const relay = new HidRelay(2, device as unknown as HidDevice, relayLink, () => ++q, () => clock, STEAM_LIZARD_REPORTS);
  return { device, relay, sent, datagram, link, advance: (ms: number) => (clock += ms) };
}

const view = (...bytes: number[]) => new DataView(new Uint8Array(bytes).buffer);
const flush = () => new Promise((r) => setTimeout(r, 0));
const settle = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe("opening", () => {
  test("asks to pass the device through, naming it, and reports nothing until the streamer agrees", () => {
    const { relay, sent, datagram } = setup();
    relay.open();
    expect(relay.state).toBe("opening");
    expect(sent).toEqual([{ k: "hid", op: "open", i: 2, vid: 0x28de, pid: 0x1302 }]);
    relay.open();
    expect(sent).toHaveLength(1);
    relay.report(0x42, view(1, 2, 3));
    expect(datagram).toEqual([]);
    expect(sent).toHaveLength(1);
  });

  test("on yes the latest report of each kind goes up, so the app has a state at once", () => {
    const { relay, datagram } = setup();
    relay.open();
    relay.report(0x42, view(1));
    relay.report(0x42, view(2));
    relay.report(0x43, view(9));
    expect(relay.accepted(true)).toBe(true);
    expect(relay.state).toBe("open");
    expect(datagram.map((d) => d.report)).toEqual([
      [0x42, 2],
      [0x43, 9],
    ]);
  });

  test("on no it is closed, and forgets what it heard", () => {
    const { relay, datagram } = setup();
    relay.open();
    relay.report(0x42, view(1));
    expect(relay.accepted(false)).toBe(false);
    expect(relay.state).toBe("closed");
    relay.report(0x42, view(2));
    expect(datagram).toEqual([]);
  });

  test("closing says so once", () => {
    const { relay, sent } = setup();
    relay.open();
    relay.accepted(true);
    relay.close();
    relay.close();
    expect(sent.filter((m) => m.op === "close")).toEqual([{ k: "hid", op: "close", i: 2 }]);
    expect(relay.state).toBe("closed");
  });
});

describe("input reports", () => {
  test("go up verbatim: the id, then the bytes, numbered in order", () => {
    const { relay, datagram, sent } = setup();
    relay.open();
    relay.accepted(true);
    relay.report(0x42, view(5, 6, 7));
    relay.report(0x42, view(8));
    expect(datagram).toEqual([
      { slot: 2, q: 1, report: [0x42, 5, 6, 7] },
      { slot: 2, q: 2, report: [0x42, 8] },
    ]);
    // Nothing went on the control channel but the open.
    expect(sent).toHaveLength(1);
    expect(relay.stats.datagrams).toBe(2);
  });

  test("a report is a copy: the browser may reuse its buffer", () => {
    const { relay, datagram } = setup();
    relay.open();
    relay.accepted(true);
    const buf = new Uint8Array([1, 2, 3]);
    relay.report(0x42, new DataView(buf.buffer));
    buf.fill(0);
    expect(datagram[0]!.report).toEqual([0x42, 1, 2, 3]);
  });

  test("without a datagram path they go on the control channel, base64", () => {
    const { relay, sent, link } = setup();
    link.datagrams = false;
    relay.open();
    relay.accepted(true);
    relay.report(0x42, view(5, 6));
    expect(sent.at(-1)).toEqual({ k: "hid", op: "rep", i: 2, q: 1, d: toBase64(new Uint8Array([0x42, 5, 6])) });
    expect(fromBase64(sent.at(-1)!.d as string)).toEqual(new Uint8Array([0x42, 5, 6]));
  });

  test("lizard mode's mouse and keyboard are not sent", () => {
    const { relay, datagram } = setup();
    relay.open();
    relay.accepted(true);
    relay.report(0x40, view(1, 2, 3, 4, 5));
    relay.report(0x41, view(0, 0, 4, 0, 0, 0, 0, 0));
    expect(datagram).toEqual([]);
  });

  test("a quiet report is sent again with its number, a few times", () => {
    const { relay, datagram, advance } = setup();
    relay.open();
    relay.accepted(true);
    relay.report(0x42, view(5));
    relay.tick();
    expect(datagram).toHaveLength(1);
    for (let i = 0; i < RAW_REST_RESENDS + 3; i++) {
      advance(RAW_RESEND_MS - 1);
      relay.tick();
      advance(1);
      relay.tick();
    }
    // The first, and three copies of it.
    expect(datagram).toHaveLength(1 + RAW_REST_RESENDS);
    expect(new Set(datagram.map((d) => d.q))).toEqual(new Set([1]));
  });

  test("a continuous stream is never sent twice", () => {
    const { relay, datagram, advance } = setup();
    relay.open();
    relay.accepted(true);
    for (let i = 0; i < 100; i++) {
      relay.report(0x42, view(i));
      advance(4);
      relay.tick();
    }
    expect(datagram).toHaveLength(100);
  });

  test("a report that went reliably needs no copy", () => {
    const { relay, sent, link, advance } = setup();
    link.datagrams = false;
    relay.open();
    relay.accepted(true);
    relay.report(0x42, view(5));
    advance(500);
    relay.tick();
    expect(sent.filter((m) => m.op === "rep")).toHaveLength(1);
  });

  test("a closed relay sends nothing", () => {
    const { relay, datagram, advance } = setup();
    relay.open();
    relay.accepted(true);
    relay.report(0x42, view(5));
    relay.close();
    advance(500);
    relay.tick();
    relay.report(0x42, view(6));
    expect(datagram).toHaveLength(1);
  });
});

describe("requests from the app", () => {
  const req = (id: number, op: string, r: number, data?: number[]) => ({ id, op, r, d: data && toBase64(new Uint8Array(data)) });

  function opened() {
    const s = setup();
    s.relay.open();
    s.relay.accepted(true);
    s.sent.length = 0;
    return s;
  }

  test("a feature get is read from the device and answered with its data", async () => {
    const { relay, device, sent } = opened();
    relay.request(req(11, "get_feature", 1));
    await flush();
    expect(device.log).toEqual(["get 1"]);
    expect(sent).toEqual([{ k: "hid", op: "reply", i: 2, id: 11, ok: true, d: toBase64(new Uint8Array([0x83, 5, 1, 2])) }]);
  });

  test("a feature set and an output set are written, then answered", async () => {
    const { relay, device, sent } = opened();
    relay.request(req(12, "set_feature", 1, [0x87, 3, 9, 0, 0]));
    relay.request(req(13, "set_output", 0x81, [0, 1, 2]));
    await flush();
    expect(device.log).toEqual(["set 1 135,3,9,0,0", "out 129 0,1,2"]);
    expect(sent).toEqual([
      { k: "hid", op: "reply", i: 2, id: 12, ok: true },
      { k: "hid", op: "reply", i: 2, id: 13, ok: true },
    ]);
  });

  test("a command that is not on the allowlist is never written to the controller", async () => {
    const { relay, device, sent } = opened();
    for (let cmd = 0; cmd < 256; cmd++) relay.request(req(cmd + 1, "set_feature", 1, [cmd, 0]));
    await flush();
    await flush();
    const written = device.log.map((l) => Number(l.split(" ")[2]?.split(",")[0]));
    expect(written.sort((a, b) => a - b)).toEqual([...RELAYED_COMMANDS].sort((a, b) => a - b));
    for (const never of [0x90, 0x86, 0x9f, 0xa9, 0xb7, 0xb8, 0xb9, 0xc1, 0xdc, 0xe2, 0xf2]) expect(written).not.toContain(never);
    // Each refusal is answered no, so the streamer's emulation answers.
    const refused = sent.filter((m) => m.ok === false);
    expect(refused).toHaveLength(256 - RELAYED_COMMANDS.size);
    expect(relay.stats.blocked).toBe(256 - RELAYED_COMMANDS.size);
  });

  test("other feature reports and other outputs are not written either", async () => {
    const { relay, device, sent } = opened();
    relay.request(req(1, "set_feature", 2, [0x83, 0]));
    relay.request(req(2, "set_output", 0x86, [0]));
    relay.request(req(0, "output", 0x89, [0]));
    relay.request(req(3, "set_output", 0x85, [0]));
    await flush();
    expect(device.log).toEqual(["out 133 0"]);
    expect(sent.map((m) => [m.id, m.ok])).toEqual([[1, false], [2, false], [3, true]]);
  });

  test("the firmware and bootloader build times Steam reads are newer than any update", async () => {
    const { relay, device, sent } = opened();
    const attrs = [0x83, 25];
    for (const [tag, value] of [[1, 0x1302], [10, 0x68d2f92e], [4, 0x6aa43b55], [9, 0x48], [5, 0x60000000]] as const) {
      attrs.push(tag, value & 0xff, (value >> 8) & 0xff, (value >> 16) & 0xff, (value >>> 24) & 0xff);
    }
    device.feature = new Uint8Array(attrs);
    relay.request(req(1, "get_feature", 1));
    await flush();
    const got = [...fromBase64(sent[0]!.d as string)];
    const time = [0xff, 0xff, 0xff, 0x7f];
    expect(got.slice(0, 2)).toEqual([0x83, 25]);
    expect(got.slice(2)).toEqual([1, 0x02, 0x13, 0, 0, 10, ...time, 4, ...time, 9, 0x48, 0, 0, 0, 5, ...time]);
  });

  test("an interrupt OUT is written and not answered", async () => {
    const { relay, device, sent } = opened();
    relay.request(req(0, "output", 0x80, [0, 1, 0, 0xff, 0xff, 0, 0, 0, 0]));
    await flush();
    expect(device.log).toEqual(["out 128 0,1,0,255,255,0,0,0,0"]);
    expect(sent).toEqual([]);
  });

  test("they run one at a time, in the order they came, however slow the device is", async () => {
    const { relay, device, sent } = opened();
    let release!: () => void;
    device.gate = new Promise((r) => (release = r));
    relay.request(req(1, "set_feature", 1, [0x83]));
    relay.request(req(2, "get_feature", 1));
    relay.request(req(0, "output", 0x80, [9]));
    await flush();
    // The first is in the device; nothing else started.
    expect(device.log).toEqual(["set 1 131"]);
    expect(sent).toEqual([]);
    release();
    await flush();
    await flush();
    expect(device.log).toEqual(["set 1 131", "get 1", "out 128 9"]);
    expect(sent.map((m) => m.id)).toEqual([1, 2]);
  });

  test("a device that fails answers no, and the next request still runs", async () => {
    const { relay, device, sent } = opened();
    device.failing = true;
    relay.request(req(1, "get_feature", 1));
    relay.request(req(2, "set_feature", 1, [0x83]));
    await settle(40);
    expect(sent.map((m) => [m.id, m.ok])).toEqual([
      [1, false],
      [2, false],
    ]);
    // The reason goes up, for the streamer's log; each was tried twice.
    expect(sent[0]!.e).toBe("Error: the device is gone");
    expect(device.log).toEqual(["get 1", "get 1", "set 1 131", "set 1 131"]);
    expect(relay.stats.failed).toBe(2);
    device.failing = false;
    relay.request(req(3, "get_feature", 1));
    await flush();
    expect(sent.at(-1)).toMatchObject({ id: 3, ok: true });
  });

  test("a transfer that fails once is tried again, and the app never hears of it", async () => {
    const { relay, device, sent } = opened();
    let fails = 1;
    const real = device.receiveFeatureReport.bind(device);
    device.receiveFeatureReport = async (id: number) => {
      if (fails-- > 0) {
        device.log.push("busy");
        throw new Error("busy");
      }
      return real(id);
    };
    relay.request(req(4, "get_feature", 1));
    await settle(40);
    expect(device.log).toEqual(["busy", "get 1"]);
    expect(sent).toEqual([{ k: "hid", op: "reply", i: 2, id: 4, ok: true, d: toBase64(new Uint8Array([0x83, 5, 1, 2])) }]);
  });

  test("an op it does not know is answered no", async () => {
    const { relay, sent } = opened();
    relay.request(req(5, "reboot", 1));
    await flush();
    expect(sent).toEqual([{ k: "hid", op: "reply", i: 2, id: 5, ok: false, e: "unknown hid op reboot" }]);
  });

  test("when it is not open any more, the streamer is told no at once", () => {
    const s = setup();
    s.relay.request(req(7, "get_feature", 1));
    expect(s.sent).toEqual([{ k: "hid", op: "reply", i: 2, id: 7, ok: false }]);
    expect(s.device.log).toEqual([]);
    // An output wants no answer.
    s.sent.length = 0;
    s.relay.request(req(0, "output", 0x80, [1]));
    expect(s.sent).toEqual([]);
  });

  test("a request that beats the streamer's yes is performed all the same", async () => {
    const s = setup();
    s.relay.open();
    s.sent.length = 0;
    s.relay.request(req(3, "get_feature", 1));
    await flush();
    expect(s.device.log).toEqual(["get 1"]);
    expect(s.sent[0]).toMatchObject({ op: "reply", id: 3, ok: true });
  });

  test("outputs queued behind a stuck device are dropped past a limit; requests with a reply never are", async () => {
    const { relay, device, sent } = opened();
    device.gate = new Promise(() => {});
    for (let i = 0; i < 100; i++) relay.request(req(0, "output", 0x80, [i]));
    relay.request(req(99, "get_feature", 1));
    expect(relay.stats.droppedOutputs).toBe(100 - 32);
    await flush();
    expect(sent).toEqual([]);
  });
});

describe("the streamer's offer", () => {
  test("the hello's list of vid:pid strings; anything else is no offer", () => {
    expect(parseRawHid(["28de:1302"])).toEqual(["28de:1302"]);
    expect(parseRawHid(undefined)).toEqual([]);
    expect(parseRawHid("28de:1302")).toEqual([]);
    expect(parseRawHid(["28de:1302", 5, null])).toEqual(["28de:1302"]);
  });
});

describe("rewriteBuildTimes", () => {
  test("leaves other replies and short or truncated ones alone", () => {
    const other = new Uint8Array([0x89, 5, 4, 1, 2, 3, 4]);
    expect(rewriteBuildTimes(other)).toBe(false);
    expect([...other]).toEqual([0x89, 5, 4, 1, 2, 3, 4]);
    const truncated = new Uint8Array([0x83, 200, 4, 1, 2, 3, 4, 10, 9, 9]);
    expect(rewriteBuildTimes(truncated)).toBe(true);
    expect([...truncated.slice(3, 7)]).toEqual([0xff, 0xff, 0xff, 0x7f]);
    expect([...truncated.slice(7)]).toEqual([10, 9, 9]);
    expect(rewriteBuildTimes(new Uint8Array())).toBe(false);
    expect(NEVER_UPDATE_TIME).toBe(2 ** 31 - 1);
  });
});

describe("a puck's late feature replies", () => {
  /** A relay of a puck's controller: reads are repeated, 1 ms apart here. */
  function puckSetup() {
    const s = setup();
    const relay = new HidRelay(
      2,
      Object.assign(s.device, { productId: 0x1304 }) as unknown as HidDevice,
      { send: (m) => void s.sent.push(m), sendReport: () => false },
      () => 1,
      () => 0,
      STEAM_LIZARD_REPORTS,
      { attempts: PUCK_GET_RETRY.attempts, delayMs: 1 },
    );
    relay.open();
    relay.accepted(true);
    s.sent.length = 0;
    return { ...s, relay };
  }
  const replies = (sent: Record<string, unknown>[]) => sent.filter((m) => m.op === "reply");
  const get = (id: number) => ({ id, op: "get_feature", r: 1 });
  const set = (id: number, cmd: number) => ({ id, op: "set_feature", r: 1, d: toBase64(new Uint8Array([cmd, 0, 0])) });

  test("a read that fails, or is not the command's, is repeated until it is", async () => {
    const { relay, device, sent } = puckSetup();
    // The first reads fail (the puck is not ready), then a stale reply, then the command's.
    device.script = [new Error("not ready"), new Error("not ready"), new Uint8Array([0, 0, 0, 0]), new Uint8Array([0x83, 5, 9, 9])];
    relay.request(set(1, 0x83));
    relay.request(get(2));
    await settle(60);
    const [ack, answer] = replies(sent);
    expect(ack).toMatchObject({ id: 1, ok: true });
    expect(answer).toMatchObject({ id: 2, ok: true });
    expect([...fromBase64(answer!.d as string)]).toEqual([0x83, 5, 9, 9]);
    expect(device.log.filter((l) => l === "get 1")).toHaveLength(4);
  });

  test("a reply that never echoes the command is the last one read, for the streamer to judge", async () => {
    const { relay, device, sent } = puckSetup();
    device.feature = new Uint8Array([0xee, 1, 2, 3]);
    relay.request(set(1, 0x83));
    relay.request(get(2));
    await settle(60);
    const answer = replies(sent)[1]!;
    expect(answer).toMatchObject({ id: 2, ok: true });
    expect([...fromBase64(answer.d as string)]).toEqual([0xee, 1, 2, 3]);
    expect(device.log.filter((l) => l === "get 1")).toHaveLength(PUCK_GET_RETRY.attempts);
  });

  test("a read that always fails is told no, so the streamer's emulation answers", async () => {
    const { relay, device, sent } = puckSetup();
    device.failing = true;
    relay.request(get(2));
    await settle(80);
    expect(replies(sent)).toEqual([expect.objectContaining({ id: 2, ok: false })]);
  });

  test("a command with no echoing reply is read once, and so is a read of another report", async () => {
    const { relay, device, sent } = puckSetup();
    device.feature = new Uint8Array([0xee, 1, 2, 3]);
    relay.request(set(1, 0x87));
    relay.request(get(2));
    relay.request({ id: 3, op: "get_feature", r: 2 });
    await settle(40);
    expect(device.log.filter((l) => l === "get 1")).toHaveLength(1);
    expect(device.log.filter((l) => l === "get 2")).toHaveLength(1);
    expect(replies(sent).map((m) => m.ok)).toEqual([true, true, true]);
  });

  test("a cable's relay reads once, as before", async () => {
    const { relay, device, sent } = setup();
    relay.open();
    relay.accepted(true);
    device.feature = new Uint8Array([0xee, 1, 2, 3]);
    relay.request(set(1, 0x83));
    relay.request(get(2));
    await settle(40);
    expect(device.log.filter((l) => l === "get 1")).toHaveLength(1);
    expect(replies(sent.filter((m) => m.k === "hid"))).toHaveLength(2);
  });
});
