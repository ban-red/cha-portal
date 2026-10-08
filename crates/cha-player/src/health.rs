//! A letter grade for the stream right now, from the last few seconds of
//! stats snapshots. It is `web/packages/player/src/health.ts` in Rust: the
//! same issue ids, severities, thresholds, titles, hints, score maths and
//! summary words, so the browser and this player judge a stream alike. Where
//! the player measures a thing differently the text says so.
//!
//! Ported: stutter, dropped frames, decoding, freezes, latency, uneven
//! latency (the latency spread only), packet loss, repairs, skipped frames
//! (PyroWave), incomplete frames (PyroWave), round trip, node overload.
//! Added for this player: Wi-Fi latency spikes (AWDL, no browser counterpart).
//!
//! Not ported, because this player can't measure the signal:
//! - `sound-restart` and `sound-out`: Web Audio's decoder and output;
//!   CoreAudio gives no peak of what it played.
//! - `audio` (sound buffering): the player's own buffer sits at its 30 ms
//!   target by design, so its size says nothing about late packets. The
//!   overlay shows it, ungraded.
//! - the delivery spread and the jitter-buffer wait inside `jitter`:
//!   they need the sender's clock, which the receive path doesn't carry to
//!   the player (the latency here is received to shown, not sent to shown).
//!
//! Checks that need a number a transport doesn't give (round trip, losses,
//! the send rate, the node) are skipped, never penalised.

use crate::ui::StatsSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grade {
    A,
    B,
    C,
    D,
    F,
}

impl Grade {
    pub fn letter(self) -> &'static str {
        match self {
            Grade::A => "A",
            Grade::B => "B",
            Grade::C => "C",
            Grade::D => "D",
            Grade::F => "F",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Minor,
    Major,
    Critical,
}

impl Severity {
    fn rank(self) -> u8 {
        match self {
            Severity::Critical => 2,
            Severity::Major => 1,
            Severity::Minor => 0,
        }
    }

    /// The best score an issue of this severity allows: any issue rules out
    /// an A, a major one a B, a critical one a C.
    fn cap(self) -> f64 {
        match self {
            Severity::Minor => 89.0,
            Severity::Major => 79.0,
            Severity::Critical => 69.0,
        }
    }

    fn of(level: f64) -> Self {
        if level >= CRITICAL_LEVEL {
            Severity::Critical
        } else if level >= MAJOR_LEVEL {
            Severity::Major
        } else {
            Severity::Minor
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub id: &'static str,
    pub severity: Severity,
    /// A few words: what is wrong.
    pub title: &'static str,
    /// What was measured, with the numbers.
    pub detail: String,
    /// What it means and what to try.
    pub hint: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Assessment {
    /// None while there is nothing to judge: fewer than `MIN_SNAPSHOTS` snapshots.
    pub grade: Option<Grade>,
    /// 0..100, None with the grade.
    pub score: Option<u32>,
    /// One line for the grade: "Smooth", "Some stutter", ...
    pub summary: &'static str,
    /// Most severe first. Empty for an A.
    pub issues: Vec<Issue>,
}

impl Default for Assessment {
    fn default() -> Self {
        Self {
            grade: None,
            score: None,
            summary: "Measuring…",
            issues: Vec::new(),
        }
    }
}

impl Assessment {
    /// The first issue with one of these ids: what makes a value read bad.
    pub fn bad(&self, ids: &[&str]) -> Option<&Issue> {
        self.issues.iter().find(|i| ids.contains(&i.id))
    }
}

/// Snapshots judged: the last ~8 s.
pub const HEALTH_WINDOW: usize = 8;
/// Fewer snapshots than this and the counters' deltas and the spread mean nothing.
pub const MIN_SNAPSHOTS: usize = 2;

const SPIKE_SHARE: f64 = 0.35;
const MIN_LEVEL: f64 = 0.1;
const MAJOR_LEVEL: f64 = 0.4;
const CRITICAL_LEVEL: f64 = 0.75;
const GRADE_FLOORS: [(Grade, f64); 5] = [
    (Grade::A, 90.0),
    (Grade::B, 80.0),
    (Grade::C, 70.0),
    (Grade::D, 55.0),
    (Grade::F, 0.0),
];

#[derive(Clone, Copy)]
struct Band {
    from: f64,
    to: f64,
}

const fn band(from: f64, to: f64) -> Band {
    Band { from, to }
}

const FPS_RATIO: Band = band(0.95, 0.6);
const BUSY_SENT_FPS: f64 = 10.0;
const DROPPED: Band = band(0.01, 0.15);
const DECODE_BUDGET: Band = band(0.5, 1.2);
const LATENCY: Band = band(20.0, 100.0);
const LATENCY_SPREAD: Band = band(6.0, 30.0);
const SKIPPED: Band = band(0.01, 0.15);
const GPU_LATE_ENCODE: f64 = 0.5;
const LOST_FRAMES: Band = band(0.2, 6.0);
const RECOVERED: Band = band(2.0, 30.0);
const PARTIAL: Band = band(2.0, 30.0);
const RTT: Band = band(10.0, 80.0);
const FREEZE_BUDGETS: f64 = 3.0;
const FREEZE: Band = band(0.0, 500.0);
const NODE_CPU: Band = band(90.0, 100.0);
const NODE_RAM: Band = band(95.0, 100.0);
const NODE_GPU: Band = band(95.0, 100.0);
const NODE_VRAM: Band = band(95.0, 100.0);
const NODE_ENC: Band = band(95.0, 100.0);
const STREAMER_CPU: Band = band(90.0, 100.0);
const FALLBACK_FPS: f64 = 60.0;

mod weight {
    pub const STUTTER: f64 = 30.0;
    pub const DROPPED: f64 = 20.0;
    pub const DECODE: f64 = 35.0;
    pub const LATENCY: f64 = 30.0;
    pub const JITTER: f64 = 15.0;
    pub const LOSS: f64 = 40.0;
    pub const RECOVERED: f64 = 8.0;
    pub const PARTIAL: f64 = 8.0;
    pub const SKIPPED: f64 = 20.0;
    pub const RTT: f64 = 12.0;
    pub const FREEZE: f64 = 45.0;
    pub const NODE: f64 = 30.0;
    pub const AWDL: f64 = 10.0;
}

/// 0 below `from`, then 0.2 at `from` rising linearly to 1 at `to`.
fn level(v: Option<f64>, band: Band) -> f64 {
    match v {
        Some(v) if v.is_finite() && v >= band.from => {
            0.2 + 0.8 * ((v - band.from) / (band.to - band.from)).min(1.0)
        }
        _ => 0.0,
    }
}

/// The same for a signal where lower is worse (`from` above `to`).
fn level_below(v: Option<f64>, band: Band) -> f64 {
    match v {
        Some(v) if v.is_finite() && v <= band.from => {
            0.2 + 0.8 * ((band.from - v) / (band.from - band.to)).min(1.0)
        }
        _ => 0.0,
    }
}

struct Finding {
    level: f64,
    detail: String,
}

struct Ctx {
    target: f64,
    budget_ms: f64,
    seconds: f64,
}

fn mean(a: &[f64]) -> f64 {
    a.iter().sum::<f64>() / a.len() as f64
}

fn max(a: impl IntoIterator<Item = f64>) -> f64 {
    a.into_iter().fold(0.0, f64::max)
}

/// Mean and worst of per-snapshot levels: sustained trouble counts, and so
/// does a spike, but a single one stays minor.
fn window_level(levels: &[f64]) -> f64 {
    if levels.is_empty() {
        return 0.0;
    }
    mean(levels).max(SPIKE_SHARE * max(levels.iter().copied()))
}

fn values(window: &[StatsSnapshot], pick: impl Fn(&StatsSnapshot) -> Option<f32>) -> Vec<f64> {
    window
        .iter()
        .filter_map(|s| pick(s).map(f64::from))
        .collect()
}

/// The growth of a cumulative counter across the window; a drop (a reconnect
/// reset it) counts as zero.
fn growth(window: &[StatsSnapshot], pick: impl Fn(&StatsSnapshot) -> u64) -> f64 {
    window
        .windows(2)
        .map(|w| pick(&w[1]).saturating_sub(pick(&w[0])) as f64)
        .sum()
}

/// The spread of the middle half.
fn interquartile(a: &[f64]) -> f64 {
    let mut s = a.to_vec();
    s.sort_by(f64::total_cmp);
    let at = |q: f64| s[(q * (s.len() - 1) as f64).round() as usize];
    at(0.75) - at(0.25)
}

fn rate_finding(rate: f64, band: Band, text: impl Fn(f64) -> String) -> Option<Finding> {
    let l = level(Some(rate), band);
    (l > 0.0).then(|| Finding {
        level: l,
        detail: text(rate),
    })
}

fn ms(v: f64, digits: usize) -> String {
    format!("{v:.digits$} ms")
}

/// Frames per second shown over the send rate's span, or the last second's.
fn shown_of_sent(s: &StatsSnapshot) -> f64 {
    f64::from(s.shown_sent_fps.unwrap_or(s.present_fps))
}

struct Check {
    id: &'static str,
    title: &'static str,
    /// For the one-line summary.
    summary: &'static str,
    weight: f64,
    hint: &'static str,
    find: fn(&[StatsSnapshot], &Ctx) -> Option<Finding>,
}

fn busy(s: &StatsSnapshot) -> Option<f64> {
    s.sent_fps
        .map(f64::from)
        .filter(|sent| *sent >= BUSY_SENT_FPS)
}

fn find_stutter(window: &[StatsSnapshot], _: &Ctx) -> Option<Finding> {
    let busy_ones: Vec<(f64, f64)> = window
        .iter()
        .filter_map(|s| busy(s).map(|sent| (shown_of_sent(s), sent)))
        .collect();
    let levels: Vec<f64> = busy_ones
        .iter()
        .map(|(shown, sent)| level_below(Some(shown / sent), FPS_RATIO))
        .collect();
    let l = window_level(&levels);
    if l == 0.0 {
        return None;
    }
    let shown: Vec<f64> = busy_ones.iter().map(|b| b.0).collect();
    let sent: Vec<f64> = busy_ones.iter().map(|b| b.1).collect();
    Some(Finding {
        level: l,
        detail: format!("{:.0} fps shown of {:.0} sent", mean(&shown), mean(&sent)),
    })
}

fn find_dropped(window: &[StatsSnapshot], ctx: &Ctx) -> Option<Finding> {
    if ctx.seconds <= 0.0 {
        return None;
    }
    let rate = growth(window, |s| s.dropped) / ctx.seconds;
    rate_finding(rate / ctx.target, DROPPED, |_| {
        format!("{rate:.1} frames/s dropped")
    })
}

fn find_decode(window: &[StatsSnapshot], ctx: &Ctx) -> Option<Finding> {
    // Only while frames come at a rate: a few frames on a still picture
    // (often keyframes, the slowest to decode) hold nothing up.
    let v: Vec<f64> = window
        .iter()
        .filter(|s| busy(s).is_some())
        .filter_map(|s| s.decode_ms.map(f64::from))
        .collect();
    let levels: Vec<f64> = v
        .iter()
        .map(|d| level(Some(d / ctx.budget_ms), DECODE_BUDGET))
        .collect();
    let l = window_level(&levels);
    if l == 0.0 {
        return None;
    }
    Some(Finding {
        level: l,
        detail: format!(
            "{} to decode a frame, of {} per frame",
            ms(mean(&v), 1),
            ms(ctx.budget_ms, 1)
        ),
    })
}

fn find_freeze(window: &[StatsSnapshot], ctx: &Ctx) -> Option<Finding> {
    let mut levels = Vec::new();
    let mut worst = 0.0f64;
    for s in window {
        // Only while frames were being sent: a still picture has long gaps by
        // nature. A source sending at 10 fps has 100 ms between frames, so
        // the gap must also beat 3 send intervals.
        let Some(sent) = busy(s) else { continue };
        let gap = match s.frame_gap_ms {
            Some(g) => f64::from(g),
            // A second with no frames at all is a gap.
            None if shown_of_sent(s) == 0.0 => 1000.0,
            None => continue,
        };
        let from = 50.0f64
            .max(FREEZE_BUDGETS * ctx.budget_ms)
            .max(FREEZE_BUDGETS * 1000.0 / sent);
        levels.push(level(Some(gap), band(from, FREEZE.to)));
        worst = worst.max(gap);
    }
    let l = window_level(&levels);
    (l > 0.0).then(|| Finding {
        level: l,
        detail: format!("up to {} between frames", ms(worst, 0)),
    })
}

fn find_latency(window: &[StatsSnapshot], _: &Ctx) -> Option<Finding> {
    let v = values(window, |s| s.latency_ms);
    let levels: Vec<f64> = v.iter().map(|d| level(Some(*d), LATENCY)).collect();
    let l = window_level(&levels);
    (l > 0.0).then(|| Finding {
        level: l,
        detail: format!("{} from received to shown", ms(mean(&v), 0)),
    })
}

fn find_jitter(window: &[StatsSnapshot], _: &Ctx) -> Option<Finding> {
    let lat = values(window, |s| s.latency_ms);
    if lat.len() < 3 {
        return None;
    }
    let sd = interquartile(&lat);
    let l = level(Some(sd), LATENCY_SPREAD);
    (l > 0.0).then(|| Finding {
        level: l,
        detail: format!("latency wanders by {} over the last seconds", ms(sd, 0)),
    })
}

fn find_loss(window: &[StatsSnapshot], ctx: &Ctx) -> Option<Finding> {
    if ctx.seconds <= 0.0
        || window.iter().any(|s| s.is_pyrowave())
        || !window.iter().any(|s| s.lost.is_some())
    {
        return None;
    }
    let rate = growth(window, |s| s.lost.unwrap_or(0)) / ctx.seconds;
    rate_finding(rate, LOST_FRAMES, |_| {
        format!("{rate:.1} frames lost per second")
    })
}

fn find_recovered(window: &[StatsSnapshot], ctx: &Ctx) -> Option<Finding> {
    if ctx.seconds <= 0.0 || !window.iter().any(|s| s.recovered.is_some()) {
        return None;
    }
    let rate = growth(window, |s| s.recovered.unwrap_or(0)) / ctx.seconds;
    rate_finding(rate, RECOVERED, |_| {
        format!("{rate:.1} frames/s rebuilt from parity")
    })
}

fn find_skipped(window: &[StatsSnapshot], ctx: &Ctx) -> Option<Finding> {
    if ctx.seconds <= 0.0
        || !window.iter().any(|s| s.is_pyrowave())
        || !window.iter().any(|s| s.lost.is_some())
    {
        return None;
    }
    let rate = growth(window, |s| s.lost.unwrap_or(0)) / ctx.seconds;
    rate_finding(rate / ctx.target, SKIPPED, |_| {
        format!("{rate:.1} frames/s skipped")
    })
}

fn find_partial(window: &[StatsSnapshot], ctx: &Ctx) -> Option<Finding> {
    if ctx.seconds <= 0.0 {
        return None;
    }
    let rate = growth(window, |s| s.partial) / ctx.seconds;
    rate_finding(rate, PARTIAL, |_| {
        format!("{rate:.1} frames/s shown with packets missing")
    })
}

fn find_rtt(window: &[StatsSnapshot], _: &Ctx) -> Option<Finding> {
    let v = values(window, |s| s.rtt_ms);
    let levels: Vec<f64> = v.iter().map(|d| level(Some(*d), RTT)).collect();
    let l = window_level(&levels);
    (l > 0.0).then(|| Finding {
        level: l,
        detail: format!("round trip {}", ms(mean(&v), 1)),
    })
}

fn find_awdl(window: &[StatsSnapshot], _: &Ctx) -> Option<Finding> {
    window.last().filter(|s| s.awdl_suspected).map(|_| Finding {
        level: MAJOR_LEVEL,
        detail: "frames arrive in bursts, typical of AWDL (AirDrop, Continuity)".into(),
    })
}

/// Each resource's level, with a phrase; the GPU's only while `encode_late`.
fn node_levels(n: &cha_client::NodeStats, encode_late: bool) -> Vec<(f64, String)> {
    let pct = |v: f64| format!("{v:.0}%");
    let f = |v: f32| f64::from(v);
    let ram = if n.mem_total > 0 {
        n.mem_used as f64 / n.mem_total as f64 * 100.0
    } else {
        f64::NAN
    };
    let vram = match (n.vram_used, n.vram_total) {
        (Some(used), Some(total)) if total > 0 => Some(used as f64 / total as f64 * 100.0),
        _ => None,
    };
    let streamer = if n.cores > 0 {
        f(n.streamer_cpu) / f64::from(n.cores)
    } else {
        f(n.streamer_cpu)
    };
    vec![
        (
            level(Some(f(n.cpu)), NODE_CPU),
            format!("CPU {}", pct(f(n.cpu))),
        ),
        (
            if ram.is_nan() {
                0.0
            } else {
                level(Some(ram), NODE_RAM)
            },
            format!("RAM {}", pct(ram)),
        ),
        (
            if encode_late {
                level(n.gpu.map(f), NODE_GPU)
            } else {
                0.0
            },
            format!("GPU {}", pct(n.gpu.map_or(0.0, f))),
        ),
        (
            level(vram, NODE_VRAM),
            format!("VRAM {}", pct(vram.unwrap_or(0.0))),
        ),
        (
            level(n.enc.map(f), NODE_ENC),
            format!("NVENC {}", pct(n.enc.map_or(0.0, f))),
        ),
        (
            level(Some(streamer), STREAMER_CPU),
            format!("streamer CPU {} of a core", pct(f(n.streamer_cpu))),
        ),
    ]
}

fn find_node(window: &[StatsSnapshot], ctx: &Ctx) -> Option<Finding> {
    let per: Vec<Vec<(f64, String)>> = window
        .iter()
        .filter_map(|s| {
            s.node.as_ref().map(|n| {
                let late = s
                    .encode_p99_ms
                    .is_some_and(|p| f64::from(p) > GPU_LATE_ENCODE * ctx.budget_ms);
                node_levels(n, late)
            })
        })
        .collect();
    if per.is_empty() {
        return None;
    }
    let worst_of = |p: &Vec<(f64, String)>| max(p.iter().map(|x| x.0));
    let levels: Vec<f64> = per.iter().map(worst_of).collect();
    let l = window_level(&levels);
    if l == 0.0 {
        return None;
    }
    // Name whatever was over in the worst reading.
    let worst = per.iter().fold(
        &per[0],
        |a, b| if worst_of(b) > worst_of(a) { b } else { a },
    );
    let detail = worst
        .iter()
        .filter(|x| x.0 > 0.0)
        .map(|x| x.1.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Some(Finding { level: l, detail })
}

const CHECKS: [Check; 13] = [
    Check {
        id: "stutter",
        title: "Stuttering picture",
        summary: "Some stutter",
        weight: weight::STUTTER,
        hint: "Frames are being sent but not all of them show. Check the other lines: if the network is fine, try 60 fps or another codec; if it isn't, a cable beats Wi-Fi.",
        find: find_stutter,
    },
    Check {
        id: "dropped",
        title: "Player dropping frames",
        summary: "Dropped frames",
        weight: weight::DROPPED,
        hint: "The player decoded frames it had no time to show. Close heavy apps on this Mac, or try 60 fps.",
        find: find_dropped,
    },
    Check {
        id: "decode",
        title: "Slow decoding",
        summary: "Slow decoding",
        weight: weight::DECODE,
        hint: "This Mac can't decode fast enough. Try 60 fps or another codec (HEVC and H.264 are hardware-decoded), and close other heavy apps.",
        find: find_decode,
    },
    Check {
        id: "freeze",
        title: "Picture freezes",
        summary: "Freezing",
        weight: weight::FREEZE,
        hint: "No frames arrived for a moment while the node was sending. Usually a network stall (Wi-Fi roaming or interference, a busy link) or a node that stopped to catch up; a cable is the first thing to try.",
        find: find_freeze,
    },
    Check {
        id: "latency",
        title: "High latency",
        summary: "High latency",
        weight: weight::LATENCY,
        hint: "Frames take long from arriving here to showing. This Mac is busy or the decoder is falling behind; close other apps, or lower the frame rate or bitrate.",
        find: find_latency,
    },
    Check {
        id: "jitter",
        title: "Uneven latency",
        summary: "Uneven latency",
        weight: weight::JITTER,
        hint: "Delivery time varies from frame to frame, which shows as judder. Queueing on the network (other traffic, Wi-Fi) is the usual cause.",
        find: find_jitter,
    },
    Check {
        id: "loss",
        title: "Packet loss",
        summary: "Network losses",
        weight: weight::LOSS,
        hint: "The network is dropping packets, which costs frames or forces resends. Wi-Fi interference or a congested link: try a cable, or move closer to the access point.",
        find: find_loss,
    },
    Check {
        id: "recovered",
        title: "Network needs repair",
        summary: "Lossy network",
        weight: weight::RECOVERED,
        hint: "Error correction is rebuilding frames that lost packets, so you see nothing yet, but the link is dropping data and a worse moment would show. Wi-Fi or a busy link: try a cable.",
        find: find_recovered,
    },
    Check {
        id: "skipped",
        title: "Frames skipped",
        summary: "Skipped frames",
        weight: weight::SKIPPED,
        hint: "PyroWave frames that lost a packet and were overtaken by the next whole one are skipped: each costs one frame, and the next shows whole. Many of them read as stutter. The link is dropping data, often because it is nearly full: 4:2:0 or 60 fps leaves room, and a cable beats Wi-Fi.",
        find: find_skipped,
    },
    Check {
        id: "partial",
        title: "Frames shown incomplete",
        summary: "Lossy network",
        weight: weight::PARTIAL,
        hint: "PyroWave shows a frame from the packets that arrived, so a lost packet softens a few blocks for one frame instead of breaking the picture. The link is dropping data, often because it is nearly full: 4:2:0 or 60 fps leaves room, and a cable beats Wi-Fi.",
        find: find_partial,
    },
    Check {
        id: "rtt",
        title: "Slow network round trip",
        summary: "Slow network",
        weight: weight::RTT,
        hint: "The round trip to the node is long for a LAN. Check you are on the same network as the node, and for a VPN or Wi-Fi in the way.",
        find: find_rtt,
    },
    Check {
        id: "node",
        title: "Node overloaded",
        summary: "Node overloaded",
        weight: weight::NODE,
        hint: "The machine running the environment is out of something, so frames are made late. Close other apps on it, lower the frame rate, or choose another device.",
        find: find_node,
    },
    Check {
        id: "awdl",
        title: "Wi-Fi latency spikes",
        summary: "Wi-Fi spikes",
        weight: weight::AWDL,
        hint: "Likely AWDL (AirDrop/Continuity) taking turns with Wi-Fi. A cable avoids it; see the README.",
        find: find_awdl,
    },
];

/// Grades the stream from `history` (oldest first, one snapshot per second;
/// only the last `HEALTH_WINDOW` count). Fields a snapshot lacks (a transport
/// that can't say) are skipped, never penalised.
pub fn assess(history: &[StatsSnapshot]) -> Assessment {
    let window = &history[history.len().saturating_sub(HEALTH_WINDOW)..];
    if window.len() < MIN_SNAPSHOTS {
        return Assessment::default();
    }
    let latest = &window[window.len() - 1];
    let target = latest
        .target_fps
        .filter(|f| *f > 0)
        .map_or(FALLBACK_FPS, f64::from);
    let ctx = Ctx {
        target,
        budget_ms: 1000.0 / target,
        seconds: (window.len() - 1) as f64,
    };

    struct Found {
        issue: Issue,
        penalty: f64,
        summary: &'static str,
    }
    let mut found: Vec<Found> = Vec::new();
    for check in &CHECKS {
        let Some(f) = (check.find)(window, &ctx) else {
            continue;
        };
        if f.level < MIN_LEVEL {
            continue;
        }
        found.push(Found {
            issue: Issue {
                id: check.id,
                severity: Severity::of(f.level),
                title: check.title,
                detail: f.detail,
                hint: check.hint,
            },
            penalty: check.weight * f.level,
            summary: check.summary,
        });
    }
    found.sort_by(|a, b| {
        b.issue
            .severity
            .rank()
            .cmp(&a.issue.severity.rank())
            .then(b.penalty.total_cmp(&a.penalty))
    });

    let mut score = 100.0 - found.iter().map(|f| f.penalty).sum::<f64>();
    for f in &found {
        score = score.min(f.issue.severity.cap());
    }
    let score = score.max(0.0).round();
    let grade = GRADE_FLOORS
        .iter()
        .find(|(_, floor)| score >= *floor)
        .map(|(g, _)| *g);
    Assessment {
        grade,
        score: Some(score as u32),
        summary: found.first().map_or("Smooth", |f| f.summary),
        issues: found.into_iter().map(|f| f.issue).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cha_client::NodeStats;

    fn healthy_node() -> NodeStats {
        NodeStats {
            cpu: 30.0,
            cores: 16,
            load1: 2.0,
            mem_used: 8,
            mem_total: 32,
            gpu: Some(50.0),
            vram_used: Some(4),
            vram_total: Some(12),
            enc: Some(20.0),
            dec: Some(0.0),
            streamer_cpu: 40.0,
            ..NodeStats::default()
        }
    }

    /// A healthy 60 fps WebTransport LAN stream.
    fn snap() -> StatsSnapshot {
        StatsSnapshot {
            codec: "Hevc".into(),
            width: 2560,
            height: 1440,
            present_fps: 60.0,
            target_fps: Some(60),
            sent_fps: Some(60.0),
            shown_sent_fps: None,
            mbps: Some(60.0),
            decode_ms: Some(3.0),
            rtt_ms: Some(2.0),
            lost: Some(0),
            recovered: Some(0),
            latency_ms: Some(12.0),
            frame_gap_ms: Some(20.0),
            node: Some(healthy_node()),
            ..StatsSnapshot::default()
        }
    }

    fn run(n: usize, f: impl Fn(usize, &mut StatsSnapshot)) -> Vec<StatsSnapshot> {
        (0..n)
            .map(|i| {
                let mut s = snap();
                f(i, &mut s);
                s
            })
            .collect()
    }

    fn ids(a: &Assessment) -> Vec<&'static str> {
        a.issues.iter().map(|i| i.id).collect()
    }

    fn issue<'a>(a: &'a Assessment, id: &str) -> &'a Issue {
        a.issues.iter().find(|i| i.id == id).unwrap()
    }

    #[test]
    fn a_healthy_stream_is_an_a_with_no_issues() {
        let h = assess(&run(8, |_, _| {}));
        assert_eq!(h.grade, Some(Grade::A));
        assert_eq!(h.score, Some(100));
        assert_eq!(h.summary, "Smooth");
        assert!(h.issues.is_empty());
    }

    #[test]
    fn too_few_snapshots_are_not_graded() {
        assert_eq!(assess(&[]).grade, None);
        assert_eq!(assess(&run(1, |_, _| {})).grade, None);
        assert_eq!(assess(&run(1, |_, _| {})).summary, "Measuring…");
        assert_eq!(assess(&run(2, |_, _| {})).grade, Some(Grade::A));
    }

    #[test]
    fn only_the_last_window_counts() {
        let mut history = run(20, |_, s| s.present_fps = 5.0);
        history.extend(run(HEALTH_WINDOW, |_, _| {}));
        assert_eq!(assess(&history).grade, Some(Grade::A));
    }

    #[test]
    fn sustained_half_rate_is_a_major_stutter() {
        let h = assess(&run(8, |_, s| s.present_fps = 30.0));
        assert!(ids(&h).contains(&"stutter"));
        assert!(
            issue(&h, "stutter")
                .detail
                .contains("30 fps shown of 60 sent")
        );
        assert!(matches!(h.grade, Some(Grade::D | Grade::F)));
    }

    #[test]
    fn a_mild_shortfall_is_minor_and_costs_an_a() {
        let h = assess(&run(8, |_, s| s.present_fps = 56.0));
        assert_eq!(
            h.issues.iter().map(|i| i.severity).collect::<Vec<_>>(),
            [Severity::Minor]
        );
        assert_eq!(h.grade, Some(Grade::B));
    }

    #[test]
    fn a_static_desktop_is_an_a_not_a_stutter_or_freeze() {
        let h = assess(&run(8, |_, s| {
            s.present_fps = 1.0;
            s.sent_fps = Some(0.5);
            s.frame_gap_ms = Some(1000.0);
            s.mbps = Some(0.01);
        }));
        assert_eq!(h.grade, Some(Grade::A));
        assert!(h.issues.is_empty());
    }

    #[test]
    fn an_unknown_send_rate_judges_neither_stutter_nor_freeze() {
        let h = assess(&run(8, |_, s| {
            s.present_fps = 5.0;
            s.sent_fps = None;
            s.frame_gap_ms = Some(900.0);
        }));
        assert_eq!(h.grade, Some(Grade::A));
    }

    #[test]
    fn a_burst_then_a_still_screen_is_smooth() {
        let h = assess(&run(8, |_, s| {
            s.present_fps = 0.0;
            s.sent_fps = Some(12.0);
            s.shown_sent_fps = Some(12.0);
            s.frame_gap_ms = Some(4.0);
        }));
        assert_eq!(h.grade, Some(Grade::A));
        assert!(h.issues.is_empty());
    }

    #[test]
    fn dropped_frames_come_from_the_counters_growth() {
        let h = assess(&run(8, |i, s| s.dropped = 1000 + i as u64 * 6));
        assert_eq!(ids(&h), ["dropped"]);
        assert_eq!(h.grade, Some(Grade::C));
        // An old total of drops is not a current problem.
        assert_eq!(
            assess(&run(8, |_, s| s.dropped = 5000)).grade,
            Some(Grade::A)
        );
        // A counter that resets (a reconnect) counts as no growth.
        let h = assess(&run(8, |i, s| {
            s.dropped = if i < 4 { 900 + i as u64 } else { 2 };
        }));
        assert!(h.issues.is_empty());
    }

    #[test]
    fn decoding_over_the_budget_is_critical() {
        let h = assess(&run(8, |_, s| s.decode_ms = Some(20.0)));
        assert_eq!(issue(&h, "decode").severity, Severity::Critical);
        assert!(issue(&h, "decode").hint.contains("60 fps"));
        assert!(matches!(h.grade, Some(Grade::D | Grade::F)));
    }

    #[test]
    fn the_decode_budget_scales_with_the_frame_rate() {
        assert_eq!(
            assess(&run(8, |_, s| s.decode_ms = Some(9.0))).grade,
            Some(Grade::B)
        );
        let h = assess(&run(8, |_, s| {
            s.decode_ms = Some(6.0);
            s.target_fps = Some(120);
            s.sent_fps = Some(120.0);
            s.present_fps = 120.0;
        }));
        assert!(ids(&h).contains(&"decode"));
        // A still picture's few slow keyframes hold nothing up.
        let still = assess(&run(8, |_, s| {
            s.decode_ms = Some(16.0);
            s.sent_fps = Some(0.5);
            s.present_fps = 0.0;
        }));
        assert!(!ids(&still).contains(&"decode"));
        assert_eq!(still.grade, Some(Grade::A));
    }

    #[test]
    fn latency_bands() {
        let sev = |l: f32| {
            assess(&run(8, |_, s| s.latency_ms = Some(l)))
                .issues
                .iter()
                .find(|i| i.id == "latency")
                .map(|i| i.severity)
        };
        assert_eq!(sev(19.0), None);
        assert_eq!(sev(25.0), Some(Severity::Minor));
        assert_eq!(sev(35.0), Some(Severity::Minor));
        assert_eq!(sev(60.0), Some(Severity::Major));
        assert_eq!(sev(110.0), Some(Severity::Critical));
        assert_eq!(
            assess(&run(8, |_, s| s.latency_ms = Some(60.0))).grade,
            Some(Grade::C)
        );
    }

    #[test]
    fn latency_wandering_is_uneven_latency() {
        let h = assess(&run(8, |i, s| {
            s.latency_ms = Some(if i % 2 == 1 { 40.0 } else { 10.0 });
        }));
        assert!(ids(&h).contains(&"jitter"));
    }

    #[test]
    fn lost_frames_are_judged_and_say_what_to_try() {
        let h = assess(&run(8, |i, s| s.lost = Some(i as u64 * 3)));
        assert!(issue(&h, "loss").detail.contains("frames lost"));
        assert_ne!(h.grade, Some(Grade::A));
        assert!(issue(&h, "loss").hint.contains("cable"));
        // A transport that can't count losses isn't penalised.
        let h = assess(&run(8, |_, s| {
            s.lost = None;
            s.recovered = None;
        }));
        assert_eq!(h.grade, Some(Grade::A));
    }

    #[test]
    fn repairs_are_a_minor_note_not_a_failure() {
        let h = assess(&run(8, |i, s| s.recovered = Some(i as u64 * 8)));
        assert_eq!(ids(&h), ["recovered"]);
        assert_ne!(h.issues[0].severity, Severity::Critical);
        assert!(matches!(h.grade, Some(Grade::B | Grade::C)));
    }

    #[test]
    fn pyrowave_skipped_frames_are_a_share_of_the_rate() {
        let pyro = |s: &mut StatsSnapshot| {
            s.codec = "PyroWave444".into();
            s.target_fps = Some(120);
            s.sent_fps = Some(120.0);
            s.present_fps = 120.0;
        };
        // 1 a second at 120 fps: under 1 %, nothing to say.
        let h = assess(&run(8, |i, s| {
            pyro(s);
            s.lost = Some(i as u64);
        }));
        assert_eq!(h.grade, Some(Grade::A));
        // 6 a second (5 %): skipped frames, not "Packet loss".
        let h = assess(&run(8, |i, s| {
            pyro(s);
            s.lost = Some(i as u64 * 6);
        }));
        assert_eq!(ids(&h), ["skipped"]);
        assert!(h.issues[0].detail.contains("frames/s skipped"));
    }

    #[test]
    fn pyrowave_incomplete_frames_are_a_minor_note() {
        let h = assess(&run(8, |i, s| s.partial = (i as f64 * 2.4).round() as u64));
        assert_eq!(ids(&h), ["partial"]);
        assert_eq!(h.issues[0].severity, Severity::Minor);
        assert!(h.issues[0].detail.contains("shown with packets missing"));
        assert_eq!(h.grade, Some(Grade::B));
    }

    #[test]
    fn a_long_round_trip() {
        let h = assess(&run(8, |_, s| s.rtt_ms = Some(50.0)));
        assert_eq!(ids(&h), ["rtt"]);
    }

    #[test]
    fn a_long_gap_between_frames_is_a_freeze() {
        let h = assess(&run(8, |i, s| {
            s.frame_gap_ms = Some(if i == 6 { 600.0 } else { 20.0 });
        }));
        assert!(ids(&h).contains(&"freeze"));
        assert_ne!(h.grade, Some(Grade::A));
        let h = assess(&run(8, |_, s| {
            s.frame_gap_ms = Some(800.0);
            s.present_fps = 20.0;
        }));
        assert_eq!(h.grade, Some(Grade::F));
        assert_eq!(h.issues[0].severity, Severity::Critical);
        // The same gap while little was being sent is not one.
        let h = assess(&run(8, |i, s| {
            s.sent_fps = Some(2.0);
            s.present_fps = 2.0;
            s.frame_gap_ms = Some(if i == 6 { 400.0 } else { 20.0 });
        }));
        assert!(!ids(&h).contains(&"freeze"));
        // A gap of a couple of frames is normal.
        assert_eq!(
            assess(&run(8, |_, s| s.frame_gap_ms = Some(30.0))).grade,
            Some(Grade::A)
        );
    }

    #[test]
    fn no_frames_shown_while_sending_is_a_freeze_without_a_gap_measure() {
        let h = assess(&run(8, |_, s| {
            s.frame_gap_ms = None;
            s.present_fps = 0.0;
        }));
        assert!(ids(&h).contains(&"freeze"));
    }

    #[test]
    fn node_saturation_is_named() {
        type Tweak = fn(&mut NodeStats);
        let cases: [(&str, Tweak); 5] = [
            ("CPU", |n| n.cpu = 97.0),
            ("VRAM", |n| n.vram_used = Some(12)),
            ("NVENC", |n| n.enc = Some(99.0)),
            ("streamer CPU", |n| n.streamer_cpu = 1500.0),
            ("RAM", |n| n.mem_used = 31),
        ];
        for (name, tweak) in cases {
            let h = assess(&run(8, |_, s| tweak(s.node.as_mut().unwrap())));
            assert_eq!(ids(&h), ["node"], "{name}");
            assert!(
                h.issues[0].detail.contains(name),
                "{name}: {}",
                h.issues[0].detail
            );
            assert_ne!(h.grade, Some(Grade::A));
        }
    }

    #[test]
    fn a_full_gpu_counts_only_while_the_streamer_encodes_late() {
        let gpu = |gpu: f32, p99: Option<f32>| {
            assess(&run(8, |_, s| {
                s.node.as_mut().unwrap().gpu = Some(gpu);
                s.encode_p99_ms = p99;
            }))
        };
        assert_eq!(gpu(100.0, Some(2.0)).grade, Some(Grade::A));
        assert_eq!(gpu(100.0, None).grade, Some(Grade::A));
        let h = gpu(99.0, Some(12.0));
        assert_eq!(ids(&h), ["node"]);
        assert!(h.issues[0].detail.contains("GPU"));
    }

    #[test]
    fn busy_but_under_the_limits_and_no_report_are_fine() {
        let h = assess(&run(8, |_, s| {
            let n = s.node.as_mut().unwrap();
            n.cpu = 80.0;
            n.gpu = Some(90.0);
            n.enc = Some(80.0);
        }));
        assert_eq!(h.grade, Some(Grade::A));
        assert_eq!(assess(&run(8, |_, s| s.node = None)).grade, Some(Grade::A));
    }

    #[test]
    fn a_cpu_only_node_has_no_gpu_fields() {
        let h = assess(&run(8, |_, s| {
            let n = s.node.as_mut().unwrap();
            n.gpu = None;
            n.vram_used = None;
            n.vram_total = None;
            n.enc = None;
            n.cpu = 96.0;
        }));
        assert_eq!(h.issues[0].detail, "CPU 96%");
    }

    #[test]
    fn awdl_is_listed_like_the_others() {
        let h = assess(&run(8, |i, s| s.awdl_suspected = i == 7));
        assert_eq!(ids(&h), ["awdl"]);
        assert_eq!(h.issues[0].title, "Wi-Fi latency spikes");
        assert_eq!(h.issues[0].severity, Severity::Major);
    }

    #[test]
    fn one_bad_second_does_not_flash_an_f_and_sustained_is_worse() {
        let h = assess(&run(8, |i, s| {
            if i == 5 {
                s.present_fps = 10.0;
                s.frame_gap_ms = Some(400.0);
            }
        }));
        assert_ne!(h.grade, Some(Grade::F));
        assert_ne!(h.grade, Some(Grade::A));
        assert!(h.issues.iter().all(|i| i.severity != Severity::Critical));

        let spike = assess(&run(8, |i, s| {
            if i == 5 {
                s.latency_ms = Some(120.0);
            }
        }));
        let steady = assess(&run(8, |_, s| s.latency_ms = Some(120.0)));
        assert!(steady.score.unwrap() < spike.score.unwrap());
        // A spike that has aged out of the window is forgotten.
        let mut history = run(1, |_, s| s.latency_ms = Some(200.0));
        history.extend(run(HEALTH_WINDOW, |_, _| {}));
        assert_eq!(assess(&history).grade, Some(Grade::A));
    }

    #[test]
    fn several_problems_sum_most_severe_first_and_the_summary_names_the_worst() {
        let h = assess(&run(8, |i, s| {
            s.present_fps = 35.0;
            s.decode_ms = Some(25.0);
            s.latency_ms = Some(90.0);
            s.lost = Some(i as u64 * 4);
            s.node.as_mut().unwrap().cpu = 99.0;
        }));
        assert_eq!(h.grade, Some(Grade::F));
        assert!(h.score.unwrap() < 55);
        assert!(h.issues.len() >= 4);
        let ranks: Vec<u8> = h.issues.iter().map(|i| i.severity.rank()).collect();
        assert!(ranks.windows(2).all(|w| w[0] >= w[1]));

        assert_eq!(
            assess(&run(8, |_, s| s.decode_ms = Some(20.0))).summary,
            "Slow decoding"
        );
        assert_eq!(
            assess(&run(8, |_, s| s.node.as_mut().unwrap().cpu = 99.0)).summary,
            "Node overloaded"
        );
    }

    #[test]
    fn everything_unknown_is_not_penalised() {
        let h = assess(&run(8, |_, s| {
            *s = StatsSnapshot {
                width: 2560,
                height: 1440,
                ..StatsSnapshot::default()
            };
        }));
        assert_eq!(h.grade, Some(Grade::A));
        assert_eq!(h.score, Some(100));
    }

    #[test]
    fn nan_fields_do_not_crash_or_penalise() {
        let h = assess(&run(8, |_, s| {
            s.decode_ms = Some(f32::NAN);
            s.latency_ms = Some(f32::NAN);
            s.rtt_ms = Some(f32::NAN);
        }));
        assert_eq!(h.grade, Some(Grade::A));
    }
}
