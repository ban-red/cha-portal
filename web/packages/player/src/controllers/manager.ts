// All controllers from every backend, as one set: stable slots 0..3 (the
// streamer makes one virtual pad per slot), the same pad not counted twice
// when two backends see it (WebHID's driver wins), and the messages that go up
// the control channel when a pad changes.

import { GamepadApiBackend } from "./gamepad-api";
import { WebHidBackend } from "./webhid";
import {
  neutralState,
  round3,
  type BackendController,
  type ControllerBackend,
  type ControllerInfo,
  type ControllerState,
  type RawReport,
} from "./types";

export const MAX_SLOTS = 4;

/** Chrome samples pads at 250 Hz; polling at that rate adds no delay. */
const POLL_MS = 4;
/** Gyro (rad/s) and accelerometer (m/s²) readings differ by noise even at rest; smaller changes aren't worth a message. */
const GYRO_EPSILON = 0.005;
const ACCEL_EPSILON = 0.05;

export interface ManagedController {
  /** Stable while the pad stays connected. */
  id: string;
  info: ControllerInfo;
  /** The pad's number in the environment, or null when all four are taken. */
  slot: number | null;
}

type Send = (msg: Record<string, unknown>) => void;

export interface ManagerOptions {
  /** Where pad messages go. Without it the manager only watches (the Controllers page). */
  send?: Send;
  /** Defaults: the Gamepad API and WebHID. */
  backends?: ControllerBackend[];
  /** Whether the page has focus; unfocused pads are released. Default: `document.hasFocus()`. */
  focused?: () => boolean;
}

/** The message for one pad's state (`gone` pads send just that). */
export function padMessage(slot: number, info: ControllerInfo, s: ControllerState): Record<string, unknown> {
  const msg: Record<string, unknown> = { k: "pad", i: slot, b: s.buttons.map(round3), a: s.axes.map(round3) };
  // Not `t`: that is the control channel's own message type ("input").
  msg.ty = info.type;
  if (s.gyro) msg.gyro = s.gyro.map((v) => Math.round(v * 1000) / 1000);
  if (s.accel) msg.accel = s.accel.map((v) => Math.round(v * 1000) / 1000);
  if (s.touch) msg.touch = s.touch.map((p) => ({ id: p.id, x: round3(p.x), y: round3(p.y), down: p.down }));
  if (s.battery !== undefined) msg.bat = Math.round(s.battery * 100) / 100;
  return msg;
}

export const goneMessage = (slot: number): Record<string, unknown> => ({ k: "pad", i: slot, gone: true });

/** What was last sent for a slot, to tell whether anything changed. */
interface Sent {
  key: string;
  gyro?: number[];
  accel?: number[];
}

const near = (a: number[] | undefined, b: number[] | undefined, eps: number) =>
  !!a && !!b && a.every((v, i) => Math.abs(v - b[i]!) <= eps);

export class ControllerManager {
  readonly backends: ControllerBackend[];
  private readonly send: Send | undefined;
  private readonly focused: () => boolean;
  /** Every controller of every backend, by key. */
  private readonly all = new Map<string, { backend: ControllerBackend; pad: BackendController }>();
  private readonly slots = new Map<string, number>();
  private visible: ManagedController[] = [];
  private readonly sent = new Map<number, Sent>();
  private readonly listeners = new Set<(list: ManagedController[]) => void>();
  private timer: ReturnType<typeof setInterval> | undefined;
  private running = false;
  private wasFocused = true;

  constructor(options: ManagerOptions = {}) {
    this.backends = options.backends ?? [new GamepadApiBackend(), new WebHidBackend()];
    this.send = options.send;
    this.focused = options.focused ?? (() => typeof document === "undefined" || document.hasFocus());
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    for (const backend of this.backends) {
      backend.start({
        arrived: (pad) => {
          if (!this.running) return;
          this.all.set(pad.key, { backend, pad });
          this.refresh();
        },
        removed: (pad) => {
          if (!this.running) return;
          this.all.delete(pad.key);
          this.refresh();
        },
      });
    }
    this.timer = setInterval(() => this.tick(), POLL_MS);
  }

  /** Stops, and lets go of every pad remotely and locally (a Steam Controller goes back to lizard mode). */
  stop(): void {
    if (!this.running) return;
    this.running = false;
    clearInterval(this.timer);
    this.releaseAll();
    for (const backend of this.backends) backend.stop();
    this.all.clear();
    this.visible = [];
    this.listeners.clear();
  }

  /** The controllers now, in slot order (those without a slot last). */
  controllers(): ManagedController[] {
    return this.visible;
  }

  /** Called with the list whenever a controller arrives or leaves; returns what ends the subscription. */
  onChange(listener: (list: ManagedController[]) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  state(id: string): ControllerState {
    return this.all.get(id)?.pad.state() ?? neutralState();
  }

  raw(id: string): RawReport | null {
    return this.all.get(id)?.pad.raw() ?? null;
  }

  rumbleController(id: string, lo: number, hi: number, ms: number): void {
    this.all.get(id)?.pad.rumble(lo, hi, ms);
  }

  /** The streamer's rumble for a slot. */
  rumble(slot: number, lo: number, hi: number, ms: number): void {
    const mine = this.visible.find((c) => c.slot === slot);
    if (mine) this.rumbleController(mine.id, clampUnit(lo), clampUnit(hi), Math.max(0, ms));
  }

  /** The WebHID backend, when this page has one. */
  get webhid(): WebHidBackend | null {
    return (this.backends.find((b) => b instanceof WebHidBackend) as WebHidBackend | undefined) ?? null;
  }

  /** Why WebHID can't be used here, or null. */
  get hidUnavailable(): string | null {
    // `unavailable` is null when all is well: not a reason to fall back on.
    const hid = this.webhid;
    return hid ? hid.unavailable : "WebHID isn't available.";
  }

  /** Opens the device picker (a user gesture must be on the stack). Returns how many controllers it added. */
  async connectHid(): Promise<number> {
    const hid = this.webhid;
    if (!hid) throw new Error("WebHID isn't available.");
    return hid.connect();
  }

  /** Sends every pad's state again, as after gaining the controls. */
  resync(): void {
    this.sent.clear();
  }

  /** Slots and duplicates, after the set of pads changed. */
  private refresh(): void {
    const hidCounts = new Map<string, number>();
    const idKey = (i: ControllerInfo) => `${i.vendorId}:${i.productId}`;
    const pads = [...this.all.values()];
    for (const { pad } of pads) {
      if (pad.info.backend === "webhid" && pad.info.vendorId !== undefined) {
        hidCounts.set(idKey(pad.info), (hidCounts.get(idKey(pad.info)) ?? 0) + 1);
      }
    }
    // A pad both backends see: the driver's version is the one that counts.
    const shown: BackendController[] = [];
    for (const { pad } of pads) {
      const left = hidCounts.get(idKey(pad.info)) ?? 0;
      if (pad.info.backend === "gamepad" && pad.info.vendorId !== undefined && left > 0) {
        hidCounts.set(idKey(pad.info), left - 1);
        continue;
      }
      shown.push(pad);
    }
    const keys = new Set(shown.map((p) => p.key));
    for (const [key, slot] of this.slots) {
      if (keys.has(key)) continue;
      this.slots.delete(key);
      this.sendNow(goneMessage(slot));
      this.sent.delete(slot);
    }
    for (const pad of shown) {
      if (this.slots.has(pad.key)) continue;
      const taken = new Set(this.slots.values());
      for (let i = 0; i < MAX_SLOTS; i++) {
        if (taken.has(i)) continue;
        this.slots.set(pad.key, i);
        break;
      }
    }
    this.visible = shown
      .map((pad) => ({ id: pad.key, info: pad.info, slot: this.slots.get(pad.key) ?? null }))
      .sort((a, b) => (a.slot ?? MAX_SLOTS) - (b.slot ?? MAX_SLOTS));
    for (const l of this.listeners) l(this.visible);
  }

  private sendNow(msg: Record<string, unknown>): void {
    this.send?.(msg);
  }

  /** Lets go of everything held remotely. */
  private releaseAll(): void {
    for (const slot of this.sent.keys()) this.sendNow(goneMessage(slot));
    this.sent.clear();
  }

  /** One pass of the loop `start()` runs every few milliseconds: look for pads, send what changed. */
  tick(): void {
    for (const backend of this.backends) backend.poll?.();
    if (!this.send) return;
    // Unfocused pages get no pad updates from the Gamepad API: let go rather than stick.
    const focused = this.focused();
    if (!focused) {
      if (this.wasFocused) this.releaseAll();
      this.wasFocused = false;
      return;
    }
    this.wasFocused = true;
    for (const c of this.visible) {
      if (c.slot === null) continue;
      const s = this.state(c.id);
      const key = `${s.buttons.map(round3).join(",")}|${s.axes.map(round3).join(",")}|${s.touch?.map((p) => `${round3(p.x)},${round3(p.y)},${+p.down}`).join(";") ?? ""}|${s.battery === undefined ? "" : Math.round(s.battery * 100)}`;
      const last = this.sent.get(c.slot);
      const motion = !!last && near(s.gyro, last.gyro, GYRO_EPSILON) && near(s.accel, last.accel, ACCEL_EPSILON);
      const sensors = s.gyro === undefined && s.accel === undefined;
      if (last && last.key === key && (sensors || motion)) continue;
      this.sent.set(c.slot, { key, gyro: s.gyro && [...s.gyro], accel: s.accel && [...s.accel] });
      this.sendNow(padMessage(c.slot, c.info, s));
    }
  }
}

const clampUnit = (v: number) => (Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0);
