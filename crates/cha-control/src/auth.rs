//! Passwords, browser sessions, and the extractors that require a signed-in
//! user or an admin.
//!
//! Sessions are random 256-bit tokens in an HttpOnly, SameSite=Lax cookie; the
//! database keeps only their SHA-256. Mutating API calls take JSON bodies, which
//! a cross-site form can't send without a CORS preflight, so Lax plus JSON is
//! the CSRF defence.

use std::net::SocketAddr;

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::header::{AUTHORIZATION, USER_AGENT};
use axum::http::request::Parts;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::db::{self, Role, User};
use crate::error::{ApiError, ApiResult};

pub const SESSION_COOKIE: &str = "cha_session";
pub const MIN_PASSWORD_LEN: usize = 3;

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| anyhow::anyhow!("random source: {e}"))?;
    let salt =
        SaltString::encode_b64(&salt).map_err(|e| anyhow::anyhow!("encoding a salt: {e}"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("hashing a password: {e}"))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

/// Spends about as long as a real verification, so a missing user doesn't
/// answer faster than a wrong password.
pub fn verify_dummy(password: &str) {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let hash = DUMMY.get_or_init(|| hash_password("not-a-real-password").unwrap_or_default());
    let _ = verify_password(password, hash);
}

pub fn check_password_policy(password: &str) -> ApiResult<()> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(ApiError::bad_request(
            "weak_password",
            format!("passwords need at least {MIN_PASSWORD_LEN} characters"),
        ));
    }
    Ok(())
}

/// A random URL-safe token with 256 bits of entropy.
pub fn random_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("random source: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Starts a session for `user` and returns the cookie to set.
pub async fn start_session(
    state: &AppState,
    user: &User,
    client: &ClientInfo,
) -> ApiResult<Cookie<'static>> {
    let token = random_token()?;
    let max_age = state.config.session_days * 24 * 3600;
    db::insert_session(
        &state.db,
        &token_hash(&token),
        &user.id,
        db::now() + max_age,
        client.user_agent.as_deref(),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(state.config.secure_cookies)
        .max_age(time::Duration::seconds(max_age))
        .build())
}

pub fn removal_cookie() -> Cookie<'static> {
    let mut cookie = Cookie::from(SESSION_COOKIE);
    cookie.set_path("/");
    cookie
}

/// Where a request came from, for sessions and the audit log.
pub struct ClientInfo {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    /// The request carries a proxy's forwarding header, so `ip` is the
    /// proxy's address rather than the client's.
    pub forwarded: bool,
    /// The request came in on the guest-only listener (ADR 0022).
    pub guest_listener: bool,
}

/// Marks a request as having come in on the guest-only listener, where the
/// client's address is the one the tunnel names in `CF-Connecting-IP`
/// (everything arrives from loopback).
#[derive(Clone, Copy)]
pub struct GuestListener;

impl<S: Send + Sync> FromRequestParts<S> for ClientInfo {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let guest_listener = parts.extensions.get::<GuestListener>().is_some();
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip().to_string());
        let ip = if guest_listener {
            parts
                .headers
                .get("cf-connecting-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<std::net::IpAddr>().ok())
                .map(|ip| ip.to_string())
                .or(peer)
        } else {
            peer
        };
        Ok(Self {
            ip,
            guest_listener,
            user_agent: parts
                .headers
                .get(USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(300).collect()),
            forwarded: guest_listener
                || ["forwarded", "x-forwarded-for", "x-real-ip"]
                    .iter()
                    .any(|h| parts.headers.contains_key(*h)),
        })
    }
}

/// A signed-in user.
pub struct CurrentUser(pub User);

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar.get(SESSION_COOKIE).ok_or(ApiError::Unauthorized)?;
        db::session_user(&state.db, &token_hash(token.value()))
            .await?
            .map(CurrentUser)
            .ok_or(ApiError::Unauthorized)
    }
}

/// What a Cha Player device token starts with.
pub const DEVICE_TOKEN_PREFIX: &str = "chadev_";

/// A fresh device token: `chadev_` and 43 URL-safe characters.
pub fn new_device_token() -> anyhow::Result<String> {
    Ok(format!("{DEVICE_TOKEN_PREFIX}{}", random_token()?))
}

/// A signed-in user on the routes Cha Player uses: a session cookie, or a
/// device token as `Authorization: Bearer chadev_...`. Every other route takes
/// [`CurrentUser`] or [`AdminUser`], which are cookie-only.
pub struct PlayerUser(pub User);

impl FromRequestParts<AppState> for PlayerUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let jar = CookieJar::from_headers(&parts.headers);
        if let Some(token) = jar.get(SESSION_COOKIE)
            && let Some(user) = db::session_user(&state.db, &token_hash(token.value())).await?
        {
            return Ok(PlayerUser(user));
        }
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|t| t.starts_with(DEVICE_TOKEN_PREFIX))
            .ok_or(ApiError::Unauthorized)?;
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip().to_string());
        db::device_user(&state.db, &token_hash(token), ip.as_deref())
            .await?
            .map(PlayerUser)
            .ok_or(ApiError::Unauthorized)
    }
}

/// A signed-in admin.
pub struct AdminUser(pub User);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        if user.role != Role::Admin {
            return Err(ApiError::forbidden("admin_only", "this needs an admin"));
        }
        Ok(AdminUser(user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_round_trip() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("wrong", &hash));
        assert!(!verify_password("anything", "not a hash"));
    }

    #[test]
    fn tokens_are_random_and_hashed() {
        let (a, b) = (random_token().unwrap(), random_token().unwrap());
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert_eq!(token_hash(&a).len(), 64);
        assert_ne!(token_hash(&a), token_hash(&b));
    }
}
