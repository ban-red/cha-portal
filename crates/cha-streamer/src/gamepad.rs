//! Gamepads: the browser's Gamepad API state → virtual controllers of the
//! kind chosen for the environment (`--pad-kind`):
//! - `xbox360` (uinput): the pad every game and SDL know;
//! - `dualsense` and `steam` (uhid): a DualSense or a Steam Controller (the
//!   2026 one, over Bluetooth), bound by the host's `hid-playstation` or, for the Steam
//!   Controller, `hid-generic` ([`crate::dualsense`],
//!   [`crate::steam_controller`], over [`crate::uhid`]); both have a `hidraw`
//!   node.
//!
//! The kernel makes the devices on the host, outside the app's container, so
//! the app gets them through two volumes this streamer fills:
//! - `<input-dir>/dev`, the app's `/dev/input`: the pads' `eventN`/`jsN`
//!   nodes (the app's device cgroup allows input devices), and for uhid pads
//!   their `hidraw` nodes in `hidraw/` (the node mounts each of those into the
//!   app at `/dev/<name>`);
//! - `<input-dir>/udev`, the app's `/run/udev`: udev's database entries
//!   marking them joysticks (Chrome, Firefox and Wine find pads through udev).
//!
//! Hotplug events don't cross into the app's network namespace, so pads are
//! made ahead (`--gamepads`) and kept for the streamer's life; SDL, told to
//! skip udev, also watches `/dev/input` and sees later ones.
//!
//! What apps do to the pads goes the other way, as [`PadEvent`]s on a
//! broadcast channel for the sessions to forward to the page. The Xbox pads
//! advertise force feedback: a thread per pad answers the kernel's uinput
//! upload/erase requests for the app's effects, and the play/stop events
//! become [`Rumble`]s. The uhid pads' output reports (rumble, lightbar, player
//! LEDs, adaptive triggers, trackpad haptics) are parsed by their protocols.
//!
//! The `steam` kind also serves Steam's own virtual Xbox pad, which Steam
//! makes through a fake `/dev/uinput`: [`crate::uinput_broker`] makes that
//! device and shares its nodes through [`Gamepads::share_uinput`].

use std::ffi::{CStr, c_int};
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::dualsense::{self, DualSense};
use crate::steam_controller::{self, SteamController};
use crate::uhid::{self, Device, Node};

/// Pads one environment can have (the Gamepad API's usual four).
pub const MAX_PADS: usize = 4;

pub(crate) const EV_SYN: u16 = 0x00;
pub(crate) const EV_KEY: u16 = 0x01;
pub(crate) const EV_ABS: u16 = 0x03;
pub(crate) const EV_FF: u16 = 0x15;
pub(crate) const EV_UINPUT: u16 = 0x0101;
pub(crate) const SYN_REPORT: u16 = 0;

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
pub(crate) const UI_FF_UPLOAD: u16 = 1;
pub(crate) const UI_FF_ERASE: u16 = 2;

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
pub(crate) const UI_DEV_CREATE: libc::c_ulong = 0x5501;
pub(crate) const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
pub(crate) const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
pub(crate) const UI_ABS_SETUP: libc::c_ulong = 0x401c_5504;
pub(crate) const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
pub(crate) const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
pub(crate) const UI_SET_ABSBIT: libc::c_ulong = 0x4004_5567;
pub(crate) const UI_SET_FFBIT: libc::c_ulong = 0x4004_556b;
pub(crate) const UI_SET_PHYS: libc::c_ulong = 0x4008_556c;
pub(crate) const UI_BEGIN_FF_UPLOAD: libc::c_ulong =
    ioc(3, 200, std::mem::size_of::<UinputFfUpload>());
pub(crate) const UI_END_FF_UPLOAD: libc::c_ulong =
    ioc(1, 201, std::mem::size_of::<UinputFfUpload>());
pub(crate) const UI_BEGIN_FF_ERASE: libc::c_ulong =
    ioc(3, 202, std::mem::size_of::<UinputFfErase>());
pub(crate) const UI_END_FF_ERASE: libc::c_ulong = ioc(1, 203, std::mem::size_of::<UinputFfErase>());
/// `_IOC(dir, 'U', nr, size)`: dir 1 is write, 3 read and write.
pub(crate) const fn ioc(dir: libc::c_ulong, nr: libc::c_ulong, size: usize) -> libc::c_ulong {
    (dir << 30) | ((size as libc::c_ulong) << 16) | (0x55 << 8) | nr
}
pub(crate) const fn ui_get_sysname(len: usize) -> libc::c_ulong {
    (2 << 30) | ((len as libc::c_ulong) << 16) | 0x552c
}

#[repr(C)]
pub(crate) struct InputId {
    pub(crate) bustype: u16,
    pub(crate) vendor: u16,
    pub(crate) product: u16,
    pub(crate) version: u16,
}

#[repr(C)]
pub(crate) struct UinputSetup {
    pub(crate) id: InputId,
    pub(crate) name: [u8; 80],
    pub(crate) ff_effects_max: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct AbsInfo {
    pub(crate) value: i32,
    pub(crate) minimum: i32,
    pub(crate) maximum: i32,
    pub(crate) fuzz: i32,
    pub(crate) flat: i32,
    pub(crate) resolution: i32,
}

#[repr(C)]
pub(crate) struct UinputAbsSetup {
    pub(crate) code: u16,
    pub(crate) absinfo: AbsInfo,
}

#[repr(C)]
pub(crate) struct InputEvent {
    pub(crate) time: libc::timeval,
    pub(crate) kind: u16,
    pub(crate) code: u16,
    pub(crate) value: i32,
}

/// `struct ff_effect`'s union, as the kernel lays it out on 64-bit: its
/// largest member, `ff_periodic_effect`, ends in a pointer. Rumble effects
/// have two u16 magnitudes (strong, weak) first; periodic ones a u16
/// waveform and period, then an s16 magnitude.
#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub(crate) struct FfUnion(pub(crate) [u8; 32]);

impl FfUnion {
    pub(crate) fn u16_at(&self, at: usize) -> u16 {
        u16::from_ne_bytes([self.0[at], self.0[at + 1]])
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct FfEffect {
    pub(crate) kind: u16,
    pub(crate) id: i16,
    pub(crate) direction: u16,
    pub(crate) trigger_button: u16,
    pub(crate) trigger_interval: u16,
    pub(crate) replay_length: u16,
    pub(crate) replay_delay: u16,
    pub(crate) u: FfUnion,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct UinputFfUpload {
    pub(crate) request_id: u32,
    pub(crate) retval: i32,
    pub(crate) effect: FfEffect,
    pub(crate) old: FfEffect,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct UinputFfErase {
    pub(crate) request_id: u32,
    pub(crate) retval: i32,
    pub(crate) effect_id: u32,
}

/// One pad's state from the page: `navigator.getGamepads()[i]`, standard
/// mapping (buttons 0..1, axes -1..1), with what the richer pads have on top.
/// Those extras are optional and kept for the DualSense and Steam Controller
/// devices to come (uhid); the Xbox 360 pad has no use for them, and a field
/// the page gets wrong never costs the buttons.
#[derive(Debug, Clone, Default, Deserialize)]
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
pub const ENDLESS_MS: u32 = 60_000;

/// Which of a controller's two sides (triggers, trackpads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    /// As the control channel spells it.
    pub fn name(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }
}

/// A trackpad pulse train an app asked for (Steam Controller haptics): `count`
/// pulses of `on_us` microseconds, `off_us` apart, at `amp` (0..1); a count of
/// 0 stops it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Haptic {
    pub slot: usize,
    pub side: Side,
    pub amp: f32,
    pub on_us: u32,
    pub off_us: u32,
    pub count: u32,
}

/// The lightbar's color (DualSense).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Led {
    pub slot: usize,
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// The player LEDs, bits 0..5 (DualSense).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Players {
    pub slot: usize,
    pub mask: u8,
}

/// An adaptive trigger effect (DualSense): the output report's block, the
/// effect's type and its ten parameters, as the game wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trigger {
    pub slot: usize,
    pub side: Side,
    pub effect: [u8; 11],
}

/// What an app does to a pad that the person holding the controller should
/// feel or see.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PadEvent {
    Rumble(Rumble),
    Haptic(Haptic),
    Led(Led),
    Players(Players),
    Trigger(Trigger),
}

/// The last value of each state-like pad event (lightbar, player LEDs,
/// trigger effects) per pad. Motors and haptics are moments, not state, and
/// aren't kept.
#[derive(Default)]
pub struct PadMemory {
    slots: [[Option<PadEvent>; EVENT_CLASSES]; MAX_PADS],
}

impl PadMemory {
    pub fn remember(&mut self, event: PadEvent) {
        if !matches!(
            event,
            PadEvent::Led(_) | PadEvent::Players(_) | PadEvent::Trigger(_)
        ) {
            return;
        }
        if let Some(slot) = self
            .slots
            .get_mut(event.slot())
            .and_then(|pad| pad.get_mut(event.class()))
        {
            *slot = Some(event);
        }
    }

    /// The kept events, pad by pad.
    pub fn replay(&self) -> Vec<PadEvent> {
        self.slots.iter().flatten().flatten().copied().collect()
    }
}

/// How many kinds of [`PadEvent`] a pad has that supersede each other.
pub const EVENT_CLASSES: usize = 7;

impl PadEvent {
    pub fn slot(&self) -> usize {
        match self {
            PadEvent::Rumble(e) => e.slot,
            PadEvent::Haptic(e) => e.slot,
            PadEvent::Led(e) => e.slot,
            PadEvent::Players(e) => e.slot,
            PadEvent::Trigger(e) => e.slot,
        }
    }

    /// What this replaces: a newer one of the same class (on a pad) makes it
    /// stale.
    pub fn class(&self) -> usize {
        match self {
            PadEvent::Rumble(_) => 0,
            PadEvent::Haptic(e) => 1 + usize::from(e.side == Side::Right),
            PadEvent::Led(_) => 3,
            PadEvent::Players(_) => 4,
            PadEvent::Trigger(e) => 5 + usize::from(e.side == Side::Right),
        }
    }

    /// Whether it ends something, and so must not wait behind a newer one.
    pub fn is_stop(&self) -> bool {
        match self {
            PadEvent::Rumble(e) => e.ms == 0,
            PadEvent::Haptic(e) => e.count == 0,
            _ => false,
        }
    }
}

/// The virtual controller an environment's pads are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GamepadKind {
    /// An Xbox 360 pad (uinput).
    #[value(name = "xbox360")]
    #[serde(rename = "xbox360")]
    Xbox360,
    /// A wired DualSense (uhid).
    #[value(name = "dualsense")]
    DualSense,
    /// A Bluetooth Steam Controller, the 2026 one (uhid).
    #[value(name = "steam")]
    Steam,
}

impl GamepadKind {
    pub fn name(self) -> &'static str {
        match self {
            GamepadKind::Xbox360 => "xbox360",
            GamepadKind::DualSense => "dualsense",
            GamepadKind::Steam => "steam",
        }
    }
}

/// A `hidraw` node of a uhid pad, in `<input-dir>/dev/hidraw/<name>`: what the
/// node mounts into the app at `/dev/<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HidrawNode {
    pub name: String,
    pub major: u32,
    pub minor: u32,
}

pub struct Gamepads {
    kind: GamepadKind,
    uinput: PathBuf,
    uhid: PathBuf,
    dev_dir: PathBuf,
    hidraw_dir: PathBuf,
    udev_dir: PathBuf,
    app_uid: Option<u32>,
    /// Makes this run's devices' MAC addresses and serials its own.
    seed: u64,
    /// Whose pads these are, the same on every launch (`CHA_PAD_IDENTITY`,
    /// from the node: owner and app). A Steam Controller's serial comes from
    /// it, so Steam, which keeps a configuration set per serial, finds last
    /// time's settings instead of meeting a new controller every launch.
    identity: Option<String>,
    pads: Mutex<Vec<Slot>>,
    events: broadcast::Sender<PadEvent>,
    /// The last lightbar, player LEDs and trigger effects, for viewers that
    /// join later.
    memory: Arc<Mutex<PadMemory>>,
}

/// One pad: a uinput device, or a uhid one.
enum Slot {
    Xbox(Pad),
    Hid(HidPad),
}

struct Pad {
    fd: OwnedFd,
    /// Ends the thread that answers the kernel's force feedback requests.
    stop: Arc<AtomicBool>,
    /// Last values sent, by (type, code).
    sent: Vec<(u16, u16, i32)>,
}

struct HidPad {
    device: Arc<Device>,
    hidraw: Vec<HidrawNode>,
}

impl Drop for HidPad {
    fn drop(&mut self) {
        self.device.destroy();
    }
}

/// What udev's database says about a device: how the app tells the pads from
/// other devices.
pub(crate) struct Identity {
    /// udev's `ID_BUS`.
    bus: &'static str,
    vendor: &'static str,
    product: &'static str,
    serial: &'static str,
}

const XBOX_360: Identity = Identity {
    bus: "usb",
    vendor: "045e",
    product: "028e",
    serial: "Microsoft_X-Box_360_pad",
};

const DUALSENSE: Identity = Identity {
    bus: "usb",
    vendor: "054c",
    product: "0ce6",
    serial: "Sony_Interactive_Entertainment_DualSense_Wireless_Controller",
};

/// The Xbox 360 pad Steam makes for games ([`crate::uinput_broker`]).
pub(crate) const STEAM_VIRTUAL_PAD: Identity = Identity {
    bus: "usb",
    vendor: "28de",
    product: "11ff",
    serial: "Valve_Software_Steam_Virtual_Gamepad",
};

const STEAM_CONTROLLER: Identity = Identity {
    bus: "bluetooth",
    vendor: "28de",
    product: "1303",
    serial: "Valve_Software_Steam_Controller",
};

/// What a node is, for its udev entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Joystick,
    Touchpad,
    Accelerometer,
    Hidraw,
}

/// How long the host's driver gets to bind a uhid device and make its nodes
/// (it may have to load the module first).
const BIND_WAIT: Duration = Duration::from_secs(5);

impl Gamepads {
    /// Checks the device files the kind needs, clears what an earlier run left
    /// in `input_dir`, and makes `ahead` pads. A DualSense or Steam Controller
    /// kind without a usable `uhid` is an Xbox 360 one, with a warning.
    pub fn new(
        uinput: &Path,
        uhid: &Path,
        mut kind: GamepadKind,
        input_dir: &Path,
        app_uid: Option<u32>,
        ahead: usize,
    ) -> Result<Self> {
        if kind != GamepadKind::Xbox360 {
            let usable = if uhid.as_os_str().is_empty() {
                Err(anyhow::anyhow!("no --uhid"))
            } else {
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(uhid)
                    .map(drop)
                    .with_context(|| format!("opening {}", uhid.display()))
            };
            if let Err(err) = usable {
                warn!(
                    "{} pads need uhid, using Xbox 360 ones: {err:#}",
                    kind.name()
                );
                kind = GamepadKind::Xbox360;
            }
        }
        if kind == GamepadKind::Xbox360 {
            OpenOptions::new()
                .write(true)
                .open(uinput)
                .with_context(|| format!("opening {}", uinput.display()))?;
        }
        let pads = Self {
            kind,
            uinput: uinput.to_path_buf(),
            uhid: uhid.to_path_buf(),
            dev_dir: input_dir.join("dev"),
            hidraw_dir: input_dir.join("dev/hidraw"),
            udev_dir: input_dir.join("udev/data"),
            app_uid,
            seed: run_seed(),
            identity: std::env::var(PAD_IDENTITY_ENV)
                .ok()
                .filter(|s| !s.trim().is_empty()),
            pads: Mutex::default(),
            events: broadcast::channel(64).0,
            memory: Arc::default(),
        };
        pads.remember_events();
        // A stale `hidraw/` goes first, whole.
        let _ = std::fs::remove_dir_all(&pads.hidraw_dir);
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

    pub fn kind(&self) -> GamepadKind {
        self.kind
    }

    /// Keeps `memory` current from the event stream; ends with the pads.
    fn remember_events(&self) {
        let mut rx = self.events.subscribe();
        let memory = Arc::clone(&self.memory);
        std::thread::spawn(move || {
            loop {
                match rx.blocking_recv() {
                    Ok(event) => memory
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remember(event),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    /// What a session that gains the controls should be told: the pads'
    /// lightbar, player LEDs and trigger effects as the apps last set them.
    pub fn memory(&self) -> Arc<Mutex<PadMemory>> {
        Arc::clone(&self.memory)
    }

    /// What the apps do to the pads: motors, lightbars, triggers, haptics.
    pub fn events(&self) -> broadcast::Receiver<PadEvent> {
        self.events.subscribe()
    }

    /// The `hidraw` nodes of the pads made so far (uhid kinds only).
    pub fn hidraw(&self) -> Vec<HidrawNode> {
        let pads = self.pads.lock().expect("pads lock");
        pads.iter()
            .filter_map(|slot| match slot {
                Slot::Hid(pad) => Some(pad.hidraw.clone()),
                Slot::Xbox(_) => None,
            })
            .flatten()
            .collect()
    }

    /// Puts every pad made so far back to rest: sticks centered, buttons up.
    pub fn release_all(&self) {
        let count = self.pads.lock().expect("pads lock").len();
        for i in 0..count {
            self.update(&PadState {
                i,
                gone: true,
                ..PadState::default()
            });
        }
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
        match &mut pads[state.i] {
            Slot::Xbox(pad) => {
                if let Err(err) = pad.apply(&events(state)) {
                    warn!("gamepad {}: {err:#}", state.i);
                }
            }
            Slot::Hid(pad) => pad.device.update(state),
        }
    }

    fn ensure(&self, index: usize) -> Result<()> {
        let mut pads = self.pads.lock().expect("pads lock");
        while pads.len() <= index {
            let n = pads.len();
            pads.push(match self.kind {
                GamepadKind::Xbox360 => Slot::Xbox(self.create_xbox(n)?),
                GamepadKind::DualSense | GamepadKind::Steam => Slot::Hid(self.create_hid(n)?),
            });
        }
        Ok(())
    }

    /// A number for this run's pad `index`, stable for the run.
    /// What a pad's serial is made from: stable per owner, app and slot when
    /// the node said whose pads these are, else the run's.
    fn serial_hash(&self, index: usize) -> u64 {
        match &self.identity {
            Some(identity) => stable_hash(&format!("{identity}/pad{index}")),
            None => self.pad_hash(index),
        }
    }

    fn pad_hash(&self, index: usize) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (self.seed, index).hash(&mut hasher);
        hasher.finish()
    }

    /// A uhid pad: the device, and once the host's driver has bound it, its
    /// nodes shared with the app.
    fn create_hid(&self, index: usize) -> Result<HidPad> {
        let hash = self.pad_hash(index);
        let bytes = hash.to_be_bytes();
        // The DualSense's "MAC address", which the kernel insists be one of a
        // kind among the host's pads: locally administered (02), our prefix (so
        // a host udev rule can tell our pads by their `uniq`), then three
        // bytes of the run's. A Steam Controller's `uniq` is one too (it is
        // Bluetooth); its serial is made as Valve formats them, from the run's.
        let mac = [0x02, 0xca, 0xfe, bytes[5], bytes[6], bytes[7]];
        let serial =
            steam_controller::serial_for(steam_controller::VARIANT, self.serial_hash(index));
        let tag = format!("{hash:016x}");
        // What the kernel keeps as the device's `phys` (and finds it by here).
        let phys = format!("cha/pad{index}/{tag}");
        let (name, bus, vendor, product, version, descriptor, uniq): (_, _, _, _, _, &[u8], _) =
            match self.kind {
                GamepadKind::DualSense => (
                    dualsense::NAME,
                    uhid::BUS_USB,
                    dualsense::VENDOR,
                    dualsense::PRODUCT,
                    dualsense::VERSION,
                    &dualsense::DESCRIPTOR,
                    mac.iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(":"),
                ),
                _ => (
                    steam_controller::VARIANT.name(),
                    steam_controller::VARIANT.bus(),
                    steam_controller::VENDOR,
                    steam_controller::VARIANT.product(),
                    steam_controller::VERSION,
                    steam_controller::VARIANT.descriptor(),
                    // The Bluetooth controller's `uniq` is its address.
                    mac.iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(":"),
                ),
            };
        let (protocol, driver, inputs, identity, label): (Box<dyn uhid::Protocol>, _, _, _, _) =
            match self.kind {
                GamepadKind::DualSense => (
                    Box::new(DualSense::new(index, mac)),
                    "playstation",
                    3,
                    &DUALSENSE,
                    "virtual DualSense",
                ),
                _ => (
                    Box::new(SteamController::new(
                        steam_controller::VARIANT,
                        index,
                        &serial,
                    )),
                    steam_controller::VARIANT.driver(),
                    steam_controller::VARIANT.inputs(),
                    &STEAM_CONTROLLER,
                    "virtual Steam Controller",
                ),
            };
        let params = uhid::Params {
            name,
            phys: &phys,
            uniq: &uniq,
            bus,
            vendor,
            product,
            version,
            descriptor,
        };
        let device = Device::create(
            &self.uhid,
            &params,
            protocol,
            self.events.clone(),
            format!("pad{index}-uhid"),
        )?;
        let found = match uhid::wait_for_nodes(&phys, driver, inputs, BIND_WAIT) {
            Ok(found) => found,
            Err(err) => {
                device.destroy();
                return Err(err);
            }
        };
        let mut pad = HidPad {
            device,
            hidraw: Vec::new(),
        };
        // The pad owns the device from here, so a failure destroys it.
        for input in &found.inputs {
            let class = if input.accelerometer {
                Class::Accelerometer
            } else if input.touch {
                Class::Touchpad
            } else {
                Class::Joystick
            };
            for node in &input.nodes {
                self.install(&self.dev_dir, node, class, identity)?;
            }
        }
        for node in &found.hidraw {
            std::fs::create_dir_all(&self.hidraw_dir)?;
            std::fs::set_permissions(&self.hidraw_dir, std::fs::Permissions::from_mode(0o755))?;
            self.install(&self.hidraw_dir, node, Class::Hidraw, identity)?;
            pad.hidraw.push(HidrawNode {
                name: node.name.clone(),
                major: node.major,
                minor: node.minor,
            });
        }
        info!(
            pad = index,
            driver = ?found.driver,
            hidraw = ?pad.hidraw,
            inputs = ?found.inputs.iter().map(|i| &i.name).collect::<Vec<_>>(),
            "gamepad: {label}"
        );
        Ok(pad)
    }

    fn create_xbox(&self, index: usize) -> Result<Pad> {
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
        let sysname = get_sysname(raw)?;
        let nodes: Vec<String> = self
            .share(&sysname, &XBOX_360)?
            .into_iter()
            .map(|n| n.name)
            .collect();
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
        let (tx, flag) = (self.events.clone(), Arc::clone(&stop));
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

    /// Mirrors a uinput device's nodes and udev entries into the app's volumes.
    fn share(&self, sysname: &str, identity: &Identity) -> Result<Vec<Node>> {
        let sys = Path::new("/sys/devices/virtual/input").join(sysname);
        // The evdev and joydev handlers attach as the device registers;
        // allow them a moment.
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut names = Vec::new();
        while names.iter().all(|n: &String| !n.starts_with("event")) {
            names = std::fs::read_dir(&sys)
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
        let mut nodes = Vec::new();
        for name in names {
            let dev = std::fs::read_to_string(sys.join(&name).join("dev"))?;
            let (major, minor) = dev
                .trim()
                .split_once(':')
                .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)))
                .context("parsing the device number")?;
            let node = Node { name, major, minor };
            self.install(&self.dev_dir, &node, Class::Joystick, identity)?;
            nodes.push(node);
        }
        Ok(nodes)
    }

    /// The uinput device a broker client made: its nodes and udev entries in
    /// the app's volumes.
    pub(crate) fn share_uinput(&self, sysname: &str, identity: &Identity) -> Result<Vec<Node>> {
        self.share(sysname, identity)
    }

    /// Takes a [`Gamepads::share_uinput`] device's nodes and udev entries away.
    pub(crate) fn unshare(&self, nodes: &[Node]) {
        for node in nodes {
            let _ = std::fs::remove_file(self.dev_dir.join(&node.name));
            let _ = std::fs::remove_file(
                self.udev_dir
                    .join(format!("c{}:{}", node.major, node.minor)),
            );
        }
    }

    /// The streamer's own `/dev/uinput`.
    pub(crate) fn uinput(&self) -> &Path {
        &self.uinput
    }

    /// Makes a device node in `dir` for the app, and its udev entry.
    fn install(&self, dir: &Path, node: &Node, class: Class, identity: &Identity) -> Result<()> {
        let path = dir.join(&node.name);
        let _ = std::fs::remove_file(&path);
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
        // SAFETY: a valid path; mknod creates a character device node.
        if unsafe {
            libc::mknod(
                c_path.as_ptr(),
                libc::S_IFCHR | 0o660,
                libc::makedev(node.major, node.minor),
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
        let initialized = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros();
        std::fs::write(
            self.udev_dir
                .join(format!("c{}:{}", node.major, node.minor)),
            udev_entry(initialized, class, identity),
        )?;
        Ok(())
    }
}

/// A node's entry in udev's database, as the app's libudev (Chrome, Wine,
/// SDL, hidapi) reads it: what the device is, and that it is the seat's.
fn udev_entry(initialized: u128, class: Class, identity: &Identity) -> String {
    let what = match class {
        Class::Joystick => "E:ID_INPUT=1\nE:ID_INPUT_JOYSTICK=1\n",
        Class::Touchpad => "E:ID_INPUT=1\nE:ID_INPUT_TOUCHPAD=1\n",
        Class::Accelerometer => "E:ID_INPUT=1\nE:ID_INPUT_ACCELEROMETER=1\n",
        Class::Hidraw => "",
    };
    format!(
        "I:{initialized}\n{what}E:ID_BUS={}\nE:ID_VENDOR_ID={}\nE:ID_MODEL_ID={}\n\
         E:ID_SERIAL={}\nG:seat\nG:uaccess\nQ:seat\nQ:uaccess\nV:1\n",
        identity.bus, identity.vendor, identity.product, identity.serial
    )
}

/// The environment variable naming whose pads these are (see
/// `Gamepads::identity`).
pub const PAD_IDENTITY_ENV: &str = "CHA_PAD_IDENTITY";

/// FNV-1a, 64-bit: the same for the same text in every build (std's hasher
/// isn't promised to be), so a serial made from it survives upgrades.
fn stable_hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Something that differs between runs, between streamers on one host (their
/// hostnames are their containers'), and between a streamer's restarts.
fn run_seed() -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for file in [
        "/proc/sys/kernel/hostname",
        "/proc/sys/kernel/random/boot_id",
    ] {
        std::fs::read_to_string(file)
            .unwrap_or_default()
            .hash(&mut hasher);
    }
    std::process::id().hash(&mut hasher);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    hasher.finish()
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
    tx: &broadcast::Sender<PadEvent>,
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
                let _ = tx.send(PadEvent::Rumble(rumble));
            }
        }
    }
}

pub(crate) fn event(kind: u16, code: u16, value: i32) -> InputEvent {
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

/// The kernel's name for a created uinput device (`input42`).
pub(crate) fn get_sysname(fd: c_int) -> Result<String> {
    let mut sysname = [0u8; 64];
    // SAFETY: the buffer is as long as the length in the request.
    if unsafe { libc::ioctl(fd, ui_get_sysname(sysname.len()), sysname.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error()).context("UI_GET_SYSNAME");
    }
    Ok(CStr::from_bytes_until_nul(&sysname)
        .context("sysname")?
        .to_string_lossy()
        .into_owned())
}

pub(crate) fn ioctl_int(fd: c_int, request: libc::c_ulong, value: c_int) -> Result<()> {
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
    fn a_users_steam_controller_keeps_its_serial() {
        // FNV-1a's published test vector: the hash can't drift between builds.
        assert_eq!(stable_hash("a"), 0xaf63_dc4c_8601_ec8c);
        let serial =
            |who: &str| steam_controller::serial_for(steam_controller::VARIANT, stable_hash(who));
        assert_eq!(serial("u1/steam/pad0"), serial("u1/steam/pad0"));
        assert_ne!(serial("u1/steam/pad0"), serial("u1/steam/pad1"));
        assert_ne!(serial("u1/steam/pad0"), serial("u2/steam/pad0"));
    }

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

    #[test]
    fn udev_entries_say_what_each_node_is() {
        assert_eq!(
            udev_entry(5, Class::Joystick, &XBOX_360),
            "I:5\nE:ID_INPUT=1\nE:ID_INPUT_JOYSTICK=1\nE:ID_BUS=usb\nE:ID_VENDOR_ID=045e\n\
             E:ID_MODEL_ID=028e\nE:ID_SERIAL=Microsoft_X-Box_360_pad\nG:seat\nG:uaccess\n\
             Q:seat\nQ:uaccess\nV:1\n"
        );
        let touchpad = udev_entry(5, Class::Touchpad, &DUALSENSE);
        assert!(touchpad.contains("E:ID_INPUT_TOUCHPAD=1\n") && !touchpad.contains("JOYSTICK"));
        assert!(touchpad.contains("E:ID_VENDOR_ID=054c\nE:ID_MODEL_ID=0ce6\n"));
        let sensors = udev_entry(5, Class::Accelerometer, &STEAM_CONTROLLER);
        assert!(sensors.contains("E:ID_INPUT_ACCELEROMETER=1\n"));
        let hidraw = udev_entry(5, Class::Hidraw, &STEAM_CONTROLLER);
        assert!(hidraw.contains("E:ID_BUS=bluetooth\nE:ID_VENDOR_ID=28de\nE:ID_MODEL_ID=1303\n"));
        assert_eq!(
            STEAM_CONTROLLER.product,
            format!("{:04x}", steam_controller::VARIANT.product())
        );
        let hidraw = udev_entry(5, Class::Hidraw, &DUALSENSE);
        assert!(!hidraw.contains("ID_INPUT"));
        assert!(hidraw.starts_with("I:5\nE:ID_BUS=usb\n") && hidraw.ends_with("V:1\n"));
    }

    #[test]
    fn pad_events_supersede_by_kind_and_side() {
        let trigger = |side| {
            PadEvent::Trigger(Trigger {
                slot: 0,
                side,
                effect: [0; 11],
            })
        };
        assert_ne!(trigger(Side::Left).class(), trigger(Side::Right).class());
        let classes: Vec<usize> = [
            PadEvent::Led(Led {
                slot: 0,
                r: 0,
                g: 0,
                b: 0,
            }),
            PadEvent::Players(Players { slot: 0, mask: 0 }),
            trigger(Side::Left),
            trigger(Side::Right),
        ]
        .iter()
        .map(PadEvent::class)
        .collect();
        assert!(classes.iter().all(|c| *c < EVENT_CLASSES));
        assert_eq!(GamepadKind::DualSense.name(), "dualsense");
    }

    /// Makes one pad of a uhid kind on a host with the driver, holds it for
    /// `CHA_LIVE_SECS` and says what the kernel and the apps' side saw. Run by
    /// hand with `--ignored --nocapture` where `/dev/uhid` is.
    fn live(kind: GamepadKind) {
        let _ = tracing_subscriber::fmt().try_init();
        let dir = std::env::temp_dir().join(format!("cha-live-{}", kind.name()));
        let _ = std::fs::remove_dir_all(&dir);
        let pads = Gamepads::new(
            Path::new("/dev/uinput"),
            Path::new("/dev/uhid"),
            kind,
            &dir,
            None,
            0,
        )
        .unwrap();
        assert_eq!(pads.kind(), kind);
        let mut events = pads.events();
        let mut b = vec![0.0; 24];
        b[0] = 1.0;
        let state = PadState {
            i: 0,
            b,
            a: vec![0.5, -0.5, 0.0, 0.0],
            gyro: Some([0.1, 0.2, 0.3]),
            accel: Some([0.0, 9.8, 0.0]),
            touch: vec![Touch {
                id: 0,
                x: 0.25,
                y: 0.75,
                down: true,
            }],
            ..PadState::default()
        };
        // Makes the pad (and waits for the driver).
        pads.update(&state);
        println!("hidraw: {:?}", pads.hidraw());
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let sub = entry.path();
            for node in std::fs::read_dir(&sub).into_iter().flatten().flatten() {
                println!("{}", node.path().display());
            }
        }
        let secs: u64 = std::env::var("CHA_LIVE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);
        let end = Instant::now() + Duration::from_secs(secs);
        // The app's side: the event nodes, and the hidraw node.
        let open = |path: &Path| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)
        };
        let mut evdevs: Vec<(String, std::fs::File)> = std::fs::read_dir(dir.join("dev"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
            .map(|e| {
                let file = open(&e.path()).unwrap();
                (e.file_name().to_string_lossy().into_owned(), file)
            })
            .collect();
        evdevs.sort_by(|a, b| a.0.cmp(&b.0));
        let hidraw_path = dir.join("dev/hidraw").join(&pads.hidraw()[0].name);
        let hidraw = open(&hidraw_path).unwrap();
        // What the apps ask: rumble and a trigger on a DualSense (an output
        // report written to hidraw), a pulse on a Steam Controller (a feature
        // report set, then the reply read back).
        let fd = hidraw.as_raw_fd();
        let hid_ioctl = |nr: u64, buf: &mut [u8]| {
            let request = (3u64 << 30) | ((buf.len() as u64) << 16) | (0x48 << 8) | nr;
            // SAFETY: HIDIOC[SG]FEATURE on a hidraw descriptor with a buffer of the length asked.
            unsafe { libc::ioctl(fd, request as libc::c_ulong, buf.as_mut_ptr()) }
        };
        if kind == GamepadKind::DualSense {
            let mut out = [0u8; 63];
            out[0] = 0x02;
            out[1] = 0x01 | 0x02 | 0x04;
            out[2] = 0x04;
            out[3] = 100;
            out[4] = 200;
            out[11..22].copy_from_slice(&[2, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
            // SAFETY: a valid descriptor and buffer.
            let n = unsafe { libc::write(fd, out.as_ptr().cast(), out.len()) };
            println!("hidraw write: {n}");
            let mut cal = [0u8; 41];
            cal[0] = 0x05;
            println!("get feature 0x05: {}", hid_ioctl(0x07, &mut cal));
            let mut fw = [0u8; 64];
            fw[0] = 0x20;
            println!(
                "get feature 0x20: {} {:02x?}",
                hid_ioctl(0x07, &mut fw),
                &fw[..4]
            );
        } else if steam_controller::VARIANT == steam_controller::Variant::Triton {
            // The 2026 controller: feature report 1 (the id and 63 bytes) both
            // ways, and output reports 0x80 (rumble) and 0x81 (a pulse).
            let command = |msg: &[u8]| {
                let mut buf = [0u8; 64];
                buf[0] = 1;
                buf[1..=msg.len()].copy_from_slice(msg);
                let set = hid_ioctl(0x06, &mut buf);
                let mut reply = [0u8; 64];
                reply[0] = 1;
                let got = hid_ioctl(0x07, &mut reply);
                println!(
                    "command {:02x?}: set {set}, get {got} {:02x?}",
                    msg,
                    &reply[..8]
                );
                reply
            };
            command(&[0x87, 3, 0x09, 0, 0]);
            command(&[0x83, 0]);
            let serial = command(&[0xae, 21, 1]);
            println!("serial: {:?}", String::from_utf8_lossy(&serial[4..17]));
            let rumble = [0x80u8, 0, 0, 0, 0xff, 0xff, 0, 0x00, 0x80, 0];
            // SAFETY: a valid descriptor and buffer.
            let n = unsafe { libc::write(fd, rumble.as_ptr().cast(), rumble.len()) };
            println!("hidraw write 0x80: {n}");
            let pulse = [0x81u8, 1, 0x90, 0x01, 100, 0, 3, 0];
            // SAFETY: as above.
            let n = unsafe { libc::write(fd, pulse.as_ptr().cast(), pulse.len()) };
            println!("hidraw write 0x81: {n}");
        } else if steam_controller::VARIANT == steam_controller::Variant::Ble {
            // Bluetooth framing: report 3, a header and 18 bytes, both ways.
            let send = |msg: &[u8]| {
                let mut buf = [0u8; 20];
                buf[0] = 3;
                buf[1] = 0xc0;
                buf[2..2 + msg.len()].copy_from_slice(msg);
                hid_ioctl(0x06, &mut buf)
            };
            let read = || {
                let mut out = Vec::new();
                for _ in 0..4 {
                    let mut buf = [0u8; 20];
                    buf[0] = 3;
                    let n = hid_ioctl(0x07, &mut buf);
                    println!("get feature: {n} {:02x?}", &buf[..20]);
                    out.extend_from_slice(&buf[2..]);
                    if n < 0 || buf[1] & 0x40 != 0 {
                        break;
                    }
                }
                out
            };
            println!(
                "set feature 0x8f: {}",
                send(&[0x8f, 8, 1, 0x90, 1, 0, 0, 1, 0, 0])
            );
            println!("set feature 0x83: {}", send(&[0x83, 0]));
            // The attributes take two segments (the first has no last flag).
            let attributes = read();
            println!("attributes: {:02x?}", &attributes[..32]);
            println!("set feature 0xae: {}", send(&[0xae, 11, 1]));
            let serial = read();
            println!("serial: {:?}", String::from_utf8_lossy(&serial[3..13]));
        } else {
            let mut cmd = [0u8; 65];
            cmd[1..11].copy_from_slice(&[0x8f, 8, 1, 0x90, 1, 0, 0, 1, 0, 0]);
            println!("set feature 0x8f: {}", hid_ioctl(0x06, &mut cmd));
            let mut get = [0u8; 65];
            cmd[1..3].copy_from_slice(&[0x83, 0]);
            println!("set feature 0x83: {}", hid_ioctl(0x06, &mut cmd));
            println!(
                "get feature: {} {:02x?}",
                hid_ioctl(0x07, &mut get),
                &get[..20]
            );
            let mut get = [0u8; 65];
            cmd[1..4].copy_from_slice(&[0xae, 11, 1]);
            hid_ioctl(0x06, &mut cmd);
            println!(
                "serial: {} {:?}",
                hid_ioctl(0x07, &mut get),
                String::from_utf8_lossy(&get[..16])
            );
        }
        // Changed after the nodes were opened, so the drivers report them.
        let state = PadState {
            a: vec![0.6, -0.5, 0.0, 0.0],
            gyro: Some([0.15, 0.25, 0.35]),
            accel: Some([0.5, 9.8, -0.5]),
            ..state
        };
        let mut last: std::collections::BTreeMap<(String, u16, u16), i32> = Default::default();
        let mut report = [0u8; 80];
        let mut reports = 0;
        while Instant::now() < end {
            pads.update(&state);
            std::thread::sleep(Duration::from_millis(10));
            for (name, file) in &mut evdevs {
                let mut buf = [0u8; 24 * 32];
                while let Ok(n) = std::io::Read::read(file, &mut buf) {
                    for ev in buf[..n].chunks_exact(24) {
                        let (t, c) = (
                            u16::from_ne_bytes([ev[16], ev[17]]),
                            u16::from_ne_bytes([ev[18], ev[19]]),
                        );
                        let v = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
                        if t == EV_ABS || t == EV_KEY {
                            last.insert((name.clone(), t, c), v);
                        }
                    }
                }
            }
            while let Ok(n) = std::io::Read::read(&mut &hidraw, &mut report) {
                reports += 1;
                if reports == 1 {
                    println!("hidraw report ({n} bytes): {:02x?}", &report[..n.min(64)]);
                }
            }
        }
        println!("hidraw reports read: {reports}");
        for ((name, t, c), v) in &last {
            println!("{name}: type {t} code {c:#x} = {v}");
        }
        while let Ok(event) = events.try_recv() {
            println!("event: {event:?}");
        }
    }

    #[test]
    #[ignore = "needs /dev/uhid and the host's hid-playstation"]
    fn live_dualsense() {
        live(GamepadKind::DualSense);
    }

    #[test]
    #[ignore = "needs /dev/uhid"]
    fn live_steam_controller() {
        live(GamepadKind::Steam);
    }
}
