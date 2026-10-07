//! Moonlight clients paired with nodes' GameStream hosts (ADR 0009).
//!
//! A node's host asks the portal to pair a client ([`ToPortal::GameStreamPairRequest`]);
//! the PIN the client shows is typed into the portal by a signed-in user, who
//! becomes the device's owner. The portal sends the PIN to the node, which
//! finishes the handshake and reports the device ([`ToPortal::GameStreamPaired`])
//! or that it failed. Devices are rows in `gamestream_devices`; the node gets
//! its list on every connect and after every change ([`push_devices`]), and
//! it is the node's only record of who is paired. Pending requests live in
//! memory ([`State`]): they are minutes old at most and mean nothing after a
//! restart or a disconnect.
//!
//! The node's host also asks the portal for what a paired client's launch needs:
//! its owner's environment of an app started on that node, and stopped again
//! when the client quits ([`node_request`]). Those go through the same launch
//! and stop code as the API's, with the owner as the user and the node fixed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State as AxumState};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use cha_wire::{GameStreamDevice, Inventory, NodeRequest, PortalRequest, PortalResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::AppState;
use crate::auth::{ClientInfo, CurrentUser};
use crate::db::{self, GameStreamDeviceRow, Role};
use crate::environments::{self, Caller, LaunchRequest};
use crate::error::{ApiError, ApiResult};
use crate::moonlight;

/// How long `POST /gamestream/pairing/{id}` waits, in milliseconds, for the
/// node to say the handshake finished.
const OUTCOME_WAIT_MS: u64 = 20_000;
/// What a node may ask for: its own attempts last two minutes.
const MAX_TTL_SECS: u64 = 300;
/// Open requests kept per node; a node can't fill the portal's memory.
const MAX_PER_NODE: usize = 16;
/// How long a node's launch waits for its environment to run: a little less
/// than the node does, so the reason arrives rather than a timeout.
const LAUNCH_WAIT: Duration = Duration::from_secs(170);
const LAUNCH_POLL: Duration = Duration::from_millis(250);
/// The audit log's `via` for what a Moonlight client asked for.
const VIA: &str = "moonlight";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/gamestream/pairing", get(pairing_requests))
        .route("/gamestream/pairing/{id}", post(submit_pin))
        .route("/gamestream/devices", get(devices))
        .route("/gamestream/devices/{id}", delete(remove_device))
        .route("/gamestream/hosts", get(hosts))
}

// ---- What the portal keeps in memory ----

/// How an attempt ended, for the request waiting on it.
enum Outcome {
    Paired(Box<GameStreamDeviceRow>),
    Failed(String),
}

struct Request {
    node_id: String,
    /// The node's name for it.
    attempt_id: String,
    device_name: String,
    address: String,
    /// Unix seconds.
    expires_at: i64,
    /// The user who typed a PIN: the device's owner if it pairs, even after
    /// their request stopped waiting.
    submitted_by: Option<String>,
    waiter: Option<oneshot::Sender<Outcome>>,
}

/// Pairing requests the nodes have open.
pub struct State {
    /// By the id the API uses.
    requests: Mutex<HashMap<String, Request>>,
    /// One list push at a time per node, so lists arrive in the order they
    /// were read.
    pushing: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// [`OUTCOME_WAIT_MS`], unless a test shortens it.
    outcome_wait_ms: AtomicU64,
    /// Launches for nodes' clients go one at a time between finding an
    /// environment to join and recording a new one, so a client that asks
    /// twice starts one environment.
    launching: tokio::sync::Mutex<()>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            requests: Mutex::default(),
            pushing: Mutex::default(),
            outcome_wait_ms: AtomicU64::new(OUTCOME_WAIT_MS),
            launching: tokio::sync::Mutex::default(),
        }
    }
}

impl State {
    /// Changes how long a PIN's request waits for the outcome (tests).
    pub fn set_outcome_wait(&self, wait: Duration) {
        self.outcome_wait_ms
            .store(wait.as_millis() as u64, Ordering::Relaxed);
    }

    /// Opens a request for what a node reported; its API id.
    pub fn open(
        &self,
        node_id: &str,
        attempt_id: String,
        device_name: String,
        address: String,
        ttl_secs: u64,
    ) -> String {
        let now = db::now();
        let id = db::new_id();
        let mut requests = self.requests.lock().expect("gamestream lock");
        requests.retain(|_, r| r.expires_at > now);
        // The same attempt again replaces itself.
        requests.retain(|_, r| !(r.node_id == node_id && r.attempt_id == attempt_id));
        let mut of_node: Vec<(String, i64)> = requests
            .iter()
            .filter(|(_, r)| r.node_id == node_id)
            .map(|(id, r)| (id.clone(), r.expires_at))
            .collect();
        of_node.sort_by_key(|(_, expires)| *expires);
        while of_node.len() >= MAX_PER_NODE {
            requests.remove(&of_node.remove(0).0);
        }
        requests.insert(
            id.clone(),
            Request {
                node_id: node_id.to_string(),
                attempt_id,
                device_name,
                address,
                expires_at: now + ttl_secs.min(MAX_TTL_SECS) as i64,
                submitted_by: None,
                waiter: None,
            },
        );
        id
    }

    /// A node went away: what it was asking is moot.
    pub fn forget_node(&self, node_id: &str) {
        self.requests
            .lock()
            .expect("gamestream lock")
            .retain(|_, r| r.node_id != node_id);
        self.pushing
            .lock()
            .expect("gamestream lock")
            .remove(node_id);
    }

    /// The requests a user can still answer: open, and with no PIN typed yet.
    fn waiting(&self) -> Vec<(String, String, String, String, i64)> {
        let now = db::now();
        let mut list: Vec<_> = self
            .requests
            .lock()
            .expect("gamestream lock")
            .iter()
            .filter(|(_, r)| r.expires_at > now && r.submitted_by.is_none())
            .map(|(id, r)| {
                (
                    id.clone(),
                    r.node_id.clone(),
                    r.device_name.clone(),
                    r.address.clone(),
                    r.expires_at,
                )
            })
            .collect();
        list.sort_by_key(|r| (r.4, r.0.clone()));
        list
    }

    /// `user` types a PIN for request `id`: the node and its attempt, and
    /// where the outcome will arrive.
    fn submit(
        &self,
        id: &str,
        user: &str,
    ) -> ApiResult<(String, String, oneshot::Receiver<Outcome>)> {
        let mut requests = self.requests.lock().expect("gamestream lock");
        let request = requests
            .get_mut(id)
            .filter(|r| r.expires_at > db::now())
            .ok_or_else(gone)?;
        if request.submitted_by.is_some() {
            return Err(ApiError::conflict(
                "pin_pending",
                "a PIN was already entered for this pairing",
            ));
        }
        let (tx, rx) = oneshot::channel();
        request.submitted_by = Some(user.to_string());
        request.waiter = Some(tx);
        Ok((request.node_id.clone(), request.attempt_id.clone(), rx))
    }

    /// The node didn't take the PIN: the request is open for another try.
    fn reopen(&self, id: &str) {
        if let Some(r) = self.requests.lock().expect("gamestream lock").get_mut(id) {
            r.submitted_by = None;
            r.waiter = None;
        }
    }

    fn drop_request(&self, id: &str) {
        self.requests.lock().expect("gamestream lock").remove(id);
    }

    /// An attempt ended: the user who typed its PIN, and the request is
    /// told. `None` for an attempt nobody typed a PIN for through the portal.
    fn finish(
        &self,
        node_id: &str,
        attempt_id: &str,
        outcome: impl FnOnce(&str) -> Outcome,
    ) -> Option<String> {
        let mut requests = self.requests.lock().expect("gamestream lock");
        let id = requests
            .iter()
            .find(|(_, r)| r.node_id == node_id && r.attempt_id == attempt_id)
            .map(|(id, _)| id.clone())?;
        let request = requests.remove(&id)?;
        let user = request.submitted_by?;
        if let Some(waiter) = request.waiter {
            let _ = waiter.send(outcome(&user));
        }
        Some(user)
    }

    /// Who typed the PIN of a node's attempt, if one did and it is still open.
    fn owner_of(&self, node_id: &str, attempt_id: &str) -> Option<String> {
        self.requests
            .lock()
            .expect("gamestream lock")
            .values()
            .find(|r| r.node_id == node_id && r.attempt_id == attempt_id)
            .and_then(|r| r.submitted_by.clone())
    }

    fn push_lock(&self, node_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.pushing
            .lock()
            .expect("gamestream lock")
            .entry(node_id.to_string())
            .or_default()
            .clone()
    }
}

fn gone() -> ApiError {
    ApiError::NotFound("this pairing request has expired; start pairing again on the device".into())
}

// ---- Views ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestView {
    id: String,
    node_id: String,
    node_name: String,
    device_name: String,
    address: String,
    /// Unix seconds.
    expires_at: i64,
}

#[derive(Serialize)]
struct OwnerView {
    id: String,
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceView {
    id: String,
    name: String,
    node_id: String,
    node_name: String,
    paired_at: i64,
    owner: OwnerView,
}

impl From<GameStreamDeviceRow> for DeviceView {
    fn from(row: GameStreamDeviceRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            node_id: row.node_id,
            node_name: row.node_name,
            paired_at: row.paired_at,
            owner: OwnerView {
                id: row.user_id,
                name: row.owner_name,
            },
        }
    }
}

fn not_for_guests(user: &db::User) -> ApiResult<()> {
    if user.role == Role::Guest {
        return Err(ApiError::forbidden(
            "guests_cannot_pair",
            "guests can't pair Moonlight devices",
        ));
    }
    Ok(())
}

// ---- The API ----

async fn pairing_requests(
    AxumState(state): AxumState<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    not_for_guests(&user)?;
    let mut views = Vec::new();
    for (id, node_id, device_name, address, expires_at) in state.gamestream.waiting() {
        let Some(node) = db::node_by_id(&state.db, &node_id).await? else {
            continue;
        };
        views.push(RequestView {
            id,
            node_id,
            node_name: node.name,
            device_name,
            address,
            expires_at,
        });
    }
    Ok(Json(json!({ "requests": views })))
}

#[derive(Deserialize)]
struct PinBody {
    pin: String,
}

async fn submit_pin(
    AxumState(state): AxumState<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<PinBody>,
) -> ApiResult<Json<serde_json::Value>> {
    not_for_guests(&user)?;
    let pin = body.pin.trim();
    if pin.len() != 4 || !pin.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::bad_request(
            "bad_pin",
            "the PIN is the four digits the device shows",
        ));
    }
    let (node_id, attempt_id, outcome) = state.gamestream.submit(&id, &user.id)?;
    let sent = state
        .nodes
        .request(
            &node_id,
            NodeRequest::GameStreamPin {
                attempt_id,
                pin: pin.to_string(),
                user_id: user.id.clone(),
            },
        )
        .await;
    if let Err(err) = sent {
        return Err(match err {
            // The node doesn't know the attempt (or says it is over).
            ApiError::Conflict("node_error", _) => {
                state.gamestream.drop_request(&id);
                gone()
            }
            other => {
                state.gamestream.reopen(&id);
                match other.node_failure() {
                    ApiError::BadGateway(_, message) => {
                        ApiError::BadGateway("node_unreachable", message)
                    }
                    other => other,
                }
            }
        });
    }
    let wait = Duration::from_millis(state.gamestream.outcome_wait_ms.load(Ordering::Relaxed));
    match tokio::time::timeout(wait, outcome).await {
        Ok(Ok(Outcome::Paired(row))) => Ok(Json(json!({ "device": DeviceView::from(*row) }))),
        Ok(Ok(Outcome::Failed(reason))) => {
            info!(%reason, "GameStream pairing failed");
            Err(ApiError::forbidden(
                "wrong_pin",
                "that PIN didn't pair the device: check the PIN it shows and start pairing again",
            ))
        }
        // The request was dropped (the node went away) or never answered.
        Ok(Err(_)) => Err(ApiError::BadGateway(
            "node_unreachable",
            "the node went away before the device paired".into(),
        )),
        Err(_) => Err(ApiError::GatewayTimeout(
            "timeout",
            "the node didn't say whether the device paired".into(),
        )),
    }
}

async fn devices(
    AxumState(state): AxumState<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    let only = (user.role != Role::Admin).then_some(user.id.as_str());
    let rows = db::gamestream_devices(&state.db, only).await?;
    let views: Vec<DeviceView> = rows.into_iter().map(Into::into).collect();
    Ok(Json(json!({ "devices": views })))
}

async fn remove_device(
    AxumState(state): AxumState<AppState>,
    CurrentUser(user): CurrentUser,
    client: ClientInfo,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let row = db::gamestream_device(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::NotFound("no such device".into()))?;
    if row.user_id != user.id && user.role != Role::Admin {
        return Err(ApiError::forbidden(
            "not_your_device",
            "that device belongs to someone else",
        ));
    }
    db::delete_gamestream_device(&state.db, &id).await?;
    db::audit(
        &state.db,
        Some(&user.id),
        "gamestream.removed",
        Some(&id),
        Some(json!({ "name": row.name, "node": row.node_name, "owner": row.user_id })),
        client.ip.as_deref(),
    )
    .await?;
    info!(device = %row.name, node = %row.node_name, "GameStream device removed");
    // The node learns now if it is here, and on its next connect if not.
    push_devices(&state, &row.node_id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn hosts(
    AxumState(state): AxumState<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    not_for_guests(&user)?;
    let mut hosts = Vec::new();
    for node in db::list_nodes(&state.db).await? {
        if state.nodes.connected_since(&node.id).await.is_none() {
            continue;
        }
        let Some(inventory) = node
            .inventory
            .as_deref()
            .and_then(|j| serde_json::from_str::<Inventory>(j).ok())
        else {
            continue;
        };
        let (Some(gamestream), Some(address)) =
            (&inventory.gamestream, inventory.addresses.first())
        else {
            continue;
        };
        hosts.push(json!({
            "nodeId": node.id,
            "nodeName": node.name,
            "address": address,
            "httpPort": gamestream.http_port,
        }));
    }
    Ok(Json(json!({ "hosts": hosts })))
}

// ---- What the nodes tell us ----

/// Sends a node its devices: the whole list, as the database has it now.
/// A node that is away gets it when it connects; one that refuses is logged.
pub async fn push_devices(state: &AppState, node_id: &str) {
    let lock = state.gamestream.push_lock(node_id);
    let _one_at_a_time = lock.lock().await;
    let rows = match db::gamestream_devices_of_node(&state.db, node_id).await {
        Ok(rows) => rows,
        Err(err) => {
            warn!(%node_id, "reading GameStream devices: {err}");
            return;
        }
    };
    let devices = rows
        .into_iter()
        .map(|d| GameStreamDevice {
            fingerprint: d.fingerprint,
            unique_id: d.unique_id,
            name: d.name,
            user_id: d.user_id,
        })
        .collect();
    match state
        .nodes
        .request(node_id, NodeRequest::GameStreamDevices { devices })
        .await
    {
        Ok(_) => {}
        Err(ApiError::Conflict("node_offline", _)) => {}
        Err(err) => warn!(%node_id, "sending GameStream devices: {err}"),
    }
}

/// A node's inventory arrived: if it runs a host, it needs its devices.
pub async fn inventory_changed(state: AppState, node_id: String, inventory: Inventory) {
    if inventory.gamestream.is_some() {
        push_devices(&state, &node_id).await;
    }
}

/// A client paired: keep the device for whoever typed its PIN.
pub async fn paired(
    state: AppState,
    node_id: String,
    attempt_id: String,
    fingerprint: String,
    unique_id: String,
    name: String,
) {
    let name: String = name.trim().chars().take(64).collect();
    let name = if name.is_empty() {
        "Moonlight".to_string()
    } else {
        name
    };
    let valid = fingerprint.len() == 64 && fingerprint.bytes().all(|b| b.is_ascii_hexdigit());
    // Who typed the PIN, settled before the row exists so the waiting request
    // can be told the device.
    let Some(owner) = state.gamestream.owner_of(&node_id, &attempt_id) else {
        warn!(%node_id, "a GameStream device paired with no PIN typed in the portal; not keeping it");
        push_devices(&state, &node_id).await;
        return;
    };
    if !valid {
        warn!(%node_id, "a GameStream device paired with a malformed fingerprint");
        state.gamestream.finish(&node_id, &attempt_id, |_| {
            Outcome::Failed("bad fingerprint".into())
        });
        push_devices(&state, &node_id).await;
        return;
    }
    let unique_id = Some(unique_id.trim()).filter(|u| !u.is_empty() && u.len() <= 64);
    let row = match db::upsert_gamestream_device(
        &state.db,
        &owner,
        &node_id,
        &fingerprint.to_ascii_lowercase(),
        unique_id,
        &name,
    )
    .await
    {
        Ok(row) => row,
        Err(err) => {
            warn!(%node_id, "keeping a GameStream device: {err}");
            state.gamestream.finish(&node_id, &attempt_id, |_| {
                Outcome::Failed("not stored".into())
            });
            push_devices(&state, &node_id).await;
            return;
        }
    };
    if let Err(err) = db::audit(
        &state.db,
        Some(&owner),
        "gamestream.paired",
        Some(&row.id),
        Some(json!({ "name": row.name, "node": row.node_name })),
        None,
    )
    .await
    {
        warn!("auditing a pairing: {err}");
    }
    info!(device = %row.name, node = %row.node_name, "GameStream device paired");
    state
        .gamestream
        .finish(&node_id, &attempt_id, |_| Outcome::Paired(Box::new(row)));
    push_devices(&state, &node_id).await;
}

/// An attempt ended without a device.
pub async fn pair_failed(state: AppState, node_id: String, attempt_id: String, reason: String) {
    let reason: String = reason.chars().take(200).collect();
    let said = reason.clone();
    let user = state
        .gamestream
        .finish(&node_id, &attempt_id, move |_| Outcome::Failed(said));
    if let Err(err) = db::audit(
        &state.db,
        user.as_deref(),
        "gamestream.pair_failed",
        Some(&node_id),
        Some(json!({ "reason": reason })),
        None,
    )
    .await
    {
        warn!("auditing a failed pairing: {err}");
    }
}

/// A client unpaired itself on the node.
pub async fn unpaired(state: AppState, node_id: String, fingerprint: String) {
    match db::delete_gamestream_device_of(&state.db, &node_id, &fingerprint.to_ascii_lowercase())
        .await
    {
        Ok(Some(row)) => {
            if let Err(err) = db::audit(
                &state.db,
                None,
                "gamestream.removed",
                Some(&row.id),
                Some(json!({ "name": row.name, "node": row.node_name, "owner": row.user_id, "by": "the device" })),
                None,
            )
            .await
            {
                warn!("auditing an unpairing: {err}");
            }
            info!(device = %row.name, node = %row.node_name, "GameStream device unpaired itself");
        }
        Ok(None) => {}
        Err(err) => warn!(%node_id, "forgetting a GameStream device: {err}"),
    }
    push_devices(&state, &node_id).await;
}

// ---- What the nodes ask of us: launching and stopping for a client ----

/// A node's request: answered on its connection, however long it takes.
pub async fn node_request(state: AppState, node_id: String, id: u64, request: PortalRequest) {
    let result = match request {
        PortalRequest::GameStreamLaunch {
            user_id,
            template_id,
        } => launch_for_client(&state, &node_id, &user_id, &template_id).await,
        PortalRequest::GameStreamStop {
            user_id,
            environment_id,
        } => stop_for_client(&state, &node_id, &user_id, &environment_id).await,
        PortalRequest::Ping => return,
    };
    if let Err(why) = &result {
        info!(%node_id, "refused a Moonlight client's request: {why}");
    }
    state.nodes.reply(&node_id, id, result).await;
}

/// The user a node's host speaks for: signed up, not disabled, not a guest, and
/// with a device paired with that node (the node says who its clients are, but
/// the portal's list of devices is what lets a user act through it).
async fn paired_user(state: &AppState, node_id: &str, user_id: &str) -> Result<db::User, String> {
    let user = db::user_by_id(&state.db, user_id)
        .await
        .map_err(|e| e.to_string())?
        .filter(|u| !u.disabled)
        .ok_or("this device's owner no longer has an account here")?;
    if user.role == Role::Guest {
        return Err("guests can't launch environments".into());
    }
    let paired = db::gamestream_devices_of_node(&state.db, node_id)
        .await
        .map_err(|e| e.to_string())?
        .iter()
        .any(|d| d.user_id == user.id);
    if !paired {
        return Err("no device of yours is paired with this node".into());
    }
    Ok(user)
}

/// Starts `user`'s environment of `template_id` on `node_id` through the API's
/// launch, unless one of theirs already starts or runs there, and waits for it.
async fn launch_for_client(
    state: &AppState,
    node_id: &str,
    user_id: &str,
    template_id: &str,
) -> Result<PortalResponse, String> {
    let user = paired_user(state, node_id, user_id).await?;
    // Only what a node offers: catalog apps, never a Moonlight gateway.
    let template = environments::template(template_id)
        .filter(|t| t.class != moonlight::CLASS)
        .ok_or("no such app")?;
    let (row, created) = {
        let _one_at_a_time = state.gamestream.launching.lock().await;
        let live = db::live_environments_of(&state.db, &user.id, &template.id)
            .await
            .map_err(|e| e.to_string())?;
        let here = live.into_iter().find(|e| {
            e.node_id.as_deref() == Some(node_id)
                && matches!(e.state.as_str(), "starting" | "running")
        });
        match here {
            Some(row) => (row, false),
            None => {
                let request = LaunchRequest {
                    template_id: template.id.clone(),
                    node: Some(node_id.to_string()),
                    device: None,
                };
                let caller = Caller {
                    ip: None,
                    via: Some(VIA),
                };
                let row = environments::launch_environment(state, &user, caller, &request)
                    .await
                    .map_err(|e| e.to_string())?;
                (row, true)
            }
        }
    };
    wait_running(state, &row.id, &template.name).await?;
    Ok(PortalResponse::GameStreamLaunched {
        environment_id: row.id,
        created,
    })
}

/// Until environment `id` runs; the reason when it fails or takes too long
/// (it goes on starting, and a launch again joins it).
async fn wait_running(state: &AppState, id: &str, name: &str) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + LAUNCH_WAIT;
    loop {
        let row = db::environment_by_id(&state.db, id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the environment vanished")?;
        match row.state.as_str() {
            "running" => return Ok(()),
            "starting" => {}
            other => {
                return Err(format!(
                    "{name} didn't start: {}",
                    row.detail.as_deref().unwrap_or(other)
                ));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("{name} is still starting; try again in a moment"));
        }
        tokio::time::sleep(LAUNCH_POLL).await;
    }
}

/// Stops `user`'s environment `environment_id`, if it runs on `node_id`.
async fn stop_for_client(
    state: &AppState,
    node_id: &str,
    user_id: &str,
    environment_id: &str,
) -> Result<PortalResponse, String> {
    let user = paired_user(state, node_id, user_id).await?;
    let row = db::environment_by_id(&state.db, environment_id)
        .await
        .map_err(|e| e.to_string())?
        .filter(|e| e.owner_id == user.id && e.node_id.as_deref() == Some(node_id))
        .ok_or("no such environment of yours on this node")?;
    let caller = Caller {
        ip: None,
        via: Some(VIA),
    };
    environments::stop_environment(state, &user, caller, &row)
        .await
        .map_err(|e| e.to_string())?;
    Ok(PortalResponse::GameStreamStopped)
}
