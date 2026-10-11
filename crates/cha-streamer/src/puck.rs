//! The 2026 Steam Controller's wireless puck (`28de:1304`) as a virtual USB
//! device, so a controller paired to the owner's real puck can be passed
//! through to Steam on the node the way the cable's is ([`crate::hid_relay`];
//! docs/controllers.md, "The wireless puck").
//!
//! The device has the real puck's layout, read from one (2026-10-10): seven
//! interfaces on one full-speed configuration.
//!
//! - **0 and 1**: a CDC pair on the real puck. Here **placeholders of vendor
//!   class** (`0xff`) with the real ones' endpoints, which no host driver
//!   binds. Real CDC class descriptors would let the host's `cdc_acm` bind
//!   them and make a `ttyACM` on the node (and ModemManager probe it), for
//!   nothing: Steam in the app never sees USB interfaces, only `hidraw`.
//! - **2 to 5**: HID, one per controller slot, with the same report
//!   descriptor as the cable's controller. SDL accepts the puck only on these.
//! - **6**: HID, vendor usage page `0xff00` usage 2, the puck's own. Never
//!   relayed: its feature requests stall, as the real one's do.
//!
//! A slot interface carries a controller while a session holds a relay for it
//! ([`UsbPuck::relay_open`]) and is empty otherwise. Empty, it refuses
//! (stalls) feature requests, as the real puck's empty slots do. Carrying one,
//! it is the cable's relay on that interface: the controller's input reports
//! complete the interface's interrupt IN URBs verbatim, Steam's feature and
//! output reports go to the page, and the control URBs wait for its reply.
//! The one difference is the fallback when the page is slow or fails: with
//! the cable the emulated wired controller answers; here the same emulation
//! answers the slot's requests and nothing else exists, so a relay that fails
//! ends and the slot reports its controller gone.
//!
//! **Wireless status.** A dongle announces a controller with input report
//! `0x79`, one byte: 2 connected, 1 disconnected (SDL's triton driver, and the
//! page's `steam-triton.ts` ported from it, read it so; the real puck's own
//! bytes are what the page relays, nothing else is known of them). The puck
//! sends `[0x79, 2]` on a slot when its relay opens and `[0x79, 1]` when it
//! ends, ahead of and after the controller's own reports.

use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::gamepad::PadEvent;
use crate::hid_relay::{
    Answer, InFlight, Relay, RelayStats, Request, STEAM_LIZARD_REPORTS, Verdict,
};
use crate::steam_controller::DESCRIPTOR_TRITON_USB;
use crate::uhid::{Found, Protocol};
use crate::usbdev::{EndpointSpec, Engine, Hid, Identity, InterfaceSpec, REPORT_FEATURE, Reply};
use crate::usbip::{self, HEADER_LEN};
use crate::vhci::{self, Serve, Shared};

pub const VENDOR: u16 = 0x28de;
pub const PRODUCT: u16 = 0x1304;
/// `bcdDevice` of the real puck.
const BCD_DEVICE: u16 = 0x0002;
/// Controllers a puck pairs.
pub const SLOTS: usize = 4;
/// The interface number of the first slot; slot `k` is interface `FIRST_SLOT + k`.
pub const FIRST_SLOT: usize = 2;
/// The puck's own HID interface.
pub const MONITOR: usize = 6;
const INTERFACES: usize = MONITOR + 1;

/// The input report that tells an app a controller came or went behind the puck.
pub const WIRELESS_STATUS: u8 = 0x79;
pub const WIRELESS_DISCONNECTED: u8 = 1;
pub const WIRELESS_CONNECTED: u8 = 2;

/// Interface 6's report descriptor, read from the real puck: input `0x42`
/// (53 bytes) and `0x79` (1), features 1 and 2 (63 each).
#[rustfmt::skip]
pub const MONITOR_DESCRIPTOR: &[u8] = &[
    0x06, 0x00, 0xff, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x42, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75,
    0x08, 0x95, 0x35, 0x09, 0x42, 0x81, 0x02, 0x85, 0x79, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75,
    0x08, 0x95, 0x01, 0x09, 0x79, 0x81, 0x02, 0x85, 0x01, 0x95, 0x3f, 0x09, 0x01, 0xb1, 0x02,
    0x85, 0x02, 0x95, 0x3f, 0x09, 0x01, 0xb1, 0x02, 0xc0,
];

/// The report descriptor of slot interfaces: the cable controller's.
pub const SLOT_DESCRIPTOR: &[u8] = DESCRIPTOR_TRITON_USB;

/// The interfaces, by number. Endpoint addresses are ours (the I/O Registry
/// doesn't show them): the CDC pair's notification endpoint and bulk pair
/// first, then an interrupt IN and OUT pair per HID interface.
fn interfaces() -> Vec<InterfaceSpec> {
    let mut list = vec![
        InterfaceSpec::placeholder(vec![EndpointSpec::interrupt(0x81, 16, 16)]),
        InterfaceSpec::placeholder(vec![
            EndpointSpec::bulk(0x82, 64),
            EndpointSpec::bulk(0x02, 64),
        ]),
    ];
    for k in 0..SLOTS as u8 {
        list.push(InterfaceSpec::hid(SLOT_DESCRIPTOR, 0x83 + k, 0x03 + k));
    }
    list.push(InterfaceSpec::hid(MONITOR_DESCRIPTOR, 0x87, 0x07));
    list
}

/// The puck as USB describes it. `serial` is ours: a real puck's is not known.
pub fn identity(serial: String) -> Identity {
    Identity {
        vendor: VENDOR,
        product: PRODUCT,
        bcd_device: BCD_DEVICE,
        manufacturer: "Valve Software".to_string(),
        product_name: "Steam Controller Puck".to_string(),
        serial,
        report_descriptor: SLOT_DESCRIPTOR,
        interfaces: interfaces(),
    }
}

/// A puck's serial for a number of the run's.
pub fn serial_for(hash: u64) -> String {
    format!("PXA{:010X}", hash & 0xff_ffff_ffff)
}

/// What answers a relayed slot's requests when the page can't, made for the
/// page slot the controller is in.
pub type Emulation = Box<dyn Fn(usize) -> Box<dyn Protocol> + Send>;

/// One controller slot: an interface and what it carries.
struct SlotIf {
    relay: Relay,
    /// The emulated controller behind the slot while a relay holds it.
    protocol: Option<Box<dyn Protocol>>,
    /// The page's pad slot of the controller it carries, or last carried (a
    /// controller that comes back gets the interface it had).
    page_slot: Option<usize>,
    /// What the app was last told: a controller is there.
    connected: bool,
}

struct Inner {
    engine: Engine,
    slots: Vec<SlotIf>,
    sock: UnixStream,
    events: broadcast::Sender<PadEvent>,
    emulation: Emulation,
}

/// What the device model asks of the slots.
struct Function<'a> {
    slots: &'a mut [SlotIf],
    events: &'a broadcast::Sender<PadEvent>,
    now: Instant,
}

impl Function<'_> {
    /// The slot behind interface `iface`: none for the puck's own interface.
    fn slot(&mut self, iface: usize) -> Option<&mut SlotIf> {
        iface
            .checked_sub(FIRST_SLOT)
            .and_then(|k| self.slots.get_mut(k))
    }
}

impl Hid for Function<'_> {
    fn get_report(&mut self, iface: usize, kind: u8, id: u8, _w_length: u16) -> Reply<Vec<u8>> {
        let (events, now) = (self.events.clone(), self.now);
        // An empty slot, and the puck's own interface, refuse.
        let Some(slot) = self.slot(iface).filter(|s| s.relay.is_active()) else {
            return Reply::Done(Err(libc::EPIPE));
        };
        if let Some((ticket, request)) = slot.relay.request_get(kind, id, now) {
            if events.send(PadEvent::Hid(request)).is_ok() {
                return Reply::Pending(ticket);
            }
            slot.relay.cancel(ticket);
        }
        Reply::Done(match slot.protocol.as_deref_mut() {
            Some(protocol) => vhci::emulated_get(protocol, kind, id),
            None => Err(libc::EPIPE),
        })
    }

    fn set_report(&mut self, iface: usize, kind: u8, id: u8, data: &[u8]) -> Reply<()> {
        let (events, now) = (self.events.clone(), self.now);
        let Some(slot) = self.slot(iface).filter(|s| s.relay.is_active()) else {
            return Reply::Done(Err(libc::EPIPE));
        };
        if let Some((ticket, request)) = slot.relay.request_set(kind, id, data, now) {
            if events.send(PadEvent::Hid(request)).is_ok() {
                // The emulated controller hears it too (its events dropped: the
                // real one does the work), so that if the page fails to answer,
                // the emulation's reply to the following get is this command's.
                if kind == REPORT_FEATURE
                    && let Some(protocol) = slot.protocol.as_deref_mut()
                {
                    let _ = protocol.set_report(id, crate::uhid::FEATURE_REPORT, data);
                }
                return Reply::Pending(ticket);
            }
            slot.relay.cancel(ticket);
        }
        match slot.protocol.as_deref_mut() {
            Some(protocol) => Reply::Done(vhci::emulated_set(protocol, &events, kind, id, data)),
            None => Reply::Done(Err(libc::EPIPE)),
        }
    }

    fn output(&mut self, iface: usize, data: &[u8]) {
        let (events, now) = (self.events.clone(), self.now);
        // The real controller owns its motors; what it doesn't get is dropped.
        let Some(slot) = self.slot(iface).filter(|s| s.relay.is_active()) else {
            return;
        };
        if let Some(request) = slot.relay.request_output(data, now) {
            let _ = events.send(PadEvent::Hid(request));
        }
    }
}

impl Inner {
    fn write(&mut self, out: &[u8]) {
        if !out.is_empty()
            && let Err(err) = self.sock.write_all(out)
        {
            debug!("usbip write: {err}");
        }
    }

    /// Tells the app slot `k` has (or has lost) a controller, with the
    /// wireless status report a dongle sends.
    fn set_wireless(&mut self, k: usize, connected: bool) {
        if self.slots[k].connected == connected {
            return;
        }
        self.slots[k].connected = connected;
        let status = if connected {
            WIRELESS_CONNECTED
        } else {
            WIRELESS_DISCONNECTED
        };
        let mut out = Vec::with_capacity(HEADER_LEN + 2);
        self.engine
            .push_report_on(FIRST_SLOT + k, &[WIRELESS_STATUS, status], &mut out);
        self.write(&out);
    }

    /// Completes held control URBs of slot `k`: with the page's answer, or the
    /// emulated controller's (a stall if there is none).
    fn answer(&mut self, k: usize, answers: Vec<(InFlight, Answer)>) {
        let mut out = Vec::new();
        for (held, answer) in answers {
            let result = match answer {
                Answer::Relayed(data) => Ok(data),
                Answer::Emulate => match self.slots[k].protocol.as_deref_mut() {
                    None => Err(libc::EPIPE),
                    Some(protocol) => match &held.req {
                        Request::Get { kind, id } => vhci::emulated_get(protocol, *kind, *id),
                        Request::Set { kind, id, data } => {
                            vhci::emulated_set(protocol, &self.events, *kind, *id, data)
                                .map(|()| Vec::new())
                        }
                    },
                },
            };
            self.engine
                .complete_control(FIRST_SLOT + k, held.ticket, result, &mut out);
        }
        self.write(&out);
    }

    /// After anything that may have ended slot `k`'s relay (`was` active
    /// before): the controller is gone, as far as the app is concerned.
    fn settle(&mut self, k: usize, was: bool) {
        if was && !self.slots[k].relay.is_active() {
            self.engine.clear_pending_on(FIRST_SLOT + k);
            self.slots[k].protocol = None;
            self.set_wireless(k, false);
        }
    }

    fn close(&mut self, k: usize, owner: Option<u64>, why: &str) {
        let was = self.slots[k].relay.is_active();
        let left = self.slots[k].relay.close(owner, why);
        self.answer(k, left.into_iter().map(|f| (f, Answer::Emulate)).collect());
        self.settle(k, was);
    }

    /// The slot for the controller in page slot `page`: the one it had, else
    /// a free one.
    fn allocate(&self, page: usize) -> Option<usize> {
        let mine = |s: &SlotIf| s.page_slot == Some(page);
        self.slots
            .iter()
            .position(mine)
            .or_else(|| {
                self.slots
                    .iter()
                    .position(|s| !s.relay.is_active() && s.page_slot.is_none())
            })
            .or_else(|| self.slots.iter().position(|s| !s.relay.is_active()))
    }

    /// The slot that carries page slot `page` now.
    fn carrying(&self, page: usize) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.page_slot == Some(page) && s.relay.is_active())
    }

    /// Answers the requests whose time is up and asks again where a reply was
    /// not the command's.
    fn expire(&mut self, now: Instant) {
        for k in 0..self.slots.len() {
            if !self.slots[k].relay.is_active() {
                continue;
            }
            let was = self.slots[k].relay.is_active();
            let due = self.slots[k].relay.expire(now);
            for request in due.resend {
                let _ = self.events.send(PadEvent::Hid(request));
            }
            if !due.answers.is_empty() {
                self.answer(k, due.answers);
            }
            self.settle(k, was);
        }
    }
}

impl Serve for Inner {
    fn serve(&mut self, cmd: &usbip::Command, body: &[u8]) {
        let mut out = Vec::new();
        let mut function = Function {
            slots: &mut self.slots,
            events: &self.events,
            now: Instant::now(),
        };
        self.engine.on_command(cmd, body, &mut out, &mut function);
        for (iface, ticket) in self.engine.take_cancelled() {
            if let Some(slot) = iface
                .checked_sub(FIRST_SLOT)
                .and_then(|k| self.slots.get_mut(k))
            {
                slot.relay.cancel(ticket);
            }
        }
        self.write(&out);
    }
}

/// The puck: a USB device on a `vhci_hcd` port, served from two threads, one
/// reading the kernel's URBs and one watching the relays' deadlines.
pub struct UsbPuck {
    shared: Arc<Shared<Inner>>,
    /// The vhci port; `None` for a device served on some other socket (tests).
    port: Option<u32>,
    sock: UnixStream,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl UsbPuck {
    /// Attaches the puck to a free port of `vhci_hcd` and serves it. The
    /// kernel's drivers need a moment to bind: [`UsbPuck::wait_for_nodes`].
    pub fn attach(
        identity: Identity,
        events: broadcast::Sender<PadEvent>,
        emulation: Emulation,
    ) -> Result<Arc<Self>> {
        let (ours, theirs) = UnixStream::pair().context("making the socketpair")?;
        let port = vhci::attach(&theirs)?;
        // The kernel holds its own reference now.
        drop(theirs);
        match Self::serve(ours, Some(port), identity, events, emulation) {
            Ok(puck) => Ok(puck),
            Err(err) => {
                let _ = vhci::detach(port);
                Err(err)
            }
        }
    }

    fn serve(
        sock: UnixStream,
        port: Option<u32>,
        identity: Identity,
        events: broadcast::Sender<PadEvent>,
        emulation: Emulation,
    ) -> Result<Arc<Self>> {
        let reader = sock.try_clone()?;
        let now = Instant::now();
        let slots = (0..SLOTS)
            .map(|k| SlotIf {
                relay: Relay::new(FIRST_SLOT + k, SLOT_DESCRIPTOR, STEAM_LIZARD_REPORTS, now),
                protocol: None,
                page_slot: None,
                connected: false,
            })
            .collect();
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                engine: Engine::new(identity),
                slots,
                sock: sock.try_clone()?,
                events,
                emulation,
            }),
            stop: AtomicBool::new(false),
        });
        let rx = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("puck-usbip".to_string())
                .spawn(move || vhci::read_urbs(reader, &shared))
                .context("spawning the usbip thread")?
        };
        let watch = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("puck-watch".to_string())
                .spawn(move || watch_deadlines(&shared))
                .context("spawning the deadline thread")?
        };
        Ok(Arc::new(Self {
            shared,
            port,
            sock,
            threads: Mutex::new(vec![rx, watch]),
        }))
    }

    /// Waits for the kernel to enumerate the puck and for each HID interface
    /// (2 to 6) to bind `driver` and make its `hidraw` node. The nodes are in
    /// interface order.
    pub fn wait_for_nodes(&self, driver: &str, within: Duration) -> Result<Vec<Found>> {
        let port = self.port.context("not attached to vhci_hcd")?;
        (FIRST_SLOT..INTERFACES)
            .map(|iface| vhci::wait_for_nodes(port, iface, driver, within))
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.shared.inner.lock().expect("usb puck lock")
    }

    /// Whether a relay carries the controller of page slot `page` now.
    pub fn carries(&self, page: usize) -> bool {
        self.lock().carrying(page).is_some()
    }

    /// `owner`'s session passes its controller in page slot `page` through
    /// the puck from now on, on a slot interface of its own (the one it had
    /// before if that is free). Returns the interface's number.
    pub fn relay_open(&self, owner: u64, page: usize) -> Result<usize, &'static str> {
        let mut inner = self.lock();
        let k = inner
            .allocate(page)
            .ok_or("every slot of the puck is in use")?;
        let protocol = (inner.emulation)(page);
        let slot = &mut inner.slots[k];
        slot.page_slot = Some(page);
        slot.relay.set_slot(page);
        slot.protocol = Some(protocol);
        let left = slot.relay.open(owner, Instant::now());
        inner.answer(k, left.into_iter().map(|f| (f, Answer::Emulate)).collect());
        // Whatever was queued for the interface is not this controller's.
        inner.engine.clear_pending_on(FIRST_SLOT + k);
        inner.set_wireless(k, true);
        info!(
            slot = page,
            interface = FIRST_SLOT + k,
            owner,
            "puck: a controller joined"
        );
        Ok(FIRST_SLOT + k)
    }

    /// Ends the relay of page slot `page` (`owner`'s only, if given): the
    /// controller is gone as far as the app can tell.
    pub fn relay_close(&self, page: usize, owner: Option<u64>, why: &str) {
        let mut inner = self.lock();
        if let Some(k) = inner.carrying(page) {
            inner.close(k, owner, why);
        }
    }

    /// Ends every relay `owner` holds.
    pub fn relay_close_owned_by(&self, owner: u64, why: &str) {
        let mut inner = self.lock();
        for k in 0..SLOTS {
            inner.close(k, Some(owner), why);
        }
    }

    /// Ends every relay.
    pub fn relay_close_all(&self, why: &str) {
        let mut inner = self.lock();
        for k in 0..SLOTS {
            inner.close(k, None, why);
        }
    }

    /// A report of the real controller in page slot `page` (id first),
    /// numbered `q`: if it passes, it completes the oldest parked interrupt-IN
    /// URB of the slot's interface.
    pub fn relay_input(&self, page: usize, owner: u64, q: u32, report: &[u8]) -> Verdict {
        let mut inner = self.lock();
        let Some(k) = inner.carrying(page) else {
            return Verdict::NotRelaying;
        };
        let verdict = inner.slots[k].relay.input(owner, q, report, Instant::now());
        if verdict == Verdict::Accept {
            let mut out = Vec::with_capacity(HEADER_LEN + report.len());
            inner
                .engine
                .push_report_on(FIRST_SLOT + k, report, &mut out);
            inner.write(&out);
        }
        verdict
    }

    /// The page's answer to the request under `ticket` of page slot `page`.
    pub fn relay_reply(
        &self,
        page: usize,
        owner: u64,
        ticket: u32,
        ok: bool,
        data: &[u8],
        error: Option<&str>,
    ) {
        let mut inner = self.lock();
        let Some(k) = inner.carrying(page) else {
            return;
        };
        let was = inner.slots[k].relay.is_active();
        let answers = inner.slots[k]
            .relay
            .reply(owner, ticket, ok, data, error, Instant::now());
        inner.answer(k, answers);
        inner.settle(k, was);
    }

    pub fn relay_stats(&self) -> RelayStats {
        let inner = self.lock();
        let mut total = RelayStats::default();
        for slot in &inner.slots {
            total.merge(&slot.relay.stats());
        }
        total
    }

    /// The interface page slot `page` is on, while a relay carries it.
    #[cfg(test)]
    pub fn interface_of(&self, page: usize) -> Option<usize> {
        self.lock().carrying(page).map(|k| FIRST_SLOT + k)
    }

    /// Takes the device away: detaches the port (the kernel disconnects the
    /// device and removes its nodes) and ends the threads. Safe to repeat.
    pub fn destroy(&self) {
        if self.shared.stop.swap(true, Ordering::Relaxed) {
            return;
        }
        if let Some(port) = self.port
            && let Err(err) = vhci::detach(port)
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

impl Drop for UsbPuck {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Answers the requests whose time is up, and asks again for replies that were
/// not the command's. Nothing else happens on its own: the puck sends no
/// reports but the controllers' and its wireless status.
fn watch_deadlines(shared: &Shared<Inner>) {
    const IDLE: Duration = Duration::from_millis(50);
    while !shared.stop.load(Ordering::Relaxed) {
        let wait = {
            let mut inner = shared.inner.lock().expect("usb puck lock");
            let now = Instant::now();
            inner.expire(now);
            inner
                .slots
                .iter()
                .filter_map(|s| s.relay.next_deadline())
                .map(|deadline| deadline.saturating_duration_since(now))
                .fold(IDLE, Duration::min)
                .max(Duration::from_millis(1))
        };
        thread::sleep(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamepad::{HidOp, HidRequest};
    use crate::steam_controller::{SteamController, Variant};
    use crate::usbdev::REPORT_OUTPUT;
    use crate::usbip::{
        DIR_IN, DIR_OUT, ECONNRESET, EPIPE, NO_ISO, Submit, encode_cmd_submit, encode_cmd_unlink,
    };
    use std::io::Read;

    const OWNER: u64 = 5;
    const DEVID: u32 = (1 << 16) | 2;

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

        /// Whether the kernel hears anything within `ms`.
        fn silent(&mut self, ms: u64) -> bool {
            self.0
                .set_read_timeout(Some(Duration::from_millis(ms)))
                .unwrap();
            let mut head = [0u8; 1];
            let silent = self.0.read_exact(&mut head).is_err();
            self.0
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            silent
        }

        /// A GET_REPORT(feature 1) on interface `iface`.
        fn get_feature(&mut self, seq: u32, iface: u8) {
            self.send(
                &submit(seq, DIR_IN, 0, 64, [0xa1, 1, 1, 3, iface, 0, 64, 0]),
                &[],
            );
        }

        /// A SET_REPORT(feature 1) of `body` on interface `iface`.
        fn set_feature(&mut self, seq: u32, iface: u8, body: &[u8]) {
            self.send(
                &submit(seq, DIR_OUT, 0, 64, [0x21, 9, 1, 3, iface, 0, 64, 0]),
                body,
            );
        }

        /// A parked interrupt IN URB of slot interface `iface` (endpoint `iface + 1`).
        fn park(&mut self, seq: u32, iface: u32) {
            self.send(&submit(seq, DIR_IN, iface + 1, 64, [0; 8]), &[]);
        }
    }

    fn emulation() -> Emulation {
        Box::new(|page| {
            Box::new(SteamController::new(
                Variant::TritonUsb,
                page,
                "FXA0123456789",
            ))
        })
    }

    fn puck() -> (Arc<UsbPuck>, Kernel, broadcast::Receiver<PadEvent>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (events, rx) = broadcast::channel(64);
        let puck = UsbPuck::serve(
            ours,
            None,
            identity("PXA0123456789".into()),
            events,
            emulation(),
        )
        .unwrap();
        (puck, Kernel(theirs), rx)
    }

    /// The next relay request the puck asked the sessions for.
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

    fn command(cmd: u8) -> Vec<u8> {
        let mut v = vec![1u8, cmd];
        v.resize(64, 0);
        v
    }

    fn state(fill: u8) -> Vec<u8> {
        let mut v = vec![0x42u8];
        v.resize(54, fill);
        v
    }

    #[test]
    fn the_device_is_a_puck_of_seven_interfaces() {
        let id = identity("PXA0123456789".into());
        let dev = id.device_descriptor();
        // 28de:1304, bcdDevice 2, bcdUSB 2.0.1, class 0 (per interface), endpoint 0 of 64.
        assert_eq!(u16::from_le_bytes([dev[8], dev[9]]), 0x28de);
        assert_eq!(u16::from_le_bytes([dev[10], dev[11]]), 0x1304);
        assert_eq!(u16::from_le_bytes([dev[12], dev[13]]), 0x0002);
        assert_eq!(
            (u16::from_le_bytes([dev[2], dev[3]]), dev[4], dev[7]),
            (0x0201, 0, 64)
        );

        let cfg = id.configuration_descriptor();
        assert_eq!(usize::from(u16::from_le_bytes([cfg[2], cfg[3]])), cfg.len());
        assert_eq!(cfg[4], 7, "bNumInterfaces");
        // Walk it: (number, class, endpoints) per interface, every endpoint's address.
        let (mut at, mut interfaces, mut endpoints, mut hid_lengths) =
            (0, Vec::new(), Vec::new(), Vec::new());
        while at < cfg.len() {
            let d = &cfg[at..at + usize::from(cfg[at])];
            match d[1] {
                4 => interfaces.push((d[2], d[5], d[4])),
                5 => endpoints.push((d[2], d[3])),
                0x21 => hid_lengths.push(usize::from(u16::from_le_bytes([d[7], d[8]]))),
                _ => {}
            }
            at += d.len();
        }
        assert_eq!(at, cfg.len());
        assert_eq!(
            interfaces,
            [
                (0, 0xff, 1),
                (1, 0xff, 2),
                (2, 3, 2),
                (3, 3, 2),
                (4, 3, 2),
                (5, 3, 2),
                (6, 3, 2)
            ]
        );
        // The slots' pairs are 0x83/0x03 to 0x86/0x06, the puck's own 0x87/0x07.
        assert_eq!(
            endpoints,
            [
                (0x81, 3),
                (0x82, 2),
                (0x02, 2),
                (0x83, 3),
                (0x03, 3),
                (0x84, 3),
                (0x04, 3),
                (0x85, 3),
                (0x05, 3),
                (0x86, 3),
                (0x06, 3),
                (0x87, 3),
                (0x07, 3)
            ]
        );
        let slot = SLOT_DESCRIPTOR.len();
        assert_eq!(
            hid_lengths,
            [slot, slot, slot, slot, MONITOR_DESCRIPTOR.len()]
        );
        assert_eq!(MONITOR_DESCRIPTOR.len(), 54);
        // Endpoint numbers are unique per direction.
        let mut all: Vec<u8> = endpoints.iter().map(|e| e.0).collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), endpoints.len());
    }

    #[test]
    fn the_monitor_descriptor_is_the_real_ones() {
        // As read from the real puck (interface 6), byte for byte.
        let hex: String = MONITOR_DESCRIPTOR
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            hex,
            "0600ff0902a1018542150026ff0075089535094281028579150026ff0075089501097981028501953f0901b1028502953f0901b102c0"
        );
        let sizes = crate::hid_relay::ReportSizes::parse(MONITOR_DESCRIPTOR);
        assert_eq!((sizes.input(0x42), sizes.input(0x79)), (Some(53), Some(1)));
        assert_eq!((sizes.feature(1), sizes.feature(2)), (Some(63), Some(63)));
        // The slot interfaces carry the cable's descriptor.
        let slot = crate::hid_relay::ReportSizes::parse(SLOT_DESCRIPTOR);
        assert_eq!((slot.input(0x42), slot.input(0x79)), (Some(53), Some(1)));
    }

    #[test]
    fn the_kernel_reads_each_interfaces_own_descriptor() {
        let (puck, mut kernel, _events) = puck();
        for (seq, iface, len) in [
            (1, 2u8, SLOT_DESCRIPTOR.len()),
            (2, 5, SLOT_DESCRIPTOR.len()),
            (3, 6, MONITOR_DESCRIPTOR.len()),
        ] {
            kernel.send(
                &submit(seq, DIR_IN, 0, 4096, [0x81, 6, 0, 0x22, iface, 0, 0, 0x10]),
                &[],
            );
            let (_, s, status, rd) = kernel.reply_in();
            assert_eq!((s, status, rd.len()), (seq, 0, len), "interface {iface}");
        }
        // Interface 0 is a placeholder with no report descriptor.
        kernel.send(
            &submit(4, DIR_IN, 0, 4096, [0x81, 6, 0, 0x22, 0, 0, 0, 0x10]),
            &[],
        );
        assert_eq!(kernel.reply().2, -EPIPE);
        puck.destroy();
    }

    #[test]
    fn empty_slots_and_the_monitor_refuse_feature_requests() {
        let (puck, mut kernel, mut events) = puck();
        for iface in [2u8, 3, 4, 5, 6] {
            kernel.set_feature(10 + u32::from(iface), iface, &command(0x83));
            assert_eq!(kernel.reply().2, -EPIPE, "set on {iface}");
            kernel.get_feature(20 + u32::from(iface), iface);
            assert_eq!(kernel.reply().2, -EPIPE, "get on {iface}");
        }
        // Nothing was asked of any page, and an output goes nowhere.
        kernel.send(
            &submit(40, DIR_OUT, 3, 10, [0; 8]),
            &[0x80, 0, 1, 0, 0xff, 0, 0, 0, 0, 0],
        );
        assert_eq!(kernel.reply().2, 0);
        assert!(events.try_recv().is_err());
        // The placeholders refuse everything, including class requests.
        kernel.get_feature(50, 0);
        assert_eq!(kernel.reply().2, -EPIPE);
        puck.destroy();
    }

    #[test]
    fn a_slot_says_connected_when_its_relay_opens_and_disconnected_when_it_ends() {
        let (puck, mut kernel, _events) = puck();
        // Steam's URB on slot interface 3 waits.
        kernel.park(1, 3);
        assert!(kernel.silent(80));
        assert_eq!(puck.relay_open(OWNER, 1), Ok(2));
        // Page slot 1 took the first interface; the connect status is what the URB gets.
        assert_eq!(puck.interface_of(1), Some(2));
        kernel.park(2, 2);
        let (_, seq, status, report) = kernel.reply_in();
        assert_eq!((seq, status), (2, 0));
        assert_eq!(report, [WIRELESS_STATUS, WIRELESS_CONNECTED]);
        // Interface 3 (empty) heard nothing.
        assert!(kernel.silent(80));
        kernel.park(3, 2);
        puck.relay_close(1, Some(OWNER), "test");
        let (_, seq, _, report) = kernel.reply_in();
        assert_eq!(seq, 3);
        assert_eq!(report, [WIRELESS_STATUS, WIRELESS_DISCONNECTED]);
        assert!(!puck.carries(1));
        // Ending it twice says nothing more.
        puck.relay_close(1, Some(OWNER), "again");
        assert!(kernel.silent(80));
        puck.destroy();
    }

    #[test]
    fn relayed_input_lands_on_its_own_interface_verbatim() {
        let (puck, mut kernel, _events) = puck();
        assert_eq!(puck.relay_open(OWNER, 0), Ok(2));
        assert_eq!(puck.relay_open(OWNER, 2), Ok(3));
        // Each interface's connect status first.
        kernel.park(1, 2);
        assert_eq!(kernel.reply_in().3, [WIRELESS_STATUS, WIRELESS_CONNECTED]);
        kernel.park(2, 3);
        assert_eq!(kernel.reply_in().3, [WIRELESS_STATUS, WIRELESS_CONNECTED]);
        // A state of page slot 2 goes to interface 3 and no other.
        kernel.park(3, 2);
        kernel.park(4, 3);
        assert_eq!(puck.relay_input(2, OWNER, 1, &state(7)), Verdict::Accept);
        let (_, seq, status, report) = kernel.reply_in();
        assert_eq!((seq, status), (4, 0));
        assert_eq!(report, state(7));
        assert!(kernel.silent(80), "interface 2 is not told");
        assert_eq!(puck.relay_input(0, OWNER, 1, &state(9)), Verdict::Accept);
        assert_eq!(kernel.reply_in().1, 3);
        // The page's own wireless status passes as any report; lizard reports,
        // wrong sizes, a stranger and a slot that carries nothing do not.
        assert_eq!(puck.relay_input(0, OWNER, 2, &[0x79, 2]), Verdict::Accept);
        assert_eq!(
            puck.relay_input(0, OWNER, 3, &[0x40, 0, 0, 0, 0, 0]),
            Verdict::Denied
        );
        assert_eq!(puck.relay_input(0, OWNER, 4, &[0x42, 1]), Verdict::Invalid);
        assert_eq!(puck.relay_input(0, 99, 5, &state(1)), Verdict::NotRelaying);
        assert_eq!(
            puck.relay_input(3, OWNER, 1, &state(1)),
            Verdict::NotRelaying
        );
        puck.destroy();
    }

    #[test]
    fn a_controller_gets_the_interface_it_had_and_slots_run_out_at_four() {
        let (puck, _kernel, _events) = puck();
        assert_eq!(puck.relay_open(OWNER, 2), Ok(2));
        assert_eq!(puck.relay_open(OWNER, 0), Ok(3));
        puck.relay_close(2, None, "left");
        assert_eq!(puck.interface_of(2), None);
        // A new controller takes a never-used interface first.
        assert_eq!(puck.relay_open(OWNER, 3), Ok(4));
        // The first one comes back to its own.
        assert_eq!(puck.relay_open(OWNER, 2), Ok(2));
        assert_eq!(puck.relay_open(OWNER, 1), Ok(5));
        // All four busy: page slot 1 is on 5, and a fifth has no room.
        assert_eq!(
            puck.relay_open(OWNER, 7),
            Err("every slot of the puck is in use")
        );
        // Opening a slot already open is its interface again, not a second one.
        assert_eq!(puck.relay_open(OWNER, 1), Ok(5));
        puck.destroy();
    }

    #[test]
    fn a_relayed_feature_request_goes_to_the_page_under_its_page_slot() {
        let (puck, mut kernel, mut events) = puck();
        puck.relay_open(OWNER, 2).unwrap();
        // Steam writes a command on interface 2 and reads the reply.
        kernel.set_feature(1, 2, &command(0x83));
        let req = request(&mut events);
        assert_eq!(
            (req.op, req.report, req.slot, req.owner),
            (HidOp::SetFeature, 1, 2, OWNER)
        );
        assert!(kernel.silent(100), "the control URB waits for the page");
        puck.relay_reply(2, OWNER, req.ticket, true, &[], None);
        assert_eq!(kernel.reply().2, 0);
        kernel.get_feature(2, 2);
        let req = request(&mut events);
        assert_eq!((req.op, req.slot), (HidOp::GetFeature, 2));
        puck.relay_reply(2, OWNER, req.ticket, true, &[0x83, 4, 9, 9, 9, 9], None);
        let (_, seq, status, data) = kernel.reply_in();
        assert_eq!((seq, status, data.len()), (2, 0, 64));
        assert_eq!(&data[..6], &[1, 0x83, 4, 9, 9, 9]);
        let stats = puck.relay_stats();
        assert_eq!((stats.requests, stats.replies), (2, 2));
        puck.destroy();
    }

    #[test]
    fn two_slots_may_hold_the_same_ticket_number() {
        let (puck, mut kernel, mut events) = puck();
        puck.relay_open(OWNER, 0).unwrap();
        puck.relay_open(OWNER, 1).unwrap();
        kernel.get_feature(1, 2);
        kernel.get_feature(2, 3);
        let (a, b) = (request(&mut events), request(&mut events));
        // Each relay counts its tickets from 1.
        assert_eq!(a.ticket, b.ticket);
        assert_eq!((a.slot, b.slot), (0, 1));
        puck.relay_reply(1, OWNER, b.ticket, true, &[0xb4, 0, 2], None);
        let (_, seq, _, data) = kernel.reply_in();
        assert_eq!((seq, &data[..2]), (2, &[1u8, 0xb4][..]));
        assert!(kernel.silent(80), "interface 2's is still waiting");
        puck.relay_reply(0, OWNER, a.ticket, true, &[0x82, 0], None);
        assert_eq!(kernel.reply_in().1, 1);
        puck.destroy();
    }

    #[test]
    fn an_interrupt_out_is_relayed_for_its_slot_only() {
        let (puck, mut kernel, mut events) = puck();
        puck.relay_open(OWNER, 3).unwrap();
        let mut rumble = vec![0x80u8, 0, 1, 0, 0xff, 0xff];
        rumble.resize(10, 0);
        // Endpoint 3 is interface 2's (carried), endpoint 4 interface 3's (empty).
        kernel.send(&submit(1, DIR_OUT, 4, 10, [0; 8]), &rumble);
        assert_eq!(kernel.reply().2, 0);
        assert!(events.try_recv().is_err(), "an empty slot drops it");
        kernel.send(&submit(2, DIR_OUT, 3, 10, [0; 8]), &rumble);
        assert_eq!(kernel.reply().2, 0);
        let req = request(&mut events);
        assert_eq!((req.op, req.report, req.slot), (HidOp::Output, 0x80, 3));
        puck.destroy();
    }

    #[test]
    fn firmware_commands_never_reach_a_real_controller_through_the_puck() {
        let (puck, mut kernel, mut events) = puck();
        puck.relay_open(OWNER, 0).unwrap();
        // 0x90 (suspected reboot), 0xb7 (audio update, flash), 0xa9 (serial number):
        // the emulation refuses them, and no request is made of the page.
        for (seq, cmd) in [(1, 0x90), (2, 0xb7), (3, 0xa9)] {
            kernel.set_feature(seq, 2, &command(cmd));
            assert_eq!(kernel.reply().2, -EPIPE, "command {cmd:#x}");
        }
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(event, PadEvent::Hid(_)), "{event:?}");
        }
        assert_eq!(puck.relay_stats().blocked, 3);
        // An output report outside 0x80..=0x85 is dropped as well.
        kernel.send(&submit(9, DIR_OUT, 3, 64, [0; 8]), &{
            let mut v = vec![0x89u8];
            v.resize(64, 0);
            v
        });
        assert_eq!(kernel.reply().2, 0);
        assert!(events.try_recv().is_err());
        // And an allowed one does go.
        kernel.set_feature(10, 2, &command(0x87));
        assert_eq!(request(&mut events).op, HidOp::SetFeature);
        puck.destroy();
    }

    #[test]
    fn a_silent_page_gets_the_emulated_answer_and_the_slot_survives() {
        let (puck, mut kernel, mut events) = puck();
        puck.shared
            .inner
            .lock()
            .unwrap()
            .slots
            .iter_mut()
            .for_each(|s| s.relay.set_timeout(Duration::from_millis(60)));
        puck.relay_open(OWNER, 0).unwrap();
        kernel.get_feature(1, 2);
        request(&mut events);
        // Past the deadline the emulated controller answers (feature 1: 64 bytes).
        let (_, seq, status, data) = kernel.reply_in();
        assert_eq!((seq, status, data.len(), data[0]), (1, 0, 64, 1));
        assert!(puck.carries(0));
        assert_eq!(puck.relay_stats().timeouts, 1);
        puck.destroy();
    }

    #[test]
    fn three_failures_end_the_relay_and_the_slot_reports_its_controller_gone() {
        let (puck, mut kernel, mut events) = puck();
        puck.relay_open(OWNER, 0).unwrap();
        kernel.park(100, 2);
        assert_eq!(kernel.reply_in().3, [WIRELESS_STATUS, WIRELESS_CONNECTED]);
        kernel.park(101, 2);
        for seq in 1..=3 {
            kernel.get_feature(seq, 2);
            let req = request(&mut events);
            puck.relay_reply(0, OWNER, req.ticket, false, &[], Some("no"));
            assert_eq!(kernel.reply_in().1, seq);
        }
        assert!(!puck.carries(0));
        // The URB parked for the next report hears the disconnect.
        let (_, seq, _, report) = kernel.reply_in();
        assert_eq!(seq, 101);
        assert_eq!(report, [WIRELESS_STATUS, WIRELESS_DISCONNECTED]);
        // Now it is an empty slot again: it refuses.
        kernel.get_feature(9, 2);
        assert_eq!(kernel.reply().2, -EPIPE);
        puck.destroy();
    }

    #[test]
    fn closing_answers_held_requests_and_an_unlink_forgets_them() {
        let (puck, mut kernel, mut events) = puck();
        puck.relay_open(OWNER, 0).unwrap();
        puck.relay_open(OWNER, 1).unwrap();
        kernel.get_feature(1, 2);
        request(&mut events);
        kernel.get_feature(2, 3);
        let held = request(&mut events);
        // Another session cannot end it.
        puck.relay_close(0, Some(9), "stranger");
        assert!(kernel.silent(80));
        // The kernel gives up on interface 3's request.
        let mut wire = Vec::new();
        encode_cmd_unlink(&mut wire, 3, DEVID, 2);
        kernel.0.write_all(&wire).unwrap();
        let (cmd, seq, status) = kernel.reply();
        assert_eq!((cmd, seq, status), (usbip::RET_UNLINK, 3, -ECONNRESET));
        puck.relay_reply(1, OWNER, held.ticket, true, &[1], None);
        assert!(kernel.silent(80));
        // Ending interface 2's relay answers its request with the emulation's.
        puck.relay_close_owned_by(OWNER, "its session left");
        let (_, seq, status, data) = kernel.reply_in();
        assert_eq!((seq, status, data[0]), (1, 0, 1));
        assert!(!puck.carries(0) && !puck.carries(1));
        puck.destroy();
    }

    #[test]
    fn output_report_fallbacks_stay_inside_the_slot() {
        // SET_REPORT(output 0x80) on the control pipe of an empty slot stalls
        // and on a carried one goes to the page.
        let (puck, mut kernel, mut events) = puck();
        puck.relay_open(OWNER, 0).unwrap();
        let mut rumble = vec![0x80u8, 0, 1, 0, 0xff, 0xff];
        rumble.resize(10, 0);
        let set = |iface: u8| [0x21, 9, 0x80, REPORT_OUTPUT, iface, 0, 10, 0];
        kernel.send(&submit(1, DIR_OUT, 0, 10, set(3)), &rumble);
        assert_eq!(kernel.reply().2, -EPIPE);
        kernel.send(&submit(2, DIR_OUT, 0, 10, set(2)), &rumble);
        let req = request(&mut events);
        assert_eq!((req.op, req.report, req.slot), (HidOp::SetOutput, 0x80, 0));
        puck.relay_reply(0, OWNER, req.ticket, true, &[], None);
        assert_eq!(kernel.reply().1, 2);
        puck.destroy();
    }

    #[test]
    fn a_status_waits_in_the_fifo_for_a_node_that_is_opened_late() {
        let (puck, mut kernel, _events) = puck();
        puck.relay_open(OWNER, 0).unwrap();
        // Nobody listens yet (no URB): the status is kept, as any report is.
        kernel.park(1, 2);
        assert_eq!(kernel.reply_in().3, [WIRELESS_STATUS, WIRELESS_CONNECTED]);
        // The controller leaves and a new one comes before anything is parked:
        // what was queued for the old one is gone, the new status stands.
        puck.relay_close(0, None, "test");
        puck.relay_open(OWNER, 0).unwrap();
        kernel.park(2, 2);
        assert_eq!(kernel.reply_in().3, [WIRELESS_STATUS, WIRELESS_CONNECTED]);
        assert!(kernel.silent(80));
        puck.destroy();
    }
}
