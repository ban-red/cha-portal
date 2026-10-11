// All controllers from every backend, as one set: stable slots 0..3 (the
// streamer makes one virtual pad per slot), the same pad not counted twice
// when two backends see it (WebHID's driver wins), and the messages that go up
// the control channel when a pad changes.

import { GamepadApiBackend } from "./gamepad-api";
import { HidRelay, STEAM_LIZARD_REPORTS, type HidRequest } from "./hid-relay";
import { WebHidBackend } from "./webhid";
import {
  hapticAsRumble,
  neutralState,
  round3,
  type BackendController,
  type ControllerBackend,
  type ControllerInfo,
  type ControllerState,
  type RawReport,
  type Side,
} from "./types";

export const MAX_SLOTS = 4;

/** Chrome samples pads at 250 Hz; polling at that rate adds no delay. */
const POLL_MS = 4;
/** Gyro (rad/s) and accelerometer (m/s²) readings differ by noise even at rest; smaller changes aren't worth a message. */
const GYRO_EPSILON = 0.005;
const ACCEL_EPSILON = 0.05;
/**
 * How often a pad's state is sent again when it goes by datagram (they can be lost). 50 ms is
 * three frames at 60 fps: a lost release or press is corrected within that, while the cost is
 * about 20 small datagrams a second per held pad. With 5% loss, a release sent once and four
 * times more is lost with probability 0.05^4 ≈ 6e-6. Shorter would mostly send what the
 * first copy already carried; longer would leave a ghost press visible in games.
 */
export const RESEND_MS = 50;
/** A pad at rest (nothing held) is sent this many times more after its last change, then left alone. */
export const REST_RESENDS = 3;
/** The streamer's hello says what it can carry raw; a page that has heard nothing this long assumes nothing. */
const OFFER_WAIT_MS = 3000;
/** A stick this far from centre counts as held, for resending; below it is drift. */
const HELD_AXIS = 0.15;

export interface ManagedController {
  /** Stable while the pad stays connected. */
  id: string;
  info: ControllerInfo;
  /** The pad's number in the environment, or null when all four are taken. */
  slot: number | null;
}

type Send = (msg: Record<string, unknown>) => void;

/**
 * Passing a controller through raw (hid-relay.ts): on, the manager opens WebHID pads without
 * claiming them, and for one the streamer says it can carry (`rawHidOffered`) sends the pad's
 * reports as they come instead of parsed state, and does what the app asks of it. Anything else
 * (no offer, a refusal, no control, no focus) leaves the pad to its driver and the parsed path.
 */
export interface RelayOptions {
  /** A raw report as an unreliable datagram: true if it went that way, false to send it on the control channel. */
  sendReport: (slot: number, q: number, report: Uint8Array) => boolean;
  /** Whether this page holds the controls now (the streamer takes a relay from no one else). */
  hasControl: () => boolean;
}

export interface ManagerOptions {
  /** Where pad messages go. Without it the manager only watches (the Controllers page). */
  send?: Send;
  /**
   * An unreliable, latest-wins path for pad state (datagrams), when the player has one now:
   * returns true if it sent the message that way, false to use `send`. The manager then sends
   * a held pad's state again every `RESEND_MS` (the datagram may have been lost). `gone`, and
   * anything the path refused, go by `send`.
   */
  sendUnreliable?: (msg: Record<string, unknown>) => boolean;
  /** The clock, in ms. Default `performance.now()`. */
  now?: () => number;
  /** Defaults: the Gamepad API and WebHID. */
  backends?: ControllerBackend[];
  /** Whether the page has focus; unfocused pads are released. Default: `document.hasFocus()`. */
  focused?: () => boolean;
  /** Pass Steam Controllers through raw where the streamer can carry them. Off (the default): the parsed path only. */
  relay?: RelayOptions;
}

/** The message for one pad's state (`gone` pads send just that). */
export function padMessage(slot: number, info: ControllerInfo, s: ControllerState, q?: number): Record<string, unknown> {
  const msg: Record<string, unknown> = { k: "pad", i: slot, b: s.buttons.map(round3), a: s.axes.map(round3) };
  // The pad's sequence number: the streamer applies a message only if it is newer than the newest it has.
  if (q !== undefined) msg.q = q;
  // Not `t`: that is the control channel's own message type ("input").
  msg.ty = info.type;
  if (s.gyro) msg.gyro = s.gyro.map((v) => Math.round(v * 1000) / 1000);
  if (s.accel) msg.accel = s.accel.map((v) => Math.round(v * 1000) / 1000);
  // Only fingers on a pad are there; a lifted one is gone.
  if (s.touch) msg.touch = s.touch.filter((p) => p.down).map((p) => ({ id: p.id, x: round3(p.x), y: round3(p.y), down: p.down }));
  if (s.battery !== undefined) msg.bat = Math.round(s.battery * 100) / 100;
  return msg;
}

export const goneMessage = (slot: number, q?: number): Record<string, unknown> =>
  q === undefined ? { k: "pad", i: slot, gone: true } : { k: "pad", i: slot, gone: true, q };

/** Whether anything on the pad is held, as far as resending goes. */
export function isHeld(s: ControllerState): boolean {
  return (
    s.buttons.some((v) => v > 0) ||
    s.axes.some((v) => Math.abs(v) > HELD_AXIS) ||
    !!s.touch?.some((p) => p.down)
  );
}

/** A pad's last state sent by datagram, and how it is sent again. */
interface Resend {
  msg: Record<string, unknown>;
  at: number;
  /** Held pads are sent until something changes; others `left` more times. */
  held: boolean;
  left: number;
}

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
  private readonly sendUnreliable: ManagerOptions["sendUnreliable"];
  private readonly now: () => number;
  /** Each slot's last sequence number; never reset while the manager lives, so a pad that comes back is newer. */
  private readonly seq = new Map<number, number>();
  private readonly resends = new Map<number, Resend>();
  private readonly focused: () => boolean;
  /** Every controller of every backend, by key. */
  private readonly all = new Map<string, { backend: ControllerBackend; pad: BackendController }>();
  private readonly slots = new Map<string, number>();
  private visible: ManagedController[] = [];
  private readonly relayOptions: RelayOptions | undefined;
  /** The controllers (`vid:pid`) the streamer says it can carry raw; null until its hello. */
  private offered: string[] | null = null;
  /** Controllers the streamer refused to carry raw (by key), until its offer changes. */
  private readonly refused = new Set<string>();
  /** The slots passing a controller through. */
  private readonly relays = new Map<number, { relay: HidRelay; pad: BackendController }>();
  private readonly sent = new Map<number, Sent>();
  private readonly listeners = new Set<(list: ManagedController[]) => void>();
  private timer: ReturnType<typeof setInterval> | undefined;
  private running = false;
  private wasFocused = true;
  private startedAt = 0;

  constructor(options: ManagerOptions = {}) {
    this.relayOptions = options.relay;
    this.backends = options.backends ?? [new GamepadApiBackend(), new WebHidBackend({ deferClaim: !!options.relay })];
    this.send = options.send;
    this.sendUnreliable = options.sendUnreliable;
    this.now = options.now ?? (() => performance.now());
    this.focused = options.focused ?? (() => typeof document === "undefined" || document.hasFocus());
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.startedAt = this.now();
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
        changed: (pad) => this.changed(pad),
      });
    }
    // The poll is for what has no events (the Gamepad API); it also sees to the rest, finding no change.
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

  /** One pad, for what the page does to it itself (the Controllers page's tests). */
  pad(id: string): BackendController | null {
    return this.all.get(id)?.pad ?? null;
  }

  private padInSlot(slot: number): BackendController | null {
    const mine = this.visible.find((c) => c.slot === slot);
    return mine ? this.pad(mine.id) : null;
  }

  /** The streamer's trackpad pulse for a slot: played on a pad that has trackpads, else as a short rumble. */
  haptic(slot: number, side: Side, amp: number, onUs: number, offUs: number, count: number): void {
    const pad = this.padInSlot(slot);
    if (!pad) return;
    const a = clampUnit(amp);
    const on = Math.max(0, num(onUs));
    const off = Math.max(0, num(offUs));
    const n = Math.max(1, Math.round(num(count)));
    if (pad.haptic) pad.haptic(side, a, on, off, n);
    else if (a > 0 && pad.info.capabilities.rumble) pad.rumble(...hapticAsRumble(a, on, off, n));
  }

  /** The streamer's lightbar colour for a slot, 0..255. */
  led(slot: number, r: number, g: number, b: number): void {
    this.padInSlot(slot)?.led?.(byte(r), byte(g), byte(b));
  }

  /** The streamer's player LEDs for a slot, bits 0–4. */
  players(slot: number, mask: number): void {
    this.padInSlot(slot)?.players?.(byte(mask) & 0x1f);
  }

  /** The streamer's adaptive trigger effect for a slot: 11 bytes. */
  trigger(slot: number, side: Side, effect: number[]): void {
    if (effect.length !== 11) return;
    this.padInSlot(slot)?.trigger?.(side, effect.map(byte));
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
    this.resends.clear();
    this.syncRelays();
  }

  /** The streamer's hello: the controllers (`vid:pid`) it can carry raw. */
  rawHidOffered(list: string[]): void {
    this.offered = list;
    this.refused.clear();
    this.syncRelays();
  }

  /** This page lost the controls: the relays end (the streamer ends them too) and the drivers have their pads back. */
  floorLost(): void {
    for (const slot of [...this.relays.keys()]) this.stopRelay(slot, true);
  }

  /** The streamer's answer to a relay's `open`. */
  hidOpened(slot: number, ok: boolean, reason?: string): void {
    const mine = this.relays.get(slot);
    if (!mine) return;
    if (mine.relay.accepted(ok)) {
      // The app in the environment turns lizard mode off from here on.
      mine.pad.rawHid?.hooks.yield();
      return;
    }
    console.info(`The streamer won't carry ${mine.pad.info.name} raw (${reason ?? "no reason given"}); sending it parsed.`);
    this.refused.add(mine.pad.key);
    this.stopRelay(slot, true);
  }

  /** What the app in the environment asks of a relayed controller (`ServerMsg::Hid`). */
  hidRequest(slot: number, req: HidRequest): void {
    const mine = this.relays.get(slot);
    if (mine) mine.relay.request(req);
    // Not relayed here: say so, so the streamer need not wait.
    else if (req.id) this.sendNow({ k: "hid", op: "reply", i: slot, id: req.id, ok: false });
  }

  /** Whether slot `slot` passes its controller through (so its parsed state is not sent). */
  relaying(slot: number | null): boolean {
    return slot !== null && this.relays.has(slot);
  }

  /** Brings the relays in line with the pads, the offer, the controls and the focus. */
  private syncRelays(): void {
    const opts = this.relayOptions;
    if (!opts || !this.send || !this.running) return;
    // A controller that left, or moved to another slot, takes its relay with it.
    for (const [slot, mine] of [...this.relays]) {
      const here = this.visible.find((c) => c.slot === slot);
      if (!here || this.all.get(here.id)?.pad !== mine.pad) this.stopRelay(slot, false);
    }
    const ready = this.focused() && opts.hasControl();
    for (const c of this.visible) {
      const pad = this.all.get(c.id)?.pad;
      const hid = pad?.rawHid;
      if (!pad || !hid) continue;
      const wanted = c.slot !== null && ready && this.offered?.includes(vidPid(hid.device)) === true && !this.refused.has(pad.key);
      if (c.slot !== null) {
        const have = this.relays.has(c.slot);
        if (wanted && !have) this.startRelay(c.slot, pad);
        else if (!wanted && have) this.stopRelay(c.slot, true);
      }
      // Not passed through, and the offer is known (so no relay is about to take it): its driver has it.
      if (!this.relaying(c.slot) && this.offered !== null && hid.hooks.waiting()) {
        void hid.hooks.claim().catch((err) => console.warn("Couldn't take over the controller:", err));
      }
    }
  }

  private startRelay(slot: number, pad: BackendController): void {
    const hid = pad.rawHid!;
    const opts = this.relayOptions!;
    const relay = new HidRelay(
      slot,
      hid.device,
      { send: (m) => this.sendNow(m), sendReport: opts.sendReport },
      () => this.nextSeq(slot),
      this.now,
      STEAM_LIZARD_REPORTS,
    );
    this.relays.set(slot, { relay, pad });
    // Parsed state stops here: the reports are the app's.
    this.resends.delete(slot);
    hid.setSink((id, data) => relay.report(id, data));
    relay.open();
  }

  /** Ends a slot's relay. `claim`: the controller's driver takes it back (lizard mode off again), if it is still there. */
  private stopRelay(slot: number, claim: boolean): void {
    const mine = this.relays.get(slot);
    if (!mine) return;
    this.relays.delete(slot);
    mine.pad.rawHid?.setSink(null);
    mine.relay.close();
    // Whatever was sent before is stale: the next change sends the state again.
    this.sent.delete(slot);
    this.resends.delete(slot);
    if (claim) void mine.pad.rawHid?.hooks.claim().catch((err) => console.warn("Couldn't take over the controller:", err));
  }

  /** Slots and duplicates, after the set of pads changed. */
  private refresh(): void {
    // A pad both backends see: the driver's version is the one that counts.
    // The Gamepad API's copy is matched by its ids, or by its name when it
    // has none: Chrome on macOS reads some pads through the GameController
    // framework, whose ids say only "Steam Controller".
    const idKey = (i: ControllerInfo) => `${i.vendorId}:${i.productId}`;
    const nameKey = (i: ControllerInfo) => i.name.trim().toLowerCase();
    const pads = [...this.all.values()];
    const unmatched = pads.filter(({ pad }) => pad.info.backend === "webhid" && pad.info.vendorId !== undefined);
    const shown: BackendController[] = [];
    for (const { pad } of pads) {
      if (pad.info.backend === "gamepad") {
        const same =
          pad.info.vendorId !== undefined
            ? (h: BackendController) => idKey(h.info) === idKey(pad.info)
            : (h: BackendController) => nameKey(h.info) === nameKey(pad.info);
        // A Steam Controller comes back through the Gamepad API under ids and a name that don't
        // always match the HID device's (macOS shows it once it is in lizard mode, as after a
        // replug): two Steam Controllers on one machine is rare, a second slot and virtual
        // controller for one is worse.
        const steamTwin = (h: BackendController) => pad.info.type === "steam" && h.info.type === "steam";
        let at = unmatched.findIndex(({ pad: h }) => same(h));
        if (at < 0) at = unmatched.findIndex(({ pad: h }) => steamTwin(h));
        if (at >= 0) {
          unmatched.splice(at, 1);
          continue;
        }
      }
      shown.push(pad);
    }
    const keys = new Set(shown.map((p) => p.key));
    for (const [key, slot] of this.slots) {
      if (keys.has(key)) continue;
      this.slots.delete(key);
      this.sendGone(slot);
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
    this.syncRelays();
    for (const l of this.listeners) l(this.visible);
  }

  private sendNow(msg: Record<string, unknown>): void {
    this.send?.(msg);
  }

  private nextSeq(slot: number): number {
    const q = ((this.seq.get(slot) ?? 0) + 1) >>> 0;
    this.seq.set(slot, q);
    return q;
  }

  /** `gone` always goes by the reliable channel; its sequence number puts it after any state still in flight. */
  private sendGone(slot: number): void {
    this.resends.delete(slot);
    this.sendNow(goneMessage(slot, this.nextSeq(slot)));
  }

  /** A pad's new state: by datagram when the player has them (and it is sent again until it is surely there), else reliably. */
  private sendState(slot: number, msg: Record<string, unknown>, held: boolean): void {
    if (this.sendUnreliable?.(msg)) {
      this.resends.set(slot, { msg, at: this.now(), held, left: REST_RESENDS });
      return;
    }
    this.resends.delete(slot);
    this.sendNow(msg);
  }

  /** Sends the states that were sent by datagram again, for those that are due. */
  private resend(): void {
    if (this.resends.size === 0) return;
    const now = this.now();
    for (const [slot, r] of this.resends) {
      if (now - r.at < RESEND_MS) continue;
      // The datagram path went away (a reconnect, a fallback): the reliable one carries it.
      if (!this.sendUnreliable?.(r.msg)) {
        this.resends.delete(slot);
        this.sendNow(r.msg);
        continue;
      }
      r.at = now;
      if (!r.held && --r.left <= 0) this.resends.delete(slot);
    }
  }

  /** Lets go of everything held remotely. */
  private releaseAll(): void {
    const relayed = [...this.relays.keys()];
    for (const slot of relayed) this.stopRelay(slot, this.running);
    // A relayed slot's device goes back to rest on the streamer too.
    for (const slot of new Set([...this.sent.keys(), ...relayed])) this.sendGone(slot);
    this.sent.clear();
    this.resends.clear();
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
    // Back in front: the relays that ended with the focus start again.
    if (!this.wasFocused) this.syncRelays();
    this.wasFocused = true;
    if (this.relayOptions && this.offered === null && this.now() - this.startedAt > OFFER_WAIT_MS) this.rawHidOffered([]);
    for (const c of this.visible) this.publish(c);
    this.resend();
    for (const { relay } of this.relays.values()) relay.tick();
  }

  /**
   * Sends a pad's state if it changed since the last one sent. Called by the poll for every pad,
   * and at once by a backend that has events (`changed`); whichever sees a change first sends it
   * and the other finds nothing new, so nothing goes twice.
   */
  private publish(c: ManagedController): void {
    if (c.slot === null || !this.send || this.relays.has(c.slot) || this.holdForOffer(c)) return;
    const s = this.state(c.id);
    const key = `${s.buttons.map(round3).join(",")}|${s.axes.map(round3).join(",")}|${s.touch?.map((p) => `${round3(p.x)},${round3(p.y)},${+p.down}`).join(";") ?? ""}|${s.battery === undefined ? "" : Math.round(s.battery * 100)}`;
    const last = this.sent.get(c.slot);
    const motion = !!last && near(s.gyro, last.gyro, GYRO_EPSILON) && near(s.accel, last.accel, ACCEL_EPSILON);
    const sensors = s.gyro === undefined && s.accel === undefined;
    if (last && last.key === key && (sensors || motion)) return;
    this.sent.set(c.slot, { key, gyro: s.gyro && [...s.gyro], accel: s.accel && [...s.accel] });
    this.sendState(c.slot, padMessage(c.slot, c.info, s, this.nextSeq(c.slot)), isHeld(s));
  }

  /** A pad opened for a relay says nothing (it is unclaimed, so its state is empty) until the streamer's offer is known. */
  private holdForOffer(c: ManagedController): boolean {
    return !!this.relayOptions && this.offered === null && !!this.all.get(c.id)?.pad.rawHid?.hooks.waiting();
  }

  /** A backend with events saw pad `pad` change: send it now rather than at the next poll. */
  private changed(pad: BackendController): void {
    if (!this.running || !this.send || !this.focused()) return;
    const c = this.visible.find((v) => v.id === pad.key);
    if (c) this.publish(c);
  }
}

const vidPid = (d: { vendorId: number; productId: number }) =>
  `${d.vendorId.toString(16).padStart(4, "0")}:${d.productId.toString(16).padStart(4, "0")}`;
const num = (v: number) => (Number.isFinite(v) ? v : 0);
const byte = (v: number) => Math.round(Math.min(255, Math.max(0, num(v))));
const clampUnit = (v: number) => (Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0);
