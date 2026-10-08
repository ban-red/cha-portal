//! Nodes: enrollment, the one WebSocket each node keeps open (ADR 0001), and
//! the admin API.
//!
//! A node proves its identity on every connection by signing the portal's
//! random challenge with the Ed25519 key it enrolled with, so no shared secret
//! crosses the wire after enrollment, and any reverse proxy can sit in between.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::{delete, get, post};
use cha_wire::{
    EnrollRequest, EnrollResponse, Inventory, NodeRequest, NodeResponse, NodeUsage,
    PROTOCOL_VERSION, PortalRequest, PortalResponse, ToNode, ToPortal, close,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{info, warn};

use crate::AppState;
use crate::auth::{AdminUser, ClientInfo, random_token, token_hash};
use crate::db::{self, NodeRow};
use crate::error::{ApiError, ApiResult};

pub const HEARTBEAT_SECS: u64 = 30;
/// A node counts as gone after this many missed heartbeats.
const MISSED_HEARTBEATS: u64 = 3;
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Node messages are small; this bounds what an unauthenticated peer can send.
const MAX_MESSAGE_BYTES: usize = 1 << 20;
/// A node's usage counts as stale after this long without a report (they come
/// every few seconds).
const USAGE_FRESH: Duration = Duration::from_secs(15);
/// Join tokens carry a prefix so they're recognisable in a shell history or a
/// secret scanner.
pub const JOIN_TOKEN_PREFIX: &str = "chajoin_";

/// `/api/node/*` for agents, `/api/nodes/*` for admins.
pub fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/node/enroll", post(enroll))
        .route("/node/connect", get(connect))
        .route("/nodes", get(list))
        .route("/nodes/join-tokens", post(create_join_token))
        .route("/nodes/discovered", get(crate::claim::discovered))
        .route("/nodes/discovered/claim", post(crate::claim::claim))
        .route("/nodes/{id}", delete(remove).patch(rename))
        .route("/nodes/{id}/ping", post(ping))
}

type Reply = Result<NodeResponse, String>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>;

enum Outgoing {
    /// Boxed: a request carries a whole environment spec, far larger than
    /// a close.
    Message(Box<ToNode>),
    Close(u16, &'static str),
}

struct Connection {
    /// Tells this connection apart from the same node's later ones.
    serial: u64,
    tx: mpsc::UnboundedSender<Outgoing>,
    pending: Pending,
    connected_at: i64,
}

/// Live node connections, and request/response correlation over them.
#[derive(Default)]
pub struct NodeHub {
    connections: Mutex<HashMap<String, Connection>>,
    /// The latest usage each connected node reported, and when. In memory
    /// only: it is for a live view and means nothing after a restart.
    usage: std::sync::Mutex<HashMap<String, (NodeUsage, Instant, i64)>>,
    /// How far each starting environment's download is. In memory only: it
    /// is a live view, gone when the environment runs or ends.
    progress: std::sync::Mutex<HashMap<String, Progress>>,
    next_id: AtomicU64,
}

/// A counted step of a starting environment (an image download).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub unit: String,
}

impl Progress {
    /// What a node's report counts, if it does: both numbers and a total above
    /// zero. `done` never passes `total`; the unit defaults to `bytes`.
    pub fn from_report(
        done: Option<u64>,
        total: Option<u64>,
        unit: Option<String>,
    ) -> Option<Self> {
        let (done, total) = (done?, total?);
        (total > 0).then(|| Self {
            done: done.min(total),
            total,
            unit: unit
                .map(|u| u.chars().take(16).collect())
                .unwrap_or_else(|| "bytes".to_string()),
        })
    }
}

/// More environments than this starting at once isn't real; the map stays
/// bounded whatever a node sends.
const PROGRESS_KEPT: usize = 256;

impl NodeHub {
    pub fn set_progress(&self, id: &str, progress: Option<Progress>) {
        let mut map = self.progress.lock().expect("progress lock");
        match progress {
            Some(p) if map.len() < PROGRESS_KEPT || map.contains_key(id) => {
                map.insert(id.to_string(), p);
            }
            Some(_) => {}
            None => {
                map.remove(id);
            }
        }
    }

    pub fn progress(&self, id: &str) -> Option<Progress> {
        self.progress
            .lock()
            .expect("progress lock")
            .get(id)
            .cloned()
    }

    pub fn clear_progress(&self, id: &str) {
        self.set_progress(id, None);
    }

    pub async fn connected_since(&self, node_id: &str) -> Option<i64> {
        self.connections
            .lock()
            .await
            .get(node_id)
            .map(|c| c.connected_at)
    }

    fn record_usage(&self, node_id: &str, usage: NodeUsage) {
        self.usage
            .lock()
            .expect("usage lock")
            .insert(node_id.to_string(), (usage, Instant::now(), db::now()));
    }

    /// What the node last reported and when (unix seconds), unless that was
    /// long ago.
    pub fn usage(&self, node_id: &str) -> Option<(NodeUsage, i64)> {
        self.usage_at(node_id, Instant::now())
    }

    fn usage_at(&self, node_id: &str, now: Instant) -> Option<(NodeUsage, i64)> {
        let usage = self.usage.lock().expect("usage lock");
        let (usage, seen, at) = usage.get(node_id)?;
        (now.saturating_duration_since(*seen) <= USAGE_FRESH).then(|| (usage.clone(), *at))
    }

    /// Sends `request` to a connected node and waits for its reply.
    pub async fn request(&self, node_id: &str, request: NodeRequest) -> ApiResult<NodeResponse> {
        self.request_timeout(node_id, request, REQUEST_TIMEOUT)
            .await
    }

    /// [`Self::request`] with its own deadline (starting an environment can take
    /// a while).
    pub async fn request_timeout(
        &self,
        node_id: &str,
        request: NodeRequest,
        timeout: Duration,
    ) -> ApiResult<NodeResponse> {
        let offline = || ApiError::conflict("node_offline", "the node isn't connected");
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, reply_rx) = oneshot::channel();
        let pending = {
            let conns = self.connections.lock().await;
            let conn = conns.get(node_id).ok_or_else(offline)?;
            conn.pending.lock().await.insert(id, reply_tx);
            conn.tx
                .send(Outgoing::Message(Box::new(ToNode::Request { id, request })))
                .map_err(|_| offline())?;
            Arc::clone(&conn.pending)
        };
        let reply = tokio::time::timeout(timeout, reply_rx).await;
        pending.lock().await.remove(&id);
        match reply {
            Ok(Ok(Ok(response))) => Ok(response),
            Ok(Ok(Err(message))) => Err(ApiError::conflict("node_error", message)),
            Ok(Err(_)) => Err(ApiError::conflict("node_offline", "the node disconnected")),
            Err(_) => Err(ApiError::conflict(
                "node_timeout",
                "the node didn't answer in time",
            )),
        }
    }

    /// Answers a request the node made, on its connection (if it still has
    /// one: a node that went away has no use for the answer).
    pub async fn reply(&self, node_id: &str, id: u64, result: Result<PortalResponse, String>) {
        if let Some(conn) = self.connections.lock().await.get(node_id) {
            let _ = conn
                .tx
                .send(Outgoing::Message(Box::new(ToNode::Response { id, result })));
        }
    }

    /// Hangs up on a node, telling it why.
    async fn kick(&self, node_id: &str, code: u16, reason: &'static str) {
        if let Some(conn) = self.connections.lock().await.remove(node_id) {
            let _ = conn.tx.send(Outgoing::Close(code, reason));
            self.usage.lock().expect("usage lock").remove(node_id);
        }
    }

    /// Registers a verified connection, replacing (and closing) an older one.
    async fn register(&self, node_id: &str, conn: Connection) {
        let old = self
            .connections
            .lock()
            .await
            .insert(node_id.to_string(), conn);
        if let Some(old) = old {
            let _ = old.tx.send(Outgoing::Close(
                close::REPLACED,
                "a newer connection took over",
            ));
        }
    }

    /// Forgets a connection that ended, unless a newer one already replaced it.
    async fn unregister(&self, node_id: &str, serial: u64) {
        let mut conns = self.connections.lock().await;
        if conns.get(node_id).is_some_and(|c| c.serial == serial) {
            conns.remove(node_id);
            self.usage.lock().expect("usage lock").remove(node_id);
        }
    }
}

// ---- Enrollment and the node connection ----

async fn enroll(
    State(state): State<AppState>,
    client: ClientInfo,
    Json(req): Json<EnrollRequest>,
) -> ApiResult<Json<EnrollResponse>> {
    cha_wire::parse_public_key(&req.public_key)
        .map_err(|e| ApiError::bad_request("bad_public_key", format!("public key: {e}")))?;
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(ApiError::bad_request(
            "bad_name",
            "node names are 1–64 characters",
        ));
    }
    let agent_version: String = req.agent_version.trim().chars().take(64).collect();
    let node_id = db::new_id();
    let redeemed = match db::redeem_join_token(
        &state.db,
        &token_hash(req.token.trim()),
        &node_id,
        name,
        &req.public_key,
        &agent_version,
    )
    .await
    {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(ApiError::conflict(
                "already_enrolled",
                "a node with this key is already enrolled",
            ));
        }
        other => other?,
    };
    if !redeemed {
        db::audit(
            &state.db,
            None,
            "node.enroll_failed",
            None,
            Some(json!({ "name": name })),
            client.ip.as_deref(),
        )
        .await?;
        return Err(ApiError::forbidden(
            "bad_join_token",
            "the join token is unknown, used or expired",
        ));
    }
    db::audit(
        &state.db,
        None,
        "node.enrolled",
        Some(&node_id),
        Some(json!({ "name": name, "agentVersion": agent_version })),
        client.ip.as_deref(),
    )
    .await?;
    info!(%node_id, name, "node enrolled");
    Ok(Json(EnrollResponse { node_id }))
}

async fn connect(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            if let Err(err) = serve_node(state, socket).await {
                warn!("node connection: {err:#}");
            }
        })
}

async fn send(socket: &mut WebSocket, msg: &ToNode) -> anyhow::Result<()> {
    socket
        .send(Message::Text(serde_json::to_string(msg)?.into()))
        .await?;
    Ok(())
}

async fn close_with(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    let _ = socket.send(Message::Close(Some(frame))).await;
}

/// One node connection: challenge → signed hello → welcome → messages.
async fn serve_node(state: AppState, mut socket: WebSocket) -> anyhow::Result<()> {
    let nonce = random_token()?;
    let challenge = ToNode::Challenge {
        nonce: nonce.clone(),
        protocol: PROTOCOL_VERSION,
    };
    send(&mut socket, &challenge).await?;

    let Ok(Some(Ok(Message::Text(text)))) =
        tokio::time::timeout(HELLO_TIMEOUT, socket.recv()).await
    else {
        anyhow::bail!("no hello");
    };
    let ToPortal::Hello {
        node_id,
        signature,
        agent_version,
        protocol,
    } = serde_json::from_str(&text)?
    else {
        anyhow::bail!("the first message wasn't a hello");
    };
    let Some(node) = db::node_by_id(&state.db, &node_id).await? else {
        close_with(&mut socket, close::UNKNOWN_NODE, "this node isn't enrolled").await;
        anyhow::bail!("unknown or removed node {node_id}");
    };
    let message = cha_wire::hello_message(&nonce, &node_id);
    if let Err(err) = cha_wire::verify_b64(&node.public_key, &message, &signature) {
        close_with(&mut socket, close::BAD_SIGNATURE, "the hello didn't verify").await;
        anyhow::bail!("node {node_id} failed the challenge: {err}");
    }
    if protocol != PROTOCOL_VERSION {
        warn!(%node_id, protocol, "node speaks a different protocol version");
    }
    let agent_version: String = agent_version.chars().take(64).collect();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let pending = Pending::default();
    let serial = state.nodes.next_id.fetch_add(1, Ordering::Relaxed);
    let conn = Connection {
        serial,
        tx,
        pending: Arc::clone(&pending),
        connected_at: db::now(),
    };
    state.nodes.register(&node_id, conn).await;
    db::touch_node(&state.db, &node_id, Some(&agent_version)).await?;
    let welcome = ToNode::Welcome {
        node_id: node_id.clone(),
        heartbeat_secs: HEARTBEAT_SECS,
        environment_warnings: true,
        node_usage: true,
        moonlight: true,
        gamestream: true,
        gamestream_launch: true,
    };
    send(&mut socket, &welcome).await?;
    info!(%node_id, name = %node.name, %agent_version, "node connected");

    let result = pump(&state, &node_id, &mut socket, &mut rx, &pending).await;
    state.nodes.unregister(&node_id, serial).await;
    // Its list is stale once it is gone (unless a newer connection took over).
    if state.nodes.connected_since(&node_id).await.is_none() {
        state.moonlight.forget(&node_id);
        state.gamestream.forget_node(&node_id);
    }
    info!(%node_id, "node disconnected");
    result
}

async fn pump(
    state: &AppState,
    node_id: &str,
    socket: &mut WebSocket,
    rx: &mut mpsc::UnboundedReceiver<Outgoing>,
    pending: &Pending,
) -> anyhow::Result<()> {
    let deadline = Duration::from_secs(HEARTBEAT_SECS * MISSED_HEARTBEATS);
    let mut last_heard = Instant::now();
    let mut watchdog = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            outgoing = rx.recv() => match outgoing {
                Some(Outgoing::Message(msg)) => send(socket, &msg).await?,
                Some(Outgoing::Close(code, reason)) => {
                    close_with(socket, code, reason).await;
                    return Ok(());
                }
                None => return Ok(()),
            },
            incoming = socket.recv() => {
                let text = match incoming {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    // Pings are answered by the WebSocket layer.
                    Some(Ok(_)) => continue,
                    Some(Err(err)) => return Err(err.into()),
                };
                last_heard = Instant::now();
                match serde_json::from_str::<ToPortal>(&text)? {
                    ToPortal::Heartbeat => db::touch_node(&state.db, node_id, None).await?,
                    ToPortal::Usage { usage } => state.nodes.record_usage(node_id, usage),
                    ToPortal::MoonlightHosts { hosts } => {
                        let first = state.moonlight.set_found(node_id, hosts);
                        tokio::spawn(crate::moonlight::hosts_changed(
                            state.clone(),
                            node_id.to_string(),
                            first,
                        ));
                    }
                    ToPortal::MoonlightPaired { unique_id, ok, message } => {
                        tokio::spawn(crate::moonlight::pairing_finished(
                            state.clone(),
                            node_id.to_string(),
                            unique_id,
                            ok,
                            message,
                        ));
                    }
                    ToPortal::Inventory { inventory } => {
                        let json = serde_json::to_string(&inventory)?;
                        db::set_node_inventory(&state.db, node_id, &json).await?;
                        tokio::spawn(crate::gamestream::inventory_changed(
                            state.clone(),
                            node_id.to_string(),
                            inventory,
                        ));
                    }
                    ToPortal::GameStreamPairRequest { attempt_id, device_name, address, expires_in_secs } => {
                        let device_name = device_name.chars().take(64).collect();
                        let address = address.chars().take(64).collect();
                        state.gamestream.open(node_id, attempt_id, device_name, address, expires_in_secs);
                    }
                    ToPortal::GameStreamPaired { attempt_id, fingerprint, unique_id, name } => {
                        tokio::spawn(crate::gamestream::paired(
                            state.clone(),
                            node_id.to_string(),
                            attempt_id,
                            fingerprint,
                            unique_id,
                            name,
                        ));
                    }
                    ToPortal::GameStreamPairFailed { attempt_id, reason } => {
                        tokio::spawn(crate::gamestream::pair_failed(
                            state.clone(),
                            node_id.to_string(),
                            attempt_id,
                            reason,
                        ));
                    }
                    ToPortal::GameStreamUnpaired { fingerprint } => {
                        tokio::spawn(crate::gamestream::unpaired(
                            state.clone(),
                            node_id.to_string(),
                            fingerprint,
                        ));
                    }
                    // These may ask the node things back, so they run apart from
                    // this loop, which carries the answers. What the portal
                    // expects is read first, though: no start the node answers
                    // after its list can then be taken for one it lost.
                    ToPortal::Environments { running } => {
                        match db::node_environments(&state.db, node_id).await {
                            Ok(expected) => {
                                tokio::spawn(crate::environments::reconcile(
                                    state.clone(),
                                    node_id.to_string(),
                                    expected,
                                    running,
                                ));
                            }
                            Err(err) => warn!(%node_id, "reconciling: {err}"),
                        }
                    }
                    ToPortal::EnvironmentProgress {
                        id,
                        detail,
                        done,
                        total,
                        unit,
                    } => {
                        let detail: String = detail.chars().take(200).collect();
                        // Only the environment's own node, while it starts.
                        let counted = db::set_environment_progress(
                            &state.db, &id, node_id, &detail,
                        )
                        .await?;
                        let progress = counted
                            .then(|| Progress::from_report(done, total, unit))
                            .flatten();
                        state.nodes.set_progress(&id, progress);
                    }
                    ToPortal::EnvironmentWarning { id, warning } => {
                        let warning = warning.map(|w| w.chars().take(300).collect::<String>());
                        db::set_environment_warning(&state.db, &id, node_id, warning.as_deref())
                            .await?;
                    }
                    ToPortal::EnvironmentExited {
                        id,
                        detail,
                        failed,
                        log,
                    } => {
                        tokio::spawn(crate::environments::exited(
                            state.clone(),
                            id,
                            detail,
                            failed,
                            log,
                        ));
                    }
                    ToPortal::Response { id, result } => {
                        if let Some(waiter) = pending.lock().await.remove(&id) {
                            let _ = waiter.send(result);
                        }
                    }
                    ToPortal::Request { id, request } => match request {
                        PortalRequest::Ping => {
                            let result = Ok(PortalResponse::Pong { unix_ms: unix_ms() });
                            send(socket, &ToNode::Response { id, result }).await?;
                        }
                        // These wait for environments, so they run apart from
                        // this loop and answer through the hub.
                        request @ (PortalRequest::GameStreamLaunch { .. }
                        | PortalRequest::GameStreamStop { .. }) => {
                            tokio::spawn(crate::gamestream::node_request(
                                state.clone(),
                                node_id.to_string(),
                                id,
                                request,
                            ));
                        }
                    },
                    ToPortal::Hello { .. } => anyhow::bail!("a second hello"),
                }
            }
            _ = watchdog.tick() => {
                if last_heard.elapsed() > deadline {
                    anyhow::bail!("{MISSED_HEARTBEATS} heartbeats missed");
                }
            }
        }
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

// ---- Admin API ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeView {
    #[serde(flatten)]
    node: NodeRow,
    online: bool,
    connected_at: Option<i64>,
    inventory: Option<Inventory>,
    /// What it uses now; null while offline or when it hasn't reported lately.
    usage: Option<UsageView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageView {
    #[serde(flatten)]
    usage: NodeUsage,
    /// When it was reported (unix seconds).
    at: i64,
}

async fn list(State(state): State<AppState>, _: AdminUser) -> ApiResult<Json<Vec<NodeView>>> {
    let mut out = Vec::new();
    for node in db::list_nodes(&state.db).await? {
        let connected_at = state.nodes.connected_since(&node.id).await;
        let inventory = node
            .inventory
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok());
        let usage = if connected_at.is_some() {
            state
                .nodes
                .usage(&node.id)
                .map(|(usage, at)| UsageView { usage, at })
        } else {
            None
        };
        out.push(NodeView {
            online: connected_at.is_some(),
            usage,
            connected_at,
            inventory,
            node,
        });
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct JoinTokenRequest {
    label: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JoinTokenResponse {
    /// Shown once; only its hash is stored.
    token: String,
    expires_at: i64,
}

async fn create_join_token(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Json(req): Json<JoinTokenRequest>,
) -> ApiResult<Json<JoinTokenResponse>> {
    let token = format!("{JOIN_TOKEN_PREFIX}{}", random_token()?);
    let label: Option<String> = req
        .label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.chars().take(64).collect());
    let expires_at =
        db::insert_join_token(&state.db, &token_hash(&token), label.as_deref(), &admin.id).await?;
    db::audit(
        &state.db,
        Some(&admin.id),
        "node.join_token_created",
        None,
        label.map(|l| json!({ "label": l })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(JoinTokenResponse { token, expires_at }))
}

async fn remove(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    db::fail_node_environments(&state.db, &id, "its node was removed").await?;
    if !db::delete_node(&state.db, &id).await? {
        return Err(ApiError::NotFound("no such node".into()));
    }
    state
        .nodes
        .kick(&id, close::UNKNOWN_NODE, "this node was removed")
        .await;
    db::audit(
        &state.db,
        Some(&admin.id),
        "node.removed",
        Some(&id),
        None,
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(json!({ "removed": id })))
}

#[derive(Deserialize)]
struct RenameRequest {
    name: String,
}

async fn rename(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    Path(id): Path<String>,
    Json(req): Json<RenameRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(ApiError::bad_request(
            "bad_name",
            "node names are 1–64 characters",
        ));
    }
    if !db::rename_node(&state.db, &id, name).await? {
        return Err(ApiError::NotFound("no such node".into()));
    }
    db::audit(
        &state.db,
        Some(&admin.id),
        "node.renamed",
        Some(&id),
        Some(json!({ "name": name })),
        client.ip.as_deref(),
    )
    .await?;
    Ok(Json(json!({ "id": id, "name": name })))
}

async fn ping(
    State(state): State<AppState>,
    _: AdminUser,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let started = Instant::now();
    let NodeResponse::Pong { unix_ms } = state.nodes.request(&id, NodeRequest::Ping).await? else {
        return Err(ApiError::conflict(
            "node_error",
            "the node answered something else",
        ));
    };
    Ok(Json(json!({
        "rttMs": started.elapsed().as_secs_f64() * 1000.0,
        "nodeUnixMs": unix_ms,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_counts_only_when_the_node_gives_both_numbers() {
        let p = |d, t, u: Option<&str>| Progress::from_report(d, t, u.map(String::from));
        assert_eq!(p(None, None, None), None);
        assert_eq!(p(Some(1), None, None), None);
        assert_eq!(p(Some(1), Some(0), None), None);
        let got = p(Some(900), Some(890), None).unwrap();
        assert_eq!(
            (got.done, got.total, got.unit.as_str()),
            (890, 890, "bytes")
        );
        assert_eq!(p(Some(1), Some(2), Some("files")).unwrap().unit, "files");
        let hub = NodeHub::default();
        hub.set_progress("e1", p(Some(1), Some(2), None));
        assert_eq!(hub.progress("e1").unwrap().done, 1);
        hub.clear_progress("e1");
        assert!(hub.progress("e1").is_none());
    }

    #[test]
    fn usage_is_kept_until_it_is_stale_or_the_node_goes() {
        let hub = NodeHub::default();
        assert!(hub.usage("n1").is_none());
        hub.record_usage(
            "n1",
            NodeUsage {
                cpu: 40.0,
                ..NodeUsage::default()
            },
        );
        let (usage, at) = hub.usage("n1").unwrap();
        assert_eq!(usage.cpu, 40.0);
        assert!(at > 0);
        assert!(hub.usage("n2").is_none());
        let later = Instant::now() + USAGE_FRESH + Duration::from_secs(1);
        assert!(hub.usage_at("n1", later).is_none());
        // A newer report revives it.
        hub.record_usage("n1", NodeUsage::default());
        assert!(hub.usage("n1").is_some());
    }
}
