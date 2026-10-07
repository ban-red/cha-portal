//! Portal-wide settings an admin sets: `GET` and `PUT /api/admin/settings`.
//!
//! The idle shutoff: a running environment nobody is watching is stopped after
//! `idleShutdownMinutes` (default 30; 0 turns it off). The portal asks each
//! running environment's streamer how many browsers are connected (`viewers`
//! in its `/info`) once a minute and times the ones with none. The clock is
//! kept in memory, so a portal restart gives every idle environment a fresh
//! full period; a streamer that doesn't report `viewers` is never stopped.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use cha_wire::{NodeRequest, NodeResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::AppState;
use crate::auth::{AdminUser, ClientInfo};
use crate::db;
use crate::environments::stop_on_node;
use crate::error::{ApiError, ApiResult};

const IDLE_KEY: &str = "idle_shutdown_minutes";
pub const DEFAULT_IDLE_MINUTES: u32 = 30;
/// A week: past this "off" is the better setting.
const MAX_IDLE_MINUTES: u32 = 7 * 24 * 60;
const CHECK_EVERY: Duration = Duration::from_secs(60);
const INFO_TIMEOUT: Duration = Duration::from_secs(10);

pub fn routes() -> Router<AppState> {
    Router::new().route("/admin/settings", get(read).put(write))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    /// Minutes an environment without viewers keeps running; 0: never stop it.
    idle_shutdown_minutes: u32,
}

/// The idle period in minutes (0: off).
pub async fn idle_minutes(db: &sqlx::SqlitePool) -> u32 {
    db::setting(db, IDLE_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_IDLE_MINUTES)
}

async fn read(State(state): State<AppState>, _: AdminUser) -> ApiResult<Json<Settings>> {
    Ok(Json(Settings {
        idle_shutdown_minutes: idle_minutes(&state.db).await,
    }))
}

async fn write(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Json(req): Json<Settings>,
) -> ApiResult<Json<Settings>> {
    if req.idle_shutdown_minutes > MAX_IDLE_MINUTES {
        return Err(ApiError::bad_request(
            "bad_idle_shutdown",
            "the idle shutoff is at most a week (10080 minutes); 0 turns it off",
        ));
    }
    db::set_setting(&state.db, IDLE_KEY, &req.idle_shutdown_minutes.to_string()).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "settings.updated",
        None,
        Some(json!({ "idleShutdownMinutes": req.idle_shutdown_minutes })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(req))
}

/// Stops running environments nobody has watched for the idle period.
pub async fn idle_loop(state: AppState) {
    // When each running environment was first seen with no viewers.
    let mut idle_since: HashMap<String, Instant> = HashMap::new();
    loop {
        tokio::time::sleep(CHECK_EVERY).await;
        let minutes = idle_minutes(&state.db).await;
        if minutes == 0 {
            idle_since.clear();
            continue;
        }
        let limit = Duration::from_secs(u64::from(minutes) * 60);
        let envs = match db::list_environments(&state.db, None, 500).await {
            Ok(envs) => envs,
            Err(err) => {
                warn!("idle shutoff: listing environments: {err}");
                continue;
            }
        };
        let running: Vec<_> = envs
            .into_iter()
            .filter(|e| e.state == "running" && !e.template_id.starts_with("moonlight:"))
            .collect();
        idle_since.retain(|id, _| running.iter().any(|e| &e.id == id));
        for env in running {
            let Some(node_id) = env.node_id.clone() else {
                continue;
            };
            let reply = state
                .nodes
                .request_timeout(
                    &node_id,
                    NodeRequest::StreamerInfo {
                        environment_id: env.id.clone(),
                    },
                    INFO_TIMEOUT,
                )
                .await;
            // Offline node or no answer: nothing to go on, so no clock.
            let Ok(NodeResponse::StreamerInfo { info }) = reply else {
                continue;
            };
            match info.get("viewers").and_then(|v| v.as_u64()) {
                // An older streamer doesn't say: never stop it blind.
                None => {
                    idle_since.remove(&env.id);
                }
                Some(n) if n > 0 => {
                    idle_since.remove(&env.id);
                }
                Some(_) => {
                    let since = *idle_since
                        .entry(env.id.clone())
                        .or_insert_with(Instant::now);
                    if since.elapsed() < limit {
                        continue;
                    }
                    idle_since.remove(&env.id);
                    stop_idle(&state, &env.id, &env.template_id, &node_id, minutes).await;
                }
            }
        }
    }
}

async fn stop_idle(state: &AppState, id: &str, template: &str, node_id: &str, minutes: u32) {
    let detail = format!("stopped: nobody was connected for {minutes} min");
    match db::transition_environment(&state.db, id, &["running"], "stopping", Some(&detail)).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(err) => {
            warn!("idle shutoff: {err}");
            return;
        }
    }
    info!(environment = id, minutes, "stopping an idle environment");
    let _ = db::audit(
        &state.db,
        None,
        "environment.idle_stopped",
        Some(id),
        Some(json!({ "template": template, "idleMinutes": minutes })),
        None,
    )
    .await;
    tokio::spawn(stop_on_node(
        state.clone(),
        node_id.to_string(),
        id.to_string(),
        Some(detail),
    ));
}
