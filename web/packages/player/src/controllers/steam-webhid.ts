// A Valve controller that only the Gamepad API shows. There the browser (on a Mac, the system's
// GameController framework) hides the gyro and trackpads and reports the Steam button as A; WebHID
// reads the controller itself, so the page offers it. The manager's twin rule (manager.ts
// `refresh()`) already drops the Gamepad API copy of a controller once WebHID has one, so a
// Valve controller left in its list is one no WebHID pad has taken.

import { VALVE_VENDOR_ID } from "./steam-protocol";
import type { ControllerInfo } from "./types";

/** Whether `info` is a Valve controller read through the Gamepad API alone. */
export function isSteamOnGamepadApi(info: ControllerInfo): boolean {
  if (info.backend !== "gamepad") return false;
  return info.type === "steam" || info.vendorId === VALVE_VENDOR_ID || /\bvalve\b|steam controller/i.test(info.name);
}

/** The controllers (the manager's visible list) that need a WebHID grant to work fully. */
export function steamNeedingWebHid<T extends { info: ControllerInfo }>(list: readonly T[]): T[] {
  return list.filter((c) => isSteamOnGamepadApi(c.info));
}
