// The parts of WebHID the drivers use (TypeScript's DOM library has none).

import type { ControllerInfo, ControllerState } from "./types";

export interface HidReportItem {
  isConstant: boolean;
  isArray: boolean;
  isRange: boolean;
  hasNull: boolean;
  /** Usages with the usage page in the high 16 bits (or the bare usage). */
  usages?: number[];
  usageMinimum?: number;
  usageMaximum?: number;
  reportSize: number;
  reportCount: number;
  logicalMinimum: number;
  logicalMaximum: number;
}

export interface HidReportInfo {
  reportId: number;
  items?: HidReportItem[];
}

export interface HidCollectionInfo {
  usagePage: number;
  usage: number;
  children?: HidCollectionInfo[];
  inputReports?: HidReportInfo[];
  outputReports?: HidReportInfo[];
  featureReports?: HidReportInfo[];
}

export interface HidInputReportEvent extends Event {
  readonly device: HidDevice;
  readonly reportId: number;
  readonly data: DataView;
}

export interface HidDevice extends EventTarget {
  readonly opened: boolean;
  readonly vendorId: number;
  readonly productId: number;
  readonly productName: string;
  readonly collections: HidCollectionInfo[];
  open(): Promise<void>;
  close(): Promise<void>;
  sendReport(reportId: number, data: BufferSource): Promise<void>;
  sendFeatureReport(reportId: number, data: BufferSource): Promise<void>;
}

export interface HidDeviceFilter {
  vendorId?: number;
  productId?: number;
  usagePage?: number;
  usage?: number;
}

export interface HidApi extends EventTarget {
  getDevices(): Promise<HidDevice[]>;
  requestDevice(options: { filters: HidDeviceFilter[] }): Promise<HidDevice[]>;
}

export function hidApi(): HidApi | null {
  return typeof navigator !== "undefined" && "hid" in navigator ? ((navigator as unknown as { hid: HidApi }).hid ?? null) : null;
}

/** Why WebHID can't be used here, or null. */
export function hidUnavailableReason(): string | null {
  // First: Chrome hides navigator.hid on insecure pages altogether.
  if (typeof isSecureContext !== "undefined" && !isSecureContext) {
    return "WebHID needs a secure page: open the portal over HTTPS, or at http://localhost.";
  }
  if (!hidApi()) return "This browser has no WebHID. Use Chrome or Edge.";
  return null;
}

/** A driver for one opened HID device. */
export interface HidDriver {
  /** Dongles: false until a controller is paired and awake behind them. */
  readonly connected: boolean;
  readonly info: ControllerInfo;
  /** Takes the device over (throws with a message to show the user). */
  open(): Promise<void>;
  /** Gives it back (a Steam Controller returns to lizard mode). */
  close(): Promise<void>;
  onReport(reportId: number, data: DataView): void;
  state(): ControllerState;
  rumble?(lo: number, hi: number, ms: number): void;
  /** Tells the backend the pad (dis)connected behind a dongle. */
  onConnection?: () => void;
}

export interface HidDriverFactory {
  name: string;
  matches(device: HidDevice): boolean;
  create(device: HidDevice): HidDriver;
}
