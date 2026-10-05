// Per-device overrides for the generic HID gamepad driver. The defaults in
// hid-generic.ts suit the Xbox-style layouts (a Bluetooth Xbox pad's, which is
// what 8BitDo's Mac mode imitates) and common DirectInput ones. A pad that
// differs gets an entry here, worked out from the portal's Controllers page
// (its raw view shows the report bytes).
//
// Nothing below has been checked against a real pad yet: the 8BitDo entry
// only names the vendor.

import type { ControllerType } from "./types";

export type AxisRole = "lx" | "ly" | "rx" | "ry" | "lt" | "rt";

export interface HidQuirk {
  /** Matches the vendor, and the product too when given. */
  vendorId: number;
  productId?: number;
  /** Shown when the device has no product name. */
  name?: string;
  type?: ControllerType;
  /** HID button number (1-based, the Button page) to standard-layout index. */
  buttons?: Record<number, number>;
  /** Generic Desktop usage id (0x30 X, 0x31 Y, 0x32 Z, 0x33 Rx, 0x34 Ry, 0x35 Rz, 0x36 Slider) to what it is. */
  axes?: Record<number, AxisRole>;
  /** Axes whose direction is reversed. */
  invert?: AxisRole[];
}

export const HID_QUIRKS: HidQuirk[] = [
  // 8BitDo: its pads' layout depends on the mode switch (X, D, Mac, Switch);
  // fill in per product once a pad's raw view says what each mode sends.
  { vendorId: 0x2dc8, name: "8BitDo controller" },
];

export function findQuirk(vendorId: number, productId: number): HidQuirk | undefined {
  return (
    HID_QUIRKS.find((q) => q.vendorId === vendorId && q.productId === productId) ??
    HID_QUIRKS.find((q) => q.vendorId === vendorId && q.productId === undefined)
  );
}
