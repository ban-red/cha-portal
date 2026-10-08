//! The saved settings' spec (`web/packages/ui-spec/prefs.json`) and the one validator the native
//! player runs its config through. `web/packages/ui-spec/prefs.ts` is its TypeScript twin and
//! `prefs-cases.json` keeps the two equal.
//!
//! A field that is missing or invalid falls back on its own, to its default (or to absent when it
//! has none), so an old or hand-edited file never loses the rest. The output is a JSON object a
//! player deserialises into its own struct, which can then rely on every field being valid.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value as Json};

use crate::health::Platform;

const PREFS_JSON: &str = include_str!("../../../web/packages/ui-spec/prefs.json");

/// Limits of int fields as constants, generated from `prefs.json` by `build.rs`
/// (`STATS_PANEL_OPACITY_MIN`, `TOOLBAR_OVERLAY_MAX`, ...).
pub mod limits {
    include!(concat!(env!("OUT_DIR"), "/prefs_limits.rs"));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Bool,
    Int,
    Number,
    String,
    Enum,
    List,
    Object,
}

/// A `default` that is present, even when it is `null`.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Json>, D::Error> {
    Json::deserialize(d).map(Some)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    #[serde(rename = "type")]
    pub kind: Kind,
    pub doc: String,
    #[serde(default, deserialize_with = "present")]
    pub default: Option<Json>,
    #[serde(default)]
    pub optional: bool,
    pub platforms: Option<Vec<Platform>>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    #[serde(default)]
    pub round: bool,
    #[serde(default)]
    pub clamp: bool,
    #[serde(default)]
    pub nonempty: bool,
    #[serde(default)]
    pub values: Vec<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, Field>,
}

impl Field {
    pub fn on(&self, platform: Platform) -> bool {
        self.platforms
            .as_ref()
            .is_none_or(|p| p.contains(&platform))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub doc: String,
    pub fields: BTreeMap<String, Field>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefsSpec {
    #[allow(dead_code)]
    notes: String,
    pub stats_panel: Group,
    pub toolbar: Group,
}

static SPEC: LazyLock<PrefsSpec> =
    LazyLock::new(|| serde_json::from_str(PREFS_JSON).expect("prefs.json parses"));

/// The spec, parsed once.
pub fn spec() -> &'static PrefsSpec {
    &SPEC
}

/// The groups of saved settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupName {
    StatsPanel,
    Toolbar,
}

impl GroupName {
    pub fn group(self) -> &'static Group {
        match self {
            GroupName::StatsPanel => &spec().stats_panel,
            GroupName::Toolbar => &spec().toolbar,
        }
    }
}

/// A whole number as an integer value, anything else as a float.
fn number(n: f64) -> Json {
    if n.fract() == 0.0 && n.abs() < 9e15 {
        Json::from(n as i64)
    } else {
        Number::from_f64(n).map_or(Json::Null, Json::Number)
    }
}

/// One value against its field: the value to keep, or `None` when it is not valid.
pub fn parse_value(field: &Field, v: &Json) -> Option<Json> {
    match field.kind {
        Kind::Bool => v.is_boolean().then(|| v.clone()),
        Kind::String => v
            .as_str()
            .filter(|s| !field.nonempty || !s.is_empty())
            .map(|s| Json::String(s.to_owned())),
        Kind::Enum => v
            .as_str()
            .filter(|s| field.values.iter().any(|x| x == s))
            .map(|s| Json::String(s.to_owned())),
        Kind::List => {
            let items = v.as_array()?;
            Some(Json::Array(
                field
                    .values
                    .iter()
                    .filter(|id| items.iter().any(|i| i.as_str() == Some(id.as_str())))
                    .map(|id| Json::String(id.clone()))
                    .collect(),
            ))
        }
        Kind::Object => {
            let o = v.as_object()?;
            let mut out = Map::new();
            for (name, sub) in &field.fields {
                out.insert(name.clone(), parse_value(sub, o.get(name)?)?);
            }
            Some(Json::Object(out))
        }
        Kind::Int | Kind::Number => {
            let mut n = v.as_f64().filter(|n| n.is_finite())?;
            if field.kind == Kind::Int {
                if field.round {
                    // A half rounds up, as JavaScript's Math.round does (f64::round goes away from zero).
                    n = (n + 0.5).floor();
                } else if n.fract() != 0.0 {
                    return None;
                }
            }
            if field.clamp {
                if let Some(min) = field.min {
                    n = n.max(min);
                }
                if let Some(max) = field.max {
                    n = n.min(max);
                }
            } else if field.min.is_some_and(|m| n < m) || field.max.is_some_and(|m| n > m) {
                return None;
            }
            Some(if field.kind == Kind::Int {
                number(n)
            } else {
                Json::from(n)
            })
        }
    }
}

/// A group's saved settings from parsed JSON: the fields `platform` keeps, each valid or its
/// default (an optional field with none is left out). Anything that is not an object gives the
/// defaults.
pub fn parse(group: GroupName, input: &Json, platform: Platform) -> Map<String, Json> {
    let empty = Map::new();
    let o = input.as_object().unwrap_or(&empty);
    let mut out = Map::new();
    for (name, field) in &group.group().fields {
        if !field.on(platform) {
            continue;
        }
        let kept = o
            .get(name)
            .and_then(|v| parse_value(field, v))
            .or_else(|| field.default.clone());
        if let Some(value) = kept {
            out.insert(name.clone(), value);
        }
    }
    out
}

/// The same from JSON text; text that does not parse gives the defaults.
pub fn parse_text(group: GroupName, text: &str, platform: Platform) -> Map<String, Json> {
    let input = serde_json::from_str(text).unwrap_or(Json::Null);
    parse(group, &input, platform)
}

/// The limit of an int field, for a player that needs it (a slider's range).
pub fn limit(group: GroupName, field: &str) -> (Option<f64>, Option<f64>) {
    let f = &group.group().fields[field];
    (f.min, f.max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_parses_and_every_default_is_valid() {
        for group in [GroupName::StatsPanel, GroupName::Toolbar] {
            for (name, field) in &group.group().fields {
                assert!(
                    field.optional != field.default.is_some(),
                    "{name}: a default or optional, not both"
                );
                if let Some(d) = field.default.as_ref().filter(|d| !d.is_null()) {
                    assert_eq!(parse_value(field, d).as_ref(), Some(d), "{name}'s default");
                }
            }
        }
    }

    #[test]
    fn the_generated_limits_are_the_specs() {
        assert_eq!(limits::STATS_PANEL_OPACITY_MIN, 30);
        assert_eq!(limits::TOOLBAR_OVERLAY_MAX, 4);
        assert_eq!(
            limit(GroupName::StatsPanel, "opacity"),
            (Some(30.0), Some(100.0))
        );
    }

    #[test]
    fn a_half_rounds_up() {
        let f = &spec().toolbar.fields["volume"];
        assert_eq!(parse_value(f, &Json::from(33.5)), Some(Json::from(34)));
        assert_eq!(parse_value(f, &Json::from(-0.5)), Some(Json::from(0)));
        assert_eq!(parse_value(f, &Json::from(-3)), Some(Json::from(0)));
    }
}
