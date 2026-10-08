//! The stats overlay sits on the video, so it stays dark whatever the
//! launcher's light or dark choice: it uses the theme's `dark` palette (the
//! `dark-more` one when contrast is More), with `panel` as its background at
//! the user's opacity, and `line`, `ink`, `ink-2`, the status colours and
//! `chart-1..3` for the rest. A theme still tints it, through its palette.

use std::sync::Arc;

use egui::Color32;

use super::palette::Palette;

#[derive(Clone, Debug, PartialEq)]
pub struct OverlayStyle {
    /// The colour roles to draw with: a dark palette.
    pub palette: Arc<Palette>,
    pub radius: f32,
    pub font_size: f32,
    /// A text line's height, and the panel's width when full.
    pub line_height: f32,
    pub width: f32,
}

impl OverlayStyle {
    pub fn from_palette(dark: &Palette) -> Self {
        Self {
            palette: Arc::new(dark.clone()),
            radius: 8.0,
            font_size: 11.0,
            line_height: 20.0,
            width: 256.0,
        }
    }

    /// The panel's background at `opacity` percent.
    pub fn fill(&self, opacity: u8) -> Color32 {
        self.palette
            .panel
            .gamma_multiply(f32::from(opacity.min(100)) / 100.0)
    }
}
