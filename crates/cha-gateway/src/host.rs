//! The Moonlight side: the node's paired client of one GameStream host
//! (Sunshine or Apollo) and the one stream it holds open for the environment's
//! life. WebRTC viewers come and go; they subscribe to what this publishes and
//! send input and keyframe requests back.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use cha_gamestream::client::front::{ClientIdentity, Encrypt, HostClient, StreamRequest};
use cha_gamestream::client::media::{Ended, Media, MediaClient, MediaOptions};
use cha_gamestream::client::{AudioPacket, VideoFrame};
use cha_gamestream::{Feedback, InputEvent, VideoCodec};
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

    fn wire(self) -> VideoCodec {
        match self {
            Codec::H264 => VideoCodec::H264,
            Codec::Hevc => VideoCodec::Hevc,
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
    pub fn pick(self, host: &[VideoCodec]) -> Result<Codec> {
        let hevc = host.contains(&VideoCodec::Hevc);
        let h264 = host.contains(&VideoCodec::H264);
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
    /// The host's clock, since its first packet.
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
    Input(InputEvent),
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

    pub fn input(&self, events: impl IntoIterator<Item = InputEvent>) {
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
    pub client: ClientIdentity,
    /// The host's certificate, PEM.
    pub server: String,
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
    let [cert, key, server] = files
        .map(|p| std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display())));
    let (cert, key) = (cert?, key?);
    Ok(Identity {
        client: ClientIdentity::from_pem(&cert, &key)
            .map_err(|e| anyhow!("the node's client certificate or key isn't usable: {e}"))?,
        server: server?,
    })
}

/// A bitrate for the picture size: about 0.18 bit per pixel, 8 to 100 Mbit/s
/// (40 for 1440p60).
pub fn auto_bitrate_kbps(width: u32, height: u32, fps: u32) -> u32 {
    let bits_per_second = f64::from(width) * f64::from(height) * f64::from(fps) * 0.18;
    ((bits_per_second / 1000.0) as u32).clamp(8_000, 100_000)
}

/// `host:port` as the client takes it (an IPv6 address needs its brackets).
fn host_port(address: &str, port: u16) -> String {
    if address.contains(':') && !address.starts_with('[') {
        format!("[{address}]:{port}")
    } else {
        format!("{address}:{port}")
    }
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
    let host = HostClient::new(
        &host_port(&config.address, config.http_port),
        identity.client,
    )
    .map_err(|err| anyhow!("creating the Moonlight client: {err}"))?
    .with_unique_id(CLIENT_NAME)
    .with_server_cert(&identity.server)
    .map_err(|err| anyhow!("loading the host's certificate: {err}"))?;

    // The host may be starting or waking. Asked over HTTPS (we hold its
    // certificate), it also says whether it knows this node.
    let waiting_since = Instant::now();
    let info = loop {
        match host.server_info().await {
            Ok(info) => break info,
            Err(err) if waiting_since.elapsed() > HOST_WAIT => {
                bail!(
                    "host {}:{} not answering after {HOST_WAIT:?}: {err}",
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
    info!(name = %info.name, version = %info.app_version, "host found");
    if !info.paired {
        bail!("the host doesn't know this node as paired; pair it again from the portal");
    }

    let outcome = stream(&config, &host, &mut plumbing, &mut stop).await;
    // Whatever happened, quit the app: the next launch starts fresh.
    plumbing.info.send_replace(None);
    match host.cancel().await {
        Ok(()) => info!("app closed on the host"),
        Err(err) => warn!("closing the app on the host: {err}"),
    }
    outcome
}

async fn stream(
    config: &HostConfig,
    host: &HostClient,
    plumbing: &mut Plumbing,
    stop: &mut watch::Receiver<bool>,
) -> Result<()> {
    // Fresh from the host: what it can encode, and what it runs now.
    let info = host
        .server_info()
        .await
        .map_err(|e| anyhow!("reading the host's state: {e}"))?;
    let codec = config.codec.pick(&info.codecs())?;

    // The host runs one app at a time. Resume ours if it is the one running
    // (a node restart, say); anything else running goes first.
    let running = info.current_game;
    if running != 0 && running != config.app_id {
        info!(running, "another app runs on the host: closing it first");
        host.cancel()
            .await
            .map_err(|e| anyhow!("closing app {running}: {e}"))?;
    }

    let mut request = StreamRequest::new(config.app_id, config.width, config.height, config.fps);
    request.bitrate_kbps = config.bitrate_kbps;
    request.codecs = vec![codec.wire()];
    request.audio_channels = 2;
    // The browser leg is WebRTC's DTLS; the leg to the host is encrypted
    // wherever the host can, so the whole path is.
    request.video_encryption = Encrypt::IfSupported;
    request.audio_encryption = Encrypt::IfSupported;

    let started = Instant::now();
    let setup = if running == config.app_id {
        host.resume(&request).await
    } else {
        host.launch(&request).await
    }
    .map_err(|e| anyhow!("launching app {}: {e}", config.app_id))?;
    let mut media = MediaClient::start_with(&setup, MediaOptions::default())
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
    if setup.codec != codec.wire() {
        close(&mut media).await;
        bail!(
            "asked for {codec:?}, but the stream came up as {:?}",
            setup.codec
        );
    }
    // The browser plays one Opus stream of two channels. A surround mix
    // (multistream) would be garbage to it: no audio, then.
    let audio =
        setup.audio.channels == 2 && setup.audio.streams == 1 && setup.audio.coupled_streams == 1;
    if !audio {
        warn!(
            channels = setup.audio.channels,
            streams = setup.audio.streams,
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
    let mut audio_clock = AudioClock::default();
    let result: Result<()> = loop {
        tokio::select! {
            _ = stop.changed() => break Ok(()),
            command = plumbing.commands.recv() => match command {
                Some(Command::Input(event)) => media.control.input(event),
                Some(Command::Idr) => media.control.request_idr(),
                // Every Link is gone: the gateway is going down.
                None => break Ok(()),
            },
            // What the network lost never gets here: the client drops the
            // frame, asks the host for a keyframe and resumes at it.
            Some(frame) = media.video.recv() => {
                if first_frame {
                    first_frame = false;
                    info!(after_ms = started.elapsed().as_millis(), "first frame from the host");
                }
                frames_sent += 1;
                // Nobody watching is fine; the host stream runs on.
                let _ = plumbing.frames.send(Arc::new(host_frame(frame)));
            }
            Some(packet) = media.audio.recv() => {
                if audio {
                    let _ = plumbing.audio.send(Arc::new(audio_clock.packet(packet)));
                }
            }
            Some(Feedback::Rumble { pad, low, high }) = media.feedback.recv() => {
                let (lo, hi) = cha_moonlight_input::rumble_levels(low, high);
                let _ = plumbing.rumble.send(HostRumble {
                    pad: usize::from(pad),
                    lo,
                    hi,
                });
            }
            ended = &mut media.ended => break Err(match ended {
                Ok(Ended::Terminated { graceful: true, .. }) => anyhow!("the host closed the stream"),
                Ok(Ended::Terminated { code, .. }) => {
                    anyhow!("the host ended the stream (status {code:#010x})")
                }
                Ok(Ended::Failed(why)) => anyhow!("the host's stream failed: {why}"),
                Ok(Ended::Stopped) | Err(_) => anyhow!("the host stream stopped"),
            }),
        }
    };
    info!(frames_sent, "leaving the host stream");
    close(&mut media).await;
    result
}

fn host_frame(frame: VideoFrame) -> HostFrame {
    HostFrame {
        data: frame.data,
        key: frame.key,
        index: frame.number,
        received: frame.received,
    }
}

/// Turns the host's wrapping 32-bit millisecond RTP clock into time since the
/// first packet.
#[derive(Default)]
struct AudioClock {
    first: Option<u32>,
}

impl AudioClock {
    fn packet(&mut self, packet: AudioPacket) -> HostAudio {
        let first = *self.first.get_or_insert(packet.timestamp);
        HostAudio {
            data: packet.data,
            timestamp: Duration::from_millis(u64::from(packet.timestamp.wrapping_sub(first))),
        }
    }
}

/// Stops the stream and waits for the client's goodbye to reach the host: a
/// peer left to time out (about 5 s) can pause the same client's next
/// session, and the app is quit only after. The feedback channel closes when
/// the control task is done.
async fn close(media: &mut Media) {
    media.control.stop();
    let said = tokio::time::timeout(Duration::from_secs(2), async {
        while media.feedback.recv().await.is_some() {}
    })
    .await;
    if said.is_err() {
        warn!("the host didn't acknowledge the disconnect within 2 s");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_choice_prefers_hevc_when_the_host_can() {
        let both = [VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264];
        assert_eq!(CodecChoice::Auto.pick(&both).unwrap(), Codec::Hevc);
        assert_eq!(CodecChoice::H264.pick(&both).unwrap(), Codec::H264);
        assert_eq!(CodecChoice::Hevc.pick(&both).unwrap(), Codec::Hevc);
        let old = [VideoCodec::H264];
        assert_eq!(CodecChoice::Auto.pick(&old).unwrap(), Codec::H264);
        assert!(CodecChoice::Hevc.pick(&old).is_err());
        // The browser can't play AV1 from here.
        assert!(CodecChoice::Auto.pick(&[VideoCodec::Av1]).is_err());
        assert!(CodecChoice::Auto.pick(&[]).is_err());
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

    #[test]
    fn the_nodes_identity_files_load_as_the_node_writes_them() {
        let dir = std::env::temp_dir().join(format!("cha-gateway-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("hosts/ABC")).unwrap();
        let client = ClientIdentity::generate().unwrap();
        let host = ClientIdentity::generate().unwrap();
        // The node's earlier library wrote CRLF line ends.
        let crlf = |pem: &str| pem.replace('\n', "\r\n");
        std::fs::write(dir.join("client-cert.pem"), crlf(client.cert_pem())).unwrap();
        std::fs::write(dir.join("client-key.pem"), crlf(client.key_pem())).unwrap();
        std::fs::write(dir.join("hosts/ABC/server-cert.pem"), crlf(host.cert_pem())).unwrap();
        let loaded = load_identity(&dir, "ABC").unwrap();
        assert_eq!(loaded.client.fingerprint(), client.fingerprint());
        HostClient::new("127.0.0.1", loaded.client)
            .unwrap()
            .with_server_cert(&loaded.server)
            .expect("the host's certificate pins");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn host_addresses_get_their_brackets() {
        assert_eq!(host_port("192.168.1.5", 47989), "192.168.1.5:47989");
        assert_eq!(host_port("pc.lan", 47989), "pc.lan:47989");
        assert_eq!(host_port("fe80::1", 47989), "[fe80::1]:47989");
        assert_eq!(host_port("[fe80::1]", 47989), "[fe80::1]:47989");
    }

    #[test]
    fn the_hosts_audio_clock_starts_at_its_first_packet_and_wraps() {
        let packet = |timestamp| AudioPacket {
            data: Bytes::from_static(b"x"),
            timestamp,
        };
        let mut clock = AudioClock::default();
        assert_eq!(clock.packet(packet(1000)).timestamp, Duration::ZERO);
        assert_eq!(
            clock.packet(packet(1005)).timestamp,
            Duration::from_millis(5)
        );
        let mut late = AudioClock::default();
        late.packet(packet(u32::MAX - 2));
        assert_eq!(late.packet(packet(2)).timestamp, Duration::from_millis(5));
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
