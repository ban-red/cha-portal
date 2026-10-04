//! Spike S3: a Moonlight → WebRTC gateway.
//!
//! The gateway is a Moonlight client of a GameStream host on the same node
//! (Wolf). It pairs once, launches an app, and forwards every frame it
//! receives, untouched, to the browser as a WebRTC video track with
//! playout-delay 0. There is no transcoding.
//!
//! It speaks the S1 server's signalling API, so the S1c/S1d page measures it
//! with its `webrtc` path, unchanged:
//! - `GET /info`: what the page needs to know (no WebTransport here);
//! - `GET /streams`: one entry per offered app and codec, e.g. `test-ball-hevc`;
//! - `POST /webrtc/media?name=<stream>&fps=&secs=&mbps=&host=`: SDP answer.

mod host;
mod session;
mod wolf;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde_json::{Value, json};
use str0m::change::{SdpAnswer, SdpOffer};
use tower_http::cors::CorsLayer;
use tracing::info;

use moonlight_common::App;

use crate::host::{Codec, Gateway, GatewayConfig, StreamRequest};

#[derive(Parser, Debug)]
#[command(about = "Spike S3: Moonlight (Wolf) → WebRTC passthrough gateway")]
struct Args {
    /// GameStream host to stream from.
    #[arg(long, default_value = "127.0.0.1")]
    moonlight_host: String,
    /// Its HTTP port.
    #[arg(long, default_value_t = 47989)]
    moonlight_port: u16,
    /// Apps to offer: titles containing any of these (case-insensitive, comma-separated).
    #[arg(long, value_delimiter = ',', default_value = "test")]
    apps: Vec<String>,
    /// Wolf's API socket, to pair without typing a PIN.
    #[arg(long)]
    wolf_socket: Option<PathBuf>,
    /// Where the paired identity is kept.
    #[arg(long, default_value = "state")]
    state_dir: PathBuf,
    /// Signalling (HTTP) port.
    #[arg(long, default_value_t = 4490)]
    http_port: u16,
    /// WebRTC UDP port (ICE-lite).
    #[arg(long, default_value_t = 4491)]
    webrtc_port: u16,
    #[arg(long, default_value_t = 2560)]
    width: u32,
    #[arg(long, default_value_t = 1440)]
    height: u32,
    /// Default bitrate in Mbit/s; the page can override it with `?mbps=`.
    #[arg(long, default_value_t = 40)]
    mbps: u32,
}

struct AppState {
    gateway: Arc<Gateway>,
    width: u32,
    height: u32,
    mbps: u32,
    webrtc_port: u16,
    primary: IpAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,str0m=warn,moonlight_common=warn".into()),
        )
        .init();
    str0m::crypto::from_feature_flags().install_process_default();
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| anyhow!("a rustls crypto provider was already installed"))?;

    let args = Args::parse();
    let gateway = Gateway::connect(GatewayConfig {
        address: args.moonlight_host.clone(),
        http_port: args.moonlight_port,
        apps: args.apps.clone(),
        state_dir: args.state_dir.clone(),
        wolf_socket: args.wolf_socket.clone(),
    })
    .await?;

    let primary = primary_ipv4().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let state = Arc::new(AppState {
        gateway,
        width: args.width,
        height: args.height,
        mbps: args.mbps,
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
        "S3 gateway up: http://{primary}:{} (signalling), WebRTC udp/{}, host {}:{}",
        args.http_port, args.webrtc_port, args.moonlight_host, args.moonlight_port
    );
    axum::serve(listener, app).await?;
    Ok(())
}

async fn info_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "gateway": true,
        "apps": state.gateway.apps.iter().map(|a| &a.title).collect::<Vec<_>>(),
        // The page reads these for its WebTransport paths, which the gateway doesn't serve.
        "wt_port": 0,
        "cert_hash_hex": "",
    }))
}

/// One pseudo-stream per offered app and codec, in the shape the S1c/S1d page expects.
async fn streams_handler(State(state): State<Arc<AppState>>) -> Json<Vec<Value>> {
    let streams = streams(&state)
        .into_iter()
        .map(|(name, app, codec)| {
            let codec_string = match codec {
                Codec::H264 => "avc1.640033",
                Codec::Hevc => "hev1.1.6.L153.B0",
                Codec::Av1 => "av01.0.13M.08",
            };
            json!({
                "name": name,
                "codec": codec.name(),
                "codecString": codec_string,
                "width": state.width,
                "height": state.height,
                "chroma": "420",
                "fps": 60,
                "bitrateMbps": state.mbps,
                "frames": 0,
                "content": app.title,
            })
        })
        .collect();
    Json(streams)
}

async fn media_offer_handler(
    State(state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
    Json(offer): Json<SdpOffer>,
) -> Result<Json<SdpAnswer>, (StatusCode, String)> {
    let query = query.unwrap_or_default();
    let bad_request = |err: anyhow::Error| (StatusCode::BAD_REQUEST, format!("{err:#}"));
    let name = query_value(&query, "name").ok_or_else(|| bad_request(anyhow!("missing ?name=")))?;
    let (_, app, codec) = streams(&state)
        .into_iter()
        .find(|(n, _, _)| n == name)
        .ok_or_else(|| bad_request(anyhow!("unknown stream {name}")))?;
    let number = |key: &str, default: u32| {
        query_value(&query, key)
            .and_then(|v| v.parse::<f64>().ok())
            .map_or(default, |v| v as u32)
    };
    let params = session::SessionParams {
        request: StreamRequest {
            app,
            codec,
            width: state.width,
            height: state.height,
            fps: number("fps", 60),
            bitrate_kbps: number("mbps", state.mbps) * 1000,
        },
        secs: number("secs", 15),
        host: query_value(&query, "host")
            .map(|h| if h == "localhost" { "127.0.0.1" } else { h })
            .and_then(|h| h.parse().ok())
            .unwrap_or(state.primary),
        port: state.webrtc_port,
        gateway: Arc::clone(&state.gateway),
    };
    let answer = session::start(params, offer).await.map_err(bad_request)?;
    Ok(Json(answer))
}

/// Every (stream name, app, codec) the gateway offers.
fn streams(state: &AppState) -> Vec<(String, App, Codec)> {
    let mut out = Vec::new();
    for app in &state.gateway.apps {
        let slug: String = app
            .title
            .to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        for codec in Codec::STREAMABLE {
            out.push((format!("{slug}-{}", codec.name()), app.clone(), codec));
        }
    }
    out
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
