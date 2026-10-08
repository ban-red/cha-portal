//! The launcher: hosts per transport, pairing, apps, launch and settings.
//!
//! [`Launcher`] only draws and remembers what the user sees; it returns
//! [`Action`]s for the app to run on a transport, and takes [`Event`]s back
//! with the results.

// Signing in to portals is only wired up with the `portal` feature.
#![cfg_attr(not(feature = "portal"), allow(dead_code))]

mod chrome;
mod settings;
mod stats;
mod toolbar;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use cha_client::{App, AppState, Host, Transport};
use egui::RichText;

use crate::config::Config;
use crate::input::pads::{InputAccess, input_access};
use crate::theme::widgets::{
    Status, bold, callout, danger_button, primary_button_enabled, selectable_card, status_label,
};
use crate::theme::{ThemeController, ThemeExt};

const INPUT_ACCESS_RECHECK: Duration = Duration::from_secs(2);
/// How often a shown portal's apps are listed again: apps start and stop
/// from browsers and other players too.
const PORTAL_APPS_REFRESH: Duration = Duration::from_secs(5);
/// …and this often while an app is starting or closing.
const PORTAL_APPS_REFRESH_CHANGING: Duration = Duration::from_secs(1);
/// System Settings, Privacy & Security, Input Monitoring.
const INPUT_MONITORING_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent";

/// Opens System Settings at Input Monitoring.
pub fn open_input_monitoring_settings() {
    let _ = std::process::Command::new("open")
        .arg(INPUT_MONITORING_SETTINGS)
        .spawn();
}

pub use stats::{StatsPanel, StatsSnapshot, show_reconnecting};
pub use toolbar::{Toolbar, ToolbarAction, ToolbarView};

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
        app: u32,
    },
    /// Swap a `cha://` link's ticket for a token (the user said yes).
    SignIn {
        transport: usize,
        portal: String,
        ticket: String,
        launch: Option<String>,
    },
    /// Forget a portal's token here.
    SignOut {
        host: String,
    },
    SaveConfig(Config),
    /// Read the themes folder again.
    ReloadThemes,
    /// Show the themes folder in Finder (made if missing).
    OpenThemesFolder,
}

/// A `cha://connect` link waiting for the user's yes.
#[derive(Clone, Debug)]
pub struct SignInPrompt {
    pub transport: usize,
    pub portal: String,
    pub ticket: String,
    pub launch: Option<String>,
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
        instructions: String,
    },
    PairingDone {
        transport: usize,
        host: String,
        result: Result<(), String>,
    },
    /// A ticket was swapped (or not): the user name it signed in as.
    SignedIn {
        transport: usize,
        portal: String,
        launch: Option<String>,
        result: Result<String, String>,
    },
    /// A quit finished (the app's name, or why it failed).
    AppQuit {
        transport: usize,
        host: String,
        result: Result<String, String>,
    },
    /// A launch or quit finished without a stream to show.
    Failed(String),
    /// Something for the status line ("Quit Steam").
    Info(String),
}

enum AppsState {
    Loading,
    Loaded {
        apps: Vec<App>,
        /// When they were listed, to list a portal's again.
        listed: Instant,
        /// Listing again in the background: the list stays meanwhile.
        refreshing: bool,
    },
    Failed(String),
}

struct PairingView {
    transport: usize,
    host: String,
    /// The code or PIN to show, and what to do with it.
    pin: Option<(String, String)>,
}

pub struct Launcher {
    config: Config,
    selected: Option<(usize, String)>,
    address: String,
    address_transport: usize,
    apps: HashMap<(usize, String), AppsState>,
    pairing: Option<PairingView>,
    /// The transport that signs in to portals, if the build has one.
    portal_transport: Option<usize>,
    /// A `cha://` link asking to sign in, waiting for a yes.
    sign_in_prompt: Option<SignInPrompt>,
    /// The app a link asked to launch (transport, portal, catalog id),
    /// started once the portal's apps are listed.
    pending_launch: Option<(usize, String, String)>,
    /// Set while a launch is in flight.
    busy: Option<String>,
    error: Option<String>,
    info: Option<String>,
    settings_open: bool,
    /// Input Monitoring, checked now and then: a user may grant it meanwhile.
    input_access: (InputAccess, Instant),
    /// The brand logo beside the title, uploaded on the first frame.
    logo: Option<egui::TextureHandle>,
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
            portal_transport: None,
            sign_in_prompt: None,
            pending_launch: None,
            busy: None,
            error: None,
            info: None,
            settings_open: false,
            input_access: (input_access(), Instant::now()),
            logo: None,
        }
    }

    /// Remember the stats panel's preferences; the config to save.
    pub fn set_overlay_prefs(&mut self, prefs: crate::overlay_prefs::OverlayPrefs) -> Config {
        self.config.overlay = prefs;
        self.config.clone()
    }

    /// Remember toolbar choices for the app `key`.
    pub fn remember_stream(&mut self, key: &str, patch: &crate::stream_prefs::StreamPrefs) {
        self.config
            .toolbar
            .entry(key.to_string())
            .or_default()
            .merge(patch);
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The theme choices, to change what a portal sent or where the look
    /// comes from. The caller saves the config if it changed.
    pub fn theme_prefs_mut(&mut self) -> &mut crate::theme::ThemePrefs {
        &mut self.config.theme
    }

    pub fn set_portal_transport(&mut self, transport: Option<usize>) {
        self.portal_transport = transport;
    }

    /// A link wants to sign in to a portal: ask first.
    pub fn ask_sign_in(&mut self, prompt: SignInPrompt) {
        self.sign_in_prompt = Some(prompt);
    }

    /// Show a portal's apps; `launch` is the app a link asked for.
    pub fn show_portal(&mut self, transport: usize, portal: String, launch: Option<String>) {
        self.selected = Some((transport, portal.clone()));
        // Its apps may have changed since they were listed.
        self.apps.remove(&(transport, portal.clone()));
        if let Some(app) = launch {
            self.busy = Some(format!("Starting {app}…"));
            self.pending_launch = Some((transport, portal, app));
        }
    }

    /// The app a link asked to launch, if it is on `portal` of `transport`.
    pub fn pending_launch(&self, transport: usize, portal: &str) -> Option<String> {
        self.pending_launch
            .as_ref()
            .filter(|(t, p, _)| *t == transport && p == portal)
            .map(|(_, _, app)| app.clone())
    }

    /// The link's app has been dealt with (started, or not to be found).
    pub fn clear_pending_launch(&mut self) {
        if self.pending_launch.take().is_some() {
            self.busy = None;
        }
    }

    /// The listed name of an app, for "Starting Steam…".
    pub fn app_name(&self, transport: usize, host: &str, app: u32) -> Option<String> {
        match self.apps.get(&(transport, host.to_string()))? {
            AppsState::Loaded { apps, .. } => {
                apps.iter().find(|a| a.id == app).map(|a| a.name.clone())
            }
            _ => None,
        }
    }

    /// Apps may have started or stopped (a stream ended, a launch failed):
    /// list the shown portal's again now rather than in a few seconds.
    pub fn refresh_apps(&mut self) {
        let due = Instant::now().checked_sub(PORTAL_APPS_REFRESH);
        for state in self.apps.values_mut() {
            if let (AppsState::Loaded { listed, .. }, Some(due)) = (state, due) {
                *listed = due;
            }
        }
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
                let key = (transport, host.clone());
                let refreshing = matches!(
                    self.apps.get(&key),
                    Some(AppsState::Loaded {
                        refreshing: true,
                        ..
                    })
                );
                let state = match result {
                    Ok(apps) => AppsState::Loaded {
                        apps,
                        listed: Instant::now(),
                        refreshing: false,
                    },
                    // A background listing that failed keeps the list shown
                    // and tries again later; a portal that signed us out
                    // shows as not signed in anyway.
                    Err(e) if refreshing => {
                        tracing::warn!(host, "listing apps again: {e}");
                        if let Some(AppsState::Loaded {
                            listed, refreshing, ..
                        }) = self.apps.get_mut(&key)
                        {
                            *listed = Instant::now();
                            *refreshing = false;
                        }
                        return;
                    }
                    Err(e) => AppsState::Failed(e),
                };
                if let AppsState::Failed(e) = &state {
                    if self.portal_transport == Some(transport) {
                        // A portal that signed us out reads as unpaired below: say why.
                        self.error = Some(e.clone());
                    }
                    if self.pending_launch(transport, &host).is_some() {
                        self.clear_pending_launch();
                    }
                }
                self.apps.insert((transport, host), state);
            }
            Event::SignedIn {
                transport,
                portal,
                launch,
                result,
            } => {
                self.busy = None;
                match result {
                    Ok(user) => {
                        self.info = Some(format!("Signed in to {portal} as {user}"));
                        self.show_portal(transport, portal, launch);
                    }
                    Err(e) => self.error = Some(format!("Could not sign in to {portal}: {e}")),
                }
            }
            Event::PairingStarted {
                transport,
                host,
                pin,
                instructions,
            } => {
                self.pairing = Some(PairingView {
                    transport,
                    host,
                    pin: Some((pin, instructions)),
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
            Event::AppQuit {
                transport,
                host,
                result,
            } => {
                match result {
                    Ok(name) => self.info = Some(format!("Quit {name}")),
                    Err(e) => self.error = Some(format!("Could not quit the app: {e}")),
                }
                if self.portal_transport == Some(transport) {
                    self.refresh_apps();
                } else {
                    // The host's own state says what runs; the list follows.
                    self.apps.remove(&(transport, host));
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
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        transports: &[Box<dyn Transport>],
        themes: &ThemeController,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let hosts: Vec<Vec<Host>> = transports.iter().map(|t| t.hosts()).collect();

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                // The logo at the heading's height, from the spec's raw pixels.
                let logo = self.logo.get_or_insert_with(|| {
                    ui.ctx().load_texture(
                        "cha-logo",
                        egui::ColorImage::from_rgba_unmultiplied(
                            [cha_ui_spec::LOGO_SIZE; 2],
                            cha_ui_spec::LOGO_RGBA,
                        ),
                        egui::TextureOptions::LINEAR,
                    )
                });
                let side = ui.text_style_height(&egui::TextStyle::Heading);
                ui.add(egui::Image::new((logo.id(), egui::vec2(side, side))));
                ui.heading("Cha Player");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Settings").clicked() {
                        self.settings_open = !self.settings_open;
                    }
                });
            });
            self.messages(ui);
        });

        // The host list is a raised surface beside the canvas, as the
        // portal's sidebar is.
        let side = egui::Frame::side_top_panel(ui.style()).fill(ui.palette().panel);
        egui::Panel::left("hosts")
            .frame(side)
            .resizable(true)
            .default_size(320.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (t, transport) in transports.iter().enumerate() {
                        ui.label(bold(transport.name()).strong());
                        if hosts[t].is_empty() {
                            ui.weak("No hosts yet. Add one below, or wait for one on the network.");
                        }
                        for host in &hosts[t] {
                            let selected = self.selected.as_ref() == Some(&(t, host.id.clone()));
                            let state = if self.portal_transport == Some(t) {
                                let up = self.apps_up(t, &host.id);
                                match (host.paired, up) {
                                    (false, _) => "not signed in".to_string(),
                                    (true, 0) => "signed in".to_string(),
                                    (true, 1) => "signed in, 1 app running".to_string(),
                                    (true, n) => format!("signed in, {n} apps running"),
                                }
                            } else {
                                match (host.paired, host.running_app) {
                                    (false, _) => "found, not paired".to_string(),
                                    (true, Some(_)) => "paired, running an app".to_string(),
                                    (true, None) => "paired".to_string(),
                                }
                            };
                            let card = selectable_card(ui, selected, |ui| {
                                ui.label(bold(&host.name).strong());
                                ui.label(
                                    RichText::new(format!("{}  ·  {}", host.address, state))
                                        .small()
                                        .color(ui.palette().ink_2),
                                );
                            });
                            if card.response.clicked() {
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

        self.sign_in_dialog(ui.ctx(), &mut actions);

        if self.settings_open {
            let mut open = true;
            let mut outcome = settings::Outcome::default();
            // The portals this player is signed in to, to follow one's theme.
            let portals: Vec<String> = hosts
                .iter()
                .enumerate()
                .filter(|(t, _)| self.portal_transport == Some(*t))
                .flat_map(|(_, hs)| hs.iter().filter(|h| h.paired).map(|h| h.id.clone()))
                .collect();
            // Centred under the header, never taller than the window: a short window
            // scrolls the settings.
            let screen = ui.ctx().content_rect();
            // Room for the title bar below the header and a margin under it.
            let body_max = (screen.height() - 56.0 - 64.0).max(120.0);
            egui::Window::new("Settings")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 56.0))
                .max_width((screen.width() - 32.0).max(240.0))
                .default_height(body_max + 40.0)
                .show(ui.ctx(), |ui| {
                    // The scroll area takes the height the content had last
                    // frame (at most the room there is), so the window fits
                    // its settings and scrolls only when they don't fit.
                    let id = ui.id().with("settings-height");
                    let last = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(body_max);
                    let out = egui::ScrollArea::vertical()
                        .max_height(last.min(body_max))
                        .auto_shrink([true, false])
                        .show(ui, |ui| {
                            settings::show(ui, &mut self.config, themes, &portals)
                        });
                    outcome = out.inner;
                    ui.data_mut(|d| d.insert_temp(id, out.content_size.y.max(1.0)));
                });
            self.settings_open = open;
            if outcome.changed {
                actions.push(Action::SaveConfig(self.config.clone()));
            }
            if outcome.reload_themes {
                actions.push(Action::ReloadThemes);
            }
            if outcome.open_themes_folder {
                actions.push(Action::OpenThemesFolder);
            }
        }

        // Hosts appear and change on their own (mDNS): look again soon.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(750));
        actions
    }

    /// "Sign in to <portal>?" for a `cha://` link.
    fn sign_in_dialog(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(prompt) = self.sign_in_prompt.clone() else {
            return;
        };
        let mut answer = None;
        egui::Window::new("Sign in?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(bold(format!("Sign in to {}?", prompt.portal)).strong());
                ui.add_space(4.0);
                ui.label(
                    "A link from a web page asked this player to sign in to that portal as you.",
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if primary_button_enabled(ui, true, "Sign in").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                });
            });
        match answer {
            Some(true) => {
                self.sign_in_prompt = None;
                self.busy = Some(format!("Signing in to {}", prompt.portal));
                actions.push(Action::SignIn {
                    transport: prompt.transport,
                    portal: prompt.portal,
                    ticket: prompt.ticket,
                    launch: prompt.launch,
                });
            }
            Some(false) => self.sign_in_prompt = None,
            None => {}
        }
    }

    fn messages(&mut self, ui: &mut egui::Ui) {
        if let Some(what) = &self.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(what);
            });
        }
        if let Some(error) = self.error.clone()
            && callout(ui, Status::Danger, &error, Some("Dismiss"))
        {
            self.error = None;
        }
        if self.input_access.1.elapsed() > INPUT_ACCESS_RECHECK {
            self.input_access = (input_access(), Instant::now());
        }
        // Only while no controller works: macOS's answer can be "denied" for
        // a build it hasn't seen, or for the terminal that started the player,
        // while the controller plays fine.
        if self.input_access.0 == InputAccess::Denied
            && crate::input::pads::connected() == 0
            && callout(
                ui,
                Status::Warn,
                "Some gamepads (Steam Controller) need Input Monitoring: allow Cha Player in \
                 System Settings → Privacy & Security → Input Monitoring, then reopen it. \
                 If it is already on, it was granted to an older build: remove it with −, \
                 add it again, then reopen it.",
                Some("Open Settings"),
            )
        {
            open_input_monitoring_settings();
        }
        if let Some(info) = self.info.clone()
            && callout(ui, Status::Info, &info, Some("OK"))
        {
            self.info = None;
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
                    .hint_text(if self.portal_transport == Some(self.address_transport) {
                        "Add portal: portal.example"
                    } else {
                        "Add host: 192.168.1.20"
                    })
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

    /// How many of a host's listed apps are running or starting.
    fn apps_up(&self, t: usize, host: &str) -> usize {
        match self.apps.get(&(t, host.to_string())) {
            Some(AppsState::Loaded { apps, .. }) => apps.iter().filter(|a| a.state.is_up()).count(),
            _ => 0,
        }
    }

    /// Whether `app` is up on `host`. A portal says so per app; a GameStream
    /// host's own state is newer than its listed apps.
    fn app_state(&self, t: usize, host: &Host, app: &App) -> AppState {
        if self.portal_transport == Some(t) {
            app.state
        } else if host.running_app == Some(app.id) {
            AppState::Running
        } else {
            AppState::Stopped
        }
    }

    fn host_panel(&mut self, ui: &mut egui::Ui, t: usize, host: &Host, actions: &mut Vec<Action>) {
        ui.heading(&host.name);
        // A portal or a PC often is its own address: say it once.
        if host.address != host.name {
            ui.weak(&host.address);
        }
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

        // A portal's apps start and stop elsewhere too: list them again now
        // and then (every second while one starts or closes), keeping the
        // list shown meanwhile.
        let changing = matches!(
            self.apps.get(&key),
            Some(AppsState::Loaded { apps, .. }) if apps.iter().any(|a| a.state.is_changing())
        );
        let portal_refresh = if changing {
            PORTAL_APPS_REFRESH_CHANGING
        } else {
            PORTAL_APPS_REFRESH
        };
        if self.portal_transport == Some(t)
            && let Some(AppsState::Loaded {
                listed, refreshing, ..
            }) = self.apps.get_mut(&key)
            && !*refreshing
            && listed.elapsed() >= portal_refresh
        {
            *refreshing = true;
            actions.push(Action::LoadApps {
                transport: t,
                host: host.id.clone(),
            });
        }

        ui.horizontal(|ui| {
            ui.label(bold("Apps").strong());
            if ui.small_button("Refresh").clicked() {
                self.apps.insert(key.clone(), AppsState::Loading);
                actions.push(Action::LoadApps {
                    transport: t,
                    host: host.id.clone(),
                });
            }
            if self.portal_transport == Some(t) && ui.small_button("Sign out").clicked() {
                actions.push(Action::SignOut {
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
                callout(
                    ui,
                    Status::Danger,
                    &format!("Could not list apps: {e}"),
                    None,
                );
            }
            Some(AppsState::Loaded { apps, .. }) if apps.is_empty() => {
                ui.weak("This host has no apps.");
            }
            Some(AppsState::Loaded { apps, .. }) => {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for app in apps {
                        let state = self.app_state(t, host, app);
                        crate::theme::widgets::card(ui, |ui| {
                            ui.horizontal(|ui| {
                                let launch = if state.is_up() { "Resume" } else { "Launch" };
                                // Closing: a new one starts once it has gone.
                                let can_launch = !busy && state != AppState::Stopping;
                                if primary_button_enabled(ui, can_launch, launch)
                                    .on_disabled_hover_text(
                                        "Still closing; launch it again once it has",
                                    )
                                    .clicked()
                                {
                                    actions.push(Action::Launch {
                                        transport: t,
                                        host: host.id.clone(),
                                        app: app.id,
                                    });
                                }
                                ui.label(bold(&app.name).strong());
                                match state {
                                    AppState::Running => {
                                        status_label(ui, Status::Ok, "(running)");
                                    }
                                    AppState::Starting => {
                                        status_label(ui, Status::Info, "(starting)");
                                    }
                                    AppState::Stopping => {
                                        status_label(ui, Status::Warn, "(closing…)");
                                    }
                                    AppState::Stopped => {}
                                }
                                if app.hdr {
                                    ui.weak("HDR");
                                }
                                if state.is_up()
                                    && danger_button(ui, !busy, "Quit")
                                        .on_hover_text(
                                            "Closes it on the host, unsaved work included",
                                        )
                                        .clicked()
                                {
                                    actions.push(Action::QuitApp {
                                        transport: t,
                                        host: host.id.clone(),
                                        app: app.id,
                                    });
                                }
                            });
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
            Some(PairingView {
                pin: Some((pin, instructions)),
                ..
            }) => {
                ui.label(instructions);
                ui.add_space(6.0);
                ui.label(RichText::new(pin).size(44.0).monospace().strong());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(if self.portal_transport == Some(t) {
                        "Waiting for you to approve it"
                    } else {
                        "Waiting for the host"
                    });
                });
            }
            Some(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Starting to pair");
                });
            }
            None => {
                let portal = self.portal_transport == Some(t);
                ui.label(if portal {
                    "This player is not signed in to this portal."
                } else {
                    "This host does not know this player yet."
                });
                if ui
                    .button(if portal {
                        "Sign in with a code"
                    } else {
                        "Pair"
                    })
                    .clicked()
                {
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

#[cfg(test)]
mod snapshots;

#[cfg(test)]
mod tests {
    use super::*;

    /// The launcher and its settings window draw under a themed context
    /// without panicking, in every variant (no window needed).
    #[test]
    fn launcher_draws_themed_in_every_variant() {
        use crate::theme::{Appearance, Contrast};
        let dir = std::env::temp_dir().join(format!("cha-player-ui-{}", std::process::id()));
        let mut themes = ThemeController::new(dir.clone(), None);
        let ctx = egui::Context::default();
        for appearance in [Appearance::Dark, Appearance::Light] {
            for contrast in [Contrast::Standard, Contrast::More] {
                let mut config = Config::default();
                config.theme.appearance = appearance;
                config.theme.contrast = contrast;
                config.theme.scale = 1.25;
                assert!(themes.sync(&ctx, &config.theme));
                assert!(!themes.sync(&ctx, &config.theme), "unchanged: no restyle");
                let mut launcher = Launcher::new(config);
                launcher.settings_open = true;
                launcher.error = Some("boom".into());
                for _ in 0..2 {
                    let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                        launcher.show(ui, &[], &themes);
                    });
                }
            }
        }
        std::fs::remove_dir_all(dir).ok();
    }
}
