//! "Keep holding Esc to release the mouse", with a bar filling up to the release: shown near the
//! top while Esc is held with the pointer captured. The words come from `toolbar.json`
//! (`release_hint`); the look is the overlay chrome the toolbar and the stats panel share.

use egui::{Align2, Context, Id, Rect, Sense, vec2};

use super::chrome::{Look, overlay_frame};
use crate::theme::ThemeExt;

/// The bar's width and height in points.
const BAR_W: f32 = 260.0;
const BAR_H: f32 = 4.0;
/// Below the toolbar when it shows, else this far from the top.
const TOP: f32 = 24.0;

/// Draw the hint with `progress` (0..1) of the hold done, at the overlay `opacity` percent.
pub fn show_release_hint(ctx: &Context, text: &str, progress: f32, opacity: u8, top_inset: f32) {
    let style = ctx.overlay_style();
    let look = Look::new(&style, opacity.max(90));
    let y = if top_inset > 0.0 {
        top_inset + super::toolbar::TOP + 12.0
    } else {
        TOP
    };
    egui::Area::new(Id::new("release_hint"))
        .anchor(Align2::CENTER_TOP, [0.0, y])
        .interactable(false)
        .show(ctx, |ui| {
            overlay_frame(&look, style.radius)
                .inner_margin(12)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 8.0);
                    ui.set_width(BAR_W);
                    let galley = look.galley(ui.painter(), text, look.p.ink);
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(BAR_W, galley.size().y), Sense::hover());
                    look.paint(
                        ui.painter(),
                        egui::pos2(rect.center().x - galley.size().x / 2.0, rect.top()),
                        galley,
                    );
                    let (bar, _) = ui.allocate_exact_size(vec2(BAR_W, BAR_H), Sense::hover());
                    let radius = BAR_H / 2.0;
                    ui.painter().rect_filled(bar, radius, look.p.line);
                    let filled = Rect::from_min_size(
                        bar.min,
                        vec2(bar.width() * progress.clamp(0.0, 1.0), BAR_H),
                    );
                    if filled.width() > 0.5 {
                        ui.painter().rect_filled(filled, radius, look.p.accent);
                    }
                });
        });
}
