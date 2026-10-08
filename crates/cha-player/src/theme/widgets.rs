//! Controls the portal has and egui doesn't: an accent (primary) button, a
//! danger button, status text, cards. All colours are roles.

use egui::{Color32, Frame, Margin, Response, RichText, Sense, Stroke, UiBuilder, WidgetText};

use super::apply::ThemeExt;
use super::fonts::bold_family;
use super::palette::Palette;

/// Text in the bold face (semibold SF Pro, or the theme's bold font), for
/// headings and strong text. egui's own `strong()` only changes the colour.
pub fn bold(text: impl Into<String>) -> RichText {
    RichText::new(text).family(bold_family())
}

/// A status colour, as the portal's `text-ok`, `text-warn`, ...
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Danger,
    Info,
}

impl Status {
    pub fn color(self, ui: &egui::Ui) -> Color32 {
        let p = ui.palette();
        match self {
            Status::Ok => p.ok,
            Status::Warn => p.warn,
            Status::Danger => p.danger,
            Status::Info => p.info,
        }
    }
}

pub fn status_text(ui: &egui::Ui, status: Status, text: impl Into<String>) -> RichText {
    RichText::new(text).color(status.color(ui))
}

pub fn status_label(ui: &mut egui::Ui, status: Status, text: impl Into<String>) -> Response {
    let text = status_text(ui, status, text);
    ui.label(text)
}

/// WCAG relative luminance of an opaque colour.
fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG contrast ratio between two opaque colours (1 to 21).
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The least contrast a disabled button's text keeps against its fill.
pub const DISABLED_MIN_CONTRAST: f32 = 3.0;

/// A disabled filled button: the fill pulled toward `panel-2` so it reads as
/// switched off, and text that still reads. Returns `(fill, text)`.
///
/// egui's own disabled look halves the alpha of both, which on a light theme
/// leaves white text on a pale fill.
pub fn disabled_colors(p: &Palette, fill: Color32, fg: Color32) -> (Color32, Color32) {
    let bg = fill.lerp_to_gamma(p.panel_2, 0.6);
    // The text colour with the most contrast against the muted fill, drawn
    // back toward the fill as far as the contrast allows.
    let base = if contrast_ratio(fg, bg) >= contrast_ratio(p.ink, bg) {
        fg
    } else {
        p.ink
    };
    let mut text = base;
    // Margin over the bound, since the colour is rounded to 8 bits.
    for step in (0..=20).rev() {
        let candidate = bg.lerp_to_gamma(base, 0.5 + step as f32 * 0.025);
        if contrast_ratio(candidate, bg) >= DISABLED_MIN_CONTRAST + 0.15 {
            text = candidate;
        } else {
            break;
        }
    }
    (bg, text)
}

fn filled_button(
    ui: &mut egui::Ui,
    enabled: bool,
    text: impl Into<WidgetText>,
    fill: Color32,
    hover: Color32,
    fg: Color32,
) -> Response {
    let text = text.into();
    let (off_fill, off_text) = disabled_colors(&ui.palette(), fill, fg);
    // Disabled: the muted colours, in every state, and no fade (egui's own
    // halves the alpha of fill and text).
    let (fill, hover, fg) = if enabled {
        (fill, hover, fg)
    } else {
        (off_fill, off_fill, off_text)
    };
    ui.scope(|ui| {
        if !enabled {
            ui.visuals_mut().disabled_alpha = 1.0;
        }
        let w = &mut ui.visuals_mut().widgets;
        for (state, bg) in [
            (&mut w.noninteractive, fill),
            (&mut w.inactive, fill),
            (&mut w.hovered, hover),
            (&mut w.active, hover),
        ] {
            state.weak_bg_fill = bg;
            state.bg_fill = bg;
            state.bg_stroke = Stroke::NONE;
            state.fg_stroke = Stroke::new(1.0, fg);
        }
        ui.add_enabled(enabled, egui::Button::new(text))
    })
    .inner
}

/// The portal's primary button: accent fill, `on-accent` text.
pub fn primary_button_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    text: impl Into<WidgetText>,
) -> Response {
    let p = ui.palette();
    filled_button(
        ui,
        enabled,
        text,
        p.accent_fill,
        p.accent_fill_hover,
        p.on_accent,
    )
}

/// The destructive button: `danger-fill` with `on-danger` text.
pub fn danger_button(ui: &mut egui::Ui, enabled: bool, text: impl Into<WidgetText>) -> Response {
    let p = ui.palette();
    let hover = p.danger_fill.lerp_to_gamma(Color32::BLACK, 0.18);
    filled_button(ui, enabled, text, p.danger_fill, hover, p.on_danger)
}

/// The border of a card at rest: `line`, most of the way to `line-strong`,
/// so cards show on a light canvas.
fn card_border(p: &super::Palette) -> Color32 {
    p.line_strong.lerp_to_gamma(p.line, 0.35)
}

/// A message box like the portal's alert: the status colour at 12% over
/// `panel`, a border 40% of it over `line`, text in the status colour, and an
/// optional button at the right. True when the button was clicked.
pub fn callout(ui: &mut egui::Ui, status: Status, text: &str, action: Option<&str>) -> bool {
    let p = ui.palette();
    let color = status.color(ui);
    let radius = ui.visuals().widgets.inactive.corner_radius;
    let width = ui.visuals().widgets.noninteractive.bg_stroke.width;
    let mut clicked = false;
    Frame::new()
        .fill(p.panel.lerp_to_gamma(color, 0.12))
        .stroke(Stroke::new(width, p.line.lerp_to_gamma(color, 0.4)))
        .corner_radius(radius)
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // The text gets the width the button leaves, so the button ends
            // at the right edge however many lines the text wraps to.
            let spacing = ui.spacing().item_spacing.x;
            let button_width = action.map_or(0.0, |label| {
                let galley = ui.painter().layout_no_wrap(
                    label.to_owned(),
                    egui::TextStyle::Button.resolve(ui.style()),
                    Color32::PLACEHOLDER,
                );
                galley.size().x + 2.0 * ui.spacing().button_padding.x + spacing
            });
            let text_width = (ui.available_width() - button_width).max(40.0);
            ui.horizontal(|ui| {
                ui.scope(|ui| {
                    ui.set_width(text_width - spacing);
                    ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
                });
                if let Some(label) = action {
                    clicked = ui.button(label).clicked();
                }
            });
        });
    ui.add_space(6.0);
    clicked
}

/// A heading for a group of settings: `ink`, bold, a little above body size.
pub fn section_heading(ui: &mut egui::Ui, text: &str) {
    let p = ui.palette();
    let size = ui.style().text_styles[&egui::TextStyle::Body].size * 1.15;
    ui.add_space(6.0);
    ui.label(bold(text).size(size).color(p.ink));
    ui.add_space(4.0);
}

/// A card: `panel` surface with a `line` border. Returns the frame's
/// response (not clickable); see [`selectable_card`].
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    let p = ui.palette();
    let radius = ui.visuals().widgets.inactive.corner_radius;
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(stroke.width, card_border(&p)))
        .corner_radius(radius)
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
}

/// A card that is clicked: accent-soft with an accent border when selected,
/// a stronger border under the pointer.
pub fn selectable_card<R>(
    ui: &mut egui::Ui,
    selected: bool,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let p = ui.palette();
    let radius = ui.visuals().widgets.inactive.corner_radius;
    let (strong, thin) = {
        let w = &ui.visuals().widgets;
        (w.active.bg_stroke.width, w.noninteractive.bg_stroke.width)
    };
    ui.scope_builder(UiBuilder::new().sense(Sense::click()), |ui| {
        let hovered = ui.response().hovered();
        let (fill, border) = if selected {
            (p.accent_soft, Stroke::new(strong, p.accent))
        } else if hovered {
            (p.panel_2, Stroke::new(thin, p.line_strong))
        } else {
            (p.panel, Stroke::new(thin, card_border(&p)))
        };
        Frame::new()
            .fill(fill)
            .stroke(border)
            .corner_radius(radius)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui)
            })
            .inner
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::registry::Registry;
    use crate::theme::resolve::Variant;

    #[test]
    fn disabled_buttons_keep_their_text_readable_in_every_variant() {
        let registry = Registry::builtin();
        let mut worst = f32::MAX;
        for theme in registry.themes() {
            for variant in Variant::ALL {
                let p = theme.palette(variant);
                for (kind, fill, fg) in [
                    ("primary", p.accent_fill, p.on_accent),
                    ("danger", p.danger_fill, p.on_danger),
                ] {
                    let (bg, text) = disabled_colors(p, fill, fg);
                    let ratio = contrast_ratio(text, bg);
                    println!("{} {variant:?} {kind}: {ratio:.2}:1", theme.id);
                    assert!(
                        ratio >= DISABLED_MIN_CONTRAST,
                        "{} {variant:?} {kind}: {ratio:.2}",
                        theme.id
                    );
                    assert_ne!(bg, fill, "muted");
                    worst = worst.min(ratio);
                }
            }
        }
        println!("lowest: {worst:.2}:1");
    }
}
