//! One session: launch (or resume) on the host, connect the stream, and relay
//! it to the player's channels until either side ends it. Patterns are
//! `cha-gateway`'s `host.rs`, which does the same for a browser.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use cha_client::{
    AudioPacket, Codec, Ended, Feedback, Input, Session, SessionControl, StreamConfig,
    TransportStats, VideoFrame,
};
use cha_gamestream::VideoCodec;
use cha_gamestream::client::front::{Encrypt, HostClient, StreamRequest};
use cha_gamestream::client::media::{self, MediaClient, MediaHandle, MediaOptions, MediaStats};
use cha_gamestream::client::{self, InputEvent};
use cha_moonlight_input::{InputState, rumble_levels};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

/// Frames and packets the player may fall behind by before the stream skips
/// ahead (to the next keyframe, for video: the media client drops and asks).
const VIDEO_BACKLOG: usize = 32;
const AUDIO_BACKLOG: usize = 128;
const FEEDBACK_BACKLOG: usize = 64;
/// How long a stopping session waits for the media client's goodbye to the
/// host before it quits the app.
const GOODBYE: Duration = Duration::from_secs(2);
/// The host's Opus is 48 kHz.
const SAMPLE_RATE: u32 = 48_000;

/// A Moonlight host never offers PyroWave (that is our own codec, carried
/// only by `cha-stream/1`), so those are `None` and left out of what is asked.
fn wire_codec(codec: Codec) -> Option<VideoCodec> {
    match codec {
        Codec::H264 => Some(VideoCodec::H264),
        Codec::Hevc => Some(VideoCodec::Hevc),
        Codec::Av1 => Some(VideoCodec::Av1),
        Codec::PyroWave420 | Codec::PyroWave444 => None,
    }
}

fn player_codec(codec: VideoCodec) -> Codec {
    match codec {
        VideoCodec::H264 => Codec::H264,
        VideoCodec::Hevc => Codec::Hevc,
        VideoCodec::Av1 => Codec::Av1,
    }
}

/// The codecs of `wanted` (the player's order) that the host encodes. None is
/// an error: the player decodes nothing the host can send.
pub fn offered(host: &[VideoCodec], wanted: &[Codec]) -> Result<Vec<VideoCodec>> {
    let offered: Vec<VideoCodec> = wanted
        .iter()
        .filter_map(|c| wire_codec(*c))
        .filter(|c| host.contains(c))
        .collect();
    if offered.is_empty() {
        bail!("the host can't encode any of the codecs this player asked for ({wanted:?})");
    }
    Ok(offered)
}

/// What the player's control handle sends the supervising task.
enum Command {
    Stop { quit_app: bool },
}

struct Control {
    media: MediaHandle,
    commands: mpsc::UnboundedSender<Command>,
    /// What this session holds down, to let go of on `release_all` and `stop`.
    input: Mutex<InputState>,
    /// The stream's size, which absolute mouse positions refer to.
    width: u32,
    height: u32,
    /// The frame rate the host was asked to encode at, which it holds to.
    fps: u32,
}

/// How the stream travels, for the codec text ("HEVC/GS"): GameStream, the
/// protocol Moonlight speaks.
const TRANSPORT_TAG: &str = "GS";

/// What the media client has measured, as the player's stats read it. A
/// GameStream host reports nothing about itself, so there is no node, send
/// rate or encode time here. `lost` and `recovered` count frames, as the
/// WebTransport transport's do: the video path gives up on whole frames, and
/// counts a frame once however many of its FEC blocks were rebuilt.
fn transport_stats_of(stats: &MediaStats, fps: u32) -> TransportStats {
    TransportStats {
        tag: TRANSPORT_TAG,
        rtt_ms: stats.rtt_ms.map(|ms| ms as f32),
        lost: stats.video.frames_lost,
        recovered: stats.video.frames_recovered,
        target_fps: Some(fps).filter(|f| *f > 0),
        ..TransportStats::default()
    }
}

impl Control {
    fn send(&self, events: impl IntoIterator<Item = InputEvent>) {
        for event in events {
            self.media.input(event);
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
        self.media.request_idr();
    }

    fn stop(&self, quit_app: bool) {
        self.release_all();
        // The task is gone only when the session has ended.
        let _ = self.commands.send(Command::Stop { quit_app });
    }

    fn transport_stats(&self) -> Option<TransportStats> {
        Some(transport_stats_of(&self.media.stats(), self.fps))
    }
}

/// Launches (or resumes) `app_id` on the paired `host` and connects.
pub async fn start(host: HostClient, app_id: u32, config: StreamConfig) -> Result<Session> {
    if config.audio_channels != 2 {
        bail!(
            "only stereo audio is supported (asked for {} channels)",
            config.audio_channels
        );
    }
    // Fresh from the host: what it can encode, and what it runs now.
    let info = host
        .server_info()
        .await
        .map_err(|e| anyhow!("reading the host's state: {e}"))?;
    let codecs = offered(&info.codecs(), &config.codecs)?;

    // A resume reconnects to whatever the host runs, so another app would be
    // attached instead of the one asked for. Ours is resumed; another is not
    // ours to quit.
    let running = info.current_game;
    if running != 0 && running != app_id {
        bail!(
            "the host is running another app (id {running}); resume that one, or quit it on the host"
        );
    }

    let mut request = StreamRequest::new(app_id, config.width, config.height, config.fps);
    request.bitrate_kbps = config.bitrate_kbps;
    request.codecs = codecs;
    request.audio_channels = 2;
    // Encrypted wherever the host can: Wi-Fi and shared LANs are not private.
    request.video_encryption = Encrypt::IfSupported;
    request.audio_encryption = Encrypt::IfSupported;

    let started = Instant::now();
    let setup = if running == app_id {
        host.resume(&request).await
    } else {
        host.launch(&request).await
    }
    .map_err(|e| anyhow!("launching app {app_id}: {e}"))?;
    // The player decodes one Opus stream of two channels; a surround mix
    // (multistream) would be noise to it.
    let audio = &setup.audio;
    if audio.channels != 2 || audio.streams != 1 || audio.coupled_streams != 1 {
        bail!(
            "the host's audio is {} channels in {} streams; only stereo is supported",
            audio.channels,
            audio.streams
        );
    }
    let samples = SAMPLE_RATE / 1000 * audio.packet_duration_ms;
    let options = MediaOptions {
        video_queue: VIDEO_BACKLOG,
        audio_queue: AUDIO_BACKLOG,
        feedback_queue: FEEDBACK_BACKLOG,
        ..MediaOptions::default()
    };
    let stream = MediaClient::start_with(&setup, options)
        .await
        .map_err(|e| anyhow!("connecting the stream: {e}"))?;
    info!(
        codec = ?setup.codec,
        width = setup.width,
        height = setup.height,
        encrypted = ?setup.encryption,
        launch_ms = started.elapsed().as_millis(),
        "moonlight stream up"
    );

    let codec = player_codec(setup.codec);
    let (video_tx, video) = mpsc::channel(VIDEO_BACKLOG);
    let (audio_tx, audio_rx) = mpsc::channel(AUDIO_BACKLOG);
    let (feedback_tx, feedback) = mpsc::channel(FEEDBACK_BACKLOG);
    let (ended_tx, ended) = oneshot::channel();
    let (commands, commands_rx) = mpsc::unbounded_channel();
    let media::Media {
        video: stream_video,
        audio: stream_audio,
        feedback: stream_feedback,
        ended: stream_ended,
        control,
    } = stream;
    tokio::spawn(forward_video(stream_video, video_tx, codec, started));
    tokio::spawn(forward_audio(stream_audio, audio_tx, samples));
    tokio::spawn(
        Relay {
            media: control.clone(),
            host,
            commands: commands_rx,
            feedback: stream_feedback,
            feedback_out: feedback_tx,
            ended: stream_ended,
        }
        .run(ended_tx),
    );
    Ok(Session {
        video,
        audio: audio_rx,
        feedback,
        ended,
        codec,
        width: setup.width,
        height: setup.height,
        control: Box::new(Control {
            media: control,
            commands,
            input: Mutex::default(),
            width: setup.width,
            height: setup.height,
            fps: setup.fps,
        }),
    })
}

/// Whole frames to the player. The media client has already dropped what the
/// network lost and asked for a keyframe; a player that falls behind makes
/// this wait, and the media client's own queue then drops frames and asks
/// again, so what reaches the player is always a decodable run.
async fn forward_video(
    mut frames: mpsc::Receiver<client::VideoFrame>,
    out: mpsc::Sender<VideoFrame>,
    codec: Codec,
    started: Instant,
) {
    let mut first = true;
    while let Some(frame) = frames.recv().await {
        if first {
            first = false;
            info!(
                after_ms = started.elapsed().as_millis(),
                "first frame from the host"
            );
        }
        // A player that stopped listening to video: the session goes on until
        // it is dropped or stopped, and the frames are let go.
        let _ = out
            .send(VideoFrame {
                codec,
                data: frame.data,
                key: frame.key,
                partial: false,
                number: u64::from(frame.number),
                received: frame.received,
            })
            .await;
    }
}

async fn forward_audio(
    mut packets: mpsc::Receiver<client::AudioPacket>,
    out: mpsc::Sender<AudioPacket>,
    samples: u32,
) {
    while let Some(packet) = packets.recv().await {
        // A full channel is a player that isn't playing; Opus conceals a gap,
        // so the packet is simply lost.
        let _ = out.try_send(AudioPacket {
            data: packet.data,
            channels: 2,
            sample_rate: SAMPLE_RATE,
            samples,
        });
    }
}

/// The task that owns the stream's ends: it relays what the host says to a
/// controller and decides how the session ends, whatever the player is doing.
struct Relay {
    media: MediaHandle,
    host: HostClient,
    commands: mpsc::UnboundedReceiver<Command>,
    feedback: mpsc::Receiver<cha_gamestream::Feedback>,
    feedback_out: mpsc::Sender<Feedback>,
    ended: oneshot::Receiver<media::Ended>,
}

impl Relay {
    async fn run(mut self, ended: oneshot::Sender<Ended>) {
        let reason = loop {
            tokio::select! {
                command = self.commands.recv() => {
                    // The session was dropped: stop, and leave the app be.
                    let quit_app = match command {
                        Some(Command::Stop { quit_app }) => quit_app,
                        None => false,
                    };
                    break self.stop(quit_app).await;
                }
                event = self.feedback.recv() => match event {
                    Some(event) => {
                        if let Some(feedback) = feedback_of(&event) {
                            let _ = self.feedback_out.try_send(feedback);
                        }
                    }
                    // The control task is gone, and says why on `ended`.
                    None => break map_ended((&mut self.ended).await),
                },
                result = &mut self.ended => break map_ended(result),
            }
        };
        let _ = ended.send(reason);
    }

    /// Ends the stream, and the app too when asked. The app goes after the
    /// disconnect: the host won't take it for a lost client.
    async fn stop(&mut self, quit_app: bool) -> Ended {
        self.media.stop();
        // The feedback channel closes when the control task has said goodbye.
        let _ = tokio::time::timeout(GOODBYE, async {
            while self.feedback.recv().await.is_some() {}
        })
        .await;
        if quit_app {
            match self.host.cancel().await {
                Ok(()) => info!("app closed on the host"),
                Err(err) => warn!("closing the app on the host: {err}"),
            }
        }
        Ended::Stopped
    }
}

fn map_ended(result: Result<media::Ended, oneshot::error::RecvError>) -> Ended {
    match result {
        Ok(media::Ended::Stopped) => Ended::Stopped,
        Ok(media::Ended::Terminated { graceful: true, .. }) => {
            Ended::ByHost("the host closed the stream".into())
        }
        Ok(media::Ended::Terminated { code, .. }) => {
            Ended::ByHost(format!("the host ended the stream (status {code:#010x})"))
        }
        Ok(media::Ended::Failed(why)) => Ended::Failed(format!("the host's stream failed: {why}")),
        Err(_) => Ended::Failed("the stream's task ended unexpectedly".into()),
    }
}

/// What a host's control message says to a controller, if it does.
fn feedback_of(event: &cha_gamestream::Feedback) -> Option<Feedback> {
    match *event {
        cha_gamestream::Feedback::Rumble { pad, low, high } => {
            let (low, high) = rumble_levels(low, high);
            Some(Feedback::Rumble {
                index: u8::try_from(pad).ok()?,
                low,
                high,
            })
        }
        cha_gamestream::Feedback::Led {
            pad,
            rgb: (r, g, b),
        } => Some(Feedback::Led {
            index: u8::try_from(pad).ok()?,
            r,
            g,
            b,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codecs_are_the_players_choices_the_host_can_encode() {
        let both = [VideoCodec::Hevc, VideoCodec::H264];
        assert_eq!(
            offered(&both, &[Codec::Av1, Codec::Hevc, Codec::H264]).unwrap(),
            [VideoCodec::Hevc, VideoCodec::H264]
        );
        assert_eq!(
            offered(&both, &[Codec::H264, Codec::Hevc]).unwrap(),
            [VideoCodec::H264, VideoCodec::Hevc]
        );
        let all = [VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264];
        assert_eq!(
            offered(&all, &[Codec::Av1, Codec::H264]).unwrap(),
            [VideoCodec::Av1, VideoCodec::H264]
        );
        let old = [VideoCodec::H264];
        assert_eq!(
            offered(&old, &[Codec::Hevc, Codec::H264]).unwrap(),
            [VideoCodec::H264]
        );
        assert!(offered(&old, &[Codec::Hevc]).is_err());
        assert!(offered(&both, &[Codec::Av1]).is_err());
        assert!(offered(&both, &[]).is_err());
        assert!(offered(&[], &[Codec::H264]).is_err());
        // PyroWave is never asked of a Moonlight host.
        assert_eq!(
            offered(
                &both,
                &[Codec::PyroWave420, Codec::PyroWave444, Codec::Hevc]
            )
            .unwrap(),
            [VideoCodec::Hevc]
        );
        assert!(offered(&both, &[Codec::PyroWave444]).is_err());
    }

    #[test]
    fn media_stats_become_transport_stats() {
        let mut stats = MediaStats::default();
        // Before the control task has read the round trip, there is none.
        let t = transport_stats_of(&stats, 60);
        assert_eq!(t.tag, "GS");
        assert_eq!((t.rtt_ms, t.lost, t.recovered), (None, 0, 0));
        assert_eq!(t.target_fps, Some(60));
        assert!(t.node.is_none() && t.sent_fps.is_none() && t.encode_p99_ms.is_none());
        // No floor, viewers or overlay: nothing for the toolbar to offer.
        assert!(t.control.is_none() && t.viewers.is_none() && t.overlay.is_none());

        stats.rtt_ms = Some(3);
        stats.video.frames_lost = 5;
        stats.video.frames_recovered = 7;
        // Blocks and discarded frames are not what the panel counts.
        stats.video.blocks_recovered = 9;
        stats.video.frames_discarded = 2;
        let t = transport_stats_of(&stats, 0);
        assert_eq!(t.rtt_ms, Some(3.0));
        assert_eq!((t.lost, t.recovered), (5, 7));
        assert_eq!(t.target_fps, None);
    }

    #[test]
    fn rumble_and_leds_become_feedback() {
        let rumble = cha_gamestream::Feedback::Rumble {
            pad: 2,
            low: u16::MAX,
            high: 0,
        };
        assert_eq!(
            feedback_of(&rumble),
            Some(Feedback::Rumble {
                index: 2,
                low: 1.0,
                high: 0.0
            })
        );
        let led = cha_gamestream::Feedback::Led {
            pad: 1,
            rgb: (1, 2, 3),
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
        let motion = cha_gamestream::Feedback::MotionEnable {
            pad: 0,
            rate_hz: 100,
            kind: 1,
        };
        assert_eq!(feedback_of(&motion), None);
    }

    #[test]
    fn the_hosts_endings_become_the_players() {
        use media::Ended as M;
        let ok = |m| map_ended(Ok(m));
        assert_eq!(ok(M::Stopped), Ended::Stopped);
        assert_eq!(
            ok(M::Terminated {
                code: 0x8003_0023,
                graceful: true
            }),
            Ended::ByHost("the host closed the stream".into())
        );
        let refused = ok(M::Terminated {
            code: 0x8003_0022,
            graceful: false,
        });
        assert!(matches!(refused, Ended::ByHost(m) if m.contains("0x80030022")));
        assert!(matches!(ok(M::Failed("x".into())), Ended::Failed(m) if m.contains('x')));
    }
}
