//! Gamepads: the browser's Gamepad API state → virtual Xbox 360 controllers
//! (uinput), the pad every game and SDL know.
//!
//! The kernel makes the devices on the host, outside the app's container, so
//! the app gets them through two volumes this streamer fills:
//! - `<input-dir>/dev`, the app's `/dev/input`: the pads' `eventN`/`jsN`
//!   nodes (the app's device cgroup allows input devices);
//! - `<input-dir>/udev`, the app's `/run/udev`: udev's database entries
//!   marking them joysticks (Chrome, Firefox and Wine find pads through udev).
//!
//! Hotplug events don't cross into the app's network namespace, so pads are
//! made ahead (`--gamepads`) and kept for the streamer's life; SDL, told to
//! skip udev, also watches `/dev/input` and sees later ones.

use std::ffi::{CStr, c_int};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tracing::{info, warn};

/// Pads one environment can have (the Gamepad API's usual four).
pub const MAX_PADS: usize = 4;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;

const BTN_A: u16 = 0x130;
const BTN_B: u16 = 0x131;
const BTN_X: u16 = 0x133;
const BTN_Y: u16 = 0x134;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c;
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;

/// The W3C standard mapping's buttons that are buttons here (the triggers are
/// axes, the d-pad a hat), by index.
const BUTTONS: [(usize, u16); 11] = [
    (0, BTN_A),
    (1, BTN_B),
    (2, BTN_X),
    (3, BTN_Y),
    (4, BTN_TL),
    (5, BTN_TR),
    (8, BTN_SELECT),
    (9, BTN_START),
    (10, BTN_THUMBL),
    (11, BTN_THUMBR),
    (16, BTN_MODE),
];

// <linux/uinput.h>
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
const UI_ABS_SETUP: libc::c_ulong = 0x401c_5504;
const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_SET_ABSBIT: libc::c_ulong = 0x4004_5567;
const UI_SET_PHYS: libc::c_ulong = 0x4008_556c;
const fn ui_get_sysname(len: usize) -> libc::c_ulong {
    (2 << 30) | ((len as libc::c_ulong) << 16) | 0x552c
}

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    absinfo: AbsInfo,
}

#[repr(C)]
struct InputEvent {
    time: libc::timeval,
    kind: u16,
    code: u16,
    value: i32,
}

/// One pad's state from the page: `navigator.getGamepads()[i]`, standard
/// mapping (buttons 0..1, axes -1..1).
#[derive(Debug, Deserialize)]
pub struct PadState {
    pub i: usize,
    #[serde(default)]
    pub b: Vec<f32>,
    #[serde(default)]
    pub a: Vec<f32>,
    /// The browser lost it: back to rest.
    #[serde(default)]
    pub gone: bool,
}

pub struct Gamepads {
    uinput: PathBuf,
    dev_dir: PathBuf,
    udev_dir: PathBuf,
    app_uid: Option<u32>,
    pads: Mutex<Vec<Pad>>,
}

struct Pad {
    fd: OwnedFd,
    /// Last values sent, by (type, code).
    sent: Vec<(u16, u16, i32)>,
}

impl Gamepads {
    /// Checks `/dev/uinput`, clears what an earlier run left in `input_dir`,
    /// and makes `ahead` pads.
    pub fn new(
        uinput: &Path,
        input_dir: &Path,
        app_uid: Option<u32>,
        ahead: usize,
    ) -> Result<Self> {
        OpenOptions::new()
            .write(true)
            .open(uinput)
            .with_context(|| format!("opening {}", uinput.display()))?;
        let pads = Self {
            uinput: uinput.to_path_buf(),
            dev_dir: input_dir.join("dev"),
            udev_dir: input_dir.join("udev/data"),
            app_uid,
            pads: Mutex::default(),
        };
        for dir in [&pads.dev_dir, &pads.udev_dir] {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            for entry in std::fs::read_dir(dir)?.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
        }
        for i in 0..ahead.min(MAX_PADS) {
            pads.ensure(i)?;
        }
        Ok(pads)
    }

    /// Applies one state message from the page, making the pad if needed.
    pub fn update(&self, state: &PadState) {
        if state.i >= MAX_PADS {
            return;
        }
        if let Err(err) = self.ensure(state.i) {
            warn!("gamepad {}: {err:#}", state.i);
            return;
        }
        let mut pads = self.pads.lock().expect("pads lock");
        let pad = &mut pads[state.i];
        if let Err(err) = pad.apply(&events(state)) {
            warn!("gamepad {}: {err:#}", state.i);
        }
    }

    fn ensure(&self, index: usize) -> Result<()> {
        let mut pads = self.pads.lock().expect("pads lock");
        while pads.len() <= index {
            let n = pads.len();
            pads.push(self.create(n)?);
        }
        Ok(())
    }

    fn create(&self, index: usize) -> Result<Pad> {
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.uinput)
            .with_context(|| format!("opening {}", self.uinput.display()))?;
        let fd: OwnedFd = file.into();
        let raw = fd.as_raw_fd();
        ioctl_int(raw, UI_SET_EVBIT, EV_KEY.into())?;
        ioctl_int(raw, UI_SET_EVBIT, EV_ABS.into())?;
        for (_, code) in BUTTONS {
            ioctl_int(raw, UI_SET_KEYBIT, code.into())?;
        }
        // As the xpad driver reports a 360 pad.
        let stick = AbsInfo {
            minimum: -32768,
            maximum: 32767,
            fuzz: 16,
            flat: 128,
            ..AbsInfo::default()
        };
        let trigger = AbsInfo {
            maximum: 255,
            ..AbsInfo::default()
        };
        let hat = AbsInfo {
            minimum: -1,
            maximum: 1,
            ..AbsInfo::default()
        };
        for (code, absinfo) in [
            (ABS_X, stick),
            (ABS_Y, stick),
            (ABS_RX, stick),
            (ABS_RY, stick),
            (ABS_Z, trigger),
            (ABS_RZ, trigger),
            (ABS_HAT0X, hat),
            (ABS_HAT0Y, hat),
        ] {
            ioctl_int(raw, UI_SET_ABSBIT, code.into())?;
            let setup = UinputAbsSetup { code, absinfo };
            // SAFETY: a valid uinput fd and a uinput_abs_setup.
            if unsafe { libc::ioctl(raw, UI_ABS_SETUP, &setup) } < 0 {
                return Err(std::io::Error::last_os_error()).context("UI_ABS_SETUP");
            }
        }
        let mut setup = UinputSetup {
            id: InputId {
                bustype: 0x03, // BUS_USB
                vendor: 0x045e,
                product: 0x028e,
                version: 0x0110,
            },
            name: [0; 80],
            ff_effects_max: 0,
        };
        // Where the host's udev rule (deploy/node/host) recognizes our pads.
        let phys = std::ffi::CString::new(format!("cha/pad{index}")).expect("no NUL");
        // SAFETY: a valid uinput fd and a NUL-terminated string.
        if unsafe { libc::ioctl(raw, UI_SET_PHYS, phys.as_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error()).context("UI_SET_PHYS");
        }
        let name = b"Microsoft X-Box 360 pad";
        setup.name[..name.len()].copy_from_slice(name);
        // SAFETY: a valid uinput fd and a uinput_setup.
        if unsafe { libc::ioctl(raw, UI_DEV_SETUP, &setup) } < 0 {
            return Err(std::io::Error::last_os_error()).context("UI_DEV_SETUP");
        }
        // SAFETY: as above; no argument.
        if unsafe { libc::ioctl(raw, UI_DEV_CREATE) } < 0 {
            return Err(std::io::Error::last_os_error()).context("UI_DEV_CREATE");
        }
        let mut sysname = [0u8; 64];
        // SAFETY: the buffer is as long as the length in the request.
        if unsafe { libc::ioctl(raw, ui_get_sysname(sysname.len()), sysname.as_mut_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error()).context("UI_GET_SYSNAME");
        }
        let sysname = CStr::from_bytes_until_nul(&sysname)
            .context("sysname")?
            .to_string_lossy()
            .into_owned();
        let nodes = self.share(&sysname)?;
        info!(pad = index, device = %sysname, ?nodes, "gamepad: virtual Xbox 360 controller");
        Ok(Pad {
            fd,
            sent: Vec::new(),
        })
    }

    /// Mirrors the device's nodes and udev entries into the app's volumes.
    fn share(&self, sysname: &str) -> Result<Vec<String>> {
        let sys = Path::new("/sys/devices/virtual/input").join(sysname);
        // The evdev and joydev handlers attach as the device registers;
        // allow them a moment.
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut nodes = Vec::new();
        while nodes.iter().all(|n: &String| !n.starts_with("event")) {
            nodes = std::fs::read_dir(&sys)
                .with_context(|| format!("reading {}", sys.display()))?
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with("event") || n.starts_with("js"))
                .collect();
            if Instant::now() > deadline {
                bail!("{sysname} has no event node");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let initialized = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros();
        for node in &nodes {
            let dev = std::fs::read_to_string(sys.join(node).join("dev"))?;
            let (major, minor) = dev
                .trim()
                .split_once(':')
                .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)))
                .context("parsing the device number")?;
            let path = self.dev_dir.join(node);
            let _ = std::fs::remove_file(&path);
            let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
            // SAFETY: a valid path; mknod creates a character device node.
            if unsafe {
                libc::mknod(
                    c_path.as_ptr(),
                    libc::S_IFCHR | 0o660,
                    libc::makedev(major, minor),
                )
            } < 0
            {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("mknod {}", path.display()));
            }
            if let Some(uid) = self.app_uid {
                std::os::unix::fs::chown(&path, Some(uid), Some(uid))?;
            }
            // Past the umask.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660))?;
            let mut db = std::fs::File::create(self.udev_dir.join(format!("c{major}:{minor}")))?;
            write!(
                db,
                "I:{initialized}\nE:ID_INPUT=1\nE:ID_INPUT_JOYSTICK=1\nE:ID_BUS=usb\n\
                 E:ID_VENDOR_ID=045e\nE:ID_MODEL_ID=028e\nE:ID_SERIAL=Microsoft_X-Box_360_pad\n\
                 G:seat\nG:uaccess\nQ:seat\nQ:uaccess\nV:1\n"
            )?;
        }
        Ok(nodes)
    }
}

impl Pad {
    fn apply(&mut self, events: &[(u16, u16, i32)]) -> Result<()> {
        let mut out: Vec<InputEvent> = Vec::new();
        for &(kind, code, value) in events {
            match self
                .sent
                .iter_mut()
                .find(|(k, c, _)| *k == kind && *c == code)
            {
                Some((_, _, v)) if *v == value => continue,
                Some((_, _, v)) => *v = value,
                None => self.sent.push((kind, code, value)),
            }
            out.push(event(kind, code, value));
        }
        if out.is_empty() {
            return Ok(());
        }
        out.push(event(EV_SYN, SYN_REPORT, 0));
        // SAFETY: plain repr(C) structs, written as the kernel reads them.
        let bytes = unsafe {
            std::slice::from_raw_parts(out.as_ptr().cast::<u8>(), std::mem::size_of_val(&out[..]))
        };
        let n = unsafe { libc::write(self.fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
        if n < 0 {
            return Err(std::io::Error::last_os_error()).context("writing events");
        }
        Ok(())
    }
}

fn event(kind: u16, code: u16, value: i32) -> InputEvent {
    InputEvent {
        // The kernel stamps events written to uinput.
        time: libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        kind,
        code,
        value,
    }
}

/// The page's state as evdev values.
fn events(state: &PadState) -> Vec<(u16, u16, i32)> {
    let b = |i: usize| {
        if state.gone {
            0.0
        } else {
            state.b.get(i).copied().unwrap_or(0.0)
        }
    };
    let a = |i: usize| {
        if state.gone {
            0.0
        } else {
            state.a.get(i).copied().unwrap_or(0.0)
        }
    };
    let stick = |v: f32| (v.clamp(-1.0, 1.0) * 32767.0).round() as i32;
    let pressed = |v: f32| i32::from(v > 0.5);
    let mut events: Vec<(u16, u16, i32)> = BUTTONS
        .iter()
        .map(|&(i, code)| (EV_KEY, code, pressed(b(i))))
        .collect();
    events.extend([
        (EV_ABS, ABS_X, stick(a(0))),
        (EV_ABS, ABS_Y, stick(a(1))),
        (EV_ABS, ABS_RX, stick(a(2))),
        (EV_ABS, ABS_RY, stick(a(3))),
        (EV_ABS, ABS_Z, (b(6).clamp(0.0, 1.0) * 255.0).round() as i32),
        (
            EV_ABS,
            ABS_RZ,
            (b(7).clamp(0.0, 1.0) * 255.0).round() as i32,
        ),
        (EV_ABS, ABS_HAT0X, pressed(b(15)) - pressed(b(14))),
        (EV_ABS, ABS_HAT0Y, pressed(b(13)) - pressed(b(12))),
    ]);
    events
}

fn ioctl_int(fd: c_int, request: libc::c_ulong, value: c_int) -> Result<()> {
    // SAFETY: the UI_SET_*BIT requests take an int by value.
    if unsafe { libc::ioctl(fd, request, value) } < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("ioctl {request:#x}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_the_ioctls() {
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
        assert_eq!(std::mem::size_of::<UinputAbsSetup>(), 28);
        assert_eq!(std::mem::size_of::<InputEvent>(), 24);
        assert_eq!((UI_DEV_SETUP >> 16) & 0x3fff, 92);
        assert_eq!((UI_ABS_SETUP >> 16) & 0x3fff, 28);
    }

    #[test]
    fn maps_the_standard_layout() {
        let mut b = vec![0.0; 17];
        b[0] = 1.0; // A
        b[7] = 0.5; // right trigger, half
        b[12] = 1.0; // d-pad up
        let state = PadState {
            i: 0,
            b,
            a: vec![-1.0, 0.5, 0.0, 0.0],
            gone: false,
        };
        let e = events(&state);
        let get = |kind, code| {
            e.iter()
                .find(|(k, c, _)| *k == kind && *c == code)
                .unwrap()
                .2
        };
        assert_eq!(get(EV_KEY, BTN_A), 1);
        assert_eq!(get(EV_KEY, BTN_B), 0);
        assert_eq!(get(EV_ABS, ABS_RZ), 128);
        assert_eq!(get(EV_ABS, ABS_HAT0Y), -1);
        assert_eq!(get(EV_ABS, ABS_X), -32767);
        assert_eq!(get(EV_ABS, ABS_Y), 16384);
        let gone = PadState {
            gone: true,
            ..state
        };
        assert!(events(&gone).iter().all(|(_, _, v)| *v == 0));
    }
}
