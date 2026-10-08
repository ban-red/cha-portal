//! The hidden panel: a faint grade chip in the same corner.

use cha_ui_spec::panel::Panel;
use egui::{Sense, Stroke, Ui, vec2};

use super::{Cmd, look::Look};
use crate::health::{Assessment, Grade};

pub(super) fn chip(
    ui: &mut Ui,
    look: &Look,
    health: &Assessment,
    panel: &Panel,
    cmds: &mut Vec<Cmd>,
) {
    let p = &look.p;
    let letter = health.grade.map_or("–", Grade::letter);
    let galley = look.galley(ui.painter(), letter, p.ink);
    let size = vec2(galley.size().x + 12.0 + 2.0, 22.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let resp = ui.interact(rect, ui.id().with("chip"), Sense::click());
    let m = if resp.hovered() { 1.0 } else { 0.3 };
    let painter = ui.painter();
    painter.rect(
        rect,
        6.0,
        p.panel.gamma_multiply(0.6 * m),
        Stroke::new(1.0, p.line.gamma_multiply(m)),
        egui::StrokeKind::Inside,
    );
    let color = look
        .tone(panel.grade_tone)
        .unwrap_or(p.ink)
        .gamma_multiply(m);
    let g = look.galley(painter, letter, color);
    painter.galley(rect.center() - g.size() / 2.0, g, color);
    if resp.on_hover_text("Show stats").clicked() {
        cmds.push(Cmd::Show);
    }
}
