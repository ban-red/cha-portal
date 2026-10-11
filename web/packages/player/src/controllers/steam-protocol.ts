// Valve's Steam Controller protocols, as far as both drivers share them.
//
// Adapted from SDL3 (https://github.com/libsdl-org/SDL, zlib licence,
// Copyright (C) 1997-2026 Sam Lantinga): src/joystick/hidapi/steam/
// controller_constants.h and controller_structs.h, and the two drivers in
// SDL_hidapi_steam.c and SDL_hidapi_steam_triton.c. Altered: ported to
// TypeScript over WebHID, and cut down to what a page needs.

export const VALVE_VENDOR_ID = 0x28de;

/** The original Steam Controller (2015). */
export const STEAM_WIRED = 0x1102;
export const STEAM_DONGLE = 0x1142;
export const STEAM_BLE = [0x1105, 0x1106];
export const STEAM_ORIGINAL = [STEAM_WIRED, STEAM_DONGLE, ...STEAM_BLE];

/** The 2026 Steam Controller (SDL calls it Triton): wired, Bluetooth, the puck and the receiver. */
export const TRITON_WIRED = 0x1302;
export const TRITON_BLE = 0x1303;
export const TRITON_PUCK = 0x1304;
export const TRITON_RECEIVER = 0x1305;
export const STEAM_TRITON = [TRITON_WIRED, TRITON_BLE, TRITON_PUCK, TRITON_RECEIVER];

/** FeatureReportMessageIDs. */
export const ID_CLEAR_DIGITAL_MAPPINGS = 0x81;
export const ID_SET_DEFAULT_DIGITAL_MAPPINGS = 0x85;
export const ID_SET_SETTINGS_VALUES = 0x87;
export const ID_LOAD_DEFAULT_SETTINGS = 0x8e;
export const ID_HAPTIC_PULSE = 0x8f;
export const ID_DONGLE_GET_WIRELESS_STATE = 0xb4;

/** Settings (the ControllerSettings enum). */
export const SETTING_LEFT_TRACKPAD_MODE = 7;
export const SETTING_RIGHT_TRACKPAD_MODE = 8;
export const SETTING_LIZARD_MODE = 9;
export const SETTING_SMOOTH_ABSOLUTE_MOUSE = 24;
export const SETTING_IMU_MODE = 48;
export const SETTING_WIRELESS_PACKET_VERSION = 49;

export const TRACKPAD_ABSOLUTE_MOUSE = 0;
export const TRACKPAD_NONE = 7;
export const LIZARD_MODE_OFF = 0;
export const GYRO_MODE_SEND_RAW_ACCEL = 0x0008;
export const GYRO_MODE_SEND_RAW_GYRO = 0x0010;

/** A SET_SETTINGS_VALUES message: type, length, then (number, 16-bit value) triples. */
export function settingsMessage(...settings: [number, number][]): number[] {
  const msg = [ID_SET_SETTINGS_VALUES, settings.length * 3];
  for (const [num, value] of settings) msg.push(num, value & 0xff, (value >> 8) & 0xff);
  return msg;
}

export const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** SDL's sensor units: gyro ±2000 deg/s and accel ±2 g over the 16-bit range. */
export const GYRO_SCALE = (2000 * Math.PI) / 180 / 32768;
export const ACCEL_SCALE = (2 * 9.80665) / 32768;

/** A signed 16-bit stick or pad value as -1..1. */
export const axis16 = (v: number) => (v < 0 ? v / 32768 : v / 32767);

/**
 * What may be written to a real Steam Controller through the raw HID relay (the streamer enforces
 * the same list, `hid_relay.rs`). A relayed link can drop, and a firmware flash cut half way can
 * leave a controller dead, so a command goes through only if SDL (`controller_constants.h`) or
 * this code names it and Steam needs it for normal play: 0x81 and 0x85 (mappings off or default),
 * the reads 0x82, 0x83, 0x89, 0xae and 0xb4, 0x87 and 0x8e (settings), 0x8f (haptic pulse).
 * Everything else is refused: 0x90 (suspected reboot into the bootloader; SDL does not name it),
 * factory reset, calibration, serial number, pairing, radio records, audio update (flash), and
 * every id nothing names.
 */
export const RELAYED_COMMANDS: ReadonlySet<number> = new Set([
  0x81, 0x82, 0x83, 0x85, 0x87, 0x89, 0x8e, 0x8f, 0xae, 0xb4,
]);
/** The output reports that may be written: SDL's haptic and rumble outputs (0x80 to 0x85). */
export const RELAYED_OUTPUTS: ReadonlySet<number> = new Set([0x80, 0x81, 0x82, 0x83, 0x84, 0x85]);
/** The feature report that carries commands. */
export const COMMAND_FEATURE = 1;
export const ID_GET_ATTRIBUTES_VALUES = 0x83;

/**
 * The build time reported for the firmware and the bootloader (2038-01-19, the largest positive
 * signed 32-bit time): newer than any firmware Steam bundles, so it never starts an update through
 * Cha. A guess about Steam, which is closed; it is the same value the streamer's emulation reports.
 */
export const NEVER_UPDATE_TIME = 0x7fffffff;
/** SDL's attributes that are build times: firmware (4), radio firmware (5), dongle firmware (8), bootloader (10). */
export const BUILD_TIME_ATTRIBUTES: ReadonlySet<number> = new Set([4, 5, 8, 10]);

/** True if a write of this report to a Valve controller may go through. */
export function relayedWriteAllowed(op: string, reportId: number, data: Uint8Array): boolean {
  if (op === "set_feature") return reportId === COMMAND_FEATURE && RELAYED_COMMANDS.has(data[0] ?? -1);
  return RELAYED_OUTPUTS.has(reportId);
}

/** Sets the build-time attributes of a 0x83 reply (`[0x83, length, (tag, u32 le)...]`) in place; true if any. */
export function rewriteBuildTimes(reply: Uint8Array): boolean {
  if (reply[0] !== ID_GET_ATTRIBUTES_VALUES) return false;
  const end = Math.min(reply.length, 2 + (reply[1] ?? 0));
  let changed = false;
  for (let at = 2; at + 5 <= end; at += 5) {
    if (!BUILD_TIME_ATTRIBUTES.has(reply[at]!)) continue;
    new DataView(reply.buffer, reply.byteOffset + at + 1, 4).setUint32(0, NEVER_UPDATE_TIME, true);
    changed = true;
  }
  return changed;
}
