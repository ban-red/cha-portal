import { describe, expect, test } from "bun:test";

import { ControllerManager, REST_RESENDS, RESEND_MS, goneMessage, isHeld, padMessage } from "./manager";
import { WebHidBackend } from "./webhid";
import {
  neutralState,
  type BackendController,
  type BackendListener,
  type BackendName,
  type ControllerBackend,
  type ControllerInfo,
  type ControllerState,
} from "./types";

class FakePad implements BackendController {
  readonly info: ControllerInfo;
  readonly s: ControllerState = neutralState();
  rumbled: number[][] = [];
  calls: unknown[][] = [];
  haptic?: BackendController["haptic"];
  led?: BackendController["led"];
  players?: BackendController["players"];
  trigger?: BackendController["trigger"];
  /** Makes the pad do the feedback the streamer can send: recorded in `calls`. */
  feedback() {
    this.haptic = (...a) => void this.calls.push(["haptic", ...a]);
    this.led = (...a) => void this.calls.push(["led", ...a]);
    this.players = (...a) => void this.calls.push(["players", ...a]);
    this.trigger = (...a) => void this.calls.push(["trigger", ...a]);
    return this;
  }
  constructor(
    readonly key: string,
    backend: BackendName,
    ids?: [number, number],
    type: ControllerInfo["type"] = "generic",
  ) {
    this.info = {
      name: key,
      backend,
      type,
      vendorId: ids?.[0],
      productId: ids?.[1],
      capabilities: { rumble: true, gyro: false, touchpad: false, battery: false },
      mapped: true,
    };
  }
  state() {
    return this.s;
  }
  raw() {
    return null;
  }
  rumble(lo: number, hi: number, ms: number) {
    this.rumbled.push([lo, hi, ms]);
  }
}

class FakeBackend implements ControllerBackend {
  readonly unavailable = null;
  listener: BackendListener | null = null;
  pads: FakePad[] = [];
  constructor(readonly name: BackendName) {}
  start(l: BackendListener) {
    this.listener = l;
  }
  stop() {
    for (const p of this.pads) this.listener?.removed(p);
    this.listener = null;
  }
  controllers() {
    return this.pads;
  }
  add(p: FakePad) {
    this.pads.push(p);
    this.listener?.arrived(p);
  }
  remove(p: FakePad) {
    this.pads = this.pads.filter((x) => x !== p);
    this.listener?.removed(p);
  }
}

function setup(focused = () => true) {
  const gamepad = new FakeBackend("gamepad");
  const hid = new FakeBackend("webhid");
  const sent: Record<string, unknown>[] = [];
  const manager = new ControllerManager({ backends: [gamepad, hid], send: (m) => sent.push(m), focused });
  manager.start();
  return { gamepad, hid, sent, manager };
}

describe("slots", () => {
  test("pads take the lowest free slot and keep it", () => {
    const { gamepad, manager } = setup();
    const [a, b, c] = ["a", "b", "c"].map((k) => new FakePad(k, "gamepad"));
    gamepad.add(a!);
    gamepad.add(b!);
    gamepad.add(c!);
    expect(manager.controllers().map((x) => [x.id, x.slot])).toEqual([
      ["a", 0],
      ["b", 1],
      ["c", 2],
    ]);
    gamepad.remove(b!);
    expect(manager.controllers().map((x) => [x.id, x.slot])).toEqual([
      ["a", 0],
      ["c", 2],
    ]);
    const d = new FakePad("d", "gamepad");
    gamepad.add(d);
    expect(manager.controllers().find((x) => x.id === "d")?.slot).toBe(1);
    expect(manager.controllers().find((x) => x.id === "c")?.slot).toBe(2);
    manager.stop();
  });

  test("a fifth pad has no slot, and sends nothing", () => {
    const { gamepad, manager, sent } = setup();
    const pads = [0, 1, 2, 3, 4].map((i) => new FakePad(`p${i}`, "gamepad"));
    pads.forEach((p) => gamepad.add(p));
    expect(manager.controllers().map((x) => x.slot)).toEqual([0, 1, 2, 3, null]);
    pads[4]!.s.buttons[0] = 1;
    manager.tick();
    expect(sent).toHaveLength(4);
    expect(sent.every((m) => (m.i as number) < 4)).toBe(true);
    manager.stop();
  });

  test("a removed pad is released, and its slot reused", () => {
    const { gamepad, manager, sent } = setup();
    const a = new FakePad("a", "gamepad");
    gamepad.add(a);
    manager.tick();
    sent.length = 0;
    gamepad.remove(a);
    expect(sent).toEqual([goneMessage(0, 2)]);
    gamepad.add(new FakePad("b", "gamepad"));
    expect(manager.controllers()[0]?.slot).toBe(0);
    manager.stop();
  });
});

describe("dedupe", () => {
  test("a pad both backends see counts once, as the WebHID one", () => {
    const { gamepad, hid, manager } = setup();
    const viaApi = new FakePad("gamepad:0", "gamepad", [0x2dc8, 0x6101]);
    gamepad.add(viaApi);
    expect(manager.controllers().map((x) => x.info.backend)).toEqual(["gamepad"]);
    hid.add(new FakePad("hid:1", "webhid", [0x2dc8, 0x6101]));
    expect(manager.controllers().map((x) => [x.id, x.info.backend])).toEqual([["hid:1", "webhid"]]);
    manager.stop();
  });

  test("two of the same model: one through each backend", () => {
    const { gamepad, hid, manager } = setup();
    gamepad.add(new FakePad("g0", "gamepad", [0x2dc8, 0x6101]));
    gamepad.add(new FakePad("g1", "gamepad", [0x2dc8, 0x6101]));
    hid.add(new FakePad("h0", "webhid", [0x2dc8, 0x6101]));
    expect(manager.controllers().map((x) => x.info.backend).sort()).toEqual(["gamepad", "webhid"]);
    manager.stop();
  });

  test("a pad of another model stays", () => {
    const { gamepad, hid, manager } = setup();
    gamepad.add(new FakePad("g0", "gamepad", [0x045e, 0x028e]));
    hid.add(new FakePad("h0", "webhid", [0x2dc8, 0x6101]));
    expect(manager.controllers()).toHaveLength(2);
    manager.stop();
  });

  test("a Gamepad API pad without ids matches the HID one by name", () => {
    const { gamepad, hid, manager } = setup();
    const api = new FakePad("g0", "gamepad", undefined, "steam");
    Object.assign(api.info, { name: "Steam Controller" });
    gamepad.add(api);
    const h = new FakePad("h0", "webhid", [0x28de, 0x1302], "steam");
    Object.assign(h.info, { name: "Steam Controller" });
    hid.add(h);
    expect(manager.controllers().map((x) => x.id)).toEqual(["h0"]);
    manager.stop();
  });

  test("a Steam Controller the Gamepad API shows under other ids and a name of its own is the HID one's twin", () => {
    const { gamepad, hid, manager, sent } = setup();
    const h = new FakePad("h0", "webhid", [0x28de, 0x1302], "steam");
    Object.assign(h.info, { name: "Steam Controller" });
    hid.add(h);
    // After a replug macOS shows it in lizard mode: other product id, other name.
    const api = new FakePad("g0", "gamepad", [0x28de, 0x1303], "steam");
    Object.assign(api.info, { name: "Valve Steam Controller (2026)" });
    gamepad.add(api);
    manager.tick();
    expect(manager.controllers().map((x) => [x.id, x.slot])).toEqual([["h0", 0]]);
    // Nothing was sent for a slot 1, whatever the twin does.
    api.s.buttons[0] = 1;
    manager.tick();
    expect(sent.every((m) => m.i === 0)).toBe(true);
    // Two Steam Controllers through WebHID are two.
    hid.add(new FakePad("h1", "webhid", [0x28de, 0x1302], "steam"));
    expect(manager.controllers().map((x) => x.id)).toEqual(["h0", "h1"]);
    manager.stop();
  });

  test("a Gamepad API pad without ids and another name stays", () => {
    const { gamepad, hid, manager } = setup();
    gamepad.add(new FakePad("Wireless Controller", "gamepad"));
    hid.add(new FakePad("h0", "webhid", [0x28de, 0x1302], "steam"));
    expect(manager.controllers()).toHaveLength(2);
    manager.stop();
  });

  test("the Gamepad API's pad returns when the HID one leaves", () => {
    const { gamepad, hid, manager } = setup();
    gamepad.add(new FakePad("g0", "gamepad", [0x2dc8, 0x6101]));
    const h = new FakePad("h0", "webhid", [0x2dc8, 0x6101]);
    hid.add(h);
    hid.remove(h);
    expect(manager.controllers().map((x) => x.id)).toEqual(["g0"]);
    manager.stop();
  });
});

describe("wire messages", () => {
  test("shape: standard layout, rounded, with the extras only when there are some", () => {
    const info = new FakePad("p", "webhid", [0x28de, 0x1302], "steam").info;
    const plain = padMessage(2, info, { buttons: new Array(17).fill(0.12345), axes: [0.5, -0.5, 0, 1] });
    expect(plain).toEqual({ k: "pad", i: 2, b: new Array(17).fill(0.123), a: [0.5, -0.5, 0, 1], ty: "steam" });
    const rich = padMessage(0, info, {
      ...neutralState(),
      gyro: [0.1234567, 0, 0],
      accel: [0, 9.80665, 0],
      touch: [
        { id: 0, x: 0.12345, y: 0.5, down: true },
        { id: 1, x: 0.5, y: 0.5, down: false },
      ],
      battery: 0.756,
    });
    expect(rich.gyro).toEqual([0.123, 0, 0]);
    expect(rich.accel).toEqual([0, 9.807, 0]);
    expect(rich.touch).toEqual([{ id: 0, x: 0.123, y: 0.5, down: true }]);
    expect(rich.bat).toBe(0.76);
    expect((rich.b as number[]).length).toBe(24);
    expect((rich.a as number[]).length).toBe(4);
    // `t` is the control channel's message type: a pad's type can't use it.
    expect("t" in rich).toBe(false);
    expect(goneMessage(3)).toEqual({ k: "pad", i: 3, gone: true });
  });

  test("sent on change only, and the motion's noise is not a change", () => {
    const { gamepad, manager, sent } = setup();
    const p = new FakePad("a", "gamepad");
    p.s.gyro = [0, 0, 0];
    p.s.accel = [0, 9.8, 0];
    gamepad.add(p);
    manager.tick();
    manager.tick();
    expect(sent).toHaveLength(1);
    p.s.gyro = [0.001, 0, 0];
    manager.tick();
    expect(sent).toHaveLength(1);
    p.s.gyro = [0.5, 0, 0];
    manager.tick();
    expect(sent).toHaveLength(2);
    p.s.buttons[0] = 1;
    manager.tick();
    expect(sent).toHaveLength(3);
    manager.resync();
    manager.tick();
    expect(sent).toHaveLength(4);
    manager.stop();
  });

  test("an unfocused page lets go, and sends again on return", () => {
    let focus = true;
    const { gamepad, manager, sent } = setup(() => focus);
    gamepad.add(new FakePad("a", "gamepad"));
    manager.tick();
    sent.length = 0;
    focus = false;
    manager.tick();
    manager.tick();
    expect(sent).toEqual([goneMessage(0, 2)]);
    focus = true;
    manager.tick();
    expect(sent).toHaveLength(2);
    expect(sent[1]).toMatchObject({ k: "pad", i: 0 });
    manager.stop();
  });

  test("stopping releases every pad", () => {
    const { gamepad, manager, sent } = setup();
    gamepad.add(new FakePad("a", "gamepad"));
    gamepad.add(new FakePad("b", "gamepad"));
    manager.tick();
    sent.length = 0;
    manager.stop();
    expect(sent).toEqual([goneMessage(0, 2), goneMessage(1, 2)]);
  });
});

describe("rumble", () => {
  test("goes to the pad in that slot, clamped", () => {
    const { gamepad, manager } = setup();
    const a = new FakePad("a", "gamepad");
    const b = new FakePad("b", "gamepad");
    gamepad.add(a);
    gamepad.add(b);
    manager.rumble(1, 2, -1, 250);
    expect(a.rumbled).toEqual([]);
    expect(b.rumbled).toEqual([[1, 0, 250]]);
    manager.rumble(3, 1, 1, 100);
    manager.stop();
  });
});

describe("WebHID availability", () => {
  test("a working WebHID backend gives no reason, and none gives one", () => {
    const g = globalThis as Record<string, unknown>;
    const saved = { navigator: g.navigator, isSecureContext: g.isSecureContext };
    const hid = { getDevices: async () => [], requestDevice: async () => [], addEventListener() {}, removeEventListener() {} };
    Object.defineProperty(globalThis, "navigator", { value: { hid }, configurable: true });
    Object.defineProperty(globalThis, "isSecureContext", { value: true, configurable: true });
    try {
      const backend = new WebHidBackend();
      expect(backend.unavailable).toBeNull();
      expect(new ControllerManager({ backends: [backend] }).hidUnavailable).toBeNull();
      expect(new ControllerManager({ backends: [] }).hidUnavailable).toBe("WebHID isn't available.");
    } finally {
      Object.defineProperty(globalThis, "navigator", { value: saved.navigator, configurable: true });
      Object.defineProperty(globalThis, "isSecureContext", { value: saved.isSecureContext, configurable: true });
    }
  });
});

describe("feedback from the streamer", () => {
  test("goes to the pad in the slot that can do it, and is cleaned up", () => {
    const { gamepad, manager } = setup();
    const a = new FakePad("a", "gamepad");
    const b = new FakePad("b", "gamepad").feedback();
    gamepad.add(a);
    gamepad.add(b);
    manager.led(1, 300, -5, 12.4);
    manager.players(1, 0xff);
    manager.trigger(1, "right", [2, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    manager.trigger(1, "left", [1, 2]);
    manager.haptic(1, "left", 2, 100, 200, 0);
    manager.led(0, 1, 2, 3);
    manager.led(3, 1, 2, 3);
    expect(b.calls).toEqual([
      ["led", 255, 0, 12],
      ["players", 0x1f],
      ["trigger", "right", [2, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]],
      ["haptic", "left", 1, 100, 200, 1],
    ]);
    manager.stop();
  });

  test("a pulse without trackpads is a short rumble", () => {
    const { gamepad, manager } = setup();
    const a = new FakePad("a", "gamepad");
    gamepad.add(a);
    manager.haptic(0, "right", 1, 1000, 1000, 5);
    // Weak motor for the tick, 10 ms of train stretched to the shortest rumble.
    expect(a.rumbled).toEqual([[0.4, 1, 20]]);
    manager.haptic(0, "left", 0, 1000, 1000, 5);
    expect(a.rumbled).toHaveLength(1);
    manager.stop();
  });
});

/** A manager whose pad state can go by datagram, on a clock the test moves. */
function datagramSetup(opts: { datagrams?: () => boolean; focused?: () => boolean } = {}) {
  const gamepad = new FakeBackend("gamepad");
  const hid = new FakeBackend("webhid");
  const reliable: Record<string, unknown>[] = [];
  const datagrams: Record<string, unknown>[] = [];
  let now = 1000;
  const manager = new ControllerManager({
    backends: [gamepad, hid],
    send: (m) => reliable.push(m),
    sendUnreliable: (m) => {
      if (opts.datagrams && !opts.datagrams()) return false;
      datagrams.push(m);
      return true;
    },
    now: () => now,
    focused: opts.focused ?? (() => true),
  });
  manager.start();
  return { gamepad, hid, reliable, datagrams, manager, advance: (ms: number) => (now += ms) };
}

describe("unreliable pad state", () => {
  test("state goes by datagram, numbered per pad; gone goes reliably, numbered after it", () => {
    const { gamepad, reliable, datagrams, manager } = datagramSetup();
    const a = new FakePad("a", "gamepad");
    const b = new FakePad("b", "gamepad");
    gamepad.add(a);
    gamepad.add(b);
    manager.tick();
    expect(datagrams.map((m) => [m.i, m.q])).toEqual([
      [0, 1],
      [1, 1],
    ]);
    a.s.buttons[0] = 1;
    manager.tick();
    expect(datagrams.map((m) => [m.i, m.q])).toEqual([
      [0, 1],
      [1, 1],
      [0, 2],
    ]);
    expect(reliable).toEqual([]);
    gamepad.remove(a);
    expect(reliable).toEqual([goneMessage(0, 3)]);
    // The pad that comes back is newer than anything still in flight from before.
    gamepad.add(new FakePad("c", "gamepad"));
    manager.tick();
    expect(datagrams.at(-1)).toMatchObject({ i: 0, q: 4 });
    manager.stop();
  });

  test("a held pad is sent again every RESEND_MS with the same number", () => {
    const { gamepad, datagrams, manager, advance } = datagramSetup();
    const a = new FakePad("a", "gamepad");
    a.s.buttons[0] = 1;
    gamepad.add(a);
    manager.tick();
    expect(datagrams).toHaveLength(1);
    advance(RESEND_MS - 1);
    manager.tick();
    expect(datagrams).toHaveLength(1);
    advance(1);
    manager.tick();
    expect(datagrams).toHaveLength(2);
    expect(datagrams[1]).toEqual(datagrams[0]);
    // Held for a long time: still going, a datagram per RESEND_MS.
    for (let i = 0; i < 20; i++) {
      advance(RESEND_MS);
      manager.tick();
    }
    expect(datagrams).toHaveLength(22);
    // Let go: the release is sent at once and then repeated, so a lost one can't leave the button down.
    a.s.buttons[0] = 0;
    manager.tick();
    expect(datagrams.at(-1)).toMatchObject({ q: 2 });
    const before = datagrams.length;
    for (let i = 0; i < REST_RESENDS + 5; i++) {
      advance(RESEND_MS);
      manager.tick();
    }
    expect(datagrams.length).toBe(before + REST_RESENDS);
    expect(datagrams.slice(before).every((m) => m.q === 2)).toBe(true);
    manager.stop();
  });

  test("a stick off centre counts as held; drift and a sleeping pad don't", () => {
    expect(isHeld(neutralState())).toBe(false);
    expect(isHeld({ ...neutralState(), axes: [0.1, -0.1, 0, 0] })).toBe(false);
    expect(isHeld({ ...neutralState(), axes: [0, 0, 0.5, 0] })).toBe(true);
    expect(isHeld({ ...neutralState(), buttons: [...new Array(7).fill(0), 0.3, ...new Array(16).fill(0)] })).toBe(true);
    expect(isHeld({ ...neutralState(), touch: [{ id: 0, x: 0.5, y: 0.5, down: true }] })).toBe(true);
  });

  test("a new state replaces the one being resent", () => {
    const { gamepad, datagrams, manager, advance } = datagramSetup();
    const a = new FakePad("a", "gamepad");
    a.s.buttons[0] = 1;
    gamepad.add(a);
    manager.tick();
    advance(RESEND_MS);
    a.s.buttons[1] = 1;
    manager.tick();
    // The change goes out at once (q 2); the older state isn't resent after it.
    expect(datagrams.map((m) => m.q)).toEqual([1, 2]);
    advance(RESEND_MS);
    manager.tick();
    expect(datagrams.map((m) => m.q)).toEqual([1, 2, 2]);
    manager.stop();
  });

  test("when the datagram path goes away the pad is sent reliably, and not resent", () => {
    let on = true;
    const { gamepad, reliable, datagrams, manager, advance } = datagramSetup({ datagrams: () => on });
    const a = new FakePad("a", "gamepad");
    a.s.buttons[0] = 1;
    gamepad.add(a);
    manager.tick();
    expect(datagrams).toHaveLength(1);
    on = false;
    advance(RESEND_MS);
    manager.tick();
    expect(reliable).toHaveLength(1);
    expect(reliable[0]).toMatchObject({ k: "pad", i: 0, q: 1 });
    // From here changes go reliably, and nothing is resent.
    a.s.buttons[0] = 0;
    manager.tick();
    expect(reliable).toHaveLength(2);
    advance(RESEND_MS * 5);
    manager.tick();
    expect(reliable).toHaveLength(2);
    expect(datagrams).toHaveLength(1);
    manager.stop();
  });

  test("without an unreliable path, everything is as before: reliable, no resends", () => {
    const { gamepad, sent, manager } = setup();
    const a = new FakePad("a", "gamepad");
    a.s.buttons[0] = 1;
    gamepad.add(a);
    manager.tick();
    manager.tick();
    expect(sent).toHaveLength(1);
    manager.stop();
  });

  test("regaining the controls sends every pad again, with new numbers", () => {
    const { gamepad, datagrams, manager, advance } = datagramSetup();
    const a = new FakePad("a", "gamepad");
    a.s.buttons[0] = 1;
    gamepad.add(a);
    manager.tick();
    manager.resync();
    advance(1);
    manager.tick();
    expect(datagrams.map((m) => m.q)).toEqual([1, 2]);
    manager.stop();
  });

  test("an unfocused page lets go reliably and stops resending", () => {
    let focus = true;
    const { gamepad, reliable, datagrams, manager, advance } = datagramSetup({ focused: () => focus });
    const a = new FakePad("a", "gamepad");
    a.s.buttons[0] = 1;
    gamepad.add(a);
    manager.tick();
    focus = false;
    manager.tick();
    expect(reliable).toEqual([goneMessage(0, 2)]);
    advance(RESEND_MS * 4);
    manager.tick();
    expect(datagrams).toHaveLength(1);
    manager.stop();
  });
});

describe("events", () => {
  /** A backend with events: `changed` is what its drivers call on each input report. */
  test("a change the backend reports is sent at once, and the poll finds nothing new", () => {
    const { hid, sent, manager } = setup();
    const p = new FakePad("h", "webhid", [0x054c, 0x0ce6], "playstation");
    hid.add(p);
    manager.tick();
    sent.length = 0;
    p.s.buttons[0] = 1;
    hid.listener!.changed!(p);
    expect(sent).toHaveLength(1);
    expect(sent[0]).toMatchObject({ k: "pad", i: 0 });
    // The next poll sees the state already sent.
    manager.tick();
    manager.tick();
    expect(sent).toHaveLength(1);
    // A report that changes nothing sends nothing.
    hid.listener!.changed!(p);
    expect(sent).toHaveLength(1);
    // And a change the backend didn't announce is still found by the poll.
    p.s.buttons[1] = 1;
    manager.tick();
    expect(sent).toHaveLength(2);
    manager.stop();
  });

  test("events for an unfocused page, a hidden duplicate or after stop send nothing", () => {
    let focus = true;
    const { gamepad, hid, sent, manager } = setup(() => focus);
    const g = new FakePad("g0", "gamepad", [0x2dc8, 0x6101]);
    gamepad.add(g);
    const h = new FakePad("h0", "webhid", [0x2dc8, 0x6101]);
    hid.add(h);
    manager.tick();
    sent.length = 0;
    g.s.buttons[0] = 1; // hidden behind the HID one
    hid.listener!.changed!(g);
    expect(sent).toEqual([]);
    h.s.buttons[0] = 1;
    focus = false;
    hid.listener!.changed!(h);
    expect(sent).toEqual([]);
    focus = true;
    const listener = hid.listener!;
    manager.stop();
    sent.length = 0;
    listener.changed!(h);
    expect(sent).toEqual([]);
  });

  test("an event is sent by datagram like a polled change, with one number", () => {
    const { hid, datagrams, manager } = datagramSetup();
    const p = new FakePad("h", "webhid", [0x054c, 0x0ce6], "playstation");
    hid.add(p);
    manager.tick();
    p.s.buttons[0] = 1;
    hid.listener!.changed!(p);
    manager.tick();
    expect(datagrams.map((m) => m.q)).toEqual([1, 2]);
    manager.stop();
  });
});
