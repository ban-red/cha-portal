// The PlayStation 5 DualSense and DualSense Edge over WebHID, wired or over
// Bluetooth. Its gamepad collection is a plain HID one the Gamepad API can
// read, but only as buttons and sticks: here the page also gets the gyro and
// accelerometer (calibrated), the touchpad's fingers, the battery, the Edge's
// paddles and function buttons, and can drive the rumble motors, the lightbar,
// the player LEDs and the adaptive triggers.
//
// Adapted from SDL3 (https://github.com/libsdl-org/SDL, zlib licence,
// Copyright (C) 1997-2026 Sam Lantinga): src/joystick/hidapi/SDL_hidapi_ps5.c.
// Altered: ported to TypeScript over WebHID, and cut down to what a page
// needs. The layouts also follow the Linux kernel's hid-playstation.c.
//
// Over USB the state is input report 0x01 and the output report 0x02. Over
// Bluetooth the controller sends a short report 0x01 (sticks, buttons,
// triggers) until something reads its calibration feature report 0x05, then
// the full report 0x31; its output is report 0x31 with a 4-bit sequence, a tag
// and a CRC-32 over the report (seeded with 0xA2). WebHID hands over reports
// without their id byte, so the offsets here are one less than the specs'.

import type { HidDevice, HidDriver, HidDriverFactory } from "./hid-types";
import { BTN, EXTRA, clamp, neutralState, type ControllerInfo, type ControllerState, type Side, type TouchPoint } from "./types";

export const SONY_VENDOR_ID = 0x054c;
export const DUALSENSE = 0x0ce6;
export const DUALSENSE_EDGE = 0x0df2;

const REPORT_USB_STATE = 0x01;
const REPORT_BT_STATE = 0x31;
const REPORT_USB_OUTPUT = 0x02;
const REPORT_BT_OUTPUT = 0x31;
const FEATURE_CALIBRATION = 0x05;
/** The state, after the report id: sticks, triggers, buttons, sensors, touch, battery. */
const STATE_BYTES = 63;
/** The short Bluetooth state: sticks, buttons, triggers. */
const SIMPLE_BYTES = 9;
/** Over Bluetooth the short state can come padded to the long report's size (SDL: 78 bytes with its id). */
const SIMPLE_PADDED_BYTES = 77;
/** The output report's common part, after the report id (and, over Bluetooth, its sequence and tag). */
const COMMON_BYTES = 47;
/** Output reports without their id: USB 63 bytes, Bluetooth 77 (sequence, tag, common part, padding, CRC). */
const USB_OUTPUT_BYTES = 63;
const BT_OUTPUT_BYTES = 77;
const BT_OUTPUT_TAG = 0x10;
const BT_CRC_SEED = 0xa2;
/** SDL's testcontroller clears a trigger with mode 0x05. */
export const TRIGGER_EFFECT_OFF: readonly number[] = [0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/** SDL's touch scale: x / 1920 and y / 1070 (TOUCHPAD_SCALEX and SCALEY; the pad itself is about 1920 by 1080). */
const TOUCH_W = 1920;
const TOUCH_H = 1070;
/** SDL's GYRO_RES_PER_DEGREE and ACCEL_RES_PER_G. */
const GYRO_RES_PER_DEGREE = 1024;
const ACCEL_RES_PER_G = 8192;
/** Without (valid) calibration SDL reads the gyro as raw * 64 per 1024 per degree, and the accelerometer as raw per 8192 per g. */
const GYRO_DEFAULT = (64 / GYRO_RES_PER_DEGREE) * (Math.PI / 180);
const ACCEL_DEFAULT = (1 / ACCEL_RES_PER_G) * 9.80665;

/** Rumble written again this often while it plays, so a missed report doesn't stop it. */
const RUMBLE_RESEND_MS = 500;

// Buttons, in the three bytes after the triggers and a counter.
const HAT_MASK = 0x0f;
const SQUARE = 0x10;
const CROSS = 0x20;
const CIRCLE = 0x40;
const TRIANGLE = 0x80;
const L1 = 0x01;
const R1 = 0x02;
const CREATE = 0x10;
const OPTIONS = 0x20;
const L3 = 0x40;
const R3 = 0x80;
const PS = 0x01;
const TOUCHPAD = 0x02;
const MUTE = 0x04;
// The Edge's own, in the third byte too.
const LEFT_FN = 0x10;
const RIGHT_FN = 0x20;
const LEFT_PADDLE = 0x40;
const RIGHT_PADDLE = 0x80;

// Output flags: what the report changes (the rest is left as it was).
const VALID0_RUMBLE = 0x01 | 0x02;
const VALID0_RIGHT_TRIGGER = 0x04;
const VALID0_LEFT_TRIGGER = 0x08;
const VALID1_LIGHTBAR = 0x04;
const VALID1_RELEASE_LEDS = 0x08;
const VALID1_PLAYERS = 0x10;
const VALID2_LIGHTBAR_SETUP = 0x02;
const VALID2_RUMBLE = 0x04;
/** Lightbar setup: fade out the blue it starts with. */
const LIGHTBAR_LIGHT_OUT = 0x02;

/** IEEE CRC-32, as the Bluetooth reports use. */
export function crc32(bytes: ArrayLike<number>, crc = 0): number {
  crc = ~crc;
  for (let i = 0; i < bytes.length; i++) {
    crc ^= bytes[i]!;
    for (let k = 0; k < 8; k++) crc = crc & 1 ? (crc >>> 1) ^ 0xedb88320 : crc >>> 1;
  }
  return ~crc >>> 0;
}

/** How a raw sensor reading becomes its unit: (raw - bias) * scale. */
export interface Calibration {
  gyro: { bias: number; scale: number }[];
  accel: { bias: number; scale: number }[];
}

export function defaultCalibration(): Calibration {
  return {
    gyro: [0, 1, 2].map(() => ({ bias: 0, scale: GYRO_DEFAULT })),
    accel: [0, 1, 2].map(() => ({ bias: 0, scale: ACCEL_DEFAULT })),
  };
}

/**
 * Feature report 0x05 (without its id): gyro biases, then each gyro axis's plus and minus, the speed's
 * plus and minus, then each accelerometer axis's plus and minus; all signed 16-bit. Gives rad/s and m/s²,
 * as SDL's LoadCalibrationData does, including its sanity check: if any axis's bias or sensitivity is
 * implausible, the whole calibration is ignored.
 */
export function parseCalibration(d: DataView): Calibration {
  const cal = defaultCalibration();
  if (d.byteLength < 34) return cal;
  const v = (at: number) => d.getInt16(at, true);
  const speed2x = v(18) + v(20);
  const raw: { bias: number; sens: number; divisor: number }[] = [];
  // Pitch, yaw and roll.
  for (let i = 0; i < 3; i++) raw.push({ bias: v(i * 2), sens: (speed2x * GYRO_RES_PER_DEGREE) / (v(6 + i * 4) - v(8 + i * 4)), divisor: 64 });
  for (let i = 0; i < 3; i++) {
    const plus = v(22 + i * 4);
    const range = plus - v(24 + i * 4);
    raw.push({ bias: plus - Math.trunc(range / 2), sens: (2 * ACCEL_RES_PER_G) / range, divisor: 1 });
  }
  if (raw.some((r) => !Number.isFinite(r.sens) || Math.abs(r.bias) > 1024 || Math.abs(1 - r.sens / r.divisor) > 0.5)) return cal;
  for (let i = 0; i < 3; i++) {
    cal.gyro[i] = { bias: raw[i]!.bias, scale: (raw[i]!.sens / GYRO_RES_PER_DEGREE) * (Math.PI / 180) };
    cal.accel[i] = { bias: raw[3 + i]!.bias, scale: (raw[3 + i]!.sens / ACCEL_RES_PER_G) * 9.80665 };
  }
  return cal;
}

/** What the output reports say, kept so each report can carry the whole of it. */
export interface Effects {
  /** Strong (low-frequency) and weak (high-frequency) motors, 0..1. */
  lo: number;
  hi: number;
  /** Set once the page colours the lightbar (until then it's the controller's own). */
  led: [number, number, number] | null;
  players: number | null;
  /** The 11-byte blocks, once set. */
  left: number[] | null;
  right: number[] | null;
}

/** The 47 bytes both output reports start with. `lightOut` is set on the first report that has a colour. */
export function commonOutput(fx: Effects, lightOut: boolean, releaseLeds = false): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(new ArrayBuffer(COMMON_BYTES));
  out[0] = VALID0_RUMBLE | (fx.right ? VALID0_RIGHT_TRIGGER : 0) | (fx.left ? VALID0_LEFT_TRIGGER : 0);
  out[1] = (fx.led ? VALID1_LIGHTBAR : 0) | (fx.players !== null ? VALID1_PLAYERS : 0) | (releaseLeds ? VALID1_RELEASE_LEDS : 0);
  out[2] = Math.round(clamp(fx.hi, 0, 1) * 255);
  out[3] = Math.round(clamp(fx.lo, 0, 1) * 255);
  if (fx.right) out.set(fx.right.slice(0, 11), 10);
  if (fx.left) out.set(fx.left.slice(0, 11), 21);
  out[38] = VALID2_RUMBLE | (fx.led && lightOut ? VALID2_LIGHTBAR_SETUP : 0);
  if (fx.led && lightOut) out[41] = LIGHTBAR_LIGHT_OUT;
  out[43] = (fx.players ?? 0) & 0x1f;
  if (fx.led) out.set(fx.led, 44);
  return out;
}

/** USB output report 0x02's data. */
export function usbOutput(common: Uint8Array): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(new ArrayBuffer(USB_OUTPUT_BYTES));
  out.set(common);
  return out;
}

/** Bluetooth output report 0x31's data: the sequence (4 bits, in the high nibble), the tag, the common part, padding and the CRC-32 of the whole report with its id, after the 0xA2 seed byte. */
export function btOutput(common: Uint8Array, seq: number): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(new ArrayBuffer(BT_OUTPUT_BYTES));
  out[0] = (seq & 0x0f) << 4;
  out[1] = BT_OUTPUT_TAG;
  out.set(common, 2);
  const crc = crc32([REPORT_BT_OUTPUT, ...out.subarray(0, BT_OUTPUT_BYTES - 4)], crc32([BT_CRC_SEED]));
  new DataView(out.buffer).setUint32(BT_OUTPUT_BYTES - 4, crc, true);
  return out;
}

const stick = (v: number) => clamp((v - 128) / (v < 128 ? 128 : 127), -1, 1);

/** Whether the descriptor has the Bluetooth reports (0x31): the same HID interface, wired, has none. */
function overBluetooth(device: HidDevice): boolean {
  const reports = (c: HidDevice["collections"][number]): boolean =>
    !![...(c.inputReports ?? []), ...(c.outputReports ?? [])].some((r) => r.reportId === REPORT_BT_STATE) ||
    !!c.children?.some(reports);
  return device.collections.some(reports);
}

export class DualSenseDriver implements HidDriver {
  readonly connected = true;
  readonly info: ControllerInfo;
  private readonly current: ControllerState = neutralState();
  private cal = defaultCalibration();
  private bluetooth: boolean;
  private readonly fx: Effects = { lo: 0, hi: 0, led: null, players: null, left: null, right: null };
  private lightOut = true;
  private seq = 0;
  private queue: Promise<void> = Promise.resolve();
  private rumbleTimer: ReturnType<typeof setInterval> | undefined;
  private rumbleStop: ReturnType<typeof setTimeout> | undefined;
  private closed = false;

  constructor(private readonly device: HidDevice) {
    this.bluetooth = overBluetooth(device);
    this.info = {
      name: device.productName || (device.productId === DUALSENSE_EDGE ? "DualSense Edge" : "DualSense"),
      backend: "webhid",
      type: "playstation",
      vendorId: device.vendorId,
      productId: device.productId,
      capabilities: { rumble: true, gyro: true, touchpad: true, battery: true, lightbar: true, triggers: true },
      mapped: true,
    };
    this.current.touch = [];
  }

  /** Reads the calibration; over Bluetooth that is also what turns the full reports on. */
  async open(): Promise<void> {
    try {
      this.cal = parseCalibration(await this.device.receiveFeatureReport(FEATURE_CALIBRATION));
    } catch {
      // Sensors in the controller's own units; the rest works without.
    }
  }

  async close(): Promise<void> {
    this.closed = true;
    clearInterval(this.rumbleTimer);
    clearTimeout(this.rumbleStop);
    // Everything of ours off: motors and triggers, and the lights back to the controller's.
    const touched = this.fx.lo || this.fx.hi || this.fx.led || this.fx.players !== null || this.fx.left || this.fx.right;
    if (!touched) return;
    const off: Effects = {
      lo: 0,
      hi: 0,
      led: null,
      players: null,
      left: this.fx.left && [...TRIGGER_EFFECT_OFF],
      right: this.fx.right && [...TRIGGER_EFFECT_OFF],
    };
    await this.write(commonOutput(off, false, !!(this.fx.led || this.fx.players !== null))).catch(() => {});
  }

  onReport(reportId: number, d: DataView): void {
    if (this.closed) return;
    if (reportId === REPORT_BT_STATE) {
      this.bluetooth = true;
      if (d.byteLength >= 1 + STATE_BYTES) this.full(d, 1);
    } else if (reportId === REPORT_USB_STATE) {
      // SDL: 10 or 78 bytes with the id is the short state (Bluetooth pads pad it to the long size).
      if (d.byteLength === SIMPLE_BYTES || d.byteLength === SIMPLE_PADDED_BYTES) this.simple(d);
      else if (d.byteLength >= STATE_BYTES) this.full(d, 0);
    }
  }

  /** The buttons the first two bytes and the hat share between the full and the short report. */
  private buttons(face: number, shoulders: number): void {
    const b = this.current.buttons;
    const hat = face & HAT_MASK;
    b[BTN.up] = hat === 0 || hat === 1 || hat === 7 ? 1 : 0;
    b[BTN.right] = hat >= 1 && hat <= 3 ? 1 : 0;
    b[BTN.down] = hat >= 3 && hat <= 5 ? 1 : 0;
    b[BTN.left] = hat >= 5 && hat <= 7 ? 1 : 0;
    b[BTN.west] = face & SQUARE ? 1 : 0;
    b[BTN.south] = face & CROSS ? 1 : 0;
    b[BTN.east] = face & CIRCLE ? 1 : 0;
    b[BTN.north] = face & TRIANGLE ? 1 : 0;
    b[BTN.leftShoulder] = shoulders & L1 ? 1 : 0;
    b[BTN.rightShoulder] = shoulders & R1 ? 1 : 0;
    b[BTN.back] = shoulders & CREATE ? 1 : 0;
    b[BTN.start] = shoulders & OPTIONS ? 1 : 0;
    b[BTN.leftStick] = shoulders & L3 ? 1 : 0;
    b[BTN.rightStick] = shoulders & R3 ? 1 : 0;
  }

  /** SDL: a trigger that reads 0 with its digital bit (L2 0x04, R2 0x08, in the shoulder byte) set is fully pulled. */
  private triggers(left: number, right: number, shoulders: number): void {
    const b = this.current.buttons;
    b[BTN.leftTrigger] = left === 0 && shoulders & 0x04 ? 1 : left / 255;
    b[BTN.rightTrigger] = right === 0 && shoulders & 0x08 ? 1 : right / 255;
  }

  /** The full state, from the sticks at `o`. */
  private full(d: DataView, o: number): void {
    const s = this.current;
    s.axes[0] = stick(d.getUint8(o));
    s.axes[1] = stick(d.getUint8(o + 1));
    s.axes[2] = stick(d.getUint8(o + 2));
    s.axes[3] = stick(d.getUint8(o + 3));
    this.buttons(d.getUint8(o + 7), d.getUint8(o + 8));
    this.triggers(d.getUint8(o + 4), d.getUint8(o + 5), d.getUint8(o + 8));
    const more = d.getUint8(o + 9);
    const b = s.buttons;
    b[BTN.guide] = more & PS ? 1 : 0;
    b[EXTRA.touchpadClick] = more & TOUCHPAD ? 1 : 0;
    b[EXTRA.mute] = more & MUTE ? 1 : 0;
    // Only the Edge has these.
    b[EXTRA.l4] = more & LEFT_PADDLE ? 1 : 0;
    b[EXTRA.r4] = more & RIGHT_PADDLE ? 1 : 0;
    b[EXTRA.l5] = more & LEFT_FN ? 1 : 0;
    b[EXTRA.r5] = more & RIGHT_FN ? 1 : 0;

    // SDL's sensor axes are the report's: x pitch, y yaw, z roll.
    const gyro = [0, 1, 2].map((i) => (d.getInt16(o + 15 + i * 2, true) - this.cal.gyro[i]!.bias) * this.cal.gyro[i]!.scale);
    const accel = [0, 1, 2].map((i) => (d.getInt16(o + 21 + i * 2, true) - this.cal.accel[i]!.bias) * this.cal.accel[i]!.scale);
    s.gyro = gyro as [number, number, number];
    s.accel = accel as [number, number, number];

    // Two fingers: a counter whose top bit is set while the slot is empty, then 12-bit x and y.
    const touch: TouchPoint[] = [];
    for (let id = 0; id < 2; id++) {
      const at = o + 32 + id * 4;
      if (d.getUint8(at) & 0x80) continue;
      const x = d.getUint8(at + 1) | ((d.getUint8(at + 2) & 0x0f) << 8);
      const y = (d.getUint8(at + 2) >> 4) | (d.getUint8(at + 3) << 4);
      touch.push({ id, x: clamp(x / TOUCH_W, 0, 1), y: clamp(y / TOUCH_H, 0, 1), down: true });
    }
    s.touch = touch;

    // Level 0..10 in the low nibble, what it's doing in the high one: 0 discharging, 1 charging, 2 full.
    const status = d.getUint8(o + 52);
    const charge = status >> 4;
    if (charge === 2) s.battery = 1;
    else if (charge <= 1) s.battery = Math.min((status & 0x0f) * 10 + 5, 100) / 100;
  }

  /** The Bluetooth short state: sticks, hat and buttons, PS and touchpad, triggers. */
  private simple(d: DataView): void {
    const s = this.current;
    for (let i = 0; i < 4; i++) s.axes[i] = stick(d.getUint8(i));
    this.buttons(d.getUint8(4), d.getUint8(5));
    const more = d.getUint8(6);
    s.buttons[BTN.guide] = more & PS ? 1 : 0;
    s.buttons[EXTRA.touchpadClick] = more & TOUCHPAD ? 1 : 0;
    this.triggers(d.getUint8(7), d.getUint8(8), d.getUint8(5));
  }

  state(): ControllerState {
    return this.current;
  }

  rumble(lo: number, hi: number, ms: number): void {
    clearTimeout(this.rumbleStop);
    clearInterval(this.rumbleTimer);
    if (ms <= 0 || (lo <= 0 && hi <= 0)) {
      this.fx.lo = this.fx.hi = 0;
      this.flush();
      return;
    }
    this.fx.lo = lo;
    this.fx.hi = hi;
    this.flush();
    this.rumbleTimer = setInterval(() => this.flush(), RUMBLE_RESEND_MS);
    this.rumbleStop = setTimeout(() => this.rumble(0, 0, 0), ms);
  }

  led(r: number, g: number, b: number): void {
    this.fx.led = [r, g, b];
    this.flush();
  }

  players(mask: number): void {
    this.fx.players = mask & 0x1f;
    this.flush();
  }

  trigger(side: Side, effect: number[]): void {
    if (effect.length !== 11) return;
    if (side === "left") this.fx.left = [...effect];
    else this.fx.right = [...effect];
    this.flush();
  }

  /** Writes the whole of what the controller should be doing. */
  private flush(): void {
    if (this.closed) return;
    const common = commonOutput(this.fx, this.lightOut);
    if (this.fx.led) this.lightOut = false;
    void this.write(common).catch(() => {});
  }

  /** In order: a Bluetooth report's sequence has to follow the last. */
  private write(common: Uint8Array): Promise<void> {
    const next = this.queue.then(() =>
      this.bluetooth
        ? this.device.sendReport(REPORT_BT_OUTPUT, btOutput(common, this.seq++))
        : this.device.sendReport(REPORT_USB_OUTPUT, usbOutput(common)),
    );
    this.queue = next.catch(() => {});
    return next;
  }
}

export const dualSenseFactory: HidDriverFactory = {
  name: "dualsense",
  matches: (device) => device.vendorId === SONY_VENDOR_ID && (device.productId === DUALSENSE || device.productId === DUALSENSE_EDGE),
  create: (device) => new DualSenseDriver(device),
};
