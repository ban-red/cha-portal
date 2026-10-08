//! Stream settings: what the player asks a host for.

use cha_client::Codec;

use crate::config::Config;
use crate::theme::widgets::{Status, section_heading, status_text};
use crate::theme::{Appearance, Contrast, Theme, ThemeController, ThemeExt, Variant};

const RESOLUTIONS: [(u32, u32, &str); 5] = [
    (1280, 720, "1280 × 720"),
    (1920, 1080, "1920 × 1080"),
    (2560, 1440, "2560 × 1440"),
    (3024, 1964, "3024 × 1964 (MacBook Pro 14)"),
    (3840, 2160, "3840 × 2160"),
];

const FPS: [u32; 3] = [60, 90, 120];

/// Codec order choices, as the config stores them.
const CODEC_ORDERS: [(&[Codec], &str); 6] = [
    (&[Codec::Hevc, Codec::H264], "HEVC, then H.264"),
    (&[Codec::H264, Codec::Hevc], "H.264, then HEVC"),
    (&[Codec::Hevc], "HEVC only"),
    (&[Codec::H264], "H.264 only"),
    // PyroWave only streams from a Cha environment on an NVIDIA node (a
    // Moonlight host never offers it), so these fall back.
    (
        &[Codec::PyroWave420, Codec::Hevc, Codec::H264],
        "PyroWave 4:2:0 (LAN), then HEVC, H.264",
    ),
    (
        &[Codec::PyroWave444, Codec::Hevc, Codec::H264],
        "PyroWave 4:4:4 (LAN, desktops), then HEVC, H.264",
    ),
];

/// What the settings window asks for besides changing `Config`.
#[derive(Default)]
pub struct Outcome {
    pub changed: bool,
    pub reload_themes: bool,
    pub open_themes_folder: bool,
}

/// Draws the settings.
pub fn show(ui: &mut egui::Ui, config: &mut Config, themes: &ThemeController) -> Outcome {
    let before = config.clone();
    let mut outcome = Outcome::default();

    section_heading(ui, "Stream");
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

    ui.add_space(10.0);
    ui.separator();
    section_heading(ui, "Appearance");
    appearance(ui, config, themes, &mut outcome);

    outcome.changed = *config != before;
    outcome
}

/// A theme's canvas, panel, accent and ok colours side by side.
fn swatches(ui: &mut egui::Ui, theme: &Theme, variant: Variant) {
    let p = theme.palette(variant);
    let size = egui::vec2(16.0, 16.0);
    let line = ui.palette().line_strong;
    for color in [p.canvas, p.panel, p.accent, p.ok] {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter().rect(
            rect,
            3.0,
            color,
            egui::Stroke::new(1.0, line),
            egui::StrokeKind::Inside,
        );
    }
}

fn appearance(
    ui: &mut egui::Ui,
    config: &mut Config,
    themes: &ThemeController,
    outcome: &mut Outcome,
) {
    let prefs = &mut config.theme;
    let variant = themes.variant();
    egui::Grid::new("appearance")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label("Theme");
            ui.horizontal(|ui| {
                let current = themes.themes().iter().find(|t| t.id == prefs.theme);
                egui::ComboBox::from_id_salt("theme")
                    .selected_text(current.map_or(prefs.theme.as_str(), |t| t.name.as_str()))
                    .show_ui(ui, |ui| {
                        for theme in themes.themes() {
                            ui.horizontal(|ui| {
                                swatches(ui, theme, variant);
                                if ui
                                    .selectable_label(prefs.theme == theme.id, &theme.name)
                                    .clicked()
                                {
                                    prefs.theme = theme.id.clone();
                                }
                                if !theme.builtin {
                                    ui.weak("yours");
                                }
                            });
                        }
                    });
                if let Some(theme) = current {
                    swatches(ui, theme, variant);
                }
            });
            ui.end_row();

            ui.label("Light or dark");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut prefs.appearance, Appearance::System, "System");
                ui.selectable_value(&mut prefs.appearance, Appearance::Dark, "Dark");
                ui.selectable_value(&mut prefs.appearance, Appearance::Light, "Light");
            });
            ui.end_row();

            ui.label("Contrast");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut prefs.contrast, Contrast::System, "System");
                ui.selectable_value(&mut prefs.contrast, Contrast::Standard, "Standard");
                ui.selectable_value(&mut prefs.contrast, Contrast::More, "More");
            });
            ui.end_row();

            ui.label("Size");
            // Scaling moves the slider under the pointer: apply on release.
            let id = ui.id().with("scale-draft");
            let mut draft = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(prefs.scale);
            let response = ui.add(
                egui::Slider::new(&mut draft, crate::theme::SCALE_RANGE)
                    .step_by(0.05)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
            );
            if response.dragged() {
                ui.data_mut(|d| d.insert_temp(id, draft));
            } else {
                if response.changed() || response.drag_stopped() {
                    prefs.scale = draft;
                }
                ui.data_mut(|d| d.remove_temp::<f32>(id));
            }
            ui.end_row();
        });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("Reload themes").clicked() {
            outcome.reload_themes = true;
        }
        if ui.button("Open themes folder").clicked() {
            outcome.open_themes_folder = true;
        }
    });
    for problem in themes.problems() {
        ui.label(status_text(ui, Status::Warn, problem).small());
    }
    ui.add_space(2.0);
    let ink = ui.palette().ink_3;
    // Labels can be selected and copied.
    ui.add(
        egui::Label::new(
            egui::RichText::new(format!("Your themes: {}", themes.themes_dir().display()))
                .small()
                .color(ink),
        )
        .selectable(true),
    );
}
