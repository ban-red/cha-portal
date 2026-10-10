//! A full-speed USB HID device and the device end of a USB/IP connection,
//! Sans-IO: commands in, reply bytes out. Generalised from the spike
//! `spikes/s9-usbip-loopback` (`device.rs` and `engine.rs`): the identity,
//! strings and report descriptor come from the pad variant, and what a HID
//! class request means is the caller's ([`Hid`]).
//!
//! The device is one configuration with one HID interface (no boot protocol)
//! and two interrupt endpoints, IN `0x81` and OUT `0x01`, 64 bytes, every
//! frame. Endpoint 0's standard requests (GET_DESCRIPTOR, SET_ADDRESS,
//! SET/GET_CONFIGURATION, SET/GET_INTERFACE, GET_STATUS, SET/CLEAR_FEATURE of
//! ENDPOINT_HALT) are answered here, always with success: replaying them on
//! a device already configured would desync the data toggles. HID GET/SET_REPORT
//! go to [`Hid`]; GET/SET_IDLE and GET/SET_PROTOCOL are kept here. Anything
//! else stalls (the URB completes with -EPIPE).
//!
//! [`Engine`] parks the kernel's interrupt-IN `CMD_SUBMIT`s; a report completes
//! the oldest, or, with none parked, waits in a FIFO of [`PENDING_MAX`].

use std::collections::VecDeque;

use crate::usbip::{
    Command, DIR_IN, ECONNRESET, EPIPE, Submit, encode_ret_submit, encode_ret_unlink,
};

/// Interrupt IN and OUT endpoint addresses; 64-byte packets, bInterval 1 (the
/// real 2026 controller's is not known: the I/O Registry does not show it).
pub const EP_IN: u8 = 0x81;
pub const EP_OUT: u8 = 0x01;
pub const EP_SIZE: u16 = 64;

/// Reports kept while no URB is parked, so a press/release pair survives.
pub const PENDING_MAX: usize = 4;

/// What the device says it is.
#[derive(Debug, Clone)]
pub struct Identity {
    pub vendor: u16,
    pub product: u16,
    pub bcd_device: u16,
    pub manufacturer: String,
    pub product_name: String,
    pub serial: String,
    pub report_descriptor: &'static [u8],
}

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
const DT_BOS: u8 = 15;
const DT_DEVICE_CAPABILITY: u8 = 16;
const DT_HID: u8 = 0x21;
const DT_REPORT: u8 = 0x22;

const FEATURE_ENDPOINT_HALT: u16 = 0;

/// Report types in `wValue`'s high byte of GET/SET_REPORT (input, 1, is not
/// served: the protocols answer feature and output reports).
pub const REPORT_OUTPUT: u8 = 2;
pub const REPORT_FEATURE: u8 = 3;

/// What the device does with HID class requests and interrupt OUT data.
/// Errors are errnos; the URB stalls.
pub trait Hid {
    /// GET_REPORT: `kind` is input (1), [`REPORT_OUTPUT`] or
    /// [`REPORT_FEATURE`]; the bytes lead with the report id.
    fn get_report(&mut self, kind: u8, id: u8) -> Result<Vec<u8>, i32>;
    /// SET_REPORT with its data stage (the report id leads when numbered).
    fn set_report(&mut self, kind: u8, id: u8, data: &[u8]) -> Result<(), i32>;
    /// Data of an interrupt OUT transfer (the report id leads).
    fn output(&mut self, data: &[u8]);
}

impl Identity {
    pub fn device_descriptor(&self) -> Vec<u8> {
        let mut d = vec![18, DT_DEVICE, 0x01, 0x02, 0, 0, 0, 64];
        d.extend_from_slice(&self.vendor.to_le_bytes());
        d.extend_from_slice(&self.product.to_le_bytes());
        d.extend_from_slice(&self.bcd_device.to_le_bytes());
        d.extend_from_slice(&[1, 2, 3, 1]); // strings: manufacturer, product, serial; one configuration
        d
    }

    /// The binary device object store: one USB 2.0 Extension capability (LPM
    /// supported), which a device with bcdUSB 0x0201 has to answer with.
    pub fn bos_descriptor(&self) -> Vec<u8> {
        vec![
            5,
            DT_BOS,
            12,
            0,
            1, // BOS: total length 12, one capability
            7,
            DT_DEVICE_CAPABILITY,
            2,
            0x02,
            0,
            0,
            0, // USB 2.0 Extension: LPM
        ]
    }

    pub fn hid_descriptor(&self) -> Vec<u8> {
        let mut d = vec![9, DT_HID, 0x11, 0x01, 0, 1, DT_REPORT];
        d.extend_from_slice(&(self.report_descriptor.len() as u16).to_le_bytes());
        d
    }

    /// Configuration 1: interface 0 (HID, no boot protocol), the HID
    /// descriptor, then the interrupt IN and OUT endpoints.
    pub fn configuration_descriptor(&self) -> Vec<u8> {
        let mut d = vec![9, DT_CONFIGURATION, 0, 0, 1, 1, 0, 0xa0, 250];
        d.extend_from_slice(&[9, DT_INTERFACE, 0, 0, 2, 3, 0, 0, 0]);
        d.extend_from_slice(&self.hid_descriptor());
        for ep in [EP_IN, EP_OUT] {
            d.extend_from_slice(&[7, DT_ENDPOINT, ep, 3]); // interrupt
            d.extend_from_slice(&EP_SIZE.to_le_bytes());
            d.push(1);
        }
        let total = d.len() as u16;
        d[2..4].copy_from_slice(&total.to_le_bytes());
        d
    }

    pub fn string_descriptor(&self, index: u8) -> Option<Vec<u8>> {
        let text = match index {
            0 => return Some(vec![4, DT_STRING, 0x09, 0x04]), // US English
            1 => &self.manufacturer,
            2 => &self.product_name,
            3 => &self.serial,
            _ => return None,
        };
        // A descriptor's length is one byte.
        let units: Vec<u8> = text
            .encode_utf16()
            .take(126)
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut d = vec![(2 + units.len()) as u8, DT_STRING];
        d.extend(units);
        Some(d)
    }
}

/// What endpoint 0 answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// Success; the data stage for an IN request (empty for the rest).
    Data(Vec<u8>),
    /// STALL (the URB completes with -EPIPE).
    Stall,
}

#[derive(Debug)]
pub struct Device {
    identity: Identity,
    pub address: u8,
    pub configuration: u8,
    pub halted_in: bool,
    pub halted_out: bool,
    pub idle: u8,
    pub protocol: u8,
}

fn truncate(mut v: Vec<u8>, w_length: u16) -> Control {
    v.truncate(usize::from(w_length));
    Control::Data(v)
}

impl Device {
    pub fn new(identity: Identity) -> Self {
        Self {
            identity,
            address: 0,
            configuration: 0,
            halted_in: false,
            halted_out: false,
            idle: 0,
            protocol: 0,
        }
    }

    /// Dispatch one control transfer: the 8-byte setup packet and, for OUT
    /// requests, the data stage.
    pub fn control(&mut self, setup: &[u8; 8], out: &[u8], hid: &mut dyn Hid) -> Control {
        let (bm, req) = (setup[0], setup[1]);
        let value = u16::from_le_bytes([setup[2], setup[3]]);
        let index = u16::from_le_bytes([setup[4], setup[5]]);
        let w_length = u16::from_le_bytes([setup[6], setup[7]]);
        let recipient = bm & RECIPIENT_MASK;
        match bm & TYPE_MASK {
            TYPE_STANDARD => self.standard(bm, req, value, index, w_length),
            TYPE_CLASS if recipient == RECIP_INTERFACE && index == 0 => {
                self.hid(req, value, w_length, out, hid)
            }
            _ => Control::Stall,
        }
    }

    fn standard(&mut self, bm: u8, req: u8, value: u16, index: u16, w_length: u16) -> Control {
        let recipient = bm & RECIPIENT_MASK;
        let [value_lo, value_hi] = value.to_le_bytes();
        match (req, recipient) {
            (GET_DESCRIPTOR, RECIP_DEVICE) => match value_hi {
                DT_DEVICE => truncate(self.identity.device_descriptor(), w_length),
                DT_CONFIGURATION if value_lo == 0 => {
                    truncate(self.identity.configuration_descriptor(), w_length)
                }
                DT_STRING => self
                    .identity
                    .string_descriptor(value_lo)
                    .map_or(Control::Stall, |d| truncate(d, w_length)),
                DT_BOS => truncate(self.identity.bos_descriptor(), w_length),
                // Device qualifier, other-speed configuration: a full-speed-only
                // device stalls these.
                _ => Control::Stall,
            },
            (GET_DESCRIPTOR, RECIP_INTERFACE) if index == 0 => match value_hi {
                DT_REPORT => truncate(self.identity.report_descriptor.to_vec(), w_length),
                DT_HID => truncate(self.identity.hid_descriptor(), w_length),
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

    fn hid(
        &mut self,
        req: u8,
        value: u16,
        w_length: u16,
        out: &[u8],
        hid: &mut dyn Hid,
    ) -> Control {
        let [id, kind] = value.to_le_bytes();
        match req {
            HID_GET_REPORT => match hid.get_report(kind, id) {
                Ok(data) => truncate(data, w_length),
                Err(_) => Control::Stall,
            },
            HID_SET_REPORT => match hid.set_report(kind, id, out) {
                Ok(()) => Control::Data(Vec::new()),
                Err(_) => Control::Stall,
            },
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

#[derive(Debug)]
struct Parked {
    seqnum: u32,
    length: u32,
}

#[derive(Debug)]
pub struct Engine {
    pub device: Device,
    parked: VecDeque<Parked>,
    pending: VecDeque<Vec<u8>>,
    /// Reports dropped because the FIFO was full.
    pub dropped: u64,
}

fn complete_in(out: &mut Vec<u8>, seqnum: u32, length: u32, report: &[u8]) {
    let n = report.len().min(length as usize);
    encode_ret_submit(out, seqnum, 0, n as u32, &report[..n]);
}

impl Engine {
    pub fn new(identity: Identity) -> Self {
        Self {
            device: Device::new(identity),
            parked: VecDeque::new(),
            pending: VecDeque::new(),
            dropped: 0,
        }
    }

    #[cfg(test)]
    pub fn parked(&self) -> usize {
        self.parked.len()
    }

    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Handle one command and its body; reply bytes are appended to `out`.
    pub fn on_command(&mut self, cmd: &Command, body: &[u8], out: &mut Vec<u8>, hid: &mut dyn Hid) {
        match cmd {
            Command::Submit(s) if s.ep == 0 => self.control(s, body, out, hid),
            Command::Submit(s) if s.direction == DIR_IN && s.ep == 1 => {
                if self.device.halted_in {
                    encode_ret_submit(out, s.seqnum, -EPIPE, 0, &[]);
                } else if let Some(report) = self.pending.pop_front() {
                    complete_in(out, s.seqnum, s.length, &report);
                } else {
                    self.parked.push_back(Parked {
                        seqnum: s.seqnum,
                        length: s.length,
                    });
                }
            }
            Command::Submit(s) if s.direction != DIR_IN && s.ep == u32::from(EP_OUT) => {
                let n = body.len().min(s.length as usize);
                hid.output(&body[..n]);
                encode_ret_submit(out, s.seqnum, 0, n as u32, &[]);
            }
            Command::Submit(s) => encode_ret_submit(out, s.seqnum, -EPIPE, 0, &[]),
            Command::Unlink {
                seqnum,
                unlink_seqnum,
                ..
            } => {
                let found = self.parked.iter().position(|p| p.seqnum == *unlink_seqnum);
                let status = if let Some(at) = found {
                    self.parked.remove(at);
                    -ECONNRESET
                } else {
                    0
                };
                encode_ret_unlink(out, *seqnum, status);
            }
        }
    }

    fn control(&mut self, s: &Submit, body: &[u8], out: &mut Vec<u8>, hid: &mut dyn Hid) {
        match self.device.control(&s.setup, body, hid) {
            Control::Data(data) => {
                if s.setup[0] & 0x80 != 0 {
                    encode_ret_submit(out, s.seqnum, 0, data.len() as u32, &data);
                } else {
                    encode_ret_submit(out, s.seqnum, 0, s.length, &[]);
                }
            }
            Control::Stall => encode_ret_submit(out, s.seqnum, -EPIPE, 0, &[]),
        }
    }

    /// A report was produced: complete the oldest parked URB with it, or keep
    /// it (the newest `PENDING_MAX`).
    pub fn push_report(&mut self, report: &[u8], out: &mut Vec<u8>) {
        if let Some(p) = self.parked.pop_front() {
            complete_in(out, p.seqnum, p.length, report);
        } else {
            if self.pending.len() == PENDING_MAX {
                self.pending.pop_front();
                self.dropped += 1;
            }
            self.pending.push_back(report.to_vec());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbip::{DIR_OUT, HEADER_LEN, NO_ISO, RET_SUBMIT, RET_UNLINK};

    #[rustfmt::skip]
    const RD: &[u8] = &[
        0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08,
        0x85, 0x01, 0x95, 0x3F, 0x09, 0x01, 0xB1, 0x02, 0xC0,
    ];

    fn identity() -> Identity {
        Identity {
            vendor: 0x28de,
            product: 0x1302,
            bcd_device: 0x0100,
            manufacturer: "Valve Software".into(),
            product_name: "Steam Controller".into(),
            serial: "FXA0123456789".into(),
            report_descriptor: RD,
        }
    }

    /// Records what the device passes on, answers feature report 1.
    #[derive(Default)]
    struct Recorder {
        sets: Vec<(u8, u8, Vec<u8>)>,
        outs: Vec<Vec<u8>>,
    }

    impl Hid for Recorder {
        fn get_report(&mut self, kind: u8, id: u8) -> Result<Vec<u8>, i32> {
            if kind == REPORT_FEATURE && id == 1 {
                let mut v = vec![0; 64];
                v[0] = 1;
                Ok(v)
            } else {
                Err(libc::EINVAL)
            }
        }
        fn set_report(&mut self, kind: u8, id: u8, data: &[u8]) -> Result<(), i32> {
            self.sets.push((kind, id, data.to_vec()));
            Ok(())
        }
        fn output(&mut self, data: &[u8]) {
            self.outs.push(data.to_vec());
        }
    }

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

    fn ctl(d: &mut Device, s: [u8; 8], out: &[u8]) -> Control {
        d.control(&s, out, &mut Recorder::default())
    }

    #[test]
    fn descriptor_lengths_are_consistent() {
        let id = identity();
        let dev = id.device_descriptor();
        assert_eq!(dev.len(), 18);
        assert_eq!(u16::from_le_bytes([dev[8], dev[9]]), 0x28de);
        assert_eq!(u16::from_le_bytes([dev[10], dev[11]]), 0x1302);
        // As the real 2026 controller's: USB 2.0.1, endpoint 0 64 bytes, strings
        // 1 to 3 (the serial is the third), one configuration.
        assert_eq!(u16::from_le_bytes([dev[2], dev[3]]), 0x0201);
        assert_eq!(dev[7], 64);
        assert_eq!(&dev[14..18], &[1, 2, 3, 1]);

        let cfg = id.configuration_descriptor();
        assert_eq!(cfg.len(), 41);
        assert_eq!(usize::from(u16::from_le_bytes([cfg[2], cfg[3]])), cfg.len());
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
        let hid = id.hid_descriptor();
        assert_eq!(usize::from(u16::from_le_bytes([hid[7], hid[8]])), RD.len());
        assert_eq!(&cfg[18..27], hid.as_slice());
        assert_eq!((cfg[4], cfg[13], cfg[14]), (1, 2, 3));
        // The endpoints: 0x81 and 0x01, interrupt, 64 bytes, every frame.
        assert_eq!(&cfg[27..34], &[7, DT_ENDPOINT, 0x81, 3, 64, 0, 1]);
        assert_eq!(&cfg[34..41], &[7, DT_ENDPOINT, 0x01, 3, 64, 0, 1]);
    }

    #[test]
    fn the_bos_comes_with_bcd_usb_0201() {
        let id = identity();
        let bos = id.bos_descriptor();
        assert_eq!(usize::from(u16::from_le_bytes([bos[2], bos[3]])), bos.len());
        assert_eq!((bos[1], bos[4]), (DT_BOS, 1));
        // One capability, USB 2.0 Extension (2), of 7 bytes.
        assert_eq!(&bos[5..8], &[7, DT_DEVICE_CAPABILITY, 2]);
        let mut d = Device::new(identity());
        assert_eq!(
            data(ctl(&mut d, setup(0x80, GET_DESCRIPTOR, 0x0f00, 0, 5), &[])),
            bos[..5]
        );
        assert_eq!(
            data(ctl(
                &mut d,
                setup(0x80, GET_DESCRIPTOR, 0x0f00, 0, 255),
                &[]
            )),
            bos
        );
    }

    #[test]
    fn strings_are_utf16() {
        let id = identity();
        assert_eq!(id.string_descriptor(0), Some(vec![4, 3, 9, 4]));
        let s = id.string_descriptor(3).unwrap();
        assert_eq!(usize::from(s[0]), s.len());
        assert_eq!(s.len(), 2 + 2 * "FXA0123456789".len());
        assert_eq!((s[2], s[3]), (b'F', 0));
        assert!(id.string_descriptor(9).is_none());
    }

    #[test]
    fn get_descriptor_truncates_to_wlength() {
        let mut d = Device::new(identity());
        let first8 = data(ctl(&mut d, setup(0x80, GET_DESCRIPTOR, 0x0100, 0, 8), &[]));
        assert_eq!(first8, identity().device_descriptor()[..8]);
        let cfg9 = data(ctl(&mut d, setup(0x80, GET_DESCRIPTOR, 0x0200, 0, 9), &[]));
        assert_eq!(cfg9.len(), 9);
        let cfg = data(ctl(
            &mut d,
            setup(0x80, GET_DESCRIPTOR, 0x0200, 0, 255),
            &[],
        ));
        assert_eq!(cfg, identity().configuration_descriptor());
        let rd = data(ctl(
            &mut d,
            setup(0x81, GET_DESCRIPTOR, 0x2200, 0, 4096),
            &[],
        ));
        assert_eq!(rd, RD);
        assert_eq!(
            ctl(&mut d, setup(0x80, GET_DESCRIPTOR, 0x0600, 0, 10), &[]),
            Control::Stall
        );
    }

    #[test]
    fn config_requests_are_acknowledged() {
        let mut d = Device::new(identity());
        let ack = Control::Data(vec![]);
        assert_eq!(ctl(&mut d, setup(0x00, SET_ADDRESS, 5, 0, 0), &[]), ack);
        assert_eq!(d.address, 5);
        assert_eq!(
            ctl(&mut d, setup(0x00, SET_CONFIGURATION, 1, 0, 0), &[]),
            ack
        );
        assert_eq!(
            data(ctl(&mut d, setup(0x80, GET_CONFIGURATION, 0, 0, 1), &[])),
            [1]
        );
        assert_eq!(ctl(&mut d, setup(0x01, SET_INTERFACE, 0, 0, 0), &[]), ack);
        assert_eq!(
            ctl(&mut d, setup(0x01, SET_INTERFACE, 1, 0, 0), &[]),
            Control::Stall
        );
        assert_eq!(
            ctl(&mut d, setup(0x00, SET_CONFIGURATION, 2, 0, 0), &[]),
            Control::Stall
        );
        assert_eq!(
            ctl(&mut d, setup(0x21, HID_SET_IDLE, 0x0400, 0, 0), &[]),
            ack
        );
        assert_eq!(d.idle, 4);
    }

    #[test]
    fn endpoint_halt_set_and_cleared() {
        let mut d = Device::new(identity());
        let ack = Control::Data(vec![]);
        assert_eq!(ctl(&mut d, setup(0x02, SET_FEATURE, 0, 0x81, 0), &[]), ack);
        assert_eq!(
            data(ctl(&mut d, setup(0x82, GET_STATUS, 0, 0x81, 2), &[])),
            [1, 0]
        );
        assert_eq!(
            ctl(&mut d, setup(0x02, CLEAR_FEATURE, 0, 0x81, 0), &[]),
            ack
        );
        assert_eq!(
            data(ctl(&mut d, setup(0x82, GET_STATUS, 0, 0x81, 2), &[])),
            [0, 0]
        );
        assert_eq!(
            ctl(&mut d, setup(0x02, CLEAR_FEATURE, 0, 0x85, 0), &[]),
            Control::Stall
        );
    }

    #[test]
    fn report_requests_go_to_the_hid_handler() {
        let mut d = Device::new(identity());
        let mut hid = Recorder::default();
        let feature = |id: u8| u16::from(REPORT_FEATURE) << 8 | u16::from(id);
        let got = data(d.control(
            &setup(0xa1, HID_GET_REPORT, feature(1), 0, 64),
            &[],
            &mut hid,
        ));
        assert_eq!((got.len(), got[0]), (64, 1));
        // Cut to wLength.
        let short = data(d.control(
            &setup(0xa1, HID_GET_REPORT, feature(1), 0, 2),
            &[],
            &mut hid,
        ));
        assert_eq!(short.len(), 2);
        // The handler refuses: the request stalls.
        assert_eq!(
            d.control(
                &setup(0xa1, HID_GET_REPORT, feature(9), 0, 8),
                &[],
                &mut hid
            ),
            Control::Stall
        );
        let body = [1u8, 0xaa, 0xbb];
        assert_eq!(
            d.control(
                &setup(0x21, HID_SET_REPORT, feature(1), 0, 3),
                &body,
                &mut hid
            ),
            Control::Data(vec![])
        );
        // An output report by SET_REPORT carries its type and id.
        let output = u16::from(REPORT_OUTPUT) << 8 | 0x80;
        d.control(
            &setup(0x21, HID_SET_REPORT, output, 0, 2),
            &[0x80, 1],
            &mut hid,
        );
        assert_eq!(
            hid.sets,
            [
                (REPORT_FEATURE, 1, body.to_vec()),
                (REPORT_OUTPUT, 0x80, vec![0x80, 1])
            ]
        );
        // Another interface's requests stall.
        assert_eq!(
            d.control(
                &setup(0xa1, HID_GET_REPORT, feature(1), 1, 8),
                &[],
                &mut hid
            ),
            Control::Stall
        );
    }

    #[test]
    fn vendor_requests_stall() {
        let mut d = Device::new(identity());
        assert_eq!(ctl(&mut d, setup(0xc0, 1, 0, 0, 8), &[]), Control::Stall);
    }

    fn submit(seqnum: u32, direction: u32, ep: u32, length: u32, setup: [u8; 8]) -> Command {
        Command::Submit(Submit {
            seqnum,
            devid: 0x10002,
            direction,
            ep,
            transfer_flags: 0,
            length,
            start_frame: 0,
            number_of_packets: NO_ISO,
            interval: 0,
            setup,
        })
    }

    fn in_urb(seqnum: u32) -> Command {
        submit(seqnum, DIR_IN, 1, 64, [0; 8])
    }

    fn word(buf: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(buf[at..at + 4].try_into().unwrap())
    }

    fn run(e: &mut Engine, cmd: &Command, body: &[u8], out: &mut Vec<u8>) -> Recorder {
        let mut hid = Recorder::default();
        e.on_command(cmd, body, out, &mut hid);
        hid
    }

    #[test]
    fn control_in_returns_data_with_actual_length() {
        let mut e = Engine::new(identity());
        let mut out = Vec::new();
        let get_dev = [0x80, 6, 0, 1, 0, 0, 18, 0];
        run(&mut e, &submit(1, DIR_IN, 0, 18, get_dev), &[], &mut out);
        assert_eq!(word(&out, 0), RET_SUBMIT);
        assert_eq!(word(&out, 4), 1);
        assert_eq!(word(&out, 24), 18);
        assert_eq!(out.len(), HEADER_LEN + 18);
        assert_eq!(out[HEADER_LEN], 18);
    }

    #[test]
    fn control_out_acks_with_requested_length_and_stalls_unknown() {
        let mut e = Engine::new(identity());
        let mut out = Vec::new();
        // SET_REPORT feature 1, 4 bytes.
        let set = [0x21, 9, 1, 3, 0, 0, 4, 0];
        let hid = run(
            &mut e,
            &submit(2, DIR_OUT, 0, 4, set),
            &[1, 2, 3, 4],
            &mut out,
        );
        assert_eq!(word(&out, 20), 0);
        assert_eq!(word(&out, 24), 4);
        assert_eq!(out.len(), HEADER_LEN);
        assert_eq!(hid.sets, [(REPORT_FEATURE, 1, vec![1, 2, 3, 4])]);
        out.clear();
        run(
            &mut e,
            &submit(3, DIR_IN, 0, 8, [0xc0, 1, 0, 0, 0, 0, 8, 0]),
            &[],
            &mut out,
        );
        assert_eq!(word(&out, 20) as i32, -EPIPE);
    }

    #[test]
    fn report_completes_the_oldest_parked_urb() {
        let mut e = Engine::new(identity());
        let mut out = Vec::new();
        run(&mut e, &in_urb(10), &[], &mut out);
        run(&mut e, &in_urb(11), &[], &mut out);
        assert!(out.is_empty());
        assert_eq!(e.parked(), 2);
        e.push_report(&[0x45, 1, 2, 3], &mut out);
        assert_eq!(word(&out, 0), RET_SUBMIT);
        assert_eq!(word(&out, 4), 10);
        assert_eq!(word(&out, 24), 4);
        assert_eq!(&out[HEADER_LEN..], &[0x45, 1, 2, 3]);
        assert_eq!(e.parked(), 1);
    }

    #[test]
    fn report_is_cut_to_the_urb_length() {
        let mut e = Engine::new(identity());
        let mut out = Vec::new();
        run(&mut e, &submit(1, DIR_IN, 1, 2, [0; 8]), &[], &mut out);
        e.push_report(&[9, 8, 7], &mut out);
        assert_eq!(word(&out, 24), 2);
        assert_eq!(out.len(), HEADER_LEN + 2);
    }

    #[test]
    fn reports_without_a_urb_wait_in_a_fifo_of_four() {
        let mut e = Engine::new(identity());
        let mut sink = Vec::new();
        for i in 0..6u8 {
            e.push_report(&[i], &mut sink);
        }
        assert!(sink.is_empty());
        assert_eq!(e.pending(), PENDING_MAX);
        assert_eq!(e.dropped, 2);
        // The next URB gets the oldest kept one (2), then 3, 4, 5.
        for want in 2..6u8 {
            let mut out = Vec::new();
            run(&mut e, &in_urb(u32::from(want)), &[], &mut out);
            assert_eq!(out[HEADER_LEN], want);
        }
        assert_eq!(e.pending(), 0);
    }

    #[test]
    fn a_halted_in_endpoint_stalls_urbs() {
        let mut e = Engine::new(identity());
        e.device.halted_in = true;
        let mut out = Vec::new();
        run(&mut e, &in_urb(1), &[], &mut out);
        assert_eq!(word(&out, 20) as i32, -EPIPE);
        assert_eq!(e.parked(), 0);
    }

    #[test]
    fn unlink_cancels_a_parked_urb() {
        let mut e = Engine::new(identity());
        let mut out = Vec::new();
        run(&mut e, &in_urb(20), &[], &mut out);
        let unlink = |seq, target| Command::Unlink {
            seqnum: seq,
            devid: 0,
            direction: 0,
            ep: 0,
            unlink_seqnum: target,
        };
        run(&mut e, &unlink(21, 20), &[], &mut out);
        assert_eq!(word(&out, 0), RET_UNLINK);
        assert_eq!(word(&out, 4), 21);
        assert_eq!(word(&out, 20) as i32, -ECONNRESET);
        assert_eq!(e.parked(), 0);
        // Nothing parked any more, and a second unlink finds nothing.
        out.clear();
        run(&mut e, &unlink(22, 20), &[], &mut out);
        assert_eq!(word(&out, 20), 0);
        e.push_report(&[1], &mut out);
        assert_eq!(out.len(), HEADER_LEN);
        assert_eq!(e.pending(), 1);
    }

    #[test]
    fn interrupt_out_goes_to_the_handler() {
        let mut e = Engine::new(identity());
        let mut out = Vec::new();
        let hid = run(
            &mut e,
            &submit(5, DIR_OUT, 1, 3, [0; 8]),
            &[0x80, 1, 2],
            &mut out,
        );
        assert_eq!(word(&out, 24), 3);
        assert_eq!(hid.outs, vec![vec![0x80, 1, 2]]);
    }
}
