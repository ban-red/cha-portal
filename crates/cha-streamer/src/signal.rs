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
use cha_nvenc::Codec;
use clap::Parser;
use serde_json::{Value, json};
use str0m::change::{SdpAnswer, SdpOffer};
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

use crate::audio::Audio;
use crate::codec::VideoCodec;
use crate::compositor::{self, fit_size};
use crate::device::{self, Device, DeviceKind};
use crate::gamepad::{GamepadKind, Gamepads};
use crate::media::{EncodeSettings, FrameHub, Media};
use crate::net;
use crate::overlay::Overlay;
use crate::pyro::PyroSettings;
use crate::session;
use crate::status::SetupStatus;
use crate::uinput_broker::UinputBroker;
use crate::viewers::{Role, Viewer, Viewers};
use crate::wt;
use crate::x11_clipboard::X11Clipboard;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "One environment's media engine: compositor -> encoder -> WebRTC"
)]
struct Args {
    /// What composites and encodes: `nvidia` (the GPU: NVENC, zero-copy),
    /// `vaapi` (an Intel or AMD GPU; encoding isn't built yet) or `cpu`
    /// (Mesa's llvmpipe, x264 and SVT-AV1: H.264 and AV1, for desktops at modest sizes).
    #[arg(long, value_enum, default_value_t = DeviceKind::Nvidia)]
    device: DeviceKind,
    /// The GPU's render node (`nvidia` and `vaapi`).
    #[arg(long, default_value = "/dev/dri/renderD128")]
    render_node: PathBuf,
    #[arg(long, default_value_t = 2560)]
    width: u32,
    #[arg(long, default_value_t = 1440)]
    height: u32,
    /// Encoded (sent) frames per second at the start: 60 (the baseline), 90
    /// or 120. A page can change it while the stream runs.
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// Frame callbacks per second, at least; apps draw at this rate. Higher
    /// cuts input latency (S2: 240 Hz halves click -> screen); only `--fps`
    /// of them are composited and encoded. Rounded up to a multiple of
    /// `--fps`, so each encoded frame falls on a tick (270 for 90).
    #[arg(long, default_value_t = 240)]
    compositor_fps: u32,
    /// The NVENC codecs' bitrate in Mbit/s at 60 fps: their starting and
    /// target rate, scaled by (fps / 60)^0.75 at other rates.
    #[arg(long, default_value_t = 40)]
    mbps: u32,
    /// Codecs offered, of those the device can make: H.264, HEVC and AV1 on
    /// NVIDIA (VA-API: what its driver encodes), H.264 on the CPU. PyroWave
    /// (NVIDIA, WebTransport only) needs libpyrowave and is left out without
    /// it.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "hevc,h264,av1,pyrowave420,pyrowave444"
    )]
    codecs: Vec<String>,
    /// PyroWave's budget at 4:2:0, 1440p and 60 fps (4:4:4 gets twice as
    /// much); scaled with the picture's area. It is per frame, so the rate
    /// grows with the frame rate, to at most 600 Mbit/s.
    #[arg(long, default_value_t = 290)]
    pyrowave_mbps: u32,
    /// The Wayland socket's name in $XDG_RUNTIME_DIR.
    #[arg(long, default_value = "wayland-0")]
    socket: String,
    /// A command to run as the compositor's client (through `sh -c`), restarted
    /// when it exits.
    #[arg(long)]
    run: Option<String>,
    /// Where signalling (HTTP) listens. The agent runs streamers on
    /// 127.0.0.1: browsers reach them through the portal, not directly.
    /// Pass `--listen 0.0.0.0` to open it to the LAN (the dev loop).
    #[arg(long, default_value = "127.0.0.1")]
    listen: IpAddr,
    /// Signalling (HTTP) port.
    #[arg(long, default_value_t = 7660)]
    http_port: u16,
    /// WebRTC UDP port (ICE-lite).
    #[arg(long, default_value_t = 7661)]
    webrtc_port: u16,
    /// WebTransport UDP port (`cha-stream/1`, the Chromium fast path); 0 for
    /// none.
    #[arg(long, default_value_t = 7662)]
    wt_port: u16,
    /// GameStream (Moonlight) media ports, `video,control,audio` (UDP): serve
    /// a Moonlight session of this environment on them, started through the
    /// local `/gamestream/*` API with the secret in `CHA_GAMESTREAM_SECRET`.
    #[cfg(feature = "gamestream")]
    #[arg(long, value_name = "VIDEO,CONTROL,AUDIO", value_parser = crate::gamestream::parse_ports)]
    gamestream_ports: Option<cha_gamestream::MediaPorts>,
    #[cfg(feature = "gamestream")]
    #[arg(skip = crate::gamestream::take_secret())]
    gamestream_secret: Option<crate::gamestream::Secret>,
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
    /// The virtual controller the pads are. `dualsense` and `steam` need
    /// `--uhid` and the host's `hid-playstation` or `hid-steam`; without them
    /// the pads are Xbox 360 ones.
    #[arg(long, value_enum, default_value_t = GamepadKind::Xbox360)]
    pad_kind: GamepadKind,
    /// The uhid device for the DualSense and Steam Controller kinds; empty for
    /// none.
    #[arg(long, default_value = "/dev/uhid")]
    uhid: PathBuf,
    /// Gamepads made at start, for apps that look for pads only once.
    #[arg(long, default_value_t = 1)]
    gamepads: usize,
    /// Make PyroWave's Vulkan device on the render node's GPU, say how that
    /// went and exit (the node doctor runs this).
    #[arg(long)]
    probe_pyrowave: bool,
    /// Print what a device offers as JSON (`{kind, name, vendor?, renderNode?,
    /// codecs, cores?}`) and exit, for `nvidia`, `vaapi:/dev/dri/renderD129` or
    /// `cpu` (the node runs this to find a machine's devices).
    #[arg(long, value_name = "KIND[:RENDER NODE]")]
    probe_device: Option<String>,
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
    /// The sessions watching, and which one has the controls (plan §3.4).
    viewers: Arc<Viewers>,
    webrtc_port: u16,
    /// The WebRTC sockets, shared by all WebRTC sessions.
    hubs: Arc<crate::rtc_hub::Hubs>,
    hosts: Vec<IpAddr>,
    public: Vec<IpAddr>,
    /// WebTransport: its port (0 for none) and certificate's SHA-256.
    wt_port: u16,
    cert_hash_hex: String,
}

pub fn main() -> Result<()> {
    let args = Args::parse();
    // Before logging starts: stdout is the JSON and nothing else.
    if let Some(spec) = &args.probe_device {
        return device::probe(spec, cha_pyrowave::available().is_ok());
    }
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,str0m=warn,smithay=warn".into()),
        )
        .init();
    str0m::crypto::from_feature_flags().install_process_default();

    let mut codecs = args
        .codecs
        .iter()
        .map(|c| VideoCodec::from_name(c).ok_or_else(|| anyhow!("unknown codec {c}")))
        .collect::<Result<Vec<_>>>()?;
    // Only NVIDIA has PyroWave; the other devices don't look at the render
    // node for it (the CPU device touches no GPU files at all).
    let pyro_ids = (args.device == DeviceKind::Nvidia).then(|| pci_ids(&args.render_node));
    let pyrowave = match (cha_pyrowave::available(), pyro_ids) {
        (_, None) => {
            info!("no PyroWave: it needs the NVIDIA device");
            None
        }
        (Ok(()), Some(Some((vendor, device)))) => {
            Some(PyroSettings::new(vendor, device, args.pyrowave_mbps))
        }
        (Err(err), _) => {
            info!("no PyroWave: {err}");
            None
        }
        (_, Some(None)) => {
            info!("no PyroWave: the render node's PCI ids are unknown");
            None
        }
    };
    if args.probe_pyrowave {
        return probe_pyrowave(pyrowave);
    }
    if pyrowave.is_none() {
        codecs.retain(|c| c.hw().is_some());
    }
    let pyrowave = pyrowave.filter(|_| codecs.iter().any(|c| c.hw().is_none()));
    if let Some(settings) = &pyrowave {
        settings.warm();
    }
    let runtime_dir =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR must be set")?);
    ensure_runtime_dir(&runtime_dir, args.app_uid)?;
    let (width, height) = fit_size(args.width, args.height);

    if args.device == DeviceKind::Nvidia {
        crate::system::set_gpu_slot(device::pci_slot(&args.render_node));
    }
    let device = Device::open(
        args.device,
        (args.device != DeviceKind::Cpu).then_some(args.render_node.as_path()),
    )?;
    anyhow::ensure!(
        device.encodes(),
        "--device {}: encoding on {} isn't built yet (the compositor and the probe are)",
        args.device.name(),
        device.name()
    );
    codecs = device::intersect(args.device, device.codecs(), &codecs, pyrowave.is_some());
    anyhow::ensure!(
        !codecs.is_empty(),
        "none of --codecs can be made on the {} device ({}; it makes {})",
        args.device.name(),
        device.name(),
        device
            .codecs()
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    info!(
        device = args.device.name(),
        name = device.name(),
        codecs = codecs
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>()
            .join(","),
        "device ready"
    );
    let hub = Arc::new(FrameHub::default());
    let handle = compositor::spawn(
        compositor::Config {
            device: Arc::clone(&device),
            width,
            height,
            compositor_fps: args.compositor_fps,
            encode_fps: args.fps,
            socket_name: args.socket.clone(),
        },
        Arc::clone(&hub),
    )?;
    if let Some(uid) = args.app_uid {
        let socket = runtime_dir.join(&handle.socket_name);
        std::os::unix::fs::chown(&socket, Some(uid), Some(uid))
            .with_context(|| format!("handing {} to uid {uid}", socket.display()))?;
    }
    // X11 apps (XFCE) keep their clipboard in the X server: a helper in the
    // app's container bridges it over this socket.
    let x11_clipboard = match X11Clipboard::start(
        &runtime_dir.join("clipboard"),
        args.app_uid,
        handle.clipboard_publisher.clone(),
    ) {
        Ok(bridge) => Some(bridge),
        Err(err) => {
            warn!("no clipboard for X11 apps: {err:#}");
            None
        }
    };
    let media = Arc::new(
        Media::new(
            hub,
            &handle,
            device,
            EncodeSettings {
                fps: args.fps,
                bitrate_bps: args.mbps * 1_000_000,
            },
            codecs,
            pyrowave,
            (width, height),
        )
        .with_x11_clipboard(x11_clipboard)
        // The app's performance overlay, if it has a config file.
        .with_overlay(Overlay::at(runtime_dir.join("mangohud.conf"), args.app_uid))
        // The app's setup progress (Steam's download), in the shared volume.
        .with_setup_status(SetupStatus::start(runtime_dir.join("status"))),
    );
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
        match Gamepads::new(
            &args.uinput,
            &args.uhid,
            args.pad_kind,
            dir,
            args.app_uid,
            args.gamepads,
        ) {
            Ok(pads) => Some(Arc::new(pads)),
            Err(err) => {
                warn!("no gamepads: {err:#}");
                None
            }
        }
    });
    // Steam makes its virtual Xbox pad through a fake /dev/uinput (the image's
    // LD_PRELOAD shim), which this serves; it lives as long as the streamer.
    let _uinput_broker = gamepads
        .as_ref()
        .filter(|pads| pads.kind() == GamepadKind::Steam)
        .and_then(|pads| {
            let socket = runtime_dir.join(crate::uinput_proto::SOCKET_NAME);
            match UinputBroker::start(Arc::clone(pads), &socket, args.app_uid) {
                Ok(broker) => Some(broker),
                Err(err) => {
                    warn!("no uinput for Steam: {err:#}");
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
    let names: Vec<IpAddr> = hosts.iter().chain(&args.public_address).copied().collect();
    let wt = if args.wt_port == 0 {
        None
    } else {
        let _runtime = runtime.enter();
        match wt::bind(args.wt_port, &names) {
            Ok(bound) => Some(bound),
            Err(err) => {
                warn!("no WebTransport: {err:#}");
                None
            }
        }
    };
    let viewers = {
        let media = Arc::clone(&media);
        let gamepads = gamepads.clone();
        Viewers::new(Box::new(move || {
            media.set_client_cursor(false);
            // What the old controller still holds (a key, a stick) would
            // stay held forever: its page can't send the release.
            media.release_input();
            if let Some(pads) = &gamepads {
                pads.release_all();
            }
        }))
    };
    let state = Arc::new(AppState {
        media,
        audio,
        gamepads,
        auth,
        viewers,
        webrtc_port: args.webrtc_port,
        hubs: Arc::default(),
        hosts,
        public: args.public_address.clone(),
        wt_port: if wt.is_some() { args.wt_port } else { 0 },
        cert_hash_hex: wt
            .as_ref()
            .map(|(_, hash)| hash.clone())
            .unwrap_or_default(),
    });
    if let Some((endpoint, _)) = wt {
        let authorizing = Arc::clone(&state);
        let sessions = Arc::new(wt::Sessions {
            media: Arc::clone(&state.media),
            audio: state.audio.clone(),
            gamepads: state.gamepads.clone(),
            authorize: Box::new(move |token| authorize(&authorizing.auth, token)),
            viewers: Arc::clone(&state.viewers),
        });
        runtime.spawn(wt::serve(endpoint, sessions));
    }
    #[cfg(feature = "gamestream")]
    let gamestream = args
        .gamestream_ports
        .map(|ports| {
            crate::gamestream::start(
                ports,
                args.gamestream_secret,
                crate::gamestream::Engine {
                    media: Arc::clone(&state.media),
                    audio: state.audio.clone(),
                    gamepads: state.gamepads.clone(),
                    viewers: Arc::clone(&state.viewers),
                },
            )
        })
        .transpose()?;
    let app = Router::new()
        .route("/info", get(info_handler))
        .route("/streams", get(streams_handler))
        .route("/webrtc/media", post(media_offer_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);
    #[cfg(feature = "gamestream")]
    let app = match gamestream {
        Some(routes) => app.merge(routes),
        None => app,
    };

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

/// The render node's PCI vendor and device ids (PyroWave picks its GPU by them).
fn pci_ids(render_node: &Path) -> Option<(u32, u32)> {
    let name = render_node.file_name()?.to_str()?;
    let read = |file: &str| {
        let text = std::fs::read_to_string(format!("/sys/class/drm/{name}/device/{file}")).ok()?;
        u32::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()
    };
    Some((read("vendor")?, read("device")?))
}

fn probe_pyrowave(settings: Option<PyroSettings>) -> Result<()> {
    let settings = settings.context("PyroWave isn't available (see above)")?;
    settings.device().map_err(|e| anyhow!("{e}"))?;
    println!(
        "PyroWave ready on {:04x}:{:04x}",
        settings.vendor_id, settings.device_id
    );
    Ok(())
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
        "gateway": true,
        "input": true,
        "audio": state.audio.is_some(),
        "gamepads": state.gamepads.is_some(),
        "pad_kind": state.gamepads.as_ref().map(|p| p.kind()),
        // The nodes of the uhid pads, which the node mounts into the app.
        "hidraw": state.gamepads.as_ref().map(|p| p.hidraw()).unwrap_or_default(),
        "token": true,
        "width": width,
        "height": height,
        "resize": true,
        "fps": state.media.fps(),
        // Sessions now, and how many are in use (a browser's hidden, silent tab
        // isn't; see `presence`), and for how long none have been: the portal
        // stops an idle environment (`settings.rs` in cha-control).
        "viewers": state.viewers.count(),
        "active_viewers": state.viewers.active_count(),
        "idle_secs": state.viewers.idle_secs(),
        "wt_port": state.wt_port,
        "cert_hash_hex": state.cert_hash_hex,
        "addresses": state.hosts.iter().chain(&state.public).collect::<Vec<_>>(),
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
                    "codecString": match codec.hw() {
                        Some(Codec::H264) => "avc1.640033",
                        Some(Codec::Hevc) => "hev1.1.6.L153.B0",
                        Some(Codec::Av1) => "av01.0.13M.08",
                        None => "pyrowave",
                    },
                    "width": width,
                    "height": height,
                    "chroma": "420",
                    "fps": state.media.fps(),
                    "bitrateMbps": state.media.bitrate_bps() / 1_000_000,
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
    let viewer = authorize(&state.auth, presented).map_err(|why| (StatusCode::FORBIDDEN, why))?;
    let bad_request = |err: anyhow::Error| (StatusCode::BAD_REQUEST, format!("{err:#}"));
    let offer: SdpOffer = serde_json::from_str(&body).map_err(|e| bad_request(e.into()))?;
    let name = query_value(&query, "name").ok_or_else(|| bad_request(anyhow!("missing ?name=")))?;
    // WebRTC carries the hardware codecs only.
    let codec = name
        .strip_prefix("live-")
        .and_then(Codec::from_name)
        .filter(|c| state.media.codecs().contains(&VideoCodec::Hw(*c)))
        .ok_or_else(|| bad_request(anyhow!("unknown stream {name}")))?;
    // WebRTC sessions share one set of sockets; they and WebTransport ones
    // coexist, up to the viewer limit.
    let seat = state
        .viewers
        .join(viewer, true)
        .await
        .map_err(|why| (StatusCode::SERVICE_UNAVAILABLE, why))?;
    let id = seat.id;
    let params = session::SessionParams {
        codec,
        secs: query_value(&query, "secs")
            .and_then(|v| v.parse::<f64>().ok())
            .map_or(0, |v| v as u32),
        hosts: match query_value(&query, "host")
            .map(|h| if h == "localhost" { "127.0.0.1" } else { h })
            .and_then(|h| h.parse().ok())
        {
            Some(host) => vec![host],
            None => state.hosts.clone(),
        },
        public: state.public.clone(),
        port: state.webrtc_port,
        hubs: Arc::clone(&state.hubs),
        media: Arc::clone(&state.media),
        audio: state.audio.clone(),
        gamepads: state.gamepads.clone(),
    };
    let (answer, running) = session::start(params, seat, offer)
        .await
        .map_err(bad_request)?;
    state.viewers.attach(id, running);
    Ok(Json(answer))
}

/// Who `presented` lets in; the error says why not.
fn authorize(auth: &Auth, presented: &str) -> std::result::Result<Viewer, String> {
    match auth {
        Auth::Token(token) => {
            if !constant_time_eq(presented.as_bytes(), token.as_bytes()) {
                // The length (never the value) tells a mistyped token from a
                // truncated or padded one.
                warn!(
                    presented_chars = presented.len(),
                    expected_chars = token.len(),
                    "rejected a stream request: missing or wrong token"
                );
                return Err("missing or wrong token".into());
            }
            // The shared token (benchmarks, the dev loop) is the owner's.
            Ok(Viewer {
                user: "token".into(),
                role: Role::Owner,
            })
        }
        Auth::Portal { key, environment } => {
            match cha_wire::verify_media_token(key, presented, environment, unix_now()) {
                Ok(claims) => {
                    info!(user = %claims.sub, role = %claims.role, "media token accepted");
                    Ok(Viewer {
                        role: Role::from_claims(&claims.role, claims.slot),
                        user: claims.sub,
                    })
                }
                Err(err) => {
                    warn!("rejected a stream request: media token {err}");
                    Err(format!("media token {err}"))
                }
            }
        }
    }
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
