//! The compact view: one line, and the top reason when the grade isn't an A.

use egui::{Align, Sense, Ui, pos2, vec2};

use super::report::Copied;
use super::{COMPACT_ISSUE_MAX, Cmd, View};

impl View<'_> {
    pub(super) fn compact_body_width(&self, ui: &Ui) -> f32 {
        let look = self.look;
        let painter = ui.painter();
        let line = format!("{} · {}", self.grade_letter(), self.panel.compact);
        let mut w = look.galley(painter, &line, look.p.ink).size().x;
        if let Some(issue) = self.health.issues.first() {
            let text = format!("{} · {}", issue.title, issue.detail);
            let issue_w = look.galley(painter, &text, look.p.ink).size().x + 4.0 + 16.0;
            w = w.max(issue_w.min(COMPACT_ISSUE_MAX));
        }
        w
    }

    pub(super) fn compact_body(&self, ui: &mut Ui, w: f32, cmds: &mut Vec<Cmd>) {
        let look = self.look;
        let painter = ui.painter().clone();
        let (rect, _) = ui.allocate_exact_size(vec2(w, 20.0), Sense::hover());
        let g = look.tone(self.panel.grade_tone).unwrap_or(look.p.ink);
        let letter = look.text(
            &painter,
            rect.left(),
            rect.center().y,
            Align::Min,
            self.grade_letter(),
            g,
        );
        look.text(
            &painter,
            letter.right(),
            rect.center().y,
            Align::Min,
            &format!(" · {}", self.panel.compact),
            look.p.ink,
        );
        if let Some(issue) = self.health.issues.first() {
            let (rect, _) =
                ui.allocate_exact_size(vec2(w.min(COMPACT_ISSUE_MAX), 20.0), Sense::hover());
            let text = format!("{} · {}", issue.title, issue.detail);
            let galley = look.fitted(&painter, &text, look.ink2, rect.width() - 20.0);
            let gw = galley.size().x;
            look.paint(
                &painter,
                pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
                galley,
            );
            let tip = ui.interact(rect, ui.id().with("compact-issue"), Sense::hover());
            tip.on_hover_text(format!("{}: {}", issue.title, issue.detail));
            self.copy_button(
                ui,
                rect.left() + gw + 4.0,
                rect.center().y,
                Copied::One(issue.id),
                cmds,
            );
        }
    }
}
