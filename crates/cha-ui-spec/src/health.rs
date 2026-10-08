//! The health grade's spec (`web/packages/ui-spec/health.json`): constants, bands, weights and
//! each issue's texts, typed, plus the template [`fill`] both players use so their details read
//! the same. The checks themselves are code in `cha-player` (and `@cha/player` for the browser).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;

const HEALTH_JSON: &str = include_str!("../../../web/packages/ui-spec/health.json");

/// Who a check or a wording is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// The browser player (`@cha/player`).
    Web,
    /// Cha Player (`cha-player`).
    Native,
}

/// A text both platforms share, or each platform's own wording (a platform may be left out where
/// it has none, as with a detail only one side can measure).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum PlatformText {
    All(String),
    Each(EachText),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EachText {
    pub web: Option<String>,
    pub native: Option<String>,
}

impl PlatformText {
    pub fn get(&self, platform: Platform) -> Option<&str> {
        match self {
            PlatformText::All(s) => Some(s),
            PlatformText::Each(e) => match platform {
                Platform::Web => e.web.as_deref(),
                Platform::Native => e.native.as_deref(),
            },
        }
    }
}

/// The first value that counts and the value that is as bad as it gets (`from` above `to` for a
/// signal where lower is worse).
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Band {
    pub from: f64,
    pub to: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Levels {
    pub base: f64,
    pub slope: f64,
    pub spike_share: f64,
    pub min: f64,
    pub major: f64,
    pub critical: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeverityCap {
    pub minor: f64,
    pub major: f64,
    pub critical: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradeFloor {
    pub grade: String,
    pub floor: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Words {
    pub hidden: String,
    pub measuring: String,
    pub smooth: String,
    pub list_separator: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    pub busy_sent_fps: f64,
    pub gpu_late_encode: f64,
    pub freeze_budgets: f64,
    pub freeze_min_ms: f64,
    pub freeze_to_ms: f64,
    pub no_frames_gap_ms: f64,
    pub latency_spread_samples: usize,
    pub sound_present: f64,
    pub sound_played: f64,
    pub sound_out_seconds: usize,
    pub sound_restart_first: f64,
    pub sound_restart_step: f64,
    pub sound_out_first: f64,
    pub sound_out_step: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bands {
    pub fps_ratio: Band,
    pub dropped: Band,
    pub decode_budget: Band,
    pub latency: Band,
    pub delivery_spread: Band,
    pub latency_spread: Band,
    pub jitter_buffer: Band,
    pub skipped: Band,
    pub lost_frames: Band,
    pub lost_packets: Band,
    pub recovered: Band,
    pub partial: Band,
    pub rtt: Band,
    pub node_cpu: Band,
    pub node_ram: Band,
    pub node_gpu: Band,
    pub node_vram: Band,
    pub node_enc: Band,
    pub streamer_cpu: Band,
    pub audio_jitter_wt: Band,
    pub audio_jitter_rtc: Band,
}

/// What a template's placeholder holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaceholderKind {
    /// Written `{name:formatter}`.
    Number,
    /// Written `{name}`.
    String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueSpec {
    pub id: String,
    pub platforms: Vec<Platform>,
    pub title: PlatformText,
    /// The one-line summary words while this is the worst issue.
    pub summary: String,
    /// Score points it costs at level 1; at lower levels, in proportion.
    pub weight: f64,
    pub hint: PlatformText,
    /// Detail templates by variant; most issues have one, `main`.
    pub details: BTreeMap<String, PlatformText>,
    pub placeholders: BTreeMap<String, PlaceholderKind>,
}

impl IssueSpec {
    /// The title for a platform. A spec without one is a bug the tests catch.
    pub fn title(&self, platform: Platform) -> &str {
        self.title
            .get(platform)
            .unwrap_or_else(|| panic!("health.json: {} has no title for {platform:?}", self.id))
    }

    pub fn hint(&self, platform: Platform) -> &str {
        self.hint
            .get(platform)
            .unwrap_or_else(|| panic!("health.json: {} has no hint for {platform:?}", self.id))
    }

    /// A detail template for a platform.
    pub fn detail(&self, variant: &str, platform: Platform) -> &str {
        self.details
            .get(variant)
            .and_then(|t| t.get(platform))
            .unwrap_or_else(|| {
                panic!(
                    "health.json: {} has no detail {variant:?} for {platform:?}",
                    self.id
                )
            })
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthSpec {
    /// Snapshots judged: the last ~8 s.
    pub window: usize,
    pub min_snapshots: usize,
    /// The frame rate assumed until the streamer says.
    pub fallback_fps: f64,
    pub levels: Levels,
    pub severity_cap: SeverityCap,
    /// Highest first.
    pub grades: Vec<GradeFloor>,
    pub text: Words,
    pub params: Params,
    pub bands: Bands,
    /// Why the numbers are what they are; not read by code.
    pub notes: BTreeMap<String, String>,
    /// In the order the checks run (it breaks ties between equal issues).
    pub issues: Vec<IssueSpec>,
}

impl HealthSpec {
    /// The issues a platform can report, in spec order.
    pub fn issues_for(&self, platform: Platform) -> impl Iterator<Item = &IssueSpec> {
        self.issues
            .iter()
            .filter(move |i| i.platforms.contains(&platform))
    }

    pub fn issue(&self, id: &str) -> Option<&IssueSpec> {
        self.issues.iter().find(|i| i.id == id)
    }
}

static SPEC: LazyLock<HealthSpec> =
    LazyLock::new(|| serde_json::from_str(HEALTH_JSON).expect("health.json parses"));

/// The health spec, parsed once.
pub fn spec() -> &'static HealthSpec {
    &SPEC
}

// --- templates ---

/// A value for a placeholder: a number needs a formatter in the template, a string takes none.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Num(f64),
    Str(String),
}

impl From<f64> for Val {
    fn from(v: f64) -> Self {
        Val::Num(v)
    }
}

impl From<&str> for Val {
    fn from(v: &str) -> Self {
        Val::Str(v.to_owned())
    }
}

impl From<String> for Val {
    fn from(v: String) -> Self {
        Val::Str(v)
    }
}

/// `Number.prototype.toFixed`, as the browser side uses it: a half rounds up (away from zero),
/// where Rust's `{:.N}` rounds a half to even, and a NaN or infinity reads as JavaScript's.
/// Values past 1e21, which JavaScript writes in exponent form, are not handled.
pub fn fixed(v: f64, digits: usize) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if v == 0.0 {
        return format!("{:.*}", digits, 0.0);
    }
    let abs = v.abs();
    // Rounding is exact on the double's decimal expansion, so only an exact half is a tie to break.
    let abs = if is_half(abs, digits) {
        abs.next_up()
    } else {
        abs
    };
    let text = format!("{:.*}", digits, abs);
    if v < 0.0 { format!("-{text}") } else { text }
}

/// Whether `abs` is exactly halfway between two numbers of `digits` decimals.
fn is_half(abs: f64, digits: usize) -> bool {
    if digits > 6 {
        return false;
    }
    let bits = abs.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1 << 52) - 1);
    let (mant, exp) = if biased == 0 {
        (frac, -1074)
    } else {
        (frac | (1 << 52), biased - 1075)
    };
    if exp >= 0 {
        return false;
    }
    // abs * 2 * 10^digits = mant * 5^digits * 2^(exp + digits + 1): a half when that is an odd integer.
    let t = u128::from(mant) * 5u128.pow(digits as u32);
    let k = -exp - digits as i32 - 1;
    (0..128).contains(&k) && t.is_multiple_of(1u128 << k) && (t >> k) & 1 == 1
}

/// The formatters a placeholder can name: `{name:ms1}`.
fn format_number(v: f64, formatter: &str) -> String {
    match formatter {
        "f0" => fixed(v, 0),
        "f1" => fixed(v, 1),
        "f2" => fixed(v, 2),
        "ms0" => format!("{} ms", fixed(v, 0)),
        "ms1" => format!("{} ms", fixed(v, 1)),
        other => panic!("fill: unknown formatter {other}"),
    }
}

/// A placeholder at the start of `s` (which begins after its `{`): `name`, an optional
/// `:formatter` and `}`. Returns the name, the formatter and the length consumed after the `{`.
fn placeholder(s: &str) -> Option<(&str, Option<&str>, usize)> {
    let b = s.as_bytes();
    let word = |from: usize, first: fn(u8) -> bool, rest: fn(u8) -> bool| -> usize {
        if from >= b.len() || !first(b[from]) {
            return from;
        }
        let mut i = from + 1;
        while i < b.len() && rest(b[i]) {
            i += 1;
        }
        i
    };
    let lower_digit = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let name_end = word(
        0,
        |c| c.is_ascii_lowercase(),
        |c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_',
    );
    if name_end == 0 {
        return None;
    }
    let mut end = name_end;
    let mut formatter = None;
    if b.get(end) == Some(&b':') {
        let fmt_end = word(end + 1, lower_digit, lower_digit);
        if fmt_end > end + 1 {
            formatter = Some(&s[end + 1..fmt_end]);
            end = fmt_end;
        }
    }
    (b.get(end) == Some(&b'}')).then_some((&s[..name_end], formatter, end + 1))
}

/// Fills a template's `{name}` and `{name:formatter}` placeholders; anything else in braces stays
/// as written. Formatters: `f0`, `f1`, `f2` (fixed digits) and `ms0`, `ms1` (the same, then " ms").
/// A missing value, an unknown formatter, a number without one or a string with one panics: those
/// are spec bugs the tests catch.
pub fn fill(template: &str, values: &[(&str, Val)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        match placeholder(after) {
            Some((name, formatter, len)) => {
                let value = values
                    .iter()
                    .find(|(n, _)| *n == name)
                    .unwrap_or_else(|| panic!("fill: no value for {{{name}}} in {template:?}"));
                match (&value.1, formatter) {
                    (Val::Str(s), None) => out.push_str(s),
                    (Val::Str(_), Some(f)) => {
                        panic!("fill: {{{name}:{f}}} is a string and takes no formatter")
                    }
                    (Val::Num(_), None) => {
                        panic!("fill: {{{name}}} is a number and needs a formatter")
                    }
                    (Val::Num(v), Some(f)) => out.push_str(&format_number(*v, f)),
                }
                rest = &after[len..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_parses_and_is_consistent() {
        let s = spec();
        assert_eq!(s.grades.first().map(|g| g.grade.as_str()), Some("A"));
        assert_eq!(s.grades.last().map(|g| g.floor), Some(0.0));
        assert!(s.grades.windows(2).all(|w| w[0].floor > w[1].floor));
        let mut ids: Vec<&str> = s.issues.iter().map(|i| i.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), s.issues.len(), "issue ids are unique");
        for i in &s.issues {
            for p in &i.platforms {
                // Every platform gets a title and a hint; panics if not.
                i.title(*p);
                i.hint(*p);
                assert!(i.details.values().any(|t| t.get(*p).is_some()), "{}", i.id);
            }
        }
    }

    #[test]
    fn halves_round_up_as_tofixed_does() {
        let cases: [(f64, usize, &str); 12] = [
            (0.5, 0, "1"),
            (1.5, 0, "2"),
            (2.5, 0, "3"),
            (34.5, 0, "35"),
            (12.25, 1, "12.3"),
            (0.125, 2, "0.13"),
            (0.15, 1, "0.1"),
            (1.005, 2, "1.00"),
            (-2.5, 0, "-3"),
            (-0.0, 1, "0.0"),
            (59.6, 0, "60"),
            (1e-7, 3, "0.000"),
        ];
        for (v, d, want) in cases {
            assert_eq!(fixed(v, d), want, "{v} to {d} digits");
        }
        assert_eq!(fixed(f64::NAN, 1), "NaN");
        assert_eq!(fixed(f64::INFINITY, 1), "Infinity");
    }
}
