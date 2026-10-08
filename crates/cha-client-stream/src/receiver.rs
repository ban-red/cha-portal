//! Everything the receiving side decides from datagrams and the clock, with no
//! sockets: the datagram demux, video reassembly ([`crate::video`]), audio
//! ordering, the clock sync from ping/pong, the rate-control `report` and the
//! silence watchdog. Time is passed in, so tests drive it with made-up
//! instants. A port of the receive half of `web/packages/player/src/wt-worker.ts`
//! (the report, the clock) and `liveness.ts` / the ping parts of `player.ts`.
//!
//! ## The report
//!
//! `{"t":"report","r","d","l"}` every 100 ms is the streamer's rate control
//! hearing from the receiver (plan §3.1 rule 1):
//!
//! - `r`: Mbit/s of all datagrams (audio and headers too) over about the last
//!   200 ms, divided by the time those buckets really span.
//! - `d`: the median of (completion − send time) of frames completed in the
//!   last 150 ms, in ms. The send time is the datagram's `send_ts_us` mapped
//!   to our clock with the ping offset; any constant offset cancels in the
//!   streamer's use of it. Absent until the clock is synced.
//! - `l`: missing over expected datagrams of frames that settled in the last
//!   second ([`VideoRx::loss`]).
//!
//! ## The clock
//!
//! A ping carries our clock in ms (since the session began) as `c`; the pong
//! echoes it with the streamer's `s_us`. The sample with the lowest round trip
//! so far sets `offset = c + rtt/2 − s_us/1000` (our clock − theirs). A new
//! session starts a new [`Clock`]: the streamer's epoch is per session.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use bytes::Bytes;
use cha_proto::{DatagramHeader, Kind};

use crate::video::{Ask, Frame, VideoRx, before};

/// No data from the streamer for this long: the connection is dead.
pub const SILENCE: Duration = Duration::from_secs(4);
/// A check gap longer than this means we were asleep: grace, not judgement.
const FROZEN: Duration = Duration::from_millis(2500);
/// An audio id this far behind the last is a restart of the count, not late.
const AUDIO_RESET: i32 = 1000;

/// Our clock and the streamer's, as pings estimate them.
#[derive(Debug)]
pub struct Clock {
    base: Instant,
    /// Our clock − the streamer's, in ms.
    offset_ms: Option<f64>,
    best_rtt_ms: f64,
}

impl Clock {
    pub fn new(base: Instant) -> Self {
        Self {
            base,
            offset_ms: None,
            best_rtt_ms: f64::INFINITY,
        }
    }

    /// `now` on our clock, in ms since the session began.
    pub fn local_ms(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.base).as_secs_f64() * 1000.0
    }

    pub fn offset_ms(&self) -> Option<f64> {
        self.offset_ms
    }

    /// A pong for the ping that carried `c` (our clock then) came now.
    pub fn on_pong(&mut self, now: Instant, c: f64, s_us: f64) {
        let rtt = self.local_ms(now) - c;
        if rtt >= 0.0 && rtt < self.best_rtt_ms {
            self.best_rtt_ms = rtt;
            self.offset_ms = Some(c + rtt / 2.0 - s_us / 1000.0);
        }
    }

    /// When a datagram stamped `ts` (the streamer's µs, 32 bits) was sent, on
    /// our clock in ms; `None` until synced.
    pub fn sent_at_ms(&self, now: Instant, ts: u32) -> Option<f64> {
        let offset = self.offset_ms?;
        let now_us = (self.local_ms(now) - offset) * 1000.0;
        let span = 4_294_967_296.0_f64;
        // Just behind the streamer's now, or a little ahead (the estimate
        // assumes symmetric paths).
        let behind = ((now_us % span) - f64::from(ts) + span) % span;
        let server_us = if behind > span / 2.0 {
            now_us + (span - behind)
        } else {
            now_us - behind
        };
        Some(server_us / 1000.0 + offset)
    }
}

/// The silence watchdog (`liveness.ts`): anything from the streamer counts.
#[derive(Debug)]
pub struct Liveness {
    last_inbound: Instant,
    last_check: Instant,
}

impl Liveness {
    pub fn new(now: Instant) -> Self {
        Self {
            last_inbound: now,
            last_check: now,
        }
    }

    pub fn bump(&mut self, now: Instant) {
        self.last_inbound = now;
    }

    /// False when nothing came for [`SILENCE`]. A check that comes much
    /// later than the last (the machine slept) grants a fresh grace instead.
    pub fn alive(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.last_check) > FROZEN {
            self.last_inbound = now;
        }
        self.last_check = now;
        now.saturating_duration_since(self.last_inbound) <= SILENCE
    }
}

/// One audio packet, in order.
#[derive(Clone, Debug)]
pub struct Audio {
    /// The packet's index: samples at its start / 480.
    pub id: u32,
    pub data: Bytes,
}

/// What the rate-control report says.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Report {
    /// Mbit/s received, two decimals.
    pub r: f64,
    /// Median delay of recently completed frames, ms, one decimal.
    pub d: Option<f64>,
    /// Share of datagrams that never came, four decimals.
    pub l: Option<f64>,
}

impl Report {
    /// The control line.
    pub fn line(&self) -> String {
        let mut v = serde_json::json!({"t": "report", "r": self.r});
        if let Some(d) = self.d {
            v["d"] = d.into();
        }
        if let Some(l) = self.l {
            v["l"] = l.into();
        }
        v.to_string()
    }
}

/// What the receiver produced since the last [`Receiver::take`].
#[derive(Debug, Default)]
pub struct Output {
    pub frames: Vec<Frame>,
    pub audio: Vec<Audio>,
    pub asks: Vec<Ask>,
    pub lost: u32,
    pub recovered: u32,
    /// PyroWave frames delivered with only their whole packets.
    pub partial: u32,
}

pub struct Receiver {
    video: VideoRx,
    clock: Clock,
    live: Liveness,
    /// (when on our clock in ms, ms) per completed frame.
    delays: VecDeque<(f64, f64)>,
    /// (when the interval began, bytes) per report interval.
    arrived: VecDeque<(f64, u64)>,
    report_bytes: u64,
    reported_at: Option<f64>,
    last_audio: Option<u32>,
    audio: Vec<Audio>,
    out: crate::video::Output,
    dropped: u64,
}

impl Receiver {
    pub fn new(now: Instant) -> Self {
        Self {
            video: VideoRx::new(),
            clock: Clock::new(now),
            live: Liveness::new(now),
            delays: VecDeque::new(),
            arrived: VecDeque::new(),
            report_bytes: 0,
            reported_at: None,
            last_audio: None,
            audio: Vec::new(),
            out: crate::video::Output::default(),
            dropped: 0,
        }
    }

    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    /// Datagrams that were not valid `cha-stream/1`, or of a kind we don't read.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The stream carries PyroWave: no keyframes, nothing is asked for.
    pub fn is_intra(&self) -> bool {
        self.video.is_intra()
    }

    /// The value of the next ping's `c`.
    pub fn ping_value(&self, now: Instant) -> f64 {
        self.clock.local_ms(now)
    }

    /// Something other than a datagram came (a control line).
    pub fn heard(&mut self, now: Instant) {
        self.live.bump(now);
    }

    pub fn on_pong(&mut self, now: Instant, c: f64, s_us: f64) {
        self.clock.on_pong(now, c, s_us);
    }

    /// False when the streamer has been silent too long.
    pub fn alive(&mut self, now: Instant) -> bool {
        self.live.alive(now)
    }

    /// A datagram came.
    pub fn on_datagram(&mut self, now: Instant, datagram: &[u8]) {
        self.report_bytes += datagram.len() as u64;
        let Ok((header, payload)) = DatagramHeader::decode(datagram) else {
            self.dropped += 1;
            return;
        };
        match header.kind {
            Kind::Audio => {
                self.live.bump(now);
                self.on_audio(&header, payload);
            }
            Kind::Video => {
                self.video.push(now, &header, payload);
                self.collect(now);
            }
            _ => self.dropped += 1,
        }
    }

    /// Run about every 10 ms.
    pub fn tick(&mut self, now: Instant) {
        self.video.tick(now);
        self.collect(now);
    }

    /// The consumer dropped frames: start over from the next keyframe.
    pub fn resync(&mut self, now: Instant) {
        self.video.resync(now);
        self.collect(now);
    }

    /// Everything produced since the last call.
    pub fn take(&mut self) -> Output {
        let v = std::mem::take(&mut self.out);
        Output {
            frames: v.frames,
            audio: std::mem::take(&mut self.audio),
            asks: v.asks,
            lost: v.lost,
            recovered: v.recovered,
            partial: v.partial,
        }
    }

    fn collect(&mut self, now: Instant) {
        let v = self.video.take();
        for ts in &v.completed {
            self.live.bump(now);
            if let Some(sent) = self.clock.sent_at_ms(now, *ts) {
                let t = self.clock.local_ms(now);
                self.delays.push_back((t, t - sent));
            }
        }
        self.out.frames.extend(v.frames);
        self.out.asks.extend(v.asks);
        self.out.lost += v.lost;
        self.out.recovered += v.recovered;
        self.out.partial += v.partial;
    }

    /// Audio in order: a duplicate or a packet behind one already passed is
    /// dropped (Opus is fed an increasing run), and a large step back is the
    /// count starting over.
    fn on_audio(&mut self, header: &DatagramHeader, payload: &[u8]) {
        let id = header.frame_id;
        if let Some(last) = self.last_audio {
            let step = id.wrapping_sub(last) as i32;
            if step <= 0 && step > -AUDIO_RESET {
                return;
            }
        }
        self.last_audio = Some(id);
        self.audio.push(Audio {
            id,
            data: Bytes::copy_from_slice(payload),
        });
    }

    /// The rate-control report: delay when frames came, the rate always.
    /// Call every 100 ms.
    pub fn report(&mut self, now: Instant) -> Report {
        let t = self.clock.local_ms(now);
        self.arrived
            .push_back((self.reported_at.unwrap_or(t - 100.0), self.report_bytes));
        self.reported_at = Some(t);
        self.report_bytes = 0;
        // About the last 200 ms, over the time it really took (timers run late).
        while self.arrived.len() > 1 && t - self.arrived[0].0 > 250.0 {
            self.arrived.pop_front();
        }
        while self.delays.front().is_some_and(|d| t - d.0 > 150.0) {
            self.delays.pop_front();
        }
        let span = (t - self.arrived[0].0).max(1.0);
        let bytes: u64 = self.arrived.iter().map(|a| a.1).sum();
        let mbps = bytes as f64 * 8.0 / (span * 1000.0);
        let d = if self.delays.is_empty() {
            None
        } else {
            let mut sorted: Vec<f64> = self.delays.iter().map(|d| d.1).collect();
            sorted.sort_by(f64::total_cmp);
            Some(sorted[sorted.len() / 2])
        };
        let l = self.video.loss(now);
        Report {
            r: (mbps * 100.0).round() / 100.0,
            d: d.map(|d| (d * 10.0).round() / 10.0),
            l: l.map(|l| (l * 10_000.0).round() / 10_000.0),
        }
    }
}

/// Used by tests and the session to number frames increasingly across the
/// u32 wrap of the frame id.
#[derive(Debug, Default)]
pub struct Numbering {
    last: Option<u32>,
    high: u64,
}

impl Numbering {
    pub fn number(&mut self, id: u32) -> u64 {
        if let Some(last) = self.last
            && id < last
            && before(last, id)
        {
            self.high += 1 << 32;
        }
        self.last = Some(id);
        self.high + u64::from(id)
    }

    /// A new streamer session starts its frame ids at 0 again: the numbers
    /// go on from where they were.
    pub fn restart(&mut self) {
        if let Some(last) = self.last.take() {
            self.high += u64::from(last) + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cha_proto::{Flags, Fragmenter};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn video(id: u32, flags: u8, ts: u32, len: usize) -> Vec<Vec<u8>> {
        let frame = vec![id as u8; len];
        let mut out = Vec::new();
        let base = DatagramHeader {
            kind: Kind::Video,
            flags: Flags(flags),
            stream: 0,
            fec: 0,
            frame_id: id,
            frag_index: 0,
            frag_count: 1,
            send_ts_us: ts,
        };
        Fragmenter::new(1200)
            .fragment(base, &frame, |d| out.push(d.to_vec()))
            .unwrap();
        out
    }

    fn audio(id: u32, ts: u32) -> Vec<u8> {
        let mut head = [0u8; cha_proto::HEADER_LEN];
        DatagramHeader {
            kind: Kind::Audio,
            flags: Flags(0),
            stream: 0,
            fec: 0,
            frame_id: id,
            frag_index: 0,
            frag_count: 1,
            send_ts_us: ts,
        }
        .encode(&mut head);
        let mut d = head.to_vec();
        d.extend_from_slice(&[0xAA, id as u8]);
        d
    }

    #[test]
    fn the_clock_takes_the_lowest_round_trip() {
        let t0 = Instant::now();
        let mut clock = Clock::new(t0);
        assert_eq!(clock.sent_at_ms(t0, 0), None);
        // Sent at local 100 ms; the streamer stamped 5_000 µs; back at 140.
        clock.on_pong(t0 + ms(140), 100.0, 5_000.0);
        // offset = 100 + 20 - 5 = 115 (we are 115 ms ahead of the streamer)
        assert!((clock.offset_ms().unwrap() - 115.0).abs() < 1e-9);
        // A slower round trip does not replace it.
        clock.on_pong(t0 + ms(1300), 1000.0, 900_000.0);
        assert!((clock.offset_ms().unwrap() - 115.0).abs() < 1e-9);
        // A faster one does.
        clock.on_pong(t0 + ms(2010), 2000.0, 1_880_000.0);
        assert!((clock.offset_ms().unwrap() - (2000.0 + 5.0 - 1880.0)).abs() < 1e-9);
    }

    #[test]
    fn a_send_stamp_maps_to_our_clock_across_the_u32_wrap() {
        let t0 = Instant::now();
        let mut clock = Clock::new(t0);
        // Offset zero: both clocks agree.
        clock.on_pong(t0 + ms(2), 0.0, 1_000.0);
        let off = clock.offset_ms().unwrap();
        assert!(off.abs() < 1.5);
        // Now is 10 s in; a stamp from 8 ms ago maps to ~9_992 ms.
        let now = t0 + Duration::from_secs(10);
        let ts = ((10_000.0 - off - 8.0) * 1000.0) as u32;
        let sent = clock.sent_at_ms(now, ts).unwrap();
        assert!((sent - 9_992.0).abs() < 1.0, "{sent}");
        // The streamer's epoch is 71 minutes old: the stamp wrapped.
        let mut late = Clock::new(t0);
        let server_us_at_pong: f64 = 4_294_967_296.0 + 5_000_000.0; // 5 s past one wrap
        late.on_pong(t0 + ms(2), 0.0, server_us_at_pong);
        let off = late.offset_ms().unwrap();
        let server_now_us = (late.local_ms(now) - off) * 1000.0; // beyond the wrap
        assert!(server_now_us > 4_294_967_296.0);
        let ts = ((server_now_us - 8_000.0) % 4_294_967_296.0) as u32; // wrapped
        let sent = late.sent_at_ms(now, ts).unwrap();
        assert!((sent - 9_992.0).abs() < 1.5, "{sent}");
        // And just before the wrap, a stamp from slightly ahead of now maps ahead.
        let ahead = ((server_now_us + 3_000.0) % 4_294_967_296.0) as u32;
        let sent = late.sent_at_ms(now, ahead).unwrap();
        assert!((sent - 10_003.0).abs() < 1.5, "{sent}");
    }

    /// Checks every 100 ms from `from` to `to`; the first time it is dead.
    fn first_dead(live: &mut Liveness, t0: Instant, from: u64, to: u64) -> Option<u64> {
        (from..=to).step_by(100).find(|&t| !live.alive(t0 + ms(t)))
    }

    #[test]
    fn the_watchdog_trips_after_four_silent_seconds() {
        let t0 = Instant::now();
        let mut live = Liveness::new(t0);
        assert_eq!(first_dead(&mut live, t0, 100, 3900), None);
        live.bump(t0 + ms(3950));
        assert_eq!(first_dead(&mut live, t0, 4000, 7900), None);
        assert_eq!(first_dead(&mut live, t0, 8000, 9000), Some(8000));
    }

    #[test]
    fn a_long_gap_between_checks_is_sleep_not_silence() {
        let t0 = Instant::now();
        let mut live = Liveness::new(t0);
        assert!(live.alive(t0 + ms(100)));
        // Asleep for a minute: a fresh grace.
        assert!(live.alive(t0 + Duration::from_secs(60)));
        let from = 60_000;
        assert_eq!(first_dead(&mut live, t0, from + 100, from + 3900), None);
        assert_eq!(
            first_dead(&mut live, t0, from + 4000, from + 4500),
            Some(from + 4100)
        );
    }

    #[test]
    fn audio_goes_out_in_order_without_duplicates() {
        let t0 = Instant::now();
        let mut rx = Receiver::new(t0);
        for id in [10, 11, 13, 12, 13, 14] {
            rx.on_datagram(t0, &audio(id, 0));
        }
        let ids: Vec<u32> = rx.take().audio.iter().map(|a| a.id).collect();
        assert_eq!(ids, [10, 11, 13, 14], "12 came late and 13 twice");
    }

    #[test]
    fn audio_ids_wrap_and_a_restart_resets() {
        let t0 = Instant::now();
        let mut rx = Receiver::new(t0);
        for id in [u32::MAX - 1, u32::MAX, 0, 1] {
            rx.on_datagram(t0, &audio(id, 0));
        }
        assert_eq!(rx.take().audio.len(), 4);
        rx.on_datagram(t0, &audio(5000, 0));
        rx.on_datagram(t0, &audio(2, 0)); // the count started over
        let ids: Vec<u32> = rx.take().audio.iter().map(|a| a.id).collect();
        assert_eq!(ids, [5000, 2]);
    }

    #[test]
    fn the_report_measures_rate_delay_and_loss() {
        let t0 = Instant::now();
        let mut rx = Receiver::new(t0);
        // Synced, offset about zero.
        rx.on_pong(t0 + ms(2), 0.0, 1_000.0);
        let mut sent = 0usize;
        // A 4-fragment keyframe, one fragment lost, then deltas, 10 ms apart.
        let mut kf = video(0, Flags::KEYFRAME, 0, 4000);
        assert_eq!(kf.len(), 4);
        kf.remove(2);
        for d in &kf {
            sent += d.len();
            rx.on_datagram(t0 + ms(5), d);
        }
        for i in 1..=8u32 {
            for d in video(i, 0, i * 10_000, 500) {
                sent += d.len();
                rx.on_datagram(t0 + ms(5 + i as u64 * 10), &d);
            }
            rx.tick(t0 + ms(5 + i as u64 * 10));
        }
        // The keyframe never completed; the deltas wait: no delay yet for it,
        // but delta frames completed and count (the delay needs only the
        // completion), and the rate is what arrived.
        let rep = rx.report(t0 + ms(100));
        let expected_mbps = (sent as f64 * 8.0 / (100.0 * 1000.0) * 100.0).round() / 100.0;
        assert!((rep.r - expected_mbps).abs() < 0.011, "{rep:?}");
        assert!(rep.d.is_some());
        // Not settled yet: the keyframe is younger than 200 ms.
        assert_eq!(rep.l, None);
        let rep = rx.report(t0 + ms(300));
        // 4 expected for the keyframe, 3 came; deltas whole.
        let l = rep.l.unwrap();
        assert!((l - 1.0 / 12.0).abs() < 0.0001, "{l}");
        assert_eq!(rep.d, None, "nothing completed in the last 150 ms");
    }

    #[test]
    fn the_report_line_has_only_what_is_known() {
        let line = Report {
            r: 12.34,
            d: None,
            l: None,
        }
        .line();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["t"], "report");
        assert_eq!(v["r"], 12.34);
        assert!(v.get("d").is_none() && v.get("l").is_none());
        let line = Report {
            r: 1.0,
            d: Some(3.5),
            l: Some(0.01),
        }
        .line();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["d"], 3.5);
        assert_eq!(v["l"], 0.01);
    }

    #[test]
    fn frames_numbers_keep_increasing_across_the_id_wrap() {
        let mut n = Numbering::default();
        let a = n.number(u32::MAX - 1);
        let b = n.number(u32::MAX);
        let c = n.number(0);
        let d = n.number(1);
        assert!(a < b && b < c && c < d, "{a} {b} {c} {d}");
        // A restart after a drop of ids (a stream switch) is not a wrap.
        assert!(n.number(0x4000_0000) > d);
    }

    #[test]
    fn bad_datagrams_are_counted_and_dropped() {
        let t0 = Instant::now();
        let mut rx = Receiver::new(t0);
        rx.on_datagram(t0, &[1, 2, 3]);
        let mut bad = video(0, 0, 0, 10).remove(0);
        bad[0] = 3 << 4; // a wire version we don't speak, kind video
        rx.on_datagram(t0, &bad);
        assert_eq!(rx.dropped(), 2);
        assert!(rx.take().frames.is_empty());
    }

    #[test]
    fn numbers_keep_increasing_across_a_new_session() {
        let mut n = Numbering::default();
        assert_eq!([n.number(0), n.number(1), n.number(7)], [0, 1, 7]);
        n.restart();
        assert_eq!([n.number(0), n.number(1)], [8, 9]);
        n.restart();
        n.restart();
        assert_eq!(n.number(0), 10);
    }
}
