//! The Moonlight side: one paired client of a GameStream host (Wolf), and one
//! stream at a time whose frames go, untouched, to the WebRTC session.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use moonlight_common::App;
use moonlight_common::crypto::rustcrypto::RustCryptoBackend;
use moonlight_common::high::tokio::MoonlightHost;
use moonlight_common::http::client::tokio_hyper::TokioHyperClient;
use moonlight_common::http::pair::{PairPin, PairingCryptoBackend};
use moonlight_common::http::{ClientIdentifier, ClientSecret, ServerIdentifier};
use moonlight_common::stream::audio::AudioConfig;
use moonlight_common::stream::control::ActiveGamepads;
use moonlight_common::stream::proto::MoonlightStreamSetup;
use moonlight_common::stream::proto::control::ControlStreamEvent;
use moonlight_common::stream::proto::control::packet::ControlPacket;
use moonlight_common::stream::proto::video::VideoStreamEvent;
use moonlight_common::stream::tokio::{MoonlightStream, MoonlightStreamEvent};
use moonlight_common::stream::video::{
    ColorRange, ColorSpace, VideoCapabilities, VideoFormat, VideoFormats,
};
use moonlight_common::stream::{
    AesIv, AesKey, EncryptionFlags, MoonlightStreamSettings, StreamingConfig,
};
use pem::Pem;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{info, warn};

use crate::wolf;

/// How long to wait for the host's HTTP API at startup.
const HOST_WAIT: Duration = Duration::from_secs(90);

/// The codecs the gateway can pass through, as the page names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    /// Not offered until moonlight-common-rust negotiates AV1 (see `STREAMABLE`).
    #[allow(dead_code)]
    Av1,
}

impl Codec {
    /// What moonlight-common-rust can negotiate today. It never selects AV1
    /// ("Av1 is not supported in this implementation currently") and falls
    /// back to H.264, so AV1 waits for an upstream change.
    pub const STREAMABLE: [Codec; 2] = [Codec::H264, Codec::Hevc];

    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
            Codec::Av1 => "av1",
        }
    }

    fn matches(self, format: VideoFormat) -> bool {
        format.contained_in(match self {
            Codec::H264 => VideoFormats::MASK_H264,
            Codec::Hevc => VideoFormats::MASK_H265,
            Codec::Av1 => VideoFormats::MASK_AV1,
        })
    }

    fn formats(self) -> VideoFormats {
        match self {
            Codec::H264 => VideoFormats::H264,
            Codec::Hevc => VideoFormats::H265,
            Codec::Av1 => VideoFormats::AV1_MAIN8,
        }
    }
}

/// What the browser asked for.
#[derive(Clone, Debug)]
pub struct StreamRequest {
    pub app: App,
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

/// One encoded frame from the host, as received.
pub struct HostFrame {
    /// The whole access unit (Annex-B for H.264/HEVC, OBUs for AV1).
    pub data: Bytes,
    pub key: bool,
    /// The host's frame number; gaps are frames lost between host and gateway.
    pub index: u32,
    /// Capture to encoded on the host, when the host reports it.
    pub host_latency: Option<Duration>,
    /// When the gateway had the whole frame.
    pub received: Instant,
}

/// A running stream: frames out, keyframe requests in, dropped to stop.
pub struct StreamHandle {
    pub frames: mpsc::Receiver<HostFrame>,
    pub idr: mpsc::UnboundedSender<()>,
    _stop: oneshot::Sender<()>,
}

pub struct Gateway {
    host: MoonlightHost<TokioHyperClient>,
    crypto: RustCryptoBackend,
    /// The host's apps the gateway offers (see [`GatewayConfig::apps`]).
    pub apps: Vec<App>,
    /// One stream at a time: GameStream hosts run one session per client.
    busy: Arc<Mutex<()>>,
}

pub struct GatewayConfig {
    pub address: String,
    pub http_port: u16,
    /// Offer the apps whose title contains one of these (case-insensitive).
    pub apps: Vec<String>,
    pub state_dir: PathBuf,
    pub wolf_socket: Option<PathBuf>,
}

impl Gateway {
    pub async fn connect(config: GatewayConfig) -> Result<Arc<Self>> {
        let host = MoonlightHost::<TokioHyperClient>::new(
            config.address.clone(),
            config.http_port,
            Some("cha-s3-gateway".to_string()),
        )
        .map_err(|err| anyhow!("creating Moonlight client: {err:?}"))?;
        let crypto = RustCryptoBackend;

        // The host may still be starting (compose only waits for its container).
        let waiting_since = Instant::now();
        while let Err(err) = host.server_info().await {
            if waiting_since.elapsed() > HOST_WAIT {
                bail!(
                    "host {}:{} not answering: {err:?}",
                    config.address,
                    config.http_port
                );
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        match load_identity(&config.state_dir)? {
            Some((client, secret, server)) => {
                host.set_identity(client, secret, server)
                    .await
                    .map_err(|err| anyhow!("restoring identity: {err:?}"))?;
                info!("using the saved Moonlight identity");
            }
            None => pair(&host, &crypto, &config).await?,
        }

        let apps = host
            .app_list()
            .await
            .map_err(|err| anyhow!("listing apps (is the gateway paired?): {err:?}"))?;
        let titles: Vec<&str> = apps.iter().map(|a| a.title.as_str()).collect();
        info!(?titles, "host apps");
        let wanted: Vec<String> = config.apps.iter().map(|w| w.to_lowercase()).collect();
        let apps: Vec<App> = apps
            .iter()
            .filter(|a| wanted.iter().any(|w| a.title.to_lowercase().contains(w)))
            .cloned()
            .collect();
        if apps.is_empty() {
            bail!("no app matches {:?} in {titles:?}", config.apps);
        }
        info!(apps = ?apps.iter().map(|a| &a.title).collect::<Vec<_>>(), "offering these apps");
        Ok(Arc::new(Self {
            host,
            crypto,
            apps,
            busy: Arc::new(Mutex::new(())),
        }))
    }

    /// Launches the app and starts forwarding its frames. The stream stops
    /// when the handle is dropped.
    pub fn start(self: &Arc<Self>, request: StreamRequest) -> Result<StreamHandle> {
        let guard = Arc::clone(&self.busy)
            .try_lock_owned()
            .map_err(|_| anyhow!("a stream is already running"))?;
        let (frames_tx, frames) = mpsc::channel(32);
        let (idr, idr_rx) = mpsc::unbounded_channel();
        let (stop, stop_rx) = oneshot::channel();
        let gateway = Arc::clone(self);
        tokio::spawn(async move {
            let _guard = guard;
            if let Err(err) = gateway.run(request, frames_tx, idr_rx, stop_rx).await {
                warn!("moonlight stream ended: {err:#}");
            }
            // Quit the app so the next run launches fresh, possibly with another codec.
            match gateway.host.cancel().await {
                Ok(_) => info!("app closed on the host"),
                Err(err) => warn!("closing the app: {err:?}"),
            }
        });
        Ok(StreamHandle {
            frames,
            idr,
            _stop: stop,
        })
    }

    async fn run(
        &self,
        request: StreamRequest,
        frames: mpsc::Sender<HostFrame>,
        mut idr_rx: mpsc::UnboundedReceiver<()>,
        mut stop_rx: oneshot::Receiver<()>,
    ) -> Result<()> {
        let mut settings = MoonlightStreamSettings {
            width: request.width,
            height: request.height,
            fps: request.fps,
            fps_x100: request.fps * 100,
            bitrate: request.bitrate_kbps,
            packet_size: 1392,
            // moonlight-common-rust can't decrypt video, and on a loopback hop there's nothing to hide.
            encryption_flags: EncryptionFlags::empty(),
            streaming_remotely: StreamingConfig::Local,
            sops: false,
            hdr: false,
            supported_video_formats: request.codec.formats(),
            color_space: ColorSpace::Rec709,
            color_range: ColorRange::Limited,
            local_audio_play_mode: false,
            audio_config: AudioConfig::STEREO,
            gamepads_attached: ActiveGamepads::empty(),
            gamepads_persist_after_disconnect: false,
            enable_mic: false,
        };
        let version = self.host.version().await.map_err(|e| anyhow!("{e:?}"))?;
        let gfe = self
            .host
            .gfe_version()
            .await
            .map_err(|e| anyhow!("{e:?}"))?;
        let modes = self
            .host
            .server_codec_mode_support()
            .await
            .map_err(|e| anyhow!("{e:?}"))?;
        settings
            .adjust_for_server(version, &gfe, modes)
            .map_err(|e| anyhow!("host can't stream {:?}: {e:?}", request.codec))?;

        let started = Instant::now();
        let config = self
            .host
            .start_stream(
                request.app.id,
                &settings,
                AesKey::new_random(&self.crypto).map_err(|e| anyhow!("{e:?}"))?,
                AesIv::new_random(&self.crypto).map_err(|e| anyhow!("{e:?}"))?,
                MoonlightStreamSetup::launch_query_parameters(),
            )
            .await
            .map_err(|e| anyhow!("launching {}: {e:?}", request.app.title))?;
        let mut stream = MoonlightStream::connect(
            config,
            settings,
            Arc::new(self.crypto.clone()),
            VideoCapabilities::default(),
        )
        .await
        .map_err(|e| anyhow!("connecting the stream: {e:?}"))?;
        let setup = stream.video_setup();
        info!(
            ?setup,
            launch_ms = started.elapsed().as_millis(),
            "moonlight stream up"
        );
        // The browser track was negotiated for the requested codec; anything else can't decode.
        if !request.codec.matches(setup.format) {
            close(&mut stream).await;
            bail!(
                "asked for {:?}, but the stream came up as {:?}",
                request.codec,
                setup.format
            );
        }

        let mut first_frame = true;
        let mut dropped = 0u64;
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                Some(()) = idr_rx.recv() => {
                    if let Err(err) = stream.send_raw(ControlPacket::RequestIdr) {
                        warn!("requesting an IDR: {err:?}");
                    }
                }
                event = stream.drive() => match event.map_err(|e| anyhow!("stream: {e:?}"))? {
                    MoonlightStreamEvent::Video(VideoStreamEvent::OnFrame(frame)) => {
                        let received = Instant::now();
                        let md = frame.metadata();
                        // The host's frame type, from the packet header. `frame.as_ref()` would
                        // re-scan the whole access unit byte by byte for NAL units (~5 ns/byte,
                        // ~0.4 ms on an 80 KB frame). Its type isn't exported, hence Debug.
                        let key = format!("{:?}", md.frame_type) == "Idr";
                        if first_frame {
                            first_frame = false;
                            info!(after_ms = started.elapsed().as_millis(), "first frame from the host");
                        }
                        let frame = HostFrame {
                            data: Bytes::copy_from_slice(frame.raw()),
                            key,
                            index: md.frame_index.0,
                            host_latency: md.host_processing_latency,
                            received,
                        };
                        // Never block the host stream on the browser side; count what doesn't fit.
                        if frames.try_send(frame).is_err() {
                            dropped += 1;
                            if frames.is_closed() {
                                break;
                            }
                        } else {
                            // Let the WebRTC task run now. Tokio puts a task woken from this
                            // worker in its LIFO slot, where it waits until we yield, and we
                            // would otherwise go on to drain this frame's FEC packets first.
                            tokio::task::yield_now().await;
                        }
                    }
                    MoonlightStreamEvent::Video(VideoStreamEvent::SignalIdr) => {
                        let _ = stream.send_raw(ControlPacket::RequestIdr);
                    }
                    MoonlightStreamEvent::Control(ControlStreamEvent::Disconnect) => {
                        info!("host closed the control stream");
                        break;
                    }
                    _ => {}
                },
            }
        }
        if dropped > 0 {
            warn!(
                dropped,
                "frames dropped between the host and the WebRTC session"
            );
        }
        close(&mut stream).await;
        Ok(())
    }
}

/// Disconnects and keeps driving the stream until the ENet disconnect is
/// actually sent. `disconnect()` only queues it. Wolf gives every session from
/// one client the same id, so a peer left to time out (~5 s later) pauses the
/// client's *next* session.
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

async fn pair(
    host: &MoonlightHost<TokioHyperClient>,
    crypto: &RustCryptoBackend,
    config: &GatewayConfig,
) -> Result<()> {
    let (client, secret) = crypto
        .generate_client_identity()
        .map_err(|e| anyhow!("generating a client identity: {e:?}"))?;
    let pin = PairPin::new_random(crypto).map_err(|e| anyhow!("{e:?}"))?;
    let pin_text = pin.to_string();
    let pairing = host.pair(
        &client,
        &secret,
        "cha-s3-gateway".into(),
        pin,
        crypto.clone(),
    );
    let pairing = match &config.wolf_socket {
        Some(socket) => {
            info!(socket = %socket.display(), "pairing through Wolf's API");
            let before = wolf::pending_secrets(socket)
                .await
                .context("reading Wolf's pending pair requests")?;
            let (result, wolf) =
                tokio::join!(pairing, wolf::accept_pairing(socket, &before, &pin_text));
            wolf.context("answering the pair request through Wolf's API")?;
            result
        }
        None => {
            info!("pairing: enter PIN {pin_text} on the host (for Wolf, open the URL it logs)");
            pairing.await
        }
    };
    pairing.map_err(|e| anyhow!("pairing: {e:?}"))?;
    let (_, _, server) = host
        .identity()
        .await
        .ok_or_else(|| anyhow!("paired, but the host identity is missing"))?;
    save_identity(&config.state_dir, &client, &secret, &server)?;
    info!(state = %config.state_dir.display(), "paired; identity saved");
    Ok(())
}

const IDENTITY_FILES: [&str; 3] = ["client-cert.pem", "client-key.pem", "server-cert.pem"];

fn load_identity(dir: &Path) -> Result<Option<(ClientIdentifier, ClientSecret, ServerIdentifier)>> {
    let paths = IDENTITY_FILES.map(|f| dir.join(f));
    if !paths.iter().all(|p| p.exists()) {
        return Ok(None);
    }
    let [client, secret, server] = paths.map(|p| {
        std::fs::read_to_string(&p)
            .with_context(|| format!("reading {}", p.display()))
            .and_then(|s| Pem::from_str(&s).with_context(|| format!("parsing {}", p.display())))
    });
    Ok(Some((
        ClientIdentifier::from_pem(client?),
        ClientSecret::from_pem(secret?),
        ServerIdentifier::from_pem(server?),
    )))
}

fn save_identity(
    dir: &Path,
    client: &ClientIdentifier,
    secret: &ClientSecret,
    server: &ServerIdentifier,
) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let pems = [client.to_pem(), secret.to_pem(), server.to_pem()];
    for (file, pem) in IDENTITY_FILES.iter().zip(pems) {
        let path = dir.join(file);
        std::fs::write(&path, pem.to_string())
            .with_context(|| format!("writing {}", path.display()))?;
    }
    restrict_key(&dir.join(IDENTITY_FILES[1]))?;
    Ok(())
}

/// The client key authenticates us to the host; keep it owner-only.
#[cfg(unix)]
fn restrict_key(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_key(_path: &Path) -> Result<()> {
    Ok(())
}
