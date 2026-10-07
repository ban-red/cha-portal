//! Pairing Moonlight devices through the portal (ADR 0009), against a
//! hand-driven node connection: the node's GameStream host asks for a PIN, a
//! user types it, the node reports the device, and the portal keeps it.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use cha_control::{AppState, Config, app, db};
use cha_wire::{
    Device, DeviceKind, EnvironmentSpec, GameStreamDevice, GameStreamInfo, GamepadKind, Inventory,
    NodeKey, NodeRequest, NodeResponse, PortalRequest, PortalResponse, StreamerEndpoint, ToNode,
    ToPortal,
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Portal {
    state: AppState,
    router: Router,
    addr: SocketAddr,
    admin: String,
    _dir: tempfile::TempDir,
}

#[derive(Debug)]
struct Reply {
    status: StatusCode,
    body: Value,
}

async fn call(
    router: &Router,
    method: &str,
    path: &str,
    cookie: &str,
    body: Option<Value>,
) -> Reply {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, cookie);
    let req = match body {
        Some(body) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            req.body(Body::empty())
        }
    }
    .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

impl Portal {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            database: dir.path().join("cha.db"),
            web_dir: None,
            secure_cookies: false,
            session_days: 1,
            ice: Default::default(),
            dev_login: false,
            discover_nodes: false,
            public_url: None,
        };
        let pool = db::open(&config.database).await.unwrap();
        let state = AppState::new(config, pool).await.unwrap();
        let router = app(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = router.clone();
        tokio::spawn(async move {
            axum::serve(
                listener,
                served.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let setup = Request::builder()
            .method("POST")
            .uri("/api/setup")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "username": "admin", "password": "correct horse battery" }).to_string(),
            ))
            .unwrap();
        let res = router.clone().oneshot(setup).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let admin = res.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        Self {
            state,
            router,
            addr,
            admin,
            _dir: dir,
        }
    }

    async fn call(&self, method: &str, path: &str, cookie: &str, body: Option<Value>) -> Reply {
        call(&self.router, method, path, cookie, body).await
    }

    /// An account with `role`, signed in: its cookie and id.
    async fn account(&self, username: &str, role: &str) -> (String, String) {
        let created = self
            .call(
                "POST",
                "/api/users",
                &self.admin,
                Some(json!({ "username": username, "password": "another long password", "role": role })),
            )
            .await;
        assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);
        let req = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "username": username, "password": "another long password" }).to_string(),
            ))
            .unwrap();
        let res = self.router.clone().oneshot(req).await.unwrap();
        let cookie = res.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        (cookie, created.body["id"].as_str().unwrap().to_string())
    }

    async fn audit_actions(&self) -> Vec<String> {
        let rows = self.call("GET", "/api/audit", &self.admin, None).await;
        rows.body
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| e["action"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A node, connected over a real WebSocket and driven by the test.
struct FakeNode {
    id: String,
    ws: Socket,
    welcome: ToNode,
}

fn inventory(gamestream: bool) -> Inventory {
    Inventory {
        hostname: "box".into(),
        addresses: vec!["192.168.1.20".into(), "100.64.0.2".into()],
        // One NVIDIA GPU and a data root: what a launch needs of a node.
        data_root: Some("/data".into()),
        devices: Some(vec![Device {
            id: "nvidia:0".into(),
            kind: DeviceKind::Nvidia,
            name: "NVIDIA GeForce RTX 4090".into(),
            render_node: None,
            vendor: Some("nvidia".into()),
            codecs: vec!["h264".into(), "hevc".into()],
            cores: None,
        }]),
        gamestream: gamestream.then(|| GameStreamInfo {
            http_port: 47989,
            name: "box".into(),
        }),
        ..Inventory::default()
    }
}

impl FakeNode {
    /// Enrolls a node by name and connects it. It says what its inventory
    /// holds, as an agent does right after the welcome.
    async fn connect(p: &Portal, name: &str, gamestream: bool) -> Self {
        let key = NodeKey::from_secret(rand_secret());
        let id = db::new_id();
        sqlx::query("INSERT INTO nodes (id, name, public_key, enrolled_at) VALUES (?, ?, ?, 0)")
            .bind(&id)
            .bind(name)
            .bind(key.public_b64())
            .execute(&p.state.db)
            .await
            .unwrap();
        Self::reconnect(p, id, &key, gamestream).await
    }

    async fn reconnect(p: &Portal, id: String, key: &NodeKey, gamestream: bool) -> Self {
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://{}{}", p.addr, cha_wire::CONNECT_PATH))
                .await
                .unwrap();
        let ToNode::Challenge { nonce, protocol } = next_text(&mut ws).await else {
            panic!("expected a challenge");
        };
        let hello = ToPortal::Hello {
            node_id: id.clone(),
            signature: key.sign_b64(&cha_wire::hello_message(&nonce, &id)),
            agent_version: "test".into(),
            protocol,
        };
        send(&mut ws, &hello).await;
        let welcome = next_text(&mut ws).await;
        send(
            &mut ws,
            &ToPortal::Inventory {
                inventory: inventory(gamestream),
            },
        )
        .await;
        Self { id, ws, welcome }
    }

    async fn send(&mut self, msg: ToPortal) {
        send(&mut self.ws, &msg).await;
    }

    /// The next request the portal makes of the node.
    async fn request(&mut self) -> (u64, NodeRequest) {
        loop {
            if let ToNode::Request { id, request } = next_text(&mut self.ws).await {
                return (id, request);
            }
        }
    }

    async fn accept(&mut self, id: u64) {
        self.send(ToPortal::Response {
            id,
            result: Ok(NodeResponse::Accepted),
        })
        .await;
    }

    /// The device list the portal sends next, which the node accepts.
    async fn devices(&mut self) -> Vec<GameStreamDevice> {
        let (id, request) = self.request().await;
        let NodeRequest::GameStreamDevices { devices } = request else {
            panic!("expected the device list, got {request:?}");
        };
        self.accept(id).await;
        devices
    }

    /// A request of the node's own, as its GameStream host makes them.
    async fn ask(&mut self, id: u64, request: PortalRequest) {
        self.send(ToPortal::Request { id, request }).await;
    }

    /// The portal's next answer to such a request.
    async fn answer(&mut self) -> (u64, Result<PortalResponse, String>) {
        loop {
            if let ToNode::Response { id, result } = next_text(&mut self.ws).await {
                return (id, result);
            }
        }
    }

    /// Takes the start the portal asks for and says the environment is up.
    async fn start(&mut self) -> EnvironmentSpec {
        let (id, request) = self.request().await;
        let NodeRequest::StartEnvironment { environment } = request else {
            panic!("expected a start, got {request:?}");
        };
        self.send(ToPortal::Response {
            id,
            result: Ok(NodeResponse::EnvironmentStarted {
                id: environment.id.clone(),
                streamer: StreamerEndpoint {
                    http_port: 7600,
                    webrtc_port: 7601,
                    webtransport_port: 0,
                },
            }),
        })
        .await;
        environment
    }

    /// Nothing more is asked of the node for a moment.
    async fn is_left_alone(&mut self) -> bool {
        tokio::time::timeout(Duration::from_millis(300), self.request())
            .await
            .is_err()
    }

    async fn pair_request(&mut self, attempt: &str, expires_in_secs: u64) {
        self.send(ToPortal::GameStreamPairRequest {
            attempt_id: attempt.into(),
            device_name: "Steam Deck".into(),
            address: "192.168.1.9".into(),
            expires_in_secs,
        })
        .await;
    }
}

fn rand_secret() -> [u8; 32] {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    secret
}

async fn next_text<T: serde::de::DeserializeOwned>(ws: &mut Socket) -> T {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("the portal said nothing for five seconds")
            .expect("the connection ended")
            .expect("a websocket error");
        if let Message::Text(text) = msg {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

async fn send(ws: &mut Socket, msg: &ToPortal) {
    ws.send(Message::text(serde_json::to_string(msg).unwrap()))
        .await
        .unwrap();
}

fn fingerprint(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

/// Waits until `check` holds of `path`, as `cookie`.
async fn wait_for(p: &Portal, path: &str, cookie: &str, check: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..100 {
        let reply = p.call("GET", path, cookie, None).await;
        if check(&reply.body) {
            return reply.body;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    panic!("{path} never reached the expected state");
}

/// What the node does once the PIN is typed: takes it and reports the device.
async fn node_pairs(node: &mut FakeNode, attempt: &str, fingerprint: String) -> String {
    let (id, request) = node.request().await;
    let NodeRequest::GameStreamPin {
        attempt_id,
        pin,
        user_id,
    } = request
    else {
        panic!("expected a PIN, got {request:?}");
    };
    assert_eq!(attempt_id, attempt);
    assert_eq!(pin, "1234");
    node.accept(id).await;
    node.send(ToPortal::GameStreamPaired {
        attempt_id: attempt.into(),
        fingerprint,
        unique_id: "0123456789ABCDEF".into(),
        name: "Steam Deck".into(),
    })
    .await;
    user_id
}

#[tokio::test]
async fn a_pairing_request_is_answered_with_a_pin_and_the_device_is_kept_and_pushed() {
    let p = Portal::start().await;
    let (bob, bob_id) = p.account("bob", "user").await;
    let (alice, _) = p.account("alice", "user").await;
    let mut node = FakeNode::connect(&p, "gpu-box", true).await;
    assert!(matches!(
        node.welcome,
        ToNode::Welcome {
            gamestream: true,
            ..
        }
    ));
    // The node has none yet, and says nothing is paired until told.
    assert!(node.devices().await.is_empty());

    node.pair_request("a1", 120).await;
    let listed = wait_for(&p, "/api/gamestream/pairing", &bob, |b| {
        !b["requests"].as_array().unwrap().is_empty()
    })
    .await;
    let request = &listed["requests"][0];
    assert_eq!(request["nodeId"], node.id);
    assert_eq!(request["nodeName"], "gpu-box");
    assert_eq!(request["deviceName"], "Steam Deck");
    assert_eq!(request["address"], "192.168.1.9");
    let expires = request["expiresAt"].as_i64().unwrap();
    let now = db::now();
    assert!(
        (now + 100..=now + 121).contains(&expires),
        "unix seconds, two minutes out: {expires} vs {now}"
    );
    // Everyone signed in sees it.
    let seen = p.call("GET", "/api/gamestream/pairing", &alice, None).await;
    assert_eq!(seen.body["requests"].as_array().unwrap().len(), 1);
    let id = request["id"].as_str().unwrap().to_string();

    // Bob types the PIN; the node pairs; the call returns the device.
    let typed = {
        let router = p.router.clone();
        let (cookie, id) = (bob.clone(), id.clone());
        tokio::spawn(async move {
            call(
                &router,
                "POST",
                &format!("/api/gamestream/pairing/{id}"),
                &cookie,
                Some(json!({ "pin": "1234" })),
            )
            .await
        })
    };
    let owner = node_pairs(&mut node, "a1", fingerprint(0xab)).await;
    assert_eq!(owner, bob_id, "the user who typed the PIN owns the device");
    let reply = typed.await.unwrap();
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    let device = &reply.body["device"];
    assert_eq!(device["name"], "Steam Deck");
    assert_eq!(device["nodeId"], node.id);
    assert_eq!(device["nodeName"], "gpu-box");
    assert_eq!(device["owner"]["id"], bob_id);
    assert_eq!(device["owner"]["name"], "bob");
    assert!(device["pairedAt"].as_i64().unwrap() >= now);
    assert!(device["id"].as_str().is_some());

    // The node is sent the whole list, with the owner.
    assert_eq!(
        node.devices().await,
        vec![GameStreamDevice {
            fingerprint: fingerprint(0xab),
            unique_id: Some("0123456789ABCDEF".into()),
            name: "Steam Deck".into(),
            user_id: bob_id.clone(),
        }]
    );
    // The request is done.
    let listed = p.call("GET", "/api/gamestream/pairing", &bob, None).await;
    assert!(listed.body["requests"].as_array().unwrap().is_empty());
    // Own devices for a user, everyone's for an admin.
    let own = p.call("GET", "/api/gamestream/devices", &bob, None).await;
    assert_eq!(own.body["devices"].as_array().unwrap().len(), 1);
    let others = p.call("GET", "/api/gamestream/devices", &alice, None).await;
    assert!(others.body["devices"].as_array().unwrap().is_empty());
    let all = p
        .call("GET", "/api/gamestream/devices", &p.admin, None)
        .await;
    assert_eq!(all.body["devices"][0]["owner"]["name"], "bob");
    assert!(
        p.audit_actions()
            .await
            .contains(&"gamestream.paired".into())
    );
}

#[tokio::test]
async fn a_wrong_pin_is_told_and_keeps_nothing() {
    let p = Portal::start().await;
    let (bob, _) = p.account("bob", "user").await;
    let mut node = FakeNode::connect(&p, "gpu-box", true).await;
    node.devices().await;
    node.pair_request("a1", 120).await;
    let listed = wait_for(&p, "/api/gamestream/pairing", &bob, |b| {
        !b["requests"].as_array().unwrap().is_empty()
    })
    .await;
    let id = listed["requests"][0]["id"].as_str().unwrap().to_string();
    let typed = {
        let router = p.router.clone();
        let cookie = bob.clone();
        tokio::spawn(async move {
            call(
                &router,
                "POST",
                &format!("/api/gamestream/pairing/{id}"),
                &cookie,
                Some(json!({ "pin": "1234" })),
            )
            .await
        })
    };
    let (rid, request) = node.request().await;
    assert!(matches!(request, NodeRequest::GameStreamPin { .. }));
    node.accept(rid).await;
    node.send(ToPortal::GameStreamPairFailed {
        attempt_id: "a1".into(),
        reason: "the device gave up".into(),
    })
    .await;
    let reply = typed.await.unwrap();
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{:?}", reply.body);
    assert_eq!(reply.body["error"], "wrong_pin");
    let devices = p.call("GET", "/api/gamestream/devices", &bob, None).await;
    assert!(devices.body["devices"].as_array().unwrap().is_empty());
    // A new attempt starts from the device, not from this request.
    let again = p.call("GET", "/api/gamestream/pairing", &bob, None).await;
    assert!(again.body["requests"].as_array().unwrap().is_empty());
    wait_for(&p, "/api/audit", &p.admin, |b| {
        b.as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "gamestream.pair_failed")
    })
    .await;
}

#[tokio::test]
async fn bad_pins_unknown_and_expired_requests_are_refused() {
    let p = Portal::start().await;
    let (bob, _) = p.account("bob", "user").await;
    let mut node = FakeNode::connect(&p, "gpu-box", true).await;
    node.devices().await;
    node.pair_request("a1", 120).await;
    node.pair_request("old", 0).await;
    let listed = wait_for(&p, "/api/gamestream/pairing", &bob, |b| {
        !b["requests"].as_array().unwrap().is_empty()
    })
    .await;
    // The one that ran out is not offered.
    assert_eq!(listed["requests"].as_array().unwrap().len(), 1);
    let id = listed["requests"][0]["id"].as_str().unwrap();

    for pin in ["123", "12345", "12a4", "", "    "] {
        let reply = p
            .call(
                "POST",
                &format!("/api/gamestream/pairing/{id}"),
                &bob,
                Some(json!({ "pin": pin })),
            )
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{pin:?}");
        assert_eq!(reply.body["error"], "bad_pin");
    }
    let unknown = p
        .call(
            "POST",
            "/api/gamestream/pairing/nope",
            &bob,
            Some(json!({ "pin": "1234" })),
        )
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.body["error"], "not_found");

    // The request the node no longer knows is gone for good.
    let typed = {
        let router = p.router.clone();
        let (cookie, id) = (bob.clone(), id.to_string());
        tokio::spawn(async move {
            call(
                &router,
                "POST",
                &format!("/api/gamestream/pairing/{id}"),
                &cookie,
                Some(json!({ "pin": "1234" })),
            )
            .await
        })
    };
    let (rid, _) = node.request().await;
    node.send(ToPortal::Response {
        id: rid,
        result: Err("no such pairing".into()),
    })
    .await;
    let reply = typed.await.unwrap();
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{:?}", reply.body);
    let after = p.call("GET", "/api/gamestream/pairing", &bob, None).await;
    assert!(after.body["requests"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_node_that_never_reports_is_a_timeout_and_one_that_leaves_is_unreachable() {
    let p = Portal::start().await;
    p.state
        .gamestream
        .set_outcome_wait(Duration::from_millis(300));
    let (bob, _) = p.account("bob", "user").await;
    let mut node = FakeNode::connect(&p, "gpu-box", true).await;
    node.devices().await;
    node.pair_request("slow", 120).await;
    node.pair_request("gone", 120).await;
    let listed = wait_for(&p, "/api/gamestream/pairing", &bob, |b| {
        b["requests"].as_array().unwrap().len() == 2
    })
    .await;
    let post = |id: String| {
        let router = p.router.clone();
        let cookie = bob.clone();
        tokio::spawn(async move {
            call(
                &router,
                "POST",
                &format!("/api/gamestream/pairing/{id}"),
                &cookie,
                Some(json!({ "pin": "1234" })),
            )
            .await
        })
    };
    let id_of = |attempt_device: usize| {
        listed["requests"][attempt_device]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // It takes the PIN and then says nothing.
    let slow = post(id_of(0));
    let (rid, _) = node.request().await;
    node.accept(rid).await;
    let reply = slow.await.unwrap();
    assert_eq!(
        reply.status,
        StatusCode::GATEWAY_TIMEOUT,
        "{:?}",
        reply.body
    );
    assert_eq!(reply.body["error"], "timeout");

    // It takes a PIN and drops off.
    let gone = post(id_of(1));
    let (rid, _) = node.request().await;
    node.accept(rid).await;
    node.ws.close(None).await.unwrap();
    let reply = gone.await.unwrap();
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY, "{:?}", reply.body);
    assert_eq!(reply.body["error"], "node_unreachable");

    // Its requests went with it, and a PIN for one finds no node.
    let after = wait_for(&p, "/api/gamestream/pairing", &bob, |b| {
        b["requests"].as_array().unwrap().is_empty()
    })
    .await;
    assert!(after["requests"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_request_goes_with_its_nodes_connection() {
    let p = Portal::start().await;
    let (bob, _) = p.account("bob", "user").await;
    let mut node = FakeNode::connect(&p, "gpu-box", true).await;
    node.devices().await;
    node.pair_request("a1", 120).await;
    let listed = wait_for(&p, "/api/gamestream/pairing", &bob, |b| {
        !b["requests"].as_array().unwrap().is_empty()
    })
    .await;
    let id = listed["requests"][0]["id"].as_str().unwrap().to_string();
    // The node drops off before the PIN is typed: its requests go with it.
    drop(node);
    let reply = wait_for_status(&p, &format!("/api/gamestream/pairing/{id}"), &bob).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{:?}", reply.body);
}

async fn wait_for_status(p: &Portal, path: &str, cookie: &str) -> Reply {
    for _ in 0..100 {
        let reply = p
            .call("POST", path, cookie, Some(json!({ "pin": "1234" })))
            .await;
        if reply.status != StatusCode::CONFLICT && reply.status != StatusCode::BAD_GATEWAY {
            return reply;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    panic!("{path} never answered");
}

#[tokio::test]
async fn removing_a_device_tells_the_node_and_only_its_owner_or_an_admin_may() {
    let p = Portal::start().await;
    let (bob, bob_id) = p.account("bob", "user").await;
    let (alice, _) = p.account("alice", "user").await;
    let mut node = FakeNode::connect(&p, "gpu-box", true).await;
    node.devices().await;

    // Two devices of bob's, paired the way a node reports them.
    let mut ids = Vec::new();
    for (attempt, n) in [("a1", 1u8), ("a2", 2)] {
        node.pair_request(attempt, 120).await;
        let listed = wait_for(&p, "/api/gamestream/pairing", &bob, |b| {
            !b["requests"].as_array().unwrap().is_empty()
        })
        .await;
        let id = listed["requests"][0]["id"].as_str().unwrap().to_string();
        let typed = {
            let router = p.router.clone();
            let cookie = bob.clone();
            tokio::spawn(async move {
                call(
                    &router,
                    "POST",
                    &format!("/api/gamestream/pairing/{id}"),
                    &cookie,
                    Some(json!({ "pin": "1234" })),
                )
                .await
            })
        };
        node_pairs(&mut node, attempt, fingerprint(n)).await;
        let reply = typed.await.unwrap();
        ids.push(reply.body["device"]["id"].as_str().unwrap().to_string());
        node.devices().await;
    }

    // Alice may not, an unknown id isn't there, and a guest can't either.
    let refused = p
        .call(
            "DELETE",
            &format!("/api/gamestream/devices/{}", ids[0]),
            &alice,
            None,
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let unknown = p
        .call("DELETE", "/api/gamestream/devices/nope", &bob, None)
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    // Bob's own: gone, and the node is told.
    let removed = p
        .call(
            "DELETE",
            &format!("/api/gamestream/devices/{}", ids[0]),
            &bob,
            None,
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    let list = node.devices().await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].fingerprint, fingerprint(2));
    assert_eq!(list[0].user_id, bob_id);

    // An admin may remove anyone's.
    let removed = p
        .call(
            "DELETE",
            &format!("/api/gamestream/devices/{}", ids[1]),
            &p.admin,
            None,
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    assert!(node.devices().await.is_empty());
    let gone = p
        .call("GET", "/api/gamestream/devices", &p.admin, None)
        .await;
    assert!(gone.body["devices"].as_array().unwrap().is_empty());
    assert!(
        p.audit_actions()
            .await
            .contains(&"gamestream.removed".into())
    );
}

#[tokio::test]
async fn a_device_that_unpairs_itself_is_forgotten() {
    let p = Portal::start().await;
    let (bob, bob_id) = p.account("bob", "user").await;
    let mut node = FakeNode::connect(&p, "gpu-box", true).await;
    node.devices().await;
    let row = db::upsert_gamestream_device(
        &p.state.db,
        &bob_id,
        &node.id,
        &fingerprint(7),
        None,
        "Phone",
    )
    .await
    .unwrap();
    assert_eq!(row.owner_name, "bob");
    node.send(ToPortal::GameStreamUnpaired {
        fingerprint: fingerprint(7),
    })
    .await;
    assert!(node.devices().await.is_empty());
    let own = p.call("GET", "/api/gamestream/devices", &bob, None).await;
    assert!(own.body["devices"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_node_is_sent_its_devices_when_it_connects_and_a_pairing_nobody_typed_is_dropped() {
    let p = Portal::start().await;
    let (_, bob_id) = p.account("bob", "user").await;
    let key = NodeKey::from_secret(rand_secret());
    let id = db::new_id();
    sqlx::query(
        "INSERT INTO nodes (id, name, public_key, enrolled_at) VALUES (?, 'gpu-box', ?, 0)",
    )
    .bind(&id)
    .bind(key.public_b64())
    .execute(&p.state.db)
    .await
    .unwrap();
    db::upsert_gamestream_device(
        &p.state.db,
        &bob_id,
        &id,
        &fingerprint(9),
        Some("U1"),
        "Deck",
    )
    .await
    .unwrap();
    let mut node = FakeNode::reconnect(&p, id.clone(), &key, true).await;
    let devices = node.devices().await;
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].fingerprint, fingerprint(9));
    assert_eq!(devices[0].unique_id.as_deref(), Some("U1"));

    // A pairing reported with no PIN typed in the portal is not kept; the
    // node is sent the list, which is how it forgets the stray.
    node.send(ToPortal::GameStreamPaired {
        attempt_id: "stray".into(),
        fingerprint: fingerprint(1),
        unique_id: "x".into(),
        name: "Stray".into(),
    })
    .await;
    let devices = node.devices().await;
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].fingerprint, fingerprint(9));
}

#[tokio::test]
async fn a_node_without_gamestream_is_sent_nothing_and_everything_is_empty() {
    let p = Portal::start().await;
    let (bob, _) = p.account("bob", "user").await;
    let mut node = FakeNode::connect(&p, "plain-box", false).await;
    // Let the inventory arrive; then the portal has nothing to push.
    wait_for(&p, "/api/nodes", &p.admin, |b| !b[0]["inventory"].is_null()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), node.request())
            .await
            .is_err(),
        "an older node isn't sent messages it doesn't know"
    );
    for path in [
        "/api/gamestream/pairing",
        "/api/gamestream/devices",
        "/api/gamestream/hosts",
    ] {
        let reply = p.call("GET", path, &bob, None).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        let list = reply.body.as_object().unwrap().values().next().unwrap();
        assert!(list.as_array().unwrap().is_empty(), "{path}");
    }
}

#[tokio::test]
async fn hosts_are_the_online_nodes_that_run_one() {
    let p = Portal::start().await;
    let (bob, _) = p.account("bob", "user").await;
    let with = FakeNode::connect(&p, "gpu-box", true).await;
    let _without = FakeNode::connect(&p, "plain-box", false).await;
    let leaving = FakeNode::connect(&p, "away-box", true).await;
    let hosts = wait_for(&p, "/api/gamestream/hosts", &bob, |b| {
        b["hosts"].as_array().unwrap().len() == 2
    })
    .await;
    let mut names: Vec<_> = hosts["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["nodeName"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["away-box", "gpu-box"]);
    let host = hosts["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["nodeId"] == with.id)
        .unwrap();
    assert_eq!(host["address"], "192.168.1.20", "the first address");
    assert_eq!(host["httpPort"], 47989);

    // Offline nodes drop out.
    let mut leaving = leaving;
    leaving.ws.close(None).await.unwrap();
    let hosts = wait_for(&p, "/api/gamestream/hosts", &bob, |b| {
        b["hosts"].as_array().unwrap().len() == 1
    })
    .await;
    assert_eq!(hosts["hosts"][0]["nodeName"], "gpu-box");
}

#[tokio::test]
async fn guests_cannot_pair_or_see_hosts_and_everyone_must_be_signed_in() {
    let p = Portal::start().await;
    let (guest, _) = p.account("visitor", "guest").await;
    let mut node = FakeNode::connect(&p, "gpu-box", true).await;
    node.devices().await;
    node.pair_request("a1", 120).await;
    // The admin sees the request, so it is there for the guest to be refused.
    let listed = wait_for(&p, "/api/gamestream/pairing", &p.admin, |b| {
        !b["requests"].as_array().unwrap().is_empty()
    })
    .await;
    let id = listed["requests"][0]["id"].as_str().unwrap();
    for (method, path, body) in [
        ("GET", "/api/gamestream/pairing".to_string(), None),
        ("GET", "/api/gamestream/hosts".to_string(), None),
        (
            "POST",
            format!("/api/gamestream/pairing/{id}"),
            Some(json!({ "pin": "1234" })),
        ),
    ] {
        let reply = p.call(method, &path, &guest, body).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{method} {path}");
    }
    // A guest has no devices of their own to list.
    let devices = p.call("GET", "/api/gamestream/devices", &guest, None).await;
    assert!(devices.body["devices"].as_array().unwrap().is_empty());
    // And nobody gets in unauthenticated.
    for path in [
        "/api/gamestream/pairing",
        "/api/gamestream/devices",
        "/api/gamestream/hosts",
    ] {
        assert_eq!(
            p.call("GET", path, "", None).await.status,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
}

// ---- Launching and stopping for a paired client ----

impl Portal {
    /// `user` has a device paired with `node`, as a pairing leaves it.
    async fn pair_device(&self, node: &FakeNode, user: &str, n: u8) {
        db::upsert_gamestream_device(
            &self.state.db,
            user,
            &node.id,
            &fingerprint(n),
            None,
            "Deck",
        )
        .await
        .unwrap();
    }

    /// Two nodes with a GPU each that run a host, nothing paired yet.
    async fn two_nodes(&self) -> (FakeNode, FakeNode) {
        let mut a = FakeNode::connect(self, "box-a", true).await;
        let mut b = FakeNode::connect(self, "box-b", true).await;
        a.devices().await;
        b.devices().await;
        (a, b)
    }

    async fn environments(&self) -> Vec<db::EnvironmentRow> {
        db::list_environments(&self.state.db, None, 50)
            .await
            .unwrap()
    }

    /// The `environment.*` audit entries with their detail, oldest first.
    async fn audited(&self, action: &str) -> Vec<Value> {
        let rows = self.call("GET", "/api/audit", &self.admin, None).await;
        let mut found: Vec<Value> = rows
            .body
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["action"] == action)
            .map(|e| serde_json::from_str(e["detail"].as_str().unwrap_or("null")).unwrap())
            .collect();
        found.reverse();
        found
    }
}

fn launch(user: &str, template: &str) -> PortalRequest {
    PortalRequest::GameStreamLaunch {
        user_id: user.into(),
        template_id: template.into(),
    }
}

#[tokio::test]
async fn a_clients_launch_starts_its_owners_environment_on_that_node_with_their_settings() {
    let p = Portal::start().await;
    let (bob, bob_id) = p.account("bob", "user").await;
    let (mut a, mut b) = p.two_nodes().await;
    p.pair_device(&a, &bob_id, 1).await;
    p.pair_device(&b, &bob_id, 2).await;
    // Their own choices for the app, as the browser's launch would use.
    for (path, body) in [
        (
            "/api/controllers/apps/chrome",
            json!({ "kind": "dualsense" }),
        ),
        ("/api/apps/settings/chrome", json!({ "fps": 120 })),
    ] {
        let set = p.call("PUT", path, &bob, Some(body)).await;
        assert_eq!(set.status, StatusCode::OK, "{:?}", set.body);
    }

    a.ask(7, launch(&bob_id, "chrome")).await;
    let spec = a.start().await;
    assert_eq!(spec.owner, bob_id);
    assert_eq!(spec.template, "chrome");
    assert_eq!(spec.gamepad, Some(GamepadKind::Dualsense));
    assert_eq!(spec.fps, 120);
    assert_eq!(spec.device.as_ref().unwrap().id, "nvidia:0");
    // The answer comes once it runs.
    let (id, result) = a.answer().await;
    assert_eq!(id, 7);
    assert_eq!(
        result,
        Ok(PortalResponse::GameStreamLaunched {
            environment_id: spec.id.clone(),
            created: true
        })
    );
    let rows = p.environments().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].node_id.as_deref(), rows[0].state.as_str()),
        (Some(a.id.as_str()), "running"),
        "on the node that asked, and nowhere else"
    );
    assert!(b.is_left_alone().await);
    // The audit log says how it was launched.
    let launched = p.audited("environment.launched").await;
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0]["via"], "moonlight");
    assert_eq!(launched[0]["template"], "chrome");
    assert_eq!(launched[0]["node"], "box-a");

    // Asked again while it runs, the launch joins it: nothing starts.
    a.ask(8, launch(&bob_id, "chrome")).await;
    let (id, result) = a.answer().await;
    assert_eq!(id, 8);
    assert_eq!(
        result,
        Ok(PortalResponse::GameStreamLaunched {
            environment_id: spec.id.clone(),
            created: false
        })
    );
    assert!(a.is_left_alone().await);
    assert_eq!(p.environments().await.len(), 1);

    // Quitting stops it, through the API's stop, and the log says so.
    a.ask(
        9,
        PortalRequest::GameStreamStop {
            user_id: bob_id.clone(),
            environment_id: spec.id.clone(),
        },
    )
    .await;
    // The portal's answer and its stop of the node can arrive in either order.
    let (mut stop, mut answer) = (None, None);
    while stop.is_none() || answer.is_none() {
        match next_text::<ToNode>(&mut a.ws).await {
            ToNode::Request {
                id,
                request: NodeRequest::StopEnvironment { id: env },
            } => {
                assert_eq!(env, spec.id);
                stop = Some(id);
            }
            ToNode::Response { id, result } => answer = Some((id, result)),
            other => panic!("unexpected {other:?}"),
        }
    }
    a.send(ToPortal::Response {
        id: stop.unwrap(),
        result: Ok(NodeResponse::EnvironmentStopped {
            id: spec.id.clone(),
        }),
    })
    .await;
    assert_eq!(answer, Some((9, Ok(PortalResponse::GameStreamStopped))));
    let stopped = p.audited("environment.stopped").await;
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0]["via"], "moonlight");
    wait_for(&p, &format!("/api/environments/{}", spec.id), &bob, |e| {
        e["state"] == "destroyed"
    })
    .await;
}

#[tokio::test]
async fn a_launch_for_someone_without_a_paired_device_here_is_refused() {
    let p = Portal::start().await;
    let (_, bob_id) = p.account("bob", "user").await;
    let (_, carol_id) = p.account("carol", "user").await;
    let (_, visitor_id) = p.account("visitor", "guest").await;
    let (mut a, b) = p.two_nodes().await;
    // Carol's device is paired with the other node; the guest's with this one
    // (a guest can't pair, but the row could have been made before the role changed).
    p.pair_device(&b, &carol_id, 1).await;
    p.pair_device(&a, &visitor_id, 2).await;
    p.pair_device(&a, &bob_id, 3).await;
    for (n, (user, template, wants)) in [
        (carol_id.as_str(), "chrome", "no device of yours"),
        (visitor_id.as_str(), "chrome", "guests"),
        ("no-such-user", "chrome", "no longer has an account"),
        (bob_id.as_str(), "no-such-app", "no such app"),
        // A gateway to someone else's Moonlight host is not an app of this one.
        (bob_id.as_str(), "moonlight:abc:1", "no such app"),
    ]
    .into_iter()
    .enumerate()
    {
        a.ask(n as u64, launch(user, template)).await;
        let (id, result) = a.answer().await;
        assert_eq!(id, n as u64);
        let why = result.expect_err(template);
        assert!(why.contains(wants), "{user} {template}: {why}");
    }
    assert!(p.environments().await.is_empty(), "nothing was started");
    // Nor may a client stop what isn't its owner's.
    let (mut other, _) = (a, b);
    other
        .ask(
            50,
            PortalRequest::GameStreamStop {
                user_id: carol_id,
                environment_id: "whatever".into(),
            },
        )
        .await;
    assert!(other.answer().await.1.is_err());
}

#[tokio::test]
async fn the_one_copy_of_a_persistent_app_is_refused_when_it_runs_on_another_node() {
    let p = Portal::start().await;
    let (_, bob_id) = p.account("bob", "user").await;
    let (mut a, b) = p.two_nodes().await;
    p.pair_device(&a, &bob_id, 1).await;
    // Steam keeps one home per user: bob's copy runs on box-b.
    db::insert_environment(
        &p.state.db,
        "e-steam",
        &bob_id,
        "steam",
        &b.id,
        Some("nvidia:0"),
        "running",
    )
    .await
    .unwrap();
    a.ask(1, launch(&bob_id, "steam")).await;
    let (_, result) = a.answer().await;
    let why = result.unwrap_err();
    assert!(why.contains("already running on box-b"), "{why}");
    assert_eq!(p.environments().await.len(), 1);
    assert!(a.is_left_alone().await);
}

#[tokio::test]
async fn a_node_that_cannot_run_the_app_or_fails_to_start_it_says_why() {
    let p = Portal::start().await;
    let (_, bob_id) = p.account("bob", "user").await;
    // A node with no GPU: the app that needs one has nowhere to run.
    let mut plain = FakeNode::connect(&p, "plain", true).await;
    plain.devices().await;
    sqlx::query("UPDATE nodes SET inventory = ? WHERE id = ?")
        .bind(
            serde_json::to_string(&Inventory {
                devices: Some(vec![Device {
                    id: "cpu".into(),
                    kind: DeviceKind::Cpu,
                    name: "CPU".into(),
                    render_node: None,
                    vendor: None,
                    codecs: vec!["h264".into()],
                    cores: Some(8),
                }]),
                data_root: Some("/data".into()),
                gamestream: Some(GameStreamInfo {
                    http_port: 47989,
                    name: "plain".into(),
                }),
                ..Inventory::default()
            })
            .unwrap(),
        )
        .bind(&plain.id)
        .execute(&p.state.db)
        .await
        .unwrap();
    p.pair_device(&plain, &bob_id, 1).await;
    plain.ask(1, launch(&bob_id, "steam")).await;
    let why = plain.answer().await.1.unwrap_err();
    assert!(why.contains("it needs a GPU"), "{why}");
    assert!(p.environments().await.is_empty());

    // The node tries and can't: the client hears the node's reason.
    plain.ask(2, launch(&bob_id, "chrome")).await;
    let (id, request) = plain.request().await;
    assert!(matches!(request, NodeRequest::StartEnvironment { .. }));
    plain
        .send(ToPortal::Response {
            id,
            result: Err("the image is missing".into()),
        })
        .await;
    let why = plain.answer().await.1.unwrap_err();
    assert!(
        why.contains("didn't start") && why.contains("the image is missing"),
        "{why}"
    );
}
