//! A virtual Steam Controller over uhid: the 2026 one over Bluetooth
//! (`28de:1303`, SDL's "Triton", the kernel's "Ibex"), which is what [`VARIANT`]
//! makes, or, behind the same enum, the original Bluetooth one (`28de:1106`) or
//! its wired one (`28de:1102`).
//!
//! **The 2026 controller** has a real d-pad, four back paddles (L4, R4, L5,
//! R5), a quick-access button, two trackpads, two sticks, an accelerometer and
//! a gyroscope, and speaks plain numbered HID reports (no segments):
//! - **Input** (SDL's `SDL_hidapi_steam_triton.c` and `controller_structs.h`,
//!   zlib; the kernel's `hid-steam.c` `steam_do_ibex_input_event`, GPL-2.0, read
//!   for the protocol, nothing copied): report `0x45` (`ID_TRITON_CONTROLLER_STATE_BLE`;
//!   `0x42` and `0x47` are the wired and timestamped states, which SDL parses
//!   alike), 45 bytes after the id (`TritonMTUNoQuat_t`): sequence, 32-bit
//!   buttons, two 16-bit triggers, the two sticks, the two pads (x, y,
//!   pressure each), the IMU's 32-bit timestamp, accelerometer, gyroscope.
//!   The web page's `steam-triton.ts` reads exactly this. Report `0x43` is the
//!   battery (`TritonBatteryStatus_t`, 14 bytes), `0x79` a wireless status.
//! - **Output:** `0x80` rumble (type, intensity, then each motor's 16-bit speed
//!   and gain: `steam_ibex_haptic_rumble`; SDL resends it every 40 ms) and `0x81`
//!   a trackpad pulse (`steam_ibex_haptic_pulse`: side, on, off, count).
//! - **Feature report 1** (63 bytes after the id) carries the same command
//!   messages as the original: settings 0x87 (SDL's lizard mode and IMU mode),
//!   get attributes 0x83, string attribute 0xAE (the serial).
//! - **Serial.** The kernel asks `0xAE`, length `0x15`, attribute 1
//!   (`ATTRIB_STR_UNIT_SERIAL`; 0 is the board's) and takes the reply as
//!   `0xAE, length, attribute, serial` (`steam_get_serial`); SDL's
//!   `MsgGetStringAttribute` is the attribute and 20 characters. The 2026
//!   controller's serial is 13 characters beginning with `FXA` (Steam's
//!   support page, "Steam Controller": "13 characters beginning with FXA"),
//!   the 2015 one's ten beginning with `F`. The rest of ours is made up.
//! - **Driver.** The host kernel (6.17) has no `hid-steam` entry for
//!   `0x1303` (newer kernels bind it, as `HID_BLUETOOTH_DEVICE`), so
//!   `hid-generic` does: a `hidraw` node and no input devices.
//!
//! **The original controller** (below) is kept behind [`Variant`].
//!
//! Steam and SDL's HIDAPI driver speak a command protocol in feature reports
//! over `hidraw`; the input state comes as input reports. There is no keyboard
//! or mouse interface, so lizard mode (the controller as a mouse and
//! keyboard) has nothing to emulate: the commands that toggle it are accepted
//! and do nothing.
//!
//! **Why Bluetooth.** A real wired controller is USB interface 2 (0 and 1
//! are its keyboard and mouse), and SDL's driver takes only that one
//! (`HIDAPI_DriverSteam_IsSupportedDevice`); Steam does the same. `hidapi`
//! finds no USB interface for a uhid device and says -1, so the wired
//! controller was found and never opened. A Bluetooth controller has no
//! interfaces: SDL takes any device whose bus is Bluetooth (`is_bluetooth`
//! comes from `HID_ID`), which is what uhid's `bus` makes ours.
//!
//! **The Bluetooth controller** (SDL's `D0G_BLE2_PID`, `0x1106`; `0x1105` is
//! the same to SDL's controller list and driver, with the older firmware's
//! id) is one HID collection of vendor-defined 19-byte reports, all report
//! id 3 (`BLE_REPORT_NUMBER`). Every message is cut in segments of 18 bytes,
//! each led by a header byte: `0x80` (data), `0x40` on the last, and the
//! segment's number in the low three bits (`REPORT_SEGMENT_*`,
//! `GetSegmentHeader`, `WriteSegmentToSteamControllerPacketAssembler`).
//! - **Input:** a state is a message of its own kind (SDL's
//!   `UpdateBLESteamControllerState`, `k_EBLEReportState`): the first
//!   nibble 4, and the rest of the first byte with the second byte flags
//!   which chunks follow, in this order: buttons (the low three bytes of the
//!   mask), the two trigger bytes, the stick, the left pad, the right pad,
//!   the accelerometer, the gyroscope. The stick and the left pad have
//!   fields of their own here (on the wired report they share). We send all
//!   of them, always (31 bytes, two segments).
//! - **Commands:** a feature set report is one segment, the id included
//!   (`SetFeatureReport`), and the command message is spread over as many as
//!   it takes; a feature get returns the reply the same way, a segment per
//!   read, until the one flagged last (`GetFeatureReport`). SDL takes
//!   replies of up to 64 bytes (three segments). Between replies a read
//!   returns an idle segment (no data flag), which SDL skips.
//! - **Driver:** the kernel's `hid-steam` binds the wired controller (and
//!   the 2026 one's Bluetooth id) only; for `0x1106` it is `hid-generic`
//!   that binds, with a `hidraw` node and no input devices, which suits
//!   Steam.
//!
//! - **Descriptors:** the Bluetooth one is ours, a single vendor collection
//!   (page `0xFF00`) with report id 3 and 19-byte input and feature reports,
//!   made to fit what SDL reads and writes. A real controller's has more (its
//!   keyboard and mouse reports); nothing here checks it but the HID core,
//!   and the shape is an assumption. The wired one is modelled on the Steam
//!   Deck controller's interface 2 as `lsusb -v` shows it
//!   (KWottrich/ally-steam-controller, MIT, `reference/`).
//! - **Wired input report** (type 1, 64 bytes, no report id): the header,
//!   packet number, buttons and trigger bytes, the left pad or stick, the
//!   right pad, triggers, accelerometer, gyroscope. Layout and button bits
//!   after SDL's `controller_structs.h` and `SDL_hidapi_steam.c` (zlib) and
//!   the kernel's `hid-steam.c` (GPL-2.0; read for the protocol, nothing
//!   copied). The right stick has no place on this controller, so a
//!   deflection is a finger on the right pad. A touch on the left pad wins
//!   the fields it shares with the stick on the wired report; over
//!   Bluetooth both go out.
//! - **Commands** (a command byte, a length, a payload; the reply to the last
//!   one is what a read returns): handled are get attributes 0x83, string
//!   attribute 0xAE (the serial), set settings 0x87 (kept), get settings
//!   0x89, load default settings 0x8E (clears them), get digital mappings
//!   0x82 (always cleared), haptic pulse 0x8F (a `haptic` message per pad).
//!   Everything else (clear and set mappings, calibration, controller mode,
//!   rumble and haptic commands of newer models, ...) is acknowledged with an
//!   empty reply and does nothing.

use std::time::{Duration, Instant};

use tracing::{debug, info};

use crate::gamepad::{ENDLESS_MS, Haptic, PadEvent, PadState, Rumble, Side};
use crate::uhid::{BUS_BLUETOOTH, BUS_USB, FEATURE_REPORT, OUTPUT_REPORT, Protocol};

pub const VENDOR: u16 = 0x28de;
const PRODUCT_WIRED: u16 = 0x1102;
const PRODUCT_BLE: u16 = 0x1106;
/// The 2026 controller over Bluetooth (the kernel's `USB_DEVICE_ID_STEAM_CONTROLLER_IBEX_BLE`,
/// SDL's `controller_list.h`); its wired id, `0x1302`, would meet the same
/// missing USB interface as `0x1102` did.
const PRODUCT_TRITON: u16 = 0x1303;
pub const VERSION: u16 = 0x0100;
/// The controller's own rate, and what it says its connection interval is.
const PERIOD: Duration = Duration::from_micros(9000);
/// The 2026 controller's (SDL's `TRITON_SENSOR_UPDATE_INTERVAL_US`).
const TRITON_PERIOD: Duration = Duration::from_micros(4032);

/// How the controller connects, which is what its framing, its ids and its
/// descriptor follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// USB `28de:1102`: unnumbered 64-byte reports. SDL and Steam skip it
    /// without a USB interface number (2), which uhid cannot give.
    #[allow(dead_code, reason = "the wired variant is kept behind `VARIANT`")]
    Wired,
    /// Bluetooth `28de:1106`: report 3, in 18-byte segments.
    #[allow(
        dead_code,
        reason = "the original Bluetooth variant is kept behind `VARIANT`"
    )]
    Ble,
    /// The 2026 controller over Bluetooth, `28de:1303`: numbered reports.
    Triton,
}

/// What the `steam` kind creates.
pub const VARIANT: Variant = Variant::Triton;

impl Variant {
    pub fn product(self) -> u16 {
        match self {
            Variant::Wired => PRODUCT_WIRED,
            Variant::Ble => PRODUCT_BLE,
            Variant::Triton => PRODUCT_TRITON,
        }
    }

    pub fn bus(self) -> u16 {
        match self {
            Variant::Wired => BUS_USB,
            Variant::Ble | Variant::Triton => BUS_BLUETOOTH,
        }
    }

    /// What the kernel and `hidapi` show as the device's name.
    pub fn name(self) -> &'static str {
        match self {
            Variant::Wired | Variant::Ble => "Valve Software Steam Controller",
            Variant::Triton => "Steam Controller",
        }
    }

    pub fn descriptor(self) -> &'static [u8] {
        match self {
            Variant::Wired => &DESCRIPTOR,
            Variant::Ble => &DESCRIPTOR_BLE,
            Variant::Triton => DESCRIPTOR_TRITON,
        }
    }

    /// How often a report goes out: the controller's own rate.
    fn period(self) -> Duration {
        match self {
            Variant::Wired | Variant::Ble => PERIOD,
            Variant::Triton => TRITON_PERIOD,
        }
    }

    /// The host kernel driver that binds it (its name in sysfs).
    pub fn driver(self) -> &'static str {
        match self {
            Variant::Wired => "hid-steam",
            Variant::Ble | Variant::Triton => "hid-generic",
        }
    }

    /// The input devices that driver makes.
    pub fn inputs(self) -> usize {
        match self {
            Variant::Wired => 1,
            Variant::Ble | Variant::Triton => 0,
        }
    }
}

#[rustfmt::skip]
pub const DESCRIPTOR: [u8; 25] = [
    0x06, 0x00, 0xFF,  // Usage Page (Vendor Defined 0xFF00)
    0x09, 0x01,        // Usage (1)
    0xA1, 0x01,        // Collection (Application)
    0x15, 0x00,        //   Logical Minimum (0)
    0x26, 0xFF, 0x00,  //   Logical Maximum (255)
    0x75, 0x08,        //   Report Size (8)
    0x95, 0x40,        //   Report Count (64)
    0x09, 0x01,        //   Usage (1)
    0x81, 0x02,        //   Input (Data,Var,Abs)
    0x09, 0x01,        //   Usage (1)
    0xB1, 0x02,        //   Feature (Data,Var,Abs)
    0xC0,              // End Collection
];

/// The Bluetooth controller's: the same collection, as report 3 and with 19
/// bytes (a segment's header and its 18 bytes of payload).
#[rustfmt::skip]
pub const DESCRIPTOR_BLE: [u8; 27] = [
    0x06, 0x00, 0xFF,  // Usage Page (Vendor Defined 0xFF00)
    0x09, 0x01,        // Usage (1)
    0xA1, 0x01,        // Collection (Application)
    0x85, 0x03,        //   Report ID (3)
    0x15, 0x00,        //   Logical Minimum (0)
    0x26, 0xFF, 0x00,  //   Logical Maximum (255)
    0x75, 0x08,        //   Report Size (8)
    0x95, 0x13,        //   Report Count (19)
    0x09, 0x01,        //   Usage (1)
    0x81, 0x02,        //   Input (Data,Var,Abs)
    0x09, 0x01,        //   Usage (1)
    0xB1, 0x02,        //   Feature (Data,Var,Abs)
    0xC0,              // End Collection
];

/// The 2026 controller's: one vendor collection (our assumption, made to fit
/// SDL and the kernel's sizes: a real one's descriptor was not at hand) with
/// the input reports 0x45 (45 bytes), 0x43 (14) and 0x79 (1), the outputs 0x80
/// (9) and 0x81 (7), and feature report 1 (63).
#[rustfmt::skip]
pub const DESCRIPTOR_TRITON: &[u8] = &[
    0x06, 0x00, 0xFF,  // Usage Page (Vendor Defined 0xFF00)
    0x09, 0x01,        // Usage (1)
    0xA1, 0x01,        // Collection (Application)
    0x15, 0x00,        //   Logical Minimum (0)
    0x26, 0xFF, 0x00,  //   Logical Maximum (255)
    0x75, 0x08,        //   Report Size (8)
    0x85, 0x01,        //   Report ID (1)
    0x95, 0x3F,        //   Report Count (63)
    0x09, 0x01,        //   Usage (1)
    0xB1, 0x02,        //   Feature (Data,Var,Abs)
    0x85, 0x45,        //   Report ID (0x45)
    0x95, 0x2D,        //   Report Count (45)
    0x09, 0x01,        //   Usage (1)
    0x81, 0x02,        //   Input (Data,Var,Abs)
    0x85, 0x43,        //   Report ID (0x43)
    0x95, 0x0E,        //   Report Count (14)
    0x09, 0x01,        //   Usage (1)
    0x81, 0x02,        //   Input (Data,Var,Abs)
    0x85, 0x79,        //   Report ID (0x79)
    0x95, 0x01,        //   Report Count (1)
    0x09, 0x01,        //   Usage (1)
    0x81, 0x02,        //   Input (Data,Var,Abs)
    0x85, 0x80,        //   Report ID (0x80)
    0x95, 0x09,        //   Report Count (9)
    0x09, 0x01,        //   Usage (1)
    0x91, 0x02,        //   Output (Data,Var,Abs)
    0x85, 0x81,        //   Report ID (0x81)
    0x95, 0x07,        //   Report Count (7)
    0x09, 0x01,        //   Usage (1)
    0x91, 0x02,        //   Output (Data,Var,Abs)
    0xC0,              // End Collection
];

pub const REPORT_SIZE: usize = 64;

// The 2026 controller (SDL's `ID_TRITON_*` and `TritonButtons`).
const TRITON_STATE: u8 = 0x45;
const TRITON_BATTERY: u8 = 0x43;
const TRITON_WIRELESS: u8 = 0x79;
const TRITON_RUMBLE: u8 = 0x80;
/// How many rumble changes are logged with their bytes.
const RUMBLE_LOG_CHANGES: u32 = 60;
const TRITON_PULSE: u8 = 0x81;
/// The command messages' feature report, and what follows its id.
const TRITON_FEATURE: u8 = 1;
const TRITON_FEATURE_BYTES: usize = 63;
const TRITON_STATE_BYTES: usize = 45;
/// `k_ETritonWirelessStateConnect`, `k_EChargeStateDischarging`.
const WIRELESS_CONNECT: u8 = 2;
const CHARGE_DISCHARGING: u8 = 1;
/// How many reports (about a second) between battery and wireless reports.
const TRITON_STATUS_EVERY: u32 = 250;

const T_A: u32 = 0x0000_0001;
const T_B: u32 = 0x0000_0002;
const T_X: u32 = 0x0000_0004;
const T_Y: u32 = 0x0000_0008;
const T_QAM: u32 = 0x0000_0010;
const T_R3: u32 = 0x0000_0020;
const T_VIEW: u32 = 0x0000_0040;
const T_R4: u32 = 0x0000_0080;
const T_R5: u32 = 0x0000_0100;
const T_R: u32 = 0x0000_0200;
const T_DPAD_DOWN: u32 = 0x0000_0400;
const T_DPAD_RIGHT: u32 = 0x0000_0800;
const T_DPAD_LEFT: u32 = 0x0000_1000;
const T_DPAD_UP: u32 = 0x0000_2000;
const T_MENU: u32 = 0x0000_4000;
const T_L3: u32 = 0x0000_8000;
const T_STEAM: u32 = 0x0001_0000;
const T_L4: u32 = 0x0002_0000;
const T_L5: u32 = 0x0004_0000;
const T_L: u32 = 0x0008_0000;
const T_RIGHT_STICK_TOUCH: u32 = 0x0010_0000;
const T_RIGHT_PAD_TOUCH: u32 = 0x0020_0000;
const T_RIGHT_PAD_CLICK: u32 = 0x0040_0000;
const T_RIGHT_TRIGGER_CLICK: u32 = 0x0080_0000;
const T_LEFT_STICK_TOUCH: u32 = 0x0100_0000;
const T_LEFT_PAD_TOUCH: u32 = 0x0200_0000;
const T_LEFT_PAD_CLICK: u32 = 0x0400_0000;
const T_LEFT_TRIGGER_CLICK: u32 = 0x0800_0000;

/// The Bluetooth reports' id, and what a segment holds (SDL's
/// `BLE_REPORT_NUMBER` and `MAX_REPORT_SEGMENT_PAYLOAD_SIZE`).
const BLE_REPORT: u8 = 3;
const SEGMENT: usize = 18;
/// The report with its id and header: 20 bytes, as `hidraw` shows it.
const SEGMENT_REPORT: usize = SEGMENT + 2;
const SEGMENT_DATA: u8 = 0x80;
const SEGMENT_LAST: u8 = 0x40;
/// SDL takes messages of up to 64 bytes: three segments.
const MAX_SEGMENTS: usize = 3;
/// `k_EBLEReportState` and the chunk flags (`EBLEOptionDataChunksBitmask`).
const BLE_STATE: u8 = 4;
const CHUNK_BUTTONS: u16 = 0x10;
const CHUNK_TRIGGERS: u16 = 0x20;
const CHUNK_STICK: u16 = 0x80;
const CHUNK_LEFT_PAD: u16 = 0x100;
const CHUNK_RIGHT_PAD: u16 = 0x200;
const CHUNK_ACCEL: u16 = 0x400;
const CHUNK_GYRO: u16 = 0x800;

// The buttons' bits in the report's `ulButtons` (SDL's STEAM_*_MASK).
const RIGHT_TRIGGER: u32 = 1 << 0;
const LEFT_TRIGGER: u32 = 1 << 1;
const RIGHT_BUMPER: u32 = 1 << 2;
const LEFT_BUMPER: u32 = 1 << 3;
const NORTH: u32 = 1 << 4;
const EAST: u32 = 1 << 5;
const WEST: u32 = 1 << 6;
const SOUTH: u32 = 1 << 7;
const DPAD_UP: u32 = 1 << 8;
const DPAD_RIGHT: u32 = 1 << 9;
const DPAD_LEFT: u32 = 1 << 10;
const DPAD_DOWN: u32 = 1 << 11;
const MENU: u32 = 1 << 12;
const STEAM: u32 = 1 << 13;
const ESCAPE: u32 = 1 << 14;
const BACK_LEFT: u32 = 1 << 15;
const BACK_RIGHT: u32 = 1 << 16;
const LEFT_PAD_CLICK: u32 = 1 << 17;
const RIGHT_PAD_CLICK: u32 = 1 << 18;
const LEFT_PAD_TOUCH: u32 = 1 << 19;
const RIGHT_PAD_TOUCH: u32 = 1 << 20;
const STICK_CLICK: u32 = 1 << 22;

// Commands (SDL's `ID_*`).
const GET_DIGITAL_MAPPINGS: u8 = 0x82;
const GET_ATTRIBUTES: u8 = 0x83;
const SET_SETTINGS: u8 = 0x87;
const GET_SETTINGS: u8 = 0x89;
const LOAD_DEFAULT_SETTINGS: u8 = 0x8e;
const HAPTIC_PULSE: u8 = 0x8f;
const GET_STRING_ATTRIBUTE: u8 = 0xae;

// Attribute tags.
const ATTRIB_UNIQUE_ID: u8 = 0;
const ATTRIB_PRODUCT_ID: u8 = 1;
const ATTRIB_FIRMWARE_BUILD_TIME: u8 = 4;
const ATTRIB_HW_ID: u8 = 9;
const ATTRIB_BOOTLOADER_BUILD_TIME: u8 = 10;
const ATTRIB_CONNECTION_INTERVAL_US: u8 = 11;
/// String attributes (SDL's `ControllerStringAttributes`).
const ATTRIB_STR_UNIT_SERIAL: u8 = 1;

/// The serial a controller of this variant has for a number of the run's: a
/// unit serial starts with `F` and is ten characters on the 2015 controller
/// and thirteen, `FXA` first, on the 2026 one (Steam's support pages). The
/// rest is made up.
pub fn serial_for(variant: Variant, hash: u64) -> String {
    match variant {
        Variant::Wired | Variant::Ble => format!("F{:09X}", hash & 0xf_ffff_ffff),
        Variant::Triton => format!("FXA{:010X}", hash & 0xff_ffff_ffff),
    }
}

/// The string attributes' length in a reply (SDL's `MsgGetStringAttribute`:
/// the attribute's tag, then 20 characters; the kernel's `0x15`).
const STRING_ATTRIBUTE_LEN: usize = 21;

/// The gyroscope reports +-2000 degrees/s in an i16.
const GYRO_PER_RAD_S: f32 = 32768.0 / (2000.0 * std::f32::consts::PI / 180.0);
/// The accelerometer +-2 g.
const ACCEL_PER_MS2: f32 = 32768.0 / (2.0 * 9.80665);

pub struct SteamController {
    variant: Variant,
    slot: usize,
    serial: String,
    packet: u32,
    /// The 2026 controller: when it began (its IMU's clock), reports sent, and
    /// the motors' speeds last asked for.
    start: Instant,
    sent: u32,
    rumble: (u16, u16),
    /// Rumble reports logged so far (the first few changes, with their bytes).
    rumble_logged: u32,
    /// The command ids seen so far, for the log.
    seen: Vec<u8>,
    /// Settings written (`SET_SETTINGS`), in the order first written.
    settings: Vec<(u8, u16)>,
    /// The reply to the last command: what the next read returns.
    reply: [u8; REPORT_SIZE],
    /// Bluetooth: how many of the reply's segments have been read.
    read: usize,
    /// Bluetooth: the command being received, and the segment expected next.
    incoming: Vec<u8>,
    expected: u8,
}

impl SteamController {
    /// `serial` is up to 20 ASCII characters (see [`serial_for`]).
    pub fn new(variant: Variant, slot: usize, serial: &str) -> Self {
        Self {
            variant,
            slot,
            serial: serial.chars().take(20).collect(),
            packet: 0,
            start: Instant::now(),
            sent: 0,
            rumble: (0, 0),
            rumble_logged: 0,
            seen: Vec::new(),
            settings: Vec::new(),
            reply: [0; REPORT_SIZE],
            read: 0,
            incoming: Vec::new(),
            expected: 0,
        }
    }

    /// A number of its own for the controller (FNV-1a of the serial).
    fn id(&self) -> u32 {
        self.serial.bytes().fold(0x811c_9dc5u32, |h, b| {
            (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
        })
    }

    fn command(&mut self, payload: &[u8]) -> Vec<PadEvent> {
        let mut events = Vec::new();
        let mut reply = Vec::new();
        let cmd = payload[0];
        // The payload as long as the command says it is.
        let len = usize::from(payload.get(1).copied().unwrap_or(0));
        let body = payload.get(2..).unwrap_or_default();
        let body = &body[..len.min(body.len())];
        let handled = matches!(
            cmd,
            GET_ATTRIBUTES
                | GET_STRING_ATTRIBUTE
                | SET_SETTINGS
                | GET_SETTINGS
                | LOAD_DEFAULT_SETTINGS
                | GET_DIGITAL_MAPPINGS
                | HAPTIC_PULSE
        );
        // What Steam asks for, every kind of command once at `info`.
        if self.seen.contains(&cmd) {
            debug!(
                slot = self.slot,
                cmd = format_args!("{cmd:#04x}"),
                len,
                ?body,
                "steam controller command"
            );
        } else {
            self.seen.push(cmd);
            info!(
                slot = self.slot,
                cmd = format_args!("{cmd:#04x}"),
                len,
                body = format_args!("{:02x?}", &body[..body.len().min(16)]),
                handled,
                "steam controller: first command of its kind"
            );
        }
        match cmd {
            GET_ATTRIBUTES => {
                for (tag, value) in [
                    (ATTRIB_UNIQUE_ID, self.id()),
                    (ATTRIB_PRODUCT_ID, u32::from(self.variant.product())),
                    (ATTRIB_FIRMWARE_BUILD_TIME, 1_600_000_000),
                    (ATTRIB_HW_ID, 0x0000_0010),
                    (ATTRIB_BOOTLOADER_BUILD_TIME, 1_550_000_000),
                    (
                        ATTRIB_CONNECTION_INTERVAL_US,
                        self.variant.period().as_micros() as u32,
                    ),
                ] {
                    reply.push(tag);
                    reply.extend_from_slice(&value.to_le_bytes());
                }
            }
            GET_STRING_ATTRIBUTE => {
                // The attribute asked for (0 the board's serial, 1 the unit's;
                // both are ours) and 20 characters, padded with zeros.
                reply.push(body.first().copied().unwrap_or(ATTRIB_STR_UNIT_SERIAL));
                reply.extend_from_slice(self.serial.as_bytes());
                reply.resize(STRING_ATTRIBUTE_LEN, 0);
            }
            SET_SETTINGS => {
                for setting in body.chunks_exact(3) {
                    let value = u16::from_le_bytes([setting[1], setting[2]]);
                    match self.settings.iter_mut().find(|(reg, _)| *reg == setting[0]) {
                        Some((_, v)) => *v = value,
                        None => self.settings.push((setting[0], value)),
                    }
                }
            }
            GET_SETTINGS => {
                for (reg, value) in self.settings.iter().take(20) {
                    reply.push(*reg);
                    reply.extend_from_slice(&value.to_le_bytes());
                }
            }
            LOAD_DEFAULT_SETTINGS => self.settings.clear(),
            // No mappings: the index byte, then 0xFF.
            GET_DIGITAL_MAPPINGS => reply.push(0xff),
            HAPTIC_PULSE => events = self.pulse(body),
            _ => {}
        }
        self.reply = [0; REPORT_SIZE];
        self.read = 0;
        self.reply[0] = cmd;
        // The length, as the real reply counts the payload's bytes (one for
        // the mappings' index, the attribute and the serial included).
        let room = match self.variant {
            Variant::Wired => REPORT_SIZE,
            Variant::Ble => MAX_SEGMENTS * SEGMENT,
            Variant::Triton => TRITON_FEATURE_BYTES,
        };
        let n = reply.len().min(room - 2);
        self.reply[1] = if cmd == GET_DIGITAL_MAPPINGS {
            1
        } else {
            n as u8
        };
        self.reply[2..2 + n].copy_from_slice(&reply[..n]);
        events
    }

    /// A haptic pulse: the pad (0 right, 1 left, 2 both, the kernel's legacy
    /// swap), on and off times in microseconds, a count and a gain in dB.
    fn pulse(&self, body: &[u8]) -> Vec<PadEvent> {
        if body.len() < 7 {
            return Vec::new();
        }
        let le = |at: usize| u32::from(u16::from_le_bytes([body[at], body[at + 1]]));
        let gain_db = body.get(7).map_or(0, |g| *g as i8);
        let amp = 10f32.powf(f32::from(gain_db) / 20.0).clamp(0.0, 1.0);
        let sides: &[Side] = match body[0] {
            0 => &[Side::Right],
            1 => &[Side::Left],
            _ => &[Side::Left, Side::Right],
        };
        sides
            .iter()
            .map(|&side| {
                PadEvent::Haptic(Haptic {
                    slot: self.slot,
                    side,
                    amp,
                    on_us: le(1),
                    off_us: le(3),
                    count: le(5),
                })
            })
            .collect()
    }
}

impl SteamController {
    /// How many segments the reply is.
    fn reply_segments(&self) -> usize {
        (2 + usize::from(self.reply[1])).div_ceil(SEGMENT).max(1)
    }

    /// One command segment from the host (the report id already taken off).
    /// The command is run when the last one has come.
    fn segment(&mut self, data: &[u8]) -> Result<Vec<PadEvent>, i32> {
        let Some((&header, payload)) = data.split_first() else {
            return Err(libc::EINVAL);
        };
        // SDL's own assembler ignores segments without the data flag.
        if header & SEGMENT_DATA == 0 {
            return Ok(Vec::new());
        }
        let number = header & 7;
        if number != self.expected {
            self.incoming.clear();
            self.expected = 0;
            if number != 0 {
                return Ok(Vec::new());
            }
        }
        // A short segment is as if the rest were zeros.
        let mut chunk = [0u8; SEGMENT];
        let n = payload.len().min(SEGMENT);
        chunk[..n].copy_from_slice(&payload[..n]);
        self.incoming.extend_from_slice(&chunk);
        if header & SEGMENT_LAST == 0 {
            self.expected = (number + 1) & 7;
            return Ok(Vec::new());
        }
        self.expected = 0;
        let message = std::mem::take(&mut self.incoming);
        Ok(self.command(&message))
    }
}

impl Protocol for SteamController {
    fn report(&mut self, state: &PadState, now: Instant) -> Vec<Vec<u8>> {
        match self.variant {
            Variant::Wired => {
                self.packet = self.packet.wrapping_add(1);
                vec![input_report(state, self.packet).to_vec()]
            }
            Variant::Ble => segments(&ble_state(state)),
            Variant::Triton => {
                self.packet = self.packet.wrapping_add(1);
                // The IMU's clock, in microseconds; it has to change for SDL
                // to take the sensors.
                let micros = now.saturating_duration_since(self.start).as_micros() as u32;
                let mut out = vec![triton_state(state, self.packet as u8, micros)];
                // A battery and a wireless status now and then (a real one
                // reports its battery as it changes; how often is a guess).
                if self.sent.is_multiple_of(TRITON_STATUS_EVERY) {
                    out.push(triton_battery(state));
                    out.push(vec![TRITON_WIRELESS, WIRELESS_CONNECT]);
                }
                self.sent = self.sent.wrapping_add(1);
                out
            }
        }
    }

    fn period(&self) -> Duration {
        self.variant.period()
    }

    fn get_report(&mut self, number: u8, kind: u8) -> Result<Vec<u8>, i32> {
        if kind != FEATURE_REPORT {
            return Err(libc::EINVAL);
        }
        match self.variant {
            Variant::Triton => {
                if number != TRITON_FEATURE {
                    return Err(libc::EINVAL);
                }
                let mut out = vec![TRITON_FEATURE];
                out.extend_from_slice(&self.reply[..TRITON_FEATURE_BYTES]);
                Ok(out)
            }
            Variant::Wired => {
                if number != 0 {
                    return Err(libc::EINVAL);
                }
                // Unnumbered, but the kernel and hidraw count a leading zero.
                let mut out = vec![0];
                out.extend_from_slice(&self.reply);
                Ok(out)
            }
            Variant::Ble => {
                if number != BLE_REPORT {
                    return Err(libc::EINVAL);
                }
                // The reply a segment at a time; when it is read, idle ones.
                let at = self.read;
                let all = self.reply_segments();
                let mut out = vec![BLE_REPORT, 0];
                out.resize(SEGMENT_REPORT, 0);
                if at < all {
                    out[1] = SEGMENT_DATA
                        | (at as u8 & 7)
                        | if at + 1 == all { SEGMENT_LAST } else { 0 };
                    let from = at * SEGMENT;
                    let to = (from + SEGMENT).min(REPORT_SIZE);
                    out[2..2 + to - from].copy_from_slice(&self.reply[from..to]);
                    self.read += 1;
                }
                Ok(out)
            }
        }
    }

    fn set_report(&mut self, number: u8, kind: u8, data: &[u8]) -> Result<Vec<PadEvent>, i32> {
        if kind != FEATURE_REPORT {
            return Err(libc::EINVAL);
        }
        match self.variant {
            Variant::Triton => {
                if number != TRITON_FEATURE {
                    return Err(libc::EINVAL);
                }
                // The report id leads, when it comes (no command is 1).
                let payload = match data.split_first() {
                    Some((&TRITON_FEATURE, rest)) => rest,
                    _ => data,
                };
                if payload.is_empty() {
                    return Err(libc::EINVAL);
                }
                Ok(self.command(payload))
            }
            Variant::Wired => {
                if number != 0 {
                    return Err(libc::EINVAL);
                }
                // The same leading zero, when it comes.
                let payload = if data.len() == REPORT_SIZE + 1 && data[0] == 0 {
                    &data[1..]
                } else {
                    data
                };
                if payload.is_empty() {
                    return Err(libc::EINVAL);
                }
                Ok(self.command(payload))
            }
            Variant::Ble => {
                if number != BLE_REPORT {
                    return Err(libc::EINVAL);
                }
                // The report id leads, when it comes (a header has its top bit
                // set, so a 3 is never one).
                let data = match data.split_first() {
                    Some((&BLE_REPORT, rest)) => rest,
                    _ => data,
                };
                self.segment(data)
            }
        }
    }

    fn output(&mut self, kind: u8, data: &[u8]) -> Vec<PadEvent> {
        debug!(
            slot = self.slot,
            kind,
            data = format_args!("{data:02x?}"),
            "steam controller output report"
        );
        // Only the 2026 controller has output reports; the id leads.
        if self.variant != Variant::Triton || kind != OUTPUT_REPORT {
            return Vec::new();
        }
        match data.split_first() {
            Some((&TRITON_RUMBLE, body)) => self.rumble_report(body),
            Some((&TRITON_PULSE, body)) => self.pulse_report(body),
            _ => Vec::new(),
        }
    }
}

impl SteamController {
    /// Output report 0x80, without the id: type, intensity (16 bits), then the
    /// left and right motors' speed (16 bits) and gain (`steam_ibex_haptic_rumble`;
    /// SDL's left is the low-frequency one). SDL sends it every 40 ms while
    /// the motors run and zeros to stop; only a change is an event.
    fn rumble_report(&mut self, body: &[u8]) -> Vec<PadEvent> {
        if body.len() < 9 {
            return Vec::new();
        }
        let speed = |at: usize| u16::from_le_bytes([body[at], body[at + 1]]);
        let motors = (speed(3), speed(6));
        if motors == self.rumble {
            return Vec::new();
        }
        self.rumble = motors;
        // What the app sends besides the speeds (intensity, gains), for the
        // first changes: a weak rumble may be in fields read nowhere else.
        if self.rumble_logged < RUMBLE_LOG_CHANGES {
            self.rumble_logged += 1;
            info!(slot = self.slot, body = ?&body[..9], "steam controller: rumble report");
        }
        vec![PadEvent::Rumble(Rumble {
            slot: self.slot,
            lo: f32::from(motors.0) / 65535.0,
            hi: f32::from(motors.1) / 65535.0,
            ms: if motors == (0, 0) { 0 } else { ENDLESS_MS },
        })]
    }

    /// Output report 0x81, without the id: the side, on and off times in
    /// microseconds and a count (`steam_ibex_haptic_pulse`). The kernel's side
    /// is 1 for the left pad, 0 for the right and 2 for both (its legacy swap,
    /// as on the 2015 controller); there is no gain.
    fn pulse_report(&self, body: &[u8]) -> Vec<PadEvent> {
        if body.len() < 7 {
            return Vec::new();
        }
        let le = |at: usize| u32::from(u16::from_le_bytes([body[at], body[at + 1]]));
        let sides: &[Side] = match body[0] {
            0 => &[Side::Right],
            1 => &[Side::Left],
            _ => &[Side::Left, Side::Right],
        };
        sides
            .iter()
            .map(|&side| {
                PadEvent::Haptic(Haptic {
                    slot: self.slot,
                    side,
                    amp: 1.0,
                    on_us: le(1),
                    off_us: le(3),
                    count: le(5),
                })
            })
            .collect()
    }
}

/// A finger's place as the pad reports it: -32767..32767, up positive.
fn pad_axis(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// What a state says in the controller's terms, for either report.
struct Parts {
    /// `ulButtons`, the touch flags included.
    buttons: u32,
    triggers: [f32; 2],
    stick: (i16, i16),
    /// A finger on the left pad, when there is one.
    left: Option<(i16, i16)>,
    right: (i16, i16),
    /// The accelerometer, then the gyroscope, in the controller's axes and units.
    sensors: [i16; 6],
}

fn parts(state: &PadState) -> Parts {
    let live = !state.gone;
    let b = |i: usize| {
        if live {
            state.b.get(i).copied().unwrap_or(0.0)
        } else {
            0.0
        }
    };
    let a = |i: usize| {
        if live {
            state.a.get(i).copied().unwrap_or(0.0)
        } else {
            0.0
        }
    };
    let on = |i: usize| b(i) > 0.5;
    let touch = |id: u32| {
        state
            .touch
            .iter()
            .find(|t| t.id == id && t.down && live)
            .map(|t| (pad_axis(t.x * 2.0 - 1.0), pad_axis(1.0 - t.y * 2.0)))
    };

    let click = |i: usize| b(i) > 0.9;
    let mut buttons = 0u32;
    for (down, bit) in [
        (click(7), RIGHT_TRIGGER),
        (click(6), LEFT_TRIGGER),
        (on(5), RIGHT_BUMPER),
        (on(4), LEFT_BUMPER),
        (on(3), NORTH),
        (on(1), EAST),
        (on(2), WEST),
        (on(0), SOUTH),
        (on(12), DPAD_UP),
        (on(15), DPAD_RIGHT),
        (on(14), DPAD_LEFT),
        (on(13), DPAD_DOWN),
        (on(8), MENU),
        (on(16), STEAM),
        (on(9), ESCAPE),
        (on(19), BACK_LEFT),
        (on(20), BACK_RIGHT),
        (on(18), LEFT_PAD_CLICK),
        (on(17) || on(11), RIGHT_PAD_CLICK),
        (on(10), STICK_CLICK),
    ] {
        if down {
            buttons |= bit;
        }
    }
    let left = touch(0);
    if left.is_some() {
        buttons |= LEFT_PAD_TOUCH;
    }
    // No right pad touch: the right stick, when it is out of its dead zone, is a
    // finger on the pad.
    let right = match touch(1) {
        Some(at) => Some(at),
        None if a(2).hypot(a(3)) > 0.1 => Some((pad_axis(a(2)), pad_axis(-a(3)))),
        None => None,
    };
    if right.is_some() {
        buttons |= RIGHT_PAD_TOUCH;
    }

    let mut sensors = [0i16; 6];
    if live {
        let unit = |v: f32, per: f32| (v * per).round().clamp(-32768.0, 32767.0) as i16;
        let accel = state.accel.unwrap_or_default();
        let gyro = state.gyro.unwrap_or_default();
        // The sensors' axes are not SDL's: SDL's y and z are the device's z and
        // y (and the device's y points the other way for acceleration).
        sensors = [
            unit(accel[0], ACCEL_PER_MS2),
            unit(-accel[2], ACCEL_PER_MS2),
            unit(accel[1], ACCEL_PER_MS2),
            unit(gyro[0], GYRO_PER_RAD_S),
            unit(gyro[2], GYRO_PER_RAD_S),
            unit(gyro[1], GYRO_PER_RAD_S),
        ];
    }
    Parts {
        buttons,
        triggers: [b(6).clamp(0.0, 1.0), b(7).clamp(0.0, 1.0)],
        stick: (pad_axis(a(0)), pad_axis(-a(1))),
        left,
        right: right.unwrap_or((0, 0)),
        sensors,
    }
}

/// The 2026 controller's state report, id first (46 bytes): see the header.
/// `timestamp` is its IMU's clock in microseconds.
fn triton_state(state: &PadState, seq: u8, timestamp: u32) -> Vec<u8> {
    let live = !state.gone;
    let b = |i: usize| {
        if live {
            state.b.get(i).copied().unwrap_or(0.0)
        } else {
            0.0
        }
    };
    let a = |i: usize| {
        if live {
            state.a.get(i).copied().unwrap_or(0.0)
        } else {
            0.0
        }
    };
    let on = |i: usize| b(i) > 0.5;
    let click = |i: usize| b(i) > 0.9;
    let touch = |id: u32| {
        state
            .touch
            .iter()
            .find(|t| t.id == id && t.down && live)
            .map(|t| (pad_axis(t.x * 2.0 - 1.0), pad_axis(1.0 - t.y * 2.0)))
    };
    let (left, right) = (touch(0), touch(1));
    // Capacitive sensing under the thumbs: a stick out of its dead zone is
    // touched (a guess; nothing says where a real one's threshold is).
    let left_stick = a(0).hypot(a(1)) > 0.05;
    let right_stick = a(2).hypot(a(3)) > 0.05;
    let mut buttons = 0u32;
    for (down, bit) in [
        (on(0), T_A),
        (on(1), T_B),
        (on(2), T_X),
        (on(3), T_Y),
        (on(23), T_QAM),
        (on(11), T_R3),
        // SDL's Start is the view button and its Back the menu one.
        (on(9), T_VIEW),
        (on(20), T_R4),
        (on(22), T_R5),
        (on(5), T_R),
        (on(13), T_DPAD_DOWN),
        (on(15), T_DPAD_RIGHT),
        (on(14), T_DPAD_LEFT),
        (on(12), T_DPAD_UP),
        (on(8), T_MENU),
        (on(10), T_L3),
        (on(16), T_STEAM),
        (on(19), T_L4),
        (on(21), T_L5),
        (on(4), T_L),
        (right_stick, T_RIGHT_STICK_TOUCH),
        (right.is_some(), T_RIGHT_PAD_TOUCH),
        (on(17), T_RIGHT_PAD_CLICK),
        (click(7), T_RIGHT_TRIGGER_CLICK),
        (left_stick, T_LEFT_STICK_TOUCH),
        (left.is_some(), T_LEFT_PAD_TOUCH),
        (on(18), T_LEFT_PAD_CLICK),
        (click(6), T_LEFT_TRIGGER_CLICK),
    ] {
        if down {
            buttons |= bit;
        }
    }
    let trigger = |i: usize| (b(i).clamp(0.0, 1.0) * 32767.0).round() as i16;
    let mut m = Vec::with_capacity(1 + TRITON_STATE_BYTES);
    m.push(TRITON_STATE);
    m.push(seq);
    m.extend_from_slice(&buttons.to_le_bytes());
    for v in [
        trigger(6),
        trigger(7),
        pad_axis(a(0)),
        pad_axis(-a(1)),
        pad_axis(a(2)),
        pad_axis(-a(3)),
    ] {
        m.extend_from_slice(&v.to_le_bytes());
    }
    // A pad's finger and its pressure (a guess: SDL reads it as 0..1 over
    // 32768; a click is a full press, a touch a light one).
    for (finger, clicked) in [(left, on(18)), (right, on(17))] {
        let (x, y) = finger.unwrap_or((0, 0));
        let pressure: u16 = match (finger.is_some(), clicked) {
            (_, true) => 32767,
            (true, false) => 8192,
            (false, false) => 0,
        };
        for v in [x as u16, y as u16, pressure] {
            m.extend_from_slice(&v.to_le_bytes());
        }
    }
    m.extend_from_slice(&timestamp.to_le_bytes());
    for v in parts(state).sensors {
        m.extend_from_slice(&v.to_le_bytes());
    }
    m
}

/// The battery report (id first, 15 bytes): the charge state, the level in
/// percent, the voltages, currents and temperature (`TritonBatteryStatus_t`),
/// the last as zeros; a state without a battery reading is a full one.
fn triton_battery(state: &PadState) -> Vec<u8> {
    let level = (state.bat.unwrap_or(1.0).clamp(0.0, 1.0) * 100.0).round() as u8;
    let millivolts = 3300 + u16::from(level) * 9;
    let mut m = vec![TRITON_BATTERY, CHARGE_DISCHARGING, level];
    m.extend_from_slice(&millivolts.to_le_bytes());
    m.resize(1 + 14, 0);
    m
}

/// The 64-byte wired input report for a state.
fn input_report(state: &PadState, packet: u32) -> [u8; REPORT_SIZE] {
    let p = parts(state);
    // A touch on the left pad takes the shared fields; else they are the stick.
    let left = p.left.unwrap_or(p.stick);
    let mut r = [0u8; REPORT_SIZE];
    // Version 1, type 1 (state), 60 bytes of payload.
    r[..4].copy_from_slice(&[1, 0, 1, 60]);
    r[4..8].copy_from_slice(&packet.to_le_bytes());
    r[8..11].copy_from_slice(&p.buttons.to_le_bytes()[..3]);
    r[11] = (p.triggers[0] * 255.0).round() as u8;
    r[12] = (p.triggers[1] * 255.0).round() as u8;
    let words: [i16; 6] = [
        left.0,
        left.1,
        p.right.0,
        p.right.1,
        (p.triggers[0] * 32767.0).round() as i16,
        (p.triggers[1] * 32767.0).round() as i16,
    ];
    for (i, w) in words.iter().enumerate() {
        r[16 + 2 * i..18 + 2 * i].copy_from_slice(&w.to_le_bytes());
    }
    for (i, w) in p.sensors.iter().enumerate() {
        r[28 + 2 * i..30 + 2 * i].copy_from_slice(&w.to_le_bytes());
    }
    r
}

/// The Bluetooth state message for a state: 31 bytes, every chunk (the first
/// byte's high nibble and the second byte flag them, its low nibble is 4).
fn ble_state(state: &PadState) -> Vec<u8> {
    let p = parts(state);
    let chunks = CHUNK_BUTTONS
        | CHUNK_TRIGGERS
        | CHUNK_STICK
        | CHUNK_LEFT_PAD
        | CHUNK_RIGHT_PAD
        | CHUNK_ACCEL
        | CHUNK_GYRO;
    let mut m = vec![chunks as u8 | BLE_STATE, (chunks >> 8) as u8];
    m.extend_from_slice(&p.buttons.to_le_bytes()[..3]);
    m.push((p.triggers[0] * 255.0).round() as u8);
    m.push((p.triggers[1] * 255.0).round() as u8);
    let (left_x, left_y) = p.left.unwrap_or((0, 0));
    for v in [p.stick.0, p.stick.1, left_x, left_y, p.right.0, p.right.1]
        .into_iter()
        .chain(p.sensors)
    {
        m.extend_from_slice(&v.to_le_bytes());
    }
    m
}

/// A message as Bluetooth reports: report 3, a header, 18 bytes (the last
/// segment padded with zeros).
fn segments(message: &[u8]) -> Vec<Vec<u8>> {
    let count = message.len().div_ceil(SEGMENT).max(1);
    (0..count)
        .map(|n| {
            let mut r = vec![0u8; SEGMENT_REPORT];
            r[0] = BLE_REPORT;
            r[1] = SEGMENT_DATA | (n as u8 & 7) | if n + 1 == count { SEGMENT_LAST } else { 0 };
            let from = n * SEGMENT;
            let chunk = &message[from..(from + SEGMENT).min(message.len())];
            r[2..2 + chunk.len()].copy_from_slice(chunk);
            r
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamepad::Touch;

    fn state() -> PadState {
        let mut b = vec![0.0; 24];
        b[0] = 1.0; // A
        b[6] = 1.0; // left trigger
        b[16] = 1.0; // guide: the Steam button
        b[19] = 1.0; // left grip
        PadState {
            i: 0,
            b,
            a: vec![0.5, 0.0, 0.0, 0.0],
            gyro: Some([0.0, 1.0, 0.0]),
            accel: Some([0.0, 9.80665, 0.0]),
            touch: vec![Touch {
                id: 1,
                x: 1.0,
                y: 0.0,
                down: true,
            }],
            ..PadState::default()
        }
    }

    fn controller() -> SteamController {
        SteamController::new(Variant::Wired, 1, "F01234567")
    }

    /// Sends a command as hidraw does (a zero, then 64 bytes) and reads the reply.
    fn exchange(sc: &mut SteamController, cmd: &[u8]) -> (Vec<PadEvent>, Vec<u8>) {
        let mut data = vec![0u8; 65];
        data[1..=cmd.len()].copy_from_slice(cmd);
        let events = sc.set_report(0, FEATURE_REPORT, &data).unwrap();
        (events, sc.get_report(0, FEATURE_REPORT).unwrap())
    }

    #[test]
    fn the_input_report_for_a_known_state_is_byte_exact() {
        let r = input_report(&state(), 5);
        let mut want = [0u8; 64];
        want[..4].copy_from_slice(&[1, 0, 1, 60]);
        want[4] = 5;
        // A, the left trigger's click, the Steam button, the left grip, a finger
        // on the right pad; the trigger's analog byte.
        want[8..11].copy_from_slice(&[0x82, 0xa0, 0x10]);
        want[11] = 255;
        // The stick (the left pad isn't touched): right half; the finger at the
        // right pad's top right; the triggers.
        want[16..18].copy_from_slice(&16384i16.to_le_bytes());
        want[20..22].copy_from_slice(&32767i16.to_le_bytes());
        want[22..24].copy_from_slice(&32767i16.to_le_bytes());
        want[24..26].copy_from_slice(&32767i16.to_le_bytes());
        // 1 g up is the device's third accelerometer axis (bytes 32..34); 1 rad/s
        // of yaw its third gyroscope one (38..40) (units 2000 degrees/s over 32768).
        want[32..34].copy_from_slice(&16384i16.to_le_bytes());
        want[38..40].copy_from_slice(&939i16.to_le_bytes());
        assert_eq!(r, want);
    }

    #[test]
    fn pads_sticks_and_rest() {
        // The left pad touched wins the shared fields and marks itself; the
        // y axis points up.
        let touched = PadState {
            touch: vec![Touch {
                id: 0,
                x: 0.0,
                y: 1.0,
                down: true,
            }],
            a: vec![0.9, 0.9],
            ..PadState::default()
        };
        let r = input_report(&touched, 1);
        assert_eq!(r[10] & 0x08, 0x08);
        assert_eq!(i16::from_le_bytes([r[16], r[17]]), -32767);
        assert_eq!(i16::from_le_bytes([r[18], r[19]]), -32767);
        // The stick pushed down (positive in the page) is negative on the pad.
        let stick = PadState {
            a: vec![0.0, 1.0, 1.0, 0.0],
            ..PadState::default()
        };
        let r = input_report(&stick, 1);
        assert_eq!(i16::from_le_bytes([r[18], r[19]]), -32767);
        // The right stick is a finger on the right pad.
        assert_eq!(r[10] & 0x10, 0x10);
        assert_eq!(i16::from_le_bytes([r[20], r[21]]), 32767);
        // At rest: no buttons, no fingers, nothing deflected.
        let rest = input_report(&PadState::default(), 1);
        assert_eq!(&rest[8..16], &[0; 8]);
        assert_eq!(&rest[16..24], &[0; 8]);
        let gone = input_report(
            &PadState {
                gone: true,
                ..state()
            },
            1,
        );
        assert_eq!(&gone[8..48], &rest[8..48]);
    }

    #[test]
    fn attributes_and_the_serial_are_answered() {
        let mut sc = controller();
        let (_, reply) = exchange(&mut sc, &[GET_ATTRIBUTES, 0]);
        // A leading zero (the report is unnumbered), the command, the length
        // (six attributes of five bytes), then the attributes.
        assert_eq!(&reply[..3], &[0, GET_ATTRIBUTES, 30]);
        assert_eq!(reply.len(), 65);
        assert_eq!(&reply[8..13], &[1, 0x02, 0x11, 0, 0]);
        assert_eq!(&reply[28..33], &[11, 0x28, 0x23, 0, 0]);
        let (_, reply) = exchange(&mut sc, &[GET_STRING_ATTRIBUTE, 21, 1]);
        // As the kernel reads it: the command, 0x15, the attribute, the
        // serial, padded with zeros to 20 characters.
        assert_eq!(&reply[..4], &[0, GET_STRING_ATTRIBUTE, 21, 1]);
        assert_eq!(&reply[4..13], b"F01234567");
        assert_eq!(&reply[13..24], &[0; 11]);
    }

    #[test]
    fn settings_are_kept_and_cleared_and_unknown_commands_acked() {
        let mut sc = controller();
        let (events, reply) = exchange(&mut sc, &[SET_SETTINGS, 6, 7, 0, 0, 0x30, 0x18, 0]);
        assert!(events.is_empty());
        assert_eq!(&reply[..3], &[0, SET_SETTINGS, 0]);
        exchange(&mut sc, &[SET_SETTINGS, 3, 7, 5, 0]);
        let (_, reply) = exchange(&mut sc, &[GET_SETTINGS, 0]);
        // Setting 7 was rewritten in place, 0x30 kept.
        assert_eq!(&reply[1..9], &[GET_SETTINGS, 6, 7, 5, 0, 0x30, 0x18, 0]);
        exchange(&mut sc, &[LOAD_DEFAULT_SETTINGS, 0]);
        let (_, reply) = exchange(&mut sc, &[GET_SETTINGS, 0]);
        assert_eq!(&reply[1..4], &[GET_SETTINGS, 0, 0]);
        let (_, reply) = exchange(&mut sc, &[GET_DIGITAL_MAPPINGS, 1, 0]);
        assert_eq!(&reply[1..4], &[GET_DIGITAL_MAPPINGS, 1, 0xff]);
        // Clearing the mappings, lizard mode, a calibration and a command we
        // have never heard of: acknowledged.
        for cmd in [0x81, 0x85, 0xb5, 0xee] {
            let (events, reply) = exchange(&mut sc, &[cmd, 0]);
            assert!(events.is_empty());
            assert_eq!(&reply[..3], &[0, cmd, 0]);
        }
        // The raw form (no leading zero) works too, and a read of another report fails.
        let mut raw = vec![0u8; 64];
        raw[0] = GET_ATTRIBUTES;
        assert!(sc.set_report(0, FEATURE_REPORT, &raw).is_ok());
        assert_eq!(sc.get_report(0, FEATURE_REPORT).unwrap()[1], GET_ATTRIBUTES);
        assert_eq!(sc.get_report(1, FEATURE_REPORT), Err(libc::EINVAL));
        assert_eq!(sc.set_report(0, FEATURE_REPORT, &[]), Err(libc::EINVAL));
    }

    #[test]
    fn haptic_pulses_become_events_per_pad() {
        let mut sc = controller();
        // Left pad (1 on the wire), 400 us on, 0 off, once, gain 0 dB.
        let (events, _) = exchange(&mut sc, &[HAPTIC_PULSE, 8, 1, 0x90, 0x01, 0, 0, 1, 0, 0]);
        assert_eq!(
            events,
            [PadEvent::Haptic(Haptic {
                slot: 1,
                side: Side::Left,
                amp: 1.0,
                on_us: 400,
                off_us: 0,
                count: 1
            })]
        );
        // The right pad, -6 dB: half the amplitude; both pads: both.
        let (events, _) = exchange(&mut sc, &[HAPTIC_PULSE, 8, 0, 100, 0, 50, 0, 3, 0, 0xfa]);
        match events[..] {
            [PadEvent::Haptic(h)] => {
                assert_eq!(
                    (h.side, h.on_us, h.off_us, h.count),
                    (Side::Right, 100, 50, 3)
                );
                assert!((h.amp - 0.501).abs() < 0.001);
            }
            ref other => panic!("{other:?}"),
        }
        let (events, _) = exchange(&mut sc, &[HAPTIC_PULSE, 8, 2, 1, 0, 1, 0, 1, 0, 0]);
        assert_eq!(events.len(), 2);
        // Too short to be a pulse.
        assert!(exchange(&mut sc, &[HAPTIC_PULSE, 2, 0, 1]).0.is_empty());
    }

    fn ble() -> SteamController {
        SteamController::new(Variant::Ble, 1, "F01234567")
    }

    /// Sends a command as `hidraw` does over Bluetooth (segments of report 3, the
    /// id first) and reads the reply the way SDL does: segments until the last,
    /// reassembled. The reply is as long as its segments are.
    fn ble_exchange(sc: &mut SteamController, cmd: &[u8]) -> (Vec<PadEvent>, Vec<u8>) {
        let mut events = Vec::new();
        for segment in segments(cmd) {
            assert_eq!(segment.len(), 20);
            events.extend(sc.set_report(BLE_REPORT, FEATURE_REPORT, &segment).unwrap());
        }
        let mut reply = Vec::new();
        for n in 0..MAX_SEGMENTS as u8 {
            let r = sc.get_report(BLE_REPORT, FEATURE_REPORT).unwrap();
            assert_eq!(r.len(), 20);
            assert_eq!(r[0], BLE_REPORT);
            assert_eq!(r[1] & 0x80, 0x80);
            assert_eq!(r[1] & 7, n);
            reply.extend_from_slice(&r[2..]);
            if r[1] & 0x40 != 0 {
                return (events, reply);
            }
        }
        panic!("no last segment");
    }

    #[test]
    fn the_bluetooth_device_is_what_sdl_takes() {
        assert_eq!(Variant::Ble.product(), 0x1106);
        assert_eq!(Variant::Ble.bus(), BUS_BLUETOOTH);
        assert_eq!(Variant::Wired.product(), 0x1102);
        // One numbered collection of 19-byte reports: a header and a segment.
        let d = Variant::Ble.descriptor();
        assert_eq!(&d[7..9], &[0x85, BLE_REPORT]);
        assert_eq!(&d[16..18], &[0x95, (SEGMENT + 1) as u8]);
        assert_eq!(d.len(), DESCRIPTOR_BLE.len());
    }

    #[test]
    fn the_bluetooth_state_for_a_known_state_is_byte_exact() {
        let m = ble_state(&state());
        let mut want = vec![0xb4, 0x0f];
        // The same buttons as the wired report, then the trigger bytes.
        want.extend_from_slice(&[0x82, 0xa0, 0x10, 255, 0]);
        // The stick (the right half), the left pad (no finger), the right pad's
        // finger at its top right.
        for v in [16384i16, 0, 0, 0, 32767, 32767] {
            want.extend_from_slice(&v.to_le_bytes());
        }
        // The accelerometer (1 g up is the third axis), then the gyroscope (1
        // rad/s of yaw is its third).
        for v in [0i16, 0, 16384, 0, 0, 939] {
            want.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(m, want);
        assert_eq!(m.len(), 31);
        // Two segments of report 3: numbered, the second flagged last and
        // padded to 18 bytes.
        let s = segments(&m);
        assert_eq!(s.len(), 2);
        assert_eq!(&s[0][..2], &[3, 0x80]);
        assert_eq!(&s[0][2..], &want[..18]);
        assert_eq!(&s[1][..2], &[3, 0xc1]);
        assert_eq!(&s[1][2..15], &want[18..]);
        assert_eq!(&s[1][15..], &[0; 5]);
        // And that is what the controller sends for it.
        assert_eq!(ble().report(&state(), Instant::now()), s);
    }

    #[test]
    fn the_bluetooth_state_reads_back_as_sdl_reads_it() {
        // SDL's UpdateBLESteamControllerState, over what we send.
        let touched = PadState {
            touch: vec![Touch {
                id: 0,
                x: 0.0,
                y: 1.0,
                down: true,
            }],
            a: vec![0.9, 0.9],
            ..PadState::default()
        };
        let m = ble_state(&touched);
        let mask = u16::from(m[0] & 0xf0) | u16::from(m[1]) << 8;
        assert_eq!(m[0] & 0x0f, 4);
        assert_eq!(mask, 0x0fb0);
        let buttons = u32::from_le_bytes([m[2], m[3], m[4], 0]);
        assert_eq!(buttons & LEFT_PAD_TOUCH, LEFT_PAD_TOUCH);
        let i16_at = |at: usize| i16::from_le_bytes([m[at], m[at + 1]]);
        // The stick and the left pad each have their own fields.
        assert_eq!((i16_at(7), i16_at(9)), (29490, -29490));
        assert_eq!((i16_at(11), i16_at(13)), (-32767, -32767));
        // Nothing at all: no buttons, no fingers.
        let rest = ble_state(&PadState::default());
        assert!(rest[2..].iter().all(|b| *b == 0));
    }

    #[test]
    fn bluetooth_commands_and_replies_are_in_segments() {
        let mut sc = ble();
        // Attributes: thirty-two bytes, so two segments, the product the Bluetooth one's.
        let (_, reply) = ble_exchange(&mut sc, &[GET_ATTRIBUTES, 0]);
        assert_eq!(&reply[..3], &[GET_ATTRIBUTES, 30, 0]);
        assert_eq!(reply.len(), 36);
        assert_eq!(&reply[7..12], &[1, 0x06, 0x11, 0, 0]);
        assert_eq!(&reply[27..32], &[11, 0x28, 0x23, 0, 0]);
        // A read with nothing left to say is an idle segment: no data flag.
        let idle = sc.get_report(BLE_REPORT, FEATURE_REPORT).unwrap();
        assert_eq!(&idle[..2], &[BLE_REPORT, 0]);
        // The serial: one segment.
        let (_, reply) = ble_exchange(&mut sc, &[GET_STRING_ATTRIBUTE, 21, 1]);
        assert_eq!(&reply[..3], &[GET_STRING_ATTRIBUTE, 21, 1]);
        assert_eq!(&reply[3..12], b"F01234567");
        assert_eq!(&reply[12..23], &[0; 11]);
        // A command of two segments (seven settings, twenty-three bytes) is run
        // when the last has come, and read back in two.
        let mut set = vec![SET_SETTINGS, 21];
        for n in 0..7u8 {
            set.extend_from_slice(&[n + 1, n, 0]);
        }
        let (events, _) = ble_exchange(&mut sc, &set);
        assert!(events.is_empty());
        let (_, reply) = ble_exchange(&mut sc, &[GET_SETTINGS, 0]);
        assert_eq!(&reply[..2], &[GET_SETTINGS, 21]);
        assert_eq!(&reply[2..8], &[1, 0, 0, 2, 1, 0]);
        // The raw form (the id left off) and the unknown commands too.
        let (events, reply) = ble_exchange(&mut sc, &[0xee, 0]);
        assert!(events.is_empty());
        assert_eq!(&reply[..2], &[0xee, 0]);
        let raw = &segments(&[GET_ATTRIBUTES, 0])[0][1..];
        assert!(sc.set_report(BLE_REPORT, FEATURE_REPORT, raw).is_ok());
        assert_eq!(
            sc.get_report(BLE_REPORT, FEATURE_REPORT).unwrap()[2],
            GET_ATTRIBUTES
        );
        // Not report 0, nor an output report.
        assert_eq!(sc.get_report(0, FEATURE_REPORT), Err(libc::EINVAL));
        assert_eq!(
            sc.set_report(0, FEATURE_REPORT, &[0; 65]),
            Err(libc::EINVAL)
        );
        assert_eq!(sc.get_report(BLE_REPORT, 1), Err(libc::EINVAL));
    }

    #[test]
    fn a_bluetooth_command_out_of_order_is_dropped() {
        let mut sc = ble();
        let mut set = vec![SET_SETTINGS, 21];
        for n in 0..7u8 {
            set.extend_from_slice(&[n + 1, n, 0]);
        }
        let s = segments(&set);
        // The second segment alone: nothing runs.
        assert!(
            sc.set_report(BLE_REPORT, FEATURE_REPORT, &s[1])
                .unwrap()
                .is_empty()
        );
        let (_, reply) = ble_exchange(&mut sc, &[GET_SETTINGS, 0]);
        assert_eq!(&reply[..2], &[GET_SETTINGS, 0]);
        // A segment without the data flag is ignored; the first then the last run.
        let mut empty = vec![BLE_REPORT];
        empty.resize(20, 0);
        assert!(
            sc.set_report(BLE_REPORT, FEATURE_REPORT, &empty)
                .unwrap()
                .is_empty()
        );
        sc.set_report(BLE_REPORT, FEATURE_REPORT, &s[0]).unwrap();
        sc.set_report(BLE_REPORT, FEATURE_REPORT, &s[1]).unwrap();
        let (_, reply) = ble_exchange(&mut sc, &[GET_SETTINGS, 0]);
        assert_eq!(&reply[..2], &[GET_SETTINGS, 21]);
    }

    #[test]
    fn bluetooth_haptic_pulses_become_events() {
        let mut sc = ble();
        let (events, _) = ble_exchange(&mut sc, &[HAPTIC_PULSE, 8, 1, 0x90, 0x01, 0, 0, 1, 0, 0]);
        assert_eq!(
            events,
            [PadEvent::Haptic(Haptic {
                slot: 1,
                side: Side::Left,
                amp: 1.0,
                on_us: 400,
                off_us: 0,
                count: 1
            })]
        );
    }

    fn triton() -> SteamController {
        SteamController::new(Variant::Triton, 2, "FXA0123456789")
    }

    /// A command as `hidraw` carries it: feature report 1, the id first, 64 bytes.
    fn triton_exchange(sc: &mut SteamController, cmd: &[u8]) -> (Vec<PadEvent>, Vec<u8>) {
        let mut data = vec![0u8; 64];
        data[0] = TRITON_FEATURE;
        data[1..=cmd.len()].copy_from_slice(cmd);
        let events = sc
            .set_report(TRITON_FEATURE, FEATURE_REPORT, &data)
            .unwrap();
        (
            events,
            sc.get_report(TRITON_FEATURE, FEATURE_REPORT).unwrap(),
        )
    }

    #[test]
    fn the_2026_controller_is_the_one_made() {
        assert_eq!(VARIANT, Variant::Triton);
        assert_eq!(Variant::Triton.product(), 0x1303);
        assert_eq!(Variant::Triton.bus(), BUS_BLUETOOTH);
        assert_eq!(Variant::Triton.name(), "Steam Controller");
        assert_eq!(Variant::Triton.period(), Duration::from_micros(4032));
        // The descriptor's report ids and sizes: feature 1 (63), inputs 0x45
        // (45), 0x43 (14) and 0x79 (1), outputs 0x80 (9) and 0x81 (7).
        // Short items: the prefix's low two bits are the data's size (3 is 4).
        let mut got = Vec::new();
        let (mut id, mut count) = (0, 0);
        let mut at = 0;
        while at < DESCRIPTOR_TRITON.len() {
            let item = DESCRIPTOR_TRITON[at];
            let size = match item & 3 {
                3 => 4,
                n => usize::from(n),
            };
            let data = DESCRIPTOR_TRITON.get(at + 1).copied().unwrap_or(0);
            match item {
                0x85 => id = data,
                0x95 => count = data,
                0x81 | 0xb1 | 0x91 => got.push((id, count, item)),
                _ => {}
            }
            at += 1 + size;
        }
        assert_eq!(
            got,
            [
                (1, 63, 0xb1),
                (0x45, 45, 0x81),
                (0x43, 14, 0x81),
                (0x79, 1, 0x81),
                (0x80, 9, 0x91),
                (0x81, 7, 0x91)
            ]
        );
    }

    #[test]
    fn serials_are_as_valve_formats_them() {
        let two = serial_for(Variant::Triton, 0x1234_5678_9abc_def0);
        assert_eq!(two, "FXA789ABCDEF0");
        assert_eq!(two.len(), 13);
        let one = serial_for(Variant::Ble, 0x1234_5678_9abc_def0);
        assert_eq!(one, "F89ABCDEF0");
        assert_eq!(one.len(), 10);
    }

    fn triton_known_state() -> PadState {
        let mut b = vec![0.0; 24];
        b[0] = 1.0; // A
        b[6] = 1.0; // left trigger
        b[12] = 1.0; // d-pad up
        b[15] = 1.0; // d-pad right
        b[16] = 1.0; // Steam
        b[19] = 1.0; // L4
        b[23] = 1.0; // quick access
        PadState {
            i: 0,
            b,
            a: vec![0.5, 0.0, 0.0, -1.0],
            gyro: Some([0.0, 1.0, 0.0]),
            accel: Some([0.0, 9.80665, 0.0]),
            touch: vec![Touch {
                id: 1,
                x: 1.0,
                y: 0.0,
                down: true,
            }],
            ..PadState::default()
        }
    }

    #[test]
    fn the_2026_state_for_a_known_state_is_byte_exact() {
        let r = triton_state(&triton_known_state(), 7, 0x0102_0304);
        let mut want = vec![0x45, 7];
        // A, the quick-access button, d-pad up and right, Steam, L4, the left
        // trigger's click and the right pad's touch; the left stick's touch too.
        let buttons = T_A
            | T_QAM
            | T_DPAD_UP
            | T_DPAD_RIGHT
            | T_STEAM
            | T_L4
            | T_LEFT_TRIGGER_CLICK
            | T_RIGHT_PAD_TOUCH
            | T_LEFT_STICK_TOUCH
            | T_RIGHT_STICK_TOUCH;
        want.extend_from_slice(&buttons.to_le_bytes());
        // The triggers, the left stick (right half), the right stick (up in the
        // page, -1, is positive on the wire: the page negates Y).
        for v in [32767i16, 0, 16384, 0, 0, 32767] {
            want.extend_from_slice(&v.to_le_bytes());
        }
        // The left pad (untouched), the right pad's finger at the top right,
        // lightly pressed.
        for v in [0i16, 0, 0, 32767, 32767, 8192] {
            want.extend_from_slice(&v.to_le_bytes());
        }
        want.extend_from_slice(&0x0102_0304u32.to_le_bytes());
        for v in [0i16, 0, 16384, 0, 0, 939] {
            want.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(r, want);
        assert_eq!(r.len(), 46);
    }

    #[test]
    fn the_2026_state_reads_back_as_the_page_reads_it() {
        // The page's `state32` (steam-triton.ts), over what we send: the
        // buttons' bits, the pads' place and the sensors' axes.
        let r = triton_state(&triton_known_state(), 1, 0);
        let d = &r[1..];
        let i16_at = |at: usize| f32::from(i16::from_le_bytes([d[at], d[at + 1]]));
        let buttons = u32::from_le_bytes([d[1], d[2], d[3], d[4]]);
        assert_ne!(buttons & T_DPAD_UP, 0);
        assert_eq!(buttons & T_DPAD_DOWN, 0);
        // The right stick's Y is the page's -y: pushed up (-1) is +32767.
        assert_eq!(i16_at(15), 32767.0);
        // The page's pad coordinates: x / 65536 + 0.5, and -y / 65536 + 0.5.
        let (x, y) = (i16_at(23) / 65536.0 + 0.5, -i16_at(25) / 65536.0 + 0.5);
        assert!((x - 1.0).abs() < 0.001 && y.abs() < 0.001, "{x} {y}");
        // 1 g up: the page's y is the third axis.
        assert_eq!(i16_at(37), 16384.0);
        // A pad with nothing touched at rest, and a gone one is at rest.
        let rest = triton_state(&PadState::default(), 1, 0);
        assert!(rest[2..6].iter().all(|b| *b == 0));
        let gone = triton_state(
            &PadState {
                gone: true,
                ..triton_known_state()
            },
            1,
            0,
        );
        assert_eq!(&gone[2..], &rest[2..]);
    }

    #[test]
    fn the_2026_controller_reports_a_battery_and_a_connection() {
        let mut sc = triton();
        let first = sc.report(&PadState::default(), Instant::now());
        // The state, the battery (full, discharging) and the wireless connect.
        assert_eq!(first.len(), 3);
        assert_eq!(first[0][0], 0x45);
        assert_eq!(first[1].len(), 15);
        assert_eq!(&first[1][..3], &[0x43, 1, 100]);
        assert_eq!(first[2], [0x79, 2]);
        let next = sc.report(&PadState::default(), Instant::now());
        assert_eq!(next.len(), 1);
        // The sequence counts and the clock moves.
        assert_eq!(next[0][1], first[0][1].wrapping_add(1));
        let level = triton_battery(&PadState {
            bat: Some(0.34),
            ..PadState::default()
        });
        assert_eq!(&level[..3], &[0x43, 1, 34]);
    }

    #[test]
    fn the_2026_serial_and_attributes_are_answered() {
        let mut sc = triton();
        let (_, reply) = triton_exchange(&mut sc, &[GET_STRING_ATTRIBUTE, 21, 1]);
        assert_eq!(reply.len(), 64);
        assert_eq!(&reply[..4], &[TRITON_FEATURE, GET_STRING_ATTRIBUTE, 21, 1]);
        assert_eq!(&reply[4..17], b"FXA0123456789");
        assert_eq!(&reply[17..25], &[0; 8]);
        // The board's serial asks for attribute 0 and is answered as such.
        let (_, reply) = triton_exchange(&mut sc, &[GET_STRING_ATTRIBUTE, 21, 0]);
        assert_eq!(&reply[1..4], &[GET_STRING_ATTRIBUTE, 21, 0]);
        let (_, reply) = triton_exchange(&mut sc, &[GET_ATTRIBUTES, 0]);
        assert_eq!(&reply[1..3], &[GET_ATTRIBUTES, 30]);
        // The product, and the interval (4032 us).
        assert_eq!(&reply[8..13], &[1, 0x03, 0x13, 0, 0]);
        assert_eq!(&reply[28..33], &[11, 0xc0, 0x0f, 0, 0]);
        // Lizard mode off and the IMU on (SDL's two settings) are kept.
        let (events, _) = triton_exchange(&mut sc, &[SET_SETTINGS, 3, 9, 0, 0]);
        assert!(events.is_empty());
        let (_, reply) = triton_exchange(&mut sc, &[GET_SETTINGS, 0]);
        assert_eq!(&reply[1..6], &[GET_SETTINGS, 3, 9, 0, 0]);
        // Not another report, nor the id left off the set.
        assert_eq!(sc.get_report(0, FEATURE_REPORT), Err(libc::EINVAL));
        assert!(
            sc.set_report(TRITON_FEATURE, FEATURE_REPORT, &[GET_ATTRIBUTES, 0])
                .is_ok()
        );
        assert_eq!(
            sc.get_report(TRITON_FEATURE, FEATURE_REPORT).unwrap()[1],
            GET_ATTRIBUTES
        );
        assert_eq!(
            sc.set_report(TRITON_FEATURE, FEATURE_REPORT, &[]),
            Err(libc::EINVAL)
        );
        // Each kind of command was seen once.
        assert!(sc.seen.contains(&GET_STRING_ATTRIBUTE) && sc.seen.contains(&SET_SETTINGS));
    }

    #[test]
    fn the_2026_rumble_report_becomes_an_event_on_change() {
        let mut sc = triton();
        // SDL's: id 0x80, type 0, intensity 0, left speed, gain, right speed, gain.
        let mut report = vec![TRITON_RUMBLE, 0, 0, 0];
        report.extend_from_slice(&65535u16.to_le_bytes());
        report.push(0);
        report.extend_from_slice(&32768u16.to_le_bytes());
        report.push(0);
        let events = sc.output(OUTPUT_REPORT, &report);
        match events[..] {
            [PadEvent::Rumble(r)] => {
                assert_eq!(r.slot, 2);
                assert_eq!(r.lo, 1.0);
                assert!((r.hi - 0.5).abs() < 0.001);
                assert_eq!(r.ms, ENDLESS_MS);
            }
            ref other => panic!("{other:?}"),
        }
        // The same again (SDL resends every 40 ms): nothing; zeros stop it.
        assert!(sc.output(OUTPUT_REPORT, &report).is_empty());
        let stop = [TRITON_RUMBLE, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        match sc.output(OUTPUT_REPORT, &stop)[..] {
            [PadEvent::Rumble(r)] => assert_eq!((r.lo, r.hi, r.ms), (0.0, 0.0, 0)),
            ref other => panic!("{other:?}"),
        }
        // Too short, or another kind of report, or another controller's.
        assert!(sc.output(OUTPUT_REPORT, &[TRITON_RUMBLE, 1]).is_empty());
        assert!(sc.output(FEATURE_REPORT, &report).is_empty());
        assert!(ble().output(OUTPUT_REPORT, &report).is_empty());
    }

    #[test]
    fn the_2026_pulse_report_becomes_haptic_events() {
        let mut sc = triton();
        // Left (1), 400 us on, 100 off, three times.
        let report = [TRITON_PULSE, 1, 0x90, 0x01, 100, 0, 3, 0];
        assert_eq!(
            sc.output(OUTPUT_REPORT, &report),
            [PadEvent::Haptic(Haptic {
                slot: 2,
                side: Side::Left,
                amp: 1.0,
                on_us: 400,
                off_us: 100,
                count: 3
            })]
        );
        let mut right = report;
        right[1] = 0;
        assert!(matches!(
            sc.output(OUTPUT_REPORT, &right)[..],
            [PadEvent::Haptic(Haptic {
                side: Side::Right,
                ..
            })]
        ));
        right[1] = 2;
        assert_eq!(sc.output(OUTPUT_REPORT, &right).len(), 2);
        assert!(sc.output(OUTPUT_REPORT, &report[..5]).is_empty());
    }
}
