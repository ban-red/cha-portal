// Gamepads from the Gamepad API. Pads show up after their first button press
// (the browser's rule), and the browser sees no pad that macOS keeps from it
// (some Bluetooth ones), nor Steam Controllers: WebHID covers those.

import {
  AXIS_COUNT,
  STANDARD_BUTTON_COUNT,
  hapticAsRumble,
  neutralState,
  round3,
  typeFromVendor,
  type BackendController,
  type BackendListener,
  type ControllerBackend,
  type ControllerInfo,
  type ControllerState,
  type ControllerType,
  type RawReport,
  type Side,
} from "./types";

/** The vendor and product id out of a Gamepad `id`: Chrome's "name (Vendor: 045e Product: 028e)", Firefox's "045e-028e-name". */
export function parseGamepadId(id: string): { vendorId: number; productId: number } | null {
  const m =
    /Vendor:\s*([0-9a-f]{4})\s+Product:\s*([0-9a-f]{4})/i.exec(id) ??
    /^([0-9a-f]{4})-([0-9a-f]{4})-/i.exec(id) ??
    /\b([0-9a-f]{4}):([0-9a-f]{4})\b/i.exec(id);
  return m ? { vendorId: parseInt(m[1]!, 16), productId: parseInt(m[2]!, 16) } : null;
}

/** "Xbox 360 pad (STANDARD GAMEPAD Vendor: 045e Product: 028e)" as "Xbox 360 pad". */
export function gamepadName(id: string): string {
  const name = id
    .replace(/^[0-9a-f]{4}-[0-9a-f]{4}-/i, "")
    .replace(/\s*\((?:STANDARD GAMEPAD\s*)?(?:Vendor:\s*[0-9a-f]{4}\s+Product:\s*[0-9a-f]{4})?\)\s*$/i, "")
    .trim();
  return name || "Gamepad";
}

function typeFromName(id: string): ControllerType {
  if (/xbox|x-box|xinput/i.test(id)) return "xbox";
  if (/playstation|dualshock|dualsense|\bps[345]\b/i.test(id)) return "playstation";
  if (/nintendo|switch|joy-?con/i.test(id)) return "switch";
  if (/steam/i.test(id)) return "steam";
  return "generic";
}

class GamepadApiController implements BackendController {
  readonly info: ControllerInfo;
  pad: Gamepad;

  constructor(
    readonly key: string,
    pad: Gamepad,
  ) {
    this.pad = pad;
    const ids = parseGamepadId(pad.id);
    const type = typeFromName(pad.id);
    this.info = {
      name: gamepadName(pad.id),
      backend: "gamepad",
      type: type === "generic" ? typeFromVendor(ids?.vendorId) : type,
      ...ids,
      capabilities: { rumble: !!pad.vibrationActuator, gyro: false, touchpad: false, battery: false },
      mapped: pad.mapping === "standard",
    };
  }

  state(): ControllerState {
    const s = neutralState();
    this.pad.buttons.slice(0, STANDARD_BUTTON_COUNT).forEach((b, i) => (s.buttons[i] = round3(b.value)));
    this.pad.axes.slice(0, AXIS_COUNT).forEach((a, i) => (s.axes[i] = round3(a)));
    return s;
  }

  raw(): RawReport {
    return {
      kind: "gamepad",
      mapping: this.pad.mapping || "none",
      buttons: this.pad.buttons.map((b) => round3(b.value)),
      axes: this.pad.axes.map(round3),
    };
  }

  rumble(lo: number, hi: number, ms: number): void {
    const actuator = this.pad.vibrationActuator;
    if (!actuator) return;
    if (ms <= 0 || (lo <= 0 && hi <= 0)) {
      void actuator.reset?.().catch(() => {});
      return;
    }
    void actuator
      .playEffect("dual-rumble", { startDelay: 0, duration: ms, strongMagnitude: lo, weakMagnitude: hi })
      .catch(() => {});
  }

  /** There are no trackpads to pulse: a short rumble instead. The rest (lightbar, triggers) the Gamepad API can't do. */
  haptic(_side: Side, amp: number, onUs: number, offUs: number, count: number): void {
    if (amp <= 0) return;
    this.rumble(...hapticAsRumble(amp, onUs, offUs, count));
  }
}

export class GamepadApiBackend implements ControllerBackend {
  readonly name = "gamepad" as const;
  readonly unavailable = typeof navigator !== "undefined" && "getGamepads" in navigator ? null : "This browser has no Gamepad API.";
  private readonly known = new Map<number, GamepadApiController>();
  private listener: BackendListener | null = null;

  start(listener: BackendListener): void {
    this.listener = listener;
  }

  stop(): void {
    for (const c of this.known.values()) this.listener?.removed(c);
    this.known.clear();
    this.listener = null;
  }

  controllers(): BackendController[] {
    return [...this.known.values()];
  }

  poll(): void {
    if (!this.listener || this.unavailable) return;
    const seen = new Set<number>();
    for (const pad of navigator.getGamepads()) {
      if (!pad?.connected) continue;
      seen.add(pad.index);
      let c = this.known.get(pad.index);
      // Another pad in a recycled index.
      if (c && c.pad.id !== pad.id) {
        this.known.delete(pad.index);
        this.listener.removed(c);
        c = undefined;
      }
      if (c) {
        c.pad = pad;
      } else {
        c = new GamepadApiController(`gamepad:${pad.index}:${pad.id}`, pad);
        this.known.set(pad.index, c);
        this.listener.arrived(c);
      }
    }
    for (const [index, c] of this.known) {
      if (seen.has(index)) continue;
      this.known.delete(index);
      this.listener.removed(c);
    }
  }
}
