//! The in-stream UI's spec for the native player (ADR 0016).
//!
//! `web/packages/ui-spec` holds the JSON; this crate compiles it in, types it,
//! and holds the Rust side of what both players share. It has no egui and no
//! platform dependencies, so it builds and tests on Linux CI.
//!
//! - [`Icon`]: every icon in `icons.json`, as an enum generated at build time.
//! - [`path`]: an SVG path-data parser and flattener the icons are drawn with.
//! - [`fill`]: convexity and ear clipping for filled parts.
//! - [`health`]: the health grade's constants, bands and texts, and the template `fill`.
//! - [`health_cases`]: the shared test vectors (feature `cases`).
//! - [`panel`]: the stats panel's spec and `build_panel`, the model both players draw; and
//!   [`panel_cases`], its shared cases (feature `cases`).
//! - [`THEMES`]: the generated built-in themes, as text.

pub mod fill;
pub mod health;
#[cfg(any(test, feature = "cases"))]
pub mod health_cases;
pub mod panel;
#[cfg(any(test, feature = "cases"))]
pub mod panel_cases;
pub mod path;

use std::sync::LazyLock;

use serde::Deserialize;

use path::Path;

include!(concat!(env!("OUT_DIR"), "/icon_enum.rs"));

/// The generated built-in themes: id and JSON text. Parsing stays with the
/// player, which owns the palette types.
pub const THEMES: [(&str, &str); 2] = [
    (
        "cha-magenta",
        include_str!("../../../web/packages/ui-spec/themes/cha-magenta.json"),
    ),
    (
        "cha-jade",
        include_str!("../../../web/packages/ui-spec/themes/cha-jade.json"),
    ),
];

const ICONS_JSON: &str = include_str!("../../../web/packages/ui-spec/icons.json");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

/// How a part is painted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    /// Stroked, this many units wide.
    Stroke(f32),
    Fill,
}

/// One part of an icon, its path data parsed.
#[derive(Clone, Debug)]
pub struct Part {
    pub d: String,
    pub paint: Paint,
    pub path: Path,
}

/// An icon: parts drawn in a square `view_box` units wide.
#[derive(Clone, Debug)]
pub struct IconSpec {
    pub view_box: f32,
    pub parts: Vec<Part>,
    /// `None` is the SVG default (butt).
    pub linecap: Option<LineCap>,
    /// `None` is the SVG default (miter).
    pub linejoin: Option<LineJoin>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPart {
    d: String,
    stroke: Option<f32>,
    fill: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIcon {
    #[serde(rename = "viewBox")]
    view_box: f32,
    parts: Vec<RawPart>,
    linecap: Option<LineCap>,
    linejoin: Option<LineJoin>,
}

static ICONS: LazyLock<Vec<IconSpec>> = LazyLock::new(|| {
    let raw: std::collections::BTreeMap<String, RawIcon> =
        serde_json::from_str(ICONS_JSON).expect("icons.json parses");
    // `Icon::ALL` is sorted by id, as is a BTreeMap.
    let specs: Vec<IconSpec> = raw
        .into_iter()
        .map(|(id, icon)| IconSpec {
            view_box: icon.view_box,
            linecap: icon.linecap,
            linejoin: icon.linejoin,
            parts: icon
                .parts
                .into_iter()
                .map(|p| Part {
                    paint: match (p.stroke, p.fill) {
                        (Some(w), None | Some(false)) => Paint::Stroke(w),
                        (None, Some(true)) => Paint::Fill,
                        _ => panic!("icon {id}: a part needs exactly one of stroke and fill"),
                    },
                    path: Path::parse(&p.d)
                        .unwrap_or_else(|e| panic!("icon {id}: bad path {:?}: {e}", p.d)),
                    d: p.d,
                })
                .collect(),
        })
        .collect();
    assert_eq!(
        specs.len(),
        Icon::ALL.len(),
        "icons.json and Icon::ALL agree"
    );
    specs
});

impl Icon {
    /// The icon's parts, parsed once.
    pub fn spec(self) -> &'static IconSpec {
        // ALL is sorted by id and the enum is declared in that order.
        &ICONS[self as usize]
    }

    pub fn from_id(id: &str) -> Option<Icon> {
        Icon::ALL.iter().copied().find(|i| i.id() == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_parses_and_stays_in_its_box() {
        for icon in Icon::ALL {
            let spec = icon.spec();
            assert_eq!(spec.view_box, 16.0);
            assert!(!spec.parts.is_empty(), "{}", icon.id());
            for part in &spec.parts {
                let subs = part.path.flatten(1.0, 0.01);
                assert!(!subs.is_empty(), "{} {}", icon.id(), part.d);
                for s in &subs {
                    for &(x, y) in &s.points {
                        assert!(
                            (-0.01..=16.01).contains(&x) && (-0.01..=16.01).contains(&y),
                            "{}: ({x}, {y}) is outside the box",
                            icon.id()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ids_round_trip() {
        for icon in Icon::ALL {
            assert_eq!(Icon::from_id(icon.id()), Some(icon));
        }
        assert_eq!(Icon::SoundMuted.id(), "sound-muted");
        assert_eq!(Icon::from_id("nope"), None);
    }

    #[test]
    fn the_themes_are_json_with_all_four_variants() {
        for (id, text) in THEMES {
            let v: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(v["id"], id);
            for k in ["dark", "light", "dark-more", "light-more"] {
                assert!(v["variants"][k].is_object(), "{id} {k}");
            }
        }
    }
}
