//! A virtual pad as a real USB device: the streamer serves USB/IP on one end of
//! a `socketpair` and hands the other to the kernel's `vhci_hcd` through
//! `/sys/devices/platform/vhci_hcd.0/attach`. The kernel enumerates a full-speed
//! USB device, binds `usbhid`, and makes a `hidraw` node, as for a controller
//! on a cable (opt-in, `CHA_STEAM_PAD_TRANSPORT=usbip`; see `docs/controllers.md`).
//! The protocol and the device model are [`crate::usbip`] and [`crate::usbdev`];
//! what the reports mean is a [`Protocol`]'s business, as with [`crate::uhid`].
//!
//! A session can also **relay** the real controller through the device
//! ([`UsbPad::relay_open`], [`crate::hid_relay`]): its input reports complete
//! the parked interrupt-IN URBs verbatim, the app's feature and output
//! reports go to the page, and the control URBs wait for the page's reply
//! (a timeout, an error or a lost page falls back on the emulation that
//! answers when nothing is relayed). While it is open the emulated state is
//! not sent: the repeat thread only watches the requests' deadlines.
//!
//! - **Attach.** `"<port> <sockfd> <devid> <speed>"`, a free port of the `hs`
//!   hub (full speed goes there). The kernel keeps its own reference to the
//!   socket. The attach file is mode 0200, root: the streamer needs write
//!   access to that one sysfs directory (the node binds it in).
//! - **Detach.** `detach` with the port, on [`UsbPad::destroy`] or drop. If the
//!   streamer dies without it (kill, abort), its end of the socket closes with
//!   the process, `vhci_rx` reads EOF and the kernel detaches the port itself
//!   (`usb 9-1: USB disconnect`); nothing outlives the process but the moment
//!   it takes the kernel to notice.
//! - **Where the nodes are.** `status` names the port's USB device (`9-1`); the
//!   HID device is `usbN/9-1/9-1:1.0/0003:VVVV:PPPP.NNNN` under the controller.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::gamepad::{PadEvent, PadState};
use crate::hid_relay::{
    Answer, InFlight, Relay, RelayStats, Request, STEAM_LIZARD_REPORTS, Verdict,
};
use crate::uhid::{self, FEATURE_REPORT, Found, OUTPUT_REPORT, Protocol};
use crate::usbdev::{Engine, Hid, Identity, REPORT_FEATURE, REPORT_OUTPUT, Reply};
use crate::usbip::{self, HEADER_LEN};

pub const VHCI: &str = "/sys/devices/platform/vhci_hcd.0";
/// `USB_SPEED_FULL` in `enum usb_device_speed`.
const SPEED_FULL: u32 = 2;
/// `VDEV_ST_NULL` and `VDEV_ST_USED` in `status`' `sta` column.
const STA_NULL: u32 = 4;
const STA_USED: u32 = 6;
/// busnum 1, devnum 2: what the kernel puts in its headers' `devid`.
const DEVID: u32 = (1 << 16) | 2;

/// One row of `status`: `hub port sta spd dev sockfd local_busid`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    hub: String,
    port: u32,
    sta: u32,
    busid: String,
}

fn parse_status(text: &str) -> Vec<Row> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let hub = f.next()?.to_string();
            let port = f.next()?.parse().ok()?;
            let sta = f.next()?.parse().ok()?;
            // spd, dev, sockfd, then the busid (blank on a free port).
            let busid = f.nth(3).unwrap_or_default().to_string();
            Some(Row {
                hub,
                port,
                sta,
                busid,
            })
        })
        .collect()
}

fn status_rows() -> io::Result<Vec<Row>> {
    Ok(parse_status(&fs::read_to_string(format!("{VHCI}/status"))?))
}

/// Whether the attach file can be written: the module is loaded and the
/// directory is ours to write.
pub fn available() -> Result<()> {
    OpenOptions::new()
        .write(true)
        .open(format!("{VHCI}/attach"))
        .map(drop)
        .with_context(|| {
            format!("opening {VHCI}/attach (is vhci-hcd loaded and the directory writable?)")
        })
}

fn sysfs_write(file: &str, text: &str) -> io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .open(format!("{VHCI}/{file}"))?;
    f.write_all(text.as_bytes())
}

/// Attaches `sock` to a free port and says which. A port another streamer
/// took first gives EBUSY; the next free one is tried.
pub(crate) fn attach(sock: &UnixStream) -> Result<u32> {
    let mut last = None;
    for row in status_rows()?
        .into_iter()
        .filter(|r| r.hub == "hs" && r.sta == STA_NULL)
    {
        match sysfs_write(
            "attach",
            &format!("{} {} {DEVID} {SPEED_FULL}", row.port, sock.as_raw_fd()),
        ) {
            Ok(()) => return Ok(row.port),
            Err(err) if err.raw_os_error() == Some(libc::EBUSY) => last = Some(err),
            Err(err) => return Err(err).context("writing vhci_hcd attach"),
        }
    }
    match last {
        Some(err) => Err(err).context("every hs port of vhci_hcd was taken"),
        None => bail!("no free hs port in {VHCI}/status"),
    }
}

pub(crate) fn detach(port: u32) -> io::Result<()> {
    sysfs_write("detach", &port.to_string())
}

/// The sysfs directory of the HID device of interface `number` of the USB
/// device `busid` (`9-1`), once the kernel has made it.
fn hid_dir(busid: &str, number: usize) -> Option<PathBuf> {
    for hub in fs::read_dir(VHCI).ok()?.flatten() {
        if !hub.file_name().to_string_lossy().starts_with("usb") {
            continue;
        }
        let interface = hub.path().join(busid).join(format!("{busid}:1.{number}"));
        for entry in fs::read_dir(interface).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().starts_with("0003:") {
                return Some(entry.path());
            }
        }
    }
    None
}

/// Waits for the port to be in use, and then for the HID device of interface
/// `number`'s driver to bind and make its nodes ([`uhid::wait_until`]).
pub(crate) fn wait_for_nodes(
    port: u32,
    number: usize,
    driver: &str,
    within: Duration,
) -> Result<Found> {
    let deadline = Instant::now() + within;
    let busid = loop {
        let busid = status_rows()?
            .into_iter()
            .find(|r| r.hub == "hs" && r.port == port && r.sta == STA_USED && !r.busid.is_empty())
            .map(|r| r.busid);
        match busid {
            Some(busid) => break busid,
            None if Instant::now() > deadline => {
                bail!("vhci_hcd port {port} never came into use")
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    };
    let left = deadline.saturating_duration_since(Instant::now());
    uhid::wait_until(
        &format!("vhci_hcd port {port} ({busid}:1.{number})"),
        || {
            hid_dir(&busid, number)
                .map(|p| uhid::scan_hid(&p))
                .unwrap_or_default()
        },
        driver,
        0,
        left.max(Duration::from_secs(1)),
    )
}

/// What the device model asks of the pad's protocol: HID class requests and
/// interrupt OUT data, as the uhid thread answers `GET_REPORT`, `SET_REPORT`
/// and `OUTPUT`. With a relay open they go to the page first.
struct Function<'a> {
    protocol: &'a mut dyn Protocol,
    events: &'a broadcast::Sender<PadEvent>,
    relay: &'a mut Relay,
    now: Instant,
}

fn emit(events: &broadcast::Sender<PadEvent>, emitted: Vec<PadEvent>) {
    for event in emitted {
        // Nobody listening is fine.
        let _ = events.send(event);
    }
}

pub(crate) fn emulated_get(protocol: &mut dyn Protocol, kind: u8, id: u8) -> Result<Vec<u8>, i32> {
    let kind = match kind {
        REPORT_FEATURE => FEATURE_REPORT,
        REPORT_OUTPUT => OUTPUT_REPORT,
        // An input report asked for: the protocols answer feature reports only.
        _ => return Err(libc::EINVAL),
    };
    protocol.get_report(id, kind)
}

pub(crate) fn emulated_set(
    protocol: &mut dyn Protocol,
    events: &broadcast::Sender<PadEvent>,
    kind: u8,
    id: u8,
    data: &[u8],
) -> Result<(), i32> {
    match kind {
        REPORT_FEATURE => {
            let emitted = protocol.set_report(id, FEATURE_REPORT, data)?;
            emit(events, emitted);
            Ok(())
        }
        REPORT_OUTPUT => {
            let emitted = protocol.output(OUTPUT_REPORT, data);
            emit(events, emitted);
            Ok(())
        }
        _ => Err(libc::EINVAL),
    }
}

impl Function<'_> {
    /// Sends `request` to the sessions; false if nobody is there to hear it.
    fn ask(&self, request: crate::gamepad::HidRequest) -> bool {
        self.events.send(PadEvent::Hid(request)).is_ok()
    }
}

impl Hid for Function<'_> {
    fn get_report(&mut self, _iface: usize, kind: u8, id: u8, _w_length: u16) -> Reply<Vec<u8>> {
        if let Some((ticket, request)) = self.relay.request_get(kind, id, self.now) {
            if self.ask(request) {
                return Reply::Pending(ticket);
            }
            self.relay.cancel(ticket);
        }
        Reply::Done(emulated_get(self.protocol, kind, id))
    }

    fn set_report(&mut self, _iface: usize, kind: u8, id: u8, data: &[u8]) -> Reply<()> {
        if let Some((ticket, request)) = self.relay.request_set(kind, id, data, self.now) {
            if self.ask(request) {
                // The emulated device hears it too (its events dropped: the
                // real controller does the work), so that if the page fails to
                // answer, the emulation's reply to the following get is this
                // command's and not a stale one.
                if kind == REPORT_FEATURE {
                    let _ = self.protocol.set_report(id, FEATURE_REPORT, data);
                }
                return Reply::Pending(ticket);
            }
            self.relay.cancel(ticket);
        }
        Reply::Done(emulated_set(self.protocol, self.events, kind, id, data))
    }

    fn output(&mut self, _iface: usize, data: &[u8]) {
        // The real controller owns its motors and trackpads: an output report
        // it doesn't get (over the rate, or none of the device's) is dropped,
        // not turned into a rumble for the page to play on the same device.
        if self.relay.is_active() {
            if let Some(request) = self.relay.request_output(data, self.now) {
                self.ask(request);
            }
            return;
        }
        let emitted = self.protocol.output(OUTPUT_REPORT, data);
        emit(self.events, emitted);
    }
}

pub(crate) struct Inner {
    engine: Engine,
    protocol: Box<dyn Protocol>,
    state: PadState,
    last_sent: Instant,
    sock: UnixStream,
    events: broadcast::Sender<PadEvent>,
    relay: Relay,
}

impl Inner {
    /// The protocol's reports for the current state, to the parked URBs. Not
    /// while the real controller's own are relayed.
    fn send_state(&mut self) {
        if self.relay.is_active() {
            return;
        }
        let now = Instant::now();
        let reports = self.protocol.report(&self.state, now);
        self.last_sent = now;
        let mut out = Vec::with_capacity(HEADER_LEN + 64);
        for report in reports {
            self.engine.push_report(&report, &mut out);
        }
        self.write(&out);
    }

    fn write(&mut self, out: &[u8]) {
        if !out.is_empty()
            && let Err(err) = self.sock.write_all(out)
        {
            debug!("usbip write: {err}");
        }
    }

    /// One command from the kernel, answered (or held, for the relay).
    fn serve(&mut self, cmd: &usbip::Command, body: &[u8]) {
        let mut out = Vec::new();
        let mut function = Function {
            protocol: &mut *self.protocol,
            events: &self.events,
            relay: &mut self.relay,
            now: Instant::now(),
        };
        self.engine.on_command(cmd, body, &mut out, &mut function);
        for (_, ticket) in self.engine.take_cancelled() {
            self.relay.cancel(ticket);
        }
        self.write(&out);
    }

    /// Completes held control URBs: with the page's answer, or the emulated
    /// device's.
    fn answer(&mut self, answers: Vec<(InFlight, Answer)>) {
        let mut out = Vec::new();
        for (held, answer) in answers {
            let result = match answer {
                Answer::Relayed(data) => Ok(data),
                Answer::Emulate => match &held.req {
                    Request::Get { kind, id } => emulated_get(&mut *self.protocol, *kind, *id),
                    Request::Set { kind, id, data } => {
                        emulated_set(&mut *self.protocol, &self.events, *kind, *id, data)
                            .map(|()| Vec::new())
                    }
                },
            };
            self.engine
                .complete_control(0, held.ticket, result, &mut out);
        }
        self.write(&out);
    }

    /// The device is the emulated one again after a relay that was `was`
    /// active: at rest, until the page sends a state.
    fn after_relay(&mut self, was_active: bool) {
        if was_active && !self.relay.is_active() {
            self.state = PadState::default();
            self.send_state();
        }
    }

    fn close_relay(&mut self, owner: Option<u64>, why: &str) {
        let was = self.relay.is_active();
        let left = self.relay.close(owner, why);
        self.answer(left.into_iter().map(|f| (f, Answer::Emulate)).collect());
        if was && !self.relay.is_active() {
            self.engine.clear_pending();
        }
        self.after_relay(was);
    }

    /// Answers the requests whose time is up.
    fn expire(&mut self, now: Instant) {
        let was = self.relay.is_active();
        let due = self.relay.expire(now);
        for request in due.resend {
            // A reply that wasn't the command's is asked for again.
            let _ = self.events.send(PadEvent::Hid(request));
        }
        if !due.answers.is_empty() {
            self.answer(due.answers);
        }
        self.after_relay(was);
    }
}

/// A device served on a USB/IP socket: where the kernel's URBs go.
pub(crate) trait Serve {
    fn serve(&mut self, cmd: &usbip::Command, body: &[u8]);
}

impl Serve for Inner {
    fn serve(&mut self, cmd: &usbip::Command, body: &[u8]) {
        Inner::serve(self, cmd, body);
    }
}

pub(crate) struct Shared<T = Inner> {
    pub(crate) inner: Mutex<T>,
    pub(crate) stop: AtomicBool,
}

/// A pad that is a USB device, served from two threads: one reads the kernel's
/// URBs, one repeats the report each period (real controllers report
/// continuously, and clients wait for the first one).
pub struct UsbPad {
    shared: Arc<Shared>,
    /// The vhci port; `None` for a device served on some other socket (tests).
    port: Option<u32>,
    sock: UnixStream,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl UsbPad {
    /// Attaches the device to a free port of `vhci_hcd` and serves it. The
    /// kernel's driver needs a moment to bind: [`UsbPad::wait_for_nodes`].
    pub fn attach(
        identity: Identity,
        protocol: Box<dyn Protocol>,
        events: broadcast::Sender<PadEvent>,
        name: &str,
        slot: usize,
    ) -> Result<Arc<Self>> {
        let (ours, theirs) = UnixStream::pair().context("making the socketpair")?;
        let port = attach(&theirs)?;
        // The kernel holds its own reference now.
        drop(theirs);
        match Self::serve(ours, Some(port), identity, protocol, events, name, slot) {
            Ok(pad) => Ok(pad),
            Err(err) => {
                let _ = detach(port);
                Err(err)
            }
        }
    }

    /// Serves the device on `sock`, the kernel's other end.
    fn serve(
        sock: UnixStream,
        port: Option<u32>,
        identity: Identity,
        protocol: Box<dyn Protocol>,
        events: broadcast::Sender<PadEvent>,
        name: &str,
        slot: usize,
    ) -> Result<Arc<Self>> {
        let reader = sock.try_clone()?;
        let relay = Relay::new(
            slot,
            identity.report_descriptor,
            STEAM_LIZARD_REPORTS,
            Instant::now(),
        );
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                engine: Engine::new(identity),
                protocol,
                state: PadState::default(),
                last_sent: Instant::now(),
                sock: sock.try_clone()?,
                events,
                relay,
            }),
            stop: AtomicBool::new(false),
        });
        let rx = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name(format!("{name}-usbip"))
                .spawn(move || read_urbs(reader, &shared))
                .context("spawning the usbip thread")?
        };
        let repeat = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name(format!("{name}-repeat"))
                .spawn(move || repeat_reports(&shared))
                .context("spawning the repeat thread")?
        };
        Ok(Arc::new(Self {
            shared,
            port,
            sock,
            threads: Mutex::new(vec![rx, repeat]),
        }))
    }

    /// Waits for the kernel to enumerate the device, bind `driver` and make
    /// its `hidraw` node.
    pub fn wait_for_nodes(&self, driver: &str, within: Duration) -> Result<Found> {
        let port = self.port.context("not attached to vhci_hcd")?;
        wait_for_nodes(port, 0, driver, within)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.shared.inner.lock().expect("usb pad lock")
    }

    /// A new state from the page: the report goes out at once. While the real
    /// controller is relayed the page's parsed state is ignored (the reports
    /// are the controller's own), except `gone`, which ends the relay.
    pub fn update(&self, state: &PadState) {
        let mut inner = self.lock();
        if state.gone {
            inner.close_relay(None, "the pad is gone");
        } else if inner.relay.is_active() {
            return;
        }
        inner.state = state.clone();
        inner.send_state();
    }

    /// `owner`'s session passes its real controller through this device from
    /// now on.
    pub fn relay_open(&self, owner: u64) {
        let mut inner = self.lock();
        let left = inner.relay.open(owner, Instant::now());
        inner.answer(left.into_iter().map(|f| (f, Answer::Emulate)).collect());
        // Reports the emulation queued while no URB waited are not the controller's.
        inner.engine.clear_pending();
    }

    /// Ends the relay (`owner`'s only, if given) and goes back to the
    /// emulated device at rest.
    pub fn relay_close(&self, owner: Option<u64>, why: &str) {
        self.lock().close_relay(owner, why);
    }

    /// The session that holds the relay.
    #[cfg(test)]
    pub fn relay_owner(&self) -> Option<u64> {
        self.lock().relay.owner()
    }

    /// A report of the real controller (id first), numbered `q`: if it
    /// passes, it completes the oldest parked interrupt-IN URB.
    pub fn relay_input(&self, owner: u64, q: u32, report: &[u8]) -> Verdict {
        let mut inner = self.lock();
        let verdict = inner.relay.input(owner, q, report, Instant::now());
        if verdict == Verdict::Accept {
            let mut out = Vec::with_capacity(HEADER_LEN + report.len());
            inner.engine.push_report(report, &mut out);
            inner.write(&out);
        }
        verdict
    }

    /// The page's answer to the request under `ticket`.
    pub fn relay_reply(&self, owner: u64, ticket: u32, ok: bool, data: &[u8], error: Option<&str>) {
        let mut inner = self.lock();
        let was = inner.relay.is_active();
        let answers = inner
            .relay
            .reply(owner, ticket, ok, data, error, Instant::now());
        inner.answer(answers);
        inner.after_relay(was);
    }

    pub fn relay_stats(&self) -> RelayStats {
        self.lock().relay.stats()
    }

    /// Takes the device away: detaches the port (the kernel disconnects the
    /// device and removes its nodes) and ends the threads. Safe to repeat.
    pub fn destroy(&self) {
        if self.shared.stop.swap(true, Ordering::Relaxed) {
            return;
        }
        if let Some(port) = self.port
            && let Err(err) = detach(port)
        {
            warn!(port, "vhci_hcd detach: {err}");
        }
        // Wakes the reader if the kernel is slow to close its end.
        let _ = self.sock.shutdown(Shutdown::Both);
        let me = thread::current().id();
        let threads = std::mem::take(&mut *self.threads.lock().expect("threads"));
        for thread in threads {
            if thread.thread().id() != me {
                let _ = thread.join();
            }
        }
    }
}

impl Drop for UsbPad {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// The kernel's side of the conversation: a header, its body, an answer.
pub(crate) fn read_urbs<T: Serve>(mut sock: UnixStream, shared: &Shared<T>) {
    loop {
        let mut head = [0u8; HEADER_LEN];
        if sock.read_exact(&mut head).is_err() {
            break;
        }
        let cmd = match usbip::parse_header(&head) {
            Ok(cmd) => cmd,
            Err(err) => {
                warn!("usbip: {err}");
                break;
            }
        };
        let mut body = vec![0u8; usbip::body_len(&cmd)];
        if sock.read_exact(&mut body).is_err() {
            break;
        }
        shared
            .inner
            .lock()
            .expect("usb pad lock")
            .serve(&cmd, &body);
    }
    if !shared.stop.load(Ordering::Relaxed) {
        warn!("the kernel closed the virtual USB controller's socket");
    }
}

/// Sends the state again whenever a period passes without a report. While the
/// real controller is relayed it sends nothing (the controller's own reports
/// are the device's) and only watches the page's deadlines.
fn repeat_reports(shared: &Shared) {
    const IDLE: Duration = Duration::from_millis(50);
    while !shared.stop.load(Ordering::Relaxed) {
        let wait = {
            let mut inner = shared.inner.lock().expect("usb pad lock");
            let now = Instant::now();
            inner.expire(now);
            let mut wait = IDLE;
            if inner.relay.is_active() {
                if let Some(deadline) = inner.relay.next_deadline() {
                    wait = wait.min(deadline.saturating_duration_since(now));
                }
            } else {
                let period = inner.protocol.period();
                if inner.last_sent.elapsed() >= period {
                    inner.send_state();
                }
                wait =
                    wait.min((inner.last_sent + period).saturating_duration_since(Instant::now()));
            }
            wait.max(Duration::from_millis(1))
        };
        thread::sleep(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam_controller::{SteamController, Variant};
    use crate::usbip::{DIR_IN, DIR_OUT, NO_ISO, Submit, encode_cmd_submit, encode_cmd_unlink};

    #[test]
    fn status_rows_are_read() {
        let text = "hub port sta spd dev      sockfd local_busid\n\
                    hs  0000 006 002 00010002 000004 9-1\n\
                    hs  0001 004 000 00000000 000000 0-0\n\
                    ss  0008 004 000 00000000 000000 0-0\n";
        let rows = parse_status(text);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            Row {
                hub: "hs".into(),
                port: 0,
                sta: STA_USED,
                busid: "9-1".into()
            }
        );
        assert_eq!((rows[1].port, rows[1].sta), (1, STA_NULL));
        assert_eq!(rows[2].hub, "ss");
        // A row cut short is skipped, not a panic.
        assert!(parse_status("header\nhs 0000\n").is_empty());
    }

    fn submit(seqnum: u32, direction: u32, ep: u32, length: u32, setup: [u8; 8]) -> Submit {
        Submit {
            seqnum,
            devid: DEVID,
            direction,
            ep,
            transfer_flags: 0,
            length,
            start_frame: 0,
            number_of_packets: NO_ISO,
            interval: 0,
            setup,
        }
    }

    /// The kernel's end: sends a command, reads the reply header (and data).
    struct Kernel(UnixStream);

    impl Kernel {
        fn send(&mut self, s: &Submit, body: &[u8]) {
            let mut wire = Vec::new();
            encode_cmd_submit(&mut wire, s, body);
            self.0.write_all(&wire).unwrap();
        }

        /// A reply to an OUT or unlink: (command, seqnum, status).
        fn reply(&mut self) -> (u32, u32, i32) {
            let (cmd, seq, status, _) = self.read(false);
            (cmd, seq, status)
        }

        /// A reply to an IN transfer: (command, seqnum, status, data).
        fn reply_in(&mut self) -> (u32, u32, i32, Vec<u8>) {
            self.read(true)
        }

        fn read(&mut self, data_follows: bool) -> (u32, u32, i32, Vec<u8>) {
            let mut head = [0u8; HEADER_LEN];
            self.0.read_exact(&mut head).unwrap();
            let word = |at: usize| u32::from_be_bytes(head[at..at + 4].try_into().unwrap());
            let mut data = vec![0u8; if data_follows { word(24) as usize } else { 0 }];
            self.0.read_exact(&mut data).unwrap();
            (word(0), word(4), word(20) as i32, data)
        }
    }

    fn pad() -> (Arc<UsbPad>, Kernel, broadcast::Receiver<PadEvent>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (events, rx) = broadcast::channel(16);
        let variant = Variant::TritonUsb;
        let identity = Identity {
            vendor: 0x28de,
            product: variant.product(),
            bcd_device: variant.version(),
            manufacturer: "Valve Software".into(),
            product_name: variant.name().into(),
            serial: "FXA0123456789".into(),
            report_descriptor: variant.descriptor(),
            interfaces: Vec::new(),
        };
        let protocol = Box::new(SteamController::new(variant, 1, "FXA0123456789"));
        let pad = UsbPad::serve(ours, None, identity, protocol, events, "test", 0).unwrap();
        (pad, Kernel(theirs), rx)
    }

    #[test]
    fn the_kernel_enumerates_reads_features_and_gets_reports() {
        let (pad, mut kernel, _events) = pad();
        // GET_DESCRIPTOR(device): idVendor and idProduct.
        kernel.send(&submit(1, DIR_IN, 0, 18, [0x80, 6, 0, 1, 0, 0, 18, 0]), &[]);
        let (cmd, seq, status, dev) = kernel.reply_in();
        assert_eq!((cmd, seq, status), (usbip::RET_SUBMIT, 1, 0));
        assert_eq!(&dev[8..12], &[0xde, 0x28, 0x02, 0x13]);
        // Feature report 1 (a command: get attributes 0x83), then read it back.
        let mut set = vec![1u8, 0x83];
        set.resize(64, 0);
        kernel.send(
            &submit(2, DIR_OUT, 0, 64, [0x21, 9, 1, 3, 0, 0, 64, 0]),
            &set,
        );
        assert_eq!(kernel.reply().2, 0);
        kernel.send(&submit(3, DIR_IN, 0, 64, [0xa1, 1, 1, 3, 0, 0, 64, 0]), &[]);
        let (_, _, status, feature) = kernel.reply_in();
        assert_eq!(status, 0);
        assert_eq!(feature.len(), 64);
        assert_eq!(&feature[..2], &[1, 0x83]);
        // An interrupt IN URB parks until the page's state makes a report.
        kernel.send(&submit(4, DIR_IN, 1, 64, [0; 8]), &[]);
        pad.update(&PadState::default());
        let (_, seq, status, report) = kernel.reply_in();
        assert_eq!((seq, status), (4, 0));
        // The state, or the battery the repeat thread queued before it.
        assert!(matches!(report[0], 0x42 | 0x43), "{report:02x?}");
        pad.destroy();
    }

    #[test]
    fn rumble_by_interrupt_out_and_by_set_report_are_events() {
        let (pad, mut kernel, mut events) = pad();
        let rumble = |lo: u8| vec![0x80, 0, 1, 0, lo, 0, 0, 0, 0, 0];
        // The interrupt OUT endpoint.
        kernel.send(&submit(1, DIR_OUT, 1, 10, [0; 8]), &rumble(0xff));
        assert_eq!(kernel.reply().2, 0);
        match events.try_recv() {
            Ok(PadEvent::Rumble(r)) => assert!(r.lo > 0.0 && r.hi == 0.0),
            other => panic!("{other:?}"),
        }
        // The same motors by SET_REPORT(output): no change, no event; then off.
        kernel.send(
            &submit(2, DIR_OUT, 0, 10, [0x21, 9, 0x80, 2, 0, 0, 10, 0]),
            &rumble(0xff),
        );
        assert_eq!(kernel.reply().2, 0);
        assert!(events.try_recv().is_err());
        kernel.send(
            &submit(3, DIR_OUT, 0, 10, [0x21, 9, 0x80, 2, 0, 0, 10, 0]),
            &rumble(0),
        );
        assert_eq!(kernel.reply().2, 0);
        assert!(matches!(events.try_recv(), Ok(PadEvent::Rumble(_))));
        pad.destroy();
    }

    #[test]
    fn the_report_repeats_while_a_urb_waits() {
        let (pad, mut kernel, _events) = pad();
        // Two URBs and no update: the repeat thread answers both within a
        // few periods (4 ms each).
        kernel.send(&submit(1, DIR_IN, 1, 64, [0; 8]), &[]);
        kernel.send(&submit(2, DIR_IN, 1, 64, [0; 8]), &[]);
        let (_, a, _, first) = kernel.reply_in();
        let (_, b, _, second) = kernel.reply_in();
        assert_eq!((a, b), (1, 2));
        // The state, then (the first period) the battery.
        assert_eq!(first[0], 0x42);
        assert!(matches!(second[0], 0x42 | 0x43));
        pad.destroy();
    }

    #[test]
    fn an_unlink_is_answered_and_destroy_is_repeatable() {
        let (pad, mut kernel, _events) = pad();
        kernel.send(&submit(1, DIR_IN, 1, 64, [0; 8]), &[]);
        // The repeat thread may answer the URB before the unlink arrives; both
        // replies are legal, so read until the unlink's.
        let mut wire = Vec::new();
        encode_cmd_unlink(&mut wire, 2, DEVID, 1);
        kernel.0.write_all(&wire).unwrap();
        loop {
            let (cmd, seq, status, _) = kernel.reply_in();
            if cmd == usbip::RET_UNLINK {
                assert_eq!(seq, 2);
                assert!(status == 0 || status == -usbip::ECONNRESET);
                break;
            }
        }
        pad.destroy();
        pad.destroy();
    }

    // ---- the raw HID relay ----

    use crate::gamepad::{HidOp, HidRequest};

    const OWNER: u64 = 5;

    /// The next relay request the pad asked the sessions for.
    fn request(events: &mut broadcast::Receiver<PadEvent>) -> HidRequest {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            match events.try_recv() {
                Ok(PadEvent::Hid(r)) => return r,
                Ok(_) => {}
                Err(_) if Instant::now() < until => thread::sleep(Duration::from_millis(2)),
                Err(err) => panic!("no relay request: {err:?}"),
            }
        }
    }

    /// Whether the kernel hears anything within `ms`.
    fn silent(kernel: &mut Kernel, ms: u64) -> bool {
        kernel
            .0
            .set_read_timeout(Some(Duration::from_millis(ms)))
            .unwrap();
        let mut head = [0u8; 1];
        let silent = kernel.0.read_exact(&mut head).is_err();
        kernel
            .0
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        silent
    }

    const GET_FEATURE_1: [u8; 8] = [0xa1, 1, 1, 3, 0, 0, 64, 0];
    const SET_FEATURE_1: [u8; 8] = [0x21, 9, 1, 3, 0, 0, 64, 0];

    #[test]
    fn a_relayed_feature_get_waits_for_the_pages_reply() {
        let (pad, mut kernel, mut events) = pad();
        pad.relay_open(OWNER);
        kernel.send(&submit(1, DIR_IN, 0, 64, GET_FEATURE_1), &[]);
        let req = request(&mut events);
        assert_eq!(
            (req.op, req.report, req.owner, req.slot),
            (HidOp::GetFeature, 1, OWNER, 0)
        );
        // The control URB stays pending: no early, emulated answer.
        assert!(silent(&mut kernel, 150));
        pad.relay_reply(OWNER, req.ticket, true, &[0x83, 3, 9, 8], None);
        let (_, seq, status, data) = kernel.reply_in();
        assert_eq!((seq, status, data.len()), (1, 0, 64));
        assert_eq!(&data[..5], &[1, 0x83, 3, 9, 8]);
        let stats = pad.relay_stats();
        assert_eq!((stats.requests, stats.replies, stats.timeouts), (1, 1, 0));
        assert!(stats.rtt_p50_us.is_some());
        pad.destroy();
    }

    #[test]
    fn a_silent_page_gets_the_emulated_answer_after_the_timeout() {
        let (pad, mut kernel, mut events) = pad();
        pad.shared
            .inner
            .lock()
            .unwrap()
            .relay
            .set_timeout(Duration::from_millis(60));
        pad.relay_open(OWNER);
        // Command 0x83 (get attributes): the emulation answers with its own.
        let mut set = vec![1u8, 0x83];
        set.resize(64, 0);
        kernel.send(&submit(1, DIR_OUT, 0, 64, SET_FEATURE_1), &set);
        let req = request(&mut events);
        assert_eq!(req.op, HidOp::SetFeature);
        // The emulated device acks once the deadline passes.
        let (_, seq, status) = kernel.reply();
        assert_eq!((seq, status), (1, 0));
        // And a read of the reply is the emulation's, the page still silent.
        kernel.send(&submit(2, DIR_IN, 0, 64, GET_FEATURE_1), &[]);
        let req = request(&mut events);
        let (_, seq, status, feature) = kernel.reply_in();
        assert_eq!((seq, status), (2, 0));
        assert_eq!(&feature[..2], &[1, 0x83], "the emulated attributes");
        // A reply after the deadline is dropped.
        pad.relay_reply(OWNER, req.ticket, true, &[0xee], None);
        assert!(silent(&mut kernel, 100));
        let stats = pad.relay_stats();
        assert_eq!((stats.timeouts, stats.replies), (2, 0));
        pad.destroy();
    }

    #[test]
    fn a_page_that_cannot_perform_a_request_gets_the_emulated_answer_at_once() {
        let (pad, mut kernel, mut events) = pad();
        pad.relay_open(OWNER);
        kernel.send(&submit(1, DIR_IN, 0, 64, GET_FEATURE_1), &[]);
        let req = request(&mut events);
        pad.relay_reply(OWNER, req.ticket, false, &[], Some("gone"));
        let (_, seq, status, feature) = kernel.reply_in();
        assert_eq!((seq, status, feature[0]), (1, 0, 1));
        assert_eq!(pad.relay_stats().errors, 1);
        pad.destroy();
    }

    #[test]
    fn a_relayed_feature_set_is_acked_by_the_page_and_not_emulated() {
        let (pad, mut kernel, mut events) = pad();
        pad.relay_open(OWNER);
        // A haptic pulse command: the emulation would make a Haptic event.
        let mut pulse = vec![1u8, 0x8f, 7, 1, 0x10, 0, 0x10, 0, 3, 0, 0];
        pulse.resize(64, 0);
        kernel.send(&submit(1, DIR_OUT, 0, 64, SET_FEATURE_1), &pulse);
        let req = request(&mut events);
        assert_eq!((req.op, req.report, req.len), (HidOp::SetFeature, 1, 63));
        assert_eq!(&req.data()[..4], &[0x8f, 7, 1, 0x10]);
        assert!(silent(&mut kernel, 100));
        pad.relay_reply(OWNER, req.ticket, true, &[], None);
        let (_, seq, status) = kernel.reply();
        assert_eq!((seq, status), (1, 0));
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(event, PadEvent::Haptic(_)), "{event:?}");
        }
        pad.destroy();
    }

    #[test]
    fn interrupt_out_is_relayed_not_emulated() {
        let (pad, mut kernel, mut events) = pad();
        pad.relay_open(OWNER);
        let mut rumble = vec![0x80u8, 0, 1, 0, 0xff, 0xff];
        rumble.resize(10, 0);
        kernel.send(&submit(1, DIR_OUT, 1, 10, [0; 8]), &rumble);
        // The transfer completes at once, whatever the page does.
        assert_eq!(kernel.reply().2, 0);
        let req = request(&mut events);
        assert_eq!(
            (req.op, req.ticket, req.report, req.len),
            (HidOp::Output, 0, 0x80, 9)
        );
        assert_eq!(&req.data()[..5], &[0, 1, 0, 0xff, 0xff]);
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(event, PadEvent::Rumble(_)), "{event:?}");
        }
        pad.destroy();
    }

    #[test]
    fn relayed_input_completes_urbs_and_the_emulation_stays_quiet() {
        let (pad, mut kernel, _events) = pad();
        pad.relay_open(OWNER);
        kernel.send(&submit(1, DIR_IN, 1, 64, [0; 8]), &[]);
        // The repeat thread would have answered within a few periods.
        assert!(silent(&mut kernel, 150));
        let mut state = vec![0x42u8];
        state.resize(54, 7);
        assert_eq!(pad.relay_input(OWNER, 1, &state), Verdict::Accept);
        let (_, seq, status, report) = kernel.reply_in();
        assert_eq!((seq, status), (1, 0));
        assert_eq!(report, state, "verbatim");
        // The wrong length, a lizard report, a stranger: nothing reaches the URB.
        kernel.send(&submit(2, DIR_IN, 1, 64, [0; 8]), &[]);
        let mut short = state.clone();
        short.pop();
        assert_eq!(pad.relay_input(OWNER, 2, &short), Verdict::Invalid);
        assert_eq!(
            pad.relay_input(OWNER, 3, &[0x40, 0, 0, 0, 0, 0]),
            Verdict::Denied
        );
        assert_eq!(pad.relay_input(9, 4, &state), Verdict::NotRelaying);
        assert!(silent(&mut kernel, 100));
        assert_eq!(pad.relay_input(OWNER, 5, &state), Verdict::Accept);
        assert_eq!(kernel.reply_in().1, 2);
        pad.destroy();
    }

    #[test]
    fn parsed_state_is_ignored_while_relaying_and_gone_ends_the_relay() {
        let (pad, mut kernel, _events) = pad();
        pad.relay_open(OWNER);
        kernel.send(&submit(1, DIR_IN, 1, 64, [0; 8]), &[]);
        let held = PadState {
            b: vec![1.0],
            ..PadState::default()
        };
        pad.update(&held);
        assert!(silent(&mut kernel, 150), "no parsed report reaches the URB");
        assert_eq!(pad.relay_owner(), Some(OWNER));
        // gone: the emulated device is back, at rest, and answers the URB.
        pad.update(&PadState {
            gone: true,
            ..PadState::default()
        });
        assert_eq!(pad.relay_owner(), None);
        let (_, seq, status, report) = kernel.reply_in();
        assert_eq!((seq, status), (1, 0));
        assert!(matches!(report[0], 0x42 | 0x43), "{report:02x?}");
        // Parsed state works again.
        pad.update(&held);
        pad.destroy();
    }

    #[test]
    fn closing_the_relay_answers_held_requests_with_the_emulation() {
        let (pad, mut kernel, mut events) = pad();
        pad.relay_open(OWNER);
        kernel.send(&submit(1, DIR_IN, 0, 64, GET_FEATURE_1), &[]);
        request(&mut events);
        // Another session cannot close it.
        pad.relay_close(Some(9), "test");
        assert!(silent(&mut kernel, 100));
        pad.relay_close(Some(OWNER), "test");
        let (_, seq, status, feature) = kernel.reply_in();
        assert_eq!((seq, status, feature[0]), (1, 0, 1));
        pad.destroy();
    }

    #[test]
    fn unlinking_a_held_request_forgets_it() {
        let (pad, mut kernel, mut events) = pad();
        pad.relay_open(OWNER);
        kernel.send(&submit(1, DIR_IN, 0, 64, GET_FEATURE_1), &[]);
        let req = request(&mut events);
        let mut wire = Vec::new();
        encode_cmd_unlink(&mut wire, 2, DEVID, 1);
        kernel.0.write_all(&wire).unwrap();
        let (cmd, seq, status) = kernel.reply();
        assert_eq!(
            (cmd, seq, status),
            (usbip::RET_UNLINK, 2, -usbip::ECONNRESET)
        );
        assert_eq!(pad.shared.inner.lock().unwrap().relay.in_flight(), 0);
        pad.relay_reply(OWNER, req.ticket, true, &[1], None);
        assert!(silent(&mut kernel, 100));
        pad.destroy();
    }

    #[test]
    fn a_reply_that_is_not_the_commands_is_asked_for_again_then_the_emulation_answers() {
        let (pad, mut kernel, mut events) = pad();
        pad.relay_open(OWNER);
        // Command 0x83 (get attributes), acked by the page.
        let mut set = vec![1u8, 0x83];
        set.resize(64, 0);
        kernel.send(&submit(1, DIR_OUT, 0, 64, SET_FEATURE_1), &set);
        let req = request(&mut events);
        pad.relay_reply(OWNER, req.ticket, true, &[], None);
        assert_eq!(kernel.reply().2, 0);
        // The get: the page gives zeros, three times (asked again each time).
        kernel.send(&submit(2, DIR_IN, 0, 64, GET_FEATURE_1), &[]);
        for _ in 0..3 {
            let req = request(&mut events);
            assert_eq!((req.op, req.report), (HidOp::GetFeature, 1));
            pad.relay_reply(OWNER, req.ticket, true, &[0; 63], None);
        }
        // The emulated device, which heard the command too, answers with the
        // attributes the command asks for.
        let (_, seq, status, feature) = kernel.reply_in();
        assert_eq!((seq, status), (2, 0));
        assert_eq!(&feature[..2], &[1, 0x83]);
        assert!(feature[2] > 0, "the attributes have a length");
        assert_eq!(pad.relay_stats().reply_mismatches, 3);
        assert_eq!(pad.relay_owner(), Some(OWNER));
        pad.destroy();
    }

    #[test]
    fn a_get_that_gets_the_right_reply_on_the_second_try_is_relayed() {
        let (pad, mut kernel, mut events) = pad();
        pad.relay_open(OWNER);
        let mut set = vec![1u8, 0x83];
        set.resize(64, 0);
        kernel.send(&submit(1, DIR_OUT, 0, 64, SET_FEATURE_1), &set);
        let req = request(&mut events);
        pad.relay_reply(OWNER, req.ticket, true, &[], None);
        assert_eq!(kernel.reply().2, 0);
        kernel.send(&submit(2, DIR_IN, 0, 64, GET_FEATURE_1), &[]);
        let req = request(&mut events);
        pad.relay_reply(OWNER, req.ticket, true, &[0; 63], None);
        let again = request(&mut events);
        assert_eq!(again.ticket, req.ticket);
        pad.relay_reply(OWNER, again.ticket, true, &[0x83, 5, 7, 7, 7], None);
        let (_, _, status, feature) = kernel.reply_in();
        assert_eq!(status, 0);
        assert_eq!(&feature[..5], &[1, 0x83, 5, 7, 7]);
        pad.destroy();
    }
}
