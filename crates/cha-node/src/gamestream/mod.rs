//! This node's GameStream host (ADR 0009, G3): what stock Moonlight clients
//! pair with and stream from.
//!
//! The protocol is `cha-gamestream`'s; this module adapts it to the node.
//! One host per node, run by the agent: it holds the host's identity
//! ([`identity`]), advertises over mDNS and serves nvhttp, pairing and RTSP.
//! The apps a client lists are its owner's running environments, and a
//! session's media is served by that environment's streamer ([`directory`]).
//! Who is paired is the portal's to say ([`pairing`]): a device pairs with a
//! PIN a signed-in user types in the portal, and the node keeps the list it
//! is sent. Everything GameStream on the node is in here, behind the
//! `gamestream` cargo feature; the agent's connection hands it the portal's
//! requests ([`GameStream::handle`]) and carries what it says back
//! ([`GameStream::outgoing`]).

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
pub use cha_gamestream::Ports;
use cha_gamestream::{Capabilities, Host, HostConfig, RunningHost, VideoCodec};
use cha_wire::{GameStreamInfo, NodeRequest, NodeResponse, ToPortal};
use tokio::sync::broadcast;
use tracing::info;

pub mod directory;
pub mod identity;
pub mod pairing;
mod streamer;

pub use directory::Environments;
use directory::NodeDirectory;
use pairing::Pairings;

/// What the host is started with.
#[derive(Clone, Debug)]
pub struct Config {
    /// What clients call the host: the node's name.
    pub name: String,
    pub ports: Ports,
    /// The agent's data root: the host's identity lives under it.
    pub data_root: PathBuf,
    /// The address to listen on.
    pub bind: IpAddr,
    /// Advertise over mDNS so clients find the host.
    pub mdns: bool,
    /// What the node's encoders make.
    pub codecs: Vec<VideoCodec>,
    /// How often a session's media is asked whether it still runs.
    pub poll: Duration,
}

impl Config {
    /// The usual host: every address, on `ports`, advertised, with the codecs
    /// this machine's GPU encodes.
    pub fn new(name: impl Into<String>, data_root: PathBuf, ports: Ports) -> Self {
        Self {
            name: name.into(),
            ports,
            data_root,
            bind: IpAddr::from([0, 0, 0, 0]),
            mdns: true,
            codecs: codecs_here(),
            poll: directory::POLL,
        }
    }
}

/// The codecs the first device with encoders makes (the streamers use it);
/// H.264 and HEVC when the probe finds none, which most clients can take.
fn codecs_here() -> Vec<VideoCodec> {
    let found: Vec<VideoCodec> = crate::inventory::collect()
        .devices_or_derived()
        .iter()
        .flat_map(|d| d.codecs.iter())
        .filter_map(|c| match c.as_str() {
            "h264" => Some(VideoCodec::H264),
            "hevc" => Some(VideoCodec::Hevc),
            "av1" => Some(VideoCodec::Av1),
            _ => None,
        })
        .fold(Vec::new(), |mut all, c| {
            if !all.contains(&c) {
                all.push(c);
            }
            all
        });
    if found.is_empty() {
        vec![VideoCodec::H264, VideoCodec::Hevc]
    } else {
        found
    }
}

/// The running host and what the agent needs of it.
pub struct GameStream {
    pairings: Arc<Pairings>,
    out: broadcast::Sender<ToPortal>,
    info: GameStreamInfo,
    unique_id: String,
    host: RunningHost,
}

impl GameStream {
    /// Loads or makes the host's identity, binds the ports and starts serving.
    pub async fn start(config: Config, environments: Arc<dyn Environments>) -> Result<Arc<Self>> {
        let dir = identity::dir(&config.data_root);
        let identity = {
            let dir = dir.clone();
            tokio::task::spawn_blocking(move || identity::load_or_create(&dir)).await??
        };
        let (out, _) = broadcast::channel(64);
        let pairings = Pairings::new(out.clone(), Some(&dir));
        let directory = NodeDirectory::new(environments, pairings.clone(), config.poll);

        let mut host = HostConfig::new(&config.name, &identity.unique_id);
        host.bind = config.bind;
        host.ports = config.ports;
        host.mdns = config.mdns;
        host.pin_timeout = pairing::ATTEMPT_TTL;
        host.capabilities = Capabilities {
            codecs: config.codecs,
            hdr: false,
            yuv444: false,
        };
        let running = Host::builder(host)
            .identity(identity.identity)
            .directory(directory.clone())
            .pairing_store(pairings.clone())
            .build()
            .context("building the GameStream host")?
            .start()
            .await
            .context(
                "starting the GameStream host (is Sunshine or another GameStream host using \
                 its ports? see CHA_GAMESTREAM_HTTP_PORT, _HTTPS_PORT and _RTSP_PORT)",
            )?;
        directory.set_handle(running.handle());
        let addrs = running.addrs();
        info!(
            name = %config.name,
            http = addrs.http.port(),
            https = addrs.https.port(),
            rtsp = addrs.rtsp.port(),
            "GameStream host up: Moonlight clients can add this node"
        );
        Ok(Arc::new(Self {
            pairings,
            out,
            info: GameStreamInfo {
                http_port: addrs.http.port(),
                name: config.name,
            },
            unique_id: identity.unique_id,
            host: running,
        }))
    }

    /// What the node tells the portal in its inventory.
    pub fn info(&self) -> GameStreamInfo {
        self.info.clone()
    }

    /// The host's `uniqueid`, as clients (and a Moonlight browse on this
    /// node) see it.
    pub fn unique_id(&self) -> &str {
        &self.unique_id
    }

    pub fn addrs(&self) -> cha_gamestream::front::HostAddrs {
        self.host.addrs()
    }

    /// What the host says to the portal (pairing requests and outcomes). One
    /// connection listens at a time; what is said while none does is dropped.
    pub fn outgoing(&self) -> broadcast::Receiver<ToPortal> {
        self.out.subscribe()
    }

    /// A request from the portal that is the host's: a PIN, or the paired
    /// devices.
    pub fn handle(&self, request: NodeRequest) -> Result<NodeResponse, String> {
        match request {
            NodeRequest::GameStreamPin {
                attempt_id,
                pin,
                user_id,
            } => self
                .pairings
                .submit_pin(&attempt_id, &pin, &user_id)
                .map(|()| NodeResponse::Accepted),
            NodeRequest::GameStreamDevices { devices } => {
                self.pairings.set_devices(devices);
                Ok(NodeResponse::Accepted)
            }
            other => Err(format!("not a GameStream request: {other:?}")),
        }
    }
}

/// What `--doctor` says of the host: whether its ports are free, and how many
/// devices the portal last said are paired.
pub fn doctor(config: &Config) -> Vec<crate::doctor::Check> {
    use crate::doctor::{Level, check};
    let mut checks = Vec::new();
    let taken: Vec<String> = [
        ("HTTP", config.ports.http),
        ("HTTPS", config.ports.https),
        ("RTSP", config.ports.rtsp),
    ]
    .into_iter()
    .filter(|(_, port)| std::net::TcpListener::bind((config.bind, *port)).is_err())
    .map(|(what, port)| format!("{what} {port}"))
    .collect();
    checks.push(if taken.is_empty() {
        check(
            Level::Ok,
            "GameStream host",
            format!(
                "Moonlight clients will find \"{}\" on TCP {}, {} and {}",
                config.name, config.ports.http, config.ports.https, config.ports.rtsp
            ),
        )
    } else {
        check(
            Level::Warn,
            "GameStream host",
            format!(
                "{} (TCP) is taken: by Sunshine, which uses the same ports, by another \
                 GameStream host, or by the agent that is running now",
                taken.join(", ")
            ),
        )
        .fix(
            "stop Sunshine, or move this host with CHA_GAMESTREAM_HTTP_PORT, \
             CHA_GAMESTREAM_HTTPS_PORT and CHA_GAMESTREAM_RTSP_PORT",
        )
    });
    let dir = identity::dir(&config.data_root);
    checks.push(match pairing::snapshot_count(&dir) {
        Some(n) => check(
            Level::Info,
            "GameStream devices",
            format!("{n} paired, as the portal last said (they are kept in the portal)"),
        ),
        None => check(
            Level::Info,
            "GameStream devices",
            "none known yet: the portal sends the list when the agent connects",
        ),
    });
    checks
}
