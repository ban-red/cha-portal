//! Environments on this node (plan §4.2). Each one is:
//! - a **streamer** container (`cha-streamer`): our compositor, NVENC (or
//!   VA-API, or x264 in software) and WebRTC, on the host network so its WebRTC
//!   port is the node's;
//! - an **app** container from the template's image: uid 1000 plus the render
//!   node's group, no capabilities, no privilege gain, Docker's seccomp profile
//!   (or the `browser` one, which lets browser sandboxes create namespaces);
//! - the **device** it runs on (`docs/devices.md`, [`EnvironmentSpec::device`]):
//!   `nvidia` (the default) gives both containers the GPU through CDI; `vaapi`
//!   gives both one render node and its group instead; `cpu` gives them no GPU.
//! - a volume both mount at `/run/cha`, holding the Wayland and sound
//!   sockets;
//! - with gamepads (`/dev/uinput` on the host, and `/dev/uhid` for the kinds
//!   made through it): the streamer makes virtual pads and shares their device
//!   nodes and udev entries through two more volumes, the app's `/dev/input`
//!   and `/run/udev` (read-only); the app may open input devices (major 13),
//!   and only these exist in its `/dev/input`. A DualSense or Steam Controller
//!   also has `hidraw` nodes, which the streamer makes in that volume under
//!   `hidraw/` and lists in its `/info`: once it is up, each is mounted into
//!   the app at `/dev/<name>` (a subpath of the volume) and allowed in its
//!   device cgroup, and nothing else of `/dev` changes. See
//!   `docs/controllers.md`.
//! - **app data** ([`crate::storage`]), when the portal names any: a directory
//!   under the node's data root mounted as the app's `/home/cha` (the user keeps
//!   their data for this app), and one shared with other users mounted at
//!   `/srv/cha-portal/shared/<template>` (or, when the node's owner keeps a
//!   template's shared directory elsewhere, a NAS share, at that path, which
//!   the agent only looks at and never changes), with per-user directories laid
//!   over the parts of it that mustn't mix; the app is told where in
//!   `CHA_SHARED_DIR`. Unlike the volumes above these belong to the user or the
//!   app, not the environment: stopping one never removes them. A
//!   user's directory that is new is first filled from their old home volume
//!   (`cha-home-<user>-<template>`), once, or from the image's own `/home/cha`.
//!   (A portal that predates this names a home volume instead, which is
//!   mounted as it always was.) The per-user directories are mounts inside the
//!   shared one, and the kernel drops those when the directory under them
//!   changes on a server (a NAS's mover, say): the agent looks at each app's
//!   mounts every [`OVERLAY_CHECK`], and a warning goes to the portal when
//!   some are gone. The app is told them in `CHA_PER_USER_DIRS`.
//!
//! A container that dies on its own, or a start that fails, takes its logs
//! with it when the agent removes it, so the agent reads the tail of each first
//! ([`crate::crashlog`]): the exit it reports says why in a sentence, carries
//! the last lines for the owner, and the node keeps them under its state
//! directory (`logs/<environment>-<role>.log`).
//!
//! The agent is never in the media path: containers outlive agent restarts,
//! and the agent finds them again by label.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use cha_wire::{
    DeviceKind, EnvironmentSpec, HostMount, HostOptionsMode, HostPolicy, HostPort, MountSource,
    PER_USER_DIR, PortRange, Protocol, SHARED_MOUNT_ROOT, SecurityProfile, StreamerEndpoint,
    check_env, home_volume_name, is_home_volume_name, valid_template_id, valid_user_id,
};
use futures_util::future::BoxFuture;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::crashlog::{self, Tail};
use crate::devices::DeviceProbes;
use crate::docker::{
    ContainerEvent, ContainerMount, ContainerSummary, Docker, PullProgress, encode, names_registry,
};
use crate::hostopts::HostOptionsConfig;
use crate::storage::{DataRoot, Seed, plan_seed};

const LABEL_ENV: &str = "sh.cha.env";
const LABEL_ROLE: &str = "sh.cha.role";
const LABEL_HTTP_PORT: &str = "sh.cha.http-port";
/// On a streamer started with GameStream media ports (`CHA_GAMESTREAM`): its
/// `video,control,audio` ports and the secret of its `/gamestream/*` API, which
/// is how a restarted agent gets them back.
const LABEL_GAMESTREAM_PORTS: &str = "sh.cha.gamestream-ports";
const LABEL_GAMESTREAM_SECRET: &str = "sh.cha.gamestream-secret";
/// On legacy home volumes (`docker volume ls --filter label=sh.cha.home`): `1`,
/// and whose (`sh.cha.owner`) and for which template (`sh.cha.template`). On an
/// app container whose home is a directory under the data root: the same two,
/// which is how a restarted agent knows which homes are in use.
const LABEL_HOME: &str = "sh.cha.home";
const LABEL_OWNER: &str = "sh.cha.owner";
const LABEL_TEMPLATE: &str = "sh.cha.template";
/// On an app container whose home is a directory under the data root: the
/// template whose directory that is, when it isn't the environment's own
/// (a custom environment that shares its base's data, ADR 0021). Homes are
/// kept by (owner, this).
const LABEL_DATA_TEMPLATE: &str = "sh.cha.data-template";
/// Volumes made for an app's network mounts: `cha-hostvol-<environment>-<n>`.
const HOST_VOLUME_PREFIX: &str = "cha-hostvol-";
/// On an app of a `vm` environment (value `vm`), so [`DockerRuntime::remove`]
/// gives it the longer stop grace, also after the agent restarted.
const LABEL_PROFILE: &str = "sh.cha.profile";
/// On a streamer: the environment streams a Moonlight host's app (a gateway),
/// not an app beside it. Which is how a restarted agent keeps them out of
/// the GameStream host's app list.
const LABEL_GATEWAY: &str = "sh.cha.gateway";
pub const APP_UID: u32 = 1000;
/// Where a gateway container sees the node's Moonlight identity.
const GATEWAY_IDENTITY_DIR: &str = "/identity";
pub(crate) const RUNTIME_DIR: &str = "/run/cha";
/// Where the streamer puts gamepad nodes (`dev/`) and udev entries (`udev/`).
const INPUT_DIR: &str = "/run/cha-input";
const BROWSER_SECCOMP: &str = include_str!("../profiles/seccomp-browser.json");
/// The AppArmor profile for the `steam` security profile, which the owner
/// loads on the node (`deploy/node/host/apparmor/cha-sandbox`).
pub const SANDBOX_APPARMOR: &str = "cha-sandbox";
pub(crate) const APP_HOME: &str = "/home/cha";
/// Copying a home from its old volume is as long as the home is big.
const COPY_TIMEOUT: Duration = Duration::from_secs(3600);
/// Looking at a shared directory on a NAS: a hard NFS mount makes a system
/// call wait for as long as the server is away.
const EXTERNAL_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the agent looks for per-user directories that came unmounted.
const OVERLAY_CHECK: Duration = Duration::from_secs(30);
/// Each environment's streamer: HTTP (localhost), WebRTC and WebTransport.
const PORTS_PER_ENVIRONMENT: u16 = 3;
/// With GameStream on, each environment's second block: a Moonlight session's
/// video, control and audio (all UDP).
const GAMESTREAM_PORTS_PER_ENVIRONMENT: u16 = 3;
/// How long a started streamer has to answer `/info` (it makes its virtual
/// controllers first), and how often it is asked.
const STREAMER_UP_TIMEOUT: Duration = Duration::from_secs(30);
const STREAMER_UP_POLL: Duration = Duration::from_millis(200);

/// An environment that stopped on its own.
#[derive(Debug, Clone)]
pub struct Exit {
    pub id: String,
    pub detail: String,
    pub failed: bool,
    /// What its containers last logged ([`crashlog::report`]); empty when
    /// there was nothing to read.
    pub log: Vec<String>,
}

/// What a starting environment is doing, when it's worth telling the user.
#[derive(Debug, Clone)]
pub struct Progress {
    pub id: String,
    pub detail: String,
    /// How far along, when it can be counted (an image download): `done` of
    /// `total` in `unit`.
    pub done: Option<u64>,
    pub total: Option<u64>,
    pub unit: Option<&'static str>,
}

impl Progress {
    /// A step that can't be counted.
    fn note(id: &str, detail: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            detail: detail.into(),
            done: None,
            total: None,
            unit: None,
        }
    }
}

/// Something wrong with a running environment that its user should be told,
/// or (`None`) no longer wrong.
#[derive(Debug, Clone)]
pub struct Warning {
    pub id: String,
    pub warning: Option<String>,
}

/// A browser's request to connect, as the portal relayed it.
#[derive(Debug, Clone)]
pub struct Connect {
    pub environment_id: String,
    pub codec: String,
    /// The SDP offer.
    pub offer: Value,
    /// The portal's media token, which the streamer checks.
    pub media_token: String,
}

/// The codecs a streamer offers.
const CODECS: [&str; 3] = ["h264", "hevc", "av1"];

/// What the agent needs from whatever runs environments.
pub trait Runtime: Send + Sync + 'static {
    /// Starts (or finds running) an environment.
    fn start(&self, spec: EnvironmentSpec) -> BoxFuture<'_, Result<StreamerEndpoint>>;
    /// Stops and removes an environment; unknown ids are fine.
    fn stop(&self, id: String) -> BoxFuture<'_, Result<()>>;
    /// Ids of the environments running now.
    fn running(&self) -> BoxFuture<'_, Result<Vec<String>>>;
    /// Hands a browser's offer to the environment's streamer; returns its answer.
    fn connect(&self, request: Connect) -> BoxFuture<'_, Result<Value>>;
    /// The environment's streamer describes itself (`GET /info`).
    fn streamer_info(&self, environment_id: String) -> BoxFuture<'_, Result<Value>> {
        let _ = environment_id;
        Box::pin(async { bail!("this runtime can't describe its streamers") })
    }
    /// The loopback HTTP port of the environment's streamer, if it runs here
    /// (the media relay dials its `/ws/media`).
    fn streamer_http_port(&self, environment_id: &str) -> Option<u16> {
        let _ = environment_id;
        None
    }
    /// The Docker engine behind this runtime, to read what environments use;
    /// `None` from a runtime that has none.
    fn engine(&self) -> Option<Docker> {
        None
    }
    /// Environments that stop on their own from now on.
    fn exits(&self) -> broadcast::Receiver<Exit>;
    /// What starting environments say they are doing, from now on.
    fn progress(&self) -> broadcast::Receiver<Progress> {
        broadcast::channel(1).1
    }
    /// Warnings about running environments, as they come and go.
    fn warnings(&self) -> broadcast::Receiver<Warning> {
        broadcast::channel(1).1
    }
    /// The `repo:tag` names of the images this runtime holds (ADR 0017);
    /// empty from one that has none.
    fn images(&self) -> BoxFuture<'_, Vec<String>> {
        Box::pin(async { Vec::new() })
    }
    /// A signal each time an image has been pulled, so the inventory can be
    /// sent again with it.
    fn pulled(&self) -> broadcast::Receiver<()> {
        broadcast::channel(1).1
    }
    /// Where each environment that is watched stands now: a warning, or none.
    fn warnings_now(&self) -> Vec<Warning> {
        Vec::new()
    }
    /// VA-API devices on this machine: Intel and AMD render nodes the
    /// streamer image can encode with. Empty from a runtime that can't ask.
    fn vaapi_devices(&self) -> BoxFuture<'_, Vec<cha_wire::Device>> {
        Box::pin(async { Vec::new() })
    }
    /// The codecs the CPU device makes, as the streamer image says; `None`
    /// when a runtime can't ask (the inventory keeps H.264).
    fn cpu_codecs(&self) -> BoxFuture<'_, Option<Vec<String>>> {
        Box::pin(async { None })
    }
    /// Where this runtime keeps app data; `None` if it keeps none.
    fn data_root(&self) -> Option<String> {
        None
    }
    /// Shared directories kept outside the data root: template → path.
    fn shared_dirs(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
    /// Deletes what `user` keeps for `template` here ([`crate::storage`]).
    /// Refused while an environment of theirs for it runs.
    fn delete_user_data(&self, user: String, template: String) -> BoxFuture<'_, Result<()>> {
        let _ = (user, template);
        Box::pin(async { bail!("this runtime keeps no user data") })
    }
    /// What custom environments may ask of this node (ADR 0021); nothing from
    /// a runtime that has no say.
    fn host_policy(&self) -> HostPolicy {
        HostPolicy::default()
    }
    /// The ports published for the environment's host options, with the
    /// node's port filled in; empty when it has none.
    fn host_ports(&self, id: String) -> BoxFuture<'_, Vec<HostPort>> {
        let _ = id;
        Box::pin(async { Vec::new() })
    }
}

#[derive(Clone, Debug)]
pub struct DockerConfig {
    pub streamer_image: String,
    /// The render node the streamer composites on, e.g. `/dev/dri/renderD128`.
    pub render_node: String,
    /// The CDI device that gives a container the GPU.
    pub gpu_device: String,
    /// The host's uinput device, for gamepads; `None` goes without.
    pub uinput: Option<String>,
    /// The host's uhid device, for the DualSense and Steam Controller;
    /// `None` goes without (the streamer then makes Xbox 360 pads instead).
    pub uhid: Option<String>,
    /// The router's public address, when it forwards the streamers' UDP ports
    /// (`port_base + 1`, `+ 3`, …) to this node.
    pub public_address: Option<String>,
    /// Streamers listen on `port_base + 2n` (HTTP) and `+ 1` (WebRTC).
    pub port_base: u16,
    /// With GameStream on (`CHA_GAMESTREAM`, [`check_gamestream_range`]): where
    /// the environments' Moonlight media ports start, three each, in the
    /// same slots as `port_base`'s. `None` (the default) gives streamers no
    /// such ports and changes nothing else.
    pub gamestream_port_base: Option<u16>,
    pub max_environments: u16,
    /// Where app data lives (`CHA_DATA_ROOT`): a directory on the host that
    /// this agent also sees at the same path, since Docker is given host paths.
    pub data_root: PathBuf,
    /// Templates whose shared directory is somewhere else (`CHA_SHARED_DIRS`,
    /// [`crate::storage::parse_shared_dirs`]): template → absolute path, which
    /// is mounted into the app at the same path.
    pub shared_dirs: BTreeMap<String, PathBuf>,
    /// The host's directory of NVIDIA's Wine DLLs (`nvngx.dll`, for DLSS),
    /// which Proton copies into its prefixes and the CDI spec leaves out:
    /// bound into apps read-only at the same path. `None` goes without.
    pub nvidia_wine_dir: Option<PathBuf>,
    /// The memory limit of a `vm` app's container, in MiB
    /// (`CHA_VM_MEMORY_MB`, [`DEFAULT_VM_MEMORY_MB`]).
    pub vm_memory_mb: u64,
    /// Where the logs of environments that died are kept (the agent's state
    /// directory's `logs`); `None` keeps none.
    pub log_dir: Option<PathBuf>,
    /// Run the catalog's apps from their published images
    /// (`CHA_IMAGE_REGISTRY`, `CHA_IMAGE_TAG`); `None` runs the local builds
    /// the catalog names (`cha/env-chrome:dev`).
    pub app_images: Option<PublishedImages>,
    /// The image that streams a Moonlight host's app (`CHA_GATEWAY_IMAGE`),
    /// mapped through [`Self::app_images`] like an app's.
    pub gateway_image: String,
    /// What custom environments may ask of this node (`CHA_HOST_OPTIONS` and
    /// its lists, ADR 0021). Off by default.
    pub host_options: HostOptionsConfig,
}

impl DockerConfig {
    /// The image an app runs from: the catalog's name, or its published copy.
    pub fn app_image(&self, image: &str) -> String {
        self.app_images
            .as_ref()
            .and_then(|p| p.image_for(image))
            .unwrap_or_else(|| image.to_string())
    }
}

impl DockerConfig {
    /// The images an app may run from, in order (ADR 0017): the spec's
    /// candidates, or its one `image` from a portal that predates them. Each
    /// has `{version}` filled in and the published mapping applied.
    pub fn app_candidates(&self, spec: &EnvironmentSpec, version: &str) -> Vec<String> {
        let listed: &[String] = if spec.image_candidates.is_empty() {
            std::slice::from_ref(&spec.image)
        } else {
            &spec.image_candidates
        };
        let mut out: Vec<String> = Vec::new();
        for candidate in listed {
            let image = self.app_image(&candidate.replace("{version}", version));
            if !out.contains(&image) {
                out.push(image);
            }
        }
        out
    }
}

/// The streamer images to try: the configured one, then the published copy
/// for this release, unless the configured one already names a registry.
pub fn streamer_candidates(configured: &str, version: &str) -> Vec<String> {
    let mut out = vec![configured.to_string()];
    if !names_registry(configured) {
        out.push(format!("{PUBLISHED_STREAMER}:{version}"));
    }
    out
}

/// Where the release's streamer image is published
/// (`.github/workflows/publish.yml`).
const PUBLISHED_STREAMER: &str = "ghcr.io/ban-red/cha-streamer";

/// Who every app runs as (`uid:gid`).
pub(crate) fn app_user() -> String {
    format!("{APP_UID}:{APP_UID}")
}

/// The environment variables every app gets from the agent, before the ones
/// its storage adds.
pub(crate) fn app_env(width: u32, height: u32, fps: u32) -> Vec<String> {
    vec![
        format!("XDG_RUNTIME_DIR={RUNTIME_DIR}"),
        "WAYLAND_DISPLAY=wayland-0".to_string(),
        format!("CHA_WIDTH={width}"),
        format!("CHA_HEIGHT={height}"),
        format!("CHA_REFRESH={fps}"),
    ]
}

/// The app container's confinement and limits for `security`: capabilities,
/// privilege gain, seccomp, AppArmor, descriptors, shared memory, init. What
/// the launch and `--check-image` both run an app under.
pub(crate) fn app_host_config(security: SecurityProfile, shm_mb: u32, vm_memory_mb: u64) -> Value {
    let mut options = vec!["no-new-privileges".to_string()];
    if matches!(security, SecurityProfile::Browser | SecurityProfile::Steam) {
        options.push(format!("seccomp={}", compact(BROWSER_SECCOMP)));
    }
    if security == SecurityProfile::Steam {
        options.push(format!("apparmor={SANDBOX_APPARMOR}"));
    }
    // Proton's esync wants many descriptors.
    let ulimits = if security == SecurityProfile::Steam {
        json!([{ "Name": "nofile", "Soft": 524288, "Hard": 524288 }])
    } else {
        json!([])
    };
    let mut host = json!({
        "Ulimits": ulimits,
        "CapDrop": ["ALL"],
        "SecurityOpt": options,
        "ShmSize": u64::from(shm_mb) * 1024 * 1024,
        "RestartPolicy": { "Name": "no" },
        "Init": true,
    });
    if security == SecurityProfile::Vm {
        // A guest's RAM plus QEMU's own must not be able to push the shared
        // host into memory exhaustion: an earlier test froze a node. Swap is
        // capped to the same figure, so the container can't spill past it.
        let bytes = vm_memory_mb * 1024 * 1024;
        host["Memory"] = json!(bytes);
        host["MemorySwap"] = json!(bytes);
    }
    host
}

/// What a `vm` app's container may use of the guest's RAM and QEMU's, when
/// `CHA_VM_MEMORY_MB` doesn't say.
pub const DEFAULT_VM_MEMORY_MB: u64 = 12 * 1024;

/// How long a `vm` app has to stop on SIGTERM, so the guest can shut down
/// (Windows with a browser open took over 25 s); other apps get
/// [`STOP_GRACE_SECS`]. Under the portal's stop timeout (120 s).
const VM_STOP_GRACE_SECS: u32 = 90;
const STOP_GRACE_SECS: u32 = 5;

/// The host's KVM device, which `vm` apps run their guests on.
pub(crate) const KVM_DEVICE: &str = "/dev/kvm";
/// The host's udmabuf device, for zero-copy buffers between the guest's
/// display and the compositor; a `vm` app gets it when the host has it.
pub(crate) const UDMABUF_DEVICE: &str = "/dev/udmabuf";
/// How the agent's container sees the host has them (sysfs is the host's).
const KVM_SYSFS: &str = "/sys/class/misc/kvm";
const UDMABUF_SYSFS: &str = "/sys/class/misc/udmabuf";

/// Seconds a container with these labels has to stop on SIGTERM.
fn stop_grace(labels: &std::collections::HashMap<String, String>) -> u32 {
    match labels.get(LABEL_PROFILE).map(String::as_str) {
        Some("vm") => VM_STOP_GRACE_SECS,
        _ => STOP_GRACE_SECS,
    }
}

/// The devices a `vm` app is given, read-write: KVM, and udmabuf when
/// `udmabuf` (the host has it). Nothing for other profiles.
fn vm_devices(spec: &EnvironmentSpec, udmabuf: bool) -> Vec<Value> {
    if spec.security != SecurityProfile::Vm {
        return Vec::new();
    }
    let mut paths = vec![KVM_DEVICE];
    if udmabuf {
        paths.push(UDMABUF_DEVICE);
    }
    paths
        .into_iter()
        .map(|path| {
            json!({
                "PathOnHost": path,
                "PathInContainer": path,
                "CgroupPermissions": "rw",
            })
        })
        .collect()
}

/// The release this agent is, for the `{version}` in image names.
pub(crate) const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The first of `candidates` the engine has, else the first that names a
/// registry, pulled (`on_progress` hears how it goes, starting with nothing
/// known). Whether it pulled comes with the image. Fails naming every
/// candidate it tried.
async fn resolve_image(
    docker: &Docker,
    role: &str,
    candidates: &[String],
    mut on_progress: impl FnMut(&str, PullProgress),
) -> Result<(String, bool)> {
    for image in candidates {
        if docker.image_exists(image).await? {
            info!(%image, %role, "using the image the engine has");
            return Ok((image.clone(), false));
        }
    }
    let tried = candidates.join(", ");
    let Some(image) = candidates.iter().find(|i| names_registry(i)) else {
        bail!(
            "no image for the {role} is on this node (tried {tried}): build it (`docker compose -f images/compose.yaml build`, and for the streamer `docker build -f deploy/streamer/Dockerfile --target runtime -t cha/streamer:dev .`)"
        );
    };
    info!(%image, %role, %tried, "pulling the image");
    on_progress(image, PullProgress::default());
    let started = std::time::Instant::now();
    let size = docker
        .pull_with(image, |p| on_progress(image, p))
        .await
        .with_context(|| {
            format!("{image} isn't on this node and couldn't be pulled (tried {tried})")
        })?;
    info!(%image, %role, secs = started.elapsed().as_secs_f32(), bytes = size.total, "pulled the image");
    Ok((image.clone(), true))
}

/// What an image is called to the user: `ghcr.io/ban-red/cha-env-chrome:0.1.0`
/// and `cha/env-chrome:dev` are "Chrome", `cha-env-test-pattern` is "Test
/// pattern". (The catalog's own display name isn't in the launch spec.)
fn display_name(image: &str) -> String {
    let name = image.rsplit('/').next().unwrap_or(image);
    let name = name.split(':').next().unwrap_or(name);
    let name = ["cha-env-", "env-", "cha-"]
        .iter()
        .find_map(|p| name.strip_prefix(p))
        .unwrap_or(name)
        .replace('-', " ");
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => image.to_string(),
    }
}

/// What the user reads while an image downloads: "Downloading Chrome (412 of
/// 890 MB)", with the counts alongside for a progress bar.
fn pull_progress(id: &str, what: &str, progress: PullProgress) -> Progress {
    if progress.total == 0 {
        return Progress::note(id, format!("Downloading {what}"));
    }
    Progress {
        id: id.to_string(),
        detail: format!("Downloading {what} ({})", sizes(progress)),
        done: Some(progress.done),
        total: Some(progress.total),
        unit: Some("bytes"),
    }
}

/// `412 of 890 MB`, or in GB from a gigabyte up.
fn sizes(p: PullProgress) -> String {
    if p.total >= 1_000_000_000 {
        format!(
            "{:.1} of {:.1} GB",
            p.done as f64 / 1e9,
            p.total as f64 / 1e9
        )
    } else {
        format!("{} of {} MB", p.done / 1_000_000, p.total / 1_000_000)
    }
}

/// The UDP ports of one environment's Moonlight session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameStreamPorts {
    pub video: u16,
    pub control: u16,
    pub audio: u16,
}

impl std::fmt::Display for GameStreamPorts {
    /// As the streamer's `--gamestream-ports` takes them.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{},{},{}", self.video, self.control, self.audio)
    }
}

impl std::str::FromStr for GameStreamPorts {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, ()> {
        let ports: Vec<u16> = text
            .split(',')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .map_err(|_| ())?;
        match ports[..] {
            [video, control, audio] => Ok(Self {
                video,
                control,
                audio,
            }),
            _ => Err(()),
        }
    }
}

/// How the node reaches an environment's GameStream media half: the streamer's
/// local API, and the ports its sessions use (what the host tells Moonlight
/// clients). The secret never leaves the node.
#[derive(Clone, PartialEq, Eq)]
pub struct GameStreamAccess {
    /// The streamer's HTTP port, on localhost.
    pub http_port: u16,
    pub ports: GameStreamPorts,
    /// The bearer token of the streamer's `/gamestream/*` API.
    pub secret: String,
}

impl std::fmt::Debug for GameStreamAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameStreamAccess")
            .field("http_port", &self.http_port)
            .field("ports", &self.ports)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl GameStreamAccess {
    /// From a streamer container's labels, for an environment the agent
    /// finds running when it starts.
    fn from_labels(
        http_port: u16,
        labels: &std::collections::HashMap<String, String>,
    ) -> Option<Self> {
        Some(Self {
            http_port,
            ports: labels.get(LABEL_GAMESTREAM_PORTS)?.parse().ok()?,
            secret: labels.get(LABEL_GAMESTREAM_SECRET)?.clone(),
        })
    }
}

/// A running environment as the node's GameStream host sees it: whose it is,
/// what it runs, and how to reach its media half.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningEnvironment {
    pub id: String,
    /// The user who launched it; empty for an environment an older portal or
    /// agent started, which then belongs to nobody the host can name.
    pub owner: String,
    /// The catalog template's id.
    pub template: String,
    /// It streams a Moonlight host's app through a gateway.
    pub gateway: bool,
    /// `None` without GameStream, or for an environment started before it was on.
    pub gamestream: Option<GameStreamAccess>,
}

/// 32 hex digits from the OS's random source.
fn random_secret() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("random source: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Checks that the GameStream ports ([`DockerConfig::gamestream_port_base`])
/// fit and keep clear of the streamers' (`port_base`): three each for
/// `max_environments`.
pub fn check_gamestream_range(
    port_base: u16,
    gamestream_port_base: u16,
    max_environments: u16,
) -> Result<()> {
    let span = u32::from(PORTS_PER_ENVIRONMENT.max(GAMESTREAM_PORTS_PER_ENVIRONMENT))
        * u32::from(max_environments);
    let streamers = u32::from(port_base)..u32::from(port_base) + span;
    let gamestream = u32::from(gamestream_port_base)..u32::from(gamestream_port_base) + span;
    anyhow::ensure!(
        gamestream.end <= 65536,
        "CHA_GAMESTREAM_PORT_BASE {gamestream_port_base} leaves no room for {max_environments} environments (three ports each)"
    );
    anyhow::ensure!(
        gamestream.end <= streamers.start || streamers.end <= gamestream.start,
        "CHA_GAMESTREAM_PORT_BASE {gamestream_port_base} (to {}) overlaps the streamers' ports from CHA_PORT_BASE {port_base} (to {})",
        gamestream.end - 1,
        streamers.end - 1
    );
    Ok(())
}

/// Where the catalog's apps are published: `ghcr.io/ban-red` and a release,
/// so `cha/env-chrome:dev` runs as `ghcr.io/ban-red/cha-env-chrome:0.1.0`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedImages {
    registry: String,
    tag: String,
}

impl PublishedImages {
    /// From `CHA_IMAGE_REGISTRY` and `CHA_IMAGE_TAG`: none without a
    /// registry (a tag alone is ignored, so compose can always pass one). The
    /// registry must name its host, so pulls never fall back to Docker Hub.
    pub fn from_settings(registry: Option<&str>, tag: Option<&str>) -> Result<Option<Self>> {
        let registry = registry.map(str::trim).unwrap_or_default();
        if registry.is_empty() {
            return Ok(None);
        }
        let registry = registry.trim_end_matches('/');
        if !names_registry(&format!("{registry}/image")) {
            bail!(
                "CHA_IMAGE_REGISTRY must start with a registry's host, e.g. ghcr.io/ban-red: {registry}"
            );
        }
        if !registry
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "./:-_".contains(c))
        {
            bail!("CHA_IMAGE_REGISTRY isn't a registry path: {registry}");
        }
        let tag = tag.map(str::trim).unwrap_or_default();
        let tag = tag.strip_prefix('v').unwrap_or(tag);
        if tag.is_empty()
            || tag.len() > 128
            || !tag
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        {
            bail!(
                "CHA_IMAGE_REGISTRY needs CHA_IMAGE_TAG, the release to run (e.g. 0.1.0): {tag:?}"
            );
        }
        Ok(Some(Self {
            registry: registry.to_string(),
            tag: tag.to_string(),
        }))
    }

    /// The published copy of a catalog image; only the catalog's own `cha/`
    /// images have one.
    pub fn image_for(&self, image: &str) -> Option<String> {
        let name = image.strip_prefix("cha/")?;
        let name = name.split_once(':').map_or(name, |(name, _)| name);
        Some(format!("{}/cha-{name}:{}", self.registry, self.tag))
    }
}

pub struct DockerRuntime {
    docker: Docker,
    config: DockerConfig,
    /// The render node's group, so the app's user can open it.
    render_gid: Option<u32>,
    /// What the streamer image said about each Intel or AMD render node, and
    /// about the CPU.
    probes: DeviceProbes,
    data: DataRoot,
    state: Mutex<State>,
    exits: broadcast::Sender<Exit>,
    progress: broadcast::Sender<Progress>,
    warnings: broadcast::Sender<Warning>,
    /// Fires after each image pull ([`Runtime::pulled`]).
    pulled: broadcast::Sender<()>,
    /// Held while an app with published ports picks them and starts, so two
    /// launches don't pick the same free port.
    ports_lock: tokio::sync::Mutex<()>,
}

/// A per-user directory laid over a shared one in an app.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Overlay {
    /// Where it is mounted in the app's container.
    target: String,
    /// Its place in the shared directory (`steamapps/compatdata`).
    part: String,
}

/// An app's per-user directories, and which of them were missing when last looked.
#[derive(Debug, Default)]
struct Overlays {
    list: Vec<Overlay>,
    lost: Vec<String>,
    /// Looks in a row that couldn't be made (the container's gone, it has no `cat`).
    failures: u32,
}

impl Overlays {
    fn new(list: Vec<Overlay>) -> Self {
        Self {
            list,
            ..Self::default()
        }
    }

    /// What the app's mounts say: `Some` when what is missing changed since
    /// the last look, with the warning to give (`None`: all is back).
    fn observe(&mut self, mounted: &HashSet<String>) -> Option<Option<String>> {
        let lost: Vec<String> = self
            .list
            .iter()
            .filter(|o| !mounted.contains(&o.target))
            .map(|o| o.part.clone())
            .collect();
        if lost == self.lost {
            return None;
        }
        self.lost = lost;
        Some(self.warning())
    }

    fn warning(&self) -> Option<String> {
        (!self.lost.is_empty()).then(|| unmounted_warning(&self.lost))
    }
}

/// What the user is told when `parts` of their own are no longer mounted.
fn unmounted_warning(parts: &[String]) -> String {
    format!(
        "Your own folders in the shared library came unmounted ({}), likely after a change on \
         the NAS. Games now see the shared copies instead. Stop this app and start it again.",
        parts.join(", ")
    )
}

/// The mount points in a `/proc/<pid>/mountinfo`: its fifth field, in which a
/// space, tab, newline or backslash is an octal escape (`\040`).
fn parse_mountinfo(text: &str) -> HashSet<String> {
    text.lines()
        .filter_map(|line| line.split(' ').nth(4))
        .map(unescape_mountinfo)
        .collect()
}

fn unescape_mountinfo(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = (bytes[i] == b'\\')
            .then(|| bytes.get(i + 1..i + 4))
            .flatten()
            .filter(|d| d.iter().all(|c| (b'0'..=b'7').contains(c)))
            .and_then(|d| u32::from_str_radix(std::str::from_utf8(d).ok()?, 8).ok())
            .and_then(|n| u8::try_from(n).ok());
        match octal {
            Some(byte) => {
                out.push(byte);
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The per-user directories an app container has, from its mounts: those whose
/// source is in a user's `.cha-shared` under `users_dir` (the data root's
/// `users`, as the host has it). What a restarted agent knows of a container
/// it didn't make, or one an older agent made.
fn overlays_from_mounts(mounts: &[ContainerMount], users_dir: &Path) -> Vec<Overlay> {
    mounts
        .iter()
        .filter_map(|m| {
            let rest = Path::new(&m.source).strip_prefix(users_dir).ok()?;
            // <user>/<template>/.cha-shared/<part…>
            let mut parts = rest.components();
            let (_user, _template, dir) = (parts.next()?, parts.next()?, parts.next()?);
            if dir.as_os_str() != PER_USER_DIR {
                return None;
            }
            let part = parts.as_path().to_str()?;
            (!part.is_empty()).then(|| Overlay {
                target: m.destination.clone(),
                part: part.to_string(),
            })
        })
        .collect()
}

#[derive(Default)]
struct State {
    /// Environment id → the per-user directories its app has to keep mounted.
    overlays: BTreeMap<String, Overlays>,
    /// Environment id → its streamer's HTTP port.
    ports: BTreeMap<String, u16>,
    /// Environment id → its GameStream media ports and API secret, for the
    /// environments that have them ([`DockerConfig::gamestream_port_base`]).
    gamestream: BTreeMap<String, GameStreamAccess>,
    /// Environment id → (owner, template, gateway), as the GameStream host
    /// lists them ([`DockerRuntime::running_environments`]).
    meta: BTreeMap<String, (String, String, bool)>,
    /// Environments being stopped on purpose (their containers' deaths aren't news).
    stopping: HashSet<String>,
    /// Environments being started: id → what died meanwhile, if anything. The
    /// launch owns their cleanup, so a death isn't acted on until it ends;
    /// otherwise the watcher would remove one container while the launch
    /// goes on to create the next, which then runs on with nothing beside it.
    starting: HashMap<String, Option<String>>,
    /// Environment id → the (user, template) whose directory under the data
    /// root is its home: one environment per home, and no deleting a home
    /// that is in use.
    homes: BTreeMap<String, (String, String)>,
    /// Homes being deleted: nothing starts on them meanwhile.
    resetting: HashSet<(String, String)>,
}

impl DockerRuntime {
    /// Connects to the engine, adopts environments left running by an earlier
    /// agent, clears out dead ones, and starts watching for exits.
    pub async fn new(docker: Docker, mut config: DockerConfig) -> Result<Arc<Self>> {
        docker.ping().await?;
        // The streamer image is settled now, not at the first launch: the
        // device probes and the checks below run in it. The configured one if
        // the engine has it, else its published copy, pulled.
        let candidates = streamer_candidates(&config.streamer_image, AGENT_VERSION);
        match resolve_image(&docker, "streamer", &candidates, |_, _| {}).await {
            Ok((image, _)) => config.streamer_image = image,
            Err(err) => warn!("no streamer image: {err:#}"),
        }
        config.nvidia_wine_dir = Self::settle_nvidia_wine_dir(&docker, &config).await;
        let render_gid = render_gid(&config.render_node);
        if render_gid.is_none() {
            warn!(render_node = %config.render_node, "can't read the render node's group; apps may fall back to software rendering");
        }
        let (exits, _) = broadcast::channel(64);
        let (progress, _) = broadcast::channel(64);
        let (warnings, _) = broadcast::channel(64);
        let (pulled, _) = broadcast::channel(8);
        let data = DataRoot::new(config.data_root.clone(), APP_UID, APP_UID)?;
        let runtime = Arc::new(Self {
            docker,
            config,
            render_gid,
            probes: DeviceProbes::default(),
            data,
            state: Mutex::default(),
            exits,
            progress,
            warnings,
            pulled,
            ports_lock: tokio::sync::Mutex::new(()),
        });
        runtime.adopt().await?;
        let watcher = Arc::clone(&runtime);
        tokio::spawn(async move { watcher.watch().await });
        let watcher = Arc::clone(&runtime);
        tokio::spawn(async move { watcher.watch_overlays().await });
        Ok(runtime)
    }

    /// The NVIDIA Wine directory apps get: the configured one when the host has
    /// it, else none (a bind whose source is missing fails the app's creation).
    async fn settle_nvidia_wine_dir(docker: &Docker, config: &DockerConfig) -> Option<PathBuf> {
        let dir = config.nvidia_wine_dir.as_ref()?;
        match docker.host_path_exists(&config.streamer_image, dir).await {
            Ok(true) => {
                info!(dir = %dir.display(), "mounting NVIDIA's Wine DLLs into apps (DLSS under Proton)");
                Some(dir.clone())
            }
            Ok(false) => {
                if nvidia_present() {
                    warn!(dir = %dir.display(), "the host has no NVIDIA Wine directory, so Proton games get no DLSS; install the driver package that ships nvngx.dll (Ubuntu: libnvidia-gl-<version>) or set CHA_NVIDIA_WINE_DIR");
                } else {
                    info!(dir = %dir.display(), "no NVIDIA Wine directory on the host; apps go without");
                }
                None
            }
            Err(err) => {
                warn!(
                    "can't tell whether the host has {} ({err:#}); apps go without it",
                    dir.display()
                );
                None
            }
        }
    }

    async fn adopt(&self) -> Result<()> {
        let mut dead = HashSet::new();
        let listed = self.docker.list(LABEL_ENV).await?;
        // An app with no streamer beside it (the launch died with the agent, or
        // the streamer was removed) has nothing to stream: left as leftovers.
        let streamers: HashSet<&String> = listed
            .iter()
            .filter(|c| c.labels.get(LABEL_ROLE).map(String::as_str) == Some("streamer"))
            .filter_map(|c| c.labels.get(LABEL_ENV))
            .collect();
        for container in &listed {
            if let Some(id) = container.labels.get(LABEL_ENV)
                && !streamers.contains(id)
            {
                dead.insert(id.clone());
            }
        }
        for container in listed.iter() {
            let Some(id) = container.labels.get(LABEL_ENV).cloned() else {
                continue;
            };
            if container.state != "running" {
                dead.insert(id);
            } else if let Some(port) = container
                .labels
                .get(LABEL_HTTP_PORT)
                .and_then(|p| p.parse().ok())
            {
                let mut state = self.state.lock().expect("state lock");
                if container.labels.get(LABEL_ROLE).map(String::as_str) == Some("app")
                    && let (Some(owner), Some(template)) = (
                        container.labels.get(LABEL_OWNER),
                        container
                            .labels
                            .get(LABEL_DATA_TEMPLATE)
                            .or_else(|| container.labels.get(LABEL_TEMPLATE)),
                    )
                {
                    state
                        .homes
                        .insert(id.clone(), (owner.clone(), template.clone()));
                    let overlays = overlays_from_mounts(
                        &container.mounts,
                        &self.data.host_path(cha_wire::USERS_DIR),
                    );
                    if !overlays.is_empty() {
                        state.overlays.insert(id.clone(), Overlays::new(overlays));
                    }
                }
                if container.labels.get(LABEL_ROLE).map(String::as_str) == Some("streamer") {
                    if let Some(access) = GameStreamAccess::from_labels(port, &container.labels) {
                        state.gamestream.insert(id.clone(), access);
                    }
                    let label = |name| container.labels.get(name).cloned().unwrap_or_default();
                    state.meta.insert(
                        id.clone(),
                        (
                            label(LABEL_OWNER),
                            label(LABEL_TEMPLATE),
                            container.labels.contains_key(LABEL_GATEWAY),
                        ),
                    );
                }
                state.ports.insert(id, port);
            }
        }
        for id in dead {
            info!(%id, "cleaning up an environment that died while the agent was away");
            self.state.lock().expect("state lock").ports.remove(&id);
            self.state
                .lock()
                .expect("state lock")
                .gamestream
                .remove(&id);
            self.state.lock().expect("state lock").meta.remove(&id);
            self.state.lock().expect("state lock").homes.remove(&id);
            self.state.lock().expect("state lock").overlays.remove(&id);
            // Nobody to tell yet (the portal finds it gone when we connect),
            // but the logs are kept, and the log says what they showed.
            let tails = self.capture(&id).await;
            if let Some(why) = self.explain(&tails, None, ("streamer", None)).await {
                info!(%id, "it had ended: {why}");
            }
            let _ = self.remove(&id).await;
        }
        let adopted = self.state.lock().expect("state lock").ports.len();
        if adopted > 0 {
            info!(adopted, "found running environments");
        }
        Ok(())
    }

    /// Follows container events; an environment whose streamer or app dies
    /// without being stopped is cleaned up and reported.
    async fn watch(self: Arc<Self>) {
        loop {
            let (tx, mut rx) = mpsc::channel(64);
            let docker = self.docker.clone();
            let stream = tokio::spawn(async move { docker.events(LABEL_ENV, tx).await });
            while let Some(event) = rx.recv().await {
                self.on_event(event).await;
            }
            match stream.await {
                Ok(Err(err)) => warn!("watching containers: {err:#}"),
                _ => warn!("the engine's event stream ended"),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// Looks at every environment's per-user directories each [`OVERLAY_CHECK`].
    async fn watch_overlays(self: Arc<Self>) {
        let mut ticks = tokio::time::interval(OVERLAY_CHECK);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            self.check_overlays().await;
        }
    }

    /// Reads each app's mounts from inside its container, and reports when
    /// some of its per-user directories are gone, or back. Looks only at what
    /// is running (an environment is watched once it has started).
    async fn check_overlays(&self) {
        let watched: Vec<String> = {
            let state = self.state.lock().expect("state lock");
            state
                .overlays
                .keys()
                .filter(|id| !state.stopping.contains(*id))
                .cloned()
                .collect()
        };
        for id in watched {
            let name = container_name(&id, "app");
            let looked = self
                .docker
                .exec(&name, &["cat", "/proc/self/mountinfo"])
                .await
                .and_then(|(code, out)| match code {
                    0 => Ok(parse_mountinfo(&out)),
                    code => bail!("cat exited {code}: {}", out.trim()),
                });
            let change = {
                let mut state = self.state.lock().expect("state lock");
                let Some(overlays) = state.overlays.get_mut(&id) else {
                    continue;
                };
                match looked {
                    Ok(mounted) => {
                        overlays.failures = 0;
                        overlays.observe(&mounted)
                    }
                    Err(err) => {
                        // Not worth a line each time: the container may be on
                        // its way out. Said once if it keeps up.
                        overlays.failures += 1;
                        match overlays.failures {
                            1 => debug!(%id, "looking at the app's mounts: {err:#}"),
                            3 => {
                                warn!(%id, "can't look at the app's mounts, so can't tell whether its own folders are still mounted: {err:#}")
                            }
                            _ => {}
                        }
                        None
                    }
                }
            };
            if let Some(warning) = change {
                match &warning {
                    Some(text) => warn!(%id, "{text}"),
                    None => info!(%id, "the app's own folders are mounted again"),
                }
                let _ = self.warnings.send(Warning { id, warning });
            }
        }
    }

    async fn on_event(&self, event: ContainerEvent) {
        if event.action != "die" {
            return;
        }
        let Some(id) = event.labels.get(LABEL_ENV).cloned() else {
            return;
        };
        {
            let mut state = self.state.lock().expect("state lock");
            if state.stopping.contains(&id) || !state.ports.contains_key(&id) {
                return;
            }
            if let Some(died) = state.starting.get_mut(&id) {
                // The launch finds out when it next looks, and cleans up.
                let role = event
                    .labels
                    .get(LABEL_ROLE)
                    .map_or("container", String::as_str);
                died.get_or_insert_with(|| match event.exit_code {
                    Some(code) => format!("the {role} exited with code {code} while starting"),
                    None => format!("the {role} stopped while starting"),
                });
                return;
            }
        }
        let role = event
            .labels
            .get(LABEL_ROLE)
            .map_or("container", String::as_str);
        let detail = match event.exit_code {
            Some(0) => format!("the {role} exited"),
            Some(code) => format!("the {role} exited with code {code}"),
            None => format!("the {role} stopped"),
        };
        let failed = role != "app" || event.exit_code.is_some_and(|c| c != 0);
        let (detail, log) = if failed {
            let tails = self.capture(&id).await;
            let why = self.explain(&tails, None, (role, event.exit_code)).await;
            (
                crashlog::detail(&detail, why.as_deref()),
                self.report(&tails, (role, event.exit_code)),
            )
        } else {
            (detail, Vec::new())
        };
        info!(%id, %detail, "environment ended on its own");
        if let Err(err) = self.stop_environment(&id).await {
            warn!(%id, "cleaning up: {err:#}");
        }
        let _ = self.exits.send(Exit {
            id,
            detail,
            failed,
            log,
        });
    }

    async fn start_environment(&self, mut spec: EnvironmentSpec) -> Result<StreamerEndpoint> {
        let version = AGENT_VERSION;
        let candidates = if spec.gateway.is_some() {
            // The gateway stands in for the app and the streamer both.
            vec![self.config.app_image(&self.config.gateway_image)]
        } else {
            self.config.app_candidates(&spec, version)
        };
        spec.image = candidates[0].clone();
        if spec.gateway.is_some() {
            self.check_gateway(&spec)?;
        }
        check_storage(&spec)?;
        check_device(&spec)?;
        self.check_extras(&spec)?;
        if let Some(port) = self
            .state
            .lock()
            .expect("state lock")
            .ports
            .get(&spec.id)
            .copied()
        {
            return Ok(endpoint(port));
        }
        if spec.gateway.is_none() {
            let streamer = vec![self.config.streamer_image.clone()];
            self.ensure_image(&spec.id, "streamer", &streamer).await?;
        }
        spec.image = self.ensure_image(&spec.id, "app", &candidates).await?;
        if spec.host.as_deref().is_some_and(|h| !h.cap_add.is_empty()) {
            let image = self.docker.inspect_image(&spec.image).await?;
            takes_added_caps(&spec.image, image.as_ref())?;
        }
        let port = self.allocate(&spec.id)?;
        if spec.gateway.is_none()
            && let Err(err) = self.allocate_gamestream(&spec.id, port)
        {
            self.state
                .lock()
                .expect("state lock")
                .ports
                .remove(&spec.id);
            return Err(err);
        }
        {
            let mut state = self.state.lock().expect("state lock");
            state.meta.insert(
                spec.id.clone(),
                (
                    spec.owner.clone(),
                    spec.template.clone(),
                    spec.gateway.is_some(),
                ),
            );
            state.starting.insert(spec.id.clone(), None);
        }
        let started = async {
            self.claim_home(&spec)?;
            self.prepare_storage(&mut spec).await?;
            self.create_containers(&spec, port).await
        }
        .await;
        // Settled with the death check, so none slips between them.
        let died = self
            .state
            .lock()
            .expect("state lock")
            .starting
            .remove(&spec.id)
            .flatten();
        let started = match (started, died) {
            (Ok(()), Some(died)) => Err(anyhow!(died)),
            (started, _) => started,
        };
        match started {
            Ok(()) => {
                let overlays = self.per_user_overlays(&spec);
                if !overlays.is_empty() {
                    self.state
                        .lock()
                        .expect("state lock")
                        .overlays
                        .insert(spec.id.clone(), Overlays::new(overlays));
                }
                info!(id = %spec.id, image = %spec.image, http_port = port, "environment started");
                Ok(endpoint(port))
            }
            Err(err) => {
                // What the containers said, before they go. The portal gets
                // the error as the answer to its request, and this alongside.
                let tails = self.capture(&spec.id).await;
                let said = format!("{err:#}");
                let why = self.explain(&tails, Some(&said), ("streamer", None)).await;
                {
                    let mut state = self.state.lock().expect("state lock");
                    state.ports.remove(&spec.id);
                    state.gamestream.remove(&spec.id);
                    state.meta.remove(&spec.id);
                    state.homes.remove(&spec.id);
                }
                // Everything this launch made, app and streamer both, whichever
                // got as far as existing.
                if let Err(err) = self.remove(&spec.id).await {
                    warn!(id = %spec.id, "cleaning up after the failed start: {err:#}");
                }
                if let Some(why) = why
                    && tails.iter().any(|t| !t.lines.is_empty())
                {
                    let _ = self.exits.send(Exit {
                        id: spec.id.clone(),
                        detail: crashlog::detail(
                            &format!("it failed to start: {}", crashlog::shorten(&said, 120)),
                            Some(&why),
                        ),
                        failed: true,
                        log: self.report(&tails, ("streamer", None)),
                    });
                }
                warn!(id = %spec.id, image = %spec.image, "environment failed to start: {err:#}");
                if spec.security == SecurityProfile::Steam
                    && format!("{err:#}").contains("apparmor")
                {
                    bail!(
                        "this node hasn't loaded the {SANDBOX_APPARMOR} AppArmor profile that \
                         Steam's sandbox needs (`cha-node --doctor` says how)"
                    );
                }
                if spec.security == SecurityProfile::Vm && format!("{err:#}").contains(KVM_DEVICE) {
                    bail!(
                        "this node couldn't give the app {KVM_DEVICE}: it needs the host's kvm \
                         module loaded and virtualisation on in its firmware ({err:#})"
                    );
                }
                Err(err)
            }
        }
    }

    /// Reads the tail of each of the environment's containers (running or
    /// dead) and keeps it in the log directory. What can't be read is left out.
    async fn capture(&self, id: &str) -> Vec<Tail> {
        let containers = match self.docker.list(&format!("{LABEL_ENV}={id}")).await {
            Ok(containers) => containers,
            Err(err) => {
                warn!(%id, "listing containers to read their logs: {err:#}");
                return Vec::new();
            }
        };
        let mut tails = Vec::new();
        for container in containers {
            let role = container
                .labels
                .get(LABEL_ROLE)
                .map_or("container", String::as_str)
                .to_string();
            match self
                .docker
                .logs_tail(&container.id, crashlog::TAIL_LINES)
                .await
            {
                Ok(lines) => tails.push(Tail { role, lines }),
                Err(err) => warn!(%id, %role, "reading the container's logs: {err:#}"),
            }
        }
        tails.sort_by(|a, b| a.role.cmp(&b.role).reverse());
        if let Some(dir) = &self.config.log_dir {
            for tail in tails.iter().filter(|t| !t.lines.is_empty()) {
                if let Err(err) = crashlog::keep(dir, id, tail) {
                    warn!(%id, role = %tail.role, "keeping the log in {}: {err}", dir.display());
                }
            }
        }
        tails
    }

    /// Why it ended, from the tails (the container that `died` leading) and
    /// `extra`, what a failed start said, which is looked at last. For a GPU
    /// out of memory, with who holds it.
    async fn explain(
        &self,
        tails: &[Tail],
        extra: Option<&str>,
        died: (&str, Option<i64>),
    ) -> Option<String> {
        let said = extra.map(|text| Tail {
            role: "error".into(),
            lines: text.lines().map(str::to_string).collect(),
        });
        let mut order: Vec<&Tail> = tails.iter().collect();
        order.sort_by_key(|t| t.role != died.0);
        order.extend(said.as_ref());
        let reason = crashlog::reason(&order, died)?;
        if !reason.gpu_memory {
            return Some(reason.text);
        }
        let now = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::task::spawn_blocking(crashlog::vram_now),
        )
        .await;
        match now {
            Ok(Ok(Some(context))) => Some(format!("{}: {context}", reason.text)),
            _ => Some(reason.text),
        }
    }

    /// The lines for the portal, the container that `died` first.
    fn report(&self, tails: &[Tail], died: (&str, Option<i64>)) -> Vec<String> {
        let mut order: Vec<&Tail> = tails.iter().collect();
        order.sort_by_key(|t| t.role != died.0);
        crashlog::report(&order)
    }

    /// The first of `candidates` the engine has, else the first that names a
    /// registry, pulled; the user is told how the download goes.
    async fn ensure_image(&self, id: &str, role: &str, candidates: &[String]) -> Result<String> {
        let (image, pulled) = resolve_image(&self.docker, role, candidates, |image, p| {
            let _ = self
                .progress
                .send(pull_progress(id, &display_name(image), p));
        })
        .await?;
        if pulled {
            let _ = self.pulled.send(());
        }
        Ok(image)
    }

    fn allocate(&self, id: &str) -> Result<u16> {
        let mut state = self.state.lock().expect("state lock");
        let used: HashSet<u16> = state.ports.values().copied().collect();
        let port = (0..self.config.max_environments)
            .map(|n| self.config.port_base + PORTS_PER_ENVIRONMENT * n)
            .find(|p| !used.contains(p))
            .ok_or_else(|| {
                anyhow!(
                    "this node is full ({} environments)",
                    self.config.max_environments
                )
            })?;
        state.ports.insert(id.to_string(), port);
        Ok(port)
    }

    /// With GameStream on, gives the environment on `port` its Moonlight
    /// media block (the same slot in the GameStream range as `port` has in the
    /// streamers') and a secret for its `/gamestream/*` API.
    fn allocate_gamestream(&self, id: &str, port: u16) -> Result<()> {
        let Some(base) = self.config.gamestream_port_base else {
            return Ok(());
        };
        let slot = (port - self.config.port_base) / PORTS_PER_ENVIRONMENT;
        let video = base + GAMESTREAM_PORTS_PER_ENVIRONMENT * slot;
        let access = GameStreamAccess {
            http_port: port,
            ports: GameStreamPorts {
                video,
                control: video + 1,
                audio: video + 2,
            },
            secret: random_secret()?,
        };
        self.state
            .lock()
            .expect("state lock")
            .gamestream
            .insert(id.to_string(), access);
        Ok(())
    }

    /// What the node's GameStream host needs to start `id`'s media: where
    /// its streamer listens, the media ports, the API secret. `None` without
    /// GameStream, or for an environment that has no streamer of ours.
    pub fn gamestream_access(&self, id: &str) -> Option<GameStreamAccess> {
        self.state
            .lock()
            .expect("state lock")
            .gamestream
            .get(id)
            .cloned()
    }

    /// The environments running now, for the node's GameStream host: not the
    /// ones still starting or being stopped.
    pub fn running_environments(&self) -> Vec<RunningEnvironment> {
        let state = self.state.lock().expect("state lock");
        state
            .ports
            .keys()
            .filter(|id| !state.starting.contains_key(*id) && !state.stopping.contains(*id))
            .filter_map(|id| {
                let (owner, template, gateway) = state.meta.get(id)?.clone();
                Some(RunningEnvironment {
                    id: id.clone(),
                    owner,
                    template,
                    gateway,
                    gamestream: state.gamestream.get(id).cloned(),
                })
            })
            .collect()
    }

    /// Takes the home directory `spec` names: refused while it is being
    /// deleted, or while another environment of ours uses it.
    fn claim_home(&self, spec: &EnvironmentSpec) -> Result<()> {
        let Some(key) = home_key(spec) else {
            return Ok(());
        };
        let mut state = self.state.lock().expect("state lock");
        if state.resetting.contains(&key) {
            bail!("your data for this app is being reset; try again in a moment");
        }
        if let Some((other, _)) = state
            .homes
            .iter()
            .find(|(id, k)| **k == key && **id != spec.id)
        {
            bail!(
                "environment {other} already uses this home on this node (its data for {}); stop it first",
                key.1
            );
        }
        state.homes.insert(spec.id.clone(), key);
        Ok(())
    }

    /// The directories `spec`'s storage names, made and filled, before the
    /// containers that mount them: see [`crate::storage`]. A shared directory
    /// the owner keeps elsewhere that isn't usable (the share isn't mounted) is
    /// dropped from `spec`: the app starts without it, and with its home.
    async fn prepare_storage(&self, spec: &mut EnvironmentSpec) -> Result<()> {
        let Some(storage) = &spec.storage else {
            return Ok(());
        };
        let data = self.data.clone();
        if let Some(shared) = &storage.shared {
            let per_user = shared.per_user.clone();
            if let Some(dir) = self.config.shared_dirs.get(spec.data_template()).cloned() {
                if let Err(problem) = check_external(dir.clone(), per_user).await {
                    warn!(
                        id = %spec.id, template = %spec.data_template(), dir = %dir.display(),
                        "starting without the shared directory: {problem}"
                    );
                    if let Some(storage) = spec.storage.as_mut() {
                        storage.shared = None;
                    }
                }
            } else {
                let template = spec.data_template().to_string();
                let data = data.clone();
                blocking(move || data.ensure_shared(&template, &per_user)).await?;
            }
        }
        let Some(storage) = &spec.storage else {
            return Ok(());
        };
        let spec = &*spec;
        let Some((user, template)) = home_key(spec) else {
            return Ok(());
        };
        let found = {
            let (data, user, template) = (data.clone(), user.clone(), template.clone());
            blocking(move || data.inspect_home(&user, &template)).await?
        };
        let legacy = match &storage.legacy_volume {
            Some(volume) => self.docker.volume_exists(volume).await?,
            None => false,
        };
        let home = data.home_path(&user, &template);
        match plan_seed(found, legacy) {
            Seed::Keep => {}
            Seed::Image => {
                // A new directory starts as a new volume would: from what the
                // image keeps in its home. Without it the app still runs.
                {
                    let (data, user, template) = (data.clone(), user.clone(), template.clone());
                    blocking(move || data.ensure_home(&user, &template)).await?;
                }
                let copied = self
                    .copy_into(spec, &CopySource::Image, &home, "the image's files")
                    .await;
                if let Err(err) = copied {
                    warn!(id = %spec.id, "starting the home empty: {err:#}");
                }
            }
            Seed::Migrate => {
                let volume = storage.legacy_volume.clone().unwrap_or_default();
                self.migrate_volume(spec, &user, &template, &volume).await?;
            }
        }
        {
            let (data, user, template) = (data.clone(), user.clone(), template.clone());
            blocking(move || data.ensure_home(&user, &template)).await?;
        }
        if let Some(shared) = &storage.shared
            && !shared.per_user.is_empty()
        {
            let per_user = shared.per_user.clone();
            blocking(move || data.ensure_per_user(&user, &template, &per_user)).await?;
        }
        Ok(())
    }

    /// Copies the user's old home volume into their new directory, once:
    /// into a directory beside it that only a finished copy turns into the
    /// home, so an interrupted one leaves nothing that looks like a home. The
    /// volume itself is never touched.
    async fn migrate_volume(
        &self,
        spec: &EnvironmentSpec,
        user: &str,
        template: &str,
        volume: &str,
    ) -> Result<()> {
        info!(id = %spec.id, %volume, "moving a home volume's files under the data root");
        let _ = self.progress.send(Progress::note(
            &spec.id,
            "Moving your files to the new storage (this happens once)",
        ));
        let data = self.data.clone();
        let temp = {
            let (data, user, template) = (data.clone(), user.to_string(), template.to_string());
            blocking(move || data.begin_migration(&user, &template)).await?
        };
        let copied = self
            .copy_into(spec, &CopySource::Volume(volume.to_string()), &temp, volume)
            .await;
        let (user, template, volume) = (user.to_string(), template.to_string(), volume.to_string());
        match copied {
            Ok(()) => blocking(move || data.finish_migration(&user, &template, &volume)).await,
            Err(err) => {
                let _ = blocking(move || data.abort_migration(&user, &template)).await;
                Err(err.context("your files are still in the old volume; nothing was changed"))
            }
        }
    }

    /// Runs `cp -a` in a short-lived container of the app's image: everything
    /// in `source`, with owners, modes and links, into the host directory `dest`.
    async fn copy_into(
        &self,
        spec: &EnvironmentSpec,
        source: &CopySource,
        dest: &Path,
        what: &str,
    ) -> Result<()> {
        let config = copy_config(&spec.image, source, dest);
        let name = format!("cha-copy-{}", spec.id);
        let (code, output) = self
            .docker
            .run(&name, &config, COPY_TIMEOUT)
            .await
            .with_context(|| format!("copying {what}"))?;
        if code != 0 {
            bail!(
                "copying {what} failed (cp exited {code}): {}",
                output.trim().lines().last().unwrap_or("no output")
            );
        }
        Ok(())
    }

    /// Deletes `user`'s directory for `template`, unless an environment of
    /// theirs uses it, and sees that the legacy volume isn't copied back.
    async fn delete_user_data(&self, user: String, template: String) -> Result<()> {
        if !valid_user_id(&user) || !valid_template_id(&template) {
            bail!("{user:?} / {template:?} aren't a user id and a template id");
        }
        let key = (user.clone(), template.clone());
        {
            let mut state = self.state.lock().expect("state lock");
            if state.homes.values().any(|k| *k == key) {
                bail!("an environment of theirs for this app is running on this node");
            }
            if !state.resetting.insert(key.clone()) {
                bail!("this data is already being deleted");
            }
        }
        let done = async {
            let data = self.data.clone();
            let (u, t) = (user.clone(), template.clone());
            blocking({
                let data = data.clone();
                move || data.delete_user_data(&u, &t)
            })
            .await?;
            // The old volume is left in place, so deleting the new directory
            // must not make the next launch copy it back.
            let volume = home_volume_name(&user, &template);
            if self.docker.volume_exists(&volume).await? {
                blocking(move || data.mark_migrated(&user, &template, &volume)).await?;
            }
            Ok(())
        }
        .await;
        self.state
            .lock()
            .expect("state lock")
            .resetting
            .remove(&key);
        done
    }

    async fn create_containers(&self, spec: &EnvironmentSpec, port: u16) -> Result<()> {
        if spec.gateway.is_some() {
            // One container, labelled as the streamer so everything that finds
            // an environment by it works; there is no app beside it.
            let gateway = self
                .docker
                .create(
                    &container_name(&spec.id, "streamer"),
                    &self.gateway_config(spec, port),
                )
                .await?;
            self.docker.start(&gateway).await?;
            return self.streamer_still_up(&spec.id).await;
        }
        let streamer = self
            .docker
            .create(
                &container_name(&spec.id, "streamer"),
                &self.streamer_config(spec, port),
            )
            .await?;
        self.docker.start(&streamer).await?;
        self.streamer_still_up(&spec.id).await?;
        let hidraw = self.streamer_hidraw(spec, port).await?;
        if let Some(node) = vaapi_node(spec) {
            // So app_render_gid knows it, though the agent may not have the node.
            if self
                .probes
                .render_gid(&self.docker, &self.config.streamer_image, node)
                .await
                .is_none()
            {
                warn!(%node, "can't tell the render node's group; the app may not open the GPU");
            }
        }
        if let Some(card) = steam_primary_node(spec)
            && self
                .probes
                .render_gid(&self.docker, &self.config.streamer_image, &card)
                .await
                .is_none()
        {
            warn!(%card, "can't tell the primary node's group; gamescope may not start");
        }
        if spec.security == SecurityProfile::Vm {
            // So app_config knows their groups, though the agent may not
            // have the nodes.
            if self
                .probes
                .render_gid(&self.docker, &self.config.streamer_image, KVM_DEVICE)
                .await
                .is_none()
            {
                bail!(
                    "can't tell which group owns {KVM_DEVICE} on this node, which the virtual \
                     machine needs to open it (does the host have the kvm module loaded?)"
                );
            }
            if Path::new(UDMABUF_SYSFS).exists()
                && self
                    .probes
                    .render_gid(&self.docker, &self.config.streamer_image, UDMABUF_DEVICE)
                    .await
                    .is_none()
            {
                warn!("can't tell {UDMABUF_DEVICE}'s group; the guest's display may be slower");
            }
        }
        // Picking a free port and publishing it is one step: the next launch
        // sees this one's ports only once the app has them.
        let wants_ports = spec.host.as_deref().is_some_and(|h| !h.ports.is_empty());
        let _ports = if wants_ports {
            Some(self.ports_lock.lock().await)
        } else {
            None
        };
        let used = if wants_ports {
            self.used_host_ports(&spec.id).await?
        } else {
            HashSet::new()
        };
        let plan = self.plan_host(spec, &used)?;
        let app = self
            .docker
            .create(
                &container_name(&spec.id, "app"),
                &self.app_config_with(spec, port, &hidraw, &plan),
            )
            .await?;
        self.docker.start(&app).await
    }

    /// Whether the node may run `spec`'s extras: its variables
    /// ([`check_env`]) and host options ([`Self::plan_host`]), before
    /// anything is pulled or created.
    fn check_extras(&self, spec: &EnvironmentSpec) -> Result<()> {
        let host = spec.host.as_deref().is_some_and(|h| !h.is_empty());
        if spec.gateway.is_some() && (spec.env.is_some() || host) {
            bail!("a Moonlight gateway takes no extra variables or host options");
        }
        if let Some(env) = &spec.env {
            check_env(env).map_err(|e| anyhow!("refusing the app's variables: {e}"))?;
        }
        self.plan_host(spec, &HashSet::new())?;
        Ok(())
    }

    /// The host ports in use on this node: published by another
    /// environment's app, or this agent's own (streamers, GameStream),
    /// whatever the protocol.
    async fn used_host_ports(&self, own: &str) -> Result<HashSet<(u16, Protocol)>> {
        let containers = self.docker.list(LABEL_ENV).await?;
        Ok(used_ports(&containers, own, &self.reserved_ports()))
    }

    /// The ports this agent hands streamers, which an app can't have.
    fn reserved_ports(&self) -> Vec<u16> {
        let span = PORTS_PER_ENVIRONMENT * self.config.max_environments;
        let mut ports: Vec<u16> = (0..span)
            .filter_map(|n| self.config.port_base.checked_add(n))
            .collect();
        if let Some(base) = self.config.gamestream_port_base {
            let span = GAMESTREAM_PORTS_PER_ENVIRONMENT * self.config.max_environments;
            ports.extend((0..span).filter_map(|n| base.checked_add(n)));
        }
        ports
    }

    /// Checks `spec`'s host options against this node's policy and turns them
    /// into what the app's container gets; `used` are the host ports taken
    /// already. Nothing is touched. Every refusal names the option.
    fn plan_host(
        &self,
        spec: &EnvironmentSpec,
        used: &HashSet<(u16, Protocol)>,
    ) -> Result<HostPlan> {
        let Some(req) = spec.host.as_deref().filter(|h| !h.is_empty()) else {
            return Ok(HostPlan::default());
        };
        let config = &self.config.host_options;
        let refuse = |why: String| anyhow!("host options refused: {why}");
        req.check_shape().map_err(refuse)?;
        if config.mode == HostOptionsMode::Off {
            return Err(refuse(
                "this node allows no host options (CHA_HOST_OPTIONS=off)".into(),
            ));
        }
        let refusals = config.policy().refusals(req);
        if !refusals.is_empty() {
            return Err(refuse(format!(
                "this node {} (CHA_HOST_OPTIONS={})",
                refusals.join("; "),
                if config.mode == HostOptionsMode::Full {
                    "full"
                } else {
                    "allowlist"
                }
            )));
        }
        let socket = self.docker.socket().to_string_lossy().into_owned();
        // What the app mounts at a fixed place that a target may not cover.
        let shared = spec
            .storage
            .as_ref()
            .is_some_and(|s| s.shared.is_some())
            .then(|| self.shared_location(spec.data_template()).1)
            .filter(|_| self.config.shared_dirs.contains_key(spec.data_template()));
        let wine = self
            .config
            .nvidia_wine_dir
            .as_ref()
            .map(|d| d.to_string_lossy().into_owned());
        let mut plan = HostPlan::default();
        for (n, mount) in req.mounts.iter().enumerate() {
            for kept in shared.iter().chain(wine.iter()) {
                if path_under(&mount.target, kept) || path_under(kept, &mount.target) {
                    return Err(refuse(format!(
                        "mount {} would overlap {kept}, which the node mounts itself",
                        mount.target
                    )));
                }
            }
            plan.mounts
                .push(self.host_mount(&spec.id, n, mount, &socket)?);
        }
        plan.ports = assign_ports(&req.ports, &config.ports, used, &self.reserved_ports())?;
        plan.cap_add = req.cap_add.clone();
        plan.devices = req.devices.clone();
        plan.privileged = req.privileged;
        plan.network_host = req.network_host;
        plan.security_opt = req.security_opt.clone();
        Ok(plan)
    }

    /// One requested mount as Docker's `Mounts` takes it. A named mount's
    /// `:ro` is a ceiling over the request.
    fn host_mount(&self, id: &str, n: usize, mount: &HostMount, socket: &str) -> Result<Value> {
        let refuse = |why: String| anyhow!("host options refused: {why}");
        let bind = |source: &str, read_only: bool| -> Result<Value> {
            if path_under(socket, source) {
                return Err(refuse(format!(
                    "mount {} would give the app this agent's Docker socket ({socket})",
                    mount.target
                )));
            }
            Ok(json!({
                "Type": "bind",
                "Source": source,
                "Target": mount.target,
                "ReadOnly": read_only,
                // The owner's directory: never for Docker to make as root.
                "BindOptions": { "CreateMountpoint": false },
            }))
        };
        match &mount.source {
            MountSource::Named { name } => {
                let Some(named) = self.config.host_options.mount(name) else {
                    return Err(refuse(format!(
                        "this node has no mount named {name} (CHA_HOST_MOUNTS)"
                    )));
                };
                bind(
                    &named.path.to_string_lossy(),
                    mount.read_only || named.read_only,
                )
            }
            MountSource::Path { path } => bind(path, mount.read_only),
            MountSource::Network {
                fs_type,
                device,
                options,
            } => {
                // The share is mounted by the engine's `local` driver when
                // the container starts, and removed with the environment.
                let (fs, o) = fs_type.driver_options(options);
                let mut driver = serde_json::Map::new();
                driver.insert("type".into(), json!(fs));
                driver.insert("device".into(), json!(device));
                driver.insert("o".into(), json!(o));
                Ok(json!({
                    "Type": "volume",
                    "Source": format!("{HOST_VOLUME_PREFIX}{id}-{n}"),
                    "Target": mount.target,
                    "ReadOnly": mount.read_only,
                    "VolumeOptions": {
                        // The share's files, not the image's directory.
                        "NoCopy": true,
                        "Labels": { LABEL_ENV: id },
                        "DriverConfig": { "Name": "local", "Options": driver },
                    },
                }))
            }
        }
    }

    /// The ports published for `id`'s host options, as the engine reports them.
    async fn published_ports(&self, id: &str) -> Result<Vec<HostPort>> {
        let containers = self.docker.list(&format!("{LABEL_ENV}={id}")).await?;
        Ok(containers
            .iter()
            .filter(|c| c.labels.get(LABEL_ROLE).map(String::as_str) == Some("app"))
            .flat_map(|c| published(&c.ports))
            .collect())
    }

    /// Fails if the streamer has already exited (the engine says a container
    /// started before it finds out the process died): no app is made for a
    /// streamer that isn't there.
    async fn streamer_still_up(&self, id: &str) -> Result<()> {
        let containers = self.docker.list(&format!("{LABEL_ENV}={id}")).await?;
        let up = containers.iter().any(|c| {
            c.labels.get(LABEL_ROLE).map(String::as_str) == Some("streamer") && c.state == "running"
        });
        if !up {
            bail!("the streamer exited as it started");
        }
        Ok(())
    }

    /// The `hidraw` nodes the streamer made for `spec`'s controller, once it
    /// is up (they exist before it answers, `docs/controllers.md`): none for
    /// the kinds without any, or on a node that goes without uhid.
    async fn streamer_hidraw(&self, spec: &EnvironmentSpec, port: u16) -> Result<Vec<Hidraw>> {
        let kind = spec.gamepad.unwrap_or_default();
        if !kind.needs_uhid() {
            return Ok(Vec::new());
        }
        if self.config.uhid.is_none() {
            warn!(
                id = %spec.id,
                "{} was asked for but this node has no uhid (CHA_UHID is empty): the app gets an Xbox 360 pad",
                kind.as_str()
            );
            return Ok(Vec::new());
        }
        let deadline = tokio::time::Instant::now() + STREAMER_UP_TIMEOUT;
        let info = loop {
            match local(Method::GET, port, "/info", None).await {
                Ok(info) => break info,
                Err(err) if tokio::time::Instant::now() >= deadline => {
                    return Err(err.context("waiting for the streamer to make the controller"));
                }
                Err(_) => tokio::time::sleep(STREAMER_UP_POLL).await,
            }
        };
        let nodes = parse_hidraw(&info)?;
        if nodes.is_empty() {
            warn!(
                id = %spec.id,
                "the streamer made no hidraw nodes for {} (it has fallen back to an Xbox 360 pad?)",
                kind.as_str()
            );
        }
        Ok(nodes)
    }

    fn labels(&self, id: &str, role: &str, port: u16) -> Value {
        json!({ LABEL_ENV: id, LABEL_ROLE: role, LABEL_HTTP_PORT: port.to_string() })
    }

    /// The app's labels: also whose home it has, when that is a directory
    /// under the data root, so a restarted agent finds the homes in use.
    fn app_labels(&self, spec: &EnvironmentSpec, port: u16) -> Value {
        let mut labels = self.labels(&spec.id, "app", port);
        if let Some((owner, data_template)) = home_key(spec) {
            labels[LABEL_OWNER] = json!(owner);
            // The environment's own template; the data's may be its base's.
            labels[LABEL_TEMPLATE] = json!(spec.template);
            labels[LABEL_DATA_TEMPLATE] = json!(data_template);
        }
        if spec.security == SecurityProfile::Vm {
            labels[LABEL_PROFILE] = json!("vm");
        }
        labels
    }

    /// Where `template`'s shared directory is on the host, and where its app
    /// finds it: under the data root, at a stable path in the container; or
    /// where the owner keeps it, at that same path in the container too.
    fn shared_location(&self, template: &str) -> (PathBuf, String) {
        match self.config.shared_dirs.get(template) {
            Some(dir) => (dir.clone(), dir.to_string_lossy().into_owned()),
            None => (
                self.data.host_path(&cha_wire::shared_dir(template)),
                format!("{SHARED_MOUNT_ROOT}/{template}"),
            ),
        }
    }

    /// The app's mounts for its persistent and shared data: directories under
    /// the data root; or, from a portal that names a home volume, that. The
    /// shared directory's mount comes before the per-user ones over it.
    fn storage_mounts(&self, spec: &EnvironmentSpec) -> Vec<Value> {
        let Some(storage) = &spec.storage else {
            return home_mount(spec).into_iter().collect();
        };
        let bind = |source: String, target: String, read_only: bool| {
            json!({
                "Type": "bind",
                "Source": source,
                "Target": target,
                "ReadOnly": read_only,
                // Made by the agent before the container: a missing source is
                // a bug, not something for Docker to make as root.
                "BindOptions": { "CreateMountpoint": false },
            })
        };
        let host = |relative: &str| self.data.host_path(relative).to_string_lossy().into_owned();
        let mut mounts = Vec::new();
        if let Some(home) = &storage.home {
            mounts.push(bind(host(home), APP_HOME.into(), false));
        }
        if let Some(shared) = &storage.shared {
            let (source, target) = self.shared_location(spec.data_template());
            mounts.push(bind(
                source.to_string_lossy().into_owned(),
                target.clone(),
                !shared.writable,
            ));
            // What isn't shared goes over it: the user's own directories, in
            // their home (a spec has to have one for these: `Storage::check`).
            for overlay in self.per_user_overlays(spec) {
                if let Some(home) = &storage.home {
                    let source = host(&format!("{home}/{PER_USER_DIR}/{}", overlay.part));
                    mounts.push(bind(source, overlay.target, false));
                }
            }
        }
        mounts
    }

    /// The user's own directories laid over the shared one in `spec`'s app.
    fn per_user_overlays(&self, spec: &EnvironmentSpec) -> Vec<Overlay> {
        let Some(storage) = &spec.storage else {
            return Vec::new();
        };
        let (Some(shared), Some(_home)) = (&storage.shared, &storage.home) else {
            return Vec::new();
        };
        let (_, target) = self.shared_location(spec.data_template());
        shared
            .per_user
            .iter()
            .map(|part| Overlay {
                target: format!("{target}/{part}"),
                part: part.clone(),
            })
            .collect()
    }

    /// Whether environments get gamepads at all: the streamer has a device to
    /// make them with.
    fn has_pads(&self) -> bool {
        self.config.uinput.is_some() || self.config.uhid.is_some()
    }

    /// What GPU access `spec`'s containers get through Docker's device
    /// requests: the CDI device for `nvidia`, nothing for the others (`vaapi`
    /// gets a device node, [`Self::gpu_devices`]).
    fn gpu(&self, spec: &EnvironmentSpec) -> Value {
        match device_kind(spec) {
            DeviceKind::Nvidia => {
                json!([{ "Driver": "cdi", "DeviceIDs": [self.config.gpu_device] }])
            }
            DeviceKind::Vaapi | DeviceKind::Cpu => json!([]),
        }
    }

    /// The render node a `vaapi` environment's containers get, as Docker's
    /// `Devices` take it.
    fn gpu_devices(&self, spec: &EnvironmentSpec) -> Vec<Value> {
        vaapi_node(spec)
            .map(|node| {
                json!({
                    "PathOnHost": node,
                    "PathInContainer": node,
                    "CgroupPermissions": "rw",
                })
            })
            .into_iter()
            .collect()
    }

    /// The group that lets the app open its GPU's render node: the configured
    /// one's for `nvidia` (CDI passes the node through with the host's
    /// ownership), the chosen node's for `vaapi` (as the agent sees it, or as
    /// [`DeviceProbes::render_gid`] found it), none for `cpu`.
    fn app_render_gid(&self, spec: &EnvironmentSpec) -> Option<u32> {
        match device_kind(spec) {
            DeviceKind::Nvidia => self.render_gid,
            DeviceKind::Vaapi => vaapi_node(spec)
                .and_then(|node| render_gid(node).or_else(|| self.probes.known_gid(node))),
            DeviceKind::Cpu => None,
        }
    }

    /// The volumes, as the streamer or the app (`app`) mounts them.
    fn mounts(&self, id: &str, app: bool) -> Value {
        let mut mounts =
            vec![json!({ "Type": "volume", "Source": volume_name(id), "Target": RUNTIME_DIR })];
        if self.has_pads() {
            for (kind, app_target) in [("input", "/dev/input"), ("udev", "/run/udev")] {
                let target = if app {
                    app_target.to_string()
                } else {
                    format!("{INPUT_DIR}/{}", if kind == "input" { "dev" } else { kind })
                };
                mounts.push(json!({
                    "Type": "volume",
                    "Source": format!("{}-{kind}", volume_name(id)),
                    "Target": target,
                    "ReadOnly": app,
                }));
            }
        }
        json!(mounts)
    }

    fn streamer_config(&self, spec: &EnvironmentSpec, port: u16) -> Value {
        let mut cmd: Vec<String> = [
            "--width",
            &spec.width.to_string(),
            "--height",
            &spec.height.to_string(),
            "--fps",
            &spec.fps.to_string(),
            "--app-uid",
            &APP_UID.to_string(),
            // Signalling only on localhost: browsers come through the portal
            // and this agent, with a media token the portal signed.
            "--listen",
            "127.0.0.1",
            "--http-port",
            &port.to_string(),
            "--webrtc-port",
            &(port + 1).to_string(),
            "--wt-port",
            &(port + 2).to_string(),
            "--portal-key",
            &spec.portal_key,
            "--environment-id",
            &spec.id,
        ]
        .map(String::from)
        .to_vec();
        if let Some(public) = &self.config.public_address {
            cmd.extend(["--public-address".to_string(), public.clone()]);
        }
        // `nvidia` is what a streamer without `--device` does, so an older
        // image still starts those.
        match device_kind(spec) {
            DeviceKind::Nvidia => {
                cmd.extend(["--render-node", &self.config.render_node].map(String::from));
            }
            DeviceKind::Vaapi => cmd.extend(
                [
                    "--device",
                    "vaapi",
                    "--render-node",
                    vaapi_node(spec).unwrap_or_default(),
                ]
                .map(String::from),
            ),
            DeviceKind::Cpu => cmd.extend(["--device", "cpu"].map(String::from)),
        }
        let mut devices = self.gpu_devices(spec);
        if self.has_pads() {
            cmd.extend(["--input-dir", INPUT_DIR].map(String::from));
        }
        if let Some(uinput) = &self.config.uinput {
            cmd.extend(["--uinput", "/dev/uinput"].map(String::from));
            devices.push(json!({
                "PathOnHost": uinput,
                "PathInContainer": "/dev/uinput",
                "CgroupPermissions": "rw",
            }));
        }
        if let Some(uhid) = &self.config.uhid {
            cmd.extend(["--uhid", "/dev/uhid"].map(String::from));
            devices.push(json!({
                "PathOnHost": uhid,
                "PathInContainer": "/dev/uhid",
                "CgroupPermissions": "rw",
            }));
        }
        // Left out for the default, which is what a streamer without the
        // option makes.
        if let Some(kind) = spec.gamepad.filter(|_| self.has_pads()) {
            cmd.extend(["--pad-kind", kind.as_str()].map(String::from));
        }
        // The Moonlight media ports and the API secret; an older streamer
        // image never gets them (this is off by default).
        let gamestream = self.gamestream_access(&spec.id);
        let mut env = vec![
            format!("XDG_RUNTIME_DIR={RUNTIME_DIR}"),
            "RUST_LOG=info,smithay=warn,str0m=warn".to_string(),
            // Whose pads: a Steam Controller's serial is made from it, so Steam
            // keeps one configuration for it across launches. An environment
            // variable, which an older streamer ignores.
            format!("CHA_PAD_IDENTITY={}/{}", spec.owner, spec.template),
        ];
        let mut labels = self.labels(&spec.id, "streamer", port);
        // Whose it is, which the node's GameStream host reads back after a restart.
        labels[LABEL_OWNER] = json!(spec.owner);
        labels[LABEL_TEMPLATE] = json!(spec.template);
        if let Some(access) = &gamestream {
            cmd.extend(["--gamestream-ports".to_string(), access.ports.to_string()]);
            env.push(format!("CHA_GAMESTREAM_SECRET={}", access.secret));
            labels[LABEL_GAMESTREAM_PORTS] = json!(access.ports.to_string());
            labels[LABEL_GAMESTREAM_SECRET] = json!(access.secret);
        }
        json!({
            "Image": self.config.streamer_image,
            "Cmd": cmd,
            "Env": env,
            "Labels": labels,
            "HostConfig": {
                "NetworkMode": "host",
                "Mounts": self.mounts(&spec.id, false),
                "DeviceRequests": self.gpu(spec),
                "Devices": devices,
                "RestartPolicy": { "Name": "no" },
                "Init": true,
            },
        })
    }

    /// Refuses a gateway launch for a host this node isn't paired with, or
    /// one whose id isn't a host id (it names a directory).
    fn check_gateway(&self, spec: &EnvironmentSpec) -> Result<()> {
        let Some(gateway) = &spec.gateway else {
            return Ok(());
        };
        if !crate::moonlight::valid_unique_id(&gateway.unique_id) {
            bail!("{:?} isn't a Moonlight host id", gateway.unique_id);
        }
        let cert = crate::moonlight::dir(&self.config.data_root)
            .join("hosts")
            .join(&gateway.unique_id)
            .join("server-cert.pem");
        if !cert.exists() {
            bail!(
                "this node isn't paired with that Moonlight host: adopt it in the portal (Admin → Moonlight) to pair"
            );
        }
        Ok(())
    }

    /// The gateway's container for a Moonlight app (ADR 0008): the streamer's
    /// arguments plus the host to stream from, on the host network, with no
    /// GPU and no pads, and the node's Moonlight identity read-only.
    fn gateway_config(&self, spec: &EnvironmentSpec, port: u16) -> Value {
        let gateway = spec
            .gateway
            .as_ref()
            .expect("a gateway launch carries its host");
        let mut cmd: Vec<String> = [
            "--listen",
            "127.0.0.1",
            "--http-port",
            &port.to_string(),
            "--webrtc-port",
            &(port + 1).to_string(),
            "--portal-key",
            &spec.portal_key,
            "--environment-id",
            &spec.id,
            "--width",
            &spec.width.to_string(),
            "--height",
            &spec.height.to_string(),
            "--fps",
            &spec.fps.to_string(),
        ]
        .map(String::from)
        .to_vec();
        if let Some(public) = &self.config.public_address {
            cmd.extend(["--public-address".to_string(), public.clone()]);
        }
        cmd.extend(
            [
                "--host",
                &gateway.address,
                "--host-http-port",
                &gateway.http_port.to_string(),
                "--host-https-port",
                &gateway.https_port.to_string(),
                "--host-unique-id",
                &gateway.unique_id,
                "--app-id",
                &gateway.app_id.to_string(),
                "--identity-dir",
                GATEWAY_IDENTITY_DIR,
            ]
            .map(String::from),
        );
        let mut labels = self.labels(&spec.id, "streamer", port);
        labels[LABEL_OWNER] = json!(spec.owner);
        labels[LABEL_TEMPLATE] = json!(spec.template);
        labels[LABEL_GATEWAY] = json!("1");
        json!({
            "Image": spec.image,
            "Cmd": cmd,
            "Env": ["RUST_LOG=info"],
            "Labels": labels,
            "HostConfig": {
                "NetworkMode": "host",
                "Mounts": [{
                    "Type": "bind",
                    "Source": crate::moonlight::dir(&self.config.data_root),
                    "Target": GATEWAY_IDENTITY_DIR,
                    "ReadOnly": true,
                    // Made by the agent when it paired: a missing source is a bug.
                    "BindOptions": { "CreateMountpoint": false },
                }],
                "CapDrop": ["ALL"],
                "SecurityOpt": ["no-new-privileges"],
                "RestartPolicy": { "Name": "no" },
                "Init": true,
            },
        })
    }

    /// The app's container. `hidraw` are the streamer's hidraw nodes
    /// ([`Self::streamer_hidraw`]): each is mounted from the input volume and
    /// allowed in the app's device cgroup.
    #[cfg(test)]
    fn app_config(&self, spec: &EnvironmentSpec, port: u16, hidraw: &[Hidraw]) -> Value {
        self.app_config_with(spec, port, hidraw, &HostPlan::default())
    }

    /// [`Self::app_config`] with the host options the node settled on
    /// ([`Self::plan_host`]).
    fn app_config_with(
        &self,
        spec: &EnvironmentSpec,
        port: u16,
        hidraw: &[Hidraw],
        plan: &HostPlan,
    ) -> Value {
        let card_gid = steam_primary_node(spec)
            .and_then(|card| render_gid(&card).or_else(|| self.probes.known_gid(&card)));
        let udmabuf = Path::new(UDMABUF_SYSFS).exists();
        // The groups that own the VM's devices, which the probes found
        // before the app was made ([`Self::create_containers`]).
        let vm_gids: Vec<u32> = vm_devices(spec, udmabuf)
            .iter()
            .filter_map(|d| d["PathOnHost"].as_str())
            // The probe's answer first: it stats the host's node, and
            // create_containers asks it before the app is made. The agent's
            // own stat (it may not have the node) is the fallback.
            .filter_map(|path| self.probes.known_gid(path).or_else(|| render_gid(path)))
            .collect();
        let mut groups: Vec<String> = Vec::new();
        for gid in self
            .app_render_gid(spec)
            .into_iter()
            .chain(card_gid)
            .chain(vm_gids)
        {
            if !groups.contains(&gid.to_string()) {
                groups.push(gid.to_string());
            }
        }
        let mut mounts = self.mounts(&spec.id, true);
        let list = mounts.as_array_mut().expect("mounts are a list");
        list.extend(self.storage_mounts(spec));
        list.extend(hidraw.iter().map(|node| hidraw_mount(&spec.id, node)));
        list.extend(plan.mounts.iter().cloned());
        // Proton copies nvngx.dll from here into its prefixes; CDI doesn't bring it.
        // Only NVIDIA's driver has it.
        if let Some(dir) = self
            .config
            .nvidia_wine_dir
            .as_ref()
            .filter(|_| device_kind(spec) == DeviceKind::Nvidia)
        {
            let dir = dir.to_string_lossy();
            list.push(json!({
                "Type": "bind",
                "Source": dir,
                "Target": dir,
                "ReadOnly": true,
                // It's the host's: never for Docker to make.
                "BindOptions": { "CreateMountpoint": false },
            }));
        }
        let mut env = app_env(spec.width, spec.height, spec.fps);
        // Where the app finds what its template shares, when it has any.
        if spec.storage.as_ref().is_some_and(|s| s.shared.is_some()) {
            env.push(format!(
                "CHA_SHARED_DIR={}",
                self.shared_location(spec.data_template()).1
            ));
        }
        // What it has to keep mounted of its own, for apps that check.
        let overlays = self.per_user_overlays(spec);
        if !overlays.is_empty() {
            let targets: Vec<&str> = overlays.iter().map(|o| o.target.as_str()).collect();
            env.push(format!("CHA_PER_USER_DIRS={}", targets.join(":")));
        }
        // A custom environment's own variables, after the node's (the names
        // the node sets were refused: `check_env`).
        if let Some(extra) = &spec.env {
            env.extend(extra.iter().map(|(name, value)| format!("{name}={value}")));
        }
        let mut host = app_host_config(spec.security, spec.shm_mb, self.config.vm_memory_mb);
        host["Mounts"] = mounts;
        // Input devices: the gamepads' nodes, the only ones it has.
        host["DeviceCgroupRules"] = self.device_cgroup_rules(hidraw);
        host["DeviceRequests"] = self.gpu(spec);
        host["GroupAdd"] = json!(groups);
        let mut config = json!({
            "Image": spec.image,
            "User": app_user(),
            "Env": env,
            "Labels": self.app_labels(spec, port),
            "HostConfig": host,
        });
        // A render node, and for Steam its primary node too; the app's input
        // devices come through the cgroup rules and the volumes.
        let mut devices = self.gpu_devices(spec);
        devices.extend(vm_devices(spec, udmabuf));
        if let Some(card) = steam_primary_node(spec) {
            devices.push(json!({
                "PathOnHost": card,
                "PathInContainer": card,
                "CgroupPermissions": "rw",
            }));
        }
        // The owner's devices, the same path in the container.
        for dev in &plan.devices {
            if !devices.iter().any(|d| d["PathInContainer"] == *dev) {
                devices.push(json!({
                    "PathOnHost": dev,
                    "PathInContainer": dev,
                    "CgroupPermissions": "rwm",
                }));
            }
        }
        if !devices.is_empty() {
            config["HostConfig"]["Devices"] = json!(devices);
        }
        plan.apply(&mut config);
        config
    }

    /// What the app's devices may be opened: input devices, and exactly the
    /// streamer's hidraw nodes.
    fn device_cgroup_rules(&self, hidraw: &[Hidraw]) -> Value {
        let mut rules = Vec::new();
        if self.has_pads() {
            rules.push("c 13:* rw".to_string());
        }
        rules.extend(
            hidraw
                .iter()
                .map(|n| format!("c {}:{} rwm", n.major, n.minor)),
        );
        json!(rules)
    }

    async fn stop_environment(&self, id: &str) -> Result<()> {
        self.state
            .lock()
            .expect("state lock")
            .stopping
            .insert(id.to_string());
        let result = self.remove(id).await;
        let mut state = self.state.lock().expect("state lock");
        state.stopping.remove(id);
        state.ports.remove(id);
        state.gamestream.remove(id);
        state.meta.remove(id);
        state.homes.remove(id);
        state.overlays.remove(id);
        result
    }

    /// Removes an environment's containers (app first) and its volumes: the
    /// runtime directory and the gamepads'. Its home volume, if it has one, is
    /// the user's and stays (`docker volume rm` it to start over).
    async fn remove(&self, id: &str) -> Result<()> {
        // Each step goes on whatever the one before said, so one stuck
        // container doesn't leave the rest behind; the first error is the answer.
        let mut first: Option<anyhow::Error> = None;
        let mut note = |result: Result<()>| {
            if let Err(err) = result {
                first.get_or_insert(err);
            }
        };
        match self.docker.list(&format!("{LABEL_ENV}={id}")).await {
            Ok(containers) => {
                let mut ordered: Vec<_> = containers.iter().collect();
                ordered
                    .sort_by_key(|c| c.labels.get(LABEL_ROLE).map(String::as_str) != Some("app"));
                for container in ordered {
                    let grace = stop_grace(&container.labels);
                    note(self.docker.remove(&container.id, grace).await);
                }
            }
            Err(err) => note(Err(err)),
        }
        for kind in ["input", "udev"] {
            note(
                self.docker
                    .remove_volume(&format!("{}-{kind}", volume_name(id)))
                    .await,
            );
        }
        note(self.docker.remove_volume(&volume_name(id)).await);
        // The shares its app mounted, made for this launch. Found by name, so
        // an agent that restarted still removes them.
        match self
            .docker
            .volumes_named(&format!("{HOST_VOLUME_PREFIX}{id}-"))
            .await
        {
            Ok(volumes) => {
                for volume in volumes.iter().filter(|v| is_host_volume_of(v, id)) {
                    note(self.docker.remove_volume(volume).await);
                }
            }
            Err(err) => note(Err(err)),
        }
        first.map_or(Ok(()), Err)
    }

    async fn connect_environment(&self, request: Connect) -> Result<Value> {
        if !CODECS.contains(&request.codec.as_str()) {
            bail!("unknown codec {:?}", request.codec);
        }
        let port = self
            .state
            .lock()
            .expect("state lock")
            .ports
            .get(&request.environment_id)
            .copied()
            .ok_or_else(|| anyhow!("environment {} isn't running here", request.environment_id))?;
        let path = format!(
            "/webrtc/media?name=live-{}&secs=0&token={}",
            request.codec,
            encode(&request.media_token)
        );
        local(Method::POST, port, &path, Some(&request.offer)).await
    }

    async fn running_ids(&self) -> Result<Vec<String>> {
        let mut ids: HashMap<String, bool> = HashMap::new();
        for c in self.docker.list(LABEL_ENV).await? {
            if let Some(id) = c.labels.get(LABEL_ENV) {
                *ids.entry(id.clone()).or_insert(true) &= c.state == "running";
            }
        }
        let mut running: Vec<String> = ids
            .into_iter()
            .filter(|(_, up)| *up)
            .map(|(id, _)| id)
            .collect();
        running.sort();
        Ok(running)
    }
}

impl Runtime for DockerRuntime {
    fn engine(&self) -> Option<Docker> {
        Some(self.docker.clone())
    }

    fn start(&self, spec: EnvironmentSpec) -> BoxFuture<'_, Result<StreamerEndpoint>> {
        Box::pin(self.start_environment(spec))
    }

    fn stop(&self, id: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.stop_environment(&id).await })
    }

    fn running(&self) -> BoxFuture<'_, Result<Vec<String>>> {
        Box::pin(self.running_ids())
    }

    fn vaapi_devices(&self) -> BoxFuture<'_, Vec<cha_wire::Device>> {
        Box::pin(async {
            self.probes
                .devices(&self.docker, &self.config.streamer_image)
                .await
        })
    }

    fn cpu_codecs(&self) -> BoxFuture<'_, Option<Vec<String>>> {
        Box::pin(async {
            self.probes
                .cpu_codecs(&self.docker, &self.config.streamer_image)
                .await
        })
    }

    fn connect(&self, request: Connect) -> BoxFuture<'_, Result<Value>> {
        Box::pin(self.connect_environment(request))
    }

    fn streamer_http_port(&self, environment_id: &str) -> Option<u16> {
        self.state
            .lock()
            .expect("state lock")
            .ports
            .get(environment_id)
            .copied()
    }

    fn streamer_info(&self, environment_id: String) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let port = self
                .state
                .lock()
                .expect("state lock")
                .ports
                .get(&environment_id)
                .copied()
                .ok_or_else(|| anyhow!("environment {environment_id} isn't running here"))?;
            local(Method::GET, port, "/info", None).await
        })
    }

    fn exits(&self) -> broadcast::Receiver<Exit> {
        self.exits.subscribe()
    }

    fn progress(&self) -> broadcast::Receiver<Progress> {
        self.progress.subscribe()
    }

    fn warnings(&self) -> broadcast::Receiver<Warning> {
        self.warnings.subscribe()
    }

    fn images(&self) -> BoxFuture<'_, Vec<String>> {
        Box::pin(async {
            self.docker.images().await.unwrap_or_else(|err| {
                warn!("listing the engine's images: {err:#}");
                Vec::new()
            })
        })
    }

    fn pulled(&self) -> broadcast::Receiver<()> {
        self.pulled.subscribe()
    }

    fn warnings_now(&self) -> Vec<Warning> {
        let state = self.state.lock().expect("state lock");
        state
            .overlays
            .iter()
            .map(|(id, o)| Warning {
                id: id.clone(),
                warning: o.warning(),
            })
            .collect()
    }

    fn data_root(&self) -> Option<String> {
        Some(self.data.path().to_string_lossy().into_owned())
    }

    fn shared_dirs(&self) -> BTreeMap<String, String> {
        self.config
            .shared_dirs
            .iter()
            .map(|(template, dir)| (template.clone(), dir.to_string_lossy().into_owned()))
            .collect()
    }

    fn delete_user_data(&self, user: String, template: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.delete_user_data(user, template))
    }

    fn host_policy(&self) -> HostPolicy {
        self.config.host_options.policy()
    }

    fn host_ports(&self, id: String) -> BoxFuture<'_, Vec<HostPort>> {
        Box::pin(async move {
            self.published_ports(&id).await.unwrap_or_else(|err| {
                warn!(%id, "reading the published ports: {err:#}");
                Vec::new()
            })
        })
    }
}

/// Tells `cha-run` which capabilities to keep as it drops to uid 1000.
const AMBIENT_CAPS_ENV: &str = "CHA_AMBIENT_CAPS";

/// What `cha-run` needs, besides the app's own capabilities, to drop to uid
/// 1000 and trim its bounding set to them.
const DROP_CAPS: [&str; 3] = ["SETUID", "SETGID", "SETPCAP"];

/// Whether `image` can be started as root for added capabilities: only one
/// whose entrypoint is our base's `cha-run` and whose user is `cha`, so it is
/// `cha-run` that runs as root and drops to uid 1000 before the app starts.
/// Any other entrypoint would run as root itself.
fn takes_added_caps(image: &str, inspect: Option<&Value>) -> Result<()> {
    let config = inspect.map(|i| &i["Config"]);
    let entrypoint = config.and_then(|c| c["Entrypoint"].as_array());
    let user = config.and_then(|c| c["User"].as_str()).unwrap_or_default();
    let ours = entrypoint.is_some_and(|e| e.len() == 1 && e[0] == "cha-run")
        && matches!(user, "cha" | "1000" | "1000:1000" | "cha:cha");
    if !ours {
        bail!(
            "host options refused: {image} can't take added capabilities: only an image built on the Cha base, with its cha-run entrypoint and user cha, starts as root to keep them"
        );
    }
    Ok(())
}

/// What a custom environment's host options come to on this node: the
/// settled form of [`HostOptions`], ready for the app's container.
#[derive(Debug, Default)]
struct HostPlan {
    /// Docker `Mounts` entries.
    mounts: Vec<Value>,
    ports: Vec<PlannedPort>,
    cap_add: Vec<String>,
    /// Host paths, passed through at the same path.
    devices: Vec<String>,
    privileged: bool,
    network_host: bool,
    security_opt: Vec<String>,
}

/// A port the app publishes; `host` is `None` for one the engine chooses.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedPort {
    container: u16,
    protocol: Protocol,
    host: Option<u16>,
}

impl HostPlan {
    /// Adds it all to the app's container configuration. The profile's
    /// `CapDrop` stays; privileged, when asked, overrides it.
    fn apply(&self, config: &mut Value) {
        if !self.cap_add.is_empty() {
            // A process that isn't root holds no capabilities, whatever the
            // container may have: the app starts as root with only these
            // and what `cha-run` needs to drop to uid 1000 keeping them
            // (ambient), which it does before anything of the image runs.
            config["User"] = json!("0:0");
            if let Some(env) = config["Env"].as_array_mut() {
                env.push(json!(format!(
                    "{AMBIENT_CAPS_ENV}={}",
                    self.cap_add.join(",")
                )));
            }
            let mut caps = self.cap_add.clone();
            for cap in DROP_CAPS {
                if !caps.iter().any(|c| c == cap) {
                    caps.push(cap.to_string());
                }
            }
            config["HostConfig"]["CapAdd"] = json!(caps);
        }
        let host = &mut config["HostConfig"];
        if self.privileged {
            host["Privileged"] = json!(true);
        }
        if self.network_host {
            host["NetworkMode"] = json!("host");
        }
        if !self.security_opt.is_empty()
            && let Some(options) = host["SecurityOpt"].as_array_mut()
        {
            options.extend(self.security_opt.iter().map(|o| json!(o)));
        }
        if !self.ports.is_empty() {
            let mut exposed = serde_json::Map::new();
            let mut bindings = serde_json::Map::new();
            for p in &self.ports {
                let key = format!("{}/{}", p.container, p.protocol.as_str());
                exposed.insert(key.clone(), json!({}));
                // An empty HostIp is every address; an empty HostPort, the
                // engine's choice.
                bindings.insert(
                    key,
                    json!([{
                        "HostIp": "",
                        "HostPort": p.host.map(|h| h.to_string()).unwrap_or_default(),
                    }]),
                );
            }
            config["ExposedPorts"] = Value::Object(exposed);
            config["HostConfig"]["PortBindings"] = Value::Object(bindings);
        }
    }
}

/// `a` is `b` or under it.
fn path_under(a: &str, b: &str) -> bool {
    b == "/" || a == b || a.strip_prefix(b).is_some_and(|r| r.starts_with('/'))
}

/// Whether `volume` is `cha-hostvol-<id>-<n>`; not another environment's whose
/// id starts with this one's.
fn is_host_volume_of(volume: &str, id: &str) -> bool {
    volume
        .strip_prefix(HOST_VOLUME_PREFIX)
        .and_then(|rest| rest.strip_prefix(id))
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// Host ports in use: published by the apps of environments other than
/// `own`, or in `reserved` (the agent's own, either protocol).
fn used_ports(
    containers: &[ContainerSummary],
    own: &str,
    reserved: &[u16],
) -> HashSet<(u16, Protocol)> {
    let mut used: HashSet<(u16, Protocol)> = reserved
        .iter()
        .flat_map(|p| [(*p, Protocol::Tcp), (*p, Protocol::Udp)])
        .collect();
    for c in containers
        .iter()
        .filter(|c| c.labels.get(LABEL_ENV).map(String::as_str) != Some(own))
    {
        used.extend(
            published(&c.ports)
                .into_iter()
                .filter_map(|p| p.host.map(|h| (h, p.protocol))),
        );
    }
    used
}

/// The ports a container publishes, once each (the engine lists an IPv4 and
/// an IPv6 binding separately).
fn published(ports: &[crate::docker::ContainerPort]) -> Vec<HostPort> {
    let mut out: Vec<HostPort> = Vec::new();
    for p in ports {
        let protocol = match p.protocol.as_str() {
            "tcp" => Protocol::Tcp,
            "udp" => Protocol::Udp,
            _ => continue,
        };
        let Some(host) = p.public_port else { continue };
        let port = HostPort {
            container: p.private_port,
            protocol,
            host: Some(host),
        };
        if !out.contains(&port) {
            out.push(port);
        }
    }
    out.sort_by_key(|p| (p.container, p.protocol));
    out
}

/// Settles each requested port's host side: the one asked for, if free, else
/// the first free one in the owner's ranges for its protocol; the engine's
/// choice (`None`) when the owner set no range for it (`full` mode only: an
/// allowlist without one was already refused).
fn assign_ports(
    requested: &[HostPort],
    ranges: &[PortRange],
    used: &HashSet<(u16, Protocol)>,
    reserved: &[u16],
) -> Result<Vec<PlannedPort>> {
    let mut taken = used.clone();
    let mut out = Vec::new();
    for want in requested {
        let proto = want.protocol.as_str();
        let host = match want.host {
            Some(host) => {
                if taken.contains(&(host, want.protocol)) {
                    let by = if reserved.contains(&host) {
                        "this agent's streamers"
                    } else {
                        "another environment on this node"
                    };
                    bail!("host options refused: port {host}/{proto} is already used by {by}");
                }
                Some(host)
            }
            None => {
                let mine: Vec<&PortRange> = ranges
                    .iter()
                    .filter(|r| r.protocol == want.protocol)
                    .collect();
                if mine.is_empty() {
                    None
                } else {
                    let found = mine
                        .iter()
                        .flat_map(|r| r.start..=r.end)
                        .find(|p| !taken.contains(&(*p, want.protocol)));
                    match found {
                        Some(p) => Some(p),
                        None => bail!(
                            "host options refused: every {proto} port this node allows is in use (CHA_HOST_PORTS)"
                        ),
                    }
                }
            }
        };
        if let Some(host) = host {
            taken.insert((host, want.protocol));
        }
        out.push(PlannedPort {
            container: want.container,
            protocol: want.protocol,
            host,
        });
    }
    Ok(out)
}

/// Whether the shared directory the owner keeps at `dir` can be mounted: see
/// [`crate::storage::check_external_dir`]. The reason, if not. Looking at a
/// share that is away can wait for as long as it is, so this gives up.
async fn check_external(dir: PathBuf, per_user: Vec<String>) -> std::result::Result<(), String> {
    let looked = tokio::time::timeout(
        EXTERNAL_CHECK_TIMEOUT,
        blocking(move || crate::storage::check_external_dir(&dir, &per_user)),
    )
    .await;
    match looked {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(format!("{err:#}")),
        Err(_) => Err(format!(
            "it didn't answer within {} s (a share that is away blocks the agent's reads)",
            EXTERNAL_CHECK_TIMEOUT.as_secs()
        )),
    }
}

/// Runs blocking file work off the async threads.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| anyhow!("file work stopped: {e}"))?
}

/// Whether the node may run `spec`: its home volume is a home and not, say,
/// this agent's own state; its storage is the layout's, for its own user and
/// template. Nothing is created or looked up before this.
fn check_storage(spec: &EnvironmentSpec) -> Result<()> {
    if let Some(home) = &spec.home
        && !is_home_volume_name(home)
    {
        bail!("{home:?} isn't the name of a home volume (cha-home-<user>-<template>)");
    }
    if let Some(storage) = &spec.storage {
        if spec.home.is_some() {
            bail!("a home volume and storage under the data root can't both be the home");
        }
        storage
            .check(&spec.owner, &spec.template)
            .map_err(|e| anyhow!("refusing the app's storage: {e}"))?;
    }
    Ok(())
}

/// The (user, data template) whose directory under the data root is the app's home.
fn home_key(spec: &EnvironmentSpec) -> Option<(String, String)> {
    spec.storage.as_ref()?.home.as_ref()?;
    Some((spec.owner.clone(), spec.data_template().to_string()))
}

/// Where a copy comes from.
enum CopySource {
    /// A Docker volume, read-only.
    Volume(String),
    /// The app image's own `/home/cha`.
    Image,
}

/// A short-lived container that copies `source` into the host directory
/// `dest` with `cp -a` (owners, modes, links, xattrs), as root with just the
/// capabilities that takes: no network, a read-only root filesystem.
fn copy_config(image: &str, source: &CopySource, dest: &Path) -> Value {
    let mut mounts = vec![json!({
        "Type": "bind",
        "Source": dest.to_string_lossy(),
        "Target": "/to",
        "BindOptions": { "CreateMountpoint": false },
    })];
    let from = match source {
        CopySource::Volume(name) => {
            mounts.push(json!({
                "Type": "volume",
                "Source": name,
                "Target": "/from",
                "ReadOnly": true,
                "VolumeOptions": { "NoCopy": true },
            }));
            "/from"
        }
        CopySource::Image => APP_HOME,
    };
    json!({
        "Image": image,
        "User": "0:0",
        "Entrypoint": ["cp"],
        "Cmd": ["-a", format!("{from}/."), "/to/"],
        "WorkingDir": "/",
        "HostConfig": {
            "Mounts": mounts,
            "NetworkMode": "none",
            "ReadonlyRootfs": true,
            "CapDrop": ["ALL"],
            // Keep owners (CHOWN), read what the app made private
            // (DAC_OVERRIDE), keep modes and file capabilities and device
            // nodes.
            "CapAdd": ["CHOWN", "DAC_OVERRIDE", "FOWNER", "FSETID", "MKNOD", "SETFCAP"],
            "SecurityOpt": ["no-new-privileges"],
        },
    })
}

fn endpoint(http_port: u16) -> StreamerEndpoint {
    StreamerEndpoint {
        http_port,
        webrtc_port: http_port + 1,
        webtransport_port: http_port + 2,
    }
}

fn container_name(id: &str, role: &str) -> String {
    format!("cha-env-{id}-{role}")
}

/// The paths in a template's shared directory that each user has their own
/// copy of, from the catalog (`shared.perUser`): what an external shared
/// directory must have for them to be mounted over. The doctor checks them.
pub fn catalog_per_user(template: &str) -> Vec<String> {
    let catalog: Value =
        serde_json::from_str(include_str!("../../../images/catalog.json")).unwrap_or_default();
    catalog["templates"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["id"] == template)
        .and_then(|t| t["shared"]["perUser"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str().map(str::to_string))
        .collect()
}

/// The catalog's images (`images/catalog.json`, as the portal has it).
pub fn catalog_images() -> Vec<String> {
    let catalog: Value =
        serde_json::from_str(include_str!("../../../images/catalog.json")).unwrap_or_default();
    catalog["templates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            // The dev build when the catalog names one (ADR 0017), else the
            // published reference for this release.
            let image = t["localImage"].as_str().or_else(|| t["image"].as_str())?;
            Some(image.replace("{version}", AGENT_VERSION))
        })
        .collect()
}

fn volume_name(id: &str) -> String {
    format!("cha-env-{id}")
}

/// The app's home volume, for a launch that has one. Docker makes the volume
/// the first time something mounts it, with these labels, and while it is
/// empty fills it from the image's `/home/cha`: the files, and the directory's
/// own owner (`cha`, uid 1000) and mode. Later launches find it as it was left.
fn home_mount(spec: &EnvironmentSpec) -> Option<Value> {
    let name = spec.home.as_ref()?;
    let mut labels = serde_json::Map::new();
    labels.insert(LABEL_HOME.into(), json!("1"));
    if !spec.owner.is_empty() {
        labels.insert(LABEL_OWNER.into(), json!(spec.owner));
    }
    if !spec.template.is_empty() {
        labels.insert(LABEL_TEMPLATE.into(), json!(spec.template));
    }
    Some(json!({
        "Type": "volume",
        "Source": name,
        "Target": APP_HOME,
        "VolumeOptions": { "Labels": labels },
    }))
}

/// Whether this machine has an NVIDIA GPU, for hints.
pub(crate) fn nvidia_present() -> bool {
    crate::inventory::collect()
        .gpus
        .iter()
        .any(|g| g.vendor == "nvidia")
}

/// What `spec` runs on; `nvidia` when it says nothing, as before devices.
fn device_kind(spec: &EnvironmentSpec) -> DeviceKind {
    spec.device.as_ref().map_or(DeviceKind::Nvidia, |d| d.kind)
}

/// The render node a `vaapi` spec names, if it is a usable one.
fn vaapi_node(spec: &EnvironmentSpec) -> Option<&str> {
    spec.device
        .as_ref()
        .filter(|d| d.kind == DeviceKind::Vaapi)
        .and_then(|d| d.render_node.as_deref())
        .filter(|n| valid_render_node(n))
}

/// The primary node (`/dev/dri/card0`) of a Steam environment's VA-API
/// render node. Gamescope wants its GPU's primary node, not only the render
/// node: without it Vulkan reports "no primary node" and gamescope aborts.
/// Only Steam gets it, since a primary node can drive the host's displays.
/// (NVIDIA's comes through CDI.)
fn steam_primary_node(spec: &EnvironmentSpec) -> Option<String> {
    if spec.security != SecurityProfile::Steam {
        return None;
    }
    primary_node_in(Path::new("/sys/class/drm"), vaapi_node(spec)?)
}

/// The `card*` beside `render_node` under `sys` (`/sys/class/drm`), which
/// every container sees as the host's.
fn primary_node_in(sys: &Path, render_node: &str) -> Option<String> {
    let name = render_node.strip_prefix("/dev/dri/")?;
    let mut cards: Vec<String> = std::fs::read_dir(sys.join(name).join("device/drm"))
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| valid_dri_node(&format!("/dev/dri/{n}")) && n.starts_with("card"))
        .collect();
    cards.sort();
    cards.first().map(|n| format!("/dev/dri/{n}"))
}

/// Whether `path` is a DRM render node (`/dev/dri/renderD128`). Docker is
/// handed it as a device to give the app, so nothing else will do.
pub(crate) fn valid_render_node(path: &str) -> bool {
    path.strip_prefix("/dev/dri/renderD")
        .is_some_and(|n| (1..=4).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `path` is a DRM render node or primary node (`/dev/dri/card0`).
pub(crate) fn valid_dri_node(path: &str) -> bool {
    valid_render_node(path)
        || path
            .strip_prefix("/dev/dri/card")
            .is_some_and(|n| (1..=4).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Refuses a `vaapi` spec without a render node of the host's DRM kind.
fn check_device(spec: &EnvironmentSpec) -> Result<()> {
    check_device_on(spec, Path::new(KVM_SYSFS).exists())
}

/// [`check_device`], given whether the host has KVM.
fn check_device_on(spec: &EnvironmentSpec, kvm: bool) -> Result<()> {
    if device_kind(spec) == DeviceKind::Vaapi && vaapi_node(spec).is_none() {
        bail!("a vaapi environment needs a render node like /dev/dri/renderD128");
    }
    if spec.security == SecurityProfile::Vm && !kvm {
        bail!(
            "this node has no {KVM_DEVICE}: a virtual machine needs the host's kvm module loaded \
             and virtualisation on in its firmware"
        );
    }
    Ok(())
}

/// The render node's group id, from the device node (CDI passes it through
/// with the host's ownership).
fn render_gid(render_node: &str) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(render_node).ok().map(|m| m.gid())
}

/// The `browser` seccomp profile, as `SecurityOpt` takes it.
pub fn browser_seccomp() -> String {
    format!("seccomp={}", compact(BROWSER_SECCOMP))
}

/// The profile as one line: Docker takes it inline in `SecurityOpt`.
fn compact(json: &str) -> String {
    serde_json::from_str::<Value>(json)
        .map(|v| v.to_string())
        .unwrap_or_else(|_| json.to_string())
}

/// A request to a streamer's signalling port on this host.
/// A `hidraw` node the streamer made (its `/info`'s `hidraw`): the device's
/// kernel name and number, which the app's device cgroup needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hidraw {
    name: String,
    major: u32,
    minor: u32,
}

/// Whether `name` is a kernel `hidraw` name, `hidraw<n>`: what the node
/// mounts under `/dev` and puts into a path, so nothing else is let through.
fn valid_hidraw_name(name: &str) -> bool {
    name.strip_prefix("hidraw")
        .is_some_and(|n| !n.is_empty() && n.len() <= 9 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The nodes in a streamer's `/info` (`"hidraw": [{ name, major, minor }]`;
/// absent is none). One that isn't well formed refuses all: the streamer is
/// ours, so it is a bug, and a half-made controller is worse than a failed
/// start.
fn parse_hidraw(info: &Value) -> Result<Vec<Hidraw>> {
    let Some(list) = info.get("hidraw") else {
        return Ok(Vec::new());
    };
    let list = list
        .as_array()
        .ok_or_else(|| anyhow!("the streamer's hidraw isn't a list"))?;
    list.iter()
        .map(|entry| {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| valid_hidraw_name(n))
                .ok_or_else(|| anyhow!("the streamer named a hidraw node {entry}"))?;
            let number = |key: &str| {
                entry
                    .get(key)
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| anyhow!("the streamer gave hidraw node {name} no {key}"))
            };
            Ok(Hidraw {
                name: name.to_string(),
                major: number("major")?,
                minor: number("minor")?,
            })
        })
        .collect()
}

/// The mount that puts `node` at `/dev/<name>` in the app: the file
/// `hidraw/<name>` of the environment's input volume (the streamer's
/// `/run/cha-input/dev/hidraw/<name>`), as a volume subpath.
fn hidraw_mount(id: &str, node: &Hidraw) -> Value {
    json!({
        "Type": "volume",
        "Source": format!("{}-input", volume_name(id)),
        "Target": format!("/dev/{}", node.name),
        "ReadOnly": true,
        "VolumeOptions": { "Subpath": format!("hidraw/{}", node.name) },
    })
}

async fn local(method: Method, port: u16, path: &str, body: Option<&Value>) -> Result<Value> {
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .context("reaching the environment's streamer")?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let payload = match body {
        Some(body) => Bytes::from(serde_json::to_vec(body)?),
        None => Bytes::new(),
    };
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(Full::new(payload))?;
    let response = tokio::time::timeout(Duration::from_secs(10), sender.send_request(request))
        .await
        .context("the streamer didn't answer")??;
    let status = response.status();
    let bytes = response.into_body().collect().await?.to_bytes();
    if !status.is_success() {
        bail!(
            "the streamer refused ({status}): {}",
            String::from_utf8_lossy(&bytes).trim()
        );
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use axum::extract::{Request, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::{Json, Router};
    use cha_wire::GamepadKind;

    use super::*;

    fn runtime() -> DockerRuntime {
        runtime_on(Docker::new("/nonexistent"))
    }

    fn runtime_on(docker: Docker) -> DockerRuntime {
        runtime_with(
            docker,
            "/srv/cha-portal".into(),
            (1000, 1000),
            BTreeMap::new(),
        )
    }

    /// A runtime with its data root at `root`, whose apps run as `owner`
    /// (uid, gid; the tests' own, since only root can give files to someone else).
    fn runtime_with(
        docker: Docker,
        root: PathBuf,
        owner: (u32, u32),
        shared_dirs: BTreeMap<String, PathBuf>,
    ) -> DockerRuntime {
        let (exits, _) = broadcast::channel(1);
        let (progress, _) = broadcast::channel(16);
        let (warnings, _) = broadcast::channel(16);
        let (pulled, _) = broadcast::channel(8);
        DockerRuntime {
            docker,
            config: DockerConfig {
                streamer_image: "cha/streamer:dev".into(),
                render_node: "/dev/dri/renderD128".into(),
                gpu_device: "nvidia.com/gpu=all".into(),
                uinput: Some("/dev/uinput".into()),
                uhid: Some("/dev/uhid".into()),
                public_address: None,
                port_base: 7600,
                gamestream_port_base: None,
                max_environments: 2,
                data_root: root.clone(),
                shared_dirs,
                nvidia_wine_dir: None,
                vm_memory_mb: DEFAULT_VM_MEMORY_MB,
                log_dir: None,
                app_images: None,
                gateway_image: "cha/gateway:dev".into(),
                host_options: HostOptionsConfig::default(),
            },
            render_gid: Some(992),
            probes: DeviceProbes::default(),
            data: DataRoot::new(root, owner.0, owner.1).unwrap(),
            state: Mutex::default(),
            exits,
            progress,
            warnings,
            pulled,
            ports_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn spec(security: SecurityProfile) -> EnvironmentSpec {
        EnvironmentSpec {
            id: "e1".into(),
            image: "cha/env-chrome:dev".into(),
            image_candidates: Vec::new(),
            security,
            shm_mb: 1024,
            width: 2560,
            height: 1440,
            fps: 60,
            portal_key: "cG9ydGFs".into(),
            home: None,
            owner: "u1".into(),
            template: "chrome".into(),
            storage: None,
            gamepad: None,
            device: None,
            gateway: None,
            env: None,
            host: None,
        }
    }

    fn steam_spec() -> EnvironmentSpec {
        EnvironmentSpec {
            id: "e1".into(),
            image: "cha/env-steam:dev".into(),
            home: Some("cha-home-u1-steam".into()),
            template: "steam".into(),
            ..spec(SecurityProfile::Steam)
        }
    }

    #[test]
    fn maps_catalog_images_to_their_published_copies() {
        let p = PublishedImages::from_settings(Some("ghcr.io/ban-red/"), Some("v0.1.0"))
            .unwrap()
            .unwrap();
        assert_eq!(
            p.image_for("cha/env-chrome:dev").as_deref(),
            Some("ghcr.io/ban-red/cha-env-chrome:0.1.0")
        );
        assert_eq!(
            p.image_for("cha/env-steam").as_deref(),
            Some("ghcr.io/ban-red/cha-env-steam:0.1.0")
        );
        assert_eq!(p.image_for("ubuntu:26.04"), None);
        assert_eq!(p.image_for("ghcr.io/x/y:1"), None);
    }

    #[test]
    fn reads_the_published_image_settings() {
        assert_eq!(
            PublishedImages::from_settings(None, Some("0.1.0")).unwrap(),
            None
        );
        assert_eq!(
            PublishedImages::from_settings(Some(" "), None).unwrap(),
            None
        );
        assert!(PublishedImages::from_settings(Some("ghcr.io/ban-red"), None).is_err());
        assert!(PublishedImages::from_settings(Some("ghcr.io/ban-red"), Some("")).is_err());
        assert!(PublishedImages::from_settings(Some("ghcr.io/ban-red"), Some("1 2")).is_err());
        // A bare namespace would be Docker Hub's.
        assert!(PublishedImages::from_settings(Some("ban-red"), Some("0.1.0")).is_err());
        assert!(PublishedImages::from_settings(Some("ghcr.io/Ban Red"), Some("0.1.0")).is_err());
        assert!(
            PublishedImages::from_settings(Some("localhost:5000/cha"), Some("sha-1a2b3c4"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_vm_gets_kvm_its_groups_a_memory_limit_and_a_longer_stop() {
        let mut rt = runtime();
        rt.probes.remember_gid(KVM_DEVICE, 993);
        rt.probes.remember_gid(UDMABUF_DEVICE, 994);
        rt.config.vm_memory_mb = 8192;
        let vm = spec(SecurityProfile::Vm);
        let app = rt.app_config(&vm, 7600, &[]);
        let host = &app["HostConfig"];
        // Docker's default seccomp and AppArmor, and no capabilities.
        let opts = host["SecurityOpt"].as_array().unwrap();
        assert_eq!(opts, &[json!("no-new-privileges")]);
        assert_eq!(host["CapDrop"], json!(["ALL"]));
        // The limit covers swap too.
        assert_eq!(host["Memory"], 8192u64 * 1024 * 1024);
        assert_eq!(host["MemorySwap"], host["Memory"]);
        let paths: Vec<&str> = host["Devices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["PathOnHost"].as_str().unwrap())
            .collect();
        assert!(paths.contains(&"/dev/kvm"));
        assert_eq!(
            paths.contains(&"/dev/udmabuf"),
            Path::new(UDMABUF_SYSFS).exists()
        );
        let groups = host["GroupAdd"].as_array().unwrap();
        assert!(groups.contains(&json!("993")));
        assert_eq!(
            groups.contains(&json!("994")),
            Path::new(UDMABUF_SYSFS).exists()
        );
        assert_eq!(app["Labels"][LABEL_PROFILE], "vm");

        // The devices, with and without udmabuf; others get none.
        let devices = |udmabuf| vm_devices(&vm, udmabuf);
        assert_eq!(devices(false).len(), 1);
        assert_eq!(devices(true)[1]["PathOnHost"], "/dev/udmabuf");
        assert_eq!(devices(true)[1]["CgroupPermissions"], "rw");
        assert!(vm_devices(&spec(SecurityProfile::Browser), true).is_empty());

        // Others have no limit, no VM label and the short stop.
        let chrome = rt.app_config(&spec(SecurityProfile::Browser), 7600, &[]);
        assert!(chrome["HostConfig"].get("Memory").is_none());
        assert!(chrome["Labels"].get(LABEL_PROFILE).is_none());
        let labels = |app: &Value| -> std::collections::HashMap<String, String> {
            serde_json::from_value(app["Labels"].clone()).unwrap()
        };
        assert_eq!(stop_grace(&labels(&app)), 90);
        assert_eq!(stop_grace(&labels(&chrome)), 5);
        assert_eq!(stop_grace(&Default::default()), 5);
    }

    #[test]
    fn a_vm_needs_a_node_with_kvm() {
        let vm = spec(SecurityProfile::Vm);
        assert!(check_device_on(&vm, true).is_ok());
        let err = check_device_on(&vm, false).unwrap_err().to_string();
        assert!(err.contains("this node has no /dev/kvm"), "{err}");
        assert!(check_device_on(&spec(SecurityProfile::Browser), false).is_ok());
    }

    #[test]
    fn steam_gets_the_sandbox_profile_and_its_home() {
        let rt = runtime();
        let app = rt.app_config(&steam_spec(), 7600, &[]);
        let opts = app["HostConfig"]["SecurityOpt"].as_array().unwrap();
        assert!(opts.iter().any(|o| o == "apparmor=cha-sandbox"));
        assert!(
            opts.iter()
                .any(|o| o.as_str().unwrap().starts_with("seccomp="))
        );
        let mounts = app["HostConfig"]["Mounts"].as_array().unwrap();
        let home = mounts.iter().find(|m| m["Target"] == "/home/cha").unwrap();
        assert_eq!(home["Type"], "volume");
        assert_eq!(home["Source"], "cha-home-u1-steam");
        assert!(home.get("ReadOnly").is_none(), "the app writes its home");
        assert_eq!(app["HostConfig"]["Ulimits"][0]["Name"], "nofile");
        // Others keep Docker's AppArmor profile and no home volume.
        let chrome = rt.app_config(&spec(SecurityProfile::Browser), 7600, &[]);
        let opts = chrome["HostConfig"]["SecurityOpt"].as_array().unwrap();
        assert!(
            !opts
                .iter()
                .any(|o| o.as_str().unwrap().starts_with("apparmor="))
        );
        let mounts = chrome["HostConfig"]["Mounts"].as_array().unwrap();
        assert!(!mounts.iter().any(|m| m["Target"] == "/home/cha"));
    }

    #[test]
    fn allocates_port_triples_until_full() {
        let rt = runtime();
        assert_eq!(rt.allocate("a").unwrap(), 7600);
        assert_eq!(rt.allocate("b").unwrap(), 7603);
        assert!(rt.allocate("c").is_err());
        rt.state.lock().unwrap().ports.remove("a");
        assert_eq!(rt.allocate("c").unwrap(), 7600);
    }

    fn with_gamestream(base: u16) -> DockerRuntime {
        let mut rt = runtime();
        rt.config.gamestream_port_base = Some(base);
        rt
    }

    #[test]
    fn gamestream_ports_follow_the_streamers_slots() {
        let rt = with_gamestream(7700);
        let a = rt.allocate("a").unwrap();
        let b = rt.allocate("b").unwrap();
        assert_eq!((a, b), (7600, 7603));
        rt.allocate_gamestream("a", a).unwrap();
        rt.allocate_gamestream("b", b).unwrap();
        let first = rt.gamestream_access("a").unwrap();
        let second = rt.gamestream_access("b").unwrap();
        assert_eq!(
            (first.http_port, first.ports.to_string()),
            (7600, "7700,7701,7702".into())
        );
        assert_eq!(
            (second.http_port, second.ports.to_string()),
            (7603, "7703,7704,7705".into())
        );
        assert_eq!(first.secret.len(), 32);
        assert!(first.secret.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first.secret, second.secret, "a secret each");
        // The slot comes back with the environment.
        rt.state.lock().unwrap().ports.remove("a");
        rt.state.lock().unwrap().gamestream.remove("a");
        let c = rt.allocate("c").unwrap();
        rt.allocate_gamestream("c", c).unwrap();
        assert_eq!(rt.gamestream_access("c").unwrap().ports.video, 7700);
    }

    #[test]
    fn the_streamer_gets_the_ports_the_secret_and_labels_when_gamestream_is_on() {
        let rt = with_gamestream(7700);
        rt.allocate_gamestream("e1", 7603).unwrap();
        let access = rt.gamestream_access("e1").unwrap();
        let s = rt.streamer_config(&spec(SecurityProfile::Standard), 7603);
        let cmd = cmd_of(&s);
        let at = cmd.iter().position(|a| *a == "--gamestream-ports").unwrap();
        assert_eq!(cmd[at + 1], "7703,7704,7705");
        assert!(
            !cmd.iter().any(|a| a.contains(&access.secret)),
            "the secret is never on the command line"
        );
        let env = s["Env"].as_array().unwrap();
        assert!(env.contains(&json!(format!("CHA_GAMESTREAM_SECRET={}", access.secret))));
        let sp = spec(SecurityProfile::Standard);
        assert!(
            env.contains(&json!(format!(
                "CHA_PAD_IDENTITY={}/{}",
                sp.owner, sp.template
            ))),
            "a Steam Controller's serial follows its owner and app"
        );
        assert_eq!(s["Labels"][LABEL_GAMESTREAM_PORTS], "7703,7704,7705");
        // Those labels bring the access back after an agent restart.
        let labels: std::collections::HashMap<String, String> = s["Labels"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();
        assert_eq!(GameStreamAccess::from_labels(7603, &labels), Some(access));
        // The app container knows nothing of it.
        let app = rt.app_config(&spec(SecurityProfile::Standard), 7603, &[]);
        assert!(!app.to_string().contains("gamestream") && !app.to_string().contains("GAMESTREAM"));
    }

    #[test]
    fn with_gamestream_off_nothing_changes() {
        let rt = runtime();
        let port = rt.allocate("e1").unwrap();
        rt.allocate_gamestream("e1", port).unwrap();
        assert!(rt.gamestream_access("e1").is_none());
        let sp = spec(SecurityProfile::Standard);
        let s = rt.streamer_config(&sp, port);
        assert!(!cmd_of(&s).iter().any(|a| a.contains("gamestream")));
        assert_eq!(
            s["Env"],
            json!([
                format!("XDG_RUNTIME_DIR={RUNTIME_DIR}"),
                "RUST_LOG=info,smithay=warn,str0m=warn",
                format!("CHA_PAD_IDENTITY={}/{}", sp.owner, sp.template),
            ])
        );
        assert!(s["Labels"].get(LABEL_GAMESTREAM_PORTS).is_none());
        assert!(s["Labels"].get(LABEL_GAMESTREAM_SECRET).is_none());
        // And an environment without it, on a node with it on, is no different from before.
        let rt = with_gamestream(7700);
        let s = rt.streamer_config(&spec(SecurityProfile::Standard), 7600);
        assert!(!cmd_of(&s).iter().any(|a| a.contains("gamestream")));
    }

    #[test]
    fn the_gamestream_range_stays_clear_of_the_streamers() {
        assert!(check_gamestream_range(7600, 7700, 16).is_ok());
        assert!(
            check_gamestream_range(7600, 7648, 16).is_ok(),
            "right after"
        );
        assert!(check_gamestream_range(7600, 7647, 16).is_err(), "one short");
        assert!(check_gamestream_range(7700, 7600, 16).is_ok(), "before");
        assert!(check_gamestream_range(7700, 7660, 16).is_err());
        assert!(check_gamestream_range(7600, 7600, 1).is_err(), "the same");
        assert!(
            check_gamestream_range(7600, 65530, 16).is_err(),
            "off the end"
        );
    }

    #[test]
    fn ports_parse_back_from_what_the_streamer_takes() {
        let ports: GameStreamPorts = "7700,7701,7702".parse().unwrap();
        assert_eq!(
            (ports.video, ports.control, ports.audio),
            (7700, 7701, 7702)
        );
        assert_eq!(ports.to_string(), "7700,7701,7702");
        for bad in ["", "1,2", "1,2,3,4", "a,b,c"] {
            assert!(bad.parse::<GameStreamPorts>().is_err(), "{bad}");
        }
    }

    #[test]
    fn the_access_keeps_its_secret_out_of_debug_output() {
        let access = GameStreamAccess {
            http_port: 1,
            ports: GameStreamPorts {
                video: 2,
                control: 3,
                audio: 4,
            },
            secret: "s3cr3t-value".into(),
        };
        assert!(!format!("{access:?}").contains("s3cr3t"));
    }

    #[test]
    fn apps_are_confined() {
        let rt = runtime();
        let app = rt.app_config(&spec(SecurityProfile::Standard), 7600, &[]);
        assert_eq!(app["User"], "1000:1000");
        assert_eq!(app["HostConfig"]["CapDrop"], json!(["ALL"]));
        assert_eq!(
            app["HostConfig"]["SecurityOpt"],
            json!(["no-new-privileges"])
        );
        assert_eq!(app["HostConfig"]["GroupAdd"], json!(["992"]));
        assert_eq!(app["HostConfig"]["ShmSize"], 1024 * 1024 * 1024);
        assert!(app["HostConfig"].get("NetworkMode").is_none());

        let browser = rt.app_config(&spec(SecurityProfile::Browser), 7600, &[]);
        let opts = browser["HostConfig"]["SecurityOpt"].as_array().unwrap();
        let seccomp = opts[1].as_str().unwrap().strip_prefix("seccomp=").unwrap();
        let profile: Value = serde_json::from_str(seccomp).unwrap();
        assert_eq!(profile["defaultAction"], "SCMP_ACT_ERRNO");
        assert!(seccomp.contains("\"unshare\""));
    }

    #[test]
    fn streamers_get_their_ports_and_the_gpu() {
        let rt = runtime();
        let s = rt.streamer_config(&spec(SecurityProfile::Standard), 7602);
        let cmd: Vec<&str> = s["Cmd"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let arg = |name: &str| cmd[cmd.iter().position(|a| *a == name).unwrap() + 1];
        assert_eq!(arg("--http-port"), "7602");
        assert_eq!(arg("--webrtc-port"), "7603");
        assert_eq!(arg("--wt-port"), "7604");
        assert_eq!(arg("--app-uid"), "1000");
        assert_eq!(arg("--listen"), "127.0.0.1");
        assert_eq!(arg("--portal-key"), "cG9ydGFs");
        assert_eq!(arg("--environment-id"), "e1");
        assert!(!cmd.contains(&"--token"));
        assert_eq!(s["HostConfig"]["NetworkMode"], "host");
        assert_eq!(
            s["HostConfig"]["DeviceRequests"][0]["DeviceIDs"][0],
            "nvidia.com/gpu=all"
        );
        assert_eq!(s["Labels"]["sh.cha.http-port"], "7602");
    }

    fn gateway_spec() -> EnvironmentSpec {
        EnvironmentSpec {
            image: "cha/gateway:dev".into(),
            template: "moonlight:h1:881448767".into(),
            gateway: Some(Box::new(cha_wire::GatewaySpec {
                unique_id: "AB12".into(),
                address: "192.168.1.9".into(),
                http_port: 47989,
                https_port: 47984,
                app_id: 881448767,
            })),
            ..steam_spec()
        }
    }

    #[test]
    fn a_gateway_is_one_labelled_container_with_the_identity_and_no_gpu() {
        let rt = runtime();
        let g = rt.gateway_config(&gateway_spec(), 7602);
        assert_eq!(g["Image"], "cha/gateway:dev");
        assert_eq!(
            cmd_of(&g),
            [
                "--listen",
                "127.0.0.1",
                "--http-port",
                "7602",
                "--webrtc-port",
                "7603",
                "--portal-key",
                "cG9ydGFs",
                "--environment-id",
                "e1",
                "--width",
                "2560",
                "--height",
                "1440",
                "--fps",
                "60",
                "--host",
                "192.168.1.9",
                "--host-http-port",
                "47989",
                "--host-https-port",
                "47984",
                "--host-unique-id",
                "AB12",
                "--app-id",
                "881448767",
                "--identity-dir",
                "/identity",
            ]
        );
        assert_eq!(g["Labels"]["sh.cha.role"], "streamer");
        assert_eq!(g["Labels"]["sh.cha.http-port"], "7602");
        assert_eq!(g["Labels"]["sh.cha.env"], "e1");
        assert_eq!(g["Labels"]["sh.cha.owner"], "u1");
        assert_eq!(g["Labels"]["sh.cha.template"], "moonlight:h1:881448767");
        let host = &g["HostConfig"];
        assert_eq!(host["NetworkMode"], "host");
        assert!(host.get("DeviceRequests").is_none() && host.get("Devices").is_none());
        let mounts = host["Mounts"].as_array().unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0]["Source"], "/srv/cha-portal/node/moonlight");
        assert_eq!(mounts[0]["Target"], "/identity");
        assert_eq!(mounts[0]["ReadOnly"], true);

        let mut rt = runtime();
        rt.config.public_address = Some("203.0.113.5".into());
        let cmd = cmd_of(&rt.gateway_config(&gateway_spec(), 7602)).join(" ");
        assert!(cmd.contains("--public-address 203.0.113.5 --host 192.168.1.9"));
    }

    #[tokio::test]
    async fn a_gateway_launch_makes_no_app_and_is_refused_unpaired() {
        let (engine, _dir, docker) = fake_engine();
        let root = tempfile::tempdir().unwrap();
        let rt = runtime_with(
            docker,
            root.path().to_path_buf(),
            (1000, 1000),
            BTreeMap::new(),
        );
        let err = rt.start_environment(gateway_spec()).await.unwrap_err();
        assert!(format!("{err:#}").contains("isn't paired"), "{err:#}");
        assert!(engine.requests.lock().unwrap().is_empty());

        let host = crate::moonlight::dir(root.path()).join("hosts/AB12");
        std::fs::create_dir_all(&host).unwrap();
        std::fs::write(host.join("server-cert.pem"), "x").unwrap();
        rt.start_environment(gateway_spec()).await.unwrap();
        let created = engine.created();
        assert!(!created.contains_key("cha-env-e1-app"));
        let g = &created["cha-env-e1-streamer"];
        assert_eq!(g["Labels"]["sh.cha.role"], "streamer");
        assert_eq!(created.len(), 1);
    }

    fn device_spec(kind: DeviceKind, render_node: Option<&str>) -> EnvironmentSpec {
        EnvironmentSpec {
            device: Some(Box::new(cha_wire::DeviceChoice {
                id: kind.as_str().into(),
                kind,
                render_node: render_node.map(String::from),
            })),
            ..steam_spec()
        }
    }

    fn cmd_of(config: &Value) -> Vec<&str> {
        config["Cmd"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect()
    }

    fn mounts_wine_dir(app: &Value) -> bool {
        app["HostConfig"]["Mounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["Target"] == "/usr/lib/nvidia/wine")
    }

    #[test]
    fn nvidia_is_the_default_device_and_goes_through_cdi() {
        let mut rt = runtime();
        rt.config.nvidia_wine_dir = Some("/usr/lib/nvidia/wine".into());
        for spec in [
            steam_spec(),
            device_spec(DeviceKind::Nvidia, Some("/dev/dri/renderD130")),
        ] {
            let s = rt.streamer_config(&spec, 7600);
            let cmd = cmd_of(&s);
            // What a streamer from before devices is started with.
            assert!(!cmd.contains(&"--device"));
            let at = cmd.iter().position(|a| *a == "--render-node").unwrap();
            assert_eq!(cmd[at + 1], "/dev/dri/renderD128");
            assert_eq!(
                s["HostConfig"]["DeviceRequests"][0]["DeviceIDs"][0],
                "nvidia.com/gpu=all"
            );
            let app = rt.app_config(&spec, 7600, &[]);
            assert_eq!(app["HostConfig"]["DeviceRequests"][0]["Driver"], "cdi");
            assert_eq!(app["HostConfig"]["GroupAdd"], json!(["992"]));
            assert!(app["HostConfig"].get("Devices").is_none());
            assert!(mounts_wine_dir(&app));
        }
    }

    #[test]
    fn vaapi_gives_the_streamer_and_the_app_one_render_node() {
        let mut rt = runtime();
        rt.config.nvidia_wine_dir = Some("/usr/lib/nvidia/wine".into());
        let spec = device_spec(DeviceKind::Vaapi, Some("/dev/dri/renderD129"));
        let s = rt.streamer_config(&spec, 7600);
        let cmd = cmd_of(&s);
        let at = cmd.iter().position(|a| *a == "--device").unwrap();
        assert_eq!(
            &cmd[at..at + 4],
            ["--device", "vaapi", "--render-node", "/dev/dri/renderD129"]
        );
        assert_eq!(cmd.iter().filter(|a| **a == "--render-node").count(), 1);
        assert_eq!(s["HostConfig"]["DeviceRequests"], json!([]));
        let node = |c: &Value| {
            c["HostConfig"]["Devices"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["PathOnHost"] == "/dev/dri/renderD129")
                .cloned()
        };
        let streamer_node = node(&s).expect("the streamer gets the render node");
        assert_eq!(streamer_node["PathInContainer"], "/dev/dri/renderD129");
        let app = rt.app_config(&spec, 7600, &[]);
        assert_eq!(app["HostConfig"]["DeviceRequests"], json!([]));
        assert_eq!(
            app["HostConfig"]["Devices"],
            json!([{
                "PathOnHost": "/dev/dri/renderD129",
                "PathInContainer": "/dev/dri/renderD129",
                "CgroupPermissions": "rw",
            }])
        );
        // Not the NVIDIA card's group, and nothing of NVIDIA's.
        assert_ne!(app["HostConfig"]["GroupAdd"], json!(["992"]));
        assert!(!mounts_wine_dir(&app));
    }

    #[test]
    fn cpu_gives_no_gpu_at_all() {
        let mut rt = runtime();
        rt.config.nvidia_wine_dir = Some("/usr/lib/nvidia/wine".into());
        let spec = device_spec(DeviceKind::Cpu, None);
        let s = rt.streamer_config(&spec, 7600);
        let cmd = cmd_of(&s);
        assert!(cmd.windows(2).any(|w| w == ["--device", "cpu"]));
        assert!(!cmd.contains(&"--render-node"));
        assert_eq!(s["HostConfig"]["DeviceRequests"], json!([]));
        assert!(
            !s["HostConfig"]["Devices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["PathOnHost"]
                    .as_str()
                    .is_some_and(|p| p.contains("/dev/dri")))
        );
        let app = rt.app_config(&spec, 7600, &[]);
        assert_eq!(app["HostConfig"]["DeviceRequests"], json!([]));
        assert!(app["HostConfig"].get("Devices").is_none());
        assert_eq!(app["HostConfig"]["GroupAdd"], json!([]));
        assert!(!mounts_wine_dir(&app));
    }

    #[test]
    fn steam_finds_the_primary_node_beside_its_render_node() {
        let sys = tempfile::tempdir().unwrap();
        let drm = sys.path().join("renderD129/device/drm");
        for n in ["renderD129", "controlD65", "card1"] {
            std::fs::create_dir_all(drm.join(n)).unwrap();
        }
        assert_eq!(
            primary_node_in(sys.path(), "/dev/dri/renderD129").as_deref(),
            Some("/dev/dri/card1")
        );
        assert_eq!(primary_node_in(sys.path(), "/dev/dri/renderD128"), None);
        assert!(valid_dri_node("/dev/dri/card0"));
        assert!(!valid_dri_node("/dev/dri/card0/../x"));
        assert!(!valid_dri_node("/dev/dri/controlD64"));
    }

    #[test]
    fn a_vaapi_environment_needs_a_render_node_of_the_hosts() {
        assert!(check_device(&steam_spec()).is_ok());
        assert!(check_device(&device_spec(DeviceKind::Cpu, None)).is_ok());
        assert!(check_device(&device_spec(DeviceKind::Vaapi, Some("/dev/dri/renderD129"))).is_ok());
        for bad in [
            None,
            Some("/dev/sda"),
            Some("/dev/dri/card0"),
            Some("/dev/dri/renderD"),
            Some("/dev/dri/renderD1/../x"),
        ] {
            assert!(
                check_device(&device_spec(DeviceKind::Vaapi, bad)).is_err(),
                "{bad:?}"
            );
        }
        assert!(valid_render_node("/dev/dri/renderD128"));
        assert!(!valid_render_node("/dev/dri/renderD12a"));
    }

    #[test]
    fn knows_the_catalog_images() {
        assert!(catalog_images().contains(&"cha/env-chrome:dev".to_string()));
    }

    #[test]
    fn gamepads_reach_the_app_read_only() {
        let rt = runtime();
        let s = rt.streamer_config(&spec(SecurityProfile::Standard), 7600);
        assert_eq!(
            s["HostConfig"]["Devices"][0]["PathInContainer"],
            "/dev/uinput"
        );
        assert!(s["Cmd"].as_array().unwrap().contains(&json!("--input-dir")));
        let app = rt.app_config(&spec(SecurityProfile::Standard), 7600, &[]);
        let mounts = app["HostConfig"]["Mounts"].as_array().unwrap();
        let input = mounts.iter().find(|m| m["Target"] == "/dev/input").unwrap();
        assert_eq!(input["Source"], "cha-env-e1-input");
        assert_eq!(input["ReadOnly"], true);
        assert!(
            mounts
                .iter()
                .any(|m| m["Target"] == "/run/udev" && m["ReadOnly"] == true)
        );
        assert_eq!(app["HostConfig"]["DeviceCgroupRules"], json!(["c 13:* rw"]));
        assert!(app["HostConfig"].get("Devices").is_none());

        let mut rt = runtime();
        rt.config.uinput = None;
        rt.config.uhid = None;
        let s = rt.streamer_config(&spec(SecurityProfile::Standard), 7600);
        assert_eq!(s["HostConfig"]["Devices"], json!([]));
        assert!(!s["Cmd"].as_array().unwrap().contains(&json!("--input-dir")));
        let app = rt.app_config(&spec(SecurityProfile::Standard), 7600, &[]);
        assert_eq!(app["HostConfig"]["Mounts"].as_array().unwrap().len(), 1);
        assert_eq!(app["HostConfig"]["DeviceCgroupRules"], json!([]));
    }

    fn pad_spec(kind: Option<GamepadKind>) -> EnvironmentSpec {
        EnvironmentSpec {
            gamepad: kind,
            ..spec(SecurityProfile::Standard)
        }
    }

    #[test]
    fn the_streamer_gets_uhid_and_the_pad_kind() {
        let rt = runtime();
        let cmd = |spec: &EnvironmentSpec| -> Vec<String> {
            rt.streamer_config(spec, 7600)["Cmd"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        let value_of = |cmd: &[String], flag: &str| {
            cmd.iter()
                .position(|a| a == flag)
                .map(|i| cmd[i + 1].clone())
        };
        let ds = cmd(&pad_spec(Some(GamepadKind::Dualsense)));
        assert_eq!(value_of(&ds, "--pad-kind").as_deref(), Some("dualsense"));
        assert_eq!(value_of(&ds, "--uhid").as_deref(), Some("/dev/uhid"));
        assert_eq!(value_of(&ds, "--uinput").as_deref(), Some("/dev/uinput"));
        assert_eq!(value_of(&ds, "--input-dir").as_deref(), Some(INPUT_DIR));
        let s = rt.streamer_config(&pad_spec(Some(GamepadKind::Steam)), 7600);
        let devices = s["HostConfig"]["Devices"].as_array().unwrap();
        assert!(devices.iter().any(|d| d["PathOnHost"] == "/dev/uhid"
            && d["PathInContainer"] == "/dev/uhid"
            && d["CgroupPermissions"] == "rw"));
        assert!(
            devices
                .iter()
                .any(|d| d["PathInContainer"] == "/dev/uinput")
        );

        // The default is left out, as it is on the wire.
        assert_eq!(value_of(&cmd(&pad_spec(None)), "--pad-kind"), None);

        // A node with only uhid still has pads, and its own host path.
        let mut only = runtime();
        only.config.uinput = None;
        only.config.uhid = Some("/dev/misc/uhid".into());
        let s = only.streamer_config(&pad_spec(Some(GamepadKind::Dualsense)), 7600);
        let cmd = s["Cmd"].as_array().unwrap();
        assert!(cmd.contains(&json!("--input-dir")) && !cmd.contains(&json!("--uinput")));
        assert_eq!(
            s["HostConfig"]["Devices"][0]["PathOnHost"],
            "/dev/misc/uhid"
        );

        // Without uhid the streamer is told the kind and falls back itself.
        let mut none = runtime();
        none.config.uhid = None;
        let s = none.streamer_config(&pad_spec(Some(GamepadKind::Dualsense)), 7600);
        assert!(!s["Cmd"].as_array().unwrap().contains(&json!("--uhid")));
    }

    #[test]
    fn hidraw_nodes_are_mounted_from_the_input_volume_and_allowed() {
        let rt = runtime();
        let nodes = [
            Hidraw {
                name: "hidraw3".into(),
                major: 240,
                minor: 3,
            },
            Hidraw {
                name: "hidraw4".into(),
                major: 240,
                minor: 4,
            },
        ];
        let app = rt.app_config(&pad_spec(Some(GamepadKind::Dualsense)), 7600, &nodes);
        let mounts = app["HostConfig"]["Mounts"].as_array().unwrap();
        assert_eq!(
            mounts.iter().find(|m| m["Target"] == "/dev/hidraw3"),
            Some(&json!({
                "Type": "volume",
                "Source": "cha-env-e1-input",
                "Target": "/dev/hidraw3",
                "ReadOnly": true,
                "VolumeOptions": { "Subpath": "hidraw/hidraw3" },
            }))
        );
        assert!(
            mounts.iter().any(|m| m["Target"] == "/dev/hidraw4"
                && m["VolumeOptions"]["Subpath"] == "hidraw/hidraw4")
        );
        // The input volume itself is still all of /dev/input, and nothing else.
        assert!(mounts.iter().any(|m| m["Target"] == "/dev/input"
            && m["Source"] == "cha-env-e1-input"
            && m.get("VolumeOptions").is_none()));
        assert_eq!(
            app["HostConfig"]["DeviceCgroupRules"],
            json!(["c 13:* rw", "c 240:3 rwm", "c 240:4 rwm"])
        );
        // No nodes: what an Xbox pad gets.
        let plain = rt.app_config(&pad_spec(None), 7600, &[]);
        assert_eq!(
            plain["HostConfig"]["DeviceCgroupRules"],
            json!(["c 13:* rw"])
        );
        assert!(
            !plain["HostConfig"]["Mounts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["Target"].as_str().unwrap().starts_with("/dev/hidraw"))
        );
    }

    #[tokio::test]
    async fn only_the_uhid_kinds_wait_for_the_streamer() {
        // Nothing listens on the port: asking would fail, so these don't ask.
        let rt = runtime();
        for kind in [None, Some(GamepadKind::Xbox360)] {
            assert_eq!(
                rt.streamer_hidraw(&pad_spec(kind), 1).await.unwrap(),
                vec![]
            );
        }
        let mut none = runtime();
        none.config.uhid = None;
        let spec = pad_spec(Some(GamepadKind::Steam));
        assert_eq!(none.streamer_hidraw(&spec, 1).await.unwrap(), vec![]);
    }

    #[test]
    fn the_streamers_hidraw_list_is_read_strictly() {
        assert_eq!(parse_hidraw(&json!({ "port": 1 })).unwrap(), vec![]);
        assert_eq!(parse_hidraw(&json!({ "hidraw": [] })).unwrap(), vec![]);
        assert_eq!(
            parse_hidraw(&json!({ "hidraw": [
                { "name": "hidraw3", "major": 240, "minor": 3 },
                { "name": "hidraw12", "major": 240, "minor": 12 },
            ] }))
            .unwrap(),
            vec![
                Hidraw {
                    name: "hidraw3".into(),
                    major: 240,
                    minor: 3
                },
                Hidraw {
                    name: "hidraw12".into(),
                    major: 240,
                    minor: 12
                },
            ]
        );
        for bad in [
            json!({ "hidraw": "hidraw3" }),
            json!({ "hidraw": [{ "name": "hidraw", "major": 1, "minor": 1 }] }),
            json!({ "hidraw": [{ "name": "hidraw3x", "major": 1, "minor": 1 }] }),
            json!({ "hidraw": [{ "name": "../hidraw3", "major": 1, "minor": 1 }] }),
            json!({ "hidraw": [{ "name": "hidraw3/../x", "major": 1, "minor": 1 }] }),
            json!({ "hidraw": [{ "name": "input0", "major": 1, "minor": 1 }] }),
            json!({ "hidraw": [{ "name": "hidraw3", "minor": 1 }] }),
            json!({ "hidraw": [{ "name": "hidraw3", "major": -1, "minor": 1 }] }),
            json!({ "hidraw": [{ "major": 1, "minor": 1 }] }),
            // One bad entry refuses the lot.
            json!({ "hidraw": [
                { "name": "hidraw3", "major": 240, "minor": 3 },
                { "name": "x", "major": 240, "minor": 4 },
            ] }),
        ] {
            assert!(parse_hidraw(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_home_volume_is_labelled_for_tooling() {
        let home = home_mount(&steam_spec()).unwrap();
        assert_eq!(
            home,
            json!({
                "Type": "volume",
                "Source": "cha-home-u1-steam",
                "Target": "/home/cha",
                "VolumeOptions": { "Labels": {
                    "sh.cha.home": "1",
                    "sh.cha.owner": "u1",
                    "sh.cha.template": "steam",
                } },
            })
        );
        // From a portal that doesn't say whose it is: still marked as a home.
        let anonymous = EnvironmentSpec {
            owner: String::new(),
            template: String::new(),
            ..steam_spec()
        };
        assert_eq!(
            home_mount(&anonymous).unwrap()["VolumeOptions"]["Labels"],
            json!({ "sh.cha.home": "1" })
        );
        // No home, no mount.
        assert!(home_mount(&spec(SecurityProfile::Browser)).is_none());
    }

    #[test]
    fn the_streamer_never_sees_the_home() {
        let rt = runtime();
        let streamer = rt.streamer_config(&steam_spec(), 7600);
        let mounts = streamer["HostConfig"]["Mounts"].as_array().unwrap();
        assert!(mounts.iter().all(|m| m["Target"] != "/home/cha"));
        assert!(mounts.iter().all(|m| m.get("VolumeOptions").is_none()));
    }

    fn candidate_spec(candidates: &[&str]) -> EnvironmentSpec {
        EnvironmentSpec {
            image: candidates
                .first()
                .unwrap_or(&"cha/env-chrome:dev")
                .to_string(),
            image_candidates: candidates.iter().map(|c| c.to_string()).collect(),
            ..spec(SecurityProfile::Browser)
        }
    }

    const DEV: &str = "cha/env-chrome:dev";
    const PUBLISHED: &str = "ghcr.io/ban-red/cha-env-chrome:{version}";

    #[test]
    fn candidates_get_the_agents_version() {
        let config = runtime().config;
        let spec = candidate_spec(&[DEV, PUBLISHED]);
        assert_eq!(
            config.app_candidates(&spec, "0.1.0"),
            [DEV, "ghcr.io/ban-red/cha-env-chrome:0.1.0"]
        );
        // A portal that predates candidates sends `image` alone.
        let old = EnvironmentSpec {
            image_candidates: Vec::new(),
            ..candidate_spec(&[])
        };
        assert_eq!(config.app_candidates(&old, "0.1.0"), [DEV]);
        // The same image twice is tried once.
        let twice = candidate_spec(&[DEV, DEV]);
        assert_eq!(config.app_candidates(&twice, "0.1.0"), [DEV]);
    }

    #[test]
    fn candidates_go_through_the_published_mapping() {
        let mut config = runtime().config;
        config.app_images =
            PublishedImages::from_settings(Some("ghcr.io/ban-red"), Some("0.2.0")).unwrap();
        let spec = candidate_spec(&[DEV, PUBLISHED]);
        // A bare catalog name runs as its published copy; a full reference stays.
        assert_eq!(
            config.app_candidates(&spec, "0.1.0"),
            [
                "ghcr.io/ban-red/cha-env-chrome:0.2.0",
                "ghcr.io/ban-red/cha-env-chrome:0.1.0"
            ]
        );
    }

    #[test]
    fn the_streamer_has_a_published_fallback() {
        assert_eq!(
            streamer_candidates("cha/streamer:dev", "0.1.0"),
            ["cha/streamer:dev", "ghcr.io/ban-red/cha-streamer:0.1.0"]
        );
        // One that names its registry is the only one.
        assert_eq!(
            streamer_candidates("ghcr.io/ban-red/cha-streamer:0.0.9", "0.1.0"),
            ["ghcr.io/ban-red/cha-streamer:0.0.9"]
        );
        assert_eq!(
            streamer_candidates("localhost:5000/streamer", "0.1.0"),
            ["localhost:5000/streamer"]
        );
    }

    #[test]
    fn images_are_named_for_people() {
        assert_eq!(
            display_name("ghcr.io/ban-red/cha-env-chrome:0.1.0"),
            "Chrome"
        );
        assert_eq!(display_name("cha/env-chrome:dev"), "Chrome");
        assert_eq!(display_name("cha/env-test-pattern:dev"), "Test pattern");
        assert_eq!(
            display_name("ghcr.io/ban-red/cha-streamer:0.1.0"),
            "Streamer"
        );
        assert_eq!(display_name("ubuntu"), "Ubuntu");
        let p = pull_progress(
            "e1",
            "Chrome",
            PullProgress {
                done: 412_300_000,
                total: 890_900_000,
            },
        );
        assert_eq!(p.detail, "Downloading Chrome (412 of 890 MB)");
        assert_eq!(
            (p.done, p.total, p.unit),
            (Some(412_300_000), Some(890_900_000), Some("bytes"))
        );
        let p = pull_progress(
            "e1",
            "Steam",
            PullProgress {
                done: 1_500_000_000,
                total: 4_200_000_000,
            },
        );
        assert_eq!(p.detail, "Downloading Steam (1.5 of 4.2 GB)");
        // Nothing is known of the size yet.
        let p = pull_progress("e1", "Chrome", PullProgress::default());
        assert_eq!(p.detail, "Downloading Chrome");
        assert_eq!((p.done, p.total, p.unit), (None, None, None));
    }

    #[tokio::test]
    async fn an_image_the_engine_has_is_not_pulled() {
        let n = node(&[]);
        let have = [
            DEV.to_string(),
            "ghcr.io/ban-red/cha-env-chrome:0.1.0".to_string(),
        ];
        let image = n.rt.ensure_image("e1", "app", &have).await.unwrap();
        assert_eq!(image, DEV, "the first the engine has");
        // The dev build is missing: the published copy that is here runs.
        n.engine.absent_images.lock().unwrap().insert(DEV.into());
        let image = n.rt.ensure_image("e1", "app", &have).await.unwrap();
        assert_eq!(image, "ghcr.io/ban-red/cha-env-chrome:0.1.0");
        assert!(n.engine.pulls().is_empty());
    }

    #[tokio::test]
    async fn a_missing_published_image_is_pulled_with_progress() {
        let n = node(&[]);
        let published = "ghcr.io/ban-red/cha-env-chrome:0.1.0".to_string();
        n.engine
            .absent_images
            .lock()
            .unwrap()
            .extend([DEV.to_string(), published.clone()]);
        let mut said = n.rt.progress.subscribe();
        let mut pulled = n.rt.pulled();
        let image =
            n.rt.ensure_image("e1", "app", &[DEV.to_string(), published.clone()])
                .await
                .unwrap();
        assert_eq!(image, published);
        // Only the one that names a registry was asked for.
        assert_eq!(
            n.engine.pulls(),
            ["/images/create?fromImage=ghcr.io%2Fban-red%2Fcha-env-chrome&tag=0.1.0"]
        );
        // A first note with nothing counted, then the final count.
        let first = said.try_recv().unwrap();
        assert_eq!(first.id, "e1");
        assert_eq!(first.detail, "Downloading Chrome");
        assert_eq!(first.total, None);
        let mut last = first;
        while let Ok(note) = said.try_recv() {
            last = note;
        }
        assert_eq!(last.detail, "Downloading Chrome (4 of 4 MB)");
        assert_eq!(
            (last.done, last.total, last.unit),
            (Some(4_000_000), Some(4_000_000), Some("bytes"))
        );
        pulled.try_recv().expect("the inventory is told");
        // Now it is here.
        assert!(n.rt.docker.image_exists(&published).await.unwrap());
    }

    #[tokio::test]
    async fn a_bare_image_is_never_pulled() {
        let n = node(&[]);
        n.engine.absent_images.lock().unwrap().insert(DEV.into());
        let err =
            n.rt.ensure_image("e1", "app", &[DEV.to_string()])
                .await
                .unwrap_err()
                .to_string();
        assert!(err.contains(DEV) && err.contains("build it"), "{err}");
        assert!(n.engine.pulls().is_empty());
    }

    #[tokio::test]
    async fn a_failed_pull_names_every_candidate() {
        let n = node(&[]);
        let published = "ghcr.io/ban-red/cha-env-chrome:0.1.0".to_string();
        n.engine
            .absent_images
            .lock()
            .unwrap()
            .extend([DEV.to_string(), published.clone()]);
        *n.engine.pull_error.lock().unwrap() = Some("denied: access forbidden".into());
        let err = format!(
            "{:#}",
            n.rt.ensure_image("e1", "app", &[DEV.to_string(), published.clone()])
                .await
                .unwrap_err()
        );
        assert!(err.contains("denied: access forbidden"), "{err}");
        assert!(err.contains(DEV) && err.contains(&published), "{err}");
    }

    #[tokio::test]
    async fn a_launch_runs_the_candidate_that_was_found() {
        let n = node(&[]);
        let published = "ghcr.io/ban-red/cha-env-chrome:{version}";
        let versioned = format!("ghcr.io/ban-red/cha-env-chrome:{AGENT_VERSION}");
        n.engine
            .absent_images
            .lock()
            .unwrap()
            .extend([DEV.to_string(), versioned.clone()]);
        n.rt.start_environment(candidate_spec(&[DEV, published]))
            .await
            .unwrap();
        assert_eq!(n.engine.pulls().len(), 1);
        assert_eq!(
            n.engine.created()["cha-env-e1-app"]["Image"],
            versioned.as_str()
        );
    }

    /// The engine's API on a Unix socket, enough for an environment's life:
    /// it records every request and keeps the containers it is asked to make.
    #[derive(Default)]
    struct Engine {
        /// Method, path with its query, and the JSON body (`null` if none).
        requests: Mutex<Vec<(String, String, Value)>>,
        containers: Mutex<Vec<Value>>,
        /// Volumes that exist (legacy homes).
        volumes: Mutex<HashSet<String>>,
        /// A copy container exits with this code; 0 is success, and a copy
        /// that succeeds writes `copied` into the directory it was given.
        copy_exit: Mutex<i64>,
        /// What a command run in a container prints, and its exit code.
        exec_output: Mutex<String>,
        exec_exit: Mutex<i64>,
        /// Host paths that don't exist: creating a container that binds one fails.
        missing_paths: Mutex<HashSet<String>>,
        /// What a container's log reads as (the engine's framed stream).
        logs: Mutex<Vec<u8>>,
        /// A streamer that starts and straight away exits (no GPU memory).
        streamer_exits: Mutex<bool>,
        /// Images the engine doesn't have until they are pulled.
        absent_images: Mutex<HashSet<String>>,
        /// What pulling says instead of succeeding.
        pull_error: Mutex<Option<String>>,
    }

    impl Engine {
        /// The bodies of the container creations, by container name.
        fn created(&self) -> BTreeMap<String, Value> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, uri, _)| {
                    method == "POST" && uri.starts_with("/containers/create")
                })
                .map(|(_, uri, body)| {
                    let name = uri.split_once("name=").unwrap().1.to_string();
                    (name, body.clone())
                })
                .collect()
        }

        /// The configs of the copy containers it ran, in order.
        fn copies(&self) -> Vec<Value> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, uri, body)| {
                    method == "POST"
                        && uri.starts_with("/containers/create")
                        && body["Entrypoint"] == json!(["cp"])
                })
                .map(|(.., body)| body.clone())
                .collect()
        }

        /// The pulls it was asked for (path and query).
        fn pulls(&self) -> Vec<String> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, uri, _)| method == "POST" && uri.starts_with("/images/create"))
                .map(|(_, uri, _)| uri.clone())
                .collect()
        }

        /// The names of the volumes it was asked to delete.
        fn deleted_volumes(&self) -> Vec<String> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, ..)| method == "DELETE")
                .filter_map(|(_, uri, _)| uri.strip_prefix("/volumes/"))
                .map(|rest| rest.split('?').next().unwrap().to_string())
                .collect()
        }
    }

    async fn engine_call(State(engine): State<Arc<Engine>>, request: Request) -> Response {
        let (parts, body) = request.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let method = parts.method.to_string();
        let uri = parts.uri.to_string();
        engine
            .requests
            .lock()
            .unwrap()
            .push((method.clone(), uri.clone(), body.clone()));
        let path = parts.uri.path();
        let container = |p: &str| {
            p.trim_start_matches("/containers/")
                .split('/')
                .next()
                .unwrap()
                .to_string()
        };
        let mut containers = engine.containers.lock().unwrap();
        match (method.as_str(), path) {
            ("POST", "/images/create") => {
                if let Some(err) = engine.pull_error.lock().unwrap().clone() {
                    return format!(
                        "{}\n{}\n",
                        json!({"status": "Pulling fs layer", "id": "a1"}),
                        json!({"error": err})
                    )
                    .into_response();
                }
                let mut absent = engine.absent_images.lock().unwrap();
                absent.retain(|name| {
                    let (repo, tag) = name.rsplit_once(':').unwrap();
                    !uri.ends_with(&format!("fromImage={}&tag={}", encode(repo), encode(tag)))
                });
                // Two layers: one 3 MB, one 1 MB, and one the node had.
                [
                    json!({"status": "Already exists", "progressDetail": {}, "id": "l0"}),
                    json!({"status": "Downloading", "progressDetail": {"current": 1000000, "total": 3000000}, "id": "l1"}),
                    json!({"status": "Downloading", "progressDetail": {"current": 1000000, "total": 1000000}, "id": "l2"}),
                    json!({"status": "Download complete", "progressDetail": {}, "id": "l2"}),
                    json!({"status": "Download complete", "progressDetail": {}, "id": "l1"}),
                    json!({"status": "Pull complete", "progressDetail": {}, "id": "l1"}),
                ]
                .iter()
                .map(|l| format!("{l}\n"))
                .collect::<String>()
                .into_response()
            }
            ("GET", p) if p.starts_with("/images/") => {
                let absent = engine.absent_images.lock().unwrap();
                if absent
                    .iter()
                    .any(|name| p == format!("/images/{}/json", encode(name)))
                {
                    (
                        StatusCode::NOT_FOUND,
                        Json(json!({ "message": "No such image" })),
                    )
                        .into_response()
                } else {
                    Json(json!({})).into_response()
                }
            }
            ("GET", "/volumes") => {
                let volumes: Vec<Value> = engine
                    .volumes
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|name| json!({ "Name": name }))
                    .collect();
                Json(json!({ "Volumes": volumes, "Warnings": null })).into_response()
            }
            ("GET", p) if p.starts_with("/volumes/") => {
                let name = p.trim_start_matches("/volumes/");
                if engine.volumes.lock().unwrap().contains(name) {
                    Json(json!({ "Name": name })).into_response()
                } else {
                    (
                        StatusCode::NOT_FOUND,
                        Json(json!({ "message": "no such volume" })),
                    )
                        .into_response()
                }
            }
            ("POST", p) if p.ends_with("/wait") => {
                let id = container(p);
                let code = *engine.copy_exit.lock().unwrap();
                let dest = containers.iter().find(|c| c["Id"] == id).and_then(|c| {
                    c["Config"]["HostConfig"]["Mounts"]
                        .as_array()?
                        .iter()
                        .find(|m| m["Target"] == "/to")
                        .and_then(|m| m["Source"].as_str().map(str::to_string))
                });
                if let (0, Some(dest)) = (code, dest) {
                    // What `cp -a` would have done: put the files there.
                    std::fs::write(std::path::Path::new(&dest).join("copied"), id).unwrap();
                }
                Json(json!({ "StatusCode": code })).into_response()
            }
            ("GET", p) if p.ends_with("/logs") => {
                engine.logs.lock().unwrap().clone().into_response()
            }
            ("POST", p) if p.ends_with("/exec") => {
                (StatusCode::CREATED, Json(json!({ "Id": "x1" }))).into_response()
            }
            ("POST", "/exec/x1/start") => {
                engine.exec_output.lock().unwrap().clone().into_response()
            }
            ("GET", "/exec/x1/json") => {
                Json(json!({ "ExitCode": *engine.exec_exit.lock().unwrap() })).into_response()
            }
            ("GET", "/containers/json") => {
                // As the engine does for `label=key=value` (a bare `key` matches any).
                let filter = encode(&format!("{LABEL_ENV}="));
                let wanted = uri.split_once(&filter).map(|(_, rest)| {
                    rest.split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                        .next()
                        .unwrap_or_default()
                });
                let listed: Vec<&Value> = containers
                    .iter()
                    .filter(|c| wanted.is_none_or(|id| c["Labels"][LABEL_ENV] == id))
                    .collect();
                Json(json!(listed)).into_response()
            }
            ("POST", "/containers/create") => {
                let name = uri.split_once("name=").unwrap().1.to_string();
                let missing = engine.missing_paths.lock().unwrap();
                let absent = body["HostConfig"]["Mounts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|m| m["Type"] == "bind")
                    .filter_map(|m| m["Source"].as_str())
                    .find(|source| missing.contains(*source));
                if let Some(source) = absent {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "message": format!("invalid mount config for type \"bind\": bind source path does not exist: {source}") })),
                    )
                        .into_response();
                }
                // As the engine lists them: where each mount comes from and goes.
                let mounts: Vec<Value> = body["HostConfig"]["Mounts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|m| json!({ "Source": m["Source"], "Destination": m["Target"] }))
                    .collect();
                // Volumes the engine makes for the container, and the ports it
                // publishes (an IPv4 and an IPv6 entry each; 40000 and up
                // where the request left the host port to the engine).
                for m in body["HostConfig"]["Mounts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    if m["Type"] == "volume"
                        && let Some(name) = m["Source"].as_str()
                        && name.starts_with(HOST_VOLUME_PREFIX)
                    {
                        engine.volumes.lock().unwrap().insert(name.to_string());
                    }
                }
                let mut ports = Vec::new();
                for (key, bound) in body["HostConfig"]["PortBindings"]
                    .as_object()
                    .into_iter()
                    .flatten()
                {
                    let (private, kind) = key.split_once('/').unwrap();
                    let public: u16 = bound[0]["HostPort"]
                        .as_str()
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(40000 + ports.len() as u16);
                    for ip in ["0.0.0.0", "::"] {
                        ports.push(json!({
                            "IP": ip,
                            "PrivatePort": private.parse::<u16>().unwrap(),
                            "PublicPort": public,
                            "Type": kind,
                        }));
                    }
                }
                containers.push(json!({
                    "Id": name, "State": "created", "Labels": body["Labels"], "Config": body,
                    "Mounts": mounts, "Ports": ports,
                }));
                (StatusCode::CREATED, Json(json!({ "Id": name }))).into_response()
            }
            ("POST", p) if p.ends_with("/start") => {
                let id = container(p);
                let dies = id.ends_with("streamer") && *engine.streamer_exits.lock().unwrap();
                for c in containers.iter_mut().filter(|c| c["Id"] == id) {
                    c["State"] = json!(if dies { "exited" } else { "running" });
                }
                StatusCode::NO_CONTENT.into_response()
            }
            ("POST", p) if p.ends_with("/stop") => StatusCode::NO_CONTENT.into_response(),
            ("DELETE", p) if p.starts_with("/containers/") => {
                let id = container(p);
                containers.retain(|c| c["Id"] != id);
                StatusCode::NO_CONTENT.into_response()
            }
            ("DELETE", p) if p.starts_with("/volumes/") => {
                let name = p.trim_start_matches("/volumes/");
                if name.starts_with(HOST_VOLUME_PREFIX) {
                    engine.volumes.lock().unwrap().remove(name);
                }
                StatusCode::NO_CONTENT.into_response()
            }
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }

    /// A fake engine, and a client on its socket.
    fn fake_engine() -> (Arc<Engine>, tempfile::TempDir, Docker) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("docker.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let engine = Arc::new(Engine::default());
        let app = Router::new()
            .fallback(engine_call)
            .with_state(Arc::clone(&engine));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (engine, dir, Docker::new(socket))
    }

    const WINE_DIR: &str = "/usr/lib/x86_64-linux-gnu/nvidia/wine";

    #[test]
    fn the_host_nvidia_wine_dir_reaches_apps_read_only() {
        let mut rt = runtime();
        let target = |config: &Value| {
            config["HostConfig"]["Mounts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["Target"] == WINE_DIR)
                .cloned()
        };
        assert!(target(&rt.app_config(&steam_spec(), 7600, &[])).is_none());

        rt.config.nvidia_wine_dir = Some(WINE_DIR.into());
        let mount = target(&rt.app_config(&steam_spec(), 7600, &[])).expect("mounted");
        assert_eq!(mount["Type"], "bind");
        assert_eq!(mount["Source"], WINE_DIR);
        assert_eq!(mount["ReadOnly"], true);
        assert_eq!(mount["BindOptions"]["CreateMountpoint"], false);
        // Streamers don't run Wine.
        assert!(target(&rt.streamer_config(&steam_spec(), 7600)).is_none());
    }

    #[tokio::test]
    async fn probes_whether_the_host_has_a_path_without_leaving_anything() {
        let (engine, _dir, docker) = fake_engine();
        let path = std::path::Path::new(WINE_DIR);
        assert!(
            docker
                .host_path_exists("cha/streamer:dev", path)
                .await
                .unwrap()
        );
        engine.missing_paths.lock().unwrap().insert(WINE_DIR.into());
        assert!(
            !docker
                .host_path_exists("cha/streamer:dev", path)
                .await
                .unwrap()
        );
        assert!(engine.containers.lock().unwrap().is_empty());
        // The probe is only ever created: never started.
        let requests = engine.requests.lock().unwrap();
        assert!(requests.iter().all(|(_, uri, _)| !uri.ends_with("/start")));
        let probe = requests
            .iter()
            .find(|(_, uri, _)| uri.starts_with("/containers/create"))
            .unwrap();
        let bind = &probe.2["HostConfig"]["Mounts"][0];
        assert_eq!(bind["Source"], WINE_DIR);
        assert_eq!(bind["BindOptions"]["CreateMountpoint"], false);
    }

    #[tokio::test]
    async fn an_engine_that_fails_otherwise_leaves_the_answer_unknown() {
        // Nothing listens: not "the path is missing".
        let docker = Docker::new("/nonexistent");
        assert!(
            docker
                .host_path_exists("cha/streamer:dev", std::path::Path::new(WINE_DIR))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn apps_get_the_nvidia_wine_dir_only_when_the_host_has_it() {
        let (engine, _dir, docker) = fake_engine();
        let mut config = runtime().config.clone();
        assert_eq!(
            DockerRuntime::settle_nvidia_wine_dir(&docker, &config).await,
            None,
            "not configured"
        );
        config.nvidia_wine_dir = Some(WINE_DIR.into());
        assert_eq!(
            DockerRuntime::settle_nvidia_wine_dir(&docker, &config).await,
            Some(WINE_DIR.into())
        );
        engine.missing_paths.lock().unwrap().insert(WINE_DIR.into());
        assert_eq!(
            DockerRuntime::settle_nvidia_wine_dir(&docker, &config).await,
            None
        );
    }

    #[tokio::test]
    async fn a_persistent_launch_mounts_its_home_on_the_app_only() {
        let (engine, _dir, docker) = fake_engine();
        let rt = runtime_on(docker);
        rt.start_environment(steam_spec()).await.unwrap();

        let created = engine.created();
        let mounts_of = |name: &str| {
            created[name]["HostConfig"]["Mounts"]
                .as_array()
                .unwrap()
                .clone()
        };
        let app = mounts_of("cha-env-e1-app");
        let home: Vec<_> = app.iter().filter(|m| m["Target"] == "/home/cha").collect();
        assert_eq!(home.len(), 1);
        assert_eq!(home[0]["Source"], "cha-home-u1-steam");
        assert_eq!(home[0]["VolumeOptions"]["Labels"]["sh.cha.owner"], "u1");
        assert!(
            mounts_of("cha-env-e1-streamer")
                .iter()
                .all(|m| m["Target"] != "/home/cha")
        );
    }

    #[tokio::test]
    async fn stopping_removes_the_scratch_volumes_and_keeps_the_home() {
        let (engine, _dir, docker) = fake_engine();
        let rt = runtime_on(docker);
        rt.start_environment(steam_spec()).await.unwrap();
        engine.requests.lock().unwrap().clear();

        rt.stop_environment("e1").await.unwrap();

        assert_eq!(
            engine.deleted_volumes(),
            ["cha-env-e1-input", "cha-env-e1-udev", "cha-env-e1"]
        );
        let requests = engine.requests.lock().unwrap();
        assert!(
            requests.iter().all(|(_, uri, _)| !uri.contains("cha-home")),
            "nothing touches the home: {requests:?}"
        );
        // The containers went, with only their anonymous volumes (`v=true`).
        assert!(engine.containers.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_streamer_that_exits_at_startup_leaves_nothing_behind() {
        let (engine, _dir, docker) = fake_engine();
        let rt = runtime_on(docker);
        *engine.streamer_exits.lock().unwrap() = true;

        let err = rt.start_environment(steam_spec()).await.unwrap_err();

        assert!(format!("{err:#}").contains("streamer exited"), "{err:#}");
        // No app was made for it, and what was made is gone.
        assert!(engine.containers.lock().unwrap().is_empty());
        assert!(
            engine.created().keys().all(|name| !name.ends_with("-app")),
            "{:?}",
            engine.created().keys()
        );
        assert_eq!(
            engine.deleted_volumes(),
            ["cha-env-e1-input", "cha-env-e1-udev", "cha-env-e1"]
        );
        let state = rt.state.lock().unwrap();
        assert!(state.ports.is_empty() && state.homes.is_empty() && state.starting.is_empty());
    }

    #[tokio::test]
    async fn the_app_that_fails_to_create_takes_the_streamer_with_it() {
        let (engine, _dir, docker) = fake_engine();
        let rt = runtime_on(docker);
        // A bind source the app needs is missing, so its creation is refused.
        let mut rt = rt;
        rt.config.nvidia_wine_dir = Some(WINE_DIR.into());
        engine
            .missing_paths
            .lock()
            .unwrap()
            .insert(WINE_DIR.to_string());
        let spec = steam_spec();
        let _ = rt.start_environment(spec).await.unwrap_err();
        assert!(engine.containers.lock().unwrap().is_empty());
        assert!(rt.state.lock().unwrap().starting.is_empty());
    }

    #[tokio::test]
    async fn a_death_while_starting_is_the_launchs_to_clean_up() {
        let (engine, _dir, docker) = fake_engine();
        let rt = runtime_on(docker);
        {
            let mut state = rt.state.lock().unwrap();
            state.ports.insert("e1".into(), 7600);
            state.starting.insert("e1".into(), None);
        }
        engine.containers.lock().unwrap().push(json!({
            "Id": "streamer1", "State": "exited",
            "Labels": { LABEL_ENV: "e1", LABEL_ROLE: "streamer" },
        }));
        let mut exits = rt.exits.subscribe();

        rt.on_event(ContainerEvent {
            action: "die".into(),
            id: "streamer1".into(),
            labels: HashMap::from([
                (LABEL_ENV.to_string(), "e1".to_string()),
                (LABEL_ROLE.to_string(), "streamer".to_string()),
            ]),
            exit_code: Some(1),
        })
        .await;

        // Nothing was removed or announced; the launch will see it.
        assert_eq!(engine.containers.lock().unwrap().len(), 1);
        assert!(exits.try_recv().is_err());
        let state = rt.state.lock().unwrap();
        assert!(state.ports.contains_key("e1"));
        assert_eq!(
            state.starting["e1"].as_deref(),
            Some("the streamer exited with code 1 while starting")
        );
    }

    #[tokio::test]
    async fn an_app_left_without_a_streamer_is_cleared_when_the_agent_starts() {
        let (engine, _dir, docker) = fake_engine();
        let rt = runtime_on(docker);
        let labels = |env: &str, role: &str| json!({ LABEL_ENV: env, LABEL_ROLE: role, LABEL_HTTP_PORT: "7600" });
        engine.containers.lock().unwrap().extend([
            // e1 lost its streamer; e2 is whole.
            json!({ "Id": "a1", "State": "running", "Labels": labels("e1", "app") }),
            json!({ "Id": "s2", "State": "running", "Labels": labels("e2", "streamer") }),
            json!({ "Id": "a2", "State": "running", "Labels": labels("e2", "app") }),
        ]);

        rt.adopt().await.unwrap();

        let left: Vec<String> = engine
            .containers
            .lock()
            .unwrap()
            .iter()
            .map(|c| c["Id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(left, ["s2", "a2"]);
        assert_eq!(
            rt.state.lock().unwrap().ports.keys().collect::<Vec<_>>(),
            ["e2"]
        );
    }

    #[tokio::test]
    async fn the_app_gets_no_volume_that_isnt_a_home() {
        let (engine, _dir, docker) = fake_engine();
        let rt = runtime_on(docker);
        for bad in [
            "cha-node_state",
            "cha-env-e1",
            "cha-home-",
            "cha-home-x/../y",
            "",
        ] {
            let err = rt
                .start_environment(EnvironmentSpec {
                    home: Some(bad.into()),
                    ..steam_spec()
                })
                .await
                .unwrap_err();
            assert!(
                format!("{err:#}").contains("home volume"),
                "{bad:?}: {err:#}"
            );
        }
        assert!(
            engine.requests.lock().unwrap().is_empty(),
            "nothing was created or even looked up"
        );
        assert!(rt.state.lock().unwrap().ports.is_empty());
    }

    #[tokio::test]
    async fn other_templates_get_no_home_volume() {
        let (engine, _dir, docker) = fake_engine();
        let rt = runtime_on(docker);
        rt.start_environment(spec(SecurityProfile::Browser))
            .await
            .unwrap();
        let created = engine.created();
        let mounts = created["cha-env-e1-app"]["HostConfig"]["Mounts"]
            .as_array()
            .unwrap();
        assert!(mounts.iter().all(|m| m["Target"] != "/home/cha"));
        assert!(mounts.iter().all(|m| m.get("VolumeOptions").is_none()));
    }

    // ---- App data under the data root ----

    use cha_wire::{Shared, Storage};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    const USER: &str = "01a10527-f79f-761b-962f-4b26924a2e68";
    const OTHER: &str = "01a10834-b378-724f-8d25-a1d4034a79d5";
    const COMPAT: &str = "steamapps/compatdata";
    const SHADER: &str = "steamapps/shadercache";

    fn steam_storage(persistent: bool, shared: Option<bool>) -> Storage {
        steam_storage_for(USER, persistent, shared)
    }

    fn steam_storage_for(user: &str, persistent: bool, shared: Option<bool>) -> Storage {
        Storage {
            home: persistent.then(|| cha_wire::user_dir(user, "steam")),
            shared: shared.map(|writable| Shared {
                path: cha_wire::shared_dir("steam"),
                writable,
                per_user: if persistent {
                    vec![COMPAT.into(), SHADER.into()]
                } else {
                    vec![]
                },
            }),
            legacy_volume: persistent.then(|| home_volume_name(user, "steam")),
            data_template: None,
        }
    }

    fn app_data_spec(id: &str, template: &str, storage: Option<Storage>) -> EnvironmentSpec {
        EnvironmentSpec {
            id: id.into(),
            image: "cha/env-steam:dev".into(),
            owner: USER.into(),
            template: template.into(),
            storage: storage.map(Box::new),
            ..spec(SecurityProfile::Steam)
        }
    }

    fn target<'a>(mounts: &'a [Value], target: &str) -> Option<&'a Value> {
        mounts.iter().find(|m| m["Target"] == target)
    }

    /// A runtime whose data root is `/data/cha` on the host, to show the
    /// containers' paths don't follow it.
    fn mounting(shared_dirs: &[(&str, &str)]) -> DockerRuntime {
        runtime_with(
            Docker::new("/nonexistent"),
            "/data/cha".into(),
            (1000, 1000),
            shared_dirs
                .iter()
                .map(|(t, d)| (t.to_string(), PathBuf::from(d)))
                .collect(),
        )
    }

    #[test]
    fn mounts_for_every_combination_of_keeping_and_sharing() {
        let rt = mounting(&[]);
        for persistent in [false, true] {
            for shared in [None, Some(false), Some(true)] {
                let storage = Storage {
                    home: persistent.then(|| cha_wire::user_dir(USER, "chrome")),
                    shared: shared.map(|writable| Shared {
                        path: cha_wire::shared_dir("chrome"),
                        writable,
                        per_user: vec![],
                    }),
                    legacy_volume: None,
                    data_template: None,
                };
                let spec = app_data_spec("e1", "chrome", Some(storage));
                let mounts = rt.storage_mounts(&spec);
                let label = format!("kept {persistent}, shared {shared:?}");
                assert_eq!(
                    mounts.len(),
                    usize::from(persistent) + usize::from(shared.is_some()),
                    "{label}: {mounts:?}"
                );
                match target(&mounts, "/home/cha") {
                    Some(home) => {
                        assert!(persistent, "{label}");
                        assert_eq!(home["Type"], "bind");
                        assert_eq!(
                            home["Source"],
                            format!("/data/cha/users/{USER}/chrome"),
                            "{label}"
                        );
                        assert_eq!(home["ReadOnly"], false, "the app writes its home");
                        assert_eq!(home["BindOptions"]["CreateMountpoint"], false);
                    }
                    None => assert!(!persistent, "{label}"),
                }
                // The shared directory is at one path in every container,
                // wherever the node keeps its data.
                match target(&mounts, "/srv/cha-portal/shared/chrome") {
                    Some(dir) => {
                        assert_eq!(dir["Source"], "/data/cha/shared/chrome", "{label}");
                        // Read access mounts it read-only; write, read-write.
                        assert_eq!(dir["ReadOnly"], shared == Some(false), "{label}");
                    }
                    None => assert_eq!(shared, None, "{label}"),
                }
                let app = rt.app_config(&spec, 7600, &[]);
                let env: Vec<&str> = app["Env"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| e.as_str().unwrap())
                    .collect();
                assert_eq!(
                    env.contains(&"CHA_SHARED_DIR=/srv/cha-portal/shared/chrome"),
                    shared.is_some(),
                    "{label}"
                );
                assert_eq!(
                    env.iter()
                        .filter(|e| e.starts_with("CHA_SHARED_DIR="))
                        .count(),
                    usize::from(shared.is_some())
                );
            }
        }
        // No storage: nothing mounted for data.
        let spec = app_data_spec("e1", "chrome", None);
        assert!(rt.storage_mounts(&spec).is_empty());
    }

    #[test]
    fn steams_per_user_directories_go_over_the_shared_library() {
        let rt = mounting(&[]);
        for writable in [false, true] {
            let spec = app_data_spec("e1", "steam", Some(steam_storage(true, Some(writable))));
            let mounts = rt.storage_mounts(&spec);
            // Home, the shared directory, then what goes over it: the parent
            // mount comes before the ones inside it.
            let targets: Vec<&str> = mounts
                .iter()
                .map(|m| m["Target"].as_str().unwrap())
                .collect();
            assert_eq!(
                targets,
                [
                    "/home/cha",
                    "/srv/cha-portal/shared/steam",
                    "/srv/cha-portal/shared/steam/steamapps/compatdata",
                    "/srv/cha-portal/shared/steam/steamapps/shadercache",
                ]
            );
            let shared = &mounts[1];
            assert_eq!(shared["ReadOnly"], !writable);
            for (mount, place) in [(&mounts[2], COMPAT), (&mounts[3], SHADER)] {
                assert_eq!(
                    mount["Source"],
                    format!("/data/cha/users/{USER}/steam/.cha-shared/{place}")
                );
                assert_eq!(mount["Type"], "bind");
                // The user's own, writable even where the library is read-only.
                assert_eq!(mount["ReadOnly"], false);
            }
        }
    }

    #[test]
    fn a_shared_directory_kept_elsewhere_mounts_at_its_own_path() {
        let rt = mounting(&[("steam", "/mnt/games/steam")]);
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, Some(true))));
        let mounts = rt.storage_mounts(&spec);
        let targets: Vec<&str> = mounts
            .iter()
            .map(|m| m["Target"].as_str().unwrap())
            .collect();
        assert_eq!(
            targets,
            [
                "/home/cha",
                "/mnt/games/steam",
                "/mnt/games/steam/steamapps/compatdata",
                "/mnt/games/steam/steamapps/shadercache",
            ]
        );
        assert_eq!(mounts[1]["Source"], "/mnt/games/steam");
        // The overlays' sources are still the user's, under the data root.
        assert_eq!(
            mounts[2]["Source"],
            format!("/data/cha/users/{USER}/steam/.cha-shared/{COMPAT}")
        );
        let app = rt.app_config(&spec, 7600, &[]);
        assert!(
            app["Env"]
                .as_array()
                .unwrap()
                .contains(&json!("CHA_SHARED_DIR=/mnt/games/steam"))
        );
        // Other templates keep theirs under the data root.
        let chrome = app_data_spec(
            "e2",
            "chrome",
            Some(Storage {
                shared: Some(Shared {
                    path: "shared/chrome".into(),
                    writable: false,
                    per_user: vec![],
                }),
                ..Storage::default()
            }),
        );
        assert_eq!(
            rt.storage_mounts(&chrome)[0]["Target"],
            "/srv/cha-portal/shared/chrome"
        );
    }

    #[test]
    fn the_streamer_never_sees_app_data() {
        let rt = mounting(&[]);
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, Some(true))));
        let streamer = rt.streamer_config(&spec, 7600);
        let mounts = streamer["HostConfig"]["Mounts"].as_array().unwrap();
        assert!(mounts.iter().all(|m| m["Type"] == "volume"));
        assert!(
            mounts
                .iter()
                .all(|m| !m["Target"].as_str().unwrap().contains("steam"))
        );
        // The app is labelled with whose home it has.
        let app = rt.app_config(&spec, 7600, &[]);
        assert_eq!(app["Labels"]["sh.cha.owner"], USER);
        assert_eq!(app["Labels"]["sh.cha.template"], "steam");
        let plain = rt.app_config(&app_data_spec("e1", "steam", None), 7600, &[]);
        assert!(plain["Labels"].get("sh.cha.owner").is_none());
    }

    /// A runtime on a fake engine with its data root in a temp directory.
    struct Node {
        engine: Arc<Engine>,
        rt: DockerRuntime,
        root: PathBuf,
        _engine_dir: tempfile::TempDir,
        /// Holds the data root, which goes when this does.
        _dir: tempfile::TempDir,
    }

    fn node(shared_dirs: &[(&str, PathBuf)]) -> Node {
        let (engine, engine_dir, docker) = fake_engine();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cha-portal");
        std::fs::create_dir(&root).unwrap();
        let meta = std::fs::metadata(&root).unwrap();
        let owner = (meta.uid(), meta.gid());
        let shared = shared_dirs
            .iter()
            .map(|(t, d)| (t.to_string(), d.clone()))
            .collect();
        let rt = runtime_with(docker, root.clone(), owner, shared);
        Node {
            engine,
            rt,
            root,
            _engine_dir: engine_dir,
            _dir: dir,
        }
    }

    impl Node {
        fn home(&self, user: &str, template: &str) -> PathBuf {
            self.root.join("users").join(user).join(template)
        }

        fn app_mounts(&self, id: &str) -> Vec<Value> {
            self.engine.created()[&format!("cha-env-{id}-app")]["HostConfig"]["Mounts"]
                .as_array()
                .unwrap()
                .clone()
        }
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[tokio::test]
    async fn a_new_home_starts_from_the_images_files() {
        let n = node(&[]);
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, None)));
        n.rt.start_environment(spec).await.unwrap();

        let home = n.home(USER, "steam");
        assert_eq!(mode(&home), 0o700);
        // No volume to take: one copy, of the image's own home.
        let copies = n.engine.copies();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0]["Cmd"], json!(["-a", "/home/cha/.", "/to/"]));
        assert_eq!(copies[0]["Image"], "cha/env-steam:dev");
        assert_eq!(copies[0]["User"], "0:0");
        assert_eq!(copies[0]["HostConfig"]["NetworkMode"], "none");
        assert_eq!(copies[0]["HostConfig"]["CapDrop"], json!(["ALL"]));
        let to = target(copies[0]["HostConfig"]["Mounts"].as_array().unwrap(), "/to").unwrap();
        assert_eq!(to["Source"], home.to_string_lossy().as_ref());
        assert!(home.join("copied").is_file());
        // The app got the directory as its home; the copy container is gone.
        let mounts = n.app_mounts("e1");
        assert_eq!(
            target(&mounts, "/home/cha").unwrap()["Source"],
            home.to_string_lossy().as_ref()
        );
        assert!(
            n.engine
                .containers
                .lock()
                .unwrap()
                .iter()
                .all(|c| c["Id"] != "cha-copy-e1")
        );
        assert!(n.rt.state.lock().unwrap().homes.contains_key("e1"));
    }

    #[tokio::test]
    async fn a_home_with_things_in_it_is_left_alone() {
        let n = node(&[]);
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, None)));
        n.rt.start_environment(spec.clone()).await.unwrap();
        std::fs::write(n.home(USER, "steam").join("save"), "mine").unwrap();
        n.rt.stop_environment("e1").await.unwrap();
        n.engine.requests.lock().unwrap().clear();

        n.rt.start_environment(app_data_spec("e2", "steam", spec.storage.map(|s| *s)))
            .await
            .unwrap();
        assert!(n.engine.copies().is_empty(), "nothing copied over it");
        assert_eq!(
            std::fs::read_to_string(n.home(USER, "steam").join("save")).unwrap(),
            "mine"
        );
    }

    #[tokio::test]
    async fn an_old_home_volume_is_copied_in_once_and_left_in_place() {
        let n = node(&[]);
        let volume = home_volume_name(USER, "steam");
        n.engine.volumes.lock().unwrap().insert(volume.clone());
        let mut said = n.rt.progress.subscribe();
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, None)));
        n.rt.start_environment(spec.clone()).await.unwrap();

        // The volume went in read-only, to a directory beside the home.
        let copies = n.engine.copies();
        assert_eq!(copies.len(), 1, "the volume, not the image's files");
        assert_eq!(copies[0]["Cmd"], json!(["-a", "/from/.", "/to/"]));
        let mounts = copies[0]["HostConfig"]["Mounts"].as_array().unwrap();
        let from = target(mounts, "/from").unwrap();
        assert_eq!(from["Source"], volume.as_str());
        assert_eq!(from["ReadOnly"], true);
        assert_eq!(from["VolumeOptions"]["NoCopy"], true);
        let to = target(mounts, "/to").unwrap();
        assert_eq!(
            to["Source"],
            n.home(USER, "steam.migrating").to_string_lossy().as_ref()
        );
        // The user is told, and the copy landed whole as the home.
        let note = said.try_recv().expect("a progress note");
        assert_eq!(note.id, "e1");
        assert!(note.detail.contains("Moving your files"));
        let home = n.home(USER, "steam");
        assert!(home.join("copied").is_file());
        assert_eq!(mode(&home), 0o700);
        assert!(!n.home(USER, "steam.migrating").exists());
        assert!(n.home(USER, "steam.migrated").is_file());
        // Nothing touched the volume.
        assert!(
            n.engine
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(m, uri, _)| !(m == "DELETE" && uri.contains("cha-home")))
        );

        // Later launches don't copy it again.
        n.rt.stop_environment("e1").await.unwrap();
        n.engine.requests.lock().unwrap().clear();
        n.rt.start_environment(app_data_spec("e2", "steam", spec.storage.map(|s| *s)))
            .await
            .unwrap();
        assert!(n.engine.copies().is_empty());
    }

    /// The engine's framing of `lines` as stdout, each with a timestamp.
    fn framed(lines: &[&str]) -> Vec<u8> {
        let text: String = lines
            .iter()
            .map(|l| format!("2026-10-05T12:00:00.000000001Z {l}\n"))
            .collect();
        let mut raw = vec![1, 0, 0, 0];
        raw.extend((text.len() as u32).to_be_bytes());
        raw.extend(text.as_bytes());
        raw
    }

    fn died(id: &str, role: &str, code: i64) -> ContainerEvent {
        ContainerEvent {
            action: "die".into(),
            id: format!("cha-env-{id}-{role}"),
            labels: [(LABEL_ENV, id), (LABEL_ROLE, role)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            exit_code: Some(code),
        }
    }

    #[tokio::test]
    async fn a_container_that_dies_is_read_before_it_is_removed() {
        let mut n = node(&[]);
        let logs = tempfile::tempdir().unwrap();
        n.rt.config.log_dir = Some(logs.path().to_path_buf());
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, None)));
        n.rt.start_environment(spec).await.unwrap();
        *n.engine.logs.lock().unwrap() = framed(&[
            "starting",
            "Error: cuDevicePrimaryCtxRetain failed: out of memory",
        ]);
        let mut exits = n.rt.exits.subscribe();
        let before = n.engine.requests.lock().unwrap().len();
        n.rt.on_event(died("e1", "streamer", 1)).await;
        let exit = exits.try_recv().unwrap();
        assert!(exit.failed);
        assert!(
            exit.detail
                .starts_with("The node's GPU is out of memory (VRAM)"),
            "{}",
            exit.detail
        );
        assert!(exit.detail.ends_with("(the streamer exited with code 1)"));
        assert!(exit.detail.chars().count() <= 300);
        assert_eq!(exit.log[0], "--- streamer ---");
        assert_eq!(
            exit.log[2],
            "Error: cuDevicePrimaryCtxRetain failed: out of memory"
        );
        // The logs were read from the engine, then the containers went.
        let requests = n.engine.requests.lock().unwrap()[before..].to_vec();
        let read = requests
            .iter()
            .position(|(m, u, _)| {
                m == "GET" && u.contains("/logs?stdout=1&stderr=1&tail=200&timestamps=1")
            })
            .unwrap();
        let removed = requests
            .iter()
            .position(|(m, u, _)| m == "DELETE" && u.starts_with("/containers/"))
            .unwrap();
        assert!(read < removed);
        // And are kept on the node.
        let kept = std::fs::read_to_string(logs.path().join("e1-streamer.log")).unwrap();
        assert!(kept.contains("cuDevicePrimaryCtxRetain"));
        assert!(logs.path().join("e1-app.log").is_file());
    }

    #[tokio::test]
    async fn an_app_that_quits_cleanly_is_not_read() {
        let n = node(&[]);
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, None)));
        n.rt.start_environment(spec).await.unwrap();
        let mut exits = n.rt.exits.subscribe();
        let before = n.engine.requests.lock().unwrap().len();
        n.rt.on_event(died("e1", "app", 0)).await;
        let exit = exits.try_recv().unwrap();
        assert!(!exit.failed && exit.log.is_empty());
        assert_eq!(exit.detail, "the app exited");
        assert!(
            !n.engine.requests.lock().unwrap()[before..]
                .iter()
                .any(|(_, u, _)| u.contains("/logs"))
        );
    }

    #[tokio::test]
    async fn a_start_that_fails_before_any_container_says_nothing_more() {
        let n = node(&[]);
        let mut exits = n.rt.exits.subscribe();
        *n.engine.copy_exit.lock().unwrap() = 1;
        n.engine
            .volumes
            .lock()
            .unwrap()
            .insert(home_volume_name(USER, "steam"));
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, None)));
        assert!(n.rt.start_environment(spec).await.is_err());
        assert!(exits.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_failed_copy_leaves_no_home_and_the_volume_untouched() {
        let n = node(&[]);
        n.engine
            .volumes
            .lock()
            .unwrap()
            .insert(home_volume_name(USER, "steam"));
        *n.engine.copy_exit.lock().unwrap() = 1;
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, None)));
        let err = n.rt.start_environment(spec.clone()).await.unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("still in the old volume"), "{message}");
        // Neither a home that looks real, nor a half copy, nor a marker that
        // would stop the next try.
        assert!(!n.home(USER, "steam").exists());
        assert!(!n.home(USER, "steam.migrating").exists());
        assert!(!n.home(USER, "steam.migrated").exists());
        // No containers, and the environment is free to start again.
        assert!(!n.engine.created().contains_key("cha-env-e1-app"));
        {
            let state = n.rt.state.lock().unwrap();
            assert!(state.ports.is_empty() && state.homes.is_empty());
        }
        *n.engine.copy_exit.lock().unwrap() = 0;
        n.rt.start_environment(spec).await.unwrap();
        assert!(n.home(USER, "steam.migrated").is_file());
    }

    #[tokio::test]
    async fn an_image_that_cant_copy_still_starts_with_an_empty_home() {
        let n = node(&[]);
        *n.engine.copy_exit.lock().unwrap() = 1;
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, None)));
        n.rt.start_environment(spec).await.unwrap();
        assert!(n.home(USER, "steam").is_dir());
        assert!(n.engine.created().contains_key("cha-env-e1-app"));
    }

    #[tokio::test]
    async fn a_local_shared_directory_is_made_with_the_mountpoints_for_the_per_user_ones() {
        let n = node(&[]);
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, Some(true))));
        n.rt.start_environment(spec).await.unwrap();
        let shared = n.root.join("shared/steam");
        for dir in [".", COMPAT, SHADER] {
            assert_eq!(mode(&shared.join(dir)), 0o770, "{dir}");
        }
        let mine = n.home(USER, "steam").join(".cha-shared");
        for dir in [COMPAT, SHADER] {
            assert_eq!(mode(&mine.join(dir)), 0o700, "{dir}");
        }
        let mounts = n.app_mounts("e1");
        assert!(target(&mounts, "/srv/cha-portal/shared/steam").is_some());
        let app = &n.engine.created()["cha-env-e1-app"];
        assert!(
            app["Env"]
                .as_array()
                .unwrap()
                .contains(&json!("CHA_SHARED_DIR=/srv/cha-portal/shared/steam"))
        );

        // A second user gets the same shared directory and their own overlays.
        let other = EnvironmentSpec {
            owner: OTHER.into(),
            storage: Some(Box::new(Storage {
                home: Some(cha_wire::user_dir(OTHER, "steam")),
                legacy_volume: Some(home_volume_name(OTHER, "steam")),
                ..steam_storage(true, Some(true))
            })),
            ..app_data_spec("e2", "steam", None)
        };
        n.rt.start_environment(other).await.unwrap();
        let mounts = n.app_mounts("e2");
        let compat = target(&mounts, "/srv/cha-portal/shared/steam/steamapps/compatdata").unwrap();
        assert_eq!(
            compat["Source"],
            n.home(OTHER, "steam")
                .join(".cha-shared")
                .join(COMPAT)
                .to_string_lossy()
                .as_ref()
        );
        let first = n.app_mounts("e1");
        let compat1 = target(&first, "/srv/cha-portal/shared/steam/steamapps/compatdata").unwrap();
        assert_ne!(
            compat["Source"], compat1["Source"],
            "never one user's for another"
        );
    }

    /// A share as it looks mounted: the library with its mountpoints.
    fn share(dir: &Path) -> PathBuf {
        let share = dir.join("nas").join("steam");
        std::fs::create_dir_all(share.join(COMPAT)).unwrap();
        std::fs::create_dir_all(share.join(SHADER)).unwrap();
        std::fs::write(share.join("libraryfolder.vdf"), "\"libraryfolder\"{}").unwrap();
        share
    }

    fn tree(dir: &Path) -> Vec<(PathBuf, u32, u32)> {
        let mut out = Vec::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let meta = std::fs::symlink_metadata(e.path()).unwrap();
                out.push((e.path(), meta.mode(), meta.uid()));
                if meta.is_dir() {
                    todo.push(e.path());
                }
            }
        }
        out.sort();
        out
    }

    #[tokio::test]
    async fn an_external_shared_directory_is_mounted_and_never_touched() {
        let tmp = tempfile::tempdir().unwrap();
        let external = share(tmp.path());
        // Make it as awkward as an NFS share can be: not ours, not 0770.
        std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o777)).unwrap();
        let before = tree(&external);
        let n = node(&[("steam", external.clone())]);
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, Some(true))));
        n.rt.start_environment(spec).await.unwrap();

        let mounts = n.app_mounts("e1");
        let path = external.to_string_lossy();
        assert_eq!(target(&mounts, &path).unwrap()["Source"], path.as_ref());
        let overlay = target(&mounts, &format!("{path}/{COMPAT}")).unwrap();
        assert_eq!(
            overlay["Source"],
            n.home(USER, "steam")
                .join(".cha-shared")
                .join(COMPAT)
                .to_string_lossy()
                .as_ref()
        );
        let app = &n.engine.created()["cha-env-e1-app"];
        assert!(
            app["Env"]
                .as_array()
                .unwrap()
                .contains(&json!(format!("CHA_SHARED_DIR={path}")))
        );
        // The node's own shared directory isn't made for it, and the share's
        // files, modes and owners are as they were.
        assert!(!n.root.join("shared/steam").exists());
        assert_eq!(tree(&external), before);
        assert_eq!(mode(&external), 0o777);
    }

    #[tokio::test]
    async fn an_unmounted_share_means_no_shared_directory_but_the_home_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        // The mountpoint, empty: what is there when the NAS isn't mounted.
        let empty = tmp.path().join("nas/steam");
        std::fs::create_dir_all(&empty).unwrap();
        let n = node(&[("steam", empty.clone())]);
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, Some(true))));
        n.rt.start_environment(spec).await.unwrap();

        let mounts = n.app_mounts("e1");
        assert!(target(&mounts, "/home/cha").is_some(), "{mounts:?}");
        assert!(
            mounts
                .iter()
                .all(|m| !m["Target"].as_str().unwrap().contains("nas/steam")),
            "{mounts:?}"
        );
        let app = &n.engine.created()["cha-env-e1-app"];
        assert!(
            app["Env"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| !e.as_str().unwrap().starts_with("CHA_SHARED_DIR")),
        );
        // Nothing was made in the mountpoint, nor a local stand-in for it.
        assert!(tree(&empty).is_empty());
        assert!(!n.root.join("shared/steam").exists());

        // The same when the share isn't there at all.
        let n = node(&[("steam", tmp.path().join("gone"))]);
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, Some(false))));
        n.rt.start_environment(spec).await.unwrap();
        assert!(target(&n.app_mounts("e1"), "/home/cha").is_some());
        assert!(!tmp.path().join("gone").exists());
    }

    #[tokio::test]
    async fn an_external_shares_symlinked_places_are_refused_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let external = share(tmp.path());
        let elsewhere = tmp.path().join("server-side");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::remove_dir(external.join(COMPAT)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, external.join(COMPAT)).unwrap();
        let n = node(&[("steam", external.clone())]);
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, Some(true))));
        n.rt.start_environment(spec).await.unwrap();
        let mounts = n.app_mounts("e1");
        assert!(target(&mounts, &external.to_string_lossy()).is_none());
        assert!(target(&mounts, "/home/cha").is_some());
    }

    #[tokio::test]
    async fn storage_that_isnt_the_layouts_is_refused_before_anything_happens() {
        let n = node(&[]);
        for storage in [
            // Someone else's home.
            Storage {
                home: Some(cha_wire::user_dir(OTHER, "steam")),
                legacy_volume: None,
                ..steam_storage(true, None)
            },
            // A path out of the layout.
            Storage {
                home: Some("users/../../etc".into()),
                legacy_volume: None,
                ..Storage::default()
            },
            Storage {
                shared: Some(Shared {
                    path: "shared/../users".into(),
                    writable: true,
                    per_user: vec![],
                }),
                ..Storage::default()
            },
            // Per-user places that climb out.
            Storage {
                shared: Some(Shared {
                    path: "shared/steam".into(),
                    writable: true,
                    per_user: vec!["../../x".into()],
                }),
                ..steam_storage(true, None)
            },
            // Per-user places with no home to keep them in.
            without_home(steam_storage(true, Some(true))),
        ] {
            let err =
                n.rt.start_environment(app_data_spec("e1", "steam", Some(storage.clone())))
                    .await
                    .unwrap_err();
            assert!(
                format!("{err:#}").contains("refusing the app's storage"),
                "{storage:?}: {err:#}"
            );
        }
        // Both ways of keeping a home at once.
        let both = EnvironmentSpec {
            home: Some(home_volume_name(USER, "steam")),
            ..app_data_spec("e1", "steam", Some(steam_storage(true, None)))
        };
        assert!(n.rt.start_environment(both).await.is_err());
        assert!(
            n.engine.requests.lock().unwrap().is_empty(),
            "no engine call"
        );
        assert_eq!(
            std::fs::read_dir(&n.root).unwrap().count(),
            0,
            "no directory"
        );
    }

    /// `storage` as a portal that forgot the user's home would send it.
    fn without_home(mut storage: Storage) -> Storage {
        storage.home = None;
        storage.legacy_volume = None;
        storage
    }

    #[tokio::test]
    async fn one_environment_per_home_and_none_while_it_is_being_reset() {
        let n = node(&[]);
        let storage = Some(steam_storage(true, None));
        n.rt.start_environment(app_data_spec("e1", "steam", storage.clone()))
            .await
            .unwrap();
        let second =
            n.rt.start_environment(app_data_spec("e2", "steam", storage.clone()))
                .await
                .unwrap_err();
        assert!(format!("{second:#}").contains("already uses this home"));
        // Starting it again by the same id is the same environment.
        n.rt.start_environment(app_data_spec("e1", "steam", storage.clone()))
            .await
            .unwrap();
        // Another user's home is free.
        let other = EnvironmentSpec {
            owner: OTHER.into(),
            storage: Some(Box::new(Storage {
                home: Some(cha_wire::user_dir(OTHER, "steam")),
                legacy_volume: Some(home_volume_name(OTHER, "steam")),
                ..Storage::default()
            })),
            ..app_data_spec("e3", "steam", None)
        };
        n.rt.start_environment(other).await.unwrap();

        // A reset waits for the environment, then goes through.
        let refused =
            n.rt.delete_user_data(USER.into(), "steam".into())
                .await
                .unwrap_err();
        assert!(format!("{refused:#}").contains("is running on this node"));
        assert!(n.home(USER, "steam").is_dir());
        n.rt.stop_environment("e1").await.unwrap();
        n.rt.state
            .lock()
            .unwrap()
            .resetting
            .insert((USER.into(), "steam".into()));
        let busy =
            n.rt.start_environment(app_data_spec("e4", "steam", storage))
                .await
                .unwrap_err();
        assert!(format!("{busy:#}").contains("being reset"));
        assert!(!n.rt.state.lock().unwrap().homes.contains_key("e4"));
    }

    #[tokio::test]
    async fn resetting_deletes_the_home_and_the_old_volume_does_not_come_back() {
        let n = node(&[]);
        let volume = home_volume_name(USER, "steam");
        n.engine.volumes.lock().unwrap().insert(volume.clone());
        let storage = Some(steam_storage(true, Some(true)));
        n.rt.start_environment(app_data_spec("e1", "steam", storage.clone()))
            .await
            .unwrap();
        std::fs::write(n.home(USER, "steam").join("save"), "mine").unwrap();
        let other = n.home(OTHER, "steam");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("theirs"), "x").unwrap();
        n.rt.stop_environment("e1").await.unwrap();

        n.rt.delete_user_data(USER.into(), "steam".into())
            .await
            .unwrap();
        assert!(!n.home(USER, "steam").exists());
        assert!(other.join("theirs").is_file(), "someone else's");
        assert!(n.root.join("shared/steam").is_dir(), "the shared directory");
        assert!(n.engine.volumes.lock().unwrap().contains(&volume));

        // The next launch starts fresh, from the image: not the old volume again.
        n.engine.requests.lock().unwrap().clear();
        n.rt.start_environment(app_data_spec("e2", "steam", storage))
            .await
            .unwrap();
        let copies = n.engine.copies();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0]["Cmd"], json!(["-a", "/home/cha/.", "/to/"]));
        assert!(!n.home(USER, "steam").join("save").exists());

        // Deleting what isn't there, or what isn't an id, is fine or refused.
        n.rt.stop_environment("e2").await.unwrap();
        n.rt.delete_user_data(USER.into(), "kde".into())
            .await
            .unwrap();
        assert!(
            n.rt.delete_user_data("..".into(), "steam".into())
                .await
                .is_err()
        );
        assert!(
            n.rt.delete_user_data(USER.into(), "../steam".into())
                .await
                .is_err()
        );
        assert!(n.rt.state.lock().unwrap().resetting.is_empty());
    }

    #[tokio::test]
    async fn a_restarted_agent_knows_which_homes_are_in_use() {
        let n = node(&[]);
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, None)));
        n.rt.start_environment(spec).await.unwrap();
        // The containers are all that survives an agent restart: the app's
        // labels say whose home it has.
        {
            let mut containers = n.engine.containers.lock().unwrap();
            for c in containers.iter_mut() {
                c["State"] = json!("running");
                c["Labels"]["sh.cha.http-port"] = json!("7600");
            }
        }
        {
            let mut state = n.rt.state.lock().unwrap();
            state.ports.clear();
            state.homes.clear();
        }
        n.rt.adopt().await.unwrap();
        assert_eq!(
            n.rt.state.lock().unwrap().homes.get("e1"),
            Some(&(USER.to_string(), "steam".to_string()))
        );
        let refused = n.rt.delete_user_data(USER.into(), "steam".into()).await;
        assert!(refused.is_err());
    }

    /// An app's `/proc/self/mountinfo`, with `mounted` mounted over the root.
    fn mountinfo(mounted: &[&str]) -> String {
        let mut text = String::from(
            "1065 1020 0:52 / / rw,relatime - overlay overlay rw,lowerdir=/x\n\
             1066 1065 0:55 / /proc rw,nosuid - proc proc rw\n",
        );
        for (n, dir) in mounted.iter().enumerate() {
            text.push_str(&format!(
                "{} 1065 0:{} / {dir} rw,relatime - nfs4 nas:/steam rw\n",
                1100 + n,
                70 + n
            ));
        }
        text
    }

    const STEAM_DIR: &str = "/srv/cha-portal/shared/steam";

    #[test]
    fn mountinfo_gives_its_mount_points_unescaped() {
        let text = "36 35 98:0 /mnt1 /mnt/with\\040space rw,noatime master:1 - ext3 /dev/root rw\n\
                    37 35 0:3 / /tab\\011and\\134slash rw - proc proc rw\n\
                    38 35 0:4 / /not\\9escape rw - tmpfs tmpfs rw\n\
                    \n\
                    short line\n";
        let points = parse_mountinfo(text);
        assert!(points.contains("/mnt/with space"));
        assert!(points.contains("/tab\tand\\slash"));
        assert!(points.contains("/not\\9escape"));
        // A terminal's line endings, as `exec` with a TTY gives them.
        let points = parse_mountinfo("1 0 0:1 / /a rw - x y rw\r\n2 0 0:1 / /b rw - x y rw\r\n");
        assert_eq!(points, HashSet::from(["/a".to_string(), "/b".to_string()]));
    }

    #[test]
    fn what_is_lost_is_reported_once_and_so_is_its_return() {
        let part = |p: &str| Overlay {
            target: format!("{STEAM_DIR}/{p}"),
            part: p.into(),
        };
        let mut o = Overlays::new(vec![part(COMPAT), part(SHADER)]);
        let all = parse_mountinfo(&mountinfo(&[
            &format!("{STEAM_DIR}/{COMPAT}"),
            &format!("{STEAM_DIR}/{SHADER}"),
        ]));
        let some = parse_mountinfo(&mountinfo(&[&format!("{STEAM_DIR}/{SHADER}")]));
        let none = parse_mountinfo(&mountinfo(&[]));

        // All there from the start: nothing to say, and nothing later.
        assert_eq!(o.observe(&all), None);
        assert_eq!(o.warning(), None);
        // One goes: said, once.
        let said = o.observe(&some).expect("a change").expect("a warning");
        assert!(said.contains("(steamapps/compatdata)"), "{said}");
        assert!(!said.contains("shadercache"), "{said}");
        assert_eq!(o.observe(&some), None);
        assert_eq!(o.warning().as_deref(), Some(said.as_str()));
        // The other goes too: said again, with both.
        let said = o.observe(&none).unwrap().unwrap();
        assert!(said.contains("steamapps/compatdata, steamapps/shadercache"));
        // Back: cleared, once.
        assert_eq!(o.observe(&all), Some(None));
        assert_eq!(o.observe(&all), None);
        assert_eq!(o.warning(), None);
    }

    #[test]
    fn a_containers_per_user_directories_come_from_its_mounts() {
        let mount = |source: &str, destination: &str| ContainerMount {
            source: source.into(),
            destination: destination.into(),
        };
        let users = Path::new("/data/cha/users");
        let mounts = [
            mount("cha-env-e1", "/run/cha"),
            mount("/data/cha/users/u1/steam", "/home/cha"),
            mount("/mnt/games/steam", "/mnt/games/steam"),
            mount(
                "/data/cha/users/u1/steam/.cha-shared/steamapps/compatdata",
                "/mnt/games/steam/steamapps/compatdata",
            ),
            mount(
                "/data/cha/users/u1/steam/.cha-shared/steamapps/shadercache",
                "/mnt/games/steam/steamapps/shadercache",
            ),
            // Not ours: someone's own directory of that name elsewhere.
            mount("/elsewhere/users/u1/steam/.cha-shared/x", "/x"),
            mount("/data/cha/users/u1/steam/.cha-shared", "/y"),
            mount("/data/cha/users/u1/steam/other/steamapps", "/z"),
        ];
        assert_eq!(
            overlays_from_mounts(&mounts, users),
            [
                Overlay {
                    target: "/mnt/games/steam/steamapps/compatdata".into(),
                    part: COMPAT.into(),
                },
                Overlay {
                    target: "/mnt/games/steam/steamapps/shadercache".into(),
                    part: SHADER.into(),
                },
            ]
        );
        assert!(overlays_from_mounts(&[], users).is_empty());
    }

    #[test]
    fn the_app_is_told_its_per_user_directories() {
        let rt = mounting(&[("steam", "/mnt/games/steam")]);
        let env_of = |rt: &DockerRuntime, spec: &EnvironmentSpec| -> Vec<String> {
            rt.app_config(spec, 7600, &[])["Env"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e.as_str().unwrap().to_string())
                .filter(|e| e.starts_with("CHA_PER_USER_DIRS="))
                .collect()
        };
        let kept = app_data_spec("e1", "steam", Some(steam_storage(true, Some(true))));
        assert_eq!(
            env_of(&rt, &kept),
            [format!(
                "CHA_PER_USER_DIRS=/mnt/games/steam/{COMPAT}:/mnt/games/steam/{SHADER}"
            )]
        );
        // The node's own shared directory, in its usual place.
        let local = mounting(&[]);
        assert_eq!(
            env_of(&local, &kept),
            [format!(
                "CHA_PER_USER_DIRS={STEAM_DIR}/{COMPAT}:{STEAM_DIR}/{SHADER}"
            )]
        );
        // None without any: nothing kept, nothing shared, or no storage.
        for storage in [
            Some(steam_storage(false, Some(true))),
            Some(steam_storage(true, None)),
            None,
        ] {
            let spec = app_data_spec("e1", "steam", storage);
            assert!(env_of(&rt, &spec).is_empty());
        }
        // Always the paths the mounts use.
        let mounts = rt.storage_mounts(&kept);
        for overlay in rt.per_user_overlays(&kept) {
            assert!(target(&mounts, &overlay.target).is_some(), "{overlay:?}");
        }
    }

    /// Steam's environment, started on a fake engine, with its per-user directories.
    async fn steam_node() -> Node {
        let n = node(&[]);
        let spec = app_data_spec("e1", "steam", Some(steam_storage(true, Some(true))));
        n.rt.start_environment(spec).await.unwrap();
        n
    }

    async fn warnings_after_a_look(
        n: &Node,
        rx: &mut broadcast::Receiver<Warning>,
        mounted: &[&str],
    ) -> Vec<Option<String>> {
        *n.engine.exec_output.lock().unwrap() = mountinfo(mounted);
        n.rt.check_overlays().await;
        let mut said = Vec::new();
        while let Ok(w) = rx.try_recv() {
            assert_eq!(w.id, "e1");
            said.push(w.warning);
        }
        said
    }

    #[tokio::test]
    async fn lost_per_user_directories_are_noticed_and_said_once() {
        let n = steam_node().await;
        let mut rx = n.rt.warnings();
        let compat = format!("{STEAM_DIR}/{COMPAT}");
        let shader = format!("{STEAM_DIR}/{SHADER}");
        let both = [STEAM_DIR, compat.as_str(), shader.as_str()];

        // The look runs `cat` in the app's container, through a terminal.
        assert!(warnings_after_a_look(&n, &mut rx, &both).await.is_empty());
        let requests = n.engine.requests.lock().unwrap().clone();
        let exec = requests
            .iter()
            .find(|(_, uri, _)| uri == "/containers/cha-env-e1-app/exec")
            .expect("it execs in the app");
        assert_eq!(exec.2["Cmd"], json!(["cat", "/proc/self/mountinfo"]));
        assert_eq!(exec.2["Tty"], true);
        drop(requests);
        assert_eq!(n.rt.warnings_now().len(), 1);
        assert_eq!(n.rt.warnings_now()[0].warning, None);

        // The kernel drops one: a warning, and no second one for the same.
        let said = warnings_after_a_look(&n, &mut rx, &[STEAM_DIR, shader.as_str()]).await;
        assert_eq!(said.len(), 1);
        assert!(said[0].as_ref().unwrap().contains("steamapps/compatdata"));
        assert!(
            warnings_after_a_look(&n, &mut rx, &[STEAM_DIR, shader.as_str()])
                .await
                .is_empty()
        );
        assert_eq!(n.rt.warnings_now()[0].warning, said[0]);

        // Back (the user restarted, or it was remounted): cleared.
        let said = warnings_after_a_look(&n, &mut rx, &both).await;
        assert_eq!(said, [None]);
        assert_eq!(n.rt.warnings_now()[0].warning, None);

        // Stopping stops the watching.
        n.rt.stop_environment("e1").await.unwrap();
        assert!(n.rt.warnings_now().is_empty());
    }

    #[tokio::test]
    async fn a_look_that_fails_changes_nothing_and_says_nothing() {
        let n = steam_node().await;
        let mut rx = n.rt.warnings();
        // `cat` isn't there: the exec exits non-zero.
        *n.engine.exec_exit.lock().unwrap() = 127;
        for _ in 0..5 {
            assert!(warnings_after_a_look(&n, &mut rx, &[]).await.is_empty());
        }
        assert_eq!(n.rt.warnings_now()[0].warning, None);
        assert_eq!(
            n.rt.state.lock().unwrap().overlays["e1"].failures,
            5,
            "counted, to say so once"
        );
        // The engine can't be reached at all.
        let gone = runtime();
        gone.state
            .lock()
            .unwrap()
            .overlays
            .insert("e1".into(), Overlays::new(vec![]));
        gone.check_overlays().await;
        // It looks again, and recovers.
        *n.engine.exec_exit.lock().unwrap() = 0;
        let said = warnings_after_a_look(&n, &mut rx, &[]).await;
        assert_eq!(said.len(), 1);
        assert_eq!(n.rt.state.lock().unwrap().overlays["e1"].failures, 0);
    }

    #[tokio::test]
    async fn an_adopted_environment_is_watched_for_the_same_directories() {
        let n = steam_node().await;
        let started: Vec<Overlay> = n.rt.state.lock().unwrap().overlays["e1"].list.clone();
        assert_eq!(started.len(), 2);
        // An agent restart loses its memory; the containers remain.
        {
            let mut containers = n.engine.containers.lock().unwrap();
            for c in containers.iter_mut() {
                c["State"] = json!("running");
                c["Labels"]["sh.cha.http-port"] = json!("7600");
            }
        }
        *n.rt.state.lock().unwrap() = super::State::default();
        n.rt.adopt().await.unwrap();
        let adopted: Vec<Overlay> = n.rt.state.lock().unwrap().overlays["e1"].list.clone();
        assert_eq!(adopted, started);

        let mut rx = n.rt.warnings();
        let said = warnings_after_a_look(&n, &mut rx, &[STEAM_DIR]).await;
        assert_eq!(said.len(), 1);
        assert!(said[0].is_some());
    }

    #[tokio::test]
    async fn an_app_without_per_user_directories_isnt_watched() {
        let n = node(&[]);
        n.rt.start_environment(app_data_spec(
            "e1",
            "steam",
            Some(steam_storage(true, None)),
        ))
        .await
        .unwrap();
        assert!(n.rt.state.lock().unwrap().overlays.is_empty());
        n.rt.check_overlays().await;
        assert!(
            n.engine
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(_, uri, _)| !uri.contains("/exec"))
        );
    }

    #[test]
    fn the_node_reports_its_data_root_and_shared_dirs() {
        let rt = mounting(&[("steam", "/mnt/games/steam")]);
        assert_eq!(Runtime::data_root(&rt).as_deref(), Some("/data/cha"));
        assert_eq!(
            Runtime::shared_dirs(&rt),
            BTreeMap::from([("steam".to_string(), "/mnt/games/steam".to_string())])
        );
        assert!(Runtime::shared_dirs(&runtime()).is_empty());
    }

    #[test]
    fn the_catalogs_per_user_places_are_what_the_node_checks_an_external_dir_for() {
        assert_eq!(catalog_per_user("steam"), [COMPAT, SHADER]);
        assert!(catalog_per_user("chrome").is_empty());
        assert!(catalog_per_user("nope").is_empty());
    }

    // ---- Against a real Docker engine ----

    /// Runs a throwaway container to its end: the exit code and its output.
    async fn throwaway(docker: &Docker, name: &str, config: Value) -> (i64, String) {
        docker
            .run(name, &config, Duration::from_secs(120))
            .await
            .unwrap_or_else(|e| panic!("{name}: {e:#}"))
    }

    /// The app's own container configuration for `spec`, as the agent makes it,
    /// with what a test machine may not have (the GPU, input devices) taken
    /// out and a script for its command, with only the storage mounts.
    fn probe_config(rt: &DockerRuntime, spec: &EnvironmentSpec, script: &str) -> Value {
        let mut config = rt.app_config(spec, 7600, &[]);
        config["Entrypoint"] = json!(["sh", "-c", script]);
        config["Cmd"] = json!([]);
        config["HostConfig"]["Mounts"] = json!(rt.storage_mounts(spec));
        let host = config["HostConfig"].as_object_mut().unwrap();
        for key in ["DeviceRequests", "DeviceCgroupRules", "GroupAdd", "Init"] {
            host.remove(key);
        }
        config
    }

    /// The agent's storage against a real Docker engine: creating the
    /// directories, copying a legacy volume into a home with `cp -a`, and what
    /// an app container sees of the mounts. Not part of `cargo test`: it needs
    /// the engine, root (for ownership, as the agent has it), and two scratch
    /// directories the engine's host sees at the same paths. From a root
    /// container with the engine's socket and the scratch directories bound in
    /// at their host paths:
    ///
    /// `CHA_REAL_DOCKER=1 CHA_TEST_ROOT=/scratch/root CHA_TEST_NAS=/scratch/nas
    ///  cargo test -p cha-node --lib real_docker -- --ignored --nocapture`
    ///
    /// It makes and removes its own containers and volumes, named for a test
    /// user.
    #[tokio::test]
    #[ignore = "needs a Docker engine and root; see the doc comment"]
    async fn real_docker_migration_mounts_and_shared_directories() {
        if std::env::var("CHA_REAL_DOCKER").is_err() {
            return;
        }
        let root = PathBuf::from(std::env::var("CHA_TEST_ROOT").expect("CHA_TEST_ROOT"));
        let nas = PathBuf::from(std::env::var("CHA_TEST_NAS").expect("CHA_TEST_NAS"));
        let image = std::env::var("CHA_REAL_IMAGE").unwrap_or("cha/env-steam:dev".into());
        let user = "deadbeef-0000-4000-8000-0000000000a1";
        let volume = home_volume_name(user, "steam");
        let docker = Docker::new("/var/run/docker.sock");
        let rt = runtime_with(
            docker.clone(),
            root.clone(),
            (1000, 1000),
            BTreeMap::from([("steam".to_string(), nas.join("steam"))]),
        );
        let _ = docker.remove_volume(&volume).await;

        // A legacy home as a Steam environment left it: made by uid 1000 in a
        // volume that took the image's /home/cha (owner and mode) on first use.
        let seed = json!({
            "Image": image,
            "User": "1000:1000",
            "Entrypoint": ["sh", "-c", "set -e; cd /home/cha; \
                mkdir -p .local/share/Steam/steamapps .local/share/Steam/config .config/app; \
                echo steam > .local/share/Steam/steam.sh; chmod 755 .local/share/Steam/steam.sh; \
                echo secret > .config/app/token; chmod 600 .config/app/token; \
                ln -s /home/cha/.local/share/Steam/steam.sh .steampath; \
                mkdir -m 700 private; touch -d 2020-01-01 old; \
                printf '\"libraryfolders\"\\n{\\n\\t\"0\"\\n\\t{\\n\\t\\t\"path\"\\t\\t\"/home/cha/.local/share/Steam\"\\n\\t}\\n}\\n' \
                  | tee .local/share/Steam/steamapps/libraryfolders.vdf > .local/share/Steam/config/libraryfolders.vdf"],
            "HostConfig": { "Mounts": [{ "Type": "volume", "Source": volume, "Target": "/home/cha" }] },
        });
        let (code, out) = throwaway(&docker, "cha-test-seed", seed).await;
        assert_eq!(code, 0, "{out}");

        // NAS-like shared directory: not ours, world-writable, with the places.
        let share = nas.join("steam");
        std::fs::create_dir_all(share.join(COMPAT)).unwrap();
        std::fs::create_dir_all(share.join(SHADER)).unwrap();
        std::fs::write(share.join("libraryfolder.vdf"), "\"libraryfolder\"\n{\n}\n").unwrap();
        for dir in [
            &share,
            &share.join("steamapps"),
            &share.join(COMPAT),
            &share.join(SHADER),
        ] {
            std::os::unix::fs::chown(dir, Some(99), Some(100)).unwrap();
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        }
        let nas_before = tree(&share);

        // The first launch: the volume is copied in.
        let mut spec = EnvironmentSpec {
            image: image.clone(),
            owner: user.into(),
            ..app_data_spec(
                "etest",
                "steam",
                Some(steam_storage_for(user, true, Some(true))),
            )
        };
        // Only the test user's, and only its volume: never anyone's real one.
        check_storage(&spec).unwrap();
        assert_eq!(
            spec.storage.as_ref().unwrap().legacy_volume.as_deref(),
            Some(volume.as_str())
        );
        assert!(volume.starts_with("cha-home-deadbeef-"));
        rt.prepare_storage(&mut spec).await.unwrap();
        let home = root.join("users").join(user).join("steam");
        let meta = |p: &Path| std::fs::symlink_metadata(p).unwrap();
        assert_eq!(
            (meta(&home).uid(), meta(&home).gid(), mode(&home)),
            (1000, 1000, 0o700)
        );
        assert_eq!(mode(&home.join(".local/share/Steam/steam.sh")), 0o755);
        assert_eq!(mode(&home.join(".config/app/token")), 0o600);
        assert_eq!(meta(&home.join(".config/app/token")).uid(), 1000);
        assert_eq!(mode(&home.join("private")), 0o700);
        assert_eq!(
            std::fs::read_link(home.join(".steampath")).unwrap(),
            Path::new("/home/cha/.local/share/Steam/steam.sh")
        );
        assert_eq!(meta(&home.join("old")).mtime(), 1577836800, "times kept");
        assert!(root.join(format!("users/{user}/steam.migrated")).is_file());
        assert!(!root.join(format!("users/{user}/steam.migrating")).exists());
        // The overlays' sources, in the home; the NAS directory is as it was.
        assert_eq!(mode(&home.join(".cha-shared").join(COMPAT)), 0o700);
        assert_eq!(tree(&share), nas_before);
        assert!(
            !root.join("shared/steam").exists(),
            "not made for an external one"
        );
        assert!(
            docker.volume_exists(&volume).await.unwrap(),
            "the volume stays"
        );

        // What an app container sees: the same paths, writable where they
        // should be, and the per-user places apart from the share's own.
        let script = format!(
            "set -u; echo uid=$(id -u); echo shared=$CHA_SHARED_DIR; \
             touch /home/cha/probe && echo home=writable; \
             touch $CHA_SHARED_DIR/steamapps/appmanifest_1.acf && echo share=writable || echo share=readonly; \
             touch $CHA_SHARED_DIR/{COMPAT}/prefix && echo overlay=writable || echo overlay=readonly; \
             ls /home/cha/.cha-shared/{COMPAT}; \
             steam-library \"$CHA_SHARED_DIR\"; \
             cat /home/cha/.local/share/Steam/steamapps/libraryfolders.vdf"
        );
        let app = probe_config(&rt, &spec, &script);
        let (code, out) = throwaway(&docker, "cha-test-app", app).await;
        println!("--- the app's view, shared directory writable:\n{out}");
        assert_eq!(code, 0, "{out}");
        let share_path = share.to_string_lossy().into_owned();
        assert!(out.contains("uid=1000") && out.contains(&format!("shared={share_path}")));
        assert!(out.contains("home=writable") && out.contains("share=writable"));
        assert!(out.contains("overlay=writable"));
        // The overlay is the user's own directory: what went in is in the home...
        assert!(
            home.join(".cha-shared")
                .join(COMPAT)
                .join("prefix")
                .is_file()
        );
        // ...and not in the share's own.
        assert!(!share.join(COMPAT).join("prefix").exists());
        // The library was listed in the home's Steam, once.
        assert_eq!(
            out.matches(&format!("\"path\"\t\t\"{share_path}\""))
                .count(),
            1,
            "{out}"
        );
        assert!(out.contains("\"1\""));

        // Read-only sharing: the library can't be written, the overlays can.
        let mut read_only = spec.clone();
        read_only
            .storage
            .as_mut()
            .unwrap()
            .shared
            .as_mut()
            .unwrap()
            .writable = false;
        let app = probe_config(&rt, &read_only, &script);
        let (_, out) = throwaway(&docker, "cha-test-app", app).await;
        println!("--- shared directory read-only:\n{out}");
        assert!(out.contains("share=readonly"), "{out}");
        assert!(out.contains("overlay=writable"), "{out}");

        // A launch after that finds its home as it was left: nothing copied.
        let mut again = spec.clone();
        rt.prepare_storage(&mut again).await.unwrap();
        assert!(home.join("probe").is_file(), "left as the app made it");

        // The share isn't mounted (an empty mountpoint): no shared directory, and the home.
        let empty = nas.join("unmounted");
        std::fs::create_dir_all(&empty).unwrap();
        let rt2 = runtime_with(
            docker.clone(),
            root.clone(),
            (1000, 1000),
            BTreeMap::from([("steam".to_string(), empty.clone())]),
        );
        let mut spec2 = spec.clone();
        rt2.prepare_storage(&mut spec2).await.unwrap();
        assert!(spec2.storage.as_ref().unwrap().shared.is_none());
        assert_eq!(rt2.storage_mounts(&spec2).len(), 1);
        assert_eq!(std::fs::read_dir(&empty).unwrap().count(), 0);

        // A local shared directory, made by the agent for apps to write.
        let rt3 = runtime_with(docker.clone(), root.clone(), (1000, 1000), BTreeMap::new());
        let mut local = spec.clone();
        rt3.prepare_storage(&mut local).await.unwrap();
        let app = probe_config(
            &rt3,
            &local,
            &script.replace("steam-library \"$CHA_SHARED_DIR\";", ""),
        );
        let (code, out) = throwaway(&docker, "cha-test-app", app).await;
        println!("--- a local shared directory:\n{out}");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("shared=/srv/cha-portal/shared/steam"), "{out}");
        assert!(
            out.contains("share=writable") && out.contains("overlay=writable"),
            "{out}"
        );
        assert_eq!(mode(&root.join("shared/steam")), 0o770);

        // What Docker makes of an anonymous volume at such a path: the owner
        // the app would have (why the portal doesn't use one for per-user parts).
        let anonymous = json!({
            "Image": image,
            "User": "1000:1000",
            "Entrypoint": ["sh", "-c", "stat -c '%u:%g %a' /srv/x/y; touch /srv/x/y/f && echo writable || echo not-writable"],
            "HostConfig": { "Mounts": [{ "Type": "volume", "Target": "/srv/x/y" }] },
        });
        let (_, out) = throwaway(&docker, "cha-test-anon", anonymous).await;
        println!(
            "--- an anonymous volume at an unmade path, as uid 1000: {}",
            out.trim().replace('\n', " | ")
        );

        // Resetting.
        rt.delete_user_data(user.into(), "steam".into())
            .await
            .unwrap();
        assert!(!home.exists());
        assert!(docker.volume_exists(&volume).await.unwrap());
        let mut fresh = spec.clone();
        rt.prepare_storage(&mut fresh).await.unwrap();
        assert!(
            home.join(".bashrc").exists(),
            "from the image's home, not the volume"
        );
        assert!(!home.join("old").exists());

        // Clean up.
        let _ = docker.remove_volume(&volume).await;
        std::fs::remove_dir_all(root.join("users")).unwrap();
        std::fs::remove_dir_all(root.join("shared")).unwrap();
        // (The scratch directories are bind mounts: their contents, not them.)
        std::fs::remove_dir_all(&share).unwrap();
        std::fs::remove_dir_all(&empty).unwrap();
    }

    // ---- Custom environments (ADR 0021) ----

    use cha_wire::{HostMount, HostOptions, HostPort, MountSource, NetworkFs};

    fn settings(
        mode: &str,
        mounts: &str,
        ports: &str,
        caps: &str,
        devices: &str,
    ) -> HostOptionsConfig {
        HostOptionsConfig::from_settings(mode, mounts, ports, caps, devices).unwrap()
    }

    /// A node that allows a media folder (read-only), a roms folder, a UDP
    /// range, a capability and a device.
    fn allowlisting() -> DockerRuntime {
        let mut rt = runtime();
        rt.config.host_options = settings(
            "allowlist",
            "media=/mnt/media:ro,roms=/srv/roms",
            "27015-27030/udp,25565/tcp",
            "SYS_NICE",
            "/dev/dri/card1",
        );
        rt
    }

    fn full() -> DockerRuntime {
        let mut rt = runtime();
        rt.config.host_options = settings("full", "media=/mnt/media", "27015-27030/udp", "", "");
        rt
    }

    fn with_host(opts: HostOptions) -> EnvironmentSpec {
        EnvironmentSpec {
            host: Some(Box::new(opts)),
            ..spec(SecurityProfile::Browser)
        }
    }

    fn named_mount(name: &str, target: &str, read_only: bool) -> HostMount {
        HostMount {
            source: MountSource::Named { name: name.into() },
            target: target.into(),
            read_only,
        }
    }

    fn path_mount(path: &str, target: &str) -> HostMount {
        HostMount {
            source: MountSource::Path { path: path.into() },
            target: target.into(),
            read_only: false,
        }
    }

    fn udp(container: u16, host: Option<u16>) -> HostPort {
        HostPort {
            container,
            protocol: Protocol::Udp,
            host,
        }
    }

    fn tcp(container: u16, host: Option<u16>) -> HostPort {
        HostPort {
            container,
            protocol: Protocol::Tcp,
            host,
        }
    }

    fn env_of(config: &Value) -> Vec<&str> {
        config["Env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect()
    }

    fn planned(rt: &DockerRuntime, spec: &EnvironmentSpec) -> Value {
        let plan = rt.plan_host(spec, &HashSet::new()).unwrap();
        rt.app_config_with(spec, 7600, &[], &plan)
    }

    fn refusal(rt: &DockerRuntime, spec: &EnvironmentSpec) -> String {
        format!("{:#}", rt.plan_host(spec, &HashSet::new()).unwrap_err())
    }

    fn with_env(mut spec: EnvironmentSpec, vars: &[(&str, &str)]) -> EnvironmentSpec {
        spec.env = Some(Box::new(
            vars.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        ));
        spec
    }

    #[test]
    fn extra_variables_follow_the_nodes_own() {
        let rt = runtime();
        let plain = spec(SecurityProfile::Browser);
        let spec = with_env(plain.clone(), &[("PROTON_LOG", "1"), ("MANGOHUD", "0")]);
        assert!(rt.check_extras(&spec).is_ok());
        let app = rt.app_config(&spec, 7600, &[]);
        let env = env_of(&app);
        let at = |entry: &str| env.iter().position(|e| *e == entry).unwrap();
        assert!(at("CHA_WIDTH=2560") < at("MANGOHUD=0"));
        assert!(at("MANGOHUD=0") < at("PROTON_LOG=1"));
        // Nothing added without any.
        let bare = rt.app_config(&plain, 7600, &[]);
        assert_eq!(env_of(&bare).len(), app_env(2560, 1440, 60).len());
    }

    #[test]
    fn the_nodes_own_variables_are_refused() {
        let rt = runtime();
        for name in ["CHA_WIDTH", "HOME", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR"] {
            let spec = with_env(spec(SecurityProfile::Browser), &[(name, "x")]);
            let err = format!("{:#}", rt.check_extras(&spec).unwrap_err());
            assert!(err.contains("set by the node"), "{name}: {err}");
        }
        // Not for a gateway either, which has no app to give them to.
        let gateway = with_env(gateway_spec(), &[("A", "1")]);
        assert!(rt.check_extras(&gateway).is_err());
    }

    #[tokio::test]
    async fn a_refused_variable_stops_the_start_before_anything_is_made() {
        let (engine, _dir, docker) = fake_engine();
        let rt = runtime_on(docker);
        let spec = with_env(spec(SecurityProfile::Browser), &[("CHA_X", "1")]);
        let err = format!("{:#}", rt.start_environment(spec).await.unwrap_err());
        assert!(err.contains("CHA_X is set by the node"), "{err}");
        assert!(engine.created().is_empty());
    }

    #[test]
    fn a_shared_base_keeps_its_data_under_the_base_template() {
        // A custom Steam variant that uses Steam's home and library.
        let rt = mounting(&[]);
        let mut storage = steam_storage(true, Some(true));
        storage.data_template = Some("steam".into());
        let spec = app_data_spec("e1", "custom.steam-big", Some(storage));
        assert!(check_storage(&spec).is_ok());
        assert_eq!(
            home_key(&spec),
            Some((USER.to_string(), "steam".to_string()))
        );
        let mounts = rt.storage_mounts(&spec);
        assert_eq!(
            target(&mounts, "/home/cha").unwrap()["Source"],
            format!("/data/cha/users/{USER}/steam")
        );
        // The base's shared directory, at the base's path.
        let shared = target(&mounts, "/srv/cha-portal/shared/steam").unwrap();
        assert_eq!(shared["Source"], "/data/cha/shared/steam");
        let app = rt.app_config(&spec, 7600, &[]);
        assert!(env_of(&app).contains(&"CHA_SHARED_DIR=/srv/cha-portal/shared/steam"));
        assert!(
            env_of(&app)
                .iter()
                .any(|e| e.starts_with("CHA_PER_USER_DIRS=/srv/cha-portal/shared/steam/"))
        );
        // The container is the custom environment's; its data is the base's.
        assert_eq!(app["Labels"]["sh.cha.template"], "custom.steam-big");
        assert_eq!(app["Labels"]["sh.cha.data-template"], "steam");
        assert_eq!(app["Labels"]["sh.cha.owner"], USER);
        // Without the base named, the node refuses the base's paths.
        let mut alone = spec.clone();
        alone.storage.as_mut().unwrap().data_template = None;
        assert!(check_storage(&alone).is_err());
        // Its streamer is the environment's own.
        let streamer = rt.streamer_config(&spec, 7600);
        assert_eq!(streamer["Labels"]["sh.cha.template"], "custom.steam-big");
    }

    #[test]
    fn a_shared_base_keeps_a_shared_directory_kept_elsewhere() {
        let rt = mounting(&[("steam", "/mnt/games/steam")]);
        let mut storage = steam_storage(true, Some(true));
        storage.data_template = Some("steam".into());
        let spec = app_data_spec("e1", "custom.steam-big", Some(storage));
        let mounts = rt.storage_mounts(&spec);
        assert_eq!(
            target(&mounts, "/mnt/games/steam").unwrap()["Source"],
            "/mnt/games/steam"
        );
    }

    #[test]
    fn one_home_is_never_live_twice_even_under_two_templates() {
        let rt = runtime();
        let base = app_data_spec("e1", "steam", Some(steam_storage(true, None)));
        let variant = {
            let mut storage = steam_storage(true, None);
            storage.data_template = Some("steam".into());
            app_data_spec("e2", "custom.steam-big", Some(storage))
        };
        rt.claim_home(&base).unwrap();
        let err = rt.claim_home(&variant).unwrap_err().to_string();
        assert!(
            err.contains("environment e1 already uses this home"),
            "{err}"
        );
        // Another user's is fine, and so is the same once the first ends.
        let mut other = variant.clone();
        other.id = "e3".into();
        other.owner = OTHER.into();
        other.storage = Some(Box::new(Storage {
            data_template: Some("steam".into()),
            ..steam_storage_for(OTHER, true, None)
        }));
        rt.claim_home(&other).unwrap();
        rt.state.lock().unwrap().homes.remove("e1");
        rt.claim_home(&variant).unwrap();
    }

    #[tokio::test]
    async fn deleting_data_waits_for_the_variant_that_uses_it() {
        let rt = runtime();
        let mut storage = steam_storage(true, None);
        storage.data_template = Some("steam".into());
        rt.claim_home(&app_data_spec("e2", "custom.steam-big", Some(storage)))
            .unwrap();
        let err = rt
            .delete_user_data(USER.into(), "steam".into())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("is running on this node"), "{err}");
    }

    #[test]
    fn nothing_is_asked_of_a_node_at_off() {
        let rt = runtime();
        // An empty request is no request.
        assert!(
            rt.plan_host(&with_host(HostOptions::default()), &HashSet::new())
                .is_ok()
        );
        let spec = with_host(HostOptions {
            cap_add: vec!["SYS_NICE".into()],
            ..Default::default()
        });
        let err = refusal(&rt, &spec);
        assert!(err.contains("CHA_HOST_OPTIONS=off"), "{err}");
        assert!(rt.check_extras(&spec).is_err());
        assert_eq!(rt.host_policy(), HostPolicy::default());
    }

    #[test]
    fn an_allowlist_maps_each_option_to_the_containers_config() {
        let rt = allowlisting();
        let spec = with_host(HostOptions {
            mounts: vec![
                // Read-write asked of a read-only mount: read-only.
                named_mount("media", "/mnt/media", false),
                named_mount("roms", "/mnt/roms", false),
            ],
            ports: vec![udp(27015, Some(27020)), udp(27016, None), tcp(25565, None)],
            cap_add: vec!["SYS_NICE".into()],
            devices: vec!["/dev/dri/card1".into()],
            ..Default::default()
        });
        let app = planned(&rt, &spec);
        let host = &app["HostConfig"];
        let mounts = host["Mounts"].as_array().unwrap();
        let media = target(mounts, "/mnt/media").unwrap();
        assert_eq!(media["Type"], "bind");
        assert_eq!(media["Source"], "/mnt/media");
        assert_eq!(media["ReadOnly"], true, "the owner's :ro is a ceiling");
        assert_eq!(media["BindOptions"]["CreateMountpoint"], false);
        let roms = target(mounts, "/mnt/roms").unwrap();
        assert_eq!(roms["Source"], "/srv/roms");
        assert_eq!(roms["ReadOnly"], false);
        // A request for read-only is kept where the owner allowed read-write.
        let ro = with_host(HostOptions {
            mounts: vec![named_mount("roms", "/mnt/roms", true)],
            ..Default::default()
        });
        let app_ro = planned(&rt, &ro);
        let mounts_ro = app_ro["HostConfig"]["Mounts"].as_array().unwrap();
        assert_eq!(target(mounts_ro, "/mnt/roms").unwrap()["ReadOnly"], true);
        // Ports: the one asked for, then the range's first free, per protocol.
        assert_eq!(
            host["PortBindings"],
            json!({
                "27015/udp": [{ "HostIp": "", "HostPort": "27020" }],
                "27016/udp": [{ "HostIp": "", "HostPort": "27015" }],
                "25565/tcp": [{ "HostIp": "", "HostPort": "25565" }],
            })
        );
        assert_eq!(app["ExposedPorts"].as_object().unwrap().len(), 3);
        // Added capabilities: the app starts as root with only them and what
        // cha-run needs to drop to uid 1000 keeping them.
        assert_eq!(
            host["CapAdd"],
            json!(["SYS_NICE", "SETUID", "SETGID", "SETPCAP"])
        );
        assert_eq!(app["User"], "0:0");
        assert!(
            app["Env"]
                .as_array()
                .unwrap()
                .contains(&json!("CHA_AMBIENT_CAPS=SYS_NICE"))
        );
        // Without them it stays uid 1000, with nothing to hand down.
        assert_eq!(app_ro["User"], app_user());
        assert!(app_ro["HostConfig"].get("CapAdd").is_none());
        assert!(
            !app_ro["Env"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e.as_str().unwrap().starts_with("CHA_AMBIENT_CAPS="))
        );
        // CapDrop and the rest of the confinement stay.
        assert_eq!(host["CapDrop"], json!(["ALL"]));
        assert!(host.get("Privileged").is_none());
        assert!(host.get("NetworkMode").is_none());
        let device = host["Devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["PathOnHost"] == "/dev/dri/card1")
            .unwrap();
        assert_eq!(device["PathInContainer"], "/dev/dri/card1");
        assert_eq!(device["CgroupPermissions"], "rwm");
        // The node's own mounts are all still there.
        assert!(target(mounts, "/run/cha").is_some());
    }

    #[test]
    fn an_allowlist_refuses_what_it_does_not_name() {
        let rt = allowlisting();
        let refused = |opts: HostOptions| refusal(&rt, &with_host(opts));
        let err = refused(HostOptions {
            mounts: vec![named_mount("secrets", "/mnt/s", false)],
            ..Default::default()
        });
        assert!(err.contains("no mount named secrets"), "{err}");
        assert!(err.contains("CHA_HOST_OPTIONS=allowlist"), "{err}");
        let err = refused(HostOptions {
            mounts: vec![path_mount("/etc", "/mnt/etc")],
            ..Default::default()
        });
        assert!(err.contains("host paths"), "{err}");
        let err = refused(HostOptions {
            cap_add: vec!["SYS_ADMIN".into()],
            ..Default::default()
        });
        assert!(err.contains("doesn't allow SYS_ADMIN"), "{err}");
        let err = refused(HostOptions {
            devices: vec!["/dev/sda".into()],
            ..Default::default()
        });
        assert!(err.contains("doesn't allow /dev/sda"), "{err}");
        let err = refused(HostOptions {
            ports: vec![udp(1, Some(8080))],
            ..Default::default()
        });
        assert!(err.contains("doesn't allow 8080/udp"), "{err}");
        let err = refused(HostOptions {
            privileged: true,
            ..Default::default()
        });
        assert!(err.contains("privileged"), "{err}");
        // Malformed asks are refused whatever the mode.
        let err = refused(HostOptions {
            mounts: vec![named_mount("media", "/home/cha/x", false)],
            ..Default::default()
        });
        assert!(err.contains("mounts itself"), "{err}");
    }

    #[test]
    fn only_an_image_that_drops_root_itself_takes_added_capabilities() {
        let image = |entrypoint: Value, user: &str| json!({ "Config": { "Entrypoint": entrypoint, "User": user } });
        for user in ["cha", "1000", "1000:1000"] {
            assert!(takes_added_caps("x", Some(&image(json!(["cha-run"]), user))).is_ok());
        }
        // Another entrypoint would run as root itself.
        for (entrypoint, user) in [
            (json!(["/bin/sh", "-c"]), "cha"),
            (json!(["cha-run", "--x"]), "cha"),
            (json!(null), "cha"),
            (json!(["cha-run"]), "root"),
            (json!(["cha-run"]), ""),
        ] {
            let err = takes_added_caps("x", Some(&image(entrypoint, user))).unwrap_err();
            assert!(err.to_string().contains("can't take added capabilities"));
        }
        assert!(takes_added_caps("x", None).is_err());
    }

    #[test]
    fn the_agents_docker_socket_is_never_mounted() {
        let mut rt = full();
        rt.docker = Docker::new("/run/cha-engine/docker.sock");
        for path in [
            "/run/cha-engine/docker.sock",
            "/run/cha-engine",
            "/run",
            "/",
        ] {
            let spec = with_host(HostOptions {
                mounts: vec![path_mount(path, "/mnt/x")],
                ..Default::default()
            });
            let err = refusal(&rt, &spec);
            assert!(err.contains("Docker socket"), "{path}: {err}");
        }
        // Nor through a name the owner gave to a folder around it.
        rt.config.host_options = settings("full", "run=/run", "", "", "");
        let spec = with_host(HostOptions {
            mounts: vec![named_mount("run", "/mnt/run", true)],
            ..Default::default()
        });
        assert!(refusal(&rt, &spec).contains("Docker socket"));
        // A folder beside it is fine.
        let spec = with_host(HostOptions {
            mounts: vec![path_mount("/run/media", "/mnt/x")],
            ..Default::default()
        });
        assert!(rt.plan_host(&spec, &HashSet::new()).is_ok());
    }

    #[test]
    fn a_target_may_not_cover_a_shared_directory_kept_elsewhere() {
        let mut rt = mounting(&[("steam", "/mnt/games/steam")]);
        rt.config.host_options = settings("allowlist", "media=/mnt/media", "", "", "");
        let mut storage = steam_storage(true, Some(true));
        storage.data_template = Some("steam".into());
        let mut spec = app_data_spec("e1", "steam", Some(storage));
        for target in ["/mnt/games", "/mnt/games/steam", "/mnt/games/steam/sub"] {
            spec.host = Some(Box::new(HostOptions {
                mounts: vec![named_mount("media", target, true)],
                ..Default::default()
            }));
            let err = refusal(&rt, &spec);
            assert!(err.contains("/mnt/games/steam"), "{target}: {err}");
        }
        spec.host = Some(Box::new(HostOptions {
            mounts: vec![named_mount("media", "/mnt/media", true)],
            ..Default::default()
        }));
        assert!(rt.plan_host(&spec, &HashSet::new()).is_ok());
    }

    #[test]
    fn full_adds_privilege_the_hosts_network_and_security_options() {
        let rt = full();
        let spec = with_host(HostOptions {
            privileged: true,
            network_host: true,
            security_opt: vec!["label=disable".into()],
            mounts: vec![path_mount("/srv/games", "/games")],
            ..Default::default()
        });
        let app = planned(&rt, &spec);
        let host = &app["HostConfig"];
        assert_eq!(host["Privileged"], true);
        assert_eq!(host["NetworkMode"], "host");
        let options = host["SecurityOpt"].as_array().unwrap();
        assert_eq!(options[0], "no-new-privileges");
        assert_eq!(options.last().unwrap(), "label=disable");
        let games = target(host["Mounts"].as_array().unwrap(), "/games").unwrap();
        assert_eq!(games["Source"], "/srv/games");
        assert_eq!(games["ReadOnly"], false);
        // Named mounts stay usable by name.
        let by_name = with_host(HostOptions {
            mounts: vec![named_mount("media", "/mnt/media", false)],
            ..Default::default()
        });
        assert!(rt.plan_host(&by_name, &HashSet::new()).is_ok());
        // The host's network and ports don't go together.
        let both = with_host(HostOptions {
            network_host: true,
            ports: vec![udp(27015, None)],
            ..Default::default()
        });
        assert!(refusal(&rt, &both).contains("ports mean nothing"));
        // A name the owner never gave is refused even in full.
        let unknown = with_host(HostOptions {
            mounts: vec![named_mount("nope", "/mnt/n", false)],
            ..Default::default()
        });
        assert!(refusal(&rt, &unknown).contains("no mount named nope"));
    }

    #[test]
    fn a_network_share_is_a_volume_of_the_local_driver() {
        let rt = full();
        let spec = with_host(HostOptions {
            mounts: vec![HostMount {
                source: MountSource::Network {
                    fs_type: NetworkFs::Nfs,
                    device: ":/export/media".into(),
                    options: "addr=10.0.0.5,nfsvers=4".into(),
                },
                target: "/mnt/nas".into(),
                read_only: true,
            }],
            ..Default::default()
        });
        let app = planned(&rt, &spec);
        let mount = target(app["HostConfig"]["Mounts"].as_array().unwrap(), "/mnt/nas").unwrap();
        assert_eq!(mount["Type"], "volume");
        assert_eq!(mount["Source"], "cha-hostvol-e1-0");
        assert_eq!(mount["ReadOnly"], true);
        assert_eq!(mount["VolumeOptions"]["NoCopy"], true);
        assert_eq!(mount["VolumeOptions"]["Labels"][LABEL_ENV], "e1");
        assert_eq!(
            mount["VolumeOptions"]["DriverConfig"],
            json!({
                "Name": "local",
                "Options": {
                    "type": "nfs",
                    "device": ":/export/media",
                    "o": "addr=10.0.0.5,nfsvers=4",
                },
            })
        );
        // An allowlist has no shares.
        assert!(refusal(&allowlisting(), &spec).contains("network shares"));
    }

    #[test]
    fn host_ports_come_from_the_range_skipping_those_in_use() {
        let ranges = [PortRange {
            start: 27015,
            end: 27018,
            protocol: Protocol::Udp,
        }];
        let used = HashSet::from([
            (27015, Protocol::Udp),
            (27017, Protocol::Udp),
            // Another protocol's use doesn't count.
            (27016, Protocol::Tcp),
        ]);
        let ports = assign_ports(
            &[udp(1, None), udp(2, None), udp(3, Some(27030))],
            &ranges,
            &used,
            &[],
        )
        .unwrap();
        let hosts: Vec<_> = ports.iter().map(|p| p.host).collect();
        assert_eq!(hosts, vec![Some(27016), Some(27018), Some(27030)]);
        // The range runs out.
        let err = assign_ports(
            &[udp(1, None), udp(2, None), udp(3, None)],
            &ranges,
            &used,
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("every udp port this node allows is in use"),
            "{err}"
        );
        // A port asked for that is taken.
        let err = assign_ports(&[udp(1, Some(27015))], &ranges, &used, &[])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("27015/udp is already used by another environment"),
            "{err}"
        );
        let err = assign_ports(
            &[udp(7600, Some(7600))],
            &ranges,
            &HashSet::from([(7600, Protocol::Udp)]),
            &[7600],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("streamers"), "{err}");
        // No range for the protocol: the engine chooses (`full` only gets here).
        let ports = assign_ports(&[tcp(80, None)], &ranges, &used, &[]).unwrap();
        assert_eq!(ports[0].host, None);
        let mut config = json!({ "HostConfig": {} });
        HostPlan {
            ports,
            ..Default::default()
        }
        .apply(&mut config);
        assert_eq!(
            config["HostConfig"]["PortBindings"]["80/tcp"],
            json!([{ "HostIp": "", "HostPort": "" }])
        );
    }

    #[test]
    fn ports_in_use_are_read_from_other_environments_apps() {
        let container = |env: &str, ports: Value| -> ContainerSummary {
            serde_json::from_value(json!({
                "Id": env, "State": "running", "Labels": { LABEL_ENV: env }, "Ports": ports,
            }))
            .unwrap()
        };
        let listed = [
            container(
                "a",
                json!([
                    { "IP": "0.0.0.0", "PrivatePort": 1, "PublicPort": 27015, "Type": "udp" },
                    { "IP": "::", "PrivatePort": 1, "PublicPort": 27015, "Type": "udp" },
                    { "PrivatePort": 9, "Type": "tcp" },
                ]),
            ),
            container(
                "mine",
                json!([{ "PrivatePort": 1, "PublicPort": 27016, "Type": "udp" }]),
            ),
        ];
        let used = used_ports(&listed, "mine", &[7600]);
        assert!(used.contains(&(27015, Protocol::Udp)));
        assert!(!used.contains(&(27016, Protocol::Udp)));
        // The agent's own ports, both protocols.
        assert!(used.contains(&(7600, Protocol::Tcp)) && used.contains(&(7600, Protocol::Udp)));
        // Published once, whatever the address family.
        assert_eq!(published(&listed[0].ports), vec![udp(1, Some(27015))]);
    }

    #[test]
    fn volumes_are_matched_to_their_environment_exactly() {
        assert!(is_host_volume_of("cha-hostvol-e1-0", "e1"));
        assert!(is_host_volume_of("cha-hostvol-e1-12", "e1"));
        assert!(!is_host_volume_of("cha-hostvol-e1-2-0", "e1"));
        assert!(!is_host_volume_of("cha-hostvol-e1-x", "e1"));
        assert!(is_host_volume_of("cha-hostvol-e1-2-0", "e1-2"));
    }

    #[test]
    fn the_inventory_says_what_the_agent_reads_and_allows() {
        let features = crate::inventory::collect().spec_features;
        for f in ["env", "data-template", "host-options"] {
            assert!(features.iter().any(|x| x == f), "{f}");
        }
        let policy = allowlisting().host_policy();
        assert_eq!(policy.mode, HostOptionsMode::Allowlist);
        assert!(
            policy
                .mounts
                .iter()
                .any(|m| m.name == "media" && m.read_only)
        );
        assert!(
            !serde_json::to_string(&policy)
                .unwrap()
                .contains("/mnt/media")
        );
    }

    #[tokio::test]
    async fn published_ports_are_reported_and_shares_removed_with_the_environment() {
        let (engine, _dir, docker) = fake_engine();
        let mut rt = runtime_on(docker);
        rt.config.host_options = settings("full", "", "27015-27030/udp", "", "");
        let share = HostMount {
            source: MountSource::Network {
                fs_type: NetworkFs::Cifs,
                device: "//nas/media".into(),
                options: "addr=10.0.0.5".into(),
            },
            target: "/mnt/nas".into(),
            read_only: false,
        };
        let request = |id: &str| EnvironmentSpec {
            id: id.into(),
            host: Some(Box::new(HostOptions {
                mounts: vec![share.clone()],
                ports: vec![udp(27015, None), tcp(80, None)],
                ..Default::default()
            })),
            ..spec(SecurityProfile::Browser)
        };
        rt.start_environment(request("e1")).await.unwrap();
        rt.start_environment(request("e2")).await.unwrap();
        let first = rt.published_ports("e1").await.unwrap();
        let second = rt.published_ports("e2").await.unwrap();
        // The second sees the first's port taken; with no TCP range, the
        // engine's choice is read back.
        assert_eq!(first, vec![tcp(80, Some(40002)), udp(27015, Some(27015))]);
        assert_eq!(second[1], udp(27015, Some(27016)));
        let created = engine.created();
        let mounts = created["cha-env-e1-app"]["HostConfig"]["Mounts"]
            .as_array()
            .unwrap();
        assert_eq!(
            target(mounts, "/mnt/nas").unwrap()["Source"],
            "cha-hostvol-e1-0"
        );
        assert!(engine.volumes.lock().unwrap().contains("cha-hostvol-e2-0"));
        // Stopping removes the environment's shares, and only its own.
        rt.stop_environment("e1").await.unwrap();
        let volumes = engine.volumes.lock().unwrap().clone();
        assert!(!volumes.contains("cha-hostvol-e1-0"), "{volumes:?}");
        assert!(volumes.contains("cha-hostvol-e2-0"));
    }

    #[tokio::test]
    async fn a_refused_host_option_stops_the_start_before_anything_is_made() {
        let (engine, _dir, docker) = fake_engine();
        let rt = runtime_on(docker);
        let spec = with_host(HostOptions {
            cap_add: vec!["SYS_NICE".into()],
            ..Default::default()
        });
        let err = format!("{:#}", rt.start_environment(spec).await.unwrap_err());
        assert!(err.contains("host options refused"), "{err}");
        assert!(engine.created().is_empty());
    }
}
