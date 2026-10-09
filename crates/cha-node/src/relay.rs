//! The media relay (ADR 0022): a browser that can't reach the node directly
//! streams over a WebSocket the portal carries. On `OpenRelay` the agent opens
//! the streamer's `/ws/media` on loopback, dials the portal back on
//! `/api/node/relay/<id>` and passes messages between the two untouched.

use std::time::Duration;

use cha_wire::{RELAY_NODE_HEADER, RELAY_SIGNATURE_HEADER, relay_message};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error, Message};
use tracing::{debug, info};

use crate::docker::encode;
use crate::{Identity, ws_base};

/// Each connection attempt, the streamer's and the portal's.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a hang-up may take to send.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Opens both ends of relay `relay_id` and starts passing messages. Returns
/// once both are open; the passing goes on in a task until either ends.
pub async fn open(
    identity: &Identity,
    http_port: u16,
    relay_id: &str,
    codec: &str,
    media_token: &str,
) -> Result<(), String> {
    let streamer_url = format!(
        "ws://127.0.0.1:{http_port}/ws/media?codec={}&token={}",
        encode(codec),
        encode(media_token)
    );
    let streamer = match tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(&streamer_url),
    )
    .await
    {
        Err(_) => return Err("the streamer didn't answer in 10 s".into()),
        Ok(Ok((ws, _))) => ws,
        // Refused before the upgrade: pass its status and what it said on.
        Ok(Err(Error::Http(res))) => {
            let body = res.body().as_deref().map(String::from_utf8_lossy);
            return Err(format!(
                "the streamer refused the stream ({}): {}",
                res.status(),
                body.as_deref().unwrap_or("no details").trim()
            ));
        }
        Ok(Err(err)) => return Err(format!("reaching the streamer: {err}")),
    };

    let node_id = &identity.node_id;
    let key = identity.key().map_err(|e| format!("{e:#}"))?;
    let signature = key.sign_b64(&relay_message(relay_id, node_id));
    let portal_url = format!(
        "{}{}/{relay_id}",
        ws_base(&identity.portal_url),
        cha_wire::RELAY_PATH
    );
    let mut request = portal_url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("the portal URL {portal_url}: {e}"))?;
    for (name, value) in [
        (RELAY_NODE_HEADER, node_id.as_str()),
        (RELAY_SIGNATURE_HEADER, signature.as_str()),
    ] {
        let value = HeaderValue::from_str(value).map_err(|e| format!("{name} header: {e}"))?;
        request.headers_mut().insert(name, value);
    }
    let portal = match tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(request),
    )
    .await
    {
        Err(_) => return Err("the portal didn't take the relay in 10 s".into()),
        Ok(Ok((ws, _))) => ws,
        Ok(Err(Error::Http(res))) => {
            return Err(format!("the portal refused the relay ({})", res.status()));
        }
        Ok(Err(err)) => return Err(format!("dialling the portal back: {err}")),
    };

    let relay_id = relay_id.to_string();
    tokio::spawn(async move {
        info!(%relay_id, "relay open");
        let (mut streamer_tx, mut streamer_rx) = streamer.split();
        let (mut portal_tx, mut portal_rx) = portal.split();
        tokio::select! {
            () = forward(&mut streamer_rx, &mut portal_tx) => {}
            () = forward(&mut portal_rx, &mut streamer_tx) => {}
        }
        // One end is gone: hang up on the other.
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, streamer_tx.close()).await;
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, portal_tx.close()).await;
        info!(%relay_id, "relay closed");
    });
    Ok(())
}

/// Passes `from`'s messages to `to` as they are (text as text, binary as
/// binary) until `from` ends, closes or errors, or `to` can't take one.
/// Pings and pongs belong to their own hop: the WebSocket library answers
/// them, and the other direction's writes flush the answer.
async fn forward<R, W>(from: &mut R, to: &mut W)
where
    R: Stream<Item = Result<Message, Error>> + Unpin,
    W: Sink<Message, Error = Error> + Unpin,
{
    while let Some(next) = from.next().await {
        match next {
            Ok(Message::Close(_)) => return,
            Ok(msg @ (Message::Text(_) | Message::Binary(_))) => {
                if to.send(msg).await.is_err() {
                    return;
                }
            }
            Ok(_) => {}
            Err(err) => {
                debug!("relay: {err}");
                return;
            }
        }
    }
}
