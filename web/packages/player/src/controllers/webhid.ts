// WebHID: pads the Gamepad API can't see (macOS keeps some Bluetooth ones
// from the browser; Steam Controllers sit in a vendor collection), and ones it
// sees with the wrong layout. Chrome and Edge only, on a secure page. The
// user has to grant each device once, in a picker a click opens; granted
// devices come back on their own.

import { genericHidFactory } from "./hid-generic";
import {
  hidApi,
  hidUnavailableReason,
  type HidApi,
  type HidDevice,
  type HidDeviceFilter,
  type HidDriver,
  type HidDriverFactory,
  type HidInputReportEvent,
} from "./hid-types";
import { DUALSENSE, DUALSENSE_EDGE, SONY_VENDOR_ID, dualSenseFactory } from "./dualsense";
import { steamControllerFactory } from "./steam-controller";
import { steamTritonFactory } from "./steam-triton";
import { VALVE_VENDOR_ID, sleep } from "./steam-protocol";
import type { BackendController, BackendListener, ControllerBackend, ControllerInfo, ControllerState, RawReport } from "./types";

/** The drivers, most specific first. */
const FACTORIES: HidDriverFactory[] = [steamTritonFactory, steamControllerFactory, dualSenseFactory, genericHidFactory];

/** The picker's filters: joysticks and gamepads, Valve's own (its controllers' interfaces aren't either) and Sony's DualSense and DualSense Edge. */
const PICKER_FILTERS: HidDeviceFilter[] = [
  { usagePage: 0x01, usage: 0x04 },
  { usagePage: 0x01, usage: 0x05 },
  { vendorId: VALVE_VENDOR_ID },
  { vendorId: SONY_VENDOR_ID, productId: DUALSENSE },
  { vendorId: SONY_VENDOR_ID, productId: DUALSENSE_EDGE },
];

/** A device and its driver, as a controller of the backend. */
class HidController implements BackendController {
  private lastReport: { reportId: number; bytes: Uint8Array } | null = null;
  readonly info: ControllerInfo;
  readonly rumble: (lo: number, hi: number, ms: number) => void;
  readonly haptic?: BackendController["haptic"];
  readonly led?: BackendController["led"];
  readonly players?: BackendController["players"];
  readonly trigger?: BackendController["trigger"];

  constructor(
    readonly key: string,
    readonly device: HidDevice,
    readonly driver: HidDriver,
  ) {
    this.info = driver.info;
    this.rumble = (lo, hi, ms) => driver.rumble?.(lo, hi, ms);
    // Only what the driver can do: the manager looks at what is there.
    if (driver.haptic) this.haptic = (...a) => driver.haptic!(...a);
    if (driver.led) this.led = (...a) => driver.led!(...a);
    if (driver.players) this.players = (...a) => driver.players!(...a);
    if (driver.trigger) this.trigger = (...a) => driver.trigger!(...a);
  }

  report(reportId: number, data: DataView): void {
    this.lastReport = { reportId, bytes: new Uint8Array(data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength)) };
    this.driver.onReport(reportId, data);
  }

  state(): ControllerState {
    return this.driver.state();
  }

  raw(): RawReport | null {
    return this.lastReport ? { kind: "hid", ...this.lastReport } : null;
  }
}

export class WebHidBackend implements ControllerBackend {
  readonly name = "webhid" as const;
  readonly unavailable = hidUnavailableReason();
  /** What went wrong opening the last device, for the user. */
  lastError: string | null = null;
  private listener: BackendListener | null = null;
  private readonly hid: HidApi | null = this.unavailable ? null : hidApi();
  private readonly pads = new Map<HidDevice, HidController>();
  /** Devices being opened, so a second arrival can't open one twice. */
  private readonly opening = new Set<HidDevice>();
  private nextKey = 1;
  private readonly keys = new WeakMap<HidDevice, string>();

  private readonly onConnect = (e: Event) => void this.adopt((e as Event & { device: HidDevice }).device);
  private readonly onDisconnect = (e: Event) => void this.drop((e as Event & { device: HidDevice }).device);

  start(listener: BackendListener): void {
    if (!this.hid) return;
    this.listener = listener;
    this.hid.addEventListener("connect", this.onConnect);
    this.hid.addEventListener("disconnect", this.onDisconnect);
    // Devices the user granted before.
    void this.hid.getDevices().then((devices) => {
      if (this.listener) for (const d of devices) void this.adopt(d);
    });
  }

  stop(): void {
    this.hid?.removeEventListener("connect", this.onConnect);
    this.hid?.removeEventListener("disconnect", this.onDisconnect);
    for (const device of [...this.pads.keys()]) void this.drop(device, true);
    this.listener = null;
  }

  controllers(): BackendController[] {
    return [...this.pads.values()].filter((c) => c.driver.connected);
  }

  /** Opens the browser's device picker (needs a user gesture). Returns how many controllers it added. */
  async connect(): Promise<number> {
    if (!this.hid) throw new Error(this.unavailable ?? "WebHID isn't available.");
    const before = this.pads.size;
    const devices = await this.hid.requestDevice({ filters: PICKER_FILTERS });
    await Promise.all(devices.map((d) => this.adopt(d)));
    return this.pads.size - before;
  }

  private keyOf(device: HidDevice): string {
    let key = this.keys.get(device);
    if (!key) this.keys.set(device, (key = `hid:${this.nextKey++}`));
    return key;
  }

  private async adopt(device: HidDevice): Promise<void> {
    if (!this.listener || this.pads.has(device) || this.opening.has(device)) return;
    const factory = FACTORIES.find((f) => f.matches(device));
    if (!factory) return;
    this.opening.add(device);
    const driver = factory.create(device);
    try {
      if (!device.opened) {
        try {
          await device.open();
        } catch (err) {
          // Closed a moment ago by another page of ours: give it a beat.
          await sleep(300);
          await device.open().catch(() => {
            throw err;
          });
        }
      }
      await driver.open();
    } catch (err) {
      this.lastError = `${device.productName || "The device"}: ${err instanceof Error ? err.message : String(err)}`;
      await device.close().catch(() => {});
      this.opening.delete(device);
      return;
    }
    this.opening.delete(device);
    if (!this.listener) {
      await driver.close();
      await device.close().catch(() => {});
      return;
    }
    const pad = new HidController(this.keyOf(device), device, driver);
    this.pads.set(device, pad);
    this.lastError = null;
    device.addEventListener("inputreport", (e) => {
      const ev = e as HidInputReportEvent;
      if (this.pads.get(device) === pad) pad.report(ev.reportId, ev.data);
    });
    // A dongle's controller comes and goes behind it.
    let announced = false;
    const announce = () => {
      const now = driver.connected;
      if (now && !announced) this.listener?.arrived(pad);
      else if (!now && announced) this.listener?.removed(pad);
      announced = now;
    };
    driver.onConnection = announce;
    announce();
  }

  private async drop(device: HidDevice, closing = false): Promise<void> {
    const pad = this.pads.get(device);
    if (!pad) return;
    this.pads.delete(device);
    if (pad.driver.connected) this.listener?.removed(pad);
    pad.driver.onConnection = undefined;
    if (closing || device.opened) {
      await pad.driver.close().catch(() => {});
      await device.close().catch(() => {});
    }
  }
}
