import { afterEach, describe, expect, test } from "bun:test";

import {
  DUALSENSE,
  DUALSENSE_EDGE,
  DualSenseDriver,
  btOutput,
  commonOutput,
  crc32,
  parseCalibration,
  usbOutput,
  type Effects,
} from "./dualsense";
import type { HidDevice } from "./hid-types";
import { BTN, EXTRA } from "./types";

interface Fake {
  device: HidDevice;
  outputs: { id: number; data: number[] }[];
  features: number[];
}

function fake(bluetooth: boolean, calibration?: DataView, productId = DUALSENSE): Fake {
  const outputs: Fake["outputs"] = [];
  const features: number[] = [];
  const device = {
    vendorId: 0x054c,
    productId,
    productName: "DualSense Wireless Controller",
    collections: bluetooth ? [{ usagePage: 1, usage: 5, inputReports: [{ reportId: 1 }, { reportId: 0x31 }] }] : [{ usagePage: 1, usage: 5, inputReports: [{ reportId: 1 }] }],
    receiveFeatureReport: async (id: number) => {
      features.push(id);
      if (!calibration) throw new Error("no");
      return calibration;
    },
    sendReport: async (id: number, data: BufferSource) => {
      const v = data as Uint8Array;
      outputs.push({ id, data: [...v] });
    },
  } as unknown as HidDevice;
  return { device, outputs, features };
}

const drivers: { close(): Promise<void> }[] = [];
afterEach(async () => {
  for (const d of drivers.splice(0)) await d.close();
});
const flush = () => new Promise((r) => setTimeout(r, 5));

/** The 63 bytes of the full state (USB report 0x01's data, Bluetooth 0x31's after its one byte of header). */
function state(f: { sticks?: number[]; trigL?: number; trigR?: number; face?: number; shoulders?: number; more?: number; gyro?: number[]; accel?: number[]; touch?: number[][]; status?: number } = {}): Uint8Array {
  const d = new Uint8Array(63);
  d.set(f.sticks ?? [128, 128, 128, 128], 0);
  d[4] = f.trigL ?? 0;
  d[5] = f.trigR ?? 0;
  d[7] = f.face ?? 0x08;
  d[8] = f.shoulders ?? 0;
  d[9] = f.more ?? 0;
  const v = new DataView(d.buffer);
  (f.gyro ?? [0, 0, 0]).forEach((x, i) => v.setInt16(15 + i * 2, x, true));
  (f.accel ?? [0, 0, 0]).forEach((x, i) => v.setInt16(21 + i * 2, x, true));
  // Two finger slots, empty unless given: counter (top bit set when empty), x low, x high and y low nibbles, y high.
  for (let i = 0; i < 2; i++) {
    const t = f.touch?.[i];
    const at = 32 + i * 4;
    if (!t) {
      d[at] = 0x80;
      continue;
    }
    d[at] = i + 1;
    d[at + 1] = t[0]! & 0xff;
    d[at + 2] = ((t[0]! >> 8) & 0x0f) | ((t[1]! & 0x0f) << 4);
    d[at + 3] = t[1]! >> 4;
  }
  d[52] = f.status ?? 0;
  return d;
}

const view = (bytes: ArrayLike<number>) => new DataView(Uint8Array.from(bytes).buffer);
/** Bluetooth: a header byte, the state, then padding and the CRC. */
const bluetooth = (s: Uint8Array) => view([0x01, ...s, ...new Array(13).fill(0)]);

describe("DualSense input", () => {
  test("USB report 0x01: sticks, triggers, hat, face, shoulders, PS", () => {
    const driver = new DualSenseDriver(fake(false).device);
    driver.onReport(
      1,
      view(
        state({
          sticks: [255, 0, 128, 64],
          trigL: 255,
          trigR: 51,
          // Hat up-right (1) with cross and triangle.
          face: 0x01 | 0x20 | 0x80,
          // L1, options, R3.
          shoulders: 0x01 | 0x20 | 0x80,
          more: 0x01,
        }),
      ),
    );
    const s = driver.state();
    expect(s.axes[0]).toBe(1);
    expect(s.axes[1]).toBe(-1);
    expect(s.axes[2]).toBe(0);
    expect(s.axes[3]).toBeCloseTo(-0.5, 2);
    expect(s.buttons[BTN.leftTrigger]).toBe(1);
    expect(s.buttons[BTN.rightTrigger]).toBeCloseTo(0.2, 3);
    expect([BTN.up, BTN.right, BTN.down, BTN.left].map((i) => s.buttons[i])).toEqual([1, 1, 0, 0]);
    expect([BTN.south, BTN.east, BTN.west, BTN.north].map((i) => s.buttons[i])).toEqual([1, 0, 0, 1]);
    expect([BTN.leftShoulder, BTN.rightShoulder, BTN.start, BTN.back, BTN.rightStick, BTN.guide].map((i) => s.buttons[i])).toEqual([1, 0, 1, 0, 1, 1]);
  });

  test("the hat's eight directions and rest", () => {
    const driver = new DualSenseDriver(fake(false).device);
    const dirs: number[][] = [];
    for (let hat = 0; hat <= 8; hat++) {
      driver.onReport(1, view(state({ face: hat })));
      dirs.push([BTN.up, BTN.right, BTN.down, BTN.left].map((i) => driver.state().buttons[i]!));
    }
    expect(dirs).toEqual([
      [1, 0, 0, 0],
      [1, 1, 0, 0],
      [0, 1, 0, 0],
      [0, 1, 1, 0],
      [0, 0, 1, 0],
      [0, 0, 1, 1],
      [0, 0, 0, 1],
      [1, 0, 0, 1],
      [0, 0, 0, 0],
    ]);
  });

  test("touchpad click, mute and the Edge's paddles and function buttons are the extras", () => {
    const driver = new DualSenseDriver(fake(false, undefined, DUALSENSE_EDGE).device);
    driver.onReport(1, view(state({ more: 0x02 | 0x04 | 0x10 | 0x20 | 0x40 | 0x80 })));
    const b = driver.state().buttons;
    expect([EXTRA.touchpadClick, EXTRA.mute, EXTRA.l5, EXTRA.r5, EXTRA.l4, EXTRA.r4, EXTRA.leftPadClick].map((i) => b[i])).toEqual([1, 1, 1, 1, 1, 1, 0]);
    expect(b[BTN.guide]).toBe(0);
  });

  test("touch: only fingers on the pad, as 0..1", () => {
    const driver = new DualSenseDriver(fake(false).device);
    driver.onReport(1, view(state({ touch: [[960, 535]] })));
    expect(driver.state().touch).toHaveLength(1);
    const [t] = driver.state().touch!;
    expect(t).toMatchObject({ id: 0, down: true });
    expect(t!.x).toBeCloseTo(0.5, 2);
    expect(t!.y).toBeCloseTo(0.5, 2);
    // The second finger alone keeps its slot's id; SDL's scale is x / 1920 and y / 1070.
    driver.onReport(1, view(state({ touch: [undefined as never, [1920, 1070]] })));
    expect(driver.state().touch).toEqual([{ id: 1, x: 1, y: 1, down: true }]);
    driver.onReport(1, view(state()));
    expect(driver.state().touch).toEqual([]);
  });

  test("battery: level and charging state", () => {
    const driver = new DualSenseDriver(fake(false).device);
    driver.onReport(1, view(state({ status: 0x03 })));
    expect(driver.state().battery).toBe(0.35);
    driver.onReport(1, view(state({ status: 0x18 })));
    expect(driver.state().battery).toBe(0.85);
    driver.onReport(1, view(state({ status: 0x20 })));
    expect(driver.state().battery).toBe(1);
    driver.onReport(1, view(state({ status: 0xa5 })));
    expect(driver.state().battery).toBe(1);
  });

  test("a trigger that reads 0 with its digital bit set is fully pulled (SDL: L2 0x04, R2 0x08 in the shoulder byte)", () => {
    const driver = new DualSenseDriver(fake(false).device);
    driver.onReport(1, view(state({ shoulders: 0x04 })));
    expect([BTN.leftTrigger, BTN.rightTrigger].map((i) => driver.state().buttons[i])).toEqual([1, 0]);
    driver.onReport(1, view(state({ shoulders: 0x08, trigL: 51 })));
    expect(driver.state().buttons[BTN.leftTrigger]).toBeCloseTo(0.2, 3);
    expect(driver.state().buttons[BTN.rightTrigger]).toBe(1);
  });

  test("sensors without calibration: SDL reads the gyro as raw * 64 per 1024 per degree, the accelerometer as raw per 8192 per g", () => {
    const driver = new DualSenseDriver(fake(false).device);
    driver.onReport(1, view(state({ gyro: [1024, 0, -1024], accel: [0, 8192, 0] })));
    const s = driver.state();
    expect(s.gyro![0]).toBeCloseTo(64 * (Math.PI / 180), 5);
    expect(s.gyro![2]).toBeCloseTo(-64 * (Math.PI / 180), 5);
    expect(s.accel![1]).toBeCloseTo(9.80665, 4);
  });

  /** Feature report 0x05's data, laid out as SDL's LoadCalibrationData reads it (its offsets, minus the id byte). */
  function calibrationReport(gyroSpan: [number, number, number], accel: number[], gyroBias = [100, 0, 0]): DataView {
    const c = new DataView(new ArrayBuffer(40));
    const set = (at: number, v: number) => c.setInt16(at, v, true);
    gyroBias.forEach((v, i) => set(i * 2, v));
    // Pitch, yaw, roll: plus then minus (SDL's data[7..18]).
    gyroSpan.forEach((span, i) => {
      set(6 + i * 4, span / 2);
      set(8 + i * 4, -span / 2);
    });
    // Speed plus and minus (data[19..22]), 540 each as a real controller's.
    set(18, 540);
    set(20, 540);
    // Accelerometer x, y, z plus and minus (data[23..34]).
    accel.forEach((v, i) => set(22 + i * 2, v));
    return c;
  }

  test("calibration: bias and scale, as SDL computes them from feature report 0x05", async () => {
    // Gyro: sensitivity (540 + 540) * 1024 / span, 64 when the span is 17280. Accelerometer x's range is off-centre.
    const c = calibrationReport([17280, 17000, 17280], [8200, -8000, 8192, -8192, 8192, -8192]);
    const cal = parseCalibration(c);
    expect(cal.gyro[0]!.bias).toBe(100);
    expect(cal.gyro[0]!.scale).toBeCloseTo((64 / 1024) * (Math.PI / 180), 10);
    expect(cal.gyro[1]!.scale).toBeCloseTo(((1080 * 1024) / 17000 / 1024) * (Math.PI / 180), 10);
    // bias = plus - range / 2; sensitivity = 2 * 8192 / range, then / 8192 * g.
    expect(cal.accel[0]).toEqual({ bias: 100, scale: (2 / 16200) * 9.80665 });
    expect(cal.accel[1]).toEqual({ bias: 0, scale: (2 / 16384) * 9.80665 });

    const f = fake(false, c);
    const driver = new DualSenseDriver(f.device);
    await driver.open();
    expect(f.features).toEqual([0x05]);
    driver.onReport(1, view(state({ gyro: [100 + 1000, 0, 0], accel: [100 + 8100, 0, 0] })));
    expect(driver.state().gyro![0]).toBeCloseTo(1000 * (64 / 1024) * (Math.PI / 180), 4);
    expect(driver.state().accel![0]).toBeCloseTo(((8100 * 2) / 16200) * 9.80665, 3);
  });

  test("an implausible calibration is ignored altogether, as in SDL (bias over 1024, or sensitivity more than half off)", () => {
    const defaults = parseCalibration(new DataView(new ArrayBuffer(10)));
    // Gyro sensitivity 128 against the expected 64.
    expect(parseCalibration(calibrationReport([8640, 17280, 17280], [8192, -8192, 8192, -8192, 8192, -8192]))).toEqual(defaults);
    // A gyro bias of 2000.
    expect(parseCalibration(calibrationReport([17280, 17280, 17280], [8192, -8192, 8192, -8192, 8192, -8192], [2000, 0, 0]))).toEqual(defaults);
    // An accelerometer range of 8192 (sensitivity 2).
    expect(parseCalibration(calibrationReport([17280, 17280, 17280], [4096, -4096, 8192, -8192, 8192, -8192]))).toEqual(defaults);
  });

  test("a calibration that can't be read leaves the defaults", async () => {
    const driver = new DualSenseDriver(fake(false).device);
    await driver.open();
    driver.onReport(1, view(state({ gyro: [1024, 0, 0] })));
    expect(driver.state().gyro![0]).toBeCloseTo(64 * (Math.PI / 180), 5);
    expect(parseCalibration(new DataView(new ArrayBuffer(10))).accel[2]!.scale).toBeCloseTo(9.80665 / 8192, 8);
  });

  test("Bluetooth report 0x31 has one byte before the state", () => {
    const driver = new DualSenseDriver(fake(true).device);
    driver.onReport(0x31, bluetooth(state({ sticks: [255, 128, 128, 128], face: 0x28, trigR: 255, touch: [[100, 200]] })));
    const s = driver.state();
    expect(s.axes[0]).toBe(1);
    expect(s.buttons[BTN.south]).toBe(1);
    expect(s.buttons[BTN.rightTrigger]).toBe(1);
    expect(s.touch![0]!.x).toBeCloseTo(100 / 1920, 4);
    expect(s.touch![0]!.y).toBeCloseTo(200 / 1070, 4);
  });

  test("Bluetooth's short report 0x01 can come padded to 77 bytes; USB's full one is 63", () => {
    const driver = new DualSenseDriver(fake(true).device);
    const padded = new Array<number>(77).fill(0);
    padded.splice(0, 9, 255, 128, 128, 0, 0x04, 0x01, 0, 0, 0);
    driver.onReport(1, view(padded));
    const s = driver.state();
    expect(s.axes[0]).toBe(1);
    expect(s.buttons[BTN.down]).toBe(1);
    expect(s.buttons[BTN.leftShoulder]).toBe(1);
  });

  test("Bluetooth's short report 0x01 until the full one", () => {
    const driver = new DualSenseDriver(fake(true).device);
    // Sticks, hat down with square, L1, PS and touchpad, the triggers.
    driver.onReport(1, view([255, 128, 128, 0, 0x04 | 0x10, 0x01, 0x03, 255, 0]));
    const s = driver.state();
    expect(s.axes[0]).toBe(1);
    expect(s.axes[3]).toBe(-1);
    expect(s.buttons[BTN.down]).toBe(1);
    expect(s.buttons[BTN.west]).toBe(1);
    expect(s.buttons[BTN.leftShoulder]).toBe(1);
    expect(s.buttons[BTN.guide]).toBe(1);
    expect(s.buttons[EXTRA.touchpadClick]).toBe(1);
    expect(s.buttons[BTN.leftTrigger]).toBe(1);
  });
});

describe("DualSense output", () => {
  const none: Effects = { lo: 0, hi: 0, led: null, players: null, left: null, right: null };

  test("CRC-32 is the standard one", () => {
    expect(crc32([...new TextEncoder().encode("123456789")])).toBe(0xcbf43926);
    expect(crc32([0x34, 0x35], crc32([0x31, 0x32, 0x33]))).toBe(crc32([0x31, 0x32, 0x33, 0x34, 0x35]));
  });

  test("the common part: motors, lightbar, players, trigger blocks, and what is valid", () => {
    const effect = [2, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    const out = commonOutput({ lo: 1, hi: 0.5, led: [1, 2, 3], players: 0x15, left: [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], right: effect }, true);
    expect(out).toHaveLength(47);
    // Compatible vibration (both), both triggers.
    expect(out[0]).toBe(0x01 | 0x02 | 0x04 | 0x08);
    // Lightbar and players.
    expect(out[1]).toBe(0x04 | 0x10);
    expect(out[2]).toBe(128);
    expect(out[3]).toBe(255);
    expect([...out.slice(10, 21)]).toEqual(effect);
    expect(out[21]).toBe(1);
    // Vibration 2, lightbar setup (light out).
    expect(out[38]).toBe(0x04 | 0x02);
    expect(out[41]).toBe(0x02);
    expect(out[43]).toBe(0x15);
    expect([...out.slice(44, 47)]).toEqual([1, 2, 3]);
  });

  test("a report only changes what was set", () => {
    const out = commonOutput({ ...none, lo: 0.2 }, true);
    expect(out[0]).toBe(0x03);
    expect(out[1]).toBe(0);
    expect(out[38]).toBe(0x04);
    expect(commonOutput(none, false, true)[1]).toBe(0x08);
  });

  test("USB: report 0x02, 63 bytes", async () => {
    const f = fake(false);
    const driver = new DualSenseDriver(f.device);
    drivers.push(driver);
    driver.led(10, 20, 30);
    driver.players(0x1f);
    driver.trigger("left", [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
    await flush();
    expect(f.outputs.map((o) => o.id)).toEqual([2, 2, 2]);
    expect(f.outputs.every((o) => o.data.length === 63)).toBe(true);
    const last = f.outputs.at(-1)!.data;
    expect(last.slice(44, 47)).toEqual([10, 20, 30]);
    expect(last[43]).toBe(0x1f);
    expect(last.slice(21, 32)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
    // Light out only on the first report with a colour.
    expect(f.outputs[0]!.data[41]).toBe(0x02);
    expect(f.outputs[1]!.data[41]).toBe(0);
    expect(usbOutput(new Uint8Array(47))).toHaveLength(63);
  });

  test("rumble: motors, resent, and off at the end", async () => {
    const f = fake(false);
    const driver = new DualSenseDriver(f.device);
    drivers.push(driver);
    driver.rumble(1, 0.5, 20);
    await flush();
    expect(f.outputs[0]!.data.slice(2, 4)).toEqual([128, 255]);
    await new Promise((r) => setTimeout(r, 40));
    expect(f.outputs.at(-1)!.data.slice(2, 4)).toEqual([0, 0]);
    driver.rumble(1, 1, 0);
    await flush();
    expect(f.outputs.at(-1)!.data.slice(2, 4)).toEqual([0, 0]);
  });

  test("Bluetooth: report 0x31 with a sequence, the tag and the CRC", async () => {
    const f = fake(true);
    const driver = new DualSenseDriver(f.device);
    drivers.push(driver);
    driver.led(255, 0, 128);
    driver.players(1);
    await flush();
    expect(f.outputs.map((o) => o.id)).toEqual([0x31, 0x31]);
    const [a, b] = f.outputs.map((o) => o.data);
    expect(a).toHaveLength(77);
    // The sequence climbs in the high nibble; the tag is 0x10.
    expect([a![0], a![1], b![0], b![1]]).toEqual([0x00, 0x10, 0x10, 0x10]);
    // The common part starts after the two.
    expect(a!.slice(2 + 44, 2 + 47)).toEqual([255, 0, 128]);
    for (const d of [a!, b!]) {
      const crc = crc32([0xa2, 0x31, ...d.slice(0, 73)]);
      expect(new DataView(Uint8Array.from(d).buffer).getUint32(73, true)).toBe(crc);
    }
    expect(btOutput(new Uint8Array(47), 17)[0]).toBe(0x10);
  });

  test("closing turns the motors and triggers off, and gives the lights back", async () => {
    const f = fake(false);
    const driver = new DualSenseDriver(f.device);
    driver.led(1, 2, 3);
    driver.trigger("right", [2, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    await flush();
    f.outputs.length = 0;
    await driver.close();
    expect(f.outputs).toHaveLength(1);
    const d = f.outputs[0]!.data;
    expect(d[1]).toBe(0x08);
    expect(d[0]! & 0x04).toBe(0x04);
    // SDL's testcontroller clears a trigger with mode 0x05.
    expect(d.slice(10, 21)).toEqual([0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    // Nothing of ours to undo: nothing sent.
    const g = fake(false);
    await new DualSenseDriver(g.device).close();
    expect(g.outputs).toHaveLength(0);
  });
});
