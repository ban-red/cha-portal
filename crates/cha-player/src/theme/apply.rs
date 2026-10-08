//! The one place palette and metrics become an `egui::Style`.

use std::sync::Arc;

use egui::style::{Selection, TextCursorStyle, WidgetVisuals};
use egui::{
    Color32, CornerRadius, FontFamily, FontId, Id, Margin, Stroke, Style, TextStyle, Theme,
    Visuals, vec2,
};

use super::overlay::OverlayStyle;
use super::palette::Palette;
use super::resolve::Resolved;

const PALETTE_ID: &str = "cha-theme-palette";
const OVERLAY_ID: &str = "cha-theme-overlay";

/// Style the context. Call when the resolved theme changes, not every frame.
/// Fonts are set separately (they are costly; see [`super::fonts`]).
pub fn apply(ctx: &egui::Context, resolved: &Resolved) {
    let style = Arc::new(style(resolved));
    // Both of egui's themes get the same style; `set_theme` picks which one
    // is live, so egui's own system-theme following can't undo this.
    ctx.set_style_of(Theme::Dark, style.clone());
    ctx.set_style_of(Theme::Light, style);
    ctx.set_theme(if resolved.variant.is_dark() {
        Theme::Dark
    } else {
        Theme::Light
    });
    ctx.set_zoom_factor(resolved.scale);
    ctx.data_mut(|d| {
        d.insert_temp(Id::new(PALETTE_ID), resolved.palette.clone());
        d.insert_temp(Id::new(OVERLAY_ID), resolved.overlay.clone());
    });
}

/// The active palette, for UI code that wants "danger" and "ink-2" rather
/// than RGB.
pub trait ThemeExt {
    fn palette(&self) -> Arc<Palette>;
    fn overlay_style(&self) -> OverlayStyle;
}

impl ThemeExt for egui::Context {
    fn palette(&self) -> Arc<Palette> {
        self.data(|d| d.get_temp::<Arc<Palette>>(Id::new(PALETTE_ID)))
            .unwrap_or_else(|| Arc::new(Palette::fallback()))
    }

    fn overlay_style(&self) -> OverlayStyle {
        self.data(|d| d.get_temp::<OverlayStyle>(Id::new(OVERLAY_ID)))
            .unwrap_or_else(|| OverlayStyle::from_palette(&Palette::fallback()))
    }
}

impl ThemeExt for egui::Ui {
    fn palette(&self) -> Arc<Palette> {
        self.ctx().palette()
    }

    fn overlay_style(&self) -> OverlayStyle {
        self.ctx().overlay_style()
    }
}

fn radius(r: f32) -> CornerRadius {
    CornerRadius::same(r.round().clamp(0.0, 255.0) as u8)
}

pub fn style(r: &Resolved) -> Style {
    let p = &*r.palette;
    let m = &r.metrics;
    let dark = r.variant.is_dark();
    let stroke = |c: Color32| Stroke::new(m.stroke_width, c);
    let strong = |c: Color32| Stroke::new(m.stroke_width_strong, c);
    let medium = radius(m.radius_medium);

    let mut visuals = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    visuals.dark_mode = dark;
    visuals.override_text_color = None;
    visuals.weak_text_color = Some(p.ink_3);
    visuals.hyperlink_color = p.accent;
    visuals.faint_bg_color = p.panel;
    visuals.extreme_bg_color = p.panel_2;
    visuals.text_edit_bg_color = Some(p.panel_2);
    visuals.code_bg_color = p.panel_2;
    visuals.warn_fg_color = p.warn;
    visuals.error_fg_color = p.danger;
    visuals.panel_fill = p.canvas;
    visuals.window_fill = p.panel;
    visuals.window_stroke = stroke(p.line_strong.lerp_to_gamma(p.line, 0.5));
    visuals.window_corner_radius = radius(m.radius_large);
    visuals.menu_corner_radius = medium;
    let shadow = Color32::from_black_alpha(if dark { 110 } else { 36 });
    visuals.window_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: shadow,
    };
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: shadow,
    };
    visuals.selection = Selection {
        bg_fill: p.accent_soft,
        stroke: strong(p.accent),
    };
    visuals.text_cursor = TextCursorStyle {
        stroke: Stroke::new(2.0, p.accent),
        ..Default::default()
    };

    let widget = |bg: Color32, border: Stroke, fg: Color32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: border,
        corner_radius: medium,
        fg_stroke: Stroke::new(1.0, fg),
        expansion: 0.0,
    };
    let hover_fill = p.panel_2.lerp_to_gamma(p.ink, 0.08);
    visuals.widgets.noninteractive = widget(p.panel, stroke(p.line), p.ink);
    visuals.widgets.inactive = widget(p.panel_2, stroke(p.line), p.ink);
    visuals.widgets.hovered = widget(hover_fill, stroke(p.line_strong), p.ink);
    visuals.widgets.active = widget(p.accent_soft, strong(p.accent), p.ink);
    visuals.widgets.open = widget(p.panel_2, stroke(p.line_strong), p.ink);

    let mut style = Style {
        visuals,
        ..Style::default()
    };
    style.spacing.item_spacing = vec2(m.item_spacing_x, m.item_spacing_y);
    style.spacing.button_padding = vec2(m.button_padding_x, m.button_padding_y);
    style.spacing.window_margin = Margin::same(m.window_margin.round() as i8);
    style.spacing.menu_margin = Margin::same((m.window_margin / 2.0).round() as i8);
    style.spacing.interact_size.y = m.control_height;
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(m.font_heading, super::fonts::bold_family()),
        ),
        (
            TextStyle::Body,
            FontId::new(m.font_body, FontFamily::Proportional),
        ),
        (
            TextStyle::Button,
            FontId::new(m.font_button, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(m.font_small, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(m.font_monospace, FontFamily::Monospace),
        ),
    ]
    .into();
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::registry::Registry;
    use crate::theme::resolve::{Appearance, SystemEnv, ThemePrefs, resolve};

    fn resolved(appearance: Appearance) -> Resolved {
        resolve(
            &Registry::builtin(),
            &ThemePrefs {
                appearance,
                ..ThemePrefs::default()
            },
            &SystemEnv::default(),
        )
    }

    #[test]
    fn style_uses_the_palette() {
        let r = resolved(Appearance::Dark);
        let s = style(&r);
        assert!(s.visuals.dark_mode);
        assert_eq!(s.visuals.panel_fill, r.palette.canvas);
        assert_eq!(s.visuals.window_fill, r.palette.panel);
        assert_eq!(s.visuals.hyperlink_color, r.palette.accent);
        assert_eq!(s.visuals.error_fg_color, r.palette.danger);
        assert_eq!(s.visuals.widgets.inactive.weak_bg_fill, r.palette.panel_2);
        assert_eq!(s.text_styles[&TextStyle::Body].size, r.metrics.font_body);
        assert_ne!(resolved(Appearance::Light).palette.canvas, r.palette.canvas);
        assert!(!style(&resolved(Appearance::Light)).visuals.dark_mode);
    }

    #[test]
    fn apply_stashes_the_palette_and_sets_the_theme() {
        let ctx = egui::Context::default();
        assert_eq!(*ctx.palette(), Palette::fallback());
        let r = resolved(Appearance::Light);
        apply(&ctx, &r);
        assert_eq!(*ctx.palette(), *r.palette);
        assert_eq!(ctx.theme(), Theme::Light);
        assert_eq!(ctx.global_style().visuals.panel_fill, r.palette.canvas);
        assert_eq!(ctx.zoom_factor(), 1.0);
    }
}
