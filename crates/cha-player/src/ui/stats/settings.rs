//! The settings strip under the header: the panel's opacity and whether it snaps to corners.

use egui::{Stroke, Ui, vec2};

use super::{Cmd, PAD, View};
use crate::overlay_prefs::OPACITY_MIN;

impl View<'_> {
    pub(super) fn menu_strip(&self, ui: &mut Ui, inner_w: f32, cmds: &mut Vec<Cmd>) {
        let look = self.look;
        let p = &look.p;
        let top = ui.cursor().top();
        ui.painter()
            .hline(ui.max_rect().x_range(), top, Stroke::new(1.0, p.line));
        let strip = ui.scope(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
            ui.spacing_mut().interact_size.y = 16.0;
            ui.spacing_mut().slider_rail_height = 3.0;
            ui.spacing_mut().slider_width = inner_w - 2.0 * PAD - 8.0 - 8.0 - 62.0 - 32.0;
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
            ui.style_mut().override_font_id = Some(look.font.clone());
            ui.add_space(6.0);
            let mut opacity = self.prefs.opacity;
            let mut snap = self.prefs.snap;
            ui.horizontal(|ui| {
                ui.add_space(PAD);
                let label = ui.label(egui::RichText::new("Opacity").color(look.ink2));
                label.on_hover_text(
                    "How see-through the panel's background is. The text stays solid.",
                );
                let slider = egui::Slider::new(&mut opacity, OPACITY_MIN..=100)
                    .step_by(5.0)
                    .trailing_fill(true)
                    .show_value(false);
                ui.add(slider);
                ui.label(format!("{opacity}%"));
            });
            ui.horizontal(|ui| {
                ui.add_space(PAD);
                ui.checkbox(&mut snap, "Snap to corners").on_hover_text(
                    "On: dropping the panel snaps it to the nearest corner. Off: it stays exactly where you drop it.",
                );
            });
            ui.add_space(6.0);
            (opacity, snap)
        });
        let (opacity, snap) = strip.inner;
        if opacity != self.prefs.opacity {
            cmds.push(Cmd::Opacity(opacity));
        }
        if snap != self.prefs.snap {
            cmds.push(Cmd::Snap(snap));
        }
        if ui.rect_contains_pointer(strip.response.rect)
            && ui.input(|i| i.pointer.is_moving() || i.pointer.any_down())
        {
            cmds.push(Cmd::KeepMenu);
        }
    }
}
