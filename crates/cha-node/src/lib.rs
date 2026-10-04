//! `cha-node`: the agent on each machine that runs environments.
//!
//! It enrolls once with an admin's join token, keeping its Ed25519 identity in
//! a state directory, then holds one WebSocket to the portal (ADR 0001):
//! answering the portal's challenge, reporting inventory, sending heartbeats
//! and serving requests. P1.3 adds the Wolf adapter behind those requests.

pub mod inventory;

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use cha_wire::{
    CONNECT_PATH, ENROLL_PATH, EnrollRequest, EnrollResponse, Inventory, NodeKey, NodeRequest,
    NodeResponse, PROTOCOL_VERSION, ToNode, ToPortal, close,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const IDENTITY_FILE: &str = "node.json";
const INVENTORY_REFRESH: Duration = Duration::from_secs(300);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Selects rustls' ring provider for every TLS client in the process.
pub fn init_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Who this node is to the portal. Holds the private key: the file is 0600.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub portal_url: String,
    pub node_id: String,
    pub name: String,
    secret_key: String,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("portal_url", &self.portal_url)
            .field("node_id", &self.node_id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Identity {
    pub fn key(&self) -> Result<NodeKey> {
        let secret: [u8; 32] = STANDARD
            .decode(&self.secret_key)
            .ok()
            .and_then(|b| b.try_into().ok())
            .context("the identity's key is corrupt")?;
        Ok(NodeKey::from_secret(secret))
    }

    pub fn load(state_dir: &Path) -> Result<Option<Self>> {
        let path = state_dir.join(IDENTITY_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .with_context(|| format!("reading {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Writes the identity atomically, readable only by this user.
    pub fn save(&self, state_dir: &Path) -> Result<()> {
        fs::create_dir_all(state_dir)
            .with_context(|| format!("creating {}", state_dir.display()))?;
        let path = state_dir.join(IDENTITY_FILE);
        let tmp = state_dir.join(format!("{IDENTITY_FILE}.tmp"));
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// `https://portal.example/` → `https://portal.example`.
pub fn normalize_portal_url(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!("the portal URL must start with http:// or https:// (got {url:?})");
    }
    Ok(url.to_string())
}

fn connect_url(portal_url: &str) -> String {
    let ws = portal_url
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    format!("{ws}{CONNECT_PATH}")
}

/// Redeems a join token with a fresh key.
pub async fn enroll(portal_url: &str, token: &str, name: &str) -> Result<Identity> {
    let portal_url = normalize_portal_url(portal_url)?;
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|e| anyhow::anyhow!("random source: {e}"))?;
    let key = NodeKey::from_secret(secret);
    let request = EnrollRequest {
        token: token.trim().to_string(),
        public_key: key.public_b64(),
        name: name.to_string(),
        agent_version: AGENT_VERSION.to_string(),
    };
    let res = reqwest::Client::new()
        .post(format!("{portal_url}{ENROLL_PATH}"))
        .json(&request)
        .send()
        .await
        .with_context(|| format!("reaching {portal_url}"))?;
    let status = res.status();
    if !status.is_success() {
        let body: serde_json::Value = res.json().await.unwrap_or_default();
        let message = body["message"].as_str().unwrap_or("no details");
        bail!("the portal refused enrollment ({status}): {message}");
    }
    let EnrollResponse { node_id } = res.json().await.context("reading the enrollment reply")?;
    Ok(Identity {
        portal_url,
        node_id,
        name: name.to_string(),
        secret_key: STANDARD.encode(key.secret()),
    })
}

/// How the portal ended a connection.
#[derive(Debug, Clone, PartialEq)]
pub struct Closed {
    pub code: u16,
    pub reason: String,
}

pub struct Agent {
    identity: Identity,
    key: NodeKey,
    inventory: fn() -> Inventory,
}

impl Agent {
    pub fn new(identity: Identity) -> Result<Self> {
        Ok(Self {
            key: identity.key()?,
            identity,
            inventory: inventory::collect,
        })
    }

    /// Replaces the inventory probe (tests).
    pub fn with_inventory(mut self, probe: fn() -> Inventory) -> Self {
        self.inventory = probe;
        self
    }

    /// Stays connected, reconnecting with backoff, until the portal says this
    /// node no longer exists; that's the only way it returns.
    pub async fn run(&self) -> Result<()> {
        let mut backoff = MIN_BACKOFF;
        loop {
            let mut welcomed = false;
            let ended = self.session(&mut welcomed).await;
            let next = match ended {
                Ok(Some(c)) if c.code == close::UNKNOWN_NODE || c.code == close::BAD_SIGNATURE => {
                    bail!(
                        "the portal no longer accepts this node ({}). Enroll it again with a new \
                         join token after deleting {IDENTITY_FILE} from the state directory.",
                        c.reason
                    );
                }
                Ok(Some(c)) if c.code == close::REPLACED => {
                    warn!("another agent with this identity connected; is it running twice?");
                    MAX_BACKOFF
                }
                Ok(closed) => {
                    info!(?closed, "disconnected from the portal");
                    if welcomed { MIN_BACKOFF } else { backoff }
                }
                Err(err) => {
                    warn!("portal connection: {err:#}");
                    if welcomed { MIN_BACKOFF } else { backoff }
                }
            };
            tokio::time::sleep(jittered(next)).await;
            backoff = (next * 2).min(MAX_BACKOFF);
        }
    }

    /// One connection, until it ends. Returns the portal's close frame, if it
    /// sent one. Sets `welcomed` once the portal accepted the hello.
    pub async fn session(&self, welcomed: &mut bool) -> Result<Option<Closed>> {
        let url = connect_url(&self.identity.portal_url);
        let (ws, _) = tokio_tungstenite::connect_async(&url)
            .await
            .with_context(|| format!("connecting to {url}"))?;
        let (mut sink, mut stream) = ws.split();

        let nonce = match next_message(&mut stream).await? {
            Next::Message(ToNode::Challenge { nonce, protocol }) => {
                if protocol != PROTOCOL_VERSION {
                    warn!(protocol, "the portal speaks a different protocol version");
                }
                nonce
            }
            Next::Closed(closed) => return Ok(closed),
            Next::Message(other) => bail!("expected a challenge, got {other:?}"),
        };
        let node_id = &self.identity.node_id;
        let hello = ToPortal::Hello {
            node_id: node_id.clone(),
            signature: self.key.sign_b64(&cha_wire::hello_message(&nonce, node_id)),
            agent_version: AGENT_VERSION.into(),
            protocol: PROTOCOL_VERSION,
        };
        sink.send(encode(&hello)?).await?;
        let heartbeat = match next_message(&mut stream).await? {
            Next::Message(ToNode::Welcome { heartbeat_secs, .. }) => {
                Duration::from_secs(heartbeat_secs.max(1))
            }
            Next::Closed(closed) => return Ok(closed),
            Next::Message(other) => bail!("expected a welcome, got {other:?}"),
        };
        *welcomed = true;
        info!(%node_id, portal = %self.identity.portal_url, "connected");

        let probe = self.inventory;
        let mut inventory = tokio::task::spawn_blocking(probe).await?;
        sink.send(encode(&ToPortal::Inventory {
            inventory: inventory.clone(),
        })?)
        .await?;

        let start = tokio::time::Instant::now();
        let mut heartbeats = tokio::time::interval_at(start + heartbeat, heartbeat);
        let mut refresh = tokio::time::interval_at(start + INVENTORY_REFRESH, INVENTORY_REFRESH);
        let mut last_heard = Instant::now();
        loop {
            tokio::select! {
                _ = heartbeats.tick() => {
                    if last_heard.elapsed() > heartbeat * 3 {
                        bail!("the portal went quiet");
                    }
                    sink.send(encode(&ToPortal::Heartbeat)?).await?;
                    // The portal's WebSocket layer pongs, which proves it's alive.
                    sink.send(Message::Ping(Default::default())).await?;
                }
                _ = refresh.tick() => {
                    let fresh = tokio::task::spawn_blocking(probe).await?;
                    if fresh != inventory {
                        inventory = fresh;
                        sink.send(encode(&ToPortal::Inventory { inventory: inventory.clone() })?).await?;
                    }
                }
                incoming = stream.next() => {
                    let msg = match incoming {
                        None => return Ok(None),
                        Some(msg) => msg?,
                    };
                    last_heard = Instant::now();
                    match msg {
                        Message::Text(text) => match serde_json::from_str::<ToNode>(&text)? {
                            ToNode::Request { id, request } => {
                                let result = handle(request).await;
                                sink.send(encode(&ToPortal::Response { id, result })?).await?;
                            }
                            // The agent doesn't ask the portal anything yet.
                            ToNode::Response { .. } => {}
                            other => bail!("unexpected {other:?}"),
                        },
                        Message::Close(frame) => return Ok(frame.map(closed)),
                        _ => {}
                    }
                }
            }
        }
    }
}

async fn handle(request: NodeRequest) -> Result<NodeResponse, String> {
    match request {
        NodeRequest::Ping => Ok(NodeResponse::Pong { unix_ms: unix_ms() }),
    }
}

enum Next {
    Message(ToNode),
    Closed(Option<Closed>),
}

async fn next_message<S>(stream: &mut S) -> Result<Next>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let wait = Duration::from_secs(10);
    loop {
        let msg = tokio::time::timeout(wait, stream.next())
            .await
            .context("the portal didn't answer")?;
        match msg {
            None => return Ok(Next::Closed(None)),
            Some(Ok(Message::Text(text))) => {
                return Ok(Next::Message(serde_json::from_str(&text)?));
            }
            Some(Ok(Message::Close(frame))) => return Ok(Next::Closed(frame.map(closed))),
            Some(Ok(_)) => continue,
            Some(Err(err)) => return Err(err.into()),
        }
    }
}

fn closed(frame: tokio_tungstenite::tungstenite::protocol::CloseFrame) -> Closed {
    Closed {
        code: frame.code.into(),
        reason: frame.reason.to_string(),
    }
}

fn encode(msg: &ToPortal) -> Result<Message> {
    Ok(Message::text(serde_json::to_string(msg)?))
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// ±20%, so a portal restart doesn't get every node back at once.
fn jittered(d: Duration) -> Duration {
    let mut byte = [0u8; 1];
    let _ = getrandom::fill(&mut byte);
    d.mul_f64(0.8 + 0.4 * f64::from(byte[0]) / 255.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_urls_become_connect_urls() {
        let url = normalize_portal_url(" https://portal.example/ ").unwrap();
        assert_eq!(url, "https://portal.example");
        assert_eq!(connect_url(&url), "wss://portal.example/api/node/connect");
        assert_eq!(
            connect_url("http://127.0.0.1:8090"),
            "ws://127.0.0.1:8090/api/node/connect"
        );
        assert!(normalize_portal_url("portal.example").is_err());
    }

    #[test]
    fn identity_round_trips_privately() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Identity::load(dir.path()).unwrap().is_none());
        let key = NodeKey::from_secret([9u8; 32]);
        let id = Identity {
            portal_url: "http://p".into(),
            node_id: "n".into(),
            name: "box".into(),
            secret_key: STANDARD.encode(key.secret()),
        };
        id.save(dir.path()).unwrap();
        let back = Identity::load(dir.path()).unwrap().unwrap();
        assert_eq!(back.key().unwrap().public_b64(), key.public_b64());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(IDENTITY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
