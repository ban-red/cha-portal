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
