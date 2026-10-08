//! The native client's core (ADR 0010): what a transport hands the player and
//! what the player hands back, with no platform or protocol code.
//!
//! - A [`Transport`] finds [`Host`]s, pairs with them and lists their [`App`]s,
//!   and [`Transport::launch`] starts a [`Session`].
//! - A session delivers [`VideoFrame`]s, [`AudioPacket`]s and [`Feedback`] on
//!   channels, and takes [`Input`] and keyframe requests.
//! - The input model is the browser player's (keys by W3C `KeyboardEvent.code`,
//!   gamepads in the W3C standard mapping), so every transport maps from one
//!   model and the browser and native players agree.
//!
//! The player (decode, present, audio out, input capture) and each transport
//! (GameStream, later `cha-stream/1`) live in their own crates.

use std::time::Instant;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Boxed futures keep the traits object-safe without `async-trait`.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Video codecs a session can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    H264,
    Hevc,
    Av1,
    /// PyroWave, 4:2:0: an intra wavelet codec for a fast LAN (`cha-stream/1`
    /// over WebTransport only). Every frame stands alone; no keyframes.
    #[serde(rename = "pyrowave420")]
    PyroWave420,
    /// PyroWave, 4:4:4 (sharper text and desktops, more bandwidth).
    #[serde(rename = "pyrowave444")]
    PyroWave444,
}

impl Codec {
    /// PyroWave (either chroma format): intra-only, decoded on the GPU.
    pub fn is_pyrowave(self) -> bool {
        matches!(self, Codec::PyroWave420 | Codec::PyroWave444)
    }
}

/// Something that can be played: a Moonlight host, or later a portal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    /// Stable for the transport (a GameStream host's `uniqueid`).
    pub id: String,
    pub name: String,
    /// Where it was found or added, for showing (`192.168.1.20:47989`).
    pub address: String,
    pub paired: bool,
    /// The app it is running now, if any. A host that runs several at once
    /// (a portal) leaves this `None` and says so on each [`App`].
    pub running_app: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    pub id: u32,
    pub name: String,
    pub hdr: bool,
    /// Whether it is up on the host, as of when the apps were listed.
    #[serde(default)]
    pub state: AppState,
}

/// Whether an app is up on its host. Launching one that is resumes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppState {
    #[default]
    Stopped,
    /// On its way up (a portal environment pulling its image, say).
    Starting,
    Running,
    /// Closing down (a portal environment being stopped): it can't be
    /// resumed, and a new one starts once it has gone.
    Stopping,
}

impl AppState {
    /// Running or on its way: launching it joins it rather than starting another.
    pub fn is_up(self) -> bool {
        matches!(self, AppState::Starting | AppState::Running)
    }

    /// On its way up or down: worth looking again soon.
    pub fn is_changing(self) -> bool {
        matches!(self, AppState::Starting | AppState::Stopping)
    }
}

/// What the player asks a host for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Codecs the player can decode, most preferred first.
    pub codecs: Vec<Codec>,
    /// Stereo for now; more once surround is played.
    pub audio_channels: u8,
}

/// One encoded picture: Annex-B for H.264/HEVC, OBUs for AV1, concatenated
/// wavelet packets (starting with the 8-byte sequence header) for PyroWave.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub codec: Codec,
    pub data: Bytes,
    /// Always true for PyroWave (every frame stands alone).
    pub key: bool,
    /// PyroWave only: some of the frame's datagrams never came, so `data`
    /// holds only the packets that arrived whole and the decoder fills in the
    /// rest (softer where blocks are missing). Always false otherwise.
    pub partial: bool,
    /// The transport's frame counter, increasing.
    pub number: u64,
    pub received: Instant,
}

/// One Opus packet.
#[derive(Clone, Debug)]
pub struct AudioPacket {
    pub data: Bytes,
    pub channels: u8,
    pub sample_rate: u32,
    /// Samples per channel this packet decodes to.
    pub samples: u32,
}

/// Input, in the browser player's model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Input {
    /// `code` is a W3C `KeyboardEvent.code` (`"KeyA"`, `"ShiftLeft"`).
    Key { code: String, down: bool },
    /// Absolute position in 0..1 of the picture.
    MouseMove { x: f32, y: f32 },
    /// Relative motion in picture pixels (pointer lock).
    MouseMotion { dx: f32, dy: f32 },
    /// `PointerEvent.button`: 0 left, 1 middle, 2 right, 3 back, 4 forward.
    MouseButton { button: u8, down: bool },
    /// Wheel in pixels, as a browser reports it (100 per notch, y down).
    Wheel { dx: f32, dy: f32 },
    /// A gamepad's whole state, `index` 0..4.
    Pad { index: u8, pad: PadState },
    /// That gamepad went away.
    PadGone { index: u8 },
}

/// A gamepad in the W3C standard mapping: `buttons[0..17]` 0..1 (6 and 7 are
/// the analog triggers), `axes[0..4]` −1..1 (Y down, as browsers report).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PadState {
    pub buttons: Vec<f32>,
    pub axes: Vec<f32>,
}

/// What a host sends back about controllers, and what a transport says
/// about the link itself.
#[derive(Clone, Debug, PartialEq)]
pub enum Feedback {
    /// Motor levels 0..1; both 0 stops.
    Rumble {
        index: u8,
        low: f32,
        high: f32,
    },
    Led {
        index: u8,
        r: u8,
        g: u8,
        b: u8,
    },
    /// The stream dropped and the transport is connecting again inside this
    /// session: show a pause ("Reconnecting...") and keep the channels and the
    /// decoder. Sent at the drop (`attempt` 1) and again for each further
    /// attempt; `reason` says what went wrong, for a tooltip or the log.
    /// Frames start again at a keyframe once the link is back.
    Reconnecting {
        attempt: u32,
        reason: String,
    },
    /// The stream is back after [`Feedback::Reconnecting`].
    Reconnected,
}

/// Why a session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// We asked (`stop`).
    Stopped,
    /// The host quit the app or closed the stream.
    ByHost(String),
    /// The connection failed.
    Failed(String),
}

/// A running stream. Dropping it stops it without quitting the app.
pub struct Session {
    pub video: mpsc::Receiver<VideoFrame>,
    pub audio: mpsc::Receiver<AudioPacket>,
    pub feedback: mpsc::Receiver<Feedback>,
    /// Resolves once, when the session ends.
    pub ended: tokio::sync::oneshot::Receiver<Ended>,
    /// The codec and size the host chose.
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub control: Box<dyn SessionControl>,
}

/// What the player can ask of a running session.
pub trait SessionControl: Send + Sync {
    fn input(&self, input: Input);
    /// Let go of everything held (keys, buttons, pads): on focus loss.
    fn release_all(&self);
    fn request_keyframe(&self);
    /// End the stream; `quit_app` also quits the app on the host.
    fn stop(&self, quit_app: bool);
    /// What the transport knows about the link and the host, for the stats
    /// overlay; `None` when it can't say anything (the default).
    fn transport_stats(&self) -> Option<TransportStats> {
        None
    }
    /// Ask the host for another frame rate. Only a transport that reports
    /// [`TransportStats::control`] can; the default does nothing.
    fn set_fps(&self, _fps: u32) {}
    /// Set the app's performance overlay (0 off to 4 full). Only for a
    /// transport that reports an [`TransportStats::overlay`]; the default does
    /// nothing.
    fn set_overlay(&self, _level: u8) {}
    /// Ask for the keyboard and mouse back when another session has them
    /// ([`TransportStats::control`] is `Some(false)`); the default does nothing.
    fn take_control(&self) {}
}

/// An app's performance overlay (Steam's Gamescope one), as the host reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfOverlay {
    /// 0 (off) to 4 (full).
    Preset(u8),
    /// A hand-written config the host doesn't change.
    Custom,
}

/// What a transport can say about the link and the host right now. Every
/// reading a transport doesn't have is `None`; the overlay hides it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransportStats {
    /// How the stream travels, in a few letters for the codec text ("WT" for
    /// WebTransport, "GS" for GameStream).
    pub tag: &'static str,
    /// Round trip to the host, ms.
    pub rtt_ms: Option<f32>,
    /// Frames the transport gave up on, since the session began (it survives
    /// reconnects). PyroWave frames lost are skipped frames. A transport counts
    /// frames, not packets, so the health check judges them on the frame scale.
    pub lost: u64,
    /// Frames rebuilt from parity instead of lost, since the session began.
    pub recovered: u64,
    /// The frame rate the host encodes at.
    pub target_fps: Option<u32>,
    /// Frames per second the host sent over its last few reports, and the
    /// span those reports cover, ms. The host sends only when the picture
    /// changes, so a still screen sends almost none.
    pub sent_fps: Option<f32>,
    pub sent_span_ms: Option<u32>,
    /// The host's composited to encoded p99 over its last report, ms.
    pub encode_p99_ms: Option<f32>,
    /// The host's resource use, if it reported lately.
    pub node: Option<NodeStats>,
    /// Whether this session has the keyboard and mouse (the floor): `None`
    /// when the transport has no such thing, and so no frame rate or overlay
    /// to change from here.
    pub control: Option<bool>,
    /// Sessions watching this stream, ours included.
    pub viewers: Option<u32>,
    /// Another session has the floor and this one may take it.
    pub can_take: bool,
    /// The app's performance overlay; `None` when it has none.
    pub overlay: Option<PerfOverlay>,
}

/// The node's CPU, RAM and GPU, as the streamer's `system` message gives
/// them. Percent is 0..100, memory is in bytes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeStats {
    pub cpu: f32,
    pub cores: u32,
    pub load1: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub gpu: Option<f32>,
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
    /// NVENC and NVDEC utilisation.
    pub enc: Option<f32>,
    pub dec: Option<f32>,
    /// °C, watts and MHz.
    pub temp: Option<f32>,
    pub power: Option<f32>,
    pub power_limit: Option<f32>,
    pub clock: Option<f32>,
    /// The streamer's own CPU, in percent of one core (it may pass 100).
    pub streamer_cpu: f32,
}

/// A way to find and play hosts.
pub trait Transport: Send + Sync {
    /// Shown in the UI ("Moonlight").
    fn name(&self) -> &str;
    /// Hosts known now (found on the LAN, added by address, paired before).
    fn hosts(&self) -> Vec<Host>;
    /// Add a host by address (`192.168.1.20` or `host:47989`).
    fn add_host(&self, address: &str) -> BoxFuture<'_, anyhow::Result<Host>>;
    /// Start pairing: returns the PIN to type on the host, and resolves the
    /// second future when the host accepts it (or fails).
    fn pair(&self, host_id: &str) -> BoxFuture<'_, anyhow::Result<Pairing>>;
    fn apps(&self, host_id: &str) -> BoxFuture<'_, anyhow::Result<Vec<App>>>;
    fn launch(
        &self,
        host_id: &str,
        app_id: u32,
        config: StreamConfig,
    ) -> BoxFuture<'_, anyhow::Result<Session>>;
    /// Quit the app on the host, unsaved work included. By default it
    /// resumes the app and stops that session with `quit_app`; a transport
    /// that can quit without streaming does so.
    fn quit_app(
        &self,
        host_id: &str,
        app_id: u32,
        config: StreamConfig,
    ) -> BoxFuture<'_, anyhow::Result<()>> {
        let host_id = host_id.to_string();
        Box::pin(async move {
            let session = self.launch(&host_id, app_id, config).await?;
            session.control.stop(true);
            Ok(())
        })
    }
}

/// A pairing in progress.
pub struct Pairing {
    /// What to show large: four digits to type on the host (or, for our
    /// nodes, in the portal), or the code a portal asks to be approved.
    pub pin: String,
    /// What to do with `pin`, in a sentence or two for the pairing view.
    pub instructions: String,
    pub done: BoxFuture<'static, anyhow::Result<()>>,
}
