//! Environments on this node (plan §4.2). Each one is:
//! - a **streamer** container (`cha-streamer`): our compositor, NVENC and
//!   WebRTC, on the host network so its WebRTC port is the node's;
//! - an **app** container from the template's image: uid 1000 plus the render
//!   node's group, no capabilities, no privilege gain, Docker's seccomp profile
//!   (or the `browser` one, which lets browser sandboxes create namespaces);
//! - a volume both mount at `/run/cha`, holding the Wayland and sound
//!   sockets;
//! - with gamepads (`/dev/uinput` on the host): the streamer makes virtual
//!   pads and shares their device nodes and udev entries through two more
//!   volumes, the app's `/dev/input` and `/run/udev` (read-only); the app may
//!   open input devices (major 13), and only these exist in its `/dev/input`.
//! - for templates marked persistent (Steam): a **home volume** the portal
//!   names per user and template, mounted at the app's `/home/cha`. Unlike the
//!   volumes above it belongs to the user, not the environment: stopping one
//!   never removes it.
//!
//! The agent is never in the media path: containers outlive agent restarts,
//! and the agent finds them again by label.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use cha_wire::{EnvironmentSpec, SecurityProfile, StreamerEndpoint, is_home_volume_name};
use futures_util::future::BoxFuture;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::docker::{ContainerEvent, Docker, encode};

const LABEL_ENV: &str = "sh.cha.env";
const LABEL_ROLE: &str = "sh.cha.role";
const LABEL_HTTP_PORT: &str = "sh.cha.http-port";
/// On home volumes (`docker volume ls --filter label=sh.cha.home`): `1`, and
/// whose (`sh.cha.owner`) and for which template (`sh.cha.template`).
const LABEL_HOME: &str = "sh.cha.home";
const LABEL_OWNER: &str = "sh.cha.owner";
const LABEL_TEMPLATE: &str = "sh.cha.template";
const APP_UID: u32 = 1000;
const RUNTIME_DIR: &str = "/run/cha";
/// Where the streamer puts gamepad nodes (`dev/`) and udev entries (`udev/`).
const INPUT_DIR: &str = "/run/cha-input";
const BROWSER_SECCOMP: &str = include_str!("../profiles/seccomp-browser.json");
/// The AppArmor profile for the `steam` security profile, which the owner
/// loads on the node (`deploy/node/host/apparmor/cha-sandbox`).
pub const SANDBOX_APPARMOR: &str = "cha-sandbox";
const APP_HOME: &str = "/home/cha";
/// Each environment's streamer: HTTP (localhost), WebRTC and WebTransport.
const PORTS_PER_ENVIRONMENT: u16 = 3;

/// An environment that stopped on its own.
#[derive(Debug, Clone)]
pub struct Exit {
    pub id: String,
    pub detail: String,
    pub failed: bool,
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
    /// The router's public address, when it forwards the streamers' UDP ports
    /// (`port_base + 1`, `+ 3`, …) to this node.
    pub public_address: Option<String>,
    /// Streamers listen on `port_base + 2n` (HTTP) and `+ 1` (WebRTC).
    pub port_base: u16,
    pub max_environments: u16,
}

pub struct DockerRuntime {
    docker: Docker,
    config: DockerConfig,
    /// The render node's group, so the app's user can open it.
    render_gid: Option<u32>,
    state: Mutex<State>,
    exits: broadcast::Sender<Exit>,
}

#[derive(Default)]
struct State {
    /// Environment id → its streamer's HTTP port.
    ports: BTreeMap<String, u16>,
    /// Environments being stopped on purpose (their containers' deaths aren't news).
    stopping: HashSet<String>,
}

impl DockerRuntime {
    /// Connects to the engine, adopts environments left running by an earlier
    /// agent, clears out dead ones, and starts watching for exits.
    pub async fn new(docker: Docker, config: DockerConfig) -> Result<Arc<Self>> {
        docker.ping().await?;
        let render_gid = render_gid(&config.render_node);
        if render_gid.is_none() {
            warn!(render_node = %config.render_node, "can't read the render node's group; apps may fall back to software rendering");
        }
        let (exits, _) = broadcast::channel(64);
        let runtime = Arc::new(Self {
            docker,
            config,
            render_gid,
            state: Mutex::default(),
            exits,
        });
        runtime.adopt().await?;
        let watcher = Arc::clone(&runtime);
        tokio::spawn(async move { watcher.watch().await });
        Ok(runtime)
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
                self.state
                    .lock()
                    .expect("state lock")
                    .ports
                    .insert(id, port);
            }
        }
        for id in dead {
            info!(%id, "cleaning up an environment that died while the agent was away");
            self.state.lock().expect("state lock").ports.remove(&id);
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
        info!(%id, %detail, "environment ended on its own");
        if let Err(err) = self.stop_environment(&id).await {
            warn!(%id, "cleaning up: {err:#}");
        }
        let failed = role != "app" || event.exit_code.is_some_and(|c| c != 0);
        let _ = self.exits.send(Exit { id, detail, failed });
    }

    async fn start_environment(&self, spec: EnvironmentSpec) -> Result<StreamerEndpoint> {
        // The app gets only a volume that is a home: never, say, this agent's
        // own state.
        if let Some(home) = &spec.home
            && !is_home_volume_name(home)
        {
            bail!("{home:?} isn't the name of a home volume (cha-home-<user>-<template>)");
        }
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
        match self.create_containers(&spec, port).await {
            Ok(()) => {
                info!(id = %spec.id, image = %spec.image, http_port = port, "environment started");
                Ok(endpoint(port))
            }
            Err(err) => {
                self.state
                    .lock()
                    .expect("state lock")
                    .ports
                    .remove(&spec.id);
                let _ = self.remove(&spec.id).await;
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

    async fn create_containers(&self, spec: &EnvironmentSpec, port: u16) -> Result<()> {
        let streamer = self
            .docker
            .create(
                &container_name(&spec.id, "streamer"),
                &self.streamer_config(spec, port),
            )
            .await?;
        self.docker.start(&streamer).await?;
        let app = self
            .docker
            .create(
                &container_name(&spec.id, "app"),
                &self.app_config(spec, port),
            )
            .await?;
        self.docker.start(&app).await
    }

    fn labels(&self, id: &str, role: &str, port: u16) -> Value {
        json!({ LABEL_ENV: id, LABEL_ROLE: role, LABEL_HTTP_PORT: port.to_string() })
    }

    fn gpu(&self) -> Value {
        json!([{ "Driver": "cdi", "DeviceIDs": [self.config.gpu_device] }])
    }

    /// The volumes, as the streamer or the app (`app`) mounts them.
    fn mounts(&self, id: &str, app: bool) -> Value {
        let mut mounts =
            vec![json!({ "Type": "volume", "Source": volume_name(id), "Target": RUNTIME_DIR })];
        if self.config.uinput.is_some() {
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
        if let Some(uinput) = &self.config.uinput {
            cmd.extend(["--input-dir", INPUT_DIR, "--uinput", "/dev/uinput"].map(String::from));
            devices.push(json!({
                "PathOnHost": uinput,
                "PathInContainer": "/dev/uinput",
                "CgroupPermissions": "rw",
            }));
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

    fn app_config(&self, spec: &EnvironmentSpec, port: u16) -> Value {
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
        if let Some(home) = home_mount(spec) {
            mounts.as_array_mut().expect("mounts are a list").push(home);
        }
        // Proton's esync wants many descriptors.
        let ulimits = if spec.security == SecurityProfile::Steam {
            json!([{ "Name": "nofile", "Soft": 524288, "Hard": 524288 }])
        } else {
            json!([])
        };
        json!({
            "Image": spec.image,
            "User": format!("{APP_UID}:{APP_UID}"),
            "Env": [
                format!("XDG_RUNTIME_DIR={RUNTIME_DIR}"),
                "WAYLAND_DISPLAY=wayland-0",
                format!("CHA_WIDTH={}", spec.width),
                format!("CHA_HEIGHT={}", spec.height),
                format!("CHA_REFRESH={}", spec.fps),
            ],
            "Labels": self.labels(&spec.id, "app", port),
            "HostConfig": {
                "Mounts": mounts,
                "Ulimits": ulimits,
                // Input devices: the gamepads' nodes, the only ones it has.
                "DeviceCgroupRules": if self.config.uinput.is_some() { json!(["c 13:* rw"]) } else { json!([]) },
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

    use super::*;

    fn runtime() -> DockerRuntime {
        runtime_on(Docker::new("/nonexistent"))
    }

    fn runtime_on(docker: Docker) -> DockerRuntime {
        let (exits, _) = broadcast::channel(1);
        DockerRuntime {
            docker,
            config: DockerConfig {
                streamer_image: "cha/streamer:dev".into(),
                render_node: "/dev/dri/renderD128".into(),
                gpu_device: "nvidia.com/gpu=all".into(),
                uinput: Some("/dev/uinput".into()),
                public_address: None,
                port_base: 47000,
                max_environments: 2,
            },
            render_gid: Some(992),
            state: Mutex::default(),
            exits,
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
        let app = rt.app_config(&steam_spec(), 47000);
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
        let chrome = rt.app_config(&spec(SecurityProfile::Browser), 47000);
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
        let app = rt.app_config(&spec(SecurityProfile::Standard), 47000);
        assert_eq!(app["User"], "1000:1000");
        assert_eq!(app["HostConfig"]["CapDrop"], json!(["ALL"]));
        assert_eq!(
            app["HostConfig"]["SecurityOpt"],
            json!(["no-new-privileges"])
        );
        assert_eq!(app["HostConfig"]["GroupAdd"], json!(["992"]));
        assert_eq!(app["HostConfig"]["ShmSize"], 1024 * 1024 * 1024);
        assert!(app["HostConfig"].get("NetworkMode").is_none());

        let browser = rt.app_config(&spec(SecurityProfile::Browser), 47000);
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
        let app = rt.app_config(&spec(SecurityProfile::Standard), 47000);
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
        let s = rt.streamer_config(&spec(SecurityProfile::Standard), 47000);
        assert_eq!(s["HostConfig"]["Devices"], json!([]));
        let app = rt.app_config(&spec(SecurityProfile::Standard), 47000);
        assert_eq!(app["HostConfig"]["Mounts"].as_array().unwrap().len(), 1);
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
            ("GET", "/containers/json") => Json(json!(*containers)).into_response(),
            ("POST", "/containers/create") => {
                let name = uri.split_once("name=").unwrap().1.to_string();
                containers
                    .push(json!({ "Id": name, "State": "created", "Labels": body["Labels"] }));
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
}
