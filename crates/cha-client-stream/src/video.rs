//! Video reassembly for the hardware codecs: a port of the video half of the
//! browser's `web/packages/player/src/wt-worker.ts` (our own AGPL code), with
//! its FEC recovery (`fec.ts` is `cha_proto::fec`), as Sans-IO state: time is
//! handed in, requests and frames come out.
//!
//! `cha_proto::Reassembler` does not do this: it has no parity fragments, no
//! in-order delivery gated on a keyframe, and no reference invalidation.
//!
//! - **Reassembly.** Fragments of a frame (`frame_id`) are collected; when
//!   every data fragment is present, or the missing ones are rebuilt from the
//!   parity fragments (`cha_proto::fec::recover` per block of up to 128), the
//!   frame is complete. A parity fragment's payload is the frame's length
//!   (u32 LE) then the parity shard, so a rebuilt (zero-padded) frame is
//!   trimmed to that length.
//! - **Order.** Frames go out by id from a start frame: a keyframe, or after
//!   loss a frame flagged RECOVERY whose id is not before the first lost
//!   frame. Until one completes, later frames are held (at most 240; over
//!   that, the newest 120 are kept).
//! - **Loss.** The frame the decoder needs next is lost when a later complete
//!   frame has waited 15 ms for it (it was overtaken), or when its fragments
//!   went quiet for 250 ms. Then everything from it on is dropped and the
//!   streamer is asked to refer around it (`rfi`, once), then for a keyframe
//!   every 250 ms until a start frame arrives. A recovery frame that is
//!   itself lost is asked for again at once.
//! - **Streams.** The header's `stream` byte counts codec switches. A newer
//!   one drops everything of the old and starts from its first keyframe;
//!   stragglers of an older one are ignored.
//! - **Loss accounting.** Every frame's datagrams are counted from its first
//!   one; 200 ms later it "settles" and what never came counts as missing,
//!   for the `l` of the rate-control report.
//!
//! PyroWave (flagged INTRA) frames are not handled (C2.3): their datagrams
//! are counted and dropped.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use bytes::Bytes;
use cha_proto::fec::{self, BLOCK};
use cha_proto::{DatagramHeader, Flags};

/// No datagram of the frame the decoder needs next for this long: it is lost.
pub const SILENCE: Duration = Duration::from_millis(250);
/// How long a complete frame waits for an earlier one still arriving.
pub const ORDER_WAIT: Duration = Duration::from_millis(15);
/// Keyframe (and first, rfi) requests are at least this far apart.
pub const KEYFRAME_RETRY: Duration = Duration::from_millis(250);
/// How long a frame's datagrams may straggle before the missing count as lost.
pub const REORDER: Duration = Duration::from_millis(200);
/// Complete frames kept while waiting for a start frame.
pub const MAX_WAITING: usize = 240;
/// While waiting for a start frame, a frame silent this long is forgotten.
const ABANDONED: Duration = Duration::from_secs(2);
/// Settled frames count towards the loss report for this long.
const LOSS_WINDOW: Duration = Duration::from_secs(1);

/// `a` comes before `b`, with u32 wraparound.
pub fn before(a: u32, b: u32) -> bool {
    a != b && b.wrapping_sub(a) < 0x8000_0000
}

/// Stream `b` came after stream `a` (8 bits, wrapping).
pub fn newer_stream(a: u8, b: u8) -> bool {
    let d = b.wrapping_sub(a);
    d > 0 && d < 0x80
}

/// A complete frame, in order, ready for the decoder.
#[derive(Clone, Debug)]
pub struct Frame {
    pub stream: u8,
    pub id: u32,
    /// A keyframe. A frame flagged RECOVERY that starts a run is not one: it
    /// is a P-frame the decoder takes as it is.
    pub key: bool,
    pub recovery: bool,
    pub send_ts: u32,
    pub first_at: Instant,
    pub last_at: Instant,
    pub data: Bytes,
}

/// What the receiver asks the streamer for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Reference invalidation: refer around the frame with this id.
    Rfi(u32),
    Keyframe,
}

/// What a call produced; drained by the caller.
#[derive(Debug, Default)]
pub struct Output {
    pub frames: Vec<Frame>,
    pub asks: Vec<Ask>,
    /// Frames given up on.
    pub lost: u32,
    /// Frames rebuilt from parity.
    pub recovered: u32,
    /// The `send_ts` of each frame that completed (for the delay report).
    pub completed: Vec<u32>,
}

struct Partial {
    total: u16,
    received: u16,
    parts: Vec<Option<Vec<u8>>>,
    /// Parity fragments per block, those that came, a data shard's length
    /// and the frame's.
    fec: u8,
    parity: Vec<Option<Vec<u8>>>,
    shard_len: usize,
    frame_len: usize,
    bytes: usize,
    key: bool,
    /// Refers only to frames before the one reported lost.
    recovery: bool,
    send_ts: u32,
    first_at: Instant,
    last_at: Instant,
}

struct Complete {
    key: bool,
    recovery: bool,
    send_ts: u32,
    first_at: Instant,
    last_at: Instant,
    data: Bytes,
}

struct Tally {
    at: Instant,
    expected: u32,
    got: u32,
}

/// The video side of a session's receive logic.
pub struct VideoRx {
    partials: HashMap<u32, Partial>,
    complete: HashMap<u32, Complete>,
    /// The next frame id the decoder may take.
    next: Option<u32>,
    need_key: bool,
    /// The first frame lost since the decoder's last, and whether the
    /// streamer was asked to refer around it.
    lost_from: Option<u32>,
    rfi_asked: bool,
    asked_at: Option<Instant>,
    stream: Option<u8>,
    /// Each frame's datagrams, expected and come, until it settles (in the
    /// order their first datagrams came).
    tally: HashMap<u32, Tally>,
    tally_order: VecDeque<u32>,
    /// Settled frames' datagrams and how many never came.
    settled: VecDeque<(Instant, u32, u32)>,
    intra_seen: u64,
    out: Output,
}

impl Default for VideoRx {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoRx {
    pub fn new() -> Self {
        Self {
            partials: HashMap::new(),
            complete: HashMap::new(),
            next: None,
            need_key: true,
            lost_from: None,
            rfi_asked: false,
            asked_at: None,
            stream: None,
            tally: HashMap::new(),
            tally_order: VecDeque::new(),
            settled: VecDeque::new(),
            intra_seen: 0,
            out: Output::default(),
        }
    }

    /// Datagrams of PyroWave frames seen and dropped.
    pub fn intra_dropped(&self) -> u64 {
        self.intra_seen
    }

    /// What the calls so far produced.
    pub fn take(&mut self) -> Output {
        std::mem::take(&mut self.out)
    }

    /// A video datagram (`header` decoded, `payload` after it) arrived.
    pub fn push(&mut self, now: Instant, h: &DatagramHeader, payload: &[u8]) {
        if self.stream != Some(h.stream) {
            // A straggler from before a switch, or the first of a new stream.
            if let Some(current) = self.stream
                && !newer_stream(current, h.stream)
            {
                return;
            }
            self.start_stream(h.stream);
        }
        if h.flags.has(Flags::INTRA) {
            self.intra_seen += 1;
            return;
        }
        let id = h.frame_id;
        let total = h.frag_count;
        let is_parity = h.flags.has(Flags::PARITY) && h.fec > 0;
        self.count(now, id, total, h.fec);
        if self.complete.contains_key(&id) {
            return;
        }
        if self.next.is_some_and(|next| before(id, next)) {
            return; // already given up on
        }
        let f = self.partials.entry(id).or_insert_with(|| Partial {
            total,
            received: 0,
            parts: vec![None; usize::from(total)],
            fec: h.fec,
            parity: vec![None; usize::from(h.fec) * usize::from(total).div_ceil(BLOCK)],
            shard_len: 0,
            frame_len: 0,
            bytes: 0,
            key: h.flags.has(Flags::KEYFRAME),
            recovery: h.flags.has(Flags::RECOVERY),
            send_ts: h.send_ts_us,
            first_at: now,
            last_at: now,
        });
        if f.total != total || f.fec != h.fec {
            return; // disagrees with the frame's first datagram
        }
        f.last_at = now;
        let done_rebuilt = if is_parity {
            let Some(p) = usize::from(h.frag_index).checked_sub(usize::from(total)) else {
                return;
            };
            if p >= f.parity.len() || f.parity[p].is_some() || payload.len() < 5 {
                return;
            }
            f.frame_len =
                u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
            f.parity[p] = Some(payload[4..].to_vec());
            f.shard_len = payload.len() - 4;
            if !rebuild(f) {
                return;
            }
            true
        } else {
            let index = usize::from(h.frag_index);
            if f.parts[index].is_some() {
                return;
            }
            f.parts[index] = Some(payload.to_vec());
            f.bytes += payload.len();
            f.received += 1;
            if f.received < f.total {
                if f.fec == 0 || !rebuild(f) {
                    return;
                }
                true
            } else {
                false
            }
        };
        self.complete_frame(now, id, done_rebuilt);
    }

    /// Run about every 10 ms: give up on frames that won't complete, or that
    /// later ones overtook, and deliver what can go.
    pub fn tick(&mut self, now: Instant) {
        if self.next.is_none() || self.need_key {
            self.deliver(now);
            if self.need_key && self.lost_from.is_some() {
                self.retry_lost_recovery(now);
            }
            // Frames that went quiet while we wait for a start frame are
            // never coming; don't keep their pieces.
            self.partials
                .retain(|_, p| now.saturating_duration_since(p.last_at) < ABANDONED);
            return;
        }
        let Some(missing) = self.next else { return };
        let partial = self.partials.get(&missing);
        let overtaken = self
            .complete
            .values()
            .any(|f| now.saturating_duration_since(f.last_at) > ORDER_WAIT);
        // Still arriving (a large keyframe takes a while on a slow link) and
        // not overtaken: wait.
        let stale = overtaken
            || partial.is_some_and(|p| now.saturating_duration_since(p.last_at) > SILENCE);
        if !stale {
            return;
        }
        // The frame the decoder needs next is gone: so is everything until
        // one that refers around it.
        let mut lost = 0;
        self.partials.retain(|&id, _| {
            let keep = before(id, missing);
            if !keep {
                lost += 1;
            }
            keep
        });
        lost += self.complete.len() as u32;
        self.complete.clear();
        self.out.lost += lost.max(1);
        self.need_key = true;
        if self.lost_from.is_none() {
            self.lost_from = Some(missing);
            self.rfi_asked = false;
            self.asked_at = None;
        }
        self.ask_to_resync(now);
    }

    /// The consumer fell behind and frames were dropped after this one:
    /// start over from the next keyframe.
    pub fn resync(&mut self, now: Instant) {
        self.complete.clear();
        self.partials.clear();
        self.need_key = true;
        self.lost_from = None;
        self.rfi_asked = false;
        self.asked_at = None;
        self.ask_to_resync(now);
    }

    /// Missing over expected datagrams of frames that settled in the last
    /// second; `None` when there were none.
    pub fn loss(&mut self, now: Instant) -> Option<f64> {
        // Frames settle in the order their first datagrams came.
        while let Some(&id) = self.tally_order.front() {
            let Some(c) = self.tally.get(&id) else {
                self.tally_order.pop_front();
                continue;
            };
            if now.saturating_duration_since(c.at) < REORDER {
                break;
            }
            let c = self.tally.remove(&id).expect("just seen");
            self.tally_order.pop_front();
            self.settled
                .push_back((now, c.expected, c.expected.saturating_sub(c.got)));
        }
        while self
            .settled
            .front()
            .is_some_and(|(at, ..)| now.saturating_duration_since(*at) > LOSS_WINDOW)
        {
            self.settled.pop_front();
        }
        let total: u64 = self.settled.iter().map(|s| u64::from(s.1)).sum();
        let missing: u64 = self.settled.iter().map(|s| u64::from(s.2)).sum();
        (total > 0).then(|| missing as f64 / total as f64)
    }

    /// A datagram of frame `id` (`total` data fragments, `fec` parity per
    /// block) came.
    fn count(&mut self, now: Instant, id: u32, total: u16, fec: u8) {
        let expected = u32::from(total) + u32::from(fec) * u32::from(total).div_ceil(BLOCK as u32);
        let c = self.tally.entry(id).or_insert_with(|| {
            self.tally_order.push_back(id);
            Tally {
                at: now,
                expected,
                got: 0,
            }
        });
        c.got += 1;
    }

    fn start_stream(&mut self, stream: u8) {
        self.stream = Some(stream);
        self.partials.clear();
        self.complete.clear();
        self.tally.clear();
        self.tally_order.clear();
        self.next = None;
        self.need_key = true;
        self.asked_at = None;
        self.lost_from = None;
        self.rfi_asked = false;
    }

    fn complete_frame(&mut self, now: Instant, id: u32, rebuilt: bool) {
        let Some(f) = self.partials.remove(&id) else {
            return;
        };
        if rebuilt {
            self.out.recovered += 1;
        }
        self.out.completed.push(f.send_ts);
        // A rebuilt frame is exactly the length the parity said.
        let size = if rebuilt { f.frame_len } else { f.bytes };
        let mut data = Vec::with_capacity(size.min(1 << 26));
        for part in f.parts.iter().flatten() {
            // A rebuilt last shard is padded past the frame's end.
            let n = part.len().min(size - data.len());
            data.extend_from_slice(&part[..n]);
        }
        self.complete.insert(
            id,
            Complete {
                key: f.key,
                recovery: f.recovery,
                send_ts: f.send_ts,
                first_at: f.first_at,
                last_at: now,
                data: Bytes::from(data),
            },
        );
        self.deliver(now);
    }

    /// Hands over complete frames in id order, from a keyframe (or a recovery
    /// frame) on.
    fn deliver(&mut self, now: Instant) {
        loop {
            if self.next.is_none() || self.need_key {
                // Start (again) from the newest complete keyframe, or frame
                // referring around the lost one.
                let lost_from = self.lost_from;
                let mut start: Option<u32> = None;
                for (&id, f) in &self.complete {
                    let starts =
                        f.key || (f.recovery && lost_from.is_some_and(|lost| !before(id, lost)));
                    if starts && start.is_none_or(|s| before(s, id)) {
                        start = Some(id);
                    }
                }
                let Some(start) = start else {
                    // Frames after a start frame still arriving are needed;
                    // older ones never will be. Keep a bounded window.
                    if self.complete.len() > MAX_WAITING {
                        let mut ids: Vec<u32> = self.complete.keys().copied().collect();
                        ids.sort_by(|&a, &b| {
                            if a == b {
                                std::cmp::Ordering::Equal
                            } else if before(a, b) {
                                std::cmp::Ordering::Less
                            } else {
                                std::cmp::Ordering::Greater
                            }
                        });
                        for id in &ids[..ids.len() - MAX_WAITING / 2] {
                            self.complete.remove(id);
                        }
                    }
                    self.ask_to_resync(now);
                    return;
                };
                self.complete.retain(|&id, _| !before(id, start));
                self.partials.retain(|&id, _| !before(id, start));
                self.next = Some(start);
                self.need_key = false;
                self.lost_from = None;
                self.rfi_asked = false;
            }
            let next = self.next.expect("set above");
            let Some(f) = self.complete.remove(&next) else {
                return;
            };
            self.out.frames.push(Frame {
                stream: self.stream.unwrap_or(0),
                id: next,
                key: f.key,
                recovery: f.recovery,
                send_ts: f.send_ts,
                first_at: f.first_at,
                last_at: f.last_at,
                data: f.data,
            });
            self.next = Some(next.wrapping_add(1));
        }
    }

    /// The recovery frame itself was lost (later frames completed past it, or
    /// it went quiet): ask again now, rather than at the retry.
    fn retry_lost_recovery(&mut self, now: Instant) {
        let lost = self.partials.iter().find_map(|(&id, f)| {
            if !f.recovery {
                return None;
            }
            let overtaken = self.complete.iter().any(|(&later, c)| {
                before(id, later) && now.saturating_duration_since(c.last_at) > ORDER_WAIT
            });
            (overtaken || now.saturating_duration_since(f.last_at) > SILENCE).then_some(id)
        });
        if let Some(id) = lost {
            self.partials.remove(&id);
            self.rfi_asked = false;
            self.asked_at = None;
            self.ask_to_resync(now);
        }
    }

    /// Asks to refer around the lost frame, then (nothing having come of it)
    /// for a keyframe.
    fn ask_to_resync(&mut self, now: Instant) {
        if self
            .asked_at
            .is_some_and(|t| now.saturating_duration_since(t) < KEYFRAME_RETRY)
        {
            return;
        }
        self.asked_at = Some(now);
        match self.lost_from {
            Some(id) if !self.rfi_asked => {
                self.rfi_asked = true;
                self.out.asks.push(Ask::Rfi(id));
            }
            _ => self.out.asks.push(Ask::Keyframe),
        }
    }
}

/// FEC: rebuilds the missing data fragments once every block has as many
/// shards as data; true if the frame is whole now.
fn rebuild(f: &mut Partial) -> bool {
    if f.shard_len == 0 || f.received >= f.total {
        return f.received >= f.total;
    }
    let m = usize::from(f.fec);
    let total = usize::from(f.total);
    let mut blocks = Vec::new();
    for first in (0..total).step_by(BLOCK) {
        let n = BLOCK.min(total - first);
        let b = first / BLOCK;
        let missing = f.parts[first..first + n]
            .iter()
            .filter(|p| p.is_none())
            .count();
        let parity = f.parity[b * m..(b + 1) * m]
            .iter()
            .filter(|p| p.is_some())
            .count();
        if missing > parity {
            return false;
        }
        if missing > 0 {
            blocks.push((first, n, missing));
        }
    }
    for (first, n, missing) in blocks {
        let b = first / BLOCK;
        let (parts, parity) = (
            &mut f.parts[first..first + n],
            &f.parity[b * m..(b + 1) * m],
        );
        if !fec::recover(parts, parity, f.shard_len) {
            return false;
        }
        f.received += missing as u16;
    }
    f.received >= f.total
}

#[cfg(test)]
mod tests {
    use super::*;
    use cha_proto::{Fragmenter, Kind};

    const MAX_DATAGRAM: usize = 220;

    fn t0() -> Instant {
        Instant::now()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The datagrams of one frame, as the streamer sends them.
    fn datagrams(id: u32, flags: u8, stream: u8, len: usize, fec: u8) -> Vec<Vec<u8>> {
        let frame: Vec<u8> = (0..len).map(|i| (i * 7 + id as usize) as u8).collect();
        let mut out = Vec::new();
        let base = DatagramHeader {
            kind: Kind::Video,
            flags: Flags(flags),
            stream,
            fec,
            frame_id: id,
            frag_index: 0,
            frag_count: 1,
            send_ts_us: id.wrapping_mul(1000),
        };
        Fragmenter::new(MAX_DATAGRAM)
            .fragment_fec(base, &frame, fec, |d| out.push(d.to_vec()))
            .unwrap();
        out
    }

    fn expected(id: u32, len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + id as usize) as u8).collect()
    }

    fn feed(rx: &mut VideoRx, now: Instant, dgs: &[Vec<u8>]) {
        for d in dgs {
            let (h, payload) = DatagramHeader::decode(d).unwrap();
            rx.push(now, &h, payload);
        }
    }

    fn ids(out: &Output) -> Vec<u32> {
        out.frames.iter().map(|f| f.id).collect()
    }

    #[test]
    fn a_keyframe_in_pieces_is_one_frame() {
        let mut rx = VideoRx::new();
        let now = t0();
        let mut d = datagrams(0, Flags::KEYFRAME, 0, 1000, 0);
        assert!(d.len() > 3);
        d.reverse(); // out of order
        feed(&mut rx, now, &d);
        let out = rx.take();
        assert_eq!(ids(&out), [0]);
        assert!(out.frames[0].key);
        assert_eq!(out.frames[0].data.as_ref(), expected(0, 1000));
        assert_eq!(out.completed, [0]);
    }

    #[test]
    fn deltas_wait_for_a_keyframe_and_ask_for_one() {
        let mut rx = VideoRx::new();
        let now = t0();
        feed(&mut rx, now, &datagrams(5, 0, 0, 300, 0));
        let out = rx.take();
        assert!(out.frames.is_empty());
        assert_eq!(out.asks, [Ask::Keyframe]);
        // Asking again waits for the retry.
        rx.tick(now + ms(100));
        assert!(rx.take().asks.is_empty());
        rx.tick(now + ms(260));
        assert_eq!(rx.take().asks, [Ask::Keyframe]);
        // The keyframe starts the run, the held delta follows it.
        feed(
            &mut rx,
            now + ms(270),
            &datagrams(4, Flags::KEYFRAME, 0, 300, 0),
        );
        assert_eq!(ids(&rx.take()), [4, 5]);
    }

    #[test]
    fn one_lost_fragment_within_the_parity_is_rebuilt() {
        let mut rx = VideoRx::new();
        let now = t0();
        let mut d = datagrams(0, Flags::KEYFRAME, 0, 1000, 2);
        d.remove(1);
        d.remove(2);
        feed(&mut rx, now, &d);
        let out = rx.take();
        assert_eq!(ids(&out), [0]);
        assert_eq!(out.recovered, 1);
        assert_eq!(out.frames[0].data.as_ref(), expected(0, 1000));
    }

    #[test]
    fn fec_rebuilds_the_last_short_shard_without_padding() {
        let mut rx = VideoRx::new();
        let now = t0();
        // Lose the last data fragment, which is shorter than the shards.
        let mut d = datagrams(0, Flags::KEYFRAME, 0, 950, 1);
        let last = usize::from(DatagramHeader::decode(&d[0]).unwrap().0.frag_count) - 1;
        d.remove(last);
        feed(&mut rx, now, &d);
        let out = rx.take();
        assert_eq!(out.frames[0].data.len(), 950);
        assert_eq!(out.frames[0].data.as_ref(), expected(0, 950));
    }

    #[test]
    fn frames_without_parity_on_a_fec_stream_work() {
        let mut rx = VideoRx::new();
        let now = t0();
        feed(&mut rx, now, &datagrams(0, Flags::KEYFRAME, 0, 600, 2));
        feed(&mut rx, now, &datagrams(1, 0, 0, 600, 0));
        assert_eq!(ids(&rx.take()), [0, 1]);
    }

    #[test]
    fn parity_arriving_first_still_completes() {
        let mut rx = VideoRx::new();
        let now = t0();
        let mut d = datagrams(0, Flags::KEYFRAME, 0, 1000, 2);
        let k = usize::from(DatagramHeader::decode(&d[0]).unwrap().0.frag_count);
        d.remove(0);
        // Parity first, then data.
        let parity: Vec<_> = d.split_off(k - 1);
        feed(&mut rx, now, &parity);
        assert!(rx.take().frames.is_empty());
        feed(&mut rx, now, &d);
        let out = rx.take();
        assert_eq!(out.frames[0].data.as_ref(), expected(0, 1000));
    }

    #[test]
    fn loss_beyond_the_parity_asks_rfi_then_keyframes_until_a_start_frame() {
        let mut rx = VideoRx::new();
        let now = t0();
        feed(&mut rx, now, &datagrams(0, Flags::KEYFRAME, 0, 600, 0));
        feed(&mut rx, now, &datagrams(1, 0, 0, 600, 0));
        assert_eq!(ids(&rx.take()), [0, 1]);
        // Frame 2 loses two of its fragments, with only one parity.
        let mut d2 = datagrams(2, 0, 0, 800, 1);
        d2.remove(0);
        d2.remove(0);
        let t = now + ms(10);
        feed(&mut rx, t, &d2);
        feed(&mut rx, t, &datagrams(3, 0, 0, 600, 0));
        assert!(rx.take().frames.is_empty(), "frame 3 waits for frame 2");
        // Not yet overtaken for long enough.
        rx.tick(t + ms(5));
        assert!(rx.take().asks.is_empty());
        rx.tick(t + ms(20));
        let out = rx.take();
        assert_eq!(out.asks, [Ask::Rfi(2)]);
        assert_eq!(out.lost, 2, "frame 2's partial and frame 3");
        // The recovery frame doesn't come; a keyframe is asked for at 250 ms.
        feed(&mut rx, t + ms(30), &datagrams(4, 0, 0, 600, 0));
        rx.tick(t + ms(40));
        assert!(rx.take().asks.is_empty());
        rx.tick(t + ms(280));
        assert_eq!(rx.take().asks, [Ask::Keyframe]);
        rx.tick(t + ms(540));
        assert_eq!(rx.take().asks, [Ask::Keyframe]);
        // A keyframe resumes, and what is older is dropped.
        feed(
            &mut rx,
            t + ms(550),
            &datagrams(5, Flags::KEYFRAME, 0, 600, 0),
        );
        feed(&mut rx, t + ms(551), &datagrams(6, 0, 0, 600, 0));
        let out = rx.take();
        assert_eq!(ids(&out), [5, 6]);
    }

    #[test]
    fn a_recovery_frame_resumes_without_a_keyframe() {
        let mut rx = VideoRx::new();
        let now = t0();
        feed(&mut rx, now, &datagrams(0, Flags::KEYFRAME, 0, 400, 0));
        let mut d1 = datagrams(1, 0, 0, 800, 0);
        d1.remove(0);
        let t = now + ms(10);
        feed(&mut rx, t, &d1);
        feed(&mut rx, t, &datagrams(2, 0, 0, 400, 0));
        rx.take();
        rx.tick(t + ms(20));
        assert_eq!(rx.take().asks, [Ask::Rfi(1)]);
        // Frames 3, 4 are delta frames that depend on the lost one: held.
        feed(&mut rx, t + ms(25), &datagrams(3, 0, 0, 400, 0));
        // Frame 4 is the recovery frame: not a keyframe, but it starts the run.
        feed(
            &mut rx,
            t + ms(30),
            &datagrams(4, Flags::RECOVERY, 0, 400, 0),
        );
        let out = rx.take();
        assert_eq!(ids(&out), [4]);
        assert!(!out.frames[0].key);
        assert!(out.frames[0].recovery);
        feed(&mut rx, t + ms(40), &datagrams(5, 0, 0, 400, 0));
        assert_eq!(ids(&rx.take()), [5]);
    }

    #[test]
    fn a_recovery_frame_from_before_the_lost_one_does_not_start_a_run() {
        let mut rx = VideoRx::new();
        let now = t0();
        feed(&mut rx, now, &datagrams(10, Flags::KEYFRAME, 0, 400, 0));
        let mut d = datagrams(11, 0, 0, 800, 0);
        d.remove(0);
        let t = now + ms(10);
        feed(&mut rx, t, &d);
        feed(&mut rx, t, &datagrams(12, 0, 0, 400, 0));
        rx.take();
        rx.tick(t + ms(20));
        rx.take();
        // A RECOVERY frame older than the loss (id 9 < 11) is not a start.
        feed(
            &mut rx,
            t + ms(25),
            &datagrams(9, Flags::RECOVERY, 0, 400, 0),
        );
        assert!(rx.take().frames.is_empty());
    }

    #[test]
    fn a_lost_recovery_frame_is_asked_for_again_at_once() {
        let mut rx = VideoRx::new();
        let now = t0();
        feed(&mut rx, now, &datagrams(0, Flags::KEYFRAME, 0, 400, 0));
        let mut d = datagrams(1, 0, 0, 800, 0);
        d.remove(0);
        let t = now + ms(10);
        feed(&mut rx, t, &d);
        feed(&mut rx, t, &datagrams(2, 0, 0, 400, 0));
        rx.take();
        rx.tick(t + ms(20));
        assert_eq!(rx.take().asks, [Ask::Rfi(1)]);
        // The recovery frame (id 3) arrives short of a fragment, and a later
        // frame completes past it.
        let mut rec = datagrams(3, Flags::RECOVERY, 0, 800, 0);
        rec.remove(0);
        feed(&mut rx, t + ms(30), &rec);
        feed(&mut rx, t + ms(35), &datagrams(4, 0, 0, 400, 0));
        rx.tick(t + ms(40));
        assert!(rx.take().asks.is_empty());
        rx.tick(t + ms(60));
        let asks = rx.take().asks;
        assert_eq!(asks, [Ask::Rfi(1)], "asked again, before the 250 ms retry");
    }

    #[test]
    fn a_slow_big_frame_is_waited_for_not_given_up_on() {
        let mut rx = VideoRx::new();
        let now = t0();
        feed(&mut rx, now, &datagrams(0, Flags::KEYFRAME, 0, 400, 0));
        rx.take();
        // Frame 1 arrives piecemeal over 100 ms; nothing completes after it.
        let d = datagrams(1, 0, 0, 2000, 0);
        let (first, rest) = d.split_at(2);
        feed(&mut rx, now + ms(10), first);
        rx.tick(now + ms(60));
        rx.tick(now + ms(100));
        let out = rx.take();
        assert!(out.asks.is_empty() && out.lost == 0);
        feed(&mut rx, now + ms(105), rest);
        assert_eq!(ids(&rx.take()), [1]);
    }

    #[test]
    fn silence_on_the_awaited_frame_is_loss_after_250_ms() {
        let mut rx = VideoRx::new();
        let now = t0();
        feed(&mut rx, now, &datagrams(0, Flags::KEYFRAME, 0, 400, 0));
        rx.take();
        let d = datagrams(1, 0, 0, 2000, 0);
        feed(&mut rx, now + ms(10), &d[..2]);
        rx.tick(now + ms(200));
        assert!(rx.take().asks.is_empty());
        rx.tick(now + ms(270));
        let out = rx.take();
        assert_eq!(out.asks, [Ask::Rfi(1)]);
        assert_eq!(out.lost, 1);
    }

    #[test]
    fn a_newer_stream_restarts_and_old_stragglers_are_ignored() {
        let mut rx = VideoRx::new();
        let now = t0();
        feed(&mut rx, now, &datagrams(0, Flags::KEYFRAME, 0, 300, 0));
        feed(&mut rx, now, &datagrams(1, 0, 0, 300, 0));
        assert_eq!(ids(&rx.take()), [0, 1]);
        // Stream 1 carries on the ids; its delta waits for a keyframe.
        feed(&mut rx, now, &datagrams(2, 0, 1, 300, 0));
        assert!(rx.take().frames.is_empty());
        feed(&mut rx, now, &datagrams(3, Flags::KEYFRAME, 1, 300, 0));
        let out = rx.take();
        assert_eq!(ids(&out), [3]);
        assert_eq!(out.frames[0].stream, 1);
        // A straggler of stream 0 is dropped.
        feed(&mut rx, now, &datagrams(4, Flags::KEYFRAME, 0, 300, 0));
        feed(&mut rx, now, &datagrams(4, 0, 1, 300, 0));
        let out = rx.take();
        assert_eq!(ids(&out), [4]);
        assert_eq!(out.frames[0].stream, 1);
        // The stream byte wraps: 255 -> 0 is newer.
        assert!(newer_stream(255, 0));
        assert!(!newer_stream(0, 255));
        assert!(!newer_stream(7, 7));
    }

    #[test]
    fn frame_ids_wrap() {
        let mut rx = VideoRx::new();
        let now = t0();
        let start = u32::MAX - 1;
        feed(&mut rx, now, &datagrams(start, Flags::KEYFRAME, 0, 300, 0));
        for i in 1..=3u32 {
            feed(
                &mut rx,
                now,
                &datagrams(start.wrapping_add(i), 0, 0, 300, 0),
            );
        }
        assert_eq!(
            ids(&rx.take()),
            [u32::MAX - 1, u32::MAX, 0, 1],
            "in order across the wrap"
        );
        assert!(before(u32::MAX, 0));
        assert!(!before(0, u32::MAX));
        assert!(!before(5, 5));
    }

    #[test]
    fn duplicates_and_late_frames_are_dropped() {
        let mut rx = VideoRx::new();
        let now = t0();
        let k = datagrams(0, Flags::KEYFRAME, 0, 300, 0);
        feed(&mut rx, now, &k);
        feed(&mut rx, now, &k);
        feed(&mut rx, now, &datagrams(1, 0, 0, 300, 0));
        assert_eq!(ids(&rx.take()), [0, 1]);
        feed(&mut rx, now, &datagrams(0, Flags::KEYFRAME, 0, 300, 0));
        assert!(rx.take().frames.is_empty());
    }

    #[test]
    fn waiting_frames_are_capped_to_the_newest_120() {
        let mut rx = VideoRx::new();
        let now = t0();
        for i in 0..250 {
            feed(&mut rx, now, &datagrams(i, 0, 0, 100, 0));
        }
        rx.take();
        assert!(rx.complete.len() <= MAX_WAITING);
        assert!(rx.complete.contains_key(&249));
        assert!(!rx.complete.contains_key(&0));
        // Trimmed down to the newest 120 when it passed the cap.
        assert!(rx.complete.len() >= MAX_WAITING / 2);
    }

    #[test]
    fn loss_counts_what_never_came_after_the_reorder_window() {
        let mut rx = VideoRx::new();
        let now = t0();
        let mut d = datagrams(0, Flags::KEYFRAME, 0, 1000, 0);
        let total = d.len();
        d.remove(1);
        feed(&mut rx, now, &d);
        assert_eq!(rx.loss(now + ms(100)), None, "not settled yet");
        let l = rx.loss(now + ms(250)).unwrap();
        assert!((l - 1.0 / total as f64).abs() < 1e-9, "{l}");
        // It leaves the window after a second.
        assert_eq!(rx.loss(now + ms(1500)), None);
    }

    #[test]
    fn a_late_straggler_within_the_window_is_not_loss() {
        let mut rx = VideoRx::new();
        let now = t0();
        let mut d = datagrams(0, Flags::KEYFRAME, 0, 1000, 0);
        let late = d.remove(1);
        feed(&mut rx, now, &d);
        feed(&mut rx, now + ms(100), &[late]);
        assert_eq!(rx.loss(now + ms(250)), Some(0.0));
    }

    #[test]
    fn a_full_consumer_resyncs_to_the_next_keyframe() {
        let mut rx = VideoRx::new();
        let now = t0();
        feed(&mut rx, now, &datagrams(0, Flags::KEYFRAME, 0, 300, 0));
        rx.take();
        rx.resync(now + ms(1));
        assert_eq!(rx.take().asks, [Ask::Keyframe]);
        feed(&mut rx, now + ms(2), &datagrams(1, 0, 0, 300, 0));
        assert!(rx.take().frames.is_empty());
        feed(
            &mut rx,
            now + ms(3),
            &datagrams(2, Flags::KEYFRAME, 0, 300, 0),
        );
        assert_eq!(ids(&rx.take()), [2]);
    }

    #[test]
    fn intra_datagrams_are_counted_and_dropped() {
        let mut rx = VideoRx::new();
        let now = t0();
        feed(
            &mut rx,
            now,
            &datagrams(0, Flags::KEYFRAME | Flags::INTRA, 0, 300, 0),
        );
        let out = rx.take();
        assert!(out.frames.is_empty());
        assert!(rx.intra_dropped() > 0);
    }
}
