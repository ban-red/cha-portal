//! Moonlight hosts adopted through a node (ADR 0008).
//!
//! Nodes browse the LAN for Sunshine and Apollo hosts and tell the portal what
//! they see ([`ToPortal::MoonlightHosts`]); the portal keeps that in memory
//! ([`State::found`]). An admin adopts a found host: if its node isn't paired
//! with it yet, the node starts PIN pairing and the portal shows the PIN
//! until the node says it is done. An adopted host is a row in
//! `moonlight_hosts` with its apps cached, and each app launches as the
//! template `moonlight:<host id>:<app id>`, which [`resolve_template`] makes
//! from the row. The environment runs `cha-gateway` on the host's node
//! ([`EnvironmentSpec::gateway`]); a host streams to one user at a time.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::{Path, State as AxumState};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use cha_wire::{
    Device, DeviceKind, GatewaySpec, MoonlightApp, MoonlightHost, NodeRequest, NodeResponse,
    SecurityProfile,
};
use serde::Serialize;
use serde_json::json;
use tracing::{info, warn};

use crate::AppState;
use crate::auth::{AdminUser, ClientInfo, CurrentUser};
use crate::db::{self, MoonlightHostRow, NodeRow, Role};
use crate::environments::Template;
use crate::error::{ApiError, ApiResult};

/// A synthesized template's class.
pub const CLASS: &str = "moonlight";
/// The image every Moonlight environment runs (the node maps it, like any).
pub const GATEWAY_IMAGE: &str = "cha/gateway:dev";
/// The device a gateway environment is recorded on: it uses no GPU, so it
/// isn't any real device's load.
const GATEWAY_DEVICE: &str = "gateway";
/// Sunshine's and Apollo's PIN page.
const PIN_PORT: u16 = 47990;
/// A pairing the node never reported on is given up after this long (the
/// node's own limit is five minutes).
const PAIRING_GIVE_UP_SECS: i64 = 6 * 60;
/// A finished pairing is kept this long for the page to read.
const PAIRING_KEEP_SECS: i64 = 15 * 60;
/// Apps are asked for again this often.
const REFRESH_EVERY: Duration = Duration::from_secs(3600);
/// Asking a node for a host's apps.
const APPS_TIMEOUT: Duration = Duration::from_secs(30);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/moonlight/found", get(found))
        .route("/moonlight/adopt", post(adopt))
        .route("/moonlight/pairing/{key}", get(pairing))
        .route("/moonlight/hosts", get(hosts))
        .route("/moonlight/hosts/{id}/refresh", post(refresh))
        .route("/moonlight/hosts/{id}", axum::routing::delete(remove))
}

// ---- What the portal knows in memory ----

enum Status {
    /// Waiting for the PIN to be entered on the host.
    Pairing,
    /// The node said it is paired; the portal is fetching the apps.
    Finishing,
    Adopted(Box<AdoptedHost>),
    Failed(String),
}

struct Session {
    status: Status,
    /// The admin who started it, for the audit log.
    admin: String,
    ip: Option<String>,
    started: i64,
    /// When it ended.
    ended: Option<i64>,
}

/// Hosts the nodes see, and the pairings in progress.
#[derive(Default)]
pub struct State {
    /// What each node last sent; a node that never sent a list isn't here
    /// (it can't stream Moonlight), and one that disconnects is dropped.
    found: Mutex<HashMap<String, Vec<MoonlightHost>>>,
    /// Pairings by `<node id>:<unique id>`.
    sessions: Mutex<HashMap<String, Session>>,
    /// Held from the host-busy check to the new environment's row.
    pub launch: tokio::sync::Mutex<()>,
}

impl State {
    /// Records a node's list; true when it is the first this connection sent.
    pub fn set_found(&self, node_id: &str, hosts: Vec<MoonlightHost>) -> bool {
        self.found
            .lock()
            .expect("moonlight lock")
            .insert(node_id.to_string(), hosts)
            .is_none()
    }

    pub fn forget(&self, node_id: &str) {
        self.found.lock().expect("moonlight lock").remove(node_id);
    }

    pub fn found_on(&self, node_id: &str) -> Option<Vec<MoonlightHost>> {
        self.found
            .lock()
            .expect("moonlight lock")
            .get(node_id)
            .cloned()
    }

    fn host_on(&self, node_id: &str, unique_id: &str) -> Option<MoonlightHost> {
        self.found_on(node_id)?
            .into_iter()
            .find(|h| h.unique_id == unique_id)
    }

    fn prune_sessions(&self) {
        let now = db::now();
        self.sessions
            .lock()
            .expect("moonlight lock")
            .retain(|_, s| match s.ended {
                Some(ended) => now - ended < PAIRING_KEEP_SECS,
                None => true,
            });
    }
}

fn key_of(node_id: &str, unique_id: &str) -> String {
    format!("{node_id}:{unique_id}")
}

fn split_key(key: &str) -> Option<(&str, &str)> {
    key.split_once(':')
        .filter(|(n, u)| !n.is_empty() && !u.is_empty())
}

// ---- Templates ----

/// `moonlight:<host id>:<app id>`.
pub fn parse_template_id(id: &str) -> Option<(&str, u32)> {
    let rest = id.strip_prefix("moonlight:")?;
    let (host, app) = rest.split_once(':')?;
    Some((host, app.parse().ok()?))
}

fn apps_of(row: &MoonlightHostRow) -> Vec<MoonlightApp> {
    row.apps
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default()
}

fn codecs_of(row: &MoonlightHostRow) -> Vec<String> {
    row.codecs
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default()
}

fn template_for(row: &MoonlightHostRow, app: &MoonlightApp) -> Template {
    Template {
        id: format!("moonlight:{}:{}", row.id, app.id),
        name: app.name.clone(),
        description: format!("On {}", row.name),
        image: GATEWAY_IMAGE.into(),
        local_image: None,
        class: CLASS.into(),
        icon: None,
        security: SecurityProfile::Standard,
        shm_mb: 64,
        persistent: false,
        // The host's picture is the size it was launched at.
        fixed_size: true,
        shared: None,
        gamepad: None,
        fps: None,
        needs_gpu: false,
    }
}

/// The template `id` names: a catalog one, or one of an adopted host's apps.
pub async fn resolve_template(state: &AppState, id: &str) -> ApiResult<Option<Template>> {
    if let Some(template) = crate::environments::template(id) {
        return Ok(Some(template.clone()));
    }
    let Some((host, app)) = parse_template_id(id) else {
        return Ok(None);
    };
    let Some(row) = db::moonlight_host(&state.db, host).await? else {
        return Ok(None);
    };
    Ok(apps_of(&row)
        .iter()
        .find(|a| a.id == app)
        .map(|a| template_for(&row, a)))
}

/// What the environment views need of Moonlight templates: app names, and
/// each host's codecs (its streamer is the host's).
#[derive(Default)]
pub struct Index {
    names: HashMap<String, String>,
    codecs: HashMap<String, Vec<String>>,
}

impl Index {
    pub async fn load(state: &AppState) -> ApiResult<Self> {
        let mut index = Self::default();
        for row in db::moonlight_hosts(&state.db).await? {
            for app in apps_of(&row) {
                index
                    .names
                    .insert(format!("moonlight:{}:{}", row.id, app.id), app.name);
            }
            index.codecs.insert(row.id.clone(), codecs_of(&row));
        }
        Ok(index)
    }

    pub fn name(&self, template_id: &str) -> Option<&str> {
        self.names.get(template_id).map(String::as_str)
    }

    pub fn codecs(&self, template_id: &str) -> Option<Vec<String>> {
        let (host, _) = parse_template_id(template_id)?;
        self.codecs.get(host).cloned()
    }
}

/// Where a Moonlight app launches: the node that paired the host, if it is
/// online and sees the host paired. Any of its devices will do (the gateway
/// uses none), so the environment is recorded on a device of its own.
pub async fn place(
    state: &AppState,
    template: &Template,
) -> ApiResult<(NodeRow, Device, GatewaySpec)> {
    let (host_id, app_id) = parse_template_id(&template.id)
        .ok_or_else(|| ApiError::bad_request("unknown_template", "no such template"))?;
    let row = db::moonlight_host(&state.db, host_id)
        .await?
        .ok_or_else(|| ApiError::bad_request("unknown_template", "no such template"))?;
    let no_node = |why: String| ApiError::conflict("no_node", why);
    let node = db::node_by_id(&state.db, &row.node_id)
        .await?
        .ok_or_else(|| no_node(format!("the node of {} was removed", row.name)))?;
    if state.nodes.connected_since(&node.id).await.is_none() {
        return Err(no_node(format!(
            "{} is offline, and {} streams through it",
            node.name, row.name
        )));
    }
    let seen = state
        .moonlight
        .host_on(&node.id, &row.unique_id)
        .ok_or_else(|| no_node(format!("{} doesn't see {} right now", node.name, row.name)))?;
    if !seen.paired {
        return Err(no_node(format!(
            "{} is no longer paired with {}: remove it and adopt it again",
            node.name, row.name
        )));
    }
    // What the node sees now beats what was stored.
    let gateway = GatewaySpec {
        unique_id: seen.unique_id,
        address: seen.address,
        http_port: seen.http_port,
        https_port: seen.https_port,
        app_id,
    };
    let device = Device {
        id: GATEWAY_DEVICE.into(),
        kind: DeviceKind::Cpu,
        name: "Moonlight gateway".into(),
        render_node: None,
        vendor: None,
        codecs: seen.codecs,
        cores: None,
    };
    Ok((node, device, gateway))
}

/// Refuses a launch while someone has the host.
pub async fn check_free(state: &AppState, template: &Template) -> ApiResult<()> {
    let Some((host_id, _)) = parse_template_id(&template.id) else {
        return Ok(());
    };
    if let Some((_, owner)) = db::live_moonlight_environment(&state.db, host_id).await? {
        let host = db::moonlight_host(&state.db, host_id).await?;
        return Err(ApiError::conflict(
            "host_busy",
            format!(
                "{owner} is already streaming from {}; a host plays for one person at a time",
                host.map_or_else(|| "it".to_string(), |h| h.name)
            ),
        ));
    }
    Ok(())
}

// ---- Views ----

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppView {
    template_id: String,
    app_id: u32,
    name: String,
    hdr: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Busy {
    environment_id: String,
    owner: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptedHost {
    id: String,
    name: String,
    node_id: String,
    node_name: String,
    /// Its node is connected and sees the host.
    online: bool,
    codecs: Vec<String>,
    apps: Vec<AppView>,
    /// When the apps were last fetched (unix seconds).
    apps_at: Option<i64>,
    busy: Option<Busy>,
}

async fn host_view(state: &AppState, row: &MoonlightHostRow) -> ApiResult<AdoptedHost> {
    let node = db::node_by_id(&state.db, &row.node_id).await?;
    let connected = state.nodes.connected_since(&row.node_id).await.is_some();
    let online = connected
        && state
            .moonlight
            .host_on(&row.node_id, &row.unique_id)
            .is_some();
    let busy = db::live_moonlight_environment(&state.db, &row.id)
        .await?
        .map(|(environment_id, owner)| Busy {
            environment_id,
            owner,
        });
    Ok(AdoptedHost {
        id: row.id.clone(),
        name: row.name.clone(),
        node_id: row.node_id.clone(),
        node_name: node.map(|n| n.name).unwrap_or_default(),
        online,
        codecs: codecs_of(row),
        apps: apps_of(row)
            .into_iter()
            .map(|a| AppView {
                template_id: format!("moonlight:{}:{}", row.id, a.id),
                app_id: a.id,
                name: a.name,
                hdr: a.hdr,
            })
            .collect(),
        apps_at: row.apps_at,
        busy,
    })
}

// ---- Talking to the node ----

/// A request to a node, with its failures as what the admin did (nothing) and
/// what the node did.
async fn ask(state: &AppState, node_id: &str, request: NodeRequest) -> ApiResult<NodeResponse> {
    state
        .nodes
        .request_timeout(node_id, request, APPS_TIMEOUT)
        .await
        .map_err(|err| match err {
            ApiError::Conflict("node_offline" | "node_timeout", m) => {
                ApiError::BadGateway("node_unreachable", m)
            }
            ApiError::Conflict("node_error", m) => ApiError::BadGateway("node_error", m),
            other => other,
        })
}

async fn fetch_apps(
    state: &AppState,
    node_id: &str,
    unique_id: &str,
) -> ApiResult<Vec<MoonlightApp>> {
    match ask(
        state,
        node_id,
        NodeRequest::MoonlightApps {
            unique_id: unique_id.to_string(),
        },
    )
    .await?
    {
        NodeResponse::MoonlightApps { apps } => Ok(apps),
        _ => Err(ApiError::BadGateway(
            "node_error",
            "the node answered something else".into(),
        )),
    }
}

fn row_from(
    host: &MoonlightHost,
    node_id: &str,
    admin: &str,
    apps: &[MoonlightApp],
) -> MoonlightHostRow {
    MoonlightHostRow {
        id: db::new_id(),
        node_id: node_id.to_string(),
        unique_id: host.unique_id.clone(),
        name: host.name.clone(),
        address: Some(host.address.clone()),
        http_port: Some(i64::from(host.http_port)),
        https_port: Some(i64::from(host.https_port)),
        codecs: serde_json::to_string(&host.codecs).ok(),
        apps: serde_json::to_string(apps).ok(),
        apps_at: Some(db::now()),
        adopted_by: Some(admin.to_string()),
        created_at: Some(db::now()),
    }
}

/// Stores a host the node is paired with, with its apps.
async fn adopt_found(
    state: &AppState,
    node_id: &str,
    host: &MoonlightHost,
    admin: &str,
    ip: Option<&str>,
) -> ApiResult<AdoptedHost> {
    if db::moonlight_host_of_node(&state.db, node_id, &host.unique_id)
        .await?
        .is_some()
    {
        return Err(ApiError::conflict(
            "already_adopted",
            format!("{} is already adopted", host.name),
        ));
    }
    let apps = fetch_apps(state, node_id, &host.unique_id).await?;
    let row = row_from(host, node_id, admin, &apps);
    db::insert_moonlight_host(&state.db, &row).await?;
    db::audit(
        &state.db,
        Some(admin),
        "moonlight.adopted",
        Some(&row.id),
        Some(json!({ "name": row.name, "node": node_id, "apps": apps.len() })),
        ip,
    )
    .await?;
    info!(host = %row.name, id = %row.id, "Moonlight host adopted");
    host_view(state, &row).await
}

// ---- Admin API ----

async fn found(
    AxumState(state): AxumState<AppState>,
    _: AdminUser,
) -> ApiResult<Json<serde_json::Value>> {
    let adopted = db::moonlight_hosts(&state.db).await?;
    let mut hosts = Vec::new();
    for node in db::list_nodes(&state.db).await? {
        if state.nodes.connected_since(&node.id).await.is_none() {
            continue;
        }
        for host in state.moonlight.found_on(&node.id).unwrap_or_default() {
            if adopted
                .iter()
                .any(|a| a.node_id == node.id && a.unique_id == host.unique_id)
            {
                continue;
            }
            hosts.push(json!({
                "key": key_of(&node.id, &host.unique_id),
                "nodeId": node.id,
                "nodeName": node.name,
                "name": host.name,
                "address": host.address,
                "uniqueId": host.unique_id,
                "paired": host.paired,
            }));
        }
    }
    Ok(Json(json!({ "hosts": hosts })))
}

#[derive(serde::Deserialize)]
struct AdoptRequest {
    key: String,
}

async fn adopt(
    AxumState(state): AxumState<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Json(req): Json<AdoptRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let not_found = || ApiError::NotFound("no such Moonlight host (is its node online?)".into());
    let (node_id, unique_id) = split_key(&req.key).ok_or_else(not_found)?;
    if state.nodes.connected_since(node_id).await.is_none() {
        // Not found if the node doesn't exist at all, unreachable if it's away.
        return Err(match db::node_by_id(&state.db, node_id).await? {
            Some(node) => {
                ApiError::BadGateway("node_unreachable", format!("{} isn't connected", node.name))
            }
            None => not_found(),
        });
    }
    let host = state
        .moonlight
        .host_on(node_id, unique_id)
        .ok_or_else(not_found)?;
    if db::moonlight_host_of_node(&state.db, node_id, unique_id)
        .await?
        .is_some()
    {
        return Err(ApiError::conflict(
            "already_adopted",
            format!("{} is already adopted", host.name),
        ));
    }
    if host.paired {
        let adopted = adopt_found(&state, node_id, &host, &admin.id, client.ip.as_deref()).await?;
        return Ok(Json(json!({ "status": "adopted", "host": adopted })));
    }
    let pin = match ask(
        &state,
        node_id,
        NodeRequest::MoonlightPair {
            unique_id: unique_id.to_string(),
        },
    )
    .await?
    {
        NodeResponse::MoonlightPairing { pin } => pin,
        _ => {
            return Err(ApiError::BadGateway(
                "node_error",
                "the node answered something else".into(),
            ));
        }
    };
    state.moonlight.prune_sessions();
    state
        .moonlight
        .sessions
        .lock()
        .expect("moonlight lock")
        .insert(
            req.key.clone(),
            Session {
                status: Status::Pairing,
                admin: admin.id.clone(),
                ip: client.ip.clone(),
                started: db::now(),
                ended: None,
            },
        );
    info!(host = %host.name, "pairing a Moonlight host");
    Ok(Json(json!({
        "status": "pairing",
        "pin": pin,
        "pinUrl": format!("https://{}:{PIN_PORT}/pin", host.address),
        "hostName": host.name,
    })))
}

async fn pairing(
    AxumState(state): AxumState<AppState>,
    _: AdminUser,
    Path(key): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    state.moonlight.prune_sessions();
    let mut sessions = state.moonlight.sessions.lock().expect("moonlight lock");
    let session = sessions
        .get_mut(&key)
        .ok_or_else(|| ApiError::NotFound("no pairing in progress for that host".into()))?;
    if matches!(session.status, Status::Pairing)
        && db::now() - session.started > PAIRING_GIVE_UP_SECS
    {
        session.status = Status::Failed("the PIN wasn't entered on the host in time".into());
        session.ended = Some(db::now());
    }
    Ok(Json(match &session.status {
        Status::Pairing | Status::Finishing => json!({ "status": "pairing" }),
        Status::Adopted(host) => json!({ "status": "adopted", "host": host }),
        Status::Failed(message) => json!({ "status": "failed", "message": message }),
    }))
}

/// A pairing ended on the node, or the node's list shows it paired: adopt the
/// host (once).
pub async fn pairing_finished(
    state: AppState,
    node_id: String,
    unique_id: String,
    ok: bool,
    message: Option<String>,
) {
    let key = key_of(&node_id, &unique_id);
    let (admin, ip) = {
        let mut sessions = state.moonlight.sessions.lock().expect("moonlight lock");
        let Some(session) = sessions.get_mut(&key) else {
            return;
        };
        if !matches!(session.status, Status::Pairing) {
            return;
        }
        if !ok {
            session.status =
                Status::Failed(message.unwrap_or_else(|| "pairing failed".to_string()));
            session.ended = Some(db::now());
            return;
        }
        session.status = Status::Finishing;
        (session.admin.clone(), session.ip.clone())
    };
    let outcome = async {
        let host = state
            .moonlight
            .host_on(&node_id, &unique_id)
            .ok_or_else(|| ApiError::conflict("not_found", "the node no longer sees the host"))?;
        db::audit(
            &state.db,
            Some(&admin),
            "moonlight.paired",
            Some(&unique_id),
            Some(json!({ "name": host.name, "node": node_id })),
            ip.as_deref(),
        )
        .await?;
        adopt_found(&state, &node_id, &host, &admin, ip.as_deref()).await
    }
    .await;
    let mut sessions = state.moonlight.sessions.lock().expect("moonlight lock");
    if let Some(session) = sessions.get_mut(&key) {
        session.status = match outcome {
            Ok(host) => Status::Adopted(Box::new(host)),
            Err(err) => {
                warn!(%key, "adopting after pairing: {err}");
                Status::Failed(format!("paired, but adopting failed: {err}"))
            }
        };
        session.ended = Some(db::now());
    }
}

async fn hosts(
    AxumState(state): AxumState<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    if user.role == Role::Guest {
        return Err(ApiError::forbidden(
            "guests_cannot_launch",
            "guests can't launch environments",
        ));
    }
    let mut views = Vec::new();
    for row in db::moonlight_hosts(&state.db).await? {
        views.push(host_view(&state, &row).await?);
    }
    Ok(Json(json!({ "hosts": views })))
}

async fn host_row(state: &AppState, id: &str) -> ApiResult<MoonlightHostRow> {
    db::moonlight_host(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("no such Moonlight host".into()))
}

/// Asks the node for the host's apps again and stores them.
async fn refresh_apps(state: &AppState, row: &MoonlightHostRow) -> ApiResult<MoonlightHostRow> {
    let apps = fetch_apps(state, &row.node_id, &row.unique_id).await?;
    db::set_moonlight_apps(
        &state.db,
        &row.id,
        &serde_json::to_string(&apps).unwrap_or_default(),
    )
    .await?;
    host_row(state, &row.id).await
}

async fn refresh(
    AxumState(state): AxumState<AppState>,
    _: AdminUser,
    Path(id): Path<String>,
) -> ApiResult<Json<AdoptedHost>> {
    let row = host_row(&state, &id).await?;
    let row = refresh_apps(&state, &row).await?;
    Ok(Json(host_view(&state, &row).await?))
}

async fn remove(
    AxumState(state): AxumState<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let row = host_row(&state, &id).await?;
    // Nobody launches in between: the check and the delete are one step.
    let _launching = state.moonlight.launch.lock().await;
    if let Some((_, owner)) = db::live_moonlight_environment(&state.db, &id).await? {
        return Err(ApiError::conflict(
            "host_busy",
            format!("{owner} is streaming from {}; stop it first", row.name),
        ));
    }
    db::delete_moonlight_host(&state.db, &id).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "moonlight.removed",
        Some(&id),
        Some(json!({ "name": row.name })),
        client.ip.as_deref(),
    )
    .await?;
    info!(host = %row.name, "Moonlight host removed");
    Ok(StatusCode::NO_CONTENT)
}

// ---- Keeping in step with the nodes ----

/// A node sent its host list: keep adopted hosts' addresses current, finish
/// pairings the list shows are done (the node's own report may have been lost
/// to a reconnect), and on a node's first list, ask for its hosts' apps again.
pub async fn hosts_changed(state: AppState, node_id: String, first: bool) {
    let found = state.moonlight.found_on(&node_id).unwrap_or_default();
    let rows = match db::moonlight_hosts(&state.db).await {
        Ok(rows) => rows,
        Err(err) => {
            warn!("reading Moonlight hosts: {err}");
            return;
        }
    };
    for row in rows.iter().filter(|r| r.node_id == node_id) {
        let Some(seen) = found.iter().find(|h| h.unique_id == row.unique_id) else {
            continue;
        };
        let codecs = serde_json::to_string(&seen.codecs).unwrap_or_default();
        let changed = row.name != seen.name
            || row.address.as_deref() != Some(&seen.address)
            || row.http_port != Some(i64::from(seen.http_port))
            || row.https_port != Some(i64::from(seen.https_port))
            || row.codecs.as_deref() != Some(&codecs);
        if changed
            && let Err(err) = db::update_moonlight_host_found(
                &state.db,
                &row.id,
                &seen.name,
                &seen.address,
                i64::from(seen.http_port),
                i64::from(seen.https_port),
                &codecs,
            )
            .await
        {
            warn!(host = %row.name, "updating a Moonlight host: {err}");
        }
        if first
            && seen.paired
            && let Err(err) = refresh_apps(&state, row).await
        {
            warn!(host = %row.name, "refreshing a Moonlight host's apps: {err}");
        }
    }
    for host in found.iter().filter(|h| h.paired) {
        pairing_finished(
            state.clone(),
            node_id.clone(),
            host.unique_id.clone(),
            true,
            None,
        )
        .await;
    }
}

/// Asks for every adopted host's apps again, every hour.
pub async fn refresh_loop(state: AppState) {
    loop {
        tokio::time::sleep(REFRESH_EVERY).await;
        let Ok(rows) = db::moonlight_hosts(&state.db).await else {
            continue;
        };
        for row in rows {
            let seen = state.moonlight.host_on(&row.node_id, &row.unique_id);
            if seen.is_some_and(|h| h.paired)
                && let Err(err) = refresh_apps(&state, &row).await
            {
                warn!(host = %row.name, "hourly refresh of a Moonlight host's apps: {err}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_ids_name_a_host_and_an_app() {
        assert_eq!(
            parse_template_id("moonlight:0192-ab:881448767"),
            Some(("0192-ab", 881448767))
        );
        assert_eq!(parse_template_id("moonlight:0192-ab:x"), None);
        assert_eq!(parse_template_id("moonlight:0192-ab"), None);
        assert_eq!(parse_template_id("chrome"), None);
    }

    #[test]
    fn keys_split_into_node_and_host() {
        assert_eq!(split_key("n1:AB12"), Some(("n1", "AB12")));
        assert_eq!(split_key("n1:"), None);
        assert_eq!(split_key("nothing"), None);
    }
}
