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
    /// The app it is running now, if any.
    pub running_app: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    pub id: u32,
    pub name: String,
    pub hdr: bool,
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

/// One encoded picture: Annex-B for H.264/HEVC, OBUs for AV1.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub codec: Codec,
    pub data: Bytes,
    pub key: bool,
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

/// What a host sends back about controllers.
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
