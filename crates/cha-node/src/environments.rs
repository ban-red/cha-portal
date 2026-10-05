//! Environments on this node (plan §4.2). Each one is:
//! - a **streamer** container (`cha-streamer`): our compositor, NVENC and
//!   WebRTC, on the host network so its WebRTC port is the node's;
//! - an **app** container from the template's image: uid 1000 plus the render
//!   node's group, no capabilities, no privilege gain, Docker's seccomp profile
//!   (or the `browser` one, which lets browser sandboxes create namespaces);
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
    EnvironmentSpec, PER_USER_DIR, SHARED_MOUNT_ROOT, SecurityProfile, StreamerEndpoint,
    home_volume_name, is_home_volume_name, valid_template_id, valid_user_id,
};
use futures_util::future::BoxFuture;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::crashlog::{self, Tail};
use crate::docker::{ContainerEvent, ContainerMount, Docker, encode};
use crate::storage::{DataRoot, Seed, plan_seed};

const LABEL_ENV: &str = "sh.cha.env";
const LABEL_ROLE: &str = "sh.cha.role";
const LABEL_HTTP_PORT: &str = "sh.cha.http-port";
/// On legacy home volumes (`docker volume ls --filter label=sh.cha.home`): `1`,
/// and whose (`sh.cha.owner`) and for which template (`sh.cha.template`). On an
/// app container whose home is a directory under the data root: the same two,
/// which is how a restarted agent knows which homes are in use.
const LABEL_HOME: &str = "sh.cha.home";
const LABEL_OWNER: &str = "sh.cha.owner";
const LABEL_TEMPLATE: &str = "sh.cha.template";
pub const APP_UID: u32 = 1000;
const RUNTIME_DIR: &str = "/run/cha";
/// Where the streamer puts gamepad nodes (`dev/`) and udev entries (`udev/`).
const INPUT_DIR: &str = "/run/cha-input";
const BROWSER_SECCOMP: &str = include_str!("../profiles/seccomp-browser.json");
/// The AppArmor profile for the `steam` security profile, which the owner
/// loads on the node (`deploy/node/host/apparmor/cha-sandbox`).
pub const SANDBOX_APPARMOR: &str = "cha-sandbox";
const APP_HOME: &str = "/home/cha";
/// Copying a home from its old volume is as long as the home is big.
const COPY_TIMEOUT: Duration = Duration::from_secs(3600);
/// Looking at a shared directory on a NAS: a hard NFS mount makes a system
/// call wait for as long as the server is away.
const EXTERNAL_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the agent looks for per-user directories that came unmounted.
const OVERLAY_CHECK: Duration = Duration::from_secs(30);
/// Each environment's streamer: HTTP (localhost), WebRTC and WebTransport.
const PORTS_PER_ENVIRONMENT: u16 = 3;
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
    /// Where each environment that is watched stands now: a warning, or none.
    fn warnings_now(&self) -> Vec<Warning> {
        Vec::new()
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
    /// Where the logs of environments that died are kept (the agent's state
    /// directory's `logs`); `None` keeps none.
    pub log_dir: Option<PathBuf>,
}

pub struct DockerRuntime {
    docker: Docker,
    config: DockerConfig,
    /// The render node's group, so the app's user can open it.
    render_gid: Option<u32>,
    data: DataRoot,
    state: Mutex<State>,
    exits: broadcast::Sender<Exit>,
    progress: broadcast::Sender<Progress>,
    warnings: broadcast::Sender<Warning>,
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
    /// Environments being stopped on purpose (their containers' deaths aren't news).
    stopping: HashSet<String>,
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
        config.nvidia_wine_dir = Self::settle_nvidia_wine_dir(&docker, &config).await;
        let render_gid = render_gid(&config.render_node);
        if render_gid.is_none() {
            warn!(render_node = %config.render_node, "can't read the render node's group; apps may fall back to software rendering");
        }
        let (exits, _) = broadcast::channel(64);
        let (progress, _) = broadcast::channel(64);
        let (warnings, _) = broadcast::channel(64);
        let data = DataRoot::new(config.data_root.clone(), APP_UID, APP_UID)?;
        let runtime = Arc::new(Self {
            docker,
            config,
            render_gid,
            data,
            state: Mutex::default(),
            exits,
            progress,
            warnings,
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
        for container in self.docker.list(LABEL_ENV).await? {
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
                        container.labels.get(LABEL_TEMPLATE),
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
                state.ports.insert(id, port);
            }
        }
        for id in dead {
            info!(%id, "cleaning up an environment that died while the agent was away");
            self.state.lock().expect("state lock").ports.remove(&id);
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
            let state = self.state.lock().expect("state lock");
            if state.stopping.contains(&id) || !state.ports.contains_key(&id) {
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
        check_storage(&spec)?;
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
        for image in [&self.config.streamer_image, &spec.image] {
            self.ensure_image(image).await?;
        }
        let port = self.allocate(&spec.id)?;
        let started = async {
            self.claim_home(&spec)?;
            self.prepare_storage(&mut spec).await?;
            self.create_containers(&spec, port).await
        }
        .await;
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
                    state.homes.remove(&spec.id);
                }
                let _ = self.remove(&spec.id).await;
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

    async fn ensure_image(&self, image: &str) -> Result<()> {
        if self.docker.image_exists(image).await? {
            return Ok(());
        }
        info!(%image, "pulling");
        self.docker.pull(image).await.with_context(|| {
            format!("{image} isn't on this node and couldn't be pulled (build it with `docker compose -f images/compose.yaml build`)")
        })
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
            bail!("environment {other} already uses this home on this node");
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
            if let Some(dir) = self.config.shared_dirs.get(&spec.template).cloned() {
                if let Err(problem) = check_external(dir.clone(), per_user).await {
                    warn!(
                        id = %spec.id, template = %spec.template, dir = %dir.display(),
                        "starting without the shared directory: {problem}"
                    );
                    if let Some(storage) = spec.storage.as_mut() {
                        storage.shared = None;
                    }
                }
            } else {
                let template = spec.template.clone();
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
        let _ = self.progress.send(Progress {
            id: spec.id.clone(),
            detail: "Moving your files to the new storage (this happens once)".into(),
        });
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
        let streamer = self
            .docker
            .create(
                &container_name(&spec.id, "streamer"),
                &self.streamer_config(spec, port),
            )
            .await?;
        self.docker.start(&streamer).await?;
        let hidraw = self.streamer_hidraw(spec, port).await?;
        let app = self
            .docker
            .create(
                &container_name(&spec.id, "app"),
                &self.app_config(spec, port, &hidraw),
            )
            .await?;
        self.docker.start(&app).await
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
        if let Some((owner, template)) = home_key(spec) {
            labels[LABEL_OWNER] = json!(owner);
            labels[LABEL_TEMPLATE] = json!(template);
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
            let (source, target) = self.shared_location(&spec.template);
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
        let (_, target) = self.shared_location(&spec.template);
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

    fn gpu(&self) -> Value {
        json!([{ "Driver": "cdi", "DeviceIDs": [self.config.gpu_device] }])
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
            "--render-node",
            &self.config.render_node,
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
        let mut devices = Vec::new();
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
        json!({
            "Image": self.config.streamer_image,
            "Cmd": cmd,
            "Env": [format!("XDG_RUNTIME_DIR={RUNTIME_DIR}"), "RUST_LOG=info,smithay=warn,str0m=warn"],
            "Labels": self.labels(&spec.id, "streamer", port),
            "HostConfig": {
                "NetworkMode": "host",
                "Mounts": self.mounts(&spec.id, false),
                "DeviceRequests": self.gpu(),
                "Devices": devices,
                "RestartPolicy": { "Name": "no" },
                "Init": true,
            },
        })
    }

    /// The app's container. `hidraw` are the streamer's hidraw nodes
    /// ([`Self::streamer_hidraw`]): each is mounted from the input volume and
    /// allowed in the app's device cgroup.
    fn app_config(&self, spec: &EnvironmentSpec, port: u16, hidraw: &[Hidraw]) -> Value {
        let mut security = vec!["no-new-privileges".to_string()];
        if matches!(
            spec.security,
            SecurityProfile::Browser | SecurityProfile::Steam
        ) {
            security.push(format!("seccomp={}", compact(BROWSER_SECCOMP)));
        }
        if spec.security == SecurityProfile::Steam {
            security.push(format!("apparmor={SANDBOX_APPARMOR}"));
        }
        let groups: Vec<String> = self.render_gid.iter().map(|g| g.to_string()).collect();
        let mut mounts = self.mounts(&spec.id, true);
        let list = mounts.as_array_mut().expect("mounts are a list");
        list.extend(self.storage_mounts(spec));
        list.extend(hidraw.iter().map(|node| hidraw_mount(&spec.id, node)));
        // Proton copies nvngx.dll from here into its prefixes; CDI doesn't bring it.
        if let Some(dir) = &self.config.nvidia_wine_dir {
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
        // Proton's esync wants many descriptors.
        let ulimits = if spec.security == SecurityProfile::Steam {
            json!([{ "Name": "nofile", "Soft": 524288, "Hard": 524288 }])
        } else {
            json!([])
        };
        let mut env = vec![
            format!("XDG_RUNTIME_DIR={RUNTIME_DIR}"),
            "WAYLAND_DISPLAY=wayland-0".to_string(),
            format!("CHA_WIDTH={}", spec.width),
            format!("CHA_HEIGHT={}", spec.height),
            format!("CHA_REFRESH={}", spec.fps),
        ];
        // Where the app finds what its template shares, when it has any.
        if spec.storage.as_ref().is_some_and(|s| s.shared.is_some()) {
            env.push(format!(
                "CHA_SHARED_DIR={}",
                self.shared_location(&spec.template).1
            ));
        }
        // What it has to keep mounted of its own, for apps that check.
        let overlays = self.per_user_overlays(spec);
        if !overlays.is_empty() {
            let targets: Vec<&str> = overlays.iter().map(|o| o.target.as_str()).collect();
            env.push(format!("CHA_PER_USER_DIRS={}", targets.join(":")));
        }
        json!({
            "Image": spec.image,
            "User": format!("{APP_UID}:{APP_UID}"),
            "Env": env,
            "Labels": self.app_labels(spec, port),
            "HostConfig": {
                "Mounts": mounts,
                "Ulimits": ulimits,
                // Input devices: the gamepads' nodes, the only ones it has.
                "DeviceCgroupRules": self.device_cgroup_rules(hidraw),
                "DeviceRequests": self.gpu(),
                "GroupAdd": groups,
                "CapDrop": ["ALL"],
                "SecurityOpt": security,
                "ShmSize": u64::from(spec.shm_mb) * 1024 * 1024,
                "RestartPolicy": { "Name": "no" },
                "Init": true,
            },
        })
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
        state.homes.remove(id);
        state.overlays.remove(id);
        result
    }

    /// Removes an environment's containers (app first) and its volumes: the
    /// runtime directory and the gamepads'. Its home volume, if it has one, is
    /// the user's and stays (`docker volume rm` it to start over).
    async fn remove(&self, id: &str) -> Result<()> {
        let containers = self.docker.list(&format!("{LABEL_ENV}={id}")).await?;
        let mut ordered: Vec<_> = containers.iter().collect();
        ordered.sort_by_key(|c| c.labels.get(LABEL_ROLE).map(String::as_str) != Some("app"));
        for container in ordered {
            self.docker.remove(&container.id, 5).await?;
        }
        for kind in ["input", "udev"] {
            self.docker
                .remove_volume(&format!("{}-{kind}", volume_name(id)))
                .await?;
        }
        self.docker.remove_volume(&volume_name(id)).await
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
    fn start(&self, spec: EnvironmentSpec) -> BoxFuture<'_, Result<StreamerEndpoint>> {
        Box::pin(self.start_environment(spec))
    }

    fn stop(&self, id: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.stop_environment(&id).await })
    }

    fn running(&self) -> BoxFuture<'_, Result<Vec<String>>> {
        Box::pin(self.running_ids())
    }

    fn connect(&self, request: Connect) -> BoxFuture<'_, Result<Value>> {
        Box::pin(self.connect_environment(request))
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

/// The (user, template) whose directory under the data root is the app's home.
fn home_key(spec: &EnvironmentSpec) -> Option<(String, String)> {
    spec.storage.as_ref()?.home.as_ref()?;
    Some((spec.owner.clone(), spec.template.clone()))
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
        .filter_map(|t| t["image"].as_str().map(str::to_string))
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
        DockerRuntime {
            docker,
            config: DockerConfig {
                streamer_image: "cha/streamer:dev".into(),
                render_node: "/dev/dri/renderD128".into(),
                gpu_device: "nvidia.com/gpu=all".into(),
                uinput: Some("/dev/uinput".into()),
                uhid: Some("/dev/uhid".into()),
                public_address: None,
                port_base: 47000,
                max_environments: 2,
                data_root: root.clone(),
                shared_dirs,
                nvidia_wine_dir: None,
                log_dir: None,
            },
            render_gid: Some(992),
            data: DataRoot::new(root, owner.0, owner.1).unwrap(),
            state: Mutex::default(),
            exits,
            progress,
            warnings,
        }
    }

    fn spec(security: SecurityProfile) -> EnvironmentSpec {
        EnvironmentSpec {
            id: "e1".into(),
            image: "cha/env-chrome:dev".into(),
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
    fn steam_gets_the_sandbox_profile_and_its_home() {
        let rt = runtime();
        let app = rt.app_config(&steam_spec(), 47000, &[]);
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
        let chrome = rt.app_config(&spec(SecurityProfile::Browser), 47000, &[]);
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
        assert_eq!(rt.allocate("a").unwrap(), 47000);
        assert_eq!(rt.allocate("b").unwrap(), 47003);
        assert!(rt.allocate("c").is_err());
        rt.state.lock().unwrap().ports.remove("a");
        assert_eq!(rt.allocate("c").unwrap(), 47000);
    }

    #[test]
    fn apps_are_confined() {
        let rt = runtime();
        let app = rt.app_config(&spec(SecurityProfile::Standard), 47000, &[]);
        assert_eq!(app["User"], "1000:1000");
        assert_eq!(app["HostConfig"]["CapDrop"], json!(["ALL"]));
        assert_eq!(
            app["HostConfig"]["SecurityOpt"],
            json!(["no-new-privileges"])
        );
        assert_eq!(app["HostConfig"]["GroupAdd"], json!(["992"]));
        assert_eq!(app["HostConfig"]["ShmSize"], 1024 * 1024 * 1024);
        assert!(app["HostConfig"].get("NetworkMode").is_none());

        let browser = rt.app_config(&spec(SecurityProfile::Browser), 47000, &[]);
        let opts = browser["HostConfig"]["SecurityOpt"].as_array().unwrap();
        let seccomp = opts[1].as_str().unwrap().strip_prefix("seccomp=").unwrap();
        let profile: Value = serde_json::from_str(seccomp).unwrap();
        assert_eq!(profile["defaultAction"], "SCMP_ACT_ERRNO");
        assert!(seccomp.contains("\"unshare\""));
    }

    #[test]
    fn streamers_get_their_ports_and_the_gpu() {
        let rt = runtime();
        let s = rt.streamer_config(&spec(SecurityProfile::Standard), 47002);
        let cmd: Vec<&str> = s["Cmd"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let arg = |name: &str| cmd[cmd.iter().position(|a| *a == name).unwrap() + 1];
        assert_eq!(arg("--http-port"), "47002");
        assert_eq!(arg("--webrtc-port"), "47003");
        assert_eq!(arg("--wt-port"), "47004");
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
        assert_eq!(s["Labels"]["sh.cha.http-port"], "47002");
    }

    #[test]
    fn knows_the_catalog_images() {
        assert!(catalog_images().contains(&"cha/env-chrome:dev".to_string()));
    }

    #[test]
    fn gamepads_reach_the_app_read_only() {
        let rt = runtime();
        let s = rt.streamer_config(&spec(SecurityProfile::Standard), 47000);
        assert_eq!(
            s["HostConfig"]["Devices"][0]["PathInContainer"],
            "/dev/uinput"
        );
        assert!(s["Cmd"].as_array().unwrap().contains(&json!("--input-dir")));
        let app = rt.app_config(&spec(SecurityProfile::Standard), 47000, &[]);
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
        let s = rt.streamer_config(&spec(SecurityProfile::Standard), 47000);
        assert_eq!(s["HostConfig"]["Devices"], json!([]));
        assert!(!s["Cmd"].as_array().unwrap().contains(&json!("--input-dir")));
        let app = rt.app_config(&spec(SecurityProfile::Standard), 47000, &[]);
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
            rt.streamer_config(spec, 47000)["Cmd"]
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
        let s = rt.streamer_config(&pad_spec(Some(GamepadKind::Steam)), 47000);
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
        let s = only.streamer_config(&pad_spec(Some(GamepadKind::Dualsense)), 47000);
        let cmd = s["Cmd"].as_array().unwrap();
        assert!(cmd.contains(&json!("--input-dir")) && !cmd.contains(&json!("--uinput")));
        assert_eq!(
            s["HostConfig"]["Devices"][0]["PathOnHost"],
            "/dev/misc/uhid"
        );

        // Without uhid the streamer is told the kind and falls back itself.
        let mut none = runtime();
        none.config.uhid = None;
        let s = none.streamer_config(&pad_spec(Some(GamepadKind::Dualsense)), 47000);
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
        let app = rt.app_config(&pad_spec(Some(GamepadKind::Dualsense)), 47000, &nodes);
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
        let plain = rt.app_config(&pad_spec(None), 47000, &[]);
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
        let streamer = rt.streamer_config(&steam_spec(), 47000);
        let mounts = streamer["HostConfig"]["Mounts"].as_array().unwrap();
        assert!(mounts.iter().all(|m| m["Target"] != "/home/cha"));
        assert!(mounts.iter().all(|m| m.get("VolumeOptions").is_none()));
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
            ("GET", p) if p.starts_with("/images/") => Json(json!({})).into_response(),
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
            ("GET", "/containers/json") => Json(json!(*containers)).into_response(),
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
                containers.push(json!({
                    "Id": name, "State": "created", "Labels": body["Labels"], "Config": body,
                    "Mounts": mounts,
                }));
                (StatusCode::CREATED, Json(json!({ "Id": name }))).into_response()
            }
            ("POST", p) if p.ends_with("/start") => {
                let id = container(p);
                for c in containers.iter_mut().filter(|c| c["Id"] == id) {
                    c["State"] = json!("running");
                }
                StatusCode::NO_CONTENT.into_response()
            }
            ("POST", p) if p.ends_with("/stop") => StatusCode::NO_CONTENT.into_response(),
            ("DELETE", p) if p.starts_with("/containers/") => {
                let id = container(p);
                containers.retain(|c| c["Id"] != id);
                StatusCode::NO_CONTENT.into_response()
            }
            ("DELETE", p) if p.starts_with("/volumes/") => StatusCode::NO_CONTENT.into_response(),
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
        assert!(target(&rt.app_config(&steam_spec(), 47000, &[])).is_none());

        rt.config.nvidia_wine_dir = Some(WINE_DIR.into());
        let mount = target(&rt.app_config(&steam_spec(), 47000, &[])).expect("mounted");
        assert_eq!(mount["Type"], "bind");
        assert_eq!(mount["Source"], WINE_DIR);
        assert_eq!(mount["ReadOnly"], true);
        assert_eq!(mount["BindOptions"]["CreateMountpoint"], false);
        // Streamers don't run Wine.
        assert!(target(&rt.streamer_config(&steam_spec(), 47000)).is_none());
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
                let app = rt.app_config(&spec, 47000, &[]);
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
        let app = rt.app_config(&spec, 47000, &[]);
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
        let streamer = rt.streamer_config(&spec, 47000);
        let mounts = streamer["HostConfig"]["Mounts"].as_array().unwrap();
        assert!(mounts.iter().all(|m| m["Type"] == "volume"));
        assert!(
            mounts
                .iter()
                .all(|m| !m["Target"].as_str().unwrap().contains("steam"))
        );
        // The app is labelled with whose home it has.
        let app = rt.app_config(&spec, 47000, &[]);
        assert_eq!(app["Labels"]["sh.cha.owner"], USER);
        assert_eq!(app["Labels"]["sh.cha.template"], "steam");
        let plain = rt.app_config(&app_data_spec("e1", "steam", None), 47000, &[]);
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
                c["Labels"]["sh.cha.http-port"] = json!("47000");
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
            rt.app_config(spec, 47000, &[])["Env"]
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
                c["Labels"]["sh.cha.http-port"] = json!("47000");
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
        let mut config = rt.app_config(spec, 47000, &[]);
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
}
