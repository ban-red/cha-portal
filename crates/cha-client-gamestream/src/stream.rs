//! One session: launch (or resume) on the host, connect the stream, and relay
//! it to the player's channels until either side ends it. Patterns are
//! `cha-gateway`'s `host.rs`, which does the same for a browser.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use bytes::Bytes;
use cha_client::{
    AudioPacket, Codec, Ended, Feedback, Input, Session, SessionControl, StreamConfig, VideoFrame,
};
use cha_moonlight_input::{ClientInputEvent, InputState, rumble_levels};
use moonlight_common::AppId;
use moonlight_common::crypto::rustcrypto::RustCryptoBackend;
use moonlight_common::stream::audio::AudioConfig;
use moonlight_common::stream::control::ActiveGamepads;
use moonlight_common::stream::proto::MoonlightStreamSetup;
use moonlight_common::stream::proto::audio::AudioStreamEvent;
use moonlight_common::stream::proto::control::ControlStreamEvent;
use moonlight_common::stream::proto::control::packet::ControlPacket;
use moonlight_common::stream::proto::video::VideoStreamEvent;
use moonlight_common::stream::tokio::{MoonlightStream, MoonlightStreamEvent};
use moonlight_common::stream::video::{
    ColorRange, ColorSpace, ServerCodecModeSupport, VideoCapabilities, VideoFormat, VideoFormats,
};
use moonlight_common::stream::{
    AesIv, AesKey, EncryptionFlags, MoonlightStreamSettings, StreamingConfig,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::Client;

/// Frames and packets the player may fall behind by before we skip ahead (to
/// the next keyframe, for video).
const VIDEO_BACKLOG: usize = 32;
const AUDIO_BACKLOG: usize = 128;
const FEEDBACK_BACKLOG: usize = 64;

/// The first of `wanted` (the player's order) that the host can encode. AV1 is
/// skipped: `moonlight-common-rust` has no AV1 yet.
pub fn pick_codec(host: ServerCodecModeSupport, wanted: &[Codec]) -> Result<Codec> {
    for codec in wanted {
        match codec {
            Codec::Hevc if host.intersects(ServerCodecModeSupport::HEVC) => return Ok(*codec),
            Codec::H264 if host.intersects(ServerCodecModeSupport::H264) => return Ok(*codec),
            _ => {}
        }
    }
    bail!(
        "the host can't encode any of the codecs this player asked for ({wanted:?}; AV1 isn't \
         supported over GameStream yet)"
    )
}

fn formats(codec: Codec) -> VideoFormats {
    match codec {
        Codec::H264 => VideoFormats::H264,
        Codec::Hevc => VideoFormats::H265,
        Codec::Av1 => VideoFormats::empty(),
    }
}

fn matches(codec: Codec, format: VideoFormat) -> bool {
    format.contained_in(match codec {
        Codec::H264 => VideoFormats::MASK_H264,
        Codec::Hevc => VideoFormats::MASK_H265,
        Codec::Av1 => VideoFormats::MASK_AV1,
    })
}

/// What the player's control handle sends the stream task.
enum Command {
    Input(ClientInputEvent),
    Idr,
    Stop { quit_app: bool },
}

struct Control {
    commands: mpsc::UnboundedSender<Command>,
    /// What this session holds down, to let go of on `release_all` and `stop`.
    input: Mutex<InputState>,
    /// The stream's size, which absolute mouse positions refer to.
    width: u32,
    height: u32,
}

impl Control {
    fn send(&self, events: impl IntoIterator<Item = ClientInputEvent>) {
        for event in events {
            // The task is gone only when the session has ended.
            let _ = self.commands.send(Command::Input(event));
        }
    }
}

impl SessionControl for Control {
    fn input(&self, input: Input) {
        let events = self
            .input
            .lock()
            .expect("input lock")
            .apply(&input, self.width, self.height);
        self.send(events);
    }

    fn release_all(&self) {
        let events = self.input.lock().expect("input lock").release_all();
        self.send(events);
    }

    fn request_keyframe(&self) {
        let _ = self.commands.send(Command::Idr);
    }

    fn stop(&self, quit_app: bool) {
        self.release_all();
        let _ = self.commands.send(Command::Stop { quit_app });
    }
}

/// Launches (or resumes) `app_id` on the paired `host` and connects.
pub async fn start(host: Client, app_id: u32, config: StreamConfig) -> Result<Session> {
    if config.audio_channels != 2 {
        bail!(
            "only stereo audio is supported (asked for {} channels)",
            config.audio_channels
        );
    }
    let crypto = RustCryptoBackend;
    // Fresh from the host: what it can encode, and what it runs now.
    host.update().await.map_err(|e| anyhow!("{e:?}"))?;
    let version = host.version().await.map_err(|e| anyhow!("{e:?}"))?;
    let gfe = host.gfe_version().await.map_err(|e| anyhow!("{e:?}"))?;
    let modes = host
        .server_codec_mode_support()
        .await
        .map_err(|e| anyhow!("{e:?}"))?;
    let codec = pick_codec(modes, &config.codecs)?;
    let mut settings = MoonlightStreamSettings {
        width: config.width,
        height: config.height,
        fps: config.fps,
        fps_x100: config.fps * 100,
        bitrate: config.bitrate_kbps,
        packet_size: 1392,
        // moonlight-common-rust can't decrypt video.
        encryption_flags: EncryptionFlags::empty(),
        streaming_remotely: StreamingConfig::Local,
        sops: false,
        hdr: false,
        supported_video_formats: formats(codec),
        color_space: ColorSpace::Rec709,
        color_range: ColorRange::Limited,
        local_audio_play_mode: false,
        audio_config: AudioConfig::STEREO,
        gamepads_attached: ActiveGamepads::empty(),
        gamepads_persist_after_disconnect: false,
        enable_mic: false,
    };
    settings
        .adjust_for_server(version, &gfe, modes)
        .map_err(|e| anyhow!("the host can't stream {codec:?}: {e:?}"))?;

    // The library resumes whatever the host runs, so another app would be
    // attached instead of the one asked for. Ours is resumed; another is not
    // ours to quit.
    let running = host.current_game().await.map_err(|e| anyhow!("{e:?}"))?;
    if running != 0 && running != app_id {
        bail!(
            "the host is running another app (id {running}); resume that one, or quit it on the host"
        );
    }

    let started = Instant::now();
    let stream_config = host
        .start_stream(
            AppId(app_id),
            &settings,
            AesKey::new_random(&crypto).map_err(|e| anyhow!("{e:?}"))?,
            AesIv::new_random(&crypto).map_err(|e| anyhow!("{e:?}"))?,
            MoonlightStreamSetup::launch_query_parameters(),
        )
        .await
        .map_err(|e| anyhow!("launching app {app_id}: {e:?}"))?;
    let mut stream = MoonlightStream::connect(
        stream_config,
        settings,
        Arc::new(crypto),
        VideoCapabilities::default(),
    )
    .await
    .map_err(|e| anyhow!("connecting the stream: {e:?}"))?;
    let setup = stream.video_setup();
    let audio = stream.audio_setup();
    info!(
        ?setup,
        ?audio,
        launch_ms = started.elapsed().as_millis(),
        "moonlight stream up"
    );
    if !matches(codec, setup.format) {
        close(&mut stream).await;
        bail!(
            "asked for {codec:?}, but the stream came up as {:?}",
            setup.format
        );
    }
    // The player decodes one Opus stream of two channels; a surround mix
    // (multistream) would be noise to it.
    if audio.channel_count != 2 || audio.streams != 1 || audio.coupled_streams != 1 {
        close(&mut stream).await;
        bail!(
            "the host's audio is {} channels in {} streams; only stereo is supported",
            audio.channel_count,
            audio.streams
        );
    }

    let (width, height) = if setup.width > 0 && setup.height > 0 {
        (setup.width, setup.height)
    } else {
        (config.width, config.height)
    };
    let (video_tx, video) = mpsc::channel(VIDEO_BACKLOG);
    let (audio_tx, audio_rx) = mpsc::channel(AUDIO_BACKLOG);
    let (feedback_tx, feedback) = mpsc::channel(FEEDBACK_BACKLOG);
    let (ended_tx, ended) = oneshot::channel();
    let (commands, commands_rx) = mpsc::unbounded_channel();
    tokio::spawn(
        Relay {
            stream,
            host,
            codec,
            audio: AudioFormat {
                sample_rate: audio.sample_rate,
                samples: audio.samples_per_frame,
            },
            commands: commands_rx,
            video: video_tx,
            audio_out: audio_tx,
            feedback: feedback_tx,
            started,
        }
        .run(ended_tx),
    );
    Ok(Session {
        video,
        audio: audio_rx,
        feedback,
        ended,
        codec,
        width,
        height,
        control: Box::new(Control {
            commands,
            input: Mutex::default(),
            width,
            height,
        }),
    })
}

struct AudioFormat {
    sample_rate: u32,
    /// Per channel, per packet.
    samples: u32,
}

/// The task that owns the stream: it must keep driving it, whatever the
/// player is doing.
struct Relay {
    stream: MoonlightStream,
    host: Client,
    codec: Codec,
    audio: AudioFormat,
    commands: mpsc::UnboundedReceiver<Command>,
    video: mpsc::Sender<VideoFrame>,
    audio_out: mpsc::Sender<AudioPacket>,
    feedback: mpsc::Sender<Feedback>,
    started: Instant,
}

impl Relay {
    async fn run(mut self, ended: oneshot::Sender<Ended>) {
        let (reason, quit_app) = self.relay().await;
        close(&mut self.stream).await;
        // After the disconnect: the host won't take it for a lost client.
        if quit_app {
            match self.host.cancel().await {
                Ok(_) => info!("app closed on the host"),
                Err(err) => warn!("closing the app on the host: {err:?}"),
            }
        }
        let _ = ended.send(reason);
    }

    /// Relays until something ends the stream; says why, and whether to quit
    /// the app on the host.
    async fn relay(&mut self) -> (Ended, bool) {
        let mut first_frame = true;
        // After a frame was dropped (the player is behind), skip to a keyframe:
        // a decoder fed a gap shows garbage.
        let mut waiting_key = false;
        loop {
            tokio::select! {
                command = self.commands.recv() => match command {
                    Some(Command::Input(event)) => {
                        if let Err(err) = self.stream.send_input(event) {
                            warn!("sending input to the host: {err:?}");
                        }
                    }
                    Some(Command::Idr) => self.request_idr(),
                    Some(Command::Stop { quit_app }) => return (Ended::Stopped, quit_app),
                    // The session was dropped: stop, and leave the app be.
                    None => return (Ended::Stopped, false),
                },
                event = self.stream.drive() => match event {
                    Err(err) => return (Ended::Failed(format!("the host's stream failed: {err:?}")), false),
                    Ok(MoonlightStreamEvent::Video(VideoStreamEvent::OnFrame(frame))) => {
                        let received = Instant::now();
                        let md = frame.metadata();
                        // The type is in the packet header; `frame.as_ref()` would
                        // scan the whole access unit for NAL units. It isn't
                        // exported, hence Debug.
                        let key = format!("{:?}", md.frame_type) == "Idr";
                        if first_frame {
                            first_frame = false;
                            info!(after_ms = self.started.elapsed().as_millis(), "first frame from the host");
                        }
                        if waiting_key && !key {
                            continue;
                        }
                        let frame = VideoFrame {
                            codec: self.codec,
                            data: Bytes::copy_from_slice(frame.raw()),
                            key,
                            number: u64::from(md.frame_index.0),
                            received,
                        };
                        match self.video.try_send(frame) {
                            Ok(()) => {
                                waiting_key = false;
                                // Let the player's task run now: tokio parks a task
                                // woken from this worker in its LIFO slot until we
                                // yield, and we'd otherwise drain this frame's FEC
                                // packets first.
                                tokio::task::yield_now().await;
                            }
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                waiting_key = true;
                                self.request_idr();
                            }
                            // The player stopped listening to video; the session
                            // goes on until it is dropped or stopped.
                            Err(mpsc::error::TrySendError::Closed(_)) => {}
                        }
                    }
                    Ok(MoonlightStreamEvent::Video(VideoStreamEvent::SignalIdr)) => self.request_idr(),
                    Ok(MoonlightStreamEvent::Audio(AudioStreamEvent::OnFrame(frame))) => {
                        // A full channel is a player that isn't playing; Opus
                        // conceals a gap, so the packet is simply lost.
                        let _ = self.audio_out.try_send(AudioPacket {
                            data: frame.buffer,
                            channels: 2,
                            sample_rate: self.audio.sample_rate,
                            samples: self.audio.samples,
                        });
                    }
                    Ok(MoonlightStreamEvent::Control(ControlStreamEvent::Packet(packet))) => {
                        if let Some(feedback) = feedback_of(&packet) {
                            let _ = self.feedback.try_send(feedback);
                        }
                    }
                    Ok(MoonlightStreamEvent::Control(ControlStreamEvent::Disconnect)) => {
                        return (Ended::ByHost("the host closed the stream".into()), false);
                    }
                    Ok(_) => {}
                },
            }
        }
    }

    fn request_idr(&mut self) {
        if let Err(err) = self.stream.send_raw(ControlPacket::RequestIdr) {
            warn!("requesting an IDR: {err:?}");
        }
    }
}

/// What a host's control packet says to a controller, if it does.
fn feedback_of(packet: &ControlPacket) -> Option<Feedback> {
    match *packet {
        ControlPacket::ControllerRumbleData {
            controller_number,
            low_frequency,
            high_frequency,
            ..
        } => {
            let (low, high) = rumble_levels(low_frequency, high_frequency);
            Some(Feedback::Rumble {
                index: u8::try_from(controller_number).ok()?,
                low,
                high,
            })
        }
        ControlPacket::ControllerSetLed {
            controller_number,
            r,
            g,
            b,
        } => Some(Feedback::Led {
            index: u8::try_from(controller_number).ok()?,
            r,
            g,
            b,
        }),
        _ => None,
    }
}

/// Disconnects and keeps driving the stream until the ENet disconnect is
/// actually sent: `disconnect()` only queues it, and a peer left to time out
/// (about 5 s) can pause the same client's next session.
async fn close(stream: &mut MoonlightStream) {
    if let Err(err) = stream.disconnect() {
        warn!("disconnecting: {err:?}");
        return;
    }
    let flushed = tokio::time::timeout(Duration::from_millis(500), async {
        while stream.is_alive() {
            if stream.drive().await.is_err() {
                break;
            }
        }
    })
    .await;
    if flushed.is_err() {
        warn!("the host didn't acknowledge the disconnect within 500 ms");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codec_is_the_players_first_choice_the_host_can_encode() {
        let both = ServerCodecModeSupport::H264 | ServerCodecModeSupport::HEVC;
        assert_eq!(
            pick_codec(both, &[Codec::Av1, Codec::Hevc, Codec::H264]).unwrap(),
            Codec::Hevc
        );
        assert_eq!(
            pick_codec(both, &[Codec::H264, Codec::Hevc]).unwrap(),
            Codec::H264
        );
        let old = ServerCodecModeSupport::H264;
        assert_eq!(
            pick_codec(old, &[Codec::Hevc, Codec::H264]).unwrap(),
            Codec::H264
        );
        assert!(pick_codec(old, &[Codec::Hevc]).is_err());
        assert!(pick_codec(both, &[Codec::Av1]).is_err());
        assert!(pick_codec(both, &[]).is_err());
        assert!(pick_codec(ServerCodecModeSupport::empty(), &[Codec::H264]).is_err());
    }

    #[test]
    fn rumble_and_leds_become_feedback() {
        let rumble = ControlPacket::ControllerRumbleData {
            unused: 0,
            controller_number: 2,
            low_frequency: u16::MAX,
            high_frequency: 0,
        };
        assert_eq!(
            feedback_of(&rumble),
            Some(Feedback::Rumble {
                index: 2,
                low: 1.0,
                high: 0.0
            })
        );
        let led = ControlPacket::ControllerSetLed {
            controller_number: 1,
            r: 1,
            g: 2,
            b: 3,
        };
        assert_eq!(
            feedback_of(&led),
            Some(Feedback::Led {
                index: 1,
                r: 1,
                g: 2,
                b: 3
            })
        );
        assert_eq!(feedback_of(&ControlPacket::RequestIdr), None);
    }
}
