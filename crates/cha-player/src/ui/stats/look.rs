//! Colours and text drawing for one frame of the panel.

use std::sync::Arc;

use cha_ui_spec::panel::{SectionColor, Tone, spec};
use egui::epaint::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, FontId, Galley, Painter, Pos2, Rect, pos2, vec2};

use crate::health::Grade;
use crate::overlay_prefs::OPACITY_MIN;
use crate::theme::{OverlayStyle, Palette};

/// Colours and text drawing for one frame: the palette, with the dimmer text
/// moved toward full ink and given a dark halo as the background gets more
/// see-through.
pub(in crate::ui) struct Look {
    pub(in crate::ui) p: Arc<Palette>,
    pub(in crate::ui) fill: Color32,
    pub(in crate::ui) ink2: Color32,
    /// 0 at full opacity, 1 at the lowest.
    halo: f32,
    pub(in crate::ui) font: FontId,
}

impl Look {
    pub(in crate::ui) fn new(style: &OverlayStyle, opacity: u8) -> Self {
        let p = style.palette.clone();
        let t = ((100.0 - f32::from(opacity)) / (100.0 - f32::from(OPACITY_MIN))).clamp(0.0, 1.0);
        Self {
            ink2: p.ink_2.lerp_to_gamma(p.ink, t),
            fill: style.fill(opacity),
            halo: t,
            font: FontId::monospace(style.font_size),
            p,
        }
    }

    pub(super) fn galley(&self, painter: &Painter, text: &str, color: Color32) -> Arc<Galley> {
        painter.layout_no_wrap(text.to_string(), self.font.clone(), color)
    }

    pub(super) fn paint(&self, painter: &Painter, at: Pos2, galley: Arc<Galley>) {
        if self.halo > 0.02 {
            let halo = Color32::from_black_alpha(((0.3 + 0.5 * self.halo) * 255.0) as u8);
            for d in [
                vec2(-1.0, 0.0),
                vec2(1.0, 0.0),
                vec2(0.0, -1.0),
                vec2(0.0, 1.0),
            ] {
                painter.galley_with_override_text_color(at + d * 0.8, galley.clone(), halo);
            }
        }
        painter.galley(at, galley, self.p.ink);
    }

    /// Draw `text` with its left (or right) edge at `x`, centred on `y`.
    pub(super) fn text(
        &self,
        painter: &Painter,
        x: f32,
        y: f32,
        align: Align,
        text: &str,
        color: Color32,
    ) -> Rect {
        let galley = self.galley(painter, text, color);
        let size = galley.size();
        let left = if align == Align::Max { x - size.x } else { x };
        let at = pos2(left, y - size.y / 2.0);
        let rect = Rect::from_min_size(at, size);
        self.paint(painter, at, galley);
        rect
    }

    /// `text` on one line within `max_w`, ending in "…" if cut.
    pub(super) fn fitted(
        &self,
        painter: &Painter,
        text: &str,
        color: Color32,
        max_w: f32,
    ) -> Arc<Galley> {
        let mut job = LayoutJob::single_section(
            text.to_string(),
            TextFormat::simple(self.font.clone(), color),
        );
        job.wrap.max_width = max_w.max(0.0);
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.wrap.overflow_character = Some('…');
        painter.layout_job(job)
    }

    /// The colour of a spec tone, or `None` for the normal ink.
    pub(super) fn tone(&self, tone: Tone) -> Option<Color32> {
        match tone {
            Tone::None => None,
            Tone::Ok => Some(self.p.ok),
            Tone::Warn => Some(self.p.warn),
            Tone::Danger => Some(self.p.danger),
            Tone::Dim => Some(self.ink2),
        }
    }

    /// A grade letter's colour, from the spec's tones (A and B ok, C and D warn, F danger, none dim).
    pub(in crate::ui) fn grade_color(&self, grade: Option<Grade>) -> Color32 {
        let t = &spec().grade_tone;
        let tone = match grade {
            Some(Grade::A) => t.a,
            Some(Grade::B) => t.b,
            Some(Grade::C) => t.c,
            Some(Grade::D) => t.d,
            Some(Grade::F) => t.f,
            None => t.none,
        };
        self.tone(tone).unwrap_or(self.p.ink)
    }

    /// An issue's title: minor in the dim ink, major amber, critical red.
    pub(super) fn title_color(&self, s: crate::health::Severity) -> Color32 {
        use crate::health::Severity;
        match s {
            Severity::Minor => self.ink2,
            Severity::Major => self.p.warn,
            Severity::Critical => self.p.danger,
        }
    }

    pub(super) fn section_color(&self, c: SectionColor) -> Color32 {
        match c {
            SectionColor::Accent => self.p.accent,
            SectionColor::Chart1 => self.p.chart_1,
            SectionColor::Chart2 => self.p.chart_2,
            SectionColor::Chart3 => self.p.chart_3,
        }
    }
}
