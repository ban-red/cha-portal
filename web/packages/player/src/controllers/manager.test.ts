import { describe, expect, test } from "bun:test";

import { ControllerManager, goneMessage, padMessage } from "./manager";
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
    expect(sent).toEqual([goneMessage(0)]);
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
      touch: [{ id: 0, x: 0.12345, y: 0.5, down: true }],
      battery: 0.756,
    });
    expect(rich.gyro).toEqual([0.123, 0, 0]);
    expect(rich.accel).toEqual([0, 9.807, 0]);
    expect(rich.touch).toEqual([{ id: 0, x: 0.123, y: 0.5, down: true }]);
    expect(rich.bat).toBe(0.76);
    expect((rich.b as number[]).length).toBe(17);
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
    expect(sent).toEqual([goneMessage(0)]);
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
    expect(sent).toEqual([goneMessage(0), goneMessage(1)]);
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
