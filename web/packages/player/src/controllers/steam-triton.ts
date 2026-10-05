// The 2026 Steam Controller (SDL3 calls it Triton) over WebHID: wired,
// Bluetooth, through the puck or the receiver. Until a page claims it, it
// does "lizard mode" (a keyboard and mouse), which is also why the Gamepad API
// sees nothing. Claiming it is a feature report that turns lizard mode off,
// which the controller's watchdog undoes after a few seconds, so it is sent
// again every 3 s; closing just stops sending it.
//
// Ported from SDL3's SDL_hidapi_steam_triton.c (zlib licence; see
// steam-protocol.ts). Input reports are numbered: 0x42, 0x45 and 0x47 are
// state (buttons, sticks, triggers, both pads, gyro and accelerometer), 0x43
// the battery, 0x46 and 0x79 a puck's or receiver's wireless status.
// Rumble is output report 0x80, resent every 40 ms, as the controller's own
// safety timeout is about 50 ms.

import type { HidDevice, HidDriver, HidDriverFactory } from "./hid-types";
import {
  ACCEL_SCALE,
  GYRO_MODE_SEND_RAW_ACCEL,
  GYRO_MODE_SEND_RAW_GYRO,
  GYRO_SCALE,
  LIZARD_MODE_OFF,
  SETTING_IMU_MODE,
  SETTING_LIZARD_MODE,
  STEAM_TRITON,
  TRITON_PUCK,
  TRITON_RECEIVER,
  VALVE_VENDOR_ID,
  axis16,
  settingsMessage,
} from "./steam-protocol";
import { BTN, clamp, neutralState, type ControllerInfo, type ControllerState, type TouchPoint } from "./types";

const REPORT_STATE = 0x42;
const REPORT_BATTERY = 0x43;
const REPORT_STATE_BLE = 0x45;
const REPORT_WIRELESS_X = 0x46;
const REPORT_STATE_TIMESTAMP = 0x47;
const REPORT_WIRELESS = 0x79;
const REPORT_RUMBLE = 0x80;
/** Lizard mode and the sensors go in feature report 1. */
const FEATURE_REPORT = 1;
const FEATURE_REPORT_BYTES = 63;

const LIZARD_RESEND_MS = 3000;
const RUMBLE_RESEND_MS = 40;
/** The state structs are 45 bytes, with and without the 32-bit timestamp. */
const STATE_BYTES = 45;
const WIRELESS_DISCONNECT = 1;
const WIRELESS_CONNECT = 2;

// The 32-bit button field.
const BTN_A = 0x00000001;
const BTN_B = 0x00000002;
const BTN_X = 0x00000004;
const BTN_Y = 0x00000008;
const BTN_R3 = 0x00000020;
const BTN_VIEW = 0x00000040;
const BTN_R = 0x00000200;
const BTN_DPAD_DOWN = 0x00000400;
const BTN_DPAD_RIGHT = 0x00000800;
const BTN_DPAD_LEFT = 0x00001000;
const BTN_DPAD_UP = 0x00002000;
const BTN_MENU = 0x00004000;
const BTN_L3 = 0x00008000;
const BTN_STEAM = 0x00010000;
const BTN_L = 0x00080000;
const RIGHT_PAD_TOUCH = 0x00200000;
const LEFT_PAD_TOUCH = 0x02000000;

export class SteamTritonDriver implements HidDriver {
  connected: boolean;
  onConnection?: () => void;
  readonly info: ControllerInfo;
  private readonly dongle: boolean;
  private readonly current: ControllerState = neutralState();
  private readonly touch: TouchPoint[] = [
    { id: 0, x: 0.5, y: 0.5, down: false },
    { id: 1, x: 0.5, y: 0.5, down: false },
  ];
  private lizardTimer: ReturnType<typeof setInterval> | undefined;
  private rumbleTimer: ReturnType<typeof setInterval> | undefined;
  private rumbleStop: ReturnType<typeof setTimeout> | undefined;
  private motors: [number, number] = [0, 0];
  private closed = false;

  constructor(private readonly device: HidDevice) {
    this.dongle = device.productId === TRITON_PUCK || device.productId === TRITON_RECEIVER;
    // A puck or receiver has no controller until one wakes up behind it.
    this.connected = !this.dongle;
    this.info = {
      name: device.productName || "Steam Controller",
      backend: "webhid",
      type: "steam",
      vendorId: device.vendorId,
      productId: device.productId,
      capabilities: { rumble: true, gyro: true, touchpad: true, battery: true },
      mapped: true,
    };
  }

  async open(): Promise<void> {
    try {
      await this.claim();
    } catch (err) {
      // Nothing is behind a dongle yet, so it may not answer; the others should.
      if (!this.dongle) {
        throw new Error(
          `Couldn't take over the Steam Controller (${err instanceof Error ? err.message : err}). Steam may be holding it: quit Steam and try again.`,
        );
      }
    }
    this.lizardTimer = setInterval(() => {
      if (this.connected) void this.lizardOff().catch(() => {});
    }, LIZARD_RESEND_MS);
  }

  async close(): Promise<void> {
    this.closed = true;
    clearInterval(this.lizardTimer);
    clearInterval(this.rumbleTimer);
    clearTimeout(this.rumbleStop);
    if (this.motors[0] || this.motors[1]) await this.writeRumble(0, 0).catch(() => {});
    // Lizard mode comes back by itself when the resends stop.
  }

  private async claim(): Promise<void> {
    await this.lizardOff();
    await this.feature(settingsMessage([SETTING_IMU_MODE, GYRO_MODE_SEND_RAW_ACCEL | GYRO_MODE_SEND_RAW_GYRO]));
  }

  private lizardOff(): Promise<void> {
    return this.feature(settingsMessage([SETTING_LIZARD_MODE, LIZARD_MODE_OFF]));
  }

  private feature(msg: number[]): Promise<void> {
    const data = new Uint8Array(FEATURE_REPORT_BYTES);
    data.set(msg);
    return this.device.sendFeatureReport(FEATURE_REPORT, data);
  }

  private setConnected(connected: boolean): void {
    if (this.connected === connected) return;
    this.connected = connected;
    if (connected && this.dongle && !this.closed) void this.claim().catch(() => {});
    this.onConnection?.();
  }

  onReport(reportId: number, d: DataView): void {
    if (this.closed) return;
    switch (reportId) {
      case REPORT_STATE:
      case REPORT_STATE_BLE:
        if (d.byteLength >= STATE_BYTES) this.state32(d, false);
        break;
      case REPORT_STATE_TIMESTAMP:
        if (d.byteLength >= STATE_BYTES) this.state32(d, true);
        break;
      case REPORT_BATTERY:
        // Byte 0 is the charge state, byte 1 the level in percent.
        if (d.byteLength >= 2) this.current.battery = clamp(d.getUint8(1) / 100, 0, 1);
        break;
      case REPORT_WIRELESS_X:
      case REPORT_WIRELESS:
        if (d.byteLength >= 1) {
          const state = d.getUint8(0);
          if (state === WIRELESS_CONNECT) this.setConnected(true);
          else if (state === WIRELESS_DISCONNECT) this.setConnected(false);
        }
        break;
    }
  }

  /**
   * A state report: seq 1, buttons 4, triggers 2×2, sticks 4×2, then (with the
   * 16-bit pad timestamp, at 17..18, in the timestamped one) both pads
   * (x, y, pressure), the IMU's timestamp, accelerometer and gyro.
   */
  private state32(d: DataView, timestamped: boolean): void {
    if (!this.connected) this.setConnected(true);
    const s = this.current;
    const buttons = d.getUint32(1, true);
    const held = (m: number) => (buttons & m ? 1 : 0);
    s.buttons[BTN.south] = held(BTN_A);
    s.buttons[BTN.east] = held(BTN_B);
    s.buttons[BTN.west] = held(BTN_X);
    s.buttons[BTN.north] = held(BTN_Y);
    s.buttons[BTN.leftShoulder] = held(BTN_L);
    s.buttons[BTN.rightShoulder] = held(BTN_R);
    // SDL's choice: its Back is the controller's menu button and its Start the view button.
    s.buttons[BTN.back] = held(BTN_MENU);
    s.buttons[BTN.start] = held(BTN_VIEW);
    s.buttons[BTN.guide] = held(BTN_STEAM);
    s.buttons[BTN.leftStick] = held(BTN_L3);
    s.buttons[BTN.rightStick] = held(BTN_R3);
    s.buttons[BTN.up] = held(BTN_DPAD_UP);
    s.buttons[BTN.down] = held(BTN_DPAD_DOWN);
    s.buttons[BTN.left] = held(BTN_DPAD_LEFT);
    s.buttons[BTN.right] = held(BTN_DPAD_RIGHT);
    s.buttons[BTN.leftTrigger] = clamp(d.getInt16(5, true) / 32767, 0, 1);
    s.buttons[BTN.rightTrigger] = clamp(d.getInt16(7, true) / 32767, 0, 1);
    s.axes[0] = axis16(d.getInt16(9, true));
    s.axes[1] = axis16(-d.getInt16(11, true));
    s.axes[2] = axis16(d.getInt16(13, true));
    s.axes[3] = axis16(-d.getInt16(15, true));

    const pads = timestamped ? 19 : 17;
    const sides: [number, number, number][] = [
      [0, pads, buttons & LEFT_PAD_TOUCH],
      [1, pads + 6, buttons & RIGHT_PAD_TOUCH],
    ];
    for (const [id, at, down] of sides) {
      const t = this.touch[id]!;
      if (down) {
        t.x = clamp(d.getInt16(at, true) / 65536 + 0.5, 0, 1);
        t.y = clamp(-d.getInt16(at + 2, true) / 65536 + 0.5, 0, 1);
      }
      t.down = !!down;
    }
    s.touch = this.touch.map((t) => ({ ...t }));

    // After both pads and the IMU's timestamp: accelerometer, then gyro, at 33 either way.
    const sensors = 33;
    const v = (i: number) => d.getInt16(sensors + i * 2, true);
    s.accel = [v(0) * ACCEL_SCALE, v(2) * ACCEL_SCALE, -v(1) * ACCEL_SCALE];
    s.gyro = [v(3) * GYRO_SCALE, v(5) * GYRO_SCALE, -v(4) * GYRO_SCALE];
  }

  state(): ControllerState {
    return this.current;
  }

  rumble(lo: number, hi: number, ms: number): void {
    clearTimeout(this.rumbleStop);
    clearInterval(this.rumbleTimer);
    if (ms <= 0 || (lo <= 0 && hi <= 0)) {
      this.motors = [0, 0];
      void this.writeRumble(0, 0).catch(() => {});
      return;
    }
    this.motors = [lo, hi];
    const play = () => void this.writeRumble(this.motors[0], this.motors[1]).catch(() => {});
    play();
    this.rumbleTimer = setInterval(play, RUMBLE_RESEND_MS);
    this.rumbleStop = setTimeout(() => this.rumble(0, 0, 0), ms);
  }

  /** Output report 0x80: type 0, intensity 0, then each side's speed (16-bit) and gain (0). */
  private writeRumble(lo: number, hi: number): Promise<void> {
    const data = new DataView(new ArrayBuffer(9));
    data.setUint16(3, Math.round(clamp(lo, 0, 1) * 65535), true);
    data.setUint16(6, Math.round(clamp(hi, 0, 1) * 65535), true);
    return this.device.sendReport(REPORT_RUMBLE, data);
  }
}

export const steamTritonFactory: HidDriverFactory = {
  name: "steam-triton",
  matches: (device) => device.vendorId === VALVE_VENDOR_ID && STEAM_TRITON.includes(device.productId),
  create: (device) => new SteamTritonDriver(device),
};
