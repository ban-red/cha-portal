//! Steam's `/dev/uinput`, for the `steam` pad kind.
//!
//! Steam's client makes the virtual Xbox pad (`28de:11ff`) it hands games
//! through `/dev/uinput`, which the app doesn't have (with it the app could
//! make keyboards and mice on the node's kernel). In the Steam image an
//! `LD_PRELOAD` shim (`images/steam/uinput-shim/`) fakes that file and speaks
//! [`crate::uinput_proto`] to this broker over `<runtime dir>/uinput.sock`. The
//! broker validates what the client asks for ([`crate::uinput_policy`]), makes
//! the real device with the streamer's own `/dev/uinput`, and shares its nodes
//! and udev entry with the app like our own pads', so Proton's games find it.
//!
//! The socket is reachable by every process of the app, games included, so the
//! validator is the boundary: the identity (`28de:11ff`, USB, phys
//! `cha/pad<N>` past our own pads so the host's udev rule matches) is ours, the
//! name is the client's but sanitised, and only gamepad buttons, axes and
//! rumble pass. One connection is one device; its end (Steam exits or crashes)
//! destroys the device and takes its nodes away.
//!
//! Force feedback: the kernel asks the device's owner to take an app's effect
//! (`UI_FF_UPLOAD`, `UI_FF_ERASE` on the uinput descriptor) and blocks the app
//! until it answers. The broker relays those requests to the client, which
//! answers as it would a real uinput (`UI_END_FF_*`), and completes them with
//! that answer, or an error after [`FF_TIMEOUT`]. Plays, stops and the gain go
//! to the client as they come; Steam, not the streamer, then drives the Steam
//! Controller's motors.

use std::collections::HashSet;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::gamepad::MAX_PADS;
use crate::gamepad::{
    AbsInfo, EV_FF, EV_UINPUT, FfEffect, Gamepads, InputEvent, InputId, STEAM_VIRTUAL_PAD,
    UI_ABS_SETUP, UI_BEGIN_FF_ERASE, UI_BEGIN_FF_UPLOAD, UI_DEV_CREATE, UI_DEV_DESTROY,
    UI_DEV_SETUP, UI_END_FF_ERASE, UI_END_FF_UPLOAD, UI_FF_ERASE, UI_FF_UPLOAD, UI_SET_ABSBIT,
    UI_SET_EVBIT, UI_SET_FFBIT, UI_SET_KEYBIT, UI_SET_PHYS, UinputAbsSetup, UinputFfErase,
    UinputFfUpload, UinputSetup, event, get_sysname, ioctl_int,
};
use crate::uhid::Node;
use crate::uinput_policy::{self as policy, RateLimit, Refusal, Slots};
use crate::uinput_proto::{
    AbsSetup, BitKind, DevSetup, Event, MAX_PACKET, Message, Request, WireEffect,
};

/// How long the client has to answer a force feedback request: the app's
/// `EVIOCSFF` waits for it (the kernel gives up after 30 s).
const FF_TIMEOUT: Duration = Duration::from_secs(2);
/// Force feedback requests waiting for the client, at most.
const MAX_PENDING: usize = 32;
/// Connections at once (most hold no device): a bound for a game that opens
/// the socket in a loop.
const MAX_CONNECTIONS: usize = 16;

/// Set by the node (to `1`) for the Steam environment: Steam makes the virtual
/// Xbox pad it hands games whatever kind our own pads are, as long as it
/// handles them (it does the Xbox 360 and DualSense ones), so the broker runs
/// for every kind there. An environment variable, which an older streamer
/// ignores.
pub const STEAM_ENV: &str = "CHA_STEAM_UINPUT";

/// Whether the broker runs: for the `steam` kind, whose pad Steam always
/// handles, and wherever the node says the app is Steam (`steam_env` is
/// [`STEAM_ENV`]'s value).
pub fn wanted(kind: crate::gamepad::GamepadKind, steam_env: Option<&str>) -> bool {
    kind == crate::gamepad::GamepadKind::Steam || matches!(steam_env, Some("1" | "true"))
}

/// The socket, and the threads behind it; ends with the streamer.
pub struct UinputBroker {
    stop: Arc<AtomicBool>,
    path: PathBuf,
}

impl UinputBroker {
    /// Listens on `path` (mode 0660, the app's uid), if the streamer can make
    /// uinput devices.
    pub fn start(pads: Arc<Gamepads>, path: &Path, app_uid: Option<u32>) -> Result<Self> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pads.uinput())
            .with_context(|| format!("opening {}", pads.uinput().display()))?;
        let listener = listen(path, app_uid)?;
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            pads,
            slots: Slots::default(),
            warned: Mutex::default(),
            connections: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
        });
        let flag = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("uinput-accept".into())
            .spawn(move || accept_loop(listener, shared, flag))
            .context("spawning the uinput broker")?;
        info!(socket = %path.display(), "Steam's uinput broker is up");
        Ok(Self {
            stop,
            path: path.to_path_buf(),
        })
    }
}

impl Drop for UinputBroker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&self.path);
    }
}

struct Shared {
    pads: Arc<Gamepads>,
    slots: Slots,
    /// Refusals already warned about, by kind: the log says each once.
    warned: Mutex<HashSet<&'static str>>,
    connections: AtomicU64,
    next_id: AtomicU64,
}

impl Shared {
    fn refused(&self, conn: u64, refusal: &Refusal) {
        let first = self
            .warned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(refusal.what);
        if first {
            warn!(
                conn,
                "uinput client refused ({}): {} (further ones of this kind are logged at debug)",
                refusal.what,
                refusal.detail
            );
        } else {
            debug!(
                conn,
                "uinput client refused ({}): {}", refusal.what, refusal.detail
            );
        }
    }
}

fn listen(path: &Path, app_uid: Option<u32>) -> Result<OwnedFd> {
    let _ = std::fs::remove_file(path);
    // SAFETY: plain socket creation.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("socket");
    }
    // SAFETY: just made, and nothing else owns it.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: an all-zero sockaddr_un is valid.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_encoded_bytes();
    anyhow::ensure!(
        bytes.len() < addr.sun_path.len(),
        "{} is too long",
        path.display()
    );
    for (dst, src) in addr.sun_path.iter_mut().zip(bytes) {
        *dst = *src as libc::c_char;
    }
    // SAFETY: a valid socket and sockaddr_un.
    if unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&raw const addr).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("binding {}", path.display()));
    }
    // Nobody can connect before `listen`, so the mode is right by then.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    if let Some(uid) = app_uid {
        std::os::unix::fs::chown(path, Some(uid), Some(uid))?;
    }
    // SAFETY: a bound socket.
    if unsafe { libc::listen(fd.as_raw_fd(), 8) } < 0 {
        return Err(std::io::Error::last_os_error()).context("listen");
    }
    Ok(fd)
}

fn accept_loop(listener: OwnedFd, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        if !poll_readable(&[listener.as_raw_fd()], 500)[0] {
            continue;
        }
        // SAFETY: a listening socket; the peer's address isn't wanted.
        let fd = unsafe {
            libc::accept4(
                listener.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC,
            )
        };
        if fd < 0 {
            continue;
        }
        // SAFETY: just accepted, and nothing else owns it.
        let sock = unsafe { OwnedFd::from_raw_fd(fd) };
        if shared.connections.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS as u64 {
            shared.connections.fetch_sub(1, Ordering::Relaxed);
            debug!("uinput: too many connections, one closed");
            continue;
        }
        let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
        let shared = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name(format!("uinput-{id}"))
            .spawn({
                let shared = Arc::clone(&shared);
                move || {
                    Conn::new(sock, &shared, id).run();
                    shared.connections.fetch_sub(1, Ordering::Relaxed);
                }
            });
        if spawned.is_err() {
            shared.connections.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Which of `fds` have something to read (or hung up), within `timeout_ms`.
fn poll_readable(fds: &[RawFd], timeout_ms: i32) -> Vec<bool> {
    let mut polls: Vec<libc::pollfd> = fds
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    // SAFETY: valid pollfds.
    unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, timeout_ms) };
    polls
        .iter()
        .map(|p| p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
        .collect()
}

/// What a client made: the kernel's device, and what the app was given of it.
struct Device {
    slot: usize,
    sysname: String,
    nodes: Vec<Node>,
}

enum Pending {
    Upload(Box<UinputFfUpload>),
    Erase(UinputFfErase),
}

struct Waiting {
    request_id: u32,
    what: Pending,
    deadline: Instant,
}

/// One client: at most one device.
struct Conn<'a> {
    sock: OwnedFd,
    shared: &'a Shared,
    id: u64,
    /// The streamer's uinput descriptor the client's calls are applied to.
    uinput: Option<OwnedFd>,
    /// From the client's `DEV_SETUP`, applied at create.
    setup: Option<DevSetup>,
    device: Option<Device>,
    rate: RateLimit,
    start: Instant,
    pending: Vec<Waiting>,
    /// The socket broke: end.
    dead: bool,
}

impl<'a> Conn<'a> {
    fn new(sock: OwnedFd, shared: &'a Shared, id: u64) -> Self {
        Self {
            sock,
            shared,
            id,
            uinput: None,
            setup: None,
            device: None,
            rate: RateLimit::new(),
            start: Instant::now(),
            pending: Vec::new(),
            dead: false,
        }
    }

    fn run(mut self) {
        debug!(conn = self.id, "uinput client connected");
        while !self.dead {
            let now = Instant::now();
            let timeout = self
                .pending
                .iter()
                .map(|p| p.deadline.saturating_duration_since(now))
                .min()
                .map_or(1000, |d| d.as_millis().min(1000) as i32 + 1);
            let mut fds = vec![self.sock.as_raw_fd()];
            fds.extend(self.uinput.as_ref().map(AsRawFd::as_raw_fd));
            let ready = poll_readable(&fds, timeout);
            if ready[0] {
                self.serve_requests();
            }
            if !self.dead && ready.get(1) == Some(&true) {
                self.serve_kernel();
            }
            self.expire();
        }
        self.teardown("client gone");
        debug!(conn = self.id, "uinput client ended");
    }

    /// Everything the client has sent so far.
    fn serve_requests(&mut self) {
        let mut buf = vec![0u8; MAX_PACKET + 1];
        loop {
            // SAFETY: the buffer is as long as the length passed.
            let n = unsafe {
                libc::recv(
                    self.sock.as_raw_fd(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if n == 0 {
                self.dead = true;
                return;
            }
            if n < 0 {
                if std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock {
                    self.dead = true;
                }
                return;
            }
            let reply = if n as usize > MAX_PACKET {
                Some(Message::err(libc::EMSGSIZE))
            } else {
                match Request::decode(&buf[..n as usize]) {
                    Ok(request) => self.handle(request),
                    Err(err) => {
                        self.shared.refused(
                            self.id,
                            &Refusal {
                                what: "packet",
                                detail: err.to_string(),
                            },
                        );
                        Some(Message::err(libc::EINVAL))
                    }
                }
            };
            if let Some(reply) = reply {
                self.send(&reply);
            }
            if self.dead {
                return;
            }
        }
    }

    fn send(&mut self, message: &Message) {
        let bytes = message.encode();
        // A client that doesn't read is as good as gone.
        // SAFETY: the buffer is as long as the length passed.
        let n = unsafe {
            libc::send(
                self.sock.as_raw_fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if n < 0 {
            self.dead = true;
        }
    }

    fn refuse(&self, refusal: Refusal) -> Option<Message> {
        self.shared.refused(self.id, &refusal);
        Some(Message::err(libc::EINVAL))
    }

    fn handle(&mut self, request: Request) -> Option<Message> {
        match request {
            Request::SetBit { kind, code } => {
                if let Err(r) = policy::check_bit(kind, code) {
                    return self.refuse(r);
                }
                let ioctl = match kind {
                    BitKind::Ev => UI_SET_EVBIT,
                    BitKind::Key => UI_SET_KEYBIT,
                    BitKind::Abs => UI_SET_ABSBIT,
                    BitKind::Ff => UI_SET_FFBIT,
                    // check_bit allowed nothing else.
                    _ => return Some(Message::err(libc::EINVAL)),
                };
                Some(self.apply(|fd| ioctl_int(fd, ioctl, code.into())))
            }
            Request::AbsSetup(setup) => {
                if let Err(r) = policy::check_abs(&setup) {
                    return self.refuse(r);
                }
                Some(self.apply(|fd| abs_setup(fd, &setup)))
            }
            Request::DevSetup(setup) => {
                if let Err(r) = policy::check_dev_setup(&setup) {
                    return self.refuse(r);
                }
                self.setup = Some(setup);
                Some(Message::ok())
            }
            Request::Create => Some(self.create()),
            Request::Destroy => {
                self.teardown("destroyed by the client");
                Some(Message::ok())
            }
            Request::GetSysname => Some(match &self.device {
                Some(d) => Message::Reply {
                    errno: 0,
                    payload: d.sysname.clone().into_bytes(),
                },
                None => Message::err(libc::ENOENT),
            }),
            Request::Events(events) => Some(self.events(&events)),
            Request::FfDone { request_id, retval } => {
                self.finish(request_id, retval);
                None
            }
        }
    }

    /// Runs `f` on the kernel descriptor (opened at the first call), turning
    /// its failure into the client's errno.
    fn apply(&mut self, f: impl FnOnce(RawFd) -> Result<()>) -> Message {
        if self.device.is_some() {
            return Message::err(libc::EINVAL);
        }
        let fd = match self.kernel_fd() {
            Ok(fd) => fd,
            Err(err) => return errno_message(&err),
        };
        match f(fd) {
            Ok(()) => Message::ok(),
            Err(err) => errno_message(&err),
        }
    }

    fn kernel_fd(&mut self) -> Result<RawFd> {
        if self.uinput.is_none() {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(self.shared.pads.uinput())
                .with_context(|| format!("opening {}", self.shared.pads.uinput().display()))?;
            self.uinput = Some(file.into());
        }
        Ok(self.uinput.as_ref().expect("just opened").as_raw_fd())
    }

    fn create(&mut self) -> Message {
        if self.device.is_some() {
            return Message::err(libc::EINVAL);
        }
        let Some(slot) = self.shared.slots.take() else {
            self.shared.refused(
                self.id,
                &Refusal {
                    what: "too many devices",
                    detail: format!("{} devices already", policy::MAX_DEVICES),
                },
            );
            return Message::err(libc::EMFILE);
        };
        match self.make(slot) {
            Ok(device) => {
                info!(
                    conn = self.id,
                    pad = MAX_PADS + slot,
                    device = %device.sysname,
                    nodes = ?device.nodes.iter().map(|n| &n.name).collect::<Vec<_>>(),
                    "Steam's virtual gamepad made"
                );
                self.device = Some(device);
                Message::ok()
            }
            Err(err) => {
                warn!(conn = self.id, "Steam's virtual gamepad failed: {err:#}");
                self.shared.slots.release(slot);
                // The half-made device goes with the descriptor.
                self.uinput = None;
                errno_message(&err)
            }
        }
    }

    fn make(&mut self, slot: usize) -> Result<Device> {
        let fd = self.kernel_fd()?;
        let client = self.setup.clone().unwrap_or(DevSetup {
            bustype: 0,
            vendor: 0,
            product: 0,
            version: 0,
            ff_effects_max: 0,
            name: [0; 80],
        });
        let name = policy::sanitize_name(&client.name);
        let mut setup = UinputSetup {
            id: InputId {
                bustype: policy::BUS_USB,
                vendor: policy::VENDOR,
                product: policy::PRODUCT,
                version: client.version,
            },
            name: [0; 80],
            ff_effects_max: client.ff_effects_max,
        };
        setup.name[..name.len()].copy_from_slice(name.as_bytes());
        // Where the host's udev rule (deploy/node/host) recognizes our pads:
        // past the numbers of the pads we make ourselves.
        let phys = CString::new(format!("cha/pad{}", MAX_PADS + slot)).expect("no NUL");
        // SAFETY: a valid uinput fd and a NUL-terminated string.
        if unsafe { libc::ioctl(fd, UI_SET_PHYS, phys.as_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error()).context("UI_SET_PHYS");
        }
        // SAFETY: a valid uinput fd and a uinput_setup.
        if unsafe { libc::ioctl(fd, UI_DEV_SETUP, &setup) } < 0 {
            return Err(std::io::Error::last_os_error()).context("UI_DEV_SETUP");
        }
        // SAFETY: as above; no argument.
        if unsafe { libc::ioctl(fd, UI_DEV_CREATE) } < 0 {
            return Err(std::io::Error::last_os_error()).context("UI_DEV_CREATE");
        }
        let sysname = get_sysname(fd)?;
        let nodes = match self.shared.pads.share_uinput(&sysname, &STEAM_VIRTUAL_PAD) {
            Ok(nodes) => nodes,
            Err(err) => {
                // SAFETY: as above.
                unsafe { libc::ioctl(fd, UI_DEV_DESTROY) };
                return Err(err);
            }
        };
        Ok(Device {
            slot,
            sysname,
            nodes,
        })
    }

    fn events(&mut self, events: &[Event]) -> Message {
        if self.device.is_none() {
            return Message::err(libc::EINVAL);
        }
        if let Err(r) = policy::check_events(events) {
            self.shared.refused(self.id, &r);
            return Message::err(libc::EINVAL);
        }
        if !self.rate.take(events.len(), self.start.elapsed()) {
            self.shared.refused(
                self.id,
                &Refusal {
                    what: "rate",
                    detail: format!("over {} events per second", policy::EVENT_RATE),
                },
            );
            return Message::err(libc::EAGAIN);
        }
        let out: Vec<InputEvent> = events
            .iter()
            .map(|e| event(e.kind, e.code, e.value))
            .collect();
        let size = std::mem::size_of_val(&out[..]);
        let Some(fd) = self.uinput.as_ref().map(AsRawFd::as_raw_fd) else {
            return Message::err(libc::EINVAL);
        };
        // SAFETY: plain repr(C) structs, written as the kernel reads them.
        let n = unsafe { libc::write(fd, out.as_ptr().cast(), size) };
        if n < 0 {
            return Message::err(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            );
        }
        Message::ok()
    }

    /// What the kernel has for the owner: force feedback uploads and erases
    /// the app asked for, and its plays.
    fn serve_kernel(&mut self) {
        let Some(fd) = self.uinput.as_ref().map(AsRawFd::as_raw_fd) else {
            return;
        };
        let mut buf: [InputEvent; 16] = std::array::from_fn(|_| event(0, 0, 0));
        loop {
            // SAFETY: the buffer holds 16 repr(C) events; the kernel writes whole ones.
            let n =
                unsafe { libc::read(fd, buf.as_mut_ptr().cast(), std::mem::size_of_val(&buf[..])) };
            if n <= 0 {
                return;
            }
            let count = n as usize / std::mem::size_of::<InputEvent>();
            for ev in buf.iter().take(count) {
                let (kind, code, value) = (ev.kind, ev.code, ev.value);
                match (kind, code) {
                    (EV_UINPUT, UI_FF_UPLOAD) => self.ff_upload(fd, value as u32),
                    (EV_UINPUT, UI_FF_ERASE) => self.ff_erase(fd, value as u32),
                    (EV_FF, code) => self.send(&Message::Event(Event {
                        kind: EV_FF,
                        code,
                        value,
                    })),
                    _ => {}
                }
            }
            if count < buf.len() {
                return;
            }
        }
    }

    fn ff_upload(&mut self, fd: RawFd, request_id: u32) {
        let mut up = Box::new(UinputFfUpload {
            request_id,
            ..UinputFfUpload::default()
        });
        // SAFETY: a uinput_ff_upload, as the request says.
        if unsafe { libc::ioctl(fd, UI_BEGIN_FF_UPLOAD, &mut *up) } < 0 {
            return;
        }
        if !policy::effect_kind_ok(up.effect.kind) || self.pending.len() >= MAX_PENDING {
            self.shared.refused(
                self.id,
                &Refusal {
                    what: "ff effect",
                    detail: format!("effect type {:#x} or too many waiting", up.effect.kind),
                },
            );
            up.retval = -libc::EINVAL;
            // SAFETY: as above.
            unsafe { libc::ioctl(fd, UI_END_FF_UPLOAD, &*up) };
            return;
        }
        let message = Message::FfUpload {
            request_id,
            effect: wire_effect(&up.effect),
            old: wire_effect(&up.old),
        };
        self.pending.push(Waiting {
            request_id,
            what: Pending::Upload(up),
            deadline: Instant::now() + FF_TIMEOUT,
        });
        self.send(&message);
    }

    fn ff_erase(&mut self, fd: RawFd, request_id: u32) {
        let mut erase = UinputFfErase {
            request_id,
            ..UinputFfErase::default()
        };
        // SAFETY: a uinput_ff_erase, as the request says.
        if unsafe { libc::ioctl(fd, UI_BEGIN_FF_ERASE, &mut erase) } < 0 {
            return;
        }
        if self.pending.len() >= MAX_PENDING {
            erase.retval = -libc::EBUSY;
            // SAFETY: as above.
            unsafe { libc::ioctl(fd, UI_END_FF_ERASE, &erase) };
            return;
        }
        let message = Message::FfErase {
            request_id,
            effect_id: erase.effect_id,
        };
        self.pending.push(Waiting {
            request_id,
            what: Pending::Erase(erase),
            deadline: Instant::now() + FF_TIMEOUT,
        });
        self.send(&message);
    }

    /// The client's answer to a request: completes it for the app.
    fn finish(&mut self, request_id: u32, retval: i32) {
        let Some(at) = self.pending.iter().position(|p| p.request_id == request_id) else {
            return;
        };
        let pending = self.pending.swap_remove(at);
        self.complete(pending.what, retval);
    }

    fn complete(&self, what: Pending, retval: i32) {
        let Some(fd) = self.uinput.as_ref().map(AsRawFd::as_raw_fd) else {
            return;
        };
        match what {
            Pending::Upload(mut up) => {
                up.retval = retval;
                // SAFETY: a uinput_ff_upload the kernel filled.
                unsafe { libc::ioctl(fd, UI_END_FF_UPLOAD, &*up) };
            }
            Pending::Erase(mut erase) => {
                erase.retval = retval;
                // SAFETY: a uinput_ff_erase the kernel filled.
                unsafe { libc::ioctl(fd, UI_END_FF_ERASE, &erase) };
            }
        }
    }

    /// Requests the client didn't answer in time fail for the app.
    fn expire(&mut self) {
        let now = Instant::now();
        let mut i = 0;
        while i < self.pending.len() {
            if self.pending[i].deadline <= now {
                let pending = self.pending.swap_remove(i);
                warn!(
                    conn = self.id,
                    "the client didn't answer a force feedback request"
                );
                self.complete(pending.what, -libc::EIO);
            } else {
                i += 1;
            }
        }
    }

    /// Ends the device, if any: the app's nodes and udev entries go first,
    /// requests waiting are failed, and the kernel destroys the device.
    fn teardown(&mut self, why: &str) {
        let device = self.device.take();
        if let Some(device) = &device {
            self.shared.pads.unshare(&device.nodes);
        }
        for pending in std::mem::take(&mut self.pending) {
            self.complete(pending.what, -libc::EIO);
        }
        if let (Some(fd), Some(_)) = (self.uinput.take(), &device) {
            destroy(fd);
        }
        self.setup = None;
        if let Some(device) = device {
            self.shared.slots.release(device.slot);
            info!(
                conn = self.id,
                device = %device.sysname,
                "Steam's virtual gamepad removed ({why})"
            );
        }
    }
}

/// Destroys a created device. While an app still holds its node, the kernel
/// asks the owner to erase that app's effects as the device goes, and waits
/// for the answer, so the answering goes on beside the destroying.
fn destroy(fd: OwnedFd) {
    let raw = fd.as_raw_fd();
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut buf: [InputEvent; 16] = std::array::from_fn(|_| event(0, 0, 0));
            while !done.load(Ordering::Relaxed) {
                if !poll_readable(&[raw], 20)[0] {
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
                for ev in buf
                    .iter()
                    .take(usize::try_from(n).unwrap_or(0) / std::mem::size_of::<InputEvent>())
                {
                    match (ev.kind, ev.code) {
                        (EV_UINPUT, UI_FF_UPLOAD) => {
                            let mut up = Box::new(UinputFfUpload {
                                request_id: ev.value as u32,
                                ..UinputFfUpload::default()
                            });
                            // SAFETY: a uinput_ff_upload, as the request says.
                            if unsafe { libc::ioctl(raw, UI_BEGIN_FF_UPLOAD, &mut *up) } >= 0 {
                                up.retval = -libc::ENODEV;
                                // SAFETY: as above.
                                unsafe { libc::ioctl(raw, UI_END_FF_UPLOAD, &*up) };
                            }
                        }
                        (EV_UINPUT, UI_FF_ERASE) => {
                            let mut erase = UinputFfErase {
                                request_id: ev.value as u32,
                                ..UinputFfErase::default()
                            };
                            // SAFETY: a uinput_ff_erase, as the request says.
                            if unsafe { libc::ioctl(raw, UI_BEGIN_FF_ERASE, &mut erase) } >= 0 {
                                erase.retval = 0;
                                // SAFETY: as above.
                                unsafe { libc::ioctl(raw, UI_END_FF_ERASE, &erase) };
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
        // SAFETY: a valid uinput fd; no argument.
        unsafe { libc::ioctl(raw, UI_DEV_DESTROY) };
        done.store(true, Ordering::Relaxed);
    });
}

fn abs_setup(fd: RawFd, setup: &AbsSetup) -> Result<()> {
    let setup = UinputAbsSetup {
        code: setup.code,
        absinfo: AbsInfo {
            value: 0,
            minimum: setup.min,
            maximum: setup.max,
            fuzz: setup.fuzz,
            flat: setup.flat,
            resolution: setup.res,
        },
    };
    // SAFETY: a valid uinput fd and a uinput_abs_setup.
    if unsafe { libc::ioctl(fd, UI_ABS_SETUP, &setup) } < 0 {
        return Err(std::io::Error::last_os_error()).context("UI_ABS_SETUP");
    }
    Ok(())
}

/// The kernel's `ff_effect`, as the wire has it.
fn wire_effect(effect: &FfEffect) -> WireEffect {
    let mut union = [0u8; 24];
    union.copy_from_slice(&effect.u.0[..24]);
    // A periodic effect's `custom_len` (and the pointer past it, not carried)
    // is the client's to have none of.
    union[20..].fill(0);
    WireEffect {
        kind: effect.kind,
        id: effect.id,
        direction: effect.direction,
        trigger_button: effect.trigger_button,
        trigger_interval: effect.trigger_interval,
        replay_length: effect.replay_length,
        replay_delay: effect.replay_delay,
        union,
    }
}

/// The OS error under an `anyhow` one, as the client's errno.
fn errno_message(err: &anyhow::Error) -> Message {
    let errno = err
        .chain()
        .find_map(|e| e.downcast_ref::<std::io::Error>())
        .and_then(std::io::Error::raw_os_error)
        .unwrap_or(libc::EIO);
    Message::err(errno)
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_broker_runs_for_the_steam_kind_and_for_a_steam_environment() {
        use crate::gamepad::GamepadKind::{DualSense, Steam, Xbox360};
        assert!(wanted(Steam, None));
        assert!(wanted(Xbox360, Some("1")));
        assert!(wanted(DualSense, Some("true")));
        // Any other app: no socket that makes gamepads.
        assert!(!wanted(Xbox360, None));
        assert!(!wanted(DualSense, Some("")));
        assert!(!wanted(Xbox360, Some("0")));
    }
    use super::*;
    use crate::gamepad::GamepadKind;
    use crate::uinput_proto::{EFFECT_LEN, NAME_LEN};

    #[test]
    fn effects_become_wire_effects() {
        let mut e = FfEffect {
            kind: 0x51,
            id: 4,
            replay_length: 250,
            ..FfEffect::default()
        };
        e.u.0[0..2].copy_from_slice(&0x5au16.to_ne_bytes());
        e.u.0[4..6].copy_from_slice(&1000i16.to_ne_bytes());
        // custom_len and the pointer after it.
        e.u.0[20..32].fill(0xee);
        let w = wire_effect(&e);
        assert_eq!((w.kind, w.id, w.replay_length), (0x51, 4, 250));
        assert_eq!(&w.union[..2], &0x5au16.to_le_bytes());
        assert_eq!(&w.union[4..6], &1000i16.to_le_bytes());
        assert_eq!(&w.union[20..], &[0; 4]);
        assert_eq!(w.encode().len(), EFFECT_LEN);
    }

    #[test]
    fn errno_comes_from_the_io_error() {
        let err = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::EPERM))
            .context("UI_DEV_CREATE");
        assert_eq!(errno_message(&err), Message::err(libc::EPERM));
        assert_eq!(
            errno_message(&anyhow::anyhow!("no node")),
            Message::err(libc::EIO)
        );
    }

    // The tests below make real devices: they need the streamer's /dev/uinput
    // (the node's dev container, or any root shell with the device) and skip
    // without it.

    struct Client(OwnedFd);

    impl Client {
        fn connect(path: &Path) -> Self {
            // SAFETY: plain socket creation and connect with a valid address.
            unsafe {
                let fd = libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0);
                assert!(fd >= 0);
                let fd = OwnedFd::from_raw_fd(fd);
                let mut addr: libc::sockaddr_un = std::mem::zeroed();
                addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
                for (dst, src) in addr
                    .sun_path
                    .iter_mut()
                    .zip(path.as_os_str().as_encoded_bytes())
                {
                    *dst = *src as libc::c_char;
                }
                let rc = libc::connect(
                    fd.as_raw_fd(),
                    (&raw const addr).cast(),
                    std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
                );
                assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
                Self(fd)
            }
        }

        fn send(&self, request: &Request) {
            let bytes = request.encode();
            // SAFETY: a valid buffer.
            let n =
                unsafe { libc::send(self.0.as_raw_fd(), bytes.as_ptr().cast(), bytes.len(), 0) };
            assert_eq!(n, bytes.len() as isize);
        }

        fn recv(&self) -> Option<Message> {
            if !poll_readable(&[self.0.as_raw_fd()], 3000)[0] {
                return None;
            }
            let mut buf = vec![0u8; MAX_PACKET];
            // SAFETY: a valid buffer.
            let n =
                unsafe { libc::recv(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n <= 0 {
                return None;
            }
            Some(Message::decode(&buf[..n as usize]).expect("a packet of ours"))
        }

        fn call(&self, request: &Request) -> Message {
            self.send(request);
            self.recv().expect("a reply")
        }

        fn ok(&self, request: &Request) {
            assert_eq!(self.call(request), Message::ok(), "{request:?}");
        }
    }

    struct Rig {
        dir: PathBuf,
        socket: PathBuf,
        pads: Arc<Gamepads>,
        _broker: UinputBroker,
    }

    impl Rig {
        fn new(name: &str) -> Option<Self> {
            let uinput = Path::new("/dev/uinput");
            if std::fs::OpenOptions::new()
                .write(true)
                .open(uinput)
                .is_err()
            {
                eprintln!("skipped: no usable /dev/uinput");
                return None;
            }
            let dir =
                std::env::temp_dir().join(format!("cha-uinput-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let pads = Arc::new(
                Gamepads::new(uinput, Path::new(""), GamepadKind::Xbox360, &dir, None, 0).unwrap(),
            );
            let socket = dir.join("uinput.sock");
            let broker = UinputBroker::start(Arc::clone(&pads), &socket, None).unwrap();
            Some(Self {
                dir,
                socket,
                pads,
                _broker: broker,
            })
        }

        fn node_exists(&self, name: &str) -> bool {
            self.dir.join("dev").join(name).exists()
        }

        fn udev_entries(&self) -> usize {
            std::fs::read_dir(self.dir.join("udev/data"))
                .unwrap()
                .count()
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn name_bytes(s: &str) -> [u8; NAME_LEN] {
        let mut n = [0u8; NAME_LEN];
        n[..s.len()].copy_from_slice(s.as_bytes());
        n
    }

    /// What Steam's pad asks for: the buttons, sticks, triggers, hat and rumble.
    fn make_pad(c: &Client, ff: u32) {
        // The kernel refuses a device with force feedback and no effects.
        let evs: &[u16] = if ff == 0 {
            &[0, 1, 3]
        } else {
            &[0, 1, 3, 0x15]
        };
        for &ev in evs {
            c.ok(&Request::SetBit {
                kind: BitKind::Ev,
                code: ev,
            });
        }
        for code in (0x130..=0x13e).chain(0x220..=0x223) {
            c.ok(&Request::SetBit {
                kind: BitKind::Key,
                code,
            });
        }
        for code in [0u16, 1, 2, 3, 4, 5, 0x10, 0x11] {
            c.ok(&Request::SetBit {
                kind: BitKind::Abs,
                code,
            });
            c.ok(&Request::AbsSetup(AbsSetup {
                code,
                min: -32768,
                max: 32767,
                fuzz: 0,
                flat: 0,
                res: 0,
            }));
        }
        for code in [0x50u16, 0x51, 0x58, 0x5a, 0x60]
            .into_iter()
            .filter(|_| ff > 0)
        {
            c.ok(&Request::SetBit {
                kind: BitKind::Ff,
                code,
            });
        }
        c.ok(&Request::DevSetup(DevSetup {
            bustype: 0x06,
            vendor: 0x1234,
            product: 0x5678,
            version: 0x0102,
            ff_effects_max: ff,
            name: name_bytes("Microsoft X-Box 360 pad 0\x1b"),
        }));
        c.ok(&Request::Create);
    }

    fn sysfs(sysname: &str, file: &str) -> Option<String> {
        std::fs::read_to_string(format!("/sys/devices/virtual/input/{sysname}/{file}"))
            .ok()
            .map(|s| s.trim().to_string())
    }

    fn sysname_of(c: &Client) -> String {
        match c.call(&Request::GetSysname) {
            Message::Reply { errno: 0, payload } => String::from_utf8(payload).unwrap(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_client_makes_a_pad_and_its_end_removes_it() {
        let Some(rig) = Rig::new("make") else { return };
        let c = Client::connect(&rig.socket);
        // Refused: a keyboard key, before anything else.
        assert_eq!(
            c.call(&Request::SetBit {
                kind: BitKind::Key,
                code: 30
            }),
            Message::err(libc::EINVAL)
        );
        assert_eq!(c.call(&Request::GetSysname), Message::err(libc::ENOENT));
        make_pad(&c, 16);
        let sys = sysname_of(&c);
        // Our identity, the client's sanitised name and version.
        assert_eq!(sysfs(&sys, "id/vendor").as_deref(), Some("28de"));
        assert_eq!(sysfs(&sys, "id/product").as_deref(), Some("11ff"));
        assert_eq!(sysfs(&sys, "id/bustype").as_deref(), Some("0003"));
        assert_eq!(sysfs(&sys, "id/version").as_deref(), Some("0102"));
        assert_eq!(
            sysfs(&sys, "name").as_deref(),
            Some("Microsoft X-Box 360 pad 0?")
        );
        assert_eq!(sysfs(&sys, "phys").as_deref(), Some("cha/pad4"));
        let nodes: Vec<String> = std::fs::read_dir(format!("/sys/devices/virtual/input/{sys}"))
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("event") || n.starts_with("js"))
            .collect();
        assert!(nodes.iter().any(|n| n.starts_with("event")));
        for node in &nodes {
            assert!(rig.node_exists(node), "{node} is shared");
        }
        assert_eq!(rig.udev_entries(), nodes.len());
        let entry = std::fs::read_dir(rig.dir.join("udev/data"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let entry = std::fs::read_to_string(entry.path()).unwrap();
        for want in [
            "E:ID_INPUT_JOYSTICK=1",
            "E:ID_VENDOR_ID=28de",
            "E:ID_MODEL_ID=11ff",
        ] {
            assert!(entry.contains(want), "{entry}");
        }
        // Events pass, refused ones don't, a second create doesn't.
        c.ok(&Request::Events(vec![
            Event {
                kind: 1,
                code: 0x130,
                value: 1,
            },
            Event {
                kind: 3,
                code: 0,
                value: 1000,
            },
            Event {
                kind: 0,
                code: 0,
                value: 0,
            },
        ]));
        assert_eq!(
            c.call(&Request::Events(vec![Event {
                kind: 1,
                code: 30,
                value: 1
            }])),
            Message::err(libc::EINVAL)
        );
        assert_eq!(c.call(&Request::Create), Message::err(libc::EINVAL));
        // Its end removes the device, the nodes and the udev entries.
        drop(c);
        for _ in 0..100 {
            if sysfs(&sys, "name").is_none() && rig.udev_entries() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(sysfs(&sys, "name"), None, "the device is gone");
        assert_eq!(rig.udev_entries(), 0);
        for node in &nodes {
            assert!(!rig.node_exists(node));
        }
    }

    #[test]
    fn destroy_and_make_again_and_the_limit() {
        let Some(rig) = Rig::new("limit") else { return };
        let c = Client::connect(&rig.socket);
        make_pad(&c, 0);
        let first = sysname_of(&c);
        c.ok(&Request::Destroy);
        assert_eq!(sysfs(&first, "name"), None);
        make_pad(&c, 0);
        drop(c);
        let clients: Vec<Client> = (0..policy::MAX_DEVICES)
            .map(|_| {
                let c = Client::connect(&rig.socket);
                make_pad(&c, 0);
                c
            })
            .collect();
        // Slots are phys numbers; they run past our own pads'.
        let mut physes: Vec<String> = clients
            .iter()
            .map(|c| sysfs(&sysname_of(c), "phys").unwrap())
            .collect();
        physes.sort();
        assert_eq!(physes, ["cha/pad4", "cha/pad5", "cha/pad6", "cha/pad7"]);
        let extra = Client::connect(&rig.socket);
        extra.ok(&Request::SetBit {
            kind: BitKind::Ev,
            code: 1,
        });
        assert_eq!(extra.call(&Request::Create), Message::err(libc::EMFILE));
        // A refused effect count never reaches the kernel.
        let big = Client::connect(&rig.socket);
        assert_eq!(
            big.call(&Request::DevSetup(DevSetup {
                bustype: 3,
                vendor: 0,
                product: 0,
                version: 0,
                ff_effects_max: 17,
                name: [0; NAME_LEN],
            })),
            Message::err(libc::EINVAL)
        );
        drop(clients);
        let _ = &rig.pads;
    }

    #[test]
    fn force_feedback_goes_to_the_client_and_back() {
        let Some(rig) = Rig::new("ff") else { return };
        let c = Client::connect(&rig.socket);
        make_pad(&c, 16);
        let sys = sysname_of(&c);
        // The app's side. The test container may not allow evdev (device
        // cgroup): skip then.
        let Some(app) = open_node(&rig, &sys) else {
            return;
        };
        let app_fd = app.as_raw_fd();
        // EVIOCSFF: _IOW('E', 0x80, struct ff_effect); EVIOCRMFF: _IOW('E', 0x81, int).
        let evioc_w = |nr: libc::c_ulong, size: usize| {
            (1 << 30) | ((size as libc::c_ulong) << 16) | (libc::c_ulong::from(b'E') << 8) | nr
        };
        let eviocsff = evioc_w(0x80, std::mem::size_of::<FfEffect>());
        let eviocrmff = evioc_w(0x81, 4);
        let mut effect = FfEffect {
            kind: 0x50,
            id: -1,
            replay_length: 100,
            ..FfEffect::default()
        };
        effect.u.0[0..2].copy_from_slice(&0xc000u16.to_ne_bytes());
        // The app's ioctl blocks until the client answers: do it on a thread.
        let app_thread = std::thread::spawn(move || {
            // SAFETY: an ff_effect and a valid fd, kept open by `app` below.
            let rc = unsafe { libc::ioctl(app_fd, eviocsff, &mut effect) };
            (rc, effect.id)
        });
        let Some(Message::FfUpload {
            request_id, effect, ..
        }) = c.recv()
        else {
            panic!("no upload request");
        };
        assert_eq!(effect.kind, 0x50);
        assert_eq!(&effect.union[..2], &0xc000u16.to_le_bytes());
        c.send(&Request::FfDone {
            request_id,
            retval: 0,
        });
        let (rc, id) = app_thread.join().unwrap();
        assert_eq!(rc, 0);
        assert!(id >= 0);
        // A play comes through as an event.
        let play = InputEvent {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            kind: 0x15,
            code: id as u16,
            value: 1,
        };
        // SAFETY: a repr(C) event.
        assert!(
            unsafe {
                libc::write(
                    app_fd,
                    (&raw const play).cast(),
                    std::mem::size_of_val(&play),
                )
            } > 0
        );
        assert_eq!(
            c.recv(),
            Some(Message::Event(Event {
                kind: 0x15,
                code: id as u16,
                value: 1
            }))
        );
        // An erase the client fails fails for the app.
        let erase = std::thread::spawn(move || {
            // SAFETY: an int argument, by value.
            unsafe { libc::ioctl(app_fd, eviocrmff, libc::c_int::from(id)) }
        });
        // The kernel stops a playing effect before it erases it.
        assert_eq!(
            c.recv(),
            Some(Message::Event(Event {
                kind: 0x15,
                code: id as u16,
                value: 0
            }))
        );
        let Some(Message::FfErase {
            request_id,
            effect_id,
        }) = c.recv()
        else {
            panic!("no erase request");
        };
        assert_eq!(effect_id, id as u32);
        c.send(&Request::FfDone {
            request_id,
            retval: -libc::EIO,
        });
        assert_eq!(erase.join().unwrap(), -1);
        drop(app);
    }

    /// A shim test program (`images/steam/uinput-shim/test.c`) running against
    /// the rig's broker.
    struct Program {
        child: std::process::Child,
        out: std::io::BufReader<std::process::ChildStdout>,
        sysname: String,
        label: String,
    }

    impl Program {
        fn start(dir: &Path, rig: &Rig, arch: &str, mode: &str) -> Option<Self> {
            let program = dir.join(format!("test-{arch}"));
            if !program.exists() {
                eprintln!("skipped {arch}: no {}", program.display());
                return None;
            }
            let label = format!("{arch} {mode}");
            let mut child = std::process::Command::new(&program)
                .arg(mode)
                .env("CHA_UINPUT_SOCK", &rig.socket)
                // Kept beside the programs, for when a test fails.
                .env(
                    "CHA_UINPUT_LOG",
                    dir.join(format!("shim-{arch}-{mode}.log")),
                )
                .env("LD_PRELOAD", dir.join(format!("libcha-uinput-{arch}.so")))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
            let sysname = Self::line(&mut out, &label)
                .strip_prefix("sysname ")
                .unwrap_or_else(|| panic!("{label}: no sysname"))
                .to_string();
            Some(Self {
                child,
                out,
                sysname,
                label,
            })
        }

        fn line(out: &mut impl std::io::BufRead, label: &str) -> String {
            let mut line = String::new();
            let n = out.read_line(&mut line).unwrap();
            assert!(n > 0, "{label}: the program ended early");
            line.trim().to_string()
        }

        fn next_line(&mut self) -> String {
            Self::line(&mut self.out, &self.label)
        }

        /// Lets it finish, and checks it succeeded and its device is gone.
        fn finish(mut self) {
            use std::io::Write;
            // The programs that wait for "go" get it; the others have closed stdin.
            let _ = self.child.stdin.take().map(|mut i| i.write_all(b"go\n"));
            let status = self.child.wait().unwrap();
            assert!(status.success(), "{}: {status}", self.label);
            for _ in 0..100 {
                if sysfs(&self.sysname, "name").is_none() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(
                sysfs(&self.sysname, "name"),
                None,
                "{}: destroyed",
                self.label
            );
        }
    }

    #[test]
    fn shim_programs_make_pads_with_both_uinput_apis() {
        // images/steam/uinput-shim/run-tests.sh builds the programs and names
        // the directory in CHA_SHIM_TESTS: `test-i386`, `test-amd64` and the
        // libraries as `libcha-uinput-<arch>.so`.
        let Some(dir) = std::env::var_os("CHA_SHIM_TESTS").map(PathBuf::from) else {
            eprintln!("skipped: CHA_SHIM_TESTS isn't set");
            return;
        };
        let Some(rig) = Rig::new("shim") else { return };
        for arch in ["amd64", "i386"] {
            for mode in ["new", "legacy"] {
                let Some(p) = Program::start(&dir, &rig, arch, mode) else {
                    continue;
                };
                let (sys, label) = (p.sysname.clone(), p.label.clone());
                assert_eq!(sysfs(&sys, "id/vendor").as_deref(), Some("28de"), "{label}");
                assert_eq!(
                    sysfs(&sys, "id/product").as_deref(),
                    Some("11ff"),
                    "{label}"
                );
                assert_eq!(
                    sysfs(&sys, "id/version").as_deref(),
                    Some("0100"),
                    "{label}"
                );
                assert_eq!(
                    sysfs(&sys, "name").as_deref(),
                    Some("Shim Test Pad"),
                    "{label}"
                );
                assert_eq!(sysfs(&sys, "phys").as_deref(), Some("cha/pad4"), "{label}");
                p.finish();
            }
        }
    }

    #[test]
    fn shim_programs_answer_force_feedback() {
        let Some(dir) = std::env::var_os("CHA_SHIM_TESTS").map(PathBuf::from) else {
            eprintln!("skipped: CHA_SHIM_TESTS isn't set");
            return;
        };
        let Some(rig) = Rig::new("shimff") else {
            return;
        };
        for arch in ["amd64", "i386"] {
            let Some(mut p) = Program::start(&dir, &rig, arch, "ff") else {
                continue;
            };
            let Some(app) = open_node(&rig, &p.sysname) else {
                // Let the program time out its own way: kill it.
                let _ = p.child.kill();
                let _ = p.child.wait();
                return;
            };
            let fd = app.as_raw_fd();
            let evioc_w = |nr: libc::c_ulong, size: usize| {
                (1 << 30) | ((size as libc::c_ulong) << 16) | (libc::c_ulong::from(b'E') << 8) | nr
            };
            let mut effect = FfEffect {
                kind: 0x50,
                id: -1,
                replay_length: 100,
                ..FfEffect::default()
            };
            effect.u.0[0..2].copy_from_slice(&0xc000u16.to_ne_bytes());
            // The app's ioctl waits for the program's answer.
            let eviocsff = evioc_w(0x80, std::mem::size_of::<FfEffect>());
            // SAFETY: an ff_effect and a valid fd.
            assert_eq!(
                unsafe { libc::ioctl(fd, eviocsff, &mut effect) },
                0,
                "{arch}"
            );
            assert!(p.next_line().starts_with("ff upload ok"), "{arch}");
            let play = InputEvent {
                time: libc::timeval {
                    tv_sec: 0,
                    tv_usec: 0,
                },
                kind: 0x15,
                code: effect.id as u16,
                value: 1,
            };
            // SAFETY: a repr(C) event.
            let n =
                unsafe { libc::write(fd, (&raw const play).cast(), std::mem::size_of_val(&play)) };
            assert!(n > 0);
            assert_eq!(p.next_line(), format!("ff play {} 1", effect.id), "{arch}");
            // The program fails the erase, so the app's ioctl fails.
            let eviocrmff = evioc_w(0x81, 4);
            // SAFETY: an int argument, by value.
            let rc = unsafe { libc::ioctl(fd, eviocrmff, libc::c_int::from(effect.id)) };
            assert_eq!(rc, -1, "{arch}");
            // The kernel stops a playing effect before it erases it.
            assert_eq!(p.next_line(), format!("ff play {} 0", effect.id), "{arch}");
            assert_eq!(p.next_line(), format!("ff erase {}", effect.id), "{arch}");
            assert_eq!(p.next_line(), "ff done", "{arch}");
            drop(app);
            p.finish();
        }
    }

    /// The app's side: the node the broker shared, if the container lets us
    /// open evdev.
    fn open_node(rig: &Rig, sysname: &str) -> Option<std::fs::File> {
        let node = std::fs::read_dir(format!("/sys/devices/virtual/input/{sysname}"))
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .find(|n| n.starts_with("event"))
            .unwrap();
        let path = rig.dir.join("dev").join(node);
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(f) => Some(f),
            Err(err) => {
                eprintln!("skipped: can't open {}: {err}", path.display());
                None
            }
        }
    }
}
