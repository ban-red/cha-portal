//! Messages between a Cha Node agent and the portal (`cha-control`).
//!
//! ADR 0001: one WebSocket per node carries JSON messages. Requests carry an
//! `id` that the reply echoes; pushes have none.
//!
//! **Enrollment** (HTTP, once): the node redeems an admin's join token with its
//! Ed25519 public key and gets a node id.
//!
//! **Connection:**
//! 1. Portal → [`ToNode::Challenge`].
//! 2. Node → [`ToPortal::Hello`], signing the challenge with its key ([`hello_message`]).
//! 3. Portal → [`ToNode::Welcome`].
//! 4. Node → [`ToPortal::Inventory`] and [`ToPortal::Environments`] (what it
//!    is running, so both sides reconcile), then [`ToPortal::Heartbeat`] every
//!    `heartbeat_secs`. Requests flow both ways; the node pushes
//!    [`ToPortal::EnvironmentExited`] when an environment stops on its own.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Bumped on breaking changes to these messages.
pub const PROTOCOL_VERSION: u32 = 1;

/// The node connection's WebSocket path on the portal.
pub const CONNECT_PATH: &str = "/api/node/connect";
/// The enrollment endpoint's path on the portal.
pub const ENROLL_PATH: &str = "/api/node/enroll";

/// WebSocket close codes the portal uses to tell a node why it hung up.
pub mod close {
    /// The node isn't enrolled, or an admin removed it: reconnecting won't help.
    pub const UNKNOWN_NODE: u16 = 4001;
    /// A newer connection from the same node took over.
    pub const REPLACED: u16 = 4002;
    /// The hello didn't verify against the node's enrolled key.
    pub const BAD_SIGNATURE: u16 = 4003;
}

/// `POST /api/node/enroll`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollRequest {
    pub token: String,
    /// Base64 Ed25519 public key.
    pub public_key: String,
    pub name: String,
    pub agent_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollResponse {
    pub node_id: String,
}

/// Portal → node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToNode {
    Challenge {
        nonce: String,
        protocol: u32,
    },
    Welcome {
        node_id: String,
        heartbeat_secs: u64,
    },
    Request {
        id: u64,
        request: NodeRequest,
    },
    Response {
        id: u64,
        result: Result<PortalResponse, String>,
    },
}

/// Node → portal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToPortal {
    Hello {
        node_id: String,
        /// Base64 Ed25519 signature of [`hello_message`].
        signature: String,
        agent_version: String,
        protocol: u32,
    },
    Heartbeat,
    Inventory {
        inventory: Inventory,
    },
    /// The environments this node is running (ids), sent after every welcome.
    Environments {
        running: Vec<String>,
    },
    /// An environment stopped without being asked (its app exited, or a
    /// container died); the node has cleaned it up.
    EnvironmentExited {
        id: String,
        detail: String,
        /// It ended with an error (a non-zero exit, a crash) rather than the
        /// app quitting normally.
        failed: bool,
    },
    Request {
        id: u64,
        request: PortalRequest,
    },
    Response {
        id: u64,
        result: Result<NodeResponse, String>,
    },
}

/// What the portal asks a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum NodeRequest {
    Ping,
    /// Start an environment: its streamer, then its app. Idempotent by id.
    StartEnvironment {
        environment: EnvironmentSpec,
    },
    /// Stop and remove an environment. Idempotent: an unknown id is stopped.
    StopEnvironment {
        id: String,
    },
    /// A browser wants to watch (and drive) an environment: hand its WebRTC
    /// offer to the environment's streamer, with the portal's media token.
    Connect {
        environment_id: String,
        /// `h264`, `hevc` or `av1`.
        codec: String,
        /// The browser's SDP offer (`{"type": "offer", "sdp": …}`).
        offer: serde_json::Value,
        media_token: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum NodeResponse {
    Pong {
        unix_ms: u64,
    },
    EnvironmentStarted {
        id: String,
        streamer: StreamerEndpoint,
    },
    EnvironmentStopped {
        id: String,
    },
    /// The streamer's SDP answer.
    Answer {
        answer: serde_json::Value,
    },
}

/// What to run, decided by the portal from a catalog template.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSpec {
    pub id: String,
    /// The app's image, e.g. `cha/env-chrome:dev`.
    pub image: String,
    pub security: SecurityProfile,
    /// `/dev/shm` for the app (browsers need more than Docker's 64 MB).
    pub shm_mb: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// The portal's public key (base64 Ed25519): the streamer accepts only
    /// connections that carry a media token it signed.
    pub portal_key: String,
    /// A volume kept across launches, mounted as the app's home (templates
    /// marked persistent: one per user and template, on this node).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
}

/// The app container's confinement (plan §4.2). Every profile runs the app as
/// an unprivileged user with no capabilities and no privilege gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityProfile {
    /// Docker's default seccomp profile.
    Standard,
    /// Also lets the app create namespaces, so browser sandboxes stay on.
    Browser,
    /// `browser`, plus mounts inside the app's own user namespaces (the
    /// `cha-sandbox` AppArmor profile, which the owner loads on the node):
    /// Steam's pressure-vessel builds a container for every game.
    Steam,
}

/// Where an environment's streamer listens on its node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamerEndpoint {
    pub http_port: u16,
    pub webrtc_port: u16,
}

/// What a node asks the portal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PortalRequest {
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PortalResponse {
    Pong { unix_ms: u64 },
}

/// What a node has, refreshed when it changes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub cpus: u32,
    pub memory_mb: u64,
    pub gpus: Vec<Gpu>,
    /// Addresses browsers might reach the node at (LAN, overlay).
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    /// `nvidia`, `amd`, `intel`, …
    pub vendor: String,
    pub name: String,
    pub memory_mb: Option<u64>,
    pub driver: Option<String>,
    /// e.g. `/dev/dri/renderD128`.
    pub render_node: Option<String>,
    /// Hardware encoders: `h264`, `hevc`, `av1`.
    pub encoders: Vec<String>,
}

/// The bytes a node signs to prove it holds its key: bound to this connection's
/// challenge and to the node id.
pub fn hello_message(nonce: &str, node_id: &str) -> Vec<u8> {
    format!("cha-node-hello/v{PROTOCOL_VERSION}:{nonce}:{node_id}").into_bytes()
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("not valid base64")]
    Base64,
    #[error("wrong length")]
    Length,
    #[error("not a valid Ed25519 key")]
    Key,
    #[error("signature doesn't verify")]
    Signature,
}

/// An Ed25519 key pair: a node's identity, or the portal's signing key.
pub struct NodeKey(SigningKey);

impl NodeKey {
    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&secret))
    }

    pub fn secret(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn public_b64(&self) -> String {
        STANDARD.encode(self.0.verifying_key().to_bytes())
    }

    pub fn sign_b64(&self, message: &[u8]) -> String {
        STANDARD.encode(self.0.sign(message).to_bytes())
    }
}

/// Parses a base64 Ed25519 public key.
pub fn parse_public_key(b64: &str) -> Result<VerifyingKey, KeyError> {
    let bytes: [u8; 32] = STANDARD
        .decode(b64)
        .map_err(|_| KeyError::Base64)?
        .try_into()
        .map_err(|_| KeyError::Length)?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| KeyError::Key)
}

/// Checks a base64 signature of `message` by `public_key_b64`.
pub fn verify_b64(
    public_key_b64: &str,
    message: &[u8],
    signature_b64: &str,
) -> Result<(), KeyError> {
    let key = parse_public_key(public_key_b64)?;
    let sig: [u8; 64] = STANDARD
        .decode(signature_b64)
        .map_err(|_| KeyError::Base64)?
        .try_into()
        .map_err(|_| KeyError::Length)?;
    key.verify(message, &Signature::from_bytes(&sig))
        .map_err(|_| KeyError::Signature)
}

/// What a media token lets its bearer do: connect to one environment, briefly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaClaims {
    /// The environment.
    pub env: String,
    /// The user it was issued to.
    pub sub: String,
    /// `owner` (the only role until sharing, plan §3.4).
    pub role: String,
    /// Expiry, Unix seconds.
    pub exp: i64,
}

const MEDIA_TOKEN_CONTEXT: &str = "cha-media/v1.";

/// Signs `claims` as `<base64url claims>.<base64url signature>`.
pub fn sign_media_token(key: &NodeKey, claims: &MediaClaims) -> String {
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("claims serialize"));
    let signature = key
        .0
        .sign(format!("{MEDIA_TOKEN_CONTEXT}{body}").as_bytes());
    format!("{body}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MediaTokenError {
    #[error("malformed token")]
    Malformed,
    #[error("bad signature")]
    Signature,
    #[error("expired")]
    Expired,
    #[error("for another environment")]
    WrongEnvironment,
}

/// Checks a media token against the portal's public key, its expiry (at
/// `now`, Unix seconds) and the environment it must be for.
pub fn verify_media_token(
    portal_key_b64: &str,
    token: &str,
    environment: &str,
    now: i64,
) -> Result<MediaClaims, MediaTokenError> {
    let (body, signature) = token.split_once('.').ok_or(MediaTokenError::Malformed)?;
    let key = parse_public_key(portal_key_b64).map_err(|_| MediaTokenError::Signature)?;
    let signature: [u8; 64] = URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(MediaTokenError::Malformed)?;
    key.verify(
        format!("{MEDIA_TOKEN_CONTEXT}{body}").as_bytes(),
        &Signature::from_bytes(&signature),
    )
    .map_err(|_| MediaTokenError::Signature)?;
    let claims: MediaClaims = URL_SAFE_NO_PAD
        .decode(body)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or(MediaTokenError::Malformed)?;
    if claims.exp < now {
        return Err(MediaTokenError::Expired);
    }
    if claims.env != environment {
        return Err(MediaTokenError::WrongEnvironment);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_hello_verifies_only_for_its_challenge() {
        let key = NodeKey::from_secret([7u8; 32]);
        let msg = hello_message("nonce-1", "node-a");
        let sig = key.sign_b64(&msg);
        assert!(verify_b64(&key.public_b64(), &msg, &sig).is_ok());
        assert!(verify_b64(&key.public_b64(), &hello_message("nonce-2", "node-a"), &sig).is_err());
        assert!(verify_b64(&key.public_b64(), &hello_message("nonce-1", "node-b"), &sig).is_err());
        let other = NodeKey::from_secret([8u8; 32]);
        assert!(verify_b64(&other.public_b64(), &msg, &sig).is_err());
    }

    #[test]
    fn messages_have_stable_json() {
        let hello = serde_json::to_value(ToNode::Challenge {
            nonce: "n".into(),
            protocol: 1,
        })
        .unwrap();
        assert_eq!(
            hello,
            serde_json::json!({ "type": "challenge", "nonce": "n", "protocol": 1 })
        );
        let req = serde_json::to_value(ToNode::Request {
            id: 3,
            request: NodeRequest::Ping,
        })
        .unwrap();
        assert_eq!(
            req,
            serde_json::json!({ "type": "request", "id": 3, "request": { "op": "ping" } })
        );
        let back: ToPortal =
            serde_json::from_value(serde_json::json!({ "type": "heartbeat" })).unwrap();
        assert!(matches!(back, ToPortal::Heartbeat));
        let start = serde_json::to_value(NodeRequest::StartEnvironment {
            environment: EnvironmentSpec {
                id: "e1".into(),
                image: "cha/env-chrome:dev".into(),
                security: SecurityProfile::Browser,
                shm_mb: 1024,
                width: 2560,
                height: 1440,
                fps: 60,
                portal_key: "k".into(),
                home: None,
            },
        })
        .unwrap();
        assert_eq!(start["op"], "start_environment");
        assert_eq!(start["environment"]["security"], "browser");
        assert_eq!(start["environment"]["shmMb"], 1024);
    }

    #[test]
    fn media_tokens_verify_only_as_issued() {
        let portal = NodeKey::from_secret([3u8; 32]);
        let claims = MediaClaims {
            env: "e1".into(),
            sub: "u1".into(),
            role: "owner".into(),
            exp: 1_000,
        };
        let token = sign_media_token(&portal, &claims);
        let key = portal.public_b64();
        assert_eq!(verify_media_token(&key, &token, "e1", 999), Ok(claims));
        assert_eq!(
            verify_media_token(&key, &token, "e1", 1_001),
            Err(MediaTokenError::Expired)
        );
        assert_eq!(
            verify_media_token(&key, &token, "e2", 999),
            Err(MediaTokenError::WrongEnvironment)
        );
        let other = NodeKey::from_secret([4u8; 32]).public_b64();
        assert_eq!(
            verify_media_token(&other, &token, "e1", 999),
            Err(MediaTokenError::Signature)
        );
        // A tampered body breaks the signature.
        let (body, sig) = token.split_once('.').unwrap();
        let forged = format!("{}x.{sig}", &body[..body.len() - 1]);
        assert!(verify_media_token(&key, &forged, "e1", 999).is_err());
        assert!(!token.contains(['+', '/', '=']), "URL-safe");
    }
}
