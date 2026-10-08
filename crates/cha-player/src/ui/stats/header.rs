//! The header: the grade, the summary word (or the compact line when collapsed), and the four
//! buttons (compact/full, collapse, settings, hide).

use egui::{Align, Rect, Sense, Ui, pos2, vec2};

use super::{BTN, Cmd, GAP, HEADER_H, PAD, View};
use crate::theme::icons::Icon;
use crate::ui::chrome::{self, IconStyle};

impl View<'_> {
    pub(super) fn word(&self) -> &str {
        self.health.summary
    }

    pub(super) fn grade_letter(&self) -> &'static str {
        self.health.grade.map_or("–", crate::health::Grade::letter)
    }

    /// The letter's width and the width of the text beside it, which is the
    /// compact line when collapsed and the summary word otherwise.
    pub(super) fn header_text_widths(&self, ui: &Ui) -> (f32, f32) {
        let look = self.look;
        let painter = ui.painter();
        let grade = look
            .galley(painter, self.grade_letter(), look.p.ink)
            .size()
            .x;
        let text = if self.prefs.collapsed {
            self.panel.compact.as_str()
        } else {
            self.word()
        };
        (grade, look.galley(painter, text, look.p.ink).size().x)
    }

    pub(super) fn header(
        &self,
        ui: &mut Ui,
        inner_w: f32,
        handle_content: Option<f32>,
        cmds: &mut Vec<Cmd>,
    ) {
        let look = self.look;
        let (rect, _) = ui.allocate_exact_size(vec2(inner_w, HEADER_H), Sense::hover());
        let painter = ui.painter().clone();
        let cy = rect.center().y;
        let handle_w = handle_content.unwrap_or(inner_w - super::BUTTONS_W);
        let handle = Rect::from_min_size(rect.min, vec2(handle_w, HEADER_H));
        let r = ui.interact(handle, ui.id().with("handle"), Sense::click_and_drag());
        if r.drag_started() {
            cmds.push(Cmd::DragStart);
        }
        if r.dragged() {
            cmds.push(Cmd::DragMove);
        }
        if r.drag_stopped() {
            cmds.push(Cmd::DragEnd);
        }
        let r = r.on_hover_text("Drag to move");
        let grade_color = look.tone(self.panel.grade_tone).unwrap_or(look.p.ink);
        let letter = look.text(
            &painter,
            handle.left() + PAD,
            cy,
            Align::Min,
            self.grade_letter(),
            grade_color,
        );
        let x = letter.right() + 6.0;
        let avail = handle.right() - PAD - x;
        if self.prefs.collapsed {
            let g = look.fitted(&painter, &self.panel.compact, look.p.ink, avail);
            look.paint(&painter, pos2(x, cy - g.size().y / 2.0), g);
        } else {
            let g = look.fitted(&painter, self.word(), look.ink2, avail);
            look.paint(&painter, pos2(x, cy - g.size().y / 2.0), g);
        }
        drop(r);

        let mut bx = handle.right() + GAP;
        let mut button = |ui: &mut Ui, id: &str, tip: &str, icon: Icon, active: bool, cmd: Cmd| {
            let b = Rect::from_min_size(pos2(bx, cy - BTN / 2.0), vec2(BTN, BTN));
            bx += BTN + GAP;
            let style = IconStyle {
                active,
                ink_when_active: true,
                ..IconStyle::new(12.0, 4.0)
            };
            let resp = chrome::icon_button(ui, &painter, look, b, ui.id().with(id), icon, style);
            if resp.on_hover_text(tip).clicked() {
                cmds.push(cmd);
            }
        };
        let compact = self.prefs.compact;
        button(
            ui,
            "compact",
            if compact { "Full view" } else { "Compact view" },
            if compact {
                Icon::StatsFull
            } else {
                Icon::StatsCompact
            },
            false,
            Cmd::Compact,
        );
        let collapsed = self.prefs.collapsed;
        button(
            ui,
            "collapse",
            if collapsed {
                "Expand"
            } else {
                "Collapse to the header"
            },
            if collapsed {
                Icon::PanelExpand
            } else {
                Icon::PanelCollapse
            },
            false,
            Cmd::Collapse,
        );
        button(
            ui,
            "menu",
            "Panel settings",
            Icon::SettingsDots,
            self.menu,
            Cmd::Menu,
        );
        button(
            ui,
            "hide",
            "Hide (click the grade chip or press Ctrl+Alt+Shift+S to bring it back)",
            Icon::EyeOff,
            false,
            Cmd::Hide,
        );
    }
}
