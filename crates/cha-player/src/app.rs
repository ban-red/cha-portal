//! The window and its two states: the launcher, and a stream.
//!
//! Everything that touches the window, the GPU or egui runs here on the main
//! thread. Transports run on the tokio runtime and report back as
//! [`UserEvent`]s; a running session's threads ([`crate::session`]) only wake
//! this loop when a frame is ready.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long to wait before asking for a drawable again when there wasn't one.
const SURFACE_RETRY: Duration = Duration::from_millis(50);
/// Toolbar choices are written to `config.json` this long after the last one.
const PREFS_SAVE_AFTER: Duration = Duration::from_millis(600);
/// Input Monitoring is asked again this often while the Controllers menu is up.
const INPUT_ACCESS_RECHECK: Duration = Duration::from_secs(2);

use anyhow::{Context, Result};
use cha_client::{Ended, PerfOverlay, Session, Transport, VideoFrame};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, KeyEvent, MouseButton, StartCause, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{ModifiersState, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use crate::config::Config;
use crate::input::pads::{self, InputAccess, PadService};
use crate::input::{self, Hotkey, keymap, pointer};
use crate::present::{self, FrameImporter, ImportedFrame};
use crate::render::egui_layer::EguiLayer;
use crate::render::video::{VideoPipeline, aspect_fit};
use crate::render::{Gpu, Skip};
use crate::session::{self, Rates, Running};
use crate::stream_prefs::{self, StreamPrefs};
use crate::theme::ThemeController;
use crate::ui::{self, Action, Launcher, ToolbarAction};
use crate::video::pyrowave::PyroPresenter;

/// Command-line behaviour that isn't in the config.
pub struct Options {
    /// Exit after this many frames are shown, printing the session's stats
    /// (a smoke test; implies `autostart`).
    pub frames: Option<u64>,
    /// Launch the first app of the first paired host as soon as there is one.
    pub autostart: bool,
    /// The portal transport and its index in the transports, to sign in
    /// through (a `cha://` link, Sign out).
    #[cfg(feature = "portal")]
    pub portal: Option<(usize, cha_client_portal::Portal)>,
}

/// Results and wake-ups from other threads.
pub enum UserEvent {
    /// A decoded frame is waiting.
    Frame,
    /// A session ended (with the generation it belonged to).
    Ended(u64, Ended),
    Ui(ui::Event),
    Launched(Result<Launched, String>),
    /// A `cha://` link was opened (macOS delivers it as an Apple Event).
    #[cfg(feature = "portal")]
    OpenUrl(String),
    /// A followed portal answered a theme fetch (or didn't).
    #[cfg(feature = "portal")]
    PortalTheme {
        portal: String,
        result: Result<crate::theme::ThemeLook, String>,
    },
}

/// A started session and whose it is, for the toolbar's per-app memory.
pub struct Launched {
    /// [`stream_prefs::app_key`].
    pub key: String,
    /// The app's name, if the launcher knew it.
    pub title: String,
    /// The transport's name.
    pub transport: String,
    pub session: Session,
}

pub fn run(
    runtime: tokio::runtime::Runtime,
    transports: Vec<Box<dyn Transport>>,
    data_dir: PathBuf,
    options: Options,
) -> Result<i32> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("creating the event loop")?;
    // Before the loop runs, so a link that launched the app is not missed.
    #[cfg(feature = "portal")]
    {
        let proxy = event_loop.create_proxy();
        crate::urlscheme::install(move |url| {
            let _ = proxy.send_event(UserEvent::OpenUrl(url));
        });
    }
    let mut app = App::new(
        runtime,
        event_loop.create_proxy(),
        transports,
        data_dir,
        options,
    );
    event_loop.run_app(&mut app).context("event loop")?;
    Ok(app.exit_code)
}

struct Graphics {
    gpu: Gpu,
    egui: EguiLayer,
    video: VideoPipeline,
    importer: Box<dyn FrameImporter>,
    /// The picture on screen and the last few before it: the GPU may still be
    /// reading them (their decoder buffers must outlive that).
    history: VecDeque<ImportedFrame>,
    /// Stand-in target for `--frames` runs whose window can't present
    /// (hidden, or no display): the same draw, nowhere to show it.
    offscreen: Option<wgpu::Texture>,
    /// PyroWave decode and draw, made with the first PyroWave stream (the
    /// pipelines compile once) and kept for later ones.
    pyro: Option<PyroPresenter>,
    /// The newest PyroWave frame, taken from the session but not yet decoded
    /// (no drawable yet): a newer one replaces it.
    pyro_pending: Option<VideoFrame>,
}

struct Stream {
    generation: u64,
    running: Running,
    rates: Rates,
    locked: bool,
    cursor: Option<(f64, f64)>,
    /// The click that locked the pointer: its release isn't sent either.
    swallow_release: bool,
    import_failed: bool,
    keys: keymap::KeyTranslator,
    /// Whose toolbar choices these are ([`stream_prefs::app_key`]), the app's
    /// name and its transport's, for the toolbar.
    key: String,
    title: String,
    transport: String,
    /// False: the mouse is switched off (keyboard and pads still go).
    mouse: bool,
    /// Mouse buttons held down (bit per `PointerEvent.button`).
    held: u8,
    /// A saved overlay level to set once the streamer says it has one.
    overlay_wanted: Option<u8>,
    /// Esc closed a menu: its release is not sent either.
    swallow_escape_up: bool,
}

struct App {
    runtime: tokio::runtime::Runtime,
    proxy: EventLoopProxy<UserEvent>,
    transports: Arc<Vec<Box<dyn Transport>>>,
    pads: Arc<PadService>,
    data_dir: PathBuf,
    options: Options,
    #[cfg(feature = "portal")]
    portal: Option<(usize, cha_client_portal::Portal)>,
    started: Instant,

    gfx: Option<Graphics>,
    launcher: Launcher,
    /// Themes, and what the system says about appearance.
    theme: ThemeController,
    /// When to ask the followed portal for its theme.
    #[cfg(feature = "portal")]
    follow: crate::theme::follow::FollowClock,
    stream: Option<Stream>,
    generation: u64,
    modifiers: ModifiersState,
    /// The stats panel over the picture: its prefs and what is dragged.
    panel: ui::StatsPanel,
    /// The bar across the top of the picture.
    toolbar: ui::Toolbar,
    /// Toolbar choices in the config in memory that aren't on disk yet.
    prefs_dirty: Option<Instant>,
    input_access: (InputAccess, Instant),
    /// Whether the last pointer event was the panel's.
    panel_hover: bool,
    /// When the launcher wants to draw again without any event.
    launcher_due: Option<Instant>,
    /// While the surface has no drawable (the window is occluded, or macOS
    /// hasn't shown it yet): don't try again before this, so we don't spin.
    surface_retry: Option<Instant>,
    autostarting: bool,
    exit_code: i32,
}

impl App {
    fn new(
        runtime: tokio::runtime::Runtime,
        proxy: EventLoopProxy<UserEvent>,
        transports: Vec<Box<dyn Transport>>,
        data_dir: PathBuf,
        #[cfg_attr(not(feature = "portal"), allow(unused_mut))] mut options: Options,
    ) -> Self {
        let config = Config::load(&data_dir);
        let panel = ui::StatsPanel::new(config.overlay.clone());
        let theme = ThemeController::new(data_dir.clone(), None);
        #[cfg_attr(not(feature = "portal"), allow(unused_mut))]
        let mut launcher = Launcher::new(config);
        #[cfg(feature = "portal")]
        let portal = options.portal.take();
        #[cfg(feature = "portal")]
        launcher.set_portal_transport(portal.as_ref().map(|(i, _)| *i));
        Self {
            #[cfg(feature = "portal")]
            portal,
            runtime,
            proxy,
            transports: Arc::new(transports),
            pads: Arc::new(PadService::spawn()),
            data_dir,
            started: Instant::now(),
            autostarting: options.autostart || options.frames.is_some(),
            options,
            gfx: None,
            launcher,
            theme,
            #[cfg(feature = "portal")]
            follow: Default::default(),
            stream: None,
            generation: 0,
            modifiers: ModifiersState::empty(),
            panel,
            toolbar: ui::Toolbar::new(),
            prefs_dirty: None,
            input_access: (pads::input_access(), Instant::now()),
            panel_hover: false,
            launcher_due: None,
            surface_retry: None,
            exit_code: 0,
        }
    }

    fn window(&self) -> Option<&Arc<Window>> {
        self.gfx.as_ref().map(|g| &g.gpu.window)
    }

    fn request_redraw(&self) {
        if let Some(w) = self.window() {
            w.request_redraw();
        }
    }

    /// Run `task` on the runtime and deliver what it returns to the loop.
    fn spawn(&self, task: impl Future<Output = UserEvent> + Send + 'static) {
        let proxy = self.proxy.clone();
        self.runtime.spawn(async move {
            let _ = proxy.send_event(task.await);
        });
    }

    // ---- launcher actions -------------------------------------------------

    fn act(&mut self, action: Action) {
        let transports = self.transports.clone();
        match action {
            Action::AddHost { transport, address } => self.spawn(async move {
                let result = transports[transport].add_host(&address).await;
                UserEvent::Ui(ui::Event::HostAdded(result.map_err(|e| format!("{e:#}"))))
            }),
            Action::Pair { transport, host } => {
                let proxy = self.proxy.clone();
                self.spawn(async move {
                    let done = |result| {
                        UserEvent::Ui(ui::Event::PairingDone {
                            transport,
                            host: host.clone(),
                            result,
                        })
                    };
                    match transports[transport].pair(&host).await {
                        Err(e) => done(Err(format!("{e:#}"))),
                        Ok(pairing) => {
                            let _ = proxy.send_event(UserEvent::Ui(ui::Event::PairingStarted {
                                transport,
                                host: host.clone(),
                                pin: pairing.pin,
                                instructions: pairing.instructions,
                            }));
                            done(pairing.done.await.map_err(|e| format!("{e:#}")))
                        }
                    }
                });
            }
            Action::LoadApps { transport, host } => self.spawn(async move {
                let result = transports[transport].apps(&host).await;
                UserEvent::Ui(ui::Event::Apps {
                    transport,
                    host,
                    result: result.map_err(|e| format!("{e:#}")),
                })
            }),
            Action::Launch {
                transport,
                host,
                app,
            } => {
                let name = self.launcher.app_name(transport, &host, app);
                self.launcher.set_busy(Some(match &name {
                    Some(name) => format!("Starting {name}…"),
                    None => "Starting the stream".into(),
                }));
                let key = stream_prefs::app_key(transports[transport].name(), &host, app);
                let config = self.launcher.config().stream_config_for(&key);
                let title = name.unwrap_or_default();
                self.spawn(async move {
                    let t = &transports[transport];
                    let result = t.launch(&host, app, config).await;
                    UserEvent::Launched(result.map_err(|e| format!("{e:#}")).map(|session| {
                        Launched {
                            key,
                            title,
                            transport: t.name().to_string(),
                            session,
                        }
                    }))
                });
            }
            Action::QuitApp {
                transport,
                host,
                app,
            } => {
                let name = self
                    .launcher
                    .app_name(transport, &host, app)
                    .unwrap_or_else(|| "the app".into());
                let config = self.launcher.config().stream_config();
                self.spawn(async move {
                    let result = transports[transport]
                        .quit_app(&host, app, config)
                        .await
                        .map(|()| name)
                        .map_err(|e| format!("{e:#}"));
                    UserEvent::Ui(ui::Event::AppQuit {
                        transport,
                        host,
                        result,
                    })
                });
            }
            #[cfg(feature = "portal")]
            Action::SignIn {
                transport,
                portal,
                ticket,
                launch,
            } => {
                let Some((_, handle)) = self.portal.clone() else {
                    return;
                };
                self.spawn(async move {
                    let link = cha_client_portal::ConnectLink {
                        portal: portal.clone(),
                        ticket,
                        launch: launch.clone(),
                    };
                    let result = handle
                        .sign_in_with_ticket(&link)
                        .await
                        .map(|s| s.username)
                        .map_err(|e| format!("{e:#}"));
                    UserEvent::Ui(ui::Event::SignedIn {
                        transport,
                        portal,
                        launch,
                        result,
                    })
                });
            }
            #[cfg(feature = "portal")]
            Action::SignOut { host } => {
                if let Some((_, handle)) = &self.portal {
                    match handle.sign_out(&host) {
                        Ok(()) => {
                            // A followed portal's look stays, as this Mac's own.
                            let left = self.launcher.theme_prefs_mut().leave_portal(&host);
                            if left {
                                self.save_config();
                            }
                            self.launcher.handle(ui::Event::Info(format!(
                                "Signed out of {host} here; revoke the device in the portal's Settings to be sure"
                            )));
                        }
                        Err(e) => self
                            .launcher
                            .handle(ui::Event::Failed(format!("Could not sign out: {e:#}"))),
                    }
                }
            }
            #[cfg(not(feature = "portal"))]
            Action::SignIn { .. } | Action::SignOut { .. } => {}
            Action::ReloadThemes => {
                self.theme.reload();
                self.request_redraw();
            }
            Action::OpenThemesFolder => {
                if let Err(e) = self.theme.open_themes_folder() {
                    self.launcher.handle(ui::Event::Failed(format!(
                        "Could not open the themes folder: {e}"
                    )));
                }
            }
            Action::SaveConfig(config) => {
                if let Err(e) = config.save(&self.data_dir) {
                    tracing::warn!("saving the settings: {e:#}");
                    self.launcher
                        .handle(ui::Event::Failed(format!("Could not save settings: {e:#}")));
                }
            }
        }
    }

    /// Writes the config as it is in memory.
    fn save_config(&mut self) {
        if let Err(e) = self.launcher.config().save(&self.data_dir) {
            tracing::warn!("saving the settings: {e:#}");
        }
    }

    /// Keeps the look in step with the portal it follows: adopts the first
    /// portal signed in to when the user never chose, lets go of one that is
    /// no longer signed in, and asks for its theme when it is time (on start,
    /// every few minutes, after sign-in and when the window regains focus).
    #[cfg(feature = "portal")]
    fn follow_tick(&mut self) {
        use cha_client::Transport as _;
        let Some((_, handle)) = self.portal.clone() else {
            return;
        };
        let prefs = self.launcher.theme_prefs_mut();
        let mut changed = false;
        if let Some(origin) = prefs.following().map(str::to_string)
            && !handle.is_signed_in(&origin)
        {
            tracing::info!("signed out of {origin}: keeping its theme as this Mac's own");
            changed |= prefs.leave_portal(&origin);
        }
        if prefs.source.is_none()
            && let Some(first) = handle.hosts().into_iter().find(|h| h.paired)
        {
            changed |= prefs.follow_by_default(&first.id);
        }
        let following = prefs.following().map(str::to_string);
        if changed {
            self.save_config();
        }
        if self.follow.poll(following.as_deref(), Instant::now())
            && let Some(origin) = following
        {
            self.spawn(async move {
                let result = handle
                    .prefs(&origin)
                    .await
                    .map(|theme| crate::theme::ThemeLook::from(theme.as_ref()))
                    .map_err(|e| format!("{e:#}"));
                UserEvent::PortalTheme {
                    portal: origin,
                    result,
                }
            });
        }
    }

    /// A followed portal's theme arrived. On failure the last one stays.
    #[cfg(feature = "portal")]
    fn portal_theme(&mut self, portal: String, result: Result<crate::theme::ThemeLook, String>) {
        if self.follow.finished(result.is_ok(), Instant::now())
            && let Err(e) = &result
        {
            tracing::warn!("following the theme of {portal}: {e}; keeping the last one");
        }
        let signed_in = self
            .portal
            .as_ref()
            .is_some_and(|(_, handle)| handle.is_signed_in(&portal));
        let prefs = self.launcher.theme_prefs_mut();
        let changed = match result {
            Ok(look) if prefs.following() == Some(portal.as_str()) => {
                prefs.store_portal_look(&portal, look)
            }
            // A refused token signed us out: the look stays, as our own.
            Err(_) if !signed_in => prefs.leave_portal(&portal),
            _ => false,
        };
        if changed {
            self.save_config();
            self.request_redraw();
        }
    }

    /// A `cha://connect` link: ask before signing in to a portal, or just
    /// show the portal when this install already has a token for it.
    #[cfg(feature = "portal")]
    fn open_url(&mut self, url: &str) {
        let Some((transport, handle)) = &self.portal else {
            return;
        };
        if let Some(w) = self.window() {
            w.focus_window();
        }
        match cha_client_portal::parse_connect_link(url) {
            Err(e) => self
                .launcher
                .show_error(format!("Could not open the link: {e:#}")),
            Ok(link) if handle.is_signed_in(&link.portal) => {
                self.launcher
                    .show_portal(*transport, link.portal, link.launch);
            }
            Ok(link) => self.launcher.ask_sign_in(ui::SignInPrompt {
                transport: *transport,
                portal: link.portal,
                ticket: link.ticket,
                launch: link.launch,
            }),
        }
        self.request_redraw();
    }

    /// A `cha://connect` link asked to launch an app: when that portal's apps
    /// arrive, the launch for the one with that catalog id.
    #[cfg(feature = "portal")]
    fn launch_for_link(&mut self, event: &ui::Event) -> Option<Action> {
        let ui::Event::Apps {
            transport,
            host,
            result: Ok(apps),
        } = event
        else {
            return None;
        };
        let (portal_transport, handle) = self.portal.as_ref()?;
        if portal_transport != transport {
            return None;
        }
        let template = self.launcher.pending_launch(*transport, host)?;
        let found = apps
            .iter()
            .find(|a| handle.template_id(host, a.id).as_deref() == Some(template.as_str()));
        // Either way the link is dealt with: it asks once.
        self.launcher.clear_pending_launch();
        match found {
            Some(app) => Some(Action::Launch {
                transport: *transport,
                host: host.clone(),
                app: app.id,
            }),
            None => {
                self.launcher.show_error(format!(
                    "The link asked to launch {template}, which this portal doesn't offer"
                ));
                None
            }
        }
    }

    /// `--frames` and `--autostart`: launch the first app of the first paired
    /// host, waiting a few seconds for one to show up.
    fn autostart(&self) {
        let transports = self.transports.clone();
        let config = self.launcher.config().clone();
        self.spawn(async move {
            for _ in 0..40 {
                for transport in transports.iter() {
                    for host in transport.hosts().into_iter().filter(|h| h.paired) {
                        let Ok(apps) = transport.apps(&host.id).await else {
                            continue;
                        };
                        if let Some(app) = apps.first() {
                            let key = stream_prefs::app_key(transport.name(), &host.id, app.id);
                            let result = transport
                                .launch(&host.id, app.id, config.stream_config_for(&key))
                                .await;
                            return UserEvent::Launched(result.map_err(|e| format!("{e:#}")).map(
                                |session| Launched {
                                    key,
                                    title: app.name.clone(),
                                    transport: transport.name().to_string(),
                                    session,
                                },
                            ));
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            UserEvent::Launched(Err("no paired host to start".into()))
        });
    }

    // ---- streaming --------------------------------------------------------

    fn start_stream(&mut self, launched: Launched) {
        let Launched {
            key,
            title,
            transport,
            session,
        } = launched;
        if session.codec.is_pyrowave() && !self.prepare_pyrowave() {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        let (wake, ended) = (self.proxy.clone(), self.proxy.clone());
        let running = session::start(
            session,
            self.runtime.handle(),
            self.pads.clone(),
            move || {
                let _ = wake.send_event(UserEvent::Frame);
            },
            move |e| {
                let _ = ended.send_event(UserEvent::Ended(generation, e));
            },
        );
        match running {
            Ok(running) => {
                self.launcher.set_busy(None);
                let rates =
                    Rates::new(&running, self.launcher.config().stream_config_for(&key).fps);
                // What the toolbar remembered for this app.
                let prefs: StreamPrefs = self
                    .launcher
                    .config()
                    .toolbar
                    .get(&key)
                    .cloned()
                    .unwrap_or_default();
                let volume = &running.shared.volume;
                volume.set_muted(prefs.muted.unwrap_or(false));
                volume.set_percent(prefs.volume.unwrap_or(100));
                self.toolbar.reset();
                self.launcher_due = None;
                self.stream = Some(Stream {
                    generation,
                    running,
                    rates,
                    locked: false,
                    cursor: None,
                    swallow_release: false,
                    import_failed: false,
                    keys: keymap::KeyTranslator::new(self.launcher.config().command_as_control),
                    key,
                    title,
                    transport,
                    mouse: prefs.mouse.unwrap_or(true),
                    held: 0,
                    overlay_wanted: prefs.overlay,
                    swallow_escape_up: false,
                });
                if let Some(w) = self.window() {
                    w.set_title(
                        "Cha Player: click to capture the mouse, Ctrl+Alt+Shift+T for the toolbar, Ctrl+Alt+Shift+Q to leave",
                    );
                }
                self.request_redraw();
            }
            Err(e) => self
                .launcher
                .show_error(format!("Could not start the stream: {e:#}")),
        }
    }

    /// Makes the PyroWave presenter on the window's device, or tells the user
    /// why this GPU can't. False: don't start the stream.
    fn prepare_pyrowave(&mut self) -> bool {
        let result = match self.gfx.as_mut() {
            None => Err(anyhow::anyhow!("no window to draw in yet")),
            Some(gfx) => {
                gfx.pyro_pending = None;
                if let Some(pyro) = &mut gfx.pyro {
                    pyro.reset();
                    Ok(())
                } else {
                    PyroPresenter::new(&gfx.gpu.device, &gfx.gpu.queue, gfx.gpu.format).map(|p| {
                        gfx.pyro = Some(p);
                    })
                }
            }
        };
        if let Err(e) = &result {
            self.launcher.set_busy(None);
            self.launcher
                .show_error(format!("Could not start the stream: {e:#}"));
        }
        result.is_ok()
    }

    /// Back to the launcher. `quit_app` also quits the app on the host.
    fn leave(&mut self, quit_app: bool) {
        let Some(stream) = self.stream.take() else {
            return;
        };
        self.stream_ended_save();
        self.toolbar.reset();
        self.set_locked(None, false);
        stream.running.control.release_all();
        stream.running.control.stop(quit_app);
        // What runs on the host changed, or may have.
        self.launcher.refresh_apps();
        self.pads.detach();
        if let Some(gfx) = &mut self.gfx {
            gfx.history.clear();
            gfx.pyro_pending = None;
        }
        if let Some(w) = self.window() {
            w.set_title("Cha Player");
        }
        self.request_redraw();
    }

    /// Writes the stats panel's and the toolbar's choices now.
    fn stream_ended_save(&mut self) {
        let mut config = self
            .panel
            .take_save(true)
            .map(|p| self.launcher.set_overlay_prefs(p));
        if self.prefs_dirty.take().is_some() && config.is_none() {
            config = Some(self.launcher.config().clone());
        }
        if let Some(config) = config
            && let Err(e) = config.save(&self.data_dir)
        {
            tracing::warn!("saving the player's choices: {e:#}");
        }
    }

    /// Remembers toolbar choices for this stream's app; they are written a
    /// moment later, once they stop changing.
    fn remember(&mut self, patch: StreamPrefs) {
        let Some(stream) = &self.stream else { return };
        self.launcher.remember_stream(&stream.key, &patch);
        self.prefs_dirty = Some(Instant::now());
    }

    /// Lock or unlock the pointer. With a stream it also records the state.
    fn set_locked(&mut self, _stream: Option<()>, lock: bool) {
        let Some(window) = self.window().cloned() else {
            return;
        };
        if lock {
            let grabbed = window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
            match grabbed {
                Ok(()) => window.set_cursor_visible(false),
                Err(e) => {
                    tracing::warn!("could not capture the pointer: {e}");
                    return;
                }
            }
        } else {
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            window.set_cursor_visible(true);
        }
        if let Some(stream) = &mut self.stream {
            stream.locked = lock;
        }
        if lock {
            self.toolbar.on_capture();
        }
    }

    fn toggle_full_screen(&self) {
        if let Some(w) = self.window() {
            let next = w
                .fullscreen()
                .is_none()
                .then_some(Fullscreen::Borderless(None));
            w.set_fullscreen(next);
        }
    }

    fn hotkey(&mut self, hotkey: Hotkey) {
        match hotkey {
            Hotkey::Leave => self.leave(false),
            Hotkey::QuitApp => self.leave(true),
            Hotkey::Stats => {
                self.panel.toggle_open();
                self.request_redraw();
            }
            Hotkey::Toolbar => {
                // The pointer has to be free to use it.
                self.set_locked(None, false);
                self.toolbar.show_now();
                self.request_redraw();
            }
            Hotkey::FullScreen => self.toggle_full_screen(),
        }
    }

    /// Where the picture sits in the window now.
    fn picture(&self) -> Option<crate::render::video::Viewport> {
        let (gfx, stream) = (self.gfx.as_ref()?, self.stream.as_ref()?);
        let shown = if stream.running.codec.is_pyrowave() {
            gfx.pyro.as_ref().and_then(|p| p.picture_size())
        } else {
            gfx.history.back().map(|f| (f.width, f.height))
        };
        let (w, h) = shown.unwrap_or((stream.running.width, stream.running.height));
        Some(aspect_fit((w, h), gfx.gpu.size()))
    }

    /// The toolbar's and the stats panel's share of the pointer. While the
    /// pointer is free, egui sees the pointer events first; those over either
    /// (or part of a press that began on it) are theirs: they capture no
    /// pointer and reach no host. Locked, every event goes to the stream.
    fn panel_event(&mut self, event: &WindowEvent) -> bool {
        let is_pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::CursorEntered { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        );
        let Some(window) = self.window().cloned() else {
            return false;
        };
        let (Some(gfx), Some(stream)) = (self.gfx.as_mut(), self.stream.as_mut()) else {
            return false;
        };
        if !is_pointer || stream.locked {
            return false;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            stream.cursor = Some((position.x, position.y));
        }
        gfx.egui.on_event(&window, event);
        let ppp = gfx.egui.ctx().pixels_per_point();
        let at = stream
            .cursor
            .map(|(x, y)| egui::pos2(x as f32 / ppp, y as f32 / ppp));
        // The toolbar is on top, then the stats panel.
        let consumed = self.toolbar.consumes(at, event) || self.panel.consumes(at, event);
        // Redraw while the pointer is on either, and once more as it leaves,
        // so hover feedback works between video frames; and while the toolbar
        // waits to fold, so the timer is armed.
        if consumed || self.panel_hover || self.toolbar.timer_pending() {
            window.request_redraw();
        }
        self.panel_hover = consumed;
        consumed
    }

    /// Window events while streaming; true when handled here.
    fn stream_event(&mut self, event: &WindowEvent) -> bool {
        if self.panel_event(event) {
            return true;
        }
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state,
                        repeat,
                        ..
                    },
                ..
            } => {
                let down = *state == ElementState::Pressed;
                if *code == winit::keyboard::KeyCode::Escape {
                    // Esc closes a menu (or cancels a power off) and is not sent on.
                    if down && self.toolbar.escape() {
                        if let Some(stream) = &mut self.stream {
                            stream.swallow_escape_up = true;
                        }
                        self.request_redraw();
                        return true;
                    }
                    if !down
                        && let Some(stream) = &mut self.stream
                        && std::mem::take(&mut stream.swallow_escape_up)
                    {
                        return true;
                    }
                }
                if down && let Some(hotkey) = input::hotkey(*code, self.modifiers) {
                    self.hotkey(hotkey);
                    return true;
                }
                // The host repeats keys itself.
                if *repeat {
                    return true;
                }
                if let Some(stream) = &mut self.stream {
                    for (code, down) in stream.keys.translate(*code, down) {
                        stream.running.control.input(cha_client::Input::Key {
                            code: code.to_string(),
                            down,
                        });
                    }
                }
                true
            }
            WindowEvent::CursorMoved { position, .. } => {
                let position = (position.x, position.y);
                let picture = self.picture();
                if let Some(stream) = &mut self.stream {
                    stream.cursor = Some(position);
                    if !stream.locked
                        && stream.mouse
                        && let Some((x, y)) = picture.and_then(|p| pointer::absolute(position, p))
                    {
                        stream
                            .running
                            .control
                            .input(cha_client::Input::MouseMove { x, y });
                    }
                }
                true
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let down = *state == ElementState::Pressed;
                let inside = self
                    .stream
                    .as_ref()
                    .and_then(|s| s.cursor)
                    .zip(self.picture())
                    .is_some_and(|(c, p)| pointer::absolute(c, p).is_some());
                let locked = self.stream.as_ref().is_some_and(|s| s.locked);
                if self.stream.as_ref().is_some_and(|s| !s.mouse) {
                    // The mouse is switched off: clicks neither capture nor reach the host.
                    return true;
                }
                if !locked && down && *button == MouseButton::Left && inside {
                    self.set_locked(None, true);
                    if let Some(stream) = &mut self.stream
                        && stream.locked
                    {
                        stream.swallow_release = true;
                    }
                    return true;
                }
                let Some(stream) = &mut self.stream else {
                    return true;
                };
                if !down && *button == MouseButton::Left && stream.swallow_release {
                    stream.swallow_release = false;
                    return true;
                }
                if (locked || inside || !down)
                    && let Some(button) = pointer::button(*button)
                {
                    if button < 8 {
                        if down {
                            stream.held |= 1 << button;
                        } else {
                            stream.held &= !(1 << button);
                        }
                    }
                    stream
                        .running
                        .control
                        .input(cha_client::Input::MouseButton { button, down });
                }
                true
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = pointer::wheel(*delta);
                if let Some(stream) = self.stream.as_ref().filter(|s| s.mouse) {
                    stream
                        .running
                        .control
                        .input(cha_client::Input::Wheel { dx, dy });
                }
                true
            }
            WindowEvent::Focused(false) => {
                self.set_locked(None, false);
                if let Some(stream) = &self.stream {
                    stream.running.control.release_all();
                }
                false
            }
            _ => false,
        }
    }

    // ---- drawing ----------------------------------------------------------

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(retry) = self.surface_retry {
            if Instant::now() < retry {
                event_loop.set_control_flow(ControlFlow::WaitUntil(retry));
                return;
            }
            self.surface_retry = None;
        }
        if self.stream.is_some() {
            self.draw_stream(event_loop);
        } else {
            self.draw_launcher(event_loop);
        }
    }

    fn draw_launcher(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(feature = "portal")]
        self.follow_tick();
        let transports = self.transports.clone();
        let Some(gfx) = self.gfx.as_mut() else {
            return;
        };
        let window = gfx.gpu.window.clone();
        // The drawable first: without one there's nothing to show, and
        // running the UI anyway would only ask for another frame at once.
        let surface = match gfx.gpu.acquire() {
            Ok(s) => s,
            Err(Skip::Retry) => return window.request_redraw(),
            Err(Skip::Later) => {
                // No drawable now: try again shortly, or as soon as macOS
                // says the window is visible (`WindowEvent::Occluded(false)`).
                let retry = Instant::now() + SURFACE_RETRY;
                self.surface_retry = Some(retry);
                event_loop.set_control_flow(ControlFlow::WaitUntil(retry));
                return;
            }
        };
        self.theme.poll_system();
        self.theme
            .sync(gfx.egui.ctx(), &self.launcher.config().theme);
        let mut actions = Vec::new();
        let frame = gfx.egui.run(&window, |ui| {
            actions = self.launcher.show(ui, &transports, &self.theme);
        });
        let view = surface.texture.create_view(&Default::default());
        let mut encoder = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("launcher"),
            });
        gfx.egui.paint(
            &gfx.gpu,
            &mut encoder,
            &view,
            &frame,
            Some(clear_color(self.theme.canvas(), gfx.gpu.format)),
        );
        gfx.gpu.queue.submit([encoder.finish()]);
        window.pre_present_notify();
        surface.present();

        if frame.repaint_after.is_zero() {
            window.request_redraw();
            self.launcher_due = None;
        } else if frame.repaint_after < Duration::from_secs(3600) {
            let due = Instant::now() + frame.repaint_after;
            self.launcher_due = Some(due);
            event_loop.set_control_flow(ControlFlow::WaitUntil(due));
        }
        for action in actions {
            self.act(action);
        }
    }

    /// Writes the stats panel's preferences once they have settled (or now,
    /// with `force`), and arranges to be woken when a save is still pending.
    fn save_overlay(&mut self, force: bool, event_loop: &ActiveEventLoop) {
        let mut config = self
            .panel
            .take_save(force)
            .map(|prefs| self.launcher.set_overlay_prefs(prefs));
        let toolbar_due = self.prefs_dirty.map(|t| {
            if force {
                Duration::ZERO
            } else {
                PREFS_SAVE_AFTER.saturating_sub(t.elapsed())
            }
        });
        if toolbar_due == Some(Duration::ZERO) {
            self.prefs_dirty = None;
            config.get_or_insert_with(|| self.launcher.config().clone());
        }
        if let Some(config) = config
            && let Err(e) = config.save(&self.data_dir)
        {
            tracing::warn!("saving the player's choices: {e:#}");
        }
        let wait = [self.panel.save_due(), self.prefs_dirty.and(toolbar_due)]
            .into_iter()
            .flatten()
            .min();
        if let Some(wait) = wait {
            let due = Instant::now() + wait;
            if self.launcher_due.is_none_or(|d| d > due) {
                self.launcher_due = Some(due);
                event_loop.set_control_flow(ControlFlow::WaitUntil(due));
            }
        }
    }

    /// What the toolbar asked for.
    fn toolbar_action(&mut self, action: ToolbarAction) {
        let Some(stream) = &mut self.stream else {
            return;
        };
        let control = stream.running.control.clone();
        let volume = stream.running.shared.volume.clone();
        match action {
            ToolbarAction::Back => self.leave(false),
            ToolbarAction::PowerOff => self.leave(true),
            ToolbarAction::Capture => {
                if stream.mouse {
                    self.set_locked(None, true);
                }
            }
            ToolbarAction::SetMouse(on) => {
                stream.mouse = on;
                if !on {
                    // Let go of what is held, so nothing stays pressed on the host.
                    for button in 0..8u8 {
                        if stream.held & (1 << button) != 0 {
                            control.input(cha_client::Input::MouseButton {
                                button,
                                down: false,
                            });
                        }
                    }
                    stream.held = 0;
                }
                self.remember(StreamPrefs {
                    mouse: Some(on),
                    ..StreamPrefs::default()
                });
            }
            ToolbarAction::SetMuted(muted) => {
                volume.set_muted(muted);
                self.remember(StreamPrefs {
                    muted: Some(muted),
                    ..StreamPrefs::default()
                });
            }
            ToolbarAction::SetVolume(percent) => {
                volume.set_percent(percent);
                let mut patch = StreamPrefs {
                    volume: Some(percent),
                    ..StreamPrefs::default()
                };
                // Turning it up is asking for sound.
                if volume.muted() && percent > 0 {
                    volume.set_muted(false);
                    patch.muted = Some(false);
                }
                self.remember(patch);
            }
            ToolbarAction::RestartSound => volume.request_restart(),
            ToolbarAction::SetFps(fps) => {
                control.set_fps(fps);
                self.remember(StreamPrefs {
                    fps: Some(fps),
                    ..StreamPrefs::default()
                });
            }
            ToolbarAction::SetOverlay(level) => {
                control.set_overlay(level);
                self.remember(StreamPrefs {
                    overlay: Some(level),
                    ..StreamPrefs::default()
                });
            }
            ToolbarAction::TakeControl => control.take_control(),
            ToolbarAction::ToggleFullScreen => self.toggle_full_screen(),
            ToolbarAction::ToggleStats => self.panel.toggle_open(),
            ToolbarAction::OpenInputSettings => ui::open_input_monitoring_settings(),
        }
        self.request_redraw();
    }

    fn draw_stream(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(gfx), Some(stream)) = (self.gfx.as_mut(), self.stream.as_mut()) else {
            return;
        };
        let shared = stream.running.shared.clone();
        shared.redraw_started();
        tracing::trace!("draw_stream");

        // Only the newest frame is ever shown; older ones were dropped by the
        // video thread.
        let fresh = shared.latest.lock().unwrap().take();
        let mut received = None;
        // PyroWave: decoded below, in the same encoder as the draw. A frame
        // still pending from a redraw that had no drawable is replaced.
        if let Some(raw) = shared.latest_pyro.lock().unwrap().take()
            && gfx.pyro_pending.replace(raw).is_some()
        {
            shared
                .stats
                .dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(frame) = fresh {
            match gfx.importer.import(&frame) {
                Ok(imported) => {
                    gfx.history.push_back(imported);
                    while gfx.history.len() > 3 {
                        gfx.history.pop_front();
                    }
                    received = Some(frame.received);
                }
                Err(e) => {
                    if !stream.import_failed {
                        tracing::warn!("showing a frame: {e:#}");
                        stream.import_failed = true;
                    }
                    stream.running.control.request_keyframe();
                }
            }
        }

        let window = gfx.gpu.window.clone();
        let smoke = self.options.frames.is_some();
        let (surface, view) = match gfx.gpu.acquire() {
            Ok(s) => {
                let view = s.texture.create_view(&Default::default());
                (Some(s), view)
            }
            Err(Skip::Retry) => return window.request_redraw(),
            Err(Skip::Later) if smoke => {
                let (width, height) = gfx.gpu.size();
                let texture = gfx.offscreen.get_or_insert_with(|| {
                    gfx.gpu.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("offscreen"),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: gfx.gpu.format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    })
                });
                (None, texture.create_view(&Default::default()))
            }
            Err(Skip::Later) => {
                // No drawable now: try again shortly, or as soon as macOS
                // says the window is visible (`WindowEvent::Occluded(false)`).
                let retry = Instant::now() + SURFACE_RETRY;
                self.surface_retry = Some(retry);
                event_loop.set_control_flow(ControlFlow::WaitUntil(retry));
                return;
            }
        };
        let mut encoder = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("stream"),
            });
        if let (Some(pyro), Some(raw)) = (gfx.pyro.as_mut(), gfx.pyro_pending.take()) {
            let stats = &shared.stats;
            match pyro.encode(&mut encoder, &raw) {
                Ok(done) => {
                    use std::sync::atomic::Ordering::Relaxed;
                    stats.decoded.fetch_add(1, Relaxed);
                    stats
                        .decode_us
                        .fetch_add(u64::from(done.decode_us), Relaxed);
                    if done.partial {
                        stats.partial.fetch_add(1, Relaxed);
                    }
                    received = Some(raw.received);
                }
                Err(e) => {
                    // Frames stand alone: count it, wait for the next, and
                    // never ask for a keyframe.
                    stats
                        .decode_errors
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::debug!("pyrowave: {e:#}");
                }
            }
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("video"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if stream.running.codec.is_pyrowave() {
                if let Some(pyro) = &gfx.pyro {
                    pyro.draw(&mut pass, gfx.gpu.size());
                }
            } else if let Some(frame) = gfx.history.back() {
                let prepared = gfx.video.prepare(&gfx.gpu.device, frame, gfx.gpu.size());
                gfx.video.draw(&mut pass, &prepared);
            }
        }
        let reconnecting = shared.reconnecting.lock().unwrap().clone();
        let snapshot = stream
            .rates
            .snapshot(&shared, &*stream.running.control)
            .clone();
        let health = stream.rates.health().clone();
        self.theme
            .sync(gfx.egui.ctx(), &self.launcher.config().theme);
        // What the transport says right now, for the toolbar (the stats
        // snapshot is a second old), and a saved overlay level to set once.
        let link = stream.running.control.transport_stats();
        if let (Some(want), Some(l)) = (stream.overlay_wanted, &link)
            && let (Some(true), Some(current)) = (l.control, l.overlay)
        {
            stream.overlay_wanted = None;
            if current != PerfOverlay::Preset(want) && current != PerfOverlay::Custom {
                stream.running.control.set_overlay(want);
            }
        }
        if self.input_access.1.elapsed() > INPUT_ACCESS_RECHECK {
            self.input_access = (pads::input_access(), Instant::now());
        }
        let open_pads = pads::open_pads();
        let toolbar_view = ui::ToolbarView {
            title: &stream.title,
            transport: &stream.transport,
            stats: &snapshot,
            health: &health,
            link: link.as_ref(),
            connected: reconnecting.is_none(),
            locked: stream.locked,
            mouse: stream.mouse,
            muted: shared.volume.muted(),
            volume: shared.volume.percent(),
            fullscreen: window.fullscreen().is_some(),
            stats_open: self.panel.prefs.open,
            opacity: self.panel.prefs.opacity,
            pads: &open_pads,
            input_access: self.input_access.0,
        };
        let (panel, toolbar) = (&mut self.panel, &mut self.toolbar);
        let mut actions = Vec::new();
        let frame = gfx.egui.run(&window, |ui| {
            actions = toolbar.show(ui.ctx(), &toolbar_view);
            let inset = toolbar.inset();
            panel.show(ui.ctx(), &snapshot, &health, inset);
            if let Some(text) = &reconnecting {
                ui::show_reconnecting(ui.ctx(), text, inset);
            }
        });
        gfx.egui.paint(&gfx.gpu, &mut encoder, &view, &frame, None);
        // Hover feedback, a tooltip or the settings strip timing out: wake
        // the window for what egui still wants to draw.
        if frame.repaint_after.is_zero() {
            window.request_redraw();
        } else if frame.repaint_after < Duration::from_secs(3600) {
            let due = Instant::now() + frame.repaint_after;
            self.launcher_due = Some(due);
            event_loop.set_control_flow(ControlFlow::WaitUntil(due));
        }
        gfx.gpu.queue.submit([encoder.finish()]);
        if let Some(pyro) = &mut gfx.pyro {
            pyro.after_submit();
        }
        match surface {
            Some(surface) => {
                window.pre_present_notify();
                surface.present();
            }
            None => {
                let _ = gfx.gpu.device.poll(wgpu::PollType::wait_indefinitely());
                shared
                    .stats
                    .offscreen
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        self.save_overlay(false, event_loop);
        for action in actions {
            self.toolbar_action(action);
        }

        if let Some(received) = received {
            shared.presented(received);
            if let Some(limit) = self.options.frames
                && shared
                    .stats
                    .presented
                    .load(std::sync::atomic::Ordering::Relaxed)
                    >= limit
            {
                println!("cha-player smoke: {}", session::summary(&shared.stats));
                self.leave(false);
                event_loop.exit();
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("Cha Player")
            .with_inner_size(LogicalSize::new(1600.0, 1000.0));
        let built = event_loop
            .create_window(attributes)
            .context("creating the window")
            .and_then(|window| {
                let gpu = Gpu::new(Arc::new(window))?;
                let importer = present::new_importer(&gpu.device)?;
                Ok(Graphics {
                    egui: EguiLayer::new(&gpu),
                    video: VideoPipeline::new(&gpu.device, gpu.format),
                    importer,
                    history: VecDeque::new(),
                    offscreen: None,
                    pyro: None,
                    pyro_pending: None,
                    gpu,
                })
            });
        match built {
            Ok(gfx) => {
                if let Some(theme) = gfx.gpu.window.theme() {
                    self.theme.set_window_theme(theme);
                }
                gfx.gpu.window.request_redraw();
                self.gfx = Some(gfx);
            }
            Err(e) => {
                eprintln!("cha-player: {e:#}");
                self.exit_code = 1;
                event_loop.exit();
                return;
            }
        }
        if self.autostarting {
            self.autostarting = false;
            self.autostart();
        }
        if self.options.frames.is_some() {
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_secs(1),
            ));
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if let Some(limit) = self.options.frames
            && self.started.elapsed() > Duration::from_secs(40)
            && !event_loop.exiting()
            && self.exit_code == 0
        {
            eprintln!("cha-player smoke: gave up before {limit} frames were shown");
            if let Some(stream) = &self.stream {
                eprintln!(
                    "cha-player smoke: {}",
                    session::summary(&stream.running.shared.stats)
                );
            }
            self.exit_code = 1;
            self.leave(false);
            event_loop.exit();
            return;
        }
        if let StartCause::ResumeTimeReached { .. } = cause {
            if self.options.frames.is_some() {
                event_loop.set_control_flow(ControlFlow::WaitUntil(
                    Instant::now() + Duration::from_secs(1),
                ));
            }
            if self.launcher_due.is_some_and(|d| Instant::now() >= d) {
                self.launcher_due = None;
                self.request_redraw();
                if self.stream.is_some() && self.options.frames.is_none() {
                    // The stream wakes the window by itself; don't spin on a due time that passed.
                    event_loop.set_control_flow(ControlFlow::Wait);
                }
            }
            if self.surface_retry.is_some_and(|d| Instant::now() >= d) {
                self.request_redraw();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Frame => self.request_redraw(),
            UserEvent::Ended(generation, ended) => {
                if self
                    .stream
                    .as_ref()
                    .is_some_and(|s| s.generation == generation)
                {
                    self.leave(false);
                    match ended {
                        Ended::Stopped => {}
                        Ended::ByHost(reason) => self.launcher.handle(ui::Event::Info(format!(
                            "The host ended the stream: {reason}"
                        ))),
                        Ended::Failed(e) => {
                            self.launcher.show_error(format!("The stream failed: {e}"))
                        }
                    }
                    if self.options.frames.is_some() {
                        eprintln!("cha-player smoke: the stream ended early");
                        self.exit_code = 1;
                        event_loop.exit();
                    }
                }
            }
            UserEvent::Ui(event) => {
                #[cfg(feature = "portal")]
                let launch = self.launch_for_link(&event);
                // A new sign-in: take its theme now.
                #[cfg(feature = "portal")]
                if matches!(&event, ui::Event::SignedIn { result: Ok(_), .. }) {
                    self.follow.ask_now();
                }
                self.launcher.handle(event);
                #[cfg(feature = "portal")]
                if let Some(action) = launch {
                    self.act(action);
                }
                self.request_redraw();
            }
            #[cfg(feature = "portal")]
            UserEvent::OpenUrl(url) => self.open_url(&url),
            #[cfg(feature = "portal")]
            UserEvent::PortalTheme { portal, result } => self.portal_theme(portal, result),
            UserEvent::Launched(Ok(launched)) => {
                if self.stream.is_some() {
                    // A second launch finished while one is running: drop it.
                    launched.session.control.stop(false);
                } else {
                    self.start_stream(launched);
                }
            }
            UserEvent::Launched(Err(e)) => {
                if self.options.frames.is_some() {
                    eprintln!("cha-player smoke: launch failed: {e}");
                    self.exit_code = 1;
                    event_loop.exit();
                }
                self.launcher
                    .show_error(format!("Could not start the stream: {e}"));
                self.launcher.refresh_apps();
                self.request_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window().cloned() else {
            return;
        };
        // Back in front: the portal's theme may have changed meanwhile.
        #[cfg(feature = "portal")]
        if matches!(event, WindowEvent::Focused(true)) {
            self.follow.focused();
        }
        if self.stream.is_some() {
            if self.stream_event(&event) {
                return;
            }
        } else if let Some(gfx) = &mut self.gfx {
            let consumed = gfx.egui.on_event(&window, &event);
            if consumed {
                window.request_redraw();
            }
            // Pass key chords like Cmd+Ctrl+F through even when egui saw them.
        }
        match event {
            WindowEvent::CloseRequested => {
                self.leave(false);
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(gfx) = &mut self.gfx {
                    gfx.gpu.resize(size.width, size.height);
                }
                window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => window.request_redraw(),
            WindowEvent::ThemeChanged(theme) => {
                self.theme.set_window_theme(theme);
                window.request_redraw();
            }
            WindowEvent::Occluded(occluded) => {
                tracing::debug!(occluded, "window occlusion");
                if !occluded {
                    self.surface_retry = None;
                    window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if input::hotkey(code, self.modifiers) == Some(Hotkey::FullScreen) {
                    self.toggle_full_screen();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            _ => {}
        }
    }

    fn device_event(&mut self, _: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event
            && let Some(stream) = &self.stream
            && stream.locked
            && stream.mouse
        {
            stream
                .running
                .control
                .input(cha_client::Input::MouseMotion {
                    dx: dx as f32,
                    dy: dy as f32,
                });
        }
    }
}

/// The window's clear colour for `canvas`: an sRGB surface wants linear
/// values, egui's usual non-sRGB one the colour as written.
fn clear_color(canvas: egui::Color32, format: wgpu::TextureFormat) -> wgpu::Color {
    let [r, g, b, _] = if format.is_srgb() {
        let c = egui::Rgba::from(canvas);
        [c.r(), c.g(), c.b(), c.a()]
    } else {
        canvas.to_normalized_gamma_f32()
    };
    wgpu::Color {
        r: f64::from(r),
        g: f64::from(g),
        b: f64::from(b),
        a: 1.0,
    }
}
