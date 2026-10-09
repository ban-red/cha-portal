//! The media relay (ADR 0022): the agent against a fake portal and a fake
//! streamer, both plain WebSocket servers on loopback.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use cha_node::environments::{Connect, Exit, Progress, Runtime};
use cha_node::{Agent, Identity};
use cha_wire::{
    EnvironmentSpec, NodeKey, NodeRequest, NodeResponse, StreamerEndpoint, ToNode, ToPortal,
};
use futures_util::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::{WebSocketStream, accept_hdr_async};

/// Runs nothing; knows one environment whose streamer is at `port`.
struct Streamers {
    port: u16,
    exits: broadcast::Sender<Exit>,
    progress: broadcast::Sender<Progress>,
}

impl Runtime for Streamers {
    fn start(&self, _: EnvironmentSpec) -> BoxFuture<'_, Result<StreamerEndpoint>> {
        Box::pin(async { anyhow::bail!("not used") })
    }
    fn stop(&self, _: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn running(&self) -> BoxFuture<'_, Result<Vec<String>>> {
        Box::pin(async { Ok(vec!["env1".into()]) })
    }
    fn connect(&self, _: Connect) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async { anyhow::bail!("not used") })
    }
    fn streamer_http_port(&self, id: &str) -> Option<u16> {
        (id == "env1").then_some(self.port)
    }
    fn exits(&self) -> broadcast::Receiver<Exit> {
        self.exits.subscribe()
    }
    fn progress(&self) -> broadcast::Receiver<Progress> {
        self.progress.subscribe()
    }
}

/// A streamer that echoes every message back, prefixed with the request URI
/// for the first one. Refuses with 403 when the token is `bad`.
async fn fake_streamer(uris: Arc<Mutex<Vec<String>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let uris = uris.clone();
            tokio::spawn(async move {
                let check = |req: &Request, res: Response| -> Result<Response, ErrorResponse> {
                    uris.lock().unwrap().push(req.uri().to_string());
                    if req.uri().query().is_some_and(|q| q.contains("token=bad")) {
                        let mut refused = ErrorResponse::new(Some("session full".into()));
                        *refused.status_mut() = 403.try_into().unwrap();
                        return Err(refused);
                    }
                    Ok(res)
                };
                let Ok(mut ws) = accept_hdr_async(tcp, check).await else {
                    return;
                };
                while let Some(Ok(msg)) = ws.next().await {
                    if matches!(msg, Message::Text(_) | Message::Binary(_)) {
                        let _ = ws.send(msg).await;
                    }
                }
            });
        }
    });
    port
}

/// What the fake portal saw of a relay dial-back.
struct Dialled {
    path: String,
    node: String,
    signature: String,
    ws: WebSocketStream<TcpStream>,
}

async fn recv_text(ws: &mut WebSocketStream<TcpStream>) -> String {
    loop {
        match ws.next().await.unwrap().unwrap() {
            Message::Text(t) => return t.to_string(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("expected text, got {other:?}"),
        }
    }
}

/// Serves the node channel on `listener`'s first connection (handshake, then
/// the agent's inventory, which goes to `inventory`) and returns its socket.
async fn accept_node(
    listener: &TcpListener,
    key_public: &str,
    inventory: &mut Option<cha_wire::Inventory>,
) -> WebSocketStream<TcpStream> {
    let (tcp, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
    let challenge = ToNode::Challenge {
        nonce: "n0nce".into(),
        protocol: cha_wire::PROTOCOL_VERSION,
    };
    ws.send(Message::text(serde_json::to_string(&challenge).unwrap()))
        .await
        .unwrap();
    let ToPortal::Hello {
        node_id, signature, ..
    } = serde_json::from_str(&recv_text(&mut ws).await).unwrap()
    else {
        panic!("expected a hello");
    };
    cha_wire::verify_b64(
        key_public,
        &cha_wire::hello_message("n0nce", &node_id),
        &signature,
    )
    .unwrap();
    let welcome = json!({ "type": "welcome", "node_id": node_id, "heartbeat_secs": 30 });
    ws.send(Message::text(welcome.to_string())).await.unwrap();
    // The agent's first message is its inventory.
    if let ToPortal::Inventory { inventory: inv } =
        serde_json::from_str(&recv_text(&mut ws).await).unwrap()
    {
        *inventory = Some(inv);
    }
    ws
}

async fn request(ws: &mut WebSocketStream<TcpStream>, id: u64, request: NodeRequest) {
    let msg = ToNode::Request { id, request };
    ws.send(Message::text(serde_json::to_string(&msg).unwrap()))
        .await
        .unwrap();
}

/// The next `Response` on the node channel (skipping what else the agent says).
async fn response(ws: &mut WebSocketStream<TcpStream>) -> (u64, Result<NodeResponse, String>) {
    loop {
        if let Ok(ToPortal::Response { id, result }) = serde_json::from_str(&recv_text(ws).await) {
            return (id, result);
        }
    }
}

fn open_relay(relay_id: &str, environment_id: &str, token: &str) -> NodeRequest {
    NodeRequest::OpenRelay {
        relay_id: relay_id.into(),
        environment_id: environment_id.into(),
        codec: "h264".into(),
        media_token: token.into(),
    }
}

/// Accepts one relay dial-back on `listener`.
async fn accept_relay(listener: &TcpListener) -> Dialled {
    let (tcp, _) = listener.accept().await.unwrap();
    let seen = Arc::new(Mutex::new((String::new(), String::new(), String::new())));
    let into = seen.clone();
    let ws = accept_hdr_async(tcp, move |req: &Request, res: Response| {
        let header = |name: &str| {
            req.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string()
        };
        *into.lock().unwrap() = (
            req.uri().path().to_string(),
            header(cha_wire::RELAY_NODE_HEADER),
            header(cha_wire::RELAY_SIGNATURE_HEADER),
        );
        Ok(res)
    })
    .await
    .unwrap();
    let (path, node, signature) = seen.lock().unwrap().clone();
    Dialled {
        path,
        node,
        signature,
        ws,
    }
}

#[tokio::test]
async fn the_agent_bridges_a_streamer_to_the_portal_and_says_it_relays() {
    cha_node::init_tls();
    let uris = Arc::new(Mutex::new(Vec::new()));
    let streamer_port = fake_streamer(uris.clone()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let portal_url = format!("http://{}", listener.local_addr().unwrap());

    let key = NodeKey::from_secret([7; 32]);
    let identity = Identity::new(portal_url, "node-1".into(), "box".into(), &key);
    let runtime = Arc::new(Streamers {
        port: streamer_port,
        exits: broadcast::channel(4).0,
        progress: broadcast::channel(4).0,
    });
    let agent = Agent::new(identity).unwrap().with_runtime(runtime);
    tokio::spawn(async move { agent.run().await });

    let mut inventory = None;
    let mut node = accept_node(&listener, &key.public_b64(), &mut inventory).await;
    assert!(inventory.expect("an inventory").relay);

    // An environment the node doesn't have.
    request(&mut node, 1, open_relay("r0", "nope", "t")).await;
    let (id, result) = response(&mut node).await;
    assert_eq!(id, 1);
    assert_eq!(result.unwrap_err(), "unknown environment");

    // The streamer refuses before the upgrade: its status and body come back.
    request(&mut node, 2, open_relay("r0", "env1", "bad")).await;
    let (_, result) = response(&mut node).await;
    let err = result.unwrap_err();
    assert!(err.contains("403") && err.contains("session full"), "{err}");

    // Two relays at once, each bridged on its own.
    let token = "a b/c+d&e=f";
    request(&mut node, 3, open_relay("r1", "env1", token)).await;
    // The node answers once the portal has taken the relay.
    let (dialled, (id, result)) = tokio::join!(accept_relay(&listener), response(&mut node));
    assert_eq!(id, 3);
    assert!(matches!(result, Ok(NodeResponse::RelayOpened)));
    let mut one = dialled;
    request(&mut node, 4, open_relay("r2", "env1", "other")).await;
    // The node answers once the portal has taken the relay.
    let (dialled, (id, result)) = tokio::join!(accept_relay(&listener), response(&mut node));
    assert_eq!(id, 4);
    assert!(matches!(result, Ok(NodeResponse::RelayOpened)));
    let mut two = dialled;

    // What the streamer was asked, with the token encoded.
    let uri = uris
        .lock()
        .unwrap()
        .iter()
        .find(|u| u.contains("a%20b"))
        .cloned();
    assert_eq!(
        uri.as_deref(),
        Some("/ws/media?codec=h264&token=a%20b%2Fc%2Bd%26e%3Df")
    );

    for (relay, id) in [(&one, "r1"), (&two, "r2")] {
        assert_eq!(relay.path, format!("{}/{id}", cha_wire::RELAY_PATH));
        assert_eq!(relay.node, "node-1");
        cha_wire::verify_b64(
            &key.public_b64(),
            &cha_wire::relay_message(id, "node-1"),
            &relay.signature,
        )
        .unwrap();
    }

    // Messages pass through (the fake streamer echoes), text as text and
    // binary as binary, independently per relay.
    one.ws
        .send(Message::text(r#"{"t":"keyframe"}"#))
        .await
        .unwrap();
    assert_eq!(recv_text(&mut one.ws).await, r#"{"t":"keyframe"}"#);
    two.ws.send(Message::binary(vec![1u8, 2, 3])).await.unwrap();
    assert_eq!(
        two.ws.next().await.unwrap().unwrap(),
        Message::binary(vec![1u8, 2, 3])
    );

    // The portal hangs up on one; the other keeps going.
    one.ws.close(None).await.unwrap();
    two.ws.send(Message::text("still here")).await.unwrap();
    assert_eq!(recv_text(&mut two.ws).await, "still here");
    // And the closed relay's streamer side is closed too: the node hangs up.
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match one.ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "the closed relay never ended");
}
