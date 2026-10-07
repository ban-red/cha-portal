//! `cha_gamestream::MediaBackend` over the streamer's engine: the shared
//! encoder, the Opus mixer, the compositor's input and the virtual pads.
//!
//! A GameStream session is one more viewer. It joins as an owner (so it takes
//! the floor, as the newest owner does), changes the environment's size and
//! frame rate to what the client asked for, and subscribes to the encoder of
//! the codec it negotiated; its bitrate is the pace it asks of that encoder.

use std::sync::{Arc, Mutex};

use cha_gamestream::backend::BackendError;
use cha_gamestream::{
    Capabilities, EncodedVideo, InputEvent, MediaBackend, MediaControl, MediaStreams, OpusPacket,
    StreamParams,
};
use cha_nvenc::Codec;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::feedback;
use super::input::{Action, Mapper};
use super::{BoxFuture, Engine};
use crate::audio::AudioPacket;
use crate::codec::VideoCodec;
use crate::framerate;
use crate::gamepad::Gamepads;
use crate::media::{EncodedFrame, Media, Pace};
use crate::viewers::{Role, Seat, Viewer};

/// Frames and packets in flight between the engine and the session's tasks.
const QUEUE: usize = 8;
const AUDIO_QUEUE: usize = 32;
const FEEDBACK_QUEUE: usize = 64;

pub struct EngineBackend {
    engine: Engine,
}

impl EngineBackend {
    pub fn new(engine: Engine) -> Self {
        Self { engine }
    }
}

/// Our codec for the protocol's, if the protocol has it.
fn ours(codec: cha_gamestream::VideoCodec) -> VideoCodec {
    VideoCodec::Hw(match codec {
        cha_gamestream::VideoCodec::H264 => Codec::H264,
        cha_gamestream::VideoCodec::Hevc => Codec::Hevc,
        cha_gamestream::VideoCodec::Av1 => Codec::Av1,
    })
}

/// The protocol's codec for ours: none for PyroWave.
fn theirs(codec: VideoCodec) -> Option<cha_gamestream::VideoCodec> {
    match codec {
        VideoCodec::Hw(Codec::H264) => Some(cha_gamestream::VideoCodec::H264),
        VideoCodec::Hw(Codec::Hevc) => Some(cha_gamestream::VideoCodec::Hevc),
        VideoCodec::Hw(Codec::Av1) => Some(cha_gamestream::VideoCodec::Av1),
        VideoCodec::PyroWave(_) => None,
    }
}

/// The nearest frame rate the environment runs at (60, 90 or 120); the
/// higher on a tie.
fn nearest_fps(asked: u32) -> u32 {
    framerate::CHOICES
        .into_iter()
        .min_by_key(|&fps| (fps.abs_diff(asked), std::cmp::Reverse(fps)))
        .expect("there are choices")
}

/// Who a client is in the viewer list: the start of its fingerprint.
fn user(client: &cha_gamestream::ClientId) -> String {
    let fingerprint = client.fingerprint();
    format!("moonlight:{}", &fingerprint[..fingerprint.len().min(8)])
}

impl MediaBackend for EngineBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            codecs: self
                .engine
                .media
                .codecs()
                .iter()
                .filter_map(|c| theirs(*c))
                .collect(),
            hdr: false,
            yuv444: false,
        }
    }

    fn start(&self, params: StreamParams) -> BoxFuture<'_, Result<MediaStreams, BackendError>> {
        Box::pin(self.engine.clone().start(params))
    }
}

impl Engine {
    async fn start(self, params: StreamParams) -> Result<MediaStreams, BackendError> {
        let codec = ours(params.codec);
        if !self.media.codecs().contains(&codec) {
            return Err(BackendError::Unsupported(format!(
                "{:?} isn't offered here",
                params.codec
            )));
        }
        // Audio first: it can be refused without having touched anything.
        let audio = self.subscribe_audio(&params)?;

        // A seat as an owner, which takes the floor: the browser sessions
        // lose it, and this client's input reaches the environment.
        let seat = self
            .viewers
            .join(
                Viewer {
                    user: user(&params.client),
                    role: Role::Owner,
                },
                false,
            )
            .await
            .map_err(BackendError::Failed)?;

        let (width, height) = self.media.resize(params.width, params.height);
        let fps = nearest_fps(params.fps);
        self.media.set_fps(fps).map_err(BackendError::Failed)?;
        info!(
            asked = format!("{}x{}@{}", params.width, params.height, params.fps),
            applied = format!("{width}x{height}@{fps}"),
            codec = ?params.codec,
            bitrate_bps = params.bitrate_bps,
            "GameStream stream starting"
        );

        let subscription = self
            .media
            .subscribe(codec)
            .map_err(|e| BackendError::Failed(format!("{e:#}")))?;
        subscription.pace.set_target(params.bitrate_bps);

        let (video_tx, video) = mpsc::channel(QUEUE);
        tokio::spawn(forward_video(subscription.frames, video_tx));

        let (audio_tx, audio_rx) = mpsc::channel(AUDIO_QUEUE);
        if let Some(packets) = audio {
            // What one packet covers, per channel at 48 kHz.
            let samples = u64::from(params.audio.packet_duration_ms) * 48;
            tokio::spawn(forward_audio(packets, audio_tx, samples));
        }

        let shared = Arc::new(Shared {
            media: Arc::clone(&self.media),
            gamepads: self.gamepads.clone(),
            seat: Mutex::new(Some(seat)),
            pace: subscription.pace,
            codec,
            mapper: Mutex::default(),
        });

        let (feedback_tx, feedback_rx) = mpsc::channel(FEEDBACK_QUEUE);
        if let Some(pads) = &self.gamepads {
            let replay = pads
                .memory()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .replay();
            let gate = Arc::clone(&shared);
            tokio::spawn(feedback::run(
                pads.events(),
                feedback_tx,
                move || gate.has_control(),
                replay,
            ));
        }

        Ok(MediaStreams {
            video,
            audio: audio_rx,
            feedback: feedback_rx,
            control: Arc::new(Control { shared }),
        })
    }

    /// The Opus packets the client wants, or `None` where there is no sound.
    /// What isn't made (surround, other packet lengths) is refused here.
    fn subscribe_audio(
        &self,
        params: &StreamParams,
    ) -> Result<Option<mpsc::Receiver<AudioPacket>>, BackendError> {
        let Some(audio) = &self.audio else {
            return Ok(None);
        };
        if params.audio.channels != 2 {
            return Err(BackendError::Unsupported(format!(
                "{}-channel audio: only stereo is made, choose stereo in the client",
                params.audio.channels
            )));
        }
        audio
            .subscribe_frames(params.audio.packet_duration_ms)
            .map(Some)
            .map_err(|e| BackendError::Unsupported(format!("{e:#}")))
    }
}

/// Frames from the shared encoder to the session, until either side is gone.
/// (If the session is gone the subscription must go too, or its pace would
/// keep capping the others.)
async fn forward_video(mut frames: mpsc::Receiver<EncodedFrame>, tx: mpsc::Sender<EncodedVideo>) {
    loop {
        tokio::select! {
            frame = frames.recv() => {
                let Some(frame) = frame else { return };
                if tx.send(video(frame)).await.is_err() {
                    return;
                }
            }
            () = tx.closed() => return,
        }
    }
}

fn video(frame: EncodedFrame) -> EncodedVideo {
    EncodedVideo {
        data: frame.data,
        key: frame.key,
        index: frame.index,
        // From the composited picture, so the client's frame processing
        // latency counts the encoder.
        captured: frame.composited,
    }
}

async fn forward_audio(
    mut packets: mpsc::Receiver<AudioPacket>,
    tx: mpsc::Sender<OpusPacket>,
    samples: u64,
) {
    loop {
        tokio::select! {
            packet = packets.recv() => {
                let Some(packet) = packet else { return };
                let packet = OpusPacket { data: packet.data, samples };
                if tx.send(packet).await.is_err() {
                    return;
                }
            }
            () = tx.closed() => return,
        }
    }
}

/// What the running stream's tasks and its control handle share.
struct Shared {
    media: Arc<Media>,
    gamepads: Option<Arc<Gamepads>>,
    /// The client's place among the viewers; given up when the stream stops.
    seat: Mutex<Option<Seat>>,
    pace: Arc<Pace>,
    codec: VideoCodec,
    mapper: Mutex<Mapper>,
}

impl Shared {
    /// Whether the client has the floor: only then does its input (or an
    /// app's rumble for it) count.
    fn has_control(&self) -> bool {
        self.seat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(Seat::has_control)
    }

    fn release(&self) {
        self.media.release_input();
        if let Some(pads) = &self.gamepads {
            pads.release_all();
        }
        self.mapper
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reset();
    }
}

struct Control {
    shared: Arc<Shared>,
}

impl MediaControl for Control {
    fn request_keyframe(&self) {
        self.shared.media.request_keyframe(self.shared.codec);
    }

    fn invalidate(&self, first: u64, _last: u64) {
        // The encoder refers to frames before `first` from then on.
        self.shared
            .media
            .request_invalidate(self.shared.codec, first);
    }

    fn set_bitrate(&self, bps: u32) {
        self.shared.pace.set_target(bps);
    }

    fn input(&self, event: InputEvent) {
        let shared = &self.shared;
        if !shared.has_control() {
            return;
        }
        let size = shared.media.size();
        shared
            .mapper
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(event, size, &mut |action| match action {
                Action::Input(input) => shared.media.input(input),
                Action::Pad(state) => match &shared.gamepads {
                    Some(pads) => pads.update(&state),
                    None => warn_once_no_pads(),
                },
            });
    }

    fn release_input(&self) {
        self.shared.release();
    }

    fn stop(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // Let go of what the client held, then of its seat (the floor
            // goes to whoever may have it next).
            self.shared.release();
            self.shared
                .seat
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            info!("GameStream stream stopped");
        })
    }
}

fn warn_once_no_pads() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| warn!("a GameStream client has a gamepad, but this environment has none"));
}
