//! The launcher: hosts per transport, pairing, apps, launch and settings.
//!
//! [`Launcher`] only draws and remembers what the user sees; it returns
//! [`Action`]s for the app to run on a transport, and takes [`Event`]s back
//! with the results.

mod overlay;
mod settings;

use std::collections::HashMap;

use cha_client::{App, Host, Transport};
use egui::{Color32, RichText};

use crate::config::Config;

pub use overlay::{StatsSnapshot, show_stats};

/// What the user asked for; the app does it.
#[derive(Debug)]
pub enum Action {
    AddHost {
        transport: usize,
        address: String,
    },
    Pair {
        transport: usize,
        host: String,
    },
    LoadApps {
        transport: usize,
        host: String,
    },
    Launch {
        transport: usize,
        host: String,
        app: u32,
    },
    QuitApp {
        transport: usize,
        host: String,
    },
    SaveConfig(Config),
}

/// What happened to an earlier [`Action`].
#[derive(Debug)]
pub enum Event {
    HostAdded(Result<Host, String>),
    Apps {
        transport: usize,
        host: String,
        result: Result<Vec<App>, String>,
    },
    PairingStarted {
        transport: usize,
        host: String,
        pin: String,
    },
    PairingDone {
        transport: usize,
        host: String,
        result: Result<(), String>,
    },
    /// A launch or quit finished without a stream to show.
    Failed(String),
    /// Something for the status line ("Quit Steam").
    Info(String),
}

enum AppsState {
    Loading,
    Loaded(Vec<App>),
    Failed(String),
}

struct PairingView {
    transport: usize,
    host: String,
    pin: Option<String>,
}

pub struct Launcher {
    config: Config,
    selected: Option<(usize, String)>,
    address: String,
    address_transport: usize,
    apps: HashMap<(usize, String), AppsState>,
    pairing: Option<PairingView>,
    /// Set while a launch is in flight.
    busy: Option<String>,
    error: Option<String>,
    info: Option<String>,
    settings_open: bool,
}

impl Launcher {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            selected: None,
            address: String::new(),
            address_transport: 0,
            apps: HashMap::new(),
            pairing: None,
            busy: None,
            error: None,
            info: None,
            settings_open: false,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn set_busy(&mut self, what: Option<String>) {
        self.busy = what;
    }

    pub fn show_error(&mut self, message: String) {
        self.busy = None;
        self.error = Some(message);
    }

    pub fn handle(&mut self, event: Event) {
        match event {
            Event::HostAdded(Ok(host)) => {
                self.info = Some(format!("Added {}", host.name));
                self.address.clear();
            }
            Event::HostAdded(Err(e)) => self.error = Some(format!("Could not add the host: {e}")),
            Event::Apps {
                transport,
                host,
                result,
            } => {
                let state = match result {
                    Ok(apps) => AppsState::Loaded(apps),
                    Err(e) => AppsState::Failed(e),
                };
                self.apps.insert((transport, host), state);
            }
            Event::PairingStarted {
                transport,
                host,
                pin,
            } => {
                self.pairing = Some(PairingView {
                    transport,
                    host,
                    pin: Some(pin),
                });
            }
            Event::PairingDone {
                transport,
                host,
                result,
            } => {
                self.pairing = None;
                match result {
                    Ok(()) => {
                        self.info = Some("Paired".into());
                        // The app list needs the new pairing.
                        self.apps.remove(&(transport, host));
                    }
                    Err(e) => self.error = Some(format!("Pairing failed: {e}")),
                }
            }
            Event::Failed(e) => {
                self.busy = None;
                self.error = Some(e);
            }
            Event::Info(i) => self.info = Some(i),
        }
    }

    /// Draw one frame of the launcher.
    pub fn show(&mut self, ui: &mut egui::Ui, transports: &[Box<dyn Transport>]) -> Vec<Action> {
        let mut actions = Vec::new();
        let hosts: Vec<Vec<Host>> = transports.iter().map(|t| t.hosts()).collect();

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Cha Player");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Settings").clicked() {
                        self.settings_open = !self.settings_open;
                    }
                });
            });
            self.messages(ui);
        });

        egui::Panel::left("hosts")
            .resizable(true)
            .default_size(320.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (t, transport) in transports.iter().enumerate() {
                        ui.label(RichText::new(transport.name()).strong());
                        if hosts[t].is_empty() {
                            ui.weak("No hosts yet. Add one below, or wait for one on the network.");
                        }
                        for host in &hosts[t] {
                            let selected = self.selected.as_ref() == Some(&(t, host.id.clone()));
                            let state = match (host.paired, host.running_app) {
                                (false, _) => "found, not paired".to_string(),
                                (true, Some(_)) => "paired, running an app".to_string(),
                                (true, None) => "paired".to_string(),
                            };
                            let label = format!("{}\n{}  ·  {}", host.name, host.address, state);
                            if ui.selectable_label(selected, label).clicked() {
                                self.selected = Some((t, host.id.clone()));
                            }
                        }
                        ui.add_space(10.0);
                    }
                });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    self.add_host_row(ui, transports, &mut actions);
                });
            });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            let selected = self.selected.clone().and_then(|(t, id)| {
                hosts
                    .get(t)
                    .and_then(|hs| hs.iter().find(|h| h.id == id))
                    .map(|h| (t, h.clone()))
            });
            match selected {
                None => {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| ui.weak("Pick a host on the left."));
                }
                Some((t, host)) => self.host_panel(ui, t, &host, &mut actions),
            }
        });

        if self.settings_open {
            let mut open = true;
            let mut changed = false;
            egui::Window::new("Settings")
                .open(&mut open)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    changed = settings::show(ui, &mut self.config);
                });
            self.settings_open = open;
            if changed {
                actions.push(Action::SaveConfig(self.config.clone()));
            }
        }

        // Hosts appear and change on their own (mDNS): look again soon.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(750));
        actions
    }

    fn messages(&mut self, ui: &mut egui::Ui) {
        if let Some(what) = &self.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(what);
            });
        }
        if let Some(error) = self.error.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(Color32::from_rgb(235, 90, 90), error);
                if ui.small_button("Dismiss").clicked() {
                    self.error = None;
                }
            });
        }
        if let Some(info) = self.info.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.weak(info);
                if ui.small_button("OK").clicked() {
                    self.info = None;
                }
            });
        }
    }

    fn add_host_row(
        &mut self,
        ui: &mut egui::Ui,
        transports: &[Box<dyn Transport>],
        actions: &mut Vec<Action>,
    ) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.address)
                    .hint_text("Add host: 192.168.1.20")
                    .desired_width(180.0),
            );
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let can_add = !self.address.trim().is_empty() && !transports.is_empty();
            if (ui.add_enabled(can_add, egui::Button::new("Add")).clicked() || (enter && can_add))
                && self.address_transport < transports.len()
            {
                actions.push(Action::AddHost {
                    transport: self.address_transport,
                    address: self.address.trim().to_string(),
                });
            }
        });
        if transports.len() > 1 {
            egui::ComboBox::from_id_salt("add-transport")
                .selected_text(transports[self.address_transport.min(transports.len() - 1)].name())
                .show_ui(ui, |ui| {
                    for (i, t) in transports.iter().enumerate() {
                        ui.selectable_value(&mut self.address_transport, i, t.name());
                    }
                });
        }
        ui.separator();
    }

    fn host_panel(&mut self, ui: &mut egui::Ui, t: usize, host: &Host, actions: &mut Vec<Action>) {
        ui.heading(&host.name);
        ui.weak(&host.address);
        ui.add_space(8.0);

        if !host.paired {
            self.pair_panel(ui, t, host, actions);
            return;
        }

        let key = (t, host.id.clone());
        if !self.apps.contains_key(&key) {
            self.apps.insert(key.clone(), AppsState::Loading);
            actions.push(Action::LoadApps {
                transport: t,
                host: host.id.clone(),
            });
        }

        ui.horizontal(|ui| {
            ui.label(RichText::new("Apps").strong());
            if ui.small_button("Refresh").clicked() {
                self.apps.insert(key.clone(), AppsState::Loading);
                actions.push(Action::LoadApps {
                    transport: t,
                    host: host.id.clone(),
                });
            }
            if host.running_app.is_some()
                && ui
                    .small_button("Quit the running app")
                    .on_hover_text("Closes it on the host, unsaved work included")
                    .clicked()
            {
                actions.push(Action::QuitApp {
                    transport: t,
                    host: host.id.clone(),
                });
            }
        });
        ui.separator();

        let busy = self.busy.is_some();
        match self.apps.get(&key) {
            Some(AppsState::Loading) | None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading apps");
                });
            }
            Some(AppsState::Failed(e)) => {
                ui.colored_label(
                    Color32::from_rgb(235, 90, 90),
                    format!("Could not list apps: {e}"),
                );
            }
            Some(AppsState::Loaded(apps)) if apps.is_empty() => {
                ui.weak("This host has no apps.");
            }
            Some(AppsState::Loaded(apps)) => {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for app in apps {
                        ui.horizontal(|ui| {
                            let running = host.running_app == Some(app.id);
                            let launch = if running { "Resume" } else { "Launch" };
                            if ui.add_enabled(!busy, egui::Button::new(launch)).clicked() {
                                actions.push(Action::Launch {
                                    transport: t,
                                    host: host.id.clone(),
                                    app: app.id,
                                });
                            }
                            ui.label(&app.name);
                            if running {
                                ui.weak("(running)");
                            }
                            if app.hdr {
                                ui.weak("HDR");
                            }
                        });
                    }
                });
            }
        }
    }

    fn pair_panel(&mut self, ui: &mut egui::Ui, t: usize, host: &Host, actions: &mut Vec<Action>) {
        match self
            .pairing
            .as_ref()
            .filter(|p| p.transport == t && p.host == host.id)
        {
            Some(PairingView { pin: Some(pin), .. }) => {
                ui.label("Type this PIN on the host to finish pairing:");
                ui.add_space(6.0);
                ui.label(RichText::new(pin).size(44.0).monospace().strong());
                ui.add_space(6.0);
                ui.weak("In Sunshine or Apollo: the web UI, PIN tab. In a Cha node: the portal's Moonlight settings.");
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Waiting for the host");
                });
            }
            Some(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Starting to pair");
                });
            }
            None => {
                ui.label("This host does not know this player yet.");
                if ui.button("Pair").clicked() {
                    self.pairing = Some(PairingView {
                        transport: t,
                        host: host.id.clone(),
                        pin: None,
                    });
                    actions.push(Action::Pair {
                        transport: t,
                        host: host.id.clone(),
                    });
                }
            }
        }
    }
}
