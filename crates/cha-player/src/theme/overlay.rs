//! The stats overlay sits on the video, so it is not themed like the
//! launcher: always dark and high contrast. It borrows the theme's
//! `dark-more` palette for text and status colours, so a theme still tints it.

use egui::Color32;

use super::palette::Palette;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayStyle {
    /// Behind the text: near-black whatever the video shows.
    pub fill: Color32,
    pub text: Color32,
    pub warn: Color32,
    pub radius: f32,
    pub margin: f32,
    pub font_size: f32,
}

impl OverlayStyle {
    pub const FILL: Color32 = Color32::from_black_alpha(190);

    /// Text and status colours from `dark_more`, which every theme defines
    /// for a dark background.
    pub fn from_palette(dark_more: &Palette) -> Self {
        Self {
            fill: Self::FILL,
            text: dark_more.ink,
            warn: dark_more.warn,
            radius: 6.0,
            margin: 8.0,
            font_size: 13.0,
        }
    }
}
