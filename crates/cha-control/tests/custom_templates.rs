//! Custom environments (ADR 0021): an admin saves a template under a new
//! name; it resolves at use time, shares or keeps its own app data, and only
//! reaches nodes that understand and allow what it asks for.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use cha_control::{AppState, Config, app, db};
use cha_wire::{
    AllowedMount, Device, DeviceKind, EnvironmentSpec, HostOptionsMode, HostPolicy, HostPort,
    Inventory, NodeKey, NodeRequest, NodeResponse, PortRange, Protocol, StreamerEndpoint, ToNode,
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

fn cookie_of(res: &axum::response::Response) -> String {
    res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
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
        let admin = cookie_of(&res);
        Self {
            state,
            router,
            addr,
            admin,
            _dir: dir,
        }
    }

    async fn send(&self, req: Request<Body>) -> Reply {
        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        }
    }

    async fn call(&self, method: &str, path: &str, cookie: &str, body: Option<Value>) -> Reply {
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
        self.send(req).await
    }

    async fn admin(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        self.call(method, path, &self.admin, body).await
    }

    /// An account with `role`, signed in: its cookie and id.
    async fn account(&self, username: &str, role: &str) -> (String, String) {
        let created = self
            .admin(
                "POST",
                "/api/users",
                Some(json!({ "username": username, "email": format!("{username}@test.local"), "password": "another long password", "role": role })),
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
        (
            cookie_of(&res),
            created.body["id"].as_str().unwrap().to_string(),
        )
    }

    async fn create(&self, slug: &str, base: &str, share: bool, overrides: Value) -> Reply {
        self.admin(
            "POST",
            "/api/admin/custom-templates",
            Some(json!({ "slug": slug, "base": base, "shareData": share, "overrides": overrides })),
        )
        .await
    }

    async fn catalog(&self, cookie: &str) -> Vec<Value> {
        let r = self.call("GET", "/api/catalog", cookie, None).await;
        assert_eq!(r.status, StatusCode::OK);
        r.body.as_array().unwrap().clone()
    }

    async fn audit_actions(&self) -> Vec<String> {
        let rows = self.admin("GET", "/api/audit", None).await;
        rows.body
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["action"].as_str().map(String::from))
            .collect()
    }
}

// ---- A node driven by the test ----

struct FakeNode {
    id: String,
    ws: Socket,
}

/// What a node lists and allows.
#[derive(Default)]
struct Abilities {
    features: Vec<&'static str>,
    policy: Option<HostPolicy>,
}

fn inventory(abilities: &Abilities) -> Inventory {
    Inventory {
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
        spec_features: abilities.features.iter().map(|f| f.to_string()).collect(),
        host_options: abilities.policy.clone(),
        ..Inventory::default()
    }
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
    /// Enrolls a node and connects it. Its inventory is stored directly:
    /// what the portal knows of a node is what it last said.
    async fn connect(p: &Portal, name: &str, abilities: Abilities) -> Self {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).unwrap();
        let key = NodeKey::from_secret(secret);
        let id = db::new_id();
        sqlx::query(
            "INSERT INTO nodes (id, name, public_key, enrolled_at, inventory) VALUES (?, ?, ?, 0, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(key.public_b64())
        .bind(serde_json::to_string(&inventory(&abilities)).unwrap())
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

    /// Takes the start the portal asks for and says the environment is up,
    /// with `ports` published.
    async fn start(&mut self, ports: Vec<HostPort>) -> EnvironmentSpec {
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
                        ports,
                    }),
                },
            )
            .await;
            return environment;
        }
    }
}

fn allowlist() -> HostPolicy {
    HostPolicy {
        mode: HostOptionsMode::Allowlist,
        mounts: vec![AllowedMount {
            name: "media".into(),
            read_only: true,
        }],
        ports: vec![PortRange {
            start: 27015,
            end: 27030,
            protocol: Protocol::Udp,
        }],
        caps: vec!["SYS_NICE".into()],
        devices: Vec::new(),
    }
}

fn media_host() -> Value {
    json!({
        "mounts": [{ "source": { "kind": "named", "name": "media" }, "target": "/mnt/media", "readOnly": true }],
        "ports": [{ "container": 27015, "protocol": "udp" }],
        "capAdd": ["SYS_NICE"]
    })
}

/// [`media_host`] without its capability, which Steam can't take.
fn media_host_no_caps() -> Value {
    let mut host = media_host();
    host.as_object_mut().unwrap().remove("capAdd");
    host
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

const FULL: &[&str] = &["env", "data-template", "host-options"];

// ---- Tests ----

#[tokio::test]
async fn an_admin_creates_lists_updates_and_deletes_one() {
    let p = Portal::start().await;
    let (player, _) = p.account("player1", "user").await;

    let r = p
        .create(
            "steam-big",
            "steam",
            false,
            json!({ "name": "Steam (big shm)", "shmMb": 4096, "env": { "PROTON_LOG": "1" } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["id"], "custom.steam-big");
    assert_eq!(r.body["slug"], "steam-big");
    assert_eq!(r.body["base"], "steam");
    assert_eq!(r.body["baseName"], "Steam");
    assert_eq!(r.body["shareData"], false);
    assert_eq!(r.body["overrides"]["shmMb"], 4096);
    assert_eq!(r.body["host"], Value::Null);
    assert_eq!(r.body["hasIcon"], false);
    assert_eq!(r.body["unavailable"], Value::Null);
    assert!(r.body["createdAt"].as_i64().unwrap() > 0);

    let list = p.admin("GET", "/api/admin/custom-templates", None).await;
    assert_eq!(list.body.as_array().unwrap().len(), 1);
    assert_eq!(list.body[0]["id"], "custom.steam-big");

    // Players see it as a card of its own, resolved from the base. The
    // variables stay with the admin.
    let cards = p.catalog(&player).await;
    let card = cards
        .iter()
        .find(|t| t["id"] == "custom.steam-big")
        .unwrap();
    assert_eq!(card["name"], "Steam (big shm)");
    assert_eq!(card["shmMb"], 4096);
    assert_eq!(
        card["description"],
        cards.iter().find(|t| t["id"] == "steam").unwrap()["description"]
    );
    assert_eq!(card["custom"]["base"], "steam");
    assert_eq!(card["custom"]["shareData"], false);
    assert_eq!(card["custom"]["host"], Value::Null);
    assert!(!card.to_string().contains("PROTON_LOG"));
    assert!(card.get("icon").is_some(), "the base's icon is inherited");
    // The built-in cards carry no `custom`.
    assert!(
        cards
            .iter()
            .find(|t| t["id"] == "steam")
            .unwrap()
            .get("custom")
            .is_none()
    );

    // Its icon is the base's until one is uploaded.
    let icon = p
        .send(
            Request::builder()
                .uri("/api/catalog/custom.steam-big/icon")
                .header(header::COOKIE, &player)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(icon.status, StatusCode::OK);

    // Changing it: the overrides are replaced, not merged.
    let r = p
        .admin(
            "PUT",
            "/api/admin/custom-templates/custom.steam-big",
            Some(json!({ "overrides": { "fps": 120 }, "shareData": false, "host": media_host_no_caps() })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["overrides"], json!({ "fps": 120 }));
    assert_eq!(r.body["host"]["ports"][0]["container"], 27015);
    let cards = p.catalog(&player).await;
    let card = cards
        .iter()
        .find(|t| t["id"] == "custom.steam-big")
        .unwrap();
    assert_eq!(
        card["name"], "Steam",
        "an override that was dropped is inherited again"
    );
    assert_eq!(card["fps"], 120);
    assert_eq!(card["custom"]["host"]["mounts"][0]["target"], "/mnt/media");

    // An uploaded icon, then back to the base's.
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><rect width="8" height="8"/></svg>"#;
    let put_icon = |body: String| {
        Request::builder()
            .method("PUT")
            .uri("/api/admin/custom-templates/custom.steam-big/icon")
            .header(header::COOKIE, &p.admin)
            .header(header::CONTENT_TYPE, "image/svg+xml")
            .body(Body::from(body))
            .unwrap()
    };
    let r = p.send(put_icon("<html>no</html>".into())).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.body["error"], "bad_icon");
    let r = p
        .send(put_icon(
            r#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#.into(),
        ))
        .await;
    assert!(r.body["message"].as_str().unwrap().contains("viewBox"));
    let r = p.send(put_icon(svg.to_string())).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["hasIcon"], true);
    let r = p
        .admin(
            "DELETE",
            "/api/admin/custom-templates/custom.steam-big/icon",
            None,
        )
        .await;
    assert_eq!(r.body["hasIcon"], false);

    let r = p
        .admin(
            "DELETE",
            "/api/admin/custom-templates/custom.steam-big",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert!(
        !p.catalog(&player)
            .await
            .iter()
            .any(|t| t["id"] == "custom.steam-big")
    );
    let r = p
        .admin(
            "DELETE",
            "/api/admin/custom-templates/custom.steam-big",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let actions = p.audit_actions().await;
    for a in [
        "custom_template.created",
        "custom_template.updated",
        "custom_template.deleted",
    ] {
        assert!(actions.contains(&a.to_string()), "{a}");
    }
}

#[tokio::test]
async fn wider_security_and_host_options_are_in_the_audit_log() {
    let p = Portal::start().await;
    let r = p
        .create(
            "chrome-pro",
            "chrome",
            false,
            json!({ "security": "steam" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let r = p
        .admin(
            "POST",
            "/api/admin/custom-templates",
            Some(json!({ "slug": "with-media", "base": "xfce", "shareData": false, "overrides": {}, "host": media_host() })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let audit = p.admin("GET", "/api/audit", None).await.body;
    let entries = audit.as_array().unwrap();
    let created = |target: &str| {
        entries
            .iter()
            .find(|e| e["action"] == "custom_template.created" && e["target"] == target)
            .unwrap_or_else(|| panic!("no audit entry for {target}"))["detail"]
            .to_string()
    };
    let wider = created("custom.chrome-pro");
    assert!(wider.contains("securityWiderThanBase"), "{wider}");
    let host = created("custom.with-media");
    assert!(
        host.contains("SYS_NICE") && host.contains("/mnt/media"),
        "{host}"
    );
}

#[tokio::test]
async fn steam_takes_no_added_capabilities() {
    let p = Portal::start().await;
    let refused = |r: &Reply| {
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.body);
        assert_eq!(r.body["error"], "bad_host", "{:?}", r.body);
        let message = r.body["message"].as_str().unwrap();
        assert!(message.contains("bubblewrap"), "{message}");
    };
    let create = |slug: &str, base: &str, overrides: Value| json!({ "slug": slug, "base": base, "shareData": false, "overrides": overrides, "host": media_host() });
    // Steam as the base, or as the profile a custom one moves to.
    refused(
        &p.admin(
            "POST",
            "/api/admin/custom-templates",
            Some(create("steam-cap", "steam", json!({}))),
        )
        .await,
    );
    refused(
        &p.admin(
            "POST",
            "/api/admin/custom-templates",
            Some(create(
                "chrome-cap",
                "chrome",
                json!({ "security": "steam" }),
            )),
        )
        .await,
    );
    // Its other host options are fine, and so are capabilities elsewhere.
    let r = p
        .admin(
            "POST",
            "/api/admin/custom-templates",
            Some(json!({ "slug": "steam-media", "base": "steam", "shareData": false, "overrides": {}, "host": media_host_no_caps() })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let r = p
        .admin(
            "POST",
            "/api/admin/custom-templates",
            Some(create("xfce-cap", "xfce", json!({}))),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    // Nor can a change add them.
    refused(
        &p.admin(
            "PUT",
            "/api/admin/custom-templates/custom.steam-media",
            Some(json!({ "overrides": {}, "shareData": false, "host": media_host() })),
        )
        .await,
    );
    refused(
        &p.admin(
            "PUT",
            "/api/admin/custom-templates/custom.xfce-cap",
            Some(json!({ "overrides": { "security": "steam" }, "shareData": false, "host": media_host() })),
        )
        .await,
    );
}

#[tokio::test]
async fn only_admins_manage_them() {
    let p = Portal::start().await;
    let (player, _) = p.account("player1", "user").await;
    let body = json!({ "slug": "x", "base": "steam", "shareData": false, "overrides": {} });
    for (method, path, body) in [
        ("GET", "/api/admin/custom-templates", None),
        ("POST", "/api/admin/custom-templates", Some(body.clone())),
        (
            "PUT",
            "/api/admin/custom-templates/custom.x",
            Some(json!({ "overrides": {}, "shareData": false })),
        ),
        ("DELETE", "/api/admin/custom-templates/custom.x", None),
        ("PUT", "/api/admin/custom-templates/custom.x/icon", None),
        ("DELETE", "/api/admin/custom-templates/custom.x/icon", None),
        ("GET", "/api/admin/host-options", None),
    ] {
        let r = p.call(method, path, &player, body.clone()).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{method} {path}");
        let r = p.call(method, path, "", body).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}

#[tokio::test]
async fn bad_input_is_refused_naming_the_field() {
    let p = Portal::start().await;
    let bad = |r: &Reply, code: &str, word: &str| {
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.body);
        assert_eq!(r.body["error"], code, "{:?}", r.body);
        assert!(
            r.body["message"].as_str().unwrap().contains(word),
            "{:?} should name {word}",
            r.body
        );
    };
    bad(
        &p.create("Bad Slug", "steam", false, json!({})).await,
        "bad_slug",
        "slug",
    );
    bad(
        &p.create("migrated", "steam", false, json!({})).await,
        "bad_slug",
        "slug",
    );
    bad(
        &p.create("ok", "nope", false, json!({})).await,
        "bad_base",
        "base",
    );
    bad(
        &p.create("ok", "custom.other", false, json!({})).await,
        "bad_base",
        "base",
    );
    bad(
        &p.create("ok", "steam", false, json!({ "env": { "CHA_WIDTH": "1" } }))
            .await,
        "bad_overrides",
        "env",
    );
    bad(
        &p.create("ok", "steam", false, json!({ "env": { "HOME": "/x" } }))
            .await,
        "bad_overrides",
        "env",
    );
    bad(
        &p.create("ok", "steam", false, json!({ "fps": 75 })).await,
        "bad_overrides",
        "fps",
    );
    bad(
        &p.create("ok", "steam", false, json!({ "class": "moonlight" }))
            .await,
        "bad_overrides",
        "class",
    );
    bad(
        &p.create("ok", "steam", false, json!({ "name": " " })).await,
        "bad_overrides",
        "name",
    );
    bad(
        &p.create("ok", "steam", false, json!({ "shmMb": 99999 }))
            .await,
        "bad_overrides",
        "shmMb",
    );
    bad(
        &p.create("ok", "steam", false, json!({ "image": "has space" }))
            .await,
        "bad_overrides",
        "image",
    );
    bad(
        &p.create("ok", "steam", false, json!({ "colour": "red" }))
            .await,
        "bad_overrides",
        "colour",
    );
    // A mount over what the node mounts itself.
    let r = p
        .admin(
            "POST",
            "/api/admin/custom-templates",
            Some(json!({
                "slug": "ok", "base": "steam", "shareData": false, "overrides": {},
                "host": { "mounts": [{ "source": { "kind": "named", "name": "media" }, "target": "/home/cha/x" }] }
            })),
        )
        .await;
    bad(&r, "bad_host", "host");
    let r = p
        .admin(
            "POST",
            "/api/admin/custom-templates",
            Some(json!({
                "slug": "ok", "base": "steam", "shareData": false, "overrides": {},
                "host": { "capAdd": ["cap_sys_admin"] }
            })),
        )
        .await;
    bad(&r, "bad_host", "capability");
    assert!(
        p.admin("GET", "/api/admin/custom-templates", None)
            .await
            .body
            .as_array()
            .unwrap()
            .is_empty()
    );

    // A name that exists is a conflict.
    assert_eq!(
        p.create("dup", "steam", false, json!({})).await.status,
        StatusCode::OK
    );
    let r = p.create("dup", "chrome", false, json!({})).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["error"], "slug_taken");
}

#[tokio::test]
async fn custom_is_not_a_catalog_slug() {
    let p = Portal::start().await;
    let doc = json!({ "version": 1, "templates": [] });
    let r = p
        .admin(
            "POST",
            "/api/admin/catalogs",
            Some(json!({ "slug": "custom", "document": doc })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.body["error"], "slug_reserved");
}

fn acme(image: &str) -> Value {
    json!({ "version": 1, "name": "Acme", "templates": [{
        "id": "notes", "name": "Notes", "description": "an app", "image": image,
        "class": "browser", "security": "standard", "shmMb": 512, "fps": 90,
    }] })
}

#[tokio::test]
async fn it_is_resolved_from_its_base_each_time() {
    let p = Portal::start().await;
    let (player, _) = p.account("player1", "user").await;
    let add = p
        .admin(
            "POST",
            "/api/admin/catalogs",
            Some(json!({ "slug": "acme", "document": acme("ghcr.io/acme/notes:1") })),
        )
        .await;
    assert_eq!(add.status, StatusCode::OK, "{:?}", add.body);
    let r = p
        .create("notes-2", "acme.notes", false, json!({ "name": "Notes 2" }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let card = |cards: &[Value]| cards.iter().find(|t| t["id"] == "custom.notes-2").cloned();

    let c = card(&p.catalog(&player).await).unwrap();
    assert_eq!(
        (c["image"].as_str(), c["fps"].as_i64(), c["shmMb"].as_i64()),
        (Some("ghcr.io/acme/notes:1"), Some(90), Some(512))
    );

    // The catalog moves on: the custom one follows in every field it didn't change.
    let r = p
        .admin(
            "POST",
            "/api/admin/catalogs/acme/refresh",
            Some(json!({ "document": acme("ghcr.io/acme/notes:2") })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let c = card(&p.catalog(&player).await).unwrap();
    assert_eq!(c["image"], "ghcr.io/acme/notes:2");
    assert_eq!(c["name"], "Notes 2");

    // Its own image drops the base's localImage and replaces the image.
    let r = p
        .admin(
            "PUT",
            "/api/admin/custom-templates/custom.notes-2",
            Some(json!({ "overrides": { "image": "ghcr.io/acme/notes:pinned" }, "shareData": false })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let c = card(&p.catalog(&player).await).unwrap();
    assert_eq!(c["image"], "ghcr.io/acme/notes:pinned");

    // Its catalog can't go while an environment of the custom one is live.
    let owner: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(&p.state.db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO environments (id, owner_id, template_id, state, created_at, updated_at) \
         VALUES ('e1', ?, 'custom.notes-2', 'running', 1, 1)",
    )
    .bind(&owner)
    .execute(&p.state.db)
    .await
    .unwrap();
    let r = p.admin("DELETE", "/api/admin/catalogs/acme", None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["error"], "in_use");
    sqlx::query("UPDATE environments SET state = 'stopped' WHERE id = 'e1'")
        .execute(&p.state.db)
        .await
        .unwrap();

    // The base goes away: unavailable, with the reason, to admins only.
    let r = p.admin("DELETE", "/api/admin/catalogs/acme", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(card(&p.catalog(&player).await).is_none());
    let list = p.admin("GET", "/api/admin/custom-templates", None).await;
    let why = list.body[0]["unavailable"].as_str().unwrap();
    assert!(why.contains("acme.notes") && why.contains("gone"), "{why}");
    assert_eq!(
        list.body[0]["overrides"]["image"],
        "ghcr.io/acme/notes:pinned"
    );
    let launch = p
        .call(
            "POST",
            "/api/environments",
            &player,
            Some(json!({ "templateId": "custom.notes-2" })),
        )
        .await;
    assert_eq!(launch.status, StatusCode::BAD_REQUEST);
    assert_eq!(launch.body["error"], "unknown_template");
}

#[tokio::test]
async fn shared_data_uses_the_bases_directories_and_one_home_is_never_written_twice() {
    let p = Portal::start().await;
    let (player, uid) = p.account("player1", "user").await;
    let mut node = FakeNode::connect(
        &p,
        "box",
        Abilities {
            features: FULL.to_vec(),
            policy: None,
        },
    )
    .await;
    p.create("steam-shared", "steam", true, json!({ "shmMb": 4096 }))
        .await;
    p.create("steam-own", "steam", false, json!({})).await;

    let launch = |t: &'static str| {
        let (p, player) = (&p, player.clone());
        async move {
            p.call(
                "POST",
                "/api/environments",
                &player,
                Some(json!({ "templateId": t })),
            )
            .await
        }
    };
    let r = launch("custom.steam-shared").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let first = r.body["id"].as_str().unwrap().to_string();
    let spec = node.start(Vec::new()).await;
    assert_eq!(spec.template, "custom.steam-shared");
    assert_eq!(spec.shm_mb, 4096);
    let storage = spec.storage.as_deref().unwrap();
    assert_eq!(storage.data_template.as_deref(), Some("steam"));
    assert_eq!(
        storage.home.as_deref(),
        Some(format!("users/{uid}/steam").as_str())
    );
    assert_eq!(storage.shared.as_ref().unwrap().path, "shared/steam");
    assert_eq!(spec.data_template(), "steam");
    assert!(spec.env.is_none() && spec.host.is_none());
    // Its own checks pass, as the node runs them.
    assert_eq!(storage.check(&uid, &spec.template), Ok(()));

    // The base would write the same home: refused, naming the live one.
    let r = launch("steam").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.body);
    assert_eq!(r.body["error"], "data_in_use");
    assert!(
        r.body["message"].as_str().unwrap().contains(&first),
        "{:?}",
        r.body
    );
    // So would a second variant sharing it (here: the same one).
    let r = launch("custom.steam-shared").await;
    assert_eq!(r.body["error"], "already_running");
    // A variant with its own data is fine.
    let r = launch("custom.steam-own").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let spec = node.start(Vec::new()).await;
    let storage = spec.storage.as_deref().unwrap();
    assert_eq!(storage.data_template, None);
    assert_eq!(
        storage.home.as_deref(),
        Some(format!("users/{uid}/custom.steam-own").as_str())
    );
    assert_eq!(storage.check(&uid, &spec.template), Ok(()));

    // The data choice can't change while the template or its base is live.
    let r = p
        .admin(
            "PUT",
            "/api/admin/custom-templates/custom.steam-shared",
            Some(json!({ "overrides": {}, "shareData": false })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["error"], "live");
    // Nor can a live one be deleted.
    let r = p
        .admin(
            "DELETE",
            "/api/admin/custom-templates/custom.steam-shared",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    // Other changes are fine.
    let r = p
        .admin(
            "PUT",
            "/api/admin/custom-templates/custom.steam-shared",
            Some(json!({ "overrides": { "shmMb": 1024 }, "shareData": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);

    // Per-app settings follow the base: a user's choice for the variant is
    // the base's choice.
    let storage = p.call("GET", "/api/storage", &player, None).await;
    let apps = storage.body["apps"].as_array().unwrap();
    let live_of = |t: &str| apps.iter().find(|a| a["template"] == t).unwrap()["live"].clone();
    assert_eq!(live_of("steam"), true);
    assert_eq!(live_of("custom.steam-shared"), true);
    assert_eq!(live_of("custom.steam-own"), true);

    // Once they stop, the base starts.
    for id in [&first] {
        let r = p
            .call("DELETE", &format!("/api/environments/{id}"), &player, None)
            .await;
        assert_eq!(r.status, StatusCode::OK);
    }
    // (the stop request goes to the node)
    let ToNode::Request { id, request } = next_text::<ToNode>(&mut node.ws).await else {
        panic!("expected a request");
    };
    assert!(matches!(request, NodeRequest::StopEnvironment { .. }));
    send(
        &mut node.ws,
        &ToPortal::Response {
            id,
            result: Ok(NodeResponse::EnvironmentStopped { id: first.clone() }),
        },
    )
    .await;
    wait_for(&p, &format!("/api/environments/{first}"), &player, |e| {
        e["state"] == "destroyed"
    })
    .await;
    let r = launch("steam").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let spec = node.start(Vec::new()).await;
    assert_eq!(spec.storage.as_deref().unwrap().data_template, None);
}

#[tokio::test]
async fn placement_skips_nodes_that_lack_a_feature_or_refuse_the_host_options() {
    let p = Portal::start().await;
    let (player, _) = p.account("player1", "user").await;
    let mut old = FakeNode::connect(&p, "old", Abilities::default()).await;
    let mut closed = FakeNode::connect(
        &p,
        "closed",
        Abilities {
            features: FULL.to_vec(),
            policy: Some(HostPolicy {
                mode: HostOptionsMode::Off,
                ..Default::default()
            }),
        },
    )
    .await;
    let mut open = FakeNode::connect(
        &p,
        "open",
        Abilities {
            features: FULL.to_vec(),
            policy: Some(allowlist()),
        },
    )
    .await;
    let r = p
        .admin(
            "POST",
            "/api/admin/custom-templates",
            Some(json!({
                "slug": "game-server", "base": "chrome", "shareData": false,
                "overrides": { "env": { "SERVER_NAME": "cha" } }, "host": media_host(),
            })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);

    // The editor sees what each node allows.
    let nodes = p.admin("GET", "/api/admin/host-options", None).await;
    let nodes = nodes.body.as_array().unwrap();
    assert_eq!(nodes.len(), 3);
    let by = |n: &str| nodes.iter().find(|x| x["nodeName"] == n).unwrap();
    assert_eq!(by("old")["online"], true);
    assert_eq!(by("old")["specFeatures"], json!([]));
    assert_eq!(by("old")["policy"], Value::Null);
    assert_eq!(
        by("open")["specFeatures"],
        json!(["env", "data-template", "host-options"])
    );
    assert_eq!(by("open")["policy"]["mode"], "allowlist");
    assert_eq!(by("open")["policy"]["mounts"][0]["name"], "media");
    assert_eq!(by("closed")["policy"]["mode"], "off");

    // Each other node says why it is out.
    let placements = p
        .call(
            "GET",
            "/api/placements?template=custom.game-server",
            &player,
            None,
        )
        .await;
    let options = placements.body["options"].as_array().unwrap();
    let option = |n: &str| options.iter().find(|o| o["nodeName"] == n).unwrap();
    assert_eq!(option("open")["allowed"], true);
    assert_eq!(option("old")["allowed"], false);
    assert!(
        option("old")["reason"]
            .as_str()
            .unwrap()
            .contains("environment variables")
    );
    assert_eq!(option("closed")["allowed"], false);
    assert!(
        option("closed")["reason"]
            .as_str()
            .unwrap()
            .contains("allows no host options")
    );
    assert_eq!(placements.body["auto"]["node"], open.id);

    // Picking one by hand is refused with the reason.
    for (node, word) in [
        (&old.id, "environment variables"),
        (&closed.id, "host options"),
    ] {
        let r = p
            .call(
                "POST",
                "/api/environments",
                &player,
                Some(json!({ "templateId": "custom.game-server", "node": node })),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.body);
        assert_eq!(r.body["error"], "placement_not_allowed");
        assert!(
            r.body["message"].as_str().unwrap().contains(word),
            "{:?}",
            r.body
        );
    }
    // Nothing was sent to either.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), old.start(Vec::new()))
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), closed.start(Vec::new()))
            .await
            .is_err()
    );

    // The one that allows it gets the variables and host options, and the
    // ports it published reach the environment's view.
    let r = p
        .call(
            "POST",
            "/api/environments",
            &player,
            Some(json!({ "templateId": "custom.game-server" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let id = r.body["id"].as_str().unwrap().to_string();
    assert_eq!(r.body["nodeName"], "open");
    let spec = open
        .start(vec![HostPort {
            container: 27015,
            protocol: Protocol::Udp,
            host: Some(27016),
        }])
        .await;
    assert_eq!(spec.env.as_deref().unwrap()["SERVER_NAME"], "cha");
    let host = spec.host.as_deref().unwrap();
    assert_eq!(host.cap_add, vec!["SYS_NICE".to_string()]);
    assert_eq!(host.ports[0].container, 27015);
    let env = wait_for(&p, &format!("/api/environments/{id}"), &player, |e| {
        e["state"] == "running"
    })
    .await;
    assert_eq!(
        env["ports"],
        json!([{ "container": 27015, "protocol": "udp", "host": 27016 }])
    );
    // The launch's audit entry says which host options it used.
    let audit = p.admin("GET", "/api/audit", None).await.body.to_string();
    assert!(audit.contains("SYS_NICE"));

    // A plain template never carries them, even to a node that could.
    let r = p
        .call(
            "POST",
            "/api/environments",
            &player,
            Some(json!({ "templateId": "chrome", "node": open.id })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let spec = open.start(Vec::new()).await;
    assert!(spec.env.is_none() && spec.host.is_none());
    let env = wait_for(
        &p,
        &format!("/api/environments/{}", r.body["id"].as_str().unwrap()),
        &player,
        |e| e["state"] == "running",
    )
    .await;
    assert!(env.get("ports").is_none());
}

#[tokio::test]
async fn a_custom_environment_with_only_env_needs_only_that_feature() {
    let p = Portal::start().await;
    let (player, _) = p.account("player1", "user").await;
    let mut node = FakeNode::connect(
        &p,
        "envy",
        Abilities {
            features: vec!["env"],
            policy: None,
        },
    )
    .await;
    p.create("envy", "chrome", false, json!({ "env": { "A": "1" } }))
        .await;
    p.admin(
        "POST",
        "/api/admin/custom-templates",
        Some(json!({ "slug": "hosty", "base": "chrome", "shareData": false, "overrides": {}, "host": media_host() })),
    )
    .await;
    p.create("shared-chrome", "kde", true, json!({})).await;

    let r = p
        .call(
            "POST",
            "/api/environments",
            &player,
            Some(json!({ "templateId": "custom.envy" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let spec = node.start(Vec::new()).await;
    assert_eq!(spec.env.as_deref().unwrap()["A"], "1");

    // Host options and a shared home each need their own feature.
    for t in ["custom.hosty", "custom.shared-chrome"] {
        let r = p
            .call(
                "POST",
                "/api/environments",
                &player,
                Some(json!({ "templateId": t })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CONFLICT, "{t}: {:?}", r.body);
        assert_eq!(r.body["error"], "no_node");
    }
}
