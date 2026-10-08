//! The portal's HTTP API as the player uses it. A [`PortalClient`] is bound to
//! one origin (and the token issued by it): every request goes to that origin
//! and redirects are not followed, so a token cannot reach anywhere else.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::origin::{normalize_origin, normalize_origin_with};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Why a portal call failed.
#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    /// The portal doesn't know the token (revoked, or the device was removed).
    #[error("signed out: the portal no longer accepts this device")]
    SignedOut,
    /// The portal answered with its `{"error", "message"}` body.
    #[error("{message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },
    #[error("{0:#}")]
    Other(anyhow::Error),
}

impl PortalError {
    /// The portal's error code (`slow_down`, `invalid_ticket`), if it gave one.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Api { code, .. } => Some(code),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for PortalError {
    fn from(e: reqwest::Error) -> Self {
        Self::Other(describe(e))
    }
}

/// A transport error with its cause chain flattened ("connection refused").
fn describe(e: reqwest::Error) -> anyhow::Error {
    let e = e.without_url();
    let mut text = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    anyhow!(text)
}

/// The signed-in user in a token response.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Grant {
    pub token: String,
    pub device_id: String,
    pub user: GrantUser,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct GrantUser {
    #[serde(default)]
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub role: String,
}

/// `POST /api/device/code`'s answer.
#[derive(Clone, Debug, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    #[serde(default = "default_path")]
    pub verification_path: String,
    #[serde(default = "default_expiry")]
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_path() -> String {
    "/link".into()
}
fn default_expiry() -> u64 {
    600
}
fn default_interval() -> u64 {
    5
}

/// One poll of `POST /api/device/token`.
#[derive(Debug)]
pub enum TokenPoll {
    Pending,
    /// Polled too fast: wait longer before the next.
    SlowDown,
    Approved(Grant),
    Denied,
    Expired,
}

/// What `GET /api/me` says about the caller (the fields we use).
#[derive(Clone, Debug, Deserialize)]
pub struct Me {
    pub username: String,
    #[serde(default)]
    pub role: String,
}

/// The portal's `appearance` pref.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalAppearance {
    System,
    Dark,
    Light,
}

/// The portal's `contrast` pref.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalContrast {
    System,
    Standard,
    More,
}

/// The theme keys of `GET /api/me/prefs`; the other keys (pins, views,
/// motion, transparency) are ignored. A field is `None` when the user hasn't
/// chosen or the value is one this player doesn't know, as the browser's
/// `parsePrefs` falls back to its default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortalTheme {
    /// A theme id, e.g. `cha-jade`.
    pub theme: Option<String>,
    pub appearance: Option<PortalAppearance>,
    pub contrast: Option<PortalContrast>,
}

impl PortalTheme {
    /// Reads the theme keys out of the portal's `{ "prefs": {...} }` answer.
    /// `None` when none of them is set to something usable.
    pub fn from_prefs(prefs: &serde_json::Value) -> Option<Self> {
        let obj = prefs.get("prefs")?.as_object()?;
        let text = |k: &str| obj.get(k).and_then(|v| v.as_str());
        let theme = text("theme")
            .filter(|t| {
                !t.is_empty()
                    && t.len() <= 40
                    && t.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            })
            .map(str::to_string);
        let appearance = match text("appearance") {
            Some("system") => Some(PortalAppearance::System),
            Some("dark") => Some(PortalAppearance::Dark),
            Some("light") => Some(PortalAppearance::Light),
            _ => None,
        };
        let contrast = match text("contrast") {
            Some("system") => Some(PortalContrast::System),
            Some("standard") => Some(PortalContrast::Standard),
            Some("more") => Some(PortalContrast::More),
            _ => None,
        };
        let found = Self {
            theme,
            appearance,
            contrast,
        };
        (found != Self::default()).then_some(found)
    }
}

/// A catalog entry (the fields we use; the portal sends more).
#[derive(Clone, Debug, Deserialize)]
pub struct Template {
    pub id: String,
    pub name: String,
}

/// An environment (the fields we use; the portal sends more).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub id: String,
    pub template_id: String,
    #[serde(default)]
    pub template_name: String,
    /// `starting`, `running`, `stopping`, `destroyed` or `failed`.
    pub state: String,
    /// Why it failed, or what it is doing.
    #[serde(default)]
    pub detail: Option<String>,
    /// The codecs its device encodes, when the node says (`h264`, `hevc`,
    /// `av1`, `pyrowave420`, `pyrowave444`).
    #[serde(default)]
    pub codecs: Option<Vec<String>>,
}

/// `POST /api/environments/{id}/connect` over WebTransport.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamerConnection {
    pub codec: String,
    /// One URL per streamer address, with the media token in the query.
    pub urls: Vec<String>,
    /// SHA-256 of the streamer's certificate, lowercase hex.
    pub cert_hash: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    message: String,
}

/// A client for one portal origin.
#[derive(Clone)]
pub struct PortalClient {
    http: reqwest::Client,
    origin: String,
    token: Option<String>,
}

impl PortalClient {
    /// `origin` as the user gave it (it is normalised, and refused if plain
    /// http to somewhere other than this machine); `token` is sent only to it.
    pub fn new(origin: &str, token: Option<String>) -> Result<Self> {
        Self::build(normalize_origin(origin)?, token)
    }

    /// As [`Self::new`] with the insecure-http rule given rather than read.
    pub fn with_insecure_allowed(
        origin: &str,
        token: Option<String>,
        allow_insecure: bool,
    ) -> Result<Self> {
        Self::build(normalize_origin_with(origin, allow_insecure)?, token)
    }

    fn build(origin: String, token: Option<String>) -> Result<Self> {
        // Needed once per process; a second install is harmless.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("cha-player/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            http,
            origin,
            token,
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    async fn call<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, PortalError> {
        // The URL is always the bound origin plus a path of ours.
        let mut request = self.http.request(method, format!("{}{path}", self.origin));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        if status.is_success() {
            return response.json().await.map_err(|e| {
                PortalError::Other(anyhow!(
                    "unexpected answer from the portal: {}",
                    describe(e)
                ))
            });
        }
        if status == StatusCode::UNAUTHORIZED && self.token.is_some() {
            return Err(PortalError::SignedOut);
        }
        let body: ErrorBody = response.json().await.unwrap_or(ErrorBody {
            error: String::new(),
            message: String::new(),
        });
        let message = if body.message.is_empty() {
            format!("the portal answered {status}")
        } else {
            body.message
        };
        Err(PortalError::Api {
            status: status.as_u16(),
            code: body.error,
            message,
        })
    }

    /// Swaps a `cha://` link's ticket for a device token.
    pub async fn swap_ticket(
        &self,
        ticket: &str,
        install_id: &str,
        name: &str,
    ) -> Result<Grant, PortalError> {
        #[derive(Serialize)]
        struct Body<'a> {
            ticket: &'a str,
            install_id: &'a str,
            name: &'a str,
        }
        self.call(
            Method::POST,
            "/api/device/ticket",
            Some(&Body {
                ticket,
                install_id,
                name,
            }),
        )
        .await
    }

    /// Starts a device-code sign-in.
    pub async fn start_code(
        &self,
        install_id: &str,
        name: &str,
    ) -> Result<DeviceCode, PortalError> {
        #[derive(Serialize)]
        struct Body<'a> {
            install_id: &'a str,
            name: &'a str,
        }
        self.call(
            Method::POST,
            "/api/device/code",
            Some(&Body { install_id, name }),
        )
        .await
    }

    /// Asks once whether the code was approved.
    pub async fn poll_code(&self, device_code: &str) -> Result<TokenPoll, PortalError> {
        #[derive(Serialize)]
        struct Body<'a> {
            device_code: &'a str,
        }
        match self
            .call::<_, Grant>(
                Method::POST,
                "/api/device/token",
                Some(&Body { device_code }),
            )
            .await
        {
            Ok(grant) => Ok(TokenPoll::Approved(grant)),
            Err(PortalError::Api { code, .. }) => match code.as_str() {
                "authorization_pending" => Ok(TokenPoll::Pending),
                "slow_down" => Ok(TokenPoll::SlowDown),
                "access_denied" => Ok(TokenPoll::Denied),
                "expired_token" => Ok(TokenPoll::Expired),
                _ => Err(PortalError::Api {
                    status: 400,
                    message: format!("the portal refused the sign-in ({code})"),
                    code,
                }),
            },
            Err(e) => Err(e),
        }
    }

    pub async fn me(&self) -> Result<Me, PortalError> {
        self.call::<(), _>(Method::GET, "/api/me", None).await
    }

    /// The signed-in user's interface preferences, as the portal stores them.
    pub async fn prefs(&self) -> Result<serde_json::Value, PortalError> {
        self.call::<(), _>(Method::GET, "/api/me/prefs", None).await
    }

    pub async fn catalog(&self) -> Result<Vec<Template>, PortalError> {
        self.call::<(), _>(Method::GET, "/api/catalog", None).await
    }

    /// The caller's environments, newest first.
    pub async fn environments(&self) -> Result<Vec<Environment>, PortalError> {
        self.call::<(), _>(Method::GET, "/api/environments", None)
            .await
    }

    pub async fn environment(&self, id: &str) -> Result<Environment, PortalError> {
        self.call::<(), _>(Method::GET, &format!("/api/environments/{id}"), None)
            .await
    }

    /// Starts an environment of the catalog template `template_id`, placed
    /// by the portal.
    pub async fn launch_environment(&self, template_id: &str) -> Result<Environment, PortalError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            template_id: &'a str,
        }
        self.call(
            Method::POST,
            "/api/environments",
            Some(&Body { template_id }),
        )
        .await
    }

    /// A media token and the streamer's addresses, for `cha-stream/1` over
    /// WebTransport in `codec`. The token lives 60 s: connect at once.
    pub async fn connect_environment(
        &self,
        id: &str,
        codec: &str,
    ) -> Result<StreamerConnection, PortalError> {
        #[derive(Serialize)]
        struct Body<'a> {
            codec: &'a str,
            transport: &'a str,
        }
        self.call(
            Method::POST,
            &format!("/api/environments/{id}/connect"),
            Some(&Body {
                codec,
                transport: "webtransport",
            }),
        )
        .await
    }

    /// Stops the environment (it ends; the app and its session go).
    pub async fn stop_environment(&self, id: &str) -> Result<Environment, PortalError> {
        self.call::<(), _>(Method::DELETE, &format!("/api/environments/{id}"), None)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn theme_keys_are_read_and_the_rest_ignored() {
        let got = PortalTheme::from_prefs(&json!({ "prefs": {
            "theme": "cha-jade", "appearance": "light", "contrast": "more",
            "motion": "reduced", "pinned": ["chrome"], "envView": "list"
        }}))
        .unwrap();
        assert_eq!(got.theme.as_deref(), Some("cha-jade"));
        assert_eq!(got.appearance, Some(PortalAppearance::Light));
        assert_eq!(got.contrast, Some(PortalContrast::More));
    }

    #[test]
    fn unknown_values_fall_back_per_field() {
        let got = PortalTheme::from_prefs(&json!({ "prefs": {
            "theme": "Not Valid", "appearance": "sepia", "contrast": "standard"
        }}))
        .unwrap();
        assert_eq!(got.theme, None);
        assert_eq!(got.appearance, None);
        assert_eq!(got.contrast, Some(PortalContrast::Standard));
    }

    #[test]
    fn no_theme_keys_is_none() {
        assert_eq!(PortalTheme::from_prefs(&json!({ "prefs": {} })), None);
        assert_eq!(
            PortalTheme::from_prefs(&json!({ "prefs": { "pinned": [] } })),
            None
        );
        assert_eq!(PortalTheme::from_prefs(&json!({ "prefs": 3 })), None);
        assert_eq!(PortalTheme::from_prefs(&json!([])), None);
    }
}
