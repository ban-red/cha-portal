//! `cha-control`: the Cha Portal control plane.
//!
//! P1.1 (foundation): accounts, browser sessions, first-run setup, audit log,
//! and serving the portal SPA. P1.2: node enrollment and the node channel.
//! P1.4: the catalog and environments. Session brokering follows (docs/PLAN.md,
//! Phase 1 delivery plan).

pub mod api;
pub mod auth;
pub mod db;
pub mod environments;
pub mod error;
pub mod nodes;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    /// SQLite database file.
    pub database: PathBuf,
    /// Built portal SPA (`web/apps/portal/dist`); not served if absent.
    pub web_dir: Option<PathBuf>,
    /// Mark the session cookie `Secure` (set when the portal is served over HTTPS).
    pub secure_cookies: bool,
    pub session_days: i64,
}

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<Config>,
    /// One-time token that lets the first admin be created; `None` once set up.
    pub setup_token: Arc<Mutex<Option<String>>>,
    pub nodes: Arc<nodes::NodeHub>,
}

impl AppState {
    /// Opens storage and, when the portal has no accounts yet, opens setup with a
    /// fresh one-time token.
    pub async fn new(config: Config, db: SqlitePool) -> Result<Self> {
        let setup_token = if db::user_count(&db).await? == 0 {
            Some(auth::random_token()?)
        } else {
            None
        };
        Ok(Self {
            db,
            config: Arc::new(config),
            setup_token: Arc::new(Mutex::new(setup_token)),
            nodes: Arc::default(),
        })
    }
}

/// The whole HTTP surface: `/api/*` plus the SPA (any other path serves
/// `index.html`, so client-side routes work on reload).
pub fn app(state: AppState) -> Router {
    let mut router = Router::new().nest("/api", api::routes());
    if let Some(dir) = state
        .config
        .web_dir
        .clone()
        .filter(|d| d.join("index.html").exists())
    {
        router = router
            .fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html"))));
    }
    router.layer(TraceLayer::new_for_http()).with_state(state)
}

pub async fn run(config: Config) -> Result<()> {
    let db = db::open(&config.database).await?;
    let state = AppState::new(config.clone(), db.clone()).await?;
    if let Some(token) = state.setup_token.lock().await.as_deref() {
        warn!(
            "No accounts yet. Open the portal and create the first admin with setup token: {token}"
        );
    }
    if config
        .web_dir
        .as_ref()
        .is_none_or(|d| !d.join("index.html").exists())
    {
        warn!("no built portal SPA found; serving the API only");
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            if let Err(err) = db::delete_expired_sessions(&db).await {
                warn!("pruning sessions: {err}");
            }
        }
    });
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    info!("cha-control listening on http://{}", config.listen);
    axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
