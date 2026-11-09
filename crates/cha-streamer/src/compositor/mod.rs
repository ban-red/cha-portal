//! Our headless Wayland compositor, built on Smithay.
//!
//! It runs on its own thread with a calloop event loop. Two clocks drive it:
//! - the **compositor tick** (e.g. 240 Hz, from a timerfd): every tick sends
//!   frame callbacks, so apps draw at that rate and react to input sooner (S2);
//! - the **encode tick** (e.g. 60 Hz, every Nth compositor tick): if anything
//!   changed, the scene is composited into a free buffer of the output pool and
//!   published to the encoders. Nothing is composited that won't be encoded.
//!
//! On a GPU the output buffers are GBM dmabufs, registered with CUDA once on
//! NVIDIA, so the encoders read them in place. On the CPU device the scene is
//! drawn by Mesa's software renderer (llvmpipe) and read back to memory as
//! planar YUV for x264.

mod clipboard;
mod cursor;
mod handlers;
mod input;
mod output;
mod schedule;

use schedule::{Pacing, Schedule, Wake};
mod tap;

use std::ffi::OsString;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
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
use smithay::wayland::compositor::{CompositorClientState, CompositorState, with_states};
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::dmabuf::{DmabufFeedbackBuilder, DmabufGlobal, DmabufState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::pointer_constraints::PointerConstraintsState;
use smithay::wayland::presentation::{PresentationState, Refresh};
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::selection::data_device::{DataDeviceState, set_data_device_selection};
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shell::xdg::{XdgShellState, XdgToplevelSurfaceData};
use smithay::wayland::shm::ShmState;
use smithay::wayland::single_pixel_buffer::SinglePixelBufferState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::viewporter::ViewporterState;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::device::{Device, DeviceKind};
use crate::input::Input;
use crate::media::FrameHub;
use clipboard::{Clipboard, TEXT_MIMES};
pub use clipboard::{
    ClipboardWatch, MAX_BYTES as CLIPBOARD_MAX_BYTES, Publisher as ClipboardPublisher,
};
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
    /// What it draws on: the device's render node, or Mesa's software
    /// renderer for the CPU device.
    pub device: Arc<Device>,
    pub width: u32,
    pub height: u32,
    /// Frame callbacks per second, at least: rounded up to a multiple of
    /// the encode rate ([`tick_rate`]), so also ≥ `encode_fps`.
    pub compositor_fps: u32,
    /// Composited (and encoded) frames per second, at most; [`Command::SetFps`]
    /// changes it.
    pub encode_fps: u32,
    /// When to composite (`--pacing`).
    pub pacing: PacingChoice,
    /// Under commit pacing, send the last picture again at half the stream's
    /// rate while nothing changes: stock Moonlight clients (the GameStream
    /// host) expect a steady stream; Cha's own players don't.
    pub keepalive: bool,
    /// The Wayland socket's name in `$XDG_RUNTIME_DIR`.
    pub socket_name: String,
    /// The frame tap (`--frame-tap`), if there is one: small pictures for local readers.
    pub tap: Option<Arc<crate::tap::Tap>>,
}

/// `--pacing`: when the compositor composites ([`schedule`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum PacingChoice {
    /// When gamescope's window commits while it is up (a Steam session: the
    /// game's own cadence), on the encode tick otherwise (desktops, whose
    /// apps pace themselves to the 240 Hz feedback).
    #[default]
    Auto,
    /// On the encode tick, whatever the app (what every session did before).
    Fixed,
    /// When the first window commits, whatever the app.
    Commit,
}

/// Under `--pacing auto`, whether a window with this xdg `app_id` drives
/// compositing: gamescope's (its Wayland backend names its window so).
fn drives(app_id: Option<&str>) -> bool {
    app_id == Some("gamescope")
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
    /// Lift every key and pointer button the page still holds down (its
    /// session ended or lost the floor).
    ReleaseInput,
    /// The browser's clipboard text, for apps to paste.
    SetClipboard(Arc<str>),
    /// The page draws the cursor (desktop mode) or wants it in the picture.
    ClientCursor(bool),
    /// Encode this many frames a second from now on; the tick rate follows.
    SetFps(u32),
}

/// The compositor's tick rate for an encode rate: the lowest multiple of
/// `fps` that reaches `at_least` Hz, so every encoded frame falls on a tick
/// (90 fps on a 240 Hz clock would alternate 2 and 3 ticks between frames,
/// a judder; 270 Hz doesn't).
pub fn tick_rate(at_least: u32, fps: u32) -> u32 {
    let fps = fps.max(1);
    fps * at_least.div_ceil(fps).max(1)
}

pub struct Handle {
    pub commands: channel::Sender<Command>,
    pub socket_name: OsString,
    /// The text apps last copied.
    pub clipboard: ClipboardWatch,
    /// Where X11 apps' copies are published (the helper's; see `x11_clipboard`).
    pub clipboard_publisher: ClipboardPublisher,
    /// The cursor's shape, for a page that draws it.
    pub cursor: CursorWatch,
    /// Where the pointer is, for viewers.
    pub pointer: PointerWatch,
    /// Whether compositing follows the driving window's commits (`--pacing`)
    /// rather than the encode tick, now.
    pub commit_paced: Arc<AtomicBool>,
}

/// Starts the compositor thread; returns once its socket is listening.
pub fn spawn(config: Config, hub: Arc<FrameHub>) -> Result<Handle> {
    let (ready_tx, ready_rx) = std_mpsc::channel();
    std::thread::Builder::new()
        .name("compositor".into())
        .spawn(move || {
            if let Err(err) = run(config, hub, &ready_tx) {
                warn!("compositor stopped: {err:#}");
                let _ = ready_tx.send(Err(err));
            }
        })?;
    ready_rx
        .recv()
        .map_err(|_| anyhow!("the compositor thread died during startup"))?
}

fn run(config: Config, hub: Arc<FrameHub>, ready: &std_mpsc::Sender<Result<Handle>>) -> Result<()> {
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;
    let (clipboard, clipboard_watch) = Clipboard::new();
    let clipboard_publisher = clipboard.publisher();
    let (cursor, cursor_watch) = watch::channel(CursorShape::Named("default"));
    let (pointer, pointer_watch) = watch::channel(PointerSpot::default());
    let published = Published {
        clipboard,
        cursor,
        pointer,
    };
    let timer = Ticker::new(tick_rate(config.compositor_fps, config.encode_fps))?;
    let mut state = State::new(
        &config,
        hub,
        published,
        timer.clone(),
        &mut event_loop,
        display,
    )?;

    let (commands, command_rx): (channel::Sender<Command>, Channel<Command>) = channel::channel();
    event_loop
        .handle()
        .insert_source(command_rx, |event, _, state| {
            if let ChannelEvent::Msg(command) = event {
                state.command(command);
            }
        })
        .map_err(|e| anyhow!("command channel: {e}"))?;

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
    event_loop
        .handle()
        .insert_source(
            Generic::new(state.alarm.clone(), Interest::READ, Mode::Level),
            |_, alarm, state| {
                alarm.clear();
                state.alarm();
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow!("alarm: {e}"))?;

    info!(
        socket = ?state.socket_name,
        width = config.width,
        height = config.height,
        compositor_fps = tick_rate(config.compositor_fps, config.encode_fps),
        encode_fps = config.encode_fps,
        gpu = state.pool.gpu_name(),
        "compositor up"
    );
    let _ = ready.send(Ok(Handle {
        commit_paced: Arc::clone(&state.commit_paced),
        commands,
        socket_name: state.socket_name.clone(),
        clipboard: clipboard_watch,
        clipboard_publisher,
        cursor: cursor_watch,
        pointer: pointer_watch,
    }));
    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}

/// A periodic timerfd: microsecond-accurate, unlike calloop's timers, which
/// round to the event loop's millisecond timeouts.
///
/// Clones share the timer: the event loop owns one, the state keeps another
/// to retime it when the frame rate changes.
#[derive(Clone)]
struct Ticker(Arc<OwnedFd>);

impl Ticker {
    fn new(hz: u32) -> Result<Self> {
        let fd = timerfd_create(
            TimerfdClockId::Monotonic,
            TimerfdFlags::NONBLOCK | TimerfdFlags::CLOEXEC,
        )?;
        let ticker = Self(Arc::new(fd));
        ticker.retime(hz)?;
        Ok(ticker)
    }

    /// Ticks at `hz` from now on.
    fn retime(&self, hz: u32) -> Result<()> {
        let period = Duration::from_secs(1) / hz.max(1);
        let spec = Timespec {
            tv_sec: 0,
            tv_nsec: period.as_nanos() as _,
        };
        timerfd_settime(
            &*self.0,
            TimerfdTimerFlags::empty(),
            &Itimerspec {
                it_interval: spec,
                it_value: spec,
            },
        )?;
        Ok(())
    }

    /// A one-shot timer, disarmed until [`Ticker::arm_in`].
    fn alarm() -> Result<Self> {
        let fd = timerfd_create(
            TimerfdClockId::Monotonic,
            TimerfdFlags::NONBLOCK | TimerfdFlags::CLOEXEC,
        )?;
        Ok(Self(Arc::new(fd)))
    }

    /// Fires once, `delay` from now (at least a microsecond: zero disarms).
    fn arm_in(&self, delay: Duration) {
        let delay = delay.max(Duration::from_micros(1));
        let spec = Timespec {
            tv_sec: delay.as_secs() as _,
            tv_nsec: delay.subsec_nanos() as _,
        };
        let none = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let _ = timerfd_settime(
            &*self.0,
            TimerfdTimerFlags::empty(),
            &Itimerspec {
                it_interval: none,
                it_value: spec,
            },
        );
    }

    fn disarm(&self) {
        let none = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let _ = timerfd_settime(
            &*self.0,
            TimerfdTimerFlags::empty(),
            &Itimerspec {
                it_interval: none,
                it_value: none,
            },
        );
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
    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        // A client we cut off for breaking the protocol is why an app
        // vanishes: say so.
        match reason {
            DisconnectReason::ConnectionClosed => info!(?client_id, "a client disconnected"),
            DisconnectReason::ProtocolError(error) => warn!(
                ?client_id,
                code = error.code,
                interface = error.object_interface,
                object = error.object_id,
                "a client broke the protocol: {}",
                error.message
            ),
        }
    }
}

/// Protocol globals that only need to stay alive.
#[allow(dead_code)]
struct Globals {
    xdg_decoration: XdgDecorationState,
    /// Not on the CPU device: software clients use shm.
    dmabuf: Option<DmabufGlobal>,
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
    /// Where the page last put the pointer (absolute moves), to turn its
    /// moves into relative motion while a surface holds a pointer lock.
    pub page_pointer: Option<Point<f64, Logical>>,
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
    /// Pointer buttons (`BTN_*`) currently held.
    pub buttons_down: Vec<u32>,
    /// New windows to give the keyboard once they first show a buffer.
    pub focus_on_map: Vec<WlSurface>,

    // Rendering.
    pub renderer: GlesRenderer,
    pub pool: OutputPool,
    pub hub: Arc<FrameHub>,
    /// Something visible changed since the last composite.
    pub dirty: bool,
    /// When an app last committed a visible change, since the last composite
    /// (taken by it: how long the picture waited before it was composited).
    pub last_commit: Option<Instant>,
    force_frame: bool,
    /// When to composite: the encode tick, or the driving surface's commits.
    schedule: Schedule,
    pacing_choice: PacingChoice,
    /// The surface whose commits drive compositing (`Pacing::Commit`).
    pub driver: Option<WlSurface>,
    /// Wakes the compositor when a commit-paced composite is due.
    alarm: Ticker,
    /// The pacing, for the sessions' hello and `/info`.
    commit_paced: Arc<AtomicBool>,
    tick_period: Duration,
    /// `--compositor-fps`: the tick rate is the lowest multiple of the
    /// encode rate at or above it.
    min_tick_hz: u32,
    ticker: Ticker,
    ticks: u64,
    pub stats: Stats,
    /// The frame tap's capture side, when `--frame-tap` is on.
    tap: Option<tap::Capture>,
}

/// The EGL display to draw on: the render node's device on a GPU, Mesa's
/// software device (llvmpipe) for the CPU. Also the node, for dmabuf
/// feedback, where there is one.
fn egl_display(device: &Device) -> Result<(EGLDisplay, Option<DrmNode>)> {
    let devices = EGLDevice::enumerate().context("enumerating EGL devices")?;
    if device.kind() == DeviceKind::Cpu {
        let software = devices
            .into_iter()
            .find(EGLDevice::is_software)
            .context("no software EGL device (Mesa's EGL: libegl-mesa0, libgl1-mesa-dri)")?;
        // SAFETY: the device outlives the display (EGLDisplay keeps it).
        let egl =
            unsafe { EGLDisplay::new(software) }.context("creating the software EGL display")?;
        return Ok((egl, None));
    }
    let path = device
        .render_node()
        .context("a GPU device needs a render node")?;
    let node = DrmNode::from_path(path)
        .with_context(|| format!("{} is not a DRM node", path.display()))?;
    let found = devices
        .into_iter()
        .find(|d| d.try_get_render_node().ok().flatten() == Some(node))
        .with_context(|| format!("no EGL device for {}", path.display()))?;
    // SAFETY: the device outlives the display (EGLDisplay keeps it).
    let egl = unsafe { EGLDisplay::new(found) }.context("creating the EGL display")?;
    Ok((egl, Some(node)))
}

/// The GL_RENDERER string of an EGL context on the device (Mesa's name for the
/// GPU, "Mesa Intel(R) UHD Graphics 630 (CFL GT2)"). Opens a context and
/// drops it again; for `--probe-device`, not for a start.
pub fn gl_renderer(device: &Device) -> Result<String> {
    let (egl, _) = egl_display(device)?;
    let context = EGLContext::new(&egl).context("creating the EGL context")?;
    // SAFETY: the context is only used on this thread.
    let mut renderer = unsafe { GlesRenderer::new(context) }.context("creating the renderer")?;
    renderer
        .with_context(|gl| {
            // SAFETY: GetString returns a NUL-terminated string that lives as
            // long as the context, or null.
            let ptr = unsafe { gl.GetString(smithay::backend::renderer::gles::ffi::RENDERER) };
            (!ptr.is_null()).then(|| {
                unsafe { std::ffi::CStr::from_ptr(ptr.cast()) }
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .context("making the context current")?
        .context("GL_RENDERER is empty")
}

impl State {
    fn new(
        config: &Config,
        hub: Arc<FrameHub>,
        published: Published,
        ticker: Ticker,
        event_loop: &mut EventLoop<State>,
        display: Display<State>,
    ) -> Result<Self> {
        let dh = display.handle();
        let clock = Clock::<Monotonic>::new();

        // The GPU: an EGL display on the render node's device (no GBM platform
        // needed to render), and GBM only to allocate the output buffers. The
        // CPU device has Mesa's software device instead.
        let (egl, render_node) = egl_display(&config.device)?;
        let context = EGLContext::new(&egl).context("creating the EGL context")?;
        // SAFETY: the context is only used on this thread.
        let renderer = unsafe { GlesRenderer::new(context) }.context("creating the renderer")?;
        let pool = OutputPool::new(&config.device, egl, (config.width, config.height))?;

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
        // Xwayland's glamor); the feedback names the render node. There is no
        // GPU on the CPU device: its clients draw into shm.
        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global = match render_node {
            Some(node) => {
                let formats = renderer.dmabuf_formats();
                let feedback = DmabufFeedbackBuilder::new(node.dev_id(), formats.clone())
                    .build()
                    .context("building dmabuf feedback")?;
                Some(dmabuf_state.create_global_with_default_feedback::<State>(&dh, &feedback))
            }
            None => None,
        };

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
        let tick_hz = tick_rate(config.compositor_fps, config.encode_fps);
        let mode = OutputMode {
            size: (config.width as i32, config.height as i32).into(),
            refresh: (tick_hz * 1000) as i32,
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
        let alarm = Ticker::alarm()?;
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
            page_pointer: None,
            cursor_status: CursorImageStatus::default_named(),
            client_cursor: false,
            cursor_changed: true,
            cursor: published.cursor,
            pointer: published.pointer,
            clipboard: published.clipboard,
            arrow: cursor::arrow(),
            keys_down: Vec::new(),
            buttons_down: Vec::new(),
            focus_on_map: Vec::new(),
            renderer,
            pool,
            hub,
            dirty: true,
            last_commit: None,
            force_frame: false,
            schedule: {
                let mut s = Schedule::new(Pacing::Fixed, encode_period, Instant::now());
                s.set_keepalive(config.keepalive.then_some(2));
                s
            },
            pacing_choice: config.pacing,
            driver: None,
            alarm,
            commit_paced: Arc::default(),
            tick_period: Duration::from_secs(1) / tick_hz,
            min_tick_hz: config.compositor_fps,
            ticker,
            ticks: 0,
            stats: Stats::default(),
            tap: config.tap.clone().map(tap::Capture::new),
        })
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Input(input) => self.input(input),
            Command::Resize { width, height } => self.resize(width, height),
            Command::ReleaseInput => self.release_input(),
            Command::ForceFrame => self.force_frame = true,
            Command::SetFps(fps) => self.set_fps(fps),
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
        if let Some(tap) = self.tap.as_mut() {
            match tap.finish(&mut self.renderer) {
                Ok(Some(us)) => self.stats.tap_us.push(us),
                Ok(None) => {}
                Err(err) => warn!("frame tap: {err:#}"),
            }
        }
        self.clipboard.poll(&self.seat);
        self.publish_cursor();
        let now = Instant::now();
        self.update_pacing(now);
        match self.schedule.pacing() {
            Pacing::Fixed => {
                if self.schedule.poll(now) == Wake::Composite {
                    self.encode_wake(false);
                }
            }
            Pacing::Commit => {
                // A change besides the driving surface's (a popup, the
                // cursor, a forced frame) composites at the next chance too.
                if self.dirty || self.force_frame {
                    self.schedule.on_commit(true);
                    self.arm_alarm(now);
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
        let commit_paced = self.schedule.pacing() == Pacing::Commit;
        for window in self.space.elements() {
            // The driving surface hears when it is actually composited.
            if commit_paced
                && window
                    .toplevel()
                    .is_some_and(|t| Some(t.wl_surface()) == self.driver.as_ref())
            {
                continue;
            }
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

    /// Encodes at `fps` from now on, with the clock (and the refresh rate
    /// apps see) a multiple of it.
    fn set_fps(&mut self, fps: u32) {
        let hz = tick_rate(self.min_tick_hz, fps);
        if let Err(err) = self.ticker.retime(hz) {
            warn!("retiming the compositor's clock: {err:#}");
            return;
        }
        info!(fps, tick_hz = hz, "changing the frame rate");
        let now = Instant::now();
        self.schedule
            .set_period(Duration::from_secs(1) / fps.max(1), now);
        self.tick_period = Duration::from_secs(1) / hz;
        if self.schedule.pacing() == Pacing::Commit {
            self.arm_alarm(now);
        }
        if let Some(mode) = self.output.current_mode() {
            let mode = OutputMode {
                refresh: (hz * 1000) as i32,
                ..mode
            };
            self.output
                .change_current_state(Some(mode), None, None, None);
            self.output.set_preferred(mode);
        }
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

    /// An encode tick (`Fixed`) or a due commit-paced composite: composite
    /// if anything changed and someone wants the picture. A keep-alive
    /// sends the last picture again.
    fn encode_wake(&mut self, keepalive: bool) {
        self.stats.encode_ticks += 1;
        self.publish_pointer();
        // The frame tap wants pictures while a reader keeps asking, with or without a viewer.
        let listeners = self.hub.has_listeners();
        let (tap_on, tap_force) = self
            .tap
            .as_mut()
            .map_or((false, false), |t| t.want(listeners));
        if tap_force || keepalive {
            self.force_frame = true;
        }
        // With nobody encoding, only a capture that is due composites (not every change).
        let wanted = if listeners {
            self.dirty || self.force_frame
        } else {
            tap_on && self.force_frame
        };
        if !self.dirty && !self.force_frame {
            self.stats.clean += 1;
        }
        if wanted {
            self.force_frame = false;
            self.dirty = false;
            if let Err(err) = self.composite() {
                warn!("compositing failed: {err:#}");
            }
            if self.schedule.pacing() == Pacing::Commit {
                self.present_driver();
                // No free buffer (the encoders still hold them all): the
                // change is still waiting.
                if self.dirty {
                    self.schedule.on_commit(true);
                }
            }
        }
    }

    /// The commit-paced alarm went off.
    fn alarm(&mut self) {
        let now = Instant::now();
        if self.schedule.pacing() != Pacing::Commit {
            return;
        }
        match self.schedule.poll(now) {
            Wake::Composite => self.encode_wake(false),
            Wake::KeepAlive => self.encode_wake(true),
            Wake::Idle => {}
        }
        self.arm_alarm(now);
    }

    /// Arms the alarm for the schedule's next composite, if one is waiting.
    fn arm_alarm(&self, now: Instant) {
        match self.schedule.due(now) {
            Some(at) => self.alarm.arm_in(at.saturating_duration_since(now)),
            None => self.alarm.disarm(),
        }
    }

    /// The driving surface committed a change: composite it as soon as the
    /// schedule allows (on the event loop's next turn at the earliest, once
    /// the commit is fully handled).
    pub fn driver_committed(&mut self) {
        self.schedule.on_commit(true);
        self.arm_alarm(Instant::now());
    }

    /// Tells the driving surface its last commit was composited now, with a
    /// variable refresh: a version 1 binder (gamescope) hears refresh 0, which
    /// with `--adaptive-sync` makes it paint when the game has a frame.
    fn present_driver(&mut self) {
        let Some(window) = self.driver.as_ref().and_then(|d| self.window_for(d)) else {
            return;
        };
        let output = self.output.clone();
        let mut feedback = OutputPresentationFeedback::new(&output);
        window.take_presentation_feedback(
            &mut feedback,
            |_, _| Some(output.clone()),
            |_, _| wp_presentation_feedback::Kind::Vsync,
        );
        self.ticks += 1;
        feedback.presented(
            self.clock.now(),
            Refresh::Variable(self.schedule.period()),
            self.ticks,
            wp_presentation_feedback::Kind::Vsync,
        );
    }

    /// Which surface drives compositing, and so the pacing: gamescope's
    /// window under `auto`, the first window under `commit`, none under
    /// `fixed`.
    fn update_pacing(&mut self, now: Instant) {
        let app_id = |surface: &WlSurface| {
            with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok()?.app_id.clone())
            })
        };
        let driver = match self.pacing_choice {
            PacingChoice::Fixed => None,
            PacingChoice::Auto => self.space.elements().find_map(|w| {
                let surface = w.toplevel()?.wl_surface();
                drives(app_id(surface).as_deref()).then(|| surface.clone())
            }),
            PacingChoice::Commit => self
                .space
                .elements()
                .find_map(|w| w.toplevel().map(|t| t.wl_surface().clone())),
        };
        if driver == self.driver {
            return;
        }
        self.driver = driver;
        let pacing = if self.driver.is_some() {
            Pacing::Commit
        } else {
            Pacing::Fixed
        };
        if pacing != self.schedule.pacing() {
            info!(?pacing, "compositing pacing");
            self.commit_paced
                .store(pacing == Pacing::Commit, Ordering::Relaxed);
            self.schedule.set_pacing(pacing, now);
            if pacing == Pacing::Fixed {
                self.alarm.disarm();
            } else {
                self.arm_alarm(now);
            }
        }
    }

    pub fn window_for(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_gamescope_drives_compositing_under_auto() {
        assert!(drives(Some("gamescope")));
        // Desktops and browsers keep the encode tick, and a window without
        // an app id (yet) isn't gamescope.
        for other in ["google-chrome", "firefox", "org.kde.dolphin", "steam", ""] {
            assert!(!drives(Some(other)), "{other}");
        }
        assert!(!drives(None));
    }

    #[test]
    fn the_clock_is_a_multiple_of_the_encode_rate() {
        assert_eq!(tick_rate(240, 60), 240);
        assert_eq!(tick_rate(240, 120), 240);
        assert_eq!(tick_rate(240, 90), 270);
        // Never below the encode rate.
        assert_eq!(tick_rate(60, 120), 120);
        assert_eq!(tick_rate(0, 90), 90);
        for fps in [60, 90, 120] {
            assert_eq!(tick_rate(240, fps) % fps, 0);
        }
    }
}
