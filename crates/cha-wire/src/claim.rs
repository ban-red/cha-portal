//! Claiming a node found on the LAN (ADR 0007): the messages on the node's
//! claim port and the SPAKE2 exchange both ends run, so they can't drift.
//!
//! The node shows an 8-digit pairing code; an admin types it into the portal.
//! Each side runs SPAKE2 with the code as the password (portal is A, node is
//! B), so neither learns the code from the other's messages and a wrong code
//! gives different keys. HMACs over the transcript, made with the shared key,
//! then show that each side knew the code and saw the same messages:
//!
//! 1. `POST /claim/start` ([`StartRequest`] → [`StartResponse`]): the portal's
//!    SPAKE2 message and URL; the node's message and a random claim id.
//! 2. `POST /claim/finish` ([`FinishRequest`] → [`FinishResponse`]): the
//!    portal's MAC; once it checks, the node's key, name and MAC.
//! 3. `POST /claim/done` ([`DoneRequest`]): the portal records the key as
//!    enrolled and sends the node's id with a MAC; the node saves its identity.
//!
//! Failures are non-2xx with an [`ErrorBody`].

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};

type HmacSha256 = Hmac<Sha256>;

/// The mDNS service an unclaimed node advertises.
pub const MDNS_SERVICE: &str = "_cha-node._tcp.local.";
/// Where an unclaimed node listens for a claim (`CHA_CLAIM_PORT`).
pub const DEFAULT_CLAIM_PORT: u16 = 7679;
pub const START_PATH: &str = "/claim/start";
pub const FINISH_PATH: &str = "/claim/finish";
pub const DONE_PATH: &str = "/claim/done";

/// Error codes in an [`ErrorBody`].
pub mod code {
    pub const WRONG_CODE: &str = "wrong_code";
    pub const COOLING_DOWN: &str = "cooling_down";
    pub const BUSY: &str = "busy";
    pub const INSECURE_PORTAL: &str = "insecure_portal";
    pub const UNKNOWN_CLAIM: &str = "unknown_claim";
    pub const BAD_REQUEST: &str = "bad_request";
}

const PORTAL_IDENTITY: &[u8] = b"cha-portal";
const NODE_IDENTITY: &[u8] = b"cha-node";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRequest {
    /// The portal's SPAKE2 message, base64.
    pub spake: String,
    /// The URL the admin is using; the transcript covers it.
    pub portal_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResponse {
    pub claim_id: String,
    /// The node's SPAKE2 message, base64.
    pub spake: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishRequest {
    pub claim_id: String,
    /// [`ClaimSession::portal_mac`], base64.
    pub mac: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishResponse {
    /// The node's Ed25519 public key, base64.
    pub public_key: String,
    pub name: String,
    pub agent_version: String,
    /// [`ClaimSession::node_mac`], base64.
    pub mac: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoneRequest {
    pub claim_id: String,
    pub node_id: String,
    /// [`ClaimSession::done_mac`], base64.
    pub mac: String,
}

/// What the node's claim port says when it refuses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ClaimError {
    #[error("the pairing code is 8 digits")]
    BadCode,
    #[error("the key exchange failed: {0}")]
    Exchange(String),
    #[error("not valid base64")]
    Base64,
    #[error("the confirmation doesn't match: wrong code, or the messages were altered")]
    Mac,
    #[error("random source: {0}")]
    Random(String),
}

/// A fresh 8-digit code from the OS's random source, digits only.
pub fn generate_code() -> Result<String, ClaimError> {
    // Rejection sampling: 10^8 doesn't divide 2^32, and a biased code is a
    // (tiny) head start for a guesser.
    const LIMIT: u32 = u32::MAX - (u32::MAX % 100_000_000);
    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).map_err(|e| ClaimError::Random(e.to_string()))?;
        let n = u32::from_le_bytes(bytes);
        if n < LIMIT {
            return Ok(format!("{:08}", n % 100_000_000));
        }
    }
}

/// `48219375` → `4821-9375`.
pub fn format_code(digits: &str) -> String {
    match digits.len() {
        8 => format!("{}-{}", &digits[..4], &digits[4..]),
        _ => digits.to_string(),
    }
}

/// The digits of a code as a person types it: `48219375`, `4821-9375` or
/// `4821 9375`, with spaces around. Anything else is `None`.
pub fn normalize_code(input: &str) -> Option<String> {
    let input = input.trim();
    if !input.is_ascii() {
        return None;
    }
    let digits = match input.as_bytes() {
        [_, _, _, _, b'-' | b' ', ..] if input.len() == 9 => {
            format!("{}{}", &input[..4], &input[5..])
        }
        _ => input.to_string(),
    };
    (digits.len() == 8 && digits.bytes().all(|b| b.is_ascii_digit())).then_some(digits)
}

/// The first 16 hex characters of SHA-256 of a raw public key: what the node
/// advertises and the portal shows, to tell nodes apart.
pub fn fingerprint(public_key: &[u8]) -> String {
    Sha256::digest(public_key)[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// [`fingerprint`] of a base64 public key; `None` if it isn't base64.
pub fn fingerprint_b64(public_key: &str) -> Option<String> {
    STANDARD.decode(public_key).ok().map(|k| fingerprint(&k))
}

fn password(code: &str) -> Result<Password, ClaimError> {
    let digits = normalize_code(code).ok_or(ClaimError::BadCode)?;
    Ok(Password::new(digits.as_bytes()))
}

fn identities() -> (Identity, Identity) {
    (Identity::new(PORTAL_IDENTITY), Identity::new(NODE_IDENTITY))
}

fn decode(b64: &str) -> Result<Vec<u8>, ClaimError> {
    STANDARD.decode(b64).map_err(|_| ClaimError::Base64)
}

/// The portal's half of the exchange, between sending its message and getting
/// the node's.
pub struct PortalStart {
    state: Spake2<Ed25519Group>,
    message: Vec<u8>,
}

impl PortalStart {
    /// Starts with the code the admin typed. [`Self::message`] goes to the node.
    pub fn new(code: &str) -> Result<Self, ClaimError> {
        let (a, b) = identities();
        let (state, message) = Spake2::<Ed25519Group>::start_a(&password(code)?, &a, &b);
        Ok(Self { state, message })
    }

    /// The SPAKE2 message, base64.
    pub fn message(&self) -> String {
        STANDARD.encode(&self.message)
    }

    /// Finishes with the node's reply to `/claim/start`.
    pub fn finish(
        self,
        node_spake: &str,
        portal_url: &str,
        claim_id: &str,
    ) -> Result<ClaimSession, ClaimError> {
        let node_message = decode(node_spake)?;
        let key = self
            .state
            .finish(&node_message)
            .map_err(|e| ClaimError::Exchange(e.to_string()))?;
        Ok(ClaimSession::new(
            key,
            &self.message,
            &node_message,
            portal_url,
            claim_id,
        ))
    }
}

/// The node's half: answers the portal's message with its own, and has the
/// shared key at once.
pub struct NodeStart;

impl NodeStart {
    /// Returns the node's SPAKE2 message (base64) and the session.
    pub fn respond(
        code: &str,
        portal_spake: &str,
        portal_url: &str,
        claim_id: &str,
    ) -> Result<(String, ClaimSession), ClaimError> {
        let portal_message = decode(portal_spake)?;
        let (a, b) = identities();
        let (state, message) = Spake2::<Ed25519Group>::start_b(&password(code)?, &a, &b);
        let key = state
            .finish(&portal_message)
            .map_err(|e| ClaimError::Exchange(e.to_string()))?;
        let session = ClaimSession::new(key, &portal_message, &message, portal_url, claim_id);
        Ok((STANDARD.encode(&message), session))
    }
}

/// The shared key and what both sides saw, from which each makes and checks
/// the confirmations.
pub struct ClaimSession {
    key: Vec<u8>,
    transcript: Vec<u8>,
}

impl std::fmt::Debug for ClaimSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimSession").finish_non_exhaustive()
    }
}

impl ClaimSession {
    fn new(
        key: Vec<u8>,
        portal_message: &[u8],
        node_message: &[u8],
        portal_url: &str,
        claim_id: &str,
    ) -> Self {
        // Length-prefixed, so no field can run into the next.
        let mut transcript = Vec::new();
        for field in [
            portal_message,
            node_message,
            portal_url.as_bytes(),
            claim_id.as_bytes(),
        ] {
            transcript.extend_from_slice(&(field.len() as u32).to_be_bytes());
            transcript.extend_from_slice(field);
        }
        Self { key, transcript }
    }

    fn mac(&self, label: &str, extra: &[u8]) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC takes any key length");
        mac.update(label.as_bytes());
        mac.update(&self.transcript);
        mac.update(extra);
        mac
    }

    fn make(&self, label: &str, extra: &[u8]) -> String {
        STANDARD.encode(self.mac(label, extra).finalize().into_bytes())
    }

    fn check(&self, label: &str, extra: &[u8], mac_b64: &str) -> Result<(), ClaimError> {
        let given = decode(mac_b64)?;
        self.mac(label, extra)
            .verify_slice(&given)
            .map_err(|_| ClaimError::Mac)
    }

    /// The portal's proof that it knows the code (base64).
    pub fn portal_mac(&self) -> String {
        self.make("cha-claim portal", &[])
    }

    pub fn verify_portal_mac(&self, mac: &str) -> Result<(), ClaimError> {
        self.check("cha-claim portal", &[], mac)
    }

    /// The node's proof, covering its public key (base64 in, base64 out).
    pub fn node_mac(&self, public_key: &str) -> Result<String, ClaimError> {
        Ok(self.make("cha-claim node", &decode(public_key)?))
    }

    pub fn verify_node_mac(&self, public_key: &str, mac: &str) -> Result<(), ClaimError> {
        self.check("cha-claim node", &decode(public_key)?, mac)
    }

    /// The portal's confirmation of the node id it recorded.
    pub fn done_mac(&self, node_id: &str) -> String {
        self.make("cha-claim done", node_id.as_bytes())
    }

    pub fn verify_done_mac(&self, node_id: &str, mac: &str) -> Result<(), ClaimError> {
        self.check("cha-claim done", node_id.as_bytes(), mac)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeKey;

    const URL: &str = "https://portal.example";

    /// Both ends with `portal_code` and `node_code`, as far as `/claim/start`.
    fn start(
        portal_code: &str,
        node_code: &str,
        node_url: &str,
    ) -> Result<(ClaimSession, ClaimSession), ClaimError> {
        let portal = PortalStart::new(portal_code)?;
        let (node_spake, node) = NodeStart::respond(node_code, &portal.message(), node_url, "c1")?;
        let portal = portal.finish(&node_spake, URL, "c1")?;
        Ok((portal, node))
    }

    fn key() -> String {
        NodeKey::from_secret([7; 32]).public_b64()
    }

    #[test]
    fn the_right_code_gives_both_ends_the_same_key() {
        let (portal, node) = start("48219375", "4821-9375", URL).unwrap();
        assert_eq!(portal.key, node.key);
        node.verify_portal_mac(&portal.portal_mac()).unwrap();
        let pk = key();
        portal
            .verify_node_mac(&pk, &node.node_mac(&pk).unwrap())
            .unwrap();
        node.verify_done_mac("n1", &portal.done_mac("n1")).unwrap();
        assert!(node.verify_done_mac("n2", &portal.done_mac("n1")).is_err());
    }

    #[test]
    fn a_wrong_code_fails_the_portal_check() {
        let (portal, node) = start("48219375", "48219376", URL).unwrap();
        assert_ne!(portal.key, node.key);
        assert!(node.verify_portal_mac(&portal.portal_mac()).is_err());
    }

    #[test]
    fn a_changed_url_fails_the_portal_check() {
        let (portal, node) = start("48219375", "48219375", "https://evil.example").unwrap();
        assert!(node.verify_portal_mac(&portal.portal_mac()).is_err());
    }

    #[test]
    fn a_swapped_public_key_fails_the_node_check() {
        let (portal, node) = start("48219375", "48219375", URL).unwrap();
        let mac = node.node_mac(&key()).unwrap();
        let other = NodeKey::from_secret([8; 32]).public_b64();
        assert!(portal.verify_node_mac(&other, &mac).is_err());
        assert!(portal.verify_node_mac(&key(), "not base64!").is_err());
    }

    #[test]
    fn codes_normalize_and_format() {
        for input in ["48219375", "4821-9375", "4821 9375", "  4821-9375 "] {
            assert_eq!(
                normalize_code(input).as_deref(),
                Some("48219375"),
                "{input}"
            );
        }
        for input in [
            "",
            "4821937",
            "482193750",
            "4821_9375",
            "4821--375",
            "abcd-efgh",
            "4821-93a5",
        ] {
            assert_eq!(normalize_code(input), None, "{input}");
        }
        assert_eq!(format_code("48219375"), "4821-9375");
        assert_eq!(format_code("00000042"), "0000-0042");
        let code = generate_code().unwrap();
        assert_eq!(normalize_code(&format_code(&code)), Some(code));
    }

    #[test]
    fn fingerprints_are_16_hex_characters() {
        let fp = fingerprint(&[1; 32]);
        assert_eq!(fp.len(), 16);
        assert!(fp.bytes().all(|b| b.is_ascii_hexdigit()));
        let raw = STANDARD.decode(key()).unwrap();
        assert_eq!(fingerprint_b64(&key()), Some(fingerprint(&raw)));
        assert_eq!(fingerprint_b64("not base64!"), None);
    }
}
