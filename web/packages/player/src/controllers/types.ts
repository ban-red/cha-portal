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
export const BUTTON_COUNT = 17;
/** LX, LY, RX, RY. */
export const AXIS_COUNT = 4;

export interface TouchPoint {
  /** Which touchpad: 0 left, 1 right. */
  id: number;
  /** 0..1 across the pad, from its top left. */
  x: number;
  y: number;
  down: boolean;
}

export interface ControllerState {
  /** 17 values 0..1 (triggers are 6 and 7, analog). */
  buttons: number[];
  /** LX, LY, RX, RY in -1..1, down and right positive. */
  axes: number[];
  /** rad/s, right-handed: x pitch, y yaw, z roll (SDL's convention). */
  gyro?: [number, number, number];
  /** m/s². */
  accel?: [number, number, number];
  touch?: TouchPoint[];
  /** 0..1. */
  battery?: number;
}

export interface Capabilities {
  rumble: boolean;
  gyro: boolean;
  touchpad: boolean;
  battery: boolean;
}

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
