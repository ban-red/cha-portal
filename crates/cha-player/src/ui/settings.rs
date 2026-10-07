//! Stream settings: what the player asks a host for.

use cha_client::Codec;

use crate::config::Config;

const RESOLUTIONS: [(u32, u32, &str); 5] = [
    (1280, 720, "1280 × 720"),
    (1920, 1080, "1920 × 1080"),
    (2560, 1440, "2560 × 1440"),
    (3024, 1964, "3024 × 1964 (MacBook Pro 14)"),
    (3840, 2160, "3840 × 2160"),
];

const FPS: [u32; 3] = [60, 90, 120];

/// Codec order choices, as the config stores them.
const CODEC_ORDERS: [(&[Codec], &str); 4] = [
    (&[Codec::Hevc, Codec::H264], "HEVC, then H.264"),
    (&[Codec::H264, Codec::Hevc], "H.264, then HEVC"),
    (&[Codec::Hevc], "HEVC only"),
    (&[Codec::H264], "H.264 only"),
];

/// Draws the settings; true when something changed.
pub fn show(ui: &mut egui::Ui, config: &mut Config) -> bool {
    let before = config.clone();

    egui::Grid::new("settings")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label("Resolution");
            let current = RESOLUTIONS
                .iter()
                .find(|(w, h, _)| (*w, *h) == (config.width, config.height))
                .map(|(_, _, name)| name.to_string())
                .unwrap_or_else(|| format!("{} × {}", config.width, config.height));
            egui::ComboBox::from_id_salt("resolution")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (w, h, name) in RESOLUTIONS {
                        if ui
                            .selectable_label((config.width, config.height) == (w, h), name)
                            .clicked()
                        {
                            config.width = w;
                            config.height = h;
                        }
                    }
                });
            ui.end_row();

            ui.label("Frame rate");
            ui.horizontal(|ui| {
                for fps in FPS {
                    ui.selectable_value(&mut config.fps, fps, format!("{fps}"));
                }
            });
            ui.end_row();

            ui.label("Bitrate");
            let mut mbps = config.bitrate_kbps as f32 / 1000.0;
            if ui
                .add(egui::Slider::new(&mut mbps, 5.0..=150.0).suffix(" Mbps"))
                .changed()
            {
                config.bitrate_kbps = (mbps * 1000.0) as u32;
            }
            ui.end_row();

            ui.label("Codec");
            let current = CODEC_ORDERS
                .iter()
                .find(|(order, _)| *order == config.codecs.as_slice())
                .map(|(_, name)| *name)
                .unwrap_or("Custom");
            egui::ComboBox::from_id_salt("codec")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (order, name) in CODEC_ORDERS {
                        if ui
                            .selectable_label(config.codecs.as_slice() == order, name)
                            .clicked()
                        {
                            config.codecs = order.to_vec();
                        }
                    }
                });
            ui.end_row();

            ui.label("Command key");
            ui.checkbox(
                &mut config.command_as_control,
                "Send as Ctrl (Cmd+C copies)",
            );
            ui.end_row();
        });
    ui.add_space(6.0);
    ui.weak("Applies to the next launch. A host may pick less than you ask for.");

    *config != before
}
