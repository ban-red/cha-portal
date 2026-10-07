//! The media boundary: what produces a session's encoded video and Opus
//! audio and takes its input. Types here are neutral: no compositor, no
//! evdev, no encoder handles.

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use futures_util::future::BoxFuture;
use tokio::sync::mpsc;

pub use crate::handoff::Capabilities;
use crate::handoff::StreamParams;
use crate::input::InputEvent;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("the backend can't serve this stream: {0}")]
    Unsupported(String),
    #[error("the backend failed: {0}")]
    Failed(String),
}

/// One access unit: Annex-B for H.264 and HEVC, OBUs for AV1.
#[derive(Clone, Debug)]
pub struct EncodedVideo {
    pub data: Bytes,
    /// A keyframe: decodable on its own (an IDR, with its parameter sets).
    pub key: bool,
    /// The backend's own frame number. [`MediaControl::invalidate`] speaks
    /// in these; the wire's frame numbers stay inside the host.
    pub index: u64,
    /// When the picture was captured, for the frame-processing latency the
    /// client shows.
    pub captured: Instant,
}

/// One Opus packet of the negotiated multistream layout.
#[derive(Clone, Debug)]
pub struct OpusPacket {
    pub data: Bytes,
    /// Samples per channel at 48 kHz the packet covers (240 for 5 ms).
    pub samples: u64,
}

/// HDR10 static metadata, in the units of the SEI: chromaticities in
/// 0.00002, luminance in 0.0001 cd/m², light levels in cd/m².
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdrMetadata {
    pub display_primaries: [(u16, u16); 3],
    pub white_point: (u16, u16),
    pub max_luminance: u32,
    pub min_luminance: u32,
    pub max_cll: u16,
    pub max_fall: u16,
}

impl HdrMetadata {
    /// BT.2020 primaries, D65, 1000 nits: for sources that give nothing.
    pub fn fallback() -> Self {
        Self {
            display_primaries: [(34000, 16000), (13250, 34500), (7500, 3000)],
            white_point: (15635, 16450),
            max_luminance: 10_000_000,
            min_luminance: 10,
            max_cll: 0,
            max_fall: 0,
        }
    }
}

/// Things the backend asks the host to tell the client.
#[derive(Clone, Debug, PartialEq)]
pub enum Feedback {
    Rumble {
        pad: u16,
        low: u16,
        high: u16,
    },
    RumbleTriggers {
        pad: u16,
        left: u16,
        right: u16,
    },
    Led {
        pad: u16,
        rgb: (u8, u8, u8),
    },
    /// Start or stop sending motion for `pad` (`kind` 1 acceleration, 2 gyro).
    MotionEnable {
        pad: u16,
        rate_hz: u16,
        kind: u8,
    },
    TriggerEffect {
        pad: u16,
        event_flags: u8,
        type_left: u8,
        type_right: u8,
        left: [u8; 10],
        right: [u8; 10],
    },
    /// The picture is (or stopped being) HDR. With metadata, the host also
    /// puts it into the keyframes' SEI (or OBU metadata); without, the
    /// backend is taken to carry its own and the client gets the fallback.
    Hdr {
        enabled: bool,
        metadata: Option<HdrMetadata>,
    },
}

/// A running stream: what comes out of the backend, and the handle to steer
/// it.
pub struct MediaStreams {
    pub video: mpsc::Receiver<EncodedVideo>,
    pub audio: mpsc::Receiver<OpusPacket>,
    pub feedback: mpsc::Receiver<Feedback>,
    pub control: Arc<dyn MediaControl>,
}

/// The handle to one started stream. Calls come from the control task and
/// must not block.
pub trait MediaControl: Send + Sync + 'static {
    /// The client needs a picture it can start from.
    fn request_keyframe(&self);
    /// The client lost frames `first..=last` (backend frame numbers): stop
    /// referencing them, or send a keyframe.
    fn invalidate(&self, first: u64, last: u64);
    fn set_bitrate(&self, bps: u32);
    fn input(&self, event: InputEvent);
    /// The client's input is over (it disconnected): let go of held keys,
    /// buttons and pads.
    fn release_input(&self);
    /// End the stream. The host calls it once, when the session's media ends.
    fn stop(&self) -> BoxFuture<'_, ()>;
}

pub trait MediaBackend: Send + Sync + 'static {
    fn capabilities(&self) -> Capabilities;
    /// Start encoding for `params`. Frames may flow at once; the host
    /// discards video until the client is ready and asks for a keyframe
    /// when it is.
    fn start(&self, params: StreamParams) -> BoxFuture<'_, Result<MediaStreams, BackendError>>;
}
