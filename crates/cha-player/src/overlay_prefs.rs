//! What the stats panel remembers: the same shape and defaults as
//! `OverlayPrefs` in the portal's `statsOverlay.ts`, saved in `config.json`.

use serde::{Deserialize, Serialize};

/// The lowest background opacity, in percent.
pub const OPACITY_MIN: u8 = 30;

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
    pub const ALL: [Section; 4] = [
        Section::Stream,
        Section::Latency,
        Section::Network,
        Section::Node,
    ];
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
#[serde(default)]
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
        Self {
            open: true,
            compact: false,
            collapsed: false,
            corner: Corner::TopLeft,
            folded: Vec::new(),
            opacity: 90,
            snap: true,
            pos: None,
        }
    }
}

impl OverlayPrefs {
    /// Keep hand-edited values in range, sections once and in order.
    pub fn sanitized(mut self) -> Self {
        self.opacity = self.opacity.clamp(OPACITY_MIN, 100);
        let folded = self.folded;
        self.folded = Section::ALL
            .into_iter()
            .filter(|s| folded.contains(s))
            .collect();
        if self
            .pos
            .is_some_and(|p| !p.left.is_finite() || !p.top.is_finite())
        {
            self.pos = None;
        }
        self
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

    #[test]
    fn defaults_are_the_browsers() {
        let p = OverlayPrefs::default();
        assert!(p.open && !p.compact && !p.collapsed && p.snap);
        assert_eq!((p.corner, p.opacity, p.pos), (Corner::TopLeft, 90, None));
    }

    #[test]
    fn partial_json_fills_in_defaults_and_bad_values_are_repaired() {
        let p: OverlayPrefs = serde_json::from_str(
            r#"{"compact": true, "corner": "bottom-right", "opacity": 5,
                "folded": ["node", "stream", "node"], "pos": {"left": 4, "top": 9}}"#,
        )
        .unwrap();
        let p = p.sanitized();
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
