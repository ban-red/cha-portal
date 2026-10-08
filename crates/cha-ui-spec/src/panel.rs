//! The stats panel's spec (`web/packages/ui-spec/stats-panel.json`) and its model: [`build_panel`]
//! turns what a player measured into the rows, summaries, compact line and copy report both
//! players draw. `web/packages/ui-spec/panel.ts` is the TypeScript twin; the cases in
//! `stats-panel-cases.json` keep them equal. No egui here: the renderers only draw the result.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;

use crate::health::{Platform, PlatformText, format_number, is_formatter, placeholder, unit};

const PANEL_JSON: &str = include_str!("../../../web/packages/ui-spec/stats-panel.json");

const DASH: &str = "–";

/// What a panel colour means; the renderer maps it to a theme colour. `Dim` is the secondary ink
/// and `None` the normal ink.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    #[default]
    None,
    Ok,
    Warn,
    Danger,
    Dim,
}

/// A theme role named in the spec: a section's heading colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum SectionColor {
    #[serde(rename = "accent")]
    Accent,
    #[serde(rename = "chart-1")]
    Chart1,
    #[serde(rename = "chart-2")]
    Chart2,
    #[serde(rename = "chart-3")]
    Chart3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Minor,
    Major,
    Critical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueKind {
    Number,
    String,
}

/// The value keys required to be present, for everyone or for a platform (a platform not listed
/// needs none).
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Keys {
    All(Vec<String>),
    Each(EachKeys),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EachKeys {
    pub web: Option<Vec<String>>,
    pub native: Option<Vec<String>>,
}

impl Keys {
    pub fn for_platform(&self, platform: Platform) -> &[String] {
        match self {
            Keys::All(k) => k,
            Keys::Each(e) => match platform {
                Platform::Web => e.web.as_deref(),
                Platform::Native => e.native.as_deref(),
            }
            .unwrap_or(&[]),
        }
    }
}

/// Hot: at or over `limit`, in the value, or in percent of `of` (the key holding the total).
/// Never hot when `key` or `of` is missing.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hot {
    pub key: String,
    pub of: Option<String>,
    pub limit: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FixedTone {
    Dim,
}

/// A piece of text with the conditions it shows under.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    pub text: PlatformText,
    /// Shown only if every key is present.
    pub when: Option<Keys>,
    /// Shown only if none of the keys is present.
    pub unless: Option<Vec<String>>,
    pub platforms: Option<Vec<Platform>>,
    /// A row value's pieces: the tone while hot.
    pub hot: Option<Hot>,
    /// A row value's pieces: a fixed tone.
    pub tone: Option<FixedTone>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum RowValue {
    One(String),
    Parts(Vec<Part>),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowSpec {
    pub id: String,
    pub label: PlatformText,
    pub tooltip: PlatformText,
    pub value: RowValue,
    /// Which issue ids colour the value, for everyone or for a platform.
    pub bad: Option<Keys>,
    pub platforms: Option<Vec<Platform>>,
    pub when: Option<Keys>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummarySpec {
    pub parts: Vec<Part>,
    pub bad: Option<Keys>,
    pub hot: Option<Vec<Hot>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionSpec {
    pub id: String,
    pub heading: String,
    pub color: SectionColor,
    pub when: Option<Keys>,
    pub summary: SummarySpec,
    pub rows: Vec<RowSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportLineSpec {
    pub id: String,
    pub label: String,
    pub platforms: Option<Vec<Platform>>,
    pub when: Option<Keys>,
    pub parts: Vec<Part>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportSpec {
    pub issue: String,
    pub lines: Vec<ReportLineSpec>,
    pub agent: PlatformText,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValueSpec {
    pub kind: ValueKind,
    pub platforms: Vec<Platform>,
    /// Made from the health grade or the call, not filled by a player.
    #[serde(default)]
    pub derived: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradeTones {
    #[serde(rename = "A")]
    pub a: Tone,
    #[serde(rename = "B")]
    pub b: Tone,
    #[serde(rename = "C")]
    pub c: Tone,
    #[serde(rename = "D")]
    pub d: Tone,
    #[serde(rename = "F")]
    pub f: Tone,
    pub none: Tone,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeverityTones {
    pub minor: Tone,
    pub major: Tone,
    pub critical: Tone,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatsPanelSpec {
    pub separator: String,
    pub grade_tone: GradeTones,
    pub severity_tone: SeverityTones,
    pub hot_tone: Tone,
    /// Every value key a template, `when`, `unless` or `hot` may use.
    pub values: BTreeMap<String, ValueSpec>,
    pub compact: Vec<Part>,
    pub sections: Vec<SectionSpec>,
    pub report: ReportSpec,
}

static SPEC: LazyLock<StatsPanelSpec> =
    LazyLock::new(|| serde_json::from_str(PANEL_JSON).expect("stats-panel.json parses"));

/// The stats panel spec, parsed once.
pub fn spec() -> &'static StatsPanelSpec {
    &SPEC
}

// --- values ---

/// A reading: a number or a string.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Num(f64),
    Str(String),
}

/// A player's readings by value key. A key a player doesn't have, can't measure yet or got NaN for
/// is left out, and reads "–" (or hides its row).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Values(BTreeMap<String, Value>);

impl Values {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a number; a NaN or infinity is the same as none.
    pub fn num(&mut self, key: &str, v: impl Into<f64>) -> &mut Self {
        let v = v.into();
        if v.is_finite() {
            self.0.insert(key.to_owned(), Value::Num(v));
        }
        self
    }

    /// Sets a number if there is one.
    pub fn opt(&mut self, key: &str, v: Option<impl Into<f64>>) -> &mut Self {
        if let Some(v) = v {
            self.num(key, v);
        }
        self
    }

    /// Sets a string; an empty one is none.
    pub fn text(&mut self, key: &str, v: &str) -> &mut Self {
        if !v.is_empty() {
            self.0.insert(key.to_owned(), Value::Str(v.to_owned()));
        }
        self
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    fn remove(&mut self, key: &str) {
        self.0.remove(key);
    }

    fn number(&self, key: &str) -> Option<f64> {
        match self.0.get(key) {
            Some(Value::Num(v)) => Some(*v),
            _ => None,
        }
    }
}

/// The health grade as the panel needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelHealth {
    pub grade: Option<String>,
    pub score: Option<u32>,
    pub summary: String,
    pub issues: Vec<(String, Severity)>,
}

impl PanelHealth {
    /// A smooth stream: A, 100, no issues.
    pub fn smooth() -> Self {
        Self {
            grade: Some("A".into()),
            score: Some(100),
            summary: "Smooth".into(),
            issues: Vec::new(),
        }
    }
}

// --- filling ---

/// Fills a template like `health::fill`, but a value that is missing reads "–" (and its unit:
/// "– ms"), where `fill` panics. A number still needs a formatter and a string takes none.
pub fn fill_panel(template: &str, values: &Values) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        match placeholder(after) {
            Some((name, formatter, len)) => {
                if let Some(f) = formatter {
                    assert!(
                        is_formatter(f),
                        "fill: unknown formatter {f} in {template:?}"
                    );
                }
                match (values.get(name), formatter) {
                    (Some(Value::Str(s)), None) => out.push_str(s),
                    (Some(Value::Str(_)), Some(f)) => {
                        panic!("fill: {{{name}:{f}}} is a string and takes no formatter")
                    }
                    (Some(Value::Num(_)), None) => {
                        panic!("fill: {{{name}}} is a number and needs a formatter")
                    }
                    (Some(Value::Num(v)), Some(f)) => out.push_str(&format_number(*v, f)),
                    (None, None) => out.push_str(DASH),
                    (None, Some("s")) => {}
                    (None, Some(f)) => {
                        out.push_str(DASH);
                        out.push_str(unit(f));
                    }
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

fn text(t: &PlatformText, platform: Platform) -> &str {
    t.get(platform)
        .unwrap_or_else(|| panic!("stats-panel.json: no text for {platform:?}"))
}

fn on(platforms: &Option<Vec<Platform>>, platform: Platform) -> bool {
    platforms.as_ref().is_none_or(|p| p.contains(&platform))
}

fn shown(
    platforms: &Option<Vec<Platform>>,
    when: &Option<Keys>,
    unless: &Option<Vec<String>>,
    values: &Values,
    platform: Platform,
) -> bool {
    on(platforms, platform)
        && when.as_ref().is_none_or(|k| {
            k.for_platform(platform)
                .iter()
                .all(|key| values.contains(key))
        })
        && unless
            .as_ref()
            .is_none_or(|u| u.iter().all(|key| !values.contains(key)))
}

fn part_shown(p: &Part, values: &Values, platform: Platform) -> bool {
    shown(&p.platforms, &p.when, &p.unless, values, platform)
}

/// Percent of `total`, 0 when there is none.
fn pct(used: f64, total: f64) -> f64 {
    if total > 0.0 {
        used / total * 100.0
    } else {
        0.0
    }
}

fn is_hot(hot: &Hot, values: &Values) -> bool {
    let Some(v) = values.number(&hot.key) else {
        return false;
    };
    match &hot.of {
        None => v >= hot.limit,
        Some(of) => values
            .number(of)
            .is_some_and(|total| pct(v, total) >= hot.limit),
    }
}

/// A value coloured only when health found its signal bad, in that issue's severity.
fn bad_tone(ids: &Option<Keys>, health: &PanelHealth, platform: Platform) -> Tone {
    let Some(ids) = ids else { return Tone::None };
    let list = ids.for_platform(platform);
    let spec = spec();
    health
        .issues
        .iter()
        .find(|(id, _)| list.contains(id))
        .map_or(Tone::None, |(_, severity)| match severity {
            Severity::Minor => spec.severity_tone.minor,
            Severity::Major => spec.severity_tone.major,
            Severity::Critical => spec.severity_tone.critical,
        })
}

fn join_parts(parts: &[Part], values: &Values, platform: Platform, joiner: &str) -> String {
    parts
        .iter()
        .filter(|p| part_shown(p, values, platform))
        .map(|p| fill_panel(text(&p.text, platform), values))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(joiner)
}

// --- the model ---

/// A piece of a row's value; neighbours of the same tone are one piece.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub text: String,
    pub tone: Tone,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PanelRow {
    pub id: String,
    pub label: String,
    pub tooltip: String,
    /// The tone of the whole value (a segment's own tone, when it has one, wins).
    pub tone: Tone,
    pub segments: Vec<Segment>,
}

impl PanelRow {
    /// The pieces' text, joined.
    pub fn value(&self) -> String {
        self.segments.iter().map(|s| s.text.as_str()).collect()
    }

    /// The tone a segment is drawn in.
    pub fn tone_of(&self, segment: &Segment) -> Tone {
        if segment.tone == Tone::None {
            self.tone
        } else {
            segment.tone
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PanelSummary {
    pub text: String,
    pub tone: Tone,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PanelSection {
    pub id: String,
    pub heading: String,
    pub color: SectionColor,
    /// What a folded section shows beside its heading.
    pub summary: PanelSummary,
    pub rows: Vec<PanelRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Panel {
    /// The grade letter's tone in the header and the hidden chip.
    pub grade_tone: Tone,
    /// "60 fps · 11.6 ms · 58.2 Mbit/s · HEVC/WT".
    pub compact: String,
    /// Only the sections and rows this player shows right now, in order.
    pub sections: Vec<PanelSection>,
    /// The copy report's numbers, one line each (see [`Panel::report`]).
    pub report_lines: Vec<String>,
    issue_template: String,
    agent_template: String,
}

/// One issue as the copy report writes it.
#[derive(Clone, Debug, PartialEq)]
pub struct ReportIssue<'a> {
    pub title: &'a str,
    pub detail: &'a str,
    pub hint: &'a str,
}

impl Panel {
    /// The text a copy button puts on the clipboard: the issues with their hints, then the
    /// panel's numbers, then who is asking (`agent`: "Cha Player 0.1.0, macOS 15.5").
    pub fn report(&self, issues: &[ReportIssue<'_>], agent: &str) -> String {
        let mut lines: Vec<String> = Vec::new();
        for i in issues {
            let mut v = Values::new();
            v.text("title", i.title).text("detail", i.detail);
            lines.push(fill_panel(&self.issue_template, &v));
            lines.push(i.hint.to_owned());
            lines.push(String::new());
        }
        lines.extend(self.report_lines.iter().cloned());
        let mut v = Values::new();
        v.text("agent", agent);
        lines.push(fill_panel(&self.agent_template, &v));
        lines.join("\n")
    }
}

fn row_segments(value: &RowValue, values: &Values, platform: Platform) -> Vec<Segment> {
    let one;
    let parts: &[Part] = match value {
        RowValue::One(t) => {
            one = [Part {
                text: PlatformText::All(t.clone()),
                when: None,
                unless: None,
                platforms: None,
                hot: None,
                tone: None,
            }];
            &one
        }
        RowValue::Parts(p) => p,
    };
    let mut out: Vec<Segment> = Vec::new();
    for p in parts.iter().filter(|p| part_shown(p, values, platform)) {
        let t = fill_panel(text(&p.text, platform), values);
        let tone = match (&p.tone, &p.hot) {
            (Some(FixedTone::Dim), _) => Tone::Dim,
            (None, Some(h)) if is_hot(h, values) => spec().hot_tone,
            _ => Tone::None,
        };
        match out.last_mut() {
            Some(last) if last.tone == tone => last.text.push_str(&t),
            _ => out.push(Segment { text: t, tone }),
        }
    }
    out
}

/// The panel a player draws, from what it measured. `values` are the spec's value keys; the
/// health-derived ones are added here. Pure: the same input gives the same panel on both players.
pub fn build_panel(
    spec: &StatsPanelSpec,
    values: &Values,
    health: &PanelHealth,
    platform: Platform,
) -> Panel {
    let mut have = values.clone();
    have.remove("health_grade");
    have.remove("health_score");
    if let Some(g) = &health.grade {
        have.text("health_grade", g);
    }
    if let Some(s) = health.score {
        have.num("health_score", f64::from(s));
    }
    have.text("health_summary", &health.summary);

    let mut sections = Vec::new();
    for s in &spec.sections {
        if !shown(&None, &s.when, &None, &have, platform) {
            continue;
        }
        let hot = s.summary.hot.iter().flatten().any(|h| is_hot(h, &have));
        let bad = bad_tone(&s.summary.bad, health, platform);
        let mut rows = Vec::new();
        for r in &s.rows {
            if !shown(&r.platforms, &r.when, &None, &have, platform) {
                continue;
            }
            let mut segments = row_segments(&r.value, &have, platform);
            let mut tone = bad_tone(&r.bad, health, platform);
            // A row that is one piece wears its tone whole.
            if segments.len() == 1 && tone == Tone::None {
                tone = segments[0].tone;
                segments[0].tone = Tone::None;
            }
            rows.push(PanelRow {
                id: r.id.clone(),
                label: text(&r.label, platform).to_owned(),
                tooltip: text(&r.tooltip, platform).to_owned(),
                tone,
                segments,
            });
        }
        sections.push(PanelSection {
            id: s.id.clone(),
            heading: s.heading.clone(),
            color: s.color,
            summary: PanelSummary {
                text: join_parts(&s.summary.parts, &have, platform, &spec.separator),
                tone: if bad != Tone::None {
                    bad
                } else if hot {
                    spec.hot_tone
                } else {
                    Tone::None
                },
            },
            rows,
        });
    }

    let mut report_lines = Vec::new();
    for line in &spec.report.lines {
        if !shown(&line.platforms, &line.when, &None, &have, platform) {
            continue;
        }
        let body = join_parts(&line.parts, &have, platform, ", ");
        if !body.is_empty() {
            report_lines.push(format!("{}: {body}", line.label));
        }
    }

    let tones = &spec.grade_tone;
    Panel {
        grade_tone: match health.grade.as_deref() {
            Some("A") => tones.a,
            Some("B") => tones.b,
            Some("C") => tones.c,
            Some("D") => tones.d,
            Some("F") => tones.f,
            _ => tones.none,
        },
        compact: join_parts(&spec.compact, &have, platform, &spec.separator),
        sections,
        report_lines,
        issue_template: spec.report.issue.clone(),
        agent_template: text(&spec.report.agent, platform).to_owned(),
    }
}

/// A number with fixed digits, or an en dash when there is none (the stats panel's `num`).
pub fn format_num(v: Option<f64>, digits: usize) -> String {
    match v {
        Some(v) if v.is_finite() => crate::health::fixed(v, digits),
        _ => DASH.into(),
    }
}

// The shared cases (stats-panel-cases.json, format-cases.json) run in `panel_cases`, behind the
// `cases` feature and this crate's own tests.
