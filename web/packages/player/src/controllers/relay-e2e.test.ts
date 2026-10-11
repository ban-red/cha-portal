// A Steam Controller on a cable through the real pieces (WebHID backend, Triton driver, manager,
// relay) with a fake `navigator.hid`: what the page does with the option off and on.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { fromBase64, toBase64 } from "./hid-relay";
import { ControllerManager } from "./manager";

class FakeTriton extends EventTarget {
  vendorId = 0x28de;
  productId = 0x1302;
  productName = "Steam Controller";
  collections = [];
  opened = false;
  /** Everything written to it, in order. */
  readonly writes: string[] = [];
  feature = new Uint8Array([0x83, 7, 1, 2, 3]);

  async open() {
    this.opened = true;
  }
  async close() {
    this.opened = false;
  }
  async sendFeatureReport(id: number, data: BufferSource) {
    this.writes.push(`feature ${id} ${[...new Uint8Array(data as ArrayBuffer).slice(0, 5)].join(",")}`);
  }
  async sendReport(id: number, data: BufferSource) {
    const v = data as DataView;
    this.writes.push(`out ${id} ${[...new Uint8Array(v.buffer, v.byteOffset, v.byteLength)].join(",")}`);
  }
  async receiveFeatureReport(id: number) {
    this.writes.push(`get ${id}`);
    return new DataView(this.feature.buffer.slice(0));
  }
  /** The controller sends an input report. */
  input(reportId: number, bytes: number[]) {
    const e = Object.assign(new Event("inputreport"), { device: this, reportId, data: new DataView(new Uint8Array(bytes).buffer) });
    this.dispatchEvent(e);
  }
}

const g = globalThis as Record<string, unknown>;
let saved: { navigator: unknown; isSecureContext: unknown };
let device: FakeTriton;

beforeEach(() => {
  saved = { navigator: g.navigator, isSecureContext: g.isSecureContext };
  device = new FakeTriton();
  const hid = {
    getDevices: async () => [device],
    requestDevice: async () => [],
    addEventListener() {},
    removeEventListener() {},
  };
  Object.defineProperty(globalThis, "navigator", { value: { hid, getGamepads: () => [] }, configurable: true });
  Object.defineProperty(globalThis, "isSecureContext", { value: true, configurable: true });
});

afterEach(() => {
  Object.defineProperty(globalThis, "navigator", { value: saved.navigator, configurable: true });
  Object.defineProperty(globalThis, "isSecureContext", { value: saved.isSecureContext, configurable: true });
});

const settle = () => new Promise((r) => setTimeout(r, 20));
const state = (buttons: number) => {
  const d = new Uint8Array(53);
  new DataView(d.buffer).setUint32(1, buttons, true);
  return [...d];
};

function pageWith(relay: boolean) {
  const sent: Record<string, unknown>[] = [];
  const datagrams: number[][] = [];
  const manager = new ControllerManager({
    send: (m) => void sent.push(m),
    focused: () => true,
    relay: relay
      ? {
          hasControl: () => true,
          sendReport: (_slot, _q, report) => (datagrams.push([...report]), true),
        }
      : undefined,
  });
  manager.start();
  return { manager, sent, datagrams };
}

describe("a wired Steam Controller through the page", () => {
  test("option off: the page claims it and sends parsed state (as before)", async () => {
    const { manager, sent } = pageWith(false);
    await settle();
    expect(device.writes.map((w) => w.slice(0, 13))).toEqual(["feature 1 135", "feature 1 135"]);
    manager.rawHidOffered(["28de:1302"]);
    device.input(0x42, state(0x1));
    await settle();
    expect(sent.filter((m) => m.k === "pad").at(-1)).toMatchObject({ i: 0, b: expect.arrayContaining([1]) });
    expect(sent.filter((m) => m.k === "hid")).toEqual([]);
    manager.stop();
  });

  test("option on, streamer offers it: the page writes nothing itself, asks to relay, and forwards reports raw", async () => {
    const { manager, sent, datagrams } = pageWith(true);
    await settle();
    expect(device.writes).toEqual([]);
    manager.rawHidOffered(["28de:1302"]);
    expect(sent).toEqual([{ k: "hid", op: "open", i: 0, vid: 0x28de, pid: 0x1302 }]);
    manager.hidOpened(0, true);
    device.input(0x42, state(0x1));
    device.input(0x43, [1, 80]);
    await settle();
    expect(datagrams).toEqual([[0x42, ...state(0x1)], [0x43, 1, 80]]);
    // No parsed state for the slot, ever, and still nothing written by the page.
    manager.tick();
    expect(sent.filter((m) => m.k === "pad")).toEqual([]);
    expect(device.writes).toEqual([]);
    manager.stop();
  });

  test("Steam's feature traffic is performed on the real device and answered", async () => {
    const { manager, sent } = pageWith(true);
    await settle();
    manager.rawHidOffered(["28de:1302"]);
    manager.hidOpened(0, true);
    sent.length = 0;
    // SET_REPORT feature 1 (a command), then GET_REPORT feature 1 (its reply), then an interrupt OUT.
    manager.hidRequest(0, { id: 5, op: "set_feature", r: 1, d: toBase64(new Uint8Array([0x83, 0, 0, 0])) });
    manager.hidRequest(0, { id: 6, op: "get_feature", r: 1 });
    manager.hidRequest(0, { id: 0, op: "output", r: 0x80, d: toBase64(new Uint8Array([0, 1, 0, 255, 255, 0, 0, 0, 0])) });
    await settle();
    expect(device.writes).toEqual(["feature 1 131,0,0,0", "get 1", "out 128 0,1,0,255,255,0,0,0,0"]);
    expect(sent.map((m) => [m.op, m.id, m.ok])).toEqual([
      ["reply", 5, true],
      ["reply", 6, true],
    ]);
    expect(fromBase64(sent[1]!.d as string)).toEqual(new Uint8Array([0x83, 7, 1, 2, 3]));
    manager.stop();
  });

  test("option on, nothing offered (old streamer, uhid transport): the page claims it and sends parsed state", async () => {
    const { manager, sent } = pageWith(true);
    await settle();
    manager.rawHidOffered([]);
    await settle();
    expect(device.writes).toHaveLength(2);
    device.input(0x42, state(0x1));
    await settle();
    expect(sent.filter((m) => m.k === "pad").at(-1)).toMatchObject({ i: 0, b: expect.arrayContaining([1]) });
    expect(sent.filter((m) => m.k === "hid")).toEqual([]);
    manager.stop();
  });

  test("option on, refused: the page claims it and sends parsed state", async () => {
    const { manager, sent } = pageWith(true);
    await settle();
    manager.rawHidOffered(["28de:1302"]);
    manager.hidOpened(0, false, "this environment's pads can't carry that controller");
    await settle();
    expect(device.writes).toHaveLength(2);
    device.input(0x42, state(0x1));
    await settle();
    expect(sent.filter((m) => m.k === "pad").at(-1)).toMatchObject({ i: 0, b: expect.arrayContaining([1]) });
    manager.stop();
  });

  test("leaving: the relay closes, the slot goes to rest, and the page doesn't touch the controller", async () => {
    const { manager, sent } = pageWith(true);
    await settle();
    manager.rawHidOffered(["28de:1302"]);
    manager.hidOpened(0, true);
    sent.length = 0;
    manager.stop();
    await settle();
    expect(sent.map((m) => m.op ?? m.k)).toEqual(["close", "pad"]);
    expect(sent[1]).toMatchObject({ gone: true });
    expect(device.writes).toEqual([]);
  });
});
