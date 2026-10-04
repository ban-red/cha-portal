//! ICE servers for players: STUN and TURN from the config. TURN credentials
//! are minted per request with coturn's shared-secret scheme
//! (`use-auth-secret`): the username is `<expiry>:<user id>` and the password
//! base64(HMAC-SHA1(secret, username)), good for a day, so nothing long-lived
//! reaches the browser. Without a secret, no TURN.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha1::Sha1;

use crate::AppState;
use crate::auth::CurrentUser;

/// How long minted TURN credentials work. coturn checks them on every
/// allocation refresh, so this bounds a relayed session's length.
const TURN_CREDENTIAL_SECS: u64 = 24 * 3600;

#[derive(Clone, Debug, Default)]
pub struct IceConfig {
    /// `stun:` URLs, handed out as they are.
    pub stun: Vec<String>,
    /// `turn:`/`turns:` URLs, e.g. `turn:turn.example:3478?transport=udp`.
    pub turn: Vec<String>,
    /// coturn's `static-auth-secret`.
    pub turn_secret: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/ice", get(ice_servers))
}

/// `RTCIceServer`s for this user's next connection.
async fn ice_servers(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> Json<Value> {
    let ice = &state.config.ice;
    let mut servers = Vec::new();
    if !ice.stun.is_empty() {
        servers.push(json!({ "urls": ice.stun }));
    }
    if let (false, Some(secret)) = (ice.turn.is_empty(), &ice.turn_secret) {
        let expiry = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            + TURN_CREDENTIAL_SECS;
        let (username, credential) = turn_credential(secret, expiry, &user.id);
        servers.push(json!({ "urls": ice.turn, "username": username, "credential": credential }));
    }
    Json(json!({ "iceServers": servers }))
}

fn turn_credential(secret: &str, expiry: u64, user: &str) -> (String, String) {
    let username = format!("{expiry}:{user}");
    let credential = hmac_sha1_base64(secret, &username);
    (username, credential)
}

fn hmac_sha1_base64(key: &str, message: &str) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(key.as_bytes()).expect("HMAC takes any key length");
    mac.update(message.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_2202() {
        // Test case 2: effcdf6ae5eb2fa2d27416d5f184df9c259a7c79.
        assert_eq!(
            hmac_sha1_base64("Jefe", "what do ya want for nothing?"),
            "7/zfauXrL6LSdBbV8YTfnCWafHk="
        );
    }

    #[test]
    fn usernames_carry_expiry_and_user() {
        let (user, pass) = turn_credential("north", 1_700_000_000, "alice");
        assert_eq!(user, "1700000000:alice");
        assert_eq!(pass, hmac_sha1_base64("north", "1700000000:alice"));
    }
}
