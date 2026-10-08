//! cha-gateway: streams a Moonlight host (Sunshine, Apollo) to the browser
//! player, standing in for cha-streamer (ADR 0008).
//!
//! The gateway is the node's Moonlight client for one environment: it
//! launches an app on the host, keeps that stream running for the
//! environment's life, and passes the host's H.264 or HEVC access units and
//! Opus packets, untouched, to however many browsers watch (WebRTC, as the
//! streamer does). The player's input goes back to the host as Moonlight
//! input.

mod host;
mod hub;
mod input;
mod net;
// Not served until the gateway has a WebTransport endpoint (docs/plans/vibepollo-pyrowave.md, "As built").
#[allow(dead_code)]
mod pyrowave;
mod session;
mod signal;
mod viewers;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use cha_gamestream::handoff::Chroma;
use clap::Parser;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::host::{CodecChoice, HostConfig};
use crate::signal::AppState;
use crate::viewers::Viewers;

/// How long to wait for the host to confirm the app is closed on the way out.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(20);

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Streams a Moonlight host (Sunshine, Apollo) to the browser player"
)]
struct Args {
    /// Where signalling (HTTP) listens. The agent runs gateways on
    /// 127.0.0.1: browsers reach them through the portal, not directly.
    #[arg(long, default_value = "127.0.0.1")]
    listen: IpAddr,
    /// Signalling (HTTP) port.
    #[arg(long)]
    http_port: u16,
    /// WebRTC UDP port (ICE-lite).
    #[arg(long)]
    webrtc_port: u16,
    /// The portal's public key (base64 Ed25519): streams need a media token
    /// it signed for `--environment-id`.
    #[arg(long)]
    portal_key: String,
    /// The environment this gateway serves (checked in media tokens).
    #[arg(long)]
    environment_id: String,
    /// The stream's width, and the size the app is launched at.
    #[arg(long)]
    width: u32,
    #[arg(long)]
    height: u32,
    #[arg(long)]
    fps: u32,
    /// The router's public address when it forwards the WebRTC port here:
    /// announced as a candidate on the same port, for WAN clients without a
    /// mesh or TURN.
    #[arg(long, value_delimiter = ',')]
    public_address: Vec<IpAddr>,
    /// The Moonlight host's address.
    #[arg(long)]
    host: String,
    /// Its HTTP port.
    #[arg(long, default_value_t = 47989)]
    host_http_port: u16,
    /// Its HTTPS port (the host's own answer wins when they differ).
    #[arg(long, default_value_t = 47984)]
    host_https_port: u16,
    /// The host's `uniqueid`: names its certificate in `--identity-dir`.
    #[arg(long)]
    host_unique_id: String,
    /// The host's app to launch.
    #[arg(long)]
    app_id: u32,
    /// The node's Moonlight identity: `client-cert.pem`, `client-key.pem` and
    /// `hosts/<uniqueid>/server-cert.pem`. Read only.
    #[arg(long)]
    identity_dir: PathBuf,
    /// The video bitrate in Mbit/s; 0 picks one for the picture size and rate.
    #[arg(long, default_value_t = 0)]
    mbps: u32,
    /// The codec to ask the host for: HEVC if it can encode it, else H.264
    /// (`auto`), or one of them. `pyrowave420` and `pyrowave444` (a Vibepollo
    /// host) are never chosen by `auto`, and need WebTransport, which the
    /// gateway doesn't serve yet.
    #[arg(long, value_enum, default_value_t = CodecChoice::Auto)]
    codec: CodecChoice,
    /// Addresses browsers may reach WebRTC on (host candidates). Default:
    /// every IPv4 address of this machine except loopback and container
    /// bridges, so LAN and mesh (Tailscale, WireGuard) clients connect directly.
    #[arg(long, value_delimiter = ',')]
    advertise: Vec<IpAddr>,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,str0m=warn".into()),
        )
        .init();
    let args = Args::parse();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            error!("starting the async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(args)) {
        Ok(code) => code,
        Err(err) => {
            error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<ExitCode> {
    str0m::crypto::from_feature_flags().install_process_default();
    cha_wire::parse_public_key(&args.portal_key).map_err(|e| anyhow!("--portal-key: {e}"))?;
    anyhow::ensure!(
        args.width > 0 && args.height > 0 && args.fps > 0,
        "--width, --height and --fps must be positive"
    );

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
    let hub = hub::Hub::bind(&hosts, args.webrtc_port).await?;

    let (link, plumbing) = host::link();
    let state = Arc::new(AppState {
        portal_key: args.portal_key.clone(),
        environment: args.environment_id.clone(),
        link,
        viewers: Viewers::new(),
        hub,
        hosts,
        public: args.public_address.clone(),
        width: args.width,
        height: args.height,
        fps: args.fps,
    });
    // Signalling first: the agent connects as soon as the container runs.
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(args.listen, args.http_port))
        .await
        .with_context(|| format!("binding {}:{}", args.listen, args.http_port))?;
    info!(
        "cha-gateway up: http://{}:{} (signalling), WebRTC {advertised} port {}/udp; host {}:{}, app {}",
        args.listen, args.http_port, args.webrtc_port, args.host, args.host_http_port, args.app_id
    );
    let server = tokio::spawn(axum::serve(listener, signal::router(state)).into_future());

    let bitrate_kbps = match (args.mbps, args.codec) {
        (0, CodecChoice::Pyrowave420) => {
            host::auto_pyrowave_bitrate_kbps(args.width, args.height, args.fps, Chroma::Yuv420)
        }
        (0, CodecChoice::Pyrowave444) => {
            host::auto_pyrowave_bitrate_kbps(args.width, args.height, args.fps, Chroma::Yuv444)
        }
        (0, _) => host::auto_bitrate_kbps(args.width, args.height, args.fps),
        (mbps, _) => mbps * 1000,
    };
    let (stop, stopped) = watch::channel(false);
    let mut stream = tokio::spawn(host::run(
        HostConfig {
            address: args.host.clone(),
            http_port: args.host_http_port,
            https_port: args.host_https_port,
            app_id: args.app_id,
            identity_dir: args.identity_dir.clone(),
            unique_id: args.host_unique_id.clone(),
            width: args.width,
            height: args.height,
            fps: args.fps,
            bitrate_kbps,
            codec: args.codec,
        },
        plumbing,
        stopped,
    ));

    let mut term = signal(SignalKind::terminate()).context("listening for SIGTERM")?;
    let mut int = signal(SignalKind::interrupt()).context("listening for SIGINT")?;
    let code = tokio::select! {
        _ = term.recv() => stop_gracefully("SIGTERM", &stop, &mut stream).await,
        _ = int.recv() => stop_gracefully("SIGINT", &stop, &mut stream).await,
        ended = &mut stream => {
            // The stream ended by itself: the host dropped it, or never started.
            match ended {
                Ok(Ok(())) => error!("the host stream ended"),
                Ok(Err(err)) => error!("the host stream ended: {err:#}"),
                Err(err) => error!("the host stream task failed: {err}"),
            }
            ExitCode::FAILURE
        }
        served = server => {
            match served {
                Ok(Err(err)) => error!("the signalling server stopped: {err}"),
                Ok(Ok(())) => error!("the signalling server stopped"),
                Err(err) => error!("the signalling server task failed: {err}"),
            }
            stop_gracefully("a server failure", &stop, &mut stream).await;
            ExitCode::FAILURE
        }
    };
    Ok(code)
}

/// Quits the app on the host and lets the stream task finish; exit 0 unless
/// that didn't work out in time.
async fn stop_gracefully(
    why: &str,
    stop: &watch::Sender<bool>,
    stream: &mut tokio::task::JoinHandle<Result<()>>,
) -> ExitCode {
    info!("{why}: closing the app on the host");
    let _ = stop.send(true);
    match tokio::time::timeout(SHUTDOWN_WAIT, stream).await {
        Ok(Ok(Ok(()))) => ExitCode::SUCCESS,
        Ok(Ok(Err(err))) => {
            // It was told to stop, and the app is closed (or was never open).
            warn!("the host stream had already failed: {err:#}");
            ExitCode::SUCCESS
        }
        Ok(Err(err)) => {
            error!("the host stream task failed: {err}");
            ExitCode::FAILURE
        }
        Err(_) => {
            error!("the host didn't finish closing within {SHUTDOWN_WAIT:?}");
            ExitCode::FAILURE
        }
    }
}

/// The IPv4 the OS would route external traffic from. UDP `connect` sends nothing.
fn primary_ipv4() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn required() -> Vec<&'static str> {
        vec![
            "cha-gateway",
            "--http-port",
            "7660",
            "--webrtc-port",
            "7661",
            "--portal-key",
            "key",
            "--environment-id",
            "env-1",
            "--width",
            "2560",
            "--height",
            "1440",
            "--fps",
            "60",
            "--host",
            "192.168.1.20",
            "--host-unique-id",
            "ABCDEF0123456789",
            "--app-id",
            "881448767",
            "--identity-dir",
            "/identity",
            "--listen",
            "127.0.0.1",
            "--host-http-port",
            "47989",
            "--host-https-port",
            "47984",
        ]
    }

    #[test]
    fn the_agents_arguments_parse() {
        let mut argv = required();
        argv.extend(["--public-address", "203.0.113.9"]);
        let args = Args::try_parse_from(argv).unwrap();
        assert_eq!(args.listen, "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!((args.http_port, args.webrtc_port), (7660, 7661));
        assert_eq!(args.environment_id, "env-1");
        assert_eq!((args.width, args.height, args.fps), (2560, 1440, 60));
        assert_eq!(
            args.public_address,
            ["203.0.113.9".parse::<IpAddr>().unwrap()]
        );
        assert_eq!(args.host, "192.168.1.20");
        assert_eq!((args.host_http_port, args.host_https_port), (47989, 47984));
        assert_eq!(args.host_unique_id, "ABCDEF0123456789");
        assert_eq!(args.app_id, 881_448_767);
        assert_eq!(args.identity_dir, PathBuf::from("/identity"));
        assert_eq!(args.codec, CodecChoice::Auto);
        assert_eq!(args.mbps, 0);
    }

    #[test]
    fn defaults_cover_the_optional_ones() {
        let args = Args::try_parse_from([
            "cha-gateway",
            "--http-port",
            "1",
            "--webrtc-port",
            "2",
            "--portal-key",
            "k",
            "--environment-id",
            "e",
            "--width",
            "1280",
            "--height",
            "720",
            "--fps",
            "30",
            "--host",
            "h",
            "--host-unique-id",
            "u",
            "--app-id",
            "1",
            "--identity-dir",
            "/i",
        ])
        .unwrap();
        assert_eq!(args.listen, "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!((args.host_http_port, args.host_https_port), (47989, 47984));
        assert!(args.public_address.is_empty());
    }

    #[test]
    fn unknown_arguments_are_refused() {
        let mut argv = required();
        argv.push("--no-such-flag");
        let err = Args::try_parse_from(argv).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn a_missing_host_is_refused() {
        let err = Args::try_parse_from(["cha-gateway", "--http-port", "1"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }
}
