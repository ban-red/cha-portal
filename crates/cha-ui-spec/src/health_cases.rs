//! The shared health test vectors (`health-cases.json`) and fill cases, typed, for the runners in
//! `cha-player`. Behind the `cases` feature (and this crate's own tests): the player has no use
//! for them at run time.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use crate::health::{Platform, PlatformText};

const CASES_JSON: &str = include_str!("../../../web/packages/ui-spec/health-cases.json");
const FILL_CASES_JSON: &str = include_str!("../../../web/packages/ui-spec/fill-cases.json");

/// A measurement: a number, or the string "NaN" (JSON can't hold one).
fn meas<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Num {
        Number(f64),
        Text(String),
    }
    match Option::<Num>::deserialize(d)? {
        None => Ok(None),
        Some(Num::Number(v)) => Ok(Some(v)),
        Some(Num::Text(t)) if t == "NaN" => Ok(Some(f64::NAN)),
        Some(Num::Text(t)) => Err(serde::de::Error::custom(format!("not a number: {t:?}"))),
    }
}

macro_rules! snapshot_structs {
    ($(#[$m:meta])* $name:ident { $($(#[$fm:meta])* $f:ident),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Debug, Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct $name {
            $($(#[$fm])* #[serde(deserialize_with = "meas")] pub $f: Option<f64>,)*
        }
    };
}

snapshot_structs!(
    /// The node's resource use. Memory is in any unit, as long as used and total agree.
    SpecNode { cpu, cores, mem_used, mem_total, gpu, vram_used, vram_total, enc, streamer_cpu }
);

/// One second of a stream, in the shape both players can fill. Every field is optional and means
/// "unknown" when missing or null; a player skips what it can't measure. See
/// `web/packages/ui-spec/health-cases.ts` for what each field measures.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpecSnapshot {
    pub codec: Option<String>,
    #[serde(deserialize_with = "meas")]
    pub target_fps: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub sent_fps: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub shown_sent_fps: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub shown_fps: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub encode_p99_ms: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub decode_ms: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub jitter_buffer_ms: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub rtt_ms: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub lost: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub recovered: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub partial: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub dropped: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub latency_ms: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub delivery_ms: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub delivery_p95_ms: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub frame_gap_ms: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub audio_jitter_ms: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub audio_restarts: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub audio_in_peak: Option<f64>,
    #[serde(deserialize_with = "meas")]
    pub audio_out_peak: Option<f64>,
    pub node: Option<SpecNode>,
    pub awdl_suspected: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecContext {
    /// Default true.
    pub visible: Option<bool>,
    /// Default 1000.
    pub interval_ms: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedIssue {
    pub id: String,
    pub severity: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    pub grade: Option<String>,
    pub score: Option<u32>,
    pub summary: String,
    pub issues: Vec<ExpectedIssue>,
    pub details: Option<BTreeMap<String, PlatformText>>,
    pub hints: Option<BTreeMap<String, PlatformText>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthCase {
    pub name: String,
    /// Absent means both.
    pub platforms: Option<Vec<Platform>>,
    /// Entries of `{ repeat?, field: value | [values] }`; see [`HealthCases::history`].
    pub history: Vec<Value>,
    pub context: Option<SpecContext>,
    pub expect: Expect,
}

impl HealthCase {
    pub fn runs_on(&self, platform: Platform) -> bool {
        self.platforms
            .as_ref()
            .is_none_or(|p| p.contains(&platform))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthCases {
    /// Every history entry starts from this: a healthy 60 fps LAN stream.
    pub base: Value,
    pub cases: Vec<HealthCase>,
}

impl HealthCases {
    /// The snapshots a case's history stands for, oldest first. An entry is the base with its
    /// fields set, `repeat` times (default 1); a field may be an array of `repeat` values, one per
    /// snapshot. `node` merges into the base's node field by field, and `node: null` is no report.
    pub fn history(&self, case: &HealthCase) -> Vec<SpecSnapshot> {
        let base = self.base.as_object().expect("base is an object");
        let mut out = Vec::new();
        for entry in &case.history {
            let entry = entry.as_object().expect("a history entry is an object");
            let repeat = entry.get("repeat").map_or(1, |r| {
                r.as_u64().expect("repeat is a whole number") as usize
            });
            for i in 0..repeat {
                let mut snap: Map<String, Value> = base.clone();
                for (key, value) in entry {
                    if key == "repeat" {
                        continue;
                    }
                    let value = match value {
                        Value::Array(a) => {
                            assert_eq!(
                                a.len(),
                                repeat,
                                "{}: history field {key}: {} values for repeat {repeat}",
                                case.name,
                                a.len()
                            );
                            a[i].clone()
                        }
                        v => v.clone(),
                    };
                    match (key.as_str(), &value, base.get("node")) {
                        ("node", Value::Object(over), Some(Value::Object(under))) => {
                            let mut merged = under.clone();
                            merged.extend(over.clone());
                            snap.insert("node".into(), Value::Object(merged));
                        }
                        _ => {
                            snap.insert(key.clone(), value);
                        }
                    }
                }
                out.push(
                    serde_json::from_value(Value::Object(snap))
                        .unwrap_or_else(|e| panic!("{}: bad snapshot: {e}", case.name)),
                );
            }
        }
        out
    }
}

static CASES: LazyLock<HealthCases> =
    LazyLock::new(|| serde_json::from_str(CASES_JSON).expect("health-cases.json parses"));

/// The shared health cases, parsed once.
pub fn health_cases() -> &'static HealthCases {
    &CASES
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FillCase {
    pub name: String,
    pub template: String,
    /// Numbers and strings; `{ "nan": true }` is a NaN.
    pub values: BTreeMap<String, Value>,
    pub expect: String,
}

pub fn fill_cases() -> Vec<FillCase> {
    serde_json::from_str(FILL_CASES_JSON).expect("fill-cases.json parses")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{Val, fill};

    #[test]
    fn the_health_cases_parse_and_expand() {
        let cases = health_cases();
        assert!(cases.cases.len() > 50);
        let mut names: Vec<&str> = cases.cases.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), cases.cases.len(), "case names are unique");
        for c in &cases.cases {
            cases.history(c);
        }
    }

    #[test]
    fn fill_cases() {
        for c in super::fill_cases() {
            let values: Vec<(&str, Val)> = c
                .values
                .iter()
                .map(|(k, v)| {
                    let val = match v {
                        Value::Number(n) => Val::Num(n.as_f64().unwrap()),
                        Value::String(s) => Val::Str(s.clone()),
                        Value::Object(_) => Val::Num(f64::NAN),
                        other => panic!("{}: bad value {other}", c.name),
                    };
                    (k.as_str(), val)
                })
                .collect();
            assert_eq!(fill(&c.template, &values), c.expect, "{}", c.name);
        }
    }

    #[test]
    #[should_panic(expected = "needs a formatter")]
    fn a_number_without_a_formatter_panics() {
        fill("{a}", &[("a", Val::Num(1.0))]);
    }

    #[test]
    #[should_panic(expected = "no value for")]
    fn a_missing_value_panics() {
        fill("{a}", &[]);
    }
}
