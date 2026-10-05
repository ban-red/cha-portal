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
//!
//! Rumble goes the other way: the pads advertise force feedback, a thread per
//! pad answers the kernel's uinput upload/erase requests for the app's
//! effects, and the play/stop events become [`Rumble`]s on a broadcast channel
//! for the sessions to forward to the page.

use std::ffi::{CStr, c_int};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer};
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Pads one environment can have (the Gamepad API's usual four).
pub const MAX_PADS: usize = 4;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const EV_FF: u16 = 0x15;
const EV_UINPUT: u16 = 0x0101;
const SYN_REPORT: u16 = 0;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;

// What the kernel's memless force feedback (the xpad driver's) advertises: the
// rumble effect, and periodic ones it turns into rumble, plus the gain.
const FF_RUMBLE: u16 = 0x50;
const FF_PERIODIC: u16 = 0x51;
const FF_SQUARE: u16 = 0x58;
const FF_TRIANGLE: u16 = 0x59;
const FF_SINE: u16 = 0x5a;
const FF_GAIN: u16 = 0x60;
const UI_FF_UPLOAD: u16 = 1;
const UI_FF_ERASE: u16 = 2;

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
const UI_SET_FFBIT: libc::c_ulong = 0x4004_556b;
const UI_SET_PHYS: libc::c_ulong = 0x4008_556c;
const UI_BEGIN_FF_UPLOAD: libc::c_ulong = ioc(3, 200, std::mem::size_of::<UinputFfUpload>());
const UI_END_FF_UPLOAD: libc::c_ulong = ioc(1, 201, std::mem::size_of::<UinputFfUpload>());
const UI_BEGIN_FF_ERASE: libc::c_ulong = ioc(3, 202, std::mem::size_of::<UinputFfErase>());
const UI_END_FF_ERASE: libc::c_ulong = ioc(1, 203, std::mem::size_of::<UinputFfErase>());
/// `_IOC(dir, 'U', nr, size)`: dir 1 is write, 3 read and write.
const fn ioc(dir: libc::c_ulong, nr: libc::c_ulong, size: usize) -> libc::c_ulong {
    (dir << 30) | ((size as libc::c_ulong) << 16) | (0x55 << 8) | nr
}
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

/// `struct ff_effect`'s union, as the kernel lays it out on 64-bit: its
/// largest member, `ff_periodic_effect`, ends in a pointer. Rumble effects
/// have two u16 magnitudes (strong, weak) first; periodic ones a u16
/// waveform and period, then an s16 magnitude.
#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
struct FfUnion([u8; 32]);

impl FfUnion {
    fn u16_at(&self, at: usize) -> u16 {
        u16::from_ne_bytes([self.0[at], self.0[at + 1]])
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FfEffect {
    kind: u16,
    id: i16,
    direction: u16,
    trigger_button: u16,
    trigger_interval: u16,
    replay_length: u16,
    replay_delay: u16,
    u: FfUnion,
}

#[repr(C)]
#[derive(Default)]
struct UinputFfUpload {
    request_id: u32,
    retval: i32,
    effect: FfEffect,
    old: FfEffect,
}

#[repr(C)]
#[derive(Default)]
struct UinputFfErase {
    request_id: u32,
    retval: i32,
    effect_id: u32,
}

/// One pad's state from the page: `navigator.getGamepads()[i]`, standard
/// mapping (buttons 0..1, axes -1..1), with what the richer pads have on top.
/// Those extras are optional and kept for the DualSense and Steam Controller
/// devices to come (uhid); the Xbox 360 pad has no use for them, and a field
/// the page gets wrong never costs the buttons.
#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
pub struct PadState {
    pub i: usize,
    #[serde(default)]
    pub b: Vec<f32>,
    #[serde(default)]
    pub a: Vec<f32>,
    /// What the page recognized it as.
    /// `ty`, not `t`: the control channel's envelope is `{"t":"input", …}`.
    #[serde(default, rename = "ty", deserialize_with = "lenient")]
    pub kind: Option<PadKind>,
    /// Angular rate, rad/s.
    #[serde(default, deserialize_with = "lenient")]
    pub gyro: Option<[f32; 3]>,
    /// Acceleration, m/s².
    #[serde(default, deserialize_with = "lenient")]
    pub accel: Option<[f32; 3]>,
    #[serde(default, deserialize_with = "lenient")]
    pub touch: Vec<Touch>,
    /// Battery level, 0..1.
    #[serde(default, deserialize_with = "lenient")]
    pub bat: Option<f32>,
    /// The browser lost it: back to rest.
    #[serde(default)]
    pub gone: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PadKind {
    Xbox,
    Playstation,
    Switch,
    Steam,
    #[serde(other)]
    Generic,
}

/// A finger on a pad's touchpad, 0..1 across it.
#[derive(Debug, Clone, Copy, Deserialize)]
#[allow(dead_code)]
pub struct Touch {
    pub id: u32,
    pub x: f32,
    pub y: f32,
    pub down: bool,
}

/// The value, or its default when the page sent something else.
fn lenient<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// What an app asked a pad's motors to do: `lo` is the strong, low-frequency
/// one, `hi` the weak one (both 0..1), for `ms` milliseconds; 0 stops them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rumble {
    pub slot: usize,
    pub lo: f32,
    pub hi: f32,
    pub ms: u32,
}

/// How long an effect with no length (it plays until stopped) is announced
/// for; the stop comes when the app sends it.
const ENDLESS_MS: u32 = 60_000;

pub struct Gamepads {
    uinput: PathBuf,
    dev_dir: PathBuf,
    udev_dir: PathBuf,
    app_uid: Option<u32>,
    pads: Mutex<Vec<Pad>>,
    rumble: broadcast::Sender<Rumble>,
}

struct Pad {
    fd: OwnedFd,
    /// Ends the thread that answers the kernel's force feedback requests.
    stop: Arc<AtomicBool>,
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
            rumble: broadcast::channel(64).0,
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

    /// What the apps ask the pads' motors to do.
    pub fn rumble(&self) -> broadcast::Receiver<Rumble> {
        self.rumble.subscribe()
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
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.uinput)
            .with_context(|| format!("opening {}", self.uinput.display()))?;
        let fd: OwnedFd = file.into();
        let raw = fd.as_raw_fd();
        ioctl_int(raw, UI_SET_EVBIT, EV_KEY.into())?;
        ioctl_int(raw, UI_SET_EVBIT, EV_ABS.into())?;
        ioctl_int(raw, UI_SET_EVBIT, EV_FF.into())?;
        for code in [
            FF_RUMBLE,
            FF_PERIODIC,
            FF_SQUARE,
            FF_TRIANGLE,
            FF_SINE,
            FF_GAIN,
        ] {
            ioctl_int(raw, UI_SET_FFBIT, code.into())?;
        }
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
            ff_effects_max: 16,
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
        let stop = Arc::new(AtomicBool::new(false));
        // The thread reads through its own descriptor on the same device.
        // SAFETY: dup returns a new descriptor we own, or -1.
        let reader = unsafe { libc::dup(raw) };
        if reader < 0 {
            return Err(std::io::Error::last_os_error()).context("dup");
        }
        // SAFETY: just made, and nothing else owns it.
        let reader = unsafe { OwnedFd::from_raw_fd(reader) };
        let (tx, flag) = (self.rumble.clone(), Arc::clone(&stop));
        std::thread::Builder::new()
            .name(format!("pad{index}-ff"))
            .spawn(move || serve_force_feedback(reader, index, &tx, &flag))
            .context("spawning the force feedback thread")?;
        Ok(Pad {
            fd,
            stop,
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

impl Drop for Pad {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
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

/// One uploaded effect, reduced to what two motors can play.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Effect {
    strong: u16,
    weak: u16,
    /// 0: until stopped.
    length_ms: u16,
    delay_ms: u16,
}

impl Effect {
    /// None for a kind we didn't advertise.
    fn from_ff(effect: &FfEffect) -> Option<Self> {
        let (strong, weak) = match effect.kind {
            FF_RUMBLE => (effect.u.u16_at(0), effect.u.u16_at(2)),
            // The kernel's memless layer drives both motors from a periodic
            // effect's magnitude (s16, so half the u16 range).
            FF_PERIODIC => {
                let magnitude = i16::from_ne_bytes([effect.u.0[4], effect.u.0[5]]);
                let level = (u32::from(magnitude.unsigned_abs()) * 2).min(0xffff) as u16;
                (level, level)
            }
            _ => return None,
        };
        Some(Self {
            strong,
            weak,
            length_ms: effect.replay_length,
            delay_ms: effect.replay_delay,
        })
    }
}

/// One pad's effects as the app uploads, plays and erases them, and the
/// motors' combined drive at any moment. Pure: times come from the caller.
struct Effects {
    slot: usize,
    stored: Vec<(i16, Effect)>,
    /// Effect id and the millisecond it ends (`u64::MAX`: when stopped).
    playing: Vec<(i16, u64)>,
    /// FF_GAIN, 0..0xffff.
    gain: u16,
}

impl Effects {
    fn new(slot: usize) -> Self {
        Self {
            slot,
            stored: Vec::new(),
            playing: Vec::new(),
            gain: 0xffff,
        }
    }

    /// Stores or replaces an effect; false if it can't be played here.
    fn upload(&mut self, effect: &FfEffect) -> bool {
        let Some(stored) = Effect::from_ff(effect) else {
            return false;
        };
        match self.stored.iter_mut().find(|(id, _)| *id == effect.id) {
            Some((_, e)) => *e = stored,
            None => self.stored.push((effect.id, stored)),
        }
        true
    }

    /// Forgets an effect, stopping it; the motors' drive afterwards.
    fn erase(&mut self, id: i16, now_ms: u64) -> Rumble {
        self.stored.retain(|(i, _)| *i != id);
        self.playing.retain(|(i, _)| *i != id);
        self.current(now_ms)
    }

    /// A play (`count` repeats) or stop (0) of an effect; the motors' drive
    /// afterwards.
    fn play(&mut self, id: i16, count: i32, now_ms: u64) -> Rumble {
        self.playing.retain(|(i, _)| *i != id);
        let stored = self.stored.iter().find(|(i, _)| *i == id).map(|(_, e)| *e);
        if let Some(effect) = stored.filter(|_| count > 0) {
            let until = if effect.length_ms == 0 {
                u64::MAX
            } else {
                let run = u64::from(effect.length_ms) * count as u64;
                now_ms + u64::from(effect.delay_ms) + run
            };
            self.playing.push((id, until));
        }
        self.current(now_ms)
    }

    fn set_gain(&mut self, gain: i32) {
        self.gain = gain.clamp(0, 0xffff) as u16;
    }

    /// What the playing effects add up to, with the gain applied. All
    /// stopped or run out: the stop.
    fn current(&mut self, now_ms: u64) -> Rumble {
        self.playing.retain(|&(_, until)| until > now_ms);
        let (mut strong, mut weak, mut until) = (0u32, 0u32, 0u64);
        for &(id, end) in &self.playing {
            if let Some((_, e)) = self.stored.iter().find(|(i, _)| *i == id) {
                strong += u32::from(e.strong);
                weak += u32::from(e.weak);
                until = until.max(end);
            }
        }
        let ms = match until {
            0 => 0,
            u64::MAX => ENDLESS_MS,
            end => (end - now_ms) as u32,
        };
        let level = |motor: u32| {
            let motor = motor.min(0xffff) as f32 / 65535.0;
            motor * f32::from(self.gain) / 65535.0
        };
        Rumble {
            slot: self.slot,
            lo: level(strong),
            hi: level(weak),
            ms,
        }
    }
}

/// Answers the kernel's requests on a pad's uinput descriptor until the pad
/// is dropped: effects uploaded and erased by the app (the app's `write` or
/// ioctl waits for the answer), and the plays, stops and gain it sends.
fn serve_force_feedback(
    fd: OwnedFd,
    slot: usize,
    tx: &broadcast::Sender<Rumble>,
    stop: &AtomicBool,
) {
    let raw = fd.as_raw_fd();
    let start = Instant::now();
    let mut effects = Effects::new(slot);
    let mut buf: [InputEvent; 16] = std::array::from_fn(|_| event(0, 0, 0));
    while !stop.load(Ordering::Relaxed) {
        let mut poll = libc::pollfd {
            fd: raw,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut poll, 1, 250) } <= 0 {
            continue;
        }
        // SAFETY: the buffer holds 16 repr(C) events; the kernel writes whole ones.
        let n = unsafe {
            libc::read(
                raw,
                buf.as_mut_ptr().cast(),
                std::mem::size_of_val(&buf[..]),
            )
        };
        if n < 0 {
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock {
                warn!(pad = slot, "force feedback reading ended");
                return;
            }
            continue;
        }
        for ev in &buf[..n as usize / std::mem::size_of::<InputEvent>()] {
            let now_ms = start.elapsed().as_millis() as u64;
            let rumble = match (ev.kind, ev.code) {
                (EV_UINPUT, UI_FF_UPLOAD) => {
                    let mut up = UinputFfUpload {
                        request_id: ev.value as u32,
                        ..UinputFfUpload::default()
                    };
                    // SAFETY: a uinput_ff_upload, as the request says.
                    if unsafe { libc::ioctl(raw, UI_BEGIN_FF_UPLOAD, &mut up) } < 0 {
                        continue;
                    }
                    up.retval = if effects.upload(&up.effect) {
                        0
                    } else {
                        -libc::EINVAL
                    };
                    // SAFETY: as above.
                    unsafe { libc::ioctl(raw, UI_END_FF_UPLOAD, &up) };
                    None
                }
                (EV_UINPUT, UI_FF_ERASE) => {
                    let mut erase = UinputFfErase {
                        request_id: ev.value as u32,
                        ..UinputFfErase::default()
                    };
                    // SAFETY: a uinput_ff_erase, as the request says.
                    if unsafe { libc::ioctl(raw, UI_BEGIN_FF_ERASE, &mut erase) } < 0 {
                        continue;
                    }
                    let rumble = effects.erase(erase.effect_id as i16, now_ms);
                    erase.retval = 0;
                    // SAFETY: as above.
                    unsafe { libc::ioctl(raw, UI_END_FF_ERASE, &erase) };
                    Some(rumble)
                }
                (EV_FF, FF_GAIN) => {
                    effects.set_gain(ev.value);
                    None
                }
                (EV_FF, id) => Some(effects.play(id as i16, ev.value, now_ms)),
                _ => None,
            };
            if let Some(rumble) = rumble {
                // No session listening is fine.
                let _ = tx.send(rumble);
            }
        }
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
        // ff_effect: a 14-byte header, then the union at its 8-byte alignment.
        assert_eq!(std::mem::size_of::<FfEffect>(), 48);
        assert_eq!(std::mem::offset_of!(FfEffect, u), 16);
        assert_eq!(std::mem::size_of::<UinputFfUpload>(), 104);
        assert_eq!(std::mem::size_of::<UinputFfErase>(), 12);
        // As the kernel's headers spell them.
        assert_eq!(UI_BEGIN_FF_UPLOAD, 0xc068_55c8);
        assert_eq!(UI_END_FF_UPLOAD, 0x4068_55c9);
        assert_eq!(UI_BEGIN_FF_ERASE, 0xc00c_55ca);
        assert_eq!(UI_END_FF_ERASE, 0x400c_55cb);
    }

    fn rumble_effect(id: i16, strong: u16, weak: u16, length: u16) -> FfEffect {
        let mut e = FfEffect {
            kind: FF_RUMBLE,
            id,
            replay_length: length,
            ..FfEffect::default()
        };
        e.u.0[0..2].copy_from_slice(&strong.to_ne_bytes());
        e.u.0[2..4].copy_from_slice(&weak.to_ne_bytes());
        e
    }

    #[test]
    fn effects_upload_play_stop_erase() {
        let mut fx = Effects::new(2);
        assert!(fx.upload(&rumble_effect(0, 0xffff, 0, 500)));
        assert!(fx.upload(&rumble_effect(1, 0, 0x8000, 0)));
        let stop = Rumble {
            slot: 2,
            lo: 0.0,
            hi: 0.0,
            ms: 0,
        };
        assert_eq!(
            fx.play(0, 1, 1000),
            Rumble {
                slot: 2,
                lo: 1.0,
                hi: 0.0,
                ms: 500
            }
        );
        // Two playing add up; the longer one, endless, sets the duration.
        let both = fx.play(1, 1, 1100);
        assert_eq!((both.lo, both.ms), (1.0, ENDLESS_MS));
        assert!((both.hi - 0.5).abs() < 0.001);
        // The first has run out.
        let later = fx.current(1600);
        assert_eq!(later.lo, 0.0);
        assert!((later.hi - 0.5).abs() < 0.001);
        assert_eq!(fx.play(1, 0, 1700), stop);
        // Repeats lengthen it; erasing stops it; an unknown id plays nothing.
        assert_eq!(fx.play(0, 3, 2000).ms, 1500);
        assert_eq!(fx.erase(0, 2100), stop);
        assert_eq!(fx.play(0, 1, 2200), stop);
    }

    #[test]
    fn gain_scales_and_unplayable_effects_are_refused() {
        let mut fx = Effects::new(0);
        fx.set_gain(0x8000);
        fx.upload(&rumble_effect(3, 0xffff, 0xffff, 100));
        let r = fx.play(3, 1, 0);
        assert!((r.lo - 0.5).abs() < 0.001 && (r.hi - 0.5).abs() < 0.001);
        fx.set_gain(-5);
        assert_eq!(fx.current(0).lo, 0.0);
        // A sine periodic effect drives both motors from its magnitude.
        let mut periodic = FfEffect {
            kind: FF_PERIODIC,
            id: 4,
            replay_length: 200,
            ..FfEffect::default()
        };
        periodic.u.0[0..2].copy_from_slice(&FF_SINE.to_ne_bytes());
        periodic.u.0[4..6].copy_from_slice(&0x3fffi16.to_ne_bytes());
        fx.set_gain(0xffff);
        assert!(fx.upload(&periodic));
        let r = fx.play(4, 1, 0);
        assert!((r.lo - 1.0).abs() < 0.001 && r.lo == r.hi);
        let constant = FfEffect {
            kind: 0x52,
            id: 5,
            ..FfEffect::default()
        };
        assert!(!fx.upload(&constant));
    }

    #[test]
    fn the_extended_message_parses_and_an_old_one_still_does() {
        let new: PadState = serde_json::from_str(
            r#"{"k":"pad","i":1,"b":[1,0],"a":[0.5],"ty":"playstation",
                "gyro":[0.1,0.2,0.3],"accel":[0,9.8,0],
                "touch":[{"id":0,"x":0.25,"y":0.5,"down":true}],"bat":0.8}"#,
        )
        .unwrap();
        assert_eq!(new.i, 1);
        assert_eq!(new.kind, Some(PadKind::Playstation));
        assert_eq!(new.gyro, Some([0.1, 0.2, 0.3]));
        assert_eq!(new.accel, Some([0.0, 9.8, 0.0]));
        assert_eq!(new.touch.len(), 1);
        assert!(new.touch[0].down && new.touch[0].x == 0.25);
        assert_eq!(new.bat, Some(0.8));
        let old: PadState =
            serde_json::from_str(r#"{"k":"pad","i":0,"b":[1],"a":[0,0,0,0],"gone":true}"#).unwrap();
        assert!(old.gone && old.kind.is_none() && old.gyro.is_none() && old.touch.is_empty());
        // Unknown families and malformed extras don't cost the buttons.
        let odd: PadState = serde_json::from_str(
            r#"{"i":0,"b":[1],"ty":"atari","gyro":"fast","touch":5,"bat":null}"#,
        )
        .unwrap();
        assert_eq!(odd.kind, Some(PadKind::Generic));
        assert!(odd.gyro.is_none() && odd.touch.is_empty() && odd.bat.is_none());
        assert_eq!(odd.b, [1.0]);
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
            ..PadState::default()
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
