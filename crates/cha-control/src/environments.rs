//! Environments (plan §4; Phase 1, P1.4): the catalog, launching a template on
//! a node, stopping it, and keeping the portal's view in step with what the
//! nodes actually run.
//!
//! States: `starting → running → stopping → destroyed`, or `failed` (with a
//! reason) from any of them. Environments are ephemeral for now: stopping one
//! destroys it. Node calls run in the background; the SPA polls.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use cha_wire::{
    EnvironmentSpec, Inventory, MediaClaims, NodeRequest, NodeResponse, SecurityProfile,
    sign_media_token,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::AppState;
use crate::auth::{ClientInfo, CurrentUser};
use crate::db::{self, EnvironmentRow, NodeRow, Role, User};
use crate::error::{ApiError, ApiResult};

/// The first start may pull images.
const START_TIMEOUT: Duration = Duration::from_secs(180);
const STOP_TIMEOUT: Duration = Duration::from_secs(60);
/// Environments one user may have live at once.
const MAX_LIVE_PER_USER: i64 = 4;
const LIST_LIMIT: i64 = 50;
/// What every environment starts at until clients ask for their own size.
/// How long a media token stays good: long enough to carry an offer to the
/// streamer, no longer.
const MEDIA_TOKEN_SECS: i64 = 60;
const CODECS: [&str; 3] = ["h264", "hevc", "av1"];
const WIDTH: u32 = 2560;
const HEIGHT: u32 = 1440;
const FPS: u32 = 60;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/catalog", get(list_catalog))
        .route("/environments", get(list).post(launch))
        .route("/environments/{id}", get(show).delete(stop))
        .route("/environments/{id}/connect", post(connect))
}

// ---- The catalog ----

/// One thing a user can launch, from `images/catalog.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: String,
    pub name: String,
    pub description: String,
    pub image: String,
    /// `browser`, `desktop`, `test`, …
    pub class: String,
    pub security: SecurityProfile,
    pub shm_mb: u32,
    /// The user's home in it survives stopping (a volume per user and
    /// template), so there is at most one of it per user at a time.
    #[serde(default)]
    pub persistent: bool,
}

#[derive(Deserialize)]
struct Catalog {
    templates: Vec<Template>,
}

pub fn catalog() -> &'static [Template] {
    static CATALOG: OnceLock<Vec<Template>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str::<Catalog>(include_str!("../../../images/catalog.json"))
            .expect("images/catalog.json is valid (checked by a test)")
            .templates
    })
}

fn template(id: &str) -> Option<&'static Template> {
    catalog().iter().find(|t| t.id == id)
}

async fn list_catalog(_: CurrentUser) -> Json<&'static [Template]> {
    Json(catalog())
}

// ---- Views ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentView {
    id: String,
    template_id: String,
    template_name: String,
    owner_id: String,
    node_id: Option<String>,
    node_name: Option<String>,
    state: String,
    detail: Option<String>,
    created_at: i64,
    updated_at: i64,
    /// Where the streamer listens, while it runs (the portal brokers
    /// connections from P1.5; until then this is for diagnostics).
    streamer: Option<StreamerView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamerView {
    host: Option<String>,
    http_port: i64,
    webrtc_port: i64,
}

fn view(row: EnvironmentRow, nodes: &HashMap<String, NodeRow>) -> EnvironmentView {
    let node = row.node_id.as_ref().and_then(|id| nodes.get(id));
    let host = node
        .and_then(|n| n.inventory.as_deref())
        .and_then(|j| serde_json::from_str::<Inventory>(j).ok())
        .and_then(|inv| inv.addresses.into_iter().next());
    let streamer = match (row.state.as_str(), row.http_port, row.webrtc_port) {
        ("running", Some(http_port), Some(webrtc_port)) => Some(StreamerView {
            host,
            http_port,
            webrtc_port,
        }),
        _ => None,
    };
    EnvironmentView {
        template_name: template(&row.template_id)
            .map_or_else(|| row.template_id.clone(), |t| t.name.clone()),
        node_name: node.map(|n| n.name.clone()),
        id: row.id,
        template_id: row.template_id,
        owner_id: row.owner_id,
        node_id: row.node_id,
        state: row.state,
        detail: row.detail,
        created_at: row.created_at,
        updated_at: row.updated_at,
        streamer,
    }
}

async fn nodes_by_id(state: &AppState) -> ApiResult<HashMap<String, NodeRow>> {
    Ok(db::list_nodes(&state.db)
        .await?
        .into_iter()
        .map(|n| (n.id.clone(), n))
        .collect())
}

/// The environment, if this user may see it (its owner, or an admin).
async fn visible(state: &AppState, user: &User, id: &str) -> ApiResult<EnvironmentRow> {
    db::environment_by_id(&state.db, id)
        .await?
        .filter(|e| e.owner_id == user.id || user.role == Role::Admin)
        .ok_or_else(|| ApiError::NotFound("no such environment".into()))
}

// ---- The API ----

async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<Json<Vec<EnvironmentView>>> {
    let rows = db::list_environments(&state.db, Some(&user.id), LIST_LIMIT).await?;
    let nodes = nodes_by_id(&state).await?;
    Ok(Json(rows.into_iter().map(|r| view(r, &nodes)).collect()))
}

async fn show(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> ApiResult<Json<EnvironmentView>> {
    let row = visible(&state, &user, &id).await?;
    Ok(Json(view(row, &nodes_by_id(&state).await?)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchRequest {
    template_id: String,
}

async fn launch(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    Json(req): Json<LaunchRequest>,
) -> ApiResult<Json<EnvironmentView>> {
    if user.role == Role::Guest {
        return Err(ApiError::forbidden(
            "guests_cannot_launch",
            "guests can't launch environments",
        ));
    }
    let template = template(&req.template_id)
        .ok_or_else(|| ApiError::bad_request("unknown_template", "no such template"))?;
    if db::count_live_environments(&state.db, &user.id).await? >= MAX_LIVE_PER_USER {
        return Err(ApiError::conflict(
            "too_many_environments",
            format!("you can have {MAX_LIVE_PER_USER} environments at once; stop one first"),
        ));
    }
    if template.persistent
        && db::live_environment_of(&state.db, &user.id, &template.id)
            .await?
            .is_some()
    {
        return Err(ApiError::conflict(
            "already_running",
            format!(
                "your {} is already running; it keeps one home, so connect to that one",
                template.name
            ),
        ));
    }
    let node = place(&state).await?;
    let id = db::new_id();
    db::insert_environment(&state.db, &id, &user.id, &template.id, &node.id, "starting").await?;
    db::audit(
        &state.db,
        Some(&user.id),
        "environment.launched",
        Some(&id),
        Some(json!({ "template": template.id, "node": node.name })),
        client.ip.as_deref(),
    )
    .await?;
    info!(%id, template = %template.id, node = %node.name, user = %user.username, "launching");
    let spec = EnvironmentSpec {
        id: id.clone(),
        image: template.image.clone(),
        security: template.security,
        shm_mb: template.shm_mb,
        width: WIDTH,
        height: HEIGHT,
        fps: FPS,
        portal_key: state.media_key.public_b64(),
        // Node-local: placement keeps a user on one node while there is one.
        home: template
            .persistent
            .then(|| format!("cha-home-{}-{}", user.id, template.id)),
    };
    tokio::spawn(start_on_node(state.clone(), node.id.clone(), spec));
    let row = db::environment_by_id(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("the new environment vanished")))?;
    Ok(Json(view(row, &nodes_by_id(&state).await?)))
}

async fn stop(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    Path(id): Path<String>,
) -> ApiResult<Json<EnvironmentView>> {
    let row = visible(&state, &user, &id).await?;
    if db::transition_environment(&state.db, &id, &["starting", "running"], "stopping", None)
        .await?
    {
        db::audit(
            &state.db,
            Some(&user.id),
            "environment.stopped",
            Some(&id),
            Some(json!({ "template": row.template_id })),
            client.ip.as_deref(),
        )
        .await?;
        if let Some(node_id) = row.node_id.clone() {
            tokio::spawn(stop_on_node(state.clone(), node_id, id.clone()));
        } else {
            db::transition_environment(&state.db, &id, &["stopping"], "destroyed", None).await?;
        }
    }
    let row = visible(&state, &user, &id).await?;
    Ok(Json(view(row, &nodes_by_id(&state).await?)))
}

#[derive(Deserialize)]
struct ConnectRequest {
    /// `h264`, `hevc` or `av1`: what this browser decodes best.
    codec: String,
    /// The browser's WebRTC offer (`{"type": "offer", "sdp": …}`).
    offer: serde_json::Value,
}

#[derive(Serialize)]
struct ConnectResponse {
    answer: serde_json::Value,
    codec: String,
}

/// Brokers a WebRTC connection: the offer goes to the environment's streamer
/// through its node, with a short-lived media token only this portal can sign;
/// the answer comes back the same way. Media then flows node → browser.
async fn connect(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    Path(id): Path<String>,
    Json(req): Json<ConnectRequest>,
) -> ApiResult<Json<ConnectResponse>> {
    let row = visible(&state, &user, &id).await?;
    if row.state != "running" {
        return Err(ApiError::conflict(
            "not_running",
            format!("the environment is {}", row.state),
        ));
    }
    if !CODECS.contains(&req.codec.as_str()) {
        return Err(ApiError::bad_request(
            "bad_codec",
            "codec must be h264, hevc or av1",
        ));
    }
    if req.offer.get("sdp").and_then(|s| s.as_str()).is_none() {
        return Err(ApiError::bad_request("bad_offer", "the offer has no SDP"));
    }
    let node_id = row
        .node_id
        .ok_or_else(|| ApiError::conflict("no_node", "the environment's node was removed"))?;
    let claims = MediaClaims {
        env: id.clone(),
        sub: user.id.clone(),
        role: if row.owner_id == user.id {
            "owner"
        } else {
            "admin"
        }
        .into(),
        exp: db::now() + MEDIA_TOKEN_SECS,
    };
    let reply = state
        .nodes
        .request(
            &node_id,
            NodeRequest::Connect {
                environment_id: id.clone(),
                codec: req.codec.clone(),
                offer: req.offer,
                media_token: sign_media_token(&state.media_key, &claims),
            },
        )
        .await?;
    let NodeResponse::Answer { answer } = reply else {
        return Err(ApiError::conflict(
            "node_error",
            "the node answered something else",
        ));
    };
    db::audit(
        &state.db,
        Some(&user.id),
        "environment.connected",
        Some(&id),
        Some(json!({ "codec": req.codec })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(ConnectResponse {
        answer,
        codec: req.codec,
    }))
}

/// Placement v0: the first connected node with an NVIDIA GPU that can encode.
async fn place(state: &AppState) -> ApiResult<NodeRow> {
    for node in db::list_nodes(&state.db).await? {
        if state.nodes.connected_since(&node.id).await.is_none() {
            continue;
        }
        let capable = node
            .inventory
            .as_deref()
            .and_then(|j| serde_json::from_str::<Inventory>(j).ok())
            .is_some_and(|inv| {
                inv.gpus
                    .iter()
                    .any(|g| g.vendor == "nvidia" && !g.encoders.is_empty())
            });
        if capable {
            return Ok(node);
        }
    }
    Err(ApiError::conflict(
        "no_node",
        "no online node can run it: environments need a node with an NVIDIA GPU that has a video encoder",
    ))
}

// ---- Talking to nodes ----

async fn start_on_node(state: AppState, node_id: String, spec: EnvironmentSpec) {
    let id = spec.id.clone();
    let reply = state
        .nodes
        .request_timeout(
            &node_id,
            NodeRequest::StartEnvironment { environment: spec },
            START_TIMEOUT,
        )
        .await;
    let result = match reply {
        Ok(NodeResponse::EnvironmentStarted { streamer, .. }) => {
            match db::set_environment_running(
                &state.db,
                &id,
                streamer.http_port,
                streamer.webrtc_port,
            )
            .await
            {
                // Stopped while starting: the stop is already on its way to the node.
                Ok(_) => Ok(()),
                Err(err) => Err(err.to_string()),
            }
        }
        Ok(other) => Err(format!("the node answered {other:?}")),
        Err(err) => Err(err.to_string()),
    };
    if let Err(reason) = result {
        warn!(%id, %node_id, "start failed: {reason}");
        let _ = db::transition_environment(&state.db, &id, &["starting"], "failed", Some(&reason))
            .await;
    }
}

async fn stop_on_node(state: AppState, node_id: String, id: String) {
    let reply = state
        .nodes
        .request_timeout(
            &node_id,
            NodeRequest::StopEnvironment { id: id.clone() },
            STOP_TIMEOUT,
        )
        .await;
    let (to, detail) = match reply {
        Ok(_) => ("destroyed", None),
        // It goes when the node reconnects and reports it (reconcile).
        Err(ApiError::Conflict("node_offline", _)) => (
            "destroyed",
            Some("its node was offline; the node removes it when it reconnects".to_string()),
        ),
        Err(err) => ("failed", Some(format!("stopping: {err}"))),
    };
    let _ = db::transition_environment(&state.db, &id, &["stopping"], to, detail.as_deref()).await;
}

/// A node reported what it runs (after every connect): environments it lost
/// fail, and ones the portal no longer wants are stopped.
pub async fn reconcile(state: AppState, node_id: String, running: Vec<String>) {
    let expected = match db::node_environments(&state.db, &node_id).await {
        Ok(rows) => rows,
        Err(err) => {
            warn!(%node_id, "reconciling: {err}");
            return;
        }
    };
    let wanted: Vec<&str> = expected
        .iter()
        .filter(|e| e.state == "starting" || e.state == "running")
        .map(|e| e.id.as_str())
        .collect();
    for env in &expected {
        let present = running.contains(&env.id);
        let moved = match (env.state.as_str(), present) {
            ("running", false) => {
                db::transition_environment(
                    &state.db,
                    &env.id,
                    &["running"],
                    "failed",
                    Some("its node no longer runs it"),
                )
                .await
            }
            ("stopping", false) => {
                db::transition_environment(&state.db, &env.id, &["stopping"], "destroyed", None)
                    .await
            }
            _ => Ok(false),
        };
        if let Err(err) = moved {
            warn!(id = %env.id, "reconciling: {err}");
        }
    }
    for id in running
        .into_iter()
        .filter(|id| !wanted.contains(&id.as_str()))
    {
        info!(%id, %node_id, "stopping an environment the portal no longer wants");
        let state = state.clone();
        let node_id = node_id.clone();
        tokio::spawn(async move {
            let _ = state
                .nodes
                .request_timeout(
                    &node_id,
                    NodeRequest::StopEnvironment { id: id.clone() },
                    STOP_TIMEOUT,
                )
                .await;
            let _ =
                db::transition_environment(&state.db, &id, &["stopping"], "destroyed", None).await;
        });
    }
}

/// An environment stopped on its own on its node.
pub async fn exited(state: AppState, id: String, detail: String, failed: bool) {
    let to = if failed { "failed" } else { "destroyed" };
    if let Err(err) =
        db::transition_environment(&state.db, &id, &["starting", "running"], to, Some(&detail))
            .await
    {
        warn!(%id, "recording an exit: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_parses() {
        let ids: Vec<&str> = catalog().iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"chrome"));
        assert!(ids.contains(&"test-pattern"));
        let chrome = template("chrome").unwrap();
        assert_eq!(chrome.security, SecurityProfile::Browser);
        assert!(chrome.shm_mb >= 512);
    }
}
