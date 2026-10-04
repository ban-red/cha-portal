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
//! 4. Node → [`ToPortal::Inventory`], then [`ToPortal::Heartbeat`] every
//!    `heartbeat_secs`. Requests flow both ways.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum NodeResponse {
    Pong { unix_ms: u64 },
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

/// A node's identity key.
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
    }
}
