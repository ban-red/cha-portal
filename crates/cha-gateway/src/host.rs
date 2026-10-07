//! The Moonlight side: the node's paired client of one GameStream host
//! (Sunshine or Apollo) and the one stream it holds open for the environment's
//! life. WebRTC viewers come and go; they subscribe to what this publishes and
//! send input and keyframe requests back.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use moonlight_common::AppId;
use moonlight_common::crypto::rustcrypto::RustCryptoBackend;
use moonlight_common::high::tokio::MoonlightHost;
use moonlight_common::http::client::tokio_hyper::TokioHyperClient;
use moonlight_common::http::{ClientIdentifier, ClientSecret, ServerIdentifier};
use moonlight_common::stream::audio::AudioConfig;
use moonlight_common::stream::control::ActiveGamepads;
use moonlight_common::stream::proto::MoonlightStreamSetup;
use moonlight_common::stream::proto::audio::AudioStreamEvent;
use moonlight_common::stream::proto::control::ControlStreamEvent;
use moonlight_common::stream::proto::control::input_batcher::ClientInputEvent;
use moonlight_common::stream::proto::control::packet::ControlPacket;
use moonlight_common::stream::proto::video::VideoStreamEvent;
use moonlight_common::stream::tokio::{MoonlightStream, MoonlightStreamEvent};
use moonlight_common::stream::video::{
    ColorRange, ColorSpace, ServerCodecModeSupport, VideoCapabilities, VideoFormat, VideoFormats,
};
use moonlight_common::stream::{
    AesIv, AesKey, EncryptionFlags, MoonlightStreamSettings, StreamingConfig,
};
use pem::Pem;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{info, warn};

/// How long to wait for the host's HTTP API at startup (the PC may be waking).
const HOST_WAIT: Duration = Duration::from_secs(90);

/// Frames and packets the viewers may fall behind by before they skip ahead
/// (to the next keyframe, for video).
const VIDEO_BACKLOG: usize = 64;
const AUDIO_BACKLOG: usize = 256;

/// The name this client gives the host in its requests (the host knows it by
/// certificate).
const CLIENT_NAME: &str = "cha-gateway";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    /// The name the page and `/webrtc/media?name=live-<codec>` use.
    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
        }
    }

    fn matches(self, format: VideoFormat) -> bool {
        format.contained_in(match self {
            Codec::H264 => VideoFormats::MASK_H264,
            Codec::Hevc => VideoFormats::MASK_H265,
        })
    }

    fn formats(self) -> VideoFormats {
        match self {
            Codec::H264 => VideoFormats::H264,
            Codec::Hevc => VideoFormats::H265,
        }
    }
}

/// Which codec to ask the host for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum CodecChoice {
    /// HEVC if the host can, else H.264.
    Auto,
    H264,
    Hevc,
}

impl CodecChoice {
    /// The codec to launch with, given what the host says it can encode.
    pub fn pick(self, host: ServerCodecModeSupport) -> Result<Codec> {
        let hevc = host.intersects(ServerCodecModeSupport::HEVC);
        let h264 = host.intersects(ServerCodecModeSupport::H264);
        match self {
            CodecChoice::Auto if hevc => Ok(Codec::Hevc),
            CodecChoice::Auto | CodecChoice::H264 if h264 => Ok(Codec::H264),
            CodecChoice::Hevc if hevc => Ok(Codec::Hevc),
            CodecChoice::Hevc => bail!("the host can't encode HEVC"),
            _ => bail!("the host can't encode H.264"),
        }
    }
}

/// One encoded frame from the host, as received.
pub struct HostFrame {
    /// The whole access unit (Annex-B).
    pub data: Bytes,
    pub key: bool,
    /// The host's frame number; gaps are frames lost between host and gateway.
    #[allow(dead_code)]
    pub index: u32,
    /// When the gateway had the whole frame.
    pub received: Instant,
}

/// One Opus packet from the host (stereo, one stream, passed through).
pub struct HostAudio {
    pub data: Bytes,
    /// The host's clock, in milliseconds since its first packet.
    pub timestamp: Duration,
}

/// Rumble the host's game asked for on pad `pad`, motors 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostRumble {
    pub pad: usize,
    pub lo: f32,
    pub hi: f32,
}

/// What the live stream is, once it is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Whether the host's audio is the stereo single-stream Opus the browser
    /// can play as is.
    pub audio: bool,
}

/// What a viewer asks of the host stream.
#[derive(Debug)]
pub enum Command {
    Input(ClientInputEvent),
    /// Send a new IDR frame.
    Idr,
}

/// The viewers' end: subscribe to what the host sends, send it what they do.
#[derive(Clone)]
pub struct Link {
    frames: broadcast::Sender<Arc<HostFrame>>,
    audio: broadcast::Sender<Arc<HostAudio>>,
    rumble: broadcast::Sender<HostRumble>,
    commands: mpsc::UnboundedSender<Command>,
    info: watch::Receiver<Option<StreamInfo>>,
}

/// The stream task's end of a [`Link`].
pub struct Plumbing {
    frames: broadcast::Sender<Arc<HostFrame>>,
    audio: broadcast::Sender<Arc<HostAudio>>,
    rumble: broadcast::Sender<HostRumble>,
    commands: mpsc::UnboundedReceiver<Command>,
    info: watch::Sender<Option<StreamInfo>>,
}

pub fn link() -> (Link, Plumbing) {
    let (frames, _) = broadcast::channel(VIDEO_BACKLOG);
    let (audio, _) = broadcast::channel(AUDIO_BACKLOG);
    let (rumble, _) = broadcast::channel(32);
    let (commands_tx, commands) = mpsc::unbounded_channel();
    let (info_tx, info) = watch::channel(None);
    (
        Link {
            frames: frames.clone(),
            audio: audio.clone(),
            rumble: rumble.clone(),
            commands: commands_tx,
            info,
        },
        Plumbing {
            frames,
            audio,
            rumble,
            commands,
            info: info_tx,
        },
    )
}

impl Link {
    pub fn frames(&self) -> broadcast::Receiver<Arc<HostFrame>> {
        self.frames.subscribe()
    }

    pub fn audio(&self) -> broadcast::Receiver<Arc<HostAudio>> {
        self.audio.subscribe()
    }

    pub fn rumble(&self) -> broadcast::Receiver<HostRumble> {
        self.rumble.subscribe()
    }

    pub fn send(&self, command: Command) {
        // The task is gone only when the gateway is going down.
        let _ = self.commands.send(command);
    }

    pub fn input(&self, events: impl IntoIterator<Item = ClientInputEvent>) {
        for event in events {
            self.send(Command::Input(event));
        }
    }

    pub fn request_idr(&self) {
        self.send(Command::Idr);
    }

    /// The stream, if it is up now.
    pub fn info(&self) -> Option<StreamInfo> {
        *self.info.borrow()
    }

    /// The stream once it is up, for at most `within`.
    pub async fn wait_for_info(&self, within: Duration) -> Option<StreamInfo> {
        let mut info = self.info.clone();
        tokio::time::timeout(within, info.wait_for(Option::is_some))
            .await
            .ok()?
            .ok()
            .and_then(|seen| *seen)
    }
}

/// What the gateway needs to reach and stream from the host.
pub struct HostConfig {
    pub address: String,
    pub http_port: u16,
    pub https_port: u16,
    pub app_id: u32,
    pub identity_dir: PathBuf,
    pub unique_id: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub codec: CodecChoice,
}

/// The node's Moonlight identity and the host's certificate, from the
/// identity directory.
pub struct Identity {
    pub client: ClientIdentifier,
    pub secret: ClientSecret,
    pub server: ServerIdentifier,
}

/// Reads `client-cert.pem`, `client-key.pem` and
/// `hosts/<unique id>/server-cert.pem`; the error says what's missing and
/// what that means (the host isn't paired).
pub fn load_identity(dir: &Path, unique_id: &str) -> Result<Identity> {
    let files = [
        dir.join("client-cert.pem"),
        dir.join("client-key.pem"),
        dir.join("hosts").join(unique_id).join("server-cert.pem"),
    ];
    let missing: Vec<String> = files
        .iter()
        .filter(|p| !p.is_file())
        .map(|p| p.display().to_string())
        .collect();
    if !missing.is_empty() {
        bail!(
            "the host isn't paired with this node: missing {}",
            missing.join(", ")
        );
    }
    let [client, secret, server] = files.map(|p| {
        std::fs::read_to_string(&p)
            .with_context(|| format!("reading {}", p.display()))
            .and_then(|s| Pem::from_str(&s).with_context(|| format!("parsing {}", p.display())))
    });
    Ok(Identity {
        client: ClientIdentifier::from_pem(client?),
        secret: ClientSecret::from_pem(secret?),
        server: ServerIdentifier::from_pem(server?),
    })
}

/// A bitrate for the picture size: about 0.18 bit per pixel, 8 to 100 Mbit/s
/// (40 for 1440p60).
pub fn auto_bitrate_kbps(width: u32, height: u32, fps: u32) -> u32 {
    let bits_per_second = f64::from(width) * f64::from(height) * f64::from(fps) * 0.18;
    ((bits_per_second / 1000.0) as u32).clamp(8_000, 100_000)
}

/// Connects, launches the app and relays the stream until `stop` fires or the
/// host's stream ends. Quits the app on the host on the way out. An error is
/// an unrecoverable stream (or no stream to start).
pub async fn run(
    config: HostConfig,
    mut plumbing: Plumbing,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let identity = load_identity(&config.identity_dir, &config.unique_id)?;
    let crypto = RustCryptoBackend;
    let host = MoonlightHost::<TokioHyperClient>::new(
        config.address.clone(),
        config.http_port,
        Some(CLIENT_NAME.to_string()),
    )
    .map_err(|err| anyhow!("creating the Moonlight client: {err:?}"))?;

    // The host may be starting or waking.
    let waiting_since = Instant::now();
    let info = loop {
        match host.server_info().await {
            Ok(info) => break info,
            Err(err) if waiting_since.elapsed() > HOST_WAIT => {
                bail!(
                    "host {}:{} not answering after {HOST_WAIT:?}: {err:?}",
                    config.address,
                    config.http_port
                );
            }
            Err(_) => {}
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            _ = stop.changed() => return Ok(()),
        }
    };
    if info.https_port != config.https_port {
        warn!(
            host = info.https_port,
            expected = config.https_port,
            "the host's HTTPS port isn't the one given; using the host's"
        );
    }
    info!(name = %info.host_name, version = ?info.app_version, "host found");

    host.set_identity(identity.client, identity.secret, identity.server)
        .await
        .map_err(|err| anyhow!("loading the node's identity: {err:?}"))?;
    if !host
        .is_paired()
        .await
        .map_err(|err| anyhow!("asking the host whether it is paired: {err:?}"))?
    {
        bail!("the host doesn't know this node as paired; pair it again from the portal");
    }

    let outcome = stream(&config, &host, &crypto, &mut plumbing, &mut stop).await;
    // Whatever happened, quit the app: the next launch starts fresh.
    plumbing.info.send_replace(None);
    match host.cancel().await {
        Ok(_) => info!("app closed on the host"),
        Err(err) => warn!("closing the app on the host: {err:?}"),
    }
    outcome
}

async fn stream(
    config: &HostConfig,
    host: &MoonlightHost<TokioHyperClient>,
    crypto: &RustCryptoBackend,
    plumbing: &mut Plumbing,
    stop: &mut watch::Receiver<bool>,
) -> Result<()> {
    let version = host.version().await.map_err(|e| anyhow!("{e:?}"))?;
    let gfe = host.gfe_version().await.map_err(|e| anyhow!("{e:?}"))?;
    let modes = host
        .server_codec_mode_support()
        .await
        .map_err(|e| anyhow!("{e:?}"))?;
    let codec = config.codec.pick(modes)?;
    let mut settings = MoonlightStreamSettings {
        width: config.width,
        height: config.height,
        fps: config.fps,
        fps_x100: config.fps * 100,
        bitrate: config.bitrate_kbps,
        packet_size: 1392,
        // moonlight-common-rust can't decrypt video; node and host share a LAN
        // and the browser leg is WebRTC's DTLS.
        encryption_flags: EncryptionFlags::empty(),
        streaming_remotely: StreamingConfig::Local,
        sops: false,
        hdr: false,
        supported_video_formats: codec.formats(),
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
        .map_err(|e| anyhow!("host can't stream {codec:?}: {e:?}"))?;

    // The host runs one app at a time. Resume ours if it is the one running
    // (a node restart, say); anything else running goes first.
    host.update().await.map_err(|e| anyhow!("{e:?}"))?;
    let running = host.current_game().await.map_err(|e| anyhow!("{e:?}"))?;
    if running != 0 && running != config.app_id {
        info!(running, "another app runs on the host: closing it first");
        host.cancel().await.map_err(|e| anyhow!("{e:?}"))?;
    }

    let started = Instant::now();
    let stream_config = host
        .start_stream(
            AppId(config.app_id),
            &settings,
            AesKey::new_random(crypto).map_err(|e| anyhow!("{e:?}"))?,
            AesIv::new_random(crypto).map_err(|e| anyhow!("{e:?}"))?,
            MoonlightStreamSetup::launch_query_parameters(),
        )
        .await
        .map_err(|e| anyhow!("launching app {}: {e:?}", config.app_id))?;
    let mut stream = MoonlightStream::connect(
        stream_config,
        settings,
        Arc::new(crypto.clone()),
        VideoCapabilities::default(),
    )
    .await
    .map_err(|e| anyhow!("connecting the stream: {e:?}"))?;
    let setup = stream.video_setup();
    let audio_setup = stream.audio_setup();
    info!(
        ?setup,
        ?audio_setup,
        launch_ms = started.elapsed().as_millis(),
        "moonlight stream up"
    );
    if !codec.matches(setup.format) {
        close(&mut stream).await;
        bail!(
            "asked for {codec:?}, but the stream came up as {:?}",
            setup.format
        );
    }
    // The browser plays one Opus stream of two channels. A surround mix
    // (multistream) would be garbage to it: no audio, then.
    let audio = audio_setup.channel_count == 2
        && audio_setup.streams == 1
        && audio_setup.coupled_streams == 1;
    if !audio {
        warn!(
            channels = audio_setup.channel_count,
            streams = audio_setup.streams,
            "the host's audio isn't stereo single-stream Opus: sending no audio"
        );
    }
    plumbing.info.send_replace(Some(StreamInfo {
        codec,
        width: config.width,
        height: config.height,
        fps: config.fps,
        audio,
    }));

    let mut first_frame = true;
    let mut frames_sent = 0u64;
    let result: Result<()> = loop {
        tokio::select! {
            _ = stop.changed() => break Ok(()),
            command = plumbing.commands.recv() => match command {
                Some(Command::Input(event)) => {
                    if let Err(err) = stream.send_input(event) {
                        warn!("sending input to the host: {err:?}");
                    }
                }
                Some(Command::Idr) => {
                    if let Err(err) = stream.send_raw(ControlPacket::RequestIdr) {
                        warn!("requesting an IDR: {err:?}");
                    }
                }
                // Every Link is gone: the gateway is going down.
                None => break Ok(()),
            },
            event = stream.drive() => match event {
                // One frame lost in reassembly (its FEC block couldn't be
                // recovered): a keyframe repairs it; don't end the stream.
                Err(err) if lost_frame(&err) => {
                    warn!("a video frame couldn't be reassembled; asking for a keyframe: {err:?}");
                    let _ = stream.send_raw(ControlPacket::RequestIdr);
                }
                Err(err) => break Err(anyhow!("the host's stream failed: {err:?}")),
                Ok(MoonlightStreamEvent::Video(VideoStreamEvent::OnFrame(frame))) => {
                    let received = Instant::now();
                    let md = frame.metadata();
                    // The type is in the packet header; `frame.as_ref()` would
                    // scan the whole access unit for NAL units. It isn't
                    // exported, hence Debug.
                    let key = format!("{:?}", md.frame_type) == "Idr";
                    if first_frame {
                        first_frame = false;
                        info!(after_ms = started.elapsed().as_millis(), "first frame from the host");
                    }
                    frames_sent += 1;
                    let frame = Arc::new(HostFrame {
                        data: Bytes::copy_from_slice(frame.raw()),
                        key,
                        index: md.frame_index.0,
                        received,
                    });
                    // Nobody watching is fine; the host stream runs on.
                    if plumbing.frames.send(frame).is_ok() {
                        // Let the viewers' tasks run now: tokio parks a task
                        // woken from this worker in its LIFO slot until we
                        // yield, and we'd otherwise drain this frame's FEC
                        // packets first.
                        tokio::task::yield_now().await;
                    }
                }
                Ok(MoonlightStreamEvent::Video(VideoStreamEvent::SignalIdr)) => {
                    let _ = stream.send_raw(ControlPacket::RequestIdr);
                }
                Ok(MoonlightStreamEvent::Audio(AudioStreamEvent::OnFrame(frame))) => {
                    if audio {
                        let _ = plumbing.audio.send(Arc::new(HostAudio {
                            data: frame.buffer,
                            timestamp: frame.timestamp,
                        }));
                    }
                }
                Ok(MoonlightStreamEvent::Control(ControlStreamEvent::Packet(
                    ControlPacket::ControllerRumbleData {
                        controller_number,
                        low_frequency,
                        high_frequency,
                        ..
                    },
                ))) => {
                    let (lo, hi) = cha_moonlight_input::rumble_levels(low_frequency, high_frequency);
                    let _ = plumbing.rumble.send(HostRumble {
                        pad: usize::from(controller_number),
                        lo,
                        hi,
                    });
                }
                Ok(MoonlightStreamEvent::Control(ControlStreamEvent::Disconnect)) => {
                    break Err(anyhow!("the host closed the stream"));
                }
                Ok(_) => {}
            },
        }
    };
    info!(frames_sent, "leaving the host stream");
    close(&mut stream).await;
    result
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

/// Whether a stream error is only one video frame lost in reassembly.
fn lost_frame(err: &moonlight_common::error::Error) -> bool {
    use moonlight_common::stream::proto::video::depayloader::VideoDepayloaderError;
    matches!(err, moonlight_common::error::Error::Other(inner) if inner.downcast_ref::<VideoDepayloaderError>().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_choice_prefers_hevc_when_the_host_can() {
        let both = ServerCodecModeSupport::H264 | ServerCodecModeSupport::HEVC;
        assert_eq!(CodecChoice::Auto.pick(both).unwrap(), Codec::Hevc);
        assert_eq!(CodecChoice::H264.pick(both).unwrap(), Codec::H264);
        assert_eq!(CodecChoice::Hevc.pick(both).unwrap(), Codec::Hevc);
        let old = ServerCodecModeSupport::H264;
        assert_eq!(CodecChoice::Auto.pick(old).unwrap(), Codec::H264);
        assert!(CodecChoice::Hevc.pick(old).is_err());
        let none = ServerCodecModeSupport::empty();
        assert!(CodecChoice::Auto.pick(none).is_err());
    }

    #[test]
    fn bitrate_follows_the_picture() {
        assert_eq!(auto_bitrate_kbps(2560, 1440, 60), 39_813);
        assert_eq!(auto_bitrate_kbps(640, 360, 30), 8_000);
        assert_eq!(auto_bitrate_kbps(7680, 4320, 120), 100_000);
    }

    #[test]
    fn missing_identity_says_the_host_isnt_paired() {
        let dir = std::env::temp_dir().join(format!("cha-gateway-test-{}", std::process::id()));
        let err = load_identity(&dir, "ABC").err().expect("no files");
        let text = err.to_string();
        assert!(text.contains("isn't paired"), "{text}");
        assert!(text.contains("client-cert.pem"), "{text}");
        assert!(text.contains("server-cert.pem"), "{text}");
    }

    #[tokio::test]
    async fn a_link_waits_for_the_stream_to_come_up() {
        let (link, plumbing) = link();
        assert_eq!(link.info(), None);
        assert_eq!(link.wait_for_info(Duration::from_millis(20)).await, None);
        let up = StreamInfo {
            codec: Codec::H264,
            width: 1280,
            height: 720,
            fps: 60,
            audio: true,
        };
        plumbing.info.send_replace(Some(up));
        assert_eq!(
            link.wait_for_info(Duration::from_millis(20)).await,
            Some(up)
        );
    }
}
