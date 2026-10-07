//! Claiming a node found on the LAN with its pairing code (ADR 0007): the
//! admin API, and the portal's half of the exchange in [`cha_wire::claim`].

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use cha_wire::claim::{
    self, DONE_PATH, DoneRequest, ErrorBody, FINISH_PATH, FinishRequest, FinishResponse,
    PortalStart, START_PATH, StartRequest, StartResponse,
};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::AppState;
use crate::auth::{AdminUser, ClientInfo};
use crate::db;
use crate::discovery::DiscoveredNode;
use crate::error::{ApiError, ApiResult};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// A node's claim replies are tiny.
const MAX_REPLY_BYTES: usize = 64 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredList {
    enabled: bool,
    nodes: Vec<DiscoveredNode>,
}

/// `GET /api/nodes/discovered`: unclaimed nodes seen on the LAN, leaving out
/// any whose key is already enrolled.
pub async fn discovered(
    State(state): State<AppState>,
    _: AdminUser,
) -> ApiResult<Json<DiscoveredList>> {
    let enrolled: Vec<String> = db::list_nodes(&state.db)
        .await?
        .iter()
        .filter_map(|n| claim::fingerprint_b64(&n.public_key))
        .collect();
    let nodes = state
        .discovered
        .list()
        .into_iter()
        .filter(|n| !enrolled.contains(&n.fingerprint))
        .collect();
    Ok(Json(DiscoveredList {
        enabled: state.config.discover_nodes,
        nodes,
    }))
}

#[derive(Deserialize)]
pub struct ClaimRequest {
    id: String,
    code: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimReply {
    node_id: String,
    name: String,
}

/// `POST /api/nodes/discovered/claim`: runs the exchange with the node and,
/// once the node has proved it knows the code, enrolls its key.
pub async fn claim(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    client: ClientInfo,
    headers: HeaderMap,
    Json(req): Json<ClaimRequest>,
) -> ApiResult<Json<ClaimReply>> {
    let code = claim::normalize_code(&req.code)
        .ok_or_else(|| ApiError::bad_request("bad_code", "the pairing code is 8 digits"))?;
    let node = state.discovered.get(&req.id).ok_or_else(|| {
        ApiError::NotFound("that node isn't on the network any more; refresh the list".into())
    })?;
    let portal_url = portal_url(&state, &headers)?;
    let addresses = addresses(&node)?;

    let portal =
        PortalStart::new(&code).map_err(|e| ApiError::bad_request("bad_code", e.to_string()))?;
    let (started, reached) = call::<StartResponse>(
        &addresses,
        START_PATH,
        &StartRequest {
            spake: portal.message(),
            portal_url: portal_url.clone(),
        },
    )
    .await?;
    let session = portal
        .finish(&started.spake, &portal_url, &started.claim_id)
        .map_err(|_| garbled())?;
    // The rest goes to the address that answered.
    let reached = [reached];
    let (finished, _) = call::<FinishResponse>(
        &reached,
        FINISH_PATH,
        &FinishRequest {
            claim_id: started.claim_id.clone(),
            mac: session.portal_mac(),
        },
    )
    .await?;
    if session
        .verify_node_mac(&finished.public_key, &finished.mac)
        .is_err()
    {
        return Err(wrong_code());
    }
    cha_wire::parse_public_key(&finished.public_key).map_err(|_| garbled())?;
    if claim::fingerprint_b64(&finished.public_key).as_deref() != Some(node.fingerprint.as_str()) {
        return Err(ApiError::BadGateway(
            "node_unreachable",
            "the node at that address isn't the one that was advertised; refresh the list".into(),
        ));
    }
    let name = finished.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(ApiError::bad_request(
            "bad_name",
            "node names are 1–64 characters",
        ));
    }
    let agent_version: String = finished.agent_version.trim().chars().take(64).collect();

    let node_id = db::new_id();
    match db::insert_node(
        &state.db,
        &node_id,
        name,
        &finished.public_key,
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
    }
    db::audit(
        &state.db,
        Some(&admin.id),
        "node.claimed",
        Some(&node_id),
        Some(json!({
            "name": name,
            "fingerprint": node.fingerprint,
            "agentVersion": agent_version,
        })),
        client.ip.as_deref(),
    )
    .await?;
    info!(%node_id, name, admin = %admin.username, "node claimed");
    state.discovered.remove(&node.id);

    let done = call::<serde_json::Value>(
        &reached,
        DONE_PATH,
        &DoneRequest {
            claim_id: started.claim_id,
            node_id: node_id.clone(),
            mac: session.done_mac(&node_id),
        },
    )
    .await;
    if let Err(err) = done {
        warn!(%node_id, "the claimed node didn't take its id: {err}");
        return Err(ApiError::BadGateway(
            "node_unreachable",
            format!(
                "the node was enrolled but didn't receive its id ({err}), so it can't connect. \
                 Remove it under Admin → Nodes and claim it again"
            ),
        ));
    }
    Ok(Json(ClaimReply {
        node_id,
        name: name.to_string(),
    }))
}

fn wrong_code() -> ApiError {
    ApiError::forbidden(
        "wrong_code",
        "the node didn't accept that pairing code; check it against the node's log",
    )
}

fn garbled() -> ApiError {
    ApiError::BadGateway(
        "node_unreachable",
        "the node sent a reply the portal couldn't read".into(),
    )
}

/// The URL the node is told to remember the portal by: `CHA_PUBLIC_URL`, else
/// what the admin's browser is using.
fn portal_url(state: &AppState, headers: &HeaderMap) -> ApiResult<String> {
    if let Some(url) = state.config.public_url.as_deref() {
        return Ok(url.trim().trim_end_matches('/').to_string());
    }
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(origin) = text("origin").filter(|o| o.starts_with("http")) {
        return Ok(origin.trim_end_matches('/').to_string());
    }
    let host = text("host").ok_or_else(|| {
        ApiError::bad_request(
            "no_portal_url",
            "can't tell which URL the portal is reached at; set CHA_PUBLIC_URL",
        )
    })?;
    let scheme = match text("x-forwarded-proto") {
        Some(proto) if proto.eq_ignore_ascii_case("https") => "https",
        Some(_) => "http",
        None if state.config.secure_cookies => "https",
        None => "http",
    };
    Ok(format!("{scheme}://{host}"))
}

/// Where to try the node: IPv4 first.
fn addresses(node: &DiscoveredNode) -> ApiResult<Vec<SocketAddr>> {
    let mut ips: Vec<IpAddr> = node
        .addresses
        .iter()
        .filter_map(|a| a.parse().ok())
        .collect();
    ips.sort_by_key(|ip| ip.is_ipv6());
    if ips.is_empty() {
        return Err(unreachable_node("the node advertised no usable address"));
    }
    Ok(ips
        .into_iter()
        .map(|ip| SocketAddr::new(ip, node.port))
        .collect())
}

fn unreachable_node(why: impl std::fmt::Display) -> ApiError {
    ApiError::BadGateway("node_unreachable", format!("can't reach the node: {why}"))
}

/// POSTs `body` to the first of `addresses` that connects. Returns the reply
/// and the address that answered; a node's refusal becomes an [`ApiError`].
async fn call<T: serde::de::DeserializeOwned>(
    addresses: &[SocketAddr],
    path: &str,
    body: &impl Serialize,
) -> ApiResult<(T, SocketAddr)> {
    let mut last = None;
    for addr in addresses {
        match post(*addr, path, body).await {
            Ok((status, bytes)) => return Ok((reply(status, &bytes)?, *addr)),
            Err(err) => last = Some(format!("{addr}: {err}")),
        }
    }
    Err(unreachable_node(last.unwrap_or_default()))
}

/// One request on its own connection, `Err` if the node can't be talked to.
async fn post(
    addr: SocketAddr,
    path: &str,
    body: &impl Serialize,
) -> Result<(StatusCode, Bytes), String> {
    let exchange = async {
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr))
            .await
            .map_err(|_| "timed out connecting".to_string())?
            .map_err(|e| e.to_string())?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(conn);
        let request = hyper::Request::post(path)
            .header(header::HOST, addr.to_string())
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONNECTION, "close")
            .body(Full::new(Bytes::from(
                serde_json::to_vec(body).map_err(|e| e.to_string())?,
            )))
            .map_err(|e| e.to_string())?;
        let res = sender
            .send_request(request)
            .await
            .map_err(|e| e.to_string())?;
        let status = res.status();
        let bytes = http_body_util::Limited::new(res.into_body(), MAX_REPLY_BYTES)
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes();
        Ok((status, bytes))
    };
    tokio::time::timeout(REQUEST_TIMEOUT, exchange)
        .await
        .map_err(|_| "timed out".to_string())?
}

/// A node's answer as the reply, or its refusal as the admin should see it.
fn reply<T: serde::de::DeserializeOwned>(status: StatusCode, bytes: &[u8]) -> ApiResult<T> {
    if status.is_success() {
        return serde_json::from_slice(bytes).map_err(|_| garbled());
    }
    match serde_json::from_slice::<ErrorBody>(bytes) {
        Ok(refusal) if refusal.error == claim::code::WRONG_CODE => Err(wrong_code()),
        Ok(refusal) => Err(ApiError::conflict("node_refused", refusal.message)),
        Err(_) => Err(unreachable_node(format!("it answered {status}"))),
    }
}
