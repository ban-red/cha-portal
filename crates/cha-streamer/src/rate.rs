//! Rate control for one session (P2.5, plan §3.1 rule 1): delay first, loss
//! last. Each update looks at how far the delay has grown over its recent
//! minimum (a queue at the bottleneck), how long our own send queue is, and
//! how much was lost, and sets the bitrate the encoder aims at. The delay is
//! the page's: send → decoded, which it reports every 100 ms; QUIC's
//! smoothed RTT (it lags by seconds on a draining queue) only stands in
//! until the reports come.
//!
//! - **Hard back-off** (send queue or delay growth over 40 ms, or over 10 %
//!   loss): to 0.85 of what the page received (0.7 of what went out, before
//!   it reports). Not again until frames sent after the cut can have
//!   arrived and been reported (the queue then plus 300 ms: the page's
//!   150 ms median, its 100 ms reports, the way back): until then the
//!   delays reported are those of frames sent into the queue. For loss
//!   alone, not until the page's loss count has left the cut behind
//!   (1.2 s): a queue's overflow is counted for that long after it.
//! - **Soft back-off** (over 20 ms of send queue or 15 ms of delay growth):
//!   10 % down, at most every 200 ms.
//! - **Hold** at 2–10 % loss: random loss isn't congestion, but no climbing.
//! - **Climb** after a calm second: 50 % a second, 10 % near where the path
//!   last pushed back, up to the session's ceiling, and to no more than
//!   twice what goes out (half a second's average): an encoder far short of
//!   its target (a simple picture) shows nothing of what the path takes,
//!   and a target far above it would let the next busy picture burst into
//!   the bottleneck. (Twice, not WebRTC's 1.5: NVENC's low-latency CBR
//!   makes about two thirds of its target even on a busy picture.)
//! - **Rebase**: a "queue" that doesn't shrink while the rate falls by a
//!   third for 2 s is a longer path (a route change), not a queue: the delay
//!   floor moves up to it.
//! - **Stall**: next to nothing arriving (under a quarter of what goes out)
//!   or the reports stopping is mostly the page, busy for a moment (a
//!   collection, a hidden tab), not the path: nothing changes for up to
//!   400 ms. What it held back then comes in a burst, whose delays show the
//!   queue it built, and the cut goes to what arrives then. A congested
//!   path still delivers at its rate; one that doesn't for longer counts.
//!
//! Everything here is in time (milliseconds, seconds) and bits per second,
//! none in frames, so it holds as it is from 60 to 120 fps: the queue the
//! thresholds speak of is the same milliseconds of latency at either rate.
//! Only the ceiling follows the frame rate (`rescale`).

use std::time::{Duration, Instant};

/// The rate's floor, either transport's.
pub const MIN_BPS: u32 = 1_500_000;
/// Video's share of the rate (audio and framing take the rest).
pub const VIDEO_SHARE: f64 = 0.92;

/// How long a minimum delay stands before a higher one replaces it (routes
/// change; a queue that never drains would otherwise look like the floor).
const MIN_WINDOW: Duration = Duration::from_secs(10);
/// A receiver report older than this doesn't count.
const REPORT_FRESH: Duration = Duration::from_millis(300);
/// How long a stall is waited out before it counts as the path's.
const STALL_GRACE: Duration = Duration::from_millis(400);
/// Arriving at under this share of what goes out is a stall.
const STALL_SHARE: f64 = 0.25;
/// How long the page's loss count remembers a loss: a second, and frames
/// settle 200 ms after their first datagram.
const LOSS_MEMORY: Duration = Duration::from_millis(1200);
/// The climb's limit, as a multiple of what goes out.
const HEADROOM: f64 = 2.0;

/// One look at the path, since the last.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// Smoothed round trip, as QUIC measures it.
    pub rtt: Duration,
    /// The page's latest send → decoded (ms, any constant clock offset
    /// included), if it reported recently.
    pub delivery_ms: Option<f64>,
    /// What the page received lately (bits per second), if it reported.
    pub rx_bps: Option<f64>,
    /// Our send queue, in milliseconds of frames.
    pub backlog_ms: f64,
    /// The share of what was sent that never arrived (the page's count when
    /// it reports, else QUIC's).
    pub loss: f64,
    /// What actually went out, bits per second.
    pub tx_bps: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Overuse,
    /// Over the line, but a cut was just made: let its queue drain.
    Draining,
    /// The delay floor moved up (a longer path).
    Rebase,
    /// Next to nothing arrives, or the page stopped reporting: waited out.
    Stall,
    Backoff,
    Hold,
    Climb,
    Steady,
}

/// A windowed minimum: the floor a delay grows from.
#[derive(Default)]
struct Floor(Option<(f64, Instant)>);

impl Floor {
    /// The value's growth over the floor, after taking it in.
    fn growth(&mut self, now: Instant, value: f64) -> f64 {
        match self.0 {
            Some((min, at)) if value > min && now.duration_since(at) < MIN_WINDOW => value - min,
            _ => {
                self.0 = Some((value, now));
                0.0
            }
        }
    }

    fn rebase(&mut self, now: Instant, value: f64) {
        self.0 = Some((value, now));
    }
}

pub struct RateControl {
    min_bps: f64,
    max_bps: f64,
    target: f64,
    rtt_floor: Floor,
    delivery_floor: Floor,
    last_decrease: Option<Instant>,
    /// The last hard cut: when, and the queue then.
    last_overuse: Option<(Instant, f64)>,
    /// What the path took when it last pushed back (what arrived, or
    /// before the page reports, what went out).
    ceiling: Option<(f64, Instant)>,
    /// What goes out, averaged over half a second or so.
    sent_bps: Option<f64>,
    /// The page has reported (so its silence means something), and since
    /// when it has stalled.
    reported: bool,
    stall_since: Option<Instant>,
    /// The queue signal at the last update (ms).
    queue_ms: f64,
    /// Since when the queue has stood, at what, and the target then.
    standing: Option<(Instant, f64, f64)>,
    last_update: Option<Instant>,
}

impl RateControl {
    pub fn new(min_bps: u32, max_bps: u32) -> Self {
        Self {
            min_bps: f64::from(min_bps),
            max_bps: f64::from(max_bps.max(min_bps)),
            target: f64::from(max_bps.max(min_bps)),
            rtt_floor: Floor::default(),
            delivery_floor: Floor::default(),
            last_decrease: None,
            last_overuse: None,
            ceiling: None,
            sent_bps: None,
            reported: false,
            stall_since: None,
            queue_ms: 0.0,
            standing: None,
            last_update: None,
        }
    }

    pub fn target(&self) -> u32 {
        self.target as u32
    }

    /// The ceiling moved (the frame rate changed, and the NVENC codecs'
    /// bitrate with it). A target that was at the old ceiling, because the
    /// path never held it back, goes to the new one; one the path had cut
    /// follows a lower ceiling down in proportion, and a higher one by
    /// climbing: what the path takes didn't change.
    pub fn rescale(&mut self, max_bps: u32) {
        let old = self.max_bps;
        self.max_bps = f64::from(max_bps).max(self.min_bps);
        if self.target >= 0.98 * old {
            self.target = self.max_bps;
        } else if self.max_bps < old {
            self.target *= self.max_bps / old;
        }
        self.target = self.target.clamp(self.min_bps, self.max_bps);
    }

    /// The queue the last update saw (delay growth over its floor, ms).
    pub fn queue_ms(&self) -> f64 {
        self.queue_ms
    }

    pub fn update(&mut self, now: Instant, s: &Sample) -> Verdict {
        let dt = self
            .last_update
            .replace(now)
            .map_or(0.0, |at| now.duration_since(at).as_secs_f64().min(0.5));
        let rtt_growth = self.rtt_floor.growth(now, s.rtt.as_secs_f64() * 1e3);
        let queue_ms = match s.delivery_ms {
            Some(d) => self.delivery_floor.growth(now, d),
            None => rtt_growth,
        };
        self.queue_ms = queue_ms;
        if queue_ms <= 15.0 {
            self.standing = None;
        } else {
            let (since, at_queue, at_target) =
                *self.standing.get_or_insert((now, queue_ms, self.target));
            if now.duration_since(since) >= Duration::from_secs(2)
                && (queue_ms - at_queue).abs() < 8.0
                && self.target < 0.67 * at_target
            {
                // The rate fell by a third and the "queue" didn't move.
                match s.delivery_ms {
                    Some(d) => self.delivery_floor.rebase(now, d),
                    None => self.rtt_floor.rebase(now, s.rtt.as_secs_f64() * 1e3),
                }
                self.standing = None;
                self.queue_ms = 0.0;
                return Verdict::Rebase;
            }
        }
        let blend = 1.0 - (-dt / 0.5).exp();
        let sent = self
            .sent_bps
            .map_or(s.tx_bps, |v| v + blend * (s.tx_bps - v));
        self.sent_bps = Some(sent);
        let stalled = match s.rx_bps {
            Some(rx) => {
                self.reported = true;
                rx < STALL_SHARE * sent
            }
            None => self.reported,
        };
        if !stalled {
            self.stall_since = None;
        } else if now.duration_since(*self.stall_since.get_or_insert(now)) < STALL_GRACE {
            return Verdict::Stall;
        }
        let loss = s.loss;
        let since_decrease = self.last_decrease.map(|at| now.duration_since(at));
        let queued = s.backlog_ms > 40.0 || queue_ms > 40.0;
        let verdict = if queued || loss > 0.10 {
            let draining = self.last_overuse.is_some_and(|(at, queue)| {
                let settle = Duration::from_secs_f64((queue.max(0.0) + 300.0) / 1e3);
                let since = now.duration_since(at);
                since < settle || (!queued && since < LOSS_MEMORY)
            });
            if draining {
                Verdict::Draining
            } else {
                // What arrived is what the path takes; aim just under it.
                let (took, factor) = match s.rx_bps {
                    Some(rx) => (rx.min(s.tx_bps), 0.85),
                    None => (s.tx_bps, 0.7),
                };
                let took = took.max(self.min_bps);
                self.target = self.target.min(took) * factor;
                self.ceiling = Some((s.rx_bps.unwrap_or(took), now));
                self.last_decrease = Some(now);
                self.last_overuse = Some((now, queue_ms));
                Verdict::Overuse
            }
        } else if s.backlog_ms > 20.0 || queue_ms > 15.0 {
            if since_decrease.is_none_or(|d| d >= Duration::from_millis(200)) {
                self.target *= 0.9;
                self.last_decrease = Some(now);
            }
            Verdict::Backoff
        } else if loss > 0.02 {
            Verdict::Hold
        } else if since_decrease.is_none_or(|d| d >= Duration::from_secs(1)) {
            let near_ceiling = self.ceiling.is_some_and(|(bps, at)| {
                now.duration_since(at) < MIN_WINDOW && self.target > 0.9 * bps
            });
            let per_second: f64 = if near_ceiling { 1.1 } else { 1.5 };
            let room = HEADROOM * sent;
            if self.target < room {
                self.target = (self.target * per_second.powf(dt)).min(room);
                Verdict::Climb
            } else {
                Verdict::Steady
            }
        } else {
            Verdict::Steady
        };
        self.target = self.target.clamp(self.min_bps, self.max_bps);
        verdict
    }
}

/// One report from the page; each part only when it has one.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Report {
    /// Send → complete (ms, any constant clock offset included).
    pub delivery_ms: Option<f64>,
    /// What arrived, bits per second.
    pub rx_bps: Option<f64>,
    /// The share of data fragments that never arrived.
    pub loss: Option<f64>,
}

/// What a `{"t":"report","d":…,"r":…,"l":…}` line carries: send →
/// complete (ms, absent while no frame arrived), the receive rate (Mbit/s,
/// as bits per second) and the share of what was sent that never arrived.
pub fn parse_report(line: &str) -> Option<Report> {
    if !line.contains("\"report\"") {
        return None;
    }
    let msg: serde_json::Value = serde_json::from_str(line).ok()?;
    if msg.get("t")?.as_str()? != "report" {
        return None;
    }
    let number = |key: &str| msg.get(key).and_then(|v| v.as_f64());
    Some(Report {
        delivery_ms: number("d"),
        rx_bps: number("r").map(|mbps| mbps * 1e6),
        loss: number("l"),
    })
}

/// The page's reports, kept fresh: each part as of its own last report.
#[derive(Default)]
pub struct Reports {
    delivery: Option<(f64, Instant)>,
    rx: Option<(f64, Instant)>,
    loss: Option<(f64, Instant)>,
}

impl Reports {
    pub fn record(&mut self, report: Report, now: Instant) {
        for (value, slot) in [
            (report.delivery_ms, &mut self.delivery),
            (report.rx_bps, &mut self.rx),
            (report.loss, &mut self.loss),
        ] {
            if let Some(v) = value {
                *slot = Some((v, now));
            }
        }
    }

    /// What the page reported lately.
    pub fn latest(&self, now: Instant) -> Report {
        let fresh = |v: Option<(f64, Instant)>| {
            v.filter(|(_, at)| now.duration_since(*at) < REPORT_FRESH)
                .map(|(v, _)| v)
        };
        Report {
            delivery_ms: fresh(self.delivery),
            rx_bps: fresh(self.rx),
            loss: fresh(self.loss),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(rtt_ms: u64, backlog_ms: f64, tx_mbps: f64) -> Sample {
        Sample {
            rtt: Duration::from_millis(rtt_ms),
            delivery_ms: None,
            rx_bps: None,
            backlog_ms,
            loss: 0.0,
            tx_bps: tx_mbps * 1e6,
        }
    }

    #[test]
    fn backs_off_hard_to_under_what_went_out() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        let t = Instant::now();
        rc.update(t, &sample(20, 0.0, 30.0));
        // The queue grows: 60 ms over the floor.
        let v = rc.update(t + Duration::from_millis(100), &sample(80, 10.0, 10.0));
        assert_eq!(v, Verdict::Overuse);
        assert_eq!(rc.target(), 7_000_000);
    }

    #[test]
    fn climbs_only_after_a_calm_second() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        let mut t = Instant::now();
        rc.update(t, &sample(20, 0.0, 30.0));
        rc.update(t, &sample(80, 0.0, 10.0));
        let low = rc.target();
        t += Duration::from_millis(500);
        assert_eq!(rc.update(t, &sample(20, 0.0, 7.0)), Verdict::Steady);
        t += Duration::from_millis(600);
        assert_eq!(rc.update(t, &sample(20, 0.0, 7.0)), Verdict::Climb);
        assert!(rc.target() > low);
    }

    #[test]
    fn climbs_no_further_than_the_encoder_goes() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        let mut t = Instant::now();
        // A simple picture: the encoder makes 6 Mbit/s whatever its target.
        rc.update(t, &sample(20, 0.0, 6.0));
        rc.update(t, &sample(80, 0.0, 6.0));
        for _ in 0..100 {
            t += Duration::from_millis(100);
            rc.update(t, &sample(20, 0.0, 6.0));
        }
        assert_eq!(rc.target(), 12_000_000);
        assert_eq!(rc.update(t, &sample(20, 0.0, 6.0)), Verdict::Steady);
    }

    #[test]
    fn random_loss_holds_without_backing_off() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        let t = Instant::now();
        rc.update(t, &sample(20, 0.0, 30.0));
        let before = rc.target();
        let lossy = Sample {
            loss: 0.03,
            ..sample(20, 0.0, 30.0)
        };
        assert_eq!(
            rc.update(t + Duration::from_millis(100), &lossy),
            Verdict::Hold
        );
        assert_eq!(rc.target(), before);
    }

    #[test]
    fn stays_within_its_bounds() {
        let mut rc = RateControl::new(2_000_000, 10_000_000);
        let mut t = Instant::now();
        for _ in 0..20 {
            t += Duration::from_millis(100);
            rc.update(t, &sample(500, 100.0, 0.1));
        }
        assert_eq!(rc.target(), 2_000_000);
    }

    #[test]
    fn one_cut_then_lets_the_queue_drain() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        let t = Instant::now();
        rc.update(t, &sample(20, 0.0, 30.0));
        assert_eq!(
            rc.update(t + Duration::from_millis(100), &sample(120, 0.0, 25.0)),
            Verdict::Overuse
        );
        let after = rc.target();
        // Frames sent into the queue still report it, even growing: no
        // second cut until the 100 ms queue plus 300 ms has passed.
        let v = rc.update(t + Duration::from_millis(250), &sample(150, 0.0, 9.0));
        assert_eq!(v, Verdict::Draining);
        assert_eq!(rc.target(), after);
        // Still there after that: cut again.
        let v = rc.update(t + Duration::from_millis(450), &sample(110, 0.0, 9.0));
        assert_eq!(v, Verdict::Draining);
        let v = rc.update(t + Duration::from_millis(550), &sample(110, 0.0, 9.0));
        assert_eq!(v, Verdict::Overuse);
    }

    #[test]
    fn the_pages_delay_wins_over_a_lagging_rtt() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        let t = Instant::now();
        let with = |rtt, d| Sample {
            delivery_ms: Some(d),
            ..sample(rtt, 0.0, 10.0)
        };
        rc.update(t, &with(20, 30.0));
        // QUIC's RTT still remembers a queue; the page sees none.
        let v = rc.update(t + Duration::from_millis(100), &with(250, 31.0));
        assert_ne!(v, Verdict::Overuse);
        assert!(rc.queue_ms() < 2.0);
    }

    #[test]
    fn climbs_gently_near_where_the_path_pushed_back() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        let mut t = Instant::now();
        rc.update(t, &sample(20, 0.0, 10.0));
        rc.update(t, &sample(80, 0.0, 10.0));
        // Cut to 7; climb back past 9 (0.9 of the 10 that went out).
        let mut last = rc.target();
        t += Duration::from_secs(1);
        let mut steps = Vec::new();
        for _ in 0..12 {
            t += Duration::from_millis(100);
            rc.update(t, &sample(20, 0.0, 9.0));
            steps.push(f64::from(rc.target()) / f64::from(last));
            last = rc.target();
        }
        // 50 % a second (0.1 s steps) until near the 10 that went out, then 10 %.
        assert!((steps[1] - 1.5f64.powf(0.1)).abs() < 1e-3);
        assert!((steps[11] - 1.1f64.powf(0.1)).abs() < 1e-3);
    }

    #[test]
    fn cuts_to_just_under_what_arrived() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        let t = Instant::now();
        rc.update(t, &sample(20, 0.0, 25.0));
        let congested = Sample {
            rx_bps: Some(10e6),
            ..sample(90, 0.0, 25.0)
        };
        assert_eq!(
            rc.update(t + Duration::from_millis(50), &congested),
            Verdict::Overuse
        );
        assert_eq!(rc.target(), 8_500_000);
    }

    #[test]
    fn waits_out_a_stall() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        let mut t = Instant::now();
        let with = |d, rx_mbps: f64| Sample {
            delivery_ms: Some(d),
            rx_bps: Some(rx_mbps * 1e6),
            ..sample(20, 0.0, 25.0)
        };
        rc.update(t, &with(20.0, 25.0));
        let before = rc.target();
        // The page stalls: next to nothing arrives, late; then no reports.
        t += Duration::from_millis(100);
        assert_eq!(rc.update(t, &with(290.0, 1.0)), Verdict::Stall);
        t += Duration::from_millis(200);
        assert_eq!(rc.update(t, &sample(20, 0.0, 25.0)), Verdict::Stall);
        assert_eq!(rc.target(), before);
        // The burst after it: its queue is cut to what goes out.
        t += Duration::from_millis(100);
        assert_eq!(rc.update(t, &with(200.0, 40.0)), Verdict::Overuse);
        assert_eq!(rc.target(), 21_250_000);
    }

    #[test]
    fn an_outage_counts_after_a_moment() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        let mut t = Instant::now();
        let with = |d, rx_mbps: f64| Sample {
            delivery_ms: Some(d),
            rx_bps: Some(rx_mbps * 1e6),
            ..sample(20, 0.0, 25.0)
        };
        rc.update(t, &with(20.0, 25.0));
        let mut verdicts = Vec::new();
        for i in 1..=6 {
            t += Duration::from_millis(100);
            verdicts.push(rc.update(t, &with(20.0 + 100.0 * f64::from(i), 0.5)));
        }
        assert_eq!(verdicts[..4], [Verdict::Stall; 4]);
        assert_eq!(verdicts[4], Verdict::Overuse);
        assert_eq!(rc.target(), 1_000_000);
    }

    #[test]
    fn a_queues_overflow_cuts_once() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        let mut t = Instant::now();
        let with = |d, loss| Sample {
            delivery_ms: Some(d),
            rx_bps: Some(10e6),
            loss,
            ..sample(20, 0.0, 25.0)
        };
        rc.update(t, &with(20.0, 0.0));
        t += Duration::from_millis(100);
        assert_eq!(rc.update(t, &with(110.0, 0.0)), Verdict::Overuse);
        let cut = rc.target();
        // The queue's gone, but the page counts what overflowed for a while.
        let mut verdicts = Vec::new();
        for _ in 0..10 {
            t += Duration::from_millis(100);
            verdicts.push(rc.update(t, &with(20.0, 0.2)));
        }
        assert!(verdicts.iter().all(|v| *v == Verdict::Draining));
        assert_eq!(rc.target(), cut);
        // Still that much loss later: that's the path's.
        t += Duration::from_millis(200);
        assert_eq!(rc.update(t, &with(20.0, 0.2)), Verdict::Overuse);
    }

    #[test]
    fn a_longer_path_moves_the_floor() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        let mut t = Instant::now();
        let with = |d| Sample {
            delivery_ms: Some(d),
            ..sample(20, 0.0, 30.0)
        };
        rc.update(t, &with(5.0));
        // 20 ms more, for good: backs off, but the delay doesn't move.
        let mut verdicts = Vec::new();
        for _ in 0..60 {
            t += Duration::from_millis(50);
            verdicts.push(rc.update(t, &with(25.0)));
        }
        assert!(verdicts.contains(&Verdict::Rebase));
        assert!(rc.queue_ms() < 1.0);
        let v = verdicts
            .iter()
            .rposition(|v| *v == Verdict::Rebase)
            .unwrap();
        assert!(verdicts[v + 1..].iter().all(|v| *v != Verdict::Backoff));
    }

    #[test]
    fn the_ceiling_follows_the_frame_rate() {
        let mut rc = RateControl::new(1_000_000, 40_000_000);
        // Never held back: to the new ceiling at once, and back.
        rc.rescale(67_000_000);
        assert_eq!(rc.target(), 67_000_000);
        rc.rescale(40_000_000);
        assert_eq!(rc.target(), 40_000_000);
        // Cut by the path to 10: a higher ceiling leaves it to climb.
        let t = Instant::now();
        rc.update(t, &sample(20, 0.0, 30.0));
        rc.update(t + Duration::from_millis(100), &sample(80, 0.0, 12.0));
        let cut = rc.target();
        assert!(cut < 20_000_000);
        rc.rescale(67_000_000);
        assert_eq!(rc.target(), cut);
        // A lower one takes it down in proportion.
        rc.rescale(40_000_000 / 2);
        let expected = f64::from(cut) * 20e6 / 67e6;
        assert!((f64::from(rc.target()) - expected).abs() < 2.0);
        // Never under the floor.
        rc.rescale(10);
        assert_eq!(rc.target(), 1_000_000);
    }
}
