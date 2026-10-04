//! Our headless Wayland compositor, built on Smithay.
//!
//! It runs on its own thread with a calloop event loop. Two clocks drive it:
//! - the **compositor tick** (e.g. 240 Hz, from a timerfd): every tick sends
//!   frame callbacks, so apps draw at that rate and react to input sooner (S2);
//! - the **encode tick** (e.g. 60 Hz, every Nth compositor tick): if anything
//!   changed, the scene is composited into a free buffer of the output pool and
//!   published to the encoders. Nothing is composited that won't be encoded.
//!
//! Output buffers are GBM dmabufs registered with CUDA once, so the encoders
//! read them in place.

mod clipboard;
mod cursor;
mod handlers;
mod input;
mod output;

use std::ffi::OsString;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use cha_nvenc::CudaContext;
use smithay::backend::drm::DrmNode;
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::ImportDma;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::desktop::{PopupManager, Space, Window, WindowSurfaceType};
use smithay::input::keyboard::XkbConfig;
use smithay::input::pointer::CursorImageStatus;
use smithay::input::{Seat, SeatState};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::channel::{self, Channel, Event as ChannelEvent};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction};
use smithay::reexports::rustix::time::{
    Itimerspec, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags, Timespec, timerfd_create,
    timerfd_settime,
};
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use smithay::utils::{Clock, Logical, Monotonic, Point, Transform};
use smithay::wayland::compositor::{CompositorClientState, CompositorState};
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::dmabuf::{DmabufFeedbackBuilder, DmabufGlobal, DmabufState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::pointer_constraints::PointerConstraintsState;
use smithay::wayland::presentation::{PresentationState, Refresh};
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::selection::data_device::{DataDeviceState, set_data_device_selection};
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::single_pixel_buffer::SinglePixelBufferState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::viewporter::ViewporterState;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::input::Input;
use crate::media::FrameHub;
use clipboard::{Clipboard, TEXT_MIMES};
pub use clipboard::{ClipboardWatch, MAX_BYTES as CLIPBOARD_MAX_BYTES};
pub use cursor::{CursorShape, CursorWatch, PointerSpot, PointerWatch};

/// What the compositor publishes to the sessions.
struct Published {
    clipboard: Clipboard,
    cursor: watch::Sender<CursorShape>,
    pointer: watch::Sender<PointerSpot>,
}
pub use output::Slot;
use output::{OutputPool, Stats};

/// Output size limits; NVENC wants sizes in multiples of 8.
pub const MIN_SIZE: (u32, u32) = (320, 240);
pub const MAX_SIZE: (u32, u32) = (3840, 2160);

/// Clamps a requested output size to what we support.
pub fn fit_size(width: u32, height: u32) -> (u32, u32) {
    let fit = |v: u32, lo: u32, hi: u32| v.clamp(lo, hi) & !7;
    (
        fit(width, MIN_SIZE.0, MAX_SIZE.0),
        fit(height, MIN_SIZE.1, MAX_SIZE.1),
    )
}

pub struct Config {
    pub render_node: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Frame callbacks per second (≥ `encode_fps`).
    pub compositor_fps: u32,
    /// Composited (and encoded) frames per second, at most.
    pub encode_fps: u32,
    /// The Wayland socket's name in `$XDG_RUNTIME_DIR`.
    pub socket_name: String,
}

/// What the rest of the streamer asks of the compositor.
#[derive(Debug)]
pub enum Command {
    Input(Input),
    Resize {
        width: u32,
        height: u32,
    },
    /// Composite at the next encode tick even if nothing changed (a new
    /// encoder needs a first frame).
    ForceFrame,
    /// The browser's clipboard text, for apps to paste.
    SetClipboard(Arc<str>),
    /// The page draws the cursor (desktop mode) or wants it in the picture.
    ClientCursor(bool),
}

pub struct Handle {
    pub commands: channel::Sender<Command>,
    pub socket_name: OsString,
    /// The text apps last copied.
    pub clipboard: ClipboardWatch,
    /// The cursor's shape, for a page that draws it.
    pub cursor: CursorWatch,
    /// Where the pointer is, for viewers.
    pub pointer: PointerWatch,
}

/// Starts the compositor thread; returns once its socket is listening.
pub fn spawn(config: Config, cuda: Arc<CudaContext>, hub: Arc<FrameHub>) -> Result<Handle> {
    let (ready_tx, ready_rx) = std_mpsc::channel();
    std::thread::Builder::new()
        .name("compositor".into())
        .spawn(move || {
            if let Err(err) = run(config, cuda, hub, &ready_tx) {
                warn!("compositor stopped: {err:#}");
                let _ = ready_tx.send(Err(err));
            }
        })?;
    ready_rx
        .recv()
        .map_err(|_| anyhow!("the compositor thread died during startup"))?
}

fn run(
    config: Config,
    cuda: Arc<CudaContext>,
    hub: Arc<FrameHub>,
    ready: &std_mpsc::Sender<Result<Handle>>,
) -> Result<()> {
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;
    let (clipboard, clipboard_watch) = Clipboard::new();
    let (cursor, cursor_watch) = watch::channel(CursorShape::Named("default"));
    let (pointer, pointer_watch) = watch::channel(PointerSpot::default());
    let published = Published {
        clipboard,
        cursor,
        pointer,
    };
    let mut state = State::new(&config, cuda, hub, published, &mut event_loop, display)?;

    let (commands, command_rx): (channel::Sender<Command>, Channel<Command>) = channel::channel();
    event_loop
        .handle()
        .insert_source(command_rx, |event, _, state| {
            if let ChannelEvent::Msg(command) = event {
                state.command(command);
            }
        })
        .map_err(|e| anyhow!("command channel: {e}"))?;

    let timer = Ticker::new(config.compositor_fps)?;
    event_loop
        .handle()
        .insert_source(
            Generic::new(timer, Interest::READ, Mode::Level),
            |_, timer, state| {
                timer.clear();
                state.tick();
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow!("timer: {e}"))?;

    info!(
        socket = ?state.socket_name,
        width = config.width,
        height = config.height,
        compositor_fps = config.compositor_fps,
        encode_fps = config.encode_fps,
        gpu = state.pool.gpu_name(),
        "compositor up"
    );
    let _ = ready.send(Ok(Handle {
        commands,
        socket_name: state.socket_name.clone(),
        clipboard: clipboard_watch,
        cursor: cursor_watch,
        pointer: pointer_watch,
    }));
    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}

/// A periodic timerfd: microsecond-accurate, unlike calloop's timers, which
/// round to the event loop's millisecond timeouts.
struct Ticker(OwnedFd);

impl Ticker {
    fn new(hz: u32) -> Result<Self> {
        let fd = timerfd_create(
            TimerfdClockId::Monotonic,
            TimerfdFlags::NONBLOCK | TimerfdFlags::CLOEXEC,
        )?;
        let period = Duration::from_secs(1) / hz.max(1);
        let spec = Timespec {
            tv_sec: 0,
            tv_nsec: period.as_nanos() as _,
        };
        timerfd_settime(
            &fd,
            TimerfdTimerFlags::empty(),
            &Itimerspec {
                it_interval: spec,
                it_value: spec,
            },
        )?;
        Ok(Self(fd))
    }

    /// Reads the expiration count so the fd stops being readable.
    fn clear(&self) {
        let mut buf = [0u8; 8];
        let _ = smithay::reexports::rustix::io::read(&self.0, &mut buf);
    }
}

impl AsFd for Ticker {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

/// Protocol globals that only need to stay alive.
#[allow(dead_code)]
struct Globals {
    xdg_decoration: XdgDecorationState,
    dmabuf: DmabufGlobal,
    output_manager: OutputManagerState,
    viewporter: ViewporterState,
    presentation: PresentationState,
    relative_pointer: RelativePointerManagerState,
    pointer_constraints: PointerConstraintsState,
    single_pixel_buffer: SinglePixelBufferState,
    cursor_shape: CursorShapeManagerState,
}

pub struct State {
    pub display_handle: DisplayHandle,
    pub socket_name: OsString,
    pub clock: Clock<Monotonic>,
    pub started: Instant,

    // Protocols.
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub seat_state: SeatState<State>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    _globals: Globals,

    // The desktop.
    pub space: Space<Window>,
    pub popups: PopupManager,
    pub seat: Seat<State>,
    pub output: Output,
    pub pointer_location: Point<f64, Logical>,
    pub cursor_status: CursorImageStatus,
    /// The page draws the cursor: leave it out of the picture.
    pub client_cursor: bool,
    /// The cursor's status or surface changed since the last publish.
    pub cursor_changed: bool,
    cursor: watch::Sender<CursorShape>,
    pointer: watch::Sender<PointerSpot>,
    pub clipboard: Clipboard,
    arrow: smithay::backend::renderer::element::memory::MemoryRenderBuffer,
    /// evdev codes currently held, so browser key repeats don't double-press.
    pub keys_down: Vec<u32>,
    /// New windows to give the keyboard once they first show a buffer.
    pub focus_on_map: Vec<WlSurface>,

    // Rendering.
    pub renderer: GlesRenderer,
    pub pool: OutputPool,
    pub hub: Arc<FrameHub>,
    /// Something visible changed since the last composite.
    pub dirty: bool,
    force_frame: bool,
    encode_period: Duration,
    tick_period: Duration,
    ticks: u64,
    next_encode: Instant,
    pub stats: Stats,
}

impl State {
    fn new(
        config: &Config,
        cuda: Arc<CudaContext>,
        hub: Arc<FrameHub>,
        published: Published,
        event_loop: &mut EventLoop<State>,
        display: Display<State>,
    ) -> Result<Self> {
        let dh = display.handle();
        let clock = Clock::<Monotonic>::new();

        // The GPU: an EGL display on the render node's device (no GBM platform
        // needed to render), and GBM only to allocate the output buffers.
        let render_node = DrmNode::from_path(&config.render_node)
            .with_context(|| format!("{} is not a DRM node", config.render_node.display()))?;
        let device = EGLDevice::enumerate()
            .context("enumerating EGL devices")?
            .find(|d| d.try_get_render_node().ok().flatten() == Some(render_node))
            .with_context(|| format!("no EGL device for {}", config.render_node.display()))?;
        // SAFETY: the device outlives the display (EGLDisplay keeps it).
        let egl = unsafe { EGLDisplay::new(device) }.context("creating the EGL display")?;
        let context = EGLContext::new(&egl).context("creating the EGL context")?;
        // SAFETY: the context is only used on this thread.
        let renderer = unsafe { GlesRenderer::new(context) }.context("creating the renderer")?;
        let pool = OutputPool::new(
            &config.render_node,
            egl.clone(),
            cuda,
            (config.width, config.height),
        )?;

        let compositor_state = CompositorState::new_v6::<State>(&dh);
        let xdg_shell_state = XdgShellState::new::<State>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<State>(&dh);
        let shm_state = ShmState::new::<State>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<State>(&dh);
        let data_device_state = DataDeviceState::new::<State>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<State>(&dh);
        let viewporter_state = ViewporterState::new::<State>(&dh);
        let presentation_state = PresentationState::new::<State>(&dh, clock.id() as u32);
        let relative_pointer_state = RelativePointerManagerState::new::<State>(&dh);
        let pointer_constraints_state = PointerConstraintsState::new::<State>(&dh);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<State>(&dh);
        let cursor_shape_state = CursorShapeManagerState::new::<State>(&dh);

        // Clients allocate on our GPU and hand us dmabufs (Chrome, Firefox,
        // Xwayland's glamor); the feedback names the render node.
        let mut dmabuf_state = DmabufState::new();
        let formats = renderer.dmabuf_formats();
        let feedback = DmabufFeedbackBuilder::new(render_node.dev_id(), formats.clone())
            .build()
            .context("building dmabuf feedback")?;
        let dmabuf_global =
            dmabuf_state.create_global_with_default_feedback::<State>(&dh, &feedback);

        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(XkbConfig::default(), 200, 25)
            .context("adding the keyboard")?;
        seat.add_pointer();

        let output = Output::new(
            "CHA-1".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Cha".into(),
                model: "Streamer".into(),
                serial_number: "0".into(),
            },
        );
        let _global = output.create_global::<State>(&dh);
        let mode = OutputMode {
            size: (config.width as i32, config.height as i32).into(),
            refresh: (config.compositor_fps * 1000) as i32,
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Integer(1)),
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        let mut space = Space::default();
        space.map_output(&output, (0, 0));

        let socket = ListeningSocketSource::with_name(&config.socket_name)
            .with_context(|| format!("creating the Wayland socket {}", config.socket_name))?;
        let socket_name = socket.socket_name().to_os_string();
        let handle = event_loop.handle();
        handle
            .insert_source(socket, |stream, _, state| {
                if let Err(err) = state
                    .display_handle
                    .insert_client(stream, Arc::new(ClientState::default()))
                {
                    warn!("couldn't add a client: {err}");
                }
            })
            .map_err(|e| anyhow!("socket source: {e}"))?;
        handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // SAFETY: we never drop the display.
                    unsafe { display.get_mut().dispatch_clients(state)? };
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| anyhow!("display source: {e}"))?;

        let encode_period = Duration::from_secs(1) / config.encode_fps.max(1);
        let pointer_location = (
            f64::from(config.width) / 2.0,
            f64::from(config.height) / 2.0,
        )
            .into();
        Ok(Self {
            display_handle: dh,
            socket_name,
            clock,
            started: Instant::now(),
            compositor_state,
            xdg_shell_state,
            shm_state,
            dmabuf_state,
            seat_state,
            data_device_state,
            primary_selection_state,
            _globals: Globals {
                xdg_decoration: xdg_decoration_state,
                dmabuf: dmabuf_global,
                output_manager: output_manager_state,
                viewporter: viewporter_state,
                presentation: presentation_state,
                relative_pointer: relative_pointer_state,
                pointer_constraints: pointer_constraints_state,
                single_pixel_buffer: single_pixel_buffer_state,
                cursor_shape: cursor_shape_state,
            },
            space,
            popups: PopupManager::default(),
            seat,
            output,
            pointer_location,
            cursor_status: CursorImageStatus::default_named(),
            client_cursor: false,
            cursor_changed: true,
            cursor: published.cursor,
            pointer: published.pointer,
            clipboard: published.clipboard,
            arrow: cursor::arrow(),
            keys_down: Vec::new(),
            focus_on_map: Vec::new(),
            renderer,
            pool,
            hub,
            dirty: true,
            force_frame: false,
            encode_period,
            tick_period: Duration::from_secs(1) / config.compositor_fps.max(1),
            ticks: 0,
            next_encode: Instant::now(),
            stats: Stats::default(),
        })
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Input(input) => self.input(input),
            Command::Resize { width, height } => self.resize(width, height),
            Command::ForceFrame => self.force_frame = true,
            Command::ClientCursor(on) => {
                if self.client_cursor != on {
                    self.client_cursor = on;
                    self.dirty = true;
                }
            }
            Command::SetClipboard(text) => {
                let mimes = TEXT_MIMES.iter().map(ToString::to_string).collect();
                set_data_device_selection(&self.display_handle, &self.seat, mimes, text);
            }
        }
    }

    /// One compositor tick: maybe composite and publish a frame, then let every
    /// visible surface draw its next one.
    fn tick(&mut self) {
        self.clipboard.poll(&self.seat);
        self.publish_cursor();
        let now = Instant::now();
        if now + self.encode_period / 8 >= self.next_encode {
            self.next_encode += self.encode_period;
            if self.next_encode < now {
                // Fell behind (e.g. a stall): realign instead of bursting.
                self.next_encode = now + self.encode_period;
            }
            self.stats.encode_ticks += 1;
            self.publish_pointer();
            let wanted = self.hub.has_listeners() && (self.dirty || self.force_frame);
            if !self.dirty && !self.force_frame {
                self.stats.clean += 1;
            }
            if wanted {
                self.force_frame = false;
                self.dirty = false;
                if let Err(err) = self.composite() {
                    warn!("compositing failed: {err:#}");
                }
            }
        }

        // Every tick is a "refresh" as far as apps can tell: their latest
        // commits count as presented, at the compositor's rate, so they pace
        // themselves to it (Chrome follows presentation feedback, not frame
        // callbacks). Whatever they drew last is what the next composite takes.
        self.ticks += 1;
        let output = self.output.clone();
        let mut feedback = OutputPresentationFeedback::new(&output);
        for window in self.space.elements() {
            window.take_presentation_feedback(
                &mut feedback,
                |_, _| Some(output.clone()),
                |_, _| wp_presentation_feedback::Kind::Vsync,
            );
        }
        feedback.presented(
            self.clock.now(),
            Refresh::Fixed(self.tick_period),
            self.ticks,
            wp_presentation_feedback::Kind::Vsync,
        );

        let elapsed = self.started.elapsed();
        for window in self.space.elements() {
            window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
        if let CursorImageStatus::Surface(surface) = &self.cursor_status {
            smithay::desktop::utils::send_frames_surface_tree(
                surface,
                &output,
                elapsed,
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
        self.space.refresh();
        self.popups.cleanup();
        let _ = self.display_handle.flush_clients();
        let windows = self.space.elements().count();
        self.stats.maybe_log(&self.pool, windows);
    }

    fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = fit_size(width, height);
        let current = self.output.current_mode().map(|m| m.size);
        if current == Some((width as i32, height as i32).into()) {
            return;
        }
        info!(width, height, "resizing the output");
        let mode = OutputMode {
            size: (width as i32, height as i32).into(),
            refresh: self.output.current_mode().map_or(60_000, |m| m.refresh),
        };
        self.output
            .change_current_state(Some(mode), None, None, None);
        self.output.set_preferred(mode);
        if let Err(err) = self.pool.resize((width, height)) {
            warn!("reallocating the output buffers: {err:#}");
        }
        self.configure_windows();
        self.dirty = true;
        self.force_frame = true;
    }

    /// The output's size in pixels.
    pub fn output_size(&self) -> (i32, i32) {
        self.output
            .current_mode()
            .map_or((0, 0), |m| (m.size.w, m.size.h))
    }

    /// The topmost surface under `pos`, and its origin.
    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space
            .element_under(pos)
            .and_then(|(window, location)| {
                window
                    .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(s, p)| (s, (p + location).to_f64()))
            })
    }

    pub fn window_for(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .cloned()
    }
}
