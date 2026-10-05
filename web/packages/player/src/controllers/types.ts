// What every controller backend hands the manager: the canonical state (the
// standard-mapping layout, whatever the physical pad is) and a small interface
// to read, rumble and watch them come and go.

export type ControllerType = "xbox" | "playstation" | "switch" | "steam" | "generic";
export type BackendName = "gamepad" | "webhid";

/** Indices into `ControllerState.buttons`: the Gamepad API's standard mapping. */
export const BTN = {
  south: 0,
  east: 1,
  west: 2,
  north: 3,
  leftShoulder: 4,
  rightShoulder: 5,
  leftTrigger: 6,
  rightTrigger: 7,
  back: 8,
  start: 9,
  leftStick: 10,
  rightStick: 11,
  up: 12,
  down: 13,
  left: 14,
  right: 15,
  guide: 16,
} as const;
/** The Gamepad API's standard 17. */
export const STANDARD_BUTTON_COUNT = 17;
/** What pads have beyond the standard layout (docs/controllers.md); the ones a pad lacks stay released. */
export const EXTRA = {
  /** A DualSense's touchpad click, or a Steam Controller's right trackpad click. */
  touchpadClick: 17,
  /** A Steam Controller's left trackpad click. */
  leftPadClick: 18,
  /** Back paddles or grips: left and right (L4, R4), then the second pair (L5, R5). */
  l4: 19,
  r4: 20,
  l5: 21,
  r5: 22,
  /** A DualSense's mute button, or a Steam Controller's quick-access one. */
  mute: 23,
} as const;
export const BUTTON_COUNT = 24;
/** LX, LY, RX, RY. */
export const AXIS_COUNT = 4;

export interface TouchPoint {
  /** A Steam Controller's left trackpad is 0 and its right one 1; a DualSense's two finger slots are 0 and 1. */
  id: number;
  /** 0..1 across the pad, from its top left. */
  x: number;
  y: number;
  down: boolean;
}

export interface ControllerState {
  /** 24 values 0..1: the standard 17 (triggers are 6 and 7, analog), then `EXTRA`'s. */
  buttons: number[];
  /** LX, LY, RX, RY in -1..1, down and right positive. */
  axes: number[];
  /** rad/s, right-handed: x pitch, y yaw, z roll (SDL's convention). */
  gyro?: [number, number, number];
  /** m/s². */
  accel?: [number, number, number];
  /** The fingers on a pad or pads: only those touching. */
  touch?: TouchPoint[];
  /** 0..1. */
  battery?: number;
}

export interface Capabilities {
  rumble: boolean;
  gyro: boolean;
  touchpad: boolean;
  battery: boolean;
  /** A lightbar to colour (`led`). */
  lightbar?: boolean;
  /** Adaptive triggers (`trigger`). */
  triggers?: boolean;
}

export type Side = "left" | "right";

export interface ControllerInfo {
  name: string;
  backend: BackendName;
  type: ControllerType;
  vendorId?: number;
  productId?: number;
  capabilities: Capabilities;
  /** False when the buttons aren't in the standard layout (a Gamepad API pad with no standard mapping): which is which is a guess. */
  mapped: boolean;
}

/** What the pad itself reports, as it came, for working out an unknown one. */
export type RawReport =
  | { kind: "gamepad"; mapping: string; buttons: number[]; axes: number[] }
  | { kind: "hid"; reportId: number; bytes: Uint8Array };

export interface BackendController {
  /** Stable while the pad stays connected. */
  readonly key: string;
  readonly info: ControllerInfo;
  state(): ControllerState;
  raw(): RawReport | null;
  /** lo: strong (low-frequency) motor, hi: weak (high-frequency), both 0..1; `ms` 0 stops. */
  rumble(lo: number, hi: number, ms: number): void;
  /** A trackpad pulse train (Steam Controller haptics). Without it the manager plays a short rumble. */
  haptic?(side: Side, amp: number, onUs: number, offUs: number, count: number): void;
  /** The lightbar, 0..255. */
  led?(r: number, g: number, b: number): void;
  /** The player LEDs, bits 0–4. */
  players?(mask: number): void;
  /** An adaptive trigger effect: the 11 bytes of the DualSense output report's block. */
  trigger?(side: Side, effect: number[]): void;
}

export interface BackendListener {
  arrived(controller: BackendController): void;
  removed(controller: BackendController): void;
}

export interface ControllerBackend {
  readonly name: BackendName;
  /** Why this browser can't use the backend, or null. */
  readonly unavailable: string | null;
  start(listener: BackendListener): void;
  stop(): void;
  controllers(): BackendController[];
  /** Called every tick by the manager, for backends that have to look. */
  poll?(): void;
}

export function neutralState(): ControllerState {
  return { buttons: new Array<number>(BUTTON_COUNT).fill(0), axes: new Array<number>(AXIS_COUNT).fill(0) };
}

export function typeFromVendor(vendorId: number | undefined): ControllerType {
  switch (vendorId) {
    case 0x045e:
      return "xbox";
    case 0x054c:
      return "playstation";
    case 0x057e:
      return "switch";
    case 0x28de:
      return "steam";
    default:
      return "generic";
  }
}

export const clamp = (v: number, lo: number, hi: number) => (v < lo ? lo : v > hi ? hi : v);
export const round3 = (v: number) => Math.round(v * 1000) / 1000;

/** A trackpad pulse train as a short rumble, for pads without trackpads: a tick is the weak (high-frequency) motor, with a little of the strong one. */
export function hapticAsRumble(amp: number, onUs: number, offUs: number, count: number): [number, number, number] {
  const a = clamp(Number.isFinite(amp) ? amp : 0, 0, 1);
  const ms = clamp(((onUs + offUs) * Math.max(1, count)) / 1000, 20, 500);
  return [a * 0.4, a, Math.round(ms)];
}
