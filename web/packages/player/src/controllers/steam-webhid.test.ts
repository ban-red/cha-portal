import { describe, expect, test } from "bun:test";

import { ControllerManager } from "./manager";
import { isSteamOnGamepadApi, steamNeedingWebHid } from "./steam-webhid";
import {
  neutralState,
  type BackendController,
  type BackendListener,
  type BackendName,
  type ControllerBackend,
  type ControllerInfo,
} from "./types";

const info = (over: Partial<ControllerInfo>): ControllerInfo => ({
  name: "Pad",
  backend: "gamepad",
  type: "generic",
  capabilities: { rumble: false, gyro: false, touchpad: false, battery: false },
  mapped: true,
  ...over,
});

describe("isSteamOnGamepadApi", () => {
  test("a Valve controller through the Gamepad API, by type, vendor or name", () => {
    expect(isSteamOnGamepadApi(info({ type: "steam", name: "Steam Controller" }))).toBe(true);
    expect(isSteamOnGamepadApi(info({ vendorId: 0x28de, productId: 0x1302 }))).toBe(true);
    expect(isSteamOnGamepadApi(info({ name: "Steam Controller Puck" }))).toBe(true);
    expect(isSteamOnGamepadApi(info({ name: "Valve Software Steam Controller" }))).toBe(true);
  });
  test("not a controller from another maker, nor one WebHID already has", () => {
    expect(isSteamOnGamepadApi(info({ vendorId: 0x045e, productId: 0x028e, name: "Xbox 360 pad" }))).toBe(false);
    expect(isSteamOnGamepadApi(info({ backend: "webhid", type: "steam", vendorId: 0x28de, productId: 0x1302 }))).toBe(false);
  });
});

class Pad implements BackendController {
  readonly info: ControllerInfo;
  constructor(readonly key: string, backend: BackendName, over: Partial<ControllerInfo> = {}) {
    this.info = info({ name: key, backend, ...over });
  }
  state() {
    return neutralState();
  }
  raw() {
    return null;
  }
  rumble() {}
}
class Backend implements ControllerBackend {
  readonly unavailable = null;
  listener: BackendListener | null = null;
  constructor(readonly name: BackendName) {}
  start(l: BackendListener) {
    this.listener = l;
  }
  stop() {
    this.listener = null;
  }
  controllers() {
    return [];
  }
}

describe("with the manager's twin rule", () => {
  const steam = { type: "steam", name: "Steam Controller" } as const;
  function setup() {
    const api = new Backend("gamepad");
    const hid = new Backend("webhid");
    const m = new ControllerManager({ backends: [api, hid], now: () => 0 });
    m.start();
    return { api, hid, m };
  }

  test("a Steam Controller only the Gamepad API shows is listed as needing WebHID", () => {
    const { api, m } = setup();
    api.listener!.arrived(new Pad("g0", "gamepad", steam));
    expect(steamNeedingWebHid(m.controllers()).map((c) => c.id)).toEqual(["g0"]);
    m.stop();
  });

  test("once WebHID has the controller, its Gamepad API twin is hidden and nothing is listed", () => {
    const { api, hid, m } = setup();
    api.listener!.arrived(new Pad("g0", "gamepad", steam));
    hid.listener!.arrived(new Pad("h0", "webhid", { ...steam, type: "steam", vendorId: 0x28de, productId: 0x1302 }));
    expect(m.controllers().map((c) => c.id)).toEqual(["h0"]);
    expect(steamNeedingWebHid(m.controllers())).toEqual([]);
    m.stop();
  });

  test("it comes back when the WebHID pad goes, and other pads are never listed", () => {
    const { api, hid, m } = setup();
    const h = new Pad("h0", "webhid", { type: "steam", vendorId: 0x28de, productId: 0x1302 });
    hid.listener!.arrived(h);
    api.listener!.arrived(new Pad("g0", "gamepad", steam));
    api.listener!.arrived(new Pad("x", "gamepad", { vendorId: 0x045e, productId: 0x028e, type: "xbox" }));
    expect(steamNeedingWebHid(m.controllers())).toEqual([]);
    hid.listener!.removed(h);
    expect(steamNeedingWebHid(m.controllers()).map((c) => c.id)).toEqual(["g0"]);
    m.stop();
  });
});
