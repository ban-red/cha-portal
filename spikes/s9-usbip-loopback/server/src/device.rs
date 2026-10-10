//! A full-speed USB HID device model, Sans-IO: endpoint 0 requests in,
//! descriptors or an ack (or a stall) out.
//!
//! Identity: the 2026 Steam Controller wired (VID 0x28de, PID 0x1302). We have
//! no dump of a real one; the descriptors are our assumption, with the HID
//! report descriptor copied from `crates/cha-streamer/src/steam_controller.rs`
//! (`DESCRIPTOR_TRITON`, itself an assumption made to fit SDL's and the
//! kernel's sizes).

use std::collections::HashMap;

pub const VENDOR: u16 = 0x28de;
pub const PRODUCT: u16 = 0x1302;
pub const BCD_DEVICE: u16 = 0x0100;
pub const SERIAL: &str = "S9LOOP0001";
/// Interrupt IN and OUT endpoint addresses; 64-byte packets, bInterval 1.
pub const EP_IN: u8 = 0x81;
pub const EP_OUT: u8 = 0x01;
pub const EP_SIZE: u16 = 64;

/// The input report the bench produces: id 0x45 and 45 bytes.
pub const REPORT_ID_BENCH: u8 = 0x45;
pub const BENCH_REPORT_LEN: usize = 46;

/// `DESCRIPTOR_TRITON` of `cha-streamer`'s `steam_controller.rs`, byte for byte.
#[rustfmt::skip]
pub const REPORT_DESCRIPTOR: &[u8] = &[
    0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08,
    0x85, 0x01, 0x95, 0x3F, 0x09, 0x01, 0xB1, 0x02,
    0x85, 0x45, 0x95, 0x2D, 0x09, 0x01, 0x81, 0x02,
    0x85, 0x43, 0x95, 0x0E, 0x09, 0x01, 0x81, 0x02,
    0x85, 0x79, 0x95, 0x01, 0x09, 0x01, 0x81, 0x02,
    0x85, 0x80, 0x95, 0x09, 0x09, 0x01, 0x91, 0x02,
    0x85, 0x81, 0x95, 0x07, 0x09, 0x01, 0x91, 0x02,
    0xC0,
];

const MANUFACTURER: &str = "Valve Software";
const PRODUCT_NAME: &str = "Steam Controller";

// bmRequestType fields.
const TYPE_MASK: u8 = 0x60;
const TYPE_STANDARD: u8 = 0x00;
const TYPE_CLASS: u8 = 0x20;
const RECIPIENT_MASK: u8 = 0x1f;
const RECIP_DEVICE: u8 = 0;
const RECIP_INTERFACE: u8 = 1;
const RECIP_ENDPOINT: u8 = 2;

// Standard requests (USB 2.0 table 9-4).
const GET_STATUS: u8 = 0x00;
const CLEAR_FEATURE: u8 = 0x01;
const SET_FEATURE: u8 = 0x03;
const SET_ADDRESS: u8 = 0x05;
const GET_DESCRIPTOR: u8 = 0x06;
const GET_CONFIGURATION: u8 = 0x08;
const SET_CONFIGURATION: u8 = 0x09;
const GET_INTERFACE: u8 = 0x0a;
const SET_INTERFACE: u8 = 0x0b;

// HID 1.11 section 7.2.
const HID_GET_REPORT: u8 = 0x01;
const HID_GET_IDLE: u8 = 0x02;
const HID_GET_PROTOCOL: u8 = 0x03;
const HID_SET_REPORT: u8 = 0x09;
const HID_SET_IDLE: u8 = 0x0a;
const HID_SET_PROTOCOL: u8 = 0x0b;

// Descriptor types.
const DT_DEVICE: u8 = 1;
const DT_CONFIGURATION: u8 = 2;
const DT_STRING: u8 = 3;
const DT_INTERFACE: u8 = 4;
const DT_ENDPOINT: u8 = 5;
const DT_HID: u8 = 0x21;
const DT_REPORT: u8 = 0x22;

const FEATURE_ENDPOINT_HALT: u16 = 0;

/// Report types in `wValue`'s high byte of GET/SET_REPORT.
pub const REPORT_INPUT: u8 = 1;
pub const REPORT_OUTPUT: u8 = 2;
pub const REPORT_FEATURE: u8 = 3;
/// Length of feature report 1 (id and 63 bytes), as the report descriptor says.
const FEATURE_1_LEN: usize = 64;

pub fn device_descriptor() -> Vec<u8> {
    let mut d = vec![18, DT_DEVICE, 0x00, 0x02, 0, 0, 0, 64];
    d.extend_from_slice(&VENDOR.to_le_bytes());
    d.extend_from_slice(&PRODUCT.to_le_bytes());
    d.extend_from_slice(&BCD_DEVICE.to_le_bytes());
    d.extend_from_slice(&[1, 2, 3, 1]); // strings: manufacturer, product, serial; one configuration
    d
}

pub fn hid_descriptor() -> Vec<u8> {
    let mut d = vec![9, DT_HID, 0x11, 0x01, 0, 1, DT_REPORT];
    d.extend_from_slice(&(REPORT_DESCRIPTOR.len() as u16).to_le_bytes());
    d
}

/// Configuration 1: interface 0 (HID, no boot protocol), the HID descriptor,
/// then the interrupt IN and OUT endpoints.
pub fn configuration_descriptor() -> Vec<u8> {
    let mut d = vec![9, DT_CONFIGURATION, 0, 0, 1, 1, 0, 0xa0, 250];
    d.extend_from_slice(&[9, DT_INTERFACE, 0, 0, 2, 3, 0, 0, 0]);
    d.extend_from_slice(&hid_descriptor());
    for ep in [EP_IN, EP_OUT] {
        d.extend_from_slice(&[7, DT_ENDPOINT, ep, 3]); // interrupt
        d.extend_from_slice(&EP_SIZE.to_le_bytes());
        d.push(1);
    }
    let total = d.len() as u16;
    d[2..4].copy_from_slice(&total.to_le_bytes());
    d
}

pub fn string_descriptor(index: u8) -> Option<Vec<u8>> {
    let text = match index {
        0 => return Some(vec![4, DT_STRING, 0x09, 0x04]), // US English
        1 => MANUFACTURER,
        2 => PRODUCT_NAME,
        3 => SERIAL,
        _ => return None,
    };
    let units: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut d = vec![(2 + units.len()) as u8, DT_STRING];
    d.extend(units);
    Some(d)
}

/// What endpoint 0 answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// Success; the data stage for an IN request (empty for the rest).
    Data(Vec<u8>),
    /// STALL (the URB completes with -EPIPE).
    Stall,
}

#[derive(Debug, Default)]
pub struct Device {
    pub address: u8,
    pub configuration: u8,
    pub halted_in: bool,
    pub halted_out: bool,
    pub idle: u8,
    pub protocol: u8,
    /// Reports set by SET_REPORT, keyed by (type, id), as sent (id first).
    reports: HashMap<(u8, u8), Vec<u8>>,
}

fn truncate(mut v: Vec<u8>, w_length: u16) -> Control {
    v.truncate(usize::from(w_length));
    Control::Data(v)
}

impl Device {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last SET_REPORT body for a report, if any.
    pub fn stored_report(&self, kind: u8, id: u8) -> Option<&[u8]> {
        self.reports.get(&(kind, id)).map(Vec::as_slice)
    }

    /// Dispatch one control transfer: the 8-byte setup packet and, for OUT
    /// requests, the data stage.
    pub fn control(&mut self, setup: &[u8; 8], out: &[u8]) -> Control {
        let (bm, req) = (setup[0], setup[1]);
        let value = u16::from_le_bytes([setup[2], setup[3]]);
        let index = u16::from_le_bytes([setup[4], setup[5]]);
        let w_length = u16::from_le_bytes([setup[6], setup[7]]);
        let recipient = bm & RECIPIENT_MASK;
        match bm & TYPE_MASK {
            TYPE_STANDARD => self.standard(bm, req, value, index, w_length),
            TYPE_CLASS if recipient == RECIP_INTERFACE => self.hid(req, value, w_length, out),
            _ => Control::Stall,
        }
    }

    fn standard(&mut self, bm: u8, req: u8, value: u16, index: u16, w_length: u16) -> Control {
        let recipient = bm & RECIPIENT_MASK;
        let [value_lo, value_hi] = value.to_le_bytes();
        match (req, recipient) {
            (GET_DESCRIPTOR, RECIP_DEVICE) => match value_hi {
                DT_DEVICE => truncate(device_descriptor(), w_length),
                DT_CONFIGURATION if value_lo == 0 => truncate(configuration_descriptor(), w_length),
                DT_STRING => {
                    string_descriptor(value_lo).map_or(Control::Stall, |d| truncate(d, w_length))
                }
                // Device qualifier, other-speed configuration, BOS: a full-speed-only
                // device stalls these.
                _ => Control::Stall,
            },
            (GET_DESCRIPTOR, RECIP_INTERFACE) if index == 0 => match value_hi {
                DT_REPORT => truncate(REPORT_DESCRIPTOR.to_vec(), w_length),
                DT_HID => truncate(hid_descriptor(), w_length),
                _ => Control::Stall,
            },
            (SET_ADDRESS, RECIP_DEVICE) => {
                self.address = value_lo;
                Control::Data(Vec::new())
            }
            (SET_CONFIGURATION, RECIP_DEVICE) if value_lo <= 1 => {
                self.configuration = value_lo;
                // Entering a configuration resets the endpoints' toggles and halts.
                self.halted_in = false;
                self.halted_out = false;
                Control::Data(Vec::new())
            }
            (GET_CONFIGURATION, RECIP_DEVICE) => truncate(vec![self.configuration], w_length),
            (SET_INTERFACE, RECIP_INTERFACE) if index == 0 && value == 0 => {
                self.halted_in = false;
                self.halted_out = false;
                Control::Data(Vec::new())
            }
            (GET_INTERFACE, RECIP_INTERFACE) if index == 0 => truncate(vec![0], w_length),
            (GET_STATUS, RECIP_DEVICE | RECIP_INTERFACE) => truncate(vec![0, 0], w_length),
            (GET_STATUS, RECIP_ENDPOINT) => match self.halt_flag(index) {
                Some(halted) => truncate(vec![u8::from(*halted), 0], w_length),
                None => Control::Stall,
            },
            (CLEAR_FEATURE | SET_FEATURE, RECIP_ENDPOINT) if value == FEATURE_ENDPOINT_HALT => {
                match self.halt_flag(index) {
                    Some(halted) => {
                        *halted = req == SET_FEATURE;
                        Control::Data(Vec::new())
                    }
                    None => Control::Stall,
                }
            }
            // Remote wakeup and the like: nothing to do.
            (CLEAR_FEATURE | SET_FEATURE, RECIP_DEVICE | RECIP_INTERFACE) => {
                Control::Data(Vec::new())
            }
            _ => Control::Stall,
        }
    }

    fn halt_flag(&mut self, endpoint: u16) -> Option<&mut bool> {
        match endpoint as u8 {
            EP_IN => Some(&mut self.halted_in),
            EP_OUT => Some(&mut self.halted_out),
            _ => None,
        }
    }

    fn hid(&mut self, req: u8, value: u16, w_length: u16, out: &[u8]) -> Control {
        let [id, kind] = value.to_le_bytes();
        match req {
            HID_GET_REPORT => {
                let data = match self.reports.get(&(kind, id)) {
                    Some(stored) => stored.clone(),
                    None if kind == REPORT_FEATURE && id == 1 => {
                        let mut v = vec![0; FEATURE_1_LEN];
                        v[0] = 1;
                        v
                    }
                    None if kind == REPORT_INPUT && id == REPORT_ID_BENCH => {
                        let mut v = vec![0; BENCH_REPORT_LEN];
                        v[0] = id;
                        v
                    }
                    None => return Control::Stall,
                };
                truncate(data, w_length)
            }
            HID_SET_REPORT => {
                self.reports.insert((kind, id), out.to_vec());
                Control::Data(Vec::new())
            }
            HID_GET_IDLE => truncate(vec![self.idle], w_length),
            HID_SET_IDLE => {
                self.idle = (value >> 8) as u8;
                Control::Data(Vec::new())
            }
            HID_GET_PROTOCOL => truncate(vec![self.protocol], w_length),
            HID_SET_PROTOCOL => {
                self.protocol = value as u8;
                Control::Data(Vec::new())
            }
            _ => Control::Stall,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(bm: u8, req: u8, value: u16, index: u16, len: u16) -> [u8; 8] {
        let mut s = [bm, req, 0, 0, 0, 0, 0, 0];
        s[2..4].copy_from_slice(&value.to_le_bytes());
        s[4..6].copy_from_slice(&index.to_le_bytes());
        s[6..8].copy_from_slice(&len.to_le_bytes());
        s
    }

    fn data(c: Control) -> Vec<u8> {
        match c {
            Control::Data(d) => d,
            Control::Stall => panic!("stalled"),
        }
    }

    #[test]
    fn descriptor_lengths_are_consistent() {
        let dev = device_descriptor();
        assert_eq!(dev.len(), 18);
        assert_eq!(dev[0], 18);
        assert_eq!(u16::from_le_bytes([dev[8], dev[9]]), 0x28de);
        assert_eq!(u16::from_le_bytes([dev[10], dev[11]]), 0x1302);

        let cfg = configuration_descriptor();
        assert_eq!(cfg.len(), 41);
        assert_eq!(usize::from(u16::from_le_bytes([cfg[2], cfg[3]])), cfg.len());
        // Walk the descriptors: lengths add up to the total, types in order.
        let (mut at, mut types) = (0, Vec::new());
        while at < cfg.len() {
            types.push(cfg[at + 1]);
            at += usize::from(cfg[at]);
        }
        assert_eq!(at, cfg.len());
        assert_eq!(
            types,
            [
                DT_CONFIGURATION,
                DT_INTERFACE,
                DT_HID,
                DT_ENDPOINT,
                DT_ENDPOINT
            ]
        );
        // The HID descriptor names the report descriptor's length.
        let hid = hid_descriptor();
        assert_eq!(
            usize::from(u16::from_le_bytes([hid[7], hid[8]])),
            REPORT_DESCRIPTOR.len()
        );
        assert_eq!(&cfg[18..27], hid.as_slice());
        assert_eq!(cfg[4], 1); // one interface
        assert_eq!(cfg[13], 2); // two endpoints
        assert_eq!(cfg[14], 3); // HID class
    }

    #[test]
    fn report_descriptor_is_balanced() {
        // Every long-form-free item is prefix + size bytes; the collection closes.
        let (mut at, mut depth) = (0, 0i32);
        while at < REPORT_DESCRIPTOR.len() {
            let item = REPORT_DESCRIPTOR[at];
            match item & 0xfc {
                0xa0 => depth += 1,
                0xc0 => depth -= 1,
                _ => {}
            }
            at += 1 + match item & 3 {
                3 => 4,
                n => usize::from(n),
            };
        }
        assert_eq!(at, REPORT_DESCRIPTOR.len());
        assert_eq!(depth, 0);
    }

    #[test]
    fn strings_are_utf16() {
        assert_eq!(string_descriptor(0), Some(vec![4, 3, 9, 4]));
        let s = string_descriptor(3).unwrap();
        assert_eq!(usize::from(s[0]), s.len());
        assert_eq!(s.len(), 2 + 2 * SERIAL.len());
        assert_eq!(s[2], b'S');
        assert_eq!(s[3], 0);
        assert!(string_descriptor(9).is_none());
    }

    #[test]
    fn get_descriptor_truncates_to_wlength() {
        let mut d = Device::new();
        let first8 = data(d.control(&setup(0x80, GET_DESCRIPTOR, 0x0100, 0, 8), &[]));
        assert_eq!(first8, device_descriptor()[..8]);
        let cfg9 = data(d.control(&setup(0x80, GET_DESCRIPTOR, 0x0200, 0, 9), &[]));
        assert_eq!(cfg9.len(), 9);
        let cfg = data(d.control(&setup(0x80, GET_DESCRIPTOR, 0x0200, 0, 255), &[]));
        assert_eq!(cfg, configuration_descriptor());
        let rd = data(d.control(&setup(0x81, GET_DESCRIPTOR, 0x2200, 0, 4096), &[]));
        assert_eq!(rd, REPORT_DESCRIPTOR);
        assert_eq!(
            d.control(&setup(0x80, GET_DESCRIPTOR, 0x0600, 0, 10), &[]),
            Control::Stall
        );
    }

    #[test]
    fn config_requests_are_acknowledged() {
        let mut d = Device::new();
        assert_eq!(
            d.control(&setup(0x00, SET_ADDRESS, 5, 0, 0), &[]),
            Control::Data(vec![])
        );
        assert_eq!(d.address, 5);
        assert_eq!(
            d.control(&setup(0x00, SET_CONFIGURATION, 1, 0, 0), &[]),
            Control::Data(vec![])
        );
        assert_eq!(
            data(d.control(&setup(0x80, GET_CONFIGURATION, 0, 0, 1), &[])),
            [1]
        );
        assert_eq!(
            d.control(&setup(0x01, SET_INTERFACE, 0, 0, 0), &[]),
            Control::Data(vec![])
        );
        assert_eq!(
            d.control(&setup(0x01, SET_INTERFACE, 1, 0, 0), &[]),
            Control::Stall
        );
        assert_eq!(
            d.control(&setup(0x00, SET_CONFIGURATION, 2, 0, 0), &[]),
            Control::Stall
        );
        assert_eq!(
            d.control(&setup(0x21, HID_SET_IDLE, 0x0400, 0, 0), &[]),
            Control::Data(vec![])
        );
        assert_eq!(d.idle, 4);
    }

    #[test]
    fn endpoint_halt_set_and_cleared() {
        let mut d = Device::new();
        assert_eq!(
            d.control(&setup(0x02, SET_FEATURE, 0, 0x81, 0), &[]),
            Control::Data(vec![])
        );
        assert_eq!(
            data(d.control(&setup(0x82, GET_STATUS, 0, 0x81, 2), &[])),
            [1, 0]
        );
        assert_eq!(
            d.control(&setup(0x02, CLEAR_FEATURE, 0, 0x81, 0), &[]),
            Control::Data(vec![])
        );
        assert_eq!(
            data(d.control(&setup(0x82, GET_STATUS, 0, 0x81, 2), &[])),
            [0, 0]
        );
        assert_eq!(
            d.control(&setup(0x02, CLEAR_FEATURE, 0, 0x85, 0), &[]),
            Control::Stall
        );
    }

    #[test]
    fn feature_reports_store_and_echo() {
        let mut d = Device::new();
        let feature = |id: u8| u16::from(REPORT_FEATURE) << 8 | u16::from(id);
        // Default feature 1: id then zeros.
        let first = data(d.control(&setup(0xa1, HID_GET_REPORT, feature(1), 0, 64), &[]));
        assert_eq!(first.len(), 64);
        assert_eq!(first[0], 1);
        let body = [1u8, 0xaa, 0xbb, 0xcc];
        assert_eq!(
            d.control(&setup(0x21, HID_SET_REPORT, feature(1), 0, 4), &body),
            Control::Data(vec![])
        );
        assert_eq!(
            data(d.control(&setup(0xa1, HID_GET_REPORT, feature(1), 0, 64), &[])),
            body
        );
        assert_eq!(
            data(d.control(&setup(0xa1, HID_GET_REPORT, feature(1), 0, 2), &[])),
            body[..2]
        );
        assert_eq!(
            d.control(&setup(0xa1, HID_GET_REPORT, feature(9), 0, 8), &[]),
            Control::Stall
        );
    }

    #[test]
    fn vendor_requests_stall() {
        let mut d = Device::new();
        assert_eq!(d.control(&setup(0xc0, 1, 0, 0, 8), &[]), Control::Stall);
    }
}
