import { afterEach, describe, expect, test } from "bun:test";

import type { HidDevice } from "./hid-types";
import { SteamControllerDriver } from "./steam-controller";
import { SteamTritonDriver } from "./steam-triton";
import { BTN, EXTRA } from "./types";

interface Fake {
  device: HidDevice;
  features: { id: number; data: number[] }[];
  outputs: { id: number; data: number[] }[];
}

function fake(productId: number): Fake {
  const features: Fake["features"] = [];
  const outputs: Fake["outputs"] = [];
  const device = {
    vendorId: 0x28de,
    productId,
    productName: "Steam Controller",
    collections: [],
    sendFeatureReport: async (id: number, data: Uint8Array) => void features.push({ id, data: [...data] }),
    sendReport: async (id: number, data: BufferSource) => {
      const v = data as DataView;
      outputs.push({ id, data: [...new Uint8Array(v.buffer, v.byteOffset, v.byteLength)] });
    },
  } as unknown as HidDevice;
  return { device, features, outputs };
}

const drivers: { close(): Promise<void> }[] = [];
afterEach(async () => {
  for (const d of drivers.splice(0)) await d.close();
});

describe("Steam Controller (2026)", () => {
  /** The 45-byte state body: SDL's TritonMTUNoQuat_t, or the 16-bit timestamp variant's layout. */
  function state(timestamped: boolean, f: Partial<Record<string, number>> = {}): DataView {
    const d = new DataView(new ArrayBuffer(45));
    d.setUint8(0, 1);
    d.setUint32(1, f.buttons ?? 0, true);
    d.setInt16(5, f.trigL ?? 0, true);
    d.setInt16(7, f.trigR ?? 0, true);
    d.setInt16(9, f.lsx ?? 0, true);
    d.setInt16(11, f.lsy ?? 0, true);
    d.setInt16(13, f.rsx ?? 0, true);
    d.setInt16(15, f.rsy ?? 0, true);
    const pads = timestamped ? 19 : 17;
    d.setInt16(pads, f.lpx ?? 0, true);
    d.setInt16(pads + 2, f.lpy ?? 0, true);
    d.setInt16(pads + 6, f.rpx ?? 0, true);
    d.setInt16(pads + 8, f.rpy ?? 0, true);
    for (const [i, key] of ["ax", "ay", "az", "gx", "gy", "gz"].entries()) d.setInt16(33 + i * 2, f[key] ?? 0, true);
    return d;
  }
  const A = 0x1;
  const L = 0x80000;
  const DPAD_UP = 0x2000;
  const LEFT_PAD_TOUCH = 0x02000000;

  for (const [name, reportId, timestamped] of [
    ["state", 0x42, false],
    ["BLE state", 0x45, false],
    ["timestamped state", 0x47, true],
  ] as const) {
    test(`decodes a ${name} report`, () => {
      const driver = new SteamTritonDriver(fake(0x1302).device);
      driver.onReport(
        reportId,
        state(timestamped, {
          buttons: A | L | DPAD_UP | LEFT_PAD_TOUCH,
          trigL: 16384,
          lsx: 32767,
          lsy: 32767,
          rsx: -32768,
          lpx: 16384,
          lpy: -16384,
          gx: 16384,
          gy: 16384,
          gz: 16384,
          ay: 16384,
        }),
      );
      const s = driver.state();
      expect(s.buttons[BTN.south]).toBe(1);
      expect(s.buttons[BTN.leftShoulder]).toBe(1);
      expect(s.buttons[BTN.up]).toBe(1);
      expect(s.buttons[BTN.east]).toBe(0);
      expect(s.buttons[BTN.leftTrigger]).toBeCloseTo(0.5, 2);
      expect(s.buttons[BTN.rightTrigger]).toBe(0);
      expect(s.axes[0]).toBe(1);
      // The pad's y points up; the standard layout's points down.
      expect(s.axes[1]).toBeCloseTo(-1, 3);
      expect(s.axes[2]).toBe(-1);
      expect(s.touch?.[0]).toMatchObject({ id: 0, down: true });
      expect(s.touch![0]!.x).toBeCloseTo(0.75, 3);
      expect(s.touch![0]!.y).toBeCloseTo(0.75, 3);
      // Only the touched pad is there.
      expect(s.touch).toHaveLength(1);
      // SDL: gyro (x, z, -y) at 2000 deg/s over 16 bits, accel at 2 g.
      const rate = (2000 * Math.PI) / 180 / 2;
      expect(s.gyro![0]).toBeCloseTo(rate, 3);
      expect(s.gyro![1]).toBeCloseTo(rate, 3);
      expect(s.gyro![2]).toBeCloseTo(-rate, 3);
      expect(s.accel![2]).toBeCloseTo(-9.80665, 3);
    });
  }

  test("a lifted finger is gone, and comes back where it lands", () => {
    const driver = new SteamTritonDriver(fake(0x1302).device);
    driver.onReport(0x42, state(false, { buttons: LEFT_PAD_TOUCH, lpx: 8192 }));
    expect(driver.state().touch![0]!.x).toBeCloseTo(0.625, 3);
    driver.onReport(0x42, state(false, { buttons: 0, lpx: 0 }));
    expect(driver.state().touch).toEqual([]);
    driver.onReport(0x42, state(false, { buttons: 0x02000000 | 0x00200000, lpx: 0, rpx: -16384 }));
    expect(driver.state().touch!.map((t) => t.id)).toEqual([0, 1]);
    expect(driver.state().touch![1]!.x).toBeCloseTo(0.25, 3);
  });

  test("the grips, paddles and quick-access button are the extras", () => {
    const driver = new SteamTritonDriver(fake(0x1302).device);
    driver.onReport(0x42, state(false, { buttons: 0x20000 | 0x80 | 0x40000 | 0x100 | 0x10 }));
    const b = driver.state().buttons;
    expect([EXTRA.l4, EXTRA.r4, EXTRA.l5, EXTRA.r5, EXTRA.mute].map((i) => b[i])).toEqual([1, 1, 1, 1, 1]);
    expect(b[BTN.guide]).toBe(0);
    driver.onReport(0x42, state(false, { buttons: 0 }));
    expect(driver.state().buttons.slice(17).every((v) => v === 0)).toBe(true);
  });

  test("both trackpad clicks are extras (SDL's TRITON_RIGHT/LEFT_TOUCHPAD_CLICK, 0x00400000 and 0x04000000)", () => {
    const driver = new SteamTritonDriver(fake(0x1302).device);
    driver.onReport(0x42, state(false, { buttons: 0x00400000 }));
    let b = driver.state().buttons;
    expect([EXTRA.touchpadClick, EXTRA.leftPadClick].map((i) => b[i])).toEqual([1, 0]);
    driver.onReport(0x42, state(false, { buttons: 0x04000000 }));
    b = driver.state().buttons;
    expect([EXTRA.touchpadClick, EXTRA.leftPadClick].map((i) => b[i])).toEqual([0, 1]);
    // The touch bits next to them are not clicks.
    driver.onReport(0x42, state(false, { buttons: 0x00200000 | 0x02000000 }));
    b = driver.state().buttons;
    expect([EXTRA.touchpadClick, EXTRA.leftPadClick].map((i) => b[i])).toEqual([0, 0]);
  });

  test("a haptic pulse is output report 0x81: MsgHapticPulse (side, on_us, off_us, repeat_count)", async () => {
    const f = fake(0x1302);
    const driver = new SteamTritonDriver(f.device);
    drivers.push(driver);
    driver.haptic("right", 0.5, 2000, 3000, 10);
    await new Promise((r) => setTimeout(r, 5));
    expect(f.outputs[0]!.id).toBe(0x81);
    // side 0 (right, as hid-steam sends it), 2000 = 0x07d0, 3000 = 0x0bb8, 10.
    expect(f.outputs[0]!.data).toEqual([0, 0xd0, 0x07, 0xb8, 0x0b, 10, 0]);
    driver.haptic("left", 1, 1, 1, 0);
    await new Promise((r) => setTimeout(r, 5));
    expect(f.outputs[1]!.data).toEqual([1, 1, 0, 1, 0, 1, 0]);
    driver.haptic("left", 0, 1, 1, 1);
    expect(f.outputs).toHaveLength(2);
  });

  test("short reports and battery", () => {
    const driver = new SteamTritonDriver(fake(0x1302).device);
    driver.onReport(0x42, new DataView(new ArrayBuffer(10)));
    expect(driver.state().buttons.every((b) => b === 0)).toBe(true);
    driver.onReport(0x43, new DataView(new Uint8Array([1, 80, 0, 0]).buffer));
    expect(driver.state().battery).toBe(0.8);
  });

  test("claiming sends lizard mode off, then the IMU on, in feature report 1", async () => {
    const f = fake(0x1302);
    const driver = new SteamTritonDriver(f.device);
    drivers.push(driver);
    await driver.open();
    expect(f.features).toHaveLength(2);
    expect(f.features[0]!.id).toBe(1);
    expect(f.features[0]!.data).toHaveLength(63);
    // SET_SETTINGS_VALUES, 3 bytes: LIZARD_MODE (9) = 0.
    expect(f.features[0]!.data.slice(0, 5)).toEqual([0x87, 3, 9, 0, 0]);
    // IMU_MODE (48) = raw accel | raw gyro.
    expect(f.features[1]!.data.slice(0, 5)).toEqual([0x87, 3, 48, 0x18, 0]);
  });

  test("a puck shows no controller until its wireless status says so", () => {
    const driver = new SteamTritonDriver(fake(0x1304).device);
    let changes = 0;
    driver.onConnection = () => changes++;
    expect(driver.connected).toBe(false);
    driver.onReport(0x79, new DataView(new Uint8Array([2]).buffer));
    expect(driver.connected).toBe(true);
    driver.onReport(0x79, new DataView(new Uint8Array([1]).buffer));
    expect(driver.connected).toBe(false);
    expect(changes).toBe(2);
  });

  test("rumble is output report 0x80, resent until it ends", async () => {
    const f = fake(0x1302);
    const driver = new SteamTritonDriver(f.device);
    drivers.push(driver);
    driver.rumble(1, 0.5, 100);
    expect(f.outputs[0]!.id).toBe(0x80);
    const data = new DataView(new Uint8Array(f.outputs[0]!.data).buffer);
    expect(data.byteLength).toBe(9);
    expect(data.getUint16(3, true)).toBe(65535);
    expect(data.getUint16(6, true)).toBe(32768);
    await new Promise((r) => setTimeout(r, 130));
    const last = new DataView(new Uint8Array(f.outputs.at(-1)!.data).buffer);
    expect(f.outputs.length).toBeGreaterThan(2);
    expect(last.getUint16(3, true)).toBe(0);
    expect(last.getUint16(6, true)).toBe(0);
  });
});

describe("Steam Controller (original)", () => {
  const STATE = { south: 0x80, east: 0x20, bumperL: 0x08, up: 0x100, steam: 0x2000, rightPadClick: 0x40000, rightPadDown: 0x100000 };

  /** ValveInReport_t: version 1, type 1, then the state packet. */
  function state(f: { packet: number; buttons: number; trigL?: number; trigR?: number; lx?: number; ly?: number; rx?: number; ry?: number; gyroY?: number; accelY?: number }): DataView {
    const d = new DataView(new ArrayBuffer(64));
    d.setUint16(0, 1, true);
    d.setUint8(2, 1);
    d.setUint8(3, 60);
    d.setUint32(4, f.packet, true);
    d.setUint8(8, f.buttons & 0xff);
    d.setUint8(9, (f.buttons >> 8) & 0xff);
    d.setUint8(10, (f.buttons >> 16) & 0xff);
    d.setUint8(11, f.trigL ?? 0);
    d.setUint8(12, f.trigR ?? 0);
    d.setInt16(16, f.lx ?? 0, true);
    d.setInt16(18, f.ly ?? 0, true);
    d.setInt16(20, f.rx ?? 0, true);
    d.setInt16(22, f.ry ?? 0, true);
    d.setInt16(36, f.gyroY ?? 0, true);
    d.setInt16(30, f.accelY ?? 0, true);
    return d;
  }

  test("a wired controller's stick (finger-down bit clear), buttons, triggers", () => {
    const driver = new SteamControllerDriver(fake(0x1102).device);
    driver.onReport(0, state({ packet: 1, buttons: STATE.south | STATE.bumperL | STATE.up | STATE.steam, trigL: 255, lx: 32767, ly: 32767 }));
    const s = driver.state();
    expect(s.buttons[BTN.south]).toBe(1);
    expect(s.buttons[BTN.leftShoulder]).toBe(1);
    expect(s.buttons[BTN.up]).toBe(1);
    expect(s.buttons[BTN.guide]).toBe(1);
    expect(s.buttons[BTN.east]).toBe(0);
    // Trigger bytes 255 are past the 26000 maximum.
    expect(s.buttons[BTN.leftTrigger]).toBe(1);
    expect(s.axes[0]).toBe(1);
    // ~32767 is -32768: up.
    expect(s.axes[1]).toBe(-1);
    expect(s.touch).toEqual([]);
  });

  test("grips and pad clicks are the extras; the left click is the stick's while the pad is idle", () => {
    const driver = new SteamControllerDriver(fake(0x1102).device);
    driver.onReport(0, state({ packet: 1, buttons: 0x8000 | 0x10000 | STATE.rightPadClick }));
    let b = driver.state().buttons;
    expect([EXTRA.touchpadClick, EXTRA.l4, EXTRA.r4, EXTRA.leftPadClick].map((i) => b[i])).toEqual([1, 1, 1, 0]);
    driver.onReport(0, state({ packet: 2, buttons: 0x20000 }));
    b = driver.state().buttons;
    expect(b[BTN.leftStick]).toBe(1);
    expect(b[EXTRA.leftPadClick]).toBe(0);
    driver.onReport(0, state({ packet: 3, buttons: 0x20000 | 0x80000 }));
    b = driver.state().buttons;
    expect(b[EXTRA.leftPadClick]).toBe(1);
    expect(driver.state().touch!.map((t) => t.id)).toEqual([0]);
  });

  test("a haptic pulse is feature report 0x8F: SDL's MsgFireHapticPulse (pad, pulse, gap, count, gain, priority)", async () => {
    const f = fake(0x1102);
    const driver = new SteamControllerDriver(f.device);
    drivers.push(driver);
    await driver.open();
    f.features.length = 0;
    driver.haptic("left", 1, 300, 0x0102, 5);
    await new Promise((r) => setTimeout(r, 5));
    expect(f.features[0]!.data.slice(0, 12)).toEqual([0x8f, 10, 1, 0x2c, 0x01, 0x02, 0x01, 5, 0, 0, 0, 0]);
    driver.haptic("right", 1, 300, 300, 1);
    await new Promise((r) => setTimeout(r, 5));
    expect(f.features[1]!.data[2]).toBe(0);
  });

  test("the right pad is the right stick, and touches", () => {
    const driver = new SteamControllerDriver(fake(0x1102).device);
    driver.onReport(0, state({ packet: 1, buttons: STATE.rightPadDown | STATE.rightPadClick, rx: 0, ry: 0 }));
    const s = driver.state();
    expect(s.touch).toHaveLength(1);
    expect(s.touch![0]).toMatchObject({ id: 1, down: true });
    expect(s.touch![0]!.x).toBeCloseTo(0.5, 1);
    expect(s.buttons[BTN.rightStick]).toBe(1);
  });

  test("the same packet number is the same state", () => {
    const driver = new SteamControllerDriver(fake(0x1102).device);
    driver.onReport(0, state({ packet: 5, buttons: STATE.south }));
    driver.onReport(0, state({ packet: 5, buttons: 0 }));
    expect(driver.state().buttons[BTN.south]).toBe(1);
    driver.onReport(0, state({ packet: 6, buttons: 0 }));
    expect(driver.state().buttons[BTN.south]).toBe(0);
  });

  test("sensors: gyro (x, z, y) and accel (x, z, -y)", () => {
    const driver = new SteamControllerDriver(fake(0x1102).device);
    driver.onReport(0, state({ packet: 1, buttons: 0, gyroY: 16384, accelY: 16384 }));
    const s = driver.state();
    expect(s.gyro![2]).toBeCloseTo((2000 * Math.PI) / 180 / 2, 3);
    expect(s.accel![2]).toBeCloseTo(-9.80665, 3);
  });

  test("claiming sends the reset in 64-byte feature reports", async () => {
    const f = fake(0x1102);
    const driver = new SteamControllerDriver(f.device);
    drivers.push(driver);
    await driver.open();
    expect(f.features.every((r) => r.id === 0 && r.data.length === 64)).toBe(true);
    expect(f.features.map((r) => r.data[0])).toEqual([0x81, 0x8e, 0x87, 0x87]);
    // WIRELESS_PACKET_VERSION = 2, both pads off, SMOOTH_ABSOLUTE_MOUSE = 0.
    expect(f.features[2]!.data.slice(0, 14)).toEqual([0x87, 12, 49, 2, 0, 7, 7, 0, 8, 7, 0, 24, 0, 0]);
  });

  test("closing brings lizard mode back", async () => {
    const f = fake(0x1102);
    const driver = new SteamControllerDriver(f.device);
    await driver.open();
    f.features.length = 0;
    await driver.close();
    expect(f.features.map((r) => r.data[0])).toEqual([0x85, 0x8e, 0x87]);
    // RIGHT_TRACKPAD_MODE = absolute mouse.
    expect(f.features[2]!.data.slice(0, 5)).toEqual([0x87, 3, 8, 0, 0]);
  });

  test("a dongle has no controller until one connects, then claims it", async () => {
    const f = fake(0x1142);
    const driver = new SteamControllerDriver(f.device);
    drivers.push(driver);
    let changes = 0;
    driver.onConnection = () => changes++;
    expect(driver.connected).toBe(false);
    const wireless = new DataView(new ArrayBuffer(64));
    wireless.setUint16(0, 1, true);
    wireless.setUint8(2, 3);
    wireless.setUint8(3, 1);
    wireless.setUint8(4, 2);
    driver.onReport(0, wireless);
    expect(driver.connected).toBe(true);
    expect(changes).toBe(1);
    await new Promise((r) => setTimeout(r, 20));
    expect(f.features.some((r) => r.data[0] === 0x81)).toBe(true);
    wireless.setUint8(4, 1);
    driver.onReport(0, wireless);
    expect(driver.connected).toBe(false);
  });

  test("Bluetooth state arrives in numbered segments of report 3", () => {
    const driver = new SteamControllerDriver(fake(0x1106).device);
    // Mask: report 4, button chunk 1 (0x10) in the first byte; the second byte has the left pad chunk's neighbour, the stick (0x80).
    const packet = new Uint8Array(36);
    packet[0] = 0x14;
    packet[1] = 0x00;
    packet[2] = STATE.south; // buttons, 3 bytes
    // No further chunks.
    const segment = (n: number, last: boolean) => {
      const d = new Uint8Array(19);
      d[0] = 0x80 | n | (last ? 0x40 : 0);
      d.set(packet.slice(n * 18, n * 18 + 18), 1);
      return new DataView(d.buffer);
    };
    driver.onReport(3, segment(0, false));
    expect(driver.state().buttons[BTN.south]).toBe(0);
    driver.onReport(3, segment(1, true));
    expect(driver.state().buttons[BTN.south]).toBe(1);
  });
});
