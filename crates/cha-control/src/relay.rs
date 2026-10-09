//! Media over a WebSocket, through the portal and the node (ADR 0022; the
//! contract is `docs/plans/wan-sharing.md`).
//!
//! A guest who can't reach the node asks to connect with the `websocket`
//! transport and is handed a one-use ticket, `/api/media/<ticket>`. Opening it
//! makes the portal ask the node to open a relay: the node connects to its
//! streamer's loopback `/ws/media` and dials the portal back on
//! `/api/node/relay/<relay id>`, signed with its key, so nothing new listens
//! on the node. The portal then passes messages between the two sockets
//! untouched: text as text, binary as binary. Nothing is logged per message.

use std::collections::HashMap;
use std::pin::pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use cha_wire::{Inventory, NodeRequest, NodeResponse};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::AppState;
use crate::auth::random_token;
use crate::db;
use crate::error::{ApiError, ApiResult};

/// How long a guest has to open its ticket.
const TICKET_TTL: Duration = Duration::from_secs(60);
/// How long the node has to open a relay, and to dial back once it has.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
const DIAL_BACK_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the second half of a relay gets to close after the first.
const CLOSE_GRACE: Duration = Duration::from_secs(5);
/// A text line or a datagram is far smaller; clipboards are the largest.
const MAX_MESSAGE_BYTES: usize = 8 << 20;
/// WebSocket close code: the server failed to do what was asked.
const CLOSE_INTERNAL: u16 = 1011;

/// What a ticket opens: one stream of one environment.
pub struct Ticket {
    pub environment_id: String,
    pub node_id: String,
    pub codec: String,
    pub media_token: String,
    /// Whose it is (`share:<id>` or a user id), for the log.
    pub sub: String,
}

struct Pending {
    node_id: String,
    expires: Instant,
    socket: oneshot::Sender<WebSocket>,
}

/// Tickets guests hold, and relays waiting for their node. In memory only: a
/// restart drops both, and both last seconds.
#[derive(Default)]
pub struct Relays {
    tickets: Mutex<HashMap<String, (Ticket, Instant)>>,
    pending: Mutex<HashMap<String, Pending>>,
    live: AtomicUsize,
}

impl Relays {
    /// Stores `ticket` and returns the one-use secret that opens it.
    pub fn issue(&self, ticket: Ticket) -> anyhow::Result<String> {
        let id = random_token()?;
        let now = Instant::now();
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.retain(|_, (_, expires)| *expires > now);
        tickets.insert(id.clone(), (ticket, now + TICKET_TTL));
        Ok(id)
    }

    /// Takes the ticket, once; `None` if it is unknown, used or expired.
    fn take(&self, id: &str) -> Option<Ticket> {
        let (ticket, expires) = self
            .tickets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)?;
        (expires > Instant::now()).then_some(ticket)
    }

    /// Registers a relay `node_id` is about to dial back for.
    fn expect(&self, relay_id: &str, node_id: &str) -> oneshot::Receiver<WebSocket> {
        let (socket, rx) = oneshot::channel();
        let now = Instant::now();
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|_, p| p.expires > now);
        pending.insert(
            relay_id.to_string(),
            Pending {
                node_id: node_id.to_string(),
                expires: now + OPEN_TIMEOUT,
                socket,
            },
        );
        rx
    }

    fn expects(&self, relay_id: &str, node_id: &str) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(relay_id)
            .is_some_and(|p| p.node_id == node_id && p.expires > Instant::now())
    }

    /// The relay's way to hand over the node's socket, once.
    fn claim(&self, relay_id: &str, node_id: &str) -> Option<oneshot::Sender<WebSocket>> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let p = pending.remove(relay_id)?;
        (p.node_id == node_id && p.expires > Instant::now()).then_some(p.socket)
    }

    fn forget(&self, relay_id: &str) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(relay_id);
    }

    /// Relays passing messages right now.
    pub fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }
}

/// Counts a relay as live while it exists.
struct Live<'a>(&'a Relays);

impl<'a> Live<'a> {
    fn new(relays: &'a Relays) -> (Self, usize) {
        let now = relays.live.fetch_add(1, Ordering::Relaxed) + 1;
        (Self(relays), now)
    }
}

impl Drop for Live<'_> {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The main listener's relay routes; the guest listener takes `media` only.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/media/{ticket}", get(media))
        .route("/node/relay/{relay_id}", get(node_relay))
}

/// The guest's side (`GET /api/media/{ticket}`, on both listeners): the
/// ticket is the credential. An unknown, used or expired one is a 404 before
/// the upgrade; a request that isn't a WebSocket upgrade doesn't spend it.
pub async fn media(
    State(state): State<AppState>,
    Path(ticket): Path<String>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let ticket = state.relays.take(&ticket).ok_or_else(|| {
        ApiError::NotFoundCode(
            "unknown_ticket",
            "this media ticket was used, expired or never existed".into(),
        )
    })?;
    Ok(ws
        .max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| guest_session(state, socket, ticket)))
}

async fn guest_session(state: AppState, mut guest: WebSocket, ticket: Ticket) {
    let (_live, live) = Live::new(&state.relays);
    info!(env = %ticket.environment_id, sub = %ticket.sub, live, "relay opening");
    match open(&state, &ticket).await {
        Ok(node) => {
            splice(guest, node).await;
            info!(env = %ticket.environment_id, live = state.relays.live() - 1, "relay closed");
        }
        Err(reason) => close(&mut guest, reason).await,
    }
}

/// Has the node open a relay for `ticket` and returns the node's socket; the
/// error is the short reason the guest's socket is closed with.
async fn open(state: &AppState, ticket: &Ticket) -> Result<WebSocket, &'static str> {
    if !node_supports_relay(state, &ticket.node_id).await {
        return Err("node_outdated");
    }
    let relay_id = new_relay_id().map_err(|err| {
        warn!("relay id: {err}");
        "internal"
    })?;
    let dialled = state.relays.expect(&relay_id, &ticket.node_id);
    let reply = state
        .nodes
        .request_timeout(
            &ticket.node_id,
            NodeRequest::OpenRelay {
                relay_id: relay_id.clone(),
                environment_id: ticket.environment_id.clone(),
                codec: ticket.codec.clone(),
                media_token: ticket.media_token.clone(),
            },
            OPEN_TIMEOUT,
        )
        .await;
    match reply {
        Ok(NodeResponse::RelayOpened) => {}
        Ok(other) => {
            state.relays.forget(&relay_id);
            warn!(node = %ticket.node_id, "relay: the node answered {other:?}");
            return Err("node_error");
        }
        Err(err) => {
            state.relays.forget(&relay_id);
            warn!(node = %ticket.node_id, "relay: {err}");
            return Err(match err {
                ApiError::Conflict(code @ ("node_offline" | "node_timeout"), _) => code,
                _ => "node_error",
            });
        }
    }
    match tokio::time::timeout(DIAL_BACK_TIMEOUT, dialled).await {
        Ok(Ok(socket)) => Ok(socket),
        _ => {
            state.relays.forget(&relay_id);
            warn!(node = %ticket.node_id, "relay: the node never dialled back");
            Err("node_error")
        }
    }
}

fn new_relay_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The node's side (`GET /api/node/relay/{relay_id}`, main listener only): the
/// node a pending relay was asked of, proving itself with a signature of
/// [`cha_wire::relay_message`] by its enrolled key. Anything else is a 403;
/// a relay id is good once, for 15 s.
pub async fn node_relay(
    State(state): State<AppState>,
    Path(relay_id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let refused =
        || ApiError::forbidden("bad_relay", "this isn't a relay the node was asked to open");
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let node_id = header(cha_wire::RELAY_NODE_HEADER).ok_or_else(refused)?;
    let signature = header(cha_wire::RELAY_SIGNATURE_HEADER).ok_or_else(refused)?;
    if !state.relays.expects(&relay_id, node_id) {
        return Err(refused());
    }
    let node = db::node_by_id(&state.db, node_id)
        .await?
        .ok_or_else(refused)?;
    let message = cha_wire::relay_message(&relay_id, node_id);
    cha_wire::verify_b64(&node.public_key, &message, signature).map_err(|_| refused())?;
    let handover = state.relays.claim(&relay_id, node_id).ok_or_else(refused)?;
    Ok(ws
        .max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            // If the guest gave up meanwhile, the socket is dropped here.
            let _ = handover.send(socket);
        }))
}

async fn close(socket: &mut WebSocket, reason: &'static str) {
    let frame = CloseFrame {
        code: CLOSE_INTERNAL,
        reason: reason.into(),
    };
    let _ = socket.send(Message::Close(Some(frame))).await;
}

/// Passes every message from each socket to the other until either ends, then
/// lets the other half finish its closing handshake for a few seconds.
async fn splice(a: WebSocket, b: WebSocket) {
    let (a_tx, a_rx) = a.split();
    let (b_tx, b_rx) = b.split();
    let mut up = pin!(forward(a_rx, b_tx));
    let mut down = pin!(forward(b_rx, a_tx));
    tokio::select! {
        _ = &mut up => { let _ = tokio::time::timeout(CLOSE_GRACE, down).await; }
        _ = &mut down => { let _ = tokio::time::timeout(CLOSE_GRACE, up).await; }
    }
}

async fn forward(mut from: SplitStream<WebSocket>, mut to: SplitSink<WebSocket, Message>) {
    while let Some(Ok(message)) = from.next().await {
        match message {
            Message::Close(frame) => {
                let _ = to.send(Message::Close(frame)).await;
                break;
            }
            // Each hop answers its own pings.
            Message::Ping(_) | Message::Pong(_) => {}
            message => {
                if to.send(message).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = to.close().await;
}

/// Whether the node's last inventory says its agent handles `OpenRelay`. An
/// older agent drops the connection on a request it can't read.
pub async fn node_supports_relay(state: &AppState, node_id: &str) -> bool {
    let Ok(Some(node)) = db::node_by_id(&state.db, node_id).await else {
        return false;
    };
    node.inventory
        .and_then(|json| serde_json::from_str::<Inventory>(&json).ok())
        .is_some_and(|inventory| inventory.relay)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket() -> Ticket {
        Ticket {
            environment_id: "e".into(),
            node_id: "n".into(),
            codec: "h264".into(),
            media_token: "t".into(),
            sub: "share:s".into(),
        }
    }

    #[test]
    fn a_ticket_opens_once() {
        let relays = Relays::default();
        let id = relays.issue(ticket()).unwrap();
        assert_eq!(id.len(), 43);
        assert!(relays.take(&id).is_some());
        assert!(relays.take(&id).is_none());
        assert!(relays.take("never-issued").is_none());
    }

    #[test]
    fn an_expired_ticket_is_gone() {
        let relays = Relays::default();
        let id = relays.issue(ticket()).unwrap();
        relays.tickets.lock().unwrap().get_mut(&id).unwrap().1 =
            Instant::now() - Duration::from_secs(1);
        assert!(relays.take(&id).is_none());
    }

    #[test]
    fn a_relay_is_for_one_node_and_good_once() {
        let relays = Relays::default();
        let _rx = relays.expect("r1", "node-a");
        assert!(relays.expects("r1", "node-a"));
        assert!(!relays.expects("r1", "node-b"));
        assert!(relays.claim("r1", "node-b").is_none(), "wrong node");
        assert!(relays.claim("r1", "node-a").is_none(), "and that spent it");
        let _rx = relays.expect("r2", "node-a");
        assert!(relays.claim("r2", "node-a").is_some());
        assert!(relays.claim("r2", "node-a").is_none());
    }

    #[test]
    fn relay_ids_are_128_bits_of_hex() {
        let id = new_relay_id().unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
