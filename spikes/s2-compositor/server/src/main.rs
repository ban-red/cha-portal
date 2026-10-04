//! Spike S2 milestone 2: a minimal `cha-streamer`.
//!
//! - The compositor (`waylanddisplaysrc`, Wolf's gst-wayland-display) runs
//!   headless on the GPU with Google Chrome as its Wayland client.
//! - Each codec has an always-on NVENC branch (CUDA zero-copy).
//! - A browser gets one branch as a WebRTC video track (str0m, playout-delay 0)
//!   and sends keyboard and mouse back on the `control` DataChannel.
//!
//! It speaks the S1/S3 signalling API, so the S1c/S1d page measures it:
//! - `GET /info`, `GET /streams` (one entry per codec);
//! - `POST /webrtc/media?name=chrome-<codec>&secs=&host=&token=`: SDP answer.
//!
//! Starting a stream needs the token printed at startup (or `--token`): the
//! stream is a live, controllable browser.

mod input;
mod pipeline;
mod session;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde_json::{Value, json};
use str0m::change::{SdpAnswer, SdpOffer};
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

use crate::pipeline::{Codec, Media, PipelineConfig};

#[derive(Parser, Debug)]
#[command(about = "Spike S2: compositor + Chrome -> NVENC -> WebRTC, with input back")]
struct Args {
    #[arg(long, default_value = "/dev/dri/renderD128")]
    render_node: String,
    #[arg(long, default_value_t = 2560)]
    width: u32,
    #[arg(long, default_value_t = 1440)]
    height: u32,
    /// Encoded (sent) frame rate.
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// The compositor's and Chrome's frame rate (default: `--fps`). Higher means
    /// lower input latency; extra frames are dropped before encoding.
    #[arg(long)]
    compositor_fps: Option<u32>,
    /// Video bitrate in Mbit/s.
    #[arg(long, default_value_t = 40)]
    mbps: u32,
    /// Encoders to run (each is always on).
    #[arg(long, value_delimiter = ',', default_value = "hevc,h264")]
    codecs: Vec<String>,
    /// What Chrome opens.
    #[arg(long, default_value = "file:///bench/page/live.html")]
    url: String,
    /// Run the compositor without Chrome.
    #[arg(long)]
    no_chrome: bool,
    /// Extra Chrome flag (repeatable), e.g. `--chrome-arg=--disable-gpu-vsync`.
    #[arg(long = "chrome-arg", allow_hyphen_values = true)]
    chrome_args: Vec<String>,
    /// Signalling (HTTP) port.
    #[arg(long, default_value_t = 4495)]
    http_port: u16,
    /// WebRTC UDP port (ICE-lite).
    #[arg(long, default_value_t = 4496)]
    webrtc_port: u16,
    /// Token a browser must present to start a stream. Without it, the one in
    /// `--token-file` is used, or a random one is generated and saved there.
    #[arg(long, env = "S2_TOKEN")]
    token: Option<String>,
    #[arg(long, default_value = "/state/token")]
    token_file: PathBuf,
}

struct AppState {
    media: Arc<Media>,
    token: String,
    mbps: u32,
    fps: u32,
    webrtc_port: u16,
    primary: IpAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,str0m=warn".into()),
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
        PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp/xdg".into()));
    std::fs::create_dir_all(&runtime_dir)?;
    let media = Arc::new(Media::new(&PipelineConfig {
        render_node: args.render_node.clone(),
        width: args.width,
        height: args.height,
        fps: args.fps,
        compositor_fps: args.compositor_fps.unwrap_or(args.fps),
        bitrate_kbps: args.mbps * 1000,
        codecs,
    })?);
    if !args.no_chrome {
        let socket = wait_for_socket(&runtime_dir, Duration::from_secs(10))?;
        keep_chrome_running(
            socket,
            args.url.clone(),
            args.chrome_args.clone(),
            args.width,
            args.height,
        );
    }

    let token = match args.token.clone() {
        Some(token) => token,
        None => saved_or_new_token(&args.token_file)?,
    };
    let primary = primary_ipv4().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let state = Arc::new(AppState {
        media,
        token: token.clone(),
        mbps: args.mbps,
        fps: args.fps,
        webrtc_port: args.webrtc_port,
        primary,
    });
    let app = Router::new()
        .route("/info", get(info_handler))
        .route("/streams", get(streams_handler))
        .route("/webrtc/media", post(media_offer_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        args.http_port,
    ))
    .await?;
    info!(
        "S2 streamer up: http://{primary}:{} (signalling), WebRTC udp/{}; token {token}",
        args.http_port, args.webrtc_port
    );
    axum::serve(listener, app).await?;
    Ok(())
}

async fn info_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        // WebRTC only, like the S3 gateway; the page reads these for its other paths.
        "gateway": true,
        "input": true,
        "token": true,
        "width": state.media.size().0,
        "height": state.media.size().1,
        "resize": true,
        "wt_port": 0,
        "cert_hash_hex": "",
    }))
}

async fn streams_handler(State(state): State<Arc<AppState>>) -> Json<Vec<Value>> {
    Json(
        state
            .media
            .codecs()
            .into_iter()
            .map(|codec| {
                json!({
                    "name": format!("chrome-{}", codec.name()),
                    "codec": codec.name(),
                    "codecString": match codec {
                        Codec::H264 => "avc1.640033",
                        Codec::Hevc => "hev1.1.6.L153.B0",
                    },
                    "width": state.media.size().0,
                    "height": state.media.size().1,
                    "chroma": "420",
                    "fps": state.fps,
                    "bitrateMbps": state.mbps,
                    "frames": 0,
                    "content": "Google Chrome (live)",
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
    if !constant_time_eq(
        query_value(&query, "token").unwrap_or("").as_bytes(),
        state.token.as_bytes(),
    ) {
        return Err((StatusCode::FORBIDDEN, "missing or wrong token".into()));
    }
    let bad_request = |err: anyhow::Error| (StatusCode::BAD_REQUEST, format!("{err:#}"));
    let offer: SdpOffer = serde_json::from_str(&body).map_err(|e| bad_request(e.into()))?;
    let name = query_value(&query, "name").ok_or_else(|| bad_request(anyhow!("missing ?name=")))?;
    let codec = name
        .strip_prefix("chrome-")
        .and_then(Codec::from_name)
        .ok_or_else(|| bad_request(anyhow!("unknown stream {name}")))?;
    let params = session::SessionParams {
        codec,
        secs: query_value(&query, "secs")
            .and_then(|v| v.parse::<f64>().ok())
            .map_or(15, |v| v as u32),
        host: query_value(&query, "host")
            .map(|h| if h == "localhost" { "127.0.0.1" } else { h })
            .and_then(|h| h.parse().ok())
            .unwrap_or(state.primary),
        port: state.webrtc_port,
        media: Arc::clone(&state.media),
    };
    let answer = session::start(params, offer).await.map_err(bad_request)?;
    Ok(Json(answer))
}

/// The compositor's Wayland socket, once it exists.
fn wait_for_socket(dir: &std::path::Path, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(name) = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .find(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
        {
            return Ok(name);
        }
        if Instant::now() > deadline {
            return Err(anyhow!(
                "no Wayland socket in {} after {timeout:?}",
                dir.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Runs Chrome against the compositor and restarts it if it exits.
fn keep_chrome_running(socket: String, url: String, extra: Vec<String>, width: u32, height: u32) {
    std::thread::spawn(move || {
        loop {
            let started = Instant::now();
            let _ = std::fs::remove_dir_all("/tmp/chrome-profile");
            info!(%url, ?extra, "starting Chrome");
            let status = Command::new("google-chrome-stable")
                .args([
                    "--ozone-platform=wayland",
                    "--enable-features=UseOzonePlatform",
                    // The spike's container runs as root, which Chrome's sandbox refuses.
                    "--no-sandbox",
                    "--no-first-run",
                    "--no-default-browser-check",
                    "--disable-sync",
                    "--password-store=basic",
                    "--force-device-scale-factor=1",
                    "--user-data-dir=/tmp/chrome-profile",
                    "--kiosk",
                    &format!("--window-size={width},{height}"),
                ])
                .args(&extra)
                .arg(&url)
                .env("WAYLAND_DISPLAY", &socket)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            warn!(?status, "Chrome exited");
            if started.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    });
}

/// The token saved by an earlier run, or a new one (saved, owner-only).
fn saved_or_new_token(path: &std::path::Path) -> Result<String> {
    if let Ok(saved) = std::fs::read_to_string(path)
        && !saved.trim().is_empty()
    {
        return Ok(saved.trim().to_string());
    }
    let token = random_token()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, &token)
        .with_context(|| format!("saving the token to {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
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
