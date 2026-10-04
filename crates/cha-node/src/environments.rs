//! Environments on this node (plan §4.2). Each one is:
//! - a **streamer** container (`cha-streamer`): our compositor, NVENC and
//!   WebRTC, on the host network so its WebRTC port is the node's;
//! - an **app** container from the template's image: uid 1000 plus the render
//!   node's group, no capabilities, no privilege gain, Docker's seccomp profile
//!   (or the `browser` one, which lets browser sandboxes create namespaces);
//! - a volume both mount at `/run/cha`, holding the Wayland socket.
//!
//! The agent is never in the media path: containers outlive agent restarts,
//! and the agent finds them again by label.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use cha_wire::{EnvironmentSpec, SecurityProfile, StreamerEndpoint};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::docker::{ContainerEvent, Docker};

const LABEL_ENV: &str = "sh.cha.env";
const LABEL_ROLE: &str = "sh.cha.role";
const LABEL_HTTP_PORT: &str = "sh.cha.http-port";
const APP_UID: u32 = 1000;
const RUNTIME_DIR: &str = "/run/cha";
const BROWSER_SECCOMP: &str = include_str!("../profiles/seccomp-browser.json");

/// An environment that stopped on its own.
#[derive(Debug, Clone)]
pub struct Exit {
    pub id: String,
    pub detail: String,
    pub failed: bool,
}

/// What the agent needs from whatever runs environments.
pub trait Runtime: Send + Sync + 'static {
    /// Starts (or finds running) an environment.
    fn start(&self, spec: EnvironmentSpec) -> BoxFuture<'_, Result<StreamerEndpoint>>;
    /// Stops and removes an environment; unknown ids are fine.
    fn stop(&self, id: String) -> BoxFuture<'_, Result<()>>;
    /// Ids of the environments running now.
    fn running(&self) -> BoxFuture<'_, Result<Vec<String>>>;
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
            .map(|n| self.config.port_base + 2 * n)
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
                &self.streamer_config(spec, port)?,
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

    fn runtime_mount(&self, id: &str) -> Value {
        json!([{ "Type": "volume", "Source": volume_name(id), "Target": RUNTIME_DIR }])
    }

    fn streamer_config(&self, spec: &EnvironmentSpec, port: u16) -> Result<Value> {
        let token = random_token()?;
        Ok(json!({
            "Image": self.config.streamer_image,
            "Cmd": [
                "--render-node", self.config.render_node,
                "--width", spec.width.to_string(),
                "--height", spec.height.to_string(),
                "--fps", spec.fps.to_string(),
                "--app-uid", APP_UID.to_string(),
                "--http-port", port.to_string(),
                "--webrtc-port", (port + 1).to_string(),
                "--token", token,
            ],
            "Env": [format!("XDG_RUNTIME_DIR={RUNTIME_DIR}"), "RUST_LOG=info,smithay=warn,str0m=warn"],
            "Labels": self.labels(&spec.id, "streamer", port),
            "HostConfig": {
                "NetworkMode": "host",
                "Mounts": self.runtime_mount(&spec.id),
                "DeviceRequests": self.gpu(),
                "RestartPolicy": { "Name": "no" },
                "Init": true,
            },
        }))
    }

    fn app_config(&self, spec: &EnvironmentSpec, port: u16) -> Value {
        let mut security = vec!["no-new-privileges".to_string()];
        if spec.security == SecurityProfile::Browser {
            security.push(format!("seccomp={}", compact(BROWSER_SECCOMP)));
        }
        let groups: Vec<String> = self.render_gid.iter().map(|g| g.to_string()).collect();
        json!({
            "Image": spec.image,
            "User": format!("{APP_UID}:{APP_UID}"),
            "Env": [format!("XDG_RUNTIME_DIR={RUNTIME_DIR}"), "WAYLAND_DISPLAY=wayland-0"],
            "Labels": self.labels(&spec.id, "app", port),
            "HostConfig": {
                "Mounts": self.runtime_mount(&spec.id),
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

    /// Removes an environment's containers (app first) and its volume.
    async fn remove(&self, id: &str) -> Result<()> {
        let containers = self.docker.list(&format!("{LABEL_ENV}={id}")).await?;
        let mut ordered: Vec<_> = containers.iter().collect();
        ordered.sort_by_key(|c| c.labels.get(LABEL_ROLE).map(String::as_str) != Some("app"));
        for container in ordered {
            self.docker.remove(&container.id, 5).await?;
        }
        self.docker.remove_volume(&volume_name(id)).await
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

    fn exits(&self) -> broadcast::Receiver<Exit> {
        self.exits.subscribe()
    }
}

fn endpoint(http_port: u16) -> StreamerEndpoint {
    StreamerEndpoint {
        http_port,
        webrtc_port: http_port + 1,
    }
}

fn container_name(id: &str, role: &str) -> String {
    format!("cha-env-{id}-{role}")
}

fn volume_name(id: &str) -> String {
    format!("cha-env-{id}")
}

/// The render node's group id, from the device node (CDI passes it through
/// with the host's ownership).
fn render_gid(render_node: &str) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(render_node).ok().map(|m| m.gid())
}

/// The profile as one line: Docker takes it inline in `SecurityOpt`.
fn compact(json: &str) -> String {
    serde_json::from_str::<Value>(json)
        .map(|v| v.to_string())
        .unwrap_or_else(|_| json.to_string())
}

fn random_token() -> Result<String> {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("random source: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> DockerRuntime {
        let (exits, _) = broadcast::channel(1);
        DockerRuntime {
            docker: Docker::new("/nonexistent"),
            config: DockerConfig {
                streamer_image: "cha/streamer:dev".into(),
                render_node: "/dev/dri/renderD128".into(),
                gpu_device: "nvidia.com/gpu=all".into(),
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
        }
    }

    #[test]
    fn allocates_port_pairs_until_full() {
        let rt = runtime();
        assert_eq!(rt.allocate("a").unwrap(), 47000);
        assert_eq!(rt.allocate("b").unwrap(), 47002);
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
        let s = rt
            .streamer_config(&spec(SecurityProfile::Standard), 47002)
            .unwrap();
        let cmd: Vec<&str> = s["Cmd"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let arg = |name: &str| cmd[cmd.iter().position(|a| *a == name).unwrap() + 1];
        assert_eq!(arg("--http-port"), "47002");
        assert_eq!(arg("--webrtc-port"), "47003");
        assert_eq!(arg("--app-uid"), "1000");
        assert_eq!(arg("--token").len(), 24);
        assert_eq!(s["HostConfig"]["NetworkMode"], "host");
        assert_eq!(
            s["HostConfig"]["DeviceRequests"][0]["DeviceIDs"][0],
            "nvidia.com/gpu=all"
        );
        assert_eq!(s["Labels"]["sh.cha.http-port"], "47002");
    }
}
