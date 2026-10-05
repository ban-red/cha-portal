// A driver for any HID gamepad, built from the report descriptor as WebHID
// exposes it: which bits of which report are axes, buttons and a hat. It maps
// them to the standard layout by usage, with HID_QUIRKS for pads that differ.
//
// Reads Generic Desktop (X, Y, Z, Rx, Ry, Rz, Slider, Hat switch), Button and
// Simulation Controls (accelerator, brake as triggers) fields, and Consumer
// "AC Home" as the guide button.

import { findQuirk, type AxisRole, type HidQuirk } from "./hid-quirks";
import type { HidCollectionInfo, HidDevice, HidDriver, HidDriverFactory, HidReportItem } from "./hid-types";
import {
  BTN,
  clamp,
  neutralState,
  typeFromVendor,
  type ControllerInfo,
  type ControllerState,
} from "./types";

const PAGE_DESKTOP = 0x01;
const PAGE_SIMULATION = 0x02;
const PAGE_BUTTON = 0x09;
const PAGE_CONSUMER = 0x0c;
const USAGE_HAT = 0x39;
const USAGE_AC_HOME = 0x0223;
const USAGE_ACCELERATOR = 0xc4;
const USAGE_BRAKE = 0xc5;

/** One variable field of an input report. */
export interface HidField {
  reportId: number;
  /** Bit offset into the report's data (after the report id byte). */
  bit: number;
  size: number;
  signed: boolean;
  min: number;
  max: number;
  page: number;
  usage: number;
}

/** The full usage out of an item's `usages` (page in the high 16 bits), or this collection's page for a bare one. */
function splitUsage(u: number, page: number): { page: number; usage: number } {
  return u > 0xffff ? { page: u >>> 16, usage: u & 0xffff } : { page, usage: u };
}

function usageOf(item: HidReportItem, k: number, page: number): { page: number; usage: number } | null {
  if (item.isRange && item.usageMinimum !== undefined && item.usageMaximum !== undefined) {
    const min = splitUsage(item.usageMinimum, page);
    const max = splitUsage(item.usageMaximum, page);
    return { page: min.page, usage: Math.min(min.usage + k, max.usage) };
  }
  const usages = item.usages;
  if (!usages?.length) return null;
  return splitUsage(usages[Math.min(k, usages.length - 1)]!, page);
}

/**
 * Every input field of the collections, with its bit position. A report's
 * items run in descriptor order; a collection's own come first, then its
 * children's (WebHID keeps nothing finer than that).
 */
export function parseInputFields(collections: HidCollectionInfo[]): HidField[] {
  const fields: HidField[] = [];
  const offsets = new Map<number, number>();
  const walk = (c: HidCollectionInfo) => {
    for (const report of c.inputReports ?? []) {
      let bit = offsets.get(report.reportId) ?? 0;
      for (const item of report.items ?? []) {
        const size = item.reportSize;
        if (!item.isConstant && !item.isArray) {
          // A maximum below the minimum is an unsigned field written as signed.
          const wrapped = item.logicalMaximum < item.logicalMinimum;
          const min = item.logicalMinimum;
          const max = wrapped ? 2 ** size - 1 : item.logicalMaximum;
          for (let k = 0; k < item.reportCount; k++) {
            const u = usageOf(item, k, c.usagePage);
            if (!u) continue;
            fields.push({
              reportId: report.reportId,
              bit: bit + k * size,
              size,
              signed: min < 0 && !wrapped,
              min,
              max,
              page: u.page,
              usage: u.usage,
            });
          }
        }
        bit += size * item.reportCount;
      }
      offsets.set(report.reportId, bit);
    }
    c.children?.forEach(walk);
  };
  collections.forEach(walk);
  return fields;
}

/** The `size` bits at `bit` of `data`, little-endian, optionally sign-extended. */
export function readBits(data: DataView, bit: number, size: number, signed: boolean): number {
  let v = 0;
  for (let i = 0; i < size; i++) {
    const at = bit + i;
    const byte = at >> 3;
    if (byte >= data.byteLength) return 0;
    if ((data.getUint8(byte) >> (at & 7)) & 1) v += 2 ** i;
  }
  return signed && v >= 2 ** (size - 1) ? v - 2 ** size : v;
}

type Binding =
  | { kind: "button"; field: HidField; index: number }
  | { kind: "stick"; field: HidField; axis: number; invert: boolean }
  | { kind: "trigger"; field: HidField; index: number }
  | { kind: "hat"; field: HidField };

const STICK_AXIS: Partial<Record<AxisRole, number>> = { lx: 0, ly: 1, rx: 2, ry: 3 };
const TRIGGER_BUTTON: Partial<Record<AxisRole, number>> = { lt: BTN.leftTrigger, rt: BTN.rightTrigger };

/** HID button number (1-based) to the standard layout, when the pad has analog triggers on axes. */
const BUTTONS_WITH_ANALOG_TRIGGERS = [0, 1, 2, 3, 4, 5, 8, 9, 10, 11, BTN.guide];
/** … and when its triggers are buttons 7 and 8. */
const BUTTONS_WITH_DIGITAL_TRIGGERS = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, BTN.guide];

/** Axis fields to roles: the quirk's, or by their usages. */
function axisRoles(fields: HidField[], quirk: HidQuirk | undefined): Map<HidField, AxisRole> {
  const roles = new Map<HidField, AxisRole>();
  const desktop = fields.filter((f) => f.page === PAGE_DESKTOP && f.usage >= 0x30 && f.usage <= 0x38);
  const byUsage = (u: number) => desktop.find((f) => f.usage === u);
  if (quirk?.axes) {
    for (const f of desktop) {
      const role = quirk.axes[f.usage];
      if (role) roles.set(f, role);
    }
  } else {
    const set = (u: number, role: AxisRole) => {
      const f = byUsage(u);
      if (f) roles.set(f, role);
    };
    set(0x30, "lx");
    set(0x31, "ly");
    if (byUsage(0x33) && byUsage(0x34)) {
      // X, Y, Rx, Ry sticks, Z and Rz the triggers (an Xbox pad's DirectInput layout).
      set(0x33, "rx");
      set(0x34, "ry");
      set(0x32, "lt");
      set(0x35, "rt");
    } else {
      // X, Y, Z, Rz: the right stick is Z and Rz (a PlayStation pad's, and Xbox over Bluetooth).
      set(0x32, "rx");
      set(0x35, "ry");
    }
  }
  for (const f of fields) {
    if (f.page !== PAGE_SIMULATION) continue;
    const role = f.usage === USAGE_BRAKE ? "lt" : f.usage === USAGE_ACCELERATOR ? "rt" : null;
    if (role && ![...roles.values()].includes(role)) roles.set(f, role);
  }
  return roles;
}

export function buildBindings(fields: HidField[], quirk?: HidQuirk): Binding[] {
  const bindings: Binding[] = [];
  const roles = axisRoles(fields, quirk);
  for (const [field, role] of roles) {
    const stick = STICK_AXIS[role];
    if (stick !== undefined) bindings.push({ kind: "stick", field, axis: stick, invert: !!quirk?.invert?.includes(role) });
    else bindings.push({ kind: "trigger", field, index: TRIGGER_BUTTON[role]! });
  }
  const analogTriggers = bindings.some((b) => b.kind === "trigger");
  const defaults = analogTriggers ? BUTTONS_WITH_ANALOG_TRIGGERS : BUTTONS_WITH_DIGITAL_TRIGGERS;
  for (const field of fields) {
    if (field.page === PAGE_DESKTOP && field.usage === USAGE_HAT) {
      bindings.push({ kind: "hat", field });
    } else if (field.page === PAGE_BUTTON) {
      const index = quirk?.buttons?.[field.usage] ?? defaults[field.usage - 1];
      if (index !== undefined) bindings.push({ kind: "button", field, index });
    } else if (field.page === PAGE_CONSUMER && field.usage === USAGE_AC_HOME) {
      bindings.push({ kind: "button", field, index: BTN.guide });
    }
  }
  return bindings;
}

/** A hat's value as which of up, down, left, right are held (all false for its null state). */
export function hatDirections(value: number, min: number, max: number): [boolean, boolean, boolean, boolean] {
  const count = max - min + 1;
  const step = value - min;
  if (step < 0 || step >= count || (count !== 4 && count !== 8)) return [false, false, false, false];
  // Clockwise from up: N, NE, E, SE, S, SW, W, NW (or N, E, S, W).
  const dir = count === 8 ? step : step * 2;
  return [dir === 7 || dir === 0 || dir === 1, dir >= 3 && dir <= 5, dir >= 5 && dir <= 7, dir >= 1 && dir <= 3];
}

export class GenericHidDriver implements HidDriver {
  readonly connected = true;
  readonly info: ControllerInfo;
  private readonly bindings: Binding[];
  private readonly byReport = new Map<number, Binding[]>();
  private readonly current: ControllerState = neutralState();

  constructor(device: HidDevice) {
    const quirk = findQuirk(device.vendorId, device.productId);
    this.bindings = buildBindings(parseInputFields(device.collections), quirk);
    for (const b of this.bindings) {
      const list = this.byReport.get(b.field.reportId) ?? [];
      list.push(b);
      this.byReport.set(b.field.reportId, list);
    }
    this.info = {
      name: device.productName || quirk?.name || "HID gamepad",
      backend: "webhid",
      type: quirk?.type ?? typeFromVendor(device.vendorId),
      vendorId: device.vendorId,
      productId: device.productId,
      capabilities: { rumble: false, gyro: false, touchpad: false, battery: false },
      mapped: true,
    };
  }

  /** How many fields mapped to something: none means this isn't a gamepad we can read. */
  get mappedFields(): number {
    return this.bindings.length;
  }

  async open(): Promise<void> {}
  async close(): Promise<void> {}

  onReport(reportId: number, data: DataView): void {
    const s = this.current;
    for (const b of this.byReport.get(reportId) ?? []) {
      const f = b.field;
      const v = readBits(data, f.bit, f.size, f.signed);
      const span = f.max - f.min;
      switch (b.kind) {
        case "button":
          s.buttons[b.index] = v !== 0 ? 1 : 0;
          break;
        case "stick": {
          if (span <= 0) break;
          const x = clamp(((v - f.min) / span) * 2 - 1, -1, 1);
          s.axes[b.axis] = b.invert ? -x : x;
          break;
        }
        case "trigger":
          if (span > 0) s.buttons[b.index] = clamp((v - f.min) / span, 0, 1);
          break;
        case "hat": {
          const [up, down, left, right] = hatDirections(v, f.min, f.max);
          s.buttons[BTN.up] = up ? 1 : 0;
          s.buttons[BTN.down] = down ? 1 : 0;
          s.buttons[BTN.left] = left ? 1 : 0;
          s.buttons[BTN.right] = right ? 1 : 0;
          break;
        }
      }
    }
  }

  state(): ControllerState {
    return this.current;
  }
}

/** A device with a joystick or gamepad top-level collection. */
function isGamepad(device: HidDevice): boolean {
  return device.collections.some((c) => c.usagePage === PAGE_DESKTOP && (c.usage === 0x04 || c.usage === 0x05));
}

export const genericHidFactory: HidDriverFactory = {
  name: "generic",
  matches: (device) => isGamepad(device) && new GenericHidDriver(device).mappedFields > 0,
  create: (device) => new GenericHidDriver(device),
};
