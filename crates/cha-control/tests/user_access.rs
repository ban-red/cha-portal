//! A restricted user launches only where an admin lets them: the nodes on
//! their list, plus single templates granted on other nodes.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use cha_control::{AppState, Config, app, db};
use cha_wire::{
    Device, DeviceKind, EnvironmentSpec, Inventory, NodeKey, NodeRequest, NodeResponse,
    StreamerEndpoint, ToNode, ToPortal,
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

struct Reply {
    status: StatusCode,
    body: Value,
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
            tunnel: Default::default(),
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
        let mut portal = Self {
            state,
            router,
            addr,
            admin: String::new(),
            _dir: dir,
        };
        let setup = portal
            .raw(
                "POST",
                "/api/setup",
                "",
                Some(json!({ "username": "admin", "password": "correct horse battery" })),
            )
            .await;
        portal.admin = setup.1.unwrap();
        portal
    }

    /// A reply and the cookie it set.
    async fn raw(
        &self,
        method: &str,
        path: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (Reply, Option<String>) {
        let mut req = Request::builder().method(method).uri(path);
        if !cookie.is_empty() {
            req = req.header(header::COOKIE, cookie);
        }
        let req = match body {
            Some(body) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let set = res
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::to_string);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (Reply { status, body }, set)
    }

    async fn call(&self, method: &str, path: &str, cookie: &str, body: Option<Value>) -> Reply {
        self.raw(method, path, cookie, body).await.0
    }

    /// A signed-in user: its cookie and id.
    async fn user(&self, username: &str) -> (String, String) {
        let (created, _) = self
            .raw(
                "POST",
                "/api/users",
                &self.admin,
                Some(json!({
                    "email": format!("{username}@test.local"),
                    "username": username,
                    "password": "another long password",
                    "role": "user",
                })),
            )
            .await;
        assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);
        let (_, cookie) = self
            .raw(
                "POST",
                "/api/auth/login",
                "",
                Some(json!({ "username": username, "password": "another long password" })),
            )
            .await;
        (
            cookie.unwrap(),
            created.body["id"].as_str().unwrap().to_string(),
        )
    }
}

struct FakeNode {
    id: String,
    ws: Socket,
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

impl FakeNode {
    /// Enrolls a node with one NVIDIA GPU and connects it.
    async fn connect(p: &Portal, name: &str) -> Self {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).unwrap();
        let key = NodeKey::from_secret(secret);
        let id = db::new_id();
        let inventory = Inventory {
            hostname: "box".into(),
            addresses: vec!["192.168.1.20".into()],
            data_root: Some("/data".into()),
            devices: Some(vec![Device {
                id: "nvidia:0".into(),
                kind: DeviceKind::Nvidia,
                name: "NVIDIA GeForce RTX 4090".into(),
                render_node: None,
                vendor: Some("nvidia".into()),
                codecs: vec!["h264".into()],
                cores: None,
            }]),
            ..Inventory::default()
        };
        sqlx::query(
            "INSERT INTO nodes (id, name, public_key, enrolled_at, inventory) VALUES (?, ?, ?, 0, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(key.public_b64())
        .bind(serde_json::to_string(&inventory).unwrap())
        .execute(&p.state.db)
        .await
        .unwrap();
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
        let _welcome: ToNode = next_text(&mut ws).await;
        Self { id, ws }
    }

    /// Takes the start the portal asks for and says the environment is up.
    async fn start(&mut self) -> EnvironmentSpec {
        loop {
            let ToNode::Request { id, request } = next_text(&mut self.ws).await else {
                continue;
            };
            let NodeRequest::StartEnvironment { environment } = request else {
                send(
                    &mut self.ws,
                    &ToPortal::Response {
                        id,
                        result: Ok(NodeResponse::Accepted),
                    },
                )
                .await;
                continue;
            };
            send(
                &mut self.ws,
                &ToPortal::Response {
                    id,
                    result: Ok(NodeResponse::EnvironmentStarted {
                        id: environment.id.clone(),
                        streamer: StreamerEndpoint {
                            http_port: 7600,
                            webrtc_port: 7601,
                            webtransport_port: 0,
                        },
                        ports: Vec::new(),
                    }),
                },
            )
            .await;
            return environment;
        }
    }
}

/// The ids of the nodes a placement lists.
fn nodes_of(reply: &Reply) -> Vec<String> {
    let mut ids: Vec<String> = reply.body["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["node"].as_str().unwrap().to_string())
        .collect();
    ids.dedup();
    ids
}

#[tokio::test]
async fn a_restricted_user_sees_and_launches_only_on_their_nodes() {
    let p = Portal::start().await;
    let mut open = FakeNode::connect(&p, "open").await;
    let shut = FakeNode::connect(&p, "shut").await;
    let (alice, alice_id) = p.user("alice").await;
    let access = format!("/api/users/{alice_id}/access");

    // Unrestricted: both.
    let all = p
        .call("GET", "/api/placements?template=chrome", &alice, None)
        .await;
    assert_eq!(nodes_of(&all).len(), 2);

    let set = p
        .call(
            "PUT",
            &access,
            &p.admin,
            Some(json!({ "nodeRestricted": true, "nodeIds": [open.id], "maxInstances": null })),
        )
        .await;
    assert_eq!(set.status, StatusCode::OK, "{:?}", set.body);

    // Only the allowed node is an option, for one template and for all.
    let one = p
        .call("GET", "/api/placements?template=chrome", &alice, None)
        .await;
    assert_eq!(nodes_of(&one), vec![open.id.clone()]);
    assert_eq!(one.body["auto"]["node"], open.id);
    let every = p.call("GET", "/api/placements", &alice, None).await;
    for (template, placements) in every.body["templates"].as_object().unwrap() {
        for option in placements["options"].as_array().unwrap() {
            assert_eq!(option["node"], open.id, "{template}");
        }
    }
    // Others, and admins, still see both.
    let (bob, _) = p.user("bob").await;
    assert_eq!(
        nodes_of(
            &p.call("GET", "/api/placements?template=chrome", &bob, None)
                .await
        )
        .len(),
        2
    );

    // Naming the other node is refused as if it weren't there.
    let refused = p
        .call(
            "POST",
            "/api/environments",
            &alice,
            Some(json!({ "templateId": "chrome", "node": shut.id })),
        )
        .await;
    assert_eq!(
        refused.status,
        StatusCode::BAD_REQUEST,
        "{:?}",
        refused.body
    );
    assert_eq!(refused.body["error"], "unknown_placement");

    // Unnamed, the launch goes to the allowed node.
    let launch = p.call(
        "POST",
        "/api/environments",
        &alice,
        Some(json!({ "templateId": "chrome" })),
    );
    let (launched, spec) = tokio::join!(launch, open.start());
    assert_eq!(launched.status, StatusCode::OK, "{:?}", launched.body);
    assert_eq!(launched.body["nodeId"], open.id);
    assert_eq!(spec.id, launched.body["id"]);
}

#[tokio::test]
async fn a_grant_lets_a_restricted_user_launch_one_template_on_another_node() {
    let p = Portal::start().await;
    let open = FakeNode::connect(&p, "open").await;
    let mut shared = FakeNode::connect(&p, "shared").await;
    let (alice, alice_id) = p.user("alice").await;
    p.call(
        "PUT",
        &format!("/api/users/{alice_id}/access"),
        &p.admin,
        Some(json!({ "nodeRestricted": true, "nodeIds": [open.id], "maxInstances": null })),
    )
    .await;
    let on_shared = json!({ "templateId": "chrome", "node": shared.id });

    let before = p
        .call("POST", "/api/environments", &alice, Some(on_shared.clone()))
        .await;
    assert_eq!(before.body["error"], "unknown_placement");

    let grant = p
        .call(
            "POST",
            &format!("/api/users/{alice_id}/grants"),
            &p.admin,
            Some(json!({ "nodeId": shared.id, "templateId": "chrome" })),
        )
        .await;
    assert_eq!(grant.status, StatusCode::OK, "{:?}", grant.body);

    // The grant is for that template only.
    let chrome = p
        .call("GET", "/api/placements?template=chrome", &alice, None)
        .await;
    assert_eq!(nodes_of(&chrome).len(), 2);
    let firefox = p
        .call("GET", "/api/placements?template=firefox", &alice, None)
        .await;
    assert_eq!(nodes_of(&firefox), vec![open.id.clone()]);
    let other = p
        .call(
            "POST",
            "/api/environments",
            &alice,
            Some(json!({ "templateId": "firefox", "node": shared.id })),
        )
        .await;
    assert_eq!(other.body["error"], "unknown_placement");

    // The dashboard's "Shared with you" launch.
    let mine = p.call("GET", "/api/me/grants", &alice, None).await;
    assert_eq!(mine.body[0]["online"], true);
    assert_eq!(mine.body[0]["nodeName"], "shared");
    let launch = p.call("POST", "/api/environments", &alice, Some(on_shared));
    let (launched, _) = tokio::join!(launch, shared.start());
    assert_eq!(launched.status, StatusCode::OK, "{:?}", launched.body);
    assert_eq!(launched.body["nodeId"], shared.id);

    // Take the grant away and the node is out again.
    p.call(
        "DELETE",
        &format!(
            "/api/users/{alice_id}/grants/{}",
            grant.body["id"].as_str().unwrap()
        ),
        &p.admin,
        None,
    )
    .await;
    let after = p
        .call("GET", "/api/placements?template=chrome", &alice, None)
        .await;
    assert_eq!(nodes_of(&after), vec![open.id.clone()]);
}
