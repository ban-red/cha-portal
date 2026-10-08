//! The shared hold-Esc cases (`capture-cases.json`), typed, and the runner that checks `EscHold`
//! against them. The browser's `escHold.test.ts` runs the same file.

use std::sync::LazyLock;

use serde::Deserialize;

use crate::esc_hold::{EscHold, HostKey, Step};

const CASES_JSON: &str = include_str!("../../../web/packages/ui-spec/capture-cases.json");

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Event {
    Down,
    Repeat,
    Up,
    Blur,
    Tick,
    Uncapture,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Host {
    Down,
    Up,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepCase {
    pub t: u64,
    pub ev: Event,
    pub captured: Option<bool>,
    pub host: Option<Host>,
    pub release: Option<bool>,
    pub hint: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscCase {
    pub name: String,
    pub steps: Vec<StepCase>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timing {
    pub release_hold_ms: u64,
    pub release_hint_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CasesFile {
    pub notes: String,
    pub timing: Timing,
    pub cases: Vec<EscCase>,
}

static FILE: LazyLock<CasesFile> =
    LazyLock::new(|| serde_json::from_str(CASES_JSON).expect("capture-cases.json parses"));

/// The shared hold-Esc cases file, parsed once.
pub fn file() -> &'static CasesFile {
    &FILE
}

/// Run one case against a fresh `EscHold`; returns what is wrong, one line per problem.
pub fn run(case: &EscCase, esc: &mut EscHold) -> Vec<String> {
    let mut problems = Vec::new();
    for (n, s) in case.steps.iter().enumerate() {
        let at = format!("{}: step {n} ({:?} at {})", case.name, s.ev, s.t);
        let captured = s.captured.unwrap_or(true);
        let step: Step = match s.ev {
            Event::Down => esc.key_down(s.t, false, captured),
            Event::Repeat => esc.key_down(s.t, true, captured),
            Event::Up => esc.key_up(s.t),
            Event::Tick => esc.tick(s.t),
            Event::Blur => esc.blur(),
            Event::Uncapture => {
                esc.uncapture();
                Step::default()
            }
        };
        let want_host = s.host.map(|h| match h {
            Host::Down => HostKey::Down,
            Host::Up => HostKey::Up,
        });
        if step.host != want_host {
            problems.push(format!(
                "{at}: host was {:?}, want {want_host:?}",
                step.host
            ));
        }
        if step.release != s.release.unwrap_or(false) {
            problems.push(format!("{at}: release was {}", step.release));
        }
        match (esc.hint(s.t), s.hint) {
            (None, None) => {}
            (Some(got), Some(want)) if (f64::from(got) - want).abs() < 1e-6 => {}
            (got, want) => problems.push(format!("{at}: hint was {got:?}, want {want:?}")),
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cases_were_worked_out_with_the_specs_timing() {
        let t = &crate::toolbar::spec().timing;
        let f = file();
        assert_eq!(f.timing.release_hold_ms, t.release_hold_ms);
        assert_eq!(f.timing.release_hint_ms, t.release_hint_ms);
        assert!(f.cases.len() >= 10);
    }

    #[test]
    fn every_shared_case_passes() {
        let mut problems = Vec::new();
        for case in &file().cases {
            problems.extend(run(case, &mut EscHold::from_spec()));
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
