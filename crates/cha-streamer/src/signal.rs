//! Startup, the app runner, and the S1/S2-compatible signalling endpoints.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use cha_nvenc::{Codec, CudaContext};
use clap::Parser;
use serde_json::{Value, json};
use str0m::change::{SdpAnswer, SdpOffer};
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

use crate::audio::Audio;
use crate::compositor::{self, fit_size};
use crate::gamepad::Gamepads;
use crate::media::{EncodeSettings, FrameHub, Media};
use crate::net;
use crate::session;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "One environment's media engine: compositor -> NVENC -> WebRTC"
)]
struct Args {
    #[arg(long, default_value = "/dev/dri/renderD128")]
    render_node: PathBuf,
    #[arg(long, default_value_t = 2560)]
    width: u32,
    #[arg(long, default_value_t = 1440)]
    height: u32,
    /// Encoded (sent) frames per second.
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// Frame callbacks per second; apps draw at this rate. Higher cuts input
    /// latency (S2: 240 Hz halves click -> screen); only `--fps` of them are
    /// composited and encoded.
    #[arg(long, default_value_t = 240)]
    compositor_fps: u32,
    /// Video bitrate in Mbit/s.
    #[arg(long, default_value_t = 40)]
    mbps: u32,
    /// Codecs offered.
    #[arg(long, value_delimiter = ',', default_value = "hevc,h264,av1")]
    codecs: Vec<String>,
    /// The Wayland socket's name in $XDG_RUNTIME_DIR.
    #[arg(long, default_value = "wayland-0")]
    socket: String,
    /// A command to run as the compositor's client (through `sh -c`), restarted
    /// when it exits.
    #[arg(long)]
    run: Option<String>,
    /// Where signalling (HTTP) listens. The agent runs streamers on
    /// 127.0.0.1: browsers reach them through the portal, not directly.
    #[arg(long, default_value = "0.0.0.0")]
    listen: IpAddr,
    /// Signalling (HTTP) port.
    #[arg(long, default_value_t = 4495)]
    http_port: u16,
    /// WebRTC UDP port (ICE-lite).
    #[arg(long, default_value_t = 4496)]
    webrtc_port: u16,
    /// Addresses browsers may reach WebRTC on (host candidates). Default:
    /// every IPv4 address of this machine except loopback and container
    /// bridges, so LAN and mesh (Tailscale, WireGuard) clients connect directly.
    #[arg(long, value_delimiter = ',')]
    advertise: Vec<IpAddr>,
    /// The router's public address when it forwards the WebRTC ports here:
    /// announced as a candidate on the same ports, for WAN clients without a
    /// mesh or TURN.
    #[arg(long, value_delimiter = ',')]
    public_address: Vec<IpAddr>,
    /// The portal's public key (base64 Ed25519). With it, a stream needs a
    /// media token the portal signed for `--environment-id`; without it, the
    /// shared token below (benchmarks, the dev loop).
    #[arg(long, requires = "environment_id")]
    portal_key: Option<String>,
    /// The environment this streamer serves (checked in media tokens).
    #[arg(long)]
    environment_id: Option<String>,
    /// Token a browser must present to start a stream. Without it, the one in
    /// `--token-file` is used, or a random one is generated and saved there.
    #[arg(long, env = "CHA_STREAMER_TOKEN")]
    token: Option<String>,
    #[arg(long, default_value = "/state/token")]
    token_file: PathBuf,
    /// The app's user, when it runs in its own container as a different uid:
    /// the runtime dir and the Wayland socket are handed to it.
    #[arg(long)]
    app_uid: Option<u32>,
    /// No sound: no PulseAudio server, no audio track.
    #[arg(long)]
    no_audio: bool,
    /// Where gamepads' device nodes (`dev/`) and udev entries (`udev/`) go,
    /// shared with the app as its `/dev/input` and `/run/udev`. Without it,
    /// no gamepads.
    #[arg(long)]
    input_dir: Option<PathBuf>,
    #[arg(long, default_value = "/dev/uinput")]
    uinput: PathBuf,
    /// Gamepads made at start, for apps that look for pads only once.
    #[arg(long, default_value_t = 1)]
    gamepads: usize,
}

/// Who may start a stream.
enum Auth {
    /// Whoever presents this shared token (benchmarks, the dev loop).
    Token(String),
    /// Whoever presents a media token the portal signed for this environment.
    Portal { key: String, environment: String },
}

struct AppState {
    media: Arc<Media>,
    audio: Option<Arc<Audio>>,
    gamepads: Option<Arc<Gamepads>>,
    auth: Auth,
    /// The one session running: a new connection takes over (one viewer per
    /// environment until sharing, plan §3.4).
    current: tokio::sync::Mutex<Option<session::Running>>,
    mbps: u32,
    fps: u32,
    webrtc_port: u16,
    hosts: Vec<IpAddr>,
    public: Vec<IpAddr>,
}

pub fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,str0m=warn,smithay=warn".into()),
        )
        .init();
    str0m::crypto::from_feature_flags().install_process_default();
    let args = Args::parse();

    let codecs = args
        .codecs
        .iter()
        .map(|c| Codec::from_name(c).ok_or_else(|| anyhow!("unknown codec {c}")))
        .collect::<Result<Vec<_>>>()?;
    let runtime_dir =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR must be set")?);
    ensure_runtime_dir(&runtime_dir, args.app_uid)?;
    let (width, height) = fit_size(args.width, args.height);

    let cuda =
        CudaContext::new(pci_slot(&args.render_node).as_deref()).map_err(|e| anyhow!("{e}"))?;
    let hub = Arc::new(FrameHub::default());
    let handle = compositor::spawn(
        compositor::Config {
            render_node: args.render_node.clone(),
            width,
            height,
            compositor_fps: args.compositor_fps.max(args.fps),
            encode_fps: args.fps,
            socket_name: args.socket.clone(),
        },
        Arc::clone(&cuda),
        Arc::clone(&hub),
    )?;
    if let Some(uid) = args.app_uid {
        let socket = runtime_dir.join(&handle.socket_name);
        std::os::unix::fs::chown(&socket, Some(uid), Some(uid))
            .with_context(|| format!("handing {} to uid {uid}", socket.display()))?;
    }
    let media = Arc::new(Media::new(
        hub,
        handle.commands,
        cuda,
        EncodeSettings {
            fps: args.fps,
            bitrate_bps: args.mbps * 1_000_000,
        },
        codecs,
        (width, height),
    ));
    let runtime = tokio::runtime::Runtime::new()?;
    // Before the app starts, so it finds the sound server.
    let audio = if args.no_audio {
        None
    } else {
        let _runtime = runtime.enter();
        Some(Audio::start(
            &runtime_dir.join("pulse/native"),
            args.app_uid,
        )?)
    };
    let gamepads = args.input_dir.as_ref().and_then(|dir| {
        match Gamepads::new(&args.uinput, dir, args.app_uid, args.gamepads) {
            Ok(pads) => Some(Arc::new(pads)),
            Err(err) => {
                warn!("no gamepads: {err:#}");
                None
            }
        }
    });
    if let Some(command) = args.run.clone() {
        keep_running(command, handle.socket_name.to_string_lossy().into_owned());
    }

    let auth = match (&args.portal_key, &args.environment_id) {
        (Some(key), Some(environment)) => {
            cha_wire::parse_public_key(key).map_err(|e| anyhow!("--portal-key: {e}"))?;
            Auth::Portal {
                key: key.clone(),
                environment: environment.clone(),
            }
        }
        _ => Auth::Token(match args.token.clone() {
            Some(token) => token,
            None => saved_or_new_token(&args.token_file)?,
        }),
    };
    let access = match &auth {
        Auth::Token(token) => format!("token {token}"),
        Auth::Portal { environment, .. } => format!("media tokens for environment {environment}"),
    };
    let primary = primary_ipv4().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let mut hosts = if args.advertise.is_empty() {
        net::host_addresses()
    } else {
        args.advertise.clone()
    };
    // The routed address first.
    hosts.sort_by_key(|h| *h != primary);
    if hosts.is_empty() {
        hosts.push(primary);
    }
    let advertised = hosts
        .iter()
        .map(IpAddr::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let state = Arc::new(AppState {
        media,
        audio,
        gamepads,
        auth,
        current: tokio::sync::Mutex::default(),
        mbps: args.mbps,
        fps: args.fps,
        webrtc_port: args.webrtc_port,
        hosts,
        public: args.public_address.clone(),
    });
    let app = Router::new()
        .route("/info", get(info_handler))
        .route("/streams", get(streams_handler))
        .route("/webrtc/media", post(media_offer_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    runtime.block_on(async move {
        let listener =
            tokio::net::TcpListener::bind(SocketAddr::new(args.listen, args.http_port)).await?;
        info!(
            "cha-streamer up: http://{}:{} (signalling), WebRTC {advertised} port {}/udp; {access}",
            args.listen, args.http_port, args.webrtc_port
        );
        axum::serve(listener, app).await?;
        Ok(())
    })
}

/// The runtime dir must exist and be private to the app's user (GLib, dbus and
/// others check its owner and mode).
fn ensure_runtime_dir(dir: &Path, app_uid: Option<u32>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    if let Some(uid) = app_uid {
        std::os::unix::fs::chown(dir, Some(uid), Some(uid))
            .with_context(|| format!("handing {} to uid {uid}", dir.display()))?;
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// The render node's PCI slot, so CUDA picks the same GPU.
fn pci_slot(render_node: &Path) -> Option<String> {
    let name = render_node.file_name()?.to_str()?;
    let uevent = std::fs::read_to_string(format!("/sys/class/drm/{name}/device/uevent")).ok()?;
    uevent
        .lines()
        .find_map(|l| l.strip_prefix("PCI_SLOT_NAME="))
        .map(|s| s.trim().to_string())
}

/// Runs the app against the compositor and restarts it when it exits.
fn keep_running(command: String, socket: String) {
    std::thread::spawn(move || {
        loop {
            let started = Instant::now();
            info!(%command, "starting the app");
            let status = Command::new("sh")
                .arg("-c")
                .arg(&command)
                .env("WAYLAND_DISPLAY", &socket)
                .env_remove("DISPLAY")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            warn!(?status, "the app exited");
            if started.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    });
}

async fn info_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    let (width, height) = state.media.size();
    Json(json!({
        // WebRTC only, like the S3 gateway; the page reads these for its other paths.
        "gateway": true,
        "input": true,
        "audio": state.audio.is_some(),
        "gamepads": state.gamepads.is_some(),
        "token": true,
        "width": width,
        "height": height,
        "resize": true,
        "wt_port": 0,
        "cert_hash_hex": "",
    }))
}

async fn streams_handler(State(state): State<Arc<AppState>>) -> Json<Vec<Value>> {
    let (width, height) = state.media.size();
    Json(
        state
            .media
            .codecs()
            .iter()
            .map(|codec| {
                json!({
                    "name": format!("live-{}", codec.name()),
                    "codec": codec.name(),
                    "codecString": match codec {
                        Codec::H264 => "avc1.640033",
                        Codec::Hevc => "hev1.1.6.L153.B0",
                        Codec::Av1 => "av01.0.13M.08",
                    },
                    "width": width,
                    "height": height,
                    "chroma": "420",
                    "fps": state.fps,
                    "bitrateMbps": state.mbps,
                    "frames": 0,
                    "content": "cha-streamer (live)",
                })
            })
            .collect(),
    )
}

async fn media_offer_handler(
    State(state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
    body: String,
) -> Result<Json<SdpAnswer>, (StatusCode, String)> {
    // Check the token before touching the body.
    let query = query.unwrap_or_default();
    let presented = query_value(&query, "token").unwrap_or("");
    match &state.auth {
        Auth::Token(token) => {
            if !constant_time_eq(presented.as_bytes(), token.as_bytes()) {
                // The length (never the value) tells a mistyped token from a
                // truncated or padded one.
                warn!(
                    presented_chars = presented.len(),
                    expected_chars = token.len(),
                    "rejected a stream request: missing or wrong token"
                );
                return Err((StatusCode::FORBIDDEN, "missing or wrong token".into()));
            }
        }
        Auth::Portal { key, environment } => {
            match cha_wire::verify_media_token(key, presented, environment, unix_now()) {
                Ok(claims) => {
                    info!(user = %claims.sub, role = %claims.role, "media token accepted")
                }
                Err(err) => {
                    warn!("rejected a stream request: media token {err}");
                    return Err((StatusCode::FORBIDDEN, format!("media token {err}")));
                }
            }
        }
    }
    let bad_request = |err: anyhow::Error| (StatusCode::BAD_REQUEST, format!("{err:#}"));
    let offer: SdpOffer = serde_json::from_str(&body).map_err(|e| bad_request(e.into()))?;
    let name = query_value(&query, "name").ok_or_else(|| bad_request(anyhow!("missing ?name=")))?;
    let codec = name
        .strip_prefix("live-")
        .and_then(Codec::from_name)
        .ok_or_else(|| bad_request(anyhow!("unknown stream {name}")))?;
    let params = session::SessionParams {
        codec,
        secs: query_value(&query, "secs")
            .and_then(|v| v.parse::<f64>().ok())
            .map_or(15, |v| v as u32),
        hosts: match query_value(&query, "host")
            .map(|h| if h == "localhost" { "127.0.0.1" } else { h })
            .and_then(|h| h.parse().ok())
        {
            Some(host) => vec![host],
            None => state.hosts.clone(),
        },
        public: state.public.clone(),
        port: state.webrtc_port,
        media: Arc::clone(&state.media),
        audio: state.audio.clone(),
        gamepads: state.gamepads.clone(),
    };
    // A new connection takes over: stop the running session first, so its UDP
    // port is free.
    let mut current = state.current.lock().await;
    if let Some(old) = current.take() {
        let _ = old.stop.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), old.handle).await;
    }
    let (answer, running) = session::start(params, offer).await.map_err(bad_request)?;
    *current = Some(running);
    Ok(Json(answer))
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The token saved by an earlier run, or a new one (saved, owner-only).
fn saved_or_new_token(path: &Path) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(saved) = std::fs::read_to_string(path)
        && !saved.trim().is_empty()
    {
        return Ok(saved.trim().to_string());
    }
    let token = random_token()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // With a newline, so `cat` shows it on a line of its own.
    std::fs::write(path, format!("{token}\n"))
        .with_context(|| format!("saving the token to {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(token)
}

/// 24 hex characters from the OS's random source.
fn random_token() -> Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 12];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .context("reading /dev/urandom")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// The IPv4 the OS would route external traffic from. UDP `connect` sends nothing.
fn primary_ipv4() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip())
}
