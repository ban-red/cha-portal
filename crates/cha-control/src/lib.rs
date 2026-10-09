//! `cha-control`: the Cha Portal control plane.
//!
//! P1.1 (foundation): accounts, browser sessions, first-run setup, audit log,
//! and serving the portal SPA. P1.2: node enrollment and the node channel.
//! P1.4: the catalog and environments. Session brokering follows (docs/PLAN.md,
//! Phase 1 delivery plan). App data (Phase 3): what apps keep and share.
//! Controllers: which virtual gamepad an app gets. Apps: the frame rate it runs at.

pub mod access;
pub mod api;
pub mod apps;
pub mod auth;
pub mod catalogs;
pub mod claim;
pub mod controllers;
pub mod custom;
pub mod db;
pub mod devices;
pub mod discovery;
pub mod environments;
pub mod error;
pub mod gamestream;
pub mod ice;
pub mod moonlight;
pub mod nodes;
pub mod placement;
pub mod prefs;
pub mod relay;
pub mod settings;
pub mod shares;
pub mod storage;
pub mod tunnel;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::routing::{get, post};
use axum::{Extension, Router};
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
    /// STUN and TURN for players.
    pub ice: ice::IceConfig,
    /// Offer "Login as Local Dev" to loopback clients: it creates (or reuses)
    /// the `dev` admin and signs in without a password, or signs in as any
    /// existing enabled admin. Never for a real portal.
    pub dev_login: bool,
    /// Browse the LAN for unclaimed nodes (ADR 0007).
    pub discover_nodes: bool,
    /// The URL nodes are told to reach the portal at when claimed; without
    /// it, the one the claiming admin's browser is using.
    pub public_url: Option<String>,
    /// Internet share links: the Cloudflare Tunnel and the listener it
    /// reaches (ADR 0022).
    pub tunnel: tunnel::TunnelConfig,
}

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<Config>,
    /// Held while the first account is created, so two visitors can't both
    /// claim a fresh portal.
    pub setup_lock: Arc<Mutex<()>>,
    pub nodes: Arc<nodes::NodeHub>,
    /// Signs media tokens; nodes' streamers check them with its public half.
    pub media_key: Arc<cha_wire::NodeKey>,
    /// Unclaimed nodes seen on the LAN.
    pub discovered: Arc<discovery::Discovered>,
    /// Moonlight hosts the nodes see, and pairings in progress (ADR 0008).
    pub moonlight: Arc<moonlight::State>,
    /// Pairing requests from the nodes' GameStream hosts (ADR 0009).
    pub gamestream: Arc<gamestream::State>,
    /// Requests to the share links' unauthenticated routes, per address.
    pub share_limit: Arc<shares::RateLimit>,
    /// Catalogs an admin loaded (ADR 0019).
    pub catalogs: Arc<catalogs::Registry>,
    /// Custom environments an admin made (ADR 0021).
    pub customs: Arc<custom::Registry>,
    /// The `cloudflared` child that serves internet share links (ADR 0022).
    pub tunnel: Arc<tunnel::Tunnel>,
    /// Tickets for WebSocket media and the relays waiting on their nodes.
    pub relays: Arc<relay::Relays>,
}

impl AppState {
    /// Opens storage. A portal with no accounts yet is open to claim: the
    /// first visitor creates the first admin.
    pub async fn new(config: Config, db: SqlitePool) -> Result<Self> {
        let media_key = Arc::new(media_key(&db).await?);
        let catalogs = catalogs::Registry::default();
        catalogs.reload(&db).await?;
        let customs = custom::Registry::default();
        customs.reload(&db).await?;
        let tunnel = tunnel::Tunnel::new(config.tunnel.clone());
        Ok(Self {
            db,
            config: Arc::new(config),
            setup_lock: Arc::default(),
            nodes: Arc::default(),
            media_key,
            discovered: Arc::default(),
            moonlight: Arc::default(),
            gamestream: Arc::default(),
            share_limit: Arc::default(),
            catalogs: Arc::new(catalogs),
            customs: Arc::new(customs),
            tunnel: Arc::new(tunnel),
            relays: Arc::default(),
        })
    }
}

/// The portal's media-token signing key, created on first start and kept in
/// the database (its public half reaches every streamer it starts).
async fn media_key(db: &SqlitePool) -> Result<cha_wire::NodeKey> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    let mut fresh = [0u8; 32];
    getrandom::fill(&mut fresh).map_err(|e| anyhow::anyhow!("random source: {e}"))?;
    let stored = db::setting_or_insert(db, "media_signing_key", &STANDARD.encode(fresh)).await?;
    let secret: [u8; 32] = STANDARD
        .decode(stored)
        .ok()
        .and_then(|b| b.try_into().ok())
        .context("the stored media signing key is corrupt")?;
    Ok(cha_wire::NodeKey::from_secret(secret))
}

/// The whole HTTP surface: `/api/*` plus the SPA (any other path serves
/// `index.html`, so client-side routes work on reload).
pub fn app(state: AppState) -> Router {
    let router = Router::new().nest("/api", api::routes());
    serve_spa(router, &state)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// What the Cloudflare Tunnel reaches (ADR 0022): the SPA's static files and
/// the guest routes of internet share links, and nothing else. No sign-in, no
/// dev login, no admin API, no node channel; `/api` is a 404 beyond the four
/// routes, and a share that isn't an internet link is unknown here. The
/// client's address is `CF-Connecting-IP` ([`auth::GuestListener`]).
pub fn guest_app(state: AppState) -> Router {
    let api = Router::new()
        .route("/shares/{token}", get(shares::info))
        .route("/shares/{token}/ice", get(shares::ice))
        .route("/shares/{token}/connect", post(shares::connect))
        .route("/media/{ticket}", get(relay::media))
        .fallback(|| async { error::ApiError::NotFound("no such API".into()) });
    let router = Router::new().nest("/api", api);
    serve_spa(router, &state)
        .layer(Extension(auth::GuestListener))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

fn serve_spa(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    match state
        .config
        .web_dir
        .clone()
        .filter(|d| d.join("index.html").exists())
    {
        Some(dir) => router
            .fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")))),
        None => router,
    }
}

pub async fn run(mut config: Config) -> Result<()> {
    if let Err(why) = config.tunnel.validate() {
        anyhow::bail!("{why}");
    }
    // The guest listener is bound whenever internet links are on. If its
    // address is taken (a second portal on this machine), run without them
    // rather than not at all.
    let guest_listener = if config.tunnel.enabled {
        match tokio::net::TcpListener::bind(config.tunnel.guest_listen).await {
            Ok(listener) => Some(listener),
            Err(err) => {
                warn!(
                    "can't bind the guest listener on {}: {err}; internet share links are off (set CHA_GUEST_LISTEN, or CHA_TUNNEL=off to silence this)",
                    config.tunnel.guest_listen
                );
                config.tunnel.enabled = false;
                None
            }
        }
    } else {
        None
    };
    let db = db::open(&config.database).await?;
    let state = AppState::new(config.clone(), db.clone()).await?;
    if db::user_count(&db).await? == 0 {
        warn!(
            "No accounts yet. Open the portal to claim it: the first visitor creates the first admin"
        );
    }
    if config
        .web_dir
        .as_ref()
        .is_none_or(|d| !d.join("index.html").exists())
    {
        warn!("no built portal SPA found; serving the API only");
    }
    if config.dev_login {
        warn!("dev login is ON: any loopback client can sign in as the `dev` admin");
    }
    if config.discover_nodes
        && let Err(err) = discovery::browse(Arc::clone(&state.discovered))
    {
        warn!("can't look for nodes on the LAN: {err:#}");
    }
    tokio::spawn(moonlight::refresh_loop(state.clone()));
    tokio::spawn(settings::idle_loop(state.clone()));
    if let Some(listener) = guest_listener {
        info!(
            "guest listener on http://{} (internet share links only)",
            config.tunnel.guest_listen
        );
        let guest = guest_app(state.clone());
        tokio::spawn(async move {
            if let Err(err) = axum::serve(
                listener,
                guest.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            {
                warn!("guest listener: {err}");
            }
        });
        tokio::spawn(tunnel::idle_loop(state.clone()));
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
