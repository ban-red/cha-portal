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
use axum::http::header::USER_AGENT;
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
}

impl<S: Send + Sync> FromRequestParts<S> for ClientInfo {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self {
            ip: parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(addr)| addr.ip().to_string()),
            user_agent: parts
                .headers
                .get(USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(300).collect()),
            forwarded: ["forwarded", "x-forwarded-for", "x-real-ip"]
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
