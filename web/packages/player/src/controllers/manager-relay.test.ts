import { describe, expect, test } from "bun:test";

import { ControllerManager, goneMessage } from "./manager";
import { toBase64 } from "./hid-relay";
import type { HidDevice, RawHidPad } from "./hid-types";
import {
  neutralState,
  type BackendController,
  type BackendListener,
  type ControllerBackend,
  type ControllerInfo,
  type ControllerState,
} from "./types";

/** A WebHID pad that can step aside for a relay: records what the manager asks of its driver. */
class RawPad implements BackendController {
  readonly info: ControllerInfo;
  readonly s: ControllerState = neutralState();
  readonly rawHid: RawHidPad;
  waiting: boolean;
  readonly calls: string[] = [];
  sink: ((reportId: number, data: DataView) => void) | null = null;

  constructor(
    readonly key: string,
    ids: [number, number] = [0x28de, 0x1302],
    deferred = true,
  ) {
    this.waiting = deferred;
    this.info = {
      name: "Steam Controller",
      backend: "webhid",
      type: "steam",
      vendorId: ids[0],
      productId: ids[1],
      capabilities: { rumble: true, gyro: true, touchpad: true, battery: true },
      mapped: true,
    };
    this.rawHid = {
      device: { vendorId: ids[0], productId: ids[1] } as unknown as HidDevice,
      hooks: {
        waiting: () => this.waiting,
        yield: () => {
          this.calls.push("yield");
          this.waiting = true;
        },
        claim: async () => {
          this.calls.push("claim");
          this.waiting = false;
        },
      },
      setSink: (sink) => void (this.sink = sink),
    };
  }
  state() {
    return this.s;
  }
  raw() {
    return null;
  }
  rumble() {}
}

class FakeBackend implements ControllerBackend {
  readonly name = "webhid" as const;
  readonly unavailable = null;
  listener: BackendListener | null = null;
  pads: RawPad[] = [];
  start(l: BackendListener) {
    this.listener = l;
  }
  stop() {
    this.listener = null;
  }
  controllers() {
    return this.pads;
  }
  add(p: RawPad) {
    this.pads.push(p);
    this.listener?.arrived(p);
  }
  remove(p: RawPad) {
    this.pads = this.pads.filter((x) => x !== p);
    this.listener?.removed(p);
  }
}

function setup() {
  const backend = new FakeBackend();
  const sent: Record<string, unknown>[] = [];
  const datagrams: { slot: number; q: number; report: number[] }[] = [];
  const env = { focused: true, control: true, datagrams: true, clock: 0 };
  const manager = new ControllerManager({
    backends: [backend],
    send: (m) => void sent.push(m),
    focused: () => env.focused,
    now: () => env.clock,
    relay: {
      hasControl: () => env.control,
      sendReport: (slot, q, report) => {
        if (!env.datagrams) return false;
        datagrams.push({ slot, q, report: [...report] });
        return true;
      },
    },
  });
  manager.start();
  const ops = () => sent.filter((m) => m.k === "hid").map((m) => m.op);
  const parsed = () => sent.filter((m) => m.k === "pad");
  return { backend, manager, sent, datagrams, env, ops, parsed };
}

const OFFER = ["28de:1302"];

describe("passing a controller through", () => {
  test("waits for the streamer's offer: nothing is sent for the pad and its driver is left alone", () => {
    const { backend, manager, sent, env } = setup();
    const pad = new RawPad("hid:1");
    backend.add(pad);
    pad.s.buttons[0] = 1;
    manager.tick();
    expect(sent).toEqual([]);
    expect(pad.calls).toEqual([]);
    // A hello that never comes is taken as an offer of nothing.
    env.clock = 3001;
    manager.tick();
    expect(pad.calls).toEqual(["claim"]);
    expect(sent.filter((m) => m.k === "pad")).toHaveLength(1);
    manager.stop();
  });

  test("an offer for the controller opens a relay, naming it; parsed state is not sent while it opens", () => {
    const { backend, manager, ops, parsed, sent } = setup();
    const pad = new RawPad("hid:1");
    backend.add(pad);
    manager.rawHidOffered(OFFER);
    expect(sent).toEqual([{ k: "hid", op: "open", i: 0, vid: 0x28de, pid: 0x1302 }]);
    expect(pad.calls).toEqual([]);
    expect(manager.relaying(0)).toBe(true);
    pad.s.buttons[0] = 1;
    manager.tick();
    expect(parsed()).toEqual([]);
    expect(ops()).toEqual(["open"]);
    manager.stop();
  });

  test("once the streamer says yes the page's own lizard handling steps aside and reports go up as they come", () => {
    const { backend, manager, datagrams, parsed } = setup();
    const pad = new RawPad("hid:1");
    backend.add(pad);
    manager.rawHidOffered(OFFER);
    // A report that arrives while it opens is kept for the answer.
    pad.sink!(0x42, new DataView(new Uint8Array([1, 2]).buffer));
    manager.hidOpened(0, true);
    expect(pad.calls).toEqual(["yield"]);
    expect(datagrams).toEqual([{ slot: 0, q: 1, report: [0x42, 1, 2] }]);
    pad.sink!(0x42, new DataView(new Uint8Array([3, 4]).buffer));
    expect(datagrams.at(-1)).toEqual({ slot: 0, q: 2, report: [0x42, 3, 4] });
    // And no parsed state, however the pad changes.
    pad.s.buttons[0] = 1;
    manager.tick();
    manager.tick();
    expect(parsed()).toEqual([]);
    manager.stop();
  });

  test("the streamer's refusal gives the pad back to its driver and the parsed path, and is not retried", () => {
    const { backend, manager, ops, parsed } = setup();
    const pad = new RawPad("hid:1");
    backend.add(pad);
    manager.rawHidOffered(OFFER);
    manager.hidOpened(0, false, "a player holds that pad");
    expect(pad.calls).toEqual(["claim"]);
    expect(manager.relaying(0)).toBe(false);
    pad.s.buttons[0] = 1;
    manager.tick();
    expect(parsed()).toHaveLength(1);
    manager.resync();
    manager.tick();
    expect(ops()).toEqual(["open"]);
    // A new offer (a reconnect) tries again.
    manager.rawHidOffered(OFFER);
    expect(ops()).toEqual(["open", "open"]);
    manager.stop();
  });

  test("an offer of nothing, or of another model, leaves the pad on the parsed path", () => {
    for (const offer of [[], ["28de:1303"], ["045e:028e"]]) {
      const { backend, manager, ops, parsed } = setup();
      const pad = new RawPad("hid:1");
      backend.add(pad);
      manager.rawHidOffered(offer);
      expect(pad.calls).toEqual(["claim"]);
      pad.s.buttons[0] = 1;
      manager.tick();
      expect(parsed()).toHaveLength(1);
      expect(ops()).toEqual([]);
      manager.stop();
    }
  });

  test("another controller of a model the streamer doesn't carry stays parsed beside a relayed one", () => {
    const { backend, manager, parsed } = setup();
    const steam = new RawPad("hid:1");
    const other = new RawPad("hid:2", [0x28de, 0x1303]);
    backend.add(steam);
    backend.add(other);
    manager.rawHidOffered(OFFER);
    manager.hidOpened(0, true);
    steam.s.buttons[0] = 1;
    other.s.buttons[0] = 1;
    manager.tick();
    expect(parsed().map((m) => m.i)).toEqual([1]);
    manager.stop();
  });

  test("nothing opens while the page lacks the controls or the focus, and it opens when it has both", () => {
    const { backend, manager, ops, env } = setup();
    env.control = false;
    const pad = new RawPad("hid:1");
    backend.add(pad);
    manager.rawHidOffered(OFFER);
    expect(ops()).toEqual([]);
    // Not passed through, and the offer is known: the driver has the pad.
    expect(pad.calls).toEqual(["claim"]);
    env.control = true;
    manager.resync();
    expect(ops()).toEqual(["open"]);
    manager.stop();
  });

  test("losing the focus ends the relay and puts the pad at rest; the focus back opens it again", () => {
    const { backend, manager, ops, sent, env } = setup();
    const pad = new RawPad("hid:1");
    backend.add(pad);
    manager.rawHidOffered(OFFER);
    manager.hidOpened(0, true);
    pad.calls.length = 0;
    sent.length = 0;
    env.focused = false;
    manager.tick();
    expect(sent.map((m) => m.op ?? m.k)).toEqual(["close", "pad"]);
    expect(sent[1]).toMatchObject({ gone: true });
    expect(pad.sink).toBeNull();
    expect(pad.calls).toEqual(["claim"]);
    sent.length = 0;
    env.focused = true;
    manager.tick();
    expect(ops()).toEqual(["open"]);
    manager.hidOpened(0, true);
    expect(pad.calls).toEqual(["claim", "yield"]);
    manager.stop();
  });

  test("losing the controls ends the relay; gaining them opens it again", () => {
    const { backend, manager, ops, env } = setup();
    const pad = new RawPad("hid:1");
    backend.add(pad);
    manager.rawHidOffered(OFFER);
    manager.hidOpened(0, true);
    env.control = false;
    manager.floorLost();
    expect(manager.relaying(0)).toBe(false);
    expect(pad.calls).toEqual(["yield", "claim"]);
    env.control = true;
    manager.resync();
    expect(ops()).toEqual(["open", "close", "open"]);
    manager.stop();
  });

  test("a controller that leaves takes its relay with it, and is put at rest", () => {
    const { backend, manager, sent } = setup();
    const pad = new RawPad("hid:1");
    backend.add(pad);
    manager.rawHidOffered(OFFER);
    manager.hidOpened(0, true);
    sent.length = 0;
    backend.remove(pad);
    expect(manager.relaying(0)).toBe(false);
    expect(pad.sink).toBeNull();
    expect(sent).toContainEqual(goneMessage(0, 1));
    expect(sent.filter((m) => m.op === "close")).toHaveLength(1);
    // The slot can be used again by the next one.
    const next = new RawPad("hid:2");
    backend.add(next);
    expect(sent.at(-1)).toMatchObject({ op: "open", i: 0 });
    manager.stop();
  });

  test("stopping closes the relay and puts the slot at rest, without taking the pad over", () => {
    const { backend, manager, sent } = setup();
    const pad = new RawPad("hid:1");
    backend.add(pad);
    manager.rawHidOffered(OFFER);
    manager.hidOpened(0, true);
    pad.calls.length = 0;
    sent.length = 0;
    manager.stop();
    expect(sent.map((m) => m.op ?? m.k)).toEqual(["close", "pad"]);
    expect(pad.calls).toEqual([]);
  });

  test("a hid_open for nothing we asked is ignored", () => {
    const { manager } = setup();
    manager.hidOpened(2, true);
    manager.hidOpened(0, false);
    manager.stop();
  });

  test("the app's requests reach the relay of that slot; those for a slot not relayed are answered no at once", () => {
    const { backend, manager, sent } = setup();
    const pad = new RawPad("hid:1");
    backend.add(pad);
    manager.rawHidOffered(OFFER);
    manager.hidOpened(0, true);
    sent.length = 0;
    manager.hidRequest(3, { id: 9, op: "get_feature", r: 1 });
    expect(sent).toEqual([{ k: "hid", op: "reply", i: 3, id: 9, ok: false }]);
    // An output needs no answer.
    sent.length = 0;
    manager.hidRequest(3, { id: 0, op: "output", r: 0x80, d: toBase64(new Uint8Array([1])) });
    expect(sent).toEqual([]);
    manager.stop();
  });

  test("without the option the manager never relays, whatever the streamer offers", () => {
    const backend = new FakeBackend();
    const sent: Record<string, unknown>[] = [];
    const manager = new ControllerManager({ backends: [backend], send: (m) => void sent.push(m) });
    manager.start();
    const pad = new RawPad("hid:1", [0x28de, 0x1302], false);
    backend.add(pad);
    manager.rawHidOffered(OFFER);
    pad.s.buttons[0] = 1;
    manager.tick();
    expect(sent.map((m) => m.k)).toEqual(["pad"]);
    expect(pad.calls).toEqual([]);
    manager.stop();
  });
});

describe("passing controllers behind a puck through", () => {
  const PUCK = [0x28de, 0x1304] as [number, number];
  const BOTH = ["28de:1302", "28de:1304"];

  test("each controller behind the puck gets a relay in its own slot, naming the puck", () => {
    const { backend, manager, sent, parsed } = setup();
    const a = new RawPad("hid:1", PUCK);
    const b = new RawPad("hid:2", PUCK);
    backend.add(a);
    backend.add(b);
    manager.rawHidOffered(BOTH);
    expect(sent).toEqual([
      { k: "hid", op: "open", i: 0, vid: 0x28de, pid: 0x1304 },
      { k: "hid", op: "open", i: 1, vid: 0x28de, pid: 0x1304 },
    ]);
    manager.hidOpened(0, true);
    manager.hidOpened(1, true);
    expect([a.calls, b.calls]).toEqual([["yield"], ["yield"]]);
    // Their reports go up under their slots, and nothing parsed is sent for either.
    a.sink!(0x42, new DataView(new Uint8Array([1]).buffer));
    b.sink!(0x42, new DataView(new Uint8Array([2]).buffer));
    a.s.buttons[0] = 1;
    b.s.buttons[0] = 1;
    manager.tick();
    expect(parsed()).toEqual([]);
    manager.stop();
  });

  test("an offer of the cable only leaves puck controllers on the parsed path", () => {
    const { backend, manager, ops, parsed } = setup();
    const pad = new RawPad("hid:1", PUCK);
    backend.add(pad);
    manager.rawHidOffered(OFFER);
    expect(pad.calls).toEqual(["claim"]);
    pad.s.buttons[0] = 1;
    manager.tick();
    expect(parsed()).toHaveLength(1);
    expect(ops()).toEqual([]);
    manager.stop();
  });

  test("one controller leaving the puck closes its relay and no other; a new one takes the free slot", () => {
    const { backend, manager, sent } = setup();
    const a = new RawPad("hid:1", PUCK);
    const b = new RawPad("hid:2", PUCK);
    backend.add(a);
    backend.add(b);
    manager.rawHidOffered(BOTH);
    manager.hidOpened(0, true);
    manager.hidOpened(1, true);
    sent.length = 0;
    backend.remove(a);
    expect(manager.relaying(0)).toBe(false);
    expect(manager.relaying(1)).toBe(true);
    expect(sent.filter((m) => m.op === "close")).toEqual([{ k: "hid", op: "close", i: 0 }]);
    expect(sent.filter((m) => m.k === "pad")).toEqual([goneMessage(0, 1)]);
    // The next one to wake behind the puck is slot 0 again.
    const c = new RawPad("hid:3", PUCK);
    backend.add(c);
    expect(sent.at(-1)).toEqual({ k: "hid", op: "open", i: 0, vid: 0x28de, pid: 0x1304 });
    manager.stop();
  });

  test("a cable controller and a puck one relay side by side, each under the model it is", () => {
    const { backend, manager, sent } = setup();
    backend.add(new RawPad("hid:1"));
    backend.add(new RawPad("hid:2", PUCK));
    manager.rawHidOffered(BOTH);
    expect(sent.map((m) => [m.i, m.pid])).toEqual([
      [0, 0x1302],
      [1, 0x1304],
    ]);
    manager.stop();
  });
});
