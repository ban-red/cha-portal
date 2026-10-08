//! The shared stats panel cases (`stats-panel-cases.json`, `format-cases.json`), typed, and the
//! runner `cha-ui-spec`'s own tests use. Behind the `cases` feature (and this crate's tests):
//! the player has no use for them at run time.
//!
//! Both players' panels come from the same spec and the same pure `build_panel`, so a case runs
//! on *both* platforms here and in `web/apps/portal/src/statsPanel.test.ts`; a case that names
//! `platforms` runs on those only.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::Value as Json;

use crate::health::Platform;
use crate::panel::{Panel, PanelHealth, Severity, Tone, Values};

const CASES_JSON: &str = include_str!("../../../web/packages/ui-spec/stats-panel-cases.json");
const FORMAT_JSON: &str = include_str!("../../../web/packages/ui-spec/format-cases.json");

/// A tone's name in the cases.
pub fn tone_name(t: Tone) -> &'static str {
    match t {
        Tone::None => "none",
        Tone::Ok => "ok",
        Tone::Warn => "warn",
        Tone::Danger => "danger",
        Tone::Dim => "dim",
    }
}

/// What a case pins about one row. `parts` replaces `value` and `tone` where a row's pieces
/// differ in tone: `[text]` or `[text, tone]`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedRow {
    pub id: String,
    pub label: String,
    pub value: Option<String>,
    pub tone: Option<Tone>,
    pub parts: Option<Vec<Vec<String>>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedSection {
    pub id: String,
    pub summary: String,
    pub summary_tone: Option<Tone>,
    /// Exactly the rows shown, in order.
    pub row_ids: Vec<String>,
    /// The rows the case pins.
    pub rows: Option<Vec<ExpectedRow>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedPanel {
    pub grade_tone: Tone,
    pub compact: String,
    /// Exactly the sections shown, in order.
    pub sections: Vec<ExpectedSection>,
    /// The whole copy report.
    pub report: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EachExpected {
    pub web: Option<ExpectedPanel>,
    pub native: Option<ExpectedPanel>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Expect {
    One(ExpectedPanel),
    Each(EachExpected),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthIssueIn {
    pub id: String,
    pub severity: Severity,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthIn {
    pub grade: Option<String>,
    pub score: Option<u32>,
    pub summary: String,
    pub issues: Vec<HealthIssueIn>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportIssueIn {
    pub title: String,
    pub detail: String,
    pub hint: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelCase {
    pub name: String,
    /// Absent means both.
    pub platforms: Option<Vec<Platform>>,
    /// Numbers and strings; null and `{ "nan": true }` are missing.
    pub values: BTreeMap<String, Json>,
    pub health: Option<HealthIn>,
    pub report_issues: Option<Vec<ReportIssueIn>>,
    pub agent: Option<String>,
    pub expect: Expect,
}

pub const DEFAULT_AGENT: &str = "Test Agent/1.0";

impl PanelCase {
    pub fn runs_on(&self, platform: Platform) -> bool {
        self.platforms
            .as_ref()
            .is_none_or(|p| p.contains(&platform))
    }

    /// What the case expects on a platform.
    pub fn expected(&self, platform: Platform) -> &ExpectedPanel {
        match &self.expect {
            Expect::One(e) => e,
            Expect::Each(e) => match platform {
                Platform::Web => e.web.as_ref(),
                Platform::Native => e.native.as_ref(),
            }
            .unwrap_or_else(|| panic!("{}: no expectation for {platform:?}", self.name)),
        }
    }

    pub fn values(&self) -> Values {
        values_of(&self.name, &self.values)
    }

    pub fn health(&self) -> PanelHealth {
        match &self.health {
            None => PanelHealth::smooth(),
            Some(h) => PanelHealth {
                grade: h.grade.clone(),
                score: h.score,
                summary: h.summary.clone(),
                issues: h
                    .issues
                    .iter()
                    .map(|i| (i.id.clone(), i.severity))
                    .collect(),
            },
        }
    }

    /// The panel in the shape the case pins: the same sections and row ids, and the rows the case
    /// lists, with the report if the case has one.
    pub fn describe(&self, panel: &Panel, expected: &ExpectedPanel) -> ExpectedPanel {
        let sections = panel
            .sections
            .iter()
            .map(|s| {
                let want = expected.sections.iter().find(|e| e.id == s.id);
                let rows = want.and_then(|w| w.rows.as_ref()).map(|pinned| {
                    pinned
                        .iter()
                        .map(|w| match s.rows.iter().find(|r| r.id == w.id) {
                            None => ExpectedRow {
                                id: w.id.clone(),
                                label: "(missing)".into(),
                                value: None,
                                tone: None,
                                parts: None,
                            },
                            Some(r) if r.segments.len() > 1 => ExpectedRow {
                                id: r.id.clone(),
                                label: r.label.clone(),
                                value: None,
                                tone: (r.tone != Tone::None).then_some(r.tone),
                                parts: Some(
                                    r.segments
                                        .iter()
                                        .map(|x| {
                                            if x.tone == Tone::None {
                                                vec![x.text.clone()]
                                            } else {
                                                vec![x.text.clone(), tone_name(x.tone).into()]
                                            }
                                        })
                                        .collect(),
                                ),
                            },
                            Some(r) => ExpectedRow {
                                id: r.id.clone(),
                                label: r.label.clone(),
                                value: Some(r.value()),
                                tone: (r.tone != Tone::None).then_some(r.tone),
                                parts: None,
                            },
                        })
                        .collect()
                });
                ExpectedSection {
                    id: s.id.clone(),
                    summary: s.summary.text.clone(),
                    summary_tone: (s.summary.tone != Tone::None).then_some(s.summary.tone),
                    row_ids: s.rows.iter().map(|r| r.id.clone()).collect(),
                    rows,
                }
            })
            .collect();
        let issues: Vec<crate::panel::ReportIssue<'_>> = self
            .report_issues
            .iter()
            .flatten()
            .map(|i| crate::panel::ReportIssue {
                title: &i.title,
                detail: &i.detail,
                hint: &i.hint,
            })
            .collect();
        ExpectedPanel {
            grade_tone: panel.grade_tone,
            compact: panel.compact.clone(),
            sections,
            report: expected
                .report
                .as_ref()
                .map(|_| panel.report(&issues, self.agent.as_deref().unwrap_or(DEFAULT_AGENT))),
        }
    }
}

/// A case's values as the panel takes them.
pub fn values_of(case: &str, values: &BTreeMap<String, Json>) -> Values {
    let mut out = Values::new();
    for (k, v) in values {
        match v {
            Json::Number(n) => {
                out.num(k, n.as_f64().expect("a finite number"));
            }
            Json::String(s) => {
                out.text(k, s);
            }
            Json::Null | Json::Object(_) => {}
            other => panic!("{case}: bad value {other}"),
        }
    }
    out
}

static CASES: LazyLock<Vec<PanelCase>> =
    LazyLock::new(|| serde_json::from_str(CASES_JSON).expect("stats-panel-cases.json parses"));

/// The shared stats panel cases, parsed once.
pub fn panel_cases() -> &'static [PanelCase] {
    &CASES
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormatCase {
    pub name: String,
    pub template: String,
    /// Numbers and strings; null and `{ "nan": true }` are missing.
    pub values: BTreeMap<String, Json>,
    pub expect: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodecTagCase {
    pub name: String,
    pub platforms: Option<Vec<Platform>>,
    pub codec: Option<String>,
    /// "webtransport", "webrtc" or null.
    pub transport: Option<String>,
    pub expect: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormatCases {
    pub fill: Vec<FormatCase>,
    pub codec_tag: Vec<CodecTagCase>,
}

static FORMATS: LazyLock<FormatCases> =
    LazyLock::new(|| serde_json::from_str(FORMAT_JSON).expect("format-cases.json parses"));

/// The shared formatter cases, parsed once.
pub fn format_cases() -> &'static FormatCases {
    &FORMATS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::{build_panel, fill_panel, spec};

    #[test]
    fn the_panel_cases_parse_and_are_enough() {
        let cases = panel_cases();
        assert!(cases.len() >= 15);
        let mut names: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), cases.len(), "case names are unique");
    }

    #[test]
    fn the_shared_panel_cases() {
        let mut ran = 0;
        let mut failures = Vec::new();
        for case in panel_cases() {
            for platform in [Platform::Web, Platform::Native] {
                if !case.runs_on(platform) {
                    continue;
                }
                ran += 1;
                let want = case.expected(platform);
                let panel = build_panel(spec(), &case.values(), &case.health(), platform);
                let got = case.describe(&panel, want);
                if &got != want {
                    failures.push(format!(
                        "{} on {platform:?}:\n  want {want:#?}\n  got  {got:#?}",
                        case.name
                    ));
                }
            }
        }
        assert!(ran >= 30, "ran only {ran}");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn the_shared_format_cases() {
        for c in &format_cases().fill {
            let values = values_of(&c.name, &c.values);
            assert_eq!(fill_panel(&c.template, &values), c.expect, "{}", c.name);
        }
    }
}
