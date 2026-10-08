//! Updating the agent from the portal (ADR 0018).
//!
//! The agent can't replace its own container: stopping it ends the process
//! doing the replacing. So it prepares (pulls the new agent and streamer
//! images, [`start_update`]) and starts a one-shot helper from the *new* image
//! (`cha-node --replace-agent`, [`replace_agent`]) that swaps the container,
//! waits for the new agent to connect and puts the old one back if it doesn't.
//! The helper can't talk to the portal; the old agent, restored, reads a note
//! the helper leaves in its state directory ([`take_result`]) and reports it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use cha_wire::{AgentUpdatability, AgentUpdateState, ToPortal};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::AGENT_VERSION;
use crate::docker::{DEFAULT_SOCKET, Docker, PullProgress, names_registry};

/// The helper's container name; only one runs at a time.
pub const HELPER_NAME: &str = "cha-node-update";
/// How long the new agent has to connect before the old one is put back.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(90);
/// How long the helper lets the old agent stop.
const STOP_GRACE_SECS: u32 = 30;
const POLL_EVERY: Duration = Duration::from_secs(2);
/// What a connected agent logs (`info!(%node_id, .., "connected")`).
const CONNECTED: &str = "connected node_id=";
/// The note the helper leaves in the agent's state directory.
const RESULT_FILE: &str = "update-result.json";
/// The compose label that records where a stack's files are.
const WORKING_DIR_LABEL: &str = "com.docker.compose.project.working_dir";
/// The agent image's repository name, and its streamer's.
const NODE_REPO_NAME: &str = "cha-node";
const STREAMER_REPO_NAME: &str = "cha-streamer";
const DEFAULT_STATE_DIR: &str = "/var/lib/cha-node";

/// `MAJOR.MINOR.PATCH`, digits only.
pub fn is_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `ghcr.io/ban-red/cha-node:0.2.0@sha256:..` → (`ghcr.io/ban-red/cha-node`,
/// `Some("0.2.0")`). A colon in the registry's host:port isn't a tag.
fn split_image(image: &str) -> (&str, Option<&str>) {
    let image = image.split_once('@').map_or(image, |(name, _)| name);
    let last = image.rfind('/').map_or(0, |i| i + 1);
    match image[last..].rfind(':') {
        Some(i) => (&image[..last + i], Some(&image[last + i + 1..])),
        None => (image, None),
    }
}

/// `image` with another tag.
pub fn retag(image: &str, tag: &str) -> String {
    format!("{}:{tag}", split_image(image).0)
}

/// The streamer image published beside this agent image, for `version`:
/// `ghcr.io/ban-red/cha-node:0.2.0` → `ghcr.io/ban-red/cha-streamer:0.2.1`.
pub fn streamer_image(agent_image: &str, version: &str) -> Option<String> {
    sibling_repo(split_image(agent_image).0).map(|repo| format!("{repo}:{version}"))
}

fn sibling_repo(node_repo: &str) -> Option<String> {
    let (parent, name) = node_repo.rsplit_once('/')?;
    (name == NODE_REPO_NAME).then(|| format!("{parent}/{STREAMER_REPO_NAME}"))
}

/// The container id in the `/var/lib/docker/containers/<64 hex>/` paths of a
/// container's `hostname`, `resolv.conf` and `hosts` mounts (`mountinfo`
/// lists them for the process's own mount namespace). `network_mode: host`
/// leaves the hostname as the machine's, so this is the way to find it.
pub fn container_id_from_mountinfo(mountinfo: &str) -> Option<String> {
    const MARK: &str = "/docker/containers/";
    mountinfo.lines().find_map(|line| {
        let rest = &line[line.find(MARK)? + MARK.len()..];
        let id = rest.get(..64)?;
        (id.bytes().all(|b| b.is_ascii_hexdigit()) && rest[64..].starts_with('/'))
            .then(|| id.to_ascii_lowercase())
    })
}

/// Whether an agent running `image` can be updated from the portal.
pub fn updatability_of(image: &str) -> AgentUpdatability {
    let reason = (!names_registry(image))
        .then(|| "a local build: update it by hand".to_string())
        .or_else(|| {
            let (_, tag) = split_image(image);
            (!tag.is_some_and(is_version))
                .then(|| "its image isn't a numbered release: update it by hand".to_string())
        });
    AgentUpdatability {
        image: image.to_string(),
        updatable: reason.is_none(),
        reason,
    }
}

/// This agent's container: its id and what the engine says of it. `None`
/// when it isn't in a container the engine knows.
async fn own_container(docker: &Docker) -> Option<(String, Value)> {
    let mountinfo = tokio::fs::read_to_string("/proc/self/mountinfo")
        .await
        .ok()?;
    let id = container_id_from_mountinfo(&mountinfo)?;
    let inspect = docker.inspect_container(&id).await.ok()?;
    Some((id, inspect))
}

fn image_of(inspect: &Value) -> &str {
    inspect["Config"]["Image"].as_str().unwrap_or_default()
}

/// What the inventory says about updating: from the container this agent runs
/// in, if it has the engine.
pub async fn updatability(docker: Option<&Docker>) -> AgentUpdatability {
    let no = |reason: &str| AgentUpdatability {
        image: String::new(),
        updatable: false,
        reason: Some(reason.to_string()),
    };
    let Some(docker) = docker else {
        return no("the agent has no Docker socket");
    };
    match own_container(docker).await {
        Some((_, inspect)) => updatability_of(image_of(&inspect)),
        None => no("not running in a container the agent can see"),
    }
}

/// The releases a new image brings: the repositories whose tags follow it.
struct Release<'a> {
    version: &'a str,
    node_repo: &'a str,
    streamer_repo: Option<String>,
}

impl<'a> Release<'a> {
    /// For `image`, whose tag is the version.
    fn of(image: &'a str) -> Result<Self> {
        let (repo, tag) = split_image(image);
        let version = tag
            .filter(|t| is_version(t))
            .with_context(|| format!("{image} isn't a numbered release (MAJOR.MINOR.PATCH tag)"))?;
        Ok(Self {
            version,
            node_repo: repo,
            streamer_repo: sibling_repo(repo),
        })
    }

    /// The new value of a deployment setting (`.env` line, container
    /// variable), `None` when it stays: the release variables follow it; an
    /// image setting only when it names the same repository.
    fn setting(&self, key: &str, value: &str) -> Option<String> {
        if value.is_empty() {
            return None;
        }
        let new = match key {
            "CHA_VERSION" | "CHA_IMAGE_TAG" => self.version.to_string(),
            "CHA_NODE_IMAGE" if split_image(value).0 == self.node_repo => {
                retag(value, self.version)
            }
            "CHA_STREAMER_IMAGE" if Some(split_image(value).0) == self.streamer_repo.as_deref() => {
                retag(value, self.version)
            }
            _ => return None,
        };
        (new != value).then_some(new)
    }
}

/// `text` (a compose `.env`) with the release settings moved to the new
/// release, and what changed (`CHA_NODE_IMAGE: a → b`). Every other line,
/// comments and line endings included, stays byte for byte.
fn rewrite_env_file(text: &str, release: &Release) -> (String, Vec<String>) {
    let mut changes = Vec::new();
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        match rewrite_env_line(line, release) {
            Some((new, change)) => {
                out.push_str(&new);
                changes.push(change);
            }
            None => out.push_str(line),
        }
    }
    (out, changes)
}

fn rewrite_env_line(line: &str, release: &Release) -> Option<(String, String)> {
    let (key, rest) = line.split_once('=')?;
    let value_start = rest;
    // The value is quoted, or runs to the first space (a comment may follow).
    let (open, value, tail) = match value_start.chars().next()? {
        q @ ('"' | '\'') => {
            let inner = &value_start[1..];
            let end = inner.find(q)?;
            (&value_start[..1], &inner[..end], &inner[end..])
        }
        _ => {
            let end = value_start
                .find(|c: char| c.is_whitespace())
                .unwrap_or(value_start.len());
            ("", &value_start[..end], &value_start[end..])
        }
    };
    let new = release.setting(key, value)?;
    Some((
        format!("{key}={open}{new}{tail}"),
        format!("{key}: {value} -> {new}"),
    ))
}

/// Rewrites `<dir>/.env` in place (the inode, owner and mode stay). Returns
/// what changed; nothing if there is no such file.
fn update_env_file(dir: &Path, release: &Release) -> Result<Vec<String>> {
    let path = dir.join(".env");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let (new, changes) = rewrite_env_file(&text, release);
    if !changes.is_empty() {
        std::fs::write(&path, new).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(changes)
}

/// The body that creates a copy of the container `inspect` describes, running
/// `image`: its configuration, host configuration and networks, with the
/// release variables in its environment moved to the new release. What the
/// engine made up for the old container (its hostname and network alias,
/// which are its id; the MAC address and addresses) is left for the new one.
fn create_body(inspect: &Value, image: &str) -> Result<Value> {
    let release = Release::of(image)?;
    let mut body = inspect["Config"].clone();
    let config = body
        .as_object_mut()
        .context("the container has no configuration")?;
    config.insert("Image".into(), json!(image));
    let id = inspect["Id"].as_str().unwrap_or_default();
    let short = id.get(..12).unwrap_or(id);
    if config["Hostname"].as_str() == Some(short) {
        config.remove("Hostname");
    }
    if let Some(env) = config.get_mut("Env").and_then(Value::as_array_mut) {
        for entry in env {
            let Some((key, value)) = entry.as_str().and_then(|e| e.split_once('=')) else {
                continue;
            };
            if let Some(new) = release.setting(key, value) {
                *entry = json!(format!("{key}={new}"));
            }
        }
    }
    config.insert("HostConfig".into(), inspect["HostConfig"].clone());
    // The built-in networks are chosen by the host configuration's mode.
    let mut endpoints = serde_json::Map::new();
    for (name, net) in inspect["NetworkSettings"]["Networks"]
        .as_object()
        .into_iter()
        .flatten()
    {
        if matches!(name.as_str(), "host" | "bridge" | "none") {
            continue;
        }
        let mut endpoint = serde_json::Map::new();
        for key in ["IPAMConfig", "Links", "DriverOpts"] {
            if !net[key].is_null() {
                endpoint.insert(key.into(), net[key].clone());
            }
        }
        let aliases: Vec<&Value> = net["Aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| a.as_str() != Some(short))
            .collect();
        if !aliases.is_empty() {
            endpoint.insert("Aliases".into(), json!(aliases));
        }
        endpoints.insert(name.clone(), Value::Object(endpoint));
    }
    if !endpoints.is_empty() {
        config.insert(
            "NetworkingConfig".into(),
            json!({ "EndpointsConfig": endpoints }),
        );
    }
    Ok(body)
}

/// Removes terminal colors from log text (the agent's tracing output has
/// them even without a terminal), so a line can be matched.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether an agent's log shows it connected to the portal.
fn shows_connected(log: &str) -> bool {
    strip_ansi(log).contains(CONNECTED)
}

fn failed(detail: impl Into<String>) -> ToPortal {
    ToPortal::AgentUpdate {
        state: AgentUpdateState::Failed,
        detail: Some(detail.into()),
        done: None,
        total: None,
    }
}

fn say(state: AgentUpdateState, detail: &str) -> ToPortal {
    ToPortal::AgentUpdate {
        state,
        detail: Some(detail.to_string()),
        done: None,
        total: None,
    }
}

/// The failure to tell the portal when `err` stops [`start_update`].
pub fn failure(err: &anyhow::Error) -> ToPortal {
    failed(format!("{err:#}"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn megabytes(bytes: u64) -> u64 {
    bytes >> 20
}

/// Pulls `image` unless the engine has it, reporting how it goes as
/// `pulling` (`base` bytes were downloaded already, for the images before
/// it). Returns the bytes this one took.
async fn pull_missing(
    docker: &Docker,
    image: &str,
    what: &str,
    base: u64,
    report: &mut (dyn FnMut(ToPortal) + Send),
) -> Result<u64> {
    if docker.image_exists(image).await? {
        info!(%image, "the engine has the image already");
        return Ok(0);
    }
    if !names_registry(image) {
        bail!("{image} isn't on this node and names no registry to pull it from");
    }
    info!(%image, "pulling the {what}");
    let pulled = docker
        .pull_with(image, |p: PullProgress| {
            report(ToPortal::AgentUpdate {
                state: AgentUpdateState::Pulling,
                detail: Some(format!(
                    "Downloading the {what} ({} of {} MB)",
                    megabytes(p.done),
                    megabytes(p.total)
                )),
                done: Some(base + p.done),
                total: Some(base + p.total),
            })
        })
        .await
        .with_context(|| format!("pulling {image}"))?;
    Ok(pulled.total)
}

/// The agent's side of an update to `version`: checks it can, pulls the new
/// agent image (and its streamer's) unless the engine has them, then starts
/// the helper that swaps the container. Returns once the helper runs; the
/// agent is stopped by it soon after, so there is nothing more to do.
/// `socket` is the engine's socket inside this container. Progress goes to
/// `report`; an error means nothing changed and the agent keeps running.
pub async fn start_update(
    docker: &Docker,
    socket: &Path,
    version: &str,
    report: &mut (dyn FnMut(ToPortal) + Send),
) -> Result<()> {
    if !is_version(version) {
        bail!("{version:?} isn't a release (MAJOR.MINOR.PATCH)");
    }
    if version == AGENT_VERSION {
        bail!("this agent is already {AGENT_VERSION}");
    }
    let (id, inspect) = own_container(docker)
        .await
        .context("this agent isn't running in a container it can see")?;
    let current = image_of(&inspect);
    let can = updatability_of(current);
    if let Some(reason) = can.reason {
        bail!("{reason}");
    }
    let image = retag(current, version);

    let mut pulled = pull_missing(docker, &image, "agent", 0, report).await?;
    // The streamer the new agent will use: the configured one if that names a
    // registry, else the release published beside the agent. It is required
    // only in the first case; elsewhere the node has its own.
    let configured = inspect["Config"]["Env"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(|e| e.strip_prefix("CHA_STREAMER_IMAGE="));
    let release = Release::of(&image)?;
    let streamer = configured
        .map(|c| release.setting("CHA_STREAMER_IMAGE", c).unwrap_or(c.into()))
        .filter(|c| names_registry(c));
    let required = streamer.is_some();
    if let Some(streamer) = streamer.or_else(|| streamer_image(&image, version)) {
        match pull_missing(docker, &streamer, "streamer", pulled, report).await {
            Ok(bytes) => pulled += bytes,
            Err(err) if required => return Err(err),
            Err(err) => warn!("not pulling the release's streamer: {err:#}"),
        }
    }
    let _ = pulled;

    report(say(AgentUpdateState::Swapping, "Restarting the agent"));
    let env_dir = inspect["Config"]["Labels"][WORKING_DIR_LABEL]
        .as_str()
        .filter(|d| Path::new(d).is_absolute())
        .map(PathBuf::from);
    let socket_source = inspect["Mounts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["Destination"].as_str() == Some(&socket.to_string_lossy()))
        .and_then(|m| m["Source"].as_str())
        .unwrap_or(DEFAULT_SOCKET);
    start_helper(docker, &id, &image, env_dir.as_deref(), socket_source).await
}

/// Creates and starts the helper, with the working directory bound in when
/// the host has it.
async fn start_helper(
    docker: &Docker,
    agent_id: &str,
    image: &str,
    env_dir: Option<&Path>,
    socket_source: &str,
) -> Result<()> {
    if let Ok(existing) = docker.inspect_container(HELPER_NAME).await {
        if existing["State"]["Running"].as_bool() == Some(true) {
            bail!("an update is already running ({HELPER_NAME})");
        }
        docker.remove(HELPER_NAME, 0).await?;
    }
    let config = |dir: Option<&Path>| {
        let mut cmd = vec![
            "--replace-agent".to_string(),
            agent_id.to_string(),
            "--image".to_string(),
            image.to_string(),
        ];
        let mut mounts = vec![json!({
            "Type": "bind",
            "Source": socket_source,
            "Target": DEFAULT_SOCKET,
        })];
        if let Some(dir) = dir {
            let dir = dir.to_string_lossy();
            cmd.extend(["--env-dir".to_string(), dir.to_string()]);
            // Never make the directory: it is there or the file isn't ours to change.
            mounts.push(json!({
                "Type": "bind",
                "Source": dir,
                "Target": dir,
                "BindOptions": { "CreateMountpoint": false },
            }));
        }
        json!({
            "Image": image,
            "Entrypoint": ["cha-node"],
            "Cmd": cmd,
            "HostConfig": {
                "AutoRemove": true,
                "NetworkMode": "host",
                "Mounts": mounts,
            },
        })
    };
    let id = match docker.create(HELPER_NAME, &config(env_dir)).await {
        Ok(id) => id,
        Err(err) if env_dir.is_some() && format!("{err:#}").contains("bind source path") => {
            warn!("the compose directory isn't on the host; the .env isn't updated");
            docker.create(HELPER_NAME, &config(None)).await?
        }
        Err(err) => return Err(err).context("creating the update helper"),
    };
    if let Err(err) = docker.start(&id).await {
        let _ = docker.remove(&id, 0).await;
        return Err(err).context("starting the update helper");
    }
    info!(%id, %image, "started the update helper");
    Ok(())
}

/// Where the state directory is in the container `inspect` describes.
fn state_dir_of(inspect: &Value) -> String {
    inspect["Config"]["Env"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(|e| e.strip_prefix("CHA_NODE_STATE="))
        .filter(|d| !d.is_empty())
        .unwrap_or(DEFAULT_STATE_DIR)
        .to_string()
}

/// Leaves the portal's report in the agent container's state directory, for
/// the agent in it to send ([`take_result`]). Best effort: the helper's own
/// log is the record.
async fn leave_result(docker: &Docker, container: &str, state: AgentUpdateState, detail: &str) {
    let Ok(inspect) = docker.inspect_container(container).await else {
        return;
    };
    let dir = state_dir_of(&inspect);
    let note = json!({ "state": state, "detail": detail }).to_string();
    let script = format!("printf %s \"$1\" > '{dir}/{RESULT_FILE}'");
    if let Err(err) = docker
        .exec(container, &["sh", "-c", &script, "sh", &note])
        .await
    {
        warn!("couldn't leave a note for the agent: {err:#}");
    }
}

/// What the helper left for the agent to report, once; removes the note.
pub fn take_result(state_dir: &Path) -> Option<ToPortal> {
    let path = state_dir.join(RESULT_FILE);
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let note: Value = serde_json::from_str(&text).ok()?;
    let state = serde_json::from_value(note["state"].clone()).ok()?;
    Some(ToPortal::AgentUpdate {
        state,
        detail: note["detail"].as_str().map(str::to_string),
        done: None,
        total: None,
    })
}

/// Waits until `container`'s log shows it connected to the portal, or fails
/// when it exits or `timeout` passes.
async fn wait_connected(
    docker: &Docker,
    container: &str,
    since: u64,
    timeout: Duration,
) -> Result<()> {
    let started = tokio::time::Instant::now();
    loop {
        if let Ok(log) = docker.logs_since(container, since).await
            && shows_connected(&log)
        {
            return Ok(());
        }
        if let Ok(state) = docker.inspect_container(container).await
            && matches!(state["State"]["Status"].as_str(), Some("exited" | "dead"))
        {
            let code = state["State"]["ExitCode"].as_i64().unwrap_or(-1);
            bail!("the new agent exited (code {code}) before connecting");
        }
        if started.elapsed() >= timeout {
            bail!(
                "the new agent didn't connect to the portal within {} s",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// The helper (`cha-node --replace-agent <id> --image <image>`), run from the
/// new image: replaces the agent's container `id` with one identical but for
/// `image`. The old one is renamed `<name>-previous` and stopped; the new one
/// is created and started under the old name and must log that it connected
/// within `health_timeout`. Then the old one is removed (its image stays, for
/// rollback) and `env_dir`'s `.env` follows the release. Otherwise the new one
/// is removed and the old one started again, and this fails.
pub async fn replace_agent(
    docker: &Docker,
    id: &str,
    image: &str,
    env_dir: Option<&Path>,
    health_timeout: Duration,
) -> Result<()> {
    let inspect = docker
        .inspect_container(id)
        .await
        .with_context(|| format!("inspecting the agent's container {id}"))?;
    let name = inspect["Name"]
        .as_str()
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_string();
    if name.is_empty() {
        bail!("the agent's container {id} has no name");
    }
    let previous = format!("{name}-previous");

    // Nothing is touched until all of this is sound.
    let prepared: Result<Value> = async {
        let body = create_body(&inspect, image)?;
        if docker.inspect_container(&previous).await.is_ok() {
            bail!("a container named {previous} exists already: remove or rename it");
        }
        docker.rename(id, &previous).await?;
        Ok(body)
    }
    .await;
    let body = match prepared {
        Ok(body) => body,
        Err(err) => {
            leave_result(docker, id, AgentUpdateState::Failed, &format!("{err:#}")).await;
            return Err(err);
        }
    };
    info!(%name, %image, "replacing the agent: stopping {previous}");

    let since = unix_now();
    let swapped = async {
        docker.stop(&previous, STOP_GRACE_SECS).await?;
        let new_id = docker
            .create(&name, &body)
            .await
            .context("creating the new agent")?;
        let started = async {
            docker
                .start(&new_id)
                .await
                .context("starting the new agent")?;
            wait_connected(docker, &new_id, since, health_timeout).await
        }
        .await;
        if let Err(err) = started {
            // The failed one goes before the old one takes its name back.
            let _ = docker.remove(&new_id, 5).await;
            return Err(err);
        }
        Ok(new_id)
    }
    .await;

    match swapped {
        Ok(new_id) => {
            info!(%new_id, "the new agent connected");
            if let Err(err) = docker.remove(&previous, 5).await {
                warn!("removing {previous}: {err:#}");
            }
            match (env_dir, Release::of(image)) {
                (Some(dir), Ok(release)) => match update_env_file(dir, &release) {
                    Ok(changes) if changes.is_empty() => {
                        info!("{}/.env has nothing to change", dir.display())
                    }
                    Ok(changes) => {
                        info!("updated {}/.env: {}", dir.display(), changes.join("; "))
                    }
                    Err(err) => warn!(
                        "couldn't update {}/.env: {err:#}; set the release there by hand",
                        dir.display()
                    ),
                },
                (None, _) => {
                    info!("no compose directory known: change the release in your .env by hand")
                }
                _ => {}
            }
            Ok(())
        }
        Err(err) => {
            warn!("the update failed: {err:#}; putting the previous agent back");
            docker
                .rename(&previous, &name)
                .await
                .with_context(|| format!("renaming {previous} back to {name}"))?;
            let restored_at = unix_now();
            docker
                .start(&name)
                .await
                .context("starting the previous agent again")?;
            // Report it once it is connected: that's when the note can be read.
            let detail = format!("{err:#}");
            if let Err(e) =
                wait_connected(docker, &name, restored_at, Duration::from_secs(60)).await
            {
                warn!("the previous agent: {e:#}");
            }
            leave_result(docker, &name, AgentUpdateState::RolledBack, &detail).await;
            info!("the previous agent runs again");
            Err(err.context("rolled back"))
        }
    }
}

/// Follows the helper's log until it ends or the caller is stopped (the
/// agent running it is replaced), printing what is new. For `--update-to`.
pub async fn follow_helper(docker: &Docker) {
    let since = unix_now().saturating_sub(1);
    let mut seen = 0;
    loop {
        match docker.logs_since(HELPER_NAME, since).await {
            Ok(log) => {
                let log = strip_ansi(&log);
                if log.len() > seen {
                    print!("{}", &log[seen..]);
                    seen = log.len();
                }
            }
            // Gone: it removes itself when it ends.
            Err(_) if seen > 0 => return,
            Err(_) => {}
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSPECT: &str = include_str!("../tests/fixtures/agent-inspect.json");
    const ID: &str = "3f1c0a9d8b7e6a5f4c3b2a190807f6e5d4c3b2a1908f7e6d5c4b3a291807f6e5";

    fn release(image: &str) -> Release<'_> {
        Release::of(image).unwrap()
    }

    #[test]
    fn versions_are_three_numbers() {
        for ok in ["0.2.1", "10.0.123"] {
            assert!(is_version(ok), "{ok}");
        }
        for bad in [
            "",
            "0.2",
            "0.2.1.4",
            "0.2.x",
            "v0.2.1",
            "0.2.-1",
            "1..2",
            "0.2.1-rc1",
            " 0.2.1",
            "latest",
        ] {
            assert!(!is_version(bad), "{bad}");
        }
    }

    #[test]
    fn images_are_retagged_in_their_repository() {
        assert_eq!(
            retag("ghcr.io/ban-red/cha-node:0.2.0", "0.2.1"),
            "ghcr.io/ban-red/cha-node:0.2.1"
        );
        assert_eq!(
            retag("localhost:5000/cha-node", "0.2.1"),
            "localhost:5000/cha-node:0.2.1"
        );
        assert_eq!(
            retag("reg.lan:5000/x/cha-node:0.1.0@sha256:abc", "0.2.1"),
            "reg.lan:5000/x/cha-node:0.2.1"
        );
        assert_eq!(
            streamer_image("ghcr.io/ban-red/cha-node:0.2.0", "0.2.1").as_deref(),
            Some("ghcr.io/ban-red/cha-streamer:0.2.1")
        );
        assert_eq!(streamer_image("ghcr.io/ban-red/other:0.2.0", "0.2.1"), None);
    }

    #[test]
    fn only_numbered_registry_images_update() {
        assert!(updatability_of("ghcr.io/ban-red/cha-node:0.2.0").updatable);
        for local in ["cha-node:dev", "cha/node:0.2.0"] {
            let can = updatability_of(local);
            assert!(!can.updatable, "{local}");
            assert!(can.reason.unwrap().contains("local build"));
        }
        let can = updatability_of("ghcr.io/ban-red/cha-node:latest");
        assert!(!can.updatable);
        assert_eq!(can.image, "ghcr.io/ban-red/cha-node:latest");
        assert!(!updatability_of("ghcr.io/ban-red/cha-node").updatable);
    }

    #[test]
    fn finds_its_container_in_mountinfo() {
        let text = format!(
            "\
1056 1049 0:107 / / rw,relatime master:361 - overlay overlay rw,lowerdir=/var/lib/docker/overlay2/l/AAAA\n\
1057 1056 0:110 / /proc rw,nosuid - proc proc rw\n\
1061 1056 8:1 /var/lib/docker/containers/{ID}/resolv.conf /etc/resolv.conf rw,relatime - ext4 /dev/sda1 rw\n\
1062 1056 8:1 /var/lib/docker/containers/{ID}/hostname /etc/hostname rw,relatime - ext4 /dev/sda1 rw\n"
        );
        assert_eq!(container_id_from_mountinfo(&text).as_deref(), Some(ID));
        // A custom data root still ends in docker/containers.
        let custom =
            format!("9 1 8:1 /srv/docker/containers/{ID}/hosts /etc/hosts rw - ext4 /dev/sda1 rw");
        assert_eq!(container_id_from_mountinfo(&custom).as_deref(), Some(ID));
        assert_eq!(
            container_id_from_mountinfo("1 0 0:1 / / rw - overlay overlay rw"),
            None
        );
        // Not 64 hex digits, or no path after them.
        assert_eq!(
            container_id_from_mountinfo(
                "9 1 8:1 /var/lib/docker/containers/abc123/hosts /etc/hosts rw"
            ),
            None
        );
        assert_eq!(
            container_id_from_mountinfo(&format!(
                "9 1 8:1 /var/lib/docker/containers/{ID}x/hosts /h rw"
            )),
            None
        );
    }

    #[test]
    fn env_files_change_only_the_release_lines() {
        let old = "\
# the node\r\n\
CHA_PORTAL_URL=http://p.lan:7677\n\
CHA_NODE_IMAGE=ghcr.io/ban-red/cha-node:0.2.0\n\
CHA_STREAMER_IMAGE=\"ghcr.io/ban-red/cha-streamer:0.2.0\" # pinned\n\
CHA_IMAGE_TAG=0.2.0\n\
CHA_VERSION=0.2.0\n\
CHA_GATEWAY_IMAGE=ghcr.io/ban-red/cha-gateway:0.2.0\n\
CHA_OTHER_IMAGE=ghcr.io/someone/else:0.2.0\n\
\n\
NOTE=CHA_VERSION=0.2.0\n\
last=line-without-newline";
        let new_image = "ghcr.io/ban-red/cha-node:0.2.1";
        let (text, changes) = rewrite_env_file(old, &release(new_image));
        assert_eq!(
            text,
            "\
# the node\r\n\
CHA_PORTAL_URL=http://p.lan:7677\n\
CHA_NODE_IMAGE=ghcr.io/ban-red/cha-node:0.2.1\n\
CHA_STREAMER_IMAGE=\"ghcr.io/ban-red/cha-streamer:0.2.1\" # pinned\n\
CHA_IMAGE_TAG=0.2.1\n\
CHA_VERSION=0.2.1\n\
CHA_GATEWAY_IMAGE=ghcr.io/ban-red/cha-gateway:0.2.0\n\
CHA_OTHER_IMAGE=ghcr.io/someone/else:0.2.0\n\
\n\
NOTE=CHA_VERSION=0.2.0\n\
last=line-without-newline"
        );
        assert_eq!(changes.len(), 4);
        // Again: nothing left to change.
        let (again, changes) = rewrite_env_file(&text, &release(new_image));
        assert_eq!(again, text);
        assert!(changes.is_empty());
        // Another registry's agent image isn't ours to retag.
        let (kept, _) = rewrite_env_file(
            "CHA_NODE_IMAGE=reg.lan/cha-node:0.2.0\n",
            &release(new_image),
        );
        assert_eq!(kept, "CHA_NODE_IMAGE=reg.lan/cha-node:0.2.0\n");
    }

    #[cfg(unix)]
    #[test]
    fn the_env_file_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        assert!(
            update_env_file(dir.path(), &release("ghcr.io/ban-red/cha-node:0.2.1"))
                .unwrap()
                .is_empty()
        );
        let path = dir.path().join(".env");
        std::fs::write(&path, "CHA_VERSION=0.2.0\n# keep\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let changes =
            update_env_file(dir.path(), &release("ghcr.io/ban-red/cha-node:0.2.1")).unwrap();
        assert_eq!(changes, ["CHA_VERSION: 0.2.0 -> 0.2.1"]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "CHA_VERSION=0.2.1\n# keep\n"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn the_new_container_is_the_old_one_with_another_image() {
        let inspect: Value = serde_json::from_str(INSPECT).unwrap();
        let body = create_body(&inspect, "ghcr.io/ban-red/cha-node:0.2.1").unwrap();
        assert_eq!(body["Image"], "ghcr.io/ban-red/cha-node:0.2.1");
        // The engine's own, per container.
        assert!(body.get("Hostname").is_none());
        // The host configuration and labels come over whole.
        assert_eq!(body["HostConfig"], inspect["HostConfig"]);
        assert_eq!(body["Labels"], inspect["Config"]["Labels"]);
        assert_eq!(body["Entrypoint"], json!(["cha-node"]));
        // The release variables follow; the rest of the environment stays.
        let env: Vec<&str> = body["Env"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(env.contains(&"CHA_STREAMER_IMAGE=ghcr.io/ban-red/cha-streamer:0.2.1"));
        assert!(env.contains(&"CHA_IMAGE_TAG=0.2.1"));
        assert!(env.contains(&"CHA_PLACEMENT=auto"));
        assert!(env.contains(&"PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"));
        // A user-defined network keeps its aliases but the id one.
        let net = &body["NetworkingConfig"]["EndpointsConfig"]["cha-node_default"];
        assert_eq!(net["Aliases"], json!(["cha-node-agent-1", "agent"]));
        assert!(net.get("MacAddress").is_none() && net.get("IPAddress").is_none());
        assert!(
            body["NetworkingConfig"]["EndpointsConfig"]
                .get("host")
                .is_none()
        );
        // A bare-name image can't be a release.
        assert!(create_body(&inspect, "cha-node:dev").is_err());
    }

    #[test]
    fn host_networking_needs_no_endpoints() {
        let mut inspect: Value = serde_json::from_str(INSPECT).unwrap();
        inspect["NetworkSettings"]["Networks"] = json!({ "host": { "NetworkID": "abc" } });
        inspect["HostConfig"]["NetworkMode"] = json!("host");
        inspect["Config"]["Hostname"] = json!("iolinux");
        let body = create_body(&inspect, "ghcr.io/ban-red/cha-node:0.2.1").unwrap();
        assert!(body.get("NetworkingConfig").is_none());
        assert_eq!(body["Hostname"], "iolinux");
    }

    #[test]
    fn a_connected_agent_is_found_through_its_colors() {
        let colored = "\u{1b}[2m2026-10-08T10:00:00Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m \u{1b}[2mcha_node\u{1b}[0m\u{1b}[2m:\u{1b}[0m connected \u{1b}[3mnode_id\u{1b}[0m\u{1b}[2m=\u{1b}[0mabc \u{1b}[3mportal\u{1b}[0m\u{1b}[2m=\u{1b}[0mhttp://p\n";
        assert!(shows_connected(colored));
        assert!(shows_connected(
            "INFO cha_node: connected node_id=abc portal=http://p\n"
        ));
        assert!(!shows_connected(
            "INFO disconnected from the portal closed=None\n"
        ));
        assert!(!shows_connected(
            "WARN portal connection: connecting to ws://p: refused\n"
        ));
    }

    #[test]
    fn the_helpers_note_is_read_once() {
        let dir = tempfile::tempdir().unwrap();
        assert!(take_result(dir.path()).is_none());
        let note = json!({ "state": AgentUpdateState::RolledBack, "detail": "no connection" });
        std::fs::write(dir.path().join(RESULT_FILE), note.to_string()).unwrap();
        match take_result(dir.path()) {
            Some(ToPortal::AgentUpdate { state, detail, .. }) => {
                assert_eq!(state, AgentUpdateState::RolledBack);
                assert_eq!(detail.as_deref(), Some("no connection"));
            }
            other => panic!("{other:?}"),
        }
        assert!(take_result(dir.path()).is_none());
        std::fs::write(dir.path().join(RESULT_FILE), "not json").unwrap();
        assert!(take_result(dir.path()).is_none());
    }

    #[test]
    fn the_state_directory_comes_from_the_environment() {
        let inspect: Value = serde_json::from_str(INSPECT).unwrap();
        assert_eq!(state_dir_of(&inspect), DEFAULT_STATE_DIR);
        let other = json!({ "Config": { "Env": ["CHA_NODE_STATE=/data/state"] } });
        assert_eq!(state_dir_of(&other), "/data/state");
    }
}
