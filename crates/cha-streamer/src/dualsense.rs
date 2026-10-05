//! A virtual DualSense (wired USB, `054c:0ce6`) over uhid.
//!
//! The host kernel's `hid-playstation` binds it, as it would a real one, and
//! makes the gamepad, touchpad and motion sensor input devices; the app also
//! gets the `hidraw` node (SDL's HIDAPI driver, Proton's winebus, Steam read
//! the report themselves).
//!
//! - **Descriptor:** the DualSense's USB report descriptor, from
//!   <https://github.com/nondebug/dualsense> (`report-descriptor-usb.txt`) as
//!   the inputtino project (MIT, games-on-whales) checked it with `hid-decode`.
//! - **Input report 0x01** (64 bytes) from a [`PadState`]: sticks, triggers,
//!   hat and buttons, the sensor timestamp (0.33 us units), gyroscope and
//!   accelerometer, two touch points, the battery. Layout after the kernel's
//!   `drivers/hid/hid-playstation.c` (GPL-2.0; read for the protocol, nothing
//!   copied) and SDL3's `SDL_hidapi_ps5.c` (zlib).
//! - **Feature reports** the kernel and SDL read: calibration 0x05, pairing
//!   info 0x09 (the MAC address) and firmware info 0x20. The calibration is
//!   ours: the gyroscope's unit is 1/16 degree/s and the accelerometer's
//!   1/8192 g, with no bias, so what the page sends maps 1:1 (both `hid-playstation`
//!   and SDL accept it as valid). Every other feature report the descriptor
//!   declares answers zeros; an id it doesn't, an error.
//! - **Output report 0x02** (rumble, lightbar, player LEDs, adaptive
//!   triggers) becomes [`PadEvent`]s; what repeats unchanged is not repeated.
//!   The Bluetooth report (0x31) isn't handled: the device is on USB.

use std::time::{Duration, Instant};

use crate::gamepad::{ENDLESS_MS, Led, PadEvent, PadState, Players, Rumble, Side, Trigger};
use crate::uhid::{FEATURE_REPORT, OUTPUT_REPORT, Protocol};

pub const VENDOR: u16 = 0x054c;
pub const PRODUCT: u16 = 0x0ce6;
/// As the kernel names a USB device: its manufacturer and product strings.
pub const NAME: &str = "Sony Interactive Entertainment DualSense Wireless Controller";
/// A real DualSense's `bcdDevice`.
pub const VERSION: u16 = 0x0100;
/// Real ones report faster; clients only need a steady stream.
const PERIOD: Duration = Duration::from_millis(8);

const INPUT_ID: u8 = 0x01;
const OUTPUT_ID: u8 = 0x02;
const CALIBRATION_ID: u8 = 0x05;
const PAIRING_ID: u8 = 0x09;
const FIRMWARE_ID: u8 = 0x20;

pub const INPUT_SIZE: usize = 64;
const CALIBRATION_SIZE: usize = 41;
const PAIRING_SIZE: usize = 20;
const FIRMWARE_SIZE: usize = 64;
/// The output report without its id (`dualsense_output_report_common`).
const OUTPUT_COMMON: usize = 47;

const TOUCH_W: f32 = 1919.0;
const TOUCH_H: f32 = 1079.0;
const GRAVITY: f32 = 9.80665;
/// Gyroscope: 1/16 degree/s per unit (+-2048 degrees/s in an i16).
const GYRO_PER_RAD_S: f32 = 16.0 * 180.0 / std::f32::consts::PI;
/// Accelerometer: 8192 units per g.
const ACCEL_PER_MS2: f32 = 8192.0 / GRAVITY;

/// Feature report ids and sizes (with the id) the descriptor declares.
const FEATURES: [(u8, usize); 20] = [
    (0x05, 41),
    (0x08, 48),
    (0x09, 20),
    (0x0a, 27),
    (0x20, 64),
    (0x21, 5),
    (0x22, 64),
    (0x80, 64),
    (0x81, 64),
    (0x82, 10),
    (0x83, 64),
    (0x84, 64),
    (0x85, 3),
    (0xa0, 2),
    (0xe0, 64),
    (0xf0, 64),
    (0xf1, 64),
    (0xf2, 16),
    (0xf4, 64),
    (0xf5, 4),
];

/// The USB report descriptor (273 bytes).
#[rustfmt::skip]
pub const DESCRIPTOR: [u8; 273] = [
    0x05, 0x01,  // Usage Page (Generic Desktop Ctrls)
    0x09, 0x05,  // Usage (Game Pad)
    0xA1, 0x01,  // Collection (Application)
    0x85, 0x01,  //   Report ID (1)
    0x09, 0x30,  //   Usage (X)
    0x09, 0x31,  //   Usage (Y)
    0x09, 0x32,  //   Usage (Z)
    0x09, 0x35,  //   Usage (Rz)
    0x09, 0x33,  //   Usage (Rx)
    0x09, 0x34,  //   Usage (Ry)
    0x15, 0x00,  //   Logical Minimum (0)
    0x26, 0xFF, 0x00,  //   Logical Maximum (255)
    0x75, 0x08,  //   Report Size (8)
    0x95, 0x06,  //   Report Count (6)
    0x81, 0x02,  //   Input (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position)
    0x06, 0x00, 0xFF,  //   Usage Page (Vendor Defined 0xFF00)
    0x09, 0x20,  //   Usage (0x20)
    0x95, 0x01,  //   Report Count (1)
    0x81, 0x02,  //   Input (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position)
    0x05, 0x01,  //   Usage Page (Generic Desktop Ctrls)
    0x09, 0x39,  //   Usage (Hat switch)
    0x15, 0x00,  //   Logical Minimum (0)
    0x25, 0x07,  //   Logical Maximum (7)
    0x35, 0x00,  //   Physical Minimum (0)
    0x46, 0x3B, 0x01,  //   Physical Maximum (315)
    0x65, 0x14,  //   Unit (System: English Rotation, Length: Centimeter)
    0x75, 0x04,  //   Report Size (4)
    0x95, 0x01,  //   Report Count (1)
    0x81, 0x42,  //   Input (Data,Var,Abs,No Wrap,Linear,Preferred State,Null State)
    0x65, 0x00,  //   Unit (None)
    0x05, 0x09,  //   Usage Page (Button)
    0x19, 0x01,  //   Usage Minimum (0x01)
    0x29, 0x0F,  //   Usage Maximum (0x0F)
    0x15, 0x00,  //   Logical Minimum (0)
    0x25, 0x01,  //   Logical Maximum (1)
    0x75, 0x01,  //   Report Size (1)
    0x95, 0x0F,  //   Report Count (15)
    0x81, 0x02,  //   Input (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position)
    0x06, 0x00, 0xFF,  //   Usage Page (Vendor Defined 0xFF00)
    0x09, 0x21,  //   Usage (0x21)
    0x95, 0x0D,  //   Report Count (13)
    0x81, 0x02,  //   Input (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position)
    0x06, 0x00, 0xFF,  //   Usage Page (Vendor Defined 0xFF00)
    0x09, 0x22,  //   Usage (0x22)
    0x15, 0x00,  //   Logical Minimum (0)
    0x26, 0xFF, 0x00,  //   Logical Maximum (255)
    0x75, 0x08,  //   Report Size (8)
    0x95, 0x34,  //   Report Count (52)
    0x81, 0x02,  //   Input (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position)
    0x85, 0x02,  //   Report ID (2)
    0x09, 0x23,  //   Usage (0x23)
    0x95, 0x2F,  //   Report Count (47)
    0x91, 0x02,  //   Output (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x05,  //   Report ID (5)
    0x09, 0x33,  //   Usage (0x33)
    0x95, 0x28,  //   Report Count (40)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x08,  //   Report ID (8)
    0x09, 0x34,  //   Usage (0x34)
    0x95, 0x2F,  //   Report Count (47)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x09,  //   Report ID (9)
    0x09, 0x24,  //   Usage (0x24)
    0x95, 0x13,  //   Report Count (19)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x0A,  //   Report ID (10)
    0x09, 0x25,  //   Usage (0x25)
    0x95, 0x1A,  //   Report Count (26)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x20,  //   Report ID (32)
    0x09, 0x26,  //   Usage (0x26)
    0x95, 0x3F,  //   Report Count (63)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x21,  //   Report ID (33)
    0x09, 0x27,  //   Usage (0x27)
    0x95, 0x04,  //   Report Count (4)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x22,  //   Report ID (34)
    0x09, 0x40,  //   Usage (0x40)
    0x95, 0x3F,  //   Report Count (63)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x80,  //   Report ID (-128)
    0x09, 0x28,  //   Usage (0x28)
    0x95, 0x3F,  //   Report Count (63)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x81,  //   Report ID (-127)
    0x09, 0x29,  //   Usage (0x29)
    0x95, 0x3F,  //   Report Count (63)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x82,  //   Report ID (-126)
    0x09, 0x2A,  //   Usage (0x2A)
    0x95, 0x09,  //   Report Count (9)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x83,  //   Report ID (-125)
    0x09, 0x2B,  //   Usage (0x2B)
    0x95, 0x3F,  //   Report Count (63)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x84,  //   Report ID (-124)
    0x09, 0x2C,  //   Usage (0x2C)
    0x95, 0x3F,  //   Report Count (63)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0x85,  //   Report ID (-123)
    0x09, 0x2D,  //   Usage (0x2D)
    0x95, 0x02,  //   Report Count (2)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0xA0,  //   Report ID (-96)
    0x09, 0x2E,  //   Usage (0x2E)
    0x95, 0x01,  //   Report Count (1)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0xE0,  //   Report ID (-32)
    0x09, 0x2F,  //   Usage (0x2F)
    0x95, 0x3F,  //   Report Count (63)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0xF0,  //   Report ID (-16)
    0x09, 0x30,  //   Usage (0x30)
    0x95, 0x3F,  //   Report Count (63)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0xF1,  //   Report ID (-15)
    0x09, 0x31,  //   Usage (0x31)
    0x95, 0x3F,  //   Report Count (63)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0xF2,  //   Report ID (-14)
    0x09, 0x32,  //   Usage (0x32)
    0x95, 0x0F,  //   Report Count (15)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0xF4,  //   Report ID (-12)
    0x09, 0x35,  //   Usage (0x35)
    0x95, 0x3F,  //   Report Count (63)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0x85, 0xF5,  //   Report ID (-11)
    0x09, 0x36,  //   Usage (0x36)
    0x95, 0x03,  //   Report Count (3)
    0xB1, 0x02,  //   Feature (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position,Non-volatile)
    0xC0,  // End Collection
];

/// A finger on the touchpad: the contact id the report carries, which counts
/// up with each new touch.
#[derive(Clone, Copy, Default)]
struct Finger {
    id: u8,
    down: bool,
}

pub struct DualSense {
    slot: usize,
    mac: [u8; 6],
    start: Instant,
    seq: u8,
    fingers: [Finger; 2],
    // What the last output report asked for, to pass on only changes.
    rumble: (u8, u8),
    led: Option<(u8, u8, u8)>,
    players: Option<u8>,
    triggers: [Option<[u8; 11]>; 2],
}

impl DualSense {
    /// `mac` as printed (`aa:bb:..`, most significant first).
    pub fn new(slot: usize, mac: [u8; 6]) -> Self {
        Self {
            slot,
            mac,
            start: Instant::now(),
            seq: 0,
            fingers: [Finger::default(); 2],
            rumble: (0, 0),
            led: None,
            players: None,
            triggers: [None; 2],
        }
    }

    /// Output report 0x02's body, without the id.
    fn parse_output(&mut self, c: &[u8]) -> Vec<PadEvent> {
        let (flag0, flag1, flag2) = (c[0], c[1], c[38]);
        let mut out = Vec::new();
        // Rumble: the classic way (flag 0 bit 0), or the newer firmware's
        // (flag 2 bit 2). Motors are 0..255; the right one is the weak one.
        if flag0 & 0x01 != 0 || flag2 & 0x04 != 0 {
            let (right, left) = (c[2], c[3]);
            if (left, right) != self.rumble {
                self.rumble = (left, right);
                out.push(PadEvent::Rumble(Rumble {
                    slot: self.slot,
                    lo: f32::from(left) / 255.0,
                    hi: f32::from(right) / 255.0,
                    ms: if left == 0 && right == 0 {
                        0
                    } else {
                        ENDLESS_MS
                    },
                }));
            }
        }
        // Adaptive triggers: type and ten parameters each, right then left.
        for (i, (flag, side)) in [(0x04, Side::Right), (0x08, Side::Left)]
            .into_iter()
            .enumerate()
        {
            if flag0 & flag != 0 {
                let at = 10 + i * 11;
                let effect: [u8; 11] = c[at..at + 11].try_into().expect("11 bytes");
                if self.triggers[i] != Some(effect) {
                    self.triggers[i] = Some(effect);
                    out.push(PadEvent::Trigger(Trigger {
                        slot: self.slot,
                        side,
                        effect,
                    }));
                }
            }
        }
        if flag1 & 0x04 != 0 {
            let rgb = (c[44], c[45], c[46]);
            if self.led != Some(rgb) {
                self.led = Some(rgb);
                out.push(PadEvent::Led(Led {
                    slot: self.slot,
                    r: rgb.0,
                    g: rgb.1,
                    b: rgb.2,
                }));
            }
        }
        if flag1 & 0x10 != 0 {
            let mask = c[43] & 0x1f;
            if self.players != Some(mask) {
                self.players = Some(mask);
                out.push(PadEvent::Players(Players {
                    slot: self.slot,
                    mask,
                }));
            }
        }
        out
    }
}

impl Protocol for DualSense {
    fn report(&mut self, state: &PadState, now: Instant) -> Vec<Vec<u8>> {
        let seq = self.seq;
        self.seq = seq.wrapping_add(1);
        // 0.33 us units; wraps as the report's 32 bits do.
        let ticks = (now.saturating_duration_since(self.start).as_nanos() / 333) as u32;
        vec![input_report(state, seq, ticks, &mut self.fingers).to_vec()]
    }

    fn period(&self) -> Duration {
        PERIOD
    }

    fn get_report(&mut self, number: u8, kind: u8) -> Result<Vec<u8>, i32> {
        if kind != FEATURE_REPORT {
            return Err(libc::EINVAL);
        }
        match number {
            CALIBRATION_ID => Ok(calibration_report().to_vec()),
            PAIRING_ID => Ok(pairing_report(&self.mac).to_vec()),
            FIRMWARE_ID => Ok(firmware_report().to_vec()),
            n => match FEATURES.iter().find(|(id, _)| *id == n) {
                Some(&(id, size)) => {
                    let mut zeros = vec![0; size];
                    zeros[0] = id;
                    Ok(zeros)
                }
                None => Err(libc::EINVAL),
            },
        }
    }

    fn set_report(&mut self, number: u8, kind: u8, _data: &[u8]) -> Result<Vec<PadEvent>, i32> {
        // The test and pairing commands of the feature reports: accepted, ignored.
        if kind == FEATURE_REPORT && FEATURES.iter().any(|(id, _)| *id == number) {
            Ok(Vec::new())
        } else {
            Err(libc::EINVAL)
        }
    }

    fn output(&mut self, kind: u8, data: &[u8]) -> Vec<PadEvent> {
        if kind == OUTPUT_REPORT && data.len() > OUTPUT_COMMON && data[0] == OUTPUT_ID {
            self.parse_output(&data[1..=OUTPUT_COMMON])
        } else {
            Vec::new()
        }
    }
}

/// The hat's value for the d-pad (b12 up, b13 down, b14 left, b15 right):
/// 0 north, clockwise to 7, and 8 for none.
fn hat(up: bool, down: bool, left: bool, right: bool) -> u8 {
    match (up, down, left, right) {
        (true, false, false, false) => 0,
        (true, false, false, true) => 1,
        (false, false, false, true) => 2,
        (false, true, false, true) => 3,
        (false, true, false, false) => 4,
        (false, true, true, false) => 5,
        (false, false, true, false) => 6,
        (true, false, true, false) => 7,
        _ => 8,
    }
}

fn touch_point(finger: Finger, at: Option<(f32, f32)>) -> [u8; 4] {
    match at {
        Some((x, y)) => {
            let x = (x.clamp(0.0, 1.0) * TOUCH_W).round() as u16;
            let y = (y.clamp(0.0, 1.0) * TOUCH_H).round() as u16;
            [
                finger.id,
                x as u8,
                ((x >> 8) & 0x0f) as u8 | ((y & 0x0f) << 4) as u8,
                (y >> 4) as u8,
            ]
        }
        // The high bit marks a contact inactive; the rest is ignored.
        None => [finger.id | 0x80, 0, 0, 0],
    }
}

/// The 64-byte input report for a state. `seq` and `ticks` (the sensor
/// timestamp) come from the device; `fingers` keeps touch ids across calls.
fn input_report(
    state: &PadState,
    seq: u8,
    ticks: u32,
    fingers: &mut [Finger; 2],
) -> [u8; INPUT_SIZE] {
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
    let stick = |v: f32| ((v.clamp(-1.0, 1.0) + 1.0) * 127.5).round() as u8;
    let trigger = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let unit = |v: f32, per: f32| (v * per).round().clamp(-32768.0, 32767.0) as i16;

    let mut r = [0u8; INPUT_SIZE];
    r[0] = INPUT_ID;
    r[1..7].copy_from_slice(&[
        stick(a(0)),
        stick(a(1)),
        stick(a(2)),
        stick(a(3)),
        trigger(b(6)),
        trigger(b(7)),
    ]);
    r[7] = seq;
    // Square, cross, circle, triangle are the standard layout's X, A, B, Y.
    r[8] = hat(on(12), on(13), on(14), on(15))
        | u8::from(on(2)) << 4
        | u8::from(on(0)) << 5
        | u8::from(on(1)) << 6
        | u8::from(on(3)) << 7;
    r[9] = u8::from(on(4))
        | u8::from(on(5)) << 1
        | u8::from(on(6)) << 2
        | u8::from(on(7)) << 3
        | u8::from(on(8)) << 4
        | u8::from(on(9)) << 5
        | u8::from(on(10)) << 6
        | u8::from(on(11)) << 7;
    r[10] = u8::from(on(16)) | u8::from(on(17) || on(18)) << 1 | u8::from(on(23)) << 2;
    if live {
        for (i, v) in state.gyro.unwrap_or_default().iter().enumerate() {
            r[16 + 2 * i..18 + 2 * i].copy_from_slice(&unit(*v, GYRO_PER_RAD_S).to_le_bytes());
        }
        for (i, v) in state.accel.unwrap_or_default().iter().enumerate() {
            r[22 + 2 * i..24 + 2 * i].copy_from_slice(&unit(*v, ACCEL_PER_MS2).to_le_bytes());
        }
    }
    r[28..32].copy_from_slice(&ticks.to_le_bytes());
    for (k, finger) in fingers.iter_mut().enumerate() {
        let at = state
            .touch
            .iter()
            .find(|t| t.id as usize == k && t.down && live)
            .map(|t| (t.x, t.y));
        if at.is_some() && !finger.down {
            finger.id = (finger.id + 1) & 0x7f;
        }
        finger.down = at.is_some();
        r[33 + 4 * k..37 + 4 * k].copy_from_slice(&touch_point(*finger, at));
    }
    // Battery: the level in tenths, 0 (discharging) in the high nibble. With no
    // level from the page, a full battery on a cable (2: charged).
    r[53] = match state.bat {
        Some(level) => (level.clamp(0.0, 1.0) * 10.0).round() as u8,
        None => 0x2a,
    };
    r
}

/// Feature report 0x05: sensor calibration. No bias, and the same range on
/// both sides of each axis (+-8192 at 512 degrees/s, and +-1 g): the
/// gyroscope's unit becomes 1/16 degree/s and the accelerometer's 1/8192 g.
fn calibration_report() -> [u8; CALIBRATION_SIZE] {
    let mut r = [0u8; CALIBRATION_SIZE];
    r[0] = CALIBRATION_ID;
    let put = |r: &mut [u8], at: usize, v: i16| r[at..at + 2].copy_from_slice(&v.to_le_bytes());
    // Gyro biases (pitch, yaw, roll) at 1..7 stay 0; then plus and minus of each.
    for axis in 0..3 {
        put(&mut r, 7 + 4 * axis, 8192);
        put(&mut r, 9 + 4 * axis, -8192);
    }
    // The speed at that range, plus and minus.
    put(&mut r, 19, 512);
    put(&mut r, 21, 512);
    for axis in 0..3 {
        put(&mut r, 23 + 4 * axis, 8192);
        put(&mut r, 25 + 4 * axis, -8192);
    }
    r
}

/// Feature report 0x09: the pairing info, whose bytes 1..7 are the MAC address
/// in little-endian order. The rest is a real controller's, as inputtino has it.
fn pairing_report(mac: &[u8; 6]) -> [u8; PAIRING_SIZE] {
    let mut r = [
        PAIRING_ID, 0, 0, 0, 0, 0, 0, 0x08, 0x25, 0x00, 0x1e, 0x00, 0xee, 0x74, 0xd0, 0xbc, 0, 0,
        0, 0,
    ];
    for (to, from) in r[1..7].iter_mut().zip(mac.iter().rev()) {
        *to = *from;
    }
    r
}

/// Feature report 0x20: the firmware info, a real controller's as inputtino
/// has it (build date 19 June 2023, hardware 0x01000208, firmware 0x01000036),
/// but with the update version (bytes 44..46) at 0x0224: the version the
/// kernel and SDL take as having the current rumble.
fn firmware_report() -> [u8; FIRMWARE_SIZE] {
    let mut r = [0u8; FIRMWARE_SIZE];
    let head = b"\x20Jun 19 202314:47:34\x03\x00\x44\x00\x08\x02\x00\x01\x36\x00\x00\x01\xc1\xc8";
    r[..head.len()].copy_from_slice(head);
    r[44..46].copy_from_slice(&0x0224u16.to_le_bytes());
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamepad::Touch;

    fn state() -> PadState {
        let mut b = vec![0.0; 24];
        b[0] = 1.0; // A: cross
        b[7] = 1.0; // right trigger
        b[12] = 1.0; // d-pad up
        b[15] = 1.0; // d-pad right
        b[16] = 1.0; // guide: PS
        b[17] = 1.0; // touchpad click
        PadState {
            i: 0,
            b,
            a: vec![-1.0, 1.0, 0.0, 0.5],
            gyro: Some([1.0, 0.0, 0.0]),
            accel: Some([0.0, GRAVITY, 0.0]),
            touch: vec![Touch {
                id: 0,
                x: 0.5,
                y: 0.5,
                down: true,
            }],
            ..PadState::default()
        }
    }

    #[test]
    fn the_descriptor_is_the_real_ones_size_and_declares_our_reports() {
        assert_eq!(DESCRIPTOR.len(), 273);
        // Report id 1 (input, 64 bytes with it), 2 (output), 5, 9 and 0x20.
        assert!(DESCRIPTOR.windows(2).any(|w| w == [0x85, 0x01]));
        assert!(DESCRIPTOR.windows(2).any(|w| w == [0x85, 0x02]));
        for (id, size) in FEATURES {
            let at = DESCRIPTOR
                .windows(2)
                .position(|w| w == [0x85, id])
                .unwrap_or_else(|| panic!("report {id:#x}"));
            // `Report Count (n)` follows the usage: 0x85 id, 0x09 u, 0x95 n.
            assert_eq!(DESCRIPTOR[at + 4], 0x95);
            assert_eq!(usize::from(DESCRIPTOR[at + 5]) + 1, size, "report {id:#x}");
        }
    }

    #[test]
    fn the_input_report_for_a_known_state_is_byte_exact() {
        let mut fingers = [Finger::default(); 2];
        let r = input_report(&state(), 7, 0x0102_0304, &mut fingers);
        let mut want = [0u8; 64];
        want[0] = 0x01;
        // Sticks: left -1, 1; right 0 (128), 0.5; triggers 0 and full.
        want[1..7].copy_from_slice(&[0, 255, 128, 191, 0, 255]);
        want[7] = 7;
        // North-east hat, cross; R2's button; PS and the touchpad click.
        want[8] = 0x01 | 0x20;
        want[9] = 0x08;
        want[10] = 0x03;
        // 1 rad/s is 917 units of 1/16 degree/s; 1 g is 8192.
        want[16..18].copy_from_slice(&917i16.to_le_bytes());
        want[24..26].copy_from_slice(&8192i16.to_le_bytes());
        want[28..32].copy_from_slice(&[0x04, 0x03, 0x02, 0x01]);
        // Finger 0 at (960, 540), contact id 1; finger 1 up.
        want[33..37].copy_from_slice(&[1, 0xc0, 0xc3, 0x21]);
        want[37..41].copy_from_slice(&[0x80, 0, 0, 0]);
        want[53] = 0x2a;
        assert_eq!(r, want);
        // The finger stays the same contact while down and is a new one next time.
        let mut up = state();
        up.touch.clear();
        let r = input_report(&up, 8, 0, &mut fingers);
        assert_eq!(&r[33..37], &[0x81, 0, 0, 0]);
        let r = input_report(&state(), 9, 0, &mut fingers);
        assert_eq!(r[33], 2);
    }

    #[test]
    fn rest_is_neutral_and_gone_is_rest() {
        let rest = input_report(&PadState::default(), 0, 0, &mut [Finger::default(); 2]);
        assert_eq!(&rest[1..7], &[128, 128, 128, 128, 0, 0]);
        assert_eq!(rest[8], 8);
        let gone = PadState {
            gone: true,
            ..state()
        };
        let gone = input_report(&gone, 0, 0, &mut [Finger::default(); 2]);
        assert_eq!(&gone[1..11], &rest[1..11]);
        assert_eq!(&gone[16..28], &[0; 12]);
        assert_eq!(gone[33], 0x80);
        // A level from the page: tenths, discharging.
        let low = PadState {
            bat: Some(0.34),
            ..PadState::default()
        };
        assert_eq!(input_report(&low, 0, 0, &mut [Finger::default(); 2])[53], 3);
    }

    #[test]
    fn the_hat_covers_the_eight_directions() {
        assert_eq!(hat(true, false, false, false), 0);
        assert_eq!(hat(false, true, false, true), 3);
        assert_eq!(hat(true, false, true, false), 7);
        assert_eq!(hat(false, false, false, false), 8);
        assert_eq!(hat(true, true, false, false), 8);
    }

    /// What `hid-playstation` and SDL do with the calibration.
    #[test]
    fn the_calibration_maps_our_units_to_theirs_one_to_one() {
        let r = calibration_report();
        assert_eq!((r[0], r.len()), (0x05, 41));
        let s = |at: usize| i32::from(i16::from_le_bytes([r[at], r[at + 1]]));
        // The kernel's gyro: speed_2x * 1024 over the span, applied to a raw value.
        let speed_2x = s(19) + s(21);
        let span = (s(7) - s(1)).abs() + (s(9) - s(1)).abs();
        let calibrated = speed_2x * 1024 * 917 / span;
        // In 1/1024 degree/s: 917/16 degrees/s.
        assert_eq!(calibrated, 917 * 64);
        // SDL's sensitivity must be near 64 to take it as valid.
        assert_eq!(speed_2x * 1024 / (s(7) - s(9)), 64);
        // Acceleration: no bias, one unit per unit.
        let range = s(23) - s(25);
        assert_eq!(s(23) - range / 2, 0);
        assert_eq!(2 * 8192 / range, 1);
    }

    #[test]
    fn feature_reports_answer_as_the_kernel_expects() {
        let mut ds = DualSense::new(0, [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
        assert_eq!(ds.get_report(0x05, FEATURE_REPORT).unwrap().len(), 41);
        let pairing = ds.get_report(0x09, FEATURE_REPORT).unwrap();
        assert_eq!(pairing.len(), 20);
        assert_eq!(pairing[0], 0x09);
        // Little-endian: the printed address reversed.
        assert_eq!(&pairing[1..7], &[0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x02]);
        let fw = ds.get_report(0x20, FEATURE_REPORT).unwrap();
        assert_eq!(fw.len(), 64);
        assert_eq!(fw[0], 0x20);
        assert_eq!(
            u32::from_le_bytes(fw[24..28].try_into().unwrap()),
            0x0100_0208
        );
        assert_eq!(u16::from_le_bytes([fw[44], fw[45]]), 0x0224);
        // Others the descriptor declares are zeros with their id; unknown ones fail.
        let other = ds.get_report(0x22, FEATURE_REPORT).unwrap();
        assert_eq!(
            (other[0], other.len(), other[1..].iter().all(|b| *b == 0)),
            (0x22, 64, true)
        );
        assert_eq!(ds.get_report(0x77, FEATURE_REPORT), Err(libc::EINVAL));
        assert_eq!(ds.get_report(0x05, OUTPUT_REPORT), Err(libc::EINVAL));
        assert!(
            ds.set_report(0x80, FEATURE_REPORT, &[0x80, 1])
                .unwrap()
                .is_empty()
        );
        assert_eq!(ds.set_report(0x77, FEATURE_REPORT, &[]), Err(libc::EINVAL));
    }

    #[test]
    fn output_reports_become_events_once() {
        let mut ds = DualSense::new(2, [0; 6]);
        let mut out = [0u8; 63];
        out[0] = OUTPUT_ID;
        // Compatible vibration, both triggers' effects; the lightbar and player LEDs.
        out[1] = 0x01 | 0x02 | 0x04 | 0x08;
        out[2] = 0x04 | 0x10;
        out[3] = 51; // right (weak) motor
        out[4] = 255; // left (strong) motor
        out[11..22].copy_from_slice(&[2, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        out[22..33].copy_from_slice(&[1; 11]);
        out[44] = 0x24;
        out[45..48].copy_from_slice(&[10, 20, 30]);
        let events = ds.output(OUTPUT_REPORT, &out);
        assert_eq!(
            events,
            [
                PadEvent::Rumble(Rumble {
                    slot: 2,
                    lo: 1.0,
                    hi: 0.2,
                    ms: ENDLESS_MS
                }),
                PadEvent::Trigger(Trigger {
                    slot: 2,
                    side: Side::Right,
                    effect: [2, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
                }),
                PadEvent::Trigger(Trigger {
                    slot: 2,
                    side: Side::Left,
                    effect: [1; 11]
                }),
                PadEvent::Led(Led {
                    slot: 2,
                    r: 10,
                    g: 20,
                    b: 30
                }),
                PadEvent::Players(Players { slot: 2, mask: 4 }),
            ]
        );
        // The same again says nothing; a change does; motors off is a stop.
        assert!(ds.output(OUTPUT_REPORT, &out).is_empty());
        out[45] = 11;
        assert_eq!(ds.output(OUTPUT_REPORT, &out).len(), 1);
        out[3] = 0;
        out[4] = 0;
        match ds.output(OUTPUT_REPORT, &out)[..] {
            [PadEvent::Rumble(r)] => assert_eq!((r.lo, r.hi, r.ms), (0.0, 0.0, 0)),
            ref other => panic!("{other:?}"),
        }
        // A report that only touches the LEDs leaves the motors alone; the
        // newer firmware's vibration flag works too; other reports and ids don't count.
        let mut leds = [0u8; 63];
        leds[0] = OUTPUT_ID;
        leds[2] = 0x04;
        leds[45] = 1;
        assert!(matches!(
            ds.output(OUTPUT_REPORT, &leds)[..],
            [PadEvent::Led(_)]
        ));
        let mut v2 = [0u8; 63];
        v2[0] = OUTPUT_ID;
        v2[39] = 0x04;
        v2[4] = 128;
        assert!(matches!(
            ds.output(OUTPUT_REPORT, &v2)[..],
            [PadEvent::Rumble(_)]
        ));
        assert!(ds.output(OUTPUT_REPORT, &[0x31; 78]).is_empty());
        assert!(ds.output(FEATURE_REPORT, &out).is_empty());
    }
}
