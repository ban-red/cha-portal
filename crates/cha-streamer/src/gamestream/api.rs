//! The local control API: how the node's GameStream host (in the agent)
//! starts and stops this environment's media.
//!
//! ```text
//! POST   /gamestream/session       a SessionHandoff: bind the ports, serve the session
//! DELETE /gamestream/session/{id}  stop it
//! GET    /gamestream/status        {"running": bool, "session_id": 7}
//! ```
//!
//! Every request carries the environment's secret as `Authorization: Bearer`:
//! a handoff holds the session's key, and starting a session moves the
//! environment's picture and input. One session runs at a time; a second POST
//! replaces the first only when it is for the same client (a resume), else 409.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use cha_gamestream::backend::BackendError;
use cha_gamestream::media::MediaError;
use cha_gamestream::{
    ClientId, MediaConfig, MediaPorts, MediaSession, MediaSockets, SessionHandoff,
};
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::BoxFuture;
use super::backend::EngineBackend;

/// Why a session didn't start.
#[derive(Debug)]
pub enum StartError {
    /// The stream asks for something this environment doesn't make.
    Refused(String),
    Failed(String),
}

/// A started session.
pub trait Running: Send + Sync + 'static {
    /// Ends it and returns when its sockets are free.
    fn stop(&self) -> BoxFuture<'_, ()>;
    /// Resolves when it has ended, by itself or not.
    fn ended(&self) -> BoxFuture<'static, ()>;
}

/// What starts a session for a handoff.
pub trait Launcher: Send + Sync + 'static {
    fn start(&self, handoff: SessionHandoff)
    -> BoxFuture<'_, Result<Arc<dyn Running>, StartError>>;
}

struct Active {
    id: u64,
    client: ClientId,
    running: Arc<dyn Running>,
}

/// The one session an environment runs.
pub struct Registry {
    launcher: Arc<dyn Launcher>,
    active: Mutex<Option<Active>>,
}

enum ApiError {
    Conflict,
    NotFound,
    Start(StartError),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Conflict => (
                StatusCode::CONFLICT,
                "another client's GameStream session runs here".to_string(),
            ),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "no such session".to_string()),
            ApiError::Start(StartError::Refused(why)) => (StatusCode::UNPROCESSABLE_ENTITY, why),
            ApiError::Start(StartError::Failed(why)) => (StatusCode::INTERNAL_SERVER_ERROR, why),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl Registry {
    pub fn new(launcher: Arc<dyn Launcher>) -> Arc<Self> {
        Arc::new(Self {
            launcher,
            active: Mutex::new(None),
        })
    }

    async fn start(self: &Arc<Self>, handoff: SessionHandoff) -> Result<(), ApiError> {
        let mut active = self.active.lock().await;
        let id = handoff.session_id;
        let client = handoff.params.client.clone();
        if active.as_ref().is_some_and(|a| a.client != client) {
            warn!(session = id, "refused a second client's GameStream session");
            return Err(ApiError::Conflict);
        }
        // The same client again (it resumed): the old media goes first, so
        // its ports are free for the new.
        if let Some(old) = active.take() {
            info!(
                session = old.id,
                "replacing the client's GameStream session"
            );
            old.running.stop().await;
        }
        let running = self
            .launcher
            .start(handoff)
            .await
            .map_err(ApiError::Start)?;
        let ended = running.ended();
        let registry = Arc::clone(self);
        let watched = Arc::clone(&running);
        tokio::spawn(async move {
            ended.await;
            registry.forget(&watched).await;
        });
        *active = Some(Active {
            id,
            client,
            running,
        });
        info!(session = id, "GameStream session started");
        Ok(())
    }

    /// `running` ended by itself: the environment has no session now.
    async fn forget(&self, running: &Arc<dyn Running>) {
        let mut active = self.active.lock().await;
        if active
            .as_ref()
            .is_some_and(|a| Arc::ptr_eq(&a.running, running))
        {
            *active = None;
        }
    }

    async fn stop(&self, id: u64) -> Result<(), ApiError> {
        let mut active = self.active.lock().await;
        if active.as_ref().is_none_or(|a| a.id != id) {
            return Err(ApiError::NotFound);
        }
        if let Some(a) = active.take() {
            a.running.stop().await;
            info!(session = id, "GameStream session stopped");
        }
        Ok(())
    }

    async fn status(&self) -> serde_json::Value {
        let active = self.active.lock().await;
        json!({
            "running": active.is_some(),
            "session_id": active.as_ref().map(|a| a.id),
        })
    }
}

/// The secret, compared without telling how much of a guess was right.
pub struct Bearer(String);

impl Bearer {
    pub fn new(secret: String) -> Self {
        Self(secret)
    }

    fn matches(&self, presented: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), presented.as_bytes());
        let mut diff = a.len() ^ b.len();
        for i in 0..a.len().max(b.len()) {
            diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
        }
        diff == 0
    }
}

/// The routes, to merge into the streamer's router.
pub fn router(registry: Arc<Registry>, secret: Bearer) -> Router {
    Router::new()
        .route("/gamestream/session", post(start))
        .route("/gamestream/session/{id}", delete(stop))
        .route("/gamestream/status", get(status))
        .layer(middleware::from_fn_with_state(
            Arc::new(secret),
            require_secret,
        ))
        .with_state(registry)
}

async fn require_secret(
    State(secret): State<Arc<Bearer>>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(token) if secret.matches(token) => next.run(request).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(json!({ "error": "missing or wrong secret" })),
        )
            .into_response(),
    }
}

async fn start(
    State(registry): State<Arc<Registry>>,
    Json(handoff): Json<SessionHandoff>,
) -> Result<Json<serde_json::Value>, ApiError> {
    registry.start(handoff).await?;
    Ok(Json(json!({})))
}

async fn stop(
    State(registry): State<Arc<Registry>>,
    Path(id): Path<u64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    registry.stop(id).await?;
    Ok(Json(json!({})))
}

async fn status(State(registry): State<Arc<Registry>>) -> Json<serde_json::Value> {
    Json(registry.status().await)
}

/// Starts sessions on the engine, on the ports the streamer was given.
pub struct EngineLauncher {
    backend: Arc<EngineBackend>,
    bind: IpAddr,
    ports: MediaPorts,
}

impl EngineLauncher {
    pub fn new(backend: EngineBackend, bind: IpAddr, ports: MediaPorts) -> Self {
        Self {
            backend: Arc::new(backend),
            bind,
            ports,
        }
    }
}

impl Launcher for EngineLauncher {
    fn start(
        &self,
        handoff: SessionHandoff,
    ) -> BoxFuture<'_, Result<Arc<dyn Running>, StartError>> {
        Box::pin(async move {
            let sockets = MediaSockets::bind(self.bind, self.ports)
                .await
                .map_err(|e| StartError::Failed(format!("binding {:?}: {e}", self.ports)))?;
            let session = MediaSession::start(
                handoff,
                self.backend.clone(),
                sockets,
                MediaConfig::default(),
            )
            .await
            .map_err(|e| match e {
                MediaError::Backend(BackendError::Unsupported(what)) => {
                    StartError::Refused(format!("this environment can't serve it: {what}"))
                }
                other => StartError::Failed(other.to_string()),
            })?;
            Ok(Arc::new(Live(Arc::new(session))) as Arc<dyn Running>)
        })
    }
}

struct Live(Arc<MediaSession>);

impl Running for Live {
    fn stop(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.0.stop())
    }

    fn ended(&self) -> BoxFuture<'static, ()> {
        let session = Arc::clone(&self.0);
        Box::pin(async move {
            let reason = session.closed().await;
            session.join().await;
            info!(?reason, "GameStream media ended");
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::body::Body;
    use cha_gamestream::{AudioParams, Encryption, SessionKeys, StreamParams, VideoCodec};
    use tokio::sync::watch;
    use tower::ServiceExt;

    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    /// A session that runs until stopped or told to end.
    struct Fake {
        end: watch::Sender<bool>,
        stops: Arc<AtomicUsize>,
    }

    impl Running for Fake {
        fn stop(&self) -> BoxFuture<'_, ()> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.end.send_replace(true);
            Box::pin(async {})
        }
        fn ended(&self) -> BoxFuture<'static, ()> {
            let mut end = self.end.subscribe();
            Box::pin(async move {
                let _ = end.wait_for(|e| *e).await;
            })
        }
    }

    #[derive(Default)]
    struct FakeLauncher {
        starts: AtomicUsize,
        stops: Arc<AtomicUsize>,
        refuse: std::sync::atomic::AtomicBool,
        sessions: std::sync::Mutex<Vec<watch::Sender<bool>>>,
    }

    impl Launcher for FakeLauncher {
        fn start(
            &self,
            _handoff: SessionHandoff,
        ) -> BoxFuture<'_, Result<Arc<dyn Running>, StartError>> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let refuse = self.refuse.load(Ordering::SeqCst);
            let (end, _) = watch::channel(false);
            self.sessions.lock().unwrap().push(end.clone());
            let fake = Fake {
                end,
                stops: Arc::clone(&self.stops),
            };
            Box::pin(async move {
                if refuse {
                    Err(StartError::Refused("AV1".into()))
                } else {
                    Ok(Arc::new(fake) as Arc<dyn Running>)
                }
            })
        }
    }

    fn handoff(session_id: u64, client: &str) -> SessionHandoff {
        SessionHandoff {
            session_id,
            keys: SessionKeys {
                key: [1; 16],
                key_id: 5,
            },
            encryption: Encryption {
                control: true,
                video: false,
                audio: false,
            },
            control_connect_data: 7,
            ping_payload: [2; 16],
            params: StreamParams {
                client: ClientId(client.into()),
                client_ip: "192.168.1.9".parse().unwrap(),
                app_id: 1,
                width: 1920,
                height: 1080,
                fps: 60,
                bitrate_bps: 20_000_000,
                codec: VideoCodec::H264,
                hdr: false,
                chroma: cha_gamestream::handoff::Chroma::Yuv420,
                full_range: false,
                max_ref_frames: 1,
                packet_size: 1392,
                fec_percent: 20,
                min_fec_packets: 0,
                audio: AudioParams::select(2, 0x3, false, 5),
            },
        }
    }

    fn app(launcher: &Arc<FakeLauncher>) -> Router {
        router(Registry::new(launcher.clone()), Bearer::new(SECRET.into()))
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<&SessionHandoff>,
    ) -> StatusCode {
        let mut request = axum::http::Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let request = match body {
            Some(handoff) => request
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(handoff).unwrap())),
            None => request.body(Body::empty()),
        }
        .unwrap();
        app.clone().oneshot(request).await.unwrap().status()
    }

    async fn running(app: &Router) -> serde_json::Value {
        let request = axum::http::Request::builder()
            .uri("/gamestream/status")
            .header("authorization", format!("Bearer {SECRET}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn every_route_needs_the_secret() {
        let launcher = Arc::new(FakeLauncher::default());
        let app = app(&launcher);
        let h = handoff(1, "aa");
        for (method, uri, body) in [
            ("POST", "/gamestream/session", Some(&h)),
            ("DELETE", "/gamestream/session/1", None),
            ("GET", "/gamestream/status", None),
        ] {
            for token in [
                None,
                Some(""),
                Some("wrong"),
                Some(&SECRET[..31]),
                Some("0123456789abcdef0123456789abcdeff"),
            ] {
                assert_eq!(
                    call(&app, method, uri, token, body).await,
                    StatusCode::UNAUTHORIZED,
                    "{method} {uri} with {token:?}"
                );
            }
        }
        assert_eq!(launcher.starts.load(Ordering::SeqCst), 0, "nothing started");
        assert_eq!(
            call(&app, "GET", "/gamestream/status", Some(SECRET), None).await,
            StatusCode::OK
        );
        // Only the Bearer scheme counts.
        let request = axum::http::Request::builder()
            .uri("/gamestream/status")
            .header("authorization", SECRET)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn a_session_starts_shows_in_the_status_and_stops() {
        let launcher = Arc::new(FakeLauncher::default());
        let app = app(&launcher);
        assert_eq!(running(&app).await["running"], false);
        let h = handoff(7, "aa");
        assert_eq!(
            call(&app, "POST", "/gamestream/session", Some(SECRET), Some(&h)).await,
            StatusCode::OK
        );
        let status = running(&app).await;
        assert_eq!(status["running"], true);
        assert_eq!(status["session_id"], 7);
        assert_eq!(
            call(&app, "DELETE", "/gamestream/session/8", Some(SECRET), None).await,
            StatusCode::NOT_FOUND,
            "another session's id"
        );
        assert_eq!(running(&app).await["running"], true);
        assert_eq!(
            call(&app, "DELETE", "/gamestream/session/7", Some(SECRET), None).await,
            StatusCode::OK
        );
        assert_eq!(launcher.stops.load(Ordering::SeqCst), 1);
        assert_eq!(running(&app).await["running"], false);
        assert_eq!(
            call(&app, "DELETE", "/gamestream/session/7", Some(SECRET), None).await,
            StatusCode::NOT_FOUND,
            "already gone"
        );
    }

    #[tokio::test]
    async fn the_same_client_replaces_its_session_another_gets_a_conflict() {
        let launcher = Arc::new(FakeLauncher::default());
        let app = app(&launcher);
        let post = |h: SessionHandoff| {
            let app = app.clone();
            async move { call(&app, "POST", "/gamestream/session", Some(SECRET), Some(&h)).await }
        };
        assert_eq!(post(handoff(1, "aa")).await, StatusCode::OK);
        assert_eq!(
            post(handoff(2, "bb")).await,
            StatusCode::CONFLICT,
            "another client"
        );
        assert_eq!(launcher.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            launcher.stops.load(Ordering::SeqCst),
            0,
            "the first runs on"
        );
        assert_eq!(running(&app).await["session_id"], 1);
        assert_eq!(post(handoff(3, "aa")).await, StatusCode::OK, "a resume");
        assert_eq!(launcher.starts.load(Ordering::SeqCst), 2);
        assert_eq!(
            launcher.stops.load(Ordering::SeqCst),
            1,
            "the old one stopped first"
        );
        assert_eq!(running(&app).await["session_id"], 3);
    }

    #[tokio::test]
    async fn a_refused_stream_leaves_no_session() {
        let launcher = Arc::new(FakeLauncher::default());
        launcher.refuse.store(true, Ordering::SeqCst);
        let app = app(&launcher);
        assert_eq!(
            call(
                &app,
                "POST",
                "/gamestream/session",
                Some(SECRET),
                Some(&handoff(1, "aa"))
            )
            .await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(running(&app).await["running"], false);
        // A bad body is the caller's mistake.
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/gamestream/session")
            .header("authorization", format!("Bearer {SECRET}"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        assert!(
            app.clone()
                .oneshot(request)
                .await
                .unwrap()
                .status()
                .is_client_error()
        );
    }

    #[tokio::test]
    async fn a_session_that_ends_by_itself_frees_the_environment() {
        let launcher = Arc::new(FakeLauncher::default());
        let app = app(&launcher);
        assert_eq!(
            call(
                &app,
                "POST",
                "/gamestream/session",
                Some(SECRET),
                Some(&handoff(1, "aa"))
            )
            .await,
            StatusCode::OK
        );
        launcher.sessions.lock().unwrap()[0].send_replace(true);
        tokio::time::timeout(Duration::from_secs(2), async {
            while running(&app).await["running"] == true {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the registry forgets it");
        // Another client may now have it.
        assert_eq!(
            call(
                &app,
                "POST",
                "/gamestream/session",
                Some(SECRET),
                Some(&handoff(2, "bb"))
            )
            .await,
            StatusCode::OK
        );
    }

    #[test]
    fn the_secret_compares_whole() {
        let secret = Bearer::new("abc".into());
        assert!(secret.matches("abc"));
        assert!(
            !secret.matches("abd")
                && !secret.matches("ab")
                && !secret.matches("abcd")
                && !secret.matches("")
        );
    }
}
