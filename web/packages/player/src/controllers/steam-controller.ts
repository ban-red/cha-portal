// The original Steam Controller (2015) over WebHID: wired, through the
// dongle, or over Bluetooth. Its controller interface is a vendor-defined HID
// collection: until a page claims it the controller does "lizard mode" (a
// keyboard and mouse), which is also why the Gamepad API sees nothing.
// Claiming it is a few feature reports; closing it sends the ones that
// bring lizard mode back.
//
// Ported from SDL3's SDL_hidapi_steam.c (zlib licence; see steam-protocol.ts).
// The input reports are the 64-byte `ValveInReport_t` (USB, dongle) or 20-byte
// segments of one (Bluetooth, report 3). The controller has no rumble motors.

import type { HidDevice, HidDriver, HidDriverFactory } from "./hid-types";
import {
  ACCEL_SCALE,
  GYRO_MODE_SEND_RAW_ACCEL,
  GYRO_MODE_SEND_RAW_GYRO,
  GYRO_SCALE,
  ID_CLEAR_DIGITAL_MAPPINGS,
  ID_DONGLE_GET_WIRELESS_STATE,
  ID_HAPTIC_PULSE,
  ID_LOAD_DEFAULT_SETTINGS,
  ID_SET_DEFAULT_DIGITAL_MAPPINGS,
  SETTING_IMU_MODE,
  SETTING_LEFT_TRACKPAD_MODE,
  SETTING_RIGHT_TRACKPAD_MODE,
  SETTING_SMOOTH_ABSOLUTE_MOUSE,
  SETTING_WIRELESS_PACKET_VERSION,
  STEAM_BLE,
  STEAM_DONGLE,
  STEAM_ORIGINAL,
  TRACKPAD_ABSOLUTE_MOUSE,
  TRACKPAD_NONE,
  VALVE_VENDOR_ID,
  axis16,
  settingsMessage,
  sleep,
} from "./steam-protocol";
import { BTN, EXTRA, clamp, neutralState, type ControllerInfo, type ControllerState, type Side, type TouchPoint } from "./types";

// ulButtons (the low 24 bits; the next two bytes are the triggers).
const MASK = {
  rightBumper: 0x000004,
  leftBumper: 0x000008,
  north: 0x000010,
  east: 0x000020,
  west: 0x000040,
  south: 0x000080,
  up: 0x000100,
  right: 0x000200,
  left: 0x000400,
  down: 0x000800,
  menu: 0x001000,
  steam: 0x002000,
  escape: 0x004000,
  leftGrip: 0x008000,
  rightGrip: 0x010000,
  leftPadClick: 0x020000,
  rightPadClick: 0x040000,
  leftPadDown: 0x080000,
  rightPadDown: 0x100000,
  stick: 0x400000,
  leftPadAndStick: 0x800000,
};

/** Both pads are rotated 15 degrees on the controller. */
const PAD_ROTATION = 0.261799;
const TRIGGER_MAX = 26000;
const REPORT_VERSION = 1;
const ID_STATE = 1;
const ID_WIRELESS = 3;
const ID_STATUS = 4;
const ID_BLE_STATE = 7;
/** sizeof(MsgFireHapticPulse). */
const HAPTIC_PULSE_BYTES = 10;
const BLE_REPORT = 3;
const BLE_SEGMENT_PAYLOAD = 18;
/** Dongles answer feature reports "not yet" while the radio round trips: SDL retries for 50 ms. */
const RADIO_RETRIES = 50;

function rotate(x: number, y: number, angle: number): [number, number] {
  return [Math.trunc(Math.cos(angle) * x - Math.sin(angle) * y), Math.trunc(Math.sin(angle) * x + Math.cos(angle) * y)];
}

const int16 = (v: number) => clamp(v, -32768, 32767);
const trigger = (raw: number) => clamp(((raw << 7) | raw) / TRIGGER_MAX, 0, 1);

/** What a state packet carries, kept between packets (the Bluetooth ones send only what changed). */
interface Sc1State {
  buttons: number;
  trigL: number;
  trigR: number;
  stick: [number, number];
  padL: [number, number];
  padR: [number, number];
  accel: [number, number, number];
  gyro: [number, number, number];
}

export class SteamControllerDriver implements HidDriver {
  connected: boolean;
  onConnection?: () => void;
  readonly info: ControllerInfo;
  private readonly kind: "wired" | "dongle" | "ble";
  private readonly current: ControllerState = neutralState();
  private readonly sc: Sc1State = {
    buttons: 0,
    trigL: 0,
    trigR: 0,
    stick: [0, 0],
    padL: [0, 0],
    padR: [0, 0],
    accel: [0, 0, 0],
    gyro: [0, 0, 0],
  };
  private prevStick: [number, number] = [0, 0];
  private prevPad: [number, number] = [0, 0];
  private lastPacket = -1;
  private bleBuffer = new Uint8Array(BLE_SEGMENT_PAYLOAD * 8);
  private bleNext = 0;
  private claimed = false;
  private closed = false;
  /** Where each pad was last touched: a lifted finger isn't sent. */
  private readonly touch: TouchPoint[] = [
    { id: 0, x: 0.5, y: 0.5, down: false },
    { id: 1, x: 0.5, y: 0.5, down: false },
  ];

  constructor(private readonly device: HidDevice) {
    this.kind = device.productId === STEAM_DONGLE ? "dongle" : STEAM_BLE.includes(device.productId) ? "ble" : "wired";
    // A dongle has no controller until one wakes up behind it.
    this.connected = this.kind !== "dongle";
    this.info = {
      name: device.productName || "Steam Controller",
      backend: "webhid",
      type: "steam",
      vendorId: device.vendorId,
      productId: device.productId,
      capabilities: { rumble: false, gyro: true, touchpad: true, battery: true },
      mapped: true,
    };
  }

  async open(): Promise<void> {
    if (this.kind === "dongle") {
      await this.send([ID_DONGLE_GET_WIRELESS_STATE, 0]).catch(() => {});
      return;
    }
    await this.claim();
  }

  async close(): Promise<void> {
    this.closed = true;
    if (!this.claimed) return;
    // Lizard mode again: the default mappings and settings, and the right pad as a mouse.
    await this.send([ID_SET_DEFAULT_DIGITAL_MAPPINGS, 0]).catch(() => {});
    await this.send([ID_LOAD_DEFAULT_SETTINGS, 0]).catch(() => {});
    await this.send(settingsMessage([SETTING_RIGHT_TRACKPAD_MODE, TRACKPAD_ABSOLUTE_MOUSE])).catch(() => {});
  }

  /** SDL's ResetSteamController, then the sensors on: no mouse, no keyboard, raw pads. */
  private async claim(): Promise<void> {
    try {
      await this.send([ID_CLEAR_DIGITAL_MAPPINGS, 0]);
      await this.send([ID_LOAD_DEFAULT_SETTINGS, 0]);
      await this.send(
        settingsMessage(
          [SETTING_WIRELESS_PACKET_VERSION, 2],
          [SETTING_LEFT_TRACKPAD_MODE, TRACKPAD_NONE],
          [SETTING_RIGHT_TRACKPAD_MODE, TRACKPAD_NONE],
          [SETTING_SMOOTH_ABSOLUTE_MOUSE, 0],
        ),
      );
      await this.send(settingsMessage([SETTING_IMU_MODE, GYRO_MODE_SEND_RAW_ACCEL | GYRO_MODE_SEND_RAW_GYRO]));
      this.claimed = true;
    } catch (err) {
      throw new Error(
        `Couldn't take over the Steam Controller (${err instanceof Error ? err.message : err}). Steam may be holding it: quit Steam and try again.`,
      );
    }
  }

  /** One feature report message: type, length, payload. Over Bluetooth it goes in 18-byte segments of report 3. */
  private async send(msg: number[]): Promise<void> {
    if (this.kind === "ble") {
      for (let at = 0, n = 0; at < msg.length; at += BLE_SEGMENT_PAYLOAD, n++) {
        const chunk = msg.slice(at, at + BLE_SEGMENT_PAYLOAD);
        const last = at + BLE_SEGMENT_PAYLOAD >= msg.length;
        const data = new Uint8Array(1 + BLE_SEGMENT_PAYLOAD);
        data[0] = 0x80 | n | (last ? 0x40 : 0);
        data.set(chunk, 1);
        await this.device.sendFeatureReport(BLE_REPORT, data);
      }
      return;
    }
    // Feature reports are always 64 bytes, report id 0.
    const data = new Uint8Array(64);
    data.set(msg);
    const attempts = this.kind === "dongle" ? RADIO_RETRIES : 3;
    for (let i = 0; ; i++) {
      try {
        await this.device.sendFeatureReport(0, data);
        return;
      } catch (err) {
        if (i + 1 >= attempts) throw err;
        await sleep(1);
      }
    }
  }

  private setConnected(connected: boolean): void {
    if (this.connected === connected) return;
    this.connected = connected;
    if (connected && this.kind === "dongle" && !this.claimed && !this.closed) {
      void this.claim().catch(() => {});
    }
    if (!connected) this.claimed = false;
    this.onConnection?.();
  }

  onReport(reportId: number, data: DataView): void {
    if (this.closed) return;
    if (this.kind === "ble") {
      if (reportId !== BLE_REPORT) return;
      const packet = this.assemble(data);
      if (packet) this.parse(packet);
      return;
    }
    this.parse(data);
  }

  /** Bluetooth: segments of 18 bytes, numbered, the last one flagged. */
  private assemble(data: DataView): DataView | null {
    if (data.byteLength < 1 + BLE_SEGMENT_PAYLOAD) return null;
    const header = data.getUint8(0);
    if (!(header & 0x80)) return null;
    const n = header & 7;
    if (n !== this.bleNext) {
      this.bleBuffer.fill(0);
      this.bleNext = 0;
      if (n) return null;
    }
    for (let i = 0; i < BLE_SEGMENT_PAYLOAD; i++) this.bleBuffer[n * BLE_SEGMENT_PAYLOAD + i] = data.getUint8(1 + i);
    if (header & 0x40) {
      this.bleNext = 0;
      return new DataView(this.bleBuffer.slice(0, (n + 1) * BLE_SEGMENT_PAYLOAD).buffer);
    }
    this.bleNext++;
    return null;
  }

  /** SDL's UpdateSteamControllerState. */
  private parse(d: DataView): void {
    if (d.byteLength < 4) return;
    if (d.getUint16(0, true) !== REPORT_VERSION) {
      // A Bluetooth state: the first nibble is the report number (4) and the rest, with the next byte, says which parts follow.
      if ((d.getUint8(0) & 0x0f) === 4) this.parseBleState(d);
      return;
    }
    const type = d.getUint8(2);
    switch (type) {
      case ID_STATE:
      case ID_BLE_STATE: {
        if (d.byteLength < 24) return;
        const packet = d.getUint32(4, true);
        if (packet === this.lastPacket) return;
        this.lastPacket = packet;
        if (!this.connected) this.setConnected(true);
        this.formatUntilGyro(d);
        if (type === ID_STATE && d.byteLength >= 40) {
          for (let i = 0; i < 3; i++) {
            this.sc.accel[i] = d.getInt16(28 + i * 2, true);
            this.sc.gyro[i] = d.getInt16(34 + i * 2, true);
          }
        } else if (type === ID_BLE_STATE && d.byteLength >= 33) {
          // One of the three sensor vectors per packet, by type: 2 accel, 3 gyro.
          const kind = d.getUint8(24);
          const target = kind === 2 ? this.sc.accel : kind === 3 ? this.sc.gyro : null;
          if (target) for (let i = 0; i < 3; i++) target[i] = d.getInt16(25 + i * 2, true);
        }
        this.publish();
        break;
      }
      case ID_WIRELESS:
        // Event 1: disconnected; 2 and 3: connected, or newly paired.
        if (d.getUint8(3) >= 1) this.setConnected(d.getUint8(4) !== 1);
        break;
      case ID_STATUS:
        if (d.byteLength >= 15) {
          this.current.battery = clamp(d.getUint8(14) / 100, 0, 1);
        }
        break;
    }
  }

  /** The first part of a state packet (SDL's FormatStatePacketUntilGyro): the left side is the stick or the pad, by a flag. */
  private formatUntilGyro(d: DataView): void {
    const sc = this.sc;
    let buttons = d.getUint8(8) | (d.getUint8(9) << 8) | (d.getUint8(10) << 16);
    const left: [number, number] = [d.getInt16(16, true), d.getInt16(18, true)];
    const right: [number, number] = [d.getInt16(20, true), d.getInt16(22, true)];
    const both = !!(buttons & MASK.leftPadAndStick);
    let stick: [number, number] = [0, 0];
    let pad: [number, number] = [0, 0];
    if (buttons & MASK.leftPadDown) {
      pad = this.prevPad = left;
      if (both) stick = this.prevStick;
      else this.prevStick = [0, 0];
    } else {
      stick = this.prevStick = left;
      if (both) {
        pad = this.prevPad;
      } else {
        this.prevPad = [0, 0];
        // Old controllers send the pad's click for the stick's when the pad is idle.
        if (buttons & MASK.leftPadClick) buttons = (buttons & ~MASK.leftPadClick) | MASK.stick;
      }
    }
    if (both) buttons |= MASK.leftPadDown;
    sc.buttons = buttons;
    sc.stick = stick;
    sc.padL = this.pad(pad, -PAD_ROTATION, !!(buttons & MASK.leftPadDown));
    sc.padR = this.pad(right, PAD_ROTATION, !!(buttons & MASK.rightPadDown));
    sc.trigL = trigger(d.getUint8(11));
    sc.trigR = trigger(d.getUint8(12));
  }

  private pad(p: [number, number], angle: number, down: boolean): [number, number] {
    const [x, y] = rotate(p[0], p[1], angle);
    const offset = down ? 1000 : 0;
    return [int16(x + offset), int16(y + offset)];
  }

  /** SDL's UpdateBLESteamControllerState: only the parts the mask names are in the packet. */
  private parseBleState(d: DataView): void {
    const sc = this.sc;
    const mask = (d.getUint8(0) & 0xf0) | (d.getUint8(1) << 8);
    let at = 2;
    const need = (n: number) => at + n <= d.byteLength;
    if (mask & 0x10 && need(3)) {
      sc.buttons = d.getUint8(at) | (d.getUint8(at + 1) << 8) | (d.getUint8(at + 2) << 16);
      at += 3;
    }
    if (mask & 0x20 && need(2)) {
      sc.trigL = trigger(d.getUint8(at));
      sc.trigR = trigger(d.getUint8(at + 1));
      at += 2;
    }
    if (mask & 0x40) at += 3;
    if (mask & 0x80 && need(4)) {
      sc.stick = [d.getInt16(at, true), d.getInt16(at + 2, true)];
      at += 4;
    }
    if (mask & 0x100 && need(4)) {
      sc.padL = this.pad([d.getInt16(at, true), d.getInt16(at + 2, true)], -PAD_ROTATION, !!(sc.buttons & MASK.leftPadDown));
      at += 4;
    }
    if (mask & 0x200 && need(4)) {
      sc.padR = this.pad([d.getInt16(at, true), d.getInt16(at + 2, true)], PAD_ROTATION, !!(sc.buttons & MASK.rightPadDown));
      at += 4;
    }
    if (mask & 0x400 && need(6)) {
      for (let i = 0; i < 3; i++) sc.accel[i] = d.getInt16(at + i * 2, true);
      at += 6;
    }
    if (mask & 0x800 && need(6)) {
      for (let i = 0; i < 3; i++) sc.gyro[i] = d.getInt16(at + i * 2, true);
    }
    if (!this.connected) this.setConnected(true);
    this.publish();
  }

  /** SDL's mapping to a gamepad: the right pad is the right stick. */
  private publish(): void {
    const { sc, current: s } = this;
    const b = sc.buttons;
    const held = (m: number) => (b & m ? 1 : 0);
    s.buttons[BTN.south] = held(MASK.south);
    s.buttons[BTN.east] = held(MASK.east);
    s.buttons[BTN.west] = held(MASK.west);
    s.buttons[BTN.north] = held(MASK.north);
    s.buttons[BTN.leftShoulder] = held(MASK.leftBumper);
    s.buttons[BTN.rightShoulder] = held(MASK.rightBumper);
    s.buttons[BTN.leftTrigger] = sc.trigL;
    s.buttons[BTN.rightTrigger] = sc.trigR;
    s.buttons[BTN.back] = held(MASK.menu);
    s.buttons[BTN.start] = held(MASK.escape);
    s.buttons[BTN.leftStick] = held(MASK.stick);
    s.buttons[BTN.rightStick] = held(MASK.rightPadClick);
    s.buttons[BTN.up] = held(MASK.up);
    s.buttons[BTN.down] = held(MASK.down);
    s.buttons[BTN.left] = held(MASK.left);
    s.buttons[BTN.right] = held(MASK.right);
    s.buttons[BTN.guide] = held(MASK.steam);
    s.buttons[EXTRA.touchpadClick] = held(MASK.rightPadClick);
    // The left pad's click is the stick's while the pad is idle (see formatUntilGyro): only a touched pad clicks.
    s.buttons[EXTRA.leftPadClick] = b & MASK.leftPadDown ? held(MASK.leftPadClick) : 0;
    s.buttons[EXTRA.l4] = held(MASK.leftGrip);
    s.buttons[EXTRA.r4] = held(MASK.rightGrip);
    s.axes[0] = axis16(sc.stick[0]);
    s.axes[1] = axis16(~sc.stick[1]);
    s.axes[2] = axis16(sc.padR[0]);
    s.axes[3] = axis16(~sc.padR[1]);
    // SDL's sensor axes: x pitch, y yaw, z roll.
    s.gyro = [sc.gyro[0] * GYRO_SCALE, sc.gyro[2] * GYRO_SCALE, sc.gyro[1] * GYRO_SCALE];
    s.accel = [sc.accel[0] * ACCEL_SCALE, sc.accel[2] * ACCEL_SCALE, -sc.accel[1] * ACCEL_SCALE];
    const pads: [number, boolean, [number, number]][] = [
      [0, !!(b & MASK.leftPadDown), sc.padL],
      [1, !!(b & MASK.rightPadDown), sc.padR],
    ];
    for (const [id, down, p] of pads) {
      const t = this.touch[id]!;
      // A lifted finger keeps its last position.
      if (down) {
        t.x = clamp(p[0] / 65536 + 0.5, 0, 1);
        t.y = clamp(-p[1] / 65536 + 0.5, 0, 1);
      }
      t.down = down;
    }
    s.touch = this.touch.filter((t) => t.down).map((t) => ({ ...t }));
  }

  /**
   * SDL's MsgFireHapticPulse in feature report 0x8F (ID_TRIGGER_HAPTIC_PULSE), a
   * 10-byte packed payload: which_pad, pulse_duration, pulse_interval and
   * pulse_count (16-bit µs, µs, count), dBgain (16-bit, here 0) and priority
   * (here 0). SDL's headers don't say which pad is 0: right is 0 and left 1 here.
   */
  haptic(side: Side, amp: number, onUs: number, offUs: number, count: number): void {
    if (amp <= 0 || !this.claimed || this.closed) return;
    const u16 = (v: number) => clamp(Math.round(v), 0, 0xffff);
    const [on, off, n] = [u16(onUs), u16(offUs), u16(count)];
    void this.send([ID_HAPTIC_PULSE, HAPTIC_PULSE_BYTES, side === "left" ? 1 : 0, on & 0xff, on >> 8, off & 0xff, off >> 8, n & 0xff, n >> 8, 0, 0, 0]).catch(() => {});
  }

  state(): ControllerState {
    return this.current;
  }
}

export const steamControllerFactory: HidDriverFactory = {
  name: "steam-controller",
  matches: (device) => device.vendorId === VALVE_VENDOR_ID && STEAM_ORIGINAL.includes(device.productId),
  create: (device) => new SteamControllerDriver(device),
};
