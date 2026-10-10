//! The Linux side: vhci_hcd's sysfs, hidraw discovery, uhid, and the bench.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::device::{self, REPORT_DESCRIPTOR, REPORT_ID_BENCH};
use crate::engine::Engine;
use crate::usbip::{self, HEADER_LEN};

pub const VHCI: &str = "/sys/devices/platform/vhci_hcd.0";
/// `USB_SPEED_FULL` in `enum usb_device_speed`.
const SPEED_FULL: u32 = 2;
/// `VDEV_ST_NULL` and `VDEV_ST_USED` in `status`' `sta` column.
const STA_NULL: u32 = 4;
const STA_USED: u32 = 6;
/// busnum 1, devnum 2.
const DEVID: u32 = (1 << 16) | 2;

pub fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn sleep_until(deadline_ns: u64) {
    let ts = libc::timespec {
        tv_sec: (deadline_ns / 1_000_000_000) as libc::time_t,
        tv_nsec: (deadline_ns % 1_000_000_000) as libc::c_long,
    };
    // SAFETY: valid timespec; absolute CLOCK_MONOTONIC sleep, EINTR just returns early.
    unsafe {
        libc::clock_nanosleep(
            libc::CLOCK_MONOTONIC,
            libc::TIMER_ABSTIME,
            &ts,
            std::ptr::null_mut(),
        )
    };
}

// ---- vhci_hcd ----

/// Rows of `status`: (hub, port, sta).
fn status_rows() -> io::Result<Vec<(String, u32, u32)>> {
    let text = fs::read_to_string(format!("{VHCI}/status"))?;
    Ok(text
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let hub = f.next()?.to_string();
            let port = f.next()?.parse().ok()?;
            let sta = f.next()?.parse().ok()?;
            Some((hub, port, sta))
        })
        .collect())
}

/// A free port on the high-speed hub (full and high speed devices go there).
pub fn find_free_port() -> io::Result<u32> {
    status_rows()?
        .into_iter()
        .find(|(hub, _, sta)| hub == "hs" && *sta == STA_NULL)
        .map(|(_, port, _)| port)
        .ok_or_else(|| io::Error::other("no free hs port in vhci_hcd status"))
}

fn sysfs_write(path: &str, text: &str) -> io::Result<()> {
    let mut f = OpenOptions::new().write(true).open(path)?;
    f.write_all(text.as_bytes())
}

/// Attach a socket to the port; the kernel keeps its own reference.
pub fn attach(port: u32, sockfd: i32) -> io::Result<()> {
    sysfs_write(
        &format!("{VHCI}/attach"),
        &format!("{port} {sockfd} {DEVID} {SPEED_FULL}"),
    )
}

pub fn detach(port: u32) -> io::Result<()> {
    sysfs_write(&format!("{VHCI}/detach"), &port.to_string())
}

pub fn wait_used(port: u32, timeout: Duration) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if status_rows().is_ok_and(|r| {
            r.iter()
                .any(|(h, p, s)| h == "hs" && *p == port && *s == STA_USED)
        }) {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

// ---- hidraw ----

pub fn hidraw_names() -> HashSet<String> {
    fs::read_dir("/sys/class/hidraw")
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    /// The HID device's real sysfs path.
    pub hid_path: PathBuf,
}

/// A new hidraw node (not in `exclude`) with our HID id whose sysfs path
/// contains `via` (`vhci_hcd` or `uhid`).
pub fn find_hidraw(exclude: &HashSet<String>, via: &str, timeout: Duration) -> Option<Node> {
    let want = format!("HID_ID=0003:{:08X}:{:08X}", device::VENDOR, device::PRODUCT);
    let end = Instant::now() + timeout;
    loop {
        for name in hidraw_names().difference(exclude) {
            let Ok(path) = fs::canonicalize(format!("/sys/class/hidraw/{name}/device")) else {
                continue;
            };
            let uevent = fs::read_to_string(path.join("uevent")).unwrap_or_default();
            if uevent.lines().any(|l| l == want) && path.to_string_lossy().contains(via) {
                return Some(Node {
                    name: name.clone(),
                    hid_path: path,
                });
            }
        }
        if Instant::now() >= end {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn read_trim(p: &Path) -> String {
    fs::read_to_string(p)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Enumeration evidence from sysfs, as `key: value` pairs.
pub fn evidence(node: &Node) -> Vec<(String, String)> {
    let mut v = vec![
        ("hidraw".to_string(), format!("/dev/{}", node.name)),
        ("hid_path".to_string(), node.hid_path.display().to_string()),
    ];
    if let Some(iface) = node.hid_path.parent() {
        let driver = fs::read_link(iface.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
        v.push(("interface".into(), iface.display().to_string()));
        v.push(("interface_driver".into(), driver.unwrap_or_default()));
        v.push((
            "bInterfaceNumber".into(),
            read_trim(&iface.join("bInterfaceNumber")),
        ));
        v.push((
            "bInterfaceClass".into(),
            read_trim(&iface.join("bInterfaceClass")),
        ));
        if let Some(dev) = iface.parent() {
            for k in [
                "idVendor",
                "idProduct",
                "bcdDevice",
                "manufacturer",
                "product",
                "serial",
                "speed",
                "devnum",
                "bConfigurationValue",
            ] {
                v.push((k.to_string(), read_trim(&dev.join(k))));
            }
        }
    }
    if let Ok(uevent) = fs::read_to_string(node.hid_path.join("uevent")) {
        for l in uevent
            .lines()
            .filter(|l| l.starts_with("HID_DRIVER") || l.starts_with("HID_NAME"))
        {
            if let Some((k, val)) = l.split_once('=') {
                v.push((k.to_string(), val.to_string()));
            }
        }
    }
    v
}

// ---- the two backends ----

pub trait Backend {
    /// Deliver one input report to the kernel.
    fn send(&mut self, report: &[u8]) -> io::Result<()>;
    /// The sysfs path fragment that identifies this backend's nodes.
    fn via(&self) -> &'static str;
}

struct Shared {
    engine: Engine,
    sock: UnixStream,
}

/// The vhci loopback: we serve USB/IP on one end of a socketpair.
pub struct Loopback {
    shared: Arc<Mutex<Shared>>,
    pub port: u32,
    server: Option<JoinHandle<()>>,
}

impl Loopback {
    pub fn attach() -> io::Result<Self> {
        let (ours, theirs) = UnixStream::pair()?;
        let port = find_free_port()?;
        attach(port, theirs.as_raw_fd())?;
        drop(theirs); // the kernel holds its own reference
        let read_side = ours.try_clone()?;
        let shared = Arc::new(Mutex::new(Shared {
            engine: Engine::new(),
            sock: ours,
        }));
        let server = {
            let shared = shared.clone();
            thread::Builder::new()
                .name("usbip-rx".into())
                .spawn(move || serve(read_side, &shared))?
        };
        Ok(Self {
            shared,
            port,
            server: Some(server),
        })
    }

    pub fn dropped(&self) -> u64 {
        self.shared.lock().expect("lock").engine.dropped
    }
}

fn serve(mut sock: UnixStream, shared: &Mutex<Shared>) {
    let mut out = Vec::new();
    loop {
        let mut head = [0u8; HEADER_LEN];
        if sock.read_exact(&mut head).is_err() {
            return;
        }
        let cmd = match usbip::parse_header(&head) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("usbip: {e}");
                return;
            }
        };
        let mut body = vec![0u8; usbip::body_len(&cmd)];
        if sock.read_exact(&mut body).is_err() {
            return;
        }
        let mut g = shared.lock().expect("lock");
        out.clear();
        g.engine.on_command(&cmd, &body, &mut out);
        if !out.is_empty() && g.sock.write_all(&out).is_err() {
            return;
        }
    }
}

impl Backend for Loopback {
    fn send(&mut self, report: &[u8]) -> io::Result<()> {
        let mut g = self.shared.lock().expect("lock");
        let mut out = Vec::with_capacity(HEADER_LEN + report.len());
        g.engine.push_report(report, &mut out);
        if out.is_empty() {
            Ok(())
        } else {
            g.sock.write_all(&out)
        }
    }

    fn via(&self) -> &'static str {
        "vhci_hcd"
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        if let Err(e) = detach(self.port) {
            eprintln!("detach {}: {e}", self.port);
        }
        if let Some(h) = self.server.take() {
            let _ = h.join();
        }
    }
}

const UHID_DESTROY: u32 = 1;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const BUS_USB: u16 = 3;

/// The same device through /dev/uhid.
pub struct Uhid {
    file: File,
}

impl Uhid {
    pub fn create() -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open("/dev/uhid")?;
        let mut ev = Vec::new();
        ev.extend_from_slice(&UHID_CREATE2.to_le_bytes());
        for (text, size) in [
            ("S9 uhid Steam Controller", 128),
            ("s9-usbip-loopback/uhid", 64),
            (device::SERIAL, 64),
        ] {
            let mut field = vec![0u8; size];
            field[..text.len()].copy_from_slice(text.as_bytes());
            ev.extend_from_slice(&field);
        }
        ev.extend_from_slice(&(REPORT_DESCRIPTOR.len() as u16).to_le_bytes());
        ev.extend_from_slice(&BUS_USB.to_le_bytes());
        for v in [
            u32::from(device::VENDOR),
            u32::from(device::PRODUCT),
            u32::from(device::BCD_DEVICE),
            0,
        ] {
            ev.extend_from_slice(&v.to_le_bytes());
        }
        ev.extend_from_slice(REPORT_DESCRIPTOR);
        (&file).write_all(&ev)?;
        Ok(Self { file })
    }
}

impl Backend for Uhid {
    fn send(&mut self, report: &[u8]) -> io::Result<()> {
        let mut ev = Vec::with_capacity(6 + report.len());
        ev.extend_from_slice(&UHID_INPUT2.to_le_bytes());
        ev.extend_from_slice(&(report.len() as u16).to_le_bytes());
        ev.extend_from_slice(report);
        (&self.file).write_all(&ev)
    }

    fn via(&self) -> &'static str {
        "uhid"
    }
}

impl Drop for Uhid {
    fn drop(&mut self) {
        let _ = (&self.file).write_all(&UHID_DESTROY.to_le_bytes());
    }
}

// ---- bench ----

pub fn bench_report(seq: u32, ts_ns: u64) -> [u8; device::BENCH_REPORT_LEN] {
    let mut r = [0u8; device::BENCH_REPORT_LEN];
    r[0] = REPORT_ID_BENCH;
    r[1..5].copy_from_slice(&seq.to_le_bytes());
    r[5..13].copy_from_slice(&ts_ns.to_le_bytes());
    r
}

pub fn parse_bench_report(buf: &[u8]) -> Option<(u32, u64)> {
    if buf.len() < 13 || buf[0] != REPORT_ID_BENCH {
        return None;
    }
    Some((
        u32::from_le_bytes(buf[1..5].try_into().ok()?),
        u64::from_le_bytes(buf[5..13].try_into().ok()?),
    ))
}

fn open_hidraw(name: &str) -> io::Result<OwnedFd> {
    let path = std::ffi::CString::new(format!("/dev/{name}")).expect("no NUL");
    // SAFETY: valid C string; the fd is wrapped right away.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a fresh, owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Read bench reports until `stop`; returns (seq, latency ns).
fn reader(fd: OwnedFd, stop: Arc<AtomicBool>) -> Vec<(u32, u64)> {
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let mut p = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut p, 1, 20) };
        if ready > 0 {
            // SAFETY: buf is valid for its length.
            let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            let t = now_ns();
            if n > 0 {
                if let Some((seq, ts)) = parse_bench_report(&buf[..n as usize]) {
                    got.push((seq, t.saturating_sub(ts)));
                }
            } else if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return got;
            }
        } else if stop.load(Ordering::Relaxed) {
            return got;
        }
    }
}

#[derive(Debug, Default)]
pub struct BenchResult {
    pub sent: u32,
    pub received: u32,
    pub lost: u32,
    pub out_of_order: u32,
    pub duration_s: f64,
    pub delivered_hz: f64,
    pub lat_us: [f64; 5],  // min p50 p95 p99 max
    pub late_us: [f64; 2], // producer wake-up lateness p99 max
}

fn pct(sorted: &[u64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let at = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[at] as f64 / 1000.0
}

/// Produce reports at `rate` Hz for `seconds` into `backend`, read them back
/// from `node`'s hidraw, and compare.
pub fn run_bench(
    backend: &mut dyn Backend,
    node: &Node,
    rate: f64,
    seconds: f64,
) -> io::Result<BenchResult> {
    let fd = open_hidraw(&node.name)?;
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let stop = stop.clone();
        thread::Builder::new()
            .name("hidraw-rx".into())
            .spawn(move || reader(fd, stop))?
    };
    thread::sleep(Duration::from_millis(400)); // usbhid submits its IN URB once opened
    let total = (rate * seconds).round() as u32;
    let period = (1e9 / rate) as u64;
    let start = now_ns() + 1_000_000;
    let mut late = Vec::with_capacity(total as usize);
    for seq in 0..total {
        let due = start + u64::from(seq) * period;
        sleep_until(due);
        let t = now_ns();
        late.push(t.saturating_sub(due));
        backend.send(&bench_report(seq, t))?;
    }
    thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);
    let got = handle
        .join()
        .map_err(|_| io::Error::other("reader panicked"))?;

    let mut seen = HashSet::new();
    let mut lat: Vec<u64> = Vec::new();
    let (mut ooo, mut highest) = (0, None::<u32>);
    for (seq, l) in &got {
        if seen.insert(*seq) {
            lat.push(*l);
        }
        match highest {
            Some(h) if *seq < h => ooo += 1,
            _ => highest = Some(*seq),
        }
    }
    lat.sort_unstable();
    late.sort_unstable();
    let received = seen.len() as u32;
    Ok(BenchResult {
        sent: total,
        received,
        lost: total.saturating_sub(received),
        out_of_order: ooo,
        duration_s: seconds,
        delivered_hz: f64::from(received) / seconds,
        lat_us: [
            pct(&lat, 0.0),
            pct(&lat, 0.5),
            pct(&lat, 0.95),
            pct(&lat, 0.99),
            pct(&lat, 1.0),
        ],
        late_us: [pct(&late, 0.99), pct(&late, 1.0)],
    })
}
