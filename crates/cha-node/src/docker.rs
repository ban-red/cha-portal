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

    /// Pulls `image` (which must name a registry it can reach).
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

/// The engine's error message from a JSON error body.
fn engine_message(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(body).trim().to_string())
}

/// Percent-encodes a path segment or query value.
fn encode(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn reads_engine_errors() {
        assert_eq!(
            engine_message(br#"{"message":"No such image"}"#),
            "No such image"
        );
        assert_eq!(engine_message(b"plain text\n"), "plain text");
    }
}
