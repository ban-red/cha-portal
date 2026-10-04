//! Spike S1 server: synthetic PyroWave-shaped traffic for measuring how much a
//! browser can receive over WebTransport datagrams and WebRTC DataChannels.
//!
//! HTTP (TCP `--http-port`):
//! - `GET  /info`          → WebTransport port + certificate hash for `serverCertificateHashes`
//! - `POST /webrtc/offer`  → SDP answer (query: traffic config + `host`)
//! - `GET  /streams`       → headers of the replayable streams in `--streams`
//! - `POST /webrtc/media`  → SDP answer for an H.264/HEVC/AV1 stream as a WebRTC
//!   video track (query: `name`, `fps`, `secs`, `host`), spike S1d
//!
//! WebTransport (UDP `--wt-port`):
//! - `https://<host>:<port>/s1?mbps=..&fps=..&secs=..&dgram=..` synthetic frames
//! - `https://<host>:<port>/stream?name=..&fps=..&secs=..&dgram=..` an encoded stream (S1c)

mod stream;
mod traffic;
mod webrtc;
mod webrtc_media;
mod wt;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::{Parser, ValueEnum};
use serde::Serialize;
use str0m::change::{SdpAnswer, SdpOffer};
use tower_http::cors::CorsLayer;
use tracing::info;
use wtransport::quinn::TransportConfig;
use wtransport::quinn::congestion::{BbrConfig, CubicConfig, NewRenoConfig};
use wtransport::{Endpoint, Identity, ServerConfig};

#[derive(Parser, Debug, Clone)]
#[command(about = "Spike S1: browser receive benchmark server")]
struct Args {
    /// UDP port for WebTransport.
    #[arg(long, default_value_t = 4433)]
    wt_port: u16,
    /// TCP port for the info/signaling API.
    #[arg(long, default_value_t = 4480)]
    http_port: u16,
    /// Extra addresses to put in the certificate SANs (the primary IPv4 and
    /// loopback are always included).
    #[arg(long)]
    advertise: Vec<IpAddr>,
    /// QUIC congestion controller for the WebTransport path.
    #[arg(long, value_enum, default_value_t = Cc::Cubic)]
    cc: Cc,
    /// QUIC datagram send buffer in bytes. Must hold at least one frame burst.
    #[arg(long, default_value_t = 8 * 1024 * 1024)]
    dgram_send_buffer: usize,
    /// UDP port for WebRTC sessions (ephemeral if busy).
    #[arg(long, default_value_t = 4434)]
    webrtc_port: u16,
    /// Directory of `.chastream` files to replay (spike S1c).
    #[arg(long)]
    streams: Option<std::path::PathBuf>,
    /// SCTP max buffered amount for the WebRTC path.
    #[arg(long, default_value_t = 8 * 1024 * 1024)]
    sctp_buffer: usize,
}

#[derive(ValueEnum, Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum Cc {
    Cubic,
    Bbr,
    NewReno,
}

#[derive(Clone, Serialize)]
struct Info {
    wt_port: u16,
    /// SHA-256 of the WebTransport certificate, lowercase hex.
    cert_hash_hex: String,
    addresses: Vec<IpAddr>,
    cc: Cc,
}

struct AppState {
    info: Info,
    streams: Arc<stream::StreamLibrary>,
    webrtc_port: u16,
    sctp_buffer: usize,
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
    let primary = primary_ipv4().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let mut addresses = vec![primary, IpAddr::V4(Ipv4Addr::LOCALHOST)];
    addresses.extend(args.advertise.iter().copied());
    addresses.dedup();

    // serverCertificateHashes requires ECDSA P-256 and a validity of at most 14 days.
    let mut sans: Vec<String> = addresses.iter().map(ToString::to_string).collect();
    sans.push("localhost".into());
    let identity = Identity::self_signed_builder()
        .subject_alt_names(&sans)
        .from_now_utc()
        .validity_days(13)
        .build()
        .context("generating self-signed certificate")?;
    let digest = identity.certificate_chain().as_slice()[0].hash();
    let cert_hash_hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();

    let mut transport = TransportConfig::default();
    transport
        .datagram_send_buffer_size(args.dgram_send_buffer)
        .datagram_receive_buffer_size(Some(1 << 20))
        .max_idle_timeout(Some(Duration::from_secs(10).try_into()?))
        .keep_alive_interval(Some(Duration::from_secs(2)));
    match args.cc {
        Cc::Cubic => transport.congestion_controller_factory(Arc::new(CubicConfig::default())),
        Cc::Bbr => transport.congestion_controller_factory(Arc::new(BbrConfig::default())),
        Cc::NewReno => transport.congestion_controller_factory(Arc::new(NewRenoConfig::default())),
    };
    let wt_config = ServerConfig::builder()
        .with_bind_default(args.wt_port)
        .with_custom_transport(identity, transport)
        .build();
    let endpoint = Endpoint::server(wt_config).context("binding WebTransport endpoint")?;
    let streams = Arc::new(stream::StreamLibrary::new(args.streams.clone()));
    tokio::spawn(wt::serve(endpoint, Arc::clone(&streams)));

    let state = Arc::new(AppState {
        streams,
        info: Info {
            wt_port: args.wt_port,
            cert_hash_hex,
            addresses: addresses.clone(),
            cc: args.cc,
        },
        webrtc_port: args.webrtc_port,
        sctp_buffer: args.sctp_buffer,
        primary,
    });
    let app = Router::new()
        .route("/info", get(info_handler))
        .route("/streams", get(streams_handler))
        .route("/webrtc/offer", post(offer_handler))
        .route("/webrtc/media", post(media_offer_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);
    let http_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), args.http_port);
    let listener = tokio::net::TcpListener::bind(http_addr).await?;
    info!(
        "S1 server up: http://{primary}:{} (info/signaling), WebTransport udp/{} (cc={:?}); addresses {addresses:?}",
        args.http_port, args.wt_port, args.cc
    );
    axum::serve(listener, app).await?;
    Ok(())
}

async fn info_handler(State(state): State<Arc<AppState>>) -> Json<Info> {
    Json(state.info.clone())
}

async fn media_offer_handler(
    State(state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
    Json(offer): Json<SdpOffer>,
) -> Result<Json<SdpAnswer>, (StatusCode, String)> {
    let query = query.unwrap_or_default();
    let bad_request = |err: anyhow::Error| (StatusCode::BAD_REQUEST, format!("{err:#}"));
    let config = traffic::TrafficConfig::from_query(&query).map_err(bad_request)?;
    let name = traffic::query_value(&query, "name")
        .ok_or_else(|| bad_request(anyhow::anyhow!("missing ?name=")))?;
    let stream = state.streams.load(name).map_err(bad_request)?;
    let params = webrtc_media::MediaParams {
        config,
        host: host_from_query(&query, state.primary),
        port: state.webrtc_port,
        stream,
    };
    let answer = webrtc_media::start(params, offer).map_err(bad_request)?;
    Ok(Json(answer))
}

/// The address the browser used to reach us (`?host=`), or the primary IPv4.
fn host_from_query(query: &str, primary: IpAddr) -> IpAddr {
    traffic::query_value(query, "host")
        .map(|h| if h == "localhost" { "127.0.0.1" } else { h })
        .and_then(|h| h.parse().ok())
        .unwrap_or(primary)
}

async fn streams_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    state
        .streams
        .list()
        .map(Json)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")))
}

async fn offer_handler(
    State(state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
    Json(offer): Json<SdpOffer>,
) -> Result<Json<SdpAnswer>, (StatusCode, String)> {
    let query = query.unwrap_or_default();
    let bad_request = |err: anyhow::Error| (StatusCode::BAD_REQUEST, format!("{err:#}"));
    let config = traffic::TrafficConfig::from_query(&query).map_err(bad_request)?;
    let host = host_from_query(&query, state.primary);
    let params = webrtc::SessionParams {
        config,
        host,
        port: state.webrtc_port,
        sctp_buffer: state.sctp_buffer,
    };
    let answer = webrtc::start(params, offer).map_err(bad_request)?;
    Ok(Json(answer))
}

/// The IPv4 the OS would route external traffic from. UDP `connect` sends nothing.
fn primary_ipv4() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip())
}
