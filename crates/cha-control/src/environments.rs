//! Environments (plan §4; Phase 1, P1.4): the catalog, launching a template on
//! a node, stopping it, and keeping the portal's view in step with what the
//! nodes actually run.
//!
//! States: `starting → running → stopping → destroyed`, or `failed` (with a
//! reason) from any of them. Stopping one destroys it; what an app keeps for a
//! user between launches is app data ([`crate::storage`]), not the
//! environment. Node calls run in the background; the SPA polls.

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
use crate::storage::{self, SharedDefaults};

/// The first start may pull images, and the first with a user's data under
/// the node's data root copies their old home in (a Steam library is
/// gigabytes).
const START_TIMEOUT: Duration = Duration::from_secs(600);
const STOP_TIMEOUT: Duration = Duration::from_secs(60);
/// Environments one user may have live at once.
const MAX_LIVE_PER_USER: i64 = 4;
const LIST_LIMIT: i64 = 50;
/// What every environment starts at until clients ask for their own size.
/// How long a media token stays good: long enough to carry an offer to the
/// streamer, no longer.
const MEDIA_TOKEN_SECS: i64 = 60;
const CODECS: [&str; 3] = ["h264", "hevc", "av1"];
/// The LAN tier's codec (plan §3.2), over WebTransport only.
const PYROWAVE_CODECS: [&str; 2] = ["pyrowave420", "pyrowave444"];
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
    /// Users keep their data for it between launches (their home in it
    /// survives stopping), unless they or an admin say otherwise: this is the
    /// default `crate::storage` starts from.
    #[serde(default)]
    pub persistent: bool,
    /// Its display has a fixed size (Steam's gamescope): the page never asks
    /// the streamer to resize, and the browser letterboxes the picture.
    #[serde(default)]
    pub fixed_size: bool,
    /// What it shares across users, unless an admin says otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<SharedDefaults>,
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

pub(crate) fn template(id: &str) -> Option<&'static Template> {
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
    /// Something to tell the user while it runs (its node noticed a problem).
    warning: Option<String>,
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
        warning: row.warning,
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
    let settings = storage::effective_for(&state, &user.id, template).await?;
    // Two environments would share one home.
    if settings.persistent
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
    let app_data = storage::spec_storage(&user.id, template, settings);
    // A node that predates app data would drop what it can't read, and the
    // user's data would quietly not be kept (or shared).
    if app_data.is_some() && !storage::node_has_storage(&node) {
        return Err(ApiError::conflict(
            "node_needs_update",
            format!(
                "{} can't keep or share app data yet: update its agent (it reports its data root once it can)",
                node.name
            ),
        ));
    }
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
    let spec = environment_spec(
        id.clone(),
        &user.id,
        template,
        state.media_key.public_b64(),
        app_data.map(Box::new),
    );
    tokio::spawn(start_on_node(state.clone(), node.id.clone(), spec));
    let row = db::environment_by_id(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("the new environment vanished")))?;
    Ok(Json(view(row, &nodes_by_id(&state).await?)))
}

/// What a node runs for `owner`'s launch of `template`, with the app data
/// ([`storage::spec_storage`]) it mounts.
fn environment_spec(
    id: String,
    owner: &str,
    template: &Template,
    portal_key: String,
    storage: Option<Box<cha_wire::Storage>>,
) -> EnvironmentSpec {
    EnvironmentSpec {
        id,
        image: template.image.clone(),
        security: template.security,
        shm_mb: template.shm_mb,
        width: WIDTH,
        height: HEIGHT,
        fps: FPS,
        portal_key,
        // Not the old way of keeping a home (a volume the portal named): the
        // node moves such a volume's files into the new directory itself.
        home: None,
        owner: owner.to_string(),
        template: template.id.clone(),
        // The data lives on the node it was first made on: placement keeps a
        // user on one node while there is one (Phase 3 makes it follow).
        storage,
    }
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

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Transport {
    #[default]
    WebRtc,
    /// `cha-stream/1` over WebTransport: the browser connects to the streamer
    /// itself, with the URLs and certificate hash this returns.
    WebTransport,
}

#[derive(Deserialize)]
struct ConnectRequest {
    /// `h264`, `hevc` or `av1`: what this browser decodes best.
    codec: String,
    #[serde(default)]
    transport: Transport,
    /// The browser's WebRTC offer (`{"type": "offer", "sdp": …}`).
    #[serde(default)]
    offer: serde_json::Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectResponse {
    codec: String,
    transport: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    answer: Option<serde_json::Value>,
    /// WebTransport: one URL per address of the node, best first, each with
    /// the media token; the browser takes the first that connects.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    urls: Vec<String>,
    /// WebTransport: the streamer's self-signed certificate (SHA-256, hex),
    /// for `serverCertificateHashes`.
    #[serde(skip_serializing_if = "Option::is_none")]
    cert_hash: Option<String>,
}

/// Brokers a WebRTC connection: the offer goes to the environment's streamer
/// through its node, with a short-lived media token only this portal can sign;
/// the answer comes back the same way. Media then flows node → browser.
/// From a streamer's `/info`: a WebTransport URL per address it has (best
/// first), carrying the media token, and its certificate's hash.
fn webtransport_urls(
    info: &serde_json::Value,
    codec: &str,
    token: &str,
) -> Option<(Vec<String>, String)> {
    let port = info["wt_port"].as_u64().filter(|p| *p > 0)?;
    let hash = info["cert_hash_hex"].as_str().filter(|h| !h.is_empty())?;
    let urls = info["addresses"]
        .as_array()?
        .iter()
        .filter_map(|a| a.as_str())
        .map(|addr| {
            let host = if addr.contains(':') {
                format!("[{addr}]")
            } else {
                addr.to_string()
            };
            format!("https://{host}:{port}/media?codec={codec}&token={token}")
        })
        .collect::<Vec<_>>();
    (!urls.is_empty()).then(|| (urls, hash.to_string()))
}

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
    let pyrowave = PYROWAVE_CODECS.contains(&req.codec.as_str());
    if !CODECS.contains(&req.codec.as_str()) && !pyrowave {
        return Err(ApiError::bad_request(
            "bad_codec",
            "codec must be h264, hevc, av1, pyrowave420 or pyrowave444",
        ));
    }
    if pyrowave && req.transport != Transport::WebTransport {
        return Err(ApiError::bad_request(
            "bad_codec",
            "PyroWave goes over WebTransport only",
        ));
    }
    if req.transport == Transport::WebRtc && req.offer.get("sdp").and_then(|s| s.as_str()).is_none()
    {
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
    let media_token = sign_media_token(&state.media_key, &claims);
    let response = match req.transport {
        Transport::WebRtc => {
            let reply = state
                .nodes
                .request(
                    &node_id,
                    NodeRequest::Connect {
                        environment_id: id.clone(),
                        codec: req.codec.clone(),
                        offer: req.offer,
                        media_token,
                    },
                )
                .await?;
            let NodeResponse::Answer { answer } = reply else {
                return Err(ApiError::conflict(
                    "node_error",
                    "the node answered something else",
                ));
            };
            ConnectResponse {
                codec: req.codec.clone(),
                transport: "webrtc",
                answer: Some(answer),
                urls: Vec::new(),
                cert_hash: None,
            }
        }
        Transport::WebTransport => {
            let reply = state
                .nodes
                .request(
                    &node_id,
                    NodeRequest::StreamerInfo {
                        environment_id: id.clone(),
                    },
                )
                .await?;
            let NodeResponse::StreamerInfo { info } = reply else {
                return Err(ApiError::conflict(
                    "node_error",
                    "the node answered something else",
                ));
            };
            let (urls, cert_hash) =
                webtransport_urls(&info, &req.codec, &media_token).ok_or_else(|| {
                    ApiError::conflict(
                        "no_webtransport",
                        "this environment's streamer doesn't offer WebTransport",
                    )
                })?;
            ConnectResponse {
                codec: req.codec.clone(),
                transport: "webtransport",
                answer: None,
                urls,
                cert_hash: Some(cert_hash),
            }
        }
    };
    db::audit(
        &state.db,
        Some(&user.id),
        "environment.connected",
        Some(&id),
        Some(json!({ "codec": req.codec, "transport": response.transport })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(response))
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
/// fail, and ones the portal no longer wants are stopped. `expected` is what
/// the portal had for the node when the report arrived: anything started
/// since isn't in `running`, and mustn't count as lost.
pub async fn reconcile(
    state: AppState,
    node_id: String,
    expected: Vec<EnvironmentRow>,
    running: Vec<String>,
) {
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
        assert!(template("steam").unwrap().persistent);
    }

    #[test]
    fn a_spec_carries_the_storage_and_never_the_old_home_volume() {
        let owner = "01a10527-f79f-761b-962f-4b26924a2e68";
        let settings = storage::Effective {
            persistent: true,
            shared_access: storage::SharedAccess::Write,
        };
        let steam = template("steam").unwrap();
        let spec = environment_spec(
            "e1".into(),
            owner,
            steam,
            "k".into(),
            storage::spec_storage(owner, steam, settings).map(Box::new),
        );
        assert_eq!(spec.home, None);
        assert_eq!(
            (spec.owner.as_str(), spec.template.as_str()),
            (owner, "steam")
        );
        let storage = spec.storage.expect("steam keeps and shares");
        assert_eq!(
            storage.home.as_deref(),
            Some("users/01a10527-f79f-761b-962f-4b26924a2e68/steam")
        );
        assert_eq!(storage.check(owner, "steam"), Ok(()));

        // Nothing kept or shared: nothing sent.
        let chrome = template("chrome").unwrap();
        let plain = environment_spec("e2".into(), owner, chrome, "k".into(), None);
        assert_eq!(plain.storage, None);
        assert_eq!(plain.home, None);
    }

    #[test]
    fn webtransport_urls_cover_every_address() {
        let info = json!({
            "wt_port": 47002,
            "cert_hash_hex": "ab12",
            "addresses": ["192.168.1.5", "100.64.0.7", "fd7a::1"],
        });
        let (urls, hash) = webtransport_urls(&info, "hevc", "tok").unwrap();
        assert_eq!(hash, "ab12");
        assert_eq!(
            urls[0],
            "https://192.168.1.5:47002/media?codec=hevc&token=tok"
        );
        assert_eq!(
            urls[2],
            "https://[fd7a::1]:47002/media?codec=hevc&token=tok"
        );
        assert!(webtransport_urls(&json!({ "wt_port": 0 }), "hevc", "tok").is_none());
    }
}
