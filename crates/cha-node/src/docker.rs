//! Our Docker Engine client: the handful of API calls the agent makes, as plain
//! HTTP/1.1 over the engine's Unix socket. (A full client crate would be most
//! of the agent's build for a dozen endpoints.)

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::Value;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

pub const DEFAULT_SOCKET: &str = "/var/run/docker.sock";

/// What [`Docker::exec`] keeps of a command's output.
pub const EXEC_OUTPUT_LIMIT: usize = 256 * 1024;
/// What [`Docker::logs_tail`] reads off the engine before it gives up on the
/// rest, and what it keeps of a line.
pub const LOGS_READ_LIMIT: usize = 4 * 1024 * 1024;
pub const LOG_LINE_LIMIT: usize = 2000;
/// How long [`Docker::logs_tail`] waits for the engine.
pub const LOGS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long [`Docker::exec`] waits for a command to end.
pub const EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct Docker {
    socket: PathBuf,
}

/// A container the engine knows, from `GET /containers/json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerSummary {
    pub id: String,
    pub state: String,
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub mounts: Vec<ContainerMount>,
}

/// One of a container's mounts, as `GET /containers/json` lists them.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerMount {
    /// The host path (or volume name) it mounts.
    #[serde(default)]
    pub source: String,
    /// Where it is inside the container.
    pub destination: String,
}

/// A container event (`GET /events`), with the labels we care about.
#[derive(Debug, Clone)]
pub struct ContainerEvent {
    pub action: String,
    pub id: String,
    pub labels: std::collections::HashMap<String, String>,
    pub exit_code: Option<i64>,
}

impl Docker {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// One request on a fresh connection (the engine is local; connections are
    /// cheap and calls are rare).
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Bytes)> {
        let res = self.open(method, path, body).await?;
        let status = res.status();
        let bytes = res.into_body().collect().await?.to_bytes();
        Ok((status, bytes))
    }

    async fn open(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<hyper::Response<hyper::body::Incoming>> {
        let stream = UnixStream::connect(&self.socket).await.with_context(|| {
            format!(
                "connecting to the Docker engine at {}",
                self.socket.display()
            )
        })?;
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let payload = match body {
            Some(v) => Bytes::from(serde_json::to_vec(v)?),
            None => Bytes::new(),
        };
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "docker")
            .header("content-type", "application/json")
            .body(Full::new(payload))?;
        Ok(sender.send_request(request).await?)
    }

    /// Fails with the engine's own message unless the status is a success.
    async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Bytes> {
        let (status, bytes) = self.send(method.clone(), path, body).await?;
        if !status.is_success() {
            bail!(
                "Docker {method} {path}: {status}: {}",
                engine_message(&bytes)
            );
        }
        Ok(bytes)
    }

    pub async fn ping(&self) -> Result<()> {
        self.call(Method::GET, "/_ping", None).await.map(drop)
    }

    /// The engine's version (`GET /version`).
    pub async fn version(&self) -> Result<String> {
        let v: Value = serde_json::from_slice(&self.call(Method::GET, "/version", None).await?)?;
        Ok(v["Version"].as_str().unwrap_or("?").to_string())
    }

    /// Runs a short-lived container to its end and removes it: the exit code
    /// and its output (stdout and stderr). For checks (`--doctor`).
    pub async fn run(
        &self,
        name: &str,
        config: &Value,
        timeout: std::time::Duration,
    ) -> Result<(i64, String)> {
        // A leftover from an interrupted run.
        let _ = self.remove(name, 0).await;
        let id = self.create(name, config).await?;
        let result = async {
            self.start(&id).await?;
            let path = format!("/containers/{}/wait", encode(&id));
            let waited = tokio::time::timeout(timeout, self.call(Method::POST, &path, None))
                .await
                .context("timed out")??;
            let code = serde_json::from_slice::<Value>(&waited)?["StatusCode"]
                .as_i64()
                .unwrap_or(-1);
            let path = format!("/containers/{}/logs?stdout=1&stderr=1", encode(&id));
            let logs = self.call(Method::GET, &path, None).await?;
            Ok((code, demux(&logs)))
        }
        .await;
        let _ = self.remove(&id, 0).await;
        result
    }

    /// Runs `cmd` in a running container, to its end: the exit code and its
    /// output (stdout and stderr together, as a terminal would show them),
    /// cut off at [`EXEC_OUTPUT_LIMIT`]. Gives up after [`EXEC_TIMEOUT`].
    pub async fn exec(&self, container: &str, cmd: &[&str]) -> Result<(i64, String)> {
        tokio::time::timeout(EXEC_TIMEOUT, self.exec_inner(container, cmd))
            .await
            .context("timed out")?
    }

    async fn exec_inner(&self, container: &str, cmd: &[&str]) -> Result<(i64, String)> {
        let created = self
            .call(
                Method::POST,
                &format!("/containers/{}/exec", encode(container)),
                Some(&serde_json::json!({
                    "AttachStdout": true,
                    "AttachStderr": true,
                    "Tty": true,
                    "Cmd": cmd,
                })),
            )
            .await?;
        let id = serde_json::from_slice::<Value>(&created)?["Id"]
            .as_str()
            .map(str::to_string)
            .context("the engine returned no exec id")?;
        // With a terminal the output is the raw stream, not framed like logs.
        let res = self
            .open(
                Method::POST,
                &format!("/exec/{}/start", encode(&id)),
                Some(&serde_json::json!({ "Detach": false, "Tty": true })),
            )
            .await?;
        if !res.status().is_success() {
            bail!("running {cmd:?} in {container}: {}", res.status());
        }
        let mut body = res.into_body();
        let mut output = Vec::new();
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame?.into_data() {
                output.extend_from_slice(&data);
                if output.len() >= EXEC_OUTPUT_LIMIT {
                    output.truncate(EXEC_OUTPUT_LIMIT);
                    break;
                }
            }
        }
        let inspected = self
            .call(Method::GET, &format!("/exec/{}/json", encode(&id)), None)
            .await?;
        let code = serde_json::from_slice::<Value>(&inspected)?["ExitCode"]
            .as_i64()
            .unwrap_or(-1);
        Ok((code, String::from_utf8_lossy(&output).into_owned()))
    }

    /// The last `tail` lines a container wrote (stdout and stderr, each led by
    /// its timestamp), alive or dead. Our containers have no TTY, so the
    /// engine frames the stream (see [`demux`]); one with a TTY gives it raw,
    /// and that reads as well. At most [`LOGS_READ_LIMIT`] bytes are read and
    /// each line is cut at [`LOG_LINE_LIMIT`] characters.
    pub async fn logs_tail(&self, container: &str, tail: usize) -> Result<Vec<String>> {
        let path = format!(
            "/containers/{}/logs?stdout=1&stderr=1&tail={tail}&timestamps=1",
            encode(container)
        );
        let raw = tokio::time::timeout(LOGS_TIMEOUT, async {
            let res = self.open(Method::GET, &path, None).await?;
            if !res.status().is_success() {
                bail!("reading the logs of {container}: {}", res.status());
            }
            let mut body = res.into_body();
            let mut raw = Vec::new();
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame?.into_data() {
                    raw.extend_from_slice(&data);
                    if raw.len() >= LOGS_READ_LIMIT {
                        break;
                    }
                }
            }
            Ok(raw)
        })
        .await
        .context("timed out reading the logs")??;
        let text = demux(&raw);
        let lines: Vec<&str> = text.lines().collect();
        let skip = lines.len().saturating_sub(tail);
        Ok(lines[skip..]
            .iter()
            .map(|l| l.chars().take(LOG_LINE_LIMIT).collect())
            .collect())
    }

    pub async fn image_exists(&self, image: &str) -> Result<bool> {
        let (status, bytes) = self
            .send(
                Method::GET,
                &format!("/images/{}/json", encode(image)),
                None,
            )
            .await?;
        match status {
            s if s.is_success() => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            s => bail!("inspecting image {image}: {s}: {}", engine_message(&bytes)),
        }
    }

    /// Pulls `image` (which must name a registry it can reach: see
    /// [`names_registry`]).
    pub async fn pull(&self, image: &str) -> Result<()> {
        let (name, tag) = match image.rsplit_once(':') {
            Some((n, t)) if !t.contains('/') => (n, t),
            _ => (image, "latest"),
        };
        let bytes = self
            .call(
                Method::POST,
                &format!(
                    "/images/create?fromImage={}&tag={}",
                    encode(name),
                    encode(tag)
                ),
                None,
            )
            .await?;
        // Pull progress is a stream of JSON lines; a failure arrives as one of them.
        for line in bytes.split(|b| *b == b'\n') {
            if let Ok(v) = serde_json::from_slice::<Value>(line)
                && let Some(err) = v.get("error").and_then(Value::as_str)
            {
                bail!("pulling {image}: {err}");
            }
        }
        Ok(())
    }

    /// Whether `path` exists on the host, which the agent in its container
    /// can't see. The engine checks a bind's source when it creates the
    /// container, so this creates one that is never started (read-only, no
    /// network, nothing run) from `image`, and removes it. The engine never
    /// makes the path (`CreateMountpoint` is off). An error is the engine
    /// failing some other way: the answer is unknown.
    pub async fn host_path_exists(&self, image: &str, path: &Path) -> Result<bool> {
        let name = "cha-probe-host-path";
        let source = path.to_string_lossy();
        let config = serde_json::json!({
            "Image": image,
            "HostConfig": {
                "NetworkMode": "none",
                "Mounts": [{
                    "Type": "bind",
                    "Source": source,
                    "Target": source,
                    "ReadOnly": true,
                    "BindOptions": { "CreateMountpoint": false },
                }],
            },
        });
        // A leftover from an interrupted probe.
        let _ = self.remove(name, 0).await;
        match self.create(name, &config).await {
            Ok(id) => {
                let _ = self.remove(&id, 0).await;
                Ok(true)
            }
            Err(err) if format!("{err:#}").contains("bind source path does not exist") => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Creates a container; returns its id.
    pub async fn create(&self, name: &str, config: &Value) -> Result<String> {
        let bytes = self
            .call(
                Method::POST,
                &format!("/containers/create?name={}", encode(name)),
                Some(config),
            )
            .await?;
        let v: Value = serde_json::from_slice(&bytes)?;
        v["Id"]
            .as_str()
            .map(str::to_string)
            .context("the engine returned no container id")
    }

    pub async fn start(&self, id: &str) -> Result<()> {
        let (status, bytes) = self
            .send(
                Method::POST,
                &format!("/containers/{}/start", encode(id)),
                None,
            )
            .await?;
        // 304: already running.
        if status.is_success() || status == StatusCode::NOT_MODIFIED {
            Ok(())
        } else {
            bail!("starting {id}: {status}: {}", engine_message(&bytes))
        }
    }

    /// Stops (SIGTERM, then SIGKILL after `grace_secs`) and removes a container
    /// and its anonymous volumes. A container that's already gone is fine.
    pub async fn remove(&self, id: &str, grace_secs: u32) -> Result<()> {
        let id = encode(id);
        let (status, bytes) = self
            .send(
                Method::POST,
                &format!("/containers/{id}/stop?t={grace_secs}"),
                None,
            )
            .await?;
        if !(status.is_success()
            || status == StatusCode::NOT_MODIFIED
            || status == StatusCode::NOT_FOUND)
        {
            bail!("stopping {id}: {status}: {}", engine_message(&bytes));
        }
        let (status, bytes) = self
            .send(
                Method::DELETE,
                &format!("/containers/{id}?force=true&v=true"),
                None,
            )
            .await?;
        if !(status.is_success() || status == StatusCode::NOT_FOUND) {
            bail!("removing {id}: {status}: {}", engine_message(&bytes));
        }
        Ok(())
    }

    pub async fn remove_volume(&self, name: &str) -> Result<()> {
        let (status, bytes) = self
            .send(
                Method::DELETE,
                &format!("/volumes/{}?force=true", encode(name)),
                None,
            )
            .await?;
        if status.is_success() || status == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            bail!(
                "removing volume {name}: {status}: {}",
                engine_message(&bytes)
            )
        }
    }

    /// Whether a volume by this name exists.
    pub async fn volume_exists(&self, name: &str) -> Result<bool> {
        let (status, bytes) = self
            .send(Method::GET, &format!("/volumes/{}", encode(name)), None)
            .await?;
        match status {
            s if s.is_success() => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            s => bail!("inspecting volume {name}: {s}: {}", engine_message(&bytes)),
        }
    }

    /// The names of the volumes that start with `prefix`, sorted.
    pub async fn volumes_named(&self, prefix: &str) -> Result<Vec<String>> {
        let filters = serde_json::json!({ "name": [prefix] }).to_string();
        let bytes = self
            .call(
                Method::GET,
                &format!("/volumes?filters={}", encode(&filters)),
                None,
            )
            .await?;
        volume_names(&bytes, prefix)
    }

    /// One reading of a running container's use (`GET /containers/<id>/stats`,
    /// without waiting for a second sample: the caller keeps the last one).
    pub async fn stats(&self, id: &str) -> Result<Value> {
        let path = format!(
            "/containers/{}/stats?stream=false&one-shot=true",
            encode(id)
        );
        Ok(serde_json::from_slice(
            &self.call(Method::GET, &path, None).await?,
        )?)
    }

    /// Every container (running or not) carrying `label`.
    pub async fn list(&self, label: &str) -> Result<Vec<ContainerSummary>> {
        let filters = serde_json::json!({ "label": [label] }).to_string();
        let bytes = self
            .call(
                Method::GET,
                &format!("/containers/json?all=true&filters={}", encode(&filters)),
                None,
            )
            .await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Container events for containers carrying `label`, until the stream ends
    /// or the receiver is dropped.
    pub async fn events(&self, label: &str, tx: mpsc::Sender<ContainerEvent>) -> Result<()> {
        let filters = serde_json::json!({ "type": ["container"], "label": [label] }).to_string();
        let res = self
            .open(
                Method::GET,
                &format!("/events?filters={}", encode(&filters)),
                None,
            )
            .await?;
        if !res.status().is_success() {
            bail!("watching events: {}", res.status());
        }
        let mut body = res.into_body();
        let mut pending = Vec::new();
        while let Some(frame) = body.frame().await {
            let Ok(data) = frame?.into_data() else {
                continue;
            };
            pending.extend_from_slice(&data);
            while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = pending.drain(..=end).collect();
                if let Some(event) = parse_event(&line)
                    && tx.send(event).await.is_err()
                {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

/// The names in a `GET /volumes` reply that start with `prefix` (the engine's
/// `name` filter matches anywhere in the name), sorted. `Volumes` is `null`
/// when there are none.
fn volume_names(body: &[u8], prefix: &str) -> Result<Vec<String>> {
    let reply: Value = serde_json::from_slice(body)?;
    let mut names: Vec<String> = reply["Volumes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["Name"].as_str())
        .filter(|name| name.starts_with(prefix))
        .map(str::to_string)
        .collect();
    names.sort();
    Ok(names)
}

fn parse_event(line: &[u8]) -> Option<ContainerEvent> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let attributes = v["Actor"]["Attributes"].as_object()?;
    let labels = attributes
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
        .collect::<std::collections::HashMap<_, _>>();
    Some(ContainerEvent {
        action: v["Action"].as_str()?.to_string(),
        id: v["Actor"]["ID"].as_str()?.to_string(),
        exit_code: labels.get("exitCode").and_then(|c| c.parse().ok()),
        labels,
    })
}

/// A container's log stream as text. Without a TTY the engine frames each
/// chunk with an 8-byte header (stream, 0, 0, 0, big-endian length); with one
/// the stream is raw. Text never starts with a 0, 1 or 2 followed by three
/// zero bytes, so that tells them apart.
fn demux(raw: &[u8]) -> String {
    if !(raw.len() >= 8 && raw[0] <= 2 && raw[1..4] == [0, 0, 0]) {
        return String::from_utf8_lossy(raw).into_owned();
    }
    let mut text = Vec::new();
    let mut rest = raw;
    while rest.len() >= 8 {
        let len = u32::from_be_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let end = (8 + len).min(rest.len());
        text.extend_from_slice(&rest[8..end]);
        rest = &rest[end..];
    }
    String::from_utf8_lossy(&text).into_owned()
}

/// The engine's error message from a JSON error body.
fn engine_message(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(body).trim().to_string())
}

/// Percent-encodes a path segment or query value.
pub(crate) fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Whether `image` names its registry (`ghcr.io/owner/name:tag`,
/// `localhost:5000/name`), so a pull goes where its name says. A bare name
/// (`cha/streamer:dev`) is one we build locally: pulled, it would come from
/// whoever owns that namespace on Docker Hub, so the agent never pulls one.
pub fn names_registry(image: &str) -> bool {
    match image.split_once('/') {
        Some((host, _)) => host.contains('.') || host.contains(':') || host == "localhost",
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demuxes_logs() {
        let mut raw = vec![1, 0, 0, 0, 0, 0, 0, 3];
        raw.extend(b"ok\n");
        raw.extend([2, 0, 0, 0, 0, 0, 0, 4]);
        raw.extend(b"err\n");
        assert_eq!(demux(&raw), "ok\nerr\n");
    }

    #[test]
    fn reads_a_terminals_logs_raw() {
        assert_eq!(demux(b"one\r\ntwo\r\n"), "one\r\ntwo\r\n");
        assert_eq!(demux(b""), "");
        assert_eq!(demux(b"short"), "short");
    }

    #[test]
    fn demuxes_a_frame_cut_short() {
        let mut raw = vec![1, 0, 0, 0, 0, 0, 0, 9];
        raw.extend(b"cut");
        assert_eq!(demux(&raw), "cut");
    }

    #[test]
    fn only_names_with_a_registry_are_pulled() {
        assert!(names_registry("ghcr.io/ban-red/cha-streamer:0.1.0"));
        assert!(names_registry("localhost:5000/cha-streamer"));
        assert!(names_registry("localhost/cha-streamer"));
        assert!(!names_registry("cha/streamer:dev"));
        assert!(!names_registry("cha/env-chrome:dev"));
        assert!(!names_registry("ubuntu"));
    }

    #[test]
    fn encodes_names_and_filters() {
        assert_eq!(encode("cha/env-chrome:dev"), "cha%2Fenv-chrome%3Adev");
        assert_eq!(
            encode(r#"{"label":["a"]}"#),
            "%7B%22label%22%3A%5B%22a%22%5D%7D"
        );
    }

    #[test]
    fn parses_die_events() {
        let line = br#"{"Type":"container","Action":"die","Actor":{"ID":"abc","Attributes":{"exitCode":"137","sh.cha.env":"e1","image":"x"}},"time":1}"#;
        let event = parse_event(line).unwrap();
        assert_eq!(event.action, "die");
        assert_eq!(event.id, "abc");
        assert_eq!(event.exit_code, Some(137));
        assert_eq!(event.labels["sh.cha.env"], "e1");
    }

    #[test]
    fn lists_volumes_by_prefix() {
        let body = br#"{"Volumes":[
            {"Name":"cha-home-u2-steam","Driver":"local"},
            {"Name":"other-cha-home-x","Driver":"local"},
            {"Name":"cha-home-u1-steam","Driver":"local"}],"Warnings":null}"#;
        assert_eq!(
            volume_names(body, "cha-home-").unwrap(),
            ["cha-home-u1-steam", "cha-home-u2-steam"]
        );
        assert!(
            volume_names(br#"{"Volumes":null,"Warnings":null}"#, "cha-home-")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reads_engine_errors() {
        assert_eq!(
            engine_message(br#"{"message":"No such image"}"#),
            "No such image"
        );
        assert_eq!(engine_message(b"plain text\n"), "plain text");
    }
}
