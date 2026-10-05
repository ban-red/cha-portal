//! uhid: virtual HID devices through `/dev/uhid`.
//!
//! The kernel's HID drivers bind them as they would a real USB device
//! (`hid-playstation` for the DualSense, `hid-steam` for the Steam Controller)
//! and make their input devices and `hidraw` nodes on the host. This is only
//! the device layer: it creates a device from a report descriptor, writes its
//! input reports, and answers what the kernel asks of it from one thread per
//! device (`GET_REPORT` and `SET_REPORT` for feature reports, `OUTPUT` for
//! output reports). What the reports mean is a [`Protocol`]'s business.
//!
//! The protocol is `<linux/uhid.h>`'s: events are a `u32` type and a payload,
//! the kernel zero-extends short writes, and it always reads our buffer whole.
//!
//! Where the kernel put a device is found in sysfs (`/sys/devices/virtual/
//! misc/uhid/<hid device>`), by the `phys` string the device was created with.

use std::fs::OpenOptions;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::gamepad::{PadEvent, PadState};

// <linux/uhid.h>'s event types.
const UHID_DESTROY: u32 = 1;
const UHID_START: u32 = 2;
const UHID_STOP: u32 = 3;
const UHID_OPEN: u32 = 4;
const UHID_CLOSE: u32 = 5;
const UHID_OUTPUT: u32 = 6;
const UHID_GET_REPORT: u32 = 9;
const UHID_GET_REPORT_REPLY: u32 = 10;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT: u32 = 13;
const UHID_SET_REPORT_REPLY: u32 = 14;

/// A report's most bytes.
const DATA_MAX: usize = 4096;
pub const BUS_USB: u16 = 0x03;
pub const BUS_BLUETOOTH: u16 = 0x05;

/// Report types, `enum uhid_report_type`.
pub const FEATURE_REPORT: u8 = 0;
pub const OUTPUT_REPORT: u8 = 1;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Create2Req {
    name: [u8; 128],
    phys: [u8; 64],
    uniq: [u8; 64],
    rd_size: u16,
    bus: u16,
    vendor: u32,
    product: u32,
    version: u32,
    country: u32,
    rd_data: [u8; DATA_MAX],
}

/// INPUT2's payload, written piecewise (the size, then the report).
#[repr(C, packed)]
#[derive(Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
struct Input2Req {
    size: u16,
    data: [u8; DATA_MAX],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct OutputReq {
    data: [u8; DATA_MAX],
    size: u16,
    rtype: u8,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GetReportReq {
    id: u32,
    rnum: u8,
    rtype: u8,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GetReportReplyReq {
    id: u32,
    err: u16,
    size: u16,
    data: [u8; DATA_MAX],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct SetReportReq {
    id: u32,
    rnum: u8,
    rtype: u8,
    size: u16,
    data: [u8; DATA_MAX],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct SetReportReplyReq {
    id: u32,
    err: u16,
}

/// `struct uhid_event`: the type and the largest payload (CREATE2's).
const EVENT_SIZE: usize = 4 + std::mem::size_of::<Create2Req>();

/// What a virtual device is: its identity and its report descriptor.
pub struct Params<'a> {
    pub name: &'a str,
    pub phys: &'a str,
    pub uniq: &'a str,
    /// `BUS_USB` or `BUS_BLUETOOTH`: the kernel's `HID_ID` bus, which `hidapi`
    /// (and so SDL) reads to tell the two apart.
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub descriptor: &'a [u8],
}

/// What a device does with the kernel's requests, and how it reports its state.
/// Errors are errnos (positive), which the kernel sees as a failed request.
pub trait Protocol: Send {
    /// The input reports for this state, as they go on the wire, in order (one,
    /// unless the device's transport splits a state over several).
    fn report(&mut self, state: &PadState, now: Instant) -> Vec<Vec<u8>>;
    /// How often a report goes out when nothing changed (real controllers
    /// report continuously, and clients wait for the first one).
    fn period(&self) -> Duration;
    /// A feature report asked for: its bytes, the report id first.
    fn get_report(&mut self, number: u8, kind: u8) -> Result<Vec<u8>, i32>;
    /// A feature report set; what it asks of the controller's owner.
    fn set_report(&mut self, number: u8, kind: u8, data: &[u8]) -> Result<Vec<PadEvent>, i32>;
    /// An output report the kernel or an app wrote.
    fn output(&mut self, kind: u8, data: &[u8]) -> Vec<PadEvent>;
}

struct Inner {
    protocol: Box<dyn Protocol>,
    state: PadState,
    last_sent: Instant,
}

/// A created device, shared with the thread that serves it.
pub struct Device {
    fd: OwnedFd,
    inner: Mutex<Inner>,
    stop: AtomicBool,
}

impl Device {
    /// Creates the device and starts the thread that answers the kernel; its
    /// first request comes while the driver probes, so call this and then wait
    /// for the nodes ([`wait_for_nodes`]).
    pub fn create(
        path: &Path,
        params: &Params,
        protocol: Box<dyn Protocol>,
        events: broadcast::Sender<PadEvent>,
        thread: String,
    ) -> Result<Arc<Self>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        let device = Arc::new(Self {
            fd: file.into(),
            inner: Mutex::new(Inner {
                protocol,
                state: PadState::default(),
                last_sent: Instant::now(),
            }),
            stop: AtomicBool::new(false),
        });
        device.write(UHID_CREATE2, &create2(params)?)?;
        let served = Arc::clone(&device);
        std::thread::Builder::new()
            .name(thread)
            .spawn(move || served.serve(&events))
            .context("spawning the uhid thread")?;
        Ok(device)
    }

    /// A new state from the page: the report goes out at once.
    pub fn update(&self, state: &PadState) {
        let mut inner = self.inner.lock().expect("uhid lock");
        inner.state = state.clone();
        self.send_state(&mut inner);
    }

    /// Takes the device away (the kernel unbinds it and removes its nodes).
    pub fn destroy(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.write(UHID_DESTROY, &[]);
    }

    fn send_state(&self, inner: &mut Inner) {
        let now = Instant::now();
        let reports = inner.protocol.report(&inner.state, now);
        inner.last_sent = now;
        for report in reports {
            let mut body = Vec::with_capacity(2 + report.len());
            body.extend_from_slice(&(report.len() as u16).to_le_bytes());
            body.extend_from_slice(&report);
            // Before the kernel has started the device this fails; the next
            // report goes out a period later.
            if let Err(err) = self.write(UHID_INPUT2, &body) {
                debug!("uhid input report: {err}");
            }
        }
    }

    /// One event: the type, then the payload as far as it is used.
    fn write(&self, kind: u32, body: &[u8]) -> std::io::Result<()> {
        let mut event = Vec::with_capacity(4 + body.len());
        event.extend_from_slice(&kind.to_le_bytes());
        event.extend_from_slice(body);
        // SAFETY: a valid descriptor and buffer.
        let n = unsafe { libc::write(self.fd.as_raw_fd(), event.as_ptr().cast(), event.len()) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Milliseconds until the next repeated report is due.
    fn until_due(&self) -> i32 {
        let inner = self.inner.lock().expect("uhid lock");
        let due = inner.last_sent + inner.protocol.period();
        due.saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, 250) as i32
    }

    fn keep_alive(&self) {
        let mut inner = self.inner.lock().expect("uhid lock");
        if inner.last_sent.elapsed() >= inner.protocol.period() {
            self.send_state(&mut inner);
        }
    }

    /// Answers the kernel's requests until the device is destroyed.
    fn serve(&self, events: &broadcast::Sender<PadEvent>) {
        let raw = self.fd.as_raw_fd();
        let mut buf = vec![0u8; EVENT_SIZE];
        while !self.stop.load(Ordering::Relaxed) {
            let mut poll = libc::pollfd {
                fd: raw,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd.
            if unsafe { libc::poll(&mut poll, 1, self.until_due()) } > 0 {
                loop {
                    // SAFETY: the buffer holds a whole event.
                    let n = unsafe { libc::read(raw, buf.as_mut_ptr().cast(), buf.len()) };
                    if n < 0 {
                        match std::io::Error::last_os_error().kind() {
                            std::io::ErrorKind::WouldBlock => break,
                            std::io::ErrorKind::Interrupted => continue,
                            _ => return,
                        }
                    }
                    if (n as usize) < 4 {
                        return;
                    }
                    self.handle(&buf, events);
                }
            }
            self.keep_alive();
        }
    }

    fn handle(&self, buf: &[u8], events: &broadcast::Sender<PadEvent>) {
        let kind = u32::from_le_bytes(buf[..4].try_into().expect("4 bytes"));
        let body = &buf[4..];
        match kind {
            UHID_START | UHID_STOP | UHID_OPEN | UHID_CLOSE => debug!(kind, "uhid event"),
            UHID_OUTPUT => {
                // SAFETY: the body is a whole event's payload; packed, so unaligned.
                let req: OutputReq = unsafe { std::ptr::read_unaligned(body.as_ptr().cast()) };
                let size = usize::from(req.size).min(DATA_MAX);
                let data = req.data;
                let emitted = self
                    .inner
                    .lock()
                    .expect("uhid lock")
                    .protocol
                    .output(req.rtype, &data[..size]);
                for event in emitted {
                    // Nobody listening is fine.
                    let _ = events.send(event);
                }
            }
            UHID_GET_REPORT => {
                // SAFETY: as above.
                let req: GetReportReq = unsafe { std::ptr::read_unaligned(body.as_ptr().cast()) };
                let answer = self
                    .inner
                    .lock()
                    .expect("uhid lock")
                    .protocol
                    .get_report(req.rnum, req.rtype);
                let mut reply = GetReportReplyReq {
                    id: req.id,
                    err: 0,
                    size: 0,
                    data: [0; DATA_MAX],
                };
                match answer {
                    Ok(bytes) => {
                        let n = bytes.len().min(DATA_MAX);
                        reply.size = n as u16;
                        reply.data[..n].copy_from_slice(&bytes[..n]);
                    }
                    Err(errno) => reply.err = errno as u16,
                }
                let used = 8 + usize::from(reply.size);
                let _ = self.write(UHID_GET_REPORT_REPLY, &bytes_of(&reply)[..used]);
            }
            UHID_SET_REPORT => {
                // SAFETY: as above.
                let req: SetReportReq = unsafe { std::ptr::read_unaligned(body.as_ptr().cast()) };
                let size = usize::from(req.size).min(DATA_MAX);
                let data = req.data;
                let answer = self.inner.lock().expect("uhid lock").protocol.set_report(
                    req.rnum,
                    req.rtype,
                    &data[..size],
                );
                let mut reply = SetReportReplyReq { id: req.id, err: 0 };
                match answer {
                    Ok(emitted) => {
                        for event in emitted {
                            let _ = events.send(event);
                        }
                    }
                    Err(errno) => reply.err = errno as u16,
                }
                let _ = self.write(UHID_SET_REPORT_REPLY, bytes_of(&reply));
            }
            other => warn!(kind = other, "unexpected uhid event"),
        }
    }
}

fn bytes_of<T: Copy>(value: &T) -> &[u8] {
    // SAFETY: plain packed integers and byte arrays, no padding.
    unsafe { std::slice::from_raw_parts((value as *const T).cast(), std::mem::size_of::<T>()) }
}

/// A CREATE2 payload, as far as it is used (the descriptor's end).
fn create2(params: &Params) -> Result<Vec<u8>> {
    if params.descriptor.len() > DATA_MAX {
        bail!("report descriptor too long");
    }
    let mut req = Create2Req {
        name: [0; 128],
        phys: [0; 64],
        uniq: [0; 64],
        rd_size: params.descriptor.len() as u16,
        bus: params.bus,
        vendor: u32::from(params.vendor),
        product: u32::from(params.product),
        version: u32::from(params.version),
        country: 0,
        rd_data: [0; DATA_MAX],
    };
    // Leave each string its NUL.
    for (to, from) in [
        (&mut req.name[..127], params.name),
        (&mut req.phys[..63], params.phys),
        (&mut req.uniq[..63], params.uniq),
    ] {
        let n = from.len().min(to.len());
        to[..n].copy_from_slice(&from.as_bytes()[..n]);
    }
    req.rd_data[..params.descriptor.len()].copy_from_slice(params.descriptor);
    let used = std::mem::offset_of!(Create2Req, rd_data) + params.descriptor.len();
    Ok(bytes_of(&req)[..used].to_vec())
}

/// A kernel device node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// `hidraw3`, `event12`, `js0`.
    pub name: String,
    pub major: u32,
    pub minor: u32,
}

/// An input device the HID driver made, and its nodes.
#[derive(Debug, Clone)]
pub struct InputDevice {
    pub name: String,
    /// `INPUT_PROP_ACCELEROMETER` is set.
    pub accelerometer: bool,
    /// It has multitouch positions (`ABS_MT_POSITION_X` is in its `abs`).
    pub touch: bool,
    pub nodes: Vec<Node>,
}

/// What the kernel made for one of our devices.
#[derive(Debug, Default)]
pub struct Found {
    /// The HID driver bound to it.
    pub driver: Option<String>,
    pub hidraw: Vec<Node>,
    pub inputs: Vec<InputDevice>,
}

const SYS_UHID: &str = "/sys/devices/virtual/misc/uhid";

/// Looks in sysfs for the devices created with this `phys`.
pub fn scan(phys: &str) -> Found {
    let mut found = Found::default();
    let want = format!("HID_PHYS={phys}");
    let Ok(dir) = std::fs::read_dir(SYS_UHID) else {
        return found;
    };
    for hid in dir.flatten() {
        let path = hid.path();
        let Ok(uevent) = std::fs::read_to_string(path.join("uevent")) else {
            continue;
        };
        if !uevent.lines().any(|line| line == want) {
            continue;
        }
        if let Some(driver) = std::fs::read_link(path.join("driver"))
            .ok()
            .and_then(|l| l.file_name().map(|n| n.to_string_lossy().into_owned()))
        {
            found.driver = Some(driver);
        }
        for entry in read_names(&path.join("hidraw")) {
            if let Some(node) = node_at(&path.join("hidraw").join(&entry), &entry) {
                found.hidraw.push(node);
            }
        }
        for input in read_names(&path.join("input")) {
            let dir = path.join("input").join(&input);
            let abs = std::fs::read_to_string(dir.join("capabilities/abs")).unwrap_or_default();
            let props = std::fs::read_to_string(dir.join("properties")).unwrap_or_default();
            let mut device = InputDevice {
                name: std::fs::read_to_string(dir.join("name"))
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                accelerometer: hex_bit(&props, 6),
                // ABS_MT_POSITION_X is 0x35.
                touch: hex_bit(&abs, 0x35),
                nodes: Vec::new(),
            };
            for node in read_names(&dir) {
                if (node.starts_with("event") || node.starts_with("js"))
                    && let Some(node) = node_at(&dir.join(&node), &node)
                {
                    device.nodes.push(node);
                }
            }
            found.inputs.push(device);
        }
    }
    found
}

/// Whether bit `bit` is set in a sysfs bitmap (space-separated hex words, the
/// most significant first, 64 bits each on this platform).
fn hex_bit(text: &str, bit: usize) -> bool {
    let words: Vec<u64> = text
        .split_whitespace()
        .map(|w| u64::from_str_radix(w, 16).unwrap_or(0))
        .collect();
    let word = bit / 64;
    word < words.len() && (words[words.len() - 1 - word] >> (bit % 64)) & 1 == 1
}

fn read_names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn node_at(dir: &Path, name: &str) -> Option<Node> {
    let dev = std::fs::read_to_string(dir.join("dev")).ok()?;
    let (major, minor) = dev.trim().split_once(':')?;
    Some(Node {
        name: name.to_string(),
        major: major.parse().ok()?,
        minor: minor.parse().ok()?,
    })
}

/// Waits for the kernel to bind `driver` (its name in sysfs) to the device and
/// make its nodes: a `hidraw` node and at least `inputs` input devices with
/// event nodes. Gives back what
/// there is when the time runs out, if that is anything.
pub fn wait_for_nodes(phys: &str, driver: &str, inputs: usize, within: Duration) -> Result<Found> {
    let deadline = Instant::now() + within;
    loop {
        let found = scan(phys);
        let events = found
            .inputs
            .iter()
            .filter(|i| i.nodes.iter().any(|n| n.name.starts_with("event")))
            .count();
        let ready =
            !found.hidraw.is_empty() && events >= inputs && found.driver.as_deref() == Some(driver);
        if ready {
            // The driver registers its input devices one after another; the
            // last may be a moment behind the first.
            std::thread::sleep(Duration::from_millis(200));
            return Ok(scan(phys));
        }
        if Instant::now() > deadline {
            if found.hidraw.is_empty() && events == 0 {
                bail!("the kernel made no nodes for {phys} (is {driver} loaded?)");
            }
            warn!(
                phys,
                driver = ?found.driver,
                hidraw = found.hidraw.len(),
                input_devices = events,
                "uhid device is not as expected (is {driver} loaded?)"
            );
            return Ok(found);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_the_kernels() {
        assert_eq!(std::mem::size_of::<Create2Req>(), 4372);
        assert_eq!(EVENT_SIZE, 4376);
        assert_eq!(std::mem::size_of::<Input2Req>(), 4098);
        assert_eq!(std::mem::size_of::<OutputReq>(), 4099);
        assert_eq!(std::mem::size_of::<GetReportReq>(), 6);
        assert_eq!(std::mem::size_of::<GetReportReplyReq>(), 4104);
        assert_eq!(std::mem::size_of::<SetReportReq>(), 4104);
        assert_eq!(std::mem::size_of::<SetReportReplyReq>(), 6);
        assert_eq!(std::mem::offset_of!(Create2Req, rd_size), 256);
        assert_eq!(std::mem::offset_of!(Create2Req, rd_data), 276);
    }

    #[test]
    fn create2_carries_the_identity_and_only_the_used_descriptor() {
        let params = Params {
            name: "Pad",
            phys: "cha/pad0/x",
            uniq: "u",
            bus: BUS_BLUETOOTH,
            vendor: 0x054c,
            product: 0x0ce6,
            version: 0x0100,
            descriptor: &[1, 2, 3],
        };
        let bytes = create2(&params).unwrap();
        assert_eq!(bytes.len(), 276 + 3);
        assert_eq!(&bytes[..4], b"Pad\0");
        assert_eq!(&bytes[128..139], b"cha/pad0/x\0");
        assert_eq!(&bytes[256..258], &3u16.to_le_bytes());
        assert_eq!(&bytes[258..260], &BUS_BLUETOOTH.to_le_bytes());
        assert_eq!(&bytes[260..264], &0x054cu32.to_le_bytes());
        assert_eq!(&bytes[276..], &[1, 2, 3]);
    }

    #[test]
    fn sysfs_bitmaps_read_most_significant_word_first() {
        // INPUT_PROP_ACCELEROMETER is bit 6; ABS_MT_POSITION_X bit 0x35 (53).
        assert!(hex_bit("40\n", 6));
        assert!(!hex_bit("40\n", 5));
        assert!(hex_bit("0 20000000000000\n", 53));
        assert!(!hex_bit("0 20000000000000\n", 54));
        assert!(hex_bit("1 0\n", 64));
    }
}
