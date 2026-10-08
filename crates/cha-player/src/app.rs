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

use anyhow::{Context, Result};
use cha_client::{Ended, Session, Transport, VideoFrame};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, KeyEvent, MouseButton, StartCause, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{ModifiersState, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use crate::config::Config;
use crate::input::pads::PadService;
use crate::input::{self, Hotkey, keymap, pointer};
use crate::present::{self, FrameImporter, ImportedFrame};
use crate::render::egui_layer::EguiLayer;
use crate::render::video::{VideoPipeline, aspect_fit};
use crate::render::{Gpu, Skip};
use crate::session::{self, Rates, Running};
use crate::theme::ThemeController;
use crate::ui::{self, Action, Launcher};
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
    Launched(Result<Session, String>),
    /// A `cha://` link was opened (macOS delivers it as an Apple Event).
    #[cfg(feature = "portal")]
    OpenUrl(String),
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
    stream: Option<Stream>,
    generation: u64,
    modifiers: ModifiersState,
    show_stats: bool,
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
            stream: None,
            generation: 0,
            modifiers: ModifiersState::empty(),
            show_stats: false,
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
                self.launcher.set_busy(Some(match name {
                    Some(name) => format!("Starting {name}…"),
                    None => "Starting the stream".into(),
                }));
                let config = self.launcher.config().stream_config();
                self.spawn(async move {
                    UserEvent::Launched(
                        transports[transport]
                            .launch(&host, app, config)
                            .await
                            .map_err(|e| format!("{e:#}")),
                    )
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
                        Ok(()) => self.launcher.handle(ui::Event::Info(format!(
                            "Signed out of {host} here; revoke the device in the portal's Settings to be sure"
                        ))),
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
        let config = self.launcher.config().stream_config();
        self.spawn(async move {
            for _ in 0..40 {
                for transport in transports.iter() {
                    for host in transport.hosts().into_iter().filter(|h| h.paired) {
                        let Ok(apps) = transport.apps(&host.id).await else {
                            continue;
                        };
                        if let Some(app) = apps.first() {
                            return UserEvent::Launched(
                                transport
                                    .launch(&host.id, app.id, config.clone())
                                    .await
                                    .map_err(|e| format!("{e:#}")),
                            );
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            UserEvent::Launched(Err("no paired host to start".into()))
        });
    }

    // ---- streaming --------------------------------------------------------

    fn start_stream(&mut self, session: Session) {
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
                let rates = Rates::new(&running);
                self.stream = Some(Stream {
                    generation,
                    running,
                    rates,
                    locked: false,
                    cursor: None,
                    swallow_release: false,
                    import_failed: false,
                    keys: keymap::KeyTranslator::new(self.launcher.config().command_as_control),
                });
                if let Some(w) = self.window() {
                    w.set_title(
                        "Cha Player: click to capture the mouse, Ctrl+Alt+Shift+Q to leave",
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
                self.show_stats = !self.show_stats;
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

    /// Window events while streaming; true when handled here.
    fn stream_event(&mut self, event: &WindowEvent) -> bool {
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
                    stream
                        .running
                        .control
                        .input(cha_client::Input::MouseButton { button, down });
                }
                true
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = pointer::wheel(*delta);
                if let Some(stream) = &self.stream {
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

    fn draw_stream(&mut self, event_loop: &ActiveEventLoop) {
        let show_stats = self.show_stats;
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
        if show_stats || reconnecting.is_some() {
            let snapshot = show_stats.then(|| stream.rates.snapshot(&shared).clone());
            self.theme
                .sync(gfx.egui.ctx(), &self.launcher.config().theme);
            let frame = gfx.egui.run(&window, |ui| {
                if let Some(snapshot) = &snapshot {
                    ui::show_stats(ui.ctx(), snapshot);
                }
                if let Some(text) = &reconnecting {
                    ui::show_reconnecting(ui.ctx(), text);
                }
            });
            gfx.egui.paint(&gfx.gpu, &mut encoder, &view, &frame, None);
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
            if self.stream.is_none() && self.launcher_due.is_some_and(|d| Instant::now() >= d) {
                self.request_redraw();
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
                self.launcher.handle(event);
                #[cfg(feature = "portal")]
                if let Some(action) = launch {
                    self.act(action);
                }
                self.request_redraw();
            }
            #[cfg(feature = "portal")]
            UserEvent::OpenUrl(url) => self.open_url(&url),
            UserEvent::Launched(Ok(session)) => {
                if self.stream.is_some() {
                    // A second launch finished while one is running: drop it.
                    session.control.stop(false);
                } else {
                    self.start_stream(session);
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
