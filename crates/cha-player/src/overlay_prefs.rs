//! What the stats panel remembers, saved in `config.json` under `overlay`. The fields, defaults
//! and limits are `prefs.json`'s `stats_panel` group, shared with the browser's `OverlayPrefs`
//! (`statsOverlay.ts`); [`OverlayPrefs::from_json`] reads them through the spec's validator, so
//! a field that is bad falls back on its own and a file never fails to load.

use cha_ui_spec::health::Platform;
use cha_ui_spec::prefs::{self, GroupName, limits};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// The lowest background opacity, in percent.
pub const OPACITY_MIN: u8 = limits::STATS_PANEL_OPACITY_MIN;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    /// The corner whose side of the area the point (the panel's centre) is on.
    pub fn nearest(x: f32, y: f32, width: f32, height: f32) -> Self {
        match (y < height / 2.0, x < width / 2.0) {
            (true, true) => Corner::TopLeft,
            (true, false) => Corner::TopRight,
            (false, true) => Corner::BottomLeft,
            (false, false) => Corner::BottomRight,
        }
    }
}

/// The full view's sections, in the order they are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Section {
    Stream,
    Latency,
    Network,
    Node,
}

impl Section {
    /// In the order of `prefs.json`'s `folded.values`; a test keeps them equal.
    #[cfg(test)]
    pub const ALL: [Section; 4] = [
        Section::Stream,
        Section::Latency,
        Section::Network,
        Section::Node,
    ];
}

/// For `Config`'s `overlay` field: never fails, whatever was saved.
pub fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<OverlayPrefs, D::Error> {
    Ok(OverlayPrefs::from_json(&Value::deserialize(d)?))
}

/// Where free placement left the panel's top left, in points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pos {
    pub left: f32,
    pub top: f32,
}

impl Pos {
    /// Moved to lie wholly inside a `width` x `height` area for a panel of
    /// `w` x `h` (flush to the top left if it can't fit).
    pub fn clamped(self, w: f32, h: f32, width: f32, height: f32) -> Self {
        Self {
            left: self.left.clamp(0.0, (width - w).max(0.0)),
            top: self.top.clamp(0.0, (height - h).max(0.0)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverlayPrefs {
    pub open: bool,
    pub compact: bool,
    pub collapsed: bool,
    pub corner: Corner,
    /// Sections folded to their heading.
    pub folded: Vec<Section>,
    /// Panel background opacity, in percent.
    pub opacity: u8,
    /// On: it snaps to the nearest corner. Off: it stays where it is dropped.
    pub snap: bool,
    pub pos: Option<Pos>,
}

impl Default for OverlayPrefs {
    fn default() -> Self {
        Self::from_json(&Value::Null)
    }
}

impl OverlayPrefs {
    /// Saved settings from parsed JSON: each field valid or its default (`prefs.json`).
    pub fn from_json(saved: &Value) -> Self {
        let valid = prefs::parse(GroupName::StatsPanel, saved, Platform::Native);
        serde_json::from_value(Value::Object(valid))
            .expect("prefs.json's stats_panel fields are OverlayPrefs's")
    }

    /// Keep values in the spec's range, sections once and in order.
    pub fn sanitized(self) -> Self {
        Self::from_json(&serde_json::to_value(&self).unwrap_or(Value::Null))
    }

    pub fn is_folded(&self, section: Section) -> bool {
        self.folded.contains(&section)
    }

    pub fn toggle_section(&mut self, section: Section) {
        if self.is_folded(section) {
            self.folded.retain(|s| *s != section);
        } else {
            self.folded.push(section);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> OverlayPrefs {
        OverlayPrefs::from_json(&serde_json::from_str(text).unwrap_or(Value::Null))
    }

    #[test]
    fn defaults_are_the_specs() {
        let p = OverlayPrefs::default();
        assert!(p.open && !p.compact && !p.collapsed && p.snap);
        assert_eq!((p.corner, p.opacity, p.pos), (Corner::TopLeft, 90, None));
        assert!(p.folded.is_empty());
    }

    #[test]
    fn the_enums_are_the_specs_lists() {
        let fields = &prefs::spec().stats_panel.fields;
        let ids: Vec<String> = Section::ALL
            .iter()
            .map(|s| {
                serde_json::to_value(s)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(ids, fields["folded"].values);
        let mut corners: Vec<String> = [
            Corner::TopLeft,
            Corner::TopRight,
            Corner::BottomLeft,
            Corner::BottomRight,
        ]
        .iter()
        .map(|c| {
            serde_json::to_value(c)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
        corners.sort();
        let mut want = fields["corner"].values.clone();
        want.sort();
        assert_eq!(corners, want);
    }

    #[test]
    fn partial_json_fills_in_defaults_and_bad_values_are_repaired() {
        let p = parse(
            r#"{"compact": true, "corner": "bottom-right", "opacity": 5,
                "folded": ["node", "stream", "node", "bogus"], "pos": {"left": 4, "top": 9}}"#,
        );
        assert!(p.compact && p.open);
        assert_eq!(p.corner, Corner::BottomRight);
        assert_eq!(p.opacity, OPACITY_MIN);
        assert_eq!(p.folded, [Section::Stream, Section::Node]);
        assert_eq!(
            p.pos,
            Some(Pos {
                left: 4.0,
                top: 9.0
            })
        );
        // One bad field no longer costs the rest.
        let p = parse(r#"{"corner": "middle", "compact": true, "opacity": 70.4}"#);
        assert!(p.compact);
        assert_eq!((p.corner, p.opacity), (Corner::TopLeft, 70));
    }

    #[test]
    fn whatever_was_saved_loads() {
        for text in [
            "",
            "{nope",
            "null",
            "[]",
            "7",
            r#""overlay""#,
            r#"{"pos": 3}"#,
        ] {
            assert_eq!(parse(text), OverlayPrefs::default(), "{text}");
        }
    }

    #[test]
    fn a_pane_saves_and_loads_what_it_holds() {
        let p = OverlayPrefs {
            folded: vec![Section::Network],
            pos: Some(Pos {
                left: 0.1,
                top: 12.5,
            }),
            snap: false,
            ..OverlayPrefs::default()
        };
        assert_eq!(p.clone().sanitized(), p);
        let text = serde_json::to_string(&p).unwrap();
        assert_eq!(parse(&text), p);
    }

    #[test]
    fn the_shared_cases() {
        use cha_ui_spec::prefs_cases::{cases, normalised};
        let mut run = 0;
        for c in cases()
            .iter()
            .filter(|c| c.group == GroupName::StatsPanel && c.runs_on(Platform::Native))
        {
            let saved = c.saved_text().unwrap_or_default();
            let got = parse(&saved);
            assert_eq!(
                normalised(&serde_json::to_value(&got).unwrap()),
                Value::Object(c.expected(Platform::Native)),
                "{}: {saved}",
                c.name
            );
            run += 1;
        }
        assert!(run >= 10);
    }

    #[test]
    fn clamping_keeps_the_panel_inside() {
        let p = Pos {
            left: 900.0,
            top: -5.0,
        };
        assert_eq!(
            p.clamped(256.0, 300.0, 1000.0, 800.0),
            Pos {
                left: 744.0,
                top: 0.0
            }
        );
        // A panel bigger than the area sits flush top left.
        assert_eq!(
            p.clamped(2000.0, 2000.0, 1000.0, 800.0),
            Pos {
                left: 0.0,
                top: 0.0
            }
        );
    }

    #[test]
    fn the_nearest_corner_is_the_centres_quadrant() {
        assert_eq!(Corner::nearest(10.0, 10.0, 100.0, 100.0), Corner::TopLeft);
        assert_eq!(Corner::nearest(90.0, 10.0, 100.0, 100.0), Corner::TopRight);
        assert_eq!(
            Corner::nearest(10.0, 90.0, 100.0, 100.0),
            Corner::BottomLeft
        );
        assert_eq!(
            Corner::nearest(90.0, 90.0, 100.0, 100.0),
            Corner::BottomRight
        );
    }

    #[test]
    fn sections_toggle() {
        let mut p = OverlayPrefs::default();
        p.toggle_section(Section::Node);
        assert!(p.is_folded(Section::Node));
        p.toggle_section(Section::Node);
        assert!(!p.is_folded(Section::Node));
    }
}
