//! The shared saved-settings cases (`prefs-cases.json`), typed, and the helpers both runners use.
//! Behind the `cases` feature (and this crate's tests).
//!
//! A case says what a platform holds after parsing a saved value: the group's defaults, the
//! case's `expect` fields over them, and only the fields that platform keeps. A case that names
//! `platforms` runs on those only.

use std::sync::LazyLock;

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Map, Value as Json};

use crate::health::Platform;
use crate::prefs::GroupName;

const CASES_JSON: &str = include_str!("../../../web/packages/ui-spec/prefs-cases.json");

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefsCase {
    pub name: String,
    pub group: GroupName,
    pub platforms: Option<Vec<Platform>>,
    /// The saved text as stored (null: nothing saved). Exactly one of `text` and `value`.
    #[serde(default, deserialize_with = "some")]
    pub text: Option<Json>,
    #[serde(default, deserialize_with = "some")]
    pub value: Option<Json>,
    pub expect: Map<String, Json>,
}

fn some<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Json>, D::Error> {
    Json::deserialize(d).map(Some)
}

impl PrefsCase {
    pub fn runs_on(&self, platform: Platform) -> bool {
        self.platforms
            .as_ref()
            .is_none_or(|p| p.contains(&platform))
    }

    /// The saved text this case stores; `None` is nothing saved.
    pub fn saved_text(&self) -> Option<String> {
        match (&self.text, &self.value) {
            (Some(Json::Null), None) => None,
            (Some(Json::String(t)), None) => Some(t.clone()),
            (None, Some(v)) => Some(v.to_string()),
            _ => panic!(
                "{}: exactly one of text (a string or null) and value",
                self.name
            ),
        }
    }

    /// What the case expects `platform` to hold: the file's defaults, this case's fields over them.
    pub fn expected(&self, platform: Platform) -> Map<String, Json> {
        let defaults = &file().defaults[&self.group];
        let mut out = Map::new();
        for (name, value) in defaults.iter().chain(&self.expect) {
            let field = &self.group.group().fields[name];
            if field.on(platform) {
                out.insert(name.clone(), value.clone());
            }
        }
        normalised(&Json::Object(out)).as_object().cloned().unwrap()
    }
}

/// Every number as a float, so `40` and `40.0` compare equal.
pub fn normalised(v: &Json) -> Json {
    match v {
        Json::Number(n) => Json::from(n.as_f64().unwrap()),
        Json::Array(a) => Json::Array(a.iter().map(normalised).collect()),
        Json::Object(o) => {
            Json::Object(o.iter().map(|(k, v)| (k.clone(), normalised(v))).collect())
        }
        other => other.clone(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CasesFile {
    /// What each group holds with nothing saved, written out in the file and not read from
    /// `prefs.json`, so changing a default there fails the cases.
    defaults: BTreeMap<GroupName, Map<String, Json>>,
    cases: Vec<PrefsCase>,
}

static FILE: LazyLock<CasesFile> =
    LazyLock::new(|| serde_json::from_str(CASES_JSON).expect("prefs-cases.json parses"));

fn file() -> &'static CasesFile {
    &FILE
}

/// The shared cases, parsed once.
pub fn cases() -> &'static [PrefsCase] {
    &file().cases
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefs;

    #[test]
    fn the_validator_makes_what_every_case_expects_on_both_platforms() {
        assert!(cases().len() >= 20);
        for platform in [Platform::Web, Platform::Native] {
            for c in cases().iter().filter(|c| c.runs_on(platform)) {
                let text = c.saved_text().unwrap_or_default();
                let got = prefs::parse_text(c.group, &text, platform);
                assert_eq!(
                    normalised(&Json::Object(got)),
                    Json::Object(c.expected(platform)),
                    "{} ({platform:?}): {}",
                    c.name,
                    text
                );
            }
        }
    }
}
