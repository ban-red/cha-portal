//! What the stats panel and the toolbar share: the overlays' look (colours, text drawing), the
//! lit background under a hovered button and the icon button on it, the shadowed box and the
//! widget style of a menu, the chips of a few fixed choices, and the rule for which pointer
//! events belong to an overlay rather than the stream.

use std::sync::Arc;

use cha_ui_spec::panel::{SectionColor, Tone};
use cha_ui_spec::toolbar::ToolbarTone;
use egui::epaint::Shadow;
use egui::epaint::text::{LayoutJob, TextFormat};
use egui::{
    Align, Color32, FontId, Galley, Id, Painter, Pos2, Rect, Response, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2, pos2, vec2,
};
use winit::event::{ElementState, WindowEvent};

use crate::overlay_prefs::OPACITY_MIN;
use crate::theme::icons::{self, Icon};
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

    pub(in crate::ui) fn galley(
        &self,
        painter: &Painter,
        text: &str,
        color: Color32,
    ) -> Arc<Galley> {
        painter.layout_no_wrap(text.to_string(), self.font.clone(), color)
    }

    pub(in crate::ui) fn paint(&self, painter: &Painter, at: Pos2, galley: Arc<Galley>) {
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
    pub(in crate::ui) fn text(
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
    pub(in crate::ui) fn fitted(
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
    pub(in crate::ui) fn tone(&self, tone: Tone) -> Option<Color32> {
        match tone {
            Tone::None => None,
            Tone::Ok => Some(self.p.ok),
            Tone::Warn => Some(self.p.warn),
            Tone::Danger => Some(self.p.danger),
            Tone::Dim => Some(self.ink2),
        }
    }

    /// The colour of a toolbar tone, or `None` for the button's own default.
    pub(in crate::ui) fn toolbar_tone(&self, tone: ToolbarTone) -> Option<Color32> {
        match tone {
            ToolbarTone::None => None,
            ToolbarTone::Ok => Some(self.p.ok),
            ToolbarTone::Accent => Some(self.p.accent),
            ToolbarTone::Warn => Some(self.p.warn),
            ToolbarTone::Danger => Some(self.p.danger),
            ToolbarTone::Dim => Some(self.ink2),
            ToolbarTone::Faint => Some(self.p.ink_3),
        }
    }

    /// An issue's title: minor in the dim ink, major amber, critical red.
    pub(in crate::ui) fn title_color(&self, s: crate::health::Severity) -> Color32 {
        use crate::health::Severity;
        match s {
            Severity::Minor => self.ink2,
            Severity::Major => self.p.warn,
            Severity::Critical => self.p.danger,
        }
    }

    pub(in crate::ui) fn section_color(&self, c: SectionColor) -> Color32 {
        match c {
            SectionColor::Accent => self.p.accent,
            SectionColor::Chart1 => self.p.chart_1,
            SectionColor::Chart2 => self.p.chart_2,
            SectionColor::Chart3 => self.p.chart_3,
        }
    }
}

// ---- the lit background and the icon button on it ------------------------

/// The background under a hovered or switched-on button.
pub(in crate::ui) fn lit(painter: &Painter, look: &Look, rect: Rect, radius: f32) {
    painter.rect_filled(rect, radius, look.p.line.gamma_multiply(0.6));
}

/// How an icon button looks and behaves.
#[derive(Clone, Copy)]
pub(in crate::ui) struct IconStyle {
    /// The icon's side, in points.
    pub(in crate::ui) icon_px: f32,
    /// The lit background's corner radius.
    pub(in crate::ui) radius: f32,
    /// The icon's colour at rest (the secondary ink if none).
    pub(in crate::ui) color: Option<Color32>,
    /// The icon's colour under the pointer (the ink if none).
    pub(in crate::ui) hover: Option<Color32>,
    /// Switched on, or its menu is open: a lit background.
    pub(in crate::ui) active: bool,
    /// Whether the lit one also takes the hovered icon colour.
    pub(in crate::ui) ink_when_active: bool,
    pub(in crate::ui) enabled: bool,
    /// False while the bar is fading out: it takes no clicks and no hover.
    pub(in crate::ui) live: bool,
}

impl IconStyle {
    pub(in crate::ui) const fn new(icon_px: f32, radius: f32) -> Self {
        Self {
            icon_px,
            radius,
            color: None,
            hover: None,
            active: false,
            ink_when_active: false,
            enabled: true,
            live: true,
        }
    }

    /// Whether a click on `resp` counts.
    pub(in crate::ui) fn fires(&self, resp: &Response) -> bool {
        resp.clicked() && self.live && self.enabled
    }
}

/// A ghost icon button: nothing at rest, lit under the pointer, the icon in its colour. The
/// caller adds the tooltip and reads the click (`style.fires(&resp)`).
pub(in crate::ui) fn icon_button(
    ui: &Ui,
    painter: &Painter,
    look: &Look,
    rect: Rect,
    id: Id,
    icon: Icon,
    style: IconStyle,
) -> Response {
    let resp = ui.interact(rect, id, Sense::click());
    let hot = style.live && style.enabled && resp.hovered();
    if hot || style.active {
        lit(painter, look, rect, style.radius);
    }
    let color = if !style.enabled {
        look.p.ink_3
    } else if hot {
        style.hover.unwrap_or(look.p.ink)
    } else if style.active && style.ink_when_active {
        look.p.ink
    } else {
        style.color.unwrap_or(look.ink2)
    };
    icons::paint(
        painter,
        Rect::from_center_size(rect.center(), Vec2::splat(style.icon_px)),
        icon,
        color,
    );
    resp
}

// ---- boxes and menus ------------------------------------------------------

/// A drop shadow `alpha` dark, below the box.
pub(in crate::ui) fn shadow(rect: Rect, radius: f32, alpha: u8) -> Shape {
    Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(alpha),
    }
    .as_shape(rect, radius)
    .into()
}

/// The overlay frame: the see-through panel colour with the line around it.
pub(in crate::ui) fn overlay_frame(look: &Look, radius: f32) -> egui::Frame {
    egui::Frame::new()
        .fill(look.fill)
        .stroke(Stroke::new(1.0, look.p.line))
        .corner_radius(radius)
}

/// Where a menu box goes and how it looks.
#[derive(Clone, Copy)]
pub(in crate::ui) struct MenuFrame {
    /// The top left.
    pub(in crate::ui) at: Pos2,
    pub(in crate::ui) width: f32,
    /// The room between the box's edge and its content.
    pub(in crate::ui) pad: f32,
    pub(in crate::ui) radius: f32,
}

/// The box a menu is drawn in: the panel colour, the line, a shadow, and the widgets' look inside.
/// Returns its rect.
pub(in crate::ui) fn menu_box(
    ui: &mut Ui,
    look: &Look,
    id: impl std::hash::Hash + std::fmt::Debug,
    frame: MenuFrame,
    body: impl FnOnce(&mut Ui),
) -> Rect {
    let MenuFrame {
        at,
        width,
        pad,
        radius,
    } = frame;
    let p = look.p.clone();
    let area = Rect::from_min_size(at, vec2(width, 600.0));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id)
            .max_rect(area)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let fill = child.painter().add(Shape::Noop);
    let inner = egui::Frame::new().inner_margin(pad).show(&mut child, |ui| {
        ui.set_width(width - 2.0 * pad);
        style_widgets(ui, &p);
        body(ui);
    });
    let rect = inner.response.rect;
    child.painter().set(
        fill,
        egui::epaint::RectShape::new(
            rect,
            radius,
            p.panel,
            Stroke::new(1.0, p.line),
            StrokeKind::Inside,
        ),
    );
    child.painter().add(shadow(rect, radius, 60));
    rect
}

/// The widgets' look inside an overlay's menu or strip: the overlay palette.
pub(in crate::ui) fn style_widgets(ui: &mut Ui, p: &Palette) {
    let v = ui.visuals_mut();
    v.override_text_color = Some(p.ink);
    v.widgets.inactive.bg_fill = p.line_strong;
    v.widgets.inactive.weak_bg_fill = p.panel_2;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.ink);
    v.widgets.hovered.bg_fill = p.panel_2;
    v.widgets.hovered.weak_bg_fill = p.panel_2;
    v.widgets.active.bg_fill = p.panel_2;
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0, p.ink);
}

/// One chip of a few fixed choices (a frame rate, an overlay level), `selected` lit.
pub(in crate::ui) fn chip(
    ui: &Ui,
    look: &Look,
    rect: Rect,
    id: Id,
    text: &str,
    selected: bool,
    enabled: bool,
) -> Response {
    let resp = ui.interact(rect, id, Sense::click());
    let hot = enabled && resp.hovered();
    let (fill, border, ink) = if !enabled {
        (look.p.canvas, look.p.line, look.p.ink_3)
    } else if selected {
        (look.p.accent_soft, look.p.accent, look.p.ink)
    } else if hot {
        (look.p.panel_2, look.p.accent, look.p.ink)
    } else {
        (look.p.canvas, look.p.line_strong, look.ink2)
    };
    ui.painter().rect(
        rect,
        8.0,
        fill,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    let g = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(10.5), ink);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, ink);
    resp
}

// ---- which pointer events are an overlay's ---------------------------------

/// What else holds the pointer for an overlay besides the area it covers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum Hold {
    Nothing,
    /// Moves and the like are the overlay's (a drag in progress); clicks and the wheel are
    /// its only over its area.
    Moves,
    /// Every pointer event is (a countdown holds the screen).
    All,
}

/// The rule both overlays follow: egui takes the pointer over any overlay area, and a press that
/// began on one belongs to it until its release. Such events are not for the host, and a click
/// there doesn't capture the pointer.
#[derive(Default)]
pub(in crate::ui) struct Press {
    down: bool,
}

impl Press {
    /// Whether the overlay owns this pointer event. `over`: the pointer is on an area of it.
    pub(in crate::ui) fn claims(&mut self, event: &WindowEvent, over: bool, hold: Hold) -> bool {
        let all = hold == Hold::All;
        match event {
            WindowEvent::MouseInput { state, .. } => match state {
                ElementState::Pressed => {
                    let claimed = over || all;
                    self.down |= claimed;
                    claimed
                }
                ElementState::Released => std::mem::take(&mut self.down) || over || all,
            },
            WindowEvent::MouseWheel { .. } => over || all,
            _ => self.down || over || hold != Hold::Nothing,
        }
    }

    /// A press began on the overlay and its release hasn't come yet.
    pub(in crate::ui) fn is_down(&self) -> bool {
        self.down
    }

    pub(in crate::ui) fn clear(&mut self) {
        self.down = false;
    }
}
