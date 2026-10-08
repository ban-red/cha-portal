//! A letter grade for the stream right now, from the last few seconds of
//! stats snapshots. It is `web/packages/player/src/health.ts` in Rust.
//!
//! The numbers, weights and words (thresholds, bands, titles, hints, summary
//! words, detail templates) are not here: they are
//! `web/packages/ui-spec/health.json`, read through `cha_ui_spec::health`,
//! and shared with the browser player. This file holds the checks, keyed by
//! issue id. The shared cases in `health-cases.json` run against both
//! implementations (`cargo test -p cha-player health`, `bun run --cwd
//! web/packages/player test`), so the two judge a stream alike; where this
//! player measures a thing differently the spec words it per platform.
//!
//! Checks here: stutter, dropped frames, decoding, freezes, latency, uneven
//! latency (the latency spread only), packet loss, repairs, skipped frames
//! (PyroWave), incomplete frames (PyroWave), round trip, node overload, and
//! `awdl`, which has no browser counterpart (Wi-Fi latency spikes).
//!
//! The spec's `platforms` field says what is left to the browser, because
//! this player can't measure the signal:
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

use cha_ui_spec::health::{Band, HealthSpec, Platform, Val, fill, spec};

use crate::ui::StatsSnapshot;

/// This player's side of the spec.
const PLATFORM: Platform = Platform::Native;

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

    fn from_letter(letter: &str) -> Grade {
        match letter {
            "A" => Grade::A,
            "B" => Grade::B,
            "C" => Grade::C,
            "D" => Grade::D,
            "F" => Grade::F,
            other => panic!("health.json: unknown grade {other:?}"),
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
        let cap = &spec().severity_cap;
        match self {
            Severity::Minor => cap.minor,
            Severity::Major => cap.major,
            Severity::Critical => cap.critical,
        }
    }

    fn of(level: f64) -> Self {
        let levels = &spec().levels;
        if level >= levels.critical {
            Severity::Critical
        } else if level >= levels.major {
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
            summary: spec().text.measuring.as_str(),
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

/// Snapshots judged: the last few seconds.
pub fn health_window() -> usize {
    spec().window
}

/// 0 below `from`, then `base` at `from` rising linearly to `base + slope` at `to`.
fn level(v: Option<f64>, band: Band) -> f64 {
    let levels = &spec().levels;
    match v {
        Some(v) if v.is_finite() && v >= band.from => {
            levels.base + levels.slope * ((v - band.from) / (band.to - band.from)).min(1.0)
        }
        _ => 0.0,
    }
}

/// The same for a signal where lower is worse (`from` above `to`).
fn level_below(v: Option<f64>, band: Band) -> f64 {
    let levels = &spec().levels;
    match v {
        Some(v) if v.is_finite() && v <= band.from => {
            levels.base + levels.slope * ((band.from - v) / (band.from - band.to)).min(1.0)
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
    mean(levels).max(spec().levels.spike_share * max(levels.iter().copied()))
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

fn rate_finding(rate: f64, band: Band, detail: impl Fn() -> String) -> Option<Finding> {
    let l = level(Some(rate), band);
    (l > 0.0).then(|| Finding {
        level: l,
        detail: detail(),
    })
}

/// The detail text of an issue's variant on this platform, filled in.
fn detail(id: &str, variant: &str, values: &[(&str, Val)]) -> String {
    let issue = spec().issue(id).expect("an issue in health.json");
    fill(issue.detail(variant, PLATFORM), values)
}

/// Frames per second shown over the send rate's span, or the last second's.
fn shown_of_sent(s: &StatsSnapshot) -> f64 {
    f64::from(s.shown_sent_fps.unwrap_or(s.present_fps))
}

type Find = fn(&[StatsSnapshot], &Ctx) -> Option<Finding>;

/// The send rate of a snapshot of a source sending at a rate, so stutter and
/// freezes can be judged.
fn busy(s: &StatsSnapshot) -> Option<f64> {
    s.sent_fps
        .map(f64::from)
        .filter(|sent| *sent >= spec().params.busy_sent_fps)
}

fn find_stutter(window: &[StatsSnapshot], _: &Ctx) -> Option<Finding> {
    let busy_ones: Vec<(f64, f64)> = window
        .iter()
        .filter_map(|s| busy(s).map(|sent| (shown_of_sent(s), sent)))
        .collect();
    let levels: Vec<f64> = busy_ones
        .iter()
        .map(|(shown, sent)| level_below(Some(shown / sent), spec().bands.fps_ratio))
        .collect();
    let l = window_level(&levels);
    if l == 0.0 {
        return None;
    }
    let shown: Vec<f64> = busy_ones.iter().map(|b| b.0).collect();
    let sent: Vec<f64> = busy_ones.iter().map(|b| b.1).collect();
    Some(Finding {
        level: l,
        detail: detail(
            "stutter",
            "main",
            &[
                ("shown", Val::Num(mean(&shown))),
                ("sent", Val::Num(mean(&sent))),
            ],
        ),
    })
}

fn find_dropped(window: &[StatsSnapshot], ctx: &Ctx) -> Option<Finding> {
    if ctx.seconds <= 0.0 {
        return None;
    }
    let rate = growth(window, |s| s.dropped) / ctx.seconds;
    rate_finding(rate / ctx.target, spec().bands.dropped, || {
        detail("dropped", "main", &[("rate", Val::Num(rate))])
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
        .map(|d| level(Some(d / ctx.budget_ms), spec().bands.decode_budget))
        .collect();
    let l = window_level(&levels);
    if l == 0.0 {
        return None;
    }
    Some(Finding {
        level: l,
        detail: detail(
            "decode",
            "main",
            &[
                ("decode", Val::Num(mean(&v))),
                ("budget", Val::Num(ctx.budget_ms)),
            ],
        ),
    })
}

fn find_freeze(window: &[StatsSnapshot], ctx: &Ctx) -> Option<Finding> {
    let p = &spec().params;
    let mut levels = Vec::new();
    let mut worst = 0.0f64;
    for s in window {
        // Only while frames were being sent: a still picture has long gaps by
        // nature. A source sending slowly has a long time between frames, so
        // the gap must also beat that many send intervals.
        let Some(sent) = busy(s) else { continue };
        let gap = match s.frame_gap_ms {
            Some(g) => f64::from(g),
            // A second with no frames at all is a gap.
            None if shown_of_sent(s) == 0.0 => p.no_frames_gap_ms,
            None => continue,
        };
        let from = p
            .freeze_min_ms
            .max(p.freeze_budgets * ctx.budget_ms)
            .max(p.freeze_budgets * 1000.0 / sent);
        levels.push(level(
            Some(gap),
            Band {
                from,
                to: p.freeze_to_ms,
            },
        ));
        worst = worst.max(gap);
    }
    let l = window_level(&levels);
    (l > 0.0).then(|| Finding {
        level: l,
        detail: detail("freeze", "main", &[("gap", Val::Num(worst))]),
    })
}

fn find_latency(window: &[StatsSnapshot], _: &Ctx) -> Option<Finding> {
    let v = values(window, |s| s.latency_ms);
    let levels: Vec<f64> = v
        .iter()
        .map(|d| level(Some(*d), spec().bands.latency))
        .collect();
    let l = window_level(&levels);
    (l > 0.0).then(|| Finding {
        level: l,
        detail: detail("latency", "main", &[("latency", Val::Num(mean(&v)))]),
    })
}

fn find_jitter(window: &[StatsSnapshot], _: &Ctx) -> Option<Finding> {
    let lat = values(window, |s| s.latency_ms);
    if lat.len() < spec().params.latency_spread_samples {
        return None;
    }
    let sd = interquartile(&lat);
    let l = level(Some(sd), spec().bands.latency_spread);
    (l > 0.0).then(|| Finding {
        level: l,
        detail: detail("jitter", "latency", &[("spread", Val::Num(sd))]),
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
    rate_finding(rate, spec().bands.lost_frames, || {
        detail("loss", "main", &[("rate", Val::Num(rate))])
    })
}

fn find_recovered(window: &[StatsSnapshot], ctx: &Ctx) -> Option<Finding> {
    if ctx.seconds <= 0.0 || !window.iter().any(|s| s.recovered.is_some()) {
        return None;
    }
    let rate = growth(window, |s| s.recovered.unwrap_or(0)) / ctx.seconds;
    rate_finding(rate, spec().bands.recovered, || {
        detail("recovered", "main", &[("rate", Val::Num(rate))])
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
    rate_finding(rate / ctx.target, spec().bands.skipped, || {
        detail("skipped", "main", &[("rate", Val::Num(rate))])
    })
}

fn find_partial(window: &[StatsSnapshot], ctx: &Ctx) -> Option<Finding> {
    if ctx.seconds <= 0.0 {
        return None;
    }
    let rate = growth(window, |s| s.partial) / ctx.seconds;
    rate_finding(rate, spec().bands.partial, || {
        detail("partial", "main", &[("rate", Val::Num(rate))])
    })
}

fn find_rtt(window: &[StatsSnapshot], _: &Ctx) -> Option<Finding> {
    let v = values(window, |s| s.rtt_ms);
    let levels: Vec<f64> = v
        .iter()
        .map(|d| level(Some(*d), spec().bands.rtt))
        .collect();
    let l = window_level(&levels);
    (l > 0.0).then(|| Finding {
        level: l,
        detail: detail("rtt", "main", &[("rtt", Val::Num(mean(&v)))]),
    })
}

fn find_awdl(window: &[StatsSnapshot], _: &Ctx) -> Option<Finding> {
    window.last().filter(|s| s.awdl_suspected).map(|_| Finding {
        level: spec().levels.major,
        detail: detail("awdl", "main", &[]),
    })
}

/// Each resource's level, with a phrase; the GPU's only while `encode_late`.
fn node_levels(n: &cha_client::NodeStats, encode_late: bool) -> Vec<(f64, String)> {
    let b = &spec().bands;
    let text = |variant: &str, pct: f64| detail("node", variant, &[("pct", Val::Num(pct))]);
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
        (level(Some(f(n.cpu)), b.node_cpu), text("cpu", f(n.cpu))),
        (
            if ram.is_nan() {
                0.0
            } else {
                level(Some(ram), b.node_ram)
            },
            text("ram", ram),
        ),
        (
            if encode_late {
                level(n.gpu.map(f), b.node_gpu)
            } else {
                0.0
            },
            text("gpu", n.gpu.map_or(0.0, f)),
        ),
        (level(vram, b.node_vram), text("vram", vram.unwrap_or(0.0))),
        (
            level(n.enc.map(f), b.node_enc),
            text("enc", n.enc.map_or(0.0, f)),
        ),
        (
            level(Some(streamer), b.streamer_cpu),
            text("streamer", f(n.streamer_cpu)),
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
                    .is_some_and(|p| f64::from(p) > spec().params.gpu_late_encode * ctx.budget_ms);
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
    let parts = worst
        .iter()
        .filter(|x| x.0 > 0.0)
        .map(|x| x.1.as_str())
        .collect::<Vec<_>>()
        .join(&spec().text.list_separator);
    Some(Finding {
        level: l,
        detail: detail("node", "main", &[("parts", Val::Str(parts))]),
    })
}

/// The checks, by issue id. Titles, hints, weights and the words of a detail
/// are the spec's.
const CHECKS: [(&str, Find); 13] = [
    ("stutter", find_stutter),
    ("dropped", find_dropped),
    ("decode", find_decode),
    ("freeze", find_freeze),
    ("latency", find_latency),
    ("jitter", find_jitter),
    ("loss", find_loss),
    ("recovered", find_recovered),
    ("skipped", find_skipped),
    ("partial", find_partial),
    ("rtt", find_rtt),
    ("node", find_node),
    ("awdl", find_awdl),
];

/// Grades the stream from `history` (oldest first, one snapshot per second;
/// only the last `health_window()` count). Fields a snapshot lacks (a transport
/// that can't say) are skipped, never penalised.
pub fn assess(history: &[StatsSnapshot]) -> Assessment {
    let spec: &'static HealthSpec = spec();
    let window = &history[history.len().saturating_sub(spec.window)..];
    if window.len() < spec.min_snapshots {
        return Assessment::default();
    }
    let latest = &window[window.len() - 1];
    let target = latest
        .target_fps
        .filter(|f| *f > 0)
        .map_or(spec.fallback_fps, f64::from);
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
    for issue in spec.issues_for(PLATFORM) {
        let Some((_, find)) = CHECKS.iter().find(|(id, _)| *id == issue.id) else {
            continue;
        };
        let Some(f) = find(window, &ctx) else {
            continue;
        };
        if f.level < spec.levels.min {
            continue;
        }
        found.push(Found {
            issue: Issue {
                id: issue.id.as_str(),
                severity: Severity::of(f.level),
                title: issue.title(PLATFORM),
                detail: f.detail,
                hint: issue.hint(PLATFORM),
            },
            penalty: issue.weight * f.level,
            summary: issue.summary.as_str(),
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
    let grade = spec
        .grades
        .iter()
        .find(|g| score >= g.floor)
        .map(|g| Grade::from_letter(&g.grade));
    Assessment {
        grade,
        score: Some(score as u32),
        summary: found
            .first()
            .map_or(spec.text.smooth.as_str(), |f| f.summary),
        issues: found.into_iter().map(|f| f.issue).collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use cha_client::NodeStats;
    use cha_ui_spec::health::PlatformText;
    use cha_ui_spec::health_cases::{SpecNode, SpecSnapshot, health_cases};

    use super::*;

    // The shared cases (web/packages/ui-spec/health-cases.json) run here and in
    // web/packages/player/src/health.test.ts. What stays in this file is what a
    // case can't say: the mapping and the spec's coverage.

    fn node_from_spec(n: &SpecNode) -> NodeStats {
        let get = |v: Option<f64>| v.unwrap_or(0.0);
        NodeStats {
            cpu: get(n.cpu) as f32,
            cores: get(n.cores) as u32,
            mem_used: get(n.mem_used) as u64,
            mem_total: get(n.mem_total) as u64,
            gpu: n.gpu.map(|v| v as f32),
            vram_used: n.vram_used.map(|v| v as u64),
            vram_total: n.vram_total.map(|v| v as u64),
            enc: n.enc.map(|v| v as f32),
            streamer_cpu: get(n.streamer_cpu) as f32,
            ..NodeStats::default()
        }
    }

    /// A neutral snapshot as this player's own. Fields it can't measure (the
    /// browser's delivery times, jitter buffer and sound) are dropped.
    fn from_snapshot(s: &SpecSnapshot) -> StatsSnapshot {
        let f32_of = |v: Option<f64>| v.map(|v| v as f32);
        let count = |v: Option<f64>| v.map(|v| v.round() as u64);
        StatsSnapshot {
            // "hevc" is "Hevc" and "pyrowave444" is "PyroWave444", as the core names them.
            codec: s.codec.as_deref().map_or_else(String::new, |c| {
                match c.strip_prefix("pyrowave") {
                    Some(rest) => format!("PyroWave{rest}"),
                    None => c[..1].to_uppercase() + &c[1..],
                }
            }),
            present_fps: f32_of(s.shown_fps).unwrap_or(0.0),
            target_fps: s.target_fps.map(|v| v as u32),
            sent_fps: f32_of(s.sent_fps),
            shown_sent_fps: f32_of(s.shown_sent_fps),
            encode_p99_ms: f32_of(s.encode_p99_ms),
            decode_ms: f32_of(s.decode_ms),
            latency_ms: f32_of(s.latency_ms),
            frame_gap_ms: f32_of(s.frame_gap_ms),
            rtt_ms: f32_of(s.rtt_ms),
            lost: count(s.lost),
            recovered: count(s.recovered),
            dropped: count(s.dropped).unwrap_or(0),
            partial: count(s.partial).unwrap_or(0),
            node: s.node.as_ref().map(node_from_spec),
            awdl_suspected: s.awdl_suspected.unwrap_or(false),
            ..StatsSnapshot::default()
        }
    }

    fn severity_name(s: Severity) -> &'static str {
        match s {
            Severity::Minor => "minor",
            Severity::Major => "major",
            Severity::Critical => "critical",
        }
    }

    /// Issue id to text; None for an issue the assessment lacks.
    type Texts = BTreeMap<String, Option<String>>;

    /// What a case checks, from an assessment or from the expectation.
    #[derive(Debug, PartialEq)]
    struct Outcome {
        grade: Option<String>,
        score: Option<u32>,
        summary: String,
        issues: Vec<(String, String)>,
        details: Option<Texts>,
        hints: Option<Texts>,
    }

    #[test]
    fn the_shared_cases() {
        let all = health_cases();
        let mut ran = 0;
        let mut failures = Vec::new();
        for case in all.cases.iter().filter(|c| c.runs_on(PLATFORM)) {
            ran += 1;
            let ctx = case.context.clone().unwrap_or_default();
            assert!(
                ctx.visible.unwrap_or(true) && ctx.interval_ms.is_none_or(|i| i == 1000.0),
                "{}: only the browser has a hidden page or another interval; give the case platforms: [\"web\"]",
                case.name
            );
            let history: Vec<StatsSnapshot> = all.history(case).iter().map(from_snapshot).collect();
            let h = assess(&history);
            let e = &case.expect;
            let texts = |want: &Option<BTreeMap<String, PlatformText>>,
                         pick: fn(&Issue) -> String|
             -> (Option<Texts>, Option<Texts>) {
                let Some(want) = want else {
                    return (None, None);
                };
                let actual = want
                    .keys()
                    .map(|id| (id.clone(), h.issues.iter().find(|i| i.id == id).map(pick)))
                    .collect();
                let expected = want
                    .iter()
                    .map(|(id, t)| (id.clone(), t.get(PLATFORM).map(str::to_owned)))
                    .collect();
                (Some(actual), Some(expected))
            };
            let (details, want_details) = texts(&e.details, |i| i.detail.clone());
            let (hints, want_hints) = texts(&e.hints, |i| i.hint.to_owned());
            let actual = Outcome {
                grade: h.grade.map(|g| g.letter().to_owned()),
                score: h.score,
                summary: h.summary.to_owned(),
                issues: h
                    .issues
                    .iter()
                    .map(|i| (i.id.to_owned(), severity_name(i.severity).to_owned()))
                    .collect(),
                details,
                hints,
            };
            let expected = Outcome {
                grade: e.grade.clone(),
                score: e.score,
                summary: e.summary.clone(),
                issues: e
                    .issues
                    .iter()
                    .map(|i| (i.id.clone(), i.severity.clone()))
                    .collect(),
                details: want_details,
                hints: want_hints,
            };
            if actual != expected {
                failures.push(format!(
                    "case {:?}\n  expected: {expected:#?}\n  actual:   {actual:#?}",
                    case.name
                ));
            }
        }
        assert!(ran > 50, "only {ran} shared cases ran");
        assert!(
            failures.is_empty(),
            "{} of {ran} cases failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn every_native_issue_in_the_spec_has_a_check_and_every_check_an_issue() {
        let mut spec_ids: Vec<&str> = spec().issues_for(PLATFORM).map(|i| i.id.as_str()).collect();
        let mut check_ids: Vec<&str> = CHECKS.iter().map(|(id, _)| *id).collect();
        spec_ids.sort_unstable();
        check_ids.sort_unstable();
        assert_eq!(spec_ids, check_ids);
    }

    #[test]
    fn the_window_and_minimum_come_from_the_spec() {
        assert_eq!(health_window(), spec().window);
        assert_eq!(
            Assessment::default().summary,
            spec().text.measuring.as_str()
        );
    }

    #[test]
    fn bad_finds_the_first_listed_issue() {
        let a = assess(
            &(0..8)
                .map(|_| StatsSnapshot {
                    present_fps: 30.0,
                    sent_fps: Some(60.0),
                    target_fps: Some(60),
                    decode_ms: Some(20.0),
                    ..StatsSnapshot::default()
                })
                .collect::<Vec<_>>(),
        );
        assert_eq!(a.bad(&["decode"]).map(|i| i.id), Some("decode"));
        assert!(a.bad(&["rtt"]).is_none());
    }
}
