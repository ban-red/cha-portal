//! The signalling endpoints the portal and player use, as cha-streamer
//! serves them: `/info`, `/streams` and `/webrtc/media`.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use str0m::change::{SdpAnswer, SdpOffer};
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

use crate::host::{Codec, Link};
use crate::hub::Hub;
use crate::session;
use crate::viewers::{Role, Viewers};

/// How long a viewer's offer waits for the host stream to come up (the app
/// starting on the host can take a while).
const STREAM_WAIT: Duration = Duration::from_secs(30);

pub struct AppState {
    pub portal_key: String,
    pub environment: String,
    pub link: Link,
    pub viewers: Arc<Viewers>,
    pub hub: Arc<Hub>,
    pub hosts: Vec<IpAddr>,
    pub public: Vec<IpAddr>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/info", get(info_handler))
        .route("/streams", get(streams_handler))
        .route("/webrtc/media", post(media_offer_handler))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// `GET /info`: what the portal and player need before connecting.
pub fn info_json(state: &AppState) -> Value {
    json!({
        "gateway": true,
        "input": true,
        "audio": true,
        "gamepads": true,
        "pad_kind": "xbox360",
        "hidraw": [],
        "token": true,
        "width": state.width,
        "height": state.height,
        // The host's picture has the size it was launched at.
        "resize": false,
        "fps": state.fps,
        // No WebTransport: the player falls back to WebRTC.
        "wt_port": 0,
        "cert_hash_hex": "",
        "addresses": state.hosts.iter().chain(&state.public).collect::<Vec<_>>(),
    })
}

async fn info_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(info_json(&state))
}

/// The one stream there is, once the host's is up.
async fn streams_handler(State(state): State<Arc<AppState>>) -> Json<Vec<Value>> {
    let Some(stream) = state.link.info() else {
        return Json(Vec::new());
    };
    Json(vec![json!({
        "name": format!("live-{}", stream.codec.name()),
        "codec": stream.codec.name(),
        "codecString": match stream.codec {
            Codec::H264 => "avc1.640033",
            Codec::Hevc => "hev1.1.6.L153.B0",
        },
        "width": stream.width,
        "height": stream.height,
        "chroma": "420",
        "fps": stream.fps,
        "frames": 0,
        "content": "cha-gateway (Moonlight host)",
    })])
}

async fn media_offer_handler(
    State(state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
    body: String,
) -> Result<Json<SdpAnswer>, (StatusCode, String)> {
    // Check the token before touching the body.
    let query = query.unwrap_or_default();
    let presented = query_value(&query, "token").unwrap_or("");
    let role = authorize(&state, presented).map_err(|why| (StatusCode::FORBIDDEN, why))?;
    let bad_request = |err: anyhow::Error| (StatusCode::BAD_REQUEST, format!("{err:#}"));
    let offer: SdpOffer = serde_json::from_str(&body).map_err(|e| bad_request(e.into()))?;
    let name = query_value(&query, "name").ok_or_else(|| bad_request(anyhow!("missing ?name=")))?;
    let wanted = name
        .strip_prefix("live-")
        .filter(|c| matches!(*c, "h264" | "hevc" | "av1"))
        .ok_or_else(|| bad_request(anyhow!("unknown stream {name}")))?;
    let stream = state.link.wait_for_info(STREAM_WAIT).await.ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "the host's stream isn't up yet".to_string(),
        )
    })?;
    // The host's codec is fixed for its session; the track carries it as is.
    if wanted != stream.codec.name() {
        return Err(bad_request(anyhow!(
            "this host streams {}, not {wanted}",
            stream.codec.name()
        )));
    }
    let seat = state
        .viewers
        .join(role)
        .map_err(|why| (StatusCode::SERVICE_UNAVAILABLE, why))?;
    let params = session::SessionParams {
        info: stream,
        link: state.link.clone(),
        hub: Arc::clone(&state.hub),
        public: state.public.clone(),
    };
    let answer = session::start(params, seat, offer).map_err(bad_request)?;
    Ok(Json(answer))
}

/// What role the portal's media token gives, or why it doesn't let this in.
fn authorize(state: &AppState, presented: &str) -> Result<Role, String> {
    match cha_wire::verify_media_token(&state.portal_key, presented, &state.environment, unix_now())
    {
        Ok(claims) => {
            info!(user = %claims.sub, role = %claims.role, "media token accepted");
            Ok(Role::from_claim(&claims.role))
        }
        Err(err) => {
            warn!("rejected a stream request: media token {err}");
            Err(format!("media token {err}"))
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use cha_wire::{MediaClaims, NodeKey, sign_media_token};
    use tower::ServiceExt;

    use super::*;

    async fn state() -> (Arc<AppState>, NodeKey) {
        let portal = NodeKey::from_secret([7u8; 32]);
        let (link, _plumbing) = crate::host::link();
        let hub = Hub::bind(&["127.0.0.1".parse().unwrap()], 0).await.unwrap();
        let state = Arc::new(AppState {
            portal_key: portal.public_b64(),
            environment: "env-1".into(),
            link,
            viewers: Viewers::new(),
            hub,
            hosts: vec!["192.0.2.10".parse().unwrap()],
            public: vec!["198.51.100.4".parse().unwrap()],
            width: 2560,
            height: 1440,
            fps: 60,
        });
        (state, portal)
    }

    fn token(key: &NodeKey, env: &str, exp: i64) -> String {
        sign_media_token(
            key,
            &MediaClaims {
                env: env.into(),
                sub: "u1".into(),
                role: "owner".into(),
                slot: None,
                exp,
            },
        )
    }

    async fn post_offer(app: Router, uri: &str) -> (StatusCode, String) {
        let response = app
            .oneshot(
                Request::post(uri)
                    .body(Body::from("{\"not\":\"an offer\"}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn info_has_the_streamers_shape() {
        let (state, _) = state().await;
        let response = router(state)
            .oneshot(Request::get("/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(info["gateway"], true);
        assert_eq!(info["input"], true);
        assert_eq!(info["audio"], true);
        assert_eq!(info["gamepads"], true);
        assert_eq!(info["hidraw"], json!([]));
        assert_eq!(info["resize"], false);
        assert_eq!(info["wt_port"], 0);
        assert_eq!(info["cert_hash_hex"], "");
        assert_eq!(info["width"], 2560);
        assert_eq!(info["height"], 1440);
        assert_eq!(info["fps"], 60);
        assert_eq!(info["addresses"], json!(["192.0.2.10", "198.51.100.4"]));
    }

    #[tokio::test]
    async fn offers_without_a_valid_token_are_refused() {
        let (state, portal) = state().await;
        let app = router(state);
        let future = unix_now() + 600;
        let other = NodeKey::from_secret([8u8; 32]);
        for (why, uri) in [
            ("no token", "/webrtc/media?name=live-h264".to_string()),
            (
                "garbage",
                "/webrtc/media?name=live-h264&token=abc".to_string(),
            ),
            (
                "wrong environment",
                format!(
                    "/webrtc/media?name=live-h264&token={}",
                    token(&portal, "env-2", future)
                ),
            ),
            (
                "expired",
                format!(
                    "/webrtc/media?name=live-h264&token={}",
                    token(&portal, "env-1", unix_now() - 10)
                ),
            ),
            (
                "another portal's key",
                format!(
                    "/webrtc/media?name=live-h264&token={}",
                    token(&other, "env-1", future)
                ),
            ),
        ] {
            let (status, body) = post_offer(app.clone(), &uri).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{why}: {body}");
        }
    }

    #[tokio::test]
    async fn a_valid_token_gets_past_the_check() {
        let (state, portal) = state().await;
        let uri = format!(
            "/webrtc/media?name=live-h264&token={}",
            token(&portal, "env-1", unix_now() + 600)
        );
        // The body isn't an SDP offer: refused for that, not for the token.
        let (status, _) = post_offer(router(state), &uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn query_values_are_found_by_key() {
        assert_eq!(query_value("name=live-h264&secs=0", "secs"), Some("0"));
        assert_eq!(query_value("name=live-h264", "token"), None);
    }
}
