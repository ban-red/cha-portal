//! Raw HID relay, Sans-IO: the bookkeeping for passing a real controller's
//! HID traffic through a [`crate::vhci`] device to the app that drives it
//! (docs/controllers.md, "Raw HID relay"; research note 10, designs A and B).
//!
//! The page owns the controller (WebHID). While a session holds the relay for
//! a pad:
//! - **Input reports** come from the page verbatim and are checked here
//!   against the report descriptor ([`ReportSizes`]): the id must be an input
//!   report of the device, the length exactly its size, and the report newer
//!   than the newest of its id (datagrams reorder, duplicate and drop). The
//!   caller completes the kernel's parked interrupt-IN URBs with what passes.
//! - **Feature and output reports** the app writes (and feature reports it
//!   asks for) are turned into [`HidRequest`]s for the page, which performs
//!   them on the real device and answers. A request that waits for an answer
//!   (a control transfer) is an [`InFlight`] with a ticket and a deadline; the
//!   caller holds the kernel's URB under the same ticket and completes it with
//!   the reply ([`Answer::Relayed`]) or, when the page is slow, wrong or gone,
//!   with the emulated device's answer ([`Answer::Emulate`]).
//!
//! Both directions are rate limited, a pad has at most [`MAX_IN_FLIGHT`]
//! requests outstanding, and [`FAIL_LIMIT`] failures in a row end the relay
//! (the device is emulated again until the page opens it anew).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::Serialize;
use tracing::{debug, info, warn};

use crate::gamepad::{HID_MAX_DATA, HidOp, HidRequest};
use crate::usbdev::{REPORT_FEATURE, REPORT_OUTPUT};

/// How long the page has to answer a request before the emulated device does.
/// The kernel gives a control transfer five seconds (`USB_CTRL_GET_TIMEOUT`),
/// so this leaves it ample time; a LAN round trip through WebHID takes a few
/// milliseconds.
pub const REQUEST_TIMEOUT: Duration = Duration::from_millis(1000);
/// Requests waiting for the page, per pad.
pub const MAX_IN_FLIGHT: usize = 16;
/// Timeouts and errors in a row that end the relay.
pub const FAIL_LIMIT: u32 = 3;
/// Round trips kept for the statistics.
const RTT_WINDOW: usize = 128;
/// Requests to the page: a start-up handshake is dozens in a burst, then a
/// rumble report every 40 ms and the odd haptic pulse.
const REQUEST_RATE: f64 = 400.0;
const REQUEST_BURST: f64 = 64.0;
/// Input reports from the page: a wired controller reports at 250 Hz; resends
/// and several report ids add some.
const INPUT_RATE: f64 = 1500.0;
const INPUT_BURST: f64 = 256.0;
/// A summary of the traffic goes to the log this often, while there is any.
const LOG_EVERY: Duration = Duration::from_secs(10);

/// The Steam Controller's command channel: feature report 1 carries a command
/// (`[cmd, length, payload...]`) written by SET_REPORT and its reply, which
/// starts with the same `cmd`, read by GET_REPORT.
pub const STEAM_COMMAND_FEATURE: u8 = 1;
/// Commands whose reply Steam reads and checks (get attributes, get string
/// attribute, get settings): a reply that doesn't start with the command is
/// not the reply to it.
const STEAM_REPLY_COMMANDS: &[u8] = &[0x83, 0xae, 0x89];
/// A reply that isn't the command's is asked for again this many times in all,
/// a few milliseconds apart (a firmware not ready yet, or a stale buffer),
/// before the emulated device answers.
pub const REPLY_ATTEMPTS: u8 = 3;
const REPLY_RETRY_AFTER: Duration = Duration::from_millis(5);
/// The same command this many times in a row, within [`LOOP_WITHIN`], is a
/// loop: the app is not satisfied with the replies it gets. It is logged, and
/// the replies to that command come from the emulated device (which the app
/// has accepted) until the app moves on.
const LOOP_AFTER: u32 = 30;
const LOOP_WITHIN: Duration = Duration::from_secs(3);
/// How many bytes of a report the log shows.
const LOG_BYTES: usize = 16;
/// The first requests of a relay are logged with their bytes at `info`.
const LOG_FIRST: u64 = 24;

/// What may reach the real controller (an allowlist; everything else is
/// answered by the emulated device or dropped). A relayed link can drop, and a
/// firmware flash that is cut half way can leave a controller dead, so a
/// command goes through only if SDL (`controller_constants.h`, zlib) or our own
/// player code names it and it is one Steam needs for normal play:
///
/// - `0x81` clear digital mappings, `0x85` set default digital mappings:
///   lizard mode off (our player sends both);
/// - `0x82` get digital mappings, `0x83` get attributes, `0x89` get settings,
///   `0xae` get string attribute, `0xb4` dongle get wireless state: reads;
/// - `0x87` set settings, `0x8e` load default settings: the settings Steam
///   writes at start-up (lizard mode, IMU mode, trackpad modes), kept in RAM
///   settings, not calibration or firmware;
/// - `0x8f` haptic pulse.
///
/// Everything else is blocked, by name or because nothing names it: `0x90`
/// (suspected reboot into the bootloader; SDL does not name it, so it is
/// unknown), `0x80` set digital mappings, `0x86` factory reset, `0x88` clear
/// settings, `0x8d` set controller mode, `0x9f` turn off, `0xa7`, `0xb5`,
/// `0xbf`, `0xc0`, `0xc3` and `0xce` calibration and IMU reset, `0xa9` set
/// serial number, `0xad` enable pairing, `0xaf`, `0xb0`, `0xb1`, `0xb2` and
/// `0xb3` radio and dongle records, `0xb7` to `0xb9` audio update (flash),
/// `0xc1` set audio mapping, `0xea` and `0xeb` (Deck only), and the ids Steam
/// sent that nothing names (`0xdc`, `0xe2`, `0xf2`). The page enforces the
/// same list (`steam-protocol.ts`, `RELAYED_COMMANDS`).
pub const RELAYED_COMMANDS: &[u8] = &[0x81, 0x82, 0x83, 0x85, 0x87, 0x89, 0x8e, 0x8f, 0xae, 0xb4];
/// Output reports that may reach the real controller: SDL's haptic and rumble
/// outputs (`ID_OUT_REPORT_HAPTIC_RUMBLE` to `ID_OUT_REPORT_HAPTIC_SCRIPT`).
/// Any other output report (`0x86` to `0x89` are not named anywhere) is dropped.
pub const RELAYED_OUTPUTS: std::ops::RangeInclusive<u8> = 0x80..=0x85;

pub fn command_relayed(cmd: u8) -> bool {
    RELAYED_COMMANDS.contains(&cmd)
}

pub fn output_relayed(id: u8) -> bool {
    RELAYED_OUTPUTS.contains(&id)
}

/// The firmware and bootloader build times Steam is told of, for the emulated
/// device and for the relay's attribute replies: 2038-01-19, the largest value
/// that is positive as a signed 32-bit time. Steam compares a controller's
/// firmware time with the firmware it bundles, and offers an update when the
/// bundled one is newer; nothing bundled is newer than this, so it never
/// starts one through Cha (a firmware update needs the controller plugged
/// into a machine running Steam). That is a guess about Steam, which is closed.
pub const NEVER_UPDATE_TIME: u32 = 0x7fff_ffff;
/// The attributes (SDL's `ControllerAttributes`) that are build times: the
/// firmware's (4), the radio's (5), the dongle's firmware (8) and the
/// bootloader's (10).
pub const BUILD_TIME_ATTRIBUTES: [u8; 4] = [4, 5, 8, 10];

/// Sets the build-time attributes in a `0x83` reply (`[0x83, length, (tag,
/// u32 little endian)...]`) to [`NEVER_UPDATE_TIME`]; true if there were any.
pub fn rewrite_build_times(reply: &mut [u8]) -> bool {
    if reply.first() != Some(&0x83) {
        return false;
    }
    let Some(&length) = reply.get(1) else {
        return false;
    };
    let Some(attrs) = reply.get_mut(2..) else {
        return false;
    };
    let len = usize::from(length).min(attrs.len());
    let mut changed = false;
    for attr in attrs[..len].chunks_exact_mut(5) {
        if BUILD_TIME_ATTRIBUTES.contains(&attr[0]) {
            attr[1..].copy_from_slice(&NEVER_UPDATE_TIME.to_le_bytes());
            changed = true;
        }
    }
    changed
}

/// The Steam Controller's lizard-mode mouse and keyboard input reports. They
/// would type and click on the node (the kernel makes input devices of the
/// descriptor's mouse and keyboard), and Steam turns lizard mode off anyway.
pub const STEAM_LIZARD_REPORTS: &[u8] = &[0x40, 0x41];

/// The sizes, in bytes without the id, of a device's reports by id, from its
/// HID report descriptor (short items only: Report ID, Report Size, Report
/// Count and the Input, Output and Feature main items).
#[derive(Debug, Clone)]
pub struct ReportSizes {
    input: [Option<u16>; 256],
    output: [Option<u16>; 256],
    feature: [Option<u16>; 256],
}

impl ReportSizes {
    pub fn parse(descriptor: &[u8]) -> Self {
        let mut bits = [[None::<u32>; 256]; 3];
        let (mut id, mut size, mut count) = (0usize, 0u32, 0u32);
        let mut at = 0;
        while at < descriptor.len() {
            let prefix = descriptor[at];
            if prefix == 0xfe {
                // A long item: its data length is the next byte.
                at += 3 + usize::from(descriptor.get(at + 1).copied().unwrap_or(0));
                continue;
            }
            let n = match prefix & 3 {
                3 => 4,
                n => usize::from(n),
            };
            let Some(data) = descriptor.get(at + 1..at + 1 + n) else {
                break;
            };
            let value = data
                .iter()
                .rev()
                .fold(0u32, |v, b| (v << 8) | u32::from(*b));
            match (prefix >> 2 & 3, prefix >> 4) {
                // Global: Report Size, Report ID, Report Count.
                (1, 7) => size = value,
                (1, 8) => id = (value & 0xff) as usize,
                (1, 9) => count = value,
                // Main: Input, Output, Feature.
                (0, tag @ (8 | 9 | 11)) => {
                    let kind = match tag {
                        8 => 0,
                        9 => 1,
                        _ => 2,
                    };
                    *bits[kind][id].get_or_insert(0) += size * count;
                }
                _ => {}
            }
            at += 1 + n;
        }
        let bytes = |kind: usize| {
            let mut out = [None; 256];
            for (slot, b) in out.iter_mut().zip(&bits[kind]) {
                *slot = b.map(|b| b.div_ceil(8).min(u32::from(u16::MAX)) as u16);
            }
            out
        };
        Self {
            input: bytes(0),
            output: bytes(1),
            feature: bytes(2),
        }
    }

    pub fn input(&self, id: u8) -> Option<usize> {
        self.input[usize::from(id)].map(usize::from)
    }

    pub fn output(&self, id: u8) -> Option<usize> {
        self.output[usize::from(id)].map(usize::from)
    }

    pub fn feature(&self, id: u8) -> Option<usize> {
        self.feature[usize::from(id)].map(usize::from)
    }
}

/// A token bucket: `rate` a second, up to `burst` at once.
#[derive(Debug)]
struct Bucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn new(rate: f64, burst: f64, now: Instant) -> Self {
        Self {
            rate,
            burst,
            tokens: burst,
            at: now,
        }
    }

    fn take(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.at).as_secs_f64();
        self.at = now;
        self.tokens = (self.tokens + dt * self.rate).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// What the app asked, kept to answer it from the emulated device if the real
/// one does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// GET_REPORT: `kind` is [`REPORT_FEATURE`] or [`REPORT_OUTPUT`].
    Get { kind: u8, id: u8 },
    /// SET_REPORT with the data stage as the kernel sent it (the id leads).
    Set { kind: u8, id: u8, data: Vec<u8> },
}

/// A request the page has not answered yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlight {
    pub ticket: u32,
    pub req: Request,
    issued: Instant,
    deadline: Instant,
    /// A get of a command's reply: the command it must start with.
    expect: Option<u8>,
    /// Times the page was asked (a reply that wasn't the command's is asked for again).
    attempts: u8,
    /// When to ask again.
    retry_at: Option<Instant>,
}

/// What the periodic check found: requests to answer, and requests to send to
/// the page again.
#[derive(Debug, Default)]
pub struct Due {
    pub answers: Vec<(InFlight, Answer)>,
    pub resend: Vec<HidRequest>,
}

/// How a held request is answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The real controller's: the report with its id for a get, nothing for a set.
    Relayed(Vec<u8>),
    /// The emulated device answers, as it does without a relay.
    Emulate,
}

/// What became of an input report from the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Complete the kernel's interrupt-IN URBs with it.
    Accept,
    /// Not this session's relay (or none is open).
    NotRelaying,
    /// Not a report of the device, or the wrong length.
    Invalid,
    /// A lizard-mode report; never passed.
    Denied,
    /// The same report as the last of its id, or an older one.
    Stale,
    /// Over the rate limit.
    Limited,
}

/// Traffic and round trips, for the streamer's stats and its log.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RelayStats {
    /// Input reports passed to the app.
    pub reports: u64,
    /// Input reports refused: not the device's, wrong length, lizard mode or over the rate.
    pub reports_dropped: u64,
    /// Feature and output requests sent to the page that wait for an answer.
    pub requests: u64,
    /// Output reports sent to the page without waiting (interrupt OUT).
    pub outputs: u64,
    pub replies: u64,
    /// Requests the emulated device answered because the page did not in time.
    pub timeouts: u64,
    /// Requests the page answered with a failure (the emulated device answered).
    pub errors: u64,
    /// Requests not sent for the rate limit or too many outstanding (emulated, or dropped for outputs).
    pub limited: u64,
    /// Replies to a command that didn't start with it (asked for again, repaired or emulated).
    pub reply_mismatches: u64,
    /// Of those, replies fixed up (the report id was in the data) and answers asked for again.
    pub reply_repaired: u64,
    pub reply_retries: u64,
    /// Replies answered by the emulated device because the app was looping on a command.
    pub loops_broken: u64,
    /// Writes not let through to the controller (not on the allowlist).
    pub blocked: u64,
    /// Median and largest round trip of the last 128 replies, microseconds.
    pub rtt_p50_us: Option<u32>,
    pub rtt_max_us: Option<u32>,
}

impl RelayStats {
    /// The traffic of two pads as one (the round trips: the larger of each).
    pub fn merge(&mut self, other: &RelayStats) {
        self.reports += other.reports;
        self.reports_dropped += other.reports_dropped;
        self.requests += other.requests;
        self.outputs += other.outputs;
        self.replies += other.replies;
        self.timeouts += other.timeouts;
        self.errors += other.errors;
        self.limited += other.limited;
        self.reply_mismatches += other.reply_mismatches;
        self.reply_repaired += other.reply_repaired;
        self.reply_retries += other.reply_retries;
        self.loops_broken += other.loops_broken;
        self.blocked += other.blocked;
        self.rtt_p50_us = self.rtt_p50_us.max(other.rtt_p50_us);
        self.rtt_max_us = self.rtt_max_us.max(other.rtt_max_us);
    }
}

pub struct Relay {
    slot: usize,
    sizes: ReportSizes,
    denied: &'static [u8],
    /// The session that opened it; `None` while nothing is relayed.
    owner: Option<u64>,
    next_ticket: u32,
    timeout: Duration,
    in_flight: Vec<InFlight>,
    /// The newest `q` passed for each input report id.
    newest: [Option<u32>; 256],
    input_bucket: Bucket,
    request_bucket: Bucket,
    fails: u32,
    stats: RelayStats,
    rtts: VecDeque<u32>,
    logged_at: Instant,
    logged: u64,
    /// The first of each request kind is logged at `info`.
    seen: Vec<(u8, u8, u8)>,
    /// The command last written to the command channel, and how many times
    /// each has been, for the log.
    last_cmd: Option<u8>,
    cmd_counts: std::collections::BTreeMap<u8, u64>,
    /// The command, how many times in a row, and since when.
    cmd_run: (u8, u32, Instant),
    /// The start of the last reply the page gave, for the log.
    last_reply: Vec<u8>,
    /// Requests logged so far (the first few, with their bytes).
    logged_requests: u64,
    /// The command last written was not let through, so the next read of the
    /// command channel is the emulated device's.
    blocked_cmd: bool,
    /// The blocked ids warned about so far.
    warned: Vec<(u8, u8)>,
}

impl Relay {
    /// `denied`: input report ids never passed.
    pub fn new(slot: usize, descriptor: &[u8], denied: &'static [u8], now: Instant) -> Self {
        Self {
            slot,
            sizes: ReportSizes::parse(descriptor),
            denied,
            owner: None,
            next_ticket: 0,
            timeout: REQUEST_TIMEOUT,
            in_flight: Vec::new(),
            newest: [None; 256],
            input_bucket: Bucket::new(INPUT_RATE, INPUT_BURST, now),
            request_bucket: Bucket::new(REQUEST_RATE, REQUEST_BURST, now),
            fails: 0,
            stats: RelayStats::default(),
            rtts: VecDeque::new(),
            logged_at: now,
            logged: 0,
            seen: Vec::new(),
            last_cmd: None,
            cmd_counts: Default::default(),
            cmd_run: (0, 0, now),
            last_reply: Vec::new(),
            logged_requests: 0,
            blocked_cmd: false,
            warned: Vec::new(),
        }
    }

    /// A shorter wait, so a test need not sit through [`REQUEST_TIMEOUT`].
    #[cfg(test)]
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn is_active(&self) -> bool {
        self.owner.is_some()
    }

    #[cfg(test)]
    pub fn owner(&self) -> Option<u64> {
        self.owner
    }

    #[cfg(test)]
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// `owner` takes the relay. A different session's earlier requests are
    /// returned, to be answered by the emulated device.
    pub fn open(&mut self, owner: u64, now: Instant) -> Vec<InFlight> {
        let dropped = if self.owner.is_some_and(|o| o != owner) {
            std::mem::take(&mut self.in_flight)
        } else {
            Vec::new()
        };
        info!(slot = self.slot, owner, "hid relay: opened");
        self.owner = Some(owner);
        self.newest = [None; 256];
        self.fails = 0;
        self.blocked_cmd = false;
        self.logged_at = now;
        self.logged = self.traffic();
        dropped
    }

    /// Ends the relay (of `owner` only, if given; a no-op if it is another's)
    /// and returns the requests still waiting, to be answered by the emulated
    /// device.
    pub fn close(&mut self, owner: Option<u64>, why: &str) -> Vec<InFlight> {
        let Some(current) = self.owner else {
            return Vec::new();
        };
        if owner.is_some_and(|o| o != current) {
            return Vec::new();
        }
        info!(
            slot = self.slot,
            why,
            stats = ?self.stats(),
            commands = %self.commands(),
            "hid relay: closed"
        );
        self.owner = None;
        std::mem::take(&mut self.in_flight)
    }

    fn traffic(&self) -> u64 {
        self.stats.reports + self.stats.requests + self.stats.outputs
    }

    /// An input report from `owner` (the id first), numbered `q`.
    pub fn input(&mut self, owner: u64, q: u32, report: &[u8], now: Instant) -> Verdict {
        if self.owner != Some(owner) {
            return Verdict::NotRelaying;
        }
        let Some((&id, data)) = report.split_first() else {
            return self.refused(Verdict::Invalid, report);
        };
        if self.denied.contains(&id) {
            return self.refused(Verdict::Denied, report);
        }
        if self.sizes.input(id) != Some(data.len()) {
            return self.refused(Verdict::Invalid, report);
        }
        if let Some(prev) = self.newest[usize::from(id)]
            && (q == prev || !cha_proto::input::seq_newer(q, prev))
        {
            // A resend of what passed, or a late copy: not news, not an error.
            return Verdict::Stale;
        }
        if !self.input_bucket.take(now) {
            self.stats.reports_dropped += 1;
            return Verdict::Limited;
        }
        self.newest[usize::from(id)] = Some(q);
        self.stats.reports += 1;
        Verdict::Accept
    }

    fn refused(&mut self, verdict: Verdict, report: &[u8]) -> Verdict {
        self.stats.reports_dropped += 1;
        // The first few say why, the rest are counted.
        if self.stats.reports_dropped <= 5 {
            warn!(
                slot = self.slot,
                ?verdict,
                id = report.first().copied(),
                len = report.len(),
                expected = report.first().and_then(|id| self.sizes.input(*id)),
                "hid relay: input report refused"
            );
        }
        verdict
    }

    /// The payload of a report the app wrote, as the page should send it: the
    /// id off, zero-padded to the descriptor's size. `None` if the report
    /// isn't one of the device's or is longer than it should be.
    fn payload(size: Option<usize>, id: u8, data: &[u8]) -> Option<Vec<u8>> {
        let size = size.filter(|s| *s <= HID_MAX_DATA)?;
        let body = if id == 0 {
            data
        } else {
            match data.split_first() {
                Some((&first, rest)) if first == id => rest,
                _ => return None,
            }
        };
        if body.len() > size {
            return None;
        }
        let mut out = body.to_vec();
        out.resize(size, 0);
        Some(out)
    }

    fn request_for(
        &self,
        ticket: u32,
        op: HidOp,
        report: u8,
        payload: &[u8],
        owner: u64,
    ) -> HidRequest {
        let mut data = [0u8; HID_MAX_DATA];
        data[..payload.len()].copy_from_slice(payload);
        HidRequest {
            slot: self.slot,
            owner,
            ticket,
            op,
            report,
            len: payload.len() as u8,
            data,
        }
    }

    /// Counts a write that the allowlist keeps from the controller, and warns
    /// once per kind and id. `what` is 0 for a command, else the report kind.
    fn block(&mut self, what: u8, id: u8) {
        self.stats.blocked += 1;
        if !self.warned.contains(&(what, id)) {
            self.warned.push((what, id));
            warn!(
                slot = self.slot,
                id = format_args!("{id:#04x}"),
                kind = if what == 0 {
                    "command"
                } else {
                    "output report"
                },
                "hid relay: not passed to the real controller (not on the allowlist); the emulated device answers"
            );
        }
    }

    /// Whether a request may go to the page now.
    fn admit(&mut self, waits: bool, now: Instant) -> Option<u64> {
        let owner = self.owner?;
        if (waits && self.in_flight.len() >= MAX_IN_FLIGHT) || !self.request_bucket.take(now) {
            self.stats.limited += 1;
            return None;
        }
        Some(owner)
    }

    fn note_first(&mut self, op: HidOp, kind: u8, id: u8) {
        let key = (op as u8, kind, id);
        if !self.seen.contains(&key) {
            self.seen.push(key);
            info!(
                slot = self.slot,
                op = op.name(),
                id = format_args!("{id:#04x}"),
                "hid relay: first request of its kind"
            );
        }
    }

    fn hold(&mut self, req: Request, expect: Option<u8>, now: Instant) -> u32 {
        self.next_ticket = self.next_ticket.wrapping_add(1).max(1);
        let ticket = self.next_ticket;
        self.in_flight.push(InFlight {
            ticket,
            req,
            issued: now,
            deadline: now + self.timeout,
            expect,
            attempts: 1,
            retry_at: None,
        });
        self.stats.requests += 1;
        ticket
    }

    /// GET_REPORT of feature report `id`: the request for the page, held
    /// under a ticket; `None` if the emulated device should answer.
    pub fn request_get(&mut self, kind: u8, id: u8, now: Instant) -> Option<(u32, HidRequest)> {
        if kind != REPORT_FEATURE {
            return None;
        }
        let size = self.sizes.feature(id).filter(|s| *s <= HID_MAX_DATA)?;
        // The app keeps writing the same command and is not satisfied with the
        // replies: the emulated device's will do (see `LOOP_AFTER`).
        if id == STEAM_COMMAND_FEATURE
            && self.cmd_run.1 >= LOOP_AFTER
            && self
                .last_cmd
                .is_some_and(|c| STEAM_REPLY_COMMANDS.contains(&c))
        {
            self.stats.loops_broken += 1;
            return None;
        }
        // The command last written was not passed to the controller: its
        // reply is the emulated device's.
        if id == STEAM_COMMAND_FEATURE && self.blocked_cmd {
            return None;
        }
        let owner = self.admit(true, now)?;
        self.note_first(HidOp::GetFeature, kind, id);
        // The reply to the command last written must start with it.
        let expect = (id == STEAM_COMMAND_FEATURE)
            .then_some(self.last_cmd)
            .flatten()
            .filter(|c| STEAM_REPLY_COMMANDS.contains(c));
        let ticket = self.hold(Request::Get { kind, id }, expect, now);
        let mut request = self.request_for(ticket, HidOp::GetFeature, id, &[], owner);
        // How many bytes the report has, for the page's information.
        request.len = size as u8;
        Some((ticket, request))
    }

    /// SET_REPORT of a feature or output report; `data` is the data stage.
    pub fn request_set(
        &mut self,
        kind: u8,
        id: u8,
        data: &[u8],
        now: Instant,
    ) -> Option<(u32, HidRequest)> {
        let (op, size) = match kind {
            REPORT_FEATURE => (HidOp::SetFeature, self.sizes.feature(id)),
            REPORT_OUTPUT => (HidOp::SetOutput, self.sizes.output(id)),
            _ => return None,
        };
        let payload = Self::payload(size, id, data)?;
        // Only the command channel and the haptic outputs are written to the
        // real controller, and of the commands only those on the allowlist.
        if kind == REPORT_FEATURE {
            let cmd = payload.first().copied().unwrap_or(0);
            if id != STEAM_COMMAND_FEATURE || !command_relayed(cmd) {
                self.blocked_cmd = id == STEAM_COMMAND_FEATURE;
                self.block(0, if id == STEAM_COMMAND_FEATURE { cmd } else { id });
                return None;
            }
            self.blocked_cmd = false;
        } else if !output_relayed(id) {
            self.block(REPORT_OUTPUT, id);
            return None;
        }
        let owner = self.admit(true, now)?;
        self.note_first(op, kind, id);
        if kind == REPORT_FEATURE && id == STEAM_COMMAND_FEATURE {
            self.note_command(&payload, now);
        }
        self.log_request(op, id, &payload);
        let ticket = self.hold(
            Request::Set {
                kind,
                id,
                data: data.to_vec(),
            },
            None,
            now,
        );
        Some((ticket, self.request_for(ticket, op, id, &payload, owner)))
    }

    /// An interrupt OUT transfer (the id leads): the request for the page, or
    /// `None` if it is not an output report of the device or over the rate
    /// (the transfer completes either way).
    pub fn request_output(&mut self, data: &[u8], now: Instant) -> Option<HidRequest> {
        let &id = data.first()?;
        let payload = Self::payload(self.sizes.output(id), id, data)?;
        if !output_relayed(id) {
            self.block(REPORT_OUTPUT, id);
            return None;
        }
        let owner = self.admit(false, now)?;
        self.note_first(HidOp::Output, REPORT_OUTPUT, id);
        self.log_request(HidOp::Output, id, &payload);
        self.stats.outputs += 1;
        Some(self.request_for(0, HidOp::Output, id, &payload, owner))
    }

    /// A command written to the command channel (`payload`: command, length,
    /// bytes): counted, remembered for checking its reply, and a loop of the
    /// same one is reported once.
    fn note_command(&mut self, payload: &[u8], now: Instant) {
        let Some(&cmd) = payload.first() else { return };
        self.last_cmd = Some(cmd);
        *self.cmd_counts.entry(cmd).or_default() += 1;
        let (last, times, since) = self.cmd_run;
        self.cmd_run = if last == cmd && now.saturating_duration_since(since) < LOOP_WITHIN {
            (cmd, times + 1, since)
        } else {
            (cmd, 1, now)
        };
        if self.cmd_run.1 == LOOP_AFTER {
            warn!(
                slot = self.slot,
                cmd = format_args!("{cmd:#04x}"),
                times = self.cmd_run.1,
                command = format_args!("{:02x?}", &payload[..payload.len().min(LOG_BYTES)]),
                last_reply = format_args!("{:02x?}", self.last_reply),
                "hid relay: the app wrote the same command again and again (it is not satisfied with the reply)"
            );
        }
    }

    /// The first requests of a relay are logged with their bytes at `info`,
    /// the rest at `debug`.
    fn log_request(&mut self, op: HidOp, id: u8, payload: &[u8]) {
        self.logged_requests += 1;
        let bytes = format_args!("{:02x?}", &payload[..payload.len().min(LOG_BYTES)]);
        if self.logged_requests <= LOG_FIRST {
            info!(slot = self.slot, op = op.name(), id = format_args!("{id:#04x}"), %bytes, "hid relay: request");
        } else {
            debug!(slot = self.slot, op = op.name(), id = format_args!("{id:#04x}"), %bytes, "hid relay: request");
        }
    }

    fn record_rtt(&mut self, rtt: Duration) {
        if self.rtts.len() == RTT_WINDOW {
            self.rtts.pop_front();
        }
        self.rtts
            .push_back(rtt.as_micros().min(u128::from(u32::MAX)) as u32);
    }

    /// A timeout or an error: counts toward ending the relay. Returns the
    /// requests still waiting if that was the last straw.
    fn failed(&mut self) -> Vec<InFlight> {
        self.fails += 1;
        if self.fails >= FAIL_LIMIT && self.owner.is_some() {
            warn!(
                slot = self.slot,
                fails = self.fails,
                "hid relay: the page keeps failing to answer, back to the emulated device"
            );
            self.owner = None;
            return std::mem::take(&mut self.in_flight);
        }
        Vec::new()
    }

    /// The page's answer to `ticket`. Nothing if it is not `owner`'s relay or
    /// the request is gone (answered by the emulated device already).
    /// `error` is why the page failed, for the log.
    pub fn reply(
        &mut self,
        owner: u64,
        ticket: u32,
        ok: bool,
        data: &[u8],
        error: Option<&str>,
        now: Instant,
    ) -> Vec<(InFlight, Answer)> {
        if self.owner != Some(owner) {
            return Vec::new();
        }
        let Some(at) = self.in_flight.iter().position(|f| f.ticket == ticket) else {
            debug!(slot = self.slot, ticket, "hid relay: a late reply, dropped");
            return Vec::new();
        };
        let rtt = now.saturating_duration_since(self.in_flight[at].issued);
        self.record_rtt(rtt);
        self.stats.replies += 1;
        let head = &data[..data.len().min(LOG_BYTES)];
        if ok && matches!(self.in_flight[at].req, Request::Get { .. }) {
            self.last_reply = head.to_vec();
            if self.logged_requests <= LOG_FIRST {
                info!(
                    slot = self.slot,
                    ticket,
                    len = data.len(),
                    bytes = format_args!("{head:02x?}"),
                    "hid relay: the page's reply"
                );
            }
        }
        let held = &self.in_flight[at];
        // Data that can't be the report (longer than it) is a failure of the page.
        let mut unusable = false;
        let mut answer = match (&held.req, ok) {
            (Request::Get { id, .. }, true) => match self.sizes.feature(*id) {
                Some(size) if data.len() <= size => {
                    let mut out = vec![*id];
                    out.extend_from_slice(data);
                    out.resize(1 + size, 0);
                    Answer::Relayed(out)
                }
                _ => {
                    unusable = true;
                    Answer::Emulate
                }
            },
            (Request::Set { .. }, true) => Answer::Relayed(Vec::new()),
            (_, false) => Answer::Emulate,
        };
        // A reply to a command starts with the command. Anything else is a
        // reply to something else, or none yet: repaired if the page only
        // left the report id in, else asked for again, else the emulation's.
        if let (Some(cmd), Answer::Relayed(out)) = (held.expect, &mut answer) {
            let id = out[0];
            if data.first() == Some(&cmd) {
                // As it should be.
            } else if data.first() == Some(&id) && data.get(1) == Some(&cmd) {
                out.remove(1);
                out.push(0);
                self.stats.reply_repaired += 1;
                warn!(
                    slot = self.slot,
                    ticket,
                    cmd = format_args!("{cmd:#04x}"),
                    "hid relay: the page's reply began with the report id; stripped it"
                );
            } else {
                self.stats.reply_mismatches += 1;
                let attempts = held.attempts;
                warn!(
                    slot = self.slot,
                    ticket,
                    cmd = format_args!("{cmd:#04x}"),
                    attempts,
                    len = data.len(),
                    bytes = format_args!("{head:02x?}"),
                    "hid relay: the page's reply is not the command's"
                );
                if attempts < REPLY_ATTEMPTS {
                    let held = &mut self.in_flight[at];
                    held.attempts += 1;
                    held.retry_at = Some(now + REPLY_RETRY_AFTER);
                    self.stats.reply_retries += 1;
                    return Vec::new();
                }
                answer = Answer::Emulate;
            }
        }
        // Steam must not see a firmware older than the one it bundles, or it
        // would start an update through this link.
        if let (Some(0x83), Answer::Relayed(out)) = (held.expect, &mut answer)
            && rewrite_build_times(&mut out[1..])
            && !self.warned.contains(&(1, 0x83))
        {
            self.warned.push((1, 0x83));
            info!(
                slot = self.slot,
                "hid relay: the controller's firmware and bootloader build times are reported as newer than any update"
            );
        }
        let held = self.in_flight.remove(at);
        debug!(
            slot = self.slot,
            ticket,
            rtt_us = rtt.as_micros() as u64,
            relayed = matches!(answer, Answer::Relayed(_)),
            "hid relay: reply"
        );
        let mut out = vec![(held, answer.clone())];
        if !ok || unusable {
            self.stats.errors += 1;
            warn!(
                slot = self.slot,
                ticket, error, "hid relay: the page could not perform the request, emulating"
            );
            out.extend(self.failed().into_iter().map(|f| (f, Answer::Emulate)));
        } else if answer == Answer::Emulate {
            // A reply that was not the command's after every try: the content
            // is wrong, not the page: no strike against the relay.
        } else {
            self.fails = 0;
        }
        out
    }

    /// Requests whose time is up, to be answered by the emulated device, and
    /// replies that were not the command's, to be asked for again.
    pub fn expire(&mut self, now: Instant) -> Due {
        let mut due = Due::default();
        while let Some(at) = self.in_flight.iter().position(|f| now >= f.deadline) {
            let held = self.in_flight.remove(at);
            self.stats.timeouts += 1;
            warn!(
                slot = self.slot,
                ticket = held.ticket,
                req = ?held.req,
                waited_ms = now.saturating_duration_since(held.issued).as_millis() as u64,
                "hid relay: no reply from the page in time, emulating"
            );
            due.answers.push((held, Answer::Emulate));
            due.answers
                .extend(self.failed().into_iter().map(|f| (f, Answer::Emulate)));
        }
        if let Some(owner) = self.owner {
            for i in 0..self.in_flight.len() {
                let held = &mut self.in_flight[i];
                if held.retry_at.is_some_and(|at| now >= at)
                    && let Request::Get { id, .. } = held.req
                {
                    held.retry_at = None;
                    let ticket = held.ticket;
                    let size = self.sizes.feature(id).unwrap_or(0);
                    let mut request = self.request_for(ticket, HidOp::GetFeature, id, &[], owner);
                    request.len = size as u8;
                    due.resend.push(request);
                }
            }
        }
        self.log_summary(now);
        due
    }

    /// The kernel gave up on a held request.
    pub fn cancel(&mut self, ticket: u32) {
        self.in_flight.retain(|f| f.ticket != ticket);
    }

    /// When the oldest waiting request times out.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.in_flight
            .iter()
            .flat_map(|f| [Some(f.deadline), f.retry_at])
            .flatten()
            .min()
    }

    fn log_summary(&mut self, now: Instant) {
        if self.owner.is_none() || now.saturating_duration_since(self.logged_at) < LOG_EVERY {
            return;
        }
        self.logged_at = now;
        let traffic = self.traffic();
        if traffic != self.logged {
            self.logged = traffic;
            info!(
                slot = self.slot,
                stats = ?self.stats(),
                commands = %self.commands(),
                "hid relay: traffic"
            );
        }
    }

    /// How many times each command was written to the command channel, for the log.
    fn commands(&self) -> String {
        self.cmd_counts
            .iter()
            .map(|(cmd, n)| format!("{cmd:#04x}:{n}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn stats(&self) -> RelayStats {
        let mut sorted: Vec<u32> = self.rtts.iter().copied().collect();
        sorted.sort_unstable();
        RelayStats {
            rtt_p50_us: sorted.get(sorted.len() / 2).copied(),
            rtt_max_us: sorted.last().copied(),
            ..self.stats.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam_controller::DESCRIPTOR_TRITON_USB;

    const OWNER: u64 = 7;

    fn relay(now: Instant) -> Relay {
        Relay::new(0, DESCRIPTOR_TRITON_USB, STEAM_LIZARD_REPORTS, now)
    }

    fn opened(now: Instant) -> Relay {
        let mut r = relay(now);
        r.open(OWNER, now);
        r
    }

    /// A report of `id` with `n` data bytes.
    fn report(id: u8, n: usize) -> Vec<u8> {
        let mut v = vec![id];
        v.resize(1 + n, 0xab);
        v
    }

    #[test]
    fn the_real_descriptors_report_sizes() {
        let s = ReportSizes::parse(DESCRIPTOR_TRITON_USB);
        // Inputs: the state with the quaternion, the rest of the vendor
        // collection, and lizard mode's mouse and keyboard.
        assert_eq!(s.input(0x42), Some(53));
        assert_eq!(s.input(0x44), Some(5));
        assert_eq!(s.input(0x79), Some(1));
        assert_eq!(s.input(0x43), Some(14));
        assert_eq!(s.input(0x7b), Some(12));
        assert_eq!(s.input(0x45), Some(45));
        assert_eq!(s.input(0x40), Some(5));
        assert_eq!(s.input(0x41), Some(8));
        assert_eq!(s.input(0x01), None);
        assert_eq!(s.input(0x80), None, "an output id is not an input");
        // Outputs: rumble, the pulse, and the ones nobody has explained.
        assert_eq!(s.output(0x80), Some(9));
        assert_eq!(s.output(0x81), Some(7));
        assert_eq!(s.output(0x89), Some(63));
        assert_eq!(s.output(0x42), None);
        // Features 1 and 2: 63 bytes each.
        assert_eq!(s.feature(1), Some(63));
        assert_eq!(s.feature(2), Some(63));
        assert_eq!(s.feature(3), None);
    }

    #[test]
    fn the_parser_handles_unnumbered_and_truncated_descriptors() {
        // One unnumbered 3-byte input, 2 bytes of output, no ids.
        #[rustfmt::skip]
        let d = [
            0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01,
            0x75, 0x08, 0x95, 0x03, 0x81, 0x02,
            0x95, 0x02, 0x91, 0x02, 0xC0,
        ];
        let s = ReportSizes::parse(&d);
        assert_eq!(
            (s.input(0), s.output(0), s.feature(0)),
            (Some(3), Some(2), None)
        );
        // 12 one-bit fields are two bytes.
        let bits = [0x75, 0x01, 0x95, 0x0c, 0x81, 0x02];
        assert_eq!(ReportSizes::parse(&bits).input(0), Some(2));
        // A cut item stops the parse without a panic.
        assert_eq!(ReportSizes::parse(&[0x75]).input(0), None);
        assert_eq!(ReportSizes::parse(&[]).input(0), None);
    }

    #[test]
    fn input_reports_are_checked_against_the_descriptor() {
        let now = Instant::now();
        let mut r = opened(now);
        assert_eq!(r.input(OWNER, 1, &report(0x42, 53), now), Verdict::Accept);
        // A different session's relay is not this one's.
        assert_eq!(r.input(8, 2, &report(0x42, 53), now), Verdict::NotRelaying);
        // The wrong length (short, long), an unknown id, an empty report.
        assert_eq!(r.input(OWNER, 2, &report(0x42, 52), now), Verdict::Invalid);
        assert_eq!(r.input(OWNER, 2, &report(0x42, 54), now), Verdict::Invalid);
        assert_eq!(r.input(OWNER, 2, &report(0x66, 53), now), Verdict::Invalid);
        assert_eq!(r.input(OWNER, 2, &[], now), Verdict::Invalid);
        // An output id is not an input one.
        assert_eq!(r.input(OWNER, 2, &report(0x80, 9), now), Verdict::Invalid);
        // Lizard mode's mouse and keyboard are never passed, though they fit.
        assert_eq!(r.input(OWNER, 2, &report(0x40, 5), now), Verdict::Denied);
        assert_eq!(r.input(OWNER, 2, &report(0x41, 8), now), Verdict::Denied);
        // The battery report is fine.
        assert_eq!(r.input(OWNER, 3, &report(0x43, 14), now), Verdict::Accept);
        let stats = r.stats();
        assert_eq!((stats.reports, stats.reports_dropped), (2, 7));
    }

    #[test]
    fn nothing_passes_before_the_relay_is_opened_or_after_it_closes() {
        let now = Instant::now();
        let mut r = relay(now);
        assert_eq!(
            r.input(OWNER, 1, &report(0x42, 53), now),
            Verdict::NotRelaying
        );
        r.open(OWNER, now);
        assert_eq!(r.input(OWNER, 1, &report(0x42, 53), now), Verdict::Accept);
        // Another session cannot close it; the owner can.
        assert!(r.close(Some(9), "test").is_empty());
        assert!(r.is_active());
        r.close(Some(OWNER), "test");
        assert!(!r.is_active());
        assert_eq!(
            r.input(OWNER, 2, &report(0x42, 53), now),
            Verdict::NotRelaying
        );
    }

    #[test]
    fn sequence_numbers_are_kept_per_report_id() {
        let now = Instant::now();
        let mut r = opened(now);
        let state = report(0x42, 53);
        let battery = report(0x43, 14);
        assert_eq!(r.input(OWNER, 10, &state, now), Verdict::Accept);
        // The battery's own sequence: an older number than the state's is fine.
        assert_eq!(r.input(OWNER, 9, &battery, now), Verdict::Accept);
        // A resend of the same q, and an older one: not applied again.
        assert_eq!(r.input(OWNER, 10, &state, now), Verdict::Stale);
        assert_eq!(r.input(OWNER, 8, &state, now), Verdict::Stale);
        assert_eq!(r.input(OWNER, 11, &state, now), Verdict::Accept);
        // Stale ones are not errors.
        assert_eq!(r.stats().reports_dropped, 0);
        // Opening again starts the numbering over (a new page counts from 1),
        // and the counter wraps.
        r.open(OWNER, now);
        assert_eq!(r.input(OWNER, u32::MAX - 1, &state, now), Verdict::Accept);
        assert_eq!(r.input(OWNER, u32::MAX, &state, now), Verdict::Accept);
        assert_eq!(r.input(OWNER, 0, &state, now), Verdict::Accept);
        assert_eq!(r.input(OWNER, u32::MAX, &state, now), Verdict::Stale);
    }

    #[test]
    fn input_is_rate_limited_and_refills() {
        let now = Instant::now();
        let mut r = opened(now);
        let state = report(0x42, 53);
        let mut accepted = 0;
        for q in 1..=1000u32 {
            if r.input(OWNER, q, &state, now) == Verdict::Accept {
                accepted += 1;
            }
        }
        assert_eq!(accepted, INPUT_BURST as u32, "a burst, then the limit");
        assert_eq!(
            r.stats().reports_dropped,
            1000 - u64::from(INPUT_BURST as u32)
        );
        // A tenth of a second later, about 150 more fit.
        let later = now + Duration::from_millis(100);
        let more = (1001..2000u32)
            .filter(|q| r.input(OWNER, *q, &state, later) == Verdict::Accept)
            .count();
        assert!((149..=150).contains(&more), "{more}");
    }

    fn feature_set(id: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![id];
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn a_feature_get_is_held_and_answered_with_the_pages_data() {
        let now = Instant::now();
        let mut r = opened(now);
        let (ticket, req) = r.request_get(REPORT_FEATURE, 1, now).unwrap();
        assert_eq!(
            (req.op, req.report, req.len, req.owner, req.ticket),
            (HidOp::GetFeature, 1, 63, OWNER, ticket)
        );
        assert_eq!(r.in_flight(), 1);
        let later = now + Duration::from_millis(12);
        let answers = r.reply(OWNER, ticket, true, &[0x83, 4, 1, 2], None, later);
        assert_eq!(answers.len(), 1);
        let (held, answer) = &answers[0];
        assert_eq!(
            held.req,
            Request::Get {
                kind: REPORT_FEATURE,
                id: 1
            }
        );
        // The id first, then the page's data padded to the report's 63 bytes.
        let Answer::Relayed(data) = answer else {
            panic!("{answer:?}")
        };
        assert_eq!((data.len(), data[0], data[1], data[4]), (64, 1, 0x83, 2));
        assert_eq!(data[5], 0);
        assert_eq!(r.in_flight(), 0);
        let stats = r.stats();
        assert_eq!((stats.requests, stats.replies, stats.timeouts), (1, 1, 0));
        assert_eq!(
            (stats.rtt_p50_us, stats.rtt_max_us),
            (Some(12_000), Some(12_000))
        );
    }

    #[test]
    fn requests_the_device_cannot_have_are_left_to_the_emulation() {
        let now = Instant::now();
        let mut r = opened(now);
        // Not a feature report of the device, or an input one.
        assert!(r.request_get(REPORT_FEATURE, 9, now).is_none());
        assert!(r.request_get(2, 0x80, now).is_none());
        assert!(r.request_get(1, 1, now).is_none());
        // Set: the id must lead and the data fit.
        assert!(
            r.request_set(REPORT_FEATURE, 1, &feature_set(2, &[1]), now)
                .is_none()
        );
        assert!(r.request_set(REPORT_FEATURE, 1, &[], now).is_none());
        assert!(
            r.request_set(REPORT_FEATURE, 1, &feature_set(1, &[0; 64]), now)
                .is_none()
        );
        assert!(
            r.request_set(REPORT_FEATURE, 7, &feature_set(7, &[1]), now)
                .is_none()
        );
        assert_eq!(r.in_flight(), 0);
        assert_eq!(r.stats().requests, 0);
        // A closed relay relays nothing.
        let mut closed = relay(now);
        assert!(closed.request_get(REPORT_FEATURE, 1, now).is_none());
        assert!(closed.request_output(&report(0x80, 9), now).is_none());
    }

    #[test]
    fn a_feature_set_goes_to_the_page_without_its_id_and_padded() {
        let now = Instant::now();
        let mut r = opened(now);
        // Steam writes 64 bytes: the id and 63.
        let mut data = vec![1u8, 0x87, 3, 9, 0, 0];
        data.resize(64, 0);
        let (ticket, req) = r.request_set(REPORT_FEATURE, 1, &data, now).unwrap();
        assert_eq!((req.op, req.report, req.len), (HidOp::SetFeature, 1, 63));
        assert_eq!(&req.data()[..5], &[0x87, 3, 9, 0, 0]);
        // A short write is padded to the report's size.
        let (_, short) = r
            .request_set(REPORT_FEATURE, 1, &feature_set(1, &[0x8e]), now)
            .unwrap();
        assert_eq!((short.len, short.data()[0], short.data()[1]), (63, 0x8e, 0));
        // The ack is relayed as an empty answer; the original data is kept
        // for the emulated device.
        let answers = r.reply(OWNER, ticket, true, &[], None, now);
        assert_eq!(answers[0].1, Answer::Relayed(Vec::new()));
        assert_eq!(
            answers[0].0.req,
            Request::Set {
                kind: REPORT_FEATURE,
                id: 1,
                data
            }
        );
        // An output report by SET_REPORT is its own op.
        let (_, out) = r
            .request_set(REPORT_OUTPUT, 0x80, &feature_set(0x80, &[0; 9]), now)
            .unwrap();
        assert_eq!((out.op, out.len), (HidOp::SetOutput, 9));
    }

    #[test]
    fn interrupt_out_is_sent_without_a_ticket() {
        let now = Instant::now();
        let mut r = opened(now);
        let mut rumble = vec![0x80u8, 0, 1, 0, 0xff, 0xff];
        rumble.resize(10, 0);
        let req = r.request_output(&rumble, now).unwrap();
        assert_eq!(
            (req.op, req.ticket, req.report, req.len),
            (HidOp::Output, 0, 0x80, 9)
        );
        assert_eq!(req.data()[3], 0xff);
        assert_eq!(r.in_flight(), 0, "nothing waits for it");
        // A longer write than the report is not one of the device's.
        assert!(r.request_output(&report(0x80, 20), now).is_none());
        // An unknown output report is left alone.
        assert!(r.request_output(&report(0x99, 3), now).is_none());
        assert_eq!(r.stats().outputs, 1);
    }

    #[test]
    fn requests_are_limited_in_number_and_rate() {
        let now = Instant::now();
        let mut r = opened(now);
        // Sixteen gets wait; the seventeenth goes to the emulation.
        for _ in 0..MAX_IN_FLIGHT {
            assert!(r.request_get(REPORT_FEATURE, 1, now).is_some());
        }
        assert!(r.request_get(REPORT_FEATURE, 1, now).is_none());
        assert_eq!(r.stats().limited, 1);
        // Outputs wait for nothing, but are bound by the rate: the burst has
        // 64 tokens, 16 spent.
        let mut r = opened(now);
        let sent = (0..200)
            .filter(|_| r.request_output(&report(0x80, 9), now).is_some())
            .count();
        assert_eq!(sent, REQUEST_BURST as usize);
        assert_eq!(r.stats().limited, 200 - REQUEST_BURST as u64);
        // And the rate refills: a tenth of a second is about 40.
        let later = now + Duration::from_millis(100);
        let more = (0..200)
            .filter(|_| r.request_output(&report(0x80, 9), later).is_some())
            .count();
        assert!((39..=40).contains(&more), "{more}");
    }

    #[test]
    fn a_silent_page_times_out_and_three_in_a_row_end_the_relay() {
        let now = Instant::now();
        let mut r = opened(now);
        let t1 = r.request_get(REPORT_FEATURE, 1, now).unwrap().0;
        let t2 = r
            .request_set(REPORT_FEATURE, 1, &feature_set(1, &[0x83]), now)
            .unwrap()
            .0;
        assert_eq!(r.next_deadline(), Some(now + REQUEST_TIMEOUT));
        // Not yet.
        assert!(
            r.expire(now + REQUEST_TIMEOUT - Duration::from_millis(1))
                .answers
                .is_empty()
        );
        let due = r.expire(now + REQUEST_TIMEOUT).answers;
        assert_eq!(
            due.iter()
                .map(|(f, a)| (f.ticket, a.clone()))
                .collect::<Vec<_>>(),
            [(t1, Answer::Emulate), (t2, Answer::Emulate)]
        );
        assert_eq!(r.stats().timeouts, 2);
        assert!(r.is_active(), "two in a row are not enough");
        // The third ends it, and takes the others waiting with it.
        let at = now + 2 * REQUEST_TIMEOUT;
        let t3 = r.request_get(REPORT_FEATURE, 1, at).unwrap().0;
        let t4 = r
            .request_get(REPORT_FEATURE, 1, at + Duration::from_millis(500))
            .unwrap()
            .0;
        let due = r.expire(at + REQUEST_TIMEOUT).answers;
        assert_eq!(
            due.iter().map(|(f, _)| f.ticket).collect::<Vec<_>>(),
            [t3, t4]
        );
        assert!(!r.is_active());
        assert_eq!(r.in_flight(), 0);
    }

    #[test]
    fn a_reply_resets_the_failure_count_and_a_late_one_is_ignored() {
        let now = Instant::now();
        let mut r = opened(now);
        // Two timeouts, then a success, then two more: still relaying.
        for round in 0..2u32 {
            let at = now + 2 * round * REQUEST_TIMEOUT;
            r.request_get(REPORT_FEATURE, 1, at).unwrap();
            assert_eq!(r.expire(at + REQUEST_TIMEOUT).answers.len(), 1);
        }
        let at = now + 10 * REQUEST_TIMEOUT;
        let (ticket, _) = r.request_get(REPORT_FEATURE, 1, at).unwrap();
        assert_eq!(r.reply(OWNER, ticket, true, &[1], None, at).len(), 1);
        for round in 0..2u32 {
            let at = now + (20 + 2 * round) * REQUEST_TIMEOUT;
            r.request_get(REPORT_FEATURE, 1, at).unwrap();
            r.expire(at + REQUEST_TIMEOUT);
        }
        assert!(r.is_active());
        // A reply to a request that timed out, or one nobody made, or from
        // another session: nothing.
        assert!(r.reply(OWNER, ticket, true, &[1], None, at).is_empty());
        assert!(r.reply(OWNER, 999, true, &[1], None, at).is_empty());
        let (live, _) = r.request_get(REPORT_FEATURE, 1, at).unwrap();
        assert!(r.reply(99, live, true, &[1], None, at).is_empty());
        assert_eq!(r.in_flight(), 1);
    }

    #[test]
    fn a_failure_from_the_page_is_emulated_and_counted() {
        let now = Instant::now();
        let mut r = opened(now);
        let (ticket, _) = r.request_get(REPORT_FEATURE, 1, now).unwrap();
        let answers = r.reply(OWNER, ticket, false, &[], None, now);
        assert_eq!(answers[0].1, Answer::Emulate);
        assert_eq!(r.stats().errors, 1);
        // Data longer than the report is a failure too.
        let (ticket, _) = r.request_get(REPORT_FEATURE, 1, now).unwrap();
        let answers = r.reply(OWNER, ticket, true, &[0; 64], None, now);
        assert_eq!(answers[0].1, Answer::Emulate);
        assert_eq!(r.stats().errors, 2);
    }

    #[test]
    fn closing_and_taking_over_hand_back_what_waits() {
        let now = Instant::now();
        let mut r = opened(now);
        let (t, _) = r.request_get(REPORT_FEATURE, 1, now).unwrap();
        // The same session opening again keeps its requests.
        assert!(r.open(OWNER, now).is_empty());
        assert_eq!(r.in_flight(), 1);
        // Another session takes over: the first one's requests are returned.
        let dropped = r.open(9, now);
        assert_eq!(dropped.iter().map(|f| f.ticket).collect::<Vec<_>>(), [t]);
        assert_eq!(r.owner(), Some(9));
        assert_eq!(
            r.input(OWNER, 1, &report(0x42, 53), now),
            Verdict::NotRelaying
        );
        // And closing returns the new owner's.
        let (t2, _) = r.request_get(REPORT_FEATURE, 1, now).unwrap();
        let left = r.close(None, "test");
        assert_eq!(left.iter().map(|f| f.ticket).collect::<Vec<_>>(), [t2]);
        assert!(r.close(None, "again").is_empty());
    }

    #[test]
    fn the_kernel_giving_up_forgets_the_request() {
        let now = Instant::now();
        let mut r = opened(now);
        let (t, _) = r.request_get(REPORT_FEATURE, 1, now).unwrap();
        r.cancel(t);
        assert_eq!(r.in_flight(), 0);
        assert!(r.next_deadline().is_none());
        assert!(r.reply(OWNER, t, true, &[1], None, now).is_empty());
    }

    #[test]
    fn round_trip_statistics_are_a_median_and_a_maximum() {
        let now = Instant::now();
        let mut r = opened(now);
        for ms in [5u64, 9, 7, 40, 6] {
            let (t, _) = r.request_get(REPORT_FEATURE, 1, now).unwrap();
            r.reply(OWNER, t, true, &[], None, now + Duration::from_millis(ms));
        }
        let s = r.stats();
        assert_eq!(s.replies, 5);
        assert_eq!((s.rtt_p50_us, s.rtt_max_us), (Some(7_000), Some(40_000)));
        let mut total = RelayStats::default();
        total.merge(&s);
        total.merge(&s);
        assert_eq!((total.replies, total.rtt_max_us), (10, Some(40_000)));
    }

    /// Writes command `cmd` to the command channel and returns the ticket of a get that follows.
    fn command_then_get(r: &mut Relay, cmd: u8, now: Instant) -> u32 {
        let (set, _) = r
            .request_set(REPORT_FEATURE, 1, &feature_set(1, &[cmd, 0]), now)
            .unwrap();
        assert_eq!(r.reply(OWNER, set, true, &[], None, now).len(), 1);
        r.request_get(REPORT_FEATURE, 1, now).unwrap().0
    }

    #[test]
    fn a_reply_that_starts_with_the_command_is_relayed() {
        let now = Instant::now();
        let mut r = opened(now);
        let ticket = command_then_get(&mut r, 0x83, now);
        let answers = r.reply(OWNER, ticket, true, &[0x83, 4, 1, 2], None, now);
        let Answer::Relayed(data) = &answers[0].1 else {
            panic!("{answers:?}")
        };
        assert_eq!(&data[..5], &[1, 0x83, 4, 1, 2]);
        assert_eq!(r.stats().reply_mismatches, 0);
    }

    #[test]
    fn a_reply_with_the_report_id_left_in_is_repaired() {
        let now = Instant::now();
        let mut r = opened(now);
        let ticket = command_then_get(&mut r, 0x83, now);
        let answers = r.reply(OWNER, ticket, true, &[1, 0x83, 4, 1, 2], None, now);
        let Answer::Relayed(data) = &answers[0].1 else {
            panic!("{answers:?}")
        };
        assert_eq!((data.len(), &data[..5]), (64, &[1, 0x83, 4, 1, 2][..]));
        assert_eq!(r.stats().reply_repaired, 1);
    }

    #[test]
    fn a_reply_to_something_else_is_asked_for_again_then_the_emulation_answers() {
        let now = Instant::now();
        let mut r = opened(now);
        let ticket = command_then_get(&mut r, 0x83, now);
        // Zeros, as a firmware that isn't ready might give. Nothing answers yet.
        assert!(r.reply(OWNER, ticket, true, &[0; 63], None, now).is_empty());
        assert_eq!(r.in_flight(), 1);
        // Not before the retry time; then the same get goes out again, same ticket.
        assert!(r.expire(now + Duration::from_millis(2)).resend.is_empty());
        assert_eq!(r.next_deadline(), Some(now + REPLY_RETRY_AFTER));
        let due = r.expire(now + REPLY_RETRY_AFTER);
        assert_eq!(due.resend.len(), 1);
        let again = &due.resend[0];
        assert_eq!(
            (again.op, again.ticket, again.report),
            (HidOp::GetFeature, ticket, 1)
        );
        // Once only per retry.
        assert!(r.expire(now + REPLY_RETRY_AFTER).resend.is_empty());
        // The second try is right: relayed.
        let later = now + Duration::from_millis(7);
        let answers = r.reply(OWNER, ticket, true, &[0x83, 9], None, later);
        assert!(matches!(&answers[0].1, Answer::Relayed(d) if d[1] == 0x83));
        assert_eq!(r.stats().reply_retries, 1);

        // Wrong every time: three tries in all, then the emulated device
        // answers, and no strike is held against the relay.
        for round in 0..5u32 {
            let at = now + Duration::from_millis(100 * u64::from(round + 1));
            let ticket = command_then_get(&mut r, 0xae, at);
            assert!(r.reply(OWNER, ticket, true, &[0; 63], None, at).is_empty());
            let at = at + REPLY_RETRY_AFTER;
            assert_eq!(r.expire(at).resend.len(), 1);
            assert!(r.reply(OWNER, ticket, true, &[0; 63], None, at).is_empty());
            let at = at + REPLY_RETRY_AFTER;
            assert_eq!(r.expire(at).resend.len(), 1);
            let answers = r.reply(OWNER, ticket, true, &[0; 63], None, at);
            assert_eq!(answers.len(), 1);
            assert_eq!(answers[0].1, Answer::Emulate);
            assert_eq!(r.in_flight(), 0);
        }
        assert!(r.is_active(), "wrong content is not a failing page");
        assert_eq!(r.stats().errors, 0);
        assert_eq!(
            r.stats().reply_mismatches,
            16,
            "one from the first part, three for each of five"
        );
    }

    #[test]
    fn only_replies_to_commands_with_replies_are_checked() {
        let now = Instant::now();
        let mut r = opened(now);
        // A settings write (0x87) has nothing to echo; a get with no command before it neither.
        let ticket = command_then_get(&mut r, 0x87, now);
        assert!(matches!(
            r.reply(OWNER, ticket, true, &[0; 63], None, now)[0].1,
            Answer::Relayed(_)
        ));
        let mut fresh = opened(now);
        let (ticket, _) = fresh.request_get(REPORT_FEATURE, 1, now).unwrap();
        assert!(matches!(
            fresh.reply(OWNER, ticket, true, &[5; 10], None, now)[0].1,
            Answer::Relayed(_)
        ));
        // Another feature report is not the command channel.
        let (ticket, _) = fresh.request_get(REPORT_FEATURE, 2, now).unwrap();
        assert!(matches!(
            fresh.reply(OWNER, ticket, true, &[5; 10], None, now)[0].1,
            Answer::Relayed(_)
        ));
    }

    #[test]
    fn a_command_written_again_and_again_gets_the_emulated_replies_until_the_app_moves_on() {
        let now = Instant::now();
        let mut r = opened(now);
        let write = |r: &mut Relay, cmd: u8, at: Instant| {
            let (t, _) = r
                .request_set(REPORT_FEATURE, 1, &feature_set(1, &[cmd, 0]), at)
                .unwrap();
            r.reply(OWNER, t, true, &[], None, at);
        };
        let mut relayed = 0;
        for i in 0..LOOP_AFTER + 10 {
            let at = now + Duration::from_millis(20 * u64::from(i));
            write(&mut r, 0x83, at);
            // A good reply each time: the app wants it again anyway.
            if let Some((t, _)) = r.request_get(REPORT_FEATURE, 1, at) {
                relayed += 1;
                r.reply(OWNER, t, true, &[0x83, 1], None, at);
            }
        }
        // The thirtieth write and everything after it is answered by the emulation.
        assert_eq!(relayed, LOOP_AFTER - 1);
        assert_eq!(r.stats().loops_broken, 11);
        // Another command is its own, relayed again.
        let at = now + Duration::from_secs(1);
        write(&mut r, 0xae, at);
        assert!(r.request_get(REPORT_FEATURE, 1, at).is_some());
        // The same command a few seconds later is not a loop.
        let mut r = opened(now);
        for i in 0..LOOP_AFTER + 5 {
            let at = now + Duration::from_millis(500 * u64::from(i));
            write(&mut r, 0x89, at);
            let (t, _) = r.request_get(REPORT_FEATURE, 1, at).unwrap();
            r.reply(OWNER, t, true, &[0x89, 0], None, at);
        }
        assert_eq!(r.stats().loops_broken, 0);
    }

    #[test]
    fn commands_are_counted_for_the_log() {
        let now = Instant::now();
        let mut r = opened(now);
        for cmd in [0x83, 0x83, 0xae, 0x87, 0x83] {
            let (t, _) = r
                .request_set(REPORT_FEATURE, 1, &feature_set(1, &[cmd, 0]), now)
                .unwrap();
            r.reply(OWNER, t, true, &[], None, now);
        }
        assert_eq!(r.commands(), "0x83:3 0x87:1 0xae:1");
    }

    #[test]
    fn only_allowlisted_commands_reach_the_controller() {
        let now = Instant::now();
        let mut r = opened(now);
        let mut passed = Vec::new();
        for cmd in 0..=255u8 {
            if let Some((ticket, req)) =
                r.request_set(REPORT_FEATURE, 1, &feature_set(1, &[cmd, 0]), now)
            {
                assert_eq!(req.data[0], cmd);
                passed.push(cmd);
                r.reply(OWNER, ticket, true, &[], None, now);
            }
        }
        assert_eq!(passed, RELAYED_COMMANDS);
        for never in [
            0x90, 0x80, 0x86, 0x88, 0x8d, 0x9f, 0xa9, 0xad, 0xaf, 0xb0, 0xb5, 0xb7, 0xb8, 0xb9,
            0xc1, 0xdc, 0xe2, 0xf2,
        ] {
            assert!(!passed.contains(&never), "{never:#04x}");
        }
        assert_eq!(r.stats().blocked, 256 - RELAYED_COMMANDS.len() as u64);
    }

    #[test]
    fn a_blocked_command_is_answered_by_the_emulation_and_so_is_its_reply() {
        let now = Instant::now();
        let mut r = opened(now);
        assert!(
            r.request_set(REPORT_FEATURE, 1, &feature_set(1, &[0x90, 0]), now)
                .is_none()
        );
        // The reply to the command is not the controller's stale one.
        assert!(r.request_get(REPORT_FEATURE, 1, now).is_none());
        assert_eq!(r.in_flight(), 0);
        // The next allowed command is relayed again, and so is its reply.
        let (set, _) = r
            .request_set(REPORT_FEATURE, 1, &feature_set(1, &[0x83, 0]), now)
            .unwrap();
        r.reply(OWNER, set, true, &[], None, now);
        assert!(r.request_get(REPORT_FEATURE, 1, now).is_some());
    }

    #[test]
    fn other_feature_reports_are_never_written_to_the_controller() {
        let now = Instant::now();
        let mut r = opened(now);
        let sizes = ReportSizes::parse(DESCRIPTOR_TRITON_USB);
        for id in 0..=255u8 {
            if id != 1 && sizes.feature(id).is_some() {
                assert!(
                    r.request_set(REPORT_FEATURE, id, &feature_set(id, &[0x83, 0]), now)
                        .is_none(),
                    "feature {id}"
                );
            }
        }
    }

    #[test]
    fn only_haptic_and_rumble_outputs_reach_the_controller() {
        let now = Instant::now();
        let mut r = opened(now);
        let sizes = ReportSizes::parse(DESCRIPTOR_TRITON_USB);
        let mut seen = 0;
        for id in 0..=255u8 {
            let Some(size) = sizes.output(id) else {
                continue;
            };
            seen += 1;
            let mut data = vec![id];
            data.resize(1 + size, 0);
            assert_eq!(
                r.request_output(&data, now).is_some(),
                output_relayed(id),
                "interrupt out {id:#04x}"
            );
            assert_eq!(
                r.request_set(REPORT_OUTPUT, id, &data, now).is_some(),
                output_relayed(id),
                "set output {id:#04x}"
            );
        }
        assert!(seen > 0);
        assert!(!output_relayed(0x86) && output_relayed(0x80) && output_relayed(0x85));
    }

    /// A 0x83 reply: product, revision, the firmware's and bootloader's build
    /// times, the hardware id.
    fn attributes() -> Vec<u8> {
        let mut v = vec![0x83, 25];
        for (tag, value) in [
            (1u8, 0x1302u32),
            (10, 0x68d2_f92e),
            (4, 0x6aa4_3b55),
            (9, 0x48),
            (5, 0x6000_0000),
        ] {
            v.push(tag);
            v.extend_from_slice(&value.to_le_bytes());
        }
        v
    }

    #[test]
    fn the_firmware_build_times_in_the_attributes_are_newer_than_any_update() {
        let now = Instant::now();
        let mut r = opened(now);
        let ticket = command_then_get(&mut r, 0x83, now);
        let answers = r.reply(OWNER, ticket, true, &attributes(), None, now);
        let Answer::Relayed(out) = &answers[0].1 else {
            panic!("{answers:?}")
        };
        assert_eq!(out[0], 1);
        let want = NEVER_UPDATE_TIME.to_le_bytes();
        assert_eq!(&out[1..3], &[0x83, 25]);
        let attrs: Vec<(u8, &[u8])> = out[3..28].chunks(5).map(|a| (a[0], &a[1..])).collect();
        assert_eq!(attrs[0], (1, &0x1302u32.to_le_bytes()[..]));
        assert_eq!(attrs[1], (10, &want[..]));
        assert_eq!(attrs[2], (4, &want[..]));
        assert_eq!(attrs[3], (9, &0x48u32.to_le_bytes()[..]));
        assert_eq!(attrs[4], (5, &want[..]));
    }

    #[test]
    fn rewriting_build_times_leaves_other_replies_and_short_ones_alone() {
        let mut other = vec![0x89, 5, 4, 1, 2, 3, 4];
        assert!(!rewrite_build_times(&mut other));
        assert_eq!(other, [0x89, 5, 4, 1, 2, 3, 4]);
        // A length that runs past the data, and a partial attribute at the end.
        let mut short = vec![0x83, 200, 4, 1, 2, 3, 4, 10, 9, 9];
        assert!(rewrite_build_times(&mut short));
        assert_eq!(&short[3..7], &NEVER_UPDATE_TIME.to_le_bytes());
        assert_eq!(&short[7..], &[10, 9, 9]);
        assert!(!rewrite_build_times(&mut []));
        assert!(!rewrite_build_times(&mut [0x83]));
    }
}
