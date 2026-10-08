//! The full view: the warnings, then the spec's sections. The sections, rows, labels, tooltips and
//! values come from `Panel` (`stats-panel.json`); this only draws them.

use std::sync::Arc;

use cha_ui_spec::panel::{PanelRow, PanelSection};
use egui::epaint::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, Galley, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};

use super::report::Copied;
use super::{Cmd, View};
use crate::health::Issue;
use crate::overlay_prefs::Section;
use crate::theme::icons::{self, Icon};
use crate::ui::chrome;

/// A section's id in the spec as the prefs name it. The spec check and a test keep them equal.
pub(super) fn section_of(id: &str) -> Section {
    match id {
        "stream" => Section::Stream,
        "latency" => Section::Latency,
        "network" => Section::Network,
        "node" => Section::Node,
        other => panic!("stats-panel.json: no Section for {other:?}"),
    }
}

impl View<'_> {
    pub(super) fn full_body(&self, ui: &mut Ui, w: f32, cmds: &mut Vec<Cmd>) {
        let look = self.look;
        let painter = ui.painter().clone();
        let issues = &self.health.issues;
        if issues.len() > 1 {
            let (rect, _) = ui.allocate_exact_size(vec2(w, 20.0), Sense::hover());
            look.text(
                &painter,
                rect.left(),
                rect.center().y,
                Align::Min,
                &format!("{} warnings", issues.len()),
                look.ink2,
            );
            let all = self.copied == Some(Copied::All);
            let label = if all { "Copied" } else { "Copy all" };
            let galley = look.galley(&painter, label, look.ink2);
            let b = Rect::from_min_size(
                pos2(rect.right() - galley.size().x - 8.0, rect.top()),
                vec2(galley.size().x + 8.0, 20.0),
            );
            let resp = ui.interact(b, ui.id().with("copy-all"), Sense::click());
            let color = if resp.hovered() {
                chrome::lit(&painter, look, b.shrink2(vec2(0.0, 2.0)), 4.0);
                look.p.ink
            } else {
                look.ink2
            };
            look.text(
                &painter,
                b.right() - 4.0,
                b.center().y,
                Align::Max,
                label,
                color,
            );
            if resp
                .on_hover_text("Copy every warning and the stream numbers")
                .clicked()
            {
                cmds.push(Cmd::Copy(Copied::All));
            }
        }
        if !issues.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt("issues")
                .max_height(160.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    for issue in issues {
                        self.issue(ui, w - 6.0, issue, cmds);
                        ui.add_space(4.0);
                    }
                });
        }
        if let Some(score) = self.health.score {
            let (rect, _) = ui.allocate_exact_size(vec2(w, 20.0), Sense::hover());
            look.text(
                &painter,
                rect.left(),
                rect.center().y,
                Align::Min,
                &format!("Health {score}/100"),
                look.ink2,
            );
        }

        for section in &self.panel.sections {
            if self.section(ui, w, section, cmds) {
                for row in &section.rows {
                    self.row(ui, w, row);
                }
            }
        }
    }

    /// One issue: title and detail with a copy button, then the hint.
    fn issue(&self, ui: &mut Ui, w: f32, issue: &Issue, cmds: &mut Vec<Cmd>) {
        let look = self.look;
        let painter = ui.painter().clone();
        let mut job = LayoutJob::default();
        job.append(
            issue.title,
            0.0,
            TextFormat::simple(look.font.clone(), look.title_color(issue.severity)),
        );
        job.append(
            &format!(" · {}", issue.detail),
            0.0,
            TextFormat::simple(look.font.clone(), look.ink2),
        );
        job.wrap.max_width = w - 20.0;
        let galley = painter.layout_job(job);
        let (rect, _) = ui.allocate_exact_size(vec2(w, galley.size().y.max(20.0)), Sense::hover());
        look.paint(&painter, rect.min, galley.clone());
        let last = galley.rows.last().map_or(0.0, |r| r.rect().right());
        let line_h = look.font.size * 1.25;
        let y = rect.top() + galley.size().y - line_h / 2.0;
        self.copy_button(ui, rect.left() + last + 4.0, y, Copied::One(issue.id), cmds);
        let hint = painter.layout(issue.hint.to_string(), look.font.clone(), look.ink2, w);
        let (rect, _) = ui.allocate_exact_size(hint.size(), Sense::hover());
        look.paint(&painter, rect.min, hint);
    }

    /// A foldable heading; true when the section is open.
    fn section(&self, ui: &mut Ui, w: f32, section: &PanelSection, cmds: &mut Vec<Cmd>) -> bool {
        let look = self.look;
        let id = section_of(&section.id);
        let folded = self.prefs.is_folded(id);
        let color = look.section_color(section.color);
        ui.add_space(6.0);
        let (rect, _) = ui.allocate_exact_size(vec2(w, 20.0), Sense::hover());
        let resp = ui.interact(
            rect,
            ui.id().with(("fold", section.id.as_str())),
            Sense::click(),
        );
        let shown = if resp.hovered() {
            color.lerp_to_gamma(Color32::WHITE, 0.25)
        } else {
            color
        };
        let painter = ui.painter().clone();
        let c = pos2(rect.left() + 5.0, rect.center().y);
        let chevron = Rect::from_center_size(c, Vec2::splat(10.0));
        let icon = if folded {
            Icon::SectionChevronRight
        } else {
            Icon::SectionChevron
        };
        icons::paint(&painter, chevron, icon, shown);
        let label = look.text(
            &painter,
            rect.left() + 14.0,
            rect.center().y,
            Align::Min,
            &section.heading.to_uppercase(),
            shown,
        );
        let summary = &section.summary;
        if folded && !summary.text.is_empty() {
            let avail = rect.right() - label.right() - 8.0;
            let ink = look.tone(summary.tone).unwrap_or(look.p.ink);
            let g = look.fitted(&painter, &summary.text, ink, avail);
            let at = pos2(
                rect.right() - g.size().x,
                rect.center().y - g.size().y / 2.0,
            );
            look.paint(&painter, at, g);
        }
        if resp.clicked() {
            cmds.push(Cmd::Fold(id));
        }
        !folded
    }

    /// Label ... leader ... value, with the label's tooltip.
    fn row(&self, ui: &mut Ui, w: f32, row: &PanelRow) {
        let look = self.look;
        let painter = ui.painter().clone();
        let (rect, _) = ui.allocate_exact_size(vec2(w, 20.0), Sense::hover());
        let cy = rect.center().y;
        let lg = look.galley(&painter, &row.label, look.ink2);
        let lw = lg.size().x;
        let galleys: Vec<Arc<Galley>> = row
            .segments
            .iter()
            .map(|s| {
                let color = look.tone(row.tone_of(s)).unwrap_or(look.p.ink);
                look.galley(&painter, &s.text, color)
            })
            .collect();
        let vw: f32 = galleys.iter().map(|g| g.size().x).sum();
        look.paint(&painter, pos2(rect.left(), cy - lg.size().y / 2.0), lg);
        let mut x = rect.right() - vw;
        for g in galleys {
            let width = g.size().x;
            look.paint(&painter, pos2(x, cy - g.size().y / 2.0), g);
            x += width;
        }
        let (x0, x1) = (rect.left() + lw + 6.0, rect.right() - vw - 6.0);
        if x1 > x0 {
            painter.hline(
                x0..=x1,
                cy.round() + 0.5,
                Stroke::new(1.0, look.ink2.gamma_multiply(0.5)),
            );
        }
        let hover = Rect::from_min_size(rect.min, vec2(lw, 20.0));
        ui.interact(
            hover,
            ui.id().with(("row", row.id.as_str())),
            Sense::hover(),
        )
        .on_hover_ui(|ui| {
            ui.set_max_width(260.0);
            ui.label(&row.tooltip);
        });
    }
}
