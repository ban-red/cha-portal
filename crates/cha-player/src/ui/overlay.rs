//! The stats overlay, toggled with Ctrl+Alt+Shift+S while streaming.

use egui::{Align2, FontId, RichText};

use crate::theme::ThemeExt;

/// One reading of the running stream, for the overlay.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatsSnapshot {
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub present_fps: f32,
    pub decode_fps: f32,
    pub decode_ms: f32,
    pub latency_ms: f32,
    pub dropped: u64,
    pub decode_errors: u64,
    /// PyroWave frames decoded from only some of their packets.
    pub partial: u64,
    pub audio_underruns: u64,
    pub audio_dropped_ms: u64,
    /// Periodic arrival gaps typical of AWDL were seen over the last seconds.
    pub awdl_suspected: bool,
}

pub fn show_stats(ctx: &egui::Context, stats: &StatsSnapshot) {
    // Over the video, so not the launcher's look: see `theme::OverlayStyle`.
    let look = ctx.overlay_style();
    egui::Area::new(egui::Id::new("stats"))
        .anchor(Align2::LEFT_TOP, [10.0, 10.0])
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(look.fill)
                .corner_radius(look.radius)
                .inner_margin(look.margin)
                .show(ui, |ui| {
                    let line = |ui: &mut egui::Ui, text: String| {
                        ui.label(
                            RichText::new(text)
                                .font(FontId::monospace(look.font_size))
                                .color(look.text),
                        );
                    };
                    line(
                        ui,
                        format!("{}x{} {}", stats.width, stats.height, stats.codec),
                    );
                    line(
                        ui,
                        format!(
                            "{:5.1} fps shown, {:5.1} decoded",
                            stats.present_fps, stats.decode_fps
                        ),
                    );
                    line(ui, format!("decode {:5.2} ms", stats.decode_ms));
                    line(ui, format!("received to shown {:5.1} ms", stats.latency_ms));
                    line(
                        ui,
                        format!(
                            "dropped {}  decode errors {}  partial {}",
                            stats.dropped, stats.decode_errors, stats.partial
                        ),
                    );
                    line(
                        ui,
                        format!(
                            "audio underruns {}  dropped {} ms",
                            stats.audio_underruns, stats.audio_dropped_ms
                        ),
                    );
                    if stats.awdl_suspected {
                        ui.label(
                            RichText::new(
                                "Wi-Fi latency spikes: likely AWDL (AirDrop/Continuity). See the README.",
                            )
                            .font(FontId::monospace(look.font_size))
                            .color(look.warn),
                        );
                    }
                });
        });
}
