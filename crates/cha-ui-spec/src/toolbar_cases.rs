//! The shared toolbar cases (`toolbar-cases.json`), typed, and the runner `cha-ui-spec`'s own
//! tests use. Behind the `cases` feature (and this crate's tests): the player has no use for them
//! at run time.
//!
//! Both players' toolbars come from the same spec and the same pure `build_toolbar`, so a case
//! runs on *both* platforms here and in `web/apps/portal/src/toolbar.test.ts`; a case that names
//! `platforms` runs on those only.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::Value as Json;

use crate::health::Platform;
use crate::toolbar::{State, Toolbar};

const CASES_JSON: &str = include_str!("../../../web/packages/ui-spec/toolbar-cases.json");

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Caps {
    Named(String),
    List(Vec<String>),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub state: BTreeMap<String, Json>,
    pub web: BTreeMap<String, Json>,
    pub native: BTreeMap<String, Json>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolbarCase {
    pub name: String,
    /// Absent means both.
    pub platforms: Option<Vec<Platform>>,
    pub caps: Caps,
    pub state: BTreeMap<String, Json>,
    /// What both players show, or `{ "web": ..., "native": ... }`.
    pub expect: Json,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CasesFile {
    pub notes: String,
    pub caps: BTreeMap<String, Vec<String>>,
    pub defaults: Defaults,
    pub cases: Vec<ToolbarCase>,
}

static FILE: LazyLock<CasesFile> =
    LazyLock::new(|| serde_json::from_str(CASES_JSON).expect("toolbar-cases.json parses"));

/// The shared toolbar cases file, parsed once.
pub fn file() -> &'static CasesFile {
    &FILE
}

/// The shared toolbar cases, parsed once.
pub fn toolbar_cases() -> &'static [ToolbarCase] {
    &FILE.cases
}

impl ToolbarCase {
    pub fn runs_on(&self, platform: Platform) -> bool {
        self.platforms
            .as_ref()
            .is_none_or(|p| p.contains(&platform))
    }

    pub fn caps(&self) -> Vec<String> {
        match &self.caps {
            Caps::List(l) => l.clone(),
            Caps::Named(n) => file()
                .caps
                .get(n)
                .unwrap_or_else(|| panic!("{}: no caps named {n}", self.name))
                .clone(),
        }
    }

    /// The state a case gives a platform: the shared defaults, that platform's own, then the case's.
    pub fn state(&self, platform: Platform) -> State {
        let d = &file().defaults;
        let mut map = d.state.clone();
        map.extend(
            match platform {
                Platform::Web => &d.web,
                Platform::Native => &d.native,
            }
            .clone(),
        );
        map.extend(self.state.clone());
        State::from_map(&map)
    }

    /// What the case expects on a platform.
    pub fn expected(&self, platform: Platform) -> &Json {
        if self.expect.get("visible").is_some() {
            return &self.expect;
        }
        let key = match platform {
            Platform::Web => "web",
            Platform::Native => "native",
        };
        self.expect
            .get(key)
            .unwrap_or_else(|| panic!("{}: no expectation for {platform:?}", self.name))
    }
}

/// Equal as JSON, except that a number is a number: `1` and `1.0` are the same.
pub fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => x.as_f64() == y.as_f64(),
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

fn pick(from: &Json, fields: &Json) -> Json {
    let mut out = serde_json::Map::new();
    if let Json::Object(fields) = fields {
        for k in fields.keys() {
            out.insert(k.clone(), from.get(k).cloned().unwrap_or(Json::Null));
        }
    }
    Json::Object(out)
}

/// The toolbar in the shape a case pins: the same ids, and the same fields of the ones the case lists.
pub fn describe(toolbar: &Toolbar, want: &Json) -> Json {
    let model = serde_json::to_value(toolbar).expect("the model serialises");
    let controls = model["controls"].as_array().cloned().unwrap_or_default();
    let by_id = |list: &[Json], id: &str| list.iter().find(|c| c["id"] == id).cloned();
    let mut out = serde_json::Map::new();
    out.insert("visible".into(), model["visible"].clone());
    out.insert(
        "controls".into(),
        Json::Array(controls.iter().map(|c| c["id"].clone()).collect()),
    );
    if let Some(Json::Object(pins)) = want.get("pin") {
        let mut got = serde_json::Map::new();
        for (id, fields) in pins {
            let v = by_id(&controls, id).map_or_else(
                || serde_json::json!({ "missing": true }),
                |c| pick(&c, fields),
            );
            got.insert(id.clone(), v);
        }
        out.insert("pin".into(), Json::Object(got));
    }
    if let Some(menu) = want.get("menu") {
        let control = menu["control"].as_str().unwrap_or_default();
        let open = by_id(&controls, control).map(|c| c["menu"].clone());
        let rows = open
            .as_ref()
            .and_then(|m| m["rows"].as_array().cloned())
            .unwrap_or_default();
        let mut got = serde_json::Map::new();
        got.insert("control".into(), Json::String(control.to_owned()));
        got.insert(
            "rows".into(),
            Json::Array(rows.iter().map(|r| r["id"].clone()).collect()),
        );
        if let Some(Json::Object(pins)) = menu.get("pin") {
            let mut pinned = serde_json::Map::new();
            for (id, fields) in pins {
                let v = by_id(&rows, id).map_or_else(
                    || serde_json::json!({ "missing": true }),
                    |r| pick(&r, fields),
                );
                pinned.insert(id.clone(), v);
            }
            got.insert("pin".into(), Json::Object(pinned));
        }
        out.insert("menu".into(), Json::Object(got));
    }
    if let Some(countdown) = want.get("countdown") {
        let v = match (countdown, &model["countdown"]) {
            (Json::Null, _) | (_, Json::Null) => Json::Null,
            (fields, c) => pick(c, fields),
        };
        out.insert("countdown".into(), v);
    }
    for key in ["folded_bar", "hide_tab"] {
        if let Some(fields) = want.get(key) {
            out.insert(key.into(), pick(&model[key], fields));
        }
    }
    Json::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolbar::{build_toolbar, spec};

    #[test]
    fn the_toolbar_cases_parse_and_are_enough() {
        let cases = toolbar_cases();
        assert!(cases.len() >= 20);
        let mut names: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), cases.len(), "case names are unique");
    }

    #[test]
    fn the_shared_toolbar_cases() {
        let mut ran = 0;
        let mut failures = Vec::new();
        for case in toolbar_cases() {
            for platform in [Platform::Web, Platform::Native] {
                if !case.runs_on(platform) {
                    continue;
                }
                ran += 1;
                let want = case.expected(platform);
                let toolbar = build_toolbar(spec(), &case.state(platform), &case.caps(), platform);
                let got = describe(&toolbar, want);
                if !same(&got, want) {
                    failures.push(format!(
                        "{} on {platform:?}:\n  want {want:#}\n  got  {got:#}",
                        case.name
                    ));
                }
            }
        }
        assert!(ran >= 40, "ran only {ran}");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
